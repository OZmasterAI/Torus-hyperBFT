# Pass 95 — Governance treasury-spend write ordering

Reviewed `perf/item6-phase1` at `14236fa50d685649bc9bdfb6440eede7fe57d576`. This pass focuses on treasury debit and recipient credit atomicity.

**Hypothesis: a failed recipient credit/write can leave treasury funds debited without a completed transfer.** Governance execution writes the reduced treasury account before it loads and credits the recipient. The branch does not use an atomic state batch. The error is returned to governance processing after the first write, so a fault-injection test should confirm whether the block overlay preserves the debit.

Evidence: [governance.rs L1067](https://github.com/OZmasterAI/Torus-hyperBFT/blob/14236fa50d685649bc9bdfb6440eede7fe57d576/crates/torus-economics/src/governance.rs#L1067), [governance.rs L1077](https://github.com/OZmasterAI/Torus-hyperBFT/blob/14236fa50d685649bc9bdfb6440eede7fe57d576/crates/torus-economics/src/governance.rs#L1077), [governance.rs L1082](https://github.com/OZmasterAI/Torus-hyperBFT/blob/14236fa50d685649bc9bdfb6440eede7fe57d576/crates/torus-economics/src/governance.rs#L1082), [governance.rs L1084](https://github.com/OZmasterAI/Torus-hyperBFT/blob/14236fa50d685649bc9bdfb6440eede7fe57d576/crates/torus-economics/src/governance.rs#L1084).

## Limits

Static review at the pinned source revision. No fault injection, live chain transaction, or Rust test was run. No source change.
