# callees.py [depth]: exec main thread, inclusive CPU by direct callee of execute_committed_block_with (ms CPU per native block in perf window)
import re,sys
from collections import Counter
R='/home/oz/bench-results-matched/'
cells=['ozarchy-c58775f-10m-crab-r1','ozarchy-c58775f-10m-crab-r2','ozarchy-main-prof-10m']
DEPTH=int(sys.argv[1]) if len(sys.argv)>1 else 1
def short(s):
    for _ in range(5): s=re.sub(r'<[^<>]*>','',s)
    return s[:90]
def met(f):
    m={}
    for l in open(f):
        p=l.rsplit(' ',1)
        if len(p)==2 and not l.startswith('#'):
            try: m[p[0]]=float(p[1])
            except: pass
    return m
res={}
for c in cells:
    d=R+c; a,b=met(d+'/prof-metrics-before.txt'),met(d+'/prof-metrics-after.txt')
    nb=b['torus_exec_engine_seconds_count']-a['torus_exec_engine_seconds_count']
    fills=b['torus_orders_matched_total']-a['torus_orders_matched_total']
    sa=open(d+'/prof-stat-before.txt').read().split(')')[1].split(); sb=open(d+'/prof-stat-after.txt').read().split(')')[1].split()
    ut=(int(sb[11])-int(sa[11]))/100
    tot=0; cnt=Counter()
    for l in open(d+'/perf.folded'):
        s,n=l.rsplit(' ',1); n=int(n); fr=s.split(';'); tot+=n
        if fr[0]!='torus-execution': continue
        idx=[i for i,x in enumerate(fr) if 'execute_committed_block_with' in x and 'closure' not in x]
        if idx and any('execution_loop' in x for x in fr[:idx[0]]):
            i=idx[0]
            key=' ; '.join(short(x) for x in fr[i+1:i+1+DEPTH]) if len(fr)>i+1 else '(self) execute_committed_block_with'
            cnt[key]+=n; cnt['__MAIN_EXEC_THREAD_BLOCK__']+=n
        elif any('execution_loop' in x for x in fr):
            cnt['(execution_loop, outside block fn) '+short(fr[-1])[:50]]+=n; cnt['__MAIN_EXEC_THREAD_OTHER__']+=n
        else:
            cnt['__SCOPED_WORKERS__']+=n
    K=ut*1000/tot/nb
    res[c]={k:v*K for k,v in cnt.items()}
    res[c]['__fills_per_blk_k']=fills/nb/1000; res[c]['__native_blocks']=nb
keys=set().union(*[set(r) for r in res.values()])
rows=[(k,res[cells[0]].get(k,0),res[cells[1]].get(k,0),res[cells[2]].get(k,0)) for k in keys]
rows.sort(key=lambda r:-max((r[1]+r[2])/2,r[3]))
print('callee | crab r1 | crab r2 | main | crab-main (ms CPU / native block)')
for k,a,b,m in rows[:int(sys.argv[2]) if len(sys.argv)>2 else 45]: print(k,f'{a:.2f}',f'{b:.2f}',f'{m:.2f}',f'{(a+b)/2-m:+.2f}',sep=' | ')
