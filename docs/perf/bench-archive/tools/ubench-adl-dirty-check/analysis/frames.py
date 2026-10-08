import re,sys,collections
# stdin: perf script -F tid,time,ip,sym ; argv[1]: bursts (k = block h=k); argv[2]: comma list of frame names (last path segment, inclusive share)
want=[int(x) for x in sys.argv[1].split(',')]; names=sys.argv[2].split(',')
samples=[];cur=None
def last(raw):
    s=re.sub(r'\s*\(inlined\)$','',raw.strip()); s=re.sub(r'::h[0-9a-f]{16}$','',s)
    for _ in range(4): s=re.sub(r'<[^<>]*>','',s)
    return s.split('::')[-1]
for line in sys.stdin:
    m=re.match(r'\s*(\d+)\s+([\d.]+):',line)
    if m:
        if cur: samples.append(cur)
        cur=(float(m.group(2))*1e3,set()); continue
    if cur is not None and line.strip():
        f=line.strip().split(None,1); cur[1].add(last(f[1]) if len(f)>1 else '?')
if cur: samples.append(cur)
liq=[s for s in samples if 'run_liquidations_with' in s[1]]
bursts=[]
for s in liq:
    if bursts and s[0]-bursts[-1][-1][0]<=3.0: bursts[-1].append(s)
    else: bursts.append([s])
print('frame'.ljust(34)+''.join(f'h={i} ({len(bursts[i-1])} smp)'.rjust(20) for i in want))
for nm in names:
    print(nm.ljust(34)+''.join(f'{100*sum(nm in fr for _,fr in bursts[i-1])/len(bursts[i-1]):19.1f}%' for i in want))
