#!/usr/bin/env python3
"""ozarchy-p3s1 analysis: per-cell table, validity, per-arm mean/sd (n=4), steps B/A and C/B, per-round paired ratios."""
import json, gzip, re, os, statistics as st, sys
R = '/home/oz/bench-results-matched'
P = 'ozarchy-p3s1-300m'
ARM = {'a': ('1b389700', '86477b00'), 'b': ('3aa516e0', 'db344840'), 'c': ('3efff0d6', '0c100f3b')}
ROUNDS = [['a', 'b', 'c'], ['b', 'c', 'a'], ['c', 'a', 'b'], ['a', 'c', 'b']]
ANSI = re.compile(r'\x1b\[[0-9;]*m')
BAD = re.compile(r'panic|ERROR|exit code 70|exit 70|exit_code=70', re.I)

def cell(tag):
    o = f'{R}/{P}-{tag}'
    s = json.load(open(f'{o}/summary.json'))
    h = s['headline']
    d = {'tag': tag}
    d['rc'] = open(f'{o}.cell.rc').read().strip()
    d['agree'] = h['agreement_verdict']; d['live'] = h['liveness_verdict']; d['acc'] = h['benchmark_accepted']
    of = s.get('oracle_feed') or {}
    d['stale'] = of.get('stale_marks_at_bench_end'); d['oracle'] = f"{of.get('accepted')}/{of.get('sent')}"
    env = open(f'{o}/node-environ-trie.txt').read()
    d['md5ok'] = env.count('exe_md5=' + ARM[tag[0]][1]) == 3
    bad = 0
    for v in range(3):
        with gzip.open(f'{o}/val{v}.log.gz', 'rt', errors='replace') as f:
            bad += sum(1 for l in f if BAD.search(ANSI.sub('', l)))
    d['bad'] = bad
    d['death'] = os.path.exists(f'{o}/node-death.txt')
    d['matched'] = h['matched_s_avg']; d['nblk'] = h['native_blk_s']
    for v in range(3):
        pv = s['phase_by_node'][f'val{v}']; ph = pv['phases']; fpb = pv['fills_per_native_block']
        d[f'fpb{v}'] = fpb
        d[f'erw{v}'] = ph['end_resident_wait']['ms']; d[f'res{v}'] = ph['residual_untimed']['ms']
        d[f'erwk{v}'] = 1000 * ph['end_resident_wait']['ms'] / fpb
        d[f'resk{v}'] = 1000 * ph['residual_untimed']['ms'] / fpb
        d[f'blkms{v}'] = pv['block_ms'] if not isinstance(pv['block_ms'], dict) else pv['block_ms'].get('avg')
    d['erwk3'] = st.mean(d[f'erwk{v}'] for v in range(3)); d['resk3'] = st.mean(d[f'resk{v}'] for v in range(3))
    d['txs'] = h.get('txs_per_block_avg'); d['act'] = h.get('actions_per_exec_block')
    d['valid'] = (d['rc'] == 'rc=0' and d['agree'] == 'AGREE' and d['live'] == 'PASS' and d['acc'] is True and d['stale'] == 0
                  and d['md5ok'] and d['bad'] == 0 and not d['death'])
    return d

M = [('matched', 'matched/s'), ('nblk', 'native blk/s'), ('fpb0', 'fills/blk'), ('erw0', 'end_res wait ms/blk'),
     ('erwk0', 'end_res wait /1k fills'), ('erwk3', 'end_res wait /1k (val0-2)'), ('res0', 'residual ms/blk'),
     ('resk0', 'residual /1k fills'), ('resk3', 'residual /1k (val0-2)')]
out = []
pr = out.append
tags = ['a-warm'] + [f'{x}-r{i+1}' for i, rd in enumerate(ROUNDS) for x in rd]
C = {}
for t in tags:
    try: C[t] = cell(t)
    except Exception as e: pr(f'{t}: MISSING/ERROR {e!r}')
pr('ozarchy-p3s1 (ozarchy): A=1b389700 (node 86477b00), B=3aa516e0 (node db344840, R02+EVM typing), C=3efff0d6 (node 0c100f3b, B+R01); bench 6c7ad1a7, harness bdd5b470; N=4 budget 900, 300 mk, 120 s, trie off, 4 MiB book CF, no perf')
pr('order: a-warm (60 s, excluded), then rounds ABC, BCA, CAB, ACB')
pr('per cell (val0 unless noted; ms per native block; /1k = ms per 1k fills; (3) = mean over val0-2)')
pr('| cell | rc | agree | live | acc | stale | oracle | md5 3/3 | bad lines | death | ' + ' | '.join(n for _, n in M) + ' | txs/blk | actions/blk |')
for t in tags:
    if t not in C: continue
    d = C[t]
    pr(f"| {t} | {d['rc']} | {d['agree']} | {d['live']} | {d['acc']} | {d['stale']} | {d['oracle']} | {d['md5ok']} | {d['bad']} | {d['death']} | "
       + ' | '.join(f'{d[k]:,.3f}' for k, _ in M) + f" | {d['txs']} | {d['act']} |")
inval = [t for t in tags[1:] if t in C and not C[t]['valid']]
pr(f'invalid counted cells: {inval or "none"}')
A = {x: [C[f'{x}-r{i+1}'] for i in range(4) if f'{x}-r{i+1}' in C and C[f'{x}-r{i+1}']['valid']] for x in 'abc'}
pr('\nper arm (counted cells, valid only): mean ± sd (n)')
pr('| arm | ' + ' | '.join(n for _, n in M) + ' |')
S = {}
for x in 'abc':
    S[x] = {k: (st.mean(c[k] for c in A[x]), st.stdev(c[k] for c in A[x]) if len(A[x]) > 1 else 0.0) for k, _ in M}
    pr(f'| {x.upper()} {ARM[x][0]} (n={len(A[x])}) | ' + ' | '.join(f'{S[x][k][0]:,.3f} ± {S[x][k][1]:,.3f}' for k, _ in M) + ' |')
for (p, q, name) in [('b', 'a', 'B/A (R02+EVM typing)'), ('c', 'b', 'C/B (R01)'), ('c', 'a', 'C/A (p3s0 step)')]:
    pr(f'\nstep {name}: ratio of means, diff in pooled-sd units (pooled sd = sqrt((sd1^2+sd2^2)/2)), per-round paired ratios r1..r4')
    for k, n in M:
        m1, s1 = S[p][k]; m0, s0 = S[q][k]; ps = ((s1**2 + s0**2) / 2) ** 0.5
        pairs = []
        for i in range(4):
            u, w = C.get(f'{p}-r{i+1}'), C.get(f'{q}-r{i+1}')
            pairs.append(f'{u[k]/w[k]:.4f}' if u and w and u['valid'] and w['valid'] else 'n/a')
        pr(f'  {n}: {m1/m0:.4f}x  diff {m1-m0:+,.3f}  pooled sd {ps:,.3f}  diff/sd {((m1-m0)/ps if ps else float("nan")):+.2f}  paired {" ".join(pairs)}')
open(f'{R}/ozarchy-p3s1-handoff-tables.txt', 'w').write('\n'.join(out) + '\n')
print('\n'.join(out))
