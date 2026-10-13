"""ozarchy-p3iv: checkpoint spacing and rates from the 2 s scrape series (python3 -I a-ckgap.py PREFIX TAG:ARM ...).
Per cell and node, from ckpt-samples.txt (torus_exec_checkpoints[_total] and torus_exec_checkpoint_layer_bytes_sum):
  in-bench rates: (counter at the last scrape inside t_bench0..t_bench1 - counter at the first scrape inside it) / the time
    between those two scrapes, so numerator and denominator cover the same window (checkpoints and layer MB per minute);
  median gap between in-bench checkpoint increments (2 s scrape resolution);
  checkpoints observed outside the bench window (setup before, drain and digest after)."""

import json
import re
import statistics as st
import sys

V = ("val0", "val1", "val2")
CK = re.compile(r"^torus_exec_checkpoints(_total)?($|\{| )")
LB = re.compile(r"^torus_exec_checkpoint_layer_bytes_sum($|\{| )")


def series(c):
    """node -> list of (scrape ts, checkpoints, layer bytes sum), one entry per ok scrape that has both series."""
    out = {v: [] for v in V}
    cur = ts = None
    ck = lb = None
    for line in open(c + "/ckpt-samples.txt"):
        p = line.split()
        if not p:
            continue
        if p[0] == "T":
            if cur in out and ck is not None and lb is not None:
                out[cur].append((ts, ck, lb))
            ts, cur, ck, lb = float(p[1]), p[2], None, None
            continue
        if CK.match(line):
            ck = (ck or 0.0) + float(p[-1])
        elif LB.match(line):
            lb = (lb or 0.0) + float(p[-1])
    if cur in out and ck is not None and lb is not None:
        out[cur].append((ts, ck, lb))
    return out


def main():
    P = sys.argv[1]
    arms = {}
    for ta in sys.argv[2:]:
        tag, arm = ta.split(":")
        c = P + tag
        s = json.load(open(c + "/summary.json"))
        t0, t1 = s["timing"]["t_bench0"], s["timing"]["t_bench1"]
        se = series(c)
        per_min, lmb_min, gaps, outside = [], [], [], []
        for v in V:
            xs = se[v]
            ins = [x for x in xs if t0 <= x[0] <= t1]
            a, b = ins[0], ins[-1]
            mins = (b[0] - a[0]) / 60
            per_min.append((b[1] - a[1]) / mins)
            lmb_min.append((b[2] - a[2]) / 1e6 / mins)
            inc = [
                y[0]
                for x, y in zip(xs, xs[1:], strict=False)
                if y[1] > x[1] and t0 <= y[0] <= t1
            ]
            gaps += [q - p for p, q in zip(inc, inc[1:], strict=False)]
            outside.append(
                sum(
                    y[1] - x[1]
                    for x, y in zip(xs, xs[1:], strict=False)
                    if not t0 <= y[0] <= t1
                )
            )
        r = dict(
            per_min=st.mean(per_min),
            layer_mb_min=st.mean(lmb_min),
            gap_med=st.median(gaps),
            outside=st.mean(outside),
        )
        arms.setdefault(arm, []).append(r)
        print(
            f"{tag} {arm} in-bench ckpts/node/min={r['per_min']:.2f} (nodes {[round(x, 2) for x in per_min]}) "
            f"in-bench layer MB/node/min={r['layer_mb_min']:.0f} median in-bench gap s={r['gap_med']:.1f} "
            f"outside-bench ckpts/node={r['outside']:.1f}"
        )
    print()
    for arm, rs in arms.items():
        print(arm, {k: round(st.mean(x[k] for x in rs), 2) for k in rs[0]})


main()
