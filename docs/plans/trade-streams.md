# Design: websocket trade and userFills streams (s80)

## Problem

`torus_subscribe("newTrades")` misses trades. The node's `on_commit_block`
handler (`crates/torus-node/src/main.rs:914`) calls `scan_trades_for_block` at
COMMIT, but execution runs behind commit (exec queue up to 64 blocks, ~71 s at
full load) and the trade rows land later still, on the `torus-trade-writer`
thread. The scan reads rows that do not exist yet, so the stream is mostly
empty under load. There is also no per-user fill stream.

Goal: feed a per-market trade stream and a per-address fill stream from the
per-fill records execution produces, not from DB rows at commit.

## Context (from memory + exploration, main d930405)

Fill data path (all verified in this worktree):

- `TradeFill` (`crates/torus-state/src/trade_rows.rs:44`): `trade_index: u32`
  (per block, across markets), `market: u64`, `maker`, `taker: Address`,
  `price_raw`, `qty_raw: i128`, `taker_side: u8`.
- `NativeExecutor::persist_trade` (`native_executor.rs:5001`) pushes into
  `ctx.pending_fills` **only when `ctx.trade_history`**. Both settle paths
  (sequential, parallel pass B) and `exec_place_order` call it; order is
  deterministic (market id, then prepared-order order, then fill order).
- `execute_committed_block_with` (`app.rs:2037`) takes the fills on the
  `torus-execution` thread, then (after flush / pipeline hand-off) moves them
  into an `EncodeRows` closure sent to the trade writer (`app.rs:2177-2199`).
- Missing from `TradeFill`: order ids (`Fill` has `maker_order_id` /
  `taker_order_id`; `persist_trade` drops them), tx hash, closed PnL
  (`PositionManager::apply_fill` discards it). Fees do not exist per fill.
  Liquidations never create fills.
- No hook from execution to the node crate. `TorusApp::new` (`app.rs:3257`)
  moves `exec_ctx` into the exec thread, so a sink must be passed into `new()`.
  torus-consensus has no tokio dependency.
- Boot replay (`replay_committed`, from `TorusApp::new`) runs the same path
  before RPC starts. Block sync fires the normal commit path. Heights at or
  below `applied` are skipped, so no duplicate fills.
- RPC side: `BlockNotifier.new_trades: broadcast::Sender<Vec<Value>>` (cap 256).
  The subscriber loop is `while let Ok(..) = rx.recv()`: on
  `RecvError::Lagged` it ends and drops the sink, so a slow subscriber is
  silently disconnected. Unknown subscription kinds are accepted, then dropped.
- Current `newTrades` JSON (from `scan_trades_for_block`, lib.rs:485): hex
  strings `marketId, tradeId, price, quantity, side, blockNumber, timestamp`,
  one WS message per trade, no addresses. No WS consumer found in any repo
  (explorer polls `torus_getBlockTrades`; the PRD plans `newTrades` with a
  `marketId` filter for the trading app).
- Perf context: trade history at full load costs 4.1 CPU-s/1M fills after s78;
  the exec thread is the bottleneck, so the sink must be O(1) per block there.

## Options

### Option A: fill sink at execution, fan-out in RPC (recommended)

After `take_pending_trade_fills`, the exec thread wraps the block's fills once
as `Arc<BlockFills { height, timestamp, fills }>`. It calls an optional sink
(`Arc<dyn Fn(Arc<BlockFills>) + Send + Sync>`, passed to `TorusApp::new`) and
moves the same `Arc` into the trade-writer closure (`encode_block` takes
`&[TradeFill]`, so no copy). Node `main.rs` builds the sink as a closure over
a `tokio::sync::broadcast::Sender<Arc<BlockFills>>` held by `BlockNotifier`.
Each subscription task receives the `Arc`, filters raw `TradeFill`s (market id
or address compare) and serializes only matching fills, one message per block.
`persist_trade` records fills when `trade_history || record_fills` (sink
present); rows are written only when `trade_history`, so streams work on
nodes with `TORUS_TRADE_HISTORY=0`. The commit-time scan is removed.

- Files: `torus-state/src/trade_rows.rs` (BlockFills, maybe order ids),
  `torus-bridge/src/native_executor.rs` (record flag, inline write guard),
  `torus-consensus/src/app.rs` (sink param, Arc hand-off), `torus-rpc/src/lib.rs`
  + `torus.rs` (notifier type, subscriptions, lag handling), `torus-node/src/main.rs`.
- Pros: lowest latency (emitted at execution, before rows land); exec-thread
  cost is one Arc + one non-blocking broadcast send per block; no DB re-read;
  independent of trade history; no new deps in torus-consensus.
- Cons: a client can see a fill on the stream before `getUserTrades` returns
  it (rows land shortly after). Per-subscriber filtering is O(fills x subs):
  fine for tens of subscribers, ~100M compares/s at 1000 subs x 100k fills/s.
- Effort: Medium. Risk: Low-Medium (touches the exec hot path, small change).

### Option B: emit from the trade writer after rows land

Same `BlockFills`, but emitted from the writer's `on_written` callback on
`torus-trade-writer`, after the rows are in the DB.

- Pros: stream and `getUserTrades` always agree; zero added exec-thread work.
- Cons: only works with trade history on (conflicts with running validators at
  `TORUS_TRADE_HISTORY=0` unless stream nodes keep history); adds writer-queue
  latency; the writer's synchronous fallback path needs the same emit.
- Effort: Medium. Risk: Low.

### Option C: keep the DB scan, trigger it at executed height

Move `scan_trades_for_block` from commit to "rows written for height N".

- Pros: smallest diff.
- Cons: re-reads rows just written (CPU per block, per node); needs history on;
  still no addresses or userFills without changing the row reader. Rejected by
  the owner's direction ("not read back from the database").
- Effort: Small. Risk: Low.

## Recommendation

Option A. It closes the gap at its source, keeps streams available on nodes
with history off, and costs the exec thread one Arc and one send per block.
The one semantic cost (stream ahead of `getUserTrades` by the writer delay) is
documented in the API, and matches how Hyperliquid streams behave.

Proposed semantics:
- Delivery: in height order, at most once; each message carries `blockNumber`
  and per-fill `tradeId` so clients detect gaps and dedupe across reconnects.
- Finality: HotStuff commits are final, so no reorg handling.
- Lag: a subscriber that falls behind gets an explicit error and is closed
  (it reconnects and backfills via `torus_getTradeHistoryRange` /
  `torus_getUserTrades`), instead of today's silent drop.
- `newTrades {marketId?}`: one message per block with an array of that
  market's fills; adds `maker`, `taker` (and order ids if Q3 = yes).
- `userFills {user}`: one message per block with the fills where `user` is
  maker or taker, with `side` from the user's view and an `isTaker` flag.
- Reject unknown subscription kinds and bad params before accepting.

## Not Building (YAGNI)

- Routing hub (market/address -> subscriber maps): per-subscriber filtering is
  enough until a node has hundreds of stream subscribers. Revisit on evidence.
- Closed PnL, fees, tx hash in fill messages: no fee exists; PnL and hash need
  executor changes on the hot path. Separate feature if the app needs them.
- Snapshot-on-subscribe: clients backfill with the existing history RPCs.
- Replay suppression: boot replay runs before RPC starts, so there are no
  receivers; block sync fills are new to this node's subscribers.
- A new crate or tokio in torus-consensus: a plain closure sink is enough.

## Open Questions

1. Message granularity: one message per block (array) per subscription, or
   keep one message per trade? (Recommend per block: 100k fills/s as single
   WS messages is heavy; no existing WS consumer to break.)
2. Names: keep `newTrades` and add `userFills`, or adopt Hyperliquid names
   (`trades`, `userFills`) and message shapes?
3. Add `maker_order_id` / `taker_order_id` to `TradeFill`? userFills is weak
   without them (clients match fills to orders by id). Costs 16 bytes per fill
   in memory; not written to the packed rows unless we choose to.
4. Lag policy: close with error (recommended), or send a gap notice and keep
   streaming?
