# Pass 93 — Jail-vote record before threshold actions

Reviewed `perf/item6-phase1` at `14236fa50d685649bc9bdfb6440eede7fe57d576`. This pass focuses on whether a failed slash/jail retains the threshold vote.

**Hypothesis: a threshold-reaching vote is stored before slash and jail operations, and is not rolled back if either later operation errors.** `record_jail_vote` writes the vote row, tallies, then invokes slash followed by jail. Native dispatch converts the returned error to a failed action result. This pass isolates the vote/threshold transition and does not duplicate pass 31’s repeated-vote slash behavior.

Evidence: [staking.rs L619](https://github.com/OZmasterAI/Torus-hyperBFT/blob/14236fa50d685649bc9bdfb6440eede7fe57d576/crates/torus-economics/src/staking.rs#L619), [staking.rs L628](https://github.com/OZmasterAI/Torus-hyperBFT/blob/14236fa50d685649bc9bdfb6440eede7fe57d576/crates/torus-economics/src/staking.rs#L628), [staking.rs L637](https://github.com/OZmasterAI/Torus-hyperBFT/blob/14236fa50d685649bc9bdfb6440eede7fe57d576/crates/torus-economics/src/staking.rs#L637), [staking.rs L643](https://github.com/OZmasterAI/Torus-hyperBFT/blob/14236fa50d685649bc9bdfb6440eede7fe57d576/crates/torus-economics/src/staking.rs#L643), [native_executor.rs L7890](https://github.com/OZmasterAI/Torus-hyperBFT/blob/14236fa50d685649bc9bdfb6440eede7fe57d576/crates/torus-bridge/src/native_executor.rs#L7890).

## Limits

Static review at the pinned source revision. No fault injection, live chain transaction, or Rust test was run. No source change.
