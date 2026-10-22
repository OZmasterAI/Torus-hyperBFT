# System map: execution (native executor + order book)

Base: main `eb993e3f`. Files: `crates/torus-bridge/src/native_executor.rs` (NE, 15,682 lines),
`crates/torus-core/src/order_book.rs` (OB, 7,768 lines) plus the helpers they lean on: `market_workers.rs` (MW),
`exec_pool.rs` (EP), `liquidation_step.rs` (LS), `order_book/cancel_batch.rs` (CB), `order_book/trader_orders.rs`
(TO), `order_book/classic_encode.rs` (CE), `torus-core/src/position.rs` (POS), `torus-state/src/backend.rs` (BE),
`torus-consensus/src/app.rs` (APP). Costs cite the results doc `docs/perf/ozarchy-antispam-item6-pf1-2026-10-04.md`
as "sN", the survey `docs/perf/8k-explore-survey.md` as "survey", and memory ids as "mem". **est.** = my estimate,
not measured.

## 1. Architecture and data flow

### 1.1 Block-level order (exec thread E = `torus-execution`)
1. The EVM section runs first (APP ~5200-5253).
2. Native verify and the trust cache run next (`batch_verify_native_actions_cached_with_sort_hashes`, APP 5381-5420).
   Then the actions are sorted into two lists (APP 5585):
   * pre list: Cancellation, then NonGtcOrder (IOC/FOK/Market/StopMarket/ModifyOrder);
   * post list: GtcOrder (GTC/PostOnly/PlaceOrderBatch), Lockbox, Oracle, Gov, Staking, Other (NE 14807-14857);
   * within a category the key is (sender, content hash) (NE 14937-14965).
3. `begin_resident` (APP 5610) joins the previous block's `end_resident` worker. The context is then built from the
   resident holder (`new_with_modes`, NE 5995; `attach_resident_block`, APP 5770).
4. `begin_block_oracle` (APP 5788, NE 14391) fills the block mark table (`BlockMarks`, NE 2322).
5. **Two `execute_batch` calls back to back**, pre then post (APP 5806-5807). The names "pre/post-EVM" are
   historical: the EVM has already run. Each call runs Phases 1-4 with its own caches and its own flush.
6. Tail on E:
   * `drain_core_writer_entries` (APP 5851);
   * `run_liquidations` (APP 5889, LS 150-178, pass 402-540);
   * governance, delist, fees, epoch, EVM gas (APP 5912-5919).
7. `save_order_books[_deferred]` (APP 6056/6069; NE 7262/7586), then `end_resident_on_worker` (APP 6434, NE 4900).
   The overlay then goes to the flush worker W.

### 1.2 `execute_batch_phases` (NE 8281-9115)
* **Flatten** (8304-8363):
  * one `Vec<(Address, FlatAction<'_>)>` sized by a counting pre-pass, with PlaceOrderBatch expanded by reference
    (no clone);
  * an oversize or empty batch is skipped whole (a consensus rule, 8341);
  * `results` is pre-filled with n "pending" values (8366).
* **Phase 1, non-place actions, serial** (8375-8456):
  * Default `cancel_batch`: each maximal run of `CancelAllOrders` (places do not break it, any other action does) goes
    to `exec_cancel_all_run` (12576).
  * That function takes the run's markets out of the P2-1 trader->markets index (12601-12613).
  * It then goes either serial per market (`take_pending_stops` + `cancel_all_many`, 12635-12663) or parallel
    (12695). The parallel path lends the run's books as LPT jobs (cap 8) on the exec pool (12809-12945).
  * Other actions go through `execute` (7959) one by one.
  * `exec_cancel_order` probes every book twice, in HashMap order (12306-12328).
* **Listed-market reject** (8464-8496): one registry read per market. **Cloid pre-pass**, serial (8503-8547):
  `HashSet<CloidKey>` + the cross-market `CloidMarkets` index (NE 4037-4200).
* **Phase 2, margin, serial by default** (8554-8817):
  * `open_order_counts` walks every book against the senders (NE 10706, OB 58-100).
  * `phase2_reservation_basis` (NE 10919: 3-4 HashMaps) and `phase2_markets` (NE 11018: shape, best bid, cfg, mark,
    band per batch market).
  * `AccountReader` (8624) is the shared read view.
  * `prepare_one` (9133-9398) runs per order in flat order over one `SenderFold` (NE 2805-2860: a
    `HashMap<Address, SenderState>` plus a held-out `cur` entry, so a sender's adjacent orders skip the map).
  * `stitch_outcome` (9407-9450) assigns the global order id and pushes onto
    `market_batches: HashMap<MarketId, Vec<PreparedOrder>>`.
  * Then `sell_top_ups_fast` (8774) and `d2_pools` (8803).
  * `TORUS_PARALLEL_ENGINE>=2` (off) shards the prepare by sender with `std::thread::scope` spawns, not the pool
    (9468-9553). The stitch stays serial (8697-8720).
* **Phase 3, matching, pool** (8819-8937):
  * A book is moved out of `ctx.order_books` (`HashMap::remove`, 6548).
  * `match_setup` (11555-11647) installs `ReduceOnlyPositions` (every batch sender with `pre_pos`) and
    `AccountMargins` (checked takers' pools) and builds the `Vec<MatchRequest>`. It runs inside the pool jobs when
    there are >= 2 markets (8827-8897).
  * `MarketWorkerPool` packs markets LPT by request count into <= `TORUS_MATCH_WORKERS` jobs (MW 115-200, 350-381).
  * Each job matches its markets one after another (`match_market`, MW 429-470): `set_next_order_id` +
    `place_order_with_accounts_cloid_checked`, with **one `PlaceOrderParams` clone per order** (MW 445). Then
    `resolve_cloid_log`.
  * A worker panic means fatal + fail-stop (8906-8931).
* **Phase 4, settle** (8939-9115):
  * Serial epilogue per market (8951-8983): absorb the trader and cloid indexes, clear RO/AM, collect triggered stops.
  * Gate (8989-9004): >= 2 markets and >= 1024 fills (>= 64 with PE).
  * Parallel path (9846-10230): pass A is a pure `compute_market_settle_plan` (10237-10336) in LPT chunks on the
    pool (9930-9972), one `PositionCache` per market.
  * Pass B is serial, by market id (10047-10218): book return, `pos_runs` push (L2.2), taker release, PnL credits in
    fill order, `persist_trade` per fill, cum-volume, maker releases, all via `bal_cache`. Sequential fallback:
    9560-9757.
  * Flush, serial (9047-9097): positions (`flush_with_runs` / `flush_all`, sorted key order, POS 975-1016), balances
    (sorted, NE 407-425), then per trader `get_cum_volume` + `put_cum_volume` (9090-9096).
  * `run_triggered_stops` goes through the single-action `place_order_inner` (11795-11820).

### 1.3 Data layouts
* **Fixed point**: `FixedPoint(i128)`, SCALE 1e8 (`torus-types/src/lib.rs` 153-157).
  * `checked_mul` = i128 mul then `/ SCALE`, with an i256 fallback (193-202).
  * Operators panic on overflow (225-245).
  * `OrderId`/`Cloid` are u128, `MarketId` u64, `Address` 20 B.
* **OrderBook** (OB 1188-1313), RAM-resident per market:
  * `bids`/`asks: BTreeMap<FixedPoint, VecDeque<Order>>`. Best ask = `first_entry`, best bid = `last_entry`
    (OB 2726, 2771).
  * `order_index: HashMap<OrderId, OrderLocation{side, price}>`.
  * `trader_orders: HashMap<Address, TraderOrders>`. TO 1-80: a Vec that grows a position map with holes past 32 ids.
  * `pending_stops: Vec<StopOrder>`, unsorted.
  * `cloids {map: HashMap<CloidKey, OrderId>, log, resolved}` (OB 1369).
  * `reduce_only_index: BTreeSet<(Address, OrderId)>`.
  * Per-batch `reduce_only_positions` (HashMap, OB 427) and `account_margins` (HashMap<Address, TraderMargins>,
    OB 659-703).
  * `order_seq: HashMap<OrderId, u64>`, used by cancel-all `partition_point` (CB 352-361).
  * Journals `row_journal: BTreeSet<OrderId>` and `level_journal: BTreeSet<(u8, i128)>`, plus `row_exists`,
    `level_exists`, `level_epoch`, `level_chunks`, `dirty_chunks` (mode 2/3 only).
  * `new_traders`, `instance`, `log_drains`, `saved_bbo`.
* **Order** (OB 108-121) carries id, trader, side, price, remaining, original, `OrderType` (an enum with stop
  payloads), tif, timestamp, reduce_only and an `Option<u128>` cloid: **est.** ~176 B. **Fill** (OB 125-134):
  **est.** ~128 B.
* **Executor per call**:
  * `BalanceCache {HashMap<Address, CachedBalance>, dirty: Vec}` (NE 211-425); `load` returns a clone of the 2 x i128
    `NativeBalance`.
  * `PositionCache {HashMap<(Address, MarketId), Option<Position>>, dirty: HashSet, markets}` (POS 781-790).
  * `VolumeCache` (NE 219), `pools: HashMap<(Address, MarketId), FixedPoint>`, `basis`, `open_at_start`, `markets`.
  * All are alloy `HashMap` with foldhash (OB 15; P2-5).
* **Per process**: thread-local `MAKER_FREE` per pool worker, keyed by batch id (NE 2731-2770).
* **Overlay** (BE 501-505, 3038-3122): per CF, `writes: BTreeMap<Vec<u8>, Vec<u8>>` and `deletes:
  BTreeSet<Vec<u8>>`, under one `RwLock`.
  * A put: CF intern, write lock, journal record when checkpoints are open, `deletes.remove`,
    `writes.insert(key.to_vec(), v)`.
  * A get: read lock, then the BTreeMap, then resident rows R, then lower frozen layers, then RocksDB. It returns an
    owned `Vec`.
  * Typed rows (`Users`) skip the decode for clean traders.

### 1.4 Allocation patterns (hot)
* **Per order**:
  * the `PlaceOrderParams` clone into the book (MW 445);
  * a `PlaceResult` with 5 Vecs (empty ones do not allocate);
  * when the order has fills and RO positions are installed (nearly always in the batch path), a
    `BTreeSet<Address>` `touched` plus one `sweep_reduce_only` per touched trader (OB 1838-1851).
* **Per fill**:
  * `Fill` push;
  * `row_journal.insert` and `order_seq` get/remove (OB 3043-3058);
  * 2 `ro_positions.apply_fill` probes (OB 3019-3022);
  * the maker `account_fill` check (OB 861);
  * pass A: 2 `PositionCache` RMWs; pass B: 1-2 `bal_cache` load/set and a `TradeFill` push.
* **Per resting insert**: `order_index`, `order_seq`, `row_journal`, `level_journal`, `trader_orders`,
  `new_traders` and the cloid map/log (OB 3111-3144).
* **Per dirty row at the flush**: a Borsh encode into a fresh Vec (POS 374-382), `key.to_vec()` and a BTreeMap node.
* **Per call**: ~15 maps/Vecs built from empty (fold, caches, `market_batches`, `basis`, `markets`, `pools`,
  `open_at_start`, `claimed`, `listed`, plans, `deferred`).

## 2. Threads, pipeline and what is serial

| thread | runs | parallel? |
|---|---|---|
| E | 1.1 + 1.2 except pool jobs: Phase 1 (except the cancel-all book pass), listed + cloid pass, Phase 2 (default), Phase 3 epilogue, pass B, flush, triggered stops, liquidation, tail, `save_order_books`, and the L3.1 window-close encode of every owed book (APP 4305-4326, NE 4482-4501) | **serial** |
| `torus-exec-pool-<i>` (rayon; size = max(host, MATCH, SETTLE) = 32 on archy; EP 20-37) | match (+ setup), settle pass A, parallel cancel-all book pass, `par_recover` | E **blocks** in `pool().scope` until the last job ends; E takes no job (EP 43-60) |
| scoped spawns | PE prepare (NE 9489); `open_order_counts` above 2x100k probes (OB 82-99; never at bench shapes, NE 1484-1498) | per call |
| end_resident worker | rows-slot rebuild, joined by the next `begin_resident` | overlaps the next block's start |
| flush worker W | overlay -> RocksDB, checkpoint layer merge | off E |
| hotstuff-algo | consensus; blocked on the full exec channel 19-26% of wall (survey s90.3) | paced by E |

* **Serial invariants**:
  * ids are assigned in flat order (the stitch);
  * pass B applies credits and clamps in market / order / fill order (NE 9780-9786), and `trade_index` follows it;
  * flushes are sorted by key;
  * cancel-all results come back in run order (12664-12686);
  * the liquidation scan is round-robin over the sorted positions CF (LS 419-525).
* **Phase 3 cannot split one market**: a hot market's match is serial inside its job (MW 437-468; s124 hot-skew lens,
  mem eae79029).

## 3. Hot paths with cost

Standard = 300 markets, Classic, 3 validators on archy (32 threads); deep = 8i P1M (1M resting).

| path (code) | cost | source |
|---|---|---|
| Whole engine, standard | ~17k fills/blk, exec block 58-60 ms, 15 blk/s, E on-CPU 2.54 ms per 1k fills, E busy 0.88-0.95; pool 1.4-1.6 cores (5.5-6.2 ms per 1k) | s114.3/.4 arm A |
| Latest-binary hash-off ref R | 263-266k matched/s; std 22.2 blk/s; p50 11.3 / p99 472 ms; 23.4-23.8 CPU-s per 1M | s124.5 |
| Engine split, deep (ms per 1k fills) | 2.89 = phase1 0.196 + **margin 0.856 (30%)** + match 0.304 + settle 1.19 (pass A 0.17 / B 0.59) + flush 0.41 + tail 0.19 + untimed 0.15 | s105 |
| Phase 2 `prepare_one` + fold + top-ups (NE 9133, 11262) | largest serial E phase at deep; top-ups were 5.5-5.7% of E (10.8 ms/blk) before T | s105, mem cc05dbcb |
| Cache flush (NE 9047-9097) | std 6.8 ms/blk = 13-14% of E (0.405 per 1k); deep 31.4 ms/blk with L2.2, 26k dirty position rows (**est.** ~1.2 us/row) | survey, s98.3 |
| Settle pass B (NE 10047) | std 5.1 ms/blk (0.30 per 1k); deep 16-19 | s114.4, s98.3 |
| Settle pass A (pool wall) | std 3.0 ms/blk; deep 8.7-10.5. Pool work: ~1/3 plan, ~2/3 match | s114.4, s98.3, mem eae79029 |
| Match phase (pool wall) | std 5.8 ms/blk; deep 18.6 | survey s94, s98.3 |
| E idle in pool scopes | 25-29% of E wall = 16.9-18.9 ms/blk: setup 7.3-8.1, plan 4.1-4.6, cancel-all 3.6-4.4, `par_recover` 1.9-2.2. ~21% of match work is left after the median thread finishes | survey s90.3/s92.3, mem 04cdadb4 |
| Cancel-all (NE 12576, CB 311) | 30 ms/blk at 79 per block before P2-1/PC; 3/4 is real removal (locate 9.7, apply 10.3), 7.3 scan | s25.1 |
| Liquidation (LS 402) | 5.6-5.9 ms/blk = 7-8% of E (2,048 accounts per block); deep 3.9-10 | survey s90.4, s110.7 |
| Classic save / L3.1 close | std per-block save 3.6-5.9 ms; deep pre-L3.1 78.9 ms/blk (borsh 73%, memmove 15%). With L3.1 each close encodes all 300 books on E: deep mean 61-70 ms, p99 126-160, every ~6 s | survey s96, s110, s117 |
| Smaller E items | `put_cf_raw*` 6-7%, malloc/free 6%, borsh 6%, BTreeMap insert 3-4%, `open_order_counts` 1.4% (~1.1 ms); verify 3.7 ms/blk; replay guard 0.8 ms | survey s90.4, s94 |
| Pool items | Classic journal inserts 3.3% of pool CPU; RO sweep 2.7% of pool | mem cc05dbcb |
| Microbench (300 markets, 16.8k fills per call) | E wall 54 ms; on-CPU 47; match 5.2; settle 25.1 (pass B 4.4) | s119.3 Q0 off |

## 4. Knobs and current defaults

| knob | default | where |
|---|---|---|
| `TORUS_MATCH_WORKERS` | host parallelism (bench leaves it unset = 32) | MW 298-313 |
| `TORUS_SETTLE_WORKERS` | -> MATCH -> host; read per block | NE 592-596 |
| `TORUS_PARALLEL_SETTLE` / `_MIN_FILLS` | on / 1024 | NE 543-569 |
| `TORUS_PARALLEL_ENGINE` / `_MIN_ORDERS` / `_MIN_FILLS` | **off** (N>=2, cap 32) / 64 / 64 | NE 1105-1150 (s128) |
| `TORUS_PARALLEL_MATCH_SETUP` | on | NE 1161-1204 |
| `TORUS_SETTLE_DEFER_RESULTS` | on | NE 631, 904 |
| `TORUS_SETTLE_NO_MERGE` (L2.2) | on | NE 757-825 |
| `TORUS_FAST_SELL_TOP_UPS` | on | NE 1388-1440 |
| `TORUS_CANCEL_BATCH` | on | NE 5135-5143 |
| `TORUS_PARALLEL_CANCEL_ALL` / `_MIN_VISITS` | 8 / 64 | NE 5169-5230 |
| `TORUS_BOOK_ROWS` | Classic (0) | NE 3218-3263 |
| `TORUS_BOOK_FAST_ENCODE` (L3.3) | on | NE 645-707, CE 1-24 |
| `TORUS_BOOK_COALESCE` (L3.1) | requested on; active only with resident Classic books, pipelined blocks, a checkpoint interval and the hash off | `exec_pipeline.rs` 232-260, NE 5664 |
| `TORUS_RESIDENT_BOOKS` / `_ADVANCE_UNTOUCHED` | on / on | NE 3878-3907 |
| `TORUS_LEVEL_HASH_CACHE` (mode 2) / `TORUS_SAVE_BOOKS_WORKERS` / `_MIN_OPS` | 256 MB / host (cap 64) / 32 | NE 3309-3470 |
| Constants | `OPEN_COUNT_WORK_PER_THREAD` 100k (1498); `PLACE_ORDER_GAS` 1000 (112); `SELL_TOP_UP_BPS` 10 (461); `LIQ_SCAN_PER_BLOCK` 2048 / `LIQ_ACT_PER_BLOCK` 64 (`liquidation.rs` 58-60); TO `SMALL` 32 | |
| Not on main (owner Batch 10) | `TORUS_SETTLE_STREAMED` (L2.1), L6.1 window books, the running hash | s114, s120, s124 |

## 5. Known limits and failure modes

* **E is the bottleneck.** It is ~0.95 busy at saturation, and a few % more E work per fill halved blk/s (s91). The
  merge rule (s91, owner s128) rejects any change that adds E on-CPU per fill; it is why L2.1 and Phase 5 step 2 were
  not merged as defaults.
* **Bistable block size** (11-16 blk/s). E back-pressure paces consensus. Bigger blocks amortise the per-call fixed
  costs but raise p50.
* **Pool scheduling.** 3 nodes x 32 pool threads share 32 hardware threads. The last finisher is preempted 15-38% and
  E idles in `scope`. Pinning was a loss (-5.7%). MATCH/SETTLE caps on 32 threads were never A/B'd (mem 38e871cb).
  Exec pool fine-grain jobs (G) are parked (s78.6).
* **Per-market serialism** (LPT balances whole markets only, MW 350-381). A hot market is the floor of Phase 3 and of
  pass A.
* **Fail-stop** (`fatal_error`, the committer never applies the block):
  * a match panic, which consumes the book (8906-8931);
  * a parallel cancel-all panic (12426-12440, 12956);
  * a flush error (9084-9096);
  * a reader local fault (NE 5648);
  * a save fault (NE 4569).

  Recoverable: a pass-A panic or position fault falls back to sequential settle (9983-10024); a PE worker panic falls
  back to serial prepare (8682-8686).
* **Determinism contracts**: any worker count, PE thread count or settle cap must give byte-identical state. The pinned
  entry points exist for the differential tests (NE 8183-8267). New parallelism must keep the stitch order, the pass-B
  order and the sorted flushes.
* **Consensus-visible**: book mode, the batch-cap skip, listed/delisted rejects, the cloid rule, the action sort and
  the Classic blob layout (CE 16-17).
* **Deep book**: whole-book encodes scale with resting orders (+0.61 ms per 1k fills per 1M resting before L3.1, s96).
  Since L3.1 this is a ~60-160 ms close stall on E every ~6 s; s110 found it does not set p99.
* **Two calls per block**: the open-order walk, the maps, the cloid pass and three flushes are each paid twice per
  block.

## 6. Corners that look unexamined (observations only)

Checked against: survey sections 0-13, the 8k ideas table, and memory (s124 lenses `cc05dbcb`, `04cdadb4`, `eae79029`,
`3059a0d8`). Not checked against every s65 JSON idea.

1. **Stop triggering is a linear scan.**
   * After every placement with fills, `trigger_stops` -> `retain_stops` walks all of the book's `pending_stops`
     (OB 1941-1945, 3188-3197, 3341-3355), O(stops) per filled taker.
   * There is no price-ordered stop index. No doc or memory found that sizes the bench's stop share or this scan.
2. **The FOK pre-check clones whole per-batch maps.** Every FOK with a maker source clones the market's
   `AccountMargins` and `ReduceOnlyPositions` (OB 1753-1754), which hold every batch sender in that market: O(senders)
   per FOK. Nothing found sizing this.
3. **Fixed-point division on every fill.** `fill.price * fill.quantity` = i128 mul + `/ 1e8` (lib.rs 193-196). It runs
   per fill in pass B, in `add_fill_volumes` and in the match charge (NE 9644, 225; OB 3009), and in every
   reservation / IM calc. An i128 divide by a constant is probably a `__divti3` libcall on x86-64 (not verified by
   disassembly). s11 profiled only the older i256 path.
4. **Maker valuation is per worker.** `MAKER_FREE` is thread-local per pool worker (NE 2731-2770). A maker quoting in
   markets spread over k workers is valued up to k times per call (balance read + position-sums view). This
   duplication is not sized separately.
5. **Per-call fixed costs twice per block.** The pre and post lists run back to back (APP 5806-5807):
   * a trader active in both gets 2 balance puts, 2 cum-volume read+put and 2 position puts per block;
   * each call walks all books in `open_order_counts` (NE 8565) and rebuilds the Phase-2 maps.

   No doc found that splits engine cost per call. The pre list is takers + cancels, the post list makers + GTC
   crossings.
6. **Overlay write shape on the flush.**
   * Each dirty row costs a `RwLock` write acquire, an owned key Vec, a `BTreeMap<Vec<u8>>` insert (memcmp descent)
     and a `deletes.remove` probe (BE 3111-3122), plus a fresh value Vec (POS 374-382).
   * P2-2 batch write was a loss. Overlay structure alternatives are not in the survey, and the 8k rules put the
     torus-state overlay absorb off limits.
   * During Phases 2-4 every pool worker read takes the same `RwLock` read side (BE 3051), while the overlay is
     logically unmodified.
7. **`Order` is large and the queues store full records.**
   * `VecDeque<Order>` holds ~176 B (**est.**) per resting order, including `OrderType` with stop payloads that never
     rest, and `original_qty`.
   * The Classic encode walks every order of every owed book (CE 195-239): ~176 MB per close at P1M (**est.**). Memory
     notes the save as DRAM-bound (mem eae79029).
   * L2.3 (seq-inline queues) and L2.4 (dense slots, dead) are the nearest explored ideas.
8. **The L3.1 close encode is serial on E** (NE 4490-4499, APP 4324-4326).
   * Parallel Classic save was examined per block (B parked, +3.5% CPU, s78.6; s124 r1 KEEP, mem 3059a0d8), before
     L3.1 existed.
   * At window granularity (300 books every ~6 s, 61-160 ms on E) it has not been examined.
9. **Bench load shape.**
   * Standard is cancel-all heavy: 58-79 per block, but only 1.22 cancel-by-id per block (`docs/plans/item6-phase2-impl.md`
     row 15).
   * `exec_cancel_order` probes all books twice (NE 12306-12328; P2-1b dropped).
   * Modify, by-id cancel, FOK and stop-heavy HL-like flow is thin, so items 1-2 and the by-id probe are unmeasured
     under that flow.
10. **Phase 2 is the biggest serial E phase at deep** (0.86 ms per 1k fills, more than match + pass A + pass B
    together, s105).
    * The only parallel variant built is PE: sender shards on per-call thread spawns (NE 9489-9534), measured -1.4%
      matched/s and +2.4% CPU per fill (s128).
    * PE appears in the owner's exclusion list (WAN items), so this is noted only to place the serial cost.
11. **Already identified elsewhere, so not new** (for the idea search's dedupe):
    * Classic journal elision (`row_journal`/`level_journal` inserts are kept and then discarded in Classic,
      OB 3133-3135 / NE 7338-7340): 3.3% of pool CPU. Dropped because it conflicts with Phase 5 step 3's dirty-mark
      sites (mem cc05dbcb, 3059a0d8).
    * Reduce-only sweep skip when `reduce_only_index` is empty (OB 1838-1851): 2.7% of pool, not proposed
      (mem cc05dbcb).
    * Caller participation in pool scopes (rayon `in_place_scope`) plus per-market items: proposed in s124 r1
      (mem 04cdadb4); the G fine-grain jobs are parked.
    * Hot-market pass-A trader shards (mem eae79029).
