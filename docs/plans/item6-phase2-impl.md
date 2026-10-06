# Implementation Plan: item 6 Phase 2 (per-block work in proportion to fills)

Status: PLAN, nothing built; owner decisions recorded s96 (section 9, with the options not
chosen). Written 2026-10-06 (after s94) from the Phase 2 step 0 profile
(ozarchy results doc `docs/perf/ozarchy-antispam-item6-pf1-2026-10-04.md` section 22, on
`integrate/s94-batch`; "section 22" below). Short form and the phases after this one:
`item6-phases-2-5-plans.md`. Design: `market-scaling-in-memory-design.md` section 3, Phase 2.
Phase 1 verdict and the carried-over gates: `item6-phase1-impl.md` 9.12 and 9.13.

Code references are `file:function` with line numbers at `origin/integrate/s94-batch`
(`a0eda77`). The step 0 profile ran crab = main `59fa407`; the hot files named here
(`cancel_batch.rs`, `market_workers.rs`, `position.rs`) are the same on both, and
`native_executor.rs` differs only in places this plan does not touch. The Phase 2 base is
now main `35e69b3` (9.6): `a0eda77` plus `feat/liq-telemetry`, which changed
`liquidation_step.rs`, `app.rs` and `torus-core/src/liquidation.rs`, so P2-4's line numbers
there may have moved.

Every number below is from section 22 or from the code, unless it is marked
**(estimate)**.

## 1. Goal and gates

Goal: cut the per-block work on the execution thread that does not scale with fills: the
cancel-all scan of every book, a new thread set per block, and the per-row cache flush.
No consensus rule, no state format and no state hash changes (same as Phase 1).

| gate | target (fail line) | measured by | source |
|---|---|---|---|
| Phase gate, 300 markets | matched/s >= +7% vs the Phase 2 base (main `35e69b3`, 9.6) | interleaved cells, section 6 | short plan, Phase 2; owner s96 (9.1) |
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

- Where: `Address`- and `OrderId`-keyed `HashMap`s / `HashSet`s on the execution path
  (section 22: SipHash `write` 6.6%, `hash_one<Address>` 3.5%, `hash_one<u128>` 3.1% of
  execution self time, ~13% together; Keccak 5.5% is the state hash and is not touched).
- Change: `foldhash` with a random seed per process (already in `Cargo.lock` 0.1.5 / 0.2.0
  through `hashbrown`; add it as a direct dependency) behind one type alias, swapped in map
  by map, hottest first.
- Expected saving (estimate, the least certain in this plan): 5-20 ms per native block,
  +2-7% matched/s. A microbench of the hot maps comes first and sets the estimate.
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

Order: step 0, then P2-1, P2-3, P2-2, P2-4 (design check). This is section 22's order
(22.4). P2-1 is the largest measured item. P2-3 is ranked by spawn count, not ms, so its
step 0.2 measurement may move it after P2-2. P2-2 is small, touches different code and can
be built in parallel by a second builder. Each step: tests first, then the change,
then the gate. A failing correctness test stops the work. A missed cost gate is written
into the review log (section 8) and the next step starts (Phase 1 rule).

Who measures: 18c builds (code, tests, sanity ubench); ozarchy gives the gate verdicts
(Phase 1 plan section 3). Own worktree `wt/item6-phase2`, own `CARGO_TARGET_DIR`. Tests per
`TESTING.md`: nextest per crate while iterating, full workspace nextest + doc tests before
each commit, one full `cargo test --workspace` before the merge.

### Step 0: guardrails (no engine change)

- 0.1 Branch `perf/item6-phase2` from main `35e69b3` (9.6). Record the suite counts.
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
| 1 | P2-1 cancel-all index | P2-1 above |
| 1b | P2-1b order id -> market map (option C, 9.2) | P2-1b above |
| 2 | P2-3 exec pool | P2-3 above |
| 3 | P2-2 batch flush | P2-2 above |
| 4 | P2-4 design check (read-only); build only if it passes | P2-4 above |
| 4b | P2-5 hasher (earlier if the step 0 checkpoint pulls it in) | P2-5 above |
| 5 | phase campaign (section 6) | section 1 |

## 5. Commit plan

C0 counters + reference paths | C1 cancel-all index | C1b order id -> market map |
C2 exec pool | C3 batch flush | (C4 liquidation skip, if step 4 passes) | C5 hasher. Every commit: full suite and goldens green,
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
- arms: Phase 2 branch vs the base, main `35e69b3` (9.6), interleaved, a 60 s warm cell first, >= 4 cells per arm; one main `92a02ed` pair for
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
