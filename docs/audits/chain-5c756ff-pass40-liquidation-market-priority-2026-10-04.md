# Pass 40 — Stage-1 cross-market liquidation priority

Reviewed `perf/item6-phase1` at `5c756ffb8f9a95675e62064269abdc31f10eb703`. This report isolates one behavior.

This pass checks only how stage-1 chooses among an account's marked positions in different markets.

The handler computes each marked position's maintenance requirement at the current mark, stores `(Reverse(MM), market_id)`, and sorts before placing liquidation orders. This gives higher maintenance requirement first, with ascending market ID as a deterministic tie break. Unmarked positions are excluded from this order list. No dependence on hash-map iteration order remains in the selected sequence.

Evidence: [priority tuple construction and sort](https://github.com/OZmasterAI/Torus-hyperBFT/blob/5c756ffb8f9a95675e62064269abdc31f10eb703/crates/torus-bridge/src/liquidation_step.rs#L265), [per-market order loop](https://github.com/OZmasterAI/Torus-hyperBFT/blob/5c756ffb8f9a95675e62064269abdc31f10eb703/crates/torus-bridge/src/liquidation_step.rs#L276).

No defect found in this ordering property. This is not a review of the separate mixed-mark collateral behavior from pass 35. Static source inspection only.
