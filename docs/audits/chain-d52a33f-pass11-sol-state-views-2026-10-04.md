# Pass 11: ordinary native RPC state views

Reviewed production source at `d52a33f`, from documentation-only HEAD
`0c967cac7f8e30bb79d98390948e9299e6ac3feb` on
`audit/chain-findings-2026-10-04`. A read-only `git diff --name-only d52a33f
HEAD -- crates` returned no production-source differences.

**Result: no additional numbered candidate established.** This bounded pass
traces normal persisted market, book, position, balance and trade rows through
their native RPC projections. It retains specific test and interface limits;
it does not close earlier findings or certify snapshot consistency.

The [audit policy](README.md), September
[round one](astra-round1-2026-09-24.md),
[round two](astra-round2-2026-09-24.md), October consolidations through F43,
[pass-eight compatibility](chain-d52a33f-pass8-astra-compatibility-2026-10-04.md),
[pass-nine collateral](chain-d52a33f-pass9-astra-collateral-2026-10-04.md),
[history](chain-d52a33f-pass9-sol-history-2026-10-04.md) and
[pass-ten recheck](chain-d52a33f-pass10-astra-recheck-2026-10-04.md)
were consulted for overlap. No applicable `AGENTS.md` was found in repository
ancestors or the reviewed source/docs subtrees. Cargo and rustc are unavailable;
all Rust tests described below were **read, not run**. No production changes,
Git mutations, installs, live-chain/network experiments or Torus writes were
performed. Only this report was written. Previously blocked pass-five
certificate/malformed-input work was not resumed.

## Books and open orders: read the actual persisted layout

**Preconditions for the comparison.** Use ordinary funded traders and valid
orders, wait for execution and book persistence to complete, and compare a
fixed DB state. Choose tick/lot-aligned values that do not depend on F25's
first-book initialization defect. Do not delete node-local order rows, change
the writer layout on an existing DB, or query during a deferred save.

[Book save](../../crates/torus-bridge/src/native_executor.rs#L3420)
writes the real Classic blob or tagged row layout; LevelAuthority splits
root-CF aggregate rows from node-local identity rows. The
[RPC layout detector](../../crates/torus-rpc/src/torus.rs#L810) delegates to
the [persisted marker/content detector](../../crates/torus-core/src/book_reader.rs#L163).
It does not choose its read layout from a process environment variable.
The [mode discriminator](../../crates/torus-core/src/book_reader.rs#L62)
maps mode 3 to the same reader layout as mode 2.

[Depth RPC](../../crates/torus-rpc/src/torus.rs#L914)
decodes the production Classic `OrderBook` before its preserved legacy
snapshot fallback. Row mode uses
[shared depth](../../crates/torus-core/src/book_reader.rs#L319):
OrderRows aggregates `remaining_qty`, reverses bids to highest-first and
leaves asks lowest-first; LevelAuthority reads root-CF quantity/count
aggregates. The corresponding
[in-memory depth](../../crates/torus-core/src/order_book.rs#L1715)
also sums remaining quantity. Thus an original quantity of five with a fill
of two is projected as three remaining, rather than five original.

[Open-orders RPC](../../crates/torus-rpc/src/torus.rs#L1738)
reads actual book state and uses the
[row reader](../../crates/torus-core/src/book_reader.rs#L448)
for row layouts. Its [field mapper](../../crates/torus-rpc/src/torus.rs#L2151)
separately exposes original and remaining quantity, side, type, time in force,
reduce-only, client ID and timestamp. This native RPC path is a counterexample
to extending F37's unused EVM order-index failure to every order query.

**Actual assertions.** The
[RPC production-save fixture](../../crates/torus-rpc/tests/book_read_modes_rpc_tests.rs#L86)
uses `NativeExecutor::execute_batch`, requires successful actions, then saves
the book. The [depth test](../../crates/torus-rpc/tests/book_read_modes_rpc_tests.rs#L133)
checks bid prices 100 then 99, remaining quantities three and seven, count one
at the first level, and ask price 105/quantity four in Classic, OrderRows and
LevelAuthority. The
[RPC order test](../../crates/torus-rpc/tests/book_read_modes_rpc_tests.rs#L162)
checks three resting orders in market one, four across markets, and inclusion
of market two. It does **not** compare every returned order field. The
[shared-reader test](../../crates/torus-bridge/tests/book_read_modes_tests.rs#L293)
adds exact `(market, id, price, remaining_qty)` comparison and an unrelated
trader's empty result. Those stronger assertions belong to the shared reader,
not to every JSON field or the separate EVM selector.

**Ordering qualification.** Shared row reads explicitly sort by
`(market_id, order_id)` before truncation. Classic RPC obtains orders through
[orders_for_trader](../../crates/torus-core/src/order_book.rs#L1670), whose
[TraderOrders sequence](../../crates/torus-core/src/order_book/trader_orders.rs#L1)
is arrival order, or decode/load order after reconstruction. This is not an
established public RPC promise of identical array order across layouts. Compare
identity/field sets unless a common RPC ordering contract is added; do not
infer a missing order from a changed array index.

The [5000-order cap test](../../crates/torus-rpc/src/lib.rs#L4822)
asserts 5000 across a directly seeded 5101-order fixture and 2500 in one market.
This proves endpoint truncation; that fixture bypasses normal per-user
execution admission. The production maximum is
[5000 slots](../../crates/torus-core/src/position.rs#L228), and
[the documented endpoint](../api/user-limits.md#L45) returns resting orders.
Pending stops count toward
[user limits](../../crates/torus-rpc/src/torus.rs#L825) but are intentionally
absent from the resting-order list. The
[real-save user-limits test](../../crates/torus-rpc/tests/book_read_modes_rpc_tests.rs#L196)
checks four resting plus one stop equals five slots, maker/taker cumulative
volume 200, and limit 1000. These differing counts do not establish lost stops.

**Focused regression additions.** In each layout, place funded valid orders,
partially fill, modify, cancel and fully fill them through the executor; require
the exact JSON field set after each settled save. Include a client ID and
pending stop, comparing resting orders and slot counts by their separate
contracts. Cleanly reopen and repeat. Existing cancel/full-fill RPC fixtures
at [cancel](../../crates/torus-rpc/src/lib.rs#L4686) and
[fill](../../crates/torus-rpc/src/lib.rs#L4747) serialize Classic in-memory books;
their assertions do not cover every production layout or a clean reopen.

## Market enumeration: normal rows agree with the serializer

[Genesis market writes](../../crates/torus-genesis/src/lib.rs#L436)
and [governance listing writes](../../crates/torus-economics/src/governance.rs#L1107)
encode base/quote strings followed by raw lot, tick and initial-margin i128s,
under eight-byte big-endian market keys. The
[RPC enumeration](../../crates/torus-rpc/src/torus.rs#L1065)
uses that typed layout, skips non-eight-byte keys, counts offset only over
market keys, and formats lot/tick through `FixedPoint::from_raw` and decimal
Display. The `__book_mode__` marker therefore does not consume a market-page
offset or become a listed asset. Big-endian fixed-width keys give numeric ID
ordering without string sorting or lossy numeric conversion.

The [genesis round-trip assertion](../../crates/torus-genesis/src/lib.rs#L991)
checks base BTC, quote USD, lot `SCALE`, tick 1,000,000 and margin `5*SCALE`.
It decodes the exact tuple layout, rather than calling the RPC. The
[RPC markets test](../../crates/torus-rpc/src/lib.rs#L4027)
checks three directly seeded markets in BTC/ETH/SOL order and `active` status;
it does not check pagination, marker exclusion, or every metadata field.

**Preconditions and counterexamples.** Static market pages assume an unchanged
set between calls. Ordinary new automatic listings allocate larger IDs, so a
later append does not shift earlier page offsets. This does not prove stable
pagination under every concurrent insertion: explicit unused governance IDs
are supported and could insert before an existing offset. The familiar mutable
OFFSET limitation is already in September's explorer review, and this pass
establishes no separately promised native snapshot-pagination contract.
`active` is a fixed RPC status; this review found no ordinary persisted inactive
market flag that the handler should have projected instead. F25 remains about
execution book settings versus listed settings, not corruption of this decoding.

Add pagination assertions with nonconsecutive IDs, the book marker, a positive
page limit, offset beyond the end and the 500-item cap. Feed one genesis and one
ordinary governance listing through actual writes before comparing all fields.

## Positions, balances and open interest: canonical rows, bounded projections

Normal settlement calls
[apply_fill_via_caches](../../crates/torus-bridge/src/native_executor.rs#L5868),
updates positions as `Cross`, credits a close's PnL to available balance, and
[flushes position/balance caches](../../crates/torus-bridge/src/native_executor.rs#L4860)
before the execution call returns. The
[fill transition](../../crates/torus-core/src/position.rs#L487)
stores positive magnitude plus `is_long`, adjusts partial closes, removes full
closes, and writes the flipped remainder as a new position. The
[position key and CRUD](../../crates/torus-core/src/position.rs#L202)
use trader bytes followed by big-endian market ID. The
[RPC point read](../../crates/torus-rpc/src/torus.rs#L945) uses that same manager;
[open interest](../../crates/torus-rpc/src/torus.rs#L1827)
filters that same market suffix and sums magnitudes into separate long/short
totals. Missing positions yield `null`; full-close deletion is not a missing
storage-index defect.

[Balance RPC](../../crates/torus-rpc/src/torus.rs#L1001)
reads the typed native balance and current EVM account. Its native total is
`available + order_margin`; margin used is `order_margin + sum(isolated_margin)`;
available retains its sign. These fields are not a universal equity/free-margin
calculation including cross-position unrealized PnL. The earlier collateral and
compatibility reports already qualify that distinction.

The position mark comes from
[usable_oracle_price](../../crates/torus-rpc/src/torus.rs#L422), aged at the
executed-head header timestamp, with fallback to entry price. With positive
size this fallback gives zero UPnL. The
[freshness RPC test](../../crates/torus-rpc/src/lib.rs#L4507)
sets a five-unit long at entry 50,000 and a three-reporter aggregate at 51,000:
it independently requires UPnL 5000 at age 60 seconds and zero at age 61.
The [basic position test](../../crates/torus-rpc/src/lib.rs#L3877)
asserts side, size, entry, realized PnL and margin mode. It seeds an isolated row
and does not assert a liquidation threshold. The current
[liquidation-price projection](../../crates/torus-rpc/src/torus.rs#L975)
is explicitly a simplified estimate, returning zero without isolated margin;
that fixture does not establish normal isolated allocation or cross-account
liquidation accuracy. Existing production-path isolation limits were already
recorded in pass nine, so they are not another candidate here.

The [basic balance assertion](../../crates/torus-rpc/src/lib.rs#L3934)
requires native total 10,500 from available 10,000 plus order margin 500,
margin used 500, available 10,000, and the seeded EVM balance. The
[isolated-margin assertion](../../crates/torus-rpc/src/lib.rs#L3985)
requires margin used 1500 from order margin 1000 plus isolated 500. Both seed
typed rows. The [OI assertion](../../crates/torus-rpc/src/lib.rs#L4296)
requires long ten and short seven, again from typed rows. These prove the tested
projection, not full funded execution/position/balance/RPC composition.

**Focused regression.** Use real funded maker/taker fills in two markets, then
query position, OI and balances after state persistence. Assert an independently
expected long and short, signed UPnL at a known usable mark, partial-close PnL
credit, flip remainder and full-close absence/OI reduction. Retain the native
total and margin field definitions when comparing balances. Snapshot consistency
while execution is writing remains a separate untested timing issue; sequential
reads in this report do not resolve the previously documented pinning concerns.

## Trade sorting, own-side conversion and pagination limits

The [real fill recorder](../../crates/torus-bridge/src/native_executor.rs#L7106)
stores raw price/quantity and taker side from the actual fill. The
[application writer submission](../../crates/torus-consensus/src/app.rs#L2598)
passes block fills to the
[packed codec](../../crates/torus-state/src/trade_rows.rs#L167).
Stable grouping preserves ascending trade index within market chunks and user
rows; market keys are `(market, block, chunk)`, while user keys complement the
block number to make forward iteration newest-block first.

[Latest market history](../../crates/torus-rpc/src/torus.rs#L1123)
reverse-iterates chunks and fills, producing descending `(block, tradeIndex)`.
[Range history](../../crates/torus-rpc/src/torus.rs#L1683)
uses forward iteration and inclusive ordinary block bounds, producing ascending
order. [User history](../../crates/torus-rpc/src/torus.rs#L1913)
keeps ascending trade index inside each newest-first block and counts only
market-filtered entries. Its own-side expression makes a maker opposite to
the taker; public history remains taker-side. Those API differences are
intentional and tested, rather than a sorting/conversion defect.

The [chunk-boundary test](../../crates/torus-rpc/src/lib.rs#L3668)
requires IDs 1029 down to 1020 across a 1024-fill chunk boundary, checks raw
price/quantity decimal conversion, alternating sides, block and timestamp.
The [range test](../../crates/torus-rpc/src/lib.rs#L3700)
requires 1032 entries from blocks six through eight, excludes block nine,
checks IDs 0 through 1029 across the chunk boundary, and a 1025-entry prefix.
The [user ordering/filter test](../../crates/torus-rpc/src/lib.rs#L4176)
checks exact `(block, id, role, market)` sequences and limiting after filtering.
The [own-side test](../../crates/torus-rpc/src/lib.rs#L4595)
checks all four maker/taker buy/sell combinations and that public history retains
taker-side. These fixtures call the production row codec directly; they are not
all independent native-execution-to-history regressions.

F40's user-history recovery cap remains open. A dense single-market block beyond
the range cap also cannot be paged by inclusive block bounds alone, as already
qualified in pass nine; the
[uncapped block reader](../../crates/torus-rpc/src/lib.rs#L768)
is a counterexample to claiming all public data is inaccessible. Zero limit
still pushes one matching entry before checking the cap in user/latest/range
history and market enumeration. Pass ten already retains that post-push pattern
as an unnumbered edge; it is not counted again here. No `u64::MAX` block-height,
overflow-scale aggregate, malformed row or storage-fault scenario is promoted
as ordinary-state evidence.

## Numeric and verification boundaries

Native fixed-point outputs use
[dec_fp](../../crates/torus-rpc/src/types.rs#L250), directly calling
[eight-decimal Display](../../crates/torus-types/src/lib.rs#L162).
There is no intervening floating-point conversion. Negative sub-unit values
preserve their sign; the
[round-trip assertions](../../crates/torus-types/src/lib.rs#L1566)
include raw -1, zero, positive/negative values and large representable endpoints.
EVM/staking balances retain U256/hex denomination; F41's hex Quantity issue is
separate and not recounted. Field names remain camelCase, so F32's wallet schema
failure is not evidence that native RPC rows fail serialization.

This review establishes source traces and accurately bounded existing test
coverage. It does not report executed Rust tests, runtime reproductions,
verified fixes, a common historical state snapshot, complete corruption/fault
handling, or a full ABI/client integration audit. The focused regressions above
would strengthen composition evidence without manufacturing another finding.
