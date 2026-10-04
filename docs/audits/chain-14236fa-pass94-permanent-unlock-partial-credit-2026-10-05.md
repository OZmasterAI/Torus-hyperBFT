# Pass 94 — Governance permanent-stake unlock write ordering

Reviewed `perf/item6-phase1` at `14236fa50d685649bc9bdfb6440eede7fe57d576`. This pass focuses on whether a failed final balance credit can strand unlocked principal.

**Hypothesis: governance unlock removes/reduces stake and deletes pending rewards before crediting liquid balance.** The operations are separate. If the final credit fails, stake principal may already be unlocked in storage and pending rewards may already be deleted, while the account has not received either value. The governance executor reports an error but this function does not make the transition atomic.

Evidence: [staking.rs L369](https://github.com/OZmasterAI/Torus-hyperBFT/blob/14236fa50d685649bc9bdfb6440eede7fe57d576/crates/torus-economics/src/staking.rs#L369), [staking.rs L385](https://github.com/OZmasterAI/Torus-hyperBFT/blob/14236fa50d685649bc9bdfb6440eede7fe57d576/crates/torus-economics/src/staking.rs#L385), [staking.rs L392](https://github.com/OZmasterAI/Torus-hyperBFT/blob/14236fa50d685649bc9bdfb6440eede7fe57d576/crates/torus-economics/src/staking.rs#L392), [governance.rs L1143](https://github.com/OZmasterAI/Torus-hyperBFT/blob/14236fa50d685649bc9bdfb6440eede7fe57d576/crates/torus-economics/src/governance.rs#L1143).

## Limits

Static review at the pinned source revision. No fault injection, live chain transaction, or Rust test was run. No source change.
