# Phase 3 — Batch KL: 13 Remaining Medium-Tier Audit Fixes

## Context

Torus-hyperBFT is an EVM-compatible L1 blockchain built in Rust. Three security audits (consensus 3.4.2, EVM 3.4.3, economic 3.4.4) produced 103 findings across all severities. All critical and high fixes are committed. Of 33 medium-tier findings, 20 are done. **This batch resolves the remaining 13** — 8 not started, 5 partially implemented.

**Crate layout** (all under `crates/`):
- `torus-types` — shared types, `NativeAction` enum, constants
- `torus-economics` — staking, governance, fee distribution, epoch management
- `torus-core` — order book, margin, liquidation, oracle, precompiles (CoreWriter)
- `torus-evm` — EVM executor, gas calculation
- `torus-bridge` — native action executor, proposer, validator, state root
- `torus-state` — RocksDB state storage, snapshots
- `torus-mempool` — transaction pool, native pool
- `torus-rpc` — JSON-RPC server
- `torus-consensus` — hotstuff_rs integration, `TorusApp`
- `hotstuff_rs` — HotStuff BFT consensus library (MonadBFT variant)
- `torus-integration-tests` — E2E test crate

**Reference doc**: `prompts/phase3-3.4.5-fix-audit-findings.md` — full audit finding list

---

## GROUP A: Consensus Fixes (3 findings)

### FIX 1 | CONS-PF-15 | Bound `seen_proposals` HashMap

**File**: `crates/hotstuff_rs/src/hotstuff/implementation.rs`

**Current code** — unbounded HashMap, never pruned:
```rust
// Line 88: field declaration
seen_proposals: std::collections::HashMap<(ViewNumber, VerifyingKey), CryptoHash>,

// Line 117: initialization
seen_proposals: std::collections::HashMap::new(),

// Lines 558-559: insertion (called on every new proposal)
self.seen_proposals.insert(proposal_key, proposal.block.hash);
```

`enter_view()` (lines 144-189) resets `proposal_status`, `recovery_state`, and `phase_vote_collectors` — but **never touches `seen_proposals`**. The map grows monotonically for the process lifetime.

**Fix**: Add eviction in `enter_view()`. After resetting other state, retain only entries where the view is within a reasonable window:
```rust
let cutoff = new_view_info.view.saturating_sub(100);
self.seen_proposals.retain(|&(view, _), _| view >= cutoff);
```
The window of 100 views is generous — equivocation detection only matters for the current view. Adjust if the equivocation logic references older views.

---

### FIX 2 | CONS-PF-16 | Bound `bracha_timeout` BTreeMaps

**File**: `crates/hotstuff_rs/src/pacemaker/implementation.rs`

**Current code** — two BTreeMaps, never pruned:
```rust
// Lines 581-583: field declarations
bracha_timeout_power: BTreeMap<ViewNumber, u64>,
bracha_timeout_voters: BTreeMap<ViewNumber, BTreeSet<[u8; 32]>>,

// Lines 625-626: initialization
bracha_timeout_power: BTreeMap::new(),
bracha_timeout_voters: BTreeMap::new(),
```

`update_view()` (lines 481-521) replaces `timeout_vote_collectors` but **never touches bracha maps**. There's already a `split_off` pattern in `update_timeouts()` (line 634) for `self.timeouts`:
```rust
self.timeouts = self.timeouts.split_off(&epoch_start_view);
```

**Fix**: Add the same `split_off` pattern for bracha maps in `update_view()`, right after the timeout_vote_collectors replacement (after line 518):
```rust
// Prune bracha state for views we'll never revisit.
let cutoff = next_view.saturating_sub(100);
self.state.bracha_timeout_power = self.state.bracha_timeout_power.split_off(&cutoff);
self.state.bracha_timeout_voters = self.state.bracha_timeout_voters.split_off(&cutoff);
```
`split_off` on BTreeMap keeps everything >= the key, which is exactly what we want.

---

### FIX 3 | CONS-FIND-14 | Cross-Collector Vote Deduplication

**File**: `crates/hotstuff_rs/src/types/signed_messages.rs`

**Current code** — CVS and PVS collectors count independently:
```rust
// Lines 223-230: struct
pub(crate) struct ActiveCollectorPair<CL: Collector> {
    cvs_collector: CL,
    pvs_collector: Option<CL>,
}

// Lines 258-271: collect() — tries CVS then PVS
pub(crate) fn collect(
    &mut self,
    signer: &VerifyingKey,
    message: CL::Vote,
) -> Option<CL::Certificate> {
    if let Some(certificate) = self.cvs_collector.collect(signer, message.clone()) {
        return Some(certificate);
    } else if let Some(ref mut collector) = self.pvs_collector {
        if let Some(certificate) = collector.collect(signer, message) {
            return Some(certificate);
        }
    }
    None
}
```

The clone amplification bug was already fixed (commit `15e7542`), but a validator present in both CVS and PVS can still have their vote power counted once in each collector independently. This matters during validator set transitions.

**Fix**: Add a `seen_signers: HashSet<VerifyingKey>` (or `BTreeSet`) field to `ActiveCollectorPair`. Before forwarding to either collector, check if the signer was already counted:
```rust
pub(crate) fn collect(
    &mut self,
    signer: &VerifyingKey,
    message: CL::Vote,
) -> Option<CL::Certificate> {
    if !self.seen_signers.insert(*signer) {
        return None; // Already counted in one of the collectors
    }
    // ... existing CVS-then-PVS logic unchanged ...
}
```
Update `new()` to initialize `seen_signers: HashSet::new()` (or `BTreeSet::new()` for determinism).

**Caution**: Verify this doesn't break the intended semantics. If CVS and PVS are supposed to independently reach quorum (e.g., during a validator set transition where BOTH sets must certify), then a cross-collector dedup would be wrong. Read the MonadBFT paper references in the codebase and the `Collector` trait contract (lines 173-182) before implementing. If independent quorum is intended, mark this finding as "by design" with a code comment explaining why.

---

## GROUP B: EVM Fixes (5 findings)

### FIX 4 | EVM-PF-02 | Safe Gas Price Truncation

**File**: `crates/torus-evm/src/executor.rs`

**Current code** — bare `as u64` casts:
```rust
// Lines 294-302
fn calc_effective_gas_price(base_fee: u64, max_fee: u128, priority_fee: Option<u128>) -> u64 {
    match priority_fee {
        Some(pf) => {
            let max_priority = pf.min(max_fee.saturating_sub(base_fee as u128));
            (base_fee as u128 + max_priority) as u64   // <-- truncates silently
        }
        None => max_fee as u64,                        // <-- truncates silently
    }
}
```

**Fix**: Cap at `u64::MAX` instead of silent truncation:
```rust
fn calc_effective_gas_price(base_fee: u64, max_fee: u128, priority_fee: Option<u128>) -> u64 {
    let result = match priority_fee {
        Some(pf) => {
            let max_priority = pf.min(max_fee.saturating_sub(base_fee as u128));
            base_fee as u128 + max_priority
        }
        None => max_fee,
    };
    u64::try_from(result).unwrap_or(u64::MAX)
}
```

---

### FIX 5 | EVM-PF-16 | Replace O(n) `next_sequence` With Counter

**File**: `crates/torus-core/src/precompiles.rs`

**Current code** — full prefix scan on every enqueue (lines 945-973):
```rust
fn next_sequence(state_db: &StateDb, target_block: u64) -> Result<u64, CoreError> {
    let db = state_db.inner();
    let cf = db.cf_handle(CF_CORE_WRITER_QUEUE)
        .ok_or(CoreError::MissingCf(CF_CORE_WRITER_QUEUE))?;

    let prefix = target_block.to_be_bytes();
    let iter = db.prefix_iterator_cf(cf, &prefix);

    let mut max_seq: u64 = 0;
    let mut found = false;
    for item in iter {                              // <-- O(n) scan
        let (key, _) = item.map_err(|e| CoreError::State(torus_state::StateError::RocksDb(e)))?;
        if !key.starts_with(&prefix) { break; }
        if key.len() == 16 {
            let seq = u64::from_be_bytes(key[8..16].try_into().unwrap());
            if seq >= max_seq { max_seq = seq; found = true; }
        }
    }
    Ok(if found { max_seq + 1 } else { 0 })
}
```

**Fix**: Use a reverse iterator and seek to the last key with the block prefix instead of scanning all keys. RocksDB supports this natively:
```rust
fn next_sequence(state_db: &StateDb, target_block: u64) -> Result<u64, CoreError> {
    let db = state_db.inner();
    let cf = db.cf_handle(CF_CORE_WRITER_QUEUE)
        .ok_or(CoreError::MissingCf(CF_CORE_WRITER_QUEUE))?;

    let prefix = target_block.to_be_bytes();
    // Seek to end of prefix range: prefix bytes + 0xFF... 
    let mut upper = prefix.to_vec();
    upper.push(0xFF); upper.extend_from_slice(&[0xFF; 7]);
    
    let mut opts = rocksdb::ReadOptions::default();
    opts.set_iterate_upper_bound(upper);
    opts.set_prefix_same_as_start(true);
    
    let mut iter = db.raw_iterator_cf_opt(cf, opts);
    iter.seek_for_prev(&[prefix.as_slice(), &[0xFF; 8]].concat());
    
    if iter.valid() {
        if let Some(key) = iter.key() {
            if key.starts_with(&prefix) && key.len() == 16 {
                let seq = u64::from_be_bytes(key[8..16].try_into().unwrap());
                return Ok(seq + 1);
            }
        }
    }
    Ok(0)
}
```
Alternatively, a dedicated counter CF key works too — pick whichever is cleaner given the existing RocksDB patterns. The key point: **no iteration over all entries**.

---

### FIX 6 | EVM-FIND-12 | Propagate CoreWriter Drain Errors

**File**: `crates/torus-bridge/src/native_executor.rs`

**Current code** — error silently swallowed (lines 899-927):
```rust
pub fn drain_core_writer(ctx: &mut NativeExecContext) -> Vec<NativeActionResult> {
    let queued = match CoreWriterQueue::drain(&ctx.state_db, ctx.block_height) {
        Ok(actions) => actions,
        Err(_) => return vec![],       // <-- silently swallows error
    };
    // ... process queued actions ...
}
```

**Call sites** (both discard the return value):
- `crates/torus-bridge/src/proposer.rs:210` — `NativeExecutor::drain_core_writer(&mut ctx);`
- `crates/torus-bridge/src/validator.rs:261` — `NativeExecutor::drain_core_writer(&mut ctx);`

**Fix**: Change return type to `Result<Vec<NativeActionResult>, CoreError>` and propagate:
```rust
pub fn drain_core_writer(ctx: &mut NativeExecContext) -> Result<Vec<NativeActionResult>, CoreError> {
    let queued = CoreWriterQueue::drain(&ctx.state_db, ctx.block_height)?;
    // ... same processing ...
    Ok(results)
}
```
Update both call sites in `proposer.rs` and `validator.rs` to handle the `Result`. A drain failure during block production/validation is a serious error — it should abort the block, not silently produce a block with missing actions.

---

### FIX 7 | EVM-FIND-16 | Remove `.expect()` in Receipts Root

**File**: `crates/torus-bridge/src/proposer.rs`

**Current code** — panics on serialization failure (lines 277-284):
```rust
pub(crate) fn compute_receipts_root(receipts: &[torus_types::Receipt]) -> B256 {
    if receipts.is_empty() {
        return B256::ZERO;
    }
    let data = serde_json::to_vec(receipts).expect("serialize receipts");  // <-- panics
    alloy_primitives::keccak256(&data)
}
```

**Call sites**:
- `proposer.rs:86` — `let receipts_root = compute_receipts_root(&exec_result.receipts);`
- `proposer.rs:236` — `let receipts_root = compute_receipts_root(&exec_result.receipts);`

**Fix**: Return `Result<B256, ...>` and propagate:
```rust
pub(crate) fn compute_receipts_root(receipts: &[torus_types::Receipt]) -> Result<B256, serde_json::Error> {
    if receipts.is_empty() {
        return Ok(B256::ZERO);
    }
    let data = serde_json::to_vec(receipts)?;
    Ok(alloy_primitives::keccak256(&data))
}
```
Update both call sites to use `?` or `.map_err(...)`. The validator path (`validator.rs`) also has its own `compute_receipts_root` — check and fix it too if it has the same pattern.

---

## GROUP C: Economic Fixes (5 findings)

### FIX 8 | ECON-PF-16 | Canonical Block Time Constant

**File**: `crates/torus-economics/src/types.rs` + `crates/torus-economics/src/governance.rs`

**Current code** — inconsistent time assumptions:
```rust
// types.rs:313-314 — assumes 1 block/sec
pub const BLOCKS_PER_YEAR: u64 = 31_536_000;

// types.rs:353-354 — assumes 6s blocks
pub const JAIL_DURATION_BLOCKS: u64 = 28_800;     // ~2 days at 6s

// types.rs:356-357 — assumes 6s blocks
pub const JAIL_VOTE_EXPIRY_BLOCKS: u64 = 14_400;  // ~1 day at 6s

// types.rs:568-569 — assumes 6s blocks
pub const COMMISSION_COOLDOWN_BLOCKS: u64 = 28_800; // ~2 days at 6s

// governance.rs:30-31 — assumes 6s blocks
pub const DEFAULT_VOTING_PERIOD_BLOCKS: u64 = 100_800; // ~7 days at 6s

// types.rs:305 — assumes 1s blocks
pub const UNBONDING_PERIOD: u64 = 604_800;        // ~7 days at 1s

// types.rs:572 — assumes 1s blocks
pub const WHITELIST_EXPIRY_BLOCKS: u64 = 604_800;  // ~7 days at 1s
```

Two groups: some assume 1-second blocks, others assume 6-second blocks. This is incorrect — the node must have ONE block time assumption.

**Fix**: Define a canonical `TARGET_BLOCK_TIME_SECS` and derive everything from it:
```rust
/// Canonical target block time in seconds. All block-count constants derive from this.
pub const TARGET_BLOCK_TIME_SECS: u64 = 6;

/// Seconds per year / block time.
pub const BLOCKS_PER_YEAR: u64 = 365 * 24 * 3600 / TARGET_BLOCK_TIME_SECS; // 5,256,000

/// ~2 days of blocks.
pub const JAIL_DURATION_BLOCKS: u64 = 2 * 24 * 3600 / TARGET_BLOCK_TIME_SECS;

/// ~1 day of blocks.
pub const JAIL_VOTE_EXPIRY_BLOCKS: u64 = 24 * 3600 / TARGET_BLOCK_TIME_SECS;

/// ~2 days of blocks.
pub const COMMISSION_COOLDOWN_BLOCKS: u64 = 2 * 24 * 3600 / TARGET_BLOCK_TIME_SECS;

/// ~7 days of blocks.
pub const UNBONDING_PERIOD: u64 = 7 * 24 * 3600 / TARGET_BLOCK_TIME_SECS;

/// ~7 days of blocks.
pub const WHITELIST_EXPIRY_BLOCKS: u64 = 7 * 24 * 3600 / TARGET_BLOCK_TIME_SECS;
```
Also update `DEFAULT_VOTING_PERIOD_BLOCKS` in `governance.rs` to use the same derivation. The actual value of `TARGET_BLOCK_TIME_SECS` (1 vs 6) is a design decision — check the genesis config or chain config for the intended block time. If configurable at runtime, make the constant the default and allow override.

**Important**: Changing `BLOCKS_PER_YEAR` from 31.5M to 5.25M will change staking reward rates by 6x. Verify this is intended or add a compensating multiplier to the reward formula.

---

### FIX 9 | ECON-PF-17 | Wire `to` Address in Withdraw

**File**: `crates/torus-bridge/src/native_executor.rs` + `crates/torus-core/src/lockbox.rs`

**Current code** — `to` address discarded:
```rust
// native_executor.rs lines 258-259: dispatch
NativeAction::Withdraw { amount, .. } => {     // <-- `to` silently dropped
    Self::exec_withdraw_from_native(ctx, sender, *amount)
}

// native_executor.rs lines 874-878: function signature
fn exec_withdraw_from_native(
    ctx: &mut NativeExecContext,
    sender: &Address,              // <-- no `to` parameter
    amount: U256,
) -> NativeActionResult {
```

**Fix**:
1. Change dispatch to capture `to`:
```rust
NativeAction::Withdraw { amount, to } => {
    Self::exec_withdraw_from_native(ctx, sender, *to, *amount)
}
```
2. Add `to` parameter to `exec_withdraw_from_native`:
```rust
fn exec_withdraw_from_native(
    ctx: &mut NativeExecContext,
    sender: &Address,
    to: Address,
    amount: U256,
) -> NativeActionResult {
```
3. Inside the function, debit from `sender` but credit to `to`. Update the lockbox call accordingly.
4. Also check `TransferToSpot` (line 255-256) — it routes to the same function but has no `to` field, so it should always credit `sender`. Consider splitting into two functions or adding a `to` parameter that defaults to sender for TransferToSpot.

---

### FIX 10 | ECON-FIND-14 | Slashing Dust Rounding

**File**: `crates/torus-economics/src/staking.rs`

**Current code** — zero-slash delegations skipped (lines 343-353):
```rust
for del in &delegations {
    let del_slash = del.amount * fraction / bps_10000;
    if !del_slash.is_zero() {          // <-- skips small delegations
        let mut updated_del = del.clone();
        updated_del.amount -= del_slash;
        let del_key = delegation_key(&del.delegator, &validator_addr);
        self.put_delegation_raw(&del_key, &updated_del)?;
        total_del_slashed += del_slash;
        total_slashed += del_slash;
    }
}
```

The `total_delegated` recompute (ECON-FIND-27, line 355) was already added, but delegations that round to zero slash are excluded from `total_slashed`. This means the sum of individual slashes is less than the expected `total * fraction`.

**Fix**: Remove the `if !del_slash.is_zero()` guard and always include the delegation in the loop body. If `del_slash` is zero, the delegation amount stays the same — `amount -= 0` is a no-op and the `put_delegation_raw` just rewrites the same value. The important thing is that `total_slashed` reflects the true accounting:

```rust
for del in &delegations {
    let del_slash = del.amount * fraction / bps_10000;
    let mut updated_del = del.clone();
    updated_del.amount -= del_slash;
    let del_key = delegation_key(&del.delegator, &validator_addr);
    self.put_delegation_raw(&del_key, &updated_del)?;
    total_del_slashed += del_slash;
    total_slashed += del_slash;
}
```

Alternatively, if avoiding unnecessary writes matters: keep the guard for the DB write but still accumulate `total_slashed` unconditionally (even though it's zero, it keeps the accounting clean).

---

### FIX 11 | ECON-FIND-15 | Wire `TopUpSelfStake` to NativeAction

**File**: `crates/torus-types/src/lib.rs`, `crates/torus-bridge/src/native_executor.rs`

The function already exists and is tested in `staking.rs:631-644`:
```rust
pub fn top_up_self_stake(&self, validator_addr: Address, amount: U256) -> Result<()> {
    if amount.is_zero() { return Ok(()); }
    let mut val = self.get_validator(&validator_addr)?
        .ok_or(EconomicsError::ValidatorNotFound(validator_addr))?;
    self.debit_balance(&validator_addr, amount)?;
    val.self_stake += amount;
    self.put_validator(&validator_addr, &val)?;
    tracing::info!(%validator_addr, %amount, "self-stake topped up");
    Ok(())
}
```

But there's no `NativeAction` variant to invoke it. The current `NativeAction` enum (in `torus-types/src/lib.rs:291-327`) has staking variants at lines 304-307:
```rust
// === Staking ===
Delegate { validator: Address, amount: U256 },
Undelegate { validator: Address, amount: U256 },
PermanentStake { amount: U256 },
ClaimRewards,
```

**Fix**:
1. Add variant to `NativeAction` enum after `ClaimRewards`:
```rust
TopUpSelfStake { amount: U256 },
```
2. Add `canonical_bytes()` encoding — next available tag is 22 (after `DelistMarket` at tag 21 on line 569). Follow the exact pattern of existing variants.
3. Add EIP-712 type hash in `eip712.rs` following the pattern of other variants.
4. Add dispatch in `native_executor.rs`:
```rust
NativeAction::TopUpSelfStake { amount } => {
    match ctx.staking.top_up_self_stake(*sender, *amount) {
        Ok(()) => NativeActionResult::ok("top_up_self_stake", "self-stake topped up"),
        Err(e) => NativeActionResult::err("top_up_self_stake", e.to_string()),
    }
}
```
5. Add mempool validation if needed (check `torus-mempool/src/validate.rs`).

---

### FIX 12 | ECON-FIND-16 | Snapshot Voting Power (Partial → Complete)

**File**: `crates/torus-economics/src/governance.rs`

**Current code** — snapshot_block recorded but ignored (lines 1126-1136):
```rust
fn compute_vote_weight_at(
    &self,
    voter: &Address,
    params: &GovernanceParams,
    _snapshot_block: u64,          // <-- intentionally unused
) -> Result<U256> {
    // TODO: Use historical state at snapshot_block when available.
    // For now, live state is used. The unbonding period (longer than voting period)
    // provides defense against flash-vote attacks.
    self.compute_vote_weight(voter, params)
}
```

The `snapshot_block` is set correctly at proposal creation (line 653: `snapshot_block: current_block`), and passed through from the voting path (line 698).

**Fix**: The simplest approach that doesn't require a full historical state DB:

**Option A** — Store voter stakes in the proposal at creation time:
- Add `snapshot_stakes: BTreeMap<Address, U256>` to the `Proposal` struct
- At proposal creation, snapshot all active staker balances into this map
- In `compute_vote_weight_at`, look up `snapshot_stakes[voter]` instead of live state
- Pro: simple, self-contained. Con: large proposals if many stakers

**Option B** — Use the existing RocksDB state at a height (if height-keyed state exists):
- Check if `StateDb` supports historical reads at a given block height
- If so, construct a staking view at `snapshot_block` and query it
- This is cleaner but depends on state pruning settings

Pick whichever fits the existing architecture. If neither is practical, add a code comment explaining that the unbonding period (longer than voting period) is the primary defense, and mark the TODO as a known limitation. Do NOT leave the underscore-prefixed `_snapshot_block` parameter — either use it or explicitly document why it's deferred.

---

## GROUP D: Acceptable Deviations (1 finding — document only)

### FIX 13 | EVM-FIND-11 | Native Pool Dedup Encoding

**File**: `crates/torus-mempool/src/native_pool.rs:164-171`

The audit requested EIP-712 struct hash for native pool dedup. The implementation uses `canonical_bytes()` + keccak256 — a custom deterministic binary encoding that replaced the original Debug format. This is collision-resistant and compiler-stable.

**Action**: Add a code comment at the dedup site explaining the deviation:
```rust
// AUDIT: EVM-FIND-11 requested EIP-712 struct hash for dedup.
// canonical_bytes() provides equivalent collision resistance with simpler implementation.
// EIP-712 would add complexity without material security benefit for internal pool dedup.
```
No code change needed.

---

## Testing Requirements

### Unit tests — add or update per fix

**hotstuff_rs tests:**
1. `test_seen_proposals_eviction` — insert proposals across 200 views, verify entries older than cutoff are pruned after `enter_view()`
2. `test_bracha_timeout_eviction` — insert timeout state across views, verify pruned after `update_view()`
3. `test_cross_collector_dedup` — validator in both CVS and PVS, verify vote counted only once

**torus-evm tests:**
4. `test_gas_price_no_truncation` — `max_fee > u64::MAX` returns `u64::MAX`, not garbage

**torus-core tests:**
5. `test_next_sequence_efficiency` — enqueue 100 actions, verify next_sequence doesn't scan all 100 (mock iterator or check call count)

**torus-bridge tests:**
6. `test_drain_core_writer_propagates_error` — inject DB error, verify Result::Err propagated (not empty vec)
7. `test_receipts_root_no_panic` — if possible, verify no panic on edge-case input
8. `test_withdraw_to_different_address` — Withdraw with `to != sender`, verify funds arrive at `to`

**torus-economics tests:**
9. `test_slash_dust_included` — slash a tiny delegation, verify `total_slashed` includes it
10. `test_top_up_self_stake_via_native_action` — submit TopUpSelfStake as NativeAction, verify stake increases
11. `test_blocks_per_year_consistent` — verify `BLOCKS_PER_YEAR == 365 * 24 * 3600 / TARGET_BLOCK_TIME_SECS`
12. `test_vote_weight_at_snapshot` — create proposal, change stake, vote — weight should reflect snapshot, not live state

---

## Architecture Constraints

1. **No new crates** — add to existing crates only
2. **Deterministic** — all changes must produce identical results across nodes
3. **No floating point** — integer math only, basis points for fractions
4. **Borsh serialization** — any new persisted types must derive `BorshSerialize, BorshDeserialize`
5. **Don't break existing tests** — run the full test suite after all changes
6. **Don't modify unrelated code** — touch only what's needed for these 13 findings
7. **Read before editing** — line numbers in this prompt may have shifted. Always read the file first.
8. **Error types** — add new variants to existing error enums (`CoreError`, `EconomicsError`) rather than creating new error types
9. **Follow existing patterns** — look at how similar fixes were done in Batch IJ (committed as `35f4d2f`)

---

## File Manifest (expected changes)

| File | Action | Fixes |
|---|---|---|
| `hotstuff_rs/src/hotstuff/implementation.rs` | Edit | FIX 1 (seen_proposals eviction) |
| `hotstuff_rs/src/pacemaker/implementation.rs` | Edit | FIX 2 (bracha map pruning) |
| `hotstuff_rs/src/types/signed_messages.rs` | Edit | FIX 3 (cross-collector dedup) |
| `torus-evm/src/executor.rs` | Edit | FIX 4 (gas price truncation) |
| `torus-core/src/precompiles.rs` | Edit | FIX 5 (next_sequence O(1)) |
| `torus-bridge/src/native_executor.rs` | Edit | FIX 6, FIX 9, FIX 11 (drain errors, withdraw `to`, TopUpSelfStake dispatch) |
| `torus-bridge/src/proposer.rs` | Edit | FIX 6 call site, FIX 7 (receipts root) |
| `torus-bridge/src/validator.rs` | Edit | FIX 6 call site, FIX 7 (receipts root if applicable) |
| `torus-economics/src/types.rs` | Edit | FIX 8 (block time constants) |
| `torus-economics/src/governance.rs` | Edit | FIX 8 (voting period), FIX 12 (snapshot voting) |
| `torus-economics/src/staking.rs` | Edit | FIX 10 (slash dust) |
| `torus-types/src/lib.rs` | Edit | FIX 11 (TopUpSelfStake variant + canonical_bytes) |
| `torus-types/src/eip712.rs` | Edit | FIX 11 (TopUpSelfStake type hash) |
| `torus-mempool/src/native_pool.rs` | Edit | FIX 13 (comment only) |
| `torus-core/src/lockbox.rs` | Edit | FIX 9 (accept `to` address) |

---

## Verification Checklist

- [ ] `cargo check --workspace` passes with zero errors
- [ ] `cargo clippy --workspace` passes with zero new warnings
- [ ] `cargo test -p torus-core -p torus-economics -p torus-bridge -p torus-evm -p torus-mempool -p torus-rpc` — all pass
- [ ] `cargo test -p hotstuff_rs` — all pass (known exception: `multiple_validator_set_updates_test` may livelock — pre-existing, ignore)
- [ ] `seen_proposals` does not grow unbounded (FIX 1)
- [ ] `bracha_timeout_power/voters` pruned on view advance (FIX 2)
- [ ] Gas price capped at `u64::MAX` (FIX 4)
- [ ] `next_sequence` does not iterate all keys (FIX 5)
- [ ] `drain_core_writer` error propagated to proposer/validator (FIX 6)
- [ ] `compute_receipts_root` does not panic (FIX 7)
- [ ] All time constants derived from `TARGET_BLOCK_TIME_SECS` (FIX 8)
- [ ] Withdraw sends funds to `to` address (FIX 9)
- [ ] `total_slashed` includes dust delegations (FIX 10)
- [ ] `TopUpSelfStake` callable as NativeAction (FIX 11)
- [ ] Voting power uses snapshot, not live state (FIX 12)

## Commit Strategy

Single commit: `fix(medium): resolve 13 remaining medium-tier audit findings (Batch KL)`
