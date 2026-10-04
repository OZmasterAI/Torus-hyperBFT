# Pass 92 — Slashing state transition atomicity

Reviewed `perf/item6-phase1` at `14236fa50d685649bc9bdfb6440eede7fe57d576`. This pass focuses on ordering of delegation reductions, validator update, burn tracking and slash record.

**Hypothesis: slashing can leave an incomplete economic transition after a later error.** The routine writes each affected delegation one by one, then writes the validator aggregate, then updates the burn tracker and slash record. Any later error returns without a cross-row atomic batch. Because slash is called from jail-vote execution, a partially applied slash could also precede failure to jail the validator. Confirm with a fault-injected state backend.

Evidence: [staking.rs L497](https://github.com/OZmasterAI/Torus-hyperBFT/blob/14236fa50d685649bc9bdfb6440eede7fe57d576/crates/torus-economics/src/staking.rs#L497), [staking.rs L503](https://github.com/OZmasterAI/Torus-hyperBFT/blob/14236fa50d685649bc9bdfb6440eede7fe57d576/crates/torus-economics/src/staking.rs#L503), [staking.rs L527](https://github.com/OZmasterAI/Torus-hyperBFT/blob/14236fa50d685649bc9bdfb6440eede7fe57d576/crates/torus-economics/src/staking.rs#L527), [staking.rs L531](https://github.com/OZmasterAI/Torus-hyperBFT/blob/14236fa50d685649bc9bdfb6440eede7fe57d576/crates/torus-economics/src/staking.rs#L531), [staking.rs L535](https://github.com/OZmasterAI/Torus-hyperBFT/blob/14236fa50d685649bc9bdfb6440eede7fe57d576/crates/torus-economics/src/staking.rs#L535).

## Limits

Static review at the pinned source revision. No fault injection, live chain transaction, or Rust test was run. No source change.
