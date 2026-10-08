#!/usr/bin/env python3
"""margin.py <cell> <elf> <exec-script.gz> <crab|main> [func-substr ...]

ms CPU per 1k fills (buckets2.py method) on the execution thread, inline-expanded (inl.py):
 1. per execute_batch_phases phase (by the body line of execute_batch_phases on the stack = the ENGINE timers)
 2. margin phase by execute_batch_phases body line (regions)
 3. margin phase inclusive / self per logical (inlined) function
 4. for each func-substr: per-source-line attribution inside that function (innermost occurrence on the
    stack; what runs on the line = next-inner logical frame or (self)), all exec samples, with phase split
 5. FixedPoint arithmetic (checked_mul/div/add/sub, Mul/Div/Add/Sub impls, mul_div) by caller function:line
"""

import sys, re, collections

sys.path.insert(0, "/home/oz/bench-results-matched/ozarchy-margin-c7-tools")
import inl

cell, elf, script, which = sys.argv[1:5]
funcs = sys.argv[5:]
K, b2, samples = inl.load(cell, elf, script)
P = lambda v: f"{K(v):7.3f}"

REG = {
    "crab": [
        ((5101, 5148), "setup (bal/pos caches, engine mode)"),
        ((5111, 5117), "open_order_counts"),
        ((5149, 5155), "place_orders collect"),
        ((5156, 5156), "phase2_reservation_basis"),
        ((5158, 5158), "phase2_bid_floors"),
        ((5164, 5173), "BatchSums::new + AccountReader"),
        ((5174, 5235), "parallel prepare / stitch (unused)"),
        ((5236, 5245), "serial loop overhead"),
        ((5246, 5246), "prepare_one"),
        ((5247, 5259), "stitch_outcome"),
        ((5260, 5274), "excess_by_sender"),
        ((5275, 5275), "d2_pool_takers"),
        ((5276, 5287), "same_batch_bid_top_ups"),
        ((5288, 5296), "pools (BalanceCache load)"),
        ((5297, 5302), "timer"),
    ],
    "main": [
        ((3937, 3984), "setup (bal/pos caches, engine mode)"),
        ((3947, 3953), "open_order_counts"),
        ((3985, 3991), "place_orders collect"),
        ((3992, 3992), "phase2_reservation_basis"),
        ((3994, 4077), "parallel prepare / stitch (unused)"),
        ((4078, 4086), "serial loop overhead"),
        ((4087, 4104), "take_open_slot (+ open_slots entry)"),
        ((4105, 4120), "validate_order_price"),
        ((4121, 4145), "basis lookup + try_reserve_for_qty_cfg"),
        ((4146, 4188), "BalanceCache load / check / set"),
        ((4189, 4208), "order id + market_batches push"),
        ((4209, 4216), "timer"),
    ],
}


def region(n):
    best = None
    for (a, b), name in REG[which]:
        if a <= n <= b and (best is None or b - a < best[0]):
            best = (b - a, name)
    return best[1] if best else f"line {n}"


ph = collections.Counter()
reg = collections.Counter()
inc = collections.Counter()
slf = collections.Counter()
margin = []
for per, st in samples:
    p = inl.phase_of(which, st)
    ph[p] += per
    if p != "margin":
        continue
    margin.append((per, st))
    reg[region(inl.ebp_line(st))] += per
    for f in {f for f, _ in st}:
        inc[f] += per
    slf[st[-1][0] if st else "?"] += per

print(f"== {cell} ({which})")
print("== 1. execute_batch_phases phases (ms/1k)")
for k, v in ph.most_common():
    print(P(v), k)
print(P(sum(ph.values())), "exec total")
print("== 2. margin phase by execute_batch_phases region")
for k, v in reg.most_common():
    print(P(v), k)
print("== 3a. margin phase inclusive by logical function (top 60)")
for k, v in inc.most_common(60):
    print(P(v), k)
print("== 3b. margin phase self (leaf logical function, top 40)")
for k, v in slf.most_common(40):
    print(P(v), k)

for F in funcs:
    byl = collections.Counter()
    byphase = collections.Counter()
    tot = 0
    for per, st in samples:
        idx = max((i for i, (f, _) in enumerate(st) if F in f), default=None)
        if idx is None:
            continue
        # innermost occurrence: its loc is the line inside F
        f, loc = st[idx]
        nxt = st[idx + 1][0] if idx + 1 < len(st) else "(self)"
        byl[(loc, nxt)] += per
        tot += per
        byphase[inl.phase_of(which, st)] += per
    print(
        f"== 4. lines inside [{F}] (all exec samples): {P(tot)}  by phase: "
        + ", ".join(f"{k} {K(v):.3f}" for k, v in byphase.most_common())
    )
    byloc = collections.Counter()
    for (loc, _), v in byl.items():
        byloc[loc] += v
    for loc, v in byloc.most_common(25):
        top = sorted(((vv, n) for (l, n), vv in byl.items() if l == loc), reverse=True)[
            :3
        ]
        print(P(v), loc, " | ", "; ".join(f"{n[:70]} {K(vv):.3f}" for vv, n in top))

ARITH = re.compile(
    r"FixedPoint(>)?::(checked_\w+|mul_div\w*|saturating_\w+)|<FixedPoint as core::ops::arith::(Mul|Div|Add|Sub|Neg)|mul_div|__divti3|__udivti3|__muloti4|idivmod|i128_div|u128_div"
)
for scope in ("margin", "all"):
    byc = collections.Counter()
    byf = collections.Counter()
    tot = 0
    for per, st in margin if scope == "margin" else samples:
        idx = next((i for i, (f, _) in enumerate(st) if ARITH.search(f)), None)
        if idx is None:
            continue
        f = st[idx][0]
        caller = st[idx - 1] if idx > 0 else ("?", "?")
        byc[(re.sub(r"^.*::", "", f)[:40], caller[0][:70], caller[1])] += per
        byf[re.sub(r"^.*::", "", f)[:40]] += per
        tot += per
    print(f"== 5. FixedPoint / i128 arithmetic ({scope}): {P(tot)}")
    for k, v in byf.most_common(10):
        print(P(v), k)
    print("-- by caller")
    for k, v in byc.most_common(30):
        print(P(v), " | ".join(k))
