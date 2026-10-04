# Pass 66 — Validator self-stake share of inflation

Reviewed `perf/item6-phase1` at `5c756ffb8f9a95675e62064269abdc31f10eb703`. This round isolates one behavior: allocation of a validator emission between self-stake and delegations.

**Hypothesis: self-stake does not receive its pro-rata share of validator inflation when delegators exist.** Each validator’s emission is weighted by `total_stake()`, which includes self-stake and delegation. The distributor then takes commission from the whole validator emission and sends the entire remaining delegator pool across delegated balances; it does not separately credit the validator’s self-stake fraction. With nonzero self-stake and delegations, the validator appears to receive only commission, while delegators divide the rest. If policy intends self-stake to earn proportionately, this misallocates rewards. A small two-component numerical regression and tokenomics confirmation are needed.

Evidence: [rewards.rs L183](https://github.com/OZmasterAI/Torus-hyperBFT/blob/5c756ffb8f9a95675e62064269abdc31f10eb703/crates/torus-economics/src/rewards.rs#L183), [rewards.rs L215](https://github.com/OZmasterAI/Torus-hyperBFT/blob/5c756ffb8f9a95675e62064269abdc31f10eb703/crates/torus-economics/src/rewards.rs#L215), [rewards.rs L231](https://github.com/OZmasterAI/Torus-hyperBFT/blob/5c756ffb8f9a95675e62064269abdc31f10eb703/crates/torus-economics/src/rewards.rs#L231).

## Limits

Static source review only. No chain transaction, economic simulation, storage-failure injection, or Rust test was run. No source changes.
