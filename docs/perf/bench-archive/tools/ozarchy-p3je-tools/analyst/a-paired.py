"""ozarchy-p3mw paired A/B (python3 -I). Usage: a-paired.py CELLS.json PREFIX BASE OTHER
Per round rK: OTHER - BASE for each a-arms.py row metric (+ device writes from io-ticks.txt), then the mean of the 4 paired
deltas with a t(n-1) 95% CI, in units and in % of the BASE mean. Rows come from a-arms.py's row() (same definitions)."""

import json
import math
import statistics as st
import sys

T975 = {1: 12.706, 2: 4.303, 3: 3.182, 4: 2.776, 5: 2.571}


def main():
    cj, prefix, base, other = sys.argv[1:5]
    # a-arms.py's helpers only (its trailing main() call is cut off)
    src = (
        open(__file__.rsplit("/", 1)[0] + "/a-arms.py").read().rsplit("\nmain()", 1)[0]
    )
    g = {"__name__": "a_arms"}
    exec(compile(src, "a-arms.py", "exec"), g)
    row = g["row"]
    C = json.load(open(cj))
    for t, x in C.items():
        x["tag"] = t
    rounds = sorted({t.split("-")[0] for t in C if t.startswith("r")})
    pairs = [
        (r, row(C[f"{r}-{base}"], prefix), row(C[f"{r}-{other}"], prefix))
        for r in rounds
        if f"{r}-{base}" in C and f"{r}-{other}" in C
    ]
    keys = list(pairs[0][1].keys())
    print(
        f"paired rounds: {[p[0] for p in pairs]}  ({other} - {base}); CI = mean +- t(n-1) * sd / sqrt(n)"
    )
    print(
        f"{'metric':12s} {base + ' mean':>12s} {other + ' mean':>12s} {'delta':>10s} {'95% CI':>22s} {'%':>8s} {'% CI':>20s}  per-round deltas"
    )
    for k in keys:
        a = [p[1][k] for p in pairs]
        b = [p[2][k] for p in pairs]
        if any(v != v for v in a + b):
            continue
        d = [y - x for x, y in zip(a, b)]
        n = len(d)
        md = st.mean(d)
        h = T975[n - 1] * st.stdev(d) / math.sqrt(n) if n > 1 else float("nan")
        ma = st.mean(a)
        pc = (lambda v: 100 * v / ma) if ma else (lambda v: float("nan"))
        print(
            f"{k:12s} {ma:12.3f} {st.mean(b):12.3f} {md:+10.3f} [{md - h:+9.3f}, {md + h:+9.3f}] {pc(md):+7.2f}% "
            f"[{pc(md - h):+7.2f}, {pc(md + h):+7.2f}]  {' '.join(f'{v:+.3f}' for v in d)}"
        )


main()
