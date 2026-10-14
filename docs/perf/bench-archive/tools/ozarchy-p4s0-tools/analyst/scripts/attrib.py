import csv, json, re, sys, collections, statistics as st
cell=sys.argv[1]; tag=sys.argv[2]; A=sys.argv[3]; node='val1'
T=json.load(open(f'{cell}/summary.json'))['timing']; t0=T['t_bench0']
rows=list(csv.DictReader(open(f'{A}/blocks-{tag}-val1.csv'))); byh={int(r['height']):r for r in rows}
L=[r for r in rows if t0<=float(r['t_start'])<=T['t_bench1'] and int(r['native_count'])>0]
ms=sorted(float(r['exec_ms']) for r in L); p99=ms[int(.99*len(ms))]; p50=ms[len(ms)//2]
# per-second histogram increments (buckets.csv cumulative counts)
B=collections.defaultdict(dict)
for r in csv.DictReader(open(f'{cell}/buckets.csv')):
    if r['node']!=node: continue
    B[(r['metric'],r['le'])][int(r['ts'])]=float(r['count'])
def over(metric,le_thr,sec):
    a=B.get((metric,'+Inf'),{}); b=B.get((metric,le_thr),{})
    if sec not in a or sec-1 not in a or sec not in b or sec-1 not in b: return 0
    return (a[sec]-a[sec-1])-(b[sec]-b[sec-1])
# checkpoint samples (2 s): checkpoints_total and flush_worker_checkpoint_seconds_sum
ck=[]; cur=None
for line in open(f'{cell}/ckpt-samples.txt'):
    p=line.split()
    if p and p[0]=='T': cur={'t':float(p[1]),'node':p[2],'ok':p[3]=='ok'}; ck.append(cur); continue
    if cur is None or len(p)<2: continue
    if p[0] in ('torus_exec_checkpoints_total','torus_flush_worker_checkpoint_seconds_sum','torus_flush_worker_checkpoint_seconds_count'): cur[p[0]]=float(p[1])
ck=[c for c in ck if c['node']==node and c['ok'] and 'torus_exec_checkpoints_total' in c]
ck_iv=[]
for a,b in zip(ck,ck[1:]):
    d=b['torus_exec_checkpoints_total']-a['torus_exec_checkpoints_total']
    w=b.get('torus_flush_worker_checkpoint_seconds_count',0)-a.get('torus_flush_worker_checkpoint_seconds_count',0)
    ck_iv.append((a['t'],b['t'],d,w))
# rocksdb intervals
starts={}; comp=[]; flush=[]
for line in open(f'{cell}/rocksdb-LOG-{node}.txt',errors='replace'):
    m=re.search(r'EVENT_LOG_v1 (\{.*\})',line)
    if not m: continue
    try: e=json.loads(m.group(1))
    except Exception: continue
    ev=e.get('event'); t=e.get('time_micros',0)/1e6; job=e.get('job')
    if ev in ('compaction_started','flush_started'): starts[(ev[:5],job)]=t
    elif ev=='compaction_finished' and ('compa',job) in starts: comp.append((starts[('compa',job)],t,e.get('total_output_size',0)))
    elif ev=='flush_finished' and ('flush',job) in starts: flush.append((starts[('flush',job)],t))
# sampler: matched per second (val1) and stall micros
S=[r for r in csv.DictReader(open(f'{cell}/sampler.csv')) if r['node']==node]
mt={}; stall={}
for a,b in zip(S,S[1:]):
    dt=float(b['ts'])-float(a['ts'])
    if dt<=0: continue
    mt[int(b['ts'])]=(float(b['torus_orders_matched_total'])-float(a['torus_orders_matched_total']))/dt
    stall[int(b['ts'])]=float(b.get('torus_rocksdb_stall_micros') or 0)-float(a.get('torus_rocksdb_stall_micros') or 0)
med_mt=st.median([v for k,v in mt.items() if t0<=k<=T['t_bench1']])
def feats(r):
    h=int(r['height']); s=float(r['t_start']); e=float(r['t_done']); secs=range(int(s),int(e)+2)
    f={}
    f['epoch_nonpipelined']= (h%100==0) or r['pipelined']=='0'
    f['ramp_first8s']= s-t0<8
    f['settle_wait_gt25ms']= any(over('torus_exec_checkpoint_settle_wait_seconds_bucket','0.0256',x)>0 for x in secs)
    f['ckpt_write_inflight']= any(a<=e and b>=s and (d>0 or w>0) for a,b,d,w in ck_iv)
    f['end_res_wait_gt64ms']= any(over('torus_exec_end_resident_wait_seconds_bucket','0.064',x)>0 for x in secs)
    f['compaction_overlap']= any(a<=e and b>=s for a,b,_ in comp)
    f['flush_overlap']= any(a<=e and b>=s for a,b in flush)
    f['rocksdb_stall']= any(stall.get(x,0)>0 for x in secs)
    f['prev_oracle_only']= h-1 in byh and int(byh[h-1]['native_count'])==0
    f['fills_burst_gt1.2med']= max([mt.get(x,0) for x in secs])>1.2*med_mt
    return f
top=[r for r in L if float(r['exec_ms'])>=p99]
F=[(r,feats(r)) for r in L]
keys=list(F[0][1].keys())
print(f'== {tag}: native blocks {len(L)}, p50 {p50:.0f} ms, p99 {p99:.0f} ms, above-p99 {len(top)}; ckpt intervals {len(ck_iv)} compactions {len(comp)} flushes {len(flush)} median matched/s {med_mt:.0f}')
cap=[(r,f) for r,f in F if int(r['native_count'])>=400]
print(f'{"feature":24s} rate_all rate_top  mean_ms(cap400) with/without')
for k in keys:
    ra=sum(f[k] for _,f in F)/len(F); rt=sum(f[k] for r,f in F if float(r['exec_ms'])>=p99)/max(1,len(top))
    w=[float(r['exec_ms']) for r,f in cap if f[k]]; wo=[float(r['exec_ms']) for r,f in cap if not f[k]]
    print(f'{k:24s} {ra:7.2f} {rt:7.2f}   {st.mean(w) if w else float("nan"):6.0f} / {st.mean(wo) if wo else float("nan"):6.0f}  (n={len(w)})')
for r,f in F:
    if float(r['exec_ms'])>=p99:
        print(f"  h={r['height']} t+{float(r['t_start'])-t0:5.1f}s {float(r['exec_ms']):4.0f}ms nc={r['native_count']} pipe={r['pipelined']} :", ','.join(k for k in keys if f[k]))
json.dump({'tag':tag,'p50':p50,'p99':p99,'top':[{'h':r['height'],'t_rel':float(r['t_start'])-t0,'ms':float(r['exec_ms']),'f':[k for k in keys if f[k]]} for r,f in F if float(r['exec_ms'])>=p99]},open(f'{A}/p99-{tag}.json','w'),indent=1)
