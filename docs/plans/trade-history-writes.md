# Design: cut the CPU cost of trade-history writes (s77)

## Problem

Every fill writes three RocksDB rows for RPC trade history, off the consensus
state: one in `CF_NATIVE_TRADES` (key `market | block | index`) and two in
`CF_NATIVE_USER_TRADES` (key `address | !block | index`, maker and taker). At
full load a native block carries ~75k fills (s76 summary), so ~227k puts per
block per node.

The s77 profile (main 07a34c2, cap 400) puts storage at ~38% of node CPU:
the trade-writer thread alone is 1.17 of 11.6 node cores, 86% of it RocksDB
write (memtable skiplist insert ~54%, WAL ~21%); compaction (`rocksdb:low`
1.91 cores, much of it ZSTD) and flush (`rocksdb:high` 0.67) include an
unknown share of trade rows. The s77 latency cells also show the trade
writer falling behind under load (60-84 queued batches), delaying when fills
become readable over RPC.

## Context (from memory + exploration)

- Rows are built in `TradeKvs::build` (`torus-bridge/src/native_executor.rs`),
  buffered under `defer_trades` and handed to `BackgroundCfWriter` per block
  (`torus-consensus/src/app.rs`); sync fallback when no writer.
- Trade CFs are node-local, outside the native consensus root. Loss on a hard
  crash is already accepted ("cosmetic RPC trade-history gap").
- `getUserTrades` is an address-prefix forward scan (`torus-rpc/src/torus.rs`),
  so user-trade keys must stay address-first. `getBlockTrades` scans
  `CF_NATIVE_TRADES` by block.
- All CFs share one `cf_opts` (128 MiB memtable, LZ4, ZSTD bottommost,
  dynamic level bytes) in `torus-state/src/db.rs`.
- Random-order keys (5,000 traders) make each memtable insert walk the
  skiplist from scratch (`RecomputeSpliceLevels` was the top trade-writer
  symbol); sorted inserts reuse the insert splice.

## Options

### Option A: measure the ceiling first (trade-history off switch)
Env `TORUS_TRADE_HISTORY=0` skips building and writing trade rows (default on,
unchanged). One full-load cell against the existing A cells prices the whole
feature: writer thread + WAL + its compaction/flush share. Doubles as a real
production option: validators that do not serve trade-history RPC (as on
Hyperliquid, where API nodes serve it) can turn it off.
- Files: `native_executor.rs` (skip `persist_trade` rows), `app.rs` (no
  writer / no deferral), a test that no trade rows are written when off.
- Effort: Small. Risk: Low (node-local; RPC trade history empty when off).

### Option B: sort each batch before writing
The writer sorts a block's rows by (CF, key) before the write, so memtable
inserts arrive in key order. Same bytes, same rows, same reads.
- Files: `torus-state/src/bg_writer.rs` (sort in the writer thread).
- Pros: attacks the top trade-writer symbol; no format or read change.
- Cons: sorting ~227k keys per block costs CPU too (off the exec thread);
  gain unknown until measured; WAL and compaction unchanged.
- Effort: Small. Risk: Low.

### Option C: no WAL for trade-history writes
Write trade batches with `disable_wal`. Removes ~21% of the writer's cost and
the WAL bytes. A hard crash then loses the trade rows still in the memtable
(up to one 128 MiB memtable) instead of only the queued batches.
- Effort: Small. Risk: Medium (bigger history gap after a crash; needs a
  decision on whether that is acceptable).

### Option D: thinner rows / cheaper compaction for trade CFs
Per-CF options for the two trade CFs (e.g. LZ4 at the bottom instead of
ZSTD), or user-trade rows holding a pointer to the trade row instead of a
full copy. Pointer rows change the stored format and the read path (two
lookups per trade) and need versioning; compression is a CPU-vs-disk trade.
- Effort: Medium. Risk: Medium.

## Recommendation

A first, then B. A is one small change and one cell, and it tells us how much
there is to win before investing in B-D. If the whole feature costs little,
we stop here; if it is large, B is the cheapest safe cut, and A itself is a
production option for validators.

## Not Building (YAGNI)

- Moving trade history to a separate DB or service: A covers the "validators
  don't need it" case with a flag.
- Changing the user-trade key order: the RPC prefix scan depends on it.

## Open Questions

- Should validators default to trade history off in production? (A makes it
  possible; the default stays on until decided.)
- Is a larger post-crash history gap acceptable (Option C)?
