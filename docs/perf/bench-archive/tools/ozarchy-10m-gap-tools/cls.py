# cls.py <thread-prefix> : classify thread samples by rule list (first match on any frame), ms/1k fills + us per request
import sys,re
from collections import Counter
R='/home/oz/bench-results-matched/'
cells=['ozarchy-c58775f-10m-crab-r1','ozarchy-c58775f-10m-crab-r2','ozarchy-main-prof-10m']
pre=sys.argv[1]
RULES={
'rpc-worker':[('hex decode (from_hex)',['FromHex','from_hex','Chunks<u8>']),('keccak (action hash)',['Keccak','keccak']),
 ('bincode decode',['decode_action_bin','bincode']),('secp verify',['secp256k1','ecrecover','recover']),('mempool admit/insert',['mempool','Mempool','admit']),
 ('JSON / HTTP / jsonrpsee',['serde_json','skip_to_escape','jsonrpsee','hyper','method_weight','http::','tower','deserialize_raw_value']),('alloc',['malloc','realloc','free','memcpy','memmove']),],
'torus-gossip-ve':[('keccak',['Keccak','keccak']),('secp verify',['secp256k1','ecrecover','recover_','verify_prehash','k256','ecdsa']),('bincode',['bincode','deserialize']),('mempool insert',['mempool','Mempool','insert']),('alloc',['malloc','realloc','free','memcpy','memmove'])],
'torus-ingress-v':[('keccak',['Keccak','keccak']),('secp verify',['secp256k1','ecrecover','recover_','verify_prehash','k256','ecdsa']),('bincode',['bincode','deserialize']),('mempool insert',['mempool','Mempool','insert']),('alloc',['malloc','realloc','free','memcpy','memmove'])],
}[pre]
def met(f):
    m={}
    for l in open(f):
        p=l.rsplit(' ',1)
        if len(p)==2 and not l.startswith('#'):
            try: m[p[0]]=float(p[1])
            except: pass
    return m
out={}
for c in cells:
    d=R+c; a,b=met(d+'/prof-metrics-before.txt'),met(d+'/prof-metrics-after.txt')
    dl=lambda k:b.get(k,0)-a.get(k,0)
    fills=dl('torus_orders_matched_total'); req=dl('torus_rpc_submit_admit_seconds_count'); refused=dl('torus_rpc_submit_admit_rejects_total{reason="backlog_preverify"}')
    acts=dl('torus_native_actions_processed_total'); grx=dl('torus_native_gossip_received_actions_total')
    sa=open(d+'/prof-stat-before.txt').read().split(')')[1].split(); sb=open(d+'/prof-stat-after.txt').read().split(')')[1].split()
    ut=(int(sb[11])-int(sa[11]))/100
    tot=0; cnt=Counter()
    for l in open(d+'/perf.folded'):
        s,n=l.rsplit(' ',1); n=int(n); fr=s.split(';'); tot+=n
        if not fr[0].startswith(pre): continue
        for name,keys in RULES:
            if any(any(k in x for k in keys) for x in fr[1:]): cnt[name]+=n; break
        else: cnt['other']+=n
        cnt['TOTAL']+=n
    cpu_s={k:v/tot*ut for k,v in cnt.items()}
    out[c]=dict(fills=fills,req=req,refused=refused,admitted=req-refused,acts=acts,grx=grx,cpu=cpu_s)
print('counts:',{c[-12:]:{k:v for k,v in o.items() if k!='cpu'} for c,o in out.items()})
names=['TOTAL']+[r[0] for r in RULES]+['other']
print('class | crab r1 ms/1k | crab r2 | main | delta(mean-main) | crab us/req | main us/req | crab us/admitted | main us/admitted | crab us/gossip-rx-act | main')
for k in names:
    v=[out[c]['cpu'].get(k,0) for c in cells]
    f=[v[i]/out[c]['fills']*1e6 for i,c in enumerate(cells)]
    q=[v[i]/out[c]['req']*1e6 for i,c in enumerate(cells)]
    ad=[v[i]/out[c]['admitted']*1e6 for i,c in enumerate(cells)]
    gx=[v[i]/out[c]['grx']*1e6 for i,c in enumerate(cells)]
    print(k,f"{f[0]:.2f}",f"{f[1]:.2f}",f"{f[2]:.2f}",f"{(f[0]+f[1])/2-f[2]:+.2f}",f"{(q[0]+q[1])/2:.0f}",f"{q[2]:.0f}",f"{(ad[0]+ad[1])/2:.0f}",f"{ad[2]:.0f}",f"{(gx[0]+gx[1])/2:.0f}",f"{gx[2]:.0f}",sep=' | ')
