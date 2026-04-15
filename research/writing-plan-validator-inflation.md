# Writing Plan: Validator Staking Inflation

**Spec:** [tech-req-validator-inflation.md](./tech-req-validator-inflation.md)
**Date:** 2026-04-15
**Estimated scope:** ~250 lines new code + ~150 lines tests

---

## Step 1: Constants (types.rs)

**File:** `crates/torus-economics/src/types.rs`

Add after `PERMANENT_STAKE_APY_BPS` (line 325):

```rust
/// Validator inflation constant: APY = C / sqrt(TotalStaked_TRS).
/// With C=200: 20% APY at 1M staked, 6.3% at 10M, 2% at 100M.
pub const VALIDATOR_INFLATION_CONSTANT: u64 = 200;

/// Seconds per year (365 days). Used for epoch fraction calculation.
pub const SECONDS_PER_YEAR: u64 = 365 * 24 * 3600;
```

`TARGET_BLOCK_TIME_SECS` (line 310) and `BLOCKS_PER_YEAR` (line 322)
already exist.

**Verify:** `cargo check -p torus-economics`

---

## Step 2: Integer square root (rewards.rs)

**File:** `crates/torus-economics/src/rewards.rs`

Add a standalone `pub fn isqrt(n: U256) -> U256` using Newton's method.
Place it near `lerp_bps` (line 162) as a math utility.

Uses `U256` bit shift (`>> 1`, `<< n`) and division. Need to verify
`alloy_primitives::U256` supports `leading_zeros()` -- if not, use
`bit_len()` or a simpler initial guess (e.g., `n >> 1`).

**Tests to add immediately (in `mod tests`):**
- `isqrt_basic`: 0->0, 1->1, 4->2, 9->3, 100->10
- `isqrt_large_u256`: isqrt(10^36) == 10^18 (1M TRS in atrs)
- `isqrt_non_perfect`: isqrt(2)==1, isqrt(5)==2, isqrt(99)==9

**Verify:** `cargo test -p torus-economics isqrt`

---

## Step 3: Supply tracker (rewards.rs)

**File:** `crates/torus-economics/src/rewards.rs`

Add to `FeeSplitter` impl (or as standalone functions near it):

```rust
const VALIDATOR_INFLATION_KEY: &[u8] = b"validator_inflation_tracker";

fn get_cumulative_validator_inflation(staking: &StakingManager) -> Result<U256>
fn put_cumulative_validator_inflation(staking: &StakingManager, total: U256) -> Result<()>
```

Reads/writes a single borsh-encoded U256 in CF_TREASURY under
`VALIDATOR_INFLATION_KEY`. Separate from `SupplyTracker` to avoid
Borsh migration.

**Verify:** `cargo check -p torus-economics`

---

## Step 4: Core distribution function (rewards.rs)

**File:** `crates/torus-economics/src/rewards.rs`

Add to `RewardDistributor` impl:

```rust
pub fn distribute_validator_inflation(
    staking: &StakingManager,
    epoch_length_blocks: u64,
) -> Result<U256>
```

Logic:
1. Get all validators via `staking.all_validators()`
2. Filter to `status == Active` only
3. Sum `total_stake()` across active validators -> `total_active_staked`
4. Guard: if zero active or zero stake, return `Ok(U256::ZERO)`
5. Compute: `sqrt_staked = isqrt(total_active_staked)`
6. Compute: `epoch_seconds = epoch_length_blocks * TARGET_BLOCK_TIME_SECS`
7. Compute: `total_emission = total_active_staked * INFLATION_CONSTANT * epoch_seconds / (sqrt_staked * SECONDS_PER_YEAR)`
8. For each active validator (sorted deterministically for remainder):
   - `val_emission = total_emission * val.total_stake() / total_active_staked`
   - Last validator gets `total_emission - distributed_so_far`
   - Split val_emission into commission + delegator_pool
   - `credit_rewards(validator, commission)`
   - For each delegator: pro-rata share, last gets remainder
   - `credit_rewards(delegator, share)`
9. Update cumulative tracker: `put_cumulative_validator_inflation(old + total_emission)`
10. Log and return `total_emission`

**Reuse:** The per-validator commission + delegator distribution follows
the exact same pattern as `FeeSplitter::distribute_validator_rewards()`
(line 245). Consider extracting a shared helper, or just duplicate the
pattern (it's ~30 lines and the two callers have different outer loops).

**Tests to add:**
- `validator_inflation_single_validator`
- `validator_inflation_with_delegators`
- `validator_inflation_multiple_validators`
- `validator_inflation_no_active_validators`
- `validator_inflation_zero_stake`
- `validator_inflation_apy_decreases_with_stake`
- `validator_inflation_epoch_fraction`

**Verify:** `cargo test -p torus-economics validator_inflation`

---

## Step 5: Export from lib.rs

**File:** `crates/torus-economics/src/lib.rs`

No change needed -- `RewardDistributor` is already re-exported (line 27).
The new function is a method on `RewardDistributor`, so it's automatically
accessible. Just verify.

**Verify:** `cargo check -p torus-bridge`

---

## Step 6: Wire into epoch boundary (native_executor.rs)

**File:** `crates/torus-bridge/src/native_executor.rs`

In `process_epoch_boundary()` (line 1094), after the existing
`compute_new_validator_set` call, add:

```rust
// Distribute validator inflation rewards.
if let Err(e) = RewardDistributor::distribute_validator_inflation(
    &ctx.staking,
    ctx.epoch_length,
) {
    tracing::error!(%e, "validator inflation distribution failed");
}
```

This is inside the `is_epoch_boundary` guard, so it only runs at
epoch boundaries. Both `proposer.rs` (line 221) and `validator.rs`
(line 274) call `process_epoch_boundary()`, so both paths get the
new logic automatically -- no changes needed in proposer.rs or
validator.rs.

**Verify:** `cargo check -p torus-bridge`

---

## Step 7: Full test pass

Run all tests to check for regressions:

```
cargo test -p torus-economics
cargo test -p torus-bridge
cargo test -p torus-integration-tests
```

---

## Step 8: Update tech requirements doc

**File:** `research/technical-requirements.md`

Update Section 11.2 (line 1816-1817) to note that delegator rewards
now come from two sources (fee split + inflation). Add a cross-reference
to the new Section 11.5 spec.

---

## Dependency chain

```
Step 1 (constants) ─┐
                     ├── Step 4 (core function) ── Step 6 (wire) ── Step 7 (tests)
Step 2 (isqrt) ─────┤
Step 3 (tracker) ────┘
                                                                     Step 8 (doc update)
```

Steps 1, 2, 3 are independent and can be done in any order.
Step 4 depends on all three.
Step 6 depends on step 4.
Step 7 is the final verification.
Step 8 is a doc-only change, independent.
