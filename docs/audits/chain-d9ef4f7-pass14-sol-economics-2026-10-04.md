# Pass 14 — economic precision and configuration transitions

Reviewed 2026-10-04 against source `merge/item6-c3-pf1` at
`d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd`, in the read-only
`/home/oz/projects/Torus-hyperBFT` checkout. This report is written to the separate
documentation worktree, `audit/chain-findings-2026-10-04`, starting at
`f39a35206772433e4bab4928d903a4366440f899`. Source links pin the reviewed commit;
the documentation branch's older implementation is not the source authority.

**Result: no additional substantive production defect is promoted.** The useful
additions are a bounded check of reciprocal leverage conversion, a correction
to the apparent units of staking actions, and stronger qualification of the
existing exact close/flip and lockbox boundary assertions. These observations
do not close the earlier economic findings.

The audit catalogue, September reports and passes 1–13 were consulted for
deduplication, particularly pass-8 marks/lifecycle, pass-9 collateral,
pass-11 settlement, pass-12 staking/governance and pass-13 execution. No
applicable ancestor or repository `AGENTS.md` was found. Source HEAD was checked
and the source checkout was clean. **Rust tests were read, not run**: Cargo and
rustc are unavailable in the established environment. A short Python calculation
checked specified integers only; it did not execute Rust, emulate a node or
prove the full accepted input domain. No installation, service, live-chain,
key, source, Git or Torus mutation was performed. Only this report was added;
the coordinator handles consolidation and commits. Interrupted pass-5
certificate/malformed-input/network assignments were not resumed.

## Reachable configuration changes and the reciprocal precision seam

The actual [application tail][tail] processes governance after trading,
queued requests and liquidation. Signed [ListMarket conversion][listing]
turns a positive integer leverage `L` into raw initial-margin percent
`floor(10^10 / L)`. The [governance executor][registry] assigns the market ID
at execution and persists that percent in the registry. The following
[context construction][context] loads market configurations through the
[registry decoder][decoder], which derives
`floor(10^10 / initial_margin_raw)`, clamped to the `u32` range.

Two truncations warrant checking rather than assuming reciprocal conversion
is exact. An independent Python integer calculation tested every integer
`L` from 1 through 1000 and found that the decoded leverage equals the requested
leverage throughout that bounded domain. Selected nondivisor cases are:

| Requested leverage | Stored initial-margin raw | Decoded leverage | IM raw for notional 100 |
| --- | ---: | ---: | ---: |
| 3 | 3,333,333,333 | 3 | 3,333,333,333 |
| 7 | 1,428,571,428 | 7 | 1,428,571,428 |
| 20 | 500,000,000 | 20 | 500,000,000 |
| 33 | 303,030,303 | 33 | 303,030,303 |
| 50 | 200,000,000 | 50 | 200,000,000 |

Thus the ordinary 7x/33x conversion does not silently become another tier.
The check is not a claim that all accepted `u32` values preserve leverage;
the submission guard currently checks zero rather than defining a complete
supported leverage range. Bounds and precision outside the checked domain
need an explicit product contract before claiming arbitrary-value fidelity.
The current [order margin formula][orderim] divides raw notional by the loaded
integer leverage; maintenance is half that result. It does not repeatedly
multiply by the rounded percent at each reservation or release.

A normal reachability control matters for stale reservations. Signed
UpdateMarketParams and DelistMarket [still produce text-only payloads][listing].
ParameterChange writes retain F27's disconnected-consumer root. These routes
therefore do not presently change a live market's risk tier halfway through
its resting order's lifetime. A fixture that edits `ctx.margin_configs`
directly is useful cache testing, but cannot establish that a supported
governance action changes the tier under an existing reservation. ListMarket
executes at the tail, after that block's user trading has finished; the next
context loads its configuration. C3 [version comparison][versions] includes
the loaded configuration map, independently of whether marks changed.

The [existing listing conversion test][listingtest] asserts auto ID zero and
exact 5% for 20x, while its zero control asserts rejection and no proposal.
It stops at the submitted payload. The [two-listing test][alloctest] actually
executes both manager proposals and asserts distinct registry IDs, but does not
place an order through a later context. A useful joined regression is a signed
7x listing, normal vote/timelock execution, next-block configuration load, and
an exact reservation/release at that tier. Supply aligned whole-unit orders
and valid oracle reports to avoid conflating this with F25's separate book
metadata default. These are coverage boundaries, not a newly proven failure.

## Partial close and flip: an independent economic assertion already exists

Both direct and cached fills share [fill_transition][fill]. Partial closes
credit only the closed quantity and retain entry price; full closes delete
the row; flips credit the old position's entire close component and open only
the opposite remainder at the current fill price. Accumulated historical
`realized_pnl` is not paid again. The [bridge cache caller][cash] applies the
returned PnL through its balance cache, so the position helper does not debit
or credit cash a second time.

The [FillEffect test][effects] is stronger than mere cached/reference equality.
It independently specifies: long 5 at 100, increase by 3 at 110, entry 103.75;
sell 2 at 120, PnL +32.5; sell 10 at 90, closing PnL −82.5 and short remainder
4 at 90; buy 4 at 90, zero-PnL full close. It asserts each starting signed
size and PnL, explicitly checks the flipped row's direction/size/entry, and
checks final deletion. Both helper paths are checked against those constants.
Independent Python arithmetic confirmed raw entry `10,375,000,000`, partial
PnL `3,250,000,000`, flip-close PnL `−8,250,000,000` and net `−5,000,000,000`.

This rejects ordinary duplicated historical PnL and a flip that opens the
whole sell quantity. It does not establish a signed block's final withdrawable
balance: the cached test consumes effects without the production balance cache,
and prices/quantities avoid nonrepresentable intermediate fractions. FixedPoint
[multiplication/division][fp] use signed integer truncation with an i256
intermediate. September's weighted-entry remainder remains existing provenance;
exact values in this fixture must not be generalized to all partitioned fills.
Pass 11's parallel-path coverage qualification also remains separate.

## Collateral dust has rejection controls as well as conservation assertions

Current [lockbox conversion][units] maps one native raw unit to `10^10` wei.
Native transfer actions request eight-decimal raw units and convert exactly.
The EVM [deposit selector][selector] floors accepted wei, queues the native
credit, and rejects a nonzero deposit smaller than one native unit. Withdrawals
reject nonmultiples of `10^10`; zero is a no-op. The [EVM provider][burn]
burns accepted deposit value in the same journaled frame as the queued credit.
The [drain helper][credit] credits native only, avoiding a second EVM debit.

Independent arithmetic checked `39,999,999,999` wei → native raw 3 plus dust
`9,999,999,999` wei. Returning that native amount gives `30,000,000,000` wei;
the difference is the specified burn, rather than a denomination error.
The [bridge deposit test][dusttest] asserts no mid-block native credit, exact
next-block credit, unchanged EVM balance during drain, and
`remaining value + burned dust = initial value`. Its zero-base-fee fixture
isolates collateral conversion; it does not validate F02's fee composition.

Controls are substantive: [core selector tests][boundarytests] reject all-dust
and mismatched-value deposits, accept zero without a queued entry, and reject
nonround withdrawals. The [EVM bad-call test][evmcontrols] additionally executes
the all-dust and nonround cases, asserts every transaction reverted, unchanged
account/native balances and an empty next-block queue. This is existing runtime
test code, read here without execution. No claim that all-dust deposits silently
burn the entire value, or that these tests only compare conversion helpers,
survives these source assertions.

## Staking units are action-specific; one helper comment is misleading

The [fp_to_u256 rustdoc][units] includes `Delegate` among allegedly eight-decimal
native action amounts. That inclusion conflicts with actual callers:
[NativeExecutor delegates][stakecall] the `U256` unchanged;
[StakingManager][stakebalance] debits EVM account wei; and the
[wallet staking command][walletstake] uses `parse_trs_to_wei`. The
[wallet transfer command][wallettransfer] deliberately uses the separate
eight-decimal parser. A native action envelope therefore does not determine
its amount's denomination; the operation does.

For example, one TRS delegation carries `10^18`, whereas a one-TRS native
lockbox transfer carries `10^8`. Correct the helper comment or document units
per action, without changing the functioning staking encoding. This is a
documentation qualification, not another factor-`10^10` production transfer
finding. The [staking lifecycle fixture][staketest] asserts wei principal,
exact pending rewards, exact claim credit and rejection of a second claim;
it calls managers directly, rather than proving signed admission. Whole-token
[oracle/voting power][power] intentionally floors wei at that later consumer;
it does not convert stored staking principal to native collateral units.

F45's zero-active recipient dust, F09/F27's governance consumer/snapshot issues,
F12's solvency sequence and F02's fee composition remain open with prior
qualifications. Funding has no implemented accrual/settlement lifecycle in the
current production tail; exposed `max_funding_rate_bps` and oracle design
vocabulary do not create one. This is already documented in passes 8 and 11,
so it is neither a new finding nor funding correctness certification.

[tail]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-consensus/src/app.rs#L2250
[listing]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-bridge/src/native_executor.rs#L8286
[registry]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-economics/src/governance.rs#L1088
[context]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-bridge/src/native_executor.rs#L2886
[decoder]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-core/src/margin.rs#L227
[orderim]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-core/src/margin.rs#L58
[versions]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-bridge/src/native_executor.rs#L8622
[listingtest]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-bridge/tests/market_governance_exec_tests.rs#L154
[alloctest]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-economics/tests/governance_tests.rs#L722
[fill]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-core/src/position.rs#L487
[cash]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-bridge/src/native_executor.rs#L6133
[effects]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-core/tests/position_cache_tests.rs#L162
[fp]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-types/src/lib.rs#L86
[units]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-core/src/lockbox.rs#L257
[selector]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-core/src/precompiles.rs#L1079
[burn]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-evm/src/precompile_provider.rs#L157
[credit]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-bridge/src/native_executor.rs#L8536
[dusttest]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-bridge/tests/lockbox_queue_tests.rs#L185
[boundarytests]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-core/tests/precompile_tests.rs#L797
[evmcontrols]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-evm/tests/evm_tests.rs#L1352
[stakecall]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-bridge/src/native_executor.rs#L7822
[stakebalance]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-economics/src/staking.rs#L1003
[walletstake]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/tools/wallet/src/commands/staking.rs#L10
[wallettransfer]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/tools/wallet/src/commands/transfer.rs#L60
[staketest]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-integration-tests/tests/staking_lifecycle.rs#L43
[power]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-bridge/src/native_executor.rs#L8758
