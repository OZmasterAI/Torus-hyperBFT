import json,statistics as st,math
c=json.load(open("/tmp/acc3-analyst/cells.json"))
A={a:[c[f"{a}-r{i}"] for i in range(1,5)] for a in ("b","tw","twd")}
for m in ("cpu","ms","allw","wal","rdb","tot","sst","walg","g","ga"):
    print("==",m)
    S={a:[x[m] for x in A[a]] for a in A}
    for a in A: print(f"  {a}: mean {st.mean(S[a]):.3f} sd {st.stdev(S[a]):.3f} min {min(S[a]):.2f} max {max(S[a]):.2f} vals {[round(v,2) for v in S[a]]}")
    for x,y in (("tw","b"),("twd","b"),("twd","tw")):
        d=st.mean(S[x])-st.mean(S[y]); ps=math.sqrt((st.variance(S[x])+st.variance(S[y]))/2); se=ps*math.sqrt(0.5)
        ov = not (min(S[x])>max(S[y]) or max(S[x])<min(S[y]))
        print(f"  {x} vs {y}: step {d:+.3f} ({100*d/st.mean(S[y]):+.2f}%) pooled sd {ps:.3f} step/sd {d/ps:+.1f} t {d/se:+.1f} ranges overlap {ov}")
