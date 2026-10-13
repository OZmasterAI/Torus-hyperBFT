"""ozarchy-p3iv: checkpoint spacing from the 2 s scrape series (python3 -I a-ckgap.py PREFIX TAG:ARM ...).
Per cell and node: torus_exec_checkpoints[_total] increments in ckpt-samples.txt, split into the bench window
(summary t_bench0 -> t_bench1) and outside it (setup before, drain after). Prints in-bench checkpoints per node per minute,
the median gap between in-bench increments (2 s scrape resolution) and the counts outside the bench window."""

import json
import re
import statistics as st
import sys

V = ("val0", "val1", "val2")
CK = re.compile(r"^torus_exec_checkpoints(_total)?($|\{| )")


def events(c):
    ev = {v: [] for v in V}
    last = {}
    ts = cur = None
    for line in open(c + "/ckpt-samples.txt"):
        p = line.split()
        if not p:
            continue
        if p[0] == "T":
            ts, cur = float(p[1]), p[2]
            continue
        if cur in ev and CK.match(line):
            v = float(p[-1])
            if cur in last and v > last[cur]:
                ev[cur].append((ts, v - last[cur]))
            last[cur] = v
    return ev


def main():
    P = sys.argv[1]
    arms = {}
    for ta in sys.argv[2:]:
        tag, arm = ta.split(":")
        c = P + tag
        s = json.load(open(c + "/summary.json"))
        t0, t1 = s["timing"]["t_bench0"], s["timing"]["t_bench1"]
        ev = events(c)
        per_min, gaps, outside = [], [], []
        for v in V:
            ins = [e for e in ev[v] if t0 <= e[0] <= t1]
            per_min.append(sum(n for _, n in ins) / ((t1 - t0) / 60))
            gaps += [b[0] - a[0] for a, b in zip(ins, ins[1:])]
            outside.append(sum(n for t, n in ev[v] if not t0 <= t <= t1))
        r = dict(
            per_min=st.mean(per_min), gap_med=st.median(gaps), outside=st.mean(outside)
        )
        arms.setdefault(arm, []).append(r)
        print(
            f"{tag} {arm} in-bench ckpts/node/min={r['per_min']:.2f} (nodes {[round(x, 2) for x in per_min]}) "
            f"median in-bench gap s={r['gap_med']:.1f} outside-bench ckpts/node={r['outside']:.1f}"
        )
    print()
    for arm, rs in arms.items():
        print(arm, {k: round(st.mean(x[k] for x in rs), 2) for k in rs[0]})


main()
