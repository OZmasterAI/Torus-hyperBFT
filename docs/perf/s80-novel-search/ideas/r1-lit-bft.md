# r1 lens lit-bft: ideas (novel-s80)

Lens: what recent BFT papers and production chains do, mapped onto `hotstuff_rs` + `torus-consensus/src/app.rs`
and the measured exec-thread ceiling. Main `eb993e3f`, read-only. Numbers come from `map/measured.md` and the
9j2 cells in `~/bench-results-matched` (`metrics-{before,after}-valN.txt`, `summary.json`).

Literature I screened and dropped because the mechanism is already in the ledger or on the owner list:
- 1-chain commit on a unanimous QC (Kudzu / Hydrangea / Votor fast path): `l1-1-unanimous-qc-1chain-commit`.
- Speculative execution at 1-QC (MonadBFT, HotStuff-1, Sei OBP): `l1-2-optimistic-exec-at-1qc`.
- Optimistic or pipelined proposals (Moonshot), stable leader windows (Alpenglow, Bullshark): `multi-block-views`,
  `stable-leader-tenure`, `leader-tenure-2-4-views`.
- DAG BFT and Narwhal dissemination: `dag-bft-mysticeti-shoal` (rejected), `dag-dissemination-narwhal`.
- Tail-forking resistance and TC/tip (MonadBFT): owner list. RaptorCast: T3.1 (owner list).
- Leader reputation (Shoal, HammerHead): `reputation-leader-selection` (rejected).
- Aptos/Sui execution backpressure: `exec-throttle-watermarks` (rejected), `propose-defer-on-exec-queue` (parked).
- Solana CU and gas-style exec metering: `consensus-work-ceilings-r16`.
- Priority lane or direct transport for consensus traffic: `consensus-priority-lane`, `consensus-direct-fan`.
- SafetyRules-style vote fsync: `vote-state-dsync-before-vote`.
- LtHash accumulators: `lthash-response-accumulator` (rejected), `lthash-snapshot-trust` (parked).
- Sui per-object congestion control: covered by the s124 hot-market lens (mem eae79029).

The key measured fact behind idea 1: exec time saved on this chain turns into more, smaller blocks, not into more
matched/s. Evidence:
- RD s67.4: block_ms -10%, then native blk/s +9.5% and matched/s only +3.0%.
- `item6-phase4-impl.md`: section 54 gave blk/s +11.3% with matched/s a tie.
- PE (s128): blk/s +15.7%, matched/s -1.4%.

The per-block fixed costs grow back as blocks shrink. Idea 1 removes that feedback.

---

## 1. exec-commit-units: execute K queued committed blocks as one engine unit

Literature sources:
- **Bullshark / Mysticeti / Sui**: one commit (a sub-DAG of many blocks) is a single execution batch.
- **Sui**: consensus-commit batching.

**Mechanism.**
- **Header flag.** Add a hashed header field `exec_join: bool`. It follows the `features` pattern: appended to the
  canonical bytes when set (`torus-types/src/lib.rs:601,611`), behind a P0.0 Feature.
- **Who sets it.** The proposer sets the flag in `produce_block` (`app.rs:10433`) when its own exec queue
  (`exec_queue_len`, `app.rs:6880`) is at or above W (for example 2).
- **Unit rule.** A unit is the maximal run of consecutive committed heights where every block except the last has
  `exec_join`. It is capped at K = 4 blocks and by the per-block caps (`rate_limit.rs:97,250,309`). A unit never
  crosses `h % 100`, an epoch boundary, or a block with EVM or governance content.
- **Execution.** `execution_loop` (`app.rs:6816`) receives the following messages while the head block has the flag
  set. It waits on all the persist tickets, then calls `execute_committed_block_with` (`app.rs:4506`) once over the
  concatenated actions. The unit gets one verify, one sort (`app.rs:5585`, key = (category, height, sender, hash)),
  one oracle step, the two `execute_batch` calls (`app.rs:5806-5807`), one tail, one save, one `end_resident`, one
  flush and one hash. All K headers and the applied marker at the last height are written.
- **What is unchanged.** Consensus blocks, views and commit latency.

**Why it helps.** Per-block fixed exec costs are about 12.6 of 37.9 ms (33%) and are paid once per unit. The exec
unit size is no longer tied to the view cadence. The exec queue is p50 9 / p90 42 blocks (std CL21), so joining
applies most of the time under load.

**Gain arithmetic.**
1. RD s110.4 measured the amortisation directly: blocks 4x smaller (62k to 15.2k fills) raised per-fill cost to
   1.20x for engine, 1.21x for verify, 1.15x for settle and 1.13x for margin.
2. Solving t = F + c·n gives these fixed shares at 11.5k fills: engine 27% (8.45 of 31.31 ms), verify 28%
   (0.73 ms), margin 19%, settle 22%.
3. Non-engine per-block costs are about 3.4 ms: `end_resident_wait` 1.49, upkeep 0.74, save 0.58, checkpoint
   wait 0.17, residual plus load_books 0.42.
4. So F is about 12.6 ms. Without the 1.9 ms per-block liquidation part (the scan budget is kept per block) it is
   about 10.7 ms.
5. K = 2: (2 × 37.94 - 10.7) / 23.1k fills means exec time per fill -14%, so exec capacity +16%. K = 3: -19% per
   fill, +23% capacity.
6. Matched/s gain:
   - Exec-bound with the feedback removed: +16% (K = 2) to +23% (K = 3).
   - If s67.4's 0.3 elasticity applies anyway: +5% to +7%.
   - Exec on-CPU per 1k fills (the merge-rule metric) falls about 14% either way.
   - Off-thread work (end_resident 4.0 ms, state write 4.3 ms, hash) also falls per unit, which relieves the shared
     host.

**Nearest prior work** (I grepped ledger and memory for super-block, multi-block, coalesc, amortis, exec unit,
backlog, catch-up, propose-defer):

| ledger entry | status | difference from this idea |
|---|---|---|
| `propose-defer-on-exec-queue` | parked | The leader delays its proposal, so consensus cadence changes. |
| `exec-throttle-watermarks` | rejected | Makes blocks thinner, the opposite direction. |
| `multi-block-views` | proposed | Consensus-side. |
| `block-cap-100`, `block-cap-25-50` | tried | Caps only; they show that small blocks lose throughput. |
| `evm-big-blocks-height-cadence` | merged | EVM lane only. |
| L3.1 / Phase 3 coalescing | merged | Coalesces writes and saves, but execution is still per block. |
| `exec-3stage-pipeline` | rejected | Pipelines stages; does not merge blocks. |

None of them makes the execution unit span several committed blocks while consensus stays unchanged.

**Risks.**
- **Semantics.** The unit behaves as one block. Cancels in block n+1 are hoisted above GTC orders in block n inside
  the unit, which is HL-style cancel priority widened from about 1 view to at most K views. This only happens when
  exec is already more than one block behind.
- **Latency.** The head of the unit waits for the next commit (+1 view) if the queue drains.
- **Byzantine proposer.** It can always set the flag. K bounds the harm.
- **Elasticity.** The 0.3 elasticity may be bench closed-loop and not fixed-cost feedback.
- **Plumbing.** Replay, applied marker, header persistence, RPC height attribution, running-hash and attestation
  definitions per unit, the oversize-batch skip (`native_executor.rs:8341`, which must keep per-block caps), and
  interplay with batch 10 L6.1.

**Effort.** 12-18 person-days.

**Prototype (≤ 3 h).**
1. ubench_econ needs no code change. Settings: `UB_MARKS=1 UB_MARK_WALK=10 UB_R_WORKER=1 UB_SENDERS=5000 UB_RUNS=3`.
   Sweep:

   | UB_ACTIONS | UB_WARM | UB_MEASURE |
   |---|---|---|
   | 60 | 40 | 12 |
   | 120 | 20 | 6 |
   | 240 | 10 | 3 |

   Compare `engine_ms/1k_fills` and `r_end/blk`. **Go** if 120 ≤ 0.92 x 60. **Refuted** if it is ≥ 0.97 x.
2. Optional test-only hack: pair heights by parity in `execution_loop` (deterministic on all 3 nodes). Run one
   120 s 300m local cell A/B. Read matched/s, exec on-CPU per 1k fills, `order_age_ms.exec` p50 and exec queue
   depth.

---

## 2. lagged-liq-scan-eah: Solana-EAH-style lagged liquidation scan off the exec thread

Literature sources:
- **Solana's Epoch Accounts Hash**: a background job over a fixed-slot snapshot, consumed at a later fixed slot.
- **Monad**: execution lag D.

**Mechanism.**
- **Today.** `run_liquidations` (`app.rs:5888`; `liquidation_step.rs:150-178`, pass `:402`) classifies the
  2,048-trader window (`liq_traders_after` `:547`, `liq_view` `:786`) serially on E at the end of every block.
- **Change (consensus-visible, behind a Feature).**
  1. At the end of block n, E hands the job "classify the window at cursor_n on state(n)" to a worker. State(n) is
     the block's record set R plus the block marks, which stay read-only during block n+1
     (`native_executor.rs:4732` `begin_resident`, `:6297` `attach_resident_block`).
  2. The worker computes healthy/unhealthy verdicts and the cursor advance while block n+1's engine runs.
  3. At block n+1's tail, E joins the worker. It re-checks only the at most 64 non-healthy traders (plus ADL-touched
     ones) on current state, acts on them, and writes the cooldown, pending and cursor rows.
- **Replay.** Crash replay from checkpoint c recomputes scan(c) from state(c), so no extra rows are needed.

**Gain arithmetic.**
- Measured `torus_exec_liquidation_step_seconds` (9j2 metrics, after minus before): 3.57-3.66 ms/blk std CL21,
  3.60-3.77 deep CL21, 4.33-4.37 deep R. That is 9.5% of the 37.9 ms exec block.
- E keeps about 0.3 ms (join, acts, writes), so E saves about 3.3 ms/blk: -8.7% exec block and -8.7% exec on-CPU
  per 1k fills.
- Matched/s: +2.6% at s67.4's 0.3 elasticity, up to +9.5% if exec-bound (for example together with idea 1).
- Empty and oracle-only blocks: `run_liquidations` was 5.25 of 6.35 ms (RD row 78), so about -80% exec per idle
  block.

**Nearest prior work** (I grepped liq, liquidat, lag, defer, off-thread, worker, pipelin):

| ledger entry | status | what it does |
|---|---|---|
| `liq-scan-parallel-prevalue` | rejected (mem 966f49e6) | Parallel inside the block on the exec pool; per-trader mutex/RwLock contention; +0.6-0.8%. |
| L1.3 | proposed | Budget per time. |
| `liq-scan-skip-unchanged-traders` | proposed | Skips traders that did not change. |
| `liq-l2-health-certificates` | proposed | Skips provably healthy candidates. |
| `liq-touched-accounts-first` | proposed | Changes scan order. |
| `liq-l3-every-account-every-block` | parked | Scans every account. |

All of these shrink or split the scan inside the block. This idea takes it off E's critical path with a fixed
one-block lag and its own read-only snapshot.

**Risks.**
- Detection is one block (about 45 ms) later. That is negligible next to the 49-block full sweep of 100k accounts.
- The read-only premise for R / records breaks if Phase 5 step 2 (in-place Users mutation, parked) lands.
- Per-scanned-trader writes (`clear_cooldown`, `set_pending`) must stay O(changed).
- Needs differential determinism tests.
- The worker still uses shared host CPU.

**Effort.** 6-9 person-days.

**Prototype (≤ 3 h).**
1. ubench_econ with `UB_MARKS=1 UB_MARK_WALK=10`: read `tail(liq)/1k`.
2. Harness-only timing: in `run_once`, run block h's `run_liquidations` on a snapshot in a `std::thread` while
   block h+1's `execute_batch` runs. Compare E wall per block with and without. Count liquidation-row writes per
   block from `LiqStats`.

---

## 3. vote-extension-oracle: oracle prices ride the consensus votes (ABCI++ vote extensions)

Literature source: CometBFT ABCI++ vote extensions, as used by dYdX v4 / Skip Connect.

**Mechanism.**
1. Each validator attaches its latest feeder price vector to `PhaseVote` (`hotstuff/messages.rs:246`). The vector is
   delta-coded against its previous extension and signed separately. Its hash is also covered by the vote, so
   `PhaseCertificate` (`hotstuff/types.rs:34`) carries per-signer extension hashes.
2. The collector (leader v+1) puts the justify votes' extensions into B_{v+1}'s CompactBlock. Validators check them.
3. `begin_block_oracle` (`native_executor.rs:14391`) takes the median over the in-block vectors. It writes
   latest-per-validator rows only when they change.
4. This replaces the `SubmitOraclePrices` actions (`native_executor.rs:8075`), the prune scan (`oracle.rs:408`)
   and the special oracle mempool class.

**Gain arithmetic.**
- **Performance (small).** Oracle-only blocks are 39 per cell (avg 6.87 ms, p95 21.6 ms), and they go to 0.
  `begin_block_oracle` is 0.83 ms per empty block, and at most about 1 ms per loaded block would be saved: at most
  2.6% of the exec block, so +0.8-2.6% matched/s.
- **Robustness (the main gain).**
  - Oracle input latency drops from the action path (commit age p50 344 ms / p99 5,022 ms under load) to one view.
  - Marks cannot go stale while consensus is live.
  - With the vote covering the extension hash, a leader cannot drop a validator's price.
  - Removes the oracle duty exemptions and the s124.2 ms-nonce collision class.

**Nearest prior work.** I grepped vote extension, piggyback and oracle; nothing puts oracle data in consensus
messages.

| ledger entry | status | what it does |
|---|---|---|
| `mempool-priority-classes-oracle` | merged | Gives oracle actions a priority class. |
| `oracle-feeder-hot-signer` | merged | Separate feeder process and hot signer. |
| `agg-rows-rewrite-only-on-change` | proposed | Rewrites aggregate rows only on change. |
| `oracle-off` | decided | Measured the cost of disabling the oracle. |

**Risks.**
- Changes the hotstuff_rs wire format and certificate format.
- TC views carry no extensions.
- About 14 KB per vector change every 2-3 s.
- Changes the key model (feeder hot signer).
- Departs from HL parity (HL uses oracle transactions).

**Effort.** 10-14 person-days.

**Prototype (≤ 3 h).**
1. Time `begin_block_oracle` in ubench_econ `UB_MARKS=1` (REPORTERS submit every block) against a stub that takes
   the median over in-memory vectors.
2. From the 9j2 node logs, extract the commit/exec age distribution of the oracle senders' actions.
