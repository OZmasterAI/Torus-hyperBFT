# val0 per-thread-group CPU (CPU-s per 1M val0 fills) over [t_bench0, t_drain] from tasks.txt (ticks, 100 Hz),
# same convention as ozarchy-14236fa-tools/threads.py, with rocksdb:high / rocksdb:low split and RocksDB LOG per-CF split:
#   compaction CPU per CF = sum compaction_time_cpu_micros of compaction_finished jobs (cf from '[cf] [JOB n]' lines)
#   flush wall per CF = flush_finished - flush_started per job; rocksdb:high CPU apportioned by flush wall share.
import collections,json,re,sys
R='/home/oz/bench-results-matched/'
GROUPS=[('rpc','rpc-worker'),('exec','torus-execution'),('tokio','tokio-rt-worker'),('ingress','torus-ingress-v'),('gossip','torus-gossip-ve'),
        ('flushw','torus-flush-wor'),('rk_high','rocksdb:high'),('rk_low','rocksdb:low'),('trade','torus-trade-wri'),('range','torus-range-com'),('hotstuff','hotstuff'),('da','torus-da')]
STATE_CFS={'cf_native_positions','cf_native_balances','cf_native_order_books','cf_native_markets','cf_native_oracle','cf_native_nonces',
           'cf_native_liquidation','cf_accounts','cf_consensus_meta','cf_block_action_status','cf_staking_rewards','cf_treasury','cf_native_orders','cf_book_order_rows'}
def grp(c):
    for g,p in GROUPS:
        if c.startswith(p): return g
    return 'other'
def logsplit(path,t0,t1):
    jobcf={}; comp=collections.Counter(); compn=collections.Counter(); fst={}; fwall=collections.Counter(); fbytes=collections.Counter(); cwrite=collections.Counter()
    for l in open(path,errors='replace'):
        m=re.search(r'\[(\w+)\] \[JOB (\d+)\]',l)
        if m: jobcf.setdefault(int(m.group(2)),m.group(1))
        if 'EVENT_LOG_v1' not in l: continue
        j=json.loads(l[l.index('{'):])
        t=j['time_micros']/1e6
        if not (t0<=t<=t1): continue
        cf=j.get('cf_name') or jobcf.get(j.get('job'),'?')
        ev=j['event']
        if ev=='compaction_finished':
            cf=jobcf.get(j['job'],'?')
            comp[cf]+=j['compaction_time_cpu_micros']/1e6; compn[cf]+=1; cwrite[cf]+=j.get('total_output_size',0)
        elif ev=='flush_started': fst[j['job']]=t
        elif ev=='flush_finished':
            if j['job'] in fst: fwall[jobcf.get(j['job'],'?')]+=t-fst[j['job']]
        elif ev=='table_file_creation' and j.get('job') in fst and j['job'] not in [k for k in []]:
            fbytes[cf]+=j.get('file_size',0)
    return comp,compn,fwall,cwrite
def row(lab):
    s=json.load(open(R+lab+'/summary.json'))
    t0,t1=s['timing']['t_bench0'],s['timing']['t_drain']
    fills=s['funnel_by_node']['val0']['delta_orders_matched_total']
    by=collections.defaultdict(dict); comm={}
    for line in open(R+lab+'/tasks.txt'):
        ts,tid,c,u,st=line.split(); ts=int(ts)
        if t0-1<=ts<=t1+1: by[tid][ts]=(int(u),int(st)); comm[tid]=c
    pk=sorted(by['PROC']); a,b=by['PROC'][pk[0]],by['PROC'][pk[-1]]
    k=10/1000/(fills/1e6)  # ticks -> CPU-s per 1M fills
    g=collections.Counter(); live=0
    for tid,m in by.items():
        if tid=='PROC': continue
        ks=sorted(m); x,y=m[ks[0]],m[ks[-1]]
        if ks[0]>pk[0]: x=(0,0)
        d=(y[0]-x[0])+(y[1]-x[1]); g[grp(comm[tid])]+=d*k; live+=d
    total=((b[0]-a[0])+(b[1]-a[1]))*k
    g['exited']=total-live*k
    comp,compn,fwall,cwrite=logsplit(R+lab+'/rocksdb-LOG-val0.txt',pk[0],pk[-1])
    fw_tot=sum(fwall.values()) or 1
    st_flush_share=sum(v for c,v in fwall.items() if c in STATE_CFS)/fw_tot
    comp_k={c:v/(fills/1e6) for c,v in comp.items()}
    state_comp=sum(v for c,v in comp_k.items() if c in STATE_CFS)
    out=dict(cell=lab,window='%d..%d'%(pk[0],pk[-1]),fills_M=fills/1e6,val0_total=total,groups={x:round(v,3) for x,v in g.items()},
             comp_per_cf={c:round(v,3) for c,v in sorted(comp_k.items(),key=lambda x:-x[1])},comp_jobs=dict(compn),
             comp_total=sum(comp_k.values()),state_comp=state_comp,flush_wall_share={c:round(v/fw_tot,3) for c,v in sorted(fwall.items(),key=lambda x:-x[1])},
             state_flush_share=st_flush_share,state_rk_high=g['rk_high']*st_flush_share)
    out['ceiling']=g['flushw']+out['state_rk_high']+state_comp
    out['ceiling_upper_all_rk']=g['flushw']+g['rk_high']+state_comp
    return out
if __name__=='__main__':
    res=[row(l) for l in sys.argv[1:]]
    json.dump(res,open(R+'ozarchy-p3s-run/analyst/threads.json','w'),indent=1)
    for r in res:
        print(r['cell'],'fills %.2fM val0 total %.2f'%(r['fills_M'],r['val0_total']),r['groups'])
        print('   comp/cf',r['comp_per_cf'],'jobs',r['comp_jobs'])
        print('   flush wall share',r['flush_wall_share'])
        print('   flushw %.3f rk_high %.3f (state share %.2f -> %.3f) state comp %.3f comp total %.3f rk_low thread %.3f | CEILING %.3f (upper %.3f) = %.1f%% of val0 total'%(r['groups']['flushw'],r['groups']['rk_high'],r['state_flush_share'],r['state_rk_high'],r['state_comp'],r['comp_total'],r['groups']['rk_low'],r['ceiling'],r['ceiling_upper_all_rk'],100*r['ceiling']/r['val0_total']))
