# Open-order limit and `torus_getUserLimits`

Each address may hold a limited number of open orders, summed over all
markets (Hyperliquid model):

    limit = min(1000 + floor(cumVolume / 5,000,000), 5000)

- `cumVolume` is the address's lifetime traded notional (`price * qty`, quote
  units). Maker and taker both add it on every fill.
- The count and the limit are taken when each execution batch starts. A block
  runs two batches (before and after its EVM transactions), so fills raise the
  limit from the next batch on, which can be later in the same block.
- Open orders are resting orders (GTC, PostOnly) and pending stops
  (StopMarket, StopLimit). Market, IOC and FOK orders never rest, so they are
  never counted or rejected by the limit.
- A restable order past the limit is rejected with an error starting with
  `open order limit`. Within one batch, every accepted restable order uses a
  slot until the batch ends, even if it fills completely or the book then
  rejects it (PostOnly would cross, dust quantity, off-tick price): the check
  runs before matching, so it is conservative.
- With 1000 or more open orders, reduce-only and stop orders are rejected even
  when the volume-scaled limit has room.
- `CancelAllOrders` also cancels the sender's pending stops.

Rejected orders are counted in the metric `torus_orders_rejected_open_limit_total`.

## `torus_getUserLimits`

```json
{"jsonrpc":"2.0","id":1,"method":"torus_getUserLimits","params":["0xaaaa…aaaa"]}
```

Reply:

```json
{"openOrders": 5, "openOrderLimit": 1000, "cumVolume": "200.00000000"}
```

| Field | Type | Meaning |
|---|---|---|
| `openOrders` | number | Resting orders plus pending stops, all markets. |
| `openOrderLimit` | number | The formula above for the stored `cumVolume`. |
| `cumVolume` | decimal string | Lifetime traded notional, same format as `torus_getBalances` amounts. |

`torus_getOpenOrders` returns up to 5000 resting orders (pending stops are not
listed there).
