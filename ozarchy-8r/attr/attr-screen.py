#!/usr/bin/env python3
"""Screen every collected series: node-mean per cell of (delta / window s) or gauge mean / max; paired B/A, C/A
(geo-mean ratio, 95% t CI df=3) and the count of pairs in the same direction. attr-screen.py ATTR_JSON WINDOW [STAT]"""

import json
import math
import statistics as st
import sys

T975 = {1: 12.706, 2: 4.303, 3: 3.182}
NODES = ("val0", "val1", "val2")


def ci(xs):
    if any(x <= 0 for x in xs):
        return None
    v = [math.log(x) for x in xs]
    m = st.mean(v)
    hw = T975[len(v) - 1] * st.stdev(v) / math.sqrt(len(v))
    return math.exp(m), math.exp(m - hw), math.exp(m + hw)


def cellval(c, win, key, stat):
    w = c["win"][win]
    vals = []
    for n in NODES:
        x = c[win][n].get(key)
        if x is None or x[stat] is None:
            return None
        vals.append(x[stat] / (w[1] - w[0]) if stat == "d" else x[stat])
    return st.mean(vals)


def main():
    d = json.load(open(sys.argv[1]))
    win = sys.argv[2]
    stats = sys.argv[3:] or ["d", "mean"]
    reps = sorted({c["rep"] for c in d.values()})
    by = {(c["rep"], c["arm"]): c for c in d.values()}
    keys = sorted(by[(reps[0], "A")][win]["val0"].keys())
    rows = []
    for k in keys:
        for stat in stats:
            v = {(r, a): cellval(by[(r, a)], win, k, stat) for r in reps for a in "ABC"}
            if any(x is None for x in v.values()):
                continue
            out = [k, stat] + [st.mean(v[(r, a)] for r in reps) for a in "ABC"]
            ok = True
            for arm in "BC":
                rat = [v[(r, arm)] / v[(r, "A")] if v[(r, "A")] else None for r in reps]
                if any(x is None for x in rat):
                    ok = False
                    break
                g = ci(rat)
                up = sum(x > 1 for x in rat)
                out += [g, up]
            if ok:
                rows.append(out)
    for o in rows:
        gb, ub, gc, uc = o[5], o[6], o[7], o[8]
        sig = lambda g: g is not None and (g[1] > 1 or g[2] < 1)
        flag = ("B" if sig(gb) else "-") + ("C" if sig(gc) else "-")
        if "--all" not in sys.argv and flag == "--":
            continue
        f = lambda g: "n/a" if g is None else f"{g[0]:.3f} [{g[1]:.3f},{g[2]:.3f}]"
        print(
            f"{flag} {o[0]} {o[1]} | A {o[2]:.6g} B {o[3]:.6g} C {o[4]:.6g} | B/A {f(gb)} up {ub}/4 | C/A {f(gc)} up {uc}/4"
        )


if __name__ == "__main__":
    main()
