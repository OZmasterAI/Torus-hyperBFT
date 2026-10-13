# Read limits on public RPCs (R24)

A response size cap (`TORUS_RPC_MAX_RESPONSE_MB`) does not limit the work a read
does before it answers. The read methods below either seek only the rows they
answer from, or stop after a fixed number of rows. They never scan a whole
table first and then refuse the reply.

## `TORUS_RPC_MAX_SCAN_ROWS`

The maximum number of rows one request may read. The default is `250000`. An
unset, empty, unparsable or `0` value uses the default. It is read once at
startup. `RpcServer::set_max_scan_rows` overrides it in embedded use.

A row is one key that the RocksDB iterator returns, or one seek. A row counts
even when the read filters it out, because reading it costs the same. A classic
whole-book blob (`TORUS_BOOK_ROWS` unset) counts as one row per started 128
bytes (about one order). It is charged before it is decoded, because it cannot
be read in part.

A read that would go past the limit stops and returns:

```json
{"code": -32005, "message": "query would read more than 250000 rows (TORUS_RPC_MAX_SCAN_ROWS); narrow it (e.g. pass a marketId) or use a node with a higher limit"}
```

`-32005` is the code already used for `TooManyResults`, `BlockRangeTooLarge`
and the per-IP rate limit. The read returns no partial result, so a reply is
never a silently short list.

## Per method

| Method | Rows read |
|---|---|
| `torus_getOrderBook(marketId, depth?)` | Returns `depth` levels per side, best first. The default and maximum are `1000`. Level rows (`TORUS_BOOK_ROWS=2/3`): one seek per side, at most `depth` rows each. Order rows (`=1`): every order of the market. Classic: the market's blob. |
| `torus_getOpenOrders(trader, marketId?)` | With `marketId`: that market's orders only. Without it: markets in id order, one seek per market. Stops at 5000 orders or the scan limit. |
| `torus_getOrderByCloid(trader, cloid)` | Stops at the first hit. Pending stops are read first (row layouts), then resting orders. A miss reads every order. |
| `torus_getUserLimits(trader)` | Every order row and stop row. There is no per-trader index. |
| `torus_getOpenInterest(marketId)` | Every position row. Positions are keyed `trader ‖ market`, with no market index. |
| `torus_getProposals(status?)` | Every proposal row. |
| `torus_getUserTrades(trader, marketId?, limit?)` | The trader's rows, newest first, until `limit` fills are found. A `marketId` filter skips rows, and those skipped rows count toward the limit. |

`torus_getTradeHistory`, `torus_getTradeHistoryRange`, `torus_getBlockTrades`,
`torus_getMarkets`, `eth_getLogs` (a 10,000-block range and 10,000 logs) and
`eth_feeHistory` (1024 blocks) were already bounded by their own limits.

## Per-IP weight

`torus_getOpenInterest` and `torus_getUserLimits` have weight 20 (scans), not
2 (point reads), in the per-IP limiter (`TORUS_RPC_IP_WEIGHT_PER_MIN`).
`torus_getOrderBook` stays at 2, because its depth bounds it.

## Follow-ups (state indexes)

These reads stay proportional to the table until an index exists:

- `torus_getOpenInterest`: a per-market open-interest aggregate, or a
  market-to-holders index, maintained with every position write.
- `torus_getUserLimits`, `torus_getOpenOrders` (all markets),
  `torus_getOrderByCloid`: a persisted node-local index from trader to orders
  and from trader and cloid to order. The executor keeps both in memory only.
- `torus_getOrderBook` under order rows (`TORUS_BOOK_ROWS=1`): a price index.
  Level rows (`=2/3`) already provide one.
