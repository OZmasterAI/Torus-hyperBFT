#!/usr/bin/env python3
"""calib.py <exec-script.gz> <val0.log.gz> <main_tid> <approx_offset>
Pins the perf-clock -> log (UTC) offset: the log line 'block done height=N' is the last
statement of execute_committed_block_with, so it lies after block N's last end_resident
sample and before its first end-of-function drop_glue::<NativeStateOverlay> sample.
Prints the feasible interval for the extra offset c (log_t - approx_offset - c = perf_t)."""
import gzip, re, sys, datetime, bisect
path, logp, main, off = sys.argv[1], sys.argv[2], sys.argv[3], float(sys.argv[4])
samp = []; tid = None; st = []
def fl():
    if tid == main: samp.append((t, '\n'.join(st)))
for line in gzip.open(path, 'rt'):
    if not line.strip(): fl(); tid = None; st = []; continue
    if line[0] not in ' \t':
        p = line.split(); i = next(k for k, x in enumerate(p) if x.endswith(':')); tid = p[i - 1]; t = float(p[i][:-1]); continue
    st.append(line.strip())
fl()
samp.sort()
A = re.compile(r'\x1b\[[0-9;]*m'); done = []
for line in gzip.open(logp, 'rt', errors='replace'):
    if 'block done' in line:
        line = A.sub('', line)
        done.append(datetime.datetime.strptime(line[:26], '%Y-%m-%dT%H:%M:%S.%f').replace(tzinfo=datetime.timezone.utc).timestamp() - off)
lo, hi, n = -1e9, 1e9, 0
er = [i for i, s in enumerate(samp) if 'native_executor::end_resident' in s[1]]
ends = [er[k] for k in range(len(er)) if k + 1 == len(er) or samp[er[k + 1]][0] - samp[er[k]][0] > 0.3]
for e in ends:
    te = samp[e][0]
    fd = next((samp[j][0] for j in range(e + 1, min(e + 200, len(samp))) if 'drop_glue::<torus_state::backend::NativeStateOverlay>' in samp[j][1]), None)
    if fd is None: continue
    i = bisect.bisect_left(done, te)  # first done after te (naive clock may be off; search window)
    cands = [d for d in done if te - 1 < d < te + 1]
    for d in cands:
        l, h = d - fd, d - te
        if l <= h and abs(l - 0.24) < 0.1:
            lo, hi, n = max(lo, l), min(hi, h), n + 1
print(f'blocks {n}: c in [{lo:.5f}, {hi:.5f}] s (width {1e3*(hi-lo):.2f} ms)')
