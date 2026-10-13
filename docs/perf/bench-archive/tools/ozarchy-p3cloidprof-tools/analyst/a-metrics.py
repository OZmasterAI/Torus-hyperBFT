#!/usr/bin/env python3
"""a-metrics.py <cell_dir>...: exec phase metrics per arm (whole cell, metrics-before/after-val{0,1,2}.txt, mean over the 3 nodes)
and over the val1 W1 profile window (prof-metrics-w1-*). Per cell: ms per native block and ms per 1k fills of
torus_exec_engine_untimed_seconds (serial cloid pre-pass + absorb land here), exec_phase1_actions_seconds (cancel-all forget_cloid),
exec_phase_match_seconds (duplicate check in place + maker-fill cleanup), plus engine, margin, settle. Paired c100 - c0 per round."""

import os
import re
import sys

PH = (
    "engine",
    "engine_untimed",
    "phase1_actions",
    "phase_margin",
    "phase_match",
    "phase_settle",
)


def met(p):
    d = {}
    for l in open(p):
        if l.startswith("#") or not l.strip():
            continue
        k, v = l.rsplit(" ", 1)
        try:
            d[k] = d.get(k, 0) + float(v)
        except ValueError:
            pass
    return d


def mval(d, name):
    return sum(v for k, v in d.items() if k == name or k.startswith(name + "{"))


def calc(a, b):
    fills = mval(b, "torus_orders_matched_total") - mval(
        a, "torus_orders_matched_total"
    )
    blk = mval(b, "torus_exec_native_blocks_total") - mval(
        a, "torus_exec_native_blocks_total"
    )
    r = {"fills": fills, "blk": blk, "fpb": fills / blk if blk else 0}
    for p in PH:
        s = mval(b, f"torus_exec_{p}_seconds_sum") - mval(
            a, f"torus_exec_{p}_seconds_sum"
        )
        r[p + "_blk"] = s / blk * 1000 if blk else 0
        r[p + "_1k"] = s / fills * 1e6 if fills else 0
    return r


rows = {}
for c in sys.argv[1:]:
    n = os.path.basename(c.rstrip("/"))
    m = re.search(r"-(r\d+)-(c0|c100)$", n)
    if not m:
        continue
    per = [
        calc(
            met(f"{c}/metrics-before-val{v}.txt"), met(f"{c}/metrics-after-val{v}.txt")
        )
        for v in range(3)
    ]
    whole = {k: sum(p[k] for p in per) / 3 for k in per[0]}
    w1 = (calc(met(f"{c}/prof-metrics-w1-before.txt"), met(f"{c}/prof-metrics-w1-after.txt"))
          if os.path.exists(f"{c}/prof-metrics-w1-after.txt") else per[1])
    rows[n] = dict(rnd=m.group(1), arm=m.group(2), whole=whole, val1=per[1], w1=w1)

keys = ["fpb"] + [p + s for p in PH for s in ("_blk", "_1k")]


def mean(x):
    return sum(x) / len(x) if x else float("nan")


for scope, title in (
    ("whole", "whole cell, mean over val0-2"),
    ("val1", "whole cell, val1 only (profiled node)"),
    ("w1", "val1 W1 profile window (perf record running)"),
):
    print(f"\n== {title}: ms per native block (_blk) and ms per 1k fills (_1k)")
    print(
        f"{'metric':22} {'c0':>9} {'c100':>9} {'delta':>9} {'%':>7}  paired per round"
    )
    for k in keys:
        a0 = [r[scope][k] for r in rows.values() if r["arm"] == "c0"]
        a1 = [r[scope][k] for r in rows.values() if r["arm"] == "c100"]
        pr = []
        for rnd in sorted(set(r["rnd"] for r in rows.values())):
            x = {r["arm"]: r[scope][k] for r in rows.values() if r["rnd"] == rnd}
            if "c0" in x and "c100" in x:
                pr.append(f"{rnd} {x['c100'] - x['c0']:+.3f}")
        m0, m1 = mean(a0), mean(a1)
        print(
            f"{k:22} {m0:9.3f} {m1:9.3f} {m1 - m0:+9.3f} {((m1 - m0) / m0 * 100 if m0 else 0):+7.1f}  "
            + "  ".join(pr)
        )
print(
    "\nper cell (whole, mean val0-2): "
    + "; ".join(
        f"{n} fills/blk {r['whole']['fpb']:.0f} untimed/1k {r['whole']['engine_untimed_1k']:.3f} p1/1k {r['whole']['phase1_actions_1k']:.3f} "
        f"match/1k {r['whole']['phase_match_1k']:.3f}"
        for n, r in rows.items()
    )
)
