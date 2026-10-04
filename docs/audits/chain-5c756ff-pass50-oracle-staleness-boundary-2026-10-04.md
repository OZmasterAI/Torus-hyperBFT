# Pass 50 — Oracle aggregate freshness boundary

Reviewed `perf/item6-phase1` at `5c756ffb8f9a95675e62064269abdc31f10eb703`. This report isolates one behavior.

This pass checks only the exact stale-age cutoff used by price readers.

An aggregate is stale only when `now.saturating_sub(timestamp) > max_age_secs`; an age exactly equal to the configured maximum remains fresh. Future timestamps saturate to age zero. The same predicate is used by `get_price` and `get_last_valid_price`, so fallback reads do not bypass the ordinary freshness gate.

Evidence: [get_price contract](https://github.com/OZmasterAI/Torus-hyperBFT/blob/5c756ffb8f9a95675e62064269abdc31f10eb703/crates/torus-core/src/oracle.rs#L372), [shared is_stale predicate and last-valid rejection](https://github.com/OZmasterAI/Torus-hyperBFT/blob/5c756ffb8f9a95675e62064269abdc31f10eb703/crates/torus-core/src/oracle.rs#L453).

Boundary is internally consistent. Header timestamp monotonicity and handling of far-future timestamps are outside this one-property review. Static only.
