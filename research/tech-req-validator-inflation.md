# Technical Requirements: Validator Staking Inflation (Section 11.5)

**Date:** 2026-04-15
**Status:** Draft v1.0
**Parent:** [technical-requirements.md](./technical-requirements.md) Section 11 Economic Modules
**Depends on:** Section 11.2 (Delegator Reward Distribution), Section 11.3 (Fee Split Logic)

---

## Summary

Add inflationary block rewards for validator stakers (validators + delegators).
This is a **second inflationary source** alongside permanent staking (Section 11.1).
Validators currently earn only from the fee-split validator share (Section 11.3);
this adds a new minting mechanism at epoch boundaries.

**Key decisions:**
- Formula: `APY = 200 / sqrt(TotalStaked_in_TRS)` (k=0.5, C=200)
- No autocompound -- rewards accumulate as claimable balance
- Inflationary -- new TRS minted each epoch
- Distributed to **active validators only** (not candidates/jailed)

---

## 1. Inflation Formula

### 1.1 APY Curve

```
APY = INFLATION_CONSTANT / sqrt(TotalActiveStaked_in_TRS)

where:
  INFLATION_CONSTANT = 200
  TotalActiveStaked_in_TRS = sum of total_stake() for all Active validators
  total_stake() = self_stake + total_delegated
  TRS conversion: TotalActiveStaked_in_TRS = TotalActiveStaked_atrs / 10^18
```

The curve is inverse-square-root: APY decreases as total stake grows,
incentivizing early participation while becoming deflationary-leaning at scale.

```
Total Staked   APY      Annual Emission   Daily Emission
---------      -----    ---------------   --------------
100K TRS       63.2%    ~63.2K TRS        ~173 TRS
500K TRS       28.3%    ~141.4K TRS       ~387 TRS
1M TRS         20.0%    ~200K TRS         ~548 TRS
5M TRS          8.9%    ~447K TRS         ~1,225 TRS
10M TRS         6.3%    ~632K TRS         ~1,733 TRS
50M TRS         2.8%    ~1.4M TRS         ~3,875 TRS
100M TRS        2.0%    ~2M TRS           ~5,479 TRS
1B TRS          0.63%   ~6.3M TRS         ~17,329 TRS
```

### 1.2 Epoch Emission Calculation

```
epoch_emission = TotalActiveStaked_atrs * INFLATION_CONSTANT * epoch_seconds
               / (isqrt(TotalActiveStaked_atrs) * SECONDS_PER_YEAR)

where:
  epoch_seconds = epoch_length_blocks * TARGET_BLOCK_TIME_SECS
```

**Why this form:** The naive formula `APY = C / sqrt(S_TRS)` requires
converting atrs to TRS (`/ 10^18`), which loses precision in integer math.
Instead, take sqrt of the raw atrs value. Since `sqrt(S_atrs) = sqrt(S_TRS * 10^18)`,
the TRS conversion cancels out naturally in the division
`S_atrs / sqrt(S_atrs) = sqrt(S_atrs)`.

Implementation:

```rust
let sqrt_staked = isqrt(total_staked_atrs);
if sqrt_staked.is_zero() { return Ok(U256::ZERO); }
let emission = total_staked_atrs
    * U256::from(VALIDATOR_INFLATION_CONSTANT)
    * U256::from(epoch_seconds)
    / (sqrt_staked * U256::from(SECONDS_PER_YEAR));
```

All arithmetic uses `U256` integer math. No floating point, no division
by `10^18`, no precision loss. Deterministic across all validators.

### 1.3 Integer Square Root

Implement Newton's method for `U256`:

```rust
/// Integer square root via Newton's method.
/// Returns floor(sqrt(n)).
pub fn isqrt(n: U256) -> U256 {
    if n.is_zero() { return U256::ZERO; }
    if n < U256::from(4u64) { return U256::from(1u64); }

    // Initial guess: 2^((bits+1)/2)
    let bits = 256 - n.leading_zeros();
    let mut x = U256::from(1u64) << ((bits + 1) / 2);

    loop {
        // x_next = (x + n / x) / 2
        let x_next = (x + n / x) >> 1;
        if x_next >= x { break; }
        x = x_next;
    }
    x
}
```

This must be deterministic and produce identical results on all nodes.
No external crate dependency -- inline implementation in `types.rs` or
a new `math.rs` utility module.

---

## 2. Distribution Mechanics

### 2.1 Who Receives Rewards

Only **active validators** and their delegators receive inflation rewards.
Candidates, jailed, and tombstoned validators are excluded.

```
Active validators = all_validators() filtered by status == Active
  For each active validator:
    Validator receives: commission portion
    Delegators receive: remainder, pro-rata by delegation amount
```

**Note:** Use the current epoch's active set (`staking.all_validators()`
filtered by `status == Active`), NOT `EpochManager::compute_new_validator_set()`
which computes the *next* epoch's set. A validator about to be rotated out
should still receive rewards for the epoch in which it was active.

### 2.2 Per-Validator Split

Same commission logic as fee distribution (Section 11.2).

Function signature:

```rust
pub fn distribute_validator_inflation(
    staking: &StakingManager,
    epoch_length_blocks: u64,
) -> Result<U256>
```

`epoch_length_blocks` comes from `NativeExecContext.epoch_length` at the
call site. Returns total emission minted.

Distribution logic:

```
total_active_staked = sum of val.total_stake() for all Active validators
epoch_seconds = epoch_length_blocks * TARGET_BLOCK_TIME_SECS
sqrt_staked = isqrt(total_active_staked)

total_emission = total_active_staked * INFLATION_CONSTANT * epoch_seconds
               / (sqrt_staked * SECONDS_PER_YEAR)

For each active validator:
  validator_emission = total_emission * val.total_stake() / total_active_staked
  (last validator gets remainder)

  commission = validator_emission * val.commission_bps / 10_000
  delegator_pool = validator_emission - commission

  credit_rewards(validator, commission)
  For each delegator:
    share = delegator_pool * del.amount / val.total_delegated
    (last delegator gets remainder)
    credit_rewards(delegator, share)
```

### 2.3 No Autocompound

Rewards are credited via `credit_rewards()` to a pending rewards balance.
Users claim with `claim_rewards()` -- tokens move to liquid balance.

- Delegation amounts do NOT change from inflation rewards
- Validator power (total_stake) does NOT change from inflation rewards
- No lockup period on claimed rewards
- User may choose to re-delegate claimed rewards manually

This matches the permanent staking reward model (Section 11.1) and the
existing fee distribution model (Section 11.2).

### 2.4 Rounding

- Each validator's share: `total_emission * val.total_stake() / total_active_staked`
- Last validator gets `total_emission - sum_of_previous` (remainder)
- Each delegator's share: `delegator_pool * del.amount / val.total_delegated`
- Last delegator gets `delegator_pool - sum_of_previous` (remainder)
- All math is U256 integer division (truncates toward zero)

---

## 3. Epoch Boundary Integration

### 3.1 Call Site

**Current state of `process_epoch_boundary()` (`native_executor.rs:1094`):**
Only calls `compute_new_validator_set()` and discards the result (`_new_set`).
No reward distribution, no status updates, no rotation cap. The following
functions exist in torus-economics, are built and tested, but NOT wired
into the bridge layer:

- `EpochManager::apply_rotation_cap(old, new, max)` -- epoch.rs:148
- `EpochManager::check_minimum_set(set)` -- epoch.rs:275
- `EpochManager::update_validator_statuses(staking, set)` -- epoch.rs:299
- `EpochManager::compute_validator_set_diff(old, new)` -- epoch.rs:86
- `EpochManager::log_rotation(old, new, diff, epoch)` -- epoch.rs:220
- `RewardDistributor::distribute_permanent_staking_rewards(staking, blocks)` -- rewards.rs:129

**This task wires all of these** plus adds the new validator inflation
distribution. The full epoch boundary flow becomes:

```
process_epoch_boundary(ctx):
    // --- Reward distribution (uses CURRENT active set) ---
    1. distribute_permanent_staking_rewards(staking, epoch_length)  // WIRE existing
    2. distribute_validator_inflation(staking, epoch_length)         // NEW

    // --- Validator set rotation ---
    3. old_set = read current active validators as ValidatorSet
    4. new_set = compute_new_validator_set(staking, max_validators, epoch+1)  // existing call
    5. new_set = apply_rotation_cap(old_set, new_set, safe_rotation_cap(old_set.len()))  // WIRE
    6. check_minimum_set(new_set)?                                  // WIRE
    7. diff = compute_validator_set_diff(old_set, new_set)          // WIRE
    8. update_validator_statuses(staking, new_set)                  // WIRE
    9. log_rotation(old_set, new_set, diff, epoch)                  // WIRE
    10. return (new_set, diff) for hotstuff_rs validator set updates
```

**Ordering rationale:**
- Steps 1-2 BEFORE rotation: rewards are for the epoch that just ended,
  so they use the current active set. A validator being rotated out still
  gets this epoch's rewards.
- Steps 3-10 are the rotation: compute new set, cap changes, update statuses.

**Step 3 — reading the old set:** `NativeExecContext` does not currently
store the previous `ValidatorSet`. Two approaches:
- **(A)** Build it from `staking.all_validators()` filtered by `status == Active`,
  converting to `ValidatorSet` format. This works because `update_validator_statuses`
  marks Active/Candidate each epoch.
- **(B)** Persist the current `ValidatorSet` in a CF key and read it back.

Recommend **(A)** — no new storage, and `all_validators()` is already called
by `compute_new_validator_set()` internally.

**Step 10 — return value:** Currently `process_epoch_boundary()` returns
`Option<NativeActionResult>`. It needs to return the diff/new set so the
consensus layer can apply `ValidatorSetUpdates` to hotstuff_rs. This
requires changing the return type — see Section 3.4.

### 3.2 Proposer-Validator Determinism

Both `proposer.rs` (line 221) and `validator.rs` (line 274) call
`process_epoch_boundary()`. All new logic goes inside that function,
so both paths execute identically. No changes to proposer.rs or
validator.rs needed for the epoch logic itself.

### 3.3 Return Type Change

Current signature:
```rust
pub fn process_epoch_boundary(ctx: &mut NativeExecContext) -> Option<NativeActionResult>
```

New signature must return the validator set diff for hotstuff_rs:
```rust
pub fn process_epoch_boundary(ctx: &mut NativeExecContext) -> Option<EpochBoundaryResult>

pub struct EpochBoundaryResult {
    pub action: NativeActionResult,
    pub new_set: Option<ValidatorSet>,
    pub diff: Option<ValidatorSetDiff>,
}
```

Callers in `proposer.rs` and `validator.rs` must be updated to handle
the new return type and apply the validator set updates. If hotstuff_rs
integration is not ready, the diff can be logged and discarded initially.

### 3.4 Edge Cases

### 3.2 Proposer-Validator Determinism

Both `proposer.rs` and `validator.rs` must call the same function in
the same position within block processing. This is critical -- any
divergence causes `StateRootMismatch` (ref: audit finding 3.4.3).

### 3.5 Edge Cases

| Condition | Behavior |
|---|---|
| No active validators | Skip rewards, skip rotation |
| Total active stake = 0 | Skip rewards -- division by zero guard |
| Single validator, no delegators | Entire emission to validator (via zero-delegators guard, regardless of commission rate) |
| Validator has 0% commission | Entire emission to delegator pool |
| Validator has 100% commission | Entire emission to validator |
| Epoch length = 0 | Skip -- epoch_fraction = 0 |
| Rotation cap exceeded | `apply_rotation_cap` defers excess changes to next epoch |
| New set below minimum (4) | `check_minimum_set` warns but does not block (dev/test mode) |
| No permanent stakers | `distribute_permanent_staking_rewards` returns zero, no-op |

---

## 4. Supply Tracking

### 4.1 SupplyTracker Update

Track cumulative validator inflation minted. The existing `SupplyTracker`
struct has two U256 fields (`cumulative_burned`, `cumulative_treasury`).
Adding a third field breaks Borsh deserialization of existing data.

**Approach: Separate key.** Store validator inflation tracker under a new
key in CF_TREASURY (e.g., `b"validator_inflation_tracker"`). No migration
needed. Existing `SupplyTracker` untouched.

```rust
/// Key in CF_TREASURY for cumulative validator inflation minted.
const VALIDATOR_INFLATION_KEY: &[u8] = b"validator_inflation_tracker";

/// Read/write a single U256 tracking total validator inflation minted.
```

### 4.2 Tracking Updates

```
cumulative_validator_inflation += epoch_emission  (updated each epoch)
```

This enables RPC queries for total minted supply:
```
total_minted = cumulative_validator_inflation + cumulative_permanent_staking_rewards
net_inflation = total_minted - cumulative_burned
```

---

## 5. Constants and Parameters

### 5.1 New Constants (types.rs)

```rust
/// Validator inflation constant: APY = C / sqrt(TotalStaked_TRS).
/// With C=200: 20% APY at 1M staked, 6.3% at 10M, 2% at 100M.
pub const VALIDATOR_INFLATION_CONSTANT: u64 = 200;

/// Seconds per year (365 days).
pub const SECONDS_PER_YEAR: u64 = 365 * 24 * 3600;
```

`BLOCKS_PER_YEAR`, `TARGET_BLOCK_TIME_SECS`, and `ONE_TRS` (10^18)
already exist or are trivially derived.

### 5.2 Governance Adjustability (Future)

`VALIDATOR_INFLATION_CONSTANT` is a compile-time constant in v1.
A future governance proposal type could make it adjustable at runtime
(stored in CF_FEE_CONFIG, read at epoch boundary). Out of scope for
this implementation.

---

## 6. Interaction with Existing Systems

### 6.1 Permanent Staking (Section 11.1)

- **Independent mechanisms.** Permanent staking rewards (5% flat APY)
  and validator inflation (200/sqrt curve) are computed and distributed
  separately.
- A user who permanently stakes AND delegates earns both reward streams
  on their respective token pools (no double-counting -- the tokens are
  separate).
- The two systems share no state except supply tracking for aggregate
  metrics.

### 6.2 Fee Split (Section 11.3)

- Fee-split validator share (0% to 25% over 5 years) is **additive** to
  inflation rewards. Validators earn from both sources.
- Fee-split rewards use `distribute_block_fees()` (per-block).
  Inflation rewards use `distribute_validator_inflation()` (per-epoch).
- Both use the same `credit_rewards()` path -- claimable by the user.

### 6.3 Tech Requirements Section 11.2 Update

Section 11.2 currently states:

> "Delegator rewards come entirely from the validator's fee split share --
>  no additional inflation."

This must be updated to:

> "Delegator rewards come from two sources: (1) the validator's fee split
>  share (Section 11.3), distributed per-block, and (2) validator staking
>  inflation (Section 11.5), distributed per-epoch. Both are credited to the
>  delegator's claimable rewards balance."

---

## 7. Files to Modify

| File | Change | Scope |
|---|---|---|
| `torus-economics/src/types.rs` | Add `VALIDATOR_INFLATION_CONSTANT`, `SECONDS_PER_YEAR` | Small |
| `torus-economics/src/rewards.rs` | Add `distribute_validator_inflation()` + `isqrt()` | Medium |
| `torus-economics/src/rewards.rs` | Add validator inflation tracker (separate CF_TREASURY key) | Small |
| `torus-bridge/src/native_executor.rs` | Rewrite `process_epoch_boundary()` with full flow (10 steps), add `EpochBoundaryResult` struct | Medium |
| `torus-bridge/src/proposer.rs` | Handle new `EpochBoundaryResult` return type | Small |
| `torus-bridge/src/validator.rs` | Handle new `EpochBoundaryResult` return type | Small |

**Not modified:** `staking.rs` (existing functions are sufficient),
`governance.rs`, `epoch.rs` (all functions already built — only wiring needed),
`ValidatorState` struct.

---

## 8. Test Requirements

### 8.1 Unit Tests (in rewards.rs)

| Test | Verifies |
|---|---|
| `isqrt_basic` | sqrt(0)=0, sqrt(1)=1, sqrt(4)=2, sqrt(100)=10 |
| `isqrt_large_u256` | sqrt of 10^36 (1M TRS in atrs) = 10^18 |
| `isqrt_non_perfect` | sqrt(2) = 1, sqrt(5) = 2 (floor) |
| `validator_inflation_single_validator` | One validator, no delegators -- gets full emission |
| `validator_inflation_with_delegators` | Commission split + pro-rata delegator distribution |
| `validator_inflation_multiple_validators` | Pro-rata across validators by total_stake, remainder to last |
| `validator_inflation_no_active_validators` | Returns zero emission, no mint |
| `validator_inflation_zero_stake` | Division-by-zero guard |
| `validator_inflation_apy_decreases_with_stake` | Higher total stake produces lower per-unit reward |
| `validator_inflation_epoch_fraction` | Emission scales correctly with epoch length |

### 8.2 Integration Tests

| Test | Verifies |
|---|---|
| `epoch_boundary_distributes_inflation` | Full epoch boundary flow mints and distributes correctly |
| `epoch_boundary_distributes_permanent_rewards` | Permanent staking rewards are distributed at epoch boundary |
| `epoch_boundary_rotates_validators` | Validator set rotation applies, statuses updated |
| `epoch_boundary_rotation_cap` | Excess validator changes deferred to next epoch |
| `proposer_validator_state_root_match` | Both paths produce identical state after full epoch boundary |
| `inflation_rewards_claimable` | Distributed rewards appear in `get_pending_rewards` and can be claimed |
| `inflation_does_not_change_delegation_amounts` | Delegation amounts and validator power unchanged after distribution |
| `epoch_boundary_returns_diff` | `EpochBoundaryResult` contains correct inserts/deletes for hotstuff_rs |

---

## 9. Out of Scope

- Autocompounding (rewards do NOT re-stake)
- Governance adjustability of inflation constant (future)
- Emission cap / max epoch emission (not needed -- sqrt curve is self-limiting)
- Interaction with slashing (slashed validators are jailed, excluded from active set, no rewards)
- EVM precompile for querying inflation APY (future RPC addition)
- **Oracle aggregation and liquidation wiring** (separate plan): `aggregate_oracle_prices()`
  and `run_liquidation_checks()` are defined in native_executor.rs but not called from
  proposer/validator paths. Trading engine concerns -- unrelated to epoch economics.
- **hotstuff_rs ValidatorSetUpdates integration**: The epoch boundary will return
  the diff, but actually applying it to the hotstuff_rs consensus layer may require
  additional work depending on the consensus integration state.
