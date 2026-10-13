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
even when the read filters it out, because reading it costs the same. A value
that packs many records under one key counts as one row per started 128 bytes
(about one order), charged before it is decoded, because it cannot be read in
part. Four kinds of value are charged this way: a classic whole-book blob
(`TORUS_BOOK_ROWS` unset), a packed user-trade row, which holds all of a
trader's fills in one block, a block trade row (up to 1024 fills of one market
in one block), and a governance proposal row (the row itself is one of its
128-byte rows).

## Book layout detection

Every book read (`torus_getOrderBook`, `torus_getOpenOrders`,
`torus_getOrderByCloid`, `torus_getUserLimits`, `torus_getMarkPrice`) first
finds the layout on disk, and that work counts toward the same limit. If the
`__book_mode__` marker row exists, reading it is one row. Every executor save
writes the marker. A DB without the marker has not been saved by a current
executor, so the read checks the shape of every key in the book column family
(one row each), then makes one seek in the node-local order store. On such a DB
a large book can make every book read return `-32005`. The RPC never writes the
marker: reads do not change state.

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
| `torus_getOrderBook(marketId, depth?)` | Returns `depth` levels per side, best first. The default and maximum are `1000`. Level rows (`TORUS_BOOK_ROWS=2/3`): one seek per side, at most `depth` rows each. Order rows (`=1`): every order of the market. Classic: the market's blob. Under the row layouts the meta row and both sides come from one RocksDB snapshot, so a book that is never crossed on disk is never returned crossed. A meta row that does not decode is an error, not an empty book. |
| `torus_getMarkPrice(marketId)` | Row layouts: the market's meta row. Classic: the market's blob, counted by size before it is decoded (the reply is one price, but the blob cannot be read in part). |
| `torus_getOpenOrders(trader, marketId?)` | With `marketId`: that market's orders only. Without it: markets in id order, one seek per market. Every order row read is decoded and checked before the trader filter, so a corrupt row of any trader is a `BookCorrupt` error, never skipped. Stops at 5000 orders or the scan limit. |
| `torus_getOrderByCloid(trader, cloid)` | Stops at the first hit. Pending stops are read first (row layouts), then resting orders. A miss reads every order. |
| `torus_getUserLimits(trader)` | Every order row and stop row. There is no per-trader index. |
| `torus_getOpenInterest(marketId)` | Every position row. Positions are keyed `trader ‖ market`, with no market index. |
| `torus_getProposals(status?)` | Every proposal row, each counted by size before it is decoded (a status filter does not make a row cheaper). |
| `torus_getMarkets(offset?, limit?)` | Every market row up to `offset + limit` (at most 500 per page): the rows before `offset` are read and counted, because the page start cannot be sought. |
| `torus_getBlockTrades(blockNumber)` | Every market row (one seek per market into the block's trade rows), even for a block with no trades, plus the block's trade rows by size. |
| `torus_getUserTrades(trader, marketId?, limit?)` | The trader's rows, newest first, until `limit` fills are found. Each row (one block's fills) counts by size, as above. A `marketId` filter skips rows, and those skipped rows count toward the limit. |

`torus_getTradeHistory`, `torus_getTradeHistoryRange`, `eth_getLogs` (a
10,000-block range and 10,000 logs) and `eth_feeHistory` (1024 blocks) were
already bounded by their own limits. `torus_getMarkets` and
`torus_getBlockTrades` were not: their response limits do not bound the market
rows they walk, so both count toward the scan limit (above).

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
- `torus_getMarkets` with a large `offset`: seekable pagination (a cursor that
  is a market id, seeked to directly) instead of skipping `offset` rows.
- `torus_getBlockTrades`: a per-block trade index (block to the markets that
  traded in it, written with the trade rows), so a block reads only its own
  trade rows instead of seeking every market.
- Book layout detection on a DB without the `__book_mode__` marker: write the
  marker once, from a write path such as a startup migration or the next
  executor save, never from an RPC. After that, detection is one row.
