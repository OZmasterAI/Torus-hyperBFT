# Pass 67 — No active validators at epoch boundary

Reviewed `perf/item6-phase1` at `5c756ffb8f9a95675e62064269abdc31f10eb703`. This round isolates one behavior: whether empty-set inflation is minted or carried.

**No validator inflation is minted when there are no active validators.** The distributor returns zero for an empty active set (and for zero aggregate stake), before calculating or updating the cumulative validator-inflation tracker. The permanent-stake reward path remains independent. This means the validator allocation is skipped for that epoch rather than accumulated for a future set; no carry-forward liability is stored in this function. Whether that is intended is a protocol-budget question, not a source-level arithmetic defect.

Evidence: [rewards.rs L166](https://github.com/OZmasterAI/Torus-hyperBFT/blob/5c756ffb8f9a95675e62064269abdc31f10eb703/crates/torus-economics/src/rewards.rs#L166), [rewards.rs L176](https://github.com/OZmasterAI/Torus-hyperBFT/blob/5c756ffb8f9a95675e62064269abdc31f10eb703/crates/torus-economics/src/rewards.rs#L176), [rewards.rs L183](https://github.com/OZmasterAI/Torus-hyperBFT/blob/5c756ffb8f9a95675e62064269abdc31f10eb703/crates/torus-economics/src/rewards.rs#L183).

## Limits

Static source review only. No chain transaction, economic simulation, storage-failure injection, or Rust test was run. No source changes.
