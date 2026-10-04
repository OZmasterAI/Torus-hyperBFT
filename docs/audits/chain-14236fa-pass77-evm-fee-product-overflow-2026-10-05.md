# Pass 77 — EVM receipt fee multiplication

Reviewed `perf/item6-phase1` at `14236fa50d685649bc9bdfb6440eede7fe57d576`. This pass focuses on overflow behavior when multiplying receipt gas by effective gas price.

**Hypothesis: fee-revenue calculation can overflow `u128` for a valid high-priced receipt.** `compute_fee_revenue` multiplies `gas_used as u128` by `effective_gas_price as u128` without a checked operation. If the EVM validator admits a transaction whose product exceeds `u128::MAX`, block fee calculation may panic or wrap according to build arithmetic settings before native distribution. Transaction fee bounds and release behavior were not exercised, so reachability remains unconfirmed.

Evidence: [proposer.rs L395](https://github.com/OZmasterAI/Torus-hyperBFT/blob/14236fa50d685649bc9bdfb6440eede7fe57d576/crates/torus-bridge/src/proposer.rs#L395), [proposer.rs L398](https://github.com/OZmasterAI/Torus-hyperBFT/blob/14236fa50d685649bc9bdfb6440eede7fe57d576/crates/torus-bridge/src/proposer.rs#L398).

## Limits

Static review at the pinned source revision. No fault injection, live chain transaction, or Rust test was run. No source change.
