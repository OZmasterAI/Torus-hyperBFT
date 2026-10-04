# Pass 48 — ADL bankruptcy price rounding

Reviewed `perf/item6-phase1` at `5c756ffb8f9a95675e62064269abdc31f10eb703`. This report isolates one behavior.

This pass isolates the integer rounding direction for bankruptcy-price calculation.

For positive position size, the calculation uses `ceil(rest * SCALE / size)`; Rust truncation toward zero is the mathematical ceiling for negative quotients and the helper adds one for positive non-integral quotients. The resulting offset is subtracted from a long's entry or added to a short's entry, rounding the close against the insolvent account. Overflow and nonpositive size return `None`, allowing the caller to use the documented mark fallback.

Evidence: [ceil division](https://github.com/OZmasterAI/Torus-hyperBFT/blob/5c756ffb8f9a95675e62064269abdc31f10eb703/crates/torus-core/src/liquidation.rs#L135), [bankruptcy price](https://github.com/OZmasterAI/Torus-hyperBFT/blob/5c756ffb8f9a95675e62064269abdc31f10eb703/crates/torus-core/src/liquidation.rs#L152), [ADL fallback/clamp](https://github.com/OZmasterAI/Torus-hyperBFT/blob/5c756ffb8f9a95675e62064269abdc31f10eb703/crates/torus-core/src/liquidation.rs#L170).

No counterexample found in the sign analysis. Extreme i128 boundary cases need property testing; unavailable without `cargo`.
