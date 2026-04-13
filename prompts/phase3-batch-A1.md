# Phase 3 — Batch A1: Slashing + Jailing + Unjail (Tasks 3.1.1, 3.1.2, 3.1.3)

## Context

Torus-hyperBFT is an EVM-compatible L1 blockchain built in Rust. Phase 1 (core infra) and Phase 2 (DeFi + economics) are complete. Phase 3 is production hardening. This batch implements **core validator accountability** — the foundation all other security work builds on.

**Crate layout** (all under `crates/`):
- `torus-types` — shared types, no internal deps
- `torus-economics` — staking, governance, fee distribution
- `torus-state` — RocksDB state storage
- `torus-consensus` — hotstuff_rs integration, `TorusApp`
- `torus-bridge` — native action executor (cross-VM bridge)
- `torus-core` — block pipeline
- `torus-network` — P2P networking
- `torus-integration-tests` — E2E test crate

---

## Existing Infrastructure (already built — DO NOT recreate)

### Types (`torus-types/src/lib.rs`)
```rust
pub struct PublicKey(pub [u8; 32]);  // Ed25519
pub struct ValidatorInfo { pub address: Address, pub pubkey: PublicKey, pub power: u64, pub commission_bps: u16 }
pub struct ValidatorSet { pub validators: Vec<ValidatorInfo>, pub epoch: u64 }

// NativeAction already has these variants (with EIP-712 hashes in eip712.rs):
pub enum NativeAction {
    // ... existing variants ...
    JailVote { target: Address },
    UnjailSelf,
}
```

### Validator State (`torus-economics/src/types.rs`)
```rust
pub enum ValidatorStatus { Candidate, Active, Jailed, Tombstoned }

pub struct ValidatorState {
    pub address: Address,
    pub pubkey: [u8; 32],
    pub commission_bps: u16,
    pub self_stake: U256,
    pub total_delegated: U256,
    pub status: ValidatorStatus,
    pub jailed_until: Option<u64>,  // already exists, unused
}
```

### Staking Manager (`torus-economics/src/staking.rs`)
`StakingManager` wraps `StateDb`. Already has:
- `register_validator()`, `delegate()`, `undelegate()`, `claim_rewards()`
- `get_validator(addr)`, `put_validator(addr, state)`, `all_validators()`
- `delegations_for_validator(validator) -> Vec<Delegation>`

### Epoch Manager (`torus-economics/src/epoch.rs`)
- `compute_new_validator_set()` already skips `Jailed | Tombstoned` validators
- `update_validator_statuses()` only promotes `Active | Candidate`
- Epoch boundary detected by `is_epoch_boundary(block_height, epoch_length)`

### Native Executor (`torus-bridge/src/native_executor.rs`)
- `JailVote` and `UnjailSelf` are **stubs** (lines ~212-213) — return `NativeActionResult::ok(...)` with no logic

### Consensus (`torus-consensus/src/app.rs`)
- `TorusApp` implements hotstuff_rs `App` trait
- Validators identified by Ed25519 `VerifyingKey` (same bytes as `PublicKey`)
- hotstuff_rs uses `PhaseVote { chain_id, view, block, phase }` — signed with borsh-serialized `(chain_id, view, block, phase)`
- Double-sign = same `(chain_id, view, phase)` but different `block` hash from same `VerifyingKey`
- Network layer (`torus-consensus/src/network.rs`) is where raw `(VerifyingKey, Message)` tuples are observable

### State Storage (`torus-state/src/cf.rs`)
- `cf_staking_validators` — key: Address(20), value: Borsh `ValidatorState`
- `cf_staking_delegations` — key: delegator(20)++validator(20), value: Borsh `Delegation`

### Governance (`torus-economics/src/governance.rs`)
- `GovernanceManager` with proposals, voting, finalization
- `ProposalType` enum: `ParameterChange | TreasurySpend | MarketListing | TextProposal`
- Vote weight = delegated stake + (permanent stake * multiplier)

### Error Types (`torus-economics/src/lib.rs` or `types.rs`)
- `EconomicsError::ValidatorJailed(Address)` — already defined
- `EconomicsError::ValidatorTombstoned(Address)` — already defined

---

## Task 3.1.1 — Slashing Implementation: Double-Sign Detection

### What to build

**1. Slashing types** — add to `torus-types/src/lib.rs` or a new `torus-types/src/slashing.rs`:

```rust
/// Evidence of a double-sign infraction
pub struct DoubleSignEvidence {
    pub validator_pubkey: PublicKey,
    pub view: u64,
    pub phase: u8,               // map from hotstuff_rs Phase enum
    pub block_hash_1: B256,
    pub block_hash_2: B256,
    pub signature_1: [u8; 64],
    pub signature_2: [u8; 64],
    pub chain_id: u64,
}

/// Record of a slashing event (persisted)
pub struct SlashRecord {
    pub validator: Address,
    pub block_height: u64,
    pub infraction_type: InfractionType,
    pub slash_fraction_bps: u16,  // basis points (e.g., 500 = 5%)
    pub amount_slashed: U256,
    pub timestamp: u64,
}

pub enum InfractionType {
    DoubleSign,
    Downtime,
}
```

**2. Double-sign detector** — add to `torus-consensus/src/` (new file `slashing.rs`):

- `DoubleSignDetector` struct that tracks recently seen `PhaseVote`s
- Key: `(VerifyingKey, ViewNumber, Phase)` → `CryptoHash` (block hash voted for)
- On receiving a vote: check if we already have a vote from this validator for the same `(view, phase)` with a different block hash
- If so, construct `DoubleSignEvidence` and emit it
- Integrate with the `Network` recv path or the `App` validation path — wherever incoming votes are processed
- Evidence must be **cryptographically verifiable**: anyone with the two signed messages + the public key can independently verify the double-sign
- Sliding window: only track votes for recent views (last N views, configurable, default ~100) to bound memory

**3. Slash execution** — add to `StakingManager` in `torus-economics/src/staking.rs`:

```rust
/// Slash a validator's stake by fraction_bps (basis points).
/// Burns slashed tokens (removes from total supply).
/// Also slashes proportionally from all delegators.
pub fn slash_validator(&mut self, validator_addr: &Address, fraction_bps: u16, block_height: u64, infraction: InfractionType) -> Result<SlashRecord>
```

Implementation rules:
- Slash the validator's `self_stake` by `fraction_bps`
- Slash each delegator's delegation by the same `fraction_bps`
- Update `total_delegated` accordingly
- Burn slashed tokens (reduce from total supply, do NOT redistribute)
- If `self_stake` drops below `MIN_SELF_DELEGATION`, auto-jail the validator
- Record the `SlashRecord` for auditability
- **Double-sign slash fraction**: 5% (500 bps) — define as constant `DOUBLE_SIGN_SLASH_BPS`
- **Downtime slash fraction**: 0.1% (10 bps) — define as constant `DOWNTIME_SLASH_BPS`

**4. State storage for slash records** — add a new column family in `torus-state/src/cf.rs`:
- `cf_slashing_records` — key: `validator_addr(20) ++ block_height(8)`, value: Borsh `SlashRecord`
- Add query method to retrieve slash history for a validator

### Constants to define
```rust
pub const DOUBLE_SIGN_SLASH_BPS: u16 = 500;    // 5%
pub const DOWNTIME_SLASH_BPS: u16 = 10;        // 0.1%
pub const DOUBLE_SIGN_JAIL_DURATION: u64 = 0;  // permanent (tombstoned)
pub const DOWNTIME_JAIL_DURATION: u64 = 28_800; // ~2 days at 6s blocks
pub const EVIDENCE_MAX_AGE_VIEWS: u64 = 100;    // sliding window
```

---

## Task 3.1.2 — Jailing: Validator Downtime Detection + Jail Vote

### What to build

**1. Downtime tracker** — add to `torus-consensus/src/` (new file or extend `slashing.rs`):

- Track which validators signed blocks in recent windows
- `DowntimeTracker` struct:
  - Sliding window of last N blocks (default: `DOWNTIME_WINDOW = 1000`)
  - For each block, record which validators signed (from QC signatures)
  - A validator is "down" if they signed fewer than `DOWNTIME_THRESHOLD_PCT`% of blocks in the window (default: 50%)
- Call `check_downtime()` at epoch boundaries or every N blocks
- When downtime detected: auto-slash at `DOWNTIME_SLASH_BPS` + jail for `DOWNTIME_JAIL_DURATION`

**2. Jail mechanism** — add to `StakingManager`:

```rust
/// Jail a validator until a specific block height.
/// Sets status to Jailed, records jailed_until.
/// For double-sign: jailed_until = None (use Tombstoned instead).
pub fn jail_validator(&mut self, validator_addr: &Address, until_block: u64) -> Result<()>

/// Tombstone a validator permanently (double-sign).
/// Sets status to Tombstoned, prevents all future participation.
/// Delegators can still undelegate (with unbonding period).
pub fn tombstone_validator(&mut self, validator_addr: &Address) -> Result<()>
```

Implementation:
- `jail_validator`: set `status = Jailed`, set `jailed_until = Some(until_block)`, remove from active validator set at next epoch
- `tombstone_validator`: set `status = Tombstoned`, `jailed_until = None`, permanently excluded
- Jailed validators: cannot produce blocks, cannot earn rewards, delegators stop earning
- Jailed validators' delegators CAN undelegate (don't trap delegator funds)

**3. JailVote processing** — wire up in `torus-bridge/src/native_executor.rs`:

Replace the `JailVote` stub with real logic:
- `JailVote { target }` is a vote from one active validator to jail another
- Tally votes: when >2/3 of active validators (by stake weight) vote to jail a target, execute the jail
- Store jail votes in state: `cf_jail_votes` — key: `target(20) ++ voter(20)`, value: `block_height(8)`
- Votes expire after `JAIL_VOTE_EXPIRY = 14_400` blocks (~1 day)
- On reaching threshold: call `jail_validator(target, current_block + DOWNTIME_JAIL_DURATION)`
- Emit appropriate events/logs

### Constants to define
```rust
pub const DOWNTIME_WINDOW: u64 = 1000;           // blocks to check
pub const DOWNTIME_THRESHOLD_PCT: u8 = 50;       // must sign 50% of blocks
pub const JAIL_VOTE_THRESHOLD_BPS: u16 = 6667;   // 2/3 supermajority by stake
pub const JAIL_VOTE_EXPIRY: u64 = 14_400;        // ~1 day
```

---

## Task 3.1.3 — Unjail Mechanism with Cooldown

### What to build

**1. Unjail logic** — add to `StakingManager`:

```rust
/// Unjail a validator if cooldown has passed and self-stake meets minimum.
/// Only the jailed validator themselves can call this (via UnjailSelf action).
pub fn unjail_validator(&mut self, validator_addr: &Address, current_block: u64) -> Result<()>
```

Implementation:
- Verify `status == Jailed` (not `Tombstoned` — tombstoned is permanent)
- Verify `current_block >= jailed_until.unwrap()` (cooldown expired)
- Verify `self_stake >= MIN_SELF_DELEGATION` (must re-stake if slashed below minimum)
- Set `status = Candidate` (NOT `Active` — must wait for next epoch to re-enter active set)
- Clear `jailed_until`
- Clear any pending jail votes against this validator

**2. UnjailSelf processing** — wire up in `torus-bridge/src/native_executor.rs`:

Replace the `UnjailSelf` stub:
- `UnjailSelf` — sender must be the jailed validator's address
- Call `unjail_validator(sender, current_block)`
- Return appropriate success/error result

**3. Cooldown enforcement**:
- After unjailing, validator enters `Candidate` status
- Must wait until next epoch boundary to potentially become `Active` again
- If they were slashed below `MIN_SELF_DELEGATION`, they must `delegate()` to themselves first to meet the minimum before calling `UnjailSelf`

---

## Testing Requirements

### Unit tests — add to each module

**`torus-economics/src/staking.rs` tests:**
1. `test_slash_validator_basic` — slash 5%, verify self_stake and delegator amounts reduced correctly
2. `test_slash_validator_burns_tokens` — verify slashed amount is removed (not redistributed)
3. `test_slash_below_min_delegation_auto_jails` — slash causes self_stake < MIN_SELF_DELEGATION → auto-jail
4. `test_jail_validator` — jail sets status, jailed_until, excluded from epoch set
5. `test_tombstone_validator` — tombstone is permanent, cannot unjail
6. `test_unjail_validator_success` — unjail after cooldown with sufficient stake
7. `test_unjail_validator_too_early` — unjail before cooldown fails
8. `test_unjail_validator_insufficient_stake` — unjail with stake below minimum fails
9. `test_unjail_tombstoned_fails` — cannot unjail tombstoned validator
10. `test_delegator_can_undelegate_from_jailed` — delegators aren't trapped
11. `test_jailed_validator_earns_no_rewards` — reward distribution skips jailed

**`torus-consensus/src/slashing.rs` tests:**
12. `test_double_sign_detection` — two votes same (view, phase), different block → evidence produced
13. `test_no_false_positive` — same vote replayed → no evidence (same block hash)
14. `test_different_view_no_detection` — different views → no evidence
15. `test_evidence_window_expiry` — old votes outside window are pruned
16. `test_evidence_cryptographic_validity` — evidence signatures verify against pubkey

**`torus-bridge/` or integration tests:**
17. `test_jail_vote_tally` — votes accumulate, threshold triggers jail
18. `test_jail_vote_expiry` — old votes don't count toward threshold
19. `test_unjail_self_action` — full flow: jail → wait → unjail → candidate status
20. `test_double_sign_to_tombstone` — evidence → slash → tombstone (full pipeline)

### Integration test — add to `torus-integration-tests/`

Create `tests/slashing_jailing.rs`:
- Full lifecycle: register validators → one double-signs → detected → slashed → tombstoned → delegators undelegate
- Full lifecycle: validator goes offline → downtime detected → slashed → jailed → waits → unjails → re-enters active set
- Edge case: validator jailed during epoch transition → set shrinks correctly

---

## Architecture Constraints

1. **No new crates** — add modules to existing crates
2. **Borsh serialization** — all new persisted types must derive `BorshSerialize, BorshDeserialize`
3. **Error handling** — add new variants to `EconomicsError` as needed, use `thiserror`
4. **No floating point** — use basis points (u16) for slash fractions, integer math for all calculations
5. **Deterministic** — all slash/jail logic must be deterministic across nodes (same input → same state change)
6. **Don't modify hotstuff_rs** — the double-sign detector wraps around it, doesn't modify it
7. **Column families** — add new CFs to `torus-state/src/cf.rs` following existing patterns
8. **Existing patterns** — follow the patterns in `StakingManager` and `GovernanceManager` for state access
9. **Run `cargo check --workspace`** after implementation to verify compilation
10. **Guard delegations to jailed validators** — `delegate()` should reject delegating to `Jailed` validators (currently only blocks `Tombstoned`)

---

## File Manifest (expected changes)

| File | Action | What |
|---|---|---|
| `torus-types/src/lib.rs` | Edit | Add slashing types (DoubleSignEvidence, SlashRecord, InfractionType) |
| `torus-economics/src/staking.rs` | Edit | Add slash_validator, jail_validator, tombstone_validator, unjail_validator |
| `torus-economics/src/types.rs` | Edit | Add constants, any new error variants |
| `torus-economics/src/epoch.rs` | Edit | Integrate downtime check at epoch boundary |
| `torus-consensus/src/slashing.rs` | Create | DoubleSignDetector, DowntimeTracker |
| `torus-consensus/src/mod.rs` | Edit | Add `pub mod slashing;` |
| `torus-consensus/src/app.rs` | Edit | Wire detector into block validation pipeline |
| `torus-bridge/src/native_executor.rs` | Edit | Replace JailVote + UnjailSelf stubs |
| `torus-state/src/cf.rs` | Edit | Add cf_slashing_records, cf_jail_votes |
| `torus-integration-tests/tests/slashing_jailing.rs` | Create | Integration tests |

---

## Verification Checklist

- [ ] `cargo check --workspace` passes
- [ ] All 20 unit tests pass
- [ ] Integration tests pass
- [ ] Double-sign produces cryptographically verifiable evidence
- [ ] Slashing uses integer math only (no floats)
- [ ] Tombstoned validators can never unjail
- [ ] Jailed validators excluded from epoch validator set
- [ ] Delegators can undelegate from jailed/tombstoned validators
- [ ] Slash records persisted and queryable
- [ ] No rewards distributed to jailed validators
