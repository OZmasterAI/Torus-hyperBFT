# Implementation Plan: item 6 Phase 2 (per-block work in proportion to fills)

Status: step 0 done on `perf/item6-phase2` (ozarchy, s27 / s101) from main `d3ba3c0a` (9.7;
review log rows 1-16); base moved to main `e934fa0e` before step 1 (9.11): 0.1, 0.2 and 0.4 built, 0.3 and Gate 0 measured (results doc section 25).
Step 0 checkpoint: P2-1 + P2-2 below 13 ms per native block; owner (s27 / s101): P2-5 joins the
gate set, the +7% stays (9.9), P2-3 goes to the backlog (9.10). Step 1: the 9.8 test-only feature
and P2-1 (C1) built (ozarchy s30, review log rows 18-19); its gate cell is open. C1 review gaps
closed and the index size gauges added (ozarchy s31, rows 20-21); 18c s104 decisions in 9.12. P2-1 gate missed
(row 24), accepted by 18c s104 (row 25, 9.14); main `12979d4b` merged, row 23 lows done (row 26); P2-1b C dropped (row 27, 9.15); P2-5 built (row 29), gate met (row 30); P2-2 built (row 32), gate regressed and reverted (rows 34-35); cumulative 1.0495x vs `e934fa0e`. Next: the P2-4 design check, then
P2-2, the P2-4 design check (section 4). Owner decisions recorded s96 (section 9, with the options
not chosen) and s27 / s101 (9.8-9.10). Written 2026-10-06 (after s94) from the Phase 2 step 0 profile
(ozarchy results doc `docs/perf/ozarchy-antispam-item6-pf1-2026-10-04.md` section 22, on
`integrate/s94-batch`; "section 22" below). Short form and the phases after this one:
`item6-phases-2-5-plans.md`. Design: `market-scaling-in-memory-design.md` section 3, Phase 2.
Phase 1 verdict and the carried-over gates: `item6-phase1-impl.md` 9.12 and 9.13.

Code references are `file:function` with line numbers at `origin/integrate/s94-batch`
(`a0eda77`). The step 0 profile ran crab = main `59fa407`; the hot files named here
(`cancel_batch.rs`, `market_workers.rs`, `position.rs`) are the same on both, and
`native_executor.rs` differs only in places this plan does not touch. The Phase 2 base is
now main `e934fa0e` (9.11; was `d3ba3c0a`, 9.7, and `35e69b3`, 9.6). Main has moved since `a0eda77` (liquidation
telemetry, the ADL budget, exact cost basis, item 7 step 0, read-precompile gas, the C2
holder-index fix and the Position v2 savings), so line
numbers in `liquidation_step.rs`, `app.rs`, `position.rs` and `native_executor.rs` may have
moved.

Every number below is from section 22 or from the code, unless it is marked
**(estimate)**.

## 1. Goal and gates

Goal: cut the per-block work on the execution thread that does not scale with fills: the
cancel-all scan of every book, the per-row cache flush and SipHash on the exec-path maps (a new
thread set per block was in the goal until 9.10 moved P2-3 to the backlog).
No consensus rule, no state format and no state hash changes (same as Phase 1).

| gate | target (fail line) | measured by | source |
|---|---|---|---|
| Phase gate, 300 markets | matched/s >= +7% vs the Phase 2 base (main `e934fa0e`, 9.11) | interleaved cells, section 6 | short plan, Phase 2; owner s96 (9.1) |
| Phase gate, 10 markets | no regression vs the base beyond the ~5% cell resolution | interleaved cells | short plan; Phase 1 plan 1.1 |
| Gate 2 holds | >= 0.9x main `92a02ed`, 300 and 10 markets (today 1.097x / 0.997x) | one reference pair per campaign | Phase 1 plan 9.12 |
| Gate 3 (carried) | margin <= 1.5 ms/1k (3.0), match <= 2.0 ms/1k (4.0) | `ubench_econ`, walk 0 and walk 10, reported | Phase 1 plan 9.12 |
| Gate 4 (carried) | tail <= 1.0 ms/1k (2.0); today 1.97 static, 2.98 moving (past fail) | `ubench_econ`, walk 0 and walk 10, reported | Phase 1 plan 9.12 |
| correctness | differential vs Phase 1 (CF dumps + `h_n` per block, all four BookModes, serial and pipelined), goldens A/B unchanged, P1 / P3 rerun | tests | short plan, common rules |

Gates 3 and 4 are reported every campaign but do not decide Phase 2: the owner put margin
and tail in Phases 4-5 (9.12). Only P2-4 (section 3) works on the tail; if P2-4 is built,
its gate is the tail with moving prices below its fail line (2.0 ms/1k).

Expected gain (estimate): P2-1 + P2-2 take about 10-28 ms per native block off the
execution thread (section 3). Engine time is 190 ms per native block on crab r2. If the
execution thread stays the bottleneck, matched/s rises about in proportion, so +5-15%.
The +7% lower bound needs about 13-17 ms per block of real saving. The pool (P2-3) is not
in that number: its cost is sys time and latency, which `cycles:u` does not rank.
Rule of thumb used in section 9: about +0.4-0.55% matched/s per ms saved per native block.

Checkpoint after step 0 (owner s96, 9.1): if steps 0.2 / 0.3 put P2-1 + P2-2 below 13 ms per
native block, the owner decides between pulling P2-5 (hasher) into the gate set and
accepting a lower gate (as Gates 3 / 4 were accepted missed in 9.12).

**Step 0 result (results doc section 25, 2026-10-08): below 13 ms.** P2-1 ~5.0 + P2-2 ~4.6 =
~9.6 ms per native block in the perf window (37.6k fills per block, close to section 22's 39.2k);
~3.7 + ~3.2 = ~6.9 ms on the standard no-perf cells (27k fills, engine 124 ms per native block).
Both are estimates from the 0.3 split; as a share of engine time 4.7-5.6%, against the ~6.8% that
13 ms of 190 stood for. **Owner (9.9): P2-5 joins the gate set, the +7% stays.** The set is then
P2-1 + P2-2 + P2-5: ~3.7 + ~3.2 + ~10.3 = **~17 ms per native block (~14% of engine) on the
standard cells** (~5.0 + ~4.6 + ~15.8 = ~25 ms in the perf window, at section 22's engine for
P2-5). P2-3 (~0.8 ms) is no longer in the set (9.10). P2-5's ~10 ms is a microbench estimate and
counts only once a cell confirms it (9.9). Note: the base `d3ba3c0a` is 6.3% below `35e69b3` on
the standard shape (results doc section 26, interleaved; not the 4 MiB book SSTs, not the build
style; bisect of the merges in between is a separate owner job). Bisected (results doc
section 27) to `a746c408` and `9e695364`; both fixed on main (section 29: C2 holder index,
Position v2 savings, together 1.018x matched/s vs `bf2edda6`). Since 9.11 the +7% is measured
against `e934fa0e`, which includes those fixes, so they do not count toward Phase 2.

## 2. What the step 0 profile measured (section 22.1, crab r2, load window)

ms per native block (39.2k fills per native block). Standard shape, section 6.

| item | crab ms/blk | ms/1k fills | Phase 2? |
|---|---|---|---|
| settle phase | 71.8 | 1.83 | no (includes the cache flush row below) |
| margin phase | 37.6 | 0.96 | no (Phases 4-5, O1 / O2) |
| match phase | 33.5 | 0.86 | no; the pool changes its spawn cost only |
| phase 1 (actions) | 31.8 | 0.81 | |
| - of which cancel-all | 26.7 | 0.68 | **P2-1** |
| - of which the book scan | 18.2 | 0.46 | P2-1 |
| save books | 23.9 | 0.61 | the pool changes its spawn cost only |
| - of which `diff_stop_rows` | 0.06 | | stops dirty flag: **dropped** |
| cache flush on the execution thread | 15.9 | 0.41 | **P2-2** |
| flush worker (own thread) | 74.4 | 1.90 | no (Phase 3); row 77 |
| verify | 10.1 | 0.26 | no |
| end_resident wait | 7.1 | 0.18 | no |
| sums re-value (walk 0) | 6.5 | 0.17 | P2-4 (24.2 at walk 10) |
| liquidation sweep (walk 0, section 22.2) | 10.7 | | P2-4 (24.7 at walk 10) |
| thread spawns | >= 15,651 per minute, ~62 per native block | | **P2-3** |
| engine / chain | 190 / 246 | | |

The rows are not additive. The cache flush is timed inside the settle phase
(`ExecPhaseAccum`, `native_executor.rs:3669-3708`: `cache_flush_ns` is nested in
`settle_ns`), and cancel-all and the book scan are inside phase 1.

Section 22.2 (crab load windows, walk 0) shows margin 45.3 and matching 99 ms per native
block, against 37.6 and 33.5 in 22.1, with the same engine total (190). The two tables
seem to use different splits; this plan uses 22.1 and asks ozarchy which rows 22.2
covers before step 0 closes.

## 3. Work items, ranked by measured ms per native block

### P2-1 Cancel-all: visit only the books where the sender has something (26.7 ms, scan 18.2)

- Where: `native_executor.rs:exec_cancel_all_run` (:8947) and
  `cancel_orders_and_stops` (:8879, used by single cancel-alls and the liquidation step);
  `cancel_batch.rs:cancel_all_many` / `plan_cancel_all_many` (:311, hot lines :338-359);
  `order_book.rs:take_pending_stops` (:1854).
- What the code does today: every bench cancel is `CancelAllOrders { market_id: None }`
  (`tools/bench-throughput/src/main.rs:724, 758, 809`), 37-39% of actions, about 54 per
  native block (139 actions per block, section 21.4). For a run of cancel-alls,
  `exec_cancel_all_run` lists all 300 books and, per book: filters the whole run for
  members, calls `take_pending_stops` per member (a linear `any` over the book's stops),
  then `cancel_all_many` (one `trader_orders.get(sender)` per member, then a
  `partition_point` per cancelled order). The result loop then walks all 300 markets per
  action. Cost scales with books x run length, not with orders cancelled.
- Change: a node-local index in the resident holder, trader -> set of `MarketId` where
  the trader has resting orders or pending stops. Cancel-all (run and single) and the
  liquidation step's cancel visit only those markets, in ascending `MarketId` order. The
  index may be a superset: a market with nothing left for the trader costs one probe and
  changes nothing, and is pruned on that visit. It is added to wherever an order rests or
  a stop is stored (placement, the rest of a matched order, stop insert), never removed
  eagerly, and rebuilt from the books at load. Not hashed, not persisted.
- Not changed: `partition_point` per cancelled order (proportional to work done); result
  strings, result order, dirty marks, the margin release sum.
- Expected saving (estimate): 5-18 ms per native block. The ceiling depends on how many
  of the 300 books a bench sender rests in, which section 22 did not count; step 0.2 adds
  that counter and step 0.3 splits the 18.2 ms scan into probes vs `partition_point`.
- **Step 0 estimate (results doc 25.1): ~5.0 ms per native block in the perf window, ~3.7 ms
  on the standard no-perf cells.** Cancel-all is 30.0 ms per native block in the s-prof load
  window (79.4 cancel-alls per block); split by line, 22.7 ms is work per cancelled order
  (`apply_cancel_all_many` removal 10.3, locating targets with `order_index` / `order_seq` gets
  and `partition_point` 9.7, margin and bookkeeping 2.7) and only 7.3 ms is scan per book
  (`trader_orders.get` probe 3.5, per-book result store / drop 2.0, the results loop over all
  markets 0.9, plan setup 0.5, members filter and per-book lookups 0.3, `take_pending_stops`
  0.05). Each cancel-all visits ~300 books and the sender has orders or stops in 87-95 of them
  (29-31%), so the index removes ~70% of the scan. Section 22's 18.2 ms "scan" was
  `cancel_all_many` self time with the plan and apply code inlined, i.e. mostly work. Index
  upkeep is not in the estimate: keep it per (trader, market), not per order.
- Risk: consensus-relevant only through results and state, which must be identical. A
  missed index entry (a market where the trader rests but the index does not list it)
  would leave orders on the book: that is the one failure mode, so every insert path is
  covered by a test. The release sum is `FixedPoint` `checked_add` (panics on overflow);
  keep ascending `MarketId` order as `cancel_orders_and_stops` does today.
  `exec_cancel_all_run` currently iterates `ctx.order_books.keys()` (a `HashMap`, :8956),
  although its comment says it follows `exec_cancel_all`'s (sorted) order; values agree
  today (set inserts, non-negative sums), and the new code uses sorted order.
- Tests (first, they fail before the change where noted):
  - existing `cancel_batch_exec_tests.rs` (`cancel_batch_flag_on_matches_off_*`,
    `cancel_all_of_stop_only_senders_persists_stop_removal`) and
    `order_book/cancel_batch/many_tests.rs` stay green;
  - differential: random block sequences (places, partial fills, stops, cancel-alls with
    `None` and `Some(m)`, repeated senders in one run, liquidation cancels) with the index
    vs a `#[cfg(test)]` full-scan reference: identical results, CF dumps and `h_n`, all
    four BookModes, serial and pipelined;
  - index invariant after every block: for every book and trader with orders or stops,
    the market is in the trader's set (superset check);
  - restart: index rebuilt at load equals the index carried across blocks;
  - counter test: a cancel-all visits only the sender's markets (fails today: 300).
- Gate P2-1: phase 1 ms per native block down by at least half the step 0.3 estimate;
  correctness tests green.
- P2-1b (**dropped from Phase 2, 9.15**), CancelOrder / ModifyOrder by id (`exec_cancel_order` :8814,
  `exec_modify_order` :9045) still loop over all books to find an order id (the cancel
  probes every book twice). Not measured: the standard shape sends no single cancels.
  Owner s96: option C now, B later as its own consensus item (9.2). Step 0.2 adds a
  cancel-by-id cell.
  - Change (C): a node-local map `OrderId -> MarketId` in the resident holder, next to
    P2-1's trader index. An entry is added wherever an order rests (placement, the rest of
    a matched order) and removed when the order leaves the book (fill, cancel, cancel-all,
    liquidation cancel), rebuilt from the books at load. Not hashed, not persisted.
    `exec_cancel_order` and `exec_modify_order` look up the market and touch only that
    book. No entry, or the book no longer has the order: the same `order {id} not found`
    result as today's full scan, and a stale entry is dropped.
  - Expected saving (estimate): ~10-20 us per cancel or modify by id; adds one map
    insert and one remove per resting order (~0.05-0.1 us each).
    Rechecked (row 27): ~10-30 us per by-id action; upkeep ~0.6-1.4 ms per native block
    (~21k rests + ~21k removals at 14-34 ns). **Dropped from Phase 2 (18c s106, 9.15).**
  - Risk: a missing entry for a resting order would turn a valid cancel into
    "not found": the one failure mode, so every insert and remove path is tested.
  - Tests (first): differential vs a `#[cfg(test)]` full-scan reference (random places,
    partial fills, cancels, modifies, cancel-alls, liquidation cancels; identical results,
    CF dumps and `h_n`); invariant after every block: every resting order has an entry
    naming its book; restart: the rebuilt map equals the carried one; ownership checks
    (CONS-FIND-30, ModifyOrder s83 fix) unchanged.
  - Gate P2-1b: the cancel-by-id cell's phase 1 ms per cancel down; standard shape not
    worse.

### P2-2 Cache flush on the execution thread (15.9 ms; flush worker 74.4 ms is Phase 3)

- Where: `position.rs:PositionCache::flush_all` (:654-669; perf: `put_position` 8.7 ms,
  `get` 4.2, sort 2.8, lines :659-662), `native_executor.rs:BalanceCache::flush_all`
  (:325), call site `execute_batch_phases` (:6327-6341); overlay write
  `backend.rs:NativeStateOverlay::put_cf_raw_owned` (:2013).
- What the code does today: collects the dirty keys, sorts them, then per key does a
  `HashMap` get and one `put_position`. Each put serialises into a presized buffer and
  moves it (`position.rs:271-279`), then takes the overlay's `RwLock` write lock, interns
  the CF name, records the key, removes it from `deletes` and inserts `key.to_vec()` into
  the CF's `BTreeMap`. R copies the bytes from the block's delta on purpose
  (`resident_rows.rs:ResidentRows::apply`, s90 measurement). So the design doc's "serialise
  once, same bytes to the overlay and to R" is already in place; what remains is the
  per-row overlay insert.
- Change: one batch write per CF: `put_cf_raw_owned_many(cf, sorted rows)` (and the same
  for deletes) that takes the lock and interns the CF once and does the same `record`,
  `deletes.remove` and insert per key in the same order. `PositionCache` keeps dirty keys
  so the sort and the second lookup go (e.g. collect `(key, &row)` pairs once, then sort).
  Same for `BalanceCache`.
- Expected saving (estimate): 5-10 ms per native block of the 15.9.
- **Step 0 estimate (results doc 25.1, s-prof, `flush_all` 17.6 ms per native block): ~4.6 ms in
  the perf window, ~3.2 ms on the standard cells** (cache flush 12.1-12.6 ms there). Split:
  `put_position` 9.7 (overlay `BTreeMap` insert of the owned key, `backend.rs:2056`, 7.2;
  `intern_cf` 0.8; serialise + buffer 1.2), the second lookup `map.get` (`position.rs:684`) 3.5,
  the key sort 3.05, balance cache 0.5. The batch API removes the second lookup, `intern_cf` and
  the per-row lock; the per-row `BTreeMap` insert stays unless the CF map is built in bulk from
  the sorted rows (not in this plan; would need the CF map to be empty or small at the flush).
- Risk: the overlay's pending set, its journal (`record`, used by writer-precompile
  checkpoints, T4.4) and the write order must be identical, because the flush and the
  running state hash read them. A partial-failure path must keep today's
  all-dirty-on-error rule (`BalanceCache::flush_all` comment, :335-337).
- Tests: overlay property test (random puts / deletes / checkpoints / reverts) for the
  batch API vs per-row calls: identical pending set, journal and reads; app differential
  (CF dumps + `h_n`); `position_cache_exec_tests.rs` green; error injection on one row
  leaves every entry dirty.
- Gate P2-2: `cache_flush_ns` per native block down by at least 5 ms (estimate-based;
  log a miss).
- Not in Phase 2: the flush worker (74.4 ms per native block on its own thread: RocksDB
  write, crc32c, `trade_rows::encode_block`, memtable insert) and row 77 (first block
  after the load, 59 / 80 / 62 ms wall on val0 / val1 / val2 with 1.75 ms of main-thread
  CPU). The handoff to W is a rendezvous of depth one (`exec_pipeline.rs` module docs), so
  that block waits for the last loaded block's flush job. Phase 3 (coalesced checkpoints)
  is the lever. Phase 2 re-measures row 77 only.

### P2-3 Persistent worker pool (>= 15.6k thread spawns per minute, ~62 per native block)

**Dropped from Phase 2 to the backlog (owner, 9.10).** Kept below as the record.

**Re-sized 2026-10-11 (review log row 49, results doc section 44):** main `4e7fa146` spawns 41 threads per native block
(exact count), all on the execution thread (match, settle, end-resident; save books no longer spawns here). A pool would
save ~0.05-0.15 CPU-s/1M and ~0.9-1.4 ms per native block.


- Where (spawn sites per native block in the code; section 22 names the first three):
  - `market_workers.rs:MarketWorkerPool::match_parallel_capped_with` (:104, scope :158);
  - `native_executor.rs:settle_market_results_parallel` (:6966, scope :7055);
  - `native_executor.rs:drain_books_parallel` (:2541, scope :2562; save books);
  - `native_executor.rs:phase2_parallel_prepare` (:6664, scope :6677; crab margin shards);
  - `order_book.rs:open_order_counts` (:41, scope :65; margin phase, when the walk is big);
  - `native_executor.rs:end_resident_on_worker` (:3209, one named thread per block, :3229);
  - on the flush worker, `backend.rs:flush_pending_after_batch` (:1554, digest spawn
    :1592).
- Change (owner s96: option A, 9.3): one dedicated, named exec pool built once (rayon is already in the workspace:
  `torus-types`, `torus-consensus`, `torus-rpc`, `torus-node`; `rayon::ThreadPool::scope`
  lets jobs borrow block data like `thread::scope` does). Not the global rayon pool:
  ingress on the global pool starved consensus work before (s352,
  `torus-rpc/src/torus.rs:521-529`). Size: the largest of today's caps
  (`TORUS_MATCH_WORKERS`, `TORUS_SETTLE_WORKERS`, `TORUS_SAVE_BOOKS_WORKERS`, host
  parallelism); each site keeps its own cap and its own chunking (`assign_chunks`,
  `chunk_indices`: deterministic LPT, scheduling only). `end_resident_on_worker` gets one
  long-lived thread with a channel instead of a thread per block. The W digest spawn
  moves only if step 0.2 shows it matters.
- Expected saving (estimate): spawns go from ~62 to ~0 per native block after start-up.
  The ms saving is not known: section 22 measured sys CPU 2 -> 9 ms per 1k fills with
  perf on, and `cycles:u` cannot rank spawn cost. Step 0.2 measures sys time and spawn
  latency first.
- **Step 0 estimate (results doc section 25): ~0.8 ms per native block of exec-thread wall,
  +0.5-1% matched/s.** Counted: 47.5-52.4 spawns per native block, 15.8-18.0k per minute on
  the standard cells, from three sites only (match, settle, save books, ~15-19 threads each)
  plus one end-resident thread; `margin_prepare`, `open_orders`, `flush_digest`,
  `root_buckets` and `load_books` spawn nothing on this shape. Spawn + join cost on ozarchy
  (C microbench, 2 MiB stacks, idle host): 16.7 us per thread in a scope of 16, i.e. ~0.27 ms
  per scope, ~0.8 ms per block; more under load (not measured). Process sys CPU is 2.14-2.32
  ms per 1k fills (74-85 ms per native block, all threads); the spawns are ~1% of it, so the
  gate's "sys CPU per 1k fills down" cannot be read at cell resolution: read the spawn counter
  (~0 per block after warm-up) and matched/s (not down) instead.
- Risk: results must not depend on the pool. Today results are scattered back by index
  or sorted by `MarketId`, so the pool changes scheduling only. Each site keeps its panic
  rule exactly: match contains a worker panic as `MarketWorkerPanic` (T1.5); settle pass
  A falls back to the sequential settle; `phase2_parallel_prepare` catches the panic per
  shard; `drain_books_parallel` re-raises it (`resume_unwind`); a failed end-resident job
  drops R (the next native block rebuilds it). No job of block N may run after N's
  fail-stop: a scope returns only after all its jobs end, and the end-resident handle is
  joined at the next access as today. Pool threads must not be shared with ingress or
  gossip verify.
- Tests: existing `parallel_matching_tests.rs`, `parallel_settle_tests.rs`,
  `save_books_parallel_tests.rs`, `engine_parallel_tests.rs` and the T1.5 tests green;
  panic injection at each site gives today's outcome (contained / fallback / fail-stop);
  a counter test: N native blocks after warm-up spawn 0 threads (fails today); app
  differential (CF dumps + `h_n`) with pool sizes 1, 2 and host.
- Gate P2-3: thread ids per minute about constant (pool size plus fixed threads);
  sys CPU per 1k fills down; matched/s not down.

### P2-4 Liquidation sweep sums with moving marks (10.7 ms walk 0, 24.7 ms walk 10)

- Where: `liquidation_step.rs:run_liquidations_with` (:31) -> `liquidation_pass` ->
  `liq_view` (:207) -> `AccountReader::pos_sums` (`native_executor.rs:1428`) ->
  `cached_sums` (:1578) -> `build_sums` (:898). Up to `LIQ_SCAN_PER_BLOCK` = 2,048
  traders per block (`torus-core/src/liquidation.rs:38`).
- What the code does today: the sums cache is keyed by the mark version
  (`BlockMarks.version`). The version is kept when the mark table and margin configs are
  equal to the previous block's (`native_executor.rs:10033-10036`), so with fixed marks
  the sweep hits the cache. When marks move, every scanned trader is rebuilt with
  `build_sums`. Section 22.3: oracle-only blocks in the walk-10 drain window mean 14.4 ms,
  of which 14.5 of 16.5 ms main-thread CPU is this path; blocks with no native action
  mean 6.0 ms, `run_liquidations` 5.25 of 6.35 ms.
- Change: none yet (owner s96: option A, 9.4). Step 4 is a read-only design check of two options:
  (a) skip a trader whose health is provably unchanged under the mark move (a bound from
  the cached sums and the largest mark change); the skip must produce the same writes
  (`clear_cooldown`, `set_pending`) and results as the full path;
  (b) re-value only the markets whose mark moved (helps only when few marks move; on the
  bench all 300 move each oracle round).
  Build only if the check finds an exact rule and an estimated cut of >= 5 ms per block at
  walk 10. Otherwise it stays with Phases 4-5 (9.12: tail by one record per trader and
  sums inline per fill).
- Risk: high. Liquidation decisions are consensus. Any skip rule needs an equivalence
  proof and a shadow test against the full build.
- Tests (if built): shadow mode (`#[cfg(test)]`, as C4's `liq_view` shadow) on random
  mark walks and near-maintenance accounts: same `Health` and the same writes per trader;
  P1 / P3; `liquidation_tests.rs` green.
- Gate P2-4: tail with walk 10 below 2.0 ms/1k (`ubench_econ`); empty block with the feed
  live still <= 20 ms.

### P2-5 Keyed fast hasher for exec-path maps (fallback item, owner s96: option C, 9.5)

**In the gate set since step 0 (owner, 9.9); built right after P2-1.** A cell must confirm the
microbench estimate before it counts (s82 caveat, 9.9).


- Where: `Address`- and `OrderId`-keyed `HashMap`s / `HashSet`s on the execution path
  (section 22: SipHash `write` 6.6%, `hash_one<Address>` 3.5%, `hash_one<u128>` 3.1% of
  execution self time, ~13% together; Keccak 5.5% is the state hash and is not touched).
- Change: `foldhash` with a random seed per process (already in `Cargo.lock` 0.1.5 / 0.2.0
  through `hashbrown`; add it as a direct dependency) behind one type alias, swapped in map
  by map, hottest first.
- Expected saving (estimate, the least certain in this plan): 5-20 ms per native block,
  +2-7% matched/s. A microbench of the hot maps comes first and sets the estimate.
- **Step 0 microbench (results doc 25.3, 10 processes x 30 reps, spread < 1% except order-id
  churn):** `hash_one` Address 0.607x, order id 0.131x; map gets 0.29x (order id) to 0.57x
  (`(Address, MarketId)`), order-id churn 0.41x (0.30-0.47), Address set insert 0.96x.
  Estimate 15.8 ms per native block at section 22's 190 ms engine, ~10.3 ms at the standard
  cells' 124 ms (13.2% hash share x engine x 0.631).
- Role: the fallback at the step 0 checkpoint (section 1). If the checkpoint does not need
  it, it is built after steps 1-3 and measured on its own.
- Risk: std's `RandomState` is already seeded per process, so a map whose iteration order
  reaches results or state is a bug today (one known case, `exec_cancel_all_run` :8956, is
  fixed by P2-1). foldhash adds no new risk of that kind. Flooding: keyed, so user-chosen
  addresses cannot be ground into one bucket.
- Tests: audit that no exec-path map iteration reaches results, state or the state hash;
  app differential (CF dumps + `h_n`) with two different seeds.
- Gate P2-5: the microbench's ms per native block mostly realised in a cell; no
  correctness change.

### Dropped: stops dirty flag in `diff_stop_rows`

`diff_stop_rows` (`native_executor.rs:5221`) costs ~0.06 ms per native block (section
22.1). Save books is now the book drain itself (23.4 ms). Not built.

## 4. Steps and order

Order (owner s27 / s101, 9.9 and 9.10; 1b dropped, 9.15): step 0, then P2-1, P2-5, P2-2, the P2-4
design check. P2-1 is the largest measured item; P2-5 joins the gate set at the step 0
checkpoint and goes right after it; P2-3 is in the backlog (9.10). Was (s96, section 22.4):
P2-1, P2-3, P2-2, P2-4, with P2-5 as the fallback. P2-2 is small, touches different code and can
be built in parallel by a second builder. Each step: tests first, then the change,
then the gate. A failing correctness test stops the work. A missed cost gate is written
into the review log (section 8) and the next step starts (Phase 1 rule).

Who measures: 18c builds (code, tests, sanity ubench); ozarchy gives the gate verdicts
(Phase 1 plan section 3). Own worktree `wt/item6-phase2`, own `CARGO_TARGET_DIR`. Tests per
`TESTING.md`: nextest per crate while iterating, full workspace nextest + doc tests before
each commit, one full `cargo test --workspace` before the merge.

### Step 0: guardrails (no engine change)

- 0.1 Branch `perf/item6-phase2` from main `d3ba3c0a` (9.7). Record the suite counts.
- 0.2 Node-local counters and harness columns (not hashed): per cancel-all, books
  visited and books where the sender had orders or stops; thread spawns per site per
  block; process sys CPU per 1k fills and per native block (from `/proc/<pid>/stat`);
  per-block exec timing in the node for oracle-only blocks (section 20.2 asked for it);
  a cancel-by-id bench cell (a share of single CancelOrder / ModifyOrder) for P2-1b (9.2);
  a microbench of the hot `Address` / `OrderId` maps with SipHash vs foldhash for P2-5.
- 0.3 `perf annotate` of `cancel_all_many` and `exec_cancel_all_run` on the standard
  shape: split the 18.2 ms scan into `trader_orders.get` probes, `take_pending_stops`,
  the members filter and `partition_point`. This sets the P2-1 estimate.
- 0.4 Test-only reference paths (`#[cfg(test)]`, like Phase 1's `test_no_resident_rows`):
  full-scan cancel-all, per-row flush, scoped spawns. No runtime flag (D16).
- Gate 0: counters in the harness, one standard-shape cell on the base with the new
  columns, estimates for P2-1 and P2-3 written into this plan.

### Steps 1-4

| step | commit | gate |
|---|---|---|
| 1 | P2-1 cancel-all index (starts with the test-only feature, 9.8) | P2-1 above |
| ~~1b~~ | ~~P2-1b order id -> market map (option C, 9.2)~~ dropped (9.15) | - |
| 2 | P2-5 hasher (9.9) | P2-5 above; the cell must confirm the microbench |
| 3 | P2-2 batch flush | P2-2 above |
| 4 | P2-4 design check (read-only); build only if it passes | P2-4 above |
| 5 | phase campaign (section 6) | section 1 |

P2-3 (exec pool) is not a step: backlog (9.10).

## 5. Commit plan

C0 counters + reference paths | C1 cancel-all index | C1b order id -> market map |
C2 hasher | C3 batch flush | (C4 liquidation skip, if step 4 passes). The exec pool commit (was
C2) is dropped (9.10); the hasher was C5. Every commit: full suite and goldens green,
per-item ms in the commit message. One commit per item so a single item can be benched
from an intermediate commit if its effect must be isolated.

## 6. Bench shape

Standard shape (results doc 21.4, as section 22):
- bench `9b32d897` or later with the same flags; `MAX_IN_FLIGHT=4`,
  `OPEN_ORDER_BUDGET=900`; 300 markets uniform, cap 400, rate 76,000, `RETRY_BUSY=1`,
  120 s; native trie maintenance off (default since `db6c9de`);
- oracle feed 30000 / 2000 ms on both arms (both are crab now), walk 0; plus walk 10
  (`ORACLE_WALK_BP=10`) cells, and `ORACLE_FEED_DRAIN=1` drain cells for rows 77-78;
- 10-market cells with the same settings;
- arms: Phase 2 branch vs the base, main `e934fa0e` (9.11), interleaved, a 60 s warm cell first, >= 4 cells per arm; one main `92a02ed` pair for
  Gate 2; perf only in separate cells (perf costs ~4.4% on both arms);
- every heavy cell under the `signal_generate` trace, each its own systemd unit through
  `detach.sh` (results doc Open, section 19);
- driver: `~/bench-results-matched/ozarchy-p2s0-campaign.sh` with the step 0.2 columns.
  Bench campaigns go to the `bench-runner` agent.
- Report per cell: matched/s, ms per native block and per 1k fills for every row in
  section 2, thread spawns per minute, sys CPU per 1k fills, rows 77 / 78.

## 7. Out of scope

- Settle (71.8 ms), margin (37.6 ms; per fill 1.26x main) and match (33.5 ms): Phases 4-5
  and optional O1 / O2 (Phase 1 plan section 5).
- The flush worker (74.4 ms) and row 77: Phase 3 (coalesced state checkpoints).
- Phases 3-5 in full (`item6-phases-2-5-plans.md`).
- P2-1b: option C dropped (9.15); option B (market in the order id) later, as its own consensus item (9.2).
- Backlog items in Phase 1 plan 9.13 (maker over-commit, governance errors, etc.).
- Before mainnet: a background prune of the P2-1 cancel-all index with fixed work per block
  (check K traders per block in turn, drop the markets where they hold nothing), node-local
  (9.12 (a)). Until then the index only shrinks at a cancel-all or a restart; the size gauges
  `torus_exec_cancel_all_index_entries` / `_traders` show its growth.

## 8. Review log (filled during the build)

| # | step | finding | decision |
|---|---|---|---|
| 1 | 0.1 | Base suites on `d3ba3c0a` (ozarchy, 2026-10-08, own target dir): nextest `--workspace` 3059 passed / 0 failed (35 skipped), doc tests 1 / 0 | branch `perf/item6-phase2` cut from `d3ba3c0a` (9.7) |
| 2 | 0.2 | Node counters (`5ea374e3`), node-local, never hashed: per cancel-all books visited / books where the sender had orders or stops (`torus_exec_cancel_all{,_books_visited,_books_hit}_total`, user runs, single cancel-alls and the liquidation step's cancels); `CancelOrder` / `ModifyOrder` executed and books probed (`torus_exec_by_id_{actions,books_probed}_total`); per-block exec wall of oracle-only blocks (`torus_exec_oracle_only_block_seconds`, results doc 20.2). Counted on the exec context (`ExecPhaseAccum`), added once per block | harness columns `phase_by_node.<val>.{cancel_all,by_id,oracle_only_blocks}` (`1a6573dc`) |
| 3 | 0.2 | Thread spawns per site: `torus_state::spawn_count` (one relaxed add per spawn batch at 9 sites: match, settle, save_books, margin_prepare, open_orders, end_resident, flush_digest, root_buckets, load_books), exported as gauges `torus_exec_thread_spawns_<site>` (process totals, set after every block); a test at each site | harness `thread_spawns` per site per native block and total per minute |
| 4 | 0.2 | Sys CPU: collected by the harness, not the node (the CPU columns are harness-side already: `cpu.csv`, `schedstat.json`, the s94 campaign's `cpu-sampler.sh`). `run-cell.sh` records utime / stime from `/proc/<pid>/stat` with the schedstat snapshots (`procstat.raw`); `summary.json` `proc_cpu_by_node` gives user / sys ms per native block and per 1k fills (load window and whole run) | harness |
| 5 | 0.2 | Cancel-by-id cell (`e644bba0`): `--cancel-by-id-fraction` / `--modify-fraction` (harness `CANCEL_BY_ID_FRACTION` / `MODIFY_FRACTION`). The bench cannot know order ids otherwise (global counter, no cancel by client id), so it reads its own ids with `torus_getOpenOrders(sender, market)` on the in-flight watch node (val2), one market per lookup, each id used once; a modify moves the price one tick away from the mid | the cell measures `phase1_actions_ms` and `by_id.books_probed_per_action`; lookups add a per-market row scan on val2's RPC |
| 6 | 0.2 | `ubench_hasher` sanity run on ozarchy (release, not a gate number): `hash_one` Address 0.625x, order id 0.135x; Address map get 0.52x, `(Address, MarketId)` map 0.56x, order-id map 0.29x, order-id churn 0.33x, Address set insert 0.96x (foldhash / SipHash ns per op). Estimate 15.6 ms per native block (13.2% of exec self time x 190 ms x (1 - 0.38)) | estimate for the step 0 checkpoint; bench-runner reruns it |
| 7 | 0.4 | Full-scan cancel-all reference: frozen copy in `cancel_batch_exec_tests.rs`; `#[cfg(test)]` switch `test_cancel_all_full_scan` routes runs, single cancel-alls and the liquidation step's cancels to it, one action at a time. Differential `cancel_all_matches_the_full_scan_reference` (4 book modes, stops, partial fills, repeated senders): results, gas, dirty marks, books, stops, CF dumps, state root equal | P2-1 keeps it green |
| 8 | 0.4 | Per-row flush reference: `PositionCache::flush_all_per_row` (`#[cfg(test)]`) with an overlay differential (pending delta, reads, checkpoint revert; a mutation that drops deletes fails it). The balance cache already has a frozen per-row reference (`balance_cache_tests.rs` `OldBalanceCache`, exact write sequence) | P2-2 keeps both green; the overlay batch-API property test comes with the API (step 3) |
| 9 | 0.4 | Scoped-spawns reference: today's code is the reference. A `#[cfg(test)]` switch with one arm would be dead code, so step 2 adds it with the pool (pool vs `thread::scope` per site). Step 0 adds the per-site spawn counts; the serial-vs-parallel differentials (`engine_parallel_tests`, `save_books_parallel_tests`, `parallel_settle_tests`, `parallel_matching_tests`, `load_books_parallel_tests`) stay the cross-check | for review |
| 10 | 0.4 | `#[cfg(test)]` is per crate: the bridge and core switches cannot be set from torus-consensus tests, so the app-level differential (CF dumps + `h_n`, serial and pipelined) cannot flip them. Options for step 1: a test-only cargo feature on torus-bridge enabled from torus-consensus dev-dependencies (like `save-timings`), or the differential at the bridge level only | decided: option A, test-only cargo feature (9.8) |
| 11 | 0.1-0.4 | Suites on `1a6573dc` (ozarchy): nextest 3073 / 0 (36 skipped), doc 1 / 0, `cargo test --workspace` 3074 / 0 (43 ignored), clippy 270 = base, fmt 3349 = base (0 new), matched-bench 167 passed; goldens A/B unchanged (in the suites) | `docs/perf/pre-merge-suites-ozarchy.md` |
| 12 | Gate 0 | Campaign `ozarchy-p2s0b` (results doc section 25, 2026-10-08): step 0 `707f132f` vs base `d3ba3c0a`, same bench, standard shape, 8 cells all rc 0 / AGREE / PASS, no deaths. Overhead (ABBA, 2 cells per arm): matched/s 1.001x, engine ms/1k 0.991x, sys CPU / 1k 0.986x. Max 657 open fds per validator (unit soft limit 65,536): `LimitNOFILE` not changed | counters cost nothing measurable; later arms carry them. Gate 0 met |
| 13 | 0.3 | Cancel-all split (s-prof, inline-expanded): 30.0 ms per native block = 22.7 work per cancelled order + 7.3 scan per book; sender present in 87-95 of 300 books | P2-1 estimate ~5.0 ms (perf window) / ~3.7 ms (standard cells), not 5-18 (section 3) |
| 14 | 0.2 / 0.3 | Cache flush split: second lookup 3.5, `intern_cf` 0.8, sort 3.05, `BTreeMap` insert 7.2 of 17.6 ms; spawns 47.5-52.4 per block from 3 sites, ~17 us each (C microbench); sys CPU 2.2 ms / 1k, spawns ~1% of it | P2-2 ~4.6 / ~3.2 ms; P2-3 ~0.8 ms, gate on the spawn counter + matched/s (not sys CPU). Checkpoint: P2-1 + P2-2 < 13 ms, owner decides P2-5 vs lower gate (section 1) |
| 15 | 0.2 | Cancel-by-id cell: 1.22 by-id actions per native block (16,445 of 17,361 id lookups found no own order in the market), 457 books probed per action, phase 1 +3.1 ms per block (~2.5 ms per action) on all 3 validators; the probes are ~10-20 us of it | P2-1b's map alone cuts only the probes; the rest is likely run splitting (inferred), which P2-1 cuts. Read P2-1b's gate with a perf cell and a larger by-id share |
| 16 | 0.2 | Hasher microbench, 10 x 30 reps: `hash_one` 0.369x mean, map gets 0.29-0.57x; estimate 15.8 ms (190 ms engine) / ~10.3 ms (124 ms) per native block | P2-5 input for the checkpoint |
| 17 | checkpoint | Owner (s27 / s101) on the step 0 checkpoint: P2-5 into the gate set, +7% kept, P2-5 right after P2-1, confirmed by a cell before it counts (s82 caveat); P2-3 to the backlog | 9.9, 9.10; order P2-1 (+1b), P2-5, P2-2, P2-4 design check (section 4); commits C2 hasher, C3 batch flush (section 5) |
| 18 | 1 (9.8) | Test-only cargo feature `test-reference-paths` on torus-bridge (-> `torus-core/test-reference-paths`), enabled only from torus-consensus `[dev-dependencies]` (torus-bridge's own tests get the core half through its dev-dependency on torus-core). It compiles the switches `NativeExecContext::test_cancel_all_full_scan` (the frozen full-scan cancel-all, moved from `cancel_batch_exec_tests.rs` to `reference_paths.rs`) and `test_flush_per_row` (`PositionCache::flush_all_per_row`) into non-test builds; both default to off, so the feature changes nothing by itself (Cargo unifies it into every crate of a `--workspace` test build). Scoped spawns: nothing to switch (row 9; P2-3 in the backlog). The balance cache flush is still per-row (its frozen reference stays `balance_cache_tests.rs`); P2-2 adds that switch with the batch API. Not reachable from a node build: `cargo tree -e features,normal,build -p torus-node -i torus-bridge` and `cargo tree --workspace -e features,no-dev -i torus-bridge` list no `test-reference-paths`; `cargo check -p torus-node --release --message-format=json` builds torus-bridge / torus-core / torus-consensus with `features = []`. App switch `ExecutionContext::test_reference_paths` (`#[cfg(test)]`). App-level differential `reference_paths_match_production_{classic_reload,classic_resident,order_rows,level_authority,level_authority_chunked}`: 130 blocks (`p2_blocks`: C5's mark walk and shocks, maker requotes, resting / crossing limits, stop-markets and stop-limits near the touch, cancel-alls `None` / `Some(m)` with repeated senders in a run, cancel-only blocks, V4 rests then is liquidated), serial and pipelined, reference vs production: per-block write sets (`h_n`), running hash and full CF dump equal; the reference keeps no cancel-all counters (switch reached the context). Sensitivity: the reference without its dirty mark fails it (write set of height 31), after cancel-only blocks were added (before, every book was dirty through the maker requotes). Suites: nextest 3090 / 0 (34 skipped, 0 flaky), doc 1 / 0, clippy 268 = 268, fmt 3352 = 3352 vs `16cb2633` (rustfmt on the new test block only) | harness for C1 (P2-1) |
| 19 | 1 (P2-1) | C1: node-local cancel-all index `TraderMarkets` (trader -> ascending `Vec<MarketId>`) in the context, carried in `ResidentInner`; `None` after a load, built from the books at the first cancel-all (`cancel_index`, scan of `OrderBook::traders_present`: orders, stops, reduce-only entries, since `cancel_all` drops those too). Feed: each book keeps an in-RAM log `new_traders` (pushed when a trader's `trader_orders` entry was absent or empty, and per stored stop; never serialized), drained into the index by `TraderMarkets::absorb`. Insert paths found: core `order_book.rs` `push_trader_order` (from `insert_order`: the rest of a placed / partly filled order in `place_order_with_accounts`, the `modify_order` re-insert, the classic `deserialize_reader`; from `insert_loaded_order`: the `book_reader.rs` row / level loaders), stops in `place_order_with_accounts`, `restore_stop_row`, `deserialize_reader`; feed sites in `native_executor.rs`: `place_order_inner` (single-action placements: `run_triggered_stops`, liquidation `stage1`), the post-match loop of `execute_batch_phases` (every book the market workers hand back), `exec_modify_order`; loads (`new_with_mode`: classic / rows / levels loaders, stale-guard rebuild) drop the logs and leave the index to the scan; crash replay and restart run the same paths. ADL / backstop touch no book. Readers: `cancel_orders_and_stops` (single cancel-all, liquidation step) and `exec_cancel_all_run` visit only the listed markets, ascending (run: (market, action) pairs sorted, `cancel_all_many` per market over the members in run order; was the book `HashMap`'s order), and take them out of the index (the sender has nothing left there). Unchanged: `partition_point` per cancelled order, result strings, result order, dirty marks, the `checked_add` release sum. `debug_assert_index_fed` (end of `execute_batch_phases`, `stash_resident`): with an index, no book log is left over. Counter semantics: `cancel_all_books_visited` now counts index visits (bench column drops from ~300 to the sender's books). Tests: counter tests `cancel_all_visits_only_the_senders_books` (300 books: 4 visited, failed before: 300) and `cancel_all_counters_count_visited_and_hit_books` (3, failed before: 13); `cancel_all_index_matches_the_full_scan_reference_random` (12 books, triggering stop-limits, partial fills, modifies, cancels by id, repeated senders; 4 modes x batched / per-action; superset invariant after every block in `run_with`); `every_insert_path_feeds_the_index` (each path the sender's only presence, index built first); `cancel_all_index_rebuilt_at_load_equals_the_carried_one` (4 modes: reload index == exact, carried covers it with stale entries, live part == rebuilt, a final all-cancel block identical on both); app: row 18's differential now reference vs index, plus the superset check after every block (resident modes) and a replica restarted every 7 blocks (same `h_n`, hash, dump). Existing `cancel_batch_flag_on_matches_off_*`, `cancel_all_of_stop_only_senders_persists_stop_removal`, `many_tests.rs` green. Planted bugs (not committed): (a) no log push for a stored stop: 4 bridge tests (random differential, insert paths, restart, and the existing `mode2_combo_matrix_byte_identical_with_midrun_restart`) and 4 of 5 app modes fail (classic reload rebuilds the index every block, so a lost log cannot show there); (b) no feed at the batch-matching site: the debug assertion fails 39 bridge tests and all 5 app modes; with the assertion silenced, 7 bridge tests (incl. the step 0 and s63 differentials) and 4 app modes; (c) no feed at the single-action site, assertion silenced: `every_insert_path_feeds_the_index` only (in the app sequence every triggered stop was indexed by its stop already, liquidation orders never rest). Suites: nextest 3094 / 0 (34 skipped, 0 flaky), doc 1 / 0, clippy 268 = 268, fmt 3352 = 3352 vs `16cb2633` (rustfmt on own hunks only) | gate cell (P2-1 gate: phase 1 ms per native block) to ozarchy's bench-runner; open: stale entries of traders who never cancel-all stay until a restart (bounded by traders x markets) |
| 20 | 1 (P2-1) | Adversarial review of C1 (GPT-6.1-sol, high): C1 approved, two coverage gaps, both closed (tests only, no production change). (1) Warm index through a crash replay or a staleness-guard rebuild: `p2_run` takes a `P2Fault`. `CrashAfter([30, 63, 87])` (pipelined): W's write of k+1 fails after E ran k+2 on top of it (fail-stop, nothing of k+1 / k+2 durable, E's holder already carries their books and a warm index); a new context that keeps that holder replays k+1 and k+2 (`replay_committed`), the guard drops books and index (holder ahead of the DB) and rebuilds. `StaleHolder([(20, 26), (71, 77), (104, 109)])` (serial and pipelined): the warm holder is taken out after a, blocks a+1..=b run without it, it is put back, block b+1 meets a holder stamped a over the DB at b (holder behind the DB), the guard trips on height and marker and rebuilds. Tests `warm_index_crash_replay_matches_reference_{classic,order_rows,level_authority,level_authority_chunked}` (~5.5 s each) and `warm_index_stale_guard_rebuild_matches_reference_*` (same 4 modes, ~9.5 s each): vs the uninterrupted reference paths, every height through the hashed flush once, per-block write sets, running hash and full CF dump equal; the superset check after every block and after each replay; every fault met a warm index; `torus_exec_resident_rebuilds` = 1 + one per crash / 1 + two per stale window. (2) Reduce-only: `index_blocks(seed, resting, ro)` and `p2_blocks(ro)` take a reduce-only switch whose extra draws happen only when it is on (existing seeds unchanged). Bridge: a prefix opens a position of 3 per sender in its home book and rests two reduce-only orders (the sweep cuts the second), a block of modifies leaves each sender a leftover (each modify is clamped to the position alone, so the pair exceeds it: no sweep cuts it until the sender places or fills there again); then a third of the places become home-book actions (reduce-only resting / crossing to close / increasing side / stop-limits / modifies, plain crossings of 1-6). `cancel_all_index_matches_the_full_scan_reference_reduce_only` (4 modes x batched / per-action, ~4.2 s): ~95 reduce-only places accepted and ~55 rejected per run, up to 63 resting, positions under them reduced / closed / flipped (1-8 times each per run), 24-30 leftovers, all equal to the full scan with the superset check after every block. App: `reference_paths_match_production_reduce_only_{classic_reload,classic_resident,order_rows,level_authority,level_authority_chunked}` (~13 s each; 101 reduce-only legs, single and batch, up to 4 resting, 10 positions under them moved). Planted bugs (not committed): (P1) a tripped guard keeps the holder's index with the reloaded books: the 8 `warm_index_*` tests fail (superset check after 27 / 32; with the check silenced, the write set of height 28 / 31) and nothing else in torus-bridge / torus-consensus (1025 run); (P3) a resting reduce-only order does not feed the log: the bridge reduce-only test and the 4 resident app reduce-only modes fail (superset; silenced: the differentials), nothing else in torus-core / torus-bridge / torus-consensus (1370 run; classic reload rebuilds the index every block, as row 19); (P4) `traders_present` without the reduce-only entries: nothing fails (1370 run): a reduce-only entry always has its resting order (every removal path drops the entry; F6), so the chain is belt and braces. Suites: nextest --workspace 3108 / 0 (34 skipped, 0 flaky), doc 1 / 0, clippy 268 = 268, fmt 3352 = 3352 vs `16cb2633` (rustfmt on own hunks only) | coverage gaps closed |
| 21 | 1 (P2-1, 9.12) | Index size gauges (18c s104): `TraderMarkets` keeps its (trader, market) entry count (`insert` +1 on a new entry, `take` / `remove` minus what leaves; the fill path still removes nothing), `NativeExecContext::cancel_index_size()` = (entries, traders), (0, 0) while not built, O(1). `app.rs` sets the node-local gauges `torus_exec_cancel_all_index_entries` / `torus_exec_cancel_all_index_traders` once per native block where the step 0 counters are added (never hashed, no state). Bench: `run-cell.sh` samples both, `summarize.py` reports `cancel_all.index_entries_end` / `index_traders_end`, README. Tests first (failed before the change): `phase2_step0_metrics_are_exported` (names and values), `test_summarize.py` phase2-step0 and `test_phase_mapping.py` (sampled names); the bridge invariant helper `assert_index_covers_books` now also checks the size against the index snapshot after every block (all index tests), and the app `p2_index_covers_books` checks the gauges against the holder's index after every block (every resident `reference_paths_*` / `warm_index_*` run). Planted: `take` not subtracting -> 5 bridge index tests and `reference_paths_match_production_classic_resident` fail. Suites: nextest --workspace 3108 / 0 (34 skipped, 0 flaky), doc 1 / 0, matched-bench Python tests 8 / 8 OK, clippy 268 = 268, fmt 3352 = 3352 vs `16cb2633` | before mainnet: the fixed-work background prune (section 7) |
| 22 | 1 (P2-1) | 18c s104 answers before the gate cell: gate on the no-perf estimate, phase 1 down by >= ~1.9 ms per native block (half of ~3.7, results doc 25.1), measured on cells without perf; 2 cells per arm, mirrored, 2 more if the result lands within ~1.4-2.4 ms. Pre-existing, not Phase 2: `exec_modify_order` limits only the modified reduce-only order and runs no sweep (fills stay safe through the match-time maker cap; the effect is overstated resting reduce-only size until the next placement or fill). The reduce-only part of `OrderBook::traders_present` is unreachable today (row 20 planted bug fails nothing) | fix the modify sweep after the P2-1 gate as its own small branch (consensus, fresh genesis), not in Phase 2; keep the reduce-only chain in `traders_present` as a safeguard, commented as unreachable; push step 1 before the gate cell (18c reviews read-only meanwhile); merge main (`12979d4b`) only after the gate cell |
| 23 | 1 (P2-1) | 18c s104 read-only review of `2ecc2bdf` (no suite rerun): approve, no critical / high / medium; index completeness, ascending order, skipped no-op books and the test feature's isolation check out. Lows: (1) `native_executor.rs:4426` with `TORUS_RESIDENT_BOOKS` unset (the code default) the index is rebuilt in every block with a cancel-all (one pass over all (trader, market) pairs vs ~300 lookups before; unmeasured, the gate runs resident); (2) the index gauge counts all entries, not stale ones, and reads 0 before the first build; (3) `ResidentBooks::trader_index` (:3026) and `TraderIndexSnapshot` (:2909) are `pub` with no production caller | after the gate cell: (1) use the index only in resident mode, keep the full scan otherwise, plus a test that the non-resident path is unchanged; (2) for the 9.12 prune, compare the gauge with `torus_exec_resting_orders` and have the prune report how many entries it removed; (3) gate both on `test-reference-paths` |
| 24 | Gate P2-1 | Campaign `ozarchy-p21g` (results doc section 30): `2ecc2bdf` vs `e934fa0e`, 2 cells per arm mirrored, no perf, all rc 0 / AGREE / PASS. Phase 1 -0.44 ms per native block (val0; -0.30 all validators; -3.2% per 1k fills) vs >= 1.9 ms: **missed**, outside the 1.4-2.4 ms band (no extra cells). Cancel-alls visit 189.1 / 188.6 books, sender present in 92.4 (stale entries kept by "never removed eagerly"); index 135k-634k entries, ~200 markets per trader; no visible upkeep cost; matched/s 1.010x | miss recorded (section 4 rule); options to 18c: A accept and continue, B drop a market from the trader's set when their last order / stop there leaves the book (then one more gate cell), C bring the 9.12 prune forward |
| 25 | Gate P2-1 | 18c s104 answer to row 24: **A, accept and continue.** The cell's own counters give ~4 us per skipped book visit (0.44 ms / 111 avoided visits), so B's further ~97 visits per cancel-all are worth ~0.4 ms, still below the 1.9 ms gate, at the cost of a fill-path check and another gate cell. The bench's ~200 markets per trader is near the worst case for stale entries | P2-1 stays as built; C (the 9.12 background prune) stays before mainnet, off the fill path, its gauges showing the entries it removes. Next: merge main `12979d4b` (sync point), the row 23 lows, then P2-1b; before P2-1b's gate, recheck its estimate against the measured per-visit cost (step 0.3's split seems to overstate the per-book saving) (9.14) |
| 26 | 1 (P2-1) | Row 23 lows after the gate and the `12979d4b` merge: (1) the cancel-all index is used only with resident books (`NativeExecContext::resident`); without them the single cancel-all scans every book again and a run goes through `exec_cancel_all_run_every_book`, the pre-P2-1 run body unchanged (`90632a75^`), so no index is built (`non_resident_cancel_all_keeps_the_full_scan`: 13 books visited as in step 0, index `None`; `non_resident_cancel_all_matches_the_full_scan_reference`, four modes; both failed before the fix). The bridge tests' `new_ctx` is now resident so the P2-1 tests keep testing the index; (2) gauge doc: counts stale entries too, `(0, 0)` until the first cancel-all after a load and always without resident books; the prune spec in 9.12 (a) (stale share from the prune's own checked / removed counters, not from `torus_exec_resting_orders`, which counts orders without stops: Codex P3); (3) `TraderIndexSnapshot`, `ResidentBooks::trader_index` and their helpers compiled only under `test` / `test-reference-paths` | rdeps(torus-bridge) 1494 / 0; no new fmt hunks in the two files |
| 27 | 1b (P2-1b) | Estimate recheck before building (ozarchy s31, 18c s106 checked): (a) row 25's "~4 us per visit" is per book per native block; per skipped visit 0.44 ms / (59.2 x 111) ~ 0.07 us (A unchanged, B ~0.4 ms); (b) saving ~457 probes x 0.02-0.07 us ~ 10-30 us per by-id action; (c) upkeep: the p21g cell rested 19.1M orders over 910 native blocks, ~21k inserts + ~21k removals per native block, ~0.6-1.4 ms per block at 14-34 ns (`ubench_hasher` order-id churn, hot cache) on the standard shape, which sends no by-id actions; break-even ~20-140 by-id actions per native block, s-byid delivered 1.22 (12-37 us saved vs >= 0.6 ms upkeep); (d) s-byid's ~2.5 ms per by-id action was mostly not probes (likely split cancel-all runs, inferred), which P2-1 may have cut | **P2-1b C dropped from Phase 2** (9.15); B (market in the order id) stays the later consensus item. P2-5 next. s-byid pair `bdd5b470` vs `e934fa0e` on a quiet host only for (d) and B's priority (no P2-5 cargo builds or tests while it runs) |
| 28 | 1b (P2-1b) | Campaign `ozarchy-p2byid` (results doc section 31; row 27 (d), not a gate): `bdd5b470` vs `e934fa0e`, by-id cells ABBA + one standard cell per arm, all rc 0 / AGREE / PASS. Per by-id action (phase 1 by-id minus standard, val0): ref 2.01 ms, p2 2.98 ms (+0.97, ~1 sd); books probed 454 / 440 per action; matched/s 1.002x. Probes and cancel-all visits explain < 1% | **P2-1 did not cut the by-id cost**; input to B's priority unchanged (~2-3 ms of phase 1 per by-id action, ~1 per native block in this shape). Where the rest goes is not split (needs a perf cell); not on Phase 2's path |
| 29 | 2 (P2-5) | Built: execution-path `HashMap` / `HashSet` from SipHash to `alloy_primitives::map::{HashMap, HashSet}` = std's map with `foldhash::fast::RandomState` (alloy's default `map-foldhash`, seeded per process and per map; already used by `trader_positions` C2 holders). Deviation from section 3: no direct `foldhash` dependency and no new alias (the alloy map is that alias; YAGNI). Swapped: torus-core order book (`order_index`, `order_seq`, `trader_orders`, `TraderOrders.pos`, account margins, reduce-only positions, level / row sets, level-hash cache), `PositionCache`; torus-bridge `native_executor` (balance / volume / sums caches, per-order maps, `order_books`, `dirty_books`, `margin_configs`, block marks, load maps), `market_workers`, `trader_positions`, `liquidation_step`; test call sites of the changed pub types. Audit (read-only inventory): no exec-path map iteration reaches results, state, the state hash or dirty-set order (iterated maps are sorted after or summed exactly); order-dependent only node-local text (worker panic market, a load error) and the by-id probe counter. Guard: `exec_map_hasher_is_seeded_per_map` (fails if alloy's map ever loses its seed, e.g. `map-fxhash` alone). Two-seed differential: the existing app differentials (`reference_paths_match_production_*`, goldens) run separate apps in one process, each map with its own seed | nextest --workspace 3119 / 0, doc 1 / 0, clippy 0 new, fmt 0 new (3355 vs 3359 at HEAD); gate cell next |
| 30 | Gate P2-5 | Campaign `ozarchy-p25g` (results doc section 32): `631becaa` vs `bdd5b470`, 2 cells per arm mirrored, no perf, all rc 0 / AGREE / PASS. Engine -8.59 ms per native block (val0; all validators -8.43; -7.5 ms at equal fills, per 1k fills 0.938x; arms do not overlap per 1k fills), ~73% of the ~10.3 ms estimate; phase margin -3.36, phase 1 -3.04, match -1.34, settle -0.81. matched/s 1.036x (pairwise +3.1 to +4.0%), native blk/s 1.069x; the s82 caveat did not repeat (exec thread busy 0.96 of the window) | **gate met** (>= ~5 ms, AGREE). Next: P2-2 batch flush, then the P2-4 design check |
| 31 | before Gate P2-2 | Cumulative matched/s vs `e934fa0e` against the +7% phase gate (18c s106 asked for it before P2-2's gate). **Chained from separate campaigns, not measured head to head (estimate):** P2-1 1.010x (p21g, `2ecc2bdf` vs `e934fa0e`; `bdd5b470` vs `e934fa0e` standard cells in p2byid 1.004x, n = 1), P2-5 1.036x (p25g, vs `bdd5b470`) -> **~1.040-1.046x today**, ~2.5-3% short. P2-2 at the step 0 estimate (~3.2 ms per native block on the standard cells, row 14) and P2-5's measured rate (-8.59 ms engine -> +3.6% matched/s, ~0.4% per ms) adds ~+1.3% -> **~1.053-1.060x after P2-2**, still ~1-2% short. Each chained ratio carries ~1-3% cell resolution | **P2-4 likely has to count** unless the phase campaign reads higher than the chain; P2-4's walk-0 cost is 10.7 ms per native block (section 22), its build rule is >= 5 ms at walk 10 (section 3). Measure the cumulative directly: P2-2's gate campaign adds an `e934fa0e` arm (3 arms, interleaved) so the head-to-head number is known before the P2-4 design check |
| 32 | 3 (P2-2) | Built: `StateBackend::write_cf_raw_many(cf, rows)` (ordered rows, `Some` = put, `None` = delete; default = one `put_cf_raw_owned` / `delete_cf_raw` per row, so the recording / failing test backends keep their exact sequences); `NativeStateOverlay` overrides it: one lock and one `intern_cf` per batch, per row the same `record` and tombstone / write update, owned key moved in; empty batch never errors, an unknown CF / frozen overlay fails before any row (where the first per-row call would). `PositionManager::write_positions` (put keyed by the row's own fields, delete by the cache key, as per row) and `put_native_balances`; `PositionCache::flush_all` one lookup per dirty key (no second lookup), unstable sort of distinct keys, one batch; `BalanceCache::flush_all` one batch. Error rule kept: an error returns before the dirty marks reset. Encode happens before any write (per row: the rows before the failing one are written); the block is fatal either way. `cum_volume` (per-trader read-modify-write, continues past errors) unchanged. Tests first: overlay property test `write_cf_raw_many_matches_per_row_writes_with_checkpoints` (20 seeds x 300 steps, puts / deletes / repeated keys / checkpoint / revert / commit, DB rows underneath; dropping `record` in the batch fails it at seed 1 step 20), `write_cf_raw_many_errors_like_the_first_row`; `flush_all_matches_the_per_row_reference` (overlay, checkpoints), the failing-backend and `OldBalanceCache` sequence tests and the app differentials with `test_flush_per_row` stay green. Codex review P3 (9.12 (b) not done: `test_flush_per_row` switched only positions): frozen `BalanceCache::flush_all_per_row` added and switched with it; dropping one balance row in the batch path fails all 10 `reference_paths_match_production_*` | nextest --workspace 3121 / 0, doc 1 / 0, clippy 232 = 232 (0 new), fmt 0 new; gate campaign with an `e934fa0e` arm (row 31). The written gate (`cache_flush_ns` -5 ms) is above the step 0 estimate (~3.2 ms on the standard cells, row 14): a miss is likely and is logged, per the plan |
| 33 | before Gate P2-2 | 18c s106, decided before the `ozarchy-p22g` result: (1) if the cache-flush gate (>= 5 ms) misses but P2-2's arms show a gain that does not overlap, keep P2-2 and log the miss (as P2-1, option A): it is in the gate set (9.9) and the cumulative needs it; drop it only if it regresses. (2) The P2-4 design check states up front whether it can cover what is left to +7% (~+0.9-1.6% from the ~1.053-1.060x expected after P2-2; replaced by the cell's head-to-head number); if not, flag it before building so the owner chooses between more items and a lower phase gate. (3) Order: P2-2 gate, R01 fault test (quiet host), merge main `f5f28f89` at the sync point. 18c skipped a review of `b7e66ce5` / `29320f6b` (Codex reviewed, owner rule) | recorded before the cell |
| 34 | Gate P2-2 | Campaign `ozarchy-p22g` (results doc section 33): base `e934fa0e`, p25 `029581e5`, p22 `29320f6b`, 2 cells per arm interleaved, all rc 0 / AGREE / PASS. **Cache flush p22 14.71 vs p25 11.86 ms per native block (+2.85 ms, +24%; ~13 pooled sd, all 3 validators)**, vs the gate's -5 ms and the -3.2 ms estimate: **regressed**. Engine p22/p25 +2.36 ms (1 sd), matched/s 0.990x (not resolved). Pass B +1.00 ms (not touched by the change; cause not established). **Cumulative head to head: p25/base 1.0495x (pairwise 1.045-1.054x)**, p22/base 1.039x; +7% is ~2.0% away from p25. Cause not profiled (no perf); suspected: every row encoded into an owned list before the inserts (the rows go cold before the `BTreeMap` insert touches them; per row they were inserted while hot) and the write lock held for the whole batch | by row 33's rule (drop only if it regresses) P2-2 as built is out; decision below (owner / 18c) |
| 35 | after Gate P2-2 | 18c s106: revert OK (numbers checked: +2.85 ms ~13 sd is a regression by row 33; cumulative 1.0495x, gap ~2.0%, ~5 ms engine at ~0.42% per ms). Host load checked per cell (`cpu.csv`, mid-run load1 21.7-36.3 with no arm pattern; the busiest cell, base r1, flushed normally; validator CPU 12.3-12.6 cores in every cell) | **P2-2 reverted** (`d7c0e596`, `3fe595d3`; history kept; code = `029581e5`). Next: merge main `f5f28f89` (sync point), the R01 fault test, the P2-4 design check alongside it (paper work; states up front whether it can close the ~2.0% gap, else flag with the options: lower the phase gate to the measured figure vs more items, owner decides). A streaming P2-2 (encode + insert per row under one lock) only after a microbench of old vs batch vs streaming at cell-sized batches (~21k rows per block); at its ~3.2 ms estimate it would be ~1.3% alone |
| 36 | 4 (P2-4 design check, read-only) | **P2-4 cannot close the ~2.0% gap on the walk-0 standard shape** (row 33 (2)): at walk 0 the marks never move (feed 30000 every 2 s, `walk_bp=0`), so `fill_block_marks` keeps the version (`native_executor.rs:10552-10555`, plan's :10033 is stale), the sums cache carries across blocks (`into_carry`, :1207-1234), clean traders hit `cached_sums` and traders who filled take the incremental `delta_sums` (:1626). Options (a) skip-by-bound and (b) re-value moved markets save ~0 at walk 0 (no mark moves); cache carry, incremental sums and skipping position-less traders are already built. The 10.7 ms (section 22) is `run_liquidations_with` inclusive over 2,048 traders per block (`liquidation.rs:58`): all `*_sums` together 6.14 ms per block incl. the margin phase, `cached_sums` 0.64; the rest is mostly per-trader reads (`get_native_balance`, `clear_cooldown` / `set_pending` `get_cf_raw`, nearly always no-ops: 981 liquidation-CF writes in ~750 blocks). Two exact walk-0 candidates outside P2-4's scope (estimates, need a walk-0 perf split first): reuse decoded records in `delta_sums` ~1-2 ms; a node-local index of cooldown / pending rows that skips the two reads per healthy trader ~1-3 ms (shadow test vs the CF); together ~2-4 ms, ~0.8-1.7% of the 2.0%. P2-4's walk-10 rule still holds (24.7 - 10.7 = 14 ms there) but moves the walk-0 phase gate ~0% | **flag to the owner (row 33 (2)):** lower the phase gate to the measured 1.0495x vs `e934fa0e`, or add items (the two walk-0 candidates, a streaming P2-2 at ~1.3% if its microbench passes, row 35). Not built |
| 37 | phase gate | 18c s106 owner decision on row 36: **A, lower the phase gate to the measured 1.0495x vs `e934fa0e`; Phase 2 closes with P2-1 + P2-5** (9.16). Estimate vs delivered per native block: P2-1 3.7 -> 0.44 ms, P2-2 3.2 -> -2.85 ms (reverted), P2-5 10.3 -> 8.59 ms. The two walk-0 candidates (`delta_sums` decode reuse ~1-2 ms, node-local cooldown / pending row index ~1-3 ms) and a streaming P2-2 (~1.3% if its microbench passes) go to the backlog as input to Phase 3's step 0 profile, built only where that profile confirms them | Phase 2 build done. Order: push `perf/item6-phase2`; the R01 fault test (`fix/r01-write-fail-stop` `01d991ab`, `/mnt/r01`); merge main `9793f1ec`; final suites on the branch head; 18c's final review, then the merge to main; after it (sync point) compiled defaults (9.13), the reduce-only modify sweep fix, C2 holder sets, and 18c starts R02's loader / save branch; then the Phase 3 plan (step 0 re-profile first) |
| 38 | after the merge (R01/R02 cost check, Phase 3 step 0 baseline) | Campaign `ozarchy-p3s0` (results doc section 34; 18c request, stop rule "if 3efff0d6 is more than noise below 1b389700, stop and report before (d)"): A `1b389700` (Phase 2 bench head, node `86477b00`) vs B main `3efff0d6` (Phase 2 merge + R01 `19f8f534` + R02 branches 1-2, node `0c100f3b`), both nodes built fresh, A B B A after a warm cell, all rc 0 / AGREE / PASS, 0 panic / ERROR / exit 70. **matched/s B/A 0.9886x (-0.9 pooled sd; pairwise 0.9996x / 0.9775x): within noise.** Native blk/s 0.9603x (-3.7 sd; pairwise 0.9555x / 0.9650x) with fills/blk +7.8% (+4.9 sd), txs/blk ~136 vs ~128; per-fill cost flat (engine per 1k fills 0.989x, chain per 1k fills +0.3%). end_resident wait +4.4% per 1k fills (+5.5 sd per native block, ~+0.7 ms) is on the write path R01 touches; residual untimed +11% per 1k fills (2.1 sd); R01 / R02 as cause not established (no perf; cause vs effect of the bigger blocks unresolved at n = 2). B's matched/s spread 4,819 is ~7x A's (b-r2). Host level A 189,241 vs p25 190,298 (section 33; different binary) | **awaiting 18c: accept as Phase 3 step 0 baseline and proceed to (d), or 4-pair rerun** |
| 39 | after the merge (R01 / R02 split of the p3s0 step) | Campaign `ozarchy-p3s1` (results doc section 35; 18c follow-up to row 38): A `1b389700` (node `86477b00`), B `3aa516e0` (Phase 2 + main `95b01af2`: R02 branches 1-2 + EVM RPC typing `9793f1ec`, no R01; node `db344840`, built fresh), C main `3efff0d6` (B + R01; node `0c100f3b`); A and C reuse the p3s0 staged binaries. Warm cell, then rounds ABC BCA CAB ACB (n = 4), all 13 cells rc 0 / AGREE / PASS / accepted, 0 panic / ERROR / exit 70. a-r1 is valid but an outlier (6 view timeouts on val0 vs 0-2, residual untimed 25.89 ms per native block vs 9.6-12.3, 172,338 matched/s, 4.333 native blk/s, oracle 392 / 396; host load1 not higher), excluded: A n = 3. **matched/s each step about -0.7% (B/A 0.9926x, -0.79 sd; C/B 0.9931x, -0.78 sd), C/A 0.9857x (-1.27 sd): not resolved, same direction as p3s0's 0.9886x; with a-r1 included A drops to 184,851 and the sign flips.** Residual untimed per 1k fills flat at both steps (B/A 0.995x, C/B 1.017x, C/A 1.012x): p3s0's +11% does not hold up. Native blk/s flat (B/A 0.986x, C/B 1.014x, C/A 1.0005x): p3s0's -4% did not reproduce; fills/blk C/A 0.957x (-1.43 sd). end_resident wait per 1k fills rises in the R01 step (C/B 1.072x, +1.45 sd; B/A 1.013x), C/A 1.085x (+2.02 sd); per native block 6.06 -> 6.31 ms (+0.24, 0.6 sd), much of the C/B per-fill rise is C's smaller blocks; mechanism not profiled | **18c: accepted**, `3efff0d6` = Phase 3 step 0 baseline; next (d) from main `1eced05c`: compiled defaults = benched config (9.13), reduce-only modify sweep fix (fresh genesis), C2 holder sets; then step 0 re-profile on that head |
| 40 | after Phase 2 (C2 holder sets, 18c s104 / s107) | `perf/c2-holder-hashset`: the per-market holder sets of `TraderPositions` are foldhash `HashSet`s (were `BTreeSet`); `holders_with` (ADL ranking only) sorts. Readers of the sets: `holders_with` (the only iteration, sorted; trader_positions.rs), `same_as` (`==`, order-free), `set_holder` / `build` (insert / remove); the field is private, the only outside reader is `liquidation_step.rs:645` through `holders_with`. New tests: `holders_with` == a BTreeSet reference in every `check`, an insertion-order pin test; ignored µbench `ubench_trader_positions_apply`. Mutations each caught: no reload pass (3 fail), no remove in follow (7), old unconditional pass (sentinel), no sort (9). µbench ABBA x3, quiet host (`apply` ms per 1k fills, writes / 2): toggle 30% 0.512 -> 0.451 (0.881x, ~3 sd), 5% 0.428 -> 0.404 (0.944x, ~1 sd), 0% 0.342 -> 0.350 (control, one B outlier). Per opened / closed key ~285 -> ~175 ns. ubench_adl HL N=5k drain p50 49.8 -> 50.0 ms (all hold), 43.2 -> 43.8 (10%): within noise | Kept (moves toward the pre-`a746c408` level). Node effect not measured (no campaign, 18c): if the ~0.022 ms per 1k fills C2 residual is the set updates, ~0.229 -> ~0.215-0.222. 18c s107: approved (Codex torus-adversarial: approve, no findings); no separate node bench, the Phase 3 step 0 re-profile reports the apply figure |
| 41 | after Phase 2 (book mode control, input to the owner's genesis decision) | Campaign `ozarchy-bkm` (results doc section 36; 18c request s107, before the owner's decision on the book mode in genesis; no Classic vs mode 3 measurement existed, only an August mode 3 vs mode 2 run at 10 markets, different shape): one node main `1eced05c` (node `3fdad0ae`), A = Classic (`EXTRA_ENV TORUS_BOOK_ROWS=0`, applied after RECORD_ENV's 3), B = mode 3 (RECORD_ENV `TORUS_BOOK_ROWS=3`); standard shape at 300 and at 10 markets, per shape warm + A B B A (n = 2). Book mode verified per validator in every cell (env, `/proc` environ, level-authority load lines, `__book_mode__` WAL marker byte 0/0/0 for A, 3/3/3 for B); all 10 cells rc 0 / AGREE / PASS / accepted, 0 panic / ERROR / exit 70; suite lock held, every cell started at load1 < 2. **matched/s B/A 1.0126x at 10m (pairwise 1.018 / 1.007), 0.9645x at 300m (pairwise 0.903 / 1.029, b-r1 173,470): not resolved at 300m.** Flush (off-chain, on W) per 1k fills 1.95x at 10m (9.53 -> 19.80 ms per native block), 1.66x at 300m (34.44 -> 57.55 ms; pairwise per block 1.94 / 1.45); ~74% / ~63% of the rise is mode 3's deferred book write (pass 2) on W; exec-thread handoff wait 0.66 -> 3.58 ms (300m), 0.61 -> 1.56 ms (10m). save_books per 1k fills flat (0.98x at 10m; 0.89x at 300m, noisy); Classic has no drain / write split (n/a). Native blk/s 0.934x at 10m with fills/blk +6.8% (block ms per 1k fills 0.993x), 0.923x at 300m (b-r1 only). Mode 3 cells peaked load1 ~53 vs 32-43 Classic at 300m (no pattern at 10m); view timeouts 5-6 in every counted 300m cell. No throughput case for mode 3 over Classic at this shape; any case for mode 3 rests on its state root / design properties (not measured) | **owner via 18c s107: Classic on testnet; book mode not in genesis now; Classic stays the compiled default (`feat/compiled-defaults`); revisit mode 3 with a deep-book shape before mainnet** (9.13) |
| 42 | Phase 3 step 0 (re-profile on main `9b7e29b2`, 18c s108) | Campaign `ozarchy-p3s0r` (results doc section 37), node `6a71ba5f`, bench `001e3176`, fresh genesis per cell (reduce-only modify sweep fix is consensus-visible); knobs unset = compiled defaults. Arm m mode 3 (`TORUS_BOOK_ROWS=3`, kept for p3s1 comparability) + cap 8: 4 no-perf, 2 perf (val0 cycles:u); arm k Classic: 2 no-perf. All 9 cells rc 0 / AGREE / PASS, oracle stale 0. m: 186,504 ± 3,163 matched/s (1.001x p3s1 C, not an A/B), 29.87 ± 0.26 node CPU-s/1M: execution 40%, signature work 27%, flush worker + RocksDB 18% (~24% with trade writer + hotstuff); disk 0.704 GB/1M, at most ~54% coalescable (trade rows, gossip, hotstuff append-only). k Classic: 1.024x matched/s, CPU 0.924x, flush worker CPU 0.56x, disk 1.34x. Backlog (row 37): `delta_sums` decode reuse not confirmed (0.63 ms per native block, decode ~0.12), cooldown / pending index not confirmed (0.35 ms), streaming P2-2 cost present (cache_flush 12.17 ms per native block, `put_position` 56%). C2 `apply` ~0.19 ms per 1k fills (row 40). Restart -> ready not measured. **Classic follow-ups (testnet runs Classic, row 41), same node and knobs:** `ozarchy-p3s0c` (k-warm, 2 no-perf, 2 perf; Classic no-perf n = 4 with the p3s0r k cells) and `ozarchy-p3s0cf` (2 Classic + 2 mode 3, RocksDB `LOG` saved per cell for a per-CF split; LOG sums = node counters in every cell); all 10 cells rc 0 / AGREE / PASS. Classic vs mode 3: node CPU 27.48 ± 0.14 vs 29.87 CPU-s/1M (-8.0%, 11 sd; exec workers -1.67, flush worker -0.77), matched/s 1.017x (not resolved), disk 0.931 vs 0.704 GB/1M (the extra is flush worker state rows that collapse in the memtable). Classic persistence ~20-23% of node CPU, third behind execution (38%) and signature work (29%). Compaction by CF (Classic / mode 3, CPU-s/1M): trades 1.69 / 1.15, DA 0.74 / 0.53, state rows 0.52 / 0.92, bodies 0.15 / 0.13. Most Phase 3 can remove on Classic (flush worker + state-row compaction + state-row memtable flush) 1.76 CPU-s/1M (6.4%), 2.20 (8.0%) with streaming P2-2, vs 2.75 for -10%; append-only compaction (trades + DA) 2.43 (8.9%), mostly LZ4 -> ZSTD re-compression. Backlog unchanged on Classic | **On Classic, Phase 3 as planned cannot reach its -10% node CPU gate** (high confidence; mode 3 reaches -10% of its own CPU only just and stays above Classic's target). 18c / owner: (1) keep Phase 3, gate ~-6%; (2) add trades / DA compaction tuning to Phase 3; or (3) compaction tuning first as its own item; and pin the gate baseline (Classic `9b7e29b2`) |
| 43 | Phase 3 step 0, option 3 (owner s108: trades + DA compaction tuning first, baseline Classic `9b7e29b2`) | Branch `perf/append-cf-compaction` `fa8b646f` (design `docs/plans/append-cf-compaction.md`): node-local `TORUS_ROCKSDB_APPEND_CF_CODEC` (`zstd1` / `lz4`, three append-only CFs only) and `TORUS_ROCKSDB_WAL_COMPRESSION` (`zstd`), exact-today when unset; tests: OPTIONS per CF, open across settings both ways; Codex quick review no findings. Campaign `ozarchy-acc` (results doc section 38), Classic, n = 2 per arm + control c (`6a71ba5f`, n = 1); all 11 cells valid, knobs and codecs verified per validator. Node CPU-s/1M vs b (29.33, val0-2 `proc_cpu`): w WAL budget 2048 MiB -1.68 (-5.7%, 9 sd), l LZ4 -1.02 (3.4 sd), z ZSTD level 1 0.00 (not resolved), d WAL compression +0.39 (4 sd); matched/s not resolved. Compaction CPU (b / l / w): 3.43 / 1.95 / 1.83. Disk per validator: all writes b 180 MB/s, d 56 (WAL 139 -> 13), w 168 (2 GiB WAL kept); SST growth (bench load, young chain) b 680 GB/day, l 912. Restart -> ready not measured | **Screen: no single setting reaches -10% (2.75); w gives -5.7%.** Owner (s42): no disk limit, ZSTD level 1 preferred, LZ4 only if clearly more CPU (not the case: 1.5 sd node CPU). WAL compression is a separate SSD-wear decision (-69% bytes, +1.3% CPU). Next: n = 4 confirm of b, w, w + z (maybe w + z + d) with restart -> ready |
| 44 | Phase 3 step 0, option 3, confirm (owner via 18c after the screen: ZSTD level 1 dropped; confirm b, w, w + d and trade history off, n = 4, with restart -> ready for the 2048 arms) | Campaign `ozarchy-acc2` (results doc section 39), node `2ede76eb` (`fa8b646f`), Classic, n = 4 interleaved (arm order rotated per round), all 16 throughput cells valid; t verified 0 trade rows on every validator. Node CPU-s/1M vs b 28.80 (val0-2 `proc_cpu`): t `TORUS_TRADE_HISTORY=0` -3.44 (-11.9%, 15 sd), w WAL budget 2048 MiB -1.83 (-6.3%, 22 sd), wd w + WAL ZSTD -1.38 (-4.8%, 13 sd). matched/s vs b 188,229: t +8.2% (6.4 sd), wd +5.2% (5.5 sd), w +2.4% (2.8 sd, marginal). Disk per validator: all writes b 164 MB/s, wd 43 (-74%), t 131 (-20%); data dir b 1.35 GB, w 2.71, wd 1.20, t 0.81; SST growth (bench load) b 634 GB/day, t 292 (-54%). Restart (SIGKILL val1, n = 3): DB open b 1.32 s, w 3.99, wd 3.04; restart -> first commit 16.3-16.8 s in all arms (not resolved). 3 validators share one disk on the bench host | **Trade history off alone passes the -10% gate (2.75).** w confirmed at -6.3%; wd best WAL arm on throughput and disk (shared-disk caveat). Next: owner's validator defaults (trade history, WAL budget, WAL compression), then the Phase 3 decision |
| 45 | Phase 3 baseline candidates (owner via 18c after row 44: validators trade history off + WAL budget 2048 MiB; WAL compression decided by this run; recovery check on twd, no restart timing) | Campaign `ozarchy-acc3` (results doc section 40), node `2ede76eb` (`fa8b646f`), Classic, n = 4 interleaved, all 12 measured cells valid; tw / twd 0 trade rows on every validator. Node CPU-s/1M vs b 28.75: tw -3.82 (-13.3%, 21.9 sd), twd -3.22 (-11.2%, 15.2 sd); twd vs tw +0.59 (+2.4%, 4.3 sd). matched/s vs b 187,202: tw +9.0% (9.7 sd), twd +10.9% (6.1 sd); twd vs tw +1.8% (1.2 sd, not resolved). Disk per validator: all writes b 168 MB/s, tw 131, twd 28; data dir b 1.33 GB, tw 2.01, twd 0.60; SST growth b 647 GB/day, tw 322, twd 323. Recovery check (SIGKILL val1 at +40 s on twd): rejoined, 1,843 commits after the restart, state digests equal, no ERROR. 3 validators share one disk on the bench host | **tw vs twd is CPU (tw -2.4%, resolved) against disk (twd 4.7x fewer bytes written, 3.4x smaller data dir); matched/s a tie.** Both pass the -10% gate vs b. Next: owner picks the Phase 3 baseline; twd needs `perf/append-cf-compaction` merged |
| 46 | Phase 3 step 0, sizing on tw (owner via 18c: plan `item6-phase3-impl.md` step 0 + add-ons: 100-block window and cap, restart budget from serial replay speed, digest as its own line, stop rule at ~5%) | Campaign `ozarchy-p3s` (results doc section 41), node `fb460a0c` (main `fd9e5dfa`), Classic, tw; tw-r1 (val0 WAL kept), tw-r2, tw-old1 control (`2ede76eb`), 3 multi-crash cells (9 kills of val1), 2 perf cells; all 8 cells AGREE, no cell excluded (crash cells rc 2 = the known INFO-line false FAIL). Coalescing (tw-r1 val0 WAL decoded: 17.7M entries, 0 CRC errors, sequence continuous; state batches only), bytes / keys: 1 block 1.00 / 1.00, 5 s 4.41 / 1.30, 15 s 5.75 / 1.57, 30 s 9.93 / 2.62, 100 blocks 6.38 / 1.64, plan min(15 s, 100 blocks) 5.95 / 1.58, min(30 s, 100 blocks) 6.34 / 1.64; book blobs 77% of state bytes (37-83x), positions 92% of entries (~1.5x). State-row WAL writes 74.8 -> 12.6 MB/s at 15 s (-83%). Layer / checkpoint peak 214 MB (15 s), 219 MB (30 s capped), 235 MB uncapped 30 s (key + value; ~1.4M keys). Restart (n = 9): DB open 2.68 s, serial replay 5.16 blocks/s, restart -> first commit 13.5 s; budget 15 s 21.4 s, 100 blocks 22.4 s (pessimistic <= 45 s incl. 66 blocks of exec lag). CPU (val0, CPU-s/1M): flush worker 0.93 + state memtable flush 0.29 + state compaction 0.34 = ceiling 1.55 (6.2% of 24.93); digest 0 (running state hash not activated in the bench genesis; estimate 0.26 if active); scaled by the factors 0.82 (3.3%) | **Owner decision needed before step 1:** the ceiling clears the -6% gate (1.50) by 0.06 only if all of the work goes; the factor-scaled saving is ~3.3%. Disk (-83% state-row bytes) and restart (<= 60 s) pass. Options (plan stop rule): build for disk and W-ceiling gains with a lower CPU gate, or drop Phase 3 |
| 47 | Phase 3 step 0, checkpoint cost by prototype (owner via 18c s111 after row 46: options (b) prototype-measure + (c) book-blobs-only arm; rule: all rows >= 5% node CPU clearing the noise -> step 1, else only books close -> re-scope to book blobs, else shelve and keep the disk numbers) | Campaign `ozarchy-p3p` (results doc section 42), throwaway `proto/p3-coalesce` `682a8df5` (not for merge), node `6e40fa3c` / bench `19f7ac94` in every arm, tw, Classic; arms c (control), a (`TORUS_P3_PROTO=all`, all state rows one batch per 100 blocks), k (`=books`, Classic book blobs only), cS / aS (c / a + `STATE_HASH_ACTIVATION=1`); n = 4 rotated rounds, 6 graceful SIGTERM restart cells, 2 multi-crash cells; all 29 cells AGREE, none excluded (the 8 restart cells rc 2 = the known INFO-line false FAIL, plus the all-arm rewind beyond the exec queue by design). Node CPU-s/1M (whole run): c 24.996 (sd 0.41), a 23.770 (**-4.90%**, CI ~2.4-7.4%), k 24.251 (-2.98%), cS 25.335, aS 24.049 (**-5.07% vs cS**, CI ~4.0-6.1%); pooled -4.99%. Node CPU per validator per cell is flat (655-661 CPU-s), so the per-fill saving rides on matched/s: a +4.71% (every a cell above every c cell), aS +6.30% vs cS, k +2.88% (not resolved). At equal fills (common slope 10.1 ± 2.1 CPU-s per 1M fills) -2.1% (a) / -2.5% (aS) / -1.0% (k). Removed work (flush worker + `rocksdb:high` + `:low`) -0.80 CPU-s/1M, the checkpoint thread <= 0.27 (section 41 model 0.82, ceiling 1.55). Digest (state hash on - off) cS - c +0.34 (1.1 sd), aS - a +0.28 (2.6 sd): ~0.3 CPU-s/1M, replacing the 0.26 estimate. Disk per validator: all writes 131.0 -> 64.5 MB/s (-51%, a) / 72.4 (-45%, k); WAL -56% / -50%; flush + compaction -22% / -14%. RSS +0.8 GB per node (a), +46 MB (k). Stalls (a): checkpoint 1.2M rows / 188 MB, build 262 ms, write 552 ms (max 1.29 s), absorb 9.5 ms per block on the flush worker; books 13 MB, 6 / 17 ms, 0.11 ms per block. Restart: SIGKILL all rows replays 120 / 130 / 34 blocks at 4.8-5.9 blocks/s, restart -> first commit 21.3 s mean (control 12.1 s; gate 60 s); graceful a / k replay nothing but drain the exec queue before exiting (down 8 s; kill -> first commit 18.8-28.8 s vs 13.5-13.8 s for the control, whose SIGTERM is a kill) | **By the rule: shelve Phase 3 and keep the disk-write numbers (borderline, owner to confirm).** All rows sits on the 5% line (4.90% / 5.07%) and does not clear it; at equal throughput it is 2.1-2.5%; books only (2.98%) is not close. The matched/s gain may be smaller with a disk per validator (the bench host shares one). To resolve the line: 4 more c / a pairs (~50 min) or a fixed-load pair at equal fills (~50 min). **Owner decision (s111, via 18c): (a) shelve the Phase 3 checkpoint core (steps 1, 2, 4, 5, 6).** Deciding number: the equal-fills saving 2.1-2.5% < 5%; per-cell CPU is flat, so the -4.9% is mostly extra throughput on a shared disk and likely won't survive a disk per validator; not worth +0.8 GB/node, 188 MB checkpoints, longer crash restarts and the extra persistence complexity. Disk-write numbers (-51% writes / -56% WAL) kept on file in case disk I/O becomes the bottleneck. Not shelved: task 1.1 pruner clamp (merged, `a9120a10`); step 3 D13 trade-history retention for RPC nodes, built as its own task. `proto/p3-coalesce` stays local, not pushed. **Reversed (owner s111, same day): Phase 3 UN-SHELVED, step 1 starts.** The 5% CPU rule missed Phase 3's payoff (matched/s +4.7%, native blocks/s +6.1%, CPU -2.1% at equal fills, writes -51%; RAM +0.8 GB/node is the intended trade of item 6), and Phases 4-5 build on it. New phase gate: matched/s and blocks/s up, no CPU regression, restart <= 60 s, crash correctness (item6-phase3-impl.md section 1) |
| 48 | Phase 3 (un-shelved, owner s111): does the prototype's throughput gain hold when the validators do not share a disk? (owner: disk isolation, tmpfs as the bound; ozarchy has one NVMe) | Campaign `ozarchy-p3t` (results doc section 43), the p3p node `6e40fa3c` (`proto/p3-coalesce` `682a8df5`) / bench `19f7ac94` in every arm, tw, Classic, n = 4 per arm in a Williams square; arms cd / ad (control / `TORUS_P3_PROTO=all`, shared NVMe) and cm / am (the same with all 3 data dirs on tmpfs, an **UPPER BOUND, not a separate-disk result**); all 17 cells AGREE, ACCEPT, no deaths. a vs c, shared NVMe: matched/s **+5.8%** [3.5, 8.1], native blocks/s +14.0% [2.3, 25.7] (noisy control), node CPU-s/1M **-5.2%** [4.3, 6.1], at equal fills -3.0%, wchar -46%, RSS +0.9 GB. tmpfs: matched/s +3.1% [-1.0, 7.3], blocks/s +1.6% [-4.5, 7.6], CPU-s/1M **-4.8%** [2.2, 7.4], at equal fills -4.2%, RSS +1.1 GB. Paired by round, tmpfs gain minus disk gain: CPU +0.4 pp [-3.9, 4.7], matched/s -2.6 pp [-7.6, 2.5], blocks/s -12.8 pp [-23.3, -2.4]. The control alone gains +5.2% matched/s / +17% blocks/s on tmpfs. tmpfs data peak 2.7-2.8 GB per validator | **The CPU saving is work removed** (same in both modes), so the gate's "no CPU regression" holds without the shared disk. **The matched/s gain shrinks** (about half is consistent with disk contention). **Most of the blocks/s gain was disk contention.** Inference (not measured): a production validator with its own disk lands between the shared-NVMe and tmpfs rows, about +3% to +6% matched/s, between +2% and +14% blocks/s depending on how loaded its disk is, and ~5% less CPU-s/1M |
| 49 | P2-3 persistent worker pool (backlog since 9.10): sizing by profile (owner, after Phase 3 un-shelved) | Campaign `ozarchy-p23` (results doc section 44), main `4e7fa146` (node `169567f2`, bench `74cef98f`), tw, Classic, 300 markets; 4 profiled cells (perf `cycles:u` on val0, 45 s, the step 0 method) + 4 unprofiled; all 9 cells AGREE. **41.0 spawns per native block** (36.6-43.4; exact `PERF_RECORD_FORK` counts, not a lower bound), all from the execution thread, ~2.5 scopes per block: `match_parallel_capped` (32-way), `settle_market_results`, `end_resident_on_worker` (1 per block); save books, margin prepare, open orders and the digest spawn nothing here. Cost: user-side glue 0.04% of user cycles; spawned-thread sys 0.142-0.147 CPU-s/1M in the node (upper bound, includes the workers' syscalls); microbench (`spawnbench.rs`, Rust scope, 190 parked threads) 47-60 us CPU per spawn vs 5-9 us per pool task, a 32-thread scope +0.7 ms vs a pool. Perf on val0 inflates its sys 1.7 -> 7.0-7.6 CPU-s/1M (inherited events) | **A pool saves ~0.05-0.15 node CPU-s/1M (0.2-0.6%; ceiling 0.28) and ~0.9-1.4 ms of execution-thread wall per native block (0.5-0.8%)**, about +0.5-1% matched/s at most (section 25's ~0.8 ms estimate holds). Stays a small backlog item; if built, gate on the fork count (perf task events) and matched/s, not sys CPU |
| 50 | Phase 3 step 1 measured (owner s111, revised gate: matched/s up, CPU-s/1M down, native blocks/s no regression, restart <= 60 s, crash correctness) | Campaign `ozarchy-p3c1` (results doc section 45): `feat/item6-phase3-step1` `7c178637` (node `c161a337`, `TORUS_CHECKPOINT_INTERVAL_MS=15000`) vs its branch point main `2725e321` (node `18fb490d`), one bench `c4559781`, tw, Classic, n = 4 per arm in the p3t Williams square, shared NVMe (cd / sd) and tmpfs UPPER BOUND (cm / sm); round 4 rerun (the harness worktree was removed mid r4-sm by a cleanup; first r4-sm excluded); all 16 analysed cells AGREE, ACCEPT, every hard check OK, `TORUS_EXEC_PIPELINE` on in every cell. Step 1 vs control, shared NVMe / tmpfs: node CPU-s/1M **-7.9%** [6.8, 9.1] / **-8.3%** [7.5, 9.0] (prototype p3t: -5.2% / -4.8%); native blocks/s **+96% / +86%** (chain_ms 124 -> 64, half the actions per block); matched/s **-7.0% / -10.6%**; device writes -57%, wchar -53%; RSS +0.15 / +0.23 GB. Checkpoints ~20 per node per cell (7.5/min), 94% at the attest height `h % 100 == 0` (100 blocks take ~8.3 s at ~12 blk/s, under the 15 s interval; the interval fired ~1 per node per cell); wait 0.2-0.3 s per cell (9-14 ms per checkpoint), layer mean 80 MB / peak 0.23 GB, flush-worker 0.30 s per checkpoint, **layer_shared 0 everywhere**. The matched/s drop is the step 1 read lag: `eth_blockNumber` advances only at checkpoints (steps of 100), so the bench's in-flight release stalls 37 s per 120 s cell and offered load falls 13-16%; node RPC latency and exec busy (0.89 vs 0.95) show headroom. No new rejection class; deterministic duplicate-skip WARN on disk 4 -> 62 per node per cell (mempool lag). Restart and crash not tested here. | Gate: CPU down **met**, blocks/s no regression **met**, matched/s up **not met as measured / not decidable with this bench** until step 2 (current RPC / mempool reads) or a bench that does not release on `eth_blockNumber`; restart and crash still to measure, riding along with the step 2 bench (18c s111). Also valid for step 1 `a44bdde6` (its only change, the byte-cap fix, acts only when `TORUS_CHECKPOINT_MAX_BYTES` is set; unset here). |
| 51 | Phase 3 step 1 restart and crash correctness (owner via 18c s111: gate restart <= 60 s, crash cells; row 50 = p3c1 is on the unmerged `docs/p3-step1-bench`) | Campaign `ozarchy-p3r1` (results doc section 46), one node `a44bdde6` (`7d1a0874`, bench `c4559781`), tw, Classic, 300 markets, shared NVMe, `STATE_HASH_ACTIVATION=1` in every cell; arms o (`TORUS_CHECKPOINT_INTERVAL_MS` unset; unset and 0 both mean off) and s (15000), exec pipeline on (checked); 12 cells of 540 s with 4 val1 kills each at two offset sets (x 60/180/300/420 s, y 65/187/309/431 s), signals KILL, TERM and INT, 2 cells per arm x signal = 48 kills. **SIGTERM is a hard kill on `a44bdde6`** (main.rs waits only on ctrl_c); SIGINT is the clean stop (final checkpoint, 0 replay). Gate restart <= 60 s **met**: kill -> first commit (val0) s KILL 14.4 s mean / 23.2 max, o KILL 14.3 / 16.9, s TERM 15.7 / 21.7, o TERM 14.7 / 15.5, s INT 13.8 / 21.2, o INT 19.4 / 22.6 (drains the exec queue before exit, down 9.0 s); p3p prototype 21.3 s mean restart -> first commit, step 1 13.4. Replay s: from the last checkpoint (multiple of 100, or a 15 s time trigger at 4099), 7-162 blocks, mean 92 (KILL); o: the exec queue, 16-65. DB open 2.4-3.9 s. Rejoin 8-13 s mean. **Correctness: no finding** on 48 kills: replay starts at marker + 1 and covers marker+1..committed once, ENABLED applied = committed, marker >= durable hash height before the kill; no mismatch / FATAL / panic / fail-stop; every 100-block state-hash checkpoint equal on the 3 validators in all cells; books, OI and mark-free balances equal. Summarize's DISAGREE in 9 cells = stale oracle marks (digest > 60 s after the oracle pause), harness artifact. No on-chain attestations (devnet passes no attest key). Excluded: x1-s-TERM kills 3-4, inside a ~9 min chain halt from the gossipsub subscription wedge after an in-place restart (S395 signature, both arms show it after 20 of 48 restarts; only this one halted). Open: SIGTERM handling, restart pattern in crash-kill.sh, digest timing |
| 52 | Phase 3 step 4 gate for step 2 (owner via 18c s112: step 2 `2296be67` at 15 s vs main `c97f56f9`, both arms with the bench in-flight tail on committed bodies, `bench/inflight-commit-tail` `86d2b2bb`; questions: blocks/s drop on the control too? DISAGREE on control? cancel-all estimate) | Campaign `ozarchy-p3s2g` (results doc section 47; rows 50 / 51 = sections 45 / 46 on `docs/p3-step1-bench` / `docs/p3-step1-restart`), nodes `f35fab7d` (control) / `1c02893b` (step 2), one bench `3af99b73`, tw, Classic, shared NVMe, n = 4 alternating, every cell's tail mode checked = commit; 7 ACCEPT + r3-cd REJECT (mark-only digest disagreement, kept). Confound: step 2 is off `2725e321`, the control adds 16 non-Phase-3 commits (R18 snapshot, trade-cutoff read fault, CoreWriter undecodable row, docs; expected neutral). sd vs cd: matched/s 199,667 -> 195,333 (-2.2% [-7.1, +2.7]), native blocks/s 3.49 -> 3.16 (-9.4% [-18.6, -0.2]), node CPU-s/1M 25.20 -> 23.32 (**-7.4%**, -9.2% at equal fills), device writes -53%, RSS peak 4.41 -> 5.93 GB (**+1.53 GB**, mean +1.13 GB). Exec busy 0.96 / 0.98, chain_ms 258 / 281, ~290-297 actions per block, exec 15 s (~63 blocks) behind commit in both arms. Control head -> commit: 6.14 -> 3.49 blk/s, matched/s and CPU per fill flat. DISAGREE: r3-cd val1 + both step 2 smokes, all 5 mark fields only (digest finishing past ~60-70 s after the oracle pause). Cancel-alls: 0 of 447,053 failed on chain, 0 open-limit rejects; step 2 only: 0.8-2.6% of cancel-alls evicted by the 60 s nonce window in 3 of 4 cells | **Q1: the blocks/s drop is on the control too: batching under an exec ceiling, not a per-fill cost.** Q2: DISAGREE also on control, mark-only, harness timing; no hard finding. Q3: negligible (no failed cancel-all). **Gate: CPU met, disk met, matched/s tie (not up), blocks/s -9% (borderline regression; Phase 3 untimed exec work, 4.8 -> 18.8 ms per 100 actions, same in step 1), RAM not met (+1.5 GB vs ~270 MB bound; step 1 vs step 2 split not measured).** Owner decision |
| 53 | Phase 3 step 2 RSS split (owner via 18c s112, Open entry of row 52 item (a): split step 2's +1.5 GB into the step 1 layer, the step 2 view and the commit-mode exec backlog; heap profile without sudo) | Campaign `ozarchy-p3rss` (results doc section 48): control `2725e321` (`18fb490d`; = merge-base(step 1, main), step 1 `a44bdde6` (`7d1a0874`) = control + 13 commits, step 2 `2296be67` (`1c02893b`) = step 1 + 5: a clean chain), main `8efcb07a` (`09827336`, 2 cells, cross-check), one bench `05d44313` + harness from `8efcb07a`; tw, Classic, shared NVMe, n = 4 rotated per arm: cth (control, tail head), ctl (control, commit), s1 / s2 (commit, interval 15 s); + 7 jemalloc heap-profiled cells (system `libjemalloc.so.2` 5.3.1 with prof via an `LD_PRELOAD` wrapper, dumps every 8 GiB allocated, own symbolizer); all 26 cells rc 0, ACCEPT, AGREE, 364 CHECK OK. RSS peak / mean GB (cpu.csv): cth 3.56 / 3.17, ctl 4.38 / 3.64, s1 5.26 / 4.27, s2 5.94 / 4.76, main 4.38 / 3.67. **Backlog** (ctl - cth) +0.83 [0.73, 0.92] / +0.47; **layer** (s1 - ctl) +0.88 [0.76, 1.00] / +0.63; **view** (s2 - s1) +0.68 [0.54, 0.82] / +0.49; main - ctl +0.00. Live heap (jemalloc, MB per node): backlog +708 (mempool +343, decoded blocks / exec queue +226), layer +250 (`CheckpointLayer` +316 for a 147 MB logical layer, frozen pending sets -157), view +324 (slices +260, layer frames +90, memtables +34); jemalloc RSS deltas +0.51 / +0.45 / +0.33 GB. RSS is >= 96% anonymous heap; RSS still grows at the bench end (0.7-1.9 GB/min), no swap | **The Phase 3 RSS is +1.55 GB (layer +0.88, view +0.68) over the clean commit-mode control. Of it, 0.57 GB is live data, and about half of each delta appears only under glibc (allocator retention).** The backlog part (+0.83 GB) is the commit-tail bench mode and is in the control too. RAM gate (~270 MB) still not met; levers: glibc retention around the per-checkpoint rebuild (~0.8 GB), `L`'s 2.1x in-memory overhead, the view's slices |
| 54 | Phase 3 gate rerun (owner via 18c s112: step 1 `b1605417` and step 2 `51a67f57`, both with main `8efcb07a` merged, vs main `8efcb07a`; commit tail, n = 4; new checkpoint timers in the harness; one heap-profiled step 2 cell for the free-off-exec-thread path) | Campaign `ozarchy-p3g2` (results doc section 49): nodes main `09827336`, step 1 `c2cfb1c9`, step 2 `448bd3ac` (interval 15 s), bench `05d44313`, harness `bench/summarize-ckpt-timers` `218ad251` (merged `b648bbf2`: `checkpoint_settle_wait` / `checkpoint_upkeep` phases); tw, Classic, shared NVMe, r1-r4 rotated, 12 throughput cells ACCEPT AGREE + h1-g2 (jemalloc heap profile) REJECT on a mark-only DISAGREE (val2 slow digest). merge-base(each, main) = `8efcb07a`: clean control. Step 2 vs main: matched/s +2.9% [-3.1, +8.9], native blocks/s -5.5% [-10.4, -0.7], CPU-s/1M -7.0% [-9.1, -4.9] (-6.8% at equal fills), writes -51%, RSS +1.55 GB peak / +1.13 mean, exec per block p50 360 vs 355 ms, p99 662 vs 654, commit interval p99 +12.7%. Step 1: CPU -8.7%, blocks/s -7.2%, RSS +0.76 GB. Exec residual: g2 `checkpoint_settle_wait` 23.9 + upkeep 1.0 ms per native block (p50 / p99 per observation 1.3 / 95 ms and 0.1 / 6.4 ms), residual 16.9 vs 13.4 (g1, untimed: 48.6); `torus-ckpt-drop` 5.0-5.5 CPU-s per cell on val0. Cancel-all nonce-window evictions 0-27 per node per cell (section 47: up to 1,351) | **CPU and disk wins hold against a clean control; matched/s a tie; native blocks/s -5.5% and RAM +1.55 GB are not met.** The section 47 residual is resolved: the merge join (24 ms per block) is timed and +3.5 ms remains untimed. The frees run off the exec thread. Results also valid for step 2 head `6d9b78af` (adds only the drop_inline counter) |
| 55 | P2-3 persistent exec worker pool A/B (owner via 18c s112: `perf/p2-3-worker-pool` `efcd1548` vs its merge-base `c97f56f9`; hard check perf forks ~0 per native block on the pool; then matched/s, blocks/s, CPU-s/1M incl. equal fills, exec time per block, exec busy) | Campaign `ozarchy-p23b` (results doc section 50): nodes control `f35fab7d`, pool `dacdeec8`, bench `05d44313`, harness `8efcb07a`; tw, Classic, shared NVMe, commit tail, n = 4 ABBA unprofiled + 2 perf cells per arm (section 44 method); all 12 cells ACCEPT AGREE, 182 CHECK OK. Perf forks per native block: pool 0 / 0, control 44.4 / 46.4 (all from `torus-execution`, ~2.9 scopes per block); spawn counters per block: control match ~20, settle ~20, end_resident 1.00; pool 0 at every site (end_resident = 1 per process). Pool vs control: matched/s -1.2% [-5.0, +2.6], native blocks/s +0.3% [-7.6, +8.2], CPU-s/1M +0.4% [-1.8, +2.6] (-0.7% [-1.5, +0.0] at equal fills), exec wall per block mean / p50 / p99 +0.3% / +0.9% / +1.8% (all CIs span 0), exec busy 0.955 / 0.956, val0 exited-thread sys 0.12 -> 0.01 CPU-s/1M | **The pool removes every per-block spawn (hard check met). Its ~0.4% expected gain does not resolve at n = 4 x 120 s; no regression is visible.** Only the spawned threads' sys CPU shows the saving (-0.11 CPU-s/1M). Merge decision is the owner's |
| 56 | Phase 3 allocator A/B (owner via 18c s112, after section 49: glibc vs jemalloc on the next step 2 head `7b006739` and its control main `219f0c20` (= merge-base); commit tail, n = 4; RSS peak / mean / growth, CPU-s/1M incl. equal fills, matched/s, blocks/s, exec time per block, exec busy; pick the soak allocator) | Campaign `ozarchy-p3alloc` (results doc section 51): nodes control `987f4eb6`, step 2 `ca388315` (interval 15 s), each also through an LD_PRELOAD wrapper for system jemalloc 5.3.1 (no `MALLOC_CONF`, profiling off; mapped in all 3 nodes of all 8 je cells, in none of the glibc cells); bench `05d44313`, harness `218ad251`; tw, Classic, shared NVMe, r1-r4 rotated, 16 cells, 238 CHECK OK. RSS peak GB gm / g2 / jgm / jg2 4.12 / 5.67 / 4.38 / 5.34, growth 0.87 / 1.64 / 0.70 / 1.01 GB/min; Phase 3 delta +1.55 [+1.35, +1.75] (glibc) / +0.97 [+0.66, +1.27] (je); je vs glibc on step 2: peak -0.32 [-0.53, -0.12], matched/s +3.6% [+2.2, +5.0], CPU-s/1M -1.1% [-2.6, +0.5] (equal fills +4.0% [+1.6, +6.3], extrapolated over a ~14% fills gap), exec wall per block -13%; on main: CPU-s/1M +1.5% [+0.9, +2.2], matched/s +0.6% [-1.2, +2.4]; je commits +15-18% faster, so the closed-loop bench (released on commit) submits +13-14% more; node RPC admit / verify not faster (offered load confound, verdict on equal-fills CPU). r3-jg2, r3-g2, r4-g2 REJECT: mark-only digest artifact (5 mark fields, 1 odd node; books / OI / cash identical) | **RAM gate (~270 MB) not met under either allocator.** jemalloc has the lower step 2 RSS but fails the CPU condition at equal fills (+4.0% [+1.6, +6.3]): **soak winner glibc**. Shipping an allocator is the owner's decision |
| 57 | SIGTERM restart cells on the next step 2 head (owner via 18c s112, after `fix/sigterm-shutdown` merged as `219f0c20`; section 46 found SIGTERM unhandled): per kill SIGTERM -> exit, shutdown phases, replay blocks, restart -> DB open / first commit, rejoin, state agreement; exit vs Docker's 10 s grace and `TORUS_SHUTDOWN_TIMEOUT_SECS`; restart gate <= 60 s | Campaign `ozarchy-p3term` (results doc section 52): step 2 `7b006739` (`ca388315`, interval 15 s) vs main `219f0c20` (`987f4eb6`), state hash on, commit tail, harness `218ad251` + `crash-kill.sh` with 0.1 s exit polling, KILL_WAIT 75 s; 8 step 2 + 4 control SIGTERM kills of val1 in 540 s cells; `TORUS_SHUTDOWN_TIMEOUT_SECS` unset (60 s default) on all nodes. Replay 0 in 12 of 12 (A = C), 0 forced, digests and every state-hash checkpoint agree. SIGTERM -> exit step 2 7.0 / 10.7 s (p95 10.2), control 3.7 / 8.8 s; drain ~0.3 s per queued block; flush + WAL sync 0.55 vs 0.41 s; kill -> first chain commit step 2 24.3 / 59.8 s, control 15.9 / 21.9 s. rc 2 = crash-gate INFO-line false FAIL + liveness FAIL from 56-75 s chain stalls (gossipsub resubscribe after the in-place restart, both arms, watchdog reconnect at 70-80 s in 9 of 12 restarts) | **SIGTERM shutdown correct on step 2 (replay 0, nothing forced); restart gate met (max 59.8 s, set by the gossipsub stall).** 1 of 8 step 2 exits passed Docker's 10 s grace: set `stop_grace_period` >= 60 s. Resubscribe stall (section 46 Open item 2) still open |
| 58 | Phase 3 soak (owner via 18c s112: fixed 600 s per arm on the section 51 allocator, no extensions): RSS per node at 5 / 10 min, last-5-min slope with 95% CI, Phase 3 delta at 10 min, matched/s and CPU-s/1M over the last 5 min, load-generator health, matched/s drift | Campaign `ozarchy-p3soak` (results doc section 53): step 2 `7b006739` (`ca388315`, 15 s) vs main `219f0c20` (`987f4eb6`), glibc, commit tail, bench `05d44313`, harness `218ad251`; w0 + k1-ctl + k1-s2 (600 s); 42 CHECK OK, no memguard stop; k1-s2 REJECT = mark-only digest artifact. RSS @10 min main 3.80 / 3.95 / 4.77 GB, step 2 6.07 / 5.93 / 6.44 GB (delta +1.98 GB mean); last-5-min slope main +3.3 [-4.2, +10.8] / +11.1 [+5.5, +16.7] / +6.4 [+2.3, +10.5], step 2 +63.1 [+14.6, +111.6] / +67.3 [+47.1, +87.4] / +39.8 [+19.1, +60.5] MB/min; matched/s 168k / 173k whole, 140k / 142k last 5 min, drift about -10k/s per min in both; CPU-s/1M 30.35 / 28.47 whole, 36.35 / 35.00 last 5 min; nonce-expired evictions ~18k per node in both arms from minute 2; bench tail errors 0 | **GROWTH FINDING: step 2 RSS does not plateau within 10 min (+40-67 MB/min per node at the end; main +3-11).** The soak load degrades by itself in both arms (cancel-all expiry reproduces on main past ~2 min); 18c investigates |
| 59 | Phase 3 soak on jemalloc (owner queue item 1 via 18c: the row 58 pair on jemalloc; flat RSS = allocator retention, still growing = data growth; RAM rule: levels off and delta <= ~1 GB/node) + per node per minute RSS next to the layer / checkpoint metrics, resting orders, flush-worker depth, handoff wait, drop_inline for both soaks + the minute-6 matched/s drop: generator or node | Campaign `ozarchy-p3soakje` (results doc section 53b): step 2 `7b006739` (`ca388315`, 15 s) vs main `219f0c20` (`987f4eb6`), system jemalloc 5.3.1 via LD_PRELOAD (defaults, no profiling), commit tail, bench `05d44313`, harness `218ad251`; w0 + k1-ctl + k1-s2 (600 s); 42 CHECK OK, no memguard stop; k1-ctl REJECT = mark-only digest artifact. RSS mean minutes 5-10 vs 2-5 step 2 -133 / -139 / -27 MB (glibc +349 / +329 / +364), main -79 / +45 / +33; last-5-min slope step 2 +15.3 [-20.6, +51.2] / +32.7 [+2.7, +62.7] / +27.9 [-5.6, +61.5] MB/min (jemalloc decay swings); Phase 3 delta +0.68 GB minutes 5-10 mean, +0.69 GB @10 min, +1.01 GB peak (glibc +1.86 / +1.98 / +2.00). Layer per checkpoint 70-140 MB after minute 2, no trend; resting orders 0.2-0.6 M, falling after minute 5; flush-worker depth 0.0-0.1 on step 2; RSS tracks neither. From minute 6, 105-155k orders/s margin-rejected in all four cells while submits, processed actions and node CPU hold or rise; expiry starts at minute 3 | **Section 53's growth is consistent with glibc retention (one pair; slow growth not ruled out, val1 last-5-min slope CI excludes 0): under jemalloc step 2's RSS levels off by ~2 min, delta +0.68 GB per node (peak +1.01). RAM rule met on the level and the mean, borderline at the peak.** The minute-6 matched/s drop is margin exhaustion in the econ workload, not the generator or the node |
| 60 | Phase 3 soak with the cancel-share fix (owner queue item 2 via 18c: cs2 = `test/p3-cancel-share` `41a95355` (`fix/cancel-share-oldest-first` `de97682f` merged onto step 2) vs step 2 `7b006739`, both 15 s, glibc, commit tail, fixed 600 s; does the fix stop the cancel-all expiry and book growth, does step 2 RSS then level off; plus 18c's margin-reject split by sender and reason) | Campaign `ozarchy-p3cssoak` (results doc section 53c): cs2 `2655ab56` (built clean in `wt/p3q-cs2-41a95355`), step 2 `ca388315`, bench `05d44313`, harness `218ad251`; w0 + k1-g2 + k1-cs2 (600 s); 42 CHECK OK, no memguard stop; k1-g2 REJECT = mark-only digest artifact (val2 51.6 s vs 34 s). Cancel-all expiry per node cs2 0 / 0 / 0 (199,812 of 199,813 executed; the 1 RPC-refused) vs step 2 16,168; margin-rejected orders cs2 0 vs step 2 37k/s (minute 6) -> 107-134k/s (7-10); matched/s minutes 6-10 182k vs 144k; resting orders cs2 0.64-0.73 M vs 0.15-0.36 M, layer per checkpoint 212 vs 108-112 MB. RSS last-5-min slope cs2 +129.3 [+112.9, +145.7] / +178.0 [+160.0, +196.1] / +137.6 [+121.8, +153.5] MB/min, step 2 +40.8 [+33.7, +48.0] / +42.0 [+16.9, +67.1] / +84.5 [+69.5, +99.5]; Phase 3 delta vs section 53 main (min 5-10 mean / @10 / peak) cs2 +2.76 / +3.07 / +3.14 GB, step 2 +1.85 / +1.93 / +1.99. Margin split: no per-reason or per-sender reject data exists (one chain-wide counter; instrumentation in 53c.4); end-of-run digest (50 senders, 6 soak cells): order margin <= 0.5% of account value everywhere, exhausted senders at 96-100% position IM, position IM by address quartile 74-88 / .. / 6-9% under the old cancel order vs 35-39% flat with the fix | **The fix removes the cancel-all expiry (0 vs 16k per node), and with it the minute-6 margin collapse; the margin user is positions, concentrated on low-address senders by the old cancel order, not reservations.** Step 2 + fix RSS under glibc does not level off in 10 min (+2.76 GB vs main), but against a confounded reference (main ran the collapsed workload); next: cs2 vs main + fix `de97682f` |
| 61 | Fair Phase 3 RAM soak with the cancel-share fix in every arm (owner go via 18c: main + fix `de97682f` (mf, glibc, no knob, control) vs step 2 + fix `41a95355` on glibc (cs2) and on jemalloc 5.3.1 (jcs2, as 53b's jg2), 15 s, commit tail, fixed 600 s; per node per minute RSS / matched/s / expiries / margin rejections / resting / flush depth / handoff / layer bytes / drop_inline / pool / select; matched/s minutes 0-2 vs 6-10 for every soak cell; RAM rule: levels off and delta <= ~1 GB/node) | Campaign `ozarchy-p3fsoak` (results doc section 53d): mf `e4b34063` (staged `cs-run`, build id `90c6f729`; fix symbols present, absent in main / step 2), cs2 `2655ab56`, jcs2 = cs2 via LD_PRELOAD wrapper `ozarchy-p3q-stage/cs2-je` (`MALLOC_CONF` unset), bench `05d44313`, harness `218ad251`; w0 + k1-mf + k1-cs2 + k1-jcs2 (600 s); all 4 rc 0 ACCEPT AGREE, 56 CHECK OK, no memguard stop. Expiry 0 and margin rejections 0 in every cell; resting orders 0.54-0.76 M per node from minute 3 in all arms; layer 204-209 MB per checkpoint (cs2 / jcs2). RSS last-5-min slope mf +2.5 [-6.1, +11.0] / +4.7 [-2.0, +11.4] / +26.2 [+17.7, +34.7], cs2 +142.4 [+128.9, +155.9] / +124.4 [+112.3, +136.5] / +181.3 [+161.9, +200.8], jcs2 +10.0 [-35.7, +55.7] / -9.6 [-71.3, +52.2] / +11.2 [-45.3, +67.8] MB/min; delta (min 5-10 mean / @10 / peak, mean of 3) cs2 - mf +2.62 / +2.92 / +3.02 GB (difference slope +120 to +158 MB/min, CIs exclude 0), jcs2 - mf +1.10 / +1.16 / +1.49 GB (difference slope -18 to +10, CIs span 0; val2, equal exec queue depth, +0.95 / +0.91 / +1.34). matched/s minutes 0-2 -> 6-10: mf -12.6%, cs2 -10.8%, jcs2 +2.7% (sections 53 / 53b / 53c without the fix -22 to -34%, 53c cs2 -9.5%). No margin reject reason split (18c's counters at `native_executor.rs:7218` / `:9418` postdate these binaries; 0 rejections anyway) | **Glibc: RAM rule not met (Phase 3 +2.62 GB per node and still growing against a levelled control). jemalloc: levels off at +1.10 GB per node mean (+1.49 peak): met on the level, borderline on the mean, not at the peak; the delta also holds the allocator and val0 / val1's exec backlog.** Same book and layer, glibc grows, jemalloc flat: consistent with glibc retention, not data growth (one cell per arm). Allocator / gate decision is the owner's |
| 62 | Phase 3 merge-wait A/B (owner-approved queue item via 18c: `perf/p3-merge-wait` `3cf8f72d` (mw: exec joins only the small part of the previous layer merge; positions / balances merge into a deferred part during the next block) vs step 2 `7b006739` (g2), both 15 s, glibc, commit tail, n = 4 ABBA, 120 s + 60 s warm-up; native blocks/s, block time p99, settle wait per block, the new deferred read waits / deferred join, block_ms, RSS, matched/s, CPU-s/1M, writes) | Campaign `ozarchy-p3mw` (results doc section 54): mw `8bb0dc8a` (staged `ozarchy-p3q-stage/mw-run`), g2 `ca388315`, bench `05d44313`, harness `218ad251`; smoke s0-mw + w0 + r1-r4 (ABBA); 126 CHECK OK; r3-mw, r4-mw, r4-g2 REJECT = mark-only digest artifact (slow digest node), the other 5 ACCEPT AGREE. Paired mw - g2 (95% CI, t(3)): `checkpoint_settle_wait` 23.36 -> 3.09 ms per native block (-20.27 [-20.92, -19.62]; p99 per observation 95 -> 16 ms); `deferred_read_waits` 0 on all 12 node-cells; `deferred_join` 0.19 ms per native block (0.13 per observation; inside upkeep, upkeep 1.16 -> 1.23 ms, tie); exec `block_ms` 261.8 -> 243.8 (-6.9% [-11.3, -2.5]); native blocks/s 3.51 -> 3.90 (+11.3% [+8.4, +14.1]); block time p99 1,321 -> 1,282 ms (-2.9% [-10.2, +4.3]), p50 -12.7% [-15.5, -10.0]; matched/s (minutes 0-2) 213,819 -> 217,658 (+1.8% [-1.9, +5.4]); fills per native block -4.4% [-9.4, +0.6]; CPU-s/1M 23.29 -> 23.61 (+1.35% [+0.09, +2.61]; equal fills +1.1% [-3.0, +5.3]); RSS peak / mean 5.67 / 4.64 -> 5.56 / 4.53 GB (-0.12 [-0.35, +0.12] / -0.11 [-0.34, +0.12]); writes 65.1 -> 66.8 MB/s per validator (+2.6% [-0.3, +5.5]) | **Exec no longer waits for the whole previous merge: settle wait -87%, no deferred read waits, join ~0.2 ms; +11% native blocks/s at -7% block_ms, RSS and writes neutral.** matched/s and block time p99 tie (smaller blocks, exec still the ceiling); CPU-s/1M +1.35% possible cost, not resolved at equal fills. Merge decision is the owner's |
| 63 | Phase 3 checkpoint interval A/B (owner-approved queue item "interval": the same step 2 binary `7b006739` at `TORUS_CHECKPOINT_INTERVAL_MS=5000` (g5) vs `15000` (g2), glibc, commit tail, n = 4 ABBA, 120 s + 60 s warm-up; does 5 s cut the Phase 3 RSS, at what CPU / write / blocks/s cost; RSS peak / mean, CPU-s/1M, device writes and WAL, native blocks/s, matched/s, settle wait, checkpoint count, layer bytes per checkpoint, block time p99) | Campaign `ozarchy-p3iv` (results doc section 55): one node `ca388315` (staged `ozarchy-p3q-stage/g2-run`), bench `05d44313`, harness `218ad251`; driver `ozarchy-p3iv-campaign.sh` CAMP=I (per-arm knob + enable-line interval check); smoke s0-g5 + w0 + r1-r4 (ABBA); 135 CHECK OK, interval confirmed in every cell (enable line `Some(15s)` / `Some(5s)` on all 3 nodes; in-bench median checkpoint gap 14.5 s vs 6.2 s, 4.4 vs 11.6 checkpoints per node per min); r1-g2, r2-g2, r4-g2 REJECT = mark-only digest artifact, the other 5 ACCEPT AGREE. Paired g5 - g2 (95% CI, t(3)): RSS peak / mean 5.68 / 4.61 -> 5.35 / 4.34 GB (-0.33 [-0.65, -0.01] / -0.28 [-0.50, -0.06]; window anon -0.36 [-0.53, -0.19], HWM -0.47 [-0.72, -0.21]); RSS growth 1.55 vs 1.54 GB/min (tie); layer per checkpoint 114 -> 74 MB (-35%), layer bytes per minute +58% [+54, +62] (bench window); CPU-s/1M 23.36 -> 23.61 (+1.10% [+0.44, +1.75]; equal fills +1.03% [+0.36, +1.70]); device writes 64.7 -> 71.7 MB/s per validator (+10.8% [+7.0, +14.6]), WAL / foreground +13.6% [+10.5, +16.6], RocksDB background +6.0% [-2.5, +14.6]; flush-worker checkpoint time +52%; settle wait 23.2 -> 21.8 ms per native block (-1.40 [-2.48, -0.32]); upkeep 1.14 -> 3.70 ms (+2.57 [+2.21, +2.93]); native blocks/s +0.2% [-2.6, +2.9]; matched/s (minutes 0-2) -0.9% [-2.5, +0.6]; block time p99 1,293 -> 1,233 ms (-4.7% [-20.6, +11.3]) | **5 s cuts RSS by ~0.3 GB per node (~6%) in minutes 0-2, about a fifth to a quarter of the 120 s step 2 - main delta (section 51), for +1.1% CPU-s/1M and +11% writes; throughput and block time tie.** Growth rate unchanged, so no sign it fixes the glibc soak growth (section 53d); a soak would have to show the plateau. Interval choice is the owner's |
| 64 | jemalloc compiled in, part A of the jemalloc batch (owner-approved, 18c plan): `feat/jemalloc-allocator` `88ddc4c2` (ja, tikv-jemallocator, default feature `jemalloc`, `torus_jemalloc_*` gauges) vs its parent main `deeacdb3` (jb, glibc), no LD_PRELOAD, commit tail, n = 4 ABBA, 120 s + 60 s warm-up, in two modes: SAT (harness RATE 76000, matched/s ceiling) and FIX (fixed offered load RATE 907 = 75% of the jb SAT submit rate, per-cell submit-rate check within 2%); matched/s, CPU-s/1M, RSS peak / mean / growth, blocks/s, block time p99, exec `block_ms`, writes, mempool wait and exec lag, jemalloc gauges | Campaign `ozarchy-p3je` (results doc section 56): nodes `f2fbe17b` / `e901fd1d` built fresh (`ozarchy-p3je-build.sh`, stage `ozarchy-p3je-stage`, + part B's jcs `e7be8460` `4521dd5f` and jmf `0360c840` `9d6e57c1`, not run), bench `05d44313`, harness `218ad251`; driver `ozarchy-p3je-campaign.sh` CAMP=SAT|FIX (dynsym allocator preflight, per-cell `jechk` gauge check, `ratechk`); 2 x 144 CHECK OK, allocator verified in every cell; SAT r4-ja, FIX r1-ja / r3-jb REJECT = mark-only digest artifact. FIX submit check: all 9 cells within 2% (ja -0.09..+0.03%, jb -1.05..-1.67%). Paired ja - jb (95% CI t(3)), SAT: matched/s -0.3% [-1.5, +0.9] (tie), submits +6.3% [+1.8, +10.9], blocks/s +14.9% [+3.2, +26.6], block time p99 -7.2% [-12.6, -1.9], `block_ms` -14.3% [-25.2, -3.4], CPU-s/1M +2.66% [+1.83, +3.49], RSS peak tie / mean -3.8% [-4.6, -2.9], growth -44% [-69, -19], writes +5.0% [+1.1, +8.9]. FIX: CPU-s/1M +4.26% [+3.26, +5.25] (equal fills +4.62% [+2.41, +6.82]), RSS peak -0.14 GB [-0.18, -0.10] / mean -0.22 GB [-0.30, -0.14] / last 15 s -0.36 GB [-0.43, -0.29], growth 1.05 -> 0.49 GB/min, blocks/s +10.2% [+6.9, +13.5], block time p99 -8.8% [-16.3, -1.3], `block_ms` -10.6% [-14.4, -6.9], in-window matched/s -2.2% [-3.5, -0.9] (exec still saturated at 907 in both arms: backlog 44 / 55 blocks), mempool wait -37%, exec lag +10%, writes +4.4%. ja gauges: allocated ~2.0-2.1, resident ~3.0-3.3, retained ~2.5-2.6 GB per node. Minutes 0-2 only | **jemalloc buys lower RSS growth and faster consensus, not throughput, at +2.7% (saturated) / +4.3% (fixed load) CPU per fill.** The RAM-gate question under jemalloc goes to part B (10 min soaks ja / jb, jcs / jmf); allocator choice is the owner's |
| 65 | jemalloc batch part B (owner-approved, 18c plan): four 10 min soaks, full workload, commit tail, saturated (RATE 76000), allocator compiled in (no LD_PRELOAD): RAM gate je vs je = jcs `test/p3-cs-je` `e7be8460` (step 2 + fix + jemalloc, 15 s) - jmf `test/main-fix-je` `0360c840` (main + fix + jemalloc, no knob), the pair differing only by Phase 3 (owner rule: RSS levels off and delta <= ~1 GB/node); secondary ja `88ddc4c2` vs jb `deeacdb3` (no fix): RSS slope / plateau and matched/s over time; per node per minute RSS, `torus_jemalloc_*` allocated / resident / retained, matched/s, resting, layer bytes, flush depth, exec busy / ms per block / queue depth, expiries, margin rejections | Campaign `ozarchy-p3jesoak` (results doc section 57): binaries from part A's stage (jmf `9d6e57c1`, jcs `4521dd5f`, ja `f2fbe17b`, jb `e901fd1d`; malloc defined in `.dynsym` on the 3 je nodes, UND on jb; gauges on every je node), bench `05d44313`, harness `218ad251`; driver `ozarchy-p3jesoak-campaign.sh` CAMP=SOAK; jcs smoke 60 s: enable line + 11 checkpoints per node. Cells w0, k1-jmf, k1-jcs, k1-jb rc 0 ACCEPT AGREE; k1-ja REJECT = mark-only digest artifact; 80 CHECK OK. **jcs - jmf per node: +0.94 GB minutes 5-10 mean (val0 / val1 / val2 +0.98 / +0.98 / +0.86), +1.00 at 10 min (+0.89 / +1.32 / +0.78), +1.27 peak (+1.21 / +1.55 / +1.05)**; last-5-min slope of the difference +43 [-4, +90] / -80 [-134, -27] / +8 [-49, +65] MB/min: levels off. Exec queue at the ~66 cap in both arms on every node (58-65 blocks), so no backlog or allocator confound (unlike 53d). Split: je allocated +0.51-0.57 GB (live; memtables -50-60 MB), resident - allocated +0.40-0.49 GB (allocator pages), non-heap -0.01 to -0.07. Layer 203 MB/checkpoint, 47-48 checkpoints, 0 expiries / 0 margin rejections in jmf and jcs. matched/s min 0-2 vs 6-10: jmf -4.1%, jcs +4.4% (whole window jcs +11.3% vs jmf, exec ms/block -10%; not attributed). ja vs jb (no fix): expiries from 152-154 s (11,354 / 17,357 per node), margin rejections from minute 6 (31.1 M / 27.4 M per node); RSS ja - jb +0.15 / +0.06 / +0.39 GB (mean / @10 / peak), both still growing (+38-52 / +7-39 MB/min), ja's exec queue at the cap vs ~2 on jb val0 / val1; matched/s 6-10 vs 0-2 ja -19.9%, jb -35.1%. One cell per arm | **RAM rule under jemalloc: met on the level and the mean (0.86-0.98 GB per node), at the bound at 10 min, not met at the peak (1.05-1.55 GB).** About half live Phase 3 data, half jemalloc-resident slack. The merge call is the owner's |
| 66 | Gossipsub resubscribe-wedge fix bench (owner-approved via 18c, after section 52's 56-75 s stalls / Open item 2): `fix/mesh-watchdog-resubscribe` `e3bb7cca` (wf: no ghost connections, stale-connection kick, watchdog 5 s / 1 s, new `torus_mesh_stale_conn_disconnects` + `torus_consensus_subscribe_delay_seconds`) vs its base main `925fd962` (wm); (1) section 52-style kill cells with SIGKILL, same-key in-place restart, n = 4 ABBA; (2) saturated 120 s A/B, n = 4 ABBA, no kill | Campaign `ozarchy-wedge` (results doc section 58): nodes wm `ea4445ca` / wf `a16b9ad2` (fresh target dirs), glibc, no Phase 3, mesh knobs unset, bench `05d44313`, harness `218ad251`, commit tail. Kill: 540 s cells, SIGKILL val1 at +60/180/300/420 s, 16 kills per arm (val2 kills dropped: the commit tail reads val2's RPC; r2-wf val2 kept as an extra). No wedge in either arm: survivors drop the dead QUIC connection at 10.0 s, the restarted node's p2p starts 11.2-29.7 s after the kill (DB open + replay), 0 ghost connections in 64 survivor-kill pairs. Longest chain gap wm 19.7 / 29.6 s, wf 18.6 / 27.2 s (mean / max), paired wf - wm -1.1 s [-3.7, +1.4]; NoPeersSubscribed 9 / 4 lines; watchdog 0 / 0, stale kicks 0; subscribe delay 32 obs. <= 50 ms. rc 2 = crash-gate INFO-line false FAIL; 4 of 8 mark-only digest DISAGREE (restarted val1, both arms); state-hash checkpoints all equal. SAT: all ACCEPT AGREE; matched/s -0.86% [-2.42, +0.69], CPU-s/1M -0.39% [-1.79, +1.02], blocks/s -0.42% [-9.1, +8.3], commit p99 +1.2% [-10.9, +13.4]; mesh counters and publish failures 0 | **No regression (SAT, kill restarts, no false kicks). Benefit not measured: SIGKILL + replay brings p2p back after the 10 s QUIC drop, so main does not wedge either.** Wedge precondition = p2p back within ~10 s (section 52 SIGTERM, section 46 y1-s-KILL at 8.1-8.8 s). A benefit bench needs SIGTERM cells or a short-replay SIGKILL restart; owner's call |
| 67 | Cloid cost A/B (requested by 18c, s116, optional filler): what do client order ids (`feat/cloid` `46a11c14`, u128, unique per trader via `CloidMarkets` / `BookCloids`) cost per order? One binary, main `ef812b02` (node + bench built together: cloid changed the signed-order format), glibc (no jemalloc feature in `ef812b02`), arms differ only in run-cell.sh `CLOID_SHARE`: c0 = 0 (no cloids, byte-identical actions) vs c100 = 100 (every placed order stamped); saturated (RATE 76000), commit tail, n = 4 ABBA, 120 s + 60 s warm-up; matched/s, placed/s, CPU-s/1M, exec `block_ms` / p99, per-phase ms (verify, engine), RSS, writes, cloid share and duplicate-cloid checks | Campaign `ozarchy-p3cloid` (results doc section 59): node `6f363582` + bench `9fda6c84` built fresh (`ozarchy-p3cloid-build.sh`, stage `ozarchy-p3cloid-stage/cl-run`), harness = the `ef812b02` worktree (`CLOID_SHARE`, `ingest.cloids`); driver `ozarchy-p3cloid-campaign.sh` CAMP=CLOID (per-cell `cloidchk`); smoke s0-c100 + w0 + r1-r4 (ABBA); 135 CHECK OK; all 10 cells rc 0 ACCEPT AGREE (no mark-only digest REJECT). Cloid checks: stamped / placed exactly 0 / 1.0000, `share_pct` 0 / 100, `duplicate_cloid` rejects 0 on all 3 nodes in every cell. Paired c100 - c0 (95% CI t(3)): matched/s 188,654 -> 166,551 (-11.72% [-13.83, -9.60], all 4 rounds -19k to -25k), placed/s -11.75% [-13.91, -9.59], submits -6.85% [-9.60, -4.09] (closed loop), native blocks/s -7.0% [-15.2, +1.2] (n.s.); exec `block_ms` 262.3 -> 293.3 (+11.84% [+1.72, +21.97]), exec per native block p99 720 -> 856 ms (+18.9% [+9.4, +28.4]), block time p99 +12.1% [+2.7, +21.5]; engine ms per 1k fills 4.12 -> 4.69 (+13.9% [+10.6, +17.2]); per native block: verify 11.48 -> 14.30 ms (+2.82 [+1.78, +3.86], +25%), engine +26.9 [+5.1, +48.7], residual +4.7 [+4.1, +5.3], end_resident wait -3.4 [-4.5, -2.3]; CPU-s/1M 26.43 -> 28.77 (+8.84% [+7.37, +10.31]; equal fills +6.37% [+3.05, +9.70]); RSS peak / mean 4.25 / 3.62 -> 4.57 / 3.86 GB (+0.32 [+0.14, +0.49] / +0.24 [+0.13, +0.35]); writes +1.8% [-4.0, +7.7] (flat); exec busy 0.952 / 0.955, mempool equally full (node-bound in both arms). Minutes 0-2 only | **At 100% share cloids cost ~12% of matched/s and ~9% CPU per fill (6% at equal fills), all in executor per-order time (engine ~87% of the extra `block_ms`, verify ~9%).** 100% is the worst case; the cost at a realistic share is not measured and linear scaling is not shown (a c25 / c50 arm would). glibc only: the cost under compiled-in jemalloc (main default since `655e874a`) is not measured |
| 68 | Cloid engine cost by perf profile (requested by 18c after section 59: where inside the engine does the +13.9% per 1k fills go?): same binary and arms as row 67 (kept p3cloid stage, main `ef812b02`, node `6f363582` with symbols, not rebuilt; glibc; c0 = `CLOID_SHARE=0` vs c100 = `CLOID_SHARE=100`), same shape (saturated, commit tail, 120 s + 60 s warm-up), w0 + 3 ABBA rounds, perf user space (`perf_event_paranoid=2`) on val1 in every cell: W1 `cycles:u` fp call graphs, whole process; W2 exec thread only, 5 events incl. DRAM demand fills (`LLC-load-misses` unsupported on Zen 3); attribution per thread group (exec thread / market workers / other) and per cloid site (index upkeep, duplicate check, cleanup) | Campaign `ozarchy-p3cloidprof` (results doc section 60): driver `ozarchy-p3cloidprof-campaign.sh` CAMP=CLOID + `cloidprof-sidecar.sh`; 105 CHECK OK; all 7 cells rc 0 ACCEPT AGREE, cloid checks as in 59 (stamped / placed 0 / 1.0000, `duplicate_cloid` 0 everywhere). Whole cell, paired c100 - c0 (n = 3, 95% CI t(2)): matched/s -12.53% [-21.55, -3.50], engine ms per 1k fills +13.10% [+2.81, +23.40], CPU-s/1M +8.94% [+3.44, +14.44], exec p99 +18.2% [+10.3, +26.1] (all consistent with 59; val1's W1 engine ~4% above its whole-cell value in both arms, not a controlled perf-overhead measurement). Profile (W1, ms CPU per 1k fills): engine threads +1.00 (exec +0.71 = +16%, workers +0.29); by site index upkeep +0.36 (`CloidMarkets::absorb` -> `drain_cloid_log` +0.32, `index_cloid` +0.04), duplicate check +0.36 (pre-pass claimed set +0.14 incl. `reserve_rehash` 0.06, `duplicate_cloid` -> `CloidMarkets::live` +0.14, per-book `contains_key` `order_book.rs:1397-1402` +0.08), cleanup `forget_cloid` +0.14, non-cloid +0.14; 93% of the exec delta at cloid sites; hashing ~27% of it; no lock frames. Engine timers: `engine_untimed` 0.21 -> 0.78 ms per 1k fills (the exec-thread sites run outside the phase timers). W2: exec DRAM demand fills +12.7% per fill, all at cloid sites; IPC index 0.54 / dup check 1.01 / cleanup 0.76 vs 1.18 (c0 thread). Outside the engine: Keccak +0.91 (callers unknown, no fp chain), `verify_one_action_with` +0.80, RocksDB background +0.82. All 9 of 18c's frame / site hypotheses confirmed (`reserve_rehash` small) | **The cloid engine cost is per-order hash-map work: index upkeep (drain into `CloidMarkets`, cache-bound) and the serial duplicate pre-pass on the exec thread, the per-book check / `index_cloid` / `forget_cloid` in the workers.** Possible levers (suggestions, not measured): pre-size the claimed set, a smaller key or cheaper hash for the cloid maps, batch or move the absorb / drain work. 100% share, val1 only, user space only, minutes 0-2 |
| 69 | FINAL Phase 3 gate (18c): s2 = `feat/item6-phase3-step2` `c88d3073` (step 1 + step 2 + merge-wait `91a10875`, merged with main `de1a97c1`) at `TORUS_CHECKPOINT_INTERVAL_MS=15000` vs m = main `de1a97c1` (its main parent), knob unset; node and bench built per arm in fresh target dirs, jemalloc compiled in both, CLOID_SHARE=0, full workload (cancel-share fix on main), commit tail; w0 + n = 4 ABBA 120 s SAT, one 600 s soak per arm, one 300 s SIGTERM restart cell per arm (val1 at bench+180 s); all Phase 3 gates: CPU-s/1M, writes, matched/s + native blocks/s, restart <= 60 s, RAM levels off and delta <= ~1 GB/node; checkpoint settle wait, deferred join, exec settle phase | Campaign `ozarchy-p3gate` (results doc section 61): nodes s2 `4e0d8b84` / m `f1081574`, benches `ae96e0c1` / `42365459` (`ozarchy-p3gate-build.sh`, stage `ozarchy-p3gate-stage`; nm 1,168 jemalloc symbols each, no LD_PRELOAD), harness = the `c88d3073` worktree, crash-kill.sh from p3term; driver `ozarchy-p3gate-campaign.sh` CAMP=GATE; smoke s0-s2-TERM + s0-m; cells 16:10-17:51; 225 CHECK OK (+36 smoke), no CHECK FAIL. 12 cells ACCEPT AGREE; t1-s2-TERM / smoke s0-s2-TERM REJECT AGREE = crash-gate match on the INFO line `running state hash fail-stop ... on=false`; t1-m-TERM REJECT DISAGREE = the same + mark-only digest artifact (val0, block hash equal). SAT paired s2 - m (95% CI t(3)): CPU-s/1M 26.51 -> 23.94 (-9.72% [-10.46, -8.98]; equal fills -8.39% [-9.26, -7.51]); device writes 159.0 -> 71.9 MB/s per validator (-54.8% [-56.9, -52.7]); matched/s 198,189 -> 215,590 (+8.78% [+5.17, +12.39]); native blocks/s 3.82 -> 4.58 (+20.1% [+10.3, +30.0]); engine ms per 1k fills -10.4%; exec `block_ms` -14.4% [-20.6, -8.3]; block time p99 1,138 -> 1,340 ms (+17.7% [+6.7, +28.7]), p50 -32.7%; RSS mean / peak 3.69 / 4.63 -> 4.68 / 5.91 GB (+0.99 [+0.67, +1.30] / +1.28 [+0.82, +1.74]). s2 checkpoint settle wait 3.1 ms per native block (metrics basis; 3.5 summary phase), deferred join p50 0.0 / p99 0.0-3.2 ms per node, upkeep 0.84 ms; exec settle phase 49.0 vs 57.1 ms per native block. Soak (one cell per arm, 0 expiries / margin rejections, exec queue at the cap in both): matched/s 193,398 -> 219,139 (+13.3%), CPU-s/1M -12.4%, writes 231.6 -> 82.9 MB/s (-64%), block time p99 +23%; s2 RSS last-5-min slopes -76.8 [-137.6, -15.9] / -65.9 [-120.3, -11.5] / -10.2 [-59.8, +39.5] MB/min, difference slopes -53 / -44 / -34 (CIs include 0); **s2 - m +1.43 GB per node minutes 5-10 mean (+1.57 / +1.20 / +1.53), +1.31 at 10 min, +1.80 peak (val0 +2.04)**; je allocated +0.50 / +0.54 / +0.56 GB (section 57: +0.51-0.57), resident - allocated +0.67 to +1.18 GB, non-heap -0.11 to +0.01; 49 checkpoints, layer 199 MB; settle wait 8.8 ms per native block. SIGTERM restart: s2 exit 19.1 s (61 queued blocks drained), up -> first chain commit 16.0 s, kill -> first chain commit 35.0 s; m 21.5 / 15.8 / 37.3 s; replay 0, not forced, one kill per arm; no SIGKILL / multi-crash cell (last SIGKILL evidence: section 46, step 1). Hardscan: 0 HARD FINDING lines in all cells (archived `a-term.py` fixed to read p3gate's log and write its own kills.json; the analyst copy read / overwrote p3term's) | **Gates: CPU, disk, throughput, restart (SIGTERM only) and RAM levels off PASS; RAM size (<= ~1 GB/node) FAIL in the soak (+1.43 GB mean, +1.80 peak; SAT +0.99 mean).** The live Phase 3 heap is +0.50-0.56 GB (as in section 57) and does not grow; the rest is jemalloc-resident slack (allocator retention, not a leak; n = 1 soak per arm, s2 +13% matched/s). Block time p99 +18-23% (not a gate). The decision is the owner's (pending); suggestions only: judge on the live heap, jemalloc decay tuning + soak rerun, n = 2-3 soaks |
| 70 | Phase 3 SIGKILL restart gate + jemalloc decay soak pair (18c after section 61; owner s118: RAM judged on the live heap, Phase 3 merges once the SIGKILL restart gate passes): same staged binaries as row 69 (s2 `c88d3073` at `TORUS_CHECKPOINT_INTERVAL_MS=15000`, m `de1a97c1`, both jemalloc compiled in, not rebuilt), CLOID_SHARE=0, full workload, commit tail; Part D: 4 x 300 s cells ABBA (d1-s2 d1-m d2-m d2-s2), SIGKILL val1 at bench+60/180 (d1) and +65/187 s (d2), KILL_WAIT 60 s, state hash on, 1 s gauge samples + `torus_getStateHash` at every 100-block checkpoint; restart <= 60 s, replay from the marker, state-hash agreement. Part B: s2 600 s soak, jemalloc defaults vs `MALLOC_CONF=background_thread:true,dirty_decay_ms:1000` (RSS, je allocated / resident, CPU-s/1M, matched/s) | Campaign `ozarchy-p3kd` (results doc section 62): stage `ozarchy-p3gate-stage` (nodes `4e0d8b84` / `f1081574`, benches `ae96e0c1` / `42365459`), harness = the `c88d3073` worktree, p3gate's matched-bench copy; driver `ozarchy-p3kd-campaign.sh` CAMP=KD; smoke s0-m-KILL + s0-s2dec; cells 19:07-20:17; 134 CHECK OK (+38 smoke), no CHECK FAIL, 0 HARD FINDING. MALLOC_CONF proven (unprefixed tikv-jemallocator: stats_print `opt.background_thread: true`, `opt.dirty_decay_ms: 1000`; MALLOC_CONF in every node's environ; 4 `jemalloc_bg_thd` threads per node vs 0). rc 2 on the 4 kill cells = crash-gate match on the INFO line `running state hash fail-stop ... on=false` (+ s2 'rewind beyond the exec queue' = replay from the last checkpoint, by design); DISAGREE in d1-m-KILL, d2-s2-KILL and k1-s2dec = mark-only digest artifact (block hash equal). Kill -> val1 first commit: s2 22.28 / 28.38 / 29.48 / 39.47 s (mean 29.9, max 39.5), m 31.95 / 25.34 / 21.78 / 29.54 s (mean 27.2, max 31.95); replayed blocks s2 72 / 74 / 98 / 113 (marker A at the last 100-block checkpoint), m 65 / 61 / 64 / 62 (= exec queue); first replayed = A + 1, contiguous, A >= the sampled durable hash height in 8 of 8; replay 2.4-4.0 blocks/s (section 46: 13.1 / 5.1) at 221-246 txs per native block; DB open 1.8-4.0 s. State-hash checkpoints: 122 heights, 121 on all 3 validators, 0 disagreements. d2-m k2 kill -> chain 0.34 s = chain-measure artifact (val0 committed at val1's restart). Soak (one cell each): RSS per node min 5-10 mean 6.25 -> 4.53 GiB (-1.72; -1.13 / -1.94 / -2.07), peak 6.91 -> 5.19 (-1.71); je resident - allocated 2.35 -> 0.67, allocated 3.46 -> 3.49; slopes within +-15 MB/min; CPU-s/1M 24.47 -> 25.60 (+4.6%), matched/s -2.5%, native blocks/s +2.6%; vs p3gate main soak (cross-session): RSS -0.03 mean / +0.40 peak, CPU-s/1M -8.4%, matched/s +9.7%; default cell vs p3gate s2: CPU +0.1%, matched/s -0.7%, RSS +0.25 | **Restart gate (SIGKILL, multi-crash) PASS on `c88d3073`** (29.9 s mean / 39.5 s max, m 27.2 / 31.95; replay complete and in order, state hashes agree); with the owner's live-heap RAM decision every Phase 3 gate is met. Times ~2x section 46 from the replay rate at this workload (m equally slow). jemalloc decay setting: s2 soak RSS -1.72 GiB per node to main's level for +4.6% CPU-s/1M, -2.5% matched/s (n = 1 per arm, env setting only; a compiled default is a suggestion for 18c, not done) |

## 9. Owner decisions (s96, 2026-10-06)

Each question lists the chosen option and the options not chosen. The options not chosen
are kept because some may matter later. Impact is an estimate unless marked measured
(rule of thumb, section 1: about +0.4-0.55% matched/s per ms saved per native block).
"HL" = Hyperliquid's official docs (links per item); "not public" = HL does not document it.

### 9.1 Phase gate

**Chosen: keep +7%** (300 markets) against the Phase 2 base (9.6), with the step 0
checkpoint (section 1): below 13 ms of P2-1 + P2-2, the owner picks P2-5 or a lower gate.
Superseded by 9.16 (2026-10-09): the gate is lowered to the measured 1.0495x.

| option | needs (ms per native block) | note |
|---|---|---|
| **+7% (chosen)** | 13-17 | smallest gate clearly above the ~5% cell resolution |
| +5% | ~10 | a pass cannot be told from noise at ~5% resolution |
| +10% | ~20-24 | likely needs P2-5 too; relevant if step 0 puts P2-1 near its 18 ms ceiling |

### 9.2 CancelOrder / ModifyOrder by order id (P2-1b)

Today: `exec_cancel_order` probes every book twice (~600 map lookups, ~10-20 us per
cancel, estimate); `exec_modify_order` also loops over all books. Standard shape: 0 ms
(every cancel is a cancel-all). With traffic like HL's (makers cancel and modify by id):
~10-20 ms per 1,000 cancels per native block (estimate).

HL: `cancel {a, o}` and `cancelByCloid {asset, cloid}` name the asset; `modify` names
only the oid plus the new order (which carries the asset); `orderStatus` takes only user
+ oid; no user cancel-all, only `scheduleCancel` (>= 5 s ahead, 10 triggers per day).
Whether oids are global or per market: not public.
(hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api/exchange-endpoint,
.../api/info-endpoint)

**Chosen: C now (inside Phase 2, spec in P2-1b), B later as its own consensus item**
(owner s96). The owner first picked B; while writing this up (s96) it turned out that
order ids come from the global counter `next_global_order_id`
(`native_executor.rs:6760, 7131`) and are stored in book rows and results, so B changes
consensus-visible values (the id format, book state, receipts) and breaks Phase 2's rule
of no consensus or state format change (section 1). B (`OrderId` is u128, `MarketId` u64:
id = `market_id << 64 | sequence`) needs: every validator switching at one height (or a
fresh genesis), a rule for ids issued before the switch (fall back to the scan, or a
one-time map), and a check of clients that assume sequential ids (trading app, SDK). With
B, C's map becomes unnecessary and can be removed.

| option | saving | HL parity | consensus | note |
|---|---|---|---|---|
| A. market in the action (`{market, oid}`) | full (no lookup) | **same as HL** | action format change | SDK and app change; modify still needs B or C. Relevant if the action format is aligned with HL anyway |
| B. market in the id (later) | full; modify too | HL-like (cancel without naming the market) | **yes** (id format, state) | no action format change; separate consensus item, see above |
| **C. node-local id -> market map (chosen now)** | full minus one map insert + remove per resting order (~0.05-0.1 us each, est.) | HL-like | none | fits Phase 2's rule; more memory |
| D. nothing | 0 | gap | none | cost grows with cancel-by-id traffic |

Step 0.2 adds a cancel-by-id cell so the saving is measured before building.

### 9.3 Exec pool (P2-3)

Today ~62 thread spawns per native block from 7 sites; cost ~0.5-3 ms per native block
(estimate; step 0.2 measures). HL: not public; the docs say more cores make blocks
faster and recommend >= 32 cores
(hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api/optimizing-latency).

| option | block time (est.) | matched/s (est.) | note |
|---|---|---|---|
| **A. dedicated rayon pool (chosen)** | -0.5-3 ms, plus work stealing across uneven markets | +0.2-1.5% | rayon already a dependency; not the global pool (s352) |
| B. small std-only pool | -0.5-3 ms, no stealing | +0.2-1.3% | own code; relevant if rayon's scope overhead shows in step 0.2 |
| C. keep scoped spawns, merge the 7 sites | about half of A | +0.1-0.7% | smallest change; fallback if the per-site panic rules are hard to keep in a pool |

### 9.4 Liquidation sums with moving marks (P2-4)

Measured (walk 10 vs walk 0): sums re-value 6.5 -> 24.2 ms, liquidation 10.7 -> 24.7 ms per
block, matched/s 0.975x; so the bench ceiling is about +2.5% matched/s, and the cost sits in
oracle-only / empty blocks (row 78).

HL: mark price = median of (oracle + 150 s EMA of mid - oracle), (book bid / ask / last),
(external perp mids); oracle every 3 s; cross accounts liquidated below maintenance margin
x open notional. Check frequency and incremental vs full recompute: not public.
(hyperliquid.gitbook.io/hyperliquid-docs/trading/robust-price-indices, .../hypercore/oracle,
.../trading/margining, .../trading/liquidations)

| option | block time | matched/s | note |
|---|---|---|---|
| **A. design check now, code in Phase 4 (chosen)** | 0 now; later most of the 14-18 ms | 0 now, up to +2.5% later | Phase 4's one record per trader changes the layout, so code now would be redone; build in Phase 2 only if the check finds an exact rule worth >= 5 ms at walk 10 |
| B. all in Phases 4-5 | 0 now | 0 now | Gate 4 stays past its fail line (2.98 vs 2.0 ms/1k) until then |
| C. re-value only markets whose mark moved | up to 14-18 ms when few marks move; little when all move | 0 to +2.5% | bench moves all 300 each oracle round; HL's mark includes its own book, so it likely moves every block (inference). Relevant if real feeds update markets at different times |

### 9.5 Hasher (P2-5)

Measured (section 22): SipHash ~13% of execution self time (`write` 6.6%,
`hash_one<Address>` 3.5%, `hash_one<u128>` 3.1%); Keccak 5.5% is the state hash and stays.
HL: hash functions and data structures not public (node source not released,
github.com/hyperliquid-dex/node). HL's flood defence is per address: deposit before acting,
1 request per 1 USDC traded + 10k buffer, 1,000-5,000 open orders per user
(.../api/rate-limits-and-user-limits). We lack these admission limits; that gap matters
more than the hasher.

| option | block time (est.) | matched/s (est.) | flood-safe | note |
|---|---|---|---|---|
| A. keep SipHash | 0 | 0 | yes | |
| B. FxHash / raw address bytes | largest | +3-8% | **no** for user-chosen keys | keys are free to make, so colliding addresses are cheap to grind. Relevant for maps whose keys users cannot choose (`MarketId`, sequential `OrderId`) |
| **C. foldhash, seed per process (chosen)** | ~90% of B, 5-20 ms | +2-7% | yes | no new order risk (std is already seeded per process); P2-5, the step 0 fallback |

### 9.6 Base

Superseded by 9.7 (base `d3ba3c0a`); kept as the s96 record.

**Chosen (owner s96, on 18c's recommendation): main `35e69b3`** = the s94 batch
(`a0eda77`) + `feat/liq-telemetry` (`0ce261b`), both merged and pushed in s96. The +7% is
measured against `35e69b3` too, not `59fa407`: the batch (bad-debt, auth replay, gas
fixes) and the telemetry changed the code since Phase 1, and a `59fa407` reference would
mix their effect into Phase 2's. ozarchy's bad-debt cost cell (`35e69b3` vs `92a02ed`,
N=4 + b900, x2) gives the new base's Gate 2 reading. `59fa407` stays the Phase 1 record.

| option | note |
|---|---|
| **main `35e69b3` (chosen)** | has every merged fix; no merge conflicts later |
| main `59fa407` | lacks the batch fixes; conflicts in `native_executor.rs` / `app.rs` at merge |
| main `a3bfab2` (batch without telemetry) | relevant only if the telemetry shows a cost on the execution thread (ozarchy: native root identical; ms cost not measured yet) |

### 9.7 Base moved to main `d3ba3c0a` (18c + owner, s101 / ozarchy s27, 2026-10-08)

**Chosen: main `d3ba3c0a`** for the branch and for the +7% comparison (replaces `35e69b3`,
9.6). Main has merged since s96: the ADL budget (`a746c408`) and its dirty check, exact
cost basis, governance params, item 7 step 0 vote checks (`7c15b5aa`), read-precompile gas
and its follow-up (`d3ba3c0a`, book CF SST 4 MiB), the test-dir leak fix (`f7fe17f3`). A
`35e69b3` base would mix their effect into Phase 2's. Main `92a02ed` stays the Gate 2
reference. ozarchy builds step 0; who builds steps 1-4 is decided after Gate 0. Main moved on
to `35953ff8` (`fix/raise-nofile-limit`, `crates/torus-node` only) after the branch was cut;
it is merged into the branch later with a plain merge, not a rebase.

### 9.8 App-level differential: test-only cargo feature (option A) (2026-10-08, s27)

`#[cfg(test)]` switches work per crate, so torus-consensus tests cannot flip the torus-bridge /
torus-core reference paths (review log row 10).

**Chosen: A**, a test-only cargo feature on torus-bridge that turns on the reference paths
(full-scan cancel-all, per-row flush, and later the scoped spawns), enabled from
torus-consensus's dev-dependencies the way the existing `save-timings` feature is. This keeps
the app-level differential (CF dumps + `h_n`, serial and pipelined, all four BookModes). Built
at the start of step 1, before the cancel-all index.

| option | note |
|---|---|
| **A. test-only cargo feature on torus-bridge (chosen)** | keeps the app-level differential, serial and pipelined |
| B. bridge-level differentials only | loses the pipelined app-level check |

### 9.9 Step 0 checkpoint: P2-5 into the gate set (2026-10-08, s27 / s101)

Step 0 put P2-1 + P2-2 below 13 ms per native block (section 1; results doc 25.4).

**Chosen: option 1.** Pull P2-5 (hasher) into the gate set, keep the +7% gate, and build P2-5
right after P2-1. Caveat: s82's ahash A/B on the exec maps (`perf/s82-exec-hasher`,
`docs/perf/s82-exec-hasher-2026-09-30.md`) cut hash-map CPU from 3.06 to 1.88 CPU-s per 1M but
gave no matched/s gain, because the exec thread was not the bottleneck then. So the P2-5
microbench sets the ms estimate (~10.3 ms per native block on the standard cells, results doc
25.3), and a cell must confirm it before it counts toward the ~17 ms set (section 1). If the cell
does not show it, the miss goes into the review log (Phase 1 rule) and the work continues.

| option | note |
|---|---|
| **1. P2-5 into the gate set, keep +7% (chosen)** | set ~17 ms on the standard cells (estimate); P2-5 must be confirmed by a cell |
| 2. accept a lower gate | P2-1 + P2-2 alone ~7-10 ms, ~+3-5% (estimate); not chosen |

### 9.10 P2-3 (exec pool) to the backlog (2026-10-08, s27 / s101)

Step 0 numbers (results doc section 25): ~50 spawns per native block from three sites, 16.7 us
per spawn + join on ozarchy (C microbench, idle host), ~0.8 ms per native block of exec-thread
wall; the spawns are ~1% of the process sys CPU. At ~0.5% of engine time that is below cell
resolution, so its gate could not be measured, and a persistent pool adds lifetime, panic and
shutdown complexity (the per-site panic rules in section 3).

**Chosen: drop P2-3 from Phase 2 to the backlog.** Revisit when blocks get small (low load, or
after Phase 3), where a fixed per-block cost weighs more. The spawn counter
(`torus_exec_thread_spawns_<site>`) stays as the evidence.

| option | note |
|---|---|
| **backlog (chosen)** | no lifetime / panic / shutdown risk in Phase 2; counter kept |
| keep P2-3 in Phase 2 | ~0.8 ms (estimate), gate not measurable at ~5% cell resolution |

### 9.11 Base moved to main `e934fa0e` before step 1 (18c s104 / ozarchy s29, 2026-10-08)

**Chosen: merge main `e934fa0e` into `perf/item6-phase2` now, and measure the +7% gate against
`e934fa0e`** (replaces `d3ba3c0a`, 9.7). The branch had only step 0 since `d3ba3c0a` (counters,
bench columns, docs). Main has since merged the open-file limit raise (`35953ff8`), the C2
holder-index fix (`c8d25db8`) and the Position v2 savings (`b8e3b606`); main changed
`native_executor.rs` and `position.rs`, so Phase 2 is built and measured on the code it will ship
on instead of being merged at the end. `e934fa0e` already includes the two regression fixes
(results doc sections 27-29), so they do not count toward Phase 2. The gate campaign uses
`e934fa0e` as the reference arm, interleaved as usual. Plain merge, not a rebase (as 9.7).

| option | note |
|---|---|
| A. keep `d3ba3c0a`, merge main at the end | gate measures Phase 2 alone, but on code it will not ship on; late conflicts in `native_executor.rs` / `position.rs` |
| **B. merge `e934fa0e` now, reference `e934fa0e` (chosen)** | built and measured on the shipping code; the C2 / v2 fixes are in the reference, not in Phase 2's share |

### 9.12 P2-1 step 1 review: approach approved, index growth (18c s104 / ozarchy s31, 2026-10-08)

**Approved: step 1 as built (C1, review log row 19), with its deviations from section 3:** the
index also lists reduce-only entries (`cancel_all` drops them too); `Some(m)` cancel-alls go
through the index (the market is visited only if the index lists it); the index is built on first
use after a load instead of at the load, since "rebuilt == carried" is tested
(`cancel_all_index_rebuilt_at_load_equals_the_carried_one`, the app restart replica, row 20's
crash and stale-guard cases).

**(a) Index growth: keep "never removed eagerly" on the fill path; add the size gauge now; a
background prune before mainnet.** A (trader, market) entry leaves the index only at a cancel-all
of that trader there (or a restart). Many addresses resting one order in each of many markets
without ever cancelling-all is a memory-growth path (node-local, no consensus effect). Now: gauges
`torus_exec_cancel_all_index_entries` and `torus_exec_cancel_all_index_traders` (set once per
native block from a kept entry count, O(1); bench columns `cancel_all.index_entries_end` /
`index_traders_end`; row 21). Before mainnet: a background prune with fixed work per block (check
K traders per block in turn, drop the markets where they hold nothing), node-local (section 7).
Row 23 (2) for the prune: the entries gauge counts stale entries too. `torus_exec_resting_orders` is
only a rough scale for it, not the stale share: it counts orders, not (trader, market) pairs, and leaves
out pending stops (Codex review of `4456f22a`). The prune reports what it checked and removed (counters
next to the gauges: entries checked, entries removed); their ratio is the stale share.

| option | note |
|---|---|
| A. remove entries eagerly on the fill / cancel-by-id path | exact index, but per-fill work on the hot path P2-1 is meant to shrink, and every removal site must be found (a missed one is harmless, an extra one loses orders) |
| **B. keep "never removed eagerly", gauge now, fixed-work background prune before mainnet (chosen)** | no fill-path cost; growth visible now; the prune bounds memory with a fixed per-block cost |
| C. no prune, rely on cancel-alls and restarts | unbounded for traders who never cancel-all |

**(b) P2-2's balance-cache flush switch extends the 9.8 app differential (no new harness).** P2-2
adds its per-row balance flush reference behind the existing `test-reference-paths` feature and
`ExecutionContext::test_reference_paths`, so `reference_paths_match_production_*` (and the
reduce-only variants) cover it.

| option | note |
|---|---|
| **A. extend the 9.8 differential (chosen)** | one switch, every existing sequence and mode; no second harness to keep in step |
| B. a separate P2-2 app harness | duplicate sequences and fixtures |

**Sync point: do not merge main into `perf/item6-phase2` until after the P2-1 gate cell** (the
next sync point, as 9.11). The gate reference stays `e934fa0e`.

### 9.13 Compiled defaults = the benched configuration (owner, via 18c s104, 2026-10-08)

**Chosen:** before the testnet genesis, the node's compiled defaults become the configuration the
benches run (`run-cell.sh` `RECORD_ENV`): pipelined write, `BOOK_ROWS=3`, `RESIDENT_BOOKS=1`,
`NATIVE_ROOT_CACHE=1`, `PARALLEL_SETTLE=1`, `BUCKET_HASH=8`, member cache 256 MB,
`COMMIT_LAG_BACKOFF_CAP=8`; each stays overridable by env. A small branch at a sync point after
Phase 2, after checking that each knob is node-local and does not change results. Row 23's item 1
still applies, so the off (non-resident) path stays exactly as it was.

**Book mode on testnet: Classic (owner, via 18c s107, 2026-10-09).** `BOOK_ROWS` is the exception
to the list above: Classic stays the compiled default and the testnet runs Classic; the book mode
does not go into genesis now. Basis: the Classic vs mode 3 control (results doc section 36, review
log row 41) shows no throughput gain for mode 3 at the standard shape (+1.3% matched/s at 10
markets, 300 not resolved) at about 2x flush per fill and extra exec-thread handoff wait. Revisit
mode 3 only with a deep-book shape, before mainnet (then with the book mode in genesis). For the
record: the Phase 1 / Phase 2 numbers were measured with mode 3 set by env (`RECORD_ENV`); Classic
performs about the same at this shape. This supersedes the "owner decision pending" in the
`feat/compiled-defaults` note below.

Order after the P2-1 gate cell (18c s104): merge main `12979d4b` into the branch (sync point, as
9.11), the row 23 lows, then the reduce-only modify sweep as its own small branch (consensus, fresh
genesis; row 22); the defaults branch after Phase 2.

**Built on `feat/compiled-defaults`** (from main `1eced05c`; ozarchy s107), per-knob check:
pipelined write, `RESIDENT_BOOKS`, `NATIVE_ROOT_CACHE`, `PARALLEL_SETTLE`, member cache 256 MB:
node-local, value-neutral (RocksDB write scheduling / caches with root self-check / differential
tests `root_cache_tests`, `resident_books_tests`, `parallel_settle_tests`), default on, `=0` keeps
each old path unchanged. `BUCKET_HASH=8` was already the compiled default. **`BOOK_ROWS` stays
Classic by default** (18c s107, after the adversarial Codex review; the mode-3 default commit
`f80ef36c` is reverted): the book mode is consensus-visible (state-root format) but genesis carries no
book mode, so a compiled default flip forks a mixed-binary fleet on the same fresh genesis, and the
`__book_mode__` marker cannot see the disagreement before the DB has history (the s450 ship-blocker).
Mode 3 runs only when set (`TORUS_BOOK_ROWS=3`, as `RECORD_ENV` does).
`env_unset_compiled_defaults_equal_the_bench_config`: env unset = the explicit knobs with
`BOOK_ROWS=0`, and explicit mode 3 = the bench config, in running hash, native root, every CF and the
marker, serial and pipelined. Mode 3 as the default needs the book mode in genesis first (owner
decision pending). If approved: its own branch, bundled with the reduce-only sweep fix's fresh genesis;
first step = check the s450 mode-2 test gaps against mode 3 (resident path re-verify, match-workers
state-root determinism, multi-market chunking). **`COMMIT_LAG_BACKOFF_CAP` not
changed:** a genesis parameter (`chain_config.commit_lag_backoff_cap`, consensus-liveness, the env is
a devnet override over genesis, `torus-node/src/main.rs` `resolve_commit_lag_cap`); a compiled env
default would override every genesis value. Set `commit_lag_backoff_cap: 8` in the testnet genesis
instead. `run-cell.sh` `RECORD_ENV` stays explicit (older binaries).

**Testnet genesis to-do** (items to set when the testnet genesis is built; add here as decided):
- `chain_config.commit_lag_backoff_cap: 8`. Owner, via 18c s108 (2026-10-09): stays out of the
  compiled defaults, set in the testnet genesis instead (the S470 value the benches run by env).
- No book mode in genesis; the testnet runs Classic (the compiled default). Owner, via 18c s107,
  above.

### 9.14 P2-1 gate missed: accept and continue (18c s104 / ozarchy s31, 2026-10-08)

**Chosen: A, accept P2-1 as built and continue** (review log rows 24-25). Phase 1 fell 0.44 ms per
native block against the >= 1.9 ms gate. Per skipped visit the cell gives ~0.07 us
(0.44 ms / (59.2 cancel-alls x 111 avoided visits); row 25 first wrote "~4 us", which is per book per
native block, corrected in row 27), so B's further ~97 visits are worth ~97 x 59 x 0.07 us ~ 0.4 ms,
still below the gate. The bench's ~200 markets per trader is near the worst case for stale entries.
9.12 (a) stands: the background prune (C) comes before mainnet, off the fill path, and its gauges
show how many entries it removes. Before P2-1b's gate, its estimate (section 3) is rechecked
against the measured per-visit cost, since step 0.3's split (row 13) seems to overstate the
per-book saving.

| option | note |
|---|---|
| **A. accept, continue (chosen)** | no fill-path cost; ~0.4 ms left on the table, below the gate either way |
| B. drop the market when the trader's last order / stop there leaves the book, one more gate cell | ~0.4 ms (from the per-visit cost); a check on the fill path, another cell |
| C. bring the 9.12 prune forward | same bound as B at best; stays before mainnet instead |

Order: merge main `12979d4b` (sync point, as 9.11), the row 23 lows, then P2-1b (step 1b; dropped
after the recheck, 9.15).

### 9.15 P2-1b option C dropped from Phase 2 (18c s106 / ozarchy s31, 2026-10-09)

**Chosen: drop C; B (market in the order id) stays the later consensus item (9.2).** The recheck
(review log row 27) puts C's saving at ~10-30 us per by-id action and its upkeep at ~0.6-1.4 ms per
native block (an insert per resting order and a removal per order leaving the book, ~21k each per
native block on the standard shape). C loses even on the cancel-by-id cell (1.22 by-id actions per
native block: 12-37 us saved vs >= 0.6 ms upkeep), break-even is ~20-140 by-id actions per native
block, and no by-id cell can change the upkeep side. B needs no per-order upkeep. The s-byid pair
(`bdd5b470` vs `e934fa0e`) runs only to settle what a by-id action costs with P2-1 in (row 27 (d))
and B's priority, on a quiet host.

| option | note |
|---|---|
| **drop C, keep B for later (chosen)** | no upkeep on the standard shape; by-id keeps the scan until B |
| build C as specified | loses at today's and the bench's by-id share; gate "standard shape not worse" misses by ~0.6-1.4 ms |

Order: P2-5 (step 2), P2-2, the P2-4 design check (section 4).

### 9.16 Phase gate lowered to the measured 1.0495x; Phase 2 closes with P2-1 + P2-5 (18c s106 / ozarchy s34, 2026-10-09)

**Chosen: A, lower the phase gate (9.1) to the measured 1.0495x vs `e934fa0e`** (results doc section 33,
head to head p25/base, pairwise 1.045-1.054x; review log rows 34-37). Phase 2 closes with P2-1 and
P2-5. P2-4 cannot close the ~2.0% gap at walk 0 (row 36), and P2-2 regressed and was reverted (row 35).

| item | estimate (ms per native block) | delivered (measured) | status |
|---|---|---|---|
| P2-1 cancel-all index | 3.7 | 0.44 (phase 1, val0; row 24) | kept (9.14) |
| P2-2 batch cache flush | 3.2 | -2.85 (cache flush +2.85, regressed; row 34) | reverted (row 35) |
| P2-5 keyed fast hasher | 10.3 | 8.59 (engine, val0; row 30) | kept |
| cumulative matched/s | +7% gate | 1.0495x head to head | gate lowered to this |

| option | note |
|---|---|
| **A. lower the gate to 1.0495x, close with P2-1 + P2-5 (chosen)** | no more Phase 2 items; the candidates below feed Phase 3's step 0 |
| B. add items to reach +7% | the two walk-0 candidates (~2-4 ms, ~0.8-1.7%) plus a streaming P2-2 (~1.3%), each needing a perf split or microbench and a gate cell; not sure to close the gap |

Backlog, not built now (input to Phase 3's step 0 profile; build only what that profile confirms):
- reuse decoded records in `delta_sums` (~1-2 ms per native block at walk 0, row 36);
- a node-local index of cooldown / pending rows that skips the two reads per healthy trader
  (~1-3 ms, shadow test vs the CF, row 36);
- streaming P2-2 (one lock, rows streamed, ~1.3% matched/s) only if an old / batch / streaming
  microbench at ~21k rows per block passes (row 35).

Order: push `perf/item6-phase2`; then (a) the R01 fault test on `/mnt/r01`
(`fix/r01-write-fail-stop` `01d991ab`); (b) merge main `9793f1ec` (eth_call / estimateGas typing,
RPC only); (c) final suites on the branch head, then 18c's final review before the merge to main;
(d) after the merge (sync point): node compiled defaults = benched config (9.13), the reduce-only
modify sweep fix, C2 holder sets, and 18c starts R02's loader / save branch; (e) the Phase 3 plan,
step 0 re-profile first.
