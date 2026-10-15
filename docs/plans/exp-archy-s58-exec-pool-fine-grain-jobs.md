# exp/archy-s58-exec-pool-fine-grain-jobs

Worktree: /home/oz/projects/wt/exp-s58-exec-pool-fine-grain-jobs
Branch: exp/archy-s58-exec-pool-fine-grain-jobs, off exp/archy-s58-perf-search (= main a44000e0). Not pushed.
Flag: `TORUS_EXEC_POOL_GRAIN=K` (K in 2..=16; unset, garbage, 0 or 1 = today). It is node-local, read once per process (OnceLock) and logged once. Default off.

## What changed

Two files of production code, one line at each of the two exec-pool call sites, plus the flag items.

- `crates/torus-bridge/src/market_workers.rs`
  - New module-level items after `impl MarketWorkerPool` (:388-474):
    - `EXEC_POOL_GRAIN_MAX = 16` and `clamp_exec_pool_grain` (below 2 gives 1, above 16 gives 16).
    - `parse_exec_pool_grain(Option<String>) -> usize` (:416). It parses a trimmed integer. Garbage, 0 and 1 give 1. Above 16, or too large for `usize`, gives 16.
    - The thread-local `EXEC_POOL_GRAIN_PIN: Cell<Option<usize>>`.
    - `#[doc(hidden)] pub fn with_exec_pool_grain(k, f)` (:435). It pins this thread's K (clamped) and restores the previous pin in a Drop guard, so the pin also comes back on unwind. This is the test seam: env vars race across test threads.
    - `exec_pool_grain()` (:448): the pin if set, else the OnceLock over `TORUS_EXEC_POOL_GRAIN` (logged once at info).
    - `pub(crate) fn grain_chunk_cap(cap)` (:467). It returns `cap` when K <= 1 or cap < 2, else `cap.saturating_mul(K)`.
  - Matching, `match_parallel_capped_with` (:147): `let workers = grain_chunk_cap(max_workers.max(1)).min(n);`. This is the only edit in that function.
- `crates/torus-bridge/src/native_executor.rs`, settle pass A (:8672-8677): `chunk_indices(&weights, grain_chunk_cap(max_workers))`.
- Unchanged:
  - `chunk_indices` and `assign_chunks`. They also size `drain_books_parallel` (modes 2/3) and the boot book load, which do not use the grain.
  - The pool size, the rayon config and `run_jobs`.
  - Pass A's cap-1 inline path, so `TORUS_SETTLE_WORKERS=1` stays serial.
  - No runner on the calling thread (no `in_place_scope`), and no sub-flag for one.
- How the extra chunks get picked up: each chunk is still one `run_jobs` job, a rayon scope spawn. A scope job sits in the spawning worker's deque and can be stolen until it starts. So a thread that finishes its chunk early takes a chunk that a late-starting or descheduled thread has not begun.

### Why state stays identical

- Matching:
  - Each market's matching is pure in its own book and requests.
  - Results are sorted by market id before settlement (native_executor.rs:7771).
  - The per-market panic containment is unchanged. A panic is still a typed `MarketWorkerPanic`, and it is fatal.
- Pass A:
  - `compute_market_settle_plan` is pure, and plans are scattered back by input index (:8707-8717).
  - A failed or missing plan still triggers the sequential fallback.
  - The MAKER_FREE cache is keyed by the BatchSums id, not by thread.
- Callers that run pool work never run it on the calling thread. `ThreadPool::scope` from a non-pool thread injects the scope and blocks. So the per-thread counters (spawn_count `thread_totals`) keep their meaning at every K.

## Why (evidence)

- On a44000e0, matching and pass A make exactly `min(cap, markets)` LPT chunks: 32 at 300 markets on archy (cap = 32 host threads).
- p4s0 W1 all-thread perf (val1, both reps), with 3 validator nodes on one 32-thread host:
  - Pool threads start their chunk 1-8.6 ms apart: start p50 3.4-4.6 ms, p90 7.1-8.6 ms.
  - The work itself is balanced: per-thread sample var/mean is 0.7-0.86.
  - 84-87% of the threads that end a phase were also descheduled mid-chunk.
  - 20-21% of match work runs after the median thread has finished.
  - The cause is that one chunk per thread leaves nothing to steal. A chunk held by a preempted thread cannot be stolen; only a chunk that has not started can be.
- Memories: 0baab8234e5bf138 (lens), 9d5da33a49e53ff7 and 8978b391cb475fb4 (adversarial checks, both KEEP).

## Expected gain (inferred, not measured)

300m, 600 s, K=4:
- Match pool part: -3.5 to -6 ms per native block.
- Pass A: -1.5 to -2.5 ms per native block.
- Total about -5 to -7 ms per native block, which is -1.6 to -2.2% of exec block_ms.
- matched/s: +1 to +2% (central about +1.6%), using sec 67.4's +3.0% matched/s per -10.2% block_ms. This is close to the n=4 paired CI (about +-1.3%, sec 67.3), so use 600 s cells and n >= 4-6.

Other cells:
- 10m: about 0. Ten markets already give 10 chunks, and min(K x cap, markets) stays 10.
- CPU-s/1M: about 0. Each extra job costs a few µs. LPT packing grows to markets x K x cap comparisons per call: 300 x 128, about 20-40 µs at K=4.
- One node per host: expect less, because the start skew comes from 3 nodes sharing 32 threads.

The adversarial checks put the replay estimate at -3.5 to -7 ms per native block. Their pessimistic duty-cycle bound is lower: match -1.9 ms and pass A -0.6 ms.

## How to bench it

- Run it in its own campaign. Never stack it with parallel-match-setup, parallel-classic-book-save or the queued TORUS_PARALLEL_ENGINE A/B: all of them share the exec pool.
- Same binary in both arms. A = env unset, B = `EXTRA_ENV='TORUS_EXEC_POOL_GRAIN=4'` on all nodes (`tools/matched-bench/run-cell.sh <wt> <label> 300 600 ... EXTRA_ENV=...`), ABBA, n >= 4-6 at 300m 600 s, plus one 10m pair.
- Optional: K=8 as a third arm, once K=4 shows the mechanism.
- Accept when all of these hold:
  - AGREE in every cell.
  - matched/s paired-CI lower bound >= 0 at 300m 600 s.
  - No 10m regression.
  - CPU-s/1M not worse.
- Mechanism check:
  - `settle_pass_a_ms` is almost all pool time (weights, chunking and run_jobs), so read it directly. Expect -1.5 to -2.5 ms per native block.
  - `phase_match_ms` also includes about 10-12 ms of serial setup (book loads, request building) per call. So the "pool part down >= 15%" check cannot be read off that timer. Either:
    - compare the phase_match_ms delta to the expected -3.5 to -6 ms per native block, or
    - take an all-thread perf profile per arm (p4s0 W1 method: per-thread first and last sample per phase) and compare the pool span. A run_jobs-only timer would be a follow-up; it is not on this branch.
  - The 15% bar was judged mis-set by both adversarial checks: 15% of the 31 ms match timer is 4.7 ms, more than the optimistic pool saving.

## Tests

- Inline, `market_workers.rs` `mod exec_pool_grain_tests` (7 tests):
  - parse: unset, empty, spaces, garbage, "-4", 0, 1 and " 1 " give 1. "2", "4", " 4\n" and "16" pass through. "17", "99" and an integer past `usize` give 16.
  - `grain_chunk_cap`:
    - cap 1 gives 1 and cap 0 gives 0 for K in {1, 2, 4, 16, 99}.
    - At K=4: cap 32 gives 128, cap 2 gives 8, and `usize::MAX/2` saturates to `usize::MAX`.
    - At K=1 (or pinned 0), cap 32 gives 32.
  - Pin: it clamps (0 gives 1, 17 and `usize::MAX` give 16), nests, is restored on return and on unwind, and does not leak to other threads.
  - Composition, 10 markets: `chunk_indices(.., grain_chunk_cap(cap))` gives 2 chunks at K1 cap 2, 8 at K4 cap 2, 10 at K16 cap 2, 1 at cap 1 for any K, and 10 at K1 cap 32.
- New `crates/torus-bridge/tests/exec_pool_grain_tests.rs` (5 tests):
  - `grain_settle_identical_across_caps_40_markets` and `..._12_markets_second_seed`.
    - Fuzzed batches: ladders, a crossing storm with skewed market load, IOC/FOK/PostOnly, dust and tick rejects, STP pairs, a 12-market batch and a 1-market batch.
    - They run through `execute_batch_settle_workers(.., true, Some(cap))` for cap in {1, 2, 3, 32}, with K pinned 1, the process env, and K pinned 2, 4 and 16.
    - Every run must equal the sequential settle with K pinned 1: CF dump of 6 native CFs, results, gas, fills, fill extras, trade_index, next order id and native state root.
    - At 40 markets on a 32-thread host, the production match cap also gets K x cap chunks.
  - `grain_settle_repeated_runs_identical`: 8 runs at cap 2 with K 4 and 16 against K=1.
  - `grain_planted_pass_a_panic_falls_back_to_sequential`:
    - A backend panics once on the fresh taker's position read, only inside a recovering scope on an exec-pool thread, which is pass A.
    - Checked at cap {2, 3} x K {1, 4, 16}: the panic fires exactly once, `settle_fallbacks == 1`, and the state equals the fault-free run.
  - `grain_match_results_identical_after_sort`:
    - `MarketWorkerPool::match_parallel_capped` on 24 hand-built books with uneven request counts.
    - It runs for cap {1, 2, 3, 5, 32} x K {1, 4, 16} and with the process env.
    - The borsh-encoded books, next order ids and every full `PlaceResult` must equal the cap-1 K=1 run.
- Results on archy, with a private `CARGO_TARGET_DIR` (see the note below):
  - RED first: before the implementation, the new tests failed to compile only on the missing flag items (`with_exec_pool_grain`, `grain_chunk_cap`, `parse_exec_pool_grain`, `exec_pool_grain`, `EXEC_POOL_GRAIN_PIN`).
  - Grain tests plus `chunk_indices_tests` and `capped_chunking_tests`, flag unset: 23/23 passed.
  - `TORUS_EXEC_POOL_GRAIN=4`: 50/50 passed over parallel_settle_tests, parallel_matching_tests, perf_equivalence_golden (GOLDEN digests and the zero-spawn checks), worker_panic, r02_parallel_settle and the grain tests.
  - `rdeps(torus-bridge)`: 1971/1971 passed with the flag unset, and 1971/1971 with `TORUS_EXEC_POOL_GRAIN=4`.
  - `--workspace`: 3824/3824 passed, 0 flaky. Doc tests: rc 0, 1 passed and 0 failed.
  - clippy (`-p torus-bridge --tests`): no warnings in the changed files or lines.
  - rustfmt: the new test file is clean. The changed regions add no diffs; `market_workers.rs` keeps base's 3 old hunks and `native_executor.rs` its old ones.
- Build note: one CARGO_TARGET_DIR shared by several worktrees is unsafe.
  - Cargo gives path packages the same artifact names in every worktree of the repo, and it checks freshness by mtime only.
  - So a sibling's build can be picked up as "fresh" and its code tested in place of yours.
  - It happened here once: the import of `with_exec_pool_grain` vanished with no source change. Every result above comes from `/home/oz/.cargo-target-exp-s58-grain`.

## Risks

- More, smaller jobs mean more rayon wake-ups and steals. That is a few µs per job, up to K x 32 jobs per phase. At K=16 and 300 markets, that is up to 300 jobs of one market each. Watch CPU-s/1M and the exec-pool futex time.
- LPT quality at K x cap is no worse. Smaller chunks make the static balance finer.
- The matching MAKER_FREE memo is per pool thread (keyed by the BatchSums id). Which thread runs a market now depends on stealing, so a maker that rests in many markets may have its `free` computed on a few more threads per block. It is the same value on every thread (state unaffected). The cost is a little CPU, which CPU-s/1M would show.
- A chunk already running on a descheduled thread still cannot be stolen. The saving covers only the start skew plus late-chunk tails, which is why the pessimistic bound is small.
- With one node per host the effect shrinks. The bench box (3 nodes per host) overstates it relative to production.
- Merge notes:
  - parallel-match-setup adds a `MarketWorkerPool` entry with its own `let workers = max_workers.max(1).min(n);`. Apply `grain_chunk_cap()` there at merge time.
  - The paused s123 cancel-all branch adds a third chunk_indices/run_jobs call site. It should adopt `grain_chunk_cap` once both land.
  - settle-passb-deferred-results-scatter edits pass B, a separate hunk.
