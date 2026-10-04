# Pass 81 — Fee burn versus cumulative burn reporting

Reviewed `perf/item6-phase1` at `14236fa50d685649bc9bdfb6440eede7fe57d576`. This pass focuses on whether the production fee burn increments SupplyTracker.

**Reconfirmed ECON-FIND-22/pass 29; no new issue.** Production block-fee distribution computes a `burn` amount and excludes it from the credited buckets, while the supply tracker is updated by a separate `FeeSplitter::execute_burn` helper. The production routine still does not call that helper, so fee burns are omitted from the cumulative burn value exposed by RPC. This is a state-reporting mismatch already documented in the earlier report.

Evidence: [rewards.rs L57](https://github.com/OZmasterAI/Torus-hyperBFT/blob/14236fa50d685649bc9bdfb6440eede7fe57d576/crates/torus-economics/src/rewards.rs#L57), [rewards.rs L60](https://github.com/OZmasterAI/Torus-hyperBFT/blob/14236fa50d685649bc9bdfb6440eede7fe57d576/crates/torus-economics/src/rewards.rs#L60), [rewards.rs L375](https://github.com/OZmasterAI/Torus-hyperBFT/blob/14236fa50d685649bc9bdfb6440eede7fe57d576/crates/torus-economics/src/rewards.rs#L375), [torus.rs L1496](https://github.com/OZmasterAI/Torus-hyperBFT/blob/14236fa50d685649bc9bdfb6440eede7fe57d576/crates/torus-rpc/src/torus.rs#L1496).

## Limits

Static review at the pinned source revision. No fault injection, live chain transaction, or Rust test was run. No source change.
