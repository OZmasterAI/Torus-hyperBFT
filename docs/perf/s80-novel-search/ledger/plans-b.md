# Ledger part plans-b: docs/plans i..n (incremental-state-root.md .. native-overlay-everywhere.md)

Source dir: /home/oz/projects/torus-economy/Torus-hyperBFT/docs/plans/ (main eb993e3f). Paths below are relative to docs/plans/ unless they are commits or branches.
Status words: merged, default-on, rejected, tried-inconclusive, measured-no-gain, queued, parked, proposed-not-done, in-progress, decided.
Owner-excluded items (owner-list.md) are still listed so later stages can reject them.

## consensus
- evm-vote-side-header-checks | Validators check header evm_gas_limit, base_fee_per_gas, declared EVM gas and evm_tx_count against the parent/body at vote time (step 0) | merged | closed a crash-replay divergence; 7c15b5aa | item7-evm-lanes.md
- evm-cadence-budget-in-header | Header field evm_block_interval N plus gas limit; governance changes switch only at epoch boundaries via the epoch plan; cadence and caps are pure pre-vote checks | merged | N=11, 3M gas; 853fed28 | item7-step1-design.md, item7-step1-impl.md
- evm-cadence-from-state-at-vote | Read N from the current epoch plan state at vote time instead of a header field | rejected | lagging validator cannot vote on EVM blocks (liveness) | item7-step1-design.md option B
- evm-cadence-genesis-only | N and budget fixed in genesis, governance keys later | rejected | contradicts owner s122 decision | item7-step1-design.md option C
- evm-per-block-tx-count-byte-caps | Per EVM block caps: tx count <= budget/21000, raw EVM bytes <= 256 KiB (768 KiB at big heights) | merged | consensus message fits 830,864 B <= 1 MiB | item7-step1-design.md, item7-step2-impl.md
- evm-big-blocks-height-cadence | Big EVM heights at height % M == 0 with own gas limit and base fee; M=0 at genesis (off) until measured | merged | cfa35ed1; big blocks off at genesis | item7-step2-design.md option A
- evm-big-blocks-wallclock-window | Big block = first block in each 60 s header-time window (HL parity) | rejected | proposer 5 s ts skew; full rate under load | item7-step2-design.md option B
- evm-big-blocks-with-step3 | Ship lanes, fees, bbo now; big-block scheduling only after EVM stops running serial | rejected | not chosen vs option A | item7-step2-design.md option C
- evm-fee-f3-epoch-eip1559 | EIP-1559 per lane per epoch from executed gas, written into plan(H+L), floor 1 gwei, one 12.5% step per epoch | merged | pure votes, reuses epoch-plan path | item7-step2-design.md
- evm-fee-f1-fixed-per-type | Two fixed governance base fees per block type, no dynamic pricing | rejected | no HL fee parity | item7-step2-design.md
- evm-fee-f2-declared-gas-1559 | Per-block EIP-1559 from declared gas carried in header | rejected | declared gas inflatable for free (griefing) | item7-step2-design.md
- evm-fee-f4-at-execution | Base fee per EVM block computed by execution from previous EVM block gas | parked | drops txs at ~14 s exec lag; parked with onchain mempool | item7-step2-design.md
- evm-onchain-mempool | EVM txs enter as L1 actions into a state pool; execution builds and prices EVM blocks; no leader routing | parked | revisit at item 6 Phase 5; pool state writes on exec thread | item7-step2-design.md, item6-phase5-impl.md parked 2
- evm-leader-concentration-rotation | Rotate leader schedule per EVM round (select_leader(view + view/N)) or hashed per-height permutation so EVM heights do not map to one proposer | proposed-not-done | N=11, n=11: same validator proposes every EVM block | item7-option0-impl.md section 7
- leader-rep-kv-cache | Write-through in-memory cache of LeaderReputation in BlockTreeSingleton; removes ~9 KV reads per round from hotstuff hot path | merged | 7a1b980f (default off); targeted ~139 ms idle floor | leader-rep-kv-cache-impl.md
- leader-rep-coalesce-and-spec-commits-cache | Coalesce 2 record_leader_success writes per commit into one; write-through cache for speculative_commits | merged | b674376a "cache spec-commits" | leader-rep-kv-cache-impl.md task 3
- vote-after-body | Vote only once block body is held; validate stops checking sigs/sessions; invalid actions skipped at execution with a record | merged | 48b5b0f6 | market-scaling-in-memory-design.md 1.4
- block-sync-no-blacklist-missing-body | Block sync distinguishes Invalid from NeedFetch; never blacklist a peer for a body-less block | merged | fixed testnet livelock at h151859 | native-action-da.md, native-action-da-impl.md T4
- commit-no-last-header-freeze | on_committed_block never early-returns before advancing last_header; fetch or fail loud | merged | app/RPC height froze before | native-action-da-impl.md T4
- jailing-j2-dline | Escalate b1 dead-leader exclusion to jail after jail_after_s without signing; time-based unjail with doubling | proposed-not-done | owner-excluded; status-only at 4 validators | jailing-design.md
- jailing-e1e2-double-sign-evidence | On-chain double-vote / double-proposal evidence action; 5% slash, tombstone, 25% reporter | proposed-not-done | owner-excluded; needs TC phase C signed bytes | jailing-design.md
- jailing-j1-cosmos-window | Jail when QC signatures < x% over an N-block window | rejected | frames the slowest honest validator | jailing-design.md
- jailing-j1b-late-votes | J1 plus leader carries late votes in next block for a full signer set | rejected | hotstuff format change, bandwidth, +3-5 d | jailing-design.md
- jailing-j3-auto-peer-jailvote | Each node's local latency monitor sends JailVote (HyperBFT style) | parked | later for large sets | jailing-design.md
- jailvote-gate-pregenesis | Gate JailVote behind an unscheduled feature or drop its slash before genesis | proposed-not-done | 3 of 4 can slash/jail the 4th today | jailing-design.md section 7
- one-validator-subset-for-multiple-of-n | Accept validator count multiple of N (one subset leads all EVM heights) for testnet | decided | revisit before mainnet | item7-step1-design.md decision 3

## pacemaker/timeouts
- ckpt-window-p99-offsets | Spread checkpoint write times across validators so all three do not stall HotStuff in the same write window | proposed-not-done | block-time p99 +17.7% sits in checkpoint windows | item6-phase5-impl.md section 9
- ckpt-window-vm-dirty-bytes | Larger vm.dirty_bytes cell to test kernel writeback throttling stalling consensus threads | queued | owner-excluded (T5.3 p99 attribution) | item6-phase5-impl.md section 9
- ckpt-window-hotstuff-offcpu | Off-CPU profile of hotstuff-algo across one checkpoint window to find the blocking call | queued | owner-excluded (T5.3) | item6-phase5-impl.md section 9
- ckpt-interval-15-vs-60-for-p99 | A/B 15 s vs 60 s checkpoint interval for p99 | rejected | changes frequency, not cause | item6-phase5-impl.md section 9
- readview-slow-admission-insert | Investigate mempool admission inserts > 0.5 s on ReadView under Phase 3 (funded check) | proposed-not-done | ~34 slow inserts on c88d3073 vs 0 main | item6-phase5-impl.md section 9
- mesh-watchdog-unsubscribed-kick | Force-disconnect a validator peer connected but not subscribed to consensus topic for >60 s so subscription is renegotiated | merged | devnet wedge repro 0/10 vs base 8/10 | mesh-watchdog-impl.md
- gossipsub-explicit-validator-peers | add_explicit_peer for all validator peer ids (redial on heartbeat, forward regardless of mesh) | merged | 576d3be7 | mesh-watchdog-impl.md
- mesh-resubscribe-stale-conn-kick | Stale-connection kick, no ghost connections, watchdog 5 s / 1 s, connection-denying behaviours first | merged | de1a97c1; SAT tie, benefit not measured | item6-phase2-impl.md row 66
- sigterm-clean-shutdown | Handle SIGTERM like SIGINT: drain exec queue, final checkpoint, zero replay | merged | SIGTERM was a hard kill; replay 0 after fix | item6-phase2-impl.md rows 51, 57
- docker-stop-grace-60s | Set stop_grace_period >= 60 s for nodes | proposed-not-done | 1 of 8 step 2 exits passed 10 s grace | item6-phase2-impl.md row 57
- commit-lag-backoff-cap-genesis | commit_lag_backoff_cap 8 in testnet genesis, not a compiled default | decided | S470 bench value | item6-phase2-impl.md 9.13
- propose-defer-on-exec-queue | Proposer defers a proposal while exec queue depth exceeds a watermark (TORUS_PROPOSE_EXEC_WATERMARK) | parked | revisit if queue ~64 and lag > 5 s | item6-phase5-impl.md parked 3

## dissemination/DA
- compact-block-hash-proposals | Proposals carry native action hashes (CompactBlock); validators reconstruct from local store | merged | ~3x smaller than bincode; once reverted 66da812 | mempool-gossip-hash-proposals-impl.md, native-action-da.md
- native-action-gossip-to-all | Gossip every native action to all validators via gossipsub topic on submission | rejected | drowned consensus (June); topic remains in code | mempool-gossip-hash-proposals-impl.md, native-action-da.md
- header-only-per-block-pull | Header-only proposals with on-demand body fetch every block | rejected | latency 46 -> 211 ms | native-action-da.md
- full-block-proposals | Self-contained full-body proposals | rejected | capped at 256 KB message; stopgap only | native-action-da.md
- da-durable-body-store | Durable CF_NATIVE_PENDING keyed by action hash, populated on every ingest/produce path, survives restart | merged | un-wedged testnet livelock | native-action-da-impl.md T1-T3
- da-store-decoupled-from-nonce-gate | Block-referenced bodies bypass the 60 s nonce-staleness gate; only mempool admission is gated | merged | stale bodies were unrecoverable | native-action-da.md
- da-pull-by-hash-protocol | /torus/native-da/1.0 request_response serving bodies by hash, rare fallback | merged | push primary, pull rare | native-action-da-impl.md T5-T6
- da-push-queue-until-connected | Replace bounded(4) fire-and-forget push with queue plus retry until peer connects | merged | drops under load before | native-action-da-impl.md T7
- da-erasure-coded-sampled | Celestia-style erasure-coded, sampled DA | parked | owner-excluded (T3.1 erasure coding) | native-action-da.md option C
- per-protocol-codec-caps | Separate codec caps: native-DA 8 MB, block-data 16 MB, direct 4 MB unchanged | merged | >4 MB bodies wedged at bs=1000 | native-da-bigbody-dissemination-fix-impl.md
- chunked-da-fetch | Client splits by-hash fetch into chunks of 16 hashes (~1.2 MB per response) | merged | 1 oversized request before | native-da-bigbody-dissemination-fix-impl.md
- sync-pull-budget-1s | Widen sync-path pull budget from 80 ms to ~1 s | merged | 4 x 20 ms too short | native-da-bigbody-dissemination-fix.md
- bigger-caps-only | Raise codec caps without chunking | rejected | moves the ceiling only | native-da-bigbody-dissemination-fix.md option A
- hot-path-pull-fallback | Bounded (~150-200 ms) native-DA pull in hot validate_block when a body is missing | merged | 7ceb01ff; 0 hot pulls caused wedge | native-da-push-hardening.md, -impl.md T1
- hot-path-async-vote-later | Fire fetch async and vote in a later view once body present | proposed-not-done | design-gated fallback if bounded pull too short | native-da-push-hardening-impl.md T1.2
- quic-bidi-stream-window | Raise quinn max_concurrent_bidi_streams from 100 to ~512 | merged | 0ef73e09; 2,537 "max sub-streams" at bs500 | native-da-push-hardening.md
- bounded-inflight-push | PushScheduler caps concurrent direct.send_request pushes (backpressure) | merged | d35cac37 | native-da-push-hardening.md option B
- gossipsub-native-dissemination | Replace per-validator unicast push with one gossipsub publish (hashes, pull bodies) | proposed-not-done | end-state option D, never built | native-da-push-hardening.md
- hash-only-push-size-gated | Push a tiny hash manifest when bundle > 512 KB; receivers pre-warm pull from proposer before CompactBlock | merged | 2139bb81; bs500 was wedged 26.8 s blocks | native-da-hash-only-push.md
- hash-only-push-always | Always manifest, always pull bodies | rejected | adds pull RTT, risks bs100 23,430 o/s baseline | native-da-hash-only-push.md option B
- skip-push-big-batches | Above threshold skip push, rely on CompactBlock hashes plus hot pull, no pre-warm | rejected | hot pull budget too tight | native-da-hash-only-push.md option C
- max-consensus-message-1mib | Raise max_consensus_message_size 256 KB to 1 MiB for larger blocks | merged | 1 MiB cap referenced by step 2 caps | native-200k-throughput-impl.md phase C
- block-compression-large-blocks | Compress big block payloads for dissemination | proposed-not-done | watch item only | native-200k-throughput-impl.md phase C
- evm-route-next-evm-leader | Forward each EVM tx to the predicted leader of the next EVM height, not current-view leader | merged | step 1: waits up to 661 blocks before fix | item7-step1-impl.md E12
- evm-resend-upcoming-n-inflight-filter | Re-send tx to next predicted leader when target commits without it; use upcoming N; drop pooled tx at commit | merged | 100% within one extra EVM height | item7-step2-design.md decision 7
- evm-resend-on-view-change | Re-send on every view change or route to an extra leader | proposed-not-done | revisit if waits > 2 EVM heights without timeouts | item7-step2-impl.md s126 decision 7
- evm-gossip-to-all-validators | Gossip EVM txs to all validators | rejected | needs in-flight filter; re-send made it unnecessary | item7-step1-design.md
- evm-lost-proposal-reinsert | Txs of a proposal that never commits go back to the pool and are re-sent | merged | reinsert_evm had no caller before | item7-step1-impl.md E11
- evm-drain-skip-ancestor-txs | EVM drain skips txs carried by uncommitted ancestor blocks (hash only, no ecrecover) | merged | 2cdc3ac7; ~1% duplicate entries before | item7-step2-impl.md decision 10

## ingress/mempool
- ingress-verify-par-iter-global-rayon | par_iter the RPC verify closure on the global rayon pool | rejected | s352: block time 4034 ms, starved consensus; reverted 1659352 | ingress-verify-fix.md
- ingress-verify-dedicated-pool | Dedicated 4-thread rayon pool for RPC submit verify, isolated from consensus/exec | merged | inclusion 34% -> ~100%; unique throughput unchanged | ingress-verify-fix.md s353
- ingress-admit-sub-timers | Insert and forward sub-histograms for the admit loop | merged | admit 42 ms/action named | ingress-verify-fix-impl.md
- ingress-per-action-crypto-tuning | Optimize PlaceOrderBatch EIP-712 hashStruct (buffer reuse, single keccak pass), byte-identical | rejected | real cost 5.8 ms vs ~65 ms contention tax | ingress-cpu-supply.md B4
- ingress-enqueue-then-ack | Return after decode screening; verify and admit on a server-side bounded queue; clients poll status | proposed-not-done | API change; biggest pipeline-fullness win est. | ingress-verify-fix.md option C
- ingress-cpu-pinning | taskset/cpuset: node on dedicated cores, framework noise confined to others | proposed-not-done | runbook prepared s356, never applied | ingress-cpu-supply.md B1
- ingress-bincode-endpoint-benches | Bench via torus_submitNativeActionsBin to drop hex plus JSON parse (~30% of real verify CPU) | proposed-not-done | ~1.9 of 5.8 ms per bs500 action | ingress-cpu-supply.md B3
- place-order-batch-action | PlaceOrderBatch: many orders under one signature and nonce, flattened into per-market matching | merged | 23,430 orders/s at bs100 | native-200k-throughput-impl.md phase B
- order-caps-per-batch-per-block | NATIVE_ORDERS_PER_BATCH_CAP 1024, NATIVE_ORDERS_PER_BLOCK_CAP 50,000 enforced at RPC and validate | merged | caps cited in bigbody doc | native-200k-throughput-impl.md B3
- antispam-funded-account | Non-exempt action enters pool only if sender holds >= N TRS (A) | merged | refused 239,908 unfunded spam actions | native-antispam-2026-10-04.md
- antispam-cancel-block-share | Cancels take at most 25% of a block's slots; cancels no longer evict orders (C) | merged | stall 142 -> ~73k matched/s with fix | native-antispam-2026-10-04.md
- antispam-per-address-limit | HL-style per-address request allowance, strict exhausted mode, counted on pool entry (B) | merged | buffer 1000 cost -8.4% / -20% honest load | native-antispam-2026-10-04.md
- antispam-per-ip-limit | 1200 weight/min per IP, 100 subs per WS connection, custom accept loop (D) | merged | no cost measured | native-antispam-2026-10-04.md
- admission-count-non-cancels | Backlog admission compares non-cancel pool size, so pooled cancels cannot shed honest orders | merged | spam cell 5,814 -> 72,690 matched/s | native-antispam-2026-10-04.md
- antispam-congestion-share-e | Per-address block-space share = 2x previous-day maker share, proposer-local | queued | after item 6; answer to many funded keys | native-antispam-2026-10-04.md deferred d
- antispam-pool-cancel-quota | At most a share of the pool may be cancels | rejected | dropped in favour of E | native-antispam-2026-10-04.md deferred f
- antispam-durable-counter | Per-address counts survive restart and idle eviction | parked | coordinate with item 6 view | native-antispam-2026-10-04.md deferred a
- antispam-enforce-at-execution | Enforce request limits at execution (HL-like) | parked | consensus change, golden re-pin | native-antispam-2026-10-04.md deferred b
- hl-in-block-order | Executor order: non-GTC actions, then cancels, then GTC/IOC (HL) | parked | consensus change | native-antispam-2026-10-04.md deferred c
- antispam-activation-fee | One-time account activation fee replacing the holding check | parked | before mainnet, with fees | native-antispam-2026-10-04.md deferred g
- cancel-share-oldest-first | Fill the 25% cancel share in arrival order, each sender's cancels in nonce order | merged | expiries 16,168/node -> 0; min 6-10 182k vs 144k | item6-phase3-impl.md, 3f54704e
- cancel-share-higher-pct-or-round-robin | Raise TORUS_CANCEL_BLOCK_SHARE_PCT or per-sender round-robin in cancel share | rejected | not chosen vs oldest-first | item6-phase3-impl.md step 4
- ingress-readview-for-validator-readers | Validator ingress readers (session, funded, duty, oracle, EVM nonce/balance) read a published per-block ReadView | merged | Phase 3 step 2 | item6-phase3-impl.md step 2
- rpc-submit-on-ingress-view | RPC submit checks (session, market, tick/lot, price band) read the ingress view | merged | RED: unknown market_id before | item6-phase3-impl.md step 2
- ingress-view-slices-ingress-cfs | View keeps only ingress CFs per block (FrozenPending::slice); full sets only for newest two | merged | ~885 MB vs ~150-200 MB est. | item6-phase3-impl.md step 2
- mempool-prune-on-view | EVM mempool prune reads the ingress view (R9), flag-gated with EVM fast path | merged | option 0 decision 2a | item7-option0-impl.md
- rpc-tick-lot-intake-check | RPC refuses off-tick/dust and price <= 0 at intake with executor's text | merged | 44b7473, 457fd70 | item6-phase1-impl.md rows 42-46
- validator-duty-exemption-registered-only | Oracle/attest/jail duty actions skip A/B only for registered validators | merged | c580eb0 | native-antispam-2026-10-04.md
- antispam-d-validator-exemption-metric | Measure oracle submissions evicted in pool before deciding per-IP validator exemption | proposed-not-done | before testnet | item6-phase1-impl.md 9.14

## signature/crypto
- verify-cost-breakdown | Measured bs500 verify phases: hex 1.15, parse 0.71, sig 2.43, ser 1.14, keccak 0.40 ms | decided | 5.8 ms real vs 71 ms under preemption | ingress-cpu-supply.md
- hash-wire-bytes-not-canonical-json | Hash raw wire bytes instead of canonical serde_json re-encode | rejected | canonical JSON is hash-identity load-bearing (376144f) | ingress-verify-fix.md
- canonical-json-buffer-reuse | Arena/buffer reuse for canonical JSON encode in verify | proposed-not-done | never profiled to need it | ingress-verify-fix.md option B
- exec-double-verify-trust-cache | Execution skips re-verify of actions already verified at ingress (trust cache) | merged | 7a1b980f default off; deferred while exec idle | ingress-verify-fix.md, leader-rep-kv-cache-impl.md
- batch-signature-one-recover | One ecrecover per PlaceOrderBatch (~200 sigs/block instead of 20k) | merged | 20k ecrecover ~125 ms on 8 cores | native-200k-throughput-impl.md
- evm-overlap-with-sig-verify | Overlap EVM part with signature verification only | rejected | saves <= ~15 ms per EVM block | item7-step3-design.md section 8
- keccak-exec-attribution | Attribute and cut Keccak 10-11% of exec on-CPU (callers lost without frame pointers) | queued | owner-excluded (9m/8e profile) | item6-phase5-impl.md step 0

## execution/matching
- cancel-all-trader-markets-index | Node-local trader -> markets index; cancel-all visits only markets where sender rests, ascending MarketId | merged | phase1 -0.44 ms/blk vs 1.9 gate (missed, kept) | item6-phase2-impl.md P2-1
- cancel-all-index-eager-removal | Drop market from trader set when last order/stop leaves the book (fill path) | rejected | ~0.4 ms left; per-fill cost | item6-phase2-impl.md 9.14
- cancel-all-index-background-prune | Fixed-work per-block prune of stale (trader, market) entries | queued | before mainnet; index 135k-634k entries | item6-phase2-impl.md 9.12
- order-id-to-market-map | Node-local OrderId -> MarketId map so cancel/modify by id touches one book | rejected | upkeep 0.6-1.4 ms/blk vs 12-37 us saved | item6-phase2-impl.md P2-1b
- market-in-order-id | OrderId = market_id << 64 or sequence so by-id actions need no lookup | parked | consensus item later | item6-phase2-impl.md 9.2 option B
- market-in-cancel-action | Cancel names {market, oid} like HL | proposed-not-done | SDK change; modify still needs B/C | item6-phase2-impl.md 9.2 option A
- by-id-action-cost-unexplained | Find where ~2-3 ms per cancel/modify-by-id goes (probes < 1%) | proposed-not-done | needs perf cell | item6-phase2-impl.md row 28
- foldhash-exec-maps | Exec-path HashMap/HashSet SipHash -> alloy foldhash map seeded per process | merged | engine -8.59 ms/blk, matched/s 1.036x | item6-phase2-impl.md P2-5
- fxhash-nonuser-keys | FxHash/raw bytes for maps whose keys users cannot choose (MarketId, sequential OrderId) | proposed-not-done | +3-8% est.; flood-unsafe for user keys | item6-phase2-impl.md 9.5 option B
- ahash-exec-maps-s82 | ahash on exec maps | measured-no-gain | hash CPU 3.06 -> 1.88 CPU-s/1M, matched/s flat | item6-phase2-impl.md 9.9
- per-level-depth-prefix-sums | same_batch_bid_top_ups sums per-level ask depth once per batch instead of walking all resting asks per bid | merged | top_ups 11.21 -> 0.02 ms/1k; later deleted | item6-phase1-impl.md PF1
- b-blind-sell-topup | Top up each non-pool sell to reserve(B0 x (1+10 bps)) from leftover free margin, replacing same-batch bound | merged | Gate 2 1.097x / 0.997x main | item6-phase1-impl.md 9.10
- second-pass-margin-cut-takers | Sequential second pass re-running margin-cut takers against account free margin (option A / D) | parked | trigger > 0.5% cuts; residual 0.063% | item6-phase1-impl.md 9.10, market-scaling-in-memory-design.md 3.6
- b-bid-same-batch-bids | Count same-batch bids in sell reservation | rejected | reopens griefing | item6-phase1-impl.md 9.10
- maker-overcommit-cross-market-fix | Split maker free margin per book, tighten band, or serialise multi-market makers | proposed-not-done | over-commit possible; counter built b1adeee9 | item6-phase1-impl.md 9.11
- offtick-dust-prebook-reject | Reject off-tick/dust/price<=0 orders before the book on all paths | merged | +3-5% under off-tick spam est. | item6-phase1-impl.md 9.2
- per-action-results-failures-only | Store failed action results only, on flush worker | merged | measured no cost | item6-phase1-impl.md 9.2
- store-every-action-status | Persist every action's status | rejected | 13-26 GB/day | item6-phase1-impl.md 9.2
- matching-per-fill-p1-p4 | One HashMap entry per trader, exact i128 fast path in checked_mul/div, loop invariants, no second position read | merged | checked_mul 1.12 -> 0.26 ms/1k | item6-phase1-impl.md 9.1
- stops-dirty-flag-diff-stop-rows | Skip per-dirty-market stop prefix scan when stops unchanged | rejected | ~0.06 ms per native block | item6-phase2-impl.md
- open-order-count-stored | Per-user stored open-order counter fed by book logs (Phase 4 step 5) | measured-no-gain | net -1.18 ms [-3.41, +1.05]; not merged 6288ebe8 | item6-phase4-impl.md, item6-phase2-impl.md row 76
- open-order-count-via-trader-markets | Count open orders by probing only books listed in TraderMarkets index | proposed-not-done | open_order_counts 1.1% of exec | item6-phase5-impl.md step 4 option b
- cloid-engine-cost-fixes | Pre-size claimed set, resolve cloid log in worker, skip repeated lookup, packed 40-byte key cheaper hash, forget in one lookup | merged | c100 +5.6% matched/s; 0be7d6ae | item6-phase2-impl.md row 71
- cloid-remaining-exec-sites | Cut cloid index upkeep 0.140, pre-pass 0.167, cleanup 0.057 ms/1k on exec thread | proposed-not-done | not item 6 | item6-phase5-impl.md section 5
- book-mode-classic-vs-mode3 | Keep Classic book layout on testnet; mode 3 only with deep-book evidence | decided | mode 3 flush 1.66-1.95x, matched/s n.s. | item6-phase2-impl.md row 41, 9.13
- mode3-deep-book-revisit | Re-measure mode 3 vs Classic with a deep-book shape before mainnet | queued | owner-excluded (9n deep-book) | item6-phase2-impl.md 9.13
- book-rows-from-dirty-set | Classic book rows emitted from per-site dirty sets at freeze instead of save_order_books diff | parked | built on feat/item6-phase5-step3, never run | item6-phase5-impl.md step 3
- liq-no-separate-account-index | Liquidation candidates = round-robin walk of CF_NATIVE_POSITIONS from a cursor, SCAN 2048 / ACT 64 | merged | full sweep of 100k in ~49 blocks | liquidation.md C1, D5
- liq-bounded-range-iterator | Bounded range iterator for the liquidation positions walk if it shows in perf | proposed-not-done | follow-up R1 | liquidation-impl.md R1
- adl-work-budget | ADL ranking and block B charged to per-block budget W=100,000 | merged | a746c408; HL 100k block B 128.8 ms | item6-phase1-impl.md 9.16
- c2-holder-sets-hashset | Per-market holder sets as foldhash HashSet (sorted only in holders_with) | merged | apply 0.512 -> 0.451 ms/1k at 30% toggle | item6-phase2-impl.md row 40
- position-v2-savings | Position v2 layout savings plus C2 holder-index fix after bisected regression | merged | together 1.018x vs bf2edda6 | item6-phase2-impl.md 9.11
- exact-cost-basis | Store exact cost_basis next to entry_price; one rounding per fill | merged | value-sum drift 0 (was 94 raw) | item6-phase1-impl.md 9.16
- drop-stored-entry-price | Derive entry price from basis at ~24 reader sites | parked | option B open | item6-phase1-impl.md 9.16
- reject-entry-canonical-empty-message | Rejected result stores empty message when canonical; reader restores; Cow error strings | merged | IOC no-fill entry 98 B -> 19 B | item6-phase1-impl.md row 50
- reject-entry-v4-compact | v4 compact rejected-order entry without message/length bytes | proposed-not-done | ~19 B -> ~13 B | item6-phase1-impl.md 9.15
- cancel-all-cancels-stops | User CancelAll also cancels pending stops and releases margin | merged | liquidation reuses helper | liquidation.md C4

## settlement/margin/liquidation
- resident-rows-r | Complete resident BTreeMap layer R of positions and balances, applied per block delta, read under overlay | merged | DB reads per 20 blocks 1308 -> 0 | item6-phase1-impl.md C1, market-scaling-in-memory-design.md
- read-through-lru-residency | Bounded LRU residency instead of complete R | rejected | misses still hit RocksDB (D3) | market-scaling-in-memory-design.md D3
- typed-caches-phase1 | Typed caches inside executor instead of raw-bytes R | rejected | every writer must be routed (D2) | market-scaling-in-memory-design.md D2
- block-mark-table | Per-block mark table with version replaces per-batch BatchMarks memo | merged | ccdb59b | item6-phase1-impl.md C2
- sums-cache-s1 | Cache per-trader position sums computed by AccountView::build over R, keyed by mark version | merged | margin gate missed 2.46 vs 1.5 ms/1k | item6-phase1-impl.md C3
- no-balance-mirror-s2 | Balances stay point reads from R; cache only position/mark-dependent sums | decided | owner s89 | item6-phase1-impl.md section 2
- stored-position-terms-o1 | Store per-position margin terms for re-values | parked | re-value share ~0 measured | item6-phase1-impl.md O1
- partial-revalue-o2 | Re-value only touched markets: sums - old terms + new terms | parked | needs O1 | item6-phase1-impl.md O2
- r-hash-index-o3 | Hash index over R entries for point lookups | parked | R lookups never showed | item6-phase1-impl.md O3
- liq-healthy-certificates-l2 | Per-trader "healthy at mark version V" certificate to skip valuation | rejected | saves ~1 ms per empty block after E2 | item6-phase1-impl.md row 71
- liquidation-l1-from-r | Liquidation walk values traders from sums cache and R in memory | merged | tail 9.83 -> 6.93 ms/1k (gate missed) | item6-phase1-impl.md C4
- skip-parent-layer-for-r-cfs | Overlay skips parent layer for R's CFs (R already holds it) | merged | C6 B0 | item6-phase1-impl.md 5.2
- a-lite-written-trader-sums | Written trader sums = memo - R-row terms + pending-row terms; removes rebuild cliff | merged | C6 | item6-phase1-impl.md 5.2
- per-batch-dirty-memo-worker-maker-cache | Per-batch memo for dirty traders, frozen dirty flag, per-worker maker cache before global mutex | merged | C6 D | item6-phase1-impl.md 5.2
- decoded-positions-per-trader-c7 | Decoded per-trader positions in R slot updated by delta; valuations read structs | merged | 0.508x -> 0.638x with C6 | item6-phase1-impl.md C7
- m1-margin-phase-cuts | M1 margin-phase cuts plus listing/tick rows | merged | 0.648x -> 0.760x main | item6-phase1-impl.md
- end-resident-on-worker | Run end_resident apply on a worker; begin_resident moved just before new_env | merged | hides ~76-95 of ~105 ms/block | item6-phase1-impl.md 9.7
- end-resident-reuse-decoded | into_cache reuses C7 decoded positions plus timers | merged | end_resident 21.1 -> 14.2 ms/block | item6-phase1-impl.md 9.7
- resident-apply-move-buffers | Move delta buffers into R instead of copying | rejected | upkeep doubled 25-29 ms, tail 2.5x | item6-phase1-impl.md row 9
- resident-apply-inplace-copy | In-place copy in ResidentRows::apply, skip change list when carry off | measured-no-gain | no measurable gain | item6-phase1-impl.md row 58
- r-rebuild-faster-single-pass | Faster single-pass R build at boot or guard trip | proposed-not-done | 2.07 s per 1M rows | item6-phase1-impl.md row 7
- oracle-single-scan-e1 | One sub-range scan per block for oracle prune plus aggregation; mark table from it | merged | oracle step 19 -> 1.8-2.0 ms | item6-phase1-impl.md E1
- liq-cf-resident-e2 | Liquidation CF (cooldown, pending, cursor) moved into R; sorted trader slice | merged | walk 232 -> 15 ms | item6-phase1-impl.md E2
- oracle-market-rows-resident-e3 | Oracle sub and market rows resident in R (R = 5 CFs) | merged | 8b0673d | item6-phase1-impl.md E3
- margin-config-cache-e4 | Cache margin_configs in the slot | rejected | ~0.4 ms; five call sites | item6-phase1-impl.md row 68
- agg-rows-rewrite-only-on-change | Oracle agg rows rewritten only when inputs change (not every block) | proposed-not-done | consensus change; ~300 rows/block | item6-phase1-impl.md E1-E4
- liq-sweep-skip-by-bound | Skip traders whose health is provably unchanged under the mark move | rejected | saves ~0 at walk 0 | item6-phase2-impl.md P2-4, row 36
- liq-revalue-moved-markets-only | Re-value only markets whose mark moved | rejected | bench moves all 300 marks | item6-phase2-impl.md 9.4
- delta-sums-decode-reuse | Reuse decoded records in delta_sums | measured-no-gain | 0.63 ms/blk, decode ~0.12 | item6-phase2-impl.md row 42
- cooldown-pending-row-index | Node-local index of cooldown/pending rows to skip 2 reads per healthy trader | measured-no-gain | 0.35 ms/blk | item6-phase2-impl.md row 42
- batch-cache-flush-p2-2 | write_cf_raw_many: one lock/intern per CF batch for PositionCache/BalanceCache flush | rejected | cache flush +2.85 ms (+24%), reverted | item6-phase2-impl.md rows 34-35
- streaming-cache-flush | Encode plus insert per row under one lock (streaming P2-2) | queued | backlog; needs microbench at ~21k rows | item6-phase2-impl.md 9.16
- users-record-per-trader | One typed UserState per trader replaces raw R positions/balances, sums, holder sets (Phase 4) | merged | 300m +3.01%, CPU -2.38%; 10m -2.75% | item6-phase4-impl.md, item6-phase2-impl.md row 75
- users-btreemap-shape | BTreeMap<Address, UserState> serving traders_after directly (HL shape) | rejected | tree descent per read, walk over balance-only | item6-phase4-impl.md decision 3
- users-per-user-memo-cell | Per-user memo cell in UserState instead of per-block Mutex HashMap | rejected | memo 0.18% of exec | item6-phase4-impl.md decision 4
- position-core-no-trader-market | PositionCore drops trader and market fields (-16 B) | rejected | ~2 MB at bench shape | item6-phase4-impl.md section 5
- p4-10m-gap-node-build | Explain Phase 4 10-market -2.5% gap located in node build (fewer actions per wave) | proposed-not-done | node -2.51%, bench +0.28% | item6-phase2-impl.md row 78
- margin-sums-slimming | Keep sums only for recently active users or store inline in UserState | parked | light user 726-819 B vs 600 B target | item6-phase4-impl.md step 6, item6-phase5-impl.md step 5
- inline-sums-per-fill | Update crab summary inline per fill within mark version | parked | BlockSums ~0 of exec | item6-phase5-impl.md step 5
- typed-oracle-market-tables | Marks and market params as typed tables following their rows | parked | needs session-read measurement | item6-phase5-impl.md step 6
- typed-dirty-sets-shadow | UsersMut plus per-key dirty sets serialized once at freeze (shadow only) plus 10k-block differential | merged | d3cab057; 10k blocks no divergence | item6-phase5-impl.md step 1
- hot-writers-mutate-users-in-place | Settle and balances mutate Users in place, caches and flush deleted, write-through for cold writers | measured-no-gain | +5% exec CPU/fill, blocks/s halved, p50 18 -> 92 ms | item6-phase5-impl.md step 2
- phase5-drain-encoding-on-w | Move dirty-set drain encoding to flush worker before re-A/B of step 2 | proposed-not-done | 0.130 of +0.143 ms/1k | item6-phase5-impl.md status
- dirty-set-hash-sort | Dirty sets as hash sets sorted at drain, not BTreeSets | decided | 51-52 vs 65-76 ms microbench | item6-phase5-impl.md step 1
- dirty-drain-fixed-size-keys | Sort fixed-size keys instead of Vec<u8> or drain on a worker | proposed-not-done | drain cost high on one block | item6-phase5-impl.md step 1
- typed-all-native-state | Type all native state (nonces, sessions, staking, governance) | rejected | cold, not on exec profile (decision 1b) | item6-phase5-impl.md decision 1
- evm-typed-undo-log | Write-through during EVM txs with typed undo log mirroring revm checkpoints | rejected | EVM blocks rare; journal handles reverts | item6-phase5-impl.md decision 4
- liq-cooldown-parity-x-b | Whole position during 30 s cooldown; positions > 100k chunk 20% each, smaller whole | merged | from 44 HL liquidated accounts | item6-phase1-impl.md row 28
- l3-every-account-every-block | Liquidation checks every account every block | parked | owner-excluded; testnet genesis with D12 | market-scaling-in-memory-design.md 3.6
- liq-stress-full-load | Liquidation stress cell (larger walk or accounts near maintenance) | queued | no cell ever fired a liquidation | item6-phase1-impl.md row 76

## state/storage/rocksdb
- incremental-evm-root | reth StateRoot/sparse trie over persisted CF_TRIE_* updated from BundleState dirty set, in commit WriteBatch | merged | 3.19 ms at 1M accounts vs 6.98 s full scan | incremental-state-root-impl.md, incremental-state-root-native-impl.md
- native-bucketed-merkle-root | 65,536-bucket depth-16 Merkle over (cf,key) with bucket-ordered mirror CF; O(changed buckets) | merged | est 1M entries ~200 ms -> 1-6 ms | incremental-state-root-native-impl.md
- native-root-smt | 256-deep sparse Merkle tree for native root | rejected | ~256 node writes per changed key | incremental-state-root-native-impl.md
- native-root-sorted-leaves | Plain Merkle of sorted leaves | rejected | insert shifts positions, O(total) | incremental-state-root-native-impl.md
- in-memory-trie-rebuilt-at-boot | Whole state trie in RAM, rebuilt O(state) at boot | rejected | slow rejoin, RAM grows | incremental-state-root.md option B
- root-scan-off-consensus-thread | Compute full-scan root for N-k on exec thread with rayon, embed lagged root | rejected | still O(state); band-aid | incremental-state-root.md option C
- native-trie-maintenance-off | TORUS_NATIVE_TRIE_MAINTENANCE off by default (no production reader) | default-on | ~160 ms/block on W; main -12.5% when on | item6-phase1-impl.md 9.3
- native-root-in-blocks-decision | Delete orphaned native trie paths or restore a native root in blocks for light clients | proposed-not-done | decide before mainnet | item6-phase1-impl.md 9.14
- coalesced-checkpoints-phase3 | Since-checkpoint layer L, one atomic checkpoint every 15 s or attest height, replay from marker on crash | merged | CPU -9.72%, writes -54.8%, matched +8.8% | item6-phase3-impl.md, dc8116d5
- checkpoint-interval-5s | Checkpoint every 5 s instead of 15 s | measured-no-gain | RSS -0.3 GB, CPU +1.1%, writes +10.8% | item6-phase2-impl.md row 63
- checkpoint-every-100-blocks-proto | Throwaway prototype: all state rows one batch per 100 blocks | tried-inconclusive | CPU -4.9%, matched +4.7%; led to un-shelve | item6-phase2-impl.md rows 47-48
- checkpoint-book-blobs-only | Coalesce only Classic book blobs | measured-no-gain | CPU -2.98%, matched +2.88% n.s. | item6-phase2-impl.md row 47
- validator-checkpoint-15s-default | Validators default 15 s interval, RPC nodes 0 (per-block flush) | default-on | d3cba5e2 | item6-phase3-impl.md step 6
- checkpoint-max-bytes-cap | TORUS_CHECKPOINT_MAX_BYTES layer byte cap trigger | merged | knob, unset in benches | item6-phase3-impl.md
- merge-wait-deferred-part | Exec joins only small part of previous layer merge; positions/balances merged as deferred part during next block | merged | settle wait 23.4 -> 3.1 ms, blk/s +11.3% | item6-phase2-impl.md row 62
- layer-per-cf-rwlock-or-persistent-map | Per-CF RwLock on L with batched merges, move entries instead of clone, or persistent map | proposed-not-done | options listed, merge-wait chosen | item6-phase3-impl.md step 4
- ckpt-drop-thread | Free C, replaced sets and old views on a torus-ckpt-drop thread | merged | 5.0-5.5 CPU-s per cell off exec | item6-phase3-impl.md step 4
- narrower-evm-barrier | Write only the part of L that EVM reads before an EVM block | rejected | DB at two heights; same write | item7-step3-design.md option N
- pruner-clamp-applied-marker | Pruner cutoff = min(current - retention, applied marker) | merged | a9120a10 | item6-phase3-impl.md task 1.1
- trade-history-retention | --trade-retention-blocks prunes trade rows below cutoff on RPC nodes | merged | b26314d9 | item6-phase3-impl.md step 3
- trade-history-off-validators | TORUS_TRADE_HISTORY=0 on validators | default-on | CPU -11.9%, matched/s +8.2% | item6-phase2-impl.md row 44
- wal-budget-2048mib | RocksDB WAL budget 2048 MiB (fewer flushes/compactions) | decided | CPU -6.3%, DB open 1.3 -> 4.0 s | item6-phase2-impl.md rows 43-45
- wal-compression-zstd | TORUS_ROCKSDB_WAL_COMPRESSION=zstd knob | merged | bytes -69..74%, CPU +1.3..2.4%, off by default | item6-phase2-impl.md rows 43-45
- append-cf-lz4 | LZ4 codec for trades/DA append-only CFs | rejected | CPU -3.5%, SST growth +48% | item6-phase2-impl.md row 43
- append-cf-zstd1 | ZSTD level 1 for append-only CFs | measured-no-gain | 0.00 CPU change | item6-phase2-impl.md row 43
- change-set-log-file | Per-block change-set log file plus checkpoints for fast restart (D5 c) | parked | only if restart too slow | market-scaling-in-memory-design.md D5
- own-snapshot-format | Own canonical sorted snapshot file (D7 b) | parked | with Phase 5 or bootstrap | market-scaling-in-memory-design.md D7
- p2p-snapshot-serving | Node serves snapshots over P2P/HTTP (D9 b) | parked | out-of-band first | market-scaling-in-memory-design.md D9
- auto-snapshot-creation | Wire SnapshotManager to create snapshots after checkpoints | parked | not building | item6-phase3-impl.md section 5
- in-memory-evm-state | EVM state in memory (D11 c) | rejected | unbounded; EVM rare | market-scaling-in-memory-design.md D11
- nonce-window-d12 | Time-major nonce key, per-block range prune below block_ms - 60 s | merged | b7b5bd04; ~20M rows/day avoided | market-scaling-in-memory-design.md D12
- typed-or-in-memory-nonce-set | Typed live-nonce set or full consumed set in memory | rejected | decision 2a; RAM grows forever | item6-phase5-impl.md decision 2
- resting-order-cap | Global or per-market resting-order cap sized at ~400 B/order (D14 b) | decided | 64 GB per validator target | market-scaling-in-memory-design.md D14
- compiled-defaults-bench-config | Pipelined write, resident books, native root cache, parallel settle, 256 MB member cache on by default | merged | 8f131571 | item6-phase2-impl.md 9.13
- book-mode3-compiled-default | TORUS_BOOK_ROWS=3 as compiled default | rejected | mixed-fleet fork risk, reverted f80ef36c | item6-phase2-impl.md 9.13
- order-range-forced-compaction | Forced background compaction of order range keeps tombstones bounded | merged | 1,000 cancels/block flat ~90-100 ms | item6-phase1-impl.md 9.16
- q6-checkpoint-slicing | Slice checkpoint writes across the window (A1) | parked | owner-excluded | item7-step3-design.md 4.4
- getorderbook-tombstone-compaction-delete | Delete per-market tombstone compaction with getOrderBook removal | decided | owner s123 decision 6 | item7-step1-design.md

## checkpoint/hash
- running-hash-prev-in-memory | h(n-1) kept in memory on W instead of META read | merged | needed for checkpoint interval | item6-phase3-impl.md task 1.3
- digest-and-checkpoint-jobs | W gets per-block Digest jobs and Checkpoint jobs; channel bounded (128) with one checkpoint in flight | merged | rendezvous blocked E before | item6-phase3-impl.md task 1.5
- attest-height-forced-checkpoint | Every h % 100 == 0 forces a checkpoint so attestations are never delayed | merged | 94% of checkpoints at attest height | item6-phase3-impl.md
- lthash-snapshot-trust | Full-state LtHash in AttestStateHash for snapshot trust (D8 b) | parked | before public bootstrap | market-scaling-in-memory-design.md D8
- per-trader-hashed-format | Per-trader rows on disk and in the hash (D4 b) | parked | fresh genesis only | market-scaling-in-memory-design.md D4
- evm-trie-on-w-window | EVM trie computed on W in checkpoint batch from per-block hashed inputs | merged | option 0 decision 5a | item7-step3-design.md, item7-option0-impl.md
- evm-trie-best-effort-or-stop | Best-effort EVM trie resync, or stop maintaining EVM trie | rejected | stale trie on crash / no root | item7-step3-design.md question 5
- incremental-root-oracle | Runtime incremental == full-scan oracle under debug or TORUS_INCREMENTAL_ORACLE | merged | determinism gate | incremental-state-root-impl.md

## serialization/wire
- bincode-ingress-endpoint | torus_submitNativeActionsBin bincode submit path, hash-identity gated | merged | unbenched at the time | ingress-cpu-supply.md
- eip712-batch-hash-shared-helper | EIP-712 array hash for PlaceOrderBatch reusing per-order struct hash helper | merged | one signature per batch | native-200k-throughput-impl.md B2
- borsh-overlay-write-path | Remove per-write overlay lock/alloc/Borsh on hot path (Phase 5 target) | parked | borsh 5%, put_cf_raw 5.4% of exec | item6-phase5-impl.md
- evm-tx-single-canonical-bytes | Reject EVM txs with trailing bytes at admission only | merged | one canonical byte form | item7-step2-impl.md decision 4

## threading/runtime/scheduling
- persistent-exec-worker-pool | Dedicated rayon exec pool replaces per-block thread::scope spawns in match and settle | merged | forks 44-46 -> 0; matched/s -1.2% n.s. | item6-phase2-impl.md P2-3, 56914dd1
- std-only-pool-or-merge-spawn-sites | Small std pool or merge the 7 scoped spawn sites | rejected | not chosen vs rayon pool | item6-phase2-impl.md 9.3
- evm-fast-path-option0 | EVM blocks take the pipelined path: EVM reads via layered overlay, rows into frozen set, trie and receipts on W | default-on | matched/s 1.079x, blocks/s 3.53x, p99 0.496x | item7-step3-design.md, item7-option0-impl.md
- evm-raise-n-option-s | Fewer EVM blocks by raising cadence N | rejected | closed s136, superseded by option 0 | item7-step3-design.md option S
- evm-hl-order | Trading first, then EVM over post-trade state, then EVM -> native | rejected | parity only, forecloses C0 | item7-step3-design.md option HL
- evm-same-block-native-t | Non-order EVM -> native kinds drain in the same block with own seq ranges | proposed-not-done | owner-excluded (phase T); genesis-bound | item7-step3-design.md option T
- evm-own-thread-c0 | EVM part on helper thread over block-start view, joined before first EVM-domain action | parked | trigger: EVM > ~3% of exec | item7-step3-design.md option C0
- evm-thread-deferred-effects-c1 | EVM thread with native EVM-account effects deferred a block | rejected | EVM-visible, no faster than C0 | item7-step3-design.md option C1
- evm-beside-next-native-d | HL order with EVM of h beside native of h+1 | rejected | XL+, effects slip to h+2 | item7-step3-design.md option D
- epoch-boundary-fast-path | Epoch boundary blocks pipelined (TORUS_EPOCH_FAST_PATH) | default-on | ~1 s barrier wait per 15 s window before | item6-phase5-impl.md step 0
- pipeline-rendezvous-to-bounded | E to W channel from sync_channel(0) to bounded queue | merged | E no longer blocks for whole write | item6-phase3-impl.md section 3
- exec-pool-not-shared-with-ingress | Exec pool threads never shared with ingress or gossip verify | decided | global pool starved consensus (s352) | item6-phase2-impl.md P2-3
- jemalloc-compiled-in | tikv-jemallocator as torus-node default allocator | merged | RSS growth halved; CPU +2.7..4.3%/fill | item6-phase2-impl.md row 64
- jemalloc-decay-tuning | MALLOC_CONF background_thread:true, dirty_decay_ms:1000 | measured-no-gain | RSS -1.72 GiB but CPU +4.6%, matched -2.5% | item6-phase2-impl.md row 70
- glibc-vs-jemalloc-preload | Allocator A/B via LD_PRELOAD on Phase 3 step 2 | tried-inconclusive | je matched +3.6% but CPU +4% at equal fills | item6-phase2-impl.md row 56

## build/compiler/allocator
- dev-profile-line-tables-only | Dev profile line-tables-only, no debuginfo for deps | merged | test build 27.8 -> 10.8 GB | item6-phase1-impl.md 9.16
- old-main-plus-jemalloc | LD_PRELOAD jemalloc on pre-item-6 main to split allocator from item 6 | measured-no-gain | ~1.11x by using more CPU | item6-phase2-impl.md row 73

## harness/bench
- moving-marks-walk | Deterministic mean-reverting mark walk (UB_MARK_WALK, ORACLE_WALK_BP) | merged | fixed marks hid re-value cost | item6-phase1-impl.md step 0.5
- deep-level-ubench-shape | UB_DEEP_LEVELS shape so ubench sees deep-level costs | merged | shallow levels hid top_ups cost | item6-phase1-impl.md PF1
- ubench-real-market-rows | Real market rows by default (UB_REAL_MARKETS) | merged | fake rows added ~6 ms/block | item6-phase1-impl.md row 69
- ubench-gates-as-ratios | Ubench gates as ratios to base on same machine | decided | ozarchy 2-4x faster than 18c | item6-phase1-impl.md 1.2
- storage-read-counter-test | Counting backend asserts zero DB reads of R CFs on margin/match/liq paths | merged | 8814fc7 | item6-phase1-impl.md step 0.2
- bench-max-in-flight | MAX_IN_FLIGHT=4 plus OPEN_ORDER_BUDGET=900 standard shape | merged | open-limit rejects 39-43% before | item6-phase1-impl.md 9.15
- bench-inflight-commit-tail | Bench in-flight tail released on committed bodies, not eth_blockNumber | merged | step 1 offered load fell 13-16% | item6-phase2-impl.md row 52
- bench-cancel-by-id-cell | Cancel/modify-by-id fractions using own ids via getOpenOrders | merged | 1.22 by-id actions per block | item6-phase2-impl.md row 5
- bench-spam-cancel-mode | Spam cancel-all keys funded/unfunded at fixed rate | merged | ce326cd | native-antispam-2026-10-04.md
- bench-exec-path-merge-rule | Merge judged on exec-thread CPU per fill, blocks/s, p50/p99, timeouts | decided | owner s128 | item7-option0-impl.md section 6
- bench-dup-body-accounting | Fix bench-throughput counting duplicate block-body inclusions | proposed-not-done | included 1013 > submitted 460 | ingress-verify-fix.md, ingress-cpu-supply.md
- bench-offbox-quiet-host | Client off-box, quiet larger host for honest numbers | proposed-not-done | host load 31-38 on 8 cores | native-200k-throughput-impl.md phase D
- toolshed-busy-core | Fix MCP gateway poll loop burning one full core on bench host | proposed-not-done | 100% core for 6 days | ingress-cpu-supply.md s356 audit
- bench-1m-genesis-cell | 1M accounts / 1M resting orders cell for RSS and restart | queued | owner-excluded (9n 8f) | item6-phase5-impl.md step 7
- node-bench-swap-10m | 2x2 node/bench build swap to localize 10-market gap | measured-no-gain | node -2.51%, bench +0.28% | item6-phase2-impl.md row 78
- per-wave-phase-breakdown | Per-wave phase breakdown node A vs B from 1 Hz data | proposed-not-done | next step after swap | item6-phase2-impl.md row 78
- d1-mixed-evm-load | Mixed-load bench with EVM transfers vs none | decided | -12.4% matched/s from barrier (Q5) | item7-step3-design.md
- liq-sweep-walk10-cost | Walk-10 vs walk-0 liquidation cost measurement | decided | sums 6.5 -> 24.2 ms, matched 0.975x | item6-phase2-impl.md 9.4

## rpc/streams
- rpc-gas-defaults-header-limit | eth_call/estimateGas default and cap gas at header limit | merged | 30M default failed at 3M header | item7-step1-impl.md E13
- getorderbook-precompile-removed | Remove getOrderBook selector from 0x0800 | merged | Classic reverted on real books | item7-step1-design.md
- getopenorders-removed | Remove getOpenOrders read (always empty) | merged | s99 | item6-phase1-impl.md 9.16
- bbo-precompile-rows | Best bid/ask row per dirty market (CF_NATIVE_BBO) plus 0x080e reader at 16,440 gas | merged | step 2 | item7-step2-design.md
- read-precompile-gas-metering | Reads 16,400 base, 500 per scanned row, 20 per word | merged | cold block 170.7 -> 33.7 ms | item6-phase1-impl.md 9.16
- read-precompile-gas-hl-formula | HL 2000 + 65 x (in + out bytes) read gas | parked | only if read cost falls | item7-step2-design.md decision 3
- eth-head-applied-marker | EVM state and receipts visible at next regular checkpoint; eth head = applied marker | merged | lag up to ~15 s | item7-step3-design.md question 4a
- eth-reads-through-view | eth reads via published view (storage, code, receipts in ingress CFs) | parked | with Phase 5 parked RPC freshness | item7-step3-design.md question 4b
- evm-checkpoint-if-older-than-x | Force checkpoint at EVM height only if last older than X s | rejected | still Q6 traffic | item7-step3-design.md question 4c
- rpc-freshness-hl-way | All RPC reads from latest executed state, not durable DB | parked | RAM and every handler | item6-phase5-impl.md parked 1
- rpc-block-consistent-view-all | Every RPC reader on block-consistent view (D10 a, step 5) | parked | ~40 call sites; only if owner asks | item6-phase3-design.md option A
- validator-rpc-lags-interval | Validator RPC stays on DB and lags up to one interval (D10 c) | decided | RPC nodes keep interval 0 | item6-phase3-design.md option C
- rpc-oi-counters | Per-market open-interest counters instead of full positions scan | proposed-not-done | getOpenInterest full scan | market-scaling-in-memory-design.md section 6
- rpc-user-limits-o1 | getUserLimits O(1) from UserState | proposed-not-done | separate RPC task | item6-phase5-impl.md section 5
- rpc-lane-from-ingress-view | eth_call/estimateGas read sender lane from ingress view | merged | C9 option B | item7-step2-impl.md decision 1

## other
- governance-local-fault-failstop | Governance/staking storage or decode errors latch fatal_error (fail-stop) | merged | f0fc022 | item6-phase1-impl.md row 74
- governance-atomic-payloads | TreasurySpend/PermanentUnlock staged in one atomic_write with Executed status | merged | 720805e | item6-phase1-impl.md row 75
- genesis-fee-split-refuse | Genesis refuses fee_split differing from built-in constants | merged | 185b38fa | item6-phase1-impl.md 9.15
- margin-currency-decision | USDC margin vs TRS with TRS-USD price vs TRS-quoted markets | proposed-not-done | owner open | item6-phase1-impl.md 9.16
- trading-fees | Trading fees on fills, maker rebates, fee asset | parked | before testnet; farmable volume credit | native-antispam-2026-10-04.md deferred e
- native-overlay-everywhere | All native execution paths on chained NativeStateOverlay; disk written only at commit flush | merged | 12 sessions of root divergence; old paths removed | native-overlay-everywhere.md, native-overlay-everywhere-impl.md
- liquidator-vault-genesis-seed | Seed liquidator vault through genesis; HLP-style deposits before mainnet | decided | owner-excluded decision | item6-phase1-impl.md 9.16
