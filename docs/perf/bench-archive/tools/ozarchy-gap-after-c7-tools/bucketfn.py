#!/usr/bin/env python3
"""bucketfn.py <cell> <bucket> [N] [below-substr]: within a buckets2.py bucket, inclusive/self ms/1k per function.
If below-substr is given, only frames below the deepest frame containing it are counted (inclusive), self = leaf."""
import importlib.util, sys, collections, re, io, contextlib
spec = importlib.util.spec_from_file_location('b', '/home/oz/bench-results-matched/ozarchy-14236fa-tools/buckets2.py')
d, bucket = sys.argv[1], sys.argv[2]; N = int(sys.argv[3]) if len(sys.argv) > 3 else 40
below = sys.argv[4] if len(sys.argv) > 4 else None
sys.argv = ['x', d]
with contextlib.redirect_stdout(io.StringIO()):
    b = importlib.util.module_from_spec(spec); spec.loader.exec_module(b)
inc = collections.Counter(); slf = collections.Counter(); tot = 0; bt = 0
def short(s): return re.sub(r'<torus_state::backend::NativeStateOverlay>', '', re.sub(r'::\{closure#\d+\}', '{cl}', s))[:150]
for l in open(d + '/perf.folded'):
    s, n = l.rsplit(' ', 1); n = int(n); fr = s.split(';'); tot += n
    if not fr[0].startswith('torus-execution'): continue
    for name, keys in b.PHASE:
        if any(any(k in x for k in keys) for x in fr[1:]): break
    else: continue
    if name != bucket: continue
    bt += n
    for x in set(fr[1:]): inc[short(x)] += n
    slf[short(fr[-1])] += n
K = lambda v: b.K(v / tot)
print(f"bucket {bucket}: {K(bt):.3f} ms/1k")
print("--- inclusive"); [print(f"{K(v):7.3f} {k}") for k, v in inc.most_common(N)]
print("--- self"); [print(f"{K(v):7.3f} {k}") for k, v in slf.most_common(N)]
