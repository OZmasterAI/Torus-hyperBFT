# Pass 7 — independent Astra ordinary accounting/lifecycle review

Date: 2026-10-04. Checkout: `/home/oz/projects/Torus-hyperBFT`, branch
`merge/item6-sync2`, HEAD `cea1254e34625e6b09c58f794de8793b5c12713c`.

**No additional production defect is promoted.** This review independently
checked ordinary valid placement, partial fills, modification/cancellation,
position settlement and persistence boundaries, fee vocabulary, and CoreWriter
return/event semantics. It adds concrete safeguards and regression-contract
qualifications; it does not close or repeat F01–F31 or the supplements.

Prior material consulted: chain passes 1/2, project-wide/trading pass 3, pass-4
missed issues, pass-6 correctness/accounting, historical Astra rounds 1/2, and
`docs/parity-audit-fixes-s515.md` including its deferred items. Historical paths
and claims were treated as data and compared with this checkout. No applicable
AGENTS.md was found. The parent coordinates Torus persistence.

This was source-only. **Tests were read, not executed.** Cargo/rustc are absent;
no toolchain was installed, model executed, network/live-chain action taken,
production source changed, or Git mutation performed. No exploit, malicious
input, adversarial consensus schedule, or blocked pass-five scope was developed.

## 1. PnL and reservation updates share a balance authority

**Classification:** confirmed source safeguard, not a new finding. **Priority:**
retain these regressions during accounting changes.

The direct [fill path](../../crates/torus-core/src/position.rs#L362) calls the
shared [pure transition](../../crates/torus-core/src/position.rs#L482), stores or
deletes the position, and credits only the returned closing-PnL event. The cached
path delegates to that same transition; the bridge's
[apply_fill_via_caches](../../crates/torus-bridge/src/native_executor.rs#L5775)
credits its returned PnL through `BalanceCache`, which also owns the subsequent
reservation releases. It does not credit the backend directly and then overwrite
that credit from an older cached balance.

This is more than a structural parity argument. The existing
[pnl_credit_survives_later_margin_release_in_same_batch](../../crates/torus-bridge/tests/position_cache_exec_tests.rs#L158)
opens a short, realizes +10 on its close, then partially fills a new buy in the
same batch. It asserts `available = 10010 - 4.25`, `order_margin = 4.25`, and a
new long of size 1 at 85. Calling ordinary realized-PnL-plus-release composition
untested would overlook this exact-value regression.

The [FillEffect regression](../../crates/torus-core/tests/position_cache_tests.rs#L162)
checks independently specified starting sizes and closing PnL for an increase,
partial close, flip and full close on both cached and direct paths. Full closure
[flushes a tombstone](../../crates/torus-core/tests/position_cache_tests.rs#L245),
and [opening then closing inside one batch](../../crates/torus-core/tests/position_cache_tests.rs#L266)
leaves no position row. These are useful checks against resurrecting a closed
position or treating previously credited partial-close PnL as another payout on
final closure. `Position.realized_pnl` is not a second cash liability: the partial
close has already credited its event; full close pays only its remaining close
component and deletes the position.

**Conditions/limits:** ordinary readable state, representable arithmetic and
the same fill sequence. Shared transition code does not prove its arithmetic
correct by itself; historical weighted-entry truncation remains the Astra
round-two assignment-7 limitation. The explicit-value tests improve that evidence
but do not cover arbitrary rounding or whole-chain supply accounting.

**Concrete next assertion:** extend the existing same-batch fixture through
cancel of the new remainder, close of the final long, context save/reload, and a
subsequent withdrawal. Assert each account's expected balance, absent closed
position, zero reservation from cancelled orders, and both sides' signed sizes.
Use real fills to obtain the PnL. This supplements the already-present fixture;
it is not a request to recreate its basic PnL/release test.

## 2. Partial fill, modify and cancel preserve the intended reservation ledger

**Classification:** negative result confirming pass 6. **Priority:** preserve
existing exact-unit tests; no new repair proposed.

The [reservation function](../../crates/torus-bridge/src/native_executor.rs#L5860)
defines the amount owed by a remaining order; the
[maker aggregation](../../crates/torus-bridge/src/native_executor.rs#L5927)
combines consumed quantity before comparing it with the final remainder.
This matters when multiple takers fill the same maker in one batch: independently
releasing rounded per-fill fractions would be a different algorithm.

[Modify](../../crates/torus-bridge/src/native_executor.rs#L7295) checks ownership,
positive values, changed-price tick alignment, requested lot size, noncrossing
price and reduce-only allowance before changing the book. It uses new minus old
reservation and marks the book dirty. A quantity-only decrease can retain time
priority through [book modification](../../crates/torus-core/src/order_book.rs#L1489).
[Individual cancellation](../../crates/torus-bridge/src/native_executor.rs#L7064)
releases the remaining order's reservation; the shared
[cancel-all helper](../../crates/torus-bridge/src/native_executor.rs#L7129) also
accounts for pending stops.

The [raw-unit partial-fill/cancel test](../../crates/torus-bridge/tests/maker_margin_release_tests.rs#L168)
and [partially filled non-pool sell/modify/cancel test](../../crates/torus-bridge/tests/account_margin_tests.rs#L1289)
already target nontrivial release shapes. Pass 6 correctly identifies the old
pending-stop cancel-all note as stale. Individual pending-stop cancellation by ID
remains explicitly deferred and is not rediscovered here.

**Conditions/limits:** use the specified batch phase ordering and stable effective
margin configuration. Negative `available` alone is not proof of a leak: the
documented closing-order/UPnL policy permits it while separately tracking
`order_margin`. A comparison must reconcile both fields. Batch/scalar outcomes
can intentionally differ under D2/D6 in the
[deferred policy](../parity-audit-fixes-s515.md#L112).

**Concrete regression assertion:** after each lifecycle step, reconcile the sum
of live-order and pending-stop reservations with `order_margin`, then verify that
save/reload preserves order IDs, remaining quantities and those balances. Compare
parallel settlement with serial **batch** settlement under the same phase policy,
rather than assuming a scalar action loop is universally equivalent.

## 3. Settlement flush protection is real but does not subsume historical faults

**Classification:** safeguard/coverage qualification. **Priority:** retain when
modifying resident-state or settlement code.

The [batch tail](../../crates/torus-bridge/src/native_executor.rs#L4776) flushes
positions, balances and cumulative volumes and sets `fatal_error` on failure.
Triggered stops run only when that error is absent and after those caches have
been flushed. The [application check](../../crates/torus-consensus/src/app.rs#L2255)
stops the block before later phases/commit when the fatal latch is present.
Therefore it would be inaccurate to say every native settlement persistence
failure is silently accepted.

This does not contradict F04, the pass-4 CoreWriter early-error extension, or
pass-6 persistence analysis. Individual read/fill errors and the ignored drain
Result have different callers. An ordinary successful overlay flush also does
not establish crash-safe durable-root synchronization. No new fault schedule was
constructed in this pass.

**Concrete assertion:** preserve the existing cache tombstone/exact-byte tests;
for a successful normal block, reload balances and positions from saved state and
compare them with expected economic values as well as another execution mode.
The current [owned-write test](../../crates/torus-core/tests/position_cache_tests.rs#L356)
already checks persisted Borsh byte compatibility and should not be dismissed as
only an in-memory assertion.

## 4. CoreWriter returns acknowledge queued work; fills are later observations

**Classification:** API-contract qualification; no new finding. **Priority:**
clarify assertions alongside the existing F22 repair.

[CoreWriter](../../crates/torus-core/src/precompiles.rs#L878) enqueues work for the
next block. `cancelOrder` returns true after enqueue, while `cancelAll` returns a
literal zero with an explicit comment that the count is determined at execution.
Staking claim returns likewise use an enqueue-time zero placeholder. The
[provider](../../crates/torus-evm/src/precompile_provider.rs#L188) translates this
result into EVM return/revert; it does not append a native completion event.
The [drain](../../crates/torus-bridge/src/native_executor.rs#L8133) later executes
the queued native action and collects separate `NativeActionResult` values.

Consequently a successful EVM receipt establishes successful enqueue, not a fill
or a completed cancellation. That distinction is inherent in this delayed API,
not a newly found atomicity bug. The alleged order ID is different: its mismatch
with the actual allocated ID is already F22 and remains separate from bool/zero
acknowledgements. This review neither repairs nor recounts it.

The native result vocabulary is also broader than a fill status. Scalar
[placement](../../crates/torus-bridge/src/native_executor.rs#L7007) and sequential
[batch settlement](../../crates/torus-bridge/src/native_executor.rs#L5287) can return
`ok` after a book outcome such as an unfilled IOC cancellation. The
[funnel](../../crates/torus-bridge/src/native_executor.rs#L5809) separately classifies
Filled/Resting/Rejected/Cancelled/PendingTrigger. Moreover,
[BlockActionStatus](../../crates/torus-state/src/action_status.rs#L1) explicitly
records executed versus skipped, not achieved trading intent. I found no basis
to relabel those distinct predicates as a new production defect.

**Concrete assertion:** for an ordinarily funded valid limit order, check queue
presence at block h, absence of immediate native order/position mutation, actual
order/fill state after h+1, and queue removal. Check cancellation by the actual
resting ID separately from enqueue success. For a valid IOC with no liquidity,
assert zero fills and released reservation without requiring it to revert its
already-completed enqueue transaction. A future action-result event/lookup should
explicitly identify its phase; no completion event is implied by current return
bytes. Existing [queue tests](../../crates/torus-core/tests/precompile_tests.rs#L485)
check enqueue and next-height contents, not the full lifecycle assertion above.

## 5. Fee reconciliation must count actual monetary movements

**Classification:** scope qualification; F02/F17 retain their provenance and
priority. No additional fee defect.

The native executor's result `gas_used` is not itself a collateral deduction.
`total_native_fees` is [initialized to zero](../../crates/torus-bridge/src/native_executor.rs#L2578),
and the reviewed production source has no increment to it. The fee-distribution
entry point [combines that counter with EVM revenue](../../crates/torus-bridge/src/native_executor.rs#L8251).
The older [margin design](../plans/account-level-margin-f1-impl.md#L99) already
records that no native balance is debited for fees. Hence charging a fictional
`1000` native units for a placement's result gas would make a proposed ledger
regression incorrect. This is a description of the current accounting contract,
not a claim that a native trading-fee product requirement is satisfied.

Pass 6's typed-fee expectation correction is sound: a signed typed-transaction
fixture must compare actual sender charge and receipt effective price with its
priority/maximum fee semantics. An application-level ledger regression must also
include distribution/reward liabilities and the chosen burn policy; helper-only
splitter conservation is insufficient. Those are existing F02/F17 regressions,
not new findings derived from this pass.

## Conclusion and evidence boundary

The useful incremental result is narrowing claims: ordinary PnL/release
composition, explicit fill effects, position tombstones, and cache-flush fail-stop
handling already have meaningful source safeguards/tests. CoreWriter enqueue
success, native action execution, order disposition and fills must remain
separate assertions. The broader deposit → actual fills/close → withdrawal ledger
test remains worthwhile, but should build on this coverage rather than claim it
is absent. No runtime correctness certification, production fix, or resolution
of historical findings is claimed.
