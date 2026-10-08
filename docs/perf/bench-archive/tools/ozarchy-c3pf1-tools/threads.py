#!/usr/bin/env python3
"""threads.py <label>...: val0 per-thread-group CPU in ms per 1k val0 fills over [t_bench0, t_drain]
from tasks.txt (task-sampler.py). Groups by comm prefix; 'exited' = process delta minus live-thread deltas."""

import collections
import json
import re
import sys

R = "/home/oz/bench-results-matched/"
GROUPS = [
    ("rpc", "rpc-worker"),
    ("exec", "torus-execution"),
    ("tokio", "tokio-rt-worker"),
    ("ingress", "torus-ingress-v"),
    ("gossip", "torus-gossip-ve"),
    ("flush", "torus-flush-wor"),
    ("rocksdb", "rocksdb"),
]


def grp(c):
    for g, p in GROUPS:
        if c.startswith(p):
            return g
    return "other"


def row(lab):
    s = json.load(open(R + lab + "/summary.json"))
    t0, t1 = s["timing"]["t_bench0"], s["timing"]["t_drain"]
    fills = s["funnel_by_node"]["val0"]["delta_orders_matched_total"]
    by = collections.defaultdict(dict)
    comm = {}
    for line in open(R + lab + "/tasks.txt"):
        ts, tid, c, u, st = line.split()
        ts = int(ts)
        if t0 - 1 <= ts <= t1 + 1:
            by[tid][ts] = (int(u), int(st))
            comm[tid] = c
    pk = sorted(by["PROC"])
    a, b = by["PROC"][pk[0]], by["PROC"][pk[-1]]
    k_ms = 10 * 1000 / fills  # ticks (10 ms) -> ms CPU per 1k fills
    g, gs = collections.Counter(), collections.Counter()
    live = 0
    for tid, m in by.items():
        if tid == "PROC":
            continue
        ks = sorted(m)
        x, y = m[ks[0]], m[ks[-1]]
        if ks[0] > pk[0]:
            x = (0, 0)  # thread born inside the window
        du, ds = y[0] - x[0], y[1] - x[1]
        g[grp(comm[tid])] += (du + ds) * k_ms
        gs[grp(comm[tid])] += ds * k_ms
        live += du + ds
    exited = (b[0] - a[0] + b[1] - a[1]) - live
    met = open(R + lab + "/metrics-after-val0.txt").read()
    bp = re.search(
        r'torus_rpc_submit_admit_rejects_total\{reason="backlog_preverify"\} ([0-9.e+]+)',
        met,
    )
    pf = re.search(
        r'torus_rpc_submit_admit_rejects_total\{reason="pool_full"\} ([0-9.e+]+)', met
    )
    return {
        "cell": lab,
        "fills_M": round(fills / 1e6, 2),
        "user": round((b[0] - a[0]) * k_ms, 2),
        "sys": round((b[1] - a[1]) * k_ms, 2),
        "total": round((b[0] - a[0] + b[1] - a[1]) * k_ms, 2),
        "rpc": round(g["rpc"], 2),
        "rpc_sys": round(gs["rpc"], 2),
        "exec": round(g["exec"], 2),
        "exec_sys": round(gs["exec"], 2),
        **{
            n: round(g[n], 2)
            for n in ("tokio", "ingress", "gossip", "flush", "rocksdb", "other")
        },
        "exited": round(exited * k_ms, 2),
        "backlog_preverify": int(float(bp.group(1))) if bp else 0,
        "pool_full": int(float(pf.group(1))) if pf else 0,
        "window": f"{pk[0]}..{pk[-1]} vs {t0}..{t1}",
    }


if __name__ == "__main__":
    for lab in sys.argv[1:]:
        print(json.dumps(row(lab)))
