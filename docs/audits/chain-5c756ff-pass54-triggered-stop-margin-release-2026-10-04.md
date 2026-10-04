# Pass 54 — Reservation release when a stop triggers

Reviewed `perf/item6-phase1` at `5c756ffb8f9a95675e62064269abdc31f10eb703`. This report isolates one behavior.

This pass examines only the pending-margin reservation lifecycle when a stop order fires.

The book removes the stop row and returns a triggered-stop record. The executor recomputes its reserved amount from the trigger order's stored price and quantity, releases that amount from order margin (clamped to the current reservation), then sends the order through normal validation/reservation/matching. If this re-placement fails, the stop has already left the pending set and its prior reservation has been released, so funds are not left locked for that trigger.

Evidence: [stop removal and returned order conversion](https://github.com/OZmasterAI/Torus-hyperBFT/blob/5c756ffb8f9a95675e62064269abdc31f10eb703/crates/torus-core/src/order_book.rs#L2279), [reservation release and normal re-placement](https://github.com/OZmasterAI/Torus-hyperBFT/blob/5c756ffb8f9a95675e62064269abdc31f10eb703/crates/torus-bridge/src/native_executor.rs#L7010).

No lost-reservation path found in this transition. Corrupt legacy rows are handled by the documented overflow fallback and remain a separate condition. Static only.
