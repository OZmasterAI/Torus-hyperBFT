#!/usr/bin/env python3
"""Running state hash attestation check of a matched-bench cell (run-cell.sh STATE_HASH_ATTEST=1; python3 stdlib).

With a genesis state_hash_activation_height (STATE_HASH_ACTIVATION) every node computes the running state hash h_n
and keeps a checkpoint every STATE_HASH_CHECKPOINT_INTERVAL (100) blocks, the last STATE_HASH_CHECKPOINT_RETAIN (64).
Without --state-hash-attest-key no node attests: no on-chain votes, no quorum check, and the fail-stop
(TORUS_STATE_HASH_FAILSTOP=1) has nothing to act on. STATE_HASH_ATTEST=1 gives each node its validator account key
(devnet/wsl/bench-validator-keys.json, the address the genesis registers it under) and turns the fail-stop on; this
script provisions the keys and proves, while the nodes still run, that every node's h_n agrees value for value.

  state_attest.py keys --keys FILE --dir DIR
      Writes DIR/val<i>.key (the hex private key of validator index i, mode 0600, DIR 0700): what start-node.sh
      passes as --state-hash-attest-key. Prints "val0=0x.. val1=0x.. val2=0x.." (the addresses).
  state_attest.py scan --rpc URL --rpc URL --rpc URL [--metrics URL x3] [--log FILE x3] --keys FILE
                       [--wait-s S] --out FILE
      1. the newest retained checkpoint T of every node (torus_getStateHash, no height); T = their minimum;
      2. waits up to S s (default 90) until every node reports a quorumHash for T (the last votes land a few blocks
         after T is durable);
      3. reads torus_getStateHash(h) on every node for h = T, T-100, ... while a node still retains h (at most
         64 heights): localHash (= that node's h_n at h), the on-chain votes (validator, hash, matchesLocal) and the
         on-chain quorumHash;
      4. per node: its metrics (torus_state_hash_attestations_submitted_total, mismatch, no_quorum, height,
         unverified) and log lines (attest key enabled + its address, fail-stop on=, attestations submitted / not
         admitted, STATE HASH MISMATCH, FAIL-STOP, no-quorum).
      Writes FILE (every height, every node's h_n, votes and quorum) with `ok` and `problems`, prints one line.
      Exit 0 even with problems (the verdict is folded by `summary`); 2 = usage.
  state_attest.py summary --dir CELL
      Puts a compact .state_attest (from CELL/state-attest.json) into CELL/summary.json; not ok (or no scan file)
      is folded into .validity (REJECT), .status and .headline.benchmark_accepted, so `health.py accept` rejects the
      cell. Idempotent: resummarize.sh runs it again.

Problems (any one fails the cell): a node unreadable; no common checkpoint; h_n differs between nodes at any common
height; an on-chain vote that differs from a node's h_n; a quorum hash that differs from h_n; a common checkpoint
other than the newest without a quorum (the newest only after the wait); a validator with no vote on a quorum
checkpoint; a node whose log lacks the attest-key line for its validator address or whose fail-stop is not on;
any STATE HASH MISMATCH / FAIL-STOP / no-quorum log line; no attestation submitted (metric) or a non-zero mismatch /
no_quorum counter or unverified gauge on a node.
"""

import argparse
import json
import os
import re
import subprocess
import sys
import time
import urllib.request

INTERVAL = 100  # torus_state::running_hash::STATE_HASH_CHECKPOINT_INTERVAL
RETAIN = 64  # torus_state::running_hash::STATE_HASH_CHECKPOINT_RETAIN
RPC_ERRORS = (OSError, ValueError, KeyError, TypeError)
ANSI = re.compile(r"\x1b\[[0-9;]*m")
HEX32 = re.compile(r"^0x[0-9a-fA-F]{64}$")
ADDR = re.compile(r"0x[0-9a-fA-F]{40}")
# log patterns (crates/torus-consensus/src/state_hash.rs, crates/torus-node/src/main.rs); the grep prefilter needs
# fixed strings, the counts below re-match each line
LOG_ENABLED = "running state hash: automatic AttestStateHash enabled"
LOG_FAILSTOP_CFG = "running state hash fail-stop (TORUS_STATE_HASH_FAILSTOP)"
LOG_SUBMITTED = "state hash attestation submitted"
LOG_NOT_ADMITTED = "state hash attestation not admitted"
LOG_MISMATCH = "STATE HASH MISMATCH"
LOG_FAILSTOP = "FAIL-STOP (TORUS_STATE_HASH_FAILSTOP=1)"
LOG_NO_QUORUM = "every active validator voted, no > 2/3 quorum"
LOG_PATTERNS = (
    LOG_ENABLED,
    LOG_FAILSTOP_CFG,
    LOG_SUBMITTED,
    LOG_NOT_ADMITTED,
    LOG_MISMATCH,
    LOG_FAILSTOP,
    LOG_NO_QUORUM,
)
METRICS = {
    "submitted": "torus_state_hash_attestations_submitted_total",
    "mismatch": "torus_state_hash_mismatch_total",  # labelled {validator=...}: summed
    "no_quorum": "torus_state_hash_no_quorum_total",
    "hash_height": "torus_state_hash_height",
    "unverified": "torus_state_hash_unverified",
}
MAX_LISTED = 5
REASON = "state hash attestation failed: "


# ------------------------------------------------------------------ keys
def read_keys(path):
    """[(index, address lowercase, private key hex without 0x)] sorted by index, exactly indices 0, 1, 2"""
    with open(path) as f:
        d = json.load(f)
    out = []
    for v in d.get("validators") or []:
        k = str(v.get("private_key", ""))
        k = k[2:] if k.startswith("0x") else k
        a = str(v.get("address", "")).lower()
        if not re.fullmatch(r"[0-9a-fA-F]{64}", k) or not re.fullmatch(
            r"0x[0-9a-f]{40}", a
        ):
            raise ValueError(
                "%s: validator index %r has no 64-hex private_key / 20-byte address"
                % (path, v.get("index"))
            )
        out.append((int(v["index"]), a, k.lower()))
    out.sort()
    if [i for i, _, _ in out] != [0, 1, 2]:
        raise ValueError(
            "%s: need validators with index 0, 1, 2 (got %s)"
            % (path, [i for i, _, _ in out])
        )
    return out


def write_keys(keys_path, d):
    keys = read_keys(keys_path)
    old = os.umask(0o077)
    try:
        os.makedirs(d, mode=0o700, exist_ok=True)
        os.chmod(d, 0o700)
        for i, _, k in keys:
            p = os.path.join(d, "val%d.key" % i)
            tmp = p + ".tmp"
            fd = os.open(tmp, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
            with os.fdopen(fd, "w") as f:
                f.write(k + "\n")
            os.chmod(tmp, 0o600)
            os.replace(tmp, p)
    finally:
        os.umask(old)
    return keys


# ------------------------------------------------------------------ rpc / metrics / logs
class Rpc:
    def __init__(self, url, timeout=10):
        self.url = url
        self.timeout = timeout

    def call(self, method, params):
        """(result, None) or (None, error text)"""
        body = json.dumps(
            {"jsonrpc": "2.0", "id": 1, "method": method, "params": params}
        ).encode()
        for attempt in range(3):
            try:
                req = urllib.request.Request(
                    self.url, body, {"content-type": "application/json"}
                )
                with urllib.request.urlopen(req, timeout=self.timeout) as r:
                    reply = json.load(r)
            except RPC_ERRORS as e:
                err = "%s: %s" % (type(e).__name__, e)
                time.sleep(0.5 * (attempt + 1))
                continue
            if "error" in reply:
                e = reply["error"] or {}
                if (
                    isinstance(e, dict) and e.get("code") == -32005
                ):  # per-IP rate limit: retry
                    err = "rate limited: %s" % e.get("message")
                    time.sleep(1.0 * (attempt + 1))
                    continue
                return None, "rpc error: %s" % (
                    e.get("message") if isinstance(e, dict) else e
                )
            return reply.get("result"), None
        return None, err


def hex_int(v):
    if isinstance(v, int):
        return v
    if isinstance(v, str) and v.startswith("0x"):
        try:
            return int(v, 16)
        except ValueError:
            return None
    return None


def state_hash(rpc, height):
    """torus_getStateHash: ({height, local, votes {addr: (hash, matches_local)}, quorum}, None) or (None, error)"""
    r, err = rpc.call("torus_getStateHash", [] if height is None else [height])
    if err is not None:
        return None, err
    try:
        h = hex_int(r["height"])
        local = str(r["localHash"]).lower()
        votes = {
            str(v["validator"]).lower(): (
                str(v["hash"]).lower(),
                bool(v["matchesLocal"]),
            )
            for v in r.get("votes") or []
        }
        q = r.get("quorumHash")
        q = str(q).lower() if q is not None else None
    except (KeyError, TypeError, AttributeError) as e:
        return None, "malformed torus_getStateHash reply: %s" % e
    if h is None or not HEX32.match(local):
        return None, "malformed torus_getStateHash reply: %s" % json.dumps(r)[:200]
    return {"height": h, "local": local, "votes": votes, "quorum": q}, None


def metric_values(text):
    """the METRICS values of a Prometheus scrape (labelled series summed); a missing one is None"""
    out = {k: None for k in METRICS}
    for line in text.splitlines():
        if not line or line.startswith("#"):
            continue
        parts = line.split()
        if len(parts) < 2:
            continue
        name = parts[0].split("{", 1)[0]
        for k, m in METRICS.items():
            if name == m:
                try:
                    v = float(parts[1])
                except ValueError:
                    continue
                out[k] = (out[k] or 0) + v
    if out["mismatch"] is None:
        out["mismatch"] = 0  # a labelled family with no series yet = no mismatch
    return out


def scrape(url, timeout=5):
    try:
        with urllib.request.urlopen(url, timeout=timeout) as r:
            return r.read().decode("utf-8", "replace"), None
    except OSError as e:
        return None, "%s: %s" % (type(e).__name__, e)


def log_facts(path):
    """the state-hash lines of a node log (grep -F prefilter, then each line re-matched ANSI-stripped)"""
    f = {
        "log": path,
        "readable": False,
        "attest_enabled_addrs": [],
        "failstop_on": [],
        "submitted": 0,
        "not_admitted": 0,
        "mismatch_lines": 0,
        "failstop_lines": 0,
        "no_quorum_lines": 0,
        "first_problem_lines": [],
    }
    if not path or not os.path.isfile(path):
        return f
    cmd = ["grep", "-a", "-F"]
    for p in LOG_PATTERNS:
        cmd += ["-e", p]
    try:
        r = subprocess.run(cmd + ["--", path], capture_output=True, timeout=600)
    except (OSError, subprocess.SubprocessError):
        return f
    if r.returncode not in (0, 1):
        return f
    f["readable"] = True
    for raw in r.stdout.decode("utf-8", "replace").splitlines():
        line = ANSI.sub("", raw)
        if LOG_ENABLED in line:
            m = ADDR.search(line[line.index(LOG_ENABLED) :]) or ADDR.search(line)
            f["attest_enabled_addrs"].append(m.group(0).lower() if m else None)
        elif LOG_FAILSTOP_CFG in line:
            m = re.search(r"\bon\W{0,3}(true|false)\b", line)
            f["failstop_on"].append(m.group(1) == "true" if m else None)
        elif LOG_SUBMITTED in line:
            f["submitted"] += 1
        elif LOG_NOT_ADMITTED in line:
            f["not_admitted"] += 1
        else:
            for key, pat in (
                ("mismatch_lines", LOG_MISMATCH),
                ("failstop_lines", LOG_FAILSTOP),
                ("no_quorum_lines", LOG_NO_QUORUM),
            ):
                if pat in line:
                    f[key] += 1
                    if len(f["first_problem_lines"]) < MAX_LISTED:
                        f["first_problem_lines"].append(line.strip()[:400])
                    break
    return f


# ------------------------------------------------------------------ scan
def wait_quorum(rpcs, t, wait_s, sleep=2.0):
    """poll every node until each reports a quorumHash for checkpoint t (or wait_s passes): seconds waited, done"""
    t0 = time.monotonic()
    while True:
        done = True
        for rpc in rpcs:
            r, err = state_hash(rpc, t)
            if err is not None or r["quorum"] is None:
                done = False
                break
        waited = round(time.monotonic() - t0, 1)
        if done or waited >= wait_s:
            return waited, done
        time.sleep(sleep)


def read_rows(rpcs, t):
    """torus_getStateHash on every node for t, t-100, ...: one row per height while a node still retains it (the
    first height some node no longer retains ends the walk, as a partial row)"""
    rows = []
    h = t
    while h >= INTERVAL and len(rows) < RETAIN:
        per = []
        for rpc in rpcs:
            r, err = state_hash(rpc, h)
            per.append(r if err is None else {"error": err})
        if all("error" in p for p in per):
            break
        rows.append({"height": h, "per_node": per})
        if any("error" in p for p in per):
            break  # a node no longer retains h: below the common window
        h -= INTERVAL
    return rows


def row_view(r, names, addr_of):
    """one common height: its table entry and what is wrong with it"""
    per = r["per_node"]
    locals_ = [p["local"] for p in per]
    quorums = [p["quorum"] for p in per]
    vote_diff, q_diff = [], []
    voters = set()
    for name, p in zip(names, per):
        voters |= set(p["votes"])
        vote_diff += [
            "%s@%d (%s) seen on %s" % (addr_of.get(v, v), r["height"], v, name)
            for v, (vh, matches) in p["votes"].items()
            if vh != p["local"] or not matches
        ]
        if p["quorum"] is not None and p["quorum"] != p["local"]:
            q_diff.append("%s@%d" % (name, r["height"]))
    entry = {
        "height": r["height"],
        "h_n": locals_,
        "agree": len(set(locals_)) == 1,
        "quorum": quorums[0] if len(set(quorums)) == 1 else quorums,
        "has_quorum": all(q is not None for q in quorums),
        "votes": sorted(addr_of.get(v, v) for v in voters),
    }
    return entry, voters, vote_diff, q_diff


def min_window(t, activation):
    """checkpoints every node must still serve at newest common t: min(RETAIN, (t - activation) // INTERVAL), >= 1
    (activation unknown: 1, the window is not checked)"""
    if t is None:
        return 0
    if activation is None:
        return 1
    return max(1, min(RETAIN, (t - activation) // INTERVAL))


def judge_rows(out, rows, names, addr_of):
    """fills out's per-height fields and appends the per-height problems"""
    problems = out["problems"]
    common = [r for r in rows if not any("error" in p for p in r["per_node"])]
    out["partial_heights"] = [r["height"] for r in rows if r not in common]
    table, vote_diff, q_diff = [], [], []
    votes_by_val = {a: 0 for a in addr_of}
    unknown = set()
    for r in common:
        entry, voters, vd, qd = row_view(r, names, addr_of)
        table.append(entry)
        vote_diff += vd
        q_diff += qd
        unknown |= voters - set(addr_of)
        if entry["has_quorum"]:
            for v in voters & set(addr_of):
                votes_by_val[v] += 1
    hn_diff = [e["height"] for e in table if not e["agree"]]
    no_q = [e["height"] for e in table if not e["has_quorum"]]
    newest = table[0]["height"] if table else None
    out.update(
        table=table,
        compared=len(table),
        heights=[table[-1]["height"], newest] if table else None,
        newest={"height": newest, "h_n": table[0]["h_n"][0]} if table else None,
        hn_equal=bool(table) and not hn_diff,
        hn_differs_at=hn_diff,
        vote_mismatches=vote_diff[: MAX_LISTED * 4],
        quorum_mismatches=q_diff,
        no_quorum_heights=no_q,
        pending_newest=newest in no_q,
        quorum_reached=len(table) - len(no_q),
        votes_per_validator={addr_of[a]: c for a, c in votes_by_val.items()},
        unknown_voters=sorted(unknown),
    )
    if out.get("newest_common") is not None and not table:
        problems.append("no checkpoint every node retains (latest per node %s)" % out["latest_per_node"])
    want = min_window(out.get("newest_common"), out.get("activation"))
    out["min_compared"] = want
    if table and len(table) < want:
        problems.append(
            "compared only %d checkpoint(s), want >= %d (an RPC error or a node's retention cut the window; partial %s)"
            % (len(table), want, out["partial_heights"])
        )
    if hn_diff:
        first = next(e for e in table if e["height"] == hn_diff[-1])
        problems.append(
            "h_n differs between nodes at %d of %d checkpoints, first %d: %s"
            % (len(hn_diff), len(table), hn_diff[-1], " ".join("%s=%s" % nh for nh in zip(names, first["h_n"])))
        )
    if vote_diff:
        problems.append("%d on-chain attestation(s) differ from a node's h_n, e.g. %s" % (len(vote_diff), "; ".join(vote_diff[:3])))
    if q_diff:
        problems.append("quorum hash differs from the node's h_n: %s" % " ".join(q_diff[:MAX_LISTED]))
    hard_no_q = sorted(h for h in no_q if h != newest)
    if hard_no_q:
        problems.append(
            "%d checkpoint(s) below the newest without an on-chain quorum on every node: %s"
            % (len(hard_no_q), " ".join(str(h) for h in hard_no_q[:MAX_LISTED]))
        )
    if table and out["quorum_reached"] == 0:
        problems.append("no compared checkpoint reached an on-chain quorum (are the nodes attesting?)")
    for name, c in out["votes_per_validator"].items():
        if c < out["quorum_reached"]:
            problems.append("%s voted on %d of the %d quorum checkpoints" % (name, c, out["quorum_reached"]))
    if unknown:
        problems.append("votes from addresses outside the key file: %s" % " ".join(sorted(unknown)))


def judge_metrics(name, url, problems):
    text, err = scrape(url)
    if err is not None:
        problems.append("%s: metrics unreadable (%s)" % (name, err))
        return {"metrics_error": err}
    nd = metric_values(text)
    if not nd["submitted"]:
        problems.append("%s: %s = %s (the node sent no attestation)" % (name, METRICS["submitted"], nd["submitted"]))
    problems += ["%s: %s = %s" % (name, METRICS[k], nd[k]) for k in ("mismatch", "no_quorum", "unverified") if nd.get(k)]
    # no_quorum (plain counter) and unverified (gauge) are always exported once the node runs: absent = unreadable
    problems += ["%s: %s missing from the scrape" % (name, METRICS[k]) for k in ("no_quorum", "unverified") if nd.get(k) is None]
    return nd


def judge_log(name, path, want, problems):
    lf = log_facts(path)
    if not lf["readable"]:
        problems.append("%s: log unreadable (%s)" % (name, path))
        return lf
    if not lf["attest_enabled_addrs"]:
        problems.append("%s: no '%s' line (the node did not load --state-hash-attest-key)" % (name, LOG_ENABLED))
    elif any(a != want for a in lf["attest_enabled_addrs"]):
        problems.append("%s: attests as %s, its validator address is %s" % (name, lf["attest_enabled_addrs"], want))
    if not lf["failstop_on"] or not all(v is True for v in lf["failstop_on"]):
        problems.append("%s: fail-stop not live (TORUS_STATE_HASH_FAILSTOP on=%s)" % (name, lf["failstop_on"]))
    for k, what in (("mismatch_lines", "STATE HASH MISMATCH"), ("failstop_lines", "FAIL-STOP"), ("no_quorum_lines", "no-quorum")):
        if lf[k]:
            problems.append("%s: %d %s log line(s), e.g. %s" % (name, lf[k], what, lf["first_problem_lines"][:1]))
    return lf


def scan(rpcs, keys, metrics_urls, logs, wait_s, sleep=2.0, activation=None):
    names = ["val%d" % i for i in range(len(rpcs))]
    addr_of = {a: "val%d" % i for i, a, _ in keys}
    out = {
        "nodes": names,
        "validators": {"val%d" % i: a for i, a, _ in keys},
        "interval": INTERVAL,
        "activation": activation,
        "problems": [],
    }
    problems = out["problems"]
    latest = []
    for name, rpc in zip(names, rpcs):
        r, err = state_hash(rpc, None)
        if err is not None:
            problems.append("%s: no retained checkpoint readable (%s)" % (name, err))
        latest.append(None if err is not None else r["height"])
    out["latest_per_node"] = latest
    t = min([h for h in latest if h is not None], default=None)
    out["newest_common"] = t
    rows = []
    if t is not None and None not in latest:
        out["quorum_wait_s"], out["quorum_wait_done"] = wait_quorum(rpcs, t, wait_s, sleep)
        rows = read_rows(rpcs, t)
    judge_rows(out, rows, names, addr_of)
    out["per_node"] = {}
    for i, name in enumerate(names):
        nd = judge_metrics(name, metrics_urls[i], problems) if i < len(metrics_urls or []) else {}
        if i < len(logs or []):
            nd["log"] = judge_log(name, logs[i], out["validators"].get(name), problems)
        out["per_node"][name] = nd
    out["ok"] = not problems
    return out


def line_of(out):
    pn = out.get("per_node") or {}

    def col(f):
        return "/".join(str(f(pn.get(n) or {})) for n in out.get("nodes") or [])

    def num(v):
        return "?" if v is None else "%d" % v

    hs = out.get("heights")
    newest = out.get("newest") or {}
    return (
        "state attest: ok=%d compared %d checkpoint(s) %s on %d nodes, h_n equal=%s%s; quorum %d/%d%s (waited %ss);"
        " votes %s; submitted %s; metric mismatch %s no_quorum %s; fail-stop on %s; MISMATCH/FAIL-STOP log lines %s%s"
        % (
            1 if out.get("ok") else 0,
            out.get("compared", 0),
            ("%d..%d" % (hs[0], hs[1])) if hs else "-",
            len(out.get("nodes") or []),
            "yes" if out.get("hn_equal") else "NO",
            (" (h_%d=%s)" % (newest["height"], newest["h_n"])) if newest else "",
            out.get("quorum_reached", 0),
            out.get("compared", 0),
            " (newest pending)" if out.get("pending_newest") else "",
            out.get("quorum_wait_s", "-"),
            " ".join(
                "%s=%s" % kv
                for kv in sorted((out.get("votes_per_validator") or {}).items())
            )
            or "-",
            col(lambda d: num(d.get("submitted"))),
            col(lambda d: num(d.get("mismatch"))),
            col(lambda d: num(d.get("no_quorum"))),
            col(
                lambda d: (
                    ",".join(
                        str(v).lower()
                        for v in (d.get("log") or {}).get("failstop_on", [])
                    )
                    or "-"
                )
            ),
            col(
                lambda d: (
                    "%s+%s"
                    % (
                        (d.get("log") or {}).get("mismatch_lines", "?"),
                        (d.get("log") or {}).get("failstop_lines", "?"),
                    )
                )
            ),
            ("; PROBLEMS: " + " | ".join(out["problems"]))
            if out.get("problems")
            else "",
        )
    )


# ------------------------------------------------------------------ summary
def compact(full):
    """the summary.json .state_attest view of a scan (the full table stays in state-attest.json)"""
    keep = (
        "ok",
        "problems",
        "compared",
        "heights",
        "newest",
        "hn_equal",
        "hn_differs_at",
        "quorum_reached",
        "pending_newest",
        "no_quorum_heights",
        "quorum_wait_s",
        "quorum_wait_done",
        "votes_per_validator",
        "validators",
        "latest_per_node",
        "partial_heights",
    )
    c = {k: full.get(k) for k in keep}
    c["per_node"] = {}
    for n, d in (full.get("per_node") or {}).items():
        lf = d.get("log") or {}
        c["per_node"][n] = {
            "attestations_submitted": d.get("submitted"),
            "mismatch_total": d.get("mismatch"),
            "no_quorum_total": d.get("no_quorum"),
            "state_hash_height": d.get("hash_height"),
            "unverified": d.get("unverified"),
            "attest_address": (lf.get("attest_enabled_addrs") or [None])[-1],
            "failstop_on": lf.get("failstop_on"),
            "log_submitted": lf.get("submitted"),
            "log_not_admitted": lf.get("not_admitted"),
            "log_mismatch_lines": lf.get("mismatch_lines"),
            "log_failstop_lines": lf.get("failstop_lines"),
            "log_no_quorum_lines": lf.get("no_quorum_lines"),
        }
    c["file"] = "state-attest.json"
    return c


def fold_verdict(d):
    """a cell whose attestation check failed (or never ran) is rejected in the canonical verdict too. Idempotent."""
    e = d.get("state_attest") or {}
    if e.get("ok") is True:
        return
    v = d.setdefault("validity", {})
    fails = [f for f in v.get("fail_reasons") or [] if not str(f).startswith(REASON)]
    v["fail_reasons"] = fails + [
        REASON + "; ".join(e.get("problems") or ["no .state_attest"])
    ]
    v["verdict"] = "REJECT"
    v["accepted"] = False
    d["status"] = "INVALID"
    d.setdefault("headline", {})["benchmark_accepted"] = False


def summary(cell):
    path = os.path.join(cell, "summary.json")
    with open(path) as f:
        d = json.load(f)
    try:
        with open(os.path.join(cell, "state-attest.json")) as f:
            d["state_attest"] = compact(json.load(f))
    except (OSError, ValueError) as e:
        d["state_attest"] = {
            "ok": False,
            "problems": ["no readable state-attest.json (%s)" % e],
        }
    fold_verdict(d)
    tmp = path + ".state-attest.tmp"
    with open(tmp, "w") as f:
        json.dump(d, f, indent=1)
    os.replace(tmp, path)
    return d["state_attest"]


def main(argv=None):
    p = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    sub = p.add_subparsers(dest="cmd", required=True)
    k = sub.add_parser("keys")
    k.add_argument("--keys", required=True)
    k.add_argument("--dir", required=True)
    s = sub.add_parser("scan")
    s.add_argument("--rpc", action="append", required=True)
    s.add_argument("--metrics", action="append", default=[])
    s.add_argument("--log", action="append", default=[])
    s.add_argument("--keys", required=True)
    s.add_argument("--wait-s", type=float, default=90)
    s.add_argument("--activation", type=int, default=None, help="STATE_HASH_ACTIVATION height (sets the minimum compared window)")
    s.add_argument("--out", required=True)
    m = sub.add_parser("summary")
    m.add_argument("--dir", required=True)
    a = p.parse_args(argv)
    if a.cmd == "keys":
        try:
            keys = write_keys(a.keys, a.dir)
        except (OSError, ValueError, KeyError) as e:
            print("state_attest.py keys: %s" % e, file=sys.stderr)
            return 1
        print(" ".join("val%d=%s" % (i, addr) for i, addr, _ in keys))
        return 0
    if a.cmd == "scan":
        try:
            keys = read_keys(a.keys)
        except (OSError, ValueError, KeyError) as e:
            print("state_attest.py scan: %s" % e, file=sys.stderr)
            return 2
        if (
            len(a.rpc) != len(keys)
            or (a.metrics and len(a.metrics) != len(a.rpc))
            or (a.log and len(a.log) != len(a.rpc))
        ):
            print(
                "state_attest.py scan: need one --rpc (and --metrics / --log, when given) per validator (%d)"
                % len(keys),
                file=sys.stderr,
            )
            return 2
        out = scan([Rpc(u) for u in a.rpc], keys, a.metrics, a.log, a.wait_s, activation=a.activation)
        with open(a.out, "w") as f:
            json.dump(out, f, indent=1)
        print(line_of(out))
        return 0
    c = summary(a.dir)
    print(
        "state_attest ok=%s problems=%d" % (c.get("ok"), len(c.get("problems") or []))
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
