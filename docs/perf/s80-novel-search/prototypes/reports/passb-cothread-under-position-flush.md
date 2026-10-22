# s80 prototype: passb-cothread-under-position-flush

Run settle pass B, unchanged, on one co-thread, overlapped with the exec thread's (E's) position flush. This is the
in-order variant.

- **Branch:** `s80/passb-cothread-under-position-flush` (local only, not pushed).
- **Commit:** `768e05d6` on base 29bfc705.
- **Worktree:** `/home/oz/projects/wt/novel-s80/proto/passb-cothread-under-position-flush`.
- **Target dir:** `/home/oz/.cargo-target-s80-passb-cothread-under-position-flush`.
- **Flag:** `TORUS_S80_PASSB_COTHREAD` (node-local, read once). Values:
  - 0 (default) = the exact 29bfc705 behaviour;
  - 1 = co-thread overlapped with the position flush;
  - 2 = co-thread joined before the flush, which costs the hand-off alone.
- **Startup log line:** `TORUS_S80_PASSB_COTHREAD ... on=<bool> mode=<n>`.
- **Cells:** none were run (session rule for this batch), so `cell_worthy=false`.

## Premise check on 29bfc705

All code references were re-located in the 29bfc705 worktree:
- streamed region `match_settle_streamed`: NE ~10892;
- end-of-call flush: NE ~9990;
- `PassBSink` / `PassBOut`: NE ~6125-6200.

I added a flag-0 probe, `ExecPhaseAccum.settle_pass_b_late_ns`: the pass B time on E for markets applied after every
pool job had finished. I also split the cache-flush timer and measured both with `ubench_settle_streamed` (forced
parallel settle, L2.1 on, L2.2 on, wake batch auto, 10 rotated reps, quiet host, round 2).

| shape | pass B on E | of it after the pool finished ("late") | position flush | whole cache flush |
|---|---|---|---|---|
| 16.8k fills | 5.74 ms | **2.54 ms (44%)** | **10.67 ms** | 19.20 ms |
| 65k fills | 16.97 ms | 3.63 ms (21%) | 36.81 ms | 45.63 ms |

- **The premise holds, but at about half the write-up's size.** Part of pass B sits on E after matching ends, and it
  is much smaller than the position flush, so a co-thread can hide it completely. In this harness that part is 44% /
  21% of pass B, not the ~80% the write-up took from the 9b cells.
- **The flag-0 cost on E that can be removed** is the late pass B (~2.5-3.6 ms per call). Part of the in-region pass
  B can go too: in the region E stops doing pass B and only takes, plans and hands off.

## Design (as built)

**Flag 0.** The 29bfc705 code path, with these changes only:
- the position part of the end-of-call flush is factored, unchanged, into `flush_positions`;
- three timers are added;
- a test seam is added.

**Flag 1 / 2.** A new branch in `execute_batch_inner`, taken when the call is streamed and the mode is not 0. It works
like this:

1. `std::thread::scope` spawns one named co-thread, `torus-passb-co`.
2. The co-thread owns:
   - the call's `PassBSink` (`absorb_indexes=false`, as in the region);
   - a `PassBOut` (results, gas, balance cache, volume cache, deferred results), holding a stand-in position cache and
     `pos_runs: None`.
3. E runs `match_settle_streamed_co`. It is a copy of `match_settle_streamed` with the same:
   - take order;
   - gate and decision;
   - plan-on-E for markets matched before the gate;
   - stop rule (failed or impure plan).
4. Instead of calling pass B, E does three things for each market:
   - it `mem::take`s the plan's `pos_cache`;
   - it merges that cache, or pushes it onto `pos_runs` under L2.2, in market order, exactly as pass B would;
   - it sends `(MatchResult batch, plan minus pos_cache)` over an `mpsc` channel.
5. The co-thread runs the unchanged `settle_pass_b_market` in market order. It records `AppliedMarket` (`was_dirty` is
   read on the co-thread) and returns `(out, applied, busy time)`.
6. **Mode 1, clean call** (no match panic, parallel decision, `rest` empty):
   - E runs `flush_positions` before joining;
   - its `Result` is kept and latched after the join, at the original site. A failed position flush therefore still
     overwrites whatever pass B latched ("the flush message wins", as in today's order).
7. **Any other exit** (match panic, plan-failure fallback, impure-plan prefix, sequential gate, or mode 2) joins first.
   It then points `out.pos_cache` / `out.pos_runs` back at the real caches and continues on the original code:
   - `undo.rollback` + `match_panic_exit`;
   - `undo_streamed_prefix`;
   - `commit_undo` / `undo.commit`;
   - `absorb_indexes_after_stream`;
   - `settle_streamed_rest`;
   - the rest of the flush.
8. **A co-thread panic** comes back through `join()`. It is re-raised on E as
   `panic!("settle pass B co-thread panicked: <msg>")`, so the node's fail-stop hook fires on `torus-execution`.
   `resume_unwind` would skip the hook.
9. **If E panics,** the sender is dropped, the co-thread drains and the scope re-raises.
10. **Scope:** the scope encloses `run_streamed`, the early flush and the join.

**Why the bytes are the same:**
- Pass B reads only balance rows (`BalanceCache::load` -> `get_native_balance_typed`) and writes nothing to the
  backend.
- The position flush writes only `CF_NATIVE_POSITIONS` rows (`put_cf_raw_owned` / `delete_cf_raw`, which take the
  overlay's `pending` write lock).
- So the overlap changes no read value and no write, and the write sequence stays positions, then balances (after the
  join), then cum-volume.
- Pass B itself is the same code, in the same market order, over the same inputs. Only E's position-cache handling
  moved, and it moved in the same order.

## Diff summary (3 files, +884 / -52)

**`crates/torus-bridge/src/native_executor.rs`:**
- Flag: `parse_passb_cothread`, `passb_cothread_mode`.
- Test pins: `with_passb_cothread`, `with_passb_panic_market`.
- Counters: `passb_cothread_calls_on_this_thread`, `passb_cothread_overlapped_on_this_thread`.
- Helper: `panic_payload_message`.
- `PassBSink.panic_market`, the test seam, checked at the top of `settle_pass_b_market`.
- `ExecPhaseAccum` fields: `cache_flush_pos_ns`, `passb_co_ns`, `passb_join_wait_ns`, `settle_pass_b_late_ns`.
- `flush_positions`: the end-of-call position flush, factored out unchanged.
- The mode-1/2 branch in `execute_batch_inner`.
- `match_settle_streamed_co`.

**`crates/torus-bridge/tests/settle_streamed_tests.rs`:**
- The `cothread_twin!` macro re-runs 17 L2.1 scenarios under modes 1 and 2. Each must equal L2.1-off in:
  - the backend write sequence;
  - results, gas, fills and extras;
  - the fatal error;
  - the DB and the state root.
- It also asserts that the co-thread ran pass B, and in mode 1 that it overlapped the flush (expected 0 for the
  forced-sequential scenario).
- Scenarios covered:
  - block stream, second seed, randomized shapes;
  - scheduling perturbation: E-side and co-thread-side jitter;
  - plan-panic fallback, plan read fault, fatal position-flush failure, match panic;
  - aliased opaque rows, Auto gate, engine mode, forced sequential, impure prefix;
  - match panic across a block rollover, prefix balance fault then plan panic;
  - fault matrix a and b.
- `passb_cothread_panic_is_reraised_on_the_exec_thread`: modes 0/1/2, planted pass-B panic.
- `passb_cothread_flag_parse`.

**`crates/torus-bridge/tests/ubench_settle_streamed.rs`:**
- `UBENCH_CO` (modes per flag-on arm), `UBENCH_OFF=0`, `UBENCH_SHAPES`.
- Per arm, printed:
  - flush, of which the position part;
  - co-thread pass B, join wait, late pass B;
  - mean wall, CPU and join;
  - raw per-rep wall, E on-CPU (schedstat) and join.

## Tests (release profile; logs in `proto/results/pbc/`)

**Red step:** the new tests reference APIs that do not exist at 29bfc705 (`with_passb_cothread`,
`passb_cothread_*_on_this_thread`, `with_passb_panic_market`, `parse_passb_cothread`), so they fail to compile there.

**Mutation check:** the early flush made to run on any non-panic parallel call, forgetting `rest`. See "Mutation"
below.

| run | env | result |
|---|---|---|
| 1 | `settle_streamed_tests` (40 = 21 L2.1 + 19 new), flag unset (modes are pinned per test) | 40/40 pass |
| 2 | `perf_equivalence_golden` + `parallel_settle_tests`, flag unset | 13/13 pass |
| 3 | `TORUS_SETTLE_STREAMED=1 TORUS_S80_PASSB_COTHREAD=1`: golden + parallel_settle + settle_streamed | 53/53 pass |
| 4 | `TORUS_S80_PASSB_COTHREAD=1` alone: golden | 5/5 pass |
| 5 | `TORUS_SETTLE_STREAMED=1 TORUS_S80_PASSB_COTHREAD=2`: golden + parallel_settle | 13/13 pass |
| 6 | torus-bridge `--lib` + 13 touched-module suites[^suites], flag unset | 371/371 pass (3 skipped) |
| 7 | same set, `TORUS_SETTLE_STREAMED=1 TORUS_S80_PASSB_COTHREAD=1` | 371/371 pass (3 skipped) |

[^suites]: `settle_no_merge`, `settle_defer_results`, `exec_phase_accum`, `engine_parallel`, `book_rows`,
`position_cache_exec`, `r02_fail_stop`, `trade_defer`, `parallel_matching`, `parallel_match_setup`, `settle_streamed`,
`parallel_settle`, `perf_equivalence_golden`.

- No flaky tests.
- The whole torus-bridge suite and the workspace were not run (prototype, lock-bound host).
- The golden runs with flag 1 do not prove the co-thread engaged there: Auto's gate decides that. The
  `settle_streamed` twins do assert engagement and overlap.

**Mutation (red) check** (`proto/results/pbc/all3.log`):
- The mutant drops `&& so.rest.is_empty()` from the clean-call test, so the early position flush also runs when an
  impure or failed plan left markets in `rest`. That is a realistic bug: the rest's position caches would land in the
  stand-in cache and never be flushed.
- Result: **at least 5 of the 19 `passb_cothread_*` tests fail** (`write sequence diverged`, `settle_streamed_tests.rs:765`):
  - `impure_prefix_plan_stops_the_stream`;
  - `fault_matrix_a`, `fault_matrix_b`;
  - `prefix_balance_fault_then_plan_panic_matches_flag_off`;
  - `identical_with_aliased_opaque_rows`.
  The output was cut at 30 lines.
- I then reverted with `git checkout`, rebuilt, and all 19 `passb_cothread_*` tests passed again.

## Microbench

### A. `ubench_settle_streamed` (the spec's harness)

Settings: forced parallel settle, L2.1 on, L2.2 on, wake batch auto. The arms co0 / co1 / co2 rotate per rep.

**Round 2** (quiet host, load 9 -> 9.6, 10 reps per arm), `proto/results/pbc/settle-ubench-2.log`:

| shape | arm | wall median | wall mean | E on-CPU median | co pass B | mean join wait |
|---|---|---|---|---|---|---|
| 16.8k | co0 (off) | 52.64 | 52.62 | 49.51 | - | 0 |
| 16.8k | co1 | **49.87** | **50.07** | **44.81** | 7.31 | **0.00** |
| 16.8k | co2 | 54.21 | 53.95 | 44.76 | 6.54 | 3.78 |
| 65k | co0 (off) | 114.59 | 115.18 | 106.46 | - | 0 |
| 65k | co1 | **112.10** | **112.35** | **93.77** | 19.51 | **0.00** |
| 65k | co2 | 115.29 | 115.48 | 93.41 | 18.31 | 3.55 |

All values are ms per call.

Raw per-rep values, wall / E CPU:
- **16.8k co0:**
  - wall 55.99, 50.78, 51.99, 52.45, 54.19, 53.03, 50.77, 51.05, 53.15, 52.83;
  - CPU 52.36, 48.1, 49.14, 49.22, 51.07, 49.64, 48.64, 49.56, 51.7, 49.47.
- **16.8k co1:**
  - wall 49.7, 49.68, 50.24, 51.02, 49.77, 50.6, 48.98, 50.99, 49.98, 49.73;
  - CPU 43.76, 45.0, 44.76, 45.68, 43.89, 44.85, 43.79, 45.9, 45.05, 44.7;
  - join all 0.0.
- **16.8k co2:**
  - wall 56.38, 56.14, 54.34, 54.5, 52.67, 52.24, 54.07, 51.52, 53.24, 54.4;
  - join 5.28, 5.03, 4.28, 4.82, 3.23, 3.25, 3.86, 2.42, 2.11, 3.5.
- **65k co0:**
  - wall 119.03, 114.12, 116.4, 112.15, 115.06, 117.19, 112.6, 113.58, 113.97, 117.65;
  - CPU 108.51, 107.91, 109.32, 105.61, 106.43, 106.5, 106.32, 105.28, 105.1, 109.68.
- **65k co1:**
  - wall 115.99, 110.73, 111.12, 110.9, 111.95, 112.25, 112.79, 110.49, 114.46, 112.81;
  - CPU 95.83, 92.48, 92.92, 92.46, 93.73, 93.81, 94.84, 91.45, 95.39, 94.75;
  - join all 0.0.
- **65k co2:**
  - wall 113.31, 113.36, 114.4, 116.19, 119.91, 116.84, 112.64, 116.9, 114.36, 116.86;
  - join 2.54, 2.09, 2.25, 4.01, 6.67, 3.76, 3.06, 3.2, 3.29, 4.67.

**Mode 1 vs off:**

| shape | wall (mean) | wall (median) | E on-CPU (median) | join wait | co-thread pass B vs E's pass B |
|---|---|---|---|---|---|
| 16.8k | **-2.55 ms (-4.8%)** | -2.77 ms (-5.3%) | -4.70 ms (-9.5%) | 0.00 ms | 7.31 vs 5.74 ms (1.27x) |
| 65k | **-2.83 ms (-2.5%)** | -2.49 ms (-2.2%) | -12.7 ms (-11.9%) | 0.00 ms | 19.51 vs 16.97 ms (1.15x) |

- **16.8k:** at 16.8k co1's slowest rep (51.02) is below co0's median.
- **Join wait:** the co-thread's post-pool backlog is about 2.5-3.6 ms x 1.15-1.27, well under the 10.7 / 36.8 ms
  position flush, so it is always hidden.
- **Co-thread pass B is slower than E's pass B,** as the write-up predicted: cross-core cache traffic plus `pending`
  RwLock contention with the flush.
- **Mode 2 (hand-off cost alone):**
  - 16.8k: +1.33 ms mean wall (+2.5%);
  - 65k: +0.30 ms;
  - mean join wait 3.78 / 3.55 ms.
  - So the hand-off (spawn, channel, the slower co-thread pass B, join) costs about 1-1.3 ms of E wall at 16.8k, and
    mode 1 hides it plus the late pass B.

**Round 1** (`settle-ubench-1.log`, 6 reps) ran while a debug test outside the lock loaded the host to load ~50-100.
Its walls are 2x inflated. It is reported for completeness only:
- 16.8k: co0 105.22 / co1 102.39 / co2 108.44 median;
- 65k: 239.06 / 225.26 / 227.10.
- Mode 1's mean join wait was 0.00 there too.

**Spec kill criteria:**
- wall saving < 1.5 ms per call at 16.8k idle: measured 2.55-2.77 ms, **pass**;
- mean join wait > 0.5 ms: measured 0.00 ms, **pass**;
- any test or byte difference: none, **pass**;
- under `stress-ng --cpu 24` (< 1.0 ms): **not run**, so as not to load a host shared with other agents' runs.

### B. `ubench_econ` (engine proxy; `ub.sh` env, 5 runs per invocation)

`TORUS_S80_PASSB_COTHREAD=0` vs `1`, ABBA order. The logs are
`proto/results/ub-passb-cothread-under-position-flush-<arm>-<label>.log`.

- **Contamination:** the host was intermittently loaded by other agents' work running outside the heavy lock (load 20
  to 100; for example a `ckpt_close_stack` debug test at 160% CPU). Runs inside those windows read 3.8-7.0 in both
  arms; calibration is 2.79-2.93.
- **Validity rule:** these runs are listed but excluded. A run is valid if its median is within the calibration band
  plus 10% (< 3.2) and the host load stayed below ~15.
- **Round 3** waits for a 1-minute load below 8 before every run and logs the load.

**Round 3** (clean; 1-minute load 2.6-6.6 at every run; `proto/results/pbc/ub-ab3.log`). Order: off, on | on, off |
off, on | on, off | off, on | on, off.

| round | off median | on median |
|---|---|---|
| 1 | 2.79 | 2.56 |
| 2 | 2.75 | 2.58 |
| 3 | 2.78 | 2.54 |
| 4 | 2.79 | 2.58 |
| 5 | 2.79 | 2.57 |
| 6 | 2.74 | 2.57 |
| **mean of medians** | **2.773** | **2.567** |

Values are engine ms per 1k fills.

- **Delta of means: -7.4%.** The arms do not overlap: off ranges 2.74-2.79 and on ranges 2.54-2.58. The spread is about
  2% within each arm, so the gap is well beyond it.
- Raw per-run values (5 per invocation) are in the `MEDIAN ... runs=[...]` lines of the logs:
  - off-3-1: 2.788 2.793 2.795 2.730 2.716
  - on-3-1: 2.565 2.607 2.558 2.537 2.560
  - on-3-2: 2.642 2.638 2.571 2.521 2.582
  - off-3-2: 2.875 2.824 2.747 2.742 2.743
  - off-3-3: 2.789 2.787 2.754 2.719 2.778
  - on-3-3: 2.593 2.611 2.544 2.502 2.531
  - on-3-4: 2.581 2.633 2.595 2.537 2.536
  - off-3-4: 2.855 2.829 2.793 2.732 2.726
  - off-3-5: 2.840 2.824 2.735 2.693 2.790
  - on-3-5: 2.594 2.609 2.542 2.522 2.571
  - on-3-6: 2.568 2.611 2.560 2.564 2.589
  - off-3-6: 2.923 2.826 2.740 2.711 2.718

**Round 3 phase breakdown** (30 runs per arm, mean):

| arm | engine ms/blk | exec/1k | margin/1k | match/1k | settle/1k | tail(liq)/1k |
|---|---|---|---|---|---|---|
| off | 32.19 | 2.295 | 0.545 | 0.167 | 0.773 | 0.483 |
| on | 29.83 | 2.085 | 0.543 | 0.170 | **0.570** | 0.488 |

- **The whole gain is in settle:** -0.203 per 1k fills, about -2.35 ms per block at about 11.6k fills per block.
- **Engine time:** -2.36 ms per block (-7.3%).
- Margin, match and the liquidation tail are unchanged within noise, so the co-thread does not slow the next phases.

**Same outcomes in both arms:** each run's fills per block, accepted orders, `rejected_cancelled`, `resting_end` and
R rows / bytes are identical between off and on. For example, off-3-1 vs on-3-1 per run: 11294 / 91200 / 46407,
11379 / 90000 / 49643, and so on. Each run also passes `ubench_econ`'s "carried R == cold build over the final DB"
assert.

**Rounds 1-2** (`proto/results/pbc/ub-ab1.log`, `ub-ab2.log`), valid runs only:
- off: 2.81, 2.75, 2.78, 2.78, 2.73, 2.80;
- on: 2.79, 2.57, 2.59, 2.55.

The contaminated runs are excluded but listed here:

| run | median | per-run values |
|---|---|---|
| off-1a | 5.86 | 5.6-6.1 |
| on-1a | 4.11 | 2.75-4.85 |
| on-2-1 | 4.10 | |
| on-2-2 | 3.82 | |
| off-2-2 | 6.95 | |
| off-2-3 | 5.59 | |
| on-2-3 | 3.84 | |
| on-2-4 | 3.97 | |

**All valid runs** (n = 12 off, 10 on): off mean 2.774, on mean 2.590, **-6.6%**.

## Verdict

**BUILT. It passes every kill criterion of the spec that was measured.** The gain is real but moved, not removed.

**Why it passes:**
- **Correctness:**
  - Byte identity holds over the whole L2.1 scenario set under modes 1 and 2, including:
    - the write sequence, the DB and the state root;
    - the fault and fallback paths;
    - perturbed scheduling.
  - Golden, `parallel_settle` and 13 touched-module suites plus the lib tests are green with the flag off and on.
  - A planted mutant is caught.
  - A co-thread panic is re-raised on the exec thread.
- **Spec harness** (`ubench_settle_streamed`, quiet host):

  | shape | E wall per call | E on-CPU | mean join wait |
  |---|---|---|---|
  | 16.8k | -2.55 ms (-4.8%) | -9.5% | 0.00 ms |
  | 65k | -2.83 ms (-2.5%) | -11.9% | 0.00 ms |

  Mode 2 shows that the hand-off alone costs about +1.3 ms per call at 16.8k. Mode 1 hides it plus the late pass B
  under the 10.7 ms position flush.
- **Engine proxy** (`ubench_econ`, clean ABBA, 6+6 rounds): **-7.4%** engine ms per 1k fills (2.773 -> 2.567), all
  of it in settle. This is above the write-up's forecast of -4..-6.5% E wall, and it lands in the forecast band of
  -2.0..-3.0 ms per block (measured -2.36).

**Caveats for a cell or a productionized version:**
- **Node CPU:** the work moves to a second thread, so node CPU rises. The co-thread's pass B runs 1.15-1.27x slower
  than on E.
- **On a busy 3-node host** the co-thread can be preempted. The join wait was 0 here only because the position flush
  (about 3x the late pass B) gives ample slack. Under load it would show up as join wait. The stress-ng arm was not run.
- **Cell expectation:** blk/s +2..+6% if E is the pacing stage. matched/s is likely flat on the standard load-bound
  cell, as with L2.1.
- **Prototype shortcuts:**
  - a thread spawn per `execute_batch` call (about 30-50 us; a persistent co-thread would remove it);
  - an unbounded `mpsc` channel;
  - `match_settle_streamed_co` is a copy of the L2.1 region (about 230 lines), which should be deduplicated;
  - the fail-stop path re-raises as a new panic on E (the co-thread is not in `FAIL_STOP_THREADS`).
- **The 80% premise was half true:** this harness shows 21-44% of pass B after the pool, not ~80%. The overlap still
  pays because E also stops doing in-region pass B.

**cell_worthy: false** under this batch's session rule (no cells). On the merits it would qualify: tests are green
and the engine gain of about 7% is well beyond the 3-5% spread. I recommend a 2+2 ABBA CL21 cell with
`TORUS_S80_PASSB_COTHREAD=1` in a later batch. The `torus-node` release binary was not rebuilt for this commit.
