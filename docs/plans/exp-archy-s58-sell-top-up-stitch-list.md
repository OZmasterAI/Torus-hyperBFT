# exp archy-s58: sell-top-up-stitch-list

Branch `exp/archy-s58-sell-top-up-stitch-list`, worktree
`/home/oz/projects/wt/exp-s58-sell-top-up-stitch-list`, based on
`exp/archy-s58-perf-search` = main `a44000e0`. Built and tested on ozarchy. No bench run yet.

Flag: `TORUS_FAST_SELL_TOP_UPS=1`. It is node-local and off by default. Unset (or any value other than a
trimmed `1`) runs today's code: nothing is recorded, and today's `sell_top_ups` call and the per-order s92
counter loop run. The env var is read once per process (OnceLock) and logged once with `tracing::info!`.
Each `execute_batch` call reads the flag once (`fast_top_ups`). Tests pin it per thread with
`with_fast_sell_top_ups(on, || ...)`.

## What changed

All code changes are in `crates/torus-bridge/src/native_executor.rs`. Line numbers are on the branch.

- **Flag items** (:715-818, right after `struct PrepPass`):
  - `parse_fast_sell_top_ups`: true only for a trimmed `1`.
  - the thread-local `FAST_SELL_TOP_UPS_PIN`, and `#[doc(hidden)] pub fn with_fast_sell_top_ups`, which
    restores the previous pin through a Drop guard (panic-safe).
  - `fast_sell_top_ups_enabled`: the pin if set, else the OnceLock env value.
  - `struct TopUpCand { index, market, k, sender, reserved, res_qty }` and `TopUpCand::record`, which pushes
    one stitched candidate when the list is on. `res_qty` follows `prepare_one`'s basis rule
    (`basis.get(&i).map_or(params.quantity, |b| b.1)`); `reserved` is `pass.required`, which equals
    `margin_reserved` at the push.
  - a cfg(test) counter `FAST_TOP_UP_PATHS` (calls of the two fast paths on this thread).
- **`stitch_outcome`** (:8331) returns `Option<(usize, FixedPoint)>`: `Some((k, pass.required))` for a pass
  with `top_up_candidate`, where `k` is the market Vec's length before the push; `None` otherwise. The two
  test callers ignore it (Option is not `must_use`).
- **`execute_batch_phases`**:
  - :7613: `let fast_top_ups = fast_sell_top_ups_enabled();` and `top_up_cands: Option<Vec<TopUpCand>>`
    (`Vec::with_capacity(place_order_indices.len())` when on, `None` when off). One list per call; a block
    makes two calls (pre-EVM and post-EVM).
  - Both production stitch loops (sharded stitch and serial loop) call `TopUpCand::record` after
    `stitch_outcome`. Both walk `place_order_indices` ascending, so the list is in flat order, which is the
    scan's sort.
  - :7693: `match top_up_cands { Some(c) => sell_top_ups_fast(..), None => <today's sell_top_ups call> }`.
  - :7919 (part b): with the flag on, `record_margin_cuts_batched`; off, the `record_margin_cuts` loop,
    unchanged.
- **`record_margin_cuts_batched`** (:9307, new): zips results and prepared orders like the loop, sums
  `margin_cancels.len()` and `reduce_only_cuts.len()` into two u64s (one `inc_by` each per market), and reads
  the cold `p.params.is_buy` only when `margin_cut_price` is Some. It uses the same tick-bucket math and the
  same `sell_margin_cuts[non_pool][filled][bucket].inc()` as `record_margin_cuts`. That function is unchanged.
- **`sell_top_ups_fast`** (:10118, new, right after `sell_top_ups`; the name keeps the `sell_top_ups` prefix
  for `storage_reads_tests.rs`' frame classifier):
  - `debug_assert` that the list is strictly ascending by index.
  - A per-market memo of `(top-up price, cfg)`; `None` means skip, like today's two `continue`s.
  - Per candidate, in today's order: `get_mut(market).get_mut(k)` (address only, no load from the prepared
    order) then `continue` if missing; `try_reserve_for_qty_cfg(cfg, price, res_qty)` then `continue` on Err;
    `extra = target - reserved` then `continue` if `extra <= 0`.
  - A per-sender run (consecutive candidates of one sender): the balance is loaded lazily with
    `bal_cache.load` at the run's first `extra > 0`, together with `pos_net` and `excess`. A load error calls
    `note_reader_fault(fault, "sell top-up balance read", &e)` and skips the candidate; the next candidate
    retries. At a sender change and after the last candidate, `bal_cache.set` runs once, and only if the run
    granted.
  - `grant = extra.min(bal.available + pos_net - excess)`, per candidate, with the same FixedPoint ops and
    no running value. `grant <= 0` counts `none`. Otherwise `available -= grant`, `order_margin += grant`,
    `p.margin_reserved = reserved + grant` (a store), and `res_price = price` on a full grant.

## Why (evidence, verified on a44000e0)

`sell_top_ups` re-scans every `PreparedOrder` of every market (176 B each, ~90k per block at 300m) for
`top_up_candidate`, sorts the hits by flat index, then per candidate does `markets.get` and the i128 price
math, a cold load of the prepared order through `p.params` and `p.params.quantity`, `try_reserve_for_qty_cfg`
and four sender-keyed probes (`bal_cache.load`, `pools.get`, `excess.get`, `bal_cache.set`).

- d3cba5e2 W1 inline profiles: 1029/17912 (5.74%) and 973/17829 (5.46%) exec samples contain `sell_top_ups`,
  about 10.8 ms per native block at 35.1k top-ups (307 ns per top-up). Split of p1 samples: the `p.params`
  load 32%, scan and collect 11%, sort 10%, `bal_cache` load and set plus `pools.get` 20%,
  `params.quantity` 7%, `try_reserve` 7% (memory 468972c6).
- Main (hsdb r2-B): 44.4k top-ups per native block, all full, so about 14 ms per block.
- Part b: the s92 loop loads `p.params.is_buy` (cold) and does two counter atomics per order, about 4-5 ms per
  block in `engine_untimed`.

**Why the result is identical.**

- Grants stay in global flat order: both stitch loops walk `place_order_indices` ascending, and that list
  is strictly ascending (the cloid `retain` keeps the order).
- Nothing mutates `market_batches` between the stitch and the top-up call; the map is local to the call.
- `pools` and `excess` are read-only and keyed by sender.
- `BalanceCache::set` marks an entry dirty once and pushes it to the dirty list once, and `flush_all` sorts
  the list. A run is one sender's consecutive candidates, so no other sender's set falls between a run's
  first grant and its end. One set per granted run therefore gives the same rows, flags and even the same
  unsorted dirty list as one set per grant. A run without a grant never sets, so a clean entry stays clean
  (an extra dirty entry would be an extra `CF_NATIVE_BALANCES` put).
- The lazy load reads the same senders as today (the first `extra > 0` of each run), with the same label on
  a fault, and caches nothing on an error.

## Tests

**In-crate module `crates/torus-bridge/src/sell_top_up_stitch_tests.rs`** (cfg(test) `#[path]` module next
to the flag items):

- `fast_top_ups_equal_the_scan_on_random_batches`: 600 random hand-built rounds over a failing backend
  (`FailBal`: a sender's balance reads fail once, twice or always). The rounds cover interleaved senders and
  senders with non-adjacent runs; tight funding (full, partial, none); runs whose first candidate needs
  nothing; Market, IOC and FOK sells with basis overrides (including quantity 0); markets without a bid,
  unlisted, or whose top-up price or notional overflows; clean and dirty pre-cached senders; transient and
  permanent faults. `sell_top_ups` and `sell_top_ups_fast` must give identical `margin_reserved` and
  `res_price` per order, counts, cache rows and dirty flags, the unsorted dirty list, the first latched fault
  and the number of failed reads. Non-vacuity floors are asserted for each case.
- `fast_top_ups_non_adjacent_runs_and_last_run` and `fast_top_ups_retry_after_a_transient_read_fault`: small
  fixed cases (A, B, A, B runs; a transient fault then a grant).
- `stitch_records_the_scan_candidates_on_real_phase2_runs`: 80 real Phase-2 runs (serial fold and 2-worker
  sharded prepare). The list built from `stitch_outcome`'s returns equals the one rebuilt from the batches,
  the top-ups from it equal the scan's on the fold's own cache, pools and excess, and a `None` list records
  nothing.
- `batched_margin_cut_counters_equal_record_margin_cuts` (part b): 300 random rounds of book outcomes (maker
  margin cancels, reduce-only cuts, buy and sell cuts at every tick distance, zero ticks, prepared lists
  shorter than the results). The batched loop leaves every counter of a fresh `Metrics` where the per-order
  loop leaves it.
- `fast_sell_top_ups_toggle_parse`, `fast_sell_top_ups_pin_nests_and_restores`.
- `execute_batch_routes_top_ups_by_the_flag`: pinned off, neither fast path runs; pinned on, both run; the
  state roots, the full top-up count and `phase_accum.sell_top_ups` are equal.

**Block-level differential `crates/torus-bridge/tests/sell_top_up_stitch_diff_tests.rs`:**

- `fast_top_ups_identical_on_fuzzed_blocks`: 10 fuzzed scenarios. Six markets with ladders (two marked);
  24 senders; two storms (two `execute_batch` calls). A third of the senders are funded to land between 0
  and 1.5 top-ups per sell after their Phase-2 fold, so partial grants occur. Orders are interleaved as runs,
  PlaceOrderBatch and CancelAllOrders.
- `fast_top_ups_identical_on_b_blind_shapes`: the `b_blind_shapes_identical` batches from
  `engine_parallel_tests.rs`.
- Each runs `execute_batch_settle_workers` (sequential and parallel settle) and `execute_batch_engine_mode(4)`
  (the sharded stitch), pinned off vs pinned on. The CF dump, results, gas, trade index, next order id, state
  root, every top-up and margin-cut counter and `phase_accum.sell_top_ups` must be identical, and each mode
  must equal the serial one.

**Test-first record:**

1. Red: before the change, both targets failed to compile only on the missing API (E0425/E0433 for
   `TopUpCand`, the flag functions and `FAST_TOP_UP_PATHS`; E0599 for `sell_top_ups_fast` and
   `record_margin_cuts_batched`; E0432 for `with_fast_sell_top_ups`).
2. Green after the change. On the first green run every equality held; only two non-vacuity floors were
   tuned (a transient fault still latches the fault; partial grants need cent-level funding).
3. Mutation check (reverted): making the run-end `bal_cache.set` unconditional fails
   `fast_top_ups_equal_the_scan_on_random_batches` (round 0) and
   `fast_top_ups_non_adjacent_runs_and_last_run`, on the dirty flags.

**Runs** (nextest; each test runs in its own process, so the OnceLock reads the env):

- New tests plus `r02_sell_top_up_*`, flag unset: 12/12 passed.
- `TORUS_FAST_SELL_TOP_UPS=1`, `account_margin_tests`, `engine_parallel_tests`, `perf_equivalence_golden`,
  `storage_reads_tests`: 180/180 passed.
- `rdeps(torus-bridge)` with the flag on: 1969/1969 passed (16 skipped).
- `rdeps(torus-bridge)` with the flag unset: 1969/1969 passed (16 skipped).
- `cargo nextest run --workspace` with the flag unset: 3822/3822 passed (38 skipped, 0 flaky). Doc tests:
  1 passed, 0 failed.

The `r02_sell_top_up_*` tests call `sell_top_ups` directly, so the in-crate module is what covers the fast
path's fault handling.

**Clippy and fmt:**

- Clippy (`-p torus-bridge --tests`): no warnings in the new files or on the touched lines. The only
  native_executor.rs warning is the pre-existing `sort_by_key` one in an unrelated function.
- rustfmt: the new files are rustfmt-clean, and no rustfmt diff falls on new code in native_executor.rs (170
  diff hunks on main, 169 on the branch; the crate is not fmt-clean on main).

## Expected gain

- Part a: margin phase -4 to -8 ms per native block at the 120 s gate shape (central about -6), -5 to -11 ms at
  600 s (central about -8). This is the certain ~4-5.3 ms of scan, sort and probes, plus 0-4 ms from the
  cold load, depending on how much of the store's RFO is hidden.
- Part b: `engine_untimed` -2 to -4 ms per native block.
- matched/s +1.5 to +3% at 300m (central about +2.3%), using sec 67.4's ~+3.0% matched/s per -10.2% exec
  block_ms (300m is exec-bound).
- CPU-s/1M matched -0.3 to -0.8%.
- 10m: depth 1 about 0; depth 4 +1 to +2%.
- Today the scan warms PreparedOrder lines for the Phase 3 walk, so part of the saving may reappear in
  `match_ms`.

## How to bench

A/B on one build. The flag goes through `EXTRA_ENV`.

```
tools/matched-bench/run-cell.sh <wt> s58-stu-off-300m 300 600
tools/matched-bench/run-cell.sh <wt> s58-stu-on-300m  300 600 76000 'TORUS_FAST_SELL_TOP_UPS=1'
tools/matched-bench/run-cell.sh <wt> s58-stu-off-10m  10 120
tools/matched-bench/run-cell.sh <wt> s58-stu-on-10m   10 120 76000 'TORUS_FAST_SELL_TOP_UPS=1'
```

- Paired rounds, n=4 (CI about +-1.3% matched/s, sec 67.3), through the campaign driver in
  `tools/matched-bench/campaign/`. Include the 120 s gate shape at 300m for the margin_ms criterion.
- Mechanism metrics: `phase_margin` ms per native block (summarize.py `margin=`, `exec_phase_margin_seconds`)
  and `engine_untimed` (part b). Watch `phase_match` for the warming effect.
- The `torus_sell_top_ups_{full,partial,none}_total`, `torus_sell_cuts_*`, `torus_maker_margin_cancels_total`
  and `torus_reduce_only_cuts_total` counters must match between arms for the same load (they are
  telemetry, so they only match up to load differences between runs).
- Check the node log for the `TORUS_FAST_SELL_TOP_UPS (s58)` line with `enabled=true`.
- Microbench (optional): `cargo test --release -p torus-bridge --test ubench_econ -- --ignored --nocapture`
  twice, unset then `TORUS_FAST_SELL_TOP_UPS=1`. `margin/1k` must drop and the `UB s92` line must be
  identical. Not run for this commit: /home was at 18-20 GB free (under the 25 GB floor) with other s58
  builders running, and this worktree's target dir has no release artifacts, so the run needed a full
  release build.

Acceptance:

- AGREE in every cell.
- `phase_margin` per native block down at least 4 ms at the 120 s gate shape; `engine_untimed` down.
- `phase_match` not up beyond noise.
- The lower bound of the 300m matched/s paired CI is at least 0.
- No 10m regression; CPU-s/1M not worse.

## Risks

- **Store RFO.** The grant still stores to the cold prepared order, so part of the old load cost stays as an
  RFO; the low end of the range assumes none of it is hidden.
- **Warming shift.** The scan warmed PreparedOrder lines for the Phase 3 walk; part of the saving may show up
  in `match_ms` instead.
- **Allocation.** The list is sized to the call's PlaceOrders (~90k x 80 B = ~7 MB at 300m, about half
  used). A much smaller estimate would regrow it.
- **Bench AGREE cannot validate against main.** All nodes of a cell run the same arm, and main's 300m load
  has no partial or none top-ups, so the differential tests are the guard.
- **Merge friction.** `parallel-match-setup` edits :7612-7731 on main and adds a fn before :10039; this
  branch edits the Phase 2 stitch and the top-up call site, the s92 loop and `stitch_outcome`, and adds a fn
  after `sell_top_ups`. The hunks are separate but close; expect a trivial rebase. Phase 5 step 3 also edits
  the epilogue around the s92 loop.
- **Phase 5 step 2** replaces `BalanceCache` with `UsersMut`; the run cache must be ported then (the
  stitch list survives).
- **Consensus.** Nothing consensus-visible changes: the same reservations, balances and dirty set (tested
  byte-identical, state root included). A mixed fleet stays in AGREE by construction.
