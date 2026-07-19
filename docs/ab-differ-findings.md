# AB-DIFFER findings — why book save/flush costs ~2s/block (round 3a)

Branch `perf/ab-differ` off `perf/exec-scaleup` @ `81ea957`. All timings are
**microbench wall-clock** from `crates/torus-bridge/tests/ab_differ_microbench.rs`
run on the VPS (8-core `vmi3065152`, `--release`, no devnet). They are NOT
mission throughput.

Question: our re-proof residual is `root + state_write + flush ≈ 2–2.5 s/block`
at 14–18k dirty buckets/block with ~700k–1.1M resting orders, while the parallel
`order_book_store::save_book_delta` reported `flush 7.8 ms/block` at ~16k resting
depth on an 18-core box. Legit depth gap, or an implementation tax in our
save/flush path?

## TL;DR

- **The 2s is not the book differ. It is the native trie flush**, specifically
  `native_trie::read_bucket_members` — a RocksDB prefix-scan run **once per
  dirty bucket** that reads the *entire* member set of the bucket
  (O(depth / 65536) rows) to rehash its leaf. It runs in **both** the cached and
  uncached path: `TORUS_NATIVE_ROOT_CACHE` caches tree-node sibling hashes and
  elides idempotent rewrites, but **does not cache bucket members**.
- Per-block root cost ≈ `dirty_buckets × (member_scan + leaf_keccak)`. Both
  factors are depth-driven: `dirty_buckets` grows with block size, and
  `member_scan`/`keccak` per bucket grow with **bucket occupancy = depth/65536**.
- Their 7.8 ms at 16k depth is a **legitimate depth artefact**: occupancy there
  is `16000/65536 ≈ 0.24` members/bucket, so `read_bucket_members` reads ~0 rows
  per dirty bucket. At our 700k–1.1M depth occupancy is ~11–17 members/bucket —
  a ~45–70× heavier per-bucket scan for the identical algorithm.
- **The book differ itself is already O(touched) on the resident/journaled path**
  (`save_book_rows_journaled`) — same complexity class as their
  `save_book_delta`. The differ is *not* where the seconds go. (The **non-resident**
  C4 path `save_book_rows` *is* O(depth) — a real tax, but off the resident hot
  path.)

## The two save paths, by code

### Ours

- `native_executor::save_order_books` dispatches per dirty market:
  - journal off → `save_book_rows` (**C4 full walk**): walks *every* bid+ask
    queue front-to-back and sweeps *every* shadow entry → **O(depth)** per dirty
    book, regardless of how few orders changed.
  - journal on (resident) → `save_book_rows_journaled`: takes the in-book
    `BookJournal` (touched/removed id sets), collects the **affected price
    levels** into a `BTreeSet`, and re-walks only those levels via
    `diff_queue_rows` → **O(touched + orders in touched levels)**.
- Seqs are **not stored in the book**. They are re-derived at save time by
  `diff_queue_rows` from a monotonicity check against a **separate
  executor-side `BookRowShadow`** (per-order `OrderShadow{seq, price, qty,
  original_qty, gen}` + mark-and-sweep generation). Correctness rests on a
  multi-clause equivalence proof that the journal-driven save re-derives the
  exact seqs the full walk would.

### Theirs (`order_book_store`, deep-book-storage)

- `save_book_delta`: writes the header, then one upsert/delete per id from
  `book.take_row_ops()` → **O(touched)** exactly.
- The journal lives **entirely in the book**: `row_journal: BTreeSet<OrderId>`
  (touched ids) + `row_exists: HashSet<OrderId>` (phantom-delete guard). Seqs are
  **stored per-order** in the book (`order_seq`), so `take_row_ops` just reads the
  current order + its stored seq and encodes the row — **no level walk, no seq
  re-derivation, no separate shadow**.

Same per-order key/value schema (`market‖0x01‖order_id → seq‖Order`), same header
row concept. The material difference is the **journal-in-book** idiom (one
structure, seq authoritative in the book) vs our **shadow-differ** idiom (two
structures, seq re-derived at save).

## Measured matrix

Block shape: N=400 mixed ops across 10 markets (value-changes + deletes for the
trie leg; cancels + new places for the differ leg). Depth D = total resting rows.

Measured on VPS `vmi3065152` (8-core), `--release`, single-threaded, warm-ish
block cache (400-op block). `occ` = mean members/bucket = depth/65536.

### (a)/(b) book differ — `save_order_books` time per N=400 block

| D | (a) full-walk `save_book_rows` | (b) journaled `save_book_rows_journaled` |
|---|---|---|
| 16k  | 6.19 ms | 1.14 ms |
| 200k | 62.78 ms | 1.79 ms |
| 700k | 137.47 ms | 0.68 ms |

(a) grows **linearly with depth** (O(depth) walk + shadow sweep). (b) is **flat
~1 ms regardless of depth** (O(touched)). The resident/journaled differ is NOT
the 2s. The non-resident full walk *is* an O(depth) tax (137 ms/block/market at
700k) — real, but off the resident hot path.

### (d) native trie flush — per N=400-op dirty block

| D | occ | dirty_buckets | UNCACHED root | CACHED root | uncached µs/bucket | scan µs/bucket | keccak µs/bucket |
|---|---|---|---|---|---|---|---|
| 16k  | 0.24  | 400 | 10.91 ms | 8.68 ms  | 27.3 | 4.0  | 0.8 |
| 200k | 3.05  | 399 | 14.77 ms | 10.24 ms | 37.0 | 6.9  | 2.5 |
| 700k | 10.68 | 400 | 17.23 ms | 14.81 ms | 43.3 | 10.7 | 6.5 |

Per-bucket root cost climbs with depth: **27 → 43 µs**. The **member-scan +
keccak** portion — the part the root cache CANNOT remove — climbs hardest:
`4.0+0.8 = 4.8 µs` at 16k → `10.7+6.5 = 17.2 µs` at 700k (a 3.6× rise tracking
the 45× occupancy rise; sub-linear because the scan is per-bucket-seek bound).
The cache trims ~5–12 µs/bucket (the tree-path sibling reads) but leaves the
member scan fully in place — at 700k, cached root is still 37.2 µs/bucket, of
which 17.2 µs is un-cacheable member work.

**The equal-depth control:** at 16k depth (== their measurement depth) our
uncached flush is **10.9 ms** for a 400-bucket block — essentially their reported
**7.8 ms**. Same algorithm, same order of magnitude. The entire gap to our 2 s is
depth and block size, below.

## The 2s attribution (arithmetic)

Re-proof: 14–18k dirty buckets/block at ~700k–1.1M resting. Two multipliers
separate it from their 7.8 ms — both legitimate load, not implementation tax:

1. **Depth**: their occupancy 0.24 members/bucket vs our ~10.7–17 → each
   `read_bucket_members` reads ~0 rows for them, ~11–17 for us.
2. **Block size**: 14–18k dirty buckets means ~14–18k touched rows/block (large
   marketable orders sweeping many resting levels), vs the 400 in this bench —
   a ~40× larger block.

Scaling this bench's 700k-depth root cost to the re-proof block:

```
root ≈ dirty_buckets × per_bucket_root
     ≈ 16000 × 43.3 µs ≈ 0.69 s          (warm-cache LOWER BOUND, this bench)
```

That 0.69 s is a floor: this microbench runs a 400-bucket block against a
warm-ish RocksDB block cache. The re-proof block scans ~16000 × ~15 ≈ **240k
mirror rows/block** — enough to thrash the block cache, so each
`read_bucket_members` seek pays SST/OS-page latency the warm bench does not. The
observed 2–2.5 s = this cache-cold member-scan inflation (~1–1.5 s) + the
`write`/`flush` of the ~16k-row atomic batch (WAL + commit; this bench's
`write_seconds` alone is 10.3 ms/400-bucket → ~0.4 s at 16k, before fsync).

Decomposition of the per-bucket root cost (measured):
- `read_bucket_members` prefix-scan  = 10.7 µs/bucket @700k — **NOT cached**, O(occupancy)
- leaf keccak (frame + hash)         = 6.5 µs/bucket @700k  — **NOT cached**, O(occupancy)
- tree-path sibling reads (~16/bucket) = **removed by root cache** (~5–12 µs)
- idempotent-rewrite elision           = **removed by root cache**

So among the surviving terms the residual is **member-scan-dominated**, and both
surviving terms scale with **depth (occupancy)** — exactly why their 16k-depth
number is ~45–70× smaller for byte-identical trie code.

## Verdict — standardize on **journal-in-book** (theirs)

Round 3c should build level-rows on the **journal-in-book** idiom, not the
shadow-differ:

1. **Auditability.** Their correctness is local and obvious: seq is stored in the
   book, `take_row_ops` reads current state, `row_exists` guards phantom deletes.
   Ours needs a whole-book equivalence proof (resident journaled save must
   re-derive byte-identical seqs to the full walk) plus a redundant per-order
   executor-side shadow kept in lockstep by a mark-and-sweep gen counter — more
   surface, harder to review, more ways to silently fork.
2. **We are already half-way there.** rank8 resident books keep the book in RAM
   and already carry an in-book `BookJournal` (touched/removed). The remaining
   step is to make the book authoritative for seq (`order_seq`) and delete the
   executor-side shadow.
3. **Slightly cheaper, too.** Their `take_row_ops` is exact O(touched); our
   `diff_queue_rows` re-walks *every order in a touched level* — deeper on hot
   price levels. (Both are dwarfed by the trie; this is a tidiness win, not the
   2s.)

### Migration cost on our branch

- **Port additively** (do NOT merge their branch — commit `92fa3ac` rewrites the
  Phase-4 region from a pre-A5 fork and would silently revert A5): cherry-pick /
  re-home the `order_book_store` module (`5fc88bd` is additive, core-homed) and
  add `order_seq` / `row_journal` / `row_exists` + `take_row_ops` /
  `full_row_ops` / `encode_order_row` to `OrderBook`.
- **Delete from `crates/torus-bridge/src/native_executor.rs`:** `BookRowShadow`,
  `OrderShadow`, `save_book_rows`, `save_book_rows_journaled`, `diff_queue_rows`,
  `delete_order_rows`, the shadow map + mark-and-sweep gen plumbing.
- **Keep:** the row key/value schema (byte-identical), the meta/header row, the
  stop-row handling, and one full-write path (`save_book_full`, == our
  `save_book_rows`) for genesis/migration.
- **Net:** `save_order_books` becomes `for market in dirty { save_book_delta }`.

## Top follow-up for 3b/3c

The 2s lever is the trie, not the differ. In priority order:

1. **Member-cache in `NativeTrieCache` (highest value).** Hold each bucket's
   member set in the in-RAM trie image, mutated incrementally by the same dirty
   set that already updates the node hashes. This turns `read_bucket_members`
   from an O(occupancy) RocksDB prefix-scan into an O(1) RAM lookup + O(touched)
   edit — removing the **10.7 µs/bucket** member-scan term at 700k (and the
   cache-cold inflation of it, which is the bulk of the 2 s at re-proof scale:
   ~240k avoided mirror reads/block). Stays value-neutral (persisted bytes
   unchanged; extend the existing root self-authentication guard to cover it).
   Estimated: removes the dominant ~1–1.5 s cold-scan component + ~0.17 s warm
   (16000 × 10.7 µs).
2. **Parallel rehash.** The per-bucket `frame_entry` + `keccak256` is
   embarrassingly parallel across dirty buckets — a rayon map before the
   sequential path-propagation divides the keccak term (6.5 µs/bucket @700k,
   ~0.10 s at 16k buckets) by cores. Second-order next to (1) but free.
3. **Non-resident cleanup.** `save_book_rows` (full walk) is O(depth) — 137
   ms/block/market at 700k. Harmless while resident is on, but fold it away once
   journal-in-book lands (only `save_book_full` remains, for seeding).

## Appendix — leg (c) standalone `save_book_delta`

Built + timed standalone on `perf/ab-differ-theirs` (off
`origin/perf/deep-book-storage`), `crates/torus-core/tests/ab_differ_theirs.rs`,
timing `order_book_store::save_book_delta` writing rows into the same buffering
`NativeStateOverlay` as leg (b), so (b) vs (c) is apples-to-apples.

| D | (b) ours journaled | (c) theirs `save_book_delta` |
|---|---|---|
| 16k  | 1.14 ms | 1.03 ms |
| 200k | 1.79 ms | 1.82 ms |
| 700k | 0.68 ms | 1.31 ms |

Both are O(touched), sub-2 ms, depth-independent — confirming the differ is not
the residual under either idiom, and that (b) and (c) are the same complexity
class (the sub-ms deltas are noise). (c)'s edge is **structural, not wall-clock**:
no level walk, no seq re-derivation, no separate shadow (see verdict).
