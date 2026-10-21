# 8k explore, Phase 3: idea report and ranking (s68, archy, 2026-10-19)

Phase 3 of the 8k prompt (`docs/perf/8k-explore-workflow.md`). Phase 2 had 6 lens agents (lens 1 consensus and
cadence, 2 execution engine, 3 state and storage, 4 networking and DA, 5 cryptography, 6 wildcard). They proposed
21 ideas. One critic per idea tried to kill it, and then the ranker scored them. This file is written before any
build. Its status column is kept current through Phases 4-6. Sections are cited as "s<N>" =
`docs/perf/ozarchy-antispam-item6-pf1-2026-10-04.md` section N. Baseline = s94 arm B (main with 8g) and s95,
standard load, 300 markets. The deep book (1M resting orders) is s96 P1M. Progress log: `docs/perf/8k-explore-log.md`.

**Picks: L3.3 (exact-size Classic book encoder) first, L2.2 (no-merge position flush) second.** Both are node-local
and byte-identical. Each costs 1.5-2.5 days, and each has a microbench seam on main. Neither one touches 18c's
item 7 option 0 code or RH4, and neither re-opens item 6 Phase 5 or checkpoint slicing. No consensus-changing idea
is picked. Their gains on the rig are not clearly larger, and the largest of them (L6.1) is blocked by the Rules.

## Score

score = G x C x L x R / days. Every term uses the critic's new estimate, not the proposer's, and takes the
midpoint of each range.

* **G** (gain, % points) = M + 0.5 E + 0.5 D + 0.25 P + 0.5 Lat. In this formula:
  * M = matched/s at the standard load.
  * E = exec-thread on-CPU (or exec wall) per 1k fills at the standard load. This is the primary metric of the
    exec-path merge rule.
  * D = matched/s at 1M resting orders.
  * P = CPU-s per 1M fills (lower is better, counted as positive).
  * Lat = p50 latency at the shape the idea targets.

  Only what archy can measure counts. WAN-only gains count 0 and are listed in the footnotes.
* **C** (confidence) = 0.3 for low, 0.45 for low-medium, 0.6 for medium, 0.8 for high.
* **L** (locality) = 1 for node-local and byte-identical, 0.5 for consensus- or genesis-changing (research-only).
* **R** (rules / production) = 1 when the idea is clear. It is 0.5 when the idea needs code in 18c's area or its
  gain disappears in production. It is 0.75 when it works next to 18c's code with a moderate merge-conflict risk.
* **days** = midpoint of the critic's cost.

## Ideas

| id | idea | lens | mechanism (one line) | attacked cost (row, size, section) | expected gain range | confidence | cost | locality | critic verdict | score | status |
|---|---|---|---|---|---|---|---|---|---|---|---|
| L1.1 | Unanimous-QC 1-chain commit | 1 | Commit B_h when its own QC carries all n votes, plus a vote-lock rule; otherwise fall back to the 2-chain rule | Second-chain wait: one view of order-to-commit (paced p50 159 ms, commit interval p50 8.4 ms, s88); WAN view ~1 RTT 227-502 ms (T0.2) | standard 0; paced p50 -3 to -12%; WAN < 1 RTT [^l11] | low | 8-14 d (incl. model) | consensus-changing | weakened | 0.05 | backlog (research) |
| L1.2 | Optimistic exec at speculative 1-QC | 1 | Feed exec at the B2 speculative commit; keep durability and checkpoints at the irrevocable height; roll back on mismatch | Exec idle until the 2-chain commit at paced load (s88); 0 at saturation (s90.3) | standard 0; paced order-to-result -2 to -6% [^l12] | low | 8-15 d | node-local, but splits 18c's on_committed_block / flush-worker code | weakened | 0.03 | backlog (research) |
| L1.3 | Cadence-invariant liquidation scan | 1 | Liquidation budget = rate x (header time delta), capped at 2,048, instead of 2,048 per block | Liquidation 7.0-7.8% of exec, 5.6-5.9 ms per block (s90.4) | E -1% (coverage-neutral); M 0 to +1.5% [^l13] | low-medium | 2-3 d | consensus-changing (genesis rate; reopens D5) | weakened | 0.11 | backlog |
| L2.1 | Streamed in-order settle | 2 | Fused match + plan jobs publish per market; the exec thread runs pass B in market order while the pool still works | Pool waits match 7.3-8.1 + plan 4.1-4.6 ms per block (s92.3), serial pass B 13.6-17.3 ms per block (s90 phase-val0) | exec block_ms -4 to -8%; M +0.5 to +3% [^l21] | low-medium | 5-7 d | node-local | weakened | 0.37 | backlog |
| L2.2 | No-merge position flush | 2 | Pass B keeps the per-market PositionCaches. The end-of-call flush writes their dirty rows in the same global key order, so no merge_disjoint and no second lookup | `merge_disjoint` 4.6-4.9%, 3.5-4.1 ms per block; `PositionCache::flush_all` self 3.5% and its driftsort 2.6-2.7% (s90.4, q8re W1) | E -3 to -6%; M +0.5 to +2%; P -0.5 to -1.5% [^l22] | low-medium | 1.5-2.5 d | node-local | weakened | **0.84** | **picked (2); built `dbe14b21` (exp/8k-settle-no-merge), microbench passed (saving 5.3 ms/block at 16.8k fills, 7.0-7.2 ms at 22.8k; stop line 1.0 ms), workspace 4163/4163; not benched: 8k bench budget used by L3.3 (prompt: pick 2 gets the budget only if pick 1 dies)** |
| L2.3 | Seq-inline level queues | 2 | Each level keeps a lockstep `VecDeque<u64>` of seqs; partition_point becomes a contiguous search, not HashMap probes | Cancel-all partition_point 4.1 ms per block of pool CPU; cancel-all wait 3.55-4.36 ms (s92.3) | M +0.2 to +0.6%; pool CPU -3 to -3.6 ms per block [^l23] | low | 2-4 d | node-local | weakened | 0.05 | backlog |
| L2.4 | Dense trader slots per market job | 2 | Intern each address once per market job; margins and positions use Vec slots | Address hashing ~16-18 ms per block of pool CPU (q8re W1) | M 0 to +0.5%; P -0.5 to -1.5% [^l24] | low | 4-6 d | node-local | **dead** (s82 hasher precedent: ~3% node CPU of hashing removed, no gain) | (0.03) | dead |
| L3.1 | Checkpoint-coalesced Classic book save | 3 | Skip the per-block whole-book encode and encode each dirty book once at the checkpoint window close (only while the running hash is off) | `save_order_books` 3.6-5.9 ms per block standard, 78.9 ms at P1M; settle wait 23.7 ms at P1M (s90.4, s96.2-96.3) | E -6 to -8%; M +1 to +3%; D +8 to +17%; 0 once the running hash is on [^l31] | low-medium | 4-6 d | node-local (gated on hash off) | weakened | 0.54 | **built `perf/l31-coalesced-save` b7bb3666 (s131); bench 8y (perf doc section 110, n = 2): deep matched/s 1.160, exec-thread CPU per 1k fills 0.802, blocks/s 3.42 -> 15.76, p50 193 -> 13 ms; standard exec CPU 0.958; merged to main ON by default where its preconditions hold (checkpoint interval, no byte cap, Classic resident books, hash off), `TORUS_BOOK_COALESCE=0` = off (s131)** |
| L3.2 | Running hash v2 for Classic books | 3 | The running hash absorbs per-block order-row deltas as hash-only extras instead of the whole-book blob | Same rows as L3.1, with the hash on; the hash is off in every cell measured | 0 on every archy shape [^l32] | low | 5-8 d | genesis-changing | **dead** (hash off in all cells; 18c areas; weakens fork detection) | 0 | dead |
| L3.3 | Exact-size fixed-layout Classic book encoder | 3 | One size pass, then a presized Vec with fixed 97 B order records. Bytes identical to `borsh::to_vec` | save_books at P1M 82.7 ms per block: serialize 60.5 (73%) + memmove 12.2 (15%) + alloc 2.4 (s96.3); standard 0.217 ms per 1k fills (s96.2) | D +3 to +8% (P1M save_books -30 to -50%); E -1.5 to -3%; M ~0 [^l33] | medium | 1-2 d | node-local | weakened | **1.60** | **picked (1); built `exp/8k-book-fast-encode` 4b916ecf (microbench P1M encode -30 to -36% spread sizes, -5.5 to -18.7% equal sizes); benched n = 3, 14 / 14 cells valid; merge rule met on both shapes; save_books per 1k fills -15% (standard 0.8454 [0.7617, 0.9382], deep 0.8534 [0.8355, 0.8716]); deep exec on-CPU per 1k fills 0.9676 [0.9622, 0.9731]; matched/s not resolved (standard 1.0046, deep 1.0140 [0.9955, 1.0330]); below the ranker's deep +3 to +8%; merge candidate, flag default off (s97.8)** |
| L3.4 | FIFO compaction for the DA CF | 3 | RocksDB FIFO style for `cf_native_pending` | Consensus-DB DA compaction ~120 CPU-s per node per cell (memory 005b6c03) | P -1.5 to -3%; M 0 [^l34] | low | 1-2 d | node-local, but changes DA retention | **dead** (owner ruled FIFO out, `docs/plans/append-cf-compaction.md`; breaks heal / RH4) | (0.11) | dead |
| L4.1 | No relay of author-direct native gossip | 4 | `validate_messages` + Ignore when the source is the validator author and all subscribers are validators | tokio 0.34-0.44 cores per node (memory dabcab1c); duplicate factor ~1.55x (memory db0cb68d, unverified on main) | P -0.7 to -1.5%; M 0; WAN bytes -30 to -36% [^l41] | low-medium | 1-2 d | node-local | weakened | 0.08 | backlog |
| L4.2 | Schema-aware compact gossip codec | 4 | Varint, bounded-expansion codec for native batches on a new topic version | Same tokio row; ~13.2 KB per action on the wire (memory db0cb68d) | P -1 to -2.5%; M 0; WAN bytes -25 to -74% [^l42] | low | 3-4 d | node-local (wire topic) | weakened | 0.04 | backlog |
| L4.3 | Prefilled pre-proposal manifest | 4 | Flush the gossip batch before the push (E0); inline likely-missing bodies in the manifest (E1) | ~0.93 pre-warm pulls/s per node, ~46 bodies each (8i P0 logs; s86.4) | archy 0; WAN up to 1 RTT off the QC for ~8-15% of blocks [^l43] | low | 0.5-3 d | node-local (wire capability) | weakened | 0 | backlog (WAN) |
| L4.4 | BBR + BDP-sized QUIC windows | 4 | Patch libp2p-quic to expose quinn BBR and a larger send window | WAN dissemination ceiling under loss (p3-testnet RESULTS, unverified) | archy 0; WAN 2-5x per-link ceiling only if loss >= 1e-5 [^l44] | low | 2-3 d | node-local | weakened | 0 | backlog (trial testnet) |
| L5.1 | EIP-712 item-hash memo cache | 5 | Per-thread 3-level memo of the PlaceOrderItem sponge states, keyed on preimage byte prefixes | Item Keccak 0.41-0.43 cores per node (memory 0a51b36b); verify pools 13-14% of node samples each (s90 W1) | P -2 to -6.5% (the upper end is a bench artefact); M 0 to +0.5% [^l51] | low-medium | 1-1.5 d | node-local | weakened | 0.47 | backlog |
| L5.2 | Known-key ECDSA check with per-signer tables | 5 | Verify against the claimed sender's cached key with a comb table, instead of ecrecover | Recovery 0.38-0.43 cores per node (memories 4864a390, a48de6c0) | P -2.5 to -4% standalone; -0.5 to -1.5% after libsecp [^l52] | low | 3-4 d | node-local (consensus-critical code) | weakened | 0.07 | backlog |
| L5.3 | Session-signed low-CPU path | 5 | (a) stateless session-signature trust cache; (b) a v2 session digest over packed bytes | Verify-pool crypto 216-241 ms CPU per block (s90 W1) | 0 on the standard EIP-712 load; P -7 to -11% only if all market-maker flow moves to v2 [^l53] | low | 5-7 d | consensus-changing (b) | weakened | 0 | backlog (research) |
| L6.1 | Window-committed order books | 6 | Hash and save Classic book blobs only at %100 attestation heights | Same rows as L3.1 (s90.4, s96.2-96.3) | E -5 to -7%; M 0 to +2%; D +5 to +12% [^l61] | low-medium | 5-8 d | consensus-changing | weakened | 0.14 | backlog (research; blocked until option 0 lands) |
| L6.2 | Attestation-window state commitment for all hashed CFs | 6 | Per-block response hash + window digest; a window-scoped typed write-back layer | Cache `flush_all` 13-14% (s90.4) | ~0 or negative (s91) [^l62] | low | 3-4 weeks | consensus-changing | **dead** (premise false: flush_all is not caused by the hash; re-opens Phase 5, s91 loss) | 0 | dead |
| L6.3 | Pre-exec lookahead verify stage | 6 | A dedicated thread computes trust-cache keys, sort hashes and miss recovers one block ahead off the exec channel | Exec verify 3.68 ms per block, ~8% of exec on-CPU (s94.2) | E -4 to -7% (full-channel windows only); M +0.5 to +2%; P 0 to +0.5% worse [^l63] | low-medium | 3-5 d | node-local | weakened | 0.33 | backlog |

Scores in brackets are for dead ideas (shown for completeness, not ranked).

**Top 5:** L3.3 1.60, L2.2 0.84, L3.1 0.54, L5.1 0.47, L2.1 0.37 (next: L6.3 0.33).

### Gain arithmetic (footnotes)

[^l11]: Critic: one view at paced load is ~8-20 ms of a 159 ms p50, because the 37.7 ms mean interval comes from the
    p99 tail. The commit still waits for the body (vote before body), which eats much of the WAN RTT. G = 0.5 x 7.5 =
    3.75. C 0.3, L 0.5, 11 d: score 0.05. Locking on the last voted block also needs timeouts that carry votes.
[^l12]: Results can be released only at max(irrevocable commit, speculative exec done), so the saving is min(exec
    latency after commit, one view) ~5-15 ms of ~170 ms. Falsifier (b) already fires on s88 (commit-interval p50
    8.37 ms < 10 ms). G = 0.5 x 4 = 2. C 0.3, R 0.5 (18c's on_committed_block EVM section and the flush-worker
    marker), 11.5 d: score 0.03.
[^l13]: Liquidation cost scales with traders scanned, not with block count. A coverage-neutral rate saves ~0.6-0.9 ms
    per block (~1% of exec). A larger cut needs ~3x lower per-second coverage (reopens D5). s94 calibration: -4.5%
    exec gave +1.5% matched/s (n.s.). G = 0.75 + 0.5 x 1 = 1.25. C 0.45, L 0.5, 2.5 d: score 0.11.
[^l21]: Amdahl cap min(pool wall 8.6, pass B 9.1 ms) at s94's 16.8k fills. 30-55% of that is hidden, so -2.5 to -5 ms
    per block. s124 sized fusion alone at +0.5 to +1.3% (memory c4ea10ed). Pass B is DRAM-bound and will slow next
    to busy workers. G = 1.75 + 0.5 x 6 (wall) + 0.25 x 0.55 = 4.89. C 0.45, 6 d: score 0.37.
[^l22]: merge_disjoint at s94's block size (16.8k fills vs q8re's 22.8k) is ~2.6-3.0 ms per block = 0.155-0.18 ms per
    1k fills (5.8-6.7% of 2.695). The second lookup is ~half of flush_all self, ~1 ms per block. The global sort stays
    (it keeps the overlay BTreeMap inserts at the right edge; critic point (a)). Net -0.08 to -0.16 ms per 1k fills =
    -3 to -6%. s94 calibration maps that to matched/s +0.5 to +2%. G = 1.25 + 0.5 x 4.5 + 0.25 x 1 = 3.75. C 0.45,
    2 d: score 0.84.
[^l23]: partition_point is 4.05 ms per block of pool CPU (2.05% of pool self). The wait is short of parallel work
    (~5 jobs), so the slowest job's -15-17% gives ~0.3-0.4 ms off the 62 ms block. A lockstep queue adds a push and
    pop on the hottest pool rows. G = 0.4 + 0.25 x 0.4 = 0.5. C 0.3, 3 d: score 0.05.
[^l24]: 60-75% of ~2.1% of node cycles = 1.2-1.7%. Match and plan waits are ~50% scheduling, so ~0.3-0.45 ms per block.
    s82 removed ~3% of node CPU of hashing and measured nothing (p = 0.91). G = 0.25 + 0.25 x 1 = 0.5. 5 d: (0.03).
[^l31]: Standard: save_books 0.214 of 2.695 ms per 1k fills (7.9%), minus one amortised encode per 15 s window. The
    settle-wait share is ~0 to -0.5 ms (s82 DB defer-books: +0.95%, within noise). P1M: -(78.9 - ~2) ms of save and
    part of the 23.7 ms settle wait, of 329.9 ms. 8i's own data maps ~1:1, with a +19% ceiling (P0 263.7k vs 221.4k).
    The gain becomes 0 when the running hash is activated (planned for the testnet upgrade). The window-close hook
    sits in app.rs CkptState / exec_pipeline.rs (18c). The `rank8` stale-books rebuild must fail-stop. G = 2 +
    0.5 x 7 + 0.5 x 12.5 + 0.25 x 1 = 12.0. C 0.45, R 0.5, 5 d: score 0.54.
[^l32]: The running hash is off in every baseline cell ("state_hash_activation = off" in the p8g and 8i run.logs), so
    the W-side SHA-256 the idea removes was never measured and G = 0. Not buildable (frozen preimage, 18c's
    HashExtras / backend area, genesis change). Delta hashing also extends the s83 untouched-account blind spot to
    resting orders.
[^l33]: P1M per block: ~1.6M orders x 176 B read + ~155 MB written. Encode + memmove = 72.7 ms (~38 ns per order). The
    s58 microbench (memory 722f6f35) shows serial save is memory-bound (8 workers gave only 2.8x), so a fixed-layout
    single-thread writer realistically gets 1.5-2x, i.e. -25 to -40 ms of 82.7 ms (-30 to -50%). That is -8 to -12%
    of the 329.9 ms block. 8i maps exec per fill ~1:1 to matched/s, so D +3 to +8%. Standard: 0.217 ms per 1k fills x
    0.3-0.5 = -0.07 to -0.11 ms of 2.695 (-1.5 to -3% E). s78's B (save_books 14.25 -> 4.05 ms per block) moved
    matched/s +0.09%, so M ~0. The checkpoint settle wait (23.7 ms at P1M) stays. G = 0 + 0.5 x 2.25 + 0.5 x 5.5 +
    0.25 x 0.45 = 3.99. C 0.6, 1.5 d: score 1.60.
[^l34]: 120 CPU-s per node per 600 s cell / 156M fills = 0.77 CPU-s per 1M per node. The critic re-sized rocksdb:low for
    all DBs at 2.2-2.7% of node user cycles (q8re W1). Dead by the owner's decision (s108). G = 0.25 x 2.25 = 0.56,
    1.5 d: (0.11).
[^l41]: tokio is 5.7% of node user cycles at P1M. 36% of it is ~2%, plus an unmeasured sys share, so P -0.7 to -1.5%.
    tokio is off the exec path, so M = 0. The 8i numbers (285k published, 571k received) are the n-1 = 2 delivery
    factor, not duplicates, so the 1.55x must be re-measured on main. WAN bytes -30 to -36% is not measurable on
    archy. G = 0.25 x 1.1 = 0.275. C 0.45, 1.5 d: score 0.08.
[^l42]: Only the byte-proportional tokio rows shrink (SHA-512, ChaCha20-Poly1305, part of QUIC and protobuf). The
    receiver still re-encodes bincode for mempool / DA. The 3.8x ratio holds for 246-order bench actions only
    (1.3-2x for 1-5 order actions). It is subsumed by pre-spread-direct-zstd if that ships. G = 0.25 x 1.75 = 0.44.
    C 0.3, 3.5 d: score 0.04.
[^l43]: On loopback a pull costs no RTT, and s86 found no throughput cost from the manifest path, so G = 0. The only
    observable is the pull count, which is not a merge-rule metric. ~2/3 of the missing bodies may come from the other
    follower's ingress, so E0 / E1 may be mis-aimed. On a WAN with vote-after-body (s85) and n = 3 (3/3 quorum), a
    follower pull delays the QC, which is the real value but is unmeasured.
[^l44]: Cubic at 300 ms RTT: ~1.3 MB/s at 1e-4 loss and ~7.5 MB/s at 1e-5, against ~7.3 MB/s per link of demand at
    260k matched/s. The p3 testnet ran at ~1-2k matched/s, far below the Cubic ceiling, so its saturation was not
    shown to be loss-bound. quinn's BBR is marked experimental. G = 0 on archy.
[^l51]: The item row is 0.41-0.43 cores of a ~6.2-core node (s94: 23.3 CPU-s per 1M at 265k/s), ~6.8%. Standard load:
    L3 hits ~100% (econ quantity is a pure function of price, cloid None) for -5.5 to -6.5%, inflated by the bench.
    With L1 hits only (unique sizes and cloids): -2 to -2.5%. The verify pools are off the exec path (trust-cache
    hits ~100%, s92.3). G = 0.25 + 0.25 x 4.25 = 1.31. C 0.45, 1.25 d: score 0.47.
[^l52]: Simpler arms on the same row: k256 without the trailing verify -3.3 to -3.7%, libsecp -5.0 to -5.7%. Without
    GLV / var-time internals exposed by k256, the margin over libsecp is ~1-1.5x. Any accept-set difference forks
    through the trust cache. G = 0.25 x 3.25 = 0.81. C 0.3, 3.5 d: score 0.07.
[^l53]: Under v2, Keccak falls ~75% (the trust-cache key and the action hash still absorb the canonical bytes).
    ed25519 vs k256 recover is ~2x (loopback FINDINGS: session sign-mode ~flat vs eip712). That gives ~86 of ~813
    node-CPU ms per block (~-10%), only with 100% market-maker v2 flow. The standard bench load gains 0, and a v2 load
    breaks A/B comparability. G = 0 on the bench.
[^l61]: Standard: save_books 7.4-8.5% of exec per fill, minus one window save per 100 blocks. P1M: save_books
    1.07 -> ~0.01 ms per 1k fills, part of the settle wait, and ~80 ms spikes at each %100 height and checkpoint. Every
    resident-holder drain / rebuild path must save first or fail-stop. It touches running_hash.rs and backend.rs
    block_digest (18c), and the owner judged a book-hash fork not justified (book-digest-3c). G = 1 + 0.5 x 6 +
    0.5 x 8.5 = 8.25. C 0.45, L 0.5, R 0.5, 6.5 d: score 0.14.
[^l62]: flush_all costs 0.405 ms per 1k fills with the hash off (s94). The cause is the one-call cache lifetime
    (native_executor.rs ~8611), not the hash. The unlock is a typed write-back layer, and s91 measured that as a loss
    (exec per fill +5.0% [1.033, 1.068], blocks/s -52%). It re-opens Phase 5. G <= 0.
[^l63]: 3.68 ms per block at 16.8k fills = 0.219 ms per 1k fills (8.1%), minus ~0.2-0.5 ms of handoff and session
    resolve. This holds only in windows where the exec channel is full (6 of 9 in s90.3). It adds a third absorb of
    the canonical bytes (CPU flat or worse). K1 (dispatch-hash key) is the cheaper competitor on the same row. G =
    1.25 + 0.5 x 5.5 - 0.25 x 0.25 = 3.94. C 0.45, R 0.75 (app.rs native verify region and exec-channel path next to
    18c's work), 4 d: score 0.33.

## The picks and why

Both picks were re-checked against the code on this branch (`adc7fddd` = origin/main `4811e2b0` + docs) and against
the cited sections.

### Pick 1: L3.3, exact-size fixed-layout Classic book encoder (branch `exp/8k-book-fast-encode`)

* **Why:** it has the highest score by a wide margin (1.60). It is the cheapest idea (1-2 days, ~100-160 lines), and
  the critic gave it medium confidence on the deep-book microbench. It is node-local and byte-identical, has no
  determinism, crash or replay risk (the overlay receives the same bytes), and touches no 18c area. It attacks the
  largest single deep-book row: s96.3 measured 60.5 ms serialize + 12.2 ms memmove of 82.7 ms per block at P1M.
  It is materially more than the open s96.6 "presize the borsh output buffer" item, which covers only the 15%
  memmove. L3.3 also replaces the 73% per-field serialize, and it includes the presize as a side effect. Unlike L3.1
  and L6.1, its gain survives the running state hash being turned on, because the same blob is still written every
  block.
* **Checked in the code:**
  * `impl BorshSerialize for Order` (`crates/torus-core/src/order_book.rs:4336`) issues ~13 `write_all` calls per
    order. The layout is 16 (id) + 20 (trader) + 1 (side) + 3 x 16 (price, remaining, original as i128 BE) + 1 (order
    type) + 1 (TIF) + 8 (timestamp) + 1 (reduce_only) + 1 (cloid tag) = 97 B. A cloid adds 16 B, a stop trigger 16 B,
    a stop limit 32 B.
  * `impl BorshSerialize for OrderBook` (`:4604`) writes a header, a u32 order count (one sum pass over the
    levels), bids and then asks by level in FIFO order, and the pending stops.
  * The Classic arm of `save_order_books` (`crates/torus-bridge/src/native_executor.rs:6960`) calls
    `borsh::to_vec(&*book)`. That call starts at 1 KiB and grows by realloc, which is the memmove row; memory
    6b3b0bd9 found the realloc chain to be 27% of the encode. `save_book_full` (`:7472`) is the genesis / offline
    caller.
* **Cost it attacks:** Classic `save_order_books` encode: 72.7 ms per block (serialize + Vec growth) + 2.4 ms alloc at
  P1M (s96.3); 0.217 ms per 1k fills at the standard load (s96.2 P0, s94).
* **Expected gain:**
  * P1M: save_books -30 to -50% (-25 to -40 ms per block of 329.9), save_books per 1k fills 1.07 -> ~0.55-0.75,
    matched/s +3 to +8%, blocks/s and p50 better.
  * Standard: exec on-CPU per 1k fills -1.5 to -3%, CPU-s per 1M -0.3 to -0.6%, matched/s ~0 (s78 precedent).
  * The checkpoint settle wait (23.7 ms at P1M) is not attacked.
* **Build spec:** flag `TORUS_BOOK_FAST_ENCODE=1`, default off, read once (OnceLock, as
  `settle_defer_results_enabled`). Add `OrderBook::encode_classic(&self) -> Vec<u8>` in torus-core. It runs one size
  pass (header + 97 B per order + 16 per cloid + 16 / 32 per stop-type order + stops), then
  `Vec::with_capacity(exact)`, then per order one stack `[u8; N]` record filled from `to_be_bytes` and appended once.
  Stops reuse `StopOrder::serialize`, and a `debug_assert` checks length == size. Use it in the Classic arm of
  `save_order_books` when the flag is on.
* **Tests first:** a proptest that the bytes equal `borsh::to_vec` over random books (both sides, empty book,
  `last_trade_price` None / Some, cloid None / Some, all order types and TIFs, pending stops), plus an executor test
  over a block stream with the flag off vs on: identical overlay pending dump, `h_n` (running hash on in the test) and
  receipts. The workspace nextest stays green.
* **Microbench and stop rule:** a release build with jemalloc as the global allocator (as the node). Encode 300 books
  holding 1.6M resting orders in total (P1M shape), plus a 0.21M shape (P0), with `borsh::to_vec` and with
  `encode_classic`. The bytes must be equal. The expected cut is ~40% of the encode time (mid of -30 to -50%). **Stop
  if the cut at 1.6M is < 20% (speed-up < 1.25x).**
* **Bench shape:** the standard cells (A B B A A B), plus P1M cells on the 1M-account genesis with `PRELOAD=1000000`
  (8i shape; A B B A A B if the ~4 h budget allows, otherwise A B B A).

### Pick 2: L2.2, no-merge position flush (branch `exp/8k-settle-no-merge`)

* **Why:** it has the second-best score (0.84) and is the best standard-load exec idea per day of work. It moves the
  merge rule's primary metric (exec-thread on-CPU per 1k fills) on the exec thread itself. It is cheap (1.5-2.5
  days) and has an existing microbench seam (`crates/torus-core/tests/ubench_position_cache.rs`,
  `ubench_position_cache_merge`). It is node-local and byte-identical, and it gives the same write sequence when
  the global sort is kept.
  * Rules: it touches only `torus-core/src/position.rs` and the parallel settle in `native_executor.rs`. That is not
    18c's option 0 code and not `torus-state`.
  * It does not re-open Phase 5. Phase 5 step 2 replaced the flush with typed in-place writes (s91 loss), whereas
    L2.2 keeps the `put_position` / `delete_position` per-row flush and removes only the merge and the second lookup.
  * s28.1 named "keep the per-market caches separate and sort their dirty keys once at flush" as 18c's possible next
    step. It was never built, and it is not in the Phase 1 list.
  * It and L3.3 attack different shapes (standard vs deep book), so the two builds do not compete for one signal.
* **Checked in the code:**
  * In the parallel path the batch `pos_cache` is created fresh (`native_executor.rs:8161`) and filled only by pass
    B's `pos_cache.merge_disjoint(plan.pos_cache)` (`:9568`, after the `reserve` at `:9543`). Nothing reads it between
    the merges and the end-of-call `pos_cache.flush_all` (`:8630`). The only other users are the sequential settle
    (`:9195`, `:9220`) and the fallback (`:9528`), which stay unchanged.
  * `PositionCache::merge_disjoint` (`position.rs:855`) is `map.extend` + `dirty.extend`. It moves every 144 B entry,
    clean read-hits included.
  * `flush_all` (`:883`) collects the dirty keys, sorts them, and does a second `map.get` per key before
    `put_position`.
* **Critic correction adopted:** the global key order is kept. `position_key` is trader || market BE, so today's sort
  makes the overlay `BTreeMap` inserts right-edge appends. Writing grouped by market would make them random inserts
  and give the saving back. The gain is therefore the merge plus the second lookup, not the sort.
* **Cost it attacks:** `merge_disjoint` 4.6-4.9% of exec, 3.5-4.1 ms per block at q8re's 22.8k fills (s90.4), ~2.6-3.0
  ms at s94's 16.8k. Plus `flush_all`'s second lookup, ~1 ms per block.
* **Expected gain:** exec on-CPU per 1k fills -3 to -6% (-0.08 to -0.16 ms of 2.695), matched/s +0.5 to +2% (likely
  below n = 3 resolution, as in s94), native blocks/s up, CPU-s per 1M -0.5 to -1.5%.
* **Build spec:** flag `TORUS_SETTLE_NO_MERGE=1`, default off. In the parallel pass B, the per-market caches are kept
  in a `Vec<PositionCache>` instead of being merged. At the end-of-call flush, the code gathers (key, reference to
  the entry) for every dirty key of the batch cache and of each run, and debug-asserts that the keys are disjoint.
  It sorts by key (the keys are unique, so the order is the same as today's), then calls `put_position` /
  `delete_position` in that order. The sequential path, the fallback and the flag-off path stay unchanged.
* **Tests first:** flag off vs on over a realistic block stream, including the fallback and a fatal-plan block. The
  overlay pending dump, `h_n` and receipts must be identical, and the write sequence must be identical too (a
  recording backend).
* **Microbench and stop rule:** extend `ubench_position_cache_merge` to the s94 shape (300 markets, ~16.8k fills per
  block of dirty positions plus clean read hits). Compare merges + `flush_all` against the runs flush, both into a
  real `NativeStateOverlay`, so the BTreeMap inserts are counted. The expected saving is ~2 ms per block (mid of
  -0.08 to -0.16 ms per 1k fills). **Stop if the saving is < 1.0 ms per block (< 0.06 ms per 1k fills).**
* **Bench shape:** the standard cells (A B B A A B) are enough.

### Not picked (and why)

* **L3.1 (0.54):** the biggest node-local deep-book gain, but its value is 0 once the running state hash is activated,
  which is planned. Its window-close hook sits in 18c's `app.rs` CkptState / `exec_pipeline.rs` area, and the
  `rank8` stale-books rebuild path must become a fail-stop.
* **L6.1 (0.14):** the largest consensus-changing deep-book idea, but its gain is not clearly larger than L3.1's or
  L3.3's, and it is blocked by the Rules (backend.rs `block_digest`, the overlay region).
* **L5.1 (0.47):** a CPU-only gain whose standard-load hit rate is a bench artefact. matched/s 0.
* **L2.1 (0.37):** a larger wall-clock gain than L2.2 is possible, but at 3x the cost, with a fault-fallback proof
  and an s124 precedent of +0.5 to +1.3% for its first step.

## Falsifying experiment for each backlog idea

* **L1.1:**
  * Zero code: in the Q9 paced logs, per height, take the collector's QC(B_h) time minus each node's
    `on_committed_block(B_h)` dispatch time. A median < 10 ms means the archy gain is null.
  * Then extend `crates/hotstuff_rs/tests/stateright_2chain_safety.rs` with the unanimity commit plus the vote lock
    (n = 3 and 4, one Byzantine voter, lost AdvanceViews). Any conflicting commit, or a liveness lock without
    vote-carrying timeouts, kills it.
* **L1.2:**
  * From the Q9 logs: the fraction of dispatches with `torus_exec_queue_depth == 0`, and the QC(B_h) to commit gap
    (already < 10 ms at the p50 in s88).
  * Count heights whose QC'd block differs from the committed one. If that is > 1 per 1,000, it is dead.
* **L1.3:**
  * Zero code: from an s94 / s95 cell, liquidation ms per block against accounts valued per block. If the cost is
    not ~linear in accounts valued, it is dead.
  * Then a one-line `LIQ_SCAN_PER_BLOCK = 683` smoke A/B. If exec on-CPU per 1k fills falls < 1.5%, it is dead.
* **L2.1:** a timers-only probe on one standard cell: per market, the fused match+plan finish time and the pass B
  duration, ~500 blocks, then an offline simulation of the market-ordered consumer with ~20 workers. If the simulated
  saving is < 2.5 ms per native block, drop it.
* **L2.3:** microbench `cancel_all_many` on a 300-market book at standard depth and at 1M resting, comparing the
  HashMap-probed `partition_point` with an inline seq (try the cheaper variant first: carry `seq` inside the
  in-memory `Order`). Standard: < 10% job-time cut means drop. Keep it only if the cut is >= 25% at 1M.
* **L3.1:**
  * Zero build: a standard cell with `TORUS_CKPT_DEFER_BOOKS=1`. Read `torus_exec_checkpoint_deferred_read_waits`
    (must be 0) and grep for mid-run "rank8: resident books stale".
  * Then a throwaway build that puts the Classic blob only every 40th block, with one 300 s P1M cell. If exec block
    ms falls < 45 ms or matched/s rises < +5%, kill it.
  * Run this after L3.3, which lowers the same row.
* **L4.1:** on main, the libp2p-gossipsub metrics `msg_recvd_unfiltered` vs `msg_recvd` must show >= 1.4x per
  delivered native batch. Then a 300 s smoke with the flag on: the duplicates fall to ~1.0x, and tokio-rt-worker CPU
  (pidstat -t) falls >= 0.05 cores per node, or the idea is dead.
* **L4.2:** an offline codec microbench on 8i P0-shaped batches and on 1-5 order actions. A size ratio < 2.5x at
  bench shape, or encode + decode + bincode re-encode > ~0.5 us per action, kills it. Demote it if pre-spread-direct
  zstd is scheduled.
* **L4.3:** first log whether the pulled hashes were later seen by gossip from the proposer as author. If < 50% are,
  E0 / E1 are mis-aimed. Then an E0 smoke: pre-warm pulls must fall >= 50% vs the 8i P0 rate (~0.93 per s per node).
* **L4.4:** a root-free userspace UDP proxy microbench (150 ms each way, 1e-4 loss), Cubic vs BBR on one quinn stream
  for 60 s. Cubic >= 15 MB/s, or BBR < 2x Cubic, kills it. On the trial testnet, quinn path loss <= 1e-6 makes it moot.
* **L5.1:**
  * Criterion microbench of `hash_place_order_batch` over 400 econ orders, cold vs warm, plus an L1-only variant
    (random cloid and size). Warm < 3x or L1-only < 1.25x means drop.
  * Then log 60 s of item keys from a standard cell and from a CLOID_SHARE=100 cell. If the L1 hit rate is < 70% in
    the cloid cell, it is a bench artefact.
  * Read the flag on the node only (the loader signs through the same function).
* **L5.2:** a microbench with 5,000 signers x 140 signatures: k256 recover vs the table check vs libsecp. If the table
  check is < 3x k256 or < 1.5x libsecp, the libsecp arm is the answer. A 1M-case differential fuzz plus constructed
  KATs (r + n overflow, recid 2/3, high-s) gates correctness.
* **L5.3:**
  * Microbench per 400-order batch: EIP-712 struct hash + k256 recover vs Keccak over packed bytes + ed25519
    `verify_batch`. < 4x means drop.
  * A 600 s cell with the Lever-1 session loader: exec verify must rise >= 10 ms per block, or part (a)'s premise is
    wrong.
* **L6.1:** an env-flag build that saves Classic books only at checkpoint heights (hash off in both arms), with every
  holder drain / rebuild path forced to save first, one standard and one PRELOAD=1000000 cell. At 1M, an exec ms per
  1k fills cut < 15% or matched/s < +5% kills it. Research-only until 18c's option 0 lands.
* **L6.3:**
  * Zero code (passes on s94: 3.68 ms > 2 ms per block): re-read `torus_exec_verify_seconds` per native block on
    current main.
  * Then a microbench: the exec-side remainder (session resolve + sort with supplied hashes) for 100 actions x 239
    orders must be < 25% of today's verify + sort.
  * Compare it with K1 before building.

Dead (no experiment planned): L2.4 (s82), L3.2 (hash off in every cell; Rules), L3.4 (owner ruled FIFO out), L6.2
(premise false; s91).

## Already explored (Phase 1)

Phase 1 is in `docs/perf/8k-explore-survey.md`. It lists 177 items in 12 areas that were already tried, built,
rejected or parked, each with its verdict and source doc, plus the profile-grounded baseline cost table. Each Phase 2
idea above names its closest explored item. The critics checked overlap against that list, and two of the ideas were
killed on prior decisions found there or in the plans: L3.4 by `docs/plans/append-cf-compaction.md` and L2.4 by
s82.
