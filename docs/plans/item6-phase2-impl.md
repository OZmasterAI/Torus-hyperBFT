# Implementation Plan: item 6 Phase 2 (per-block work in proportion to fills)

Status: step 0 done on `perf/item6-phase2` (ozarchy, s27 / s101) from main `d3ba3c0a` (9.7;
review log rows 1-16); base moved to main `e934fa0e` before step 1 (9.11): 0.1, 0.2 and 0.4 built, 0.3 and Gate 0 measured (results doc section 25).
Step 0 checkpoint: P2-1 + P2-2 below 13 ms per native block; owner (s27 / s101): P2-5 joins the
gate set, the +7% stays (9.9), P2-3 goes to the backlog (9.10). Step 1: the 9.8 test-only feature
and P2-1 (C1) built (ozarchy s30, review log rows 18-19); its gate cell is open. Next: P2-1b, then P2-5,
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
- P2-1b, CancelOrder / ModifyOrder by id (`exec_cancel_order` :8814,
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

Order (owner s27 / s101, 9.9 and 9.10): step 0, then P2-1 (+ 1b), P2-5, P2-2, the P2-4
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
| 1b | P2-1b order id -> market map (option C, 9.2) | P2-1b above |
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
- P2-1b option B (market in the order id): later, as its own consensus item (9.2).
- Backlog items in Phase 1 plan 9.13 (maker over-commit, governance errors, etc.).

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

## 9. Owner decisions (s96, 2026-10-06)

Each question lists the chosen option and the options not chosen. The options not chosen
are kept because some may matter later. Impact is an estimate unless marked measured
(rule of thumb, section 1: about +0.4-0.55% matched/s per ms saved per native block).
"HL" = Hyperliquid's official docs (links per item); "not public" = HL does not document it.

### 9.1 Phase gate

**Chosen: keep +7%** (300 markets) against the Phase 2 base (9.6), with the step 0
checkpoint (section 1): below 13 ms of P2-1 + P2-2, the owner picks P2-5 or a lower gate.

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
