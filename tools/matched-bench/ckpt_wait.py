#!/usr/bin/env python3
"""Before the state digest: wait until every live validator has made durable a
checkpoint SUBMITTED after the drain completed.

Item 6 Phase 3 coalesced checkpoints (TORUS_CHECKPOINT_INTERVAL_MS, 15 s on
the standard load): validator RPC reads RocksDB only and lags up to one
checkpoint window by design (docs/plans/item6-phase3-design.md, option C). The
drain's quiet window is 10 s < 15 s, so a digest right after the drain can read
a durable state that predates the last state change
(ozarchy-9k-smokek-smoke-s0: the close after the last cancel-all landed 1.6 s /
2.8 s into the digest, books flipped stale -> correct mid-digest, DISAGREE).

The proof, per live node (the caller passes only the live ones):
  C0 = torus_exec_checkpoints_total at T0 (now; the chain is quiet). The
       counter goes up when checkpoint_submit hands a checkpoint to the flush
       worker (app.rs), after waiting for the previous one in flight.
  W  = torus_exec_checkpoint_db_write_seconds_count, observed when the flush
       worker's checkpoint DB write finished (exec_pipeline.rs
       observe_checkpoint_write).
  Submits are serialized, so W >= C0 + 1 means the first checkpoint submitted
  after T0 is on disk, and it covers every state change before T0.

Checkpoints off (the node's own resolution, resolve_interval_ms) and no node
reporting a checkpoint -> skipped, one line, no wait. Checkpoints on and both
counters 0 at T0 is NOT a skip: the first window has not flushed yet (blocks
stay memory-only until the first checkpoint), so the node waits for its first
durable checkpoint (C0 + 1 = 1) like any other. The env says off but a node
did checkpoint -> the node is the truth: wait, with a WARNING note. A node
without either metric (older binary) with checkpoints on -> sleep interval +
margin for it (the nodes that do report them are still held to W >= C0 + 1),
outcome "fallback_sleep": exit 0, but no durable evidence for that node, so
summarize.py does not count the digest as pinned. No W >= C0 + 1 on every node
within the timeout (default 2 x interval + 30 s) -> outcome "timeout", exit 3:
the digest may predate the final state (run-cell.sh logs it, summarize.py
marks the cell).

  ckpt_wait.py --metrics val0=URL [--metrics val2=URL ...] --node-env JSON
               [--genesis-book-mode MODE] --out FILE [--timeout-s S]
               [--poll-s S] [--fallback-margin-s S]

Prints one line ("digest checkpoint wait: ..."); FILE gets the evidence.
Exit 0 durable / skipped / fallback_sleep (no evidence, not a pass), 3
timeout, 2 bad arguments.
Standard library only: run-cell.sh runs it with python3 -I.
"""

import argparse
import json
import sys
import time
import urllib.request

SUBMITS = "torus_exec_checkpoints_total"
WRITES = "torus_exec_checkpoint_db_write_seconds_count"
VALIDATOR_DEFAULT_INTERVAL_MS = 15000
TIMEOUT_RC = 3

# Rust `char::is_whitespace` (what `str::trim` strips), as summarize.py.
RUST_WHITESPACE = "\t\n\x0b\x0c\r \x85\xa0                　"
U64_MAX = 2**64 - 1
GENESIS_BOOK_MODES = {
    "classic": "0",
    "order_rows": "1",
    "level_authority": "2",
    "level_authority_chunked": "3",
}


def rust_trim(v):
    return (v or "").strip(RUST_WHITESPACE)


def rust_parse_u64(v):
    body = v[1:] if v.startswith("+") else v
    if not body or not all("0" <= ch <= "9" for ch in body):
        return None
    n = int(body)
    return n if n <= U64_MAX else None


def resolve_interval_ms(node_env, genesis_book_mode=""):
    """The checkpoint interval (ms) the validators run, None = off. Same
    resolution as summarize.py checkpoint_interval_s (exec_pipeline.rs
    effective_checkpoint_interval + app.rs): TORUS_EXEC_PIPELINE=0 -> off;
    TORUS_CHECKPOINT_INTERVAL_MS non-blank -> trim().parse::<u64>(), 0 or Err
    -> off; blank/unset -> the validator default 15 s unless the book mode is
    2/3 (genesis consensus.book_mode, else the legacy TORUS_BOOK_ROWS)."""
    if rust_trim(node_env.get("TORUS_EXEC_PIPELINE")) == "0":
        return None
    raw = rust_trim(node_env.get("TORUS_CHECKPOINT_INTERVAL_MS"))
    if raw:
        return rust_parse_u64(raw) or None
    gm = (genesis_book_mode or "").strip()
    if gm:
        mode = GENESIS_BOOK_MODES.get(gm, gm)
    else:
        rows = rust_trim(node_env.get("TORUS_BOOK_ROWS"))
        mode = rows if rows in ("1", "2", "3") else "0"
    return None if mode in ("2", "3") else VALIDATOR_DEFAULT_INTERVAL_MS


def off_reason(node_env, genesis_book_mode):
    if rust_trim(node_env.get("TORUS_EXEC_PIPELINE")) == "0":
        return "TORUS_EXEC_PIPELINE=0"
    raw = node_env.get("TORUS_CHECKPOINT_INTERVAL_MS")
    if rust_trim(raw):
        return "TORUS_CHECKPOINT_INTERVAL_MS=%r" % raw
    return (
        "TORUS_CHECKPOINT_INTERVAL_MS unset, book mode %s has no default interval"
        % (genesis_book_mode or "TORUS_BOOK_ROWS=%s" % node_env.get("TORUS_BOOK_ROWS"))
    )


def scrape(url, timeout=3.0):
    """{SUBMITS: n|None, WRITES: n|None}, or None when the scrape failed."""
    try:
        with urllib.request.urlopen(url, timeout=timeout) as r:
            if r.status != 200:
                return None
            text = r.read().decode("utf-8", "replace")
    except (OSError, ValueError):
        return None
    vals = {SUBMITS: None, WRITES: None}
    for line in text.splitlines():
        f = line.split()
        if len(f) >= 2 and f[0] in vals:
            try:
                vals[f[0]] = int(float(f[1]))
            except ValueError:
                pass
    return vals


def parse_args(argv):
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--metrics", action="append", required=True, metavar="NAME=URL")
    ap.add_argument("--node-env", required=True)
    ap.add_argument("--genesis-book-mode", default="")
    ap.add_argument("--out", required=True)
    ap.add_argument("--timeout-s", type=float, default=None)
    ap.add_argument("--poll-s", type=float, default=0.5)
    ap.add_argument("--fallback-margin-s", type=float, default=5.0)
    a = ap.parse_args(argv)
    a.nodes = []
    for m in a.metrics:
        name, sep, url = m.partition("=")
        if not sep or not name or not url:
            ap.error("--metrics wants NAME=URL, got %r" % m)
        a.nodes.append((name, url))
    try:
        a.env = json.loads(a.node_env) if a.node_env.strip() else {}
    except ValueError:
        ap.error("--node-env is not JSON")
    if not isinstance(a.env, dict):
        ap.error("--node-env is not a JSON object")
    return a


def baseline(nodes, deadline, poll_s, retry, base=None):
    """name -> scrape() for every node that answered; with `retry`, keep
    asking the others until the deadline. `base`: answers already in."""
    base = dict(base or {})
    while True:
        for name, url in nodes:
            if name not in base:
                v = scrape(url)
                if v is not None:
                    base[name] = v
        if not retry or len(base) == len(nodes) or time.monotonic() >= deadline:
            return base
        time.sleep(poll_s)


def poll_durable(rec, nodes, t0, deadline, poll_s):
    """Poll WRITES until every node reached its target (C0 + 1) or the
    deadline; marks rec nodes durable. Returns the names still pending.

    The pairing: SUBMITS (C0) goes up when checkpoint_submit hands a job to
    the flush worker (app.rs), WRITES when the worker finished WRITING one
    (exec_pipeline.rs). Submits are serialized (each waits for the previous
    job in flight), so WRITES >= C0 + 1 means every job submitted before T0
    plus one more, submitted after T0, is durable: the on-disk state covers
    every state change before T0."""
    pending = dict(nodes)
    while True:
        for name in list(pending):
            node = rec["nodes"][name]
            if (node["write_count"] or 0) >= node["target"]:
                node["durable"] = True
                node["waited_s"] = round(time.monotonic() - t0, 3)
                del pending[name]
        if not pending or time.monotonic() >= deadline:
            return list(pending)
        time.sleep(poll_s)
        for name, url in pending.items():
            v = scrape(url)
            if v is not None and v[WRITES] is not None:
                rec["nodes"][name]["write_count"] = v[WRITES]


def per_node_line(rec, nodes):
    return ", ".join("%s ckpt %s->%s %s" % (n, rec["nodes"][n]["c0"], rec["nodes"][n]["write_count"],
                                          "durable" if rec["nodes"][n]["durable"] else "NOT durable")
                     for n, _ in nodes)


def new_record(nodes, interval_ms, timeout_s, poll_s):
    return {"outcome": None, "interval_ms": interval_ms, "timeout_s": timeout_s,
            "poll_s": poll_s, "metrics": {"submits": SUBMITS, "writes": WRITES},
            "note": None, "waited_s": 0.0, "t0_unix": round(time.time(), 3),
            "nodes": {n: {"c0": None, "target": None, "write_count": None, "durable": False,
                          "waited_s": None, "missing": []} for n, _ in nodes}}


def main(argv=None):
    a = parse_args(argv)
    nodes = a.nodes
    interval_ms = resolve_interval_ms(a.env, a.genesis_book_mode)
    eff_ms = interval_ms or VALIDATOR_DEFAULT_INTERVAL_MS
    timeout_s = a.timeout_s if a.timeout_s is not None else 2 * eff_ms / 1000.0 + 30
    rec = new_record(nodes, interval_ms, timeout_s, a.poll_s)
    t0 = time.monotonic()
    deadline = t0 + timeout_s
    iv = "interval %s ms" % interval_ms if interval_ms else "checkpoints off"
    off = "checkpoints off (%s)" % off_reason(a.env, a.genesis_book_mode)

    def finish(outcome, line, rc=0):
        rec["outcome"] = outcome
        rec["waited_s"] = round(time.monotonic() - t0, 3)
        with open(a.out, "w") as f:
            json.dump(rec, f, indent=1)
        print("digest checkpoint wait: " + line.replace("{waited}", "%.1f" % rec["waited_s"]))
        return rc

    # T0 baseline. Checkpoints off: one pass, no retry (no wait when off),
    # unless a node that answered did checkpoint: then they are on whatever
    # the env says, and an unreadable node must not let the wait be skipped.
    base = baseline(nodes, deadline, a.poll_s, retry=interval_ms is not None)
    if interval_ms is None and len(base) < len(nodes) and any(
            (v[SUBMITS] or 0) > 0 for v in base.values()):
        base = baseline(nodes, deadline, a.poll_s, retry=True, base=base)
    bad = [n for n, _ in nodes if n not in base]
    ran_any = any((v[SUBMITS] or 0) > 0 for v in base.values())
    if bad and interval_ms is None and not ran_any:
        rec["note"] = "metrics unreadable on %s" % " ".join(bad)
        return finish("skipped", "skipped, %s; metrics unreadable on %s" % (off, " ".join(bad)))
    if bad:
        if interval_ms is None:
            rec["note"] = ("WARNING: the node env resolves %s but %s ran checkpoints"
                           % (off, " ".join(n for n, v in base.items() if (v[SUBMITS] or 0) > 0)))
        return finish("timeout", "TIMEOUT after {waited}s: metrics unreadable on %s (%s)%s"
                      % (" ".join(bad), iv, " (%s)" % rec["note"] if rec["note"] else ""), TIMEOUT_RC)
    for name, v in base.items():
        node = rec["nodes"][name]
        node["c0"], node["write_count"] = v[SUBMITS], v[WRITES]
        node["missing"] = [k for k in (SUBMITS, WRITES) if v[k] is None]
        node["target"] = None if v[SUBMITS] is None else v[SUBMITS] + 1
    missing = [n for n, _ in nodes if rec["nodes"][n]["missing"]]
    ran = [n for n, _ in nodes if (base[n][SUBMITS] or 0) > 0]
    # Checkpoints on (by the env, or by a node that ran one) and a counter at
    # 0 = the first window has not flushed yet, NOT "no checkpoints": the
    # node is held to its first durable checkpoint (target 1) like any other.
    fresh = [n for n, _ in nodes if n not in missing and not base[n][SUBMITS]]
    fresh_note = ("%s=0 at T0 on %s: waiting for the first checkpoint" % (SUBMITS, " ".join(fresh))
                  if fresh else None)

    if interval_ms is None and not ran:
        return finish("skipped", "skipped, %s%s" % (
            off, "; checkpoint metrics absent on %s" % " ".join(missing) if missing else ""))
    if missing:
        # Mixed fleet: the nodes that report both metrics are still held to
        # W >= C0 + 1 (an older binary elsewhere excuses no stalled node, nor
        # one whose first checkpoint never landed); the others only get a
        # sleep of interval + margin, which proves nothing about them:
        # outcome fallback_sleep, and summarize.py leaves the digest unpinned.
        names = "/".join(sorted({k for n in missing for k in rec["nodes"][n]["missing"]}))
        rec["note"] = ("checkpoint metrics absent on %s (older binary): slept interval + %.1f s, "
                       "no durable evidence on %s" % (" ".join(missing), a.fallback_margin_s,
                                                      " ".join(missing)))
        if fresh_note:
            rec["note"] += "; " + fresh_note
        capable = [(n, u) for n, u in nodes if n not in missing]
        pending = poll_durable(rec, capable, t0, deadline, a.poll_s) if capable else []
        per = per_node_line(rec, capable)
        if pending:
            return finish("timeout", "TIMEOUT: %s, waited {waited}s of %.0fs (%s) (%s)"
                          % (per, timeout_s, iv, rec["note"]), TIMEOUT_RC)
        time.sleep(max(0.0, eff_ms / 1000.0 + a.fallback_margin_s - (time.monotonic() - t0)))
        return finish("fallback_sleep", "%s%s absent on %s (older binary), slept {waited}s >= "
                      "interval + %.1fs margin, no durable evidence on %s (%s)"
                      % (per + ", " if per else "", names, " ".join(missing),
                         a.fallback_margin_s, " ".join(missing), iv))
    if interval_ms is None:
        rec["note"] = ("WARNING: the node env resolves %s but %s ran checkpoints: waiting anyway "
                       "(timeout from the 15 s default)" % (off, " ".join(ran)))
    if fresh_note:
        rec["note"] = "; ".join(x for x in (rec["note"], fresh_note) if x)

    # The wait. C0 counted submits (at submit), WRITES counts jobs the flush
    # worker finished writing; submits are serialized, so WRITES >= C0 + 1 =
    # every job submitted before T0 plus one more is durable (poll_durable).
    pending = poll_durable(rec, nodes, t0, deadline, a.poll_s)
    per = per_node_line(rec, nodes)
    warn = " (%s)" % rec["note"] if rec["note"] else ""
    if pending:
        return finish("timeout", "TIMEOUT: %s, waited {waited}s of %.0fs (%s)%s"
                      % (per, timeout_s, iv, warn), TIMEOUT_RC)
    return finish("durable", "%s, waited {waited}s (%s)%s" % (per, iv, warn))


if __name__ == "__main__":
    sys.exit(main())
