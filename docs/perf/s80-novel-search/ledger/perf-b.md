# Ledger part perf-b: earlier idea-search rounds (s64, s65, s63-s85 docs, 8k, ozarchy summary, bench archive)

Read-only extraction, 2026-10-22, repo main eb993e3f. One line per idea:
`- slug | mechanism | status | evidence | source`.

Source paths are relative to `docs/perf/` in /home/oz/projects/torus-economy/Torus-hyperBFT unless they start with
`mem:` (memory id prefix), a commit sha or a branch. Short forms used:
- `S64` = s64-exploration-2026-09-24.md (sections §1-§6; `#n` = top-10 rank)
- `S65` = s65-explorer-synthesis-2026-09-27.md
- `R2a:<angle>` = s65-explorers/r2_fable_0-15.json, `R2b:<angle>` = s65-explorers/r2_fable_15-30.json
- `KR:<angle>` = s65-explorers/killed-runs-finished.json, `UA:<angle>` = s65-explorers/unfinished-angles.json (angle never run)
- `8KS §n` = 8k-explore-survey.md section n, `8KI Lx.y` = 8k-explore-ideas.md, `8KL` = 8k-explore-log.md
- `OZS` = ozarchy-summary-2026-10-13.md, `BA` = bench-archive/INDEX.md + cells.md (arm means recomputed from cells.md)
- `RD sN` = ozarchy-antispam-item6-pf1-2026-10-04.md section N (cited by the other docs, not read here)

Status words: merged (on main, flag may be off), default-on, rejected (measured worse / premise refuted / ruled out),
tried-inconclusive, measured-no-gain, queued, parked, proposed-not-done, in-progress (owner list / other machine), decided.

## consensus

- header-first-hybrid-pipelining | Header-first proposals, vote before body arrives, early QC view advance (hybrid pipelining option A) | merged | shipped, basis of later work | 8KS §1; plans/hybrid-pipelining*.md
- vote-after-body | Replica votes only once it holds the block body; pre-vote parent check; invalid actions skipped with executed/skipped record | merged | full load +1.7% (r2-r4), low load +19 ms vote delay | s83-s85-campaigns-2026-10-03.md §12; merge 48b5b0f
- commit-then-execute | Execute after commit on its own thread (Hyperliquid model), consensus no longer waits for execution | merged | shipped | 8KS §1; plans/commit-then-execute-impl.md
- two-chain-commit-three-chain-lock | 2-chain commit with 3-chain grandparent lock, model-checked before f=1 | proposed-not-done | open safety work T1.2 | 8KS §1; plans/s442-consensus-findings-and-limitations.md
- re-add-third-chain | Return to 3-chain HotStuff commit | decided | not chosen (roadmap "deliberately NOT") | 8KS §1
- speculative-produce-option-c | Next leader speculatively builds its block during the voting of the current view | decided | not chosen until T1.2; never built; S244 App hook removed | S64 §3; 8KS §1; plans/speculative-pipelining-impl.md
- next-leader-payload-prebuilder | Build selection, DA mirror and attestation for the next own view on a worker thread before entering it | proposed-not-done | est +5-15%, L effort, s64 rank 11 | S64 §5; 8KS §2
- epoch-boundary-fast-path-f | Remove the serial exec barrier at h % 100 epoch boundaries | default-on | native blk/s +88.6%, p50 -73.6%, matched/s -1.3% | 8KS §1 (s84, s90, s128)
- defer-leader-commit-feed | Leader runs on_committed_block feed after its header broadcast plus inline self-vote (TORUS_DEFER_COMMIT_FEED) | measured-no-gain | s66-abc3 57.3k vs ctl 51.8k noisy; default off; avoid list | S64 #1; mem:f6fdcdcf; mem:38e871cb
- broadcast-advanceview-before-feed | Collector broadcasts AdvanceView before process_update_result so commit feed overlaps transit | measured-no-gain | same mechanism as deferred feed, s66 null | s70-qc-split-2026-09-27.md Next; mem:38e871cb
- generalize-feed-deferral-all-nodes | Run feed_committed_blocks_to_app last in every algo-loop iteration on every node, not inline | proposed-not-done | est +1-3% matched/s, low conf | R2a:event-bus
- defer-parent-feed-fix-d | Next leader skips the commit feed inside the parent-body insert, broadcasts its retried proposal first (TORUS_DEFER_PARENT_FEED) | rejected | all flag-on cells failed dissemination gate; with serve thread 3/6 pairs | s72-parent-body-2026-09-28.md; branch perf/s72-defer-parent-feed 678719e
- defer-feed-only-with-exec-room | Fix D variant: defer the feed only when the exec channel has room (App hint to hotstuff) | proposed-not-done | s72 revisit option | s72-parent-body-2026-09-28.md §5
- nonblocking-exec-dispatch | Full exec channel parks the block instead of blocking the algo thread (TORUS_EXEC_NONBLOCKING_DISPATCH, bounded deferred_exec) | merged | off in s72 benches; never A/B'd with fix D | s72-parent-body-2026-09-28.md §5; ee763a20
- collector-local-advance | Collector hands its own PC to its pacemaker instead of the AdvanceView loopback (TORUS_COLLECTOR_LOCAL_ADVANCE) | rejected | qc_to_advance -45..-70 ms, propose_build +55 ms; 68.5k vs 74.3k | s70-qc-split-2026-09-27.md; b20b235; R2a:main-loop
- serve-block-data-off-algo-thread | BlockDataServer thread serves BlockDataRequests from a BlockTreeCamera snapshot (TORUS_BODY_SERVE_THREAD) | default-on | throughput neutral; serve queue 12-18 to 3-4 ms | s72-parent-body-2026-09-28.md §6; f9b013e
- tail-view-algo-queue-backlog | Find what backs up the algo thread inbound queue in tail views (vote queue p90 ~195 ms vs p50 4 ms) | proposed-not-done | medians small, means dominated by tail | s70-qc-split-2026-09-27.md Next
- rejoin-view-sync-round-skip | Adopt an authenticated future-view header (round-skip), parked header re-check, NewView leader skip, stuck-leader direct resend, 1 MiB progress buffer (fixes A-E) | merged | rejoin ready-to-commit 24-36 s to 0.3-0.7 s | S64 #2; s75-rejoin-and-pinning-2026-09-29.md; e12bdac
- resend-tc-timeoutvote-on-reconnect | On validator reconnect re-send latest TimeoutVote or highest TC so fallback_tc adopts it | proposed-not-done | cheaper complement to round-skip | S64 #2(b)
- leader-header-resend-250ms | Leader re-sends its header every 250 ms when dropped by the consensus gossip rate limiter | default-on | fixed lost-proposal timeouts | 8KS §1 (s77, s128)
- hold-light-proposals-rate-limit | Hold light proposals under the consensus gossip rate limit | decided | superseded by 250 ms resend | 8KS §1; mem:bfbd7612
- vote-persist-before-send | Header fast path persists vote state before sending the PhaseVote (safety ordering) | merged | in f05b20e | S64 §4 bug 2; mem:f6fdcdcf
- vote-state-own-db | Move vote-state writes out of the shared RocksDB write group into their own DB (s67) | measured-no-gain | vote save p99 113 to 4 ms, matched/s flat; parked | 8KS §1; mem:cb3604fd; mem:38e871cb
- one-durable-batch-before-vote | Fold lock, leader-reputation and vote-state writes on the header-vote path into one RocksDB write-group join | proposed-not-done | est +1-5% blk/s | R2a:blocktree-io; R2a:qc-verify
- skip-noop-blocktree-writes | Skip empty/no-op block-tree batches (duplicate AdvanceViews, known NewView PCs, lock-only updates) that still join the write group | proposed-not-done | est +2-4% matched/s | R2a:blocktree-io; R2a:qc-verify
- blocktree-singleton-cache | Write-through in-memory cache for hot block-tree singletons (highest_pc, locked_pc, highest_view_voted, local_tip...) plus cheap contains() | proposed-not-done | est -3-4% consensus CPU per view | R2a:blocktree-io; plans/leader-rep-kv-cache-impl.md
- drop-leader-rep-rmw | Remove the unconditional leader-reputation read-modify-write from vote and commit paths while B3 is off | proposed-not-done | est vote_delay 46-50 to ~30 ms | R2a:event-bus
- b3-leader-reputation | Leader reputation (B3) from locally observed behaviour | parked | stalled devnet s339; behind kill switch | S64 §3; 8KS §1
- reputation-from-committed-chain | Derive leader reputation from the committed chain so all nodes agree, re-enable B3 | proposed-not-done | L effort | S64 §5
- skip-redundant-qc-verify | Memo of PhaseCertificates already verified in the validator-set epoch, skip re-verify | merged | b674376a per-view floor shaves | R2a:qc-verify; b674376a
- certificate-aggregation | Replace O(n^2) ed25519 signature-set certificate verifies (AdvanceView re-verify, nested TCs) with aggregation | proposed-not-done | ~0% at n=3, matters at n>=25 | KR:vote-aggregation
- leader-tenure-2-4-views | Keep the proposer that already holds the parent for 2-4 consecutive views | proposed-not-done | est +6-14%, low conf | R2a:leader-rotation
- vote-lane-next-leader | Dispatch queued PhaseVotes ahead of the 4 MB body at the next leader so the QC forms earlier | proposed-not-done | est +3-6% standalone, low conf | R2a:leader-rotation; S65 lead 2
- votes-to-two-next-leaders | Broadcast PhaseVotes to leader(v+1) and leader(v+2) so a stalled next leader does not orphan the view | proposed-not-done | open s64 list | S64 §5; 8KS §1
- weighted-quorum-211-devnet | Stake weights 2/1/1 on the devnet for crash tolerance at zero CPU cost (not Byzantine-safe) | proposed-not-done | VALIDATOR_STAKES harness knob exists | S64 §5; cee86e4b
- fourth-validator-f1 | Fourth equal validator for real f=1 | parked | s442: throughput dropped >4x with 1 of 4 dead; owner decision | S64 §3, §5; 8KS §1
- iwrr-normalize-stake-powers | Normalize stake powers before IWRR leader selection (raw stake/1e18 gives multi-day single-leader runs) | proposed-not-done | 2M/1M/1M gives 1M consecutive views | S64 §4 bug 9
- select-leader-closed-form | O(n log n) closed-form IWRR select_leader instead of O(view) | merged | S405 height-drag root cause | 8KS §1; 5828ec20
- cons-find-26-fallback-tc | Guard returns early on TimeoutVotes from ahead senders, making fallback_tc catch-up unreachable for laggers | proposed-not-done | harmless at n=3, matters n>=4 | S64 §4 bug 3
- header-first-tc-nec-validation | Header-first path validates TC/NEC/reproposal and writes LOCAL_TIP (MonadBFT tail-fork resistance) | in-progress | 1b751f6c cert checks both paths; owner TC-required | S65 bugs; KR:vote-aggregation; 1b751f6c
- timeout-signature-binding | TimeoutVote/TC/NE signatures bind only part of what the protocol acts on; sign the full content (tip signing) | in-progress | owner TC option B + tip signing | KR:vote-aggregation
- unanimous-qc-one-chain-commit | Commit B_h when its own QC carries all n votes plus a vote-lock rule, else 2-chain (L1.1) | proposed-not-done | score 0.05; standard 0, paced p50 -3..-12% | 8KI L1.1
- optimistic-exec-speculative-1qc | Feed exec at the speculative 1-QC commit, keep durability at irrevocable height, roll back on mismatch (L1.2) | proposed-not-done | score 0.03; saving ~5-15 ms; own falsifier fires | 8KI L1.2; UA:optimistic-execution
- propose-defer-on-exec-queue | Defer proposing while the exec queue is deep (TORUS_PROPOSE_EXEC_WATERMARK, TORUS_EXEC_QUEUE_DEPTH) | parked | campaign B dropped s126; branch exp/s123-propose-defer-on-exec-queue-v2 | 8KS §1
- proposer-commit-on-advance-view | Proposer commits on AdvanceView; compact exec-channel handoff | proposed-not-done | carried unverified, s124 r2-r3 rank 13-24 | 8KS §1; mem:8b1039de
- block-tree-pruner | Prune cf_consensus_meta, which grows forever | proposed-not-done | designed S391, build status not recorded | 8KS §7; plans/block-tree-pruner-and-blockspeed-quick-wins.md
- epoch-valset-from-committed-state | Derive epoch validator-set updates from committed, not exec-lagging local staking state | proposed-not-done | prevents valset split at first stake-change epoch | KR:determinism
- header-timestamp-validation | Validate proposer header timestamp at vote time (monotonic, bounded skew), detect validator clock skew | parked | owner M2 timestamp bound decision | KR:timers-clocks; KR:test-gaps; KR:determinism
- validate-block-rejects-fatal-checks | Header/body consistency, action cap and parent-hash checks enforced in validate_block, not only as exec-time fail-stops | proposed-not-done | one faulty leader can halt all validators | KR:test-gaps
- stateright-calls-production-code | Stateright safety model drives production commit/lock rules, unit tests for block_to_commit / extends_locked_pc_block | proposed-not-done | model re-implements rules | KR:test-gaps; UA:model-checking
- multi-replica-inprocess-test | In-process multi-replica test with real TorusApp and Mempool (produce, compact, reconstruct, commit, exec) | proposed-not-done | only stub-app test exists | KR:test-gaps
- power-loss-view-entered-fsync | fsync highest_view_entered before proposing so a power loss cannot cause a re-proposal in one view | proposed-not-done | needs fsync cost measurement | s75-rejoin-and-pinning-2026-09-29.md Open
- tc-recover-double-propose | TC RECOVER path can propose twice in one view (pre-existing bug) | proposed-not-done | found in fix-D review | s75-rejoin-and-pinning-2026-09-29.md Open
- multi-block-views | More than one block per view, sub-blocks, pipelined proposals, proposer batching | proposed-not-done | angle never run | UA:multi-block-views
- protocol-alternatives | Adopt pieces of HotStuff-2, Jolteon 2-chain, Narwhal/Bullshark, Mysticeti, Autobahn incrementally | proposed-not-done | angle never run | UA:protocol-alternatives
- validator-scaling-3-to-20 | Study message complexity, cert size, body fan-out and leader bandwidth from 3 to 20 validators | proposed-not-done | angle never run | UA:validator-scaling
- tier5-cadence-p99-attribution | Attribute ~310 ms cadence, ~1.2 s p99 floor and in-window p99 jump (T5.1-T5.3) | in-progress | owner list T5.3; T5.1/T5.2 after testnet | 8KS §1; roadmap T5
- port-insert-and-update | Single-batch propose writes: block-tree insert and update in one RocksDB batch (fd1fbba insert_and_update) | proposed-not-done | -20-25 ms on perf/matched-200k, never ported | S64 §3, §5; 8KS §2; fd1fbba
- consensus-determinism-fix | Consensus determinism fix branch (fix/consensus-determinism) | merged | +2.6% matched/s, -1.8% CPU n.s. | s83-s85 §10; 91cd3f1
- three-validator-liveness-fix | 3-validator liveness fix (fix/liveness-3val) | merged | -1.4% matched/s n.s. | s83-s85 §10; 51e3096
- proposer-body-backlog-watermark | Proposer defers on consensus-side body backlog (pending_headers + deferred_bodies), TORUS_PROPOSER_EXEC_WATERMARK | merged | off by default; not the exec-queue lever | s77-s80 §4

## pacemaker/timeouts

- timeout-base-1200 | Raise pacemaker timeout_base_ms 500 to 1200 in genesis and serde default | default-on | dead views -85%, matched/s +7%, 5/5 ABBA pairs p~0.03 | s68-timeout-base-ab-2026-09-27.md; 5393caf3
- timeout-base-1500-cap100 | Raise timeout base 500 to 1500 at cap 100 (s55) | measured-no-gain | views/block 1.50 to 1.07, matched/s within +-40% noise | S64 §3
- pacemaker-deadline-rebase-when-behind | Rebase entered-view deadline when behind / entry-relative or progress-relative deadlines (period-2 limit cycle, absolute epoch grid) | rejected | s68: only 2-5 of ~590 views entered past deadline | S65 top lead; R2a:steady-timeouts; R2a:leader-rotation; R2a:qc-verify; R2a:futex-waits; R2a:main-loop; R2b:correctness-perf-bugs; KR:timers-clocks; s68-timeout-base-ab-2026-09-27.md
- load-keyed-first-view-deadline | Lengthen first-view deadline with load | rejected | s55 null and s68 refutation apply | S64 §5
- budget-aware-proposing | Leader with less view budget than build+finalize+vote does not build a doomed 200-action block (hand-off or partial block) | proposed-not-done | est +1-3%, low conf | R2a:steady-timeouts; R2a:qc-verify
- view-timeout-telemetry | Log/histogram view budget, slack, timeout cause classes, uncertified own proposals | merged | classes after_vote/leader/no_proposal measured | R2a:steady-timeouts; R2b:correctness-perf-bugs; f37b7d5f
- clamp-deadline-timeout-max-ms | Read genesis timeout_max_ms (dead config, 30000) to clamp backed-off view deadlines | proposed-not-done | timeout_max_ms still only in torus-genesis | S64 §4 bug 4, §5
- align-commit-lag-backoff-cap | Harness runs TORUS_COMMIT_LAG_BACKOFF_CAP=8 vs genesis 0; align bench with production pacemaker | proposed-not-done | config deviation, 0% direct | S65; R2b:correctness-perf-bugs
- adaptive-multiplicative-backoff | Adaptive multiplicative pacemaker backoff (option C) | proposed-not-done | designed, status not recorded | 8KS §1; plans/pacemaker-backoff-*.md

## dissemination/DA

- compact-block-hash-proposals | Hash-only proposals (CompactBlock) with mempool reconstruction | merged | shipped | 8KS §2; plans/mempool-gossip-hash-proposals-impl.md
- direct-to-leader-forwarding | Forward submitted actions directly to the leader; B1 batched forward envelope | merged | shipped | 8KS §2; 56ddd74f
- native-da-store-push-pull | Native-action DA store with push and by-hash pull; big-body fix, push hardening | merged | shipped | 8KS §3
- push-manifest-threshold-512kb | Above 512 KB push a hash manifest instead of full bodies, followers pull misses | default-on | full-body push 10.1 to 0.12 GB/cell, CPU -5.9%, matched +4.3% | 8KS §0, §3 (s86)
- hash-only-push-with-gossip | Skip full-body pre-proposal push when gossip already delivered bodies (TORUS_HASH_ONLY_PUSH_THRESHOLD) | merged | subsumed by 512 KB manifest default | S64 §5; R2a:msg-encoding; R2a:network-loop
- hash-only-push-s387 | Hash-only pre-proposal push where every validator pulls every body | rejected | S387 failed; Fix C pulls only missing | S64 §3
- gossip-prespread-sprint3 | Gossip pre-spread of native actions to all validators | default-on | S458 gossip-off canary wedged testnet | 8KS §3
- prespread-over-direct-zstd | Native pre-spread over the direct zstd protocol instead of Signed+Strict gossipsub | proposed-not-done | CPU-s/1M -3.2..-4.4%, matched ~0 | 8KS §3; mem:dabcab1c
- zstd-body-paths | zstd compression on /torus/*/2.0 body protocols | default-on | shipped | 8KS §3; 6cbda397
- erasure-coded-dissemination | Reed-Solomon k-of-n body dissemination (T3.1) | parked | owner T3.1 follow-ups; scaffold b29d843 | 8KS §3
- shard-custody | Persist erasure shards on every node at validate | merged | default off since s128: -21.6% matched/s, 140 ms/validate | S64 §4 bug 11; 8KS §3
- lazy-serve-time-shards | Derive erasure shards on serve instead of eager custody on the consensus thread | rejected | rejected as perf, bench pins custody 0 | R2b:erasure-da-prod; KR:erasure-da-prod; mem:e7087388
- shard-merkle-once-per-body | Build shard Merkle tree once (O(1) proofs), fuse custody into body mirror | proposed-not-done | -75 ms/block only when custody on | R2b:erasure-da-prod; KR:erasure-da-prod
- custody-only-when-adds-source | Production custody default matches the bench at small validator sets | decided | custody default off (s128) | R2b:erasure-da-prod; 7b9ebf7a
- erasure-recovery-early-end | Shard recovery pre-step counts present=false replies, re-checks DA store each tick | proposed-not-done | -0.5 s per recovery cycle | KR:erasure-da-prod
- da-reconstruct-offthread | Move DA reconstruct off the consensus thread (BS-4a/BS-4b) | proposed-not-done | plan written; BS-4b mid-budget re-fetch merged 1f206f1e | 8KS §3; plans/bs4a-da-reconstruct-offthread*.md
- da-flush-on-miss | DA batch read flushes the ingress mirror buffer only on a miss | merged | reconstruct mean 47-54 to 20-23 ms; throughput no claim | s76-flush-on-miss-2026-09-29.md; bce54d0
- leader-body-push-with-header | Leader pushes the body with its header (TORUS_BODY_PUSH_MAX_BYTES=65536) | measured-no-gain | +7% idle, neutral saturated; low-load view -10%; off | S64 #8, §3; s83-s85 §12
- inline-body-small-blocks | Env-gated inline body for small blocks (TORUS_INLINE_BODY_MAX_BYTES) | merged | default 0; avoid list "body push/inline body" | 0c3ea780; mem:38e871cb
- follower-body-request-before-vote-persist | Issue BlockDataRequest before persisting/sending the vote on the header fast path | decided | moot after vote-after-body | R2a:main-loop; S65 lead 1
- follower-reconstruct-pool-first | Follower reconstructs bodies from the pool by hash_index first, DA store only for misses; or attest over raw bytes | proposed-not-done | est da_reconstruct 39-46 to 5-15 ms | S64 #10; 8KS §3
- async-validate | Move validate_block and DA reconstruction off the HotStuff thread (TORUS_ASYNC_VALIDATE) | parked | never benched on; predicted hit rate <30% | S64 §3; 8KS §5; 4076355
- fix-async-validate-race | Add in-flight wait and hit/miss counters to async validate before any flag-on A/B | proposed-not-done | double-counts validate histograms | S64 §5
- body-fetch-wallclock-budget | Body fetch slow phase every 500 ms for 4 s, MissingData validation retry instead of parking | merged | cost neutral, benefit not proven live | s63-builds-and-body-fetch-2026-09-23.md; ac8782c
- body-fetch-rotate-stalled-proposer | Rotate body fetch off a stalled proposer to another validator after 200 ms | merged | 59fabcf8 | R2b:sync-catchup; 59fabcf8
- dedup-body-fetches | Dedup body fetches across the three fetch paths, adapt first retry above loaded p90 RTT | proposed-not-done | 61% extra requests, 38% duplicate 3.9 MB responses | R2a:futex-waits; R2a:network-loop; R2b:sync-catchup; R2b:correctness-perf-bugs; S65 lead 3
- dedup-pushed-bodies-at-ingest | Followers skip re-clone, re-mirror, re-verify of pushed bodies already in pool | proposed-not-done | ~1.5 GB/cell redundant writes per follower | R2a:mempool-structures; KR:gossip-reconciliation; KR:gossip-reverify(opus)
- no-relay-author-direct-gossip | Ignore relay of native gossip when source is the author validator and all subscribers are validators (L4.1) | queued | owner WAN list; P -0.7..-1.5%, M 0 | 8KI L4.1; KR:gossip-reconciliation
- prefilled-preproposal-manifest | Flush gossip batch before the push; inline likely-missing bodies in the manifest (L4.3) | queued | owner WAN list; archy 0 | 8KI L4.3
- bbr-quic-windows | Patch libp2p-quic for quinn BBR and BDP-sized windows (L4.4) | queued | owner WAN list; archy 0 | 8KI L4.4
- mesh-watchdog-resubscribe-fix | Mesh watchdog, explicit gossipsub peers, stale-connection kick on resubscribe wedge | merged | wedge recovers <1 s; KILL gap -1.1 s n.s. | 8KS §3; OZS s58; e3bb7cca
- sentries-priority-lane | Sentry nodes, validator-only mesh, consensus priority lane (Hyperliquid style) | proposed-not-done | HL comparison list | 8KS §3; hyperliquid-comparison-2026-09-29.md
- colocate-validators | Co-locate validators to cut inter-validator RTT | parked | owner: after testnet | 8KS §3
- block-store-unbounded | Evict or stop writing the swarm block_store and pending_bodies (two 4 MB copies per own proposal kept forever) | proposed-not-done | ~1.4 GB/day; -0.9 GB RSS/node/300 s | S64 §4 bug 7; R2b:sync-catchup
- dedicated-block-data-protocol | Serve bodies over a dedicated /torus/block-data protocol from the network thread | rejected | bypassed May 2026, "silently fails"; serve thread used instead | S64 §3, §5
- da-mirror-after-signature | Verify native-action signatures before durably mirroring gossip bodies; bound inbound channel; penalize peers | proposed-not-done | unauthenticated disk growth ~36 GB/h | KR:gossip-reconciliation; KR:dos-vectors
- rh4-da-body-gc | DA body GC worker with prune-by-default retention N=100,000 blocks | merged | GC 1.5-2.1 ms/native block | S64 §4 bug 8; 8KS §7; 0467f67d
- dag-dissemination-narwhal | Narwhal-style worker batches with availability certificates mapped onto native DA | proposed-not-done | angle never run | UA:dag-dissemination
- partition-loss-slow-validator | Behaviour under partition, packet loss or one slow validator | proposed-not-done | angle never run | UA:partition-loss; S65 counts
- direct-consensus-fan | Direct consensus fan-out instead of gossip (TORUS_CONSENSUS_DIRECT_FAN=1, +GOSSIP_MIRROR=0) | merged | opt-in off; fixed s58 timeouts n=1 | 8KS §1; R2a:network-loop
- trial-testnet-wan | Trial net archy + cachyoz + 18c to measure WAN dissemination and RTT | in-progress | owner list trial net | 8KS §3; owner-list.md
- proposer-custody-on-worker | Move proposer shard custody onto the pre-proposal worker thread | decided | moot: custody default off | S64 §5

## ingress/mempool

- placeorderbatch | One signature per batch of up to 1,024 orders | merged | ~38x | 8KS §2
- batch-submit-local-submitters | Batch-submit RPC and local submitters (Sprint 2 ingress spread) | merged | shipped | 8KS §10
- binary-ingress-verify-rayon | Binary ingress format and rayon ingress verify | merged | shipped sprint 5 / s351 | 8KS §6
- sprint1-rails | Byte cap, mempool TTL, size-aware sync pull | merged | shipped | 8KS §2
- pool-full-preverify-shed | Shed submissions before verify when the pool is full (65,536) | merged | never fires: nonce expiry keeps pool ~15k | S64 #6; b5816146
- capacity-keyed-shed | Node-side shed at pool > K x block cap for non-cancels | proposed-not-done | 37% of acked actions expire after verify+gossip+DA | S64 #6
- admission-horizon-20s | Mempool admission limit keyed to a 20 s horizon (TORUS_ADMISSION_HORIZON_MS) | default-on | expired 17-18% vs 29-50%, throughput neutral | mem:ee9d1d82; f2122d1
- exec-aware-admission | Admission keyed to exec backlog (TORUS_ADMISSION_EXEC_AWARE) | rejected | 2-5 s horizons -33/-51%, 20 s inert; opt-in | 8KS §5 (s73)
- arrival-order-selection | Mempool SortKey (prio, seq, sender, nonce) to fix sender-address starvation | rejected | stall/flood, 3.4-4.5k matched/s; reverted | S64 §4 bug 5; mem:c4912130; mem:611238f9
- rotate-selection-cursor | Rotate the selection start cursor over senders to remove low-address bias | proposed-not-done | Spearman(address, margin) -0.54..-0.78 | S64 §4 bug 5
- cancel-share-oldest-first | Native pool 25% cancel share filled oldest-first, each sender's cancels in nonce order | merged | soak expiries 0 vs 16,168/node | OZS s53c; 3f54704e
- native-antispam | Admission limit, cancel share C 25%, B throttle, per-address rate buffer, per-IP RPC weight limit | merged | no-regression A/B; spam rounds 1-3 | 8KS §10; BA as-r1..r3; 92a02ed6
- incremental-pool-ordering | Incremental native-pool ordering, no full sort per produce_block (T2.3) | proposed-not-done | status not recorded | 8KS §2
- single-pass-ingest | Encode and hash each action once at RPC/gossip ingress, carry hash and bytes into pool, DA and ack; drop deep clones | proposed-not-done | ~0.9-1.0 ingest cores; dup action hash <1% (s82) | R2a:rpc-ingress; R2a:mempool-structures; R2b:fees-accounting; s77-s80 Open
- hash-dedup-typehash-trustkey | Hash batch-item typehash once; one trust-cache key per gossip ingest | merged | gossip-verify CPU -25%, ingress -19% | s77-s80-trade-history-and-streams-2026-09-30.md §2; 0ff946e
- restash-key-in-pool-entry | Store EIP-712 trust-cache key in the pool entry so remove_committed restash is a lookup | proposed-not-done | remove_committed ~30 ms/commit; status not recorded | S64 #4; 8KS §6
- restash-lock-hygiene | One write-lock per restash batch, ingress-side DA-mirror flushing, selection prelude timers | proposed-not-done | est +0.5-2% matched/s | R2a:futex-waits
- order-charged-proposer-budget | Charge proposer block budget by orders, not actions, for small actions | proposed-not-done | s124 r3 rank 34 | 8KS §2; mem:9769f127
- dup-inclusion-inflight-ledger | In-flight hash ledger fixes the duplicate-inclusion tail | merged | shipped | 8KS §2
- batch-ingress-da-write | Batch the ingress DA mirror write (T2.2) | merged | b674376a batch DA mirror | 8KS §10; b674376a
- skip-leader-da-remirror | Proposer writes only DA bodies not yet durable, reuses pool hashes in PendingProposal | measured-no-gain | block_build -30%, matched 67.7k vs 67.1k | S64 #5; s68-block-build-phases-2026-09-26.md; cc81630
- select-phase-split | Split select_block_payload timing into pool-lock wait vs selection work | proposed-not-done | select 39% of block_build | s68-block-build-phases-2026-09-26.md
- ingress-market-spec-cache | Cache market specs at ingress | proposed-not-done | CPU only -4% | 8KS §6; mem:bfbd7612
- gossip-session-action-checks | Gossip ingest of session actions enforces scope, requires_eip712 and expiry like RPC ingress | proposed-not-done | peer can fill cap with doomed actions | KR:session-keys
- d12-nonce-window | Time-major nonce key, prune nonces below block_ms - 60 s every block (bounded replay guard) | merged | b7b5bd04 | R2b:fees-accounting; KR:timers-clocks; b7b5bd04
- non-validator-request-rate-limit | Rate-limit BlockDataRequest/ProposalRequest/NewView from non-validator peers | proposed-not-done | 50,000x amplification on consensus thread | KR:dos-vectors
- origin-attested-gossip | Stop re-verifying gossiped actions on every validator; origin attestation plus pre-exec verify for committed only | proposed-not-done | ~1.2 cores freed on non-RPC validators | R2b:graph-hubs
- sharded-sender-verification | Each action verified by 2f+1 designated validators with QC-certified claimed senders | proposed-not-done | -2.5 cores host-wide est | KR:gossip-reverify(opus)
- fee-priority-admission | Fee-based prioritisation and anti-spam economics under overload | proposed-not-done | angle never run | UA:fee-priority
- batch-size-economics | Re-size PlaceOrderBatch (400) for throughput vs latency vs block packing | proposed-not-done | angle never run | UA:batch-size-economics
- ingress-view-skip-position-slices | Ingress view skips position slices to save RSS | proposed-not-done | RSS hygiene | 8KS §9; mem:e7087388
- cancel-prefix-starvation | Junk or unauthenticated CancelOrders take the cancels-first block prefix; cap and fund-check cancels | merged | antispam C share 25%, cancel-spam rounds 1-3 | KR:economic-attacks; KR:dos-vectors; BA as-r2, as-r3
- dedicated-gossip-verify-pool | Parallel gossip-ingest verify on dedicated gossip-verify threads (T2.1) | merged | threads exist since s16 | 8KS §6

## signature/crypto

- exec-hashing-8g | Absorb canonical bytes once for sort and trust-cache keys, skip all-hit par_recover, cache EIP-712 domain | merged | exec CPU/1k fills -4.5%, blk/s +39% | 8KS §6; 70993fa9
- k1-trust-key-dispatch-hash | Key the exec trust cache by the dispatch action hash carried with the block; exec pre-pass becomes lookup | proposed-not-done | needs fork-safety review; exec verify 3.68 ms/block | 8KS §6; R2a:gossip-reverify; R2b:graph-hubs; KR:fees-accounting(opus)
- k3-one-absorb-both-keys | One Keccak absorb for sort key and trust-cache key | proposed-not-done | one-absorb not built | 8KS §6
- exec-action-keys-parallel | Compute exec action keys in parallel (TORUS_EXEC_PARALLEL_KEYS) | measured-no-gain | +1.03% within noise, CPU +2.1%; opt-in | 8KS §6
- double-verify-trust-cache | Trust cache so exec skips re-verifying ingress-verified actions | merged | shipped | 8KS §6
- dispatch-hash-reuse | Reuse CompactBlock/PendingProposal action hashes in dispatch instead of compute_action_hash on hotstuff | proposed-not-done | hotstuff Keccak 11.2-12.5 ms/native block | S64 #4; 8KS §0, §6
- eip712-item-hash-memo | Per-thread 3-level memo of PlaceOrderItem sponge states keyed on preimage prefixes (L5.1) | proposed-not-done | score 0.47; P -2..-6.5%, M ~0 | 8KI L5.1
- known-key-ecdsa-tables | Verify against claimed sender's cached key with per-signer comb tables instead of ecrecover (L5.2) | proposed-not-done | score 0.07; fork risk via accept-set | 8KI L5.2
- libsecp-ecrecover | libsecp256k1 backend for ecrecover | proposed-not-done | CPU only, recovery 0.41-0.43 cores/node | 8KS §6; mem:e7087388
- session-signed-low-cpu-path | Stateless session-signature trust cache; v2 session digest over packed bytes (L5.3) | proposed-not-done | 0 on standard load | 8KI L5.3
- placeorderbatch-v2-bytes | EIP-712 `bytes orders` over canonical packed encoding instead of 400 per-order struct hashes | proposed-not-done | est 2.2-3.2 cores freed; consensus/client change | KR:gossip-reverify(opus)
- eip712-keccak-x4 | Multi-buffer x4 Keccak for batch item hashing | proposed-not-done | CPU-s/1M -3.9..-4.9%, needs >=2x microbench | 8KS §6; mem:e7087388
- asm-keccak-workspace | asm Keccak backend workspace-wide | merged | 82523da5 | 82523da5
- asm-keccak-level-hash-cache | Make the level-hash sponge cache use asm Keccak instead of pure-Rust sha3 | proposed-not-done | est ~9 ms/block on save_books tail | R2b:build-config
- hoist-eip712-typehash-domain | Hoist EIP-712 typehashes and domain separator out of per-order loop | merged | s77 typehash once + 8g domain cache | S64 §5; R2a:gossip-reverify
- eip712-encode-in-place | Encode EIP-712 struct fields in place on verify pools | proposed-not-done | -0.6..-0.7 core host-wide est | R2a:gossip-reverify
- proposer-attestation-batch-verify | Proposer attestation with ed25519 batch verify | proposed-not-done | plan only | 8KS §6; plans/proposer-attestation-impl.md
- attest-over-action-hashes | Attest over action hashes instead of re-serializing ~4-5 MB bodies | proposed-not-done | -10 ms/validate est; consensus-format change | S64 §5; 8KS §6
- ingress-crypto-microopt | Per-action crypto micro-optimisation at ingress | decided | not chosen: inflation was CPU starvation | 8KS §6; plans/ingress-cpu-supply.md
- session-action-parallel-verify | Session actions join the parallel Phase-1 verify (struct hash + ed25519 batch) | proposed-not-done | session verify 65-70 to 8-10 ms/block est | KR:session-keys
- quic-aes128gcm | Prefer AES-128-GCM in QUIC | rejected | -0.4..-0.6% CPU, below resolution | 8KS §3; mem:8b1039de
- skip-allhit-rayon-verify | Skip the rayon recover phase when the trust cache hits 100% | merged | part of 8g | S64 §5; 70993fa9
- exec-carried-verified-senders | Carry verified senders from ingress into exec | decided | folded into KP / 8g | 8KS §6; mem:bfbd7612
- seed-follower-compact-oncelock | Seed followers' cached_matches_compact OnceLock so commit does not stream another Keccak pass | proposed-not-done | part of s64 #4, status not recorded | S64 #4

## execution/matching

- exec-pipeline | Flush worker 1-deep with applied-height fence, resident books (TORUS_EXEC_PIPELINE) | merged | +15.6% matched/s, disjoint ranges | s63-builds-and-body-fetch-2026-09-23.md Step 4; 8KS §5
- batched-cancel-all | Batched cancel-all book pass (TORUS_CANCEL_BATCH) | merged | phase1 257-301 to 103-108 ms | s63-builds-and-body-fetch-2026-09-23.md Step 4; 58c9eb4
- parallel-cancel-all-per-market | Run cancel_all_many per market in parallel (PC) | default-on | 8 workers, +4.51% | S64 #7; 8KS §4
- p2-1-cancel-all-index | Trader to markets index so cancel-all visits only the sender's books | merged | gate missed, accepted | 8KS §4; 90632a75
- lazy-delete-level-queues | Tombstone cancels in level queues: O(targets) instead of O(depth) memmove | proposed-not-done | phase1 19 to 216 ms/block across a cell | R2a:matching-inner-loop; R2b:memory-growth
- seq-inline-level-queues | Carry seq inside Order or a lockstep VecDeque so partition_point avoids HashMap probes (L2.3) | proposed-not-done | score 0.05; M +0.2..+0.6% | 8KI L2.3; R2a:matching-inner-loop
- mode3-chunk-width | Retune LEVEL_CHUNK_SEQS 64 to 512-1024; fix chunked_top O(all chunks) | parked | mode 3 not chosen | R2a:matching-inner-loop; R2b:memory-growth
- deep-cancel-compaction-thresholds | Deep-compaction threshold variants for cancel-all | rejected | -6% | S64 §3
- cancel-all-by-level-two-ended | Cancel-all grouped by price level, two-ended cancel, integer-leverage cancel margin | proposed-not-done | s61 candidates unmeasured | 8KS §4
- p2-5-foldhash | foldhash for exec maps | merged | gate met; 187.2k vs 180.7k | 8KS §4; BA p25g
- s82-ahash | Seeded ahash on exec-path maps | parked | ~3% node CPU, matched +0.4% p=0.91 | s82-exec-hasher-2026-09-30.md
- dense-trader-slots | Intern addresses per market job into Vec slots (L2.4) | rejected | dead: s82 hasher precedent | 8KI L2.4
- p2-3-persistent-worker-pool | Persistent rayon exec pool replaces per-block thread::scope spawns | merged | spawns 41 to 0/block; throughput within noise | 8KS §4; 56914dd1
- match-setup-scatter-stitch | Parallel match setup (M), deferred pass-B results scatter (D), sell top-up stitch list (T) | default-on | +9.1% together | 8KS §4
- parallel-engine | Sender-sharded parallel Phase-2 prepare plus settle (TORUS_PARALLEL_ENGINE) | measured-no-gain | -1.4% matched/s, +2.4% CPU/fill, +15.7% blk/s; opt-in | 8KS §4; R2a:risk-margin; faa50ef8
- exec-pool-fine-grain-jobs | Fine-grain exec pool jobs (G) | parked | s78.6 | 8KS §4
- pool-wait-cuts | Fewer chunks, worker caps, merged round trips for match/settle/cancel-all pool waits (P2-P4) | proposed-not-done | upper bounds 4.0-4.3 / 2.1-2.3 / 1.5-2.1 ms | 8KS §4
- match-settle-worker-caps | MATCH / SETTLE worker caps on the 32-thread host | proposed-not-done | never A/B'd on archy | 8KS §4; mem:38e871cb
- cloid-engine-cost-fixes | Cross-market cloid index, one lookup per duplicate check | merged | +5.6% at cloid share 100 | OZS s63; cf2861ad
- parallel-cloid-upkeep | Parallel sharded cloid upkeep | proposed-not-done | +4% at cloid 100, 0 at 0 | 8KS §4; mem:e7087388
- match-epilogue-trader-index | Batched trader index in match epilogue | proposed-not-done | ~+1.2%, s124 r3 rank 45 | 8KS §4; mem:8b1039de
- pass-a-hot-market-shards | Pass-A hot-market trader shards | proposed-not-done | carried unverified | 8KS §4; mem:8b1039de
- open-order-limit-no-spawn | Per-user open-order limit with count computed without per-count spawned threads | merged | ol -5.2%, olfix +4.1% closes gap | s83-s85 §10-12; ded6e6c
- stored-open-order-counter | Phase 4 step 5 stored open-order counter | measured-no-gain | dropped | 8KS §4
- item6-phase1 | Resident rows, margin sums, liquidation L1, C1-C7, PF1, M1, B-blind | merged | Gate 2 met s19 | 8KS §4; BA sections 3-19
- item6-phase4-one-record-per-trader | One state record per trader (Phase 4 steps 1-4) | merged | +3.0% at 300 markets, -2.75% at 10 | 8KS §4
- item6-phase5-step2 | Users in place, write-through, dirty-set drain (typed write-back) | rejected | exec CPU/fill +5%, blk/s 10.8 to 5.2; Phase 5 parked | 8KS §4 (s91)
- item6-phase5-step3 | Book rows from a dirty set | parked | branch feat/item6-phase5-step3, no 10k run | 8KS §4
- attestation-window-typed-writeback | Per-block response hash + window digest with typed write-back layer (L6.2) | rejected | dead: premise false, s91 loss | 8KI L6.2
- exec-prologue-sort-key-ties | Hash sort keys only for (category, sender) ties; stop deep-cloning bodies to sort | merged | 8g sort 5.40 to 0.42 ms/block | KR:fees-accounting(opus); KR:evm-overhead(opus)
- zero-rehash-exec-frontend | Carry ingress trust key and sort digest into CommittedBlockMsg, move not clone | merged | partly via 8g; trust key part = K1 open | R2b:exec-pipeline; R2b:fees-accounting
- lookahead-verify-stage | Dedicated thread computes trust keys, sort hashes, miss recovers one block ahead (L6.3) | proposed-not-done | score 0.33; only full-channel windows | 8KI L6.3
- overlap-drain-next-frontend | Overlap block N book drain with block N+1 verify/sort/overlay build on the exec thread | proposed-not-done | est +1-4% | R2b:exec-pipeline
- end-resident-on-worker | end_resident on a worker, begin_resident before new_env | merged | s15, s18 | 8KS §5; BA 4acdc59
- fixedpoint-i128-fast-path | i128 fast path in FixedPoint checked_mul/checked_div, i256 fallback | merged | b809d439 | R2a:risk-margin; b809d439
- oracle-off | Disable oracle processing | decided | +4.0% matched/s; oracle stays on (product) | 8KS §4 (s81)
- modifyorder-ownership | Ownership check on ModifyOrder | merged | 710dde7 | S65 bugs; KR:order-amend
- modifyorder-price-change-matches | ModifyOrder with a price change re-enters matching (PostOnly/STP/reduce-only) | proposed-not-done | crossed books possible | KR:order-amend
- batched-cancel-modify | Batched cancel/modify actions for market-maker flow | proposed-not-done | cancel-heavy flow ~400x slower than place | KR:order-amend
- cancel-by-cloid | u128 cloids, cross-market uniqueness, CancelByCloid | merged | 2ab09577 | KR:order-amend; 2ab09577
- stop-order-scan-and-fills | Fix O(S) pending-stop rescan per fill, dropped triggered-stop fills, stop id collision, zero-margin stops | proposed-not-done | fix status not verified | S65 bugs; KR:economic-attacks; KR:oracle-funding
- per-market-parallel-exec | Deterministic per-market partitioning of the still-serial parts of execution | proposed-not-done | angle never run | UA:per-market-exec
- frequent-batch-auctions | Batch auction per block per market instead of continuous matching | proposed-not-done | angle never run | UA:matching-design
- block-stm-parallel-evm | Block-STM / parallel EVM (T4.4) | parked | owner: after testnet | 8KS §4
- evm-option0-fast-path | EVM fast path with block overlay (item 7 option 0) | default-on | 1e62f38f; owner item 7 follow-ups | 8KS §11; 1e62f38f
- evm-lanes-option-s | EVM lanes: larger cadence N (option S) | rejected | stopped after r1, EVM pool filled; superseded by option 0 | 8KS §11
- book-first-then-evm | Order book first then EVM; time-based CoreWriter delay | proposed-not-done | HL comparison | 8KS §11
- level-queue-16byte-slab | 16-byte level-queue entries in a slab; compact per-order book footprint | proposed-not-done | exec-ceiling lever, low conf | S64 §5
- p2-2-batch-cache-flush | Batch the exec cache flush into write_cf_raw_many | rejected | +2.85 ms regression, reverted; p22 188.3k vs p25 190.3k | 8KS §4; BA p22g; mem:51613142
- position-v2-savings | C2 holder-index fix plus Position v2 record savings | tried-inconclusive | sav 163.5k vs base 175.9k; both 179.1k | BA c2h
- evm-lanes-step1-2 | EVM lanes: budget and cadence N (step 1), HL small/big EVM blocks (step 2) | merged | step 2 native A/B passed (Q7) | 8KS §11; 853fed28
- deep-book-topn-precompile | Deep-book storage and topn precompile branches | tried-inconclusive | WAN results unverified; topn regression inherited | 8KS §8; testnet/p3-testnet/RESULTS.md

## settlement/margin/liquidation

- l2-2-no-merge-position-flush | Keep per-market PositionCaches, flush dirty rows in global key order without merge_disjoint (TORUS_SETTLE_NO_MERGE) | default-on | exec CPU/1k fills 0.957, blk/s 1.095 | 8KI L2.2; 8KL; dbe14b21; be65552e
- l2-1-streamed-settle | Fused match+plan jobs publish per market, exec thread runs pass B in market order meanwhile | in-progress | owner batch 10; score 0.37 | 8KI L2.1
- parallel-settle-c3 | Deterministic parallel Phase-4 settlement (TORUS_PARALLEL_SETTLE) | merged | default off | S64 §5; 4c4e8b9c
- net-balance-per-trader | Settle per trader: net pass-B balance events and margin releases into one RMW per trader | proposed-not-done | est pass B -12..-35 ms/block | KR:risk-margin(opus); KR:settlement-writes(opus); KR:fees-accounting(opus)
- margin-reserve-running-balance | Fold a PlaceOrderBatch's 400 margin reserves against one local running balance per sender-run | proposed-not-done | est -9.5..-11.5 ms/block | R2a:risk-margin; KR:risk-margin(opus)
- maker-remaining-qty-report | Matching engine reports maker remaining qty so margin release needs no BTreeMap/book lookup | proposed-not-done | est -23..-26 ms CPU/block | R2a:risk-margin
- o1-margin-balance-cache | Margin write-back balance cache (O1) | decided | superseded by item 6 resident rows | 8KS §4; plans/o1-margin-cache.md
- adl-budget-dirty-check | ADL per-block budget plus dirty check | merged | shipped | 8KS §4; BA adlcells
- adl-speedups | ADL rank sort, balance reads, record search speedups | parked | 18c s99: not now | 8KS §4
- liq-touched-accounts-first | Liquidation checks accounts touched this block first | proposed-not-done | P1 follow-up | 8KS §4
- liq-scan-parallel-prevalue | Parallel pre-valuation for the liquidation scan | rejected | refuted s123 | 8KS §4; mem:85a53ffe
- liq-cadence-invariant-scan | Liquidation budget = rate x header time delta instead of 2,048 per block (L1.3) | proposed-not-done | score 0.11; ~1% of exec | 8KI L1.3
- risk-snapshot-engine | Per-trader risk snapshot updated from settlement deltas instead of prefix scans per trader per market | proposed-not-done | naive wiring costs 0.7-5 s/block | KR:risk-margin(opus); KR:oracle-funding; KR:liquidations
- liq-correctness-set | Per-market oracle price in force-close, cross-margin net before ADL, socialize_loss, deterministic market order | proposed-not-done | fix status not verified | KR:liquidations; KR:determinism
- oracle-aggregate-every-block | Aggregate every listed market's oracle at the start of each block (risk loop wired) | merged | 454a9bba | KR:oracle-funding; 454a9bba
- oracle-collect-bounded | Bound OracleManager collect_submissions prefix scan and prune | proposed-not-done | 285k keys/day growth | KR:oracle-funding
- fees-per-trader-per-block | Accrue trading fees per trader per block, not per fill | proposed-not-done | owner P2 fees after testnet | R2b:fees-accounting
- portfolio-margin-scaling | Incremental cross-market margin as accounts span many markets | proposed-not-done | angle never run | UA:portfolio-margin

## state/storage/rocksdb

- wal-cap-512mib | Default TORUS_ROCKSDB_MAX_TOTAL_WAL_MB 512 so cold CFs stop pinning WAL | default-on | DB open 20.5 to 7.9 s | S64 #3; s74-wal-cap-2026-09-28.md; 51e42c1
- wal-cap-256-avoid-flush-recovery | 256 MiB WAL cap or avoid_flush_during_recovery for faster restart | proposed-not-done | 7.9 s open remains, not measured | s74-wal-cap-2026-09-28.md
- tw-history-off-wal-2048 | Validators run TORUS_TRADE_HISTORY=0 plus WAL budget 2048 MiB | merged | acc3 tw 204.0k vs b 187.2k | 8KS §7; BA acc2, acc3
- wal-compression-zstd | TORUS_ROCKSDB_WAL_COMPRESSION=zstd | parked | knob off; twd 207.7k vs tw 204.0k; re-decide after Phase 3 | 8KS §7; BA acc, acc3; mem:38e871cb
- append-cf-codec | Append-CF codec ZSTD1 or LZ4 for trade/DA CFs | rejected | ZSTD1 no change; LZ4 knob off | 8KS §7; BA acc
- hotstuff-da-own-rocksdb | HotStuff store and DA store in their own RocksDB instance (71.8 option A, consensus KV split) | merged | in-window DB wait -91%, block step -31% | S64 §3; 8KS §7; 1ca719f1
- chunk-exec-flush-batches | Split exec-side trie flush and save_books WriteBatches into bounded chunks so consensus writes do not queue | proposed-not-done | est +4-10% if write wait >=5 ms | R2a:blocktree-io
- chunked-lowpri-trade-writes | Chunked low-priority background trade-history writes | merged | 2eb62ce5 | 2eb62ce5; R2a:blocktree-io
- rocksdb-pipelined-write | TORUS_ROCKSDB_PIPELINED_WRITE as compiled default | default-on | -9% view time (s46) | S64 §3; 8f131571
- packed-trade-rows | One per-market row per block chunk plus one per trader per block, built off the exec thread | merged | history cost 28.5 to 4.1 CPU-s/1M | s77-s80 §3; d930405; R2b:settlement-writes; R2b:trade-history; R2b:graph-hubs; KR:trade-history(opus)
- trade-history-off-switch | Node-role switch TORUS_TRADE_HISTORY=0 for validators | merged | off 102.8k vs full 68.4k (n=1) | s77-s80 §3; 826bcf3; KR:trade-history(opus)
- trade-history-per-market-only | TORUS_TRADE_HISTORY=trades, per-market rows only | rejected | experiment 523c26c only, superseded by packed | s77-s80 §3
- trade-rows-sort-before-write | Sort trade rows before write (option B) | rejected | dropped: row count matters, not key order | s77-s80 §3
- trade-segment-files | Append-only per-block trade segment files / per-height files pruned by unlink, no memtable/WAL/compaction | proposed-not-done | bench-queue Later | R2b:trade-history; 8KS §7
- trade-cf-universal-compaction | Universal/tiered compaction, LZ4 bottom, no bloom for trade-history CFs | proposed-not-done | est -0.4..-0.55 core | R2b:rocksdb-options; KR:rocksdb-options(opus)
- memtable-insert-hint | Per-batch memtable insert hint on trade writer and flush-worker writes | proposed-not-done | est -20-40% memtable insert time | R2b:rocksdb-options
- block-cache-hygiene | Fix block-cache metric (default CF only), fill_cache=false for body reads, high-pri index/filter, hit/miss metrics | proposed-not-done | diagnostic first | R2b:rocksdb-options; KR:rocksdb-options(opus); s73-bad-minutes-2026-09-28.md
- trade-writer-disable-wal | disable_wal and drop low_pri for the trade-history writer | proposed-not-done | low-pri limiter pinned queue mean 27 | S64 §5
- wal-off-state-write | WAL off for the exec state write | proposed-not-done | open lead r9 | 8KS §7; mem:38e871cb
- trade-writer-bounded-handoff | Byte-bounded, non-blocking trade-writer hand-off with drop-and-count, arena recycle | proposed-not-done | sync_channel(256) stalls exec after ~15 min | R2b:settlement-writes; KR:trade-history(opus); KR:user-latency
- trade-history-retention-d13 | --trade-retention-blocks N trade-history retention | merged | ee7d630d | ee7d630d
- zstd-bottommost-compaction | Drop bottommost ZSTD for churny CFs | proposed-not-done | ZSTD ~3.8% node CPU | s77-s80 Open; R2b:graph-hubs; s82-exec-hasher-2026-09-30.md
- rocksdb-stats-off-probe | Run one arm with TORUS_ROCKSDB_STATS=0 to bound ticker overhead | proposed-not-done | est 0-1% | R2a:logging-telemetry; KR:metrics-cardinality
- native-trie-maintenance-off | Skip native trie maintenance (option 0 root skip, TORUS_NATIVE_TRIE_MAINTENANCE=0) | default-on | +31% matched/s, -26% CPU at 300 markets | s83-s85 §3, §5; e4273ec; R2b:trie-hash
- native-root-node-cache | In-RAM native-trie node cache with clean-write elision (TORUS_NATIVE_ROOT_CACHE) | merged | 4de37116 | S64 §5; 4de37116
- bucket-group-by-account | Group positions and balances by account in bucket assignment to keep the root cheaper | decided | superseded by maintenance off | R2b:trie-hash
- serial-bucket-hashing-arm | TORUS_PARALLEL_BUCKET_HASH=1 serial bucket hashing arm | proposed-not-done | moot with maintenance off | R2b:trie-hash
- incremental-evm-root-a1 | Incremental EVM state root | merged | 3.19 ms at 1M accounts | 8KS §7
- incremental-native-root-a2 | Bucketed Merkle incremental native root | decided | superseded by maintenance off + running hash | 8KS §7
- async-native-trie-maintenance | Async native trie maintenance | decided | not chosen ("wrong first move") | 8KS §7
- native-overlay-everywhere | No disk writes until commit | merged | shipped | 8KS §7
- dirty-page-limit | Raise dirty-page limit (dirty_bytes 2 GB) | rejected | H1 ruled out: write queue, not writeback | 8KS §7; mem:38e871cb
- xfs-node-data | Separate XFS filesystem (direct I/O) for node data | rejected | not adopted; within cell noise | s73-bad-minutes-2026-09-28.md §5
- state-pruner-compaction-filter | State pruner via compaction filter + DeleteFilesInRange, no range tombstones | merged | TORUS_PRUNE_MODE=filter default | baffa3a5
- commit-fsync-durability | Per-commit flush_wal(sync); move commit-boundary fsync off the hotstuff thread | proposed-not-done | designed only | 8KS §7; R2b:erasure-da-prod
- book-cf-sst-target-64mib | TORUS_BOOK_CF_TARGET_FILE_MB=64 for the book CF | measured-no-gain | 169.2k vs 168.6k | BA p2s0r; 05b1b71d
- t5-3-commit-persist-offthread | Commit manifest in consensus DB and commit-persist write off the consensus thread | merged | owner T5.3 p99 attribution continues | 173f9a12; S64 §5; R2b:body-storage-format(KR)
- compact-body-record | Stop storing bodies twice: manifest as durable body record (TORUS_BODY_RECORD_COMPACT) | proposed-not-done | CPU-s/1M -1.45..-1.6%, matched ~0 | 8KS §7; KR:body-storage-format; mem:ab99e13b
- lazy-commit-manifest | Write the commit manifest only when a committed block is parked | proposed-not-done | est +0.2-1.0% | R2a:event-bus; R2b:evm-overhead
- fed-frontier-piggyback | Piggyback the S444 fed-frontier write on the next block-tree batch | proposed-not-done | est +0.05-0.3% | R2b:evm-overhead; R2a:event-bus
- encode-body-record-once | encode_body_record walks 4.7 MB body twice (serialized_size + serialize_into) | proposed-not-done | -1.5-2.5 ms/full block | KR:body-storage-format
- pruner-committed-vs-executed | State pruner cuts by executed, not committed, height so unexecuted bodies survive | proposed-not-done | fail-stop risk when backlog > retention | KR:body-storage-format
- state-sync-snapshots | Snapshot producer, vote-state-safe restore, lowest-available-height advert, checkpoint-transfer catch-up | proposed-not-done | owner P4 state sync after testnet | KR:state-sync-snapshots
- da-cf-blobdb | Store native bodies as RocksDB blob files in CF_NATIVE_PENDING / CF_BLOCK_BODIES | proposed-not-done | CPU only -2.6..-3.3% | 8KS §7; KR:rocksdb-options(opus)
- blocked-time-attribution | wchan sampler and RocksDB PerfContext on hotstuff-algo to attribute ~70% off-CPU time | proposed-not-done | off-CPU tracing unusable (6843d79) | S64 #9; KR:build-config(opus)
- store-scan-per-market | Range-scan backend API to split the boot store scan per market | proposed-not-done | scan 1.4-2.1 s now largest boot phase | s74-boot-books-2026-09-28.md
- one-million-account-ram | 1M-account genesis RAM check | merged | 8.1-9.2 GB max per node | 8KS §9
- crash-consistency-audit | Audit multi-step persistence sequences for SIGKILL/power-loss holes | proposed-not-done | angle never run | UA:crash-consistency
- book-persistence-modes | Compare TORUS_BOOK_ROWS, resident books, deferred saves on RAM, write volume, rebuild | proposed-not-done | angle never run | UA:book-persistence

## checkpoint/hash

- running-state-hash | Running state hash on the flush worker with on-chain attestation tally | merged | -2% matched/s n.s.; ~40 ms/hashed block on W; owner: on | s83-s85 §8; d623d3c; ed266540
- phase3-coalesced-checkpoints | Coalesce exec writes into 15 s checkpoints with replay (Phase 3 steps 1-2) | merged | CPU -9.7%, writes -55%, matched +8.8% | OZS s61; 8KS §7
- phase3-merge-wait-fix | Remove the checkpoint layer merge wait on the exec thread | merged | settle -20.3 ms, blk/s +11.3% | OZS s54; 3cf8f72d
- checkpoint-interval-5s | Checkpoint interval 5 s vs 15 s | measured-no-gain | tie/small loss: CPU +1.1%, writes +10.8% | OZS s55; 8KS §7
- checkpoint-size-cap-50mb | Cap checkpoint size at 50 MB (ZC) | rejected | blk/s 0.36x | 8KS §7
- checkpoint-slicing-a1-q6 | Slice checkpoint writes (A1 / Q6) | parked | owner, behind T5.3 | 8KS §7
- staggered-60s-checkpoints | Staggered or 60 s checkpoints | proposed-not-done | open lead | 8KS §7; mem:38e871cb
- defer-book-blobs-layer-merge | Defer book blobs in the layer merge (TORUS_CKPT_DEFER_BOOKS) | parked | +0.95% within noise; opt-in | 8KS §7
- l3-1-coalesced-book-save | Encode each dirty Classic book once at checkpoint window close (TORUS_BOOK_COALESCE) | default-on | deep matched 1.160x, blk/s 3.42 to 15.76 | 8KI L3.1; b7bb3666
- l3-2-running-hash-v2 | Running hash absorbs per-block order-row deltas instead of whole-book blob | rejected | dead: hash off in cells, weakens fork detection | 8KI L3.2
- l6-1-window-committed-books | Hash and save Classic book blobs only at %100 attestation heights | in-progress | owner batch 10; score 0.14 | 8KI L6.1
- l3-3-exact-size-book-encoder | Exact-size fixed-layout Classic book encoder, bytes identical to borsh (TORUS_BOOK_FAST_ENCODE) | default-on | save_books/1k fills -15%; deep exec CPU 0.968 | 8KI L3.3; 8KL; 4b916ecf
- presize-borsh-vec | Presize the borsh output Vec (memmove 15% of deep save_books) | merged | covered by L3.3 | 8KS §8
- book-save-deferral-flush-worker | Move pass 2 of save_books to the flush worker | merged | +6% inconclusive; in combined line 19adf42 | s63-builds-and-body-fetch-2026-09-23.md Steps 3-4
- parallel-classic-book-save | Parallel Classic book save (TORUS_PARALLEL_BOOK_SAVE) | parked | +3.5% CPU-s/1M at equal matched/s | 8KS §8
- parallel-mode2-journal-drain | Parallel mode-2 journal drain | merged | perf/matched-200k | 8KS §8
- mode2-level-rows-authority | Level rows as authority with hash-only native mirror (3c) | parked | ship-blockers listed | 8KS §8
- level-hash-sponge-cache | Level-hash sponge cache 256 MB (TORUS_LEVEL_HASH_CACHE) | default-on | cannot help match-touched levels | 8KS §8; 3a3a96d8
- mode3-default | Chunked book rows (TORUS_BOOK_ROWS=3) as default | rejected | 1.013x / 0.9645x; bkm 300m 181.9k vs Classic 188.6k | 8KS §8; BA bkm; 1d5d7bd0
- chunked-book-digest-3c | Chunked level hash / book digest hard fork | rejected | hard fork judged not justified | 8KS §8
- mode3-deep-book-ab | 0 vs 1M resting A/B on mode 3 | proposed-not-done | s96.6 candidate | 8KS §8
- order-rows-into-chunk-digest | Fold order-row persistence into seq-chunk digest blobs instead of ~31k row KVs per block | proposed-not-done | est +1-4% | R2b:exec-pipeline
- execution-root-in-headers | Height-tagged lagged state/execution root checked without blocking | merged | via running hash + AttestStateHash | S64 §5; KR:env-config-drift
- state-root-placement | Execution certificates / post-commit attestations off the voting path | proposed-not-done | angle never run | UA:state-root-placement

## serialization/wire

- compact-order-encoding | Compact order wire/preimage encoding 51 B to ~19 B (varint, drop i128 padding, u32 tags) | proposed-not-done | est body 4.1 to 1.5 MB/block | KR:order-amend; S64 §5; UA:body-encoding
- schema-aware-gossip-codec | Varint bounded-expansion codec for native gossip batches on a new topic (L4.2) | queued | owner WAN list; score 0.04 | 8KI L4.2
- small-frame-no-zstd | Skip zstd for sub-1 KB consensus frames, reuse a thread-local compressor for large ones | proposed-not-done | +0-0.3% | R2a:msg-encoding
- push-zstd-off-connection-task | Move zstd-3 compress of 3.6 MB push envelope out of the libp2p connection task, compress once | proposed-not-done | -7-10 ms/leader view est | KR:transport-hol
- body-codec-off-swarm-task | spawn_blocking decode/encode of >64 KB bodies off the single swarm task | proposed-not-done | est -15..-28 ms/view | R2a:network-loop; S65 lead 6
- multistream-v1lazy | Single protocol advert / V1Lazy to drop a multistream-select RTT per unicast | proposed-not-done | ~0 loopback; -60 ms/view at 30 ms RTT | KR:transport-hol
- no-block-clone-events | Stop cloning the ~3.5 MB block into InsertBlockEvent / ReceiveProposalEvent | proposed-not-done | 0.5-1 ms/view | R2a:logging-telemetry
- rpc-block-body-raw-json | Serve RPC block bodies as raw JSON | tried-inconclusive | split verdict, CPU only | 8KS §6; mem:bfbd7612
- action-hash-scratch-buffers | Action-hash scratch reuse, batch-local DA body serialization, lazy leader OnceLock | merged | allocation only | S64 §3; 8KS §6
- reqresp-worker-cap | Separate consensus from bulk pushes under request-response 100-stream cap; surface inbound overflow | proposed-not-done | 0% measured at n=3 | KR:transport-hol
- outbound-failure-retry | Retry failed direct sends on a live connection instead of waiting for reconnect | proposed-not-done | each strand costs a view timeout | KR:transport-hol; S65 bugs

## threading/runtime/scheduling

- hotstuff-cpus-pinning | Pin hotstuff threads (HOTSTUFF_CPUS) | rejected | runqueue wait 19-22 to 32-41 ms/blk | S64 §3
- per-node-cpu-pinning | NODE_CPUS / BENCH_CPUS per-node cpusets | rejected | -5.7% matched/s, spread not lower | s75-rejoin-and-pinning-2026-09-29.md
- renice-sched-batch | Renice / SCHED_BATCH verify, rpc, rocksdb:low pools so consensus wins CPU | rejected | BORE premise not supported | S64 §5; 8KS §5; mem:e7087388
- shrink-main-tokio-runtime | Main tokio runtime 18 to 4 workers per node | proposed-not-done | main runtime still #[tokio::main] default | R2b:thread-oversubscription; S65 lead 6
- size-verify-pools-by-budget | Size the two verify rayon pools by budget, not cores/2; cap RPC blocking pool | proposed-not-done | est +2-4% via runqueue relief | R2b:thread-oversubscription
- poller-wake-on-enqueue | Hotstuff inbound poller blocks on the queue instead of 250 us sleep-poll; one-hop swarm to algo | proposed-not-done | sleep(250us) still at receiving.rs:95 | R2a:network-loop; R2a:msg-encoding; R2b:thread-oversubscription
- rpc-dedicated-runtime | RPC on its own 4-worker tokio runtime | merged | torus-node main.rs rpc-runtime | crates/torus-node/src/main.rs
- exec-throttle-watermarks | Shrink proposals as exec backlog grows (TORUS_EXEC_THROTTLE_WATERMARKS=8,16,24) | rejected | -26% matched/s, -44% total | s77-s80 §4
- async-post-flush-stage | Async post-flush stage | measured-no-gain | neutral in vivo, default off | 8KS §5
- validation-worker-thread | Dedicated validation worker thread | proposed-not-done | plan only | 8KS §5
- phase5-capacity-lag-fix | Phase 5 capacity as the exec-lag fix | parked | parked with Phase 5 | 8KS §5
- channel-capacity-audit | Bounded channel capacities, block vs drop, stalls backing into consensus | proposed-not-done | angle never run | UA:channel-capacity; S65 counts
- async-blocking-audit | Blocking RocksDB/locks/CPU work on tokio workers | proposed-not-done | angle never run | UA:async-blocking
- lock-contention-audit | Mempool RwLock, block tree, metrics mutexes: hold times, sharded/lock-free alternatives | proposed-not-done | angle never run | UA:lock-contention

## build/compiler/allocator

- jemalloc-compiled-in | tikv-jemallocator as global allocator (also RocksDB) | merged | matched -0.3%, blk/s +14.9%, CPU +4.3% | OZS s56; 8KS §9
- jemalloc-decay | MALLOC_CONF background_thread + dirty_decay_ms tuning | rejected | -7/-31%; dec RSS -1.72 GiB but matched -2.5% | OZS s62; 8KS §9
- malloc-arena-max-probe | MALLOC_ARENA_MAX=4 probe before allocator change | proposed-not-done | superseded by jemalloc | S64 §5
- release-profile-lto | Add [profile.release] with LTO / codegen-units 1 | parked | s124 r2 +0.9..+2.0% central; owner parked opt-level A/B | S64 §5; 8KS §9; mem:e7087388
- target-cpu-crc32c | Build with SSE4.2/PCLMUL or x86-64-v3 for hardware CRC32C in RocksDB | proposed-not-done | CPU -0.8..-1.0% | R2b:build-config; KR:build-config(opus); 8KS §7
- pgo-bolt | Profile-guided optimization (later BOLT) trained on a matched-bench cell | proposed-not-done | est +2-8%, low-medium conf | R2b:build-config; KR:build-config(opus)
- bench-prof-frame-pointers | bench-prof profile with frame pointers and line tables for perf sched_switch callchains | proposed-not-done | diagnostic | KR:build-config(opus)
- test-binary-merge-hakari | Test-binary merge, hakari, sccache cap | parked | owner parked | owner-list.md
- rss-attribution-sampler | 5 s memory samples and smaps split of RSS into backlog, layer, view | merged | p3rss campaign | R2b:memory-growth; BA p3rss
- debug-leftovers | Remove debug instrumentation and per-message INFO logs from release hot paths | proposed-not-done | angle never run | UA:debug-leftovers
- hasher-choice | FxHash/ahash vs SipHash on hot paths | merged | done later as P2-5 foldhash | UA:hasher-choice; 8KS §4

## harness/bench

- offered-load-knee-sweep | Same-binary RATE/CONC knee sweep, offered load as operating point | tried-inconclusive | CONC 512 no higher offer; cap 400 +14% total | S64 #6; s76-ingress-and-cap-sweep-2026-09-29.md
- bench-ingress-all-validators | Bench submits to all 3 validators; node RPC max conns 64 to 512 | merged | 512 conns all 3: 71.9k vs 57.7-68.5k | s76-ingress-and-cap-sweep-2026-09-29.md; adc583e; b65fb0e
- block-cap-400 | Block cap 200 to 400 with companion caps; cap 1000 wedged | merged | total matched +14%; ozarchy cap 200 144.6k vs 400 186.9k | s76-ingress-and-cap-sweep-2026-09-29.md; 7bcc179; BA harness-cap-ab
- inflight-cap-budget-shape | Generator per-sender in-flight cap N=4 plus open-order budget 900 as the standard shape | merged | n4-b900 174.3k vs base 96.4k | 8KS §10; BA mif, mif2a, mif3
- commit-tail-inflight | Bench in-flight released on committed bodies (commit tail) | merged | standard since p3s2g | OZS; BA p3s2g
- retry-busy-bench | Bench --retry-busy on busy RPC | merged | cap 20 222 to 75k; cap 400 175k vs 187k | BA harness-cap-ab
- loadgen-suspect-check | Record submit rate; flag cells below 80% of campaign median | merged | most outliers were slow generators | s77-s80 Decisions; s83-s85 lessons
- host-state-sampler | Per-process host sampler, quiet-host gate, per-cell RocksDB LOG | merged | s63 morning cells were host interference | s63-builds-and-body-fetch-2026-09-23.md; s74-wal-cap-2026-09-28.md
- host-cron-fix | Remove provider drop_caches/fstrim hourly cron at :17 | merged | host fix | s73-bad-minutes-2026-09-28.md §4
- multi-crash-harness | 660 s cells with 5 kills, crash-freeze.py per-kill freeze, KILL_NO_RESTART knob | merged | basis of rejoin A/B | s75-rejoin-and-pinning-2026-09-29.md; cee86e4b
- crash-gate-timing | Liveness gate with timing criterion, exclude planned kill window, max-commit-gap and freeze decomposition gates | proposed-not-done | 30 s freeze passed as "Stalls: none" | S64 §4 bug 10, §5; s63 Next
- summarize-metric-bugs | Fix negative commit_interval, flush_count 0, verify-cpu as wall, gossip_messages_sent counting direct sends | proposed-not-done | status not verified | S64 §4 bug 10
- sign-after-permit | Generator signs after taking the semaphore permit (actions arrive ~12 s into nonce window) | proposed-not-done | status not verified | S64 §5
- clamp-generator-pacing | Clamp generator pacing sleep to the deadline (RATE cells overshoot 15-17 s) | proposed-not-done | window parity +0-1.3% | R2b:bench-generator
- loadgen-low-priority | Run load generator and samplers at reduced CPU priority with first-fire jitter | proposed-not-done | est +0.3-1% | R2b:bench-generator
- count-429-timeouts | Make offered load explicit, total silent 429/timeout errors | proposed-not-done | 305-424/s across identical controls | R2b:bench-generator; R2a:rpc-ingress
- per-minute-reporting-deep-shape | Per-minute series plus a bounded / deep-book (P1M) workload | merged | series.txt; 1M resting shape | S64 §5; 8KS §0
- order-age-histograms | End-to-end order age per stage (admit, commit, exec, durable, visible) | merged | 01d2ee5 | S64 §5; s77-s80 §4
- gossipsub-log-directive | RUST_LOG libp2p_gossipsub::behaviour=error so Send Queue full stops dumping payloads | merged | harness env since s68; node default unverified | R2a:logging-telemetry; R2b:build-config; S65 lead 4
- collapse-genesis-info-lines | Collapse 100k per-account genesis balance INFO lines into one | proposed-not-done | -29% log bytes | R2a:logging-telemetry
- validate-timers-body-trace | Validate DA subphase timers, TORUS_BODY_FETCH_TRACE insert line | merged | 1067cc5 + af00202 | s76-validate-timers-2026-09-29.md
- block-build-phase-timers | Leader block_build phase timers (select, mirror, encode, attest) | merged | mirror 42%, select 39% | s68-block-build-phases-2026-09-26.md
- offcpu-tracing | offcputime tracing of hotstuff-algo | rejected | 14x slowdown, unusable (6843d79) | S64 §3
- production-profile-arm | Bench arm with custody on, 4 validators, WAN shaping and per-node egress | proposed-not-done | trial net is owner item | KR:erasure-da-prod; S64 §5
- moving-prices-walk | Moving prices walk 10 bp vs 0 at 300 markets | measured-no-gain | w10 107.1k vs w0 111.4k | BA walk
- market-count-locality | Market count scan, uniform vs markets-per-sender locality | merged | uniform 300 markets -46%; MPS=10 -5% | s83-s85 §1-2
- cloid-share-knob | Bench CLOID_SHARE knob | merged | cloid 100 -11.7% vs 0 | OZS s59
- cancel-by-id-cell | Bench cancel-by-id / modify fractions | merged | ~2.0 vs ~3.0 ms with P2-1 | BA p2byid; e644bba0
- liq-stress-cell | Liquidation stress bench (thin accounts, signed shock) | merged | bench/liq-stress | BA liq
- workload-realism | Compare generator order mix with real exchange flow | proposed-not-done | angle never run | UA:workload-realism
- s80-loadgen-slow-state | Investigate loadgen slow state (main cells at 119-276 actions/s) | proposed-not-done | cause unknown | s77-s80 Open
- process-total-cpu-metric | Score CPU A/Bs on node process total, not pidstat per-thread rows (per-block workers missed) | merged | per-thread sum 34.2 vs total 47.1 CPU-s/1M | s82-exec-hasher-2026-09-30.md; s83-s85 Metrics
- harness-disagree-artifacts | Fix false DISAGREE from INFO fail-stop line and height spread >5 | merged | fixes with d623d3c | s83-s85 §4, §8
- oracle-feed-drain | Keep the oracle feed live through the drain (ORACLE_FEED_DRAIN=1) | merged | 37ff2afe | BA feeddrain; 37ff2afe

## rpc/streams

- trade-streams | newTrades / userFills websocket streams fed from exec fills, one message per block | merged | +2.1% p=0.67 without subscriber | s77-s80 §5; 7ebcfa3
- fill-extras-when-subscribed | Record fill extras only while subscribed; serialize payload once per filter; all-markets newTrades gated | merged | e80d08a, 7117269 | s77-s80 §5.1
- subscriber-cell-ab | Interleaved A/B with live subscribers after stream fixes | proposed-not-done | never run since fixes | s77-s80 Open
- noscan-commit-trade-scan | Drop or gate commit-time scan_trades_for_block on subscribers, feed from exec batch | tried-inconclusive | noscan 94.3k vs main 91.9k p 0.37-0.64 | s77-s80 §5.3; 5270a3f; R2b:trade-history
- ws-notifications-exact | WS notifications from event.block after exec instead of lossy pre-exec scan; executed height in eth_blockNumber | proposed-not-done | 165/401 heights never notified; T5.3 head fix partial | S64 §4 bug 6; 0036f1f3
- newtrades-fanout-scalable | Aggregate newTrades fan-out (not per fill), handle Lagged subscribers without silent drop | proposed-not-done | ~63k msgs/s per subscriber | KR:user-latency
- bound-public-read-rpcs | Bound rows examined by public read RPCs (getOpenOrders full scan) | merged | R24 e7f3faa3 | KR:user-latency; e7f3faa3
- getorderbook-in-memory | Serve getOrderBook from the in-memory book | proposed-not-done | before mainnet | 8KS §8
- native-action-status | Status / receipt query for native actions and the height state reflects | proposed-not-done | fate unknown for 60-95 s | KR:user-latency
- metrics-method-label | Bound the method label on torus_rpc_requests_total | proposed-not-done | unbounded heap via RPC client | KR:metrics-cardinality
- metrics-accept-loop | serve_metrics survives accept() errors | proposed-not-done | /metrics lost for process lifetime | KR:metrics-cardinality; S65 bugs
- single-submit-ingress-pool | Single-submit verify uses the dedicated ingress pool | proposed-not-done | consensus-CPU starvation path | KR:dos-vectors
- rpc-read-load | Move read-side load to separate RPC/indexer nodes or caches | proposed-not-done | angle never run | UA:rpc-read-load
- market-data-streaming | Scalable book-delta / trades / fills streaming design | proposed-not-done | angle never run | UA:market-data-streaming

## other

- session-expiry-ms | Session-key expiry compared in milliseconds everywhere (was seconds vs ms) | merged | 5509c724 | S65 bugs; KR:timers-clocks; KR:session-keys; KR:determinism
- count-sessions-scan | count_sessions_for_owner full CF scan counting expired sessions | proposed-not-done | O(N) exec stall per CreateSession | KR:session-keys
- session-key-nonce-space | Per-key nonce space so same-ms actions from two session keys are not dropped | proposed-not-done | ~2% silent loss per key pair | KR:session-keys
- book-mode-in-genesis | Consensus-visible knobs (TORUS_BOOK_ROWS) bound in genesis, genesis-bound data dir refuses restart without --genesis | merged | c78d75f4; book-mode-genesis | KR:env-config-drift; c78d75f4
- fleet-config-fingerprint | Boot config dump and peer-exchanged config fingerprint for env drift | proposed-not-done | ~70 private OnceLock env reads | KR:env-config-drift
- upgrade-path | Safer upgrade/migration process for lockstep consensus changes | in-progress | owner P0.0 phase 7 governance activation | UA:upgrade-path
- validator-key-ops | Validator key management and opsec | proposed-not-done | angle never run | UA:validator-key-ops
- prod-observability | Liveness-freeze, expiry-rate and fork-detection alerts | proposed-not-done | angle never run | UA:prod-observability
- recent-change-safety | Crash/determinism review of exec pipeline, body fetch, deferred saves, batched cancel-all, CTE | proposed-not-done | angle never run | UA:recent-change-safety
- start-network-before-replay | Start networking before replaying committed-but-unexecuted blocks, or replay async | parked | most of each crash freeze; parked for throughput | s74-boot-books-2026-09-28.md; s75 Open
- parallel-boot-book-load | Rebuild and verify each market's book on scoped workers at boot (LPT order) | merged | load_books 4.5 to 3.1 s in vivo | s74-boot-books-2026-09-28.md; d6528ec
- quic-idle-keepalive | Shorten QUIC idle/keep-alive 10 s to 2-3 s for fast-restarting peers | proposed-not-done | worth 0 s today | S64 §5
- udp-receive-buffer | Raise UDP receive buffer (sysctl, SO_RCVBUF) | proposed-not-done | ~70-105 drops/s, not linked to slow legs | S64 §5; s63; s72
- graceful-sigterm | SIGTERM is a bounded graceful shutdown like SIGINT | merged | SIGTERM to exit 19-21 s | 8KS §12; OZS s61; 8dedad03
- unwedge-tool-durability | Unwedge tool and body-durability invariant (durable before reference, S459) | merged | shipped | 8KS §12
