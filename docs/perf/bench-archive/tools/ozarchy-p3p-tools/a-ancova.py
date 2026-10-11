import json, statistics as st
R="/home/oz/bench-results-matched/ozarchy-p3p-300m-"
arms={a:[f"r{i}-{a}" for i in range(1,5)] for a in ("c","a","k","cS","aS")}
rows=[]
for a,tags in arms.items():
    for t in tags:
        s=json.load(open(R+t+"/summary.json"))
        cpu=sum((s["proc_cpu_by_node"][v]["whole_run"]["user_ms"]+s["proc_cpu_by_node"][v]["whole_run"]["sys_ms"])/1000 for v in ("val0","val1","val2"))/3
        fills=st.mean(s["proc_cpu_by_node"][v]["whole_run"]["fills"] for v in ("val0","val1","val2"))/1e6
        rows.append((a,t,cpu,fills))
for a in arms:
    xs=[r for r in rows if r[0]==a]
    print(a,"CPU-s/node mean %.1f sd %.1f | fills M mean %.3f sd %.3f" % (st.mean(r[2] for r in xs),st.stdev(r[2] for r in xs),st.mean(r[3] for r in xs),st.stdev(r[3] for r in xs)), [ (r[1],round(r[2],1),round(r[3],3)) for r in xs])
# common-slope ANCOVA: cpu = alpha_arm + beta*fills
import itertools
names=list(arms)
# within-arm centered regression
sxx=sxy=0
for a in names:
    xs=[r for r in rows if r[0]==a]; mx=st.mean(r[3] for r in xs); my=st.mean(r[2] for r in xs)
    for r in xs: sxx+=(r[3]-mx)**2; sxy+=(r[3]-mx)*(r[2]-my)
beta=sxy/sxx
print("common within-arm slope beta = %.2f CPU-s per 1M fills (marginal)" % beta)
for base,arm in (("c","a"),("c","k"),("cS","aS"),("c","cS")):
    b=[r for r in rows if r[0]==base]; x=[r for r in rows if r[0]==arm]
    mb=(st.mean(r[2] for r in b),st.mean(r[3] for r in b)); mx=(st.mean(r[2] for r in x),st.mean(r[3] for r in x))
    adj=mx[0]-(mb[0]+beta*(mx[1]-mb[1]))
    F=mx[1]
    print(f"{arm} vs {base}: raw total CPU diff {mx[0]-mb[0]:+.1f} CPU-s/node; at equal fills (beta-adjusted) {adj:+.1f} CPU-s/node = {adj/F:+.3f} CPU-s/1M ({100*adj/(mb[0]+beta*(F-mb[1])):+.2f}%)")
# residual sd
res=[]
for a in names:
    xs=[r for r in rows if r[0]==a]; mx=st.mean(r[3] for r in xs); my=st.mean(r[2] for r in xs)
    res+= [r[2]-my-beta*(r[3]-mx) for r in xs]
import math
print("residual sd %.2f CPU-s/node (df %d)" % (math.sqrt(sum(e*e for e in res)/(len(res)-len(names)-1)), len(res)-len(names)-1))
s2=sum(e*e for e in res)/(len(res)-len(names)-1)
print("beta se %.2f" % math.sqrt(s2/sxx))
for base,arm in (("c","a"),("c","k"),("cS","aS")):
    b=[r for r in rows if r[0]==base]; x=[r for r in rows if r[0]==arm]
    dx=st.mean(r[3] for r in x)-st.mean(r[3] for r in b)
    se=math.sqrt(s2*(1/4+1/4)+ (dx**2)*s2/sxx)
    print(arm,base,"adj diff se %.1f CPU-s/node = %.3f CPU-s/1M"%(se, se/st.mean(r[3] for r in x)))
