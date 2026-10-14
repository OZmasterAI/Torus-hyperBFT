import sys, re, collections, json
sys.path.insert(0, sys.argv[1]); from stacks import samples
path=sys.argv[2]; ev=None if sys.argv[3]=='-' else sys.argv[3]
def nm(x): return re.sub(r'<.*','',x.split(' @')[0])
cats=collections.Counter(); n=0
for tid,t,fr in samples(path,ev):
    n+=1; names=[nm(x) for x in fr]; full=fr
    hit=set()
    # R reads of the two CFs via the overlay (balances / cvlm / dirty positions / positions prefix)
    if 'get_native_balance' in names: hit.add('R_bal')
    if 'get_cum_volume' in names: hit.add('R_cvlm')
    if 'get_position' in names:
        i=max(k for k,s in enumerate(names) if s=='get_position')
        sub=names[:i]
        if sub and not any(s in sub for s in ('find','cloned','resident_positions')): hit.add('R_pos_dirty')
    if 'has_key' in names or 'positions_for_trader' in names: hit.add('R_pos_prefix')
    if any('try_from_slice<torus_core::position::Position>' in s for s in full): hit.add('decode_pos_any')
    # cache-load miss paths
    if 'load' in names:
        i=names.index('load')
        if 'get_native_balance' in names[:i]: hit.add('miss_BalanceCache_load')
    if ('update_in_place' in names or 'settle_market_results_sequential' in names) and 'get_position' in names: hit.add('miss_PositionCache_load')
    # resident_changes (R lookup+clone per pending key; stays per plan unless reworked)
    if 'resident_changes' in names:
        hit.add('resident_changes_all')
        i=names.index('resident_changes'); sub=names[:i]
        if ('search_tree' in sub and 'get' in sub) or any(s in sub for s in ('clone','cloned','to_vec')): hit.add('resident_changes_Rget_clone')
    # memo: pos_sums residual (lock/entry/Arc/OnceLock) + memo drop
    if 'pos_sums' in names:
        i=names.index('pos_sums'); sub=names[:i]
        if not any(s in sub for s in ('dirty_sums','cached_sums','layer_touches','delta_sums')): hit.add('memo_lock_oncelock')
    if any(re.search(r'memo|Memo|OnceLock',s) for s in full): hit.add('memo_lock_oncelock')
    # open_order_counts on the exec thread
    if 'open_order_counts' in names or 'add_open_order_counts' in names: hit.add('open_order_counts')
    if 'end_resident' in names or 'end_resident_on_worker' in names: hit.add('end_resident_inline')
    if hit & {'R_bal','R_cvlm','R_pos_dirty','R_pos_prefix'}: hit.add('R_union_exec')
    strict={'R_bal','R_cvlm','R_pos_dirty','R_pos_prefix','memo_lock_oncelock','open_order_counts'}
    if hit & strict: hit.add('UNION_strict')
    if hit & (strict|{'resident_changes_Rget_clone','decode_pos_any'}): hit.add('UNION_upper')
    for h in hit: cats[h]+=1
out={k:round(100*v/n,3) for k,v in sorted(cats.items())}; out['_samples']=n
print(json.dumps(out))
