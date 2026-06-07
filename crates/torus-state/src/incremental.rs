//! Incremental EVM state root via reth's `StateRoot` engine (Phase A).
//!
//! [`build_trie_to_cf`] is the one-time migration that builds the persistent trie (CF_TRIE_*) and
//! the keccak-ordered hashed-state mirror (CF_HASHED_*) from the current plain state. The
//! per-block incremental path (`incremental_evm_root`, Task A1.3) builds on the same cursor
//! factories from `trie_cursor`.

use std::collections::BTreeMap;

use alloy_primitives::{keccak256, Address, B256, U256};
use reth_trie::hashed_cursor::HashedPostStateCursorFactory;
use reth_trie::prefix_set::{PrefixSetMut, TriePrefixSetsMut};
use reth_trie::StateRoot;
use reth_trie_common::updates::TrieUpdates;
use reth_trie_common::{HashedPostState, KeccakKeyHasher};
use revm::database::BundleState;
use revm::state::AccountInfo;
use rocksdb::WriteBatch;

use crate::cf::{CF_ACCOUNTS, CF_HASHED_ACCOUNTS, CF_HASHED_STORAGE, CF_STORAGE};
use crate::db::{decode_account_info, encode_account_info, storage_key, StateDb, KECCAK_EMPTY};
use crate::error::StateError;
use crate::trie::{compute_state_root, compute_storage_root, TrieAccount, EMPTY_ROOT_HASH};
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

/// `true` if `info` is an EIP-161 "empty" account (zero nonce, zero balance, no code) — such
/// accounts are excluded from the state trie.
fn is_empty_account(info: &AccountInfo) -> bool {
    info.nonce == 0
        && info.balance.is_zero()
        && (info.code_hash == KECCAK_EMPTY || info.code_hash == B256::ZERO)
}

/// Full-scan EVM state root after applying `bundle` — the determinism ORACLE for A1.3/A1.5.
///
/// Mirrors `torus-bridge::state_root::compute_post_bundle_evm_root` exactly: load all accounts,
/// apply the bundle (`Some` = upsert, `None` = remove), EIP-161-clear empty accounts, recompute
/// each surviving account's storage root from DB + bundle storage, then hash. O(total state).
pub fn full_post_bundle_evm_root(db: &StateDb, bundle: &BundleState) -> Result<B256, StateError> {
    let mut accounts: BTreeMap<Address, AccountInfo> = BTreeMap::new();
    let cf = db.cf_handle(CF_ACCOUNTS)?;
    let iter = db.inner().iterator_cf(cf, rocksdb::IteratorMode::Start);
    for item in iter {
        let (key, value) = item?;
        if key.len() != 20 {
            return Err(StateError::InvalidData(format!("account key len {} != 20", key.len())));
        }
        accounts.insert(Address::from_slice(&key), decode_account_info(&value)?);
    }

    for (addr, bundle_acct) in &bundle.state {
        match &bundle_acct.info {
            Some(info) => {
                accounts.insert(*addr, info.clone());
            }
            None => {
                accounts.remove(addr);
            }
        }
    }
    accounts.retain(|_, info| !is_empty_account(info));

    if accounts.is_empty() {
        return Ok(EMPTY_ROOT_HASH);
    }

    let mut trie_accounts = Vec::with_capacity(accounts.len());
    for (addr, info) in &accounts {
        let mut storage: BTreeMap<U256, U256> = db.account_storage(addr)?.into_iter().collect();
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

/// Incremental EVM state root after applying `bundle`, over the committed persistent trie.
///
/// Computes the post-bundle root in O(changed) via reth's `StateRoot` over the committed
/// `CF_TRIE_*` nodes + a `HashedPostState` OVERLAY of the bundle on top of the committed
/// `CF_HASHED_*` cursors. Does NOT mutate any CF — the root is computed pre-commit and the returned
/// [`TrieUpdates`] are persisted atomically with the state write at commit time (Task A1.4). This
/// avoids the eager-commit state pollution behind prior `StateRootMismatch` (mem f3d3f858).
///
/// EIP-161 parity: bundle accounts that end empty are marked deleted, matching
/// [`full_post_bundle_evm_root`]; the result is byte-identical to that oracle (the A1.3 gate).
pub fn incremental_evm_root(
    db: &StateDb,
    bundle: &BundleState,
) -> Result<(B256, TrieUpdates), StateError> {
    // Hash the bundle diff into a post-state overlay (reth handles storage zeroing / wipes).
    let mut hashed = HashedPostState::from_bundle_state::<KeccakKeyHasher>(bundle.state.iter());
    // EIP-161: an account that ends empty is dropped from the trie. reth's from_bundle_state keeps
    // `Some(empty)`; override those to `None` so we match the full-scan oracle's `retain`.
    for (addr, bundle_acct) in &bundle.state {
        if let Some(info) = &bundle_acct.info {
            if is_empty_account(info) {
                hashed.accounts.insert(keccak256(addr.as_slice()), None);
            }
        }
    }

    let prefix_sets = hashed.construct_prefix_sets().freeze();
    let sorted = hashed.into_sorted();

    let hashed_factory =
        HashedPostStateCursorFactory::new(RocksHashedCursorFactory::new(db), &sorted);
    let trie_factory = RocksTrieCursorFactory::new(db);

    StateRoot::new(trie_factory, hashed_factory)
        .with_prefix_sets(prefix_sets)
        .root_with_updates()
        .map_err(|e| StateError::InvalidData(format!("incremental evm root: {e}")))
}

/// Apply `bundle` to the plain EVM state (`CF_ACCOUNTS` / `CF_STORAGE`) into `batch`.
///
/// Mirrors `torus-bridge::committer::commit_block` exactly (no EIP-161 here — revm nulls empty
/// accounts in the bundle), so `CF_ACCOUNTS` stays byte-identical to the production commit path.
pub fn apply_bundle_plain(
    db: &StateDb,
    batch: &mut WriteBatch,
    bundle: &BundleState,
) -> Result<(), StateError> {
    let cf_accounts = db.cf_handle(CF_ACCOUNTS)?;
    let cf_storage = db.cf_handle(CF_STORAGE)?;
    for (address, bundle_acct) in &bundle.state {
        match &bundle_acct.info {
            Some(info) => {
                batch.put_cf(cf_accounts, address.as_slice(), encode_account_info(info));
                for (slot, slot_val) in &bundle_acct.storage {
                    let key = storage_key(address, slot);
                    if slot_val.present_value.is_zero() {
                        batch.delete_cf(cf_storage, key);
                    } else {
                        batch.put_cf(cf_storage, key, slot_val.present_value.to_be_bytes::<32>());
                    }
                }
            }
            None => {
                if bundle_acct.original_info.is_some() {
                    batch.delete_cf(cf_accounts, address.as_slice());
                }
                for (slot, _) in &bundle_acct.storage {
                    batch.delete_cf(cf_storage, storage_key(address, slot));
                }
            }
        }
    }
    Ok(())
}

/// Apply `bundle` to the keccak-ordered hashed-state mirror (`CF_HASHED_*`) into `batch`.
///
/// Applies EIP-161 (accounts that end empty are dropped + their hashed storage wiped) so
/// `CF_HASHED_*` is the EIP-161-clean post-state that backs [`incremental_evm_root`]'s base
/// cursors. Keeping this base clean — independent of whether `CF_ACCOUNTS` retains empties — is
/// what makes the incremental root byte-identical to the full-scan oracle for every block.
pub fn apply_bundle_hashed(
    db: &StateDb,
    batch: &mut WriteBatch,
    bundle: &BundleState,
) -> Result<(), StateError> {
    let cf_acc = db.cf_handle(CF_HASHED_ACCOUNTS)?;
    let cf_stor = db.cf_handle(CF_HASHED_STORAGE)?;
    for (address, bundle_acct) in &bundle.state {
        let hashed_address = keccak256(address.as_slice());
        let keep = match &bundle_acct.info {
            Some(info) if !is_empty_account(info) => {
                batch.put_cf(cf_acc, hashed_address.as_slice(), encode_account_info(info));
                true
            }
            _ => {
                // None or EIP-161 empty: account removed from the trie.
                batch.delete_cf(cf_acc, hashed_address.as_slice());
                false
            }
        };
        if keep {
            for (slot, slot_val) in &bundle_acct.storage {
                let hashed_slot = keccak256(slot.to_be_bytes::<32>());
                let key = hashed_storage_key(&hashed_address, &hashed_slot);
                if slot_val.present_value.is_zero() {
                    batch.delete_cf(cf_stor, key);
                } else {
                    batch.put_cf(cf_stor, key, slot_val.present_value.to_be_bytes::<32>());
                }
            }
        } else {
            // Wipe every hashed storage entry under this account's 32-byte prefix.
            let mut iter = db.inner().raw_iterator_cf(cf_stor);
            iter.seek(hashed_address.as_slice());
            while iter.valid() {
                let k = match iter.key() {
                    Some(k) if k.starts_with(hashed_address.as_slice()) => k.to_vec(),
                    _ => break,
                };
                batch.delete_cf(cf_stor, &k);
                iter.next();
            }
            iter.status()?;
        }
    }
    Ok(())
}

/// Commit `bundle` and its incremental trie/hashed updates to the DB in ONE atomic `WriteBatch`,
/// returning the new EVM state root.
///
/// Crash-consistent: plain state (`CF_ACCOUNTS`/`CF_STORAGE`), the hashed mirror (`CF_HASHED_*`),
/// and the trie nodes (`CF_TRIE_*`) all land together or not at all. The root + `TrieUpdates` are
/// computed over the committed base BEFORE the batch is written (no mid-commit mutation).
pub fn commit_evm_bundle_incremental(
    db: &StateDb,
    bundle: &BundleState,
) -> Result<B256, StateError> {
    let (root, trie_updates) = incremental_evm_root(db, bundle)?;
    let mut batch = WriteBatch::default();
    apply_bundle_plain(db, &mut batch, bundle)?;
    apply_bundle_hashed(db, &mut batch, bundle)?;
    write_trie_updates(db, &mut batch, &trie_updates)?;
    db.write(batch)?;
    Ok(root)
}

#[cfg(test)]
mod tests {
    use super::{
        build_trie_to_cf, commit_evm_bundle_incremental, full_post_bundle_evm_root,
        incremental_evm_root,
    };
    use crate::cf::CF_TRIE_ACCOUNTS;
    use crate::db::{StateDb, KECCAK_EMPTY};
    use crate::trie::compute_state_root_from_db;
    use alloy_primitives::{address, keccak256, Address, U256};
    use revm::database::BundleState;
    use revm::primitives::StorageKeyMap;
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

    /// The EVM determinism gate: the incremental post-bundle root must be byte-identical to the
    /// full-scan oracle for every bundle shape (new / changed / deleted accounts, storage
    /// insert/delete, EIP-161 clearing).
    #[test]
    fn incremental_evm_root_equals_full_scan() {
        let (db, _dir) = temp_db();
        seed(&db);
        build_trie_to_cf(&db).expect("migration");

        let eoa1 = address!("0000000000000000000000000000000000000001");
        let contract = address!("00000000000000000000000000000000000000aa");
        let new_addr = address!("00000000000000000000000000000000000000cc");

        // Matches the seed so storage-only cases leave the account info unchanged.
        let contract_info = AccountInfo {
            balance: U256::from(5u64),
            nonce: 7,
            code_hash: keccak256([1u8, 2, 3]),
            account_id: None,
            code: None,
        };

        let storage_map = |entries: &[(u64, u64, u64)]| -> StorageKeyMap<(U256, U256)> {
            let mut m = StorageKeyMap::default();
            for &(slot, orig, present) in entries {
                m.insert(U256::from(slot), (U256::from(orig), U256::from(present)));
            }
            m
        };

        let cases: Vec<(&str, BundleState)> = vec![
            ("empty", BundleState::builder(0..=0).build()),
            (
                "balance_nonce_change",
                BundleState::builder(0..=0).state_present_account_info(eoa1, eoa(424_242, 9)).build(),
            ),
            (
                "new_account",
                BundleState::builder(0..=0).state_present_account_info(new_addr, eoa(7_777, 3)).build(),
            ),
            (
                "storage_insert",
                BundleState::builder(0..=0)
                    .state_present_account_info(contract, contract_info.clone())
                    .state_storage(contract, storage_map(&[(2, 0, 555)]))
                    .build(),
            ),
            (
                "storage_delete",
                BundleState::builder(0..=0)
                    .state_present_account_info(contract, contract_info.clone())
                    .state_storage(contract, storage_map(&[(0, 42, 0)]))
                    .build(),
            ),
            (
                "account_delete",
                BundleState::builder(0..=0).state_original_account_info(eoa1, eoa(100, 1)).build(),
            ),
            (
                "eip161_new_empty",
                BundleState::builder(0..=0).state_present_account_info(new_addr, eoa(0, 0)).build(),
            ),
            (
                "eip161_drain_existing",
                BundleState::builder(0..=0).state_present_account_info(eoa1, eoa(0, 0)).build(),
            ),
            (
                "combined",
                BundleState::builder(0..=0)
                    .state_present_account_info(eoa1, eoa(5_000, 2))
                    .state_present_account_info(new_addr, eoa(123, 1))
                    .state_present_account_info(contract, contract_info.clone())
                    .state_storage(contract, storage_map(&[(0, 42, 0), (5, 0, 9)]))
                    .build(),
            ),
        ];

        for (name, bundle) in &cases {
            let (incremental, _updates) = incremental_evm_root(&db, bundle).expect("incremental");
            let oracle = full_post_bundle_evm_root(&db, bundle).expect("oracle");
            assert_eq!(
                incremental, oracle,
                "case '{name}': incremental EVM root != full-scan oracle"
            );
        }
    }

    /// Randomized corpus for the EVM determinism gate: many random bundles (mix of existing/fresh
    /// accounts, deletes, EIP-161 empties, and storage insert/delete) — each must match the
    /// full-scan oracle. Deterministic xorshift seed so failures are reproducible.
    #[test]
    fn incremental_evm_root_equals_full_scan_randomized() {
        fn xorshift(state: &mut u64) -> u64 {
            *state ^= *state << 13;
            *state ^= *state >> 7;
            *state ^= *state << 17;
            *state
        }

        let (db, _dir) = temp_db();
        seed(&db);
        build_trie_to_cf(&db).expect("migration");

        // idx < 300 hits the seed's bulk accounts (existing); idx >= 300 is fresh.
        let addr_of = |i: u32| -> Address {
            let mut b = [0u8; 20];
            b[0..4].copy_from_slice(&i.to_be_bytes());
            b[19] = 0x5a;
            Address::from(b)
        };

        let mut rng = 0x9e3779b97f4a7c15u64;
        for round in 0..40u32 {
            let mut builder = BundleState::builder(0..=0);
            let ops = (xorshift(&mut rng) % 8) + 1;
            for _ in 0..ops {
                let idx = (xorshift(&mut rng) % 320) as u32;
                let addr = addr_of(idx);
                match xorshift(&mut rng) % 5 {
                    0 => {
                        // Delete (info = None).
                        builder = builder.state_original_account_info(addr, eoa(1, 1));
                    }
                    1 => {
                        // EIP-161 empty.
                        builder = builder.state_present_account_info(addr, eoa(0, 0));
                    }
                    _ => {
                        // Change / new, optionally with storage.
                        let bal = xorshift(&mut rng) % 1_000_000;
                        let nonce = xorshift(&mut rng) % 100;
                        builder = builder.state_present_account_info(addr, eoa(bal, nonce));
                        if xorshift(&mut rng) % 2 == 0 {
                            let mut s = StorageKeyMap::default();
                            let slots = (xorshift(&mut rng) % 4) + 1;
                            for _ in 0..slots {
                                let slot = xorshift(&mut rng) % 50;
                                let present = if xorshift(&mut rng) % 3 == 0 {
                                    0
                                } else {
                                    xorshift(&mut rng) % 10_000
                                };
                                s.insert(U256::from(slot), (U256::ZERO, U256::from(present)));
                            }
                            builder = builder.state_storage(addr, s);
                        }
                    }
                }
            }
            let bundle = builder.build();
            let (incremental, _) = incremental_evm_root(&db, &bundle).expect("incremental");
            let oracle = full_post_bundle_evm_root(&db, &bundle).expect("oracle");
            assert_eq!(
                incremental, oracle,
                "randomized round {round}: incremental EVM root != full-scan oracle"
            );
        }
    }

    /// Crash-consistency: commit a bundle (plain + hashed + trie in one atomic batch), then drop &
    /// reopen the DB and confirm the root recomputes identically from the persisted CFs.
    #[test]
    fn trie_survives_crash_reopen() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().to_path_buf();

        let contract = address!("00000000000000000000000000000000000000aa");
        let eoa1 = address!("0000000000000000000000000000000000000001");
        let new_addr = address!("00000000000000000000000000000000000000cc");
        let mut storage = StorageKeyMap::default();
        storage.insert(U256::from(0u64), (U256::from(42u64), U256::ZERO)); // delete slot 0
        storage.insert(U256::from(9u64), (U256::ZERO, U256::from(77u64))); // insert slot 9
        let bundle = BundleState::builder(0..=0)
            .state_present_account_info(eoa1, eoa(424_242, 9))
            .state_present_account_info(new_addr, eoa(7_777, 1))
            .state_present_account_info(
                contract,
                AccountInfo {
                    balance: U256::from(5u64),
                    nonce: 7,
                    code_hash: keccak256([1u8, 2, 3]),
                    account_id: None,
                    code: None,
                },
            )
            .state_storage(contract, storage)
            .build();
        let empty = || BundleState::builder(0..=0).build();

        // Commit, then verify the committed trie reproduces the root before any crash.
        let root = {
            let db = StateDb::open(&path).expect("open db");
            seed(&db);
            build_trie_to_cf(&db).expect("migration");
            let root = commit_evm_bundle_incremental(&db, &bundle).expect("commit");
            assert_eq!(
                incremental_evm_root(&db, &empty()).unwrap().0,
                root,
                "post-commit incremental(empty) must equal the committed root"
            );
            assert_eq!(
                full_post_bundle_evm_root(&db, &empty()).unwrap(),
                root,
                "post-commit full-scan(empty) must equal the committed root"
            );
            root
            // `db` dropped here — simulates process shutdown / crash.
        };

        // Reopen the same directory and confirm nothing changed.
        let db = StateDb::open(&path).expect("reopen db");
        assert_eq!(
            incremental_evm_root(&db, &empty()).unwrap().0,
            root,
            "trie root changed after drop+reopen"
        );
        assert_eq!(
            full_post_bundle_evm_root(&db, &empty()).unwrap(),
            root,
            "plain-state root changed after drop+reopen"
        );

        drop(dir);
    }

    #[test]
    fn trie_migration_empty_state_is_empty_root() {
        let (db, _dir) = temp_db();
        let oracle = compute_state_root_from_db(&db).expect("oracle root");
        let migrated = build_trie_to_cf(&db).expect("migration");
        assert_eq!(migrated, oracle, "empty-state migration must match oracle (EMPTY_ROOT_HASH)");
    }
}
