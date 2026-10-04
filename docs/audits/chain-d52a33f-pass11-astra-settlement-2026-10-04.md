# Pass 11 — ordinary market settlement and conservation

Reviewed 2026-10-04 in `/home/oz/projects/Torus-hyperBFT`, branch
`audit/chain-findings-2026-10-04`, starting HEAD
`0c967cac7f8e30bb79d98390948e9299e6ac3feb`. The inspected production files and
parallel-settlement tests have no diff against production revision
`d52a33f51038105a8d0e102fc2e1a5c51f43fb09`.

**Result: no additional production defect is promoted.** This bounded source
review traces ordinary fills, realized PnL, reservation release, stop triggering,
and the implemented fee/funding boundary. It identifies one specific test-path
qualification: the exact-PnL test named for cross-market parallel settlement
actually settles its PnL-producing batch through the sequential path.

The [audit policy](README.md), September Astra rounds, prior chain finding
catalogues, pass-6/7 accounting, pass-8 lifecycle, pass-9 collateral and pass-10
actions were consulted. Established F01–F43, September weighted-entry rounding,
the documented margin policies, and already-deferred features are not new
findings. No applicable `AGENTS.md` was found. Cargo and rustc are unavailable on
PATH; **tests were read, not run**. No build, model, installation, live-chain
action, adversarial experiment, production edit, Git mutation or Torus write was
performed. Only this report was added; the parent coordinates persistence.

## The executable economic scope is narrower than the API vocabulary

The [production caller](../../crates/torus-consensus/src/app.rs#L2251) executes
the pre/post-EVM native batches, then drains queued requests, runs liquidation,
and calls fee distribution. Native matching settles perpetual positions: both
fill sides pass `MarginType::Cross` to the position manager in the
[canonical batch loop](../../crates/torus-bridge/src/native_executor.rs#L5306)
and [parallel plan](../../crates/torus-bridge/src/native_executor.rs#L5805).
Genesis likewise describes its listings as
[perpetual markets](../../crates/torus-genesis/src/lib.rs#L193).

`TransferToSpot` is dispatched to
[native withdrawal](../../crates/torus-bridge/src/native_executor.rs#L4140),
which crosses into EVM account balances. It is not a distinct spot order-book
fill handler. A ledger test must not infer delivery of base/quote inventory from
that action name. The ordinary transfer boundary and queue timing remain the
scope already reviewed in [pass 9](chain-d52a33f-pass9-astra-collateral-2026-10-04.md).

There is no native fill-fee/rebate movement in the inspected settlement paths.
The native fee counter is [initialized to zero](../../crates/torus-bridge/src/native_executor.rs#L2655)
and [read by distribution](../../crates/torus-bridge/src/native_executor.rs#L8380),
without a production increment found by the symbol search. The
[current fee decision](../plans/native-antispam-2026-10-04.md#L324) explicitly
defers trading rates, maker rebates, fee asset and destination. Handler
`gas_used = 1000` is not a balance debit. A purported missing rebate or native
fee deduction is therefore not a new implemented-lifecycle defect here.

Likewise, the exposed
[`max_funding_rate_bps`](../../crates/torus-types/src/lib.rs#L1224) field is not
evidence of funding accrual or payment. The production transition inspected
below has no funding leg; funding's absence is already explained in
[pass 8](chain-d52a33f-pass8-astra-lifecycle-2026-10-04.md). This review does not
certify an unimplemented spot/funding product, and does not alter F02/F17's
separate EVM fee findings.

## Fill cash flows and cancellation conserve different quantities

For ordinary successful reads/writes, representable arithmetic and valid market
configuration, both sides of a fill receive the same quantity at the same
price with opposite signed directions in the
[settlement loop](../../crates/torus-bridge/src/native_executor.rs#L5303).
Direct and cached callers share
[`fill_transition`](../../crates/torus-core/src/position.rs#L487).
An increase changes size and weighted entry; partial/full closes and flips emit
only their closing component's PnL. Full closure deletes the row. Previously
accumulated `Position.realized_pnl` is not paid again on full close.

The [cached bridge](../../crates/torus-bridge/src/native_executor.rs#L5868)
credits that returned event through the same balance cache used for order-margin
release. The [direct caller](../../crates/torus-core/src/position.rs#L362) credits
it directly; the cached position helper itself does not also credit cash.
This rejects the lead that cache settlement systematically duplicates or
overwrites closing PnL. The existing
[PnL-then-release regression](../../crates/torus-bridge/tests/position_cache_exec_tests.rs#L158)
already checks that composition with exact values.

Cash alone need not stay constant when some traders realize PnL while their
counterparties retain open positions. A conservation assertion must include
unrealized position value at a common reference price, alongside
`available + order_margin`. Computed position IM is a requirement, not another
cash pot. The existing
[seeded accounting test](../../crates/torus-bridge/tests/account_margin_tests.rs#L1416)
uses this broader value equation. It permits 100 raw units of rounding drift;
that tolerance is neither a proof of exact conservation nor a universal bound.
September's lost weighted-entry remainder and pass 6's tolerance correction
retain their provenance; no further rounding issue is promoted.

Reservation accounting has a stronger exact identity. The
[maker release](../../crates/torus-bridge/src/native_executor.rs#L6055) aggregates
consumed quantity across all takers, then releases `reserve(before) -
reserve(after)`, with the STP remainder released separately. Reduce-only cuts and
margin cancellations join the consumed quantity before this calculation.
[Taker release](../../crates/torus-bridge/src/native_executor.rs#L6675) uses the
actual resting quantity, so a clamp or cancelled IOC remainder is not treated
as still reserved. Individual cancellation and cancel-all use the same raw
integer leverage division as
[`order_initial_margin`](../../crates/torus-core/src/margin.rs#L65); the
[optimized division](../../crates/torus-bridge/src/native_executor.rs#L7363)
does not introduce a second rounding rule.

The [parallel A5 regression](../../crates/torus-bridge/tests/parallel_settle_tests.rs#L579)
has four markets in its consuming batch and independently asserts full, partial,
raw-unit and STP releases, then exact zero reservations after cancellation.
Its positions can remain open when balances return to funding: it verifies
reservation release, not that every trading liability has closed. This coverage
is substantive and must not be described as only differential equality.

## Fired stops release the pending reservation before ordinary revalidation

Batch settlement [collects triggered stops](../../crates/torus-bridge/src/native_executor.rs#L4798),
flushes position/balance caches, then
[runs them after settlement](../../crates/torus-bridge/src/native_executor.rs#L4883).
The [trigger runner](../../crates/torus-bridge/src/native_executor.rs#L6740)
first releases the pending stop's cap/limit reservation, then sends its converted
order through the normal scalar placement path under its existing ID. A
reduce-only stop whose position already closed can fail that revalidation
without leaving its old reservation stranded.

This is explicitly tested. The
[already-closed position fixture](../../crates/torus-bridge/tests/reduce_only_tests.rs#L422)
asserts that the stop disappears, no reverse position opens, unrelated resting
liquidity remains, and the trader returns to funding with zero order margin.
The [live-position fixture](../../crates/torus-bridge/tests/reduce_only_tests.rs#L458)
starts long 4, triggers a reduce-only sell of 6, and asserts closure of exactly
4 at 94, counterparty size 4, remaining bid size 6 and a realized loss of 24.
The independent
[cancel-all fixture](../../crates/torus-bridge/tests/market_order_margin_tests.rs#L1092)
checks pending-stop removal and exact release, including context save/reload.
These reject the ordinary stranded-stop-reservation lead. Individual pending
stop cancellation by ID remains the previously deferred interface limitation.

## One exact-PnL fixture does not enter parallel settlement for its closing batch

**Classification: test-coverage qualification, not a production defect.**

[`pnl_realizing_cross_market_flow_exact`](../../crates/torus-bridge/tests/parallel_settle_tests.rs#L503)
runs with both `parallel = false` and `true`. Its seed batch includes markets 1
and 2, but the
[PnL-producing second batch](../../crates/torus-bridge/tests/parallel_settle_tests.rs#L527)
contains only market 2: trader A buys 4 at 200 and sells 4 at 195. The exact
final balance assertion of 9,980 is useful sequential economic coverage.
However, the
[selection gate](../../crates/torus-bridge/src/native_executor.rs#L4809)
requires `market_results.len() >= 2` before considering `SettleMode::Force`.
Consequently the second batch takes the sequential path for both runs. The
first batch's actual fill opens positions and generates no closing PnL.

Countercheck: this does **not** mean parallel PnL has no tests. The
[six-market scenario](../../crates/torus-bridge/tests/parallel_settle_tests.rs#L234)
feeds [repeated byte-equivalence tests](../../crates/torus-bridge/tests/parallel_settle_tests.rs#L328)
and [worker-cap comparisons](../../crates/torus-bridge/tests/parallel_settle_tests.rs#L370).
They compare the broader observed state against sequential settlement, with a
trade-count non-vacuity assertion. Their inspected assertions do not independently
pin a particular trader's nonzero closing PnL in the parallel plan. No broad
claim that every other test misses this path is made.

**Focused regression:** extend the exact-PnL fixture's closing batch with a valid
order in another market, preferably an independently specified close by the same
trader. Assert that parallel planning actually occurred and that nonzero closing
events were produced in both markets, then assert the exact net balance,
remaining reservations, positions and persisted rows. Keep the existing
sequential comparison and avoid relying only on timing being nonzero to prove
path selection. This is a test improvement proposal, not evidence that the
parallel implementation returns a wrong balance.

This bounded pass leaves the existing production candidates open and makes no
runtime reproduction, verified-fix or whole-chain correctness claim.
