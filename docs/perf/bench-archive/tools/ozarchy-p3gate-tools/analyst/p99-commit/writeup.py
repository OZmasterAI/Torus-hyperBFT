"""Cross-checks for results doc section 66 (python3 -I), written for the write-up, not by the analyst.
Usage: writeup.py ANALYST_DIR BENCH_DIR > writeup.txt
ANALYST_DIR = ozarchy-p3gate-run/analyst/p99-commit (reads attrib-tagged.csv, attrib-windows.json from attrib.py);
BENCH_DIR = ~/bench-results-matched (reads each cell's sampler.csv, io-ticks.txt, cpu-ticks.txt).
1. Tails > T outside the windows by tag, with the m cells' pseudo-window intervals removed (attrib.txt's per-tag
   columns count m over all of its time, so its tag deltas are not the outside-window split).
2. Tail intervals > T by arm, regime and in / out of window: mean length, fleet blocked-on-exec-send union,
   commit persist (max over nodes), exec inter-block gap of the critical node, residual (no node busy).
3. Flush-worker state writes per node (sampler deltas, 1 s): total (build + DB) seconds, DB seconds, batch MB.
4. Disk write rate of the 3 nodes (io-ticks write_bytes) and node CPU (cpu-ticks utime + stime) in / out of windows.
5. RocksDB stall counters (max over the cell)."""
import csv, json, math, sys, collections, statistics as st
A, B = sys.argv[1], sys.argv[2]
CELL = {**{f'{r}-{a}': f'{B}/ozarchy-p3gate-300m-{r}-{a}' for r in ('r1', 'r2', 'r3', 'r4', 'k1') for a in ('s2', 'm')},
        **{f'{r}-d3': f'{B}/ozarchy-p4s0-300m-{r}-m' for r in ('u1', 'u2', 'k1')}}
W = json.load(open(A + '/attrib-windows.json'))
rows = collections.defaultdict(lambda: collections.defaultdict(list))
for x in csv.DictReader(open(A + '/attrib-tagged.csv')): rows[x['label']][x['val']].append(x)
def pct(xs, q):
    xs = sorted(xs); k = (len(xs) - 1) * q; f = math.floor(k); c = min(f + 1, len(xs) - 1)
    return xs[f] + (xs[c] - xs[f]) * (k - f)
def q(v, f): v = sorted(v); return v[min(len(v) - 1, int(f * len(v)))]
def first_t(l): return min(float(x['t']) - float(x['ci_ms']) / 1000 for xs in rows[l].values() for x in xs)
def inwin(x, w):
    t1 = float(x['t']); t0 = t1 - float(x['ci_ms']) / 1000
    return any(a < t1 and b > t0 for a, b, _ in w)
def wins(l):
    if l.endswith('-m'):
        s = l[:-2] + '-s2'; off = first_t(l) - first_t(s)
        return [(a + off, b + off, v) for a, b, v in W[s]['ckwin']]
    return W[l]['ckwin']
REG = {'SAT': ['r1', 'r2', 'r3', 'r4'], 'soak': ['k1']}
T = {g: sum(pct([float(x['ci_ms']) for x in xs], .99) for r in rs for xs in rows[f'{r}-m'].values()) / (3 * len(rs))
     for g, rs in REG.items()}
print('== 1. tails > T per validator per 100 s, by tag; IN = in a (pseudo-)window, others outside')
for g, rs in REG.items():
    agg = {a: collections.Counter() for a in ('s2', 'm')}
    for r in rs:
        for a in ('s2', 'm'):
            l = f'{r}-{a}'; w = wins(l)
            for v, xs in rows[l].items():
                span = float(xs[-1]['t']) - float(xs[0]['t']) + float(xs[0]['ci_ms']) / 1000
                for x in xs:
                    if float(x['ci_ms']) > T[g]:
                        agg[a]['IN' if inwin(x, w) else x['tag']] += 100 / span / 3 / len(rs)
    print(f'{g} T={T[g]:.0f} ms')
    for k in ('IN', 'timeout', 'epoch', 'execbp', 'other'):
        print(f'  {k:8} s2 {agg["s2"][k]:5.2f}  m {agg["m"][k]:5.2f}  s2-m {agg["s2"][k] - agg["m"][k]:+5.2f}')
    so = sum(v for k, v in agg['s2'].items() if k != 'IN'); mo = sum(v for k, v in agg['m'].items() if k != 'IN')
    print(f'  outside  s2 {so:5.2f}  m {mo:5.2f}  s2-m {so - mo:+5.2f}')
print('== 2. tail intervals > T: mean ms and share of the summed interval length')
acc = collections.defaultdict(lambda: collections.defaultdict(list))
for l, per in rows.items():
    g = 'soak' if l.startswith('k1') else 'SAT'; w = wins(l)
    for xs in per.values():
        for x in xs:
            if float(x['ci_ms']) <= T[g]: continue
            a = acc[(l.split('-')[1], g, 'win' if inwin(x, w) else 'out')]
            a['ci'].append(float(x['ci_ms'])); a['ub'].append(float(x['union_blocked_ms']))
            a['mp'].append(float(x['max_persist_any'])); a['res'].append(float(x['resid_ms']))
            if x['bl_idle_ms'] not in ('', 'nan'): a['gap'].append(float(x['bl_idle_ms']))
for k in sorted(acc):
    a = acc[k]; s = sum(a['ci'])
    print(f"{k[0]:3} {k[1]:4} {k[2]:3} n {len(a['ci']):3}  interval {st.mean(a['ci']):5.0f}  blocked-on-send {st.mean(a['ub']):4.0f} "
          f"({100 * sum(a['ub']) / s:2.0f}%)  persist max-node p50 {st.median(a['mp']):4.0f}  exec gap mean (n {len(a['gap'])}) "
          f"{st.mean(a['gap']) if a['gap'] else 0:4.0f}  residual {st.mean(a['res']):4.0f} ({100 * sum(a['res']) / s:2.0f}%)")
def sampler(cell):
    by = collections.defaultdict(list)
    for x in csv.DictReader(open(cell + '/sampler.csv')): by[x['node']].append(x)
    return by
g_ = lambda x, c: float(x.get(c) or 0)
print('== 3. state writes per node: big = total >= 1.0 s; seconds and MB per write sample')
for l in ('r1-s2', 'r2-s2', 'r3-s2', 'r4-s2', 'k1-s2', 'u1-d3', 'u2-d3', 'k1-d3'):
    big, small = [], []
    for xs in sampler(CELL[l]).values():
        for p, x in zip(xs, xs[1:]):
            if g_(x, 'torus_exec_state_write_db_seconds_count') > g_(p, 'torus_exec_state_write_db_seconds_count'):
                d = lambda c: g_(x, c) - g_(p, c)
                r = (d('torus_exec_state_write_seconds_sum'), d('torus_exec_state_write_db_seconds_sum'), d('torus_exec_state_write_batch_bytes_sum') / 1e6)
                (big if r[0] >= 1.0 else small).append(r)
    sm = [s for s in small if s[0] > 0.02]
    print(f"{l:6} big n {len(big):2} total s p10/p50/p90 {q([b[0] for b in big], .1):.2f}/{q([b[0] for b in big], .5):.2f}/{q([b[0] for b in big], .9):.2f}"
          f"  DB s p50 {q([b[1] for b in big], .5):.2f}  MB p50 {q([b[2] for b in big], .5):.0f} max {max(b[2] for b in big):.0f}"
          f" | follow-ups n {len(sm):3} total s p50/p90 {q([s[0] for s in sm], .5):.2f}/{q([s[0] for s in sm], .9):.2f}")
def ticks(path, f):
    t = collections.defaultdict(dict)
    for ln in open(path):
        p = ln.split()
        if len(p) >= 4: t[int(p[0])][p[1]] = f(p)
    ts = sorted(t); r = {}
    for a, b in zip(ts, ts[1:]):
        if b - a == 1: r[b] = sum(t[b][k] - t[a].get(k, t[b][k]) for k in t[b])
    return r
print('== 4. per second, 3 nodes: disk writes GB/s and node CPU cores, in / out of windows')
for l in ('r1-s2', 'r2-s2', 'r3-s2', 'r4-s2', 'k1-s2', 'u1-d3', 'u2-d3', 'k1-d3'):
    w = W[l]['ckwin']; iw = lambda t: any(a < t <= b for a, b, _ in w)
    io = ticks(CELL[l] + '/io-ticks.txt', lambda p: int(p[5]) / 1e9); cpu = ticks(CELL[l] + '/cpu-ticks.txt', lambda p: (int(p[2]) + int(p[3])) / 100)
    ii = [v for t, v in io.items() if iw(t)]; io_ = [v for t, v in io.items() if not iw(t)]
    ci = [v for t, v in cpu.items() if iw(t)]; co = [v for t, v in cpu.items() if not iw(t)]
    print(f'{l:6} write GB/s in p50 {q(ii, .5):.2f} p90 {q(ii, .9):.2f} max {max(ii):.2f} | out p50 {q(io_, .5):.2f} p90 {q(io_, .9):.2f}'
          f'   cores in p10 {q(ci, .1):4.1f} p50 {q(ci, .5):4.1f} max {max(ci):4.1f} | out p50 {q(co, .5):4.1f}')
print('== 5. RocksDB stall counters, max over the cell and nodes')
for l, c in CELL.items():
    mx = collections.Counter()
    for xs in sampler(c).values():
        for x in xs:
            for k in x:
                if 'stall' in k or 'stopped' in k: mx[k] = max(mx[k], g_(x, k))
    print(f'{l:6} ' + ' '.join(f"{k.replace('torus_rocksdb_', '')}={v:g}" for k, v in sorted(mx.items())))
