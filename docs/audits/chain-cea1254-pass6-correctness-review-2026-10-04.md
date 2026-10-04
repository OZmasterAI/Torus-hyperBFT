# Torus pass 6 — correctness and regression-coverage review

Date: 2026-10-04. Branch: `merge/item6-sync2`. Revision: `cea1254e34625e6b09c58f794de8793b5c12713c`.

The user requested two further GPT-6.1-sol agents after the interrupted fifth pass. This pass assigned different, bounded scopes: ordinary persistence/restart correctness and trading/accounting regression coverage. It did not resume the blocked certificate or input-boundary analyses. The coordinating review checked the principal source/test observations below.

This review does **not add new numbered defect findings** or close F01–F31. Its contribution is checking the accuracy of earlier claims, identifying existing safeguards and meaningful test gaps, and correcting outdated descriptions. Tests were read, not run: Cargo/rustc remain unavailable. No toolchain installation, production changes, live-chain actions, branch changes, commits, or pushes occurred.

## Detailed reviews

- [Persistence and restart review](chain-cea1254-pass6-persistence-review-2026-10-04.md): F18, F19, F21, F30, F31; source qualifications and concrete regression assertions.
- [Accounting and trading review](chain-cea1254-pass6-accounting-review-2026-10-04.md): partial fills, cancellation/modification, transfer accounting, decimal scaling, fee expectations, and path-equivalence limits.

The [pass-five status](chain-cea1254-pass5-status-2026-10-04.md) remains incomplete. This pass does not turn its unverified lead into a finding or imply completion of its abandoned scopes. Earlier source findings remain in [pass 1](chain-cea1254-2026-10-04.md), [pass 2](chain-cea1254-pass2-2026-10-04.md), [pass 3](chain-cea1254-pass3-project-wide-2026-10-04.md), and [pass 4](chain-cea1254-pass4-missed-issues-2026-10-04.md).

## Corrections and qualifications

### Cancellation coverage exists, including pending stops

The older parity document lists cancel-all leaving pending-stop margin locked. Current [cancel-all execution](../../crates/torus-bridge/src/native_executor.rs#L7118) calls a shared helper that removes resting orders and pending stops and includes their reservations in the released amount. The existing [pending-stop cancellation test](../../crates/torus-bridge/tests/market_order_margin_tests.rs#L1092) checks stop-only and mixed books, market-specific and all-market cancellation, restored balances, and save/reload persistence.

Likewise, [partial-fill then cancel](../../crates/torus-bridge/tests/maker_margin_release_tests.rs#L134), truncation-dust, and self-trade-prevention cancellation tests already cover basic reservation reconciliation. Those ordinary cases should not be called untested or newly defective solely from the historical note. This is a source/documentation correction, not a fix implemented or runtime-verified during this session.

### Genesis fallback is logged and may come from the config file

F19 concerns acceptance of changed configuration for existing state, not a complete absence of diagnostics. [Config-file defaults](../../crates/torus-node/src/main.rs#L287) can populate the genesis field; omission must therefore mean absence from both CLI and config. Default fallback is logged, and state-hash activation changes warn.

The pass-three consolidated report now states these conditions explicitly and replaces the ambiguous “silently” wording. Its fix guidance also preserves supported, explicit activation upgrades: rejecting every changed field indiscriminately would interfere with the planned state-hash activation workflow. The persistence review also identifies a relaunch-runbook statement that genesis is ignored on later restarts; current source skips initialization but still reads its configuration. No change was made to node startup itself.

### Snapshot copying and verification coverage are different questions

F21 remains a limitation of what the verification commitment covers. Snapshot creation copies the database column families, so the omitted root coverage does not by itself mean an intact snapshot loses those rows on normal restoration. The existing [snapshot lifecycle test](../../crates/torus-integration-tests/tests/snapshot_dos_keys.rs#L50) seeds EVM accounts/storage and verifies their round trip; the neighboring metadata test checks a changed root. Neither establishes complete native-state verification coverage. The persistence report specifies additional normal round-trip and commitment-coverage assertions without claiming ordinary restoration already loses all omitted state.

### Existing crash/prune test names do not establish the required coverage

[`crash_replay_runs_after_pruning`](../../crates/torus-consensus/src/app.rs#L7434) inserts prune metadata and replays empty blocks. It does not run `StatePruner` or remove a nonempty body before application, so it does not refute F31.

The trie tests cover completed commits/reopen and explicit resync. A targeted test is still needed for restart after the account/applied-marker write but before separate mirror resync. F30's pass-four database-write link was corrected from line 517 to the actual [write at line 480](../../crates/torus-state/src/incremental.rs#L480). This was a clerical audit-reference correction; no persistence behavior changed.

## Accounting-test contracts need precise assertions

### Some typed-transaction fee tests encode legacy assumptions

[Bridge transaction helpers](../../crates/torus-bridge/tests/bridge_tests.rs#L65) construct signed EIP-1559 transactions. Yet [sender-debit expectations](../../crates/torus-bridge/tests/bridge_tests.rs#L577) use `gas_used × max_fee`, and [beneficiary expectations](../../crates/torus-bridge/tests/bridge_tests.rs#L621) use `gas_used × (max_fee − base_fee)` even when the priority cap is smaller. Those assertions are consistent with the previously reported F17 transaction-type defect rather than evidence against it.

A regression should make the maximum fee clearly exceed `base_fee + priority_cap`, then compare sender debit, receipt effective price, beneficiary credit, and RPC projection with the intended typed-transaction rules. These tests need their expectations corrected alongside the implementation; no test execution or expectation change was performed here.

### Allocation conservation is not whole-ledger conservation

The integration test named [full fee pipeline conservation](../../crates/torus-integration-tests/tests/fee_flow.rs#L42) supplies a synthetic fee amount and invokes splitting/distribution helpers. The bridge fee tests execute transactions but commit through `BlockCommitter`, without the application's later native reward-distribution phase. Neither alone reconciles the combined path implicated in F02/F17.

The missing assertion is a whole-ledger reconciliation through the actual application path, including sender charges, credited accounts, reward liabilities, and cumulative burn. Pure splitter arithmetic remains useful coverage; it answers a narrower question.

### Basic lockbox scaling and pending-credit accounting already have coverage

The [deposit/transfer test](../../crates/torus-evm/tests/evm_tests.rs#L1189) explicitly balances EVM value, native value, queued credit, and burned sub-unit dust. Bridge queue tests also exercise the delayed native leg. These zero-fee cases should not be labeled missing decimal-conversion or basic queue-conservation coverage. They do not replace a combined fee-inclusive reconciliation regression.

### Equivalence and tolerance assertions must match the intended contract

Sequential scalar calls are not a universal reference for `execute_batch`: batch preparation groups some non-order actions before matching. Differential tests must compare the same specified phase ordering rather than require identical results for intentionally different action semantics.

The accounting review also distinguishes documented observations from enforced bounds. A test comment reporting entry-rounding drift of at most six raw units is narrower than its assertion allowing 100 raw units. This is a test-description/tolerance qualification, not evidence of a new live accounting defect. See the companion report for exact test anchors and suitable assertions.

## Follow-up regression work

| Area | Useful next assertion |
| --- | --- |
| Bytecode persistence, F18 | Exact deployed bytes, hash, and code-length behavior survive commit and reopen |
| Restart configuration, F19 | Omitted/changed configuration is handled consistently while explicit supported upgrades remain possible |
| Snapshot coverage, F21 | Full normal state round trip is checked separately from what the verification commitment certifies |
| Mirror/application frontier, F30 | Restart reconstructs a root consistent with account state at the durable applied frontier |
| Pruning/application frontier, F31 | Pruning plus restart preserves replay of nonempty unapplied blocks |
| Typed fees and distribution, F02/F17 | Application-level sender, recipient, reward-liability, receipt, and burn deltas reconcile |
| Trading paths | Assertions compare equivalent phase ordering and include required persisted rows, not only selected in-memory totals |

These are recommended production-code regressions, not tests added or executed in this pass. Earlier Python models remain illustrative and were not rerun merely because audit prose changed. Final artifact validation covers local links, whitespace, unchanged audited revision, and the absence of tracked/staged production changes.
