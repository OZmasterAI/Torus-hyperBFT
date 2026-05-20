use std::collections::{BTreeMap, HashSet};
use std::sync::{Arc, RwLock};

use alloy_primitives::Address;
use revm::state::AccountInfo;
use rocksdb::WriteBatch;

use crate::cf::{CF_ACCOUNTS, CF_SESSIONS};
use crate::db::{decode_account_info, encode_account_info, StateDb};
use crate::error::StateError;

pub enum AtomicWriteOp<'a> {
    Put {
        cf: &'a str,
        key: &'a [u8],
        value: &'a [u8],
    },
    Delete {
        cf: &'a str,
        key: &'a [u8],
    },
}

pub trait StateBackend: Clone + Send + Sync {
    fn get_cf_raw(&self, cf: &str, key: &[u8]) -> Result<Option<Vec<u8>>, StateError>;
    fn put_cf_raw(&self, cf: &str, key: &[u8], value: &[u8]) -> Result<(), StateError>;
    fn delete_cf_raw(&self, cf: &str, key: &[u8]) -> Result<(), StateError>;

    /// Iterate entries in a column family. `prefix: Some(p)` returns only keys
    /// starting with `p`; `None` returns all entries. Results are in sorted key order.
    fn iterate_cf(
        &self,
        cf: &str,
        prefix: Option<&[u8]>,
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>, StateError>;

    fn atomic_write(&self, ops: &[AtomicWriteOp<'_>]) -> Result<(), StateError>;

    fn get_account(&self, address: &Address) -> Result<Option<AccountInfo>, StateError> {
        match self.get_cf_raw(CF_ACCOUNTS, address.as_slice())? {
            Some(data) => Ok(Some(decode_account_info(&data)?)),
            None => Ok(None),
        }
    }

    fn put_account(&self, address: &Address, info: &AccountInfo) -> Result<(), StateError> {
        self.put_cf_raw(CF_ACCOUNTS, address.as_slice(), &encode_account_info(info))
    }

    fn get_session(
        &self,
        pubkey: &[u8; 32],
    ) -> Result<Option<torus_types::SessionData>, StateError> {
        match self.get_cf_raw(CF_SESSIONS, pubkey)? {
            Some(bytes) => {
                let data: torus_types::SessionData = serde_json::from_slice(&bytes)
                    .map_err(|e| StateError::InvalidData(e.to_string()))?;
                Ok(Some(data))
            }
            None => Ok(None),
        }
    }

    fn put_session(
        &self,
        pubkey: &[u8; 32],
        data: &torus_types::SessionData,
    ) -> Result<(), StateError> {
        let bytes =
            serde_json::to_vec(data).map_err(|e| StateError::InvalidData(e.to_string()))?;
        self.put_cf_raw(CF_SESSIONS, pubkey, &bytes)
    }

    fn delete_session(&self, pubkey: &[u8; 32]) -> Result<(), StateError> {
        self.delete_cf_raw(CF_SESSIONS, pubkey)
    }

    fn count_sessions_for_owner(&self, owner: &Address) -> Result<usize, StateError> {
        let entries = self.iterate_cf(CF_SESSIONS, None)?;
        let mut count = 0;
        for (_key, value) in &entries {
            if let Ok(data) = serde_json::from_slice::<torus_types::SessionData>(value) {
                if data.owner == *owner {
                    count += 1;
                }
            }
        }
        Ok(count)
    }
}

// ============================================================================
// StateBackend for StateDb — thin delegation to existing methods
// ============================================================================

impl StateBackend for StateDb {
    fn get_cf_raw(&self, cf: &str, key: &[u8]) -> Result<Option<Vec<u8>>, StateError> {
        StateDb::get_cf_raw(self, cf, key)
    }

    fn put_cf_raw(&self, cf: &str, key: &[u8], value: &[u8]) -> Result<(), StateError> {
        StateDb::put_cf_raw(self, cf, key, value)
    }

    fn delete_cf_raw(&self, cf: &str, key: &[u8]) -> Result<(), StateError> {
        StateDb::delete_cf_raw(self, cf, key)
    }

    fn iterate_cf(
        &self,
        cf: &str,
        prefix: Option<&[u8]>,
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>, StateError> {
        let db = self.inner();
        let cf_handle = db
            .cf_handle(cf)
            .ok_or_else(|| StateError::MissingColumnFamily(cf.to_string()))?;
        let mut results = Vec::new();
        match prefix {
            Some(pfx) => {
                let iter = db.prefix_iterator_cf(cf_handle, pfx);
                for item in iter {
                    let (key, value) = item?;
                    if !key.starts_with(pfx) {
                        break;
                    }
                    results.push((key.to_vec(), value.to_vec()));
                }
            }
            None => {
                let iter = db.iterator_cf(cf_handle, rocksdb::IteratorMode::Start);
                for item in iter {
                    let (key, value) = item?;
                    results.push((key.to_vec(), value.to_vec()));
                }
            }
        }
        Ok(results)
    }

    fn atomic_write(&self, ops: &[AtomicWriteOp<'_>]) -> Result<(), StateError> {
        let mut batch = WriteBatch::default();
        let db = self.inner();
        for op in ops {
            match op {
                AtomicWriteOp::Put { cf, key, value } => {
                    let h = db
                        .cf_handle(cf)
                        .ok_or_else(|| StateError::MissingColumnFamily(cf.to_string()))?;
                    batch.put_cf(h, key, value);
                }
                AtomicWriteOp::Delete { cf, key } => {
                    let h = db
                        .cf_handle(cf)
                        .ok_or_else(|| StateError::MissingColumnFamily(cf.to_string()))?;
                    batch.delete_cf(h, key);
                }
            }
        }
        self.write(batch)
    }

    fn get_account(&self, address: &Address) -> Result<Option<AccountInfo>, StateError> {
        StateDb::get_account(self, address)
    }

    fn put_account(&self, address: &Address, info: &AccountInfo) -> Result<(), StateError> {
        StateDb::put_account(self, address, info)
    }

    fn get_session(
        &self,
        pubkey: &[u8; 32],
    ) -> Result<Option<torus_types::SessionData>, StateError> {
        StateDb::get_session(self, pubkey)
    }

    fn put_session(
        &self,
        pubkey: &[u8; 32],
        data: &torus_types::SessionData,
    ) -> Result<(), StateError> {
        StateDb::put_session(self, pubkey, data)
    }

    fn delete_session(&self, pubkey: &[u8; 32]) -> Result<(), StateError> {
        StateDb::delete_session(self, pubkey)
    }

    fn count_sessions_for_owner(&self, owner: &Address) -> Result<usize, StateError> {
        StateDb::count_sessions_for_owner(self, owner)
    }
}

// ============================================================================
// NativeStateOverlay — in-memory pending map + RocksDB fallthrough
// ============================================================================

struct PendingState {
    writes: BTreeMap<(String, Vec<u8>), Vec<u8>>,
    deletes: HashSet<(String, Vec<u8>)>,
}

#[derive(Clone)]
pub struct NativeStateOverlay {
    db: StateDb,
    pending: Arc<RwLock<PendingState>>,
}

impl NativeStateOverlay {
    pub fn new(db: StateDb) -> Self {
        Self {
            db,
            pending: Arc::new(RwLock::new(PendingState {
                writes: BTreeMap::new(),
                deletes: HashSet::new(),
            })),
        }
    }

    pub fn flush(&self, target: &StateDb) -> Result<(), StateError> {
        let state = self.pending.read().unwrap();
        let db = target.inner();
        let mut batch = WriteBatch::default();
        for ((cf_name, key), value) in &state.writes {
            let cf = db
                .cf_handle(cf_name)
                .ok_or_else(|| StateError::MissingColumnFamily(cf_name.clone()))?;
            batch.put_cf(cf, key, value);
        }
        for (cf_name, key) in &state.deletes {
            if let Some(cf) = db.cf_handle(cf_name) {
                batch.delete_cf(cf, key);
            }
        }
        target.write(batch)
    }

    /// Pre-populate CF_ACCOUNTS from an EVM BundleState so that native execution
    /// (specifically Lockbox) can read EVM account changes from this block.
    pub fn seed_from_bundle(&self, bundle: &revm::database::BundleState) {
        use crate::cf::CF_ACCOUNTS;
        use crate::db::encode_account_info;
        for (address, bundle_acct) in &bundle.state {
            match &bundle_acct.info {
                Some(info) => {
                    let _ = StateBackend::put_cf_raw(
                        self,
                        CF_ACCOUNTS,
                        address.as_slice(),
                        &encode_account_info(info),
                    );
                }
                None => {
                    if bundle_acct.original_info.is_some() {
                        let _ =
                            StateBackend::delete_cf_raw(self, CF_ACCOUNTS, address.as_slice());
                    }
                }
            }
        }
    }

    pub fn db(&self) -> &StateDb {
        &self.db
    }

    pub fn pending_write_count(&self) -> usize {
        let state = self.pending.read().unwrap();
        state.writes.len() + state.deletes.len()
    }
}

impl StateBackend for NativeStateOverlay {
    fn get_cf_raw(&self, cf: &str, key: &[u8]) -> Result<Option<Vec<u8>>, StateError> {
        let state = self.pending.read().unwrap();
        let map_key = (cf.to_string(), key.to_vec());
        if let Some(value) = state.writes.get(&map_key) {
            return Ok(Some(value.clone()));
        }
        if state.deletes.contains(&map_key) {
            return Ok(None);
        }
        drop(state);
        self.db.get_cf_raw(cf, key)
    }

    fn put_cf_raw(&self, cf: &str, key: &[u8], value: &[u8]) -> Result<(), StateError> {
        let mut state = self.pending.write().unwrap();
        let map_key = (cf.to_string(), key.to_vec());
        state.deletes.remove(&map_key);
        state.writes.insert(map_key, value.to_vec());
        Ok(())
    }

    fn delete_cf_raw(&self, cf: &str, key: &[u8]) -> Result<(), StateError> {
        let mut state = self.pending.write().unwrap();
        let map_key = (cf.to_string(), key.to_vec());
        state.writes.remove(&map_key);
        state.deletes.insert(map_key);
        Ok(())
    }

    fn iterate_cf(
        &self,
        cf: &str,
        prefix: Option<&[u8]>,
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>, StateError> {
        let state = self.pending.read().unwrap();
        let cf_str = cf.to_string();

        // Collect pending writes matching this CF+prefix
        let pending_entries: BTreeMap<Vec<u8>, Vec<u8>> = state
            .writes
            .range((cf_str.clone(), Vec::new())..)
            .take_while(|((c, _), _)| c == &cf_str)
            .filter(|((_, k), _)| prefix.map_or(true, |p| k.starts_with(p)))
            .map(|((_, k), v)| (k.clone(), v.clone()))
            .collect();

        let tombstones: HashSet<Vec<u8>> = state
            .deletes
            .iter()
            .filter(|(c, k)| c == &cf_str && prefix.map_or(true, |p| k.starts_with(p)))
            .map(|(_, k)| k.clone())
            .collect();
        drop(state);

        // Merge RocksDB entries with pending: RocksDB first, pending overrides
        let db_entries = StateBackend::iterate_cf(&self.db, cf, prefix)?;
        let mut merged: BTreeMap<Vec<u8>, Vec<u8>> = BTreeMap::new();
        for (key, value) in db_entries {
            if !tombstones.contains(&key) {
                merged.insert(key, value);
            }
        }
        for (key, value) in pending_entries {
            merged.insert(key, value);
        }
        Ok(merged.into_iter().collect())
    }

    fn atomic_write(&self, ops: &[AtomicWriteOp<'_>]) -> Result<(), StateError> {
        let mut state = self.pending.write().unwrap();
        for op in ops {
            match op {
                AtomicWriteOp::Put { cf, key, value } => {
                    let map_key = (cf.to_string(), key.to_vec());
                    state.deletes.remove(&map_key);
                    state.writes.insert(map_key, value.to_vec());
                }
                AtomicWriteOp::Delete { cf, key } => {
                    let map_key = (cf.to_string(), key.to_vec());
                    state.writes.remove(&map_key);
                    state.deletes.insert(map_key);
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cf::CF_NATIVE_BALANCES;

    fn temp_db() -> (StateDb, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("create tempdir");
        let db = StateDb::open(dir.path()).expect("open db");
        (db, dir)
    }

    #[test]
    fn statedb_backend_roundtrip() {
        let (db, _dir) = temp_db();
        let cf = CF_NATIVE_BALANCES;
        let key = b"test_key";
        let value = b"test_value";

        assert!(StateBackend::get_cf_raw(&db, cf, key).unwrap().is_none());
        StateBackend::put_cf_raw(&db, cf, key, value).unwrap();
        assert_eq!(
            StateBackend::get_cf_raw(&db, cf, key).unwrap().unwrap(),
            value
        );
        StateBackend::delete_cf_raw(&db, cf, key).unwrap();
        assert!(StateBackend::get_cf_raw(&db, cf, key).unwrap().is_none());
    }

    #[test]
    fn statedb_iterate_cf_prefix() {
        let (db, _dir) = temp_db();
        let cf = CF_NATIVE_BALANCES;
        StateBackend::put_cf_raw(&db, cf, b"aa1", b"v1").unwrap();
        StateBackend::put_cf_raw(&db, cf, b"aa2", b"v2").unwrap();
        StateBackend::put_cf_raw(&db, cf, b"bb1", b"v3").unwrap();

        let all = StateBackend::iterate_cf(&db, cf, None).unwrap();
        assert_eq!(all.len(), 3);

        let aa = StateBackend::iterate_cf(&db, cf, Some(b"aa")).unwrap();
        assert_eq!(aa.len(), 2);
        assert_eq!(aa[0].0, b"aa1");
        assert_eq!(aa[1].0, b"aa2");
    }

    #[test]
    fn statedb_atomic_write() {
        let (db, _dir) = temp_db();
        let cf = CF_NATIVE_BALANCES;
        StateBackend::put_cf_raw(&db, cf, b"to_delete", b"val").unwrap();

        StateBackend::atomic_write(
            &db,
            &[
                AtomicWriteOp::Put {
                    cf,
                    key: b"new_key",
                    value: b"new_val",
                },
                AtomicWriteOp::Delete {
                    cf,
                    key: b"to_delete",
                },
            ],
        )
        .unwrap();

        assert_eq!(
            StateBackend::get_cf_raw(&db, cf, b"new_key")
                .unwrap()
                .unwrap(),
            b"new_val"
        );
        assert!(StateBackend::get_cf_raw(&db, cf, b"to_delete")
            .unwrap()
            .is_none());
    }

    #[test]
    fn overlay_pending_overrides_db() {
        let (db, _dir) = temp_db();
        let cf = CF_NATIVE_BALANCES;
        StateBackend::put_cf_raw(&db, cf, b"key1", b"db_val").unwrap();

        let overlay = NativeStateOverlay::new(db);
        assert_eq!(
            StateBackend::get_cf_raw(&overlay, cf, b"key1")
                .unwrap()
                .unwrap(),
            b"db_val"
        );

        StateBackend::put_cf_raw(&overlay, cf, b"key1", b"overlay_val").unwrap();
        assert_eq!(
            StateBackend::get_cf_raw(&overlay, cf, b"key1")
                .unwrap()
                .unwrap(),
            b"overlay_val"
        );
    }

    #[test]
    fn overlay_tombstone_hides_db() {
        let (db, _dir) = temp_db();
        let cf = CF_NATIVE_BALANCES;
        StateBackend::put_cf_raw(&db, cf, b"key1", b"val").unwrap();

        let overlay = NativeStateOverlay::new(db);
        StateBackend::delete_cf_raw(&overlay, cf, b"key1").unwrap();
        assert!(StateBackend::get_cf_raw(&overlay, cf, b"key1")
            .unwrap()
            .is_none());
    }

    #[test]
    fn overlay_iterate_merges_correctly() {
        let (db, _dir) = temp_db();
        let cf = CF_NATIVE_BALANCES;
        StateBackend::put_cf_raw(&db, cf, b"a1", b"db_a1").unwrap();
        StateBackend::put_cf_raw(&db, cf, b"a2", b"db_a2").unwrap();
        StateBackend::put_cf_raw(&db, cf, b"a3", b"db_a3").unwrap();

        let overlay = NativeStateOverlay::new(db);
        // Override a2, delete a3, add a4
        StateBackend::put_cf_raw(&overlay, cf, b"a2", b"ov_a2").unwrap();
        StateBackend::delete_cf_raw(&overlay, cf, b"a3").unwrap();
        StateBackend::put_cf_raw(&overlay, cf, b"a4", b"ov_a4").unwrap();

        let entries = StateBackend::iterate_cf(&overlay, cf, Some(b"a")).unwrap();
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0], (b"a1".to_vec(), b"db_a1".to_vec()));
        assert_eq!(entries[1], (b"a2".to_vec(), b"ov_a2".to_vec()));
        assert_eq!(entries[2], (b"a4".to_vec(), b"ov_a4".to_vec()));
    }

    #[test]
    fn overlay_flush_applies_to_target() {
        let (db, _dir) = temp_db();
        let cf = CF_NATIVE_BALANCES;
        StateBackend::put_cf_raw(&db, cf, b"existing", b"old").unwrap();

        let overlay = NativeStateOverlay::new(db.clone());
        StateBackend::put_cf_raw(&overlay, cf, b"existing", b"new").unwrap();
        StateBackend::put_cf_raw(&overlay, cf, b"added", b"fresh").unwrap();
        StateBackend::delete_cf_raw(&overlay, cf, b"existing").unwrap();

        // Before flush: db still has old value
        assert_eq!(
            StateDb::get_cf_raw(&db, cf, b"existing").unwrap().unwrap(),
            b"old"
        );

        overlay.flush(&db).unwrap();

        assert!(StateDb::get_cf_raw(&db, cf, b"existing").unwrap().is_none());
        assert_eq!(
            StateDb::get_cf_raw(&db, cf, b"added").unwrap().unwrap(),
            b"fresh"
        );
    }

    #[test]
    fn overlay_clone_shares_state() {
        let (db, _dir) = temp_db();
        let cf = CF_NATIVE_BALANCES;
        let overlay = NativeStateOverlay::new(db);
        let clone = overlay.clone();

        StateBackend::put_cf_raw(&overlay, cf, b"k", b"v").unwrap();
        assert_eq!(
            StateBackend::get_cf_raw(&clone, cf, b"k").unwrap().unwrap(),
            b"v"
        );
    }

    #[test]
    fn overlay_account_roundtrip() {
        let (db, _dir) = temp_db();
        let overlay = NativeStateOverlay::new(db);
        let addr = Address::from([0xABu8; 20]);
        let info = AccountInfo {
            balance: alloy_primitives::U256::from(1000u64),
            nonce: 5,
            code_hash: alloy_primitives::B256::ZERO,
            code: None,
            account_id: None,
        };

        StateBackend::put_account(&overlay, &addr, &info).unwrap();
        let loaded = StateBackend::get_account(&overlay, &addr).unwrap().unwrap();
        assert_eq!(loaded.balance, info.balance);
        assert_eq!(loaded.nonce, info.nonce);
    }
}
