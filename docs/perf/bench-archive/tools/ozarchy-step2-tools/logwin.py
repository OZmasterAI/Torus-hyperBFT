#!/usr/bin/env python3
"""logwin.py <val0.log.gz> <t0_unix> <t1_unix> [--csv out.csv]
Per native block on the exec thread, from val0 log timestamps (us resolution):
  sent(h)  = 'on_committed_block: sending to execution pipeline height=h' (consensus thread)
  start(h) = 'execution pipeline: executing finalized block height=h' (exec thread)
  done(h)  = 'execution pipeline: block done height=h' (exec thread)
Gap(h) = start(h+1) - done(h) for consecutive native blocks h, h+1 (both with has_native=true).
idle(h) = max(0, sent(h+1) - done(h)): the exec thread had nothing queued.
"""
import gzip, re, sys, datetime
ANSI = re.compile(r'\x1b\[[0-9;]*m')
TS = re.compile(r'^(\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d\.\d+)Z')
def ts(s):
    return datetime.datetime.strptime(s, '%Y-%m-%dT%H:%M:%S.%f').replace(tzinfo=datetime.timezone.utc).timestamp()
path, t0, t1 = sys.argv[1], float(sys.argv[2]), float(sys.argv[3])
out = sys.argv[sys.argv.index('--csv') + 1] if '--csv' in sys.argv else None
sent, start, done, native = {}, {}, {}, set()
H = re.compile(r'height=(\d+)')
for line in gzip.open(path, 'rt', errors='replace'):
    if 'execution pipeline' not in line: continue
    line = ANSI.sub('', line)
    m = TS.match(line); h = H.search(line)
    if not m or not h: continue
    t = ts(m.group(1)); h = int(h.group(1))
    if 'sending to execution pipeline' in line: sent[h] = t
    elif 'executing finalized block' in line:
        start[h] = t
        if 'has_native=true' in line: native.add(h)
    elif 'block done' in line: done[h] = t
rows = []
for h in sorted(native):
    n = h + 1
    if n not in native or h not in done or n not in start or n not in sent: continue
    if not (t0 <= done[h] <= t1): continue
    gap = start[n] - done[h]
    idle = max(0.0, sent[n] - done[h])
    rows.append((h, done[h] - start[h], gap, idle, sent[n] - done[h], start[n] - sent[n]))
def pct(v, p):
    v = sorted(v); return v[min(len(v) - 1, int(round(p / 100 * (len(v) - 1))))]
print(f'native pairs in window: {len(rows)}')
for name, i in (('blk start->done ms', 1), ('gap done(h)->start(h+1) ms', 2), ('idle (h+1 not yet sent) ms', 3)):
    v = [r[i] * 1e3 for r in rows]
    print(f'{name:32s} mean {sum(v)/len(v):8.2f} p10 {pct(v,10):8.2f} p50 {pct(v,50):8.2f} p90 {pct(v,90):8.2f} max {max(v):8.2f}')
q = sum(1 for r in rows if r[4] <= 0)
print(f'blocks where h+1 was already queued at done(h): {q}/{len(rows)}')
v = [-r[4] for r in rows]
print(f'queue lead (done(h) - sent(h+1)) s: p10 {pct(v,10):.2f} p50 {pct(v,50):.2f} p90 {pct(v,90):.2f}')
if out:
    with open(out, 'w') as f:
        f.write('height,blk_ms,gap_ms,idle_ms,sent_minus_done_ms\n')
        for r in rows: f.write(f'{r[0]},{r[1]*1e3:.3f},{r[2]*1e3:.3f},{r[3]*1e3:.3f},{r[4]*1e3:.3f}\n')
