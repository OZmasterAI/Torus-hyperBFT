# thr.py <thread-prefix> <mode self|incl|path> [N]: per-thread symbol ms CPU per 1k fills, crab r1, r2, mean vs main-prof
import sys,re
from collections import Counter
R='/home/oz/bench-results-matched/'
cells=['ozarchy-c58775f-10m-crab-r1','ozarchy-c58775f-10m-crab-r2','ozarchy-main-prof-10m']
pre,mode=sys.argv[1],sys.argv[2]; N=int(sys.argv[3]) if len(sys.argv)>3 else 30
def met(f):
    m={}
    for l in open(f):
        p=l.split()
        if len(p)>=2 and not l.startswith('#'):
            try: m[p[0]]=float(p[1])
            except: pass
    return m
def short(s):
    s=re.sub(r'<[^<>]*>','',s); s=re.sub(r'<[^<>]*>','',s); s=re.sub(r'<[^<>]*>','',s)
    return s[:110]
res={}
for c in cells:
    d=R+c
    a,b=met(d+'/prof-metrics-before.txt'),met(d+'/prof-metrics-after.txt')
    fills=b['torus_orders_matched_total']-a['torus_orders_matched_total']
    sa=open(d+'/prof-stat-before.txt').read().split(')')[1].split(); sb=open(d+'/prof-stat-after.txt').read().split(')')[1].split()
    ut=(int(sb[11])-int(sa[11]))/100
    tot=0; cnt=Counter(); thr=0
    for l in open(d+'/perf.folded'):
        s,n=l.rsplit(' ',1); n=int(n); fr=s.split(';'); tot+=n
        if not fr[0].startswith(pre): continue
        thr+=n
        if mode=='self': cnt[short(fr[-1])]+=n
        elif mode=='incl':
            for x in set(short(y) for y in fr[1:]): cnt[x]+=n
        else:
            cnt[' > '.join(short(y)[:60] for y in fr[-int(mode):])]+=n
    K=ut/fills*1e6/tot
    res[c]={k:v*K for k,v in cnt.items()}; res[c]['__THREAD__']=thr*K
keys=set().union(*[set(r) for r in res.values()])
rows=[(k,res[cells[0]].get(k,0),res[cells[1]].get(k,0),res[cells[2]].get(k,0)) for k in keys]
rows=[(k,a,b,(a+b)/2,m,(a+b)/2-m) for k,a,b,m in rows]
rows.sort(key=lambda r:-max(r[3],r[4]))
print('symbol | crab r1 | crab r2 | crab mean | main | delta')
for r in rows[:N]: print(r[0],*[f'{x:.3f}' for x in r[1:]],sep=' | ')
print('--- by |delta|')
rows.sort(key=lambda r:-abs(r[5]))
for r in rows[:N]: print(r[0],*[f'{x:.3f}' for x in r[1:]],sep=' | ')
