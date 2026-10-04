# Pass 76 — Treasury live balance and cumulative report

Reviewed `perf/item6-phase1` at `5c756ffb8f9a95675e62064269abdc31f10eb703`. This round isolates one behavior: whether RPC cumulative totals reconcile to the treasury account.

**Reconfirmed the historical fee-tracker integration gap; no new finding.** Production block fees credit the configured treasury account directly, while `SupplyTracker.cumulative_treasury` is updated by a separate `FeeSplitter::credit_treasury` helper. The RPC returns both the live account balance and that cumulative field. Normal block-fee credits therefore increase the balance without increasing the cumulative tracker. This is the known ECON-FIND-22/pass-29 issue and is documented here only as a narrow RPC reconciliation recheck.

Evidence: [rewards.rs L66](https://github.com/OZmasterAI/Torus-hyperBFT/blob/5c756ffb8f9a95675e62064269abdc31f10eb703/crates/torus-economics/src/rewards.rs#L66), [rewards.rs L445](https://github.com/OZmasterAI/Torus-hyperBFT/blob/5c756ffb8f9a95675e62064269abdc31f10eb703/crates/torus-economics/src/rewards.rs#L445), [torus.rs L1496](https://github.com/OZmasterAI/Torus-hyperBFT/blob/5c756ffb8f9a95675e62064269abdc31f10eb703/crates/torus-rpc/src/torus.rs#L1496).

## Limits

Static source review only. No chain transaction, economic simulation, storage-failure injection, or Rust test was run. No source changes.
