# Pass 41 — Liquidation health classification at the two-thirds boundary

Reviewed `perf/item6-phase1` at `5c756ffb8f9a95675e62064269abdc31f10eb703`. This report isolates one behavior.

This pass covers only the exact transition between Stage1 and Backstop.

Health classification computes available value as available collateral plus order margin plus UPnL with checked arithmetic. Healthy is `AV >= MM`; negative AV selects ADL; otherwise Backstop requires the strict condition `3*AV < 2*MM`. Therefore exact equality at two-thirds remains Stage1, while one raw unit below is Backstop. Checked multiplication overflow returns `None`, and the caller skips the account rather than classifying it with wrapped arithmetic.

Evidence: [classification and checked threshold](https://github.com/OZmasterAI/Torus-hyperBFT/blob/5c756ffb8f9a95675e62064269abdc31f10eb703/crates/torus-core/src/liquidation.rs#L68).

This matches the strict boundary described in the implementation. No new finding. No arithmetic test was executed because `cargo` is unavailable.
