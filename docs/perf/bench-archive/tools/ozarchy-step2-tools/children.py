#!/usr/bin/env python3
"""children.py <exec-script.gz>: count samples by the frame directly called from
execute_committed_block_with (or by execution_loop when the sample is outside it)."""
import gzip, sys, re
from collections import Counter
H = re.compile(r'\[[0-9a-f]{6,16}\]')
c = Counter(); n = 0
def flush(fr):
    if not fr: return
    fr = list(reversed(fr))  # outer -> leaf
    for i, f in enumerate(fr):
        if 'execute_committed_block_with' in f:
            c[fr[i + 1] if i + 1 < len(fr) else '<self>'] += 1; return
    for i, f in enumerate(fr):
        if 'execution_loop' in f:
            c['LOOP:' + (fr[i + 1] if i + 1 < len(fr) else '<self>')] += 1; return
    c['OTHER:' + fr[-1]] += 1
fr = []
for line in gzip.open(sys.argv[1], 'rt'):
    if not line.strip():
        flush(fr); fr = []; continue
    if line[0] not in ' \t': n += 1; continue
    p = line.strip().split(' ', 1)
    if len(p) > 1 and not p[1].startswith('[unknown]'): fr.append(H.sub('', p[1]))
flush(fr)
print('samples', n)
for k, v in c.most_common(60): print(v, k[:200])
