import re
D='/home/oz/bench-results-matched/ubench-adl-s99'
for c in ['c1','c2','c3','c4']:
    R=[]
    for r in (1,2,3):
        t=open(f'{D}/{c}.r{r}.log').read()
        base=float(re.search(r'scan baseline ([\d.]+)',t).group(1))
        bl=[(int(a),float(b),float(s),int(adl),int(sc),int(u)) for a,b,s,adl,sc,u in re.findall(r'transfers=(\d+) B_ms=([\d.]+) step_ms=([\d.]+) adl=(\d+) scanned=(\d+) rows=\d+ adl_work=(\d+)',t)]
        R.append((bl,base))
    n=len(R[0][0])
    print('==',c,'blocks',n)
    def rng(v): return f'{min(v):.1f}-{max(v):.1f}'
    for i in range(n):
        if n>6 and i not in (0,1,2,4,n-1) : continue
        st=[r[0][i][2] for r in R]; tr,_,_,adl,sc,u=R[0][0][i]
        bu=tr*6; bms=[r[0][i][1] for r in R]
        drain=[r[0][i][2]-r[0][i][1]-r[1]*(sc-adl) for r in R]
        du=u-bu
        s=f' blk{i+1}: step {"/".join(f"{x:.1f}" for x in st)} rig {min(st)*1.9:.0f}-{max(st)*2:.0f} | gross ns/unit {rng([x*1e6/u for x in st])}'
        if bu: s+=f' | B {rng(bms)} ms = {rng([x*1e6/bu for x in bms])} ns/B-unit, {rng([x*1e3/tr for x in bms])} us/transfer'
        if du>0: s+=f' | drain {rng(drain)} ms / {du} drain units = {rng([x*1e6/du for x in drain])} ns/unit'
        print(s)
    if n>6:
        later=[r[0][i][2] for r in R for i in range(1,n-1)]
        print(f' later blocks 2..{n-1}: step {min(later):.1f}-{max(later):.1f} ms, rig {min(later)*1.9:.0f}-{max(later)*2:.0f}; gross ns/unit {min(r[0][i][2]*1e6/r[0][i][5] for r in R for i in range(1,n-1)):.0f}-{max(r[0][i][2]*1e6/r[0][i][5] for r in R for i in range(1,n-1)):.0f}')
    tot=[sum(b[2] for b in r[0]) for r in R]; print(' sum of step ms per event:', '/'.join(f'{x:.0f}' for x in tot))
