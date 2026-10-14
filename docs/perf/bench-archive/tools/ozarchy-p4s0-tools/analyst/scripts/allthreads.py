import gzip, sys, re, collections
path=sys.argv[1]; exec_tid=int(sys.argv[2]); pats=sys.argv[3:]
def it():
    cur=None; fr=[]
    for line in gzip.open(path,'rt',errors='replace'):
        if not line.strip():
            if cur: yield cur, fr
            cur=None; fr=[]; continue
        if line[0] not in ' \t':
            m=re.match(r'^(.*?)\s+(\d+)\s+([0-9.]+):',line.rstrip()); cur=(m.group(1),int(m.group(2)))
        else:
            s=line.strip().split(' ',1); fr.append(s[1] if len(s)>1 else '?')
    if cur: yield cur, fr
groups=collections.Counter(); hits=collections.defaultdict(collections.Counter); roots=collections.defaultdict(collections.Counter)
for (comm,tid),fr in it():
    g='EXEC' if tid==exec_tid else comm
    groups[g]+=1
    if g=='torus-execution':
        # what spawned it: first torus frame from the root side
        tf=[x for x in fr if 'torus' in x and 'thread' not in x and 'spawn' not in x]
        roots[g][(tf[-1] if tf else '?')[:120]]+=1
    for p in pats:
        if any(re.search(p,x) for x in fr): hits[g][p]+=1
print({k:v for k,v in groups.most_common(8)})
for g in ['EXEC','torus-execution','torus-exec-pool','torus-end-resid']:
    print('==',g,groups[g])
    for p in pats: print(f'   {hits[g][p]:6d} {p}')
print('-- roots of non-exec torus-execution threads')
for s,c in roots['torus-execution'].most_common(8): print(f'   {c:6d} {s}')
