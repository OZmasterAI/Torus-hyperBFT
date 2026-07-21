# L3 save_books attribution — what the 107 ms actually is

Branch `perf/l3-savebooks` @ 134e44e (base `perf/re-proof5` @ 2564319). Explains
the l3scrape finding (`devnet/wsl/results/l3scrape-18c-b1aba10.md`): at the pegged
cap-400 cell, `exec_save_books_seconds` measured **106.7 ms per loaded block**
(n=1748) — the dominant exec consumer, where design intent said ~1–3 ms.

**Verdict up front:** the span is NOT doing wasted work and is NOT
deschedule-inflated. It is **genuine, uncontended CPU** — `level_row_data`
re-hashing the full FIFO queue of every *touched* price level (O(orders-at-level)),
run against **deep books**. The cell's stated premise "resting depth ≈ 0" is FALSE:
the scrape's own gauge reads **`torus_exec_resting_orders = 195024`** (~19.5k resting
orders/market). The offered load (300 k/s) ≫ matched (2.4 k/s), so ~99 % of orders
rest and the book grows monotonically; touching those deep levels re-hashes thousands
of orders per block. The `level_hash` is **consensus-frozen** (native state-root
preimage, `native_executor.rs:1090`), so the fix is consensus-visible → **STOP and
report** (sketch below), per the round's hard constraint.

---

## 1. Span anatomy — the 107 ms is `ctx.save_order_books()`

`app.rs:1390-1394` times exactly one call: `ctx.save_order_books()`.
In mode 2 (`BookMode::LevelAuthority`, the cell's `TORUS_BOOK_ROWS=2`) it iterates
**`self.dirty_books` only** (dirty-only — NOT all markets ✓) and per dirty market
(`native_executor.rs:1724`):

| # | sub-step | cost class | RocksDB |
|---|---|---|---|
| 1 | `take_row_ops()` → drain `row_journal`, `encode_order_row`, put/del `CF_BOOK_ORDER_ROWS` | O(changed orders) | overlay put (RAM) |
| 2 | **`take_level_ops()` → drain `level_journal`, `level_row_data` (keccak the WHOLE level queue), put/del level rows** | **O(orders at touched level)** | overlay put (RAM) |
| 3 | `diff_stop_rows()` → `iterate_cf(prefix=market‖0x02)` — 1 RocksDB prefix seek/market | O(1) + 1 read | read |
| 4 | `write_meta_if_moved()` → `get_cf_raw(meta_key)` — 1 RocksDB point read/market | 0–1 write + 1 read | read |

### Answers to the attribution questions

- **Anything iterating ALL markets vs dirty-only?** No — the loop is over `dirty_books`.
- **What is serialized per save in rows mode 2?** NOT a full book image. Per changed
  order: one order-row blob (node-local CF). **Per changed level: `qty‖count‖level_hash`,
  where `level_hash = keccak256(all framed order rows at that level, front→back)` —
  O(orders in the level).** Serialization is O(changed) in *count*, but the level hash
  is O(*depth*) in *compute*.
- **RocksDB read/write inside the span?** Writes are journaled O(changed) to the
  overlay (real disk write deferred to `exec_flush`). Two RocksDB *reads* per dirty
  market (steps 3–4) — but these are **negligible** (measured ~2 ms + 0.5 ms total,
  flat vs book size; the read-amplification hypothesis was tested and **rejected**).
- **Is the BookJournal O(changed) path engaged, or a fallback running?** The journal
  IS engaged (`take_row_ops`/`take_level_ops` drain the `BTreeSet` journals; write
  counts are O(changed), rank8's 201→≤3 witness holds). **There is no disengaged fast
  path.** The blind spot is different: rank8 proved *write counts* are O(changed) but
  never measured *hash compute*, which is O(depth) per touched level. On a resting≈0
  book that is invisible; on a 195 k-order book it is the whole 107 ms.

---

## 2. Sub-timer µbench (18c, uncontended single-thread)

`crates/torus-bridge/tests/l3_savebooks_ubench.rs` (#[ignore]) runs OVERLAY-backed
exactly like `app.rs` (so save-time puts are cheap RAM inserts and RocksDB writes are
billed to flush, not save — a StateDb-direct ctx over-bills save by the deferred
writes). Sub-timers gated behind `collect_save_timings` (default off = zero hot-path
cost). Numbers below are `--test-threads=1` (uncontended).

### 2a. Cell shape, resting ≈ 0 (baseline — matches old design intent)

| blk | rows µs | lvls µs | stops µs | meta µs | SAVE µs |
|---|---|---|---|---|---|
| 2–300 (steady) | ~190 | ~220 | ~450 | ~180 | **~0.8–1.3 ms** |

With empty levels the whole span is ~1 ms — the "~1–3 ms" the budget doc assumed.
Reads (stops+meta ≈ 0.6 ms) do NOT grow with churn (probed to 1000 blocks).

### 2b. Realistic depth sweep — 10 mkts × 40 levels, touch every level

| depth | resting | rows µs | **lvls µs** | stops µs | meta µs | SAVE µs |
|---|---|---|---|---|---|---|
| 1 | 400 | 3896 | 51 640 | 2167 | 439 | 58 245 |
| 5 | **2000** | 5705 | **106 619** | 2607 | 589 | **115 633** |
| 25 | 10 000 | 4896 | 436 642 | 2290 | 463 | 444 408 |
| 50 | 20 000 | 5884 | 818 253 | 2172 | 419 | 826 840 |
| 100 | 40 000 | 5863 | 1 638 920 | 2291 | 447 | 1 647 629 |
| 200 | 80 000 | 8209 | 3 539 726 | 2332 | 467 | 3 550 856 |
| 400 | 160 000 | 13 224 | 6 287 968 | 2388 | 420 | 6 304 120 |

**`lvls` is ~99 % of the span and scales linearly with total re-hashed order-frames**
(= touched levels × depth). `rows`, `stops`, `meta` stay flat (~4–13 ms, 2 ms, 0.5 ms)
regardless of depth. This sweep touches ALL 400 levels; in vivo a block touches only
the levels its ~400 orders + ~330 trades hit, so the per-block frame count is smaller.

### 2c. The 107 ms reconciliation

At **depth 5** the sweep re-hashes ~2000 order-frames (400 levels × ~5) and lands at
**106.6 ms** — essentially identical to the in-vivo **106.7 ms**. So the in-vivo block
re-hashes an equivalent of ~2000 order-frames of touched level depth per block
(e.g. touching ~10–20 hot levels holding ~100–170 resting orders each, out of the
195 k-order book). **Because these µbench numbers are single-threaded and uncontended,
the in-vivo 107 ms is real CPU, essentially un-inflated by deschedule** — unlike
engine (16 → 43 ms, ~2.7×) and flush. The per-order-frame cost is ~40–64 µs (preimage
build + keccak of the growing per-level preimage).

---

## 3. Root-cause verdict — ARCHITECTURAL (consensus-frozen), not a bug

`save_order_books` mode 2 recomputes each touched level's consensus hash as a full
keccak over the level's entire FIFO order queue. That is O(depth-at-touched-level).
The cell runs deep books (`exec_resting_orders = 195024`, driven by offered ≫ matched),
so the per-block re-hash is ~2000 frames ≈ 107 ms. This is **not** a defect, **not** a
silently-disengaged fast path, and **not** a deschedule artifact — it is honest
O(depth) hashing that the earlier write-count-only proof never measured.

The `level_hash` bytes are the **native state-root preimage** (frozen once deployed,
`native_executor.rs:1088-1091`; the mode-2 level rows are the level authority read by
the reload path). **Any change to how it is computed is consensus-visible** → out of
scope for this node-local round. No node-local change can cut it, because the hash IS
the consensus commitment; the only node-local wins (eliminating the ~2.5 ms stops+meta
reads via a RAM shadow) are negligible against the 100 ms and not worth the risk.

---

## 4. Ranked fix sketch (consensus-visible — DO NOT implement here)

All change the committed `level_hash`/level-row format ⇒ fleet-uniform, fresh genesis,
Fable + stateright (or a determinism/replay proof), and coordination with the
mode-2 authority + state-root owners.

1. **Chunked level commitment (recommended).** Replace `level_hash = keccak(all rows)`
   with `level_hash = keccak(chunk_hash₀ ‖ chunk_hash₁ ‖ …)` where each chunk covers a
   fixed run of K queue positions. A touch re-hashes only the affected chunk(s) + the
   top fold → O(K + depth/K) ≈ O(√depth) instead of O(depth). Bounded, simple,
   preserves FIFO/order semantics. At depth 170 and K=16 this is ~10× fewer bytes
   hashed per touched level.
2. **Merkle-over-orders per level.** `level_hash` = Merkle root of per-order leaves
   keyed by queue seq. Insert/remove/modify updates one leaf → O(log depth) path
   recompute. Preserves order via seq in the leaf. More invasive than chunking; also
   enables cheap level proofs.
3. **Incremental keccak absorb for tail-appends (narrowest, partially node-local-ish).**
   Keccak is a sponge: keep each resident level's absorbed hasher state and, when the
   only change is orders appended at the FIFO tail (the dominant case for a growing
   resting book), absorb just the new frames → **byte-identical digest** in O(delta).
   Front-removals/modifies (matching) still force a full recompute. This one keeps the
   committed bytes identical, so it is *closer* to node-local, but the per-level hasher
   state must be carried in the resident book and the append-only fast path must be
   proven to never diverge from the one-shot hash — still needs the full byte-identity
   matrix + a determinism proof before trust.
4. **Reduce the driver (not a hash fix).** The 195 k resting is a load-shape artifact
   (offered 300 k/s, matched 2.4 k/s ⇒ books never drain). A balanced campaign, or
   TTL/GC on stale far-from-touch resting orders, shrinks touched-level depth and thus
   the hash cost without any format change — worth flagging to the campaign owner.

**Recommendation:** option 1 (chunked) for the best effort/return, folded into a future
consensus-visible mode-2 revision; do NOT attempt in this node-local round.

---

## 5. Projected per-block savings

- Node-local (this round): **~0 useful** — only the ~2.5 ms stops+meta reads are
  node-local, negligible vs 107 ms; not worth the byte-identity risk. **No fix shipped.**
- Consensus-visible option 1 (chunked, K≈16): level-hash bytes/touched-level drop
  ~√depth → at in-vivo depth ~170, roughly **107 → ~10–20 ms** for the save span
  (bounded by chunk + fold), i.e. **~85–95 ms/loaded block** if adopted — but gated on
  a fresh-genesis format change and full consensus review.

---

## 6. Instrumentation & state-identity

Temporary sub-timers (`SaveTimings`, `collect_save_timings`, default off) added to the
mode-2 `save_order_books` arm are inert on the hot path when off (the default every
production/test path uses). State-identity witnesses re-run green on this branch:
`resident_matrix_state_identical`, `resident_restart_mid_sequence_identical`,
`resident_rows_save_is_incremental_via_journal`, plus the torus-bridge book/level/root
suites — see report. The instrumentation should be reverted (or kept #[cfg(test)]-gated)
before any merge; it exists only to produce §2.
