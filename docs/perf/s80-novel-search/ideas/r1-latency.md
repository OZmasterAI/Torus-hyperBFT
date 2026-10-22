# r1 latency lens: novel ideas (s80)

Base: the task's `eb993e3f`. All file:line anchors below are at eb993e3f. Note: the main checkout moved to `2d2f3ed9` (batch 10 merged) while this was written. On that commit `crates/torus-consensus/src/app.rs` lines shift by about +215. `torus-rpc`, `torus-mempool`, `hotstuff_rs` and `tools/bench-throughput` are unchanged.

Nothing was built or run. The new numbers come from re-reading existing 9j2 cell data (sampler.csv, metrics-1hz.txt.gz, bench.log, summary.json under `~/bench-results-matched/ozarchy-9j2-*`). The parse scripts are in `/home/oz/projects/wt/novel-s80/scratch-latency/`.

## 0. New measurement: the latency headline comes from a ~13.5 s burst cycle

I split every load second into phases using val0's 1 Hz gauges:
- **A**: mempool ≥ 1,500.
- **B**: mempool < 1,500 and exec queue ≥ 40.
- **C**: everything else.

The phase shares are the same in all cells checked (std r1/r2 CL21, std r1 R, deep r2 CL21).

| phase | share of s | consensus | exec | mempool | commit-age mean | exec-age mean |
|---|---|---|---|---|---|---|
| A (burst digest) | 12-17% | views 173-215 ms, 167-263 actions/blk (cap 400 often hit) | busy 0.82, q 3-4 | 2.7-3.6k (peaks 6.4k) | 1.86 s | 2.17 s |
| B (exec full) | 12-25% | paced by the blocking exec send | busy 1.0-1.04, ~2,000 actions/s | ~150 | 1.21 s | **3.71 s** |
| C (calm) | 62-75% | views ~40 ms, ~52 actions/blk | busy 0.86; trough seconds 0.39-0.6 | ~350 | 0.38 s (0.20-0.25 in calm seconds) | 1.01 s (0.3-0.4 in calm seconds) |

The figures are std r2 CL21. Ages are the per-second sum/count of `torus_order_age_*_seconds`. Other cells are within about 10%.

- **The bench creates the cycle.** Its submit rate swings between 154 and 4,227 actions/s with a ~13.5 s period (`bench.log`).
- **Phase A is consensus-bound, not exec-bound.** With 263 actions per block the view is 197 ms, against 40.6 ms in phase C. On the collector, `qc_to_advance` is 47 ms (C: 6.1), of which `on_committed_block` is 43 ms (C: 4.0) and mempool remove 12 ms (C: 0.9). The leader's build is 72 ms (C: 8.2). That gives about 1,335 actions/s through consensus. At the same block size, exec does about 2,000 actions/s (phase B).
- **Block p99 (475 ms) and commit-age p99 (5 s) both sit in phase A.** Exec-age p99 (9.3 s) sits in phases A and B.
- **The tail node pays extra CPU.** val2 is the bench's in-flight tail node (`--in-flight-watch-rpc :8647`). In all 9 cells checked (std CL21 r1-r4, std R r1-r2, deep CL21 r1, deep R r1, deep M15 r1) it uses +2.0-2.2 ms user CPU per 1k fills (+8-10%) and +0.27-0.47 cores more than val0/val1. This is the 300-market version of the s121 10-market finding (mem ea054ff3: "load is tail-bound ... 50-72 heights behind").
- **Moving the queue does not help.** The bench releases its in-flight slots at commit, so moving waiting between the exec queue and the mempool barely changes exec age. Exec-queue-depth or credit variants and admission shedding mainly shift age from exec to commit, so I did not propose them.

---

## 1. Commit-time inclusion stream (`torus_subscribe("inclusions")`), and the bench tail on it

**Mechanism.**
- Today a client learns that its action committed only by polling `torus_getBlockBody`.
  - The node reads the body record from the state DB and builds `serde_json::to_value` for every action (`crates/torus-rpc/src/torus.rs:1985-2001`). A PlaceOrderBatch becomes 400 JSON objects of 8 fields each, about 50-70 KB (est.).
  - The bench tail polls every 200 ms (`tools/bench-throughput/src/in_flight.rs:46`) and reads the heights one at a time (`tail_poll` :676).
- Proposed:
  - `dispatch_to_exec` already computes every committed action's `compute_action_hash` (`app.rs:11505-11516`, right after the commit-age observation at :11499).
  - Push `(height, Vec<B256>)` into a tokio broadcast channel there (`try_send`, never blocking). This copies the fill-sink pattern (`app.rs:1564`, `:6464`).
  - Expose it as a new `sub_type` of the existing `torus_subscribe` (`torus.rs:274`).
  - The bench gets a tail mode `stream`. It computes `compute_action_hash` once per signed action at sign time and releases slots on stream events, with no body reads.
- Optional second event, `executed` (height, per-action ok bit), from the exec thread.

**Expected.**
- **Tail-node CPU:** −0.45 to 0.5 cores on the tail node, measured as 2.1 ms/1k fills × 253k fills/s. That is about 9-10% of that node and about 2.5% of the three nodes' CPU on the contended host.
- **Notification lag:** the client sees inclusion about 100 ms earlier on average (half the 200 ms poll). It also loses the multi-second tail backlog during commit floods.
  - Evidence of the backlog: after the phase-B flood (63 commits in 2 s, sampler idx 34-35 ≈ bench t 30-31), the bench's submit rate fell to 154-323/s and burst only 9 s later (t 40-43: 2.2-3.2k/s).
- **Bench limit cycle:** if the release lag drives it, part of the 12% exec idle in phase C comes back: +3-8% matched/s.
  - Prior hint: depth-4 tail read-ahead gave +4.5-6.5% at 10 markets, with tail lag down from 60 to 2 heights.
  - Commit and exec ages would move toward the calm-phase values.
- **Caveat:** with a closed loop, faster releases can also raise the in-system population, so the age effect must be measured.

**Nearest prior.**
- rpc-block-body-raw-json (proposed): cheaper JSON for the same full body.
- bench-tail-depth-4 / in-flight-tail-depth-4 (parked, inconclusive): parallel full-body reads. These multiply val2's serving cost instead of removing it.
- native-action-status (proposed): a pull query.
- sync-submit-action-result-sink (proposed): result at exec time, on the submit call.
- trade-streams / fill-sink (merged): fills only, at exec.
- release-timing instrumentation (mem dfea0acc): diagnostics only.

**Prototype (≤ 3 h).**
1. Bench-only falsifier. Build bench-throughput with `TAIL_POLL` 20 ms plus the existing `feat/bench-tail-depth` read-ahead.
2. Run ABBA 120 s std cells, 2 per arm.
3. Measure:
   - the bench submit-rate oscillation (amplitude and period);
   - the A/B/C phase shares;
   - exec busy in phase C;
   - matched/s;
   - val2 − val0 CPU;
   - order-age p50/p99.
4. If the cycle and the phase-C starvation are unchanged, keep only the CPU and client-lag claims.

---

## 2. Feed-ahead: prepare the commit feed at insert time on a `torus-commit-prep` thread

**Mechanism.**
- Today the commit feed runs on hotstuff-algo, on the view's critical path.
  - On the collector it runs between the QC and the AdvanceView (`hotstuff_rs/src/hotstuff/implementation.rs:1641` → :1727 → :1750).
  - On a follower it runs inside the parent insert that gates its next vote (`try_insert_body` :2858).
  - The work is `on_committed_block` (`app.rs:10601`): datum decode; `cached_matches_compact` (:1986), which hashes every body when the cache is cold (:1999); `dispatch_to_exec` (:11421), which hashes every body again (:11512) and runs `remove_committed_native` (`torus-mempool/src/lib.rs:2207`: restash + write lock + removal) and the DA-GC hand-off; and finally the blocking send (:11621).
- Proposed: when B_v is inserted (2 views before it commits), hand the validated block (Arc) to a commit-prep thread. It computes:
  - the hashes;
  - a seeded `PendingProposal::with_hashes`;
  - the restash keys and trust-cache keys;
  - the sender list and the GC set;
  - the persist record.
- At commit the algo thread does only the manifest put, the persist ticket, the blocking exec send (so backpressure is unchanged) and an O(1) prune hand-off.
  - The committed hashes stay in `InFlightHashLedger` (:6942) until the prep thread confirms the prune.
  - Orphaned preparations are dropped.

**Expected.**
- On the collector, phase-A `on_committed_block` is 38-43 ms, about 5 ms of which is unavoidable. Saving about 35 ms there plus about 10-15 ms on follower inserts takes the view from 182-197 ms to about 150-160 ms (−18 to 20%).
- Phase-A consensus capacity goes from 1,335 to about 1,650 actions/s (still under exec's ~2,000).
- Phase-A commit-age mean goes from 1.86 to about 1.5 s; commit p99 from 5.0 to about 4.0-4.2 s; block p99 from 475 to about 390 ms.
- matched/s: about +1% (phase A is 14% of the time and exec busy there rises from 0.82 to about 0.9).
- Calm phase: `qc_to_advance` goes from 6.1 to about 2 ms, so the view goes from 40.6 to about 37 ms (−9%) and commit improves by about 10 ms.

**Nearest prior.**
- consensus-reuse-action-hashes (rejected): the hash-reuse half only, judged on CPU (about 1%), not on phase-A critical-path wall time.
- on-committed-heavy-to-exec-thread (proposed): it would load the exec thread, which is the ceiling.
- generalize-feed-deferral-all-nodes (proposed): only reorders work on algo.
- defer-commit-feed, defer-parent-feed and broadcast-advanceview-before-feed: measured at 1.2 blk/s, with no gain or rejected.
- rh4-da-gc-worker and T5.3 (merged): moved only GC and persist off the thread.
- mempool-remove-committed-hash-index (proposed): makes the prune cheaper but keeps it on the path.

**Prototype (≤ 3 h).**
1. Write an `#[ignore]` timing test in torus-consensus, next to the app tests that drive `on_committed_block` (e.g. the test around app.rs:20675).
2. Use follower-side CompactBlocks of 52, 263 and 400 PlaceOrderBatch actions. Compare the cold path with a pre-seeded `with_hashes` cache and a deferred prune. Run 20 reps each and report ms per block and ms per action.
3. Verdict:
   - Go if the per-action on-path cost falls by at least 60% at 263 actions.
   - If the isolated cold path is already under about 12 ms at 263 actions, the 43 ms in phase A is mainly host contention, and the idea is refuted.

---

## Considered and dropped (in the ledger, or zero-sum here)

- L1.1 unanimous-QC 1-chain commit, L1.2 optimistic exec, and proposer-commit-on-AdvanceView: in the ledger.
- next-leader-payload-prebuilder / speculative block production: in the ledger. It is the build-side partner of idea 2 (72 ms of the phase-A view).
- RPC reads from the executed view: rpc-freshness-hl-way, parked (Phase 5). RPC state is durable-gated (`eth_head` = min(applied marker, committed), `torus-rpc/src/eth.rs:182`), so RPC staleness is the durable age: 4.0-6.5 s mean by phase.
- Exec queue bounded by work (action credits): new as a unit, but under the closed-loop finding above it mostly shifts age from exec to commit. Nearest: exec-queue-depth-16 (parked).
- Rotating sender cursor and arrival order in selection: they change who is last, not the burst p99. Arrival order was rejected (s66).
- Exec-time result push: sync-submit-action-result-sink (proposed).
