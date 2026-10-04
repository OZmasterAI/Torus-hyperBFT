# Pass 24 — staking claim atomicity

Reviewed 2026-10-04 against `perf/item6-phase1` at
`0a255607dbe2227ee47b70638b0198b80eaeb3cd`. Focus: the production
ClaimUnbonded and ClaimRewards handlers and the lower-level unbonding helper.

**No new finding.** The production `ClaimUnbonded` path uses
[`claim_unbonded`](https://github.com/OZmasterAI/Torus-hyperBFT/blob/0a255607dbe2227ee47b70638b0198b80eaeb3cd/crates/torus-economics/src/staking.rs#L240),
which gathers matured rows, checked-adds their amounts, computes the account
balance, and writes all delegation-row changes and account credit through one
`atomic_write`. An empty matured set returns before writes. The actual user
handler routes through this operation at
[`exec_claim_unbonded`](https://github.com/OZmasterAI/Torus-hyperBFT/blob/0a255607dbe2227ee47b70638b0198b80eaeb3cd/crates/torus-bridge/src/native_executor.rs#L7879).

`claim_rewards` still calls balance credit and pending-reward deletion as
separate manager operations. In production these execute against the block's
native overlay, and block state is flushed together; the generic manager method
alone does not make the two writes atomic when used against a direct backend.
The legacy `process_unbonding` helper also separately credits the account and
updates/deletes a delegation row, but repository call search found no production
callsite—only manager tests. This is a boundary on what the static manager
review proves, not a newly identified production failure.

The pass-10 action review already traced actual claim handlers and tests,
including partial and multi-validator claims. This round independently
rechecked the current source path and leaves that conclusion unchanged.

## Limits

No write-failure injection, Rust tests or production block execution was run.
No source changes were made.
