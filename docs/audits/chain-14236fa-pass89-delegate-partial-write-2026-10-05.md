# Pass 89 — Delegation operation write ordering

Reviewed `perf/item6-phase1` at `14236fa50d685649bc9bdfb6440eede7fe57d576`. This pass focuses on balance debit and delegation/validator rows on a later write error.

**Hypothesis: a failed final validator-row write can leave a debit and delegation row without the aggregate update.** `delegate` debits the account, writes the delegation row, then increments and writes `validator.total_delegated`. The method does not use an atomic batch. Native action dispatch captures an operation error as a failed action result; it does not restore prior writes. A storage-fault regression should check whether earlier overlay writes are included in the committed block.

Evidence: [staking.rs L120](https://github.com/OZmasterAI/Torus-hyperBFT/blob/14236fa50d685649bc9bdfb6440eede7fe57d576/crates/torus-economics/src/staking.rs#L120), [staking.rs L131](https://github.com/OZmasterAI/Torus-hyperBFT/blob/14236fa50d685649bc9bdfb6440eede7fe57d576/crates/torus-economics/src/staking.rs#L131), [staking.rs L136](https://github.com/OZmasterAI/Torus-hyperBFT/blob/14236fa50d685649bc9bdfb6440eede7fe57d576/crates/torus-economics/src/staking.rs#L136), [native_executor.rs L4285](https://github.com/OZmasterAI/Torus-hyperBFT/blob/14236fa50d685649bc9bdfb6440eede7fe57d576/crates/torus-bridge/src/native_executor.rs#L4285).

## Limits

Static review at the pinned source revision. No fault injection, live chain transaction, or Rust test was run. No source change.
