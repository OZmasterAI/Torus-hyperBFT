# exp/archy-s58-parallel-match-setup

Branch: `exp/archy-s58-parallel-match-setup` (base `exp/archy-s58-perf-search` = main `a44000e0`). Not pushed.
Flag: `TORUS_PARALLEL_MATCH_SETUP=1`. Node-local and default off. Only `1` (after trim) turns it on. It is read
once per process and logged once (`tracing::info!`, `enabled = ...`). Tests pin it per thread with
`with_parallel_match_setup(on, || ..)`.

## What changed

The Phase 3 setup now runs in each market's pool match job. Per market, the setup is the reduce-only positions its
book polices, the checked takers' `AccountMargins`, and the `Vec<MatchRequest>`. Today it runs serially on the exec
thread inside `match_timer`, before the pool starts. With the flag on, each market's pool match job builds these
inputs first and then matches the market.

`crates/torus-bridge/src/native_executor.rs`:

- Flag items after `parallel_engine_min_fills()` (:686-735): `parse_parallel_match_setup`, the thread-local
  `PARALLEL_MATCH_SETUP_PIN`, `#[doc(hidden)] pub fn with_parallel_match_setup` (restores the previous pin in a
  `Drop` guard, so also on unwind), and `parallel_match_setup_enabled()` (the pin, else a `OnceLock` over the env).
- `fn match_setup` (:10193). The old loop body (s515 comment through the `requests` collect, including the
  `cfg(test)` M1 shadow check) moved here verbatim and was dedented by 4. Edits inside the body:
  - `ctx.margin_configs` became `margin_configs`. This is the spec's one edit.
  - `&reader` (x2) and `&book` became `reader` and `book`. Both are already references in the new fn, so the `&`
    would be a `clippy::needless_borrow`. The code they produce is identical.
- The Phase 3 loop (:7798-7832):
  - `setup_in_jobs = parallel_match_setup_enabled() && market_batches.len() >= 2`.
  - Taking or creating the book (`take_book_for_matching`, `market_shape`, `reader.note_fault`) stays on the exec
    thread.
  - Flag off: `Self::match_setup(..)` and then `worker_batches.insert`. This is today's loop and today's
    `match_parallel_with`.
  - Flag on: `setup_items.push((market_id, book, prepared.as_slice(), prepared.len()))`.
- Pool call (:7845-7871). Flag on calls `MarketWorkerPool::match_parallel_setup_with(setup_items, ts,
  resolve_worker_cap_named(None), Some(&reader), true, &|m, book, prepared| Self::match_setup(&reader, &markets,
  &pools, &ctx.margin_configs, m, book, prepared))`. Flag off calls today's `match_parallel_with`. Both results feed
  the unchanged `match` and its fatal `Err` arm.

`crates/torus-bridge/src/market_workers.rs`:

- `pub(crate) fn match_parallel_setup_with<'a, W: Send, S>` (:205), added after `match_parallel_capped_with`.
  :115-195 are untouched. It mirrors the capped fn:
  - Empty input returns `Ok(vec![])`.
  - One item runs inline: `run_contained(setup + match_market)`.
  - Otherwise it uses `workers = max_workers.max(1).min(n)` and `assign_chunks` on the weights (`prepared.len()`,
    which equals today's `reqs.len()`, so the chunk layout is the same). Empty chunks are dropped. There is one
    `exec_pool::run_jobs` job per chunk. Each job runs its markets in order, each inside `run_contained`, and stops
    at its first `Err`.
  - A job that dies outside containment flattens to the same "worker thread died outside contained matching"
    representative as today.
  - A setup panic is therefore that market's `MarketWorkerPanic` and follows today's fatal path (`ctx.fatal_error`).
    It is never skipped or retried serially.

## Why (evidence)

- The d3cba5e2 W1 profile (`ozarchy-p4s0-run/analyst/p{1,2}-mperf-w1-exec-inline`) puts the setup at 4.1-4.6% of
  exec samples (822/17912 and 758/17829), about 8-8.8 ms per native block at 71.5k placed orders.
  - The parts: reduce-only `known.insert`, the `MatchRequest` collect, `insert_taker_only`, `AccountMargins::get`,
    `pools.get`.
  - The per-call timeline puts the setup at 39-40% of match wall time per call. The exec pool sits idle meanwhile.
- `TORUS_PARALLEL_ENGINE` does not touch this loop. `engine_threads` appears only in Phase 2 and in the settle gate.
- Every input is read-only during Phase 3:
  - `AccountReader` is `Sync` (it is the `MakerAccountSource` the match workers already share).
  - `markets`, `pools` and `margin_configs` are immutable maps.
  - The backend is frozen from the Phase 2 reader to the cache flush.
  - `ReduceOnlyPositions` and `AccountMargins` are only point-read or point-written, never iterated
    (order_book.rs:419-425, 655-681). So the thread that builds them cannot leak into any result.
- Reader faults stay first-wins in the context's `OnceLock` (log-only message) and still fail-stop the block.

## Tests

- `market_workers.rs` `setup_in_jobs_tests` (new):
  - (a) `match_parallel_setup_with` equals `match_parallel_capped_with` on the same pre-built batches for caps
    {1, 2, 3, 32} and for a single market. The comparison covers every result, `next_order_id` and the borsh
    bytes of each book.
  - The setup's book mutation (reduce-only positions) is in effect before matching: a qty 3 reduce-only sell is
    clamped to 2, and to 3 when it is not policed.
  - Empty input returns empty.
  - (b) A panicking setup becomes `Err(MarketWorkerPanic)` carrying market 3 and the payload, for caps
    {1, 2, 32} and for a single market.
- `native_executor.rs` `parallel_match_setup_flag_tests` (new):
  - Parse rules: only a trimmed `1` turns it on.
  - The pin overrides the env, nests, restores on return and on unwind, and is per-thread.
  - Non-vacuity: with the flag on, a 2-market block takes the in-jobs path (a `cfg(test)` thread-local counter at
    the call site). A 1-market block and the flag-off path never take it.
- `tests/parallel_match_setup_tests.rs` (new). It runs two fuzz seeds through 4 paths:
  `execute_batch_settle_workers(false/true)` and `execute_batch_engine_mode(0/4)`.
  - Each path runs three ways: flag pinned off, process env, and flag pinned on (3x and 2x reps).
  - Every `NativeActionResult` field must be equal, plus `total_gas`, the 7 native CFs, fills, fill extras,
    `trade_index`, `next_global_order_id` and the native state root.
  - The scenario covers:
    - resting reduce-only orders of holders who send nothing in batch 4 (their positions are read by the setup,
      not by Phase 2);
    - an oversize reduce-only order;
    - checked takers in their D2 pool market and taker-only in the others;
    - market orders, stop-market and stop-limit orders, cloids (with duplicates), FOK/IOC/PostOnly, dust and
      off-tick rejects;
    - tiered configs with oracle marks;
    - market 9 (row, no book), market 11 (no row) and market 12 (no book, single-market batch);
    - single-market batches.
  - Sanity asserts check that the scenario really fills every market and the holders' reduce-only orders.

### Results (archy, own target dir)

- New and touched targeted tests (`setup_in_jobs`, `parallel_match_setup*`, `worker_panic`, `capped_chunking`):
  20/20 passed.
- `TORUS_PARALLEL_MATCH_SETUP=1`, `binary(parallel_matching_tests | m1_correctness_tests | account_margin_tests |
  engine_parallel_tests | perf_equivalence_golden | r02_reader_faults_tests)`: 207/207 passed.
- `TORUS_PARALLEL_MATCH_SETUP=1`, `rdeps(torus-bridge)`: 1970/1970 passed, 16 skipped. No per-thread counter test
  changed.
- Flag unset, `--workspace`:
  - First run: 3822/3823 passed. The one failure was
    `torus-consensus crash_recovery_tests::checkpoint_crash_replay_with_cloids_matches_uncrashed_order_rows`
    ("crash at 61 latched"), a timing-based checkpoint crash test that took 11.1 s under full load. It then passed
    5/5 in isolation (4.8-4.9 s).
  - Rerun: 3823/3823 passed, 38 skipped, 0 flaky.
- Doc tests: rc=0, 1 passed, 0 failed.
- Clippy (`-p torus-bridge --all-targets`): 0 warnings in the changed code. The 6 remaining warnings are
  pre-existing in other lines and files.
- rustfmt: the crate is not rustfmt-clean at base. The changed files keep exactly base's diff count
  (market_workers 3, native_executor 170; the new test file has 0). The only remaining diffs in touched regions
  are base's own: the book-take block and the verbatim moved body.

### Microbench (indicative)

`ubench_econ` (release, defaults, 3 seeds x 6 blocks, about 11.3-11.6k fills per block):

| arm | match/1k fills | engine_ms/blk | median engine_ms/1k fills |
|---|---|---|---|
| env unset | 0.30 / 0.30 / 0.31 | 32.3 / 31.1 / 31.4 | 2.73 |
| `TORUS_PARALLEL_MATCH_SETUP=1` | 0.19 / 0.17 / 0.19 | 31.0 / 29.4 / 29.9 | 2.58 (-5.5%) |

The s92 sell-cut lines, the top-up counters (`full=375838 / 379397 / 381743`), `accepted` and `resting_end` are
identical in both arms.

## Expected gain (from the spec, not measured on the chain)

- 300m: match_ms -6.5 to -8 ms per native block at the 120 s shape, and -9 to -11.5 ms at 600 s. The pool span
  grows by about 0.5-1 ms.
  - Match ms per 1k placed: 0.33 down to about 0.22-0.24.
  - matched/s +2 to +2.8% (central about +2.5%). Native blk/s +3-5%.
  - CPU-s/1M about 0 (the work moves; it is not removed).
- 10m: depth 4 +2 to +3.5%, depth 1 about 0. A 1-market block keeps the inline path.

## How to bench it

- A/B on one binary: arm A has the env unset, arm B has `TORUS_PARALLEL_MATCH_SETUP=1`. `TORUS_PARALLEL_ENGINE`
  stays unset in both arms.
- Cells: 300m and 10m (depth 1 and 4), n=4 paired.
- Acceptance:
  - AGREE in every cell.
  - `phase_match_ms` per 1k placed down at least 20%.
  - The paired-CI lower bound of matched/s at 300m is at least 0.
  - No 10m regression, CPU-s/1M not worse, timeouts not up.
- run-cell forces one TORUS_* env digest across nodes, so a mixed-fleet AGREE cell (only some nodes with the flag)
  needs harness work. It is recommended, because the flag is node-local by design.
- Microbench (indicative): `cargo test --release -p torus-bridge --test ubench_econ -- --ignored --nocapture`, env
  unset, then `TORUS_PARALLEL_MATCH_SETUP=1`. match_ms per block should drop by about the setup share. The s92 and
  top-up counter lines must be identical.

## Risks

- Pool contention: the setup adds about 8-11 ms of work per block to the pool jobs. A hot market's chunk now
  carries its own setup too, so the critical chunk grows (estimated +0.5-1 ms).
- LPT weights stay `prepared.len()`. A market whose setup cost is dominated by resting reduce-only reads (not
  requests) is under-weighted. This is rare.
- Merge notes:
  - exec-pool-fine-grain-jobs: apply `grain_chunk_cap()` to this entry's `let workers = ...` line too.
  - The paused s123 cancel-all branch makes `run_contained` generic. This entry's calls still compile.
  - Phase 5 step 3 changes what match workers return (dirty sets) and the epilogue. Coordinate with 18c.
  - sell-top-up-stitch-list touches different hunks of `execute_batch_phases`.
- Per-thread test counters: any test that counts reader work on the calling thread would see the setup's reads move
  to the pool threads with the flag on. None failed (see the env reruns).
