#!/usr/bin/env python3
"""Dead-leader window report of a no-restart kill cell (run-cell.sh DL_REPORT, bench 9p; python3 stdlib).

  dl_window.py --dir CELL --t-bench0 T0 --t-bench1 T1 [--dl-k K] [--dl-jcap J]

Reads the cell's kill-norestart.json (kill_ts, the killed node, its height at the kill), sampler.csv (the 1 Hz
per-node counters) and the survivors' node logs (CELL/val<i>.log.gz, run-cell.sh step 9), and splits the load window
[T0, T1] into three windows:

  pre_kill            [T0, kill_ts]
  kill_to_exclusion   [kill_ts, exclusion committed]
  post_exclusion      [exclusion committed, T1]

How the exclusion is observed (feat/dead-leader-p3; no RPC and no metric names a leader-excluded validator, and
hotstuff's UpdateValidatorSet event is not logged, log_events(false)):
  * the delta: every node logs, when it builds or validates an epoch boundary block that carries a leader-exclusion
    delta, the INFO line "epoch boundary: leader exclusion delta from the execution plan height=H exclude=X
    readmit=Y excluded=Z" (torus-consensus app.rs epoch_validator_set_updates; counts only, the validator is not
    named). The first such line with exclude >= 1 after the kill, on any survivor, gives the boundary height H and
    the time the delta was proposed (exclusion.delta_ts);
  * committed: block H carries the validator-set update, so the update is committed when H is: the first sampler.csv
    row (after the kill) of any survivor whose committed height (torus_block_height + torus_exec_queue_depth, as
    health.py) reaches H (exclusion.committed_ts; 1 Hz resolution). Per survivor too (committed_ts_by_node);
  * corroboration: the last increment of torus_consensus_timeout_total_total on each survivor after the kill
    (timeouts.last_increment_ts_by_node): with the dead leader excluded its views stop timing out.
No delta line after the kill (K = 0, a binary without the feature, or the load window ended first): exclusion.found
is false, kill_to_exclusion runs to T1 and post_exclusion is empty (None rates, with a reason).

Per window and survivor: blk_s (torus_block_height), committed_blk_s, matched_s, timeouts_per_min, views_per_min;
consecutive windows share their boundary sample, as summarize.py's kill_no_restart windows. headline = val0 (never
the killed node: KILL_NO_RESTART refuses val0). Writes CELL/dl-window.json, sets summary.json .dead_leader (when
summary.json exists) and prints one line for run.log. Exit 0 with problems (recorded in .problems); 2 = usage or
no kill record.
"""

import argparse
import csv
import gzip
import json
import math
import os
import re
import shutil
import subprocess
import sys
from datetime import datetime

DELTA_MSG = "leader exclusion delta from the execution plan"
ANSI = re.compile(r"\x1b\[[0-9;]*m")
TS = re.compile(r"(\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d(?:\.\d+)?)Z")
RATES = (
    ("blk_s", "torus_block_height", 1, 3),
    ("committed_blk_s", "torus_blocks_committed_total", 1, 3),
    ("matched_s", "torus_orders_matched_total", 1, 1),
    ("timeouts_per_min", "torus_consensus_timeout_total_total", 60, 2),
    ("views_per_min", "torus_consensus_view", 60, 1),
)
TIMEOUTS = "torus_consensus_timeout_total_total"


def field(line, name):
    m = re.search(r'"?%s"?[=:] ?"?(\d+)' % name, line)
    return int(m.group(1)) if m else None


def log_ts(line):
    m = TS.search(line)
    if not m:
        return None
    s = m.group(1)
    if "." in s:
        a, b = s.split(".")
        s = a + "." + (b + "000000")[:6]
    return datetime.fromisoformat(s + "+00:00").timestamp()


def parse_delta_line(raw, node):
    line = ANSI.sub("", raw)
    if DELTA_MSG not in line:
        return None
    return {
        "node": node,
        "ts": log_ts(line),
        "height": field(line, "height"),
        "exclude": field(line, "exclude"),
        "readmit": field(line, "readmit"),
        "excluded": field(line, "excluded"),
    }


def delta_lines(path, node):
    """the delta lines of one node log (.gz or plain); grep when present (logs are hundreds of MB)"""
    if not os.path.exists(path):
        return None
    lines = []
    if shutil.which("grep") and (not path.endswith(".gz") or shutil.which("gzip")):
        if path.endswith(".gz"):
            p1 = subprocess.Popen(["gzip", "-cd", path], stdout=subprocess.PIPE)
            p2 = subprocess.run(
                ["grep", "-aF", DELTA_MSG], stdin=p1.stdout, capture_output=True
            )
            p1.stdout.close()
            p1.wait()
        else:
            p2 = subprocess.run(["grep", "-aF", DELTA_MSG, path], capture_output=True)
        text = p2.stdout.decode("utf-8", "replace").splitlines()
    else:
        opener = gzip.open if path.endswith(".gz") else open
        with opener(path, "rt", errors="replace") as f:
            text = [l for l in f if DELTA_MSG in l]
    for raw in text:
        d = parse_delta_line(raw, node)
        if d is not None:
            lines.append(d)
    return lines


def num(v):
    try:
        x = float(v)
    except (TypeError, ValueError):
        return None
    return x if math.isfinite(x) else None


def load_rows(path, dead):
    rows = {}
    with open(path) as f:
        for r in csv.DictReader(f):
            ts = num(r.get("ts"))
            node = r.get("node")
            if ts is None or not node:
                continue
            if node == dead and num(r.get("scrape_valid")) == 0:
                continue  # the killed node's failed scrapes, as summarize.py
            rows.setdefault(node, []).append(dict(r, ts=ts))
    for rs in rows.values():
        rs.sort(key=lambda r: r["ts"])
    return rows


def committed(r):
    h, q = num(r.get("torus_block_height")), num(r.get("torus_exec_queue_depth"))
    return None if h is None or q is None else h + q


def window(rs, lo, hi, prev_boundary=True):
    """samples in (lo, hi], plus the last sample at or before lo (the shared boundary)"""
    pre = [r for r in rs if r["ts"] <= lo]
    inner = [r for r in rs if lo < r["ts"] <= hi]
    return (pre[-1:] if prev_boundary else []) + inner


def rates(sel):
    if len(sel) < 2:
        return dict(
            {k: None for k, _, _, _ in RATES},
            samples=len(sel),
            reason="%d sampler.csv sample(s) in the window: a delta needs >= 2"
            % len(sel),
        )
    a, b = sel[0], sel[-1]
    dt = b["ts"] - a["ts"]
    out = {
        "samples": len(sel),
        "first_ts": a["ts"],
        "last_ts": b["ts"],
        "span_s": round(dt, 3),
    }
    for k, col, scale, nd in RATES:
        x, y = num(a.get(col)), num(b.get(col))
        out[k] = (
            None
            if dt <= 0 or x is None or y is None
            else round((y - x) / dt * scale, nd)
        )
    return out


def last_increment(rs, lo, hi):
    last, prev = None, None
    for r in rs:
        v = num(r.get(TIMEOUTS))
        if r["ts"] > hi:
            break
        if prev is not None and v is not None and v > prev and r["ts"] > lo:
            last = r["ts"]
        prev = v if v is not None else prev
    return last


def report(cell, t0, t1, dl_k=None, dl_jcap=None):
    with open(os.path.join(cell, "kill-norestart.json")) as f:
        kill = json.load(f)
    kill_ts = num(kill.get("kill_ts"))
    dead = kill.get("kill_node")
    if kill_ts is None or not dead:
        raise ValueError("kill-norestart.json has no kill_ts / kill_node")
    rows = load_rows(os.path.join(cell, "sampler.csv"), dead)
    nodes = sorted(n for n in rows if n != dead)
    problems = []
    deltas = []
    logs_read = {}
    for n in nodes:
        got = delta_lines(os.path.join(cell, n + ".log.gz"), n)
        if got is None:
            got = delta_lines(os.path.join(cell, n + ".log"), n)
        logs_read[n] = got is not None
        if got is None:
            problems.append("%s: no node log (%s.log.gz)" % (n, n))
            continue
        deltas += got
    deltas.sort(key=lambda d: (d["ts"] is None, d["ts"] or 0))
    # one entry per boundary height: its first line on any survivor
    by_height = {}
    for d in deltas:
        if d["height"] is not None and d["height"] not in by_height:
            by_height[d["height"]] = d
    after = [
        d
        for d in sorted(by_height.values(), key=lambda d: d["height"])
        if d["ts"] is not None and d["ts"] >= kill_ts
    ]
    first_ex = next((d for d in after if (d["exclude"] or 0) >= 1), None)
    excl = {
        "found": first_ex is not None,
        "source": "survivor node logs: '%s' (first line with exclude >= 1 "
        "after the kill); committed = the first sampler.csv row whose committed height "
        "(torus_block_height + torus_exec_queue_depth) reaches its boundary height"
        % DELTA_MSG,
    }
    commit_ts = None
    if first_ex:
        hb = first_ex["height"]
        by_node = {}
        for n in nodes:
            c = next(
                (
                    r["ts"]
                    for r in rows[n]
                    if r["ts"] >= first_ex["ts"] - 5 and (committed(r) or -1) >= hb
                ),
                None,
            )
            by_node[n] = c
        seen = [c for c in by_node.values() if c is not None]
        commit_ts = min(seen) if seen else None
        excl.update(
            boundary_height=hb,
            delta_node=first_ex["node"],
            delta_ts=first_ex["ts"],
            kill_to_delta_s=round(first_ex["ts"] - kill_ts, 3),
            exclude=first_ex["exclude"],
            readmit=first_ex["readmit"],
            excluded=first_ex["excluded"],
            committed_ts=commit_ts,
            committed_ts_by_node=by_node,
            kill_to_exclusion_s=None
            if commit_ts is None
            else round(commit_ts - kill_ts, 3),
            committed_resolution_s=1.0,
        )
        if commit_ts is None:
            problems.append(
                "exclusion delta at boundary height %s logged but no survivor's committed height "
                "reached it in sampler.csv" % hb
            )
        elif commit_ts > t1:
            problems.append("the exclusion committed after the load window (T1)")
    else:
        excl["reason"] = (
            "no '%s' line with exclude >= 1 after the kill on any survivor (K = 0, a binary "
            "without dead-leader skip, or the load window ended before the next epoch plan)"
            % DELTA_MSG
        )

    # heights at the edges (val0 = the headline node; every survivor in per_node)
    def height_at(n, ts):
        sel = [r for r in rows.get(n, []) if r["ts"] <= ts]
        return None if not sel else committed(sel[-1])

    heights = {
        "killed_node_height_at_kill": (kill.get("pre_kill") or {}).get("block_height"),
        "committed_at_kill_by_node": {n: height_at(n, kill_ts) for n in nodes},
    }
    if commit_ts is not None:
        heights["committed_at_exclusion_by_node"] = {
            n: height_at(n, commit_ts) for n in nodes
        }
        h0 = heights["committed_at_kill_by_node"].get("val0")
        if h0 is not None and excl.get("boundary_height") is not None:
            heights["blocks_kill_to_boundary_val0"] = int(excl["boundary_height"] - h0)
    split = commit_ts if commit_ts is not None and commit_ts <= t1 else None
    bounds = {
        "pre_kill": [t0, kill_ts],
        "kill_to_exclusion": [kill_ts, split if split is not None else t1],
        "post_exclusion": [split, t1] if split is not None else None,
    }
    per_node = {}
    for n in nodes:
        rs = [r for r in rows[n] if t0 - 5 <= r["ts"] <= t1]
        w = {"pre_kill": rates(window(rs, t0, kill_ts, prev_boundary=True))}
        w["kill_to_exclusion"] = rates(
            window(rs, kill_ts, bounds["kill_to_exclusion"][1])
        )
        if split is not None:
            w["post_exclusion"] = rates(window(rs, split, t1))
        else:
            w["post_exclusion"] = dict(
                {k: None for k, _, _, _ in RATES},
                samples=0,
                reason="no exclusion committed inside the load window",
            )
        per_node[n] = w
    timeouts = {
        "last_increment_ts_by_node": {
            n: last_increment(rows[n], kill_ts, t1) for n in nodes
        }
    }
    timeouts["last_increment_after_kill_s_by_node"] = {
        n: (None if v is None else round(v - kill_ts, 3))
        for n, v in timeouts["last_increment_ts_by_node"].items()
    }
    return {
        "killed_node": dead,
        "kill_ts": kill_ts,
        "kill_at_s": kill.get("kill_at_s"),
        "survivors": nodes,
        "dl_k": dl_k,
        "dl_jcap": dl_jcap,
        "exclusion": excl,
        "heights": heights,
        "windows": bounds,
        "boundary": "shared: the last sample at or before a window's start begins it (per node)",
        "per_node": per_node,
        "headline": per_node.get("val0"),
        "timeouts": timeouts,
        "deltas_after_kill": after,
        "logs_read": logs_read,
        "problems": problems,
    }


def fmt(v, nd=1):
    return "-" if v is None else ("%.*f" % (nd, v))


def line_of(d):
    e = d["exclusion"]
    h = d.get("headline") or {}

    def w3(k, nd):
        return "/".join(
            fmt((h.get(w) or {}).get(k), nd)
            for w in ("pre_kill", "kill_to_exclusion", "post_exclusion")
        )

    if e["found"]:
        ex = (
            "exclusion delta at boundary h=%s (+%ss, %s), committed +%ss (exclude=%s excluded=%s)"
            % (
                e["boundary_height"],
                fmt(e["kill_to_delta_s"]),
                e["delta_node"],
                fmt(e.get("kill_to_exclusion_s")),
                e["exclude"],
                e["excluded"],
            )
        )
    else:
        ex = "NO exclusion observed"
    last = [
        v
        for v in d["timeouts"]["last_increment_after_kill_s_by_node"].values()
        if v is not None
    ]
    return (
        "dead-leader window: kill %s at bench+%ss h=%s; %s; last survivor timeout +%ss; val0 pre/kill->excl/post: "
        "blk/s %s matched/s %s timeouts/min %s%s"
        % (
            d["killed_node"],
            d["kill_at_s"],
            d["heights"]["killed_node_height_at_kill"],
            ex,
            fmt(max(last) if last else None),
            w3("blk_s", 2),
            w3("matched_s", 0),
            w3("timeouts_per_min", 1),
            "; problems: " + "; ".join(d["problems"]) if d["problems"] else "",
        )
    )


def main(argv=None):
    p = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    p.add_argument("--dir", required=True)
    p.add_argument("--t-bench0", type=float, required=True)
    p.add_argument("--t-bench1", type=float, required=True)
    p.add_argument(
        "--dl-k",
        default=None,
        help="the genesis consensus.leader_miss_threshold (recorded)",
    )
    p.add_argument(
        "--dl-jcap",
        default=None,
        help="the genesis consensus.leader_exclusion_cap (recorded)",
    )
    a = p.parse_args(argv)
    try:
        d = report(a.dir, a.t_bench0, a.t_bench1, a.dl_k or None, a.dl_jcap or None)
    except (OSError, ValueError) as e:
        print("dead-leader window: FAILED: %s" % e)
        return 2
    with open(os.path.join(a.dir, "dl-window.json"), "w") as f:
        json.dump(d, f, indent=1)
    sp = os.path.join(a.dir, "summary.json")
    if os.path.exists(sp):
        with open(sp) as f:
            s = json.load(f)
        s["dead_leader"] = {
            k: d[k]
            for k in (
                "killed_node",
                "kill_at_s",
                "dl_k",
                "dl_jcap",
                "exclusion",
                "heights",
                "windows",
                "headline",
                "timeouts",
                "problems",
            )
        }
        tmp = sp + ".dl.tmp"
        with open(tmp, "w") as f:
            json.dump(s, f, indent=1)
        os.replace(tmp, sp)
    print(line_of(d))
    return 0


if __name__ == "__main__":
    sys.exit(main())
