import json,statistics as st,sys
d=json.load(open('/tmp/claude-1000/p3s0cf-analysis/res.json'))
S={c:json.load(open(f'/home/oz/bench-results-matched/{c}/summary.json')) for c in {k.split('/')[0] for k in d}}
GR=['trades','DA','state','bodies+hdr','other']
def cellstats(cell):
    out={}
    for v in range(3):
        r=d[f'{cell}/val{v}']; g=r['groups']; f=r['fills_M']
        T={k:sum(x.get(k,0) for x in g.values()) for k in ['c_cpu','c_out','f_out','f_wall']}
        for gr in GR:
            x=g.get(gr,{})
            for name,val in [('cpu_sh',x.get('c_cpu',0)/T['c_cpu']),('cpu_M',x.get('c_cpu',0)/1e6/f),('gb_M',x.get('c_out',0)/1e9/f),('cw_sh',x.get('c_out',0)/T['c_out']),('fw_sh',x.get('f_out',0)/T['f_out']),('ft_sh',x.get('f_wall',0)/T['f_wall'])]:
                out.setdefault((gr,name),[]).append(val)
        for name,val in [('tot_cpu_M',T['c_cpu']/1e6/f),('tot_gb_M',T['c_out']/1e9/f),('tot_fw_M',T['f_out']/1e9/f)]:
            out.setdefault(('ALL',name),[]).append(val)
        pc=S[cell]['proc_cpu_by_node'][f'val{v}']['load']
        out.setdefault(('ALL','user_M'),[]).append(pc['user_ms_per_1k_fills']); out.setdefault(('ALL','usys_M'),[]).append(pc['user_ms_per_1k_fills']+pc['sys_ms_per_1k_fills'])
    t=d[f'{cell}/val0']['threads_per_M']
    out[('THR','flushw')]=[t['torus-flush-wor']]; out[('THR','rdbhigh')]=[t['rocksdb:high']]; out[('THR','rdblow')]=[t['rocksdb:low']]
    return {k:st.mean(v) for k,v in out.items()}
cells={c:cellstats(c) for c in ['ozarchy-p3s0cf-300m-k-r1','ozarchy-p3s0cf-300m-k-r2','ozarchy-p3s0c-300m-k-p2','ozarchy-p3s0cf-300m-m-r1','ozarchy-p3s0cf-300m-m-r2']}
json.dump({c:{'|'.join(k):v for k,v in s.items()} for c,s in cells.items()},open('/tmp/claude-1000/p3s0cf-analysis/cells.json','w'),indent=1)
keys=list(cells['ozarchy-p3s0cf-300m-k-r1'])
print('%-22s'%'metric',' k-r1   k-r2   k-p2  | m-r1   m-r2  | kmean(r1r2) mmean  step  pooled_sd step/sd')
for k in keys:
    a=[cells[c][k] for c in ['ozarchy-p3s0cf-300m-k-r1','ozarchy-p3s0cf-300m-k-r2','ozarchy-p3s0c-300m-k-p2']]
    b=[cells[c][k] for c in ['ozarchy-p3s0cf-300m-m-r1','ozarchy-p3s0cf-300m-m-r2']]
    ka=st.mean(a[:2]); mb=st.mean(b); sd=((st.variance(a[:2])+st.variance(b))/2)**.5
    print('%-22s'%'|'.join(k),' '.join('%6.3f'%x for x in a),'|',' '.join('%6.3f'%x for x in b),'| %6.3f %6.3f %+6.3f %6.3f %5.1f'%(ka,mb,mb-ka,sd,(mb-ka)/sd if sd else 0))
