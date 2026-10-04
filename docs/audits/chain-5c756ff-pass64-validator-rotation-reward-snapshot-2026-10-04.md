# Pass 64 — Validator rotation versus boundary rewards

Reviewed `perf/item6-phase1` at `5c756ffb8f9a95675e62064269abdc31f10eb703`. This round isolates one behavior: which validator set receives the boundary emission.

**Rewards use the pre-rotation active validator set at the boundary.** `process_epoch_boundary` explicitly distributes permanent rewards and validator inflation before executing planned rotation. Thus a validator becoming active in that rotation is absent from this boundary’s validator-inflation snapshot, while a validator removed by it remains eligible for that boundary. This is deterministic and appears intentional from the phase ordering; no separate defect established.

Evidence: [native_executor.rs L8696](https://github.com/OZmasterAI/Torus-hyperBFT/blob/5c756ffb8f9a95675e62064269abdc31f10eb703/crates/torus-bridge/src/native_executor.rs#L8696), [native_executor.rs L8704](https://github.com/OZmasterAI/Torus-hyperBFT/blob/5c756ffb8f9a95675e62064269abdc31f10eb703/crates/torus-bridge/src/native_executor.rs#L8704), [native_executor.rs L8710](https://github.com/OZmasterAI/Torus-hyperBFT/blob/5c756ffb8f9a95675e62064269abdc31f10eb703/crates/torus-bridge/src/native_executor.rs#L8710).

## Limits

Static source review only. No chain transaction, economic simulation, storage-failure injection, or Rust test was run. No source changes.
