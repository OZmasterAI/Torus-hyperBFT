# Phase 3 — Batch MN: 20 Remaining Low/Informational Audit Fixes

## Context

Torus-hyperBFT is an EVM-compatible L1 blockchain built in Rust. Three security audits (consensus 3.4.2, EVM 3.4.3, economic 3.4.4) produced 103 findings across all severities. All critical, high, and medium fixes are committed. Of 28 low/informational findings, 8 were fixed in earlier batches. **This batch resolves the remaining 20**.

**Crate layout** (all under `crates/`):
- `torus-types` — shared types, `NativeAction` enum, `Side`/`OrderType`/`TimeInForce` enums
- `torus-economics` — staking, governance, fee distribution, epoch management
- `torus-core` — order book, margin, liquidation, oracle, precompiles (CoreWriter), lockbox, position
- `torus-evm` — EVM executor, gas calculation
- `torus-bridge` — native action executor, proposer, validator, state root, committer, decode
- `torus-state` — RocksDB state storage, column family constants
- `torus-mempool` — transaction pool, native pool
- `torus-rpc` — JSON-RPC server
- `torus-consensus` — hotstuff_rs integration, `TorusApp`
- `torus-network` — libp2p swarm, peer scoring
- `hotstuff_rs` — HotStuff BFT consensus library (MonadBFT variant)

---

## GROUP A: Consensus Fixes (8 findings)

### FIX 1 | CONS-FIND-25 | Operator Precedence Bug in AdvanceView

**File**: `crates/hotstuff_rs/src/pacemaker/implementation.rs`

**Current code** — missing parentheses at lines 342-350:
```rust
// 3.2. If we are a validator and we haven't broadcasted an AdvanceView message in the current view,
//      broadcast an AdvanceView message containing the newly collected TimeoutCertificate.
if is_validator(&self.config.keypair.public(), &validator_set_state)
    && self.state.last_advance_view.is_none()
    || self
        .state
        .last_advance_view
        .is_some_and(|v| v < self.view_info.view)
{
```

Due to `&&` binding tighter than `||`, this parses as:
`(is_validator(...) && last_advance_view.is_none()) || last_advance_view.is_some_and(|v| ...)`

The `is_validator` check is **bypassed** when `is_some_and` is true — a non-validator can broadcast AdvanceView.

Contrast with the correctly parenthesized version at lines 172-176 in the same file:
```rust
if block_tree.highest_pc()?.view >= cur_view
    && !block_tree.highest_pc()?.is_genesis_pc()
    && is_validator(&self.config.keypair.public(), &validator_set_state)
    && (self.state.last_advance_view.is_none()
        || self.state.last_advance_view.is_some_and(|v| v < cur_view))
```

**Fix**: Add parentheses around the `is_none() || is_some_and(...)` sub-expression:
```rust
if is_validator(&self.config.keypair.public(), &validator_set_state)
    && (self.state.last_advance_view.is_none()
        || self
            .state
            .last_advance_view
            .is_some_and(|v| v < self.view_info.view))
{
```

---

### FIX 2 | CONS-FIND-26 | Validate TipInfo in TimeoutVote

**File**: `crates/hotstuff_rs/src/pacemaker/implementation.rs`

**Current code** — `local_tip` consumed without validation in `on_receive_timeout_vote`. The `highest_tc` field is validated with `tc.is_correct(block_tree)?` (line 302), but `local_tip` and `highest_qc` are stored raw with no sanity checks.

In the `TimeoutVoteCollector::collect` method (`crates/hotstuff_rs/src/types/signed_messages.rs`, around lines 199-228), the `local_tip` is consumed directly:
```rust
let tip_hash = vote.local_tip.as_ref().map(|t| t.block_hash);
// ... stored without validation
if let Some(ref tip) = vote.local_tip {
    if self.high_tip.is_none() || tip.view > self.high_tip.as_ref().unwrap().view {
        self.high_tip = Some(tip.clone());
    }
}
```

**Fix**: Add validation before the collector stores the tip. In `on_receive_timeout_vote` (in `implementation.rs`), after the `is_correct` check (around line 251) and before the vote is forwarded to `collect()`, validate the `local_tip` fields:

```rust
// FIX CONS-FIND-26: Validate TipInfo fields before collecting.
if let Some(ref tip) = timeout_vote.local_tip {
    // Reject tips with implausible views (future views or very old).
    if tip.view > self.view_info.view {
        return Ok(());
    }
}
```

Only reject clearly invalid tips (view in the future). The `block_hash` and `block_justify` cannot be fully validated without DB lookups, which would be too expensive here. A view bounds check is the pragmatic minimum.

---

### FIX 3 | CONS-FIND-27 | Update `local_tip` for Nudge Votes

**File**: `crates/hotstuff_rs/src/hotstuff/implementation.rs`

**Current code** — `on_receive_nudge` (lines 754-853) processes a nudge but never calls `block_tree.set_local_tip()`.

Contrast with `on_receive_proposal` (lines 704-721) which does:
```rust
if !proposal.is_reproposal() && vote_phase == Phase::Generic {
    use crate::pacemaker::types::TipInfo;
    let tip = TipInfo {
        block_hash: proposal.block.hash,
        block_height: proposal.block.height,
        block_justify: proposal.block.justify.clone(),
        block_data_hash: proposal.block.data_hash,
        view: self.view_info.view,
    };
    block_tree.set_local_tip(&tip)?;
}
```

**Fix**: After the `block_tree.update(&nudge.justify, ...)` call (line 791) and before the voting section, update local_tip based on the nudge's justify. The nudge certifies a block (referenced in `nudge.justify.block`), so set the tip to that block:

```rust
// FIX CONS-FIND-27: Update local_tip for nudge-certified blocks.
// The nudge's justify certifies a block; update our tip to reflect it.
{
    use crate::pacemaker::types::TipInfo;
    if let Ok(Some(block)) = block_tree.block(&nudge.justify.block) {
        let tip = TipInfo {
            block_hash: block.hash,
            block_height: block.height,
            block_justify: nudge.justify.clone(),
            block_data_hash: block.data_hash,
            view: self.view_info.view,
        };
        block_tree.set_local_tip(&tip)?;
    }
}
```

Insert this between lines 796 and 798 (after `update_validator_sets` and before the vote collectors update). Check the block tree API — if `block()` returns the block by hash, use it. If the API differs (e.g., `get_block` or `block_by_hash`), adjust accordingly.

---

### FIX 4 | CONS-FIND-28 | Shuffle ProposalRequest Recipients

**File**: `crates/hotstuff_rs/src/hotstuff/implementation.rs`

**Current code** — deterministic first-k selection (lines 370-380):
```rust
let mut sent_count = 0;
for (vk, _power) in validator_set.validators_and_powers() {
    if sent_count >= kappa { break; }
    if vk != self.config.keypair.public() {
        self.sender_handle.send::<HotStuffMessage>(
            vk,
            req.clone().into(),
        );
        sent_count += 1;
    }
}
```

The same validators are always picked for ProposalRequest, enabling predictable withholding attacks.

**Fix**: Shuffle the validator list using the view number as a deterministic seed:

```rust
// FIX CONS-FIND-28: Shuffle recipients to prevent deterministic withholding.
let mut recipients: Vec<_> = validator_set.validators_and_powers()
    .map(|(vk, _)| vk)
    .filter(|vk| *vk != self.config.keypair.public())
    .collect();

// Deterministic shuffle seeded by view — all honest nodes pick the same set
// for a given view, but adversary can't predict recipients far in advance.
let seed = self.view_info.view.int();
let len = recipients.len();
if len > 1 {
    for i in (1..len).rev() {
        let j = (seed.wrapping_mul(6364136223846793005).wrapping_add(i as u64)
            % (i as u64 + 1)) as usize;
        recipients.swap(i, j);
    }
}

for vk in recipients.into_iter().take(kappa) {
    self.sender_handle.send::<HotStuffMessage>(
        vk,
        req.clone().into(),
    );
}
```

---

### FIX 5 | CONS-FIND-29 | Async Ban File I/O

**File**: `crates/torus-network/src/peer_scoring.rs`

**Current code** — `save_bans()` uses blocking `std::fs::write` (line 274), called from `penalize()` on every ban event, blocking the consensus event loop.

**Fix**: Use `tokio::task::spawn_blocking` (or `std::thread::spawn` if tokio is not available) to move the I/O off the event loop:

```rust
/// Save ban list to JSON file.
/// FIX CONS-FIND-29: Non-blocking — spawns write to avoid blocking event loop.
fn save_bans(&self) {
    let path = match &self.ban_file {
        Some(p) => p.clone(),
        None => return,
    };
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let entries: Vec<BanEntry> = self
        .perm_bans
        .iter()
        .map(|(peer, reason)| BanEntry {
            peer_id: peer.to_string(),
            permanent: true,
            banned_at: now_secs,
            expires_at: None,
            reason: reason.clone(),
        })
        .collect();

    tokio::task::spawn_blocking(move || {
        if let Ok(json) = serde_json::to_string_pretty(&entries) {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if let Err(e) = std::fs::write(&path, json) {
                tracing::warn!(%e, "failed to save ban list");
            }
        }
    });
}
```

**Note**: Check that `torus-network` has `tokio` as a dependency. If not, use `std::thread::spawn` instead.

---

### FIX 6 | CONS-FIND-30 | Add Ownership Check to `cancel_order`

**File**: `crates/torus-bridge/src/native_executor.rs`

**Current code** — `sender` not passed to cancel handler (line 215):
```rust
NativeAction::CancelOrder { order_id } => Self::exec_cancel_order(ctx, *order_id),
```

And in `exec_cancel_order` (lines 436-462), the cancelled order's trader is never compared to sender. Any user can cancel any other user's order.

**Fix**:

1. Update dispatch to pass `sender`:
```rust
NativeAction::CancelOrder { order_id } => Self::exec_cancel_order(ctx, sender, *order_id),
```

2. Update function signature and add ownership check:
```rust
fn exec_cancel_order(ctx: &mut NativeExecContext, sender: &Address, order_id: u128) -> NativeActionResult {
    for book in ctx.order_books.values_mut() {
        if let Ok(cancelled) = book.cancel_order(order_id) {
            // FIX CONS-FIND-30: Verify sender owns the order.
            if cancelled.trader != *sender {
                // Re-insert — caller is not the owner. Use place_order to restore it.
                let _ = book.place_order(cancelled);
                return NativeActionResult::err(
                    "cancel_order",
                    format!("order {order_id} not owned by sender"),
                );
            }
            // ... existing margin release logic unchanged ...
```

**Alternative** (cleaner but more invasive): Add a `cancel_order_if_owner(order_id, sender) -> Result<Order>` method to the order book that checks ownership before removal.

---

### FIX 7 | CONS-FIND-31 | Propagate `access_list` to TxEnv

**File**: `crates/torus-bridge/src/decode.rs`

**Current code** — `access_list` dropped via `..Default::default()` for EIP-2930 (line 74) and EIP-1559 (line 89).

**Fix**: Add the `access_list` field to both arms. The revm `TxEnv` has `access_list: AccessList` and alloy's `TxEip2930`/`TxEip1559` expose a compatible `access_list`:

```rust
// EIP-2930 arm:
TxEnvelope::Eip2930(signed) => {
    let tx = signed.tx();
    TxEnv {
        caller: sender,
        gas_limit: tx.gas_limit,
        gas_price: tx.gas_price,
        kind: tx.to,
        value: tx.value,
        data: tx.input.clone(),
        nonce: tx.nonce,
        chain_id: Some(tx.chain_id),
        access_list: tx.access_list.clone(),  // FIX CONS-FIND-31
        ..Default::default()
    }
}

// EIP-1559 arm:
TxEnvelope::Eip1559(signed) => {
    let tx = signed.tx();
    TxEnv {
        caller: sender,
        gas_limit: tx.gas_limit,
        gas_price: tx.max_fee_per_gas,
        gas_priority_fee: Some(tx.max_priority_fee_per_gas),
        kind: tx.to,
        value: tx.value,
        data: tx.input.clone(),
        nonce: tx.nonce,
        chain_id: Some(tx.chain_id),
        access_list: tx.access_list.clone(),  // FIX CONS-FIND-31
        ..Default::default()
    }
}
```

**Note**: Verify type compatibility. If alloy's `AccessList` and revm's `AccessList` are different types, you may need `.into()` or a conversion. Both crates use `alloy-eips` so `.clone()` should work.

---

### FIX 8 | CONS-FIND-32 | Overflow-Safe ViewNumber Arithmetic

**File**: `crates/hotstuff_rs/src/types/data_types.rs`

**Current code** — bare arithmetic that panics in debug, wraps in release (lines 408-428):
```rust
impl Add<u64> for ViewNumber {
    type Output = ViewNumber;
    fn add(self, rhs: u64) -> Self::Output {
        ViewNumber(self.0.add(rhs))
    }
}

impl Sub<u64> for ViewNumber {
    type Output = ViewNumber;
    fn sub(self, rhs: u64) -> Self::Output {
        ViewNumber(self.0.sub(rhs))
    }
}

impl Sub<ViewNumber> for ViewNumber {
    type Output = i64;
    fn sub(self, rhs: ViewNumber) -> Self::Output {
        (self.0 as i64).sub(rhs.0 as i64)
    }
}
```

**Fix**: Use saturating arithmetic:
```rust
impl Add<u64> for ViewNumber {
    type Output = ViewNumber;
    fn add(self, rhs: u64) -> Self::Output {
        // FIX CONS-FIND-32: Saturating to prevent overflow panic/wrap.
        ViewNumber(self.0.saturating_add(rhs))
    }
}

impl Sub<u64> for ViewNumber {
    type Output = ViewNumber;
    fn sub(self, rhs: u64) -> Self::Output {
        // FIX CONS-FIND-32: Saturating to prevent underflow panic/wrap.
        ViewNumber(self.0.saturating_sub(rhs))
    }
}

impl Sub<ViewNumber> for ViewNumber {
    type Output = i64;
    fn sub(self, rhs: ViewNumber) -> Self::Output {
        // FIX CONS-FIND-32: Safe cast via i128 intermediate to avoid truncation.
        let diff = self.0 as i128 - rhs.0 as i128;
        diff.clamp(i64::MIN as i128, i64::MAX as i128) as i64
    }
}
```

---

## GROUP B: EVM Fixes (2 findings)

### FIX 9 | EVM-FIND-21 | Document CF_LOGS/CF_LOGS_BLOOM as Deferred

**Files**: `crates/torus-state/src/cf.rs`, `crates/torus-bridge/src/committer.rs`

**Current state**: `CF_LOGS` and `CF_LOGS_BLOOM` are defined in `cf.rs` (lines 15-16) and registered in `ALL_CF_NAMES`, but the committer never writes to them.

**Fix**: Add documentation comments. No code change needed:

In `cf.rs`, update the constants at lines 15-16:
```rust
/// Reserved for future eth_getLogs indexing. Currently unpopulated.
/// AUDIT: EVM-FIND-21 — full log/bloom indexing deferred to post-launch optimisation.
/// Logs are currently served from receipts via sequential scan in the RPC layer.
pub const CF_LOGS: &str = "cf_logs";
pub const CF_LOGS_BLOOM: &str = "cf_logs_bloom";
```

In `committer.rs`, add a comment near the CF imports (around line 8-11):
```rust
// AUDIT: EVM-FIND-21 — CF_LOGS / CF_LOGS_BLOOM not populated in commit path.
// Log indexing deferred; logs served from receipts in RPC layer.
```

---

### FIX 10 | EVM-FIND-22 | Add Unit Tests for executor.rs

**File**: `crates/torus-evm/src/executor.rs`

**Current state**: No `#[cfg(test)]` module in `executor.rs`. The existing integration test file covers `EvmExecutor` as a black box but not internal functions.

**Fix**: Add a `#[cfg(test)]` module at the bottom of `executor.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    // AUDIT: EVM-FIND-22 — Unit tests for internal executor functions.

    #[test]
    fn calc_effective_gas_price_legacy() {
        // Legacy (no priority fee): gas price passed through.
        assert_eq!(calc_effective_gas_price(100, 500, None), 500);
    }

    #[test]
    fn calc_effective_gas_price_eip1559_normal() {
        // EIP-1559: base_fee + min(priority_fee, max_fee - base_fee)
        assert_eq!(calc_effective_gas_price(100, 500, Some(50)), 150);
    }

    #[test]
    fn calc_effective_gas_price_eip1559_capped() {
        // priority_fee > max_fee - base_fee → capped at max_fee
        assert_eq!(calc_effective_gas_price(100, 500, Some(1000)), 500);
    }

    #[test]
    fn calc_effective_gas_price_overflow_saturates() {
        // FIX EVM-PF-02 regression: values > u64::MAX → u64::MAX, not garbage.
        let huge: u128 = u64::MAX as u128 + 1000;
        assert_eq!(calc_effective_gas_price(0, huge, None), u64::MAX);
        assert_eq!(calc_effective_gas_price(100, huge, Some(huge)), u64::MAX);
    }

    #[test]
    fn calc_effective_gas_price_zero_base() {
        assert_eq!(calc_effective_gas_price(0, 100, Some(50)), 50);
        assert_eq!(calc_effective_gas_price(0, 0, Some(0)), 0);
    }
}
```

---

## GROUP C: Economic Fixes (10 findings)

### FIX 11 | ECON-FIND-20 | Oracle `get_last_valid_price` Staleness Check

**File**: `crates/torus-core/src/oracle.rs`

**Current code** — no staleness check (lines 344-354):
```rust
fn get_last_valid_price(&self, market_id: MarketId) -> Result<FixedPoint, CoreError> {
    let key = aggregated_price_key(market_id);
    match self.state_db.get_cf_raw(CF_NATIVE_ORACLE, &key)? {
        Some(data) => {
            let stored = StoredAggregatedPrice::try_from_slice(&data)
                .map_err(|e| CoreError::Borsh(e.to_string()))?;
            Ok(stored.price)
        }
        None => Err(CoreError::NoOraclePrice(market_id)),
    }
}
```

Contrast with `get_price` (lines 271-293) which checks `current_block.saturating_sub(stored.block_number) > self.config.max_oracle_age`.

**Fix**: Add `current_block` parameter and staleness check. Add a `StaleOraclePrice` variant to `CoreError` if it doesn't exist:

```rust
/// FIX ECON-FIND-20: Apply staleness check, same as get_price.
fn get_last_valid_price(&self, market_id: MarketId, current_block: u64) -> Result<FixedPoint, CoreError> {
    let key = aggregated_price_key(market_id);
    match self.state_db.get_cf_raw(CF_NATIVE_ORACLE, &key)? {
        Some(data) => {
            let stored = StoredAggregatedPrice::try_from_slice(&data)
                .map_err(|e| CoreError::Borsh(e.to_string()))?;
            if current_block.saturating_sub(stored.block_number) > self.config.max_oracle_age {
                return Err(CoreError::StaleOraclePrice(market_id));
            }
            Ok(stored.price)
        }
        None => Err(CoreError::NoOraclePrice(market_id)),
    }
}
```

Update all call sites to pass `current_block`. Check if `CoreError` already has a `StaleOraclePrice` variant — if not, add one following the pattern of `NoOraclePrice`.

---

### FIX 12 | ECON-FIND-21 | Whitelist Consume Failure Must Fail Registration

**File**: `crates/torus-bridge/src/native_executor.rs`

**Current code** — consume failure silently ignored (lines 697-699):
```rust
if let Err(e) = ctx.governance.consume_whitelist(sender) {
    tracing::warn!(%sender, %e, "failed to consume whitelist entry");
}
NativeActionResult::ok("register_validator", 5000)
```

**Fix**: Fail the registration action on whitelist error:
```rust
// FIX ECON-FIND-21: Whitelist consumption is mandatory — fail if it errors.
if let Err(e) = ctx.governance.consume_whitelist(sender) {
    tracing::error!(%sender, %e, "whitelist consumption failed after registration");
    return NativeActionResult::err(
        "register_validator",
        format!("registered but whitelist error: {e}"),
    );
}
NativeActionResult::ok("register_validator", 5000)
```

---

### FIX 13 | ECON-FIND-22 | Remove Dead FeeSplitter Call

**File**: `crates/torus-bridge/src/native_executor.rs`

**Current code** — `FeeSplitter::split_fees` result discarded (line 1052):
```rust
let _split = FeeSplitter::split_fees(total_fees, ctx.epoch);
```

`RewardDistributor::distribute_block_fees` computes its own `lerp_bps` ratios internally. The `split_fees` call is dead code that computes a result nobody uses.

**Fix**: Remove the dead line and add a comment:
```rust
// FIX ECON-FIND-22: Removed dead FeeSplitter::split_fees call.
// RewardDistributor::distribute_block_fees computes fee splits internally.
```

Also check if `FeeSplitter` is now unused. If its only caller was this line, consider whether to keep it (for future analytics) or remove it.

---

### FIX 14 | ECON-FIND-26 | Safe `u128` → `i128` Cast in Lockbox Precompile

**File**: `crates/torus-core/src/precompiles.rs`

**Current code** — unsafe cast at lines 846 and 851:
```rust
let amount = FixedPoint::from_raw(amount_raw as i128);       // wraps if > i128::MAX
```

There is already a safe function `u256_to_fp()` in `crates/torus-core/src/lockbox.rs:224`.

**Fix**: Validate the range before casting:
```rust
// FIX ECON-FIND-26: Validate range instead of wrapping cast.
if amount_raw > i128::MAX as u128 {
    return Err(CoreError::Overflow("lockbox amount exceeds i128::MAX".into()));
}
let amount = FixedPoint::from_raw(amount_raw as i128);
```

Apply this to BOTH the `depositToNative` and `withdrawFromNative` branches. Add an `Overflow(String)` variant to `CoreError` if it doesn't exist.

---

### FIX 15 | ECON-FIND-27 | Validate CoreWriter Enum Discriminants

**File**: `crates/torus-core/src/precompiles.rs`

**Current code** — raw `u8` values stored without validation (lines 718-722):
```rust
let side = abi::decode_u8(&abi::word(input, 1)?);           // 0=Buy, 1=Sell
let order_type = abi::decode_u8(&abi::word(input, 2)?);     // 0-3
let time_in_force = abi::decode_u8(&abi::word(input, 5)?);  // 0-3
```

Valid ranges: `Side` (0-1), `OrderType` (0-3), `TimeInForce` (0-3).

**Fix**: Validate after decoding, before enqueuing:
```rust
let side = abi::decode_u8(&abi::word(input, 1)?);
let order_type = abi::decode_u8(&abi::word(input, 2)?);
let price = abi::decode_u128(&abi::word(input, 3)?);
let quantity = abi::decode_u128(&abi::word(input, 4)?);
let time_in_force = abi::decode_u8(&abi::word(input, 5)?);

// FIX ECON-FIND-27: Validate enum discriminants at enqueue time.
if side > 1 {
    return Err(CoreError::InvalidInput(format!("invalid side: {side}")));
}
if order_type > 3 {
    return Err(CoreError::InvalidInput(format!("invalid order_type: {order_type}")));
}
if time_in_force > 3 {
    return Err(CoreError::InvalidInput(format!("invalid time_in_force: {time_in_force}")));
}
```

Add an `InvalidInput(String)` variant to `CoreError` if it doesn't exist. Also validate `price` and `quantity` ranges (same `i128::MAX` check as FIX 14).

---

### FIX 16 | ECON-FIND-30 | Document `delegations_for_validator` O(n) Scan

**File**: `crates/torus-economics/src/staking.rs`

**Current code** — full table scan (lines 845-866). Key layout is `delegator(20) ++ validator(20)`, so prefix iteration by validator suffix is structurally impossible.

**Fix**: Document the limitation. A secondary index would require a schema migration. The function is only used in slashing paths (not per-block):

```rust
/// Read all delegations for a specific validator.
///
/// AUDIT: ECON-FIND-30 — O(n) scan over all delegations. The key layout
/// `delegator(20) ++ validator(20)` prevents prefix-based lookup by validator.
/// A secondary index would fix this but requires schema migration. Acceptable
/// for now: only called during slashing, not on the per-block hot path.
pub fn delegations_for_validator(&self, validator: &Address) -> Result<Vec<Delegation>> {
```

---

### FIX 17 | ECON-FIND-31 | Add Schema Version to Position/NativeBalance

**File**: `crates/torus-core/src/position.rs`

**Current code** — no version byte in serialization (lines 80-92 for Position, 143-149 for NativeBalance).

**Fix**: Add a version byte as the first byte of serialization. Since this is pre-launch with no production data, a breaking change is acceptable:

For Position:
```rust
const POSITION_SCHEMA_VERSION: u8 = 1;

impl BorshSerialize for Position {
    fn serialize<W: Write>(&self, w: &mut W) -> io::Result<()> {
        w.write_all(&[POSITION_SCHEMA_VERSION])?;   // FIX ECON-FIND-31
        borsh_write_address(&self.trader, w)?;
        // ... rest unchanged ...
    }
}

impl BorshDeserialize for Position {
    fn deserialize_reader<R: Read>(r: &mut R) -> io::Result<Self> {
        let mut ver = [0u8; 1];
        r.read_exact(&mut ver)?;
        if ver[0] != POSITION_SCHEMA_VERSION {
            return Err(io::Error::new(io::ErrorKind::InvalidData,
                format!("unknown Position schema version: {}", ver[0])));
        }
        let trader = borsh_read_address(r)?;
        // ... rest unchanged ...
    }
}
```

Same pattern for `NativeBalance`:
```rust
const NATIVE_BALANCE_SCHEMA_VERSION: u8 = 1;

impl BorshSerialize for NativeBalance {
    fn serialize<W: Write>(&self, w: &mut W) -> io::Result<()> {
        w.write_all(&[NATIVE_BALANCE_SCHEMA_VERSION])?;   // FIX ECON-FIND-31
        borsh_write_fp(&self.available, w)?;
        borsh_write_fp(&self.order_margin, w)?;
        Ok(())
    }
}

impl BorshDeserialize for NativeBalance {
    fn deserialize_reader<R: Read>(r: &mut R) -> io::Result<Self> {
        let mut ver = [0u8; 1];
        r.read_exact(&mut ver)?;
        if ver[0] != NATIVE_BALANCE_SCHEMA_VERSION {
            return Err(io::Error::new(io::ErrorKind::InvalidData,
                format!("unknown NativeBalance schema version: {}", ver[0])));
        }
        let available = borsh_read_fp(r)?;
        let order_margin = borsh_read_fp(r)?;
        Ok(Self { available, order_margin })
    }
}
```

**IMPORTANT**: This breaks deserialization of any existing data. Update ALL tests that serialize/deserialize these types. Since this is pre-launch, the breaking change should be acceptable. Add a comment noting this.

---

### FIX 18 | ECON-PF-07 | Document Admin Action Stubs

**File**: `crates/torus-bridge/src/native_executor.rs`

**Current code** — no-op stubs (lines 283-289).

**Fix**: Add documentation explaining the intentional stub status:
```rust
// ---- Admin (governance-gated, stubs) ----
// AUDIT: ECON-PF-07 — Intentional stubs. Market management (listing,
// delisting, param updates) will be implemented in the market registry
// feature. Variants are defined now so governance pipeline and EIP-712
// encoding are stable before the registry is built.
NativeAction::UpdateMarketParams { .. } => {
    NativeActionResult::ok("update_market_params", 0)
}
NativeAction::ListMarket(_) => NativeActionResult::ok("list_market", 0),
NativeAction::DelistMarket { .. } => NativeActionResult::ok("delist_market", 0),
```

---

### FIX 19 | ECON-PF-09 | Safe Unbonding Vec Length Serialization

**File**: `crates/torus-economics/src/types.rs`

**Current code** — silent truncation (line 155):
```rust
BorshSerialize::serialize(&(self.unbonding.len() as u32), writer)?;
```

**Fix**: Use `try_into()` with an error:
```rust
// FIX ECON-PF-09: Validate unbonding length fits in u32 before serializing.
let unbonding_len: u32 = self.unbonding.len().try_into().map_err(|_| {
    io::Error::new(io::ErrorKind::InvalidData, "unbonding vec exceeds u32::MAX")
})?;
BorshSerialize::serialize(&unbonding_len, writer)?;
```

---

## Testing Requirements

### Unit tests — add or update per fix

**hotstuff_rs tests:**
1. `test_advance_view_requires_validator` — verify non-validators can't broadcast AdvanceView via TC path (FIX 1)
2. `test_timeout_vote_future_tip_rejected` — TimeoutVote with `local_tip.view > current_view` is rejected (FIX 2)
3. `test_viewnumber_saturating_add` — `ViewNumber(u64::MAX) + 1` returns `ViewNumber(u64::MAX)` (FIX 8)
4. `test_viewnumber_saturating_sub` — `ViewNumber(0) - 1` returns `ViewNumber(0)` (FIX 8)
5. `test_viewnumber_sub_clamped` — `ViewNumber(0) - ViewNumber(u64::MAX)` doesn't overflow (FIX 8)

**torus-bridge tests:**
6. `test_cancel_order_ownership_check` — user B can't cancel user A's order (FIX 6)
7. `test_eip2930_access_list_propagated` — decode EIP-2930 tx with access list, verify in TxEnv (FIX 7)
8. `test_whitelist_consume_failure_errors` — consume_whitelist Err → registration fails (FIX 12)

**torus-evm tests:**
9-13. Five `calc_effective_gas_price_*` tests as in FIX 10

**torus-core tests:**
14. `test_lockbox_precompile_overflow_rejected` — `amount > i128::MAX` → error (FIX 14)
15. `test_core_writer_invalid_side_rejected` — `side=5` → error at enqueue (FIX 15)
16. `test_core_writer_invalid_order_type_rejected` — `order_type=99` → error (FIX 15)
17. `test_oracle_stale_price_rejected` — stale price → error (FIX 11)

**torus-core position tests:**
18. `test_position_schema_version_roundtrip` — serialize + deserialize Position (FIX 17)
19. `test_native_balance_schema_version_roundtrip` — serialize + deserialize NativeBalance (FIX 17)

**torus-economics tests:**
20. `test_unbonding_length_serialization_safe` — roundtrip with 50 entries (FIX 19)

---

## Architecture Constraints

1. **No new crates** — add to existing crates only
2. **Deterministic** — all changes must produce identical results across nodes
3. **No floating point** — integer math only, basis points for fractions
4. **Borsh serialization** — any new persisted types must derive `BorshSerialize, BorshDeserialize`
5. **Don't break existing tests** — run the full test suite after all changes
6. **Don't modify unrelated code** — touch only what's needed for these 20 findings
7. **Read before editing** — line numbers in this prompt may have shifted. Always read the file first.
8. **Error types** — add new variants to existing error enums (`CoreError`, `EconomicsError`) rather than creating new error types
9. **Follow existing patterns** — look at how Batch KL (commit `51afebf`) and Batch IJ (commit `35f4d2f`) were done
10. **Schema version changes** (FIX 17) — pre-launch with no persistent data, breaking change acceptable

---

## File Manifest (expected changes)

| File | Action | Fixes |
|---|---|---|
| `hotstuff_rs/src/pacemaker/implementation.rs` | Edit | FIX 1 (precedence), FIX 2 (TipInfo validation) |
| `hotstuff_rs/src/hotstuff/implementation.rs` | Edit | FIX 3 (nudge local_tip), FIX 4 (shuffle recipients) |
| `hotstuff_rs/src/types/data_types.rs` | Edit | FIX 8 (saturating arithmetic) |
| `torus-network/src/peer_scoring.rs` | Edit | FIX 5 (async ban I/O) |
| `torus-bridge/src/native_executor.rs` | Edit | FIX 6 (cancel ownership), FIX 12 (whitelist), FIX 13 (dead FeeSplitter), FIX 18 (stub comment) |
| `torus-bridge/src/decode.rs` | Edit | FIX 7 (access_list) |
| `torus-state/src/cf.rs` | Edit | FIX 9 (CF_LOGS comment) |
| `torus-bridge/src/committer.rs` | Edit | FIX 9 (CF_LOGS comment) |
| `torus-evm/src/executor.rs` | Edit | FIX 10 (unit tests) |
| `torus-core/src/oracle.rs` | Edit | FIX 11 (staleness check) |
| `torus-core/src/precompiles.rs` | Edit | FIX 14 (lockbox cast), FIX 15 (discriminant validation) |
| `torus-economics/src/staking.rs` | Edit | FIX 16 (O(n) scan comment) |
| `torus-core/src/position.rs` | Edit | FIX 17 (schema version) |
| `torus-economics/src/types.rs` | Edit | FIX 19 (unbonding length check) |

---

## Verification Checklist

- [ ] `cargo check --workspace` passes with zero errors
- [ ] `cargo clippy --workspace` passes with zero new warnings
- [ ] `cargo test -p torus-core -p torus-economics -p torus-bridge -p torus-evm -p torus-mempool -p torus-rpc -p torus-network` — all pass
- [ ] `cargo test -p hotstuff_rs` — all pass (known exception: `multiple_validator_set_updates_test` may livelock — pre-existing, ignore)
- [ ] AdvanceView requires `is_validator` in both code paths (FIX 1)
- [ ] Future-view TipInfo rejected in timeout votes (FIX 2)
- [ ] Nudge updates `local_tip` (FIX 3)
- [ ] ProposalRequest recipients are shuffled (FIX 4)
- [ ] Ban file I/O is non-blocking (FIX 5)
- [ ] `cancel_order` verifies sender ownership (FIX 6)
- [ ] EIP-2930/EIP-1559 access lists propagated to TxEnv (FIX 7)
- [ ] ViewNumber arithmetic is saturating (FIX 8)
- [ ] CF_LOGS/CF_LOGS_BLOOM deferred status documented (FIX 9)
- [ ] executor.rs has unit tests (FIX 10)
- [ ] `get_last_valid_price` checks staleness (FIX 11)
- [ ] Whitelist consume failure fails registration (FIX 12)
- [ ] Dead `FeeSplitter::split_fees` call removed (FIX 13)
- [ ] Lockbox precompile rejects `amount > i128::MAX` (FIX 14)
- [ ] CoreWriter rejects invalid `side`/`order_type`/`time_in_force` (FIX 15)
- [ ] `delegations_for_validator` O(n) documented (FIX 16)
- [ ] Position/NativeBalance have schema version byte (FIX 17)
- [ ] Admin stub status documented (FIX 18)
- [ ] Unbonding vec length checked before u32 cast (FIX 19)

## Commit Strategy

Single commit: `fix(low): resolve 20 remaining low/informational audit findings (Batch MN)`
