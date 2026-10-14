import sys, re, collections
sys.path.insert(0, sys.argv[1]); from stacks import samples
path=sys.argv[2]; ev=None if sys.argv[3]=='-' else sys.argv[3]; pats=sys.argv[4:]
S=list(samples(path,ev)); n=len(S)
for p in pats:
    r=re.compile(p); hit=0; callers=collections.Counter(); leaves=collections.Counter()
    for tid,t,fr in S:
        idx=[i for i,s in enumerate(fr) if r.search(s)]
        if idx:
            hit+=1; i=idx[-1]
            callers[fr[i+1][:150] if i+1<len(fr) else '-']+=1
            leaves[fr[idx[0]][:150]]+=1
    print(f'=== {p}: {hit} samples {100*hit/n:.2f}% of {n}')
    for s,c in callers.most_common(6): print(f'   caller {100*c/n:5.2f} {s}')
    if len(leaves)>1:
        for s,c in leaves.most_common(4): print(f'   match  {100*c/n:5.2f} {s}')
