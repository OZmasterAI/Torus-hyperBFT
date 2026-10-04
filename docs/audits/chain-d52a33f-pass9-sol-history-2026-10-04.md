# Pass 9 — ordinary trade history, native streams, and explorer projection

Reviewed 2026-10-04 in `/home/oz/projects/Torus-hyperBFT`, branch
`perf/item6-phase1`, HEAD `d52a33f51038105a8d0e102fc2e1a5c51f43fb09`.
This bounded review traces legitimate fills, persisted market/user history,
query ordering/filtering/limits, native subscriptions, and the explorer's
candle projection. No production source, Git history, service, or live chain
was changed. Cargo and rustc are absent from PATH; all Rust tests cited below
were **read, not run**. No toolchain or dependencies were installed. Parent
review owns Torus records and final finding numbering.

The [audit policy](README.md), September Astra rounds
[one](astra-round1-2026-09-24.md) and
[two](astra-round2-2026-09-24.md), and pass
[three](chain-cea1254-pass3-project-wide-2026-10-04.md),
[four](chain-cea1254-pass4-missed-issues-2026-10-04.md),
[six](chain-cea1254-pass6-correctness-review-2026-10-04.md),
[seven](chain-cea1254-pass7-mixed-review-2026-10-04.md), and
[eight](chain-d52a33f-pass8-mixed-review-2026-10-04.md) and their relevant
companions were used for deduplication. F01–F37, explorer envelope parsing,
descending OFFSET pagination, F26 incomplete indexing, F33 Ethereum log
publication/closure, and the old explorer quiet-tail/reconnect gap are not
new findings here. No applicable AGENTS.md was found in the checkout or its
ancestor directories.

## H01 / F40 — P2 candidate: documented user-fill backfill cannot continue beyond the newest 1,000 entries

**Source-established interface/control flow; runtime regression outstanding.**
The [delivery documentation](../api/streams.md#L133) tells a reconnecting
client to backfill from its last `blockNumber` using the history RPCs, and the
[lag error](../../crates/torus-rpc/src/torus.rs#L2041) explicitly directs it to
`torus_getTradeHistoryRange / torus_getUserTrades`.
However, [getUserTrades](../../crates/torus-rpc/src/torus.rs#L1913) accepts only
the user, optional market, and limit. It clamps that limit to 1,000,
starts at the user's newest row on every request, filters the market before
counting, and exits after the requested number of matches
([scan](../../crates/torus-rpc/src/torus.rs#L1943),
[limit exit](../../crates/torus-rpc/src/torus.rs#L1977)). There is no block bound,
offset, trade cursor, continuation token, or incomplete-result indication.

**Concrete valid preconditions.** Use a listed market and independently funded
maker/taker accounts with usable marks. Let one ordinary user accumulate 1,001
valid fills in that same market during a disconnected interval, over as many
normally committed blocks as necessary. Every order can be modest, tick/lot
aligned, and individually admissible; no large request, invalid signature,
self-trade, per-user resting-order overflow, or one-block throughput assumption
is needed. Leave history enabled, wait for the background writer to drain, and
leave the chain quiet before recovery. The user's 1,001 own-role entries remain
stored in [user rows](../../crates/torus-state/src/trade_rows.rs#L191).

With `limit=1000`, the reader returns only the newest 1,000 entries. A repeated
request returns the same set. A greater limit is clamped; selecting the same
market leaves the same set; selecting another market cannot expose the missing
entry. A lower limit returns a prefix of that set. Thus a previously unseen
older user fill is inaccessible through the advertised user recovery endpoint
even though its history row is intact. New activity can only push older blocks
farther out of reach. Within each newest block, user entries are intentionally
ascending by trade index, so the exact omitted member for a densely filled
single block depends on that documented ordering; the multi-block example
avoids relying on any contrary newest-within-block assumption.

This is a completeness/recovery gap, not a claim that the latest-N endpoint
misimplements its cap, that consensus loses fills, or that monetary state is
incorrect. The missing continuation becomes material because reconnect/lag
recovery is the stated client contract. A client cannot reliably reconstruct
its full trade list/roles/own-side information with this endpoint once a gap
exceeds the available window.

**Checked fallback and narrower claim.**
[getBlockTrades](../../crates/torus-rpc/src/torus.rs#L1636) is uncapped and
[scans every known market's packed chunks](../../crates/torus-rpc/src/lib.rs#L768).
It is a valid fallback for complete **public market trade** recovery. It is
not a user-history equivalent: its
[projection](../../crates/torus-rpc/src/lib.rs#L819) has trade ID, market,
price, quantity, taker side, block, and timestamp, but no maker/taker addresses,
user role, or order ID. The persistent market codec
[also omits those identities](../../crates/torus-state/src/trade_rows.rs#L179).
Those fields exist on the [live public stream](../../crates/torus-rpc/src/streams.rs#L133),
which cannot replay the disconnected interval. This report does not claim that
every possible independent indexer or historical execution reconstruction is
unable to recover ownership; it identifies the shipped RPC contract gap.

The market range reader has a related **endpoint-specific** limitation:
[limit is clamped to 5,000](../../crates/torus-rpc/src/torus.rs#L1694), and its
only continuation inputs are inclusive block bounds. A single-market block
with more than 5,000 fills cannot be completely paged using that method alone;
starting again at the final returned block repeats its prefix, while starting
at the next block skips its suffix. The uncapped `getBlockTrades` fallback
prevents promoting this as public market data inaccessible across all RPCs.

**Coverage read.**
[user_trades_newest_block_first_then_trade_index_with_filter_and_limit](../../crates/torus-rpc/src/lib.rs#L4176)
asserts a four-entry fixture, newest block first, ascending trade index within
each block, and limiting after market filtering.
[get_user_trades_newest_first](../../crates/torus-rpc/src/lib.rs#L4969) checks
three blocks. These are useful ordering tests, not recovery past the cap.
[lagging_subscriber_gets_error_and_is_closed](../../crates/torus-rpc/src/streams.rs#L881)
checks the close error using a two-block synthetic notifier buffer; it does
not exercise a real subsequent backfill. No inspected test asks the shipped
RPC to recover 1,001 intact user entries or a capped single-block range suffix.

**Regression/fix direction.** Produce real funded fills for one user in one
market over ordinary blocks, retain a stream cursor, disconnect across at
least 1,001 own-role entries, wait for history persistence, and require recovery
of the exact full `(blockNumber, tradeId, role)` set without duplicates. Include
several fills within a block and newly arriving later blocks while paging. A
user history cursor/bounds with an explicit continuation/completeness signal
would permit bounded requests; it must respect the actual newest-block then
ascending-in-block ordering. Market-range pagination needs a within-block
cursor or explicit direction to the uncapped block endpoint. These are proposed
assertions and design directions, not implemented or passing regressions.

## F26 supplement — an empty successful trade response can permanently omit normal candles

No new ID is proposed for this consequence of the existing explorer completion
root. It does **not** require an RPC error, SQLite failure, host crash, or absent
later head notification.

The [normal native state flush](../../crates/torus-consensus/src/app.rs#L2489)
publishes the durable applied-height marker. The
[Ethereum head](../../crates/torus-rpc/src/eth.rs#L176) exposes that marker,
capped by commit height. The same execution path only subsequently
[hands fills to the history writer](../../crates/torus-consensus/src/app.rs#L2620).
The writer operates on its own thread and queue
([worker loop](../../crates/torus-state/src/bg_writer.rs#L229)); history may
still be pending when the explorer reads a valid applied block.

A legitimate ordering is: block H's native state and marker are durable;
explorer samples applied head H; the queued history rows for H are not yet
written; explorer successfully reads H's Ethereum block and then gets an empty
`getBlockTrades(H)` response. That endpoint directly scans the current trade
CFs and has no history-complete frontier or “pending” response. Explorer treats
the successful empty array as no trades
([indexing](../../crates/torus-explorer/src/indexer.rs#L145)), then
[advances its cursor](../../crates/torus-explorer/src/indexer.rs#L157).
The rows subsequently land, but later heads start at H+1; explicit same-hash
retry [returns early](../../crates/torus-explorer/src/indexer.rs#L71). H's valid
fills can therefore remain absent from every candle interval while direct node
history queries correctly show them.

This extends the existing F26 explanation beyond trade RPC errors: the applied
state frontier does not certify completion of node-local history. The source
also deliberately permits chunked history batches, so a large block can expose
a successful partial set before all chunks land
([writer contract](../../crates/torus-state/src/bg_writer.rs#L27)). That can
omit only a suffix of candle updates rather than the entire block.

[deferred_trades_reach_db_via_background_writer](../../crates/torus-consensus/src/app.rs#L11433)
executes real funded orders, then **drops the context to drain/join the writer
before reading**. It establishes eventual normal persistence, not explorer
completeness while the writer is pending.
[fill_sink_receives_every_executed_block_in_order](../../crates/torus-consensus/src/app.rs#L13496)
compares sink/history after fixture shutdown; it has the same visibility limit.
The [explorer decimal-candle fixture](../../crates/torus-explorer/src/indexer.rs#L625)
directly calls `index_trades`; it does not coordinate the applied marker,
history writer, node RPC, and explorer cursor.

Recommended regression: pause the background history writer without failing
it, execute normal fills, prove H is available through the applied Ethereum
head, and run the actual explorer indexer. Release/drain the writer and give
the explorer later heads or an explicit retry. Require correct OHLCV/counts
for H exactly once. Repeat with a pause between history chunks. A repair needs
a separate history-complete signal/retryable candle phase or another durable
source for trades; moving explorer to the applied state head alone is
insufficient. This is source-derived timing evidence, not a runtime race
reproduction or a fresh F26 count.

## Checked behavior and negative results

| Path | Current source/tests support | Practical evidence limit |
| --- | --- | --- |
| Packed market and user rows | [Stable market/address grouping and deterministic keys](../../crates/torus-state/src/trade_rows.rs#L167); [round_trip_both_row_kinds](../../crates/torus-state/src/trade_rows.rs#L332); [market_rows_chunk_at_1024_fills](../../crates/torus-state/src/trade_rows.rs#L368) | Codec tests are synthetic; deterministic bytes do not guarantee history was durably written before a query. |
| Latest market history | Reverse chunk/key traversal plus reverse entries produces newest block/trade first; [trade_history_limit_spans_chunk_boundary_newest_first](../../crates/torus-rpc/src/lib.rs#L3668) exercises 1,030 fills and crosses the 1,024 boundary | No new positive-limit ordering defect found. This endpoint is latest-N, not a cursor API. |
| Inclusive market block range | [Forward key bounds](../../crates/torus-rpc/src/torus.rs#L1707); [trade_history_range_oldest_first_with_limit](../../crates/torus-rpc/src/lib.rs#L3700) includes both ends, excludes the later block, and crosses chunks | Block bounds are not Unix timestamp bounds. Extreme integer-height behavior was outside this ordinary-query scope. |
| User side/filter/ordering | Maker's side is opposite taker's, filtering occurs before the limit; [user_trades_report_the_users_own_side](../../crates/torus-rpc/src/lib.rs#L4595) and the four-entry ordering/filter fixture cover those contracts | Ascending index within a newest block is explicitly tested; it is not a newly discovered inversion. |
| Native stream production wiring | [Fill sink implementation](../../crates/torus-rpc/src/lib.rs#L184) publishes execution fills; application fixture covers serial/pipeline output and state neutrality | This is distinct from F33's dormant Ethereum log publisher. WS tests using direct notifier sends alone do not prove production wiring, but the application fixture supplies additional relevant coverage. |
| Stream limits and closure | [Subscribe before accept, RAII slot, sink closure selection](../../crates/torus-rpc/src/torus.rs#L2016); [lag regression](../../crates/torus-rpc/src/streams.rs#L881) checks explicit terminal notification | No new native quiet-disconnect leak established. The subscription cap is shared with Ethereum subscriptions, whose earlier defects remain separately tracked. |
| First in-flight block and stream-only fields | [One wants-fills sample per block](../../crates/torus-consensus/src/app.rs#L2243); [missing extras deliberately yields no userFills](../../crates/torus-rpc/src/streams.rs#L228); [specific test](../../crates/torus-rpc/src/streams.rs#L502) | Initial in-flight misses, stream ahead of history, and absent historical orderId/startPosition/closedPnl/dir are [documented](../api/streams.md#L118), not new findings. |
| Public block trade ordering | [Known-market scan](../../crates/torus-rpc/src/lib.rs#L790) returns market-key order then each market's trade-index order; [scan_trades_for_block_lists_every_market_in_order](../../crates/torus-rpc/src/lib.rs#L3780) covers both chunks | It need not be globally sorted by trade ID across interleaved markets. Candle processing is independent per market, so no new OHLC ordering defect was inferred. |
| Explorer decimal/timestamp projection | [Decimal parsing](../../crates/torus-explorer/src/indexer.rs#L431), [candle aggregation](../../crates/torus-explorer/src/db.rs#L492), [API scale conversion](../../crates/torus-explorer/src/api.rs#L232), and decimal-candle fixture preserve ordinary eight-decimal values | [Production headers use Unix seconds](../../crates/torus-consensus/src/app.rs#L1512), matching second-based candle buckets and [API docs](../api/streams.md#L113). Millisecond-looking test fixtures elsewhere are not evidence of a production candle time-unit defect. |

The reviewed decimal-candle test also asserts that legacy hexadecimal
price/quantity are skipped rather than silently persisted as zero. Ordinary
values fitting SQLite's signed integer representation round-trip through this
path; no claim was made for arbitrary i128 extremes or aggregate-volume
overflow. The explorer still has the earlier envelope/action-count projection
gap; that is already described by September Astra and pass-three R5, and was
not recounted.

History-off operation, format-change wipes, and hard-crash loss of queued
node-local history are deliberate documented storage choices
([writer contract](../../crates/torus-state/src/bg_writer.rs#L1),
[format reset](../../crates/torus-state/src/trade_rows.rs#L225)). They must be
distinguished from H01, whose rows are fully written and retained, and the F26
supplement, which needs only ordinary concurrent writer delay. This review
does not certify historical completeness for those deliberate modes, perform
crash experiments, reopen the interrupted pass-five scopes, or establish a
runtime-confirmed finding.
