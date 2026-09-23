# Torus-hyperBFT improvement synthesis (20 explorers, code at wt/s63-body-fetch @ 9ddece6)

## Spot-checks I ran myself (read-only)

| # | Claim | Result |
|---|---|---|
| V1 | The leader runs the commit feed (`on_committed_block`) before it broadcasts its header. | **Verified.** `implementation.rs:636-651` runs `block_tree.update` → `process_update_result` → `broadcast_proposal_as_header`. `process_update_result` (`:294-305`) calls `feed_committed_blocks_to_app` inline. On the full-proposal follower path the feed also runs before `PhaseVote::new` (`:1157-1190`). The header fast path votes before the body arrives, so on followers the feed delays the block insert, not the vote. |
| V2 | The rejoining node validated the proposal but could not vote because its local view was behind. | **Verified.** The `on_receive_msg` log prints `self.view_info.view`, the local view (`:860-864`). A vote requires `header_is_current` (`:1831`, `:2010`). In `crash-restart-tail.log` val1 validated h558 at 21:48:01.208 while its local view was **610**. It proposed a stale block (parent 557) at 21:48:07.85 at view 612 and got nothing until NewView 614 at 21:48:32.99. The explorers disagree whether that header was for view 613 or 614; both agree the local view was 610. |
| V3 | No WAL bound exists, and `timeout_max_ms` is never read. | **Verified.** A grep of `crates/` finds no `max_total_wal_size`, `avoid_flush_during_recovery` or `MAX_TOTAL_WAL`. `timeout_max_ms` appears only in `torus-genesis/src/lib.rs:88,596`. The restart log shows 'loaded node key' at 21:47:30.33 and 'state database opened' at 21:47:53.93 (23.6 s), then 'chain configuration loaded' at 54.12. |
| V4 | Mempool selection is ordered by sender address. | **Verified.** `native_pool.rs:23` has `type SortKey = (u8, Address, u64, u64)`. |
| V5 | The leader re-mirrors every selected body, and the commit path recomputes action hashes. | **Verified.** `app.rs:4087-4092` clones every action and calls `mirror_native_to_da` unconditionally. `app.rs:5446` recomputes `compute_action_hash_with_scratch` in dispatch. |
| V6 | Load-window numbers for val0 in s64-confirm-main-r1. | **Verified.** on_committed_block 124.62 ms, commit_persist 53.69, mempool_remove_committed 30.24, propose_finalize 128.72, view 669.83, block_build 187.6, views/committed 1.507. Nonce-expired evictions are 44,823 / 44,870 / 44,860 out of 120,158 submitted. |
| V7 | The header path sends the vote before it persists vote state. | **Verified.** `implementation.rs:~2030-2039` calls `sender_handle.send(... phase_vote)` and only then `set_vote_state_atomic`. |

**Corrections to the task premise** (from explorers; V2 and V3 confirmed by me):
- "~25 s before chain configuration loaded" is almost all RocksDB open. Parsing the genesis file takes 0.19 s.
- At +31.9 s the rejoining node was at local view 610, not the right view.
- "Send Queue full" appears only in crash cells, aimed at the dead peer. It is 0 in s64-confirm-main-r1. (data-availability/fable, networking/opus)
- Commit-interval p50 in s64-confirm is 487.6 ms, not 58 ms. (consensus-protocol/opus)
- RATE=76000 is an **actions/s budget that never binds**. The generator acked 394 actions/s; the load is set by CONC=256 and RPC latency. (harness/opus, block-sizing/opus)
- "Execution is no longer the limit" is true only as a 300 s average. Per minute, exec busy goes 0.61 → 0.72 → 0.76 → 0.93 → 0.91 as resting orders grow 0.94M → 3.13M. (harness/opus)

**Measurement caveat for every throughput estimate below.**
- Within-arm CV is 1.7-7.2%, so the minimum detectable effect is about 9-10% at n=3-4.
- The last ~120 s of each 300 s cell is exec-bound. A pure consensus gain therefore shows up at roughly 0.6x its early-window size in `matched_s_avg`.
- Early-window exec headroom caps consensus gains at about +25%.
- I applied this dilution in the "estimated gain" column where noted.

---

## 1. Top 10

| Rank | Title | Area | Metric | Est. gain | Conf. | Effort | Proposed by |
|---|---|---|---|---|---|---|---|
| 1 | Run the app commit feed after the leader's header broadcast and its own vote | hotstuff_rs enter_view / on_receive_phase_vote, app.rs | matched/s, blk/s | view ~660 → ~480-570 ms; **+6-15% matched/s** after dilution (+10-25% early window) | medium | M | consensus-protocol/opus, validate-vote-path/opus, exec-parallelism/fable (off-thread variant), threads-locks/opus (step d), encoding-hashing/fable (body write). **Both fable and opus** |
| 2 | Rejoin view-sync: adopt an authenticated future-view header, or f+1 view evidence, or re-send the highest TC/TimeoutVote on reconnect | hotstuff_rs pacemaker + header path | recovery_s | freeze 64 → ~32 s alone | high (mechanism) / medium (fix) | M | pacemaker-rejoin/fable, consensus-protocol/opus, validate-vote-path/opus, startup-recovery/fable; also noted by networking/opus and validator-topology/fable. **Both** |
| 3 | Bound the RocksDB WAL (`max_total_wal_size` 256-1024 MiB; port b0a57ce), optionally `avoid_flush_during_recovery` | torus-state db.rs | recovery_s | DB open 23.6 → ~3-8 s; with #2, freeze 64 → ~10-15 s | medium-high | S | pacemaker-rejoin/fable, storage-rocksdb/fable, state-root/opus, startup-recovery/fable, memory-allocator/opus, threads-locks/opus (note). **Both** |
| 4 | Remove recomputation on the commit path: store the trust-cache key in the pool entry (so restash is a lookup), reuse the CompactBlock/pending hashes in dispatch, seed the follower OnceLock | torus-mempool, app.rs dispatch_to_exec | blk/s | remove_committed 30 → <1 ms, plus ~15-30 ms of keccak; +2-6% | high (the 30 ms part) | S | mempool-ingress/fable, encoding-hashing/fable, threads-locks/opus, exec-parallelism/fable (step 0), crypto/fable (note). **Both** |
| 5 | Produce path: skip the re-mirror of bodies that are already durable (presence check), carry pool hashes into PendingProposal, drop the second deep clone | app.rs mirror_or_drop_native / produce_block | blk/s | block_build −25-60 ms of ~190; +3-6% | medium | S-M | block-building/fable, mempool-ingress/fable, storage-rocksdb/fable, encoding-hashing/fable, memory-allocator/opus, threads-locks/opus. **Both** |
| 6 | Right-size offered load: knee sweep of RATE/CONC (no code), then a capacity-keyed pre-verify shed | harness; torus-rpc admission | matched/s, cpu | +5-15% (drain-window evidence); 37% silent expiry → ~0 | medium | S | block-sizing/opus (x2), harness-validity/opus, threads-locks/opus, end-to-end/opus, mempool-ingress/fable. **Both** |
| 7 | Run the batched cancel-all book pass on all 10 markets in parallel | torus-bridge exec_cancel_all_run | exec ceiling (matters in the exec-bound late window) | phase1 111 → ~18-37 ms; +2-5% on the 300 s average via the late window | high | S | matching-engine/opus, exec-parallelism/fable. **Both** |
| 8 | Re-A/B leader body push (`TORUS_BODY_PUSH_MAX_BYTES=65536`), env only | hotstuff_rs broadcast_proposal_as_header | blk/s | parent-body wait −50-70 ms; +2-8% | medium | S | validate-vote-path/opus, data-availability/fable. **Both** |
| 9 | Attribute what the HotStuff thread blocks on: wchan/stack sampler plus RocksDB PerfContext and wait timers on hotstuff-algo | harness + instrumentation | diagnostic | enables the rest; no direct gain | medium | S | storage-rocksdb/fable, threads-locks/opus. **Both** |
| 10 | Follower body reconstruction: pool-first lookup, or raw-bytes attest, instead of MultiGet + decode | app.rs reconstruct_native_actions_hot_core | blk/s | da_reconstruct 39-46 → ~5-15 ms on the next leader's path; +2-4% | medium | S | data-availability/fable, mempool-ingress/fable, validate-vote-path/opus (raw bytes), encoding-hashing/fable. **Both** |

---

## 2. Detail per item

**1. Defer the commit feed on the leader's propose path.**
- **Why.** Votes for view v go to leader(v+1), so the next proposer is always the first node to commit. It runs `on_committed_block` synchronously between inserting its own block and broadcasting its header (V1). The feed does the manifest write, a ~4 MB durable body write (commit_persist 45-54 ms), 200 hash recomputes, the mempool prune (30 ms) and exec dispatch.
- **Evidence.** propose_finalize (insert → Propose) is 96-129 ms, about equal to on_committed_block at 114-125 ms (s64), and the same holds in all 12 s63 combo node-cells. Opus's log parse of QC → own header: views with 0 commits have a median of 238-333 ms, views with 3 commits 655-844 ms. The mean is 1.3-1.6 commit dispatches per leader view.
- **Arithmetic (estimate).**
  - ~1.5 × ~120 ms ≈ 180 ms inside a ~660 ms view. The upper bound is −27% view time.
  - Discount by half for work that moves onto the node's follower duty: −90..−180 ms, about +15-35% blk/s.
  - The early-window exec cap (~+25%) and the 0.6x dilution give **+6-15% on the 300 s average**.
- **Keep:** `block_tree.update` before the broadcast (lock and highest_pc must be durable first). The leader must cast its own PhaseVote inline right after the broadcast, or the 3/3 QC waits on the deferred feed.
- **Validate.** Env flag (e.g. `TORUS_DEFER_COMMIT_FEED`), 3+3 interleaved cells. Expect propose_finalize < 20 ms and the 1/2/3-commit buckets to collapse onto the 0-commit bucket. Then run the crash gate.
- **Risk.** In_flight/prune coupling (the s355 duplicate-inclusion class) and the crash window between the commit write and the feed (the S444 reconcile covers it). The later "move commit work fully off-thread" variant (exec-parallelism, threads-locks step d, encoding-hashing #3) is the follow-up once this lands.

**2. Rejoin view-sync.**
- **Why.** With n=3 the quorum is 3/3, so no QC or TC can form while one node is down. The survivors advance on local backed-off timers only. The restarted node starts at highest_view_with_progress+1 and has no certificate to catch up from.
- **Evidence.** V2. val1 received and validated the survivors' current-view header at +31.9 s but was at local view 610, so `header_is_current` was false and it did not vote. Everyone then waited out the survivors' 32 s view (next commit 21:48:33.5).
- **Fix options** (explorers converge):
  - (a) When an authenticated header for view w > local view arrives from leader(w), with a safe justify and highest_view_voted < w, enter w through the pacemaker (`update_view`) and vote. Require f+1 power of evidence; at n=3, f+1 is one validator. This is the DiemBFT/Tendermint round-skip rule.
  - (b) Cheaper complement: on validator reconnect, re-send your latest TimeoutVote or highest TC. The existing `fallback_tc` path adopts it.
- **Arithmetic.** Kill → resume is 63.8-64.2 s. The node could have voted at 21:48:01.1 instead of 21:48:33.0, which removes ~31.9 s (~50%).
- **Validate.** Unit test: a replica at view v−4 receives leader(v)'s header, votes exactly once and never votes again at ≤ v. Then the crash cell (`--crash-at 60`).
- **Risk.** Liveness-sensitive. A Byzantine leader could pull nodes forward one view per valid header. Safety still rests on highest_view_voted plus the lock. Keep the S395 stale-schedule rebase. Also see the CONS-FIND-26 guard in §4.

**3. Bound the WAL.**
- **Why.** No bound is set (V3). With 44 CFs × 128 MiB × 4 buffers, RocksDB's implicit cap is ~86-88 GiB, so any cold CF pins every WAL segment since open.
- **Evidence.**
  - Four crash cells show 19.8-23.6 s in `StateDb::open` against 0.14 s on a cold start.
  - At the kill, val1 had written 2.92 GB of WAL and flushed only 346 MB (sampler).
  - Host log: the restarting process ran single-core CPU-bound with bi=0, i.e. WAL replay from page cache at ~124 MB/s.
  - The retained devnet dir has 40 WAL files (4.95 GB), all kept since `000004.log`.
- **Arithmetic.** 256 MiB cap → ~2-4 s; 1 GiB → ~8-11 s. Replay time today grows with uptime (a kill at +300 s would be ~12 GB, ~95 s; linear estimate).
- **Validate.** Crash cell, cap vs unset: measure 'loaded node key' → 'state database opened', freeze length, and matched/s neutrality over n≥3 load cells.
- **Risk.** More small flushes of cold CFs, so more L0 files. The s60 1024 MiB cell was not slower (n=1). Low risk. **Caveat:** the causal chain is inferred from timestamps and counters; the RocksDB LOG from the crash cells was deleted. Keep it next time.

**4. Commit-side recomputation.**
- **Why.** `remove_committed_native` → `verified_restash_keys` recomputes the EIP-712 cache key (canonical encoding of 400 orders + keccak) for all ~193 actions while holding the pool read lock. The ingress path already computed that key and discarded it.
- **Evidence.**
  - `torus_mempool_remove_committed_seconds` averages 29.9 ms per commit, the same on all 3 nodes (V6: 30.24 ms load).
  - `app.rs:5446` recomputes action hashes although the CompactBlock and PendingProposal already carry them (V5).
  - Followers' `cached_matches_compact` OnceLock is cold, so every commit streams another keccak pass.
- **Arithmetic.** ~30 ms (measured) + ~15 ms (one ~4 MB keccak pass at 248 MB/s) ≈ 45 ms off the HotStuff thread per commit on every node. At the s55 3.2x thread-cost-to-interval ratio that is up to ~−110 ms, but take −3..−6% interval as the estimate.
- **Validate.** Golden test that restash keys and hashes are identical. Then A/B on on_committed_block_ms and remove_committed p50 < 1 ms.
- **Risk.** Low: the stored key is a pure function of the entry bytes.

**5. Produce-side waste.**
- **Why.** Every pooled action is already durable in CF_NATIVE_PENDING, because ingress mirrors it and `flush_da_mirrors` runs before selection. `mirror_or_drop_native` still clones and `put_batch`es all ~4 MB again, re-hashing and re-serializing each body (V5). PendingProposal then recomputes the hashes the pool entry already stores.
- **Arithmetic (estimates vary between explorers).** 15-25 ms (storage/fable) to 70-100 ms (mempool/fable) of a ~190-230 ms build. Take ~25-50 ms → loaded view −4-7% → +3-6%.
- **Validate.** Env flag. Watch block_build and view_propose_delay, keep `proposer_body_mirror_failures` at 0 and body-fetch exhaustions at 0 (proves bodies are still durable before they are referenced).
- **Risk.** Must keep the S459 durable-before-reference rule. Using a presence check (`has_all_native_da`, which already exists) is the safest variant.
- **History.** This is s55 backlog item 2 ("skip DA re-mirror, cache hashes"). It was never implemented.

**6. Offered load and admission.**
- **Why.** The chain is overloaded, not short of work.
  - 394 actions/s acked vs ~222/s included; 37% expire after paying for verify, gossip and a DA write on 3 nodes.
  - The existing pre-verify shed triggers at 65,536, but nonce expiry keeps the pool near 15k, so it can never fire.
- **Key evidence (block-sizing/opus).**
  - In the drain window, full 200-action blocks commit every **485-786 ms (mean 607)**, against 1,074-1,184 ms in the last 60 s of load (5 consensus-bound cells).
  - Host run queue is 41-48 on 18 CPUs under load vs ~23 in the drain.
  - HotStuff runqueue wait is 90-105 ms per committed block.
- **Arithmetic (estimate).** Removing ~0.35-0.4 of the CPU that the drain frees gives 950 − 0.4 × 343 ≈ 813 ms/block, about +17%. Take +5-15%. The confound is that book depth also grows.
- **Validate.** A no-code, same-binary sweep at RATE 76000 / 320 / 270 / 230, 3 cells each. If it wins, add a node-side shed at pool > K × block cap (non-cancels only).
- **Risk.** Too low a rate under-fills blocks. Report this as a separate operating point, not a like-for-like record comparison.

**7. Parallel cancel-all per market.**
- **Why.** `exec_cancel_all_run` loops over markets serially (`native_executor.rs:5104-5121`). Each `cancel_all_many` touches only its own book, and `match_parallel` already provides the scoped, LPT-chunked, panic-contained pattern.
- **Evidence.** phase1 is 111 ms per native block overall, 23 ms early and **137 ms late**. Markets are balanced to within ~1%.
- **Arithmetic.** With a 3-6x speedup on a busy host, phase1 drops to 18-37 ms, about −74-93 ms of exec per block. The late window is exec-bound (busy 0.91-0.93), so ~10% faster exec there gives ~+2-5% on the 300 s average. It also raises the ceiling that item 1 will hit.
- **Validate.** Differential test (serial == parallel for rows, levels and root), then one A/B.
- **Risk.** Low. Keep the margin and balance loop serial.

**8. Body push.**
- **Why.** The next leader cannot propose until the parent body is fetched, validated and inserted. Header → body averages 45-52 ms (p90 120-134 ms, p99 462-608 ms). The requests are served on the leader's HotStuff thread.
- **Arithmetic.** −57-77 ms of a ~750 ms chain step, about −8% at best. Discount to +2-8%, because the July A/B saw qc_collect absorb the saving.
- **Validate.** Env only, 3+3 cells, with `TORUS_BODY_FETCH_TRACE=1`.
- **Risk.** Minimal. Already safety-A/B'd in July.

**9. Blocked-time attribution.**
- **Why.** schedstat puts hotstuff-algo at 162 ms on-CPU + 90 ms runqueue per 862 ms block, so ~70% of its time is off-CPU and not runnable, cause unknown. offcputime cost a 14x slowdown.
- **What each explorer found.**
  - Storage counters argue against RocksDB stalls: stall_micros 0; write_other 956 vs write_self 248k; jbd2 shows no waits.
  - opus notes one shared RocksDB/WAL for consensus KV, DA, commit records, exec flush and trade history (`main.rs:569`).
- **Change.** Sample `/proc/<tid>/wchan` at 1 Hz, and enable PerfContext (`EnableTimeExceptForMutex`) on the hotstuff-algo thread only.
- **Arithmetic.** None. This decides whether storage or write-queue work (WAL-off for trade history, a separate consensus KV instance) is worth doing at all.
- **Risk.** Negligible.

**10. Follower reconstruction.**
- **Why.** validate_block da_reconstruct takes 39-46 ms: a flush, a ~4 MB MultiGet and 200 bincode decodes on the HotStuff thread. It gates the next leader's produce. The same decoded actions are already in the follower's pool (gossip delivered all 120,158).
- **Change.** Look up pool hits by `hash_index` first and read the DA store only for misses. Alternative (validate-vote/opus): attest over raw stored bytes and skip decode plus re-encode.
- **Arithmetic.** ~30-40 ms off a ~750 ms chain step, +2-4%.
- **Risk.** The early livelock (mem 28e1a821) came from treating a pool miss as fatal. Here a miss falls through to the durable store, so the S459 rule holds.
- **Overlap.** TORUS_ASYNC_VALIDATE targets the same work (see §3).

---

## 3. Already tried / known

- **Raising `timeout_base_ms`** (s55, cap 100, 500 → 1500): views per block went 1.50 → 1.07, but matched/s stayed within ±40% noise. The verdict was "do not tune timeouts for throughput". It has never been re-run at cap 200 with the pipeline; one explorer (pacemaker-rejoin) ranks a load-keyed re-test last, at low confidence.
- **Body push:** July A/B at 2a0d207 showed +7% at idle and neutral at cap 400 under exec saturation. Default OFF; never run on this line.
- **WAL budget:** b0a57ce `TORUS_ROCKSDB_MAX_TOTAL_WAL_MB` (s60, for disk usage, 35.4k vs 29.1k, n=1, not merged) and RH1 e19cb8f (July, 256 MiB, not merged). **Neither was measured for restart time.**
- **HOTSTUFF_CPUS pinning** (s46): negative; runqueue wait rose from 19-22 to 32-41 ms/blk. Priority changes (nice, SCHED_BATCH) have never been tried.
- **TORUS_ASYNC_VALIDATE:** implemented (3e4f0c2 / 4076355), never benchmarked ON. validate-vote-path/opus predicts a hit rate under 30% because `take_verdict` is non-blocking and the consensus thread picks the body up at p50 1.8 ms. It also has no hit/miss telemetry and double-counts `validate_block_*` histograms.
- **Speculative pipelining (Option C):** planned in docs/plans/speculative-pipelining-impl.md, never implemented. The S244 speculative-produce App-trait extension no longer exists in the tree.
- **Hash-only pre-proposal push:** S387 failed because every validator pulled every body. Fix C now pulls only missing bodies, and the floor was raised to 8 MB. Hash-only with gossip ON has not been A/B'd at cap 200.
- **Block cap sweeps** (r2-r4, exec-bound era): cap 300 was about +5% but noisy; cap 1000 wedged (s365). Not re-run since consensus became the limit.
- **Cancel-all batching** (58c9eb4): landed; phase1 went ~279 → ~108 ms. Deep-compaction threshold variants were rejected (−6%).
- **Single-batch propose writes** (fd1fbba `insert_and_update`): measured −20-25 ms on perf/matched-200k, **never ported** to this line.
- **Dedicated /torus/block-data protocol:** bypassed in May 2026 ("silently fails"). Body requests are still served on the HotStuff thread.
- **Consensus KV split instance** (ebcfc46, August, exec-bound): 29.5k/29.1k vs 31.2k control, n=1, not merged. Worth a re-test only if item 9 implicates the write queue.
- **B3 leader reputation:** stalled the devnet (s339) because locally observed reputation diverged; now behind a kill switch. Deriving it from the committed chain is new (validator-topology/fable).
- **4-validator devnet** (s442): real f=1, but throughput dropped more than 4x with 1 of 4 dead.
- **Action-hash scratch reuse** (bb32ac5, 8b5d37f, 1364d74) and **lazy leader OnceLock** (984e6ef): allocation-only changes. The recomputations themselves remain.
- **TORUS_ROCKSDB_PIPELINED_WRITE:** adopted in the bench env (−9% view time, s46). Node default is still off.
- **Off-CPU tracing:** unusable (6843d79).

---

## 4. Possible bugs found

1. **Rejoin non-vote** (verified, V2). This is not a vote-safety rejection. A restarted replica has no way to learn the survivors' view without a certificate, and with n=3 no certificate can form while a node is down. The s64 memory ("reached view 614 at +31.9 s") is wrong: the node was at local view 610. Fix: item 2.
2. **Vote sent before it is persisted** (verified, V7). On the header fast path, `send(phase_vote)` comes before `set_vote_state_atomic`. A crash between the two could allow a second vote in the same view after restart. Small window, but it is a safety-ordering defect. Reported by validate-vote-path/opus and threads-locks/opus.
3. **CONS-FIND-26 guard** (`pacemaker/implementation.rs:345-349`, not verified by me). It returns early on TimeoutVotes whose sender tip is ahead of us. That makes the `fallback_tc` catch-up path (`:419-445`) unreachable for exactly the lagging replica it was written for. Harmless at n=3, where no TC exists; it will matter at n≥4. (pacemaker-rejoin/fable)
4. **Dead config: `timeout_max_ms`** (verified). It is in genesis (30000) and read by nothing. The effective ceiling is 0.5 s × 2^8 = 128 s.
5. **Sender-address selection starvation** (SortKey verified).
   - Under a 1.8x overload, low-address senders are always served first and a fixed high-address band expires.
   - Spearman(address, marginUsed) is −0.54 to −0.78 in three cells. end-to-end/opus reports r < 0 in 225 of 230 retained cells.
   - Consequences: a fairness/grinding defect, and the bench measures a distorted workload.
   - Fix: order by `(priority, seq, sender, nonce)` or rotate the start cursor. Proposer-local, so it cannot fork.
   - Proposed independently by mempool/fable, harness/opus, end-to-end/opus and block-sizing/opus.
6. **WebSocket notifications are lossy under CTE** (end-to-end/opus). The `on_commit_block` handler ignores `event.block` and scans trades before exec and the background writer have produced them. In the load window 165 of 401 heights were never notified, 112 were notified twice, and 138 before exec finished. `eth_blockNumber` reports the committed height, not the executed one.
7. **Unbounded, unused `block_store`** (data-availability/fable). It is written on every proposal and never read or evicted, since the dedicated protocol is bypassed. About 1.4 GB/day of growth.
8. **`NativeDaStore::remove` has no production caller.** Every ingested body, including the 37% that expire, stays in CF_NATIVE_PENDING forever. (mempool/fable)
9. **Weighted IWRR with raw powers** (validator-topology/fable). Power = stake/1e18, so unequal stakes give multi-day single-leader runs (e.g. 2M/1M/1M means 1M consecutive views for val0). Normalize powers before using stake-weighted leadership.
10. **Harness gates.**
    - The liveness threshold is 30 s, and s63-crash-on-r1 froze exactly 30 s and was reported PASS / "Stalls: none". `crash_gate` has no timing criterion.
    - summarize.py produces a negative commit_interval average for the restarted node.
    - `torus_rocksdb_flush_count` reads 0 while flush bytes grow.
    - `torus_rpc_submit_verify_cpu_seconds` measures wall time, not CPU (about 3x inflated).
    - `gossip_messages_sent` also counts direct sends.
11. **Shard custody apparent no-op.** Explained: devnet `env.sh:56` sets `TORUS_SHARD_CUSTODY=0`, so every bench build number excludes an estimated ~20-40 ms of production leader cost. (block-building/fable)
12. **The rejoiner proposed a stale block** (view 612, parent 557) that the survivors validated. Wasted work, harmless.

---

## 5. Everything else (deduplicated, one line each)

- **Next-leader payload pre-builder** (block-building/fable, consensus-protocol/opus, both models): build selection, mirror and attestation off-thread; est. +5-15% but L effort. Its overlap window shrinks after item 1. This is the 11th-ranked item.
- **Clamp view deadlines to `timeout_max_ms`** (~8 s) (pacemaker-rejoin/fable, startup-recovery/fable): cheap insurance; freeze 64 → ~40 s alone; mostly subsumed by item 2.
- **Renice/SCHED_BATCH the verify, rpc and rocksdb:low pools** so consensus threads win CPU (threads-locks/opus); +3-8% est.; a harness-only first step. cpuset 5/5/5 plus capped fan-out knobs is the fable variant (exec-parallelism).
- **Parallel boot book rebuild** (startup-recovery/fable, memory-allocator/opus, exec-parallelism/fable): load_books takes 3.96 s of the 5.1 s single-block replay → ~1 s; the cost grows with resting depth.
- **Port fd1fbba `insert_and_update`** and time the ~90-100 ms gap between propose_build and block_build (block-building/fable).
- **Attest over action hashes** instead of re-serializing ~4-5 MB (crypto/fable, encoding-hashing/fable, block-building/fable): −10 ms per validate and per produce; consensus-format change.
- **Move the commit-time ~4 MB body-record write to the exec-side writer**, keeping the manifest for recovery (encoding-hashing/fable; exec-parallelism/fable step 2): −45-54 ms per commit.
- **Serve body fetches from the network thread's block_store** (dedicated protocol) and evict it (data-availability/fable).
- **Hash-only pre-proposal push** (`TORUS_HASH_ONLY_PUSH_THRESHOLD`) while gossip already delivers every body (data-availability/fable, networking/opus): CPU relief only, +0-3%.
- **Raise the UDP receive buffer** (sysctl, later SO_RCVBUF): header delivery ~14 → ~5 ms; +1-3%; drops were earlier ruled out as the cause of gossip collapse (networking/opus).
- **Shorten QUIC idle/keep-alive** (10 s → 2-3 s) so a fast-restarting peer is not black-holed on a stale connection (networking/opus); worth 0 s today.
- **Weighted quorum 2/1/1 on the devnet** (val0 heavy) for crash tolerance at zero CPU cost (validator-topology/fable). Devnet-only, not Byzantine-safe.
- **Broadcast PhaseVotes to leader(v+1) and leader(v+2)** so a stalled next leader does not orphan the view (validator-topology/fable).
- **Reputation derived from the committed chain** to re-enable B3 (validator-topology/fable, L).
- **4th equal validator** (true f=1): costs ~4 cores on this host (validator-topology/fable).
- **Settle pass B parallelization** (exec-parallelism/fable) and **16-byte level-queue entries / slab** (matching-engine/opus): exec-ceiling levers for the late window.
- **Faster hasher and journal containers** in the order book (matching-engine/opus, low confidence; profile first).
- **Compact the per-order book footprint** (merge maps; memory-allocator/opus).
- **Global allocator** (mimalloc/jemalloc), after a `MALLOC_ARENA_MAX=4` probe (memory-allocator/opus). RSS grows 124 MB → 7.7 GB per node over 328 s.
- **Workspace has no `[profile.release]`** (LTO off, codegen-units 16): a cheap CPU lever nobody has tried (memory-allocator/opus note).
- **Trade-history writer:** `disable_wal` and drop low_pri (RocksDB's 1 MiB/s low-pri limiter pins it; queue mean 27, max 83) (storage-rocksdb/fable).
- **Keep trie internal nodes in RAM** and persist only leaves plus the root (state-root/opus, low confidence).
- **Height-tagged lagged state root in the header**, checked without blocking (state-root/opus): robustness, 0 throughput. Today the root is computed at 110 ms/block and never used.
- **Exec verify:** skip the rayon phase when the trust cache hits 100% (crypto/fable); exec verify is 44 ms/block despite 0 misses. Test first with `TORUS_PARALLEL_VERIFY=1`.
- **Hoist EIP-712 typehashes and the domain separator** out of the per-order loop (crypto/fable): tiny CPU gain.
- **Varint/packed local body encoding** (encoding-hashing/fable, L, low confidence).
- **Sign after taking the semaphore permit in the generator** (harness/opus): actions currently arrive ~12 s into their 60 s nonce window.
- **Per-minute / resting-depth reporting in summarize.py plus a bounded-book workload** (harness/opus): high value for deciding A/Bs.
- **Max-commit-gap and freeze-decomposition gates, plus a 4-validator crash cell** (harness/opus).
- **Action-age histograms** (now − nonce at commit/exec) and the expired-action fraction in the headline (end-to-end/opus).
- **Load-keyed first-view deadline** (pacemaker-rejoin/fable, low confidence; the s55 null result applies).
- **Fix the async-validate race** (in-flight wait, counters) before any flag-ON A/B (validate-vote-path/opus).
- **Move proposer shard custody onto the pre-proposal worker thread** (block-building/fable): production-only gain.

---

## 6. Recommended next 3 experiments

1. **Zero-code same-day batch.** Interleaved, n=3 per arm, 9ddece6, cap 200.
   - Arms: (a) control; (b) RATE=270 (knee probe, item 6); (c) `TORUS_BODY_PUSH_MAX_BYTES=65536` (item 8).
   - Add the wchan sampler on hotstuff-algo to the host sampler (item 9), and score per minute as well as whole-window.
   - Cost: harness edits only. This settles two env levers and names the blocked-time wait channel.
2. **Deferred commit feed plus commit-side dedup** (items 1 and 4) behind one env flag.
   - Build: store the restash key in the pool entry, carry hashes into dispatch, move `feed_committed_blocks_to_app` after the header broadcast with an inline self-vote.
   - Primary signals: propose_finalize < 20 ms, on_committed_block, view_duration and the early-window blk/s. Then check matched/s and run the `--crash-at 60` gate.
   - This is the largest consensus lever, and the arithmetic for it is directly checked in V1 and V6.
3. **Crash-recovery cell with WAL cap plus view-sync** (items 2 and 3).
   - Port b0a57ce with a default of 512 MiB. Add the "adopt a leader-signed future header, f+1 evidence" rule with its unit test.
   - Keep the RocksDB LOG from the crash cell.
   - Expected: DB open 23.6 → < 8 s, and a vote within 1 s of receiving the first current header. The freeze should drop from 64 s to about 10-15 s.
   - Fix the vote-before-persist ordering (bug 2) in the same change, since it touches the same code path.