#!/usr/bin/env python3
"""make-driver.py — write a matched-bench campaign driver from a short JSON spec (python3 stdlib only).

    python3 -I tools/matched-bench/campaign/make-driver.py SPEC.json -o DRIVER.sh

The driver = a generated config block (arms, env, cells, md5s from the spec) + driver-body.sh (the standard
parts, built and tested once: tools/matched-bench/test_make_driver.py), which carries the Job 4 driver's runtime
watchers (mid-cell node death -> node-death.txt + CELL FAIL, memguard, hardscan) on every cell, and its samplers
and host checks unless the spec turns them off ("samplers": false, "host_checks": false). Then:

    DRIVER.sh dry            preflight only (paths, staged md5s), prints the cell list
    DRIVER.sh show           the plan as tables (paste into the report)
    DRIVER.sh launch smoke   the one smoke cell with every check, in its own unit (bench-<campaign>-smoke)
    DRIVER.sh launch         the campaign (unit bench-<campaign>; detach.sh holds the build lock)

Spec format and the shapes the generator does NOT support: tools/matched-bench/README.md ("Campaign driver
generator"); example: campaign/driver-spec.example.json. Any unknown or unsupported key is a hard error
(exit 2, nothing written).
"""

import argparse
import hashlib
import json
import os
import re
import shlex
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
BODY = os.path.join(HERE, "driver-body.sh")

TOP_REQUIRED = {
    "campaign",
    "run_dir",
    "label_prefix",
    "harness_worktree",
    "tools_dir",
    "binaries",
    "arms",
    "baseline",
    "duration_s",
    "markets",
}
TOP_OPTIONAL = {
    "comment",
    "results_root",
    "devnet_dir",
    "rate",
    "replicates",
    "order",
    "warmup",
    "smoke",
    "node_env",
    "harness_env",
    "absent_env",
    "expect",
    "analysis",
    "max_load",
    "max_busy_pct",
    "samplers",
    "host_checks",
    "checks",
}
ARM_KEYS = {"label", "binary", "env", "harness_env", "expect", "note"}
BIN_KEYS = {
    "stage_dir",
    "node_md5",
    "bench_md5",
    "md5_file",
    "node_row",
    "bench_row",
    "wrapper",
}
EXPECT_KEYS = {"file", "regex", "min", "max"}
WARMUP_KEYS = {"arm", "duration_s", "tag", "gate"}
CHECKS_KEYS = {"source", "per_cell"}
SMOKE_KEYS = {"arm", "duration_s"}
ANALYSIS_KEYS = {"cmd", "metric"}
HOST_KEYS = {"min_mem_avail_gb", "sysctl"}
MIN_MEM_AVAIL_GB = 40  # the Job 4 driver's preflight floor
# set by the driver itself (paths, md5 guards)
RESERVED_HARNESS = {
    "TARGET_DIR",
    "TOOLS_DIR",
    "DATA_ROOT",
    "RESULTS_ROOT",
    "EXPECT_NODE_MD5",
    "EXPECT_BENCH_MD5",
    "BENCH_ALLOW_UNDETACHED",
}
# set by the driver from its preflight (preflight_post), after every spec / arm / harness env on run-cell.sh's env
# line, in both directions: DEFER_REPORT_SCANS = DEFER_SCANS (in-line 0, deferred 1), GENESIS_CACHE_DIR = the run's
# genesis cache or empty (Codex round 6 on bb2a727f: an arm's DEFER_REPORT_SCANS=1 deferred the scans an in-line
# campaign's expectation read). Refused in every env object: harness env (the driver's value wins anyway) and node
# env (run-cell.sh's EXTRA_ENV, exported mid-run, would overwrite run-cell.sh's own; run-cell.sh refuses it too)
DRIVER_OWNED = {"DEFER_REPORT_SCANS", "GENESIS_CACHE_DIR"}
# every name the generated config assigns (render() unsets them first; driver-body.sh DRIVER_VARS lists them)
CONFIG_VARS = (
    "CAMPAIGN R RUN P WT_H TOOLS DEVNET MARKETS RATE MAX_BUSY_PCT NODES BASELINE BDIR BMD5 BBMD5 BWRAP ABIN AENV AXE"
    " AHENV ARMS ENV_NAMES PROC_REC_RE EXP_ARM EXP_FILE EXP_RE EXP_MIN EXP_MAX CELLS SMOKE_CELL ANALYSIS_CMD"
    " ANALYSIS_METRIC SAMPLERS HOST_CHECKS MIN_MEM_AVAIL_KB SYSCTL_WANT GATE_TAGS CHECKS_SOURCE CHECK_FNS"
).split()
# restart / crash cells: not a supported shape (hand-write the driver)
UNSUPPORTED_HARNESS = {"CRASH_KILL_AT_S", "KILL_SIG", "KILL_WAIT", "KILL_NODE"}

NAME_RE = re.compile(r"^[A-Za-z_][A-Za-z0-9_]*$")
ARM_RE = re.compile(r"^[A-Za-z0-9_]+$")
CAMP_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9_.-]*$")
TAG_RE = re.compile(r"^[a-z][a-z0-9]*$")
MD5_RE = re.compile(r"^[0-9a-f]{32}$")
BAD_VALUE_RE = re.compile(r"[\s'\"\\$`]")
SYSCTL_RE = re.compile(r"^[a-z][a-z0-9_]*(\.[A-Za-z0-9_-]+)+$")
FN_RE = re.compile(r"^[a-z_][a-z0-9_]*$")
SMOKE_TAG = "s0"
# run-cell.sh N_VALIDATORS: the devnet sizes it can launch (devnet/wsl env.sh: val0..val6)
NODE_COUNTS = ("3", "4", "5", "6", "7")


def driver_functions():
    """every function the driver defines (its body and the cell-step.sh it sources): a campaign check must not
    take one of these names"""
    names = set()
    for f in (BODY, os.path.join(HERE, "..", "cell-step.sh")):
        names.update(re.findall(r"^([a-z_][a-z0-9_]*)\(\)", open(f).read(), re.M))
    return names


class SpecError(Exception):
    pass


def need(cond, msg):
    if not cond:
        raise SpecError(msg)


def keys_ok(obj, allowed, where, required=()):
    need(isinstance(obj, dict), "%s must be an object" % where)
    unknown = sorted(set(obj) - set(allowed) - set(required))
    need(
        not unknown,
        "%s: unknown key(s) %s (allowed: %s)"
        % (where, ", ".join(unknown), ", ".join(sorted(set(allowed) | set(required)))),
    )
    missing = sorted(set(required) - set(obj))
    need(not missing, "%s: missing required key(s) %s" % (where, ", ".join(missing)))


def abs_path(v, where):
    need(
        isinstance(v, str) and v.startswith("/") and not re.search(r"[\s'\"\\$`]", v),
        "%s must be an absolute path without whitespace, quotes or $ (got %r)"
        % (where, v),
    )
    return os.path.normpath(v)


def int_ge(v, lo, where):
    need(
        isinstance(v, int) and not isinstance(v, bool) and v >= lo,
        "%s must be an integer >= %d (got %r)" % (where, lo, v),
    )
    return v


def env_obj(d, where, harness=False):
    need(isinstance(d, dict), '%s must be an object NAME: "value"' % where)
    for k, v in d.items():
        need(NAME_RE.match(k), "%s: bad env name %r" % (where, k))
        need(
            isinstance(v, str), "%s: %s value must be a string (got %r)" % (where, k, v)
        )
        need(
            not BAD_VALUE_RE.search(v),
            "%s: %s=%r: no whitespace, quotes, backslash or $ in values"
            % (where, k, v),
        )
        need(
            k not in DRIVER_OWNED,
            "%s: %s is set by the driver from its preflight (report scans in-line or deferred, genesis reuse),"
            " after every spec and arm env: drop the key" % (where, k),
        )
        if harness:
            need(
                k not in RESERVED_HARNESS,
                "%s: %s is set by the driver itself" % (where, k),
            )
            need(
                k not in UNSUPPORTED_HARNESS,
                "%s: %s = crash/restart cells, not supported by the generator (hand-write the driver)"
                % (where, k),
            )
            # run-cell.sh refuses a non-empty cell dir unless OVERWRITE=1; the driver writes its records there
            # first and refuses an existing cell dir itself, so it runs every cell with OVERWRITE=1 (as Job 4)
            need(
                k != "OVERWRITE" or v == "1",
                "%s: OVERWRITE=%r: the driver runs every cell with OVERWRITE=1 (its records precede run-cell.sh"
                ' in the cell dir); drop the key or set it to "1"' % (where, v),
            )
    return dict(d)


def expect_list(v, where):
    need(isinstance(v, list), "%s must be a list" % where)
    out = []
    for i, e in enumerate(v):
        w = "%s[%d]" % (where, i)
        keys_ok(e, EXPECT_KEYS - {"file", "regex"}, w, required=("file", "regex"))
        f = e["file"]
        need(
            isinstance(f, str)
            and f
            and not f.startswith("/")
            and ".." not in f.split("/")
            and not re.search(r"[\s'\"\\$`]", f),
            "%s.file must be a path relative to the cell dir (got %r)" % (w, f),
        )
        rx = e["regex"]
        need(
            isinstance(rx, str) and rx and "\n" not in rx,
            "%s.regex must be a one-line string" % w,
        )
        try:
            re.compile(rx)
        except re.error as ex:
            raise SpecError("%s.regex %r: %s" % (w, rx, ex))
        lo = int_ge(e.get("min", 1), 0, w + ".min")
        hi = e.get("max")
        if hi is not None:
            int_ge(hi, lo, w + ".max")
        out.append({"file": f, "regex": rx, "min": lo, "max": hi})
    return out


def read_md5_row(path, row, where):
    """last line of md5s.txt whose first two columns equal `row` ("KEY SHA"): its third column"""
    need(
        isinstance(row, str) and len(row.split()) == 2,
        "%s must be 'KEY SHA' (first two md5s.txt columns)" % where,
    )
    k, s = row.split()
    try:
        lines = open(path).read().splitlines()
    except OSError as ex:
        raise SpecError("%s: cannot read md5_file %s: %s" % (where, path, ex))
    hits = [
        ln.split()[2]
        for ln in lines
        if len(ln.split()) >= 3 and ln.split()[:2] == [k, s]
    ]
    need(hits, "%s: no line '%s %s <md5>' in %s" % (where, k, s, path))
    need(
        MD5_RE.match(hits[-1]),
        "%s: %r in %s is not a 32-hex md5" % (where, hits[-1], path),
    )
    return hits[-1]


def host_checks(hc):
    """spec host_checks: true (default) / false / {min_mem_avail_gb, sysctl}. Returns None (off) or
    {"min_mem_avail_kb": int, "sysctl": {name: value}}"""
    need(
        isinstance(hc, (bool, dict)),
        "host_checks must be true, false or an object {min_mem_avail_gb, sysctl} (got %r)"
        % (hc,),
    )
    if hc is False:
        return None
    if hc is True:
        hc = {}
    keys_ok(hc, HOST_KEYS, "host_checks")
    mm = hc.get("min_mem_avail_gb", MIN_MEM_AVAIL_GB)
    need(
        isinstance(mm, (int, float)) and not isinstance(mm, bool) and mm >= 0,
        "host_checks.min_mem_avail_gb must be a number >= 0 (got %r)" % (mm,),
    )
    sc = hc.get("sysctl", {})
    need(
        isinstance(sc, dict),
        'host_checks.sysctl must be an object NAME: "value" (e.g. "vm.dirty_bytes": "268435456")',
    )
    out = {}
    for k, v in sc.items():
        need(
            SYSCTL_RE.match(k),
            "host_checks.sysctl: bad sysctl name %r (e.g. vm.dirty_bytes)" % (k,),
        )
        if isinstance(v, int) and not isinstance(v, bool):
            v = str(v)
        need(
            isinstance(v, str) and v and not BAD_VALUE_RE.search(v),
            "host_checks.sysctl: %s value must be a non-empty string or integer without whitespace, quotes,"
            " backslash or $ (got %r)" % (k, v),
        )
        out[k] = v
    return {"min_mem_avail_kb": int(round(mm * 1000000)), "sysctl": out}


def checks_spec(ck):
    """spec checks {source: /abs/file.sh, per_cell: [fn, ...]} -> {"source", "per_cell"} (None when absent)"""
    if ck is None:
        return None
    keys_ok(ck, CHECKS_KEYS - {"source", "per_cell"}, "checks", required=("source", "per_cell"))
    src = abs_path(ck["source"], "checks.source")
    fns = ck["per_cell"]
    need(
        isinstance(fns, list) and fns,
        "checks.per_cell must be a non-empty list of function names (got %r)" % (fns,),
    )
    taken = driver_functions()
    for fn in fns:
        need(
            isinstance(fn, str) and FN_RE.match(fn),
            "checks.per_cell: %r is not a function name (^[a-z_][a-z0-9_]*$)" % (fn,),
        )
        need(fns.count(fn) == 1, "checks.per_cell: %s appears twice" % fn)
        need(
            fn not in taken,
            "checks.per_cell: %s is a driver function (name the check something else)" % fn,
        )
    return {"source": src, "per_cell": list(fns)}


def cell_list(arms, replicates, order, duration_s, warmup):
    """[(tag, arm, dur, kind)]: warm-up first (if any), then the replicates.
    rotate: replicate r (1-based) starts at arm (r-1) mod n, so every arm sits in several positions (Job 4).
    fixed: the arm order of the spec every replicate. A list: explicit "TAG:ARM" items, tag "rN" -> "rN-ARM"."""
    cells = []
    if warmup:
        cells.append((warmup[0], warmup[1], warmup[2], "none"))
    if isinstance(order, list):
        for item in order:
            r, a = item.split(":")
            cells.append(("%s-%s" % (r, a), a, duration_s, "none"))
        return cells
    n = len(arms)
    for r in range(replicates):
        k = r % n if order == "rotate" else 0
        for a in arms[k:] + arms[:k]:
            cells.append(("r%d-%s" % (r + 1, a), a, duration_s, "none"))
    return cells


def validate(spec):
    """check every key; return the normalised plan (raises SpecError)"""
    keys_ok(spec, TOP_OPTIONAL, "spec", required=TOP_REQUIRED)
    p = {}
    need(
        isinstance(spec["campaign"], str) and CAMP_RE.match(spec["campaign"]),
        "campaign: letters, digits, _ . - (got %r)" % spec["campaign"],
    )
    need(
        isinstance(spec["label_prefix"], str) and CAMP_RE.match(spec["label_prefix"]),
        "label_prefix: letters, digits, _ . - (got %r)" % spec["label_prefix"],
    )
    p["campaign"] = spec["campaign"]
    p["label_prefix"] = spec["label_prefix"]
    p["comment"] = spec.get("comment", "")
    need(isinstance(p["comment"], str), "comment must be a string")
    p["results_root"] = abs_path(
        spec.get("results_root", os.path.expanduser("~/bench-results-matched")),
        "results_root",
    )
    for k in ("run_dir", "harness_worktree", "tools_dir"):
        p[k] = abs_path(spec[k], k)
    p["devnet_dir"] = abs_path(
        spec.get("devnet_dir", os.path.expanduser("~/torus-wsl-devnet")), "devnet_dir"
    )
    p["duration_s"] = int_ge(spec["duration_s"], 10, "duration_s")
    p["markets"] = int_ge(spec["markets"], 1, "markets")
    p["rate"] = int_ge(spec.get("rate", 76000), 1, "rate")
    # quiet() gates on a QUIET_SAMPLE_S (5 s) CPU busy sample, below max_busy_pct (s131: the 1-min load average
    # it used to read, below max_load, lagged ~2.4 min behind a host already idle after a cell). max_load: older
    # specs still validate; it gates nothing (main says so)
    ml = spec.get("max_load", 2)
    need(
        isinstance(ml, (int, float)) and not isinstance(ml, bool) and ml > 0,
        "max_load must be a number > 0",
    )
    p["max_load_ignored"] = "max_load" in spec
    mb = spec.get("max_busy_pct", 10)
    need(
        isinstance(mb, (int, float)) and not isinstance(mb, bool) and 0 < mb <= 100,
        "max_busy_pct must be a number in (0, 100] (got %r)" % (mb,),
    )
    p["max_busy_pct"] = mb
    # the Job 4 driver's samplers and host checks (default on); its watchers run on every cell regardless
    sp = spec.get("samplers", True)
    need(isinstance(sp, bool), "samplers must be true or false (got %r)" % (sp,))
    p["samplers"] = sp
    p["host_checks"] = host_checks(spec.get("host_checks", True))

    # binaries
    bins = spec["binaries"]
    need(
        isinstance(bins, dict) and bins,
        "binaries must be a non-empty object NAME: {...}",
    )
    p["binaries"] = {}
    for name, b in bins.items():
        w = "binaries.%s" % name
        need(ARM_RE.match(name), "%s: binary name: letters, digits, _" % w)
        keys_ok(b, BIN_KEYS - {"stage_dir"}, w, required=("stage_dir",))
        nb = {"stage_dir": abs_path(b["stage_dir"], w + ".stage_dir")}
        need(
            ("node_md5" in b) != ("node_row" in b),
            "%s: give exactly one of node_md5, node_row (+ md5_file)" % w,
        )
        need(
            not ("bench_md5" in b and "bench_row" in b),
            "%s: give at most one of bench_md5, bench_row" % w,
        )
        if "node_row" in b or "bench_row" in b:
            need("md5_file" in b, "%s: node_row / bench_row need md5_file" % w)
            mf = abs_path(b["md5_file"], w + ".md5_file")
        else:
            need("md5_file" not in b, "%s: md5_file without node_row / bench_row" % w)
        nb["node_md5"] = (
            b["node_md5"]
            if "node_md5" in b
            else read_md5_row(mf, b["node_row"], w + ".node_row")
        )
        need(
            isinstance(nb["node_md5"], str) and MD5_RE.match(nb["node_md5"]),
            "%s.node_md5 must be 32 lowercase hex digits" % w,
        )
        bm = b.get("bench_md5", "")
        if "bench_row" in b:
            bm = read_md5_row(mf, b["bench_row"], w + ".bench_row")
        need(
            bm == "" or (isinstance(bm, str) and MD5_RE.match(bm)),
            "%s.bench_md5 must be 32 lowercase hex digits" % w,
        )
        nb["bench_md5"] = bm
        need(
            isinstance(b.get("wrapper", False), bool),
            "%s.wrapper must be true or false" % w,
        )
        nb["wrapper"] = b.get("wrapper", False)
        p["binaries"][name] = nb

    # base env, harness env, absent names, expectations for every arm
    p["node_env"] = env_obj(spec.get("node_env", {}), "node_env")
    p["harness_env"] = env_obj(spec.get("harness_env", {}), "harness_env", harness=True)
    ae = spec.get("absent_env", [])
    need(
        isinstance(ae, list)
        and all(isinstance(x, str) and NAME_RE.match(x) for x in ae),
        "absent_env must be a list of env names",
    )
    p["absent_env"] = list(ae)
    p["expect"] = expect_list(spec.get("expect", []), "expect")

    # arms
    arms = spec["arms"]
    need(isinstance(arms, list) and arms, "arms must be a non-empty list")
    p["arms"] = []
    seen = set()
    for i, a in enumerate(arms):
        w = "arms[%d]" % i
        keys_ok(a, ARM_KEYS - {"label"}, w, required=("label",))
        lb = a["label"]
        need(
            isinstance(lb, str) and ARM_RE.match(lb),
            "%s.label: letters, digits, _ (got %r)" % (w, lb),
        )
        need(lb not in seen, "arms: duplicate arm label %r" % lb)
        seen.add(lb)
        w = "arm %s" % lb
        if "binary" in a:
            bn = a["binary"]
            need(
                bn in p["binaries"],
                "%s: binary %r is not in binaries (%s)"
                % (w, bn, ", ".join(sorted(p["binaries"]))),
            )
        else:
            need(
                len(p["binaries"]) == 1,
                "%s: name its binary (the spec has %d)" % (w, len(p["binaries"])),
            )
            bn = next(iter(p["binaries"]))
        env = env_obj(a.get("env", {}), w + ".env")
        xe = dict(p["node_env"])
        xe.update(env)
        henv = dict(p["harness_env"])
        henv.update(env_obj(a.get("harness_env", {}), w + ".harness_env", harness=True))
        need(isinstance(a.get("note", ""), str), "%s.note must be a string" % w)
        p["arms"].append(
            {
                "label": lb,
                "binary": bn,
                "env": env,
                "xe": xe,
                "henv": henv,
                "expect": expect_list(a.get("expect", []), w + ".expect"),
            }
        )
    labels = [a["label"] for a in p["arms"]]
    # the devnet's validator count: run-cell.sh N_VALIDATORS (3..7, default 3) from the arm's harness env. NODES
    # and the body's per-node loops are campaign-wide, so every arm (and with it the warm-up and smoke cells) must
    # run the same count
    nv = {a["label"]: a["henv"].get("N_VALIDATORS", "3") for a in p["arms"]}
    for lb, v in nv.items():
        need(
            v in NODE_COUNTS,
            "arm %s: N_VALIDATORS=%r: run-cell.sh runs %s to %s validators"
            % (lb, v, NODE_COUNTS[0], NODE_COUNTS[-1]),
        )
    need(
        len(set(nv.values())) == 1,
        "N_VALIDATORS differs between arms (%s): one node count per campaign (set it in the top-level harness_env)"
        % ", ".join("%s=%s" % kv for kv in nv.items()),
    )
    p["nodes"] = int(next(iter(nv.values())))
    need(
        spec["baseline"] in labels,
        "baseline %r is not an arm (%s)" % (spec["baseline"], ", ".join(labels)),
    )
    p["baseline"] = spec["baseline"]

    # order, replicates, warm-up, smoke
    order = spec.get("order", "rotate")
    if isinstance(order, list):
        need(
            "replicates" not in spec,
            "replicates: not allowed with an explicit order list (the list is the order)",
        )
        need(order, "order list is empty")
        tags = set()
        for it in order:
            need(
                isinstance(it, str) and it.count(":") == 1,
                "order item %r must be 'TAG:ARM' (e.g. r1:A)" % (it,),
            )
            t, a = it.split(":")
            need(
                TAG_RE.match(t),
                "order item %r: tag must be lowercase letters/digits (e.g. r1)" % it,
            )
            need(a in labels, "order item %r: arm %r is not an arm" % (it, a))
            need((t, a) not in tags, "order item %r appears twice" % it)
            tags.add((t, a))
        missing = [a for a in labels if not any(x == a for _, x in tags)]
        need(not missing, "order list has no cell for arm(s) %s" % ", ".join(missing))
        p["replicates"] = None
    else:
        need(
            order in ("rotate", "fixed"),
            'order must be "rotate", "fixed" or a list of \'TAG:ARM\' (got %r)'
            % (order,),
        )
        need("replicates" in spec, "replicates is required (order %s)" % order)
        p["replicates"] = int_ge(spec["replicates"], 1, "replicates")
    p["order"] = order
    wu = spec.get("warmup", {})
    p["warmup_gate"] = True
    if wu is False:
        p["warmup"] = None
    else:
        keys_ok(wu, WARMUP_KEYS, "warmup")
        p["warmup_gate"] = wu.get("gate", True)
        need(
            isinstance(p["warmup_gate"], bool),
            "warmup.gate must be true or false (got %r)" % (p["warmup_gate"],),
        )
        wa = wu.get("arm", p["baseline"])
        need(wa in labels, "warmup.arm %r is not an arm" % wa)
        wt = wu.get("tag", "w1")
        need(
            isinstance(wt, str)
            and TAG_RE.match(wt)
            and not wt.startswith("r")
            and wt != "s0",
            "warmup.tag must be lowercase letters/digits, not r* (replicates) or s0 (smoke) (got %r)"
            % (wt,),
        )
        p["warmup"] = (
            wt,
            wa,
            int_ge(wu.get("duration_s", p["duration_s"]), 10, "warmup.duration_s"),
        )
    sm = spec.get("smoke", {})
    keys_ok(sm, SMOKE_KEYS, "smoke")
    sa = sm.get("arm", p["baseline"])
    need(sa in labels, "smoke.arm %r is not an arm" % sa)
    p["smoke"] = (SMOKE_TAG, sa, int_ge(sm.get("duration_s", 60), 10, "smoke.duration_s"))
    an = spec.get("analysis", {})
    keys_ok(an, ANALYSIS_KEYS, "analysis")
    if an:
        need(
            isinstance(an.get("cmd"), str) and an["cmd"].strip(),
            "analysis.cmd must be a non-empty string",
        )
    p["analysis_cmd"] = an.get("cmd", "")
    p["analysis_metric"] = an.get("metric", "")
    need(isinstance(p["analysis_metric"], str), "analysis.metric must be a string")
    p["checks"] = checks_spec(spec.get("checks"))
    p["cells"] = cell_list(labels, p["replicates"], order, p["duration_s"], p["warmup"])
    return p


def q(v):
    return shlex.quote(str(v))


def kvs(d):
    return " ".join("%s=%s" % (k, v) for k, v in d.items())


def render(p, spec, spec_path):
    raw = json.dumps(spec, indent=1, sort_keys=False)
    sha = hashlib.sha256(raw.encode()).hexdigest()[:12]
    names = []
    for src in [p["node_env"]] + [a["env"] for a in p["arms"]]:
        names += [k for k in src if k not in names]
    names += [k for k in p["absent_env"] if k not in names]
    proc_names = ["LD_PRELOAD", "MALLOC_CONF"] + [
        k
        for k in names
        if not k.startswith("TORUS_") and k not in ("LD_PRELOAD", "MALLOC_CONF")
    ]
    L = [
        "#!/usr/bin/env bash",
        "# %s campaign driver — GENERATED by tools/matched-bench/campaign/make-driver.py from %s (spec sha256 %s)."
        % (p["campaign"], spec_path, sha),
        "# Do not edit: change the spec and regenerate. %s"
        % p["comment"].replace("\n", " "),
        "#   %s dry | show | launch smoke | launch     (modes: see the driver body below)"
        % "DRIVER",
        "# spec:",
    ]
    L += ["#   " + ln for ln in raw.splitlines()]
    L += [
        "# >>> GENERATED CONFIG",
        "# a caller-exported name must not survive into the config (export attribute, or a stray key: an exported",
        "# string BDIR=x would become BDIR[0]=x under declare -A); scrub_env un-exports these names, never unsets them",
        "unset -v %s" % " ".join(CONFIG_VARS),
        "CAMPAIGN=%s" % q(p["campaign"]),
        "R=%s" % q(p["results_root"]),
        "RUN=%s" % q(p["run_dir"]),
        "P=%s" % q(p["label_prefix"]),
        "WT_H=%s" % q(p["harness_worktree"]),
        "TOOLS=%s" % q(p["tools_dir"]),
        "DEVNET=%s" % q(p["devnet_dir"]),
        "MARKETS=%d" % p["markets"],
        "RATE=%d" % p["rate"],
        "MAX_BUSY_PCT=%s   # quiet(): CPU busy %% over a QUIET_SAMPLE_S sample must be below this" % p["max_busy_pct"],
        "NODES=%d" % p["nodes"],
        "BASELINE=%s" % q(p["baseline"]),
        "declare -A BDIR BMD5 BBMD5 BWRAP",
    ]
    for n, b in p["binaries"].items():
        L.append(
            "BDIR[%s]=%s; BMD5[%s]=%s; BBMD5[%s]=%s; BWRAP[%s]=%d"
            % (
                n,
                q(b["stage_dir"]),
                n,
                b["node_md5"],
                n,
                q(b["bench_md5"]),
                n,
                1 if b["wrapper"] else 0,
            )
        )
    L.append(
        "declare -A ABIN AENV AXE AHENV   # AENV the arm's own node env, AXE = node_env + AENV (run-cell.sh EXTRA_ENV), AHENV run-cell.sh env"
    )
    for a in p["arms"]:
        lb = a["label"]
        L.append("ABIN[%s]=%s; AENV[%s]=%s" % (lb, a["binary"], lb, q(kvs(a["env"]))))
        L.append("AXE[%s]=%s" % (lb, q(kvs(a["xe"]))))
        L.append("AHENV[%s]=%s" % (lb, q(kvs(a["henv"]))))
    L.append("ARMS=(%s)" % " ".join(a["label"] for a in p["arms"]))
    L.append(
        "ENV_NAMES=%s   # checked on every arm: set (its value) or unset"
        % q(" ".join(names))
    )
    L.append(
        "PROC_REC_RE=%s   # recorded into node-preload.txt" % q("|".join(proc_names))
    )
    exp = [("*", e) for e in p["expect"]] + [
        (a["label"], e) for a in p["arms"] for e in a["expect"]
    ]
    L.append("EXP_ARM=(%s)" % " ".join(q(x[0]) for x in exp))
    L.append("EXP_FILE=(%s)" % " ".join(q(x[1]["file"]) for x in exp))
    L.append("EXP_RE=(%s)" % " ".join(q(x[1]["regex"]) for x in exp))
    L.append("EXP_MIN=(%s)" % " ".join(str(x[1]["min"]) for x in exp))
    L.append(
        "EXP_MAX=(%s)"
        % " ".join(q("" if x[1]["max"] is None else x[1]["max"]) for x in exp)
    )
    L.append(
        "# tag:arm:dur:kind:killat (cell_step spec; rep = the tag up to its first '-')"
    )
    L.append("CELLS=( %s )" % " ".join(q("%s:%s:%d:%s:" % c) for c in p["cells"]))
    L.append("SMOKE_CELL=%s" % q("%s:%s:%d:none:" % p["smoke"]))
    L.append("ANALYSIS_CMD=%s" % q(p["analysis_cmd"]))
    L.append("ANALYSIS_METRIC=%s" % q(p["analysis_metric"]))
    hc = p["host_checks"]
    L.append(
        "SAMPLERS=%d   # per-cell du/memory/metrics/fd/soak-1hz/vmstat samplers, RocksDB LOG copies, the fd stop"
        % (1 if p["samplers"] else 0)
    )
    L.append(
        "HOST_CHECKS=%d   # MemAvailable preflight, sysctls at each cell's start and end, analysis-process quiet"
        % (0 if hc is None else 1)
    )
    L.append("MIN_MEM_AVAIL_KB=%d" % (0 if hc is None else hc["min_mem_avail_kb"]))
    L.append(
        "declare -A SYSCTL_WANT   # required sysctl values; the dirty limits not given here are pinned at the run's preflight"
    )
    for k, v in sorted((hc or {}).get("sysctl", {}).items()):
        L.append("SYSCTL_WANT[%s]=%s" % (k, q(v)))
    gate = []
    if p["warmup_gate"]:
        gate = ([p["warmup"][0]] if p["warmup"] else []) + [SMOKE_TAG]
    L.append(
        "GATE_TAGS=%s   # warm-up gate (spec warmup.gate): these cells must pass it or the campaign stops"
        % q(" ".join(gate))
    )
    ck = p["checks"] or {"source": "", "per_cell": []}
    L.append("CHECKS_SOURCE=%s   # spec checks.source (sourced at preflight)" % q(ck["source"]))
    L.append(
        "CHECK_FNS=%s   # spec checks.per_cell: each runs after every cell as FN LABEL ARM"
        % q(" ".join(ck["per_cell"]))
    )
    L.append("# <<< GENERATED CONFIG")
    return "\n".join(L) + "\n" + body_text(p["nodes"])


def body_text(n):
    """driver-body.sh for an n-node devnet: its @@NAME@@ tokens are the per-node literals (the metrics ports, the
    val<i> loops, fdsum's columns, soakchk's expected scrapes). n = 3 gives the Job 4 drivers' text, byte for byte."""
    idx = range(n)
    col = range(2, n + 2)  # fds.txt: epoch, then one column per node
    tok = {
        "NODES": str(n),
        "NODE_IDX": " ".join(str(i) for i in idx),
        "METS": " ".join(str(9161 + i) for i in idx),
        "FD_LAST": str(n + 1),
        "FD_FMT": " ".join("val%d %%d/%%d" % i for i in idx),
        "FD_ARGS": ", ".join("m[%d], e[%d]" % (c, c) for c in col),
        "FD_STOPS": " || ".join("m[%d] > stop" % c for c in col),
    }
    body = open(BODY).read()
    body = re.sub(r"@@([A-Z_]+)@@", lambda m: tok[m.group(1)], body)
    assert "@@" not in body, "driver-body.sh: an unresolved @@ token"
    return body


def main():
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    ap.add_argument("spec")
    ap.add_argument("-o", "--out", required=True)
    a = ap.parse_args()
    try:
        spec = json.load(open(a.spec))
    except (OSError, ValueError) as ex:
        print("SPEC ERROR: cannot read %s: %s" % (a.spec, ex), file=sys.stderr)
        return 2
    try:
        p = validate(spec)
    except SpecError as ex:
        print("SPEC ERROR: %s" % ex, file=sys.stderr)
        return 2
    if p["max_load_ignored"]:
        print(
            "NOTE: spec max_load (%s) is ignored: quiet() gates on a CPU busy sample below max_busy_pct (%s)"
            % (spec["max_load"], p["max_busy_pct"]),
            file=sys.stderr,
        )
    txt = render(p, spec, os.path.abspath(a.spec))
    tmp = a.out + ".tmp"
    with open(tmp, "w") as f:
        f.write(txt)
    os.chmod(tmp, 0o755)
    os.replace(tmp, a.out)
    print(
        "wrote %s: %d cells (%s), arms %s, baseline %s"
        % (
            a.out,
            len(p["cells"]),
            " ".join(c[0] for c in p["cells"]),
            " ".join(x["label"] for x in p["arms"]),
            p["baseline"],
        )
    )
    print(
        "next: %s dry; %s launch smoke (read its CHECK lines); then %s launch"
        % (a.out, a.out, a.out)
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
