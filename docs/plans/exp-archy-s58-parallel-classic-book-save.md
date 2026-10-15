# exp/archy-s58-parallel-classic-book-save

Perf experiment (s58 perf search, archy). Branch
`exp/archy-s58-parallel-classic-book-save`, cut from `exp/archy-s58-perf-search`
(= main a44000e0). Node-local flag `TORUS_PARALLEL_BOOK_SAVE=1`, default off.

## What changed

Only `crates/torus-bridge/src/native_executor.rs` (plus two new test files).
The change applies to the Classic book mode only, and only when the flag is on.

- Flag items, placed after `parse_save_books_min_ops`:
  - `parse_parallel_book_save`: on only for a trimmed `1`.
  - `CLASSIC_PARALLEL_SAVE_MIN_ORDERS = 16_384`: the gate counts total resting orders over the dirty books.
  - A thread-local pin, `with_parallel_book_save(on, min_orders, f)`, `#[doc(hidden)]`, for tests and µbenches.
  - `parallel_book_save() -> Option<usize>`: reads the pin, or else a `OnceLock` over the env, logged once.
  - `classic_blob_capacity_hint`: `128 + 97*orders + 145*stops`, saturating, capped at 256 MiB. It is used only for `Vec::with_capacity`.
  - `encode_classic_book_into(&OrderBook, Vec<u8>)`: a small pure free fn, so Phase 5's freeze can reuse it. Its bytes equal `borsh::to_vec`, which is `with_capacity` plus the same `serialize` call.
- `save_order_books` gets a middle arm: `else if Classic && classic_parallel_save_gate()`.
  - The gate needs three things: the flag on, at least 2 dirty books present in `order_books`, and their order total at or above the minimum.
  - Otherwise the serial loop runs unchanged.
  - Both paths then write the counter row and the mode-marker row unchanged.
- `save_classic_parallel`:
  1. Gathers the dirty books in one `iter_mut` pass.
  2. Presizes one buffer per book on the exec thread.
  3. Builds one `exec_pool::run_jobs` job per book, heaviest `order_count` first. Each job encodes its book and discards its journals.
  4. Back on the exec thread, sorts the results ascending by market id and puts them serially:
     - put error: `latch_save_fault`, with today's text;
     - encode `Err`: today's `tracing::error!`, then skip (no latch);
     - job panic: `latch_save_fault("book save: encode job panicked (market X): ...")` and no put. A panic is never a silent skip.
  5. Sets `last_save_workers` to the job count.
  6. Records the encode wall time in `save_split.drain_ns` and the puts in `save_split.write_ns`. On the serial Classic loop both stay 0, as before.
- Not changed: torus-core, OrderRows, modes 2-3, `save_order_books_deferred`, `exec_pool.rs`, `market_workers.rs`.
- With the flag unset, `exec_pool::pool()` is never touched: the gate returns before it looks at any book.

## Why (evidence)

- Bench and testnet run Classic: campaigns pass `EXTRA_ENV` `TORUS_BOOK_ROWS=` (empty).
- In `save_order_books`, every dirty book runs `borsh::to_vec` → `put_cf_raw_owned` → journal discard, serially on the exec thread.
- Cost of save_books per native block:

| cell | save_books |
|---|---|
| 300m 600 s | 20.7-21.7 ms of a ~313 ms block |
| 300m 600 s, late in the cell (604-719k resting) | 23-26 ms |
| 300m 120 s | 10-11.7 ms |
| 10m depth 4 | 30-36 ms |

- It is all on-CPU. In the p4s0 exec profile it is 5.3-5.5% of exec samples: serialize ~60%, realloc ~22% (1 KiB initial buffer, doubling), journal discard ~16%.
- The exec pool is idle at this point. Its only callers are match (`market_workers.rs:183`) and settle pass A.
- Memories: 6b3b0bd9, 9e83dbd7, 3059a0d8, f183eb47.

### Microbench (gate)

`tests/classic_book_save_ubench.rs` (`#[ignore]`, release) builds the production shape:
- an overlay-backed Classic ctx with resident books, and a fresh overlay and ctx per block;
- every book touched once and marked dirty each block;
- serial and parallel saves interleaved, 20 reps.

Results on archy, 2026-10-15. Sibling builds were running (load average ~50-110), so archy was not idle:

| shape | allocator | serial median | parallel median | parallel / serial |
|---|---|---|---|---|
| 300 books x 2.3k (703k resting, 67 MB) | jemalloc (LD_PRELOAD) | 26.0 / 24.8 ms | 9.24 / 9.14 ms | **35.5% / 36.9% (gate <= 40%: PASS)** |
| 10 books x 64k (640k resting, 62 MB) | jemalloc (LD_PRELOAD) | 20.0 / 20.6 ms | 8.34 / 8.30 ms | 41.7% / 40.3% |
| 300 books x 2.3k | glibc | 49.0 / 61.3 ms | 8.65 / 11.44 ms | 17.6% / 18.7% |
| 10 books x 64k | glibc | 20.3 / 24.1 ms | 7.61 / 9.71 ms | 37.6% / 40.3% |

- jemalloc (the node's allocator) is the relevant row. It reproduces the production serial save of 21-26 ms at ~700k resting. glibc page-faults every fresh buffer and inflates the serial 300-book time about 2x, so its ratio overstates the gain.
- The parallel save is bounded by memory bandwidth. Each worker runs about 3-4x slower than alone: 67 MB written plus ~120 MB of `Order` structs read.
- **Not done: the run next to a live 3-node cell.** No cell was running, and launching one belongs to the bench agent. The three nodes share DRAM bandwidth, so expect the in-cell ratio to be worse than the table.

## Flag

`TORUS_PARALLEL_BOOK_SAVE=1` turns it on. It is read once per process and logged once at info level. Any other value, or unset, means off.

The flag is node-local and changes no bytes. The parallel path writes the same bytes and the same `written` counts as the serial loop, and issues the same put sequence (CF and key order). Nodes with and without the flag agree.

## Tests

1. Inline, `native_executor::parallel_book_save_toggle_tests` (11 tests):
   - parse table;
   - pin nesting and restore, including on unwind;
   - capacity hint saturation and cap;
   - `encode_classic_book_into == borsh::to_vec` for any presize and for a dirty buffer;
   - gate: one dirty book, total below min, flag off, and dirty ids with no book all keep `last_save_workers == 1`; `total == min` engages;
   - OrderRows untouched;
   - parallel rows, counter and marker equal the serial path's, journals empty;
   - planted job panic: latches `encode job panicked (market 2)`, writes no blob for that market, still writes the others;
   - first latched fault wins;
   - planted encode `Err`: logged and skipped, no latch (today's behaviour).
2. `tests/parallel_book_save_tests.rs`:
   - A seeded adversarial Classic universe: 8 markets, resident books, places, fills, IOC, cancels, price and qty modifies, cancel-all (all markets and one market), stop-limits, stop-markets and cloid orders. Two seeds, 30 blocks each, serial vs parallel.
     - Equal per block: `written`, the PUT SEQUENCE, every `CF_NATIVE_ORDER_BOOKS` row (each == `borsh::to_vec(book)`), every `CF_NATIVE_MARKETS` row (next order id, mode marker), the post-save books (bytes and empty journals), the native roots.
     - Equal at the end: the final CF dumps, the full-scan oracle, and a cold reload.
     - The parallel run engaged on >= 25 of 30 blocks, one job per present dirty book.
   - The same universe with the flag alternating per block equals the serial run.
   - Injected put faults on markets 3 and 5: the same first fault (market 3), the same put attempts in the same order, the same `written` count and the same rows.
3. Mutation check: both mutants are caught, and the source was restored.
   - Descending put order is caught by the put-sequence and first-fault assertions.
   - "Panic → silent skip" is caught by the planted-panic test.
4. Env reruns with `TORUS_PARALLEL_BOOK_SAVE=1` (all green):
   - torus-bridge: `order_book_persist_tests`, `r02_load_save_tests`, `perf_equivalence_golden`, `save_split_attribution_tests`;
   - torus-consensus: `reference_paths_match_production_classic_reload` and `_resident`;
   - `rdeps(torus-bridge)` with the flag set: 1973/1973 passed.
   - With the flag unset: the whole workspace passed (3826/3826, 0 flaky), plus the doc tests.

   The default minimum (16,384 orders) keeps the small fixtures serial. The pinned differential tests (items 1 and 2) are what prove the parallel path.

## Expected gain

save_books per native block:

| cell | today | expected |
|---|---|---|
| 300m 600 s | 21.7 ms | ~7-10 ms |
| 300m 600 s, late in the cell | 23-26 ms | ~8-10 ms |
| 300m 120 s | 10.5 ms | ~4-6 ms |

matched/s:

| cell | expected |
|---|---|
| 300m 120 s gate | +1 to +2.5% |
| 300m 600 s | +1.5 to +3.5% |
| 10m depth 1 | ~0 |
| 10m depth 4 | +3 to +6% |

Other metrics:
- native blk/s: +3-5%;
- CPU-s/1M: neutral to -0.3%;
- RSS: within ±0.3 GB.

## How to bench it

A/B on the same binary, `EXTRA_ENV` differs by `TORUS_PARALLEL_BOOK_SAVE=1` only. Keep the campaign's Classic setting (`TORUS_BOOK_ROWS=` empty); under modes 2/3 the flag is a no-op.

- Cells: 300m 120 s gate, then 300m 600 s with n=4 paired; 10m depth 1 and depth 4.
- Mechanism check:
  - `torus_exec_save_books_seconds` per native block down >= 50% at 600 s.
  - Engagement shows in `torus_exec_save_books_drain_seconds` and `_write_seconds`. They are 0 on serial Classic and non-zero when the parallel path ran.
  - Main has **no `torus_exec_save_books_workers` gauge**: `last_save_workers` is only a ctx accessor. Adding the gauge needs telemetry + app.rs edits, which are outside this experiment's file list.
- Acceptance:
  - AGREE in every cell;
  - matched/s paired-CI lower bound >= 0 at 300m 600 s;
  - no 10m regression;
  - CPU-s/1M not worse;
  - RSS mean within +0.3 GB.
- Watch:
  - engine ms per 1k fills in save bursts (the 3 nodes share DRAM bandwidth);
  - p99 block.

## Risks

- **Memory bandwidth.** The save becomes a 67 MB-per-block burst across the pool, and in the bench three nodes do it on one host. The µbench speedup (2.7x, idle-ish) can shrink in a cell, and the burst can slow the other two nodes' engines.
- **RSS.** Blobs are presized to the exact size for 97 B orders. Cloid orders (113 B) grow a buffer once, so slack is at most 2x, no worse than today's doubling. The buffers are allocated on the exec thread (its jemalloc arena), and pool threads only write into them.
- **Pool contention.** It is fork-join on the exec thread, between match/settle and the next block. Nothing else uses the pool then, and it never overlaps the next block's engine, because the books are live.
- **Panics.** A job panic is caught by `run_jobs` and becomes a fail-stop latch (`take_fatal_error` in app.rs). The R01b hook does not fire for `torus-exec-pool-*` threads, so the latch is the only fail-stop: never a skip, never a stale blob.
- **Conflicts:**
  - Phase 5 step 3 (18c) moves Classic serialization to freeze. It can reuse `encode_classic_book_into`, but its cost table already counts save_books as removable, so do not claim the same ms twice.
  - s123 defer-book-blobs (checkpoint_layer.rs) complements this change.
  - s123 cancel-all edits other hunks of native_executor.rs.
