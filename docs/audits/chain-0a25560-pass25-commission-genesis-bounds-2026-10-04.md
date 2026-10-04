# Pass 25 — commission bounds at genesis and reward distribution

Reviewed 2026-10-04 against `perf/item6-phase1` at
`0a255607dbe2227ee47b70638b0198b80eaeb3cd`. Focus: commission admission from
genesis, runtime update bounds, and commission-to-delegator arithmetic.

**Reconfirmed E02; no new finding.** Runtime registration and update enforce
the 5,000-bps maximum; update also enforces a 100-bps step and cooldown. The
genesis initializer directly copies `GenesisValidator.commission_bps` into
`ValidatorState` and does not reuse the runtime bound. Inflation distribution
then calculates commission and subtracts it from the emission without first
validating the stored commission. A malformed or operator-supplied genesis
commission above 10,000 bps can therefore underflow the delegator pool in the
U256 implementation described in E02. This remains a genesis-input candidate,
not a remotely reachable runtime exploit, and is already documented in the
pass-4 economics review.

Current source anchors: genesis copies the rate at
[`genesis/lib.rs`](https://github.com/OZmasterAI/Torus-hyperBFT/blob/0a255607dbe2227ee47b70638b0198b80eaeb3cd/crates/torus-genesis/src/lib.rs#L387),
runtime registration checks it at
[`staking.rs`](https://github.com/OZmasterAI/Torus-hyperBFT/blob/0a255607dbe2227ee47b70638b0198b80eaeb3cd/crates/torus-economics/src/staking.rs#L43),
and updates check both absolute rate and delta at
[`update_commission`](https://github.com/OZmasterAI/Torus-hyperBFT/blob/0a255607dbe2227ee47b70638b0198b80eaeb3cd/crates/torus-economics/src/staking.rs#L427).
The reward subtraction remains at
[`rewards.rs`](https://github.com/OZmasterAI/Torus-hyperBFT/blob/0a255607dbe2227ee47b70638b0198b80eaeb3cd/crates/torus-economics/src/rewards.rs#L215).

The targeted hardening expectation remains to validate genesis commission
before writing any validator rows and to reject invalid persisted values before
reward arithmetic. Boundary regressions should cover 5,000, 5,001, 10,001 and
20,000 bps and assert initialization fails before partial state is left behind.
This pass did not rerun the previous Python model or execute Rust.

## Limits

Static source recheck only. No malformed genesis was initialized and no
production reward execution was run. No new issue number assigned.
