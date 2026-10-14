"""Histogram deltas (metrics-after minus metrics-before) per cell, mean of 3 validators (python3 -I).
Usage: hist.py OUT.txt CELLDIR:arm:label ...   Prints per metric: count, mean ms, p50/p95/p99 (bucket-interpolated), share >0.256/0.512/1.024 s."""
import re, sys, math, collections
RX = re.compile(r'^(torus_[a-z_]+)_bucket\{le="([^"]+)"\} ([0-9.e+]+)$')
RS = re.compile(r'^(torus_[a-z_]+)_(sum|count) ([0-9.e+-]+)$')
def load(p):
    b = collections.defaultdict(dict); s = {}
    for line in open(p):
        m = RX.match(line.strip())
        if m:
            le = float('inf') if m[2] == '+Inf' else float(m[2]); b[m[1]][le] = float(m[3]); continue
        m = RS.match(line.strip())
        if m: s[(m[1], m[2])] = float(m[3])
    return b, s
def q(cum, n, p):
    les = sorted(cum); prev_le, prev_c = 0.0, 0.0
    tgt = p * n
    for le in les:
        c = cum[le]
        if c >= tgt:
            if le == float('inf'): return prev_le
            f = (tgt - prev_c) / (c - prev_c) if c > prev_c else 0
            return prev_le + f * (le - prev_le)
        prev_le, prev_c = le, c
    return float('nan')
out = collections.defaultdict(dict)
for spec in sys.argv[2:]:
    cell, arm, lab = spec.split(':')
    per = collections.defaultdict(list)
    for v in ('val0', 'val1', 'val2'):
        b0, s0 = load(f'{cell}/metrics-before-{v}.txt'); b1, s1 = load(f'{cell}/metrics-after-{v}.txt')
        for name, cum1 in b1.items():
            cum = {le: cum1[le] - b0.get(name, {}).get(le, 0.0) for le in cum1}
            n = cum.get(float('inf'), 0)
            if n <= 0: continue
            sm = s1.get((name, 'sum'), 0) - s0.get((name, 'sum'), 0)
            def above(x):
                k = max((le for le in cum if le <= x), default=None)
                return (n - cum[k]) / n if k is not None else 1.0
            per[name].append((n, 1000 * sm / n, 1000 * q(cum, n, .5), 1000 * q(cum, n, .95), 1000 * q(cum, n, .99), above(.256), above(.512), above(1.024)))
    for name, xs in per.items():
        out[name][lab] = (arm, [sum(c) / len(xs) for c in zip(*xs)])
with open(sys.argv[1], 'w') as fh:
    for name in sorted(out):
        fh.write(f'== {name}\n')
        fh.write(f'{"cell":8} {"arm":3} {"n":>7} {"mean":>8} {"p50":>8} {"p95":>8} {"p99":>8} {">.256":>7} {">.512":>7} {">1.024":>7}\n')
        for lab, (arm, x) in out[name].items():
            fh.write(f'{lab:8} {arm:3} {x[0]:7.0f} {x[1]:8.1f} {x[2]:8.1f} {x[3]:8.1f} {x[4]:8.1f} {x[5]:7.4f} {x[6]:7.4f} {x[7]:7.4f}\n')
