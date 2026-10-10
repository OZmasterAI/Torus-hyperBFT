import json,os,re,statistics as st,collections,sys
R='/home/oz/bench-results-matched/'; P='ozarchy-acc2-300m-'
APP=("cf_native_trades","cf_native_user_trades","cf_native_pending")
ARMS={a:[f'{a}-r{i}' for i in range(1,5)] for a in ('b','w','wd','t')}
def thr(cell):
    def load(p):
        d={}
        for l in open(p):
            x=l.split()
            if len(x)>=6: d[(x[0],x[1])]=(x[2],int(x[4]))
        return d
    s=load(cell+'/io-threads-start.txt'); e=load(cell+'/io-threads-end.txt')
    t0=float(open(cell+'/io-threads-start.txt.ts').read()); t1=float(open(cell+'/io-threads-end.txt.ts').read())
    rk=app=0
    for k,(n,w) in e.items():
        dw=w-s.get(k,(n,0))[1]
        if n.startswith('rocksdb:'): rk+=dw
        else: app+=dw
    return app/(t1-t0)/1e6, rk/(t1-t0)/1e6
def logwin(path,t0,t1):
    cr={};de={}
    for line in open(path,errors='replace'):
        i=line.find('EVENT_LOG_v1 ')
        if i<0: continue
        try: d=json.loads(line[i+13:])
        except: continue
        ev=d.get('event'); t=d['time_micros']/1e6
        if ev=='table_file_creation' and d.get('file_size',0)>0: cr[d['file_number']]=(d['cf_name'],d['file_size'],t,d.get('table_properties',{}))
        elif ev=='table_file_deletion': de[d['file_number']]=t
    def live(T,cfs=None):
        return sum(sz for n,(cf,sz,t,_) in cr.items() if t<=T and (n not in de or de[n]>T) and (cfs is None or cf in cfs))
    return dict(live0=live(t0),live1=live(t1),app0=live(t0,APP),app1=live(t1,APP),
                liveend=live(1e12),append_=live(1e12,APP))
out={}
for a,tags in ARMS.items():
    for tag in tags:
        c=R+P+tag; s=json.load(open(c+'/summary.json')); h=s['headline']
        t0,t1=s['timing']['t_bench0'],s['timing']['t_bench1']
        cpu=[];
        for v in ('val0','val1','val2'):
            w=s['proc_cpu_by_node'][v]['whole_run']; cpu.append((w['user_ms']+w['sys_ms'])/1000/(w['fills']/1e6))
        env=open(c+'/node-environ-trie.txt').read()
        md5=sorted(set(re.findall(r'exe_md5=(\w+)',env)))
        knobs=sorted(set(re.findall(r'(TORUS_ROCKSDB_MAX_TOTAL_WAL_MB=\S*|TORUS_ROCKSDB_WAL_COMPRESSION=\S*|TORUS_TRADE_HISTORY=\S*)',env)))
        # io-ticks
        rows=collections.defaultdict(list)
        for l in open(c+'/io-ticks.txt'):
            p=l.split()
            if len(p)>=6: rows[p[1]].append((float(p[0]),int(p[5])))
        wb=0
        for rs in rows.values():
            ins=[r for r in rs if t0<=r[0]<=t1]; wb+=ins[-1][1]-ins[0][1]
        wmb=wb/(t1-t0)/1e6
        app,rk=thr(c)
        lw=[logwin(f'{c}/rocksdb-LOG-{v}.txt',t0,t1) for v in ('val0','val1','val2')]
        du={}
        for l in open(c+'/data-du.txt'):
            p=l.split(); du[p[0]]=(int(p[2]),int(p[4]),int(p[-1]))
        trows=[];codec=[]
        for v in ('val0','val1','val2'):
            r=0
            for l in open(f'{c}/lsm-{v}.txt'):
                m=re.match(r'cf=(\S+) files=(\d+) bytes=(\d+) rows=(\d+)',l)
                if m and m.group(1) in APP[:2]: r+=int(m.group(4))
            trows.append(r)
            codec.append(re.findall(r'wal_compression_opt=(\d+)',open(f'{c}/codec-check-{v}.txt').read()))
        rc=open(R+P+tag+'.cell.rc').read().strip()
        bs=t1-t0
        o=dict(rc=rc,agree=h.get('agreement_verdict'),live=h.get('liveness_verdict'),valid=s['validity']['verdict'],
           stale=(s.get('oracle_feed') or {}).get('stale_marks_at_bench_end'),md5=md5,knobs=knobs,codec=codec,
           m=h['matched_s_avg'],blk=h['blk_s_avg'],cpu=st.mean(cpu),cpuv=[round(x,2) for x in cpu],
           wr=wmb,app=app,rk=rk,
           sstwin=st.mean([(x['live1']-x['live0'])/bs*86400/1e9 for x in lw]),
           appwin=st.mean([(x['app1']-x['app0'])/bs*86400/1e9 for x in lw]),
           liveend=st.mean([x['liveend'] for x in lw])/1e9, appendlive=st.mean([x['append_'] for x in lw])/1e9,
           du_tot=st.mean([v[0] for v in du.values()])/1e9, du_sst=st.mean([v[1] for v in du.values()])/1e9, du_wal=st.mean([v[2] for v in du.values()])/1e9,
           trows=trows, bs=bs)
        out[tag]=o
        print(tag,rc,o['agree'],o['live'],o['valid'],o['stale'],md5,knobs,codec,'m=%.1f cpu=%.2f %s wr=%.1f app=%.1f rk=%.1f sstwin=%.1f appwin=%.1f liveend=%.2f du=%.2f/%.2f/%.2f trows=%s'%(o['m'],o['cpu'],o['cpuv'],wmb,app,rk,o['sstwin'],o['appwin'],o['liveend'],o['du_tot'],o['du_sst'],o['du_wal'],trows))
json.dump(out,open('/tmp/claude-1000/acc2-analyst/cells.json','w'))
print()
b={k:st.mean([out[t][k] for t in ARMS['b']]) for k in ('m','cpu','wr','app','rk','sstwin','appwin','du_tot','du_wal','du_sst')}
for a,tags in ARMS.items():
    for k in ('m','cpu','wr','app','rk','sstwin','appwin','du_tot','du_sst','du_wal'):
        xs=[out[t][k] for t in tags]; xb=[out[t][k] for t in ARMS['b']]
        mu=st.mean(xs); sd=st.stdev(xs); psd=((st.variance(xs)+st.variance(xb))/2)**.5
        step=mu-b[k]; se=psd*(0.5)**.5
        print(f'{a:3} {k:7} mean {mu:10.2f} sd {sd:8.2f} step {step:+9.2f} ({100*step/b[k]:+6.2f}%) pooled_sd {psd:7.2f} step/psd {step/psd if psd else 0:+6.1f} t {step/se if se else 0:+6.1f}')
