# exp archy-s58: settle-passb-deferred-results-scatter

Branch `exp/archy-s58-settle-passb-deferred-results-scatter`, worktree
`/home/oz/projects/wt/exp-s58-settle-passb-deferred-results-scatter`, based on
`exp/archy-s58-perf-search` = main `a44000e0`. Built and tested on ozarchy. No bench run yet.

On `exp/archy-s58-bundle-dmt` (s62 owner decision) this flag is ON by default: unset, `1` or `true` = on;
`0` or `false` = off; anything else logs a warning once and stays on. See
`docs/plans/exp-archy-s58-bundle-dmt.md`. The text below describes the flag branch (default off).

Flag: `TORUS_SETTLE_DEFER_RESULTS=1`. It is node-local and off by default. Unset (or any value other than a
trimmed `1`) runs today's code. The env var is read once per process (OnceLock). The first parallel settle
logs the resolved value once with `tracing::info!`. Tests can pin the flag per thread with
`with_settle_defer_results(on, || ...)`.

## What changed

All code changes are in `crates/torus-bridge/src/native_executor.rs`. Line numbers below are on the branch.

- **One value builder.** `NativeActionResult::placed_reject(Option<FailureReason>)` (:132, `#[inline]`) now
  holds the old body of `placed`: the match on the book's reject. `placed(&PlaceResult)` (:124) is reduced to
  `Self::placed_reject(result.reject)`. The inline write and the deferred write both build their value here.
- **Flag items** (:596-640), placed right after `settle_worker_cap()`:
  - `parse_settle_defer_results`: returns true only for a trimmed `1`.
  - the thread-local `SETTLE_DEFER_RESULTS_PIN`.
  - `#[doc(hidden)] pub fn with_settle_defer_results`: restores the previous pin through a Drop guard, so it
    is panic-safe.
  - `settle_defer_results_enabled`: uses the pin if one is set, otherwise the OnceLock env value.
- **Pass B of `settle_market_results_parallel`** (:8754). Nothing else changed.
  - After `pos_cache.reserve`, it sets `let defer = settle_defer_results_enabled();` and
    `deferred: Vec<Option<Option<FailureReason>>>`. With the flag on this is `vec![None; results.len()]`;
    with it off it is an empty Vec that never allocates. The Vec is a fresh local on every call, because
    `execute_batch` runs twice per block.
  - At the old `results[prep.index] = NativeActionResult::placed(result)` (:9080): with the flag on it
    records `deferred[prep.index] = Some(result.reject)`; with it off it runs the same statement as before.
    The fill-failure write stays inline before its `continue`, so a fill-failed index is never recorded.
  - After the market loop and before `settle_pass_b_ns += ...` (:9122), with the flag on, one ascending pass
    runs: `for (slot, d) in results.iter_mut().zip(deferred) { if let Some(reject) = d { *slot =
    placed_reject(reject) } }`. The pass has no metrics or atomics, uses only `usize` indices and no
    `unsafe`, and stays inside the pass-B timer.
- **Not changed:** the sequential settle (including its own `placed` write), the parallel path's fallback
  branch (it returns before pass B), and `NativeActionResult` itself.

## Why (evidence, verified on a44000e0)

Each matched order in pass B wrote its 56-byte `NativeActionResult` into the flat results Vec, built at
:7415 with one slot per flat action. About 90k slots per native block at 300m/600 s.

- **The writes are scattered.** Pass B walks markets in ascending order. The bench draws a market per order
  (`tools/bench-throughput` `MarketPlan::pick`). So at 300m, consecutive writes land about 300 slots apart.
- **The old slot misses on every write.** Each write runs the drop glue of the old slot: a load, then
  `cmp $-3` (vaddr 0x143af5d on d3cba5e2). The load misses every time.
- **Cost:** 3.6-4.0% of exec on-CPU samples (W1 p1/p2), or 2.8-3.1% on ef812b02 W2 cycles.
- **The miss is serialized.** It sits behind the lock-prefixed counter increments of the same iteration (the
  funnel metrics).

Pass B costs about 462-471 ns per order at 300m against 252-270 ns at 10m. The difference is stride
locality. The adversarial checks in memory (9c27ff84, d4ccb345) put the exposed stall at 85-106 ns per order.

With the flag on, the loop writes one byte per order into a ~90 KB buffer that stays in L2. The 5 MB results
array is then written once, front to back, which the prefetcher can follow.

**Why the result is identical.**

- Nothing reads `results` between its initialization and the end of settle. Pass B only writes, at the
  fill-failure site and the placed site, and those two writes exclude each other per order (`continue`).
- `stitch_outcome` pushes each flat index into `market_batches` exactly once.
- `placed()` depends only on `result.reject`.
- Pass B leaves an iteration only through `continue`. It has no early return.
- `FailureReason` is `Copy` and `repr(u8)` with 16 variants, so `Option<Option<FailureReason>>` is 1 byte.
  A unit test asserts this.

## Tests

**New differential test: `crates/torus-bridge/tests/settle_defer_results_tests.rs`.**

- The helpers are copied from `parallel_settle_tests.rs`, which was not edited.
- Fuzzed multi-market batches. The two seeds and the wide scatter test use 6, 3 and 24 markets with 200, 160
  and 600 storm orders; the flag-flip test uses 5 markets with 220.
- 24 senders are interleaved, and each order draws its own market.
- The order mix:
  - GTC, IOC, FOK and PostOnly.
  - IOC orders far from the book (IocCancel) and oversized FOK orders.
  - Dust and off-tick rejects.
  - STP self-trades.
- Each case runs with `record_fills` true and false, at pass-A caps 1, 2 and 8. Flag-off (pinned), flag-on
  (pinned) and plain (env) runs must be equal in:
  - every `NativeActionResult` field: action_type, success, error, gas_used, reason.
  - total_gas.
  - the CF dump: balances, positions, books, markets, trades, user trades.
  - `take_pending_fills_and_extras`, `trade_index`, `next_global_order_id`.
  - `compute_native_state_root`.
- Guards:
  - The parallel path must have run (`settle_pass_a_ns > 0`) with no fallback.
  - At least 10 placed book rejects, with BadAloPx and IocCancel among them.
  - At least 20 trades and at least 20 placed successes.
- `settle_defer_results_flag_flip_between_calls`: the flag flips between calls on one context. This checks
  that the buffer is per call.

**Inline unit tests** (`mod settle_defer_results_flag_tests`, :645):

- the parse table.
- the unpinned value follows the env.
- the pin overrides the env, nests, restores the previous pin after a panic and stays on its own thread.
- the record is 1 byte.
- `placed_reject(r)` equals `placed(&PlaceResult { reject: r, .. })` field by field, for None and all 16
  codes.

**Test-first record:**

1. Red: before the change, both test targets failed to compile on the missing API: E0432 for
   `with_settle_defer_results` and the flag items, E0599 for `placed_reject`.
2. With only the flag items and `placed_reject` added, all 8 new tests passed. This pins today's behavior.
3. The pass-B change kept them green.

**Mutation checks** (both reverted):

- Mutation 1: the materialize pass writes `placed_reject(None)`. All 4 differential tests fail ("deferred
  results diverged").
- Mutation 2: a fill-failed index is also recorded (`Some(None)`). Under `TORUS_SETTLE_DEFER_RESULTS=1`,
  three tests fail: `r02_settle_taker_fill_balance_read_fault_fail_stops`,
  `r02_settle_maker_pnl_balance_read_fault_fail_stops` and
  `r02_settle_maker_undecodable_balance_fail_stops_at_the_fill`. This shows the env reruns reach pass B and
  guard the fill-failure constraint.

**Runs** (nextest; each test runs in its own process, so the OnceLock reads the env):

- Flag unset, targeted (r02_settle, r02_parallel_settle, parallel_settle_tests, perf_equivalence_golden,
  and the new tests): 31/31 passed.
- `TORUS_SETTLE_DEFER_RESULTS=1`, the same targeted set: 31/31 passed. The goldens are unchanged, and the
  r02 pass-B faults keep their Fill rejection and the fail-stop latch.
- `rdeps(torus-bridge)` with the flag on: 1968/1968 passed (16 skipped).
- `rdeps(torus-bridge)` with the flag unset: 1968/1968 passed (16 skipped).
- `cargo nextest run --workspace` with the flag unset: 3821/3821 passed (38 skipped, 0 flaky). Doc tests:
  1 passed, 0 failed.

**Clippy and fmt:**

- Clippy: no warnings in the new file or on the touched lines. The only native_executor.rs warning is the
  pre-existing `sort_by_key` one in an unrelated function.
- rustfmt: no diff overlaps the touched hunks, and the new file is rustfmt-clean. The crate as a whole is
  not fmt-clean on main.

## Expected gain (adversarial-corrected, memory 9c27ff84)

These figures are for 300m at 600 s:

- pass B goes from ~41.5 ms to ~33-35 ms per native block (-6.5 to -9 ms), about -2.5% exec block_ms.
- engine ms per 1k fills: -2.7 to -3.8%.
- matched/s: +1.5 to +2.8%, central about +2%. This uses sec 67.4's ~+3.0% matched/s per -10.2% block_ms
  (300m is exec-bound).
- CPU-s per 1M matched: -0.4 to -0.5%.

Shape at the 120 s gate: -4 to -6.5 ms per native block.

At 10m, depth 1: about 0, because the bench tail sets the pace and 10m pass B is already stride-local
(252-270 ns per order). There is a small gain at depth 4.

The gain is specific to the bench shape (a random market per order). Batches clustered in one market gain
little, but do not regress: the materialize pass is a 1-byte linear scan plus the same writes.

## How to bench

The cells are A/B on one build. The flag goes through `EXTRA_ENV`; ambient `TORUS_*` variables are unset
first, and `EXTRA_ENV` is applied last.

```
tools/matched-bench/run-cell.sh <wt> s58-dr-off-300m 300 600
tools/matched-bench/run-cell.sh <wt> s58-dr-on-300m  300 600 76000 'TORUS_SETTLE_DEFER_RESULTS=1'
tools/matched-bench/run-cell.sh <wt> s58-dr-off-10m  10 120
tools/matched-bench/run-cell.sh <wt> s58-dr-on-10m   10 120 76000 'TORUS_SETTLE_DEFER_RESULTS=1'
```

- Use paired rounds, n=4. The CI is about ±1.3% matched/s (sec 67.3). Use the campaign driver in
  `tools/matched-bench/campaign/`.
- The mechanism metric is `settle_pass_b_ms` per native block. It appears in the exec phase log line
  (app.rs `settle_pass_b_ms`), as `exec_settle_pass_b_seconds`, and as `passB=` in summarize.py.
- Check the node log for the `TORUS_SETTLE_DEFER_RESULTS ... on=true` line.

Acceptance:

- AGREE holds in every cell.
- `settle_pass_b` ms per native block falls by at least 15%.
- The lower bound of the 300m matched/s paired CI is at least 0.
- No 10m regression.
- CPU-s/1M is not worse.

## Risks

- **Bench-shape dependence.** In production, flow clustered by market gives little gain. There is no
  regression path apart from a 1-byte-per-action buffer (~90 KB at 300m), its memset and one linear scan.
  That is well under 0.1 ms per block.
- **Merge friction.** `exec-pool-fine-grain-jobs` edits only pass A's `chunk_indices` call, so the hunks are
  disjoint. Phase 5 step 2 will rewrite pass B's merges and balance writes; the results write survives as a
  rebase.
- **Possible misread.** If the drop-glue miss moves into the materialize pass rather than disappearing, the
  pass-B timer shows it, because the pass sits inside the timer. The sequential pass is prefetch-friendly,
  so the miss should be hidden.
- **Consensus.** Nothing is consensus-visible. Results are the same values in the same slots, `results`
  feeds `CF_BLOCK_ACTION_STATUS`, which is not hashed (app.rs:3915 per memory d4ccb345; not re-checked
  here), and the native state root is unchanged (tested). A mixed fleet with
  and without the flag stays in AGREE by construction. run-cell forces one env digest across nodes, so
  mixed-fleet cells would need harness work.
