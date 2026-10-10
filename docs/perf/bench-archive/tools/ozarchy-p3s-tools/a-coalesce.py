# Coalescing factor and layer size from the decoded tw-r1 val0 WAL (waldec output).
# written = sum(klen+vlen) of every WAL entry; last-value = sum(klen+vlen) of the LAST write of each distinct (cf,key) in a window
# (a delete counts klen). Windows over EXECUTION time (val0 'execution pipeline: block done' timestamps), load window only.
import numpy as np, json, sys, collections
A='/home/oz/bench-results-matched/ozarchy-p3s-run/analyst/wal/'
S=json.load(open('/home/oz/bench-results-matched/ozarchy-p3s-300m-tw-r1/summary.json'))
T0=S['timing']['t_bench0']; T1=S['timing']['t_bench1']
CF={}
import re
for l in open('/home/oz/bench-results-matched/ozarchy-p3s-300m-tw-r1/rocksdb-LOG-val0.txt',errors='replace'):
    m=re.search(r'Created column family \[(\w+)\] \(ID (\d+)\)',l)
    if m: CF[int(m.group(2))]=m.group(1)
CF[0]='default'
bt=np.dtype([('seq','<u8'),('count','<u4'),('file','<u4'),('ah','<u8'),('pb','<u8'),('ne','<u4'),('fl','<u4')])
et=np.dtype([('batch','<u4'),('cf','u1'),('op','u1'),('pad','<u2'),('kl','<u4'),('vl','<u4'),('kh','<u8')])
B=np.fromfile(A+'batches.bin',dtype=bt); E=np.fromfile(A+'entries.bin',dtype=et)
tm=json.load(open(A+'tw-r1-val0-times.json')); done={int(k):v for k,v in tm['done'].items()}; start={int(k):v for k,v in tm['start'].items()}
NONE=np.uint64(2**64-1)
isstate=B['ah']!=NONE
hs=B['ah'][isstate].astype(np.int64)
res={'n_batches':int(len(B)),'n_entries':int(len(E)),'n_state_batches':int(isstate.sum()),'state_h_min':int(hs.min()),'state_h_max':int(hs.max()),
     'state_h_consecutive':bool(np.all(np.diff(hs)==1)),'log_done_heights':len(done)}
# block of each batch = last state height at or before it in WAL order (0 before first)
idx=np.where(isstate)[0]
blk_of_batch=np.zeros(len(B),dtype=np.int64)
pos=np.searchsorted(idx,np.arange(len(B)),side='right')-1
blk_of_batch[pos>=0]=hs[pos[pos>=0]]
eb=E['batch'].astype(np.int64)
eblk=blk_of_batch[eb]; est=isstate[eb]
ebytes=(E['kl'].astype(np.int64)+np.where((E['op']==4)|(E['op']==0)|(E['op']==7)|(E['op']==8),0,E['vl'].astype(np.int64)))
ecf=E['cf'].astype(np.int64); kh=E['kh']
# heights with exec done inside the load window
H=np.array(sorted(h for h,t in done.items() if T0<=t<=T1))
res['load_heights']=[int(H.min()),int(H.max()),int(len(H))]
tdone=np.zeros(hs.max()+2); 
for h,t in done.items():
    if h<len(tdone): tdone[h]=t
inload=np.zeros(hs.max()+2,dtype=bool); inload[H]=True
# check: state batch per height present
res['state_batches_for_load_heights']=int(np.isin(H,hs).sum())
# per-entry 'payload' check: entries per state batch vs native activity
def windows(kind,W=None):
    """returns wid per height (array over heights, -1 = not in a complete window)"""
    wid=np.full(len(tdone),-1,dtype=np.int64)
    if kind=='block':
        wid[H]=H
    elif kind=='time':
        n=int((T1-T0)//W)
        w=np.floor((tdone[H]-T0)/W).astype(np.int64)
        ok=w<n
        wid[H[ok]]=w[ok]
    elif kind=='blocks':
        w=H//W
        full=[x for x in np.unique(w) if np.all(np.isin(np.arange(x*W,(x+1)*W),H))]
        ok=np.isin(w,full); wid[H[ok]]=w[ok]
    elif kind=='plan':   # plan trigger: checkpoint at end of block n if exec time since last checkpoint >= W or n%100==0
        cur=0; tstart=None; k=0; 
        # anchor: first window starts after the first h%100==0 boundary or T0 (first block of load)
        prev_end_t=None
        for h in H:
            if prev_end_t is None: prev_end_t=tdone[h-1] if h-1>=0 and tdone[h-1]>0 else tdone[h]
            wid[h]=k
            if (tdone[h]-prev_end_t>=W) or (h%100==0):
                k+=1; prev_end_t=tdone[h]
        # drop first and last (partial) windows
        last=wid[H[-1]]
        wid[(wid==0)|(wid==last)]=-1
    return wid
def analyse(wid,mask_state):
    m=(wid[eblk]>=0)
    if mask_state is not None: m&=(est==mask_state)
    sel=np.where(m)[0]
    w=wid[eblk[sel]]; k=kh[sel]; b=ebytes[sel]; c=ecf[sel]
    # last write per (window,key): sort stable by (w,k) keeping entry order
    o=np.lexsort((sel,k,w))
    w2=w[o];k2=k[o]
    last=np.ones(len(o),dtype=bool); last[:-1]=(w2[1:]!=w2[:-1])|(k2[1:]!=k2[:-1])
    lb=b[o][last]; lc=c[o][last]; lw=w2[last]
    out={}
    wins=np.unique(w)
    # totals per window
    wb=np.bincount(np.searchsorted(wins,w),weights=b,minlength=len(wins))
    wk=np.bincount(np.searchsorted(wins,w),minlength=len(wins))
    lbw=np.bincount(np.searchsorted(wins,lw),weights=lb,minlength=len(wins))
    lkw=np.bincount(np.searchsorted(wins,lw),minlength=len(wins))
    nblk=np.array([np.sum(wid[H]==x) for x in wins])
    out['n_windows']=int(len(wins)); out['blocks_per_window_mean']=float(nblk.mean()); out['blocks_per_window_min_max']=[int(nblk.min()),int(nblk.max())]
    out['written_bytes']=float(wb.sum()); out['written_keys']=int(wk.sum()); out['last_bytes']=float(lbw.sum()); out['distinct_keys']=int(lkw.sum())
    out['factor_bytes']=float(wb.sum()/lbw.sum()); out['factor_keys']=float(wk.sum()/lkw.sum())
    out['last_bytes_per_window_mean']=float(lbw.mean()); out['last_bytes_per_window_peak']=float(lbw.max()); out['last_bytes_per_window_min']=float(lbw.min())
    out['written_bytes_per_window_mean']=float(wb.mean()); out['written_bytes_per_window_peak']=float(wb.max())
    out['distinct_keys_per_window_peak']=int(lkw.max())
    out['factor_per_window_min_med_max']=[float(x) for x in np.percentile(wb/lbw,[0,50,100])]
    per={}
    for cf in np.unique(c):
        mw=c==cf; ml=lc==cf
        per[CF.get(int(cf),str(cf))]=dict(written=float(b[mw].sum()),keys=int(mw.sum()),last=float(lb[ml].sum()),distinct=int(ml.sum()),
            factor_bytes=float(b[mw].sum()/max(1,lb[ml].sum())),factor_keys=float(mw.sum()/max(1,ml.sum())),
            peak_last_per_window=float(np.bincount(np.searchsorted(wins,lw[ml]),weights=lb[ml],minlength=len(wins)).max()))
    out['per_cf']=per
    return out
WIN=[('1 block','block',None),('5 s','time',5),('15 s','time',15),('30 s','time',30),('100 blocks','blocks',100),
     ('plan 15 s (cap h%100)','plan',15),('plan 30 s (cap h%100)','plan',30)]
res['windows']={}
for name,kind,W in WIN:
    wid=windows(kind,W)
    res['windows'][name]={'state':analyse(wid,True),'all':analyse(wid,None),'nonstate':analyse(wid,False)}
# load-window totals and rates
m=inload[eblk]
res['load_window_s']=float(T1-T0)
res['load_written_bytes_state']=float(ebytes[m&est].sum()); res['load_written_bytes_all']=float(ebytes[m].sum())
res['load_wal_payload_bytes_all']=float(B['pb'][inload[blk_of_batch]].sum()); res['load_wal_payload_bytes_state']=float(B['pb'][inload[blk_of_batch]&isstate].sum())
res['load_span_exec_s']=float(tdone[H].max()-tdone[H].min())
# state CFs seen in state batches vs non-state
res['cfs_state']={CF.get(int(c),str(c)):int(n) for c,n in zip(*np.unique(ecf[est],return_counts=True))}
res['cfs_nonstate']={CF.get(int(c),str(c)):int(n) for c,n in zip(*np.unique(ecf[~est],return_counts=True))}
json.dump(res,open(A+'coalesce.json','w'),indent=1)
print(json.dumps({k:v for k,v in res.items() if k!='windows'},indent=0)[:3000])
for name in res['windows']:
    for part in ('state','all'):
        o=res['windows'][name][part]
        print('%-22s %-5s nwin %3d blk/win %.1f written %.1f MB last %.1f MB factor B %.2f keys %.2f | per-win last mean %.1f peak %.1f MB | f min/med/max %s'%(name,part,o['n_windows'],o['blocks_per_window_mean'],o['written_bytes']/1e6,o['last_bytes']/1e6,o['factor_bytes'],o['factor_keys'],o['last_bytes_per_window_mean']/1e6,o['last_bytes_per_window_peak']/1e6,['%.2f'%x for x in o['factor_per_window_min_med_max']]))
