"""a-jemalloc-rss.py (ozarchy-p3cloidec): the analyst's inline jemalloc query (it wrote jemalloc-rss.txt through tee),
saved for the archive with the query unchanged and only the paths parameterised. Per cell: torus_jemalloc_allocated_bytes
and torus_jemalloc_resident_bytes from mem-samples.txt inside the bench window (t0..t1 of cells-all.json), mean of the
per-node window means and mean of the per-node peaks, GB; then per arm (w0 and q1-F0 left out, as the analyst did).
Usage: python3 -I a-jemalloc-rss.py [CELLS.json [PREFIX]]   (stdout = jemalloc-rss.txt)"""
import json, statistics as st, sys
C = json.load(open(sys.argv[1] if len(sys.argv) > 1 else "cells-all.json"))
P = sys.argv[2] if len(sys.argv) > 2 else "/home/oz/bench-results-matched/ozarchy-p3cloidec-300m-"
K = ("torus_jemalloc_allocated_bytes", "torus_jemalloc_resident_bytes")
res = {}
for t, x in C.items():
    t0, t1 = x["t0"], x["t1"]
    acc = {k: {} for k in K}
    for line in open(P + t + "/mem-samples.txt"):
        f = line.split()
        if len(f) < 4 or f[0] != "G" or not (t0 <= float(f[1]) <= t1): continue
        for kv in f[3:]:
            k, _, v = kv.rpartition("=")
            if k in K:
                acc[k].setdefault(f[2], []).append(float(v))
    r = {k: (st.mean(st.mean(v) for v in acc[k].values()) / 1e9, st.mean(max(v) for v in acc[k].values()) / 1e9) for k in K if acc[k]}
    res[t] = (x["arm"], r)
    print(f"{t:8} {x['arm']:5} " + " ".join(f"{k.split('_')[2]} mean {a:.3f} peak {b:.3f} GB" for k, (a, b) in r.items()))
for arm in ("F100", "B100", "B0", "F0"):
    ts = [t for t, (a, _) in res.items() if a == arm and t not in ("w0", "q1-F0")]
    for k in K:
        xs = [res[t][1][k][0] for t in ts]
        print(f"{arm} {k} mean-of-window-means {st.mean(xs):.3f} sd {(st.stdev(xs) if len(xs) > 1 else 0):.3f} n={len(xs)}")
