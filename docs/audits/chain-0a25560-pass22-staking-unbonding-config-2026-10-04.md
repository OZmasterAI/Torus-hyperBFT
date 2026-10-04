# Pass 22 — staking unbonding configuration

Reviewed 2026-10-04 against `perf/item6-phase1` at
`0a255607dbe2227ee47b70638b0198b80eaeb3cd`. Focus: the configured unbonding
period from genesis through the ordinary undelegate/claim lifecycle.

**Reconfirmed existing G01; no new finding.** The genesis schema accepts
`unbonding_period_blocks`, and the shipped devnet file sets 604,800 blocks.
The operational undelegate path still uses the compiled
`UNBONDING_PERIOD = 302,400` blocks, then persists `current_block + period`.
Claims compare current height against that stored release height. Thus users
on a freshly initialized chain can reclaim after the compiled seven-day period
even though the accepted genesis parameter declares fourteen days when measured
at the two-second target. This is the same normal-operation configuration
contract mismatch already reported as G01.

[`ValidatorConstraints`](https://github.com/OZmasterAI/Torus-hyperBFT/blob/0a255607dbe2227ee47b70638b0198b80eaeb3cd/crates/torus-genesis/src/lib.rs#L162)
contains the setting; [`chain_config`](https://github.com/OZmasterAI/Torus-hyperBFT/blob/0a255607dbe2227ee47b70638b0198b80eaeb3cd/crates/torus-genesis/src/lib.rs#L503)
projects only a subset of validator parameters. [`StakingManager::undelegate`](https://github.com/OZmasterAI/Torus-hyperBFT/blob/0a255607dbe2227ee47b70638b0198b80eaeb3cd/crates/torus-economics/src/staking.rs#L142)
uses the fixed constant, and the native handler routes ordinary actions to
that manager at [`native_executor.rs`](https://github.com/OZmasterAI/Torus-hyperBFT/blob/0a255607dbe2227ee47b70638b0198b80eaeb3cd/crates/torus-bridge/src/native_executor.rs#L4374).

The prior report's regression expectation remains appropriate: initialize from
a genesis with a non-default period, execute a signed undelegation, and assert
release height and pre-maturity rejection using that configured value. This
pass did not rerun such a regression or independently prove historical chain
state. It records G01 as still open, without a second issue number.

## Limits

Static source comparison only. No Rust test, genesis initialization, chain
execution or claim transaction was run. No source changes were made.
