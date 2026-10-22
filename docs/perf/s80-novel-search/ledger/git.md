# Ledger part 'git': ideas tried, merged, rejected or proposed (from branches, main history, unmerged branches)

Scope: 387 branch names in branches.txt (local + origin), `git log -n 3` and `merge-base --is-ancestor <b> main` for each, main first-parent log (all 1,240 commits), main non-merge perf/exp commits, and the s58 perf-search journals kept on the unmerged branch docs/s58-workflow-arch (run1/run2 journal.jsonl: candidates, 3-lens verdicts, prior-work map, s123 exclusion list). main = eb993e3f.
Status key: merged = on main, flag off or no default change; default-on = on main and on by default; "(owner-list)" in the source = on the owner's s136 exclusion list.
Flag letters used in commits: D = TORUS_SETTLE_DEFER_RESULTS, M = TORUS_PARALLEL_MATCH_SETUP, T = TORUS_FAST_SELL_TOP_UPS, E/F = TORUS_EPOCH_FAST_PATH, G = TORUS_EXEC_POOL_GRAIN, B = TORUS_PARALLEL_BOOK_SAVE, PC = TORUS_PARALLEL_CANCEL_ALL, KP = TORUS_EXEC_PARALLEL_KEYS, DB = TORUS_CKPT_DEFER_BOOKS, PE = TORUS_PARALLEL_ENGINE.

## consensus
- tc-required-phases-abc | Leaders always back a timeout view with TC(v-1); VALIDPROPOSAL V1/V3 always on, high QCs verified; phase C leader-signed headers, signed tips, tip views | in-progress | batch 9 unmerged (ahead 33); TC liveness stall open | origin/feat/tc-required-phase-a/b/c, origin/merge/s134-batch9 56923063 (owner-list)
- tc-liveness-ed25519-check-once | Per-thread memo of passed ed25519 checks so a replica verifies each signature once; single verify_signature entry point with counter | in-progress | RED test: four validators keep committing with slow signature checks | origin/fix/tc-liveness 0937f178, 44587105 (owner-list)
- skip-reverify-high-qc-equal-highest-pc | Skip re-verifying a high QC that equals this replica's highest PC (TC phase B) | in-progress | part of batch 9, no separate bench | e9b74b2c on origin/feat/tc-required-phase-b (owner-list)
- tip-checks-reuse-vset-state | Tip checks reuse the caller's validator-set state; reputation read only when reputation leader selection is on | in-progress | batch 9 | a45d1c13 on origin/fix/tc-liveness (owner-list)
- dead-leader-skip-b1 | Execution attributes missed views from committed blocks, excludes a leader after K=3 misses (2^j re-admit floor, j cap 6) via the epoch plan | in-progress | phases 1-2 built, evidence rides commit manifest | origin/feat/dead-leader-p1, origin/feat/dead-leader-p2 0f3f6524, d8156eca (owner-list)
- one-fresh-proposer-per-view | During a validator-set update only one fresh proposer per view; NE reply to requester; timeout blame on view proposer | decided | owner s134/s136 decision | 76862c2c (owner-list)
- unanimous-qc-one-chain-commit | Commit B_h when its own QC carries all n votes, plus a vote-lock rule; fall back to 2-chain otherwise (8k L1.1) | proposed-not-done | est paced p50 -3 to -12%; saturated 0 | docs/perf/8k-explore-ideas.md L1.1 (main 013bc94d)
- optimistic-exec-at-speculative-qc | Feed exec at the speculative 1-QC commit, keep durability at the irrevocable height, roll back on mismatch (8k L1.2) | proposed-not-done | est paced order-to-result -2 to -6%; saturated 0 | 8k-explore-ideas L1.2
- speculative-block-production | Next proposer builds its block before the QC arrives (option C) | proposed-not-done | planned, never implemented | 6e9da0b6 (hybrid-pipelining + speculative plans)
- proposer-commit-on-advance-view | Proposer commits from the AdvanceView carrying its own block's QC, gated on a non-full exec channel | proposed-not-done | targets val0 structural ~1.2 s out-of-window p99 | docs/s58-workflow-arch run1 consensus-cadence-2
- stable-leader-tenure | leader_tenure K: select_leader uses view/tenure so one leader proposes K consecutive views | tried-inconclusive | May branch, old architecture, never benched | origin/stable-leader 2e193c46
- iwrr-closed-form-leader-selection | O(n log n) closed-form IWRR leader selection instead of iterative walk | merged | fixed height-drag root cause | 5828ec20
- reputation-leader-selection-off | B3 reputation-weighted leader selection disabled; plain round-robin | merged | removed reputation death spiral | 1d638b44, 82ab6fed
- leader-reputation-kv-cache | Write-through in-memory leader-reputation cache in BlockTreeSingleton | merged | removes ~9 KV reads per round | 7a1b980f
- backup-vote-collector-leader-v2 | Votes also go to leader(view+2) as backup collector; rejected body drops its header | merged | 3-validator liveness fix | 8dcf0c5f
- vote-after-body | Vote only once the body is held; pre-vote parent check | merged | no full-load regression; +19 ms vote delay at low load | 48b5b0f6
- persist-vote-before-send | Persist vote state before sending the vote (safety ordering) | merged | -8% n.s. | f05b20e1
- vote-state-own-rocksdb | Keep HotStuff vote state in its own RocksDB | measured-no-gain | vote save p99 113->4 ms; matched/s +0.8% noise | origin/perf/s67-vote-db 85a4331d, 0dec24b1
- collector-local-advance | Collector enters its next view on its own PC instead of the AdvanceView loopback | measured-no-gain | qc_to_advance -45..-70 ms; 68.5k vs 74.3k matched/s | origin/perf/s70-collector-local-advance b20b2350, fede4849
- consensus-reuse-action-hashes | Seed follower PendingProposal hashes and reuse them in dispatch prune | rejected | ~1% CPU; hotstuff-algo only 0.17 cores at 300m | s123 list in docs/s58-workflow-arch (prior_18c-s123)
- next-leader-parent-path-slim | Hash reuse plus parallel DA decode on the next leader's parent-body path | rejected | duplicates hash reuse; consensus not the limiter | s123 list (docs/s58-workflow-arch)
- consensus-view-legs-trim | Trim per-view consensus legs (r2 matched-200k round) | measured-no-gain | -6% vs same-binary control | M200k section 6 via docs/s58-workflow-arch prior_history
- async-block-validation-item3 | Validate blocks asynchronously off the HotStuff thread (torus-async-validate) | tried-inconclusive | 287 tests pass, never benched; L3 cancelled item 3 | origin/wip/async-validate-cand 2f118ba0, 4076355c, 34169d60
- single-batch-propose-writes | Port fd1fbba insert_and_update: one write batch for propose-time persists | parked | -20-25 ms on 200k branch, suspected timeout regression | s64 section 3 via docs/s58-workflow-arch prior_history
- m2-timestamp-lower-bound | Block timestamp lower bound checked before the vote (30 s bound, TC tip power rule) | parked | reverted off testnet genesis branch (owner s131) | c8cb3e20, 6d28c163, origin/wip/m2-timestamp-lower-bound (owner-list)
- p00-activation-gating | Governance-scheduled activation heights and feature bits for consensus changes (P0.0 phases 1-6) | merged | phase 7 deferred to after testnet | 40705baf (owner-list)
- cte-consensus-then-execute | Commit first, execute native actions post-commit on a background exec thread | merged | foundation (May) | 6a2e55dc, a42b4d1f
- header-first-proposals | Header-first proposals for hybrid pipelining; bodies disseminated separately | merged | foundation (May) | e73edaa2

## pacemaker/timeouts
- view-timeout-base-1200 | Default view-timeout base 500 -> 1200 ms in genesis | default-on | +7% matched/s (5/5 pairs), dead views -85% | 7284de3f, 5393caf3, d02e593f
- timeout-base-raise-cap100 | Raise timeout_base_ms 500 -> 1500 at block cap 100 | measured-no-gain | dead views gone, view length absorbed it | 7bc3ff90
- pacemaker-overdue-deadline-rebase | Hypothesis: views entered past deadline burn instantly; rebase overdue deadlines | rejected | only 2-5 of ~590 views entered late | s68 timeout-base doc via prior_history
- pacemaker-stall-backoff | Fleet backoff_factor/backoff_cap; scale schedule increments, rebase clamp, epoch extend by stall multiplier | merged | Jul tasks A1-A6 | 32278ea9..76894f4a
- commit-lag-view-backoff-s470 | Commit-lag view backoff plus proposer exec watermark (S470 wedge fix); commit_lag_backoff_cap 8 in testnet genesis | merged | eliminated 175-310 s commit freezes | b8fcdec2, 95cd3996, 162b3987
- header-resend-250ms | Stuck leader re-sends a dropped proposal header over the direct path after 250 ms (was 3000 ms default) | default-on | lost-proposal timeouts 33 -> 3; keeps +10% matched/s | 3429855d, 87bbdd22, 49a86049 (section 77)
- consensus-gossip-rate-limit-metric | Count drops by the hardcoded 50 msg/s per-author consensus gossip limiter (no behaviour change) | merged | drops preceded 16/16 lost-proposal timeouts | 0af69a76, exp/archy-s58-consensus-ratelimit-drops 857b9770
- consensus-limiter-exemption-or-direct-fan | Exempt consensus messages from the limiter or send them over the direct fan (ALLfan arm) | parked | ALLfan -1% matched/s, +3% CPU-s/1M vs ALL | 49a86049, 9bb98b10 (section 78)
- hold-light-proposals-under-rate-limit | Pace near-empty proposals so honest consensus gossip stays under 50 msg/s per author | proposed-not-done | superseded by header resend 250 ms | docs/s58-workflow-arch run1 consensus-cadence-1
- propose-defer-on-exec-queue | Leader defers a fresh proposal while its exec queue is at a watermark, capped at half a view, skipped after a TC | parked | campaign B dropped; revisit after Phase 5 | origin/exp/s123-propose-defer-on-exec-queue-v2 4496a674, 3a8c363f
- exec-throttle-watermarks | TORUS_EXEC_THROTTLE_WATERMARKS proposer throttle on exec lag | rejected | s77 -25% matched/s; section 73 arm W -8% | s123 list (docs/s58-workflow-arch)
- exec-backlog-watermark-pacing-pkgd | Scale proposer caps by exec queue depth (pkgD-r1); non-blocking exec dispatch with bounded deferred_exec (r2) | merged | early proof NO-GO: ceiling flat ~2.5-3.0k/s | 6d3b2580, ee763a20, 1faa1fe7
- epoch-boundary-fast-path | No-EVM, no-slash epoch-boundary blocks skip pipeline_barrier; checkpoint due rule gains a boundary term | default-on | on PC: blk/s 6.38->12.04, p50 57->15 ms | efe9aa47, 3429855d, 545bcf12 (section 84)
- epoch-fast-path-archy-duplicate | Archy's own epoch-boundary fast path with a forced boundary checkpoint (TORUS_EPOCH_FAST_PATH, arm E) | measured-no-gain | E alone = C; duplicate of efe9aa47, reverted | exp/archy-s58-epoch-boundary-fast-path bb62bab1, 78373203
- fail-views-fast-bs4a | Fail views fast and recover DA bodies off-thread (BS-4a); one event-driven mid-budget re-fetch (BS-4b) | merged | livelock fix (Jul) | e4ebccdd, 1f206f1e, origin/integration/bs4a-livelock-s428
- qc-to-advance-step-timers | Sub-timers inside qc_to_advance to attribute per-view consensus time | tried-inconclusive | commit_feed_app is 94-96% of qc_to_advance; 8z gap reversed | origin/perf/qca-timers-o/b0/b, 3aac6513 (section 118)
- bounded-receive-before-expiry | Bounded receive-before-expiry experiment on proposals (Sep) | tried-inconclusive | no result recorded in subject | 110ebfc6
- t51-block-cadence-310ms | Attribute and cut the ~310 ms block cadence (roadmap T5.1) | parked | after testnet | 8ee3b085 (owner-list)
- t52-p99-floor-1200ms | Attribute the ~1.2 s out-of-window block-time p99 floor (T5.2) | parked | val0 129 intervals >1 s vs 24-32 others | 8ee3b085, section 71.5 (owner-list)
- a2-short-timeout | Short timeout for the dead-leader W=L transient (a2) | parked | follow-up to b1 | 6c16e5c0 (owner-list)

## dissemination/DA
- compact-block-proposals | CompactBlock proposals with unicast native-action dissemination and durable DA store reconstruction | merged | foundation (Jun) | 865d4e1f, 67461710, 4ddfe595
- native-da-pull-fallback | Rare by-hash pull of missing CompactBlock bodies via /torus/native-da/1.0; widened ~1 s pull budget | merged | foundation (Jun) | 5ec83a83, 7ceb01ff, d08fed87
- gossip-prespread | Gossip pre-spread of native actions before proposal; hardened body fetch | merged | Sprint 3 | af17cd68
- hash-only-push-threshold-512k | Pre-proposal push sends a hash manifest above 512 KB (code default) instead of the 8 MB bench pin | default-on | matched/s 1.043, CPU-s/1M 0.941 | ac95916a (section 86), 54cad185 (owner-list WAN threshold)
- bench-push-manifest-not-full-body | s58 duplicate of the 512 KB threshold: send manifests, gossip already delivers bodies | merged | same as section 86 | docs/s58-workflow-arch run1 network-da-1
- inline-body-small-blocks | TORUS_INLINE_BODY_MAX_BYTES: inline the body in small proposals | rejected | proven liveness hazard; branch removed | 0c3ea780, 6bee671f, f9c91edc
- proactive-body-push-small-blocks | TORUS_BODY_PUSH_MAX_BYTES: push header plus body for small blocks | measured-no-gain | idle +7%, loaded neutral (work-conserved views) | 53b233fa, d80b4220
- shard-custody-erasure | Encode and store erasure shards per block on hotstuff-algo (T3.1) | merged | default off: custody costs -21.6% matched/s | d6c4d0d2, 3cb143ab (owner-list T3.1)
- lazy-shard-custody-serve-time | Build erasure shards on peer request from the DA body instead of eagerly per block | rejected | gain zero on every measured config | docs/s58-workflow-arch run2 production-default-hidden-costs
- eager-custody-linear-merkle-offthread | Keep eager custody but linear Merkle, streaming leaf hash, one background writer off hotstuff-algo | proposed-not-done | choose-one with lazy; custody off anyway | docs/s58-workflow-arch run2
- native-prespread-direct-zstd | Send native pre-spread batches over the authenticated /torus/direct 0xFD fan with per-peer zstd, not signed gossipsub | proposed-not-done | safe but needs design changes | docs/s58-workflow-arch run2 r3 tokio-network-cpu-1
- no-relay-author-direct-gossip | Ignore relays when the source is the validator author and all subscribers are validators (8k L4.1) | proposed-not-done | est CPU -0.7 to -1.5%, WAN bytes -30% | 8k-explore-ideas L4.1 (owner-list L4.1-L4.4)
- schema-aware-gossip-codec | Varint bounded-expansion codec for native batches on a new topic version (8k L4.2) | proposed-not-done | est CPU -1 to -2.5%; WAN bytes -25 to -74% | 8k-explore-ideas L4.2 (owner-list)
- prefilled-preproposal-manifest | Flush gossip before the push; inline likely-missing bodies in the manifest (8k L4.3) | proposed-not-done | archy 0; WAN up to 1 RTT | 8k-explore-ideas L4.3 (owner-list)
- bbr-quic-windows | Patch libp2p-quic for quinn BBR and BDP-sized windows (8k L4.4) | proposed-not-done | archy 0; WAN only if loss >= 1e-5 | 8k-explore-ideas L4.4 (owner-list)
- quic-prefer-aes128-gcm | Prefer TLS_AES_128_GCM over ChaCha20 in libp2p QUIC | rejected | -0.4 to -0.6% CPU, below resolution | docs/s58-workflow-arch run2 r3 dropped
- zstd-wire-frames | /torus/*/2.0 zstd wire frames with per-peer fallback, gossip zstd default-on; v2 zstd gossip topic removed | merged | Sprint 5; v2 topic removed as DoS vector | 6cbda397, 507a5711, d922c1f2
- b2-dual-path-dedup | Scope dual-path message dedup to one-shot HotStuff messages; exempt block-data | merged | perf/dissemination tip unmerged but fixes on main | 4f832e59, 2a0d2074, origin/perf/dissemination
- body-serve-thread | Serve block-data requests off the HotStuff algorithm thread | default-on | throughput neutral; serve queue 4-6x lower | f9b013ef, 996cee29
- defer-parent-feed | TORUS_DEFER_PARENT_FEED: defer commit feed on next leader's parent-body insert (s72 fix D) | measured-no-gain | fails dissemination gate; mean -2.0k | 678719e3, 15798339
- defer-commit-feed | TORUS_DEFER_COMMIT_FEED (s65 item C) | measured-no-gain | no throughput gain, n=3 | s123 list (docs/s58-workflow-arch)
- da-batch-read-flush-on-miss | One MultiGet per compact block; DA batch read flushes the ingress mirror only on a miss | merged | next-leader validate p50 40 -> 25 ms; throughput n.s. | a3a476f8, bce54d0e
- proposer-writes-only-missing-da | Proposer mirrors only DA bodies the store lacks | merged | block_build -30%, throughput flat | cc81630c
- body-fetch-fast-rotation | Rotate body fetch off a stalled proposer after 200 ms (was 900 ms); wake reconstruct on body arrival | merged | Jul | 59fabcf8, fa7791d2
- da-recovery-worker-offthread | DaRecoveryWorker recovers native-DA bodies off the consensus thread | merged | Jul | 597f5787
- per-view-floor-shaves | Batch DA mirror, cache spec-commits, skip redundant QC verify, deadline-bound reconstruct waits | merged | Jul | b674376a
- compact-exec-channel-handoff | Queue committed blocks to exec as compact refs, rematerialize just in time (prefetch 2) | proposed-not-done | RSS -0.3..-0.45 GB, +0.6% CPU-s/1M | docs/s58-workflow-arch r1-r3 consensus-cadence-3
- compact-body-record-from-da | Store a hash-list body record in CF_BLOCK_BODIES for DA-backed commits, rebuild bodies from DA on read | proposed-not-done | ~6.1 MB body encode on hotstuff-algo per commit | docs/s58-workflow-arch run2 r3 committed-body-double-persist
- da-cf-blob-files | Integrated BlobDB (key-value separation) for cf_native_pending in the consensus DB | proposed-not-done | compaction 121.6 CPU-s/node/run, 3.7% node CPU | docs/s58-workflow-arch run1 state-write-1
- fifo-compaction-da-cf | RocksDB FIFO compaction for cf_native_pending (8k L3.4) | rejected | owner ruled FIFO out; breaks heal and RH4 | 8k-explore-ideas L3.4
- rh4-da-body-gc | Durable DA body GC with retention, uncommitted-tail hold, waits on CommitPersistWriter ticket | default-on | prune-by-default; SST growth 0.200 -> ~0 GB/min | 687eac9e, b452b4af, 0c0b01a9
- rh4-gc-worker-offthread | DA body GC pass on a worker off the consensus thread with a fence | merged | worker removes 8o's p50 cost (B/A 0.989) | bbc38711, 34273f55 (section 103)
- state-pruner-compaction-filter | State pruner via compaction filter + DeleteFilesInRange instead of range tombstones (TORUS_PRUNE_MODE=filter) | default-on | removes 74% of extra cf_block_bodies compaction | baffa3a5, bf2e828f (section 112)
- direct-to-leader-forwarding | Forward EVM txs and batched native actions directly to the leader | merged | S458 A/B, honest bar recorded | c3a87d5e, 56ddd74f, 3fd35e90
- push-queue-hardening | Bounded in-flight native pushes, QUIC bidi window raise, enqueue-on-disconnect, re-push on reconnect | merged | Jun | d35cac37, 0ef73e09, ad3c136e, 7570e7b4
- quic-idle-timeout-shorter | Cut QUIC idle timeout 10 s -> 2-3 s | rejected | worth 0 s today | s64 section 5 via prior_history
- udp-receive-buffer-8mb | sysctl UDP receive buffer 8 MB | measured-no-gain | no change (s50) | prior_history (docs/s58-workflow-arch)
- mesh-watchdog-resubscribe | Stale-connection kick plus 5 s/1 s watchdog for gossipsub resubscribe wedge; explicit validator peering | merged | wedge recovers <1 s; -0.86% n.s. | de1a97c1, 576d3be7

## ingress/mempool
- block-cap-400 | Native block cap 200 -> 400 with companion caps | default-on | cap 400 +14% total matched vs 200 | 7bcc179a, 0b7f6f73
- block-cap-1000 | NATIVE_TOTAL_BLOCK_CAP 100 -> 1000 | rejected | cap 1000 wedged under load; reverted | 65498218, cc0f958d, origin/cap1000-3val-perf
- small-place-order-charged-block-budget | Proposer budget charged by orders, so 1-50-order place actions fill a block like 400-order ones | rejected | 0 gain on every tracked cell | docs/s58-workflow-arch run2 r3 small-action-regime
- arrival-order-pool-selection | Pool selection key (priority, seq, sender, nonce) instead of nonce order | rejected | stalled ~4.5 min under gossip flood; reverted | 67b6e9a4, origin/exp/s66-abc-no-arrival-order
- cancel-share-oldest-first | Fill the native pool's 25% cancel share oldest-first, per-sender nonce order | merged | cancel-all expiries 16k/node -> 0 | 3f54704e, 844c86fe
- admission-limit-horizon | Shed non-cancels before verify above horizon x commit rate (20 s) | default-on | expired 17-18% vs 29-50%; throughput neutral | 42cde1b0
- exec-aware-admission | TORUS_ADMISSION_EXEC_AWARE counts committed-but-unexecuted actions in admission backlog | merged | default off: 5 s -33%, 2 s -51% matched/s | e72a5021, 45286113 (section 73), origin/perf/exec-aware-admission
- remove-committed-hash-index | remove_committed via hash-index multimap, no pool scan | merged | +10% matched/s (cap 25), remove_committed -52% | 1e41beab
- gossip-ingest-early-dedup | Hash once and drop already-pooled duplicates before DA re-mirror, key derivation and insert | proposed-not-done | s58 r1 candidate | docs/s58-workflow-arch run1 ingress-verify
- gossip-prescreen-sojourn-gate | R2.2 dedup+staleness prescreen before verify; R2.3 sojourn admission gate; R2.4 wider pre-proposal channel | tried-inconclusive | Jul branches, old arch; never on main | origin/perf/deep-book-storage 5d630906, 742eaf6d, 471cc698
- gossip-mirror-off-loop | R2.1 off-loop mirror worker and decode off the swarm loop | tried-inconclusive | Jul, unmerged | origin/perf/p3-throughput 06c13819
- proposer-near-expiry-filter | Proposer filters near-expiry actions (option A) with a counter | tried-inconclusive | Jul, unmerged | 41418182 on origin/perf/deep-book-storage
- native-antispam-a-d | Funded check, per-address limit, per-IP weight, bounded cancel prefix | merged | -0.85% at cap 400; A,B,D off on bench | cf773659..9fc85e5f, 92a02ed6
- pool-full-preverify-shedding | Shed at pool-full before verify; shed by peeking the bincode action tag before decode | merged | cut 6 | b5816146, f37633d5
- ingress-market-spec-cache | Cache market tick/lot rows and oracle marks per published ReadView height in RPC verify | proposed-not-done | ~221 market reads per batch removed | docs/s58-workflow-arch run1 ingress-verify-2
- ingress-view-skip-position-slices | Leave CF_NATIVE_POSITIONS out of per-block ingress-view slices | proposed-not-done | RSS -0.2..-0.5 GB, matched/s ~0 | docs/s58-workflow-arch run1 state-write-2
- native-expiry-index-by-nonce | Index native expiry by nonce; reuse sender count entries during selection | merged | s61 bundle | 9e9800ea, 7ef271e1
- d12-nonce-window | Time-major nonce key, prune nonces below block_ms - 60 s each block | merged | replaces consumed-nonce set | b7b5bd04
- exec-trust-cache-default | Exec trust cache on by default, size derived from the native block cap | default-on | Jun/Aug | 7a1b980f, e2efffe3, bd18ba49
- evm-per-block-gas-budget | Per-block EVM gas budget replaces count caps | merged | Jul | c6834e25
- batch-submit-rpc-bincode | Batch-submit RPC and torus_submitNativeActionsBin (bincode ingress) | merged | Sprint 2 / 5 | 16c24681, 8f2b5308
- sprint1-throughput-rails | Admission semaphore and throughput rails for submit_native_action | merged | May-Jun | e2f5c227, 45dfd816

## signature/crypto
- parallel-eip712-sender-recovery | Parallelize EIP-712 sender recovery in batch_verify_native_actions | merged | Jun | ce50e823
- resolve-senders-once | Resolve native action senders once, no double secp256k1 recovery | merged | Jun | 0704ac38
- ingress-verify-dedicated-pool | Ingress verify on a dedicated rayon pool, not the global pool | merged | global pool: block 529 -> 4034 ms inversion | 1659352e, 40227743
- parallel-verify-l3 | TORUS_PARALLEL_VERIFY env-gated parallel signature verify | merged | L3: verify already banked | a2bcb886, 34169d60
- hash-typehash-once-gossip-key | Hash batch item typehash once; one trust-cache key per gossip ingest | merged | gossip-verify CPU -25%, node CPU -5% | 74d488f5, 0ff946ea
- exec-hash-once | Absorb canonical bytes once for sort and trust-cache keys; skip all-hit recovery; cache EIP-712 domain separator | merged | exec on-CPU/1k 0.955, blk/s 1.39x | 70993fa9, 893130c9 (section 94)
- exec-parallel-trust-keys-kp | Compute exec trust-cache keys in an order-preserving parallel pre-pass | measured-no-gain | KP +1.03% CI crosses 1; flag off | 61f8bb1a, fec7ebfe, 3cb143ab (section 82)
- exec-carried-verified-senders | Carry each committed action's locally verified sender from dispatch prune to exec; skip phase-1 fork-join | proposed-not-done | overlaps KP; s58 kept with major caveat | docs/s58-workflow-arch run1 ingress-verify-1
- ecrecover-libsecp256k1 | Recover EIP-712 senders with libsecp256k1 (already linked by revm) plus explicit low-S check, not k256 | proposed-not-done | s58 r2 kept 3/3 lenses | docs/s58-workflow-arch run2 secp256k1-recovery-cpu
- ecrecover-skip-trailing-verify | k256 recovery without the redundant trailing verify_prehash, plus explicit low-S check | proposed-not-done | trailing verify always passes once low-S holds | docs/s58-workflow-arch run2
- eip712-item-keccak-x4 | 4-lane AVX2 Keccak for the 400 independent PlaceOrderItem hashes per batch | proposed-not-done | est CPU-s/1M -4 to -7% | docs/s58-workflow-arch run1 ingress-verify-4
- eip712-item-hash-memo | Per-thread 3-level memo of PlaceOrderItem sponge states keyed on preimage prefixes (8k L5.1) | proposed-not-done | est CPU -2 to -6.5% | 8k-explore-ideas L5.1
- known-key-ecdsa-tables | Verify against the claimed sender's cached key with per-signer comb tables instead of ecrecover (8k L5.2) | proposed-not-done | est CPU -2.5 to -4% | 8k-explore-ideas L5.2
- session-signed-low-cpu-path | Stateless session-signature trust cache and v2 session digest over packed bytes (8k L5.3) | proposed-not-done | 0 on EIP-712 load | 8k-explore-ideas L5.3
- session-sig-validity-cache | Session signature-validity cache and session-owner cache to skip exec ed25519 verify (Jul) | tried-inconclusive | +38.6% session-signed mode, old arch | origin/perf/p3-throughput a80bd4c5, becdde22
- batch-gossip-ed25519-vk-cache | Batch gossip-ingest ed25519 verify with a shared verifying-key cache (#17) | tried-inconclusive | Jul, unmerged | 060e5f68 on origin/perf/deep-book-storage
- eip712-keccak-lazylock-statics | Hoist constant EIP-712 keccaks to LazyLock statics, no per-order alloc | tried-inconclusive | Jul unmerged; domain separator cache later merged | 4b50d799, 70993fa9
- keccak-asm-backend | Enable the asm keccak backend workspace-wide | merged | keccak 1.20x | 82523da5
- session-crypto-ingress-tuning | Cheaper signature path for ingress | rejected | ingress verify cost was host contention | prior_history (docs/s58-workflow-arch)
- compute-action-hash-twice | Reuse the pool's action hash instead of computing it twice per action | proposed-not-done | ~75 us/action, not re-measured | s77-s80 doc via prior_history

## execution/matching
- per-market-parallel-matching | Per-market parallel order matching | merged | May | 9c143607
- market-workers-lpt-cap | Cap market worker threads at core count with LPT chunking | merged | Jul | 9b09b630
- persistent-exec-pool-p23 | Dedicated rayon pool for match, settle pass A, end_resident instead of per-block thread::scope | merged | forks 44 -> 0; matched/s -1.2% n.s. | efcd1548, 56914dd1
- exec-pool-fine-grain-jobs-g | Split match and pass-A work into K x cap LPT chunks so idle threads steal from late starters | measured-no-gain | G not needed for cadence; left out of bundle | exp/archy-s58-exec-pool-fine-grain-jobs aaf0b804, 9bb98b10
- dynamic-exec-pool-items | Hand out per-market items dynamically, heaviest first, exec thread joins | proposed-not-done | folded into G | docs/s58-workflow-arch run1 exec-pipeline
- parallel-match-setup-m | Build each market's match inputs (reduce-only map, margins, requests) inside its pool match job | default-on | M +3.1% matched/s single-flag | 6954e735, 4156a83b
- parallel-cancel-all-pc | Batched cancel-all book pass on the exec pool, one job group per market | default-on | PC +4.51% matched/s (8 workers) | 7b23d38c, 7f9bb3a9, 3429855d
- batched-cancel-all-compaction | Each run of consecutive cancel-alls compacts each level once (TORUS_CANCEL_BATCH) | default-on | phase1 ~279 -> ~108 ms/blk | 58c9eb49, 9ddece6c
- deep-cancel-threshold-policies | Eligibility/movement-budget variants for deep-level compaction (queues >= 32,768) | rejected | policy 2 same-binary -6.15% | TC campaign 2026-09-18, 5e1593ba
- cancel-all-cheaper-margin-math | Cheaper margin/balance math in cancel-all | rejected | book removal is 99.4% of cancel-all | TC campaign via prior_history
- cancel-all-trader-index-p21 | Cancel-all visits only books where the sender has orders (TraderMarkets index) | merged | phase1 -0.44 ms/blk (gate missed, accepted) | 90632a75
- cancel-all-stale-index-prune | Drop a market from the trader set when its last order leaves, or background prune (P2-1 option B) | parked | ~0.4 ms/blk; before mainnet | P2 rows 24-25 via prior_history
- by-id-order-market-map | Order-id -> market map for cancel/modify by id (P2-1b) | rejected | 10-30 us saved vs 0.6-1.4 ms/blk upkeep | b082cda7
- seq-inline-level-queues | Each level keeps a lockstep VecDeque of seqs for contiguous partition_point (8k L2.3) | proposed-not-done | est matched/s +0.2 to +0.6% | 8k-explore-ideas L2.3
- level-seq-binary-search | Locate orders in their level by binary search on seq | merged | O(log depth) | f5c66dac
- matching-core-microarch-rank10 | FxHash indices, single-descent levels, O(1) trader back-index in the matching core | tried-inconclusive | Jul, unmerged, never benched on main | origin/perf/package-d-wave2 ef4bab61
- fixedpoint-i128-fast-path | FixedPoint checked_mul/checked_div i128 fast path | merged | part of P1-P4 +1.5% n.s. | b809d439, origin/perf/package-d-wave2 7a5981f0
- parallel-engine-pe | TORUS_PARALLEL_ENGINE: sender-sharded parallel Phase-2 prepare plus settle engagement | merged | default off: -1.4% matched/s, +15.7% blk/s | faa50ef8, 5a5fc909 (section 83) (owner-list)
- match-epilogue-batched-trader-index | Feed cancel-all trader index per trader, skip stop walk when nothing triggered, clear scratch in job | rejected | gain does not hold (fatal) | docs/s58-workflow-arch run2 r3 exec-untimed-remainder
- phase2-3-param-deref-cuts | Remove scattered PreparedOrder/params loads in sell_top_ups, s92 counters, Phase-3 request build | proposed-not-done | s58 r1 candidate | docs/s58-workflow-arch run1 profile-mining
- shared-code-instruction-inflation | perf-annotate drain_book/cancel_all_many/insert_order/match_market instruction inflation per fill | proposed-not-done | 1.2-2.5x instructions per fill, never followed | R section 9.2 via prior_history, 27a98a68
- traderorders-presize | Pre-size per-trader order vectors | measured-no-gain | probe test | d2d55147
- stops-dirty-flag | Skip diff_stop_rows unless stops changed | measured-no-gain | save_books -18% but matched/s flat | M200k section 13 via prior_history
- cloid-engine-cost-fixes | Pre-sized claimed set, cloid log resolved in worker, packed (trader,cloid) key, cross-market index | merged | c100 matched/s +5.6% | 0be7d6ae, 51a72114 (section 63)
- parallel-sharded-cloid-upkeep | Shard the cross-market cloid index by key; run duplicate pre-pass and drains per shard on the exec pool | proposed-not-done | gain only when orders carry cloids | docs/s58-workflow-arch run2 order-id-actions-regime
- by-id-heavy-bench-cell | 300m cell that really lands by-id cancels/lookups to split their cost | rejected | measurement only, ~1 by-id action per block | docs/s58-workflow-arch run2 dropped
- exec-action-sortkey-keccak | Cache or parallelize action_sort_key hashing | proposed-not-done | sort keccak ~0.12 share of exec thread | s123 follow-up via prior_history
- exec-prep-stage-fused-keys | Separate prep stage / fused keys before exec | rejected | folded into KP; ~0.4 ms | s123 list (docs/s58-workflow-arch)
- pre-exec-lookahead-verify | Dedicated thread computes trust keys, sort hashes and miss recovers one block ahead (8k L6.3) | proposed-not-done | est E -4 to -7% in full-channel windows | 8k-explore-ideas L6.3
- market-sharded-execution | Multiple exec engines in flight, one per market shard, deterministic cross-shard settle | proposed-not-done | named only path to 200k (Aug), never designed | M200k section 18 via prior_history
- exec-pipeline-flush-worker | Flush (root + state_write) on worker W one block deep behind an applied-height fence | default-on | chain -27.4%, matched/s +2.7% | fe4b40ac, 9ddece6c
- resident-books | Order books resident in memory, holder advanced across untouched blocks | default-on | load_books 65.7 -> 0.06 ms/blk | 1f5eb93f, ac031c7d, 8f131571
- cross-block-book-cache | Kill per-block O(depth) book reload with a cross-block cache (Jul) | tried-inconclusive | superseded by resident books | origin/perf/deep-book-storage 9898c34b
- drop-exec-deep-clone-bodies | Drop the exec thread's deep clone of native bodies (#18) | tried-inconclusive | Jul, unmerged | ddc74b6a on origin/perf/deep-book-storage
- end-resident-on-worker | end_resident on a worker, begin_resident before new_env | merged | hides ~92% of end_resident; -56 ms/blk | 2333ba4c
- item6-phase1-resident-rows | C1-C7: resident rows R, mark table, sums cache, liq over sums, partial re-value, decoded positions | merged | C6+C7 +25.7% at 300m | 81a95677..82bd1a41
- pf1-per-level-ask-depth | O(levels) same-batch bid top-up bound instead of crossing bids x resting asks | merged | 10m 1.93x (65.9k -> 127.5k) | 0ebfd71d
- p2-5-foldhash-maps | Exec-path maps SipHash -> seeded foldhash | merged | matched/s 1.036x | 631becaa
- seeded-ahash-exec-maps | Seeded ahash on execution-path maps (s82) | measured-no-gain | +0.4% p=0.91; superseded by P2-5 | origin/perf/s82-exec-hasher 3dabdb44, d8b15f7d
- dense-trader-slots | Intern each address once per market job, Vec slots for margins/positions (8k L2.4) | rejected | s82 hasher precedent | 8k-explore-ideas L2.4
- phase4-users-record | Users record replaces TraderPositions; overlay reads Users; typed PositionCache/BalanceCache loads | merged | 300m matched/s +3.01%, CPU-s/1M -2.38% | 7487b764, 4043e9de, bb33b988, 27d2c979
- phase4-open-order-counter | Stored per-user open-order counter fed by books' open-order logs (step 5) | measured-no-gain | net -1.18 ms/blk n.s. | origin/feat/item6-phase4-step5 6288ebe8, 839cbc9e
- open-order-count-no-spawn | Open-order count without per-count spawned threads | merged | closes OL -7..-11% cost | ded6e6c6
- per-action-results-v2 | Record per-action execution results (v2 status) | merged | no measurable cost | 9195c324
- trade-history-off-validators | TORUS_TRADE_HISTORY off on validators, on for --rpc-only | default-on | CPU -11.9%, matched/s +8.2% | 8c5ca921, 826bcf34
- packed-trade-rows | Packed fixed trade-history rows, encoded off the exec thread | merged | cost 28.5 -> 4.1 CPU-s/1M | b9b0c5a0, d930405e
- trades-only-history | TORUS_TRADE_HISTORY=trades: per-market rows, skip per-user rows | measured-no-gain | superseded by packed rows | origin/exp/s77-trades-only 523c26c1
- fixed-key-deferred-trade-rows | Fixed-key deferred trade rows (s60) | rejected | single pair +24.5%, repeat rejected | 3bb76341
- background-trade-writer-o3 | Background writer for trade-history CFs | merged | -39% bench cost (S414) | 1a51d1e3
- engine-parallel-phases-l3 | Parallel native executor phases / market workers (L3-ENG) | measured-no-gain | engine parallelism negative (L3 round 1) | 792ef61b, 34169d60

## settlement/margin/liquidation
- settle-pass-a-lpt-chunks | Settle pass A LPT-chunked across capped workers, not one thread per market | merged | settle -50%, matched/s +49% | 583baddb
- parallel-settle-c3 | Deterministic parallel Phase-4 settlement (TORUS_PARALLEL_SETTLE) | default-on | compiled default since 9.13 | 4c4e8b9c, 8f131571
- settle-via-position-cache | Settle fills through PositionCache, no per-fill balance flushes; write-back native-balance cache | merged | Jul | 349a1300, 3071831c
- settle-defer-results-d | Settle pass B defers placed-result writes and scatters them in one sequential pass after the market loop | default-on | DMTE +9.1% matched/s, -0.5% CPU-s/1M | 95e2d48e, 4156a83b, 219af113
- sell-top-up-stitch-list-t | B-blind sell top-ups from a stitch-time candidate list with per-sender run cache | default-on | T +2.0% matched/s single-flag | 6d1100b8, 4156a83b
- b-blind-sell-top-up | Replace same-batch sell bound with B-blind non-pool top-up; failures by original index | merged | Gate 2 1.097x main at 300m | 31cea69 (section 19)
- m1-margin-phase | One margin entry per sender, per-batch market table, sums carried across stable marks | merged | +17% at 300m | 49df3ebe, b9959e23
- no-merge-position-flush-l22 | Pass B keeps per-market PositionCaches as runs; one sorted flush, no merge_disjoint | default-on | exec CPU/1k 0.957, blk/s 1.095 | dbe14b21, ddf41758, 5013cc79
- streamed-in-order-settle-l21 | Fused match+plan jobs publish per market; exec thread runs pass B in market order while pool works | in-progress | deep exec CPU 1.044 fails rule; blk/s 1.206 | origin/perf/l21-streamed-settle 39f8c7f3, merge/s136-batch10 451b4074 (owner-list)
- l21-batched-wakeups | Batch the exec thread's wake-ups in StreamRx::take | measured-no-gain | cost is not wake-ups (+~2 ms/call) | 39f8c7f3, 0c6f35c9 (section 119)
- settle-pass-b-parallel-shard-apply | Parallelize pass B's serial canonical apply with deterministic shard order | proposed-not-done | pass B ~43 ms/blk at 300m | M200k section 17 via prior_history
- pass-a-hot-market-trader-shards | Split one heavy market's pass-A plan into trader shards | rejected | no gain on uniform bench shape | docs/s58-workflow-arch run2 hot-market-skew
- p22-batch-cache-flush | write_cf_raw_many: one lock and CF lookup per batch for Position/BalanceCache flush | rejected | cache flush +2.85 ms/blk (+24%); reverted | b7e66ce5, 3fe595d3
- streaming-cache-flush | Stream cache rows into the overlay instead of per-row BTreeMap inserts | proposed-not-done | cache_flush 12-26 ms/blk | R section 37.4 via prior_history
- c2-holder-index-fixes | Holder index moved only on key appear/disappear; holder sets as foldhash HashSet | merged | apply per 1k fills -36% | d7bd1c36, 98d0f081
- position-v2-savings | Reserve batch PositionCache before pass B merges; update cached position in place | merged | pass B per 1k fills -11% | 5d197a3b, 37b28dd6
- adl-work-budget | ADL under a per-block work budget W=100,000 with drain caches | merged | S=750 freeze 11.6 min -> live | a746c408, 6a25e201
- adl-dirty-check | ADL ranking dirty checks from a drain-local trader set | merged | block B 149.6 -> 128.8 ms | 87a05374
- adl-optional-speedups | adl_rank sort, balance reads, resident-position binary search | parked | not now (18c) | ebbb3cdd
- liquidation-sweep-moving-marks | Skip unchanged-health traders in the liquidation sweep (P2-4) | parked | 10.7 ms walk 0, 24.7 ms walk 10 | P2 rows 36-37 via prior_history
- cadence-invariant-liquidation-scan | Liquidation budget = rate x header time delta (cap 2,048) instead of 2,048 per block (8k L1.3) | proposed-not-done | est matched/s 0 to +1.5% | 8k-explore-ideas L1.3
- liq-scan-parallel-prevalue | Parallel liq_view pre-valuation | rejected | +0.6-0.8% est, below CI | s123 list (docs/s58-workflow-arch)
- liquidation-bounded-seeks | Bounded seeks instead of whole-CF loads; no valuation reads without marks | merged | s517 review H3; no reads without marks | f21b89f4, 473a0377
- phase5-step2-typed-users | Hot writers mutate Users in place, write-through cold writers, cut in-place apply and drain | measured-no-gain | exec-thread +5% halves blk/s; p50 18->92 ms | origin/feat/item6-phase5-step2, 43b954ba (section 91) (owner-list 9m)
- phase5-step3-classic-save-dirty-levels | Classic save writes rows from the level dirty set in one pass | parked | PARKED s128 | origin/feat/item6-phase5-step3 bfab6ea0 (owner-list)

## state/storage/rocksdb
- hotstuff-da-own-rocksdb | HotStuff store and DA store in their own RocksDB (71.8 option A) | merged | in-window DB wait -91%; p99 1,575 -> 1,370 ms | 1ca719f1, a44000e0 (section 74)
- consensus-kv-split-aug | Separate consensus RocksDB on the exec-bound Aug branch | measured-no-gain | 29.5k/29.1k vs 31.2k (n=1) | s64 section 3 via prior_history
- t53-manifest-consensus-db | Commit manifest in the consensus DB, CommitPersistWriter off-thread header/body write, HeadPublisher | merged | gap G 452 -> 100 ms; in-window p99 917 -> 485 | 173f9a12, dfb55f70 (section 115)
- rocksdb-pipelined-write | TORUS_ROCKSDB_PIPELINED_WRITE compiled default on | default-on | view -9%, matched/s +7-13% (cap 25) | 6f61f057, 8f131571
- rocksdb-wal-cap | max_total_wal_size 512 MiB, later 2048 MiB | default-on | restart open 20.5 -> 7.9 s; 2048 CPU -6.3% | de16eb37, 8c5ca921
- rocksdb-wal-compression | TORUS_ROCKSDB_WAL_COMPRESSION zstd knob | merged | default off; re-decide after Phase 3 | fa8b646f, 73503705
- append-cf-codec | TORUS_ROCKSDB_APPEND_CF_CODEC zstd1/lz4 for append-only CFs | measured-no-gain | lz4 -1.27 CPU-s/1M but +48% SST | 4a3f6546, 233c026a
- state-write-wal-off | Disable WAL for the flush state batch, recover by replay | parked | state_write DB -35%, matched/s flat | r9 79ea483 via prior_history
- book-cf-sst-4mib | Book CF target_file_size 4 MiB plus per-market tombstone compaction threshold 64 | default-on | getOrderBook 131 -> 62 ms; matched/s neutral | 020f0b5d, 8e3326c6
- consensus-meta-8mib-write-buffer | 8 MiB write buffer for cf_consensus_meta | rejected | propose_build growth unchanged; kept as hygiene | 7e67bd10
- rocksdb-node-options-tuning | Tune RocksDB options for the node state DB; compaction and bytes_per_sync knobs | merged | Jun/Jul | 7cdd22b3, 06de7326
- low-pri-background-trade-writes | Chunked low-priority background trade writes plus write-controller knobs | merged | Aug | 2eb62ce5
- native-trie-maintenance-off | Skip per-block native trie maintenance (root has no production reader) | default-on | +31% matched/s, -26% CPU at 300m | e4273ec2, db6c9de5
- async-native-trie-maintenance | Move native trie maintenance to an async worker | rejected | nothing reads the root; skipped instead | f186cc4b
- native-root-cache-bucket-hash | In-RAM native-trie node cache, parallel dirty-bucket rehash (8 threads), bounded member cache | default-on | bucket hash +10.8%; dormant with trie off | 4de37116, 4bf3e804, e44a38ca
- incremental-state-roots | Incremental EVM root (reth overlay) and native bucketed-Merkle root from overlay dirty set | merged | flat vs linear full scan | 64c46193, 4efa2278
- block-tree-pruner | Retention pruner for the HotStuff block tree in cf_consensus_meta | merged | fixed +45%/50k-block drift | f99d07a5, 0a9a241e
- bounded-prefix-scans | Bound every RocksDB prefix scan by the prefix successor; overlay walks only the prefix | merged | s89 fix A | b529aee5, 6cbd8129
- fsync-wal-at-commit | Fsync WAL once at the commit boundary (TORUS_SYNC_WAL_ON_COMMIT, gated) | merged | Task B, gated toggle (Jul) | bbc6a118, b1dbb971, origin/feat/commit-fsync-durability
- xfs-node-data | Node DBs on a separate XFS filesystem | measured-no-gain | dip unchanged, throughput in noise | s73 doc via prior_history
- vm-dirty-bytes-raise | Raise vm.dirty_bytes to 2 GB / 512 MB for writeback | rejected | 0 s in balance_dirty_pages; p99 in noise | section 71.4-71.5 via prior_history
- parallel-boot-book-load | Rebuild and boot-verify order books per market on worker threads | merged | load_books 4.5 -> 3.1 s | d6528ec4
- restart-shortening-knobs | Start networking before replay, 256 MiB WAL, avoid_flush_during_recovery | parked | not measured | s74/s75 docs via prior_history
- per-order-row-book-storage | Per-order-row book persistence, save/flush O(touched) (TORUS_BOOK_ROWS modes) | merged | consensus-visible; mode 3 not default | 9c5e23ec, origin/perf/deep-book-storage 92fa3aca
- delegations-scan-index | Index staking delegations instead of full CF_STAKING_DELEGATIONS scan per block | proposed-not-done | scan once fee share > 0 | R Open via prior_history
- staged-kv-byte-arena | Pack staged consensus KV writes into one byte arena; owned overlay values without copying | merged | s61 bundle | d73320e4, bdc335d1

## checkpoint/hash
- phase3-coalesced-checkpoints | State rows coalesced into a layer, written every 15 s on validators | default-on | CPU-s/1M -9.72%, writes -54.8%, matched/s +8.8% | dc8116d5, d3cba5e2
- checkpoint-interval-5s | TORUS_CHECKPOINT_INTERVAL_MS 5,000 vs 15,000 | measured-no-gain | RSS -0.3 GB, CPU +1.1%, throughput tie | eaeb0c8b (section 55), 926000e9 (section 87)
- checkpoint-size-cap-50mb | TORUS_CHECKPOINT_MAX_BYTES 50 MB (Z2) | measured-no-gain | W_db -72.7% but blk/s 15.91 -> 5.79 | 3e5ae439 (section 89)
- checkpoint-interval-60s | Fewer, larger checkpoint windows (60 s) | proposed-not-done | never run | section 66.6 via prior_history
- stagger-checkpoints-across-validators | Per-validator offset so at most one node writes a checkpoint at a time | proposed-not-done | aimed at in-window p99 step | section 66.6 #4 via prior_history
- checkpoint-batch-slicing-q6-a1 | Write the ~200 MB checkpoint batch as several WriteBatches; stage-split timers built | parked | A1 parked behind T5.3 attribution | origin/perf/q6-ckpt-slicing 90b34e6f, e6cf1090 (owner-list)
- p3-merge-wait | Exec joins only the small part of the previous layer merge | merged | settle wait 23.4 -> 3.1 ms; blk/s +11.3% | b32ca313, 32dfc8b1
- layer-merge-early-handback | Layer merge hands L back first; frees on torus-ckpt-drop off the exec thread | merged | untimed residual explained | 97cf1d07
- defer-book-blobs-layer-merge-db | Merge Classic book blobs into the checkpoint layer in the deferred part | measured-no-gain | DB +0.95% n.s.; flag off | 4fb841ed, 495d1de8, 3cb143ab
- p3-proto-books-only | Checkpoint only Classic book blobs (throwaway prototype) | measured-no-gain | -2.98% CPU vs -4.99% for all rows | proto/p3-coalesce 7f784cbd, 4e7fa146
- coalesced-classic-book-save-l31 | Skip per-block whole-book encode; encode each dirty book once at checkpoint window close (hash off) | default-on | deep blk/s 3.50 -> 15.23, matched/s 1.137 | 54fbc7f6, 52efa121 (section 117)
- window-committed-books-l61 | Hash and save Classic book blobs only at %100 attestation heights (consensus-changing) | in-progress | deep B/A 0.997, B/C 1.167 | origin/perf/l61-window-books 9930fc60, merge/s136-batch10 (owner-list)
- running-hash-v2-classic-books | Running hash absorbs per-block order-row deltas instead of whole-book blob (8k L3.2) | rejected | 0 gain; weakens fork detection | 8k-explore-ideas L3.2
- window-commitment-all-cfs | Attestation-window state commitment for all hashed CFs (8k L6.2) | rejected | premise false: flush_all not caused by hash | 8k-explore-ideas L6.2
- running-state-hash | Running state hash in the flush batch, digest overlapped with batch build, validator attestations | in-progress | -2% n.s.; on at genesis in batch 10 | 8166a741, 2c8d3279, 970c5f13 (owner-list)
- book-mode3-default | TORUS_BOOK_ROWS=3 (chunked level authority) as default | rejected | Classic -8% CPU; reverted to Classic | f80ef36c, 1d5d7bd0
- mode3-checkpoints-b0 | Book modes 2/3 run with checkpoints, book save on E | measured-no-gain | shallow matched/s 0.911; blk/s 13.65 -> 3.99 | origin/perf/mode3-checkpoints-b0 e32d4612, c54c0123 (section 104) (owner-list b0)
- mode3-b1-write-half-on-w | Modes 2/3 with checkpoints keep the book save's write half on W | measured-no-gain | CPU-s/1M +13.3% shallow; M15/CL21 0.963 | origin/perf/mode3-b1 244a767c, 1f03d1a7, 7ad5418e (owner-list b1)
- classic-vs-mode3-head-to-head | Classic + L2.1 (CL21) vs mode 3 + b1 (M15) final head-to-head | decided | Classic wins both shapes; book mode Classic | perf/h2h-combined 29bfc705, af53295c (section 124) (owner-list)
- level-hash-sponge-cache | Level-hash sponge cache with staged seeding (TORUS_LEVEL_HASH_CACHE, 256 MB) | default-on | +81% matched/s in vivo (mode 2/3 era) | 42bf96a5, b4dc190d, 3a3a96d8
- depth-independent-level-hash | Keccak over per-64-seq-chunk digests (consensus-visible) | merged | mode 3 era | e2712916, a59dfaf9
- parallel-level-hashing | Parallelize mode-2 level hashing / journal drain across dirty books | merged | node-local, byte-identical | fec3cff3, b4afbe08
- level-rows-authority-3c | Level rows as authority plus hash-only mirror (3c) | merged | flush cut 6x; 13.7k avg matched/s (Jul) | 59f91881, 18add95e
- book-save-pass2-deferred | Book-save pass 2 deferred to the flush worker | merged | save_books on-thread 30.8 -> 8.3 ms | 124a842c
- book-save-pass1-offthread | Move level-digest drain (pass 1) off the exec thread | rejected | pass 1 reads live levels mutated by engine(N+1) | design-exec-pipeline 3.6 via prior_history
- async-post-flush-body-persist | Async post-flush body persist with adaptive parallel state-root threshold | measured-no-gain | async-post-flush neutral; ubench -22% | a4a924b1, 25643196, 724a1b15
- async-body-persist-off-exec | Move block-body persist off the exec thread (TORUS_ASYNC_BODY_PERSIST, Jul) | tried-inconclusive | Jul unmerged; T5.3 later did off-thread persist | 4af063ee on origin/perf/deep-book-storage
- commit-persist-reuse-wire-body | Consensus-thread commit persist as one WriteBatch of the encoded bin body | merged | +35.2% (cap 100 era) | 1adb76eb, 55da9796
- header-in-flush-batch | Fold the native-block header into the flush batch; no exec-thread small writes | merged | +21.7% (r5) | 602b88e1, 49f0130b

## serialization/wire
- bincode-consensus-serialization | bincode for consensus messages instead of JSON | merged | May | 6668fdfb
- exact-size-book-encoder-l33 | One size pass, presized Vec, fixed 97 B order records, bytes identical to borsh | default-on | save_books -15%; blk/s 1.086, p99 0.919 | 7d1c2510, 342c88a2 (section 105)
- parallel-classic-book-save-b | Encode Classic book blobs as per-book exec-pool jobs, puts serial ascending | measured-no-gain | +3.5% CPU-s/1M for +0.09% matched/s | exp/archy-s58-parallel-classic-book-save 8525822b, 9bb98b10
- rpc-block-body-raw-json | torus_getBlockBody serializes actions straight to JSON RawValue, not a serde_json::Value tree | rejected | rpc CPU real, no chain gain (2/3 lenses) | docs/s58-workflow-arch run1 network-da-3
- native-push-envelope-encoding | Encode native pushes into final envelopes, share push buffers, skip body encode for hash-only pushes | merged | s61 bundle | cdd98715, 78659525, 0bfef342
- s61-scratch-reuse-bundle | Reuse hash/serialization scratch buffers, borrowed compact encoding, pinned DA reads, pre-sized vectors | merged | not measured singly; engine 10.6-13.4 vs 16-18.7 ms/1k | 1364d747..778f0599, a75534f0

## threading/runtime/scheduling
- cpu-pinning-nodes-bench | Whole-process CPU pinning for nodes and the bench client | measured-no-gain | -5.7% | origin/perf/s75-cpu-pinning 17565a36, b9b2fb46
- hotstuff-cpus-pinning | Pin the HotStuff consensus thread (HOTSTUFF_CPUS) | rejected | runqueue wait 19-22 -> 32-41 ms/blk | s64 section 3 via prior_history
- bounded-pools-pinning | Bounded worker pools with pinning | rejected | -67.7% | M200k section 6 via prior_history
- bulk-thread-nice | nice +10 on gossip and ingress verify pools so exec wins CPU | rejected | gain refuted (fatal) | docs/s58-workflow-arch run1 locks-threads-tail-2
- verify-pools-parking-executor | Run gossip-verify and single RPC verifies on a parking executor, not rayon's yield-and-steal loop | proposed-not-done | s58 r3 kept 3/3 lenses | docs/s58-workflow-arch run2 r3 pool-idle-steal-cpu
- exec-worker-count-knobs | A/B TORUS_MATCH_WORKERS / TORUS_SETTLE_WORKERS (32 threads per node on one 32-thread host) | proposed-not-done | never A/B'd on archy | prior_history (docs/s58-workflow-arch)
- exec-queue-depth-16 | TORUS_EXEC_QUEUE_DEPTH=16 control arm (64-deep exec channel) | parked | dropped with campaign B | a7daa18e, 3a8c363f
- hotstuff-park-idle-threads | Park idle HotStuff threads instead of busy-spinning | merged | Jun | b136d7c8
- dedicated-rpc-runtime | Dedicated RPC tokio runtime, gossip flooding off, no sig re-verify | merged | May | 66ddef44
- nonblocking-block-sync | Block sync on a worker thread, drain sync batch per tick | merged | Apr | aa6d1272, 57a8255c
- cheaper-delta-drop-offthread | Cheaper delta/drop moved off the exec thread (cut 5) | merged | item 6 cut 5, part of Gate 2 | ef11c4e3

## build/compiler/allocator
- jemalloc-allocator | tikv-jemallocator as torus-node allocator (default feature) | default-on | blk/s +14.9%, p99 -7.2%, CPU +2.66% | 88ddc4c2, 655e874a
- jemalloc-decay-tuning | MALLOC_CONF dirty_decay 1000 / 0 | rejected | -7.1% / -30.7% matched/s | 45286113 (section 73)
- glibc-vs-jemalloc-preload | LD_PRELOAD jemalloc vs glibc on Phase 3 step 2 | measured-no-gain | CPU +4% at equal fills; parked then adopted | f7873c02 (section 51)
- release-lto-cgu1 | Fat LTO (thin as cheaper arm) and codegen-units=1; no [profile.release] exists | proposed-not-done | est +1.5-3.7%; gain lens says noise floor | docs/s58-workflow-arch run2 build-profile-codegen-1 (owner-list opt-level A/B parked)
- target-cpu-pgo-bolt | target-cpu=native, PGO, BOLT for the node binary | proposed-not-done | never measured (s64 section 5) | prior_history (docs/s58-workflow-arch)
- rocksdb-hw-crc32c | Build RocksDB with -msse4.2 -mpclmul so CRC32C uses hardware (LOG says not supported) | proposed-not-done | targets ~0.6 s checkpoint batch write | docs/s58-workflow-arch run2 build-profile-codegen-2
- binary-layout-effect | Two binaries of the same code differ (8kd uniform rise) | tried-inconclusive | likely code layout, untested | 5d7a8d84 (section 105)
- binary-gap-9l | Check binary gap 53a6d83c vs 29bfc705 | queued | HOLD until owner go | 8663c81d (owner-list 9l)
- build-infra-parked | opt-level A/B, test-binary merge + hakari, sccache cap | parked | owner-parked | owner-list.md (owner-list)

## harness/bench
- bench-in-flight-cap-budget | --max-in-flight 4 plus open-order budget 900 | merged | open-limit rejects 40% -> 0% | 0d6f2c38, section 21
- bench-commit-tail | IN_FLIGHT_TAIL=commit releases slots on the committed body | merged | stall 33.5 -> 2.0 s | 29b1592f, 8efcb07a
- bench-tail-read-ahead | --in-flight-tail-depth N ordered read-ahead | tried-inconclusive | +4.5-6.5% at 10m; not on main | feat/bench-tail-depth 7733a0e1, 72567a57
- bench-release-timing | Release timing instrumentation in bench-throughput | tried-inconclusive | unmerged | origin/feat/bench-release-timing, origin/feat/summarize-release-timing
- bench-batch-jsonrpc-50 | Batch JSON-RPC submissions, 50 actions per HTTP call | tried-inconclusive | May branch unmerged | origin/cte-architecture f660359f
- bench-all-validators-rpc512 | Bench ingress over all 3 validators; node RPC max connections 64 -> 512 | merged | small offered-load gain | 8b8432c7, adc583ec
- bench-conc-rate-knee | CONC 512 more in-flight requests; lower offered rate 270 | measured-no-gain | CONC 512 no more load; rate +8.6% only overloaded | s76 ingress doc via prior_history
- standard-bench-load | One standard bench load and exec-path merge rule (exec CPU per fill, blk/s, p50/p99) | decided | ABBA n=4 CI ~+-1.3% | 12534647, 47c5f6fb
- bench-production-defaults-gap | Bench turns off custody and anti-spam vs production | decided | custody off by default now | docs/s58-workflow-arch run2 production-default-hidden-costs
- one-down-baseline-9k | One-validator-down baseline before TC / dead-leader | queued | HOLD until owner go | 8663c81d, bc408dab (owner-list 9k)
- deep-book-rerun-9n | 8f deep-book re-run after batch 10 | queued | HOLD | 2229b3ab (owner-list 9n)
- phase5-exec-profile-9m | Item 6 Phase 5 exec profile | queued | HOLD | 8663c81d (owner-list 9m)

## rpc/streams
- rpc-read-write-isolation | RH5: dedicated read RPC server separate from write path | tried-inconclusive | Jul unmerged | 92050576 on origin/perf/deep-book-storage
- trade-streams | newTrades/userFills websocket streams fed from fills; extras recorded only while subscribed | merged | equal throughput (s80) | 7ebcfa37, e80d08ad
- getorderbook-from-memory | Serve getOrderBook from the in-memory book | proposed-not-done | before mainnet | R Open via prior_history
- getorderbook-topn-gas | 0x0800 getOrderBook top-N bound with per-level gas via price-level aggregate rows | tried-inconclusive | Jul unmerged; later 64 levels per side | origin/feat/precompile-0800-topn-gas 39c74a1f, fc1fb25a
- rpc-scan-budgets | Bounded public read RPC scans (TORUS_RPC_MAX_SCAN_ROWS 250k) | merged | safety | 657b52e4
- rpc-small-encode-wins | getUserLimits streams rows; reuse batch ack JSON scratch; env max response size | merged | node-local RPC CPU only | 0e4f2b43, 8a343588, 6c3e1f23
- rpc-freshness-hl-way | RPC freshness the HL way | parked | Phase 5 decision | 5e6f331a

## other
- evm-fast-path-option0 | Item 7 option 0: EVM blocks on the pipelined fast path (TORUS_EVM_FAST_PATH) | default-on | EVM load blk/s 3.53x, p99 0.496, matched 1.079 | 7245a876 (section 107)
- evm-lanes-n11 | EVM txs only at height % N (N=11), header gas limit, HL small/big EVM blocks with lanes | merged | EVM load free on std and deep | 853fed28, cfa35ed1, 639ab185
- option-s-raise-evm-n | Q12 option S: raise EVM cadence N under EVM load | rejected | E11 0.913 matched/s; superseded by option 0 | b6508e15, ee23ff21 (owner-list)
- evm-onchain-mempool-fees | Onchain EVM mempool plus per-block fees | parked | Phase 5 decision | 5e6f331a
- place-order-batch | PlaceOrderBatch native order batching | merged | Jun | b58f858f, 8c42f278
- after-testnet-perf-items | Co-location, ingress dispersal, Block-STM | parked | after testnet | owner-list.md (owner-list)
- trial-net-wan-items | Trial net, PE over WAN, WAN push threshold, archive node | parked | owner decisions | owner-list.md (owner-list)
