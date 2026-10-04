# Pass 28 — fee split allocation and rounding

Reviewed 2026-10-04 against `perf/item6-phase1` at
`0a255607dbe2227ee47b70638b0198b80eaeb3cd`. Focus: the scheduled burn,
validator, treasury and development-pool percentages and integer rounding.

**No new allocation defect found.** The production fee distributor computes
burn, validator and treasury amounts from basis points, then assigns the
remaining amount to the development pool. The split therefore conserves the
input amount exactly despite floor division in the first three buckets. At the
start schedule those three ratios total 5,500 bps; at the end they total 7,500
bps, so the remainder is nonnegative throughout the interpolation. The explicit
`FEE_START_DEV_POOL_BPS`/`FEE_END_DEV_POOL_BPS` do not independently determine
the production remainder; it absorbs rounding and any schedule mismatch.

[`distribute_block_fees`](https://github.com/OZmasterAI/Torus-hyperBFT/blob/0a255607dbe2227ee47b70638b0198b80eaeb3cd/crates/torus-economics/src/rewards.rs#L24)
implements the schedule and remainder. The companion pure
[`FeeSplitter::split_fees`](https://github.com/OZmasterAI/Torus-hyperBFT/blob/0a255607dbe2227ee47b70638b0198b80eaeb3cd/crates/torus-economics/src/rewards.rs#L328)
uses the same arithmetic. `lerp_bps` clamps epochs at or beyond the transition
length to endpoint rates. The declared genesis percentages are copied into
`ChainConfig`, but the operational distributor uses compiled schedule
constants; the prior economics/config reviews already noted that accepted
configuration can differ from effective production behavior.

This calculation pass does not establish that the correct fee amount reaches
the splitter, that burned fees are reflected in every supply statistic, or
that all credits are atomic with other block operations. Those are separate
cross-path questions for the next rounds.

## Limits

No numerical Rust test or production fee-distribution execution was run. The
conservation conclusion follows from source arithmetic and the configured
endpoint sums. No source changes were made.
