# r1 lens: robustness (adversarial and pathological load)

Repo main `eb993e3f`, read-only. Numbers come from `map/measured.md` (std CL21: exec block 38.35 ms, 265k matched/s,
21.96 blk/s, block p99 475 ms; E-bound, so matched/s scales roughly as 38.35 / (38.35 + extra ms per block)).
Costs marked **est.** are my estimates from the code and are not measured. Each idea was checked against
`ledger/*.md` with the greps listed under "nearest prior", and against memory.

**Common attacker budget today:**
- Item A wants 1 TRS per funded key at ingress. The TRS can be recycled by transfers.
- Item B gives each address 10k requests. A batch of n orders counts as n of them. Every cancel and every other
  non-place action counts as 1, against a cancel allowance of min(10k + 100k, 2 x 10k) = 20k
  (`addr_rate.rs:16-21,146-152`).
- Cancels get up to 25% of a block (100 of the 400 slots) ahead of orders (`native_pool.rs:873-880`).
- There are no fees, and failed or rejected work is free.

**The shared pattern.** Real matching consumes what it skips: STP makers, reduce-only cuts and margin-cancelled makers
are removed (`order_book.rs:2819+`), so its cost is paid for by placements. Three engine paths re-scan state without
consuming it, so one cheap action can repeat the same scan many times:
- the FOK pre-check (idea 2);
- the reduce-only sweep (idea 1);
- the linear pending-stop scans (ideas 3 and 4).

A single action in one market runs serially inside one pool job, and E waits for that job.

---

## 1. ro-sweep-aggregate: the reduce-only sweep re-scans a trader's kept RO orders on every placement

**Mechanism.** A placement in the batch path builds a `touched` set (the placing trader plus every fill's maker and
taker) and runs `sweep_reduce_only` for each trader in it (`order_book.rs:1836-1850`). This happens whenever
`reduce_only_positions` is non-empty, which is almost always: it holds every batch sender with a position.

The sweep (`:1967-2014`) works like this:
- it collects all of the trader's reduce-only order ids from `reduce_only_index` into a new Vec;
- it calls `get_order` on each one (index lookup, level lookup, binary search on seq);
- every order that fits the position is skipped with `continue`, so nothing is consumed.

A trader with R kept RO orders therefore pays O(R) on every one of its own placements, even IOCs that do not fill.

**Fix.** Keep per (trader, book) aggregates, updated O(1) whenever an RO order is inserted, filled, cancelled or
modified:
- the count of RO orders on the wrong side;
- the summed remaining qty on the reducing side.

When the wrong-side count is 0 and the sum is ≤ |position|, return `Some(|pos| − sum)` without sweeping. That is
exactly what the full sweep returns in that case.

**Target:** block p99, matched/s.

**Expected gain.**
- Attack:
  - Account A holds a long position of about 2k lots (dust margin) and rests R = 1,000 dust RO sells far above mid.
    They fit the position, so they are kept.
  - Each block, A sends one 1,024-order batch of reduce-only IOC sells priced above best bid. These need no fill and
    almost no margin.
  - 1,024 x 1,000 `get_order` at **est.** 0.25-0.35 us ≈ 0.26-0.36 s serial in one market job, whatever the honest
    depth.
- Effect: exec block 38 → ~0.35 s, matched/s about -89%, block p99 ≥ 0.35 s. If volume has raised A's open-order limit
  to 5k, the cost is about 1.5 s per action.
- Attacker cost: about 1 TRS, dust margin and 1k requests per account. B allows about 8 such actions per address.
- After the fix: O(1) per placement.
- Bench: the per-placement sweep overhead is part of the "RO sweep 2.7% of pool CPU" (mem cc05dbcb), and the fix removes
  most of it.

**Nearest prior.** Grepped "reduce.only|sweep|RO sweep" and found nothing.
- exec.md corner 11 / mem cc05dbcb: skip the sweep when `reduce_only_index` is empty. That is a common-case perf skip,
  not proposed. It does nothing when the index is full, which is the attack case.
- modifyorder-price-change-matches is a different path.

**Consensus-visible:** no. Results are identical. The aggregates are derived state and are rebuilt at book load.

**Risks:**
- Every RO mutation site has to keep the aggregates in sync. Debug-assert them against a full recount.
- Stale index ids are cleaned up today only by the sweep (`:1984-1986`), so index removal has to stay exact.

**Effort:** 1.5-2.5 person-days.

**Prototype (≤3 h):**
- Add a microbench to `crates/torus-core/benches/order_book.rs`:
  - one book;
  - trader A with position 2k and R ∈ {100, 1k, 5k} kept RO asks;
  - 1,024 RO IOC sells from A with `reduce_only_positions` installed.
- Record ns per placement today, and with the aggregate stub.
- Add an env knob to `ubench_econ` that injects this action into market 1 once per block, and record engine ms per
  block.
- Refuted if today's cost is under 20 ms per action at R = 1k.

---

## 2. fok-precheck-bounded: the FOK pre-check re-walks depth it never consumes

**Mechanism.** Every FOK that has a maker source clones the market's whole `AccountMargins` and `ReduceOnlyPositions`
(`order_book.rs:1753-1754`). The batch path always has one: `makers` = the AccountReader. Then `can_fill_completely`
(`:3224-3330`) walks every opposite order up to the limit and runs `account_fill` / `mark_loss` for each one. If the
order cannot fill, it rejects with no state change (`:1797`), so N FOKs re-walk the same depth.

Fix:
- (a) **Exact early reject.** Keep per-side level totals (O(1) to update). The total up to the limit, minus the taker's
  own resting qty, is an upper bound on what the FOK can fill. If that bound is below the FOK's qty, return FokCancel
  without walking. The result is identical to today's.
- (b) **No clones.** Use a copy-on-write scratch over the two maps instead of cloning them.
- (c) **Consensus budget.** The FOK pre-checks of one action may visit at most W = 16,384 orders. The action's later
  FOKs reject.

**Target:** block p99, matched/s, deep-book stability.

**Expected gain.**
- Deep shape: about 1.9-2.2k orders per side (1.11-1.30M resting over 300 books). A 1,024-FOK action with qty above the
  depth costs 1,024 x 2,000 x **est.** 0.2 us ≈ 0.41 s.
  - One per block: exec block 38 → ~450 ms, matched/s about -91%.
  - Std late window (about 173 per side): 35 ms per action, about -48%.
  - The reservation is released after the batch, so the attacker's capital is reusable every block.
- Clones alone, almost no margin: 150-1,000 map entries x ~140 B ≈ 2-20 us per FOK, so 2-20 ms per action.
- After the fix: an unfillable FOK costs O(levels) or less, and any action is capped at about 3.3 ms.
- Bench: 0.

**Nearest prior.** Grepped "FOK|fill.or.kill|can_fill|precheck|work budget|STP" and found 0 FOK entries.
- adl-work-budget (merged) is the precedent for a consensus work budget.
- exec.md corner 2 saw the clone but did not size it.

**Consensus-visible:** (a) and (b) no; (c) yes.

**Risks:**
- The bound must be a true upper bound. Needs a differential test against the walk.
- Level-total upkeep on pool jobs: **est.** <0.5% of match CPU.
- (c) changes the result for honest FOKs that need more than 16k makers.

**Effort:** 3-5 person-days.

**Prototype:**
- Bench a book with D ∈ {2k, 20k} asks from 50 makers and `AccountMargins` holding {150, 1k} traders. Place 1,024 FOKs
  with qty = depth + 1 lot and record ns per FOK today, with early reject, and with copy-on-write.
- Add a `ubench_econ` knob and record engine ms per block.

---

## 3. stop-index-cancel-paths: parked stops times failing by-id cancels

**Mechanism.** Pending stops are an unsorted `Vec<StopOrder>` of about 176 B each (`order_book.rs:1031-1048`). Three
non-fill paths scan it linearly:
- (1) A `CancelOrder` whose id is not resting probes `pending_stop(id)` (`:2576-2578`, a linear find) in every book
  (`native_executor.rs:12332-12335`). That is O(all stops chain-wide) per failing by-id cancel, which counts as one
  priority cancel.
- (2) Each cancel-all visit runs `take_pending_stops` (`:2155-2171`) or `retain_stops`.
- (3) `add_open_order_counts` (`:2465-2482`) walks every book's stops, in both `execute_batch` calls of every block.

Fix: a node-local index per book, `HashMap<OrderId, slot>` plus a list of stop ids per trader, updated at every place
stops change. Keep the Vec for trigger order. Results are byte-identical.

**Target:** matched/s, block p99.

**Expected gain.**
- 300 accounts x 1,000 dust stops = S 300k stops = 53 MB. The open-order limit counts stops (`position.rs:244-256`).
- A failing by-id cancel streams 53 MB on E, ≈ 4-5 ms.
  - 20 per block: +90 ms, exec 38 → 128 ms, about -70%.
  - All ~76 free cancel slots: +340 ms, about -90%.
- On top of that: open-order counting costs about 9 ms CPU per block, and the cancel-all visits cost about 15 ms of pool
  CPU.
- After the fix: about 6 us per failing by-id.
- Bench: 0 (no stops).

**Nearest prior.** Grepped "stop|pending_stop|by-id|P2-1b".
- stop-order-scan-and-fills (proposed): per-FILL trigger rescan with a price index. That is a different path and a
  different index.
- order-id-to-market-map (rejected): resting orders, with 0.6-1.4 ms/blk of upkeep. This index covers stops only, so
  bench upkeep is zero.
- open-order-count-via-trader-markets (proposed): covers the resting part only.

**Consensus-visible:** no.

**Risks:**
- Every stop mutation site has to keep the index in sync.
- About +60 B per stop.

**Effort:** 2-3 person-days.

**Prototype:**
- Microbench: 300 books x {1k, 3k} stops. Time the cancel-miss path, `take_pending_stops` and `open_order_counts`
  before and after.
- `ubench_econ` knob: park S stops, then add k failing by-ids per block.

---

## 4. stop-trigger-budget-carryover: no per-block bound on a stop cascade

**Mechanism.** Stops fire on `last_trade_price` (`order_book.rs:3341-3360`), and every eligible stop fires at once.
`run_triggered_stops` (`native_executor.rs:11795-11820`, called at `:9103` and `:11782`) places each one serially on E
through `place_order_inner`. Stops fired by those fills join the queue. The code comment says "bounded by the pending
set", which means there is no per-block bound. A single 1-lot self-trade at the band edge fires every parked stop in
that market.

Fix (consensus, the ADL work-budget pattern):
- At most T_max = 1,024 triggered placements per block.
- The rest stay in a persisted FIFO in trigger order and run first in the next block(s).
- A queued stop keeps its reservation and can still be cancelled.

**Target:** block p99, blk/s.

**Expected gain.**
- 100 accounts x 1,000 dust stop-markets → T = 100k single-path placements in one block.
  - At **est.** 10-30 us each, that is 1-3 s of E, plus the per-fill rescans of the remaining stops (the ledger item).
  - The 64-deep exec channel fills in about 3 s.
- With T_max: at most +10-30 ms per block. The cascade drains in 98 blocks (≈ 4.5 s), and block p99 stays near 475 ms.
  Honest flash-crash stop clusters get the same bound.
- Bench: 0.

**Nearest prior.** Grepped "cascade|trigger|carry.over".
- adl-work-budget (merged) is the same pattern in ADL.
- stop-order-scan-and-fills has no budget.
- mark-price-hl-median (P2) changes what triggers a stop, not how many fire per block.

**Consensus-visible:** yes.

**Risks:**
- New persisted queue state and a golden re-pin.
- Stops execute late.
- Reservation accounting while a stop is queued.

**Effort:** 3-4 person-days.

**Prototype:** a `ubench_econ` knob that parks {10k, 100k} stops and fires them with one self-trade. Measure E ms for
that block and µs per single-path placement, then stub the budget.

---

## 5. deep-book-capital-floor: min resting notional plus a per-book resting cap

**Mechanism.** Only two things bound total resting depth today: 1,000-5,000 open orders per account
(`position.rs:244-256`) and the ±50% band. There is no notional floor: the only size check is qty ≥ lot
(`order_book.rs:1552-1553`).

Fix, both consensus rules:
- (a) A minimum resting notional N_min for any order that can rest. Check it at the pre-book reject and at RPC intake.
- (b) A resting cap K per book (about 20k). When a placement would go over it, evict the resting order farthest from the
  mark (ties: highest id) and release its margin.

**Target:** deep-book stability (RSS, close stall), block p99.

**Expected gain.**
- Measured per 1M resting (deep vs std; the deep shape also adds 1M accounts):
  - RSS +1.15 GB mean, max 5.68 GB;
  - checkpoint 175 vs 87 MB;
  - window close 61-70 ms mean, 126-160 ms p99;
  - -2.9% matched/s.
- With the hash on and no L6.1, as on main, the save cost 56 ms per block.
- Attack today: 10k accounts x 1,000 dust orders = 10M resting, for about 10k recyclable TRS.
  - Window close grows to about 0.6-1.6 s every ~4.5 s (≈ 14% of E), and block p99 ≥ 1.3 s.
  - Memory grows by about 3 GB of live book (**est.** ~300 B per order) plus about 1 GB per encoded copy. That passes
    the 8 GB validator class.
- N_min = 10 TRS at 5% IM makes 10M orders lock 5M TRS. K = 20k caps the whole chain at 6M orders (≈ 1.8 GB).
- Bench: 0 if N_min is set below the bench's smallest resting notional.

**Nearest prior.** Grepped "min.*notional|minimum|lot|resting cap|book cap|per.book".
- per-user-open-order-limit: bounds each account, not the total.
- offtick-dust-prebook-reject: qty ≥ lot, not notional.
- per-market-oi-cap-max-notional (P2): a maximum, and on open interest.
- mode 2/3 (excluded): changes storage, not admission.

**Consensus-visible:** yes.

**Risks:**
- Product overlap with the owner's P2.
- Eviction must be deterministic and margin-exact.
- N_min has to be set per market.

**Effort:** 3-5 person-days.

**Prototype:** generate 1M, 3M and 10M dust orders in `ubench_econ`. Measure close / save ms and jemalloc bytes per
order, and count the bench orders below each candidate N_min.

---

## Dropped after the novelty check

- **cancel-lane-fusion** (keep the cancel-all run together across interleaved by-id and cloid cancels). By-id cancels
  do split runs (`native_executor.rs:8395-8445`), and a run of one goes to the serial single path (`:12580`). But this
  "run-preserving hoist" is already in memory (84718b38 item 3, correction 7c46d88f, s58 round 2: a split costs
  ≤ 0.15-0.3 ms). Not novel.
- **Exec-cost feedback into the item B limiter** (node-local post-exec charge). Generic, and ideas 1-4 fix the concrete
  gaps it would only price.
- **Gossip invalid-signature peer penalty.** Already in the ledger (da-mirror-after-signature).
- **Liquidation scan dilution.** Already in the ledger (R06, liq-touched-accounts-first).
- **Selection address bias.** Already in the ledger (rotate-selection-cursor, in-block-seeded-priority-r31).
