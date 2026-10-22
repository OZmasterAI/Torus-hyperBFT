# novel-s80 round 2: ranked survivors (ranking judge)

Inputs: the 21 round-2 survivors, each with its novelty check and three skeptic verdicts (gain, safety,
feasibility), plus the 2 killed ideas. Every gain below is the skeptics' corrected gain, never the author's
claim. Where the skeptics disagree, the range spans them and leans to the refuting verdict. Two duplicate pairs
are merged, so 21 survivors become 19 ranked entries:
- p2-headroom-certificate + phase2-order-free-certificate -> **phase2-sender-certificate**
- book-cow-snapshot-encode + cow-book-snapshot-window-save -> **cow-book-level-snapshot**

**Ranking key:** expected yield on the target metrics x confidence / effort. The target metrics are matched/s,
native blk/s, order/commit latency, block p99, deep-book stability, and robustness/security. Ties go to the
higher potential yield.

**Conversion rule** (same as round 1):
- Exec-thread (E) time saved converts to matched/s at elasticity <= 0.3 (RD s67.4: block_ms -10% gave
  matched/s +3.0% and blk/s +9.5%).
- Later evidence is lower. s114 and s124 showed about 0 on the standard (load-bound) shape; the deep shape is
  about 0.26.
- An E on-CPU cut that removes work counts for more than one that only moves the work to another thread. The
  owner's merge rule is exec CPU per fill, and L2.1 was merged only by exception after failing it.

**Code facts checked by the judge (h2h = 29bfc705 worktree):**
- `ubench_econ` calls `NativeExecutor::execute_batch`, which runs `SettleMode::Auto`
  (`native_executor.rs:8890-8900`). So the zero-build MIN_FILLS probe for #4 is valid in `ub.sh`.
- `ubench_settle_streamed` forces parallel settle, so it cannot test #4.
- `cancel_all_many_timing_probe_deep_levels` exists as an `#[ignore]` test at
  `crates/torus-core/src/order_book/cancel_batch/many_tests.rs:479`, which #8 extends.
- `exec_hash_once_ubench.rs`, `ubench_settle_streamed.rs`, `ubench_epoch.rs`, `ubench_adl.rs` and
  `reduce_only_tests.rs` all exist in `crates/torus-bridge/tests/`.
- The r1 #5 adversarial harness lives in `proto/adversarial-worst-block` as
  `crates/torus-core/tests/adversarial_match_ubench.rs`, uncommitted.
- Deep cells: `cell.sh` passes the caller's env through `detach.sh`. So `PRELOAD=1000000 BULK_COUNT=1000000
  TOOLS=best cell.sh ...` gives the deep shape; the preload adds several minutes per cell.

## Summary table

| # | slug (merged from) | corrected gain (skeptic-based) | conf | effort | cons.-vis. | proto |
|---|---|---|---|---|---|---|
| 1 | phase2-sender-certificate (p2-headroom-certificate, phase2-order-free-certificate) | Arm H (skip by bound): E on-CPU -1.0..-3.5 ms/blk, matched/s 0..+2.5% (central ~+1%), blk/s +4..+9%. Arm B (pool, PE-like): E -3.5..-8 ms/blk, blk/s +8..+20%, matched/s -1..+3%, CPU/fill +1..+2% | medium | 5-7 d (H), 8-12 d (B) | no | YES |
| 2 | epoch-plan-logical-ready | consensus timeouts -> ~0 (deep CL21 ~3 stalls of 2.3-3.2 s per cell); deep blk/s +1..+3% (deep R +3..+7%); matched/s 0..+0.8%; the +9..+50% "regime" claim is refuted | high (timeouts) / low (blk/s) | 2-3 d | no | YES |
| 3 | ro-basis-walk-memo | robustness: 82-154 ms of E per 1,024-order RO action (R=1,000) -> <1 ms; 5-26 s/blk per attacking sender removed; bench 0; close the class together with r1 #10 | high | 0.5-1 d | no | YES |
| 4 | streamed-settle-gate-prefork | E on-CPU -0.4..-0.9 ms/blk (-2.4..-2.8% per 1k fills); E block -0.5..-0.8 ms; matched/s 0..+0.7% | medium-high | 0.5 d probe + 1-1.5 d | no | YES |
| 5 | keyed-local-trust-key | E verify on-CPU -1.0..-1.2 ms/blk (SHA-NI); E on-CPU/1k fills -3.5..-5%; E wall -0.3..-0.8; matched/s +0.2..+0.7%; CPU-s/1M -0.8..-1.5% | medium (CPU) / low (matched/s) | 2 d | no | YES |
| 6 | passb-cothread-under-position-flush | E wall -1.5..-2.5 ms/blk; E on-CPU/fill -6..-8% (moved to a co-thread); blk/s +2..+6%; matched/s ~0 std, 0..+2% deep; node CPU +0.5..+1% | medium | 4-6 d | no | YES |
| 7 | lazy-ro-policing | robustness: K=100k RO owners: +20-35 ms per match call, +25-100 ms per single-path placement (200 CoreWriter rows +5-20 s; stop cascades N x 25-100 ms); bench 0 | medium | 2-4 d | no | YES |
| 8 | gov-market-cancel-batch (gov-market-wipe) | robustness: governance cancel on a deep market ~10 s (extrapolated) -> 0.1-0.3 s via the merged cancel_all_many; adversarial 1M orders 10-20 min -> <1 s; throughput and p99 0 | medium | 0.5-1 d (batch reuse); +1.5-2 d drain | no | YES |
| 9 | liq-stage1-running-view (Fix A only) | robustness in shock blocks: worst case -0.45..-1.9 s per 64-act block; liquid books -0.1..-0.2 s; realistic P~6: <3 ms; bench 0 | low-medium | 2-3 d | no | no |
| 10 | staking-reward-index (delegation half only) | robustness: adversarial 300k delegations +0.7-1.5 s per epoch block (0.4-0.9 s beyond scan-index); organic 50k ~0.14 s/epoch; bench 0 | medium (unmeasured) | 5-8 d + owner | yes | no |
| 11 | rule-versioned-trust-key | 0 perf; latent fork guard for a future height-gated sender-rule change | high (mechanism) / latent value | 0.5-1 d | no | no |
| 12 | cancel-all-split-phase-hoist | E wall -0.35..-1.0 ms/blk; matched/s +0.1..+0.9% (central ~+0.3%) | low | 3-5 d | no | no |
| 13 | users-sharded-parallel-apply | E end_resident wait -0.45..-1.1 ms/blk; matched/s +0.35..+0.85%; only -0.15..-0.4 ms beyond full r1 #8 | low-medium | 5-8 d | no | no |
| 14 | cow-book-level-snapshot (book-cow-snapshot-encode, cow-book-snapshot-window-save) | deep E -0.43..-0.6 ms/blk; deep matched/s +0.3..+0.45% (+0.2..+0.4 over r1 #6); std ~0 or negative; %100 E spike -55..-60 ms | low-medium | 6-10 d | no | no |
| 15 | users-apply-sorted-merge | users apply -6..-20%; E wait -0.08..-0.3 ms/blk; matched/s +0.07..+0.24% | medium (small) | 1.5-2.5 d | no | no |
| 16 | cold-level-demotion | deep RSS -0.3..-0.41 GB per 1M cold orders; throughput 0..+0.5% deep, 0 std; boot <=-1 s | low | 8-12 d | no | no |
| 17 | streamed-region-caller-cpu-priority | matched/s ~0 (-1..+0.8%); quiet-host wall <=-1.5..-2.1% per call, same as the existing TORUS_MATCH_WORKERS=17 knob | low | 0.5 d probe, 1.5-2 d | no | no |
| 18 | storm-deferred-gossip-verify | matched/s -1.1..+0.2%; phase-A host relief 1.2-2.4 cores; commit p99 at most -5..-10%; exec p99 ~0 | low | 5-8 d | no | no |
| 19 | causal-cancel-lane | mean exec age 0..-0.07 s; p99 unchanged; matched/s ~0 or negative (the gate changes the workload and wipes requotes) | low | 2-3 d | no | no |

**Prototype budget note.** The eight prototypes share one host.
- Microbench and test gates run first, under `~/projects/wt/.cargo-heavy.lock`. Cells serialize on
  `proto/.cell.lock`, and only run after a microbench gate passes.
- Cells needed:

  | prototype | cells |
  |---|---|
  | #1, #4, #5 | 4 std cells each, if their gate passes |
  | #2 | 4-6 deep cells (PRELOAD) |
  | #3, #6, #7, #8 | none (microbench/test-only) |

- Total: roughly 16-18 cells, with the deep ones taking longer.
- Every arm builds on `h2h` (29bfc705) with `TOOLS=best`.
- Compare flag-off vs flag-on of the same binary, ABBA, never across binaries.

**Bundles to keep together.**
- #3, #7 and r1 #5/#10 are four sites of one attack class: resting reduce-only orders that make other paths
  O(R) or O(K). Fixing one site leaves the rest open. Measure them in one adversarial ubench, and ship them as
  one "RO amplifier" PR series.
- #5 and #11 both change the `verified_cache_key` preimage. If both ship, do it in one change (rule byte in the
  key tail only).
- #13, #15 and r1 #8 all attack the end_resident wait. They compose, but their gains do not add linearly.
  Whichever lands first takes most of the 1.53 ms/blk ceiling.

---

## 1. phase2-sender-certificate (merged: p2-headroom-certificate, phase2-order-free-certificate)

**Mechanism.** One per-sender, per-call certificate for Phase 2. It is a lower bound on free margin:

> available + pos_net >= R + T + X_ub + C_ub

where R is the run's total reservation, T the B-blind top-up extras, X_ub the excess bound (1 raw unit per
order under flat tiers), and C_ub the pool-market charge bound. Free margin only moves monotonically, so if the
bound holds, every order of the sender passes in any order. Two ways to use it:
- **Arm H (headroom skip, the recommended core).** For certified senders E skips get_position, Projections,
  placement_need and excess/release. It sets `pre_pos = None`, so match_setup re-reads positions on the pool,
  and the top-ups are granted inline. This removes work from E.
- **Arm B (order-free, market-major on the exec pool).** Phase 2 runs per market inside exec-pool jobs with no
  stitch and no fold maps. This moves work off E. It is the same lever as TORUS_PARALLEL_ENGINE, which the
  owner may count as excluded.
- **Stage A** runs the certificate serially on E with no pool.

**Corrected gain:**
- Arm H:
  - E on-CPU -1.0..-3.5 ms/blk (skeptics -1.0..-2.5 / -1.5..-3.5 / -1.5..-3.5). In ubench terms, margin
    0.54 ms/1k should fall 0.12-0.2 ms/1k (22-37%).
  - Offsets: the pre-pass costs +0.3-0.9 ms. Pool re-reads (reduce_only_positions_for, d2_taker_px) cost
    2.5-4.8 ms of pool CPU, which raises E wait 0.25-0.5 ms.
  - matched/s 0..+2.5% (central ~+1%); blk/s +4..+9%.
  - If r1 #3 users-position-key-index lands first: -0.5..-1.8 ms.
- Arm B: E -3.5..-8 ms/blk. A full pool round costs E 2-4 ms, not 1.0-1.3. blk/s +8..+20%, block p99 -5..-10%,
  matched/s -1..+3% (PE base case), CPU/fill +1..+2%.
- Stage A: -0.3..-1.5 ms/blk.

**Confidence:** medium. Soundness was not refuted by any skeptic. The certificate pass rate on the node shape is
unverified, but likely high: the 9j2 funnel shows 0 margin rejects and 100% full top-ups, and the ubench shows
0 pool cuts. **Consensus-visible:** no (byte-identical by construction; debug shadow required).

**Must-have soundness fixes** (from the safety and feasibility skeptics):
1. **Floor anchor.** floor_candidate depends on the sender's first *accepted* checked order (`st.pool`,
   NE ~10049/10221). Pre-scan the anchor and check it afterwards.
2. **Certify per sender per whole call.** The flat order is sorted by (category, sender), so IOC/modify form a
   second run. Bail out to the exact path on RO, stop, IOC/FOK/market, or unmarked markets.
3. **C_ub coverage.** It must cover:
   - checked pool-market sells that fill above their limit (taker budget, OB ~1717), bounded with
     max(bid_floor, max buy limit);
   - GTC buys with a mark charge;
   - maker fills of the sender's resting orders in the pool market.

   Also guard that the pool market's best bid/ask is within about 2.6% of the band reference.
4. **Checked pre-pass sums, falling back to the exact path on overflow.** The hypothetical walk otherwise
   reaches panicking FixedPoint ops that serial only hits after a margin pass, which is a chain-halt DoS. The
   bound must also catch placement_need's overflow `Price` rejects.
5. **Flat tiers only.** Drop the tiered arm (0x-tier panic, non-monotone tiers).
6. **Arm B only:** the slot certificate must include the RO/stop base limit (NE ~12160), and the BalanceCache
   must be rebuilt before d2_pools.
7. Update the M1 shadow / pre_pos assertions (NE ~13099-13105, test at NE ~17041).

**Rationale.** This has the largest potential E cut of the round, and arm H *removes* E work instead of moving
it, which fits the owner's exec-CPU-per-fill rule. The two source ideas share one premise and one decisive probe
(the pass rate), so they are merged. Arm H is the primary deliverable. Arm B should only proceed with an explicit
owner ruling that it is outside the PE exclusion, and its expected outcome is PE's (blk/s up, matched/s flat).
Ranked #1 on potential. Medium confidence and the 5-7 d effort keep it from being a sure thing.

**Prototype spec (<= 3 h, one agent):**
- **Build target:** new worktree `proto/p2-cert` from h2h (29bfc705).
  `CARGO_TARGET_DIR=$HOME/.cargo-target-s80-p2-cert`, test binary `ubench_econ` (build command in the `ub.sh`
  header).
- **Files:** `crates/torus-bridge/src/native_executor.rs`: the Phase 2 serial loop (pre-pass, NE ~9448-9480)
  and `prepare_one` (NE ~9977-10245, account block ~10075-10200). Add env flag
  `TORUS_S80_P2_CERT={0,probe,1,shadow}`.
- **Step 1, probe (about 45 min).** After the existing fold, compute per sender per call F0 = available + pos_net
  and B = R + T + X_ub + C_ub (flat 20x). Print
  `UB CERT senders_pass/total orders_covered/total min_slack p5_slack`.
- **Step 2, skip arm (=1).** Certified senders skip the account block, set `pre_pos=None`, and get top-ups
  inline. `shadow` runs both paths and asserts equal output, excluding pre_pos: PrepOutcomes, top-up grants,
  d2 pools, order ids, and the per-block digest.
- **Tests:**
  - A new `crates/torus-bridge/tests/p2_cert_tests.rs` with near-margin random fixtures (accounts at
    0.9-1.1x need, mixed RO/IOC/stop orders) run with flag=shadow. It must show bail-outs and no mismatches.
  - `perf_equivalence_golden` with flag=1.
- **Microbench:** `ub.sh p2-cert off-N` vs `ub.sh p2-cert on-N TORUS_S80_P2_CERT=1`, ABAB x3 at
  `UB_SENDERS=5000` and once at 600. Read MEDIAN engine_ms/1k_fills, margin ms/1k and match ms/1k. Also check
  that the `UB s92 ... top_ups / sell_cuts` line is unchanged between arms.
- **Decisive measurement:** pass rate, then margin ms/1k and engine ms/1k on vs off.
- **Kill if any of:**
  - pass rate < 85% of senders or < 90% of orders;
  - any shadow mismatch that a tighter bound cannot remove;
  - margin/1k saving < 20%;
  - engine/1k saving < 2% (the pool re-read wait eats it).
- **If it passes:** 2+2 ABBA std cells: `TOOLS=best cell.sh p2-cert {off,on}-{1,2} TORUS_S80_P2_CERT=...`.
  Read margin ms/blk, sched E on-CPU per 1k fills, block_ms, matched_s_first120 and agreement.

---

## 2. epoch-plan-logical-ready

**Mechanism.** epoch_plan (app.rs ~9689-9719) refuses boundary H until the *durable* applied marker reaches
H-100. On a refusal the leader proposes a datum-less block, followers return MissingData, and the view burns.
The change is to publish plan(H+L) from the exec write site (epoch_plan.rs ~440) into a shared in-memory map,
and to share E's logical applied height (exec_applied) as an Arc<AtomicU64>. Readiness becomes
max(durable, logical). The plan bytes do not change. Node-local.

**Corrected gain:**
- Consensus timeouts drop to about 0 in every arm that has them:
  - deep CL21: ~3 stalls of 2.3-3.2 s per cell;
  - M15 and R: 5-12 per cell;
  - std CL21 is already ~0.
- Native blk/s: deep +1..+3% (deep R +3..+7%), std ~+0.2%.
- matched/s 0..+0.8% on deep (exec idle at refusals is 0.2-1.4% per cell).
- Block p50/p99 and RSS: no change.
- The "+9..+50% regime flip" upside is refuted. std r2/r4-CL21 and w1 have 0 refusals and still ramp up late,
  so refusals look like a symptom of warm-up, not its cause.

**Confidence:** high on timeouts (refused heights >= timeouts in all 22-25 9j2 cells; 0 refusals means 0
timeouts). Low on blk/s beyond +1-3%. **Consensus-visible:** no.

**Must-have safety fix** (safety skeptic, bug-b2 class): epoch_validator_set_updates turns a missing plan into
"no change". If readiness comes from the logical height and the map misses, the DB read before its checkpoint
returns None, and that node would install no validator change while its peers do. So:
- Read the DB only when durable >= H-L; otherwise return Err. Same rule in refresh_upcoming_evm.
- Insert into the map *before* the exec_applied store, after the fatal_error check, not mid-block.
- Keep map entries until H+L has executed.

**Rationale.** A cheap, node-local fix that removes a measured consensus-stall class (timeouts that cost
2.3-3.2 s each), with high confidence on that part. The idea revives the item6-phase3-design.md:86 "in-memory
applied height" reader, which was dropped only because of a barrier premise that epoch-boundary-fast-path
removed. Ranked #2 for its confidence/effort ratio on a stability metric. Its throughput gain is modest.

**Prototype spec (<= 3 h):**
- **Step 0, zero build (20 min).** Run `/home/oz/projects/wt/novel-s80/scratch-r2db/{reg,tmo}.py` over the 9j2
  deep cells and over `proto/results/s80-*` to get the base refusal and timeout rate per 120 s cell. If deep
  CL21 120 s cells average < 1.5 timeouts, plan 3+3 cells instead of 2+2.
- **Build target:** worktree `proto/epoch-plan-logical` from h2h. Build `torus-node`; reuse best's
  `bench-throughput` (md5 fa8fd4bd, per the cell.sh header).
- **Files:**
  - `crates/torus-bridge/src/app.rs`: epoch_plan ~9689-9719, refresh_upcoming_evm ~11160-11185, exec_applied
    ~1686-1689 and the store at ~4106.
  - `crates/torus-bridge/src/epoch_plan.rs`: write site ~440, read ~206-214.
  - Flag `TORUS_S80_PLAN_LOGICAL=1`.
- **Tests:**
  - Adapt app.rs ~15663 and ~44935.
  - New: durable lags and the map holds the plan, so epoch_plan returns Ok, and the bytes equal the DB plan
    once the checkpoint lands.
  - New: neither map nor durable is ready, so it returns Err (never "no change").
  - New: after a restart (empty map) it falls back to today's DB path.
- **Cells:** deep shape, ABBA:
  `PRELOAD=1000000 BULK_COUNT=1000000 TOOLS=best cell.sh epoch-plan-logical {off,on}-{1,2} TORUS_S80_PLAN_LOGICAL={0,1}`.
- **Decisive measurement:**
  - count of the WARN "epoch boundary: execution has not applied the previous boundary yet" (expect 0 on);
  - `torus_consensus_timeout_total` delta per node;
  - secondary: blk_s per 15 s bin, matched_s_first120, agreement, RSS max.
- **Kill if:** the on-arm shows any refusal WARN, or timeouts are not >= 80% lower than off, pooled. About 6
  expected off-arm timeouts make 0 decisive (p ~ 0.002).
- Keep only the timeout win (ship as a stability fix) if blk/s is within +/-2%.

---

## 3. ro-basis-walk-memo

**Mechanism.** phase2_reservation_basis (NE ~12402-12487) calls orders_for_trader once for *every*
reduce-only non-stop order. That builds a Vec over all R resting orders of the sender, with one get_order each,
before any slot check. The fix is an exact per-call memo keyed by (sender, market), walking ids with no Vec and
the same sat_add order. Stage 2 (a maintained aggregate) is dropped: it is not new, and drift would fork.

**Corrected gain:**
- Bench: 0.
- Attack (R = 1,000 dust non-RO orders, 1,024-order RO batch, no position needed):
  - Today: 82-154 ms of E per action. At 64 actions/sender/block that is 5-10 s per block per sender, up to
    5x more at the 5,000 open limit. Caps are proposer-local, so a Byzantine proposer can exceed them.
  - After the memo: at most one walk per (sender, market) per call, about 0.1-0.75 ms, a >= 1,000x cut.
- On its own it only halves the r1 #10 variant (1,000 resting RO orders), because the match-time sweep stays.
  Ship it with r1 #10 and #5/R16.

**Confidence:** high. All three skeptics confirmed the code path and exactness. **Consensus-visible:** no.

**Rationale.** A chain-stall-class DoS that one funded account can trigger, fixed by an exact half-day memo.
That is the best robustness yield/effort of the round.

**Prototype spec (about 2 h, test-only, no node build):**
- **Build target:** worktree `proto/ro-basis-memo` from h2h, release test build of torus-bridge.
- **File:** `crates/torus-bridge/src/native_executor.rs` phase2_reservation_basis (walk at ~12436-12443).
  Flag `TORUS_S80_RO_BASIS_MEMO=1`: a `HashMap<(Address, MarketId), FixedPoint>` next to `growth`. On the first
  RO order of a pair, iterate the trader's order ids directly with the same sat_add fold order.
- **Tests:**
  - An in-crate `#[ignore]` timing test next to `sell_top_up_stitch_tests.rs:837`, which already calls
    phase2_reservation_basis directly:
    - one book where sender S rests R in {100, 1,000, 5,000} non-RO orders (one level, and spread over 50
      levels);
    - one call with 1,024 RO non-stop orders of S;
    - median of 11 runs, base vs memo;
    - assert the output maps are identical.
  - A non-ignored differential test: random books and RO/non-RO mixes give memo == walk.
  - End-to-end: in the `reduce_only_tests.rs` harness, run one execute_batch with the 1,024-order RO batch.
    Read phase_accum.margin_ns and assert results, ids and state are identical.
- **Optional:** add the same scenario to `proto/adversarial-worst-block/crates/torus-core/tests/adversarial_match_ubench.rs`
  so r1 #5/#10 and this are measured on one shape.
- **Kill if:** base R=1,000 costs < 20 ms per action (below honest block cost, so a nice-to-have only), or any
  output differs.

---

## 4. streamed-settle-gate-prefork

**Mechanism.** Under L2.1 a pool job computes its market's settle plan only once cumulative fills reach the C3
gate (`settle_gate(matched)`, NE ~10639, MIN_FILLS 1024). So the first ~9-12 markets of every placement call
return no plan, and E computes those plans serially, booked as settle_pass_a (~2.2 ms per call). The fix is to
decide "parallel" before the fork (at item 0) so every job plans. The data is the same, so C3 byte-identity
holds.

**Corrected gain:**
- E on-CPU -0.4..-0.9 ms/blk (-2.4..-2.8% per 1k fills). Plan wait is a fixed ~2.2-2.95 ms per call, and parks
  are ~1 per call, so it is mostly compute.
- E block -0.5..-0.8 ms (-1.5..-2.3%). matched/s 0..+0.7%.
- It does not explain s119's L2.1 CPU step (that microbench forced parallel), but it removes 36-71% of s114's.

**Confidence:** medium-high. **Consensus-visible:** no.

**Must-have** (safety skeptic): the production rule must key on something shared by the streamed and
non-streamed arms (setup_in_jobs, or >= 64 prepared orders), not "streamed only". The L2.1 read-parity contract
(settle_streamed_tests.rs:7-19, 433-438) must hold. Add a test comparing per-block write sets parallel vs
sequential, because the running hash covers writes.

**Measurement trap:** once `decided` is set early, take-waits move into plan_wait (NE ~10658-10659). So
settle_pass_a_ms will *not* fall to ~0. Judge on engine ms/1k, match+settle per 1k, and E on-CPU per 1k fills.

**Rationale.** Removes real E work. The probe needs no build (an existing env knob) and the production change is
small. High yield per effort.

**Prototype spec (<= 1.5 h for the gate, + cells):**
- **Step 1, zero build.** `ub.sh h2h g1024-N` vs `ub.sh h2h g0-N TORUS_PARALLEL_SETTLE_MIN_FILLS=0`, ABBA with
  3 rounds per arm (5 if the spread is > 2%). ubench_econ runs execute_batch in Auto mode (checked, NE
  8890-8900), and ub.sh sets TORUS_SETTLE_STREAMED=1. Read MEDIAN engine_ms/1k_fills and the match and settle
  per-1k lines. Kill if engine/1k falls < 1.5%.
- **Step 2, no build.** 2+2 ABBA cells:
  `TOOLS=best cell.sh best g0-{1,2} TORUS_PARALLEL_SETTLE_MIN_FILLS=0` vs `cell.sh best g1024-{1,2}`.
  Read sched_by_node E on-CPU per 1k fills, block_ms, phases match+settle, matched_s_first120 and agreement.
  Kill if E on-CPU/1k falls < 1.5%.
- **If it passes:** production flag in `native_executor.rs` ~9574-9589 / 10639 with the shared rule, update
  the settle_streamed_tests and parallel_settle_tests seams, and re-run perf_equivalence_golden.

---

## 5. keyed-local-trust-key

**Mechanism.** `verified_cache_key` (torus-types lib.rs ~1443-1457) is node-local: a HashMap key that is never
on the wire or in state. Today it is Keccak over C||nonce||v||r||s. The change has three parts:
- Switch the key to SHA-256 via SHA-NI when the CPU has it, falling back to Keccak, chosen at startup.
- Store (sender, keccak(C)) as the cache value. Seeding forks keccak(C) from the content-hash sponge that pool
  insert already runs.
- On a hit, E gets the sender and the exact sort hash with no Keccak.

The POLYVAL arm is dropped from the plan. It is only safe with canonical-byte input, a length block and an AES
finalize, and it is linear.

**Corrected gain:**
- In vivo Keccak is ~455 MB/s and 59% of verify, not the 80-84% claimed. So E verify on-CPU falls -1.0..-1.2
  ms/blk with SHA-NI (ubench ~-0.85).
- E on-CPU per 1k fills -3.5..-5%.
- E wall -0.3..-0.8 ms/blk. 28% of blocks already wait on end_resident, so part of the saving turns into wait.
- matched/s +0.2..+0.7% (below the 1% bar); blk/s +1..+2%; CPU-s/1M -0.8..-1.5%.
- Big-block verify about 1.6x faster, not 3-6x.
- It is not additive with exec-carried-verified-senders or K1 if either is built. Tie-only sort hashing is a
  simpler alternative to the memo.

**Confidence:** medium on the CPU cut, low on matched/s. **Consensus-visible:** no (key and memo node-local;
the safety skeptic found no flaw in the SHA-NI arm).

**Rationale.** A clean, contained E on-CPU cut on the owner's merge metric, with no consensus-thread plumbing,
and a fast decisive microbench. Ranked below #4 because the wall conversion is partial and it overlaps proposed
priors (K1, carried senders).

**Prototype spec (<= 3 h, ubench only):**
- **Build target:** worktree `proto/trust-key` from h2h, test `exec_hash_once_ubench`
  (`crates/torus-bridge/tests/exec_hash_once_ubench.rs`). Uses sha2 0.10.9, already in Cargo.lock; confirm the
  SHA-NI path is taken (cpufeatures detect, print it).
- **Changes:**
  - Add `UB_TRUST_KEY={keccak,sha256}`.
  - Add a bench-shaped block: 60 actions = 37 PlaceOrderBatch x 400 + 23 CancelAll, sender tie-rate
    {0, 20, 50}%.
  - Seed outside the timed region, mimicking ingress: key -> (sender, keccak(C)).
  - Timed region = E's path: key pre-pass + lookup + sort using the memoized hash.
- **Correctness:** assert senders and the sorted action order are identical to today's path for every tie rate.
- **Runs:** release, median of 200 reps, idle and under `stress-ng --cpu 24`.
- **Decisive measurement:** verify+sort ms per block, keccak vs sha256.
- **Kill if:** the SHA-NI arm saves < 0.8 ms/blk idle or < 0.6 ms/blk under load, or there is any
  sender/order mismatch.
- **If it passes (follow-up, not in the 3 h):** node arm `TORUS_S80_TRUST_KEY=sha256` at:
  - the mempool seed/restash sites (lib.rs ~1355, 1495, 1575, 2213; native_pool.rs ~357-402);
  - eip712.rs ~1469-1539;
  - app.rs ~5597-5603.

  Then run 2+2 ABBA cells and read torus_exec_verify_seconds, E on-CPU/1k fills, end_resident_wait sum,
  matched_s_first120 and agreement.

---

## 6. passb-cothread-under-position-flush

**Mechanism.** About 80% of pass B (82 of ~100 streamed markets) still runs on E after the pool finishes.
- The streamed region hands each finished market to one scoped co-thread, which runs the unchanged
  settle_pass_b_market in market order.
- E keeps the pos_runs pushes and runs flush_with_runs (positions CF) concurrently, then joins before
  bal_cache.flush_all.
- Pass B reads only balance rows and writes no overlay rows, and the flush writes only position rows. The key
  sets are disjoint, so the result is byte-identical.
- The completion-order option is dropped: trade_index is stamped in persist order.

**Corrected gain:**
- E wall -1.5..-2.5 ms/blk (-4..-6.5%). Lock contention on the shared `pending` RwLock costs +0.3-0.9 ms on E;
  join and handoff wakes cost 0.2-0.5 ms.
- E on-CPU per fill -6..-8%. The work moves to the co-thread; node CPU is +0.5..+1%.
- blk/s +2..+6%; matched/s ~0 on std (load-bound, like L2.1's 0.998), 0..+2% on deep.

**Confidence:** medium. **Consensus-visible:** no.

**Must-haves:**
- Merge the fatal latch so the flush message wins.
- Re-raise co-thread panics on E: the co-thread is not in FAIL_STOP_THREADS.
- Join before the match-panic rollback, undo_streamed_prefix, commit_undo and out.finish.
- The scope must enclose run_streamed and the flush.

**Rationale.** The largest wall cut of the round after #1, with a 3 h ubench that settles it. Ranked below #4
and #5 because the work is moved rather than removed, node CPU rises, and std matched/s is expected to be flat.

**Prototype spec (<= 3 h):**
- **Build target:** worktree `proto/passb-cothread` from h2h, test `ubench_settle_streamed`
  (`crates/torus-bridge/tests/ubench_settle_streamed.rs`, UBENCH_L22=on, 16.8k and 65k shapes; it forces
  parallel settle, which is fine here).
- **Changes:**
  - Add timers around the call (~:180-200): cache_flush_ns, co-thread pass B ns, join-wait ns.
  - Flag `TORUS_S80_PASSB_COTHREAD`:
    - 0: today's path;
    - 1: co-thread overlapped with flush_with_runs;
    - 2: co-thread joined *before* the flush, to cost the hand-off alone;
    - optional mirror arm: pass B stays on E, flush_with_runs goes to the co-thread.
- **Code sites:** native_executor.rs streamed region ~10483-10766, flush ~9891-9934, PassBSink/PassBOut
  ~6018-6070.
- **Tests:** settle_streamed_tests, parallel_settle_tests and perf_equivalence_golden with flag=1. All must
  pass.
- **Measure:** exec wall per call, E thread CPU per call (`clock_gettime(CLOCK_THREAD_CPUTIME_ID)`), join wait.
  6 rotated passes per arm, idle and under `stress-ng --cpu 24`.
- **Kill if:** the wall saving is < 1.5 ms/call at 16.8k idle or < 1.0 ms under load, mean join wait is
  > 0.5 ms, or any test/byte difference.

---

## 7. lazy-ro-policing

**Mechanism.** Every match call installs ReduceOnlyPositions for *every* owner of a resting RO order in the
book: one position read per owner, NE ~13056-13076 and 13141-13167. The single path repeats this on every
placement (triggered stops, stage-1 closes, CoreWriter rows; NE ~13541-13553), with a slower batch:None reader.
The fix installs only the senders plus an install-time flag. On a miss it loads the position from the frozen
reader and memoizes it. Owners are policed if they are in reduce_only_index or in a departed set.

**Corrected gain:**
- Bench: 0 (the bench tools hard-code reduce_only:false).
- With K = 100k RO owners in one market:
  - batch path +20-35 ms per match call, 1-2 calls per block;
  - single path +25-100 ms per placement: 64 stage-1 closes add 1.6-6 s, 200 CoreWriter rows add 5-20 s, and
    N fired stops add N x 25-100 ms.
- matched/s under attack -20..-55% (the linear model overstates it).
- After the fix: O(senders + touched owners).

**Confidence:** medium. The cost exists for certain, but the per-owner cost is estimated. **Consensus-visible:**
no (frozen reader; exact).

**Possible simplification** (feasibility skeptic): non-sender reads happen only at the RO maker cap, where the
owner always has an index entry, and for fill makers, which maker_slot already loads lazily. If so, the
departed set is unnecessary. Verify with a debug shadow.

**Rationale.** A cheap attack: dust positions, no recurring cost, Sybil-spread under the 1,000 per-account
limit. The serial single-path amplification, through stops and CoreWriter rows on E, is the real danger. Same
attack class as #3 and r1 #5/#10.

**Prototype spec (<= 2.5 h):**
- **Build target:** worktree `proto/lazy-ro` from h2h, test `ubench_econ`.
- **Seeding:** add `UB_RO_OWNERS=K`, `UB_RO_MARKETS=M`. Seed K traders with a 1-lot Cross position each
  (long/short pairs, direct writes like UB_SEED_TRADERS) and one resting RO order per market, placed >= 1,000
  ticks from mid so it never fills.
- **Timers:**
  - around the batch RO install (NE ~13056-13076, per market) and the single-path install (NE ~13541-13553);
  - for 100 single-path 1-lot IOCs in market 1 via execute(PlaceOrder).
- **Arms:** K in {0, 10k, 100k} x M in {1, 300}, plus a senders-only stub install (exact here, since no seeded
  RO order fills) as the post-fix bound.
- **Decisive measurement:** per-call install ms, per-single-placement ms, engine ms/blk.
- **Kill if:** K=100k, M=1 adds < 5 ms per batch call *and* < 5 ms per single-path placement.

---

## 8. gov-market-cancel-batch (was gov-market-wipe)

**Mechanism.** MarketDelist, any MarketParamsUpdate that changes tick/lot/config, and delist settlement call
cancel_market_orders_and_stops (NE ~16196-16205). It loops over every trader:
- take_pending_stops and retain_stops each scan *all* stops;
- cancel_all_resting does a VecDeque::remove per order;
- the batch path needs >= 32 orders per trader.

Recommended fix (from the skeptics): call the already-merged, state-equivalent cancel_all_many over all traders
plus one stop partition. That is ~O(n log n + S). The full mem::take drain adds only a constant factor (2-4x)
and more equivalence risk, so it is optional.

**Corrected gain:**
- Throughput 0. Block p99 0: governance blocks are well under 1%.
- Honest deep market: the s63 shape (292k orders, ~1,500 traders) costs ~10 s per governance cancel today. This
  is extrapolated from the s63 probe (13 ms per cancel-all), not measured. With the fix: ~0.1-0.3 s.
- Adversarial 1M orders/stops: 10-20 min -> < 1 s.
- 300-market bench depth: 5-20 ms, so ~0.

**Confidence:** medium. The 10 s figure is an extrapolation. **Consensus-visible:** no, if state-equivalent.
The equivalence constraints:
- per-order margin;
- journals;
- next_seq/next_id;
- cloids.log one key at a time;
- the P2-1 index guard in resident mode.

**Rationale.** A routine parameter change on a deep market could stall the chain for seconds. The fix reuses
merged code and needs about a day, and the probe already exists.

**Prototype spec (1-2 h, torus-core test only):**
- **File:** `crates/torus-core/src/order_book/cancel_batch/many_tests.rs`. Extend the `#[ignore]`
  `cancel_all_many_timing_probe_deep_levels` (~:479) into a governance probe:
  - the s63 shape (300k orders, 4 deep levels, 1,500 traders ~200 each), plus S in {0, 100k} pending stops;
  - a second shape with T = 10k traders x 30 orders, below the 32-order batch gate.
- **Arms:**
  - (a) the per-trader loop in sorted trader order (take_pending_stops + cancel_all), as NE ~13968-13969;
  - (b) one stop partition + `cancel_all_many(&all_traders_sorted)`;
  - (c) an optional drain sketch.
- **Correctness:** assert identical cancelled-order lists per trader and identical margin inputs, plus
  `assert_books_equal` for (a) vs (b).
- **Run:** `flock -w 900 -o $HOME/projects/wt/.cargo-heavy.lock cargo test --release -p torus-core --lib cancel_all_many_timing -- --ignored --nocapture`.
- **Kill if:** (a) costs < 250 ms at the s63 shape and < 1 s at T=10k/S=100k (no stall risk), or (b) is not
  >= 5x faster than (a).
- **If it passes:** ship (b) at NE ~16196-16205 behind the resident P2-1 index guard (NE ~13948-13952).

---

## 9. liq-stage1-running-view (Fix A only)

**Mechanism.** The stage-1 loop re-runs liq_view after every close (liquidation_step.rs ~930). A dirty trader
is valued by delta_sums over all changed rows, so close k costs O(k) and the loop is O(P^2) per account. Fix A
keeps a loop-local running PosSums, updated by one market per close via sums_with_changes with the ABS_GUARD
fallback. This is exact. Fix B (a stage-1 order budget) is dropped: it is consensus-visible, goes against
owner s99's 64 acts, and re-parameterises existing caps.

**Corrected gain:**
- Bench 0.
- Shock blocks, worst case (P ~ 270, every close partially fills; an attacker can arrange this with 1-lot
  makers): -0.45..-1.9 s per 64-act block.
- Liquid books (the loop breaks after <= P/3 closes): -0.1..-0.2 s per block.
- Realistic P ~ 6 (HL data): < 3 ms.
- The per-close maker-side revaluation (maker_free) remains.

**Confidence:** low-medium. **Consensus-visible:** no.

**Rationale.** A real shock-time liveness amplifier, fixed exactly and node-locally with a merged pattern
(adl-running-rest). The realistic gain is small, and the prototype needs new book and maker setup in ubench_adl
(> 3 h), so no prototype this round. Run separate l1 and walk arms when it is built.

---

## 10. staking-reward-index (delegation half only)

**Mechanism.** F1-style per-validator cumulative reward-per-delegated-unit index. Each delegation stores its
entry index, and rewards settle lazily on delegate, undelegate, claim and slash. This replaces the per-epoch and
per-fee-block O(D) scan + RMW. The permanent-stake half is dropped: it would turn balance credits into claimable
rewards, an owner-level semantic change.

**Corrected gain:**
- Bench 0 (10 permanent stakes, 0 delegations, no EVM fees).
- Adversarial: 300k delegations at >= ~3.2e6 wei each.
  - Epoch block: +0.7-1.5 s of E, about -15..-25% exec capacity under attack. Of that, 0.4-0.9 s is beyond
    what the proposed delegations-scan-index removes.
  - EVM-fee blocks: +0.2-0.4 s.
- Organic 50k delegations: ~0.14 s per epoch block, 3-4% of exec.

**Confidence:** medium (sizes unmeasured). **Consensus-visible:** yes (state format, rounding).

**Requirements:**
- precision P ~ 1e36 plus a per-validator residual;
- precompile 0x0803, queries, claim, and governance unlock must compute accrual from the index;
- slash settles first;
- fresh genesis (R17).

**Rationale.** A cheap-to-mount epoch-block DoS that stacks on the %100 checkpoint-close block. It needs owner
sign-off and 5-8 d, so it ranks below the node-local fixes. Cheap next step for round 2: a 1-2 h `ubench_epoch`
sizing run that seeds D rows (`UB_SEED_DELEGATIONS`) and kills the idea if x00 at D=300k adds < 100 ms.

---

## 11. rule-versioned-trust-key

**Mechanism.** Add a sender-rule identity to the node-local trust-cache key, so a cached sender never crosses a
P0.0 activation that changes sender resolution.

**Corrected gain:** 0 perf. The value is latent: today no Feature affects sender resolution and the builtin
activation table is empty. It becomes relevant once a post-genesis sender-rule change ships by activation (the
open stop trigger/limit typehash gap is the likely first case, per roadmap :370). Without it, warm and cold
nodes disagree, which shows up as a running-hash fail-stop (halt). The miss wave at activation costs ~80 ms wall.

**Confidence:** high on the mechanism; the value is latent. **Consensus-visible:** no.

**Design fixes** (safety skeptic):
- Carry the rule id from the moment of verification. Do not recompute it at seed time from the tip, or an
  action verified under the old rule gets stored under the new key.
- Put the rule byte in the key tail only, never in the sort hash.
- Encode nothing for rule 0 (frozen-key test action_hash_scratch_tests.rs:694).
- Prefer the full feature bitmap, or an exhaustive-match `affects_sender_resolution()`, so every new Feature
  must be classified.

**Rationale.** Cheap insurance. File it as a P0.0 phase-7 prerequisite rather than prototype it now: all three
skeptics already confirmed the hazard in code. If #5 ships, fold this into the same key change.

---

## 12. cancel-all-split-phase-hoist

**Mechanism.** Start the block's cancel-all pool pass right after the sort and holder lock, as a split-phase
fork. It then overlaps the end_resident wait, new_env and the oracle step, and Phase 1 joins it.

**Corrected gain:**
- E wall -0.35..-1.0 ms/blk. The gain skeptic says 0.35-0.55, and the B1 oracle-only arm 0.26-0.46, which is
  below its own 0.5 ms kill gate.
- matched/s +0.1..+0.9% (central ~+0.3%).
- "Cancel-only blocks get shorter" is false: phase 1 is <= 1 ms in 66-68% of blocks.
- Less in production; zero for cold holders; shrinks if r1 #7 lands.

**Confidence:** low. **Consensus-visible:** no, but there are many safety fixes:
- take the whole ResidentInner (early exits otherwise leave a holder missing books);
- repeat the height/marker guard;
- market_listed fast path while books are lent;
- catch_unwind;
- margin config timing.

**Rationale.** Small, uncertain gain behind a lot of lent-state complexity. Before anything else, a one-cell
instrumentation run: time run_jobs at NE ~14149 and compute the joint min(0.81 x phase1, end_resident wait +
0.85 ms) per block. Kill if it hides < 0.6 ms/blk.

---

## 13. users-sharded-parallel-apply

**Mechanism.** Split Users into 16 address-range shards (map, opaque, traders, holders, counters). Apply them
in parallel on the idle exec pool during end_resident, optionally with E claiming shards.

**Corrected gain:**
- The measured parallel fraction is 0.62 (users 18.07 s of 29.14 s worker time); resident_delta, drops and
  finish stay serial.
- E wait -0.45..-1.1 ms/blk, out of 1.53; matched/s +0.35..+0.85%; blk/s +1.1..+2.5%.
- Only -0.15..-0.4 ms/blk beyond the full r1 #8.

**Confidence:** low-medium. **Consensus-visible:** no (node-local; sound per the safety skeptic).

**Routing fixes:**
- short position keys and non-trader balances go to the global `other`;
- cvlm keys route by their embedded address;
- E-assist needs a claim counter, a latch and panic poisoning.

**Rationale.** Real but mostly redundant with r1 #8(B). The 5-8 d refactor of users.rs is not worth +0.2%
beyond it. Revisit only if r1 #8 lands and the wait tail remains.

Prototype caveat for later: the test helper `trader()` puts every address at 0x3C, so shard on another byte.

---

## 14. cow-book-level-snapshot (merged: book-cow-snapshot-encode, cow-book-snapshot-window-save)

**Mechanism.** Levels become `Arc<VecDeque<Order>>` with `Arc::make_mut` at mutation sites. At the L6.1 %100
full save, E takes an O(levels) snapshot and hands a lazy book value to W, which runs the unchanged
encode_classic.

**Corrected gain:**
- Deep: E -0.43..-0.6 ms/blk (60 ms per window, minus a 5-9 ms cold Arc snapshot of ~63-66k levels, minus the
  Arc hop). Deep matched/s +0.3..+0.45%, or +0.2..+0.4% over r1 #6.
- Std: ~0 or slightly negative (first-touch copies of 15-60 MB per window and the Arc hop).
- %100 E spike -55..-60 ms, but those blocks sit below the exec p95, so p99 does not move.
- Transient RSS -70..-80 MB.
- The window-close leg is dead under L6.1 (`unsaved` is cleared), and the close clone belongs to r1 #6 (16.85
  ms per close measured, not 45-52).

**Confidence:** low-medium. **Consensus-visible:** no.

**Must-haves:**
- a lazy *variant* of the overlay value type (66 `.writes` sites; a side map risks a missed reader, i.e. a fork);
- a fixed size for merge_cf byte accounting;
- memoize the encode so W encodes once;
- fail-stop on a W encode error.

**Rationale.** The structural-sharing mechanism is new and removes the obstacle that got
book-save-pass1-offthread rejected. Its value is depth scaling (E cost tracks levels, not resting orders), but
on today's shapes the gain is below the 1% bar and overlaps r1 #6 and #12. If deep-book scaling becomes a
target, the 3 h `book_encode` P1M microbench (snapshot <= 5 ms, criterion order_book ops <= +2%, first-touch
<= 3 ms/blk, byte-equal proptest) is ready to run.

---

## 15. users-apply-sorted-merge

**Mechanism.** Four exact edits to Users::apply:
- (a) touch the E2 trader set only on appear/disappear;
- (b) one map entry probe;
- (c) a cursor search per trader group;
- (d) a merge-join of balances in place of the per-block HashMap.

**Corrected gain:**
- Users apply -6..-20%. The base is 2.43-2.68 ms/blk, not 3.0. BTreeSet insert is 1.1% of apply and the
  balances hashbrown is <= 3.3%. There is one search per key, not two.
- E wait -0.08..-0.3 ms/blk; matched/s +0.07..+0.24%; blk/s +0.2..+0.75%.
- About 0 on E if #13 lands, and (c) about 0 if r1 #3's SoA keys land.

**Confidence:** medium that it helps, but the gain is tiny. **Consensus-visible:** no.

**Rationale.** Cheap but below resolution. If built, use a bounded binary search [prev_i, len) rather than a
gallop. A one-pass merge rebuild of each trader's Vec would also remove the ps.insert/remove memmove (libc
5.86%), which is the stronger version of (c). Low priority.

---

## 16. cold-level-demotion

**Mechanism.** Far, untouched levels leave order_index, order_seq and trader_orders and are kept as their exact
Classic bytes until touched.

**Corrected gain:**
- Deep RSS -0.3..-0.41 GB per 1M cold orders: the resident cost is ~465-555 B per order vs ~145-150 B cold, not
  0.7-0.85 KB. Deep RSS mean goes from 4.37 to ~4.0 GB.
- Throughput 0..+0.5% deep, 0 std.
- Boot <= -1 s (the store scan stays).

**Confidence:** low. **Consensus-visible:** no, but it is a fork-risk class (the precedent is the rejected
lazy-book-loading). Every consensus query must answer identically for hot and cold levels:
- traders_present;
- the TraderMarkets rebuild;
- open_order_count;
- get_order(&self).

**Rationale.** RSS is not binding: owner D14 sets 64 GB per validator, and D3 says "complete residency, no LRU".
The effort is high (8-12 d) and so is the fork risk, for a modest RSS gain.

---

## 17. streamed-region-caller-cpu-priority

**Mechanism.** Lower the CFS weight of E's own exec-pool threads (nice or SCHED_BATCH), so that E wins the CPU
in the 33-on-32 streamed region.

**Corrected gain:**
- matched/s ~0 (-1..+0.8%).
- On a quiet host the most E can save is the runq: 1.5% per call at 16.8k fills and 2.1% at 65k. The existing
  TORUS_MATCH_WORKERS=17 knob already gets -2.1%.
- SCHED_BATCH changes no weight. Nice is one-way (RLIMIT_NICE=0) and also slows cancel-all and pass A. BORE
  already penalises E's bursts.
- Negative under ingress spam.

**Confidence:** low. **Consensus-visible:** no.

**Rationale.** A refuted gain in a twice-rejected family. The only cheap use is a 25-min zero-build sidecar
filler on the prebuilt `ubench-Q`. Kill unless it beats W17 by >= 0.5% wall.

---

## 18. storm-deferred-gossip-verify

**Mechanism.** While the verified pool is deep, defer arrival-time gossip recovery. Verify proposal-referenced
actions first and seed the exec trust cache.

**Corrected gain:**
- matched/s -1.1..+0.2%. The deferred work lands in the saturated phase B (exec busy 1.02-1.04), so net exec
  time changes by -0.2..+1.1%.
- Phase-A relief is 1.2-2.4 host cores (81% to 74-77%), not 2.5-4.
- Commit p99 at most -5..-10%, because the wait moves into the exec queue. Exec p99 ~0.

**Confidence:** low. **Consensus-visible:** no, but there are many fixes:
- tag the native_rx source;
- RPC shed counts must include deferred actions;
- charge the addr limiter after verify;
- exempt cancels, oracle and duty actions;
- seed the cache, not the pool (no committed tombstone);
- resolve the conflict with da-mirror-after-signature.

**Rationale.** It moves work in time without removing it, and the evidence says the destination is not idle.
The proposed probe (2 drainers, no proposal path) only caps verify pool width, a rejected family. Prior: s65
explorer `r2_fable_0-15` raw[11] (lazy verify).

---

## 19. causal-cancel-lane

**Mechanism.** Cancel-alls get a separate per-block lane outside the 400-action cap, plus a cross-block
per-sender causal gate.

**Corrected gain:**
- Mean exec age at most -0.03..-0.07 s, realistically ~0. Flush-second ages come from a full exec queue (depth
  40-66), which delays places equally.
- Commit p99 and exec p99: no measurable change.
- matched/s ~0 or negative.

**Confidence:** low. **Consensus-visible:** no, but the safety verdict is major:
- The gate holds C behind the sender's older P1, but not the newer P2, so C wipes cancel-replace requotes. That
  is a bench semantic change.
- Byte or order stops in the non-cancel-first walk can starve cancels entirely.
- With the non-cancel cap at 400, storm blocks carry +25% exec work.

**Rationale.** Refuted gain plus a semantic flaw. If the lane premise is ever revisited, start with the
zero-build `TORUS_CANCEL_BLOCK_SHARE_PCT=100` cell, and kill it if flush seconds remain. The causal half's goal
is the undecided s84 nonce-order proposal (mem 86096b39); that is an owner decision, not a perf item.

---

## Appendix A: killed ideas (one line each)

- **exec-garbage-off-e** (lit-exchange-2): killed on novelty (already-tried). It is the
  `TORUS_SETTLE_DROP_OFFLOAD` sub-flag of the s58 settle-passb-deferred-results-scatter candidate: hand each
  market's `Vec<MatchResult>` drop to a background dropper (journal.jsonl lines 24/56, mem 85b83cc5,
  d4ccb345). Proposed-not-done; only the scatter part (D) shipped.
- **resident-lazy-users-join** (end-resident-apply): killed on novelty (already-tried). It is r1
  streamed-end-resident (killed by skeptic majority in round 1): publish R first, then Users shard by shard,
  with block N's reads blocking per shard. Its own prototype step was r1's step 3, and it sits on the merged
  end-resident-on-worker join.

## Appendix B: coverage gaps for a round 2

1. **Elasticity calibration (measurement, highest leverage).**
   - Every ranking here converts E time to matched/s at 0.3, but the std shape has repeatedly measured about 0.
   - A deliberate E-delay injection would pin the current CL21 elasticity per shape. For example, a
     `TORUS_S80_E_SLEEP_US` busy-spin of 1/2/4 ms per block in 2+2 cells.
   - That would tell us whether removed-work ideas (#1, #4, #5) can move matched/s at all, or only blk/s and
     latency.
2. **Fixed per-block cost (~30% of the block) outside the round-1 commit-units idea.** No round-2 idea attacks:
   - new_env, begin_block_oracle, upkeep, save_books (std) or the tail (3.65 ms);
   - the non-flush parts of settle.

   The position-flush sort (~2% of exec cycles) and lookups (~3%) are untouched outside the excluded Phase 5
   area.
3. **Liquidation tail (3.65 ms/blk) and the 2,048-account scan.** Nothing in round 2 beyond the r1 #2 /
   liq-tail-offload findings.
4. **Commit and order latency on the consensus side.** qc_collect is 39.7 of a 52.5 ms view, and block_build is
   9.7 ms. Round 2 only had epoch-plan (#2) and two refuted storm/cancel ideas. No idea looked at vote/QC
   aggregation latency, proposal build cost, or the consensus gossip limiter (25-26 views/s, 567-796 drops per
   node per cell), which #2's authors flag as the next lever once refusals stop.
5. **The rest of the adversarial catalogue.** The skeptics surfaced uncovered O(S)/O(n) sites:
   - trigger_stops O(S);
   - open_order_count scanning all stops (OB ~2459);
   - cancel_order's VecDeque::remove shift on deep levels (OB ~2105);
   - every user cancel-all sender scanning all stops (NE ~14102/14225/14477).

   These need a per-block work-ceiling design (R16), not point fixes.
6. **Match-phase hot path on the pool.** Level data structures, order_seq binary search, the per-fill
   account_fill (13% of pool), and maker_slot loads. Pool CPU matters through host contention and the
   last-job wait, and round 2 had nothing here except CoW.
7. **Restart, recovery and catch-up.** Boot book load (3.1 s at 1M), replay throughput, and state sync under
   load. No idea in either round.
8. **EVM and CoreWriter interplay with native exec.** The fee path, CoreWriter single-path rows (only touched
   through #7), and EVM-fee blocks for staking (#10).
9. **Lenses that produced nothing that survived:**
   - lit-exchange-2's garbage-offload idea (dead on novelty);
   - end-resident-apply's lazy join (dead);
   - pool-roundtrip's third idea is weak (#17, refuted gain).

   Round 2 needs fresh angles rather than more pool-scheduling tweaks.
