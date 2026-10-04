# Pass 26 — validator inflation units and epoch fraction

Reviewed 2026-10-04 against `perf/item6-phase1` at
`0a255607dbe2227ee47b70638b0198b80eaeb3cd`. Focus: stake units, rate basis,
epoch duration and the inflation accumulator.

**No new finding.** The current implementation uses a fixed 500-bps annual
rate for validator stake. For each epoch it multiplies wei-denominated active
stake by that rate and the epoch's block count, then divides by
`BLOCKS_PER_YEAR * 10,000`; the stake unit is preserved as wei and the block
fraction is dimensionless. The denominator derives from the same two-second
target block time. Active validator stakes include self stake and delegated
stake. The current flat-rate implementation should not be conflated with older
research documents describing `200 / sqrt(total_staked_TRS)`; the latter is
not the formula in this source revision.

[`distribute_validator_inflation`](https://github.com/OZmasterAI/Torus-hyperBFT/blob/0a255607dbe2227ee47b70638b0198b80eaeb3cd/crates/torus-economics/src/rewards.rs#L166)
filters to active validators, sorts by address for deterministic rounding,
calculates total emission and assigns the final validator the residual after
floored earlier shares. The constants are defined at
[`types.rs`](https://github.com/OZmasterAI/Torus-hyperBFT/blob/0a255607dbe2227ee47b70638b0198b80eaeb3cd/crates/torus-economics/src/types.rs#L346);
[`total_stake`](https://github.com/OZmasterAI/Torus-hyperBFT/blob/0a255607dbe2227ee47b70638b0198b80eaeb3cd/crates/torus-economics/src/types.rs#L97)
adds self stake and total delegation. At a full year of blocks, the formula
returns nominally 5% of total active stake before integer rounding. Epoch
processing passes `ctx.epoch_length` to the distributor.

The accepted genesis annual-rate fields remain disconnected from the effective
constants, as already recorded under G01. This pass verifies current units and
allocation math; it does not reopen that configuration finding as a separate
inflation defect. The accumulator uses unchecked U256 addition, but this review
did not establish a protocol-reachable state that overflows it.

## Limits

No numeric execution fixture, Rust test, long-horizon supply simulation or
malformed-genesis case was run. This is a static dimensional and call-path
review only.
