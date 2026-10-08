#!/usr/bin/env python3
"""glue.py <cell_dir>: within the 'execute_batch_phases glue' bucket of buckets2.py, ms/1k by the first frame below execute_batch_phases."""
import importlib.util, sys, collections, re
spec = importlib.util.spec_from_file_location('b', '/home/oz/bench-results-matched/ozarchy-c3pf1-tools/buckets2.py')
d = sys.argv[1]; sys.argv = ['x', d]
import io, contextlib
with contextlib.redirect_stdout(io.StringIO()):
    b = importlib.util.module_from_spec(spec); spec.loader.exec_module(b)
c = collections.Counter(); tot = 0
for l in open(d + '/perf.folded'):
    s, n = l.rsplit(' ', 1); n = int(n); fr = s.split(';'); tot += n
    if not fr[0].startswith('torus-execution'): continue
    for name, keys in b.PHASE:
        if any(any(k in x for k in keys) for x in fr[1:]): break
    else: continue
    if name != 'execute_batch_phases glue': continue
    i = max(j for j, x in enumerate(fr) if 'execute_batch_phases' in x)
    nxt = fr[i + 1] if i + 1 < len(fr) else '(self)'
    c[re.sub(r'<torus_state::backend::NativeStateOverlay>', '', nxt)[:110]] += n
for k, v in c.most_common(12): print(f'{b.K(v / tot):7.3f} {k}')
