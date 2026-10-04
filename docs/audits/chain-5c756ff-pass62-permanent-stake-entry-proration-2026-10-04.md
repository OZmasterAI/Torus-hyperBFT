# Pass 62 — Permanent-stake entry-time reward proration

Reviewed `perf/item6-phase1` at `5c756ffb8f9a95675e62064269abdc31f10eb703`. This round isolates one behavior: whether a new permanent position earns only the time it was locked.

**Hypothesis: a permanent stake added just before an epoch boundary receives a full epoch of rewards.** The epoch distributor iterates current permanent-stake records and computes reward using each full stored amount multiplied by the entire `blocks_in_epoch`; it does not use `locked_at_block` to prorate the first epoch. Since the native action writes the position at the current block and boundary processing later passes the configured full epoch length, a late entrant appears eligible for a full epoch. Confirm block action ordering and intended reward policy with a regression/spec check.

Evidence: [rewards.rs L130](https://github.com/OZmasterAI/Torus-hyperBFT/blob/5c756ffb8f9a95675e62064269abdc31f10eb703/crates/torus-economics/src/rewards.rs#L130), [rewards.rs L146](https://github.com/OZmasterAI/Torus-hyperBFT/blob/5c756ffb8f9a95675e62064269abdc31f10eb703/crates/torus-economics/src/rewards.rs#L146), [native_executor.rs L7849](https://github.com/OZmasterAI/Torus-hyperBFT/blob/5c756ffb8f9a95675e62064269abdc31f10eb703/crates/torus-bridge/src/native_executor.rs#L7849).

## Limits

Static source review only. No chain transaction, economic simulation, storage-failure injection, or Rust test was run. No source changes.
