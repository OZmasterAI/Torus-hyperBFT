"""p23-exited.py: unprofiled cells: val0 CPU of short-lived threads (process minus threads seen by the 1 Hz task sampler)
over [t_bench0, t_drain], user and sys, per native block and per 1M fills; spawns per block from the profiled cells."""

import collections
import json
import sys

R = "/home/oz/bench-results-matched/ozarchy-p23b-300m-"
SPAWNS_PER_BLK = float(sys.argv[1])
for tag in sys.argv[2:]:
    s = json.load(open(R + tag + "/summary.json"))
    t0, t1 = s["timing"]["t_bench0"], s["timing"]["t_drain"]
    fills = s["funnel_by_node"]["val0"]["delta_orders_matched_total"]
    by = collections.defaultdict(dict)
    comm = {}
    for line in open(R + tag + "/tasks.txt"):
        ts, tid, c, u, st = line.split()
        ts = int(ts)
        if t0 - 1 <= ts <= t1 + 1:
            by[tid][ts] = (int(u), int(st))
            comm[tid] = c
    pk = sorted(by["PROC"])
    a, b = by["PROC"][pk[0]], by["PROC"][pk[-1]]
    lu = ls = 0
    eu = es = 0
    for tid, m in by.items():
        if tid == "PROC":
            continue
        ks = sorted(m)
        x, y = m[ks[0]], m[ks[-1]]
        if ks[0] > pk[0]:
            x = (0, 0)
        lu += y[0] - x[0]
        ls += y[1] - x[1]
        if comm[tid].startswith("torus-execution"):
            eu += y[0] - x[0]
            es += y[1] - x[1]
    xu = (b[0] - a[0] - lu) / 100
    xs = (b[1] - a[1] - ls) / 100
    pu, ps = (b[0] - a[0]) / 100, (b[1] - a[1]) / 100
    win = pk[-1] - pk[0]
    blk = s["headline"]["native_blk_s"] * (t1 - t0)
    fpb = fills / blk
    sp = SPAWNS_PER_BLK * blk
    print(
        f"{tag}: window {win} s, fills {fills / 1e6:.2f}M, native blk ~{blk:.0f} ({fpb:.0f} fills/blk) | process user {pu:.1f} sys {ps:.1f} s "
        f"({1e6 * ps / fills:.2f} sys CPU-s/1M) | exited-thread user {xu:.1f} sys {xs:.1f} s = sys {1e6 * xs / fills:.3f} CPU-s/1M, "
        f"{1e6 * xs / sp:.0f} us sys per spawn (at {SPAWNS_PER_BLK}/blk) | exec live-thread names (torus-execution*) user {eu / 100:.1f} sys {es / 100:.1f} s"
    )
