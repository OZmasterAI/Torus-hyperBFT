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

/// Compute the EVM state root after applying the given `BundleState`.
///
/// Reads all accounts and storage from the DB, merges `BundleState` changes
/// in memory, and computes the MPT root. O(n) in total state size —
/// suitable for genesis and testing.
fn compute_post_bundle_evm_root(
    state_db: &StateDb,
    bundle: &BundleState,
) -> Result<B256, StateError> {
    // 1. Load all existing accounts.
    let existing = state_db.all_accounts()?;
    let mut accounts: BTreeMap<Address, AccountInfo> = existing.into_iter().collect();

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
