# exp s123: defer-book-blobs-in-layer-merge (rank 4)

Branch `exp/s123-defer-book-blobs-in-layer-merge` in worktree
`/home/18c/projects/Torus-hyperBFT/.claude/worktrees/wf_443f7b55-fcb-32`, based on main `c612dc7f`.
Patch: `/home/18c/projects/wt/exp-s123/defer-book-blobs-in-layer-merge.patch` (staged, not committed).
Rebased onto main `2bc88bcf`, built and unit-tested (see "Build and test results" at the end). Not benched.

## What changed

Flag: `TORUS_CKPT_DEFER_BOOKS=1`, default off. It is read once per process, the first time any L split, merge
or read asks for the routing table.

`crates/torus-state/src/checkpoint_layer.rs`
- `parse_defer_books(flag, book_rows, resident_books)` (:61), a pure function. It returns true only if all
  three hold:
  - `flag` trims to `1`
  - `TORUS_BOOK_ROWS` is not 1, 2 or 3 after trimming. This matches `parse_book_rows_mode`,
    native_executor.rs:2502-2509 (Classic).
  - `TORUS_RESIDENT_BOOKS` is not 0 after trimming. This matches `parse_resident_books_toggle`, :3106-3108.

  The doc comment says this is a performance-only gate.
- `deferred_slot_table(defer_books)` (:74): `DEFERRED_CFS.contains(name)`, or `defer_books` and the name is
  `CF_NATIVE_ORDER_BOOKS`.
- `is_deferred_slot` (:85): its OnceLock now builds `deferred_slot_table(parse_defer_books(env...))`. If
  `TORUS_CKPT_DEFER_BOOKS` is set at all, it logs one `tracing::info!` line with `defer_books=<bool>`, so a
  bench arm can check from the log that the flag took effect (or was gated off).
- Docs: `DEFERRED_CFS`, `LayerPart::Main` / `Deferred` and `split_deferred` now say "plus the book CF with
  TORUS_CKPT_DEFER_BOOKS=1". `DEFERRED_CFS` is still the constant pair, because retention.rs:231 and the
  existing tests use it.
- Unit tests (flag-independent, pure):
  - `parse_defer_books_table` (:844)
  - `deferred_slot_table_off_is_todays` (:887): only positions and balances.
  - `deferred_slot_table_on_adds_only_the_book_slot` (:899)
- The comment at :917 records that a shell exporting the flag breaks two existing tests:
  `split_merge_equals_whole_merge_random` and `deferred_part_blocks_only_deferred_reads`, which uses books as
  its non-deferred CF. The other env knobs have the same issue. `retention.rs`'s split test is not affected
  (it has no book rows).

`crates/torus-state/tests/ckpt_defer_books.rs` (new; its own test binary, so its own process)
- `defer_books_env()` uses `Once` to call `set_var TORUS_CKPT_DEFER_BOOKS=1` and remove `TORUS_BOOK_ROWS` and
  `TORUS_RESIDENT_BOOKS`. Every test calls it first, before it opens a DB or touches a layer.
- The tests use the public API only.
- (a) `split_merge_with_books_deferred_equals_whole_merge`
  - 300 random blocks over 6 CFs, a third of the ops on books with blob-sized values, writes and tombstones,
    on a seeded DB.
  - After every block, split + `merge_part(Main)` + `merge_part(Deferred)` + `absorb` gives the same
    bytes, entry count and top as a whole `merge`.
  - Every 50 blocks, every key is read with get, len, full scan, prefix scan and prefix_exists, and must
    equal the model. The reads go through three overlays: the whole layer, the split layer before the
    absorb (main + `DeferredLayer::ready(part)` via `with_split_layers`), and the absorbed layer.
- (b) `split_moves_book_rows_to_the_deferred_part`: after a split, the main part has no book writes or
  tombstones and the part has all of them, plus positions and balances. Markets and sessions stay in main.
  This is checked by `append_to_batch` of each part into its own seeded DB.
- (c) `book_reads_wait_for_the_deferred_part`, with `DeferredLayer::pending`:
  - Markets and sessions reads (get, len, scans) answer at once, with no `on_block` call and the part not
    settled.
  - A book point read and a book prefix scan each block, on their own threads. Both call the `on_block`
    seam, and neither answers before the part is sent. After it is sent they answer as the whole layer
    would.
  - `take_read_waits` counts 2. Later reads are not counted.
  - With the sender dropped, book reads are `Err`, and markets and sessions still answer.

No edits to app.rs, backend.rs or torus-bridge. Routing changes for every L path together, because each one
reads the same `is_deferred_slot` table:
- `split_deferred` (checkpoint_layer.rs, part of the split)
- `LayerPart::holds` (merge_part)
- `NativeStateOverlay::layer_for` (backend.rs:1431-1442), which handles every point read and scan of L

## Why (evidence, read on c612dc7f)

- At the start of a block, `CkptState::settle` (app.rs:135-158) waits for the merge thread's Main part.
  `spawn_layer_merge` (app.rs:359-392) merges Main, sends it, and only then merges the Deferred part.
- `merge_cf` (checkpoint_layer.rs, `clone_from` / `clone`) copies every value.
- Book blobs are 77% of state bytes: about 199 per block at about 44 KB each (results doc sec 41.1,
  lines 4299-4300). Today they sit in Main, so E waits for their copy.
- Measured `checkpoint_settle_wait` per native block:
  - 9.04 ms (summary phase) / 8.8 ms (metrics) in the 600 s soak (sec 61.2, line 7948)
  - 4.44 ms (main) / 3.75 ms (Phase 4) in SAT at 300 m (sec 67.3, line 9341)
  - 3.09 ms after the p3 merge-wait fix (sec 54.2, line 6734)
- Nothing on a native block reads the book CF through L, in Classic mode with resident books:
  - The Classic save writes whole blobs and reads nothing (native_executor.rs:6256-6279).
  - Books stay in memory between blocks.
  - `load_order_books` (:5589-5598) runs only when the resident holder is rebuilt.
  - `compute_native_state_root` reads RocksDB directly (state_root.rs:138-160).
  - The ingress `ReadView` has no L (read_view.rs holds only C).
- The deferred part is already almost always ready by the end of the block. Sec 54.2: deferred join
  0.19 ms per block, and `deferred_read_waits` was 0 on all 12 node-cells. So there is about 300 ms of slack
  to absorb roughly 9 ms more merge work.
- Memory 1e1cde98 (s114 p3 merge wait): deferring the book CF already tested OK in a Classic+resident
  fixture. It was kept in Main only because `TORUS_RESIDENT_BOOKS=0` reloads the books every block. This
  patch gates on exactly that.

## Determinism

The change is node-local scheduling only, and the layer's contents are unchanged:
- (a) shows split+absorb == whole merge on rows, bytes and top.
- The checkpoint flush writes the whole absorbed layer in CF-index order (`PendingState::append_to_batch`,
  backend.rs:672-696), so the checkpoint batch is byte-identical.
- What reads return is unchanged. A read that reaches a deferred CF through L waits for its part and gets
  the answer the whole layer gives (c).
- A node whose gate disagrees with its executor's mode only changes its own timing.

## Expected gain (from the brief, not measured)

- `exec_checkpoint_settle_wait_seconds` per native block: soak 8.8 -> under 1.5 ms; SAT 3.1-4.4 -> under
  1 ms.
- `block_ms`: SAT -2.5 to -3.5 ms (about -1.5%, inside n=4 noise); soak -7 to -11 ms (-2.5 to -3.5%),
  growing with book depth.
- Native blocks/s +2-5%, commit-interval p50 -3-5%.
- matched/s likely ties (sec 54.1). Commit p99 does not move: its floor is the 1.2 s outside-window tail of
  sec 72.5 plus the checkpoint windows of sec 66, and this does not touch either.

## How to measure

- Same binary, A/B on the env only: `TORUS_CKPT_DEFER_BOOKS` unset vs `=1`. Keep Classic books (no
  `TORUS_BOOK_ROWS`), resident books on, checkpoint interval 15 s, jemalloc, commit-tail mode.
- Cells, either of:
  - a 600 s soak at 300 markets, n >= 2 per arm, ABBA
  - SAT at 300 m with pre-filled deep books, n = 4 paired ABBA
- Check that the `=1` arm's node log has `TORUS_CKPT_DEFER_BOOKS ... defer_books=true`.
- Primary: exec `block_ms`, native blk/s, commit-interval p50, and `checkpoint_settle_wait` per native block
  (summary phase and metrics delta).
- Guards:
  - `torus_exec_checkpoint_deferred_read_waits_total` must stay 0 (a non-zero value means something reads
    books through L; see the risks).
  - `torus_exec_checkpoint_deferred_join_seconds` should stay under 1 ms mean per block.
  - Watch engine ms per 1k fills: the book memcpy now runs on the merge thread while E executes, and sec 54
    saw engine +3.4% when positions moved there.
  - Nodes agree (same state hash).
- Secondary: matched/s, commit p99, RSS.

## Risks

- **EVM book precompile.** precompiles.rs:734-736 `book_levels` reads `CF_NATIVE_ORDER_BOOKS` through the
  block overlay (app.rs:3290 builds one overlay per block for native and EVM). An EVM tx that calls the
  book reader early in a block now waits for the whole deferred part: positions + balances + books. Today
  that wait is only for Main at block start. Correctness is unchanged and the read_waits counter shows it.
  Under item 7 lanes (EVM every 5th height) this could make such blocks slower if they call the precompile
  heavily. Bench workloads with no book-precompile calls do not see it.
- **Resident holder rebuild** (startup, after a fatal block, or after a non-successor drain).
  `load_order_books` scans books through L and waits for the deferred part. This is rare, it is a
  multi-second path anyway, and it is counted as one read wait.
- **CPU and memory-bandwidth contention.** About 9 MB of memcpy per block moves from "E waits" to "runs
  beside E". The engine may slow a few percent (sec 54 precedent), which eats part of the gain. This is the
  main reason the gain may land at the low end.
- **Short or empty blocks.** The end-of-block `settle_all` join may now wait for the rest of the book merge
  when a block finishes in under about 10 ms. That is the same work moved from the next block's start, so it
  is no worse in total.
- **Ingress slice timing.** The slice cut (spawn_layer_merge after the deferred send) starts about 9 ms
  later. It is installed only at end-of-block upkeep after `settle_all`, so I do not expect a visible wait
  (inferred, not measured).
- **Stale docs.** The `CkptState` doc (app.rs:75-81) and the `spawn_layer_merge` doc (:356-358) still say
  "positions, balances". Fix them in a follow-up after the propose-defer worktree merges (left out on
  purpose: no app.rs edits).
- **Test env.** A shell that exports `TORUS_CKPT_DEFER_BOOKS=1` breaks two unit tests in checkpoint_layer.rs
  (documented). torus-consensus app tests that assert read_waits == 0 with book reads may also break
  (inferred; not checked one by one).

## Untested

- Nothing was compiled or run: the unit tests, the integration tests and the existing suites.
- Not checked with `TORUS_CKPT_DEFER_BOOKS=1` on: the torus-consensus app tests (DeferredGate seam) and the
  torus-bridge tests (inferred unaffected with the flag unset, which is the default).
- No bench numbers. The gains above are the brief's estimates from secs 41.1, 54, 61.2 and 67.3.

## Compile confidence: high

- The changes are small and use only types and functions already in scope: `OnceLock`, `NUM_CFS`,
  `crate::cf`, and `tracing` (already a dependency of the crate).
- The integration test uses only pub items that unit and integration tests already use the same way:
  - `torus_state::{CheckpointLayer, DeferredLayer, FrozenPending, LayerPart, NativeStateOverlay,
    StateBackend, StateError}`
  - `torus_state::db::StateDb`
  - `torus_state::cf::*`
  - `rocksdb::WriteBatch` (a normal dependency, usable from tests/ as in prefix_scan_bound.rs)
  - `tempfile`
- Edition 2021: `std::env::set_var` / `remove_var` are safe functions.
- The main things to watch on first build are type inference on the tuple `assert_eq!`s and
  `Some(&[u8; N])` coercions. Both follow patterns already used in checkpoint_layer.rs's tests.

## Build and test results (s123 builder, on `2bc88bcf`)

- Built as written; no code change was needed (only the env-assumption comment in `checkpoint_layer.rs` grew).
- Flag off: `-p torus-state` 344/344; `cargo nextest run --workspace` 3870/3870 (0 flaky), doc tests pass. That
  includes `tests/ckpt_defer_books.rs`, which sets the flag in its own process.
- Flag on (`TORUS_CKPT_DEFER_BOOKS=1` exported), `-p torus-consensus -p torus-state --no-fail-fast`: 762 run,
  759 passed, 3 failed. All three hard-code a book row as a main-part (non-deferred) CF, so they fail by design
  under the flag, not because of a bug: torus-state `split_merge_equals_whole_merge_random` and
  `deferred_part_blocks_only_deferred_reads` (predicted above), and torus-consensus
  `layer_merge_hands_l_back_before_the_slice` ("main part: the book", left 0, right 1). Owner decision: leave them
  env-sensitive like the other knobs' tests, or rewrite them to route by the live table.
- `cargo clippy --workspace --all-targets`: no new warnings vs main.
