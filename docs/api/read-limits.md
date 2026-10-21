# Read limits on public RPCs (R24)

A response size cap (`TORUS_RPC_MAX_RESPONSE_MB`) does not limit the work a read
does before it answers. The read methods below either seek only the rows they
answer from, or stop after a fixed number of rows. They never scan a whole
table first and then refuse the reply.

## `TORUS_RPC_MAX_SCAN_ROWS`

The maximum number of rows one request may read. The default is `250000`. An
unset, empty, unparsable or `0` value uses the default. It is read once at
startup. `RpcServer::set_max_scan_rows` overrides it in embedded use.

A row is one key that the RocksDB iterator returns, or one seek or point read
the method makes per item it walks (a market row and an oracle aggregate per
position in `torus_getBalances`). A row counts even when the read filters it
out or skips it, because reading it costs the same. A value that packs many
records under one key counts as one row per started 128 bytes, charged before
it is decoded, because it cannot be read in part. Four kinds of value are
charged this way: a classic whole-book blob (`TORUS_BOOK_ROWS` unset), a
packed user-trade row (all of a trader's fills in one block), a market trade
row (up to 1024 fills of one market in one block, `torus_getTradeHistory`,
`torus_getTradeHistoryRange`, `torus_getBlockTrades`), and a governance
proposal row (`torus_getProposal`, `torus_getProposals`; the row itself is one
of its 128-byte rows). A fixed number of point reads that does not grow with
the data (a balance, a header, a meta row) is not counted.

Under the limit, every method returns what it returned before R24, including
the same error for a row that does not decode (see "Same answer as before"
below).

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

Every public read method registered by `crates/torus-rpc` (`torus_*`,
`eth_*`, `net_*`, `web3_*`), and what bounds the database work it does before
it answers. "Answer" means the work is proportional to the reply (each row
read is in the reply). "Constant" means a fixed number of reads. "Budget"
means the method counts rows toward `TORUS_RPC_MAX_SCAN_ROWS` and stops with
`-32005`. Tests are in `crates/torus-rpc/src/scan_tests.rs` unless noted.

| Method | How its DB work is bounded | Test |
|---|---|---|
| `torus_getOrderBook(marketId, depth?)` | Budget. Row layouts: every row of the market (meta, order, stop and level rows), from one RocksDB snapshot, validated as `book_reader::read_book_depth` validates them, so an unknown key or a corrupt level row anywhere in the market is an error, not skipped. Mode 1 has no price index; mode 2 reads every level so it can validate them. Classic: the market's blob, by size. `depth` (default and maximum 1000) bounds the reply. | `order_book_depth_bounds_the_levels_read`, `unknown_or_corrupt_market_rows_fail_as_main`, `level_book_sides_are_read_from_one_snapshot`, `a_corrupt_meta_row_is_an_error_not_an_empty_book` |
| `torus_getMarkPrice(marketId)` | Budget. Row layouts: the meta row. Classic: the blob, by size, before it is decoded. Plus the oracle aggregate (constant). | `classic_mark_price_charges_the_blob_before_decoding` |
| `torus_getOpenOrders(trader, marketId?)` | Budget. With `marketId`: every row of that market is validated (as main), then its orders (mode 2: the node-local store rows of the market). Without it: one seek per market into its order rows (mode 1), or one seek per market for a level row plus the whole node-local store (mode 2). Every order row read is decoded and checked, whoever's it is, and all are read before the reply is cut to 5000. Classic: blobs by size, stopping at 5000 orders (as main). | `open_orders_of_one_market_read_only_that_market`, `open_orders_validate_rows_past_the_reply_limit`, `a_truncated_order_row_is_book_corrupt_not_skipped`, `classic_open_orders_report_main_decode_error` |
| `torus_getOrderByCloid(trader, cloid)` | Budget. Pending stops first (one seek per market); a stop hit returns at once. A resting hit, or a miss, reads and validates every resting order, as main does. Classic: one pass over the blobs. | `order_by_cloid_stop_hits_return_early_resting_hits_read_every_order`, `order_by_cloid_validates_every_order_before_answering` |
| `torus_getUserLimits(trader)` | Budget. Every order row of the order store (only the trader's own are decoded, as main), then each market's stop rows (one seek per market). Classic: every blob, by size. | `user_limits_stop_at_the_scan_budget`, `user_limits_count_as_main_did` |
| `torus_getOpenInterest(marketId)` | Budget. Every position row (keyed `trader ‖ market`, no market index). | `open_interest_stops_at_the_scan_budget` |
| `torus_getBalances(trader)` | Budget. Each of the trader's position rows, and per market held its market row and (with an executed head) its oracle aggregate. The balance, EVM account, header and stake reads are constant. | `balances_and_liquidator_vault_stop_at_the_scan_budget` |
| `torus_getLiquidatorVault` | Budget. Each of the vault's position rows; the balance read is constant. | `balances_and_liquidator_vault_stop_at_the_scan_budget` |
| `torus_getPosition(trader, marketId)` | Budget for a cross position: its `liquidationPrice` (R23) values the whole account, so each of the trader's position rows, and per market held its market row and (with an executed head) its oracle aggregate, are charged as in `torus_getBalances`. The position row, its oracle aggregate and the header read are constant; an isolated position reads nothing more. | `cross_position_liquidation_price_stops_at_the_scan_budget` |
| `torus_getMarkets(offset?, limit?, after?)` | Budget. With `after` (a market id as the reply prints it; s121) the page starts at the first market above it, sought directly: a page reads only its own rows (at most 500), so listing every market reads each row once. With `offset` every market row up to `offset + limit` is read and counted. `offset` above 0 with `after` is `-32602`. | `markets_offset_is_charged_to_the_scan_budget`, `markets_after_cursor_seeks_to_the_page` |
| `torus_getTradeHistory(marketId, limit?)` | Budget, and `limit` (at most 1000 fills). Every trade row visited, newest first, by size, before it is decoded or skipped: rows the decoder skips (old format) do not count toward `limit`. | `trade_history_charges_every_row_visited` |
| `torus_getTradeHistoryRange(marketId, from, to, limit?)` | Budget, and `limit` (at most 5000 fills). As `torus_getTradeHistory`, over `[from, to]`. | `trade_history_charges_every_row_visited` |
| `torus_getBlockTrades(blockNumber)` | Budget. Every market row (one seek per market into the block's trade rows), even for a block with no trades, plus the block's trade rows by size. | `block_trades_are_charged_per_market_to_the_scan_budget` |
| `torus_getUserTrades(trader, marketId?, limit?)` | Budget, and `limit`. The trader's rows, newest first, each by size; a `marketId` filter skips rows, and those count. | `user_trades_with_a_market_filter_stop_at_the_scan_budget`, `packed_user_trade_rows_are_charged_by_size_before_decoding` |
| `torus_getProposal(id)` | Budget. One row, by size, before it is decoded (the reply leaves out the execution payload; the decode does not). | `get_proposal_charges_the_row_by_size_before_decoding` |
| `torus_getProposals(status?)` | Budget. Every proposal row, by size, before it is decoded (a status filter does not make a row cheaper). | `proposals_stop_at_the_scan_budget`, `proposal_rows_are_charged_by_size_before_decoding`, `proposals_report_main_decode_errors` |
| `torus_getLeader` | Budget. Every validator row (main decodes them all before it looks the leader's key up). | `leader_lookup_stops_at_the_scan_budget` |
| `torus_getValidators` | Answer: every validator row is in the reply (read twice). Each validator's state is found by one lookup in a map by address (was a scan of every state per validator, quadratic CPU); the first row with an address wins, as before. | `validators_reply_is_unchanged_by_the_address_map` |
| `torus_getStakingInfo(address)` | Answer: the address's delegations (all in the reply), plus two point reads. | none needed |
| `torus_getDelegations(delegator)` | Answer: the delegator's delegations. | none needed |
| `torus_getStateHash(height?)` | Constant: the retained checkpoint keys (at most 64), then that height's votes (in the reply, at most one per validator). | none needed |
| `torus_getEpoch`, `torus_getGovernanceParams`, `torus_getTreasuryInfo` | Constant: none, or a few point reads. | none needed |
| `torus_getFeatureSchedule` | Constant: no state read (the schedule and the known features are in memory; at most 128 features). | none needed |
| `torus_getBlockBody(blockNumber)` | Answer: one body, its status record, and one link per CoreWriter row in it. | none needed |
| `torus_subscribe` | No DB reads: items are pushed from execution. | none needed |
| `eth_chainId`, `eth_blockNumber`, `eth_gasPrice`, `eth_maxPriorityFeePerGas` | Constant: none, or one header. | none needed |
| `eth_getBalance`, `eth_getCode`, `eth_getStorageAt`, `eth_getTransactionCount` | Constant: account, code and storage point reads at the head. | none needed |
| `eth_getTransactionByHash`, `eth_getTransactionReceipt` | Budget. The hash index, one body and the tx's receipt are constant; its eth index (and the receipt's log offset) reads one receipt slot per earlier body position, each charged as one row. | `block_lookups_charge_every_receipt_slot_to_the_scan_budget` (receipt) |
| `eth_getBlockByNumber`, `eth_getBlockByHash`, `eth_getBlockTransactionCountByNumber`, `eth_getBlockTransactionCountByHash`, `eth_getTransactionByBlockNumberAndIndex`, `eth_getTransactionByBlockHashAndIndex` | Budget. The header and body are constant; the receipt lookup reads one slot per submitted transaction of the block, undecodable ones included (the gas limit does not bound them), and each slot read is one row, charged before it is made. A by-index lookup still reads every slot (as before) before it answers. | `block_lookups_charge_every_receipt_slot_to_the_scan_budget` |
| `eth_call`, `eth_estimateGas` | The gas limit bounds the EVM's work (state point reads). It is the head header's limit of the caller's lane (item 7 step 2): `evm_gas_limit` (3M at genesis; item 7 step 1), or `evm_big_gas_limit` (10M at genesis) while big blocks are on (`evm_big_block_interval` > 0) and the caller (`from`) has opted into the big lane; that lane row is one point read from the ingress view. A call without `gas` gets that limit, a larger `gas` is clamped to it, and `eth_estimateGas` searches up to it. Before block 1 (no header) it is 30M. Reader precompiles also meter their own work in gas (`docs/perf/read-precompile-gas.md`); the best bid / ask reader `bbo(bytes32)` at 0x080e (item 7 step 2) is one point read for 16,440 gas. | `call_gas_defaults_and_caps_at_the_block_limit` (`eth.rs`), `bare_call_and_estimate_work_under_a_3m_header`, `eth_call_gas_above_the_header_limit_is_capped`, `estimate_gas_caps_at_the_callers_lane_limit`, `estimate_gas_reads_the_callers_lane_from_the_ingress_view`, `eth_call_gas_cap_reads_the_callers_lane_from_the_ingress_view` (`tests/eth_compliance_tests.rs`) |
| `eth_getLogs` | Budget, plus its own limits: a range of at most 10,000 blocks and at most 10,000 logs. Every receipt slot of every block in the range with EVM transactions is read and charged as one row, even when the filter keeps none of its logs (s121); blocks without EVM transactions read no receipt. | `get_logs_charges_every_receipt_slot_to_the_scan_budget`, `lib.rs` getLogs tests |
| `eth_feeHistory` | Its own limit: at most 1024 headers. The optional 4th parameter `blockType` (`"small"` default, `"big"`; item 7 step 2) only picks which base fee each header reports. | `big_block_gas_price_and_fee_history_per_lane` (`tests/eth_compliance_tests.rs`) |
| `torus_bigBlockGasPrice`, `torus_usingBigBlocks(address)` | Constant: one header, or one lane-row point read through the ingress view (item 7 step 2). | `big_block_gas_price_and_fee_history_per_lane`, `using_big_blocks_reads_the_lane_row` (`tests/eth_compliance_tests.rs`) |
| `eth_subscribe` | No DB reads: items are pushed from execution. | none needed |
| `net_version`, `net_listening`, `net_peerCount`, `web3_clientVersion`, `web3_sha3` | No DB reads. | none needed |

`torus_submitNativeAction`, `torus_submitNativeActions`,
`torus_submitNativeActionsBin` and `eth_sendRawTransaction` are writes, not
reads; their ingress checks read a fixed number of rows per action. Since item
7 step 1 the EVM pool refuses a transaction above 256 KiB of raw bytes
(`TxTooLarge`) or with a gas limit above the `evm_gas_limit` of the last
committed header handed to execution (or `TORUS_EVM_BLOCK_GAS_BUDGET`, if
lower). Since item 7 step 2 these are the small lane's caps; a sender in the
big lane (big blocks on and its `EvmUserModify` row set, one point read)
gets 768 KiB and the header's `evm_big_gas_limit` instead, and the budget
never applies to it. A transaction with bytes after it is refused before
the sender is recovered (`tx decode failed: trailing bytes after the tx`).

## Same answer as before

Under the limit, every method returns what it returned before R24, and the
same error for a row that does not decode. Reads that stop early stop only
where main stopped early (a pending stop hit in `torus_getOrderByCloid`, the
5000th order of a classic `torus_getOpenOrders`, `limit` fills of trade
history); everywhere main read and validated every row, the bounded read
does too, and the reply is cut afterwards. One difference is intended: a
classic whole-book blob that does not decode was skipped by
`torus_getOpenOrders` without a `marketId`; it is now the same
`borsh decode order book` error as with a `marketId`.

## Per-IP weight

`torus_getOpenInterest` and `torus_getUserLimits` have weight 20 (scans), not
2 (point reads), in the per-IP limiter (`TORUS_RPC_IP_WEIGHT_PER_MIN`).
`torus_getOrderBook` stays at 2: it reads one market, as it did before R24,
and the scan limit bounds that market.

## Follow-ups

These reads stay proportional to a table (within the scan limit) until an
index exists, or have a known cost noted here:

- `torus_getOpenInterest`: a per-market open-interest aggregate, or a
  market-to-holders index, maintained with every position write.
- `torus_getUserLimits`, `torus_getOpenOrders` (all markets),
  `torus_getOrderByCloid`: a persisted node-local index from trader to orders
  and from trader and cloid to order. The executor keeps both in memory only.
- `torus_getOrderBook` under the row layouts: the read covers the whole market
  so that every key and level row is validated as before. Reading only the
  best `depth` levels (mode 2 has a price index; mode 1 would need one) means
  giving up that whole-market validation, an owner decision.
- `torus_getBlockTrades`: a per-block trade index (block to the markets that
  traded in it, written with the trade rows), so a block reads only its own
  trade rows instead of seeking every market.
- `torus_getBalances` / `torus_getLiquidatorVault` / cross `torus_getPosition` read one position per
  market held, so they stay proportional to the number of markets (listed by
  governance). No index is needed.
- Block `logsBloom`: the proposer writes a zero bloom in every header (the
  block is built before it executes), so `eth_getBlockByNumber` /
  `ByHash` report `logsBloom` 0x00.. even for blocks with logs, and
  `eth_getLogs` cannot skip blocks by bloom. A client or indexer that
  trusts block blooms would skip blocks that have logs. Not a read limit;
  listed here because it came up with the `eth_getLogs` budget (s121).
- Book layout detection on a DB without the `__book_mode__` marker: write the
  marker once, from a write path such as a startup migration or the next
  executor save, never from an RPC. After that, detection is one row.
