# s80 prototype: epoch-plan-logical-ready (`TORUS_S80_PLAN_LOGICAL`)

Branch `s80/epoch-plan-logical-ready` (worktree `/home/oz/projects/wt/novel-s80/proto/epoch-plan-logical-ready`,
base 29bfc705), target dir `/home/oz/.cargo-target-s80-epoch-plan-logical-ready`. Local only: nothing pushed.
I ran no cells (session rule for this batch).

## 1. Premise check on 29bfc705

### Where the code is
The r2 write-ups cite `crates/torus-bridge/src/app.rs`. On 29bfc705 the code is in
`crates/torus-consensus/src/app.rs`:

- `TorusApp::epoch_plan` (`app.rs:9105` before the change) refuses boundary `H` while the **durable** applied marker
  is below `H - L`. It logs WARN `epoch boundary: execution has not applied the previous boundary yet`.
  - A refusing leader proposes nothing (`build_proposal_on`, `refused_proposal()`).
  - A refusing follower answers `MissingData` (validate, `:9707`).
- `refresh_upcoming_evm` (`:10472`) has the same durable gate.
- E's logical height is `ExecutionContext.exec_applied` (`:1668`). It is stored at `attach_flush_worker` (`:3563`) and
  at `pipeline_handoff` (`:3702`). `TorusApp` cannot see it.
- The plan row `epoch_vset:plan:<H+L>` is written by `EpochManager::execute_planned_rotation`
  (`torus-economics/src/epoch_plan.rs`), called from `process_epoch_boundary` (`torus-bridge/src/native_executor.rs:15985`)
  inside the boundary block's overlay.
- The epoch fast path is on by default (`TORUS_EPOCH_FAST_PATH`), so a boundary is pipelined. Its plan becomes durable
  only when the boundary's own close lands (`checkpoint_upkeep` `due`: `Attest` at `%100`, `Epoch` otherwise).

### Measured (zero build)
Script: `/home/oz/projects/wt/novel-s80/proto/results/eplr/refusals.py`. Raw per-cell output:
`/home/oz/projects/wt/novel-s80/proto/results/eplr/out-*.txt`.

For every refusal WARN in `val{0,1,2}.log.gz`, the script records:
- the durable `applied` value and `needed` from the WARN;
- the execution thread's **logical** height: the last `execution pipeline: block done height=` before the WARN in the
  same log;
- the stall: time from the first WARN for `H` to `on_committed_block: sending ... height=H` on that node.

Timeouts are the min-to-max delta of `torus_consensus_timeout_total_total` per node in `sampler.csv`.

Result: **191 of 191 refusal WARNs had execution logically at or past `needed`.**
- Logical minus needed: 32 / 50 / 82 blocks (min / median / max).
- Needed minus durable: exactly 100 blocks in every case. The previous boundary's checkpoint had not landed yet.
- With the flag, every one of these refusals is answered `Ok` from memory. The boundary was pipelined, so its plan
  was published before its hand-off.

| cell (bench wall) | refused heights | sum of max stall per height | timeouts per node |
|---|---|---|---|
| 9j2-deep-r1-CL21 (302 s) | 3 | 6.38 s | 3/3/3 |
| 9j2-deep-r2-CL21 | 3 | 4.32 s | 2/2/2 |
| 9j2-deep-r3-CL21 | 4 | 5.75 s | 4/4/4 |
| 9j2-deep-r4-CL21 | 3 | 6.70 s | 3/3/3 |
| 9j2-deep-r1-M15 | 6 | 12.06 s | 6/6/6 |
| 9j2-deep-r2-M15 | 5 | 8.11 s | 2/3/3 |
| 9j2-deep-r3-M15 | 4 | 8.21 s | 4/4/4 |
| 9j2-deep-r4-M15 | 12 | 23.19 s | 10/10/10 |
| 9j2-deep-r1-R | 8 | 13.10 s | 6/6/6 |
| 9j2-deep-r2-R | 8 | 15.92 s | 8/8/8 |
| s80-base-r0 (std, 122 s) | 3 | 3.77 s | 1/1/1 |
| s80-base-b10r1 (std, main) | 0 | 0 | 0/0/0 |
| s80-base-b10r2 (std, main) | 3 | 4.36 s | 1/2/2 |
| s80-thp-heap-off-1 (std) | 1 | 0.51 s | 0/0/0 |
| s80-thp-heap-off-2 (std) | 0 | 0 | 0/0/0 |
| s80-thp-heap-on-1 (std) | 1 | 2.53 s | 1/1/1 |
| s80-thp-heap-on-2 (std) | 1 | 0.51 s | 0/0/0 |

- Stalls per (node, height), n = 69: mean 1.71 s, median 1.50 s, range 0.32-3.32 s. They are quantized at about 0.5,
  1.3 and 2.5 s (view timeouts).
- Timeouts per node track refused heights about 1:1. With n = 3 the quorum needs every node, so one refusing node
  stalls the view.

Example (deep-r1-CL21, val0):
- Boundary 1200 executes at 20:14:29.83-29.92 (pipelined).
- At 30.56, `produce_block` for 1300 refuses: durable=1100, needed=1200, and E is executing 1243.
- Checkpoint 1200 (298 MB batch, 747 ms write) lands at 31.07, but the view only recovers at 32.99.
- 1300 commits at 33.32, a 2.76 s stall.
- E still had about 57 blocks of backlog during the stall. Most of the cost is consensus time and timeouts, not
  E idle time. That fits the r2 correction: matched/s 0..+0.8%, blk/s deep +1..+3%.

**The premise holds:** the refusal class exists on the 29bfc705 binary in deep and std cells, and the flag removes all
of it.

## 2. Design (as built)

New module `crates/torus-consensus/src/plan_handoff.rs`, with `PlanHandoff { logical: AtomicU64, plans:
Mutex<BTreeMap<u64, Vec<u8>>> }`.
- The flag `TORUS_S80_PLAN_LOGICAL` is read once in `TorusApp::try_new_with_manifest_db`. `1`/`true` turns it on;
  unset or anything else is off.
- Startup log: INFO `TORUS_S80_PLAN_LOGICAL on=<bool>`.
- One `Arc` is shared by `ExecutionContext.plan_handoff` and `TorusApp.plan_handoff`. Off means `None`, which is
  exactly today's behaviour.

### E side (`app.rs`)
- **Read.** After the block's last fatal-error check (the book save's `take_fatal_error`) and before the
  pipelined/serial split, a boundary block reads back the plan row it wrote for `H + L` through its own overlay. This
  is the exact byte string the flush or checkpoint will write. A read error only loses the shortcut.
- **Pipelined.** `publish_plan` runs before `pipeline_handoff(Job::Flush ..)`. `pipeline_handoff` then publishes the
  logical height right after `exec_applied.store`. A failed hand-off retracts the entry; that node is fail-stopping
  anyway.
- **Serial** (barrier boundary: EVM or slash). The plan is published after the successful flush, when it is already
  durable. This keeps the map an exact mirror of the DB rows.
- **Pruning** mirrors the DB delete of `plan(X - L)` at the execution of `X`. Entries are kept until `H + L` has
  executed.
- `attach_flush_worker` seeds the logical height at the durable marker. After a restart the map is empty, so the DB
  path is used.

### Consensus side
`PlanHandoff::source(boundary, needed, durable)`:
- If `max(durable, logical) < needed`, the plan is not ready. This case logs the same WARN text as today, plus
  `logical`.
- Otherwise the map is read first: a hit is `Memory(bytes)`, decoded with the new
  `torus_economics::decode_epoch_rotation_plan`, the same borsh decode as `epoch_rotation_plan`.
- On a map miss with `durable >= needed`, it is `Db`: today's DB read.
- On a map miss with only logical readiness, it is **not ready**, with a separate WARN. A missing plan is never read
  as "no change" (the bug-b2 safety rule from the spec).

`refresh_upcoming_evm` uses the same rule. Not ready means it keeps predicting from the committed values, as today.

### Safety argument
- The plan bytes are the row the same block writes, read back from the same pending set, so they are identical to the
  durable row. The tests check this byte for byte after the checkpoint.
- Readiness changes only in timing. A node now votes or proposes on boundary `H` once its execution has logically
  applied `H - L`. If it crashes before the checkpoint, replay recomputes the same plan deterministically, and the
  running-hash fail-stop guards divergence.
- Memory ordering: the plan is inserted before the SeqCst store of the logical height. Consensus loads the height
  first, then locks the map. So a ready height always sees its entry.
- Node-local: no consensus-visible change.

## 3. Diff summary
- `crates/torus-consensus/src/plan_handoff.rs` (new, about 210 lines including 6 unit tests).
- `crates/torus-consensus/src/lib.rs` (`pub mod plan_handoff`).
- `crates/torus-consensus/src/app.rs`:
  - two struct fields and their construction;
  - E-side read, publish and set_logical (4 sites);
  - `epoch_plan` and `refresh_upcoming_evm`;
  - test helper `fast_path_run_plan`;
  - 2 tests.
- `crates/torus-economics/src/epoch_plan.rs` and `lib.rs`: public `epoch_plan_key` and `decode_epoch_rotation_plan`,
  plus 1 test.

Commit: `62373d5d` on `s80/epoch-plan-logical-ready`. rustfmt: my hunks are clean. The app.rs diff count with
standalone rustfmt is 440, the same as base.

## 4. Tests (tests first; logs in `/home/oz/projects/wt/novel-s80/proto/results/eplr/`)

### RED, then GREEN
RED (`red.log`) is the build with the fields added but nothing wired. Both new app.rs tests failed with `logical 0`
(left 0, right 3 and right 13). GREEN (`green.log`) after wiring: **9/9 passed**:
- 6 `plan_handoff` unit tests: parse default off; (a) map hit; (b) neither ready; map miss with durable behind is
  not ready; (c) restart reads the DB; pruning mirrors the DB.
- `s80_plan_logical_durable_lags_map_hit_equals_db_plan`. Epoch length 4, W parked on boundary 4's own checkpoint,
  so logical = 4 and durable < 4.
  - (b) before block 4: `Err`.
  - Flag off: `Err`, and the leader proposes nothing for block 8.
  - Logical 4 with an empty map: `Err` (never "no change").
  - Flag on: `Ok(plan)`, `boundary_evm` is `Ok`, and the leader proposes block 8.
  - After release and checkpoint, the DB row bytes equal the hand-off bytes, and the decoded plans are equal.
  - (c) a fresh app with an empty hand-off reads the same plan from the DB.
- `s80_plan_logical_on_matches_off_and_mirrors_the_db_rows`. Flag on vs off: identical per-block consensus write
  sets, running hash, native root, every CF and the durable markers.
  - Checkpoint policies: per-block, never, and every 5.
  - Also with a serial EVM boundary 8 and a serial slash boundary 12.
  - The map holds exactly the DB's plan rows (12, 16; 8 pruned in both) with the same bytes. Logical height = 13.
- `torus-economics` `s80_plan_key_and_decode_match_epoch_rotation_plan`.

### Touched crates
- `torus-economics`, full, flag off (`econ-off.log`): **190/190**.
- `torus-consensus`, the epoch/plan/pipeline subset (`cons-sub-*.log`), 129 tests, filter `test(epoch) |
  test(fast_path) | test(boundary) | test(plan) | test(exec_pipeline) | test(checkpoint) | test(refresh_upcoming) |
  test(rotation) | test(validator_set)`:
  - flag off: **129/129**;
  - `TORUS_S80_PLAN_LOGICAL=1`: **129/129**. With the env set, every `TorusApp::new` and its exec thread run with the
    hand-off on.
- `torus-consensus` full (637 tests), flag off and on: still waiting on the build lock when this report was written
  (11:22). They are queued in the detached `pipeline3.sh`: see `pipeline3.status`, `cons-full-off.log`,
  `cons-full-on.log`.
- `perf_equivalence_golden` (`golden-*.log`): flag off **5/5**; flag on was queued in `pipeline3.sh` (`golden-on.log`)
  at report time.
  - torus-bridge does not link the flag code. The only change it sees is the two new pub fns in torus-economics.
- Release `torus-node` for cells: queued in `pipeline3.sh` (`node-build.log`, status line `node-build rc=`). Check it
  before scheduling cells.
- Not run: clippy, and the full workspace gate (time box and lock queue).

### Incident
My first golden attempt used `-E 'binary(perf_equivalence_golden)'`. That builds every torus-bridge test binary in
debug, and `/home` hit 100%: the link failed with `mold: Disk full`.
- I stopped my pipelines and posted to the `#torus-hyperbft` chat channel.
- I did not delete anything: the NO DESTROY gate blocked removing my own regenerable incremental cache.
- Space was freed by someone else. I reran with `--test perf_equivalence_golden`.
- My target dir's `debug/` is about 12 GB and can go when this branch is done.

## 5. Microbench
**Not applicable.**
- The engine proxy `ubench_econ` links no torus-consensus code, so the flag cannot change it. An ABBA there would
  measure noise.
- Added work on E with the flag on:
  - per hand-off: one `Option` check and one SeqCst store;
  - per epoch (every 100 blocks): one overlay point read of the plan row (about 1 KB) and one map insert.
- Flag off: one `Option` check per hand-off and per `epoch_plan` call.
- The target metric (refused boundaries and consensus timeouts) is only visible in a cell.

### Evidence without cells (section 1, plus E idle inside the stalls)
Script `idle.py`, output `idle-*.txt`. For each refused height it measures, on the refusing node, the stall window time
not covered by `executing finalized block -> block done`:

| cell | sum of stalls | E idle inside the stalls |
|---|---|---|
| deep-r1-CL21 | 6.38 s | 3.94 s (all 3 refusals within the first 120 s: bench heights 1140-3157) |
| deep-r2-CL21 | 4.64 s | 2.25 s |
| deep-r3-CL21 | 5.75 s | 3.28 s |
| deep-r4-CL21 | 6.70 s | 3.40 s (all 3 within the first 120 s: 1104-3179) |
| deep-r1-R | 13.10 s | 4.92 s |
| deep-r2-R | 15.92 s | 8.21 s |
| s80-base-r0 (std 120 s) | 4.33 s | 1.33 s |
| s80-base-b10r2 (std 120 s, main) | 4.36 s | 2.90 s |

- The commit frontier trails the proposal by the 3-chain. So during a stall, E drains its backlog and then idles for
  0.2-2.0 s per refusal.
- Deep CL21 refusals concentrate in the first 120 s. A 120 s deep CL21 cell loses about 2.3-3.9 s of E time, 2-3% of
  the window.
- **Expected with the flag:**
  - refusal WARNs and the timeouts they cause go to 0 (the code path is removed for every observed case);
  - executed blk/s up to +2..3% in 120 s deep cells and +1..2% in std cells that have refusals;
  - matched/s 0..+1%, because load-bound mempool backlog partly refills the bigger next blocks.

## 6. Cell plan (decisive; for the main session; I ran none)
- Binary: `/home/oz/.cargo-target-s80-epoch-plan-logical-ready/release/torus-node` (built by `pipeline3.sh`; confirm `node-build rc=0` in `pipeline3.status`).
  `bench-throughput` is the copied h2h one (md5 fa8fd4bd).
- Deep CL21, 120 s, ABBA, 2+2 (3+3 if the off arm averages < 1.5 refused heights):
  - `PRELOAD=1000000 BULK_COUNT=1000000 TOOLS=best cell.sh epoch-plan-logical-ready off-1 TORUS_S80_PLAN_LOGICAL=0`
  - `... on-1 TORUS_S80_PLAN_LOGICAL=1`, then `on-2`, then `off-2`.
- Std CL21 (no PRELOAD) is optional. 0-3 refusals per cell, so it is less decisive.
- Read:
  - `zgrep -c 'has not applied the previous boundary' val*.log.gz`: expect 0 on;
  - `zgrep -c 'its plan is not in the hand-off'`: expect 0;
  - `zgrep 'TORUS_S80_PLAN_LOGICAL' val0.log.gz`: `on=true` / `on=false`;
  - `torus_consensus_timeout_total_total` delta per node from `sampler.csv` (tmo.py or refusals.py): off ~2-4, on ~0;
  - `idle.py` (E idle in stalls);
  - blk_s_avg, matched_s_avg, matched_s_first120, agreement, commit p99, RSS max.
- **Kill if:** the on arm shows any refusal WARN, or pooled timeouts fall less than 80%. Keep it as a stability fix
  even if blk/s is flat.

## 7. Verdict
**Built, tests green, mechanism proven offline; the throughput gain is modest.**
- The refusal path the flag removes is the cause of every refusal observed on 29bfc705: 191/191 WARNs had execution
  logically ready.
- Each refusal is a 0.3-3.3 s consensus stall and about one timeout per node. In 120 s cells it includes 0.2-2.0 s
  of E idle.
- The change is node-local and the plan bytes are byte-identical (tested). The bug-b2 rule holds: a missing plan is
  never read as "no change".
- Expected blk/s gain: up to +2..3% on 120 s deep cells, about 1% on std. That is at or below the cell spread. The
  firm win is consensus timeouts going to about 0.
- cell_worthy = false per this batch's session rule. The timeout metric is only visible in a cell; the plan above
  makes it decisive with 2+2 deep cells.
