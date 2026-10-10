import json,sys,os,collections
R='/home/oz/bench-results-matched'
CELLS=sys.argv[1:]
STATE={'cf_native_positions','cf_native_balances','cf_native_order_books','cf_book_order_rows','cf_native_markets','cf_native_liquidation','cf_native_oracle'}
G={'cf_native_trades':'trades','cf_native_user_trades':'trades','cf_native_pending':'DA','cf_block_bodies':'bodies+hdr','cf_block_headers':'bodies+hdr'}
def grp(cf): return 'state' if cf in STATE else G.get(cf,'other')
def metric(path,name):
    for l in open(path):
        if l.startswith(name+' '): return float(l.split()[1])
def parse(path,t0,t1):
    cjob={};fstart={};C=collections.defaultdict(collections.Counter)
    for line in open(path,errors='replace'):
        if 'EVENT_LOG_v1' not in line: continue
        j=json.loads(line[line.index('{'):]); ev=j.get('event'); job=j.get('job'); t=j['time_micros']/1e6
        if ev=='compaction_started': cjob[job]=j['cf_name']
        elif ev=='compaction_finished':
            if not(t0<=t<=t1): continue
            c=C[cjob.get(job,'?')]; c['c_cpu']+=j['compaction_time_cpu_micros']; c['c_out']+=j['total_output_size']; c['c_n']+=1
        elif ev=='flush_started': fstart[job]=t
        elif ev=='table_file_creation' and job in fstart and job not in cjob:
            st=fstart.pop(job)
            if not(t0<=t<=t1): continue
            c=C[j['cf_name']]; c['f_out']+=j['file_size']; c['f_wall']+=(t-st)*1e6; c['f_n']+=1
    return C
def ticks(cell,t0,t1):
    snap=collections.defaultdict(dict)
    for l in open(f'{R}/{cell}/tasks.txt'):
        p=l.split()
        if len(p)<5 or p[1]=='PROC': continue
        snap[int(p[0])][(p[1],p[2])]=int(p[3])+int(p[4])
    ts=sorted(snap); a=min(ts,key=lambda x:abs(x-t0)); b=min(ts,key=lambda x:abs(x-t1))
    out=collections.Counter()
    for k,v in snap[b].items(): out[k[1]]+=(v-snap[a].get(k,0))/100
    return out,a,b
res={}
for cell in CELLS:
    S=json.load(open(f'{R}/{cell}/summary.json'))
    for v in range(3):
        vn=f'val{v}'; ph=S['phase_by_node'][vn]
        t0=ph['incl_drain']['window'][0]; t1=ph['drain']['window'][0]
        pc=S['proc_cpu_by_node'][vn]['load']; fills=pc['fills']/1e6
        node_cpu=(pc['user_ms']+pc['sys_ms'])/1e3
        LOG=f'{R}/{cell}/rocksdb-LOG-{vn}.txt'
        tm=os.stat(f'{R}/{cell}/metrics-after-{vn}.txt').st_mtime
        Ca=parse(LOG,0,tm); Cf=parse(LOG,0,1e12)
        chk=dict(log_ccpu=sum(c['c_cpu'] for c in Ca.values())/1e6, ctr_ccpu=metric(f'{R}/{cell}/metrics-after-{vn}.txt','torus_rocksdb_compaction_cpu_micros')/1e6,
                 log_cw=sum(c['c_out'] for c in Ca.values())/1e9, ctr_cw=metric(f'{R}/{cell}/metrics-after-{vn}.txt','torus_rocksdb_compact_write_bytes')/1e9,
                 log_fw=sum(c['f_out'] for c in Ca.values())/1e9, ctr_fw=metric(f'{R}/{cell}/metrics-after-{vn}.txt','torus_rocksdb_flush_write_bytes')/1e9,
                 logfull_ccpu=sum(c['c_cpu'] for c in Cf.values())/1e6)
        C=parse(LOG,t0,t1)
        g=collections.defaultdict(collections.Counter); cfs={}
        for cf,c in C.items():
            for k,x in c.items(): g[grp(cf)][k]+=x
            cfs[cf]=dict(c)
        r=dict(chk=chk,fills_M=fills,node_cpu_per_M=node_cpu/fills,window=[t0,t1],groups={k:dict(x) for k,x in g.items()},cfs=cfs)
        if v==0:
            tk,a,b=ticks(cell,t0,t1); r['threads_per_M']={k:tk[k]/fills for k in ['torus-flush-wor','rocksdb:high','rocksdb:low','torus-trade-wri']}; r['tick_win']=[a,b]
        res[f'{cell}/{vn}']=r
json.dump(res,sys.stdout,indent=1)
