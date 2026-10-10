#!/usr/bin/env python3
"""cells3.py: ozarchy-p3s0r per-cell + per-group tables (no-perf m, perf m, Classic k) and p3s1 C (3efff0d6) reference.
Phases: summary.json phase_by_node (ms per native block, bench+drain window). CPU: proc_cpu_by_node load window.
Disk: io-ticks.txt (/proc/<pid>/io write_bytes) and RocksDB counters (summary rocksdb MB/s, load window).
Per-thread write bytes / CPU: io-threads-{start,end}.txt (bench start -> bench exit)."""
import json, gzip, re, os, sys, statistics as stt
from collections import Counter, defaultdict
R = '/home/oz/bench-results-matched'
ANSI = re.compile(r'\x1b\[[0-9;]*m'); BAD = re.compile(r'panic|ERROR|exit code 70', re.I)
def grp(c):
    for g, p in [('exec', 'torus-execution'), ('flush_worker', 'torus-flush-wor'), ('end_resident', 'torus-end-resid'), ('trade_writer', 'torus-trade-wri'),
                 ('rocksdb', 'rocksdb'), ('tokio', 'tokio-rt-worker'), ('rpc', 'rpc-worker'), ('main', 'torus-node'), ('hotstuff', 'hotstuff'), ('gossip', 'torus-gossip'), ('ingress', 'torus-ingress')]:
        if c.startswith(p): return g
    return 'other'
def cell(lab, md5=None):
    o = f'{R}/{lab}'; s = json.load(open(f'{o}/summary.json')); h = s['headline']; d = {'cell': lab.split('-300m-')[1]}
    d['rc'] = open(f'{o}.cell.rc').read().strip() if os.path.exists(f'{o}.cell.rc') else '?'
    d['agree'] = h['agreement_verdict']; d['live'] = h['liveness_verdict']
    of = s.get('oracle_feed') or {}; d['stale'] = of.get('stale_marks_at_bench_end')
    bad = 0
    for v in range(3):
        with gzip.open(f'{o}/val{v}.log.gz', 'rt', errors='replace') as f: bad += sum(1 for l in f if BAD.search(ANSI.sub('', l)))
    d['bad'] = bad; d['death'] = os.path.exists(f'{o}/node-death.txt')
    if md5: d['md5ok'] = open(f'{o}/node-environ-trie.txt').read().count('exe_md5=' + md5) == 3
    d['matched'] = h['matched_s_avg']; d['nblk'] = h['native_blk_s']
    for v in range(3):
        pv = s['phase_by_node'][f'val{v}']; ph = pv['phases']; fpb = pv['fills_per_native_block']; x = {}
        x['fpb'] = fpb; x['block'] = pv['block_ms']; x['chain'] = pv.get('chain_ms')
        for k in ['verify', 'replay_guard', 'load_books', 'engine', 'save_books', 'flush', 'end_resident', 'end_resident_wait', 'residual_untimed']:
            x[k] = ph[k]['ms']
        e = ph['engine']
        for k in ['phase_margin_ms', 'phase_match_ms', 'phase_settle_ms', 'phase1_actions_ms', 'cache_flush_ms', 'post_engine_tail_ms', 'engine_untimed_ms']:
            x[k[:-3]] = e.get(k)
        fl = ph['flush']
        for k in ['state_write_ms', 'state_write_build_ms', 'state_write_db_ms', 'root_ms']: x['fl_' + k[:-3]] = fl.get(k)
        x['state_write_batch_kb'] = fl.get('state_write_batch_kb')
        x['save_books_write'] = ph['save_books'].get('save_books_write_ms')
        x['commit_persist'] = pv.get('commit_persist_ms_avg')
        rk = pv.get('rocksdb') or {}
        nb = pv['native_blk_s']
        for k in ['wal_mb_per_s', 'flush_write_mb_per_s', 'compact_write_mb_per_s', 'compact_read_mb_per_s', 'compaction_cpu_cores']: x['rk_' + k] = rk.get(k)
        x['oracle_only_ms'] = (pv.get('oracle_only_blocks') or {}).get('ms_avg')
        pc = s['proc_cpu_by_node'][f'val{v}']['load']
        x['cpu_user_1k'] = pc['user_ms_per_1k_fills']; x['cpu_sys_1k'] = pc['sys_ms_per_1k_fills']
        x['cpu_user_blk'] = pc['user_ms_per_native_block']; x['cpu_sys_blk'] = pc['sys_ms_per_native_block']
        x['load_blocks'] = pc['native_blocks']; x['load_fills'] = pc['fills']; x['pid'] = pc.get('pid') or s['proc_cpu_by_node'][f'val{v}'].get('pid')
        d[f'v{v}'] = x
    t0, t1 = s['timing']['t_bench0'], s['timing']['t_bench1']
    io = defaultdict(dict)
    if os.path.exists(f'{o}/io-ticks.txt'):
        for l in open(f'{o}/io-ticks.txt'):
            p = l.split()
            if len(p) == 7 and p[2] != '': io[p[1]][int(p[0])] = [int(z) for z in p[2:]]
    for v in range(3):
        x = d[f'v{v}']; m = io.get(str(x['pid']))
        if m:
            ks = [k for k in sorted(m) if t0 <= k <= t1]
            if len(ks) > 2:
                a, b = m[ks[0]], m[ks[-1]]; secs = ks[-1] - ks[0]; frac = secs / max(1, t1 - t0)
                wb = b[3] - a[3]; wc = b[1] - a[1]
                x['io_secs'] = secs
                x['wb_MB_s'] = wb / secs / 1e6; x['wchar_MB_s'] = wc / secs / 1e6
                x['wb_MB_blk'] = wb / 1e6 / (x['load_blocks'] * frac); x['wb_GB_1M'] = wb / 1e9 / (x['load_fills'] * frac / 1e6)
                x['wchar_GB_1M'] = wc / 1e9 / (x['load_fills'] * frac / 1e6); x['rb_MB_s'] = (b[2] - a[2]) / secs / 1e6
    if os.path.exists(f'{o}/io-threads-end.txt'):
        def rd(f):
            z = {}
            for l in open(f):
                p = l.split()
                if len(p) == 8: z[(p[0], p[1])] = (p[2], int(p[4]), int(p[6]) + int(p[7]))
            return z
        A, B = rd(f'{o}/io-threads-start.txt'), rd(f'{o}/io-threads-end.txt')
        for v in range(3):
            x = d[f'v{v}']; pid = str(x['pid']); gw = Counter(); gc = Counter()
            for (p, tid), (c, w, cpu) in B.items():
                if p != pid: continue
                w0, c0 = (A[(p, tid)][1], A[(p, tid)][2]) if (p, tid) in A else (0, 0)
                gw[grp(c)] += w - w0; gc[grp(c)] += cpu - c0
            x['thr_wb_MB_blk'] = {k: round(val / 1e6 / x['load_blocks'], 2) for k, val in gw.most_common()}
            x['thr_cpu_ms_blk_live'] = {k: round(val * 10 / x['load_blocks'], 1) for k, val in gc.most_common()}
    d['valid'] = d['rc'] == 'rc=0' and d['agree'] == 'AGREE' and d['live'] == 'PASS' and d['stale'] == 0 and d['bad'] == 0 and not d['death'] and d.get('md5ok', True)
    return d
COLS = ['fpb', 'block', 'chain', 'engine', 'phase_settle', 'phase_margin', 'phase_match', 'phase1_actions', 'cache_flush', 'post_engine_tail', 'engine_untimed',
        'verify', 'replay_guard', 'save_books', 'save_books_write', 'end_resident_wait', 'residual_untimed', 'end_resident', 'flush', 'fl_state_write', 'fl_state_write_build',
        'fl_state_write_db', 'state_write_batch_kb', 'commit_persist', 'oracle_only_ms',
        'cpu_user_blk', 'cpu_sys_blk', 'cpu_user_1k', 'cpu_sys_1k',
        'rk_wal_mb_per_s', 'rk_flush_write_mb_per_s', 'rk_compact_write_mb_per_s', 'rk_compact_read_mb_per_s', 'rk_compaction_cpu_cores',
        'wb_MB_s', 'wchar_MB_s', 'rb_MB_s', 'wb_MB_blk', 'wb_GB_1M', 'wchar_GB_1M']
def v3(d, k):
    xs = [d[f'v{v}'].get(k) for v in range(3)]
    return stt.mean(xs) if all(isinstance(z, (int, float)) for z in xs) else None
def ms(xs):
    xs = [z for z in xs if isinstance(z, (int, float))]
    if not xs: return 'n/a'
    return f'{stt.mean(xs):,.3f} ± {stt.stdev(xs):,.3f}' if len(xs) > 1 else f'{xs[0]:,.3f}'
pr = print
G = {'m (no perf)': ('ozarchy-p3s0r-300m', ['m-r1', 'm-r2', 'm-r3', 'm-r4'], '6a71ba5f'),
     'm (perf val0)': ('ozarchy-p3s0r-300m', ['m-p1', 'm-p2'], '6a71ba5f'),
     'k Classic (no perf)': ('ozarchy-p3s0r-300m', ['k-r1', 'k-r2'], '6a71ba5f'),
     'p3s1 C 3efff0d6': ('ozarchy-p3s1-300m', ['c-r1', 'c-r2', 'c-r3', 'c-r4'], '0c100f3b')}
C = {}
pr('## per cell (val0; ms per native block unless noted)')
pr('| group | cell | rc | agree | live | stale | bad | death | md5 | matched/s | native blk/s | ' + ' | '.join(COLS[:12]) + ' |')
for gname, (P, tags, md5) in G.items():
    for t in tags:
        try: d = cell(f'{P}-{t}', md5)
        except Exception as e: pr(f'| {gname} | {t} | MISSING {e!r} |'); continue
        C[(gname, t)] = d; x = d['v0']
        pr(f"| {gname} | {t} | {d['rc']} | {d['agree']} | {d['live']} | {d['stale']} | {d['bad']} | {d['death']} | {d.get('md5ok')} | {d['matched']:,.0f} | {d['nblk']} | "
           + ' | '.join(f"{x.get(k):,.2f}" if isinstance(x.get(k), (int, float)) else 'n/a' for k in COLS[:12]) + ' |')
pr('\n## group means ± sd over valid cells; val0 and (3) = mean of val0-2 per cell')
for gname, (P, tags, md5) in G.items():
    ds = [C[(gname, t)] for t in tags if (gname, t) in C and C[(gname, t)]['valid']]
    pr(f'\n### {gname}: n={len(ds)} valid of {len(tags)}')
    pr(f"matched/s {ms([d['matched'] for d in ds])}   native blk/s {ms([d['nblk'] for d in ds])}")
    pr('| metric | val0 | (3) | val0 per 1k fills |')
    for k in COLS:
        pk = ms([1000 * d['v0'][k] / d['v0']['fpb'] for d in ds if isinstance(d['v0'].get(k), (int, float))]) if k not in ('fpb', 'state_write_batch_kb') and not k.startswith(('rk_', 'wb_', 'wchar', 'rb_', 'cpu_')) else ''
        pr(f"| {k} | {ms([d['v0'].get(k) for d in ds])} | {ms([v3(d, k) for d in ds])} | {pk} |")
    tw = defaultdict(list); tc = defaultdict(list)
    for d in ds:
        for v in range(3):
            for kk, val in (d[f'v{v}'].get('thr_wb_MB_blk') or {}).items(): tw[kk].append(val)
            for kk, val in (d[f'v{v}'].get('thr_cpu_ms_blk_live') or {}).items(): tc[kk].append(val)
    if tw:
        pr('per-thread-group write_bytes MB per native block (val0-2 pooled, load window; live threads at bench end): ' + '; '.join(f'{k} {ms(v)}' for k, v in sorted(tw.items(), key=lambda z: -stt.mean(z[1]))))
        pr('per-thread-group CPU ms per native block (live threads at bench end, user+sys): ' + '; '.join(f'{k} {ms(v)}' for k, v in sorted(tc.items(), key=lambda z: -stt.mean(z[1]))))
