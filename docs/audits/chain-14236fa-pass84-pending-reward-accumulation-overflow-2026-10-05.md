# Pass 84 — Pending reward balance arithmetic

Reviewed `perf/item6-phase1` at `14236fa50d685649bc9bdfb6440eede7fe57d576`. This pass focuses on overflow behavior when multiple reward credits accumulate.

**Hypothesis: pending rewards can overflow during accumulation.** `credit_rewards` loads the prior pending amount and adds the new credit with unchecked `U256` addition. This helper is used by fee and validator reward paths. Whether protocol bounds make the overflow unreachable is not established; the current helper does not return a checked-overflow error or reject a value above the representable maximum.

Evidence: [staking.rs L982](https://github.com/OZmasterAI/Torus-hyperBFT/blob/14236fa50d685649bc9bdfb6440eede7fe57d576/crates/torus-economics/src/staking.rs#L982), [staking.rs L989](https://github.com/OZmasterAI/Torus-hyperBFT/blob/14236fa50d685649bc9bdfb6440eede7fe57d576/crates/torus-economics/src/staking.rs#L989), [rewards.rs L220](https://github.com/OZmasterAI/Torus-hyperBFT/blob/14236fa50d685649bc9bdfb6440eede7fe57d576/crates/torus-economics/src/rewards.rs#L220).

## Limits

Static review at the pinned source revision. No fault injection, live chain transaction, or Rust test was run. No source change.
