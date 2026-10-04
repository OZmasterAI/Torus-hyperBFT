# Pass 49 — Previous-mark reset across oracle outages

Reviewed `perf/item6-phase1` at `5c756ffb8f9a95675e62064269abdc31f10eb703`. This report isolates one behavior.

This pass examines only whether ADL can reuse a mark from before an oracle outage.

At each liquidation step, `put_prev_marks` records current usable marks for listed markets and deletes the prior-mark row when a listed market has no current mark. A subsequent ADL therefore falls back to the current mark instead of using a stale pre-outage price. Delisted/unlisted markets are not iterated by this helper, but their positions are not included in the current step's listed-market mark table either.

Evidence: [previous mark read](https://github.com/OZmasterAI/Torus-hyperBFT/blob/5c756ffb8f9a95675e62064269abdc31f10eb703/crates/torus-core/src/liquidation.rs#L524), [listed markets write/delete behavior](https://github.com/OZmasterAI/Torus-hyperBFT/blob/5c756ffb8f9a95675e62064269abdc31f10eb703/crates/torus-core/src/liquidation.rs#L534), [ADL selection of previous/current mark](https://github.com/OZmasterAI/Torus-hyperBFT/blob/5c756ffb8f9a95675e62064269abdc31f10eb703/crates/torus-bridge/src/liquidation_step.rs#L323).

No stale pre-outage reuse found for listed markets. This does not assess mixed-mark collateral behavior already recorded in pass 35. Static review only.
