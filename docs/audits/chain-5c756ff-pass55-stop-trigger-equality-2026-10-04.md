# Pass 55 — Stop trigger equality semantics

Reviewed `perf/item6-phase1` at `5c756ffb8f9a95675e62064269abdc31f10eb703`. This report isolates one behavior.

This pass checks only whether stop conditions fire at equality with the last trade.

A buy stop fires when `current_price >= trigger_price`; a sell stop fires when `current_price <= trigger_price`. Equality therefore triggers both directions consistently, and the order book returns fired rows rather than executing them inline so the executor can revalidate the order and reservation.

Evidence: [trigger predicates](https://github.com/OZmasterAI/Torus-hyperBFT/blob/5c756ffb8f9a95675e62064269abdc31f10eb703/crates/torus-core/src/order_book.rs#L2279), [executor revalidation path](https://github.com/OZmasterAI/Torus-hyperBFT/blob/5c756ffb8f9a95675e62064269abdc31f10eb703/crates/torus-bridge/src/native_executor.rs#L7005).

No boundary error found. This review does not assess whether a mark/oracle rather than last trade should trigger stops; that is product policy. Static only.
