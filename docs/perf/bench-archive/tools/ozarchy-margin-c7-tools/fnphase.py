#!/usr/bin/env python3
"""fnphase.py <cell> <elf> <exec-script.gz> <crab|main> <name-substr>...
Inclusive ms CPU per 1k fills of each logical (inline-expanded) function whose name contains the substring,
split by ENGINE phase (inl.phase_of). A sample counts once per substring."""

import sys, collections

sys.path.insert(0, "/home/oz/bench-results-matched/ozarchy-margin-c7-tools")
import inl

cell, elf, script, which = sys.argv[1:5]
K, b2, samples = inl.load(cell, elf, script)
for F in sys.argv[5:]:
    byp = collections.Counter()
    for per, st in samples:
        if any(F in f for f, _ in st):
            byp[inl.phase_of(which, st)] += per
    tot = sum(byp.values())
    print(
        f"{K(tot):7.3f} {F}: "
        + ", ".join(f"{k} {K(v):.3f}" for k, v in byp.most_common())
    )
