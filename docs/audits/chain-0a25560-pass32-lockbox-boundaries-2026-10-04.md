# Pass 32 — lockbox boundary and delayed settlement review

Reviewed source `0a255607dbe2227ee47b70638b0198b80eaeb3cd` on `perf/item6-phase1`, focusing on EVM/native unit conversion, queueing, and settlement.

## Result

The normal conversion paths are deliberately strict: EVM deposits require `amountWei == msg.value`, reject nonzero all-dust deposits, and floor only sub-native-unit remainder; EVM withdrawals require a multiple of `10^10`. Native-action transfers use atomic paired writes. These checks close the common unit mismatch and torn-transfer cases.

A narrow liveness/funds-loss edge remains at delayed deposit settlement. The EVM frame burns the value and queues the native credit, while next-block `credit_native` performs a checked addition to the account's native `available`. If that addition overflows `i128`, settlement returns an action error after the queue drain has removed the queued entry. The EVM debit has already committed, so that deposit has no automatic retry or refund. Reaching this requires an account balance near the representable `FixedPoint` ceiling and a further valid deposit; no ordinary balance path or practical supply bound was established in this review. Treat as a boundary-condition hypothesis, not a demonstrated user-reachable exploit.

Evidence: [precompile amount checks](https://github.com/OZmasterAI/Torus-hyperBFT/blob/0a255607dbe2227ee47b70638b0198b80eaeb3cd/crates/torus-core/src/precompiles.rs#L1064), [native credit overflow](https://github.com/OZmasterAI/Torus-hyperBFT/blob/0a255607dbe2227ee47b70638b0198b80eaeb3cd/crates/torus-core/src/lockbox.rs#L113), [delayed settlement result](https://github.com/OZmasterAI/Torus-hyperBFT/blob/0a255607dbe2227ee47b70638b0198b80eaeb3cd/crates/torus-bridge/src/native_executor.rs#L8536), [queue drain](https://github.com/OZmasterAI/Torus-hyperBFT/blob/0a255607dbe2227ee47b70638b0198b80eaeb3cd/crates/torus-core/src/precompiles.rs#L1172).

## Verification limits

Static source review only; no Rust tests or reachable-balance construction was run. The general next-block acknowledgement/failure behavior was already documented in pass 9; this pass isolates the lockbox's asymmetric irreversible EVM leg and checked native credit.
