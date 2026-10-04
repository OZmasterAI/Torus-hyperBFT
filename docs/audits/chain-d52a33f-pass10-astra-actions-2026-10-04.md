# Pass 10 — ordinary native action failure semantics

Reviewed 2026-10-04 on `audit/chain-findings-2026-10-04`, HEAD
`feb01b60f28e2111611c68947c7063eac9e06f41`. The production crates have no diff
against `d52a33f`.

**Result: no additional production defect candidate is promoted.** Ordinary
insufficient-margin, insufficient-balance, missing-order and premature-claim
paths inspected below have relevant pre-mutation checks or explicit independent
action semantics. This is bounded source evidence, not runtime confirmation or
a statement that every action failure is atomic.

The September Astra reports, F01–F40 consolidations, pass-3 trading, pass-4
atomicity, pass-6 accounting/correctness, pass-7 accounting/execution, and pass-8/9
reports were used for deduplication. F04 storage-error behavior, F22 returned
CoreWriter IDs, F23 stop conversion, D2/D6 margin policy, phase-hoisted non-order
actions, queue acknowledgement versus settlement, and earlier reservation/PnL
coverage are not new findings. No blocked pass-5 scope, adversarial input,
fault-injection, certificate schedule, EVM rollback investigation, live-chain
operation, network access, key generation, production edit, or Git mutation was
performed. Cargo/rustc are absent; tests were read, not run. Only this report was
written; the coordinating agent handles consolidation, commits and Torus writes.

## The result contracts are distinct

Production builds executable actions only after its sender/replay checks,
[consumes their nonces](../../crates/torus-consensus/src/app.rs#L2104), and prepares
the [executed/skipped bitmap](../../crates/torus-consensus/src/app.rs#L2129)
before invoking the handlers. The application
[discards the batch return values](../../crates/torus-consensus/src/app.rs#L2251).
The pass-9 qualification therefore stands: `nativeActionStatus = executed`
records dispatch past those gates, not successful fulfillment of the user's
economic request. A normal business failure does not entitle the same native
nonce to run again.

There is a further useful precision even inside the executor.
[`NativeActionResult`](../../crates/torus-bridge/src/native_executor.rs#L47)
and the book's `OrderStatus` are different layers. A placement rejected before
matching for insufficient margin produces a failed action. Once the book has
returned a `PlaceResult`, ordinary rejection or cancellation can still finish
the handler successfully: both
[scalar completion](../../crates/torus-bridge/src/native_executor.rs#L7100) and
[batch completion](../../crates/torus-bridge/src/native_executor.rs#L5380)
return `ok` after release/settlement without requiring a resting order or fill.
For example, a funded market order can encounter no liquidity; a funded
PostOnly order can cross an existing spread.

This is observable source behavior, not a newly inferred promise that a business
request succeeded. The existing
[funnel fixture](../../crates/torus-bridge/tests/funnel_metrics_tests.rs#L221)
explicitly asserts action success alongside book rejection and zero fills;
the [scalar fixture](../../crates/torus-bridge/tests/funnel_metrics_tests.rs#L323)
does the same. Those comments call this a funnel gap, so they establish current
behavior, not a permanent product specification. No new externally exposed
business-success receipt was established here. Future receipt work should
represent admission, dispatch, handler result and order outcome separately.

## Ordinary failures and counterchecks

| Ordinary operation | Source evidence and bounded conclusion |
| --- | --- |
| Place an order with insufficient account margin | The [scalar account check](../../crates/torus-bridge/src/native_executor.rs#L6885) precedes reservation and book insertion. Batch [preparation](../../crates/torus-bridge/src/native_executor.rs#L5013) checks before updating the sender balance fold. [Stitching](../../crates/torus-bridge/src/native_executor.rs#L5112) assigns a global ID only for a passed preparation. An ordinary margin reject was not found to consume a reservation or order ID. This does not generalize to every later book rejection. |
| A rejected preparation near the open-order limit | [`take_open_slot`](../../crates/torus-bridge/src/native_executor.rs#L6092) returns the proposed increment; [preparation stores it only after passing](../../crates/torus-bridge/src/native_executor.rs#L5082). Loading the initial slot counter before a margin check is not consumption of the proposed slot. Conversely, the explicit contract allows an order rejected by the book later to retain a slot for that batch call; that distinction is not a newly discovered persistent open-order leak. |
| Resize a legitimate resting order beyond available account margin | [Modify](../../crates/torus-bridge/src/native_executor.rs#L7388) finds the order, checks ownership, replacement conditions and account need before changing balances or the book. Its [insufficient-margin return](../../crates/torus-bridge/src/native_executor.rs#L7510) precedes reservation and `book.modify_order`. There is no ordinary concurrent book mutation between the lookup and replacement in this handler. |
| Cancel or modify an order that has already filled/cancelled | [Cancel](../../crates/torus-bridge/src/native_executor.rs#L7157) returns `not found` after unsuccessful lookups; no balance release occurs without a removed order. [Modify lookup](../../crates/torus-bridge/src/native_executor.rs#L7400) returns before writes. Empty CancelAll is intentionally an idempotent success. |
| Request more delegated stake than remains | [Undelegate](../../crates/torus-economics/src/staking.rs#L143) checks the recorded amount before writing. Its unbonding-cap check follows a subtraction on an owned local struct, but the first persistence is later. That local subtraction alone is not a stored partial debit. |
| Delegate, permanently stake or top up without enough EVM balance | The common [debit helper](../../crates/torus-economics/src/staking.rs#L1006) rejects before `put_account`. [Delegate](../../crates/torus-economics/src/staking.rs#L103) checks validator existence/status before that debit; [top-up](../../crates/torus-economics/src/staking.rs#L867) checks validator existence first. Later IO failures remain outside this normal-business-error conclusion and do not close F04. |
| Claim unbonding before maturity, or with no delegation | [`claim_unbonded`](../../crates/torus-economics/src/staking.rs#L249) builds candidate delegation rows and the account credit in memory, rejects an empty matured set, then submits one atomic write. Early failure does not delete the pending claim. |
| Claim rewards when none remain | [`claim_rewards`](../../crates/torus-economics/src/staking.rs#L406) checks missing/zero liability before crediting. This is distinct from errors after credit on storage paths, which were not re-investigated. |
| Submit a proposal without the required stake, or vote on a missing/finished proposal | [Submission](../../crates/torus-economics/src/governance.rs#L698) checks stake before allocating/persisting a proposal; [voting](../../crates/torus-economics/src/governance.rs#L794) checks proposal existence/status, duplicate voting and positive weight before writing tallies. No ordinary rejection after a partial tally write was established. |

For ordinary FOK failure, the
[book precheck](../../crates/torus-core/src/order_book.rs#L1077) uses cloned maker
account/reduction state. Insufficient depth returns before matching. The
[depth fixture](../../crates/torus-core/src/order_book.rs#L4217) independently
requires the maker's quantity to remain five. PostOnly crossing also
[returns before matching](../../crates/torus-core/src/order_book.rs#L992), with
an [existing unchanged-book assertion](../../crates/torus-core/src/order_book.rs#L4027).
The executor subsequently releases the unused reservation. This is narrower
than treating every rejected action as a whole-state no-op: allocator progression,
batch-local slot policy and the dispatch nonce are separate contracts.

## Partial batches have an explicit contract and substantive tests

The [batch documentation](../../crates/torus-bridge/src/native_executor.rs#L4194)
explicitly says individual failures do not stop the batch. PlaceOrderBatch is
[flattened in member order](../../crates/torus-bridge/src/native_executor.rs#L4340)
and gets per-member results. The
[single-action wrapper](../../crates/torus-bridge/src/native_executor.rs#L4065)
calls that same batch engine, then reports aggregate success only if all members
succeeded. Its aggregate failure does not roll back successful members. No
atomic-all-or-none promise was found that would justify labeling this a defect.

The existing
[`mixed_batch_pass_fail_pins_partial_per_order_contract`](../../crates/torus-bridge/tests/parallel_matching_tests.rs#L790)
uses a balance of 120 and ordinary orders needing reservations of 50, 200 and
50. It asserts the result sequence success/failure/success, a two-ID increment,
and balance, reservation, ID and trade-index equality against a fresh batch
containing only the two valid siblings. This is meaningful coverage of failure
isolation. It does not directly assert final balances of 20/100 or compare every
book row, and its reference uses `execute_batch` again rather than independently
testing the entire signed application path.

The phase rule remains equally important: all non-placement actions execute
before placements, so cancel/modify/withdraw actions observe Phase-1 state.
Scalar list order is not a universal reference for mixed production batches.
Known D2/D6 preparation policies are not reclassified as transaction rollback
failures. Existing [modify coverage](../../crates/torus-bridge/tests/modify_order_tests.rs#L261)
does independently assert exact original order and balance values after an
insufficient-margin rejection on both scalar and batch Phase-1 paths.

## Delayed CoreWriter requests are attempted once at the due height

The [queue](../../crates/torus-core/src/precompiles.rs#L1153) targets the next
height; [draining](../../crates/torus-core/src/precompiles.rs#L1172) removes that
height's entries and returns their actions. The
[executor loop](../../crates/torus-bridge/src/native_executor.rs#L8226) collects
each result and continues through ordinary business errors. A failed due
request is not automatically retried at a later height. It runs after both
native batches in the [application](../../crates/torus-consensus/src/app.rs#L2286),
so current execution state, not enqueue-time balances, governs its outcome.

The [lockbox contract](../../crates/torus-core/src/precompiles.rs#L1071)
specifically allows insufficient native balance at drain time to fail without
an economic state change and defines `true` as queued. Both the withdrawal
account-margin gate and underlying cash check remain in the ordinary handler
path. A no-op economic rejection still consumes the queued request; it does
not mean the queue itself stays unchanged.

The existing [insufficient-balance fixture](../../crates/torus-bridge/tests/lockbox_queue_tests.rs#L264)
queues a withdrawal with 50 native, changes available native to 30 before drain,
and asserts a failed result plus unchanged native/EVM balances. Its intermediate
balance change is a direct fixture write, not a signed spending transaction.
The [premature-claim fixture](../../crates/torus-bridge/tests/claim_unbonded_tests.rs#L123)
compares the delegation bytes and zero account credit after rejection; the
[CoreWriter claim fixture](../../crates/torus-bridge/tests/claim_unbonded_tests.rs#L239)
checks the accepted selector, no same-block execution and successful next-block
credit. These are source-inspected regressions, not proof of executed tests or
end-to-end application status delivery.

## Useful remaining regression work

Preserve the existing expected-value tests. Extend an ordinary signed
application fixture with a funded successful placement, a valid but unaffordable
member, and another affordable member; assert the exact 20/100 balances,
individual resting rows, two allocated IDs, consumed nonce and dispatch status.
Separately assert a book-rejected funded order's order status and handler result
so future API changes cannot silently merge those meanings.

For mixed actions, specify the established phase order before asserting expected
results. For the queue, combine a normal insufficient-balance or already-filled
order request with a succeeding sibling, assert the sibling's exact effect and
that another drain does not replay either request. Use the real ordinary native
spend before withdrawal drain rather than a direct fixture balance overwrite.
These are integration coverage improvements, not newly counted defects or
claims that the absent integration already fails.

This bounded pass ends without a new finding, runtime reproduction, verified
fix, previous-finding closure or broader correctness certification.
