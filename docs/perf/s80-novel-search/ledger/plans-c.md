# Ledger part plans-c: docs/plans o..z + PRPs/*.tasks.json (Torus-hyperBFT, main eb993e3f)

Format: slug | mechanism | status | evidence | source. Status checked against main code where cheap (git grep); PRP task statuses are stale and were not trusted.

## consensus
- committed-datum-header-verbatim | execute committed datum header verbatim; cached proposal only if hash-for-hash equal (cached_matches_compact) else DA reconstruct | merged | 0 forks over 1358 heights n=4 | docs/plans/s442-consensus-findings-and-limitations.md
- header-fastpath-lock-before-vote | header fast path calls update_locks_only before voting | merged | safety | docs/plans/s442-consensus-findings-and-limitations.md
- view-monotonic-pc-to-lock | view-monotonicity guard in pc_to_lock; lock-on-parent not grandparent | merged | safety, commit 4a94e8b | docs/plans/s442-consensus-findings-and-limitations.md
- sync-contiguity-guard | refuse synced block whose parent not in tree (NonContiguousSync) | rejected | never fired; conflicts with gap backfill | docs/plans/s442-consensus-findings-and-limitations.md
- gap-driven-backfill-strict-order-exec | strict-order exec queue + lowest-hole-first backfill for deferred-body/restart holes | merged | fixes 96-height restart holes | docs/plans/s442-consensus-findings-and-limitations.md
- dead-leader-slot-throughput | 1-of-4 dead validator drops throughput >4x due to leader-slot timeout cadence | queued | owner list: dead-leader skip b1 | docs/plans/s442-consensus-findings-and-limitations.md (f)
- commit-feed-after-header | latency lever: feed commit after header broadcast | proposed-not-done | listed as lever needing order-age metric | docs/plans/order-latency-histogram.md
- batched-propose-writes | latency lever: batch the proposer's writes on propose | proposed-not-done | listed s77 | docs/plans/order-latency-histogram.md
- body-request-before-vote | latency lever: request body before vote | proposed-not-done | listed s77 | docs/plans/order-latency-histogram.md
- async-validate | latency lever: validate block asynchronously | proposed-not-done | listed s77 | docs/plans/order-latency-histogram.md
- evm-cadence-latency | latency lever: EVM cadence | proposed-not-done | listed s77 | docs/plans/order-latency-histogram.md
- speculative-block-production | next proposer runs produce_block speculatively during current view's vote on cloned state; use on QC, discard on TC | proposed-not-done | target 15-20ms->10-15ms; no SpeculativeSlot in code | docs/plans/speculative-pipelining-impl.md, PRPs/speculative-pipelining
- tc-required-option-b | proposals for v need justify.view==v-1 or TC(v-1); leader waits awaiting_tc; prev_collector for v-1 timeout votes | in-progress | owner list; est one-down LAN 1.51->1.50 blk/s, WAN -8..-13% | docs/plans/tc-required-and-tip-signing.md
- tip-signing-leader-header-sig | leader signs headers (domain-tagged); TipInfo becomes signed header; timeout vote signs tip view | in-progress | owner list; +64B header, 1 sign/verify | docs/plans/tc-required-and-tip-signing.md
- tc-require-strict-validator-only | strict TC validator rule alone without leader wait | rejected | wedged one_down_liveness_test (exp/tc-require-tc-probe) | docs/plans/tc-required-and-tip-signing.md
- reputation-leader-selection | reputation-weighted leader selection | rejected | batch 9 refuses at start | docs/plans/testnet-genesis-checklist.md, 76862c2c
- iwrr-power-normalization | normalize voting powers (gcd / smooth WRR) or per-epoch seeded rotation for leader schedule | proposed-not-done | dead-leader Q4/Q7 open | docs/plans/testnet-genesis-checklist.md
- evm-cadence-n11 | keep EVM block interval N=11 (coprime to 3/4 validators); option S raise N superseded | decided | rotate EVM proposer before mainnet | docs/plans/testnet-genesis-checklist.md
- commit-then-execute | strip native exec from propose/validate; execute post-commit on exec thread (CTE8) | merged | exec queue up to 64 blocks | PRPs/commit-then-execute.tasks.json
- hybrid-header-first-pipelining | proposer gossips header, validators vote on header and fetch body; DA guard delays commit until body | merged | header fast path is sole vote path | PRPs/hybrid-pipelining.tasks.json
- early-view-advance-on-qc | advance view as soon as QC forms | merged | hybrid pipelining T8 | PRPs/hybrid-pipelining.tasks.json
- consensus-fast-path-header-broadcast | all 5 proposal broadcast sites send header; gossipsub heartbeat 500->100ms | merged | heartbeat now config-wired | PRPs/consensus-fast-path.tasks.json
- consensus-fast-path-cap-2000 | raise NATIVE_PER_BLOCK_CAP 16->2000 | proposed-not-done | per-sender cap is 64 on main, not 2000 | PRPs/consensus-fast-path.tasks.json
- direct-to-leader-forward | non-leader RPC forwards native actions to current leader (+torus_getLeader) | merged | later gated off when gossip on | PRPs/direct-to-leader.tasks.json
- full-torusblock-proposals | produce_block serializes full TorusBlock instead of CompactBlock | rejected | superseded by CompactBlock + DA (865d4e1) | PRPs/direct-to-leader.tasks.json
- inflight-hash-ledger | InFlightHashLedger excludes in-flight/committed-unfed action hashes from selection (dup-inclusion tail) | merged | dup factor ~1.0 | PRPs/dup-inclusion-tail-fix.tasks.json
- failstop-delivered-height-fix | halted app stops feed advancing delivered height; boot re-delivers gap | merged | batch 7 6eabad95 | docs/plans/wave-s134-tracker.md
- dead-leader-skip-b1 | exclude leaders missing K=3 slots, j cap 6, one-fresh-proposer-per-view | in-progress | owner list; 1-of-4 down drops throughput >4x | docs/plans/wave-s134-tracker.md
- four-validators-f1 | 4 validators (f=1) instead of 3 (f=0, all-3 quorum, any node down halts) | parked | owner decision open (3 or 4), owner list | docs/plans/step3-dissemination-hardening.md, testnet-genesis-checklist.md

## pacemaker/timeouts
- pacemaker-backoff-epoch-extend-only | multiplicative backoff only on extend_epoch_change_view_timeout | rejected | local Instant divergence | docs/plans/pacemaker-backoff-design.md opt A
- pacemaker-backoff-global-counter | local consecutive-timeout counter scaling every view | rejected | breaks Lewis-Pye optimistic responsiveness | docs/plans/pacemaker-backoff-design.md opt B
- pacemaker-schedule-rebase-clamp | rebase remaining schedule when deadline > now+2x max_view_time | merged | fixed S395 5.5h park | commit 15bc833, docs/plans/pacemaker-backoff-design.md
- rejoin-round-skip-header | checked future-view leader header moves rejoining replica to that view via Pacemaker::skip_to_view (cap 16 views, same epoch), then votes once | merged | targets 16-35s rejoin freeze (mean 28s) | docs/plans/rejoin-view-sync.md, perf/s74-rejoin-view-sync, TORUS_ROUND_SKIP
- rejoin-leader-skip-newview | deliver future-view NewView early; rejoiner that is leader(w) skips to w after NewViews power>f and proposes | merged | targets 19-38s freezes (s74 views 423/570) | docs/plans/rejoin-leader-sync.md
- boot-enter-init-view | call enter_view for init view at boot so rejoiner leading its init view proposes immediately | merged | stateright 14.3M states safe | docs/plans/rejoin-leader-sync.md
- parked-header-c | park future header with unknown justify, fetch QC block by hash | merged | part of s74 branch (C) | docs/plans/rejoin-leader-sync.md
- newview-resend-while-waiting | survivors re-send last NewView to leader(v) every ~2s while WaitingForProposal | proposed-not-done | only if A/B shows no leader-skip log line | docs/plans/rejoin-leader-sync.md opt 2
- reconnect-hook-resend-newview | surface ConnectionEstablished to algo thread and re-send latest NewView/TC | rejected | queue flush already re-delivers | docs/plans/rejoin-leader-sync.md opt 3, rejoin-view-sync.md opt C
- f+1-timeoutvote-view-sync | jump to view when TimeoutVotes with power>f seen (LibraBFT/Bracha) + drop CONS-FIND-26 guard | rejected | does not fix observed freeze | docs/plans/rejoin-view-sync.md opt B
- future-header-unknown-justify-park | extend parking to future headers whose justify is unknown | proposed-not-done | decide after A/B | docs/plans/rejoin-leader-sync.md OQ3
- fsync-entered-view-before-propose | fsync entered-view write before proposing (power-loss double proposal) | proposed-not-done | residual risk note | docs/plans/rejoin-leader-sync.md
- validate-retry-blocking-sleep | 5x20ms blocking thread::sleep retry in validate_block on consensus thread | merged | 20% of 500ms view; kept for 3-val | docs/plans/step3-dissemination-hardening.md, 4e097af
- dead-leader-a2-short-timeout | shorter local timeout for views led by a dead leader | parked | owner list; after one-down bench | docs/plans/testnet-genesis-checklist.md
- header-resend-250ms | node-local header re-send interval default 250ms (TORUS_HEADER_RESEND_MS) | default-on | WAN check pending trial net | docs/plans/testnet-genesis-checklist.md, 3429855d
- pacemaker-backoff-state-derived | per-view timeout x factor^min(stall gap, cap), factor 2 cap 8, neutral on happy path | merged | 1200ms base x2 per stalled view (S470) | docs/plans/pacemaker-backoff-impl.md, crates/hotstuff_rs/src/pacemaker/implementation.rs

## dissemination/DA
- o5-full-push-threshold-raise | raise hash-only push threshold 512KB->3MB so bs400 bodies full-pushed, not manifest+pull | measured-no-gain | 3MB 456-4518 orders/s vs 512KB 11.4k; pulls 326 vs 49 | docs/plans/o5-feed-gates-impl.md (S415)
- wan-push-threshold-ab | seed-only env A/B at testnet: 512KB vs clamped 4MB push over WAN | queued | owner list: push-manifest threshold | docs/plans/o5-feed-gates-impl.md
- rh4-skip-presence-multiget | skip body-presence MultiGet until GC has deleted something | proposed-not-done | ~30% of pre-window pass (~0.4ms/block) | docs/plans/rh4-retention-early-cost.md
- rh4-reference-bloom | separate CF or prefix bloom for 39-byte GC reference keys (MultiGet nearly always misses) | proposed-not-done | ref MultiGet ~40% of pass | docs/plans/rh4-retention-early-cost.md
- rh4-slow-pass-log | WARN line + metrics for GC passes >= TORUS_DA_GC_SLOW_MS | merged | observability | docs/plans/rh4-retention-early-cost.md
- retention-blk-s-regime-hypothesis | r2-B 16.4->9.3 blk/s attributed to big/small block-regime split, not GC cost | tried-inconclusive | n=2; GC adds only 0.6-1.0ms/block | docs/plans/rh4-retention-early-cost.md
- s72-defer-parent-feed | next leader skips commit feed on parent-body insert, broadcasts header+self-vote first, then feeds | tried-inconclusive | merged default OFF; alone caused 1.35s serve stalls | docs/plans/s72-defer-parent-feed.md, TORUS_DEFER_PARENT_FEED
- s65-defer-commit-feed | leader feeds commit after header broadcast + inline self-vote in finish_leader_proposal | measured-no-gain | s66 A/B no gain at n=3; default OFF | docs/plans/s72-defer-parent-feed.md, TORUS_DEFER_COMMIT_FEED
- next-leader-skip-da-reconstruct | skip/overlap DA reconstruct in validate on next leader (~57ms, reconstruct ~48) | proposed-not-done | separate target | docs/plans/s72-defer-parent-feed.md opt C
- defer-feed-early-body | also defer feed for body arriving before next leader enters v+1 | proposed-not-done | out of scope | docs/plans/s72-defer-parent-feed.md OQ
- legacy-swarm-block-store-prune | prune unbounded swarm block_store / pending_bodies | proposed-not-done | pre-existing growth | docs/plans/s72-body-serve-thread.md
- native-gossip-prespread | enable dormant native-action gossip by default (--native-gossip) to pre-spread bodies | merged | bs500 35.2k = no gain vs 34.5k | docs/plans/sprint3-gossip-prespread-impl.md (s334/s338)
- gossip-verify-at-ingest | gossip ingest checks recovered sender == claimed after DA mirror | merged | hardening | docs/plans/sprint3-gossip-prespread-impl.md
- rotated-body-fetch-targets | body fetch: proposer 3 tries then round-robin other validators, 9 total before sync | merged | hardening | docs/plans/sprint3-gossip-prespread-impl.md
- byte-cap-2mb-to-6mb | NATIVE_BLOCK_BYTES_CAP 2MB->6MB after pre-spread | measured-no-gain | bs500 35.2k vs 34.5k; bs1000 1.06k ingress collapse | docs/plans/sprint3.5-ingress-unblock.md
- sync-pull-retries-scaled | sync-path pull retries scale missing/2 clamped 20..160 (~1s..8s) | merged | hot path untouched | docs/plans/sprint1-throughput-rails-impl.md
- erasure-recovery-shards | RS(k=f+1,n) shards with Merkle proofs, recovery fetch k shards from k distinct peers, whole-body fallback | merged | n=3 only ~2x per-link; custody off | docs/plans/sprint5-erasure-coding-impl.md, b29d843
- erasure-ingress-dispersal | disperse shard i to validator i at ingress instead of full-body gossip | queued | owner list: after testnet (ingress dispersal) | docs/plans/sprint5-erasure-coding.md opt B, T12
- erasure-root-in-header | bind erasure_root into CompactBlock/header | parked | genesis-relaunch gated | docs/plans/sprint5-erasure-coding-impl.md T11
- step3-gossip-reenable-s297 | re-enable native gossip-on-submission with 100ms retry (options A/C) | rejected | s297: floods GossipSub, drowns consensus (66ddef4) | docs/plans/step3-dissemination-hardening.md
- full-block-proposals-cap100 | drop CompactBlock + reconstruction, send full blocks at cap 100 | rejected | 4th delivery churn; compact kept for cap raise | docs/plans/step3-dissemination-hardening.md opt E
- unicast-pending-send-queue | bounded per-peer PendingSendQueue: enqueue consensus unicast when validator disconnected, flush on ConnectionEstablished; re-enqueue on Direct OutboundFailure | merged | f254e66/ad7c136 | docs/plans/step3-dissemination-hardening-impl.md
- native-bundle-repush-ring | re-push recent native-action bundles on peer (re)connection | merged | 7570e7b | docs/plans/step3-dissemination-hardening-impl.md
- header-fetch-instead-of-push | per-block header/body fetch round trip in place of push | rejected | 46->211ms block time | docs/plans/step3-dissemination-hardening.md
- block-sync-retry-no-blacklist-timeout | retry worker fetch >=1x before ending session; do not blacklist on timeout/disconnect; never blacklist last server | proposed-not-done | T6 gated on quiet-host bench | docs/plans/step3-dissemination-hardening-impl.md
- hash-only-push-manifest | above HASH_ONLY_PUSH_THRESHOLD push hash manifest; receivers pre-warm pull from proposer | merged | bs500 un-wedge | PRPs/native-da-hash-only-push.tasks.json
- native-da-bigbody-chunked-fetch | per-protocol codec caps, chunked native-DA fetch, widened pull budget, >4MB reconstruction | merged | E2E >4MB proof | PRPs/native-da-bigbody.tasks.json
- quinn-bidi-streams-raise | raise quinn max_concurrent_bidi_streams for push | proposed-not-done | no code found | PRPs/native-da-push-hardening.tasks.json
- push-loop-inflight-backpressure | bounded in-flight backpressure on BroadcastNativeActions push loop (PushScheduler) | merged | bs1000 un-wedge | PRPs/native-da-push-hardening.tasks.json
- mesh-watchdog-explicit-peers | gossipsub mesh watchdog force-disconnect + explicit peers for validator set | merged | wedge repro A/B | PRPs/mesh-watchdog.tasks.json
- compact-block-gossip-hash-proposals | CompactBlock hash proposals reconstructed from mempool hash_index + 200ms retry | merged | basis for DA | PRPs/mempool-gossip-hash-proposals.tasks.json

## ingress/mempool
- mempool-priority-classes-oracle | SortKey classes cancel(0), oracle(1), rest(2); oracle bypasses backlog/pool-full screens; 4 pending per validator | merged | feat/oracle-feeder cbab523 | docs/plans/oracle-feeder.md
- rpc-oracle-ingress-checks | apply exec submission rules (entries, dup, listed, price) at RPC ingress | merged | none | docs/plans/oracle-feeder.md
- byte-capped-native-selection | store encoded_len per pool entry, stop selection at NATIVE_BLOCK_BYTES_CAP | merged | bs1000 no wedge | docs/plans/sprint1-throughput-rails-impl.md (s334)
- lazy-nonce-ttl-eviction | evict pool entries with nonce+NONCE_WINDOW_MS < now at select/drain | merged | stuffed pool self-heals | docs/plans/sprint1-throughput-rails-impl.md
- submit-semaphore-64-queue | submit permits 16->64 with acquire timeout 250ms instead of try_acquire | merged | bursts queue, not instant overloaded | docs/plans/sprint1-throughput-rails-impl.md
- batch-submit-endpoint | torus_submitNativeActions(Vec) up to 100: one permit + one spawn_blocking per batch, per-item results | merged | bs500 29,964 o/s at ~50 act/s | docs/plans/sprint2-ingress-spread-impl.md
- jsonrpsee-protocol-batching | use JSON-RPC protocol-level request batching instead of endpoint | rejected | keeps per-item permit/spawn_blocking | docs/plans/sprint2-ingress-spread.md opt B
- websocket-streaming-ingest | streaming/WebSocket ingest transport | proposed-not-done | premature (s334) | docs/plans/sprint2-ingress-spread.md opt C
- local-submitters-per-validator | bench instance per validator host submitting to localhost, split key ranges | merged | bench topology | docs/plans/sprint2-ingress-spread.md
- leader-forward-gated-by-gossip | no full-body leader-forward when native gossip on (gossip + pull carry bodies) | merged | cuts redundant body path | docs/plans/sprint3.5-ingress-unblock-impl.md
- submit-ack-phase-histograms | permit-wait / verify / admit histograms + gossip publish/receive/drop counters | merged | observability | docs/plans/sprint3.5-ingress-unblock-impl.md
- swarm-backpressure-qos-lanes | per-peer in-flight caps, substream budgets, consensus>gossip>DA priority lanes, backoff+jitter | proposed-not-done | only if telemetry shows queue buildup | docs/plans/sprint3.5-ingress-unblock.md opt B
- bench-pregen-signed-ammo | pre-sign full action set before window | merged | removes client CPU from bs1000 | docs/plans/sprint3.5-ingress-unblock-impl.md T6
- verify-cpu-vs-queue-split | rpc_submit_verify_cpu_seconds inside closure vs wall; micro-bench of verify_one_action phases | merged | 115ms/action wall vs est 1-2ms CPU | docs/plans/sprint5-binary-ingress-impl.md
- cheap-first-admission-gate | pool-saturation pre-verify reject before ecrecover (cancels still verified) | merged | targets 59-97% pay-then-drop at bs300+ | docs/plans/sprint5-binary-ingress-impl.md

## signature/crypto
- proposer-sig-attestation | proposer batch-verifies ed25519 native sigs, signs one attestation; validators check one sig; exec re-verifies and slashes | tried-inconclusive | generate_sig_attestation called; verify_sig_attestation has no caller on main | docs/plans/proposer-attestation-impl.md, crates/torus-bridge/src/proposer.rs
- ed25519-batch-verify | batch verification of ed25519 session sigs (~5ms/4096) | merged | batch_verify used in native_executor/app | docs/plans/proposer-attestation-impl.md
- raise-drain-cap-4096 | drain_for_block 4096 native actions per block with attestation | proposed-not-done | target 95-190k ops/s at 43ms blocks | docs/plans/proposer-attestation-impl.md
- s395-dedupe-double-secp-recovery | skip second secp256k1 recovery at finalize for actions verified at ingress (trust cache) | merged | est ~2x verify; --exec-trust-cache default off | docs/plans/sprint-s395-blockspeed-orders.md, PRPs/double-verify-trust-cache
- s395-rayon-parallel-verify | rayon-parallelize native sig verify (exec + ingress par_iter) | merged | est 6-8x on 8 cores | docs/plans/sprint-s395-blockspeed-orders.md, PRPs/ingress-verify-fix
- exec-trust-cache-verified-senders | FifoCache of locally verified senders; exec skips recover on hit, verifies+slashes on miss | merged | default off; PARKED gate: verify must still dominate | PRPs/double-verify-trust-cache.tasks.json
- leader-header-sign-cost | ed25519 header sign 36us, verify 54us (TC phase C) | merged | measured 18c | docs/plans/wave-s134-tracker.md
- eip712-per-variant-typehash | per-variant Proposal<Variant> typed structs (cross-variant sig collision fix) | merged | 9f53045c | docs/plans/wave-s134-tracker.md

## execution/matching
- o2-place-order-batch | PlaceOrderBatch action: one sig/nonce per Vec of orders, executor flattens, per-order isolation | merged | 11.4k orders/s at bs400 (S415) | docs/plans/o2-placeorderbatch-design.md, commit b58f858
- o2-exec-oversize-batch-skip | deterministic exec-path skip of batches > NATIVE_ORDERS_PER_BATCH_CAP=1024 + gossip/DA admit checks | merged | DoS hardening, no perf number | docs/plans/o2-placeorderbatch-impl.md
- o2-order-aware-selection-budget | wire order_count() into proposer selection, stop at NATIVE_ORDERS_PER_BLOCK_CAP | merged | caps 102k-order blocks to 50k | docs/plans/o2-placeorderbatch-impl.md
- o2-trading-scope-batch | SessionScope::Trading includes PlaceOrderBatch (lockstep) | merged | none | docs/plans/o2-placeorderbatch-impl.md
- o2-atomic-batch-variant | PlaceOrderBatchAtomic tag 26: all-or-nothing pre-match reserve dry-run | rejected | no MM demand; atomic only at reserve stage | docs/plans/o2-placeorderbatch-design.md
- o2-per-order-receipts | persist per-order results/reject reasons, new CF + RPC/WS | proposed-not-done | deferred follow-up | docs/plans/o2-placeorderbatch-design.md
- o2-batch-category-split | split batch inner IOC/market orders into own phase instead of whole batch as GTC unit | proposed-not-done | deferred | docs/plans/o2-placeorderbatch-design.md
- o2-criterion-exec-place-batch | criterion bench 1 batch x N vs N singles (N=400) | merged | bench tool | docs/plans/o2-placeorderbatch-impl.md
- per-user-open-order-limit | HL limit min(1000+vol/5M,5000) across markets, counted at Phase-2 start from books | merged | lean A/B -7% matched/s +8% CPU before cost fixes | docs/plans/per-user-open-order-limit-impl.md
- trader-orders-holes-compaction | per-book trader id list with holes + compaction, O(1) amortized removal | merged | cancel 18.6->5.9ms, maker fills 17.5->3.5ms | docs/plans/per-user-open-order-limit-impl.md
- trader-orders-small-vec | TraderOrders plain Vec up to 32 slots, map only past 32; 80->32 bytes | merged | place 164-172->151-161ms (noisy) | docs/plans/per-user-open-order-limit-impl.md
- open-count-global-upper-bound | global per-trader counter to skip book walk | rejected | too much divergent state vs ~15-30ms cost | docs/plans/per-user-open-order-limit-impl.md
- book-mode-classic-genesis | genesis book mode Classic (CL21) over mode 3 M15 | decided | 9j stage 2: CL21 beat M15 on both shapes | docs/plans/testnet-genesis-checklist.md, af53295c
- l61-window-committed-books | Feature BookWindowCommit: window-committed books so L3.1 keeps gain with running hash | in-progress | batch 10, owner list | docs/plans/testnet-genesis-checklist.md
- parallel-engine-wan | TORUS_PARALLEL_ENGINE (PE) on per validator | parked | off by default; awaits trial-net WAN A/B (owner list) | docs/plans/testnet-genesis-checklist.md
- deep-book-1m-resting-regression | ~1-1.7M resting orders: matched/s -14.3%, blk/s 13.3->3.0 (8f) | queued | 9n re-run after batch 10 (owner list) | docs/plans/testnet-genesis-checklist.md
- block-cap-400-wan | native block cap 400 (loopback-proven) on WAN | parked | open, trial net | docs/plans/testnet-genesis-checklist.md
- in-memory-getorderbook-precompile | serve getOrderBook precompile from memory instead of tombstone scan | queued | before mainnet | docs/plans/testnet-genesis-checklist.md, b26bd3b3
- s395-raise-per-sender-cap | NATIVE_PER_BLOCK_CAP 64->256, NATIVE_TOTAL_BLOCK_CAP 1000->4000 env-gated | proposed-not-done | main: per-sender 64, total cap 400 (T2.5) | docs/plans/sprint-s395-blockspeed-orders.md, crates/torus-mempool/src/rate_limit.rs
- s395-order-book-delta-persist | replace full per-block book reload+rewrite (O(all resting)) with delta persist | merged | dirty_books tracking; resident books, L3.1 coalesce, book modes on main | docs/plans/sprint-s395-blockspeed-orders.md
- s395-leader-rotation-geo-bias | env-gated leader-rotation biasing toward low-RTT neighbors for chain-wide sub-100ms | proposed-not-done | WAN legs 227-502ms RTT; no code found | docs/plans/sprint-s395-blockspeed-orders.md
- s395-non-rtt-floor-shave | shave 75-93ms non-RTT fresh-chain view floor | proposed-not-done | target sub-100ms local | docs/plans/sprint-s395-blockspeed-orders.md

## settlement/margin/liquidation
- o1-margin-write-back-cache | per-execute_batch BalanceCache write-back: reserve/release mutate cache, one overlay PUT per sender at flush; invalidate before apply_fill | merged | micro 1177us->831us (-29%), overlay ops ~800->~200 | docs/plans/o1-margin-cache.md (S412)
- o1-margin-write-through-cache | write-through HashMap cache in front of overlay for balance reads | measured-no-gain | 1177us->1224us, in noise; PUT cost dominates | docs/plans/o1-margin-cache.md
- o1b-position-cache | extend write-back cache to get_position/put_position in apply_fill (settle phase) | proposed-not-done | only if settle hot under fill-heavy load | docs/plans/o1-margin-cache.md
- apply-fill-positions-only-split | split apply_fill so PnL credit goes via cache instead of invalidate | rejected | more invasive, no hot-path gain | docs/plans/o1-margin-cache.md
- cum-volume-per-market-sum-pass-a | parallel settle pass A sums per-trader per-market notional; pass B merges one entry | merged | removes i256 mul per fill side | docs/plans/per-user-open-order-limit-impl.md
- l21-streamed-settle | TORUS_SETTLE_STREAMED=1 streamed settle, node-local byte-identical | merged | +8.6% deep blocks/s for +0.75% exec CPU/fill; default off | docs/plans/testnet-genesis-checklist.md, section 123
- delist-option-c | grace N blocks reduce-only then bounded force-close at fixed price (2048 traders/256 positions per block) | merged | product | docs/plans/wave-s134-tracker.md
- account-level-margin-f1 | HL-strict account-margin placement gate, running free at match, maker check at fill | merged | product (F1) | PRPs/account-level-margin-f1.tasks.json
- adl-budget-obligation-rows | ADL with obligation rows, running rest O(P), per-block ranking cache, budgeted drain | merged | product | PRPs/adl-budget.tasks.json
- liquidation-scan-act-budgets | liquidation step SCAN 2048 / ACT 64 per block, round-robin cursor, 20% chunks, 30s cooldown | merged | product | PRPs/liquidation.tasks.json

## state/storage/rocksdb
- overlay-btreemap-string-key-replace | replace NativeStateOverlay BTreeMap<(String,Vec<u8>)> + RwLock + CF-name String alloc per put | proposed-not-done | separate bench-gated candidate (O1 note) | docs/plans/o1-margin-cache.md
- trade-history-off-switch | TORUS_TRADE_HISTORY=0 skips building/writing trade rows (non-serving validators) | merged | 68.4k->102.8k matched/s, 70.4->41.9 CPU-s/1M | docs/plans/trade-history-writes.md
- packed-trade-rows | one row per (market,block,chunk<=1024) + one per (trader,block); exec pushes compact fills; encode on writer thread | merged | est 88-97k matched/s vs 68k full history | docs/plans/packed-trade-rows-impl.md
- trade-rows-sort-before-write | bg writer sorts block rows by (CF,key) so memtable inserts reuse splice | proposed-not-done | RecomputeSpliceLevels top symbol | docs/plans/trade-history-writes.md opt B
- trade-rows-disable-wal | write trade-history batches with disable_wal | proposed-not-done | ~21% of writer cost | docs/plans/trade-history-writes.md opt C
- trade-cf-lz4-bottommost | per-CF options for trade CFs (LZ4 bottommost instead of ZSTD) | proposed-not-done | compaction 1.91 cores incl ZSTD | docs/plans/trade-history-writes.md opt D
- trade-rows-pointer-user-rows | per-user trade rows point into market rows | rejected | slow user reads | docs/plans/packed-trade-rows.md opt C
- trade-rows-pack-market-only | pack only per-market rows | rejected | per-user rows are ~69% of cost | docs/plans/packed-trade-rows.md opt B
- set-sync-commit-frontier-manifest | set_sync(true) only on commit-frontier + body-manifest writes (not hot ingress) | queued | crash-durability; measure commit latency delta | docs/plans/post-erasure-backlog-s466.md Task B, PRPs/commit-fsync-durability
- history-retention-100k-default | prune by default N=100,000 blocks, --archive opts out | default-on | 9i soak PASS | docs/plans/testnet-genesis-checklist.md, b452b4af
- block-tree-pruner-consensus-meta | prune cf_consensus_meta committed block tree (height drag fix) | merged | DB -244MB expected; f99d07a | docs/plans/sprint-s395-blockspeed-orders.md
- body-durability-fail-closed | mirror_native_to_da returns Result; legacy TorusBlock voter path mirrors before Valid; produce_block fail-closed | merged | fixes S458 permanent wedge formation | docs/plans/wedge-durability-fix.md, 9d56144
- quorum-durable-spread-barrier | leader waits for f+1 durable body acks before proposing | rejected | adds RTT per proposal | docs/plans/wedge-durability-fix.md opt B
- sync-wal-on-commit | StateDb::sync_wal at commit boundary, TORUS_SYNC_WAL_ON_COMMIT default off | merged | only for hard-power-loss threat model; A/B pending | PRPs/commit-fsync-durability.tasks.json
- unwedge-tool | torus-unwedge: inspect + prune uncommitted tree, reset singletons to recovery PC | merged | ops recovery for S458 wedge | docs/plans/unwedge-tool.md
- crash-safety-applied-height | NATIVE_APPLIED_HEIGHT marker + startup replay of committed-not-applied | merged | crash safety | PRPs/crash-safety-post-commit.tasks.json
- native-da-durable-store | CF_NATIVE_PENDING DA body store by action hash decoupled from nonce gate; NeedFetch not Invalid | merged | foundation of compact proposals | PRPs/native-action-da.tasks.json
- native-overlay-everywhere | NativeStateOverlay generic; validate/produce/catch-up/crash via overlay + PendingExec; remove post-commit exec path | merged | ensure_native_committed_through gone | PRPs/native-overlay-everywhere.tasks.json
- item6-resident-rows-mark-table | resident rows R, block mark table, sums cache, liquidation L1 (item 6 phase 1) | merged | warm == cold tests | PRPs/item6-phase1.tasks.json
- leader-rep-kv-cache | write-through in-memory cache of LEADER_REPUTATION on BlockTreeSingleton | proposed-not-done | moot: reputation selection refused at start | PRPs/leader-rep-kv-cache.tasks.json

## checkpoint/hash
- native-root-skip | TORUS_NATIVE_TRIE_MAINTENANCE=0 default: no per-block native trie maintenance | default-on | +12% matched/s 100 markets, +31% at 300 | docs/plans/running-state-hash-impl.md, e4273ec
- running-state-hash-sha256-writeset | h_n = SHA-256(h_{n-1} || n || D_native || D_evm) over sorted block write set, O(changed rows) | default-on | digest 3.3ms@10k rows, ~25ms@60k | docs/plans/running-state-hash-impl.md, feat/running-state-hash
- running-hash-streaming-kway-merge | hash multi-layer CF writes via streaming k-way merge, no BTreeMap materialization | merged | output unchanged | docs/plans/running-state-hash-impl.md Dev 14
- lthash-response-accumulator | HL-style LtHash16 over execution responses / full-state LtHash | rejected | old values unavailable in write set | docs/plans/running-state-hash-impl.md
- attest-state-hash-action | validator-only AttestStateHash every N=100 blocks, stake-weighted quorum, mismatch metrics, gated fail-stop | merged | devnet drill detected corruption at next checkpoint | docs/plans/running-state-hash-impl.md
- state-root-parent-semantics-header | re-add state_root to header preimage committing parent's post-exec root | proposed-not-done | future work (S442 e) | docs/plans/s442-consensus-findings-and-limitations.md
- checkpoint-interval-15s | TORUS_CHECKPOINT_INTERVAL_MS default 15000 on validators, 0 on rpc-only | decided | 9j winner ran 15s | docs/plans/testnet-genesis-checklist.md
- incremental-evm-state-root | DB-backed SparseStateTrie incremental EVM root with TrieUpdates in commit batch | merged | A1; native SMT A2 milestone | PRPs/incremental-state-root.tasks.json
- native-incremental-smt-root | A2 keyed SMT/Merkle over CF_NATIVE_TRIE updated from overlay dirty keys | parked | superseded by root skip + running hash | PRPs/incremental-state-root.tasks.json
- item6-since-checkpoint-layer | checkpoint interval/max-bytes, since-checkpoint layer L, W digest/checkpoint jobs, published view for ingress readers | merged | checkpoints 15s default | PRPs/item6-phase3.tasks.json

## serialization/wire
- o5-raise-accept-gates | consensus gossip accept 256KB->1MB, /torus/direct read 4MB->8MB, gossip transmit wired to config, ladder tests | merged | closes EVM-inline 319,796B livelock trap | docs/plans/o5-feed-gates-impl.md, commit 97b931b
- o5-env-threshold-clamp | clamp TORUS_HASH_ONLY_PUSH_THRESHOLD to fleet direct-codec floor 4MB with WARN | merged | none | docs/plans/o5-feed-gates-impl.md
- skip-verify-reserialize | hash canonical input bytes instead of serde_json re-serialize on verify path | proposed-not-done | part of 3.5 opt A; identity determinism test needed | docs/plans/sprint3.5-ingress-unblock.md
- binary-ingress-endpoint | torus_submitNativeActionsBin hex-bincode payloads, canonical JSON hash kept | merged | 5-20x cheaper decode est | docs/plans/sprint5-binary-ingress.md, crates/torus-rpc/src/ip_limit.rs
- zstd-v2-reqresp-protocols | zstd level 3 framing on /torus/{direct,native-da,block-data}/2.0 with per-peer 1.0 fallback | default-on | 9.4x on 500-order batch (64,664->6,882 B) | docs/plans/sprint5-zstd-bodies.md, 6cbda39
- gossip-zstd | zstd-compress gossip frames (--gossip-zstd) | rejected | removed s364: tiny frames did not pay | docs/plans/sprint5-zstd-bodies-rollout-note.md
- zstd-inband-format-byte | 0x00/0x01 prefix sniff on 1.0 protocols | rejected | not mixed-binary safe | docs/plans/sprint5-zstd-bodies.md opt B
- transport-muxer-compression | compression at libp2p muxer layer | rejected | no standard transport, deep risk | docs/plans/sprint5-zstd-bodies.md opt C
- zstd-dictionary | shared trained zstd dictionary for small frames | proposed-not-done | deferred; batches large enough | docs/plans/sprint5-zstd-bodies.md
- zstd-level-1 | zstd level 1 instead of 3 | measured-no-gain | 11.0x vs 9.4x, <15% size diff; level 3 kept | docs/plans/sprint5-zstd-bodies.md

## threading/runtime/scheduling
- open-count-parallel-split | split add_open_order_counts across scoped workers per 25k probes | rejected | 229->38ms probe, but spawn cost 28% CPU, +43ms/block | docs/plans/per-user-open-order-limit-impl.md, perf/ol-count-no-spawn
- open-count-serial-threshold-100k | OPEN_COUNT_WORK_PER_THREAD=100k: bench-shape counts run serial on exec thread | merged | 8-16ms serial per call | perf/ol-count-no-spawn
- rh4-da-gc-worker | DA body GC pass moved off algo thread to bounded worker queue (512 heights, coalesce+backfill) | merged | on_committed_block 4.16->2.25ms vs inline; handoff 127us | docs/plans/rh4-retention-early-cost.md, perf/rh4-gc-worker
- rh4-pass-after-exec-send | run GC pass after exec_tx.send instead of before | proposed-not-done | superseded by worker | docs/plans/rh4-retention-early-cost.md
- running-hash-overlap-digest-thread | blocks >= 4096 pending entries digest on scoped thread overlapped with WriteBatch build | merged | flush wall 214.8 vs 226.9ms at 60k rows | docs/plans/running-state-hash-impl.md Dev 6
- s72-body-serve-thread | BlockDataRequests served on dedicated BlockDataServer thread from BlockTreeCamera snapshot, misses fall back to algo | default-on | throughput neutral, serve queue 4-6x lower | docs/plans/s72-body-serve-thread.md, TORUS_BODY_SERVE_THREAD
- swarm-side-block-store-serve | serve bodies from swarm task in-memory store (revive /torus/block-data/1.0) | rejected | unbounded store, abandoned protocol | docs/plans/s72-body-serve-thread.md opt B
- algo-loop-serve-first | drain block-data requests at top of each algo loop pass | rejected | busy algo thread still blocks | docs/plans/s72-body-serve-thread.md opt C
- on-committed-heavy-to-exec-thread | move materialize + durable persist of on_committed_block to exec thread | proposed-not-done | needs new crash-safety argument | docs/plans/s72-defer-parent-feed.md opt B
- rayon-parallel-in-batch-verify | bounded 2-4 thread parallel verify inside batch | parked | gated on CPU split; consensus starvation risk | docs/plans/sprint5-binary-ingress.md opt D
- background-trade-writer-o3 | trade-history CF writes on background writer thread | merged | -39% bench cost (S414) | git 1a51d1e3, 8437bd15
- validation-worker-thread | move validate_block (EVM + sig recovery) to worker with bounded wait, Invalid on timeout | proposed-not-done | superseded by CTE + header-first | docs/plans/validation-worker-thread-impl.md
- async-validate-speculative-accept | return Valid optimistically, roll back if validation fails later | rejected | needs rollback machinery | docs/plans/validation-worker-thread-impl.md opt A
- pre-validation-on-proposal-arrival | dispatch validation to worker on proposal arrival, inline fallback | proposed-not-done | follow-up option C | docs/plans/validation-worker-thread-impl.md
- da-recovery-worker-offthread | DaRecoveryWorker: hot path hands off DA body recovery to named thread, single event-driven mid-budget re-fetch | merged | BS-4a/4b | PRPs/bs4a-da-reconstruct-offthread.tasks.json
- async-body-persist | move block-body persist off exec thread (TORUS_ASYNC_BODY_PERSIST) | tried-inconclusive | flag not on main | git 4af063ee

## build/compiler/allocator
- opt-level-1-deps-ab | opt-level 1 dependency A/B on 18c | parked | owner list | docs/plans/wave-s134-tracker.md
- cargo-hakari-test-binary-merge | merge test binaries via cargo-hakari | parked | owner list | docs/plans/wave-s134-tracker.md
- sccache-cap | sccache cap 15GB on 18c | parked | owner list | docs/plans/wave-s134-tracker.md
- incremental-off-remote-runners | CARGO_INCREMENTAL=0 on remote runners (cachyoz, ubuntoz) | merged | 18c keeps incremental | docs/plans/wave-s134-tracker.md
- per-branch-cargo-target | per-branch target dirs instead of shared ~/.cargo-target (stale artifacts) | decided | 33 errors from shared target | docs/plans/wave-s134-tracker.md

## harness/bench
- order-age-stage-histograms | node-side histograms of now_ms-nonce at admit/commit/exec/durable/fills_visible; summarize p50/p90/p99 per stage | merged | s77 observability | docs/plans/order-latency-histogram.md
- bench-side-e2e-latency-poll | bench polls RPC per action for fill visibility | rejected | perturbs saturated box | docs/plans/order-latency-histogram.md
- sampled-per-action-trace | 1-in-N action trace joined across nodes by hash | rejected | too many moving parts | docs/plans/order-latency-histogram.md
- s68-block-build-phase-timers | split leader block_build (~171-199ms of 600ms view) into parent/select/mirror/attest/encode histograms | merged | observability s68 | docs/plans/s68-block-build-timers.md
- exec-ceiling-phase-histograms | exec phase timers, queue depth gauge, block_build_seconds | merged | observability | PRPs/exec-ceiling.tasks.json
- one-down-baseline-9k | one validator down baseline (kill no-restart, 2/2/1 stakes) | queued | owner list 9k | docs/plans/wave-s134-tracker.md
- binary-gap-check-9l | 53a6d83c vs 29bfc705 hash-off Classic: -1.7/-3.0% matched/s, +5.3/+8.7% exec CPU/fill | queued | owner list 9l | docs/plans/wave-s134-tracker.md
- h2h-9j-checkpoint-5s-vs-15s | Classic checkpoint 5s vs 15s | measured-no-gain | deep C5 256.1k vs C15 255.2k matched/s | docs/plans/wave-s134-tracker.md
- mode3-b1-book-rows | book mode 3 (book rows) + b1 vs Classic | parked | M15 243.7k vs CL21 256.3k deep matched/s | docs/plans/wave-s134-tracker.md
- l61-9g-3arm | L6.1 window books with hash on vs off | merged | deep matched/s B/A 0.997, blk/s 4.82x vs C | docs/plans/wave-s134-tracker.md section 120

## rpc/streams
- rpc-unknown-market-guard | RPC ingress rejects unknown market_id (phantom tick=1 book) | merged | correctness | docs/plans/o2-placeorderbatch-impl.md
- fill-sink-at-execution | exec thread wraps block fills in Arc<BlockFills>, non-blocking broadcast to RPC; streams work with history off | merged | full-load target 99.1k matched/s, 46.0 CPU-s/1M | docs/plans/trade-streams.md, feat/s80-trade-streams
- stream-from-trade-writer | emit fills from writer on_written callback after rows land | rejected | needs history on, writer latency | docs/plans/trade-streams.md opt B
- stream-db-scan-at-exec-height | keep scan_trades_for_block but trigger at executed height | rejected | re-reads rows; owner: not from DB | docs/plans/trade-streams.md opt C
- record-fills-only-while-subscribed | record stream-only fill extras only while a stream subscriber exists | merged | e80d08ad/b5b1d59f (s80 fix 1) | git log
- stream-block-shared-serialization | newTrades array serialized once per filter per block, shared by subscribers; all-markets rejected on validators | merged | all-markets sub cost 22-33MB JSON/block, +0.5 core, 15s lag | docs/plans/trade-streams-impl.md fix 2
- stream-routing-hub | market/address -> subscriber maps instead of per-subscriber filtering | proposed-not-done | YAGNI until hundreds of subs | docs/plans/trade-streams.md
- pnl-fields-in-history-rows | add closedPnl/startPosition/dir to history rows | proposed-not-done | gated on 4+4 cell A/B | docs/plans/trade-streams.md
- per-trade-ws-messages | one WS message per trade | rejected | one message per block chosen | docs/plans/trade-streams.md Q1

## other
- oracle-begin-block-aggregation | aggregate every listed market once at block start before execute_batch(pre_evm); time-based 10s window/60s stale; one mark per block | merged | product; cost ~2+M prefix scans per native block | docs/plans/oracle-aggregation.md, feat/oracle-aggregation
- oracle-due-gate | run native phase for oracle only when submission rows exist (oracle_due), else skip | merged | with feeder every block runs native phase (C1) | docs/plans/oracle-aggregation-impl.md
- oracle-one-row-per-market-validator | submission key without block: one row per (market,validator), prune bounded by VxM, no cap | merged | bounded prune work | docs/plans/oracle-aggregation-impl.md
- oracle-hl-mark-formula | option C: on-chain EMA of mid-oracle + book median + CEX perp mids for mark | proposed-not-done | after liquidation; owner list P2 mark price | docs/plans/oracle-aggregation.md
- m2-timestamp-lower-bound | reject proposals with ts < local-30s pre-vote; needs validate-before-voting | parked | built then reverted: two wedges; pre-mainnet (owner list) | docs/plans/oracle-aggregation.md, wip/m2-timestamp-lower-bound 62a6f8f4
- proposal-timestamp-validation | refuse ts<parent.ts at parent link; refuse vote if ts>local+5s; proposer max(now,parent.ts) | merged | correctness | docs/plans/oracle-aggregation-impl.md T0b
- epoch-processing-every-boundary | run process_epoch_boundary even on empty boundary blocks | merged | correctness | docs/plans/oracle-aggregation-impl.md T0
- oracle-feeder-hot-signer | separate price-feeder tool + SetOracleSigner hot signer; HL-weighted median of 7 CEX, 3s cadence | merged | feature branch feat/oracle-feeder | docs/plans/oracle-feeder.md
- oracle-signer-hard-block | block every non-oracle action from a signer address | rejected | one index read per action on hot exec path | docs/plans/oracle-feeder.md Q-S1
- p00-activation-gating | header features u128 bitmap + per-chain built-in schedule; old binaries refuse vote/fail-stop at H | merged | phases 1-6 s134 feat/p00-* | docs/plans/p00-activation-height-gating.md
- p00-governance-schedule-activation | governance ScheduleActivation through EpochRotationPlan | queued | before mainnet (owner) | docs/plans/p00-activation-height-gating.md
- genesis-file-digest | store keccak digest of canonical genesis JSON at first boot; refuse start on mismatch | merged | s134 feat/p00-phase4 | docs/plans/p00-activation-height-gating.md
- hash-unverified-node-keeps-voting | node missing running-hash rows keeps voting, no attestations, ERROR + gauge | decided | owner s134 option b | docs/plans/p00-activation-height-gating.md
- t5-1-cadence-310ms | Tier 5 T5.1 block cadence 310ms | queued | owner list after testnet | docs/plans/wave-s134-tracker.md
- t5-2-p99-floor | Tier 5 T5.2 1.2s p99 floor | queued | owner list after testnet | docs/plans/wave-s134-tracker.md
- item7-evm-lanes | EVM cadence N, big-block lane M, per-lane pools/fees, bbo precompile 0x080e | merged | step1 done; step2 lanes | PRPs/item7-step1.tasks.json, item7-step2.tasks.json

