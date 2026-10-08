import sys, re, collections
f, which = sys.argv[1], sys.argv[2]
R = {'crab': [((4600,4745),'Phase 1 (flatten/cancel_all/execute)'),((4746,4770),'open_order_counts'),((4771,4808),'phase2_reservation_basis'),((4809,4809),'phase2_bid_floors'),
              ((4810,4906),'Phase 2 serial loop (stitch_outcome; prepare_one is own bucket)'),((4907,4922),'excess_by_sender/d2_pool_takers'),((4923,4935),'same_batch_bid_top_ups (inlined part)'),
              ((4936,4943),'pools build (BalanceCache load)'),((4944,4985),'reduce-only tracking (reduce_only_positions_for)'),((4986,5005),'AccountMargins setup (position_px, insert)'),
              ((5006,5100),'Phase 3 setup/match dispatch/drain results'),((5101,5115),'settle_market_results_parallel (inlined)'),((5140,5143),'PositionCache flush_all'),((5144,5146),'BalanceCache flush_all'),((5147,5300),'cum_volume/tail')],
     'main': [((3800,3930),'Phase 1 (flatten/cancel_all/execute)'),((3931,3960),'open_order_counts'),((3961,3993),'phase2_reservation_basis'),
              ((3994,4215),'Phase 2 serial loop (inline prepare: slots, BalanceCache, push)'),((4216,4245),'reduce-only tracking (reduce_only_positions_for)'),
              ((4246,4345),'Phase 3 setup/match dispatch/drain results'),((4346,4360),'settle_market_results_parallel (inlined)'),((4380,4384),'PositionCache flush_all'),((4385,4387),'BalanceCache flush_all'),((4388,4500),'cum_volume/tail')]}[which]
agg = collections.Counter(); sec = None
for l in open(f):
    if l.startswith('==='): sec = l; continue
    if not sec or 'by anchor source line' not in sec: continue
    v, loc = l.split(None, 1); v = float(v)
    m = re.search(r'native_executor\.rs:(\d+)', loc)
    n = int(m.group(1)) if m else -1
    name = next((r for (a, b), r in R if a <= n <= b), 'other (' + loc.strip()[:40] + ')')
    agg[name] += v
for k, v in agg.most_common(): print(f"{v:7.3f} {k}")
print(f"{sum(agg.values()):7.3f} TOTAL")
