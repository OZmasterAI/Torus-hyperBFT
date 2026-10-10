# CPU ceiling (owner definition) and a factor-scaled estimate of what Phase 3 removes, per tw cell.
# ceiling = flush worker + rocksdb:high x state-CF flush-wall share + state-CF compaction CPU (val0, CPU-s/1M val0 fills, [t_bench0,t_drain])
# scaled  = flushw x (1 - (b/fB + (1-b)/fK)) + sum_cf stateflush_cf x (1-1/fB_cf) + sum_cf comp_cf x (1-1/fB_cf)
#   b = byte-proportional share of the flush worker from perf (crc32c + string append), fB/fK = state byte/key coalescing factors.
import json,statistics as st
A='/home/oz/bench-results-matched/ozarchy-p3s-run/analyst/'
T=json.load(open(A+'threads.json')); C=json.load(open(A+'wal/coalesce.json'))
b=(0.3661+0.0836+0.3831+0.0916)/2   # tw-p1, tw-p2 perf: crc32c + _M_append inclusive shares of the flush worker
GATE_BASE=24.93
STATE={'cf_native_positions','cf_native_balances','cf_native_order_books','cf_native_markets','cf_native_oracle','cf_native_nonces','cf_native_liquidation','cf_accounts','cf_consensus_meta','cf_block_action_status','cf_staking_rewards','cf_treasury'}
res={}
for wname in ('15 s','plan 15 s (cap h%100)','100 blocks','plan 30 s (cap h%100)','30 s'):
    w=C['windows'][wname]['state']; fB=w['factor_bytes']; fK=w['factor_keys']; pc=w['per_cf']
    rows=[]
    for r in T:
        g=r['groups']; fw=g['flushw']; rk=g['rk_high']; sh=r['flush_wall_share']; tot_sh=sum(sh.values())
        rem_fw=b/fB+(1-b)/fK
        sflush=0; scomp=0
        for cf in STATE:
            fcf=pc.get(cf,{}).get('factor_bytes',1.0)
            sflush+=rk*sh.get(cf,0)/tot_sh*(1-1/fcf)
            scomp+=r['comp_per_cf'].get(cf,0)*(1-1/fcf)
        scaled=fw*(1-rem_fw)+sflush+scomp
        rows.append(dict(cell=r['cell'].replace('ozarchy-',''),flushw=fw,state_flush=r['state_rk_high'],state_comp=r['state_comp'],ceiling=r['ceiling'],
                         scaled=scaled,scaled_fw=fw*(1-rem_fw),scaled_flush=sflush,scaled_comp=scomp,val0_total=r['val0_total']))
    res[wname]=dict(fB=fB,fK=fK,b=b,rows=rows)
json.dump(res,open(A+'ceiling.json','w'),indent=1)
def ms(x): return '%.3f (sd %.3f, n %d)'%(st.mean(x),st.stdev(x),len(x))
for wname,v in res.items():
    print('== window',wname,'fB %.2f fK %.2f b %.3f'%(v['fB'],v['fK'],v['b']))
    for grp,sel in (('p3s fd9e5dfa no-perf (tw-r1,r2)',lambda c:c in('p3s-300m-tw-r1','p3s-300m-tw-r2')),('p3s fd9e5dfa all 4 (r1,r2,p1,p2)',lambda c:c.startswith('p3s') and 'old' not in c),('acc3 tw r1-r4 (2ede76eb)',lambda c:c.startswith('acc3')),('tw-old1',lambda c:'old1' in c),('all 9',lambda c:True)):
        rr=[r for r in v['rows'] if sel(r['cell'])]
        if len(rr)<2:
            r=rr[0]; print('  %-36s flushw %.3f stflush %.3f stcomp %.3f CEIL %.3f (%.2f%% of 24.93) scaled %.3f (%.2f%%) [fw %.3f fl %.3f cp %.3f]'%(grp,r['flushw'],r['state_flush'],r['state_comp'],r['ceiling'],100*r['ceiling']/GATE_BASE,r['scaled'],100*r['scaled']/GATE_BASE,r['scaled_fw'],r['scaled_flush'],r['scaled_comp'])); continue
        f=lambda k:[r[k] for r in rr]
        print('  %-36s flushw %s stflush %s stcomp %s CEIL %s = %.2f%% | scaled %s = %.2f%% [fw %.3f fl %.3f cp %.3f]'%(grp,ms(f('flushw')),ms(f('state_flush')),ms(f('state_comp')),ms(f('ceiling')),100*st.mean(f('ceiling'))/GATE_BASE,ms(f('scaled')),100*st.mean(f('scaled'))/GATE_BASE,st.mean(f('scaled_fw')),st.mean(f('scaled_flush')),st.mean(f('scaled_comp'))))
