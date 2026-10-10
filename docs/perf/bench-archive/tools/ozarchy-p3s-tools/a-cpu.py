# node CPU-s/1M fills = mean over val0-2 of (whole_run user_ms+sys_ms)/1000/(fills/1e6)  (acc3 definition, ozarchy-acc3-tables.py L4)
import json,statistics as st
B='/home/oz/bench-results-matched/'
cells=[('p3s','tw-r1'),('p3s','tw-r2'),('p3s','tw-old1'),('p3s','tw-p1'),('p3s','tw-p2'),('p3s','tw-c1'),('p3s','tw-c2'),('p3s','tw-c3')]+[('acc3','tw-r%d'%i) for i in range(1,5)]+[('acc3','b-r%d'%i) for i in range(1,5)]
out={}
for camp,t in cells:
    d=B+'ozarchy-%s-300m-%s/summary.json'%(camp,t)
    try: s=json.load(open(d))
    except Exception as e: print(camp,t,'missing',e); continue
    v=[]; vb=[]
    for n in ('val0','val1','val2'):
        w=s['proc_cpu_by_node'][n]['whole_run']
        v.append((w['user_ms']+w['sys_ms'])/1000/(w['fills']/1e6))
        vb.append((w['user_ms']+w['sys_ms'])/w['native_blocks'])
    out[camp+':'+t]=dict(cpu=st.mean(v),cpu_v=v,ms_per_blk_v=vb,matched=s['headline']['matched_s_avg'],md5=s['binaries']['torus_node_md5'][:8])
    print('%-14s cpu %.3f  (%s)  ms/native blk val0 %.0f  matched %.1f md5 %s'%(camp+':'+t,st.mean(v),' '.join('%.2f'%x for x in v),vb[0],s['headline']['matched_s_avg'],s['binaries']['torus_node_md5'][:8]))
json.dump(out,open(B+'ozarchy-p3s-run/analyst/cpu.json','w'),indent=1)
def ms(keys):
    x=[out[k]['cpu'] for k in keys]; return st.mean(x),(st.stdev(x) if len(x)>1 else 0)
print('p3s tw fd9e5dfa r1,r2', ms(['p3s:tw-r1','p3s:tw-r2']))
print('acc3 tw r1-4', ms(['acc3:tw-r%d'%i for i in range(1,5)]))
print('acc3 b r1-4', ms(['acc3:b-r%d'%i for i in range(1,5)]))
