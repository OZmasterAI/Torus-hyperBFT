#!/usr/bin/env python3
"""phasecmp.py <prefix> <armX> <armY>: mean over r1/r2 and val0-2 of every timed phase (ms per native block and per 1k fills)."""
import json, sys, statistics as S
P, X, Y = sys.argv[1:4]
def flat(c, v):
    s = json.load(open(f"{P}-{c}/summary.json")); p = s["phase_by_node"][v]
    f = p["fills_per_native_block"]; out = {}
    for ph, d in p["phases"].items():
        if not isinstance(d, dict): continue
        for k, val in d.items():
            if isinstance(val, (int, float)) and (k == "ms" or k.endswith("_ms")):
                out[f"{ph}.{k}"] = val
    for k in ("chain_ms", "pipelined_ms", "handoff_wait_ms", "block_ms", "commit_persist_ms_avg", "end_resident_wait_ms_p50", "wall_ms_per_native_block"):
        if isinstance(p.get(k), (int, float)): out[k] = p[k]
    out["_fills"] = f
    return out
def arm(a):
    rows = [flat(f"{a}-r{r}", v) for r in (1, 2) for v in ("val0", "val1", "val2")]
    keys = set.intersection(*(set(r) for r in rows))
    return {k: S.mean(r[k] for r in rows) for k in keys}, {k: S.mean(r[k] / r["_fills"] * 1000 for r in rows) for k in keys}
xb, x1k = arm(X); yb, y1k = arm(Y)
print(f"fills per native block: {X} {xb['_fills']:.0f}  {Y} {yb['_fills']:.0f}  ({yb['_fills']/xb['_fills']:.3f}x)")
print(f"{'phase':42s} {X+' ms/blk':>10s} {Y+' ms/blk':>10s} {'d ms/blk':>9s} {'ratio':>6s} | {X+' /1k':>8s} {Y+' /1k':>8s} {'d /1k':>7s} {'ratio':>6s}")
for k in sorted((k for k in xb if not k.startswith("_")), key=lambda k: -(yb[k] - xb[k])):
    if max(abs(xb[k]), abs(yb[k])) < 0.05: continue
    r = yb[k] / xb[k] if xb[k] else float("nan"); r1 = y1k[k] / x1k[k] if x1k[k] else float("nan")
    print(f"{k:42s} {xb[k]:10.2f} {yb[k]:10.2f} {yb[k]-xb[k]:9.2f} {r:6.3f} | {x1k[k]:8.3f} {y1k[k]:8.3f} {y1k[k]-x1k[k]:7.3f} {r1:6.3f}")
