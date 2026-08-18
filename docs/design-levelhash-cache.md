# L3 — Incremental level-hash sponge cache (`TORUS_LEVEL_HASH_CACHE`)

Branch `perf/l3-levelhash-cache`, base `perf/l3-savebooks` @ 7d729b9. Node-local
and env-gated. **Default: ON at 256 MB** (`DEFAULT_LEVEL_HASH_CACHE_MB`,
`native_executor.rs`) — the budget the in-vivo A/B actually ran;
`TORUS_LEVEL_HASH_CACHE=0` is the explicit opt-out back to exact-today. It
shipped default-OFF originally and was flipped once §5.3's A/B and the
byte-identity matrix had both landed. The consensus commitment is FROZEN
(`docs/design-level-rows-authority.md` §1.3): mode-2 `level_hash =
keccak256(for each order front→back: row_len(u32 LE) ‖ seq(8 BE) ‖ borsh(Order))`.
Nothing here changes those bytes — the cache changes only *how* the identical
digest is computed in the dominant (tail-append) case. Byte-identity is the
contract in BOTH cache states; any doubt in the fast path falls back to the full
rehash (today's code path, verbatim).

Attribution (`docs/l3-savebooks-attribution.md`): at the pegged cap-400 cell
with 195k resting orders, `save_order_books` = 106.7 ms/loaded block, ~99 % of
it `level_row_data` re-keccaking each touched level's ENTIRE FIFO queue —
O(orders-at-level) per touch. At resting-heavy load ~99 % of orders *rest*
(appends at the FIFO tail); matches consume the *front*. The deep, expensive
levels are exactly the append-mostly ones — the cacheable case is the dominant
case.

**Scope of the win — what this cache CANNOT do.** It helps append-heavy,
low-churn levels only. It can never help a MATCH-touched level:
`OrderBook::match_at_level` bumps that level's `level_epoch` unconditionally at
loop entry, on every taker touch, which forces the MISS arm (§1.3) — a plain
one-shot rehash, still O(depth), plus one probe insert. That is by construction,
not a tuning gap: keccak absorbs front→back and a match pops the FRONT, so no
prefix survives. The residual `save_books` time in the in-vivo A/B below (37 ms,
down from 83) is exactly this class. Bounding it needs a commitment change
(chunked/Merkle level hashing — consensus-visible, out of scope here).

## 0. The mechanism

Keccak-256 is a sponge: absorption is sequential over the byte stream and the
padding is applied only at finalization. Therefore for any prefix P and suffix S:

```
finalize(absorb(absorb(init, P), S)) == finalize(absorb(init, P ‖ S))
```

and the post-absorb state after P can be snapshotted (the hasher is a plain
value type — cloneable) and extended later. So for a level whose queue only
ever APPENDED at the tail since the last save, we can keep the un-finalized
hasher state (post-absorb of the first N frames), absorb only the new tail
frames, and finalize a *clone* — producing the byte-identical digest in
O(new frames) instead of O(depth).

## 1. Prefix-unchanged proof

The cached state is only valid if the first N frames of the current queue are
**byte-identical** to the N frames absorbed when the entry was created. Frame
count alone is NOT sufficient: an order's `remaining_qty` can mutate in place
(partial fill, in-place qty-decrease modify), a cancel can remove mid-queue
(count could coincide after later appends), and `modify_order` re-uses the same
order id with a NEW seq after cancel+reinsert.

### 1.1 Exhaustive mutation surface

`OrderBook`'s queues (`bids`/`asks: BTreeMap<FixedPoint, VecDeque<Order>>`) are
private fields of the single module `torus_core::order_book` — no code outside
that file can touch a queue. Every mutation path in the file, classified:

| op (site) | queue effect | class |
|---|---|---|
| `insert_order` (place_order rest, modify reinsert, stop trigger rest) | `push_back` of a NEW frame (fresh seq) | **append** |
| `insert_loaded_order` (boot/rebuild loaders) | `push_back` on a freshly-built book (never journals) | append (fresh book ⇒ cache empty anyway) |
| `cancel_order` | `queue.remove(pos)` — front/mid/tail | **invalidating** |
| `cancel_all` | `queue.remove(pos)` per order | **invalidating** |
| `modify_order` in-place qty decrease | `front/mid/tail` order's `remaining_qty` mutated, seq kept | **invalidating** |
| `modify_order` price/qty-increase | `cancel_order` (invalidating) + `insert_order` (append) | invalidating via cancel |
| `match_at_level` — STP cancel | `pop_front` | **invalidating** |
| `match_at_level` — partial fill | `front_mut().remaining_qty -=` | **invalidating** |
| `match_at_level` — full fill | `pop_front` | **invalidating** |
| level-emptying `BTreeMap::remove` (cancel/match paths) | whole queue gone | always preceded by an invalidating pop/remove |
| `BorshDeserialize for OrderBook` | builds a FRESH book via `insert_order` | fresh book ⇒ empty cache |
| `trigger_stops`, executor paths, STP, liquidations | all funnel through the primitives above | covered |

Non-queue mutators (`set_next_seq`, `set_next_order_id`, `set_last_trade_price`,
`restore_stop_row`, stop bookkeeping) do not touch any frame byte of a resting
order: frame bytes are `len ‖ seq ‖ borsh(Order)`, a pure function of the
`Order` struct at its queue position and its entry in `order_seq`. `order_seq`
entries change only at `insert_order` (new id) and at removals (which are all
classified invalidating) — an in-place modify explicitly keeps the seq.

### 1.2 The epoch invariant

Per-level monotonic **prefix epoch**: `level_epoch: HashMap<(side_tag,
raw_price), u64>` in the book (in-RAM only, never serialized). Every
**invalidating** op above bumps the touched level's epoch (bumps are active
whenever the cache is enabled; entries only exist while it is enabled).
Epoch entries are **never removed** for the lifetime of the book instance —
a level emptied and later re-created at the same price keeps its (bumped)
epoch, so a stale cache entry can never match a reincarnated level.
`match_at_level` bumps once at loop entry: every iteration of its body
mutates the front (pop or in-place decrement), and the bump site is the same
guard that journals the level.

Cache entry = `(epoch, frame_count = N, absorbed_bytes_len, prefix_qty,
hasher_state, last_used)`. On the next save of that level:

- **hit** requires `entry.epoch == level_epoch[key]` AND `N <= queue.len()`.
  Epoch equality proves no invalidating op ran since the state was absorbed;
  therefore the ops that touched this level since then were appends only, which
  by VecDeque semantics leave elements `0..N` untouched (moves are bitwise),
  their seqs unchanged (no removal ⇒ no seq churn; in-place changes are
  invalidating), hence frames `0..N` byte-identical to what was absorbed.
  Absorb frames `N..len` into the stored state, finalize a clone ⇒ digest
  byte-identical to the one-shot hash (sponge property, §0). The cached
  `prefix_qty` is extended by the tail quantities **in the same front→back
  `FixedPoint +=` order** as the one-shot sum, so the aggregate is the same
  arithmetic op sequence, not merely the same mathematical value.
- **anything else** (epoch mismatch, `N > len`, no entry) ⇒ full rehash from
  scratch — computationally identical to today's path — and the entry is
  re-seeded from the fresh absorb.

The conservative direction is structural: *forgetting* to classify an op as
append-only costs performance never correctness (epoch bump ⇒ full rehash);
the only fatal direction would be an unclassified queue mutation that bumps
nothing — impossible outside the enumerated file-local surface above.

### 1.3 Staged seeding (the miss-path refinement)

A naïve miss path re-seeds the sponge on *every* miss: it runs a full **streaming**
`sha3::Keccak256` absorb and stores the un-finalized state. On a level that
invalidates every save (all-churn worst case) that is pure overhead — the entry
is never reused, and the streaming absorb + hasher clone is measurably slower
than the frozen one-shot path, whose `alloy_primitives::keccak256` never builds
a resumable state. The round-1 DEBUG µbench showed this as a +15–30 % worst-case
regression against the parity target.

> **Correction (s450).** Earlier revisions of this section and of the MISS arm
> below asserted that the one-shot path was "asm-backed (keccak-asm)". That was
> FALSE for every build up to that point: the workspace never enabled
> `alloy-primitives/asm-keccak`, so `alloy_primitives::keccak256` compiled down
> to the pure-Rust `sha3` core, and `cargo tree -p torus-node -i keccak-asm`
> printed "nothing to print". The feature is now enabled workspace-wide in the
> root `Cargo.toml`, so the claim is true as of that change — verify it for any
> given build with that same `cargo tree` command rather than trusting this
> paragraph. The staged-seeding argument above never depended on the backend:
> the one-shot path wins the all-churn case because it does not build a
> resumable state, whichever Keccak implementation is compiled in.

The fix stages the sponge investment. A level's cache slot is now one of two
states:

- **`Probe { epoch, frame_count }`** — a cheap record (no sponge), written after
  a miss.
- **`Seeded { epoch, frame_count, hasher, prefix_qty, … }`** — the full
  un-finalized sponge (as before).

The cached path picks exactly one arm per save:

- **MISS** (no slot, a `Probe`/`Seeded` whose epoch moved, or `N > len`): run the
  frozen one-shot path VERBATIM (`level_row_data`) — same keccak backend, same
  bytes, same cost as cache-off — and write a `Probe(epoch, N)`. No streaming absorb, no
  state clone. A level that invalidates every save therefore pays only the plain
  one-shot cost plus one `HashMap` insert.
- **PROMOTE** (`Probe` present, epoch unchanged): the level survived one full
  append-only interval (epoch equality between the probe's save and this one
  proves no invalidating op ran between them, §1.2). Only now is the sponge
  investment made: one full front→back absorb that both produces this save's
  digest and seeds the `Seeded` sponge. It replaces the probe in place (no
  entry-count growth, no eviction).
- **HIT** (`Seeded`, epoch unchanged, `N ≥ frame_count`): the O(new-tail)
  clone-extend, unchanged from §1.1.

So the sponge is built only for a level that has demonstrated append-only
behaviour across a whole save interval — precisely the levels that will yield
hits — and a churning level is demoted back to a `Probe` (via the MISS arm) the
moment it invalidates, never paying to rebuild a state it won't reuse. Output is
byte-identical in every arm (the PROMOTE absorb and the MISS one-shot hash the
identical framed byte stream, §0/§3). The change is entirely inside the cache
module (`level_row_data_cached` + the `LevelHashCacheEntry` enum); no commitment
byte, epoch-bump site, or engagement-path code outside the cache moved.
`level_hash_cache_stats` now reports `(hits, misses, seeds, entries)` — `seeds`
counting the PROMOTE investments.

## 2. Cache lifecycle

- **Storage**: `Option<Box<LevelHashCache>>` inside `OrderBook` (like the 3c
  journals: in-RAM bookkeeping, never serialized, never part of any hash or
  row). It therefore RIDES the rank8 resident-books holder automatically —
  `stash_resident`/adopt moves the book struct, cache and epoch map included,
  so mutation tracking is continuous across blocks.
- **Every rebuild path is trivially safe**: restart, resident staleness-guard
  trip, mode-2 boot verify, per-block RESIDENT=0 rebuild — all construct fresh
  `OrderBook`s (loaders/deserializer), which start with an EMPTY cache and
  empty epoch map. Cold start ⇒ first save is a full rehash that re-seeds the
  cache. Correctness never depends on cache contents; a dropped cache is only
  a cold start.
- **Enablement**: `TORUS_LEVEL_HASH_CACHE` (MB, parsed once per process;
  unset or unparseable = the 256 MB default, explicit `0` = OFF). The executor enables it ONLY in the
  `BookMode::LevelAuthority` (mode 2) save arm — one
  `ensure_level_hash_cache(per_book_budget)` per dirty book per save (no-op
  when already enabled); budget 0 actively drops any cache. Modes 0/1 never
  reach the arm (they `discard_level_ops`) and never enable the cache —
  untouched. Entries are created only at save time from the live queue, so a
  mid-run enable is safe: an entry is always a truthful snapshot at creation,
  and bumps are active from the moment the cache exists.
- **Bounding**: global budget split evenly across live books
  (`level_hash_cache_bytes / order_books.len()`, `native_executor.rs`), so a
  node with many markets gets a thinner slice per book — market count shrinks
  the per-book budget, it never grows the total. Entry cost
  modeled at 512 B (keccak state ~200 B + block buffer ~136 B + metadata +
  map overhead), so the 256 MB default models ~524k cached levels across the
  whole node, allocated only for books dirtied in a mode-2 save.
  LRU-approximate eviction: when a book's cache is full, the
  least-recently-used quarter is evicted in one batch (rare, amortized
  cheap). Eviction is always safe — it is a cold start for that level.
- **Deletes**: when a save observes an emptied level (delete op), its cache
  entry is dropped.
- **`full_level_ops`** (genesis/offline full write) clears the book's cache
  entries defensively and uses the plain path.

## 3. Why the digest cannot differ

- **Implementation**: the cached path uses `sha3::Keccak256` (RustCrypto,
  v0.10 — already in the workspace tree as a direct dependency of
  `alloy-primitives 1.5`). Its hasher is a plain value type implementing
  `Clone`, and the `digest` streaming contract guarantees
  `update(a); update(b); finalize()` ≡ `update(a ‖ b); finalize()` — the
  Keccak padding (`0x01…80` domain byte for legacy Keccak-256) is applied
  exclusively in `finalize`. Snapshot-and-extend is therefore exact, not
  approximate: the stored state after absorbing P, later extended with S and
  finalized, is byte-for-byte the state machine that a one-shot absorb of
  P ‖ S would reach. No internal length caching or midstream padding exists
  in the sponge construction that could distinguish the two.
- **Cross-implementation**: the frozen one-shot path (`level_row_data`) uses
  `alloy_primitives::keccak256`, which since the workspace enabled
  `alloy-primitives/asm-keccak` is the cryptogams assembly backend
  (`keccak-asm` → `sha3-asm`) rather than the pure-Rust `sha3` core the cached
  path streams. Both compute FIPS-202-draft Keccak-256 with the 0x01 domain
  padding — a single mathematical function, two implementations. This is
  enforced, not assumed, at two levels: `keccak_stream_clone_equivalence`
  (`torus-core/src/order_book.rs`) hashes random inputs at random split points
  through the streaming `sha3::Keccak256` and compares against
  `alloy_primitives::keccak256`, and `keccak_backend_vectors.rs`
  (`torus-core/tests/`) pins the compiled-in backend against fixed digests
  across the 136-byte rate boundary. The cached path's frame encoding is the
  SAME helper (`encode_order_row_parts` + u32-LE length framing) the one-shot
  path uses, so the absorbed byte stream is identical by construction.
- **Fallback identity**: with `TORUS_LEVEL_HASH_CACHE=0`, `take_level_ops` runs
  the pre-existing `level_row_data` code verbatim — not a re-implementation.
  Cache-on differs only in which correct algorithm computes the same digest.

If either property had failed (non-cloneable hasher, finalize-time state
mutation observable across clones, or a padding scheme applied during absorb),
the instruction was to STOP; both properties hold for `sha3 = 0.10`
(`CoreWrapper<Keccak256Core>` derives `Clone`; padding lives in
`finalize_core`).

## 4. What is NOT attempted

- No change to any committed byte: level rows, order rows, meta, stops, roots.
- No caching across removals (no "un-absorb", no chunking, no Merkle) — those
  are the consensus-visible options 1/2 of the attribution doc, out of scope.
- No persistence of cache or epochs; no cross-restart warm state.
- No engagement in modes 0/1, no touch of consensus/hotstuff crates.

## 5. Verification (acceptance)

1. **Core oracle** (`level_rows_core_tests.rs`): the existing scratch-recompute
   oracle (`drain_and_check`) re-run with the cache enabled over the
   adversarial op script; plus a seeded randomized differential (cache-on book
   vs cache-off book fed identical ops, `take_level_ops` outputs compared
   op-for-op, multiple seeds, invalidation-churn profile, tiny-budget eviction
   profile); plus the keccak split-point equivalence test.
2. **Full-stack byte-identity matrix** (`level_rows_tests.rs`): cache-on vs
   cache-off universes over randomized 40-block sequences (places, crossing
   matches, front/mid/tail cancels, both modify kinds, cancel-alls, stops),
   compared at EVERY block: state root, level-row/book CF bytes; with a
   mid-sequence restart (fresh holder ⇒ cold cache) and ≥3 seeds plus an
   adversarial churn seed; final full CF dumps + `native_root_full` oracle.
3. **µbench** (`l3_savebooks_ubench.rs`): depth-5 append-heavy shape cache-on
   vs off, and an all-invalidated worst case (in-place front modify at every
   level each block). **RELEASE A/B** (`savebooks_levelcache_ab`, `--release
   --features save-timings`, 40 levels × 10 markets; `lvls_us` = level-hash
   sub-step):

   ```
   append-heavy    off_lvls  on_lvls        worst-case      off_lvls  on_lvls
   blk 1 (seed→promote) 2496     1980        blk 1               2651     2263
   blk 2 (hit)          2364     1096        blk 2               2139     2236
   blk 3 (hit)          5405     1089        blk 3               2245     2179
   blk 4 (hit)         11248     1252        blk 4               2209     2671
   blk 5 (hit)          4169     1994        blk 5               1998     2118
   ```

   Append-heavy: cache-off `lvls_us` climbs with queue depth (each block
   re-hashes every level's whole growing FIFO) while cache-on stays flat at
   ~1–2 µs/level after the one-block promote — up to ~9× at block 4 (11.2 ms
   → 1.25 ms) and growing with depth. Worst case (staged seeding): cache-on
   tracks cache-off within noise on both `lvls_us` and end-to-end `SAVE_us`
   — the round-1 +15–30 % all-invalidate regression is gone, because a
   churning level now takes the plain one-shot path plus a cheap probe rather
   than re-seeding a sponge it never reuses (§1.3).
4. Existing suites: torus-core book/rows/level tests, 12-combo byte-identity
   matrix, dirty-bound witness, full workspace `--no-fail-fast`.

## 6. SaveTimings disposition

Per the attribution doc's recommendation, the temporary `SaveTimings` hot-path
instrumentation is now compiled OUT by default: the fields and collection are
gated behind the new `torus-bridge` cargo feature `save-timings`, and the
µbench target declares `required-features = ["save-timings"]` (run with
`cargo test -p torus-bridge --test l3_savebooks_ubench --features save-timings
-- --ignored --nocapture`). Production builds carry zero instrumentation.

## 7. Mode 3 — chunked level digest (`TORUS_BOOK_ROWS=3`, consensus-visible)

The sponge cache (§0–§3) is node-local and byte-identical, but its win is
bounded to append-only intervals: `match_at_level` bumps the epoch on every
taker touch, so a level that is *filled from the front* every block pays the
full O(depth) one-shot rehash on every save. With band-5 flow the top levels
hold thousands of resting orders and the fills consume exactly that front, so
`save_books` grows with resting depth over a run (the early→late 60 s spread).

Mode 3 keeps mode 2's row layout, loader, saver and boot verify **byte for
byte** and changes only the `level_hash` preimage (hence the root ⇒ its own
marker byte `3`, fresh genesis, fleet-uniform — same rules as any mode flip):

```text
chunk_idx(seq)  = seq / LEVEL_CHUNK_SEQS            (64; book_rows.rs)
chunk_digest(i) = keccak256(frames of the level's orders with seq in
                            [64·i, 64·(i+1)), queue order)   -- same frames as mode 2
level_hash      = keccak256(LEVEL_HASH_CHUNKED_DOMAIN ("TORUSLV2")
                            ‖ order_count(u32 BE) ‖ total_qty_raw(i128 BE)
                            ‖ for each NON-EMPTY chunk, idx ascending:
                                  chunk_idx(u64 BE) ‖ chunk_digest)
```

Why seq buckets: the per-book `seq` is monotone, unique, assigned at insert
and persisted in every row, and the queue is seq-ascending, so (a) a chunk's
members are one contiguous queue range (binary search on seq), (b) the digest
is a **pure function of level content** — no dependence on which mutation
happened, so the incremental drain and a from-scratch recompute (boot verify,
`OrderBook::level_row_data_chunked`) agree by construction, and (c) every
mutation dirties only the chunk(s) holding the touched seq: front pops
(fills) touch 1–2 chunks, appends 1–2, a mid cancel 1. Per dirty level the
save costs O(dirty chunks × 64 frames) + O(n_chunks × 40 B) instead of
O(depth) — depth-independent.

Maintenance (`order_book.rs`, block "Chunked level digest"): per level a
`BTreeMap<chunk_idx, (digest, Σqty, count)>`, built in full on the level's
first drain and refreshed per dirty chunk afterwards; `mark_chunk_dirty` is
called at EVERY queue-mutation site enumerated in §1.1 (`insert_order`,
`cancel_order`, `cancel_all`, in-place `modify_order`, all three
`match_at_level` arms, `insert_loaded_order`) — the same surface that journals
the level, gated on the book's `level_hash_chunked` flag so modes 0/1/2 pay
nothing. `total_qty`/`count` are Σ over chunks in ascending (== queue) order,
i.e. the same checked `FixedPoint +=` sequence as the flat sum.

Fork protection: marker byte 3 (`__book_mode__`) AND boot verify 3 — a mode-2
node on a mode-3 DB (or vice versa) recomputes different level rows and
fail-stops before serving or signing (message names `TORUS_BOOK_ROWS`).
Readers (`book_reader::BookLayout::from_marker_byte(3)`) treat the layout as
level-authority — they never recompute the digest.

Verification: hand-spelled oracle + pinned vectors (`PINNED_CHUNKED_LEVELS`,
depths 1/2/100/1000 — 3 and 17 chunks); scripted incremental-vs-scratch drain
(`chunked_incremental_drain_matches_scratch`); randomized differential against
a flat twin (`level_ops_chunked_vs_scratch_randomized_differential`, 5 seeds ×
900 ops, drained every few ops, coverage-checked — a missing dirty mark at the
partial-fill site was mutation-tested to fail it); `full_level_ops` re-seed;
executor-level `semantic_differential_mode_3_vs_2` (identical semantics, store,
meta/stop rows, qty‖count prefixes; different hashes and root), the 4×4
fail-stop matrix, and `mode3_incremental_saves_survive_fresh_reload_boot_verify`
(one resident ctx over 12 blocks of deep-level fills/cancels/modifies/
cancel-alls, then a fresh reload whose boot verify recomputes from scratch).
