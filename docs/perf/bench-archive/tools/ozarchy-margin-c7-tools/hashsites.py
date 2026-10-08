#!/usr/bin/env python3
"""hashsites.py <cell> <elf> <exec-script.gz> <crab|main> [phase]
Margin phase (default) HashMap cost by call site: for each sample whose stack has a SipHash / hashbrown /
rehash frame, the nearest frame in crab code (crates/torus-*) above the first such frame; ms CPU per 1k fills.
Also: rehash (table growth) only, by call site."""

import sys, re, collections

sys.path.insert(0, "/home/oz/bench-results-matched/ozarchy-margin-c7-tools")
import inl

cell, elf, script, which = sys.argv[1:5]
phase = sys.argv[5] if len(sys.argv) > 5 else "margin"
K, b2, samples = inl.load(cell, elf, script)
P = lambda v: f"{K(v):7.3f}"
HM = re.compile(
    r"hashbrown|HashMap|HashSet|hash_one|sip::|rustc_entry|reserve_rehash|^make_hash$|DefaultHasher|btree"
)
RH = re.compile(r"reserve_rehash|resize_inner")
site = collections.Counter()
rsite = collections.Counter()
tot = rt = 0
for per, st in samples:
    if inl.phase_of(which, st) != phase:
        continue
    idx = next((i for i, (f, _) in enumerate(st) if HM.search(f) and i > 0), None)
    if idx is None:
        continue
    j = idx - 1
    while j >= 0 and not st[j][1].startswith("crates/torus-"):
        j -= 1
    key = f"{st[j][1]} {st[j][0][:50]}" if j >= 0 else "?"
    kind = "BTree" if "btree" in st[idx][0].lower() else "Hash"
    site[(key, kind)] += per
    tot += per
    if any(RH.search(f) for f, _ in st):
        rsite[key] += per
        rt += per
print(f"== {cell} {phase}: map work {P(tot)} (rehash {P(rt)})")
for (k, kind), v in site.most_common(30):
    print(P(v), kind, k)
print("-- rehash by site")
for k, v in rsite.most_common(10):
    print(P(v), k)
