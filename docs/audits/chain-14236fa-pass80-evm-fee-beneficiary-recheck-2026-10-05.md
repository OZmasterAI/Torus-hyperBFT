# Pass 80 — EVM beneficiary and protocol fee accounting

Reviewed `perf/item6-phase1` at `14236fa50d685649bc9bdfb6440eede7fe57d576`. This pass focuses on whether receipt fee revenue overlaps the EVM coinbase credit.

**Reconfirmed pass 30’s EVM fee-accounting concern; no new finding.** Consensus computes protocol fee revenue as the sum of receipt `gas_used * effective_gas_price` and passes it to native fee distribution. EVM transaction execution also follows its own beneficiary/base-fee accounting path. This pass confirms both inputs still exist at the current head but does not independently reconcile per-receipt balance deltas; use pass 30’s targeted reconciliation as the prior finding record.

Evidence: [app.rs L1825](https://github.com/OZmasterAI/Torus-hyperBFT/blob/14236fa50d685649bc9bdfb6440eede7fe57d576/crates/torus-consensus/src/app.rs#L1825), [app.rs L2305](https://github.com/OZmasterAI/Torus-hyperBFT/blob/14236fa50d685649bc9bdfb6440eede7fe57d576/crates/torus-consensus/src/app.rs#L2305), [app.rs L2306](https://github.com/OZmasterAI/Torus-hyperBFT/blob/14236fa50d685649bc9bdfb6440eede7fe57d576/crates/torus-consensus/src/app.rs#L2306), [proposer.rs L395](https://github.com/OZmasterAI/Torus-hyperBFT/blob/14236fa50d685649bc9bdfb6440eede7fe57d576/crates/tor-bridge/src/proposer.rs#L395).

## Limits

Static review at the pinned source revision. No fault injection, live chain transaction, or Rust test was run. No source change.
