# r1 lens: deep-book (depth-proportional costs, flat p99 under book growth)

Base: main `eb993e3f` (read-only). Batch-10 refs were read with `git show origin/merge/s136-batch10:<path>` and are
marked "b10".

Inputs:
- `map/measured.md`, `map/state.md`, `map/exec.md`;
- a ledger grep (the terms are listed per idea);
- memory (search only);
- RD = `docs/perf/ozarchy-antispam-item6-pf1-2026-10-04.md`.

## What the deep residual is (from the measured numbers)

- **Exec-thread gap, deep minus standard (CL21, measured.md 3.1): +1.92 ms/blk.**
  - save_books +0.65 and upkeep +0.52 = 1.17 ms/blk (61%). Both are the window close / %100 full save.
  - The rest has no profile (9m is excluded): phase1 +0.27, margin +0.16, match +0.12, settle +0.10, tail +0.12.
- **Close cost per close.**
  - Deep: 59-69 ms mean, p99 126-212 ms. Standard: 4.6-5.0 ms (RD s110.6, s117.4, s120).
  - Derived close spike including the merges: ~50 ms standard, ~92 ms deep (state.md:138).
  - The parts of the close are not timed separately.
- **What happens on E at a close.**
  - E synchronously folds blocks n-1 and n into L (`app.rs:4108-4114`). Each fold clones every key and value
    (`checkpoint_layer.rs:142-169`).
  - Under L6.1 (b10, owner-decided), block n is the %100 block. It carries all ~300 Classic blobs, ~110 MB on deep
    (b10 `native_executor.rs:8009-8070`). So the close also clones those blobs on E (b10 `app.rs:4279-4282`).
  - RD s120.5 left +1.5% deep exec on-CPU of B vs A unattributed. That is ≈ 0.5 ms/blk ≈ 50 ms per %100.
- **Most of the deep book is cold.**
  - ~1.0M of the ~1.14M deep resting orders sit on levels the load never touches: 100 levels a side, >= 1,000 ticks
    from the +-5-tick load band (`tools/bench-throughput/src/preload.rs:16-22`, test `:643-655`).
  - Real books behave the same way: far levels rarely change.
- **The Classic encode is memory-bound, so copying cached bytes is not enough.**
  - 8k-explore-log.md:25 says so directly.
  - Parked item 6 Phase 5 step 3 (owner list) built a per-level Classic byte cache plus a copy-all assembly. Its
    all-clean floor was ~30% of the reference (mem `5e4def28`; branch `origin/feat/item6-phase5-step3`, plan line
    ~605).
  - To be flat in depth, the cold bytes must not cross E at all.

---

## 1. ckpt-close-stack: close a window without merging; W writes [L, n-1, n] in order

**Mechanism.**
- Stop running `layer.merge(p)` and `layer.merge(c)` on E at the close (`app.rs:4108-4114`).
- C becomes `CheckpointStack {base: L taken whole, tail: [Arc<FrozenPending> n-1, n], books: coalesced layer}`.
- W appends it as one WriteBatch in that order, then the side rows, marker and hash rows, as today
  (`exec_pipeline.rs:1217-1298`).
- RocksDB applies batch entries in order, so last-write-wins reproduces `merge_cf`: later puts replace earlier ones,
  later deletes tombstone them. The DB state is byte-identical.
- Readers of C probe the tail newest-first, then the base: the overlay layer chain (`backend.rs:1601-1607`), the
  ReadView's inflight layer (`app.rs:4185`) and `lookup_row` for the EVM prefix (`checkpoint_layer.rs:374`).
- Under L6.1 the %100 block's ~110 MB of blobs is never cloned. C references the frozen block that W already holds
  for its digest job.

**Expected gain.**
- **Merges removed from E:** 2 block merges per close. The 18c probe measured 84-114 ms per merge at 2x the
  section-41 block sizes (mem `020b56cd`). Today's blocks are ~4x smaller, so this is ≈ 10-25 ms each.
- **Standard:** 20-45 ms per close, i.e. 0.2-0.45 ms/blk, +0.5-1.2% matched/s. This fits the ~45 ms non-encode
  close spike (~50 - 5).
- **Deep under L6.1:** add the 110 MB clone, ~17-50 ms. Total 0.4-0.95 ms/blk, +1-2.4% deep matched/s.
- **Tails and memory:** close p99 -20-90 ms. The upkeep p99 falls toward the merge-free level. C stops holding a
  second 110 MB copy (deep RSS -110 MB per window).
- **Scaling:** at 10M resting, today's L6.1 clone would be ~1.1 GB on E per %100. The stack makes it 0.

**Nearest prior.** Ledger grep terms: close|merge.*checkpoint|layer stack|last wins|sync merge|persistent|cow.
- `phase3-merge-wait-fix` (the per-block merge moved to a thread; the close merges stay synchronous).
- `layer-per-cf-rwlock-or-persistent-map` (concurrent-read L / move instead of clone; proposed).
- `defer-book-blobs-layer-merge` (books into the deferred part; the close still merges All on E).
- Q6/A1 checkpoint slicing (excluded; it splits the write, not the merge).

The difference: there is no merge at the close at all, because the batch order does the fold.

**Risks.**
- Every reader of C must handle the stack. Byte and retention accounting must count the tail.
- The batch carries duplicate keys (+p+c, ~2-4 MB, +2-4% WAL per close).
- `checkpoint_drain` should use the same stack.
- The test needed: merged-batch DB == stacked-batch DB on random sequences, including put/delete/put chains.

**Effort.** 3-5 person-days.

**Prototype (<= 3 h).** Write a torus-state microbench:
- Build an L of ~700k position-like keys and two frozen sets of 10k rows.
- Time `merge(p); merge(c)` against building the stack (Arc clones).
- Write both versions into two temp RocksDB instances and assert that full scans are equal.
- Repeat with c carrying 300 x 367 KB blobs (the L6.1 case).
- Record E-side ms, W build ms and batch bytes.

---

## 2. classic-cold-level-rope: zero-copy hand-off of cached Classic level bytes

**Mechanism.**
- Take the parked step-3 per-level byte cache:
  - it is invalidated by `level_journal`, which is captured at every book primitive (`order_book.rs:1176-1184`);
  - it must keep the journal across a window instead of discarding it per block (`native_executor.rs:7314-7318`).
- Stop its copy-all assembly, which is the 30% floor.
- Emit the blob as `BookRope {header, Vec<Arc<[u8]>> level segments, stops}` instead, carried as a shared value for
  CF_NATIVE_ORDER_BOOKS:
  - L3.1: into the closing layer (`app.rs:4345`);
  - L6.1: into the %100 block's PendingState, which idea 1 then passes to W uncopied.
- W streams the segments:
  - into SHA-256, with the same framing (`running_hash.rs:150-156`), so the hash is identical;
  - into the batch with `rocksdb_writebatch_putv_cf` (c.h:807), or by assembling on W.
- Seeding is staged like the sponge cache (`order_book.rs:1086-1120`). E's close cost becomes O(levels + hot bytes).

**Expected gain (deep).**
- **Close:** 59-69 ms today. Step 3 alone gets ~23-26 ms (0.3 x 88% cold + 12% hot). The rope gets ~7-12 ms: about
  the standard close of 4.6-5.0 ms plus 63k Arc clones at 2-4 ms.
- **Throughput vs main:** 0.5-0.6 ms/blk, +1.2-1.5% deep matched/s, about half of the -2.9% gap. Close p99 drops
  from 126-212 ms to ~15-25 ms.
- **Beyond step 3:** ~0.15 ms/blk at 1M. At 10M cold, the step-3 floor is ~0.2 s per close and the rope stays at
  ~10 ms.

**Nearest prior.** Ledger grep terms: classic|blob|segment|rope|memo|cache.*encod|cold|unchanged|copy.
- `phase5-step3-classic-save-dirty-levels` (parked, owner list): the same cache, but it copies every level, so it is
  O(depth) at ~30%.
- `level-hash-sponge-cache` (keccak state, mode 2 only).
- L3.3 (O(depth)).
- Parallel save B (+3.5-7.5% CPU).
- `book-buffer-move-into-overlay` (put copy only).

The difference: the cold bytes are shared by Arc from cache to RocksDB and are never copied on E. Caution: this
depends on the parked step-3 code.

**Risks.**
- A missed invalidation is a running-hash fork. Mitigations:
  - step 3's reference-check test mode;
  - a sampled full-encode check of one book per close.
- The rope value type touches PendingState, FrozenPending and the layer get path (only for one CF). Rare readers must
  materialize it.
- The memo keeps ~97 MB resident, offset by the copies it removes.

**Effort.** 6-9 person-days: 3-4 on top of step 3, plus rope plumbing.

**Prototype (<= 3 h).**
- In a scratch worktree off `origin/feat/item6-phase5-step3`, add a rope arm to `crates/torus-core/tests/
  classic_row_ubench.rs` (or `benches/book_encode.rs` on main, which already has P1M with 100 levels a side).
- Each rep, mutate 6 hot levels per side per book.
- Compare reference vs cached copy vs rope (Arc clones), at 1M and 4M cold orders.
- Assert that the concatenated rope equals the reference.
- Pass: the rope is <= 15% of the reference and flat in cold depth.

---

## 3. book-blob-cdc-store: node-local content-defined chunk store for Classic book blobs (W side)

**Mechanism.** Only W changes. Where W appends CF_NATIVE_ORDER_BOOKS values (`backend.rs:724-726`):
- Parse the fixed-layout Classic records (`classic_encode.rs:29-35`).
- Cut chunks at level boundaries and at records whose order-id hash is 0 mod 64.
- Key the chunks `(market, xxh3-128)` in a new node-local CF.
- Put only the chunks absent from W's in-RAM manifest for that market, delete the ones that left, and store a small
  manifest as the book-CF value.
- All of this goes in the same atomic checkpoint batch.

A StateDb read shim for that CF reassembles the exact Classic blob for boot, RPC and precompile readers. The running
hash is computed from in-memory writes (`running_hash.rs:114-176`) and is unchanged. A format marker goes next to
`BOOK_MODE_MARKER_KEY`.

**Expected gain (deep).** Book bytes per checkpoint drop from ~110 MB to ~10-25 MB of hot chunks.

| metric | deep today | with the store |
|---|---|---|
| checkpoint batch | 174.5-181 MB | ~85-95 MB (standard is 86.8) |
| W state write | 7.26 ms/blk | ~4.3 ms/blk |
| checkpoint db.write | 447 ms (max 1.27 s) | ~290 ms (max ~0.85 s) |
| WAL per node | 55.1 MB/s | ~38 MB/s |
| book-CF flush per run | baseline | -~2 GB |
| commit persist per call | 6.2 ms | ~4.6 ms |

- Commit persist is header and body, which queue behind the batch (state.md 2c). Faster persist means fewer
  >100 ms persist-ticket waits on E at closes.
- RSS: -~95 MB of WriteBatch.
- Cost: parse + xxh3 adds ~15-25 ms per window on W.
- At 10M resting, today's batch would be ~1.2 GB per window. W would be busy ~4 of every 4.7 s and the one-in-flight
  rule (`exec_pipeline.rs:854-869`) would stall E's closes. With the store the batch stays at hot size.

**Nearest prior.** Ledger grep terms: blob|delta|segment|snapshot|wal|compress|dedup.
- `book-persist-dirty-only-snapshot` (rejected at s351 at save_books 0.07 ms/blk: per-level keys).
- Book modes 1-3 (consensus rows; excluded).
- `da-cf-blob-files` / BlobDB (still writes every blob every window).
- `wal-compression-zstd` (parked; O(depth) bytes plus CPU).

The difference: storage-level dedup, hash-preserving, no E change.

**Risks.**
- Every DB reader of the book CF must use the shim.
- An old binary on a new DB must be refused.
- Chunk GC must stay in the same batch.
- Classic only.
- The mean matched/s gain is small; the value is tail and scaling.

**Effort.** 4-6 person-days.

**Prototype (<= 3 h).**
- Offline chunker sim on P1M books from the `book_encode` builder with per-window hot mutations. Report new bytes
  per window and chunker MB/s.
- A Rust RocksDB microbench: a 181 MB vs 90 MB batch while another thread does 3 ms writes every 45 ms. Record that
  thread's p99 and max.

---

## 4. ckpt-batch-presize: build the checkpoint WriteBatch at its final size

**Mechanism.**
- Today W starts each checkpoint from `rocksdb::WriteBatch::default()` (`exec_pipeline.rs:1234-1235`;
  `checkpoint_layer.rs:401`). RocksDB's `rep_` std::string then doubles its way to 87 / 181 MB.
- On deep that means ~255 MB of regrowth memcpy, a 128 + 256 MB transient, fresh-extent page faults, and 75 MB of
  slack held through the write.
- Instead, use `WriteBatch::with_capacity_bytes(layer.bytes() + side + 16 x entries + 1 MiB)` (rust-rocksdb 0.24
  `write_batch.rs:170`). `layer.bytes()` is already exact.
- Pass the same hint into `evm_trie_prefix`.

**Expected gain.**
- W build: 160 -> ~100-130 ms per deep checkpoint (95 -> ~70 standard).
- Checkpoint peak RSS: -~200 MB deep / -100 MB standard (VmHWM 5.29 vs p50 3.9 GB, state.md corner 14).
- Less memory-bandwidth contention with E right after a close.

**Nearest prior.** Ledger grep terms: presize|with_capacity|writebatch|batch build.
- `classic-borsh-presize` / L3.3 (the blob Vec).
- Q6/A1 slicing (excluded).
- `chunk-exec-flush-batches`.

The difference: one batch, sized right.

**Risks.** An over-estimate wastes RAM, bounded by the hint. Low risk.

**Effort.** 0.5 person-day.

**Prototype (<= 1 h).** A Rust microbench: build a 181 MB batch from a synthetic 1M-entry layer with default vs
presized creation. Record build ms and the `getrusage` max RSS delta.

---

## Considered, not proposed

- **Step-3-style memo with copy-all.** It is parked Phase 5 step 3 (owner list) and has a measured 30% floor.
- **Shadow encoder fed with per-block book deltas.** Extracting the deltas costs ~30-40k records/blk x 60 ns ≈
  2 ms/blk on E. That is more than the 0.6 ms/blk amortized close it would replace.
- **Batching the close-time dropper hand-offs (~100 per close into a 2-slot queue, `app.rs:235-240`, `:485`,
  `:4188-4191`).** Probably a free fix, but no evidence it costs anything:
  - If the old view's ~100 slices were freed inline, the spike would be hundreds of ms. The 18c full-set drop is
    11-34 ms each (mem `020b56cd`).
  - The measured close spike is ~50 ms, so the view is not freed inline in most closes.
- **Early-window 1.5x blocks.** A load transient: per placed order, early blocks are cheaper (2.3 vs 2.56 us). Not a
  depth cost.
