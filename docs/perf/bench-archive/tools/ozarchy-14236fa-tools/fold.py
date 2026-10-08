#!/usr/bin/env python3
"""fold.py: perf script (-F comm,tid,ip,sym --no-inline) on stdin -> folded stacks (outer;...;leaf count) on stdout.
Thread comm kept as the first frame. Crate hashes stripped."""
import re, sys
from collections import Counter
WEIGHT = len(sys.argv) > 1 and sys.argv[1] == '--period'
H = re.compile(r'\[[0-9a-f]{6,16}\]')
c = Counter(); comm = None; frames = []; per = 1
def flush():
    if comm is not None:
        c[(comm,) + tuple(reversed(frames))] += per
for line in sys.stdin:
    if not line.strip():
        flush(); comm = None; frames = []; continue
    if line[0] not in ' \t':
        comm = line.split()[0] if not line.startswith(' ') else '?'
        # comm may contain spaces; perf prints comm then tid; take everything before the tid
        m = re.match(r'^(.*?)\s+(\d+)\s', line)
        comm = m.group(1).strip().replace(' ', '_') if m else line.split()[0]
        per = int(line.split()[-1]) if WEIGHT else 1
        continue
    parts = line.strip().split(' ', 1)
    sym = parts[1] if len(parts) > 1 else '[unknown]'
    sym = H.sub('', sym).replace(';', ',')
    if sym.startswith('[unknown]'): continue
    frames.append(sym)
flush()
for k, v in c.most_common():
    print(';'.join(k), v)
