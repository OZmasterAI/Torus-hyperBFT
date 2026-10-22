# s80 close-merge-removal (`TORUS_S80_CLOSE_STACK`)

Branch `s80/close-merge-removal` from 29bfc705. Worktree `/home/oz/projects/wt/novel-s80/proto/close-merge-removal`.
Target dir `/home/oz/.cargo-target-s80-close-merge-removal`. Spec: rank #6, merged from ckpt-close-stack
(`ideas/r1-deep-book.md` section 1) and lazy-window-shared-cf-maps (`ideas/r1-storage-durable.md` section 3).

## 1. Premise check on 29bfc705

These references were re-located in the 29bfc705 worktree, not taken from main.
- Window close, `crates/torus-consensus/src/app.rs:3883-3891` (base numbering). When a trigger fires, E runs
  `layer.merge(p)` for n-1 and `layer.merge(c)` for n on the exec thread, then calls `checkpoint_submit`, which runs
  the L3.1 coalesce `put_raw` and the W hand-off.
- `checkpoint_drain`, `app.rs:4013`: `layer.merge(&top)`.
- `CheckpointLayer::merge_part` / `merge_cf`, `crates/torus-state/src/checkpoint_layer.rs:142-170`, clones every
  key and value of the frozen set into L.
- The close fires at `height % 100` (`STATE_HASH_CHECKPOINT_INTERVAL`), so at 22 blk/s that is 1 close per 100 blocks.

Step 1 premise microbench: `ubench_close_merge_vs_stack` in `crates/torus-state/tests/ckpt_close_stack.rs`, release
build, `LD_PRELOAD=/usr/lib/libjemalloc.so`.
- Shape: L is built by 98 block-sized merges of 700k position keys (52 B key, 100 B value) plus 175k balances. That
  gives 875k entries, 119 MB, with the B-tree nodes scattered over the heap.
- p (n-1) has 5.5k positions, 1.4k balances, 0.9k orders and about 1% deletes.
- c (n) has the same rows plus 300 Classic book blobs: 367 KB each at the deep shape (the L6.1 %100 full save, 113.75
  MB) or 44 KB at the std shape (14.5 MB).
- Each iteration builds a fresh L (untimed) and thrashes 256 MB of cache. It then times one arm's E-side close, the
  W-side `append_to_batch` into a temp RocksDB and the DB write. The two arms alternate ABBA inside the run.
- Iteration 0 asserts that full CF scans of the two DBs are byte-identical.

Kill criteria:
- E-side merge < 10 ms at the deep shape: NOT met. It measured 17.0 ms.
- W batch build +> 60 ms: NOT met. The stacked build is equal or faster.

So the premise holds, but the effect is smaller than the write-up's corrected gain. The deep shape gives about 17 ms
per close, which is -0.17 ms/blk at 1 close per 100 blocks. The write-up claimed -0.45..-0.55 ms/blk.

Note: the earlier session's `premise2` failure ("stacked batch != merged batch") was a bench bug. Both arms built L
from one shared rng, so their L values differed. The fix seeds L per iteration, and the scans then compare equal.

## 2. Design (step 2, behind `TORUS_S80_CLOSE_STACK`, default off = 29bfc705 behaviour)

The flag is read once per process (`torus_state::close_stack_enabled`, `checkpoint_layer.rs:215`). It logs
`TORUS_S80_CLOSE_STACK on=<bool>` at INFO and is resolved at startup from `configure_book_coalesce` (`app.rs:4190`).

**`CheckpointLayer`** (`checkpoint_layer.rs`):
- New fields:
  - `tail: Vec<Arc<FrozenPending>>`, at most `MAX_STACKED` = 2.
  - `head: Option<Box<PendingState>>`: rows `put_raw` writes after stacking, i.e. the L3.1 coalesced books at the
    close.
  - `tail_bytes` and `head_bytes`.
- `push_frozen(Arc<FrozenPending>)` is O(1) on E: an Arc clone plus one length walk for the byte count. It is not a
  merge.
- Reads resolve newest first: head, then the tail newest first, then the base. This covers `lookup`,
  `states_newest_first` and `lookup_row`.
- `append_to_batch` writes the base, then the tail in order, then head. RocksDB applies batch entries in order, so
  last-write-wins gives exactly the merged DB.
- A stacked layer refuses `merge_part`, `split_deferred` and `absorb` (assert). L is never stacked; only C is.
- `bytes()` = own + tail, an upper bound used for the metric. The retention share counts own bytes only, because each
  stacked block keeps its own tracked share, so nothing is double counted.

**Overlay and ReadView** (`backend.rs`, `read_view.rs`):
- `lower()` now has `LOWER_SLOTS` = 6 slots: parent, L, then C's 4 sets newest first. A merged C fills one slot.
- The point read goes through `CheckpointLayer::lookup`.
- Scans, `prefix_exists` and `iterate_cf_from` work on the slot array unchanged, with newest-first precedence.
- `ReadView::lower` expands C the same way.

**Checkpoint flush** (`backend.rs`):
- `flush_checkpoint_batch` passes the stacked sets as `upper` to `flush_stack_after_batch`.
- The batch is built in order.
- The trie dirty map extends newest-wins.
- `dirty_entries_by_cf` counts distinct keys (`native_dirty_counts_stack`).
- The oracle-submission prune and the s99 (c) scanned-delete counts consider only deletes that stay the key's newest
  entry in the stack (`newest_in_stack`). That reproduces the merged set's deletes.
- A stacked flush is only allowed with checkpoint `ChainSrc::Rows`, i.e. no digest of its own (assert).

**E** (`app.rs`): when the flag is on, the close and `checkpoint_drain` call `push_frozen` instead of `merge`.
Everything else is unchanged: the `coalesce_unsaved_books` `put_raw` goes into head, the W job, C in the overlay and
the view, and the dropper.

## 3. Diff summary

`git diff --stat` against 29bfc705, plus the new test file:
- `crates/torus-consensus/src/app.rs` +48: the close and the drain switch, the startup flag resolve, and the new test
  `close_stack_flag_decides_the_closing_layer`.
- `crates/torus-state/src/backend.rs` about +200: `LOWER_SLOTS` lower(), the flush over a stack, distinct dirty
  counts, the newest-in-stack prune and delete filters, and the `probe_exists` slot loop.
- `crates/torus-state/src/checkpoint_layer.rs` about +180: tail, head, `push_frozen`, the stacked reads, batch and
  bytes, and the guards.
- `crates/torus-state/src/read_view.rs` +11, `lib.rs` +4.
- `crates/torus-state/tests/ckpt_close_stack.rs` (new):
  - 3 correctness tests:
    - `stacked_close_equals_merged_close_random`: 60 seeds of put/delete/put chains. It covers every overlay and
      ReadView read, `lookup_row`, scans, `prefix_exists` and `iterate_cf_from` while C is in flight, plus
      `flush_checkpoint`: DB scans over all CFs and dirty counts. It includes drains and L3.1 `put_raw` on top.
    - `stacked_append_to_batch_equals_merged`: 40 seeds.
    - `stacked_layer_refuses_merge_and_overfill`.
  - 2 `#[ignore]` microbenches: `ubench_close_merge_vs_stack` and `ubench_reads_through_stacked_c`.

Bugs found and fixed in this session, on top of the earlier session's uncommitted work:
1. `put_raw` on a stacked layer counted head bytes in a fresh local starting at 0. An overwrite or delete would
   underflow (a panic in debug). Head now has its own exact `head_bytes`.
2. The retention share used `bytes()`, which includes the tail, so stacked blocks were double counted. It now uses
   `own_bytes()`.
3. `probe_exists` (test-probes) hard-coded `0..3` slots. With 6 slots, C's base sits in slot 5, so C hits were dropped
   from probe counts. This failed `app::crash_recovery_tests::o0_poison_readers_answer_from_every_layer` even with the
   flag OFF. The loop now runs over `0..LOWER_SLOTS`.

## 4. Tests

Commands use `F="--cargo-quiet --status-level fail --final-status-level fail --hide-progress-bar"`, all on 18c under
the heavy lock.

| run | result |
|---|---|
| `cargo nextest run -p torus-state --no-fail-fast` (includes the 3 ckpt_close_stack tests) | 421/421 passed, 7 skipped |
| `cargo nextest run -p torus-consensus --no-fail-fast` (flag off) | 622/622 passed, 7 skipped |
| `TORUS_S80_CLOSE_STACK=1 cargo nextest run -p torus-consensus --no-fail-fast` (flag on: every checkpoint test then stacks, including the b0/b1 per-block-vs-checkpoint DB and hash differentials and crash recovery) | 622/622 passed, 7 skipped |
| first flag-off consensus run, before fix 3 | 334 passed, 1 failed (`o0_poison_readers_answer_from_every_layer`), fail-fast |
| golden `perf_equivalence_golden`, flag off / on | PENDING |
| `close_stack_flag_decides_the_closing_layer`, flag off / on (proves the arm: C `is_stacked()` == flag at every close) | PENDING |

## 5. Measurements (raw)

### 5.1 E-side close vs stack, W build, W write

Each line is the median over 6 ABBA iterations inside one run; 3 runs per shape.

| run | E merge ms (mean) | E stack ms | W build merged / stacked ms | W write merged / stacked ms | batch bytes merged / stacked |
|---|---|---|---|---|---|
| deep r1 | 16.98 (17.04) | 0.1617 | 139.30 / 134.21 | 439.9 / 439.3 | 235,748,046 / 237,329,001 (+0.67%) |
| deep r2 | 17.03 (17.05) | 0.1627 | 147.86 / 134.02 | 443.5 / 441.6 | same |
| deep r3 | 17.13 (17.12) | 0.1618 | 146.71 / 147.59 | 443.8 / 444.2 | same |
| deep premise3 | 17.18 (17.81) | 0.1664 | 130.69 / 121.92 | 436.2 / 433.1 | same |
| std r1 | 9.89 (9.94) | 0.1622 | 117.76 / 106.85 | 292.5 / 305.9 | 136,522,446 / 138,103,401 (+1.16%) |
| std r2 | 9.84 (9.78) | 0.1638 | 108.60 / 105.49 | 292.0 / 304.8 | same |
| std r3 | 9.89 (9.90) | 0.1633 | 110.23 / 105.17 | 293.1 / 306.2 | same |
| std premise3 | 9.88 (10.06) | 0.1692 | 101.27 / 100.15 | 289.4 / 302.0 | same |
| deep, 15k positions/blk | 29.42 (29.53) | 0.4664 | 158.26 / 145.26 | 443.1 / 466.6 | 236,628,819 / 240,902,230 (+1.81%) |

Means of the r1-r3 medians:

| shape | E per close | W build | W write |
|---|---|---|---|
| deep | 17.05 -> 0.162 ms (-16.9 ms, -99%) | 144.6 -> 138.6 ms (-6.0) | 442.4 -> 441.7 ms (-0.7) |
| std | 9.87 -> 0.163 ms (-9.7 ms) | 112.2 -> 105.8 ms (-6.4) | 292.5 -> 305.6 ms (+13.1, +4.5%) |

Every run asserted byte-identical DB scans for merged vs stacked.

### 5.2 Read-side cost of a stacked C

While C is in flight, every point read that misses the parent and L probes C.

`ubench_reads_through_stacked_c`, deep shape:
- 200k position reads: 1/3 base hits, 1/3 hits in n-1/n, 1/3 absent (each absent read is an empty-DB miss).
- Cache thrashed between runs; 6 rounds ABBA.

| arm | per-round ns/read | median | mean |
|---|---|---|---|
| merged | 641.5, 643.4, 629.9, 644.1, 639.2, 634.4 | 641.5 | 638.8 |
| stacked | 879.4, 881.2, 891.2, 889.4, 867.0, 880.0 | 881.2 | 881.4 |

That is **+243 ns per read that reaches C** (+38%).

How many reads reach C? A temporary counting build of `ubench_econ` (atomic counters in `lookup_lower_as`, reverted,
not committed; run with UB_RUNS=1 and `ub.sh` defaults) gave:
- **2 overlay lower-layer point reads per block**;
- **1 that passes the parent** (which in the node would go on to L, then C, then the DB).

Resident R serves the engine's position and balance reads, so the read-side cost on E is about 0.5 µs per block while
C is in flight: negligible. Ingress ReadView reads (mempool admission, on ingress threads, not on E) also pay +243 ns
during C's flight. That was not measured in a node.

### 5.3 Off-path engine proxy A/B (flag off; does the 6-slot `lower()` cost anything?)

ub.sh, 300 markets, UB_MARKS=1, UB_RUNS=5, TORUS_SETTLE_STREAMED=1. Order h2h (29bfc705), cmr, cmr, h2h, h2h, cmr.

PENDING

## 6. Expected effect

- One close per 100 blocks at 22 blk/s:
  - **std:** -9.7 ms per close, -0.10 ms/blk, about -0.25% of a ~38 ms block.
  - **deep:** -17 ms per close (-29 ms at 15k positions per block), -0.17..-0.29 ms/blk, about -0.4..-0.65%.
- The close block is the upkeep tail: closes are 1% of blocks, so the upkeep p99 is about the close upkeep. Std close
  upkeep sits in 12.8-25.6 ms; deep upkeep p99 is 30 ms. Both should fall by roughly the measured 10 / 17-29 ms.
- E no longer clones the %100 block's ~110 MB of deep book blobs into C, so transient RSS on deep drops by about 110 MB
  per window.
- Costs:
  - W write +13 ms at the std shape (+4.5%) and +23 ms at deep 15k, from duplicate keys in the batch (+0.7..1.8%
    bytes). This is off E, and inside the ~0.3-0.45 s W checkpoint.
  - Reads through C while in flight: +243 ns each, about 1-2 per block on E.
- Engine proxy (ubench_econ): no close path, so no gain is expected or visible there. The A/B in 5.3 only checks for
  off-path regression.

## 7. Verdict

PENDING (filled at the end of the stage)
