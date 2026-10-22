# WebSocket streams

A node pushes market data to WebSocket clients through four JSON-RPC
subscriptions:

- `newTrades`: public trades, per market or for all markets. All markets is
  off on validators by default (see [All-markets `newTrades`](#all-markets-newtrades)).
- `userFills`: the fills of one address, with its position effect.
- `l2Book` (s139): one market's order book, a full snapshot of its best
  levels on subscribe and after each block that changed them. See
  [`l2Book`](#l2book).
- `orderUpdates` (s139): every state change of one address's orders (placed,
  partially filled, filled, canceled, rejected, triggered, modified). See
  [`orderUpdates`](#orderupdates).

The rest of this section and the next ones describe the two trade streams;
`l2Book` and `orderUpdates` have their own sections below.

Both are fed from execution (the fills each executed block produced), not read
back from the database. They work on nodes without trade history: validators
by default (s109), or any node with `TORUS_TRADE_HISTORY=0`. On such nodes the
history RPCs return nothing, but the streams still deliver. `--rpc-only` nodes
keep trade history on unless `TORUS_TRADE_HISTORY=0`.

## Subscribing

Connect to the node's RPC port over WebSocket and call `torus_subscribe` with
the kind and its params:

```json
{"jsonrpc":"2.0","id":1,"method":"torus_subscribe","params":["newTrades",{"marketId":"0x1"}]}
{"jsonrpc":"2.0","id":2,"method":"torus_subscribe","params":["newTrades"]}
{"jsonrpc":"2.0","id":3,"method":"torus_subscribe","params":["userFills",{"user":"0xaaaa…aaaa"}]}
```

| Kind | Params | Notes |
|---|---|---|
| `newTrades` | `marketId` (optional, hex u64 like `"0x1"`) | Omit it for every market, where the node allows that (see below). |
| `userFills` | `user` (required, 20-byte hex address) | Fills where `user` is maker or taker. |
| `l2Book` | `marketId` (required, hex u64), `nLevels` (optional, 1-100, a number or hex string; default 20) | Levels per side. |
| `orderUpdates` | `user` (required, 20-byte hex address) | The user's order transitions. |

- The reply is a subscription id.
- `torus_unsubscribe` with that id stops the subscription.
- An unknown kind or a bad param is rejected with error `-32602` before any
  subscription is created.
- A node allows 1000 subscriptions at once and rejects more with `-32000`.

### All-markets `newTrades`

At full load one block's all-markets array can be tens of MB of JSON. So
`newTrades` without `marketId` is:

- **rejected on validators** by default, with `-32602`:
  `invalid params: newTrades without marketId is disabled on this node;
  subscribe per market or use a node that allows it; operators:
  TORUS_ALL_MARKET_TRADES=1`;
- **allowed on `--rpc-only` nodes** by default.

Per-market `newTrades` and `userFills` are allowed on every node.

Operators override the default with the node-local env var
`TORUS_ALL_MARKET_TRADES`:

| Value | All-markets `newTrades` |
|---|---|
| unset | allowed only on `--rpc-only` nodes |
| `1` | allowed |
| `0` | rejected |
| anything else | a warning is logged; the default applies |

The node reads it once at startup. It is checked only when a subscription is
opened, so it adds nothing to execution or to block delivery.

## Messages

Each notification carries **one executed block's matching fills as an array**,
in trade order. Blocks without a matching fill send nothing.

A block's `newTrades` array is **serialized once per filter** (all markets, or
one `marketId`) and the same bytes go to every subscriber with that filter.
Extra subscribers on a filter add only the frame copy, not a new
serialization. `userFills` arrays are built per subscriber.

### `newTrades`

```json
{"jsonrpc":"2.0","method":"torus_subscription","params":{"subscription":"<id>","result":[
  {"tradeId":"0x0","marketId":"0x1","price":"100.00000000","quantity":"2.50000000","side":"buy",
   "blockNumber":"0x7","timestamp":"0x6553f107",
   "maker":"0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","taker":"0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"}]}}
```

- The fields are those of `torus_getBlockTrades` / `torus_getTradeHistory*`
  rows, plus `maker` and `taker`.
- `side` is the **taker's** side, so it says whether the trade was buy- or
  sell-initiated.

### `userFills`

```json
{"jsonrpc":"2.0","method":"torus_subscription","params":{"subscription":"<id>","result":[
  {"tradeId":"0x0","marketId":"0x1","side":"sell","price":"100.00000000","quantity":"2.50000000",
   "role":"maker","orderId":"0xb","startPosition":"3.00000000","closedPnl":"-82.50000000",
   "dir":"Close Long","blockNumber":"0x7","timestamp":"0x6553f107"}]}}
```

| Field | Meaning |
|---|---|
| `side` | The **user's own** side (`buy` / `sell`), as in `torus_getUserTrades` |
| `role` | `maker` or `taker` |
| `orderId` | The user's order that filled |
| `startPosition` | The user's signed position in this market before the fill (long > 0, short < 0) |
| `closedPnl` | Realized PnL of the fill's closing part; `0.00000000` when the fill only opened or increased |
| `dir` | `Open Long`, `Open Short`, `Close Long`, `Close Short`, `Long > Short` or `Short > Long` (a flip) |

There are no self-trades: an order that would match the same user's resting
order cancels that resting order instead (self-trade prevention).

## Encoding

- **Prices, sizes, positions and PnL** are decimal strings with 8 decimals
  (`"123.45000000"`, `"-0.50000000"`). This holds for **all** `torus_*`
  methods since s80; before that they were raw fixed-point hex.
- **Ids, block numbers and timestamps** stay `0x` hex. `timestamp` is the
  block timestamp in seconds.
- `eth_*` methods are unchanged (hex).

## Delivery rules

- **When:** fills are sent right after the node executes the block. Execution
  runs behind commit (seconds under load), and blocks are final once
  committed, so fills are never retracted.
- **Order:** blocks arrive in height order. Within a block, entries are in
  `tradeId` order.
- **Only while subscribed:** streams carry fills only while a subscriber is
  connected. A node records fills for the streams only while at least one
  stream subscriber exists, so a block executing at the moment you subscribe
  may be missed; backfill covers it. `userFills` may also skip the block
  executing at the moment you subscribe, even when `newTrades` delivers it.
- **Ahead of history:** a fill can arrive on the stream shortly **before**
  `torus_getUserTrades` / `torus_getTradeHistory*` return it, because the
  history rows are written just after.
- **At most once, per connection:**
  - Blocks the node executes while replaying at startup are not streamed.
  - After a reconnect, backfill the gap from the last `blockNumber` you saw
    with the history RPCs.
  - Dedupe merged rows by `blockNumber` + `tradeId` (+ `role` for `userFills`).
- **Stream-only fields:** `orderId`, `startPosition`, `closedPnl` and `dir` are
  not yet in `torus_getUserTrades` rows. A backfilled row lacks them.

## Slow subscribers

- **Buffer:** the node buffers the last 256 blocks per subscription.
- **What happens when a subscriber falls behind:** the subscription is
  **closed with an error**, sent as a final frame:

  ```json
  {"jsonrpc":"2.0","method":"torus_subscription","params":{"subscription":"<id>","error":"subscriber lagged: 3 blocks dropped; resubscribe and backfill with torus_getTradeHistoryRange / torus_getUserTrades"}}
  ```

- **Client action:** resubscribe, then backfill from the last `blockNumber` you
  processed.
- **Rust clients:** jsonrpsee's Rust client drops this error text and simply
  ends the stream (`next()` returns `None`). Treat the end of the stream as
  "lagged or closed, backfill".

## `l2Book`

`l2Book` serves exactly what `torus_getOrderBook` serves: the order book the
node has written to its database, read with the same code. Each message is
`torus_getOrderBook(marketId, nLevels)`'s answer at one block height.

```json
{"jsonrpc":"2.0","id":1,"method":"torus_subscribe","params":["l2Book",{"marketId":"0x1","nLevels":20}]}
```

### Messages

The first message is a full snapshot at the node's current height. After it,
the node sends a full snapshot again after every block that changed the best
`nLevels` levels of either side. A block that leaves them unchanged sends
nothing.

```json
{"jsonrpc":"2.0","method":"torus_subscription","params":{"subscription":"<id>","result":
  {"marketId":"0x1","blockNumber":"0x2a","seq":"0x0",
   "levels":[[{"px":"99.00000000","sz":"3.00000000","n":2}],
             [{"px":"101.00000000","sz":"1.50000000","n":1}]]}}}
```

| Field | Meaning |
|---|---|
| `marketId` | The market (hex). |
| `blockNumber` | The height the snapshot was read at (hex). It is the node's executed height, the same height `eth_blockNumber`'s eth head is taken from (see [eth-head.md](eth-head.md)). |
| `seq` | This subscription's message number (hex): `0x0` for the first snapshot, then +1 per message. |
| `levels` | `[bids, asks]`, best first (bids highest price first, asks lowest first), at most `nLevels` each. |
| `px`, `sz`, `n` | Level price, total size and order count: `torus_getOrderBook`'s `price`, `quantity`, `orderCount`. |

The shape is Hyperliquid's `l2Book` (`WsBook`: `coin` -> `marketId`, `time` ->
`blockNumber`, plus `seq`), with this node's encodings: decimal strings for
prices and sizes, hex for ids and heights.

### Gaps and consistency

- Every message is a complete snapshot: apply it by replacing your book. You
  never need an earlier message to use a later one.
- `blockNumber` strictly increases within a subscription. It skips heights
  that did not change the levels.
- `seq` increases by exactly 1 per message. A missing number means you lost a
  message; that cannot happen on a live subscription, because a subscriber
  that falls behind is closed instead (see below). After a reconnect the new
  subscription starts at `seq` `0x0` with a full snapshot.
- A snapshot and its `blockNumber` come from one database snapshot, so the
  book is never mixed between two heights.
- The stream and `torus_getOrderBook` read the same data, so polling the
  method at the same height returns the same levels.

### When updates arrive

The node publishes a book when its database moves to a new height. That
depends on the node:

- **`--rpc-only` nodes** write every executed block (checkpoint interval 0),
  so the stream updates after each block. Use one of these for a trading UI.
- **Validators** with checkpoints on (the default, 15 s) write executed state
  once per checkpoint. Their `torus_getOrderBook` lags by up to one interval,
  and their `l2Book` updates only at checkpoints, with the book at the
  checkpoint's height. With the L3.1 coalesced book save and L6.1
  window-committed books, the books on disk are also only complete at
  checkpoints; the stream never reads the books in memory, so it never shows a
  book that is not on disk.

The node checks the height every 25 ms, so it may merge several fast blocks
into one message (with the last height).

### Cost

- **Execution:** none. The stream never touches the execution thread; it
  reads back what the node already wrote.
- **No subscriber:** nothing. A publisher task runs only while at least one
  `l2Book` subscription exists, and stops when the last one ends.
- **With subscribers:** every 25 ms one point read (the height). When the
  height moved, one read of each subscribed market's levels (the work of one
  `torus_getOrderBook` call per market, 100 levels deep), shared by all its
  subscribers. Each distinct `nLevels` of a market is serialized once per
  update.
- Reads are bounded like `torus_getOrderBook` (`TORUS_RPC_MAX_SCAN_ROWS`). A
  market whose read fails closes its subscriptions with the error; a
  subscription whose first read fails is rejected.

### Slow subscribers

The node keeps the last 64 updates. A subscriber that falls further behind
is closed with a final error frame:

```json
{"jsonrpc":"2.0","method":"torus_subscription","params":{"subscription":"<id>","error":"subscriber lagged: 3 book updates dropped; resubscribe (the first message is a full snapshot)"}}
```

Resubscribe: the first message is a full snapshot.

## `orderUpdates`

`orderUpdates` sends every state change of one address's orders, as the
blocks that cause them execute.

```json
{"jsonrpc":"2.0","id":1,"method":"torus_subscribe","params":["orderUpdates",{"user":"0xaaaa…aaaa"}]}
```

### Messages

One message per executed block that changed any of the user's orders: an
array of rows, one per transition, in execution order within a market
(markets in ascending id; then placements refused before the book).

```json
{"jsonrpc":"2.0","method":"torus_subscription","params":{"subscription":"<id>","result":[
  {"orderId":"0x7","clientOrderId":"0x00000000000000000000000000000107","marketId":"0x1",
   "side":"sell","price":"100.00000000","originalQty":"5.00000000","remainingQty":"3.00000000",
   "filledQty":"2.00000000","reduceOnly":false,"status":"partiallyFilled",
   "blockNumber":"0x7","timestamp":"0x6553f107"}]}}
```

The order fields are `torus_getOpenOrders`' names and encodings.

| Field | Meaning |
|---|---|
| `orderId` | The order id (hex). `null` for a placement refused before the book assigned one. |
| `clientOrderId` | The order's cloid, or `null`. |
| `price` | Limit price; for a market or stop-market order its price cap; for a stop-limit its limit. |
| `triggerPrice` | Stops only: the trigger price. Absent otherwise. |
| `originalQty` | The order's size. |
| `remainingQty` | Size still live after this transition (resting, or pending for a stop). 0 for `filled` and `rejected`. For `canceled`, the size that was canceled. |
| `filledQty` | Size this transition filled (0 unless `partiallyFilled` / `filled`). |
| `status` | See below. |
| `reason` | `canceled`, `modified` and `rejected` only (absent otherwise), see below. |
| `blockNumber`, `timestamp` | The executed block (hex). |

| `status` | When |
|---|---|
| `open` | Placed and resting, nothing filled at placement. |
| `triggerPending` | A stop order stored, waiting for its trigger. |
| `partiallyFilled` | Part of the order filled; `remainingQty` rests (or, for an order that never rests, is canceled in the next row). |
| `filled` | The rest of the order filled. |
| `canceled` | Removed with `remainingQty` unfilled. |
| `triggered` | A stop's trigger fired. Its order is placed next under the same `orderId`, and its rows follow. |
| `modified` | A resting order's size or price changed; `price` and `remainingQty` are the new values. |
| `rejected` | Refused; it never rested. |

| `reason` | With |
|---|---|
| `user` | `canceled` / `modified`: the owner's cancel, cancel-all, cancel by cloid or modify. |
| `self_trade` | `canceled`: self-trade prevention (the owner's own order crossed it). |
| `reduce_only` | `canceled` / `modified`: a reduce-only order that can no longer reduce the position (cut or shrunk). |
| `margin` | `canceled`: the maker's account could not afford the fill (Hyperliquid `marginCanceled`). |
| `liquidation` | `canceled`: the owner was liquidated. |
| `delisted` | `canceled`: the market was delisted (or its delist settlement ran). |
| `market_params` | `canceled`: a governance update changed the market's tick, lot or margin parameters, which cancels its orders. |
| `unfilled` | `canceled`: the unfilled rest of an order that never rests (IOC, market, FOK), or of a taker cut by its margin or by a reduce-only clamp. |
| a failure name | `rejected`: the action status reason (`tick`, `lot`, `price`, `price_band`, `margin`, `open_limit`, `reduce_only`, `ioc_cancel`, `bad_alo_px`, `market_no_liquidity`, `fok_cancel`, `bad_trigger_px`, `duplicate_cloid`, `unknown_market`, `market_delisted`, `other`), as `FailureReason::as_str`. |

A taker shows its own outcome once per placement (`filled`; `partiallyFilled`
and resting; `open`; or `partiallyFilled` then `canceled` / `unfilled`). The
makers it hit each show their fill (`partiallyFilled` or `filled`) in the same
block. Fills themselves (price, role, PnL) are on `userFills`.

### Consistency with the RPC

- The rows come from execution, like `userFills`: the order book logs what it
  does to each order while a subscriber exists. They are final (blocks are
  final once committed).
- After a block's rows, `torus_getOpenOrders` returns the same open orders
  once the node has written that block (at once on `--rpc-only` nodes, at the
  next checkpoint on validators). The stream can run ahead of the method,
  never behind.
- To start: subscribe first, then read `torus_getOpenOrders` and apply the
  rows with a higher `blockNumber` than the state you read. Rows for orders
  you did not see are new orders.
- **Only while subscribed.** Execution records order events only while at
  least one `orderUpdates` subscriber exists, checked once per block, so the
  block executing at the moment you subscribe may be missed. After a
  reconnect, resync with `torus_getOpenOrders`.
- Blocks the node executes while replaying at startup are not streamed.

### Cost

- **No subscriber:** each order book mutation checks one flag (the log is
  off) and each executed block sets the flag on every book (O(markets)). No
  event is built.
- **With a subscriber:** each order book mutation also appends one event per
  order it touched (a fill touches two), and each block's events are moved to
  the subscription tasks in one message. Filtering by user and JSON happen in
  the subscription tasks, off the execution thread. The events never change
  state: books, roots and the running state hash are byte-identical with and
  without subscribers (tested).

### Slow subscribers

Like the trade streams: the node buffers 256 blocks; a subscriber further
behind is closed with

```json
{"jsonrpc":"2.0","method":"torus_subscription","params":{"subscription":"<id>","error":"subscriber lagged: 3 blocks dropped; resubscribe and resync with torus_getOpenOrders"}}
```
