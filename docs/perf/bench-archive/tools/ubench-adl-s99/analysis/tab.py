import re,sys
D='/home/oz/bench-results-matched/ubench-adl-s99'
for c in ['c1','c2','c3','c4']:
    runs=[]
    for r in (1,2,3):
        t=open(f'{D}/{c}.r{r}.log').read()
        bl=[dict(h=int(m[0]),tr=int(m[1]),b=float(m[2]),s=float(m[3]),adl=int(m[4]),sc=int(m[5]),rows=int(m[6]),u=int(m[7])) for m in re.findall(r'h=(\d+) transfers=(\d+) B_ms=([\d.]+) step_ms=([\d.]+) adl=(\d+) scanned=(\d+) rows=(\d+) adl_work=(\d+)',t)]
        runs.append((bl,re.search(r'ADL summary.*',t).group(0), re.findall(r'ADL setup.*',t)))
    print('=====',c); print('\n'.join(runs[0][2]))
    b0=runs[0][0]
    same=all([(x['u'],x['rows'],x['tr']) for x in r[0]]==[(x['u'],x['rows'],x['tr']) for x in b0] for r in runs)
    print('blocks',len(b0),'units identical across runs:',same,'total',sum(x['u'] for x in b0))
    for i,x in enumerate(b0):
        print(f"blk{i+1} h={x['h']} tr={x['tr']} rows={x['rows']} u={x['u']} adl={x['adl']} sc={x['sc']} B_ms={'/'.join(str(r[0][i]['b']) for r in runs)} step={'/'.join(str(r[0][i]['s']) for r in runs)}")
    for r in runs: print(r[1])
