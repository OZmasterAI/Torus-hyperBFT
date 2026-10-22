# s80 prototype: exec-commit-units (`TORUS_S80_EXEC_JOIN=K`)

Branch `s80/exec-commit-units` (base 29bfc705; build round 38189fe2, **fix round ff75d475**), worktree `/home/oz/projects/wt/novel-s80/proto/exec-commit-units`,
target dir `/home/oz/.cargo-target-s80-exec-commit-units` (release `torus-node` rebuilt at ff75d475 12:02:55, md5
bcfa318148d02610a9758079265bb115;
`bench-throughput` md5 fa8fd4bd unchanged). Host: ozarchy. Idea: `ideas/r1-lit-bft.md` section 1.

## Verdict

The premise holds on 29bfc705, and the exec-side join works as a prototype. Running 2 committed blocks as one exec
unit cuts the exec thread's own time per fill by about **10-12%**:
- Node-level µbench, build round: **-10.5%** (ABBA 6 vs 6, spread < 1.5%).
- Node-level µbench, fix round (ff75d475): **-11.2%** over ABBA 6 vs 6 (fix-a -12.0% on a loaded host, fix-b -10.5% on
  a quiet host; medians -10.5%).
- Engine-only proxy (unchanged by the fix round): -10.2% in the interleaved sweep and -7.6% in the s4 ABBA.
- K = 3 gave -12.5% and K = 4 gave -8.7% (build round, n = 2, noisy).

The fix round closes the review's two P1s, the P2 and the P3s:
- CoreWriter rows due inside a unit are drained.
- A restart draws the same units as a live peer.
- Units never span two header seconds.
- K is thread-scoped, not a test-writable process global.

The flag-on state transition is deterministic, and new tests show it is the same across:
- runs
- serial and pipelined execution
- live and restart (close mid-unit, crash gap, tip mid-unit)

The flag is off by default, and off is the 29bfc705 behaviour: golden `binary(perf_equivalence_golden)` passes
5/5 with the flag off and 5/5 with `TORUS_S80_EXEC_JOIN=2` (fix round).

**Cell-worthy: yes.** Proposed cell env: `TORUS_S80_EXEC_JOIN=2`, with the rebuilt release `torus-node` (ff75d475,
md5 bcfa3181...).
- The exec-side gain is well above the spread. matched/s, blk/s and commit / exec latency (including the head-of-unit
  wait) can only be seen in a cell.
- Expected: exec capacity +10-12% at K = 2. With the 0.3 elasticity seen before, that is about +3% matched/s. If the
  run is purely exec-bound, it is up to +11%.
- Read the cell with the per-1k-fills metrics; see "Cell request".

This is a measurement prototype, **not production wiring**. The flag-on state transition differs from per-block
execution (see "Semantic delta"). Production would need the header flag, P0.0 feature, and the RPC / replay plumbing
from the write-up.

## Fix round (review issues 1-7), commit ff75d475

Base of this round: 38189fe2. All seven review points are addressed in code or in this report. The flag-off path is
the same as before: `execution_loop` with K = 1 is the per-block loop (`exec_one` per received block), the replay
with K = 1 is the old per-block replay, and the only new work on the K = 1 path is one thread-local read per block
(`EXEC_JOIN_FIRST`, already there) plus the settings read at loop start.

1. **[P1] CoreWriter rows due inside a unit.** Fixed in `execute_committed_block_with` (`app.rs`, right after the
   block overlay is built): for a joined unit (`EXEC_JOIN_FIRST` > 0) it checks `core_writer_due(&overlay, h)` for
   every member below the last height, reading through the same overlay as the drain (DB + pipelined parent + L/C
   layers). If any are due, it sets `EXEC_JOIN_REFUSED` and returns before any write, and `execute_joined` runs the
   members alone. Each member then drains its own height. A read error fail-stops, like the existing due-check. The
   choice depends only on the state after the previous block, so the live loop and the replay make it the same way.
   Test: `s80_exec_join_core_writer_row_due_inside_a_unit_is_drained` queues a lockbox deposit due at 1 and runs
   blocks [1,2] (same second) at K = 2 on the pipelined and the serial paths. Result: the row is drained, the deposit
   is credited, and the full CF dump equals the K = 1 run.
2. **[P1] Restart determinism.** Three changes:
   - `exec_join_loop` (new, `app.rs`): a unit cut by the channel close is **not executed**. Its rows are durable, so
     it is left for the next start.
   - `replay_committed` → `replay_committed_joined` (flag on): the boot replay runs **whole units only**, each through
     `exec_unit`, which is the live loop's grouping function. A unit that the committed tip cuts is left unexecuted,
     and the replay returns the tip's header so consensus resumes at the same view. A hole inside a unit leaves the
     unit's earlier members unexecuted too.
   - The live loop tracks the last executed height (seeded from the applied marker). When a received height's unit
     has earlier members that never came over the channel (after a restart, or a parked hole that heals), it loads
     them from the DB and runs the unit whole.

   Test: `s80_exec_join_restart_matches_the_live_run` covers three cases, each ending byte-identical (CF dump +
   running hash) to the uninterrupted K = 2 run of the 14-block paired fixture:
   - (a) The channel closes inside [13,14]: applied stays 12. The replay leaves 13 and returns header 13. The
     restarted loop gets 14, loads 13 and runs [13,14].
   - (b) Crash after 1..4 with 5..14 committed: the replay runs [5,6], 7..12, [13,14].
   - (c) As (b) with the tip at 13: the replay stops at 12 and the loop runs [13,14].
3. **[P2] Timestamps.** `exec_unit` only extends a join run with a block that has the **same header timestamp** (the
   header is in seconds; `block_timestamp_ms` = seconds x 1000). So the merged block's time equals every member's
   time. The nonce window of the replay guard, session expiry in verify, oracle timestamps and the nonce-window prune
   are therefore exactly the per-block checks.
   - Cost: at the cells' ~55 ms cadence, a second boundary falls about once per 18 blocks. At K = 2 it lands
     inside a unit about half the time, so about 5% of units run as two blocks (at most 1 in 10 at a 50 ms cadence).
     The µbench's 55 ms header seconds happen to put all 5 boundaries between units (heights 19 / 37 / 55 / 73 / 91
     start units), so its K = 2 arms join every unit. Its counters equal the previous round's exactly.
   - Test: `s80_exec_join_never_joins_across_header_seconds`. When every block has its own second, K = 2 is
     byte-identical to K = 1 (dump, hash, native phase count), pipelined and serial.
   - The loop test now uses a fixture with paired seconds.
4. **[P3] Process-global K.** `exec_join` no longer has a writable global:
   - The process default is read from the env once (`OnceLock`, which logs the INFO line) and never written.
   - A thread-local `exec_join::Scope` overrides it on one thread and restores it on drop, including on a panic.
   - `ExecutionContext.join` captures the constructing thread's settings. `execution_loop`, the boot replay and the
     flush worker (captured at spawn) install them on their own threads.
   - In a node every thread sees the env value. In tests a scope never leaks to another test's threads.
   - `set_k` / `set_liq_per_unit` are gone. Test: `scope_is_thread_local_and_restores` (torus-state).
5. **[P3] Golden with the flag on.** Confirmed: the torus-bridge golden never forms units. With the lazy env default,
   `TORUS_S80_EXEC_JOIN=2` now makes `follows` / `reaches` use K = 2 inside the executor, so the flag-on golden
   checks only that the relaxed height checks leave per-block execution unchanged. The flag-on equivalence evidence
   is the consensus s80 tests above:
   - loop vs direct units
   - K = 2 vs K = 1 when no units may form
   - restart vs live
   - CoreWriter refusal vs per-block
6. **[P3] Fail-stop reported K times.** `exec_after_block` is split into `exec_bookkeep` (per member) and
   `exec_failstop_check` (once per unit). Queue gauges are decremented only for blocks that came over the channel,
   not for members loaded from the DB.
7. **[Cell interpretation]** See "Cell request" below.

Remaining non-production edges (unchanged, flag on only):
- Replay always passes empty `pending_slashes`, as the base binary does. A block that carried slashes live ran alone,
  but on replay it could join a unit. That restart divergence needs slashes inside the replay gap; the cell has none.
- A height cadence that is not aligned to K (for example `snapshot_interval`) can land mid-unit.
- A non-consecutive height arriving mid-collection still runs the partial unit. This is logged; strict-order dispatch
  never does it.
- A node that switches K between restarts groups differently.

## Premise check on 29bfc705 (Step 1, zero code)

Engine proxy `ubench_econ` (ub.sh: UB_MARKS=1, UB_RUNS=5, TORUS_SETTLE_STREAMED=1). K = UB_ACTIONS/60, with warm and
measured action counts held constant (UB_WARM = 2400/A, UB_MEASURE = 720/A). One engine pass over K blocks' actions
is the engine side of a K-unit. Script: `proto/exec-commit-units-tools/sweep.sh`, parser `parse.py`.

**s1, default load (600 senders), 4 interleaved rounds (1234 / 4321 / 1234 / 4321) x 5 runs per K.** Medians per
round (engine ms/1k fills):

| K | fills/blk | medians | mean of medians | engine ms/1k (all runs) | vs K1 |
|---|---|---|---|---|---|
| 1 | 11,570 | 2.85, 2.86, 2.81, 2.80 | 2.830 | 2.856 | 0 |
| 2 | 23,156 | 2.55, 2.54, 2.56, 2.58 | 2.558 | 2.565 | -10.2% |
| 3 | 34,844 | 2.42, 2.38, 2.40, 2.37 | 2.393 | 2.417 | -15.4% |
| 4 | 46,260 | 2.35, 2.36, 2.38, 2.37 | 2.365 | 2.360 | -17.4% |

- Fills per action stay at 192.7-193.6, so the work is linear in fills.
- Least-squares fit t = F + c·n per block: engine F = 8.18 ms + 2.185 ms per 1k fills (F is 25% at 11.5k fills).
  - exec F = 5.97 (c 1.864)
  - tail(liq) F = 2.23 (c 0.321)
  - margin F = 0.61
  - match F = 1.40
  - settle F = 1.71
  - r_end F = 1.59
- The tail(liq) share of the K = 2 proxy gain: tail/blk is 5.78 at K = 1 and 9.89 per 2 blocks at K = 2. That saves
  0.83 of 33.05 ms per block, about 2.5 of the 10.2 points. The proxy runs one liquidation pass per unit.

**s2, write-up settings (UB_SENDERS=5000, UB_MARK_WALK=10, UB_R_WORKER=1), 4 rounds x 3 runs per K:**

| K | medians | mean | vs K1 |
|---|---|---|---|
| 1 | 2.80, 2.79, 2.79, 2.76 | 2.785 | 0 |
| 2 | 2.32, 2.27, 2.27, 2.26 | 2.280 | -18.4% |
| 3 | 2.11, 2.06, 2.14, 2.11 | 2.105 | -24.6% |
| 4 | 1.99, 2.03, 2.08, 1.96 | 2.015 | -27.7% |

The fit here is F = 16.4 ms, of which tail F = 13.0 ms. The 2,048-account liquidation scan dominates. It amortises
only if one pass per unit is allowed, which this prototype does NOT do by default (see Design).

**s4, ABBA K1 vs K2 at default load (off,on,on,off,off,on), 3 x 5 runs per arm** (`exec-commit-units-tools/abba.sh
s4`, 09:15-10:07; the host was heavily loaded by other agents, with load average up to 448):

| round | arm | median | runs |
|---|---|---|---|
| r1 | K1 | 2.79 | 2.783, 2.834, 2.765, 2.791, 2.877 |
| r2 | K2 | 2.53 | 2.533, 2.569, 2.484, 2.554, 2.435 |
| r3 | K2 | 2.73 | **3.743, 4.082**, 2.729, 2.684, 2.499 (load average about 300 during the run: disturbed) |
| r4 | K1 | 2.83 | 2.834, 3.086, 2.902, 2.822, 2.752 |
| r5 | K1 | 2.82 | 3.123, 2.859, 2.804, 2.816, 2.697 |
| r6 | K2 | 2.54 | 2.592, 2.539, 2.490, 2.565, 2.484 |

Mean of medians: K1 2.813, K2 2.600, so the delta of means is **-7.6%**. Without the disturbed r3, K2 is 2.535
(**-9.9%**), and the median of medians is also -9.9%. The all-run mean (-4.2%) is dominated by r3's two outliers.

There was an earlier ABBA, s3 (K1 3.08 / 3.24 / 3.10 vs K2 2.63 / 2.61 / 2.65, -16.8%), but its extra env was not
recorded, so it is not used.

Premise verdict: the fixed per-block exec cost is real (F about 8 ms of 33 ms engine per block at the CL21 shape).
K = 2 clears the >= 5% gate, so Step 2 went ahead.

## Design (Step 2)

This section describes the build round (38189fe2). The fix round (above) adds: same-second runs, the
CoreWriter refusal, whole-unit replay with DB-loaded members, partial units never executed, and thread-scoped
settings.

The work is behind `TORUS_S80_EXEC_JOIN=K` (unset / 0 / 1 = off; clamped to 8). It is read once at `TorusApp`
start (`exec_join::init_from_env`), which logs `TORUS_S80_EXEC_JOIN on=<bool> k=<K> liq=<member|unit>`.

- **Unit rule (deterministic: height and content only).**
  - Units are aligned inside each 100-block segment: `[100m+1 .. 100m+K]`, `[100m+K+1 .. 100m+2K]`, and so on. The
    last unit is truncated at `100m+100`, so a checkpoint / attest height always ends a unit.
  - Inside a unit, a maximal run of *join-safe* blocks with one header timestamp (fix round) executes as one unit. A block is join-safe when it has only
    Place / PlaceBatch / Cancel / CancelAll / CancelByCloid / Modify / oracle prices / AttestStateHash /
    TransferToPerp / TransferToSpot actions, no EVM txs, no CoreWriter rows, no pending slashes, and is not an epoch
    boundary.
  - Every other block runs alone, exactly as today.
- **`execution_loop`** (`app.rs`). With K > 1, the loop takes the head message and collects the next heights of the
  head's unit from the channel. The receive is blocking, so the unit's head waits for its last member's commit.
  Collection stops at the first unsafe block. Every member's persist ticket is waited.
  - **Single block:** `exec_one` runs it (the old per-block body, factored out unchanged).
  - **Multi-block run:** `execute_joined` builds one `TorusBlock`: the last member's header, `native_action_count` =
    the unit's total, and all members' actions in height order. It runs once through `execute_committed_block_with`
    with `DurableRows{header, body}` (every member's rows are dispatch-durable; a member whose rows did not land is
    persisted first). So the unit gets one verify, replay guard, sort, oracle step, engine pass, tail, save_books,
    end_resident, flush (W), applied marker and running-hash step, all at the last height.
  - **Per-member bookkeeping after a unit:** exec queue gauges, mempool `note_exec_released`, header hook. The
    fail-stop is handled as today.
- **Liquidation cadence kept.** By default the unit runs one liquidation pass per member block (`exec_join::liq_passes`).
  This keeps the per-block scan / act / ADL budgets: a pass stops at the end of the trader set, so one pass per unit
  would halve the scan work and the liquidation cadence. `TORUS_S80_EXEC_JOIN_LIQ=unit` gives one pass per unit for
  comparison (µbench run d).
- **Invariants relaxed via `torus_state::exec_join::follows(prev, h)`.** It is `prev+1 == h`, or (join on) every
  height in `prev+1..=h` is in h's unit. It is used by:
  - running-hash `chain_step` and `ChainMemo::step` (with `reaches` for a unit that covers the activation height)
  - the resident rows slot and `rows_slot_reusable`
  - the resident books holder in `ResidentBooks` and `NativeExecContext`
  - the bl2 parent `debug_assert`
  - the exec_pipeline digest window
  - the nonce-window prune of a unit, which starts at the header before its FIRST member (thread-local
    `EXEC_JOIN_FIRST`)

  With K = 1 every helper is exactly the old rule, and `k()` stays 1 unless the env sets it, so flag-off is the
  29bfc705 code path.

## Diff summary (6 files, +1076 / -97; fix round: 3 files, +778 / -214)

Fix round (ff75d475):
- `crates/torus-state/src/exec_join.rs`: `Settings`, `scope` / `scope_k` / `Scope`, `settings`, a lazy env default;
  `set_k` / `set_liq_per_unit` removed; test `scope_is_thread_local_and_restores`.
- `crates/torus-consensus/src/exec_pipeline.rs`: the flush worker installs the spawning thread's settings.
- `crates/torus-consensus/src/app.rs`:
  - `ExecutionContext.join`
  - `EXEC_JOIN_REFUSED` and the CoreWriter refusal in `execute_committed_block_with`
  - `exec_join_same_time`, `ExecReady.queued` / `from_db`, `exec_join_load_committed`
  - `exec_bookkeep` / `exec_failstop_check`, `exec_run`, `exec_unit`
  - `exec_join_loop` (partial units left; earlier members loaded from the DB), `replay_committed_joined`
  - 3 new tests plus the reworked loop test, and the µbench through `exec_unit` with 55 ms header seconds

Build round (38189fe2):

- `crates/torus-state/src/exec_join.rs` (new, 242 lines). Contains `parse`, `init_from_env`, `k` / `set_k`,
  `unit_bounds`, `follows(_k)`, `reaches(_k)`, `liq_passes(_with)` / `set_liq_per_unit`, and 5 unit tests.
- `crates/torus-state/src/running_hash.rs`: `chain_started` plus `follows` / `reaches` in `chain_step` and
  `ChainMemo::step`.
- `crates/torus-bridge/src/native_executor.rs`: 3 resident-holder height checks now go through `follows`.
- `crates/torus-consensus/src/exec_pipeline.rs`: the digest-window gap check goes through `follows`.
- `crates/torus-consensus/src/app.rs`:
  - the `execution_loop` refactor (`exec_wait_persist` / `exec_one` / `exec_after_block` / `exec_unit`)
  - `exec_join_safe`, `exec_join_merge`, `execute_joined`
  - the liquidation pass loop, the nonce-window first height, the bl2 assert
  - `init_from_env` at exec-thread spawn
  - tests: `s80_exec_join_merge_and_safety`, `s80_exec_join_loop_is_deterministic_and_runs_the_units`, and the
    `#[ignore]` node-level µbench `s80_ubench_exec_join`

## Tests and results

### Fix round (ff75d475; debug nextest, ozarchy, under the heavy lock)

| run | result |
|---|---|
| `-p torus-state -p torus-consensus` (full, flag off) | **1055/1055 pass**, 13 skipped, 0 flaky |
| `-p torus-state -p torus-consensus -E 'test(s80_) \| test(exec_join) \| test(running_hash) \| test(replay) \| test(execution_loop) \| test(r01_)'` | **101/101 pass** (33.9 s). Includes the 4 new / reworked s80 tests, `scope_is_thread_local_and_restores`, the 6 exec_join unit tests, the running-hash tests, every replay / execution_loop / R01 restart test (flag off) |
| golden `binary(perf_equivalence_golden)`, flag off | **5/5 pass** (4.9 s) |
| golden, `TORUS_S80_EXEC_JOIN=2` (the lazy env default makes `follows` / `reaches` K = 2 in the executor; the bridge golden still runs per height) | **5/5 pass** (5.0 s) |
| RED (mutation; the new tests cannot compile on 38189fe2) | The 3 fixes were disabled in place (refusal check off, partial unit run at the close, per-block replay, `exec_join_same_time` = true), and `test(s80_exec_join)` was run: **4 of 5 FAIL**, each on its own fix. `core_writer_row_due_inside_a_unit_is_drained`: row not drained (1 vs 0). `restart_matches_the_live_run`: applied 13, expected 12 (the partial unit ran). `never_joins_across_header_seconds`: `cf_native_order_books` diverged. `merge_and_safety`: the same-time helper. The loop test passes, as expected: its fixture joins anyway. Sources were restored afterwards (`git status` clean at ff75d475). Log: `exec-commit-units-tools/logs/red-fixround.log`. RED2, with only the joined replay disabled (per-block replay): `restart_matches_the_live_run` FAILs with "the replay leaves the cut unit" (applied 13, expected 12). Log: `logs/red2-replay-fixround.log`. Restored afterwards. |

New / reworked tests (`crates/torus-consensus/src/app.rs`, s80 block of the test module):
- `s80_exec_join_loop_is_deterministic_and_runs_the_units` (reworked). The 14-block fixture has paired header
  seconds. K = 1 pipelined == serial. K = 2 forms [1,2], 3, 4, [5,6], 7..12, [13,14]. K = 2 pipelined x2 and serial
  equal the direct-unit reference: dump, hash at 14, chained, fewer native phases.
- `s80_exec_join_never_joins_across_header_seconds` (fix 3): every block has its own second, and K = 2 == K = 1 byte
  for byte, pipelined and serial.
- `s80_exec_join_restart_matches_the_live_run` (fix 2): close mid-unit, crash gap, tip mid-unit. Each equals the
  live K = 2 run.
- `s80_exec_join_core_writer_row_due_inside_a_unit_is_drained` (fix 1): K = 2 refused unit == K = 1, pipelined and
  serial; row drained and deposit credited.
- `exec_join::tests::scope_is_thread_local_and_restores` (fix 4).

Full suites, flag off, at ff75d475 (sources restored after the RED runs): `cargo nextest run -p torus-state -p
torus-consensus --no-fail-fast` gave **1055/1055 pass**, 13 skipped, 0 flaky (218.9 s). Log:
`exec-commit-units-tools/logs/full-state-consensus-fixround.log`. The flag-on full consensus suite was not run, for the
same reason as in the build round: tests that commit one block and wait for it would block on the unit's head.

### Build round (38189fe2)

Tests first: the s80 tests reference `exec_join::*`, `exec_join_merge`, `exec_join_safe` and `execute_joined`, which
do not exist on 29bfc705, so they cannot compile there. That is the RED state; it was not re-run in this session
(this stage resumed an interrupted earlier run that wrote them). Results on this branch (debug nextest, ozarchy, under
the heavy lock):

| run | result |
|---|---|
| `-p torus-state -E 'test(exec_join) \| test(running_hash)'` | 22/22 pass (before the liq change) |
| `-p torus-state` (full, after the liq change) | 423/423 pass, 5 skipped |
| `-p torus-consensus -E 'test(s80_)'` | 2/2 pass (before the liq change; also in the full run below) |
| `-p torus-consensus` (full, flag off, incl. s80 tests, after the liq change) | 624/624 pass, 8 skipped (rerun `--no-fail-fast`). The first run had 570 pass and 1 SIGKILL after 0.047 s in `da_gc::tests::a_retention_drop_never_deletes_a_body_the_tree_still_serves` (an untouched module; most likely an external kill under host memory / lock contention), and fail-fast cancelled 53 tests. That counts as flaky = 1, reported not passed. The same test passed in the rerun. |
| `-p torus-bridge` resident_books_tests + settle_streamed_tests + perf_equivalence_golden + `test(resident)` + `test(rows_slot)` | 67/67 pass |
| golden `binary(perf_equivalence_golden)`, flag off | 5/5 pass |
| golden, `TORUS_S80_EXEC_JOIN=2` | 5/5 pass |

Golden with the flag on is identical by construction. The bridge golden drives the executor per height and never
forms units: `k()` is set only by `TorusApp` start or `set_k`. The flag-on equivalence that matters is the
determinism test:

`s80_exec_join_loop_is_deterministic_and_runs_the_units` runs the 13-block pipeline fixture (orders, cancels, oracle,
claim, session, epoch boundaries every 4) through `execution_loop`.
- K = 1 serial == K = 1 pipelined (full CF dump and running hash).
- K = 2 forms the units [1,2], 3, 4, [5,6], 7..13. Units are split by unsafe blocks and the channel close.
- K = 2 pipelined, again, and serial all equal a reference that executes those units directly: CF dump, running hash
  at 13, not HASH-UNVERIFIED, and fewer native phases than K = 1.

The flag-on full consensus suite was NOT run. By design the joined loop blocks a unit's head until the next height
arrives, so a test that commits one block and waits for it to execute would hang under `TORUS_S80_EXEC_JOIN=2` while
holding the shared build lock.

## Node-level µbench (Step 2 measurement)

### Fix round (ff75d475)

Same harness. The units now go through `exec_unit`, the loop's grouping, and blocks carry 55 ms-cadence header
seconds (`S80_UB_BLOCK_MS`, default 55). At 55 ms every second boundary falls between K = 2 units, so all units join.
The whole-run counters equal the build round's exactly (K1 placed 1,565,360 / matched 1,167,100; K2 1,561,334 /
1,161,326). Logs: `results/node-ub-exec-commit-units-fix-{a,b}.log`.

Raw E-own and wall (ms per 1k fills), arms in run order:

| run (arms) | K | E-own per arm | wall per arm |
|---|---|---|---|
| fix-a (1,2,2,1,1,2) | 1 | 2.903, 3.106, 2.960 | 9.791, 10.118, 9.918 |
| fix-a | 2 | 2.585, 2.669, 2.642 | 7.322, 7.584, 7.409 |
| fix-b (2,1,1,2,2,1) | 1 | 2.869, 2.909, 2.889 | 9.805, 9.855, 9.738 |
| fix-b | 2 | 2.598, 2.601, 2.560 | 7.286, 7.299, 7.199 |

Means:

| set | K1 E-own | K2 E-own | delta | K1 wall | K2 wall | delta |
|---|---|---|---|---|---|---|
| fix-a (n = 3 / 3) | 2.990 | 2.632 | **-12.0%** | 9.942 | 7.438 | -25.2% |
| fix-b (n = 3 / 3) | 2.889 | 2.586 | **-10.5%** | 9.799 | 7.261 | -25.9% |
| both (n = 6 / 6) | 2.939 | 2.609 | **-11.2%** | 9.871 | 7.350 | -25.5% |
| both, medians | 2.906 | 2.600 | -10.5% | | | |

Notes:
- fix-a ran while the host's 5-minute load average was about 77 from other agents (`uptime` at its end). Its K1
  arm 3.106 is the outlier; without it, K1 = 2.906 (n = 5) and the delta is -10.2%.
- fix-b ran on a quiet host and reproduces the build round almost exactly (K1 2.889 vs 2.897, K2 2.586 vs 2.592).
- Phase means per 1k fills, fix-a + fix-b, K1 vs K2: verify 0.399 / 0.356 (fix-a), engine 2.062 / 1.875 (fix-a).
  This is the same per-phase picture as the build round.
- With the fixes, the exec-side gain at K = 2 is unchanged: about -10.5 to -11% E-own per 1k fills.
- In a cell about 5% of units split at second boundaries (see fix 3). That costs about 0.5 points of the gain:
  half of 10.5% on 5% of units.

### Build round (38189fe2)


`s80_ubench_exec_join` (torus-consensus lib, release, `#[ignore]`) is the full exec path on fresh DBs per arm:
- the `ubench_econ` econ load (600 senders, 300 markets, 60 actions/blk of 400-order GTC batches, cancel-all 5%,
  budget 900), signed
- 3 validators' oracle prices per block
- `execute_committed_block_with`: verify, replay guard, sort, oracle, engine, tail, save_books, end_resident
- the pipelined flush worker W, trade writer, running hash + L6.1 on, TORUS_SETTLE_STREAMED=1
- 48 warm + 48 measured blocks, the same blocks for every arm, arms ABBA in one process

Metrics:
- **wall** = the exec thread's wall per 1k fills, including the final W drain.
- **E-own** = `torus_exec_chain` minus `torus_exec_handoff_wait`, i.e. the exec thread's own time.

E-own is the representative metric. In this harness the flush worker (test DB: root 3.6 + state_write_db 5.1 ms/1k
fills, about 100 ms per block) is far slower than in the cell (state write 4.2 ms/blk). So the wall is W-bound here,
while the cell's exec thread is the ceiling. E-own at K = 1 is 2.90 ms/1k = 34 ms/blk, in line with the cell's
exec on-CPU of 31.8 + 3.0 runqueue ms/blk.

Raw (ms per 1k fills; logs `results/node-ub-exec-commit-units-{a,b,c,d}.log`, parser `exec-commit-units-tools/nparse.py`):

| run (arms) | K | wall per arm | E-own per arm |
|---|---|---|---|
| a (1,2,2,1,1,2) | 1 | 9.708, 9.925, 9.809 | 2.890, 2.909, 2.906 |
| a | 2 | 7.228, 7.192, 7.227 | 2.614, 2.575, 2.596 |
| b (2,1,1,2,2,1) | 1 | 9.746, 9.744, 9.762 | 2.895, 2.879, 2.894 |
| b | 2 | 7.339, 7.314, 7.336 | 2.597, 2.583, 2.588 |
| c (1,3,4,4,3,1) | 1 | 9.801, 9.757 | 2.908, 2.898 |
| c | 3 | 6.444, 6.267 | 2.540, 2.530 |
| c | 4 | 5.630, 5.522 | 2.829, 2.460 |
| d, `S80_UB_LIQ_UNIT=1` (1,2,2,1) | 1 | 9.757, 9.709 | 2.975, 2.896 |
| d, `S80_UB_LIQ_UNIT=1` | 2 | 7.224, 7.422 | 2.536, 2.568 |

Means:

| K | n | wall | vs K1 | E-own | vs K1 |
|---|---|---|---|---|---|
| 1 | 8 | 9.781 | | 2.897 | |
| 2 | 6 | 7.273 | -25.6% | 2.592 | **-10.5%** |
| 3 | 2 | 6.356 | -35.0% | 2.535 | -12.5% |
| 4 | 2 | 5.576 | -43.0% | 2.644 | -8.7% (one arm 2.83: noisy) |

With one liquidation pass per unit (`=unit`, run d): K2 E-own is 2.552, which is -11.9% against the 8-arm K1 mean
and -13.1% against run d's own K1 (2.935, which includes one 2.975 outlier). The liquidation step falls from 0.113
(K1) and 0.154 (K2, per member) to 0.085 ms/1k fills. At this 600-sender load the scan is small (about 1.3 ms/blk).
In the cell it is 3.65 ms/blk (tail), so `=unit` would be worth more there, but that changes the liquidation cadence.

Phase means per 1k fills, K1 vs K2:
- verify 0.377 vs 0.339
- engine 1.995 vs 1.838
- margin 0.571 vs 0.555
- match 0.183 vs 0.116
- settle 0.941 vs 0.858
- save_books 0.180 vs 0.121
- end_resident 0.430 vs 0.343
- liquidation 0.113 vs 0.154 (2 passes per unit)
- off-thread root 3.607 vs 2.790, state_write_db 5.088 vs 3.586 (the per-flush cost amortises too, which relieves
  the shared host in a cell)

Spread: E-own K1 2.879-2.909 (1.0%), K2 2.575-2.614 (1.5%). The -10.5% delta is about 7x the spread.

## Semantic delta (flag on; why "not byte-identical")

1. **Cancel hoisting widens to the unit.** The block sort puts cancels above GTCs, so a cancel in block n+1 now acts
   before block n's GTCs.
   - µbench whole-run counters, K1 vs K2 / K3 / K4:
     - placed 1,565,360 vs 1,561,334 / 1,556,852 / 1,552,756
     - matched 1,167,100 vs 1,161,326 / 1,153,535 / 1,147,107 (-0.5% / -1.2% / -1.7%)
     - rejected reason 2: 3,440 vs 7,080 / 11,358 / 14,673
     - open-limit rejects: 0 vs 386 / 590 / 1,371
   - The same K reproduces the same counters exactly, on every arm.
2. **Per-unit steps.** One oracle step (each validator submits twice per unit), one fee distribution, one
   funding / oracle mark. Since the fix round, every member has the same header timestamp, so nothing in these steps
   is time-shifted.
3. **Status attribution.** Action status, trades and the applied marker are at the unit's last height. Intermediate
   heights have no executed state of their own: RPC height attribution, receipts and the running hash exist only at
   unit ends. 100-multiples always end a unit, so attestations are unaffected.
4. **Liquidation passes run at the unit end.** With the default cadence there are K passes on the end-of-unit state,
   not one per block. `=unit`: one pass.
5. **Latency.** The unit's head waits for its last member's commit (+1 view at K = 2) whenever the exec queue drains.
   Under load the queue is p50 9 / p90 42, so this is rare, but the cell must check exec / commit p99.
6. **Non-production edges.**
   - (Fixed in the fix round) a shutdown mid-unit no longer runs the partial unit, and the replay draws the live
     units.
   - `pending_slashes` gating assumes they are deterministic. The replay passes none, as the base binary does.
   - `h % snapshot_interval` (and any other height cadence that is not a multiple of K) could fall mid-unit.

## Cell request

Use the release `torus-node` rebuilt at ff75d475 (12:02:55, md5 bcfa318148d02610a9758079265bb115) in
`/home/oz/.cargo-target-s80-exec-commit-units/release/`. bench-throughput is md5 fa8fd4bd.

**Reading the cell (review item 7).** With the join on, `exec_engine_seconds_count` (and every per-execution
histogram count) counts **units**, not blocks. summarize.py sets `nblk` from that count (summarize.py:315/392), so in
the ON arm chain_ms, pipelined_ms and the other per-block exec metrics read up to about K times higher.
- Compare instead: engine_ms/1k, user ms/1k fills, matched_s_avg, matched_s_first120, blk_s_avg, commit p50 / p99,
  exec p99.
- The ON arm also places about 0.4-0.5% fewer orders and matches about 0.5% fewer, because cancels are hoisted
  across the unit (µbench counters). That is a small bias against the ON arm in matched/s.
- A unit's head waits for the next commit when the exec queue is empty. Expect that in commit / exec p99 if the run
  is not exec-bound.


`TOOLS=best /home/oz/projects/wt/novel-s80/proto/cell.sh exec-commit-units on-1 TORUS_S80_EXEC_JOIN=2` and off arms
with no extra env (same binary).
- Check that the node log shows `TORUS_S80_EXEC_JOIN on=true k=2 liq=member` and that agreement is OK on all 3 nodes.
- Compare matched_s, blk_s, engine_ms/1k, commit p50 / p99, exec p99.
- Optional third arm: `TORUS_S80_EXEC_JOIN=3`.
