#!/usr/bin/env python3
"""ozarchy-bd-analysis.py: campaign A (bad-debt batch cost), cand main@35e69b3 vs main 92a02ed, N=4 b900 300mk.
Reuses cell()/checks() from ozarchy-mif2-analysis.py (as ozarchy-mif3-analysis.py does)."""
R = "/home/oz/bench-results-matched/"
src = open(R + "ozarchy-mif2-analysis.py").read()
src = src[: src.rindex('{"A": A, "B": B}')]
g = {"__name__": "mif2"}
exec(compile(src, "mif2", "exec"), g)
cell, checks, f = g["cell"], g["checks"], g["f"]
P = "ozarchy-bd-300m-"
L = [(P + t, t) for t in ("crab-warm", "crab-r1", "main-r1", "crab-r2", "main-r2")]
L += [("ozarchy-p2s0-300m-crab-r1", "p2s0 crab-r1 (59fa407)"), ("ozarchy-p2s0-300m-main-r1", "p2s0 main-r1")]
cs = [cell(l, t) for l, t in L]
print("## Checks"); checks(cs)
print("\n| cell | matched/s | best60 | native blk/s | open_limit share | engine ms/1k | margin ms/1k | match ms/1k | CPU-s per 1M fills | age_commit p50 / p95 (ms) |")
print("|---|---|---|---|---|---|---|---|---|---|")
for c in cs:
    if "mps" not in c:
        print(f"| {c['tag']} | no summary (rc {c['rc']}) |"); continue
    a = c["age"]
    print(f"| {c['tag']} | {f(c['mps'])} | {f(c['best60'])} | {f(c['nblk'], 3)} | {f(c['ol'], 2, True)} | {c['eng']:.2f} | {f(c.get('margin'), 2)} | "
          f"{f(c.get('match'), 2)} | {f(c['cpu1m'], 1)} | {f(a[0])} / {f(a[1])} |")
by = {c["tag"]: c for c in cs}
keys = ("mps", "best60", "nblk", "eng", "margin", "match", "cpu1m")
for a, b in (("crab-r1", "main-r1"), ("crab-r2", "main-r2"), ("p2s0 crab-r1 (59fa407)", "p2s0 main-r1"), ("crab-r1", "p2s0 crab-r1 (59fa407)"), ("main-r1", "p2s0 main-r1")):
    print(f"{a} / {b}: " + "  ".join(f"{k} {by[a][k] / by[b][k]:.3f}x" for k in keys if by[a].get(k) and by[b].get(k)))
m = lambda t, k: (by["crab-" + t][k], by["main-" + t][k])
import statistics
cr = statistics.mean(by[t]["mps"] for t in ("crab-r1", "crab-r2")); mr = statistics.mean(by[t]["mps"] for t in ("main-r1", "main-r2"))
print(f"mean r1+r2: crab {cr:,.0f} main {mr:,.0f} ratio {cr / mr:.3f}x")
