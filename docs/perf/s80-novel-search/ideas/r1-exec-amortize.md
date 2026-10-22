# novel-s80 r1, lens exec-amortize (per-block fixed exec costs, deep exec queue)

Main `eb993e3f`, read-only. All numbers are from `map/measured.md` (std CL21 unless marked) and the results doc
`docs/perf/ozarchy-antispam-item6-pf1-2026-10-04.md` (RD). Ledger checked by grep over `ledger/*.md` (terms listed
per idea).

Finding behind the ranking: with per-block results kept identical (and the per-block running hash, which folds
every block's change set: `crates/torus-state/src/running_hash.rs:1-5`), the deep exec queue buys little. The block
boundary forces one verify/sort/engine/flush/tail/freeze/hand-off/end_resident cycle per height. The exact
options are already in the ledger (L6.3 lookahead verify, exec-prep-stage, cooldown-pending-row-index,
health certificates, exec-carried senders), or they are small. The two large wins below change consensus
semantics and need a P0.0 feature bit.

---

## 1. exec-groups: execute a run of committed blocks as one virtual block (consensus-visible, bold)

**Mechanism.** Add a hashed header field `exec_group_open` (`TorusBlockHeader`, `crates/torus-types/src/lib.rs:520`,
appended to the canonical bytes only when set, as `features` is). The proposer sets it when its exec backlog is
deep: the same `exec_queue_len + deferred_exec` read the selection path already does (`app.rs:11116-11119`).
`execution_loop` (`app.rs:6816`) keeps pulling committed blocks while the last one is open, up to `G_max`
(e.g. 4). It stops at barriers (EVM txs, slashes, epoch boundary, `h % 100`). The group then executes once, as
one block whose action list is the union of the members' lists:
* per-member checks;
* one verify, replay-guard and sort pass;
* one `begin_resident` / `begin_block_oracle` (last member's timestamp);
* one pre/post `execute_batch`;
* one tail, with the scan budget at 2,048 x members;
* one save, freeze, hand-off and `end_resident`;
* one running-hash link.

Action status and fills map back to member heights by body position.

**Why.** The per-block fixed cost is paid once per G blocks. That covers the per-call engine setup, the pool
fork-joins and per-market epilogues, the oracle step, end_resident, upkeep and the save. A maker filled in
several members also has its rows flushed once.

**Target.** matched/s, native blk/s, exec age p50/p99.

**Expected gain (assumed split, not measured).** Assumed per-block E fixed cost F:
* oracle step <=1.54 (engine untimed);
* end_resident_wait 1.53;
* upkeep + ckpt wait 0.90;
* save_books 0.57;
* liquidation per-step overhead ~0.3;
* per-call engine overhead 3-4.5 (open_order_counts ~1.1, settle epilogue ~1.26, fork-join stragglers);
* flush dedupe ~1.2.

Total F ≈ 8-11 ms of the 38.35 ms exec block.
* G=3 saves 2/3 x F = 5.3-7.3 ms per block (14-19%), so exec capacity goes x1.16-1.24.
* G=2 saves 4.0-5.5 ms per block, so x1.12-1.17.
* With 8u's engine fit (16 ms fixed per block, deep), G=3 saves 10.7 ms per block, so x1.39.
* Expect matched/s +10-20% on the exec-bound 300m shapes.

The queue supports G=2-4 most of the time (depth p50 9 std / 17 deep, p90 41-47). W also gets relief: state
write 4.2-7.3 ms and hash 1.5-2.2 ms per block become one job per group with deduped keys.

**Evidence.**
* O2 puts the fixed cost at ~11-12 of 38 ms.
* Early vs late window: 53.6 vs 35.9 ms per block at 23.3k vs 14.0k placed. A linear fit gives an intercept of
  9.3 ms, and the resting-depth confound biases this low.
* 8u engine fit: 16 ms + 2.47 ms per 1k fills.
* Smaller blocks lose throughput: exec-throttle-watermarks -25% matched/s; block-cap-25/50 -25 to -53%;
  block-cap-100 -5.4% at 2.16x cadence.

**Nearest prior** (grepped multi-block, super-block, coalesc, group, fuse, bigger block, block size, cadence,
exec queue, backlog):
* `propose-defer-on-exec-queue` (parked): the leader withholds proposals, which interacts with view timers.
* `exec-throttle-watermarks` (rejected): thinner blocks, the opposite direction.
* `multi-block-views` (proposed): more blocks per view.
* `block-cap-*`.
* L1.3 (liquidation only).

Difference: consensus cadence, DA and commit latency are untouched. Committed blocks are fused only at exec,
sealed by a proposer bit.

**Risks.**
* Semantics change: cancels-first and one mark apply across the whole group. This is equal to what a bigger
  block already allows.
* The first member's actions wait for the later members to commit, which only happens when the backlog is deep.
* A TC inside a group stretches that wait.
* A Byzantine proposer can set the bit on every block. `G_max` bounds it.
* The running hash is per group: attest heights close groups.
* RPC height granularity becomes the group.
* Crash replay must regroup from the headers.
* A heavier single exec call per group.

**Effort.** 10-15 person-days.

**Prototype (<=3 h).** Run `crates/torus-bridge/tests/ubench_econ.rs` with:
* `UB_MARKETS=300 UB_MARKS=1 UB_MARK_WALK=10 UB_R_WORKER=1 UB_SENDERS=5000`;
* `UB_ACTIONS` set to 63, 126, 189 and 252, with total actions held constant via `UB_MEASURE`.

Record ms per block (engine incl. liquidation, plus the end_resident wait) and fills per block. Fit ms = F + c x
fills. Go if F >= 6 ms and c stays flat across sizes. Optional direct check: concatenate k generated blocks'
action lists into one `execute_batch` inside the harness and compare with k sequential blocks.

---

## 2. Liquidation classification at block start, on a helper thread overlapping the engine (consensus-visible)

**Mechanism.** Today the step classifies the 2,048-trader window after the engine, serially on E. It uses
post-block rows and the block-start mark (`app.rs:5877-5889`; `liquidation_step.rs:402-540`, `liq_view`
786-826).

New rule: classify on the block-start state. That is the same mark as today, with positions and balances as of
the end of block h.
* Right after `begin_block_oracle` (`app.rs:5788`), E sends a long-lived `torus-liq-scan` thread these inputs:
  Arc clones of R and the Users record set (immutable during a block, only `end_resident` mutates them through
  `Arc::get_mut`), the mark table and the cursor.
* The helper walks the window (`traders_after` over the Users slice) and computes health. It also reports
  whether cooldown or pending rows exist.
* At the tail, E joins the helper, which is done long before the tail. It applies the cursor and row ops, then
  re-checks only the flagged (non-healthy) traders on post-engine rows before acting.
* ADL drain and the vault check stay on E.

**Target.** matched/s, native blk/s, light-block latency.

**Expected gain.**
* The liquidation step costs 1.91 + 0.127 x 11.5k fills = 3.37 ms per block (tail 3.65).
* About 0.35 of that is the row reads, the rest is `liq_view`/`pos_sums`.
* E keeps ~0.2-0.3 ms, so it saves ~3.1 ms per block: 8.1% of 38.35 ms, exec capacity +8.8%.
* Deep is similar: tail 3.77.
* Expect matched/s +5-9%.
* No-action blocks drop from 6.35 ms to ~1 ms (`run_liquidations` is 5.25 of it, RD row 78).

**Evidence.**
* `liquidation-step-timer` (s110): 1.91 ms per block + 0.127 per 1k fills.
* RD section 7: 94% of the step is `liq_view -> pos_sums -> direct_sums`.
* `cooldown-pending-row-index` measured 0.35 ms per block for the row reads.
* The window covers ~2/5 of ~5k position holders per block, and ~90% of holders are dirty each block. So an
  exact "re-value only dirty" variant would recover only ~10% at the bench. The consensus rule is what moves the
  cost.

**Nearest prior** (grepped liq, liquidat, L1.3, certificate, prevalue, cooldown, pending, async, off-thread,
snapshot):
* `liq-scan-parallel-prevalue` (rejected s123): fork-join on the exec pool after the engine, so E waited; the
  overlay RwLock and memo Mutex were taken ~4x per trader; the gain was sized at 206-239 ms blocks.
* `liquidation-health-certificate`, `liq-l2-health-certificates` (proposed).
* L1.3 (proposed).
* `liq-touched-accounts-first` (proposed).
* `cooldown-pending-row-index` (no gain).
* `liquidation-sweep-moving-marks` (parked).

Difference: no fork-join, no shared locks, and an immutable block-start snapshot. The block is now 38 ms, so
the step's weight has grown ~4-6x since the rejection.

**Risks.**
* Semantics change: a window member pushed under maintenance margin by this block's own fills is caught at the
  next sweep, ~2.5 blocks at the bench and ~49 at 100k holders.
* The helper must use exactly the same arithmetic as the inline path (share the code).
* If Phase 5 in-place Users mutation is ever revived, it breaks the immutable snapshot.
* A helper panic means fail-stop.

**Effort.** 5-8 person-days.

**Prototype (<=3 h).** In `ubench_econ` (`UB_MARKETS=300 UB_MARKS=1 UB_MARK_WALK=10 UB_R_WORKER=1`):
1. Time `run_liquidations` per block as it is today.
2. Hack in a std thread that classifies the window over Arc clones of R and Users after `begin_block_oracle`.
3. Join at the tail and measure E tail ms (go if <= 0.4), helper ms (must be below the engine ms) and how many
   classifications differ from the inline pass (expected 0).

---

## 3. Time-quantised oracle step memo (exact, node-local)

**Mechanism.** `begin_block_oracle` (`native_executor.rs:14391-14478`) runs every block:
* `prune_submissions`: iterates and decodes all ~900 submission rows (`oracle.rs:408-422`);
* `collect_submissions` for each of 300 markets, then median and outlier cut (`oracle.rs:286-370`);
* a fresh aggregate-row write;
* `aggregated_market_ids`, then `BlockMarks::read`.

Header time is in whole seconds (`eip712.rs:56-58`). So about 22 consecutive blocks share `now`, and their inputs
only change on a new submission (~1.5 feed actions per second).

Memo, kept in the resident holder:
* Key: an oracle-input epoch, bumped by the handlers that write submission, staking, listing or oracle-config
  rows. Also a next-expiry second: the minimum over submission timestamp + window and aggregate timestamp + max
  age.
* On a hit, skip prune, collect, median and the mark-table rebuild (version unchanged). Rewrite only the aggregate
  rows that were fresh, by patching block_number (bytes 16..24) and timestamp (bytes 28..36) of the cached
  36-byte rows (`oracle.rs:126-145`).
* State stays byte-identical.

**Target.** matched/s, native blk/s.

**Expected gain.**
* Per-block cost: <=1.54 ms (engine untimed), 0.83 ms on no-action blocks (RD row 78).
* Hit rate ≈ 1 - 1.5/22 ≈ 93%.
* Saving ≈ (cost - 0.1 ms for 300 patched puts) x 0.93 = 0.65-1.3 ms per block, i.e. 1.7-3.4%.
* matched/s +1.5-3%.

**Nearest prior** (grepped oracle, aggregat, mark table, memo, per-second, prune):
* `agg-rows-rewrite-only-on-change` (proposed, consensus change that drops writes).
* `oracle-single-scan-e1` (merged).
* `e1-oracle-step` (parked).
* `oracle-collect-bounded` (proposed).
* `block-mark-table` (merged).
* `oracle-off` (decided, +4%).

Difference: an exact memo that still writes the rows. No consensus change.

**Risks.** A missed invalidation source causes divergence. Mitigations:
* a shadow mode that compares memo output with the full path;
* clear the memo on any barrier, replay or R rebuild.

The gain is bounded by the untimed 1.54 ms.

**Effort.** 2-3 person-days.

**Prototype (<=3 h).** In `ubench_econ` with `UB_MARKS=1 UB_MARKETS=300`:
1. Time `begin_block_oracle` per block.
2. Add a feed-cadence knob (submit every 15th block, advance the second every 22 blocks) and the memo behind an
   env flag.
3. Assert identical per-block digests (`perf_equivalence_golden` pattern) and record ms per block saved.

---

## 4. Start the end_resident worker at freeze, before hand-off and upkeep (exact, cheap, small)

**Mechanism.** In the pipelined branch, E currently runs these steps in order:
1. freeze (`app.rs:6243`);
2. `pipeline_handoff` (6254);
3. `checkpoint_upkeep` (6274, mean 0.72 / p99 16.7 ms std; 1.24 / 30 deep);
4. only then `end_resident_on_worker` (6434).

The next block's `begin_resident` joins that worker, and E waits 1.53 ms per block there.

The worker needs only R detached and the frozen set (`BlockDelta::Frozen`). The upkeep touches overlay layers
and parent, not R, and the L3.1 close `try_lock`s only the books holder (`app.rs:4325`), which E releases after
the hand-over. So move `end_resident_on_worker` to directly after the freeze. On a failed hand-off or upkeep,
invalidate the slot: both paths are terminal today.

**Target.** matched/s, native blk/s.

**Expected gain.**
* The wait shrinks by the head start (hand-off + upkeep): ~0.4-0.9 ms per block std (1-2.3%).
* 0.6-1.3 ms deep (1.5-3.2%).
* On every %100 close the wait goes to 0.

**Nearest prior** (grepped end_resident, begin_resident, upkeep, hand-off, freeze):
* `end-resident-on-worker` (merged: worker plus late `begin_resident`).
* `exec-prep-stage` (rejected: about 0.4 ms of extra overlap).
* `end-resident-reuse-decoded`, `cheaper-delta-drop-offthread` (merged).

Difference: give the worker a head start instead of adding work on E.

**Risks.**
* A hidden ordering dependency between upkeep and R.
* Three background threads (W, ckpt-merge, end-resident) start together on an oversubscribed host.

**Effort.** 0.5-1 person-day.

**Prototype.** Not measurable in `ubench_econ`: the harness has no inter-block work. Use a short local
3-validator cell (60-120 s) before and after, comparing `torus_exec_end_resident_wait_seconds` sum/count and
exec block ms. Run the existing test `end_resident_runs_on_a_worker_joined_by_the_next_block` (`app.rs:30427`).
