# Pass 46 — ADL candidate scan window and progress

Reviewed `perf/item6-phase1` at `5c756ffb8f9a95675e62064269abdc31f10eb703`. This report isolates one behavior.

This pass covers the bounded ADL counterparty scan, not the ranking formula.

Each `adl_candidates` call begins at the first `CF_NATIVE_POSITIONS` key and scans at most `max_rows` rows in key order. It filters the resulting window for the requested market and opposite side, then returns that set. There is no per-ADL cursor or market-aware seek. A bankrupt position that remains partially open after exhausting all eligible counterparties in this first window will rescan the same window on the next liquidation pass; candidates beyond the fixed prefix are not reached by that retry. This is a progress limitation if the first-window liquidity is exhausted while later-key candidates remain available.

Evidence: [scan starts at empty key and is capped](https://github.com/OZmasterAI/Torus-hyperBFT/blob/5c756ffb8f9a95675e62064269abdc31f10eb703/crates/torus-core/src/liquidation.rs#L341), [caller passes fixed ADL_MAX_SCAN_ROWS](https://github.com/OZmasterAI/Torus-hyperBFT/blob/5c756ffb8f9a95675e62064269abdc31f10eb703/crates/torus-bridge/src/liquidation_step.rs#L330), [partial close return path](https://github.com/OZmasterAI/Torus-hyperBFT/blob/5c756ffb8f9a95675e62064269abdc31f10eb703/crates/torus-core/src/liquidation.rs#L307).

The cap is intentional for per-block cost and documented in code; impact requires >65,536 position rows before useful counterparties. No stress test run; `cargo` unavailable.
