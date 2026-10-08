#!/usr/bin/env python3
"""inlfn.py <cell_dir> <elf> <exec-inl-script.gz>: inline-expanded (llvm-addr2line -i, ozarchy-margin-c7-tools/inl.py)
exec-thread costs for the Phase 2 items whose functions are inlined away in the --no-inline perf.folded
(exec_cancel_order / exec_modify_order are inlined into the phase-1 dispatcher; their book scan is a loop over
order_books calling OrderBook::get_order / cancel_order, also inlined).
Script: perf script -F comm,tid,time,period,ip,sym,symoff --no-inline --comms torus-execution (load window only:
inl.py's K needs <cell_dir>/perf.folded + prof snapshots with fills > 0).
Prints, per logical function: inclusive ms per 1k fills and per native block, and what runs directly inside it
(next-inner logical frame), so the scan part of cancel/modify is visible."""

import os
import sys
from collections import Counter

sys.path.insert(0, "/home/oz/bench-results-matched/ozarchy-margin-c7-tools")
import inl  # noqa: E402

FUNCS = [
    ("cancel_order (action)", "exec_cancel_order"),
    ("modify_order (action)", "exec_modify_order"),
    ("cancel_all (action)", "exec_cancel_all"),
    ("cancel_orders_and_stops", "cancel_orders_and_stops"),
    (
        "OrderBook::get_order (any caller)",
        ("OrderBook>::get_order", "OrderBook::get_order"),
    ),
    (
        "OrderBook::cancel_order (any caller)",
        ("OrderBook>::cancel_order", "OrderBook::cancel_order"),
    ),
    ("cache flush: flush_all", "flush_all"),
    ("cache flush: add_cum_volume", "add_cum_volume"),
    ("save_order_books", "save_order_books"),
    ("diff_stop_rows", "diff_stop_rows"),
    ("encode_order_row", "encode_order_row"),
    ("stash_resident", "stash_resident"),
    ("begin_block_oracle", "begin_block_oracle"),
    ("SumsCarry", "SumsCarry"),
    ("into_carry", "into_carry"),
    ("cached_sums", "cached_sums"),
    ("any *_sums function (crab valuation)", "_sums"),
    ("unrealized_pnl", "unrealized_pnl"),
    ("prepare_one", "prepare_one"),
    ("maker_fill_fits", "maker_fill_fits"),
    ("match_parallel", "match_parallel"),
]
SPLIT = [
    "exec_cancel_all",
    "cancel_all_many",
    "exec_cancel_order",
    "exec_modify_order",
    "flush_all",
    "diff_stop_rows",
    "save_order_books",
]


def met(f):
    m = {}
    for line in open(f):
        p = line.split()
        if len(p) >= 2 and not line.startswith("#"):
            try:
                m[p[0]] = float(p[1])
            except ValueError:
                pass
    return m


cell, elf, script = sys.argv[1:4]
a, b = met(cell + "/prof-metrics-before.txt"), met(cell + "/prof-metrics-after.txt")
fills = b["torus_orders_matched_total"] - a["torus_orders_matched_total"]
nblk = b["torus_exec_native_blocks_total"] - a["torus_exec_native_blocks_total"]
K, b2, samples = inl.load(cell, elf, script)
blk = lambda ms1k: ms1k * fills / 1000 / nblk  # noqa: E731
print(
    f"== {os.path.basename(cell)} inline-expanded exec functions ({len(samples)} samples, {fills:.0f} fills, "
    f"{nblk:.0f} native blocks)   ms/1k | ms/native blk"
)
for name, sub in FUNCS:
    subs = (sub,) if isinstance(sub, str) else sub
    tot = sum(per for per, st in samples if any(x in f for f, _ in st for x in subs))
    print(f"  {name:42s} {K(tot):8.3f} | {blk(K(tot)):8.2f}")
for sub in SPLIT:
    inner = Counter()
    for per, st in samples:
        idx = [i for i, (f, _) in enumerate(st) if sub in f]
        if not idx:
            continue
        i = idx[-1]
        nxt = st[i + 1] if i + 1 < len(st) else ("(self)", st[i][1])
        inner[
            f"{nxt[0]} @ {st[i][1]}" if nxt[0] != "(self)" else f"(self) @ {nxt[1]}"
        ] += per
    if inner:
        print(
            f"  -- inside {sub} (next-inner logical frame @ line in {sub}), ms/native blk"
        )
        for k, v in inner.most_common(10):
            print(f"     {blk(K(v)):8.3f} {k[:150]}")
