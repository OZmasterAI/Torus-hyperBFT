"""a-writeup.py (ozarchy-p3cloidec, results doc section 63; written for the write-up, read-only on the cells).
Usage: python3 -I a-writeup.py CELLS.json PREFIX OUTDIR
  CELLS.json  a-cells.py output with all 13 cells (cells-all.json); PREFIX = .../ozarchy-p3cloidec-300m-
Writes OUTDIR/paired.txt: the c100 ABBA pairs F100 - B100 (r1-r2, r4-r3, r6-r5, r7-r8) for every a-arms.py row() metric
(+ device writes), in a-paired.py's format (a-paired.py pairs cells of one round, rK-BASE with rK-OTHER; here every
round has one cell, so the pairs are listed explicitly). Writes OUTDIR/writeup.txt: recovery of B's own c100 - c0 gap,
the c0 regression check with q1-F0 COUNTED (F0 n = 2: its rc 2 / DISAGREE is the mark-only digest artifact), and the
engine phase timers per 1k fills (a-ec-metrics.py's calc(), whole cell, mean of val0-2) with q1-F0 counted.
a-arms.py (section 60's, unchanged) must sit next to this script: its helpers are exec'd as a-paired.py does."""

import json
import math
import os
import statistics as st
import sys

T975 = {1: 12.706, 2: 4.303, 3: 3.182, 4: 2.776, 5: 2.571}
PAIRS = (
    ("r1-F100", "r2-B100"),
    ("r4-F100", "r3-B100"),
    ("r6-F100", "r5-B100"),
    ("r7-F100", "r8-B100"),
)
C100 = {
    "F100": ["r1-F100", "r4-F100", "r6-F100", "r7-F100"],
    "B100": ["r2-B100", "r3-B100", "r5-B100", "r8-B100"],
}
C0 = {"F0": ["q1-F0", "q4-F0"], "B0": ["q2-B0", "q3-B0"]}
PH = (
    "engine",
    "engine_untimed",
    "phase1_actions",
    "phase_margin",
    "phase_match",
    "phase_settle",
)


def arms_helpers():
    src = (
        open(os.path.join(os.path.dirname(os.path.abspath(__file__)), "a-arms.py"))
        .read()
        .rsplit("\nmain()", 1)[0]
    )
    g = {"__name__": "a_arms"}
    exec(compile(src, "a-arms.py", "exec"), g)
    return g


def met(p):  # a-ec-metrics.py's met / mval / calc, unchanged
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


def mval(d, n):
    return sum(v for k, v in d.items() if k == n or k.startswith(n + "{"))


def calc(a, b):
    f = mval(b, "torus_orders_matched_total") - mval(a, "torus_orders_matched_total")
    k = mval(b, "torus_exec_native_blocks_total") - mval(
        a, "torus_exec_native_blocks_total"
    )
    r = {"fpb": f / k if k else 0}
    for p in PH:
        s = mval(b, f"torus_exec_{p}_seconds_sum") - mval(
            a, f"torus_exec_{p}_seconds_sum"
        )
        r[p + "_blk"] = s / k * 1000 if k else 0
        r[p + "_1k"] = s / f * 1e6 if f else 0
    return r


def sd(x):
    return st.stdev(x) if len(x) > 1 else float("nan")


def main():
    cj, prefix, out = sys.argv[1:4]
    g = arms_helpers()
    row, welch = g["row"], g["welch"]
    C = json.load(open(cj))
    for t, x in C.items():
        x["tag"] = t
    R = {t: row(C[t], prefix) for t in C if t != "w0"}
    keys = list(R["r1-F100"].keys())

    # paired.txt (a-paired.py format; BASE = B100, OTHER = F100)
    with open(os.path.join(out, "paired.txt"), "w") as fh:
        p = lambda s="": print(s, file=fh)
        p(
            f"paired rounds: {[f + '/' + b for f, b in PAIRS]}  (F100 - B100); CI = mean +- t(n-1) * sd / sqrt(n)"
        )
        p(
            f"{'metric':12s} {'B100 mean':>12s} {'F100 mean':>12s} {'delta':>10s} {'95% CI':>22s} {'%':>8s} {'% CI':>20s}  per-round deltas"
        )
        for k in keys:
            a = [R[b][k] for _, b in PAIRS]
            b = [R[f][k] for f, _ in PAIRS]
            if any(v != v for v in a + b):
                continue
            d = [y - x for x, y in zip(a, b)]
            n = len(d)
            md = st.mean(d)
            h = T975[n - 1] * st.stdev(d) / math.sqrt(n)
            ma = st.mean(a)
            pc = (lambda v: 100 * v / ma) if ma else (lambda v: float("nan"))
            p(
                f"{k:12s} {ma:12.3f} {st.mean(b):12.3f} {md:+10.3f} [{md - h:+9.3f}, {md + h:+9.3f}] {pc(md):+7.2f}% "
                f"[{pc(md - h):+7.2f}, {pc(md + h):+7.2f}]  {' '.join(f'{v:+.3f}' for v in d)}"
            )

    E = {}
    for t in R:
        c = prefix + t
        per = [
            calc(
                met(f"{c}/metrics-before-val{v}.txt"),
                met(f"{c}/metrics-after-val{v}.txt"),
            )
            for v in range(3)
        ]
        E[t] = {k: sum(q[k] for q in per) / 3 for k in per[0]}

    with open(os.path.join(out, "writeup.txt"), "w") as fh:
        p = lambda s="": print(s, file=fh)
        # recovery of B's own gap
        f = [R[t]["ms"] for t in C100["F100"]]
        b = [R[t]["ms"] for t in C100["B100"]]
        b0 = [R[t]["ms"] for t in C0["B0"]]
        d = [R[x]["ms"] - R[y]["ms"] for x, y in PAIRS]
        md, h = st.mean(d), T975[3] * st.stdev(d) / 2
        gap = st.mean(b0) - st.mean(b)
        p("== c100 matched/s and recovery of B's c100 - c0 gap")
        p(
            f"F100 {st.mean(f):.1f} (sd {sd(f):.1f})  B100 {st.mean(b):.1f} (sd {sd(b):.1f})  B0 {st.mean(b0):.1f} (sd {sd(b0):.1f}, n=2)"
        )
        dw, pw, lo, hi = welch(b, f)
        p(f"F100 - B100 Welch {dw:+.1f} = {pw:+.2f}% [{lo:+.2f}, {hi:+.2f}]")
        p(
            f"F100 - B100 ABBA paired {md:+.1f} [{md - h:+.1f}, {md + h:+.1f}] t(3); per pair {' '.join(f'{x:+.1f}' for x in d)}"
        )
        p(f"B100 vs B0 {-gap:+.1f} = {-100 * gap / st.mean(b0):+.2f}% of B0")
        p(
            f"recovery = paired / (B0 - B100) = {md / gap:.3f} [{(md - h) / gap:.3f}, {(md + h) / gap:.3f}] (CI from the paired delta only)"
        )
        p(f"F100 vs B0 {100 * (st.mean(f) - st.mean(b0)) / st.mean(b0):+.2f}%")

        # c0 regression check, q1-F0 counted
        p(
            "\n== c0 regression check, F0 = q1-F0 + q4-F0 (q1-F0 counted: mark-only digest artifact), B0 = q2-B0 + q3-B0"
        )
        p(
            f"{'metric':12s} {'q1-F0':>11s} {'q4-F0':>11s} {'q2-B0':>11s} {'q3-B0':>11s} {'F0 mean':>11s} {'B0 mean':>11s} {'F0-B0 %':>8s}  Welch 95% CI %"
        )
        for k in keys:
            fv = [R[t][k] for t in C0["F0"]]
            bv = [R[t][k] for t in C0["B0"]]
            if any(v != v for v in fv + bv):
                continue
            mf, mb = st.mean(fv), st.mean(bv)
            try:
                _, pc, lo, hi = welch(bv, fv)
                ci = f"[{lo:+.2f}, {hi:+.2f}]"
            except ZeroDivisionError:
                pc, ci = (100 * (mf - mb) / mb if mb else float("nan")), "n/a"
            p(
                f"{k:12s} "
                + " ".join(f"{v:11.3f}" for v in fv + bv)
                + f" {mf:11.3f} {mb:11.3f} {pc:+8.2f}  {ci}"
            )

        # engine phase timers per 1k fills, whole cell, val0-2 mean
        p(
            "\n== engine phase timers, ms per 1k fills (whole cell, mean of val0-2; a-ec-metrics.py calc), q1-F0 counted in F0"
        )
        ek = ["fpb"] + [q + "_1k" for q in PH]
        p(f"{'cell':10s} {'arm':5s} " + " ".join(f"{k:>18s}" for k in ek))
        for t in R:
            p(f"{t:10s} {C[t]['arm']:5s} " + " ".join(f"{E[t][k]:18.3f}" for k in ek))
        arms = dict(C100, **C0)
        p(
            f"\n{'metric':20s} "
            + " ".join(f"{a:>17s}" for a in arms)
            + "   F0-B0     F100-B100 paired [95% CI]"
        )
        for k in ek:
            m = {a: [E[t][k] for t in ts] for a, ts in arms.items()}
            pr = [E[x][k] - E[y][k] for x, y in PAIRS]
            ph = T975[3] * st.stdev(pr) / 2
            p(
                f"{k:20s} "
                + " ".join(f"{st.mean(v):9.3f} ({sd(v):5.3f})" for v in m.values())
                + f"   {st.mean(m['F0']) - st.mean(m['B0']):+.3f}   {st.mean(pr):+.3f} [{st.mean(pr) - ph:+.3f}, {st.mean(pr) + ph:+.3f}]"
            )


main()
