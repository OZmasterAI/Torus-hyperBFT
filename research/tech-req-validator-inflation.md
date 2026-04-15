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

**Current state of `process_epoch_boundary()`:** Only calls
`compute_new_validator_set()` and discards the result (`_new_set`).
No reward distribution, no status updates, no rotation cap.
`distribute_permanent_staking_rewards()` and `update_validator_statuses()`
exist in torus-economics but are NOT wired into the bridge layer.
Full epoch boundary wiring is a **separate plan**.

This task adds `distribute_validator_inflation()` to `process_epoch_boundary()`:

```
process_epoch_boundary(ctx):
    1. compute_new_validator_set()        // existing (result currently discarded)
    2. distribute_validator_inflation()   // NEW — this task
```

The new function must appear in both `proposer.rs` and `validator.rs`
at the same position to maintain deterministic state roots.

### 3.2 Proposer-Validator Determinism

Both `proposer.rs` and `validator.rs` must call the same function in
the same position within block processing. This is critical -- any
divergence causes `StateRootMismatch` (ref: audit finding 3.4.3).

### 3.3 Edge Cases

| Condition | Behavior |
|---|---|
| No active validators | Skip -- no emission, no mint |
| Total active stake = 0 | Skip -- division by zero guard |
| Single validator, no delegators | Entire emission to validator (via zero-delegators guard, regardless of commission rate) |
| Validator has 0% commission | Entire emission to delegator pool |
| Validator has 100% commission | Entire emission to validator |
| Epoch length = 0 | Skip -- epoch_fraction = 0 |

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
| `torus-bridge/src/native_executor.rs` | Wire into `process_epoch_boundary()` | Small |
| `torus-bridge/src/proposer.rs` | Ensure epoch boundary calls match validator path | Small |
| `torus-bridge/src/validator.rs` | Ensure epoch boundary calls match proposer path | Small |

**Not modified:** `staking.rs` (existing `credit_rewards`, `get_validator`,
`delegations_for_validator` are sufficient), `governance.rs`, `epoch.rs`,
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
| `proposer_validator_state_root_match` | Both paths produce identical state after inflation distribution |
| `inflation_rewards_claimable` | Distributed rewards appear in `get_pending_rewards` and can be claimed |
| `inflation_does_not_change_delegation_amounts` | Delegation amounts and validator power unchanged after distribution |

---

## 9. Out of Scope

- Autocompounding (rewards do NOT re-stake)
- Governance adjustability of inflation constant (future)
- Emission cap / max epoch emission (not needed -- sqrt curve is self-limiting)
- Interaction with slashing (slashed validators are jailed, excluded from active set, no rewards)
- EVM precompile for querying inflation APY (future RPC addition)
- **Full epoch boundary wiring** (separate plan): applying computed validator
  sets, rotation cap, status updates, permanent staking reward distribution.
  These are built and tested in torus-economics but not wired into the
  bridge layer. This task only adds the new `distribute_validator_inflation()`
  call to the existing `process_epoch_boundary()` function.
