# Pass 43 — Reduce-only allowance at matching time

Reviewed `perf/item6-phase1` at `5c756ffb8f9a95675e62064269abdc31f10eb703`. This report isolates one behavior.

This pass examines only whether reduce-only orders can increase a position when their order size exceeds the current exposure.

Placement checks that the sender has an opposite-side position to reduce, while the order book receives current signed positions for reduce-only traders and clamps each match to the available reduction. That match-time check matters because the position can change after order admission, including after earlier fills in the same block. The implementation carries reduce-only traders from the book and current submitters into the settlement map.

Evidence: [placement-side reduce-only rejection](https://github.com/OZmasterAI/Torus-hyperBFT/blob/5c756ffb8f9a95675e62064269abdc31f10eb703/crates/torus-bridge/src/native_executor.rs#L6907), [book trader set and signed-position map](https://github.com/OZmasterAI/Torus-hyperBFT/blob/5c756ffb8f9a95675e62064269abdc31f10eb703/crates/torus-bridge/src/native_executor.rs#L6919), [order-book allowance helper](https://github.com/OZmasterAI/Torus-hyperBFT/blob/5c756ffb8f9a95675e62064269abdc31f10eb703/crates/torus-core/src/order_book.rs#L377).

No new violation found in this isolated invariant. Existing test coverage was inspected by symbol, not run; `cargo` is unavailable.
