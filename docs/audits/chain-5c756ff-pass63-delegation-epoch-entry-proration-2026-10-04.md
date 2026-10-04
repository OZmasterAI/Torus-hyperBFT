# Pass 63 — Delegation entry-time reward proration

Reviewed `perf/item6-phase1` at `5c756ffb8f9a95675e62064269abdc31f10eb703`. This round isolates one behavior: whether late delegations earn a full validator-inflation epoch.

**Hypothesis: a delegation added shortly before an epoch boundary participates in the whole next distribution amount.** At the boundary the distributor reads current active validators and current delegation amounts, computes emission over the full epoch length, and has no per-delegation activation height or time weighting. A late deposit therefore appears to share the same epoch pool as stake present throughout the epoch. This depends on action/boundary ordering and the intended accounting convention; a regression should compare an early and final-block delegation.

Evidence: [rewards.rs L166](https://github.com/OZmasterAI/Torus-hyperBFT/blob/5c756ffb8f9a95675e62064269abdc31f10eb703/crates/torus-economics/src/rewards.rs#L166), [rewards.rs L183](https://github.com/OZmasterAI/Torus-hyperBFT/blob/5c756ffb8f9a95675e62064269abdc31f10eb703/crates/torus-economics/src/rewards.rs#L183), [native_executor.rs L8696](https://github.com/OZmasterAI/Torus-hyperBFT/blob/5c756ffb8f9a95675e62064269abdc31f10eb703/crates/torus-bridge/src/native_executor.rs#L8696).

## Limits

Static source review only. No chain transaction, economic simulation, storage-failure injection, or Rust test was run. No source changes.
