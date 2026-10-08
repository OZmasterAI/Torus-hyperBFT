import re,statistics as st
D='/home/oz/bench-results-matched/ubench-adl-dirty-check'
P=r'h=(\d+) transfers=(\d+) B_ms=([\d.]+) step_ms=([\d.]+) adl=(\d+) scanned=(\d+) rows=(\d+) adl_work=(\d+)'
R={}
for arm in 'AB':
    R[arm]=[]
    for r in (1,2,3):
        t=open(f'{D}/c3.{arm}.r{r}.log').read()
        assert 'test result: ok. 1 passed' in t, (arm,r)
        bl=[dict(h=int(m[0]),tr=int(m[1]),b=float(m[2]),s=float(m[3]),adl=int(m[4]),sc=int(m[5]),rows=int(m[6]),u=int(m[7])) for m in re.findall(P,t)]
        R[arm].append((bl,re.search(r'ADL summary.*',t).group(0),re.findall(r'ADL setup.*',t)))
key=lambda bl:[(x['h'],x['u'],x['rows'],x['tr'],x['adl'],x['sc']) for x in bl]
ref=key(R['A'][0][0])
print('setup A:',*R['A'][0][2],sep='\n  '); print('setup B:',*R['B'][0][2],sep='\n  ')
print('blocks with ADL work:',{a:[len(r[0]) for r in R[a]] for a in 'AB'})
print('per-block (h,units,rows,transfers,adl,scanned) identical in all 6 runs:',all(key(r[0])==ref for a in 'AB' for r in R[a]))
print('units total:',sum(x[1] for x in ref))
n=len(ref)
print(f"{'blk':>4} {'h':>3} {'units':>7} | {'A step ms r1/r2/r3':>20} {'med':>6} | {'B step ms r1/r2/r3':>20} {'med':>6} | {'B-A':>6} {'%':>6}")
meds={}
for i in range(n):
    a=[r[0][i]['s'] for r in R['A']]; b=[r[0][i]['s'] for r in R['B']]
    ma,mb=st.median(a),st.median(b); meds[i]=(ma,mb)
    print(f"{i+1:>4} {ref[i][0]:>3} {ref[i][1]:>7} | {'/'.join(f'{x:.1f}' for x in a):>20} {ma:6.1f} | {'/'.join(f'{x:.1f}' for x in b):>20} {mb:6.1f} | {mb-ma:+6.1f} {100*(mb-ma)/ma:+5.1f}%")
for a in 'AB':
    later=[r[0][i]['s'] for r in R[a] for i in range(1,n-1)]
    print(f'{a} later blocks 2..{n-1}: median {st.median(later):.1f} ms, range {min(later):.1f}-{max(later):.1f}; sum of step ms per event:',
          '/'.join(f"{sum(x['s'] for x in r[0]):.0f}" for r in R[a]))
mb=meds[0][1]
print(f'B block B median {mb:.1f} ms ozarchy -> rig x1.9-2 = {mb*1.9:.0f}-{mb*2:.0f} ms vs 250 ms target (={250/2:.0f}-{250/1.9:.0f} ms ozarchy)')
for a in 'AB':
    for r in R[a]: print(a,r[1])
