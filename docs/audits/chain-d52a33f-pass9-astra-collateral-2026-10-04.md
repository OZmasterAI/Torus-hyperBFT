# Pass 9 — ordinary collateral lifecycle and accounting

Reviewed 2026-10-04 on `perf/item6-phase1`, HEAD
`d52a33f51038105a8d0e102fc2e1a5c51f43fb09`.

**Result: no additional production defect is promoted.** This bounded review
traced legitimate native and EVM deposits, queued native credits, reservation and
position settlement, withdrawal gates, and the reachability of isolated-margin
state. It records safeguards and coverage boundaries, without adding another
finding number or closing existing candidates.

The [audit policy](README.md), September Astra rounds, pass-3 trading,
pass-6 accounting, pass-7 accounting/execution and pass-8 compatibility/marks
were consulted. F02/F12/F17/F22–F25/F30 and the previously documented rounding,
queue and reservation issues are not recounted. No applicable `AGENTS.md` was
found. Cargo/rustc are unavailable: all Rust tests cited below were **read, not
run**. No production edits, Git mutations, installation, network/live-chain
actions, key generation, adversarial inputs or Torus writes were performed.
Only this report was added.

## Isolated collateral is not an ordinary production allocation path

This is a reachability qualification, not a newly found funds-loss bug.
[`Position`](../../crates/torus-core/src/position.rs#L51) contains
`isolated_margin` and `margin_type`, and its serializer preserves both. Read
interfaces and helper margin checks can therefore describe isolated positions.
That does not establish a public operation that creates and funds one.

The current [`NativeAction`](../../crates/torus-types/src/lib.rs#L594) and
[`PlaceOrderParams`](../../crates/torus-types/src/lib.rs#L1123) expose no isolated
mode selector or isolated-margin allocation operation. Actual
[scalar settlement](../../crates/torus-bridge/src/native_executor.rs#L7026),
[cached settlement](../../crates/torus-bridge/src/native_executor.rs#L5878) and
[parallel settlement planning](../../crates/torus-bridge/src/native_executor.rs#L5805)
pass `MarginType::Cross`. New positions and flipped remainders start with zero
isolated margin in the shared
[transition](../../crates/torus-core/src/position.rs#L498). A source search of
`isolated_margin` and `MarginType::Isolated` found the positive allocations in
test fixtures, alongside readers and helper calculations; it did not identify
a shipped trading operation that debits available collateral into that field.

Accordingly, a hypothetical manually seeded isolated position losing its margin
on full close is not promoted as an ordinary reachable production defect.
The [cross-account view](../../crates/torus-core/src/margin.rs#L142) explicitly
filters for Cross positions and documents that production positions are Cross.
Activating isolated trading would require an explicit allocation/release and
withdrawal contract, plus production-caller tests. Existing isolated read tests
and byte compatibility tests do not establish that lifecycle. This review does
not assert compatibility with an unspecified historical database containing
funded isolated positions.

## Successful deposits and withdrawals use the expected balance authority

The native
[deposit](../../crates/torus-core/src/lockbox.rs#L62) reads the current EVM
account, credits the versioned native balance and writes both legs atomically.
The [withdrawal helper](../../crates/torus-core/src/lockbox.rs#L146) likewise
debits the sender's available amount and credits the requested EVM recipient.
Its account update preserves nonce/code hash in the current 72-byte record;
this matches the actual
[account serializer](../../crates/torus-state/src/db.rs#L1011).
The native amount is raw eight-decimal fixed point and EVM wei is scaled by
`10^10`; the historical raw-identity conversion is not the present code.

For EVM deposits, the actual
[provider](../../crates/torus-evm/src/precompile_provider.rs#L157) removes the
accepted value through the EVM journal, while the
[lockbox selector](../../crates/torus-core/src/precompiles.rs#L1079) records a
next-block credit. The
[drain's deposit branch](../../crates/torus-bridge/src/native_executor.rs#L8265)
calls `credit_native`, which only credits native available collateral. It does
not debit EVM again. Ordinary queued credits belong in the intermediate ledger
until drained; they are not immediately spendable native balances.

Production [runs both native batches before the drain](../../crates/torus-consensus/src/app.rs#L2251).
The existing
[application lockbox fixture](../../crates/torus-consensus/src/app.rs#L17013)
checks exact timing: no native credit at block 1, a credit of 1000 at block 2,
zero reservation for block 2's premature order, and positive reservation for
block 3's order. It compares resident rows on/off and serial/pipelined execution.
This meaningful coverage was already identified in
[pass 7](chain-cea1254-pass7-astra-execution-2026-10-04.md); it is not new coverage
discovered by this pass, nor merely a helper equality assertion.

All three public withdrawal routes meet the same
[account gate](../../crates/torus-bridge/src/native_executor.rs#L8171): native
`TransferToSpot`, native `Withdraw`, and queued lockbox withdrawal mapped back to
`TransferToSpot`. The gate requires remaining account equity excluding resting
reservations to cover transfer margin; the lockbox separately enforces cash
availability. A result reporting insufficient collateral does not itself debit
funds. The existing
[queued margin test](../../crates/torus-bridge/tests/lockbox_queue_tests.rs#L381)
checks the 51-rejected/50-accepted boundary, and
[account-margin tests](../../crates/torus-bridge/tests/account_margin_tests.rs#L266)
cover position IM, UPnL, flat withdrawal and exclusion of resting reservations.
These do not resolve F12's separate fill-solvency mechanism.

## Reservation, position and reload evidence has specific limits

Current `available + order_margin` is collateral before unrealized PnL;
position IM is a computed requirement, not another stored pot to add to cash.
The [account formulas](../../crates/torus-core/src/margin.rs#L172) and the
[shared balance-cache contract](../../crates/torus-bridge/src/native_executor.rs#L105)
preserve that distinction. Signed negative available can arise under the
documented UPnL policy; it alone is not evidence of missing funds.

This review found no additional ordinary partial-fill/close release issue beyond
the mechanisms already inspected in pass 7. Retain the independently specified
[partial-fill/cancel assertions](../../crates/torus-bridge/tests/maker_margin_release_tests.rs#L134),
[PnL followed by reservation release](../../crates/torus-bridge/tests/position_cache_exec_tests.rs#L158)
and [full-close tombstone assertion](../../crates/torus-core/tests/position_cache_tests.rs#L245).
They meaningfully test economic values and deletion, rather than only comparing
two implementations. No dust or PnL-release candidate is added here.

The [owned-write compatibility test](../../crates/torus-core/tests/position_cache_tests.rs#L356)
compares actual stored position/balance bytes against Borsh serialization and
checks the 95/33-byte lengths. Its isolated fixture has zero isolated margin;
it is not evidence of funded isolated lifecycle coverage. Context reconstruction
and CF-dump parity are also narrower than closing and reopening RocksDB through
normal startup, as pass 7 already explains.

The remaining useful integration extension is the previously proposed ordinary
deposit → real partial fill → cancel remainder → profitable/loss-making close →
withdraw sequence with independently stated balances. Use listed markets,
aligned prices/quantities, ordinary funded counterparties and usable marks;
finish persistence before comparing queries. Check available and reservation
separately, absent closed positions, pending-credit liabilities, both EVM
accounts and actual fee/dust movements. Repeat a clean database reopen under
unchanged settings and compare the same expected values. This is a coverage
recommendation, not a missing-test finding, an executed regression, or a claim
that byte parity proves economic solvency or crash recovery.
