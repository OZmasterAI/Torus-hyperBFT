# Pass 47 — ADL candidate ranking total-order determinism

Reviewed `perf/item6-phase1` at `5c756ffb8f9a95675e62064269abdc31f10eb703`. This report isolates one behavior.

This pass checks only ranking determinism and its behavior for zero/negative ranking inputs.

The ranking builds an exact rational score from mark-to-entry profitability and notional/account value, compares by U1024 cross multiplication, places nonpositive account value or a zero denominator last, and breaks ties by address. The inputs are clamped nonnegative by `wide`; the comparator uses a deterministic address tie break. Given valid bounded FixedPoint inputs, the documented product-width argument keeps cross-products within U1024.

Evidence: [rank formula and invalid denominator handling](https://github.com/OZmasterAI/Torus-hyperBFT/blob/5c756ffb8f9a95675e62064269abdc31f10eb703/crates/torus-core/src/liquidation.rs#L207), [total comparator and address tie break](https://github.com/OZmasterAI/Torus-hyperBFT/blob/5c756ffb8f9a95675e62064269abdc31f10eb703/crates/torus-core/src/liquidation.rs#L228).

No nondeterministic tie behavior found. No property test could be run because `cargo` is unavailable; this review did not independently prove the width bound for malformed serialized values.
