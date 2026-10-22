# r1 lens margin-algo: margin pre-reserve / prepare, settle, valuation tail (exec thread E)

Base: main `eb993e3f` (read-only). Numbers: `map/measured.md` (std CL21: exec block 38.35 ms, margin 10.29,
tail 3.65, 11,544 fills/blk, ~15.5k placed/blk, ~39 PlaceOrderBatch x 400 orders/blk) and the q8re EXEC profile
`~/bench-results-matched/ozarchy-q8re-run/analysis/ozarchy-q8re-300m-r1-ONP-w1.txt` (main `fbe36391`, EXEC 75.18
ms/blk, 34.5k placed/blk = placed_s 341,963 / 9.9 blk/s; `git diff fbe36391 eb993e3f` shows no change to Phase 2's
`Projections` / `get_position` / `users::find`). Conversion used: results doc s67.4 (memory b6d2198b), block_ms
-10.2% -> matched/s +3.0%, native blk/s +9.5%. Nothing below was built or run.

## 1. phase2-run-dense-position-view: kill the cold binary-search chain in Phase 2 (and every typed position read)

**Mechanism.** Every new (sender, market) in a sender's run calls `reader.get_position` (`native_executor.rs:9267-9272`)
-> `resident_positions` (dirty set + `Users` map, `:2544-2550`, per order although the sender is constant) ->
`users::find` = `binary_search_by_key` over the trader's `Vec<Position>` (`users.rs:1537`). A bench sender holds
~300 positions of ~112 B (`position.rs:54-73`) = ~33 KB, cold at the start of its run: ~8 dependent misses per
lookup, no memory-level parallelism. Then `Projections` (`:2866-2915`) grows Vec(16) -> HashMap 16..256 per sender.
Change: (a) at a run's first order resolve the sender's slice once, sweep it once (independent, prefetchable loads)
into a reusable epoch-tagged `Vec<(u32 epoch, u16 idx)>` indexed by market id (ids < `DENSE_MARKETS` 4096, else the
current path), and keep the run's projections in a parallel dense array; per order = 2 array reads + 1 direct
`&ps[idx]`. Safe because a sender's place orders are contiguous in a call (action sort by (sender, hash)) and
projections are dropped at `SenderFold::finish`. (b) `users::find` becomes interpolation-guess + gallop with
binary-search fallback (identical result) for the pool's point lookups (`maker_position_px`, pass A typed reads).

**Evidence.** q8re EXEC: `AccountReader::get_position` SELF 4.32%, incl 4.96%; `Projections::get_or_try_insert` incl
6.54% -> 4.92 ms/blk at 34.5k placed = ~143 ns per order. Results doc s11.3: `find` is 0.99 of 1.27 ms/1k of
get_position (clone 0.16, resident lookup 0.10). Bench batch = 400 orders uniform over 300 markets
(`tools/bench-throughput/src/main.rs:871-877`) = ~221 distinct markets per batch -> 8.6k lookups/blk.
Process-wide: `get_position` 1.43% (pool 74%) + `Users::position` 1.28% (pool).

**Gain.** E today: 15.5k x 143 ns = 2.2 ms/blk in projection+lookup; after: ~221/400 x ~40 ns + 5 ns/order + 39 sweeps
x ~3 us = ~0.5 ms -> **-1.7 ms/blk = -4.4% exec block** (16% of margin) -> ~+1.3% matched/s, ~+4% blk/s; plus a
shorter pool critical path (part of 2.68 ms match wait + 0.93 pass A).

**Nearest prior.** Grepped `binary.?search|find|trader_positions|projection|dense|interpolat|sweep|records|prefetch|
position.?(index|lookup)`. l2-4-dense-trader-slots (rejected, s82 hasher precedent: interns ADDRESSES in match jobs to
save hashing) - here the target is dependent cache-miss chains, keyed by market, per run; adl-holder-record-slice
(proposed, ADL only); resident-caches-hoist-prefetch (proposed Aug, pre-Users, unspecific). Not PE (serial).

**Prototype (<=3 h).** (1) #[ignore] microbench: 5,000 traders x 300 Positions (>160 MB, cold), 221 random markets
per trader: ns/lookup for `find` vs interpolation vs sweep+dense. (2) Hack prepare_one per (a); `ubench_econ`
UB_MARKETS=300 UB_MARKS=1, 3 runs per arm: margin ms/1k and engine ms/1k; run `projections_tests` and the sums
shadow tests for identity.

## 2. phase2-order-prep-lookahead: state-free Phase-2 order constants computed while the block waits in the exec queue

**Mechanism.** Per order, prepare_one/stitch/phase2_markets compute values that depend only on the params and the
block's margin/market config: flags (`takes_open_slot`, `is_stop`, `never_rests`, `match_margin_checked`,
`takes_bid_floor`), `validate_order_price`, tick/lot shape (2 x `__modti3`), `reserve_price x qty` and its order-tier
IM (`try_reserve_for_qty_cfg`, `:10521-10535`: 2 x `__divti3`), the opening notional `placement_need` recomputes
(`margin.rs:172`), `markets.get` + `market_batches.entry` probes (`:9167`, `:9422`), `phase2_markets`' per-order
entry (`:11025-11038`). A helper (one `torus-exec-prep` thread, fed from `dispatch_to_exec` `app.rs:11421`, which
already walks every action) fills an `Arc<OnceLock<Phase2Prep>>` carried in `CommittedBlockMsg` (`app.rs:535`):
24-48 B SoA per order + config generation. E at Phase 2 uses it iff ready and generation matches, else computes
inline (never waits).

**Evidence.** Exec queue p50 9 (std) / 17-18 (deep, R), peak 66: blocks sit ~400 ms before E. Results doc s11.2:
first touch of order params is a cold ~150 ns load; s11.1: `try_reserve` + configs/basis ~0.29 of 2.66 ms/1k of
prepare_one. Binary imports `__divti3/__modti3@GCC_3.0` (nm on `~/.cargo-target-s80-base/release/torus-node`).

**Gain.** ~100-125 ns/order of state-free work x 15.5k = 1.6-1.9 ms -> ~0.15 ms of SoA reads: **-1.0 to -1.8 ms/blk
(2.6-4.7%)**; zero when the queue is empty (paced load). Overlaps idea 5 (if 5 lands, ~-40%).

**Nearest prior.** Grepped `lookahead|pre-?exec|prep|precompute|side.?table|annotat|ingress.*compute`. L6.3
pre-exec-lookahead-verify (proposed: trust keys, sort hashes, recovers) - same carrier idea, different payload;
complementary. exec-3stage-pipeline (rejected because the queue was empty at s351; now p50 9-18). exec-parallel-keys
KP (no gain: an in-exec parallel pre-pass E waits for). Here E never waits.

**Prototype.** ubench_econ: compute the prep for each block's actions outside the timed region (emulates the lane),
pass via a test hook; margin ms/1k A/B, n=3; time the prep alone per block.

## 3. mark-version-sums-prefill: exact async re-valuation when marks move, off the exec thread

**Mechanism.** Sums entries are tagged by `BlockMarks.version` (`:2272-2284`); any mark change invalidates every
trader, and the next reader rebuilds with `build_sums` over all positions (`cached_sums` `:2642-2681`): Phase 2
senders (E), Phase 3 `maker_free` (pool), and the liquidation window, 2,048 traders/blk on E (`liq_view`
`liquidation_step.rs:805`). `position_terms` costs 4 `__divti3` per position (disassembly). Change: when
`begin_block_oracle` produces a new version (or predictably at the end of N-1), helper threads walk a priority list
(liquidation window from the cursor, the block's senders, resting-order holders) and `get_or_init` the SAME memo
cells (`BlockSums.memo`, `:1942`) with the SAME closure; `SumsCarry::finish` persists them. Exact by construction;
E only ever hits or computes the cell itself.

**Evidence.** All 9j2 cells run ORACLE_WALK_BP=0 (`ozarchy-9j2-std-campaign.sh:410-416`), so the mark version never
changes in the bench. P2-4 (`docs/plans/item6-phase2-impl.md:298-325`): liquidation sweep 24.7 ms/blk at walk 10 vs
10.7 at walk 0; `liq-sweep-walk10-cost`: sums 6.5 -> 24.2 ms, matched 0.975x.

**Gain.** Standard bench: 0. Production-like 2 s oracle at 22 blk/s: ~5,000 holders x ~300 positions x ~50 ns =
~75 ms of E per version change (over ~2.4 blocks, +25-30 ms per block = tail spikes), ~1.7 ms/blk average (4.4%) ->
most of it off E; at walk-every-block (P2-4) up to ~14 ms/blk.

**Nearest prior.** Grepped `mark.?version|walk|re-?value|prevalue|certificate|sums`. liq-scan-parallel-prevalue
(rejected: synchronous pool pre-pass in the tail at walk 0, +0.6-0.8%); liq-l2-health-certificates (proposed:
bound-based skip); P2-4 (a) bound skip / (b) moved-markets-only (rejected); stored-position-terms-o1 (parked).
Difference: asynchronous, version-triggered, exact fill of existing cells, aimed at moving marks.

**Prototype.** ubench_econ UB_MARKS=1 UB_MARK_WALK=10 vs 0 (baseline cost); then a hack that, at each measured
block start, spawns 4 threads prefilling memo cells for window + senders; compare post-engine tail and margin
ms/blk, n=3.

## 4. liq-scan-row-existence-fastpath: stop probing the overlay twice per healthy scanned trader

**Mechanism.** Each of the 2,048 scanned traders (`liquidation_step.rs:433-462`) pays `set_pending` and
`clear_cooldown` (`torus-core/src/liquidation.rs:573-582, 643-649`), each a `get_cf_raw` = SipHash of the CF name in
`intern_cf` (`backend.rs:470`), RwLock, two BTreeMap probes, R lookup (`backend.rs:3038-3093`), almost always a
miss; plus an untyped `get_native_balance` with decode + Vec (`liquidation_step.rs:809`). Change: keep a
node-local set of traders that HAVE a pending/cooldown row (maintained in the resident slot from each block's delta,
like `Users.holders`); skip both gets for non-members when the block layer has no liquidation-CF writes before the
step (else fall back); read the balance typed (`get_native_balance_typed`, `position.rs:416`). Optional: run the
clean traders' `liq_view` inside E's Phase-3 pool wait (`exec_pool.rs:43-60` -> `in_place_scope`).

**Evidence.** Liquidation step 1.91 ms/blk fixed + 0.127 ms/1k fills (section 110); tail 3.65 ms (std CL21); q8re
EXEC: run_liquidations 7.81% of which liq_view 5.15% -> ~2.66% is outside valuation.

**Gain.** 2,048 x (2 misses ~120 ns + decode/alloc ~100 ns) = **-0.4 to -0.9 ms/blk (1-2.3%)**, fixed per block (rises
with blk/s); dead-time option up to -0.5 ms more.

**Nearest prior.** Grepped `liq|cooldown|pending|existence|typed`. liq-cf-resident-e2 (merged: CF moved to R),
liquidation-health-certificate (proposed: skip valuation per mark version), liq-scan-skip-unchanged-traders
(proposed), liq-scan-parallel-prevalue (rejected). Difference: exact negative-existence index plus typed read;
helps dirty traders too, no semantics change.

**Prototype.** ubench_econ UB_MARKS=1: hack the two checks + typed balance; post-engine tail ms/blk A/B, n=3; a
test that the step's writes and results are identical over random pending/cooldown fixtures.

## 5. fixedpoint-reciprocal-division: no `__divti3` PLT call on the hot margin paths

**Mechanism.** `FixedPoint::checked_mul` divides the i128 product by 10^8 (`torus-types/src/lib.rs:193-196`),
`order_initial_margin` divides by the tier leverage (`margin.rs:65-75`), `maintenance_margin` recomputes the IM
(`margin.rs:79-81`), shape checks use i128 `%`. All go to libgcc_s via PLT (`U __divti3@GCC_3.0`, `__modti3`).
Change: exact multiply-high by precomputed reciprocals (10^8 compile-time; leverage / tick / lot per config,
cached with the config), with sign handling and an i256 fallback outside the proven range; compute IM once in
`position_terms`.

**Evidence.** q8re EXEC self: `[unknown] libgcc_s.so.1` 1.90% + `FixedPoint::checked_mul` 1.17% = 2.31 ms/blk at 25k
fills -> ~1.06 ms/blk today (11.5k fills). Disassembly: `position_terms` has 4 `__divti3` calls.

**Gain.** ~-60% of that: **-0.5 to -0.7 ms/blk on E (1.3-1.8%)**, plus pool (match / pass A) and ~45% cheaper
re-valuations (helps idea 3's regime).

**Nearest prior.** Grepped `divti|i128|fixed.?point|division|integer.?lev|reciprocal|magic`.
fixedpoint-i128-fast-path (merged: avoids i256), fixedpoint-u64-fastpath (rejected: products >= 2^64; this works on
full i128), reserve-integer-leverage (parked: still divides), cancel-all-integer-leverage (inconclusive).

**Prototype.** #[ignore] test: 10^8 random bench-shaped and edge operands (signs, i128 bounds, the overflow path):
identical results vs the current code; ns/op; then ubench_econ engine ms/1k A/B (ABBA, n>=4: binary drift is +-5%).
