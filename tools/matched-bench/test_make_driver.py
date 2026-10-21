#!/usr/bin/env python3
"""test_make_driver.py — tests for campaign/make-driver.py (spec -> campaign driver) and its driver body.

    python3 tools/matched-bench/test_make_driver.py

Offline, no nodes, no cargo: drivers are generated into a tempdir, checked with `bash -n`, and their
functions are run in "library mode" (DRIVER_LIB_ONLY=1 source DRIVER) against fixture cell dirs and a
fake /proc (DRIVER_PROC). Pinned:

1. golden: the Job 4 horizon and decay specs (testdata/make-driver/p4j4-{h,d}.json) give the same arms,
   node env, harness env, cell order, replicates, markets, md5 and env checks as the hand-derived drivers
   ozarchy-p4j4-{h,d}-campaign.sh (copied verbatim into testdata/make-driver/), compared as tables.
2. the env check reads TORUS_* from node-env-all.txt and every other process env (MALLOC_CONF) from
   node-preload.txt (Job 4's false STOP read MALLOC_CONF from node-env-all.txt).
3. the md5 check hashes the running exe (/proc/PID/exe), never the staged file, which may be a wrapper
   script (g3's 4 needless reruns hashed the g-je wrapper).
4. a bad spec (unknown key, missing baseline arm, duplicate arm, ...) is a hard error.
5. preflight fails when the frozen tools copy lacks the genesis inputs run-cell.sh reads from ITS OWN repo
   root (tools_dir/../..), or when they differ from the harness worktree's (Codex P2: a standalone tools
   copy passed preflight, then every cell died at genesis, exit 127).
6. a caller-exported name (RUN, P, ARMS, ...) never deletes or overrides driver config: scrub_env un-exports
   driver names and clears only the children's env (Codex P2: RUN exported -> scrub_env -> RUN unbound).
7. the runtime watchers of the hand-written Job 4 driver are in every generated driver (s123: driver-body.sh had
   none, so a node that died mid-cell was never flagged, the cell just looked slow): the golden check compares
   the copied functions (watch_nodes, death, stopwin, memguard, hardscan, the samplers) with the Job 4 drivers
   verbatim, and the cell() wiring (harness pid, watchers, samplers, the node-death branch) in order.
8. a node that dies mid-cell (a stub pid that exits) gives <cell>/node-death.txt in the Job 4 format, a TERMed
   harness, a "CELL FAIL ... death" line, and a cells.tsv row that cell_step flags death (+ a queued rerun);
   the campaign goes on. Run for real: the generated cell() against a fake detach.sh / systemctl / run-cell.sh.
9. memguard stops the cell unit when MemAvailable < 6 GB (a stubbed /proc/meminfo), and only then.
10. the spec options samplers / host_checks (default on): sampler records (with the hand drivers' 1 Hz soak
   sampler: metrics-1hz.txt.gz + host-1hz.tsv, started and stopped with every cell, soakchk's CHECK WARN on too
   few samples), the fd stop, the sysctl record and check (spec values, else pinned at the run's preflight), the
   MemAvailable preflight; off = none of them.
13. a top-level declare -A / -a without -g in the checks file is a PREFLIGHT WARN (sourced inside a function it
   is a local of load_checks: the checks still get it, nothing else does); the preflight goes on.
14. check scoping (Codex P2: bash variables are dynamic, so `rc=1; guardchk() { return "$rc"; }` read
   campaignchecks' rc=0 and passed): a check sees its file's top-level values (also under names that are locals of
   cell / run_spec / run_cells / campaignchecks, or of the preflight frames the file is sourced from), none of the
   driver locals the file never set, still the driver globals and its label / arm; the _DRV_ prefix is refused.
11. the warm-up gate (spec warmup.gate, default on): a warm-up or smoke cell that is not run-cell rc 0, validity
   ACCEPT, agreement AGREE, liveness PASS and 0 panic/fail-stop lines stops the campaign before any measured cell
   (s58 warmgate; before, a failed warm-up was a row and the measured cells ran anyway). gate false = a row only.
12. campaign checks (spec checks {source, per_cell}): the source file is sourced at preflight (each function must
   be defined, no driver function or name redefined), each function runs after every cell as `fn <label> <arm>`
   with the driver's R / AENV / ...; non-zero = CHECK FAIL, the cell's row bad:check, the campaign stops.
15. post-cell work (bench queue pacing rule, s131): the POST_INLINE steps run in cell() / run_spec; with the
   scans deferred (DEFER_SCANS=1) the POST_ENDPASS ones (cell-endpass.sh, hardscan, soakchk) run only in end_pass,
   which run_campaign calls once after the last cell (also on a CAMPAIGN STOP) and before the EXIT trap's
   campaign.done; each cell gets DEFER_REPORT_SCANS=1 and GENESIS_CACHE_DIR=$RUN/genesis-cache when run-cell.sh
   supports them (preflight_post; an expectation, checks file or analysis script that reads a deferred output
   keeps the scans in-line: DeferredReadersTest).
   The driver owns both (Codex round 6): set after the arm's harness env, both ways, never from a spec or the
   inherited env; make-driver.py refuses them in every env object (DriverOwnedHarnessVarsTest).
16. in-line (DEFER_SCANS=0; owner rule after Codex round 4 on d6647299): each cell runs exactly the pre-end-pass
   driver's (0f2ca85b^) steps, in its order, with the same inputs, and the end pass does nothing
   (InlineSequenceTest: both bodies from one spec, run against tracing fakes, the traces compared).
"""

import glob
import gzip
import http.server
import importlib.util
import json
import os
import re
import shlex
import shutil
import signal
import subprocess
import sys
import tempfile
import threading
import time
import types
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
GEN = os.path.join(HERE, "campaign", "make-driver.py")
EXAMPLE = os.path.join(HERE, "campaign", "driver-spec.example.json")
FIX = os.path.join(HERE, "testdata", "make-driver")
CELLFIX = os.path.join(HERE, "testdata", "cell-step")
NODE_MD5_H = "03a2b75727727f972cf5e80865646f0d"
# preflight's analysis script check is advisory (s131): these NOTEs, never a PREFLIGHT FAIL
NOTE_MISSING = "PREFLIGHT NOTE: analysis script %s not found (not checked further)"
NOTE_NOSPLIT = "PREFLIGHT NOTE: analysis.cmd does not split as cell_step splits it (shlex.split; not checked further)"


def load_gen():
    spec = importlib.util.spec_from_file_location("make_driver", GEN)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def md5_of(path):
    import hashlib

    return hashlib.md5(open(path, "rb").read()).hexdigest()


# the genesis inputs run-cell.sh reads from its own repo root (MAINREPO = <tools_dir>/../..)
GENESIS_FILES = {
    "devnet/wsl/gen-3val-genesis.sh": "#!/bin/sh\necho fake gen-3val-genesis\n",
    "devnet/wsl/bench-validator-keys.json": '{"keys": "fake"}\n',
    "devnet/wsl/launch-3val.sh": "#!/bin/sh\necho fake launch\n",
    "devnet/genesis.json": '{"genesis": "fake"}\n',
    "testnet/lib/cargo-bin.sh": "cargo_bin() { echo fake; }\n",
    "testnet/gen-weighted-genesis.sh": "#!/bin/sh\necho fake weighted\n",
}
TOOL_FILES = [
    "run-cell.sh",
    "cell-step.sh",
    "cell_step.py",
    "campaign/detach.sh",
    "soak-sampler.py",
    "log-scans.sh",
    "cell-endpass.sh",
    "genesis_cache.py",
]


def write_tree(root, files):
    for rel, body in files.items():
        f = os.path.join(root, rel)
        os.makedirs(os.path.dirname(f), exist_ok=True)
        with open(f, "w") as fh:
            fh.write(body)
        if rel.endswith(".sh"):
            os.chmod(f, 0o755)


def frozen_fixture(tmp, complete=True):
    """a fake harness worktree tmp/wt (its tracked genesis inputs) and a standalone frozen tools copy
    tmp/ft/tools/matched-bench (copies of the tools the driver runs). complete: the frozen root tmp/ft also
    carries the harness's genesis inputs + testnet/genesis-weighted-full.json, as Job 4's ozarchy-p4j4-tools
    did. Returns (wt, tools)."""
    wt = os.path.join(tmp, "wt")
    if not os.path.exists(os.path.join(wt, "devnet")):
        write_tree(wt, GENESIS_FILES)
    ft = os.path.join(tmp, "ft")
    tools = os.path.join(ft, "tools", "matched-bench")
    for rel in TOOL_FILES:
        dst = os.path.join(tools, rel)
        if not os.path.exists(dst):
            os.makedirs(os.path.dirname(dst), exist_ok=True)
            shutil.copy2(os.path.join(HERE, rel), dst)
    if complete:
        write_tree(ft, GENESIS_FILES)
        write_tree(
            ft, {"testnet/genesis-weighted-full.json": '{"native_balances": []}\n'}
        )
    return wt, tools


def minimal_spec(tmp, **over):
    """a valid one-binary two-arm spec whose paths all live in tmp (the stage dir holds a fake node; the tools
    are a complete frozen copy, frozen_fixture)"""
    st = os.path.join(tmp, "stage", "b-run", "release")
    os.makedirs(st, exist_ok=True)
    node = os.path.join(st, "torus-node")
    if not os.path.exists(node):
        with open(node, "wb") as f:
            f.write(b"\x7fELF fake node binary\x00\x01")
        os.chmod(node, 0o755)
        with open(os.path.join(st, "bench-throughput"), "wb") as f:
            f.write(b"\x7fELF fake bench\x00")
        os.chmod(os.path.join(st, "bench-throughput"), 0o755)
    wt, tools = frozen_fixture(tmp)
    s = {
        "campaign": "test-camp",
        "results_root": os.path.join(tmp, "res"),
        "run_dir": os.path.join(tmp, "res", "test-camp-run"),
        "label_prefix": "test-camp-10m",
        "harness_worktree": wt,
        "tools_dir": tools,
        "devnet_dir": os.path.join(tmp, "devnet"),
        "binaries": {"b": {"stage_dir": os.path.dirname(st), "node_md5": md5_of(node)}},
        "markets": 10,
        "duration_s": 120,
        "replicates": 2,
        "baseline": "A",
        "arms": [
            {"label": "A"},
            {
                "label": "M1",
                "env": {"MALLOC_CONF": "dirty_decay_ms:1000", "TORUS_X_KNOB": "7"},
            },
        ],
    }
    s.update(over)
    return s


class Base(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.mkdtemp(prefix="make-driver-test-")
        self.gen = load_gen()

    def tearDown(self):
        shutil.rmtree(self.tmp, ignore_errors=True)

    def write_spec(self, spec, name="spec.json"):
        p = os.path.join(self.tmp, name)
        with open(p, "w") as f:
            json.dump(spec, f)
        return p

    def generate(self, spec_path, name="driver.sh"):
        out = os.path.join(self.tmp, name)
        r = subprocess.run(
            [sys.executable, "-I", GEN, spec_path, "-o", out],
            capture_output=True,
            text=True,
        )
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        return out

    def lib(self, driver, script, env=None):
        """run `script` after sourcing the driver in library mode (functions + config only)"""
        e = dict(os.environ)
        e.update(env or {})
        src = "DRIVER_LIB_ONLY=1; source %s || exit 99\n%s" % (
            shlex.quote(driver),
            script,
        )
        return subprocess.run(
            ["bash", "-c", src], capture_output=True, text=True, env=e, timeout=120
        )

    def plan(self, driver, smoke=False):
        r = self.lib(driver, "show_plan%s" % (" smoke" if smoke else ""))
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        return r.stdout.splitlines()


# ---------------------------------------------------------------- golden (Job 4)


def kv_dict(s):
    d = {}
    for kv in s.split():
        k, _, v = kv.partition("=")
        d[k] = v
    return d


def ref_tables(path):
    """the arm / env / order tables of a hand-derived Job 4 driver (campaign-template lineage)"""
    t = open(path).read()
    aenv = dict(re.findall(r'^AENV\[(\w+)\]="([^"]*)"$', t, re.M))
    arms = {}
    for m in re.finditer(
        r"^arm (\S+) (\S+) (\S+) (\S+) (\S+) (\S+) (\S+) (\S+)$", t, re.M
    ):
        a, b, tail, _knob, _prof, cloid, orc, mk = m.groups()
        arms[a] = {"bin": b, "tail": tail, "cloid": cloid, "orc": orc, "mk": mk}
    knobs = re.search(r'^KNOBS="([^"]*)"$', t, re.M).group(1)
    cells_txt = re.search(r"^CELLS=\( (.*) \)$", t, re.M).group(1)
    cells = [tuple(c.split(":")[:4]) for c in re.findall(r'"([^"]+)"', cells_txt)]
    run, prefix = re.search(r"^RUN=\$R/(\S+); P=(\S+)$", t, re.M).groups()
    budget = re.search(r"^BUDGET=(\d+)$", t, re.M).group(1)
    depth = re.search(r"^DEPTH=(\d+)", t, re.M).group(1)
    sha_b = re.search(r"^SHA_B=(\w+)$", t, re.M).group(1)
    md5_row = re.search(
        r'BMD5\[b\]=\$\(awk -v s="\$\{SHA_B:0:8\}" \'\$1=="(\w+)"', t
    ).group(1)
    # the harness env of cell(): E=(...) with the driver's per-arm values resolved (N = the launch arg 4)
    e_txt = re.search(r"^    E=\((.*?)\)\n", t, re.M | re.S).group(1)
    henv_tpl = dict(kv.split("=", 1) for kv in shlex.split(e_txt))
    # env checks: armenvchk's absent list (node-env-all.txt) and heapchk's MALLOC_CONF
    ae = re.search(r"^armenvchk\(\) \{.*?^\}\n", t, re.M | re.S).group(0)
    absent_torus = re.search(r"for k in ([A-Z_ ]+); do", ae).group(1).split()
    i = t.index('armenvchk "$label"')
    assert "node-env-all.txt" in t[i : i + 120]
    hc = re.search(r"^heapchk\(\) \{.*?^\}\n", t, re.M | re.S).group(0)
    # the file heapchk reads MALLOC_CONF from: node-preload.txt in the fixed d driver; node-env-all.txt
    # (TORUS_* only, so the check is vacuous) in the h driver, made before Job 4's false STOP fixed d
    hc_files = set(re.findall(r'MALLOC_CONF=[^"]*" "\$o/(node-[a-z-]+\.txt)"', hc))
    assert len(hc_files) == 1, hc
    return {
        "aenv": aenv,
        "arms": arms,
        "knobs": knobs,
        "cells": cells,
        "run": run,
        "prefix": prefix,
        "budget": budget,
        "depth": depth,
        "sha_b": sha_b,
        "md5_row": md5_row,
        "henv_tpl": henv_tpl,
        "absent_torus": absent_torus,
        "malloc_file": hc_files.pop(),
    }


def ref_harness_env(ref, arm):
    a = ref["arms"][arm]
    sub = {
        "$mif": "4",
        "$bud": ref["budget"],
        "${AORC[$arm]}": a["orc"],
        "$tail": a["tail"],
        "$DEPTH": ref["depth"],
        "${ACLOID[$arm]}": a["cloid"],
    }
    out = {}
    for k, v in ref["henv_tpl"].items():
        if k in ("TARGET_DIR", "TOOLS_DIR", "DATA_ROOT"):
            continue  # paths: compared separately
        out[k] = sub.get(v, v)
    return out


def ref_checks(ref, arm):
    """(present, absent) env checks of the reference driver for one arm: name -> (value, file)"""
    present = {}
    for k, v in kv_dict(ref["knobs"]).items():
        present[k] = (v, "node-env-all.txt")  # envok / knobok / statschk
    for k, v in kv_dict(ref["aenv"][arm]).items():
        present[k] = (
            v,
            ref["malloc_file"] if k == "MALLOC_CONF" else "node-env-all.txt",
        )
    absent = {k: "node-env-all.txt" for k in ref["absent_torus"] if k not in present}
    if "MALLOC_CONF" not in present:
        absent["MALLOC_CONF"] = ref["malloc_file"]
    return present, absent


def gen_tables(lines):
    arms, henv, present, absent, cells = {}, {}, {}, {}, []
    paths = {}
    for ln in lines:
        f = ln.split(" ")
        if f[0] == "ARM":
            arms[f[1]] = {
                "bin": f[2],
                "md5": f[3],
                "xe": ln.split(" xe=", 1)[1] if " xe=" in ln else "",
            }
        elif f[0] == "HENV":
            henv.setdefault(f[1], {})
            if len(f) > 2:
                k, _, v = f[2].partition("=")
                henv[f[1]][k] = v
        elif f[0] == "PRESENT":
            k, _, v = f[2].partition("=")
            present.setdefault(f[1], {})[k] = (v, f[3])
        elif f[0] == "ABSENT":
            absent.setdefault(f[1], {})[f[2]] = f[3]
        elif f[0] == "CELL":
            cells.append((f[1], f[2], f[3], f[4]))
        elif f[0] == "PATH":
            paths[f[1]] = f[2]
    return arms, henv, present, absent, cells, paths


# the Job 4 drivers' runtime watchers and samplers, copied verbatim into driver-body.sh. memguard reads
# "$PROC/meminfo" (DRIVER_PROC: a test stubs it) where the hand driver read /proc/meminfo; watch_nodes keeps its
# restart-cell branch (CRASHCELL=0 in the body: restart cells are a SPEC ERROR)
VERBATIM_FUNCS = (
    "stopbg",
    "death",
    "lastline",
    "watch_nodes",
    "stopwin",
    "memguard",
    "hardscan",
    "dusamp",
    "ckptsamp",
    "memsamp",
    "fdsampler",
    "fdsum",
    "dumeas",
    "savelog",
)
FUNC_SUBST = {
    "memguard": ("/proc/meminfo", '"$PROC/meminfo"'),
    # run-cell.sh KILL_NO_RESTART (s134): the one-validator-down baseline kills a validator for good; its death is
    # not a mid-cell node death once crash-kill.sh norestart logged the marker for exactly that pid
    "watch_nodes": (
        '            fi\n            if ! kill -0 "$p" 2>/dev/null; then\n',
        '            fi\n'
        "            # run-cell.sh KILL_NO_RESTART: this exact pid's death is expected once crash-kill.sh norestart"
        " logged its marker\n"
        '            if ! kill -0 "$p" 2>/dev/null && grep -qF "kill-no-restart: SIGKILL val$i pid $p "'
        ' "$o/run.log" 2>/dev/null; then\n'
        "                i=$((i+1)); continue\n"
        "            fi\n"
        '            if ! kill -0 "$p" 2>/dev/null; then\n',
    ),
}
# cell(): the Job 4 drivers' watcher and sampler wiring, in their order (lines compared stripped)
CELL_WIRING = [
    'dusamp "$R/$label" & DP=$!',
    'ckptsamp "$R/$label" & KP=$!',
    'memsamp "$R/$label" & MP=$!',
    'fdsampler "$R/$label" & FP=$!',
    'python3 -I "$SOAKPY" "$R/$label" "${METS[@]}" & SKP=$!',
    'timeout $((dur + 1200)) vmstat -n -t 1 > "$R/$label/vmstat-1hz.txt" 2>&1 & VMP=$!',
    'until mp=$(systemctl --user show -p MainPID --value "$unit.service" 2>/dev/null) && [ -n "$mp" ] && [ "$mp" != 0 ] \\',
    '&& HP=$(pgrep -P "$mp" | head -1) && [ -n "$HP" ]; do',
    'watch_nodes "$R/$label" "$HP" & WP=$!',
    'memguard "$unit" & GP=$!',
    'until [ -e "$rcf" ] || ! systemctl --user is-active -q "$unit.service"; do sleep 2; done',
    'wait "$WP"',
    'savelog "$label"',
    'if [ -e "$R/$label/node-death.txt" ]; then',
    'hardscan "$label"',
]
# the Job 4 cell() wiring that a generated driver runs in end_pass instead when the report scans are deferred
# (DEFER_SCANS=1; bench queue pacing rule s131: once after the campaign's last cell): checked there too; in-line
# (DEFER_SCANS=0) the generated cell() runs it where Job 4 did (InlineSequenceTest)
CELL_WIRING_ENDPASS = ['hardscan "$label"']
# cell(): the Job 4 drivers' failure handling around them (substrings)
CELL_HANDLING = [
    'sysctlchk "$label" start || { CELL_FAIL=abort-sysctl; return 1; }',
    "CELL_FAIL=abort-no-pid",
    'CELL FAIL $label rc=$rc death: $(cat "$R/$label/node-death.txt") -> continuing',
    "CELL_FAIL=abort-node-left",
    'echo "[$label] $(fdsum "$label")" || true; fdsum "$label" > /dev/null || fdok=1',
    'sysctlchk "$label" end',
    "STOP: a node held > $FD_STOP open fds in $label",
    "CELL_FAIL=stop-fd",
]


def func_text(text, name):
    """the bash function `name() {...}` of a driver: its line (a one-line function), else from its first line to
    the next line that is just '}'"""
    m = re.search(r"^%s\(\) \{[^\n]*\n" % re.escape(name), text, re.M)
    if not m:
        return None
    if m.group(0).rstrip().endswith("; }"):
        return m.group(0)
    return text[m.start() : text.index("\n}\n", m.start()) + 3]


class GoldenTest(Base):
    def assert_watchers(self, kind, drv):
        """the regenerated Job 4 driver carries the hand driver's watcher blocks (s123: node-death.txt was never
        written by a generated driver, so cell_step's death flag could not fire)"""
        hand = open(os.path.join(FIX, "ozarchy-p4j4-%s-campaign.sh" % kind)).read()
        gen = open(drv).read()
        for name in VERBATIM_FUNCS:
            h, g = func_text(hand, name), func_text(gen, name)
            self.assertIsNotNone(h, "%s is not in the Job 4 %s driver" % (name, kind))
            self.assertIsNotNone(g, "%s is missing from the generated driver" % name)
            if name in FUNC_SUBST:
                self.assertIn(FUNC_SUBST[name][0], h)
                h = h.replace(*FUNC_SUBST[name])
            self.assertEqual(
                g, h, "%s differs from the Job 4 %s driver's" % (name, kind)
            )
        for who, txt in (("Job 4 " + kind, hand), ("generated", gen)):
            body = func_text(txt, "cell")
            lines = [ln.strip() for ln in body.splitlines()]
            wiring = CELL_WIRING
            if who == "generated":
                ep = [ln.strip() for ln in func_text(txt, "end_pass").splitlines()]
                for m in CELL_WIRING_ENDPASS:
                    self.assertIn(m, ep, "generated end_pass lacks %s" % m)
            self.assertEqual(
                [m for m in wiring if m not in lines], [], "%s cell()" % who
            )
            idx = [lines.index(m) for m in wiring]
            self.assertEqual(idx, sorted(idx), "%s cell(): wiring order" % who)
            for s in CELL_HANDLING:
                self.assertIn(s, body, "%s cell()" % who)
        # the values: the phase's dirty limits (sysctlchk), the preflight MemAvailable floor, the fd stop
        plan = self.plan(drv)
        phase = re.search(r"^PHASE=(\d+)$", hand, re.M).group(1)
        dirty, dbg = re.search(
            r"^\s*%s\) DIRTY_WANT=(\d+); DBG_WANT=(\d+) ;;$" % phase, hand, re.M
        ).groups()
        mem = re.search(
            r"/proc/meminfo\)\" -ge (\d+) \] \|\| PF \"MemAvailable", hand
        ).group(1)
        fd = re.search(r"^FD_STOP=(\d+)$", hand, re.M).group(1)
        self.assertIn("WATCH node-death memguard hardscan", plan)
        self.assertIn(
            "SAMPLERS on du mem metrics fd soak-1hz vmstat rocksdb-log fd_stop=%s" % fd,
            plan,
        )
        self.assertIn(
            "SOAK on %s/soak-sampler.py metrics-1hz.txt.gz host-1hz.tsv"
            % json.load(open(os.path.join(FIX, "p4j4-%s.json" % kind)))["tools_dir"],
            plan,
        )
        self.assertIn("HOSTCHECK on min_mem_avail_kb=%s" % mem, plan)
        self.assertIn("SYSCTL vm.dirty_bytes %s" % dirty, plan)
        self.assertIn("SYSCTL vm.dirty_background_bytes %s" % dbg, plan)

    def golden(self, kind):
        ref = ref_tables(os.path.join(FIX, "ozarchy-p4j4-%s-campaign.sh" % kind))
        # the one intended difference: MALLOC_CONF is read from node-preload.txt (the h driver read node-env-all.txt)
        self.assertEqual(
            ref["malloc_file"], {"h": "node-env-all.txt", "d": "node-preload.txt"}[kind]
        )
        ref["malloc_file"] = "node-preload.txt"
        drv = self.generate(
            os.path.join(FIX, "p4j4-%s.json" % kind), "p4j4-%s.sh" % kind
        )
        r = subprocess.run(["bash", "-n", drv], capture_output=True, text=True)
        self.assertEqual(r.returncode, 0, r.stderr)
        arms, henv, present, absent, cells, paths = gen_tables(self.plan(drv))
        # arms, binary, md5 (the reference reads md5s.txt row N bdd59888 -> the Job 4 node md5)
        self.assertEqual(sorted(arms), sorted(ref["aenv"]))
        self.assertEqual(ref["md5_row"], "N")
        for a in arms:
            self.assertEqual(arms[a]["md5"], NODE_MD5_H, a)
            # node env = KNOBS + the arm's AENV (the reference's xe), as a dict
            want = kv_dict(ref["knobs"])
            want.update(kv_dict(ref["aenv"][a]))
            self.assertEqual(kv_dict(arms[a]["xe"]), want, a)
            self.assertEqual(henv[a], ref_harness_env(ref, a), a)
            rp, ra = ref_checks(ref, a)
            self.assertEqual(present.get(a, {}), rp, a)
            self.assertEqual(absent.get(a, {}), ra, a)
        # order, replicates, durations, kinds, warm-up first
        self.assertEqual(cells, ref["cells"])
        self.assertEqual(paths["RUN"], "/home/oz/bench-results-matched/" + ref["run"])
        self.assertEqual(paths["P"], ref["prefix"])
        self.assertEqual(paths["MARKETS"], ref["arms"][cells[0][1]]["mk"])
        self.assertEqual(paths["RATE"], "76000")
        self.assert_watchers(kind, drv)
        return ref, cells

    def test_golden_horizon(self):
        _, cells = self.golden("h")
        self.assertEqual(len(cells), 16)

    def test_golden_decay(self):
        _, cells = self.golden("d")
        self.assertEqual(len(cells), 10)

    def test_example_spec_generates(self):
        drv = self.generate(EXAMPLE, "example.sh")
        r = subprocess.run(["bash", "-n", drv], capture_output=True, text=True)
        self.assertEqual(r.returncode, 0, r.stderr)

    def test_rotation_and_fixed_order(self):
        rot = self.gen.cell_list(["A", "B", "C"], 4, "rotate", 120, ("w1", "A", 60))
        self.assertEqual(
            [c[0] for c in rot],
            [
                "w1",
                "r1-A",
                "r1-B",
                "r1-C",
                "r2-B",
                "r2-C",
                "r2-A",
                "r3-C",
                "r3-A",
                "r3-B",
                "r4-A",
                "r4-B",
                "r4-C",
            ],
        )
        fx = self.gen.cell_list(["A", "B"], 2, "fixed", 120, None)
        self.assertEqual([c[0] for c in fx], ["r1-A", "r1-B", "r2-A", "r2-B"])
        ex = self.gen.cell_list(
            ["A", "B"], 2, ["r1:A", "r1:B", "r2:B", "r2:A"], 120, None
        )
        self.assertEqual(
            [c[:2] for c in ex],
            [("r1-A", "A"), ("r1-B", "B"), ("r2-B", "B"), ("r2-A", "A")],
        )


# ---------------------------------------------------------------- env check


def node_files(cell, torus_lines, preload_lines):
    os.makedirs(cell, exist_ok=True)
    with open(os.path.join(cell, "node-env-all.txt"), "w") as f:
        f.write(
            "".join("pid=%d %s\n" % (100 + i, ln) for i, ln in enumerate(torus_lines))
        )
    with open(os.path.join(cell, "node-preload.txt"), "w") as f:
        f.write(
            "".join("pid=%d %s\n" % (100 + i, ln) for i, ln in enumerate(preload_lines))
        )


class EnvCheckTest(Base):
    def setUp(self):
        super().setUp()
        self.drv = self.generate(self.write_spec(minimal_spec(self.tmp)))
        self.cell = os.path.join(self.tmp, "cell")

    def chk(self, arm):
        return self.lib(self.drv, "envchk %s %s" % (shlex.quote(self.cell), arm))

    def test_malloc_conf_missing_from_preload_fails(self):
        """M1's MALLOC_CONF is in node-env-all.txt (TORUS_* only in real cells) but not in node-preload.txt: FAIL"""
        node_files(self.cell, ["TORUS_X_KNOB=7 "] * 3, ["", "", ""])
        with open(os.path.join(self.cell, "node-env-all.txt"), "a") as f:
            f.write(
                ""
            )  # node-env-all.txt never carries MALLOC_CONF; the check must not look there
        r = self.chk("M1")
        self.assertNotEqual(r.returncode, 0, r.stdout)
        self.assertIn("CHECK FAIL", r.stdout)
        self.assertIn("MALLOC_CONF", r.stdout)
        self.assertIn("node-preload.txt", r.stdout)

    def test_malloc_conf_present_in_preload_passes(self):
        node_files(
            self.cell, ["TORUS_X_KNOB=7 "] * 3, ["MALLOC_CONF=dirty_decay_ms:1000 "] * 3
        )
        r = self.chk("M1")
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertIn("CHECK OK", r.stdout)

    def test_malloc_conf_on_two_of_three_nodes_fails(self):
        node_files(
            self.cell,
            ["TORUS_X_KNOB=7 "] * 3,
            ["MALLOC_CONF=dirty_decay_ms:1000 "] * 2 + [""],
        )
        self.assertNotEqual(self.chk("M1").returncode, 0)

    def test_baseline_with_malloc_conf_fails(self):
        """an arm that does not set MALLOC_CONF must run without it (jemalloc defaults)"""
        node_files(self.cell, [""] * 3, ["MALLOC_CONF=dirty_decay_ms:1000 "] * 3)
        r = self.chk("A")
        self.assertNotEqual(r.returncode, 0, r.stdout)
        node_files(self.cell, [""] * 3, [""] * 3)
        r = self.chk("A")
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)

    def test_torus_knob_checked_in_node_env_all(self):
        node_files(
            self.cell,
            ["TORUS_X_KNOB=7 "] * 2 + [""],
            ["MALLOC_CONF=dirty_decay_ms:1000 "] * 3,
        )
        r = self.chk("M1")
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("TORUS_X_KNOB=7", r.stdout)
        self.assertIn("node-env-all.txt", r.stdout)
        # another arm's knob on the baseline: FAIL
        node_files(self.cell, ["TORUS_X_KNOB=7 "] * 3, [""] * 3)
        self.assertNotEqual(self.chk("A").returncode, 0)

    def test_wrong_value_fails(self):
        node_files(
            self.cell, ["TORUS_X_KNOB=8 "] * 3, ["MALLOC_CONF=dirty_decay_ms:1000 "] * 3
        )
        self.assertNotEqual(self.chk("M1").returncode, 0)

    def test_record_writes_preload_from_process_env(self):
        """envrec_now: MALLOC_CONF from each node's environ goes to node-preload.txt, TORUS_* to node-env-all.txt"""
        proc, dev = fake_proc(
            self.tmp,
            os.path.join(self.tmp, "stage", "b-run", "release", "torus-node"),
            ["TORUS_X_KNOB=7", "MALLOC_CONF=dirty_decay_ms:1000", "HOME=/x"],
        )
        os.makedirs(self.cell, exist_ok=True)
        r = self.lib(
            self.drv,
            "DEVNET=%s; envrec_now %s && envchk %s M1"
            % (shlex.quote(dev), shlex.quote(self.cell), shlex.quote(self.cell)),
            env={"DRIVER_PROC": proc},
        )
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        pre = open(os.path.join(self.cell, "node-preload.txt")).read()
        allv = open(os.path.join(self.cell, "node-env-all.txt")).read()
        self.assertEqual(pre.count("MALLOC_CONF=dirty_decay_ms:1000"), 3, pre)
        self.assertNotIn("MALLOC_CONF", allv)
        self.assertEqual(allv.count("TORUS_X_KNOB=7"), 3)


# ---------------------------------------------------------------- md5 check


def fake_proc(tmp, exe, environ):
    """a fake /proc with 3 node pids whose exe links to `exe`, and a devnet pids file"""
    proc = os.path.join(tmp, "proc")
    dev = os.path.join(tmp, "devnet")
    os.makedirs(os.path.join(dev, "run"), exist_ok=True)
    pids = [4101, 4102, 4103]
    for p in pids:
        d = os.path.join(proc, str(p))
        os.makedirs(d, exist_ok=True)
        if os.path.lexists(os.path.join(d, "exe")):
            os.remove(os.path.join(d, "exe"))
        os.symlink(exe, os.path.join(d, "exe"))
        with open(os.path.join(d, "environ"), "wb") as f:
            f.write(b"\0".join(x.encode() for x in environ) + b"\0")
    with open(os.path.join(dev, "run", "pids"), "w") as f:
        f.write("\n".join(map(str, pids)) + "\n")
    return proc, dev


class Md5Test(Base):
    def setUp(self):
        super().setUp()
        rel = os.path.join(self.tmp, "real", "release")
        os.makedirs(rel)
        self.real = os.path.join(rel, "torus-node")
        with open(self.real, "wb") as f:
            f.write(b"\x7fELF the real node\x00")
        os.chmod(self.real, 0o755)
        wst = os.path.join(self.tmp, "stage", "w-run", "release")
        os.makedirs(wst)
        self.wrapper = os.path.join(wst, "torus-node")
        with open(self.wrapper, "w") as f:
            f.write(
                '#!/bin/sh\nexport LD_PRELOAD=/usr/lib/libjemalloc.so.2\nexec %s "$@"\n'
                % self.real
            )
        os.chmod(self.wrapper, 0o755)
        self.assertNotEqual(md5_of(self.real), md5_of(self.wrapper))
        spec = minimal_spec(self.tmp)
        spec["binaries"]["w"] = {
            "stage_dir": os.path.dirname(wst),
            "node_md5": md5_of(self.real),
            "wrapper": True,
        }
        for a in spec["arms"]:
            a["binary"] = "b"
        spec["arms"].append({"label": "GJ", "binary": "w"})
        self.spec = spec
        self.drv = self.generate(self.write_spec(spec))
        self.cell = os.path.join(self.tmp, "cell")
        os.makedirs(self.cell)

    def test_md5_hashes_running_exe_not_wrapper(self):
        """the wrapper arm's node runs the real exe (/proc/PID/exe): md5 = real node md5 -> OK"""
        proc, dev = fake_proc(
            self.tmp, self.real, ["LD_PRELOAD=/usr/lib/libjemalloc.so.2"]
        )
        r = self.lib(
            self.drv,
            "DEVNET=%s; envrec_now %s && md5chk %s GJ"
            % (shlex.quote(dev), shlex.quote(self.cell), shlex.quote(self.cell)),
            env={"DRIVER_PROC": proc},
        )
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        rec = open(os.path.join(self.cell, "node-environ-trie.txt")).read()
        self.assertEqual(rec.count("exe_md5=" + md5_of(self.real)), 3, rec)
        self.assertNotIn(md5_of(self.wrapper), rec)

    def test_md5_mismatch_fails(self):
        """a node that runs the wrapper file itself (not the real exe) does not match the arm md5"""
        proc, dev = fake_proc(self.tmp, self.wrapper, [])
        r = self.lib(
            self.drv,
            "DEVNET=%s; envrec_now %s && md5chk %s GJ"
            % (shlex.quote(dev), shlex.quote(self.cell), shlex.quote(self.cell)),
            env={"DRIVER_PROC": proc},
        )
        self.assertNotEqual(r.returncode, 0, r.stdout)
        self.assertIn("CHECK FAIL", r.stdout)

    def test_preflight_resolves_wrapper_target(self):
        """dry preflight: a wrapper binary's staged md5 is the md5 of the node it execs"""
        r = subprocess.run(
            ["bash", self.drv, "dry"], capture_output=True, text=True, timeout=60
        )
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertIn("PREFLIGHT OK", r.stdout)
        # the same wrapper declared as a plain binary: its own md5 is not the node md5 -> PREFLIGHT FAIL
        self.spec["binaries"]["w"]["wrapper"] = False
        drv2 = self.generate(self.write_spec(self.spec, "spec2.json"), "driver2.sh")
        r = subprocess.run(
            ["bash", drv2, "dry"], capture_output=True, text=True, timeout=60
        )
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("PREFLIGHT FAIL", r.stdout)

    def test_wrapper_arm_skips_summary_md5_in_step(self):
        """the step's summary.json md5 is run-cell.sh's hash of the staged file (the wrapper): off for wrapper arms"""
        r = self.lib(self.drv, "step_summary_md5 GJ; step_summary_md5 A")
        self.assertEqual(r.stdout.split(), ["0", "1"], r.stdout + r.stderr)


class CellStepSummaryMd5Test(unittest.TestCase):
    """cell_step.py --no-summary-md5: only the running-process records count"""

    def test_flag(self):
        tmp = tempfile.mkdtemp(prefix="make-driver-step-")
        try:
            lb = "ozarchy-p3gate-300m-r1-m"
            cell = os.path.join(tmp, lb)
            shutil.copytree(os.path.join(CELLFIX, lb), cell)
            shutil.copy(os.path.join(CELLFIX, lb + ".cell.rc"), cell + ".cell.rc")
            sp = os.path.join(cell, "summary.json")
            s = json.load(open(sp))
            s["binaries"]["torus_node_md5"] = (
                "0123456789abcdef0123456789abcdef"  # a wrapper's md5
            )
            json.dump(s, open(sp, "w"))
            base = [
                sys.executable,
                "-I",
                os.path.join(HERE, "cell_step.py"),
                "record",
                "--cell-dir",
                cell,
                "--label",
                lb,
                "--tag",
                "r1-m",
                "--arm",
                "m",
                "--rep",
                "r1",
                "--staged-md5",
                "f10815747be79f22f502b51a8cd466eb",
            ]
            run1 = os.path.join(tmp, "run1")
            run2 = os.path.join(tmp, "run2")
            os.makedirs(run1)
            os.makedirs(run2)
            r = subprocess.run(
                base + ["--run-dir", run1], capture_output=True, text=True
            )
            self.assertIn(
                "bad:md5",
                open(os.path.join(run1, "cells.tsv")).read(),
                r.stdout + r.stderr,
            )
            r = subprocess.run(
                base + ["--run-dir", run2, "--no-summary-md5"],
                capture_output=True,
                text=True,
            )
            row = (
                open(os.path.join(run2, "cells.tsv")).read().splitlines()[1].split("\t")
            )
            self.assertEqual(row[5], "yes", r.stdout + r.stderr)
            self.assertEqual(row[7], "ok", row)
        finally:
            shutil.rmtree(tmp, ignore_errors=True)


# ---------------------------------------------------------------- cells.tsv, CELL-FLAG, smoke, done marker


class DriverFlowTest(Base):
    def setUp(self):
        super().setUp()
        self.spec = minimal_spec(self.tmp)
        self.drv = self.generate(self.write_spec(self.spec))
        self.R = self.spec["results_root"]
        self.RUN = self.spec["run_dir"]
        os.makedirs(self.RUN, exist_ok=True)

    def test_run_spec_rows_and_cell_flag(self):
        """run_spec (generated body) with a stub cell(): clean cell -> ok row; failed check -> bad:check row,
        CELL-FLAG line in campaign.log, queued rerun"""
        lb = "ozarchy-p3gate-300m-r1-m"
        stub = r"""
source "$TOOLS/cell-step.sh"
export CELL_STEP_LOG=$RUN/campaign.log
BMD5[b]=f10815747be79f22f502b51a8cd466eb
cell() { local label=$P-$1
    cp -r %(fix)s "$R/$label"; echo rc=0 > "$R/$label.cell.rc"; STARTED_LABEL=$label
    [ "$1" = r1-M1 ] && { echo "CHECK FAIL $label: x"; CELL_FAIL=check; return 1; }
    return 0; }
run_spec "r1-A:A:120:none:"; echo "a=$?"
run_spec "r1-M1:M1:120:none:"; echo "m=$?"
""" % {"fix": shlex.quote(os.path.join(CELLFIX, lb))}
        r = self.lib(self.drv, stub)
        self.assertIn("a=0", r.stdout, r.stdout + r.stderr)
        self.assertIn("m=1", r.stdout, r.stdout + r.stderr)
        rows = [
            ln.split("\t")
            for ln in open(os.path.join(self.RUN, "cells.tsv")).read().splitlines()
        ]
        self.assertEqual(rows[0], "label arm rep rc agree md5_ok metric flag".split())
        self.assertEqual(
            [(x[0], x[1], x[2], x[7]) for x in rows[1:]],
            [
                ("test-camp-10m-r1-A", "A", "r1", "ok"),
                ("test-camp-10m-r1-M1", "M1", "r1", "bad:check"),
            ],
        )
        log = open(os.path.join(self.RUN, "campaign.log")).read()
        self.assertIn("CELL-FLAG test-camp-10m-r1-M1 check", log)
        self.assertIn(
            "test-camp-10m-r1-M1-rerun",
            open(os.path.join(self.RUN, "rerun-queue.tsv")).read(),
        )

    def test_dry_lists_warmup_first_and_smoke_alone(self):
        r = subprocess.run(
            ["bash", self.drv, "dry"], capture_output=True, text=True, timeout=60
        )
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        cells = self.plan(self.drv)
        tags = [ln.split()[1] for ln in cells if ln.startswith("CELL ")]
        self.assertEqual(tags, ["w1", "r1-A", "r1-M1", "r2-M1", "r2-A"])
        sm = [ln for ln in self.plan(self.drv, smoke=True) if ln.startswith("CELL ")]
        self.assertEqual(len(sm), 1)
        self.assertEqual(sm[0].split()[1:4], ["s0", "A", "60"])
        paths = gen_tables(self.plan(self.drv, smoke=True))[5]
        self.assertEqual(paths["RUN"], self.RUN + "/smoke")
        self.assertEqual(paths["P"], "test-camp-10m-smoke")

    def test_run_outside_unit_refuses_and_writes_done(self):
        """`run` outside a bench-*.service unit: PREFLIGHT FAIL (launch goes through detach.sh, which holds the
        build lock), and campaign.done carries the exit code so the waiting session never hangs"""
        r = subprocess.run(
            ["bash", self.drv, "run"], capture_output=True, text=True, timeout=60
        )
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("PREFLIGHT FAIL", r.stdout)
        done = open(os.path.join(self.RUN, "campaign.done")).read()
        self.assertTrue(done.startswith("exit=1"), done)

    def test_launch_goes_through_detach(self):
        """`launch` execs DETACH NAME RUN/campaign.log DRIVER run (detach.sh takes the build lock)"""
        fake = os.path.join(self.tmp, "fake-detach.sh")
        with open(fake, "w") as f:
            f.write('#!/bin/bash\necho "DETACH $*"\n')
        os.chmod(fake, 0o755)
        r = subprocess.run(
            ["bash", self.drv, "launch"],
            capture_output=True,
            text=True,
            timeout=60,
            env=dict(os.environ, DRIVER_DETACH=fake),
        )
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertIn(
            "DETACH test-camp %s/campaign.log %s run"
            % (self.RUN, os.path.realpath(self.drv)),
            r.stdout,
        )
        r = subprocess.run(
            ["bash", self.drv, "launch", "smoke"],
            capture_output=True,
            text=True,
            timeout=60,
            env=dict(os.environ, DRIVER_DETACH=fake),
        )
        self.assertIn(
            "DETACH test-camp-smoke %s/smoke/campaign.log %s run smoke"
            % (self.RUN, os.path.realpath(self.drv)),
            r.stdout,
        )

    def test_body_wiring(self):
        t = open(self.drv).read()
        self.assertIn("cell_step_rerun_pass", t)
        self.assertIn("\nrun_campaign || exit 1\nexit 0\n", t)
        rc = func_text(t, "run_campaign")
        self.assertLess(rc.index("cell_step_rerun_pass"), rc.index("end_pass"))
        self.assertLess(rc.index("end_pass"), rc.index('log "CAMPAIGN END"'))
        self.assertNotRegex(t, r"cell_step [^\n]*&\s*$")
        self.assertIn("campaign.done", t)
        self.assertIn('"$DETACH" "$label"', t)  # every cell in its own bench unit

    def test_expect_and_market_checks(self):
        spec = minimal_spec(
            self.tmp,
            expect=[{"file": "run.log", "regex": "oracle feed pid [0-9]+;", "min": 1}],
        )
        spec["arms"][1]["expect"] = [
            {"file": "run.log", "regex": "admission horizon", "min": 0, "max": 0}
        ]
        drv = self.generate(self.write_spec(spec, "s3.json"), "d3.sh")
        cell = os.path.join(self.tmp, "c3")
        os.makedirs(cell)
        with open(os.path.join(cell, "run.log"), "w") as f:
            f.write("[x] oracle feed pid 123; ok\n")
        with open(os.path.join(cell, "summary.json"), "w") as f:
            json.dump({"cell": {"markets": 10}}, f)
        q = shlex.quote(cell)
        self.assertEqual(
            self.lib(drv, "expectchk %s M1 && marketchk %s" % (q, q)).returncode, 0
        )
        with open(os.path.join(cell, "run.log"), "a") as f:
            f.write("admission horizon 5000\n")
        self.assertNotEqual(self.lib(drv, "expectchk %s M1" % q).returncode, 0)
        self.assertEqual(self.lib(drv, "expectchk %s A" % q).returncode, 0)
        with open(os.path.join(cell, "summary.json"), "w") as f:
            json.dump({"cell": {"markets": 300}}, f)
        self.assertNotEqual(self.lib(drv, "marketchk %s" % q).returncode, 0)


# ---------------------------------------------------------------- bad specs


class SpecValidationTest(Base):
    def bad(self, spec, needle):
        with self.assertRaises(self.gen.SpecError) as cm:
            self.gen.validate(spec)
        self.assertIn(needle, str(cm.exception))

    def test_minimal_ok(self):
        self.gen.validate(minimal_spec(self.tmp))

    def test_unknown_top_key(self):
        self.bad(minimal_spec(self.tmp, cell_secs=60), "cell_secs")

    def test_unknown_arm_key(self):
        s = minimal_spec(self.tmp)
        s["arms"][0]["knob"] = "x"
        self.bad(s, "knob")

    def test_unknown_binary_key(self):
        s = minimal_spec(self.tmp)
        s["binaries"]["b"]["md5"] = "x"
        self.bad(s, "md5")

    def test_missing_baseline_arm(self):
        self.bad(minimal_spec(self.tmp, baseline="Z"), "baseline")

    def test_duplicate_arm(self):
        s = minimal_spec(self.tmp)
        s["arms"].append({"label": "A"})
        self.bad(s, "duplicate")

    def test_missing_required(self):
        s = minimal_spec(self.tmp)
        del s["replicates"]
        self.bad(s, "replicates")

    def test_unknown_binary_ref(self):
        s = minimal_spec(self.tmp)
        s["arms"][1]["binary"] = "nope"
        self.bad(s, "nope")

    def test_env_value_with_space(self):
        s = minimal_spec(self.tmp)
        s["arms"][1]["env"]["MALLOC_CONF"] = "a b"
        self.bad(s, "MALLOC_CONF")

    def test_reserved_harness_env(self):
        self.bad(minimal_spec(self.tmp, harness_env={"TARGET_DIR": "/x"}), "TARGET_DIR")
        self.bad(
            minimal_spec(self.tmp, harness_env={"CRASH_KILL_AT_S": "40"}),
            "CRASH_KILL_AT_S",
        )

    def test_bad_md5(self):
        s = minimal_spec(self.tmp)
        s["binaries"]["b"]["node_md5"] = "xyz"
        self.bad(s, "node_md5")

    def test_explicit_order_unknown_arm(self):
        s = minimal_spec(self.tmp, order=["r1:A", "r1:Q"])
        del s["replicates"]
        self.bad(s, "Q")
        s["order"] = ["r1:A", "r1:M1"]
        self.gen.validate(s)
        self.bad(minimal_spec(self.tmp, order=["r1:A", "r1:M1"]), "replicates")

    def test_generator_cli_rejects_bad_spec(self):
        p = self.write_spec(minimal_spec(self.tmp, baseline="Z"))
        r = subprocess.run(
            [sys.executable, "-I", GEN, p, "-o", os.path.join(self.tmp, "x.sh")],
            capture_output=True,
            text=True,
        )
        self.assertEqual(r.returncode, 2, r.stdout + r.stderr)
        self.assertIn("SPEC ERROR", r.stderr)
        self.assertFalse(os.path.exists(os.path.join(self.tmp, "x.sh")))


class Md5SourceTest(Base):
    def test_md5_file_rows(self):
        """node_row / bench_row read md5s.txt (last line whose first two columns match), as the Job 4 driver's awk"""
        mf = os.path.join(self.tmp, "md5s.txt")
        with open(mf, "w") as f:
            f.write(
                "# comment\nN bdd59888 %s\nbench-F ff822b95 %s\nN bdd59888 %s\n"
                % ("1" * 32, "2" * 32, "3" * 32)
            )
        s = minimal_spec(self.tmp)
        st = s["binaries"]["b"]["stage_dir"]
        s["binaries"]["b"] = {
            "stage_dir": st,
            "md5_file": mf,
            "node_row": "N bdd59888",
            "bench_row": "bench-F ff822b95",
        }
        p = self.gen.validate(s)
        self.assertEqual(p["binaries"]["b"]["node_md5"], "3" * 32)
        self.assertEqual(p["binaries"]["b"]["bench_md5"], "2" * 32)
        s["binaries"]["b"]["node_row"] = "N 00000000"
        with self.assertRaises(self.gen.SpecError):
            self.gen.validate(s)
        s["binaries"]["b"] = {
            "stage_dir": st,
            "node_md5": "3" * 32,
            "node_row": "N bdd59888",
            "md5_file": mf,
        }
        with self.assertRaises(self.gen.SpecError):
            self.gen.validate(s)

    def test_md5chk_accepts_older_8_char_records(self):
        """records of older drivers (exe_md5=<first 8>) are matched as a prefix; another md5 still fails"""
        spec = minimal_spec(self.tmp)
        drv = self.generate(self.write_spec(spec))
        want = spec["binaries"]["b"]["node_md5"]
        cell = os.path.join(self.tmp, "c8")
        os.makedirs(cell)
        with open(os.path.join(cell, "node-environ-trie.txt"), "w") as f:
            f.write(
                "".join(
                    "pid=%d exe_md5=%s TRIE_VAR_MISSING\n" % (i, want[:8])
                    for i in range(3)
                )
            )
        self.assertEqual(self.lib(drv, "md5chk %s A" % shlex.quote(cell)).returncode, 0)
        with open(os.path.join(cell, "node-environ-trie.txt"), "w") as f:
            f.write("".join("pid=%d exe_md5=%s\n" % (i, "0" * 8) for i in range(3)))
        self.assertNotEqual(
            self.lib(drv, "md5chk %s A" % shlex.quote(cell)).returncode, 0
        )


class ExpectMissingFileTest(Base):
    def test_missing_file_fails_even_for_zero_match(self):
        """an expectation on a file the cell lacks is a CHECK FAIL, also for min 0 / max 0 (absence) expectations"""
        spec = minimal_spec(
            self.tmp,
            expect=[{"file": "missing.log", "regex": "panic", "min": 0, "max": 0}],
        )
        drv = self.generate(self.write_spec(spec, "sx.json"), "dx.sh")
        cell = os.path.join(self.tmp, "cx")
        os.makedirs(cell)
        r = self.lib(drv, "expectchk %s A" % shlex.quote(cell))
        self.assertNotEqual(r.returncode, 0, r.stdout)
        self.assertIn("CHECK FAIL", r.stdout)
        self.assertIn("missing.log", r.stdout)
        open(os.path.join(cell, "missing.log"), "w").write("all fine\n")
        self.assertEqual(
            self.lib(drv, "expectchk %s A" % shlex.quote(cell)).returncode, 0
        )

    def test_grep_error_fails(self):
        """a regex grep -E rejects (Python accepted it) is a CHECK FAIL, not 0 matches"""
        spec = minimal_spec(
            self.tmp,
            expect=[{"file": "run.log", "regex": "[[:foo:]]", "min": 0, "max": 0}],
        )
        drv = self.generate(self.write_spec(spec, "sg.json"), "dg.sh")
        cell = os.path.join(self.tmp, "cg")
        os.makedirs(cell)
        open(os.path.join(cell, "run.log"), "w").write("x\n")
        r = self.lib(drv, "expectchk %s A" % shlex.quote(cell))
        self.assertNotEqual(r.returncode, 0, r.stdout)


# ---------------------------------------------------------------- frozen tools: genesis inputs (Codex P2)


class FrozenToolsTest(Base):
    """run-cell.sh resolves its genesis inputs from its OWN location (MAINREPO = dir/../..), not from the
    harness worktree it is handed: a frozen tools copy must carry them. Job 4 (ozarchy-p4j4-tools) froze
    devnet/wsl, devnet/genesis.json, testnet/lib/cargo-bin.sh, testnet/gen-weighted-genesis.sh and
    testnet/genesis-weighted-full.json next to tools/ and its driver diffed devnet/wsl against the harness."""

    def dry(self, spec, name):
        drv = self.generate(self.write_spec(spec, name + ".json"), name + ".sh")
        return subprocess.run(
            ["bash", drv, "dry"], capture_output=True, text=True, timeout=60
        )

    def test_run_cell_reads_genesis_from_its_own_root(self):
        """the reviewer's case: a standalone frozen copy resolves MAINREPO to the frozen root, which has no
        devnet/wsl/gen-3val-genesis.sh (the cell's genesis step: exit 127)"""
        wt, tools = frozen_fixture(self.tmp, complete=False)
        r = subprocess.run(
            ["bash", os.path.join(tools, "run-cell.sh"), wt, "x"],
            capture_output=True,
            text=True,
            timeout=60,
            env=dict(os.environ, RUN_CELL_PRINT_PATHS="1", TOOLS_DIR=tools),
        )
        paths = dict(ln.split("=", 1) for ln in r.stdout.splitlines() if "=" in ln)
        self.assertEqual(
            paths["MAINREPO"], os.path.join(self.tmp, "ft"), r.stdout + r.stderr
        )
        self.assertFalse(
            os.path.exists(
                os.path.join(paths["MAINREPO"], "devnet/wsl/gen-3val-genesis.sh")
            )
        )

    def test_standalone_copy_without_genesis_inputs_fails_preflight(self):
        bare = os.path.join(self.tmp, "bare")
        wt, tools = frozen_fixture(bare, complete=False)
        r = self.dry(
            minimal_spec(self.tmp, tools_dir=tools, harness_worktree=wt), "bare"
        )
        self.assertNotEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertIn("PREFLIGHT FAIL", r.stdout)
        self.assertIn(
            os.path.join(bare, "ft", "devnet/wsl/gen-3val-genesis.sh"), r.stdout
        )
        self.assertNotIn("PREFLIGHT OK", r.stdout)

    def test_each_missing_input_fails_preflight(self):
        spec = minimal_spec(self.tmp)
        ft = os.path.join(self.tmp, "ft")
        required = sorted(
            set(GENESIS_FILES) - {"devnet/wsl/launch-3val.sh"}
        )  # launch-3val.sh: the diff
        for i, rel in enumerate(required + ["testnet/genesis-weighted-full.json"]):
            f = os.path.join(ft, rel)
            keep = f + ".away"
            os.rename(f, keep)
            try:
                r = self.dry(spec, "miss%d" % i)
                self.assertNotEqual(r.returncode, 0, rel + ": " + r.stdout)
                self.assertIn("PREFLIGHT FAIL", r.stdout, rel)
                self.assertIn(rel, r.stdout)
            finally:
                os.rename(keep, f)

    def test_empty_weighted_full_fails_preflight(self):
        spec = minimal_spec(self.tmp)
        open(
            os.path.join(self.tmp, "ft", "testnet/genesis-weighted-full.json"), "w"
        ).close()
        r = self.dry(spec, "empty")
        self.assertNotEqual(r.returncode, 0, r.stdout)
        self.assertIn("genesis-weighted-full.json", r.stdout)

    def test_genesis_script_not_executable_fails_preflight(self):
        spec = minimal_spec(self.tmp)
        os.chmod(os.path.join(self.tmp, "ft", "devnet/wsl/gen-3val-genesis.sh"), 0o644)
        os.chmod(os.path.join(self.tmp, "wt", "devnet/wsl/gen-3val-genesis.sh"), 0o644)
        r = self.dry(spec, "noexec")
        self.assertNotEqual(r.returncode, 0, r.stdout)
        self.assertIn("gen-3val-genesis.sh", r.stdout)

    def test_frozen_inputs_differ_from_harness_fail_preflight(self):
        spec = minimal_spec(self.tmp)
        for rel in (
            "devnet/wsl/gen-3val-genesis.sh",
            "devnet/genesis.json",
            "testnet/lib/cargo-bin.sh",
        ):
            f = os.path.join(self.tmp, "ft", rel)
            orig = open(f).read()
            with open(f, "a") as fh:
                fh.write("# drift\n")
            try:
                r = self.dry(spec, "drift")
                self.assertNotEqual(r.returncode, 0, rel + ": " + r.stdout)
                self.assertIn("differ", r.stdout, rel)
                self.assertIn(rel.split("/")[-1], r.stdout, rel)
            finally:
                with open(f, "w") as fh:
                    fh.write(orig)
        # a file only the harness has (a newer devnet/wsl script) is a difference too
        write_tree(
            os.path.join(self.tmp, "wt"), {"devnet/wsl/stop-3val.sh": "#!/bin/sh\n"}
        )
        r = self.dry(spec, "extra")
        self.assertNotEqual(r.returncode, 0, r.stdout)
        self.assertIn("stop-3val.sh", r.stdout)

    def test_genesis_output_ignored_in_comparison(self):
        """devnet/wsl/genesis-3val.json is each cell's OUTPUT (written into the harness worktree): no difference"""
        spec = minimal_spec(self.tmp)
        write_tree(
            os.path.join(self.tmp, "wt"), {"devnet/wsl/genesis-3val.json": "{}\n"}
        )
        r = self.dry(spec, "out")
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)

    def test_complete_frozen_copy_passes_and_names_the_root(self):
        r = self.dry(minimal_spec(self.tmp), "ok")
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertIn("PREFLIGHT OK", r.stdout)
        self.assertIn("genesis_root=%s" % os.path.join(self.tmp, "ft"), r.stdout)

    def test_tools_inside_the_harness_worktree(self):
        """tools_dir = <harness>/tools/matched-bench: the root IS the harness worktree; only the untracked
        weighted-full genesis has to be there"""
        wt, _ = frozen_fixture(self.tmp)
        tools = os.path.join(wt, "tools", "matched-bench")
        for rel in TOOL_FILES:
            os.makedirs(os.path.dirname(os.path.join(tools, rel)), exist_ok=True)
            shutil.copy2(os.path.join(HERE, rel), os.path.join(tools, rel))
        spec = minimal_spec(self.tmp, tools_dir=tools)
        r = self.dry(spec, "inwt")
        self.assertNotEqual(r.returncode, 0, r.stdout)
        self.assertIn("genesis-weighted-full.json", r.stdout)
        write_tree(wt, {"testnet/genesis-weighted-full.json": "{}\n"})
        r = self.dry(spec, "inwt2")
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)


# ---------------------------------------------------------------- env scrub vs caller exports (Codex P2)


class ScrubEnvTest(Base):
    BODY = os.path.join(HERE, "campaign", "driver-body.sh")

    def test_reviewer_case_run_survives_scrub(self):
        """RUN=/tmp/caller bash -c 'CAMPAIGN=c TOOLS=.; DRIVER_LIB_ONLY=1 source driver-body.sh; scrub_env;
        echo "$RUN"' printed 'RUN: unbound variable'"""
        r = subprocess.run(
            [
                "bash",
                "-c",
                'CAMPAIGN=c TOOLS=.; DRIVER_LIB_ONLY=1 source "$1"; scrub_env; echo "RUN=$RUN";'
                ' env | grep -c "^RUN=" || true',
                "x",
                self.BODY,
            ],
            capture_output=True,
            text=True,
            timeout=60,
            env=dict(os.environ, RUN="/tmp/caller"),
        )
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertEqual(
            r.stdout.split(), ["RUN=/tmp/caller", "0"], r.stdout + r.stderr
        )

    def caller_env(self):
        return {
            "RUN": "/tmp/caller-run",
            "P": "caller-p",
            "TOOLS": "/tmp/caller-tools",
            "MARKETS": "999",
            "ARMS": "caller-arm",
            "CELLS": "caller-cell",
            "BDIR": "caller-bdir",
            "SMOKE": "1",
            "HARNESS": "/tmp/caller-harness",
            "TORUS_FOO": "1",
            "MALLOC_CONF": "x",
        }

    def test_caller_exported_config_names_keep_driver_values(self):
        spec = minimal_spec(self.tmp)
        drv = self.generate(self.write_spec(spec))
        r = self.lib(
            drv,
            'scrub_env; echo "RUN=$RUN"; echo "P=$P"; echo "TOOLS=$TOOLS"; echo "MARKETS=$MARKETS";'
            ' echo "ARMS=${ARMS[*]}"; echo "NCELLS=${#CELLS[@]}"; echo "BDIR=${BDIR[b]}"; echo "SMOKE=[$SMOKE]";'
            ' echo "HARNESS=$HARNESS";'
            ' echo "CHILD=$(env | grep -E "^(RUN|P|TOOLS|MARKETS|ARMS|CELLS|BDIR|SMOKE|HARNESS|TORUS_FOO|MALLOC_CONF)=" | tr "\\n" " ")"',
            env=self.caller_env(),
        )
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        got = dict(ln.split("=", 1) for ln in r.stdout.splitlines() if "=" in ln)
        self.assertEqual(got["RUN"], spec["run_dir"])
        self.assertEqual(got["P"], spec["label_prefix"])
        self.assertEqual(got["TOOLS"], spec["tools_dir"])
        self.assertEqual(got["MARKETS"], "10")
        self.assertEqual(got["ARMS"], "A M1")
        self.assertEqual(got["NCELLS"], "5")
        self.assertEqual(got["BDIR"], spec["binaries"]["b"]["stage_dir"])
        self.assertEqual(got["SMOKE"], "[]")
        self.assertEqual(got["HARNESS"], os.path.join(spec["tools_dir"], "run-cell.sh"))
        self.assertEqual(got["CHILD"].strip(), "")

    def test_caller_smoke_export_does_not_make_a_smoke_run(self):
        """SMOKE=1 from the caller must not skip the rerun pass of a full run (the driver owns SMOKE)"""
        drv = self.generate(self.write_spec(minimal_spec(self.tmp)))
        r = self.lib(
            drv,
            'echo "[${SMOKE}]"; apply_smoke; scrub_env; echo "[$SMOKE] $RUN"',
            env={"SMOKE": "1"},
        )
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        lines = r.stdout.splitlines()
        self.assertEqual(lines[0], "[]")
        self.assertTrue(
            lines[1].startswith("[1] ") and lines[1].endswith("/smoke"), lines
        )

    def test_launch_with_caller_exports(self):
        """`launch` with caller-exported config names: detach gets the driver's values, the unit env none of them"""
        spec = minimal_spec(self.tmp)
        drv = self.generate(self.write_spec(spec))
        fake = os.path.join(self.tmp, "fake-detach.sh")
        with open(fake, "w") as f:
            f.write(
                '#!/bin/bash\necho "DETACH $*"\n'
                'echo "CHILD=$(env | grep -E "^(RUN|P|TOOLS|ARMS|TORUS_FOO|MALLOC_CONF)=" | tr "\\n" " ")"\n'
            )
        os.chmod(fake, 0o755)
        r = subprocess.run(
            ["bash", drv, "launch"],
            capture_output=True,
            text=True,
            timeout=60,
            env=dict(os.environ, DRIVER_DETACH=fake, **self.caller_env()),
        )
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertIn(
            "DETACH test-camp %s/campaign.log %s run"
            % (spec["run_dir"], os.path.realpath(drv)),
            r.stdout,
        )
        self.assertIn("CHILD=\n", r.stdout.replace("CHILD= \n", "CHILD=\n"))
        self.assertFalse(os.path.exists("/tmp/caller-run/campaign.log"))

    def test_driver_vars_cover_every_config_and_body_global(self):
        """scrub_env keeps every driver name (DRIVER_VARS): each name the generated config or the body assigns
        at top level is listed"""
        drv = self.generate(self.write_spec(minimal_spec(self.tmp)))
        text = open(drv).read()
        names = set()
        for ln in text.splitlines():
            m = re.match(r"^declare -A (.*?)\s*(#|$)", ln)
            if m:
                names.update(m.group(1).split())
                continue
            for m in re.finditer(r"(?:^|;\s*)([A-Z][A-Z0-9_]*)(?:\[[^]]*\])?=", ln):
                names.add(m.group(1))
        r = self.lib(drv, 'echo "$DRIVER_VARS"')
        listed = set(r.stdout.split())
        self.assertTrue(listed, r.stdout + r.stderr)
        # names the driver only reads/exports for its children (not driver state)
        child_env = {
            "CELL_STEP_LOG",
            "CELL_STEP_TIMEOUT",
            "CELL_STEP_ANALYSIS_TIMEOUT",
            "CELL_STEP_ANALYSIS_CMD",
            "CELL_STEP_ANALYSIS_METRIC",
            "CELL_STEP_SUMMARY_MD5",
            "XDG_RUNTIME_DIR",
            "DRIVER_LIB_ONLY",
            "E",
            "IFS",
        }
        self.assertEqual(sorted(names - listed - child_env), [])


# ---------------------------------------------------------------- runtime watchers (s123: ported from Job 4)

LB = "test-camp-10m-r1-A"  # minimal_spec's label of cell r1-A

# fake `systemctl --user`: a unit is $FAKE_UNITS/<unit>.pid (its main pid, written by the fake detach.sh); active =
# that process exists and is no zombie; stop = SIGTERM to its process group (what systemd does to the unit's cgroup)
FAKE_SYSTEMCTL = r"""#!/bin/bash
[ "$1" = --user ] && shift
cmd=$1; shift
echo "$cmd $*" >> "$FAKE_UNITS/systemctl.log"
u=${!#}; u=${u%.service}; f=$FAKE_UNITS/$u.pid
alive() { local p; p=$(cat "$f" 2>/dev/null); [ -n "$p" ] && [ -r "/proc/$p/status" ] && ! grep -q '^State:[[:space:]]*Z' "/proc/$p/status"; }
case $cmd in
    is-active) alive ;;
    show) if alive; then cat "$f"; else echo 0; fi ;;
    stop) echo "STOPPED $u.service" >> "$FAKE_UNITS/stops.log"; p=$(cat "$f" 2>/dev/null); [ -z "$p" ] || kill -TERM -- "-$p" 2>/dev/null; exit 0 ;;
    *) exit 0 ;;
esac
"""
# fake detach.sh NAME LOG CMD...: CMD in its own session (the "unit"); its pid is the unit's main pid
FAKE_DETACH = r"""#!/bin/bash
name=$1 log=$2; shift 2
mkdir -p "$(dirname "$log")"
setsid "$@" >> "$log" 2>&1 < /dev/null &
echo "$!" > "$FAKE_UNITS/bench-$name.pid"
echo "fake detach: bench-$name.service pid $!"
"""
# fake pgrep: the test host's own torus-node / bench-throughput processes stay invisible
FAKE_PGREP = r"""#!/bin/bash
for a in "$@"; do case $a in torus-node|*bench-throughput*) exit 1 ;; esac; done
exec @PGREP@ "$@"
"""
# fake run-cell.sh WT LABEL MARKETS DUR RATE EXTRA_ENV: 3 stub "nodes" (sleep), the run.log lines the watchers read
# (node pids, bench, stopping pid / stopped.), the devnet pids file, node logs, RocksDB LOGs, a fake /proc entry per
# node (FAKE_PROC: exe = the staged node, environ = EXTRA_ENV), summary.json (a passing warm-up gate; FAKE_SUMMARY = a
# JSON object whose sections update it), exit FAKE_RC (0); FAKE_DIE=i: node i exits after 2 s.
# TERM: stops the stubs as run-cell.sh's finish_fail does, exit 130. @GUARD@ = run-cell.sh's own output-dir guard
# (run_cell_guard): the driver's records (sysctl.txt, the samplers) precede the harness in the cell dir (Codex P1)
FAKE_HARNESS = r"""#!/bin/bash
set -u
label=$2 markets=$3 xe=${6:-}
OUT=$RESULTS_ROOT/$label
@GUARD@mkdir -p "$OUT" "$DATA_ROOT/run"
: > "$OUT/run.log"
log() { printf '[%s] %s\n' "$(date +%H:%M:%S)" "$*" | tee -a "$OUT/run.log"; }
PIDS=() W=
for i in 0 1 2; do
    if [ "$i" = "${FAKE_DIE:-}" ]; then sleep 2 & else sleep 600 & fi
    PIDS+=("$!")
    mkdir -p "$DATA_ROOT/data/val$i"; echo "rocksdb LOG val$i" > "$DATA_ROOT/data/val$i/LOG"
    printf '\033[31mERROR\033[0m fake val%s last line\n' "$i" > "$DATA_ROOT/run/val$i.log"
done
printf '%s\n' "${PIDS[@]}" > "$DATA_ROOT/run/pids"
if [ -n "${FAKE_PROC:-}" ]; then
    for p in "${PIDS[@]}"; do
        mkdir -p "$FAKE_PROC/$p"
        ln -sfn "$TARGET_DIR/release/torus-node" "$FAKE_PROC/$p/exe"
        printf '%s\0' $xe > "$FAKE_PROC/$p/environ"
    done
fi
stopall() {
    local p
    for p in "${PIDS[@]}"; do
        if kill -0 "$p" 2>/dev/null; then echo "stopping pid $p" >> "$OUT/run.log"; kill "$p" 2>/dev/null; fi
    done
    [ -z "$W" ] || kill "$W" 2>/dev/null
    echo "stopped." >> "$OUT/run.log"
}
trap 'log "interrupted"; stopall; exit 130' INT TERM
log "node pids: ${PIDS[*]}"
log "bench: /fake/bench-throughput consensus --markets $markets"
sleep "${FAKE_RUN_S:-3}" & W=$!
wait "$W"; W=
log "bench exited rc=0 after ${FAKE_RUN_S:-3}s"
python3 -I -c 'import json, os, sys
s = {"cell": {"markets": int(sys.argv[2])}, "validity": {"verdict": "ACCEPT"}, "liveness": {"verdict": "PASS"},
     "agreement": {"panic_or_failstop_lines": 0}, "headline": {"agreement_verdict": "AGREE", "matched_s_avg": 1000.0}}
for k, v in json.loads(os.environ.get("FAKE_SUMMARY") or "{}").items():
    s.setdefault(k, {}).update(v)
json.dump(s, open(sys.argv[1], "w"))' "$OUT/summary.json" "$markets"
for i in 0 1 2; do echo "2026-10-15T00:00:00.000Z  INFO torus_node: fake val$i" | gzip > "$OUT/val$i.log.gz"; done
echo "harness env DEFER_REPORT_SCANS=${DEFER_REPORT_SCANS:-unset} GENESIS_CACHE_DIR=${GENESIS_CACHE_DIR-unset}" >> "$OUT/run.log"
[ "${DEFER_REPORT_SCANS:-0}" != 1 ] || echo '{"version": 1, "scans": "x", "bench_submitted": "5", "ingest_log_bytes": [9, 9, 9]}' > "$OUT/deferred-scans.json"
stopall
exit "${FAKE_RC:-0}"
"""
# fake sysctl -n NAME: the value in $FAKE_SYSCTL/NAME
FAKE_SYSCTL = r"""#!/bin/bash
[ "$1" = -n ] && shift
cat "$FAKE_SYSCTL/$1" 2>/dev/null || { echo "sysctl: cannot stat /proc/sys/${1//.//}" >&2; exit 255; }
"""


def run_cell_guard():
    """run-cell.sh's output-dir guard, verbatim: a non-empty $OUT without OVERWRITE=1 is FATAL (exit 1)"""
    t = open(os.path.join(HERE, "run-cell.sh")).read()
    m = re.search(
        r'^if \[ -d "\$OUT" \] && \[ -n "\$\(ls -A "\$OUT" 2>/dev/null\)" \].*?^fi\n',
        t,
        re.M | re.S,
    )
    assert m and "OVERWRITE" in m.group(0), "run-cell.sh's output-dir guard moved"
    return m.group(0)


SAMPLER_FILES = (
    "data-du-samples.txt",
    "ckpt-samples.txt",
    "mem-samples.txt",
    "node-nofile.txt",
    "fds.txt",
    "vmstat-1hz.txt",
    "metrics-1hz.txt.gz",
    "host-1hz.tsv",
    "rocksdb-LOG-val0.txt",
    "data-du.txt",
)


class FakeHostBase(Base):
    """a fake bin dir (put first on PATH), a fake units dir and a fake /proc (DRIVER_PROC: meminfo, node pids)"""

    def setUp(self):
        super().setUp()
        self.bin = os.path.join(self.tmp, "fakebin")
        self.units = os.path.join(self.tmp, "units")
        self.proc = os.path.join(self.tmp, "fakeproc")
        for d in (self.bin, self.units, self.proc):
            os.makedirs(d)
        fakes = {
            "systemctl": FAKE_SYSTEMCTL,
            "pgrep": FAKE_PGREP.replace("@PGREP@", shutil.which("pgrep")),
            "detach.sh": FAKE_DETACH,
            "run-cell.sh": FAKE_HARNESS.replace("@GUARD@", run_cell_guard()),
        }
        for name, body in fakes.items():
            self.write_fake(name, body)
        self.sysctl_dir = (
            None  # set_sysctl() installs the fake sysctl; until then the host's answers
        )
        self.meminfo(50000000)
        self.procs = []

    def write_fake(self, name, body):
        with open(os.path.join(self.bin, name), "w") as f:
            f.write(body)
        os.chmod(os.path.join(self.bin, name), 0o755)

    def tearDown(self):
        # every spawned process and every fake unit leads its own session: kill each whole group
        pgids = [p.pid for p in self.procs]
        for f in glob.glob(os.path.join(self.units, "*.pid")):
            try:
                pgids.append(int(open(f).read()))
            except ValueError:
                pass
        for g in pgids:
            try:
                os.killpg(g, signal.SIGKILL)
            except OSError:
                pass
        for p in self.procs:
            p.wait()
        super().tearDown()

    def meminfo(self, kb):
        with open(os.path.join(self.proc, "meminfo"), "w") as f:
            f.write(
                "MemTotal:       65000000 kB\nMemFree:         1000000 kB\nMemAvailable:   %d kB\n"
                % kb
            )

    def set_sysctl(self, **vals):
        """fake sysctl values (name with '__' for '.'): installs the fake sysctl on first use"""
        if self.sysctl_dir is None:
            self.sysctl_dir = os.path.join(self.tmp, "sysctl")
            os.makedirs(self.sysctl_dir)
            self.write_fake("sysctl", FAKE_SYSCTL)
        for k, v in vals.items():
            with open(os.path.join(self.sysctl_dir, k.replace("__", ".")), "w") as f:
                f.write("%s\n" % v)

    def env(self, **over):
        e = {
            "PATH": self.bin + os.pathsep + os.environ["PATH"],
            "FAKE_UNITS": self.units,
            "DRIVER_PROC": self.proc,
        }
        if self.sysctl_dir is not None:
            e["FAKE_SYSCTL"] = self.sysctl_dir
        e.update(over)
        return e

    def spawn(self, *args):
        p = subprocess.Popen(
            list(args),
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            start_new_session=True,
        )
        self.procs.append(p)
        return p


class WatchNodesTest(FakeHostBase):
    """watch_nodes (library mode) on stub processes: the node-death.txt the step reads, and the harness TERM"""

    def setUp(self):
        super().setUp()
        self.spec = minimal_spec(self.tmp)
        self.drv = self.generate(self.write_spec(self.spec))
        self.cell = os.path.join(self.spec["results_root"], LB)
        os.makedirs(self.cell)
        run = os.path.join(self.spec["devnet_dir"], "run")
        os.makedirs(run, exist_ok=True)
        for i in range(3):
            with open(os.path.join(run, "val%d.log" % i), "w") as f:
                f.write("first\n\x1b[31mERROR\x1b[0m val%d: thread panicked at x\n" % i)

    def harness(self):
        """a stub harness pid: TERM -> harness-term.txt, exit 130 (run-cell.sh's trap stops the nodes)"""
        f = os.path.join(self.tmp, "harness-term.txt")
        p = self.spawn(
            "bash",
            "-c",
            "sleep 60 & s=$!; trap 'echo TERM > %s; kill $s; exit 130' TERM; echo > %s.ready; wait $s"
            % (f, f),
        )
        self.wait_for(lambda: os.path.exists(f + ".ready"))  # its trap is set
        return p, f

    def wait_for(self, cond, what="condition"):
        for _ in range(200):
            if cond():
                return
            time.sleep(0.05)
        self.fail("timed out waiting for " + what)

    def nodes(self, die=None):
        """3 stub node pids in the cell's run.log 'node pids:' line; node `die` exits after 1 s. The stubs are
        children of a holder bash that reaps them (a zombie still answers kill -0, as a reaped pid does not)"""
        pf = os.path.join(self.tmp, "node-pids")
        starts = "; ".join(
            "sleep %d & echo $! >> %s" % (1 if i == die else 60, pf) for i in range(3)
        )
        self.spawn("bash", "-c", "%s; wait" % starts)
        self.wait_for(
            lambda: os.path.exists(pf) and len(open(pf).read().split()) == 3,
            "the 3 stub node pids",
        )
        pids = [int(x) for x in open(pf).read().split()]
        with open(os.path.join(self.cell, "run.log"), "w") as f:
            f.write("[12:00:00] node pids: %s\n" % " ".join(map(str, pids)))
        return pids

    def test_death_writes_node_death_and_terms_the_harness(self):
        pids = self.nodes(die=1)
        hp, term = self.harness()
        r = self.lib(
            self.drv,
            'watch_nodes %s %d; echo "WN=$?"' % (shlex.quote(self.cell), hp.pid),
            env=self.env(),
        )
        self.assertIn("WN=3", r.stdout, r.stdout + r.stderr)
        death = open(os.path.join(self.cell, "node-death.txt")).read()
        self.assertRegex(
            death,
            r"^DEATH node val1 comm=torus-node pid=%d detected at \d{4}-\d\d-\d\d \d\d:\d\d:\d\d\.\d{3} "
            r"\(0\.5 s poll; previous check ok <= ~0\.6 s earlier\); "
            r"last val1 log line: ERROR val1: thread panicked at x\n$" % pids[1],
        )
        life = open(os.path.join(self.cell, "node-life.txt")).read()
        self.assertIn("watching node pids: %s" % " ".join(map(str, pids)), life)
        self.assertIn(death, life)
        self.assertIn("DEATH node val1 comm=torus-node pid=%d at " % pids[1], r.stdout)
        self.assertIn("-> aborting cell (TERM harness %d)" % hp.pid, r.stdout)
        self.assertEqual(hp.wait(timeout=10), 130)
        self.assertEqual(open(term).read().strip(), "TERM")

    def test_harness_stop_is_no_death(self):
        """nodes stopped by the harness (stop-3val's 'stopping pid' lines first): a stop window, no death"""
        pids = self.nodes()
        hp, _ = self.harness()
        runlog = shlex.quote(os.path.join(self.cell, "run.log"))
        stop = (
            '(sleep 1; for p in %s; do echo "stopping pid $p"; done >> %s; echo stopped. >> %s) &'
            % (" ".join(map(str, pids)), runlog, runlog)
        )
        r = self.lib(
            self.drv,
            '%s\nwatch_nodes %s %d; echo "WN=$?"'
            % (stop, shlex.quote(self.cell), hp.pid),
            env=self.env(),
        )
        self.assertIn("WN=0", r.stdout, r.stdout + r.stderr)
        self.assertFalse(os.path.exists(os.path.join(self.cell, "node-death.txt")))
        life = open(os.path.join(self.cell, "node-life.txt")).read()
        self.assertIn("all 3 node pids alive at every 0.5 s check", life)
        self.assertIn("stop-3val window", life)
        self.assertIsNone(hp.poll())  # not TERMed

    def test_no_restart_kill_of_the_named_pid_is_no_death(self):
        """run-cell.sh KILL_NO_RESTART: crash-kill.sh norestart logs the marker for val2's pid, then val2 dies for
        good; the watcher keeps watching the survivors and the harness stop ends it as usual"""
        pids = self.nodes(die=2)
        hp, _ = self.harness()
        runlog = os.path.join(self.cell, "run.log")
        with open(runlog, "a") as f:
            f.write("kill-no-restart: SIGKILL val2 pid %d (no restart; height=600 committed=602)\n" % pids[2])
        stop = (
            '(sleep 2.5; for p in %d %d; do echo "stopping pid $p"; done >> %s; echo stopped. >> %s) &'
            % (pids[0], pids[1], shlex.quote(runlog), shlex.quote(runlog))
        )
        r = self.lib(
            self.drv,
            '%s\nwatch_nodes %s %d; echo "WN=$?"' % (stop, shlex.quote(self.cell), hp.pid),
            env=self.env(),
        )
        self.assertIn("WN=0", r.stdout, r.stdout + r.stderr)
        self.assertFalse(os.path.exists(os.path.join(self.cell, "node-death.txt")))
        self.assertIsNone(hp.poll())  # not TERMed

    def test_no_restart_marker_does_not_cover_another_node(self):
        """the marker names val2's pid: val1 dying is still a mid-cell death"""
        pids = self.nodes(die=1)
        hp, _ = self.harness()
        with open(os.path.join(self.cell, "run.log"), "a") as f:
            f.write("kill-no-restart: SIGKILL val2 pid %d (no restart; height=600 committed=602)\n" % pids[2])
        r = self.lib(
            self.drv,
            'watch_nodes %s %d; echo "WN=$?"' % (shlex.quote(self.cell), hp.pid),
            env=self.env(),
        )
        self.assertIn("WN=3", r.stdout, r.stdout + r.stderr)
        death = open(os.path.join(self.cell, "node-death.txt")).read()
        self.assertIn("DEATH node val1 comm=torus-node pid=%d " % pids[1], death)
        self.assertEqual(hp.wait(timeout=10), 130)

    def test_step_flags_the_dead_cell(self):
        """a clean fixture cell (rc 0, ACCEPT AGREE, md5 ok) whose node dies: its cells.tsv row is bad:death"""
        fx = "ozarchy-p3gate-300m-r1-m"
        shutil.rmtree(self.cell)
        shutil.copytree(os.path.join(CELLFIX, fx), self.cell)
        shutil.copy(os.path.join(CELLFIX, fx + ".cell.rc"), self.cell + ".cell.rc")
        self.nodes(die=0)
        hp, _ = self.harness()
        os.makedirs(self.spec["run_dir"], exist_ok=True)
        script = (
            'source "$TOOLS/cell-step.sh"\nexport CELL_STEP_LOG=$RUN/campaign.log\n'
            "BMD5[b]=f10815747be79f22f502b51a8cd466eb\n"
            'cell() { STARTED_LABEL=$P-$1; watch_nodes "$R/$P-$1" %d; return 0; }\n'
            'run_spec "r1-A:A:120:none:"; echo "RS=$?"\n' % hp.pid
        )
        r = self.lib(self.drv, script, env=self.env())
        self.assertIn("RS=0", r.stdout, r.stdout + r.stderr)
        rows = [
            ln.split("\t")
            for ln in open(os.path.join(self.spec["run_dir"], "cells.tsv"))
            .read()
            .splitlines()
        ]
        self.assertEqual(
            [(x[0], x[3], x[4], x[5], x[7]) for x in rows[1:]],
            [(LB, "0", "yes", "yes", "bad:death")],
        )
        log = open(os.path.join(self.spec["run_dir"], "campaign.log")).read()
        self.assertIn("CELL-FLAG %s death" % LB, log)


class MemguardTest(FakeHostBase):
    """memguard UNIT: while the unit is active, MemAvailable < 6 GB (6000000 kB) stops it (stubbed meminfo)"""

    def guard(self, kb, unit_life):
        drv = self.generate(self.write_spec(minimal_spec(self.tmp)))
        unit = self.spawn("sleep", str(unit_life))
        with open(os.path.join(self.units, "bench-x.pid"), "w") as f:
            f.write("%d\n" % unit.pid)
        self.meminfo(kb)
        r = self.lib(drv, 'memguard bench-x; echo "MG=$?"', env=self.env())
        return r, unit

    def test_trips_below_6_gb(self):
        r, unit = self.guard(5999999, 60)
        self.assertIn("MG=0", r.stdout, r.stdout + r.stderr)
        self.assertRegex(
            r.stdout,
            r"\[\d\d:\d\d:\d\d\] MEMGUARD: MemAvailable 5999999 kB < 6 GB -> stopping bench-x\n",
        )
        self.assertEqual(
            open(os.path.join(self.units, "stops.log")).read(),
            "STOPPED bench-x.service\n",
        )
        self.assertEqual(unit.wait(timeout=10), -signal.SIGTERM)

    def test_quiet_at_6_gb_until_the_unit_ends(self):
        r, unit = self.guard(6000000, 2)
        self.assertIn("MG=0", r.stdout, r.stdout + r.stderr)
        self.assertNotIn("MEMGUARD", r.stdout)
        self.assertFalse(os.path.exists(os.path.join(self.units, "stops.log")))
        self.assertEqual(unit.wait(timeout=10), 0)


def cpu_line(user, system, idle, iowait, guest=0):
    """an aggregate /proc/stat cpu line: user nice system idle iowait irq softirq steal guest guest_nice"""
    return "cpu  %d 0 %d %d %d 0 0 0 %d 0\n" % (user, system, idle, iowait, guest)


class QuietSpecTest(Base):
    """spec max_busy_pct (s131, archy 8k: the 1-min load average fell below max_load 2 only ~2.4 min after an
    idle-again host, the 142-152 s gap between cells): a number in (0, 100], default 10; max_load is still
    accepted from older specs but no longer gates (the generator says so once)"""

    def bad(self, spec, needle):
        with self.assertRaises(self.gen.SpecError) as cm:
            self.gen.validate(spec)
        self.assertIn(needle, str(cm.exception))

    def test_default_and_values(self):
        self.assertEqual(self.gen.validate(minimal_spec(self.tmp))["max_busy_pct"], 10)
        for v in (0.5, 25, 100):
            p = self.gen.validate(minimal_spec(self.tmp, max_busy_pct=v))
            self.assertEqual(p["max_busy_pct"], v)

    def test_bad_values(self):
        for v in (0, -1, 100.5, True, "10", None, [10]):
            self.bad(minimal_spec(self.tmp, max_busy_pct=v), "max_busy_pct")

    def test_max_load_still_accepted_and_ignored(self):
        self.gen.validate(minimal_spec(self.tmp, max_load=2))
        p = self.write_spec(minimal_spec(self.tmp, max_load=2))
        out = os.path.join(self.tmp, "ml.sh")
        r = subprocess.run(
            [sys.executable, "-I", GEN, p, "-o", out], capture_output=True, text=True
        )
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertEqual(r.stderr.count("max_load"), 1, r.stderr)
        self.assertIn("ignored", r.stderr)
        self.assertIsNone(re.search(r"^MAX_LOAD=", open(out).read(), re.M))
        r = subprocess.run(
            [
                sys.executable,
                "-I",
                GEN,
                self.write_spec(minimal_spec(self.tmp), "n.json"),
                "-o",
                os.path.join(self.tmp, "n.sh"),
            ],
            capture_output=True,
            text=True,
        )
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertNotIn("max_load", r.stderr)

    def test_rendered_config(self):
        drv = self.generate(self.write_spec(minimal_spec(self.tmp, max_busy_pct=25)))
        r = self.lib(drv, 'echo "B=$MAX_BUSY_PCT S=$QUIET_SAMPLE_S"')
        self.assertIn("B=25 S=5", r.stdout, r.stdout + r.stderr)
        listed = self.lib(drv, 'echo "$DRIVER_VARS"').stdout.split()
        self.assertIn("MAX_BUSY_PCT", listed)
        self.assertIn("QUIET_SAMPLE_S", listed)


class QuietTest(Base):
    """quiet(): no build / node / bench / analysis process (stubbed here), then a QUIET_SAMPLE_S sample of the
    aggregate cpu line of $PROC/stat (DRIVER_PROC) must be below MAX_BUSY_PCT busy, where busy % =
    100 * (dtotal - didle - diowait) / dtotal. A stubbed sleep moves the fake /proc/stat to the next sample."""

    BASE = (100000, 50000, 9000000, 4000, 777)  # user system idle iowait guest

    def quiet(self, deltas, hits="", wait=3600, **spec):
        """deltas: one (user, system, idle, iowait[, guest]) increment per sleep; returns (rc, sleeps, stdout)"""
        drv = self.generate(self.write_spec(minimal_spec(self.tmp, **spec)))
        proc, samp = os.path.join(self.tmp, "proc"), os.path.join(self.tmp, "samp")
        os.makedirs(proc)
        os.makedirs(samp)
        cur = list(self.BASE)
        for k in range(len(deltas) + 1):
            if k:
                d = list(deltas[k - 1]) + [0] * (5 - len(deltas[k - 1]))
                cur = [a + b for a, b in zip(cur, d)]
            with open(os.path.join(samp, "stat.%d" % k), "w") as f:
                f.write(cpu_line(*cur) + "cpu0 1 2 3 4 5 6 7 8 9 10\nintr 12345\n")
        shutil.copy(os.path.join(samp, "stat.0"), os.path.join(proc, "stat"))
        script = r"""
buildhits() { printf '%s' "$HITS"; }; benchhits() { :; }; anahits() { :; }
sleep() { echo "$1" >> "$SAMP/sleeps"; local k; k=$(wc -l < "$SAMP/sleeps")
          [ -f "$SAMP/stat.$k" ] && cp "$SAMP/stat.$k" "$PROC/stat"; return 0; }
quiet; echo "QRC=$?"
"""
        r = self.lib(
            drv,
            script,
            env={
                "DRIVER_PROC": proc,
                "SAMP": samp,
                "HITS": hits,
                "QUIET_WAIT_S": str(wait),
            },
        )
        m = re.search(r"QRC=(\d+)", r.stdout)
        self.assertIsNotNone(m, r.stdout + r.stderr)
        sl = os.path.join(samp, "sleeps")
        sleeps = open(sl).read().split() if os.path.exists(sl) else []
        return int(m.group(1)), sleeps, r.stdout

    def test_idle_sample_returns_at_once(self):
        # 5 % busy: iowait counts as idle (with iowait as busy it would be 10 %, not < 10)
        rc, sleeps, out = self.quiet([(4, 1, 90, 5)])
        self.assertEqual((rc, sleeps), (0, ["5"]), out)

    def test_guest_time_is_not_counted_twice(self):
        # guest is already inside user: 9 % busy (summed again it would be 9+40 / 140 = 35 %)
        rc, sleeps, out = self.quiet([(49, 0, 500, 0, 40)], max_busy_pct=10)
        self.assertEqual((rc, sleeps), (0, ["5"]), out)

    def test_busy_sample_waits_then_idle_returns(self):
        rc, sleeps, out = self.quiet(
            [(40, 10, 50, 0), (100, 100, 800, 0), (2, 1, 97, 0)]
        )
        self.assertEqual((rc, sleeps), (0, ["5", "10", "5"]), out)

    def test_busy_until_timeout(self):
        rc, sleeps, out = self.quiet([(40, 10, 50, 0)] * 20, wait=40)
        self.assertEqual(rc, 1, out)
        self.assertRegex(out, r"host not quiet after 40s \(cpu busy 50(\.0)?%")

    def test_threshold_is_the_specs(self):
        rc, sleeps, out = self.quiet([(40, 10, 50, 0)], max_busy_pct=60)
        self.assertEqual((rc, sleeps), (0, ["5"]), out)

    def test_process_hits_wait_without_sampling(self):
        rc, sleeps, out = self.quiet([(1, 0, 99, 0)] * 10, hits="cargo:4242 ", wait=30)
        self.assertEqual(rc, 1, out)
        self.assertEqual(sleeps, ["10", "10"], out)
        self.assertIn("cargo:4242", out)


class CellRunTest(FakeHostBase):
    """the generated cell() for real (library mode, run_spec) against the fakes: detach.sh starts the cell command
    in its own session (the unit), systemctl --user answers for it, run-cell.sh runs 3 stub nodes. quiet() is
    stubbed (the test host may be busy); the host's sysctls are read as they are (pinned first, as the run does)."""

    def run_cell(self, spec, pre="sysctl_pin", **env):
        drv = self.generate(self.write_spec(spec, "cellrun.json"), "cellrun.sh")
        os.makedirs(spec["run_dir"], exist_ok=True)
        script = "\n".join(
            [
                "DRIVER_LIB_ONLY=1; source %s || exit 99" % shlex.quote(drv),
                "HARNESS=%s" % shlex.quote(os.path.join(self.bin, "run-cell.sh")),
                "quiet() { return 0; }",
                'source "$TOOLS/cell-step.sh" || exit 98',
                "export CELL_STEP_LOG=$RUN/campaign.log",
                pre,
                'run_spec "r1-A:A:120:none:"; echo "RUN_SPEC_RC=$?"',
                "end_pass",
            ]
        )
        e = dict(os.environ)
        e.update(
            self.env(
                DRIVER_DETACH=os.path.join(self.bin, "detach.sh"),
                FAKE_PROC=self.proc,
                # the fake cell outlives cell()'s harness-pid lookup (0.2 s polls) on a loaded host
                FAKE_RUN_S="5",
            )
        )
        e.update(env)
        out = os.path.join(self.tmp, "cellrun.out")
        with open(out, "w") as fo:
            subprocess.run(
                ["bash", "-c", script],
                stdout=fo,
                stderr=subprocess.STDOUT,
                env=e,
                timeout=300,
            )
        self.cell = os.path.join(spec["results_root"], LB)
        return open(out).read()

    def cellfile(self, name):
        return open(os.path.join(self.cell, name)).read()

    def row(self, spec):
        rows = [
            ln.split("\t")
            for ln in open(os.path.join(spec["run_dir"], "cells.tsv"))
            .read()
            .splitlines()
        ]
        self.assertEqual(len(rows), 2, rows)
        self.assertEqual(rows[1][0], LB)
        return rows[1]

    def test_cell_starts_watchers_and_samplers(self):
        spec = minimal_spec(self.tmp)
        out = self.run_cell(spec)
        self.assertIn("RUN_SPEC_RC=0", out, out)
        self.assertRegex(
            out, r"%s unit=bench-%s main_pid=\d+ harness_pid=\d+ " % (LB, LB)
        )
        life = self.cellfile("node-life.txt")
        self.assertRegex(life, r"watching node pids: \d+ \d+ \d+\n")
        self.assertIn(
            "all 3 node pids alive at every 0.5 s check until the harness stop began",
            life,
        )
        self.assertFalse(os.path.exists(os.path.join(self.cell, "node-death.txt")), out)
        for f in SAMPLER_FILES + ("sysctl.txt",):
            self.assertTrue(os.path.exists(os.path.join(self.cell, f)), f + "\n" + out)
        self.assertEqual(
            [ln.split()[0] for ln in self.cellfile("sysctl.txt").splitlines()],
            ["start", "end"],
        )
        for s in (
            "CHECK OK %s sysctl (start)" % LB,
            "CHECK OK %s sysctl (end)" % LB,
            "CHECK OK %s hardscan done" % LB,
            "CHECK SOAK %s" % LB,
            "[%s] fds max/end" % LB,
        ):
            self.assertIn(s, out)
        # in-line (DEFER_SCANS=0 here: no preflight_post): hardscan and soakchk run in cell(), as the pre-end-pass
        # driver did, before run_spec's step; the end pass does nothing (EndPassRunTest: the deferred case)
        rc_at = out.index("RUN_SPEC_RC=0")
        self.assertLess(out.index("CHECK OK %s hardscan done" % LB), rc_at)
        self.assertLess(out.index("CHECK SOAK %s" % LB), rc_at)
        self.assertLess(out.index("[%s] fds max/end" % LB), rc_at)
        self.assertNotIn("END PASS", out)
        self.assertEqual(self.row(spec)[7], "ok", out)
        self.assertEqual(sampler_procs(self.cell), [], "soak sampler left running")

    def metrics_servers(self, n=3):
        body = (
            "# HELP torus_x x\n# TYPE torus_x counter\n"
            "torus_consensus_rate_limited_dropped_total 7\n"
            'torus_commit_interval_seconds_bucket{le="0.064"} 1\n'
            'torus_commit_interval_seconds_bucket{le="0.256"} 2\n'
            'torus_commit_interval_seconds_bucket{le="+Inf"} 3\n'
            'torus_other_seconds_bucket{le="1"} 4\n'
        ).encode()

        class H(http.server.BaseHTTPRequestHandler):
            def do_GET(self):
                self.send_response(200)
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

            def log_message(self, *a):
                pass

        ports = []
        for _ in range(n):
            srv = http.server.ThreadingHTTPServer(("127.0.0.1", 0), H)
            threading.Thread(target=srv.serve_forever, daemon=True).start()
            self.addCleanup(srv.server_close)
            self.addCleanup(srv.shutdown)
            ports.append(srv.server_address[1])
        return ports

    def test_soak_sampler_every_cell(self):
        """the 1 Hz soak sampler starts with the cell, scrapes the METS ports (here 3 fake /metrics servers) in the
        hand drivers' format, and is stopped (gone) when cell() returns"""
        ports = self.metrics_servers()
        spec = minimal_spec(self.tmp)
        out = self.run_cell(spec, pre="sysctl_pin\nMETS=(%d %d %d)" % tuple(ports))
        self.assertIn("RUN_SPEC_RC=0", out, out)
        with gzip.open(os.path.join(self.cell, "metrics-1hz.txt.gz"), "rt") as z:
            m = z.read()
        for i in range(3):
            self.assertRegex(m, r"(?m)^T \d+\.\d{3} val%d ok \d+\.\d{4}$" % i)
        self.assertIn("torus_consensus_rate_limited_dropped_total 7\n", m)
        self.assertIn('torus_commit_interval_seconds_bucket{le="0.256"} 2\n', m)
        self.assertNotIn('le="0.064"', m)
        self.assertNotIn("torus_other_seconds_bucket", m)
        self.assertNotIn("# HELP", m)
        host = self.cellfile("host-1hz.tsv").splitlines()
        self.assertEqual(host[0], HOST_1HZ_HEADER)
        self.assertGreaterEqual(len(host), 2, host)
        self.assertEqual(len(host[1].split("\t")), 23, host[1])
        self.assertRegex(
            out,
            r"CHECK SOAK %s: .*metrics-1hz ok scrapes [1-9]\d*, host-1hz rows [1-9]"
            % LB,
        )
        self.assertEqual(sampler_procs(self.cell), [], "soak sampler left running")

    def test_node_death_mid_cell(self):
        """val1 exits 2 s into the cell: node-death.txt, harness TERMed, CELL FAIL, row flagged death + rerun
        queued, and the campaign goes on (run_spec 0)"""
        spec = minimal_spec(self.tmp)
        out = self.run_cell(spec, FAKE_DIE="1", FAKE_RUN_S="120")
        self.assertIn("RUN_SPEC_RC=0", out, out)
        pids = re.search(
            r"node pids: (\d+) (\d+) (\d+)", self.cellfile("run.log")
        ).groups()
        death = self.cellfile("node-death.txt")
        self.assertRegex(
            death,
            r"^DEATH node val1 comm=torus-node pid=%s detected at \d{4}-\d\d-\d\d \d\d:\d\d:\d\d\.\d{3} "
            r"\(0\.5 s poll; previous check ok <= ~0\.6 s earlier\); last val1 log line: ERROR fake val1 last line\n$"
            % pids[1],
        )
        self.assertIn("interrupted", self.cellfile("run.log"))  # the harness got TERM
        self.assertIn("CELL FAIL %s rc=130 death: DEATH node val1 " % LB, out)
        self.assertEqual(sampler_procs(self.cell), [], "soak sampler left running")
        flag = self.row(spec)[7]
        self.assertTrue(flag.startswith("bad:"), flag)
        self.assertIn("death", flag[4:].split(","), flag)
        self.assertIn(
            "CELL-FLAG %s " % LB,
            open(os.path.join(spec["run_dir"], "campaign.log")).read(),
        )
        self.assertIn(
            LB + "-rerun",
            open(os.path.join(spec["run_dir"], "rerun-queue.tsv")).read(),
        )

    def test_memguard_stops_the_cell_unit(self):
        self.meminfo(5000000)
        spec = minimal_spec(self.tmp)
        out = self.run_cell(spec, FAKE_RUN_S="120")
        self.assertIn(
            "MEMGUARD: MemAvailable 5000000 kB < 6 GB -> stopping bench-%s" % LB, out
        )
        self.assertIn(
            "STOPPED bench-%s.service" % LB,
            open(os.path.join(self.units, "stops.log")).read(),
        )
        self.assertIn("rc=unit-ended-without-rc", out)
        self.assertTrue(self.row(spec)[7].startswith("bad:"), out)

    def test_samplers_and_host_checks_off(self):
        spec = minimal_spec(self.tmp, samplers=False, host_checks=False)
        out = self.run_cell(spec, pre="")
        self.assertIn("RUN_SPEC_RC=0", out, out)
        self.assertIn(
            "watching node pids", self.cellfile("node-life.txt")
        )  # the watchers always run
        self.assertIn("CHECK OK %s hardscan done" % LB, out)
        for f in SAMPLER_FILES + ("sysctl.txt",):
            self.assertFalse(os.path.exists(os.path.join(self.cell, f)), f)
        self.assertNotIn("CHECK SOAK", out)
        self.assertNotIn(" sysctl (", out)
        self.assertEqual(self.row(spec)[7], "ok", out)


class HostChecksTest(FakeHostBase):
    """host_checks: the sysctl record and check (spec values, else the dirty limits pinned at the run's
    preflight), the MemAvailable preflight; a fake sysctl"""

    HOST = {
        "vm__dirty_bytes": 0,
        "vm__dirty_background_bytes": 0,
        "vm__dirty_ratio": 20,
        "vm__dirty_background_ratio": 10,
        "vm__dirty_expire_centisecs": 3000,
        "vm__dirty_writeback_centisecs": 500,
        "kernel__perf_event_paranoid": 2,
    }

    def setUp(self):
        super().setUp()
        self.set_sysctl(**self.HOST)

    def drv(self, **over):
        return self.generate(
            self.write_spec(minimal_spec(self.tmp, **over), "hc.json"), "hc.sh"
        )

    def test_pinned_dirty_limits_must_hold(self):
        """no spec values: the dirty limits are pinned at the preflight; a change before a cell's end fails it"""
        drv = self.drv()
        r = self.lib(
            drv,
            'sysctl_pin; mkdir -p "$R/x"; sysctlchk x start; echo "S=$?"; echo 5 > %s; sysctlchk x end; echo "E=$?"'
            % shlex.quote(os.path.join(self.sysctl_dir, "vm.dirty_bytes")),
            env=self.env(),
        )
        self.assertIn("S=0", r.stdout, r.stdout + r.stderr)
        self.assertIn("E=1", r.stdout, r.stdout + r.stderr)
        self.assertIn("CHECK OK x sysctl (start):", r.stdout)
        self.assertIn("CHECK FAIL x (end): sysctl vm.dirty_bytes=5 (want 0)", r.stdout)
        rec = open(os.path.join(self.tmp, "res", "x", "sysctl.txt")).read().splitlines()
        self.assertEqual([ln.split()[0] for ln in rec], ["start", "end"])
        self.assertIn(
            " dirty_bytes=0 dirty_background_bytes=0 dirty_ratio=20 dirty_background_ratio=10"
            " dirty_expire_centisecs=3000 dirty_writeback_centisecs=500 perf_event_paranoid=2 ",
            rec[0],
        )
        self.assertIn(" dirty_bytes=5 ", rec[1])

    def test_spec_values_are_required_at_preflight(self):
        drv = self.drv(host_checks={"sysctl": {"vm.dirty_bytes": 268435456}})
        r = self.lib(drv, "sysctl_pin; echo PINNED", env=self.env())
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("PREFLIGHT FAIL", r.stdout)
        self.assertIn("vm.dirty_bytes=0 (want 268435456)", r.stdout)
        self.assertNotIn("PINNED", r.stdout)
        self.set_sysctl(vm__dirty_bytes=268435456)
        r = self.lib(
            drv,
            'sysctl_pin; echo PINNED; mkdir -p "$R/x"; sysctlchk x start',
            env=self.env(),
        )
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertIn("PINNED", r.stdout)
        self.assertIn(
            "CHECK OK x sysctl (start): vm.dirty_background_bytes=0 vm.dirty_bytes=268435456",
            r.stdout,
        )

    def test_min_mem_avail_preflight(self):
        drv = self.drv()
        self.meminfo(39999999)
        r = self.lib(drv, "preflight_mem; echo MEMOK", env=self.env())
        self.assertNotEqual(r.returncode, 0)
        self.assertIn(
            "PREFLIGHT FAIL: MemAvailable 39999999 kB < 40000000 kB", r.stdout
        )
        self.meminfo(40000000)
        r = self.lib(drv, "preflight_mem; echo MEMOK", env=self.env())
        self.assertIn("MEMOK", r.stdout, r.stdout + r.stderr)
        drv = self.drv(host_checks={"min_mem_avail_gb": 0.5})
        self.meminfo(600000)
        r = self.lib(drv, "preflight_mem; echo MEMOK", env=self.env())
        self.assertIn("MEMOK", r.stdout, r.stdout + r.stderr)

    def test_plan_rows(self):
        on = self.plan(self.drv())
        for ln in (
            "WATCH node-death memguard hardscan",
            "SAMPLERS on du mem metrics fd soak-1hz vmstat rocksdb-log fd_stop=16384",
            "HOSTCHECK on min_mem_avail_kb=40000000",
            "SYSCTL vm.dirty_background_bytes pinned",
            "SYSCTL vm.dirty_bytes pinned",
        ):
            self.assertIn(ln, on)
        off = self.plan(self.drv(samplers=False, host_checks=False))
        self.assertIn("WATCH node-death memguard hardscan", off)
        self.assertIn("SAMPLERS off", off)
        self.assertIn("HOSTCHECK off", off)
        self.assertEqual([ln for ln in off if ln.startswith("SYSCTL ")], [])


class SamplerCheckTest(Base):
    """fdsum (the fd stop) and hardscan on fixture cell files"""

    def setUp(self):
        super().setUp()
        self.spec = minimal_spec(self.tmp)
        self.drv = self.generate(self.write_spec(self.spec))
        self.cell = os.path.join(self.spec["results_root"], LB)
        os.makedirs(self.cell)

    def test_fd_stop(self):
        with open(os.path.join(self.cell, "node-nofile.txt"), "w") as f:
            f.write(
                "pid=1 Max open files            1048576              1048576              files\n"
            )
        with open(os.path.join(self.cell, "fds.txt"), "w") as f:
            f.write("1700000000 100 200 300\n1700000005 16385 210 -\n")
        r = self.lib(self.drv, 'fdsum %s; echo "FD=$?"' % LB)
        self.assertIn(
            "fds max/end val0 16385/16385 val1 210/210 val2 300/300 (limit soft/hard 1048576/1048576)",
            r.stdout,
        )
        self.assertIn("FD=1", r.stdout)
        with open(os.path.join(self.cell, "fds.txt"), "w") as f:
            f.write("1700000000 100 16384 300\n")
        r = self.lib(self.drv, 'fdsum %s; echo "FD=$?"' % LB)
        self.assertIn("FD=0", r.stdout, r.stdout + r.stderr)

    def test_hardscan_reports_warn_error_lines_only(self):
        import gzip

        logs = {
            0: "2026 ERROR torus: thread 'exec' panicked at x.rs:1\n"
            "2026  INFO torus: running state hash fail-stop (TORUS_STATE_HASH_FAILSTOP) on=false\n",
            1: "2026  INFO torus: running state hash fail-stop (TORUS_STATE_HASH_FAILSTOP) on=false\n",
            2: "2026  WARN torus: \x1b[31mSTATE HASH MISMATCH\x1b[0m at 7\n",
        }
        for v, txt in logs.items():
            with gzip.open(os.path.join(self.cell, "val%d.log.gz" % v), "wt") as f:
                f.write(txt)
        r = self.lib(self.drv, "hardscan %s" % LB)
        self.assertIn(
            "HARD FINDING %s: val0 has 1 WARN/ERROR correctness lines: 2026 ERROR torus: thread 'exec' panicked"
            % LB,
            r.stdout,
        )
        self.assertIn(
            "HARD FINDING %s: val2 has 1 WARN/ERROR correctness lines: 2026  WARN torus: STATE HASH MISMATCH at 7"
            % LB,
            r.stdout,
        )
        self.assertNotIn("val1 has", r.stdout)
        self.assertIn("CHECK OK %s hardscan done" % LB, r.stdout)


SOAK_MD5 = "f147d7c657b2d71f9bc2934825845695"  # the s58rl / p4soak10 hand drivers' soak-sampler.py
HOST_1HZ_HEADER = "\t".join(
    ["ts", "Dirty_kB", "Writeback_kB", "MemAvailable_kB", "Cached_kB"]
    + [
        "val%d_%s" % (i, k)
        for i in range(3)
        for k in (
            "VmRSS",
            "RssAnon",
            "RssFile",
            "voluntary_ctxt_switches",
            "nonvoluntary_ctxt_switches",
            "cpu_ticks",
        )
    ]
)


def sampler_procs(cell):
    """pids of soak-sampler.py processes still running for this cell dir"""
    out = []
    for f in glob.glob("/proc/[0-9]*/cmdline"):
        try:
            c = open(f, "rb").read()
        except OSError:
            continue
        if b"soak-sampler.py" in c and cell.encode() in c:
            out.append(f.split("/")[2])
    return out


class SoakSamplerTest(Base):
    """the 1 Hz soak sampler (samplers on): metrics-1hz.txt.gz + host-1hz.tsv, the hand drivers' sampler copied
    unchanged; soakchk's sample counts; its preflight; show"""

    def setUp(self):
        super().setUp()
        self.spec = minimal_spec(self.tmp)
        self.drv = self.generate(self.write_spec(self.spec))
        self.cell = os.path.join(self.spec["results_root"], LB)
        os.makedirs(self.cell)

    def test_sampler_is_the_hand_drivers_copy(self):
        self.assertEqual(md5_of(os.path.join(HERE, "soak-sampler.py")), SOAK_MD5)

    def soak(self, scrapes, rows, dur=10):
        if scrapes is not None:
            with gzip.open(os.path.join(self.cell, "metrics-1hz.txt.gz"), "wt") as z:
                z.write("T 1700000000.000 val0 ERR URLError\n")
                for i in range(scrapes):
                    z.write(
                        "T 1700000%03d.000 val%d ok 0.0012\nx_total 1\n" % (i, i % 3)
                    )
        if rows is not None:
            with open(os.path.join(self.cell, "host-1hz.tsv"), "w") as f:
                f.write(HOST_1HZ_HEADER + "\n")
                for i in range(rows):
                    f.write("%d.000\t1\n" % (1700000000 + i))
        r = self.lib(self.drv, "soakchk %s %d" % (LB, dur))
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)  # recorded, never fails
        return r.stdout

    def test_soakchk_counts_enough(self):
        out = self.soak(30, 10)
        self.assertIn("metrics-1hz ok scrapes 30, host-1hz rows 10", out)
        self.assertNotIn("CHECK WARN", out)

    def test_soakchk_too_few_samples(self):
        out = self.soak(29, 9)
        self.assertIn(
            "CHECK WARN %s: metrics-1hz has 29 ok scrapes (< 3 x 10)" % LB, out
        )
        self.assertIn("CHECK WARN %s: host-1hz has 9 rows (< 10)" % LB, out)
        os.remove(os.path.join(self.cell, "metrics-1hz.txt.gz"))
        os.remove(os.path.join(self.cell, "host-1hz.tsv"))
        out = self.soak(None, None)
        self.assertIn("metrics-1hz ok scrapes 0, host-1hz rows 0", out)
        self.assertIn("CHECK WARN %s: metrics-1hz has 0 ok scrapes" % LB, out)
        self.assertIn("CHECK WARN %s: host-1hz has 0 rows" % LB, out)

    def test_show(self):
        plan = self.plan(self.drv)
        self.assertIn(
            "SOAK on %s/soak-sampler.py metrics-1hz.txt.gz host-1hz.tsv"
            % self.spec["tools_dir"],
            plan,
        )
        self.assertTrue(
            any(ln.startswith("SAMPLERS on ") and " soak-1hz " in ln for ln in plan),
            plan,
        )
        drv = self.generate(
            self.write_spec(minimal_spec(self.tmp, samplers=False), "off.json"),
            "off.sh",
        )
        plan = self.plan(drv)
        self.assertIn("SAMPLERS off", plan)
        self.assertFalse([ln for ln in plan if ln.startswith("SOAK")], plan)

    def dry(self, drv):
        return subprocess.run(
            ["bash", drv, "dry"], capture_output=True, text=True, timeout=60
        )

    def test_preflight_needs_the_sampler(self):
        sp = os.path.join(self.spec["tools_dir"], "soak-sampler.py")
        r = self.dry(self.drv)
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        with open(sp, "w") as f:
            f.write("import sys\n")  # compiles, lacks the slow-tail buckets
        r = self.dry(self.drv)
        self.assertEqual(r.returncode, 1, r.stdout + r.stderr)
        self.assertIn("PREFLIGHT FAIL", r.stdout)
        self.assertIn("slow-tail", r.stdout)
        with open(sp, "w") as f:
            f.write("def (:\n")
        r = self.dry(self.drv)
        self.assertEqual(r.returncode, 1, r.stdout + r.stderr)
        self.assertIn("does not compile", r.stdout)
        os.remove(sp)
        r = self.dry(self.drv)
        self.assertEqual(r.returncode, 1, r.stdout + r.stderr)
        self.assertIn("soak-sampler.py missing", r.stdout)
        # samplers off: no sampler needed
        drv = self.generate(
            self.write_spec(minimal_spec(self.tmp, samplers=False), "off.json"),
            "off.sh",
        )
        r = self.dry(drv)
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)


class WatcherSpecTest(Base):
    """the spec options samplers / host_checks: strict like every other key"""

    def bad(self, spec, needle):
        with self.assertRaises(self.gen.SpecError) as cm:
            self.gen.validate(spec)
        self.assertIn(needle, str(cm.exception))

    def test_defaults_on(self):
        p = self.gen.validate(minimal_spec(self.tmp))
        self.assertIs(p["samplers"], True)
        self.assertEqual(p["host_checks"], {"min_mem_avail_kb": 40000000, "sysctl": {}})

    def test_values(self):
        p = self.gen.validate(
            minimal_spec(
                self.tmp,
                samplers=False,
                host_checks={
                    "min_mem_avail_gb": 12,
                    "sysctl": {
                        "vm.dirty_bytes": 268435456,
                        "kernel.numa_balancing": "0",
                    },
                },
            )
        )
        self.assertIs(p["samplers"], False)
        self.assertEqual(
            p["host_checks"],
            {
                "min_mem_avail_kb": 12000000,
                "sysctl": {"vm.dirty_bytes": "268435456", "kernel.numa_balancing": "0"},
            },
        )
        self.assertIsNone(
            self.gen.validate(minimal_spec(self.tmp, host_checks=False))["host_checks"]
        )

    def test_bad_values(self):
        self.bad(minimal_spec(self.tmp, samplers="yes"), "samplers")
        self.bad(minimal_spec(self.tmp, samplers=["du"]), "samplers")
        self.bad(minimal_spec(self.tmp, host_checks="on"), "host_checks")
        self.bad(minimal_spec(self.tmp, host_checks={"sysctls": {}}), "sysctls")
        self.bad(
            minimal_spec(self.tmp, host_checks={"min_mem_avail_gb": -1}),
            "min_mem_avail_gb",
        )
        self.bad(
            minimal_spec(self.tmp, host_checks={"min_mem_avail_gb": True}),
            "min_mem_avail_gb",
        )
        self.bad(
            minimal_spec(self.tmp, host_checks={"sysctl": {"dirty bytes": "1"}}),
            "dirty bytes",
        )
        self.bad(
            minimal_spec(self.tmp, host_checks={"sysctl": {"vm.dirty_bytes": "1 2"}}),
            "vm.dirty_bytes",
        )
        self.bad(
            minimal_spec(self.tmp, host_checks={"sysctl": {"vm.dirty_bytes": True}}),
            "vm.dirty_bytes",
        )
        self.bad(
            minimal_spec(self.tmp, host_checks={"sysctl": {"vm.dirty_bytes": ""}}),
            "vm.dirty_bytes",
        )

    def test_rendered_config(self):
        drv = self.generate(
            self.write_spec(
                minimal_spec(
                    self.tmp,
                    host_checks={"sysctl": {"vm.dirty_bytes": "268435456"}},
                )
            )
        )
        r = self.lib(
            drv,
            'echo "S=$SAMPLERS H=$HOST_CHECKS M=$MIN_MEM_AVAIL_KB W=${SYSCTL_WANT[vm.dirty_bytes]} N=${#SYSCTL_WANT[@]}"',
        )
        self.assertIn(
            "S=1 H=1 M=40000000 W=268435456 N=1", r.stdout, r.stdout + r.stderr
        )

    def test_overwrite_is_the_drivers(self):
        """run-cell.sh refuses a non-empty cell dir unless OVERWRITE=1, and the driver writes its records there
        before the harness starts (it refuses an existing cell dir itself): every cell runs with OVERWRITE=1, as
        Job 4's did; a spec may only repeat it (Codex P1)"""
        self.gen.validate(minimal_spec(self.tmp, harness_env={"OVERWRITE": "1"}))
        self.bad(minimal_spec(self.tmp, harness_env={"OVERWRITE": "0"}), "OVERWRITE")
        s = minimal_spec(self.tmp)
        s["arms"][1]["harness_env"] = {"OVERWRITE": ""}
        self.bad(s, "OVERWRITE")


# ---------------------------------------------------------------- warm-up gate, campaign checks

CHECKS_SH = r"""# campaign checks for the tests: each call appends "<fn> <label> <arm> ..." to $R/checks.log
okchk() { echo "okchk $1 $2 R=$R aenv=${AENV[$2]:-none} cell=$([ -d "$R/$1" ] && echo yes)" >> "$R/checks.log"; echo "CHECK OK $1 okchk"; }
alsochk() { echo "alsochk $1 $2" >> "$R/checks.log"; }
badchk() { echo "badchk $1 $2" >> "$R/checks.log"; echo "CHECK FAIL $1: badchk says no"; return 1; }
"""

# Codex P2: bash variables are dynamically scoped, so a check called from campaignchecks saw campaignchecks'
# rc=0 (and every other local of cell / run_spec / run_cells) instead of its own top-level rc=1
CHECKS_RC_SH = r"""rc=1
guardchk() { return "$rc"; }
"""

# dispatch: each name is a local of a driver function on the stack when a check runs (campaignchecks: fn r rc;
# cell: tg arm dur label t rc kv crc E ...; run_spec: spec tg arm dur label crc ...; run_cells: c). The check must
# see the file's own values, never the driver's, and must not see the driver locals the file never set.
CHECKS_OWN_SH = r"""arm=own-arm label=own-label t=own-t crc=own-crc tg=own-tg kv=own-kv rc=own-rc fn=own-fn r=own-r
dur=own-dur
E=(own-e0 own-e1)
declare -gA OWNMAP=([k]=own-k)
readonly ROLIM=own-rolim
ownchk() {
    local n bad=
    for n in arm label t crc tg kv rc fn r dur ROLIM; do [ "${!n-}" = "own-${n,,}" ] || bad+=" $n=${!n-UNSET}"; done
    [ "${E[*]-}" = "own-e0 own-e1" ] || bad+=" E=${E[*]-UNSET}"
    [ "${OWNMAP[k]-}" = own-k ] || bad+=" OWNMAP"
    echo "ownchk $1 $2${bad:+ BAD$bad}" >> "$R/checks.log"
    [ -z "$bad" ]
}
hidechk() {
    local n seen=
    for n in spec sfx kind killat rcf unit b nt fdok mp HP EP WP DP c xr; do [ -z "${!n+x}" ] || seen+=" $n=${!n}"; done
    echo "hidechk $1 $2${seen:+ SEEN$seen}" >> "$R/checks.log"
    [ -z "$seen" ]
}
globchk() { echo "globchk $1 $2 R=$R aenv=${AENV[$2]:-none} axe=${AXE[$2]:-none} arms=${ARMS[*]}" >> "$R/checks.log"; }
"""

# load: each name is a local of preflight_static (f b node m tgt) or of the old load_checks (fns before after f),
# the frames the file used to be sourced in: a top-level assignment landed in that local and was gone after the
# preflight, and the file saw the loader's values at source time. LOCMAP (declare -A without -g) is a local of
# load_checks (PREFLIGHT WARN), and the checks still see it.
CHECKS_LOAD_SH = r"""SRC_SEEN="f=${f-UNSET} m=${m-UNSET} b=${b-UNSET} node=${node-UNSET} tgt=${tgt-UNSET} fns=${fns-UNSET} before=${before-UNSET} after=${after-UNSET}"
f=own-f m=own-m b=own-b node=own-node tgt=own-tgt fns=own-fns before=own-before after=own-after
declare -A LOCMAP=([k]=own-k)
minechk() {
    local n bad=
    for n in f m b node tgt fns before after; do [ "${!n-}" = "own-$n" ] || bad+=" $n=${!n-UNSET}"; done
    [ "${LOCMAP[k]-}" = own-k ] || bad+=" LOCMAP"
    echo "minechk $1 $2 src[$SRC_SEEN]${bad:+ BAD$bad}" >> "$R/checks.log"
    [ -z "$bad" ]
}
"""


class WarmGateChecksSpecTest(Base):
    """spec warmup.gate and checks: strict like every other key"""

    def bad(self, spec, needle):
        with self.assertRaises(self.gen.SpecError) as cm:
            self.gen.validate(spec)
        self.assertIn(needle, str(cm.exception))

    def checks_file(self):
        f = os.path.join(self.tmp, "checks.sh")
        with open(f, "w") as fh:
            fh.write(CHECKS_SH)
        return f

    def test_gate_default_on_and_false_accepted(self):
        self.assertIs(self.gen.validate(minimal_spec(self.tmp))["warmup_gate"], True)
        p = self.gen.validate(
            minimal_spec(self.tmp, warmup={"arm": "A", "gate": False})
        )
        self.assertIs(p["warmup_gate"], False)
        self.assertEqual(p["cells"][0][:2], ("w1", "A"))  # the warm-up still runs
        p = self.gen.validate(minimal_spec(self.tmp, warmup={"gate": True}))
        self.assertIs(p["warmup_gate"], True)

    def test_gate_bad_values(self):
        self.bad(minimal_spec(self.tmp, warmup={"gate": "no"}), "warmup.gate")
        self.bad(minimal_spec(self.tmp, warmup={"gate": 0}), "warmup.gate")
        self.bad(minimal_spec(self.tmp, warmup={"gates": False}), "gates")

    def test_checks_ok(self):
        f = self.checks_file()
        p = self.gen.validate(
            minimal_spec(
                self.tmp, checks={"source": f, "per_cell": ["okchk", "alsochk"]}
            )
        )
        self.assertEqual(p["checks"], {"source": f, "per_cell": ["okchk", "alsochk"]})
        self.assertIsNone(self.gen.validate(minimal_spec(self.tmp))["checks"])

    def test_checks_bad(self):
        f = self.checks_file()

        def ck(**c):
            return minimal_spec(self.tmp, checks=c)

        self.bad(ck(source="checks.sh", per_cell=["okchk"]), "checks.source")
        self.bad(ck(source=f + " x", per_cell=["okchk"]), "checks.source")
        self.bad(ck(per_cell=["okchk"]), "source")
        self.bad(ck(source=f), "per_cell")
        self.bad(ck(source=f, per_cell=[]), "per_cell")
        self.bad(ck(source=f, per_cell="okchk"), "per_cell")
        for name in ("OkChk", "1chk", "ok-chk", "ok chk", "", "ok;rm", 7):
            self.bad(ck(source=f, per_cell=[name]), "checks.per_cell")
        self.bad(ck(source=f, per_cell=["okchk", "okchk"]), "twice")
        self.bad(ck(source=f, per_cell=["okchk"], post=[]), "post")
        self.bad(minimal_spec(self.tmp, checks=["okchk"]), "checks")
        # a driver function name: the driver would call itself (cell) or the check would replace it
        for name in (
            "cell",
            "log",
            "md5chk",
            "cell_step",
            "warmgatechk",
            "load_checks",
        ):
            self.bad(ck(source=f, per_cell=[name]), name)

    def test_cli_spec_error(self):
        p = self.write_spec(
            minimal_spec(self.tmp, checks={"source": "rel.sh", "per_cell": ["okchk"]})
        )
        r = subprocess.run(
            [sys.executable, "-I", GEN, p, "-o", os.path.join(self.tmp, "x.sh")],
            capture_output=True,
            text=True,
        )
        self.assertEqual(r.returncode, 2, r.stdout + r.stderr)
        self.assertIn("SPEC ERROR: checks.source", r.stderr)

    def test_show_lists_gate_and_checks(self):
        f = self.checks_file()
        drv = self.generate(
            self.write_spec(
                minimal_spec(
                    self.tmp, checks={"source": f, "per_cell": ["okchk", "alsochk"]}
                )
            )
        )
        plan = self.plan(drv)
        self.assertIn("WARMGATE on w1 s0", plan)
        self.assertIn("CHECKS %s okchk alsochk" % f, plan)
        self.assertIn("WARMGATE on w1 s0", self.plan(drv, smoke=True))
        drv = self.generate(
            self.write_spec(minimal_spec(self.tmp, warmup={"gate": False}), "s2.json"),
            "d2.sh",
        )
        plan = self.plan(drv)
        self.assertIn("WARMGATE off", plan)
        self.assertIn("CHECKS none", plan)
        drv = self.generate(
            self.write_spec(minimal_spec(self.tmp, warmup=False), "s3.json"), "d3.sh"
        )
        self.assertIn("WARMGATE on s0", self.plan(drv))

    def test_preflight_loads_checks(self):
        f = self.checks_file()
        drv = self.generate(
            self.write_spec(
                minimal_spec(self.tmp, checks={"source": f, "per_cell": ["okchk"]})
            )
        )
        r = subprocess.run(
            ["bash", drv, "dry"], capture_output=True, text=True, timeout=60
        )
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertIn("checks=okchk", r.stdout)
        self.assertIn("warmgate=w1,s0", r.stdout)

    def dry_fail(self, spec, needle):
        drv = self.generate(self.write_spec(spec, "pf.json"), "pf.sh")
        r = subprocess.run(
            ["bash", drv, "dry"], capture_output=True, text=True, timeout=60
        )
        self.assertEqual(r.returncode, 1, r.stdout + r.stderr)
        self.assertIn("PREFLIGHT FAIL", r.stdout)
        self.assertIn(needle, r.stdout)

    def test_preflight_missing_function(self):
        f = self.checks_file()
        self.dry_fail(
            minimal_spec(
                self.tmp, checks={"source": f, "per_cell": ["okchk", "nochk"]}
            ),
            "nochk",
        )

    def test_preflight_missing_or_failing_source(self):
        none = os.path.join(self.tmp, "none.sh")
        self.dry_fail(
            minimal_spec(self.tmp, checks={"source": none, "per_cell": ["okchk"]}),
            "none.sh",
        )
        f = os.path.join(self.tmp, "fails.sh")
        with open(f, "w") as fh:
            fh.write("okchk() { :; }\nfalse\n")
        self.dry_fail(
            minimal_spec(self.tmp, checks={"source": f, "per_cell": ["okchk"]}),
            "cannot source",
        )

    def test_preflight_source_must_not_redefine_driver_state(self):
        f = os.path.join(self.tmp, "clobber.sh")
        for body in ("log() { :; }\n", "RUN=/tmp/elsewhere\n", "AENV[A]=X=1\n"):
            with open(f, "w") as fh:
                fh.write("okchk() { :; }\n" + body)
            self.dry_fail(
                minimal_spec(self.tmp, checks={"source": f, "per_cell": ["okchk"]}),
                "redefines",
            )

    def dry_ok(self, body):
        f = os.path.join(self.tmp, "decl.sh")
        with open(f, "w") as fh:
            fh.write(body)
        drv = self.generate(
            self.write_spec(
                minimal_spec(self.tmp, checks={"source": f, "per_cell": ["okchk"]}),
                "decl.json",
            ),
            "decl-drv.sh",
        )
        r = subprocess.run(
            ["bash", drv, "dry"], capture_output=True, text=True, timeout=60
        )
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertNotIn("PREFLIGHT FAIL", r.stdout)
        self.assertIn("checks=okchk", r.stdout)  # the preflight still runs to the end
        return r.stdout

    def test_preflight_warns_on_top_level_declare_without_g(self):
        """the checks file is sourced inside load_checks: a top-level declare -A / -a without -g makes a
        function-local array (no error from bash; load_checks keeps it for the checks only), so preflight warns"""
        out = self.dry_ok(
            "okchk() { :; }\ndeclare -A WANT=([a]=1)\ndeclare -a LIST=(x y)\ndeclare -r -A RO=([b]=2)\n"
        )
        warns = [ln for ln in out.splitlines() if ln.startswith("PREFLIGHT WARN")]
        self.assertEqual(len(warns), 3, out)
        for w, (n, name) in zip(warns, ((2, "WANT"), (3, "LIST"), (4, "RO"))):
            self.assertIn("decl.sh:%d" % n, w)
            self.assertIn(name, w)
            self.assertIn("-g", w)

    def test_preflight_silent_on_declare_with_g(self):
        out = self.dry_ok(
            "okchk() {\n    declare -A loc=([a]=1)\n    local -a l2\n}\n"
            "declare -gA WANT=([a]=1)\ndeclare -g -a LIST=(x)\ndeclare -Ag M2\ndeclare -r RO=1\n"
            "# declare -A COMMENTED\n"
        )
        self.assertNotIn("PREFLIGHT WARN", out)

    def test_preflight_refuses_the_reserved_prefix(self):
        """the driver keeps its check state (load_checks, _run_check) in _DRV_ names: a checks file that uses one
        could clobber the snapshot its checks run with, so preflight refuses it (any mention, comments too)"""
        f = os.path.join(self.tmp, "reserved.sh")
        for body in (
            "_DRV_DECL=x\n",
            'okchk2() { echo "$_DRV_KEEP"; }\n',
            "# see _DRV_NAMES\n",
        ):
            with open(f, "w") as fh:
                fh.write("okchk() { :; }\n" + body)
            self.dry_fail(
                minimal_spec(self.tmp, checks={"source": f, "per_cell": ["okchk"]}),
                "reserved.sh:2: uses the reserved prefix _DRV_",
            )


class WarmGateRunTest(FakeHostBase):
    """the generated campaign loop (run_cells) for real against the fakes (as CellRunTest): the warm-up gate and
    the campaign checks"""

    W, M = "w1:A:120:none:", "r1-A:A:120:none:"

    def run_cells(self, spec, cells, smoke=False, loader="load_checks", **env):
        """loader: what loads the checks file (load_checks, or the whole preflight_static, whose locals the
        file must not see or land in)"""
        drv = self.generate(self.write_spec(spec, "gate.json"), "gate.sh")
        os.makedirs(spec["run_dir"], exist_ok=True)
        script = "\n".join(
            [
                "DRIVER_LIB_ONLY=1; source %s || exit 99" % shlex.quote(drv),
                "HARNESS=%s" % shlex.quote(os.path.join(self.bin, "run-cell.sh")),
                "quiet() { return 0; }",
                "apply_smoke" if smoke else "",
                "mkdir -p $RUN",
                "CELLS=(%s)" % " ".join(shlex.quote(c) for c in cells),
                'source "$TOOLS/cell-step.sh" || exit 98',
                "export CELL_STEP_LOG=$RUN/campaign.log",
                loader,
                "sysctl_pin",
                'run_cells; echo "RUN_CELLS_RC=$?"',
            ]
        )
        e = dict(os.environ)
        e.update(
            self.env(
                DRIVER_DETACH=os.path.join(self.bin, "detach.sh"),
                FAKE_PROC=self.proc,
                FAKE_RUN_S="4",
            )
        )
        e.update(env)
        out = os.path.join(self.tmp, "gate.out")
        with open(out, "w") as fo:
            subprocess.run(
                ["bash", "-c", script],
                stdout=fo,
                stderr=subprocess.STDOUT,
                env=e,
                timeout=300,
            )
        return open(out).read()

    def rows(self, spec, smoke=False):
        f = os.path.join(spec["run_dir"], "smoke" if smoke else "", "cells.tsv")
        return [ln.split("\t") for ln in open(f).read().splitlines()[1:]]

    def started(self, spec, label):
        return os.path.exists(os.path.join(spec["results_root"], label))

    def test_failing_warmup_stops_before_measured_cells(self):
        spec = minimal_spec(self.tmp)
        out = self.run_cells(
            spec, [self.W, self.M], FAKE_SUMMARY='{"validity": {"verdict": "REJECT"}}'
        )
        self.assertIn("RUN_CELLS_RC=1", out, out)
        self.assertIn(
            "CHECK FAIL test-camp-10m-w1: warm-up gate: validity=REJECT agreement=AGREE liveness=PASS",
            out,
        )
        self.assertIn("CAMPAIGN STOP at w1", out)
        self.assertFalse(self.started(spec, "test-camp-10m-r1-A"), out)
        # cell_step adds its own reason (verdict-reject) to the driver's check
        self.assertEqual(
            [(r[0], r[7]) for r in self.rows(spec)],
            [("test-camp-10m-w1", "bad:verdict-reject,check")],
        )

    def test_each_gate_condition(self):
        cases = [
            ('{"liveness": {"verdict": "FAIL"}}', "0", "liveness=FAIL"),
            (
                '{"headline": {"agreement_verdict": "DISAGREE"}}',
                "0",
                "agreement=DISAGREE",
            ),
            (
                '{"agreement": {"panic_or_failstop_lines": 2}}',
                "0",
                "panic_or_failstop_lines=2",
            ),
            ("{}", "3", "warm-up gate: run-cell rc=3"),
        ]
        for i, (summ, rc, needle) in enumerate(cases):
            spec = minimal_spec(
                self.tmp,
                campaign="g%d" % i,
                run_dir=os.path.join(self.tmp, "res", "g%d" % i),
                label_prefix="g%d" % i,
            )
            out = self.run_cells(spec, [self.W, self.M], FAKE_SUMMARY=summ, FAKE_RC=rc)
            self.assertIn("RUN_CELLS_RC=1", out, out)
            self.assertIn("CHECK FAIL g%d-w1: " % i, out)
            self.assertIn(needle, out)
            self.assertFalse(self.started(spec, "g%d-r1-A" % i), out)

    def test_gate_false_lets_the_campaign_go_on(self):
        spec = minimal_spec(self.tmp, warmup={"gate": False})
        out = self.run_cells(
            spec, [self.W, self.M], FAKE_SUMMARY='{"validity": {"verdict": "REJECT"}}'
        )
        self.assertIn("RUN_CELLS_RC=0", out, out)
        self.assertNotIn("warm-up gate", out)
        self.assertTrue(self.started(spec, "test-camp-10m-r1-A"), out)
        self.assertEqual(
            [r[0] for r in self.rows(spec)], ["test-camp-10m-w1", "test-camp-10m-r1-A"]
        )

    def test_passing_gate_and_checks_on_every_cell(self):
        f = os.path.join(self.tmp, "checks.sh")
        with open(f, "w") as fh:
            fh.write(CHECKS_SH)
        spec = minimal_spec(
            self.tmp, checks={"source": f, "per_cell": ["okchk", "alsochk"]}
        )
        out = self.run_cells(spec, [self.W, "r1-M1:M1:120:none:"])
        self.assertIn("RUN_CELLS_RC=0", out, out)
        self.assertIn(
            "CHECK OK test-camp-10m-w1 warm-up gate: validity=ACCEPT agreement=AGREE liveness=PASS"
            " panic_or_failstop_lines=0",
            out,
        )
        # measured cells are not gated
        self.assertNotIn("CHECK OK test-camp-10m-r1-M1 warm-up gate", out)
        R = spec["results_root"]
        self.assertEqual(
            open(os.path.join(R, "checks.log")).read().splitlines(),
            [
                "okchk test-camp-10m-w1 A R=%s aenv=none cell=yes" % R,
                "alsochk test-camp-10m-w1 A",
                "okchk test-camp-10m-r1-M1 M1 R=%s aenv=MALLOC_CONF=dirty_decay_ms:1000"
                " TORUS_X_KNOB=7 cell=yes" % R,
                "alsochk test-camp-10m-r1-M1 M1",
            ],
        )
        self.assertEqual([r[7] for r in self.rows(spec)], ["ok", "ok"])

    def test_failing_check_is_a_check_fail(self):
        f = os.path.join(self.tmp, "checks.sh")
        with open(f, "w") as fh:
            fh.write(CHECKS_SH)
        spec = minimal_spec(
            self.tmp,
            warmup=False,
            checks={"source": f, "per_cell": ["badchk", "okchk"]},
        )
        out = self.run_cells(spec, [self.M, "r1-M1:M1:120:none:"])
        self.assertIn("RUN_CELLS_RC=1", out, out)
        self.assertIn("CHECK FAIL test-camp-10m-r1-A: badchk says no", out)
        self.assertIn(
            "CHECK FAIL test-camp-10m-r1-A: campaign check badchk returned 1 (arm A)",
            out,
        )
        self.assertIn("CAMPAIGN STOP at r1-A", out)
        # every check runs (one cell shows every failure), as the standard checks
        self.assertIn(
            "okchk test-camp-10m-r1-A A",
            open(os.path.join(spec["results_root"], "checks.log")).read(),
        )
        self.assertEqual(
            [(r[0], r[7]) for r in self.rows(spec)],
            [("test-camp-10m-r1-A", "bad:check")],
        )
        self.assertFalse(self.started(spec, "test-camp-10m-r1-M1"), out)

    def test_smoke_cell_is_gated(self):
        spec = minimal_spec(self.tmp)
        out = self.run_cells(
            spec,
            ["s0:A:60:none:"],
            smoke=True,
            FAKE_SUMMARY='{"validity": {"verdict": "REJECT"}}',
        )
        self.assertIn("RUN_CELLS_RC=1", out, out)
        self.assertIn(
            "CHECK FAIL test-camp-10m-smoke-s0: warm-up gate: validity=REJECT", out
        )

    def test_death_in_gated_warmup_stops(self):
        spec = minimal_spec(self.tmp)
        out = self.run_cells(spec, [self.W, self.M], FAKE_DIE="1", FAKE_RUN_S="120")
        self.assertIn("RUN_CELLS_RC=1", out, out)
        self.assertIn("CHECK FAIL test-camp-10m-w1: warm-up gate: node death", out)
        self.assertFalse(self.started(spec, "test-camp-10m-r1-A"), out)

    def checks_spec(self, body, fns, **over):
        f = os.path.join(self.tmp, "scope-checks.sh")
        with open(f, "w") as fh:
            fh.write(body)
        return minimal_spec(
            self.tmp, warmup=False, checks={"source": f, "per_cell": fns}, **over
        )

    def test_check_sees_its_own_rc_not_campaignchecks(self):
        """Codex P2: `rc=1; guardchk() { return "$rc"; }` passed because the check read campaignchecks' rc=0
        (dynamic scoping through the subshell); it must fail the cell and stop the campaign"""
        spec = self.checks_spec(CHECKS_RC_SH, ["guardchk"])
        out = self.run_cells(spec, [self.M, "r1-M1:M1:120:none:"])
        self.assertIn("RUN_CELLS_RC=1", out, out)
        self.assertIn(
            "CHECK FAIL test-camp-10m-r1-A: campaign check guardchk returned 1 (arm A)",
            out,
        )
        self.assertIn("CAMPAIGN STOP at r1-A", out)
        self.assertEqual(
            [(r[0], r[7]) for r in self.rows(spec)],
            [("test-camp-10m-r1-A", "bad:check")],
        )
        self.assertFalse(self.started(spec, "test-camp-10m-r1-M1"), out)

    def test_check_sees_its_own_values_and_no_driver_local(self):
        """a check sees its file's top-level values under the names cell / run_spec / run_cells / campaignchecks
        use as locals (scalars, an array, an assoc array, a readonly), none of the driver locals the file never
        set, and still the driver globals and its label / arm arguments"""
        spec = self.checks_spec(CHECKS_OWN_SH, ["ownchk", "hidechk", "globchk"])
        out = self.run_cells(spec, ["r1-M1:M1:120:none:"])
        self.assertIn("RUN_CELLS_RC=0", out, out)
        self.assertNotIn("readonly variable", out)
        R = spec["results_root"]
        drv = os.path.join(self.tmp, "gate.sh")
        axe = self.lib(drv, 'echo "${AXE[M1]}"').stdout.strip()
        self.assertTrue(axe, "AXE[M1] empty")
        self.assertEqual(
            open(os.path.join(R, "checks.log")).read().splitlines(),
            [
                "ownchk test-camp-10m-r1-M1 M1",
                "hidechk test-camp-10m-r1-M1 M1",
                "globchk test-camp-10m-r1-M1 M1 R=%s aenv=MALLOC_CONF=dirty_decay_ms:1000"
                " TORUS_X_KNOB=7 axe=%s arms=A M1" % (R, axe),
            ],
        )
        self.assertEqual([r[7] for r in self.rows(spec)], ["ok"])

    def test_check_sees_names_that_are_loader_locals(self):
        """load-time mirror: loaded by the whole preflight_static, a checks file that sets f m b node tgt fns
        before after (locals of the frames it used to be sourced in) keeps its values for its checks, saw none
        of the loader's at source time, and a top-level declare -A without -g still reaches the checks"""
        spec = self.checks_spec(CHECKS_LOAD_SH, ["minechk"])
        out = self.run_cells(spec, [self.M], loader="preflight_static")
        self.assertNotIn("PREFLIGHT FAIL", out)
        self.assertIn("RUN_CELLS_RC=0", out, out)
        self.assertEqual(
            open(os.path.join(spec["results_root"], "checks.log")).read().splitlines(),
            [
                "minechk test-camp-10m-r1-A A src[f=UNSET m=UNSET b=UNSET node=UNSET tgt=UNSET"
                " fns=UNSET before=UNSET after=UNSET]"
            ],
        )
        self.assertEqual([r[7] for r in self.rows(spec)], ["ok"])


# ---------------------------------------------------------------- post-cell work: in-line vs the end pass (s131)


class PostCellClassificationTest(Base):
    """the driver's POST_INLINE / POST_ENDPASS lists against the code: every in-line step runs in cell() / run_spec;
    deferred (DEFER_SCANS=1) every end-pass step runs only in end_pass (never while a later cell could run). In-line
    (DEFER_SCANS=0) cell() runs hardscan / soakchk where the pre-end-pass driver did (InlineSequenceTest), in the
    DEFER_SCANS branch's else, and nothing else of them"""

    DEFER_IF = '    if [ "$DEFER_SCANS" = 1 ]; then\n'

    # how each listed step shows in the driver text
    INLINE_CALLS = {
        "node-death": 'if [ -e "$R/$label/node-death.txt" ]; then',
        "md5chk": "cellchecks",
        "envchk": "cellchecks",
        "marketchk": "cellchecks",
        "expectchk": "cellchecks",
        "campaignchecks": 'campaignchecks "$label" "$arm"',
        "sysctlchk": 'sysctlchk "$label" end',
        "warmgatechk": 'warmgatechk "$label" "$rc"',
        "fdsum": 'fdsum "$label"',
        "savelog": 'savelog "$label"',
    }
    ENDPASS_CALLS = {
        "cell-endpass": '"$ENDPASS" "$R/$label"',
        "hardscan": 'hardscan "$label"',
        "soakchk": 'soakchk "$label" "$dur"',
    }

    def setUp(self):
        super().setUp()
        self.drv = self.generate(self.write_spec(minimal_spec(self.tmp)))
        self.text = open(self.drv).read()

    def lists(self):
        r = self.lib(self.drv, 'echo "$POST_INLINE"; echo "$POST_ENDPASS"')
        a, b = r.stdout.splitlines()
        return a.split(), b.split()

    def test_lists_cover_the_code(self):
        inline, endpass = self.lists()
        self.assertEqual(sorted(set(inline) - {"cell_step"}), sorted(self.INLINE_CALLS))
        self.assertEqual(sorted(endpass), sorted(self.ENDPASS_CALLS))
        self.assertEqual(set(inline) & set(endpass), set())
        cell = func_text(self.text, "cell")
        spec = func_text(self.text, "run_spec")
        ep = func_text(self.text, "end_pass")
        for k, call in self.INLINE_CALLS.items():
            self.assertIn(call, cell, "in-line %s" % k)
            self.assertNotIn(call, ep, "in-line %s in the end pass" % k)
        cc = func_text(self.text, "cellchecks")
        for k in ("md5chk", "envchk", "marketchk", "expectchk"):
            self.assertIn(k + ' "$1"', cc)
        self.assertIn("cell_step ", spec)
        # cell()'s report-scan branch: deferred = END_SCANS only; else (in-line) = hardscan, soakchk
        self.assertEqual(cell.count(self.DEFER_IF), 1, cell)
        i = cell.index(self.DEFER_IF)
        j = cell.index("\n    fi\n", i) + len("\n    fi\n")
        branch, rest = cell[i:j], cell[:i] + cell[j:]
        deferred, inline_ = branch.split("\n    else", 1)
        self.assertIn('END_SCANS+=("$label:$dur")', deferred)
        self.assertNotIn("END_SCANS", rest + inline_)
        for k, call in self.ENDPASS_CALLS.items():
            self.assertIn(call, ep, "end pass %s" % k)
            self.assertNotIn(
                call, rest, "end pass %s in cell() outside its in-line branch" % k
            )
            self.assertNotIn(
                call, deferred, "end pass %s in cell()'s deferred branch" % k
            )
            self.assertEqual(
                call in inline_, k != "cell-endpass", "in-line branch: %s" % k
            )
            self.assertNotIn(call, spec, "end pass %s in run_spec" % k)
            self.assertNotIn(call, func_text(self.text, "run_cells"))
        # in-line, where the pre-end-pass driver had them: after the checks' stop, before the rc / fd-stop lines
        self.assertLess(
            cell.index('[ "$crc" = 0 ] || { CELL_FAIL=check; return 1; }'), i
        )
        self.assertLess(
            j, cell.index('[ "$rc" = 0 ] || log "CELL FAIL $label: run-cell rc=$rc')
        )

    def test_show_lists_them(self):
        plan = self.plan(self.drv)
        inline, endpass = self.lists()
        self.assertIn("POSTCELL inline %s" % " ".join(inline), plan)
        self.assertIn(
            "POSTCELL endpass %s (once, after the last cell, before campaign.done)"
            % " ".join(endpass),
            plan,
        )


class PreflightPostTest(Base):
    """preflight_post: the end pass and genesis reuse follow what run-cell.sh in tools_dir supports"""

    def dry(self, spec):
        drv = self.generate(self.write_spec(spec))
        r = subprocess.run([drv, "dry"], capture_output=True, text=True, timeout=120)
        return r.returncode, r.stdout + r.stderr

    def test_current_tools_turn_both_on(self):
        rc, out = self.dry(minimal_spec(self.tmp))
        self.assertEqual(rc, 0, out)
        self.assertIn("end_pass=1 genesis_reuse=1", out)

    def test_old_run_cell_runs_as_before(self):
        spec = minimal_spec(self.tmp)
        with open(os.path.join(spec["tools_dir"], "run-cell.sh"), "w") as f:
            f.write("#!/bin/bash\n# an older run-cell.sh without the knobs\n")
        rc, out = self.dry(spec)
        self.assertEqual(rc, 0, out)
        self.assertIn("end_pass=0 genesis_reuse=0", out)

    def test_missing_helpers_fail_preflight(self):
        for f in ("log-scans.sh", "cell-endpass.sh", "genesis_cache.py"):
            with self.subTest(f=f):
                spec = minimal_spec(self.tmp)
                p = os.path.join(spec["tools_dir"], f)
                os.rename(p, p + ".away")
                try:
                    rc, out = self.dry(spec)
                finally:
                    os.rename(p + ".away", p)
                self.assertEqual(rc, 1, out)
                self.assertIn("PREFLIGHT FAIL: %s missing" % p, out)

    def test_a_reader_of_a_deferred_output_keeps_them_in_line(self):
        for exp in (
            {"file": "val0.log.excerpt", "regex": "book mode"},
            {"file": "run.log", "regex": "ingest: bench submitted"},
        ):
            with self.subTest(exp=exp):
                rc, out = self.dry(minimal_spec(self.tmp, expect=[exp]))
                self.assertEqual(rc, 0, out)
                self.assertIn("PREFLIGHT NOTE: report scans: in-line because ", out)
                self.assertIn("end_pass=0 genesis_reuse=1", out)
        rc, out = self.dry(
            minimal_spec(
                self.tmp,
                expect=[{"file": "agreement.jsonl", "regex": "agree", "min": 0}],
            )
        )
        self.assertIn("end_pass=1", out)


class DeferredReadersTest(Base):
    """deferred_readers (Codex P2s on 0f2ca85b and bcc95fab): the report scans defer only when no in-line step
    reads anything a deferred scan writes or could write. The deferred outputs: summary.json
    .ingest.mempool_nonce_expired_evictions_per_node ([] until the end pass), run.log's ingest line (a "deferred"
    line until then), val<i>.log.excerpt (absent until then), deferred-scans.json (until then). No guessing which
    regex or script reads which field (a sample line or a keyword test missed valid readers): any expectation on
    one of those files, any checks file and any analysis.cmd keep the scans in-line. The rest of the in-line work
    reads none of them (marketchk: summary.json cell.markets; warmgatechk / cell_step: validity, agreement,
    liveness, headline, crash, binaries)."""

    PLACEHOLDER = {
        "file": "summary.json",
        "regex": '"mempool_nonce_expired_evictions_per_node": \\[\\]',
        "min": 0,
        "max": 0,
    }
    ANALYSIS = (
        "import json, sys\n"
        's = json.load(open(sys.argv[2] + "/summary.json"))\n'
        'v = s["ingest"]["mempool_nonce_expired_evictions_per_node"]\n'
        'assert v, "deferred evictions"\n'
        'json.dump({sys.argv[3]: {"evictions": sum(v)}}, open(sys.argv[1], "w"))\n'
    )

    def dry(self, spec):
        drv = self.generate(self.write_spec(spec))
        r = subprocess.run([drv, "dry"], capture_output=True, text=True, timeout=120)
        return r.returncode, r.stdout + r.stderr

    def assertInline(self, spec, why):
        rc, out = self.dry(spec)
        self.assertEqual(rc, 0, out)
        self.assertIn("end_pass=0 genesis_reuse=1", out)
        self.assertEqual(out.count("PREFLIGHT NOTE: report scans: "), 1, out)
        self.assertIn("PREFLIGHT NOTE: report scans: in-line because ", out)
        self.assertIn(why, out)
        return out

    def assertDeferred(self, spec):
        rc, out = self.dry(spec)
        self.assertEqual(rc, 0, out)
        self.assertIn("end_pass=1 genesis_reuse=1", out)
        self.assertEqual(out.count("PREFLIGHT NOTE: report scans: "), 1, out)
        self.assertIn(
            "PREFLIGHT NOTE: report scans: deferred to the campaign end pass (ingest-evictions log-excerpts;",
            out,
        )
        self.assertNotIn("in-line because", out)

    def defer_scans(self, drv):
        r = self.lib(drv, "preflight_static > /dev/null; echo DEFER_SCANS=$DEFER_SCANS")
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        return int(r.stdout.rsplit("DEFER_SCANS=", 1)[1].split()[0])

    def probe_cell(self, drv):
        """the review probes' cell: run-cell.sh's ingest and save blocks with the DEFER_SCANS preflight chose"""
        sys.path.insert(0, HERE)
        from test_cell_endpass import EquivalenceTest

        defer = self.defer_scans(drv)
        e = EquivalenceTest()
        e.setUp()
        self.addCleanup(e.doCleanups)
        cell, _ = e.run_cell_blocks(defer)
        return defer, cell

    def campaign_rc(self, drv, cell):
        r = self.lib(
            drv,
            'preflight_static > /dev/null; cell() { STARTED_LABEL=$P-$1; expectchk "$CELL" "$2" || '
            '{ CELL_FAIL=check; return 1; }; }; cell_step() { :; }; CELLS=("w1:A:120:none:"); '
            "run_cells; echo CAMPAIGN_RC=$?",
            {"CELL": cell},
        )
        self.assertNotIn("CHECK FAIL", r.stdout, r.stdout + r.stderr)
        return r

    # ---- no reader: the scans defer
    def test_no_reader_defers(self):
        self.assertDeferred(minimal_spec(self.tmp))

    def test_expectation_on_another_file_defers(self):
        self.assertDeferred(
            minimal_spec(
                self.tmp,
                expect=[{"file": "agreement.jsonl", "regex": "agree", "min": 0}],
            )
        )

    # ---- expectations
    def test_summary_json_expectation_keeps_them_in_line(self):
        self.assertInline(
            minimal_spec(self.tmp, expect=[self.PLACEHOLDER]), "on summary.json"
        )

    def test_per_arm_summary_json_expectation_keeps_them_in_line(self):
        s = minimal_spec(self.tmp)
        s["arms"][1]["expect"] = [
            {"file": "summary.json", "regex": '"benchmark_accepted": true', "min": 1}
        ]
        self.assertInline(s, "arm M1")

    def test_excerpt_and_marker_expectations_keep_them_in_line(self):
        for f in ("val0.log.excerpt", "deferred-scans.json"):
            with self.subTest(f=f):
                self.assertInline(
                    minimal_spec(
                        self.tmp, expect=[{"file": f, "regex": "x", "min": 0}]
                    ),
                    "on %s" % f,
                )

    def test_any_run_log_expectation_keeps_them_in_line(self):
        # Codex P2 on bcc95fab: 'per node: 435 ' matched no sample ingest line, so the scans deferred and the
        # cell failed before the end pass wrote the matching line. Any run.log regex may read the ingest line
        for rx in (
            "per node: 435 ",
            "bench submitted=[0-9]+ actions",
            "oracle feed pid [0-9]+;",
            "genesis built",
        ):
            with self.subTest(rx=rx):
                self.assertInline(
                    minimal_spec(self.tmp, expect=[{"file": "run.log", "regex": rx}]),
                    "on run.log",
                )

    def test_run_log_probe_does_not_stop_the_campaign(self):
        """the bcc95fab review's runlog probe end to end: in-line scans, the cell's run.log has the expected
        ingest line when expectchk reads it, the campaign goes on"""
        drv = self.generate(
            self.write_spec(
                minimal_spec(
                    self.tmp,
                    expect=[{"file": "run.log", "regex": "per node: 435 ", "min": 1}],
                )
            )
        )
        defer, cell = self.probe_cell(drv)
        self.assertEqual(defer, 0)
        r = self.campaign_rc(drv, cell)
        self.assertIn("CAMPAIGN_RC=0", r.stdout, r.stdout + r.stderr)

    def test_placeholder_expectation_does_not_stop_the_campaign(self):
        """the 0f2ca85b review's probe: an expectation that the eviction counts are filled in (not the []
        placeholder) passes on the cell the campaign runs, and the campaign goes on"""
        drv = self.generate(
            self.write_spec(minimal_spec(self.tmp, expect=[self.PLACEHOLDER]))
        )
        defer, cell = self.probe_cell(drv)
        self.assertEqual(defer, 0)
        r = self.campaign_rc(drv, cell)
        self.assertIn("CAMPAIGN_RC=0", r.stdout, r.stdout + r.stderr)

    # ---- checks file
    def test_any_checks_file_keeps_them_in_line(self):
        f = os.path.join(self.tmp, "checks.sh")
        for body in (
            "okchk() { jq -e '.x.mempool_nonce_expired' \"$R/$1/summary.json\" > /dev/null; }\n",
            "okchk() { f=summary; jq -e '.ingest' \"$R/$1/$f.json\" > /dev/null; }\n",
            "okchk() { return 0; }\n",
        ):
            with self.subTest(body=body):
                with open(f, "w") as fh:
                    fh.write(body)
                self.assertInline(
                    minimal_spec(self.tmp, checks={"source": f, "per_cell": ["okchk"]}),
                    "checks file %s" % f,
                )

    # ---- analysis.cmd
    def test_any_analysis_cmd_keeps_them_in_line(self):
        a = os.path.join(self.tmp, "a-cells.py")
        with open(a, "w") as fh:
            fh.write(
                'import json, sys\ns = json.load(open(sys.argv[1]))\nh = s["headline"]\n'
            )
        for cmd in (
            "python3 -I %s {out} {prefix} {tag}:{arm}" % a,
            'python3 -I "%s" {out} {cell} {tag}'
            % a,  # Codex P2 on bcc95fab: quotes hid the script
            "python3 -I -c 'import sys' {out}",  # no script file at all
        ):
            with self.subTest(cmd=cmd):
                self.assertInline(
                    minimal_spec(self.tmp, analysis={"cmd": cmd}), "analysis.cmd"
                )

    def test_quoted_analysis_script_is_noted_like_cell_step(self):
        """preflight's advisory script note splits analysis.cmd as cell_step runs it (shlex.split): a quoted
        path is a path. Advisory only (s131): a missing script or a command that does not split is a
        PREFLIGHT NOTE, never a FAIL"""
        a = os.path.join(self.tmp, "with space", "a.py")
        for cmd in ('python3 -I "%s" {out}' % a, "python3 -I '%s' {out}" % a):
            with self.subTest(cmd=cmd):
                rc, out = self.dry(minimal_spec(self.tmp, analysis={"cmd": cmd}))
                self.assertEqual(rc, 0, out)
                self.assertIn(NOTE_MISSING % a, out)
                self.assertNotIn("PREFLIGHT FAIL", out)
        os.makedirs(os.path.dirname(a))
        with open(a, "w") as fh:
            fh.write("import sys\n")
        rc, out = self.dry(
            minimal_spec(self.tmp, analysis={"cmd": 'python3 -I "%s" {out}' % a})
        )
        self.assertEqual(rc, 0, out)
        self.assertNotIn("PREFLIGHT NOTE: analysis", out)
        rc, out = self.dry(
            minimal_spec(self.tmp, analysis={"cmd": 'python3 -I "%s {out}' % a})
        )
        self.assertEqual(rc, 0, out)
        self.assertIn(NOTE_NOSPLIT, out)

    def test_quoted_analysis_probe_reads_filled_counts(self):
        """the bcc95fab review's analysis probe end to end: a quoted script path keeps the scans in-line, so the
        analysis cell_step runs right after the cell reads filled eviction counts (status ok)"""
        sys.path.insert(0, HERE)
        import cell_step

        script = os.path.join(self.tmp, "analysis.py")
        with open(script, "w") as fh:
            fh.write(self.ANALYSIS)
        cmd = 'python3 -I "%s" {out} {cell} {tag}' % script
        drv = self.generate(
            self.write_spec(
                minimal_spec(self.tmp, analysis={"cmd": cmd, "metric": "evictions"})
            )
        )
        defer, cell = self.probe_cell(drv)
        self.assertEqual(defer, 0)
        step = os.path.join(self.tmp, "step")
        os.makedirs(step, exist_ok=True)
        a = types.SimpleNamespace(
            label=os.path.basename(cell),  # cell_step: the cell dir ends with the tag
            tag=os.path.basename(cell).rsplit("-", 1)[1],
            arm="A",
            run_dir=self.tmp,
            analysis_cmd=cmd,
            analysis_timeout=30,
            analysis_key="{tag}",
            analysis_metric="evictions",
        )
        status, _row, evidence = cell_step.run_analysis(a, cell, step)
        self.assertEqual(status, "ok", evidence)

    def test_old_run_cell_logs_the_decision(self):
        spec = minimal_spec(self.tmp)
        with open(os.path.join(spec["tools_dir"], "run-cell.sh"), "w") as f:
            f.write("#!/bin/bash\n# an older run-cell.sh without the knobs\n")
        rc, out = self.dry(spec)
        self.assertEqual(rc, 0, out)
        self.assertIn("end_pass=0 genesis_reuse=0", out)
        self.assertIn("PREFLIGHT NOTE: report scans: in-line because ", out)


class AnalysisPlaceholderPreflightTest(Base):
    """preflight's missing-script check sees analysis.cmd as cell_step runs it (Codex P2 on 9f30ce12: a quoted
    "{run}/analysis.py" failed preflight although the script existed after expansion). The frozen tools copy's
    cell_step.py fills the words (analysis-words: run_analysis's own placeholders and fill) for every cell the
    campaign can run (each planned cell and its -rerun; dry smoke: the smoke cell), and a */*.py or */*.sh word
    that is no file is a PREFLIGHT NOTE naming the filled path (advisory, s131: preflight never fails on it). An
    older cell_step.py without analysis-words: a word with a placeholder is not checked (one PREFLIGHT NOTE names
    it), the other words still are."""

    SCRIPTS = {  # each placeholder cell_step fills -> a script path built from it
        "run": "{run}/an.py",
        "prefix": "{prefix}an.py",
        "cell": "{cell}.an.py",
        "label": "{run}/an-{label}.py",
        "tag": "{run}/an-{tag}.sh",
        "arm": "{run}/an-{arm}.py",
        "out": "{out}.py",
    }

    def dry(self, spec, smoke=False):
        drv = self.generate(self.write_spec(spec))
        r = subprocess.run(
            [drv, "dry"] + (["smoke"] if smoke else []),
            capture_output=True,
            text=True,
            timeout=120,
        )
        return r.returncode, r.stdout + r.stderr

    def filled(self, spec, tpl, smoke=False):
        """tpl filled for each cell cell_step can run, in order (the cells, then their reruns), computed here
        from the plan and cell_step's documented placeholders, not by cell_step"""
        drv = self.generate(self.write_spec(spec, "plan.json"), "plan.sh")
        cells = [
            tuple(l.split()[1:3])
            for l in self.plan(drv, smoke)
            if l.startswith("CELL ")
        ]
        if not smoke:
            cells += [(t + "-rerun", a) for t, a in cells]
        run = spec["run_dir"] + ("/smoke" if smoke else "")
        p = spec["label_prefix"] + ("-smoke" if smoke else "")
        out = []
        for tag, arm in cells:
            label = "%s-%s" % (p, tag)
            cell = os.path.normpath(os.path.join(spec["results_root"], label))
            ph = {
                "out": os.path.join(run, "cell-step", label + ".analysis.json"),
                "prefix": cell[: -len(tag)],
                "tag": tag,
                "arm": arm,
                "label": label,
                "cell": cell,
                "run": os.path.normpath(run),
            }
            s = tpl
            for k, v in ph.items():
                s = s.replace("{%s}" % k, v)
            if s not in out:
                out.append(s)
        return out

    def touch(self, paths):
        for f in paths:
            os.makedirs(os.path.dirname(f), exist_ok=True)
            with open(f, "w") as fh:
                fh.write("import sys\n")

    def unlink(self, paths):
        for f in paths:
            if os.path.exists(f):
                os.remove(f)

    def assertMissing(self, out, paths):
        self.assertNotIn("PREFLIGHT NOTE: analysis script {", out)
        self.assertNotIn("PREFLIGHT FAIL", out)
        self.assertTrue(
            any(NOTE_MISSING % f in out for f in paths),
            "no PREFLIGHT NOTE names one of %s:\n%s" % (paths, out),
        )

    def test_every_placeholder_is_filled_before_the_check(self):
        for ph, tpl in self.SCRIPTS.items():
            for word in ('"%s"' % tpl, tpl, "'%s'" % tpl):
                cmd = "python3 -I %s {out} {prefix} {tag}:{arm}" % word
                spec = minimal_spec(self.tmp, analysis={"cmd": cmd})
                paths = self.filled(spec, tpl)
                with self.subTest(placeholder=ph, cmd=cmd):
                    self.assertTrue(paths and all("{" not in f for f in paths), paths)
                    rc, out = self.dry(spec)
                    self.assertEqual(rc, 0, out)
                    self.assertMissing(out, paths)
                    if len(paths) > 1:  # per cell: only the last cell's rerun lacks it
                        self.touch(paths[:-1])
                        rc, out = self.dry(spec)
                        self.assertEqual(rc, 0, out)
                        self.assertIn(NOTE_MISSING % paths[-1], out)
                        for f in paths[:-1]:
                            self.assertNotIn(NOTE_MISSING % f, out)
                    self.touch(paths)
                    rc, out = self.dry(spec)
                    self.assertEqual(rc, 0, out)
                    self.assertIn("PREFLIGHT OK (dry", out)
                    self.assertNotIn("PREFLIGHT NOTE: analysis", out)
                    self.unlink(paths)

    def test_smoke_fills_the_smoke_run(self):
        """dry smoke: RUN/smoke, P-smoke, the smoke cell, no rerun (as the smoke run's cell_step sees them)"""
        for tpl in (self.SCRIPTS["run"], self.SCRIPTS["label"], self.SCRIPTS["cell"]):
            spec = minimal_spec(
                self.tmp, analysis={"cmd": 'python3 -I "%s" {out}' % tpl}
            )
            full, smoke = self.filled(spec, tpl), self.filled(spec, tpl, smoke=True)
            with self.subTest(tpl=tpl):
                self.assertEqual(len(smoke), 1, smoke)
                self.assertNotIn(smoke[0], full)
                self.touch(full)
                rc, out = self.dry(spec, smoke=True)
                self.assertEqual(rc, 0, out)
                self.assertIn(NOTE_MISSING % smoke[0], out)
                self.touch(smoke)
                rc, out = self.dry(spec, smoke=True)
                self.assertEqual(rc, 0, out)
                self.assertNotIn("PREFLIGHT NOTE: analysis", out)
                self.unlink(full + smoke)

    def test_review_probe_shape(self):
        """the 9f30ce12 review probe: python3 -I "{run}/analysis.py" ... with the script in the run dir"""
        spec = minimal_spec(
            self.tmp,
            analysis={"cmd": 'python3 -I "{run}/analysis.py" {out} {cell} {tag}'},
        )
        self.touch([os.path.join(spec["run_dir"], "analysis.py")])
        rc, out = self.dry(spec)
        self.assertEqual(rc, 0, out)

    def archived_cmds(self):
        """every analysis command shape in the archived campaigns and specs: CELL_STEP_ANALYSIS_CMD="..." in the
        hand-written drivers, analysis.cmd in the JSON specs"""
        arch = os.path.join(
            os.path.dirname(os.path.dirname(HERE)), "docs", "perf", "bench-archive"
        )
        cmds, files = [], []
        for pat in ("*.sh", "*.py", "*.json"):
            files += glob.glob(os.path.join(FIX, pat))
            files += glob.glob(os.path.join(arch, "**", pat), recursive=True)
        files.append(EXAMPLE)
        for f in sorted(set(files)):
            txt = open(f, errors="replace").read()
            found = re.findall(r'CELL_STEP_ANALYSIS_CMD="([^"]*)"', txt)
            if f.endswith(".json"):
                try:
                    an = json.loads(txt).get("analysis") or {}
                except (ValueError, AttributeError):
                    an = {}
                found += [an["cmd"]] if an.get("cmd") else []
            cmds += [(f, c) for c in found]
        return cmds

    def test_archived_command_shapes(self):
        """the archived shapes (python3 -I $ANALYST/a-cells.py {out} {prefix} {tag}:{arm}, the example spec's
        absolute path) pass with the script, and pass with a NOTE naming it without it (advisory, s131; before,
        a FAIL: the pre-9f30ce12 bash word split checked their unquoted absolute script word the same way)"""
        cmds = self.archived_cmds()
        self.assertGreaterEqual(len(cmds), 5, cmds)
        analyst = os.path.join(self.tmp, "analyst")
        seen = set()
        for f, raw in cmds:
            cmd = raw.replace("$ANALYST", analyst)
            for w in shlex.split(cmd):
                if re.match(r".*/.*\.(py|sh)$", w):
                    cmd = cmd.replace(w, os.path.join(analyst, os.path.basename(w)))
            if cmd in seen:
                continue
            seen.add(cmd)
            scripts = [
                w for w in cmd.split() if re.match(r".*/.*\.(py|sh)$", w)
            ]  # the pre-9f30ce12 check: bash word split, */*.py|*/*.sh
            self.assertTrue(scripts, (f, raw))
            spec = minimal_spec(self.tmp, analysis={"cmd": cmd})
            with self.subTest(source=f, cmd=raw):
                rc, out = self.dry(spec)
                self.assertEqual(rc, 0, out)
                self.assertIn(NOTE_MISSING % scripts[0], out)
                self.touch(scripts)
                rc, out = self.dry(spec)
                self.assertEqual(rc, 0, out)
                self.assertNotIn("PREFLIGHT NOTE: analysis", out)
                self.unlink(scripts)

    def test_words_are_cell_steps_argv(self):
        """analysis-words prints the argv run_analysis runs (its own placeholders and fill): compared with the
        command line run_analysis logs, every placeholder, a quoted word with a space"""
        sys.path.insert(0, HERE)
        import cell_step

        run = os.path.join(self.tmp, "run") + "/"
        cell = os.path.join(self.tmp, "res", "pfx-r1-A")
        step = os.path.join(run, "cell-step")
        os.makedirs(step)
        cmd = (
            "python3 -I -c 'import sys' {out} {prefix} {tag}:{arm} {label} {cell} {run} "
            "'{run}/x y.py' plain"
        )
        a = types.SimpleNamespace(
            label="pfx-r1-A",
            tag="r1-A",
            arm="A",
            run_dir=run,
            analysis_cmd=cmd,
            analysis_timeout=30,
            analysis_key="{tag}",
            analysis_metric="",
        )
        cell_step.run_analysis(a, cell, step)
        with open(os.path.join(step, "pfx-r1-A.analysis.log")) as fh:
            logged = shlex.split(fh.readline()[2:])
        r = subprocess.run(
            [
                sys.executable,
                "-I",
                os.path.join(HERE, "cell_step.py"),
                "analysis-words",
                "--run-dir",
                run,
                "--analysis-cmd",
                cmd,
                "--cell",
                cell,
                "pfx-r1-A",
                "r1-A",
                "A",
            ],
            capture_output=True,
            text=True,
            timeout=60,
        )
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(r.stdout.split("\0")[:-1], logged)
        r = subprocess.run(
            [
                sys.executable,
                "-I",
                os.path.join(HERE, "cell_step.py"),
                "analysis-words",
                "--run-dir",
                run,
                "--analysis-cmd",
                "python3 'x",
                "--cell",
                cell,
                "pfx-r1-A",
                "r1-A",
                "A",
            ],
            capture_output=True,
            text=True,
            timeout=60,
        )
        self.assertEqual((r.returncode, r.stdout), (1, ""), r.stderr)
        self.assertIn("cannot parse the analysis command", r.stderr)

    def test_older_cell_step_skips_placeholder_words(self):
        """a frozen cell_step.py without analysis-words: a word with a placeholder is not checked (PREFLIGHT
        NOTE), a plain missing script is still noted, a command that does not split too (never a FAIL)"""
        spec = minimal_spec(self.tmp)
        with open(os.path.join(spec["tools_dir"], "cell_step.py"), "w") as fh:
            fh.write("# an older cell_step.py: record and pending-reruns only\n")
        cmd = 'python3 -I "{run}/analysis.py" {out} {tag}'
        rc, out = self.dry(minimal_spec(self.tmp, analysis={"cmd": cmd}))
        self.assertEqual(rc, 0, out)
        self.assertIn(
            "PREFLIGHT NOTE: analysis script {run}/analysis.py not checked", out
        )
        a = os.path.join(self.tmp, "nope", "a.py")
        rc, out = self.dry(
            minimal_spec(self.tmp, analysis={"cmd": "python3 -I %s {out}" % a})
        )
        self.assertEqual(rc, 0, out)
        self.assertIn(NOTE_MISSING % a, out)
        rc, out = self.dry(
            minimal_spec(self.tmp, analysis={"cmd": "python3 -I '%s {out}" % a})
        )
        self.assertEqual(rc, 0, out)
        self.assertIn(NOTE_NOSPLIT, out)


class AnalysisShellStringPreflightTest(Base):
    """Codex P2 on c2b72397: preflight took a `bash -c` command string ending in .py/.sh for a script path and
    failed a valid spec. Owner decision s131: the script check is advisory (a NOTE, never a FAIL), preflight
    accepts every spec the pre-0f2ca85b driver accepted, and a -c command string (bash -c, bash -lc, python3
    -c) is skipped entirely (no NOTE); the operands after it are still noted"""

    def dry(self, spec):
        drv = self.generate(self.write_spec(spec))
        r = subprocess.run([drv, "dry"], capture_output=True, text=True, timeout=120)
        return r.returncode, r.stdout + r.stderr

    def script(self):
        p = os.path.join(self.tmp, "analysis.py")
        with open(p, "w") as fh:
            fh.write(
                "import json, os\n"
                'json.dump({os.environ["REPORT_TAG"]: {"metric": 1}}, open(os.environ["REPORT_JSON"], "w"))\n'
            )
        return p

    def test_review_probe(self):
        """the c2b72397 review probe: bash -c '...; exec python3 -I /path/analysis.py' passes preflight_static
        (rc 0, no NOTE, as the pre-0f2ca85b driver) and cell_step runs the analysis (status ok)"""
        sys.path.insert(0, HERE)
        import cell_step

        spec = minimal_spec(self.tmp, samplers=False)
        run = spec["run_dir"]
        os.makedirs(run)
        script = self.script()
        cmd = (
            "bash -c 'export REPORT_JSON={out} REPORT_TAG={tag}; exec python3 -I %s'"
            % script
        )
        spec["analysis"] = {"cmd": cmd, "metric": "metric"}
        drv = self.generate(self.write_spec(spec))
        r = self.lib(drv, "preflight_static")
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertNotIn("PREFLIGHT NOTE: analysis", r.stdout)
        old = subprocess.run(
            [
                "git",
                "-C",
                HERE,
                "show",
                "0f2ca85b^:tools/matched-bench/campaign/driver-body.sh",
            ],
            capture_output=True,
            text=True,
        )
        if (
            old.returncode == 0
        ):  # parity with the pre-0f2ca85b driver (skipped outside a full clone)
            text = open(drv).read()
            olddrv = os.path.join(self.tmp, "old-driver.sh")
            with open(olddrv, "w") as fh:
                fh.write(text[: text.index("# ---- driver body (")] + old.stdout)
            r = self.lib(olddrv, "preflight_static")
            self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        cell = os.path.join(self.tmp, "cell-0")
        os.makedirs(cell)
        step = os.path.join(run, "cell-step")
        os.makedirs(step)
        a = types.SimpleNamespace(
            label="cell-0",
            tag="0",
            arm="A",
            run_dir=run,
            analysis_cmd=cmd,
            analysis_timeout=30,
            analysis_key="{tag}",
            analysis_metric="metric",
        )
        status, _row, evidence = cell_step.run_analysis(a, cell, step)
        self.assertEqual(status, "ok", evidence)

    def test_command_strings_are_skipped(self):
        """a -c command string is no script path, whatever it ends with and whether its script exists"""
        missing = os.path.join(self.tmp, "nope", "a.py")
        for cmd in (
            "bash -c 'exec python3 -I %s {out}'" % missing,
            "bash -lc 'exec python3 -I %s {out}'" % missing,
            "sh -ec 'cd {run} && ./%s'" % "x/run.sh",
            "python3 -I -c 'import runpy; runpy.run_path(\"%s\")' {out}" % missing,
        ):
            with self.subTest(cmd=cmd):
                rc, out = self.dry(minimal_spec(self.tmp, analysis={"cmd": cmd}))
                self.assertEqual(rc, 0, out)
                self.assertNotIn("PREFLIGHT NOTE: analysis", out)
                self.assertNotIn("PREFLIGHT FAIL", out)

    def test_operands_after_a_command_string_are_noted(self):
        """bash -c 'python3 -I "$0" "$@"' /path/a.py {out}: the operand is a script path, noted when missing"""
        missing = os.path.join(self.tmp, "nope", "a.py")
        cmd = 'bash -c \'exec python3 -I "$0" "$@"\' %s {out}' % missing
        rc, out = self.dry(minimal_spec(self.tmp, analysis={"cmd": cmd}))
        self.assertEqual(rc, 0, out)
        self.assertIn(NOTE_MISSING % missing, out)
        self.assertNotIn("PREFLIGHT FAIL", out)
        self.assertEqual(out.count("PREFLIGHT NOTE: analysis"), 1, out)

    def test_missing_script_is_a_note(self):
        missing = os.path.join(self.tmp, "nope", "a.py")
        rc, out = self.dry(
            minimal_spec(self.tmp, analysis={"cmd": "python3 -I %s {out}" % missing})
        )
        self.assertEqual(rc, 0, out)
        self.assertIn(NOTE_MISSING % missing, out)
        self.assertIn("PREFLIGHT OK (dry", out)


class EndPassRunTest(FakeHostBase):
    """run_campaign for real against the fakes (as CellRunTest; cell-endpass.sh is the real one in the frozen
    tools copy): the end pass runs once, after the last cell and before campaign.done, also when the campaign
    stops"""

    def campaign(self, spec, cells, **env):
        drv = self.generate(self.write_spec(spec, "ep.json"), "ep.sh")
        os.makedirs(spec["run_dir"], exist_ok=True)
        script = "\n".join(
            [
                "DRIVER_LIB_ONLY=1; . %s || exit 99" % shlex.quote(drv),
                "HARNESS=%s" % shlex.quote(os.path.join(self.bin, "run-cell.sh")),
                "quiet() { return 0; }",
                "CELLS=(%s)" % " ".join(shlex.quote(c) for c in cells),
                '. "$TOOLS/cell-step.sh" || exit 98',
                "export CELL_STEP_LOG=$RUN/campaign.log",
                "sysctl_pin",
                "DEFER_SCANS=1; GENESIS_REUSE=1",
                # main's trap: campaign.done when the driver exits
                """trap 'rc=$?; echo "DONE-MARKER exit=$rc"; echo "exit=$rc" > "$RUN/campaign.done"' EXIT""",
                "run_campaign",
            ]
        )
        e = dict(os.environ)
        e.update(
            self.env(
                DRIVER_DETACH=os.path.join(self.bin, "detach.sh"),
                FAKE_PROC=self.proc,
                FAKE_RUN_S="4",
            )
        )
        e.update(env)
        out = os.path.join(self.tmp, "ep.out")
        with open(out, "w") as fo:
            r = subprocess.run(
                ["bash", "-c", script],
                stdout=fo,
                stderr=subprocess.STDOUT,
                env=e,
                timeout=600,
            )
        return r.returncode, open(out).read()

    def test_end_pass_once_after_the_last_cell_before_done(self):
        spec = minimal_spec(self.tmp)
        cells = ["r1-A:A:120:none:", "r1-M1:M1:120:none:", "r2-A:A:120:none:"]
        rc, out = self.campaign(spec, cells)
        self.assertEqual(rc, 0, out)
        labels = ["test-camp-10m-" + c.split(":")[0] for c in cells]
        last_end = max(out.index("] END %s rc=0" % lb) for lb in labels)
        self.assertEqual(out.count("END PASS start: 3 cells"), 1, out)
        start = out.index("END PASS start")
        self.assertGreater(
            start, last_end, "the end pass ran while cells were still running"
        )
        for lb in labels:
            self.assertEqual(out.count("END PASS OK %s:" % lb), 1, out)
            self.assertGreater(out.index("END PASS OK %s:" % lb), last_end)
            self.assertGreater(out.index("CHECK OK %s hardscan done" % lb), last_end)
            cell = os.path.join(spec["results_root"], lb)
            self.assertFalse(os.path.exists(os.path.join(cell, "deferred-scans.json")))
            self.assertTrue(os.path.exists(os.path.join(cell, "val0.log.excerpt")))
            runlog = open(os.path.join(cell, "run.log")).read()
            self.assertIn(
                "harness env DEFER_REPORT_SCANS=1 GENESIS_CACHE_DIR=%s/genesis-cache"
                % spec["run_dir"],
                runlog,
            )
            self.assertIn(
                "ingest: bench submitted=5 actions; mempool nonce-expired evictions per node: 0 0 0 ",
                runlog,
            )
        self.assertLess(out.index("END PASS done"), out.index("CAMPAIGN END"))
        self.assertLess(out.index("CAMPAIGN END"), out.index("DONE-MARKER exit=0"))
        self.assertEqual(
            open(os.path.join(spec["run_dir"], "campaign.done")).read(), "exit=0\n"
        )

    def test_end_pass_runs_on_a_campaign_stop(self):
        """a failed warm-up gate stops the campaign: the end pass still completes the started cell, before
        campaign.done; no hardscan for it (as before: a cell that fails its checks never reached it)"""
        spec = minimal_spec(self.tmp)
        rc, out = self.campaign(
            spec,
            ["w1:A:120:none:", "r1-A:A:120:none:"],
            FAKE_SUMMARY='{"validity": {"verdict": "REJECT"}}',
        )
        self.assertEqual(rc, 1, out)
        self.assertIn("CAMPAIGN STOP at w1", out)
        self.assertIn("END PASS OK test-camp-10m-w1:", out)
        self.assertNotIn("hardscan done", out)
        self.assertNotIn("CAMPAIGN END", out)
        self.assertLess(out.index("END PASS done"), out.index("DONE-MARKER exit=1"))


class DriverOwnedHarnessVarsTest(FakeHostBase):
    """Codex round 6 (P2 on bb2a727f): an arm's harness env DEFER_REPORT_SCANS=1 came after the driver's own on
    run-cell.sh's env line, so a campaign whose preflight chose in-line scans (a run.log expectation) ran its cells
    deferred and the expectation stopped the campaign. Rule: the driver's preflight decision is the only source of
    run-cell.sh's DEFER_REPORT_SCANS (and GENESIS_REUSE of its GENESIS_CACHE_DIR): make-driver.py refuses both in
    every env object of the spec, and the driver sets both after every override (a hand-edited AHENV, an inherited
    environment), in both directions"""

    INGEST_RE = "ingest: bench submitted=5 actions; mempool nonce-expired evictions per node: 0 0 0"

    def campaign(self, spec, ahenv, **env):
        """Codex's probe: the generated driver with AHENV[A] edited to AHENV (what an older generator or a hand
        edit leaves), run-cell.sh = the fake harness with run-cell.sh's real report-scan blocks; preflight_static,
        then run_campaign of one smoke cell r1-A"""
        from test_cell_endpass import block

        body = FAKE_HARNESS.replace("@GUARD@", run_cell_guard())
        i = body.index('for i in 0 1 2; do echo "2026-10-15')
        j = body.index("stopall\nexit", i)
        body = (
            body[:i]
            + 'RUN_DIR=$DATA_ROOT/run\n. "$TOOLS_DIR/log-scans.sh"\n'
            + "echo 'Submitted (load-gen accepted): 5' > \"$OUT/bench.log\"\n"
            + 'echo "harness env DEFER_REPORT_SCANS=${DEFER_REPORT_SCANS:-unset}'
            + ' GENESIS_CACHE_DIR=${GENESIS_CACHE_DIR-unset}" >> "$OUT/run.log"\n'
            + block("ingest")
            + "\n"
            + block("save")
            + "\n"
            + body[j:]
        )
        self.write_fake("scan-run-cell.sh", body)
        drv = self.generate(self.write_spec(spec, "own.json"), "own.sh")
        text = open(drv).read()
        text, n = re.subn(
            r"^AHENV\[A\]=.*$", "AHENV[A]=%s" % shlex.quote(ahenv), text, flags=re.M
        )
        self.assertEqual(n, 1, "the generated driver has no AHENV[A] line")
        with open(drv, "w") as f:
            f.write(text)
        os.makedirs(spec["run_dir"])
        script = "\n".join(
            [
                "DRIVER_LIB_ONLY=1; . %s || exit 99" % shlex.quote(drv),
                "preflight_static || exit 90",
                "echo PREFLIGHT_DEFER_SCANS=$DEFER_SCANS GENESIS_REUSE=$GENESIS_REUSE",
                "HARNESS=%s" % shlex.quote(os.path.join(self.bin, "scan-run-cell.sh")),
                "quiet() { return 0; }",
                '. "$TOOLS/cell-step.sh" || exit 98',
                "export CELL_STEP_LOG=$RUN/campaign.log",
                "sysctl_pin",
                'CELLS=("r1-A:A:120:none:"); SMOKE=1',
                "run_campaign",
            ]
        )
        e = {
            k: v
            for k, v in os.environ.items()
            if k not in ("DEFER_REPORT_SCANS", "GENESIS_CACHE_DIR")
        }
        e.update(
            self.env(
                DRIVER_DETACH=os.path.join(self.bin, "detach.sh"),
                FAKE_PROC=self.proc,
                FAKE_RUN_S="4",
            )
        )
        e.update(env)
        log = os.path.join(spec["run_dir"], "campaign.log")
        with open(log, "w") as fo:
            r = subprocess.run(
                ["bash", "-c", script],
                stdout=fo,
                stderr=subprocess.STDOUT,
                env=e,
                timeout=600,
            )
        out = open(log).read()
        runlog = open(
            os.path.join(spec["results_root"], "test-camp-10m-r1-A", "run.log")
        ).read()
        return r.returncode, out, runlog

    def test_codex_probe_inline_preflight_wins_over_arm_and_inherited_env(self):
        """the round 6 probe (/tmp/review-bb2a-arm-override.py, NEW): preflight in-line (a run.log expectation),
        the arm's harness env and the launching environment both say DEFER_REPORT_SCANS=1 and GENESIS_CACHE_DIR=
        empty: the cell runs with DEFER_REPORT_SCANS=0 and the driver's cache dir, the expectation holds, no end
        pass"""
        spec = minimal_spec(
            self.tmp,
            samplers=False,
            expect=[{"file": "run.log", "regex": self.INGEST_RE, "min": 1}],
        )
        rc, out, runlog = self.campaign(
            spec,
            "DEFER_REPORT_SCANS=1 GENESIS_CACHE_DIR=",
            DEFER_REPORT_SCANS="1",
            GENESIS_CACHE_DIR="/tmp/elsewhere",
        )
        self.assertIn("PREFLIGHT_DEFER_SCANS=0 GENESIS_REUSE=1", out)
        self.assertEqual(rc, 0, out)
        self.assertNotIn("CHECK FAIL", out)
        self.assertNotIn("CAMPAIGN STOP", out)
        self.assertNotIn("END PASS", out)
        self.assertIn(
            "harness env DEFER_REPORT_SCANS=0 GENESIS_CACHE_DIR=%s/genesis-cache\n"
            % spec["run_dir"],
            runlog,
        )
        self.assertIn(self.INGEST_RE, runlog)

    def test_deferred_preflight_wins_over_arm_env(self):
        """the other direction: preflight deferred (no reader), the arm's harness env says DEFER_REPORT_SCANS=0:
        the cell runs deferred and the end pass completes it"""
        spec = minimal_spec(self.tmp, samplers=False)
        rc, out, runlog = self.campaign(spec, "DEFER_REPORT_SCANS=0")
        self.assertIn("PREFLIGHT_DEFER_SCANS=1 GENESIS_REUSE=1", out)
        self.assertEqual(rc, 0, out)
        self.assertIn("harness env DEFER_REPORT_SCANS=1 GENESIS_CACHE_DIR=", runlog)
        self.assertIn("ingest scan deferred to the campaign end pass", runlog)
        self.assertIn("END PASS OK test-camp-10m-r1-A:", out)

    def test_spec_may_not_set_driver_owned_vars(self):
        """make-driver.py refuses DEFER_REPORT_SCANS and GENESIS_CACHE_DIR in harness_env (spec-wide, per arm) and
        in node_env / an arm's env (run-cell.sh's EXTRA_ENV, exported mid-run: it would overwrite run-cell.sh's
        DEFER_REPORT_SCANS before the scans read it)"""
        for name, val in (
            ("DEFER_REPORT_SCANS", "0"),
            ("DEFER_REPORT_SCANS", "1"),
            ("GENESIS_CACHE_DIR", ""),
            ("GENESIS_CACHE_DIR", "/x"),
        ):
            with self.subTest(name=name, val=val):
                specs = {
                    "harness_env": minimal_spec(self.tmp, harness_env={name: val}),
                    "node_env": minimal_spec(self.tmp, node_env={name: val}),
                }
                s = minimal_spec(self.tmp)
                s["arms"][1]["harness_env"] = {name: val}
                specs["arm M1.harness_env"] = s
                s = minimal_spec(self.tmp)
                s["arms"][1]["env"][name] = val
                specs["arm M1.env"] = s
                for where, spec in specs.items():
                    with self.assertRaises(self.gen.SpecError) as cm:
                        self.gen.validate(spec)
                    msg = str(cm.exception)
                    self.assertIn(
                        "%s: %s is set by the driver from its preflight"
                        % (where, name),
                        msg,
                    )
                # and the generator CLI refuses it (no driver written)
                p = self.write_spec(specs["harness_env"], "bad.json")
                out = os.path.join(self.tmp, "bad.sh")
                r = subprocess.run(
                    [sys.executable, "-I", GEN, p, "-o", out],
                    capture_output=True,
                    text=True,
                )
                self.assertEqual(r.returncode, 2, r.stdout + r.stderr)
                self.assertIn("SPEC ERROR", r.stderr)
                self.assertFalse(os.path.exists(out))


# ---------------------------------------------------------------- in-line = the pre-end-pass driver (owner rule)

# the driver body before the end pass (0f2ca85b: matched-bench end pass + genesis reuse). Owner rule (s131, after
# Codex round 4 on d6647299: an in-line campaign ran hardscan after cell_step, so an analysis that rejects a cell's
# HARD FINDING lines recorded ok): with the report scans in-line (DEFER_SCANS=0) every cell runs EXACTLY this
# body's steps, in its order, with the same inputs, and the end pass does nothing; only DEFER_SCANS=1 may differ
OLD_BODY_REV = "0f2ca85b^"
BODY_MARKER = "# ---- driver body ("
# the foreground steps of a cell and its write-up (each traced "> fn" / "< fn rc=N" in call order). The background
# ones (envrec, the samplers, watch_nodes, memguard) run concurrently by design: no order to pin
TRACED = (
    "run_campaign run_cells run_spec cell end_pass sysctlchk savelog fdsum cellchecks md5chk envchk marketchk"
    " expectchk campaignchecks gated warmgatechk hardscan soakchk step_summary_md5 cell_step cell_step_has_row"
    " cell_step_fallback cell_step_rerun_pass"
)
TRACE_WRAP = r"""
_trace_wrap() { # each named function -> a wrapper that traces its entry and exit (with its rc) to $TRACE
    local f src
    for f in "$@"; do
        declare -F "$f" > /dev/null || continue
        src=$(declare -f "$f")
        eval "_traced_$src"
        eval "$f() { echo \"> $f\" >> \"\$TRACE\"; _traced_$f \"\$@\"; set -- \$?; echo \"< $f rc=\$1\" >> \"\$TRACE\"; return \$1; }"
    done
}
"""
# the cell's run-cell.sh (FAKE_HARNESS) also traces itself: its label, and the report scans it runs in-line
TRACE_HARNESS = FAKE_HARNESS.replace(
    'stopall\nexit "${FAKE_RC:-0}"',
    'echo "run-cell $label" >> "$TRACE"\n'
    '[ "${DEFER_REPORT_SCANS:-0}" = 1 ] || echo "run-cell $label report scans in-line (ingest, excerpts)" >> "$TRACE"\n'
    'stopall\nexit "${FAKE_RC:-0}"',
)
assert TRACE_HARNESS != FAKE_HARNESS
# Codex's round 4 probe harness: each node log has a STATE HASH MISMATCH WARN line (hardscan: a HARD FINDING)
HARD_LINE = (
    "INFO torus_node: fake val$i",
    "WARN torus_node: STATE HASH MISMATCH fake val$i",
)
# fake detach.sh that traces the cell command it starts: its argv (minus GENESIS_CACHE_DIR: genesis reuse stays)
TRACE_DETACH = FAKE_DETACH.replace(
    "mkdir -p",
    'a=(); for x in "$@"; do case $x in GENESIS_CACHE_DIR=*) ;; *) a+=("$x") ;; esac; done\n'
    'echo "detach $name $log ${a[*]}" >> "$TRACE"\nmkdir -p',
    1,
)
assert TRACE_DETACH != FAKE_DETACH
# an analysis that rejects its cell's HARD FINDING lines in {run}/campaign.log (Codex round 4)
HARD_ANALYSIS = (
    "import json, sys\n"
    'text = open(sys.argv[2] + "/campaign.log").read()\n'
    'assert "HARD FINDING " + sys.argv[3] + ":" not in text, "correctness finding"\n'
    'json.dump({sys.argv[4]: {"metric": 1}}, open(sys.argv[1], "w"))\n'
)


def body_at(rev):
    """driver-body.sh at git rev REV; None outside a git checkout that has it"""
    r = subprocess.run(
        [
            "git",
            "-C",
            HERE,
            "show",
            "%s:tools/matched-bench/campaign/driver-body.sh" % rev,
        ],
        capture_output=True,
        text=True,
    )
    return r.stdout if r.returncode == 0 else None


def main_tail(body):
    """what a body's main runs after its PREFLIGHT OK line (the cells, the rerun pass, the end pass, the totals)"""
    i = body.index('\nlog "PREFLIGHT OK ')
    return body[body.index("\n", i + 1) + 1 :]


class InlineSequenceTest(FakeHostBase):
    """the owner rule: report scans in-line (DEFER_SCANS=0) = the pre-end-pass driver's per-cell sequence (same
    steps, same order, same inputs) and a no-op end pass; DEFER_SCANS=1 moves only the report-only scans (hardscan,
    soakchk, run-cell.sh's ingest scan and excerpts) into the end pass. Old and new: one driver generated from the
    same spec (old = OLD_BODY_REV's body under the same config), each run through its own main tail against the
    traced fakes"""

    def setUp(self):
        super().setUp()
        self.old = body_at(OLD_BODY_REV)
        if self.old is None:
            self.skipTest("no git history with %s (a frozen tools copy)" % OLD_BODY_REV)
        self.write_fake(
            "trace-run-cell.sh", TRACE_HARNESS.replace("@GUARD@", run_cell_guard())
        )
        self.write_fake("trace-detach.sh", TRACE_DETACH)
        self.write_fake(
            "trace-endpass.sh",
            '#!/bin/bash\necho "cell-endpass ${1##*/}" >> "$TRACE"\nexec "$REAL_ENDPASS" "$@"\n',
        )

    def tearDown(self):
        for f in glob.glob(os.path.join(self.units, "*", "*.pid")):
            try:
                os.killpg(int(open(f).read()), signal.SIGKILL)
            except (OSError, ValueError):
                pass
        super().tearDown()

    def start(self, version, spec_over, cells, smoke=False, hard=False):
        """generate the driver for VERSION ('old': OLD_BODY_REV's body; 'new') under tmp/VERSION and start its
        campaign: preflight_static, then the body's main tail, stdout to {run}/campaign.log as its unit has it"""
        root = os.path.join(self.tmp, version)
        os.makedirs(root)
        spec_over = dict(spec_over)
        if spec_over.pop("analysis", False):
            script = os.path.join(root, "analysis.py")
            with open(script, "w") as f:
                f.write(HARD_ANALYSIS)
            spec_over["analysis"] = {
                "cmd": "python3 -I %s {out} {run} {label} {tag}" % shlex.quote(script),
                "metric": "metric",
            }
        if spec_over.pop("checks", False):
            src = os.path.join(root, "checks.sh")
            with open(src, "w") as f:
                f.write(CHECKS_SH)
            spec_over["checks"] = {"source": src, "per_cell": ["okchk", "alsochk"]}
        spec = minimal_spec(root, **spec_over)
        drv = self.generate(
            self.write_spec(spec, "%s.json" % version), "%s.sh" % version
        )
        text = open(drv).read()
        if version == "old":
            text = text[: text.index(BODY_MARKER)] + self.old
            with open(drv, "w") as f:
                f.write(text)
        harness = os.path.join(self.bin, "trace-run-cell.sh")
        if hard:
            harness = os.path.join(self.bin, "hard-run-cell.sh")
            self.write_fake(
                "hard-run-cell.sh",
                TRACE_HARNESS.replace("@GUARD@", run_cell_guard()).replace(*HARD_LINE),
            )
        run = spec["run_dir"]
        os.makedirs(run)
        trace = os.path.join(root, "trace.txt")
        open(trace, "w").close()
        script = "\n".join(
            [
                "DRIVER_LIB_ONLY=1; . %s || exit 99" % shlex.quote(drv),
                "CELLS=(%s)" % " ".join(shlex.quote(c) for c in cells),
                "SMOKE=1" if smoke else "",
                "preflight_static || exit 90",
                'echo "DEFER_SCANS=${DEFER_SCANS-none}" > %s'
                % shlex.quote(os.path.join(root, "defer")),
                "HARNESS=%s" % shlex.quote(harness),
                "REAL_ENDPASS=${ENDPASS:-}; export REAL_ENDPASS",
                "ENDPASS=%s" % shlex.quote(os.path.join(self.bin, "trace-endpass.sh")),
                "quiet() { return 0; }",
                # the main's run mode up to its PREFLIGHT OK line
                "export CELL_STEP_LOG=$RUN/campaign.log",
                '[ -z "$ANALYSIS_CMD" ] || export CELL_STEP_ANALYSIS_CMD=$ANALYSIS_CMD',
                '[ -z "$ANALYSIS_METRIC" ] || export CELL_STEP_ANALYSIS_METRIC=$ANALYSIS_METRIC',
                '. "$TOOLS/cell-step.sh" || exit 98',
                "sysctl_pin",
                TRACE_WRAP,
                "_trace_wrap %s" % TRACED,
                main_tail(text),
            ]
        )
        units = os.path.join(self.units, version)
        os.makedirs(units)
        e = dict(os.environ)
        e.update(
            self.env(
                DRIVER_DETACH=os.path.join(self.bin, "trace-detach.sh"),
                FAKE_PROC=self.proc,
                FAKE_UNITS=units,
                FAKE_RUN_S="4",
                TRACE=trace,
            )
        )
        log = os.path.join(run, "campaign.log")
        fo = open(log, "w")
        self.addCleanup(fo.close)
        p = subprocess.Popen(
            ["bash", "-c", script],
            stdout=fo,
            stderr=subprocess.STDOUT,
            env=e,
            start_new_session=True,
        )
        self.procs.append(p)
        return p, spec, root

    def finish(self, started):
        p, spec, root = started
        p.wait(timeout=900)
        run = spec["run_dir"]
        tsv = os.path.join(run, "cells.tsv")
        rows = (
            [ln.split("\t") for ln in open(tsv).read().splitlines()[1:]]
            if os.path.exists(tsv)
            else []
        )
        return {
            "rc": p.returncode,
            "defer": open(os.path.join(root, "defer")).read().strip(),
            "out": open(os.path.join(run, "campaign.log")).read(),
            "trace": open(os.path.join(root, "trace.txt"))
            .read()
            .replace(root, "<ROOT>")
            .splitlines(),
            "rows": [(r[0], r[7]) for r in rows],
            "spec": spec,
        }

    def both(self, spec_over, cells, **kw):
        """old and new from the same spec, concurrently (own dirs and fake units)"""
        a = self.start("old", spec_over, cells, **kw)
        b = self.start("new", spec_over, cells, **kw)
        return self.finish(a), self.finish(b)

    @staticmethod
    def report_lines(out):
        """the campaign.log lines that record a step's outcome, without times (digits -> N: sample counts)"""
        keep = (
            "HARD FINDING",
            "CHECK ",
            "CELL-FLAG",
            "CELL FAIL",
            "MAIN CELLS END",
            "CAMPAIGN",
            "END PASS",
            "cells.tsv:",
        )
        res = []
        for ln in out.splitlines():
            ln = re.sub(r"^\[[0-9:]+\] ", "", ln)
            if ln.startswith(keep):
                res.append(re.sub(r"\d+", "N", ln))
        return res

    def without_end_pass(self, trace):
        """the new trace minus run_campaign's and end_pass's own lines; end_pass ran once and traced nothing"""
        enter = [i for i, ln in enumerate(trace) if ln == "> end_pass"]
        self.assertEqual(len(enter), 1, trace)
        self.assertEqual(trace[enter[0] + 1], "< end_pass rc=0", trace[enter[0] :])
        return [ln for ln in trace if ln.split()[1] not in ("run_campaign", "end_pass")]

    def inline_inputs(self, trace):
        """the new trace with run-cell.sh's DEFER_REPORT_SCANS=0 taken off each detach line (Codex round 6: the
        driver sets it after the arm's harness env, from its preflight, so neither an arm nor the launching
        environment can defer an in-line cell's scans; run-cell.sh reads unset as 0, so the input is the old one)"""
        res = []
        for ln in trace:
            if ln.startswith("detach "):
                self.assertEqual(ln.count(" DEFER_REPORT_SCANS="), 1, ln)
                self.assertIn(" DEFER_REPORT_SCANS=0 ", ln)
                ln = ln.replace(" DEFER_REPORT_SCANS=0 ", " ", 1)
            res.append(ln)
        return res

    def assertSameInline(self, old, new, why):
        self.assertEqual(old["defer"], "DEFER_SCANS=none")
        self.assertEqual(new["defer"], "DEFER_SCANS=0", new["out"])
        self.assertIn(
            "PREFLIGHT NOTE: report scans: in-line because %s" % why, new["out"]
        )
        self.assertEqual(new["rc"], old["rc"], new["out"])
        self.assertEqual(
            self.inline_inputs(self.without_end_pass(new["trace"])), old["trace"]
        )
        self.assertNotIn("cell-endpass", "\n".join(new["trace"]))
        self.assertNotIn("END PASS", new["out"])
        self.assertEqual(self.report_lines(new["out"]), self.report_lines(old["out"]))
        self.assertEqual(new["rows"], old["rows"])

    @staticmethod
    def steps(trace, label):
        """one cell's traced steps, from its detach to the end of its run_spec ("> fn" -> fn)"""
        i = next(k for k, ln in enumerate(trace) if ln.startswith("detach %s " % label))
        j = trace.index("< run_spec rc=0", i)
        return [ln[2:] if ln.startswith("> ") else ln for ln in trace[i : j + 1]]

    # ---- Codex round 4 (P2 on d6647299): an in-line analysis that rejects the cell's HARD FINDING lines
    def test_analysis_sees_the_cells_hard_findings(self):
        """the review probe: one smoke cell whose node logs have STATE HASH MISMATCH WARN lines, an analysis.cmd
        that rejects the cell's HARD FINDING lines in {run}/campaign.log: hardscan runs before cell_step (as in the
        pre-end-pass driver), so the row is bad:analysis and a rerun is queued"""
        r = self.finish(
            self.start(
                "new", {"analysis": True}, ["r1-A:A:120:none:"], smoke=True, hard=True
            )
        )
        self.assertEqual(r["defer"], "DEFER_SCANS=0", r["out"])
        self.assertEqual(r["out"].count("HARD FINDING"), 3, r["out"])
        self.assertEqual(r["rows"], [(LB, "bad:analysis")], r["out"])
        ev = json.load(
            open(os.path.join(r["spec"]["run_dir"], "cell-step", LB + ".json"))
        )
        self.assertEqual(ev["analysis"]["status"], "fail", ev)
        self.assertTrue(ev.get("rerun_queued"), ev)
        self.assertLess(r["trace"].index("> hardscan"), r["trace"].index("> cell_step"))

    # ---- the structural pin: in-line = the old sequence
    def test_inline_analysis_campaign_is_the_old_sequence(self):
        """analysis.cmd (in-line), hard findings, so the analysis rejects the cell and its rerun"""
        old, new = self.both({"analysis": True}, ["r1-A:A:120:none:"], hard=True)
        self.assertSameInline(old, new, "analysis.cmd")
        self.assertEqual(
            new["rows"],
            [(LB, "bad:analysis"), (LB + "-rerun", "bad:analysis")],
            new["out"],
        )
        steps = self.steps(new["trace"], LB)
        self.assertIn("run-cell %s report scans in-line (ingest, excerpts)" % LB, steps)
        order = [
            "savelog",
            "cellchecks",
            "hardscan",
            "soakchk",
            "< cell rc=0",
            "step_summary_md5",
            "cell_step",
        ]
        self.assertEqual([s for s in steps if s in order], order, steps)

    def test_inline_checks_and_expect_campaign_is_the_old_sequence(self):
        """a checks file and a run.log expectation (in-line); a warm-up gate cell, then a measured cell (other arm);
        a harness_env (its place on run-cell.sh's env line, in detach's traced argv, is pinned too)"""
        old, new = self.both(
            {
                "checks": True,
                "expect": [{"file": "run.log", "regex": "bench exited rc=0", "min": 1}],
                "harness_env": {"BULK_COUNT": "100000"},
            },
            ["w1:A:120:none:", "r1-M1:M1:120:none:"],
        )
        self.assertSameInline(old, new, "expectation")
        # the driver's DEFER_REPORT_SCANS comes after the arm's harness env on run-cell.sh's env line
        for ln in new["trace"]:
            if ln.startswith("detach "):
                self.assertIn(" BULK_COUNT=100000 DEFER_REPORT_SCANS=0 ", ln)
        self.assertEqual(new["rc"], 0, new["out"])
        self.assertEqual([f for _, f in new["rows"]], ["ok", "ok"], new["out"])
        steps = self.steps(new["trace"], "test-camp-10m-w1")
        order = ["campaignchecks", "warmgatechk", "hardscan", "soakchk", "cell_step"]
        self.assertEqual([s for s in steps if s in order], order, steps)

    def test_deferred_moves_only_the_report_scans(self):
        """DEFER_SCANS=1 (no reader): the old sequence minus hardscan / soakchk and run-cell.sh's in-line report
        scans; the end pass, after the last cell, runs cell-endpass.sh, hardscan and soakchk on the cell"""
        old, new = self.both({}, ["r1-A:A:120:none:"])
        self.assertEqual(new["defer"], "DEFER_SCANS=1", new["out"])
        self.assertEqual(new["rc"], 0, new["out"])
        t = new["trace"]
        i, j = t.index("> end_pass"), t.index("< end_pass rc=0")
        self.assertEqual(
            [
                ln
                for ln in t[i + 1 : j]
                if ln.startswith(("cell-endpass", "> hardscan", "> soakchk"))
            ],
            ["cell-endpass %s" % LB, "> hardscan", "> soakchk"],
        )
        moved = ("hardscan", "soakchk")
        expect = [
            ln
            for ln in old["trace"]
            if ln.split()[1] not in moved
            and not ln.endswith("report scans in-line (ingest, excerpts)")
        ]
        # the one input that differs: run-cell.sh's env line gets DEFER_REPORT_SCANS=1
        self.assertEqual(
            [
                ln.replace(" DEFER_REPORT_SCANS=1 ", " ", 1)
                for ln in t[:i] + t[j + 1 :]
                if ln.split()[1] != "run_campaign"
            ],
            expect,
        )
        self.assertIn(" OVERWRITE=1 DEFER_REPORT_SCANS=1 ", t[t.index("> cell") + 3])
        self.assertEqual(new["rows"], old["rows"])


if __name__ == "__main__":
    unittest.main(verbosity=2)
