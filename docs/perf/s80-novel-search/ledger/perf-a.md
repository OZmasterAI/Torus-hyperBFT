# perf-a ledger: ideas from docs/perf bench-queue.md and pre-2026-09-20 perf docs

Format: slug | mechanism | status | evidence | source. Read-only extraction from main eb993e3f docs (s80 novel search, part perf-a).

## consensus

- epoch-fast-path-f | TORUS_EPOCH_FAST_PATH: h%100 epoch-boundary block takes pipelined path instead of barrier (forced checkpoint + W drain) | default-on | boundary gap 500-630 ms -> ~1 ms; on PC: blk/s +88.6%, p50 -73.6% | bench-queue Q1, 5b / sections 75-76, 84, efe9aa47, main 3429855d
- qc-to-advance-subtimers | per-step qc_advance timers (commit_feed_app = decode + manifest persist + blocking exec send 94-96%) | decided | commit_feed_app 8-13 ms per block; others 1.6-1.7 ms per view | bench-queue 9f / section 118, perf/qca-timers-*
- commit-feed-app-blocking-exec-send | commit_feed_app incl. blocking enqueue_for_execution send dominates qc_to_advance | proposed-not-done | 94-96% of qc_to_advance; qc-timers parked | bench-queue 9f / section 118
- propose-defer-campaign-b | propose-defer: defer proposal until exec catches up (s123 fourth experiment) | parked | dropped s126; revisit rule in item6-phase5-impl.md | bench-queue Done/dropped, Q4
- committed-block-interval-floor | commit path floor 151-295 ms per committed block under load vs 37-83 idle; exec starved (busy 0.53-0.64) | decided | max 6.6 committed blk/s at cap 25 | block-latency-campaign-2026-08-20 section 6
- consensus-timeout-attribution-under-pipeline | attribute view timeouts (18-76 per 120 s cell, 163-180 per 300 s) and 500-2300 ms commit p95 under load | proposed-not-done | ranked #1 backlog | block-latency-campaign-2026-08-20 section 7 #1; matched-200k-campaign section 11 #8
- epoch-validator-set-staking-writer | consensus-thread epoch_validator_set_updates CF_STAKING writer: pre-existing consensus-visible hazard | proposed-not-done | needs own design | block-latency-campaign-2026-08-20 section 7 #11 (F8)
- hotstuff-blocked-time-variance | throughput variance = hotstuff-algo blocked time (71-96% of interval); system idle and slow | decided | slow cell all-node on-CPU 0.79 cores vs 1.46 | blown-cell-diagnosis-2026-09-17 sections 4-5
- block-build-critical-path | leader block_build (231 ms) dominates view; follower proposal-arrival wait mirrors it | decided | propose_delay 410 ms of 745 ms view under load | blown-cell-diagnosis-2026-09-17 section 7
- hotstuff-idle-cpu-spin-fix | fix/hotstuff-idle-cpu-spin: stop idle spin of hotstuff thread | merged | S391 | blown-cell-diagnosis-2026-09-17 section 9
- hotstuff-thread-per-order-cost | ms per full block ~3.2x hotstuff thread CPU+rq; ~4 us per order on consensus thread | decided | ~1.7 ms per action | cap100-gap-attribution-2026-09-10 sections 4, 6
- write-group-commit | write-group (s55 next-steps item 1) to cut consensus-thread per-order persistence | proposed-not-done | none | cap100-gap-attribution-2026-09-10 section 5
- off-thread-validate | TORUS_ASYNC_VALIDATE: speculative validate_block body work (DA reconstruct, attest) off the HotStuff thread on arrival | parked | exists, default off; cap-100 A/B never resolved | cap100-gap-attribution-2026-09-10 section 5; consensus-thread-attribution 5 #3; blown-cell sections 4, 6; app.rs
- next-leader-insert-persist-constant | QC waits on next leader's single thread busy in insert_persist; ~68 ms size-independent constant | decided | block_build 13+0.21/action, insert_persist 48+0.22/action ms | consensus-thread-attribution-2026-08-21 section 2
- block-tree-writes-batch-low-pri | batch 6-9 per-view block-tree writes on leader or set_low_pri on non-critical ones | proposed-not-done | propose_build - block_build 20-25 ms | consensus-thread-attribution-2026-08-21 section 5 #1
- consensus-kv-byte-arena | consensus KV write batch: keys/values in one owned byte arena, one RocksDB write | parked | built+159 tests, never benched | load-improvements-2026-09-19, d73320e
- consensus-view-legs-trim | trim consensus view legs (nosplit) | rejected | -6% vs same-binary control | matched-200k-campaign-2026-08-18 r2
- header-folded-into-flush-batch | persist_block_header folded into exec flush batch; body/DA writes decoupled | merged | +21.7% 120 s; 300 s 40.9k | matched-200k-campaign-2026-08-18 r5, 602b88e, 49f0130
- conflicting-committed-chain-failstop | val2 ConflictingCommittedChain after body fetch exhausted 9 retries | proposed-not-done | 1 of 108 cells; pre-existing | matched-200k-campaign-2026-08-18 sections 6, 11 #9
- cached-proposal-hashes | cache signed-action hashes of pending proposals lazily (recomputed per produce, dispatch, commit match) | tried-inconclusive | 51.6k vs 49.2k n=1; latest-base rerun 27.7k, not reproduced | throughput-campaign-2026-09-18, perf/cached-proposal-hashes 388f7cd/3495652; load-improvements 984e6ef; cap100-gap s55 item
- viewbound-header-votes | bind header votes and proposal-status updates to header's matching local view | merged | 92 HotStuff tests; rebaseline 19.6k slower (unattributed) | throughput-campaign-2026-09-18, 09f3449
- future-buffer-occupancy-release | release future-message buffer occupancy on delivery/expiry, evict only actual deficit | merged | 92 tests | throughput-campaign-2026-09-18, b60097a
- gossipsub-resubscribe-watchdog | stale-connection kick + 5 s/1 s mesh watchdog for gossipsub resubscribe wedge | merged | -0.86% CI spans 0; benefit unmeasured | ozarchy-antispam-item6-pf1 section 58, fix/mesh-watchdog-resubscribe e3bb7cca
- explicit-gossipsub-validator-peers | explicit gossipsub peers among validators to avoid resubscribe stall after restart | proposed-not-done | stall 56-75 s once per cell | ozarchy-antispam-item6-pf1 Open
- graceful-sigterm-shutdown | graceful SIGTERM shutdown (replay 0 blocks) | merged | exit 7.0 s mean / 10.7 s max | ozarchy-antispam-item6-pf1 section 52, 219f0c20
- consensus-gossip-rate-limiter-drops | per-author consensus gossip rate limiter (50/s burst 50) drops leader header after fast empty views | decided | 9/10 and 16/16 lost-proposal events aligned | ozarchy-antispam-item6-pf1 section 77, 857b9770
- direct-fan-header | ALLfan: direct fan of proposal header to bypass gossip limiter | rejected | -1% matched, +3% CPU; 14.1k mirror-copy drops | ozarchy-antispam-item6-pf1 section 77
- rate-limiter-drop-counter | torus_consensus_rate_limited_dropped_total metric | merged | none | ozarchy-antispam-item6-pf1 section 77, 857b9770

## pacemaker/timeouts

- header-resend-250ms | TORUS_HEADER_RESEND_MS 3 s -> 250 ms: leader re-sends proposal header, covering headers dropped by the gossip rate limiter | default-on | ALLrs core timeouts 47 -> 3; 0 no_proposal in 600 s cells | bench-queue s126/s128 note; ozarchy-antispam-item6-pf1 sections 77-78
- commit-lag-backoff-cap | TORUS_COMMIT_LAG_BACKOFF_CAP 2 vs 8 | measured-no-gain | null result 166 vs 168 ms; only timeout count 23 vs 41 | block-latency-campaign-2026-08-20 section 6
- timeout-cliff-amplification | exec ms 2x -> ms/view 3x -> 500 ms timeout cliff -> dead views -> nonce eviction; bimodal | decided | timeout 6-16% -> 19-29%, views/blk 1.16->1.6 | blown-cell-diagnosis-2026-09-17 section 2
- timeout-base-1500-ab | raise genesis timeout_base_ms 500 -> 1500 at cap 100 | measured-no-gain | views/blk 1.5->1.07 but ms/view absorbs it; matched +5% +-40% | cap100-gap-attribution-2026-09-10 section 6; blown-cell-diagnosis section 4
- view-deadline-rebase-s395 | per-view deadlines 500 ms apart from epoch start, rebased to now+500 ms when >2 slots ahead | decided | budget oscillates 500 ms-1 s | cap100-gap-attribution-2026-09-10 section 4
- proposer-exec-watermark | TORUS_PROPOSER_EXEC_WATERMARK: yield proposal slot on pending headers + deferred bodies | proposed-not-done | off in every cell | design-exec-pipeline-2026-08-20 section 4.1
- exponential-pacemaker-backoff-stall | pacemaker exponential backoff (update_view, stall_multiplier) lengthens waits during stall | decided | no change justified | stall-584-diagnosis-2026-09-18

## dissemination/DA

- shard-custody-c | TORUS_SHARD_CUSTODY=1: validators keep erasure shards custody per block | rejected | -21.6% matched/s; 140 ms per validate; ~1.7 GB shards per node per cell | bench-queue step 4 / section 82
- push-manifest-threshold-512k | TORUS_HASH_ONLY_PUSH_THRESHOLD 8 MB pin -> 512 KB code default: push hash manifest + pull instead of full bodies | default-on | matched +4.3%, CPU/1M -5.9%, full-body volume 10.1->0.12 GB | bench-queue 7b / section 86
- custody-model-n2-over-k | custody per-shard proof work grows ~n^2/k at N=11; lazy serve-time shards for T3.1 | proposed-not-done | model 95-145 ms per block per node | bench-queue Items Q10
- rh4-shard-count-fix-option-d | GC keeps max(old,new) n shards for 64 blocks after validator-set change | parked | waits for T3.1 (owner list) | bench-queue Items Q10
- dissemination-sync-fallback-residue | 2-14 sync_fallback, 0-5 exhausted per node every cell | decided | campaign-wide residue | block-latency-campaign-2026-08-20 section 8
- quic-send-queue-full | QUIC Send Queue full warnings in starved cap-100 cell | proposed-not-done | unexplained | blown-cell-diagnosis-2026-09-17 section 8
- udp-buffer-8m | 8 MB UDP socket buffers (udpbuf8m control) | tried-inconclusive | cap 25 pair 23.9k vs 4.2k | blown-cell-diagnosis-2026-09-17 section 5
- body-before-expiry | TORUS_BODY_BEFORE_EXPIRY: when body/justify fetch pending, poll up to 8 ordinary-channel envelopes before retry expiry (receiver gap fix) | tried-inconclusive | live OFF 27.2k / ON 35.7k, both REJECT dissemination | body-before-expiry-experiment-2026-09-18, 110ebfc; throughput-campaign OFF/ON pair
- body-fetch-trace | TORUS_BODY_FETCH_TRACE: admission/serve/receive clocked records for body fetches | merged | diagnostic only; late handler processing ~2 s after expiry seen | body-fetch-queue-time-diagnostic-2026-09-18
- body-send-stages-trace | TORUS_BODY_SEND_TRACE: outbound enqueue/dequeue/encode/send stage stamps for BlockData req/resp | merged | first response admitted 443 ms after expiry | body-send-stages-diagnostic-2026-09-18
- body-fetch-dedicated-protocol-reroute | route body requests over dedicated block-data protocol instead of ordinary consensus transport | proposed-not-done | needs separate validation review | body-fetch-queue-time-diagnostic-2026-09-18
- skip-da-remirror-on-produce | skip DA re-mirror of body on produce | proposed-not-done | none | cap100-gap-attribution-2026-09-10 section 5; cap200-loadwin-repeats
- da-batch-read-multiget | one flush_da_mirrors + one RocksDB MultiGet per compact block instead of 25+25 | merged | da_reconstruct -37%; view -2.5% | consensus-thread-attribution-2026-08-21 section 4, a3a476f
- shared-arc-fanout-buffer | share one immutable Vec across peers, reconnect retention, retries in native fanout | parked | built+tests, never benched | load-improvements-2026-09-19, 7865952
- push-decision-exact-size | compute exact serialized size before choosing hash-only push; skip discarded body buffer | parked | built+tests, never benched | load-improvements-2026-09-19, 0bfef34
- ban-persistence-serial-worker | lazy serial ban-list writer with one replaceable pending snapshot and atomic replace | parked | fixed ban-file race; never benched | load-improvements-2026-09-19, d3f667f
- da-pinned-value-decode | decode native DA batches from RocksDB pinned values, no temp Vec copies | parked | built+tests, never benched | load-improvements-2026-09-19, fe83745
- da-write-bincode-scratch | reuse batch-local bincode buffers for DA body and shard custody writes | parked | built+tests, never benched | load-improvements-2026-09-19, c1e32b7
- da-serve-batched-16 | fetch raw DA response bodies in bounded 16-hash batches with fallback | parked | built+tests, never benched | load-improvements-2026-09-19, 778f059
- send-queue-full-log-payload | Send Queue full warnings print entire RawMessage payloads (91% of 1.66 GB logs); truncate logging | proposed-not-done | 1.513 GB of 1.661 GB logs | stall-584-diagnosis-2026-09-18
- stall-584-body-retrieval | height-584 stall: body-fetch exhaustion -> sync fallback -> queue full, views advance, commits stop | proposed-not-done | 81-84 s no-commit with 15.5k pending | stall-584-diagnosis-2026-09-18
- swarm-poll-trace | TORUS_SWARM_POLL_TRACE: wrap select_next_some poll, Pending/Ready counts, interpoll gaps; receiver stage records | merged | diagnostic only, 127 tests | swarm-poll-stages-diagnostic-2026-09-18; body-send-stages-diagnostic (fairness counters)
- swarm-biased-select-priority | outbound commands before swarm events in biased select may starve swarm polling | proposed-not-done | unverified; no fairness change authorized | swarm-poll-stages-diagnostic-2026-09-18
- authenticated-body-recovery | sync batch terminal cert, retry deferred bodies after synced parents, bind body metadata to authenticated header, genesis-body transport exception | merged | first ACCEPT cell 49,242.8 fills/s | throughput-campaign-2026-09-18, 9af0eea
- compact-body-push-64k | TORUS_BODY_PUSH_MAX_BYTES=65536 compact consensus-body push | proposed-not-done | exists, default off, never tested | throughput-campaign-2026-09-18
- exact-byte-da-reuse | default-off exact-byte DA reuse + producer-stage attribution | tried-inconclusive | OFF/ON pending at writing | throughput-campaign-2026-09-18, perf/proposal-attribution 0444b7e
- proposal-body-reuse-move | reuse/move the DA-mirror action vector into the proposal instead of a second deep clone of selected bodies | measured-no-gain | 30.9k vs DA-off control 38.3k; no promotion | throughput-campaign-2026-09-18, perf/proposal-body-reuse 2f1b832; load-improvements c250a5d
- da-ensure-flag | DA ensure (explicit durable DA mirror ensure) ON vs OFF in proposal path | measured-no-gain | 37,966 vs 38,274 fills/s; mirror 44-53 vs 48-54 ms | throughput-campaign-2026-09-18 proposal-storage pair
- outbound-command-queue-delay | pre-enqueue to dequeue p95 10-16 ms, max 164-521 ms on network command queue | decided | max 410 ms on a body response | throughput-campaign-2026-09-18 outbound diagnostic
- swarm-interpoll-gaps | swarm interpoll gaps max 452-504 ms, poll-call max 295-352 ms under load | decided | event-to-admission p95 46-52 us | throughput-campaign-2026-09-18 swarm diagnostic

## ingress/mempool

- dedicated-ingress-pool-s353 | dedicated 4-thread ingress pool (starvation fix) | merged | s353 'starvation FIXED' | blown-cell-diagnosis-2026-09-17 section 9
- nonce-window-60s-eviction | NONCE_WINDOW_MS=60s: aged bench backlog silently evicted | decided | tens of thousands evicted in blown cells | blown-cell-diagnosis-2026-09-17 section 2
- batch-1000-per-action-test | cap 25 with batch 1000 orders/action: is thread cost per action or per order | proposed-not-done | none | cap100-gap-attribution-2026-09-10 section 5
- mempool-remove-committed-hash-index | mempool remove_committed: replace two O(pool) scans under write lock with hash index | proposed-not-done | 9.3 ms per commit | consensus-thread-attribution-2026-08-21 section 5 #2
- mempool-expiry-nonce-index | exact nonce-ordered expiry index; visit only expired entries instead of full scan under write lock | parked | built+85 tests, never benched | load-improvements-2026-09-19, 9e9800e
- mempool-selection-sender-lookup | one sender-count entry lookup per selection loop | parked | built+86 tests, never benched | load-improvements-2026-09-19, 7ef271e
- mempool-size-gauge-under-lock | publish native mempool size under pool mutation lock (stale gauge fix) | merged | metric correctness; 83 tests | throughput-campaign-2026-09-18, 3bd7c01
- native-antispam | native anti-spam (per-IP RPC limit, cancel spam protection) | merged | -0.85% vs base at cap 400 | ozarchy-antispam-item6-pf1 section 1, feat/native-antispam
- exec-aware-admission-e2-e5 | exec-aware admission horizon 2-5 s | rejected | matched -33%/-51%, CPU 1.8-2.5x | ozarchy-antispam-item6-pf1 section 73.5
- exec-aware-admission-e10-e14 | exec-aware admission horizon 10-14 s | proposed-not-done | useful horizon between 5 and 20 s | ozarchy-antispam-item6-pf1 section 73.5

## signature/crypto

- exec-hash-once | one encoding per action for sort key + trust-cache key; skip ecrecover when all cache hits; cache EIP-712 domain separator | merged | exec CPU/1k 0.9546; blk/s 1.39x; p99 0.75; verify+sort -51% | bench-queue 8g / section 94, perf/exec-hash-once 70993fa9
- skip-empty-par-recover | skip par_recover pool job when every action hits trust cache | merged | 1.9-2.2 ms per native block of pure overhead | section 92 / 8e, part of exec-hash-once
- keccak-attribution | Keccak 12% of exec = two hashes of ~10.8 KB per action (trust key + sort key) | decided | 8.75-10.39 ms per native block | bench-queue 8e / section 92
- attestation-scratch-buffer | reuse call-local serialization buffer across signed actions in attestation | parked | built+tests, never benched | load-improvements-2026-09-19, 8b5d37f
- canonical-hash-scratch | reuse canonical signed-preimage buffers in DA batches and compact blocks | parked | built+tests, never benched | load-improvements-2026-09-19, a488a00
- trust-cache-hash-batch-buffers | reuse batch-local buffers for trust-cache hashing | parked | built+tests, never benched | load-improvements-2026-09-19, 105ba3a
- session-signing-ingress | session signing changes ingress authorization work (needs registrations) | proposed-not-done | declared distinct shape | matched-fill-units-and-generator-2026-09-18
- action-hash-preimage-scratch | compute_action_hash_with_scratch reuses caller buffer in DA put_batch/shards/CompactBlock | parked | correctness only, no bench | action-hash-scratch-2026-09-19, bb32ac5
- signature-work-29pct-node-cpu | signature work is 29% of node CPU (ingress verify) | decided | crab verify 1.22-1.27x per admitted action | ozarchy-antispam-item6-pf1 sections 6.4, 37.7
- l51-eip712-item-hash-memo | L5.1: per-thread 3-level memo of PlaceOrderItem Keccak sponge states keyed on preimage prefixes | proposed-not-done | ranked 0.47; CPU-only, hit rate bench artefact | bench-queue 8k / section 97 (8k top 5)

## execution/matching

- parallel-match-setup-m | TORUS_PARALLEL_MATCH_SETUP: match setup prepared on worker pool (s58 bundle) | default-on | +3.05% matched/s alone; phase_match 28 -> 16.9 ms/blk | ozarchy-antispam-item6-pf1 sections 76-79, merge 4156a83b
- evm-cadence-option-s | raise EVM block cadence N (E11/E33/E66) as stopgap for EVM barrier cost | rejected | E66 mempool rejected 41%, E11 0.913; superseded by option 0 | bench-queue 7 / section 85 (Q12)
- evm-fast-path-option0 | TORUS_EVM_FAST_PATH: pipelined EVM heights, no barrier, EVM trie step moved to flush worker | default-on | E-on/E-off matched 1.079, blk/s 3.53x, p99 0.496 | bench-queue 8l / section 107, feat/item7-option0 f8a45a2d
- l33-engine-timer-rise | investigate +2-3% engine ms per 1k fills with L3.3 (liquidation tail +20%) | tried-inconclusive | not scheduling artefact; likely code layout; n=1 | bench-queue 8u / section 105
- liquidation-step-timer | add torus_exec_liquidation_step_seconds; liquidation step = 99.5% of L3.3 engine tail | merged | 1.91 ms per block + 0.127 ms per 1k fills | bench-queue 8y / section 110
- item7-step2-bbo-rows | item 7 step 2: best bid/ask rows, one bbo write per dirty market per block, EVM lane routing | merged | Classic B/A 1.0115, mode3 1.0264; save_books +0.5 ms | bench-queue Q7 / item7-step2-native-ab.md, feat/item7-step2 c567b1b3
- evm-barrier-q5-cost | native vs native+EVM at N=11: barrier before every EVM block, exec idle ~0.5 s | decided | E/N 0.876 (-12.4% matched/s) | bench-queue Q5 / section 80
- big-block-mixed-load-sizing | big-block sizing with native + swaps when big blocks on | proposed-not-done | none | bench-queue Later
- phase1-actions-depth-drift | phase1_actions grows 13x over a run with depth; 31->89 us per cancelled order | decided | 58.4 -> 741.6 ms over 120 s cell | block-latency-campaign-2026-08-20 r4
- block-cap-100-operating-point | BLOCK_CAP 200 -> 100 (commit-lag backoff 8) as default operating point | tried-inconclusive | cadence 2.16x, chain 644->325 ms, matched -5.4%; needs 300 s confirm | block-latency-campaign-2026-08-20 section 6
- block-cap-25-50 | very small block caps 25/50 for sub-100 ms cadence | rejected | best 217 ms cadence; matched -25 to -53% | block-latency-campaign-2026-08-20 section 6
- engine-pipelining-two-in-flight | run engine(N+1) on N's in-progress state (market-sharded) | rejected | out of scope; engine is the state transition | block-latency-campaign-2026-08-20 section 7
- cancel-all-two-ended-compaction | batch cancel targets per level; stable two-ended compaction around largest untouched interval instead of repeated VecDeque::remove | tried-inconclusive | correctness only; guard 32-200 targets, >=1,024 orders | cancel-batch-2026-09-19, 96063cc; block-latency-campaign section 7 #8
- saved-bbo-hit-rate-b16 | B16 follow-up: measure saved_bbo hit rate (skip unchanged bbo writes) for item 7 step 2 | proposed-not-done | save_books per 1k fills unchanged (+0.0003 ms) | item7-step2-native-ab.md R2.5
- executor-presize-flatten | pre-size flattened actions and place-order indices from batch lengths | parked | built+19 tests, never benched | load-improvements-2026-09-19, 90e04d2
- matching-reuse-best-price-entry | reuse occupied best-price entry through matching, remove only afterward | parked | built+142 tests, never benched | load-improvements-2026-09-19, 2386826
- order-locate-binary-search-seq | locate orders in their level by binary search on seq | merged | part of mode 3 merge | matched-200k-campaign-2026-08-18, f5c66da
- block-cap-raise-200 | BLOCK_CAP 100 -> 200 bundle (orders/block 100k, 12 MB bytes, trust-cache 32k, 8 MB push floor) | default-on | +28% vs cap 100 control | matched-200k-campaign-2026-08-18 r3, d950a84; compiled default e44a38c
- empty-block-fill-under-backlog | fill empty blocks when backlog exists | proposed-not-done | dropped for time | matched-200k-campaign-2026-08-18 r3
- block-cap-300 | block cap 300 | rejected | inconsistent 44.1k / 35.3k | matched-200k-campaign-2026-08-18 r4
- phase1-actions-fastpath | fast path for phase1 actions (cancels) at 10 m | proposed-not-done | 391 ms/blk avg | matched-200k-campaign-2026-08-18 section 17 #9
- phase1-cancel-tombstones | tombstone cancels in phase 1 instead of eager removal | proposed-not-done | est -15-45 ms/blk | matched-200k-campaign-2026-08-18 section 11 #3
- fixedpoint-i128-fastpath | i128 fast path for FixedPoint math (avoid i256) | proposed-not-done | est -15-45 ms/blk | matched-200k-campaign-2026-08-18 section 11 #3
- exec-thread-trims | misc exec-thread trims | proposed-not-done | est -15-45 ms/blk | matched-200k-campaign-2026-08-18 section 11 #3
- order-index-map | order index map for markets track | proposed-not-done | none | matched-200k-campaign-2026-08-18 sections 11, 17
- market-sharded-execution | one exec thread per market shard with deterministic cross-shard settlement | proposed-not-done | only path to 200k per report | matched-200k-campaign-2026-08-18 sections 10, 18
- cancel-level-compaction-unrestricted | unrestricted cancel-all level compaction policy | rejected | regressed shallow/sparse microbench; confounds | throughput-campaign-2026-09-18, perf/cancel-level-compaction
- cancel-multilevel-gate | bounded allocation-free multilevel cancel compaction gate with scan/removal crossover | rejected | dense +1.67-1.84x but dispersed 0.915-0.987, fallback 0.938 | throughput-campaign-2026-09-18, perf/cancel-level-compaction ca30eea
- match-workers-cap-4 | TORUS_MATCH_WORKERS=4 vs 18 (settle 18) | measured-no-gain | 38,213 vs 43,619 fills/s | throughput-campaign-2026-09-18 worker screening
- engine-prep-workers-4-s60 | TORUS_PARALLEL_ENGINE=4 prep workers (s60 screen) | measured-no-gain | 34,203 vs 43,619; slow idle | throughput-campaign-2026-09-18 worker screening
- cancel-levels-lazy | group eligible deep queues per level, lazily allocate deferred output; shallow share one lookup | tried-inconclusive | accepted 38,143 vs rejected control 32,073; micro 2.0-3.5x deep | throughput-campaign-2026-09-18, perf/cancel-levels-viewbound f890e06
- deep-queue-concentration | two deep queues per side per market hold ~99% of resting orders | decided | 98.4-99.7% in snapshots | throughput-campaign-2026-09-18 sustained pair
- cancel-all-diag-attribution | TORUS_CANCEL_ALL_DIAG: book removal is 99.3-99.4% of cancel-all time; margin 0.24-0.33% | decided | 4.33M removed orders in 7,329 calls | throughput-campaign-2026-09-18, cad99c7
- cancel-deep-compaction-32k-gate-budget1 | deep eight-target gate at depth >=32768 + movement budget one queue length | rejected | dispersed ratios 0.728/0.908/0.948 | throughput-campaign-2026-09-18, c2526d9
- cancel-deep-compaction-32k-eligibility | deep eight-target eligibility only (four-length budget kept), default off | rejected | same-binary ON 16,451 vs OFF 17,528 (-6.15%) | throughput-campaign-2026-09-18, 5cf2d0c; cancel-batch (threshold variants)
- book-cancel-substage-split | split book cancel timing into index/position lookup, queue removal/compaction, journal/commitment bookkeeping | proposed-not-done | none | throughput-campaign-2026-09-18 route toward 200k
- book-queue-representation-change | change order queue representation (preserve FIFO, journals, commitments) for cheap cancels | proposed-not-done | phase1 64->573 ms first vs last 120 s | throughput-campaign-2026-09-18 route toward 200k
- safe-action-partitioning-across-markets | partition actions across markets respecting shared trader balances and deterministic merge | proposed-not-done | none | throughput-campaign-2026-09-18 route toward 200k
- sustained-600s-phase1-drift | 600 s cell: fills/s 46.5k first 120 s -> 25.5k last 120 s; phase1 65 -> 573 ms | decided | resting 1.11M -> 1.86M | throughput-campaign-2026-09-18 sustained baseline
- item6-phase2-p2-2 | item 6 Phase 2 step P2-2 | rejected | reverted 3fe595d3 | pre-merge-suites-ozarchy.md perf/item6-phase2 ab6f01dc
- shared-code-instruction-inflation | crab shared code 1.2-2.5x more instructions per fill, not lower IPC; perf annotate drain_book/cancel_all_many/insert_order/match_market | proposed-not-done | ~1.7 ms/1k | ozarchy-antispam-item6-pf1 section 9.2 / Open
- per-action-failure-records-c | C: per-action execution failure records | merged | no measurable cost; 147 failed actions per block | ozarchy-antispam-item6-pf1 section 13, 9195c32
- native-failures-canonical-bytes | map native_failures by original index instead of re-encoding actions with canonical_bytes | proposed-not-done | ~20 ms per block at 10 m | ozarchy-antispam-item6-pf1 section 17 / Open
- oracle-walk-reprice-cost | moving prices walk 10 bp cost | decided | ~4% matched/s; +13 ms engine per block | ozarchy-antispam-item6-pf1 sections 21-22
- cancel-all-book-scan | P2-1: per-trader cancel-all index so cancel-all visits only books the sender has orders in (vs scanning all books) | merged | scan 27 ms/blk; gate missed: -0.44 ms vs >=1.9 ms | ozarchy-antispam-item6-pf1 sections 22, 25, 30, perf/item6-phase2
- cancel-all-index-background-prune | background prune of stale cancel-all index entries (cancel-alls visit 189 books, sender in 92) | proposed-not-done | ~0.4 ms est; before mainnet | ozarchy-antispam-item6-pf1 section 30 / Open
- e1-oracle-step | E1 oracle step on empty blocks | parked | 5.8 ms per empty block | ozarchy-antispam-item6-pf1 section 7.3 / Open
- cloid-cost-share | cloid at 100% share costs ~12% matched/s ceiling, ~9% CPU per fill (per-order hash-map work) | decided | engine +14%/1k, verify +25% | ozarchy-antispam-item6-pf1 sections 59-60
- cloid-claimed-set-presize | pre-size cloid claimed set HashSet from batch placements (reserve_rehash) | proposed-not-done | ~0.06 ms/1k | ozarchy-antispam-item6-pf1 section 60
- cloid-smaller-key-hash | 64-bit digest key or cheaper hash for cloid maps (foldhash long path) | proposed-not-done | ~0.3 of exec delta | ozarchy-antispam-item6-pf1 section 60
- cloid-worker-built-index-delta | workers build per-book cloid index delta; exec thread only merges; skip pre-pass for senders without live cloids | proposed-not-done | index upkeep 0.140 + pre-pass 0.167 ms/1k left | ozarchy-antispam-item6-pf1 sections 60, 63
- cloid-engine-cost-fixes | cloid engine-cost fixes (perf/cloid-engine-cost) | merged | +5.6% matched/s at 100% share; ~1/3 recovered | ozarchy-antispam-item6-pf1 section 63, bc44a845
- p2-5-foldhash-exec-maps | P2-5: execution-path HashMap/HashSet std SipHash -> foldhash RandomState (bit-identical) | merged | engine -8.59 ms per native block (gate met) | ozarchy-antispam-item6-pf1 section 32, perf/item6-phase2 631becaa
- cancel-by-id-market-in-order-id | consensus item B: market in order id so cancel/modify by id doesn't probe 457 books | proposed-not-done | ~2.0-2.5 ms per by-id action; P2-1 did not cut it | ozarchy-antispam-item6-pf1 sections 25, 31
- exec-profile-q8-rows | exec rows: flush_all 11%, Keccak 10-11%, malloc 6%, save_order_books 6-7%, put_cf_raw 5.4%, borsh 5%, merge_disjoint 4%, BTreeMap insert 3.5-4% | decided | exec on-CPU ~131-149 ms per native block | ozarchy-antispam-item6-pf1 section 81
- oracle-feed-off-cost | oracle feed off vs on | decided | oracle off +4.0% matched/s | ozarchy-antispam-item6-pf1 section 81
- exec-p99-tail-causes | exec-time p99 = start-of-bench ramp, epoch-boundary blocks, checkpoint upkeep spikes (not RocksDB stalls) | decided | Phase 4 step 0 | ozarchy-antispam-item6-pf1 section 64.4

## settlement/margin/liquidation

- settle-defer-results-d | TORUS_SETTLE_DEFER_RESULTS: defer settle result materialisation (s58 bundle) | default-on | settle_pass_b 38 -> 30.7 ms/blk; no matched/s alone | ozarchy-antispam-item6-pf1 sections 76-79, merge 4156a83b
- fast-sell-top-ups-t | TORUS_FAST_SELL_TOP_UPS: fast path for sell-side margin top-ups (s58 bundle) | default-on | +1.97% matched/s alone; margin 56 -> 46.4 ms/blk | ozarchy-antispam-item6-pf1 sections 76-79, merge 4156a83b
- l22-settle-no-merge | TORUS_SETTLE_NO_MERGE: position flush without merge_disjoint, per-cache market-id ranges, fallback on overlap | default-on | std exec CPU 0.957, blk/s 1.095, p99 0.877; deep settle -6.9 ms | bench-queue 8n/8q/8t / sections 98, 101, 108, exp/8k-settle-no-merge-fix ddf41758
- l21-streamed-settle | TORUS_SETTLE_STREAMED: start each market's pass B as soon as it is matched, undo/replay on fallback | decided | deep exec CPU/1k 1.044 fails rule; deep blk/s +8.6-20%; on in batch 10 | bench-queue 9b, 9j / sections 114, 123-124, perf/l21-streamed-settle df9e1d8a; owner list batch 10
- l21-microbench-quiet-host | L2.1 microbench on quiet host, with and without batched per-market park/unpark wake-ups | measured-no-gain | exec CPU +1.7-2.5%; batching changes nothing (<=0.02 ms per park) | bench-queue 9h / section 119
- settle-passb-attribution-cut | attribute and cut settle pass B (96-133 ms, largest engine chunk) | proposed-not-done | largest single engine chunk | block-latency-campaign-2026-08-20 section 7 #7
- cancel-all-integer-leverage-margin | hoist margin config once per market and replace fixed-point division by integer leverage in exec_cancel_all | tried-inconclusive | live 38,611 vs 38,143 fills/s (+1.2%, n=1) | cancel-all-integer-margin-2026-09-18, perf/cancel-margin-integer f2a7164
- reserve-integer-leverage | exact raw integer-leverage division for per-order reservation/release and single cancel | parked | built+tests, never benched | load-improvements-2026-09-19, 796a70e
- balance-cache-first-dirty-flags | track first-dirty senders with entry flags + retained vector | parked | built+tests, never benched | load-improvements-2026-09-19, 9cf45f7
- settle-passa-capped-chunks | LPT-chunk settle pass A across capped workers instead of one thread per market | merged | +49% at 300 m same head; settle -50% | matched-200k-campaign-2026-08-18 r6, 583badd, 484115e
- settle-passb-parallel-shard-apply | parallel shard-apply of settle pass B with deterministic order | proposed-not-done | pass B 185 ms serial at 300 m | matched-200k-campaign-2026-08-18 section 17 #3
- settle-cache-flush-serial-diet | cut per-market settle cache_flush overhead | proposed-not-done | 4.3 -> 55.4 ms from 10 to 300 m | matched-200k-campaign-2026-08-18 section 17 #6
- passb-serial-diet | trim serial settle pass B | proposed-not-done | est -15-45 ms/blk | matched-200k-campaign-2026-08-18 section 11 #3
- settle-workers-cap | TORUS_SETTLE_WORKERS cap for parallel settle pass A | merged | pass B stays serial | throughput-campaign-2026-09-18 section existing parallelism
- parallel-passb-persistent-shards | parallel pass B or persistent execution shards with ordering design for shared traders | proposed-not-done | none | throughput-campaign-2026-09-18 candidates
- balance-cache-inplace | update borrowed cached balance entry in place with dirty flag beside value; no map reinsertion | tried-inconclusive | pass B ms/1k fills -10%; engine total flat; 34.6k vs rejected base | throughput-campaign-2026-09-18, perf/balance-cache-inplace e4340d8
- settle-passb-diag-attribution | TORUS_SETTLE_PASSB_DIAG: pass B split into position merge, balance apply, trade route, residual | decided | trade routing 49-50% of pass B; balances 15-18% | throughput-campaign-2026-09-18, diag/settle-passb-attribution c43909f
- deferred-trade-fixed-keys | TORUS_DEFERRED_TRADE_FIXED_KEYS: keep fixed-size trade keys through background writer (no 3 Vec allocs per fill) | tried-inconclusive | pair OFF/ON 32.8k/40.8k (+24.5%), pass B -33%; reversed repeat both REJECT | throughput-campaign-2026-09-18, perf/deferred-trade-fixed-keys 0571fba
- c2-holder-hashset-foldhash | C2 position holder sets as foldhash HashSet | merged | ubench_trader_positions_apply | pre-merge-suites-ozarchy.md, perf/c2-holder-hashset dde72cd3
- c2-set-holder-on-key-change | C2 holder index moved only when position key appears/disappears; foldhash market map | merged | results doc section 27 | pre-merge-suites-ozarchy.md, perf/c2-set-holder c8d25db8
- position-v2-savings | position v2 encoding/savings (ubench_position_cache) | merged | 18c 'merge as is' | pre-merge-suites-ozarchy.md, perf/position-v2-savings b8e3b606
- c1-margin-ubench-full-node | C1 margin ubench gain on full node at 10 markets | measured-no-gain | +3.3% within noise | ozarchy-antispam-item6-pf1 section 3
- pf1-same-batch-bid-top-ups | PF1: remove same_batch_bid_top_ups hot function (49% of crab exec path) | merged | 11.2 -> 0.02 ms/1k; matched 65.9k -> 127.5k (1.93x) | ozarchy-antispam-item6-pf1 sections 4-5, 0ebfd71
- c4-liquidation-maker-fill-fits | C4: cheaper liquidation; maker_fill_fits biggest crab cost | merged | liquidation 3.1-3.4 ms/1k (-15%) | ozarchy-antispam-item6-pf1 section 7, 14236fa
- c6-c7-state-reads-liquidation | C6+C7: cut state reads 8.3->1.7, liquidation 3.2->0.9, maker_fill_fits 4.3->1.5 ms/1k | merged | +25.7% at 300 m (0.638x main) | ozarchy-antispam-item6-pf1 section 10, 82bd1a4
- margin-f1-account-check-cuts | margin phase cut list: F1 account check in prepare_one 1.77, top-ups/pool takers/bid floors 1.15 ms/1k | proposed-not-done | named cuts ~1.7-2.7 ms/1k | ozarchy-antispam-item6-pf1 section 11
- p1-p4-fix-a-checked-mul | P1-P4 + fix A: checked_mul -77%, matching -15%, settle -17% | merged | +1.5% (0.648x main) | ozarchy-antispam-item6-pf1 section 12, 239ff69
- cheaper-address-hasher | cheaper hasher for Address-keyed maps (SipHash 0.46 ms/1k; hash_one<&Address> 0.94 ms/1k) | proposed-not-done | 0.94 ms/1k remains | ozarchy-antispam-item6-pf1 sections 12.2, 14.3 / Open
- m1-margin-match | M1: margin 210->125 ms/blk, match 92->45 | merged | +17% (0.760x main); engine 9.93->7.72 ms/1k | ozarchy-antispam-item6-pf1 section 14, 90a752c
- end-resident-reuse-decoded-positions | BlockSums::into_cache reuse decoded positions instead of re-decoding | merged | 21.1 -> 14.2 ms per block (ubench) | ozarchy-antispam-item6-pf1 sections 14-16, 1242d80
- b-blind-non-pool-sell-top-up | B-blind: non-pool sell top-up replaces same-batch bound | merged | Gate 2 1.097x main at 300 m, 0.997x at 10 m | ozarchy-antispam-item6-pf1 section 19, 31cea69
- option-a-same-batch | option A for same-batch sell bound | rejected | not needed: cuts 0.063% of placed | ozarchy-antispam-item6-pf1 section 19
- liquidation-sweep-sums-caching | cache pos_sums in liquidation sweep (empty blocks rebuild it) | proposed-not-done | row 78 | ozarchy-antispam-item6-pf1 section 22
- adl-work-budget | ADL per-block work budget with carry-over (W=100,000, charged by market holders) | merged | S=750 consensus frozen 11.6 min -> passes | ozarchy-antispam-item6-pf1 sections 23-24, perf/adl-budget 6a25e20, a746c408
- adl-dirty-check | node-local ADL dirty check | merged | block B 149.6 -> 128.8 ms | ozarchy-antispam-item6-pf1 Open, perf/adl-dirty-check f859907f
- adl-rank-sort-speedups | ADL: adl_rank sort (~28%), get_native_balance for candidates (~13%), records binary search (~12%) | parked | 18c: not now | ozarchy-antispam-item6-pf1 Open
- exact-cost-basis-merge-disjoint | exact cost basis (9e695364) made pass B +10% via PositionCache::merge_disjoint DRAM fills | decided | ~44% of 6.3% regression | ozarchy-antispam-item6-pf1 sections 26-28
- trader-positions-apply-residual | find what remains of TraderPositions::apply per-fill cost after C2 fix | proposed-not-done | 0.229 vs 0.201-0.213 ms/1k | ozarchy-antispam-item6-pf1 section 29 / Open
- cancel-share-fix | fix address-ordered cancel share so margin not concentrated on low-address senders | merged | cancel-all expiry 0; matched 179-187k vs 144k min 6-10 | ozarchy-antispam-item6-pf1 section 53c, de97682f
- maker-overcommit-9-11 | maker over-commit under flat tiers: split free margin per book or serialise makers | proposed-not-done | OPEN before mainnet | ozarchy-antispam-item6-pf1 Open
- fill-transition-value-drift | fill_transition truncating average entry drifts value sum ~-0.2 units per fill | proposed-not-done | backlog | ozarchy-antispam-item6-pf1 Open
- p2-2-batch-cache-flush | P2-2: write_cf_raw_many one batch per PositionCache/BalanceCache flush | rejected | cache flush +2.85 ms per block (reversed); reverted | ozarchy-antispam-item6-pf1 section 33, 29320f6b
- put-position-overlay-btreemap | cache flush put_position: overlay BTreeMap insert of owned rows dominates flush_all | decided | put_position 9.7 of 17.6 ms flush_all | ozarchy-antispam-item6-pf1 section 25

## state/storage/rocksdb

- exec-parallel-keys-kp | TORUS_EXEC_PARALLEL_KEYS=1: parallel key preparation in exec | measured-no-gain | +1.03% [0.997,1.024] within noise | bench-queue step 4 / section 82
- parallel-book-save-b | TORUS_PARALLEL_BOOK_SAVE: encode/save order books on worker threads | parked | +7.5% CPU-s/1M, no throughput alone; buys lower exec lag at saturation | bench-queue main-as-benched note; ozarchy-antispam-item6-pf1 sections 76, 78
- rh4-da-body-gc | DA body GC with --retention-blocks: delete old bodies, GC on consensus thread | merged | SST growth ~0; 1.5-2.1 ms/block; p50 +8.1% inline | bench-queue Q9/8h/8o, perf/rh4-da-body-gc b2dd21a4, main 687eac9e
- rh4-gc-worker | move DA GC record/delete/write off consensus thread to bounded-queue worker with pin fence | merged | p50 fixed 0.989; blk/s 0.949 left (state-pruner compaction) | bench-queue 8r / section 103, perf/rh4-gc-worker a0181b8b
- prune-by-default-retention | retention N=100,000 on by default (state pruner, block-tree pruner, DA GC) | merged | 8o blk/s 0.961, p50 +8.1%; landed with RH4 s131 | bench-queue header note, sections 100/103
- state-pruner-delete-range-tombstones | state pruner delete_range_cf on cf_block_bodies causes compaction CPU x2 | proposed-not-done | compaction 0.077->0.15 s/s; next: retention with body pruning off | bench-queue 8r-attr / section 103.6, data/ozarchy-8r
- da-gc-dispatch-behind-repasses | drop ~300 dispatch_behind held re-passes per node per cell in GC worker | proposed-not-done | ~300 holds per cell | bench-queue 8r
- phase5-step2-users-in-place | Users mutated in place, write-through, dirty-set drain (item 6 Phase 5 step 2) | measured-no-gain | exec CPU/1k fills +5%; blk/s 0.48; p99 +37% | bench-queue 8d / section 91, feat/item6-phase5-step2 1e51c8b9
- phase5-drain-on-flush-worker | move Phase 5 dirty-set drain encoding to flush worker | proposed-not-done | trigger not met (no gain in 8d) | bench-queue 8d
- boot-empty-db-probe | boot all_accounts() emptiness check replaced by single-key probe | merged | ready 34.3 -> 34.2 s at 1M accounts | bench-queue 8f, fix/boot-empty-db-probe
- deep-book-save-books-scaling | Classic save_books = borsh encode of whole book 73% + memmove 15%, linear in resting depth | decided | +0.61 ms per 1k fills per 1M resting; 1M: -16% matched | bench-queue 8f/8i / sections 93, 96
- book-mode1-orderrows | TORUS_BOOK_ROWS=1 per-order row storage | rejected | P0 matched 265.9k -> 233.0k; deep 0.709 ms/1k | bench-queue 8m / section 99
- book-mode2-levelauthority | TORUS_BOOK_ROWS=2 level-authority storage | rejected | close to mode 3, superseded | bench-queue 8m / section 99
- book-mode3-chunked-per-block-flush | TORUS_BOOK_ROWS=3 chunked level rows, saves only changes, per-block flush | rejected | deep +8.7%, shallow 0.937, CPU/1M +17-18%; genesis stays Classic | bench-queue 8m/8p / sections 99, 102
- book-mode3-checkpoints-b0 | mode 3 with checkpoints, write half of book save on exec thread | rejected | shallow 0.911, blk/s 13.65->3.99; deep 1.061 | bench-queue 8s / section 104, perf/mode3-checkpoints-b0 c2fbfbc0
- rh4-prune-compaction-filter | TORUS_PRUNE_MODE=filter: durable cutoff, DeleteFilesInRange whole SSTs, compaction filter for rest, replaces delete_range_cf | merged | p50 1.003, CPU/1M 1.003; blk/s 0.963 unresolved; extra compaction -74% | bench-queue 8w / section 112, perf/rh4-gc-worker baffa3a5
- book-mode3-b1-screen | b1: mode 3 + checkpoints, book write pass 2 stays on flush worker via window.side | parked | CPU/1M +13.3% shallow / +11.4% deep; deep blk/s 3.18x; owner parked b1 | bench-queue 8x / section 109, perf/mode3-b1 ab4ded4e
- b1-binary-classic-slowdown | b1 binary slows Classic blk/s 0.746 with equal per-fill cost; clean rebuild identical md5 | tried-inconclusive | blk/s 0.746 [0.712,0.780]; cause unknown, next qc_to_advance timers | bench-queue 8z/8z2 / section 111
- l31-coalesced-book-save-ab | L3.1 TORUS_BOOK_COALESCE: skip per-block Classic whole-book encode; encode each owed book once at checkpoint window close | default-on | deep matched 1.160, blk/s 3.42->15.76, p50 193->13 ms; std exec CPU 0.958 | bench-queue 8y/9e / sections 110, 117, perf/l31-coalesced-save b7bb3666, merge/l31-default-on e1106362
- h2h-mode3-m15-vs-cl21 | final head-to-head: mode 3 + b1 hash-on vs Classic CL21 | rejected | M15/CL21 matched 0.963/0.952, blk/s 0.714/0.747, CPU/1M 1.15 | bench-queue 9j2 / section 124, perf/h2h-combined 29bfc705
- rh4-100k-retention-soak | prune-by-default N=100,000 keeps DBs flat past window | merged | SST growth 0.0231 -> 0.0012 GB/min; GC 0.33 ms per pass | bench-queue 9i / section 121, main b452b4af
- cf-block-headers-growth | cf_block_headers grows in both arms (not pruned) | proposed-not-done | 0.048 GB at 120 min | bench-queue 9i / section 121
- body-stored-twice | bodies stored twice (DA cf_native_pending + state-DB block bodies; commit JSON body write); compact body record, drop duplicate write | proposed-not-done | data dir 0.19-0.2 GB/min; fds 351->667 | bench-queue Items Q9 / Later (s124); cap100-gap-attribution section 5
- max-open-files-unset | RocksDB max_open_files unset so open fds track SST count | proposed-not-done | fds 351 -> 667 in 60 min | bench-queue Items Q9
- hl-style-segment-file-storage | write-once delete-by-age data: per-height segment files pruned by unlink; DA as append log + hash index or blob files | proposed-not-done | bodies ~15 s, DA ~113 s compaction CPU per 600 s cell | bench-queue Later (s127)
- rocksdb-blob-files-da | RocksDB blob files for DA bodies as a config-only try | proposed-not-done | none | bench-queue Later
- resident-books-advance-untouched | ResidentBooks::advance_untouched on non-native path so empty blocks don't force book rebuild | default-on | load_books 65.74 -> 0.06 ms/blk; block ms -8.4% paired | block-latency-campaign-2026-08-20 r3, ac031c7, TORUS_RESIDENT_ADVANCE_UNTOUCHED
- save-books-pass2-on-worker | move save_books pass 2 (overlay puts of drained ops) to flush worker W into separate W-owned books(N) layer (TORUS_SAVE_BOOKS_ON_WORKER) | proposed-not-done | drain 79.8 + write 53.2 ms at 300 s | block-latency-campaign-2026-08-20 section 7 #3; design-exec-pipeline-2026-08-20 candidate 4
- save-books-pass1-off-thread | move pass 1 (level digests from live book) off exec thread | rejected | reads live levels engine(N+1) mutates | block-latency-campaign-2026-08-20 section 7
- memtable-cap-769d7b3 | memtable/L0 cap to reduce cell variance | rejected | no RocksDB stall/backlog in any cell; hypothesis dead | blown-cell-diagnosis-2026-09-17 section 1, 769d7b3
- rocksdb-pipelined-write | TORUS_ROCKSDB_PIPELINED_WRITE=1 so consensus tiny writes don't queue behind exec flush batches in write group | default-on | view -9%, matched +7-13% n=2; compiled default via plan 9.13 | consensus-thread-attribution-2026-08-21 section 4, 6f61f05
- owned-value-overlay-puts | overlay puts move owned values instead of copying (owned API) | parked | built+32 tests, never benched | load-improvements-2026-09-19, bdc335d
- save-books-parallel-levels | parallelise mode-2 journal drain across dirty books (node-local, byte-identical) | merged | +2.3% 120 s; +32% 300 s vs control | matched-200k-campaign-2026-08-18 r1, fec3cff
- body-persist-skip-r1 | skip redundant exec-time body persist (first attempt) | rejected | -6.6% at cap 100 | matched-200k-campaign-2026-08-18 r1
- chunked-low-pri-bg-trade-writer | chunked low-priority background trade writes + RocksDB write-controller knobs | merged | best-60 64.6-81k | matched-200k-campaign-2026-08-18 r3/r5, 2eb62ce
- commit-persist-reuse-wire-body | consensus-thread commit persist as one WriteBatch of bin wire body; exec skips rewrite (FIX 1a) | merged | +35.2%; body_persist 215 -> 0.09 ms | matched-200k-campaign-2026-08-18 r4, 55da979
- save-books-workers-4 | TORUS_SAVE_BOOKS_WORKERS=4 | rejected | -34% cliff | matched-200k-campaign-2026-08-18 r5
- bucket-member-cache-512 | TORUS_BUCKET_MEMBER_CACHE_MB 512 with pbh8 | tried-inconclusive | mixed | matched-200k-campaign-2026-08-18 r5
- stops-dirty-flag-range-scan | dirty flag + range scan for stop rows in save_books | tried-inconclusive | save_books -18% at 300 m; matched flat | matched-200k-campaign-2026-08-18 r8/r9, a79afcd; design-exec-pipeline 4.1
- state-write-waloff-writeopts | WriteOptions disable WAL for state flush (default OFF) | tried-inconclusive | state_write db -35%; 11-block crash rewind; matched/s flat | matched-200k-campaign-2026-08-18 r9, 79ea483; design-exec-pipeline 3.5
- state-write-cf-breakdown-memtable-sweep | which CFs own 12.24 MB/block batch; memtable sizing sweep | proposed-not-done | 37 MB/s effective write | matched-200k-campaign-2026-08-18 section 17 #7
- trade-kvs-arena-user-index-diet | trade KV arena + user index diet | proposed-not-done | +2-8% est | matched-200k-campaign-2026-08-18 section 11 #7
- bg-trade-writer-wal-off | WAL off for background trade writer | proposed-not-done | +2-8% est | matched-200k-campaign-2026-08-18 section 11 #7
- history-db-split | split trade history into separate DB | proposed-not-done | +2-8% est | matched-200k-campaign-2026-08-18 section 11 #7
- late-cell-drift-compaction-tuning | compaction tuning for late-cell state_write drift | proposed-not-done | state_write 106 -> 158 ms late | matched-200k-campaign-2026-08-18 section 17 #10
- resident-caches-hoist-prefetch | hoist/prefetch resident caches | proposed-not-done | smaller follow-on | matched-200k-campaign-2026-08-18 section 11 #10
- rocksdb-max-total-wal-mb | TORUS_ROCKSDB_MAX_TOTAL_WAL_MB: flush CFs pinning old WAL files (1024 vs automatic, fresh DBs) | tried-inconclusive | 35.4k vs 29.1k (control slow idle); WAL retained 10.5->1.8 GB | throughput-campaign-2026-09-18 WAL budget sections, perf/wal-budget b0a57ce
- per-market-tombstone-compaction | node-local per-market background compact_range when uncompacted deletes reach T=64 (ForceOptimized, one worker) | merged | markers 20,000 -> 3,750; costs ~one background core under multi-market churn | read-precompile-gas.md s99 (c), db.rs note_scanned_deletes
- book-cf-sst-target-4mib | TORUS_BOOK_CF_TARGET_FILE_MB: target_file_size_base 4 MiB for cf_native_order_books | default-on | markers -73%; getOrderBook block 131->62 ms; compaction CPU -9% | read-precompile-gas.md SST section, fix/read-gas-followup
- book-cf-sst-target-1-2mib | 1-2 MiB SST target for book CF | rejected | better (40-47 ms) but fd count with max_open_files=-1 | read-precompile-gas.md SST section
- bounded-max-open-files-limitnofile | bounded max_open_files or raised LimitNOFILE (DB keeps every SST open) | proposed-not-done | fd exhaustion known validator-death mode | read-precompile-gas.md SST section
- compact-on-deletion-collector | add_compact_on_deletion_collector_factory for tombstones | rejected | tombstones live in memtable; would not change numbers | read-precompile-gas.md (c)
- iterator-max-skippable-internal-keys | ReadOptions::set_max_skippable_internal_keys or count visited keys to bound tombstone walks | proposed-not-done | 1,024 markers -> 820 ms per 30M block | read-precompile-gas.md not fixed section
- compiled-defaults-9-13 | plan 9.13 node-local compiled defaults on when unset: pipelined write, resident books, root cache, parallel settle, member cache 256 MB | default-on | env_unset_compiled_defaults_equal_the_bench_config | pre-merge-suites-ozarchy.md feat/compiled-defaults 63449230; matched-200k section 3 env bundle
- book-rows-3-default | TORUS_BOOK_ROWS=3 compiled default | rejected | reverted 1d5d7bd0 (genesis carries no book mode) | pre-merge-suites-ozarchy.md feat/compiled-defaults
- stops-diff-drop | drop stops diff work in save | decided | ~0.06 ms; dropped from plan | ozarchy-antispam-item6-pf1 section 22
- phase3-checkpoint-coalescing | item 6 Phase 3: coalesce state writes into checkpoints (layer + ingress view), 15 s interval | merged | CPU-s/1M -7.0%, writes -51%; blk/s -5.5%; RSS +1.55 GB | ozarchy-antispam-item6-pf1 sections 41-49, 61
- phase3-ingress-view | Phase 3 step 2: ingress view for current RPC/mempool reads without per-block state write | merged | RPC head lag fixed (step 1 stalled 37 s per cell) | ozarchy-antispam-item6-pf1 section 47
- phase3-merge-wait-fix | Phase 3 merge-wait fix (checkpoint_settle_wait merge join) | merged | section 54 | ozarchy-antispam-item6-pf1 section 54, 3cf8f72d
- trades-da-compaction-tuning | compaction tuning for trades + DA CFs | tried-inconclusive | screen + confirm sections 38-39 | ozarchy-antispam-item6-pf1 sections 38-39
- raise-nofile-limit | torus-node raises soft RLIMIT_NOFILE to hard limit at start-up | merged | 4 MiB SSTs ~256 files/GB | ozarchy-antispam-item6-pf1 Open, 35953ff8
- append-cf-codec-zstd1 | TORUS_ROCKSDB_APPEND_CF_CODEC=zstd1: bottommost ZSTD level 1 on trades/user_trades/pending CFs | measured-no-gain | changed nothing measurable; dropped | ozarchy-antispam-item6-pf1 sections 38-39, perf/append-cf-compaction fa8b646f
- append-cf-codec-lz4 | LZ4 at every level on append-only CFs | rejected | node CPU -1.02 not resolved; +48% SST growth | ozarchy-antispam-item6-pf1 section 38
- wal-budget-2048-default | TORUS_ROCKSDB_MAX_TOTAL_WAL_MB=2048 validator default | default-on | -6.3% node CPU, matched +2.4%; +2.7 s DB open | ozarchy-antispam-item6-pf1 sections 39-40
- wal-compression-zstd | TORUS_ROCKSDB_WAL_COMPRESSION=zstd | parked | -69 to -74% bytes written; +0.45-0.59 CPU-s/1M vs w | ozarchy-antispam-item6-pf1 sections 38-40, 4a3f6546
- trade-history-off-validators | TORUS_TRADE_HISTORY=0 on validators (RPC/explorer keep it) | default-on | -11.9% node CPU, +8.2% matched/s, -54% SST growth | ozarchy-antispam-item6-pf1 section 39
- phase3-books-only-coalescing | Phase 3 prototype books-only coalescing | rejected | -2.98% CPU, below 5% line | ozarchy-antispam-item6-pf1 section 42
- phase3-checkpoint-layer-shrink | shrink Phase 3 layer (~2.1x logical size) or tighter jemalloc decay to cut RSS | proposed-not-done | +0.55 GB live + 0.45 GB retained | ozarchy-antispam-item6-pf1 section 57.5
- vm-dirty-bytes-2g | larger vm.dirty_bytes (2 GB) to test writeback throttling | rejected | 0 s in balance_dirty_pages; d1 inside noise | ozarchy-antispam-item6-pf1 sections 66.6, 71.5 (campaign ozarchy-p4j3)
- phase4-steps-1-4 | item 6 Phase 4 steps 1-4 (end_resident -45%, typed loads) | merged | 300 m +3.01%; 10 m -2.75% (harness tail) | ozarchy-antispam-item6-pf1 sections 67, 69-70, 2a245321
- phase4-open-order-counter-step5 | stored per-user open-order counter | rejected | net exec saving -1.18 ms [-3.41,+1.05]; dropped | ozarchy-antispam-item6-pf1 section 68, 6288ebe8
- consensus-da-own-rocksdb | 71.8 option A: HotStuff store + DA store in own RocksDB instance | merged | in-window write wait -91%; p99 -10.5% | ozarchy-antispam-item6-pf1 section 74 (71.8 option A, campaign ozarchy-hsdb)
- r01-r02-failstop-cost | R01/R02 fail-stop checks on every failed serial write/read | decided | ~1% drift unresolved (0.986-0.989x) | ozarchy-antispam-item6-pf1 sections 34-35
- persistence-cpu-share-classic | Classic: execution 38%, signature work 29%, persistence 20-23% of node CPU; append-only compaction 78% of compaction | decided | trades+DA compaction 2.43 CPU-s/1M | ozarchy-antispam-item6-pf1 section 37.7
- data-dir-growth-checkpoint-unreclaimed | data dir grows ~0.19-0.21 GB/min per node with or without load | decided | led to Q9 / RH4 | ozarchy-antispam-item6-pf1 section 79

## checkpoint/hash

- ckpt-defer-books-db | TORUS_CKPT_DEFER_BOOKS=1: defer order-book save to checkpoint time instead of per block | measured-no-gain | +0.95% noise; deep P1M save_books 66.7 ms no gain | bench-queue step 4, 8m / sections 82, 99
- ckpt-interval-5s-stepz | checkpoint interval 15000 -> 5000 ms to shrink batch (zero code) | rejected | W_db -26.5% only; blk/s 0.777, p99 556->780 ms | bench-queue 8 / section 87
- ckpt-max-bytes-50mb-z2 | TORUS_CHECKPOINT_MAX_BYTES=50 MB cap on checkpoint batch | rejected | cap fired every 3.7 blocks; blk/s 0.364, state writes 2.46x | bench-queue 8b / section 89
- ckpt-cadence-preserving-cap | checkpoint byte cap ~80-100 MB that keeps cadence | proposed-not-done | option listed in 89.7 | bench-queue 8b
- ckpt-slicing-a1 | A1: write the checkpoint batch in slices so manifest put and other state-DB writers interleave instead of queuing behind ~128-200 MB batch | parked | step 0: manifest waits in write queue; parked behind T5.3 | bench-queue 8/8b/10, perf/q6-ckpt-slicing 9d770519; ozarchy-antispam-item6-pf1 71.5, 74.6
- t53-commit-hook-blocked-by-ckpt-write | attribution: consensus commit hook (persist_commit_manifest) waits behind own node's ~128 MB checkpoint DB write | decided | in-window p99 1,071 vs 542 ms; 446 of 529 ms G | bench-queue 8v / section 106
- t53-offthread-header-body-writer | T5.3: commit manifest in consensus DB (old path TORUS_MANIFEST_IN_STATE_DB=1) + header/body state-DB write on FIFO writer thread with per-height ticket | default-on | G 452->100 ms; in-window p99 0.53; blk/s 1.20 | bench-queue 9c / section 115, perf/t53-manifest-consensus-db f72eda52
- l61-window-books-hash | L6.1: hash order-book CF only at %100 heights where every book is written, so L3.1 runs with running hash on | default-on | deep B/C matched 1.167, blk/s 4.82x, hash 59.6->2.41 ms | bench-queue 9g / section 120, perf/l61-window-books 53a6d83c
- running-state-hash-attest | STATE_HASH_ATTEST=1 h_n attestation with fail-stop; drain check | default-on | 213/213 checkpoints equal on 3/3 nodes | bench-queue 9ghn / section 122
- checkpoint-interval-5s-c5-m5 | 5 s vs 15 s checkpoints with L3.1+L6.1 (C5) and mode 3 (M5) | rejected | 100-block bound fires first; C5 std p99 1.013; M5 std blk/s 0.920 | bench-queue 9j stage 1 / section 123
- flush-root-overlap-1deep | overlap root(N) with engine(N+1) as separate batch (async-trie Option A) | rejected | needs two batches + O(state) rebuild after crash | block-latency-campaign-2026-08-20 section 7
- trie-parent-vector-reuse | sorted child indices dedup to parent sequence instead of BTreeSet in native trie | parked | built+29 tests, never benched | load-improvements-2026-09-19, 3a9e53a
- level-hash-seq-chunked-mode3 | chunked level digest: keccak over per-64-seq chunk digests (depth-independent level_hash), TORUS_BOOK_ROWS=3 | merged | +37%; save_books 336->73 ms/blk | matched-200k-campaign-2026-08-18 r2, e271291, a59dfaf
- parallel-bucket-hash-8 | TORUS_PARALLEL_BUCKET_HASH unset = 8 threads for native trie bucket hashing | default-on | +10.8% n=3; pbh12 +3% | matched-200k-campaign-2026-08-18 r5, e44a38c
- root-subphase-attribution | sub-timers for native trie root (415 ms/blk at 300 m) | proposed-not-done | root 26% of block at 300 m | matched-200k-campaign-2026-08-18 section 17 #1
- positions-bucket-by-trader | bucket position trie rows by trader to cut dirty buckets at 300 m | proposed-not-done | 26.4k dirty buckets/flush vs 3.1k | matched-200k-campaign-2026-08-18 sections 11, 17
- bucket-hash-threads-300m-sweep | sweep bucket hash thread count at 300 m | proposed-not-done | none | matched-200k-campaign-2026-08-18 section 17 #10
- native-trie-maintenance-off-default | native trie root maintenance off by default (root has no production reader); TORUS_NATIVE_TRIE_MAINTENANCE=1 re-enables; stale sentinel, rebuild at boot | default-on | ~17 ms per empty block, ~4 ms/1k on W; -415 ms W at 300 m | ozarchy-antispam-item6-pf1 sections 8-9, db6c9de; design-exec-pipeline candidate 6
- phase3-interval-5s | Phase 3 checkpoint interval 5 s vs 15 s | rejected | section 55 | ozarchy-antispam-item6-pf1 section 55
- checkpoint-interval-60s | checkpoint interval 15 s vs 60 s (fewer, larger windows) | proposed-not-done | none | ozarchy-antispam-item6-pf1 section 66.6
- checkpoint-per-validator-offset | spread checkpoint times across validators (offset per validator) so <=1 node writes at a time | proposed-not-done | untested code change | ozarchy-antispam-item6-pf1 section 66.6
- shared-write-queue-checkpoint-stall | consensus/admission writers queue behind checkpoint batch in shared RocksDB write queue (WriteThread::AwaitState) | decided | DB write 12-24x in windows | ozarchy-antispam-item6-pf1 section 71.5

## serialization/wire

- l33-book-fast-encode | TORUS_BOOK_FAST_ENCODE: exact-size Classic book encoder, no output-Vec realloc/memmove | default-on | save_books 0.845 std / 0.853 deep; exec CPU 0.968 deep | bench-queue 8k / section 97.8, exp/8k-book-fast-encode 4b916ecf
- l33-size-pass-design | ranker's two-pass size-then-encode book encoder design | rejected | microbench -9 to -16%, dead vs exact-size | bench-queue 8k / section 97
- raw-frame-direct-read | network raw frames read directly into final payload Vec, write payload behind stack header | parked | built+tests, never benched | load-improvements-2026-09-19, perf/s61-load-improvements b3f1c7e
- borrowed-compact-encoding | borrow header/hash/EVM/core-writer fields during compact block serialization instead of cloning | parked | built+tests, never benched | load-improvements-2026-09-19, 1e26d30
- push-envelope-direct-serialize | serialize push envelopes directly into marker-prefixed buffers; no full-body copy on swarm path | parked | built+tests, never benched | load-improvements-2026-09-19, cdd9871
- book-buffer-move-into-overlay | move consumed Classic/order-row serialized buffers into overlay | parked | built+12 tests, never benched | load-improvements-2026-09-19, ee79f89
- typed-persistence-small-buffers | serialize positions/balances into 95/33-byte buffers, avoid Borsh 1 KiB default | parked | built+tests, never benched | load-improvements-2026-09-19, 43be807
- packed-trade-history-arena | TradeKvs fixed arrays packed into one batch arena (removes 6 small allocs per fill) | parked | correctness only; 297 B inline | packed-trade-history-2026-09-19, 5582366, e4cf9a4

## threading/runtime/scheduling

- parallel-cancel-all-pc | TORUS_PARALLEL_CANCEL_ALL=8: cancel-all loop split over 8 pool workers instead of serial loop | default-on | campaign A matched/s +4.51% [1.039,1.051]; exec block_ms -45% | bench-queue step 4 / section 82, main 3429855d
- parallel-engine-pe | TORUS_PARALLEL_ENGINE=4: run matching engine over N worker threads per market group, byte-identical outputs | decided | on PC+F matched 0.986 loss, blk/s +15.7%, CPU/fill +2.4%; owner: off, node-local opt-in | bench-queue step 5 / section 83
- exec-pool-grain-g | TORUS_EXEC_POOL_GRAIN: tune exec worker pool job granularity | parked | settle_pass_a -1 ms; no matched/s gain | bench-queue main-as-benched note; ozarchy-antispam-item6-pf1 sections 75-76, 78.6
- pool-wait-to-slowest-job | cut exec pool waits (match setup, settle plan, cancel-all ~5 jobs) toward slowest job | proposed-not-done | upper bound 4.0-4.3 / 2.1-2.3 / 1.5-2.1 ms per block | bench-queue 8e / section 92
- flush-worker-exec-pipeline | TORUS_EXEC_PIPELINE: whole flush (build+root+marker) on worker W via sync_channel(0) rendezvous, FrozenPending overlay; serial barrier on EVM/slash/boundary/replay | default-on | block ms -27.4% paired; now only =0 disables | block-latency-campaign r2, fe4b40a; exec-pipeline-bl2-impl; exec_pipeline.rs
- hotstuff-cpu-pinning | HOTSTUFF_CPUS pin hotstuff-algo to one core | rejected | runqueue wait 20 -> 32-41 ms; wall 210 -> 230 ms | consensus-thread-attribution-2026-08-21 section 3
- flush-worker-sync-channel-1 | buffered sync_channel(1) handoff to flush worker | rejected | two non-durable pending sets -> stale reads/divergence (F1/F6) | design-exec-pipeline-2026-08-20 section 8
- exec-throttle-watermarks | TORUS_EXEC_THROTTLE_WATERMARKS: thin blocks on exec_queue_len + deferred_exec | proposed-not-done | unset in every cell | design-exec-pipeline-2026-08-20 section 4.1
- bench-env-bounded-pools | TORUS_CORE_BUDGET=5 / cpusets / tokio 18 bounded pools on shared box | rejected | -67.7%; starves exec and consensus | matched-200k-campaign-2026-08-18 r1
- joint-worker-budget-sweep | sweep matching/settle/save/root worker counts jointly on shared host; persistent pool only if spawn material | proposed-not-done | none | throughput-campaign-2026-09-18 candidates
- exec-pipeline-replay-liveness | TORUS_EXEC_PIPELINE positive replay but post-restart 30-51 s commit gaps | tried-inconclusive | crash gate PASS, liveness REJECT | throughput-campaign-2026-09-18 positive replay
- end-resident-off-exec-thread | step 2: end_resident on worker, begin_resident(N+1) before new_env | merged | hides ~92%; residual -56 ms per block | ozarchy-antispam-item6-pf1 section 18, 4acdc59, 2333ba4
- p2-3-persistent-worker-pool | persistent exec worker pool replacing ~41-46 thread spawns per native block | tried-inconclusive | matched -1.2% [-5.0,+2.6]; 0 forks; owner merge call | ozarchy-antispam-item6-pf1 sections 44, 50, perf/p2-3-worker-pool efcd1548
- thread-spawns-per-block | 47.5-52.4 thread spawns per native block (15.8-18k/min) on exec thread | decided | led to P2-3 pool | ozarchy-antispam-item6-pf1 sections 22, 25, 44
- hotstuff-blocked-on-exec-channel | hotstuff-algo blocked on full 64-slot exec channel (exec backpressure) | decided | 9-35% of wall (Q8); 0-26% after F | bench-queue Q8/8c / sections 81, 90

## build/compiler/allocator

- binary-gap-53a6d83c-vs-29bfc705 | cross-campaign exec CPU/fill +5.3/+8.7% between binaries; confirm, profile, bisect | queued | R/120A matched 0.983/0.970, exec CPU 1.053/1.087 | bench-queue 9l (HOLD, owner list)
- code-layout-binary-effect | per-binary code layout shifts exec cost/cadence equilibrium (8kd uniform rise, 9f O 14.4->8.0 blk/s) | tried-inconclusive | O blk/s 14.4 vs 8.0 same env | bench-queue 8u, 9f / sections 105, 118
- jemalloc-allocator | jemalloc compiled in as global allocator vs glibc | merged | Phase 3 RSS growth flat vs glibc +120-158 MB/min; step 2 matched +3.6% | ozarchy-antispam-item6-pf1 sections 51, 53b, 56-57, feat/jemalloc-allocator 88ddc4c2
- jemalloc-decay-background-thread | jemalloc background_thread:true, dirty_decay_ms:1000 | tried-inconclusive | decay soak pair | ozarchy-antispam-item6-pf1 sections 62, 73
- jemalloc-decay-m1-m0 | jemalloc decay settings M1/M0 for RSS | rejected | M1 -7.1% matched for -21% RSS; M0 -30.7% | ozarchy-antispam-item6-pf1 section 73.5
- jemalloc-ld-preload-pre-item6 | LD_PRELOAD jemalloc on pre-item-6 main | decided | ~1.11x matched/s by using more CPU | ozarchy-antispam-item6-pf1 section 65

## harness/bench

- standard-load-hsdb-300m | fixed standard load shape (300 mkts, 76k rate, MAX_IN_FLIGHT 4, commit tail) for every cell | decided | bare defaults gave 140-156k vs 240-250k | bench-queue order note s127
- exec-path-merge-rule | merge gate: exec-thread on-CPU per 1k fills, blk/s, p50/p99, timeouts per 1k blocks | decided | +5% exec CPU -> blk/s 10.8->5.2 | bench-queue owner s128 rule
- quiet-cpu-busy-gate | quiet() gates on 5 s CPU busy sample instead of 1-min load avg | merged | max_busy_pct 10 | bench-queue pacing rule s131
- genesis-reuse-cache | reuse unchanged genesis per cell (GENESIS_CACHE_DIR, md5 keyed) | merged | deep non-load time 302 -> 238 s per cell | bench-queue 8p
- one-detached-driver-pacing | run all cells of a step from one detached driver, <=60 s gaps | decided | gaps 5-70 s | bench-queue pacing rule s131
- deferred-post-cell-scans | report-only scans once after last cell (end_pass) | merged | gaps 66 -> 5-6 s | bench-queue 8k pacing note
- merged-main-baseline-9a | baseline with all s131 defaults (exec-hash, L3.3, L2.2, option 0) N vs E | decided | N 268.8k, 15.47 blk/s, p50/p99 14/659 | bench-queue 9a / section 113
- merged-main-deep-baseline-9d | deep P1M baseline with all defaults; EVM load free on deep | decided | 232.7k, 3.54 blk/s, p99 1,344 ms, save_books 56 ms | bench-queue 9d / section 116
- margin-exhaustion-paced-soak | soaks > ~20 min exhaust account margin; need top-ups or lower load | decided | 13.3% margin rejects min 110-120 | bench-queue 9i, s126 notes
- oracle-attestation-same-ms-nonce | bench-only oracle/attestation same-ms nonce caused late vote resubmitted after 65 s | decided | one hole at 6800 healed | bench-queue 9j2 / section 124
- evm-tx-latency-timestamps | per-tx EVM inclusion latency needs tx-flood send timestamps (not built) | proposed-not-done | none | bench-queue Items Q12
- ckpt-db-write-stage-histograms | step 0 timers: checkpoint DB write/wal/memtable/delay/pre_post/residual histograms + start/end gauges, 10 Hz dirty sampler | merged | used by Z, Z2, 8v | bench-queue Items Q6, perf/q6-ckpt-slicing
- exec-chain-attribution-timers | exec_chain_seconds, handoff_wait, flush_worker_seconds, save_books drain/write split, native vs empty block counters | merged | chain+empty == block ms to the digit | block-latency-campaign-2026-08-20 r1, 6eab88d
- crash-gate-survivor-agreement | crash gate judges killed node on digest/hash/root, counters among survivors only | merged | first live kill -9 PASS | block-latency-campaign-2026-08-20 r4, db02d93
- crash-gate-process-lifetime-counters | crash gate comparing process-lifetime Prometheus counters | rejected | unpassable by construction | block-latency-campaign-2026-08-20 r3
- crash-gate-replay-gap-arm | kill val1 while exec_queue_depth > 0, prove rewind <= 2 | proposed-not-done | only zero-rewind arm tested | block-latency-campaign-2026-08-20 section 7 #5
- idle-out-of-band-fatal | run-cell refuses after 3 failed idle probes unless FORCE_IDLE=1 | proposed-not-done | 5 cells lost to settling | block-latency-campaign-2026-08-20 section 7 #6
- block-cap-resweep-rate-100k | resweep block cap 50/100/200 under pipeline at RATE 76k and 100k so mempool not ingress bounds blocks | proposed-not-done | none | block-latency-campaign-2026-08-20 section 7 #9; design-exec-pipeline 4.2, candidate 5
- views-per-block-timeout-screen | validity screen views/blk <= 1.35 and timeout rate <= 18% | decided | FAIL group spread 110x vs PASS 3.2x | blown-cell-diagnosis-2026-09-17 section 3
- offcputime-bpfcc-profiling | eBPF offcputime on context switches to name blocking call | rejected | cost 14x throughput; -t TID matches nothing | blown-cell-diagnosis-2026-09-17 section 6
- steady-window-slicing-gap-attr | gap_attr.py steady window [bench0+10 s, bench1] and histogram tail; fold into summarize.py | proposed-not-done | ramp commits 90-134 idle blocks | cap100-gap-attribution-2026-09-10 section 7
- stall-detect-acceptance | acceptance detects sustained no-commit-progress with pending work, separate from AGREE | merged | r3 stalled at h584 still AGREE | cap200-loadwin-repeats-2026-09-18, stall-584-diagnosis
- streamed-log-collector | stream log processing instead of materializing 1.6 GB logs in Bash vars | proposed-not-done | collector 12 min post-stop | cap200-loadwin-repeats-2026-09-18
- load-window-summarize | summarize.py reports load window at top level (drain and incl_drain siblings); view histograms + per-thread schedstat | merged | wall per block 151 ms (drain) vs 239 ms load | consensus-thread-attribution-2026-08-21 section 1; blown-cell-diagnosis section 7, fce94b0
- rocksdb-stats-2 | TORUS_ROCKSDB_STATS=2 RocksDB stats exported in bench | merged | db.write avg 7.2 ms p99 27 ms | consensus-thread-attribution-2026-08-21
- validate-block-timers | torus_validate_block_* timer family; mempool_native_size gauge set after prune | merged | none | consensus-thread-attribution-2026-08-21 section 6
- cap-200-only-rule | all perf/load runs must use cap 200, never lower | decided | cap-100 regime unstable | load-improvements-2026-09-19
- markets-per-sender-locality | --markets-per-sender K locality load shape (K=0 unchanged) | merged | never run with K>=1 | matched-200k-campaign-2026-08-18 r7, 121c940
- concurrent-pinned-300m-digest | 300-market state digest concurrent and pinned (53 s vs ~4 min/node), first120 window | merged | AGREE on 4/4 | matched-200k-campaign-2026-08-18 r7, 26680ac
- engine-untimed-subtimers | sub-timers for ~390 ms untimed engine share (phase1, settle passA/passB/cache_flush) | merged | identities exact; overhead 3-7 ms/blk | matched-200k-campaign-2026-08-18 r9, 6333eaa, d7a073f
- state-write-build-vs-db-split | split flush state_write into WriteBatch build vs RocksDB write | merged | 86% RocksDB write, 14% build | matched-200k-campaign-2026-08-18 r8, ca159f3
- core-scaling-demonstration | 1-validator + bench TORUS_MATCH_WORKERS/RAYON sweeps or taskset 5/5/5 vs 8/8/2 | proposed-not-done | none | matched-200k-campaign-2026-08-18 section 10
- matched-cell-duration-parity | first-120 s window in every summary + 2x120 s + 1x300 s confirm | merged | 120 s pool sd ~2.9k | matched-200k-campaign-2026-08-18 r5/r7
- matched-unit-fill-events | torus_orders_matched_total counts fill events per validator, not unique orders | decided | 45-55k campaign results = matched fills/s | matched-fill-units-and-generator-2026-09-18
- client-concurrency-sweep | compare generator CONC 32/64/128/256 under fixed server conn limit | proposed-not-done | none | matched-fill-units-and-generator-2026-09-18 exp 2
- generator-worker-budget | scope TOKIO_WORKER_THREADS to generator process only (2/4/8) | proposed-not-done | none | matched-fill-units-and-generator-2026-09-18 exp 4
- generator-attempt-instrumentation | instrument attempted actions/orders, failures, sign/semaphore/HTTP durations in generator | proposed-not-done | ACK 443-460 actions/s | matched-fill-units-and-generator-2026-09-18
- collect-logs-single-scan | collect_logs.py scans each log once, retains queue-warning byte counts | merged | collector was ~12 min | stall-584-diagnosis-2026-09-18
- locality-balance-generator-fix | generator locality: alternate actual per-market owners (fix one-sided markets at even periods); rate schedules | merged | 46 Rust + 76 Py tests | throughput-campaign-2026-09-18, perf/bench-locality-balance a8d9542
- strict-crash-replay | strict replay qualification: forced replay must show actual rewind | merged | positive replay gap 1; post-restart stall | throughput-campaign-2026-09-18, test/strict-crash-replay 441e967
- fstrim-aware-driver-start | driver waits for load1 < 1.5 and no active fstrim | merged | fstrim I/O wait observed | throughput-campaign-2026-09-18 build identity
- host-observer-psi | read-only host observer: CPU/steal/iowait, PSI, Dirty/Writeback, diskstats every 2 s | merged | none | throughput-campaign-2026-09-18
- stale-cargo-artifact-check | require flag + metric presence in node binary; shared target reused stale executable | decided | DA artifact byte-identical to hash node | throughput-campaign-2026-09-18 build identity
- independent-bounded-sampler | independent bounded per-node scrape with completion timestamps; invalid rows not zero | merged | 81 Python tests | throughput-campaign-2026-09-18, f969b3d
- c1-pending-parent-fixture | TORUS_C1_QUALIFICATION hook: park W, SIGKILL, replay vs serial on all CFs | merged | 132+152 tests | throughput-campaign-2026-09-18, test/c1-pending-parent 93b94ce
- digest-quiescence-fix | digest snapshots require HTTP success, 4 finite counters, two equal nonempty snapshots | merged | false AGREE from empty strings | throughput-campaign-2026-09-18, f7a1b68
- depth-observer | optional val1 depth observer at nominal offsets (resting counts) | merged | default off | throughput-campaign-2026-09-18, 998d1eb
- runner-env-allowlist | campaign driver --runner-env allowlist (BAND/CROSS/CANCEL/MPS/RATE_SCHEDULE/DEPTH_OBSERVER) | merged | none | throughput-campaign-2026-09-18
- analyze-fetch-timing | analyze_fetch_timing.py pairs admission->handler; p95 13-14 ms | merged | no drops; queue depth max 7-9 | throughput-campaign-2026-09-18 instrumented body-fetch
- burst-rate-schedule | generator RATE_SCHEDULE burst 0:30000,60:120000,120:0,240:30000 with recovery phase scoring | merged | pause recovery 48.5 s | throughput-campaign-2026-09-18 resumed 2026-09-19
- deeper-book-cross-025 | CROSS_FRACTION=0.25 deeper-book workload | decided | 17.6k fills/s; phase1 122->805 ms early->late | throughput-campaign-2026-09-18
- bench-resend-loop | bench resend loop as cause of 10-market gap | rejected | 0.75x with retry, 0.66x without | ozarchy-antispam-item6-pf1 section 6
- max-in-flight-open-order-budget | per-sender in-flight cap (--max-in-flight) + OPEN_ORDER_BUDGET removes open-limit rejects | merged | rejects 40% -> 0%; main 96-101k -> 173k | ozarchy-antispam-item6-pf1 section 21, 59fa407
- crash-kill-stop-wait-start | stop/wait/start restart in crash-kill.sh instead of in-place restart | proposed-not-done | 20 of 48 restarts un-exchanged subs | ozarchy-antispam-item6-pf1 Open
- in-flight-tail-depth-4 | bench in-flight commit tail depth 4 (tail read-ahead) | tried-inconclusive | +4.5-6.5% matched/s; tail lag 60 -> 2 heights | ozarchy-antispam-item6-pf1 section 70.7
- item6-gate3-ratio | item 6 Phases 1-3 vs pre-item-6 main like for like | decided | 1.931x [1.877,1.985] matched/s | ozarchy-antispam-item6-pf1 section 65

## rpc/streams

- rpc-ack-json-scratch | task-local JSON scratch for RPC acks; keep only parsed actions and ack hashes after verify | parked | built+62 tests, never benched | load-improvements-2026-09-19, 8a34358
- rpc-max-conns-headroom | TORUS_RPC_MAX_CONNS 256/512 vs default 64 with client CONC=256 | proposed-not-done | config mismatch; parse errors cause unknown | matched-fill-units-and-generator-2026-09-18 exp 1
- rpc-submit-batching | SUBMIT=1/4/8 actions per RPC request to amortize HTTP and permit overhead | proposed-not-done | none | matched-fill-units-and-generator-2026-09-18 exp 3
- submit-pipeline-64-permits | node 64 concurrent submit pipelines, 250 ms permit timeout, ingress verify pool half CPUs | decided | 23.9 ms verify wall per call | matched-fill-units-and-generator-2026-09-18
- rpc-orderbook-row-depth-scan | RPC torus_getOrderBook under row layouts reads every row of market (tombstone cost) | proposed-not-done | none | read-precompile-gas.md item 7 step 1
- shed-path-parse-bytes | cheaper shed path: peek action tag or const-hex before decoding whole hex payload | proposed-not-done | ~1.2-1.9 ms/1k | ozarchy-antispam-item6-pf1 section 17.2 / Open
- getblockbody-serve-contention | val2 getBlockBody serving contention paces tail-bound bench | proposed-not-done | lead for 10 m gap | ozarchy-antispam-item6-pf1 section 70

## other

- read-precompile-gas-per-unit | price EVM reader precompiles: base 16,400, 500 gas per scanned row + 20 per word (vs 50 placeholder) | decided | worst row 417-513 ns; 30M block 234 ms at 50 gas | read-precompile-gas.md s99, bench/read-precompile-gas
- reader-unit-types-priced-apart | price rows (~125 gas) and returned words/blob bytes (~3 gas) separately | decided | adopted as 500/row + 20/word in s99 | read-precompile-gas.md budget section
- getorderbook-from-memory | serve getOrderBook from executor in-memory book (no markers) | rejected | superseded: getOrderBook removed in item 7 step 1 | read-precompile-gas.md s100, item 7 step 1
- per-market-level-delete-cap | per-market cap on level deletes per block as validity rule | proposed-not-done | fallback option | read-precompile-gas.md s100 decisions
- bbo-precompile-0x080e | bbo(bytes32) best bid/ask point read from node-written cf_native_bbo, saved_bbo skip | merged | 16,440 gas | read-precompile-gas.md item 7 step 2
- delegations-for-validator-full-scan | delegations_for_validator scans whole CF_STAKING_DELEGATIONS per call (per block with fee share) | proposed-not-done | none | ozarchy-antispam-item6-pf1 Open
