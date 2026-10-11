# prelim numbers (main session), read-only: CPU-s/1M fills (acc3 formula), matched/s, write MB/s per cell
import json, os, statistics as st, sys
R="/home/oz/bench-results-matched"; P=R+"/ozarchy-p3p-300m-"
MB=1e6
def io(cell,t0,t1):
    rows={}
    for l in open(os.path.join(cell,"io-ticks.txt")):
        p=l.split()
        if len(p)>=6: rows.setdefault(p[1],[]).append((float(p[0]),int(p[3]),int(p[5])))
    wb=wc=0;n=0
    for rs in rows.values():
        r=[x for x in rs if t0<=x[0]<=t1]
        if len(r)<2: continue
        wb+=r[-1][2]-r[0][2]; wc+=r[-1][1]-r[0][1]; n+=1
    return (wb/(t1-t0)/MB, wc/(t1-t0)/MB) if n==3 else (None,None)
arms={a:[f"r{i}-{a}" for i in range(1,5)] for a in ("c","a","k","cS","aS")}
res={}
for a,cells in arms.items():
    for c in cells:
        d=P+c; s=json.load(open(d+"/summary.json"))
        cpu=[]
        for v in ("val0","val1","val2"):
            w=s["proc_cpu_by_node"][v]["whole_run"]; cpu.append((w["user_ms"]+w["sys_ms"])/1000/(w["fills"]/1e6))
        t=s["timing"]; wb,wc=io(d,t["t_bench0"],t["t_bench1"])
        res.setdefault(a,[]).append((st.mean(cpu),s["headline"]["matched_s_avg"],wb,wc,s["binaries"]["torus_node_md5"][:8],s["status"]))
for a,rs in res.items():
    print(a, " | ".join(f"{r[0]:.2f} {r[1]/1e3:.1f}k w{r[2]:.0f} wc{r[3]:.0f} {r[4]} {r[5]}" for r in rs))
def summ(a):
    x=[r[0] for r in res[a]]; m=[r[1] for r in res[a]]; w=[r[2] for r in res[a]]; wc=[r[3] for r in res[a]]
    return st.mean(x),st.stdev(x),min(x),max(x),st.mean(m),st.stdev(m),st.mean(w),st.mean(wc)
print()
for a in res:
    print(a, "CPU %.2f sd %.2f [%.2f-%.2f] | matched/s %.1fk sd %.1fk | write %.0f MB/s wchar %.0f MB/s"%(summ(a)[0],summ(a)[1],summ(a)[2],summ(a)[3],summ(a)[4]/1e3,summ(a)[5]/1e3,summ(a)[6],summ(a)[7]))
print()
for a,b in (("a","c"),("k","c"),("aS","cS"),("cS","c")):
    A,B=summ(a),summ(b)
    print(f"{a} vs {b}: CPU {A[0]-B[0]:+.2f} ({(A[0]/B[0]-1)*100:+.1f}%), ranges A[{A[2]:.2f}-{A[3]:.2f}] B[{B[2]:.2f}-{B[3]:.2f}]; matched/s {(A[4]/B[4]-1)*100:+.1f}%; write MB/s {(A[6]/B[6]-1)*100:+.0f}%; wchar {(A[7]/B[7]-1)*100:+.0f}%")
