# r2 lens: worst-block catalogue (adversarial and pathological single-block cost)

Code refs are current main `2d2f3ed9` (eb993e3f + batch 10; the exec paths cited here are unchanged by batch 10).
NE = `crates/torus-bridge/src/native_executor.rs`, OB = `crates/torus-core/src/order_book.rs`,
CB = `crates/torus-core/src/order_book/cancel_batch.rs`, LS = `crates/torus-bridge/src/liquidation_step.rs`,
RW = `crates/torus-economics/src/rewards.rs`, ST = `crates/torus-economics/src/staking.rs`.
Base numbers come from `map/measured.md`: std CL21 exec block 38.35 ms, 21.96 blk/s, block p99 475 ms, exec queue p90 42
of 64. When E-bound, matched/s scales roughly as 38.35 / (38.35 + extra ms per block). **est.** = estimated from the
code, not measured.

## 0. Catalogue (every path the lens names, plus what turned up)

| path | client input that grows the cost | cost driver (code) | status |
|---|---|---|---|
| ADL cascade | many-position bankrupt accounts | budget W = 100k (`liquidation.rs:72`), B never cut; S=750-like B1 738-786 ms measured | covered (adl-work-budget, adl-holder-units-b-in-w, adl-running-rest) |
| cancel-by-cloid | none | one book probe (NE 13886-13910) | no amplifier |
| cancel-by-id / modify | deep levels, parked stops | all-book probe (NE 13760-13790, 14558); `VecDeque::remove` shift (OB 2105, 2208); miss path linear stop scan (NE 13783) | covered (order-id-to-market-map, market-in-order-id, lazy-delete-level-queues, r1 stop-index-cancel-paths) |
| **liquidation stage 1 (in-block cascade)** | accounts with P positions in the [2/3 MM, MM) band | full re-valuation after every close: O(P^2) per account (LS 895-934) | **new: idea 2** |
| trigger_stops O(S) | parked stops | `retain_stops` per filling placement | covered (stop-order-scan-and-fills, R09, R16) |
| oracle-only / mark-move storm | none (reporters are validators, NE 15208-15230) | mark version renew, then memo rebuild | covered (r1 mark-version-sums-prefill); no client amplifier |
| many-market cancel-all | orders in many books | P2-1 index visits only the books the trader holds; paid by placements | covered (cancel-all index R28; r1 #16 for the stop scans) |
| deep-level sweeps | many resting makers consumed in one block | consumed makers are paid by placements; time-shifted bursts | covered (R16 option B) |
| account churn | new accounts | Users = HashMap + BTreeSet (users.rs:166-183), O(log N); scan dilution | covered (R06) |
| funding | - | not implemented (R10) | n/a |
| delist / param-update cancel | deep book + many traders + parked stops in one market | per-trader cancel: 2 x O(S_m) stop scans + per-order level shifts (NE 16196-16205, also `delist_settlement.rs:216`) | **new: idea 3** (the position force-close is already bounded by delist-option-c) |
| epoch / fee reward distribution | dust delegations and permanent stakes | full delegation-CF scan per validator + one reward RMW per delegator (RW 206-247, ST 1085-1097) | scan covered (delegations-scan-index); **O(D) writes new: idea 4** |
| **reduce-only policing install** | K accounts each with a dust position + 1 resting RO order | O(R + K reads) per (market, call) and per single-path placement (NE 13056-13076, 13541-13553, 13141-13167) | **new: idea 1** |
| CoreWriter drain | EVM txs | 200 rows/block cap (`torus-types/src/lib.rs:799`); each row runs the single path (NE 15800) | cap covered; multiplies idea 1 and r1 #16 |
| FOK / RO sweep | batched FOKs, kept RO orders | per-FOK clone + walk; O(R) sweep | covered (r1 #5, #10) |
| sessions | - | max 5 per address (NE 15115) | bounded |

---

## 1. lazy-ro-policing: load reduce-only policing positions on demand, not for every RO owner up front

**Mechanism**

- **Batch path.** Every match call installs `ReduceOnlyPositions` for every trader that owns a resting reduce-only
  order in the book (`match_setup`, NE 13056-13076). `reduce_only_positions_for` (NE 13141-13167) then:
  - walks the whole `reduce_only_index` via `reduce_only_traders()` (OB 2036-2040);
  - builds a `BTreeSet<Address>`;
  - does one `get_position` per owner.
- **Single path.** It repeats the install for every placement in a book that has any RO order (NE 13541-13553). That
  covers triggered stops, stage-1 liquidation closes and CoreWriter placements (NE 15800).
- **How the map is read.** Only by key (OB 423-425; uses at OB 1610, 1668, 1701, 1747, 1754, 1838, 2892, 2931,
  3019-3021).

**Fix**

1. Install only the senders, whose positions Phase 2 already read (`pre_pos`), plus a flag recording that the book
   had RO owners at install time.
2. On a miss in `get` or `apply_fill`, check whether the trader is policed: it has an entry in `reduce_only_index`, or
   it is in a small `departed` set of owners whose entries were removed since install.
3. If it is policed, load its position from the frozen reader and memoise it. The load goes through a new
   `policed_position` on `MakerAccountSource`, which AccountReader already implements (NE 3087).

**Why it is exact.** The reader is frozen during the match (Phase 3). On the single path, positions are applied only
after `place_order_with_accounts` returns. So a lazy load returns exactly what the eager load read.

**Target:** matched/s and block p99 under attack, deep-book stability. Bench: 0, because the bench has no RO orders
and the gate is off.

**Expected gain**

- **Per-owner cost today:** ~0.2-0.3 us (est.). The ADL C2 profile measured 114-200 ns per holder position read
  (adl-budget.md 11.5); a BTreeSet insert and a HashMap insert come on top.
- **Standing attack S1:** 10k accounts, each with a 1-lot position and 1 RO order in each of 300 markets (3M RO
  orders).
  - Pool CPU per call: 300 x 10k x 0.25 us = 0.75 s, at two calls per block.
  - On a 32-thread host shared by 3 nodes that is roughly +75 ms wall per call, so +150 ms/blk.
  - Exec block goes from 38 to ~190 ms, matched/s about -80%. The attacker pays no recurring cost.
- **Attack S2:** one market with K = 100k owners.
  - +25 ms per call in that market's job (E waits on the straggler), so +50 ms/blk (matched/s -57%).
  - Every single-path placement there also pays 25 ms: 200 CoreWriter placements add +5 s to one block, and a
    1,000-stop cascade adds +25 s.
- **After the fix:** O(senders + RO owners actually touched by fills), i.e. bounded by work the attacker pays for.

**Nearest prior.** Grepped `reduce.only|ro.position|ReduceOnlyPositions|install|lazy|policed`.

- parallel-match-setup (default-on) moved the same eager install into the pool job: same work, different thread.
- r1 fok-precheck-bounded noticed the map can be inflated, but fixes only the FOK clone (copy-on-write).
- r1 ro-sweep-aggregate makes the per-trader sweep O(1).

None removes the O(R + K) install per call and per single-path placement.

**Risks**

- Membership must mean "policed at install time". Index entries removed mid-call (fills, cuts, cancels) must go into
  `departed`. Otherwise a later fill of that owner's non-RO order would skip a maker check that the eager map runs.
- The `is_empty()` gate (OB 1838) must use the install flag.
- The FOK scratch clone must share the departed set.
- Shadow mode: keep the eager map in debug builds and assert equality on every `get`.

**Effort:** 2-4 person-days. **Consensus-visible:** no (exact).

**Prototype (<= 3 h)**

1. In `ubench_econ.rs`, add `UB_RO_OWNERS=K` and `UB_RO_MARKETS=M`. Before the load, seed K traders, each with:
   - a 1-lot Cross position (pairs long/short);
   - one resting reduce-only order in markets 1..M, placed via `OrderBook::place_order_with_accounts` with the RO map
     installed.
2. Run arms K in {0, 10k, 100k} and M in {1, 300}. Add a stub arm that installs senders only; it is exact for this
   shape because no seeded RO order is ever filled.
3. Record:
   - `match_setup` time per market (wrap NE 13056-13076 in a timer);
   - engine ms/blk;
   - the time of 100 single-path 1-lot IOCs in market 1 (`execute` with `PlaceOrder`).
4. Kill if K = 100k, M = 1 adds less than 5 ms per call.

---

## 2. liq-stage1-running-view: stage-1 liquidation without O(P^2) re-valuation, plus a position-unit act budget

**Mechanism**

- `stage1` (LS 885-959) places one reduce-only IOC per marked position of the account (LS 895-924). After each close
  it runs a full `liq_view` (LS 930).
- After its first fill the trader is dirty, so `pos_sums` takes one of two dirty paths:
  - `delta_sums` (NE 2917-2935) materialises every row the block changed under the trader's prefix
    (`resident_changes`, `crates/torus-state/src/backend.rs:3484-3502`). Per row that is a key clone, a Users record
    encode, a value clone and two decodes.
  - `direct_sums` walks all P rows.
- So close k costs O(k), which makes stage 1 O(P^2) per account. That repeats for up to 64 acts per block
  (`LIQ_ACT_PER_BLOCK`, `liquidation.rs:60`).

**Fix A (exact, node-local)**

- Keep a running `PosSums` through the loop.
- Per close, subtract the terms of the position row that `stage1` already reads before placing the order (LS ~905),
  and add the terms of the row read after it (one point read).
- Only market m changes in a close, because all of the account's orders and stops were cancelled first (LS 477).
- Keep `delta_sums`'s ABS guard, and fall back to today's path above it.

**Fix B (consensus-visible, P0.0 bit)**

- Charge acts in stage-1 orders: at most 2,048 stage-1 orders per block, with at least one account per block always
  allowed.
- This is the delist-option-C pattern (2,048 traders / 256 positions per block).

**Target:** block p99 and liveness in a market shock; liquidation latency.

**Expected gain**

- **Calibration.** The r1 liq-tail-offload probe measured dirty-trader valuation at 5.2-5.3 ms for ~650 dirty
  traders, about 8 us each. That puts the cost at ~0.4-0.8 us per changed row (est.).
- **Today, at P = 270 positions** (the S=750 cell shape, adl-budget.md section 1):
  - re-valuation: 36.5k rows in total x 0.4-0.8 us = 15-29 ms;
  - closes: 270 x 5-15 us = 1.4-4 ms;
  - total 16-33 ms per account, so **1.0-2.1 s per 64-act block**, for ceil(N / 64) blocks.
- **Why this was never measured.** adl-budget.md section 1 notes that a 251-333 bp shock puts that population in the
  stage-1 band, but the measured cells only reached backstop (S=400) or ADL (S=750).
- **Fix A:** 64 x 270 x (6-16 us) = 0.10-0.28 s per block.
- **Fix B:** at most 2,048 x 6-16 us = 12-33 ms per block.
- **Bench:** 0, because no stage-1 acts happen in the cells.

**Nearest prior.** Grepped `stage.?1|liquidation order|positions per|per.account|liq.*budget|running`.

- adl-running-rest (merged) used the same running-sum idea to make ADL block B O(P). Stage 1 never got it.
- delist-option-c bounds positions per block for delist only.
- R06 options A/B/C change priority and caps, not the unit the budget counts.
- liq-scan-skip-unchanged-traders and L1.3 work on the scan, not on the act loop.

**Risks**

- Fix A: the balance changes with realised PnL, so it must be read fresh after each close (one point read).
- Fix A: the `marked` and `any_isolated` counters must track positions going flat.
- Fix A: shadow-assert against `liq_view` in tests.
- Fix B needs owner sign-off on the economics: the rest of a shocked population waits more blocks.

**Effort:** A 2-3 person-days; B 2-3 person-days (feature bit, golden re-pin).

**Prototype (<= 3 h)**

1. Extend `crates/torus-bridge/tests/ubench_adl.rs` with `UB_ADL_DROP=<mark units>`: from block 2, marks drop by
   that amount instead of 1,000.
2. Top up the bankrupt accounts' balances so they land at AV/MM about 0.8.
3. From a maker account, rest one 1-lot ask/bid per market at the slippage cap, so that every close partially fills.
4. Run `UB_ADL_TRADERS=5000 UB_ADL_BANKRUPT=64 UB_ADL_POSITIONS={30,100,270}` and read the `liquidation step` ms.
5. Fit ms against P; the quadratic term should dominate at 270.
6. Add the running-sums arm behind `TORUS_S80_STAGE1_RUNNING=1`, with a shadow assert.
7. Kill if the step is under 250 ms at 64 x 270, or if the arm saves less than 50%.

---

## 3. gov-market-wipe: governance market delist / param update drains the book in one pass

**Mechanism**

- An executed `MarketDelist`, or a `MarketParamsUpdate` that changes tick, lot or margin config (NE 16134-16166),
  runs `cancel_market_orders_and_stops` (NE 16196-16205). Delist settlement blocks call it too
  (`delist_settlement.rs:216`).
- It loops over every trader present in the book, in sorted order, calling `cancel_orders_and_stops`
  (NE 13937-13990) for each one.
- Per trader:
  - `take_pending_stops` does an `any` scan over all of the book's stops plus a partition (OB 2155-2171).
  - `cancel_all` then calls `retain_stops`, another full pass (OB 2175-2183, 3188-3196).
  - `cancel_all_resting` does one `VecDeque::remove` per order. Each remove shifts min(i, n - i) orders of the level
    (OB 2186-2208).
- The batch path only applies to traders with at least 32 orders and a concentrated prefix (CB 167-206). Even then it
  compacts the whole level once per trader.
- Total cost: O(T x S + sum of per-order shifts), which is quadratic in book size.

**Fix**

- Every order and stop in the book is cancelled, so drain the book once:
  1. `mem::take` the levels and the stops.
  2. Accumulate each trader's margin (`cancelled_orders_margin` + `stop_reservation`) in a BTreeMap. These are integer
     sums, so their order does not matter.
  3. Clear the book's indexes: order_index, order_seq, trader_orders, cloids, RO index.
  4. Drop the market from the P2-1 and cloid indexes, and mark the book dirty.
  5. Call `release_order_margin` per trader in ascending address order. That is the same balance-write sequence as
     today.
- A lower-risk first step: one stop partition, plus the proven `cancel_all_many(&traders)` (CB 311), which is
  state-equivalent to the sequential loop.

**Target:** block p99 and liveness on governance blocks; deep-book stability.

**Expected gain.** Assumes ~176 B per order and per stop (est., exec.md) and memmove at ~5-10 GB/s (est.).

| book in the market | cost today | approx. time |
|---|---|---|
| 10k traders x 100 stops (S = 1M) | 2 x 10k full passes over 176 MB = ~3.5 TB of traffic | minutes |
| 1M orders in two levels, from 32k traders x 31 orders (non-batch path) | ~n^2/8 = 1.25e11 order moves = ~22 TB | ~1 h |
| 1M orders from 1k traders x 1,000 orders (batch path) | 1k x 1M x 176 B = 176 GB | 20-35 s |
| honest deep testnet queue (195k resting, ~50 makers) | ~1.7 GB | 0.2-0.3 s |

- After the fix: O(n + S + T log T). For 1M orders + 1M stops + 32k traders that is about 0.1-0.2 s (est.).
- Bench: 0.

**Nearest prior.** Grepped `delist|params.?update|wipe|clear.*book|cancel_market|tombstone|compaction`.

- delist-option-c (merged) bounds the position force-close, not this resting-order cancel.
- lazy-delete-level-queues and phase1-cancel-tombstones (proposed) tombstone per-order removals.
- cancel-all-in-place-compaction, cancel-all-two-ended-compaction and cancel-levels-lazy (tried-inconclusive) batch
  user cancel-all runs.
- r1 stop-index-cancel-paths adds a per-book stop index.

None treats "cancel everything in one market" as a single drain.

**Risks**

- Every derived book structure must end exactly as the per-trader loop leaves it: journals and epochs (modes 2/3
  only), the P2-1 and cloid indexes, `new_traders` and `saved_bbo`.
- This needs a differential test against the loop on random books with stops, RO orders and cloids.

**Effort:** 1.5-3 person-days. **Consensus-visible:** no (exact).

**Prototype (<= 3 h)**

1. Write an `#[ignore]` test in `crates/torus-core/tests/`.
2. Build one `OrderBook` with n orders at two prices from T traders (interleaved), plus S stops.
3. Time the per-trader loop (`take_pending_stops` + `cancel_all` per sorted trader, as in NE 13968-13969) against a
   drain prototype, for n in {30k, 100k, 300k}, T in {100, 1k, 10k} and S in {0, 100k}.
4. Extrapolate to 1M quadratically.
5. Assert that the drain's per-trader margin inputs equal the loop's.
6. Kill if the loop takes less than 250 ms at n = 300k, T = 10k.

---

## 4. staking-reward-index: O(V) epoch and fee-block reward distribution (cumulative reward per stake unit)

**Mechanism**

- **Every epoch boundary.** `epoch_length` is 100, the same height as the %100 checkpoint close and the L6.1 forced
  book save. Two loops run:
  - `distribute_permanent_staking_rewards` does a full `iterate_cf(CF_STAKING_PERMANENT)` and one balance RMW per
    staker (RW 96-125, ST 1192-1201).
  - `distribute_validator_inflation` calls `split_validator_reward` per active validator. That does a full scan of
    `CF_STAKING_DELEGATIONS` filtered by suffix (ST 1085-1097), then one `credit_rewards` RMW per delegation
    (RW 206-247, ST 997-1008).
- **Every block with fees > 0.** The same distribution runs for the proposer (RW 24-90, NE 15956-15990). In practice
  these are EVM-fee blocks. The validator share is > 0 from epoch 1 (2500 bps x e / 1825).
- **Why dust is enough.** There is no minimum delegation (ST 103-140). A dust delegation still earns a non-zero share
  from about 3.2e6 wei of stake per epoch.

**Fix: F1-style accounting**

- Each validator keeps a cumulative reward-per-delegated-unit index; permanent stake shares one global index.
- Epoch and fee distribution then update V + 1 indexes.
- Each delegation stores its entry index; pending = amount x (index_now - index_entry).
- Pending is settled on delegate, undelegate, claim and slash.

**Target:** block p99 (the %100 block class is exactly 1% of blocks) and exec age after epochs.

**Expected gain.** Assumes 0.3-0.6 us per row for a full-CF materialisation and 1-2 us per RMW (est.).

- Setup: D = 300k dust delegations (100k accounts x 3 validators), about 1,000 blocks of actions.
- Epoch block: 3 x 300k x 0.3-0.6 us + 300k x 1-2 us = 0.6-1.1 s, every 100 blocks. This lands on the block that
  already carries the checkpoint close.
- EVM-fee block (every 11th block at N = 11): 0.2-0.4 s.
- Effect on consensus: exec queue p90 is 42 of 64, so a 1 s stall (22 blocks) fills the channel and commit-interval
  p99 rises.
- Organic case, 50k delegators: +0.1-0.2 s per epoch block.
- After the fix: O(V) per epoch and per fee block. Bench: 0 today (no delegations).

**Nearest prior.** Grepped `delegat|reward|staker|permanent|per.share|accumulat|claim|epoch`.

- delegations-scan-index and delegations-for-validator-full-scan (proposed) index the scan. The O(D) reward writes
  per epoch and per fee block remain.
- governance-stake-checkpoints (proposed) covers the governance O(stakers) snapshot.
- epoch-boundary-fast-path (merged) removed the barrier, not this work.

**Risks**

- Consensus-visible state format.
- Rounding: today the last delegator takes the remainder; F1 leaves dust that must be tracked.
- Interacts with slashing of delegations and with unbonding.
- Needs owner sign-off. A fresh genesis per launch (R17 policy) avoids a migration.

**Effort:** 5-8 person-days. **Consensus-visible:** yes.

**Prototype (<= 3 h)**

1. Use `crates/torus-bridge/tests/ubench_epoch.rs`, which already times the x00 / x01 / other block classes with
   `epoch_length` 100.
2. Add `UB_SEED_DELEGATIONS=D`: raw-put D delegation rows over the 3 validators, plus D/3 permanent stakes, each above
   the dust threshold.
3. Add a `UB_FEE_EVERY=11` arm that calls `distribute_fees` with a non-zero fee.
4. Measure x00 ms and fee-block ms at D in {0, 30k, 300k}.
5. Kill if x00 at D = 300k adds less than 100 ms.
