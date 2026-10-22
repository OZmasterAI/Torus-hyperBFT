# s80 end-resident-gate: start the end_resident worker at the hand-off, borrow the frozen delta

Branch `s80/end-resident-gate` (worktree `/home/oz/projects/wt/novel-s80/proto/end-resident-gate`, base 29bfc705),
flag `TORUS_S80_ER_GATE` (default off = 29bfc705 behaviour; `1`/`true`/`on` = on; one startup INFO line
`TORUS_S80_ER_GATE on=<bool>`, logged when the execution context is built). Target dir
`/home/oz/.cargo-target-s80-end-resident-gate`.

## Premise check on 29bfc705

* Order in the pipelined branch of `ExecutionContext` block execution (29bfc705 `crates/torus-consensus/src/app.rs`):
  freeze `:6032`, `pipeline_handoff(Job::Flush)` `:6043`, L3.1 debt / L6.1 clear, `checkpoint_upkeep` `:6069`,
  then (after the serial-only branch) `end_resident_on_worker` `:6229`. Same shape as main's `:6243/6254/6274/6434`
  quoted in the write-ups. Confirmed.
* The worker (`EndResidentJob::run`, 29bfc705 `crates/torus-bridge/src/native_executor.rs:5387`) first drops the
  block's `retired` values, then `BlockDelta::into_delta` copies every key and value of R's six CFs out of the frozen
  set (`FrozenPending::resident_delta` -> `resident_delta_of`, `crates/torus-state/src/backend.rs:1025-1034`), then
  `ResidentRows::apply` + `Users::apply` + `carry.finish`, then sends the slot. Confirmed.
* Cost on 29bfc705 (`measured.md`, std CL21): worker 4.07 ms/blk off E, E waits 1.53 ms/blk on it
  (`end_resident_wait`); upkeep 0.72 ms/blk mean (p99 16.7). ubench_econ `UB_R_WORKER=1` at 29bfc705 (prior
  session, `results/ub-end-resident-gate-base-w1.log`): timer/blk 4.06-4.41 ms = users 2.68-2.94 + rows 0.16 +
  ~1.25 ms other (the delta copy, the `retired` drop, `carry.finish`, gauges). The cost is there: not killed.

## Design (implemented: (0) + (A); (B) parallel re-value not attempted)

* (A) borrowed delta, `crates/torus-state`:
  * `ResidentDeltaEntries` trait (`resident_rows.rs:217`): per-slot key-sorted `(key, Option<value>)` entries;
    implemented by the owned `ResidentDelta` and by `FrozenResidentDelta<'_>` (`backend.rs:1039`), which streams
    the frozen `PendingState`'s CFs with the same `cf_stream` merge `resident_delta_of` copies from.
    `FrozenPending::resident_entries()` (`backend.rs:1161`) builds it (no copy).
  * `ResidentRows::apply_entries` (`resident_rows.rs:389`): `apply` over any `ResidentDeltaEntries` (same rows,
    same byte count).
  * `Users::apply` is generic over `ResidentDeltaEntries` (`crates/torus-bridge/src/users.rs:492`), body unchanged.
* (A) worker, `crates/torus-bridge/src/native_executor.rs`:
  * `end_resident_on_worker_with(.., er_gate)` (`:5264`); `end_resident_on_worker` = it with `er_gate_enabled()`.
  * `EndResidentJob::run_split` -> `run_borrowed` (`:5542`) when the gate is on and the delta is
    `BlockDelta::Frozen`: R and Users follow `frozen.resident_entries()`; the block's `retired` values and the frozen
    `Arc` come back as `Leftovers`, dropped on the worker after `done.send(slot)` (inside `catch_unwind`), so the
    next `begin_resident` join returns before those frees. Gate on with a taken delta: only `retired` moves after
    the send. Gate off: byte-for-byte the old `run` (copy, drop `retired` first).
* (0) early start, `crates/torus-consensus/src/app.rs` (`ExecutionContext.er_gate`, read once from
  `er_gate_enabled()` at context construction):
  * right after a successful `pipeline_handoff`, with the gate on: take `resident_rows` / `resident_delta` (a
    detached `begin_resident(None, ..)` placeholder in their place), lock the holder, call
    `end_resident_on_worker_with(.., ok = true, .., true)`, release the holder, `ended_early = true` (`:6071`).
  * the upkeep and the L3.1 close then run while the worker works (the close `try_lock`s the holder: free again;
    it encodes books, never the rows slot; the upkeep detaches the overlay's layers, not R).
  * upkeep failure (`!checkpoint_upkeep`, or the test-only `test_upkeep_fault`): `holder.drop_rows()` waits for the
    worker and drops the slot, the same empty slot as the gate-off early return. The fail-stop is latched either way.
  * the late `end_resident_on_worker_with` call runs only if `!ended_early` (serial blocks: epoch boundaries,
    non-fast-path EVM, ...).
* Node-local, value-neutral: same slot (R rows, Users records, sums version, height) and the same state; only when
  the slot is ready changes.

## Diff summary

Commit f8e1ffff on `s80/end-resident-gate`: 10 files, +734 / -27 (about 450 lines are tests). Production:
`torus-state` backend.rs +25, resident_rows.rs +43, lib.rs export; `torus-bridge` native_executor.rs (+171 incl. docs)
(flag, `_with` entry, `run_split`/`run_borrowed`, `Leftovers`, `drop_rows`), users.rs generic `apply`;
`torus-consensus` app.rs (`er_gate` field, early start, failed-upkeep `drop_rows`, `!ended_early` guard, test hook
`test_upkeep_fault`). Release torus-node built: `/home/oz/.cargo-target-s80-end-resident-gate/release/torus-node`
md5 7df6cc15075651fdda7a5f848b8f62ad (production code identical to f8e1ffff; only test code changed after it).

## Tests

New tests (written in the prior session before the implementation; this session fixed 2 wrong expectations):
* torus-state `resident_rows::tests::frozen_resident_entries_equal_the_copied_delta_and_apply_alike` (200 random
  rounds, parent layers, nested checkpoints, tombstones: view == copied delta per slot/CF; R via view == R via copy,
  byte count).
* torus-bridge `users::tests::users_follow_the_frozen_view_like_the_copied_delta` (8 seeds x 150 blocks: same
  reports in the same order, same records, == cold build).
* torus-bridge `sums_cache_tests::er_gate_worker_on_the_frozen_view_equals_inline_and_reference` (results + rows
  per block vs inline and reference, carried sums).
* torus-bridge `end_resident_worker_tests::{er_gate_worker_slot_equals_the_copied_delta_slot,
  er_gate_slot_is_ready_before_the_leftovers_drop, er_gate_flag_parse}` (slot == inline == gate-off worker, R == DB;
  the join returns while a blocking `drop_later` value is still dropping with the gate on, waits with it off).
* torus-consensus `crash_recovery_tests::{resident_rows_equal_db_scan_after_every_block_serial_and_pipelined,
  end_resident_runs_on_a_worker_joined_by_the_next_block}` extended to (pipelined x er_gate); new
  `er_gate_failed_upkeep_leaves_the_rows_slot_empty`; `o0_coverage_report_counts_a_known_stream` expects
  `er_gate.end_resident_at_handoff = 8` (= handoff.flush) when the env flag is on.

Fixed expectations (test bugs, not code bugs): (1) the early-start count must equal the workers of pipelined blocks
only: epoch boundaries (`epoch_length 4`) run serially and end late (8 of 11 in the fixture); (2) the failed-upkeep
test compared DB dumps while racing W against the fail-stop latch (W refuses to write once `exec_failed` is set,
R01b; the gate's hand-over delays the latch by microseconds, so W wrote block 3 only in the gate-on arm). W is now
parked with `WorkerGate::hold_from(3)` until the block returned; both arms land marker 2 and equal dumps.

Results (18c, debug, all under the heavy lock):
| run | result |
|---|---|
| golden `binary(perf_equivalence_golden)` gate off / on | 5/5 pass / 5/5 pass (note: `test(perf_equivalence_golden)` selects 0 tests; the binary filter is needed) |
| state+bridge targeted (52 tests: er_gate, frozen view, resident_rows, end_resident, sums_cache, users_) off / on | 52/52 / 52/52 |
| consensus targeted (er_gate, resident, end_resident) off, before the 2 fixes | 13/15 (the 2 wrong expectations) |
| consensus targeted on / off, after the fixes | 15/15 / 15/15 |
| torus-consensus full, gate on (before the coverage fix) | 622/623: only `o0_coverage_report_counts_a_known_stream` (extra path key); fixed, rerun with the er_gate set on: 16/16 |
| torus-state + torus-bridge full, gate on | 1512/1513: `checkpoint_layer::tests::deferred_part_blocks_only_deferred_reads` SIGSEGV AFTER `test result: ok` (process teardown with detached reader threads; torus-state checkpoint code and the flag are untouched by this change; load average ~460 at the time). Stress rerun queued, result not in at report time |
| torus-state + torus-bridge full, gate off; torus-consensus full, gate off | queued behind the shared lock, not finished at report time (logs: `results/erg/state-bridge-full-off.log`, `consensus-full-off.log`) |

All logs: `/home/oz/projects/wt/novel-s80/proto/results/erg/`.

## Microbench (ubench_econ, `UB_R_WORKER=1`, 300 markets, UB_RUNS=5, `TORUS_SETTLE_STREAMED=1`, one binary)

Method: `ub.sh end-resident-gate w1-<arm>-r<i> UB_R_WORKER=1 TORUS_S80_ER_GATE=0|1`, ABBA order
off,on,on,off,off,on,(on,off pending), one binary (`ubench_econ-1a6f456edd610a23`, built 08:59 from the change).
The harness has no inter-block work, so `wait/blk` is 0 in both arms and `timer/blk` (the worker's
`exec_end_resident_seconds`, i.e. until the slot is sent) is the worker's critical path. Per-log value = median of 5 runs.

| round | arm | engine ms/1k | rows/blk | users/blk | timer/blk | per-run timer/blk |
|---|---|---|---|---|---|---|
| r1 | off | 2.80 | 0.16 | 2.74 | 4.15 | 4.07 4.07 4.17 4.15 4.16 |
| r2 | on | 2.79 | 0.20 | 2.75 | 2.96 | 2.89 2.94 3.05 2.96 3.21 |
| r3 | on | 6.74 | 0.50 | 8.24 | 9.29 | EXCLUDED: host load spike (load avg ~90, engine per run 3.97 6.74 7.33 7.42 5.03 vs calibration 2.79-2.93; timer 5.24 9.29 9.61 9.06 9.61) |
| r4 | off | 2.77 | 0.17 | 2.78 | 4.16 | 4.10 4.01 4.26 4.16 4.16 |
| r5 | off | 2.80 | 0.16 | 2.76 | 4.17 | 4.04 4.12 4.45 4.17 4.25 |
| r6 | on | 2.81 | 0.20 | 2.81 | 3.02 | 3.00 3.20 3.30 3.02 3.00 |
| r7 on, r8 off, r9 on, r10 off | | queued | | | | not in at report time (`results/ub-end-resident-gate-w1-*-r{7..10}.log`) |

Means of medians (clean rounds): off (n=3) timer 4.160, users 2.760, rows 0.163, engine 2.790; on (n=2) timer
2.990, users 2.780, rows 0.200, engine 2.800. Delta timer -1.17 ms/blk (-28.1%); users +0.02 (noise); rows +0.04
(the `cf_stream` merge per entry vs iterating a copied map); engine +0.01 ms/1k (+0.4%, noise: this path is off the
engine). Spread within an arm: off 4.15-4.17, on 2.96-3.02, far apart. Prior base log (29bfc705 binary,
`ub-end-resident-gate-base-w1.log`): timer 4.06-4.41, median engine 2.80, consistent with the off arm.

(0) (the early start) is not measurable here (no upkeep / hand-off between blocks); only a cell shows it.

## Verdict

Built; (A) cuts the worker's critical path by ~1.17 ms/blk (4.16 -> 2.99 ms, -28%) in ubench, well above the
0.3 ms/blk cell threshold; (0) adds a head start of hand-off + upkeep (~0.7 ms mean std) on top. With the cell's
E wait at 1.53 ms/blk, the two together should take most of `end_resident_wait` away (expected -1.0..-1.5 ms/blk of
the ~38 ms chain: +2.5..+4% blk/s if the wait translates 1:1; the write-ups estimated +0.6..+2.7% blk/s,
+0.2..+0.9% matched/s). Engine ms/1k is unchanged (expected: it is E wait, not engine CPU), so the gain is only
visible in a cell: cell-worthy. Risks for the cell: three background threads (W, ckpt merge, end-resident) now start
together on an oversubscribed host, which can slow the upkeep a little; check `torus_exec_end_resident_wait_seconds`
sum/count (target <= 0.6 ms/blk), `torus_exec_checkpoint_upkeep_seconds`, R rebuilds (`rows_builds` must stay 1).
Cell env for the ON arm: `TORUS_S80_ER_GATE=1` (log line `TORUS_S80_ER_GATE on=true`).
Open: the gate-off full suites and the SIGSEGV stress rerun had not finished when the session had to report;
(B) (parallel re-value of Users::apply, the remaining 2.8 ms) not attempted.
