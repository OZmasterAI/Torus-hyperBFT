#!/usr/bin/env python3
"""flushsplit.py <cell> <elf> <inl-script>: exec-thread cache flush (PositionCache/BalanceCache flush_all) split by the
logical frames below flush_all / put_position (inline-expanded), ms per native block in the perf window."""
import sys, re
from collections import Counter
sys.path.insert(0, "/home/oz/bench-results-matched/ozarchy-margin-c7-tools")
sys.path.insert(0, "/home/oz/bench-results-matched/ozarchy-p2s0b-tools")
import inl
def met(f):
    m = {}
    for l in open(f):
        p = l.split()
        if len(p) >= 2 and not l.startswith("#"):
            try: m[p[0]] = float(p[1])
            except ValueError: pass
    return m
cell, elf, script = sys.argv[1:4]
a, b = met(cell + "/prof-metrics-before.txt"), met(cell + "/prof-metrics-after.txt")
fills = b["torus_orders_matched_total"] - a["torus_orders_matched_total"]
nblk = b["torus_exec_native_blocks_total"] - a["torus_exec_native_blocks_total"]
K, _, samples = inl.load(cell, elf, script)
blk = lambda v: K(v) * fills / 1000 / nblk
top, inner, leaf = Counter(), Counter(), Counter()
tot = 0
for per, st in samples:
    i = [k for k, (f, _) in enumerate(st) if re.search(r"(^|::)flush_all$", f)]
    if not i: continue
    i = i[0]; tot += per
    owner = st[i][0]
    nxt = st[i + 1] if i + 1 < len(st) else ("(self)", st[i][1])
    top[f"{owner} -> {nxt[0][:70]} @ {st[i][1]}"] += per
    j = [k for k, (f, _) in enumerate(st) if re.search(r"put_position$|put_cf_raw_owned$|put_cf_raw$|put_balance", f)]
    if j:
        j = j[-1]; n2 = st[j + 1] if j + 1 < len(st) else ("(self)", st[j][1])
        inner[f"{st[j][0][:50]} -> {n2[0][:70]} @ {st[j][1]}"] += per
    leaf[st[-1][0][:80]] += per
print(f"== cache flush (flush_all incl.) {blk(tot):.2f} ms per native block ({nblk:.0f} blocks, {fills/nblk:.0f} fills/blk)")
print("  flush_all -> next frame @ line:")
for k, v in top.most_common(15): print(f"    {blk(v):6.2f}  {k}")
print("  inside put_position / put_cf_raw*:")
for k, v in inner.most_common(20): print(f"    {blk(v):6.2f}  {k}")
print("  leaves:")
for k, v in leaf.most_common(15): print(f"    {blk(v):6.2f}  {k}")
