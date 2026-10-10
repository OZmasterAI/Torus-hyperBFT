# Design: trades + DA compaction tuning (Phase 3 step 0, option 3)

## Problem

Phase 3 step 0 (results doc section 37) found that on Classic, the testnet configuration,
Phase 3's state-row work can remove at most ~1.76 node CPU-s per 1M fills (6.4%), short of the
-10% gate (2.75). RocksDB compaction of the append-only CFs is larger: trades
(`cf_native_trades`, `cf_native_user_trades`) 1.69 and DA (`cf_native_pending`) 0.74 CPU-s/1M,
2.43 together (8.9% of node CPU, 78% of compaction). Those compactions go from L0 (LZ4) to the
bottom level (ZSTD) and write nearly what they read: mostly re-compression.

Owner decision (18c s108): do the trades + DA compaction tuning first, as its own small item.
Gate baseline: Classic on main `9b7e29b2` (node `6a71ba5f`, `ozarchy-p3s0r-stage/m`). Measure
node CPU per 1M fills and disk bytes per 1M fills in a Classic A/B against `9b7e29b2`. Then decide
whether Phase 3's state-row work goes ahead at a lowered gate (~-6%) or effort moves to signature
checks or execution. Streaming P2-2 stays a candidate either way.

Constraints (18c s108):
- Node-local options only; no consensus or state-root change.
- Must open existing data dirs without a resync (in-place upgrade, mixed fleet). Compression
  changes are fine (RocksDB reads old codecs, applies new ones to new files only). A compaction
  style change needs a leveled <-> universal test on a copy of a real data dir. FIFO is out (it
  deletes trades / DA). Anything that only works on a fresh dir is flagged for the owner.
- Measure disk bytes per 1M fills next to CPU: LZ4 vs ZSTD trades CPU for disk.
- Also report absolute disk write rates (MB/s, WAL vs SST flush vs compaction) for Classic at
  bench load, and the share that is the per-block state-row rewrite (flush worker batch, Classic
  ~10.0 MB per native block vs mode 3 ~4.1): those bytes collapse in the memtable but still hit the
  WAL. Input for an SSD-wear case for Phase 3's coalescing. First pass from the existing p3s0c /
  p3s0cf cells (analysis only); the A/B cells report the same split.

## Context (from memory + exploration)

- `crates/torus-state/src/db.rs` `StateDb::open` (~line 629): every CF shares one `cf_opts`:
  128 MiB memtable, `compression = Lz4`, `bottommost_compression = Zstd` (no level set, so the
  RocksDB default level), `level_compaction_dynamic_level_bytes = true`. Overrides exist only for
  `cf_consensus_meta` (8 MiB memtable), the churny book CFs (env `churny_cf_write_buffer_bytes`)
  and `cf_native_order_books` (target file size). Pattern to follow: an env-gated per-CF override
  that defaults to exact-today.
- With dynamic level bytes and this data size, L0 compacts straight into the bottom level, so
  every trade / DA byte is written LZ4 at flush, read back, and rewritten ZSTD (p3s0cf: trades
  1.37 GB in, 1.19 GB out per node per run).
- Keys are not globally append-ordered: `trade_key(market, block, chunk)` and
  `user_trade_key(trader, block)` are prefixed by market / trader, so new L0 files overlap the
  whole bottom-level key range and each L0 compaction also rewrites overlapping bottom files.
- `TORUS_ROCKSDB_MAX_TOTAL_WAL_MB` (default 512 MiB) already exists. "WAL full" flushes
  dominate the big CFs (p3s0cf val0 ~475 per run on Classic vs ~367 on mode 3); mode 3's fewer
  flush rounds cut trades / DA compaction by 0.75 CPU-s/1M.
- rust-rocksdb 0.24 (RocksDB 10.4.2) has `set_bottommost_compression_options(w_bits, level,
  strategy, max_dict_bytes, enabled)` for the ZSTD level, and per-CF options via the descriptors.
- p3s0cf showed the per-cell `LOG` copy gives an exact per-CF split (LOG sums = node counters);
  the A/B reuses that driver.

## Options

### Option A: codec only, per CF (append-only CFs)

How: a node-local env knob selects the compression for the three append-only CFs only, e.g.
`TORUS_ROCKSDB_APPEND_CF_CODEC` = unset (exact-today: LZ4 then ZSTD bottom), `zstd1` (bottom ZSTD
level 1), `lz4` (LZ4 at every level), `none-l0` (no compression at flush, ZSTD bottom). Applied
through the existing descriptor loop. Old files keep their codec; new files use the new one.

Files: `crates/torus-state/src/db.rs` (knob parse + per-CF override), its tests.

Trade-offs: in-place safe and reversible (downgrade reads mixed files too); smallest change. LZ4
everywhere likely removes most of the ZSTD compress CPU but grows the trades / DA SST bytes on
disk (size not known; measured). ZSTD level 1 keeps most of the disk ratio at lower CPU.
`none-l0` saves the flush-side LZ4 but makes L0 files larger.

Effort: Small. Risk: Low.

### Option B: fewer flush rounds for the big CFs

How: raise `TORUS_ROCKSDB_MAX_TOTAL_WAL_MB` (existing knob, no code) and / or give the
append-only CFs a larger memtable (new per-CF knob, same pattern as the churny override), so
fewer, larger L0 files reach the bottom level and each bottom file is rewritten fewer times.
This is the mechanism mode 3 hit by accident.

Files: none for the WAL knob arm; `db.rs` for a per-CF memtable knob.

Trade-offs: in-place safe. Costs memtable memory (bounded by `db_write_buffer_size`) and longer
WAL replay on restart (measure restart -> ready). Does not change the bytes compressed once.

Effort: Small. Risk: Low (memory / restart time to check).

### Option C: universal compaction for the append-only CFs

How: switch the three CFs to universal compaction, which rewrites data less often than leveled.

Trade-offs: the largest possible cut in rewrites, but changing compaction style on an existing
leveled dir needs migration care (owner: test leveled <-> universal on a copy of a real data dir),
more space amplification, and a different read profile. Possibly fresh-dir only: if so, flagged
for the owner.

Effort: Medium. Risk: Medium-High.

### Option D: WAL compression (disk bytes, not CPU)

How: `Options::set_wal_compression_type(Zstd)` (rust-rocksdb 0.24, RocksDB 10.4; ZSTD is the only
WAL codec) behind a node-local env knob, default off. Disk write rates at bench load (p3s0c /
p3s0r / p3s0cf, n = 6 per mode, analysis 2026-10-10) put the WAL at **131.7 of 175.5 MB/s per
validator on Classic (75%)**, and the per-block state-row rewrite (flush worker batch, ~11.8 MB
per native block) at 74.1 MB/s, 56% of the WAL and 42% of all writes; SST from memtable flush 17.0,
from compaction 21.5. Options A-C only touch the 21.5. Compressing the WAL attacks the largest
byte stream without any state-format work, and is a cheaper SSD-wear lever than Phase 3's
coalescing; it costs ZSTD CPU on the write path (measured in the same A/B).

Trade-offs: node-local, opens existing dirs (RocksDB reads compressed and plain WAL records
alike); a downgrade to a binary on an older RocksDB that cannot read compressed WAL records would
need a clean shutdown first (WAL flushed) - check and document. Write-path CPU on the writing
threads (flush worker, trade writer) rather than in background threads.

Effort: Small. Risk: Low-Medium (write-path latency; measure flush / block ms).

## Recommendation

Do A, B and D in one screening campaign (owner go, s42); keep C in reserve unless A + B fall short of a
useful cut. Both are node-local, default to exact-today, open existing dirs and can be rolled
back. One new knob (codec) plus the existing WAL knob covers the screen; add the per-CF memtable
knob only if the WAL arm shows flush rounds matter.

Campaign (Classic, same shape as p3s0c, `LOG` copied per cell as in p3s0cf):
- arms: base (new binary, knobs unset = must match `6a71ba5f`), plus one control cell on the
  `6a71ba5f` stage binary; `zstd1`; `lz4`; WAL 2048 MiB; WAL compression (option D, if the owner
  wants the SSD-wear lever screened now); then the best combination.
- per cell: node CPU-s/1M (val0, threads.py), compaction CPU and bytes per CF (LOG), disk write
  GB/1M and MB/s split WAL / flush SST / compaction SST, SST bytes on disk per CF at the end,
  matched/s; restart -> ready for the WAL arm.
- n = 2 per arm first (screen), n = 4 for the arm we would ship.

Tests first: knob parse (unset / each value / garbage -> default with a warning); the OPTIONS
file RocksDB writes shows the chosen codec on the three CFs only; a DB written with today's
options reopens with each new setting and reads every row back, and the reverse (downgrade).

## Not Building (YAGNI)

- FIFO compaction (deletes trades / DA; owner: out).
- Per-level codec tables for every CF; only the three append-only CFs change.
- A compaction-style migration tool unless option C is chosen.
- ZSTD dictionary training (small values, not measured to help).

## Open Questions

1. Disk growth the owner accepts for `lz4` on trades / DA (the A/B measures SST bytes per 1M fills).
2. Does RocksDB 10.4 still allow trivial moves into the bottom level when the input file's codec
   differs from the bottommost codec? Affects how much `none-l0` / `lz4` save. Check in the A/B logs.
3. Option C only if needed: does a leveled -> universal switch open a real data dir in place?
4. Whether the shipped setting becomes the compiled default (like 9.13), a later decision.
