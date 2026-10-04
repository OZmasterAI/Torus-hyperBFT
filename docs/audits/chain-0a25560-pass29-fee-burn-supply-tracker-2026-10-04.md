# Pass 29 — production fee burn and supply reporting

Reviewed 2026-10-04 against `perf/item6-phase1` at
`0a255607dbe2227ee47b70638b0198b80eaeb3cd`. Focus: whether the fee split's
burn and treasury amounts update the cumulative supply tracker exposed to RPC.

**Reconfirmed historical ECON-FIND-22; no new finding number.** The production
path calls `RewardDistributor::distribute_block_fees`. It computes a `burn`
amount and excludes it from the balance credits, so that portion is economically
removed from the fee proceeds. However, it never calls
`FeeSplitter::execute_burn`, which is the helper that increments
`SupplyTracker.cumulative_burned`. Likewise, treasury is credited directly,
not through `FeeSplitter::credit_treasury`, the helper that increments
`cumulative_treasury`. The RPC treasury-info method reads those tracker fields.
Consequently, normal fee distribution can reduce spendable fee proceeds while
the cumulative burned and treasury values reported by RPC do not include those
flows. `execute_burn` is called from staking slash handling, so the burn field
can reflect slashes while omitting fee burns.

The production route is visible at [`distribute_fees`](https://github.com/OZmasterAI/Torus-hyperBFT/blob/0a255607dbe2227ee47b70638b0198b80eaeb3cd/crates/torus-bridge/src/native_executor.rs#L8653)
through [`RewardDistributor::distribute_block_fees`](https://github.com/OZmasterAI/Torus-hyperBFT/blob/0a255607dbe2227ee47b70638b0198b80eaeb3cd/crates/torus-economics/src/rewards.rs#L24).
The calculated burn is unused after the subtraction at L57-L60. Tracker-mutating
helpers are separate at [`FeeSplitter`](https://github.com/OZmasterAI/Torus-hyperBFT/blob/0a255607dbe2227ee47b70638b0198b80eaeb3cd/crates/torus-economics/src/rewards.rs#L373),
and RPC exposes the stored values at [`torus_get_treasury_info`](https://github.com/OZmasterAI/Torus-hyperBFT/blob/0a255607dbe2227ee47b70638b0198b80eaeb3cd/crates/torus-rpc/src/torus.rs#L1496).
The earlier `research/ECON-AUDIT-3.4.4.md` identifies the disconnected
production tracker as ECON-FIND-22; this round confirms it remains in the
current integration head and does not count it as new.

## Limits

Static callgraph review only. No fee-generating transaction, RPC query against a
running node, or supply reconciliation was performed. No source change.
