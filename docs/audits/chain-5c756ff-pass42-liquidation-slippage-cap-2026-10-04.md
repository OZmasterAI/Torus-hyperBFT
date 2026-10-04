# Pass 42 — Liquidation slippage cap by margin tier

Reviewed `perf/item6-phase1` at `5c756ffb8f9a95675e62064269abdc31f10eb703`. This report isolates one behavior.

This pass checks the cap formula and whether stage-1 applies it to the order side.

The cap chooses the effective leverage for the position's notional (or the default when no tier table exists), clamps leverage to at least one, and computes `mark ± mark/(2*leverage)` with saturating raw addition/subtraction. Stage1 sets `is_buy = !p.is_long`, so a liquidated long sells against the lower cap and a short buys against the upper cap. The order is still a market IOC, so it cannot execute beyond that cap.

Evidence: [cap formula](https://github.com/OZmasterAI/Torus-hyperBFT/blob/5c756ffb8f9a95675e62064269abdc31f10eb703/crates/torus-core/src/liquidation.rs#L112), [stage-1 direction/cap wiring](https://github.com/OZmasterAI/Torus-hyperBFT/blob/5c756ffb8f9a95675e62064269abdc31f10eb703/crates/torus-bridge/src/liquidation_step.rs#L278).

No direction inversion found. Boundary behavior near FixedPoint extrema and nonpositive marks was not dynamically tested; the oracle path rejects invalid prices elsewhere. Static review only.
