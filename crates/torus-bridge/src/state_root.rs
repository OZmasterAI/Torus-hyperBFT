//! State root computation from current DB state + a `BundleState` overlay.
//!
//! Computes the EVM state root that *would* result from applying a `BundleState`
//! without actually modifying the database. Used during block proposal and validation.

use std::collections::BTreeMap;

use alloy_primitives::{Address, B256, U256};
use revm::database::BundleState;
use revm::state::AccountInfo;

use torus_state::db::StateDb;
use torus_state::error::StateError;
use torus_state::trie::{
    compute_composite_root, compute_state_root, compute_storage_root, TrieAccount, EMPTY_ROOT_HASH,
};

/// Compute the composite state root (EVM + native) after applying the given `BundleState`.
///
/// Native state is a no-op in Phase 1 — native root is `EMPTY_ROOT_HASH`.
/// Does NOT modify the database.
pub fn compute_post_bundle_state_root(
    state_db: &StateDb,
    bundle: &BundleState,
) -> Result<B256, StateError> {
    let evm_root = compute_post_bundle_evm_root(state_db, bundle)?;
    Ok(compute_composite_root(evm_root, EMPTY_ROOT_HASH))
}

/// Compute the composite state root (EVM + native) with an explicit native root.
///
/// Used in Phase 2 when native execution produces actual state changes.
/// The native root is computed by the caller (e.g. from native CF hashes).
pub fn compute_full_composite_root(
    state_db: &StateDb,
    bundle: &BundleState,
    native_root: B256,
) -> Result<B256, StateError> {
    let evm_root = compute_post_bundle_evm_root(state_db, bundle)?;
    Ok(compute_composite_root(evm_root, native_root))
}

/// Compute a native state root by hashing key native column families.
///
/// Iterates all entries in native balance and position CFs with length-framed
/// key-value pairs, and returns `keccak256(data)`. Length framing prevents
/// hash collisions between entries with different key/value boundaries.
///
/// Iterator errors are propagated — a DB error must fail loudly rather than
/// silently omitting entries and producing an incorrect root.
pub fn compute_native_state_root(
    state_db: &StateDb,
) -> Result<B256, torus_state::error::StateError> {
    use torus_state::cf::{
        CF_NATIVE_BALANCES, CF_NATIVE_ORDER_BOOKS, CF_NATIVE_ORACLE, CF_NATIVE_POSITIONS,
        CF_STAKING_DELEGATIONS, CF_STAKING_VALIDATORS,
    };

    let db = state_db.inner();
    let mut data = Vec::new();

    // Hash each native CF's contents in deterministic order.
    for cf_name in &[
        CF_NATIVE_BALANCES,
        CF_NATIVE_ORDER_BOOKS,
        CF_NATIVE_POSITIONS,
        CF_NATIVE_ORACLE,
        CF_STAKING_DELEGATIONS,
        CF_STAKING_VALIDATORS,
    ] {
        if let Some(cf) = db.cf_handle(cf_name) {
            let iter = db.iterator_cf(cf, rocksdb::IteratorMode::Start);
            for item in iter {
                // FIX 8: Propagate iterator errors instead of silently skipping.
                let (key, value) = item?;
                // FIX 2: Length-frame each element to prevent hash collisions.
                // ("ab","cd") and ("a","bcd") now produce different byte sequences.
                data.extend_from_slice(&(key.len() as u32).to_le_bytes());
                data.extend_from_slice(&key);
                data.extend_from_slice(&(value.len() as u32).to_le_bytes());
                data.extend_from_slice(&value);
            }
        }
    }

    if data.is_empty() {
        return Ok(EMPTY_ROOT_HASH);
    }

    Ok(alloy_primitives::keccak256(&data))
}

/// Compute the EVM state root after applying the given `BundleState`.
///
/// FIX 7 (EVM-PF-03): Streams accounts directly from the DB iterator into
/// the BTreeMap instead of collecting into an intermediate Vec first.
/// This halves peak memory during state root computation.
fn compute_post_bundle_evm_root(
    state_db: &StateDb,
    bundle: &BundleState,
) -> Result<B256, StateError> {
    // 1. Stream existing accounts directly into BTreeMap (no intermediate Vec).
    use torus_state::cf::CF_ACCOUNTS;
    use torus_state::db::decode_account_info;

    let mut accounts: BTreeMap<Address, AccountInfo> = BTreeMap::new();
    let cf = state_db.cf_handle(CF_ACCOUNTS)?;
    let iter = state_db.inner().iterator_cf(cf, rocksdb::IteratorMode::Start);
    for item in iter {
        let (key, value) = item?;
        if key.len() != 20 {
            return Err(StateError::InvalidData(format!(
                "account key len {} != 20",
                key.len()
            )));
        }
        let address = Address::from_slice(&key);
        let info = decode_account_info(&value)?;
        accounts.insert(address, info);
    }

    // 2. Apply bundle account changes.
    for (addr, bundle_acct) in &bundle.state {
        match &bundle_acct.info {
            Some(info) => {
                accounts.insert(*addr, info.clone());
            }
            None => {
                // Account destroyed or never existed — remove if present.
                accounts.remove(addr);
            }
        }
    }

    // Remove accounts with zero nonce, zero balance, and empty code hash
    // (EIP-161 state clearing).
    accounts.retain(|_, info| {
        !(info.nonce == 0
            && info.balance.is_zero()
            && (info.code_hash == torus_state::db::KECCAK_EMPTY || info.code_hash == B256::ZERO))
    });

    if accounts.is_empty() {
        return Ok(EMPTY_ROOT_HASH);
    }

    // 3. Build trie accounts with storage roots.
    let mut trie_accounts = Vec::with_capacity(accounts.len());

    for (addr, info) in &accounts {
        // Start from existing storage.
        let existing_storage = state_db.account_storage(addr)?;
        let mut storage: BTreeMap<U256, U256> = existing_storage.into_iter().collect();

        // Apply bundle storage changes.
        if let Some(bundle_acct) = bundle.state.get(addr) {
            for (slot, slot_val) in &bundle_acct.storage {
                if slot_val.present_value.is_zero() {
                    storage.remove(slot);
                } else {
                    storage.insert(*slot, slot_val.present_value);
                }
            }
        }

        let storage_root = if storage.is_empty() {
            EMPTY_ROOT_HASH
        } else {
            compute_storage_root(storage.into_iter())
        };

        trie_accounts.push((
            *addr,
            TrieAccount {
                nonce: info.nonce,
                balance: info.balance,
                storage_root,
                code_hash: info.code_hash,
            },
        ));
    }

    Ok(compute_state_root(trie_accounts))
}
