# r2 lens: deep-book-2 (deep stability beyond close-merge removal and the level rope)

Base: repo HEAD `2d2f3ed9` (batch 10 merged: L6.1 + L2.1 on). Read-only. All new numbers below come from existing
9j2 cells in `~/bench-results-matched/ozarchy-9j2-*` (logs, `sampler.csv`, `metrics-after-val0.txt`,
`mem-samples.txt`). Nothing was built or run. The analysis scripts are in
`/home/oz/projects/wt/novel-s80/scratch-r2db/` (`tmo.py`, `ck.py`, `reg.py`, `cad.py`, `bnd2.py`) and can be re-run on
any campaign.

## Finding that drives idea 1: every 9j2 consensus timeout is an epoch-boundary refusal

- **Mechanism in the code.** At every chain epoch boundary H (`epoch_length` = 100 in `devnet/genesis.json:7` and
  `testnet/genesis-weighted-base.json:7`), `TorusApp::epoch_plan` (`crates/torus-consensus/src/app.rs:9689-9719`)
  refuses H unless the DURABLE applied marker (`applied_height()` = `native_applied_height(state_db)`, `app.rs:3581-3587`)
  is at least H-100. The reason is that the plan row is read from the state DB
  (`crates/torus-economics/src/epoch_plan.rs:206-214`; the consensus-side `staking` is built on `state_db` at
  `app.rs:8203`).
  - The durable marker only advances when W finishes the %100 checkpoint write (deep: 181 MB batch, `db.write`
    0.45-1.5 s).
  - A refusal plays out like this:
    - the leader returns a datum-less `refused_proposal()` (`app.rs:10463-10469`);
    - followers answer `MissingData` (`app.rs:10297`), retried every 500 ms;
    - the view burns;
    - when the view is also the pacemaker epoch-change view (v % 100, same genesis field), a timeout only extends it
      and waits for a TC (`crates/hotstuff_rs/src/pacemaker/implementation.rs:128-151`), 3/3 at n = 3.
- **Log evidence.** The WARN `epoch boundary: execution has not applied the previous boundary yet ... height=1300
  applied=1100 needed=1200` was counted per cell:

  | arm | distinct refused boundary heights | timeouts (s124 table) |
  |---|---|---|
  | deep CL21 r1-r4 | 3, 3, 4, 3 = 13 | 12 |
  | deep M15 | 27 | 22 |
  | deep R | 16 | 14 |
  | std CL21 | 2 | 1 |
  | std M15 | 32 | 26 |
  | std R | 4 | 3 |

  - Every cell with 0 refusals has 0 timeouts (std CL21 r2, r4, w1).
  - No cell has more timeouts than refused heights.
  - The refused heights are all multiples of 100, with `applied = needed - 100`, so the H-100 checkpoint was still
    in flight.
- **Sampler evidence (1 Hz, val0).** Every timeout follows a checkpoint `db.write` of 0.75-1.29 s, with the commit-persist
  writer blocked for the same time, within 0-3 s. In that window:
  - the exec queue drains to 0 (so this is not exec back-pressure);
  - consensus completes 0-3 views in 1-2 s;
  - the timed-out view lasts 1.3-3.1 s (2.80, 2.28, 2.68, 3.15 s).
  - Equally long checkpoint writes at other view offsets (v % 100 = 10-99) leave about 70-80 views per 3 s and no
    timeout.
- **Cadence regime.** Per 15 s bins, every 9j2 cell runs a slow regime (15-18 blk/s) and switches to a fast one (24-27
  blk/s) only after its LAST refusal:

  | cell | refusals at (s into the load window) | fast regime from |
  |---|---|---|
  | deep r1-CL21 | 0, 45, 75 | 90 s |
  | deep r3-CL21 | 45, 60, 105, 120 | 165 s |
  | deep r4-CL21 | 0, 45, 90 | ~150 s |
  | std r3-CL21 | 30 | 60 s |
  | std r1-R | 15, 45 | ~135 s |

  - Deep R has 8 refusals spread over 0-270 s per cell and never leaves the slow regime. By sixths, deep r1-R runs
    13.7 / 17.9 / 16.2 / 15.3 / 18.9 / 17.8 blk/s, against std r1-R 14.3 / 18.3 / 23.8 / 24.4 / 25.7 / 25.7.
  - M15 deep also stays slow.
  - This is a candidate mechanism for the "unexplained" s124.5 deep R 15.95 vs CL21 21.49 blk/s, for the deep
    timeouts (0.46 vs 0.04 per 1k) and for part of the "early-window blocks 1.5x late".
  - The causal direction is not proven: slow regime, then bigger checkpoints and lag, then refusals, then backlog
    blocks, then slow regime is a loop. Idea 1's cells settle it.
- The 9f cells (s118 O 8.0 vs 14.4 blk/s) have 0 refusals, so not every cadence anomaly is this one.
- **Prior work.** Memory `865d2bfa`, `cb42abb1` and `7ec1ed70` (s124, 2026-10-15) saw the DB-marker >= H-L read.
  They judged it non-binding: "boundary H first validated 3.2-12 s after exec finished H-100", "consensus timeouts do
  not correlate with epoch stalls". That held at the older, slower cadence. At 22 blk/s with exec lag 40-66 blocks the
  margin is (100 - lag) / blk/s ≈ 1.5-3.5 s, and the deep checkpoint write (up to 1.5 s, behind W's queue) misses it.

---

## 1. epoch-plan-logical-ready: boundary readiness from E's logical watermark plus an in-memory plan hand-off

**Mechanism.**
- E already writes plan(H+L) when it executes boundary H (`epoch_plan.rs:440`).
- Also publish the decoded plan into a shared `Arc<Mutex<BTreeMap<u64, EpochRotationPlan>>>`, and share E's logical
  applied height. Today that height is `ExecutionContext.exec_applied`, `app.rs:1686-1689`, stored at `:4106`; it is not
  visible to `TorusApp`, so share it as an `Arc<AtomicU64>`.
- `epoch_plan` (`app.rs:9689-9719`) and `refresh_upcoming_evm` (`app.rs:11160-11185`) then use
  `max(durable, logical) >= H-L` and read the map first. After a restart the map is empty, so they fall back to
  today's DB path.
- Prune entries when H+L executes, as the DB row is pruned.
- The plan bytes and the validator-set updates are unchanged (they are a deterministic function of the executed
  state). Only the readiness timing changes, so this is node-local.
- Fallback variant (b), if the owner wants durability kept: the leader defers its proposal while not ready (the
  parent-missing defer path, `hotstuff/implementation.rs:833-848`) instead of burning the view with a datum-less block.

**Expected gain.**
- Timeouts: about -100% (deep CL21 0.46 -> ~0, std 0.04 -> 0, deep R 1.45 -> ~0, M15 1.18/1.30 -> ~0).
- Native blk/s:
  - Lower bound: +1-2%, from removing 2-8 stalls of 1.3-3.1 s per 300 s cell.
  - Upper bound, if the fast regime starts at warm-up end (~30 s): deep CL21 21.49 -> ~23.5-24 (+9..+12%; the r1 /
    r3 cells spent 75 / 150 s slow); deep R 15.95 -> 22-24 (+38..+50%), closing the R/CL21 gap; std CL21 +5..+9%.
- Block p50: -10..-20% where the regime flips (deep R 14.0 -> ~11-12 ms).
- RSS: deep R shows +0.3-0.5 GB local maxima at refusals (4,777 MB at the h=3300 refusal), which would go.
- matched/s: ~0 (load-bound).
- Scaling: today the refusal rate grows with checkpoint write time, i.e. with book depth. At 10M resting orders
  (~1.1 GB book blobs per window) every boundary would refuse. After the fix, consensus no longer depends on depth.

**Nearest prior.** Ledger greps: `epoch.?plan`, `boundary.*(refus|stall|not ready)`, `applied_height`, `durable applied`,
`EpochPlanNotReady`, `exec_applied`, `epoch.change`, `pacemaker epoch`, `TimeoutVote`, `vm.dirty`, `writeback`,
`rate.?limit`, `regime`.
- `epoch-plan-by-exec` (merged): this check is part of it.
- `epoch-snapshot-h-minus-d`, `validator-set-in-block`, `block-until-exec-catches-up` (all rejected): these are
  consensus-visible alternatives.
- `dead-leader-exclusion-b1` and `p00-governance-schedule-activation`: they ride the same plan and would inherit the
  fix.
- `ckpt-window-vm-dirty-bytes` (queued, owner-excluded) and `dirty-page-limit` (rejected, "write queue, not
  writeback"): this finding names the checkpoint-window consensus stall as a boundary refusal.
- `retention-blk-s-regime-hypothesis` and `code-layout-binary-effect` (inconclusive regime splits).
- Memory `865d2bfa`, `cb42abb1`, `7ec1ed70`: the epoch fast path; they saw the read and called it non-binding.

Nobody records the durable-marker coupling as the timeout cause. The change differs from the rejected options: no
earlier snapshot, no block field, no wait for exec of H-1.

**Risks.**
- Crash after a logical-ready vote: replay recomputes the same plan deterministically, and the hash fail-stop catches
  divergence. A restarted node behaves as today until its checkpoint lands.
- The fast regime then sits on the consensus gossip limiter (25-26 views/s; 567-796 drops per node per cell). The
  `consensus-ratelimit-per-class-buckets` fix (proposed) becomes the next lever.
- Smaller blocks can raise exec CPU per fill by a few %. That is a cadence effect, not a cost of the fix.
- The work overlaps the owner-excluded T5.3 p99 attribution scope (checkpoint-window stalls), so route it there if the
  owner prefers.
- s124's blocks/s and timeout rows for M15/CL21 and R/CL21 are confounded (deep M15 27 vs CL21 13 refusals) and need
  re-measuring.

**Effort.** 2-3 person-days, including a crash/replay test and the boundary test at `app.rs:15663` adapted.

**Prototype (<= 3 h).**
1. Arm A, no node build. Deep R env, genesis `.consensus.epoch_length = 1000` (one jq line after
   `gen-3val-genesis.sh`) vs 100, 2+2 ABBA cells (`TOOLS=best cell.sh`).
2. Arm B, the code above behind `TORUS_S80_PLAN_LOGICAL=1`, deep CL21 env, 2+2 ABBA cells.
3. Read:
   - refusal WARNs (expect 0);
   - `torus_consensus_timeout_total`;
   - blk/s per 15 s and the fast-regime onset (`reg.py`);
   - p50 / p99, matched/s, exec on-CPU per 1k fills, RSS max.
4. Kill if timeouts fall less than 80%. Keep only the timeout win if blk/s in the 30-150 s bins does not rise at
   least 10%.

---

## 2. cow-book-snapshot-window-save: copy-on-write level snapshots so the %100 full save and the close never encode or clone books on E

**Mechanism.**
- Store levels as `BTreeMap<FixedPoint, Arc<VecDeque<Order>>>` (`crates/torus-core/src/order_book.rs:1191-1193`).
  Every mutation goes through `Arc::make_mut`: ~87 level-access sites in `order_book.rs` and `order_book/*.rs`, mostly
  on pool threads.
- At the L6.1 %100 full save (`native_executor.rs:8009-8073`) and the L3.1 close encode (`encode_classic_books`,
  `:4791`), E calls `book.snapshot()`: header scalars, an Arc-bump clone of both level maps (O(levels)) and the stops.
  It puts a lazy book value for `CF_NATIVE_ORDER_BOOKS` into the block's `PendingState`.
- The lazy value rides freeze, the close merge (`checkpoint_layer.rs:216-243` now clones an Arc, not ~110 MB), C and W.
- W runs the unchanged `encode_classic` (`classic_encode.rs:195-237`) for the digest and writes it straight into the
  WriteBatch.
- Overlay readers of the book CF (`precompiles.rs`, `book_reader.rs`, `read_view.rs`) materialize it with the same
  encoder, so the bytes stay deterministic. A W-only sidecar would not work: precompile reads must stay in the layer
  chain.
- Old level versions stay pinned only until W drops the snapshot (1-2 blocks).

**Expected gain (deep).**
- E work per window: today ~64 ms of encode (save_books deep 1.22 vs std 0.57 ms/blk) plus ~45-52 ms of blob clone at
  the close (upkeep 1.24 vs 0.72 ms/blk). After the change, ~5-10 ms. That is -1.0..-1.1 ms/blk, 2.5-2.7% of the
  40.4 ms block.
- Exec on-CPU per fill: 2.780 -> ~2.70 (-3%), which closes about half of the +5.6% deep-vs-std gap.
- matched/s: +0.5..+0.8% at elasticity 0.3 (~0 if the shape is load-bound).
- E %100 spike p99: 126-212 -> ~20-30 ms. Upkeep p99: 30 -> ~17 ms.
- Transient RSS: -0.2..-0.33 GB per window.
- Std: -0.05..-0.1 ms/blk.
- At 10M resting, E stays at ~10 ms per window (today ~600 ms plus a 1.1 GB clone).
- W gets +60-65 ms per window (+1.4% busy), so this should ship after idea 1.

**Nearest prior.** Ledger greps: `cow`, `copy-on-write`, `persistent`, `snapshot`, `Arc<VecDeque`, `encode off`,
`save.*worker`, `parallel.*save`.
- r1 #12 `classic-cold-level-rope`: a byte cache with an invalidation fork risk, built on the parked step 3, and E
  still encodes hot levels.
- r1 #6 `close-merge-removal`: removes the clone, not the encode.
- `phase5-step3-classic-save-dirty-levels` (parked).
- `parallel-classic-book-save` B (parked; pool fork-join, +3.5-7.5% CPU).
- `book-save-pass2-deferred` (mode 2/3 `DeferredBookSave`, `native_executor.rs:8330-8395`).
- `layer-per-cf-rwlock-or-persistent-map` (applies to L only).

The difference: snapshots of the live order structures, so there is no byte cache, nothing to invalidate, and no
encode on E.

**Risks.**
- An extra Arc hop per level access, plus a `make_mut` CAS per mutation (est. < 0.6 ms of pool CPU per block).
- Every book-CF overlay reader must materialize. A missed reader means a fork.
- Pinned versions grow if W stalls.
- `PendingState`, `FrozenPending` and the layer code all need a lazy value type for one CF.

**Effort.** 6-9 person-days.

**Prototype (<= 3 h).** Use `crates/torus-core/benches/book_encode.rs` P1M (300 books, 1.6M resting).
1. Time `encode_classic` for all books.
2. Time the snapshot clone of Arc-level maps built from the same books.
3. After a snapshot, time the first-touch `make_mut` copies of 6 hot levels per side per book.
4. Time 1M pop / push through `make_mut` vs a plain `VecDeque`.
5. Kill if the snapshot takes more than 10 ms, the per-op cost rises more than 3%, or the first-touch copies take more
   than 5 ms per block.

---

## 3. cold-level-demotion: far, untouched levels leave the live book structures and live as their Classic bytes

**Mechanism.** Node-local, decided at a %100 save.
- A level qualifies when it is at least D ticks from its side's best price and has been untouched for at least W
  windows (Classic keeps `level_journal`, `order_book.rs:1176-1184`).
- Its `VecDeque<Order>` becomes `ColdLevel {bytes: Arc<[u8]>` (its exact Classic records, `classic_encode.rs:74-120`),
  `count, qty}`.
- Its orders leave `order_index`, `order_seq` and `trader_orders` (`order_book.rs:1194-1232`).
- A per-book sorted cold index answers lookups: `(OrderId, level, offset)` at 24 B per order, plus per-trader cold
  counts. It serves by-id lookups, `open_order_counts` and cancel-all membership.
- Any touch rehydrates the level first: price reaching it, a by-id cancel or modify, cancel-all, the RO sweep, the FOK
  walk, or bbo / band reads.
- Encoding a cold level is a memcpy.

**Expected gain.**
- Deep heap at end of cell: jemalloc allocated 1,970-2,045 MB vs std 1,021-1,126 MB, for +0.98M resting orders and
  +0.9M accounts. That is ≈ 0.7-0.85 KB per resting order including slack (the Users share is unknown).
- Demotion brings that to ~0.12 KB, i.e. -0.55..-0.65 GB per 1M cold orders: deep RSS mean 4.37 -> ~3.8 GB, max 5.68
  -> ~5.0 GB.
- Exec: the unattributed non-save deep delta (+0.77 ms/blk) is an upper bound on the hot-path gain from fewer diluted
  map cache lines.
- Boot book load: 3.1 -> ~1 s at 1M.
- Std: 0.

**Nearest prior.** Ledger greps: `cold`, `evict`, `demot`, `spill`, `page out`, `lazy load`.
- r1 #19 `resting-order-compaction` (shrinks the record only, -105 MB).
- `level-queue-16byte-slab` (proposed).
- r1 #12 rope (caches bytes but keeps every order resident).
- `block-store-unbounded` (a different RSS source).

**Risks.**
- Fork class: every consensus-relevant query must answer the same whether a level is hot or cold. That covers
  `bbo_row`, `would_cross`, `band_reference`, the FOK walk, the RO index, the cloid duplicate check, open-order counts,
  cancel-all result order and modify.
- Rehydration spikes when price moves fast. Stops, cloids and reduce-only orders pin a level hot.
- Low confidence.

**Effort.** 8-12 person-days.

**Prototype (<= 3 h).** A torus-core microbench on the P1M builder.
1. Measure jemalloc `stats.allocated` per resting order.
2. Run a 200k hot-op mix (`order_index` / `order_seq` / `trader_orders` probes, queue pop / push) with and without 1M
   cold entries in the same maps.
3. Time demote and rehydrate of a 3.3k-order level.
4. Kill if the resident cost is under 400 B per order, or if hot ops differ less than 5% and the saving is under
   0.3 GB per 1M orders.
