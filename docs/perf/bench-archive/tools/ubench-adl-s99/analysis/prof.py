import re,sys,collections
gap=3.0; want=[int(x) for x in sys.argv[1].split(',')]
samples=[];cur=None
def name(raw):
    s=re.sub(r'\s*\(inlined\)$','',raw.strip()); s=re.sub(r'::h[0-9a-f]{16}$','',s)
    for _ in range(4): s=re.sub(r'<[^<>]*>','',s)
    return s
for line in sys.stdin:
    m=re.match(r'\s*(\d+)\s+([\d.]+):',line)
    if m:
        if cur: samples.append(cur)
        cur=(float(m.group(2))*1e3,[]); continue
    if cur is not None and line.strip():
        f=line.strip().split(None,1); cur[1].append(name(f[1]) if len(f)>1 else '?')
if cur: samples.append(cur)
liq=[s for s in samples if any('run_liquidations_with' in f for f in s[1])]
bursts=[]
for s in liq:
    if bursts and s[0]-bursts[-1][-1][0]<=gap: bursts[-1].append(s)
    else: bursts.append([s])
NOISE=set('with_default {closure#0} {closure#1} {closure#4} non_null drop_glue size_of_val_raw as_mut_ptr ptr __rust_begin_short_backtrace catch_unwind for_value_raw ubench_adl_p2 run_test_in_process do_call drop thread_start call_once done assert_test_result [unknown] run_liquidations_with get call get_or_init branch find deallocate current_memory result::Result> cmp compare'.split())
proj=None
roots=['adl_to_escrow','adl_drain','classify','liq_view']
for i in want:
    b=bursts[i-1]; n=len(b); inc=collections.Counter(); self_=collections.Counter(); child=collections.Counter(); under=collections.Counter()
    for t,fr in b:
        short=[f.split('::')[-1] if f.split('::')[-1]!='{{closure}}' else '::'.join(f.split('::')[-2:]) for f in fr]
        pf=[(f,s) for f,s in zip(fr,short) if s not in NOISE and not s.startswith('get_or_init::') and not s.startswith('{closure')]
        for s in set(s for f,s in pf): inc[s]+=1
        if pf: self_[pf[0][1]]+=1
        # callee of liquidation_pass
        ss=[s for f,s in pf]
        if 'liquidation_pass' in ss:
            j=ss.index('liquidation_pass'); child[ss[j-1] if j>0 else '(self)']+=1
        for x in ('pos_sums','get_position','traders_after','has_key','adl_rank','holders_with','sort'):
            if x in ss or any(x==s for s in short):
                k=ss.index(x) if x in ss else None
                anc=[a for a in ss[(k or 0):] if a in ('adl_to_escrow','adl_candidates_of','adl_close','adl_cross','adl_drain','liq_view','classify_trader','adl_rank')]
                under[(x,anc[0] if anc else '?')]+=1
    print(f'\n== burst {i}: {n} samples (~{n/4.999:.0f} ms)')
    print(' callees of liquidation_pass:', ', '.join(f'{k} {100*v/n:.1f}%' for k,v in child.most_common(8)))
    print(' inclusive project frames:')
    for f,k in inc.most_common(40): print(f'  {100*k/n:5.1f}% {f}')
    print(' leaf (self) project frames:', ', '.join(f'{k} {100*v/n:.1f}%' for k,v in self_.most_common(10)))
    print(' attribution (frame, nearest named caller):', ', '.join(f'{a}<-{c} {100*v/n:.1f}%' for (a,c),v in sorted(under.items(), key=lambda x:-x[1])[:14]))
