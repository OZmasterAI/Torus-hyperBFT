# Ledger part: roadmap (s80 novel-improvement search, prep)

Docs read (main eb993e3f, read-only): docs/Torus-hyperBFT-roadmap.md, docs/plans/wave-s134-tracker.md,
docs/plans/testnet-genesis-checklist.md, docs/Torus-hyperBFT-review.md, docs/hyperliquid-comparison-2026-09-29.md,
docs/mission-*.md, docs/l3-*.md, docs/design-*.md, docs/re-proof5-context.md, docs/buildlist-fixes-s17.md,
docs/parity-audit-fixes-s515.md, docs/val2-baseline-workorder.md, docs/work-order-s443-crash-recovery-fixes.md,
docs/ladder-10run-setup1-setup2b.md, docs/configuration-reference.md, docs/pruning-guide.md, docs/audits/*.md,
docs/reports/** (sprint-s395, hl-parity-2026-10-01 README + findings skim), research/*.md (skim), and the L-id table
of docs/perf/8k-explore-ideas.md (referenced by the roadmap and the owner list). Some statuses were checked against
code on main (env knobs, Cargo features, vote persist order). "(OWNER-LIST)" in the last field = excluded by
owner-list.md (s136). docs/perf/8k-explore-survey.md lists 177 already-explored items; it is not expanded here.

Format: - slug | mechanism | status | evidence | where recorded


## consensus

- t1-1-s430-parent-body-fetch | request_missing_parent: proactive by-hash fetch of missing parent body for body-starved leader; TEMP-DIAGNOSTIC kill-switch removed | merged | was ~84% failure under 8-way load | roadmap T1.1, 2e340938
- t1-2-lock-on-parent-2chain | pc_to_lock(Generic) locks on parent (not grandparent) to make 2-chain commit safe; stateright model PRODUCTION=Parent | merged | model found grandparent lock UNSAFE | roadmap T1.2, 4a94e8bc, f6fac872, 0923812d
- t1-3-header-path-lock-before-vote | header fast path: lock advance before vote, view-monotonic lock; vote only once body held (s84 option A); all relaxations modeled | merged | in stateright_2chain_safety | roadmap T1.3, 2f196f9c, 973cc747
- t1-3-app-invalid-vote-metric | metric counting header votes later found app-invalid, to observe vote-before-validate window | merged | header_vote_invalid_count increments on app-invalid body | roadmap T1.3, hotstuff implementation.rs (header_voted)
- t1-4-fourth-validator-f1 | 4-validator genesis f=1 after wrong-set-PC fix; restart inside uncertified epoch-change view rejoins | merged | trial net still plans 3 (owner open) | roadmap T1.4, 1b4c3b9f, 771b063, d995f68a
- t1-5-fail-stop-no-zombie-exec | fail-stop watcher; exec thread death halts consensus; replay stops at fail-stop; every failed serial write fail-stops | merged | — | roadmap T1.5, e63dcb70, 6f09ec3a, c0fd7c11, 3b93d3df, 01d991ab
- t1-6-crash-recovery-under-pruning | find_last_committed_height ignores pruner meta key; crash-injection harness | merged | — | roadmap T1.6, 6f09ec3a, 87a7b570
- failstop-delivered-height-fix | feed stops advancing delivered height after fail-stop latch; boot lowers delivered height and re-delivers gap | merged | RED delivered 8 vs app 3 | tracker s134, 6eabad95 (batch 7)
- t1-7-dead-leader-skip-b1 | count missed leader slots from committed blocks; exclude after K=3 misses, re-admit after L*2^j | in-progress | design: one-down ~1.5 blk/s (-87..93%) | roadmap T1.7, feat/dead-leader-p1/p2 (OWNER-LIST)
- dead-leader-a2-short-timeout | ~250 ms short local view timeout for a leader that missed its last slot; no format change | proposed-not-done | decided after one-down bench | roadmap T1.7, plans/dead-leader-skip.md 7a
- dead-leader-iwrr-normalize | normalize voting powers (gcd or smooth WRR) before genesis for IWRR leader rotation | proposed-not-done | open Q4 | dead-leader-skip.md:207, genesis checklist
- dead-leader-per-epoch-seeded-rotation | per-epoch seeded leader rotation instead of plain IWRR | proposed-not-done | open Q7 | genesis checklist s2
- jailing-j0-j4 | remove voting power of downtime/double-sign validators (evidence E1/E2, JailVote gate) | parked | DowntimeTracker has no caller | roadmap P5, tracker (OWNER-LIST)
- tc-required-option-b-tip-signing | leader collects TC for view-1 before proposing; signed tip views, domain-tagged signing, leader-signed headers, drop nudge tips | in-progress | batch 9 failed liveness gate under CPU load; header sign 36us verify 54us | tc-required-and-tip-signing.md, merge/s134-batch9 56923063 (OWNER-LIST)
- tc-option-a-advance-only-on-qc-tc | pacemaker rewrite: advance views only on QC/TC | rejected | option B chosen s134 | tracker 2e
- tc-option-c-keep-tc-optional | keep TC optional and document no tail-fork resistance | rejected | option B chosen s134 | tracker 2e
- tc-validation-gap-fix | TC validation gap + review gaps + NEC committed-set acceptance | merged | 8/9 mutations caught | tracker 2a/2d, 1b751f6c
- consensus-db-format-v1 | CONSENSUS_DB_FORMAT v1 refuses pre-phase-C consensus dirs at start | in-progress | comes with batch 9 | genesis checklist s2, 56923063
- reputation-leader-selection-b3 | reputation-weighted leader selection from locally observed successes (C8: divergent maps livelock) | rejected | kill-switched off; batch 9 refuses it at start | review C8, 76862c2c
- m2-timestamp-lower-bound | replica refuses to vote for proposal >30 s behind its clock, with reproposal exemption + tip_vouched | parked | forged-tip wedge found in review | roadmap P0, wip/m2-timestamp-lower-bound 62a6f8f4 (OWNER-LIST)
- p00-activation-height-gating | feature registry, genesis activations, header feature bits, UPGRADE REQUIRED, genesis digest | merged | phases 1-6; phase 7 ScheduleActivation pre-mainnet | roadmap P0.0, 40705baf
- evm-proposer-rotate-by-evm-height | rotate EVM proposer by EVM height instead of view to avoid N multiple of validator count | parked | keep N=11 for testnet (Q12) | roadmap pre-testnet, owner s131
- vote-persist-before-send | persist vote state before sending vote (C4 cross-crash equivocation) | merged | s65: set_vote_state_atomic before send_phase_vote on both paths | review C4, work-order-s443, implementation.rs:1465/2768
- vote-state-fsync-grouped | one grouped sync=true batch of vote-safety keys before vote send (~7 ms dsync/view) + stateright vote_state_durable_before_send | proposed-not-done | nothing on vote path fsyncs (July) | l3-persist-attribution s5, mission-handoff-layer3 debt
- sync-commit-safety-check | block-sync commits skip safe_block/safe_pc; made conditional on lock-on-parent outcome | merged | covered by T1.3 model | review C3, roadmap T1.3
- speculative-block-production | next leader pre-builds block during voting (speculative-pipelining PRP), ~15->10 ms | parked | deliberately excluded until safety model settled | roadmap "not on roadmap", plans/speculative-pipelining-impl.md
- re-add-third-chain | restore 3-chain commit to fix lock gap | rejected | costs ~1 RTT finality; lock-on-parent chosen | roadmap "not on roadmap"
- unknown-market-reject-in-consensus | executor rejects actions on markets with no CF_NATIVE_MARKETS row (UnknownMarket code 16) | merged | — | roadmap P0, de70325d, d78ea41b
- commit-lag-backoff-s470 | pacemaker lengthens deadlines by exp of (qc - committed) lag, cap 8, genesis field | merged | wedge fix S470; stateright lockstep backoff | l3-work-budget s2, torus-types lib.rs:1180
- speculative-commits-write-through | SPECULATIVE_COMMITS list cached write-through instead of full read-deserialize-rewrite twice per view | merged | S395 floor shave | sprint-s395-report, b674376
- drop-local-qc-reverify | skip redundant full signature re-verify of locally collected QCs (kept as debug_assert) | merged | n ed25519 verifies/QC saved | sprint-s395-report, b674376
- block-tree-pruner | prune block tree to stop height-correlated slowdown | merged | f99d07a | sprint-s395-report goal 3
- epoch-boundary-quorum-trap | 3 equal stakes: epoch-change view quorum > any two validators; halts <=100 views after losing one | decided | structural fix = 4th validator | sprint-s395-report finding 1
- boot-frontier-reconciliation | boot scans CF_COMMIT_MANIFEST above header scan and parks missing heights; watchdog on silent drain wait | merged | F-1 blocker | work-order-s443 F-1, plans/crash-safety-post-commit-impl.md
- sync-server-proof-of-commit | require QC proof before latching fatal safety violation from sync server | proposed-not-done | sync-server halt DoS | work-order-s443 s3
- slash-durability-atomic | fold buffered pending_slashes into atomic batch; replay keeps slashes | proposed-not-done | known limitation (c) | work-order-s443 s3
- consensus-write-panic-fail-stop | hotstuff-algo thread panic / native DA write failures latch node-wide fail-stop exit 70 | merged | light-load full disk now exits 70 | audits finding-consensus-write-panic, r01-fault-test R01b, d2b2736b
- validate-before-vote | move body validity (incl. timestamp) before vote, CometBFT PBTS-style | proposed-not-done | prerequisite for M2 | parity-audit-fixes deferred, roadmap P0 M2
- l1-1-unanimous-qc-1chain-commit | commit block on its own QC when all n votes present, else 2-chain | proposed-not-done | backlog (research), score 0.05 | perf/8k-explore-ideas L1.1
- dag-bft-mysticeti-shoal | uncertified-DAG BFT (Mysticeti/Shoal++) consensus | rejected | built HotStuff fork instead | research/hotstuff-evm-options, HL-comparison Part 2 note
- t5-1-cadence-attribution | per-view attribution of the ~310 ms block cadence at 300 markets, then pacemaker/pipelining/smaller bodies fix | queued | 3.13-3.24 blk/s; view dominated by proposal wait ~345 of 600 ms (s67) | roadmap T5.1 (OWNER-LIST after testnet)
- t5-2-p99-floor-attribution | explain ~1.2 s block-time p99 outside checkpoint windows, fix by cause | queued | val0 commit p99 1,164-1,281 ms | roadmap T5.2 (OWNER-LIST after testnet)
- storage-faults-fail-stop-r01-r02 | failed serial/marker writes and storage read faults fail-stop (exit 70) in every branch | merged | fault tests passed | main-chain-deep-review status R01/R02, roadmap row 74 aae6b9b3
- rank3-lock-fork-recovery-net | recovery net for lock forks (production debt) | proposed-not-done | listed debt, no design found | mission-handoff-layer3 production debt
- cons-pf-15-16-unbounded-maps | bound seen_proposals HashMap and bracha_timeout_counts BTreeMap (grow forever) | proposed-not-done | fix status not checked in these docs | research/consensus-safety-audit-3.4.2 CONS-PF-15/16

## pacemaker/timeouts

- timeout-base-500-to-1200 | raise genesis timeout_base_ms 500 -> 1200 | merged | dead views -85%, +7% matched/s, block rate flat | HL-comparison App A table, s68 timeout-base-ab
- header-resend-250ms-default | stuck-leader header re-send code default 3 s -> 250 ms | merged | timeouts 33-47 -> 3 per 300 s cell | roadmap pre-testnet, 3429855d (WAN check OWNER-LIST)
- header-resend-per-validator-budget | rate-limiter fix: per-validator header budget so re-send is a backup (option c) | rejected | option (a) 250 ms default chosen s128 | roadmap pre-testnet
- pacemaker-exponential-backoff | adaptive timeout/backoff; timeout_max_ms was parsed but dead (July) | merged | mission base a225844 "has pacemaker backoff" | review s4a item 3, plans/pacemaker-backoff-impl.md
- pacemaker-deadline-rebase | rebase pacemaker view deadlines (s65 idea) | proposed-not-done | listed untried s513 | HL-comparison App A s5
- load-scaled-view-deadline-rank16 | view deadline scales with load; consensus-visible genesis-gated | proposed-not-done | never scheduled (D-3) | mission-s470-status rank16
- epoch-fast-path-F | TORUS_EPOCH_FAST_PATH default on | default-on | shipped with PC in defaults change | roadmap pre-testnet, 3429855d
- proposer-exec-watermark | leader yields slot when local exec backlog exceeds TORUS_PROPOSER_EXEC_WATERMARK | merged | default off (0 = never yields) | l3-work-budget s2, HL-comparison App A
- leader-rep-kv-cache | cache highest_pc / validator_set_state KV reads on algorithm tick (~11 KV round trips/block) | merged | write-through cache for LeaderReputation + SPECULATIVE_COMMITS (~9 KV reads/round removed) | plans/leader-rep-kv-cache-impl.md, internal.rs BlockTreeSingleton
- pacemaker-tick-hot-state-cache | in-memory hot-state cache for Pacemaker::tick KV churn | proposed-not-done | claimed ~0.5-1 ms/view | mission-s470-status frontier #18
- pacemaker-rebase-forward-jump | rebase stale absolute per-epoch view schedule when a replica jumps ahead (deadline >2x view time away) | merged | fixed ~39k-view jump parking hours out | sprint-s395-report, 15bc833

## dissemination/DA

- t3-1-erasure-recovery-phase-a | erasure-coded bodies for pull-fallback recovery; reconstruct from any k shards, Merkle shard proofs, whole-body fallback | merged | only ~0.5x per-link at n=3 | roadmap T3.1, 9a0806a7
- shard-custody | validators custody erasure shards (TORUS_SHARD_CUSTODY=1) | rejected | custody on costs 21.6% matched/s, 140 ms per follower validate | roadmap pre-testnet, d6c4d0d2, perf s82
- t3-1-ingress-dispersal | validators custody shards at admission (T3.1 step 4) | queued | after testnet | roadmap T3.1 (OWNER-LIST)
- rh4-shard-count-fix | fix RH4 retention shard-count accounting for erasure shards | queued | waits for T3.1 | roadmap pre-testnet (OWNER-LIST)
- t3-2-zstd-reqresp-2-0 | zstd level-3 on /torus/{direct,native-da,block-data}/2.0 request/response, on-wire gauge | merged | 9.4x on bodies | roadmap T3.2, 0baf2143
- gossip-zstd-topic | zstd-compressed gossip topic | rejected | removed as DoS vector | roadmap T3.2, d922c1f2
- t2-5-block-cap-400 | NATIVE_TOTAL_BLOCK_CAP 100 -> 200 -> 400 without erasure coding | merged | +14% total matched, block time ~+20%; WAN unproven | roadmap T2.5, 7bcc179a, s76 (WAN OWNER-LIST)
- hash-only-push-threshold-512k | manifest+pull above 512 KB vs full push | merged | O5: 11.4k vs 0.5-4.5k o/s for 3 MB full push | review s4b knobs
- push-threshold-4mb-loopback | raise TORUS_HASH_ONLY_PUSH_THRESHOLD to 4-8 MB floor on loopback (one-way push) | tried-inconclusive | benches pin 8 MB, code default 512 KB; WAN value pending | mission-s470 frontier #26, roadmap pre-testnet (OWNER-LIST)
- consensus-direct-fan | consensus broadcasts over /torus/direct unicast fan instead of gossipsub (TORUS_CONSENSUS_DIRECT_FAN), dedup LRU | tried-inconclusive | opt-in, default OFF; fixed s58 timeouts like re-send (s77, n=1); parked for 250 ms re-send | mission-s470 B2 fcc49b54, perf ozarchy s77/s78.6, l3-arrival fix 3
- consensus-gossip-mirror-off | stop mirroring consensus msgs to gossip when fan on (TORUS_CONSENSUS_GOSSIP_MIRROR=0) | proposed-not-done | needs all validators flipped together | l3-arrival-attribution fix 3
- gossip-queue-len-512 | gossipsub connection_handler_queue_len cut to 512 (configurable TORUS_GOSSIP_QUEUE_LEN) + drop counters | merged | fixed 21.5 -> 1.15 blk/s collapse | HL-comparison App C s4, mission B3 ec3200c
- gossipsub-heartbeat-100ms | gossipsub heartbeat 500 -> 100 ms | merged | S391 | review App A, HL-comparison App C
- body-fetch-4s-wallclock-budget | body fetch retry budget as 4 s wall clock (s63) | merged | header-to-body p99 0.9-1.3 s vs 1 s budget | HL-comparison App C s4
- da-skip-mirror | skip redundant DA mirror write on propose | merged | block_build -30%, throughput flat | HL-comparison App A s5, cc81630
- serve-thread-body | serve body requests from separate thread (s72) | default-on | neutral | HL-comparison App A s5, f9b013e
- flush-on-miss | flush pending DA on body miss before validate (s76) | merged | validate -15 ms p50 (40 -> 25) | HL-comparison App A s5, bce54d0
- da-batch-read | batched DA reads (s46) | merged | -2.5% view time | HL-comparison App A s5
- fix-d-defer-parent-feed | defer parent commit feed (s72 fix D) | rejected | failed dissemination gate | HL-comparison App A s5, plans/s72-defer-parent-feed.md
- collector-local-advance | QC collector advances view locally before AdvanceView loopback (s70) | rejected | 45-70 ms saving reappeared as +55 ms parent-body wait | HL-comparison App A s3/s5
- body-push-with-header-july | push small bodies together with header (HL style) | tried-inconclusive | +7% idle, neutral under load; re-test on WAN | HL-comparison s4 / App A s5
- inline-body-full-proposal | broadcast full Proposal instead of header when body <= threshold (TORUS_INLINE_BODY_MAX_BYTES) | rejected | idle views halved, loaded cell collapsed (0 matched); revert advised | l3-persist-attribution s6, 0c3ea78
- body-push-v2-unsolicited | leader pushes body as unsolicited BlockDataResponse right after header (TORUS_BODY_PUSH_MAX_BYTES <= 64 KB) | measured-no-gain | A/B: arrival 58.7->21.3 ms but view 113.6->115.6 ms (work-conserved) | mission-handoff-layer3, bodypush-ab 2a0d207
- follower-body-request-before-vote | follower sends body request before casting its vote (s65) | proposed-not-done | est. ~-50 ms/view | HL-comparison s2/App A s5
- commit-feed-after-header-broadcast | leader sends commit feed after header broadcast, not before (s64 #1) | proposed-not-done | predicted view 660 -> 480-570 ms | HL-comparison s2/App A s5
- sentries-validator-only-mesh | sentry nodes + validator-only mesh, hide validators from Kademlia | proposed-not-done | DoS protection, pre-mainnet | HL-comparison s4/App C s3
- consensus-priority-lane | separate connection or priority lane so votes/headers never queue behind bulk data | proposed-not-done | proposal behind 128 KB batches caused stalls | HL-comparison s4/App C s5
- quic-mtu-discovery-raise | raise QUIC MTU discovery upper bound for loopback/devnet | proposed-not-done | claimed ~45x fewer packets | mission-s470 frontier #23
- quic-flow-control-windows | raise per-connection QUIC conn/stream flow-control windows | proposed-not-done | claimed removes latency cliff | mission-s470 frontier #24
- da-serve-inflight-lift | lift MAX_DA_SERVE_INFLIGHT=8 serve-pool cliff | proposed-not-done | claimed removes empty-response retry storm | mission-s470 frontier #25
- compress-once-zstd-cache | compress-once content cache + reusable zstd contexts for /2.0 frames | proposed-not-done | claimed 0.5-0.75 core off proposer | mission-s470 frontier #22
- block-store-eviction | swarm block_store never evicts; ne_sent_views grows per view (C9) | proposed-not-done | unbounded memory under flood (July) | review C9
- direct-to-leader-forward-batcher | batched 0xFD forward envelope to leader every 25 ms / 512 KB, flush-time leader re-resolution | merged | replaced 1 request per action | mission-s470 B1, HL-comparison App C (forward_batcher.rs)
- leader-hint-real-view | real pacemaker view + selection into LeaderState instead of height+1 heuristic | merged | — | mission-s470 leader-hint 9b6d797
- wedge-push-manifest-hardening | manifest+pull hardening made cap=1000 no longer wedge | merged | S387 probe | review s4b item 3
- pull-storm-single-source | body pull concentrated on proposer (s338 storm: 155 timeouts, 238 substream exhaustions) | merged | addressed by T3.1 phase A recovery | roadmap T3.1
- tcp-nodelay-direct-reqresp | TCP_NODELAY / direct req-resp to cut loopback transport hops if arrival dominates | proposed-not-done | idle 38 ms/view attributed to hops | l3-diagnosis fix 5
- proposal-wire-dispatch-stamp | stamp leader wire-dispatch time into proposal to split true wire delay vs build | proposed-not-done | attribution only | l3-arrival-attribution fix 1
- leader-da-mirror-one-batch | leader DA mirror as one atomic WriteBatch + one condvar wake per block instead of up to 100 writes | merged | S395 floor shave | sprint-s395-report, b674376
- wallclock-bounded-reconstruct-waits | reconstruct/pull waits bounded by wall-clock deadline (13x20 ms nominal grew to 500 ms+) | merged | — | sprint-s395-report, b674376
- message-cap-ladder-o5 | reconcile max_consensus_message_size / gossip / direct caps (O5 feed gates), consensus accept 1 MiB | merged | bs400 bodies exceeded caps (S395) | sprint-s395-report finding 4, plans/o5-feed-gates-impl.md
- erasure-rollout-drop-regression | native-DA/erasure rollout raised submit->include drop from 27-29% to 80-99% | decided | ladder r1 vs r2+ | ladder-10run synthesis 2
- da-bind-signature-in-content-address | bind full signed bytes into DA content address (action hash omits signature) | proposed-not-done | F-2 byzantine body substitution | work-order-s443 F-2
- l4-1-no-relay-author-gossip | ignore relay of author-direct native gossip when all subscribers are validators | queued | backlog; WAN at trial net | perf/8k-explore-ideas L4.1 (OWNER-LIST)
- l4-2-compact-gossip-codec | varint bounded-expansion codec for native batches on new topic version | queued | WAN bytes -25..-74% est. | perf/8k-explore-ideas L4.2 (OWNER-LIST)
- l4-3-prefilled-preproposal-manifest | flush gossip batch before push; inline likely-missing bodies in manifest | queued | WAN up to 1 RTT for 8-15% blocks | perf/8k-explore-ideas L4.3 (OWNER-LIST)
- l4-4-bbr-quic-windows | libp2p-quic with quinn BBR + BDP-sized send window | queued | only if loss >= 1e-5 | perf/8k-explore-ideas L4.4 (OWNER-LIST)
- parallel-multipeer-block-download | block sync downloads from multiple peers in parallel | proposed-not-done | — | research/technical-requirements 7.x
- udp-rcvbuf-drops | raise UDP receive buffer for QUIC (~70 drops/s observed) | proposed-not-done | not correlated with slow deliveries | HL-comparison App C s4

## ingress/mempool

- t2-1-gossip-verify-pool | gossip-ingest signature verification on dedicated torus-gossip-verify rayon pool | merged | removed ~10k action/s single-core ceiling | roadmap T2.1, dd7457e5
- shared-ingress-verify-pool | ingress verify on a shared (not dedicated) rayon pool | rejected | block time 529 -> 4034 ms (s352) | roadmap T2.1, plans/ingress-verify-fix.md
- t2-2-da-mirror-batching | ingress DA mirror batched to one WriteBatch at 128 bodies or 50 ms | merged | — | roadmap T2.2, dd7457e5
- t2-3-incremental-pool-order | native pool selection order as BTreeMap maintained on insert/remove, no per-block sort | merged | — | roadmap T2.3, dd7457e5
- t2-4-action-hash-once | compute keccak action hash once on shared verify path; reuse canonical bytes | merged | RPC ingress only; end-to-end once not proven | roadmap T2.4, dd7457e5
- verified-sender-cache-scales | VERIFIED_SENDER_CACHE_CAP scales with block cap, floor 16,384 | merged | — | roadmap T2.5
- unbounded-ingress-channels | bound inbound EVM/native channels and RPC->leader forward channels (C9) | merged | RPC->leader forward channel bounded + drop counter (B1); other channels unverified | review C9, mission-s470 B1
- txsubmitlimiter-prune | prune TxSubmitLimiter senders (never pruned) | proposed-not-done | July review C9 | review C9
- rpc-max-connections | raise jsonrpsee max_connections 64 -> 1024 (TORUS_RPC_MAX_CONNS) | merged | env-gated, Phase 0 #41 | mission-s470 frontier #1/#41
- submit-batch-max-raise | raise SUBMIT_BATCH_MAX 100 -> 500-1000, scale permits | proposed-not-done | claimed RPC call rate /5-10 | mission-s470 frontier #2
- rpc-runtime-worker-threads | env-tune RPC runtime worker_threads (fixed 4) and verify pool (cores/2) | proposed-not-done | — | mission-s470 frontier #3
- cache-known-market-ids | cache market ids, drop per-action RocksDB get from verify loop | proposed-not-done | claimed 5-15% verify CPU | mission-s470 frontier #4
- no-hex-in-json-bin-path | base64 / raw-bytes endpoint instead of hex in JSON on bin submit path | proposed-not-done | claimed halves ingress wire bytes | mission-s470 frontier #5
- compact-ack-bulk-submit | compact ack mode for bulk submit (no per-action hash strings) | proposed-not-done | claimed -30 MB/s, -400k allocs/s | mission-s470 frontier #6
- single-pass-ingest-rank12 | hash + bincode once outside pool lock on ingest | proposed-not-done | never started (D-2) | mission-s470 rank12
- fifo-selection-rank4 | FIFO block selection | proposed-not-done | never started (D-1) | mission-s470 rank4
- pool-scan-indexes-rank7 | indexes for pool scans | proposed-not-done | never started (D-1) | mission-s470 rank7
- ingest-qos-lanes-rank11 | ingest QoS lanes | proposed-not-done | never started (D-1) | mission-s470 rank11
- exec-throttle-watermarks-rank1 | scale proposer caps by exec queue depth (TORUS_EXEC_THROTTLE_WATERMARKS) | merged | unmeasured at the time | mission-s470 rank1 a992aaa
- address-volume-rate-limits | address-level rate limits earned by trading volume (HL style) | proposed-not-done | P4 | roadmap P4
- t0-1-cpu-pinning | pin node threads to isolated cores (cpuset/taskset) | measured-no-gain | -5.7% (s46, s75) | roadmap T0.1, HL-comparison App A s5
- t0-1-dedicated-validator-hardware | one box per validator, tooling off validator boxes | in-progress | partly: archy dedicated but 3 nodes on one host | roadmap T0.1
- da-pending-buffer-cap | cap/backpressure da_pending under failing local DA store (O(n^2) reflush) | proposed-not-done | degraded mode only | work-order-s443 s3
- evm-pool-drain-before-inclusion-r21 | EVM selection removes txs before durable inclusion; reinsert abandoned proposals | queued | planned after Phase 3 | main-chain-deep-review R21, astra round2
- lower-d2l-batch-ms | lower TORUS_D2L_BATCH_MS 25 ms forward flush on inclusion critical path | proposed-not-done | — | hl-parity findings e2e-latency-path
- admission-exec-aware | shed non-cancel actions while committed-unexecuted backlog is high (TORUS_ADMISSION_EXEC_AWARE) | merged | opt-in, default off | configuration-reference
- evm-tx-route-to-predicted-leader | EVM txs not gossiped; forwarded to predicted EVM-height leaders, in-flight filter, re-send <=8 | merged | — | configuration-reference "Routing (owner decision 7)"
- hygiene-bundle-rank17 | Package D hygiene bundle | proposed-not-done | never started (D-1) | mission-s470 rank17

## signature/crypto

- o2-placeorderbatch | one ecrecover per 1024-order batch | merged | ~38x (293 -> 11.2k orders/s) | roadmap Tier 2, review s4b
- per-action-crypto-microopt | micro-optimize per-action crypto | rejected | real cost ~5.8 ms, inflation was CPU contention (B4) | roadmap "not on roadmap", ingress-cpu-supply.md
- t2-1-batch-verify-api | ed25519 batch API for session keys + parallel secp256k1 recover; dedup vs trust cache before recover | merged | exec-path verify already rayon + ed25519-batched | roadmap T2.1 plan, mission-handoff-layer3 item 2
- parallel-verify-flag | TORUS_PARALLEL_VERIFY control + differential/cache tests | merged | cold verify ~18 ms vs 81 ms serial at cap-400 | mission-handoff-layer3, perf/l3-verify-par a2bcb88
- verified-pc-memo-cache | memoize verified PCs + batch/hoisted signature verify in hotstuff | proposed-not-done | claimed ~2-3% view at V=3 | mission-s470 frontier #17
- preverify-next-block-ecrecover | pre-verify block N+1 signatures while N executes | proposed-not-done | claimed 1.3-2x | mission-s470 frontier #27
- sig-attestation-digest-o1-rank15 | flag-gated O(1) signature-attestation digest; consensus-visible | proposed-not-done | never scheduled (D-3) | mission-s470 rank15
- eip712-typehash-statics | cache EIP-712 typehashes and domain separator as statics | proposed-not-done | claimed ~0.15-0.3 core | mission-s470 frontier #21
- eip712-per-variant-typehash | standard keccak(typeHash||encodeData) per ProposalAction variant | merged | fixed cross-variant substitution | roadmap P0, 9f02839a
- session-verify-batch-trust-cache | batched/trust-cached session-key verification (2-12 ms each, never trust-cached) | proposed-not-done | session-mode load collapsed (S395) | sprint-s395-report finding 5
- exec-trust-cache | verified-signature trust cache on exec re-verify (--exec-trust-cache) | default-on | CLI default on (July review knobs) | plans/double-verify-trust-cache-impl.md, review App B knobs
- l5-1-eip712-item-hash-memo | per-thread memo of PlaceOrderItem sponge states keyed on preimage prefixes | proposed-not-done | backlog; hit rate bench artefact; matched/s 0 | perf/8k-explore-ideas L5.1
- l5-2-known-key-ecdsa-comb-tables | verify against claimed sender's cached key with comb table instead of ecrecover | proposed-not-done | backlog; P -2.5..-4% standalone | perf/8k-explore-ideas L5.2
- l5-3-session-signed-low-cpu | stateless session-signature trust cache; v2 session digest over packed bytes | proposed-not-done | backlog (research) | perf/8k-explore-ideas L5.3
- l6-3-preexec-lookahead-verify | dedicated thread computes trust-cache keys, sort hashes, miss recovers one block ahead | proposed-not-done | backlog, score 0.33 | perf/8k-explore-ideas L6.3
- bls-aggregate-qc | BLS aggregate signatures for QCs (Meter style) | proposed-not-done | ed25519 vote sets today | research/hotstuff-evm-options

## execution/matching

- t4-3-position-cache-c1 | write-back PositionCache for position rows, deterministic sorted flush; fills settled through it | merged | claimed 3-5x settle (unmeasured then) | roadmap T4.3, 1b5494ac, 349a1300
- o1-balance-cache-write-back | per-execute_batch write-back balance/margin cache | merged | micro-bench 1177->831 us/400-order batch (-29%); superseded by C1 + item 6 | roadmap T4.3, review App C
- o1-balance-cache-write-through | write-through balance cache | measured-no-gain | null: cost is per-order overlay PUT not reads | roadmap T4.3
- c2-overlay-cf-interning | overlay CF-name interning + per-CF stack keys (no (String,Vec) alloc per get/put); drop PlaceOrderParams deep clones | merged | — | mission-s470 C2 ba17048, review "also noted"
- parallel-engine-pe | sender-sharded Phase-2 prepare across workers (TORUS_PARALLEL_ENGINE), node-local byte-identical | merged | default OFF: -1.4% matched/s, +2.4% CPU/fill, +15.7% native blk/s, -7% p99 | roadmap pre-testnet, design-parallel-engine.md, perf s83 (WAN OWNER-LIST)
- cross-market-match-parallel | MarketWorkerPool::match_parallel matches markets in parallel | merged | always-on already | mission-handoff-layer3 item 1
- engine-par-l3-microbench | parallel engine across markets at cap-400 | measured-no-gain | serial wins at cap-400 and 5k; +9-13% only at 25k | mission-handoff-layer3, perf/l3-engine-par 21b3073
- parallel-cancel-all-pc | parallel cancel-all book pass, 8 workers (TORUS_PARALLEL_CANCEL_ALL) | default-on | — | roadmap pre-testnet, 3429855d
- item6-phases-1-4 | in-memory execution phases 1-4 (resident state, overlay) | merged | — | roadmap pre-testnet, plans/item6-*
- item6-phase5-full-in-memory | full in-memory execution; drain moved to flush worker | parked | step 2 +5% exec CPU per fill; trigger >=15% exec CPU in flush | roadmap pre-testnet, plans/item6-phase5-impl.md (OWNER-LIST 9m)
- item7-evm-lanes-steps-1-2 | EVM only every Nth block (lanes), N=11 | merged | — | roadmap Tier 5 s131, plans/item7-*
- item7-option0-evm-fast-path | EVM blocks no longer barrier the exec pipeline (TORUS_EVM_FAST_PATH) | default-on | perf s107 | roadmap Tier 5
- item7-option-s-raise-n | raise EVM cadence N (bench Q12) | rejected | superseded by Q11; N stays 11 | roadmap Tier 5 (OWNER-LIST)
- item7-c0-evm-own-thread | EVM execution on its own thread | parked | trigger: EVM >3% exec time | roadmap Tier 5 (OWNER-LIST)
- item7-phase-t-evm-to-native | same-block EVM -> native for non-order kinds | queued | not built, status unknown | roadmap Tier 5 (OWNER-LIST)
- hl-order-book-first-then-evm | run order book first, then EVM per block so contracts see same-block fills | proposed-not-done | not built ("HL order") | HL-comparison s3, roadmap Tier 5
- t4-4-writer-precompile-journal | writer precompiles journaled with per-call-frame checkpoints; revert discards writes | merged | — | roadmap T4.4, 4b0a5834, 3cbed9b7
- block-stm-parallel-evm | optimistic parallel EVM execution with MV memory over native CFs | parked | deferred until gas budget binds | roadmap T4.4 (OWNER-LIST after testnet)
- evm-cross-block-warm-cache | shared moka/LRU over basic_ref/storage_ref/code_by_hash_ref invalidated from commit bundle | proposed-not-done | review: cheap, high-yield (unmeasured) | review s4c item 2
- corewriter-next-sequence-prefix-scan | next_sequence scans prefix on every CoreWriter enqueue, O(N^2) per block (EVM-PF-16) | proposed-not-done | — | HL-comparison App B s6
- corewriter-time-based-delay | wall-clock CoreWriter delay instead of one block (front-running protection) | merged | CoreWriter order actions drain at B+K, K=10 (~3 s), drain cap 200/block | HL-comparison s3, configuration-reference "CoreWriter delay" (item 7 owner decision 4)
- corewriter-drain-error-discard | drain_core_writer result discarded with let _ | merged | swallowed drain errors fixed (EVM-FIND-12); later fail-stop row 74 | HL-comparison App B s6, aae6b9b3
- exec-nonblocking-dispatch-rank2 | try_send into exec channel + bounded deferred_exec (TORUS_EXEC_NONBLOCKING_DISPATCH) | merged | default off; ~0 mean gain (variance only) | mission-s470 rank2 1075f68, l3-work-budget option 4
- deepen-exec-lead-past-64 | raise exec channel bound so consensus leads exec further | rejected | re-arms S470 wedge / OOM | l3-work-budget s4
- vote-before-exec-relaxation | let votes precede execution | rejected | already the case (vote at +1.6 ms, pre-body) | l3-work-budget s4 option 5, mission-handoff-layer3 item 3
- two-stage-exec-pipeline | overlap flush(N) with engine(N+1) on second exec worker with snapshot reads | proposed-not-done | est. 15-28 ms/view | l3-work-budget s4 option 2, frontier #29
- phase2-parallel-by-sender | parallelize Phase-2 reserve by sender (cross-batch unsafe verdict) | merged | became PE (default off) | mission-s470 frontier #30, design-parallel-engine.md
- matching-core-microarch-rank10 | FxHash indices, single-descent price levels, O(1) trader back-index removal | merged | — | mission-s470 rank10 3105b56
- fixedpoint-i128-fast-path-rank13 | FixedPoint i128 fast path skipping ethnum i256 when operands fit | merged | exhaustive boundary oracle | mission-s470 rank13 8cb5590
- batch-nonce-replay-guard | batch nonce replay guard via multi_get + memtable whole-key filter | proposed-not-done | claimed 5-10x exec_replay_guard | mission-s470 frontier #28
- exec-loop-untimed-residual | scrape all exec_* + exec_queue_depth to pin ~40 ms untimed exec-loop residual | proposed-not-done | residual ~40-50 ms/blk deschedule | l3-work-budget s1c/verdict
- resting-order-ttl-gc | TTL/GC of stale far-from-touch resting orders to cut level depth | proposed-not-done | driver 195k resting orders | l3-savebooks-attribution s4 item 4
- a5-maker-margin-release | maker fills release reservation via telescoping reserve() differences | merged | consensus-visible, fresh genesis | mission-s470 A5 98c76c6
- zero-margin-market-orders-fix | market orders reserve margin at mark and re-check at match | merged | A5 bug | roadmap P0, bd830af, 03c40ac
- exec-ceiling-decomposition | 20k orders 1 market: match 98.6 ms, margin 726.7 ms, settle 1699.1 ms | decided | matching is not the lever | sprint-s395-report finding 5
- o3-trade-history-off-exec | trade-history CF writes on background torus-trade-writer thread | merged | — | review App C (O3), l3-work-budget s0
- evm-native-one-flush-batch | EVM bundle, queue rows, native phase and applied marker in ONE write batch | merged | 93d4fff | work-order-s443 s3, parity-audit-fixes F1, plans/consensus-bug-c-evm-commit-atomicity.md
- consensus-work-ceilings-r16 | consensus-enforced aggregate deterministic resource ceilings + metered matching/cancel work | proposed-not-done | honest caps only (400 actions, 200k orders, 12 MB) | main-chain-deep-review R16
- cancel-all-sparse-index-r28 | sender-market/order indexes for cancel-all instead of broad book scans | merged | cancel-all index 3efff0d6 | main-chain-deep-review R28 A
- incremental-mark-revaluation-r28 | update cached position terms on mark/position change instead of rebuilding account views | proposed-not-done | needs fresh profile | main-chain-deep-review R28 B
- dependency-aware-evm-native-r28 | overlap EVM and native where dependencies proven safe | merged | addressed by item 7 option 0 EVM fast path | main-chain-deep-review R28 C
- in-block-seeded-priority-r31 | seeded deterministic in-block priority / within-account nonce order instead of address+hash sort | proposed-not-done | fairness policy, needs activation | main-chain-deep-review R31
- epoch-fast-path-bench | epoch boundary block skips full exec pipeline drain | default-on | block p50 57 -> 15 ms | configuration-reference TORUS_EPOCH_FAST_PATH (s84)
- evm-fast-path-q11 | EVM blocks skip pipeline drain | default-on | exec CPU/1k fills 0.981, blk/s 3.53x, p99 0.496x at EVM load | configuration-reference TORUS_EVM_FAST_PATH (s107)
- evm-big-blocks-lane | big EVM lane every M heights with own gas limit (item 7 step 2) | merged | M=0 off by default | configuration-reference "EVM blocks (item 7 step 2)"
- l1-2-optimistic-exec-at-1qc | feed exec at speculative 1-QC commit; durability at irrevocable height; roll back on mismatch | proposed-not-done | backlog (research), score 0.03 | perf/8k-explore-ideas L1.2
- l2-3-seq-inline-level-queues | lockstep VecDeque<u64> of seqs per level for contiguous partition_point | proposed-not-done | backlog; pool CPU -3 ms/block | perf/8k-explore-ideas L2.3
- l2-4-dense-trader-slots | intern addresses per market job into Vec slots | rejected | dead: s82 hasher precedent ~3% CPU no gain | perf/8k-explore-ideas L2.4, perf s82
- delegations-for-validator-scan | delegations_for_validator scans whole delegation table per call | proposed-not-done | perf backlog | buildlist-fixes-s17 item 4
- create-session-full-scan | CreateSession full-scans CF_SESSIONS on exec thread; expired sessions never pruned | proposed-not-done | O(all sessions) | hl-parity findings (session keys)
- cf-native-nonces-prune | consumed (sender,nonce) CF grows forever; prune outside window | proposed-not-done | — | hl-parity README Tier A, HL-comparison App D row 10
- matching-simd-cache-layout | SIMD / cache-friendly matching structures (plan 2.1b) | proposed-not-done | matching only ~5% of exec | research/implementation-plan 2.1b.1
- revmc-jit-pevm | revmc JIT (2-19x) and pevm parallel EVM (1.7x) path | proposed-not-done | EVM ~2-4 ms/block today | research/hotstuff-evm-options s6
- speculative-execute-after-1qc | execute block after 1 QC with rollback (plan 3.3.4) | rejected | commit-then-execute chosen; speculative commit only | research/implementation-plan 3.3.4, review s1
- engine-contention-remeasure | re-measure engine 41.6 ms in vivo vs 16 ms ubench after save_books freed CPU; settle engagement tuning | proposed-not-done | 2.6x contention | re-proof5-context s4 item 3

## settlement/margin/liquidation

- c3-parallel-settle | deterministic parallel Phase-4 settlement (TORUS_PARALLEL_SETTLE) for blocks >1024 fills | merged | pass-A only parallel | mission-s470 C3, roadmap pre-testnet aliased rows
- l2-1-streamed-settle | streamed settle (TORUS_SETTLE_STREAMED) | in-progress | +0.75% exec CPU/fill, +8.6% deep blk/s; batch 10, default off | genesis checklist s3 (OWNER-LIST)
- aliased-rows-reject-on-load | decode_position_row rejects row whose key != own position_key | merged | parallel vs serial settle disagreed before | roadmap pre-testnet, b57b3fd4
- offmark-loss-and-price-band | charge off-mark loss at fill + chain-wide price band +-50% of mark | merged | probe: -900 bad debt route closed | roadmap P0, f6b0f4c5, 45e6de00
- liq-touched-accounts-first | liquidation step checks accounts touched this block before round-robin | proposed-not-done | decide before mainnet | roadmap P0 follow-ups
- liq-value-sum-walk | slow value-sum walk with TORUS_LIQ_VALUE_SUM=1 (proof-only) | proposed-not-done | open from s72.6 | roadmap P0 follow-ups
- liquidation-adl-every-block | HL-style liquidation + ADL at end of every block, bounded 2,048 scan / 64 act | merged | stress cell PASS S=750 | roadmap P1, b360813, 735bb570
- delist-option-c-settlement | delist grace N blocks reduce-only then forced close at one price, bounded 2,048 traders/256 positions per block | merged | — | roadmap P1, 21f385c6, 9f02839a, 52a95d86
- adl-leverage-weighting | ADL ranking by PnL x leverage (HL) not PnL only | proposed-not-done | — | HL-comparison App D row 7
- liquidator-vault-genesis-seed | seed liquidator vault with genesis capital, ADL backstop | decided | amount open | roadmap P5 (OWNER-LIST)
- hlp-deposit-vault | HLP-style deposit vault that takes liquidations before ADL | queued | pre-mainnet | roadmap P5 (OWNER-LIST)
- funding-engine | premium sampling from impact prices, hourly settlement, cumulative index | queued | OPEN | roadmap P2 (OWNER-LIST)
- mark-price-hl-median | median of oracle+basis, book, external for margin/liq/stops | queued | PARTLY | roadmap P2 (OWNER-LIST)
- per-market-oi-cap-max-notional | per-market OI cap, max order notional, per-market band overrides | queued | OPEN | roadmap P2 (OWNER-LIST)
- isolated-margin-update-leverage | isolated margin + updateLeverage/updateIsolatedMargin actions | queued | OPEN | roadmap P2 (OWNER-LIST)
- maker-taker-fees | flat maker/taker fees debited at settlement to treasury | queued | OPEN | roadmap P2 (OWNER-LIST)
- account-level-margin-f1 | account-level free margin, HL cross model | merged | — | roadmap P0, ca06c8c
- liq-affected-account-priority-queue | R06 option A affected-account priority + fair sweep, then B persistent urgency queue | proposed-not-done | 100k accounts >=49 passes to scan | main-chain-deep-review R06
- liq-raise-caps-only | R06 option C: just raise 2,048/64 caps | proposed-not-done | review: block p99 cliffs remain | main-chain-deep-review R06
- maker-free-margin-per-market-r05 | fix parallel books reusing a maker's free budget per market (options 4/6/7) | queued | counter added s99 | main-chain-deep-review R05
- l2-2-no-merge-position-flush | parallel settle keeps per-market position caches as runs; flush in global key order (TORUS_SETTLE_NO_MERGE) | default-on | std blk/s 1.095, p99 0.877, exec CPU 0.957 | perf/8k-explore-ideas L2.2, configuration-reference (s108)
- l1-3-cadence-invariant-liq-scan | liquidation budget = rate x header time delta (cap 2,048) | proposed-not-done | backlog, consensus-changing | perf/8k-explore-ideas L1.3
- liquidator-vault-deficit-monitoring | gauge torus_liquidator_vault_deficit + RPC torus_getLiquidatorVault | merged | — | roadmap P5, cfd2db4f, 86c42f43

## state/storage/rocksdb

- t5-3-commit-persist-writer | commit manifest in consensus DB, off-thread CommitPersistWriter header/body write, HeadPublisher | merged | ckpt-window gap G 452->100 ms, in-window p99 917->485 ms | roadmap T5.3, dfb55f70
- q6-a1-checkpoint-slicing | slice checkpoint batch so consensus manifest put does not wait behind it | parked | X only ~6-12 ms/put; at most a few % of G | roadmap T5.3, perf/q6-ckpt-slicing (OWNER-LIST)
- z2-checkpoint-max-bytes-cap | cap checkpoint batch at 50 MB (TORUS_CHECKPOINT_MAX_BYTES) | measured-no-gain | X halved, write -73%, G flat; ckpt rate x3, blk/s to a third | roadmap T5.3
- hs-da-own-db | own RocksDB for HotStuff and DA stores, off the shared state DB | merged | consensus/DA in own DB; T5.3 manifest lives in consensus DB | roadmap T5.2/T5.3, plans/hs-da-own-db-impl.md
- rh4-prune-by-default | prune by default N=100k; RH4 DA GC worker off consensus thread; state-pruner compaction filter | merged | 9i 100k soak PASS; GC passes 51-205 ms removed from consensus thread | roadmap pre-testnet, b452b4af
- rh4-derive-from-block-tree | derive "may this body go" from block tree at deletion time instead of separate records | parked | not chosen s128, revisit on divergence | roadmap pre-testnet
- l3-1-book-coalesce | checkpoint-coalesced Classic book save (TORUS_BOOK_COALESCE) | default-on | deep matched/s 1.160, blk/s 3.42->15.76, p50 193->13 ms, p99 1461->672 | roadmap pre-testnet, perf s110
- l3-3-book-fast-encode | fast book encode (TORUS_BOOK_FAST_ENCODE) | default-on | deep blk/s 1.086, p50 0.884, p99 0.919 (n=1) | roadmap pre-testnet, perf s105
- l6-1-window-committed-books | window-committed books so L3.1 keeps gain with running hash on | in-progress | 9g deep B/A 0.997, blk/s 4.82x vs C | roadmap, merge/s136-batch10 (OWNER-LIST)
- book-mode-3-level-authority-chunked | TORUS_BOOK_ROWS=3 level-authority chunked rows, book save on flush worker | parked | 8m: save 65.7->4.93 ms, blk/s 3.24->10.14 but +18% CPU-s; lost 9j (0.963/0.952) | roadmap pre-testnet, perf s99/s124 (OWNER-LIST)
- b1-mode3 | b1 variant on mode 3 | parked | lost head-to-head to Classic CL21 | tracker 5, perf/mode3-b1-main f4faa419 (OWNER-LIST)
- b0-book-save | b0 book-save variant | parked | — | roadmap pre-testnet (OWNER-LIST)
- book-rows-mode2-level-authority | level rows as authority + hash-only mirror (TORUS_BOOK_ROWS=2) | merged | flush 1.4 s -> 0.23 s/blk; 13,659 matched/s window | mission-handoff-layer3, design-level-rows-authority.md
- resident-books-rank8 | resident in-memory order books (TORUS_RESIDENT_BOOKS) | merged | writes 201 -> <=3 per block | mission-handoff-layer3 bench env, mission-s470 rank8
- rocksdb-cf-shape-options | shape-differentiated CF options: universal compaction / no upper compression on firehose CFs; small write buffer on churny CFs | merged | churny CF write buffer knob TORUS_CHURNY_CF_WRITE_BUFFER_MB=16 | mission-s470 frontier #9/#12, l3-diagnosis fix 3, mission-handoff-layer3
- rocksdb-bg-jobs-subcompactions | max_background_jobs 4->8, subcompactions, WriteBufferManager, max_total_wal_size | merged | compaction knobs in re-proof5 | mission-s470 frontier #10, l3-diagnosis fix 3
- rocksdb-bytes-per-sync | A/B bytes_per_sync 1 MiB vs 4 MiB or 0 | merged | knob TORUS_BYTES_PER_SYNC_MIB exists; A/B result not in these docs | l3-diagnosis fix 4, torus-state db.rs
- rocksdb-cache-env-tunable | expose block cache 256 MiB / memtable 128 MiB / bloom via env | proposed-not-done | block cache still not env-tunable; write buffer has TORUS_DB_WRITE_BUFFER_MB | review s4c knobs, torus-state db.rs
- trade-history-wal-off | disable_wal on background trade-history writer | proposed-not-done | claimed ~half WAL bytes | mission-s470 frontier #11/#13
- fd-limit-max-open-files | raise ulimit + bound max_open_files (-1 exhausts fds) | proposed-not-done | claimed validator-death mode | mission-s470 frontier #14
- pipelined-write | RocksDB PIPELINED_WRITE | default-on | -9% view time (s46); default on since plan 9.13 (TORUS_ROCKSDB_PIPELINED_WRITE) | HL-comparison App A s5, torus-state db.rs
- single-batch-propose-writes | batch propose-path writes into one RocksDB write | proposed-not-done | est. -20-25 ms, never ported | HL-comparison s2/App A s5
- retention-gc-per-order-cfs-rank9 | retention horizon GC for per-order CFs | merged | superseded by RH4 retention | mission-s470 rank9
- snapshot-manager-commit-wiring | wire SnapshotManager into commit + P2P state sync for new nodes | queued | P4, only tests use it | roadmap P4 (OWNER-LIST P4)
- bincode-once-persistence-rank5 | persist bodies with bincode encoded once | proposed-not-done | never started (D-1) | mission-s470 rank5
- da-mirror-shave-rank6 | shave DA-mirror work | proposed-not-done | never started (D-1) | mission-s470 rank6
- deep-book-capacity-8f | 1M accounts / 1-1.7M resting orders capacity check | in-progress | RAM PASS 8.1-9.2 GB; matched/s -14.3%, blk/s 13.3->3.0 | roadmap pre-testnet (OWNER-LIST 9n)
- save-books-dirty-only | save_order_books writes only dirty markets, not every loaded book (was 6-9 MB/block) | merged | byte-identical | sprint-s395-report, 69f0a99
- next-global-order-id-persist | persist next_global_order_id durably off-root | merged | fixed order-id reuse on restart | sprint-s395-report, 69f0a99
- lazy-book-loading | load order books lazily | rejected | all-books iteration sites make partial maps consensus-unsafe | sprint-s395-report
- sync-wal-on-commit | fsync WAL at commit (TORUS_SYNC_WAL_ON_COMMIT) | merged | default off; ~-30% low-load cadence when on, inclusion-neutral | ladder-10run synthesis 3, plans/commit-fsync-durability-impl.md
- group-commit-batched-fsync | group commit / batched fsync to cut opt-in WAL sync cost | proposed-not-done | follow-up of WAL1 ladder | ladder-10run synthesis 4
- rocksdb-max-total-wal-2048 | max_total_wal_size 2048 MiB default | merged | acc2 -6.3% node CPU vs 512; restart open 1.3->4.0 s | torus-state db.rs (s109)
- rocksdb-wal-compression | zstd WAL record compression (TORUS_ROCKSDB_WAL_COMPRESSION) | merged | default off | torus-state db.rs
- append-cf-bottommost-codec | bottommost codec for append-only CFs (trades, DA) | merged | default exact-today | torus-state db.rs, plans/append-cf-compaction.md
- l3-4-fifo-compaction-da-cf | RocksDB FIFO compaction for cf_native_pending | rejected | owner ruled FIFO out (breaks heal / RH4) | perf/8k-explore-ideas L3.4
- rocksdb-l0-triggers-write-buffers | tunable L0 slowdown/stop triggers and max_write_buffer_number | merged | default exact-today | torus-state db.rs
- queue-position-binary-search | OrderBook::queue_position binary-searches seq instead of front-to-back scan | merged | 10.66 -> 0.86 us/row on 7,000-deep level | design-levelhash-cache s7.1
- save-books-workers-parallel-drain | parallel journal drain across dirty books (TORUS_SAVE_BOOKS_WORKERS), serial writes | merged | byte-identical across workers 1-64 | design-levelhash-cache s7.1
- parallel-boot-verify-per-market | parallelize O(depth) boot verify per market | proposed-not-done | ~seconds at 1M | design-level-rows-authority s9 risk 5
- book-order-rows-state-sync | state-sync transport for cf_book_order_rows | proposed-not-done | fresh nodes replay from genesis | design-level-rows-authority s9 risk 3
- monaddb-io-uring | custom trie DB with io_uring async I/O (MonadDB style) | proposed-not-done | research only | research/hotstuff-evm-options
- t5-3-in-window-attribution | phase breakdown of consensus + exec thread inside vs outside checkpoint windows (A1/Q6 parked behind it) | queued | G drifts 497-637 ms between campaigns | roadmap T5.3 (OWNER-LIST)
- vote-db-ab-s67 | vote DB save A/B on consensus thread | measured-no-gain | vote save p99 113 -> 4 ms, no change downstream | roadmap T5.1 (s67)
- trie-node-pruning | prune EVM trie nodes (refcount / mark-sweep / snapshot diffs) | proposed-not-done | trie CFs never pruned | pruning-guide "Not Yet Pruned"

## checkpoint/hash

- t4-1-evm-root-once | EVM state root computed once per block; TrieUpdates plumbed into commit | merged | removes ~2/3 root cost | roadmap T4.1, 38841cf2
- t4-2-native-incremental-root | bucketed-Merkle native root, 65,536 buckets, full-scan oracle in CI | merged | EVM A1.6 3.19 ms @1M vs 6.98 s full scan | roadmap T4.2, cd803ede, 64c46193
- incremental-oracle-off-prod | keep TORUS_INCREMENTAL_ORACLE off in production | default-on | oracle costs 350-503 ms/block | review s4c item 3
- running-state-hash | running state hash h_n with checkpoints and attestation | decided | ON at genesis (s136) | roadmap T4.2, plans/running-state-hash-impl.md (OWNER-LIST)
- parallel-bucket-hash-off | TORUS_PARALLEL_BUCKET_HASH=1 (serial) at ~226 dirty buckets | merged | N=226 p1 4.22 ms vs p4 6.47 ms (-35%); serial default (TORUS_PARALLEL_BUCKET_HASH unset) | l3-diagnosis fix 1, native_trie.rs
- parallel-bucket-min-threshold | adaptive gate: parallel bucket hash only above PARALLEL_MIN_BUCKETS (~3000) / TORUS_BUCKET_HASH_MIN_BUCKETS | merged | TORUS_BUCKET_HASH_MIN_BUCKETS default 1 = exact-today; helps uncapped only | l3-diagnosis fix 1, design-flush-pipeline s1
- member-cache-misses | raise TORUS_BUCKET_MEMBER_CACHE_MB / pre-warm hot buckets to cut cold CF_NATIVE_HASHED scans | tried-inconclusive | prod root 26 ms vs warm 6.47 ms; hits not scraped | l3-diagnosis fix 2
- native-root-cache | trie node cache for native root (TORUS_NATIVE_ROOT_CACHE=1) | merged | bench standard env | mission-handoff-layer3
- level-hash-sponge-cache | incremental keccak absorb for tail-append-only levels, byte-identical (TORUS_LEVEL_HASH_CACHE 256 MB) | default-on | +45% blk/s, +81% matched/s, save_books 83->37 ms | design-levelhash-cache.md, cacheab-18c-13bd833
- chunked-level-commitment | level_hash = keccak of fixed-K chunk hashes; O(sqrt depth) rehash; consensus-visible tag 0x04+ | parked | built as book mode 3 level_authority_chunked (64-seq chunks); lost 9j to Classic | l3-savebooks-attribution s4 #1, design-levelhash-cache s7
- merkle-over-orders-per-level | per-level Merkle root over order leaves, O(log depth) update | proposed-not-done | more invasive than chunking | l3-savebooks-attribution s4 #2
- parallel-trie-recompute-rank14 | parallel trie recompute after C4 | proposed-not-done | deferred by design | mission-s470 rank14
- c4-per-order-row-book-persistence | replace whole-book Borsh+keccak with per-order rows (changes state-root format) | merged | became book rows modes | mission-s470 C4
- state-root-triple-compute | root computed in validate_block_for_catchup, commit and resync | merged | fixed by T4.1 | review s4c
- checkpoint-interval-15s | validator checkpoint every 15 s | decided | 9j winner ran 15 s | genesis checklist s3
- evm-fallback-resync-storage | fallback commit resync covering contract storage so incremental trie never lags | proposed-not-done | F3 B2 open item | parity-audit-fixes deferred
- l3-2-running-hash-v2-classic | running hash absorbs per-block order-row deltas instead of whole-book blob | rejected | dead (weakens fork detection) | perf/8k-explore-ideas L3.2
- l6-2-attestation-window-commitment | per-block response hash + window digest for all hashed CFs | rejected | dead: premise false (flush_all not caused by hash) | perf/8k-explore-ideas L6.2
- hash-only-native-mirror | native mirror leaf framing hash-only (halves bucket bytes) | merged | 3c preimage round | design-level-rows-authority s3.2

## serialization/wire

- receipts-root-serde-json | receipts_root = keccak(serde_json(receipts)); move to RLP/bincode (C10) | proposed-not-done | consensus depends on serde_json stability (July) | review C10
- json-persisted-headers-bodies | headers/bodies/receipts JSON-persisted, 2-3x byte inflation on 6 MB bodies | proposed-not-done | July review C10 | review C10, frontier #31
- attestation-digest-unwrap-default | attestation_digest bincode serialize unwrap_or_default hashes nothing on failure | proposed-not-done | July review "also noted" | review s3
- action-identity-serde-json-keep | keep action hash = keccak(canonical serde_json); cache bytes, do not change definition | decided | lockstep change avoided | roadmap T2.4
- commit-callback-slimming | kill duplicate body persist, move JSON serialize off consensus thread | merged | addressed by T5.3 CommitPersistWriter | mission-s470 frontier #31, roadmap T5.3
- arc-block-data | Arc-ify hotstuff block Data to kill 6 MB deep clones on propose/serve/insert | proposed-not-done | claimed +5-12% blk/s under 6 MB load | mission-s470 frontier #15

## threading/runtime/scheduling

- exec-thread-single-worker | single torus-execution thread drains 64-slot channel; view wall = exec drain rate | decided | work-conservation law | l3-work-budget s0
- work-conservation-law | cutting one consensus-thread phase moves wait elsewhere (qc_collect absorbs) | decided | s68 build -56 ms -> view -8 ms; body-push A/B view flat | l3-work-budget, HL-comparison App A s3
- async-validate | TORUS_ASYNC_VALIDATE off-thread validate | parked | implemented (3e4f0c2), default OFF, perf gate blocked, never benched ON; predicted hit rate <30% | plans/async-validate-flag-off-2026-09-17.md, perf s64
- poller-condvar-wake | replace 250 us poller park with condvar notified by enqueue_inbound | proposed-not-done | ~125 us avg, marginal | l3-arrival-attribution fix 2
- algo-loop-10ms-body-wakeup | close 10 ms block-body wakeup gap in algorithm loop | proposed-not-done | claimed 4-9 ms/view on followers | mission-s470 frontier #16
- event-publish-gating-log-demote | per-event-type publish gating + hot-path log demotion (unbounded event channel) | proposed-not-done | — | mission-s470 frontier #19
- async-body-persist-flush-pipe | defer body_persist off exec chain (perf/l3-flush-pipe) | measured-no-gain | ubench -22% (0.543->0.664 ms/blk); in-vivo async-post-flush NEUTRAL | mission-handoff-layer3, design-flush-pipeline.md
- exec-backlog-deschedule-tax | relieve ~40-50 ms/blk deschedule tax from oversubscribed cores | proposed-not-done | estimate | l3-work-budget s1c
- single-host-rig-contention | rig load 36-40 on 18 vCPU; algo thread waits for core as long as it runs | decided | s73 s6, s76 rig is ceiling | HL-comparison App A s6
- disk-tenant-minute-dips | minute-aligned dips -26% views/s from other-tenant disk latency | decided | s73 | HL-comparison App A s3

## build/compiler/allocator

- release-thin-lto-cgu1 | [profile.release] thin LTO + codegen-units=1 | proposed-not-done | no [profile.release] in workspace Cargo.toml on main | mission-s470 frontier #7
- jemalloc-global-allocator | jemalloc/mimalloc global allocator | default-on | torus-node default feature jemalloc (tikv-jemallocator, unprefixed) | mission-s470 frontier #8, crates/torus-node/Cargo.toml
- opt-level-1-deps-ab | opt-level 1 dependency A/B for build speed | parked | owner s134 | tracker build plan (OWNER-LIST)
- test-binary-merge-hakari | merge test binaries + cargo-hakari | parked | owner s134 | tracker build plan (OWNER-LIST)
- sccache-cap-15gb | sccache cap 15 GB on 18c | parked | — | tracker build plan (OWNER-LIST)
- shared-cargo-target-stale | shared ~/.cargo-target gives stale-artifact failures; per-branch target dirs | decided | 33 errors seen | tracker new items

## harness/bench

- t0-3-slope-cadence-quiet-gate | cadence as LSQ slope over load window; quiet() gates on 5 s CPU busy sample | merged | — | roadmap T0.3, d9512362, 43179dc5
- t0-3-load-refuse-report | refuse to report when load > 1.0 on a validator core; 2 back-to-back runs within 5% | proposed-not-done | PARTLY | roadmap T0.3
- trading-semantics-mix | standard bench mix with stops, modifies, funding, liquidations | in-progress | liquidation stress cells exist; stops/modifies not in mix | roadmap T0.3, perf s72
- order-latency-histogram | end-to-end order latency submit->inclusion->exec->visible | merged | order latency metrics exist; fills visible p50 241 ms, p99 3.7 s (s77) | HL-comparison s2, plans/order-latency-histogram-impl.md
- node-counter-scraper | bench scrapes node /metrics counters, not load-gen x batch (190x inflation) | merged | Phase 0 #32 | mission-s470 frontier #32
- bench-csv-emitter | machine-readable --csv with fixed campaign schema | proposed-not-done | — | mission-s470 frontier #33
- no-live-block-body-sweep | stop fetching full block bodies inside timed window; counter deltas headline | merged | Phase 0 #34/#35/#40 | mission-s470 frontier #34/#35/#40
- loadgen-bin-format-zero-copy | default econ loadgen to --format bin + zero-copy body splice | proposed-not-done | claimed 2.7 -> 1.1 cores | mission-s470 frontier #20
- per-market-funnel-histogram | per-market funnel + per-block matched histogram | proposed-not-done | — | mission-s470 frontier #38
- age-at-exec-histogram | action age-at-exec histogram with clock alignment | proposed-not-done | — | mission-s470 frontier #39
- ingest-admission-reject-counters | gossip/forward/push admission reject counters | proposed-not-done | — | mission-s470 frontier #37
- econ-bench-mode | --econ loadgen: orders sized to funding, cross fraction, cancel fraction | merged | acceptance 0.013% -> 100% | mission-s470 A4 37a0958
- funnel-counters | order-funnel Prometheus counters (placed/resting/matched/rejected...) | merged | resting counter later broken | mission-s470 A1 98c3778
- view-metric-histograms-scrape | scrape the 9 torus_view_* histograms on idle cell to split 38 ms | merged | done in l3verify | l3-diagnosis fix 5
- genesis-cache-book-window-key | port genesis_cache.py BOOK_WINDOW_COMMIT keying to main | proposed-not-done | small build | tracker new items
- detachtest-lock-timeout | DetachTest wait timeout on shared build lock | proposed-not-done | 3 harness test failures | tracker new items
- digest-ckpt-wait | harness digest waits for durable post-drain checkpoint; counter validation | merged | — | 9ea22dbd, eb993e3f
- attest-h-n-check | harness attest key, value-for-value h_n compare, hole wait | merged | 63 ckpts x 3 nodes equal | tracker, 54ffec29, 482803ad
- kill-no-restart-knob | KILL_NO_RESTART + VALIDATOR_STAKES for one-down benches | merged | — | tracker, bc408dab, af53295c
- matched-not-x400-metric | measure matched/placed from node counters, not incl-actions x400 (~190x inflation) | merged | WAL1 chain 2.1 matched/action | val2-baseline-workorder, mission-s470
- max-level-depth-telemetry | telemetry on max level depth + hot mega-level bench cell | proposed-not-done | 1M-order level ~130 ms keccak | design-level-rows-authority s9 risk 1
- cap-sweep-uncapped | sweep native block cap 400 -> uncapped | measured-no-gain | matched/s 2.4k -> 13k but blk/s 8.6 -> 0.7; no cap gives both | re-proof5-context s2 (capsweep-18c-4f832e5)
- level-hash-cache-stats-export | export level-hash cache hit/miss/seed gauges to Prometheus | proposed-not-done | debt from cacheab run | re-proof5-context s4
- resting-funnel-counter-broken | torus_orders_resting funnel counter reads 0; use torus_exec_resting_orders | proposed-not-done | debt | mission-handoff-layer3
- sweep-rawvalue-hashing | hash raw JSON slices (RawValue) in block-body sweep instead of Value.to_string | proposed-not-done | claimed half sweep CPU | mission-s470 frontier #36
- pkg-b-flood-integration-tests | 3-node in-proc bulk-flood test (proposals beat timeout) + 2-node direct-to-leader bounded request test | proposed-not-done | never written | mission-s470 Package B deferred tests

## rpc/streams

- ws-newtrades-userfills | WebSocket newTrades / userFills from exec fills | merged | — | roadmap P4, 7ebcfa37
- ws-l2book-bbo-allmids | WebSocket l2Book, bbo, allMids, orderUpdates, userEvents | queued | OPEN | roadmap P4 (OWNER-LIST)
- hl-info-exchange-shim | HL-compatible /info + /exchange shim | queued | OPEN | roadmap P4 (OWNER-LIST)
- sync-order-responses | synchronous order responses (resting/filled/error) | queued | OPEN | roadmap P4
- rpc-freshness-option-b | eth reads through published view for fresher receipts | proposed-not-done | status unknown | roadmap Tier 5, item7-step3-design q4
- in-memory-getorderbook | in-memory getOrderBook precompile/RPC | queued | before mainnet | roadmap P0 read precompiles, b26bd3b3
- read-precompile-gas-s100 | read precompile gas base 16,400 + per row/word | merged | — | roadmap P0, 00bf7346
- getblockbody-response-cap | lift 10 MiB getBlockBody response cap and kill live-poll melt | merged | Phase 0 #40 env-gated | mission-s470 frontier #40
- rpc-scan-cost-admission-r24 | cost-based read admission / dedicated RPC resources; maintained OI aggregates instead of full scans | proposed-not-done | TORUS_RPC_MAX_SCAN_ROWS 250k cap added | main-chain-deep-review R24, configuration-reference
- sync-submit-action-result-sink | waiting submit (oneshot keyed by action hash) resolved by per-action ActionResultSink from exec | proposed-not-done | HL ~0.2 s sync response | hl-parity findings e2e-latency-path
- bbo-precompile-0x080e | best bid/ask precompile from cf_native_bbo written at block end | merged | 16,440 gas | configuration-reference
- row-aware-book-readers | RPC getOrderBook reads level/order rows (scan.rs row_book_depth); precompile getOrderBook removed | merged | — | design-level-rows-authority s9 risk 4

## other

- t0-2-co-locate-validators | co-locate validators in one region for <50 ms RTT | queued | after testnet | roadmap T0.2 (OWNER-LIST)
- trial-net-wan | trial net archy+cachyoz+18c doubling as WAN test | queued | not started | roadmap (OWNER-LIST)
- target-block-time-assumption | TARGET_BLOCK_TIME_SECS=2 economics windows not enforced; run ~7x faster | proposed-not-done | delist grace uses 2 s | review s4a safety flag
- p3-order-surface | TP/SL, cloid index, batchModify, expiresAfter, scheduleCancel, TWAP | queued | OPEN | roadmap P3 (OWNER-LIST)
- p5-subaccounts-spot-bridge | subaccounts, spot, bridge, multisig, permissionless validators | queued | OPEN | roadmap P5 (OWNER-LIST)
- compiled-in-bootstrap-peers-contamination | devnet fell back to compiled TESTNET_BOOTSTRAP_PEERS and cross-contaminated meshes | merged | fixed in compose; repo fix flagged | sprint-s395-report finding 3
- validators-ntp-clock | validators need NTP-synced clocks (5 s drift bound) | decided | — | parity-audit-fixes block timestamps
