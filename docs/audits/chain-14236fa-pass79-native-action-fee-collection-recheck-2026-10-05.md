# Pass 79 — Native-action fee collection

Reviewed `perf/item6-phase1` at `14236fa50d685649bc9bdfb6440eede7fe57d576`. This pass focuses on whether native action gas is charged into the fee distributor.

**Reconfirmed existing native fee gap; no new issue number.** `NativeExecContext.total_native_fees` is initialized to zero and repository search at this source revision finds no writes or increments. Fee distribution adds that field to the EVM receipt revenue, so native action handler `gas_used` values do not contribute to the fee split through this path. Existing repository planning notes classify native trading as gas-free; this report does not claim a defect absent a requirement to charge native action fees.

Evidence: [native_executor.rs L2419](https://github.com/OZmasterAI/Torus-hyperBFT/blob/14236fa50d685649bc9bdfb6440eede7fe57d576/crates/torus-bridge/src/native_executor.rs#L2419), [native_executor.rs L2920](https://github.com/OZmasterAI/Torus-hyperBFT/blob/14236fa50d685649bc9bdfb6440eede7fe57d576/crates/torus-bridge/src/native_executor.rs#L2920), [native_executor.rs L8658](https://github.com/OZmasterAI/Torus-hyperBFT/blob/14236fa50d685649bc9bdfb6440eede7fe57d576/crates/torus-bridge/src/native_executor.rs#L8658).

## Limits

Static review at the pinned source revision. No fault injection, live chain transaction, or Rust test was run. No source change.
