# Pass 37 — Stage-1 cooldown full-position semantics

Reviewed `perf/item6-phase1` at `5c756ffb8f9a95675e62064269abdc31f10eb703`. This report isolates one behavior.

The `perf/item6-phase1` head changed during this review; this pass pins the new behavior to `5c756ffb8f9a95675e62064269abdc31f10eb703`. It covers only the 30-second stage-1 cooldown rule.

The new branch checks `in_cooldown` once per account pass and submits the whole current position as a reduce-only IOC market order during cooldown. Outside cooldown it retains the 20% chunk path. The new integration case exercises a filled chunk, a full-position follow-up during cooldown, empty blocks, and the next chunk at expiry. Source and test tell a coherent story for that sequence; this pass found no separate defect in full-position order sizing during cooldown.

Evidence: [stage-1 sizing and IOC construction](https://github.com/OZmasterAI/Torus-hyperBFT/blob/5c756ffb8f9a95675e62064269abdc31f10eb703/crates/torus-bridge/src/liquidation_step.rs#L257), [cooldown integration case](https://github.com/OZmasterAI/Torus-hyperBFT/blob/5c756ffb8f9a95675e62064269abdc31f10eb703/crates/torus-bridge/tests/liquidation_tests.rs#L426), [30-second predicate](https://github.com/OZmasterAI/Torus-hyperBFT/blob/5c756ffb8f9a95675e62064269abdc31f10eb703/crates/torus-core/src/liquidation.rs#L480).

Verification is static. `cargo` is unavailable in this environment, so the targeted test could not be executed. The result is a source review, not a runtime confirmation.
