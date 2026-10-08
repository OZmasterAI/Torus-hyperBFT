import re,sys,collections
# stdin: perf script -F tid,time,sym ; argv: gap_ms
gap=float(sys.argv[1]); want=[int(x) for x in sys.argv[2].split(',')] if len(sys.argv)>2 else []
samples=[];cur=None
def short(s):
    s=re.sub(r'::h[0-9a-f]{16}$','',s.strip())
    s=re.sub(r'<[^<>]*>','',s); s=re.sub(r'<[^<>]*>','',s); s=re.sub(r'<[^<>]*>','',s)
    p=[x for x in s.split('::') if x and x!='{{closure}}']
    return '::'.join(p[-2:]) if p else s
for line in sys.stdin:
    m=re.match(r'\s*(\d+)\s+([\d.]+):',line)
    if m:
        if cur: samples.append(cur)
        cur=(float(m.group(2))*1e3,[]) ; continue
    if cur is not None and line.strip():
        f=line.strip().split(None,1)
        cur[1].append(short(f[1]) if len(f)>1 else '?')
if cur: samples.append(cur)
liq=[s for s in samples if any('run_liquidations_with' in f for f in s[1])]
bursts=[];
for s in liq:
    if bursts and s[0]-bursts[-1][-1][0]<=gap: bursts[-1].append(s)
    else: bursts.append([s])
for i,b in enumerate(bursts):
    print(f'burst {i+1}: {len(b)} samples, span {b[-1][0]-b[0][0]:.1f} ms')
for i in want:
    b=bursts[i-1]; n=len(b); c=collections.Counter()
    for t,fr in b:
        for f in set(fr): c[f]+=1
    print(f'\n== burst {i}: {n} samples (~{n/4.999:.0f} ms) inclusive top')
    for f,k in c.most_common(45): print(f'{100*k/n:5.1f}% {f}')
