import gzip, sys, re, collections
def samples(path, event=None):
    cur=None; frames=[]
    with gzip.open(path,'rt',errors='replace') as f:
        for line in f:
            if not line.strip():
                if cur is not None and (event is None or cur[1]==event): yield cur[0], cur[2], frames
                cur=None; frames=[]; continue
            if not line.startswith('\t') and not line.startswith('        '):
                p=line.split()
                ev=None
                for x in p:
                    if x.endswith(':') and not re.match(r'^[0-9.]+:$',x): ev=x.rstrip(':')
                t=float([x for x in p if re.match(r'^[0-9]+\.[0-9]+:?$',x)][0].rstrip(':'))
                cur=(int(p[0]),ev,t); frames=[]
            else:
                s=line.strip().split(' ',1)
                sym=s[1] if len(s)>1 else '?'
                sym=re.sub(r' \(([^)]*)\)$', lambda m: ' @'+m.group(1).split('/')[-1], sym)
                frames.append(sym)
        if cur is not None and (event is None or cur[1]==event): yield cur[0], cur[2], frames
if __name__=='__main__':
    path=sys.argv[1]; ev=sys.argv[2] if len(sys.argv)>2 and sys.argv[2]!='-' else None
    n=0; incl=collections.Counter(); self_=collections.Counter()
    for tid,t,fr in samples(path,ev):
        n+=1
        if fr: self_[fr[0]]+=1
        for s in set(fr): incl[s]+=1
    print('samples',n)
    print('--- inclusive top');
    for s,c in incl.most_common(int(sys.argv[3]) if len(sys.argv)>3 else 70): print(f'{100*c/n:6.2f} {s[:200]}')
    print('--- self top')
    for s,c in self_.most_common(40): print(f'{100*c/n:6.2f} {s[:200]}')
