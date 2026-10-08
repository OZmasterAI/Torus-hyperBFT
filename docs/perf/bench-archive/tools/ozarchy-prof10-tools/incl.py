#!/usr/bin/env python3
"""incl.py <folded> <comm-prefix> [N] [filter-substr]: inclusive and self % (of comm total) per function."""
import sys, re
from collections import Counter
f, cp = sys.argv[1], sys.argv[2]; N = int(sys.argv[3]) if len(sys.argv) > 3 else 60
flt = sys.argv[4] if len(sys.argv) > 4 else None
inc = Counter(); slf = Counter(); tot = 0
def short(s): 
    s = re.sub(r'::\{closure#\d+\}', '{cl}', s)
    return s[:230]
for l in open(f):
    s, n = l.rsplit(' ', 1); n = int(n); fr = s.split(';')
    if not fr[0].startswith(cp): continue
    if flt and not any(flt in x for x in fr): continue
    tot += n
    for x in set(fr[1:]): inc[x] += n
    if len(fr) > 1: slf[fr[-1]] += n
print("total", tot)
print("--- inclusive"); [print(f"{v/tot*100:6.2f}% {short(k)}") for k, v in inc.most_common(N)]
print("--- self"); [print(f"{v/tot*100:6.2f}% {short(k)}") for k, v in slf.most_common(N)]
