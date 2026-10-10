#!/usr/bin/env python3
"""p3q.py <cell_dir>: val0 perf window (cycles:u, whole process) -> user CPU ms per native block and per 1k fills
for thread groups and for inclusive function keys. ms = share_of_user_cycles * utime_delta / (blocks|fills/1000).
Inclusive key = any frame contains every substring of the key (AND), counted once per sample; optional comm prefix."""
import sys, json, re
from collections import Counter
d = sys.argv[1]
def met(f):
    m = {}
    for l in open(f):
        if l.startswith('#'): continue
        p = l.split()
        if len(p) >= 2:
            try: m[p[0]] = float(p[1])
            except ValueError: pass
    return m
a, b = met(d + '/prof-metrics-before.txt'), met(d + '/prof-metrics-after.txt')
span = float(open(d + '/prof-after.ts').read()) - float(open(d + '/prof-before.ts').read())
fills = b['torus_orders_matched_total'] - a['torus_orders_matched_total']
blks = b['torus_exec_native_blocks_total'] - a['torus_exec_native_blocks_total']
sa = open(d + '/prof-stat-before.txt').read().split(')')[1].split(); sb = open(d + '/prof-stat-after.txt').read().split(')')[1].split()
ut = (int(sb[11]) - int(sa[11])) / 100; st = (int(sb[12]) - int(sa[12])) / 100
GROUPS = [('exec (torus-execution*)', 'torus-execution'), ('flush worker', 'torus-flush-wor'), ('end_resident', 'torus-end-resid'),
          ('trade writer', 'torus-trade-wri'), ('rocksdb bg', 'rocksdb'), ('tokio', 'tokio-rt-worker'), ('rpc', 'rpc-worker'),
          ('ingress', 'torus-ingress'), ('gossip', 'torus-gossip'), ('hotstuff', 'hotstuff')]
KEYS = [  # (label, comm prefix or '', [substrings AND])
 ('execute_batch_phases (exec)', 'torus-execution', ['execute_batch_phases']),
 ('cache flush: flush_all (exec)', 'torus-execution', ['flush_all']),
 ('  PositionCache::flush_all', 'torus-execution', ['PositionCache', 'flush_all']),
 ('  BalanceCache::flush_all', 'torus-execution', ['BalanceCache', 'flush_all']),
 ('  flush_all > put_position', 'torus-execution', ['flush_all', 'put_position']),
 ('  flush_all > BTreeMap insert', 'torus-execution', ['flush_all', 'BTreeMap', 'insert']),
 ('  flush_all > sort', 'torus-execution', ['flush_all', 'sort']),
 ('  flush_all > intern_cf', 'torus-execution', ['flush_all', 'intern_cf']),
 ('save_books', '', ['save_books']),
 ('run_liquidations_with', '', ['run_liquidations_with']),
 ('  liq > delta_sums', '', ['run_liquidations_with', 'delta_sums']),
 ('  liq > build_sums', '', ['run_liquidations_with', 'build_sums']),
 ('  liq > cached_sums', '', ['run_liquidations_with', 'cached_sums']),
 ('  liq > clear_cooldown', '', ['run_liquidations_with', 'clear_cooldown']),
 ('  liq > set_pending', '', ['run_liquidations_with', 'set_pending']),
 ('  liq > get_native_balance', '', ['run_liquidations_with', 'get_native_balance']),
 ('  liq > get_cf_raw', '', ['run_liquidations_with', 'get_cf_raw']),
 ('delta_sums (any)', '', ['delta_sums']),
 ('  delta_sums > decode', '', ['delta_sums', 'decode']),
 ('  delta_sums > from_bytes/deser', '', ['delta_sums', 'from_']),
 ('build_sums (any)', '', ['build_sums']),
 ('clear_cooldown (any)', '', ['clear_cooldown']),
 ('set_pending (any)', '', ['set_pending']),
 ('end_resident (any thread)', '', ['end_resident']),
 ('running_hash (any)', '', ['running_hash']),
 ('state hash / digest (any)', '', ['digest']),
 ('keccak (any)', '', ['eccak']),
 ('rocksdb::DBImpl::Write* (any)', '', ['DBImpl', 'Write']),
 ('WriteBatch (any)', '', ['WriteBatch']),
 ('memtable insert (any)', '', ['MemTable']),
 ('crc32c (any)', '', ['crc32c']),
 ('compaction (any)', '', ['Compaction']),
 ('rocksdb flush job (any)', '', ['FlushJob']),
 ('trade_rows encode (any)', '', ['trade_rows']),
 ('commit persist (any)', '', ['persist']),
 ('overlay / PendingState drop (any)', '', ['drop_in_place', 'NativeStateOverlay']),
 ('BTreeMap drop (any)', '', ['drop_in_place', 'BTreeMap']),
]
tot = 0; g = Counter(); kc = Counter()
for l in open(d + '/perf.folded'):
    s, n = l.rsplit(' ', 1); n = int(n); fr = s.split(';'); tot += n; c = fr[0]
    for name, p in GROUPS:
        if c.startswith(p): g[name] += n; break
    else: g['other: ' + re.sub(r'\d+$', '', c)[:20]] += n
    body = fr[1:]
    for lab, cp, subs in KEYS:
        if cp and not c.startswith(cp): continue
        if all(any(x in f for f in body) for x in subs):
            kc[lab] += n
ms = lambda v, cpu=ut: v / tot * cpu * 1000
out = {'cell': d.rsplit('/', 1)[-1], 'span_s': round(span, 1), 'native_blocks': blks, 'fills': fills,
       'fills_per_blk': round(fills / blks), 'utime_s': ut, 'stime_s': st,
       'proc_user_ms_per_blk': round(ut * 1000 / blks, 1), 'proc_sys_ms_per_blk': round(st * 1000 / blks, 1),
       'proc_user_ms_per_1k': round(ut * 1e6 / fills, 3), 'proc_sys_ms_per_1k': round(st * 1e6 / fills, 3),
       'groups': {k: [round(ms(v) / blks, 2), round(ms(v) / fills * 1000, 4)] for k, v in g.most_common(16)},
       'keys': {lab: [round(ms(kc[lab]) / blks, 2), round(ms(kc[lab]) / fills * 1000, 4)] for lab, _, _ in KEYS}}
print(json.dumps(out, indent=1))
