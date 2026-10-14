import gzip, sys, re, collections
path=sys.argv[1]; exec_tid=int(sys.argv[2]); nblk=int(sys.argv[3])
B=1/499
def it():
    cur=None; fr=[]
    for line in gzip.open(path,'rt',errors='replace'):
        if not line.strip():
            if cur: yield cur, fr
            cur=None; fr=[]; continue
        if line[0] not in ' \t':
            m=re.match(r'^(.*?)\s+(\d+)\s+([0-9.]+):',line.rstrip()); cur=(m.group(1),int(m.group(2)),float(m.group(3)))
        else:
            s=line.strip().split(' ',1); fr.append(s[1] if len(s)>1 else '?')
    if cur: yield cur, fr
R=re.compile(r'PositionManager.*get_position|get_native_balance|StateBackend>::get_cf_raw')
ex=set(); st=collections.Counter(); sr=collections.Counter(); mt=collections.Counter(); t0=None; t1=0
for (comm,tid,t),fr in it():
    t0=t if t0 is None else min(t0,t); t1=max(t1,t); b=int(t/B)
    if tid==exec_tid: ex.add(b); continue
    if comm!='torus-exec-pool': continue
    s=' '.join(fr)
    if 'compute_market_settle_plan' in s:
        st[b]+=1
        if R.search(s): sr[b]+=1
    elif 'market_workers' in s or 'match' in s.lower(): mt[b]+=1
span=t1-t0
wait_bins=[b for b in st if b not in ex]
settle_wall=len(wait_bins)*B; rr_wall=sum(sr[b]/st[b] for b in wait_bins)*B
allsettle=len(st)*B
print(f'span {span:.1f}s blocks {nblk}; exec on-CPU bins {len(ex)*B:.1f}s ({100*len(ex)*B/span:.0f}%)')
print(f'settle-plan pool bins {allsettle:.2f}s, of which exec idle {settle_wall:.2f}s = {1000*settle_wall/nblk:.1f} ms/native block')
print(f'R-read wall-equivalent in those bins {rr_wall:.2f}s = {1000*rr_wall/nblk:.2f} ms/native block')
