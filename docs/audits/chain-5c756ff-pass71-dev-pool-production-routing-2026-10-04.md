# Pass 71 — Production routing of the developer fee bucket

Reviewed `perf/item6-phase1` at `5c756ffb8f9a95675e62064269abdc31f10eb703`. This round isolates one behavior: whether the gas-weighted DevPool receives production fee proceeds.

**Hypothesis: the production dev-pool share bypasses the gas-weighted DevPool.** The live block-fee distributor credits the entire calculated `dev_pool` amount to a configured account balance. The separate `DevPool::record_gas_usage` and `DevPool::distribute` APIs implement per-deployer proportional distribution, but repository call sites found for those functions are tests only; the production boundary path has no call to them. If the intended 45%/transition share funds gas-weighted contract developers, it instead accrues to one fixed account and is not allocated by usage.

Evidence: [rewards.rs L24](https://github.com/OZmasterAI/Torus-hyperBFT/blob/5c756ffb8f9a95675e62064269abdc31f10eb703/crates/torus-economics/src/rewards.rs#L24), [rewards.rs L70](https://github.com/OZmasterAI/Torus-hyperBFT/blob/5c756ffb8f9a95675e62064269abdc31f10eb703/crates/torus-economics/src/rewards.rs#L70), [dev_pool.rs L47](https://github.com/OZmasterAI/Torus-hyperBFT/blob/5c756ffb8f9a95675e62064269abdc31f10eb703/crates/torus-economics/src/dev_pool.rs#L47).

## Limits

Static source review only. No chain transaction, economic simulation, storage-failure injection, or Rust test was run. No source changes.
