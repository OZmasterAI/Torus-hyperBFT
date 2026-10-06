# Implementation Plan: item 6 Phase 2 (per-block work in proportion to fills)

Status: PLAN, nothing built. Written 2026-10-06 (after s94) from the Phase 2 step 0 profile
(ozarchy results doc `docs/perf/ozarchy-antispam-item6-pf1-2026-10-04.md` section 22, on
`integrate/s94-batch`; "section 22" below). Short form and the phases after this one:
`item6-phases-2-5-plans.md`. Design: `market-scaling-in-memory-design.md` section 3, Phase 2.
Phase 1 verdict and the carried-over gates: `item6-phase1-impl.md` 9.12 and 9.13.

Code references are `file:function` with line numbers at `origin/integrate/s94-batch`
(`a0eda77`). The step 0 profile ran crab = main `59fa407`; the hot files named here
(`cancel_batch.rs`, `market_workers.rs`, `position.rs`) are the same on both, and
`native_executor.rs` differs only in places this plan does not touch.

Every number below is from section 22 or from the code, unless it is marked
**(estimate)**.

## 1. Goal and gates

Goal: cut the per-block work on the execution thread that does not scale with fills: the
cancel-all scan of every book, a new thread set per block, and the per-row cache flush.
No consensus rule, no state format and no state hash changes (same as Phase 1).

| gate | target (fail line) | measured by | source |
|---|---|---|---|
| Phase gate, 300 markets | matched/s >= +7% vs Phase 1 (`59fa407`) | interleaved cells, section 6 | short plan, Phase 2 |
| Phase gate, 10 markets | no regression vs Phase 1 beyond the ~5% cell resolution | interleaved cells | short plan; Phase 1 plan 1.1 |
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
- Optional P2-1b, CancelOrder / ModifyOrder by id (`exec_cancel_order` :8814,
  `exec_modify_order` :9045) still loop over all books to find an order id. Not measured:
  the standard shape sends no single cancels. Build only on owner request (open question
  2), with its own ubench cell.

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
- Change: one dedicated, named exec pool built once (rayon is already in the workspace:
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
- Change: none yet. Step 4 is a read-only design check of two options:
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

- 0.1 Branch `perf/item6-phase2` from the base (open question 6). Record the suite counts.
- 0.2 Node-local counters and harness columns (not hashed): per cancel-all, books
  visited and books where the sender had orders or stops; thread spawns per site per
  block; process sys CPU per 1k fills and per native block (from `/proc/<pid>/stat`);
  per-block exec timing in the node for oracle-only blocks (section 20.2 asked for it).
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
| 2 | P2-3 exec pool | P2-3 above |
| 3 | P2-2 batch flush | P2-2 above |
| 4 | P2-4 design check (read-only); build only if it passes | P2-4 above |
| 5 | phase campaign (section 6) | section 1 |

## 5. Commit plan

C0 counters + reference paths | C1 cancel-all index | C2 exec pool | C3 batch flush |
(C4 liquidation skip, if step 4 passes). Every commit: full suite and goldens green,
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
- arms: Phase 2 branch vs Phase 1 (`59fa407`, or main after the s94 batch merge),
  interleaved, a 60 s warm cell first, >= 4 cells per arm; one main `92a02ed` pair for
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
- A cheaper hasher for `Address`-keyed maps: hashing is ~17% of execution self time
  (SipHash `write` 6.6%, `hash_one<Address>` 3.5%, `hash_one<u128>` 3.1%), but it is not a
  Phase 2 item in section 22 and user-chosen keys raise a hash-flooding question
  (open question 5).
- Backlog items in Phase 1 plan 9.13 (maker over-commit, governance errors, etc.).

## 8. Review log (filled during the build)

| # | step | finding | decision |
|---|---|---|---|

## 9. Open questions for the owner

1. Phase gate: keep +7% vs Phase 1? The measured items give about +5-15% (estimate);
   step 0.2 / 0.3 will narrow it.
2. P2-1b (CancelOrder / ModifyOrder id index): in Phase 2 or not? Not measured on the
   standard shape.
3. Pool: a dedicated rayon pool (rayon is already a workspace dependency), or a small
   std-only pool?
4. P2-4: keep the design check in Phase 2, or move the liquidation sums to Phases 4-5 as
   9.12 says for the tail?
5. Hasher swap (~17% of self time): its own item, Phase 2 optional, or later?
6. Base: main `59fa407`, or main after `integrate/s94-batch` merges?
