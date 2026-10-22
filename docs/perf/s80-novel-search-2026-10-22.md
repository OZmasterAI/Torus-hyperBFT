# s80: search for new, untried improvements, 15 prototypes on 29bfc705 (2026-10-22)

Owner task s80, 2026-10-22, 05:14-13:00 CEST. Every prototype is built on `29bfc705` (`perf/h2h-combined`, the best
binary). The bar is a 120 s CL21 cell on 29bfc705: **270.3k matched/s, 18.1 blk/s**. Nothing was pushed or merged,
and every branch is local. Status is as of 11:47-11:50. Several cells and review steps were still running at that
time; they are listed in section 7.

**Late cells (11:50-12:37, added by the main session at 12:38).** All ACCEPT / AGREE. The flag-on cells prove the
flag was active (startup line `on=true`). n = 1-2 per arm; the flag-off cells of this session span 252.7k-269.1k
matched/s, so none of these rows resolves a matched/s difference.

| cell | matched/s avg | first 120 s | blk/s | chain ms | eng/1k (cell) | commit p99 ms | exec p99 ms |
|---|---|---|---|---|---|---|---|
| liq-tail-offload off-1 | 252,702.5 | 257,428.0 | 21.0 | 34.83 | 2.64 | 7,145.9 | 9,666.3 |
| liq-tail-offload **on-1** | 251,652.7 | 256,429.5 | 20.8 | 33.64 | **2.53** | 6,912.7 | 9,569.6 |
| oracle-step-memo off-1 | 257,038.2 | 260,732.9 | 21.0 | 34.50 | 2.58 | 6,154.5 | 9,667.9 |
| oracle-step-memo on-1c | 256,370.8 | 257,455.4 | 20.3 | 35.59 | 2.60 | 7,159.2 | 9,704.3 |
| oracle-step-memo on-2 | 257,105.2 | 259,972.8 | 20.7 | 34.46 | 2.57 | 6,932.4 | 9,653.4 |
| users-position-key-index off-1 | 261,948.8 | 263,421.9 | 19.9 | 36.32 | 2.57 | 7,124.9 | 9,725.9 |
| keyed-local-trust-key off-1 | 263,170.0 | 264,619.9 | 21.1 | 34.29 | 2.54 | 7,575.1 | 9,660.0 |
| end-resident-gate off-1 | 252,991.3 | 258,401.1 | 21.2 | 32.51 | 2.49 | 6,827.3 | 9,687.2 |
| close-merge-removal off-1 | 260,132.4 | 261,983.4 | 19.2 | 38.15 | 2.64 | 6,401.1 | 9,770.0 |

Reading: liq-tail-offload's microbench cut shows up in the cell's own engine metric (eng/1k 2.64 -> 2.53, -4.2%;
chain 34.83 -> 33.64 ms, -3.4%) but not in matched/s (-0.4%, n=1) or blk/s. That is the work-conservation pattern
of earlier sections (s114, s124): on the standard 300-market shape a few % of exec time does not convert into
matched/s; it may on shapes where E is saturated longer (deep, larger blocks). oracle-step-memo on vs off: 256.7k
(mean of 2) vs 257.0k, no gain: park. The other candidates' flag-on cells did not run before the session end; their
off-1 rows are kept as same-binary references for whoever finishes the ABBA.

Terms used below:
- **E** is the `torus-execution` thread, the serial exec thread.
- **eng/1k** is `ubench_econ` engine ms per 1k fills; lower is better.
  - Default settings are `ub.sh`'s: 300 markets, `UB_MARKS=1`, `UB_RUNS=5`, `TORUS_SETTLE_STREAMED=1`.
  - Each invocation's value is the median of its 5 runs. Arms run in ABBA order, and an arm's value is the mean of
    its invocation medians.
- **cell** is one 120 s, 300-market CL21 cell from `proto/cell.sh`: 3 validators plus the bench on one 32-thread
  host.

## 1. Summary

- **No prototype has shown a matched/s gain in a cell yet.**
  - Cells were the bottleneck: one cell lock, three prototype workflows' agents on the host, build-lock queues of 10-20 min, and a
    disk-full event.
  - Only three prototypes have cells: thp-heap (2 + 2), oracle-step-memo (1 + 1) and liq-tail-offload (one off arm).
  - Cell-to-cell matched/s spread on this host was 4.6% between two cells of one binary and 6.5% across all
    flag-off cells, larger than most of the expected gains.
  - Exec-thread time converts to matched/s at an elasticity of 0.3 at most, and the standard shape has measured
    about 0 (s114, s124).
  - So the ranking below rests on microbenches. It says nothing yet about matched/s.
- **1. liq-tail-offload** (node-local, exact):
  - eng/1k 2.760 -> 2.410 (-12.7%); with mark walk 10, 2.983 -> 2.443 (-18.1%). The reviewer reproduced both
    (-12.7% / -18.6%).
  - The whole gain is in the liquidation tail (0.482 -> 0.154 ms/1k). The valuation work moves onto the exec pool
    (8 jobs per block per node); it is not removed.
  - Blocks that actually liquidate pay the pass on E (not mitigated).
  - The fix round has landed; the on-arm cells are queued.
- **2. exec-commit-units** (consensus-visible):
  - Running 2 queued committed blocks as one exec unit cuts E's own time from 2.897 to 2.592 ms per 1k fills (-10.5%);
    the reviewer measured -11.0%.
  - The review found 2 confirmed P1s: restart/replay divergence and stranded CoreWriter rows. Their fixes are
    committed but not verified.
  - Production needs a header flag, a P0.0 feature and an owner decision on hoisting cancels across blocks.
- **3. Two overlapping Phase-2 margin cuts** (both remove work rather than move it):
  - phase2-sender-certificate: eng/1k 2.787 -> 2.620 (-6.0%; reviewer -6.5%). It has two proven P1s (margin bypass,
    FixedPoint panic). Its ~100% pass rate exists only because every bench account holds 100M.
  - users-position-key-index: eng/1k 2.818 -> 2.695 (-4.4%; reviewer -3.2%). It passed review and is ready for a
    cell.
  - Both cut the Phase-2 position read, so their gains do not add.
- **4. passb-cothread-under-position-flush:**
  - eng/1k 2.773 -> 2.567 (-7.4%); reviewer -5.98%. Exact, and it passed review.
  - It moves settle pass B onto a co-thread rather than removing it, so node CPU rises.
  - That puts it in the same class as L2.1, which failed the owner's exec-CPU-per-fill rule and was merged only by
    exception.
- **5. Smaller exact cuts that need cells:**
  - keyed-local-trust-key cuts E verify + sort from 1.685-1.689 to 0.584-0.588 ms/blk (-1.10 ms, -65%). The estimated
    effect is +0.2..+0.7% matched/s.
  - end-resident-gate cuts the end_resident worker's critical path from 4.160 to 2.990 ms/blk (-28%). Part of that is
    drops moved after the send.
  - close-merge-removal removes the E merge at a checkpoint close: 17.05 -> 0.162 ms per close on the deep shape,
    about 0.17 ms/blk.
- **6. Stability and robustness** (keep these, and judge each on its own metric):
  - epoch-plan-logical-ready targets the deep-cell consensus timeouts. All 191 epoch-plan refusals in 17 cells came
    while E had already logically applied the needed boundary. Stalls were 0.32-3.32 s per node and height.
  - ro-basis-walk-memo cuts a 1,024-order reduce-only action from 73-107 ms to 0.47-0.49 ms at 1,000 resting orders.
  - The FOK bound (adversarial-worst-block) cuts an over-depth FOK action from 516.4 ms to 5.7 ms.
  - The margin/phantom-depth FOK walk (99-112 s per worst block) stays open. It needs a consensus work budget, which
    is an owner decision.
- **7. Killed or parked:**
  - thp-heap, killed by cells:
    - matched/s 263,183 -> 265,538 (+0.89%), inside the off arm's own 4.6% spread;
    - engine 2.625 -> 2.675 ms/1k, i.e. worse;
    - anon memory +37%, about +1.1 GB per node.
  - exec-block-arena, killed: pooled jemalloc eng/1k 2.570 -> 2.562 (-0.3%), and the review failed it.
  - cancel-all-generation-bump, killed at the probe: realistic saving -0.03..+0.28 ms/blk, below the 0.4 ms kill
    line, plus a +16..42 ms spike at each window close.
  - oracle-step-memo, leans park (on-2 / off-2 pending). It is exact and cuts the oracle step from 0.595 to 0.177
    ms/blk. But eng/1k improves only 1.05%, and the cell pair read 257,038 -> 256,371 matched/s (-0.26%, n=1 per
    arm).

## 2. Method

- **Task (owner s80).**
  - Goal: find new, untried, high-yield improvements to matched/s, blk/s, latency, p99 and deep-book stability.
  - Exclusions: nothing from the owner's s136 list (`ledger/owner-list.md`), and nothing tried before.
  - Limits: prototypes local only, nothing pushed or merged. Cells 120 s at most, small and local.
- **Step 1, ledger and maps.**
  - A ledger of about 2,400 one-line entries covering everything tried, proposed or decided. Sources: docs/plans,
    docs/perf, the roadmap, memory, and about 390 branches.
  - 7 measured subsystem maps: consensus, crypto-runtime, exec, infra, ingress-net, state, measured.
- **Step 2, ideas.** Each idea went through a novelty check against the ledger and memory, then three skeptics
  (gain/premise, safety/determinism, feasibility/novelty), then a ranking judge.
  - Round 1: 13 lenses, 44 ideas.
    - 3 killed by the novelty check and 7 by the skeptics (2 of them fatal).
    - 34 survivors, 27 after merging duplicates (`ideas/round1-ranked.md`).
  - Round 2: 8 lenses aimed at the round-1 gaps, 23 ideas.
    - 2 killed by the novelty check.
    - 21 survivors, 19 ranked (`ideas/r2-ranked.md`).
- **Step 3, prototypes.** 15 prototypes in 3 batches, all on base 29bfc705:
  - batch 1: exec-commit-units, liq-tail-offload, users-position-key-index, thp-heap, exec-block-arena;
  - batch 2: adversarial-worst-block, close-merge-removal, oracle-step-memo, end-resident-gate,
    cancel-all-generation-bump;
  - batch 3: phase2-sender-certificate, epoch-plan-logical-ready, ro-basis-walk-memo, keyed-local-trust-key,
    passb-cothread-under-position-flush. The batch-3 rule was no cells.

  Each prototype sits behind a `TORUS_S80_*` flag on a local branch `s80/<slug>`; flag off is 29bfc705 behaviour.
  The process for each was:
  1. tests first;
  2. an adversarial review, including a Codex review;
  3. one fix round;
  4. 120 s ABBA cells, where the lock allowed.
- **Why 29bfc705.**
  - It is the best binary: 120 s CL21 = 270.3k matched/s, 18.1 blk/s.
  - It is also the binary behind the 300 s 22 blk/s figure. The 9j2 300 s cells read 17.9-19.3 blk/s over their
    first 120 s, so a 120 s cell reads lower blk/s than a 300 s cell.
- **Measurement setup** (`proto/cell.sh`, `proto/ub.sh`):
  - `ub.sh` runs one `ubench_econ` invocation under the heavy build lock.
    - With `UB_MARKS=1` it tracks the cell engine. Calibration on main 2d2f3ed9: 2.78 ms/1k fills (11.3k fills/blk,
      32 ms engine per block), against the cell's 2.76 ms/1k at about 11.5k fills/blk.
    - L2.1 must be on in the microbench for 29bfc705 (`TORUS_SETTLE_STREAMED=1`). Without it the microbench reads 5-8%
      slower (2.87-3.10 vs 2.79-2.93).
  - `cell.sh` runs one 300-market, 120 s CL21 cell.
    - Config: running hash + attest, L6.1, L2.1, rate 76,000, BLOCK_CAP 400, MAX_IN_FLIGHT 4.
    - It uses `TOOLS=best`: the 29bfc705 node family with the 0f8eedb3 tools, as in 9j2.
    - Flag-off and flag-on arms of the same binary are compared ABBA, never across binaries.
- **Session incidents.**
  - A claude.ai usage-limit pause: agents stalled from about 07:30 until the reset at about 08:20.
  - /home filled up at 11:07 from agents' debug builds.
    - One cell was lost to ENOSPC (oracle-step-memo on-1).
    - Fixed by the main session at 11:10-11:12: removed the unused main-based target dir and the s80 targets'
      `debug/incremental` caches (68 MB -> 42 GB free); a disk watcher ran from then on.
  - Build-lock congestion (15-28 waiters at times) limited cells. Two cell attempts (close-merge-removal off-1,
    oracle-step-memo on-1b) were stopped by the pre-run guard because a cargo build was running outside the lock.

## 3. Results table

Rows follow the ranking in section 1. Delta is on minus off; negative is better for every metric here.

| # | prototype: change | cons.-visible | microbench, off -> on (absolute) | delta | cells (matched/s avg, blk/s) | review | verdict | next step |
|---|---|---|---|---|---|---|---|---|
| 1 | liq-tail-offload: value the liquidation window on the exec pool before the serial loop | no | eng/1k 2.760 -> 2.410; mark walk 10: 2.983 -> 2.443 | -12.7%; -18.1% | off-1: 252,702.5 / 21.0. On arm: not run yet (queued) | fix (6 issues incl. a Codex P2); fix round 147781cb landed, not re-reviewed | inconclusive, pending cells | finish the ABBA cells; compare matched/s, blk/s, liquidation step ms |
| 2 | exec-commit-units: run K=2 queued committed blocks as one exec unit | **yes** | node microbench E-own ms/1k fills 2.897 -> 2.592; eng/1k proxy 2.813 -> 2.600 | -10.5%; -7.6% | not run | fix: 2 P1 + 1 P2 (Codex, confirmed), 3 P3; fixes in ff75d475, not verified | keep-with-fixes, provisional | verify ff75d475, rebuild node, cells K=2 vs off |
| 3 | phase2-sender-certificate: per-sender margin headroom bound; certified senders skip Phase-2 work | no | eng/1k 600 senders 2.787 -> 2.620; 5,000 senders 2.320 -> 2.203 | -6.0%; -5.0% | not run | fix: 2 proven P1, 1 P2, 4 P3; no fix committed | keep-with-fixes | land the P1 fixes, re-run nextest + ABBA, then cells |
| 4 | passb-cothread-under-position-flush: settle pass B on a co-thread, overlapping the position flush | no | eng/1k 2.773 -> 2.567 | -7.4% | not run (batch rule) | pass, Codex no findings; non-blocking items | keep-with-fixes | rebuild node, 2+2 cells |
| 5 | users-position-key-index: sorted market-id keys per trader; sender record read once per fold | no | eng/1k 2.818 -> 2.695 (reviewer 2.7825 -> 2.6925) | -4.4% (reviewer -3.2%) | not run (2+2 ABBA driver queued on the cell lock since 11:26) | pass, Codex no findings; 4 minor | keep | cell pair from one binary |
| 6 | keyed-local-trust-key: SHA-256 trust-cache key, keccak(C) kept in the cache value | no | E verify + sort ms/blk 1.685-1.689 -> 0.584-0.588 | -1.10 ms (-65%) | not run (batch rule) | fix (P1: test panics in debug builds); fix round f5f55045 not recorded | keep-with-fixes (microbench only) | verify f5f55045; cells read verify ms |
| 7 | end-resident-gate: start the end_resident worker at hand-off; read the frozen state directly | no | worker timer ms/blk 4.160 -> 2.990; eng/1k 2.790 -> 2.800 | -1.17 ms (-28%); +0.4% (noise) | not run | Codex P2 (timer excludes the drops), not fixed | inconclusive, leans keep-with-fixes | fix the metric, ABBA cells |
| 8 | close-merge-removal: stack frozen sets at a checkpoint close instead of merging on E | no | E per close, deep 17.05 -> 0.162 ms; std 9.87 -> 0.163 ms; eng/1k 2.755 -> 2.770 | -99%; eng/1k +0.5% (cannot see it) | none finished (off-1 running at 11:47) | pass, Codex no findings; doc nit, L3.1 gate gap, map walk on E | inconclusive, leans keep-with-fixes | finish cells; judge on close-block upkeep and exec p99 |
| 9 | epoch-plan-logical-ready: epoch boundary proceeds once E has logically applied the previous one | no | none (ubench has no consensus code) | - | not run (batch rule) | Codex summary, no findings; no workflow review | inconclusive, leans keep (stability) | deep CL21 2+2 cells; count refusals and timeouts |
| 10 | ro-basis-walk-memo: reduce-only basis summed once per (sender, market) per call | no | RO action at R=1,000, 1 level: 106.898 -> 0.490 ms; eng/1k 2.760 -> 2.810 | -99.5%; +1.8% (noise, bench has no RO) | not run (bench has no RO orders) | Codex no issues; adversarial review partial | keep (robustness) | finish review; ship in the RO-amplifier bundle |
| 11 | adversarial-worst-block: bounded FOK pre-check, scratch copy of touched traders only | no | over-depth FOK, D=2k, K=0: 516.395 -> 5.684 ms/action; eng/1k 2.78 -> 2.805 | -98.9%; +0.9% (noise) | not run (bench sends no FOK) | none (review started 11:49) | keep, provisional | Codex review; owner decision on a work budget |
| 12 | oracle-step-memo: reuse the oracle step result while its inputs are unchanged | no | oracle step ms/blk 0.600 -> 0.180; eng/1k 2.863 -> 2.833 (reviewer) | -0.42 ms; -1.05% | off-1: 257,038.2 / 21.0; on-1c: 256,370.8 / 20.3 | pass | inconclusive, leans park | let on-2 / off-2 finish |
| 13 | thp-heap: jemalloc HPA via MALLOC_CONF, no code change | no | proxy eng/1k (jemalloc) 2.563 -> 2.507 | -2.2% | off: 257,237.5 / 19.6, 269,128.3 / 18.3; on: 265,911.1 / 20.7, 265,164.2 / 18.2 | pass (2 tool-only P2s) | **kill** | park |
| 14 | exec-block-arena: reuse per-market scratch maps instead of rebuilding them | no | eng/1k jemalloc pooled 2.570 -> 2.562; glibc 2.770 -> 2.737 | -0.3%; -1.2% | not run | **fail** (no real gain; 3 Codex P2) | **kill** | drop |
| 15 | cancel-all-generation-bump: O(1) cancel-all per (trader, book) by generation bump | no | no A/B (probe only); net E saving -0.32..+0.59 ms/blk, realistic -0.03..+0.28 | - | not run | none | **kill** | none |

## 4. Prototypes

### 4.1 liq-tail-offload

- **Origin.** Round 1 #2, liq-offE-block-start, merged from liq-block-start-snapshot-helper, lagged-liq-scan-eah and
  liq-window-prevalue-r-snapshot.
  - Only the cheaper, exact, node-local pool variant was built. It re-runs the s123-rejected
    liq-scan-parallel-prevalue at today's 9.5% step share.
  - The consensus-visible block-start / lagged arm of #2 was not built.
- **Premise.** Holds.
  - Node cost (9j2 std CL21 r2 val0): 25.4 s of liquidation step over 7,117 blocks = 3.57 ms/blk, or 1,638 traders
    per block at 2.18 us each.
  - A probe inside the pass showed that dirty traders dominate: about 14 us each at 600 senders and about 8 us at
    5,000, against about 0.7 us for clean ones.
  - Pre-valuing only the clean traders (the write-up's variant) would cover just 2-7% of the step.
  - So the design values the whole window, dirty traders included, on the exec pool from the block's own overlay. The
    pre-values are dropped at the first act.
- **Built.** `TORUS_S80_LIQ_PREVALUE=1` turns it on; `pool` runs part A only and `2` runs part B only.
  - Knobs: `_WORKERS` (default 8) and `_MIN` (default 64; smaller windows stay live).
  - A startup INFO line was added in the fix round.
- **Tests.**
  - Equivalence over 3 seeds x (node path, reference walk) x masks 1/2/3, checked block by block against flag off.
  - A fault test written red first: flag on returned a fatal error where flag off returned none. All 7 cases are now
    green.
  - torus-bridge + torus-state, flag off: 1,511 passed, 0 failed.
  - torus-bridge, flag on (`MIN=1 WORKERS=4`): 1,092 passed, 0 failed.
  - Golden + liquidation 80/80. clippy 0 errors.
- **Numbers (eng/1k).**

  | set | off | on | delta |
  |---|---|---|---|
  | round 1, walk 0 | 2.770 | 2.437 | -12.0% |
  | round 1, walk 0 (r2) | 2.777 | 2.433 | -12.4% |
  | round 1, walk 10 | 3.000 | 2.433 | -18.9% |
  | round 1, 5,000 senders | 2.293 | 1.967 | -14.2% |
  | fix-round binary, walk 0 | 2.760 | 2.410 | -12.7% |
  | fix-round binary, walk 10 | 2.983 | 2.443 | -18.1% |
  | reviewer rebuild, walk 0 | 2.787 | 2.433 | -12.7% |
  | reviewer rebuild, walk 10 | 3.057 | 2.487 | -18.6% |

  - The liquidation tail per 1k fills (fix round, 15 runs per arm) goes 0.482 -> 0.154.
  - The exec phase per 1k is unchanged (2.295 vs 2.272). Within-arm spread is about 2%.
  - Part B alone is within the spread.
  - Worker sweep: W2 2.57/2.53, W8 2.41/2.41, W16 2.37/2.45. The default of 8 was kept.
- **Cells.** Only off-1 (flag off) has run:
  - 252,702.5 matched/s average, 257,428.0 over the first 120 s, 21.0 blk/s;
  - chain 34.83 ms, eng/1k 2.64;
  - liquidation step 3.197 ms per step on val0 (val1 3.162, val2 3.181), with 0 liquidations acted.

  Against the bar that is matched/s -6.51% (average) / -6.14% (first 120 s, against the bar cell's own 274.3k first
  120 s) and blk/s +16.0%, with many agents
  on the host. There is no on-arm cell yet.
- **Review.** Verdict fix, with 6 issues, all addressed in 147781cb:
  - P2 (Codex, confirmed): the pass values accounts beyond the act cap and can latch a reader fault. That fails a
    block which commits with the flag off (a node-local fail-stop, no wrong state). Fixed, and proven by the red fault
    test.
  - The pass blocks E for its whole duration. In blocks that act early, its wall time (about 1 ms/blk at ubench
    scale) is added. Not mitigated.
  - Smaller items: the INFO line moved to startup, a doc nit, ubench provenance (rebuilt; the numbers reproduced), and
    test gaps (closed).
- **Verdict.** Inconclusive, pending cells.
  - The microbench favours keeping it, and exactness holds.
  - The report predicts +4-6% blk/s and matched/s if exec-bound.
  - If the cells confirm, the verdict becomes keep-with-fixes.
- **Caveats.**
  - The node's liquidation step is cheaper per fill than the ubench's (0.31 vs 0.49 ms/1k), so the node saving may be
    smaller.
  - The default golden (MIN 64) does not take the new path; only the MIN=1 runs do.
  - The bench cell has 0 liquidations, so the cost in blocks that liquidate will not show.
  - Three validators each put 8 pool jobs per block on 32 shared threads.
  - The work moves to the pool, and total CPU was not measured.
- **Branch.** `s80/liq-tail-offload` @ 147781cb: 130036f7 (implementation), b40f47f0 (flag test), 147781cb (fix
  round). 8 files, +729/-6 vs 29bfc705. Node md5 070f33779928ac0afa00059ae0e0f510.

### 4.2 exec-commit-units

- **Origin.** Round 1 #1 (lit-BFT lens, `ideas/r1-lit-bft.md` section 1).
- **Premise.** Holds: each block carries a real fixed exec cost.
  - ubench sweep at equal work (600 senders), eng/1k (mean of 4 invocation medians): K1 2.830, K2 2.558, K3 2.393,
    K4 2.365. On pooled runs that is -10.2% / -15.4% / -17.4%; the means shown give -9.6% / -15.5% / -16.4%.
  - Fit: engine = 8.18 ms per block + 2.185 ms per 1k fills, so the fixed part is 25% of a block at 11.5k fills.
  - About 2.5 of the 10.2 points at K2 come from amortising the liquidation tail.
  - At 5,000 senders with walk 10, K2 reads -18.4%. Most of that is the liquidation scan, which amortises only with
    one pass per unit (not the default).
- **Built.** `TORUS_S80_EXEC_JOIN=K`; unset, 0 or 1 is off, and K is clamped to 8.
  - The exec thread runs K committed, trading-only blocks as one unit. The unit gets a single verify, engine pass,
    tail, save_books, flush and hash step at its last height.
  - Sub-knob `TORUS_S80_EXEC_JOIN_LIQ=unit` runs one liquidation pass per unit. The default, `member`, runs one per
    member block.
  - Size: 38189fe2 is 6 files, +1,076/-97; the fix commit ff75d475 is 3 files, +778/-214.
- **Tests (at 38189fe2).**
  - torus-state 423/423.
  - torus-consensus, flag off: 624/624. One SIGKILL flake on the first run passed on rerun.
  - Golden 5/5 with the flag off. The "5/5 with the flag on" result tests nothing, because the golden never reads the
    flag.
  - The full consensus suite with the flag on was not run: tests that commit one block and wait would hang.
  - ff75d475 adds 4 tests; their results were not reported yet.
- **Numbers.** Node-level microbench (full exec path, release). E-own = exec chain minus hand-off wait, in ms per 1k
  fills:

  | arm | runs | mean | delta vs K1 |
  |---|---|---|---|
  | K1 | 8 (spread 2.879-2.909) | 2.897 | - |
  | K2 | 6 (spread 2.575-2.614) | 2.592 | -10.5% |
  | K3 | 2 | 2.535 | -12.5% |
  | K4 | 2 (noisy) | 2.644 | -8.7% |
  | K2, liq=unit | 2 | 2.552 | -11.9% |

  - Reviewer: node microbench 2.861 -> 2.547 (-11.0%); eng/1k proxy 2.780 -> 2.507 (-9.8%). That is about -7.8%
    without the tail amortisation, which the default does not get.
  - Builder proxy ABBA: 2.813 -> 2.600 (-7.6%).
- **Review.** Verdict fix:
  - P1 (Codex, confirmed): CoreWriter rows that fall due at a non-last member height are stranded. A trading-only cell
    does not hit this.
  - P1 (Codex and reviewer, confirmed): crash replay runs a gap's blocks one by one. Any restart whose committed tip
    falls mid-unit diverges from its peers; at K=2 that is about 1 restart in 2.
  - P2 (Codex, confirmed): the merged block uses the last member's header time, so actions at the ±60 s edge can be
    dropped or accepted wrongly.
  - P3s: K is a process-global static; the golden-on result tests nothing; fail-stop logs repeat K times.
  - Reading cells:
    - `chain_ms` and the other per-block exec metrics read about K times higher in the on arm, because the summary
      counts units as blocks. Compare engine ms/1k, user ms/1k and matched/s instead.
    - The on arm also matches about 0.3-0.5% less per action, because cancels are hoisted.
- **Verdict.** Keep-with-fixes, provisional.
  - The exec-thread gain is solid and reproduced.
  - The builder expects about +3% matched/s at 0.3 elasticity (up to +11% if fully exec-bound). That is below this
    session's cell noise.
  - This is a measurement prototype. Production needs:
    - a header flag, a P0.0 feature, and RPC/replay plumbing;
    - owner sign-off on the cancel hoist. Inside a unit, block n+1's cancels run before block n's GTC orders, which
      opens a last-look window of up to K views.
- **Caveats.**
  - The release node built at 09:04 is from 38189fe2, not from the fix.
  - At 11:47 the fix-round microbench was partial and unreported.
  - Host load average reached 448 during one ABBA set.
- **Branch.** `s80/exec-commit-units` @ ff75d475 (reviewed at 38189fe2).

### 4.3 users-position-key-index

- **Origin.** Round 1 #3, phase2-position-lookup, merged from users-position-key-index (hw-memory lens section 5) and
  phase2-run-dense-position-view.
- **Premise.** Holds, and the cost is larger than the write-up estimated.
  - In the profile, `AccountReader::get_position` is the top self symbol (3.47%) and `Users::position` takes 1.38%.
  - 76.4% of their samples sit on one dependent load in the binary search over 112-byte rows: about 3.7% of all
    process cycles.
  - `Users::get` is one map probe, not three hash passes as the write-up said.
- **Built.**
  - Each trader record gets a sorted 8-byte market-id key array, and position lookups search that array.
  - Phase 2 reads each sender's record once per fold (the hoist).
  - Flag: `TORUS_S80_POS_KEY_INDEX=1`, or `keys` / `hoist` for one part alone.
  - 3 files, +387/-34. The keys are node-local: they are not in any encoded row, gauge or digest.
- **Tests.**
  - New differential test: 6 seeds x 80 random blocks, key index on vs off, byte-equal for every trader and market.
  - A many-markets test and a flag test.
  - torus-bridge 1,092/1,092 with the flag off and on; golden 5/5 both ways; clippy clean.
  - Codex ran the lib tests in all three configurations.
- **Numbers.**

  | run | off eng/1k | on eng/1k | delta | engine ms/blk |
  |---|---|---|---|---|
  | builder, 6 per arm | 2.818 (2.77-2.92) | 2.695 (2.66-2.73) | -4.4% | 32.62 -> 31.33 |
  | reviewer, 4 per arm | 2.7825 (2.74-2.88) | 2.6925 (2.67-2.71) | -3.2% | 32.40 -> 31.50 |

  - The arms do not overlap in either run.
  - The gain lands where the profile pointed: margin 0.568 -> 0.512 ms/1k (-10%), liquidation tail 0.483 -> 0.432,
    and off E, end_resident 2.742 -> 2.548 ms/blk.
  - Attribution (not interleaved with off): keys only -3.4%, hoist only -2.3%.
- **Review.** Pass, with minor items:
  - The trader record grows from 112 to 144 B even with the flag off. With it on, each position adds 8 B, about 11 MB
    at 1.4M positions.
  - Only a debug assert keeps the keys and positions arrays in step. Production needs private fields or a release
    check.
  - The hoist has no dedicated unit test.
  - The reviewer advises planning on about 3%, not 4.4%.
- **Verdict.** Keep, and go to a cell. The builder expects +2..+3.5% matched/s if exec stays the binding stage. That
  is close to cell noise, so use repeated or interleaved cells.
- **Branch.** `s80/users-position-key-index` @ 01fce9da. Node md5 7c0b4de9a2f19edbd1318de90d7ed490.

### 4.4 phase2-sender-certificate

- **Origin.** Round 2 #1, merged from p2-headroom-certificate and phase2-order-free-certificate. Only arm H (skip by a
  per-sender bound) was built; arm B (pool evaluation) was not.
- **Premise.** Holds on the bench.
  - The margin phase is about 20% of the engine (0.55-0.60 of 2.74-2.87 ms/1k).
  - Every bench sender holds 100M. So the pass rate is 100% at 5,000 senders (37,791 of 37,791) and 99.99% at 600.
- **Built.** Before Phase 2, check per sender that the balance covers a worst-case margin bound. Certified senders
  skip the position read, projections, placement_need and excess work.
  - `TORUS_S80_P2_CERT=1` turns it on.
  - Other modes: `shadow` (exact path plus an assert), `probe` (measures the pre-pass cost) and `ub` (an unsound upper
    bound, for the bench only).
- **Tests.**
  - Near-margin fixture byte-identical off vs on. There, 55-56 senders per seed fail the bound.
  - torus-bridge 1,096 passed with the flag unset, `1` and `shadow`. Golden 5/5, though it mostly bails.
  - Not run: clippy, the workspace gate.
  - The review's two probe tests fail on a2dac3ea.
- **Numbers (eng/1k).**

  | run | senders | off | on | delta | margin ms/1k |
  |---|---|---|---|---|---|
  | builder v2 | 600 | 2.787 | 2.620 | -6.0% | 0.570 -> 0.378 |
  | builder v2 | 5,000 | 2.320 | 2.203 | -5.0% | 0.479 -> 0.361 |
  | reviewer | 600 | 2.777 | 2.597 | -6.5% | 0.566 -> 0.374 |
  | reviewer | 5,000 | 2.310 | 2.210 | -4.3% | 0.479 -> 0.362 |

  - Engine per block at 600 senders: 32.45 -> 30.49 ms.
  - The pre-pass costs +0.032-0.035 ms/1k.
  - The unsound upper bound (every plain limit order skips) reads 2.54 at 600 senders, against 2.620 for the built
    version.
  - Flag off vs 29bfc705: +0.4% (noise).
- **Review.** Verdict fix. The speedup reproduces, and flag off is identical to 29bfc705. But:
  - P1 (proven), margin bypass: the certificate sums every order in the call, including ones the exact path later
    rejects, and it does not check signs. One order with a non-positive price makes the bound negative and certifies
    the sender. In the repro an over-sized order is accepted and the available balance goes to about -14,900.
  - P1 (proven), panic: a FixedPoint subtraction in the bound panics on an extreme price (i128::MIN + 1). RPC intake
    blocks such orders but block execution does not, so a byzantine proposer could crash or fork flag-on nodes.
  - P2 (Codex): the sharded Phase-2 path never certifies, so there the flag silently does nothing. PE is off by
    default.
  - P3s:
    - a rare overflow fallback;
    - no tests at the bound;
    - a test-global counter;
    - `ub` and `shadow` must be compile-gated off validators.
- **Verdict.** Keep-with-fixes. Every kill rule in the spec passed, but the P1 fixes have not landed.
- **Caveats.**
  - The pass rate on lean real-world accounts is unmeasured.
  - It overlaps users-position-key-index: both cut the Phase-2 position read.
  - At 11:50 the worktree had 5 uncommitted modified files. A fix round may be in progress; this document did not
    check them.
- **Branch.** `s80/phase2-sender-certificate` @ a2dac3ea. Node md5 3cb731239a677b305c6ce0c3126d9301.

### 4.5 passb-cothread-under-position-flush

- **Origin.** Round 2 #6 (settle-margin-serial lens, idea 2). The nearest prior work is L2.1; this is the in-order
  variant.
- **Premise.** Holds, at about half the assumed size.
  - Only 21-44% of pass B runs after the pool has finished. The write-up assumed about 80%.
  - At 16.8k fills: pass B on E takes 5.74 ms, of which 2.54 ms is late. The position flush takes 10.67 ms.
  - At 65k fills: pass B takes 16.97 ms, 3.63 ms late. The flush takes 36.81 ms.
  - So the flush is long enough to hide pass B completely.
- **Built.** `TORUS_S80_PASSB_COTHREAD=1` runs pass B, unchanged and in market order, on one scoped co-thread. The
  co-thread overlaps E's end-of-call position flush.
  - `=2` joins before the flush instead, to measure the hand-off cost alone.
  - Needs `TORUS_SETTLE_STREAMED=1`.
  - 3 files, +884/-52.
- **Tests.**
  - settle_streamed 40/40; 53/53 with both flags on.
  - lib + 13 touched suites: 371/371, off and on.
  - Twins of 17 L2.1 scenarios under modes 1 and 2: write sequence, results, DB and state root all equal.
  - A mutation check made at least 5 tests fail.
  - Not run: the full torus-bridge suite, a debug build.
- **Numbers.**
  - Builder, load-gated, ABBA x 6: eng/1k 2.773 -> 2.567 (-7.4%), no overlap. Engine 32.19 -> 29.83 ms/blk.
    Settle 0.773 -> 0.570 ms/1k carries the whole gain.
  - Pooled rounds 1-3: 2.774 -> 2.590 (-6.6%).
  - Reviewer: 2.760 -> 2.595 (-5.98%), no overlap.
  - Settle microbench:
    - mode 1: wall -2.55 ms per call (-4.8%) at 16.8k fills and -2.83 ms (-2.5%) at 65k; E on-CPU -9.5% / -11.9%;
    - mode 2: +1.33 ms per call, which is the hand-off cost alone;
    - pass B on the co-thread takes 1.27x / 1.15x as long as it does on E.
- **Review.** Pass; Codex had no findings. Non-blocking items to fix before any merge:
  - A failed thread spawn fail-stops the node, where flag off would carry on. It needs an inline fallback.
  - One OS thread is spawned per streamed call (2 per block), and it is not counted in `spawn_count`.
  - A ~230-line copy of the streamed settle function must be deduplicated.
  - Some fault twins are missing.
- **Verdict.** Keep-with-fixes.
  - The gain is E wall time, not CPU; node CPU rises.
  - Expected in cells: blk/s +2..+6% if E paces the chain. matched/s is likely flat on the standard cell, as it was
    with L2.1.
  - The under-load (stress) criterion was not run.
- **Branch.** `s80/passb-cothread-under-position-flush` @ 768e05d6. The node must be rebuilt before a cell.

### 4.6 keyed-local-trust-key

- **Origin.** Round 2 #5 (e-verify-replay lens).
- **Premise.** Holds.
  - To build the trust-cache key and the sort hash, E makes exactly one Keccak pass over the canonical bytes C of each
    action. Verify costs 2.62 ms/blk in the 9j2 CL21 cells (s124).
  - On this host keccak-asm runs at 450 MB/s and SHA-256 (SHA-NI) at 1,676 MB/s.
  - A bench-shaped block carries 666,415 B of C: 1.48 ms to hash with Keccak, 0.40 ms with SHA-256.
- **Built (083baa6c).**
  - A node-local SHA-256 key, with keccak(C) stored in the cache value, so a warm hit skips Keccak.
  - The flag was not wired into the node at 083baa6c.
  - The later, unrecorded commit f5f55045 says it wires `TORUS_S80_LOCAL_TRUST_KEY` and adds a release re-hash guard
    (every 32 hits). This document did not verify that.
- **Tests.** Red first (14 compile errors). torus-types 139 passed; bridge ubench + golden 6/6, release only.
- **Numbers.**
  - Verify + sort ubench (pinned CPU, 12 rounds per tie share): Keccak 1.685-1.689 ms/blk -> memo 0.584-0.588 ms/blk.
    That is -1.10 ms (-65.3%) at 0, 20 and 50% ties, with a round spread of at most 0.3%.
  - Reviewer: -1.0951 ms (-65.0%).
  - Under synthetic load: -2.40 ms (-78.7%). This overstates it, because the load hurts Keccak more.
  - eng/1k 2.735 -> 2.790 (+2.0%) is noise: `ubench_econ` does not include verify.
  - Draft fix-round numbers:
    - with the default guard, the saving shrinks to -1.02..-1.07 ms;
    - a guard on every hit is slower than Keccak (+0.53..+0.55 ms);
    - with parallel keys on 4 CPUs, Keccak falls to about 0.50 ms and the memo saves only -0.19..-0.24 ms.
- **Review.** Verdict fix.
  - P1 (Codex): the new bridge test panics in debug builds. Only release builds were run, which hid it.
  - Design risk for the node arm: the consensus sort hash would be trusted from a node-local cache. A seeding bug
    could make a warm node order a block differently from a cold one, a fork that only the hash fail-stop would catch.
    The node arm needs a release check and seed-path tests.
- **Verdict.** Keep-with-fixes at the microbench level; inconclusive at node level.
  - Estimate: E on-CPU -3.6% per 1k fills, matched/s +0.2..+0.7%, blk/s +1..+2%. All of this is below cell noise, so
    judge cells on verify ms and E on-CPU, not matched/s.
  - Without SHA-NI the node must keep Keccak.
  - The cache value grows by 32 B per entry, about 2 MB at 64k entries.
- **Branch.** `s80/keyed-local-trust-key` @ f5f55045 (reviewed at 083baa6c; red tests at 69341e76).

### 4.7 end-resident-gate

- **Origin.** Round 1 #8, merged from end-resident-early-start and end-resident-gate-shrink (storage-durable lens).
  Parts (0) early start and (A) borrowed frozen delta were built. Part (B), the parallel Users re-value, was not.
- **Premise.** Holds.
  - Today the worker starts only after checkpoint upkeep. It first copies every key and value of R's 6 column families
    out of the frozen set.
  - Cell cost: the worker takes 4.07 ms/blk, and E waits 1.53 ms/blk on it.
  - ubench timer: 4.06-4.41 ms/blk = Users 2.68-2.94 + rows 0.16 + about 1.25 other (the copy, drops, carry, gauges).
- **Built.** `TORUS_S80_ER_GATE=1` starts the worker right after the pipeline hand-off and lets it read the frozen
  state directly. 10 files, +734/-27, about 450 of the added lines are tests.
- **Tests.**
  - Golden 5/5 off and on; targeted 52/52.
  - Consensus targeted 15/15 both ways, after fixing two wrong test expectations.
  - torus-consensus full, flag on: 622/623, then 16/16 after the fix.
  - torus-state + bridge full, flag on: 1,512/1,513. The one failure was a SIGSEGV during teardown of an untouched
    test at load ~460; a stress rerun passed 60/60.
  - Flag off full suites: 1,513/1,513 and 623/623.
- **Numbers.**
  - Worker timer per block (ABBA, medians of 5 runs; off n=3, on n=2 after one load-hit on round was excluded): off
    4.15-4.17 (mean 4.160) -> on 2.96-3.02 (mean 2.990). That is -1.17 ms (-28.1%).
  - Review-stage repeat: 4.2025 -> 3.027 (-28%).
  - eng/1k unchanged (2.790 -> 2.800).
  - The early start cannot be measured in the ubench.
- **Review.** Codex P2, not fixed:
  - With the flag on, the timer stops before the slot is sent and before the leftover drops.
  - So the off and on arms measure different things, and part of the -1.17 ms is drops moved after the send.
- **Verdict.** Inconclusive, pending a cell; leans keep-with-fixes. Expected +0.2..+0.9% matched/s, below cell noise.
- **Next.**
  - Fix the metric: record the total after the drops, plus a separate slot-ready metric.
  - In cells, read `torus_exec_end_resident_wait_seconds` (target <= 0.6 ms/blk, from 1.53) and checkpoint upkeep.
- **Branch.** `s80/end-resident-gate` @ f8e1ffff. Node md5 7df6cc15075651fdda7a5f848b8f62ad.

### 4.8 close-merge-removal

- **Origin.** Round 1 #6, merged from ckpt-close-stack (deep-book lens) and lazy-window-shared-cf-maps
  (storage-durable lens).
- **Premise.** Holds, but smaller than claimed.
  - At a window close, E merges the frozen sets into the checkpoint layer and clones every key and value. That costs
    17.0 ms deep, 9.9 ms std, and 29.4 ms deep at 15k positions per block.
  - At one close per 100 blocks that is -0.10 ms/blk std and -0.17..-0.29 deep. The write-up claimed -0.45..-0.55
    deep.
- **Built.** `TORUS_S80_CLOSE_STACK=1`. At a close, E stacks the frozen sets as shared references (O(1)), and the
  flush worker writes L, n-1, n in order in one batch.
- **Tests.**
  - torus-state 421/421.
  - Close-stack tests 4/4 (60 and 40 random seeds, byte-identical DBs).
  - torus-consensus 622/622 with the flag off and on.
  - Golden 5/5 off and on, from the reviewer's re-run. The builder's golden filter matched 0 tests.
- **Numbers.** Close microbench, merged -> stacked:

  | shape | E per close (ms) | W batch build (ms) | W write (ms) |
  |---|---|---|---|
  | deep | 17.05 -> 0.162 | 144.6 -> 138.6 | 442.4 -> 441.7 |
  | std | 9.87 -> 0.163 | 112.2 -> 105.8 | 292.5 -> 305.6 |
  | deep, 15k positions | 29.42 -> 0.4664 | - | 443.1 -> 466.6 |

  - The reviewer reproduced the E figure (16.98 -> 0.167 deep).
  - Reads that reach the stacked layer pay +243 ns (+38%), about 0.5 us per block on E.
  - eng/1k cannot see the change (2.755 -> 2.770, +0.5%).
- **Cells.** None finished. off-1 was running at 11:47. The first attempt was killed by a cargo build outside the
  lock.
- **Review.** Pass; Codex had no findings. Items:
  - a misplaced doc comment;
  - a missing gate: the write-up asked to stack only when L3.1 coalesce is on. With L3.1 off, the flush batch can
    carry duplicate blobs (the cost lands on W);
  - the byte count walks the frozen set on E (0.16-0.47 ms).
- **Verdict.** Inconclusive at cell level; leans keep-with-fixes.
  - The per-block gain (0.25-0.45%, per the review) is far below what a 120 s matched/s cell can resolve.
  - Judge it on close-block upkeep and exec p99, ideally on the deep shape.
- **Branch.** `s80/close-merge-removal` @ c4308d5f.

### 4.9 epoch-plan-logical-ready

- **Origin.** Round 2 #2 (deep-book-2 lens, item 1).
- **Premise.** Strongly confirmed.
  - On 29bfc705 the epoch plan for boundary H is refused while the durable applied marker is below the previous
    boundary.
  - Over 10 deep 9j2 cells and 7 s80 std cells, all 191 refusal warnings came while execution was already logically
    32-82 blocks past the needed height.
  - The gap to durable was exactly 100 blocks every time: the previous boundary's checkpoint had not landed yet.
  - Stalls per node and height: mean 1.71 s, range 0.32-3.32 s (n=69), quantized at the view timeouts.
  - Timeouts track refused heights about 1:1.

  | cells | refused heights per cell | sum of max stalls per cell (s) | timeouts per node |
  |---|---|---|---|
  | deep CL21 (4) | 3-4 | 4.32-6.70 | 2-4 |
  | deep M15 (4) | 4-12 | 8.11-23.19 | 2-10 |
  | deep R (2) | 8 | 13.10-15.92 | 6-8 |
  | std (7) | 0-3 | 0-4.36 | 0-2 |

- **Built.** `TORUS_S80_PLAN_LOGICAL=1` lets a boundary proceed once E has logically applied the previous one, reading
  the plan from an in-memory hand-off.
  - The durable-read fallback stays, and a missing plan is never read as "no change".
  - 5 files, +577/-6, in torus-consensus plus a plan accessor in torus-economics (not torus-bridge as the write-ups
    said).
- **Tests.**
  - Red, then 9/9 green. On vs off gives identical write sets, running hash, root and markers under three checkpoint
    cadences.
  - torus-economics 190/190.
  - Consensus epoch/plan/pipeline/checkpoint subset: 129/129 off and on.
  - Golden 5/5 off; it also passes on.
  - Full torus-consensus with the flag off: 630 passed (11:43). The full suite with the flag on was still pending.
- **Numbers.** No microbench applies: the ubench has no consensus code.
- **Review.** Only a one-paragraph Codex summary, with no findings. No review stage was recorded.
- **Verdict.** Inconclusive; leans keep as a stability fix.
  - Expected: blk/s up to +2..3% on deep cells, about +1% std; matched/s 0..+1%. All below cell noise.
  - Judge it on refusal warnings and timeouts.
- **Next.**
  - Deep CL21 cells, 2+2 ABBA (3+3 if the off arm averages under 1.5 refused heights).
  - Kill if the on arm shows any refusal warning, or if pooled timeouts fall by less than 80%.
- **Branch.** `s80/epoch-plan-logical-ready` @ 62373d5d.

### 4.10 ro-basis-walk-memo

- **Origin.** Round 2 #3 (settle-margin-serial lens, section 3), a robustness item.
- **Premise.** Holds. For every reduce-only order, Phase 2 walks the sender's whole resting set. Direct timing of one
  call with 1,024 RO sells (median of 11):

  | resting orders R | levels | today (ms) | memo (ms) |
  |---|---|---|---|
  | 100 | 1 | 6.309 | 0.366 |
  | 1,000 | 1 | 106.898 | 0.490 |
  | 1,000 | 50 | 72.964 | 0.471 |
  | 5,000 | 1 | 725.726 | 1.050 |
  | 5,000 | 50 | 704.305 | 1.030 |

  - The kill line was 20 ms at R=1,000.
  - At R=1,000, 64 such actions per sender per block cost 4.7-6.8 s of serial engine time today, and about 31 ms with
    the memo.
- **Built.** `TORUS_S80_RO_BASIS_MEMO=1` computes the sum of a sender's resting non-RO orders once per (sender, market)
  per call. It keeps the same iteration and saturating-sum order. 4 files, +665/-6.
- **Tests.**
  - A 300-round differential (memo == walk).
  - End-to-end, serial and 4-thread: off vs on give identical results, ids, orders and hashed rows (1,088 walks vs 1).
  - torus-core 418/418; torus-bridge with the flag on 1,094/1,094.
  - The review run: golden 5/5 and reduce_only 22 passed, off and on.
- **Numbers.**
  - End-to-end margin phase per call, off -> on:
    - R=1,000, 1 level: 91.916 -> 0.686 ms;
    - R=1,000, 50 levels: 76.610 -> 0.660 ms;
    - R=900 with a long position: 68.682 -> 0.768 ms.
  - eng/1k 2.760 -> 2.810 (+1.8%) is noise: the bench sends no RO orders.
- **Review.** Codex found no correctness issues. The adversarial review re-timed the same numbers; its ABBA and clippy
  were still waiting on the lock.
- **Verdict.** Keep as a robustness fix, not a throughput gain. Ship it in the "RO amplifier" bundle (section 5.3).
- **Branch.** `s80/ro-basis-walk-memo` @ ce73bc73.

### 4.11 adversarial-worst-block

- **Origin.** Round 1 #5, fok-precheck-bounded (merged with batch-resting-only-fok-cow). The same bench measured round
  1 #10 ro-sweep-aggregate (arm B) and round 1 #16 stop-index-cancel-paths (arm C).
- **Premise.** The attack is real and bigger than the round-1 estimate.
  - The main cost is the pre-check walk, about 250 ns per visited order with maker checks.
  - It is not the per-FOK map clone, which costs 1.6 us per FOK at S=209/K=0 and 21 us at K=10k.
  - At 29bfc705 (FOK: one market, D=2k resting, S=80-2,000):
    - over-depth FOK: 504.6-557.1 ms per 1,024-FOK action, about 25x over the 20/50 ms kill line;
    - RO sweep at R=1k: 64.8-118.0 ms per action (kill line 20 ms);
    - stop miss: 583 us at 300k stops and 2,648 us at 900k. A binary search would give 3.9 / 4.9 us.
- **Built.** `TORUS_S80_FOK_BOUND=1`:
  - The FOK pre-check rejects early when the book's quantity cannot fill the order.
  - The scratch copies only the traders it touches, instead of cloning the margin/RO maps.
  - 4 files, +968/-2.
- **Tests.**
  - A 400-scenario differential, flag off vs on, byte-identical.
  - A mutation check: 3 bugs, all caught.
  - torus-core 422/422 off and on; torus-bridge 223/223 off and on.
- **Numbers.** FOK pre-check ABBA, ms per action, off -> on:

  | shape | K=0 | K=10k |
  |---|---|---|
  | non-crossing | 1.730 -> 0.081 | 21.561 -> 0.080 |
  | thin book | 2.045 -> 0.618 | 22.105 -> 0.617 |
  | over-depth | 516.395 -> 5.684 | 547.031 -> 5.387 |
  | margin variant | 516.130 -> 525.259 (+1.8%) | 555.018 -> 559.617 (+0.8%) |

  - At D=20k the over-depth case goes 4,977.9 -> 69.9 and 5,304.9 -> 69.3 ms per action.
  - eng/1k on the v1 binary: off 2.78, on 2.805 (+0.9%), with rounds 3 and 5 excluded for load; on 6b381e04: 2.780 ->
    2.740 (-1.4%). The bench sends no FOK, so the flag is never consulted.
- **Review.** None recorded. A Codex review file appeared at 11:49 and was empty when this was written.
- **Verdict.** Keep, provisional; not merge-ready until reviewed. It does not touch the margin/phantom-depth walk.
- **Branch.** `s80/adversarial-worst-block` @ 6b381e04. The ub.sh A/B ran on the earlier v1 binary.

### 4.12 oracle-step-memo

- **Origin.** Round 1 #7, oracle-step-second-memo.
- **Premise.** Holds.
  - The real oracle step takes 0.682-0.700 ms/blk, against a hit-floor stub of 0.087-0.089. So 0.60 ms/blk is
    removable (kill line 0.3).
  - The step is about 2% of a 32 ms engine block.
- **Built.** `TORUS_S80_ORACLE_MEMO=1`. While submissions, aggregates, listed markets, stakes and the time window are
  unchanged, the next block re-puts the aggregate rows with block and time patched and reuses the mark table.
- **Tests.**
  - Equivalence over 4 seeds x 400 blocks. Removing any one of the 5 validity checks makes it fail.
  - torus-core 420/420; torus-bridge 1,091/1,091 off and on.
  - Golden passes off and on (9/9 and 7/7, counted together with the memo and ubench targets).
  - Clippy not run.
- **Numbers.**
  - Builder (beba69e0): eng/1k 2.883 -> 2.850 (-1.16%); oracle step 0.595 -> 0.177 ms/blk.
  - Reviewer (b314c9c5): eng/1k 2.863 -> 2.833 (-1.05%); oracle step 0.600 -> 0.180; engine 33.4 -> 33.0 ms/blk.
  - A hit costs 0.128-0.137 ms, a miss 0.80-0.85 ms.
  - At beba69e0, a feed in every block cost +0.57 ms/blk. At b314c9c5 that regression is gone in the ubench, where
    feeds are written before the step. The review notes the node writes feeds after the step, so a feed in most
    blocks would still cost about +0.23 ms/blk.
- **Cells.**

  | cell | matched/s avg | first 120 s | blk/s | chain ms | eng/1k | commit p99 ms | exec p99 ms |
  |---|---|---|---|---|---|---|---|
  | off-1 | 257,038.2 | 260,732.9 | 21.0 | 34.5 | 2.58 | 6,154.5 | 9,667.9 |
  | on-1c | 256,370.8 | 257,455.4 | 20.3 | 35.59 | 2.6 | 7,159.2 | 9,704.3 |

  - The hit rate in the cell was 95.6% (1,958 hits, 90 misses).
  - on-1 was lost to ENOSPC, and on-1b was aborted by the pre-run guard.
  - on-2 and off-2 were pending.
- **Review.** Pass; Codex found no issues.
  - A miss costs +0.23 ms over the plain step, so the memo loses below about a 33% hit rate.
  - A stub function sits in production code.
- **Verdict.** Inconclusive; leans park. It is exact, but the engine gain is about 1%, inside the ubench spread, and
  the cell pair (n=1 per arm) shows no gain.
- **Branch.** `s80/oracle-step-memo` @ b314c9c5. Node md5 e70f8a024b59a85877b267d726cc497d.

### 4.13 thp-heap

- **Origin.** Round 1 #4, thp-preserving-heap (hw-memory lens, section 1). Arm E was tested. Arm A (thp:always)
  matched the defaults in the proxy and was dropped.
- **Premise.** Holds. Under jemalloc defaults the node heap starts 96% huge-page backed. Within about 45 s of load it
  collapses to 27-36% (about 3.0 GB anon, 0.8-1.2 GB THP).
- **Built.** No node code. The nodes get the env
  `MALLOC_CONF=hpa:true,hpa_hugify_style:eager,hpa_purge_threshold:2097152`. Measurement tools live under
  `tools/s80/`.
- **Tests.** Golden 5/5 under four allocators: glibc, jemalloc defaults, arm E, arm A.
- **Proxy numbers.** `ubench_econ` under LD_PRELOAD jemalloc, steady config:
  - defaults 2.563 -> arm E 2.507 eng/1k (-2.2%);
  - THP share 49.3% -> 87.7%, anon memory +18%;
  - dTLB misses per kinstr -60%, user cycles -2.8%;
  - reviewer: -2.3% (-1.0% without one outlier).
- **Cells.**

  | cell | matched/s avg | first 120 s | blk/s | chain ms | eng/1k | commit p99 ms | exec p99 ms |
  |---|---|---|---|---|---|---|---|
  | off-1 | 257,237.5 | 259,059.7 | 19.6 | 36.9 | 2.62 | 6,581.0 | 9,748.1 |
  | on-1 | 265,911.1 | 269,854.0 | 20.7 | 37.3 | 2.7 | 7,363.6 | 9,671.0 |
  | on-2 | 265,164.2 | 265,073.7 | 18.2 | 40.72 | 2.65 | 7,945.8 | 9,786.8 |
  | off-2 | 269,128.3 | 269,369.9 | 18.3 | 41.16 | 2.63 | 7,673.7 | 9,842.6 |
  | **mean off** | **263,183** | 264,215 | 18.95 | 39.03 | 2.625 | 7,127 | 9,795 |
  | **mean on** | **265,538** | 267,464 | 19.45 | 39.01 | 2.675 | 7,655 | 9,729 |

  - All 4 cells: ACCEPT / AGREE.
  - THP share under load: 76.9-77.4% on vs 33.8% off.
  - Anon memory +37%, about +1.1-1.15 GB per node.
  - The jemalloc resident gauge roughly doubles: 2.96 / 3.03 GB on vs 1.62 / 1.57 GB off.
- **Review.** Pass. Two Codex P2s concern the measurement scripts only.
- **Verdict.** Kill. It is active and correct in the real node, but shows no measurable gain (n=2 per arm):
  - matched/s +0.89% sits inside the off arm's own 4.6% spread;
  - eng/1k is 1.9% worse, where the proxy predicted -2.2%;
  - user ms/1k is flat;
  - memory rises by about 1.1 GB per node.

  If ever revisited: add `background_thread:true` and run at least 3 cells per arm.
- **Branch.** `s80/thp-heap` @ 61372703 (tools only).

### 4.14 exec-block-arena

- **Origin.** Round 1 #17 (hw-memory lens, section 3), ranked "no prototype (stop)". The cheaper clear-and-reuse
  version was built, not the bump arena.
- **Premise.** Real, but small.
  - Under jemalloc (the node's allocator) the timed window takes about 205 page faults per block (17.8 per 1k fills).
    That is at most about 0.2 ms/blk.
  - Rehashing the per-batch pool maps takes 1.52% of engine cycles.
  - Ceiling: about 1.5-2% of the engine.
- **Built.** `TORUS_S80_EXEC_ARENA=1` reuses the per-market scratch maps by clearing them, instead of dropping and
  rebuilding them every batch. The maps are the plan position cache, ReduceOnly, AccountMargins and MAKER_FREE.
- **Tests.**
  - Golden 5/5 in four flag combinations; arena equivalence tests 5/5.
  - torus-core 421/421; torus-bridge 1,094/1,094 off and on.
  - A reviewer mutant (no MAKER_FREE clear) survives the tests.
- **Numbers (eng/1k).**
  - Builder:
    - glibc 2.770 -> 2.737 (-1.2%);
    - jemalloc 2.577 -> 2.553 (-0.9%);
    - jemalloc on the final binary 2.563 -> 2.570 (+0.3%);
    - pooled jemalloc 2.570 -> 2.562 (-0.3%).
  - Reviewer: jemalloc per-run means +0.64% (t=0.25); glibc -0.36%.
  - All of these are within the 3-5% run spread.
- **Review.** Fail: the code is sound, but the gain is not real. Codex P2s:
  - no budget on retained map memory (it could pin several GB);
  - no cap on MAKER_FREE;
  - plan caches are lost on some paths.
- **Verdict.** Kill. No cell.
- **Branch.** `s80/exec-block-arena` @ ea886f5a.

### 4.15 cancel-all-generation-bump

- **Origin.** Round 1 #11 (contrarian lens, idea 2).
- **Premise and probe.**
  - In resident mode the cancel-all book pass costs 1.402 ms/blk wall, about 5% of the engine.
  - 2,902 orders are cancelled per block, about 1.3 orders per (sender, book) hit.
  - Serial probe at the bench mix:
    - today: about 1.0 us per order, 2.90 ms/blk;
    - the generation bump costs 58% of that, and the per-book sum upkeep 65%.
  - Net E saving: -0.32..+0.59 ms/blk. The realistic band is -0.03..+0.28, below the 0.4 ms/blk kill line.
  - The window-close reclaim adds +16..42 ms to one block in 100 (against a ~28 ms engine block).
- **Built.** Probe and diagnostic counters only; no flag. Golden + cancel_all 29/29; the probe's books are
  byte-identical after every window.
- **Verdict.** Kill.
  - The bench cancels only about 1.3 orders per hit, so an O(1) bump removes little.
  - A full build would take 8-12 days and carry fork-class risk: about 110 order-book methods would have to filter
    stale orders.
- **Side lead.**
  - An eager batch cancel with one retain per touched level. Today the plan step costs as much as the apply (1.02 vs
    0.89 ms at 50k).
  - The ceiling is about 0.3-0.5 ms/blk.
  - Check the prior art cancel-all-in-place-compaction first.
- **Branch.** `s80/cancel-all-generation-bump` @ 89fe9dc7.

## 5. Ranked ideas not prototyped, and robustness findings

Gains here are the skeptics' corrected gains from the ranked files. "E" figures are exec-thread time. Converting them
to matched/s uses an elasticity of 0.3 at most, and the standard shape has measured about 0.

### 5.1 Round 1 (`ideas/round1-ranked.md`)

| r1 # | idea | corrected gain | conf. | effort | cons.-vis. |
|---|---|---|---|---|---|
| 2 (other arm) | liq-offE-block-start, consensus-visible block-start / one-block-lag rule | E -2.7..-3.3 ms/blk; matched/s +1.5..+2.5%; blk/s +5..+8%. The exact pool variant (4.1) now covers the dirty-trader cost node-locally | medium | 5-9 d | yes |
| 9 | serial-chain-sched-priority (FIFO / sched_ext priority for exec, hotstuff, end-resident) | exec runqueue 2.97 -> <1 ms/blk; matched/s about -1..+1.6%; mostly a co-location artefact | low | 1-2 d | no |
| 12 | classic-cold-level-rope (zero-copy Classic blob from cached cold levels) | deep %100 save 60 -> 7-12 ms; deep matched/s +0.8..+1.0% (overlaps #6); std 0 | low-medium | 6-9 d | no |
| 13 | exec-smt-core-cookie | matched/s about -1..+1.5%; can go negative | low | 0.5-1 d | no |
| 14 | mark-version-sums-prefill | 0 on every standard cell; +1..+3% only with moving marks | low | 4-6 d | no |
| 15 | phase2-order-prep-lookahead | E -0.5..-0.8 ms/blk; matched/s +0.4..+0.6% | low | 5-7 d | no |
| 18 | fixedpoint-reciprocal-division | E -0.1..-0.3 ms/blk; matched/s <+0.5% | medium | 2-3 d | no |
| 19 | resting-order-compaction (176 B -> 80-96 B) | about 0.1-0.4% exec; deep RSS -105 MB | medium | 5-8 d | no |
| 20 | gossip-pubkey-batch-msm-verify | CPU-s/1M -3.5..-4%; matched/s 0 | medium | 7-10 d | no |
| 21 | gossipsub-native-anonymous-instance | CPU-s/1M -1.2..-1.4%; matched/s 0 | medium | 4-6 d | no |
| 22 | feed-ahead-commit-prep | matched/s about 0; block p99 -1..-3% | medium | 4-6 d | no |
| 23 | lazy-commit-persist-ticket | matched/s +0.03..+0.3%; removes 10-40 E stalls per cell | high | 3-4 d | no |
| 24 | commit-inclusion-stream | node about 0; the bench-side gain is harness-only | medium | 2-3 d | no |
| 25 | book-blob-cdc-store | deep batch -45%; matched/s 0..+0.7% | medium | 4-6 d | no |
| 26 | ckpt-batch-presize | W build -20..-50 ms per checkpoint; target metrics 0 | high | 0.5 d | no |
| 27 | l3-cat-exec-partition | matched/s 0..+1%; needs root | low | 0.5 d test | no |

Round 1 #10 and #16 were measured inside the adversarial bench (section 5.3).

### 5.2 Round 2 (`ideas/r2-ranked.md`)

| r2 # | idea | corrected gain | conf. | effort | cons.-vis. |
|---|---|---|---|---|---|
| 1 (arm B) | phase2-sender-certificate arm B, market-major pool evaluation | E -3.5..-8 ms/blk; blk/s +8..+20%; matched/s -1..+3%; CPU per fill +1..+2% | medium | 8-12 d | no |
| 4 | **streamed-settle-gate-prefork** (decide "parallel" before the fork so every pool job plans) | E on-CPU -0.4..-0.9 ms/blk (-2.4..-2.8% per 1k fills); matched/s 0..+0.7%. Step 1 is a zero-build probe (`TORUS_PARALLEL_SETTLE_MIN_FILLS=0`) that was not run | medium-high | 0.5 d probe + 1-1.5 d | no |
| 7 | lazy-ro-policing | robustness (section 5.3) | medium | 2-4 d | no |
| 8 | gov-market-cancel-batch | robustness (section 5.3) | medium | 0.5-1 d | no |
| 9 | liq-stage1-running-view (Fix A) | robustness (section 5.3) | low-medium | 2-3 d | no |
| 10 | staking-reward-index (delegation half) | robustness (section 5.3) | medium (unmeasured) | 5-8 d + owner | yes |
| 11 | rule-versioned-trust-key | 0 perf; latent fork guard for a future sender-rule change; ship together with 4.6 if both land | high (mechanism) | 0.5-1 d | no |
| 12 | cancel-all-split-phase-hoist | E wall -0.35..-1.0 ms/blk; matched/s +0.1..+0.9% | low | 3-5 d | no |
| 13 | users-sharded-parallel-apply | E end_resident wait -0.45..-1.1 ms/blk; only -0.15..-0.4 beyond round 1 #8 | low-medium | 5-8 d | no |
| 14 | cow-book-level-snapshot | deep E -0.43..-0.6 ms/blk; deep matched/s +0.3..+0.45%; std about 0 or negative | low-medium | 6-10 d | no |
| 15 | users-apply-sorted-merge | E wait -0.08..-0.3 ms/blk; matched/s +0.07..+0.24% | medium | 1.5-2.5 d | no |
| 16 | cold-level-demotion | deep RSS -0.3..-0.41 GB per 1M cold orders; throughput 0..+0.5% deep | low | 8-12 d | no |
| 17 | streamed-region-caller-cpu-priority | matched/s about 0 (-1..+0.8%) | low | 0.5 d probe | no |
| 18 | storm-deferred-gossip-verify | matched/s -1.1..+0.2%; commit p99 at most -5..-10% | low | 5-8 d | no |
| 19 | causal-cancel-lane | matched/s about 0 or negative | low | 2-3 d | no |

A measurement to make before more E-time work (gap 1 in both rounds): calibrate the E-time -> matched/s elasticity per
shape with a deliberate E delay. For example, a `TORUS_S80_E_SLEEP_US` busy-spin of 1/2/4 ms per block in 2+2 cells.
That tells whether E cuts like 4.1-4.6 can move matched/s at all, or only blk/s and latency.

### 5.3 Robustness and DoS findings (one group)

Measured at 29bfc705 unless marked "estimate". The adversarial bench's worst-block figures are ms per action x 195
actions in one market; they are not measured blocks.

| attack (source) | today | worst block today | fix | after fix |
|---|---|---|---|---|
| FOK over-depth, D=2k (r1 #5, 4.11) | 504.6-557.1 ms per action | 102-107 s | FOK bound, built | 5.4-5.7 ms per action, about 1 s per block |
| FOK over-depth, D=20k (4.11) | 4,977.9-5,304.9 ms per action | 971-1,034 s | FOK bound, built | 69.3-69.9 ms per action |
| FOK clone shapes (4.11) | 21.6-22.1 ms per action at K=10k | 4.2-6.4 s | FOK bound, built | about 0.12 s per block |
| FOK margin / phantom depth, D=2k (4.11) | 507.3-572.4 ms per action | 99-112 s | **none** (the bound does not apply) | unchanged |
| RO sweep, R=1k (r1 #10, 4.11 arm B) | 64.8-118.0 ms per action | 22-23 s | ro-sweep-aggregate, not built | - |
| RO basis walk, R=1,000 (r2 #3, 4.10) | 73-107 ms per 1,024-order action | 4.7-6.8 s per attacking sender | memo, built | 0.47-0.49 ms per action |
| stop miss, 300k / 900k stops (r1 #16, 4.11 arm C) | 583 / 2,648 us per miss | - | binary search, not built (~5 lines) | 3.9 / 4.9 us |
| RO owners policed per match call, K=100k (r2 #7) | estimate: +20-35 ms per match call, +25-100 ms per single-path placement | estimate: 200 CoreWriter rows +5-20 s | lazy policing, not built | O(senders + touched owners) |
| governance cancel on a deep market (r2 #8) | estimate: about 10 s (s63 shape); 10-20 min at 1M adversarial orders | - | reuse cancel_all_many, not built | estimate 0.1-0.3 s; < 1 s |
| liquidation stage-1 re-view (r2 #9) | estimate: worst case 0.45-1.9 s per 64-act block | - | running view, not built | - |
| delegations at an epoch block, 300k (r2 #10) | estimate: +0.7-1.5 s per epoch block | - | reward index, not built, consensus-visible | - |

The skeptics named more linear sites that nothing covers yet (r2 appendix B item 5):
- trigger_stops, which is O(S);
- open_order_count, which scans all stops;
- cancel_order's shift on deep levels;
- every user cancel-all, which scans all stops.

**Worst-block budget note.**
- Point fixes do not close this class.
  - After the FOK bound, the margin/phantom-depth FOK walk still costs 99-112 s per worst block at D=2k.
  - Every new O(n) site an attacker can grow adds another row to the table above.
- The class-wide fix is a deterministic work budget. Two options:
  - R16 option B: W=16,384 visits per action, plus a per-market per-block budget;
  - a rule that batch items are GTC/ALO only.
- Both options are consensus-visible and need P0.0 activation, so this is an owner decision.
- The reduce-only sites (r2 #3 and #7, r1 #5 and #10) are one attack class. Ship them as one "RO amplifier" PR series;
  fixing one leaves the rest open.

## 6. Side findings

- **The microbench tracks the cell engine, but on glibc.**
  - `ub.sh` reads 2.78 ms/1k at 11.3k fills/blk, against the cell's 2.76 at about 11.5k.
  - It runs the test binary on glibc, while the node runs jemalloc. Under LD_PRELOAD jemalloc the same ubench reads
    2.53-2.61 (exec-block-arena premise: 2.53 on jemalloc vs 2.85 on glibc, -11%).
  - The allocator-sensitive prototypes (thp-heap, exec-block-arena) were measured both ways. The thp-heap proxy gain
    (-2.2%) did not carry into cells, where engine time was 1.9% worse.
  - Treat proxy results for allocator policy as weak.
- **L2.1 config gap.**
  - On 29bfc705, L2.1 is opt-in (`TORUS_SETTLE_STREAMED=1`; the CL21 cells set it). On main it is on by default.
  - Without it, the 29bfc705 microbench reads 5-8% slower.
  - With L2.1 on in both, the main 2d2f3ed9 and 29bfc705 engines were within about 2% (noise).
  - Any microbench or test of the 29bfc705 family must set the flag explicitly. passb-cothread acts only when it is
    on.
- **Main vs the bar.** A batch-10 main (2d2f3ed9) 120 s cell read 250.3k matched/s and 17.4 blk/s (n=1, host busy with
  agents), against 270.3k / 18.1 for 29bfc705. With the engines within 2%, one cell under load cannot say whether the
  gap is real.
- **Host contention dominated the cells.**
  - The agents of three prototype workflows shared the 32-thread host with the cells.
  - The thp-heap off arm runs 29bfc705 node code, and it read 257,237.5 and 269,128.3 matched/s in two cells, 4.6%
    apart.
  - Off arms across prototypes read 252.7k-269.1k matched/s and 18.3-21.0 blk/s. So blk/s above the 18.1 bar is not a
    gain by itself.
  - ubench run-to-run spread was 3-5%. Load spikes (load average up to about 460) forced some runs to be excluded, and
    heavy-lock queues reached 15-28 waiters.
- **Elasticity is the open question.** The ranking files convert E time to matched/s at 0.3 at most, and the standard
  shape has measured about 0. Most surviving prototypes cut E time. A 2+2 E-delay cell set (section 5.2) would show
  how much of the 3-13% engine cuts can reach matched/s at all.
- **Moving vs removing work.**
  - liq-tail-offload and passb-cothread move work from E to other threads. So did L2.1, which failed the owner's
    exec-CPU-per-fill rule and was merged only by exception.
  - users-position-key-index, phase2-sender-certificate, keyed-local-trust-key, oracle-step-memo and
    exec-commit-units remove work.
- **Golden filter trap.** `nextest -E 'test(perf_equivalence_golden)'` matches 0 tests (exit 4); use
  `binary(perf_equivalence_golden)` instead. The s80 prototype spec had the wrong filter. Because of it,
  close-merge-removal's builder golden runs were vacuous, and the reviewer re-ran them.
- **Harness note for joined blocks.** The cell summary takes the block count from the engine-call counter. So for
  exec-commit-units, `chain_ms` and the other per-block exec metrics read about K times higher in the on arm.
- **Epoch-plan refusals explain the deep-cell timeouts**, independently of the prototype. In 17 cells, every refusal
  came while E was logically ready and the durable marker lagged by exactly one 100-block checkpoint. Timeouts track
  refused heights about 1:1 (section 4.9).
- **Write-up corrections from the premise checks.**
  - Pre-valuing only clean traders would cover 2-7% of the liquidation step (4.1).
  - The late share of pass B is 21-44%, not about 80% (4.5).
  - The FOK cost is the walk, not the clone (4.11).
  - The close merge saves -0.17..-0.29 ms/blk deep, not -0.45..-0.55 (4.8).
  - The cancel-all bench mix cancels about 1.3 orders per hit (4.15).

## 7. Where everything is

Nothing is pushed. All branches are local in the Torus-hyperBFT repo. Each prototype has a worktree at
`/home/oz/projects/wt/novel-s80/proto/<slug>` and a target dir at `~/.cargo-target-s80-<slug>`.

| prototype | branch @ head | flag |
|---|---|---|
| liq-tail-offload | `s80/liq-tail-offload` @ 147781cb | `TORUS_S80_LIQ_PREVALUE` |
| exec-commit-units | `s80/exec-commit-units` @ ff75d475 (reviewed 38189fe2) | `TORUS_S80_EXEC_JOIN` |
| phase2-sender-certificate | `s80/phase2-sender-certificate` @ a2dac3ea (+ uncommitted edits at 11:50) | `TORUS_S80_P2_CERT` |
| passb-cothread-under-position-flush | `s80/passb-cothread-under-position-flush` @ 768e05d6 | `TORUS_S80_PASSB_COTHREAD` |
| users-position-key-index | `s80/users-position-key-index` @ 01fce9da | `TORUS_S80_POS_KEY_INDEX` |
| keyed-local-trust-key | `s80/keyed-local-trust-key` @ f5f55045 (reviewed 083baa6c) | `TORUS_S80_LOCAL_TRUST_KEY` |
| end-resident-gate | `s80/end-resident-gate` @ f8e1ffff | `TORUS_S80_ER_GATE` |
| close-merge-removal | `s80/close-merge-removal` @ c4308d5f | `TORUS_S80_CLOSE_STACK` |
| epoch-plan-logical-ready | `s80/epoch-plan-logical-ready` @ 62373d5d | `TORUS_S80_PLAN_LOGICAL` |
| ro-basis-walk-memo | `s80/ro-basis-walk-memo` @ ce73bc73 | `TORUS_S80_RO_BASIS_MEMO` |
| adversarial-worst-block | `s80/adversarial-worst-block` @ 6b381e04 | `TORUS_S80_FOK_BOUND` |
| oracle-step-memo | `s80/oracle-step-memo` @ b314c9c5 | `TORUS_S80_ORACLE_MEMO` |
| thp-heap | `s80/thp-heap` @ 61372703 (tools only) | `MALLOC_CONF=...` (env) |
| exec-block-arena | `s80/exec-block-arena` @ ea886f5a | `TORUS_S80_EXEC_ARENA` |
| cancel-all-generation-bump | `s80/cancel-all-generation-bump` @ 89fe9dc7 (probe only) | none |

Base worktrees under `/home/oz/projects/wt/novel-s80/proto/`:
- `best`: the 29bfc705 binary with the 0f8eedb3 tools, i.e. the bar;
- `h2h`: 29bfc705 rebuilt, the prototype base;
- `base`: main 2d2f3ed9, reference only.

Files:
- Runners: `/home/oz/projects/wt/novel-s80/proto/cell.sh` and `/home/oz/projects/wt/novel-s80/proto/ub.sh`.
- Prototype reports and Codex review files: `/home/oz/projects/wt/novel-s80/proto/reports/<slug>.md` and
  `<slug>-codex.txt`.
- Stage records: `/home/oz/projects/wt/novel-s80/proto/reports/_workflow-results.json`. It lags the branches in
  several places, noted per prototype above.
- Results: `/home/oz/projects/wt/novel-s80/proto/results/`, with `ub-<slug>-<label>.log` per microbench invocation and
  `s80-<slug>-<round>/summary.json` per cell.
- Ledger, maps and ideas: `/home/oz/projects/wt/novel-s80/{ledger,map,ideas}/`.
- Appendix in this repo: `docs/perf/s80-novel-search/`, containing `ledger/`, `map/`, `ideas/` (lens files and ranked
  lists) and `prototypes/` (`cell.sh`, `ub.sh`) and `prototypes/reports/` (the 15 prototype reports, Codex review outputs and `_workflow-results.json`, copied by the main session at 12:40).

Still running or open at 11:47-11:50:
- liq-tail-offload: the cell driver (pid 3665814) was waiting on the cell lock for on-1, then on-2 and off-2.
- oracle-step-memo: `/tmp/osm-cells/run6.sh` (`osm-abba-driver.service`, log `/tmp/osm-cells/run6.log`) for on-2 and
  off-2.
- close-merge-removal: `cmr-cell-ab.sh`, with off-1 running, then on-1, on-2, off-2.
- users-position-key-index: a 2+2 ABBA cell driver (started 11:26, log `proto/reports/cells-upki/run.log`) waiting on
  the cell lock for off-1.
- exec-commit-units: fix-round verification of ff75d475 (node microbench, golden, release rebuild).
- epoch-plan-logical-ready: the full torus-consensus suite with the flag on (`pipeline3.sh`), and ubench rv-on-1.
- ro-basis-walk-memo: the review ABBA (`rbwm-rv/ub-rest.sh`) and clippy, waiting on the heavy lock.
- adversarial-worst-block: a Codex review started at 11:49.
- keyed-local-trust-key: fix round f5f55045, with draft reports in `proto/results/klk-fix/`; not recorded or
  reviewed.
- Disk:
  - free `~/.cargo-target-s80-epoch-plan-logical-ready/debug` (about 12 GB);
  - reviewer mutation scratch is still in `proto/results/eba-review/mut/src` and `~/.cargo-target-s80-eba-review-mut`.
