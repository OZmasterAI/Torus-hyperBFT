#!/usr/bin/env python3
"""window.py <exec-script.gz> <perf.folded> <utime_ms> [--dump N] [--csv out.csv]
Exec main thread (the torus-execution tid with the most samples) per-block timeline from
`perf script -F comm,tid,time,period,ip,sym --no-inline --comms torus-execution` (cycles:u, 499 Hz, fp).

Per native block N (an end_resident sample cluster):
  ER(N)   = first..last end_resident sample (wall), and its CPU (period -> ms)
  W1(N)   = last end_resident sample of N -> first begin_resident / verify sample of N+1
            (the window as the code stands: begin_resident(N+1) runs right before verify)
  W2(N)   = last end_resident sample of N -> first engine sample of N+1
            (begin_block_oracle / execute_batch*): the window if begin_resident were moved
            to just before the context is built (verify + replay guard + sort + ctx don't read R)
CPU ms = sum(period) * (process utime ms / process total period in perf.folded).
Idle/kernel/wait = wall - CPU of the main thread in the window (cycles:u: kernel time and
blocked time both show as no samples).
"""
import gzip, re, sys
from collections import Counter, defaultdict
H = re.compile(r'\[[0-9a-f]{6,16}\]')
path, folded, utime_ms = sys.argv[1], sys.argv[2], float(sys.argv[3])
dump = int(sys.argv[sys.argv.index('--dump') + 1]) if '--dump' in sys.argv else 0
csv = sys.argv[sys.argv.index('--csv') + 1] if '--csv' in sys.argv else None
ptot = 0
for line in open(folded):
    ptot += int(line.rsplit(' ', 1)[1])
MS = utime_ms / ptot

RULES = [  # (category, substrings) in priority order; matched against the whole stack
    ('end_resident', ['native_executor::end_resident']),
    ('begin_resident', ['native_executor::begin_resident']),
    ('verify', ['batch_verify']),
    ('sort_actions', ['sort_native_actions']),
    ('engine', ['execute_batch', 'begin_block_oracle', 'run_liquidations', 'drain_core_writer',
                'process_governance', 'distribute_fees', 'process_epoch_boundary']),
    ('save_books', ['save_order_books', 'stash_resident']),
    ('ctx_new', ['>::new_env', '>::new_with_mode', 'load_books']),
    ('own_pending_delta', ['own_pending_delta']),
    ('freeze', ['freeze']),
    ('drop overlay (end of fn)', ['drop_glue::<torus_state::backend::NativeStateOverlay>']),
    ('handoff', ['pipeline_handoff', 'exec_pipeline']),
    ('exec_block other (replay guard, checks, logs)', ['execute_committed_block_with']),
    ('execution_loop (recv / metrics)', ['execution_loop']),
]
def cat(stack):
    s = '\n'.join(stack)
    for c, subs in RULES:
        if any(x in s for x in subs): return c
    if stack and ('Keccak' in stack[0] or 'KECCAK' in stack[0]): return 'keccak (truncated stack)'
    return 'truncated stack (other leaf)'

samples = []  # (tid, time, period, cat, leaf, child)
tid = t = per = None; st = []
def flush():
    if tid is None: return
    child = ''
    rev = list(reversed(st))
    for i, f in enumerate(rev):
        if 'execute_committed_block_with' in f and i + 1 < len(rev): child = rev[i + 1]; break
    samples.append((tid, t, per, cat(st), st[0] if st else '?', child))
for line in gzip.open(path, 'rt'):
    if not line.strip():
        flush(); tid = None; st = []; continue
    if line[0] not in ' \t':
        p = line.split()
        # comm tid time: period
        i = next(k for k, x in enumerate(p) if x.endswith(':'))
        tid = int(p[i - 1]); t = float(p[i][:-1]); per = int(p[i + 1]); continue
    p = line.strip().split(' ', 1)
    if len(p) > 1 and not p[1].startswith('[unknown]'): st.append(H.sub('', p[1]))
flush()
main = Counter(s[0] for s in samples).most_common(1)[0][0]
S = sorted((s for s in samples if s[0] == main), key=lambda s: s[1])
print(f'main tid {main}: {len(S)} samples, {S[-1][1]-S[0][1]:.1f} s, ms/period-unit {MS:.3e}, '
      f'main CPU {sum(s[2] for s in S)*MS/1e3:.1f} s')

START = ('begin_resident', 'verify')
ENGINE = ('engine',)
# clusters of end_resident samples; a new cluster starts after a non-ER categorized marker
blocks = []; cur = None
for i, s in enumerate(S):
    if s[3] == 'end_resident':
        if cur is None: cur = [i, i]
        else: cur[1] = i
    elif s[3] in START + ENGINE + ('save_books',) and cur is not None:
        blocks.append(cur); cur = None
if cur: blocks.append(cur)

def pct(v, p):
    v = sorted(v); return v[min(len(v) - 1, int(round(p / 100 * (len(v) - 1))))] if v else float('nan')
def stat(v): return f'mean {sum(v)/len(v):7.1f}  p10 {pct(v,10):7.1f}  p50 {pct(v,50):7.1f}  p90 {pct(v,90):7.1f}'

rows = []
for k, (a, b) in enumerate(blocks[:-1]):
    nxt = blocks[k + 1][0]
    er_cpu = sum(S[j][2] for j in range(a, b + 1) if S[j][3] == 'end_resident') * MS
    er_all = sum(S[j][2] for j in range(a, b + 1)) * MS
    er_wall = (S[b][1] - S[a][1]) * 1e3 + 2.0  # + one sample interval
    j1 = next((j for j in range(b + 1, nxt) if S[j][3] in START), None)
    j2 = next((j for j in range(b + 1, nxt) if S[j][3] in ENGINE), None)
    if j1 is None or j2 is None: continue
    def win(j):
        cats = Counter()
        for x in range(b + 1, j): cats[S[x][3]] += S[x][2] * MS
        wall = (S[j][1] - S[b][1]) * 1e3
        return wall, cats
    w1, c1 = win(j1); w2, c2 = win(j2)
    rows.append(dict(t=S[b][1], er_cpu=er_cpu, er_all=er_all, er_wall=er_wall, w1=w1, c1=c1, w2=w2, c2=c2,
                     j1=j1, j2=j2, b=b))
print(f'blocks with a full window: {len(rows)} (end_resident clusters {len(blocks)})')
print('end_resident CPU ms/blk      ', stat([r['er_cpu'] for r in rows]))
print('end_resident cluster wall ms ', stat([r['er_wall'] for r in rows]))
for key, ck, lab in (('w1', 'c1', 'W1 end_resident(N) -> begin_resident/verify(N+1)'),
                     ('w2', 'c2', 'W2 end_resident(N) -> engine(N+1)')):
    print(f'== {lab}')
    w = [r[key] for r in rows]; cpu = [sum(r[ck].values()) for r in rows]
    idle = [max(0.0, a - c) for a, c in zip(w, cpu)]
    print('  wall ms                ', stat(w))
    print('  main-thread CPU ms     ', stat(cpu))
    print('  no-sample ms (kernel/blocked/idle)', stat(idle))
    tot = Counter()
    for r in rows: tot.update(r[ck])
    for c, v in tot.most_common(): print(f'    {c:45s} {v/len(rows):7.2f} ms/blk')
    for lab2, src in (('hide(busy CPU only)', cpu), ('hide(wall)', w)):
        h = [min(x, r['er_cpu']) for x, r in zip(src, rows)]
        print(f'  min({lab2}, end_resident CPU) ms', stat(h))
if csv:
    with open(csv, 'w') as f:
        f.write('t,er_cpu_ms,er_wall_ms,w1_wall_ms,w1_cpu_ms,w2_wall_ms,w2_cpu_ms\n')
        for r in rows:
            f.write(f"{r['t']:.4f},{r['er_cpu']:.2f},{r['er_wall']:.2f},{r['w1']:.2f},{sum(r['c1'].values()):.2f},"
                    f"{r['w2']:.2f},{sum(r['c2'].values()):.2f}\n")
if '--log' in sys.argv:
    import gzip as _gz, datetime as _dt
    ANSI = re.compile(r'\x1b\[[0-9;]*m'); TS = re.compile(r'^(\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d\.\d+)Z'); HH = re.compile(r'height=(\d+)')
    off = float(sys.argv[sys.argv.index('--offset') + 1])  # realtime - monotonic (perf clock)
    start, done, native = {}, {}, set()
    for line in _gz.open(sys.argv[sys.argv.index('--log') + 1], 'rt', errors='replace'):
        if 'execution pipeline' not in line: continue
        line = ANSI.sub('', line); m = TS.match(line); h = HH.search(line)
        if not m or not h: continue
        t = _dt.datetime.strptime(m.group(1), '%Y-%m-%dT%H:%M:%S.%f').replace(tzinfo=_dt.timezone.utc).timestamp() - off
        h = int(h.group(1))
        if 'executing finalized block' in line:
            start[h] = t
            if 'has_native=true' in line: native.add(h)
        elif 'block done' in line: done[h] = t
    T = [s[1] for s in S]
    import bisect
    seg = {k: [] for k in 'ABCD'}; segc = {k: Counter() for k in 'ABCD'}; nseg = 0; bad = 0
    combo = []  # per block: (er_cpu, er_wall, Wnow_wall, Wnow_cpu, Wmov_wall, Wmov_cpu, C0)
    for r in rows:
        tE = S[r['b']][1]
        # the native block whose done() is the first after tE
        cand = [h for h in native if h in done and done[h] >= tE and h + 1 in start and h + 1 in native]
        if not cand: continue
        h = min(cand, key=lambda x: done[x])
        if done[h] - tE > 0.2: bad += 1; continue
        tD, tS, tV, tG = done[h], start[h + 1], S[r['j1']][1], S[r['j2']][1]
        if not (tE <= tD <= tS <= tV <= tG): bad += 1; continue
        nseg += 1
        # C0: executing(N+1) -> first main-thread sample after it (preamble + begin_resident + <=1 sample interval)
        i0 = bisect.bisect_right(T, tS); c0 = (T[i0] - tS) * 1e3 if i0 < len(T) else 0.0
        def cpu(a, b):
            return sum(S[x][2] for x in range(bisect.bisect_right(T, a), bisect.bisect_left(T, b))) * MS
        tB = tS + c0 / 1e3
        combo.append((r['er_cpu'], r['er_wall'], (tB - tE) * 1e3, cpu(tE, tB), (tG - tE) * 1e3, cpu(tE, tG), c0))
        for k, (a, b) in zip('ABCD', ((tE, tD), (tD, tS), (tS, tV), (tV, tG))):
            i0, i1 = bisect.bisect_right(T, a), bisect.bisect_left(T, b)
            seg[k].append(((b - a) * 1e3, sum(S[x][2] for x in range(i0, i1)) * MS))
            for x in range(i0, i1): segc[k][S[x][3]] += S[x][2] * MS
    print(f'== log-aligned segments ({nseg} blocks, {bad} rejected as misaligned)')
    names = {'A': 'A last end_resident sample(N) -> log block done(N)',
             'B': 'B block done(N) -> log executing(N+1)',
             'C': 'C executing(N+1) -> first verify sample(N+1) [begin_resident is in here]',
             'D': 'D first verify sample -> first engine sample(N+1) [verify, replay guard, sort, ctx]'}
    for k in 'ABCD':
        w = [x[0] for x in seg[k]]; c = [x[1] for x in seg[k]]
        print(f'  {names[k]}')
        print(f'    wall {stat(w)}');  print(f'    CPU  {stat(c)}')
        for cc, v in segc[k].most_common(8): print(f'      {cc:45s} {v/nseg:7.2f} ms/blk')
    if combo:
        print('== per-block windows (log-aligned)')
        print('  C0 executing(N+1) -> first sample after it ms', stat([c[6] for c in combo]))
        lab = ['end_resident CPU', 'end_resident wall', 'Wnow wall (ER end -> begin_resident, upper bound)', 'Wnow CPU',
               'Wmov wall (ER end -> engine start)', 'Wmov CPU']
        for i, l in enumerate(lab): print(f'  {l:52s}', stat([c[i] for c in combo]))
        for l, i in (('Wnow', 2), ('Wmov', 4)):
            busy = [min(c[i + 1], c[0]) for c in combo]; wall = [min(c[i], c[0]) for c in combo]
            print(f'  hide {l} (a) busy only: min(CPU, ER)          ', stat(busy))
            print(f'  hide {l} (b) wall incl no-sample: min(wall, ER)', stat(wall))
            print(f'  {l}: blocks where wall >= ER: {sum(1 for c in combo if c[i] >= c[0])}/{len(combo)}')
        if csv:
            with open(csv.replace('.csv', '-aligned.csv'), 'w') as f:
                f.write('er_cpu_ms,er_wall_ms,wnow_wall_ms,wnow_cpu_ms,wmov_wall_ms,wmov_cpu_ms,c0_ms\n')
                for c in combo: f.write(','.join(f'{x:.2f}' for x in c) + '\n')
    # whole-run per-block CPU by category (main thread), for context
    allc = Counter()
    for x in S: allc[x[3]] += x[2] * MS
    print(f'== main-thread CPU by category over the whole perf window, ms per end_resident cluster ({len(blocks)})')
    for c, v in allc.most_common(): print(f'    {c:45s} {v/len(blocks):7.2f}')
# P: block N's tail before end_resident: last engine/save_books sample -> first end_resident sample
pre = []; prec = Counter()
for a, b in blocks:
    j = next((x for x in range(a - 1, max(0, a - 400), -1) if S[x][3] in ('engine', 'save_books')), None)
    if j is None: continue
    pre.append(((S[a][1] - S[j][1]) * 1e3, sum(S[x][2] for x in range(j + 1, a)) * MS))
    for x in range(j + 1, a): prec[S[x][3]] += S[x][2] * MS
if pre:
    print(f'== P: last engine/save_books sample(N) -> first end_resident sample(N) ({len(pre)} blocks)')
    print('  wall', stat([p[0] for p in pre])); print('  CPU ', stat([p[1] for p in pre]))
    for c, v in prec.most_common(6): print(f'    {c:45s} {v/len(pre):7.2f} ms/blk')
if dump:
    for r in rows[:dump]:
        print('---- block window at', r['t'])
        for x in range(r['b'] - 2, r['j2'] + 2):
            s = S[x]; print(f'  {s[1]:.4f} {s[2]*MS:6.2f}ms {s[3]:28s} child={s[5][:70]} leaf={s[4][:70]}')
