# Pass 44 — Empty-block scheduling for pending liquidations

Reviewed `perf/item6-phase1` at `5c756ffb8f9a95675e62064269abdc31f10eb703`. This report isolates one behavior.

This pass checks only how a liquidation step remains scheduled when there are no new user actions or oracle submissions.

`liquidation_due` returns true if cooldown rows, pending-under-maintenance rows, or the scan cursor exist. These are the three persisted reasons the account walk may need another block; a due read error is propagated to the caller rather than silently treated as no work. This allows an empty block to advance cooldown/full-order handling and resume a cut scan.

Evidence: [due predicates](https://github.com/OZmasterAI/Torus-hyperBFT/blob/5c756ffb8f9a95675e62064269abdc31f10eb703/crates/torus-bridge/src/liquidation_step.rs#L43), [liquidation pending state](https://github.com/OZmasterAI/Torus-hyperBFT/blob/5c756ffb8f9a95675e62064269abdc31f10eb703/crates/torus-core/src/liquidation.rs#L419).

No missing trigger found in the listed state transitions during this pass. It does not assess global block production liveness or write failure handling. Static review only.
