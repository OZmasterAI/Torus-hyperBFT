# Writing Plan: Validator Staking Inflation + Epoch Boundary Wiring

**Spec:** [tech-req-validator-inflation.md](./tech-req-validator-inflation.md)
**Date:** 2026-04-15
**Estimated scope:** ~350 lines new code + ~200 lines tests

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

Add as standalone functions near the existing `SupplyTracker` helpers:

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
pattern (~30 lines, two callers have different outer loops).

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

## Step 5: EpochBoundaryResult type (native_executor.rs)

**File:** `crates/torus-bridge/src/native_executor.rs`

Add a new return type near `NativeActionResult`:

```rust
pub struct EpochBoundaryResult {
    pub action: NativeActionResult,
    pub new_set: Option<ValidatorSet>,
    pub diff: Option<ValidatorSetDiff>,
}
```

Will need to import `ValidatorSet` from `torus_types` and
`ValidatorSetDiff` from `torus_economics::epoch`.

**Verify:** `cargo check -p torus-bridge`

---

## Step 6: Rewrite process_epoch_boundary (native_executor.rs)

**File:** `crates/torus-bridge/src/native_executor.rs`

Replace the current `process_epoch_boundary()` (lines 1094-1107) with
the full 10-step flow from the tech requirements Section 3.1.

Current (hollow):
```rust
pub fn process_epoch_boundary(ctx: &mut NativeExecContext) -> Option<NativeActionResult> {
    if !EpochManager::is_epoch_boundary(...) { return None; }
    match EpochManager::compute_new_validator_set(...) {
        Ok(_new_set) => Some(NativeActionResult::ok(...)),
        Err(e) => Some(NativeActionResult::err(...)),
    }
}
```

New:
```rust
pub fn process_epoch_boundary(ctx: &mut NativeExecContext) -> Option<EpochBoundaryResult> {
    if !EpochManager::is_epoch_boundary(ctx.block_height, ctx.epoch_length) {
        return None;
    }

    // --- Phase A: Reward distribution (current active set) ---

    // A1: Permanent staking rewards (existing function, newly wired)
    if let Err(e) = RewardDistributor::distribute_permanent_staking_rewards(
        &ctx.staking, ctx.epoch_length,
    ) {
        tracing::error!(%e, "permanent staking rewards failed");
    }

    // A2: Validator inflation rewards (NEW)
    if let Err(e) = RewardDistributor::distribute_validator_inflation(
        &ctx.staking, ctx.epoch_length,
    ) {
        tracing::error!(%e, "validator inflation distribution failed");
    }

    // --- Phase B: Validator set rotation ---

    // B1: Build old set from current Active validators
    let old_set = build_current_validator_set(&ctx.staking, ctx.epoch);

    // B2: Compute new set (existing call -- now use the result)
    let new_set = match EpochManager::compute_new_validator_set(
        &ctx.staking, ctx.max_validators, ctx.epoch + 1,
    ) {
        Ok(set) => set,
        Err(e) => return Some(EpochBoundaryResult {
            action: NativeActionResult::err("epoch_rotation", e.to_string()),
            new_set: None, diff: None,
        }),
    };

    // B3: Apply rotation cap
    let cap = EpochManager::safe_rotation_cap(old_set.validators.len());
    let new_set = EpochManager::apply_rotation_cap(&old_set, new_set, cap);

    // B4: Check minimum set
    if let Err(e) = EpochManager::check_minimum_set(&new_set) {
        tracing::error!(%e, "validator set below minimum");
    }

    // B5: Compute diff
    let diff = EpochManager::compute_validator_set_diff(&old_set, &new_set);

    // B6: Update statuses
    if let Err(e) = EpochManager::update_validator_statuses(&ctx.staking, &new_set) {
        tracing::error!(%e, "validator status update failed");
    }

    // B7: Log
    EpochManager::log_rotation(&old_set, &new_set, &diff, ctx.epoch + 1);

    Some(EpochBoundaryResult {
        action: NativeActionResult::ok("epoch_boundary", 5000),
        new_set: Some(new_set),
        diff: Some(diff),
    })
}
```

**Helper needed:** `build_current_validator_set(staking, epoch) -> ValidatorSet`
-- reads `all_validators()`, filters `status == Active`, converts to
`ValidatorSet` format. ~15 lines.

**Verify:** `cargo check -p torus-bridge`

---

## Step 7: Update callers (proposer.rs + validator.rs)

**Files:**
- `crates/torus-bridge/src/proposer.rs` (line 221)
- `crates/torus-bridge/src/validator.rs` (line 274)

Both currently do:
```rust
NativeExecutor::process_epoch_boundary(&mut ctx);
```

Update to handle new return type. For now, log the diff but don't
apply to hotstuff_rs (that integration is out of scope):

```rust
if let Some(epoch_result) = NativeExecutor::process_epoch_boundary(&mut ctx) {
    if let Some(ref diff) = epoch_result.diff {
        if !diff.is_empty() {
            tracing::info!(
                inserts = diff.inserts.len(),
                deletes = diff.deletes.len(),
                "epoch boundary: validator set changed"
            );
        }
    }
}
```

Both files get identical code -- deterministic.

**Verify:** `cargo check -p torus-bridge`

---

## Step 8: Full test pass

Run all tests to check for regressions:

```
cargo test -p torus-economics
cargo test -p torus-bridge
cargo test -p torus-integration-tests
```

Pay special attention to:
- Existing `epoch_boundary_*` tests in `torus-bridge/tests/native_bridge_tests.rs`
- Existing `process_epoch_boundary` tests in `torus-integration-tests/tests/chaos.rs`
- Any tests that match on the old `Option<NativeActionResult>` return type

---

## Step 9: Update tech requirements doc

**File:** `research/technical-requirements.md`

Update Section 11.2 (line 1816-1817) to note that delegator rewards
now come from two sources (fee split + inflation). Add a cross-reference
to the new Section 11.5 spec.

---

## Dependency chain

```
Step 1 (constants) ─────┐
Step 2 (isqrt) ──────────┼── Step 4 (inflation fn) ──┐
Step 3 (tracker) ────────┘                            │
                                                      │
Step 5 (result type) ─── Step 6 (rewrite epoch) ─────┼── Step 8 (tests)
                              │                       │
                         Step 7 (callers) ────────────┘
                                                           Step 9 (doc)
```

Steps 1, 2, 3, 5 are independent -- can be done in any order.
Step 4 depends on 1+2+3.
Step 6 depends on 4+5.
Step 7 depends on 6.
Step 8 is the final verification after everything compiles.
Step 9 is a doc-only change, independent.
