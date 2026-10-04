# Pass 56 — Triggered-stop cascade ordering and termination

Reviewed `perf/item6-phase1` at `5c756ffb8f9a95675e62064269abdc31f10eb703`. This report isolates one behavior.

This pass focuses only on the order and termination of cascaded stop triggers.

The executor processes a FIFO `VecDeque`. Each re-placed stop may append newly triggered stops to the same queue. The book removes a stop when it fires, so a stop can enter the cascade at most once; the loop therefore terminates after processing the finite set of pending stops present or created by fills in the block. FIFO order is explicit rather than dependent on hash-map traversal.

Evidence: [FIFO processing loop](https://github.com/OZmasterAI/Torus-hyperBFT/blob/5c756ffb8f9a95675e62064269abdc31f10eb703/crates/torus-bridge/src/native_executor.rs#L7010), [newly triggered stops appended by matching path](https://github.com/OZmasterAI/Torus-hyperBFT/blob/5c756ffb8f9a95675e62064269abdc31f10eb703/crates/torus-bridge/src/native_executor.rs#L7037), [triggered rows removed by `retain`](https://github.com/OZmasterAI/Torus-hyperBFT/blob/5c756ffb8f9a95675e62064269abdc31f10eb703/crates/torus-core/src/order_book.rs#L2279).

No cycle found in the stop-trigger lifecycle. Worst-case work still scales with the number of pending stops; global admission bounds are outside this pass. Static only.
