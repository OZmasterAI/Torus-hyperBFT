#!/usr/bin/env python3
"""cats.py <cell> <elf> <exec-script.gz> <crab|main>
Margin phase (ENGINE margin timer, inl.phase_of) cross-cuts, ms CPU per 1k fills:
 a. what the innermost frames are doing: SipHash, other HashMap work, FixedPoint/i128 arithmetic, format!/alloc, rest
 b. crab only: prepare_one split by source-line group, each with its HashMap+SipHash part
 c. AccountReader::get_position / position_px / valuation (pos_net -> cached_sums -> build_with) by phase and caller
"""

import sys, re, collections

sys.path.insert(0, "/home/oz/bench-results-matched/ozarchy-margin-c7-tools")
import inl

cell, elf, script, which = sys.argv[1:5]
K, b2, samples = inl.load(cell, elf, script)
P = lambda v: f"{K(v):7.3f}"

HASHMAP = re.compile(
    r"hashbrown|HashMap|HashSet|rustc_entry|^entry$|^get$|^get_mut$|^insert$|^find$|^find_inner$|reserve_rehash|^or_insert|^or_default"
)
SIP = re.compile(
    r"sip::|hash_one|^make_hash$|^write$|^write_usize$|^write_length_prefix$|^finish$|^hash$|hash_slice|c_rounds|d_rounds|u8to64_le|DefaultHasher"
)
ARITH = re.compile(
    r"FixedPoint>::checked_|FixedPoint as core::ops::arith|__divti3|__udivti3|__muloti4|idivmod|udivmod|umulddi3|^checked_(mul|div|add|sub)$|^mul$|^div$"
)
FMT = re.compile(r"format|fmt::|to_string|Display")
ALLOC = re.compile(
    r"alloc::alloc|malloc|realloc|cfree|^free$|grow_one|finish_grow|drop_glue|RawVec"
)


def cat(st):
    names = [f for f, _ in st]
    # innermost-first classification
    for f in reversed(names):
        if SIP.search(f):
            return "SipHash (std HashMap hashing)"
        if ARITH.search(f):
            return "FixedPoint / i128 arithmetic"
        if FMT.search(f):
            return "format! / String"
        if ALLOC.search(f):
            return "alloc / free / drop"
        if HASHMAP.search(f):
            return "HashMap probe / insert / rehash"
    return "other"


GROUPS = [
    ((5547, 5557), "take_open_slot + open_slots.entry"),
    ((5558, 5563), "validate_order_price"),
    ((5564, 5567), "basis.get"),
    ((5568, 5568), "margin_configs.get"),
    ((5569, 5582), "bid floor (Option B)"),
    ((5583, 5596), "try_reserve_for_qty_cfg"),
    ((5597, 5598), "BalanceCache::load"),
    ((5599, 5614), "pos_nets.get + AccountReader::pos_net (valuation)"),
    ((5615, 5627), "proj.get + AccountReader::position_px + proj.entry"),
    ((5628, 5638), "released / committed gets"),
    ((5639, 5654), "account_check (placement_need)"),
    ((5655, 5664), "excess / committed entry"),
    ((5665, 5678), "BalanceCache::set"),
    ((5679, 5706), "projection (proj.get_mut, release checked_mul, im_delta)"),
    ((5707, 5713), "open_slots.insert + pool.entry"),
    ((5714, 5719), "return"),
]

catm = collections.Counter()
po = collections.Counter()
po_h = collections.Counter()
mtot = 0
for per, st in samples:
    if inl.phase_of(which, st) != "margin":
        continue
    mtot += per
    c = cat(st)
    catm[c] += per
    idx = max(
        (i for i, (f, _) in enumerate(st) if f.endswith(">::prepare_one")), default=None
    )
    if idx is None:
        continue
    m = re.search(r"native_executor\.rs:(\d+)", st[idx][1])
    n = int(m.group(1)) if m else -1
    g = next((name for (a, b), name in GROUPS if a <= n <= b), f"line {n}")
    po[g] += per
    if c.startswith("SipHash") or c.startswith("HashMap"):
        po_h[g] += per

print(f"== {cell} ({which}) margin phase {P(mtot)}")
print("== a. margin phase by innermost activity")
for k, v in catm.most_common():
    print(P(v), k)
if po:
    print("== b. prepare_one by line group (total | of which HashMap + SipHash)")
    for k, v in po.most_common():
        print(P(v), "|", P(po_h[k]), k)
    print(P(sum(po.values())), "|", P(sum(po_h.values())), "prepare_one total")

print("== c. AccountReader valuation / position reads by phase and nearest crab caller")
for F in (
    "<AccountReader>::get_position",
    "<AccountReader>::position_px",
    "<AccountReader>::pos_sums",
    "<margin::AccountView>::build_with",
    "<position::PositionManager>::get_position",
    "trader_positions::find",
    "find",
):
    byp = collections.Counter()
    byc = collections.Counter()
    tot = 0
    for per, st in samples:
        idx = next((i for i, (f, _) in enumerate(st) if f == F or f.endswith(F)), None)
        if idx is None:
            continue
        if F == "find" and not st[idx][1].startswith(
            "crates/torus-bridge/src/trader_positions"
        ):
            continue
        tot += per
        byp[inl.phase_of(which, st)] += per
        # nearest caller frame that is crab code with a line
        j = idx - 1
        while j >= 0 and not (
            "native_executor.rs" in st[j][1]
            or "order_book.rs" in st[j][1]
            or "liquidat" in st[j][1]
        ):
            j -= 1
        byc[f"{st[j][0][:60]} @{st[j][1]}" if j >= 0 else "?"] += per
    if not tot:
        continue
    print(
        f"-- {F}: {P(tot)}  by phase: "
        + ", ".join(f"{k} {K(v):.3f}" for k, v in byp.most_common())
    )
    for k, v in byc.most_common(8):
        print("   ", P(v), k)
