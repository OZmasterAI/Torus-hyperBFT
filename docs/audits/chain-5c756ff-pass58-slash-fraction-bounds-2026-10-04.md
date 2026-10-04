# Pass 58 — Slash fraction input bounds

Reviewed `perf/item6-phase1` at `5c756ffb8f9a95675e62064269abdc31f10eb703`. This round isolates one behavior: whether production callers can request a slash above 10000 bps.

**No user-reachable bounds defect found.** `StakingManager::slash` divides the supplied `u16` fraction by 10000 and subtracts the result from stake without validating `fraction_bps <= 10000`. Such a value can make the computed slash exceed stake and trigger arithmetic underflow. The reviewed production invocation comes from jail-vote processing with a fixed internal fraction, rather than an action field supplied by the voter. This leaves a defensive API invariant, but source review did not establish a user-controlled path to an excessive fraction.

Evidence: [staking.rs L471](https://github.com/OZmasterAI/Torus-hyperBFT/blob/5c756ffb8f9a95675e62064269abdc31f10eb703/crates/torus-economics/src/staking.rs#L471), [staking.rs L482](https://github.com/OZmasterAI/Torus-hyperBFT/blob/5c756ffb8f9a95675e62064269abdc31f10eb703/crates/torus-economics/src/staking.rs#L482), [staking.rs L637](https://github.com/OZmasterAI/Torus-hyperBFT/blob/5c756ffb8f9a95675e62064269abdc31f10eb703/crates/torus-economics/src/staking.rs#L637).

## Limits

Static source review only. No chain transaction, economic simulation, storage-failure injection, or Rust test was run. No source changes.
