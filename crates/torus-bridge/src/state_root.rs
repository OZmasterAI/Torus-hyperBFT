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
use torus_state::incremental::TrieUpdates;
use torus_state::trie::{
    compute_composite_root, compute_state_root, compute_storage_root, TrieAccount, EMPTY_ROOT_HASH,
};

/// Runtime switch (Phase A **default ON**, devnet-baked): compute the EVM + native state root
/// incrementally (O(changed)/block) instead of by full scan. `TORUS_INCREMENTAL_STATE_ROOT=0` (or
/// `false`) is the kill-switch back to the proven full-scan root. The node always builds/maintains
/// the tries at boot (`ensure_trie_built` / `ensure_native_trie_built`), so the incremental base is
/// ready before any block; the validator `StateRootMismatch` check remains the live safety net.
fn incremental_state_root_enabled() -> bool {
    match std::env::var("TORUS_INCREMENTAL_STATE_ROOT") {
        Ok(v) => !(v == "0" || v.eq_ignore_ascii_case("false")),
        Err(_) => true,
    }
}

/// EVM post-bundle root honoring the incremental flag. Split out with an explicit `incremental`
/// param so both paths are unit-testable without touching process env.
///
/// T4.1: also surfaces the incremental engine's [`TrieUpdates`] so the ONE validation-time
/// computation can be reused by `commit_evm_bundle_incremental` (plumbed via `ValidatedBlock`);
/// the full-scan fallback yields `None`.
fn evm_root_routed(
    state_db: &StateDb,
    bundle: &BundleState,
    incremental: bool,
) -> Result<(B256, Option<TrieUpdates>), StateError> {
    // Fall back to the full scan when incremental is off OR the persistent trie hasn't been built
    // yet (an unmigrated DB — production builds it at boot, but unit tests may bypass that). This
    // keeps default-on safe: a missing trie yields the correct full-scan root, never a wrong one.
    if !incremental || !torus_state::incremental::is_trie_built(state_db)? {
        return Ok((compute_post_bundle_evm_root(state_db, bundle)?, None));
    }
    let (root, updates) = torus_state::incremental::incremental_evm_root(state_db, bundle)?;
    // Runtime determinism oracle: under debug or TORUS_INCREMENTAL_ORACLE, cross-check against the
    // full scan and fail loudly on any divergence (a consensus-splitting bug) rather than voting it.
    if cfg!(debug_assertions) || std::env::var("TORUS_INCREMENTAL_ORACLE").is_ok() {
        let full = compute_post_bundle_evm_root(state_db, bundle)?;
        if root != full {
            return Err(StateError::InvalidData(format!(
                "incremental state-root divergence: incremental={root} full-scan={full}"
            )));
        }
    }
    Ok((root, Some(updates)))
}

/// EVM post-bundle root, routed by the runtime flag ([`incremental_state_root_enabled`]).
fn flagged_evm_root(
    state_db: &StateDb,
    bundle: &BundleState,
) -> Result<(B256, Option<TrieUpdates>), StateError> {
    evm_root_routed(state_db, bundle, incremental_state_root_enabled())
}

/// Native root honoring the incremental flag — the symmetric twin of [`evm_root_routed`]. Flag-off
/// computes the bucketed-Merkle root by full scan (`native_root_full`, the determinism ORACLE);
/// flag-on reads the incrementally-maintained persisted root and, under debug or
/// `TORUS_INCREMENTAL_ORACLE`, cross-checks it against the full scan, failing loud on any divergence
/// (a consensus-splitting bug) rather than voting it.
fn native_root_routed(state_db: &StateDb, incremental: bool) -> Result<B256, StateError> {
    // Same full-scan fallback as the EVM half when incremental is off or the native trie is unbuilt.
    if !incremental || !torus_state::native_trie::is_native_trie_built(state_db)? {
        return torus_state::native_trie::native_root_full(state_db);
    }
    let persisted = torus_state::native_trie::persisted_native_root(state_db)?;
    if cfg!(debug_assertions) || std::env::var("TORUS_INCREMENTAL_ORACLE").is_ok() {
        let full = torus_state::native_trie::native_root_full(state_db)?;
        if persisted != full {
            return Err(StateError::InvalidData(format!(
                "incremental native-root divergence: incremental={persisted} full-scan={full}"
            )));
        }
    }
    Ok(persisted)
}

/// Native state root, routed by the runtime flag ([`incremental_state_root_enabled`]). Replaces the
/// flat keccak ([`compute_native_state_root`]) at the consensus callsites: BOTH flag states compute
/// the same bucketed-Merkle root (value-neutral flag), so flipping the flag needs no coordination —
/// the new binary (which maintains the native trie) is the unit of upgrade.
pub fn flagged_native_root(state_db: &StateDb) -> Result<B256, StateError> {
    native_root_routed(state_db, incremental_state_root_enabled())
}

/// Compute the composite state root (EVM + native) after applying the given `BundleState`.
///
/// Native state is a no-op in Phase 1 — native root is `EMPTY_ROOT_HASH`.
/// Does NOT modify the database.
pub fn compute_post_bundle_state_root(
    state_db: &StateDb,
    bundle: &BundleState,
) -> Result<B256, StateError> {
    Ok(compute_post_bundle_state_root_with_updates(state_db, bundle)?.0)
}

/// [`compute_post_bundle_state_root`] variant that also surfaces the incremental engine's
/// `(evm_root, TrieUpdates)` pair when it ran (T4.1). The pair is reusable by
/// `commit_evm_bundle_incremental` for the SAME bundle over the SAME committed base, so the
/// committed-block path computes the EVM root exactly once.
pub fn compute_post_bundle_state_root_with_updates(
    state_db: &StateDb,
    bundle: &BundleState,
) -> Result<(B256, Option<(B256, TrieUpdates)>), StateError> {
    let (evm_root, updates) = flagged_evm_root(state_db, bundle)?;
    Ok((
        compute_composite_root(evm_root, EMPTY_ROOT_HASH),
        updates.map(|u| (evm_root, u)),
    ))
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
    Ok(compute_full_composite_root_with_updates(state_db, bundle, native_root)?.0)
}

/// [`compute_full_composite_root`] variant surfacing the incremental engine's
/// `(evm_root, TrieUpdates)` pair when it ran — the native-pipeline twin of
/// [`compute_post_bundle_state_root_with_updates`] (T4.1).
pub fn compute_full_composite_root_with_updates(
    state_db: &StateDb,
    bundle: &BundleState,
    native_root: B256,
) -> Result<(B256, Option<(B256, TrieUpdates)>), StateError> {
    let (evm_root, updates) = flagged_evm_root(state_db, bundle)?;
    Ok((
        compute_composite_root(evm_root, native_root),
        updates.map(|u| (evm_root, u)),
    ))
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
        CF_NATIVE_BALANCES, CF_NATIVE_ORACLE, CF_NATIVE_ORDER_BOOKS, CF_NATIVE_POSITIONS,
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
    let iter = state_db
        .inner()
        .iterator_cf(cf, rocksdb::IteratorMode::Start);
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

#[cfg(test)]
mod tests {
    use super::evm_root_routed;
    use alloy_primitives::{address, Address, U256};
    use revm::database::BundleState;
    use revm::state::AccountInfo;
    use torus_state::db::{StateDb, KECCAK_EMPTY};
    use torus_state::incremental::build_trie_to_cf;

    /// The bridge-level routing must produce the same EVM root via the incremental engine as via
    /// the full scan (the determinism gate, exercised through state_root.rs's own dispatch).
    #[test]
    fn bridge_incremental_evm_root_matches_full_scan() {
        let dir = tempfile::tempdir().unwrap();
        let db = StateDb::open(dir.path()).unwrap();
        for i in 1u8..24 {
            let mut b = [0u8; 20];
            b[0] = i;
            b[19] = i;
            db.put_account(
                &Address::from(b),
                &AccountInfo {
                    balance: U256::from(i as u64 * 1000),
                    nonce: i as u64,
                    code_hash: KECCAK_EMPTY,
                    account_id: None,
                    code: None,
                },
            )
            .unwrap();
        }
        build_trie_to_cf(&db).unwrap();

        let bundle = BundleState::builder(0..=0)
            .state_present_account_info(
                address!("0c000000000000000000000000000000000000cc"),
                AccountInfo {
                    balance: U256::from(42u64),
                    nonce: 9,
                    code_hash: KECCAK_EMPTY,
                    account_id: None,
                    code: None,
                },
            )
            .build();

        let (full, full_updates) = evm_root_routed(&db, &bundle, false).unwrap();
        let (incremental, incr_updates) = evm_root_routed(&db, &bundle, true).unwrap();
        assert_eq!(
            full, incremental,
            "bridge routing: incremental EVM root must equal full scan"
        );
        // T4.1: the incremental path must surface its TrieUpdates for commit-time reuse;
        // the full scan has none to offer.
        assert!(full_updates.is_none(), "full-scan path yields no updates");
        assert!(
            incr_updates.is_some(),
            "incremental path must surface TrieUpdates for reuse"
        );
    }
}
