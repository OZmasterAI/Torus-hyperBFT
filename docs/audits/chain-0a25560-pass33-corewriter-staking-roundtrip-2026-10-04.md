# Pass 33 — CoreWriter queue and staking action roundtrip

Reviewed source `0a255607dbe2227ee47b70638b0198b80eaeb3cd` on `perf/item6-phase1`, focusing on CoreWriter admission, deterministic sequence allocation, next-block draining, and staking action conversion.

## Result

CoreWriter returns queue acceptance rather than execution success. For staking actions, ABI amounts are bounded before `u128 → i128`, the queue assigns ordered `(target block, sequence)` keys, and drain converts Delegate, Undelegate, LockPermanent, ClaimRewards, and ClaimUnbonded into the same native action handlers used by signed native actions. The stateful staking validation therefore occurs at execution time, not when the EVM call returns.

The delayed-failure limitation is real: the drain deletes entries before native handlers run, and per-action errors are returned as results rather than requeued. This is already captured as the general CoreWriter acknowledgement risk in pass 9; this pass found no distinct staking-specific bypass or accounting rule violation. A caller must treat `true` / zero-return placeholders as “accepted into the queue,” and should check the later native execution result before assuming a stake change or reward claim occurred.

Evidence: [amount bounds and enqueues](https://github.com/OZmasterAI/Torus-hyperBFT/blob/0a255607dbe2227ee47b70638b0198b80eaeb3cd/crates/torus-core/src/precompiles.rs#L914), [queue drain/deletion](https://github.com/OZmasterAI/Torus-hyperBFT/blob/0a255607dbe2227ee47b70638b0198b80eaeb3cd/crates/torus-core/src/precompiles.rs#L1172), [next-block action conversion](https://github.com/OZmasterAI/Torus-hyperBFT/blob/0a255607dbe2227ee47b70638b0198b80eaeb3cd/crates/torus-bridge/src/native_executor.rs#L8889), [drain result creation](https://github.com/OZmasterAI/Torus-hyperBFT/blob/0a255607dbe2227ee47b70638b0198b80eaeb3cd/crates/torus-bridge/src/native_executor.rs#L8497).

## Verification limits

Static source review only. Existing pass 9 covers delayed queue failure visibility; no separate test was run to extend that result.
