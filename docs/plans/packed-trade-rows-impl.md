# Implementation Plan: packed trade-history rows

## Design Decision

Option A of `packed-trade-rows.md`, with the user's calls (s77):
- Existing node-local history is **wiped** on first start with the new format
  (devnet, no users).
- Per-market rows are **chunked** at ≤ 1024 fills.
- The fill stream (websocket) is a separate, later feature; the per-fill
  record below is what it will subscribe to.

Row layout (all integers fixed-width; keys big-endian, values little-endian
like today):

| CF | key | value |
|---|---|---|
| `CF_NATIVE_TRADES` | `market u64 | block u64 | chunk u16` (18 B) | `ver u8=1 | timestamp u64 | n u16 | n × {trade_index u32, price i128, qty i128, taker_side u8}` (37 B per fill) |
| `CF_NATIVE_USER_TRADES` | `address[20] | !block u64` (28 B) | `ver u8=1 | timestamp u64 | n u16 | n × {trade_index u32, market u64, price i128, qty i128, taker_side u8, role u8}` (46 B per fill) |

- A market's fills in a block are in `trade_index` order; chunk `k` holds its
  fills `[1024k, 1024k+1024)`. A trader's row holds all of their fills in the
  block (maker and taker entries) in `trade_index` order.
- Rows are built **once per block** from all of the block's fills (both
  `execute_batch` phases), so phases never overwrite each other and every
  execution path (sequential / parallel settle, serial / pipelined) produces
  identical bytes.
- `trade_id` stays the per-block `trade_index` (unchanged semantics).
- Format marker `trade_history_format = 2` in `CF_CONSENSUS_META`; if absent
  or different at startup, both trade CFs are wiped (delete-range) before
  replay, then the marker is written.

## Success Criteria

1. RPC `getTradeHistory`, `getTradeHistoryRange`, `getBlockTrades` (and the
   `newTrades` subscription via `scan_trades_for_block`) and `getUserTrades`
   return the same fields, order and limit semantics as today.
2. Rows per full-load block drop from ~3 per fill to ~(markets × chunks +
   active traders); no per-fill rows remain.
3. Byte-identical rows across sequential / parallel settle and serial /
   pipelined execution (existing whole-CF equality tests keep passing).
4. `TORUS_TRADE_HISTORY=0` still writes nothing; `ctx.trade_index` counts every
   fill in both settle paths regardless of the switch (fixes an s77 regression
   in the parallel path).
5. Workspace tests pass; one full-load cell shows the CPU / throughput effect.

## Tasks

### Task 1: codec in `torus-state` (`src/trade_rows.rs`)
- Test first: round-trip encode → decode for both CFs; a market with 2,500
  fills gives chunks 0, 1, 2 (1024, 1024, 452); a trader who is both maker and
  taker of one fill gets two entries (roles 0 and 1); same input → same bytes;
  decoding rejects a wrong version / truncated value with an error, never a panic.
- Implementation: `pub struct TradeFill { trade_index: u32, market: u64, maker:
  Address, taker: Address, price_raw: i128, qty_raw: i128, taker_side: u8 }`;
  `pub fn encode_block(block: u64, timestamp: u64, fills: &[TradeFill], out:
  &mut PackedCfBatch)`; `decode_trade_row` / `decode_user_row`; key helpers.
- Verify: `cargo test --release -p torus-state trade_rows`

### Task 2: executor collects fills instead of rows
- Test first (`torus-bridge/tests/trade_defer_tests.rs`, rewritten): one block
  with fills in two `execute_batch` calls → `take_pending_trade_fills()` returns
  every fill once, in `trade_index` order; `trade_index` equals the fill count
  in sequential and parallel settle, with history on AND off.
- Implementation: `ctx.pending_fills: Vec<TradeFill>` replaces
  `pending_trades` and the inline puts; `TradeKvs` and `route_trade_kvs` are
  removed; pass A builds `TradeFill` (index 0), pass B stamps the index;
  `trade_index` is incremented per fill in both paths. Bridge-level callers
  without a writer use a helper that encodes and writes the block's rows.
- Update: `trade_kvs_tests.rs` (legacy encoder test retired, replaced by Task
  1), `cancel_batch_exec_tests.rs` fill check, bench-throughput KV count print.
- Verify: `cargo test --release -p torus-bridge`

### Task 3: writer encodes off the exec thread
- Test first (`bg_writer.rs`): `send_encode_then(encode, on_written)` runs the
  encoder on the writer thread, writes its rows, then runs the hook; on
  handback both closures are returned unrun.
- Implementation: `CfBatch::Encode(Box<dyn FnOnce() -> PackedCfBatch + Send>,
  Option<OnWritten>)`.
- Verify: `cargo test --release -p torus-state bg_writer`

### Task 4: block-end hand-off in `app.rs`
- Test first: `deferred_trades_reach_db_via_background_writer` and the s77
  order-age / pipeline tests updated to the new keys (block read from the new
  key layout); fills_visible still observed once per action of a trading block.
- Implementation: take the block's fills; with a writer, send an encode
  closure (`encode_block`) + the fills_visible hook; sync fallback encodes and
  writes inline.
- Verify: `cargo test --release -p torus-consensus`

### Task 5: format marker + wipe before replay
- Test first: a DB holding old-format rows → after `TorusApp::new` both CFs
  are empty and the marker is 2; a second start with new rows present leaves
  them untouched.
- Implementation: `ensure_trade_history_format(&state_db)` next to
  `ensure_trie_built` (before `replay_committed`), `delete_range_cf` over the
  full key range of both CFs.
- Verify: `cargo test --release -p torus-consensus trade_history_format`

### Task 6: RPC readers
- Test first (`torus-rpc/src/lib.rs` tests): rewrite `store_trade` /
  `store_user_trade` helpers to write rows via the Task 1 codec; existing
  cases keep their expected output (ids, order, limit, market filter, newest
  first); new cases for `getTradeHistoryRange`, `getBlockTrades` and a chunk
  boundary (limit spanning two chunks).
- Implementation: the four readers decode rows and slice them; pruned-block
  cut-off unchanged.
- Verify: `cargo test --release -p torus-rpc`

### Task 7: prove it
- `cargo test --release --workspace`; one full-load cell (cap 400) vs the
  full-history cells (~70 CPU-s / 1M, ~68k matched/s) and history off (41.9,
  102.8k).

## Rollback

Revert the merge. History written in the new format is not readable by the
old code; the old code keeps working (it reads nothing from new-format rows —
wipe the two CFs to start clean).
