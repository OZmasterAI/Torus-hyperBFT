# r2 lens `pool-roundtrip`: exec-pool fork-join round trips on the exec thread (novel-s80, round 2)

Base: main `2d2f3ed9` (batch 10 merged: L2.1 streamed settle is default-on). Read-only. Numbers come from
`map/measured.md`, the 9j2 cells (`~/bench-results-matched/ozarchy-9j2-{std,deep}-r*-{CL21,R}/summary.json`,
`metrics-after-val0.txt`), RD s92 / s114 / s119, q8re W1 (`ozarchy-q8re-run/analysis/...-r1-ONP-w1.json`), and the
memories 0baab823, 04cdadb4, 9d5da33a, 8978b391 and 12d0cc8f. E = the `torus-execution` thread.

## What the lens premise looks like on the current binary (my reads)

- **The historical 16.9-18.9 ms/blk of pool waits (s92) no longer applies.** In 9j2 std CL21 r2 val0, E's off-CPU
  sleep inside a block is block 37.94 - on-CPU 31.59 - runq 2.97 = about 3.4 ms/blk. Of that, end_resident_wait is
  1.49 and checkpoint settle 0.17, which leaves about 1.7-2 ms/blk of pool waits.
- **Two populations of blocks.** Only 2,896 of 7,117 blocks have a matching call (`torus_exec_phase_match_seconds_count`).
  About 60% of blocks are cancel-only (~24 cancel-alls plus the oracle). There are 24.2 cancel-alls per block on
  average, so the cancel-all pool round trip runs in almost every block.
- **What remains of each round trip per block:**
  - Cancel-all (`run_jobs` via `ThreadPool::scope`): phase 1 takes 2.21-2.29 ms/blk on std and 2.52-2.54 on deep.
    About 81% of it is wait (8u: 0.145 off-CPU vs 0.033 on-CPU ms per 1k fills).
  - The streamed L2.1 region: 6.37 ms of "match" per call, that is the head wait plus the per-market prologues.
    `settle_pass_a` (plan_wait) is 0.90-1.03 ms/blk.
  - Parks: 3,056 parks in 7,117 blocks (0.43/blk, about 1 per streamed call).
- **L2.1 made E share CPUs with its own pool.** CL21 vs R:
  - E runq 2.80-3.09 vs 1.61-1.66 ms/blk;
  - timeslices 17.3-17.9 vs 12.1-12.8 per block.

  Parks cannot explain this (0.43/blk, ≤0.02 ms each per s119). On a quiet host (s119), the streamed path with 32
  workers costs E 2.4-2.7 ms of runq per 65k call. With 17 workers it costs 0.06 ms, and with L2.1 off 0.03 ms.

Ledger greps (all `ledger/*.md`, plus the round-1 idea files):
- `in_place_scope|caller|participat|fork-join|round.?trip`
- `pool|rayon|worker|spin|park|wake|steal|chunk|LPT|grain|scope|futex|yield`
- `gate|min.?fills|1024|parallel.?settle|plan.?wait|speculative read`
- `pre-?wake|standby|warm.?pool|help.?while|wait.?fill|lookahead|next block`
- `hoist|launch|split.?phase|async|non-?blocking|early.?start`
- `nice|renice|SCHED_|start_handler|MATCH_WORKERS|worker.?cap|affinit|ccd|autogroup`
- `prologue|epilogue|absorb|pnl|pass.?b|net-balance`

---

## 1. `streamed-settle-gate-prefork`: decide the parallel settle before the streamed fork, so no settle plan runs on E

**Mechanism.**
- **Today.** Under L2.1 a pool job computes its market's settle plan only once the fills matched so far reach the
  C3 gate: `let plan = settle_gate(matched).then(|| plan_of(&mbr))` (`native_executor.rs:10639`). The gate is
  `TORUS_PARALLEL_SETTLE_MIN_FILLS`, default 1024 (`:651-659`, `:9574`).
- **Effect.** Jobs finish roughly in market order. So the first ~9-12 markets of every placement call (1024 / ~110
  fills per market) return no plan. E then computes those plans serially: `:10698-10705` ("exec-thread pass A
  time") and `:10755-10766`, booked as `settle_pass_a`.
- **Why the gate exists.** It was a break-even heuristic for a separate pass-A round trip (`:643-650`: "determinism
  holds at any size"). Under streaming the jobs exist anyway, so the gate only moves plan work onto E.
- **Change.**
  - When `streamed` (`:9587`), decide "parallel" before the fork: always, or on ≥64 prepared orders.
  - Set `decided` at item 0, and every job computes its plan.
  - The state is the same (C3 byte-identity) and nothing is read speculatively. The flag-off path takes the same
    rule so the fault-parity tests keep matching.

**Target metric.** E on-CPU per 1k fills (the owner's merge rule), exec block ms, matched/s.

**Expected gain.**
- **Size today.** `settle_pass_a_ms` in CL21 is 0.90-0.96 (std r2), 0.91-0.94 (std r3) and 0.99-1.03 (deep r2)
  ms/blk. That is ≈2.25 ms per placement call (×7,005/2,896), or ~0.2 ms per plan. For comparison, the R arm's
  pool pass-A wall is 2.32-2.5 ms/blk.
- **E saving.** -0.6..-1.0 ms/blk. That is -2..-3% of E's on-CPU (30.6-31.6) and -1.6..-2.6% of the block
  (37-38 ms).
- **matched/s.** +0.5..+0.8% at the s67.4 elasticity.
- **Merge rule.** It cancels most of L2.1's +1.7-3.7% E-CPU cost (s119), which is why L2.1 failed the merge rule
  (s114 deep 1.044).
- **Pool side.** About 2 ms more pool CPU per call, about +0.2 ms of head. Fine, because the pool stays ahead
  (0.43 parks/blk).

**Evidence.** The code lines above. 9j2 `summary.json` `.phase_by_node.valN.phases.engine.settle_pass_a_ms` (all 3
nodes, std r2/r3, deep r2). `torus_exec_settle_streamed_parks_total` is 3,056 against 7,117 native blocks.

**Nearest prior.**
- `parallel-settle-pass-ab` / `c3-parallel-settle`: the 1024 gate, merged before L2.1.
- `parallel-engine-phase2-sharding`: PE lowered the gate to 64 as a side effect. It is excluded and was never
  analysed for L2.1.
- `streamed-in-order-settle-l21`, `l21-batched-wakeups`, `l21-microbench-quiet-host`.

No entry notes that, under L2.1, the fill gate puts ~10 markets' plans per call on E. The production change is
decision timing. The zero-build probe (`MIN_FILLS=0`) is only a knob value.

**Consensus-visible.** No.

**Risks.**
- The flag-on/off fault-parity seams (`settle_streamed_tests`, `parallel_settle_tests`) must take the same rule.
- Calls with ≥2 markets but only a few fills now settle in parallel. With streaming that costs nothing; with the
  flag off it is a pass-A round trip, so apply the rule only when streamed.
- plan_wait may hold a few post-decision parks; these are bounded by 0.43 parks/blk.
- It touches the L2.1 region (batch 10, merged). It is a follow-up, not batch-10 work.

**Effort.** 0.5 d for the probe; 1-1.5 d for production.

**Prototype (≤ 1.5 h, no code for the probe).**
1. ubench, ABAB ×3: `ub.sh h2h g1024-N` vs `ub.sh h2h g0-N TORUS_PARALLEL_SETTLE_MIN_FILLS=0`, with defaults
   `UB_MARKETS=300 UB_MARKS=1 TORUS_SETTLE_STREAMED=1`. Read engine ms/1k fills and settle / pass-A ms per call.
2. Cells, 2+2 ABBA: `TOOLS=best cell.sh best g0-{1,2} TORUS_PARALLEL_SETTLE_MIN_FILLS=0` vs `g1024-{1,2}`. Read:
   - `settle_pass_a_ms` (expect 0.9 → ≤0.2);
   - `sched_by_node` E on-CPU per 1k fills;
   - `block_ms`, `matched_s_first120`, agreement.
3. Kill if `settle_pass_a_ms` is not below 0.3 ms/blk, or E on-CPU/1k does not fall by at least 1.5%.

---

## 2. `cancel-all-split-phase-hoist`: launch the block's cancel-all book pass right after the sort, join it in Phase 1

**Mechanism.**
- **Today.** The cancel-all book pass is a blocking fork-join inside `execute_batch(pre)` (`app.rs:5997`):
  `exec_cancel_all_run` `NE:14030` → `exec_cancel_all_run_parallel` `:14149` → `run_jobs` `:14352`, which uses
  `ThreadPool::scope`, i.e. an install hop onto one worker.
- **What runs before it.** It starts only after:
  - `begin_resident`, which waits for end_resident (`app.rs:5804`);
  - `new_env`;
  - `begin_block_oracle` (`app.rs:5982`).
- **None of these touch the run's books or senders.**
  - `begin_resident` handles rows only.
  - The oracle reads only `order_books.keys()` (`NE:15909`).
  - The EVM section has already run (`app.rs:~5428`).
- **Change.**
  1. Right after the sort and the holder lock (`app.rs:5779-5787`), check whether the pre list starts with a
     CancelAll run (≥2 actions, resident books, holder not rebuilt).
  2. If so, take the run's index entries and books out of the `ResidentBooks` holder.
  3. Spawn `take_pending_stops` + `cancel_all_many` per market group as owned jobs, injected from E.
  4. `new_env` and the mark table get the list of lent markets.
  5. Phase 1 joins, computes the margin terms (0.3% of the work) and applies results in run order, as today.

**Target metric.** Exec block ms (phase 1 wait), native blk/s, matched/s. Cancel-only blocks (60% of all blocks)
get shorter.

**Expected gain.**
- **Wait today.** Phase 1 is 2.21-2.29 ms/blk on std and 2.52-2.54 on deep, ~81% of it wait, so ≈1.8-2.0 ms/blk.
  It occurs in nearly every block.
- **Overlap window.** end_resident wait 1.46-1.68 ms/blk (mean; lumpy) + oracle step 0.68-0.83 + load_books 0.14.
- **Hidden.** Central ~1.0 ms/blk (0.7-1.6). That is E wall -1.8..-4.2%, so matched/s +0.5..+1.3%.
- **Stage 2 (optional).** Verify and replay-guard the run's small cancel actions first. That adds ~3 ms of window,
  for a ceiling of about -1.9 ms/blk.

**Evidence.**
- Pool start skew: p50 3.4-4.0 / p90 7.8-8.6 ms (mem 0baab823).
- E→pool handoff: 0.4-0.8 ms (mem 9d5da33a).
- s92 cancel-all shape: ~5 jobs, 3.2-3.6 busy, lead 13-15%, 0-1 workers busy for 32-36% of the wait.
- Book removal is 99.3% of the cost (`cancel-all-diag-attribution`).
- 24.2 cancel-alls per block (9j2 `.cancel_all`).
- 57-60% of blocks never reach Phase 2 (`r1-contrarian`).

**Nearest prior.**
- `parallel-cancel-all` (PC: the pass itself).
- `pool-wait-cuts` / `pool-wait-to-slowest-job` P4 (fold into another round trip, or raise the gate).
- round-1 #11 `cancel-all-generation-bump` (removes the pass).
- `exec-pool-caller-participation` (in_place_scope removes the start hop; reused here).
- `exec-prep-stage` (rejected: N+1 verify overlapping end_resident) and `overlap-drain-next-frontend`.

The difference: the same pass is neither fused nor shrunk. It starts 1-2.5 ms earlier and overlaps E's own
pre-engine serial work and the end_resident wait (split-phase fork, deferred join).

**Consensus-visible.** No.

**Risks.**
- **Lent books.** `new_env` must not rebuild a lent book.
- **Mark table.** `fill_block_marks` must see the lent keys, or the mark table changes and the nodes diverge.
- **When to gate off.** Gate it off for:
  - a cold or rebuilt holder;
  - the non-resident path;
  - any pre-list action ahead of the run.
- **CPU contention with the end_resident worker.** It can lengthen end_resident wait, which is zero-sum on a
  saturated host.
- **Round-1 #7.** If oracle-step-memo lands, it shrinks the window by about 0.5 ms.

**Effort.** 3-5 d.

**Prototype (≤ 3 h).**
1. Build variant B1 behind `TORUS_S80_CANCEL_HOIST=1` in `proto/h2h`:
   - when Phase 1 opens with a parallel cancel-all run, run its book pass in a `pool().in_place_scope`;
   - the scope body runs `begin_block_oracle`, moved from `app.rs:5982` into a ctx hook, with the full book key set
     captured before lending.

   This measures the oracle-only window.
2. Run `perf_equivalence_golden` and the flag on/off digest equality.
3. Cells, 2+2 ABBA (120 s each). Read:
   - `phase1_actions_ms`, `block_ms`;
   - E runq;
   - `matched_s_first120`, agreement.
4. Kill unless phase 1 falls by at least 0.5 ms/blk and `block_ms` by at least 0.5 ms/blk.

---

## 3. `streamed-region-caller-cpu-priority`: stop E being time-sliced by its own 32-worker pool during the L2.1 region

**Mechanism.**
- **Today.** `run_streamed` (`exec_pool.rs:240-317`) spawns `resolve_worker_cap_named(None)` jobs (`NE:9655`),
  which is 32, the host's parallelism. E runs pass B in the same scope. That makes 33 runnable threads for 32
  hardware threads even on a quiet host, so E is preempted at slice ends by its own workers.
- **Change** (node-local, no state effect):
  - (a) In the pool's `start_handler` (`exec_pool.rs:36-41`), give the exec-pool threads a lower CFS weight:
    `setpriority` +3..+5, which is unprivileged, or SCHED_BATCH, which also drops their wakeup preemption. E then
    wins any CPU it shares with its pool, while the pool still fills every idle CPU.
  - (b) Streamed regions spawn at most `available_parallelism - 1` jobs.
  - (c) An optional arm: spin-free idle for the exec pool. Rayon's 33 yield rounds after a pool phase keep up to
    32 spinners runnable just as E resumes serial work. In q8re, the exec pool's idle/steal frames are 2,491 of
    39,661 user samples (6.3%).

**Target metric.** E runq ms/blk, exec block ms, matched/s; also production (one node per host) per-call wall.

**Expected gain.**
- **Quiet host (s119.3).**
  - L2.1 on: runq 0.78-0.90 ms/call at 16.8k and 2.39-2.70 at 65k; with L2.1 off, 0.01-0.03.
  - 17 workers: runq 0.06 and wall 111.46 vs 113.57 ms (-1.9%), but parked 5.4 vs 3.9 ms because the narrow pool
    is slower.
  - Expected from (a): about -0.7..-2.6 ms runq per call, with no loss of pool width.
- **Bench node.** CL21 vs R: runq +1.2..+1.4 ms/blk and +5 slices/blk, while parks are only 0.43/blk. Recovering
  30-70% of that gives E -0.4..-1.0 ms/blk, so matched/s +0.3..+0.8%. It can turn negative if lower-weight workers
  ramp up more slowly (a longer head wait or cancel-all pass).

**Evidence.**
- s119.3 table (Q0 / Q17).
- 9j2 `sched_by_node`: std CL21 rq 2.80-3.09 and slices 17.3-17.9; R rq 1.61-1.66 and slices 12.1-12.8; deep
  3.25-3.27 vs 2.14-2.31.
- s114: rq per 1k fills ×1.88 std, ×2.11 deep, with L2.1 alone.
- s119: parks cost ≤0.02 ms each.

**Nearest prior.**
- `bulk-thread-nice` and `renice-sched-batch`. Both were rejected, both de-prioritized verify, rpc and rocksdb
  pools, and both rested on a BORE premise that was refuted (mem 12d0cc8f).
- round-1 #9 `serial-chain-sched-priority`. Its SCHED_IDLE arm spares the exec pool explicitly; its other arms are
  FIFO and slice tuning.
- round-1 #13 core cookie.
- `exec-worker-count-knobs` and `match-settle-worker-caps` (static caps, never A/B'd).
- `verify-pools-parking-executor` (spin-free verify pools).

The difference: it targets E's own pool, the competitor that L2.1 created. The premise is measured 33-on-32
time-slicing on a quiet host, not the BORE burst penalty, and it keeps the pool's width.

**Consensus-visible.** No.

**Risks.**
- The family has been rejected twice.
- `RLIMIT_NICE=0`, so a niced thread can never be un-niced. Only SCHED_BATCH can be toggled per region, at
  32 syscalls.
- On the 3-node host, niced pools lose to verify and tokio threads, which slows the region's ramp and the
  cancel-all pass.
- It is Linux-only.

**Effort.** 0.5 d for the probe; 1.5-2 d for production.

**Prototype (≤ 2.5 h).**
1. Quiet host, s119 harness: `cargo test --release -p torus-bridge --test ubench_settle_streamed` in `proto/h2h`
   (`UBENCH_L22=on`, 16.8k and 65k), run at nice 0.
   - A sidecar polls `/proc/<pid>/task` every 50 ms and renices or `chrt -b`s the `torus-exec-pool-*` threads.
   - Arms: none, nice+5, SCHED_BATCH, and `TORUS_MATCH_WORKERS=31`.
   - Read exec wall, on-CPU, runq, parks and parked per call.
   - Kill unless runq falls by at least 50% and wall by at least 1%.
2. If it passes: 2+2 cells with the sidecar on all 3 nodes (zero build). Read:
   - E runq and slices per block;
   - `phase_match_ms`, `phase1_actions_ms`, `block_ms`;
   - `matched_s_first120`.

---

## Considered and dropped

- **Pre-wake / armed standby workers before the region fork.**
  - Novel in the ledger, but the head wait is better removed by `exec-pool-caller-participation`: E runs item 0
    itself and has no handoff. That idea is proposed-not-done.
  - Pre-wake would burn about 8 workers × ~1-3 ms of spin per call on a saturated host.
- **E fills pool waits with block N+1's verify / sort keys.** Round 1 refuted this as `exec-prep-stage` /
  `exec-3stage-pipeline`, and L6.3 `lookahead-verify-stage` is proposed.
- **`run_jobs` through `in_place_scope`, to drop the install hop and E→pool handoff of 0.4-0.8 ms.** This is
  already in `exec-pool-caller-participation` (mem 9d5da33a: "caller removes start handoff only"). It is reused
  inside idea 2.
- **Moving `absorb_matched`'s RO/AM clears and metrics into the job.** Covered by `match-epilogue-batched-trader-index`
  (one entry is proposed, one rejected).
- **Pass B per-trader PnL netting.** Covered by `net-balance-per-trader` (proposed).
- **Anticipatory wake of E plus a bounded MONITORX/MWAITX wait (the host has `mwaitx`).**
  - It saves ~0.3-0.5 ms per handoff at ~1.4 handoffs/blk.
  - It adds E on-CPU (it fails the merge rule).
  - Ideas 1 and 2 plus caller participation remove most of those handoffs anyway.
- **One priority pool shared by exec and verify jobs.** Rayon has no priorities. Merging FIFO verify queues with
  exec jobs repeats the s353 starvation (`ingress-verify-dedicated-pool`, `naive-rayon-par-verify`).
- **Adaptive per-call job count from load.** Round 1 called this "worker caps under another name"
  (r1-tail-sched).
