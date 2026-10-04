# Pass 90 — Undelegation operation write ordering

Reviewed `perf/item6-phase1` at `14236fa50d685649bc9bdfb6440eede7fe57d576`. This pass focuses on delegation queue and validator total on a later write error.

**Hypothesis: undelegation can persist its row mutation without updating validator totals if the second write fails.** It first stores the reduced active amount plus unbonding entry, then decrements and writes `validator.total_delegated`. An error on the latter write returns failure after the first operation already changed the backend/overlay. No atomic batch wraps both rows.

Evidence: [staking.rs L173](https://github.com/OZmasterAI/Torus-hyperBFT/blob/14236fa50d685649bc9bdfb6440eede7fe57d576/crates/torus-economics/src/staking.rs#L173), [staking.rs L185](https://github.com/OZmasterAI/Torus-hyperBFT/blob/14236fa50d685649bc9bdfb6440eede7fe57d576/crates/torus-economics/src/staking.rs#L185), [staking.rs L190](https://github.com/OZmasterAI/Torus-hyperBFT/blob/14236fa50d685649bc9bdfb6440eede7fe57d576/crates/torus-economics/src/staking.rs#L190), [staking.rs L192](https://github.com/OZmasterAI/Torus-hyperBFT/blob/14236fa50d685649bc9bdfb6440eede7fe57d576/crates/torus-economics/src/staking.rs#L192).

## Limits

Static review at the pinned source revision. No fault injection, live chain transaction, or Rust test was run. No source change.
