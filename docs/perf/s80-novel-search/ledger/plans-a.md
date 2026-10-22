# Ledger part plans-a

Sources: docs/plans/a*..h*.md (44 files), docs/design-*.md (4), docs/l3-*.md (5) in Torus-hyperBFT @ main eb993e3f.
Statuses were cross-checked against git history on main where the doc was stale (commit ids in the source column).
Format: slug | mechanism | status | evidence | source

## consensus

- header-first-proposals | Leader gossips a ~200 B ProposalHeader at every propose site and serves the body via direct BlockDataRequest/Response; legacy full Proposal still accepted | merged | all 5 propose sites; hybrid impl tasks 1,2,4-7,9 done | docs/plans/hybrid-pipelining.md (A), docs/plans/consensus-fast-path-impl.md
- direct-full-proposal-to-each-validator | Proposer sends the full Proposal (4 MB direct cap) to every validator instead of gossip, plus immediate view advance | rejected | O(N) proposer bandwidth, dead-ends near 10 validators | docs/plans/hybrid-pipelining.md (Option B)
- speculative-next-block-production | Next leader builds its block speculatively on the current proposal before the QC forms, discarded on fork | proposed-not-done | est 10-20% latency for ~2x complexity; deferred | docs/plans/hybrid-pipelining.md (Option C)
- early-view-advance-on-qc | Pacemaker enters the next view the instant a QC forms in the vote collector instead of waiting for the deadline | merged | hybrid impl Task 7 | docs/plans/hybrid-pipelining-impl.md
- da-guard-commit-after-body | Delay commit of a header-voted block until its body is confirmed held | merged | hybrid impl Task 5b | docs/plans/hybrid-pipelining-impl.md
- vote-before-body | Replicas phase-vote on the header before holding the body | rejected | s83 wedge: QC on a block only its proposer held | commit 973cc747; docs/l3-work-budget.md §2
- vote-only-when-body-held | Replica votes only once it holds the body (in tree, own, or pushed copy passing check_block_data); every QC implies a quorum holds it | merged | fixes s83 wedge | commit 973cc747 (s84 option A)
- inline-full-proposal-small-blocks | Leader broadcasts full Proposal when body <= TORUS_INLINE_BODY_MAX_BYTES, skipping the body-fetch RTT | rejected | idle views halved; loaded cell 0 matched (view-drop, vote-before-DA lost) | docs/l3-persist-attribution.md §6, commit 0c3ea78
- body-push-after-header | Leader pushes the body as an unsolicited BlockDataResponse right after the header; follower inserts with zero fetch RTT | measured-no-gain | arrival 58.7->21.3 ms but qc_collect 72.5->138, view 113.6->115.6 | docs/l3-work-budget.md (bodypush-ab), docs/l3-persist-attribution.md fix 2
- leader-dispatch-timestamp-split | Stamp the leader's wire-dispatch time in the proposal so followers split true wire delay from leader build in proposal_arrival | proposed-not-done | attribution only; ~60% of 16.5 ms is leader build | docs/l3-arrival-attribution.md fix 1
- consensus-direct-fan | Consensus broadcasts go over the /torus/direct fan (1 hop) instead of the gossipsub per-peer queue behind bulk native batches | parked | B2 isolation code exists; s58 78.6 parked direct fan | docs/l3-arrival-attribution.md fix 3, commit fcc49b54, docs/plans/exp-archy-s58-bundle-dmt.md
- consensus-ratelimit-exemption | Exempt proposal headers from the consensus gossip rate limiter that dropped them | parked | s58 §77: headers lost at limiter caused timeouts | docs/plans/exp-archy-s58-bundle-dmt.md (78.6)
- consensus-ratelimit-drop-counter | Metric torus_consensus_rate_limited_dropped_total for messages dropped by the consensus rate limiter | merged | observability only | docs/plans/exp-archy-s58-bundle-dmt.md (0af69a76)
- select-leader-closed-form | Replace O(view mod total_power) select_leader walk with a closed-form O(n log n) IWRR, consensus-identical | merged | removed height drag; propose_delay ~240 ms at view 1.2M | docs/plans/blockspeed-orders-roadmap-s405.md
- vote-earlier-than-exec | Move the vote ahead of execution | merged | no-op: header path already votes at +1.6 ms pre-exec | docs/l3-work-budget.md §4 row 5
- deepen-consensus-lead-over-exec | Raise the 64-slot exec channel / weaken backpressure so consensus runs further ahead of exec | rejected | re-arms S470 wedge and OOM risk | docs/l3-work-budget.md §4
- commit-lag-backoff | Pacemaker lengthens view deadlines by qc-committed lag (GRACE 4, cap 8) so exec can drain without timeout cascades | merged | S470 wedge fix; must stay visible | docs/l3-work-budget.md §2
- proposer-exec-watermark-yield | Leader yields its slot when local exec backlog (pending headers + deferred bodies) exceeds TORUS_PROPOSER_EXEC_WATERMARK | merged | default off (0) | docs/l3-work-budget.md §2
- propose-defer-exec-watermark | Defer a leader's fresh proposal while its exec queue is at a watermark, capped at half a view, skipped after a TC | parked | campaign B dropped; revisit after item 6 Phase 5 | commits 00d1c839, 30a2760f, 3a8c363f
- equivocation-local-slash-off | Stop applying slashes from locally observed equivocations (node-local memory); keep detection, rollback, stored evidence | merged | determinism fix, no format change | docs/plans/consensus-bug-a-equivocation.md
- onchain-equivocation-evidence | NativeAction SubmitEquivocationEvidence verified against leader-signed proposals or conflicting signed votes, slash in the flush batch | proposed-not-done | needs hotstuff wire change; owner decision D-a1..a3 | docs/plans/consensus-bug-a-equivocation.md (option 2)
- quorum-vote-slash | Validators vote "saw equivocation by X" as an action; slash at >2/3 stake | rejected | trusts observation, weaker than cryptographic proof | docs/plans/consensus-bug-a-equivocation.md (option 3)
- epoch-plan-by-exec | Execution of boundary H writes the validator-set plan for H+L; consensus at H+L only reads it; no consensus-thread DB writes | merged | removes divergent validator sets under exec lag | docs/plans/consensus-bug-b-epoch-race.md
- epoch-snapshot-h-minus-d | Compute the epoch plan D blocks before the boundary instead of one epoch | rejected | stalls boundary whenever exec lag > D | docs/plans/consensus-bug-b-epoch-race.md (option 2)
- validator-set-in-block | Proposer puts the next validator set in the block; validators verify | rejected | wire change, no gain | docs/plans/consensus-bug-b-epoch-race.md (option 3)
- block-until-exec-catches-up | Consensus waits until exec reaches H-1 before boundary H | rejected | deadlock under pipelined HotStuff | docs/plans/consensus-bug-b-epoch-race.md (option 4)
- validate-fastpath-on-consensus-thread | Constraint: keep validate_block's linear fast path on the consensus thread; only change waiting mechanisms | decided | memory 0efcef9de231cc5a, load-bearing | docs/plans/block-tree-pruner-and-blockspeed-quick-wins.md
- async-validate | TORUS_ASYNC_VALIDATE moves validate_block body work off the HotStuff thread to a worker; flag-off overhead reworked | tried-inconclusive | perf gate blocked: baseline cell invalid, zero valid pairs | docs/plans/async-validate-flag-off-2026-09-17.md, branch perf/async-validate-rework
- leader-propose-writes-one-batch | Batch the leader's propose-path writes into one atomic WriteBatch | tried-inconclusive | only in matched-200k composite; not on main | commit fd1fbbad, docs/plans/async-validate-flag-off-2026-09-17.md
- fourth-validator-quorum-slack | Run 4 validators so quorum no longer needs all 3 and qc_collect stops tracking the slowest node | decided | owner decision (3 or 4 validators) | docs/plans/blockspeed-orders-roadmap-s405.md
- dead-leader-exclusion-b1 | Execution counts missed leader slots from committed justify views; epoch plan excludes leaders with miss_run >= 3 from IWRR, re-admit after L*2^j | in-progress | derived -87..-95% blk/s with one leader dead | docs/plans/dead-leader-skip.md (owner-list item)
- dead-leader-b2-justify-chain | Derive the exclusion set per block inside hotstuff_rs from the justify chain (no epoch wait) | parked | heavier alternative if transient matters | docs/plans/dead-leader-skip.md
- dead-leader-b0-reputation | Switch on B3 reputation leader weighting | rejected | diverges on local inputs; 25% floor keeps dead node | docs/plans/dead-leader-skip.md
- dead-leader-c1-term-per-k-views | One leader per k views | rejected | no gain unless terms end on agreed TCs | docs/plans/dead-leader-skip.md
- dead-leader-c2-skip-on-newviews | Skip a leader on >f "no proposal" NewViews | rejected | ~a2 plus one message round | docs/plans/dead-leader-skip.md
- iwrr-power-normalization | Normalize validator powers (gcd or smooth WRR) before genesis to fix IWRR coarseness | proposed-not-done | stake-1 node leads 1/3 instead of 1/5 | docs/plans/dead-leader-skip.md Q4
- seeded-rotation-per-epoch | Per-epoch seeded leader rotation order to stop fixed-neighbour framing | proposed-not-done | relevant at n >= 7 | docs/plans/dead-leader-skip.md Q7
- downtime-jailing | Wire DowntimeTracker / jail votes to remove voting power of dead validators | proposed-not-done | separate jailing design before mainnet | docs/plans/dead-leader-skip.md 7a

## pacemaker/timeouts

- base-timeout-500ms | Lower timeout_base_ms 1200 -> 500 to shorten dead-leader views | rejected | s68 A/B -6.5% matched/s, 7x timeouts; WAN risk | docs/plans/dead-leader-skip.md (a1)
- timeout-base-500-vs-1500 | Raise timeout_base_ms to remove dead views at cap 100 | measured-no-gain | surviving views absorb the time, net inside noise | commit 7bc3ff90
- short-timeout-missed-leader | Give a leader whose last slot missed ~250 ms instead of 1.2-2.4 s, reset after one on-time proposal (local timers) | parked | derived transient 1.5 -> 6.9 blk/s | docs/plans/dead-leader-skip.md (a2)
- header-resend-250ms | Stuck leader re-sends its header directly every TORUS_HEADER_RESEND_MS (3000 -> 250) | default-on | 0 lost-proposal timeouts in every s58 arm | docs/plans/exp-archy-s58-bundle-dmt.md, commit d28f6166
- epoch-boundary-rejoin-boot | A restarted replica boots back INTO an uncertified epoch-change view it entered instead of view+1 | merged | drill wedge ~19 min -> heals via TC | docs/plans/epoch-boundary-rejoin-fix.md
- past-view-timeoutvote-reply | Replica already past E answers TimeoutVote(E) | rejected | tip above view dropped; needs past-view collector exceptions | docs/plans/epoch-boundary-rejoin-fix.md

## dissemination/DA

- bs4a-da-recovery-worker | On a body miss the hot path votes MissingData after one <=20 ms wake slice; a DaRecoveryWorker thread pulls bodies off-thread (~1 s event-driven) | merged | consensus-thread stall ~260 ms -> <=40 ms | docs/plans/bs4a-da-reconstruct-offthread*.md, commit e05b65c7
- bs4a-fail-fast-passive | Fail MissingData immediately with no owned recovery | rejected | lossy WAN could raise MissingData churn | docs/plans/bs4a-da-reconstruct-offthread.md (A)
- bs4a-prewarm-at-gossip | Trigger by-hash body prefetch at CompactBlock gossip arrival, keep 260 ms inline budget | rejected | does not remove the stall | docs/plans/bs4a-da-reconstruct-offthread.md (C)
- reconstruct-wake-on-arrival | NativeDaStore arrival condvar replaces fixed 20 ms sleep ticks in reconstruct and hot pull loops | merged | push-race wake 0-5 ms vs 20 ms tick | docs/plans/block-tree-pruner-and-blockspeed-quick-wins.md 2b
- body-retry-faster-rotation | BODY_RETRY_INTERVAL 300 -> 100 ms, proposer-only retries 3 -> 2 before rotating | merged | first alternate target 900 -> 200 ms | docs/plans/block-tree-pruner-and-blockspeed-quick-wins.md 2c
- gossipsub-heartbeat-wired | Wire config gossipsub_heartbeat_ms (behaviour hardcoded 500 ms) at default 100 ms | merged | config drift fix | docs/plans/block-tree-pruner-and-blockspeed-quick-wins.md 2a, commit b6dd03ec
- o5-feed-gate-ladder | Single caps source: consensus accept 256 KB -> 1 MB, direct read 4 -> 8 MB, env threshold clamped, gossip transmit wired | merged | bs400 512 KB manifest+pull 11.4k o/s vs 3 MB push 0.5-4.5k | docs/plans/blockspeed-orders-roadmap-s405.md (O5)
- push-manifest-threshold-wan | WAN A/B of 512 KB manifest threshold vs clamped 4 MB full push | queued | owner-list WAN item | docs/plans/blockspeed-orders-roadmap-s405.md
- full-blocks-not-compact | Proposals carry the full TorusBlock instead of CompactBlock hashes to end reconstruction stalls | rejected | superseded: compact proposals re-enabled to fix bs=500 collapse | docs/plans/direct-to-leader-impl.md step 1, commits 66da8121, 67461710
- dup-inclusion-hash-ledger | Note compact datum action hashes at validate (before reconstruction, so MissingData counts), union per height, exclude at produce | merged | dup factor was 1.08-1.27 (8-27% slots wasted) | docs/plans/dup-inclusion-tail-fix*.md, commit 7702da07
- dup-ancestor-walk-exclusion | At propose, walk uncommitted ancestors and exclude their datum hashes | proposed-not-done | follow-up only if factor > 1.05 | docs/plans/dup-inclusion-tail-fix.md (B)
- destructive-selection-reoffer | Remove actions from the pool at selection, re-add if the proposal dies | rejected | fork-sensitive abort detection, liveness loss risk | docs/plans/dup-inclusion-tail-fix.md (C)
- dup-copy-verify-prefilter | Sender-independent hash pre-filter so duplicate action copies skip exec verify | proposed-not-done | <=27% of verify at 1.27x dup | docs/plans/dup-inclusion-tail-fix.md
- shard-custody | Erasure shard custody validation before vote | measured-no-gain | campA -21.6% matched/s, 140 ms per follower validate; default off | docs/perf §82 (status), owner list T3.1

## ingress/mempool

- direct-to-leader-forwarding | Non-leader RPC forwards native actions unicast to the current leader over /torus/direct (batched envelope); gossip fallback | merged | S458 +18% committed o/s, zero drops | docs/plans/direct-to-leader-impl.md, commits 56ddd74f, 3fd35e90
- native-gossip-flood-off | Disable native-action gossip flooding (byte-bounded publish queue overflowed ~44% under flood) | merged | testnet 44% gossip drops at 140-150k o/s | docs/plans/direct-to-leader-impl.md, commit 66ddef44
- torus-getleader-rpc | RPC returning current leader and peer info for clients that connect directly | merged | optional task 7 | docs/plans/direct-to-leader-impl.md
- placeorderbatch | One ecrecover and one manifest entry per batch of orders, exec-side batch cap | merged | bs1 293 -> bs100 11.2k orders/s (~38x) | docs/plans/blockspeed-orders-roadmap-s405.md (O2)
- order-aware-selection-budget | Block selection counts orders (NATIVE_ORDERS_PER_BLOCK_CAP), not actions | merged | O2 G2 | docs/plans/blockspeed-orders-roadmap-s405.md
- native-per-block-cap-2000 | Raise NATIVE_PER_BLOCK_CAP 16 -> 2000 once proposals are header-only | merged | gossip 256 KB cap no longer binds | docs/plans/consensus-fast-path-impl.md Task 4
- exec-cap-channel-palliative | Lower total per-block action cap 1000 -> 256 and/or deepen exec channel to bound block weight | rejected | exec queue depth 0 during load at s351 | docs/plans/exec-ceiling.md (D), exec-ceiling-verdict.md
- ingress-verify-pool | Parallelize within-batch RPC signature verify (rayon) on a dedicated ingress verify pool | merged | s351: verify 1.47 s/batch, 0.77 s pool queueing | docs/plans/exec-ceiling-verdict.md, s123 doc (ingress_verify_pool)
- evm-gas-budget-cap | Replace EVM_TOTAL_BLOCK_CAP 20 / 4 per sender with a per-block EVM gas budget (5M, env) + 25% sender share (first-tx grace) | merged | shipped S392 | docs/plans/evm-blocker-set-decisions.md D1
- evm-gas-budget-15m-raise | Raise EVM block gas budget default 5M -> 15M after mixed-load bench | queued | acceptance bench after relaunch | docs/plans/evm-blocker-set-decisions.md D1
- evm-per-tx-decode-isolation | Decode EVM txs per tx so one undecodable tx skips only itself, not the block | merged | closes whole-block poisoning | docs/plans/evm-blocker-set-decisions.md D3
- evm-tx-direct-to-leader | eth_sendRawTransaction forwards raw RLP to the leader over /torus/direct (marker 0xFB); leader runs add_evm_tx | merged | EVM txs dead-ended on non-proposers before | docs/plans/evm-tx-direct-to-leader.md
- evm-forward-next-evm-leader | Forward EVM txs to the leader of the next EVM height (lanes every N=5 heights) | merged | item 7 step 1 | commit 853fed28 (a1dc4fa5)
- evm-forward-leader-change-retry | Re-forward EVM txs on leader rotation before inclusion | proposed-not-done | review follow-up 1 | docs/plans/evm-tx-direct-to-leader.md
- generic-forward-typed-payload | One generic forward-to-leader pipeline parameterized by marker + gate instead of duplicated native/EVM code | proposed-not-done | review follow-up 3 | docs/plans/evm-tx-direct-to-leader.md
- forwarded-ingest-rate-limit | Apply RPC tx_submit_limiter / bound channels on forwarded-tx ingest at the leader | proposed-not-done | parity with native; unbounded channels | docs/plans/evm-tx-direct-to-leader.md

## signature/crypto

- exec-trust-cache | Mempool FIFO verified_senders cache filled at ingress/gossip recover; exec reuses sender on hit, full recover + slash path on miss | default-on | exec verify 291-429 ms/blk at s351 | docs/plans/double-verify-trust-cache-impl.md, s123 doc (default_value_t=true)
- exec-parallel-keys | Trust-cache keys keccak'd in an order-preserving rayon pre-pass (TORUS_EXEC_PARALLEL_KEYS), lookups stay sequential | measured-no-gain | campA +1.03% [0.997,1.024], +2.1% CPU/fill | docs/plans/exp-s123-exec-action-keys-parallel.md, perf §82
- exec-hash-once | One canonical encoding per action for sort key and trust-cache key; skip recovery when all hit; cached EIP-712 domain separator | merged | exec on-CPU/1k fills 0.955, native blk/s 1.39x | commit 893130c9
- sort-key-keccak-parallel | Compute action_sort_key keccak in the same parallel pre-pass | proposed-not-done | follow-up listed | docs/plans/exp-s123-exec-action-keys-parallel.md
- carry-senders-from-dispatch | Carry recovered senders from dispatch into exec instead of re-looking up | proposed-not-done | listed not done | docs/plans/exp-s123-exec-action-keys-parallel.md
- batched-verified-sender-lookup | Replace per-action RwLock verified_sender lookup with a batched lookup | proposed-not-done | listed not done | docs/plans/exp-s123-exec-action-keys-parallel.md
- keccak-asm-backend | Enable alloy-primitives/asm-keccak workspace-wide (was silently pure-Rust sha3) | merged | commit 82523da | docs/design-levelhash-cache.md §1.3, docs/plans/async-native-trie-maintenance.md §9

## execution/matching

- exec-phase-instrumentation | Per-phase exec histograms (verify, replay guard, engine, save_books, flush) + exec queue depth gauge | merged | s351: engine 38.7%, verify 31.3%, save_books 0% | docs/plans/exec-ceiling*.md
- exec-3stage-pipeline | verify(N+1) || execute(N) || flush(N-1) on three threads | rejected | exec queue depth 0 at s351 (ingress-bound then) | docs/plans/exec-ceiling-verdict.md (C)
- per-market-parallel-match | Phase 3 matching runs one worker per market (book has no balance access) | merged | always on | docs/design-parallel-engine.md §1
- parallel-settle-pass-ab | Settlement pass A per-market plans on workers, pass B canonical serial apply; TORUS_PARALLEL_SETTLE default on, 1024-fill gate | default-on | plan 9.13 | docs/design-parallel-engine.md §1
- parallel-engine-phase2-sharding | TORUS_PARALLEL_ENGINE=N: Phase-2 margin prepare sharded by sender + serial stitch; also lowers settle gate to 64 fills | tried-inconclusive | ubench serial wins cap-400; campA +1.03% at +2% CPU | docs/design-parallel-engine.md §7, perf §82 (PE)
- parallel-engine-adaptive-threshold | Auto-engage engine parallelism above a work threshold | rejected | no shape wins enough | docs/design-parallel-engine.md §7
- parallel-match-setup | Build each market's Phase 3 inputs (reduce-only positions, AccountMargins, MatchRequests) inside its pool match job | default-on | ubench engine ms/1k -5.5%; DMT bundle +8-9% | docs/plans/exp-archy-s58-parallel-match-setup.md
- settle-defer-results-scatter | Pass B records a 1-byte reject per order in an L2-resident buffer, then writes the 5 MB results array once front-to-back | default-on | 3.6-4% exec samples removed; bundle +8-9% | docs/plans/exp-archy-s58-settle-passb-deferred-results-scatter.md
- fast-sell-top-ups-stitch-list | Record top-up candidates at stitch time with a per-sender run cache instead of rescanning all PreparedOrders; batched s92 counters | default-on | sell_top_ups ~5.5% exec (~10.8 ms/blk) | docs/plans/exp-archy-s58-sell-top-up-stitch-list.md
- parallel-classic-book-save | Classic book blob save parallelized across books (s58 arm B) | parked | +3.5% CPU for +0.09% matched/s | docs/plans/exp-archy-s58-bundle-dmt.md (78.6)
- exec-pool-fine-grain-jobs | Smaller exec-pool jobs / grain chunk cap (s58 arm G) | parked | ALL-G +8.3% at +4.7% CPU vs +1.1% without G | docs/plans/exp-archy-s58-bundle-dmt.md (78.6)
- parallel-cancel-all | Batched CancelAll book pass on the exec pool, one job group per market, merged in market order (TORUS_PARALLEL_CANCEL_ALL=8) | default-on | campA +4.51% matched/s, exec block_ms -45%, blk/s +75% | docs/plans/exp-s123-parallel-cancel-all-book-pass.md, commit d28f6166
- parallel-cancel-all-nonresident | Parallelize the non-resident every-book cancel-all path and single-action cancel-all | proposed-not-done | left out of s123 | docs/plans/exp-s123-parallel-cancel-all-book-pass.md
- cancel-all-per-level-batch-removal | Cancel-all removes orders per level in a batch; book-save pass 2 off the exec thread | tried-inconclusive | matched-200k composite only; not on main | commit 42987284, docs/plans/async-validate-flag-off-2026-09-17.md
- epoch-fast-path | No-EVM, no-slash epoch boundary blocks take the pipelined exec path with a forced boundary checkpoint (TORUS_EPOCH_FAST_PATH) | default-on | campA +1.50% matched/s, exec block_ms -7% | docs/plans/exp-archy-s58-bundle-dmt.md, commit d28f6166
- queue-position-binary-search | Locate an order in its level by binary search on seq instead of a front-to-back id scan (rows, cancel, modify) | merged | 10.66 -> 0.86 us/row on a 7000-deep level | docs/design-levelhash-cache.md §7.1
- o1-margin-cache | Per-block write-through sender balance/position cache for Phase-2 margin | proposed-not-done | ~13 us/order top exec cost; later covered by item 6 summary | docs/plans/blockspeed-orders-roadmap-s405.md (O1)
- resting-order-ttl-gc | TTL/GC of stale far-from-touch resting orders to shrink touched-level depth | proposed-not-done | 195k resting drove save_books 107 ms | docs/l3-savebooks-attribution.md §4.4

## settlement/margin/liquidation

- account-margin-f1-hl-computed | Account-level cross margin computed per sender per batch (equity, position-tier IM, UPnL at mark), sole placement gate | merged | 40x leverage repro closed | docs/plans/account-level-margin-f1*.md (option B)
- account-margin-stored-lock | Store position margin in NativeBalance at fill (schema v2) | rejected | not HL; ignores UPnL; overlap remains | docs/plans/account-level-margin-f1.md (A)
- account-margin-hybrid | Stored lock for settlement + computed check at placement | rejected | two sources of truth drift | docs/plans/account-level-margin-f1.md (C)
- d2-exclusive-pool | Sender's free margin is exclusive to the market of its first checked taker; other markets taker-only | merged | ~19% rejected_cancelled in ubench_econ | docs/plans/account-level-margin-f1-impl.md D2
- maker-margin-cancel-at-fill | Maker fills checked against reservation share + start-of-batch free snapshot; breaching maker cancelled (HL marginCanceled) | merged | F1 D8 | docs/plans/account-level-margin-f1-impl.md
- sender-view-cache-per-block | Cache each sender's account view for the block (valid until Phase-4 flush) | proposed-not-done | R10: "not planned now" | docs/plans/account-level-margin-f1-impl.md R10
- batch-maker-account-memo | Memoize each maker's start-of-batch free margin once per execute_batch across market workers (OnceLock) + per-batch lazy mark table | merged | ubench 95 -> 41 ms/1k fills | docs/plans/crab-perf-fixes-s87.md (Fix 1, C3)
- maker-memo-from-phase2-posnets | Seed the maker memo from Phase-2 SenderFold pos_nets instead of recomputing views | proposed-not-done | 1c, only if Fix 1 missed target | docs/plans/crab-perf-fixes-s87.md
- sell-reserve-start-best-bid | Non-pool batch sells reserve at max(limit or cap, start-of-Phase-2 best bid); taker-only budget gets +1 raw allowance | merged | rejected_cancelled 17.5k -> 5.6k; ms/1k 37-41 -> 29-30 | docs/plans/crab-perf-fixes-s87.md (C5)
- same-batch-bid-top-up | After Phase 2, top up non-pool sells from leftover free margin to cover the highest earlier same-batch resting bid | merged | rejected_cancelled 5.4-5.9k -> 1.6-1.8k; fills/blk 10.9k | docs/plans/crab-perf-fixes-s87.md (C6)
- same-batch-bid-rest-only | Count a same-batch bid only if it will rest against start asks + earlier resting asks (closes PostOnly/1-lot drains) | merged | rejected_cancelled back to 4.2-4.8k (correctness) | docs/plans/crab-perf-fixes-s87.md (C7)
- level-depth-prefix-sums | Per-level resting depth prefix sums once per market per batch for the same-batch bid bound | merged | was 11.2 ms/1k fills, ~49% of crab exec CPU | docs/plans/crab-speed-target-design.md §2.1 (PF1), commit 73ae3bbb
- trader-margin-summary | In-memory per-trader margin sums (available, order margin, per-position terms) updated O(1) per fill; marks re-valued once per touched trader per block | merged | item 6 Phase 4 UserState; light 726-819 B/user | docs/plans/crab-speed-target-design.md §2.2, commit fd82758f
- slim-summary-light-users | Shrink the margin summary memory for light users | queued | Phase 5 step 5 (RSS overshoot accepted) | commit 79f8afbd
- aggregated-upnl-formula | UPnL = sum(size*mark) - K with K = sum(size*entry) instead of per-position terms | rejected | not bit-exact (FixedPoint truncation); consensus change | docs/plans/crab-speed-target-design.md §2.2
- sequential-second-pass-d | Re-run margin-cut non-pool takers serially against account-level free margin after the parallel pass | proposed-not-done | after item 6 Phase 4/5; golden re-pin | docs/plans/crab-speed-target-design.md §2.4
- liq-skip-valuation-no-marks | When no listed market has a usable mark, skip liq_view reads for every scanned trader; same cursor/pending writes | merged | tail ~50 -> 1.2 ms/1k fills | docs/plans/crab-perf-fixes-s87.md (Fix 2a)
- liq-l1-memory-walk | Same 2048/64 round-robin window, traders_after and liq_view served from in-memory records/summary | merged | owner s88 Q1; item 6 Phase 1 | docs/plans/crab-speed-target-design.md §2.3
- liq-l2-health-certificates | Node-local certificate (AV0, G, W, k) skips provably-healthy candidates while relative mark move < rho* | proposed-not-done | only if oracle-on tail > 1.0 ms/1k | docs/plans/crab-speed-target-design.md §2.3, crab-perf-fixes-s87.md (2b)
- liq-l3-every-account-every-block | Rule SCAN = infinity with a node-local trigger-price index; acts on up to 64 unhealthy in address order | parked | owner: bundle with D12 fresh genesis | docs/plans/crab-speed-target-design.md §6.1
- liq-certificates-in-state | Store health certificates as hashed CF_NATIVE_LIQUIDATION rows | rejected | write per checked account/block; reverses C1 | docs/plans/crab-perf-fixes-s87.md, crab-speed-target-design.md
- adl-c1-trader-set-candidates | ADL candidates = every opposite holder via point reads over the sorted trader set (drop 65,536-row window from empty key) | merged | adl_candidates was 97.7% of run_liquidations | docs/plans/adl-budget.md §3
- adl-c2-holder-index | Node-local per-market holder list in TraderPositions updated from ResidentDelta, merged with block-dirty keys | merged | 10% holders ~7x less time per unit | docs/plans/adl-budget.md §10-11
- adl-c3-consensus-index | Consensus index rows market||trader written on every open/close | rejected | extra hashed write on order hot path | docs/plans/adl-budget.md §3, §7
- adl-rank-once-per-block-side | Rank (market, side) once per block from a cache with a per-key cursor | merged | ~270 rankings/block instead of ~12,600 | docs/plans/adl-budget.md §3 Q2
- adl-work-budget-queue | Per-block ADL work budget W (units = rows + holders + reads) with FIFO obligation rows carried over | merged | S=750 stalled 11.6 min before | docs/plans/adl-budget.md §4, §8
- adl-escrow-p2 | ADL'd positions move at B to two escrow accounts at clamped prices; escrows drain against ranked holders | merged | account flat at B, no freeze | docs/plans/adl-budget.md §8
- adl-freeze-queued-account | Reject all actions of a queued ADL account | rejected | dropped by P2 | docs/plans/adl-budget.md §4 Q5
- adl-vault-takes-all | Move bankrupt positions to the vault at mark then ADL the vault | rejected | larger semantic change from HL | docs/plans/adl-budget.md §4 Q6
- adl-drain-caches | One trader set per drain, ranking AV memoized per trader | merged | S=750 B1 12.3 s -> 0.72 s | docs/plans/adl-budget.md §9
- adl-running-rest | Running account rest O(P) per ADL'd account instead of re-reading all positions per market O(P^2) | merged | ~0.7 s/block at S=750 | docs/plans/adl-budget-impl.md
- adl-holder-units-b-in-w | Ranking charges holders H(m) not trader set; block-B escrow transfers charged into W (T=6); W=100,000 | merged | S=750 B1 290 -> 66 ms | docs/plans/adl-budget.md §12-13
- adl-has-key-prefix-bounded | Bound has_key overlay seek to the trader's prefix instead of seeking past emptied traders | merged | removed ~96 ms quadratic in B1 | docs/plans/adl-budget.md §12.4
- adl-drain-dirty-set | Drain-local HashSet of dirty traders replaces per-read layer_touches RwLock + 2 B-tree range searches | merged | block B 149.6 -> 128.8 ms (-13.9%) | docs/plans/adl-dirty-check.md
- adl-overlay-prefix-index | Overlay-wide per-CF dirty prefix count map maintained on every write | rejected | cost on every write; YAGNI | docs/plans/adl-dirty-check.md (B)
- adl-per-key-dirty-check | Use R's record when t||m is clean even if trader dirty elsewhere | rejected | not needed | docs/plans/adl-dirty-check.md (C)
- adl-holder-record-slice | Hand the holder list's record slice to the ranking to skip resident_positions binary search | proposed-not-done | binary search 24.7% of case 1 | docs/plans/adl-dirty-check.md
- adl-lower-act-limit | Lower per-block act limit for ADL accounts | rejected | owner kept 64 for every class | docs/plans/adl-budget.md §12
- adl-queue-running-count | ADL queue gauge as a running count instead of rescanning the queue per metrics block | merged | fix (b) | docs/plans/adl-budget.md §10
- adl-edge-pairing-index | Index opposite rows for escrow-vs-escrow pairing instead of O(queue) scan | proposed-not-done | accepted as rare | docs/plans/adl-budget-impl.md OQ4
- oi-symmetry-genesis-check | OI-symmetry check per market at genesis/upgrade so a historical imbalance cannot halt on first ADL | proposed-not-done | before mainnet | docs/plans/adl-budget.md §10
- exact-cost-basis | Positions store exact cost basis; each fill rounds once, shared by both sides | merged | value sum exact, escrow dust 0 when rows close whole | docs/plans/adl-budget-impl.md (s100)

## state/storage/rocksdb

- overlay-prefix-range-merge | iterate_cf merges parent/pending layers by BTreeMap range over the prefix (take_while) + DB-only fast path | merged | ubench ~135-169 -> 105 ms/1k fills | docs/plans/crab-perf-fixes-s87.md (Fix 3, C1)
- block-tree-pruner | Bounded prune-on-commit of BLOCKS/BLOCK_AT_HEIGHT/children (64 heights/commit, retention >= 1000) in a separate batch | merged | cf_consensus_meta ~50% of DB; +45% block time/50k blocks | docs/plans/block-tree-pruner-and-blockspeed-quick-wins.md, commit f99d07a5
- block-tree-pruner-bg-thread | Background pruner thread over RocksKVStore | rejected | violates hotstuff single-writer; races sync server | docs/plans/block-tree-pruner-and-blockspeed-quick-wins.md (B)
- consensus-meta-compact-range | compact_range cf_consensus_meta after the prune drain | proposed-not-done | open question | docs/plans/block-tree-pruner-and-blockspeed-quick-wins.md
- consensus-meta-8mib-memtable | 8 MiB write buffer for cf_consensus_meta to cut InlineSkipList read cost | merged | commit 7e67bd1 | docs/plans/blockspeed-orders-roadmap-s405.md
- memtable-sawtooth-headers-bodies | Smaller write buffers for 128 MiB headers/bodies CFs to remove +-tens-of-ms memtable-fill oscillation | parked | optional, only if live legs show it | docs/plans/blockspeed-orders-roadmap-s405.md
- append-cf-codec-knob | TORUS_ROCKSDB_APPEND_CF_CODEC zstd1/lz4 for trades + DA CFs (skip LZ4->ZSTD recompression) | measured-no-gain | LZ4 -3.5% CPU but +48% SST (owner rule); zstd1 no change | docs/plans/append-cf-compaction.md, commit 233c026a
- wal-budget-2048 | TORUS_ROCKSDB_MAX_TOTAL_WAL_MB 512 -> 2048 so big CFs flush in fewer, larger rounds | default-on | -5.7% CPU/1M; flushes 557 -> 236 | docs/plans/append-cf-compaction.md (B), commit 67bbf6aa
- trade-history-off-validators | Validators skip trade history CFs (on for --rpc-only) | default-on | -11.9% CPU/1M, matched/s +8.2% | commits 73503705, 67bbf6aa
- wal-compression-zstd | TORUS_ROCKSDB_WAL_COMPRESSION zstd on the WAL (75% of bytes written) | parked | bytes written -74..-83%, CPU +1.3..+2.4%; merged off | docs/plans/append-cf-compaction.md (D), commit fd9e5dfa
- universal-compaction-append-cfs | Universal compaction for trades/DA CFs | parked | reserve; migration risk | docs/plans/append-cf-compaction.md (C)
- fifo-compaction-append-cfs | FIFO compaction for trades/DA | rejected | deletes trades/DA | docs/plans/append-cf-compaction.md
- zstd-dictionary-training | ZSTD dictionaries for append CFs | rejected | small values, not measured to help | docs/plans/append-cf-compaction.md
- append-cf-memtable-knob | Larger per-CF memtable for append-only CFs | proposed-not-done | only if WAL arm showed flush rounds matter | docs/plans/append-cf-compaction.md
- compaction-jobs-knobs | max_background_jobs 4 -> 8, max_subcompactions, smaller churny write buffers (env-gated) | merged | flush swings 39->119 ms/blk from compaction contention | docs/l3-diagnosis.md fix 3, commit 06de7326
- bytes-per-sync-knob | A/B bytes_per_sync 1 MiB -> 4 MiB or 0 | merged | env knob; perf-only | docs/l3-diagnosis.md fix 4, commit 06de7326
- hs-da-own-db | HotStuff KV store and DA store in their own RocksDB (own WAL, write queue) so votes/DA don't queue behind checkpoint batches | merged | in-window DB write wait -91%, p99 1,575 -> 1,370 ms | docs/plans/hs-da-own-db-impl.md, commit a44000e0
- checkpoint-slicing | Slice the ~200 MB state checkpoint WriteBatch (71.8 option B / Q6 A1) | parked | behind T5.3 p99 attribution | docs/plans/hs-da-own-db-impl.md, owner list
- sync-wal-on-commit | One flush_wal(true) at the commit boundary covers frontier + header + body (TORUS_SYNC_WAL_ON_COMMIT) | merged | default off; 2 fsyncs/commit with own consensus DB | docs/plans/commit-fsync-durability*.md, commits b1dbb971, bbc6a118
- sync-every-consensus-write | WriteOptions sync on every consensus-meta and body write | rejected | fsyncs hot path | docs/plans/commit-fsync-durability-design.md (A)
- targeted-write-sync | write_sync KV method only for frontier + body manifest | rejected | ordering hazard between two synced writes | docs/plans/commit-fsync-durability-design.md (B)
- vote-state-dsync-before-vote | One grouped sync batch of vote-safety state before sending the vote | proposed-not-done | ~7 ms dsync per view, consensus-visible contract | docs/l3-persist-attribution.md §5
- disable-wal-state-batch | Env-gated disable_wal for the per-block state batch with atomic_flush | merged | env-gated | commit 79ea483d
- commit-persist-one-batch | Commit-time persist writes header + bin body record in one WriteBatch; exec skips the redundant rewrite | merged | removes exec-time body rewrite | commits 1adb76eb, 0f26d1bf, docs/design-flush-pipeline.md
- async-post-flush-body | TORUS_ASYNC_POST_FLUSH moves exec-time body_persist to a background writer (body already durable at dispatch) | measured-no-gain | body_persist 3.3 -> 0.8 ms, absorbed by work conservation | docs/design-flush-pipeline.md, commit a4a924b1
- async-state-write-read-through | Defer state_write with read-through for N+1 point reads (two-stage MVCC) | proposed-not-done | state_write ~20-24 ms largest flush term | docs/design-flush-pipeline.md §6, docs/l3-work-budget.md option 2
- async-evm-resync | Defer evm_resync of the incremental EVM mirror | rejected | read-your-writes hazard for EVM block N+1 | docs/design-flush-pipeline.md §2
- exec-flush-overlap-pipeline | Overlap flush(N) with engine(N+1) via overlay layers and a flush worker | merged | exec pipeline + checkpoint layers on main | docs/l3-work-budget.md option 2, exec_pipeline.rs FlushWorker
- nonblocking-exec-dispatch | TORUS_EXEC_NONBLOCKING_DISPATCH with bounded deferred_exec bytes | merged | est ~0 mean, smooths variance | docs/l3-work-budget.md option 4, commit ee763a20
- native-applied-height-marker | Applied-height marker folded into the native flush batch; replay gap on restart | merged | crash safety | docs/plans/crash-safety-post-commit-impl.md, commit-then-execute-impl.md
- evm-bundle-one-batch | EVM bundle + writer-precompile rows built (not written) and prefixed to the block's single flush batch | merged | crash-replay non-idempotency fix | docs/plans/consensus-bug-c-evm-commit-atomicity.md
- evm-applied-marker | Separate META_EVM_APPLIED replay marker, two batches | rejected | loses EVM write set for running hash | docs/plans/consensus-bug-c-evm-commit-atomicity.md (2)
- commit-then-execute | Consensus agrees on ordering; native executes once post-commit on the exec thread (no proposer/validator double write) | merged | base of CTE pipeline | docs/plans/commit-then-execute-impl.md
- trade-history-bg-writer | Fill KVs buffered and written by BackgroundCfWriter off the exec thread | merged | 200-fill block 2.15 -> 1.30 ms (-39%) | docs/plans/blockspeed-orders-roadmap-s405.md (O3)
- ckpt-defer-books | Classic book blobs merged in the checkpoint layer's deferred part so exec doesn't wait for ~9 MB memcpy | measured-no-gain | campA +0.95% (noise); dropped s131 | docs/plans/exp-s123-defer-book-blobs-in-layer-merge.md, commit bfad0d62
- book-persist-dirty-only-snapshot | Persist only dirty markets / periodic snapshot / per-level keys for save_order_books | rejected | save_books 0.07 ms/blk at s351 | docs/plans/exec-ceiling.md (B), exec-ceiling-verdict.md

## checkpoint/hash

- incremental-native-trie | Bucketed Merkle native trie updated O(dirty buckets x depth) instead of flat keccak of all entries | merged | root 750 -> ~100 ms uncapped | docs/plans/blockspeed-orders-roadmap-s405.md (O6), async-native-trie-maintenance.md §2a
- native-trie-maintenance-off | Skip per-block CF_NATIVE_TRIE/HASHED maintenance with a stale sentinel; full-scan fallback + boot rebuild (no production reader) | default-on | removes 100% root_seconds (~9 ms cap-400) | docs/plans/async-native-trie-maintenance.md (Option 0), commits e4273ec2, db6c9de5
- async-native-trie-worker | Move apply_native_dirty to a worker thread with a bounded queue and its own trie-height marker | rejected | breaks batch atomicity; lagged root plausible-but-wrong | docs/plans/async-native-trie-maintenance.md (A)
- trie-one-block-lag | Apply block N's trie during N+1 | rejected | strictly dominated by A | docs/plans/async-native-trie-maintenance.md (B)
- durable-dirty-set-replay | Persist the dirty set in the state batch to repair a lagging trie | rejected | +250 KB/blk to state_write | docs/plans/async-native-trie-maintenance.md (B sub)
- trie-mirror-offthread-split | Bucket leaf hashing off-thread, tree fold inline | rejected | fold waits on leaves; no pipelining | docs/plans/async-native-trie-maintenance.md (C)
- parallel-bucket-hash | Per-dirty-bucket leaf hashing on N threads (TORUS_PARALLEL_BUCKET_HASH, default 1 -> 8) | default-on | cap-400 p1 4.22 vs p4 6.47 ms; mode-3 r5 +10.8%; moot with trie off | docs/l3-diagnosis.md fix 1, commit e44a38ca
- bucket-hash-min-buckets | Engage parallel bucket hashing only above TORUS_BUCKET_HASH_MIN_BUCKETS dirty buckets | merged | default 1 = exact-today | docs/design-flush-pipeline.md §1, commit a4a924b1
- member-cache-raise-prewarm | Raise TORUS_BUCKET_MEMBER_CACHE_MB / prewarm hot buckets to cut cold mirror prefix scans | proposed-not-done | prod root 26 ms vs warm bench 6.5 ms | docs/l3-diagnosis.md fix 2
- hash-only-native-mirror | CF_NATIVE_HASHED stores keccak(value); bucket leaves frame value hashes | merged | ~4x smaller member sets | docs/design-level-rows-authority.md §3.2
- level-rows-authority-mode2 | Per-price-level rows (qty, count, level_hash) are root authority; order rows move to node-local CF | decided | dirty buckets est ~9x fewer; owner chose Classic | docs/design-level-rows-authority.md
- book-mode3-chunked-digest | Level hash = fold of 64-seq chunk digests so a touch rehashes only dirty chunks (TORUS_BOOK_ROWS=3) | decided | 8m: save_books 65.7 -> 4.9 ms but CPU/fill +18%; owner chose Classic | docs/design-levelhash-cache.md §7, commit bfad0d62
- book-mode1-order-rows | Every order a root-CF row (TORUS_BOOK_ROWS=1) | rejected | dropped s131; P0 worse than Classic | commit bfad0d62
- level-hash-sponge-cache | Keep un-finalized keccak state per level; on append-only intervals absorb only the new tail frames (byte-identical) | default-on | in-vivo save_books 83 -> 37 ms; up to 9x at depth | docs/design-levelhash-cache.md
- level-hash-staged-seeding | Miss writes a cheap Probe; sponge seeded only after a level survives one append-only interval | merged | removed +15-30% all-churn regression | docs/design-levelhash-cache.md §1.3
- level-hash-merkle-over-orders | Per-level Merkle root of per-order leaves keyed by seq, O(log depth) updates | proposed-not-done | chunked preferred | docs/l3-savebooks-attribution.md §4.2
- savebooks-ram-shadow-reads | RAM shadow for the stops prefix scan + meta point read per dirty market | rejected | ~2.5 ms, not worth byte-identity risk | docs/l3-savebooks-attribution.md §5

## serialization/wire

- compact-proposals-precomputed-hashes | Carry precomputed action hashes through selection, DA mirror, in-flight window and compact datum | merged | zero extra hashing for dup ledger | commit 0e5dd6e3, docs/plans/dup-inclusion-tail-fix-impl.md
- gossip-path-zstd | zstd compression on the native-action gossip path | rejected | removed s364 (DoS); body path /2.0 zstd stays on | docs/plans/direct-to-leader-impl.md
- evm-forward-raw-rlp | Forwarded EVM payload is raw RLP only; leader recovers sender | merged | no sender prefix | docs/plans/evm-tx-direct-to-leader.md
- one-byte-deferred-result-record | Option<Option<FailureReason>> as a 1-byte record for deferred settle results | merged | part of D | docs/plans/exp-archy-s58-settle-passb-deferred-results-scatter.md

## threading/runtime/scheduling

- poller-condvar-wake | Replace the 250 us inbound poller park with a condvar notified by enqueue_inbound | proposed-not-done | ~125 us avg; irrelevant on WAN | docs/l3-arrival-attribution.md fix 2
- dedicated-rpc-runtime | RPC served on its own tokio runtime | merged | commit 66ddef44 | docs/plans/direct-to-leader-impl.md (context)
- per-core-utilization-capture | Capture per-core utilisation (mpstat) in bench cells to test deschedule-inflation hypotheses | proposed-not-done | engine 16 -> 43 ms in vivo (~2.7x deschedule) | docs/plans/async-native-trie-maintenance.md §6
- flush-worker-failed-race | Store exec_failed before shared.failed in the flush worker so wait_idle cannot observe a half state | proposed-not-done | causes "crash at N latched" test flake | docs/plans/exp-archy-s58-bundle-dmt.md
- transport-hop-reduction | TCP_NODELAY / direct req-resp to cut loopback idle RTT (38 ms/view) if transport hops dominate | proposed-not-done | measure first; consensus-visible territory | docs/l3-diagnosis.md fix 5

## build/compiler/allocator

- jemalloc-default | Release torus-node built with jemalloc as the default feature | merged | standard bench recipe | docs/plans/exp-archy-s58-bundle-dmt.md (build)
- mold-framepointers-bench-build | Bench builds link with mold, -C force-frame-pointers, line-tables-only debug for perf call graphs | merged | harness recipe | docs/plans/adl-budget.md §9, exp-archy-s58-bundle-dmt.md
- save-timings-feature-gate | SaveTimings hot-path instrumentation compiled out behind cargo feature save-timings | merged | zero production instrumentation | docs/design-levelhash-cache.md §6

## harness/bench

- native-dup-factor-script | Chain-body ground truth for duplicate inclusion (native-dup-factor.py) | merged | bench Submitted under-counted | docs/plans/dup-inclusion-tail-fix.md
- bench-submitted-accounting | Count drain-phase submissions in bench "Submitted" | proposed-not-done | queued with drain-gate cosmetics | docs/plans/dup-inclusion-tail-fix.md
- ofat-node-side-committed | Measure node-side committed (node_actions_s x batch), not tool-derived orders_s | decided | tool lies under load | docs/plans/direct-to-leader-impl.md
- separate-build-provenance | Build each arm separately and verify binary hash; shared target "Fresh" reused baseline binary | decided | candidate initially identical to baseline | docs/plans/async-validate-flag-off-2026-09-17.md
- steady-window-schedstat | Runner scrapes metrics + schedstat at bench+10 s and end for steady per-block CPU | proposed-not-done | gap_attr.py uses whole load window | docs/plans/async-validate-flag-off-2026-09-17.md
- interleaved-abba-2sigma | Interleaved arms, >=3 repeats, discard warm cell, accept only >=2 sigma vs pooled OFF spread | decided | n=1 spread 12% (8.43 vs 7.46 blk/s) | docs/plans/async-native-trie-maintenance.md §3, §6
- mixed-fleet-agree-cell | Harness support for per-node env (only some nodes with a flag) AGREE cells | proposed-not-done | run-cell forces one env digest | docs/plans/exp-archy-s58-parallel-match-setup.md
- ubench-econ-golden | ubench_econ ignored bench + perf_equivalence_golden pinned per-block digests (serial and engine) | merged | oracle for value-neutral perf changes | docs/plans/crab-perf-fixes-s87.md (C0)
- ubench-adl | ADL microbench with HL / thin-holder / S=750 shapes, per-block units and ms | merged | used for W sizing | docs/plans/adl-budget.md §9
- view-metrics-scrape | Scrape the 9 view_metrics histograms to split idle view time | merged | used in l3 attributions | docs/l3-diagnosis.md fix 5
- header-resend-unset-knob | HEADER_RESEND_UNSET=1 harness knob to run arms without TORUS_HEADER_RESEND_MS | merged | campaign-wide only | docs/plans/exp-archy-s58-bundle-dmt.md
- liq-value-sum-proof | Node-local conservation sum (TORUS_LIQ_VALUE_SUM) logged per liquidation step | merged | proof-only flag | docs/plans/adl-budget-impl.md

## rpc/streams

- rpc-signed-available-balance | RPC available_balance may be negative (signed hex) under strict-HL margin | merged | client must accept sign | docs/plans/account-level-margin-f1-impl.md D1
- precompile-negative-balance-clamp | 0x0801 getBalances clamps negative i128 at 0 instead of wrapping to u128 | merged | T14 | docs/plans/account-level-margin-f1-impl.md
- getorderbook-row-layouts | Precompile getOrderBook removed; RPC torus_getOrderBook reads level row layouts | merged | item 7 step 1 | docs/design-level-rows-authority.md §9.4
- forwarded-evm-pending-notifier | Fire newPendingTransactions for EVM txs forwarded to the leader | proposed-not-done | review follow-up 5 | docs/plans/evm-tx-direct-to-leader.md
- book-state-sync-rows | State-sync / snapshot transport for node-local book order rows | proposed-not-done | fresh nodes replay from genesis | docs/design-level-rows-authority.md §9.3

## other

- governance-snapshot-record | At submit write a per-proposal snapshot record (weighted denominator, frozen quorum/unlock thresholds); no row = no weight; delete at finalize | merged | decided s111, fresh genesis | docs/plans/governance-snapshot-r18-options.md (A)
- governance-snapshot-in-proposal | Snapshot fields inside Proposal borsh | rejected | layout change | docs/plans/governance-snapshot-r18-options.md (B)
- governance-stake-checkpoints | ERC20Votes-style per-voter stake checkpoints instead of an O(stakers) scan per proposal | proposed-not-done | better long-term | docs/plans/governance-snapshot-r18-options.md (C)
- governance-undelegate-lock | Block undelegate of stake that voted on a running proposal | rejected | fixes only half of finding 3 | docs/plans/governance-snapshot-r18-options.md (D)
- governance-delegated-voting | HL-style validator voting or Cosmos-style override voting | parked | later upgrade | docs/plans/governance-snapshot-r18-options.md §6
- evm-call-geth-semantics | eth_call/estimateGas: disable_base_fee, no balance requirement when fee fields omitted | merged | D2 | docs/plans/evm-blocker-set-decisions.md
- evm-fee-floor-dynamic | Reject EVM txs with max_fee below current base fee at admission and drain | merged | D4 | docs/plans/evm-blocker-set-decisions.md
- evm-receipt-alignment | Map receipts through included_indices; skipped txs get no receipt | merged | D5 | docs/plans/evm-blocker-set-decisions.md
- sigterm-hang-repro | Reproduce wedge-frozen node ignoring SIGTERM | parked | healthy seed exits < 5 s | docs/plans/blockspeed-orders-roadmap-s405.md
- explicit-gossip-peering | Gossipsub explicit peering for validator-set peers + mesh watchdog | merged | wedges 8/10 -> 0/10 on devnet | docs/plans/blockspeed-orders-roadmap-s405.md, commits 576d3be7, 8e9e1620
