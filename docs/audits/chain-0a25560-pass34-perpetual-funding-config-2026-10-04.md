# Pass 34 — perpetual funding configuration and settlement

Reviewed source `0a255607dbe2227ee47b70638b0198b80eaeb3cd` on `perf/item6-phase1`, searching runtime use of market funding parameters and tracing fills/position persistence for recurring payments.

## Finding candidate — funding-rate parameter has no execution consumer

`MarketParams` exposes `max_funding_rate_bps`, and wallet governance input requires and signs that field. In the production Rust tree, references outside `torus-types` and the wallet are limited to one fixture/setup value in a test; no bridge/core/economics runtime reads the field. `Position` stores size, entry price, realized PnL and margin, but no funding checkpoint; the fill settlement path updates PnL and volume without periodic long/short funding transfers.

If this chain intends these markets to behave as perpetual contracts with funding (as the parameter name and product model imply), the configured cap currently has no effect and positions do not exchange funding. That permits a persistent premium/discount without the configured balancing mechanism. This is an implementation-completeness finding conditional on funding being in launch scope; repository docs should explicitly mark funding as unsupported if that is intentional.

Evidence: [field declaration](https://github.com/OZmasterAI/Torus-hyperBFT/blob/0a255607dbe2227ee47b70638b0198b80eaeb3cd/crates/torus-types/src/lib.rs#L1219), [wallet requires and populates the setting](https://github.com/OZmasterAI/Torus-hyperBFT/blob/0a255607dbe2227ee47b70638b0198b80eaeb3cd/tools/wallet/src/commands/governance.rs#L90), [position fields](https://github.com/OZmasterAI/Torus-hyperBFT/blob/0a255607dbe2227ee47b70638b0198b80eaeb3cd/crates/torus-core/src/position.rs#L45), [native settlement touches fills/volume but not funding](https://github.com/OZmasterAI/Torus-hyperBFT/blob/0a255607dbe2227ee47b70638b0198b80eaeb3cd/crates/torus-bridge/src/native_executor.rs#L5597).

## Verification limits

Static symbol search across `crates/` and source inspection. No integration test or economic simulation was run. A runtime reference search found no use of `max_funding_rate_bps` outside types/tests/wallet/research, but this does not establish the product's intended launch scope.
