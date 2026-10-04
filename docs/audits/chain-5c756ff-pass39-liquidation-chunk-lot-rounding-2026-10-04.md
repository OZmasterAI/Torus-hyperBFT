# Pass 39 — Chunk quantity rounding and minimum-lot fallback

Reviewed `perf/item6-phase1` at `5c756ffb8f9a95675e62064269abdc31f10eb703`. This report isolates one behavior.

This pass isolates the 20% chunk quantity calculation and its relation to the market lot size.

For notional strictly above 100,000, the chunk uses integer `size.raw() / 5`; if that rounded quantity is below one lot, it falls back to the entire position and does not mark the attempt as chunked. Otherwise it returns the truncated quantity and starts the cooldown after the order attempt. The fallback avoids generating a sub-lot chunk, while a quantity at least one lot can still be a non-integral multiple of lot size unless position sizes are guaranteed to be lot-aligned. Position creation/fill constraints should be treated as that invariant; this review did not prove legacy or ADL-created positions preserve it.

Evidence: [threshold, truncation, and fallback](https://github.com/OZmasterAI/Torus-hyperBFT/blob/5c756ffb8f9a95675e62064269abdc31f10eb703/crates/torus-core/src/liquidation.rs#L94), [lot source and use](https://github.com/OZmasterAI/Torus-hyperBFT/blob/5c756ffb8f9a95675e62064269abdc31f10eb703/crates/torus-bridge/src/liquidation_step.rs#L277).

No independent exploit confirmed. Add boundary coverage for position sizes not divisible by five and by lot size. Static review only; `cargo` unavailable.
