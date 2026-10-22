# Ledger part: memory (Torus-hyperBFT perf ideas mined from the memory store)

Source: ~95 search_knowledge queries (project torus-hyperbft) + get_memory on hits >= 0.25, 2026-10-22, main eb993e3f.
Flag defaults spot-checked read-only on main (on_by_default_flag callers in crates/torus-bridge/src/native_executor.rs; FixedPoint in crates/torus-types/src/lib.rs).
Format: slug | mechanism | status | evidence | source. "(owner list)" = also on owner-list.md exclusions.

## consensus
- defer-leader-commit-feed | Run the leader's on_committed_block feed after the header broadcast plus inline self-vote (TORUS_DEFER_COMMIT_FEED) | measured-no-gain | s66 57.3k vs ctl 51.8k n.s.; later "no throughput gain" | f6fdcdcf641dc3c0, 259902bb44e79b96, 93695d41711b2545
- defer-parent-feed-fix-d | s72 fix D: move parent-body commit feed after proposal broadcast on next leader | rejected | 4/4 cells REJECT dissemination failures; blocking exec send stalls 1.35 s | 495af7804c85cd60, branch perf/s72-defer-parent-feed
- collector-local-advance | s70: collector skips AdvanceView loopback and enters next view locally (TORUS_COLLECTOR_LOCAL_ADVANCE) | rejected | on 68.5k vs off 74.3k; wait moved to parent body | c0b54371b9c8b2c2, branch perf/s70-collector-local-advance
- vote-state-persist-before-send | Persist highest-voted state before sending the vote (safety ordering fix f05b20e) | merged | -8% n.s. (t~1.3); kept for safety | dc61d3392592f04b, 4e9916779431e53c
- vote-db-separate | s67: move vote-state writes out of the shared RocksDB write group into own DB | parked | vote save p99 113->4 ms; matched +0.8% noise | 088def4c13ea36f5, branch perf/s67-vote-db 85a4331
- hotstuff-da-own-db | 71.8 option A: HotStuff block store + mempool DA in their own RocksDB, off the state DB | merged | in-window DB wait -91%; p99 1575->1370; matched +0.65% n.s. | f87e2022ae34a9e0, a44000e0
- commit-manifest-consensus-db | T5.3: commit manifest written to consensus DB so commit hook does not wait on state WriteBatch | merged | in-window p99 917->485 ms; matched 0.997 | 26a1213a033382ba, 72302e3e161f8007, c92599907ac254f5
- dispatch-commit-persist-off-state-db | T5.3 remainder: dispatch_to_exec header+body write still on state DB on consensus thread | proposed-not-done | hook still 1502 ms behind 400k-entry batch in test | c92599907ac254f5
- proposer-commit-on-advance-view | Leader commits on first AdvanceView carrying QC(v) instead of waiting for try_insert_body feed | parked | modelled p99 -9..-13%; matched 0; Tier 5 | 8ab1bc0ecad0d7b1, 0ff99fc53f5ee2a2
- propose-defer-on-exec-queue | App::proposal_backpressure: leader holds proposals while exec queue >= watermark (TORUS_PROPOSE_EXEC_WATERMARK) | parked | owner s126: revisit after Phase 5 queue/lag read | bb664ddf555c4650, 730adda898e04b9c, branch exp/s123-propose-defer-on-exec-queue
- exec-queue-depth-knob | Expose TORUS_EXEC_QUEUE_DEPTH (clamp 1..64) alone, no HotStuff change | parked | fallback only if liquidation lag hurts | bb664ddf555c4650
- nonblocking-exec-dispatch | Decouple hotstuff thread from blocking sync_channel(64) exec send (durable committed queue, TORUS_EXEC_NONBLOCKING_DISPATCH) | parked | 2026-07 audit #1 cause of blk/s collapse; flag default off | 684009eb82de568b, 495af7804c85cd60, 5b133bf0d1f86443
- consensus-reuse-action-hashes | Seed follower PendingProposal from compact hashes; reuse in dispatch prune/mirror; skip DA re-mirror on produce | rejected | hotstuff Keccak 0.07-0.09 cores at 300m, ~1% CPU | 93695d41711b2545, c0185e795bb6a1ff
- next-leader-parent-path-slim | Reuse compact hashes plus parallel bincode decode in NativeDaStore::get_batch on next leader | rejected | DA decode only on body-late views; no pool available | 6544729f533865cb
- async-validate | TORUS_ASYNC_VALIDATE: validate proposal on worker ahead of take_verdict | rejected | worker head start 1-2 ms; cannot recover ~57 ms | 7e308af19c03c308
- flush-on-miss-da-reconstruct | s76 step 2: compact-block DA reconstruct flushes memtable only on miss | merged | reconstruct mean 47-54 -> 20-23 ms; matched noise | 75932ae150171b3f, a21a9faeb89992eb
- parallel-da-reconstruct-prefetch | Parallelise DA reconstruct inside validate_block or prefetch shards at body arrival | proposed-not-done | named as real lever in async-validate review | 7e308af19c03c308
- consensus-direct-fan | TORUS_CONSENSUS_DIRECT_FAN: mirror consensus messages over /torus/direct as well as gossip | rejected | matched -1.1%, CPU-s/1M +3.5% vs header-resend | 65bdaa0cd9bd6133
- dup-native-action-fix | Committed-hash cache rejects re-gossiped committed actions; produce_block excludes actions in uncommitted ancestors | decided | owner: build after item 7 step 2 | ffcae5aa706e675f
- stable-leader-tenure | leader_tenure param to keep a stable leader for several views | parked | old branch never benched | cb3604fd9dd0ffe9, de0d28debfb5e166
- leader-block-build-critical-path | Attack leader block build (231-321 ms/view) since followers wait ~444 ms for proposal arrival | proposed-not-done | load-window attribution n=1 | c0185e795bb6a1ff, e0003c72fde56e9e
- tc-required-dead-leader-skip | TC-required liveness, dead-leader skip K=3, one fresh proposer per view (owner list) | in-progress | owner s136 roadmap | owner-list.md, 93bc000c353eff43
- b1-mode3-b0-binary | b1 book-window binary and qca-timers bisect (owner list parked) | parked | b1 slows Classic native blk/s 0.746x | 0ac37a8bcb9a20a5, 71721f200bc970b3

## pacemaker/timeouts
- timeout-base-1200 | Raise timeout_base_ms 500 -> 1200 so after_vote timeouts stop redoing views | tried-inconclusive | +6.5% matched, timeouts -85%, sign test p=0.125 | 21f77740b402399d
- timeout-cliff-cap100 | s55 timeout-base A/B at cap 100 | measured-no-gain | null result, underpowered | cd44df5b539c9110, 0ee700756bb34f57
- timeout-base-250 | Lower timeout_base 500 -> 250 ms | proposed-not-done | judged low payoff, healthy block 417-486 ms | dcd104ace4ce60b5
- pacemaker-past-deadline-rebase | Rebase entered view deadline when scheduled < now + max_view_time so backed-off grid does not burn next views | in-progress | QCs only every other view after a backed-off view | 4b14780f5a9b6e2e, branch fix/pacemaker-past-deadline
- pacemaker-dead-view-symmetric-rebase | s68 explorer claim: views entered past grid deadline time out instantly | rejected | only 2-5 of ~590 views entered past deadline | 21f77740b402399d, 0ee700756bb34f57
- header-resend-250ms | TORUS_HEADER_RESEND_MS 3000 -> 250: direct re-send of headers dropped by gossip limiter | default-on | lost-proposal timeouts 33/47 -> 3; matched +0.6% | 65bdaa0cd9bd6133, c4e3ea2bb4097c06, 315f7ee81202c74b
- consensus-ratelimit-per-class-buckets | Per-class limiter buckets (header vs AdvanceView), validator-only authors, monotonic per-author view gate, configurable limit | proposed-not-done | 50 msg/s limiter drops headers -> 1.2 s stalls | 10a86b6c15a14f40, 8d5ce8bc7e58a9a2
- hold-light-proposals-under-rate-limit | Pace light/empty proposals so fast-view bursts stay under per-author limiter (min block interval) | parked | 10m p99 1.32 -> ~0.53 s modelled; Tier 5 | ee2fd1ca37ae5e62, bfbd7612197a53cd
- timeout-max-ms-dead-config | timeout_max_ms config is never read | proposed-not-done | s64 explorer finding | 259902bb44e79b96

## dissemination/DA
- hash-only-push-512k | Bench push threshold 8 MB -> 512 KB code default: manifests + pull instead of full-body push duplicating gossip | parked | q7b: matched +4.3%, CPU-s/1M -5.9%; owner call | 1aae7d942596510e, 488eb4680876a4fd, c4e3ea2bb4097c06
- native-prespread-direct-zstd | Send native pre-spread batches as 0xFD direct zstd envelopes, bypassing gossipsub ed25519 sign/verify and relays | proposed-not-done | CPU-s/1M -3.2..-4.4% est; matched 0 | dabcab1cf8cf56a9, db0cb68d48c09fbe, 44dbf09a9b85cdd9
- gossip-zstd-topic | zstd-compressed v2 gossip topic for native batches | rejected | 18x regression; topic removed s364 | e481f18404ae5353, dcd104ace4ce60b5
- body-push-max-64k | TORUS_BODY_PUSH_MAX_BYTES=65536 smaller pushed bodies | measured-no-gain | 59.5k vs ctl 62.9k | 4411c2cff86f17ce
- compact-block-relay | CompactBlock relay of action hashes instead of bodies | merged | already wired 865d4e1 | dcd104ace4ce60b5
- erasure-coded-bodies | RS k-of-n shard dissemination, per-link bytes ~1/n (T3.1, owner list) | in-progress | custody ON -21.6% matched; custody default off | 2d3c5f6d7ed082f7, 98a58bca6cd87b23, dcd104ace4ce60b5
- lazy-shard-custody-serve-time | Compute custody shards at serve time instead of eagerly | rejected | bench pins custody 0, gain 0 by construction | e708738839560b1d
- erasure-merkle-proof-quadratic | merkle_proof rebuilds merkle levels per index and leaf_hash copies shard | proposed-not-done | spotted in r2 triage, unmeasured | 2a8af8fa4fbe5f43
- gossip-early-dedup-before-mirror | Dedup gossip/push duplicates before mirror_to_da and pool hash under write lock | proposed-not-done | push dups 40% of consensus WAL; substitute for full push | 873b7f65c4ceda69, 488eb4680876a4fd
- quic-prefer-aes128-gcm | Reorder libp2p-tls cipher list to AES-128-GCM (VAES) before ChaCha20 | rejected | -0.4..-0.6% CPU-s/1M, below resolution | 8b1039de8a02143b
- gossipsub-explicit-peer-relay-dup | Validators are explicit peers so IDONTWANT never suppresses relays (~1.55x receive) | proposed-not-done | each batch received ~1.55x | db0cb68d48c09fbe
- gossipsub-validation-none | Drop gossipsub envelope signing (ValidationMode::None) | rejected | breaks consensus source auth and msg-id | db0cb68d48c09fbe
- native-da-gc-rh4 | Prune CF_NATIVE_PENDING (DA) below body retention | in-progress | RH4 Codex P1s open; DA never pruned on main | e8d5a7678385766b, 51286ff25300f349

## ingress/mempool
- dedicated-ingress-verify-pool | Separate 4-thread ingress rayon pool so RPC verify cannot starve exec/global | merged | s353 starvation fixed | 51ea60711f30fbe1, 3e3c4cec88c1a48f
- naive-rayon-par-verify | rayon par_iter on global pool for RPC ingress/exec verify | rejected | 529 -> 4034 ms regression | 946c2d3cbd4852df, dcd104ace4ce60b5
- raise-submit-permits | Raise SUBMIT_PERMITS / TxSubmitLimiter for higher ingress | rejected | misdiagnosed; fix was submit batches 10-25 | dcd104ace4ce60b5, 683519accd27c3e4
- ingress-market-spec-cache | Per-ReadView memo of market spec/mark gets in validate_known_markets (~221 gets per batch) | proposed-not-done | ~0.19-0.23 cores/node; CPU-s/1M -3.5..-4.2% | 521c523f25e850bd, 91b0e4a67271b4b8, db71a3ab1482d1ce
- ingress-view-skip-position-slices | Drop CF_NATIVE_POSITIONS from per-block ingress ReadView slices (keep C layer) | parked | RSS -0.15..-0.3 GB; matched ~0; owner deferred | 5bc0002a2c88c186, 091a069d63879027
- arrival-order-pool-selection | Mempool SortKey (prio, seq, sender, nonce) to stop high-address starvation | rejected | caused stall/flood under overload; reverted | c491213040fa61f0
- admission-horizon-limit | Shed non-cancel submits pre-verify when backlog > commit rate x horizon (TORUS_ADMISSION_HORIZON_MS) | default-on | expired actions halved; throughput neutral | f6fdcdcf641dc3c0, 6c5a892954590510
- exec-aware-admission | Count committed-but-unexecuted backlog in admission limit (TORUS_ADMISSION_EXEC_AWARE) | rejected | 2-5 s horizons -33/-51%; stays default off | a55d368e767aabaa, a6049ace023e5d9e, 38e871cba76285a0
- exec-throttle-watermarks | Thin proposals when exec queue exceeds block-count watermarks | rejected | matched -25%, total -44%, more smaller blocks | 272b71ca530df3ae
- small-place-order-charged-budget | Charge proposer block budget by orders for small place actions (TORUS_NATIVE_MIN_ACTION_ORDERS) | rejected | gain 0 on tracked bench (all batch 400) | f1fddcb5ef5c9eda, 9769f127f199bc7c
- block-cap-1000 | Raise NATIVE_TOTAL_BLOCK_CAP to 1000 actions | rejected | wedged chain under 3-box load | db2289e11496cf82, dcd104ace4ce60b5
- block-cap-400 | Block action cap 200 -> 400 | decided | total matched +13% vs c200; cap binds 96% of blocks | 71921a86343c3936, 700c5ced6b791847
- remove-committed-hash-index | mempool remove_committed via hash index / stored restash key instead of O(pool) scan | merged | remove_committed ~30 ms -> <1 ms est | 003abf49ce0492b3, 259902bb44e79b96
- session-owner-cache | Cache session-key owner lookups on exec verify path | merged | P3 exec_verify lever proven | d472420ed313bdbc, a3ce20beb42cf567
- trust-cache-exec-skip | Exec reuses gossip/RPC-verified sender via trust cache keyed by full sig | merged | exec misses ~0 | 36cac2f19fe2d937, c21bed8620285781
- json-ack-serde-keccak | Ingress JSON ack serde plus keccak of ~64 KB JSON per batch | proposed-not-done | 4.5-4.7% of ingress verify | 5fa915a380d61fcf, 91b0e4a67271b4b8

## signature/crypto
- ecrecover-libsecp256k1-backend | Recover via already-linked libsecp256k1 with explicit low-S check, keep k256 parse (TORUS_ECRECOVER) | proposed-not-done | ~0.28-0.33 cores/node; CPU-s/1M -5..-6%; matched 0 | 93fd4bf0284dd712, 4864a390f0af7cd5, a48de6c0b4d50402
- k256-recover-noverify | Drop ecdsa trailing verify_prehash (48% of recover), add explicit is_high | proposed-not-done | CPU-s/1M ~-3.3..-3.5% | 93fd4bf0284dd712, a48de6c0b4d50402
- eip712-batch-item-keccak-x4 | 4-lane AVX2 Keccak for 400 PlaceOrderItem hashes per batch (TORUS_EIP712_KECCAK_X4) | proposed-not-done | CPU-s/1M -3.9..-4.9% at 2-2.5x; microbench gate | 0a51b36b24c7e3b8, 4823ab93b81a2c6d, 3955c6f164a81450
- eip712-typehash-hoist | Hoist EIP-712 typehash/domain work out of per-item struct hashing (s77 74d488f5) | merged | struct hash -39%, gossip-verify CPU-s/1M -25% | 0a51b36b24c7e3b8
- exec-hash-once | Drop duplicate per-action Keccak on exec thread (sort key + trust-cache key computed once) | merged | exec Keccak was 9-10 ms/blk | 642942286640c5b3, 8f93bc91556d42ba, 2c1c949f55b7f821
- exec-action-keys-parallel | Parallel verify pre-pass in eip712 batch_verify (TORUS_EXEC_PARALLEL_KEYS) | measured-no-gain | campaign A noise; merged flag off, dropped | 2d3c5f6d7ed082f7, 9ecbc25cf0300e11
- exec-carried-verified-senders | Carry pool-verified sender from dispatch prune into exec, skip trust-cache key pre-pass and no-op par_recover | proposed-not-done | est exec block -4.8%, matched +3..+5% | 5a8a9cc52915085a, c6513199580db04a
- parallel-verify-zero-code | TORUS_PARALLEL_VERIFY=1 removes no-op global-pool par_recover wait on exec | proposed-not-done | exec waits 2.45 ms/blk mean | 91563c5dc136d778
- double-recovery-fix | Remove second secp256k1 recovery per action | merged | a5445ff | aed2e2359d905040, cd6afb52fae0e0d8
- proposer-attestation | validate_block uses ed25519 proposer attestation instead of per-action ecrecover | merged | S375 | a48de6c0b4d50402, 5a736870b5c3548d
- session-keys-for-crypto | Use session keys to cut per-action ecrecover cost | rejected | per-order serde + struct hash paid anyway | aed2e2359d905040
- ingress-crypto-tuning | Optimise ingress crypto code paths | rejected | ~97% of verify wall is queue/contention | 6b852ecc83246fc3, aed2e2359d905040
- hex-parse-const-hex | Faster hex decode (const-hex) for parse_bytes on ingress | proposed-not-done | hex 6.9% of ingress verify | 521c523f25e850bd

## execution/matching
- parallel-match-setup | Move per-market match setup out of serial match_timer into each market's pool job (TORUS_PARALLEL_MATCH_SETUP) | default-on | single flag +3.05%; D/M/T bundle +8.3..+9.1% | ad331407b13444f2, 8f33f0eab4cc3227, aea37e938b67883e
- sell-top-up-stitch-list | Collect sell top-up candidates at stitch with per-sender cache; batch s92 loop (TORUS_FAST_SELL_TOP_UPS) | default-on | single flag +1.97% | 46551edccddbef01, 8f33f0eab4cc3227
- exec-pool-fine-grain-jobs | K chunks per worker instead of one static chunk per pool phase (TORUS_EXEC_POOL_GRAIN) | measured-no-gain | single flag +0.58% n=1; not merged | 8f33f0eab4cc3227, 0baab8234e5bf138, 2e3bd1d94b49691e
- exec-pool-caller-participation | Exec thread runs pool jobs itself instead of waiting | proposed-not-done | optional arm of grain idea | 0baab8234e5bf138
- parallel-cancel-all | Run cancel-all per-market book groups on exec pool (TORUS_PARALLEL_CANCEL_ALL=8) | default-on | +4.51% matched; block p50 192 -> 55 ms | 2d3c5f6d7ed082f7, 315f7ee81202c74b, c1204b92c0ffe6ce
- parallel-engine-phase2 | Sender-sharded Phase 2 prepare (TORUS_PARALLEL_ENGINE=N) | measured-no-gain | +1.03% alone; stacked on PC+F matched 0.986 | 2d3c5f6d7ed082f7, 81bac2d2363dd979, 92541a79122528bd
- per-market-parallel-matching | MarketWorkerPool matches markets in parallel, LPT by order count | merged | s207 | 3ef2da2abaddc45b, 289a0b9eb82dd3a2
- persistent-exec-worker-pool | P2-3: persistent exec_pool replaces per-block scoped thread spawns | merged | ~0.9-1.4 ms exec wall/blk | e1ea11de3e4e37b5, 4ca47cf72ad7df8b
- match-epilogue-batched-trader-index | Batch TraderMarkets::absorb per trader, gate triggered-stop walk (TORUS_MATCH_EPILOGUE_BATCH) | proposed-not-done | ~-2.8 ms/blk, matched ~+0.85% | b03ebb529b964b47, 53a59ba2a0de8ff6
- parallel-sharded-cloid-upkeep | Shard cloid pre-pass, absorb and stash drain on exec pool (TORUS_PARALLEL_CLOID) | proposed-not-done | +4% at CLOID_SHARE 100; exactly 0 at c0 | e91d60ba98508151, c32b25a20e2ec2fb
- cloid-count-scan-at-c0 | Skip cloid filter().count() scan when no cloid places | proposed-not-done | ~1.0 ms/blk at c0 | 53a59ba2a0de8ff6
- cancel-all-in-place-compaction | Per-level in-place compaction instead of VecDeque::remove shift per cancelled order | tried-inconclusive | 99.3% of cancel-all in book.cancel_all; 2 policies rejected | 18e6df60441768a0, 64cef1cce9ab0689, 769b16efdb48ac87
- deep-cancel-thresholds | Retain-policy thresholds for deep cancel compaction | rejected | -6% | 38e871cba76285a0
- stale-cancel-all-index-prune | P2-1 option B prune stale cancel-all index entries | proposed-not-done | ~0.4 ms/blk est; before mainnet | cb3604fd9dd0ffe9
- fixedpoint-i128-fastpath | checked_mul/div in i128 when product fits, i256 only on overflow | merged | verified torus-types lib.rs:189-199 | 92541a79122528bd
- fixedpoint-u64-fastpath | u64 path when abs product < 2^64; chunked u64 division | rejected | bench products >= 2^64; neutral | 81ee744a45982378
- exec-hasher-p2-5 | Fast seeded hasher (alloy HashMap) for exec maps (s82 ahash lineage) | merged | engine -8.59 ms/blk, matched +3.6% | e441004fb64f2281, ad303af9525e1eb5, cb3604fd9dd0ffe9
- exec-prep-stage | Move verify/sort-key work into a prep stage overlapping end_resident | rejected | overlap recovers only ~0.4 ms | b6d2198bbc027450, 85a53ffe73e49c50
- phase4-step5-counter | Phase 4 step 5 counter change | measured-no-gain | no net gain | 38e871cba76285a0
- s80-noscan | s80 no-scan exec change | measured-no-gain | checked no-gain in s111 sweep | cb3604fd9dd0ffe9
- optimize-matching-code | Micro-optimise matching/order-book code | rejected | matching 4-5% of exec cost (July) | 684009eb82de568b, ff77335e11bcba09
- crab-fixes-3-2a-1 | Overlay range lookup, skip liquidation valuation without marks, one maker account snapshot per batch, same-batch bid bound | merged | crab stack 35x slower -> near main | b727822420474271, 22d33ab79f5048d4, d7adce420f1a9475
- market-sharded-multinode-exec | Shard markets across exec threads/nodes for 200k matched/s | proposed-not-done | single exec thread ceiling ~60-75k (Aug) | d2aea2352eee1b9f, 3fc20985a735b4ec
- block-stm-optimistic-exec | Optimistic parallel execution (owner list: after testnet) | parked | no memory measurement | owner-list.md

## settlement/margin/liquidation
- settle-passb-deferred-results-scatter | Defer pass B results writes to a 1-byte/slot vec, materialise once (TORUS_SETTLE_DEFER_RESULTS) | default-on | single -0.5%; part of +9% bundle | ebcc0c73da696a2f, aea37e938b67883e, 3955c6f164a81450
- settle-passa-capped-chunks | Capped pass A chunking instead of one scoped thread per market | merged | 300m 17.5k -> 26.2k matched (+49%) | b1b9be4e7f1cd773
- settle-passb-parallel-shard-apply | Parallel pass B apply by trader shard with prefix-sum trade_index | proposed-not-done | Aug top-3 next lever | b1b9be4e7f1cd773, 92541a79122528bd
- settle-streamed-l2-1 | Stream settle results to overlap pass B (TORUS_SETTLE_STREAMED, owner batch 10) | in-progress | owner s136 default on | 7088b69389270ba9, 78afc40828eb68bc
- settle-no-merge-l2-2 | No-merge position flush keeping per-market PositionCaches (TORUS_SETTLE_NO_MERGE) | default-on | settle -9.8 ms/blk; pass B -42..-48% | f0b014fffcfaa00e, e41a84a2ea7400f0, 822e72f1edbfeae1
- l2-4-idea | 8k L2.4 settle idea | rejected | s82 precedent | 4ceb062ed559c150
- pos-sums-cache-c3 | PosSums cache (upnl, IM, notional, maintenance) per trader for margin checks | merged | item 6 C3 | e5c40655bec6defd, d74869a1e38917da
- liquidation-l1-over-sums-c4 | Liquidation valuation over sums cache instead of full position reads | merged | item 6 C4 | f1498f77b3600bfa
- liq-scan-parallel-prevalue | Parallel liq_view of the 2,048-trader liquidation window on exec pool, serial fallback | rejected | +0.6-0.8% below CI; Phase 5 rewrites path | 966f49e6b842cec8
- liquidation-health-certificate | Per-trader Healthy-at-mark-version certificate to skip rescans until positions/marks change | proposed-not-done | ~0.8-1 ms per empty block | d2998a49d755d245
- liq-scan-skip-unchanged-traders | Stop scanning 2,048 traders every block (changed-traders scan, fix 2b) | proposed-not-done | liq step 8-13 ms/blk at 300m; 99.5% of 8u tail | 9769f127f199bc7c, c535e154a9938816, 2e94e8f34a014a56
- p2-2-batch-cache-flush | Batch cache flush with pre-materialised rows under whole-batch write lock | rejected | cache_flush +2.85 ms/blk (+24%) | f1b4dee81822e36d
- pass-a-hot-market-trader-shards | Split one hot market's settle plan by trader (TORUS_PASS_A_SHARDS) + fused match plan | rejected | inert at 300m uniform; fusion inside CI | c4ea10ed82bf2d62, eae7902984529bca
- item6-phase5-in-memory-users | Users mutated in place, inline sums per fill, drain encoding to flush worker | parked | step 2 exec CPU/fill +5%; blocks/s 10.8 -> 5.2 | e8d5a7678385766b, ea2ab0652a8c4a85
- drain-encoding-to-flush-worker | Move Phase 5 drain encoding off exec thread to flush worker | parked | 0.130 of +0.143 ms/1k fills | e8d5a7678385766b, 315f7ee81202c74b
- flush-all-remaining-target | Attack flush_all (~0.44 ms/1k fills, ~18% exec on-CPU) after fresh profile | proposed-not-done | no profile after option 0/L3.1/L6.1 | ea2ab0652a8c4a85
- markets-track-trader-buckets | Locality knob, trader-bucketed positions, order index, markets-per-sender K | proposed-not-done | untouched in matched-200k | d2aea2352eee1b9f
- margin-cache-o1 | Balance/margin cache in exec (S412) | merged | | e0f0213c4fe09014

## state/storage/rocksdb
- da-cf-blob-files | Integrated BlobDB on consensus-DB cf_native_pending (append-only, point-read) | proposed-not-done | CPU-s/1M -2.7..-3.2%; shrinks if push dedup lands | f9113815f4efefaa, 38326019aa14d2c2, 005b6c0395887023
- append-cf-lz4-codec | LZ4 codec on append-only CFs (TORUS_ROCKSDB_APPEND_CF_CODEC) | rejected | compaction -1.27 CPU-s/1M but +48% SST; owner rule | cb3604fd9dd0ffe9, f9113815f4efefaa
- wal-compression-zstd | RocksDB WAL compression zstd | parked | owner re-decide after Phase 3 | 38e871cba76285a0, ebcc0c73da696a2f
- fifo-or-universal-compaction | FIFO (ruled out by owner) or universal compaction for append CFs | rejected | FIFO out; universal kept in reserve | f9113815f4efefaa, 4ceb062ed559c150
- waloff-state-write | Disable WAL on state write batch (r9), crash recovery via replay | parked | state_write DB -35%; matched flat | b1b9be4e7f1cd773, 1748fa3c5008c489
- wal-cap-512mb | TORUS_ROCKSDB_MAX_TOTAL_WAL_MB=512 bounds WAL replay | merged | restart DB open 20.5 -> 7.9 s | 7a89b3661de92729, a497db108ef38d7f
- wal-cap-256-or-no-flush-recovery | 256 MiB WAL cap or avoid_flush_during_recovery for remaining open time | proposed-not-done | remaining ~7.9 s DB open | 7a89b3661de92729
- start-networking-before-replay | Bring networking up before DB replay to shorten rejoin freeze | parked | freeze 25-63 s is restart->ready | a2e3c0aaa2e7675b
- global-memtable-cap | Global memtable cap across CFs (s450) | merged | stability fix | ea0252e28a7854b8
- consensus-meta-8mb-write-buffer | 8 MiB write buffer for cf_consensus_meta to flatten propose_build growth | measured-no-gain | slope unchanged; kept as hygiene | 3b6725a3f0bee7a3
- rocksdb-env-knobs-census | Write buffer MB, max bg jobs, subcompactions, bytes_per_sync, L0 slowdown, etc. | measured-no-gain | inert: stall_micros 0, never bind | 700c5ced6b791847
- compact-body-record-from-da | Store hash-list body record (tag 0x02) in CF_BLOCK_BODIES for DA-backed commits | proposed-not-done | CPU-s/1M -1.3..-1.5%; device writes -22% | 7ce3df10fe08cd75, ab99e13bb69f6140
- commit-persist-reuse-wire-body | Reuse received wire body bytes for commit persist | merged | matched-200k r4 | d2aea2352eee1b9f, 1bd5fb2e8aeeb4d4
- body-persist-skip | Remove exec-time duplicate body persist and header persist | merged | duplicates were 11% of exec wall | d06ba5be5a6e85b3, 5b9e93302156fc3e
- bg-writer-chunked-batches | Chunk trade-writer WriteBatch to <=2k KVs to cut write-group head-of-line | proposed-not-done | header put 78-137 ms/blk behind 66k-put batch (Aug) | d06ba5be5a6e85b3
- packed-trade-rows | Packed per-block trade/user history rows instead of per-fill KVs | merged | s78 | bea763693622cd83
- trades-only-history | s77 trades-only persistence | rejected | superseded by packed rows | cb3604fd9dd0ffe9
- separate-db-or-sst-ingest-checkpoint | Separate DBs or SST ingest for checkpoint batch so DA/consensus writes do not queue | proposed-not-done | write waits up to 615 ms in windows | a680b2e5ee3622cf, e4aaa797cb5dcdd9
- dirty-bytes-writeback-tuning | Raise vm.dirty_bytes to avoid writeback throttling | rejected | H1 ruled out: balance_dirty_pages 0 | e4aaa797cb5dcdd9, 38e871cba76285a0
- native-trie-maintenance-off | Native trie maintenance off by default (TORUS_NATIVE_TRIE_MAINTENANCE=1 enables) | merged | 300m main 89.4k -> 100.6k (+12.5%) | 2dfcafdf595b428f, a3eafa98d7fa64f9, 00350532f88e8317
- async-trie-mirror-maintenance | Async trie/mirror maintenance off flush (~100 ms/blk) | decided | superseded by trie maintenance off | 731b5979ee3adb95
- parallel-bucket-hash-8 | TORUS_PARALLEL_BUCKET_HASH=8 parallel trie bucket hashing | tried-inconclusive | +10.8% n=3 (Aug, trie-on era) | d2aea2352eee1b9f
- flush-root-overlap | Overlap trie root with state_write in flush | rejected | standalone refuted, needs 2 batches | 48ac751a041ee3aa
- flush-1deep-worker-pipeline | Flush worker W writes block N while engine runs N+1 (TORUS_EXEC_PIPELINE) | merged | block -27%; matched +2.7% | 63d1f7d0937e3bdd, 48ac751a041ee3aa
- resident-books | Keep order books resident in memory across blocks (TORUS_RESIDENT_BOOKS) | merged | | 381a76a66d6211f1
- resident-books-untouched-advance | Advance resident books without rebuild when untouched | measured-no-gain | -1.5% noise | f11ff3f2238ea0d5
- persistent-books-no-rebuild | Persistent books + next_global_order_id to stop per-block book rebuild | rejected | falsified: book load negligible | 4913e6b6a9aa9b9d, 684009eb82de568b
- stops-dirty-flag | Dirty flag to skip unchanged stop rows in save_books | parked | save_books -18%; matched flat | b1b9be4e7f1cd773
- save-books-parallel-levels | Mode-2 level-row hashing/encoding in parallel (byte-identical) | merged | +32% (Aug) | f6e9deb9baceae94, 199ef652fe9658a6
- level-hash-seq-chunked | Consensus-visible chunked-by-seq level hash (mode 3 book rows) | merged | matched-200k r2; Classic now default | d2aea2352eee1b9f, 42870cbdd6c87bd0
- mode3-book-rows-default | Make mode 3 book rows the default book mode | rejected | matched 1.013x/0.9645x, flush 2x; Classic restored | a3599e0016f8d965
- save-books-workers-4 | SAVE_BOOKS_WORKERS=4 | rejected | -34% cliff (4 workers < 10 books) | d2aea2352eee1b9f, f456b67b3596b431
- parallel-classic-book-save | Parallel presized Classic book blob encode on exec pool, serial puts (TORUS_PARALLEL_BOOK_SAVE) | measured-no-gain | single -0.2%, CPU-s/1M +7.5% | 8f33f0eab4cc3227, 722f6f35f91c91af, 3059a0d8a3c5cade
- book-fast-encode-l3-3 | Exact-size fixed-layout Classic book encoder (TORUS_BOOK_FAST_ENCODE) | default-on | deep save_books -15%; matched +1.4% n.s. | 1ab1db96195f51a8, native_executor.rs:646
- book-save-coalesce-l3-1 | Save Classic books once per window instead of every block when running hash off | merged | deep blk/s 3.42 -> 15.76; matched 1.16x | c535e154a9938816, 04f3910ca0bb5169
- window-committed-books-l6-1 | Hash books only at attest heights so L3.1 works with running hash on (owner batch 10) | decided | 9g: within noise of A, beats C deep 4.8x | 4a70fa90778156a7, 7088b69389270ba9
- l3-2-l3-4-l6-2 | 8k dead ideas: L3.2 needs hash on, L3.4 FIFO ruled out, L6.2 flush_all not from hash | rejected | | 4ceb062ed559c150

## checkpoint/hash
- epoch-boundary-fast-path | Skip pipeline_barrier checkpoint drain at h%100 for no-EVM/no-slash blocks (TORUS_EPOCH_FAST_PATH) | default-on | +1.5% matched; latency p50 57 -> 15 ms | 2d3c5f6d7ed082f7, 315f7ee81202c74b, 7d1b757ad156e67d
- q6-checkpoint-slicing-a1 | 4 MiB checkpoint slices with redo file (TORUS_CKPT_SLICE_MB) | parked | 50 MB cap cut W_db 73%; p99 gap stays ~640 ms | 021942d39154fa27, a12f3b07b272a921
- defer-book-blobs-in-layer-merge | Defer Classic book blobs in checkpoint layer merge (TORUS_CKPT_DEFER_BOOKS) | measured-no-gain | campaign A noise; merged flag off | 2d3c5f6d7ed082f7, 9ecbc25cf0300e11
- checkpoint-interval-5s | 5 s checkpoint interval instead of 15 s | rejected | -0.3 GB RSS, +1.1% CPU, +10.8% writes | 24c15c77e309ce25
- checkpoint-60s-or-staggered | 60 s interval or staggered checkpoints across validators | proposed-not-done | listed as revisit in perf-done map | 38e871cba76285a0
- unordered-write-arm | RocksDB unordered_write for checkpoint | rejected | owner Q6 pick 9: no | a12f3b07b272a921
- running-state-hash-cost | Running state hash computed on flush worker per block | decided | no measurable cost (-2% n.s., CPU equal) | 39a0948b2d47f2bb, 955e0bacc4ad47c5

## serialization/wire
- binary-ingress-bin-endpoint | torus_submitNativeActionsBin bincode submit instead of hex JSON | merged | hex+parse was ~30% CPU/action | 6b852ecc83246fc3, bc33a4d8de84e02d
- classic-borsh-presize | Presize borsh to_vec buffers for Classic blobs (realloc 27% of encode) | merged | folded into L3.3 exact-size encoder | 5fa915a380d61fcf, 1ab1db96195f51a8
- direct-path-zstd | zstd-3 on /torus/direct 2.0 push bodies | merged | ratio 12.1x on bench bodies | db0cb68d48c09fbe

## threading/runtime/scheduling
- cpu-pinning-cpuset | Pin node threads to cpusets on shared host | rejected | -5.7% | cb3604fd9dd0ffe9, 38e871cba76285a0
- hotstuff-cpus-pin | Pin hotstuff thread to dedicated cores (HOTSTUFF_CPUS) without isolcpus | rejected | rq-wait 19-24 -> 32-41 ms/blk | 578faa1f9d718f36
- core-budget-umbrella | CORE_BUDGET with gossip/ingress verify thread knobs | rejected | -67.7% | c6bf7c3fd4132ed5
- dedicated-cores-per-node | Dedicated cores or off-box nodes (contention is 75-85% of ceiling; co-location owner list) | decided | pinned v0 verify 407 -> 106 ms | ff77335e11bcba09, ba0ec1908f90d10d
- bulk-thread-nice | setpriority/SCHED_BATCH for gossip/ingress verify pools (TORUS_BULK_NICE) | rejected | BORE premise unsupported; ~+1% central | e708738839560b1d, a0ece97264c6959a, 12d0cc8f
- verify-pools-parking-executor | Spin-free parking executor replacing rayon for per-action gossip verify and 1-item RPC verify | proposed-not-done | CPU-s/1M ~-1%; matched 0 | bc33a4d8de84e02d, c6bf7c3fd4132ed5, 91563c5dc136d778
- verify-pool-sizing-by-budget | Size gossip/ingress verify pools by CPU budget, cap RPC blocking pool | proposed-not-done | s65 explorer, deprioritised | c6bf7c3fd4132ed5
- idle-cpu-spin-fix | Remove hotstuff idle CPU spin (S370) | merged | | 25c0bb26a5661db2, a00020aa6b530f8d

## build/compiler/allocator
- jemalloc-allocator | jemalloc as global allocator (default feature) | merged | RAM plateau +0.95 GB accepted | 24c15c77e309ce25, 47cfbb64d3bd0f32
- jemalloc-decay-tuning | Tune jemalloc dirty/muzzy decay | rejected | -7/-31% | 38e871cba76285a0
- release-lto-fat-cgu1 | Fat LTO + codegen-units=1 release build A/B | proposed-not-done | gain refuted to +0.9..+2.0%, at CI | b5a9511e4a5b99f1, ee8d45e7c278cc6f, 1aa1aac3d47618db
- thin-lto-arm | Thin LTO arm with nm/objdump precheck, n>=6-8 | proposed-not-done | suggested if LTO ever run | b5a9511e4a5b99f1
- pgo-build | Profile-guided optimisation with system llvm-profdata 22 | proposed-not-done | reth reverted PGO from CI | 4a421c03782f12d0
- target-cpu-x86-64-v3 | -C target-cpu=x86-64-v3 for node build | proposed-not-done | sha3-asm stays scalar; little effect | 4a421c03782f12d0
- rocksdb-hw-crc32c-cxxflags | Build librocksdb-sys with -msse4.2 -mpclmul for hardware CRC32C | proposed-not-done | CPU-s/1M -0.8..-1.0%; ckpt write 0.605 -> ~0.4 s | 77748bc96d7acdf3, a77058fa713786bb, 4a421c03782f12d0
- opt-level-ab | opt-level A/B (owner list parked) | parked | | owner-list.md

## harness/bench
- load-windowed-stage-timers | Window per-view stage timers to load window only | merged | whole-run understated 2.4-3.5x | c0185e795bb6a1ff
- engine-sub-timers-attribution | Engine sub-timers, exec-write-stall attribution, qc-split timers | merged | | d2aea2352eee1b9f, 79024d61fd5486f8
- order-latency-histogram | End-to-end order latency histogram (admit to fill) | merged | s77 | 0bb3dd02c44c8d21, 6383e2fdca7362a4
- hot-market-share-knob | bench-throughput --hot-market-share for skewed-market cells | proposed-not-done | no skew cells exist | eae7902984529bca
- small-batch-cells | BATCH {10,50} x BLOCK_CAP {400,1600} zero-code cells | proposed-not-done | all 856 archived cells are batch 400 | f1fddcb5ef5c9eda
- bench-tail-depth-4 | Bench tail depth 4 (not on main) | parked | 10m depth-1 tail pacing is harness artifact | 38e871cba76285a0
- offered-load-rate-270 | Lower bench offered load to RATE 270 actions/s | tried-inconclusive | +7% mean, expired 40% -> <18%; overlap | 4411c2cff86f17ce
- resting-depth-sweep | Sweep resting depth 0/250k/500k/1M, profile save_books at 1M | proposed-not-done | 8f: -14.3% matched at 1M resting | ecdc9854e4d5c7a8
- by-id-heavy-cell | Bench cell heavy on by-id actions | rejected | measurement only, 0 gain | e708738839560b1d
- cloid-share-sizing | Size cloid cost at CLOID_SHARE 25/50/100 before sharding | proposed-not-done | c0 default shows 0 | e91d60ba98508151

## rpc/streams
- rpc-block-body-raw-json | getBlockBody serialises actions via RawValue instead of serde_json Value tree (TORUS_RPC_BODY_RAW_JSON) | proposed-not-done | fleet CPU-s/1M -1.5..-2.5%; matched 0 | de0458c45556b416, bfbd7612197a53cd
- newtrades-rawvalue | RawValue for newTrades stream payloads (s80 fix 2) | merged | | de0458c45556b416

## other
- hl-style-in-memory-state | Whole state in memory, log + snapshots, one record per user (item 6 gap vs Hyperliquid) | in-progress | phases 1-4 merged; Phase 5 parked | 381a76a66d6211f1, e8d5a7678385766b
- evm-option0-fast-path | Item 7 option 0 EVM fast path (owner list) | merged | 9a: EVM load free (1.011) | 71721f200bc970b3
