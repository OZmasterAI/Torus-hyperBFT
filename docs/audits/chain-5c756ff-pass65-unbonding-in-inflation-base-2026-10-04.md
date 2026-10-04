# Pass 65 — Pending unbonding principal in inflation base

Reviewed `perf/item6-phase1` at `5c756ffb8f9a95675e62064269abdc31f10eb703`. This round isolates one behavior: whether unbonding balances remain in validator stake totals.

**Pending unbondings are removed from active delegation weight immediately.** `undelegate` subtracts the requested amount from `delegation.amount` and from validator `total_delegated`, while retaining the principal only in the unbonding vector. Validator inflation totals active validator stake and apportions the delegator pool using active delegation amounts, so queued principal does not continue earning ordinary validator inflation. This is consistent with an exit taking effect when unbonding starts; the known zero-active-row remainder edge is tracked separately in pass 23.

Evidence: [staking.rs L173](https://github.com/OZmasterAI/Torus-hyperBFT/blob/5c756ffb8f9a95675e62064269abdc31f10eb703/crates/torus-economics/src/staking.rs#L173), [staking.rs L192](https://github.com/OZmasterAI/Torus-hyperBFT/blob/5c756ffb8f9a95675e62064269abdc31f10eb703/crates/torus-economics/src/staking.rs#L192), [rewards.rs L231](https://github.com/OZmasterAI/Torus-hyperBFT/blob/5c756ffb8f9a95675e62064269abdc31f10eb703/crates/torus-economics/src/rewards.rs#L231).

## Limits

Static source review only. No chain transaction, economic simulation, storage-failure injection, or Rust test was run. No source changes.
