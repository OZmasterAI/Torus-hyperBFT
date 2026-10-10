import json,statistics as st
STATE={'cf_native_positions','cf_native_balances','cf_native_order_books','cf_book_order_rows','cf_native_markets','cf_native_liquidation','cf_native_oracle'}
G={'cf_native_trades':'trades','cf_native_user_trades':'trades','cf_native_pending':'DA (native_pending)','cf_block_bodies':'block bodies','cf_consensus_meta':'hotstuff/consensus','cf_commit_manifest':'hotstuff/consensus','cf_block_headers':'block bodies','cf_native_nonces':'nonces (append)'}
def grp(cf): return 'STATE rows' if cf in STATE else G.get(cf,'other')
for win in ['all','load']:
    rows={}
    tot=[]
    for v in range(3):
        d=json.load(open(f'/tmp/claude-1000/p3cf/val{v}-{win}.json'))
        agg={}
        for cf,x in d.items():
            if cf.startswith('_'): continue
            a=agg.setdefault(grp(cf),dict(c_cpu=0,c_out=0,c_in=0,f_out=0,f_wall=0))
            for k in a: a[k]+=x.get(k,0)
        T={k:sum(a[k] for a in agg.values()) for k in ['c_cpu','c_out','c_in','f_out','f_wall']}
        tot.append(T)
        for g,a in agg.items():
            rows.setdefault(g,[]).append((a['c_cpu']/T['c_cpu'],a['c_out']/T['c_out'],a['f_out']/T['f_out'],a['f_wall']/T['f_wall'],a['c_cpu']/1e6,a['c_out']/1e9,a['f_out']/1e9))
    print('==',win,'totals per val: c_cpu s',[round(t['c_cpu']/1e6,2) for t in tot],'c_out GB',[round(t['c_out']/1e9,3) for t in tot],'c_in GB',[round(t['c_in']/1e9,3) for t in tot],'f_out GB',[round(t['f_out']/1e9,3) for t in tot],'f_wall s',[round(t['f_wall']/1e6,2) for t in tot])
    print('group | compCPU share | compWrite share | flushWrite share | flushWall share | compCPU s | compW GB | flushW GB  (mean+-sd over val0-2)')
    for g,l in sorted(rows.items(),key=lambda x:-x[1][0][0]):
        cols=list(zip(*l))
        print(g,' | '.join('%.3f+-%.3f'%(st.mean(c),st.stdev(c)) for c in cols))
