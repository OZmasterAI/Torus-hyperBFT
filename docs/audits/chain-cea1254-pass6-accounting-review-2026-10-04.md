# Ordinary trading/accounting correctness and regression review — pass 6

Reviewed on 2026-10-04 in `/home/oz/projects/Torus-hyperBFT`, branch
`merge/item6-sync2`, HEAD `cea1254e34625e6b09c58f794de8793b5c12713c`.

**Result: no additional production defect is promoted.** This bounded source review
finds useful corrections to coverage claims and a stale deferred-item note, plus
focused regression recommendations on ordinary valid orders and transfers. It
does not add to F01–F31 or the earlier supplements, close their findings, or
establish that the trading/accounting engine is fully correct.

The comparison material was the first/second chain reports, project-wide and
trading pass 3, pass-4 missed issues, historical Astra rounds 1/2, and
`docs/parity-audit-fixes-s515.md`. Existing fee ownership/type issues, weighted-entry
rounding, CoreWriter order identity, stop conversion, market initialization and
margin limitations retain their earlier provenance. No exploit sequences,
malicious schedules or blocked consensus/input-boundary tasks were developed.

Cargo/rustc are unavailable on PATH. **Tests below were read, not run.** No Rust
build, model, live-chain action, network call, production edit, toolchain install,
commit, push or branch change was performed. The only addition is this report;
the existing untracked audit artifacts were retained. Torus persistence is handled
by the coordinating agent.

## 1. Existing fee tests assert an inconsistent typed-transaction contract

**Classification:** correction to regression expectations for existing F17;
coverage qualification for F02, not a new production finding.

The [helper](../../crates/torus-bridge/tests/bridge_tests.rs#L65) says it builds a
signed EIP-1559 transfer and constructs
[`TxEip1559`](../../crates/torus-bridge/tests/bridge_tests.rs#L102). However,
[`gas_accounting_sender_balance_deducted`](../../crates/torus-bridge/tests/bridge_tests.rs#L577)
expects `gas_used * max_fee_per_gas`, even with zero priority fee and a block base
fee below the cap. Similarly,
[`gas_accounting_tip_to_proposer`](../../crates/torus-bridge/tests/bridge_tests.rs#L621)
expects `gas_used * (max_fee - block_base_fee)`, despite the distinct signed
priority cap. Those are the legacy-style expectations identified in the earlier
typed-envelope finding. Presence of these tests cannot establish correct typed
fee behavior; they encode that disputed behavior as the expected result.

**Ordinary invariant:** for a successfully executed typed transaction, the
sender's gas charge and its receipt must agree on the effective price
`min(max_fee, base_fee + priority_cap)`. Value movement is accounted separately.
The direct EVM beneficiary component must agree with the selected fee ownership
policy.

The same file's
[`gas_accounting_base_fee_burned`](../../crates/torus-bridge/tests/bridge_tests.rs#L667)
sums sender, recipient and proposer balances. This is useful local EVM accounting
coverage. It invokes
[`BlockCommitter::commit_block`](../../crates/torus-bridge/src/committer.rs#L30),
whose arguments are the block, EVM bundle and receipts. It does not execute the
application's native fee-distribution tail at
[`app.rs:2303`](../../crates/torus-consensus/src/app.rs#L2303). Therefore its
conservation assertion does not contradict F02's composition issue.

**Recommendation:** update the typed gas expectations as part of F17's repair,
with a fee cap distinctly above `base_fee + priority_cap`, and compare actual
sender debit, receipt effective price and beneficiary delta. Preserve separate
tests for legacy transactions. Execute a real signed transaction through the
complete application fee path and count account balances, reward liabilities and
the specified burn exactly once, including a reverted transaction. These tests
must use the adopted Torus burn/reward policy rather than importing the direct
EVM-only burn rule as the full-chain policy.

**Evidence limit:** no assertion was executed. This is a source-established
expectation mismatch and caller-coverage distinction, not a new reproduction of
F02/F17.

## 2. Transfer conservation coverage is substantial, but its economic scope is specific

**Classification:** negative result plus a focused composition recommendation.

The ordinary 8↔18 decimal conversion and queued-credit accounting are already
covered in source:

- [Lockbox native round-trip/dust tests](../../crates/torus-integration-tests/tests/lockbox_e2e.rs#L186)
  assert multiplication by `10^10`, exact native round trips and the EVM remainder.
- [Bridge deposit→next-block drain](../../crates/torus-bridge/tests/lockbox_queue_tests.rs#L185)
  uses real EVM execution, includes sub-unit dust, then checks the native credit
  and combined value after drain.
- [EVM pending-credit test](../../crates/torus-evm/tests/evm_tests.rs#L1185)
  checks the intermediate equation `EVM + native + queued deposit credit + burned
  dust = before`, so pending-credit reconciliation is not absent.
- [Queued withdrawal margin test](../../crates/torus-bridge/tests/lockbox_queue_tests.rs#L381)
  exercises both sides of the account transfer-margin limit.
- [Replay-before-commit test](../../crates/torus-bridge/tests/lockbox_queue_tests.rs#L350)
  discards one execution result, commits replay, and checks exactly one credit.
  This is a useful dropped-result test; it is not an actual process restart or
  power-loss experiment.

The [bridge EVM helper](../../crates/torus-bridge/tests/lockbox_queue_tests.rs#L101)
sets transaction gas price to zero, and its
[block helper](../../crates/torus-bridge/tests/lockbox_queue_tests.rs#L140) sets base
fee to zero. The equivalent
[EVM helper](../../crates/torus-evm/tests/evm_tests.rs#L1060) is explicitly named
`zero_fee_block_cfg`. Meanwhile,
[`test_full_fee_pipeline_conservation`](../../crates/torus-integration-tests/tests/fee_flow.rs#L41)
creates `total_fees` itself and calls splitting/distribution directly. It checks
allocation conservation, recipient credits and reward allocation, but no signed
EVM transaction pays that synthetic amount.

The [profit round-trip test](../../crates/torus-integration-tests/tests/lockbox_e2e.rs#L131)
also injects native profit with `fund_native`; it does not obtain the profit from
two-sided fills and closing settlement.

**Ordinary invariant/recommendation:** add a modest composed fixture using valid
listed markets, usable marks and adequately funded independent traders. Deposit,
rest and partially fill an order, close positions through actual fills, release
or cancel any remainder, then withdraw in a subsequent block. Include the
counterparty, `available + order_margin`, actual fees, reward liabilities and
explicit dust/burn in the ledger. Check queued deposits as liabilities before
drain and spendable native collateral after drain, without double counting.
This joins already-covered components and gives the synthetic-profit test an
actual settlement counterpart; it does not imply a newly found transfer bug.

**Evidence limit:** these are coverage observations about the cited fixtures.
The review does not claim that all other integration tests omit such composition.

## 3. Exact partial-fill/cancellation accounting is already tested

**Classification:** negative result; no new ordinary reservation leak established.

The shared [reserve function](../../crates/torus-bridge/src/native_executor.rs#L5860)
and [maker release computation](../../crates/torus-bridge/src/native_executor.rs#L5908)
use differences between reservation at pre/post remaining quantities. Cancellation
releases the reservation for the remainder; modification computes the difference
between old/new reservations at
[`exec_modify_order`](../../crates/torus-bridge/src/native_executor.rs#L7295).

Existing tests meaningfully exercise this contract:

- [Partial maker fill then cancel](../../crates/torus-bridge/tests/maker_margin_release_tests.rs#L134)
  and [raw-unit dust variant](../../crates/torus-bridge/tests/maker_margin_release_tests.rs#L168).
- [Multiple takers consuming one maker](../../crates/torus-bridge/tests/maker_margin_release_tests.rs#L281),
  including aggregate-release behavior.
- [Reservation/modify/cancel sequence](../../crates/torus-bridge/tests/modify_order_tests.rs#L314).
- [Partially filled non-pool sell, then modify and cancel](../../crates/torus-bridge/tests/account_margin_tests.rs#L1289).
- [Parallel-settlement A5 regression](../../crates/torus-bridge/tests/parallel_settle_tests.rs#L579),
  with balances restored to the raw unit.

**Ordinary invariant:** outstanding order margin equals the reservations still
owed by live orders/stops; a fully cancelled/filled order leaves none of its
reservation stranded. The cited source/tests directly address that invariant.

**Recommendation:** retain these exact assertions while fixing unrelated margin
or market initialization issues. A new regression should fill an uncovered
composition, not simply restate the existing partial-fill/cancel test.

## 4. State equivalence needs a correctly scoped reference

**Classification:** semantic/coverage qualification, not a new mismatch finding.

[`execute_batch`](../../crates/torus-bridge/src/native_executor.rs#L4095) documents
that Phase 1 runs all non-placement actions before order preparation/matching.
The actual [Phase-1 loop](../../crates/torus-bridge/src/native_executor.rs#L4312)
records placement indices and executes other actions immediately. Thus a scalar
loop over the original mixed action list is not a universal reference for this
batch API. A trailing withdrawal or modification in that list is checked against
Phase-1 state, before the batch's placements settle. Returned result indices alone
do not establish execution order.

Existing equivalence tests have real strength within their stated reference:
[parallel settlement](../../crates/torus-bridge/tests/parallel_settle_tests.rs#L119)
compares stored CF bytes, roots, trades, results and counters;
[resident restart/reload](../../crates/torus-bridge/tests/resident_books_tests.rs#L334)
compares a script containing partial fills, modifies and pending stops;
[engine margin shapes](../../crates/torus-bridge/tests/engine_parallel_tests.rs#L1326)
compare worker configurations, including a withdrawal;
[golden pipelined execution](../../crates/torus-bridge/tests/perf_equivalence_golden.rs#L129)
compares persisted state/digests while carrying overlays and resident rows.

**Ordinary invariant/recommendation:** keep serial batch execution as the reference
for the parallel/engine modes. Add explicit expected-result checks for a normal
mixed placement/modify/cancel/withdraw batch, so Phase-1 policy is pinned as well
as implementation parity. Comparing identical modes can preserve the same
unexpected policy outcome on both sides. Use scalar comparisons only for shapes
whose action ordering and margin-reservation rules agree with the batch policy.
The resident test's reset of the holder is simulated memory loss, not reopening
RocksDB or a full application restart; do not label it broader recovery proof.

## 5. Two documentation/assertion qualifications

**Pending-stop cancellation:** the deferred note at
[`parity-audit-fixes-s515.md:204`](../parity-audit-fixes-s515.md#L204) says
`cancel_all` does not release pending-stop margin. Current
[`cancel_orders_and_stops`](../../crates/torus-bridge/src/native_executor.rs#L7129)
takes pending stops and adds their reservations to the release. The explicit
[regression](../../crates/torus-bridge/tests/market_order_margin_tests.rs#L1092)
checks stop removal, exact restored balance, and save/reload across execution
paths and market-specific/all-market targets. Recommend retiring that stale note.
This does not establish individual cancellation of a stop by ID.

**Rounding tolerance:** the seeded conservation test's
[comment](../../crates/torus-bridge/tests/account_margin_tests.rs#L1341) describes
historically measured drift `<= 6 raw`, but its actual
[assertion](../../crates/torus-bridge/tests/account_margin_tests.rs#L1433) allows
`<= 100 raw`. The native SCALE makes these `0.00000006` and `0.000001` units,
respectively. Neither is a universal bound on weighted-entry rounding. The
[position transition](../../crates/torus-core/src/position.rs#L514) truncates the
weighted entry, and the earlier Astra report already records fragmentation
dependence. This review adds no new production rounding finding.

Recommend stating the enforced tolerance separately from a historical observation,
then adding focused valid fractional fill/close cases with an independently
specified arithmetic expectation. Compare cached/direct implementations for the
same fill sequence; do not assert that arbitrary fragmentation must give identical
PnL while the chosen accounting representation intentionally discards remainder.

## Evidence limits and next work

All conclusions above come from source and test inspection at the stated HEAD.
No passing Rust suite or observed ledger execution is claimed. No production
finding or historical issue has been marked resolved. The useful next work is
correcting typed-fee regression expectations, executing fee-inclusive application
ledger tests, adding explicit mixed-batch policy assertions, and updating the
stale pending-stop note and rounding-bound wording.
