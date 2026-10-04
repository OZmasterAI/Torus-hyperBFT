# Pass 38 — Cooldown begins after an unfilled chunk attempt

Reviewed `perf/item6-phase1` at `5c756ffb8f9a95675e62064269abdc31f10eb703`. This report isolates one behavior.

This pass examines exactly when a chunked stage-1 attempt writes the account's cooldown timestamp.

The stage-1 handler calls the IOC order path, records an unsuccessful result if it failed, then unconditionally writes cooldown whenever the requested quantity was classified as a chunk. It does not inspect fill quantity before doing so. The documented policy describes a cooldown after an account is partially liquidated; a zero-fill IOC has not partially liquidated the account. A book with no eligible liquidity therefore starts the 30-second window anyway, and liquidity arriving within that window is met with a whole-position order rather than a 20% chunk. The price cap still bounds execution price, but size/market-impact policy differs.

Evidence: [IOC result and unconditional chunk cooldown write](https://github.com/OZmasterAI/Torus-hyperBFT/blob/5c756ffb8f9a95675e62064269abdc31f10eb703/crates/torus-bridge/src/liquidation_step.rs#L293), [chunk predicate](https://github.com/OZmasterAI/Torus-hyperBFT/blob/5c756ffb8f9a95675e62064269abdc31f10eb703/crates/torus-core/src/liquidation.rs#L97), [documented cooldown trigger](https://github.com/OZmasterAI/Torus-hyperBFT/blob/5c756ffb8f9a95675e62064269abdc31f10eb703/crates/torus-bridge/src/liquidation_step.rs#L267).

Candidate only; no test covers zero fills followed by new liquidity. `cargo` is unavailable. Add a regression with an empty book on the chunk block, then liquidity in a later block inside the nominal window.
