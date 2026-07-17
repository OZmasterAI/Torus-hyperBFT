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
    #[allow(clippy::type_complexity)]
    fn iterate_cf(
        &self,
        cf: &str,
        prefix: Option<&[u8]>,
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>, StateError>;

    /// First `limit` entries of [`Self::iterate_cf`], in the same order.
    /// Default is scan-then-truncate (always correct); backends with real
    /// iterators should override to stop early (0x0800 top-N gas round).
    fn iterate_cf_bounded(
        &self,
        cf: &str,
        prefix: Option<&[u8]>,
        limit: usize,
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>, StateError> {
        let mut entries = self.iterate_cf(cf, prefix)?;
        entries.truncate(limit);
        Ok(entries)
    }

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
        let bytes = serde_json::to_vec(data).map_err(|e| StateError::InvalidData(e.to_string()))?;
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

    /// Early-stop override: touches at most `limit` entries instead of
    /// materializing the whole prefix (the point of the bounded read).
    fn iterate_cf_bounded(
        &self,
        cf: &str,
        prefix: Option<&[u8]>,
        limit: usize,
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>, StateError> {
        let db = self.inner();
        let cf_handle = db
            .cf_handle(cf)
            .ok_or_else(|| StateError::MissingColumnFamily(cf.to_string()))?;
        let mut results = Vec::with_capacity(limit.min(1024));
        if limit == 0 {
            return Ok(results);
        }
        match prefix {
            Some(pfx) => {
                let iter = db.prefix_iterator_cf(cf_handle, pfx);
                for item in iter {
                    let (key, value) = item?;
                    if !key.starts_with(pfx) {
                        break;
                    }
                    results.push((key.to_vec(), value.to_vec()));
                    if results.len() >= limit {
                        break;
                    }
                }
            }
            None => {
                let iter = db.iterator_cf(cf_handle, rocksdb::IteratorMode::Start);
                for item in iter {
                    let (key, value) = item?;
                    results.push((key.to_vec(), value.to_vec()));
                    if results.len() >= limit {
                        break;
                    }
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
                        let _ = StateBackend::delete_cf_raw(self, CF_ACCOUNTS, address.as_slice());
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

    /// Addresses whose `CF_ACCOUNTS` entry this overlay wrote or deleted.
    ///
    /// Native post-commit (fee distribution to treasury/dev_pool, validator rewards) credits EVM
    /// account *balances* through this overlay; on flush those land in `CF_ACCOUNTS` but bypass the
    /// incremental EVM trie. The consensus commit path feeds these addresses to
    /// `torus_state::incremental::resync_evm_accounts` so `CF_HASHED_*`/`CF_TRIE_*` keep tracking
    /// `CF_ACCOUNTS` (otherwise the incremental root drifts from the full scan — devnet-smoke find).
    pub fn dirty_evm_accounts(&self) -> Vec<Address> {
        let state = self.pending.read().unwrap();
        let mut addrs = Vec::new();
        for (cf_name, key) in state.writes.keys() {
            if cf_name == CF_ACCOUNTS && key.len() == 20 {
                addrs.push(Address::from_slice(key));
            }
        }
        for (cf_name, key) in &state.deletes {
            if cf_name == CF_ACCOUNTS && key.len() == 20 {
                addrs.push(Address::from_slice(key));
            }
        }
        addrs
    }

    /// The native-root dirty `(cf_tag, key) -> Option<value>` set this overlay would flush — writes
    /// (`Some`) and deletes (`None`) hitting the 6 native-root CFs only (mirrors `dirty_evm_accounts`).
    /// Fed to the bucketed native trie (A2.2) so it tracks the committed native state. Non-root CFs
    /// (nonces, governance, markets, …) are excluded — they are not part of the native root.
    pub fn dirty_native_keys(&self) -> BTreeMap<(u8, Vec<u8>), Option<Vec<u8>>> {
        let state = self.pending.read().unwrap();
        let mut dirty = BTreeMap::new();
        for ((cf_name, key), value) in &state.writes {
            if let Some(tag) = crate::native_trie::cf_tag(cf_name) {
                dirty.insert((tag, key.clone()), Some(value.clone()));
            }
        }
        for (cf_name, key) in &state.deletes {
            if let Some(tag) = crate::native_trie::cf_tag(cf_name) {
                dirty.insert((tag, key.clone()), None);
            }
        }
        dirty
    }

    /// Flush pending native writes AND maintain the bucketed native trie — the native-CF writes and
    /// the trie/mirror updates land in ONE atomic `WriteBatch` (crash-consistent: native state and
    /// its root advance together or not at all).
    ///
    /// Safety: the trie ops are computed read-only FIRST and only appended on success, and the batch
    /// is written either way — so a trie-maintenance failure can NEVER drop committed native state.
    /// On failure the error is returned AFTER the native writes are durable; the caller logs it and
    /// the (off-by-default) incremental root is merely stale for that block (repaired by replay /
    /// caught by the runtime oracle).
    pub fn flush_with_native_trie(&self, target: &StateDb) -> Result<(), StateError> {
        let state = self.pending.read().unwrap();
        let raw = target.inner();
        let mut batch = WriteBatch::default();
        for ((cf_name, key), value) in &state.writes {
            let cf = raw
                .cf_handle(cf_name)
                .ok_or_else(|| StateError::MissingColumnFamily(cf_name.clone()))?;
            batch.put_cf(cf, key, value);
        }
        for (cf_name, key) in &state.deletes {
            if let Some(cf) = raw.cf_handle(cf_name) {
                batch.delete_cf(cf, key);
            }
        }

        // Native-root dirty map, folded into the SAME batch.
        let mut dirty: BTreeMap<(u8, Vec<u8>), Option<Vec<u8>>> = BTreeMap::new();
        for ((cf_name, key), value) in &state.writes {
            if let Some(tag) = crate::native_trie::cf_tag(cf_name) {
                dirty.insert((tag, key.clone()), Some(value.clone()));
            }
        }
        for (cf_name, key) in &state.deletes {
            if let Some(tag) = crate::native_trie::cf_tag(cf_name) {
                dirty.insert((tag, key.clone()), None);
            }
        }

        // apply_native_dirty_to_batch computes all ops before appending, so on Err nothing was
        // appended and the native-CF writes still flush.
        let trie_result = if dirty.is_empty() {
            Ok(())
        } else {
            crate::native_trie::apply_native_dirty_to_batch(target, &mut batch, &dirty).map(|_| ())
        };
        target.write(batch)?;
        trie_result
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
            .filter(|((_, k), _)| prefix.is_none_or(|p| k.starts_with(p)))
            .map(|((_, k), v)| (k.clone(), v.clone()))
            .collect();

        let tombstones: HashSet<Vec<u8>> = state
            .deletes
            .iter()
            .filter(|(c, k)| c == &cf_str && prefix.is_none_or(|p| k.starts_with(p)))
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

    /// A2.2: flushing through `flush_with_native_trie` commits native CFs AND keeps the bucketed
    /// native root equal to the full scan, in one atomic batch. Non-root CFs (nonces) are excluded.
    #[test]
    fn flush_with_native_trie_maintains_root() {
        use crate::cf::{CF_NATIVE_NONCES, CF_STAKING_VALIDATORS};
        let (db, _dir) = temp_db();
        crate::native_trie::build_native_trie_to_cf(&db).unwrap(); // empty base

        let overlay = NativeStateOverlay::new(db.clone());
        StateBackend::put_cf_raw(&overlay, CF_NATIVE_BALANCES, b"\x00\x01acct", b"bal1").unwrap();
        StateBackend::put_cf_raw(&overlay, CF_STAKING_VALIDATORS, b"val1", b"stake").unwrap();
        StateBackend::put_cf_raw(&overlay, CF_NATIVE_NONCES, b"nonce", b"x").unwrap(); // non-root

        overlay.flush_with_native_trie(&db).unwrap();

        assert_eq!(
            StateDb::get_cf_raw(&db, CF_NATIVE_BALANCES, b"\x00\x01acct")
                .unwrap()
                .unwrap(),
            b"bal1"
        );
        let persisted = crate::native_trie::persisted_native_root(&db).unwrap();
        assert_eq!(
            persisted,
            crate::native_trie::native_root_full(&db).unwrap()
        );
        assert_ne!(persisted, crate::trie::EMPTY_ROOT_HASH);
    }
}
