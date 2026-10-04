# Pass 96 — Delegated and permanent stake in proposal voting snapshots

Reviewed `perf/item6-phase1` at `14236fa50d685649bc9bdfb6440eede7fe57d576`. This pass focuses on which stake balances are included in governance vote weight.

**No new snapshot defect found.** Proposal creation requires a minimum based on current delegated plus permanent stake; voting weight uses active delegated amounts plus permanent stake multiplied by the configured ratio. The proposal path snapshots voter weight, so subsequent delegation or permanent-stake changes do not rewrite prior vote weights. Unbonding entries are excluded because the delegation scan sums `Delegation.amount` only. This is consistent with active-stake weighting and pass 11’s snapshot review.

Evidence: [governance.rs L721](https://github.com/OZmasterAI/Torus-hyperBFT/blob/14236fa50d685649bc9bdfb6440eede7fe57d576/crates/torus-economics/src/governance.rs#L721), [governance.rs L782](https://github.com/OZmasterAI/Torus-hyperBFT/blob/14236fa50d685649bc9bdfb6440eede7fe57d576/crates/torus-economics/src/governance.rs#L782), [governance.rs L823](https://github.com/OZmasterAI/Torus-hyperBFT/blob/14236fa50d685649bc9bdfb6440eede7fe57d576/crates/torus-economics/src/governance.rs#L823), [governance.rs L1284](https://github.com/OZmasterAI/Torus-hyperBFT/blob/14236fa50d685649bc9bdfb6440eede7fe57d576/crates/torus-economics/src/governance.rs#L1284), [governance.rs L1296](https://github.com/OZmasterAI/Torus-hyperBFT/blob/14236fa50d685649bc9bdfb6440eede7fe57d576/crates/torus-economics/src/governance.rs#L1296), [governance.rs L1425](https://github.com/OZmasterAI/Torus-hyperBFT/blob/14236fa50d685649bc9bdfb6440eede7fe57d576/crates/torus-economics/src/governance.rs#L1425).

## Limits

Static review at the pinned source revision. No fault injection, live chain transaction, or Rust test was run. No source change.
