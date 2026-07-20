# 3c — Level-rows-as-authority + hash-only native mirror (design)

Status: **IMPLEMENTED** on `perf/level-rows` (3b `perf/root-cost` @ 5a443ab
merged first per decision 6). Implementation notes / deviations from this
design:
- `diff_stop_rows` / `write_meta_if_moved` are STATELESS (persisted-state
  compare: one meta point read + one bounded stop-prefix scan per dirty
  market) instead of shadow-backed — `BookRowShadow` is gone entirely, so
  resident holders carry only books (journals ride inside the book).
- `take_level_ops` returns full `LevelRowData` (qty ‖ count ‖ hash);
  modes 0/1 use `discard_level_ops`/`discard_row_ops` so no keccak or
  encoding is spent outside mode 2.
- Row/level key codecs live in `torus_core::book_rows` (codec-only module,
  no StateBackend dep); order-row/meta VALUE codecs stay next to their
  owners (book / executor).
- `__book_mode__` marker is written on the first `save_order_books` call
  (even with zero dirty books) — strongest empty-chain wrong-flag guard.
Round 3c of the throughput mission
(Layer 2, exec envelope). Branch `perf/level-rows`, cut from
`perf/exec-scaleup` @ `81ea957` (the DESIGN BASE — all file:line anchors
below refer to that commit unless a branch is named).

One **batched preimage change**: (A) per-price-level aggregate rows become
the root authority for order-book state, (B) the native mirror stores
`keccak(value)` instead of `value` (hash-only mirror, design inherited from
`origin/perf/deep-book-storage` @ `42bca69`,
`docs/plans/hash-only-native-mirror.md`). Both alter native-root preimages,
so they ship together in ONE consensus-visible round with ONE fresh genesis.
A third, non-preimage piece rides along: (C) save-path standardization on
the **journal-in-book** idiom and deletion of the executor-side shadow
differ (verdict: `docs/ab-differ-findings.md` on `perf/ab-differ`).

## 0. Problem statement (why levels, why now)

Proven root cause of the flush wall (`ab-differ` round 3a): under
`TORUS_BOOK_ROWS=1` every touched **order** is a native-root CF entry.
`bucket_id = keccak(cf_tag ‖ key)[0..2]` sprays those entries uniformly
over 65,536 buckets, so a 16k-touched-order block dirties ~14–18k buckets.
Each dirty bucket pays a mirror prefix-scan (O(occupancy), NOT removed by
`TORUS_NATIVE_ROOT_CACHE`), a whole-bucket keccak, and 2× value bytes
(CF + mirror). Residual root + state_write + flush ≈ 2–2.5 s/block at
700k–1.1M resting depth. Gate needs ~42 ms/block.

Strategy: make the number of dirty **root** entries per block scale with
touched *price levels* (10s–100s), not touched *orders* (~16k). Order
identity stays consensus-committed — via a per-level hash inside the level
row — while full order data moves to a node-local CF outside the root.

The parallel session's `feat/precompile-0800-topn-gas` @ `39c74a1` built
level rows as **additive read-side** data (order rows stay authoritative;
level rows are extra root entries — strictly *more* dirty buckets). This
design inverts that: levels become the authority, order rows leave the
root. We mine their encoding (`price_enc`, level journal,
`iterate_cf_bounded`) but do NOT merge their branches (their `92fa3ac`
rewrites the Phase-4 region from a pre-A5 fork — silent A5 revert; their
`0x02` row tag collides with our frozen `0x02` = stops).

## 1. Preimage / schema spec

### 1.1 Mode value

`TORUS_BOOK_ROWS` becomes a three-way mode (single knob, one detection
function, modes mutually exclusive per chain):

| value | mode | book authority in `cf_native_order_books` |
|---|---|---|
| unset / other (DEFAULT) | `Classic` | one borsh whole-book blob per market (8-byte key) — **exact-today** |
| `1` | `OrderRows` (C4) | meta row + per-order rows + stop rows — today's C4, save path rewritten (§2), row bytes unchanged in layout |
| `2` (NEW) | `LevelAuthority` (3c) | meta row + stop rows + **level rows**; NO order rows in the root CF |

Consensus-visible, fleet-uniform, fail-stop on mismatch (§4). Changing the
mode on an existing chain requires a fresh genesis — no migration path, by
policy (same as C4 today, `native_executor.rs:388-399`).

### 1.2 `cf_native_order_books` rows under mode 2 (root CF, tag 1 in `NATIVE_ROOT_CFS`)

All integers big-endian. Key layouts are length-disjoint (9 / 25 / 26) and
tag-disjoint, so misreads are impossible:

```
meta  key market_id(8) ‖ 0x00                       (9 B)   [unchanged C4 layout]
      val next_seq(8) ‖ tick_raw(16) ‖ lot_raw(16) ‖ next_id(16) ‖ ltp_tag(1) [‖ ltp_raw(16)]
stop  key market_id(8) ‖ 0x02 ‖ stop_id(16)         (25 B)  [unchanged C4 layout]
      val StopOrder borsh
level key market_id(8) ‖ 0x03 ‖ side(1) ‖ price_enc(16)   (26 B)  [NEW]
      val total_qty_raw(i128 BE, 16) ‖ order_count(u32 BE, 4) ‖ level_hash(32)   (52 B)
```

- **Tag byte `0x03` for level rows** — this resolves the collision with the
  parallel branch explicitly: our `0x02` (`BOOK_ROW_STOP`,
  `native_executor.rs:442`) is frozen; their `ROW_TAG_LEVEL = 0x02`
  (`order_book_store.rs:50` @ `39c74a1`) is NOT adopted. Modes are
  additionally exclusive by genesis, but a fresh tag byte keeps the row
  namespace unambiguous forever and lets loaders fail loudly on foreign
  artifacts instead of misparsing them.
- **No `0x01` order rows in mode 2.** A `0x01` key found under mode 2 is a
  fail-stop (mode-mismatch corruption).
- `side`: `0x00` bid, `0x01` ask (their `side_tag`, adopted).
- `price_enc` (adopted from `39c74a1` verbatim): sign-flipped BE i128
  (`(raw as u128) ^ (1<<127)`), bitwise-NOT for bids — forward
  lexicographic iteration is best-first on both sides. Enables O(N)
  top-N reads later (production-debt item, §9).
- **`level_hash` — the order-identity commitment** (§1.3).
- Invariants: a level row exists iff its queue is non-empty;
  `total_qty > 0`; `order_count >= 1`; `total_qty` = sum of member
  `remaining_qty`; deleting the last order deletes the row.

Mode 1 keeps today's exact row layouts (`meta 0x00 / order 0x01 / stop
0x02`, `native_executor.rs:401-408`). Mode 0 keeps the whole-book blob.

### 1.3 Order-identity commitment: per-level hash

**Decision: commit order identity per price level, embedded in the level
row value** — not a single per-market rolling commitment.

```
level_hash = keccak256( for each order in queue, front → back:
                          row_len(u32 LE) ‖ seq(8 BE) ‖ borsh(Order) )
```

`seq(8 BE) ‖ borsh(Order)` is byte-identical to today's order-row VALUE
(`book_order_value`, `native_executor.rs:523-528`) — one frozen codec, one
framing helper. `row_len` frames each order so concatenation is
injective. Queue order (price-time priority) is consensus state; `seq` is
included so the node-local row store (§1.4) is fully verifiable — a wrong
seq breaks FIFO rebuild and is caught by the hash.

Why per-level, not the alternatives considered:

- **Per-market rolling multiset hash (XOR/add of per-order keccaks) —
  REJECTED.** O(1) per touched order and O(1) bytes, but XOR/additive set
  commitments admit Wagner k-sum subset collisions, and an attacker can
  freely sample order-row hashes by grinding trader-chosen fields
  (`client_order_id`, qty, price). The commitment guards state-sync and
  light-client proofs — collision resistance is load-bearing. Not worth a
  novel cryptographic assumption to save hashing we can afford.
- **Per-market whole-book hash — REJECTED.** O(book) keccak per touched
  market per block; recreates the classic-blob wall.
- **Per-level keccak — CHOSEN.** Collision-resistant (plain keccak over an
  injective encoding), recomputed only for touched levels, and the rehash
  input is exactly the data the save path already walks. Cost is
  O(orders in touched levels) RAM-side keccak: 16k touched orders ×
  ~140 B ≈ 2.2 MB ≈ 2–3 ms/block. Adversarial mega-level risk in §9.

So "the per-market commitment" = the market's set of level rows + stop
rows + meta row, all individually under the native root. There is no
additional per-market row.

**Verification story (light clients / replay / state-sync):**

- *Full nodes / replay*: execute actions, maintain books, recompute level
  rows; root is a pure function of consensus history — the A2 oracle
  property is preserved (§3.3).
- *Proof of a resting order*: Merkle path root → bucket leaf (leaf
  preimage = framed `(cf_tag, key, value_hash)` members under hash-only
  mirror, §3) → level row value → level preimage (the framed order list,
  which hashes to `level_hash`). Proof size O(bucket occupancy + level
  depth).
- *State-sync*: a fresh node fetches order rows from an untrusted peer,
  rebuilds books, recomputes every level row + hash, and compares against
  the root-committed CF. Any forged/omitted/reordered order fails the
  comparison. (Snapshot-serving protocol itself is follow-on work, §9.)

### 1.4 Node-local order-row store: `cf_book_order_rows` (NEW CF)

Full order data must survive restart but must NOT touch the root. New
column family `cf_book_order_rows`:

- declared in `crates/torus-state/src/cf.rs` (native block, near `:44`) and
  appended to `ALL_CF_NAMES` (`cf.rs:147`);
- **NOT** in `NATIVE_ROOT_CFS` (`native_trie.rs:38-45`) — never bucketed,
  never mirrored, never hashed;
- key/value schema = today's C4 order row, byte-identical:
  `market_id(8) ‖ 0x01 ‖ order_id(16)` → `seq(8 BE) ‖ borsh(Order)`;
- written through the same `NativeStateOverlay` → same atomic
  `WriteBatch` as the root CFs, trie, mirror and applied-height marker
  (`backend.rs:601`, flush call `app.rs:1136-1199`) — crash cannot split
  root state from the order store;
- content is a deterministic materialization of consensus state (every
  validator holds identical bytes), but it is **node-local by
  classification**: no reader may feed it into a preimage. The boot
  verifier (§5) enforces its integrity against the level hashes.

Determinism rule (restated as an invariant for review): *level rows, level
hashes and meta rows must be derived exclusively from `OrderBook` state
(consensus). Resident books, shadows, caches, and `cf_book_order_rows`
never contribute bytes to any preimage except by way of the verified
book rebuild path.*

## 2. Write-path spec — journal-in-book, shadow differ deleted

### 2.1 What is adopted (port, do not merge)

From `origin/perf/deep-book-storage` / `39c74a1` (additive re-home into
our `OrderBook`, `crates/torus-core/src/order_book.rs`):

- `order_seq: HashMap<OrderId, u64>` + book-owned `next_seq` — **seq
  assigned at insert time** by the book, persisted via the meta row's
  `next_seq` field. The book is authoritative for seq.
- `row_journal: BTreeSet<OrderId>` (touched) + `row_exists:
  HashSet<OrderId>` (phantom-delete guard) + `take_row_ops()` /
  `full_row_ops()` draining — exact O(touched) row ops, no level walk, no
  seq re-derivation.
- `level_journal: BTreeSet<(u8 /*side_tag*/, i128 /*raw_price*/)>` +
  `level_exists` + `take_level_ops()` / `full_level_ops()` — every
  mutation site that touches a level journals it (place, cancel, modify,
  match loop; the `39c74a1` order_book.rs hunks enumerate all sites).
- `price_enc` / key builders / `side_tag` (re-homed, level tag **0x03**).
- `StateBackend::iterate_cf_bounded` (`39c74a1` backend.rs) — read-side
  only, optional in this round (needed when RPC/precompiles become
  level-aware; §9).

Our rank8 `BookJournal` (`order_book.rs:112-135`, drained at `:1048`) is
**replaced** by the ported journals — one journal idiom, not two. The
rank8 resident-book holder machinery (`ResidentBooks`,
`native_executor.rs:658-681`, staleness guard `:629-638`) is kept
unchanged except that `ResidentInner.shadows` disappears.

### 2.2 Seq-timing change (consensus-visible, batched here)

Today mode-1 seqs are assigned at SAVE time by walking the book in
canonical order (`native_executor.rs:409-418`, `diff_queue_rows:1469`).
Journal-in-book assigns them at INSERT time. Both are deterministic pure
functions of consensus history, but they emit different seq values for the
same block (e.g. ask placed before bid: insert-time seqs follow arrival;
save-time seqs follow bids-first canonical walk) ⇒ different row bytes ⇒
different roots. This is acceptable **only because this round already
changes every mode's root** (hash-only mirror) and requires a fresh
genesis. It is part of the batched preimage change and must never be
back-ported alone.

### 2.3 Executor delete list (`crates/torus-bridge/src/native_executor.rs`)

Deleted outright:

- `OrderShadow` (`:550-564`), `BookRowShadow` (`:566-580`)
- `save_book_rows` (`:1360-1390`), `save_book_rows_journaled`
  (`:1411-1462`), `diff_queue_rows` (`:1469-1523`),
  `delete_order_rows` (`:1527-1545`)
- the shadow map + mark-and-sweep `gen` plumbing, `book_shadows` field,
  and the shadow-rebuild half of `load_order_books_rows` (`:1096-1244`
  is rewritten, not kept)

Kept: row key/value codecs (`:444-539`, re-homed to torus-core next to the
book so `take_row_ops` can encode rows), `diff_stop_rows` (`:1549-1582`),
`write_meta_if_moved` (`:1585-1604`), the C4 fail-stop philosophy
(`:1044-1087`), `NEXT_GLOBAL_ORDER_ID_KEY` (`:1246-1261`, unchanged —
already node-local in `CF_NATIVE_MARKETS`).

### 2.4 Save flow per dirty market (`save_order_books`, `:1277`)

```
match mode {
  Classic      => whole-book blob put (unchanged, :1302-1319)
  OrderRows(1) => header/meta if moved
                  for (id, op) in book.take_row_ops():
                      put/delete cf_native_order_books order row      // root CF
                  book.take_level_ops() is drained and DISCARDED (mode 1 has no level rows)
  LevelAuthority(2) =>
                  for (id, op) in book.take_row_ops():
                      put/delete cf_book_order_rows order row         // node-local CF
                  for (side_tag, raw_price) in book.take_level_ops():
                      match book.level_queue(side, price)             // order_book.rs:1065
                        Some(queue) => put level row:
                            value = total_qty ‖ count ‖ keccak(framed rows front→back)
                        None        => delete level row
                  diff_stop_rows(...)                                  // root CF, unchanged
                  write_meta_if_moved(...)                             // root CF, unchanged
}
```

O(touched orders) node-local writes + O(touched levels) root writes +
O(orders in touched levels) RAM keccak. No book walk, no shadow, no seq
re-derivation. `save_book_full` (genesis seeding / tests / offline
rebuild) = `full_row_ops` + `full_level_ops` + reconcile-delete, mirroring
`39c74a1 order_book_store.rs:394-446`.

## 3. Root-authority integration with `native_trie.rs` (incl. hash-only mirror)

### 3.1 What mode 2 changes for the trie: nothing structural

The trie consumes the overlay dirty set (`backend.rs:360`,
`dirty_native_keys:544`) — it never knows about books. Mode 2 simply makes
the dirty set small: level/meta/stop keys instead of 16k order keys.
`cf_book_order_rows` writes bypass `native_dirty()` entirely (not a root
CF ⇒ `cf_tag()` returns `None`, `native_trie.rs:48-53`; the overlay flush
writes them in the same batch without trie ops — verify the overlay's
native-dirty filter excludes the new CF, `backend.rs:357-374`).

### 3.2 Hash-only mirror (fold-in of `42bca69`, assumptions re-verified)

Change `CF_NATIVE_HASHED` values from `value` to `keccak256(value)` and
define bucket leaves over per-entry hashes:

```
leaf = keccak( for each (cf_tag, key, value_hash) in bucket, canonical order:
                 cf_tag(1) ‖ len(key) u32 LE ‖ key ‖ 32 u32 LE ‖ value_hash )
```

i.e. `frame_entry` (`native_trie.rs:99-105`) unchanged, `value_hash` in
the value position. `bucket_id`, tree shape, node keys, defaults, root
finalization: unchanged.

Verification of the `42bca69` doc against CURRENT `native_trie.rs`
(root cache landed after the doc was written):

| doc assumption | status on `81ea957` |
|---|---|
| mirror written in `build_native_trie_to_cf` and dirty path | holds — `:253` and `:533/:544` (uncached), `:724/:735` (cached); all three sites store `keccak(value)` instead |
| leaf recompute rereads full values from mirror | holds — `read_bucket_members` (`:479`) returns values; post-change it returns 32 B hashes (member sets shrink ~4× for book rows, more for blobs) |
| `native_root_full` must hash identically | holds — `leaves_from_db:192` frames raw values at `:202`; add per-value keccak there |
| "reuse frame_entry unchanged" | holds — leaf assembly sites `:553-556` (uncached) and `:748-756` (cached) |
| NEW since doc: cached clean-write elision | `:719-723` compares mirror value == new value. Under hash-only it compares stored hash == `keccak(new value)` — elision semantics identical (keccak collision ⇒ broken world anyway). Requires hashing each dirty value before compare: ≤ ~2 MB/block keccak in mode 1, trivial in mode 2 |
| NEW since doc: 3b (`perf/root-cost` @ `5a443ab`) | NOT on the design base. 3b's `4bf3e80` unified `compute_native_dirty_ops[_cached]` + `apply_native_dirty_to_batch[_cached]` into one `apply_native_dirty()` and added `NativeMemberCache` (`Members = BTreeMap<(u8,Vec<u8>),Vec<u8>>`). Hash-only integrates cleanly with either base: on 3b, `Members` values become 32 B hashes — the member cache holds ~4× more buckets per MB and the parallel leaf hash hashes fixed-size frames. Whichever of 3b / 3c merges second rebases this section; the preimage definition above is base-independent |

LOC estimate: ~50–100 on `81ea957` (per the doc), ~+50 on top of 3b.
Migration: none live — `build_native_trie_to_cf` (`:235`) regenerates the
mirror from the CFs; this round is fresh-genesis anyway.

### 3.3 Oracle discipline

`native_root_full` stays the O(total) determinism oracle. Both it and the
incremental path pick up value-hashing in exactly one place each, both via
`build_tree` (`:146`) — they cannot diverge structurally. All A2.1/A2.2
gates (`native_trie.rs` tests `:947-1323`) rerun green under the new
framing with zero structural test edits (the framing change is inside the
helpers they exercise).

## 4. Flag / genesis semantics + mode interaction matrix

### 4.1 Semantics

- `TORUS_BOOK_ROWS`: unset/other = Classic (**exact-today default**),
  `"1"` = OrderRows, `"2"` = LevelAuthority. Parsed once per process
  (extend `parse_book_rows_toggle`, `native_executor.rs:436-438`, to a
  mode enum with the same trim/strictness rules).
- Consensus-visible, fleet-uniform. A mixed fleet forks at the first
  block that touches a book; the on-disk fail-stops below catch restarts
  with the wrong flag before that.
- Fresh genesis required to select mode 2 (and to flip any mode). One
  fresh genesis for the whole 3c round at integration.
- Env-var-as-consensus-config follows the C4 precedent; folding the mode
  into the genesis file proper is noted production debt (§9).

**Fail-stop detection at load** (extends `load_order_books*`,
`:1049-1244`):

- mode 0 loader: any 9/25/26-byte tagged key ⇒ fatal (today's check
  `:1056-1070` extended to 26-byte keys).
- mode 1 loader: 8-byte key ⇒ fatal (today, `:1125-1131`); 26-byte
  `0x03` key ⇒ fatal (NEW: "DB written under mode 2").
- mode 2 loader: 8-byte key ⇒ fatal; 25-byte `0x01` key in the ROOT CF ⇒
  fatal ("DB written under mode 1"); unknown tag/length ⇒ fatal.
  `cf_book_order_rows` non-empty while root CF has no level/meta rows ⇒
  fatal (split-brain store).
- NEW robustness marker: `__book_mode__` row in `CF_NATIVE_MARKETS`
  (node-local, non-root — same classification as
  `NEXT_GLOBAL_ORDER_ID_KEY:1250`), written on first save, checked at
  boot. Catches wrong-flag restarts on chains whose books are still
  empty (content sniffing has nothing to sniff there).

### 4.2 Mode interaction matrix

BOOK_ROWS (consensus) × RESIDENT_BOOKS (node-local) × NATIVE_ROOT_CACHE
(node-local); 3b's MEMBER_CACHE / PARALLEL_BUCKET_HASH (node-local) are
orthogonal to all cells and omitted for brevity:

| BOOK_ROWS | RESIDENT=0 | RESIDENT=1 |
|---|---|---|
| unset (Classic) | today's default. Root: whole blobs | supported (rank8), value-neutral |
| 1 (OrderRows) | per-block row reload (O(depth)/block — legal, slow) | C4+rank8: O(touched) via `take_row_ops` |
| 2 (LevelAuthority) | per-block rebuild from `cf_book_order_rows` + boot-style verify skipped intra-run (O(depth)/block — legal, **loud warn**, exists because it IS the staleness-guard fallback path) | **target config**: O(touched) rows + O(touched levels) root entries |

ROOT_CACHE ∈ {0,1} is valid in every cell (value-neutral by its own
differential gates). Mode 2 gets the most from it: with ~10²–10³ dirty
buckets/block the cached sibling reads and elision dominate less, but the
member-scan term also shrinks because root-entry count (hence bucket
occupancy) collapses. RESIDENT=1 + mode 2 is the benched configuration;
RESIDENT=0 under mode 2 is functional-but-discouraged (it is exactly the
recovery path, §5, run every block).

Fail-stop interactions: none new across node-local flags (they remain
mixable per node). The only fail-stops are mode-vs-DB (above) and the
existing rank8/rank-root staleness guards.

## 5. Restart / recovery + differential test plan

### 5.1 Restart (same mode, clean or crashed)

1. Atomicity: root CFs + `cf_book_order_rows` + mirror + trie +
   `META_NATIVE_APPLIED_HEIGHT` commit in ONE batch
   (`backend.rs:601-697`) — a crash never splits them.
2. Boot rebuild (mode 2): scan `cf_book_order_rows` → group per market →
   rebuild books in canonical order (bids asc (price, seq), then asks,
   stops by id — same walk as `load_order_books_rows:1181-1231`) →
   populate `order_seq`/`row_exists`/`level_exists` from what was read.
3. **Boot verification (mandatory):** recompute every market's meta row,
   stop-row set and full level-row set (qty ‖ count ‖ hash) from the
   rebuilt books; byte-compare against `cf_native_order_books`. Any
   mismatch ⇒ `ctx.fatal_error` (node-local store corrupt/stale — do NOT
   serve or sign). O(depth) once per boot (~1M orders ≈ ~140 MB keccak ≈
   low seconds; parallelizable per market later).
4. Resident holder population + rank8 staleness guard unchanged
   (`holder.height + 1 == H` && marker check, `:629-638`). A mid-run
   guard trip rebuilds from `cf_book_order_rows` WITH the level-row
   byte-compare for the touched markets (cheap insurance, same code as
   boot).
5. Fresh node with empty disk: genesis replay or state-sync
   (order-row snapshot + level-hash verification — protocol is follow-on
   work, §9). There is no way to conjure books from level rows alone;
   that is by design (level rows commit, order rows materialize).

### 5.2 Test plan (RED → GREEN order)

1. **RED — codec/property unit tests** (torus-core): `price_enc` total
   order (their `book_levels_oracle_tests.rs` @ `39c74a1` as seed);
   level-row key order property; `level_hash` oracle (recompute from a
   scratch walk == incremental journal-driven value after arbitrary op
   sequences); framed-encoding injectivity (`row_len` framing).
2. **RED — save/load roundtrip** (torus-bridge, new
   `level_rows_tests.rs`): random op stream → save deltas each "block" →
   reopen → rebuilt book == in-RAM book (orders, queues, seqs, stops,
   ltp); `save_book_full` == accumulated deltas, byte-compare both CFs.
3. **Fail-stop matrix**: each mode's loader against each other mode's DB
   artifacts (9 cells, 6 fatal) + `__book_mode__` marker mismatch + the
   split-brain check + corrupt-row cases (bad tag, truncated value,
   qty/count/hash mismatch vs rebuilt book — flip one byte in one
   node-local order row ⇒ boot verify MUST fail).
4. **Semantic differential across modes** (the consensus-safety
   argument): same action stream through modes 0, 1, 2 ⇒ identical fills,
   cancels, events, balances, positions (roots DIFFER by design — assert
   book-semantics equality, not root equality).
5. **N-mode combo differential** (the 16-mode-style gate, patterned on
   `cached_commits_byte_identical_to_uncached`, `native_trie.rs:1152`,
   and the 3b 7-config matrix): within mode 2, all combinations of
   {RESIDENT 0/1} × {ROOT_CACHE 0/1} (× {MEMBER_CACHE, PARALLEL} once 3b
   lands) over a 40-block adversarial sequence with a mid-run
   invalidation/restart ⇒ roots, trie CF, mirror CF, root oracle and BOTH
   book CFs byte-identical across all combos.
6. **Hash-only mirror gates**: rerun all `native_trie.rs` tests under the
   new framing; add a mirror-value witness (every mirror row is exactly
   32 B = keccak of the live CF value); rerun crash-reopen
   (`native_trie_survives_crash_reopen:1291`).
7. **Dirty-entry bound witness** (the point of the round): synthetic 16k
   touched-order block across M markets / L levels ⇒ assert
   `dirty_native_keys()` count ≤ L + M(meta) + stops + non-book entries,
   and `exec_root_dirty_buckets` ≪ touched orders.
8. Crash-consistency: kill between save and stash (resident), reopen,
   boot verify green, roots equal oracle.
9. VPS microbench legs (post-GREEN, not this round's deliverable):
   ab-differ-style per-block flush matrix at D ∈ {16k, 200k, 700k} in
   mode 2; A/B vs mode 1 at identical offered load.

## 6. Expected dirty-bucket arithmetic (16k touched orders/block, ~700k resting, 10 markets)

Before (mode 1, measured re-proof + ab-differ):

```
dirty root entries ≈ 16k order rows + 10 meta + ~1–2k balance/position entries ≈ ~18k
dirty buckets      ≈ 65536·(1 − e^(−18k/65536)) ≈ 15.7k   (observed 4.9k–18.3k)
root+write+flush   ≈ 16k × 43 µs warm + cold member-scan inflation + 16k-row batch
                   ≈ 0.7 s floor → 2–2.5 s observed
```

After (mode 2), assuming L ≈ 300 touched levels (30/market × 10) and
~1.5k balance/position entries (dominated by distinct traders, not
orders):

```
dirty root entries ≈ 300 (levels) + 10 (meta) + ~1.5k (balances/positions) ≈ ~1.8k
dirty buckets      ≈ ~1.8k (sparse — collisions negligible)               ~9× fewer
per-bucket cost    also falls: root-CF entry count collapses (book entries:
                   ~700k order rows → ~#resting levels), so bucket occupancy and
                   the un-cacheable member-scan/keccak terms shrink; hash-only
                   mirror cuts scanned+hashed bytes to 32 B/entry
root (warm)        ≈ 1.8k × ~25–35 µs ≈ 45–65 ms
level rehash       ≈ 16k × ~140 B ≈ 2.2 MB keccak ≈ 2–3 ms (RAM)
state_write        root rows ~1.8k × (52 B + 32 B mirror) ≈ 150 KB
                   + node-local rows 16k × ~140 B ≈ 2.2 MB (no mirror, no trie)
                   → tens of ms vs 0.48 s today
estimate           root+write ≈ 0.05–0.15 s/block vs 1.2–2.5 s today (~10–25×)
```

Honest caveats: (a) the balance/position term now dominates the dirty set
— if the bench drives ~10k distinct traders/block, dirty buckets stay
~10k and the win shrinks to ~2×; add per-cf-tag dirty-entry telemetry
(work item 7) so the funnel attributes the post-3c composition before we
chase the next term. (b) L is workload-dependent; marketable sweeps
concentrate on few levels (good), spray-limit workloads touch more.

Gate arithmetic: 42 ms/block total budget is not met by 3c alone
(engine 0.45 s / verify 0.29 s / body_persist 0.27 s remain Layer-2/3
work), but 3c removes the dominant flush term from the healthy-window
budget (~1.4 s → ~0.1 s estimated).

## 7. Implementation plan (ordered work items)

0. **Discipline**: all work on `perf/level-rows` (off `81ea957`). Port
   from `origin/*` by cherry-pick/re-type only — NEVER merge (`92fa3ac`
   A5-revert hazard). Builds/tests on VPS per the operational rule; one
   fresh genesis at integration; nothing lands with a default other than
   exact-today.
1. **Hash-only mirror** (self-contained, mode-independent —
   `crates/torus-state/src/native_trie.rs`): hash values at
   `build_native_trie_to_cf:253-254`, `leaves_from_db:202`, dirty paths
   `:533/:544` + `:719-741`; leaf assembly untouched (`:553-556`,
   `:748-756` already frame whatever the member map holds);
   `read_bucket_members:479` doc-comment update (values are hashes).
   Rerun trie gates + add mirror-value witness. (~50–100 LOC)
2. **New CF**: `CF_BOOK_ORDER_ROWS` in `crates/torus-state/src/cf.rs`
   (`:44` region, `ALL_CF_NAMES:147`). Confirm `NATIVE_ROOT_CFS`
   (`native_trie.rs:38`) and `native_dirty()` (`backend.rs:357-374`)
   exclude it (they key off `cf_tag()` — verify with a test).
3. **OrderBook journal-in-book** (`crates/torus-core/src/order_book.rs`):
   port `order_seq`/book-`next_seq`/`row_journal`/`row_exists` +
   `level_journal`/`level_exists` + `take_row_ops`/`full_row_ops`/
   `take_level_ops`/`full_level_ops`/`encode_order_row` from `39c74a1`
   (order_book.rs hunks; journal sites: place `:938`, cancel `:491`,
   cancel_all `:516`, modify `:560`, match loop `:794-860`); delete our
   `BookJournal` (`:112-135`, `:1032-1059`) and its call sites; re-home
   level key codecs (`price_enc`, `side_tag`, keys — tag 0x03) into a new
   `torus-core` module (shape of their `order_book_store.rs`, minus the
   load/save halves that conflict with our executor).
4. **Executor rewrite** (`crates/torus-bridge/src/native_executor.rs`):
   mode enum replacing `book_rows: bool` (`:430-438` and `NativeExecContext`
   fields); save flow per §2.4 at `save_order_books:1277`; loaders per
   §4.1/§5.1 replacing `:1049-1244`; delete list per §2.3; `__book_mode__`
   marker; `save_book_full` for genesis/tests.
5. **Wiring + telemetry** (`crates/torus-consensus/src/app.rs:1136-1199`,
   `crates/torus-telemetry`): no structural change (flush API already
   carries the dirty set); add `torus_exec_book_levels_written/deleted`,
   `torus_exec_book_rows_written/deleted` (node-local), and per-cf-tag
   dirty-entry counters (§6 caveat a).
6. **Tests** per §5.2 (items 1–8), RED committed before GREEN where the
   harness allows.
7. **Integration round**: fresh genesis on VPS devnet, N-mode combo gate,
   then bench legs (§5.2 item 9). Not part of this design round.

Dependency order: 1 ⊥ {2,3}; 4 needs 2+3; 5,6 trail 4. If 3b
(`perf/root-cost`) merges first, item 1 rebases onto the unified
`apply_native_dirty()` (semantics unchanged; `Members` values become
hashes — member-cache budget effectively 4× larger).

## 8. Consensus-visible vs node-local pieces

Consensus-visible (all batched into THIS round's single fresh genesis):

1. Hash-only mirror leaf framing — changes every mode's root, all 6 CFs.
2. Mode value `TORUS_BOOK_ROWS=2` and everything it implies in
   `cf_native_order_books`: level rows (tag 0x03, key + 52 B value incl.
   `level_hash`), absence of 0x01 order rows.
3. Seq-at-insert timing (affects mode-1 row bytes and mode-2
   `level_hash` preimages).
4. (Unchanged but consensus: meta row layout, stop rows, `price_enc`
   ordering as part of the level key.)

Node-local (mixable per node, value-neutral by test gate §5.2-5):

- `cf_book_order_rows` content (deterministic materialization, verified
  at boot, never a preimage input)
- `__book_mode__` marker, `NEXT_GLOBAL_ORDER_ID_KEY`
- `TORUS_RESIDENT_BOOKS`, `TORUS_NATIVE_ROOT_CACHE`, and 3b's
  `TORUS_BUCKET_MEMBER_CACHE_MB` / `TORUS_PARALLEL_BUCKET_HASH`
- all journals, resident holders, caches

## 9. Open risks & production debt

1. **Adversarial mega-level**: one price level with N orders costs
   O(N) keccak each time it is touched (100k orders ≈ 13 MB ≈ ~13 ms;
   1M ≈ ~130 ms). Griefing requires paying margin/fees to park depth and
   touch it every block. Mitigation if it bites: chunked level hashing
   (fixed-fanout segment hashes, only touched segments rehash) — a
   further preimage change, so decide before mainnet freeze. For now:
   telemetry on max-level-depth + bench a hot-level cell.
2. **Balances/positions become the dirty-set floor** (§6 caveat). Next
   lever if the funnel confirms it; hash-only mirror already halves their
   bucket bytes.
3. **State-sync / snapshot protocol** for `cf_book_order_rows` does not
   exist; fresh nodes must replay from genesis until it ships.
   Verification design (level-hash check) is specified here; transport is
   not.
4. **RPC/precompile book readers are not row-aware** (existing debt,
   restated): under modes 1/2 they see empty books; `getOrderBook` should
   eventually ride level rows + `iterate_cf_bounded` (port of `39c74a1`'s
   read path — deliberately out of 3c scope). `margin_configs` empty ⇒
   zero margin reserved (existing debt, unchanged by 3c).
5. **Boot verify cost** O(depth) (~seconds at 1M): acceptable; do NOT add
   a skip flag (safety > boot latency); parallelize per market if it ever
   matters.
6. **Two-preimage-changes-in-one-round risk** (the reason `42bca69`
   deferred hash-only): mitigated by strictly separated work items
   (1 lands and gates green before 4), the mode-2 differential matrix,
   and the fact that both changes are exercised by the same oracle
   (`native_root_full`). This round should be the LAST preimage change
   before the freeze — reserve tag 0x04+ and the chunked-level-hash
   decision (risk 1) explicitly.
7. **3b/3c merge order**: both touch `native_trie.rs`'s dirty path.
   Whichever lands second pays a rebase (item-7 note). The preimage spec
   in §3.2 is identical on either base — no consensus interaction.
8. **`level_exists` vs delete-tombstones**: `take_level_ops` must emit a
   delete only for levels that HAVE a persisted row (their `level_exists`
   guard) or the root batch carries useless tombstones — harmless for the
   root (delete-of-absent is elided by the cached path,
   `native_trie.rs:732-741`) but noisy for stats; keep the guard.

## Appendix A — source-mining ledger

| what | from | verdict |
|---|---|---|
| `price_enc`, `side_tag`, level key layout, level journal sites, `iterate_cf_bounded`, level-oracle test shapes | `origin/feat/precompile-0800-topn-gas` @ `39c74a1` | adopt encoding + journal; INVERT authority (theirs additive read-side); re-tag 0x02 → 0x03 |
| hash-only mirror design | `origin/perf/deep-book-storage` @ `42bca69` (`docs/plans/hash-only-native-mirror.md`) | fold in; assumptions re-verified §3.2 |
| journal-in-book idiom, `order_seq`, `take_row_ops`, `save_book_delta`/`save_book_full` shape | same branches (`order_book_store.rs`) | adopt idiom; re-home; delete our shadow differ |
| differ exoneration + standardization verdict + delete list | `perf/ab-differ` `docs/ab-differ-findings.md` | followed §2 |
| member cache + parallel leaf hash | `perf/root-cost` @ `5a443ab` | orthogonal node-local; integration note §3.2/§7 |
| DO NOT merge `92fa3ac` / `perf/p3-throughput` wholesale | mission | restated §0, §7-0 |
