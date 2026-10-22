# r1 contrarian lens: work a first-principles design would not do

Read-only search. Code refs are main `eb993e3f` (`git show eb993e3f:<path>`). Note: main moved to `2d2f3ed9`
(batch 10 merged: L6.1 + L2.1 + running hash defaults) during this session; `native_executor.rs` and `app.rs`
lines shifted there (+~1,450 in native_executor). `order_book.rs`, `order_book/cancel_batch.rs` and
`torus-mempool/src/rate_limit.rs` are unchanged between the two commits.

## Side observations found while checking premises (not proposed as ideas)

- **57-60% of native blocks never reach Phase 2.** `torus_exec_phase_margin_seconds_count` vs
  `torus_exec_engine_seconds_count` (val0): std CL21 r2 2,896 / 7,290; std R r1 2,929 / 7,251; deep CL21 r1
  3,080 / 7,773; deep R r1 2,231 / 6,016. Engine time is bimodal (57.5% of blocks < 4 ms, 15% > 64 ms, 1% in
  256-512 ms). Placements arrive in ~40% of blocks (~37k orders each, margin ~25.7 ms per call); the rest are
  ~24 cancel-alls + oracle. Every per-block mean in measured.md mixes these two populations. Liquidation step
  is 3.49 ms/blk mean (25.44 s / 7,290), median ~1.6 ms, i.e. it runs on all the fill-free blocks too.
- **Commit-persist ticket waits on E are 3-7x larger in CL21 than in R** (sum per ~303 s per node: std CL21
  2.5-5.3 s, deep CL21 5.3-7.5 s with 17-24 waits > 100 ms; std R 0.5-0.7 s, deep R 2.8-3.1 s;
  `torus_exec_commit_persist_wait_seconds`). E waits for the persist ticket at block start
  (`app.rs:6829-6840`) although rows only need to be durable before the checkpoint marker. This is T5.3
  territory (owner-excluded), so it is recorded here only.

---

## 1. batch-resting-only-fok-cow: batched FOK takers are a 50-100x block-time amplifier

**Premise (code).** Any `PlaceOrderBatch` item may be FOK / IOC / Market / Stop: only the length is checked
(`rate_limit.rs:435-446`, cap 1,024 `torus-types/src/lib.rs:762`), and the whole batch sorts as `GtcOrder`
(`native_executor.rs:14834`). In the batch path every FOK clones the market's full `AccountMargins` and
`ReduceOnlyPositions` before its pre-check walk (`order_book.rs:1753-1754`; `TraderMargins` = 2 x
`Option<{free, px}>` + bool, `:659-713`), and `can_fill_completely` `continue`s past the taker's own resting
orders without consuming them (`order_book.rs:3246`), so a rejected FOK changes nothing and can be repeated.
All of it runs in one market's serial pool job (`market_workers.rs:429-470`) that E waits on.

**Arithmetic.** ~180 B per trader in the two maps. Bench S ~78 per market (94 batches x ~250 of 300 markets).
Adversarial: 400 senders per block -> S ~400 -> ~100 KB memcpy per FOK; 200,000 FOKs per block
(`rate_limit.rs:250`, 64 actions/sender `:50`) -> ~20 GB/block, 2-4 s on one worker vs a 38 ms exec block.
Self-walk: 200k FOKs x 1,000 own resting asks (open-order base limit) = 2x10^8 visits, 0.4-0.8 s.
Rejected FOKs release their reservation and pay no fill fee.

**Mechanism.** (a) Byte-identical: run the pre-check on small copy-on-write overlays (only makers the walk
touches; read-through to the live maps) -> O(makers crossed) per FOK. (b) Owner call: batch items must be
GTC/ALO limits; takers go as single actions (<= 400/block, 64/sender) -> 500x fewer FOKs per block. (b) also
fixes an ordering asymmetry: a batched IOC/FOK/Market runs in the post call and can take this block's GTC
liquidity from lower-sorted senders, while the same order sent alone runs in the pre call and cannot.
Optional owner call (c): a FOK that crosses its own resting order is rejected at once.

**Gain.** Robustness: bounds the worst-case exec block under adversarial load from seconds to tens of ms.
Zero cost to the bench (bench sends only GTC: `tools/bench-throughput/src/main.rs:471,847,1449`).

**Nearest prior (grepped FOK, fill-or-kill, stop, metering, batch; memory search too).** No FOK entry.
`stop-order-scan-and-fills` (O(S) stop rescan per fill: different structure), `consensus-work-ceilings-r16`
(generic metering, proposed), `hl-in-block-order` (executor category order, parked), `o2-atomic-batch-variant`
(rejected). None restricts batch item types or touches the FOK clone or self-skip.

**Prototype (<= 3 h).** torus-core microbench: a book with 1,000 own asks + 100 maker asks, maps with
S in {100, 400, 2,000}; time 10k FOK buys priced to cross only own asks through
`place_order_with_accounts(.., makers=Some)`; split ns/FOK into clone vs walk; repeat with a CoW overlay stub.
Then a ubench_econ block with 195 x 1,024-FOK batches in one market: engine ms and match-scope wait vs baseline.

Effort: (a) 2-3 person-days with a differential test; (b) 1-2 days + P0.0 activation gating.

---

## 2. cancel-all-generation-bump: O(1) cancel-all with lazy reclamation

**Premise.** Phase 1 cancel-all is 2.28 ms/blk std / 2.55 deep on E (5.9-6.3% of the exec block), 24.16
cancel-alls per block, 196.7 books visited / 96.66 hit each (`summary.json .cancel_all`). Book removal is
99.33-99.39% of cancel-all time, measured as 4.33M orders over 7,329 calls, ~590 per call (memory 331c4086).
Per target, `cancel_all_many` (`cancel_batch.rs:311-370`) does an `order_index` get, a level get, an
`order_seq` get and a `partition_point` binary search with a HashMap probe per step. It then compacts the
queue, removes 5+ index entries, and returns cloned `Order`s so that E can sum the margin release
(`cancelled_orders_margin` `native_executor.rs:12531`, release `:12683`, parallel path `:12695-12778`).

**Mechanism.** Each book keeps `gen[trader]: u32`, plus a per-trader reserved-margin sum that is maintained
where orders rest, fill and cancel (inside the pool match jobs). Each order carries the generation it was
placed under. A cancel-all becomes: bump `gen`, drop the trader's `trader_orders` entry, release the sum.
That is O(1) per (trader, book), with no per-order visit.

Stale orders are reclaimed lazily:
- matching pops or skips stale orders at queue fronts;
- BBO, open counts, the cloid map, the RO index, by-id lookups and stops all filter by generation;
- one pool pass per window compacts stale orders before the L6.1/L3.1 encode, which writes only live orders.

Live FIFO order is unchanged, so fills, blobs, bbo rows and balances stay byte-identical to eager removal.

**Gain.** After the change E pays ~24 x 197 x ~50 ns ~= 0.24 ms/blk. That saves ~2.0-2.3 ms/blk (5.3-5.7%
of E), or about +4..+6% matched/s at E saturation (busy 0.88-0.97). Pool side: cancel-all waits were
3.6-4.4 ms/blk with 3.2-3.6 workers busy (s92), i.e. ~0.26-0.33 cores/node. The stale-skip and compaction
cost comes back out of that (one linear retain per level per window). Deep book: the cost per cancelled
order grew 31 -> 89 us with depth (`phase1-actions-depth-drift`); the bump does not depend on depth.

**Nearest prior.** `phase1-cancel-tombstones` and `lazy-delete-level-queues` (proposed: tombstone each
target order, still O(targets) visits at cancel time); compaction policies (`cancel-levels-lazy`,
`cancel-all-in-place-compaction`, two-ended: inconclusive/rejected); `cancel-all-cheaper-margin-math`
(rejected); P2-1 index (merged). The difference: no per-order work at cancel time at all, and the margin
release comes from a maintained sum instead of the cancelled `Order` list.

**Risks.** Every book reader must honour the generation. Stale memory builds up between reclamations (~8.5k
orders/blk if nothing is popped). The A5 identity must hold when tiers change (recompute the sums when a
margin config changes). The encode at window close must not walk stale orders on E. Differential-test burden.
Effort 8-12 person-days.

**Prototype (<= 3 h).** Microbench using `random_book` (`cancel_batch/many_tests.rs:174`), 300 books, ~100k
resting orders over ~2k traders:
1. Time `cancel_all_many` for 24 traders x ~97 books (ns per cancelled order) against a gen-bump stub.
2. Time matching on levels with 0/30/60% stale orders.
3. Time a 100-block churn loop with per-window `VecDeque::retain` compaction.

Go if bump + skip + compaction comes to less than 40% of today's removal time.

---

## 3. exec-heap-hugepage-retention: the hot heap ends up on 4 KB pages

**Premise (measured, `mem-samples.txt`).** AnonHugePages as a share of anonymous memory, at ~+25 s and then
during the load window:

| cell | node | ~+25 s | load window |
|---|---|---|---|
| std CL21 r2 | val0 | 796/1,092 MB (72%) | 570/2,610 (21%) |
| std CL21 r2 | val1 | 830/1,128 (73%) | 560/2,545 (21%) |
| deep CL21 r1 | val0 | 524/541 (96%) | 1,078/4,095 (26%), then 820/3,850 (21%) |

Host: THP `enabled=[always]`, `defrag=[madvise]`, `thp_fault_fallback` 577k, `compact_stall` 1,622.
jemalloc is compiled in with `MALLOC_CONF` unset (`crates/torus-node/src/allocator.rs`), so it never sets
MADV_HUGEPAGE. Its 4 KB purges split huge pages, and new faults fall back to 4 KB once memory fragments.
On E: IPC 1.18-1.23 and 60 page faults per 1k fills (0.153 ms/1k fills, 8u). E and the pool make random
accesses over 2.5-4.7 GB (R/Users rows, books and their HashMaps, overlay).

**Mechanism.** Env only:
- `MALLOC_CONF=thp:always,metadata_thp:always`: jemalloc madvises its extents, so under `defrag=madvise` they
  get fault-time compaction and khugepaged priority.
- Optional variant: jemalloc 5.3 HPA (`hpa:true`), which allocates and purges at hugepage granularity.
- Decay stays at its defaults.

First-principles view: an in-memory matching engine keeps its hot state on huge pages, so a DRAM miss does
not also pay a page walk.

**Gain (estimate, not measured).** Assume dTLB walks are 10-20% of E cycles at ~21% coverage, and that
coverage back above 70% removes about two thirds of them. Then E ms/1k fills drops 3-8% (2.63 -> about
2.42-2.55 std), which is +3..+8% matched/s while E is saturated. Fewer and larger faults would also shrink
the ~1.7 ms/blk fault share. The gain applies per node in production too.

**Nearest prior (grepped THP, huge page, madvise: 0 hits; memory search 0 relevant).**
`jemalloc-decay-tuning` (dirty_decay 1000/0 was rejected at -7%/-31%; that change purged more, the opposite
direction), `jemalloc-compiled-in`, `malloc-arena-max-probe`, `phase3-checkpoint-layer-shrink`. The maps list
THP as unexamined (measured.md O10, crypto-runtime 6.4).

**Risks.** Direct-compaction stalls on E could raise block p99. Partially used 2 MB pages inflate RSS
(validator class is 8 GB; deep peaks at 5.3-5.7 GB). khugepaged uses CPU on the shared host. HPA is
experimental. The gain may be small if E is bound by cache misses rather than TLB misses.
Effort 0.5-1 person-day (HPA variant 2).

**Prototype (<= 3 h).**
1. `perf stat` on a running cell's `torus-execution` TID for 60 s (dTLB-loads, dTLB-load-misses, AMD
   `ls_tablewalker`, cycles, instructions) to get the page-walk share at ~21% coverage.
2. Run 120 s 300m CL21 cells, `EXTRA_ENV MALLOC_CONF=thp:always,metadata_thp:always` against unset,
   3 cells per arm, interleaved.
3. Compare exec ms/1k fills, E on-CPU per fill, the AnonHugePages trajectory, RSS max, the `compact_stall`
   delta, matched/s and block p99.

Go if E ms/1k fills drops at least 3% with RSS up less than 10% and no p99 regression.
