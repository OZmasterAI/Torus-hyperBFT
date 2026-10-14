import csv, json, sys, statistics as st
cell=sys.argv[1]; tag=sys.argv[2]; A=sys.argv[3]; node='val1'
T=json.load(open(f'{cell}/summary.json'))['timing']; t0=T['t_bench0']
S=[r for r in csv.DictReader(open(f'{cell}/sampler.csv')) if r['node']==node]
cols={'margin':'torus_exec_phase_margin_seconds_sum','match':'torus_exec_phase_match_seconds_sum','settle':'torus_exec_phase_settle_seconds_sum','ph1':'torus_exec_phase1_actions_seconds_sum','cflush':'torus_exec_cache_flush_seconds_sum','save':'torus_exec_save_books_seconds_sum','verify':'torus_exec_verify_seconds_sum','ckwait':'torus_exec_checkpoint_settle_wait_seconds_sum','upkeep':'torus_exec_checkpoint_upkeep_seconds_sum','erwait':'torus_exec_end_resident_wait_seconds_sum','block':'torus_exec_block_seconds_sum'}
def g(r,k): return float(r[k] or 0)
iv=[]
for a,b in zip(S,S[1:]):
    n=g(b,'torus_exec_engine_seconds_count')-g(a,'torus_exec_engine_seconds_count')
    if n<=0: continue
    d={k:1000*(g(b,c)-g(a,c))/n for k,c in cols.items()}
    d['fills']=(g(b,'torus_orders_matched_total')-g(a,'torus_orders_matched_total'))/n
    d['n']=n; iv.append((float(a['ts']),float(b['ts']),d))
load=[d for a,b,d in iv if t0<=a<=T['t_bench1']]
med={k:st.median(d[k] for d in load) for k in load[0]}
top=json.load(open(f'{A}/p99-{tag}.json'))['top']
keys=['block','fills','margin','match','settle','ph1','cflush','save','verify','ckwait','upkeep','erwait']
print(f'== {tag} per-engine-block, 1 s sampler intervals (val1)'); print('   median    '+' '.join(f'{k:>7s}' for k in keys)); print('             '+' '.join(f'{med[k]:7.1f}' for k in keys))
for t in top:
    s=t0+t['t_rel']; hit=[d for a,b,d in iv if a<=s<b]
    if hit: d=hit[0]; print(f"   h={t['h']:>5s} +{t['t_rel']:5.1f}s "+' '.join(f'{d[k]:7.1f}' for k in keys))
# --- attribution: largest excess component per above-p99 block (1 s interval data, val1)
import collections
blk=json.load(open(f'{A}/p99-{tag}.json'))
cnt=collections.Counter(); rowsout=[]
for t in blk['top']:
    s=t0+t['t_rel']; hit=[d for a,b,d in iv if a<=s<b]
    d=hit[0] if hit else None
    ex={}
    if d:
        work=sum(max(0,d[k]-med[k]) for k in ('margin','match','settle','cflush','save'))
        ex['fill/arrival burst (margin+match+settle+flush+save)']=work if d['fills']>1.3*med['fills'] else 0
        ex['other engine work at normal fills']=work if d['fills']<=1.3*med['fills'] else 0
        ex['checkpoint (upkeep+settle wait)']=max(0,d['upkeep']-med['upkeep'])+max(0,d['ckwait']-med['ckwait'])
        ex['end_resident wait']=max(0,d['erwait']-med['erwait'])
        ex['verify (sig verify)']=max(0,d['verify']-med['verify'])
        ex['phase1 actions']=max(0,d['ph1']-med['ph1'])
    if 'epoch_nonpipelined' in t['f']: ex['epoch boundary (non-pipelined, rewards)']=1e9
    cause=max(ex,key=ex.get) if ex and max(ex.values())>15 else 'unexplained'
    cnt[cause]+=1; rowsout.append((t['h'],round(t['ms']),cause,{k:round(v,1) for k,v in ex.items() if v>5 and v<1e9}))
print(f'-- {tag} attribution of {len(blk["top"])} above-p99 blocks (p99 {blk["p99"]:.0f} ms)')
for r in rowsout: print('  ',r)
for k,v in cnt.most_common(): print(f'   {v:3d} {k}')
