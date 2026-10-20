# 8k explore, Phase 1: what is already explored (survey, s68, archy, 2026-10-19)

Phase 1 of the 8k prompt (`docs/perf/8k-explore-workflow.md`): a list, not ideas. It is the "already explored"
filter for Phase 2. Base: branch `docs/perf-section-97-8k` at `bf824339` (= origin/main `4811e2b0` + results
section 96). Results doc = `docs/perf/ozarchy-antispam-item6-pf1-2026-10-04.md` (cited as "s<N>" = section N).
Sources read: `docs/Torus-hyperBFT-roadmap.md`, `docs/perf/bench-queue.md`, the results doc (summary table for
sections 1-23, the head of every section 24-96, sections 81 / 90 / 92 / 93 / 94 / 96 in full), the head and status
line of every file in `docs/plans/` and `docs/perf/` (incl. the s64 and s65 explorer syntheses), the design docs on
main (`design-level-rows-authority.md`, `mission-handoff-layer3.md`, `design-parallel-engine.md`,
`design-levelhash-cache.md`, `design-flush-pipeline.md`, `hyperliquid-comparison-2026-09-29.md`,
`configuration-reference.md`), `testnet/p3-testnet/RESULTS.md`, and the memory store (project torus-hyperbft /
torus-economy; memory ids given where a fact lives only there; memory is a hint, the docs on main are the authority).

Verdict words: **shipped** (on main, on by default), **opt-in** (on main, off by default), **no gain** (measured,
not adopted), **loss** (measured, worse), **parked** (built or designed, waiting on a decision or trigger),
**dropped**, **open** (proposed, not built or not measured), **not chosen** (decided against).

## 0. Baseline costs today (profile-grounded)

Standard load (hsdb-300m, 300 markets, 3 validators on one host), main with code defaults (PC 8, F on, re-send
250 ms, 512 KB push threshold, custody 0, 8g exec-hash-once merged: `70993fa9` is an ancestor of origin/main).
Cells: s94 arm B (= today's main behaviour) and s95; profile shares from s90 / s92 (main `fbe36391`, before 8g);
deep book from s93 / s96.

| thread / area | cost | size today | section |
|---|---|---|---|
| **Headline** | matched/s; native blk/s; block p50 / p99; CPU-s per 1M fills | 257-265k; 11-16 (bistable small / big blocks); 14-20 / 650-860 ms; ~23.3-23.8 | s94, s95 |
| **Exec thread** (serial, busy ~0.95 at saturation) | exec block_ms; exec-thread on-CPU per 1k fills | 62 ms per native block at ~16.8k fills; 2.70-2.84 ms per 1k fills | s94 (B), s95 |
| exec | engine (`torus_exec_engine_seconds`) | 49.2 ms per native block = 79% of block_ms (2.93 ms per 1k fills) | s94 |
| exec | cache `flush_all` | 13-14% of exec on-CPU; 6.8 ms per native block (0.405 ms per 1k fills), unchanged per fill since s81 | s90.4, s94 |
| exec | Keccak (two per-action hashes over ~10.8 KB) | was 12% (8.8-10.4 ms); after 8g verify 5.95 -> 3.68 ms and sort 5.40 -> 0.42 ms per native block; what is left is mostly the trust-cache key pre-pass inside verify | s92.2, s94 |
| exec | liquidation step (round-robin scan, 2,048 valued per block) | 7-8%, 5.6-5.9 ms per native block | s90.4 |
| exec | `save_order_books` (Classic whole-book borsh blob per dirty book) | 7%, 3.6-5.9 ms per native block, 0.21-0.23 ms per 1k fills at ~0.2M resting | s90.4, s94, s96 |
| exec | `put_cf_raw*` / malloc-free / borsh / `merge_disjoint` / `BTreeMap` insert / `open_order_counts` | 6-7% / 6% / 6% / 4.6-4.9% / 3-4% / 1.4% (overlapping inclusive shares; ~5 / 5 / 4.7 / 3.5-4.1 / 2.4-3.1 / 1.1 ms per native block at s90's block size) | s90.4 |
| exec | match phase; replay guard | 5.8 ms; 0.8 ms per native block | s94 |
| exec off-CPU | pool waits (exec thread waits on its own workers) | 25-29% of wall; 16.9-18.9 ms per native block: match setup 7.3-8.1, settle plan 4.1-4.6, cancel-all 3.6-4.4 (~5 jobs, too little work), `par_recover` 1.9-2.2 (all cache hits; skipped by 8g when every action hits); slowest job on CPU only 45-58% of each wait, last finisher preempted 15-38% (shared host) | s90.3, s92.3 |
| exec | checkpoint settle wait; upkeep | 2.0-2.6 ms; 0.9-1.0 ms per native block | s90.3 |
| flush worker W (off exec) | per block; checkpoint write | 5.6 ms per native block; 804-935 ms per ~160-198 MB checkpoint, 4.3-4.8 s per minute | s94, s90.3 |
| **Deep book** (1M resting) | save_books; matched/s; blocks/s; p50 | 78.9 ms per block (x19), 1.07 ms per 1k fills, +0.61 ms per 1k fills per 1M resting; -16% matched/s; 10.5 -> 2.9 blk/s; 18 -> 243 ms. Split: borsh encode of whole book 73%, output-Vec memmove 15%, jemalloc 3%, journal drop 2%, DB put 0.1-0.2%, hashing 0 (Classic). Checkpoint settle wait 2.0 -> 23.7 ms per block (same blobs through the layer merge) | s93, s96.2, s96.3 |
| **Consensus thread** (hotstuff-algo) | on-CPU | 36-44 ms per native block, busy ~22%; 41-46% of it Keccak = `compute_action_hash` at dispatch over the same bytes the exec thread hashes (11.2-12.5 ms per native block; not touched by 8g) | s81.4, s92.2 |
| consensus | blocked on the full exec channel (`send CommittedBlockMsg`) | 19-26% of wall in 6 of 9 windows, ~0 in 3 (s81: 30-33%): exec backpressure paces consensus | s90.3 |
| consensus | commit-time full body record (~6.1 MB per full block) on hotstuff-algo | ~12-15 ms per block (memory `ab99e13b`, s124 r3, from hsdb cells; not in a results section) | memory |
| consensus | checkpoint DB-write windows (shared write queue) | in-window block p99 ~500-650 ms above out-of-window (G); manifest put 6-12 ms in the queue (X) | s71, s74, s87, s89 |
| consensus | DA body GC (only with `--retention-blocks`) | 1.5-2.1 ms per native block, a few passes 51-205 ms | s95 |
| consensus | shard custody (off by default) | 140 ms per follower validate, -21.6% matched/s | s82 |
| **Network / ingress** (other threads) | node CPU split (older binary, mode 3 / Classic) | of 27.6-29.9 CPU-s per 1M: gossip signature recovery 4.2, ingress verify 3.8, exec workers 6.3-7.9, exec main 4.1-4.3, RocksDB bg 3.8, tokio 2.0, flush worker 1.0-1.7 | s37.2 |
| network | user cycles by thread (10 markets) | gossip-verify 24%, exec-pool 21%, ingress-verify 14%, exec 14%, tokio 9-10%, rocksdb:low 7%, hotstuff 5%; Keccak 24% of node, k256 point add 5% | s69.5 |
| network | tokio (libp2p) | 0.34-0.44 cores per node: QUIC 19.5%, ed25519 verify 19%, AEAD 11%, sign 11%, gossipsub 12%, bincode 7.6%, zstd 6.6% (memory `dabcab1c`, s124 r3; not in a results section) | memory |
| network | pre-proposal push | 512 KB manifest threshold: full-body push volume 10.1 -> 0.12 GB per cell, CPU-s per 1M -5.9%, matched/s +4.3% (now default via `env.sh`) | s86 |
| network | consensus gossip rate limiter (50 msg/s per author) | dropped leader headers -> lost-proposal timeouts; fixed by 250 ms header re-send | s77 |
| network (WAN) | real testnet | network-bound (gossip + DA), not engine-bound; numbers in that doc flagged unverified (2026-07-19 note); inter-validator legs 227-502 ms RTT | `testnet/p3-testnet/RESULTS.md`, roadmap T0.2 |

Reading: on one host the exec thread is the critical resource (busy ~0.95; a few % more exec work per fill halves
the block rate, s91). Exec on-CPU is a flat profile with no item above ~14%; ~18% of exec wall is pool-wait
scheduling. Consensus is paced by exec backpressure, not by its own CPU. On a WAN the binding cost is RTT and
dissemination, not measured on archy.

## 1. Consensus protocol, pacemaker and cadence

| idea | verdict | doc |
|---|---|---|
| 2-chain commit with 3-chain grandparent lock; model-check before f=1 | open (safety work, T1.2); s442 found the "forks" were an app body-determinism bug | roadmap T1.2-T1.3, `plans/s442-consensus-findings-and-limitations.md` |
| Re-add a third chain | not chosen | roadmap "deliberately NOT" |
| Header-first proposals, vote before body, early QC view advance (hybrid pipelining, option A) | shipped | `plans/hybrid-pipelining*.md`, `plans/consensus-fast-path-impl.md` |
| Speculative block production (next leader builds during voting, option C) | not chosen until T1.2 settles; never built | `plans/speculative-pipelining-impl.md`, roadmap |
| Commit-then-execute (HL model, exec after commit on own thread) | shipped | `plans/commit-then-execute-impl.md` |
| Epoch-boundary fast path F (no serial barrier at h % 100) | shipped default on (s128): native blk/s +88.6%, p50 -73.6%, matched/s -1.3% within noise | s84, s90, bench-queue 5b |
| Defer the leader's commit feed until after the header broadcast (s64 #1, s66) | no gain (s66 defer commit feed in the avoid list) | `perf/s64-exploration-2026-09-24.md`, memory `38e871cb` |
| Defer the commit feed on the next leader's parent-body insert (fix D) | no gain, not adopted | `perf/s72-parent-body-2026-09-28.md`, `plans/s72-defer-parent-feed*.md` |
| Collector local advance (broadcast AdvanceView before the feed) | no gain | `perf/s70-qc-split-2026-09-27.md`, memory `cb3604fd` |
| Serve block-data requests off the consensus thread | shipped (throughput neutral) | `plans/s72-body-serve-thread*.md`, s72 doc |
| Rejoin view sync (round-skip on an authenticated future-view header) + fixes A-E | shipped; rejoin freeze 24-36 s -> 0.3-0.7 s | `plans/rejoin-view-sync*.md`, `plans/rejoin-leader-sync.md`, `perf/s75-rejoin-and-pinning-2026-09-29.md` |
| Pacemaker timeout base 500 -> 1200 ms | shipped in devnet genesis (s68); s55 base 1500 at cap 100 was null on throughput | `perf/s68-timeout-base-ab-2026-09-27.md`, `perf/cap100-gap-attribution-2026-09-10.md` |
| Adaptive multiplicative pacemaker backoff (option C) | designed, status not recorded | `plans/pacemaker-backoff-*.md` |
| Pacemaker deadline grid never rebases when behind (s65 top lead) | open lead, never fixed or measured as such | `perf/s65-explorer-synthesis-2026-09-27.md` |
| Header re-send 250 ms (lost headers at the rate limiter) | shipped as code default (s128) | s77, roadmap pre-testnet |
| Direct consensus fan (`TORUS_CONSENSUS_DIRECT_FAN=1`) | opt-in; fixed s58 timeouts as well as re-send (s77, n = 1) | s77, `hyperliquid-comparison-2026-09-29.md` |
| Hold light proposals under the consensus rate limit | superseded by re-send 250 | memory `bfbd7612` (s58 r1) |
| Vote after body (follower) | no full-load regression, +19 ms vote delay at low load | `perf/s83-s85-campaigns-2026-10-03.md` |
| Vote-state DB (vote save p99 113 -> 4 ms) | no gain, parked until loaded views <= 300 ms | memory `cb3604fd`, roadmap T5.1 |
| Leader-reputation in-memory cache | impl plan written; B3 reputation stalled devnet, behind a kill switch | `plans/leader-rep-kv-cache-impl.md`, s64 doc 3 |
| `select_leader` O(view) -> closed form | shipped (S405) | `plans/blockspeed-orders-roadmap-s405.md` |
| Propose-defer on the exec queue (`TORUS_PROPOSE_EXEC_WATERMARK`, `TORUS_EXEC_QUEUE_DEPTH`) | parked, campaign B dropped (s126); revisit after Phase 5 by queue depth / lag; code pushed `exp/s123-propose-defer-on-exec-queue-v2` | bench-queue Done / dropped, `plans/item6-phase5-impl.md` "Parked" item 3 |
| Proposer commit on advance view; compact exec-channel handoff | carried, unverified (s124 r2-r3, rank 13-24) | memory `8b1039de` |
| Broadcast votes to leader(v+1) and leader(v+2); weighted quorum 2/1/1 | open (s64 list) | s64 doc 5 |
| 4th validator (f = 1) | open (T1.4); s442: 4-val devnet throughput dropped > 4x with 1 of 4 dead | roadmap T1.4, s64 doc 3 |
| Tier 5 attribution: ~310 ms cadence, ~1.2 s p99 floor, in-window p99 jump | attribution steps open; T5.3's checkpoint slicing A1 parked | roadmap T5.1-T5.3 |

## 2. Block building, proposals and mempool selection

| idea | verdict | doc |
|---|---|---|
| Hash-only proposals (CompactBlock) + mempool reconstruction | shipped | `plans/mempool-gossip-hash-proposals-impl.md`, `plans/native-action-da*.md` |
| Direct-to-leader forwarding | shipped | `plans/direct-to-leader-impl.md` |
| `PlaceOrderBatch` (one signature per 1,024 orders) | shipped (~38x) | `plans/o2-placeorderbatch-*.md` |
| Block cap 100 -> 200 -> 400 actions | shipped as bench shape (cap 400 +14% total matched, s76); cap 1000 wedged (s365) | `perf/s76-ingress-and-cap-sweep-2026-09-29.md`, roadmap T2.5 |
| Byte cap, mempool TTL, size-aware sync pull (Sprint 1 rails) | shipped | `plans/sprint1-throughput-rails*.md` |
| Incremental native-pool ordering (no full sort per `produce_block`) | open (roadmap T2.3; status not recorded) | roadmap T2.3 |
| Selection ordered by sender address (starvation) -> rotate / order by seq | open finding (s64 bug 5) | s64 doc 4 |
| Arrival-order selection | loss (stall), reverted | memory `38e871cb`, branch `exp/s66-abc-no-arrival-order` |
| Skip the leader re-mirror of already durable bodies; carry pool hashes into PendingProposal (s64 #5) | open; s68 DA-rewrite skip cut block_build -30% with no throughput change | s64 doc, `perf/s65-explorer-synthesis-2026-09-27.md` |
| Single-batch propose writes (`insert_and_update`, fd1fbba) | measured -20-25 ms on perf/matched-200k, never ported | s64 doc 3 |
| Block-build phase timers | shipped (s68) | `plans/s68-block-build-timers.md`, `perf/s68-block-build-phases-2026-09-26.md` |
| Next-leader payload pre-builder | open (s64 rank 11) | s64 doc 5 |
| Order-charged proposer budget for small actions | open (s124 r3, rank 34) | memory `9769f127` |
| Duplicate-inclusion tail fix (in-flight hash ledger) | shipped | `plans/dup-inclusion-tail-fix*.md` |

## 3. Dissemination, DA and networking

| idea | verdict | doc |
|---|---|---|
| Native-action DA store + push + by-hash pull | shipped | `plans/native-action-da*.md` |
| Big-body dissemination fix, push hardening, hash-only push with pre-warm pull | shipped | `plans/native-da-bigbody-*.md`, `plans/native-da-push-hardening*.md`, `plans/native-da-hash-only-push*.md` |
| Push-manifest threshold 512 KB (code default) vs bench 8 MB | shipped in `env.sh` (s86: CPU-s/1M -5.9%, matched/s +4.3%); WAN threshold open | s86, roadmap pre-testnet |
| Gossip pre-spread of native actions (Sprint 3) | shipped (default on); S458 gossip-off canary wedged the testnet | `plans/sprint3-gossip-prespread*.md`, `plans/wedge-durability-fix.md` |
| Native pre-spread over the direct zstd protocol instead of Signed+Strict gossipsub (`TORUS_NATIVE_PRESPREAD_DIRECT`) | open (s124 r3 KEEP minor: CPU-s/1M -3.2..-4.4%, matched/s ~0); never built | memory `dabcab1c`, `44dbf09a` |
| zstd on body paths (`/2.0` protocols) | shipped, on by default | `plans/sprint5-zstd-bodies*.md`, `plans/sprint5-zstd-bodies-rollout-note.md` |
| Erasure-coded body dissemination (RS k-of-n) | parked until the validator set grows (T3.1); scaffold `b29d843` uncompiled | `plans/sprint5-erasure-coding*.md`, roadmap T3.1 |
| Shard custody | opt-in, off by default (s128): -21.6% matched/s, 140 ms per validate | s82, bench-queue Q10 |
| Lazy serve-time shards | rejected as perf (bench pins custody 0) | memory `e7087388` |
| DA reconstruct off the consensus thread (BS-4a / BS-4b) | impl plan written; status not recorded | `plans/bs4a-da-reconstruct-offthread*.md` |
| DA batch read flushes only on a miss | shipped (s76) | `perf/s76-flush-on-miss-2026-09-29.md` |
| Leader body push with the header (`TORUS_BODY_PUSH_MAX_BYTES`) | +7% idle, neutral under exec saturation (July); push on main -10% view at low load (s85); off | s64 doc 3, s83-s85 doc, HL comparison |
| Follower requests body before voting; follower reconstruction pool-first | open (s64 #10, s65) | s64 doc, s65 synthesis |
| Mesh watchdog + explicit gossipsub peers; resubscribe-wedge fix | shipped | `plans/mesh-watchdog-impl.md`, s58 |
| QUIC AES-128-GCM preference | dropped (-0.4..-0.6% CPU, below resolution) | memory `8b1039de` |
| UDP receive buffer, shorter QUIC idle | open (s64 list) | s64 doc 5 |
| Sentries / validator-only mesh / consensus priority lane | open (HL comparison) | `hyperliquid-comparison-2026-09-29.md` |
| Co-locate validators (RTT) | open (ops, T0.2) | roadmap T0.2 |
| Trial testnet archy + cachy + 18c (WAN measurement) | planned (owner s131) | roadmap pre-testnet |

## 4. Execution engine, matching, margin and settlement

| idea | verdict | doc |
|---|---|---|
| Item 6 Phase 1 (resident rows, margin sums, liquidation L1), C1-C7, PF1, M1, B-blind | shipped (Gate 2 met, s19) | `plans/item6-phase1-impl.md`, s1-s23 |
| Item 6 Phase 2: P2-1 cancel-all index | shipped (gate missed, accepted) | s30, `plans/item6-phase2-impl.md` |
| P2-5 foldhash for exec maps | shipped (gate met) | s32 |
| P2-2 batch cache flush (`write_cf_raw_many`) | loss, reverted (+2.85 ms) | s33, memory `38e871cb` |
| P2-3 persistent exec worker pool | shipped (spawns 41 -> 0 per block; throughput within noise) | s44, s50 |
| Earlier s82 ahash on exec maps | no gain (~3% node CPU, below resolution); superseded by P2-5 | `perf/s82-exec-hasher-2026-09-30.md` |
| Item 6 Phase 4 (one record per trader) steps 1-4 | shipped (+3.0% at 300 markets; -2.75% at 10 markets, node build, cause not found) | s67, s69, s70 |
| Phase 4 step 5 stored open-order counter | no gain, dropped | s68 |
| Item 6 Phase 5 step 1 | shipped (`d3cab057`) | `plans/item6-phase5-impl.md` |
| Phase 5 step 2 (`Users` in place, write-through, dirty-set drain) | loss: exec-thread CPU per fill +5%, blk/s 10.8 -> 5.2, p99 +37%; not merged; Phase 5 parked (revisit triggers listed) | s91, `plans/item6-phase5-impl.md` |
| Phase 5 step 3 (book rows from a dirty set) | parked on `feat/item6-phase5-step3`, no 10k run | `plans/item6-phase5-impl.md` |
| Parallel cancel-all book pass (PC) | shipped default 8 workers (+4.51%) | s82, `plans/exp-s123-parallel-cancel-all-book-pass.md` |
| Parallel match setup (M), deferred pass-B results scatter (D), sell top-up stitch list (T) | shipped default on (s58 bundle, +9.1% together) | s75-s79, `plans/exp-archy-s58-*.md` |
| `TORUS_PARALLEL_ENGINE` (PE) | opt-in, off (s128): -1.4% matched/s, +2.4% CPU per fill, +15.7% blk/s; WAN A/B pending | s82, s83, `docs/design-parallel-engine.md` |
| Exec pool fine-grain jobs (G) | parked (s78.6) | s76, s78, branch `exp/archy-s58-exec-pool-fine-grain-jobs` |
| Match / settle / cancel-all pool-wait cuts (P2-P4: fewer chunks, worker caps, merged round trips) | open (8e upper bounds 4.0-4.3 / 2.1-2.3 / 1.5-2.1 ms per block) | s92.4 |
| `MATCH` / `SETTLE` worker caps on a 32-thread host | open, never A/B'd on archy | memory `38e871cb` |
| Margin write-back balance cache (O1, -29% in microbench) | planned S412; superseded in substance by item 6 resident rows (status not recorded) | `plans/o1-margin-cache.md`, roadmap T4.3 |
| Cloid engine-cost fixes | shipped (+5.6% at 100% cloid share) | s59, s60, s63 |
| Parallel sharded cloid upkeep | open (+4% at cloid share 100, 0 at the standard 0) | memory `e7087388` |
| Match epilogue batched trader index | open (s124 r3 rank 45, ~+1.2%) | memory `8b1039de` |
| Pass-A hot-market trader shards | carried unverified | memory `8b1039de` |
| Per-user open-order limit cost fixes (no-spawn) | shipped (olfix +4.1%) | s83-s85 doc |
| ADL per-block budget + dirty check | shipped | s24, `plans/adl-budget*.md`, `plans/adl-dirty-check.md` |
| ADL speedups (rank sort, balance reads, record search) | open, not now (18c s99) | results doc Open |
| Liquidation: check accounts touched this block first | open (P1 follow-up) | roadmap P1 |
| Liquidation scan parallel pre-value | refuted (s123) | memory `85a53ffe` |
| Cancel-all grouped by price level; two-ended cancel; integer-leverage cancel margin | candidates (s61), unmeasured | `perf/cancel-batch-2026-09-19.md`, `perf/cancel-all-integer-margin-2026-09-18.md`, `perf/throughput-campaign-2026-09-18.md` |
| Deep-cancel compaction threshold variants | loss (-6%) | s64 doc 3 |
| Block-STM / parallel EVM after journaling writer precompiles | deferred (T4.4) | roadmap T4.4 |
| Oracle off | +4.0% matched/s (s81); oracle stays on (product) | s81 |

## 5. Exec pipeline and threading

| idea | verdict | doc |
|---|---|---|
| Exec pipeline: flush worker 1-deep with applied-height fence; resident books | shipped (perf/matched-200k) | `perf/block-latency-campaign-2026-08-20.md`, `perf/design-exec-pipeline-2026-08-20.md`, `perf/exec-pipeline-bl2-impl-2026-08-20.md` |
| end_resident on a worker; begin_resident before `new_env` (step 2) | shipped | s15, s18 |
| Async post-flush stage | neutral in vivo, default off | `docs/design-flush-pipeline.md`, `mission-handoff-layer3.md` |
| Async validate (`TORUS_ASYNC_VALIDATE`) | performance gate blocked, never benched on | `plans/async-validate-flag-off-2026-09-17.md` |
| Validation worker thread | plan | `plans/validation-worker-thread-impl.md` |
| Exec throttle watermarks (8, 16, 24) | loss (-8 to -44%) | s77-s80 doc, memory `38e871cb` |
| Exec-aware mempool admission (`TORUS_ADMISSION_EXEC_AWARE`) | opt-in on main; 2-5 s horizons loss (-33 / -51%), 20 s inert; `perf/exec-aware-admission` parked WIP (uncompiled, unbenched) | s73, `configuration-reference.md` |
| Phase 5 capacity as the lag fix | parked with Phase 5 | s79.4 |
| CPU pinning per node | loss (-5.7%), not adopted; HOTSTUFF_CPUS pinning negative (s46) | `perf/s75-rejoin-and-pinning-2026-09-29.md`, s64 doc 3 |
| Renice / SCHED_BATCH bulk threads | rejected (BORE premise not supported) | memory `e7087388` |
| RocksDB pipelined write | bench env only (-9% view time, s46) | s64 doc 3, HL comparison |

## 6. Hashing, signatures and verification

| idea | verdict | doc |
|---|---|---|
| Exec hashing quick wins: one encoding for sort key + trust-cache key, skip all-hit `par_recover`, cached EIP-712 domain (8g) | shipped (merged; exec-thread CPU per 1k fills -4.5%, blk/s +39%) | s92, s94 |
| K1 key the trust cache by the dispatch action hash (skip the pre-pass) | open; needs fork-safety review ("DO NOT unify the two keys", session actions) | s92.4 |
| K2 sort key upstream on hotstuff-algo / K3 one absorb for both keys | partly covered by 8g (one encoding); one-absorb not built | s92.4, s94 |
| Exec action keys in parallel (KP, `TORUS_EXEC_PARALLEL_KEYS`) | opt-in, within noise (+1.03%, CPU +2.1%) | s82, `plans/exp-s123-exec-action-keys-parallel.md` |
| Exec-carried verified senders | folded into KP / 8g | memory `bfbd7612` |
| Double-verify trust cache | shipped | `plans/double-verify-trust-cache-impl.md` |
| Hash dedup on gossip / ingress verify (s77) | shipped (gossip-verify CPU -25%) | `perf/s77-s80-trade-history-and-streams-2026-09-30.md` |
| Commit-path recomputation: restash keys in the pool entry, reuse CompactBlock hashes in dispatch (s64 #4) | open for the dispatch hash (hotstuff Keccak 11-12.5 ms per block today); restash part status not recorded | s64 doc, s92.2 |
| Action-hash scratch buffers; batch-local DA body serialization | shipped (allocation only) | `perf/action-hash-scratch-2026-09-19.md`, `perf/native-da-body-scratch-2026-09-19.md` |
| EIP-712 batch item Keccak x4 (multi-buffer) | open (s124 r2 KEEP, CPU-s/1M -3.9..-4.9%, needs >= 2x microbench) | memory `e7087388` |
| libsecp256k1 ecrecover backend | open (s124 r2 KEEP, CPU only, recovery 0.41-0.43 cores per node) | memory `e7087388` |
| Parallel gossip-ingest verify on a dedicated pool | dedicated gossip-verify threads exist (s16, s69.5); roadmap T2.1 status not recorded | roadmap T2.1 |
| Proposer attestation + ed25519 batch verify | plan | `plans/proposer-attestation-impl.md` |
| Attest over action hashes instead of re-serializing bodies | open, consensus-format change | s64 doc 5 |
| Per-action crypto micro-optimisation at ingress | not chosen (inflation was CPU starvation) | `plans/ingress-cpu-supply.md`, roadmap |
| Binary ingress (A -> C -> B), ingress verify rayon | shipped (sprint 5 / s351) | `plans/sprint5-binary-ingress*.md`, `plans/ingress-verify-fix*.md` |
| Canonical-bytes caching end to end (T2.4) | partly (8g exec side); dispatch still hashes | roadmap T2.4, s92.2 |
| Ingress market-spec cache | open (CPU only, -4%) | memory `bfbd7612` |
| RPC block body raw JSON | split verdict (CPU only) | memory `bfbd7612` |

## 7. State, storage, checkpoints and RocksDB

| idea | verdict | doc |
|---|---|---|
| Native overlay everywhere (no disk writes until commit) | shipped | `plans/native-overlay-everywhere*.md` |
| Incremental EVM state root (A1) | shipped (3.19 ms at 1M accounts) | `plans/incremental-state-root*.md` |
| Incremental native root (A2, bucketed Merkle) | superseded: native trie maintenance off by default (root skip +31% matched/s at 300 markets) + running state hash | `plans/incremental-state-root-native-impl.md`, s12, `perf/s83-s85-campaigns-2026-10-03.md`, `plans/running-state-hash-impl.md` |
| Async native trie maintenance | not chosen ("wrong first move") | `plans/async-native-trie-maintenance.md` |
| Running state hash | shipped (no measurable cost, ~40 ms per hashed block on W) | s83-s85 doc |
| Item 6 Phase 3: coalesced checkpoints (15 s) + replay | shipped (CPU-s/1M -7%, writes -51%) | s41-s62, `plans/item6-phase3-*.md` |
| Phase 3 merge-wait fix | shipped (settle wait 23.4 -> 3.1 ms) | s54 |
| Checkpoint interval 5 s | tie / small loss | s55, s87 |
| Checkpoint size cap 50 MB (ZC) | loss (blk/s 0.36x) | s89 |
| Checkpoint slicing A1 (Q6) | parked (owner s128); design on `perf/q6-ckpt-slicing` | s87, s89, roadmap T5.3 |
| Defer book blobs in the layer merge (DB, `TORUS_CKPT_DEFER_BOOKS`) | opt-in, within noise (+0.95%); parked for the book-depth check | s82, `plans/exp-s123-defer-book-blobs-in-layer-merge.md` |
| HotStuff store + DA store in their own RocksDB (71.8 option A) | shipped (in-window DB wait -91%, block step -31%) | s74, `plans/hs-da-own-db-impl.md` |
| Raised dirty-page limit | ruled out (write queue, not writeback) | s71 |
| WAL cap 512 MiB | shipped (DB open 20.5 -> 7.9 s) | `perf/s74-wal-cap-2026-09-28.md` |
| Trade history off on validators + WAL budget 2048 | shipped as validator config | s38-s40 |
| Packed trade rows | shipped (history cost 28.5 -> 4.1 CPU-s/1M) | `plans/packed-trade-rows*.md`, s77-s80 doc |
| Append-CF codec ZSTD1 / LZ4 | ZSTD1 dropped (no change); LZ4 merged knob, off | s38, s39, `plans/append-cf-compaction.md` |
| WAL compression | merged knob, off; re-decide after Phase 3 | s40 |
| DA CF as RocksDB blob files | open (CPU only, -2.6..-3.3%) | memory `bfbd7612`, bench-queue Later |
| HL-style per-height files, prune by unlink | open (Later) | bench-queue Later |
| Compact commit body record (stop storing bodies twice, `TORUS_BODY_RECORD_COMPACT`) | open (s124 r3, CPU-s/1M -1.45..-1.6%, matched/s ~0) | memory `ab99e13b`, s64 doc |
| RH4 DA body GC (`--retention-blocks`) | shipped, retention off by default; prune-by-default pre-testnet open | s88, s95 |
| Block-tree pruner (`cf_consensus_meta` grows forever) | designed (S391); build status not recorded in the doc | `plans/block-tree-pruner-and-blockspeed-quick-wins.md` |
| Commit fsync durability (per-commit `flush_wal(sync)`) | designed | `plans/commit-fsync-durability-*.md` |
| RocksDB hardware CRC32C | open (CPU -0.8..-1.0%) | memory `e7087388` |
| WAL off for the state write | open lead | memory `38e871cb` |
| Staggered / 60 s checkpoints | open lead | memory `38e871cb` |

## 8. Book storage and the deep-book `save_books` cost

| idea | verdict | doc |
|---|---|---|
| Classic book rows (whole-book borsh blob per dirty book) | shipped default; testnet book mode = Classic (owner s107) | s36, results doc Open |
| Mode 2 level-rows-as-authority + hash-only native mirror (3c) | implemented on `perf/level-rows`; ship-blockers (mode-blind readers, mode not in genesis, fresh genesis) | `docs/design-level-rows-authority.md`, memory `25368eed` |
| Level-hash sponge cache (`TORUS_LEVEL_HASH_CACHE`) | shipped default on 256 MB (mode 2 path); cannot help match-touched levels | `docs/design-levelhash-cache.md`, `mission-handoff-layer3.md` |
| Mode 3 (`TORUS_BOOK_ROWS=3`, chunked) as default | reverted / not chosen at the standard shape (1.013x / 0.9645x); revisit before mainnet for the deep-book shape only | s36, results doc Open, memory `cb3604fd` |
| Chunked level hash / book digest (`perf/book-digest-3c` + `wip/book-digest-3c-stash`) | hard fork judged not justified; tag 0x04+ reserved | `docs/design-level-rows-authority.md` (risk 1, 6), `docs/mission-handoff-layer3.md` |
| Parallel Classic book save (B, `TORUS_PARALLEL_BOOK_SAVE`) | parked: +3.5% CPU-s per 1M at equal matched/s (s78.6) | s76, s78, branch `exp/archy-s58-parallel-classic-book-save` |
| Parallel mode-2 journal drain (save-books-parallel-levels) | shipped on perf/matched-200k | `perf/matched-200k-campaign-2026-08-18.md` |
| Presize the borsh output Vec (memmove 15% of deep save_books) | open (s96.6 candidate) | s96.6 |
| 0 / 1M A/B on mode 3 | open (s96.6 candidate) | s96.6 |
| Deep-book storage / topn precompile branches | WAN results unverified; topn regression inherited | `testnet/p3-testnet/RESULTS.md` |
| `getOrderBook` from the in-memory book | open (before mainnet) | results doc Open |

## 9. Memory, allocator, build

| idea | verdict | doc |
|---|---|---|
| jemalloc compiled in | shipped (matched/s -0.3%, blk/s +14.9%, CPU at fixed load +4.3%) | s51, s56, s57 |
| jemalloc decay settings | loss (-7 / -31%), except background_thread for the soak RSS gap | s62, s73, memory `38e871cb` |
| Release profile LTO / codegen-units 1 | open (s124 r2: +0.9..+2.0% matched/s central, gain check refuted the larger claim) | memory `e7087388`, s64 doc 5 |
| Ingress view skip position slices (RSS) | open (RSS hygiene) | memory `e7087388` |
| 1M-account RAM | passed (8.1-9.2 GB max per node) | s93 |

## 10. Ingress, admission and load shape

| idea | verdict | doc |
|---|---|---|
| Native anti-spam (admission limit, cancel-share) | shipped | s1, `plans/native-antispam-2026-10-04.md`, s53c |
| Per-sender in-flight cap + open-order budget in the generator | shipped as the standard load | s21 |
| Right-size offered load (RATE / CONC knee) | measured via the standard shape; s64 #6 | s64 doc |
| Batch-submit RPC, local submitters | shipped | `plans/sprint2-ingress-spread*.md` |
| Batch the ingress DA write (T2.2) | open (status not recorded) | roadmap T2.2 |

## 11. EVM (item 7)

| idea | verdict | doc |
|---|---|---|
| EVM lanes step 1 (budget, cadence N) | shipped (`853fed28`) | `plans/item7-step1-*.md` |
| Step 2 (HL small / big EVM blocks) | built, native A/B passed throughput (Q7) | `plans/item7-step2-*.md`, `perf/item7-step2-native-ab.md` |
| Option S: larger N | stopped after r1 (EVM pool filled) | s85 |
| Option 0 (EVM fast path, block overlay) | in progress by 18c: OFF LIMITS for 8k | `plans/item7-option0-impl.md`, 8k Rules |
| Order book first, then EVM; time-based CoreWriter delay | open (HL comparison) | `hyperliquid-comparison-2026-09-29.md` |

## 12. Recovery and restart

| idea | verdict | doc |
|---|---|---|
| Parallel boot book load | shipped (s74) | `perf/s74-boot-books-2026-09-28.md` |
| Graceful SIGTERM | shipped | s52 |
| Crash replay of committed-but-unexecuted blocks | works; 76-block replay at 1M took 41 s (chain halts with 3 validators) | s96.4 |
| Unwedge tool, body-durability invariant | shipped | `plans/unwedge-tool*.md`, `plans/wedge-durability-fix*.md` |

## 13. Idea pools already generated (count as "already identified")

- s64 explorers (20 agents): top 10 + ~40 one-liners, `docs/perf/s64-exploration-2026-09-24.md`.
- s65 explorers (56 agents, 193 ideas, ~167 left in ~80 themes, 49 angles never run), `docs/perf/s65-explorer-synthesis-2026-09-27.md`
  and the raw JSON in `docs/perf/s65-explorers/`.
- s123 workflow (21 candidates, 7 survived, 4 built; 14 refuted, 11 of them only in 18c's transcripts), memory `13db18e4`, `85a53ffe`.
- s124 / archy s58 rounds 1-3 (~45 candidates), memory `bfbd7612`, `e7087388`, `8b1039de`.
- Hyperliquid comparison adoption list, `docs/hyperliquid-comparison-2026-09-29.md`.

Rules that bind Phase 2 (8k prompt): do not touch item 7 option 0's code (EVM section and block overlay in
`torus-consensus/src/app.rs`, the flush worker in `exec_pipeline.rs`, `torus-state` backend / overlay absorb) or
RH4's DA GC; do not re-open item 6 Phase 5 (parked) or checkpoint slicing (parked).
