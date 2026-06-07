//! MPT state root computation using reth-trie.
//!
//! Provides Ethereum-compatible Merkle Patricia Trie state root computation
//! using the utility functions from `reth-trie-common` (backed by `alloy-trie`).

use alloy_primitives::{Address, B256, U256};
use reth_trie_common::root::{state_root_unhashed, storage_root_unhashed};
pub use reth_trie_common::TrieAccount;
pub use reth_trie_common::EMPTY_ROOT_HASH;

use crate::db::StateDb;
use crate::error::StateError;

/// Compute the Ethereum MPT storage root for a single account.
///
/// Takes (slot, value) pairs where slot is `U256`. Internally hashes each
/// slot key with keccak256 and sorts, matching the Ethereum storage trie layout.
/// Returns `EMPTY_ROOT_HASH` when there are no non-zero storage entries.
pub fn compute_storage_root(storage: impl IntoIterator<Item = (U256, U256)>) -> B256 {
    let entries: Vec<(B256, U256)> = storage
        .into_iter()
        .filter(|(_, v)| !v.is_zero())
        .map(|(slot, value)| (B256::from(slot.to_be_bytes::<32>()), value))
        .collect();

    if entries.is_empty() {
        return EMPTY_ROOT_HASH;
    }
    storage_root_unhashed(entries)
}

/// Compute the Ethereum MPT state root from a set of accounts.
///
/// Each account needs its `storage_root` pre-computed. The function hashes
/// each address with keccak256 internally, matching the Ethereum state trie layout.
/// Returns `EMPTY_ROOT_HASH` for an empty set of accounts.
pub fn compute_state_root(accounts: impl IntoIterator<Item = (Address, TrieAccount)>) -> B256 {
    let entries: Vec<_> = accounts.into_iter().collect();
    if entries.is_empty() {
        return EMPTY_ROOT_HASH;
    }
    state_root_unhashed(entries)
}

/// Compute the state root by iterating all state in the database.
///
/// This is a full recomputation: it reads every account and every storage slot.
/// Suitable for genesis and testing. For production block processing, use the
/// incremental `StateRoot` with cursor factories.
pub fn compute_state_root_from_db(db: &StateDb) -> Result<B256, StateError> {
    let accounts = db.all_accounts()?;
    if accounts.is_empty() {
        return Ok(EMPTY_ROOT_HASH);
    }

    let mut trie_accounts = Vec::with_capacity(accounts.len());

    for (address, info) in &accounts {
        let storage_slots = db.account_storage(address)?;
        let storage_root = compute_storage_root(storage_slots);

        trie_accounts.push((
            *address,
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

/// Compute the composite state root (EVM + native) per section 6.3.
///
/// `composite_root = keccak256(evm_root || native_root)`
pub fn compute_composite_root(evm_root: B256, native_root: B256) -> B256 {
    let mut data = [0u8; 64];
    data[..32].copy_from_slice(evm_root.as_slice());
    data[32..].copy_from_slice(native_root.as_slice());
    alloy_primitives::keccak256(data)
}

// `compute_native_state_root_from_db` (a divergent 5-CF flat keccak, missing CF_NATIVE_ORDER_BOOKS)
// was removed in Phase A A2.3. The native root is now the bucketed-Merkle root
// (`crate::native_trie::native_root_full`), which covers all 6 native CFs and matches the consensus
// native root — the single source of truth used by both proposer/validator and snapshot verification.
