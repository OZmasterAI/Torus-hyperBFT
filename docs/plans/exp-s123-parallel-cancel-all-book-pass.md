# parallel-cancel-all-book-pass (s123, rank 1)

Branch `exp/s123-parallel-cancel-all-book-pass` in worktree
`/home/18c/projects/Torus-hyperBFT/.claude/worktrees/wf_443f7b55-fcb-29`, based on main `c612dc7f`.
Patch: `/home/18c/projects/wt/exp-s123/parallel-cancel-all-book-pass.patch` (staged diff, not committed).
Not compiled and not tested: no cargo of any kind, no LSP, per the run rules.

## What changed

Flag: `TORUS_PARALLEL_CANCEL_ALL=N`, default off. Unset, `0`, `1` or garbage give 1, which is the serial loop.
N>=2 means N jobs, capped at 64. Gate: `TORUS_PARALLEL_CANCEL_ALL_MIN_VISITS`, default 64 (market, action)
visits per run.

1. `crates/torus-bridge/src/market_workers.rs:319`: `run_contained` is now
   `pub(crate) fn run_contained<R>(market_id, f: impl FnOnce() -> R) -> Result<R, MarketWorkerPanic>`. Its callers
   (:129, :302) and its unit test (:398, where R falls back to `()`) are unchanged. Nothing else in the file changed.
2. `native_executor.rs:4360-4452`: the env functions, which read the env once each (OnceLock), copied from the
   save_books pattern. Pure parses `parse_parallel_cancel_all_workers` and `parse_parallel_cancel_all_min_visits`,
   with unit tests in `mod parallel_cancel_all_toggle_tests` (:4415).
3. `NativeExecContext` (:4803-4812): adds `pub cancel_all_workers` and `pub cancel_all_min_visits`, set from the env
   at :5356. Test-only fields (:4857-4866): `test_cancel_all_panic_market: Option<MarketId>` (planted panic) and
   `test_cancel_all_parallel_runs: u64` (counts runs that used the pool, so the tests can prove the pool ran).
4. `exec_cancel_all_run` (:11080) returns `Result<Vec<NativeActionResult>, Vec<NativeActionResult>>`. Serial and
   parallel modes share one code path:
   - `cancel_all_group` (:11197) holds the old per-group body: per member in run order, `take_pending_stops` plus
     `stop_reservation`, then one `cancel_all_many`. It returns `(k, cancelled_orders_margin(cfg, &orders), stops)`
     for each member that removed something. The margin is computed here (a pure i128 sum over this market's
     orders), so `Vec<Order>` is freed inside the job.
   - `hits` is now `(k, mid, margin, stops)`. The per-action loop is unchanged in substance: same counters, same
     dirty marks, same `+= margin; += stops` order in ascending market order, same `release_order_margin`. Each
     per-market term is exactly the sum the old loop computed, added in the same order, so no value is reassociated.
5. Parallel path, used only when `cancel_all_workers >= 2 && groups.len() >= 2 && visits.len() >= min_visits`,
   in `cancel_all_groups_parallel` (:11243). It is on the resident (P2-1) branch only:
   - One `ctx.order_books.iter_mut()` pass collects disjoint `&mut OrderBook` for the groups' markets (binary search
     over the sorted group mids), sorted by mid and merge-walked with the groups. A group whose market has no book is
     skipped, as the serial loop's `continue` does. `&ctx.margin_configs` is borrowed as a separate field. No ctx
     method is called while the books are lent, no book leaves the map, and there is no unsafe.
   - The pass is split with `MarketWorkerPool::chunk_indices`, weight = members + 4. Each chunk becomes one
     `exec_pool::run_jobs` job. A job walks its markets in ascending order, runs each through `run_contained`, and
     stops at the first panic. A job-level `Err` maps to the chunk's first market.
   - Results are reassembled by item index: visited counts are added per item and hits are pushed in market order,
     then the stable `sort_by_key(|h| h.0)` runs as before.
6. Fail-stop:
   - `cancel_all_worker_failed` (:11369) logs `tracing::error!` and latches `fatal_error` only if it is `None`:
     "cancel-all worker panicked (market {id}): {msg}".
   - It skips the margin loop and returns `Err` with every run action set to `NativeActionResult::err("cancel_all", ..)`.
   - The phase-1 caller (:7419) writes those results and returns `NativeBatchResult` at once: no cloid pre-pass, no
     Phase 2+, mirroring the match-panic exit. The early return is keyed on the `Err`, not on `fatal_error`.
   - One addition beyond the brief: the books a panic may have left half-done (the panicking market, or every
     market of a job that died outside containment) are removed from `ctx.order_books`, as a panicking match worker
     consumes its book. Reason: app.rs:3736-3737 runs the post-EVM `execute_batch` on the same ctx BEFORE the
     committer's `take_fatal_error` check (:3745). A half-cancelled book could otherwise panic the `torus-execution`
     thread there (exit 70 via the hook at torus-node main.rs:1776-1810) instead of reaching the graceful
     `exec_failed` latch. Both indexes already tolerate a missing book: cancel-all `get_mut .. else continue`, and
     `CloidMarkets::live` uses `books.get(&market)?`. Phase 2 recreates a missing book (:7752).
7. Left out, as briefed: the non-resident `exec_cancel_all_run_every_book`, the full-scan reference, the
   single-action `exec_cancel_all`, the by-id cancels, liquidation cancels, any torus-core edit, metrics in
   app.rs/telemetry, exec_pool.rs, and default-on.

## Tests written first (in `cancel_batch_exec_tests.rs`, not run)

- Helpers:
  - `run_observed_par` (:252) is `run_observed_in` plus `par: Option<(workers, min_visits)>`; `run_observed_in`
    now calls it with `None`.
  - `run_par` (:1514) pins workers and sets min_visits = 0, because MARKETS has only 4 books. It records the
    cumulative (cancel_alls, visited, hit) after every block and checks the cloid index after every block.
  - `long_emptying_run_blocks` (:364) holds the long run's blocks; the existing test now uses it.
- (a) `parallel_cancel_all_matches_serial_mixed_blocks`: seeds/modes of the flag-on/off test plus OrderRows,
  workers 2, 3, 4 and 64 against workers 1. Asserts full `RunFingerprint` equality, equal counters, the serial arm
  never used the pool, and each parallel arm did.
- (b) `parallel_cancel_all_matches_serial_long_run_emptying_books`: Classic and LevelAuthorityChunked, exactly
  one parallel run.
- (c) `parallel_cancel_all_of_stop_only_senders_matches_serial`: stop-only senders across 3 or 4 books, all 4
  modes. The existing stop-only test uses market 1 only, so it never reaches the parallel path.
- (d) Counters are compared per block in (a), (b), (c) and (e). There is also a direct port,
  `parallel_cancel_all_counts_visited_and_hit_books` (visited 3, hit 3, dirty [1,2,3]).
- (e) The trader index is checked after every block (`assert_index_covers_books` in the run loop). The new
  `assert_cloid_index_covers_books` checks the cloid index. `parallel_cancel_all_then_cloid_reuse_matches_serial`
  covers cloids freed by a parallel run and reused in the same block (80 reused, 16 duplicates rejected).
  - Deviation: the brief asked for "carried == rebuilt". That does not hold by design. The cancel-all index is a
    superset (see the existing `cancel_all_index_rebuilt_at_load_equals_the_carried_one`), and cloid removes may
    wait in the book logs (comment at :3243-3246). So the tests assert "covers", the invariant the code
    guarantees.
- (f) `parallel_cancel_all_worker_panic_fail_stops_before_phase_2`, for Classic and LevelAuthorityChunked:
  - The planted panic at market 3 sets fatal_error naming market 3 and the payload. The 10 cancel-alls are
    errors. `trade_index` and `next_global_order_id` are unchanged, and no order id >= the old counter rests, so
    Phase 2 did not run.
  - Book 3 is gone and the others stay. The test thread does not unwind (catch_unwind wrapper).
  - A second batch on the fatal ctx runs without a panic and keeps the latch.
  - The same block with min_visits = usize::MAX runs clean on the serial loop, and the pool counter stays 0.
  - A ctx carrying a preexisting fault still stops at the panic and keeps its first fault, which proves the return
    is keyed on the Err.
- (g) The parse tests in `native_executor.rs` (:4415).

## Why (evidence)

- At 300 markets exec is the ceiling: exec thread busy 0.965, block_ms 206.6 (perf doc sec 67.3 table,
  `docs/perf/ozarchy-antispam-item6-pf1-2026-10-04.md:9329-9330`).
- `phase1_actions` = 25.98 ms per native block (sec 68 table, :9426). Per memory 686c9ac3 that is the main-equivalent
  arm (Phase 4 step 5 `6288ebe8` is not on main).
- Sec 25.1 (:2644-2658, measured on `707f132f`): cancel-all inclusive 30.0 ms per native block. Per-book work is
  apply 10.3 + locate 9.7 + trader_orders probe 3.5 + store 2.0 + sort/push 1.2, about 27 ms. Margin/hit/release
  is 1.5 ms and the results loop 0.9 ms. P2-1 has since removed most of the empty visits.
- Sec 30 (:3155-3167): about 59 cancel-alls per block, 189 books visited per cancel-all. That is ~11k visits over
  up to 300 books per run, so ~37 members per group, enough work per job.
- Exec pool usage: only matching (market_workers.rs:183) and settle pass A (native_executor.rs `run_jobs` call)
  use it. Both run later on the same exec thread, so the pool is idle in phase 1 (read in this session).
- The brief said the order_book module has no statics. Correction: `order_book.rs:1319` has `NEXT_BOOK_INSTANCE`
  (AtomicU64). It is used only at book creation and clone (node-local log marks) and `cancel_all_many` does not use
  it, so determinism is unaffected.

## Expected gain (inferred, not measured)

- phase1_actions ~26 -> ~8-12 ms per native block at N=8. The serial part left is the index take, the visits sort,
  the per-action release loop, and the other Phase 1 actions (by-id cancels, modifies).
- That is ~13-19 ms off block_ms (-6 to -9%). Using the sec 67.4 conversion (block_ms -10% -> native blk/s +9.5%,
  matched/s +3%), expect native blk/s +6-8% and matched/s +2-4%.
- This is specific to the bench shape: every bench cancel is `CancelAllOrders{None}`.
- At 10 markets, expect little or nothing: few groups per run, and exec is not the limit there (busy 0.77-0.80).

## How to measure

- 300 markets, ABBA n=4. Arm A: env unset. Arm B: `TORUS_PARALLEL_CANCEL_ALL=8`. Same binary for both.
- Primary metric: `phase1_actions` ms per native block.
- Also: block_ms per native block, native blk/s, matched/s, CPU-s/1M fills, commit-interval p50/p99, and consensus
  timeouts per cell. Require AGREE on every cell (block_hash + state digest).
- Optional sweep: N in {4, 8, 16} and `TORUS_PARALLEL_CANCEL_ALL_MIN_VISITS` in {64, 512}.
- Re-profile phase 1 on head first: sec 25.1's per-book share was measured before P2-1, Phase 3 and Phase 4.
- Before benching, run the tests: `cargo nextest run -p torus-bridge -E 'test(parallel_cancel_all) | test(cancel_batch) | test(cancel_all) | test(worker_panic)'`
  plus a full `rdeps(torus-bridge)` run.

## Risks

- Determinism: each book is touched only by its own group, with the same member order and the same calls as the
  serial loop. The margin terms are pure. Results are merged in market order. The one remaining risk is a hidden
  cross-book shared mutable state inside OrderBook. I found none: `OrderBook` is already `Send` and moved across
  threads by matching, and the tiers are `Arc<[MarginTier]>`, read-only. The A/B must still show AGREE.
- Panics in the margin math (e.g. `margin_at_integer_leverage` with leverage 0) used to run after all book work on
  the exec thread. Serial mode now runs them per market, so the same panic may be hit at a different point. The
  outcome class is the same (exec-thread panic, then fail-stop exit 70). In parallel mode they are contained and
  become `fatal_error`.
- Pool size: the exec pool is sized at first use from host / TORUS_MATCH_WORKERS / TORUS_SETTLE_WORKERS (exec_pool.rs
  :19-35). N above that only queues jobs (same results, less speedup). exec_pool.rs was out of scope.
- CPU contention: the jobs compete with the end_resident worker, the flush worker and checkpoint threads for
  cores, so the gain may be below linear. Watch CPU-s/1M and the p99 numbers.
- Memory: cancelled `Vec<Order>` is now freed on pool threads (cross-thread frees under jemalloc). This is
  expected to be harmless, but watch RSS.
- COORDINATE with item 6 Phase 5 step 3 (plan `docs/item6-phase5-plan:docs/plans/item6-phase5-impl.md` step 3).
  If its cancel-site dirty marks live in ctx rather than in the book, `cancel_all_group` must return each market's
  marks, and the caller must merge them in market order after the jobs. Book-local marks need no change.

## Untested / confidence

- Nothing was compiled or run.
- Compile confidence: medium-high. The borrows follow existing patterns: disjoint `iter_mut` books, as at
  save_order_books :6548. Field-disjoint ctx borrows are used as in the old loop. Pool jobs borrow non-'static data,
  as `jobs_borrow_caller_data` in exec_pool.rs does. `#[cfg(test)] if` statements are used as at :1788.
- Spots most likely to need a touch-up:
  - the deref coercion `mids.binary_search(m)` with `m: &&MarketId`;
  - closure type inference in `jobs.push(move || ..)`;
  - the `()` fallback for R in market_workers' existing `run_contained(7, || panic!(..))` test (edition 2021).
- Test-side uncertainty: (e) assumes a cloid re-place after a cancel-all in the same block succeeds on the serial
  path. That follows from the cloid pre-pass running after Phase 1 (:7448-7460), but has not been run.
