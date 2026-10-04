# Pass 83 — Commission effective at epoch boundary

Reviewed `perf/item6-phase1` at `14236fa50d685649bc9bdfb6440eede7fe57d576`. This pass focuses on which commission rate determines the just-ended epoch’s validator rewards.

**Hypothesis: a commission update in the boundary block is applied to the full epoch’s inflation payout.** Native actions execute before `process_epoch_boundary`; that boundary then reads the validator’s current `commission_bps` and uses it to split the entire computed epoch emission. Commission changes are capped and delayed, but no historical commission snapshot or within-epoch proration is consulted here. Confirm the intended effective-height policy and add a boundary-block regression.

Evidence: [app.rs L2305](https://github.com/OZmasterAI/Torus-hyperBFT/blob/14236fa50d685649bc9bdfb6440eede7fe57d576/crates/torus-consensus/src/app.rs#L2305), [app.rs L2307](https://github.com/OZmasterAI/Torus-hyperBFT/blob/14236fa50d685649bc9bdfb6440eede7fe57d576/crates/torus-consensus/src/app.rs#L2307), [rewards.rs L215](https://github.com/OZmasterAI/Torus-hyperBFT/blob/14236fa50d685649bc9bdfb6440eede7fe57d576/crates/torus-economics/src/rewards.rs#L215), [staking.rs L429](https://github.com/OZmasterAI/Torus-hyperBFT/blob/14236fa50d685649bc9bdfb6440eede7fe57d576/crates/torus-economics/src/staking.rs#L429), [staking.rs L456](https://github.com/OZmasterAI/Torus-hyperBFT/blob/14236fa50d685649bc9bdfb6440eede7fe57d576/crates/torus-economics/src/staking.rs#L456).

## Limits

Static review at the pinned source revision. No fault injection, live chain transaction, or Rust test was run. No source change.
