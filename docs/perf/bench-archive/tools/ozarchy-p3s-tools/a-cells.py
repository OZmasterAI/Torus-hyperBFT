import json,sys,os
B='/home/oz/bench-results-matched/'
tags=sys.argv[1:] or ['tw-r1','tw-r2','tw-old1','tw-c1','tw-c2','tw-c3','tw-p1','tw-p2']
for t in tags:
    d=B+('ozarchy-p3s-300m-' if not t.startswith('acc3') else 'ozarchy-acc3-300m-')+t.replace('acc3-','')
    s=json.load(open(d+'/summary.json'))
    rc=open(d+'.cell.rc').read().strip() if os.path.exists(d+'.cell.rc') else '?'
    ag=s['agreement']
    nodes=ag.get('nodes',[])
    digs=set(n.get('state_digest') for n in nodes); hashes=set(n.get('block_hash') for n in nodes)
    pf=[n.get('panic_or_failstop_lines') for n in nodes]; er=[n.get('error_lines') for n in nodes]
    agv={k:v for k,v in ag.items() if k!='nodes'}
    p=s['proc_cpu_by_node']
    cpu=[]
    for v in ('val0','val1','val2'):
        w=p[v]['whole_run']; l=p[v]['load']
        cpu.append((v, round((w['user_ms']+w['sys_ms'])/w['fills']*1000,3), round((l['user_ms']+l['sys_ms'])/l['fills']*1000,3), round(w['user_ms']/w['fills']*1000,3)))
    print(t, rc, s['status'], s['binaries'], 'digests',len(digs),'hashes',len(hashes),'pf',pf,'err',er, json.dumps(agv)[:300])
    print('   liveness',s['liveness']['verdict'],'validity',s['validity']['verdict'],s['validity']['fail_reasons'],'matched',s['headline']['matched_s_avg'],'nblk',s['headline']['native_blk_s'],'wall',s['timing']['bench_wall_s'])
    print('   cpu (val, whole (u+s)ms/1kfills=CPU-s/1M, load u+s, whole user)',cpu)
    if s.get('crash'): print('   crash', json.dumps(s['crash'])[:1500])
