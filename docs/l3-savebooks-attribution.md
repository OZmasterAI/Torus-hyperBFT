# L3 save_books attribution — what the 107 ms actually is

Branch `perf/l3-savebooks` @ 8e0baf2 (base `perf/re-proof5` @ 2564319). Explains
the l3scrape finding (`devnet/wsl/results/l3scrape-18c-b1aba10.md`): at the pegged
cap-400 cell, `exec_save_books_seconds` measured **106.7 ms per loaded block**
(n=1748) — the dominant exec consumer, where design intent said ~1–3 ms.

Bench-standard env of that cell: `TORUS_BOOK_ROWS=2` (⇒ `BookMode::LevelAuthority`),
`TORUS_RESIDENT_BOOKS=1`, `TORUS_NATIVE_ROOT_CACHE=1`, `TORUS_PARALLEL_SETTLE=1`,
cap-400. ~400 orders / ~330 trades across 10 markets, resting depth ≈ 0.

---

## 1. Span anatomy — the 107 ms is `ctx.save_order_books()`

`app.rs:1390-1394` times exactly one call:

```rust
let save_books_timer = std::time::Instant::now();
ctx.save_order_books();
m.exec_save_books_seconds.observe(save_books_timer.elapsed());
```

`save_order_books` (`native_executor.rs:1640`) iterates **`self.dirty_books` only**
(dirty-only — NOT all markets ✓), and for the cell's mode (`LevelAuthority`,
`native_executor.rs:1724`) does, **per dirty market**:

| # | sub-step | cost class | RocksDB? |
|---|---|---|---|
| 1 | `book.take_row_ops()` → drain `row_journal`, `encode_order_row`, put/delete `CF_BOOK_ORDER_ROWS` (node-local) | O(changed orders) | overlay puts (in-RAM this block) |
| 2 | `book.take_level_ops()` → drain `level_journal`, `level_row_data` (keccak over O(orders in level)), put/delete level rows in `CF_NATIVE_ORDER_BOOKS` | O(changed levels) + hash | overlay puts (in-RAM this block) |
| 3 | **`diff_stop_rows()`** → `state.iterate_cf(CF_NATIVE_ORDER_BOOKS, prefix = market‖0x02)` — **a RocksDB prefix SEEK, every block, unconditional** | O(1) writes, but 1 **read seek** | **YES — read** |
| 4 | **`write_meta_if_moved()`** → `state.get_cf_raw(CF_NATIVE_ORDER_BOOKS, meta_key)` then compare + conditional put — **a RocksDB point READ, every block, unconditional** | 0–1 writes, but 1 **read** | **YES — read** |

Plus once per block: `next_global_order_id` row (only when moved), `__book_mode__`
marker (only first save).

### Answers to the attribution questions

- **Does anything iterate ALL markets rather than dirty-only?** No. The save loop is
  over `dirty_books`. But at cap-400 all 10 markets are active every block, so
  `N_dirty ≈ 10`, and the per-market reads (steps 3–4) run ~10× per block.
- **What is serialized per save in rows mode 2?** NOT a full book image. Per changed
  order: one order-row blob (node-local CF). Per changed level: one `qty‖count‖
  level_hash` row, where `level_hash = keccak256(framed order rows at that level)` —
  O(orders in that level). Serialization is O(changed), not O(book).
- **Is any RocksDB read/write inside the span?** **YES — and this is the finding.**
  The *writes* are journaled O(changed) (rank8 proof stands). But every block also
  pays **two unconditional RocksDB READS per dirty market** — a prefix seek
  (`diff_stop_rows`) and a point read (`write_meta_if_moved`) — that the journaling
  round never eliminated.
- **Is the BookJournal O(changed) path engaged, or is a fallback silently running?**
  The journal IS engaged: `take_row_ops`/`take_level_ops` drain the `BTreeSet`
  journals (`order_book.rs:1168, 1244`); the write-count witness
  (`resident_rows_save_is_incremental_via_journal`: 201→2→≤3) confirms O(changed)
  *writes*. **The disengaged accounting is on the READ side:** rank8 measured write
  counts, never read counts. `diff_stop_rows` and `write_meta_if_moved` are
  *stateless* reconciliations that re-read the DB every block to decide whether to
  write — so they were invisible to a write-count proof yet run every block.

---

## 2. Why the reads are slow in vivo (the 107 ms mechanism)

`CF_NATIVE_ORDER_BOOKS` in mode 2 holds meta(0x00), stop(0x02) and **level(0x03)**
rows. Under load the level rows CHURN: a block writes level rows for touched levels,
a later block empties those levels and DELETES the rows (`take_level_ops` → `None` →
`delete_cf_raw`). RocksDB deletes are tombstones; the CF has a 128 MiB memtable
(`db.rs:70`) and **no prefix extractor**, so its skiplist accumulates a large,
tombstone-laden version set before flush/compaction. This is the exact pathology the
code already calls out for `cf_consensus_meta` (`db.rs:76-80`: "the growing skiplist
slows reads").

Every block then pays, per dirty market:
- `diff_stop_rows`: a **prefix seek** into that skiplist (merges memtable + every L0
  SST), even though the stop set is empty (resting≈0) — the seek returns nothing but
  still positions across all sorted runs.
- `write_meta_if_moved`: a **point read** of the meta key down the same LSM.

~10 markets × (seek + point read) per block, into a CF whose read cost grows with
churn = the un-accounted, deschedule-and-I/O-inflated bulk of the 107 ms wall.

---

## 3. Sub-timer µbench (uncontended, 18c)

`crates/torus-bridge/tests/l3_savebooks_ubench.rs` (#[ignore]) reproduces the cell
shape (10 markets, ~400 orders/block, heavy matching, resting≈0, mode-2 + resident)
against a real `StateDb`, and times `save_order_books` split into the four sub-steps.

<!-- NUMBERS: filled from savebooks-ubench.log -->

### 3a. Span breakdown (`savebooks_span_breakdown`)

TABLE_PLACEHOLDER_BREAKDOWN

### 3b. Read cost vs churn (`savebooks_read_cost_vs_churn`)

TABLE_PLACEHOLDER_CHURN

**Reading:** uncontended `save_order_books` = ~X µs/block, of which stops+meta reads
= ~Y%. In vivo the same reads hit a loaded, tombstone-laden LSM and inflate to the
107 ms wall. Deschedule/I-O share = 107 ms − (uncontended CPU) ≈ Z ms.

---

## 4. Root-cause verdict

VERDICT_PLACEHOLDER

---

## 5. Fix

FIX_PLACEHOLDER

---

## 6. Projected per-block savings

SAVINGS_PLACEHOLDER
