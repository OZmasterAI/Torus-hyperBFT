import json, os, re, statistics as st, sys, gzip
R="/home/oz/bench-results-matched"; P=R+"/ozarchy-p3t-300m-"
V=("val0","val1","val2"); MB=1e6
ARMS={"c":"c","a":"a","k":"k","cS":"cS","aS":"aS"}
def jl(p): return json.load(open(p))
def io_proc(cell,t0,t1):
    rows={}
    for l in open(cell+"/io-ticks.txt"):
        p=l.split()
        if len(p)>=6: rows.setdefault(p[1],[]).append((float(p[0]),int(p[3]),int(p[5])))
    wb=wc=0;n=0
    for rs in rows.values():
        ins=[r for r in rs if t0<=r[0]<=t1]
        if len(ins)<2: continue
        a,b=ins[0],ins[-1]; wb+=b[2]-a[2]; wc+=b[1]-a[1]; n+=1
    return n, wb, wc
def thr(cell,f):
    d={}
    for l in open(cell+"/"+f):
        p=l.split()
        if len(p)>=8: d[(p[0],p[1])]=(p[2],int(p[4]),int(p[5]),int(p[6])+int(p[7]))
    return d
def grp(n):
    if n.startswith("rocksdb:high"): return "rdb_high"
    if n.startswith("rocksdb:low"): return "rdb_low"
    if n.startswith("torus-flush"): return "flush"
    if n.startswith("torus-execution"): return "exec"
    return "other"
out={}
for tag in sys.argv[1:]:
    c=P+tag; s=jl(c+"/summary.json"); h=s["headline"]
    cpu=[];cpul=[]
    for v in V:
        w=s["proc_cpu_by_node"][v]["whole_run"]; cpu.append((w["user_ms"]+w["sys_ms"])/1000/(w["fills"]/1e6))
        w=s["proc_cpu_by_node"][v]["load"]; cpul.append((w["user_ms"]+w["sys_ms"])/1000/(w["fills"]/1e6))
    t0,t1=s["timing"]["t_bench0"],s["timing"]["t_bench1"]
    n,wb,wc=io_proc(c,t0,t1)
    a=thr(c,"io-threads-start.txt"); b=thr(c,"io-threads-end.txt")
    ts0=float(open(c+"/io-threads-start.txt.ts").read().split()[0]); ts1=float(open(c+"/io-threads-end.txt.ts").read().split()[0])
    g={}
    for k,(nm,w,wcx,cp) in b.items():
        a0=a.get(k,(nm,0,0,0)); x=g.setdefault(grp(nm),[0,0,0])
        x[0]+=w-a0[1]; x[1]+=wcx-a0[2]; x[2]+=cp-a0[3]
    # process wchar over thread window from io-ticks nearest
    n2,wb2,wc2=io_proc(c,ts0-0.5,ts1+0.5)
    rdb_wc=g.get("rdb_high",[0,0,0])[1]+g.get("rdb_low",[0,0,0])[1]
    rdb_wb=g.get("rdb_high",[0,0,0])[0]+g.get("rdb_low",[0,0,0])[0]
    fills=s["funnel_by_node"]["val0"]["delta_orders_matched_total"]
    out[tag]=dict(rc=open(P+tag+".cell.rc").read().strip(),agree=h.get("agreement_verdict"),valid=s["validity"]["verdict"],
      ms=h["matched_s_avg"],blk=h["native_blk_s"],cpu=st.mean(cpu),cpuv=cpu,cpul=st.mean(cpul),
      hand=h.get("handoff_wait_ms"),pipe=h.get("pipelined_ms"),chain=h.get("chain_ms"),ci=h.get("commit_interval_ms_avg"),txb=h.get("txs_per_block_avg"),
      wr_MBps=wb/3/(t1-t0)/MB, wchar_MBps=wc/3/(t1-t0)/MB, bench_s=t1-t0, tdur=ts1-ts0,
      wal_wchar_MBps=(wc2-rdb_wc)/3/(ts1-ts0)/MB, wal_wb_MBps=(wb2-rdb_wb)/3/(ts1-ts0)/MB,
      rdb_wb_MBps=rdb_wb/3/(ts1-ts0)/MB,
      thr={k:[x[0]/3/(ts1-ts0)/MB, x[1]/3/(ts1-ts0)/MB, x[2]/100/3/(fills/1e6)] for k,x in g.items()},
      fills=fills, wc_total_GB=wc/3/1e9, wb_total_GB=wb/3/1e9)
json.dump(out,open("/tmp/p3t-analyst/cells.json","w"),indent=0)
for k,v in out.items():
    print(k, v["rc"], v["agree"], v["valid"], "ms=%.0f cpu=%.3f cpul=%.3f blk=%.2f hand=%s pipe=%s wr=%.1f wchar=%.1f walwc=%.1f walwb=%.1f rdbwb=%.1f" % (v["ms"],v["cpu"],v["cpul"],v["blk"],v["hand"],v["pipe"],v["wr_MBps"],v["wchar_MBps"],v["wal_wchar_MBps"],v["wal_wb_MBps"],v["rdb_wb_MBps"]))
    print("   thr", {k2:[round(y,3) for y in x] for k2,x in v["thr"].items()})
