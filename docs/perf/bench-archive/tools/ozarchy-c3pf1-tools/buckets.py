#!/usr/bin/env python3
"""buckets.py <cell_dir>: exec-path (comm torus-execution) CPU buckets in ms CPU per 1k fills.
Weights = sample period (cycles:u). ms/1k = share_of_process_user_cycles * process_utime_rate / fills_rate * 1e6."""
import sys, json
from collections import Counter
d = sys.argv[1]
def met(f):
    m = {}
    for l in open(f):
        p = l.split()
        if len(p) >= 2 and not l.startswith('#'):
            try: m[p[0]] = float(p[1])
            except ValueError: pass
    return m
a, b = met(d + '/prof-metrics-before.txt'), met(d + '/prof-metrics-after.txt')
span = float(open(d + '/prof-after.ts').read()) - float(open(d + '/prof-before.ts').read())
fills = b['torus_orders_matched_total'] - a['torus_orders_matched_total']
sa = open(d + '/prof-stat-before.txt').read().split(')')[1].split(); sb = open(d + '/prof-stat-after.txt').read().split(')')[1].split()
ut = (int(sb[11]) - int(sa[11])) / 100; st = (int(sb[12]) - int(sa[12])) / 100
K = lambda share, cpu=ut: share * cpu / fills * 1e6   # ms CPU per 1k fills
# first matching rule wins (stack = any frame contains substring)
PHASE = [
 ('liquidation',      ['run_liquidations', 'liq_view', 'liquidat']),
 ('oracle',           ['begin_block_oracle', 'oracle', 'mark_price']),
 ('margin:same_batch_bid_top_ups', ['same_batch_bid_top_ups']),
 ('maker checks in match', ['maker_fill_fits', 'MakerAccountSource', 'maker_account', 'TakerMarginLimit']),
 ('order-book matching', ['match_parallel_capped_with', 'match_market', 'place_order_with_accounts', 'match_at_level', 'cancel_all_many']),
 ('settle',           ['settle_market_results_parallel', 'compute_market_settle_plan', 'apply_fill', 'settle']),
 ('margin checks (other)', ['prepare_one', 'account_check', 'AccountReader', 'AccountView', 'try_reserve', 'taker_margin', 'placement_need', 'd2_pool_takers', 'margin']),
 ('books drain/save/load', ['drain_book', 'save_books', 'load_books', 'encode_order_row', 'rebuild_one_chunk', 'resident']),
 ('verify/replay guard', ['verify', 'ecrecover', 'secp256k1', 'replay']),
 ('execute_batch_phases inline glue', ['execute_batch_phases']),
]
KIND = [  # leaf-frame kind, for the cross-cut and for samples no phase rule claims
 ('state reads (overlay/RocksDB)', ['as torus_state::backend::StateBackend>', 'rocksdb', 'StateDb>::', 'BalanceCache>::load', 'PositionManager<torus_state::backend::NativeStateOverlay>>::get_position']),
 ('hashing', ['Keccak', 'keccak', 'hash_one', 'sip::', 'Hasher', 'blake', 'sha2', 'sha3']),
 ('alloc/memcpy', ['malloc', 'free', 'realloc', 'memcpy', 'memmove', 'memset', 'finish_grow', 'drop_glue', 'clone', 'grow_one', 'reserve_rehash']),
 ('spawn/sync', ['spawn', 'thread::', 'futex', 'Mutex', 'Condvar', 'park', 'crossbeam', 'scope', 'pthread', 'clone3']),
]
def leafkind(fr):
    leaf = fr[-1]
    if 'same_batch_bid_top_ups' in leaf: return 'compute'
    for name, keys in KIND:
        if any(k in leaf for k in keys): return name
    # state reads anywhere in stack (inlined into callers)
    if any(('get_cf_raw' in x or 'iterate_cf' in x or 'rocksdb' in x) for x in fr[-4:]): return 'state reads (overlay/RocksDB)'
    return 'compute'
tot = 0; ex = 0; ph = Counter(); kd = Counter(); comm = Counter(); exflush = 0
for l in open(d + '/perf.folded'):
    s, n = l.rsplit(' ', 1); n = int(n); fr = s.split(';'); tot += n; comm[fr[0]] += n
    if fr[0].startswith('torus-flush-wor'): exflush += n
    if not fr[0].startswith('torus-execution'): continue
    ex += n
    k = leafkind(fr); kd[k] += n
    for name, keys in PHASE:
        if any(any(key in x for key in keys) for x in fr[1:]):
            ph[name] += n; break
    else:
        ph['other exec (' + k + ')'] += n
out = {'cell': d, 'span_s': round(span, 1), 'fills': fills, 'fills_per_s': round(fills / span), 'utime_s': ut, 'stime_s': st,
       'proc_cpu_ms_per_1k': {'user': round(K(1), 2), 'sys': round(K(1, st), 2), 'total': round(K(1, ut + st), 2)},
       'exec_share_of_user': round(ex / tot, 4), 'exec_ms_per_1k': round(K(ex / tot), 2),
       'flush_worker_ms_per_1k': round(K(exflush / tot), 2),
       'outside_exec_share_of_total_cpu': round(1 - (ex / tot * ut) / (ut + st), 4),
       'buckets_ms_per_1k': {k: round(K(v / tot), 3) for k, v in ph.most_common()},
       'leaf_kind_ms_per_1k': {k: round(K(v / tot), 3) for k, v in kd.most_common()},
       'threads_ms_per_1k_user': {k: round(K(v / tot), 2) for k, v in comm.most_common(10)}}
print(json.dumps(out, indent=1))
