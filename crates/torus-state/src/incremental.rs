//! Incremental EVM state root via reth's `StateRoot` engine (Phase A).
//!
//! [`build_trie_to_cf`] is the one-time migration that builds the persistent trie (CF_TRIE_*) and
//! the keccak-ordered hashed-state mirror (CF_HASHED_*) from the current plain state. The
//! per-block incremental path (`incremental_evm_root`, Task A1.3) builds on the same cursor
//! factories from `trie_cursor`.

use alloy_primitives::{keccak256, B256};
use reth_trie::prefix_set::{PrefixSetMut, TriePrefixSetsMut};
use reth_trie::StateRoot;
use rocksdb::WriteBatch;

use crate::cf::{CF_HASHED_ACCOUNTS, CF_HASHED_STORAGE};
use crate::db::{encode_account_info, StateDb};
use crate::error::StateError;
use crate::trie_cursor::{write_trie_updates, RocksHashedCursorFactory, RocksTrieCursorFactory};

/// Storage key in `CF_HASHED_STORAGE`: keccak(address)(32) ++ keccak(slot)(32).
fn hashed_storage_key(hashed_address: &B256, hashed_slot: &B256) -> [u8; 64] {
    let mut key = [0u8; 64];
    key[..32].copy_from_slice(hashed_address.as_slice());
    key[32..].copy_from_slice(hashed_slot.as_slice());
    key
}

/// One-time migration: build the persistent MPT (`CF_TRIE_*`) and the keccak-ordered hashed-state
/// mirror (`CF_HASHED_*`) from the current plain state (`CF_ACCOUNTS` / `CF_STORAGE`), returning
/// the resulting EVM state root.
///
/// **Determinism gate (A1.2):** the returned root equals
/// [`crate::trie::compute_state_root_from_db`] (the full-scan oracle) for any state — reth's
/// `StateRoot` and `state_root_unhashed` drive the same `HashBuilder`. Idempotent: re-running
/// reproduces the same root and node set.
pub fn build_trie_to_cf(db: &StateDb) -> Result<B256, StateError> {
    // 1. Mirror plain state into the keccak-ordered hashed-state CFs, recording which accounts
    //    have storage (so their storage tries are marked dirty for the from-scratch rebuild).
    let mut batch = WriteBatch::default();
    let hashed_acc_cf = db.cf_handle(CF_HASHED_ACCOUNTS)?;
    let hashed_stor_cf = db.cf_handle(CF_HASHED_STORAGE)?;

    let accounts = db.all_accounts()?;
    let mut storage_addrs: Vec<B256> = Vec::new();
    for (address, info) in &accounts {
        let hashed_address = keccak256(address.as_slice());
        batch.put_cf(hashed_acc_cf, hashed_address.as_slice(), encode_account_info(info));

        let mut has_storage = false;
        for (slot, value) in db.account_storage(address)? {
            // Zero slots are absent from the canonical trie (matches compute_storage_root).
            if value.is_zero() {
                continue;
            }
            has_storage = true;
            let hashed_slot = keccak256(slot.to_be_bytes::<32>());
            batch.put_cf(
                hashed_stor_cf,
                hashed_storage_key(&hashed_address, &hashed_slot),
                value.to_be_bytes::<32>(),
            );
        }
        if has_storage {
            storage_addrs.push(hashed_address);
        }
    }
    db.write(batch)?;

    // 2. Compute root + full TrieUpdates over the (empty) trie cursor + populated hashed cursor,
    //    marking everything changed so reth walks and rebuilds the whole trie.
    let mut prefix_sets = TriePrefixSetsMut::default();
    prefix_sets.account_prefix_set = PrefixSetMut::all();
    for hashed_address in &storage_addrs {
        prefix_sets.storage_prefix_sets.insert(*hashed_address, PrefixSetMut::all());
    }

    let (root, updates) =
        StateRoot::new(RocksTrieCursorFactory::new(db), RocksHashedCursorFactory::new(db))
            .with_prefix_sets(prefix_sets.freeze())
            .root_with_updates()
            .map_err(|e| StateError::InvalidData(format!("incremental state root: {e}")))?;

    // 3. Persist the trie nodes.
    let mut batch = WriteBatch::default();
    write_trie_updates(db, &mut batch, &updates)?;
    db.write(batch)?;

    Ok(root)
}

#[cfg(test)]
mod tests {
    use super::build_trie_to_cf;
    use crate::cf::CF_TRIE_ACCOUNTS;
    use crate::db::{StateDb, KECCAK_EMPTY};
    use crate::trie::compute_state_root_from_db;
    use alloy_primitives::{address, keccak256, Address, U256};
    use revm::state::AccountInfo;

    fn temp_db() -> (StateDb, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        (StateDb::open(dir.path()).expect("open db"), dir)
    }

    fn eoa(balance: u64, nonce: u64) -> AccountInfo {
        AccountInfo { balance: U256::from(balance), nonce, code_hash: KECCAK_EMPTY, account_id: None, code: None }
    }

    /// Seed a mix of accounts (EOAs, a contract with multiple storage slots, an account with a
    /// single slot) and enough distinct addresses to force real branch nodes.
    fn seed(db: &StateDb) {
        db.put_account(&address!("0000000000000000000000000000000000000001"), &eoa(100, 1)).unwrap();

        let contract = address!("00000000000000000000000000000000000000aa");
        db.put_account(
            &contract,
            &AccountInfo {
                balance: U256::from(5u64),
                nonce: 7,
                code_hash: keccak256([1u8, 2, 3]),
                account_id: None,
                code: None,
            },
        )
        .unwrap();
        db.put_storage(&contract, &U256::from(0u64), &U256::from(42u64)).unwrap();
        db.put_storage(&contract, &U256::from(1u64), &U256::from(99u64)).unwrap();
        db.put_storage(&contract, &U256::from(1000u64), &U256::from(7u64)).unwrap();

        for i in 2..9u8 {
            let mut bytes = [0u8; 20];
            bytes[0] = i;
            bytes[19] = i;
            db.put_account(&Address::from(bytes), &eoa(i as u64 * 1000, i as u64)).unwrap();
        }

        let single = address!("00000000000000000000000000000000000000bb");
        db.put_account(&single, &eoa(1, 0)).unwrap();
        db.put_storage(&single, &U256::from(5u64), &U256::from(123u64)).unwrap();

        // Bulk accounts so the (keccak-hashed) trie has guaranteed non-root branch nodes
        // (reth never persists the root node itself), exercising real node persistence + the
        // determinism gate at a meaningful structure depth.
        for i in 0u32..300 {
            let mut bytes = [0u8; 20];
            bytes[0..4].copy_from_slice(&i.to_be_bytes());
            bytes[19] = 0x5a;
            let addr = Address::from(bytes);
            db.put_account(&addr, &eoa(1_000 + i as u64, i as u64)).unwrap();
            if i % 10 == 0 {
                db.put_storage(&addr, &U256::from(i), &U256::from(i + 1)).unwrap();
                db.put_storage(&addr, &U256::from(i + 7), &U256::from(i + 2)).unwrap();
            }
        }
    }

    #[test]
    fn trie_migration_root_matches_full_scan() {
        let (db, _dir) = temp_db();
        seed(&db);

        let oracle = compute_state_root_from_db(&db).expect("oracle root");
        let migrated = build_trie_to_cf(&db).expect("migration");
        assert_eq!(migrated, oracle, "migrated trie root must equal full-scan oracle");

        // Idempotent: re-running reproduces the same root.
        let again = build_trie_to_cf(&db).expect("migration #2");
        assert_eq!(again, oracle, "migration must be idempotent");

        // Trie nodes were actually persisted.
        let cf = db.cf_handle(CF_TRIE_ACCOUNTS).unwrap();
        let mut iter = db.inner().raw_iterator_cf(cf);
        iter.seek_to_first();
        assert!(iter.valid(), "account trie CF should be non-empty after migration");
    }

    #[test]
    fn trie_migration_empty_state_is_empty_root() {
        let (db, _dir) = temp_db();
        let oracle = compute_state_root_from_db(&db).expect("oracle root");
        let migrated = build_trie_to_cf(&db).expect("migration");
        assert_eq!(migrated, oracle, "empty-state migration must match oracle (EMPTY_ROOT_HASH)");
    }
}
