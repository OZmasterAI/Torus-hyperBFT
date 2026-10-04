# Pass 91 — Permanent-stake principal debit ordering

Reviewed `perf/item6-phase1` at `14236fa50d685649bc9bdfb6440eede7fe57d576`. This pass focuses on effect of an error after debiting the staker account.

**Hypothesis: a storage/read error after the liquid debit can leave principal removed without a permanent-stake credit.** `permanent_stake` writes the account debit before loading or storing the permanent-stake record. The function returns an error on a later failure and has no multi-row atomic write. As with pass 89, a targeted backend-fault test is required to establish production overlay persistence and practical exposure.

Evidence: [staking.rs L317](https://github.com/OZmasterAI/Torus-hyperBFT/blob/14236fa50d685649bc9bdfb6440eede7fe57d576/crates/torus-economics/src/staking.rs#L317), [staking.rs L322](https://github.com/OZmasterAI/Torus-hyperBFT/blob/14236fa50d685649bc9bdfb6440eede7fe57d576/crates/torus-economics/src/staking.rs#L322), [staking.rs L330](https://github.com/OZmasterAI/Torus-hyperBFT/blob/14236fa50d685649bc9bdfb6440eede7fe57d576/crates/torus-economics/src/staking.rs#L330).

## Limits

Static review at the pinned source revision. No fault injection, live chain transaction, or Rust test was run. No source change.
