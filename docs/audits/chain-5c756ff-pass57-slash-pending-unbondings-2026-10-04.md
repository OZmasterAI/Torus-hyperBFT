# Pass 57 — Slashing of pending undelegations

Reviewed `perf/item6-phase1` at `5c756ffb8f9a95675e62064269abdc31f10eb703`. This round isolates one behavior: slash coverage after undelegation begins.

**Hypothesis: pending undelegation principal escapes later validator slashes.** `slash` iterates delegation rows but applies the fraction only to `del.amount`; it leaves each row’s `unbonding` entries unchanged. Once `undelegate` moves principal out of `amount`, a slash processed during the unbonding window does not reduce that queued amount, and `claim_unbonded` credits it at face value at maturity. If the protocol intends in-flight unbondings to remain slashable for prior faults, an operator can begin exit before a delayed slash and withdraw unslashed principal. Whether evidence timing and protocol rules permit this sequence needs runtime/spec confirmation.

Evidence: [staking.rs L471](https://github.com/OZmasterAI/Torus-hyperBFT/blob/5c756ffb8f9a95675e62064269abdc31f10eb703/crates/torus-economics/src/staking.rs#L471), [staking.rs L173](https://github.com/OZmasterAI/Torus-hyperBFT/blob/5c756ffb8f9a95675e62064269abdc31f10eb703/crates/torus-economics/src/staking.rs#L173), [staking.rs L249](https://github.com/OZmasterAI/Torus-hyperBFT/blob/5c756ffb8f9a95675e62064269abdc31f10eb703/crates/torus-economics/src/staking.rs#L249).

## Limits

Static source review only. No chain transaction, economic simulation, storage-failure injection, or Rust test was run. No source changes.
