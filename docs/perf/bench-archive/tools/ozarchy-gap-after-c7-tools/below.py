#!/usr/bin/env python3
"""below.py <cell> <bucket> <frame-substr> [depth]: within a bucket, ms/1k of samples with frame on stack, split by the frame path (depth levels) below it."""
import importlib.util, sys, collections, re, io, contextlib
spec = importlib.util.spec_from_file_location('b', '/home/oz/bench-results-matched/ozarchy-14236fa-tools/buckets2.py')
d, bucket, sub = sys.argv[1:4]; depth = int(sys.argv[4]) if len(sys.argv) > 4 else 1
sys.argv = ['x', d]
with contextlib.redirect_stdout(io.StringIO()):
    b = importlib.util.module_from_spec(spec); spec.loader.exec_module(b)
def sh(s): return re.sub(r'<torus_state::backend::NativeStateOverlay>|torus_state::backend::|alloy_primitives::bits::address::', '', re.sub(r'::\{closure#\d+\}', '{cl}', s))[:90]
c = collections.Counter(); tot = 0; t = 0
for l in open(d + '/perf.folded'):
    s, n = l.rsplit(' ', 1); n = int(n); fr = s.split(';'); tot += n
    if not fr[0].startswith('torus-execution'): continue
    for name, keys in b.PHASE:
        if any(any(k in x for k in keys) for x in fr[1:]): break
    else: continue
    if bucket != '*' and name != bucket: continue
    idx = [j for j, x in enumerate(fr) if sub in x]
    if not idx: continue
    i = idx[0]; t += n
    c[' > '.join(sh(x) for x in fr[i + 1:i + 1 + depth]) or '(self)'] += n
print(f"{b.K(t / tot):.3f} total with {sub}")
for k, v in c.most_common(25): print(f"{b.K(v / tot):7.3f} {k}")
