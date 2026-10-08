#!/usr/bin/env python3
"""ozarchy-mif3-analysis.py: crab 59fa407 N sweep at b900 (ozarchy, 2026-10-06).
Reuses cell()/checks() from ozarchy-mif2-analysis.py. Crab N=2 = mif2 crab-r1/-r2; N=4/8 = mif3 cells.
Main reference (mif/mif2 sweep): N=2 173.0k, N=4 174.3k (r1-r3), N=8 176.3k (best), N=16 172.3k."""

import statistics

R = "/home/oz/bench-results-matched/"
src = open(R + "ozarchy-mif2-analysis.py").read()
src = src[: src.rindex('{"A": A, "B": B}')]
g = {"__name__": "mif2"}
exec(compile(src, "mif2", "exec"), g)
cell, checks, f = g["cell"], g["checks"], g["f"]

L = [
    ("ozarchy-mif2-300m-crab-warm", "N=2 warm 60s (mif2)"),
    ("ozarchy-mif2-300m-crab-r1", "N=2 r1 (mif2)"),
    ("ozarchy-mif2-300m-crab-r2", "N=2 r2 (mif2)"),
    ("ozarchy-mif3-300m-crab-warm", "N=4 warm 60s"),
    ("ozarchy-mif3-300m-crab-n4-b900", "N=4"),
    ("ozarchy-mif3-300m-crab-n8-b900", "N=8"),
]
cs = [cell(l, t) for l, t in L]
print("## Checks")
checks(cs)
print("\n## crab 59fa407, b900")
print(
    "| cell | matched/s | best60 | native blk/s | actions / native blk | open_limit share | cancel-all share | engine ms/1k | margin ms/1k | match ms/1k | CPU-s per 1M fills | age_commit p50 / p95 (ms) | released committed / refused / timeout | in flight at end | tail fetched / err / missed |"
)
print("|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|")
for c in cs:
    if "mps" not in c:
        print(f"| {c['tag']} | no summary (rc {c['rc']}) |")
        continue
    i, a = c["inf"] or {}, c["age"]
    rel = (
        f"{i['released_committed']:,} / {i['released_refused']:,} / {i['released_timeout']:,}"
        if i
        else "-"
    )
    print(
        f"| {c['tag']} | {f(c['mps'])} | {f(c['best60'])} | {f(c['nblk'], 3)} | {f(c['apb'], 0)} | {f(c['ol'], 2, True)} | {f(c['ca_sh'], 1, True)} | "
        f"{c['eng']:.2f} | {f(c.get('margin'), 2)} | {f(c.get('match'), 2)} | {f(c['cpu1m'], 1)} | {f(a[0])} / {f(a[1])} | {rel} | {i.get('in_flight_at_end', '-')} | "
        f"{i.get('tail_fetched', '-')} / {i.get('tail_errors', '-')} / {i.get('tail_missed', '-')} |"
    )
by = {c["tag"]: c for c in cs}
n2 = statistics.mean(by[k]["mps"] for k in ("N=2 r1 (mif2)", "N=2 r2 (mif2)"))
pts = {"2 (r1/r2 mean)": n2, "4": by["N=4"].get("mps"), "8": by["N=8"].get("mps")}
cb = max(v for v in pts.values() if v)
main = {"2": 173.0e3, "4": 174.3e3, "8": 176.3e3, "16": 172.3e3}
mb = max(main.values())
print(f"\ncrab best {cb:,.0f}; main best {mb:,.0f}")
print(
    "| N | crab matched/s | % crab best | % main best | main matched/s | main % own best |\n|---|---|---|---|---|---|"
)
for k, v in pts.items():
    m = main[k.split()[0]]
    print(
        f"| {k} | {f(v)} | {v / cb:.1%} | {v / mb:.1%} | {m:,.0f} | {m / mb:.1%} |"
        if v
        else f"| {k} | - |"
    )
ok = [
    k.split()[0]
    for k, v in pts.items()
    if v and v >= 0.97 * cb and main[k.split()[0]] >= 0.97 * mb
]
print(
    f"\nfinal N (smallest N with both arms within 3% of own best): {ok[0] if ok else 'none'}"
)
