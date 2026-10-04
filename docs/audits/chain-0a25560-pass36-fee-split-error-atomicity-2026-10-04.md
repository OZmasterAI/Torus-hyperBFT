# Pass 36 — fee split write failures and block handling

Reviewed source `0a255607dbe2227ee47b70638b0198b80eaeb3cd` on `perf/item6-phase1`, focusing on fee-split write ordering, rollback guarantees, and consensus app handling of distribution errors.

## Finding candidate — a failed fee split can leave partial credits while block execution continues

`RewardDistributor::distribute_block_fees` credits the validator share first (which may itself perform multiple reward-row writes), then treasury, then dev pool. These are separate state writes; the distributor does not stage them in a single atomic batch or restore earlier credits if a later write fails. The native block app calls `distribute_fees` and discards its returned `NativeActionResult`, then continues into epoch processing. The failure therefore is not promoted to the existing fatal-error path.

On a backend error after one or more successful credits, the fee action can leave partial credits in the block overlay while the block continues. Because the failure can be local storage I/O or an individual corrupt/overflowing reward row, nodes may apply different partial fee state. This is a fail-stop/atomicity concern around a consensus-critical economic write, not a rounding issue. A focused mock-backend test should fail each write position and establish whether the overlay is flushed and state roots diverge; harden by making distribution atomic or treating failure as fatal before committing the block.

Evidence: [ordered independent credits](https://github.com/OZmasterAI/Torus-hyperBFT/blob/0a255607dbe2227ee47b70638b0198b80eaeb3cd/crates/torus-economics/src/rewards.rs#L62), [per-delegator writes](https://github.com/OZmasterAI/Torus-hyperBFT/blob/0a255607dbe2227ee47b70638b0198b80eaeb3cd/crates/torus-economics/src/rewards.rs#L95), [error is returned as an action result](https://github.com/OZmasterAI/Torus-hyperBFT/blob/0a255607dbe2227ee47b70638b0198b80eaeb3cd/crates/torus-bridge/src/native_executor.rs#L8653), [application discards it and proceeds](https://github.com/OZmasterAI/Torus-hyperBFT/blob/0a255607dbe2227ee47b70638b0198b80eaeb3cd/crates/torus-consensus/src/app.rs#L2305).

## Verification limits

Static source trace only; no fault-injection test or multi-node reproduction was run. The report identifies a credible failure mode, but its final persistence behavior should be confirmed against the concrete execution overlay and flush path before assigning exploit severity.
