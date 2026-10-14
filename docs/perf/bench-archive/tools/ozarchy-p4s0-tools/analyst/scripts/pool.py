import gzip, sys, re, collections
path=sys.argv[1]
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
tids=set(); job=collections.Counter(); rr=collections.Counter(); n=0
R=re.compile(r'PositionManager.*get_position|get_native_balance|StateBackend>::get_cf_raw|StateBackend>::iterate_cf|Position as borsh|NativeBalance as borsh')
for (comm,tid,t),fr in it():
    if comm!='torus-exec-pool': continue
    n+=1; tids.add(tid)
    s=' '.join(fr)
    j='settle' if 'settle_market' in s or 'settle' in s.lower() and 'native_executor' in s else ('match' if 'market_workers' in s or 'match' in s.lower() else 'other')
    job[j]+=1
    if R.search(s): rr[j]+=1
print('pool samples',n,'tids',len(tids))
for j,c in job.most_common(): print(f'  {j:7s} {c:6d} R-read {rr[j]:6d} ({100*rr[j]/c:.1f}% of {j})')
