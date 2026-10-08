#!/usr/bin/env python3
"""split.py <cell_dir> <elf> <exec-inl-script.gz>: item 6 Phase 2 step 0.3, the cancel-all split (707f132f line numbers).

Inline-expanded exec-thread samples (ozarchy-margin-c7-tools/inl.py, llvm-addr2line -i) under the cancel-all entry
functions (exec_cancel_all_run, exec_cancel_all, cancel_orders_and_stops), split by WHAT the time is spent on:
  scan  = cost per (book x run member) or per (action x market), paid whether or not the sender has anything there
          (the part a trader -> markets index removes for books without the sender);
  work  = cost per cancelled order / per stop taken / per hit (kept by P2-1).
Lines: native_executor.rs exec_cancel_all_run 9083-9160, cancel_orders_and_stops 9008-9046; cancel_batch.rs
plan_cancel_all_many 321-410 (338 = trader_orders.get probe, 353-361 = order_index/order_seq get + partition_point),
apply_cancel_all_many; order_book.rs take_pending_stops 1904-1918, cancel_all 1921-.
Prints ms per native block (load window K of buckets2.py, as inlfn.py) per category and the raw top (frame@line -> leaf)."""

import json
import os
import re
import sys
from collections import Counter

sys.path.insert(0, "/home/oz/bench-results-matched/ozarchy-margin-c7-tools")
import inl  # noqa: E402


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


def name_is(f, n):
    return re.search(r"(^|::|>::)" + re.escape(n) + r"(\{cl\})?$", f) is not None


def line_of(loc, fname):
    m = re.search(re.escape(fname) + r":(\d+)$", loc)
    return int(m.group(1)) if m else None


ENTRY = ("exec_cancel_all_run", "exec_cancel_all", "cancel_orders_and_stops")


def classify(st):
    """(category, kind) for a logical stack root -> leaf, or None if not under a cancel-all entry."""
    idx = [i for i, (f, _) in enumerate(st) if any(name_is(f, e) for e in ENTRY)]
    if not idx:
        return None
    i = idx[0]
    entry = st[i][0]
    sub = st[i:]
    names = [f for f, _ in sub]

    def has(n):
        return any(name_is(f, n) for f in names)

    def frame(n):
        for f, loc in sub:
            if name_is(f, n):
                return f, loc
        return None

    if has("take_pending_stops"):
        f, loc = frame("take_pending_stops")
        # the `any` scan of the book's stops (1905) is paid per (book x member); the partition per hit
        ln = line_of(loc, "order_book.rs")
        if ln is not None and ln <= 1906:
            return "take_pending_stops: any() over the book's stops", "scan"
        return "take_pending_stops: take (hit)", "work"
    if has("plan_cancel_all_many"):
        f, loc = frame("plan_cancel_all_many")
        ln = line_of(loc, "cancel_batch.rs")
        if ln is None:
            return "plan_cancel_all_many: other", "scan"
        if ln == 338:
            return "plan: trader_orders.get probe (338)", "scan"
        if 322 <= ln <= 336:
            return "plan: first-occurrence sort + allocs (323-336)", "scan"
        if 337 <= ln <= 346:
            return "plan: probe loop bookkeeping (337-346)", "scan"
        if 352 <= ln <= 361:
            return (
                "plan: locate targets, order_index/order_seq get + partition_point (352-361)",
                "work",
            )
        if 348 <= ln <= 372:
            return "plan: per-target push (348-372)", "work"
        return "plan: sort + levels (373-)", "work"
    if has("apply_cancel_all_many"):
        return "apply_cancel_all_many: removal (work)", "work"
    if has("cancel_all") and any(
        name_is(f, "cancel_all") and "order_book" in loc for f, loc in sub
    ):
        f, loc = next(
            (f, loc)
            for f, loc in sub
            if name_is(f, "cancel_all") and "order_book" in loc
        )
        ln = line_of(loc, "order_book.rs")
        if ln is not None and ln <= 1924:
            return (
                "sequential cancel_all: pending_stops.retain / forget_reduce_only (per book x member)",
                "scan",
            )
        if ln is not None and ln <= 1928:
            return "sequential cancel_all: trader_orders.remove probe", "scan"
        return "sequential cancel_all: removal (work)", "work"
    if has("cancel_all_many"):
        f, loc = frame("cancel_all_many")
        return f"cancel_all_many other @ {loc}", "scan"
    if has("cancelled_orders_margin"):
        return "margin of cancelled orders", "work"
    if has("stop_reservation"):
        return "stop reservation", "work"
    if has("release_order_margin"):
        return "release_order_margin (balance)", "work"
    ln = line_of(st[idx[-1]][1], "native_executor.rs")
    if ln is None:
        return f"{entry} other ({sub[-1][0][:60]})", "scan"
    if name_is(st[idx[-1]][0], "exec_cancel_all_run"):
        if ln == 9100:
            return "run: market_ids collect (9100)", "scan"
        if 9101 <= ln <= 9107:
            return "run: run-level allocs (9101-9107)", "scan"
        if 9108 <= ln <= 9116:
            return "run: members filter per book (9108-9116)", "scan"
        if 9117 <= ln <= 9118:
            return "run: per-book per_action / stops_k allocs (9117-9118)", "scan"
        if 9119 <= ln <= 9122:
            return (
                "run: order_books.get_mut / margin_configs.get per book (9119-9122)",
                "scan",
            )
        if 9123 <= ln <= 9130:
            return "run: stops loop (9125-9130)", "scan"
        if 9131 <= ln <= 9137:
            return "run: per_action store / push per book (9131-9137)", "scan"
        if 9141 <= ln <= 9149:
            return "run: results loop over all markets per action (9141-9149)", "scan"
        if 9150 <= ln <= 9157:
            return "run: results hit bookkeeping (dirty mark, sums) (9150-9157)", "work"
        return f"run: other line {ln} (drops at 9159-9160 = per-book vectors)", "scan"
    if 9008 <= ln <= 9046:
        if ln <= 9024:
            return "single: market_ids collect + sort", "scan"
        if ln <= 9030:
            return "single: per-book get_mut / counters", "scan"
        return f"single: line {ln}", "work"
    return f"{entry} line {ln}", "scan"


def main():
    cell, elf, script = sys.argv[1:4]
    a, b = met(cell + "/prof-metrics-before.txt"), met(cell + "/prof-metrics-after.txt")
    fills = b["torus_orders_matched_total"] - a["torus_orders_matched_total"]
    nblk = b["torus_exec_native_blocks_total"] - a["torus_exec_native_blocks_total"]
    ca = {
        k: b.get(k, 0) - a.get(k, 0)
        for k in (
            "torus_exec_cancel_all_total",
            "torus_exec_cancel_all_books_visited_total",
            "torus_exec_cancel_all_books_hit_total",
        )
    }
    K, _b2, samples = inl.load(cell, elf, script)
    blk = lambda v: K(v) * fills / 1000 / nblk  # noqa: E731
    cat, kind, raw = Counter(), Counter(), Counter()
    total = 0
    for per, st in samples:
        c = classify(st)
        if c is None:
            continue
        total += per
        cat[c] += per
        kind[c[1]] += per
        idx = [i for i, (f, _) in enumerate(st) if any(name_is(f, e) for e in ENTRY)]
        deep = st[idx[-1] :]
        # deepest frame with a torus source line, then the leaf
        tor = [x for x in deep if x[1].startswith("crates/")]
        raw[f"{tor[-1][0][:50]} @ {tor[-1][1]} -> {deep[-1][0][:60]}"] += per
    print(
        f"== {os.path.basename(cell)} cancel-all split ({fills:.0f} fills, {nblk:.0f} native blocks in the perf window; "
        f"{fills / nblk:.0f} fills per native block)"
    )
    print(
        f"  window counters: cancel_alls {ca['torus_exec_cancel_all_total']:.0f} "
        f"({ca['torus_exec_cancel_all_total'] / nblk:.1f} per native block), books visited "
        f"{ca['torus_exec_cancel_all_books_visited_total'] / max(1, ca['torus_exec_cancel_all_total']):.1f} / hit "
        f"{ca['torus_exec_cancel_all_books_hit_total'] / max(1, ca['torus_exec_cancel_all_total']):.2f} per cancel-all"
    )
    print(
        f"  cancel-all inclusive: {blk(total):.2f} ms per native block ({K(total):.3f} ms/1k)"
    )
    for k in ("scan", "work"):
        print(f"    {k:5s} {blk(kind[k]):7.2f} ms/blk")
    print("  categories (ms per native block, kind):")
    for (c, k), v in sorted(cat.items(), key=lambda x: -x[1]):
        print(f"    {blk(v):7.2f}  {k:4s}  {c}")
    print("  raw top 30 (deepest torus frame @ line -> leaf), ms/blk:")
    for r, v in raw.most_common(30):
        print(f"    {blk(v):7.3f}  {r}")
    out = {
        "cell": cell,
        "fills": fills,
        "native_blocks": nblk,
        "counters": ca,
        "total_ms_per_blk": blk(total),
        "scan_ms_per_blk": blk(kind["scan"]),
        "work_ms_per_blk": blk(kind["work"]),
        "categories": {f"{k}|{c}": blk(v) for (c, k), v in cat.items()},
    }
    json.dump(out, open(cell + "/p2s0b-split.json", "w"), indent=1)


main()
