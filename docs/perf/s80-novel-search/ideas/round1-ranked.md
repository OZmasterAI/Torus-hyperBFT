# novel-s80 round 1: ranked survivors (ranking judge)

Inputs: the 34 round-1 survivors (each with its novelty check and three skeptic verdicts: gain, safety, feasibility)
and the 10 killed ideas. All gains below are the skeptics' corrected gains, not the authors' claims. Duplicates are
merged, which turns 34 survivors into 27 ranked entries. The ranking key is expected yield on the target metrics
(matched/s, native blk/s, order/commit latency, block p99, deep-book stability, robustness) x confidence / effort.
Ties go to the higher potential yield.

**Conversion rule used throughout.** Exec-thread time saved is converted to matched/s at the measured elasticity of
about 0.3: in RD s67.4, block_ms -10% gave matched/s +3.0% and blk/s +9.5%. Later results are at or below that:
s124 CL21 vs R shows about 0, and s91 shows about 0. Any "+X% if strictly exec-bound" figure is treated as a ceiling,
not an expectation. CPU saved off the exec thread only counts through host contention, which has repeatedly measured
about 0.

**New evidence from parallel prototype agents.** I read these from `/home/oz/projects/wt/novel-s80/proto/results/`
and memory 8eb93876; I did not re-run them. They changed three rankings.

- **exec-commit-units ubench sweep** (`ecu-sweep-s1.out`, `ecu-sweep-s2.out`; mem 8eb93876).
  - Setup: equal total actions; block size x1/x2/x3/x4; fills per action constant (194.5 vs 193.5).
  - Walk 0, 600 senders: engine ms/1k fills 2.83 / 2.56 / 2.39 / 2.37, i.e. -10% / -15% / -17%.
  - Fit: engine fixed cost F = 8.2 ms/blk + 2.19 ms per 1k fills. Split: exec-only F 6.0, liquidation tail F 2.2.
  - Walk 10, 5,000 senders, R worker: K2 -18.6%, but exec-only only -7.8%. The rest is the 2,048-account liquidation
    scan, which amortises only if the scan budget is per unit.
  - Conclusion: the gain skeptic's 2-5% regression estimate is refuted in the ubench regime.
- **thp-heap ubench A/B** (`thp-ub-ab.out`; jemalloc via LD_PRELOAD; 3 rounds per arm).

  | arm | THP share | engine ms/1k fills |
  |---|---|---|
  | thp:never (holds the node's steady-state THP share) | 22-24% | 2.70-2.74 |
  | jemalloc default (short run) | 55-58% | 2.54-2.57 |
  | hpa:true | 71% | 2.54-2.59 |
  | hpa eager, 2 MiB purge threshold | 94% | 2.49-2.61 |

  - Moving THP share from 22% to 55% or more cuts engine time per fill by about 6% in the ubench.
  - The gain skeptic's in-cell cross-node regression found about 0. This conflict is unresolved, and only a cell can
    settle it.
- **users-position-key-index ubench A/B** (`ub-users-position-key-index-ab-{off,on}-{1..4}.log`,
  `TORUS_S80_POS_KEY_INDEX`).
  - Engine ms/1k fills: off 2.79 / 2.92 / 2.77 / 2.85, on 2.71 / 2.67 / 2.73 / 2.66. That is -4.9% engine per fill.
  - margin/1k 0.57 -> 0.51 and liq tail 0.48 -> 0.43, about -1.6 ms/blk at 11.5k fills.
  - This beats every skeptic estimate (0.13-1.0 ms/blk), measured at 600 senders.
- **liq-tail-offload probe** (`ub-liq-tail-offload-probe*-{600,5000}.log`).
  - Per liquidation call at 5,000 senders: window (liq_traders_after) 1.41 ms, loop 5.44 ms, about 1,120 traders.
  - 58% of window traders are position-dirty. Valuing them costs 5.2-5.3 ms, against 0.33 ms for clean traders and
    0.15 ms for cooldown rows.
  - So any exact node-local variant that re-values only touched traders on E saves at most about 0.3-0.5 ms. Only the
    consensus-visible block-start / lagged rule takes the dirty-trader cost off E.
- **exec-block-arena pre-profile** (`eba/exec-block-arena-rec-pre.out`).
  - 3-10 exec-thread page faults per 1k fills under jemalloc (8u measured 30.7).
  - At about 2.5 us each that is 0.1-0.3 ms/blk, which confirms the low ranking.

## Summary table

| # | slug (merged from) | corrected gain (skeptic-based) | conf | effort | consensus-visible | proto |
|---|---|---|---|---|---|---|
| 1 | exec-commit-units | exec per fill -6..-8% at K2 with liq kept per height (-10..-18% if the liq budget is per unit, overlaps #2), while the queue holds >=2 blocks; matched/s +2..+4%; blk/s up | medium | 12-18 d | yes | YES |
| 2 | liq-offE-block-start (liq-block-start-snapshot-helper, lagged-liq-scan-eah, liq-window-prevalue-r-snapshot) | E -2.7..-3.3 ms/blk (7-9%); matched/s +1.5..+2.5%; blk/s +5..+8% | medium | 5-9 d | yes | YES |
| 3 | phase2-position-lookup (users-position-key-index, phase2-run-dense-position-view) | E -1.0..-1.6 ms/blk on-CPU (ubench -4.9% engine/1k); matched/s +0.5..+1.5% | medium | 2-4 d | no | YES |
| 4 | thp-preserving-heap | exec on-CPU per fill 0..-6% (ubench -6%; in-cell regression ~0); matched/s 0..+2% | low | 0.5 d test, 2-3 d prod | no | YES |
| 5 | fok-precheck-bounded (batch-resting-only-fok-cow) | robustness: worst block ~1-1.4 s -> ~0.3-0.4 s; needs a per-block per-market budget; bench 0 | high on existence, medium on size | 3-5 d | yes (budget/restriction) | YES |
| 6 | close-merge-removal (ckpt-close-stack, lazy-window-shared-cf-maps) | std -0.15..-0.45 ms/blk, deep -0.45..-0.85 ms/blk; deep matched/s +0.9..+1.3%; deep upkeep p99 -40..-50 ms; transient RSS -110 MB | medium | 3-5 d stage 1, 6-8 d full | no | YES |
| 7 | oracle-step-second-memo | E on-CPU -0.45..-0.55 ms/blk (1.2-1.4%); matched/s +0.4..+1.0% | medium-high | 2-3 d | no | YES |
| 8 | end-resident-gate (end-resident-early-start, end-resident-gate-shrink) | E wait -0.25..-0.6 ms/blk with (0)+(A), -0.5..-1.1 with (B); matched/s +0.2..+0.9%; blk/s +0.6..+2.7% | medium | 1.5 d (0)+(A); +3 d (B) | no | no |
| 9 | serial-chain-sched-priority (rt-serial-chain, scx-forkjoin-scheduler) | exec rq 2.97 -> <1 ms/blk; matched/s about -1..+1.6%; bench artefact | low | 1-2 d (FIFO) / 5-8 d (scx) | no | YES (no-root bound) |
| 10 | ro-sweep-aggregate | robustness: -0.08..-0.29 s per 1,024-order RO IOC action; fill variant needs a newest-first trim; bench 0 | medium | 2-3 d | no | (in #5 bench) |
| 11 | cancel-all-generation-bump | net E -0.5..-0.9 ms/blk; matched/s +0.5..+1.5%; window-close +13-28 ms | low | 8-12 d | no | no |
| 12 | classic-cold-level-rope | deep %100 save 60 -> 7-12 ms; deep matched/s +0.8..+1.0% (overlaps #6); std 0 | low-medium | 6-9 d | no | no |
| 13 | exec-smt-core-cookie | matched/s about -1..+1.5%; can go negative | low | 0.5-1 d | no | (arm of #9) |
| 14 | mark-version-sums-prefill | 0 on every standard cell; +1..+3% only with moving marks | low | 4-6 d | no | no |
| 15 | phase2-order-prep-lookahead | E -0.5..-0.8 ms/blk; matched/s +0.4..+0.6% | low | 5-7 d | no | no |
| 16 | stop-index-cancel-paths | bench 0; ~1-3 ms per failing by-id cancel at 300k stops; trigger_stops O(S) remains | low-medium | 2-3 d | no | (in #5 bench) |
| 17 | exec-block-arena | E -0.2..-0.5 ms/blk; matched/s +0.3..+1% | medium | 4-6 d | no | no (stop) |
| 18 | fixedpoint-reciprocal-division | E -0.1..-0.3 ms/blk; matched/s <+0.5% | medium | 2-3 d | no | no |
| 19 | resting-order-compaction | ~0.1-0.4% exec; deep RSS -105 MB | medium | 5-8 d | no | no |
| 20 | gossip-pubkey-batch-msm-verify | CPU-s/1M -3.5..-4% vs today, -0.2..-0.5% beyond libsecp; matched/s 0 | medium | 7-10 d | no | no |
| 21 | gossipsub-native-anonymous-instance | CPU-s/1M -1.2..-1.4%; matched/s 0 | medium | 4-6 d | no | no |
| 22 | feed-ahead-commit-prep | matched/s ~0; block p99 -1..-3%; phase-A view -10..-18% | medium | 4-6 d | no | no |
| 23 | lazy-commit-persist-ticket | matched/s +0.03..+0.3%; removes 10-40 E stalls per cell (exec-age tail) | high | 3-4 d | no | no |
| 24 | commit-inclusion-stream | node ~0; bench +0..+5% is harness-only (read-ahead lever); val2 -0.5 cores | medium | 2-3 d | no | no |
| 25 | book-blob-cdc-store | deep batch -45%, W db.write -140 ms; matched/s 0..+0.7% | medium | 4-6 d | no | no |
| 26 | ckpt-batch-presize | W build -20..-50 ms per checkpoint; target metrics 0 | high | 0.5 d | no | no |
| 27 | l3-cat-exec-partition | matched/s 0..+1%; MBA 0; needs root | low | 0.5 d test, 2 d prod | no | no |

**Prototype budget note.** All eight prototypes share one host.
- Cells serialize on `proto/.cell.lock`. Builds and ubench runs serialize on `~/projects/wt/.cargo-heavy.lock`.
- The plans below are microbench-first: cells run only after a microbench gate passes.
- Total cell demand is about 16-20 cells of 120 s, roughly 1.1 h of serialized host time.
- All prototypes build on the `h2h` base (29bfc705), the same binary family as the 9j2 CL21 bar.
- Commands:
  - cells: `TOOLS=best /home/oz/projects/wt/novel-s80/proto/cell.sh <arm> <round> <ENV...>`
  - ubench: `/home/oz/projects/wt/novel-s80/proto/ub.sh <arm> <label> <ENV...>`
- A flag-off arm must be compared against the flag-on arm of the same binary (ABBA), never against a different binary.

---

## 1. exec-commit-units: run K queued committed blocks as one engine unit

- **Merged from:** exec-commit-units.
- **Corrected gain:**
  - Exec-only per fill: -6..-8% at K2, with the liquidation tail kept per height. The fit gives an exec-only fixed
    cost of 6.0 ms/blk at 11.5k fills.
  - -10..-18% if the liquidation scan budget is per unit. That overlaps with #2.
  - Applies only while the exec queue holds at least 2 blocks, which is 83-92% of seconds in 9j2.
  - Expected matched/s +2..+4% at elasticity 0.3. blk/s and exec age improve. Commit latency and block p99 are
    unchanged.
  - The per-block non-engine work (verify, sort, save, end_resident, upkeep) also amortises but is not in the ubench.
    end_resident per fill already falls from 0.24 to 0.17 ms per 1k at K4.
  - Skeptics' estimates: 2-5% (gain), 8-14% (safety), 6-15% (feasibility). The equal-work ubench sweep supports the
    upper half.
- **Confidence:** medium. **Consensus-visible:** yes.
- **Rationale:**
  - Highest potential of the round, and the only idea whose amortisation claim already has a direct equal-work
    measurement.
  - The risks are semantic, not performance:
    - Inside a unit the sort key (category, sender, hash) runs every non-GTC action of block n+1 (cancels, IOC/FOK,
      modify, withdrawals) ahead of block n's GTC orders. That gives a last-look window of up to K views.
    - Fees need per-height attribution.
    - Liquidation and ADL limits need scaling, and sessions must resolve per height.
    - Units must break at feature bits, slashes and CoreWriter drains.
    - Replay and attest need per-unit definitions. mem 8eb93876 lists the `prev+1==height` invariants to relax:
      running_hash chain_step, ChainMemo, resident rows/books slots, bl2 parent assert, exec_pipeline Digest window,
      nonce-window prune.
  - The owner must sign off on the cancel-hoist semantics before production work. This is a product decision.
- **Prototype** (in progress: `proto/exec-commit-units`, `TORUS_S80_EXEC_JOIN=K`, branch s80/exec-commit-units):
  1. Finish the node-side deterministic join. Every node uses the same K, only queued committed heights are joined,
     and there is one running-hash/attest result per unit.
  2. Build: `cargo build --release -p torus-node` into `~/.cargo-target-s80-exec-commit-units`; copy
     `bench-throughput` from `best` (md5 fa8fd4bd).
  3. Tests:
     - agreement on a local 3-node devnet over 200 blocks with K=2;
     - `perf_equivalence_golden` with the flag off.
  4. Cells, ABBA: `TOOLS=best cell.sh exec-commit-units {off-1,on-1,on-2,off-2} TORUS_S80_EXEC_JOIN={0,2}`.
  5. Read:
     - `.headline.agreement_verdict`;
     - schedstat exec on-CPU per 1k fills (and `engine_ms_per_1k_fills`);
     - `matched_s_first120`, `blk_s_avg`;
     - exec age p50/p99;
     - placed and rejected per action. Guard: within 2%, because cancel hoisting can push traders over the
       1,000-order open limit.
  6. Kill if any of these holds:
     - agreement fails;
     - exec on-CPU per 1k fills improves less than 5% at K=2;
     - exec on-CPU improves but `matched_s_first120` gains less than 1% (zero conversion).
  7. If it passes, write the owner semantics memo before any further work.

## 2. liq-offE-block-start: liquidation classification off the exec thread under a block-start (or one-block-lagged) rule

- **Merged from:** liq-block-start-snapshot-helper, lagged-liq-scan-eah, liq-window-prevalue-r-snapshot.
- **Corrected gain:**
  - E -2.7..-3.3 ms/blk. The step measured 3.57 ms/blk in 9j2, 7-9% of the exec block.
  - matched/s +1.5..+2.5%, native blk/s +5..+8%.
  - Empty and oracle-only blocks gain about 0 (E would wait for the helper).
  - Block p99 is unchanged.
- **Confidence:** medium. **Consensus-visible:** yes. Needs a P0.0 feature bit.
- **Rationale:**
  - The step is real, serial on E, and now 9.5% of the block. The s123 rejection of liq-scan-parallel-prevalue
    assumed a 2% share.
  - The parallel probe shows why only the consensus-visible rule pays: 58% of window traders are dirty in the block
    and account for about 94% of loop time. liq-window-prevalue-r-snapshot's exact "touched-check" variant (kept here
    as the dead sub-variant) therefore recovers at most about 0.3-0.5 ms, consistent with its gain skeptic's
    refutation.
  - The two consensus variants are one family:
    - (a) classify block n+1's window on the block-start R(n) and marks(n+1), on a helper started after
      begin_block_oracle;
    - (b) a fixed one-block lag.
    - Variant (a) is preferred. Start the helper after begin_block_oracle and drop every Arc before
      end_resident_take, or R gets rebuilt.
  - Economics need sign-off:
    - a one-block detection lag;
    - clear_cooldown on traders healthy at block start, versus the HL whole-position cooldown;
    - in-pass cascades are lost;
    - new holders are missed until the next sweep.
  - Phase 5 must stay parked: in-place Users mutation breaks the snapshot.
  - A cheaper fallback to compare first is the in-progress exact pool variant (`TORUS_S80_LIQ_PREVALUE`,
    `proto/liq-tail-offload`). It re-runs liq-scan-parallel-prevalue at today's 9.5% share and needs no owner
    sign-off.
- **Prototype** (ubench only, no cell). In `proto/liq-tail-offload`, add `UB_LIQ_BLOCKSTART=1` to
  `crates/torus-bridge/tests/ubench_econ.rs` and a read-only `classify_window(Arc<Users>, &BlockMarks, cursor)` in
  `liquidation_step.rs`.
  1. After begin_block_oracle, spawn a std thread with clones of Arc<Users>/R, the mark table and the cursor. It walks
     the window over the records only: no overlay, no memo locks.
  2. In `liquidation_pass`, join before the cursor and row writes, run the inline path only for flagged traders, and
     drop the Arcs before end_resident.
  3. Arms, ABAB with n=3:
     - inline (flag off);
     - block-start helper;
     - `TORUS_S80_LIQ_PREVALUE=1` pool.

     Settings: `UB_SENDERS=5000 UB_MARKS=1 UB_R_WORKER=1`, with `UB_MARK_WALK` at 0 and at 10.
  4. Record per block:
     - `tail(liq)/1k` and engine_ms/1k;
     - helper ms;
     - E join wait;
     - count of classifications that differ from the inline path (expected only for traders dirtied in this block);
     - R rebuilds (must be 0).
  5. Kill the block-start arm if any of these holds:
     - the E tail saving is under 2.0 ms/blk at 5,000 senders (the inline step costs about 6.9 ms per call there);
     - the mean join wait is over 1 ms/blk;
     - any R rebuild happens.
  6. If the pool arm reaches at least 70% of the block-start saving, prefer it: it needs no consensus change.

## 3. phase2-position-lookup: cheaper (sender, market) position lookups in Phase 2

- **Merged from:** users-position-key-index, phase2-run-dense-position-view.
- **Corrected gain:**
  - E on-CPU -1.0..-1.6 ms/blk; matched/s +0.5..+1.5%.
  - Skeptics: 0.13-1.0 ms/blk for SoA keys plus hoist, 1.0-1.6 for the dense run view.
  - The parallel ubench A/B measured -4.9% engine per fill, about -1.6 ms/blk, with margin and liq tail both down
    about 10%.
  - The pool-side maker lookups add no matched/s.
- **Confidence:** medium. **Consensus-visible:** no. Exact and node-local.
- **Rationale:**
  - The best yield per effort among the throughput ideas: 2-4 days, exact, no owner semantics, already measured
    positive.
  - Both entries attack the same cost: in prepare_one, every (sender, market) pair does a users::find binary search
    over about 300 Positions of 112-128 B each, plus resident_positions.
  - SoA keys plus hoist is already built. The dense run view (one sweep per sender run, plus interpolation/gallop find)
    is the follow-up arm if keys alone fall short.
  - Fixes from the skeptics:
    - a sender that reappears non-adjacently must spill or seed SenderState.proj, so unsorted test callers stay
      correct;
    - gate the sweep on run length against position count (DoS);
    - keep size_of(UserState) and the users_tests:558 pin, or accept +16 B per record;
    - keep keys and positions in sync at all 5 mutation sites, and widen the shadow check.
- **Prototype** (arm exists: `proto/users-position-key-index`, `TORUS_S80_POS_KEY_INDEX` in {0, keys, hoist, 1}):
  1. Cold-records ubench, ABAB n=4, flag 0 vs 1, with `UB_SENDERS=5000 UB_WARM=300`. Then split attribution: keys vs
     hoist, n=2 each.
  2. Run `cargo nextest run -p torus-bridge -E 'test(users_) | test(projections) | test(perf_equivalence_golden)'`
     with the flag on and with it off.
  3. Cells, ABBA: `TOOLS=best cell.sh users-position-key-index {off-1,on-1,on-2,off-2} TORUS_S80_POS_KEY_INDEX={0,1}`.
     Read:
     - `engine_ms_per_1k_fills`;
     - margin ms/blk from `phase_by_node.*.phases`;
     - schedstat exec on-CPU per 1k fills;
     - `matched_s_first120`.
  4. Kill if cell engine per fill improves less than 2%, or the margin phase saves less than 0.5 ms/blk.
  5. If keys alone deliver less than 70% of the combined gain, add the interpolation-find arm
     (phase2-run-dense-position-view part c) before deciding.

## 4. thp-preserving-heap: keep the node heap huge-page backed

- **Merged from:** thp-preserving-heap.
- **Corrected gain:**
  - Exec on-CPU per fill 0..-6%, matched/s 0..+2%. RSS risk of +0.3-0.7 GB.
  - Skeptics: about 0..+2% matched/s; the gain skeptic's central estimate is 0.5%.
  - Ubench: -6% engine per fill when THP share rises from 22% (thp:never, the node's steady state) to 55-94%.
- **Confidence:** low. **Consensus-visible:** no.
- **Rationale:**
  - The cheapest decisive test of the round: no build, only a MALLOC_CONF change. The node's unprefixed
    tikv-jemalloc 5.3.1 reads MALLOC_CONF, and its conf.c supports `hpa`, `hpa_hugify_style`, `hpa_purge_threshold`
    and `hpa_dirty_mult`.
  - The ubench and the in-cell cross-node regression disagree: val2 at 36-40% THP showed no exec difference from val0
    at 22%. Only a cell can settle it.
  - Risks:
    - defrag=madvise compaction stalls on exec faults;
    - HPA deferred work runs inline because background_thread is false;
    - RSS growth against the 8 GB class;
    - khugepaged bloat at max_ptes_none=511;
    - do not use arm (c), the decay -1 arena.
- **Prototype** (no build; uses the `best` binaries):
  1. Pre-check: start one node binary with the arm's MALLOC_CONF for 5 s and confirm stderr shows no
     "Invalid conf pair".
  2. Cells, ABBA: `TOOLS=best cell.sh best thp-{D1,E1,E2,D2}`, adding
     `MALLOC_CONF=hpa:true,hpa_hugify_style:eager,hpa_purge_threshold:2097152` for the E rounds only.
     D = default, E = hpa eager.
  3. Add a third arm, `MALLOC_CONF=hpa:true,background_thread:true`, only if E passes but p99 regresses.
  4. Read:
     - load-window AnonHugePages/Anonymous share (mem-samples, all 3 nodes);
     - schedstat exec on-CPU per 1k fills and `engine_ms_per_1k_fills`;
     - `matched_s_first120`;
     - VmHWM/RSS max;
     - chain p99 and timeouts.
  5. Kill if any of these holds:
     - load-window THP share stays below 60%;
     - exec on-CPU per 1k fills improves less than 3%;
     - RSS max rises more than 10%.

## 5. fok-precheck-bounded: bounded, clone-free FOK pre-check, plus a per-block FOK/batch work bound

- **Merged from:** fok-precheck-bounded, batch-resting-only-fok-cow.
- **Corrected gain:**
  - Robustness only. Bench matched/s is 0 (the bench sends only GTC).
  - Worst adversarial block today: about 200k batched FOKs that fail to cross, at S≈209 senders, cost about
    1-1.4 s. About 0.6-1.0 s of that is the per-FOK clone of AccountMargins and ReduceOnlyPositions; the rest is the
    non-consuming walk.
  - The RO map can be inflated without spending block slots (owners of resting reduce-only orders), which makes the
    clone cost unbounded.
  - Copy-on-write removes the O(S+K) clone: 2.5-4x on the worst block.
  - The per-side early reject can be bypassed, because margin- and RO-skipped makers count as phantom depth.
  - Only two things bound the walk: a per-block, per-market deterministic budget (R16 option B), or an owner rule of
    "no FOK inside PlaceOrderBatch". HL has no FOK, and IOC self-crosses already cancel via STP.
- **Confidence:** high that the amplifier exists (confirmed in code at 2d2f3ed9); medium on its size.
- **Consensus-visible:** yes for the budget or batch restriction; the COW part is not.
- **Rationale:**
  - The real security win of the round. The fix is small, and the attack needs only capital that is reusable every
    block.
  - #10 (RO sweep) and #16 (stop miss path) are the same class of repeatable linear rescan. One adversarial bench
    measures all three.
  - The COW scratch must reproduce maker_slot's first-touch inserts exactly (differential test).
- **Prototype** (torus-core microbench, about 2.5 h): an `#[ignore]` test in
  `crates/torus-core/tests/adversarial_match_ubench.rs`. Run it with
  `cargo test --release -p torus-core --test adversarial_match_ubench -- --ignored --nocapture` under
  `LD_PRELOAD=/usr/lib/libjemalloc.so`, which matches the node allocator. It uses the public `OrderBook` API
  (`place_order_with_accounts`, `set_account_margins`, `set_reduce_only_positions`, `restore_stop_row`).
  - **Arm A, FOK.** Book depth D in {2k, 20k} from 50 makers. AccountMargins S in {80, 209, 2k}; RO map K in
    {0, 10k}. Time 1,024 rejected FOKs per action in two forms:
    - qty larger than the depth;
    - qty within the depth but failing margin (the early-reject bypass).

    Compare `makers=Some` against `makers=None` (the lower bound for COW).
  - **Arm B, RO sweep.** A trader holds R in {100, 1000} kept RO asks, either at one level or spread across levels.
    Time 1,024 RO IOC sells, with no fill and with 1-lot fills.
  - **Arm C, stop miss.** 300 books x {1k, 3k} stops. Time a pending_stop miss with the current linear scan against a
    binary search over the id-ascending stops.
  - Report ns per op and ms per action.
  - Kill each arm separately:
    - A: 1,024 FOKs at S=209, D=2k cost under 20 ms, and K=10k costs under 50 ms;
    - B: under 20 ms per action at R=1k;
    - C: a miss costs under 0.5 ms at 300k stops.
  - Every arm that passes goes into an R16-style per-block budget design note for the owner.

## 6. close-merge-removal: no synchronous layer merge on E at checkpoint close (stack first, lazy window later)

- **Merged from:** ckpt-close-stack (stage 1), lazy-window-shared-cf-maps (stage 2).
- **Corrected gain:**
  - **Stage 1.** C becomes a stack [L, n-1, n] and W folds it by RocksDB batch order.
    - std -0.11..-0.18 ms/blk.
    - deep -0.45..-0.55 ms/blk, about +0.9..+1.3% deep matched/s. About 80% of the deep saving is the clone of the
      roughly 110 MB L6.1 %100 book blob.
    - Deep upkeep p99 drops about 40-50 ms, and deep transient RSS about 110 MB.
  - **Stage 2.** Also removes the per-block deferred join: about -0.45 ms/blk total on std and -0.8 on deep. W's close
    build grows 50-100 ms unless W pre-merges incrementally.
- **Confidence:** medium. **Consensus-visible:** no. Last-write-wins in batch order is exact: only put/delete, no merge
  operator, and h_n comes from the per-block frozen sets.
- **Rationale:**
  - Deep-book stability is one of the targets. The deep close premium, about 40-46 ms per close above std, is measured
    and attributed to merge(current) cloning the L6.1 blobs.
  - Fixes needed:
    - every reader of C must probe newest-first: overlay lower() (a fixed [_;3]), ReadView, W lookup_row for the EVM
      root, the trie dirty map;
    - gate the stack to L6.1/L3.1 coalesce on, so genesis files without L6.1 don't duplicate per-block blobs;
    - make the same change in checkpoint_drain.
  - Stage 2 needs a key index or bloom filter for EVM precompile reads, which could otherwise probe up to 200 maps.
  - Overlaps #12 (rope).
- **Prototype** (torus-state microbench, about 2.5 h): an `#[ignore]` test in `crates/torus-state/tests/`, run under
  `LD_PRELOAD=/usr/lib/libjemalloc.so`.
  1. Build L with about 700k position-like keys (52 B keys, about 100 B values) and two FrozenPending-equivalent sets
     of about 8k rows (std). For the deep case, `c` also carries 300 x 367 KB book blobs.
  2. Time `layer.merge(p); layer.merge(c)` (today's E-side cost) against building the stack (Arc clones). Also time
     W-side `append_to_batch` for both forms into two temp RocksDB instances.
  3. Assert that full CF scans are byte-identical over random put/delete/put sequences.
  4. Repeat under `stress-ng --cpu 24`.
  5. Kill if the E-side merge is under 10 ms at the deep shape (the stack would save too little), or if the W batch
     build grows by more than 60 ms.

## 7. oracle-step-second-memo: exact memo of begin_block_oracle while inputs and header second are unchanged

- **Merged from:** oracle-step-second-memo.
- **Corrected gain:**
  - begin_block_oracle measures 0.68-0.83 ms/blk. A memo hit still pays about 0.13 ms for puts and checks, and the
    hit rate is 86-93% at the bench feed rate.
  - Net E on-CPU -0.45..-0.55 ms/blk, -1.2..-1.4% of the exec block; matched/s +0.4..+1.0%.
  - The gain shrinks with more validators or a faster feed.
- **Confidence:** medium-high, but the gain is small. **Consensus-visible:** no. Results are discarded and the
  written bytes are identical.
- **Rationale:**
  - Small, exact and cheap, and it moves the owner's merge metric (exec on-CPU per fill) directly.
  - Use input comparison, not epoch bumps in the handlers: compare the stakes and market vectors that oracle_inputs
    already reads every block.
  - next-expiry must cover all sub and agg rows, including delisted ones, with +1 boundaries.
  - A hit must still run prev_marks.take() and sums.start. Clear the memo on any R rebuild or restart.
- **Prototype** (45-90 min, ubench):
  1. In the h2h-based worktree, time begin_block_oracle on its own in `ubench_econ.rs` (:158-176 already isolates
     it).
  2. Add a stub arm that does only the 300 x 36-byte agg-row puts plus version reuse; this bounds the hit-path cost.
  3. ABAB n=3 with `UB_MARKETS=300 UB_MARKS=1`.
  4. Kill if real minus stub is under 0.3 ms/blk.
  5. If it passes:
     - build the memo behind `TORUS_S80_ORACLE_MEMO=1`;
     - add a feed-cadence knob: submit every 15th block, advance the header second every 22 blocks;
     - assert identical per-block digests with the flag on and off.

## 8. end-resident-gate: hand-off before upkeep (0), borrowed frozen delta (A), parallel Users re-value (B)

- **Merged from:** end-resident-early-start, end-resident-gate-shrink.
- **Corrected gain:**
  - (0) alone: -0.1..-0.16 ms/blk of E wait. Upkeep is at most 0.1 ms in 81% of blocks, and the hand-off wait is
    0.012 ms.
  - (0)+(A): -0.25..-0.6 ms/blk. The frozen-to-delta clone costs about 0.5 ms of worker time.
  - (0)+(A)+(B): -0.5..-1.1 ms/blk.
  - matched/s +0.2..+0.9%, blk/s +0.6..+2.7%. Block p99 unchanged.
  - The saving is E wait time, not E on-CPU time, so the owner's per-fill CPU metric does not move.
- **Confidence:** medium. **Consensus-visible:** no. The reorder is exact on 2d2f3ed9. One fix: the failed-hand-off
  test at app.rs:33398 must use a rows-only drop.
- **Rationale:**
  - Cheap and exact, but small. Wait time is concentrated in the roughly 14% of blocks where the worker overruns badly,
    so head-start models built on mean wait overstate the gain.
  - It does create slack: today, cuts to verify and replay turn into extra end_resident wait instead of saved time.
- **Cheapest test (not selected):**
  1. ubench `UB_R_WORKER=1 UB_MARKS=1`, ABAB, timer/blk before and after (A);
  2. then 2+2 cells comparing `torus_exec_end_resident_wait_seconds` sum/count.

## 9. serial-chain-sched-priority: give the serial chain (exec, hotstuff-algo, end-resident) scheduling priority without pinning

- **Merged from:** rt-serial-chain (SCHED_FIFO / EEVDF custom slice), scx-forkjoin-scheduler (sched_ext policy).
- **Corrected gain:**
  - exec rq wait 2.97 -> about 0.4-1 ms/blk, so E wall time -1.8..-2.4 ms/blk.
  - matched/s about 0 on std (the shape is load-bound), 0..+1% on deep, central about +1%.
  - It can go negative: a FIFO exec thread holds off other nodes' pools, and a FIFO pool starves tokio.
  - Production gain with one node per host is about 0, so most of this is a co-location artefact.
  - The sched_ext-only parts (gang dispatch, yield demotion) add about 0.
- **Confidence:** low. **Consensus-visible:** no.
- **Rationale:** the family has failed before (pinning -5.7%, nice/SCHED_BATCH about +1%). It stays on the list
  because one no-root, no-build test bounds all three scheduling ideas (FIFO, scx, cookie #13) in about 1.5 h of
  cells.
- **Prototype** (no build, no root). A sidecar script under `/home/oz/projects/wt/novel-s80/proto/sched-bound/` waits
  for the 3 `torus-node` PIDs.
  - **Arm A:** baseline, no sidecar changes.
  - **Arm B, priority upper bound:** `chrt -i -p 0` (SCHED_IDLE) on every TID except `torus-execution`,
    `hotstuff-algo`, `torus-end-resid*` and `torus-exec-pool*`, plus the bench process. Rescan /proc every 5 s,
    because pools spawn lazily.
  - **Arm C, core cookie:** `prctl(PR_SCHED_CORE, CREATE, tid, SCOPE_THREAD)` via python ctypes, on `torus-execution`
    only. Keep a dummy cookied task alive so core scheduling stays enabled.
  1. Cells in order A, B, C, C, B, A on `best` (`TOOLS=best cell.sh best sched-{A,B,C}-n`), with the sidecar started
     alongside each cell.
  2. Read:
     - schedstat exec rq and on-CPU ms/blk;
     - exec block ms;
     - `matched_s_first120`;
     - view timeouts and commit p99;
     - for C, also `perf stat -t <exec tid> -e cycles:u,instructions:u` per window.
  3. Kill the whole family if arm B does not bring exec rq below 1.0 ms/blk and cut exec block by at least 2 ms, or if
     B passes on those but `matched_s_first120` gains less than 1%.
  4. Kill the cookie if exec CPI improves less than 5%, or if the rise in rq exceeds half of the on-CPU saving.
  5. Only if B passes, ask the owner for rtprio and run a FIFO arm: exec, hotstuff and end-resident only, with
     SCHED_RESET_ON_FORK.

## 10. ro-sweep-aggregate: O(1) reduce-only sweep when nothing is cut

- **Merged from:** ro-sweep-aggregate.
- **Corrected gain:**
  - Robustness only; about 0 on the bench, which has no RO orders and where the empty-index skip already covers the
    common case.
  - Under attack: removes about 0.08-0.29 s of exec-blocking time per 1,024-order RO IOC action, about 0.7-2.6 s per
    fresh address.
  - The 1-lot-fill variant stays O(R) unless the trim runs newest-first from the stored sum, re-emitting cuts
    oldest-first.
- **Confidence:** medium. **Consensus-visible:** no. The aggregates are derived and never serialized. Drift between
  nodes is a fork risk under the hash fail-stop, so add a debug recount.
- **Rationale:** real and cheap, but it closes only one of several linear rescans (the FOK clone and trigger_stops
  remain). Value it together with #5 and R16. Measured by #5 arm B.

## 11. cancel-all-generation-bump: O(1) cancel-all per (trader, book) with lazy reclamation

- **Merged from:** cancel-all-generation-bump.
- **Corrected gain:**
  - Gross: phase 1 -1.1..-1.2 ms/blk.
  - Net: -0.5..-0.9 ms/blk, after paying for a 3-4-probe bump per hit book, per-book sum upkeep on about 21k
    rest/fill events per block, and deferred index removal.
  - matched/s +0.5..+1.5%.
  - Window-close blocks get 13-28 ms longer, so block p99 gets slightly worse. Deep gains only about 0.27 ms more.
- **Confidence:** low. **Consensus-visible:** no, but every hashed or consensus reader must filter by generation:
  bbo_row, would_cross, band_reference, the modify cross, the RO sweep via get_order and the cloid duplicate check.
  Otherwise a restarted node diverges.
- **Rationale:**
  - Phase 1 is now mostly pool wait (8u: 0.033 ms/1k on-CPU vs 0.149 off-CPU), so the E saving is about a third of
    the claim.
  - Top-of-book reads need a stale-front purge that E waits on, and row modes stay eager.
  - 8-12 days of work with a fork-class risk.
- **Cheapest test:** extend the `#[ignore]` probe at `cancel_batch/many_tests.rs:479` to 300 books (104k and 1.1M
  resting orders). Time plan+apply against bump + top purge + window retain/100.

## 12. classic-cold-level-rope: zero-copy Classic blob from cached cold-level segments

- **Merged from:** classic-cold-level-rope.
- **Corrected gain:**
  - Deep only. The %100 save on E drops from about 60-65 to 7-12 ms per window, about 0.5 ms/blk, so deep matched/s
    +0.8..+1.0%.
  - The rope's own increment over the parked step-3 cache is only 0.1-0.2 ms/blk.
  - Std about 0.
- **Confidence:** low-medium. **Consensus-visible:** no. A deterministic invalidation bug would write the same wrong
  blob on every node, so it needs an O(levels) segment check and a rotating reference encode.
- **Rationale:** it depends on un-parking the Phase 5 step-3 cache (owner-adjacent) and overlaps #6 on the %100 clone.
  Most of the gain comes from the step-3 cache with a journal that persists across the window. Ranks behind #6.

## 13. exec-smt-core-cookie: core-scheduling cookie on torus-execution (and hotstuff-algo)

- **Merged from:** exec-smt-core-cookie.
- **Corrected gain:**
  - matched/s about -1..+1.5%.
  - The SMT plus L3 excess is at most 1.9-2.8 ms/blk, and 62-71% of it falls in seconds where exec is not binding.
  - Core scheduling shares the core fairly, so exec can lose the core-wide pick.
  - The cookie is inherited by the exec pool, end-resident and ckpt-merge (about 2.3 cores forced idle) unless reset.
- **Confidence:** low. **Consensus-visible:** no.
- **Rationale:** tested as arm C of #9.

## 14. mark-version-sums-prefill: async prefill of BlockSums memo cells on a mark-version change

- **Merged from:** mark-version-sums-prefill.
- **Corrected gain:**
  - 0 in every standard and deep cell (ORACLE_WALK_BP=0).
  - With walk>0, at best +1..+3% (the moving-mark penalty is 2.5-4%). Most rebuilds already run on the exec pool via
    maker_free.
- **Confidence:** low. **Consensus-visible:** no. Helpers must read only Arc<Users> and the mark table, never the
  overlay, and need a generation check against renew_mark_version.
- **Rationale:** only relevant if P2 makes the product mark price move every block. Park until then.

## 15. phase2-order-prep-lookahead: state-free Phase-2 per-order constants computed while blocks queue

- **Merged from:** phase2-order-prep-lookahead.
- **Corrected gain:** E -0.5..-0.8 ms/blk; about 0.3-0.5 ms once cheaper inline cuts are taken. matched/s
  +0.4..+0.6%. Shrinks to about 0.3 ms if #18 lands.
- **Confidence:** low. **Consensus-visible:** no. Compare per-market tick, lot and cfg against the live Phase2Market,
  not a generation tag.
- **Rationale:** most of the payload is either state-dependent (basis, bid floor, marks), already cheap, or reachable
  inline (dense market ids exist). The cold-params premise fails because E reads params earlier anyway.

## 16. stop-index-cancel-paths: per-book stop id/trader index

- **Merged from:** stop-index-cancel-paths.
- **Corrected gain:**
  - Bench 0.
  - With 300k parked stops, a failing by-id cancel costs about 1-3 ms (19-38 MB streamed). An index, or a binary
    search on the id-ascending Vec, brings it to 10-30 us.
  - Unchanged: the unexplained 2-3 ms per by-id action, and trigger_stops' O(S) cost per filling placement.
- **Confidence:** low-medium. **Consensus-visible:** no.
- **Rationale:** the simplest fix is a binary search with no new state, since restore already enforces id order.
  Measured by #5 arm C.

## 17. exec-block-arena: per-call bump arena for engine transient containers

- **Merged from:** exec-block-arena.
- **Corrected gain:**
  - E -0.2..-0.5 ms/blk; matched/s +0.3..+1%.
  - 62-70% of the 8u engine faults came from merge_disjoint, which L2.2 removed on main.
  - The parallel pre-profile shows 3-10 exec-thread faults per 1k fills under jemalloc, about 0.1-0.3 ms/blk.
- **Confidence:** medium. **Consensus-visible:** no. Bump is !Sync, so market_batches cannot use it.
- **Rationale:** low yield. The in-progress prototype (`proto/exec-block-arena`) should stop after its pre-profile.
  Keeping the Vecs in the context and calling clear() gets most of what remains.

## 18. fixedpoint-reciprocal-division: multiply-high by precomputed reciprocals instead of __divti3

- **Merged from:** fixedpoint-reciprocal-division.
- **Corrected gain:** E -0.1..-0.3 ms/blk. __divti3 is 1.31% of exec CPU, about 0.42 ms upper bound. matched/s
  <+0.5%. Only a microbench can see this.
- **Confidence:** medium. **Consensus-visible:** no. The result must be bit-exact, and the reciprocal cache must be
  keyed by divisor value.
- **Rationale:**
  - In s104 a single-mul magic divide saved only 2.5 of 17.3 ns per op.
  - placement_need already computes IM once.
  - The position_terms common-subexpression part is a free micro-cut; fold it into any margin change.

## 19. resting-order-compaction: 176 B -> 80-96 B resting record

- **Merged from:** resting-order-compaction.
- **Corrected gain:** +0.1..+0.4% exec on deep, about 0.1-0.2% on std. Deep RSS -105 MB (-2.4%).
- **Confidence:** medium. **Consensus-visible:** no.
- **Rationale:** the deep preload is never touched by load cancel-alls or matches, so the deep-gain premise is false.
  A wide refactor for about 0 throughput.

## 20. gossip-pubkey-batch-msm-verify: ship the signer key, batch-verify each frame with one MSM

- **Merged from:** gossip-pubkey-batch-msm-verify.
- **Corrected gain:** CPU-s/1M -3.5..-4% against today's k256, but only -0.2..-0.5% beyond the simpler libsecp
  backend (proposed, KEEP). matched/s 0.
- **Confidence:** medium. **Consensus-visible:** no.
- **Accept-set risk:** signatures with v=29/30 (recid 2/3) must lift x=r+n or go to the fallback. Otherwise a
  byzantine peer can poison one node's trust cache and halt the n=3 chain.
- **Rationale:** do libsecp first. This is not worth 7-10 days on top of it.

## 21. gossipsub-native-anonymous-instance: unsigned, content-addressed second gossipsub instance for native frames

- **Merged from:** gossipsub-native-anonymous-instance.
- **Corrected gain:** CPU-s/1M -1.2..-1.4%. matched/s, blk/s and p99 about 0. Consensus would get its own per-peer
  send queue as a side effect, unmeasured.
- **Confidence:** medium. **Consensus-visible:** no. Use a collision-resistant id (blake3 or sha256, not xxh3), and
  never penalize validator forwarders.
- **Rationale:** overlaps native-prespread-direct-zstd, which saves the same CPU and more.

## 22. feed-ahead-commit-prep: prepare the commit feed at block insert on its own thread

- **Merged from:** feed-ahead-commit-prep.
- **Corrected gain:**
  - matched/s ≤+0.5%, likely 0.
  - block p99 -1..-3%; commit-age p99 at most -5%.
  - Phase-A view -10..-18% (an internal metric).
  - Exec is not underfed in phase A, and phase A is an early-window transient.
- **Confidence:** medium. **Consensus-visible:** no. The in-flight ledger evicts heights more than 10 below the
  newest, so the off-path prune needs its own pending set.
- **Rationale:** the hash part repeats the rejected consensus-reuse-action-hashes. The new part (an off-path prune) is
  bound by lock contention.

## 23. lazy-commit-persist-ticket: exec stops waiting on the T5.3 ticket

- **Merged from:** lazy-commit-persist-ticket.
- **Corrected gain:**
  - matched/s +0.03..+0.09% (std) and +0.15..+0.34% (deep). The stall is absorbed today: the exec queue drains to 0
    afterwards.
  - Removes about 10-40 stalls of 100-500 ms per node per cell, which only helps the exec-age tail.
- **Confidence:** high. **Consensus-visible:** no. Three places need in-memory parents: the silent parent-link check
  (detect_parent_link_violation passes when the row is missing), the epoch-boundary fee header_at, and
  derive_feature_activation.
- **Rationale:** sits in the owner's T5.3 area, where p99 attribution comes first. Low yield.

## 24. commit-inclusion-stream: commit-time hash-only inclusion push, bench tail on it

- **Merged from:** commit-inclusion-stream.
- **Corrected gain:**
  - Node about 0; val2 uses 0.5-0.58 fewer cores.
  - The bench may read +0..+5% matched/s, but that is the same harness lever as the parked read-ahead (+4.5-6.5% at
    10m).
  - RD s70.7 showed that freeing the tail makes exec and commit ages much worse in the closed loop.
- **Confidence:** medium. **Consensus-visible:** no.
- **Rationale:** useful as a product or client feature (inclusion notice), not as a perf lever. It also changes the
  measuring stick.

## 25. book-blob-cdc-store: node-local content-defined chunk store for book blobs on W

- **Merged from:** book-blob-cdc-store.
- **Corrected gain:** deep checkpoint batch 160 -> 85-95 MB, W db.write -140 ms per checkpoint, WAL -20 MB/s per
  node. matched/s 0..+0.7%. Block p99 about 0. E's O(depth) work is unchanged.
- **Confidence:** medium. **Consensus-visible:** no. Raw book-CF readers bypass any shim (13+ sites), and the chunk id
  must be keyed or cryptographic.
- **Rationale:** storage hygiene only. The z2 cap and waloff precedents already showed that a smaller batch does not
  move matched/s.

## 26. ckpt-batch-presize: WriteBatch::with_capacity_bytes for the checkpoint batch

- **Merged from:** ckpt-batch-presize.
- **Corrected gain:** W build -20..-25 ms (std) and -35..-50 ms (deep) per checkpoint; transient RSS -40..-80 MB.
  Target metrics 0.
- **Confidence:** high. **Consensus-visible:** no. An under-estimate is worse than today, because the batch then
  doubles from the reserved size. Clamp the hint.
- **Rationale:** a half-day hygiene item. Bundle it into #6 if that proceeds. Skip it when trie maintenance or the EVM
  prefix is on.

## 27. l3-cat-exec-partition: resctrl CAT/MBA for the exec path

- **Merged from:** l3-cat-exec-partition.
- **Corrected gain:** matched/s 0..+1% with a 75-25 mask; a 50-50 mask is likely negative; MBA gives 0. Exec plus pool
  already cause 55-70% of L3 refills.
- **Confidence:** low. **Consensus-visible:** no. Needs root (mount resctrl).
- **Rationale:** monitoring-only CQM/MBM would answer the open RD s103.3 question, but the yield case is weak and
  depends on the bench layout (3 nodes per host).

---

## Appendix A: killed in round 1 (one line each)

- **exec-groups-virtual-block** (exec-amortize): killed by skeptic majority. It is the same exec-side lever as bigger
  consensus blocks (block-cap-400 merged, block-cap-1000 rejected, cap-sweep-uncapped no gain), and #1
  exec-commit-units supersedes it.
- **liq-scan-row-existence-fastpath** (margin-algo): already tried. cooldown-pending-row-index measured no gain at
  0.35 ms/blk, and the typed balance load is the merged Phase 4 mechanism.
- **exec-group-fusion** (exec-stage-pipeline): killed by skeptic majority. Near-duplicate of
  propose-defer-on-exec-queue (parked) and the exec-throttle watermarks (rejected); its viable form is #1.
- **streamed-end-resident** (exec-stage-pipeline): killed by skeptic majority. It falls inside end-resident-on-worker
  plus cut 5 (both merged); what remains lives in #8.
- **vote-extension-oracle** (lit-bft): fatal on gain. begin_block_oracle still runs every block, so both the speed
  premise and the latency premise are false.
- **stale-mark-preopen** (lit-exchange): killed by skeptic majority. Deep-review R08 option A already covers it, and
  the current stale-mark policy (band reference clamped to the last fresh mark) is a deliberate design choice.
- **exec-heap-hugepage-retention** (contrarian): fatal on gain. In jemalloc 5.3.1, thp:always does not keep hugepages
  across purges. The idea survives as #4 (the HPA arms).
- **stop-trigger-budget-carryover** (robustness): already covered by deep-review R09 (incremental stop indexes,
  bounded trigger queue) and R16 option B (deterministic work budget with continuation).
- **deep-book-capital-floor** (robustness): already decided or proposed: resting-order-cap (D14 b, decided) and
  resting-order-ttl-gc (proposed).
- **nonce-hwm-db-skip** (storage-durable): killed by skeptic majority. Close to batch-nonce-replay-guard (proposed)
  and to the owner's s119 rejection of an in-memory nonce set (decision 2a).

## Appendix B: coverage gaps for round 2

1. **Elasticity calibration.** Every ranking above depends on the exec-ms -> matched/s conversion: 0.3 is assumed,
   and s124 suggests about 0.
   - Measure it directly: inject a fixed spin of +2 ms and +4 ms on the exec thread per block, plus a matching
     negative arm, in 120 s cells, and read dmatched/s and dblk/s per exec ms.
   - Do the same for CPU on other threads: a synthetic N-core host load measures the host-contention conversion.
2. **E-side verify (2.62 ms/blk) and replay guard (0.55 ms/blk).** No round-1 survivor targets them: the crypto ideas
   went to ingest, and nonce-hwm-db-skip died.
3. **Settle pass B (3.71 ms/blk) and the rest of the serial margin phase.** Beyond position lookups, about 8 ms of the
   margin phase remains. No structural idea survived outside the excluded PE and Phase 5.
4. **Pool fork-join round-trip cost.** The ECU sweep shows match/1k halving with bigger blocks, so the per-round-trip
   overhead (0.34-0.53 ms wake, last-worker preemption) is material. No idea fuses or removes pool round trips beyond
   L2.1.
5. **Latency lens under closed-loop rules.** Only near-zero latency ideas survived (feed-ahead, inclusion stream, lazy
   ticket). Block p99 (475 ms, consensus-bound) and commit p99 (about 5 s) need a lens that models the closed-loop
   population. infra gap 10 (no open-loop latency curve) blocks the evaluation.
6. **Deep-book.** No strong survivor apart from #6 and #12 addresses deep R vs CL21 blk/s (15.95 vs 21.49), deep
   timeouts (0.46/1k) or deep RSS (4.4-5.7 GB).
7. **Robustness catalogue.** FOK, RO sweep and stop miss were all found by inspection.
   - Round 2 should systematically list every per-action loop over state an attacker can grow: ADL cascades,
     cancel-by-cloid, modify, liquidation cascades, trigger_stops.
   - Size each one in the #5 adversarial bench, under the R16 budget frame.
8. **Contrarian challenge to the bench shape.** The 38.5% cancel-all share, BLOCK_CAP 400 and MAX_IN_FLIGHT 4 were
   never questioned. No idea asked whether the econ mix or the cap is the binding factor.
9. **lit-exchange lens.** It produced no survivors. HL- and CEX-style engine techniques (price-level aggregation,
   maker queues without per-order margin re-checks) remain uncovered.
10. **Users::apply on the end-resident worker.** It costs 2.6 ms/blk off E and is the source of the 1.53 ms E wait;
    only #8(B) touches it.
