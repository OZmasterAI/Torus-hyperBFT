import json,os,sys,statistics as st
R="/home/oz/bench-results-matched/ozarchy-acc3-300m-"
V=("val0","val1","val2"); MB=1e6; GB=1e9
APP=("cf_native_trades","cf_native_user_trades","cf_native_pending")
def logev(p):
    ev=[]
    for l in open(p,errors="replace"):
        i=l.find("EVENT_LOG_v1 ")
        if i<0 or ("table_file_creation" not in l and "table_file_deletion" not in l): continue
        try: d=json.loads(l[i+13:])
        except ValueError: continue
        ev.append(d)
    return ev
def live_at(ev,t):
    cr={};de=set()
    for d in ev:
        if d["time_micros"]/1e6>t: continue
        if d.get("event")=="table_file_creation" and d.get("cf_name") is not None and int(d.get("file_size",0))>0:
            cr[d["file_number"]]=(d["cf_name"],int(d["file_size"]))
        elif d.get("event")=="table_file_deletion": de.add(d["file_number"])
    tot=sum(s for n,(c,s) in cr.items() if n not in de); ap=sum(s for n,(c,s) in cr.items() if n not in de and c in APP)
    return tot,ap
out={}
for tag in sys.argv[1:]:
    c=R+tag; s=json.load(open(c+"/summary.json"))
    t0,t1=s["timing"]["t_bench0"],s["timing"]["t_bench1"]; dur=t1-t0
    cpu=[]
    for v in V:
        w=s["proc_cpu_by_node"][v]["whole_run"]; cpu.append((w["user_ms"]+w["sys_ms"])/1000/(w["fills"]/1e6))
    rows={}
    for l in open(c+"/io-ticks.txt"):
        p=l.split()
        if len(p)>=6: rows.setdefault(p[1],[]).append((float(p[0]),int(p[5])))
    wb=0
    for rs in rows.values():
        ins=[r for r in rs if t0<=r[0]<=t1]; wb+=ins[-1][1]-ins[0][1]
    def thr(f):
        d={}
        for l in open(c+"/"+f):
            p=l.split()
            if len(p)>=8: d[(p[0],p[1])]=(p[2],int(p[4]))
        return d
    a=thr("io-threads-start.txt"); b=thr("io-threads-end.txt")
    ts0=float(open(c+"/io-threads-start.txt.ts").read()); ts1=float(open(c+"/io-threads-end.txt.ts").read())
    wal=rdb=0
    for k,(nm,wv) in b.items():
        d=wv-(a[k][1] if k in a else 0)
        if nm.startswith("rocksdb:"): rdb+=d
        else: wal+=d
    tdur=ts1-ts0
    du=[l.split() for l in open(c+"/data-du.txt") if l.split() and l.split()[0] in V]
    tot=st.mean(int(p[2]) for p in du)/GB; sst=st.mean(int(p[4]) for p in du)/GB; walg=st.mean(int(p[-1]) for p in du)/GB
    g=[];ga=[]
    for v in V:
        ev=logev(c+"/rocksdb-LOG-"+v+".txt"); x0=live_at(ev,t0); x1=live_at(ev,t1)
        g.append((x1[0]-x0[0])/GB/dur*86400); ga.append((x1[1]-x0[1])/GB/dur*86400)
    h=s["headline"]
    out[tag]=dict(ms=h["matched_s_avg"],cpu=st.mean(cpu),cpuv=cpu,agree=h.get("agreement_verdict"),valid=(s.get("validity") or {}).get("verdict") if isinstance(s.get("validity"),dict) else s.get("validity"),
      allw=wb/3/dur/MB,wal=wal/3/tdur/MB,rdb=rdb/3/tdur/MB,tot=tot,sst=sst,walg=walg,g=st.mean(g),ga=st.mean(ga),dur=dur,tdur=tdur,thr_names=sorted({n for n,_ in b.values() if n.startswith("rocksdb")}))
json.dump(out,open("/tmp/acc3-analyst/cells.json","w"),indent=0)
for k,v in out.items(): print(k, {x:(round(y,2) if isinstance(y,float) else y) for x,y in v.items() if x not in("cpuv",)})
