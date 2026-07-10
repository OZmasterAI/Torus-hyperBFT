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

/// One recorded native-write mutation plus the prior state needed to undo it — an
/// entry in [`NativeStateOverlay`]'s call-frame undo trail (T4.4 revert-safety).
struct JournalEntry {
    map_key: (String, Vec<u8>),
    /// Value previously held in `writes` for `map_key` (`None` if it was absent).
    prev_write: Option<Vec<u8>>,
    /// Whether `map_key` was previously tombstoned in `deletes`.
    prev_deleted: bool,
}

struct PendingState {
    writes: BTreeMap<(String, Vec<u8>), Vec<u8>>,
    deletes: HashSet<(String, Vec<u8>)>,
    /// T4.4: append-only undo trail for writer-precompile side effects. `checkpoints`
    /// holds savepoint lengths into this log so a call frame that reverts can roll its
    /// native writes back (see [`NativeStateOverlay::checkpoint`]).
    journal_log: Vec<JournalEntry>,
    checkpoints: Vec<usize>,
}

impl PendingState {
    /// Record the pre-mutation state of `map_key` so any open checkpoint can undo it.
    /// No-op when no checkpoint is active — tx-scope writes made outside any EVM call
    /// frame are reverted wholesale via [`NativeStateOverlay::discard_tx`], so they need
    /// no per-mutation trail (and we avoid growing the log on that path).
    fn record(&mut self, map_key: &(String, Vec<u8>)) {
        if self.checkpoints.is_empty() {
            return;
        }
        self.journal_log.push(JournalEntry {
            map_key: map_key.clone(),
            prev_write: self.writes.get(map_key).cloned(),
            prev_deleted: self.deletes.contains(map_key),
        });
    }
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
                journal_log: Vec::new(),
                checkpoints: Vec::new(),
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

    /// T4.4: Persist all pending writes to `target` in one atomic batch, then clear the
    /// journal. This is the per-transaction COMMIT for writer-precompile side effects —
    /// the EVM executor calls it only when the calling tx succeeded. A no-op when the
    /// journal is empty, so read-only paths never touch the database.
    pub fn commit_tx(&self, target: &StateDb) -> Result<(), StateError> {
        let mut state = self.pending.write().unwrap();
        if state.writes.is_empty() && state.deletes.is_empty() {
            // Per-tx reset even on the empty path: the frame checkpoint stack must not
            // leak across transactions (see `checkpoint`).
            state.journal_log.clear();
            state.checkpoints.clear();
            return Ok(());
        }
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
        target.write(batch)?;
        state.writes.clear();
        state.deletes.clear();
        state.journal_log.clear();
        state.checkpoints.clear();
        Ok(())
    }

    /// T4.4: Drop all pending writes without persisting them — the per-transaction
    /// REVERT for writer-precompile side effects (calling EVM tx reverted or halted).
    pub fn discard_tx(&self) {
        let mut state = self.pending.write().unwrap();
        state.writes.clear();
        state.deletes.clear();
        state.journal_log.clear();
        state.checkpoints.clear();
    }

    /// T4.4 revert-safety: open a call-frame checkpoint over the writer-precompile journal.
    ///
    /// The EVM executor drives one checkpoint per revm CALL/CREATE frame (via an
    /// inspector): a checkpoint is opened on frame entry and, on frame exit, either
    /// [`commit_checkpoint`](Self::commit_checkpoint) (the frame succeeded — its native
    /// writes flow up to the parent) or [`revert_to_checkpoint`](Self::revert_to_checkpoint)
    /// (the frame reverted/halted — its native writes are dropped). This makes a native
    /// side effect enqueued inside a CAUGHT inner-frame revert (e.g. a Solidity try/catch
    /// around a sub-call) roll back even when the top-level transaction succeeds.
    pub fn checkpoint(&self) {
        let mut state = self.pending.write().unwrap();
        let len = state.journal_log.len();
        state.checkpoints.push(len);
    }

    /// T4.4: close the innermost checkpoint, keeping its native writes (they merge into the
    /// enclosing frame's scope). When no checkpoint remains the undo trail is dead weight,
    /// so it is cleared to bound memory — the surviving writes live in `writes`/`deletes`.
    pub fn commit_checkpoint(&self) {
        let mut state = self.pending.write().unwrap();
        state.checkpoints.pop();
        if state.checkpoints.is_empty() {
            state.journal_log.clear();
        }
    }

    /// T4.4: roll the journal back to the innermost checkpoint, undoing every native write
    /// made since it was opened (including inner frames that had committed up into it). A
    /// no-op if no checkpoint is open.
    pub fn revert_to_checkpoint(&self) {
        let mut state = self.pending.write().unwrap();
        let Some(target) = state.checkpoints.pop() else {
            return;
        };
        while state.journal_log.len() > target {
            let entry = state.journal_log.pop().unwrap();
            match entry.prev_write {
                Some(v) => {
                    state.writes.insert(entry.map_key.clone(), v);
                }
                None => {
                    state.writes.remove(&entry.map_key);
                }
            }
            if entry.prev_deleted {
                state.deletes.insert(entry.map_key);
            } else {
                state.deletes.remove(&entry.map_key);
            }
        }
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
        self.flush_with_native_trie_inner(target, None)
    }

    /// Like [`flush_with_native_trie`], but additionally folds the native applied-height marker
    /// (`CF_CONSENSUS_META` / `META_NATIVE_APPLIED_HEIGHT`, big-endian `u64`) into the SAME atomic
    /// `WriteBatch` as the native-CF writes and the trie/mirror updates.
    ///
    /// Crash-safety (T156-F1): previously `execute_committed_block` flushed native state and THEN
    /// wrote the applied-height marker in a separate, error-swallowing call. A hard crash in that
    /// window left the block's native state durable but the height still marked un-applied, so a
    /// restart re-executed the block and double-applied non-nonce-guarded effects (fee distribution,
    /// epoch rewards). Folding the marker into this batch makes native state and its "this height is
    /// applied" marker commit together or not at all.
    ///
    /// The marker is encoded byte-for-byte identically to the old `write_native_applied_height`
    /// (`META_NATIVE_APPLIED_HEIGHT` -> `applied_height.to_be_bytes()`).
    pub fn flush_with_native_trie_and_marker(
        &self,
        target: &StateDb,
        applied_height: u64,
    ) -> Result<(), StateError> {
        self.flush_with_native_trie_inner(target, Some(applied_height))
    }

    /// Shared implementation for [`flush_with_native_trie`] and
    /// [`flush_with_native_trie_and_marker`]. When `applied_height` is `Some(h)`, the native
    /// applied-height marker is appended to the same batch (T156-F1).
    fn flush_with_native_trie_inner(
        &self,
        target: &StateDb,
        applied_height: Option<u64>,
    ) -> Result<(), StateError> {
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

        // T156-F1: fold the native applied-height marker into the SAME batch as the native writes,
        // so a crash can never leave native state flushed but the height still un-marked (which would
        // double-apply the block on restart replay). Encoded exactly as write_native_applied_height.
        if let Some(height) = applied_height {
            let cf = raw.cf_handle(crate::cf::CF_CONSENSUS_META).ok_or_else(|| {
                StateError::MissingColumnFamily(crate::cf::CF_CONSENSUS_META.to_string())
            })?;
            let marker = height.to_be_bytes();
            batch.put_cf(cf, crate::cf::META_NATIVE_APPLIED_HEIGHT, marker);
        }

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
        state.record(&map_key);
        state.deletes.remove(&map_key);
        state.writes.insert(map_key, value.to_vec());
        Ok(())
    }

    fn delete_cf_raw(&self, cf: &str, key: &[u8]) -> Result<(), StateError> {
        let mut state = self.pending.write().unwrap();
        let map_key = (cf.to_string(), key.to_vec());
        state.record(&map_key);
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
                    state.record(&map_key);
                    state.deletes.remove(&map_key);
                    state.writes.insert(map_key, value.to_vec());
                }
                AtomicWriteOp::Delete { cf, key } => {
                    let map_key = (cf.to_string(), key.to_vec());
                    state.record(&map_key);
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

    /// T4.4: `commit_tx` persists pending writes and clears the journal so the next
    /// transaction starts from a clean buffer.
    #[test]
    fn overlay_commit_tx_persists_and_clears() {
        let (db, _dir) = temp_db();
        let cf = CF_NATIVE_BALANCES;

        let overlay = NativeStateOverlay::new(db.clone());
        StateBackend::put_cf_raw(&overlay, cf, b"k1", b"v1").unwrap();
        assert_eq!(overlay.pending_write_count(), 1);

        overlay.commit_tx(&db).unwrap();
        assert_eq!(
            StateDb::get_cf_raw(&db, cf, b"k1").unwrap().unwrap(),
            b"v1"
        );
        assert_eq!(overlay.pending_write_count(), 0, "journal cleared on commit");

        // Empty commit is a no-op (must not error).
        overlay.commit_tx(&db).unwrap();
    }

    /// T4.4: `discard_tx` drops pending writes — nothing reaches the database.
    #[test]
    fn overlay_discard_tx_drops_pending() {
        let (db, _dir) = temp_db();
        let cf = CF_NATIVE_BALANCES;

        let overlay = NativeStateOverlay::new(db.clone());
        StateBackend::put_cf_raw(&overlay, cf, b"k1", b"v1").unwrap();
        StateBackend::delete_cf_raw(&overlay, cf, b"k2").unwrap();

        overlay.discard_tx();
        assert_eq!(overlay.pending_write_count(), 0);
        assert!(StateDb::get_cf_raw(&db, cf, b"k1").unwrap().is_none());
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

    /// T156-F1 RED-first: `flush_with_native_trie_and_marker` must persist BOTH the native-CF writes
    /// AND the applied-height marker atomically (one `WriteBatch`), so a crash can never leave native
    /// state durable while the height is still marked un-applied (which double-applies on replay).
    ///
    /// RED on pre-fix code: the method did not exist — the marker was written by a separate call in
    /// `execute_committed_block` AFTER the flush returned, so this test both fails to compile pre-fix
    /// (missing symbol) and encodes the atomicity the fix guarantees. GREEN after: reading the DB
    /// finds the native balance AND the marker, and the marker is byte-for-byte the height's BE bytes.
    #[test]
    fn flush_with_native_trie_and_marker_writes_marker_atomically() {
        use crate::cf::{CF_CONSENSUS_META, CF_STAKING_VALIDATORS, META_NATIVE_APPLIED_HEIGHT};
        let (db, _dir) = temp_db();
        crate::native_trie::build_native_trie_to_cf(&db).unwrap(); // empty base

        // Before the flush the marker is absent.
        assert!(
            StateDb::get_cf_raw(&db, CF_CONSENSUS_META, META_NATIVE_APPLIED_HEIGHT)
                .unwrap()
                .is_none(),
            "marker must not exist before the flush"
        );

        let overlay = NativeStateOverlay::new(db.clone());
        StateBackend::put_cf_raw(&overlay, CF_NATIVE_BALANCES, b"\x00\x01acct", b"bal1").unwrap();
        StateBackend::put_cf_raw(&overlay, CF_STAKING_VALIDATORS, b"val1", b"stake").unwrap();

        let height: u64 = 42;
        overlay
            .flush_with_native_trie_and_marker(&db, height)
            .unwrap();

        // Native write landed.
        assert_eq!(
            StateDb::get_cf_raw(&db, CF_NATIVE_BALANCES, b"\x00\x01acct")
                .unwrap()
                .unwrap(),
            b"bal1"
        );
        // Marker landed in the SAME flush, byte-for-byte the BE height (== write_native_applied_height).
        assert_eq!(
            StateDb::get_cf_raw(&db, CF_CONSENSUS_META, META_NATIVE_APPLIED_HEIGHT)
                .unwrap()
                .unwrap(),
            height.to_be_bytes().to_vec(),
            "the applied-height marker must be flushed atomically with native state"
        );
        // The trie stayed consistent (marker put must not disturb native-root maintenance).
        assert_eq!(
            crate::native_trie::persisted_native_root(&db).unwrap(),
            crate::native_trie::native_root_full(&db).unwrap()
        );
    }

    /// T156-F1: the plain `flush_with_native_trie` (no marker) must NOT touch the applied-height
    /// marker — only the `_and_marker` variant writes it. Guards against the marker leaking into
    /// callers (chaos tests, incremental-root maintenance) that only want native state flushed.
    #[test]
    fn flush_with_native_trie_leaves_marker_untouched() {
        use crate::cf::{CF_CONSENSUS_META, META_NATIVE_APPLIED_HEIGHT};
        let (db, _dir) = temp_db();
        crate::native_trie::build_native_trie_to_cf(&db).unwrap();

        let overlay = NativeStateOverlay::new(db.clone());
        StateBackend::put_cf_raw(&overlay, CF_NATIVE_BALANCES, b"\x00\x01acct", b"bal1").unwrap();
        overlay.flush_with_native_trie(&db).unwrap();

        assert!(
            StateDb::get_cf_raw(&db, CF_CONSENSUS_META, META_NATIVE_APPLIED_HEIGHT)
                .unwrap()
                .is_none(),
            "flush_with_native_trie must not write the applied-height marker"
        );
    }

    /// T4.4 revert-safety: reverting to a checkpoint rolls native writes made in the frame
    /// back to their pre-checkpoint state (overwrites restored, fresh keys removed).
    #[test]
    fn checkpoint_revert_rolls_back_writes() {
        let (db, _dir) = temp_db();
        let cf = CF_NATIVE_BALANCES;
        let overlay = NativeStateOverlay::new(db);

        StateBackend::put_cf_raw(&overlay, cf, b"k", b"base").unwrap();
        overlay.checkpoint();
        StateBackend::put_cf_raw(&overlay, cf, b"k", b"inner").unwrap();
        StateBackend::put_cf_raw(&overlay, cf, b"new", b"x").unwrap();
        // Read-your-writes holds INSIDE the frame.
        assert_eq!(
            StateBackend::get_cf_raw(&overlay, cf, b"k").unwrap().unwrap(),
            b"inner"
        );

        overlay.revert_to_checkpoint();
        // Overwrite rolled back to the pre-checkpoint value; the fresh key is gone.
        assert_eq!(
            StateBackend::get_cf_raw(&overlay, cf, b"k").unwrap().unwrap(),
            b"base"
        );
        assert!(StateBackend::get_cf_raw(&overlay, cf, b"new")
            .unwrap()
            .is_none());
    }

    /// T4.4: committing a checkpoint keeps the frame's native writes.
    #[test]
    fn checkpoint_commit_keeps_writes() {
        let (db, _dir) = temp_db();
        let cf = CF_NATIVE_BALANCES;
        let overlay = NativeStateOverlay::new(db);

        overlay.checkpoint();
        StateBackend::put_cf_raw(&overlay, cf, b"k", b"v").unwrap();
        overlay.commit_checkpoint();

        assert_eq!(
            StateBackend::get_cf_raw(&overlay, cf, b"k").unwrap().unwrap(),
            b"v"
        );
    }

    /// T4.4: an OUTER-frame revert must undo writes an inner frame had COMMITTED up into it
    /// — the exact caught-inner-frame scenario (inner precompile-call frame returns Ok, its
    /// enqueuing outer frame then reverts).
    #[test]
    fn nested_checkpoint_outer_revert_undoes_committed_inner() {
        let (db, _dir) = temp_db();
        let cf = CF_NATIVE_BALANCES;
        let overlay = NativeStateOverlay::new(db);

        overlay.checkpoint(); // outer frame A
        StateBackend::put_cf_raw(&overlay, cf, b"a", b"1").unwrap();
        overlay.checkpoint(); // inner frame B (e.g. a precompile call)
        StateBackend::put_cf_raw(&overlay, cf, b"b", b"2").unwrap();
        overlay.commit_checkpoint(); // B returns Ok -> its write flows up to A
        assert_eq!(
            StateBackend::get_cf_raw(&overlay, cf, b"b").unwrap().unwrap(),
            b"2"
        );

        overlay.revert_to_checkpoint(); // A reverts -> BOTH a and b undone
        assert!(StateBackend::get_cf_raw(&overlay, cf, b"a")
            .unwrap()
            .is_none());
        assert!(StateBackend::get_cf_raw(&overlay, cf, b"b")
            .unwrap()
            .is_none());
    }

    /// T4.4: `discard_tx` resets the checkpoint stack so no undo trail leaks into the next tx.
    #[test]
    fn discard_tx_clears_checkpoint_state() {
        let (db, _dir) = temp_db();
        let cf = CF_NATIVE_BALANCES;
        let overlay = NativeStateOverlay::new(db);

        overlay.checkpoint();
        StateBackend::put_cf_raw(&overlay, cf, b"k", b"v").unwrap();
        overlay.discard_tx();
        assert_eq!(overlay.pending_write_count(), 0);

        // A fresh checkpoint/revert cycle must start clean (no stale entries).
        overlay.checkpoint();
        StateBackend::put_cf_raw(&overlay, cf, b"k2", b"v2").unwrap();
        overlay.revert_to_checkpoint();
        assert_eq!(overlay.pending_write_count(), 0);
    }
}
