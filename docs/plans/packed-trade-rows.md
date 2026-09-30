# Design: packed trade-history rows (s77)

## Problem

Trade history writes three RocksDB rows per fill. At full load (cap 400,
~75k fills per native block) that is ~227k rows per block per node, and it
costs ~28.5 node CPU-s per 1M matched orders (~40% of node CPU):

| s77 cell, full load | rows / fill | node CPU-s / 1M | matched/s |
|---|---|---|---|
| full history (5 cells) | 3 | ~70.4 | ~68.4k |
| per-market rows only (`s77-split-split-r1`) | 1 | 50.8 | 92.7k |
| no history (`s77-th-th0-r1`) | 0 | 41.9 | 102.8k |

Both tables cost about the same per row (~9-10 CPU-s / 1M each), although one
has sequential keys and the other random ones. The cost follows the number
of rows, not their order. Goal: keep full RPC trade history while writing
~15-30x fewer rows.

## Context (from memory + exploration)

- Rows today (`TradeKvs::build`, `torus-bridge/src/native_executor.rs`):
  - `CF_NATIVE_TRADES`: key `market(8) | block(8) | trade_index(4)`, 65-byte value
    (trade_id u128, price i128, qty i128, taker_side, block u64, timestamp u64)
  - `CF_NATIVE_USER_TRADES` x2 (maker, taker): key `address(20) | !block(8) | trade_index(4)`,
    74-byte value (+ market, role)
- `trade_index` is per block, across markets, in canonical order; parallel
  settle builds rows in workers and stamps the index in pass B.
- Rows go through `route_trade_kvs` into a per-block `PackedCfBatch`, handed
  to `BackgroundCfWriter` after the flush (`app.rs`); `fills_visible` is
  observed when the batch is written. Both CFs are node-local, outside the
  native consensus root. `TORUS_TRADE_HISTORY=0` (merged) turns them off.
- Readers (`torus-rpc`): `get_trade_history(market, limit)` (newest in a
  market), `get_trade_history_range(market, from, to, limit)`,
  `get_block_trades(block)` via `scan_trades_for_block`, and
  `get_user_trades(address, market?, limit ≤ 1000)` (address-prefix scan,
  newest first). Nothing else reads these CFs (the bench harness does not).

## Options

### Option A: one row per (market, block) and one per (trader, block), grouped off the exec thread (recommended)
- The exec thread pushes one compact fixed-size record per fill (trade_index,
  market, maker, taker, price, qty, taker_side) instead of three key/value rows.
  This is less work than today.
- The background writer groups each block's records and writes:
  - `CF_NATIVE_TRADES`: key `market | block` → timestamp once + the block's
    fills in trade_index order (~37 bytes each).
  - `CF_NATIVE_USER_TRADES`: key `address | !block` → timestamp once + that
    trader's fills in the block (market, role, ~46 bytes each).
- Rows per full-load block: ~10 market rows + one row per active trader
  (≤ a few thousand) instead of ~227k; bytes also drop (shared fields stored
  once, better compression).
- Readers decode rows and slice them: newest-first iteration stays a key
  scan; `limit` is applied after unpacking.
- Keys stay deterministic per block, so a crash-replay rewrite still
  overwrites the same rows (idempotent, as today).
- Files: `native_executor.rs` (record instead of `TradeKvs`, incl. parallel
  settle), `app.rs` (hand-off), `torus-state` writer or a small encoder
  module, `torus-rpc` (four readers), tests.
- Estimated gain: most of the 28.5 CPU-s / 1M (estimate ~48-53 node CPU-s /
  1M and ~88-97k matched/s at full load; to be measured).
- Effort: Medium-Large. Risk: Medium (storage format + RPC read code).

### Option B: pack only the per-market rows
Per-market rows packed, per-user rows unchanged.
- Cons: the per-user rows are ~69% of the cost; leaves most of it.
- Effort: Medium. Risk: Low-Medium.

### Option C: one blob per block plus small per-user pointer rows
Per-user rows hold only pointers into the block's trades.
- Cons: a user query must read the market rows it points into (hundreds of
  KB per market per block at full load) — slow reads for every user query.
- Effort: Medium. Risk: Medium.

## Recommendation

Option A. It removes most rows from both tables, moves the grouping cost off
the exec thread (the exec thread does less than today), keeps every RPC
method and its semantics, and stays idempotent under crash replay.

## Not Building (YAGNI)

- Keeping the per-fill format as a third mode: history is either off or packed.
- Bounded per-user retention (Hyperliquid keeps 10k fills per user): a
  separate decision; compaction cost follows inserts, not retention.
- A separate indexer service or file export: `TORUS_TRADE_HISTORY=0` already
  covers validators that do not serve history.

## Open Questions

- Upgrade path for existing node-local history:
  1. wipe the two CFs on first start of the new format (testnet: simplest), or
  2. readers accept both formats (old keys are 20 / 32 bytes, new 16 / 28, so
     key length tells them apart) and old rows age out naturally.
- Per-market rows at full load are ~250-300 KB per block. Fine for RocksDB, but
  `get_trade_history(limit 100)` then reads one large row to return 100 trades.
  Acceptable, or chunk rows (e.g. ≤ 4k fills per row)?
