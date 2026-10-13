"""a-ec-metrics.py (ozarchy-p3cloidec analyst, new script; same definitions as section 60 a-metrics.py calc()):
exec phase ms per native block / per 1k fills per cell (whole cell mean val0-2, and val1 W1 window), per-arm mean (sd),
arm deltas with pooled sd, ABBA paired F-B deltas. Read-only on cells. Usage: a-ec-metrics.py PREFIX"""
import math, os, statistics as st, sys
P = sys.argv[1]
PH = ("engine", "engine_untimed", "phase1_actions", "phase_margin", "phase_match", "phase_settle")
CELLS = {"r1-F100": "F100", "r2-B100": "B100", "r3-B100": "B100", "r4-F100": "F100", "r5-B100": "B100", "r6-F100": "F100",
         "r7-F100": "F100", "r8-B100": "B100", "q2-B0": "B0", "q3-B0": "B0", "q4-F0": "F0", "q1-F0": "F0-BAD", "w0": "B100-warm"}
def met(p):
    d = {}
    for l in open(p):
        if l.startswith("#") or not l.strip():
            continue
        k, v = l.rsplit(" ", 1)
        try: d[k] = d.get(k, 0) + float(v)
        except ValueError: pass
    return d
def mval(d, n): return sum(v for k, v in d.items() if k == n or k.startswith(n + "{"))
def calc(a, b):
    f = mval(b, "torus_orders_matched_total") - mval(a, "torus_orders_matched_total")
    k = mval(b, "torus_exec_native_blocks_total") - mval(a, "torus_exec_native_blocks_total")
    r = {"fpb": f / k if k else 0}
    for p in PH:
        s = mval(b, f"torus_exec_{p}_seconds_sum") - mval(a, f"torus_exec_{p}_seconds_sum")
        r[p + "_blk"] = s / k * 1000 if k else 0
        r[p + "_1k"] = s / f * 1e6 if f else 0
    return r
rows = {}
for t, arm in CELLS.items():
    c = P + t
    per = [calc(met(f"{c}/metrics-before-val{v}.txt"), met(f"{c}/metrics-after-val{v}.txt")) for v in range(3)]
    whole = {k: sum(p[k] for p in per) / 3 for k in per[0]}
    w1 = calc(met(f"{c}/prof-metrics-w1-before.txt"), met(f"{c}/prof-metrics-w1-after.txt"))
    rows[t] = dict(arm=arm, whole=whole, w1=w1)
keys = ["fpb"] + [p + s for p in PH for s in ("_1k", "_blk")]
def sd(x): return st.stdev(x) if len(x) > 1 else float("nan")
for scope in ("whole", "w1"):
    print(f"\n== {scope}: per arm mean (sd) [n]")
    arms = ("F100", "B100", "F0", "B0")
    print(f"{'metric':22}" + "".join(f"{a:>20}" for a in arms))
    for k in keys:
        print(f"{k:22}" + "".join(f"{st.mean(v):11.3f} ({sd(v):6.3f})" for v in ([r[scope][k] for r in rows.values() if r['arm'] == a] for a in arms)))
    print(f"\n-- {scope}: deltas, pooled sd (of a single cell), step/sd; ABBA paired F-B (r1-r2, r4-r3, r6-r5, r7-r8)")
    for k in keys:
        g = {a: [r[scope][k] for r in rows.values() if r["arm"] == a] for a in arms}
        out = []
        for x, y in (("B100", "F100"), ("B0", "B100"), ("B0", "F0"), ("F0", "F100")):
            a, b = g[x], g[y]
            dfs = (len(a) - 1) + (len(b) - 1)
            ps = math.sqrt(((len(a) - 1) * (sd(a) ** 2 if len(a) > 1 else 0) + (len(b) - 1) * (sd(b) ** 2 if len(b) > 1 else 0)) / dfs) if dfs else float("nan")
            d = st.mean(b) - st.mean(a)
            out.append(f"{y}-{x} {d:+.3f} ({100 * d / st.mean(a) if st.mean(a) else 0:+.1f}%) sd {ps:.3f} s/sd {d / ps if ps else float('nan'):+.1f}")
        pr = [rows[f][scope][k] - rows[b][scope][k] for f, b in (("r1-F100", "r2-B100"), ("r4-F100", "r3-B100"), ("r6-F100", "r5-B100"), ("r7-F100", "r8-B100"))]
        h = 3.182 * st.stdev(pr) / 2
        out.append(f"paired {st.mean(pr):+.3f} [{st.mean(pr) - h:+.3f},{st.mean(pr) + h:+.3f}]")
        print(f"{k:22} " + " | ".join(out))
print("\nper cell (whole mean val0-2 | val1 W1): fpb, untimed/1k, engine/1k, phase1/1k, match/1k, engine/blk")
for t, r in rows.items():
    w, x = r["whole"], r["w1"]
    print(f"{t:10} {r['arm']:10} {w['fpb']:8.0f} {w['engine_untimed_1k']:.3f} {w['engine_1k']:.3f} {w['phase1_actions_1k']:.3f} {w['phase_match_1k']:.3f} {w['engine_blk']:6.1f} | "
          f"{x['fpb']:8.0f} {x['engine_untimed_1k']:.3f} {x['engine_1k']:.3f} {x['phase1_actions_1k']:.3f} {x['phase_match_1k']:.3f} {x['engine_blk']:6.1f}")
