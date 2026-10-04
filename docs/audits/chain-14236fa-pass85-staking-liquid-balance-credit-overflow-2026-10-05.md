# Pass 85 — Liquid account credit arithmetic

Reviewed `perf/item6-phase1` at `14236fa50d685649bc9bdfb6440eede7fe57d576`. This pass focuses on overflow behavior for staking and treasury balance credits.

**Hypothesis: staking-side balance credits use unchecked addition.** `credit_balance` reads the account and adds the requested amount before writing it. It is used for permanent staking rewards, claims, and fee treasury/dev-pool credits. An account near `U256::MAX` could overflow under this helper; no checked-add or explicit error is present. Practical reachability needs supply-bound analysis.

Evidence: [staking.rs L1020](https://github.com/OZmasterAI/Torus-hyperBFT/blob/14236fa50d685649bc9bdfb6440eede7fe57d576/crates/torus-economics/src/staking.rs#L1020), [staking.rs L1022](https://github.com/OZmasterAI/Torus-hyperBFT/blob/14236fa50d685649bc9bdfb6440eede7fe57d576/crates/torus-economics/src/staking.rs#L1022), [rewards.rs L152](https://github.com/OZmasterAI/Torus-hyperBFT/blob/14236fa50d685649bc9bdfb6440eede7fe57d576/crates/torus-economics/src/rewards.rs#L152).

## Limits

Static review at the pinned source revision. No fault injection, live chain transaction, or Rust test was run. No source change.
