import json, statistics as st, math
d=json.load(open("/tmp/p3p-analyst/cells.json"))
arms={a:[f"r{i}-{a}" for i in range(1,5)] for a in ("c","a","k","cS","aS")}
def m(xs): return st.mean(xs)
def sd(xs): return st.stdev(xs)
def stats(key, f=lambda v,k:v[k]):
    r={}
    for a,tags in arms.items():
        xs=[f(d[t],key) for t in tags]; r[a]=xs
    return r
def cmp(r, base, arm, label):
    x=r[arm]; b=r[base]
    diff=m(x)-m(b); ps=math.sqrt((sd(x)**2+sd(b)**2)/2); se=math.sqrt(sd(x)**2/4+sd(b)**2/4)
    # paired by round
    pd=[xi-bi for xi,bi in zip(x,b)]; pse=sd(pd)/2
    print(f"  {label} {arm} vs {base}: {m(x):.3f} vs {m(b):.3f} diff {diff:+.3f} ({100*diff/m(b):+.2f}%) pooled sd {ps:.3f} step/sd {diff/ps:+.1f} welch t {diff/se:+.1f} paired diffs {[round(p,3) for p in pd]} paired t {m(pd)/pse:+.1f} range {min(x):.3f}-{max(x):.3f} vs {min(b):.3f}-{max(b):.3f}")
for key in ("cpu","cpul","ms","wr_MBps","wchar_MBps","wal_wchar_MBps","wal_wb_MBps","rdb_wb_MBps","blk","pipe","hand","chain","ci","txb","wb_total_GB","wc_total_GB"):
    r=stats(key)
    print(key, {a:(round(m(v),3),round(sd(v),3)) for a,v in r.items()})
    for a,b in (("a","c"),("k","c"),("aS","cS"),("cS","c"),("aS","a")): cmp(r,b,a,key)
for g in ("flush","rdb_high","rdb_low","exec","other"):
    for idx,nm in ((2,"cpu/1M"),(0,"wb MB/s")):
        r=stats(g, lambda v,k: v["thr"].get(k,[0,0,0])[idx])
        print("thr",g,nm,{a:(round(m(v),3),round(sd(v),3)) for a,v in r.items()})
# unattributed CPU = per-node load cpu/1M (approx, val0 fills) minus thread sums
r=stats("x", lambda v,k: v["cpul"]-sum(x[2] for x in v["thr"].values()))
print("unattributed (exited threads) cpu/1M", {a:(round(m(v),3),round(sd(v),3)) for a,v in r.items()})
