# Pass 70 — Permanent-stake top-up timestamp

Reviewed `perf/item6-phase1` at `5c756ffb8f9a95675e62064269abdc31f10eb703`. This round isolates one behavior: whether later deposits reset the position’s recorded initial lock height.

**Top-ups preserve the original `locked_at_block`.** A new record sets the field to the current block; an existing record only increments `amount`. The comment explicitly says the timestamp tracks the initial lock, and governance unlock currently checks amount rather than elapsed time. No current reward path uses this timestamp, so the stale timestamp has no demonstrated economic consequence in this source revision.

Evidence: [staking.rs L312](https://github.com/OZmasterAI/Torus-hyperBFT/blob/5c756ffb8f9a95675e62064269abdc31f10eb703/crates/torus-economics/src/staking.rs#L312), [staking.rs L319](https://github.com/OZmasterAI/Torus-hyperBFT/blob/5c756ffb8f9a95675e62064269abdc31f10eb703/crates/torus-economics/src/staking.rs#L319), [staking.rs L322](https://github.com/OZmasterAI/Torus-hyperBFT/blob/5c756ffb8f9a95675e62064269abdc31f10eb703/crates/torus-economics/src/staking.rs#L322).

## Limits

Static source review only. No chain transaction, economic simulation, storage-failure injection, or Rust test was run. No source changes.
