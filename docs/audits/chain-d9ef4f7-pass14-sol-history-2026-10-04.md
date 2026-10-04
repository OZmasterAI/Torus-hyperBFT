# Pass 14 — transaction receipts, history and explorer recovery

Reviewed 2026-10-04 against `merge/item6-c3-pf1` at
`d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd`, reading the separate source
checkout `/home/oz/projects/Torus-hyperBFT`. Only this report was written in
`/home/oz/projects/Torus-hyperBFT-audit-docs`. No applicable ancestor or
repository `AGENTS.md` was found. Cargo and rustc are absent from PATH; every
Rust test cited here was **read, not run**. No source/Git/Torus mutations,
installs, services, live-chain calls or key operations occurred. Read-only
Torus status identified the newer pass-13 documentation checkpoint; its source
and documentation separation agrees with the current task.

**Result: no additional finding proposed.** This pass follows ordinary signed
transactions and funded fills through executor-to-receipt alignment, Ethereum
and body indices, skipped/reverted explorer rows, native subscriptions,
persistent trade history, candles and pruning. [Audit policy](README.md),
September reports, October main reports through pass 13 and the detailed
[transaction](chain-d52a33f-pass9-sol-transactions-2026-10-04.md),
[history](chain-d52a33f-pass9-sol-history-2026-10-04.md),
[RPC](chain-cea1254-pass7-sol-rpc-2026-10-04.md) and
[pass-13 interfaces](chain-d9ef4f7-pass13-sol-interfaces-2026-10-04.md)
reports informed deduplication. F01–F46 and historical supplements retain
those reports' qualifications. Interrupted pass-5 certificate, malformed-input
and adversarial networking assignments were not resumed.

## Hash identity and skipped/reverted transitions

The EVM executor [records included input indices][included], while skipped
transaction errors revert the journal and produce no receipt. The bridge
[aligns each surviving receipt][align] with its decoded transaction's original
body index and signed hash. The committer [indexes only receipt-bearing
transactions][location]. Thus an ordinary skipped future-nonce transaction
cannot overwrite an earlier executed hash's durable location merely by
reappearing in a later body. Reverts are executed transactions, retain receipts
and consume their Ethereum list positions.

Ethereum [hash lookups][lookup] translate the stored body position into a dense
index by counting preceding receipts; [receipt logs][receiptlogs] add preceding
receipt log counts. [getLogs][logs] separately counts nonmatching logs before
applying its filter, preserving block-global log indices. No new arithmetic
misalignment was established. Explorer deliberately [stores original body
positions][bodypos], so its row index need not equal the Ethereum dense index.

A specific candidate was falsified: a skipped row does **not** permanently
hide the same signed transaction if it executes later. [insert_transaction][upsert]
uses `IGNORE` for skipped outcomes and `REPLACE` for success or executed failure.
An ordinary bounded scenario is a funded sender's nonce N+1 transaction being
selected before N, skipped, then resubmitted after N executes. Admission
[allows bounded future nonces][nonce]; this scenario does not need malformed
bytes, changed signatures or a failed DB. The later execution replaces the
explorer's earlier skipped row, while a later skipped replay cannot demote an
executed row. The global hash primary key means this is a canonical outcome
projection, not a complete per-body occurrence table. No separate guarantee
of recording every repeated skipped occurrence was established.

The [Ethereum view test][ethtest] executes success/skipped/revert through the
bridge and asserts two receipts, a status-zero reverted receipt, dense index
one for the reverted transaction, null skipped hash/receipt results, complete
body hashes/statuses, and invisibility of the next unexecuted block. It manually
installs the applied marker and action-status record; it is not a full
application failure-atomicity regression. The [explorer mock-RPC test][explorertest]
asserts body indices 0/1/2 with success/skipped/failed, executed count two, and
an empty later block transaction list for a skipped replay of a prior success.
It does not assert the reverse skipped-first transition, though production
upsert logic directly supplies the counterexample. A useful missing regression
would index skipped X, then actually execute X at a later height and require its
new location, receipt status and block membership; follow with another skipped
replay and require the executed projection to remain stable.

## Fills, persistence and native subscriptions

The application [shares one BlockFills Arc][fillhandoff] between stream sink
and history encoder. Packed rows [preserve trade indices][encode] via stable
market/address grouping. Public [stream trades][streamtrades] and
[block-history trades][blocktrades] both project the fill's trade index, price,
quantity, taker side, block and timestamp. Block-wide history visits markets
in market-key order, while the live stream follows global trade-index order.
Within each market they agree; candles aggregate independently per market.
This is a counterexample to calling cross-market ordering a candle OHLC bug.
Trade IDs restart each block: comparison/recovery must retain block number.

The [application sink fixture][sinktest] runs serial and pipelined execution,
asserts fill summaries `(3,1,0), (6,1,0), (11,1,0)`, compares exact
`(height,timestamp,market,MarketTrade)` tuples between sink and stored rows,
and checks extras count/order IDs. Adjacent fixtures assert history-off streams
have identical fills/extras but empty history CFs, and adding a sink leaves all
CFs/root unchanged. These assertions refute a blanket claim that subscription
activity changes fill settlement or that history-off stops active streams.
They compare after fixture teardown; they do not establish simultaneous RPC
visibility during writer delay.

Native [subscription lifecycle][subscribe] subscribes before accepting,
selects sink closure while waiting, and releases its RAII slot. The
[quiet cleanup test][quiettest] unsubscribes with no published block, verifies
2→1 active slots, closes the connection and requires zero. The
[lag test][lagtest] overflows a two-block synthetic buffer with five sends,
asserts the explicit three-block-drop terminal error, zero remaining slots,
and no post-close frame. The [jsonrpsee client test][clientlag] instead asserts
stream end: that client removes the subscription without surfacing the close
error text. This narrows the delivery contract; it does not establish a fresh
lost-fill defect. These WS fixtures send the notifier directly. The application
fixture provides complementary producer wiring, not a single end-to-end
execution/disconnection/backfill test.

## Existing recovery boundaries remain open

**F26:** [same-hash early return][skipblock], [body-error swallowing][bodypos],
[best-effort trade indexing and cursor advance][cursor] remain. Normal writer
delay alone can expose an empty/partial successful history response before
its rows land, which explorer permanently accepts. The applied state marker
certifies execution, not history completion. [Chunked writes][writer] deliberately
permit partial visibility; eventual sink/history equality does not disprove
this timing consequence. A paused-writer regression must run the actual
indexer, release the writer, retry/later-head-index, and require exact once-only
OHLCV/count recovery. This is the existing F26 supplement, not another count.

Clean missed intervals are reconciled [on subsequent heads][reconcile]. Startup
backfill followed by subscription still lacks an independent periodic retry
for a quiet tail; that historical limitation is not rediscovered here.
The decimal candle [test][candletest] rejects legacy hex prices/quantities,
then asserts timestamp bucket 60, OHLC raw 10,000,000,000 and volume/count
250,000,000/1 for a valid decimal fill. It does not cover writer timing.

**F40:** capped user history still lacks a continuation cursor. The latest-1,000
contract cannot recover an older same-market own-role fill after a 1,001-entry
gap. Public uncapped block trades remain a market-data fallback, with no
stored maker/taker identity, and do not close user-role recovery. Existing
small ordering/filter/side tests should not be represented as cap recovery.

**F31/F38:** [pruning][prune] removes bodies, status and receipts, retaining
headers and trade-history CFs. Consequently an old stored trade can remain
available while its Ethereum receipt is explicitly pruned; this is not proof
of inconsistent transaction execution. The [pruner test][prunetest] asserts
bodies/receipts absent below five, present from five, and all headers retained.
Its current-frontier fixture does not close replay-safety or archive-restart
frontier initialization. No new pruning defect was established.

The remaining tests proposed here target seams beyond existing fixtures;
none was implemented, executed or represented as passing. Existing roots
remain candidates awaiting production-code regressions, with no verified fix
or runtime incident claimed by this review.

[included]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-evm/src/executor.rs#L298
[align]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-bridge/src/validator.rs#L487
[location]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-bridge/src/committer.rs#L186
[lookup]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-rpc/src/eth.rs#L724
[receiptlogs]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-rpc/src/eth.rs#L822
[logs]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-rpc/src/eth.rs#L1107
[bodypos]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-explorer/src/indexer.rs#L90
[upsert]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-explorer/src/db.rs#L358
[nonce]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-mempool/src/validate.rs#L102
[ethtest]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-rpc/src/lib.rs#L3200
[explorertest]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-explorer/src/indexer.rs#L525
[fillhandoff]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-consensus/src/app.rs#L2598
[encode]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-state/src/trade_rows.rs#L165
[streamtrades]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-rpc/src/streams.rs#L126
[blocktrades]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-rpc/src/lib.rs#L768
[sinktest]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-consensus/src/app.rs#L13496
[subscribe]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-rpc/src/torus.rs#L2016
[quiettest]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-rpc/src/streams.rs#L945
[lagtest]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-rpc/src/streams.rs#L881
[clientlag]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-rpc/src/streams.rs#L918
[skipblock]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-explorer/src/indexer.rs#L71
[cursor]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-explorer/src/indexer.rs#L145
[writer]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-state/src/bg_writer.rs#L313
[reconcile]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-explorer/src/indexer.rs#L239
[candletest]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-explorer/src/indexer.rs#L625
[prune]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-state/src/pruner.rs#L207
[prunetest]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-state/src/pruner.rs#L350
