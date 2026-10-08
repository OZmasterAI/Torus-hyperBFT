import csv,json,sys
R='/home/oz/bench-results-matched/'
H=['view_duration','view_propose_delay','block_build','view_qc_collect','view_proposal_arrival','view_insert_persist','view_vote_delay','view_vote_gather','view_qc_to_advance','on_committed_block','mempool_remove_committed','commit_persist','exec_chain','exec_block','exec_engine','exec_handoff_wait','commit_interval']
def g(r,k):
    v=r.get(k); 
    try: return float(v)
    except: return None
def seg(rows,a,b):
    ra=min(rows,key=lambda r:abs(float(r['ts'])-a)); rb=min(rows,key=lambda r:abs(float(r['ts'])-b))
    dt=float(rb['ts'])-float(ra['ts'])
    o={'dt':dt}
    for k,n in [('views','torus_consensus_view'),('commit','torus_blocks_committed_total'),('native','torus_exec_native_blocks_total'),('execblk','torus_exec_block_seconds_count'),('matched','torus_orders_matched_total')]:
        o[k]=(g(rb,n)-g(ra,n))/dt
    for h in H:
        s='torus_'+h+'_seconds_sum'; c='torus_'+h+'_seconds_count'
        if s in rb and g(rb,s) is not None and g(rb,c):
            dc=g(rb,c)-g(ra,c)
            o[h]=round(1000*(g(rb,s)-g(ra,s))/dc,1) if dc else None
    return o
for c in sys.argv[1:]:
    t=json.load(open(R+c+'/summary.json'))['timing']
    for node in ['val0']:
        rows=[r for r in csv.DictReader(open(R+c+'/sampler.csv')) if r['node']==node]
        # find fill time: first ts with queue depth >=64
        b0=t['t_bench0']; b1=t['t_bench1']
        fill=next((float(r['ts']) for r in rows if float(r['ts'])>=b0 and g(r,'torus_exec_queue_depth')>=64),None)
        print(f"== {c} {node} fill_at={fill-b0 if fill else None:.0f}s")
        for name,a,b in [('ramp 20s..fill-5',b0+20,fill-5),('full fill+5..bench_end',fill+5,b1),('load',b0,b1)]:
            o=seg(rows,a,b)
            print(name, ' '.join(f"{k}={v:.3g}" if isinstance(v,float) else f"{k}={v}" for k,v in o.items()))
