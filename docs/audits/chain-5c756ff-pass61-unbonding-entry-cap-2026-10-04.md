# Pass 61 — Per-delegation unbonding queue cap

Reviewed `perf/item6-phase1` at `5c756ffb8f9a95675e62064269abdc31f10eb703`. This round isolates one behavior: cap semantics and exact maximum.

**The cap is enforced at 100 pending entries per delegator-validator pair.** A 100th entry is allowed when the prior count is 99; a further request fails before the updated delegation is written. This limits queue growth per row, while leaving the number of validator rows per delegator outside this narrow cap. No off-by-one defect found.

Evidence: [staking.rs L173](https://github.com/OZmasterAI/Torus-hyperBFT/blob/5c756ffb8f9a95675e62064269abdc31f10eb703/crates/torus-economics/src/staking.rs#L173), [staking.rs L177](https://github.com/OZmasterAI/Torus-hyperBFT/blob/5c756ffb8f9a95675e62064269abdc31f10eb703/crates/torus-economics/src/staking.rs#L177), [staking.rs L185](https://github.com/OZmasterAI/Torus-hyperBFT/blob/5c756ffb8f9a95675e62064269abdc31f10eb703/crates/torus-economics/src/staking.rs#L185).

## Limits

Static source review only. No chain transaction, economic simulation, storage-failure injection, or Rust test was run. No source changes.
