import sys, re, collections
sys.path.insert(0, sys.argv[1]); from stacks import samples
path=sys.argv[2]; ev=None if sys.argv[3]=='-' else sys.argv[3]; pats=sys.argv[4:]
S=list(samples(path,ev)); n=len(S)
def short(s): return re.sub(r'<.*', '<..>', s)[:90] if not s.startswith('<') else s[:110]
for p in pats:
    r=re.compile(p); hit=0; callers=collections.Counter(); kids=collections.Counter()
    for tid,t,fr in S:
        idx=[i for i,s in enumerate(fr) if r.search(s)]
        if not idx: continue
        hit+=1; i=idx[-1]
        callers[' <- '.join(short(x) for x in fr[i+1:i+4])]+=1
        kids[short(fr[i-1]) if i>0 else '(self)']+=1
    print(f'=== {p}: {hit} {100*hit/n:.2f}% of {n}')
    for s,c in callers.most_common(5): print(f'   up  {100*c/n:5.2f} {s}')
    for s,c in kids.most_common(8): print(f'   dn  {100*c/n:5.2f} {s}')
