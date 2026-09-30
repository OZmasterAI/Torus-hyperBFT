# WebSocket trade streams

A node pushes fills to WebSocket clients through two JSON-RPC subscriptions:

- `newTrades`: public trades, per market or for all markets. All markets is
  off on validators by default (see [All-markets `newTrades`](#all-markets-newtrades)).
- `userFills`: the fills of one address, with its position effect.

Both are fed from execution (the fills each executed block produced), not read
back from the database. They work on nodes running with
`TORUS_TRADE_HISTORY=0`. On such nodes the history RPCs return nothing, but
the streams still deliver.

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

- The reply is a subscription id.
- `torus_unsubscribe` with that id stops the subscription.
- An unknown kind or a bad param is rejected with error `-32602` before any
  subscription is created.
- A node allows 1000 subscriptions at once and rejects more with `-32000`.

### All-markets `newTrades`

At full load one block's all-markets array can be tens of MB of JSON. So
`newTrades` without `marketId` is:

- **rejected on validators** by default, with `-32602`:
  `invalid params: newTrades without marketId is disabled on this node
  (validator); subscribe per market or use an RPC node; operators:
  TORUS_ALL_MARKET_TRADES=1`;
- **allowed on `--rpc-only` nodes** by default.

Per-market `newTrades` and `userFills` are allowed on every node.

Operators override the default with the node-local env var
`TORUS_ALL_MARKET_TRADES`:

| Value | All-markets `newTrades` |
|---|---|
| unset | allowed only on `--rpc-only` nodes |
| `1` | allowed |
| `0` | rejected (the message still says "validator") |
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
