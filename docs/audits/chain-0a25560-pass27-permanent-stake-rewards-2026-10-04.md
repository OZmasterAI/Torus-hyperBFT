# Pass 27 — permanent-stake rewards and governance unlock

Reviewed 2026-10-04 against `perf/item6-phase1` at
`0a255607dbe2227ee47b70638b0198b80eaeb3cd`. Focus: permanent-stake reward
accrual over an epoch and the governance-only principal unlock path.

**No new finding.** Permanent rewards use a fixed 500-bps annual rate and the
same two-second-derived blocks-per-year denominator as validator inflation.
The reward is credited directly to liquid account balance and therefore does
not compound into the permanent stake amount. At governance unlock, the record
is reduced or removed, any pending rewards are claimed, then principal plus
rewards are credited. This is staged in the block's state overlay during
production execution; no live balance transfer occurs outside that transaction
path.

[`distribute_permanent_staking_rewards`](https://github.com/OZmasterAI/Torus-hyperBFT/blob/0a255607dbe2227ee47b70638b0198b80eaeb3cd/crates/torus-economics/src/rewards.rs#L130)
uses `stake * 500 * blocks_in_epoch / (blocks_per_year * 10,000)` and skips
zero positions. [`governance_unlock_permanent_stake`](https://github.com/OZmasterAI/Torus-hyperBFT/blob/0a255607dbe2227ee47b70638b0198b80eaeb3cd/crates/torus-economics/src/staking.rs#L346)
checks positive amount and principal bounds before updating the lock and
crediting. The accepted genesis `annual_rate_bps` still does not parameterize
this fixed constant; that is already part of configuration candidate G01.

The reviewed path does not show an additional principal duplication or
premature user-callable unlock. Governance execution and storage-failure
atomicity remain outside this static source check.

## Limits

No epoch-boundary execution, governance vote, storage-fault injection or Rust
test was run. No source change or new issue number.
