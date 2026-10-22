# s80 prototype: ro-basis-walk-memo

Branch `s80/ro-basis-walk-memo` (local only), worktree `/home/oz/projects/wt/novel-s80/proto/ro-basis-walk-memo`,
base 29bfc705, commit ce73bc73. Target dir `/home/oz/.cargo-target-s80-ro-basis-walk-memo` (release test binaries;
no node build: session rule for this batch, no cells). Flag `TORUS_S80_RO_BASIS_MEMO=1` (default off = 29bfc705
behaviour; read once at the first `phase2_reservation_basis` call, INFO `TORUS_S80_RO_BASIS_MEMO on=<bool>`).
Host: ozarchy, every timing under the heavy-build lock.

## 1. Premise check on 29bfc705

Re-located on 29bfc705 (main's write-up lines differ):
- `NativeExecutor::phase2_reservation_basis` `crates/torus-bridge/src/native_executor.rs:12384` (29bfc705), called
  once per `execute_batch` place set at `:9340`, inside the Phase-2 `margin_timer` (`:9279`, booked to
  `phase_accum.margin_ns` `:9537`), before any open-slot or margin check.
- The walk: `:12420-12426`, for EVERY reduce-only non-stop order whose position read succeeds:
  `book.orders_for_trader(&sender)` (`crates/torus-core/src/order_book.rs:2448`: a `Vec<&Order>` built with one
  `get_order` per resting order: `order_index` probe, level `BTreeMap` get, `order_seq` binary search
  `:2376-2410`), filtered to non-reduce-only, folded with `sat_add`.
- Phase 2 holds `&NativeExecContext` (books cannot change inside the call), so the sum is a pure function of
  (sender, market) per call. It is the only exec-path caller of `orders_for_trader`.

Measured (release, in-crate `#[ignore]` `ro_basis_walk_timing`, one call of `phase2_reservation_basis` holding
1,024 reduce-only sells of sender S, S resting R dust non-RO bids, plus one other trader's order per 4 in the same
queues; median of 11 runs per arm, arms alternating):

| R | levels | base ms / call | base us / RO order | memo ms / call | cut |
|---|---|---|---|---|---|
| 100 | 1 | 6.309 | 6.16 | 0.366 | 17x |
| 100 | 50 | 5.293 | 5.17 | 0.369 | 14x |
| 1,000 | 1 | **106.898** | 104.4 | 0.490 | 218x |
| 1,000 | 50 | **72.964** | 71.3 | 0.471 | 155x |
| 5,000 | 1 | 725.726 | 708.7 | 1.050 | 691x |
| 5,000 | 50 | 704.305 | 687.8 | 1.030 | 684x |

Raw runs: `/home/oz/projects/wt/novel-s80/proto/results/ro-basis-walk-memo-timing-1.log`.

Kill line "base R = 1,000 < 20 ms per action": base is 73-107 ms per 1,024-order action, **premise holds, not
killed** (the write-up's 82-154 ms band: our 50-level case is a little under it, the one-level case inside it).
~70-104 ns per resting order per RO order. The memo's remaining ~0.36 ms is the 1,024 `signed_position` state
reads (one per RO order, unchanged), plus one walk of R (~0.12 ms at R = 1,000, ~0.7 ms at 5,000).

Attack arithmetic (from the measured per-call cost): 64 such actions per sender per block at R = 1,000 is
64 x 73-107 ms = 4.7-6.8 s of serial engine time per block per attacking sender; R = 5,000 (open limit after
volume) 45-46 s. After the memo: one walk per (sender, market) per `execute_batch` call, 64 x ~0.5 ms = ~31 ms of
which ~23 ms are the position reads (the next floor, not addressed here).

## 2. Design

- `torus-core` `OrderBook::trader_orders_iter(&self, trader) -> impl Iterator<Item = &Order>`
  (`order_book.rs:2457`): `orders_for_trader` without the `Vec` (same orders, same `trader_orders` order). Additive.
- `torus-bridge` `native_executor.rs`:
  - flag plumbing `:917-1000`: `parse_ro_basis_memo` (only `1` / `true`, trimmed, any case, = on), thread-local pin
    `with_ro_basis_memo(on, f)` (tests / differential runs; the basis runs on the `execute_batch` caller's thread),
    `ro_basis_memo_enabled()` (pin, else env read once + INFO line), walk counter `ro_basis_walks_on_this_thread()`.
  - `phase2_reservation_basis` (`:12475`) = `phase2_reservation_basis_with(ctx, orders, ro_basis_memo_enabled())`.
  - `phase2_reservation_basis_with` (`:12492`): a `HashMap<(Address, MarketId), FixedPoint>` beside `growth`
    (empty, never allocated, with the flag off). Memo arm: `entry((sender, market)).or_insert_with(walk)`, the walk
    being `trader_orders_iter` filtered to non-RO and folded with the same `sat_add` from `ZERO` in the same order.
    Off arm: the 29bfc705 code verbatim (+ the walk counter). Everything else (position read and its fault note,
    `growth`, the closing allowance, the output rule) is untouched, so the map is byte-identical by construction:
    same inputs, same fold, and `resting` is only consumed where it was.
- Not consensus-visible: the output map is identical; no state, wire or hash change.
- Out of scope (as the write-up says): the match-time RO sweep (r1 #10) and the per-RO-order `signed_position`
  read stay O(1,024) per action.

## 3. Tests (written first; the first compile failed on the missing `phase2_reservation_basis_with`,
`with_ro_basis_memo`, `ro_basis_walks_on_this_thread`, `trader_orders_iter`)

New:
- `crates/torus-bridge/src/ro_basis_memo_tests.rs` (in-crate, next to `sell_top_up_stitch_tests.rs`):
  - `memo_matches_walk_on_random_books` (300 rounds): random books (1-4 markets, a missing book 1 in 4, 0-80 orders,
    1 in 3 resting reduce-only, partial fills from a crossing IOC, 1 in 40 near-`i128::MAX/3` sizes so sums
    saturate), random signed positions, 1-80 Phase-2 orders of every type (Market / IOC / FOK / PostOnly /
    StopLimit, 40% reduce-only, sizes incl. saturating). Asserts memo map == walk map, base walks == RO non-stop
    orders, memo walks == distinct (sender, market) pairs, and the env entry point pinned off / on == the walk.
    Generator check: > 150 rounds with overrides, > 1,000 walks saved.
  - `memo_walks_once_per_pair`: 101 walks base vs 3 memo; resting RO orders excluded, no-book market bound 0,
    short-position allowance, the stop keeps its size, an earlier non-RO order in the batch grows a later bound.
  - `ro_basis_walk_timing` (`#[ignore]`): the table in section 1 (asserts identical maps every run).
  - `ro_basis_memo_flag_tests::{parse_off_by_default, pin_overrides_and_restores}`.
- `crates/torus-core/src/order_book.rs` `trader_orders_iter_is_orders_for_trader`: small list, large list with holes
  (cancels), maker fills / partials, unknown trader.
- `crates/torus-bridge/tests/reduce_only_tests.rs` (the spec's harness):
  - `ro_basis_memo_one_call_same_results_ids_and_state`: S rests R dust bids (one level / 50 / 7), optionally long
    against a maker, then ONE `execute_batch`: a 1,024-order RO `PlaceOrderBatch`, 64 single RO sells, a maker
    taker buy that crosses RO sells; serial (threads 1) and sharded Phase 2 (threads 4). Flag off vs on: identical
    per-action results (success, error text, reason), every resting order (id, trader, side, price, remaining,
    original, RO), `next_global_order_id`, S's position and every `HASHED_CFS` row after `save_order_books`; walks
    1,088 vs 1.
  - `ro_basis_memo_e2e_timing` (`#[ignore]`): the call's `phase_accum.margin_ns`, section 5.

Results (release nextest, ozarchy, logs `/home/oz/projects/wt/novel-s80/proto/results/rbwm-tests.log`):
- `cargo nextest run --release -p torus-core`: 418/418 passed (14 skipped), incl. `trader_orders_iter_is_orders_for_trader`.
- `cargo nextest run --release -p torus-bridge` flag OFF (env unset): 1,094 run, 1,093 passed, 1 failed: the first
  version of `ro_basis_memo_one_call_same_results_ids_and_state` failed only its own sanity line ("some RO order
  rests") on the long-3 case, where the taker legitimately closes the position through the RO sells; every equality
  assert (results, book, ids, rows, walks) had passed before it. Fixed the sanity line (long 40: RO rests; long 3:
  the position shrank; flat: stays flat). The test pins both arms itself (`with_ro_basis_memo`), so the env does
  not matter for it.
- `TORUS_S80_RO_BASIS_MEMO=1 cargo nextest run --release -p torus-bridge` flag ON (built after the fix): **1,094/1,094
  passed** (19 skipped), incl. `perf_equivalence_golden` (5 tests, none ignored; scenario A sends ~5% reduce-only
  GTC orders, so the memo arm is exercised against the pinned digests) and the fixed e2e test.
- The literal golden command `-E 'test(perf_equivalence_golden)'` matches no test names (nextest exit 4, "no tests
  to run"); the golden binary ran inside both full-crate runs above (flag off: passed; flag on: passed). A direct
  re-run of the golden / reduce_only_tests / lib ro_basis binaries off and on was queued
  (`results/rbwm-verify.sh`, log `results/rbwm-verify.log`) but may not have completed before the stage ended.

## 4. Microbench (ubench_econ, engine proxy; the bench sends no RO orders, so this is the no-regression check)

`ub.sh`-equivalent runs (same env defaults: UB_MARKETS=300 UB_RUNS=5 UB_MARKS=1 TORUS_SETTLE_STREAMED=1, same
binary `ubench_econ-1a6f456edd610a23`), all under one lock hold (`results/rbwm-bench2.sh`), ABBA planned
off-1, on-1, on-2, off-2, off-3, on-3; logs `results/ub-ro-basis-walk-memo-<label>.log`.

| label | MEDIAN engine_ms/1k | runs |
|---|---|---|
| off-1 | 2.77 | 2.831 2.882 2.754 2.771 2.728 |
| on-1 | 2.78 | 2.962 2.895 2.750 2.740 2.782 |
| on-2 | 2.87 | 2.896 2.872 2.951 2.777 2.753 |
| off-2 | 2.76 | 2.857 2.923 2.754 2.732 2.756 |
| off-3 | 2.75 | 2.886 2.829 2.729 2.729 2.746 |
| on-3 | 2.78 | 2.849 2.782 2.777 2.738 2.742 |

Off mean 2.760, on mean 2.810 (3 rounds each): +1.8% (on slower), entirely on-2 (2.87, runs 2.75-2.95); on-1 and
on-3 (2.78) sit 0.01-0.03 above their neighbours. Inside the 3-5% run-to-run spread, and a cost is not plausible: the
bench sends no reduce-only order, so the flag-on path differs only by one thread-local pin read + OnceLock load
per `execute_batch` and an empty (unallocated) HashMap.

## 5. End-to-end margin phase (release `ro_basis_memo_e2e_timing`, one `execute_batch`, serial Phase 2)

Log `results/ro-basis-walk-memo-e2e-timing.log` (7 fresh contexts per arm, arms alternate; each call = 1,024-order
RO batch + 64 single RO sells + one taker = 1,088 RO walks off vs 1 on; outputs asserted identical every run):

| R | levels | long | off margin ms (median) | on margin ms | delta ms | off raw | on raw |
|---|---|---|---|---|---|---|---|
| 100 | 1 | 0 | 6.723 | 0.564 | 6.159 | 6.93 6.72 6.72 6.66 6.69 6.81 6.75 | 0.58 0.56 0.59 0.58 0.56 0.56 0.56 |
| 1,000 | 1 | 0 | **91.916** | 0.686 | 91.230 | 100.23 88.97 89.53 101.95 91.92 88.97 100.06 | 0.68 0.69 0.69 0.71 0.70 0.67 0.67 |
| 1,000 | 50 | 0 | **76.610** | 0.660 | 75.951 | 75.36 77.63 76.30 76.61 79.12 75.30 77.23 | 0.66 0.67 0.67 0.67 0.64 0.65 0.66 |
| 900 | 50 | 40 | 68.682 | 0.768 | 67.915 | 68.11 70.97 72.17 67.12 68.49 68.68 72.30 | 0.74 0.74 0.76 0.78 0.78 0.78 0.77 |

The whole Phase-2 margin phase of the attack call drops 100-134x; at R = 1,000 (the base open limit, so every RO
order is then rejected at its slot AFTER the walk) the call costs ~92 ms of serial engine time today.

## 6. Verdict

**Built; premise confirmed; robustness fix, not a throughput gain.**
- Premise (kill line 20 ms per action at R = 1,000): 73-107 ms per 1,024-order RO call in the direct timing,
  77-92 ms of the call's margin phase end to end. Not killed. No output differs anywhere (300-round differential,
  e2e serial and sharded, golden flag on).
- Memo: 0.47-0.49 ms per call at R = 1,000 (155-218x), 1.03-1.05 ms at R = 5,000 (684-691x). The remaining floor is
  the per-RO-order `signed_position` read (~0.35 us each), a candidate for the same memo later (it carries R02
  fault semantics per read, so it was left alone).
- Bench: 0 by design (no RO orders); ubench ABBA off 2.77/2.76/2.75 vs on 2.78/2.87/2.78 (delta of means +1.8%, inside the 3-5% spread): noise.
- cell_worthy = false (session rule: no cells; and the target metric, attack-block engine time, is visible in the
  microbench, not in the CL21 cell, which sends no RO orders).
- Ship advice: as the write-up says, bundle with r1 #10 (match-time RO sweep) and r1 #5 / R16 (FOK pre-check) as
  the "RO amplifier" fix; this one alone removes the Phase-2 half. Production form: drop the flag and keep the memo
  arm (it is exact), keep `trader_orders_iter`.
