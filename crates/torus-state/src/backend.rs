use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, OnceLock, RwLock};

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

/// C2 (perf): interned column-family id — an index into [`crate::cf::ALL_CF_NAMES`].
///
/// INTERNAL representation only. The overlay's public API still speaks `&str` CF
/// names and `&[u8]` keys, and every flush resolves the id back to the exact
/// `&'static str` it was interned from, so the byte-level RocksDB keys (and hence
/// every hash preimage over them) are identical to the pre-interning
/// `(String, Vec<u8>)` composite-key map — proven by
/// `interned_overlay_stores_identical_rocksdb_keys` below.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
struct CfId(u8);

/// Number of registered CFs — one overlay bucket per CF.
const NUM_CFS: usize = crate::cf::ALL_CF_NAMES.len();

impl CfId {
    #[inline]
    fn name(self) -> &'static str {
        crate::cf::ALL_CF_NAMES[self.0 as usize]
    }
}

/// Intern a CF name to its [`CfId`]. `None` for a name outside
/// [`crate::cf::ALL_CF_NAMES`] — `StateDb::open` registers exactly that list, so
/// such a CF cannot exist in any target database and no overlay entry for it
/// could ever flush.
#[inline]
fn intern_cf(name: &str) -> Option<CfId> {
    static TABLE: OnceLock<HashMap<&'static str, u8>> = OnceLock::new();
    let table = TABLE.get_or_init(|| {
        crate::cf::ALL_CF_NAMES
            .iter()
            .enumerate()
            .map(|(i, n)| (*n, i as u8))
            .collect()
    });
    table.get(name).map(|&i| CfId(i))
}

/// One recorded native-write mutation plus the prior state needed to undo it — an
/// entry in [`NativeStateOverlay`]'s call-frame undo trail (T4.4 revert-safety).
struct JournalEntry {
    cf: CfId,
    key: Vec<u8>,
    /// Value previously held in `writes` for the key (`None` if it was absent).
    prev_write: Option<Vec<u8>>,
    /// Whether the key was previously tombstoned in `deletes`.
    prev_deleted: bool,
}

/// Per-CF pending mutations. Keys are plain `Vec<u8>` — no `(String, Vec<u8>)`
/// composite allocation per get/put, and lookups borrow the caller's `&[u8]`
/// directly (zero allocations on the read path).
#[derive(Default)]
struct CfPending {
    writes: BTreeMap<Vec<u8>, Vec<u8>>,
    /// `BTreeSet` (was a `HashSet` over composite keys) so delete flush order is
    /// deterministic; final state never depended on it (put/delete keys are
    /// disjoint by invariant), this just removes the last order wobble.
    deletes: BTreeSet<Vec<u8>>,
}

impl CfPending {
    fn is_empty(&self) -> bool {
        self.writes.is_empty() && self.deletes.is_empty()
    }
}

struct PendingState {
    /// Indexed by [`CfId`] — one bucket per registered CF.
    cfs: [CfPending; NUM_CFS],
    /// T4.4: append-only undo trail for writer-precompile side effects. `checkpoints`
    /// holds savepoint lengths into this log so a call frame that reverts can roll its
    /// native writes back (see [`NativeStateOverlay::checkpoint`]).
    journal_log: Vec<JournalEntry>,
    checkpoints: Vec<usize>,
    /// bl2 exec pipeline: set by [`NativeStateOverlay::freeze`] after the block's
    /// pending set was moved out into a [`FrozenPending`]. Every later write
    /// through this overlay (or any clone sharing the Arc) is an error — the
    /// block is closed and its writes are owned by the flush worker.
    frozen: bool,
}

impl PendingState {
    fn new() -> Self {
        Self {
            cfs: std::array::from_fn(|_| CfPending::default()),
            journal_log: Vec::new(),
            checkpoints: Vec::new(),
            frozen: false,
        }
    }

    /// Layered point lookup: `Some(Some(v))` = pending write, `Some(None)` =
    /// tombstone, `None` = not in this layer (fall through).
    #[inline]
    fn lookup(&self, id: CfId, key: &[u8]) -> Option<Option<&[u8]>> {
        let cfp = self.cf(id);
        if let Some(value) = cfp.writes.get(key) {
            return Some(Some(value.as_slice()));
        }
        if cfp.deletes.contains(key) {
            return Some(None);
        }
        None
    }

    /// Apply this layer's writes/tombstones (under `prefix`) on top of `merged`.
    fn overlay_into(
        &self,
        id: CfId,
        prefix: Option<&[u8]>,
        merged: &mut BTreeMap<Vec<u8>, Vec<u8>>,
    ) {
        let cfp = self.cf(id);
        for key in cfp
            .deletes
            .iter()
            .filter(|k| prefix.is_none_or(|p| k.starts_with(p)))
        {
            merged.remove(key);
        }
        for (key, value) in cfp
            .writes
            .iter()
            .filter(|(k, _)| prefix.is_none_or(|p| k.starts_with(p)))
        {
            merged.insert(key.clone(), value.clone());
        }
    }

    fn frozen_err() -> StateError {
        StateError::InvalidData(
            "native overlay is frozen (block handed to the flush worker); no further writes".into(),
        )
    }

    #[inline]
    fn cf(&self, id: CfId) -> &CfPending {
        &self.cfs[id.0 as usize]
    }

    #[inline]
    fn cf_mut(&mut self, id: CfId) -> &mut CfPending {
        &mut self.cfs[id.0 as usize]
    }

    fn is_empty(&self) -> bool {
        self.cfs.iter().all(CfPending::is_empty)
    }

    fn clear(&mut self) {
        for cfp in &mut self.cfs {
            cfp.writes.clear();
            cfp.deletes.clear();
        }
        self.journal_log.clear();
        self.checkpoints.clear();
    }

    /// Record the pre-mutation state of `(cf, key)` so any open checkpoint can undo it.
    /// No-op when no checkpoint is active — tx-scope writes made outside any EVM call
    /// frame are reverted wholesale via [`NativeStateOverlay::discard_tx`], so they need
    /// no per-mutation trail (and we avoid growing the log on that path).
    fn record(&mut self, cf: CfId, key: &[u8]) {
        if self.checkpoints.is_empty() {
            return;
        }
        let cfp = self.cf(cf);
        let entry = JournalEntry {
            cf,
            key: key.to_vec(),
            prev_write: cfp.writes.get(key).cloned(),
            prev_deleted: cfp.deletes.contains(key),
        };
        self.journal_log.push(entry);
    }

    /// Append every pending write and delete to `batch`, resolving each CF handle
    /// ONCE per CF (was once per key). Byte-identical stored keys/values to the
    /// pre-C2 composite-key loop; CF order is `ALL_CF_NAMES` order and keys are
    /// sorted within a CF — put/delete keys are disjoint (put removes the
    /// tombstone, delete removes the write), so batch entry order cannot change
    /// the final state.
    ///
    /// Semantics preserved from pre-C2: a missing CF errors on the WRITE path
    /// and is silently skipped on the DELETE path.
    fn append_to_batch(&self, db: &rocksdb::DB, batch: &mut WriteBatch) -> Result<(), StateError> {
        for (idx, cfp) in self.cfs.iter().enumerate() {
            let name = CfId(idx as u8).name();
            if !cfp.writes.is_empty() {
                let cf = db
                    .cf_handle(name)
                    .ok_or_else(|| StateError::MissingColumnFamily(name.to_string()))?;
                for (key, value) in &cfp.writes {
                    batch.put_cf(cf, key, value);
                }
            }
            if !cfp.deletes.is_empty() {
                if let Some(cf) = db.cf_handle(name) {
                    for key in &cfp.deletes {
                        batch.delete_cf(cf, key);
                    }
                }
            }
        }
        Ok(())
    }

    /// The native-root dirty `(cf_tag, key) -> Option<value>` set these pending
    /// mutations imply — writes (`Some`) and deletes (`None`) hitting the 6
    /// native-root CFs only, BORROWED out of the pending maps.
    ///
    /// s450 (double-clone elimination): the flush used to clone every dirty key
    /// AND value out of the overlay here, and `apply_native_dirty` then cloned
    /// both AGAIN into its per-bucket grouping — ~4 redundant allocations per
    /// dirty entry per block, scaling with the (workload-driven) dirty-set size.
    /// The trie consumes the dirty set READ-ONLY inside the flush, which holds
    /// the `pending` read guard for its whole duration, so pointing at the
    /// overlay's own key/value buffers is sound and copies nothing. Iteration
    /// order is unchanged: `&[u8]` orders exactly like `Vec<u8>` (lexicographic
    /// over the same bytes), and that `(tag, key)` order is what the trie's
    /// canonical bucket grouping is defined over.
    fn native_dirty_ref(&self) -> BTreeMap<(u8, &[u8]), Option<&[u8]>> {
        let mut dirty: BTreeMap<(u8, &[u8]), Option<&[u8]>> = BTreeMap::new();
        for (idx, cfp) in self.cfs.iter().enumerate() {
            let Some(tag) = crate::native_trie::cf_tag(CfId(idx as u8).name()) else {
                continue;
            };
            for (key, value) in &cfp.writes {
                dirty.insert((tag, key.as_slice()), Some(value.as_slice()));
            }
            for key in &cfp.deletes {
                dirty.insert((tag, key.as_slice()), None);
            }
        }
        dirty
    }

    /// Owning twin of [`PendingState::native_dirty_ref`] for callers that outlive
    /// the pending read guard (the public `dirty_native_keys` API). Byte-identical
    /// content and order — it is exactly `native_dirty_ref` materialised.
    fn native_dirty(&self) -> BTreeMap<(u8, Vec<u8>), Option<Vec<u8>>> {
        self.native_dirty_ref()
            .into_iter()
            .map(|((tag, key), value)| ((tag, key.to_vec()), value.map(<[u8]>::to_vec)))
            .collect()
    }
}

/// bl2 exec pipeline (`TORUS_EXEC_PIPELINE`): the CLOSED pending set of one
/// committed block, moved out of its [`NativeStateOverlay`] by
/// [`NativeStateOverlay::freeze`] once the block's execution is complete.
///
/// Two consumers, on two threads:
/// * the flush worker flushes it ([`FrozenPending::flush_with_native_trie_stats`])
///   — exactly the key/value set the live overlay would have flushed, because it
///   IS the live overlay's maps, moved not copied;
/// * the exec thread layers it under the NEXT block's overlay
///   ([`NativeStateOverlay::with_parent`]) so block N+1 reads block N's post-state
///   (read-your-writes) whether or not the worker has made it durable yet. A
///   layered read of an already-durable set returns the same bytes the DB does,
///   so the layering is timing-independent.
///
/// Read-only after construction: `Sync` by construction (no interior mutability).
pub struct FrozenPending {
    height: u64,
    state: PendingState,
}

impl FrozenPending {
    /// The block height whose post-state this set completes.
    pub fn height(&self) -> u64 {
        self.height
    }

    /// A 1-key frozen set holding only the native applied-height marker
    /// (`CF_CONSENSUS_META` / `META_NATIVE_APPLIED_HEIGHT` = `height`, big-endian —
    /// byte-identical to the standalone marker write). Used for empty / non-native
    /// blocks under the pipeline so the marker advances IN ORDER behind the previous
    /// block's batch and is visible through the next overlay's parent layer while
    /// the write is still in flight (resident-book staleness guard, F5/F7).
    pub fn marker_only(height: u64) -> Self {
        let mut state = PendingState::new();
        let id = intern_cf(crate::cf::CF_CONSENSUS_META).expect("CF_CONSENSUS_META is registered");
        state.cf_mut(id).writes.insert(
            crate::cf::META_NATIVE_APPLIED_HEIGHT.to_vec(),
            height.to_be_bytes().to_vec(),
        );
        Self { height, state }
    }

    /// Number of pending writes + tombstones in this set.
    pub fn entry_count(&self) -> usize {
        self.state
            .cfs
            .iter()
            .map(|c| c.writes.len() + c.deletes.len())
            .sum()
    }

    /// Mirror of [`NativeStateOverlay::dirty_evm_accounts`] over the frozen set.
    pub fn dirty_evm_accounts(&self) -> Vec<Address> {
        dirty_evm_accounts_of(&self.state)
    }

    /// Mirror of [`NativeStateOverlay::flush_with_native_trie_stats`] over the
    /// frozen set — same code path, same batch content.
    pub fn flush_with_native_trie_stats(
        &self,
        target: &StateDb,
        applied_height: Option<u64>,
        trie_cache: Option<&mut crate::native_trie::NativeTrieCache>,
        member_cache: Option<&mut crate::native_trie::NativeMemberCache>,
    ) -> Result<NativeFlushStats, StateError> {
        self.flush_with_sidecar_native_trie_stats(None, target, applied_height, trie_cache, member_cache)
    }

    /// Item 6a (deferred book save): [`Self::flush_with_native_trie_stats`]
    /// with an optional SIDECAR pending set folded in — the flush worker's
    /// worker-side book writes. The sidecar's entries join the SAME atomic
    /// batch and the SAME native-root dirty set as this frozen set's own
    /// entries, so the persisted bytes, the state root and the marker
    /// atomicity are exactly what a serial save-on-exec-thread would have
    /// produced. The sidecar is appended AFTER the main set (and overwrites
    /// it in the dirty map) — in practice the key sets are disjoint: under
    /// deferral the exec thread writes NO book-CF keys into its overlay.
    pub fn flush_with_sidecar_native_trie_stats(
        &self,
        sidecar: Option<&FrozenPending>,
        target: &StateDb,
        applied_height: Option<u64>,
        trie_cache: Option<&mut crate::native_trie::NativeTrieCache>,
        member_cache: Option<&mut crate::native_trie::NativeMemberCache>,
    ) -> Result<NativeFlushStats, StateError> {
        flush_pending_with_native_trie_stats(
            &self.state,
            sidecar.map(|s| &s.state),
            target,
            applied_height,
            trie_cache,
            member_cache,
        )
    }
}

fn dirty_evm_accounts_of(state: &PendingState) -> Vec<Address> {
    let accounts_cf = intern_cf(CF_ACCOUNTS).expect("CF_ACCOUNTS is registered");
    let cfp = state.cf(accounts_cf);
    let mut addrs = Vec::new();
    for key in cfp.writes.keys() {
        if key.len() == 20 {
            addrs.push(Address::from_slice(key));
        }
    }
    for key in &cfp.deletes {
        if key.len() == 20 {
            addrs.push(Address::from_slice(key));
        }
    }
    addrs
}

#[derive(Clone)]
pub struct NativeStateOverlay {
    db: StateDb,
    pending: Arc<RwLock<PendingState>>,
    /// bl2 exec pipeline: the previous block's frozen pending set, consulted
    /// between this overlay's own pending set and the DB on every read. `None`
    /// on the serial path (exact-today: own pending -> DB).
    parent: Option<Arc<FrozenPending>>,
}

impl NativeStateOverlay {
    pub fn new(db: StateDb) -> Self {
        Self::with_parent(db, None)
    }

    /// Overlay whose reads fall through own pending -> `parent` -> DB
    /// (see [`FrozenPending`]). The parent never flushes through this overlay:
    /// `flush*` / `dirty_*` cover this overlay's own writes only.
    pub fn with_parent(db: StateDb, parent: Option<Arc<FrozenPending>>) -> Self {
        Self {
            db,
            pending: Arc::new(RwLock::new(PendingState::new())),
            parent,
        }
    }

    /// Height of the layered parent, if any (debug assertions / tests).
    pub fn parent_height(&self) -> Option<u64> {
        self.parent.as_ref().map(|p| p.height)
    }

    /// Close this block: MOVE the pending set out into a [`FrozenPending`] tagged
    /// with `height`, leaving this overlay (and every clone sharing its Arc)
    /// empty and write-rejecting. The parent link is NOT carried into the frozen
    /// set: by the pipeline's rendezvous invariant everything below the parent is
    /// durable when the next block starts, and the parent itself is what the
    /// frozen set will be layered over next.
    pub fn freeze(&self, height: u64) -> Arc<FrozenPending> {
        let mut guard = self.pending.write().unwrap();
        let mut state = std::mem::replace(&mut *guard, PendingState::new());
        guard.frozen = true;
        state.journal_log.clear();
        state.checkpoints.clear();
        state.frozen = false;
        Arc::new(FrozenPending { height, state })
    }

    pub fn flush(&self, target: &StateDb) -> Result<(), StateError> {
        let state = self.pending.read().unwrap();
        let mut batch = WriteBatch::default();
        state.append_to_batch(target.inner(), &mut batch)?;
        target.write(batch)
    }

    /// T4.4: Persist all pending writes to `target` in one atomic batch, then clear the
    /// journal. This is the per-transaction COMMIT for writer-precompile side effects —
    /// the EVM executor calls it only when the calling tx succeeded. A no-op when the
    /// journal is empty, so read-only paths never touch the database.
    pub fn commit_tx(&self, target: &StateDb) -> Result<(), StateError> {
        let mut state = self.pending.write().unwrap();
        if state.is_empty() {
            // Per-tx reset even on the empty path: the frame checkpoint stack must not
            // leak across transactions (see `checkpoint`).
            state.journal_log.clear();
            state.checkpoints.clear();
            return Ok(());
        }
        let mut batch = WriteBatch::default();
        state.append_to_batch(target.inner(), &mut batch)?;
        target.write(batch)?;
        state.clear();
        Ok(())
    }

    /// T4.4: Drop all pending writes without persisting them — the per-transaction
    /// REVERT for writer-precompile side effects (calling EVM tx reverted or halted).
    pub fn discard_tx(&self) {
        let mut state = self.pending.write().unwrap();
        state.clear();
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
            let cfp = state.cf_mut(entry.cf);
            match entry.prev_write {
                Some(v) => {
                    cfp.writes.insert(entry.key.clone(), v);
                }
                None => {
                    cfp.writes.remove(&entry.key);
                }
            }
            if entry.prev_deleted {
                cfp.deletes.insert(entry.key);
            } else {
                cfp.deletes.remove(&entry.key);
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
        state
            .cfs
            .iter()
            .map(|c| c.writes.len() + c.deletes.len())
            .sum()
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
        dirty_evm_accounts_of(&state)
    }

    /// The native-root dirty `(cf_tag, key) -> Option<value>` set this overlay would flush — writes
    /// (`Some`) and deletes (`None`) hitting the 6 native-root CFs only (mirrors `dirty_evm_accounts`).
    /// Fed to the bucketed native trie (A2.2) so it tracks the committed native state. Non-root CFs
    /// (nonces, governance, markets, …) are excluded — they are not part of the native root.
    pub fn dirty_native_keys(&self) -> BTreeMap<(u8, Vec<u8>), Option<Vec<u8>>> {
        let state = self.pending.read().unwrap();
        state.native_dirty()
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
        self.flush_with_native_trie_stats(target, applied_height, None, None)
            .map(|_| ())
    }

    /// rank-root: [`flush_with_native_trie_inner`] with (a) an optional in-RAM
    /// trie cache (`TORUS_NATIVE_ROOT_CACHE` — sibling reads from RAM +
    /// clean-write elision; persisted bytes identical either way) and (b) a
    /// flush-phase breakdown for the exec metrics. `cache: None` is the
    /// exact-today path — the legacy entry points delegate here with `None`
    /// and discard the stats.
    pub fn flush_with_native_trie_stats(
        &self,
        target: &StateDb,
        applied_height: Option<u64>,
        trie_cache: Option<&mut crate::native_trie::NativeTrieCache>,
        member_cache: Option<&mut crate::native_trie::NativeMemberCache>,
    ) -> Result<NativeFlushStats, StateError> {
        let state = self.pending.read().unwrap();
        flush_pending_with_native_trie_stats(
            &state,
            None,
            target,
            applied_height,
            trie_cache,
            member_cache,
        )
    }
}

/// The one flush implementation, over a borrowed pending set: shared by the live
/// overlay (serial path, read guard held by the caller) and by [`FrozenPending`]
/// (flush worker). Byte-identical batch content either way.
fn flush_pending_with_native_trie_stats(
    state: &PendingState,
    sidecar: Option<&PendingState>,
    target: &StateDb,
    applied_height: Option<u64>,
    trie_cache: Option<&mut crate::native_trie::NativeTrieCache>,
    member_cache: Option<&mut crate::native_trie::NativeMemberCache>,
) -> Result<NativeFlushStats, StateError> {
    {
        let raw = target.inner();

        let build_timer = std::time::Instant::now();
        let mut batch = WriteBatch::default();
        state.append_to_batch(raw, &mut batch)?;
        // Item 6a: the sidecar (worker-side deferred book writes) joins the
        // SAME atomic batch, appended after the main set (last-wins on the
        // — in practice disjoint — key sets).
        if let Some(side) = sidecar {
            side.append_to_batch(raw, &mut batch)?;
        }

        // Native-root dirty map, folded into the SAME batch. BORROWED from the
        // pending maps (s450): `state`'s read guard is held for this whole
        // function and the trie only reads the set, so nothing is copied out.
        let mut dirty = state.native_dirty_ref();
        // Item 6a: the sidecar's book-CF entries enter the ROOT computation
        // exactly as if the exec thread had written them into its overlay
        // (same canonical (tag, key) order — BTreeMap merge).
        if let Some(side) = sidecar {
            for (k, v) in side.native_dirty_ref() {
                dirty.insert(k, v);
            }
        }
        // r7: the batch BUILD (serializing the pending maps) is a different
        // lever from the RocksDB WRITE (WAL + memtable), so keep the two
        // timers apart instead of summing them into one `write_seconds`.
        let write_build_secs = build_timer.elapsed().as_secs_f64();

        // Trie maintenance (rank-root round-3): the unified append computes all
        // ops fallibly BEFORE appending, so on Err nothing was appended and the
        // native-CF writes still flush (the incremental root is merely stale for
        // the block). Optionally uses the in-RAM trie node cache, the bucket-
        // member cache, and parallel per-bucket hashing — any combination,
        // always byte-identical to the serial uncached path.
        let root_timer = std::time::Instant::now();
        let parallel = crate::native_trie::parallel_bucket_hash_threads();
        // L3 flush-pipe (a): adaptive engagement threshold — parallel per-bucket
        // hashing engages only above this dirty-bucket count (default 1 = exact-today).
        let bucket_hash_min = crate::native_trie::bucket_hash_min_buckets();
        let mut trie_cache = trie_cache;
        let mut member_cache = member_cache;
        let mut apply_out: Option<crate::native_trie::NativeTrieApply> = None;
        let mut stats = NativeFlushStats {
            root_seconds: 0.0,
            write_seconds: 0.0,
            write_build_seconds: write_build_secs,
            write_db_seconds: 0.0,
            batch_bytes: 0,
            dirty_buckets: 0,
            bucket_scans: 0,
            member_hits: 0,
            member_misses: 0,
            member_evictions: 0,
            member_resident_buckets: 0,
            dirty_entries_by_cf: [0; 6],
        };
        // 3c funnel attribution: dirty-entry composition per cf_tag.
        for (tag, _) in dirty.keys() {
            if let Some(slot) = stats.dirty_entries_by_cf.get_mut(*tag as usize) {
                *slot += 1;
            }
        }
        let trie_result = if dirty.is_empty() {
            Ok(())
        } else {
            match crate::native_trie::apply_native_dirty(
                target,
                &mut batch,
                &dirty,
                trie_cache.as_deref_mut(),
                member_cache.as_deref_mut(),
                parallel,
                bucket_hash_min,
            ) {
                Ok(a) => {
                    stats.dirty_buckets = a.rehashed_buckets;
                    stats.bucket_scans = a.bucket_scans;
                    stats.member_hits = a.member_hits;
                    stats.member_misses = a.member_misses;
                    apply_out = Some(a);
                    Ok(())
                }
                Err(e) => {
                    if let Some(c) = trie_cache.as_deref_mut() {
                        c.invalidate();
                    }
                    if let Some(c) = member_cache.as_deref_mut() {
                        c.invalidate();
                    }
                    Err(e)
                }
            }
        };
        stats.root_seconds = root_timer.elapsed().as_secs_f64();

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

        // Measured AFTER the trie/mirror puts and the applied-height marker were
        // appended: this is the batch RocksDB actually gets.
        stats.batch_bytes = batch.size_in_bytes();
        let write_timer = std::time::Instant::now();
        let write_result = target.write(batch);
        stats.write_db_seconds = write_timer.elapsed().as_secs_f64();
        // Legacy total kept intact so `torus_exec_state_write_seconds` stays
        // comparable across the split.
        stats.write_seconds = stats.write_build_seconds + stats.write_db_seconds;

        match write_result {
            Ok(()) => {
                // Batch durable: fold the committed changes into the caches. A
                // trie-maintenance ERROR left the persisted trie stale — the
                // images must not pretend otherwise (invalidate both, which
                // share the staleness cause).
                match (&trie_result, apply_out) {
                    (Ok(()), Some(apply)) => {
                        if let (Some(c), Some(changed)) =
                            (trie_cache.as_deref_mut(), &apply.trie_changed)
                        {
                            c.commit_changed(changed, apply.root);
                        }
                        if let Some(c) = member_cache.as_deref_mut() {
                            stats.member_evictions =
                                c.commit_finals(apply.member_finals, apply.root);
                        }
                    }
                    (Ok(()), None) => {} // empty dirty set — trie untouched
                    (Err(_), _) => {
                        if let Some(c) = trie_cache.as_deref_mut() {
                            c.invalidate();
                        }
                        if let Some(c) = member_cache.as_deref_mut() {
                            c.invalidate();
                        }
                    }
                }
            }
            Err(e) => {
                if let Some(c) = trie_cache.as_deref_mut() {
                    c.invalidate();
                }
                if let Some(c) = member_cache.as_deref_mut() {
                    c.invalidate();
                }
                return Err(e.into());
            }
        }
        // L3 #2: record post-flush residency for the eviction-pressure gauge.
        if let Some(c) = member_cache.as_deref() {
            stats.member_resident_buckets = c.len();
        }
        trie_result?;
        Ok(stats)
    }
}

/// rank-root: flush-phase breakdown surfaced to the exec metrics
/// (`exec_root_seconds` / `exec_state_write_seconds` / `exec_root_dirty_buckets`).
pub struct NativeFlushStats {
    /// Native-trie maintenance time (bucket rehash + path propagation).
    pub root_seconds: f64,
    /// WriteBatch build + atomic RocksDB write time
    /// (== `write_build_seconds` + `write_db_seconds`; kept for continuity of
    /// the `torus_exec_state_write_seconds` series across the r7 split).
    pub write_seconds: f64,
    /// r7 split: serializing the pending overlay maps into the WriteBatch
    /// (`append_to_batch`) — CPU, parallelizable.
    pub write_build_seconds: f64,
    /// r7 split: the atomic `rocksdb::write(batch)` alone — WAL + memtable,
    /// where a write stall shows up. Includes the trie/mirror puts appended
    /// during the root phase (their *append* cost is in `root_seconds`).
    pub write_db_seconds: f64,
    /// r7 split: size of the batch handed to RocksDB, measured after the
    /// trie/mirror puts and the applied-height marker were appended.
    pub batch_bytes: usize,
    /// Buckets rehashed (uncached: distinct dirty buckets; cached: post-elision).
    pub dirty_buckets: usize,
    /// rank-root round-3: CF_NATIVE_HASHED prefix-scans performed this flush.
    /// Drops below `dirty_buckets` as the bucket-member cache serves hits.
    pub bucket_scans: usize,
    /// rank-root round-3: bucket-member cache hits / misses / LRU evictions.
    pub member_hits: usize,
    pub member_misses: usize,
    pub member_evictions: usize,
    /// L3 #2: resident bucket count in the member cache after this flush's
    /// write-through (0 when the cache is disabled) — eviction-pressure witness.
    pub member_resident_buckets: usize,
    /// 3c: native-root dirty entries per cf_tag this flush (frozen
    /// NATIVE_ROOT_CFS order) — funnel attribution of the dirty-set
    /// composition.
    pub dirty_entries_by_cf: [usize; 6],
}

impl StateBackend for NativeStateOverlay {
    fn get_cf_raw(&self, cf: &str, key: &[u8]) -> Result<Option<Vec<u8>>, StateError> {
        // C2: interned CF + borrowed key lookup — ZERO allocations on this path
        // (was one String + one Vec<u8> per get). An unregistered CF name can
        // never hold overlay entries (put/delete reject it), so it falls through
        // to the database, which reports MissingColumnFamily exactly as before.
        if let Some(id) = intern_cf(cf) {
            {
                let state = self.pending.read().unwrap();
                if let Some(hit) = state.lookup(id, key) {
                    return Ok(hit.map(<[u8]>::to_vec));
                }
            }
            // bl2 exec pipeline: previous block's frozen set (read-your-writes).
            if let Some(parent) = &self.parent {
                if let Some(hit) = parent.state.lookup(id, key) {
                    return Ok(hit.map(<[u8]>::to_vec));
                }
            }
        }
        self.db.get_cf_raw(cf, key)
    }

    fn put_cf_raw(&self, cf: &str, key: &[u8], value: &[u8]) -> Result<(), StateError> {
        // C2: an unregistered CF errors HERE instead of at flush time (pre-C2 the
        // write sat in the map and `flush` returned this same MissingColumnFamily).
        // No registered-CF path changes; unknown names never occur in execution.
        let id = intern_cf(cf).ok_or_else(|| StateError::MissingColumnFamily(cf.to_string()))?;
        let mut state = self.pending.write().unwrap();
        if state.frozen {
            return Err(PendingState::frozen_err());
        }
        state.record(id, key);
        let cfp = state.cf_mut(id);
        cfp.deletes.remove(key);
        cfp.writes.insert(key.to_vec(), value.to_vec());
        Ok(())
    }

    fn delete_cf_raw(&self, cf: &str, key: &[u8]) -> Result<(), StateError> {
        let id = intern_cf(cf).ok_or_else(|| StateError::MissingColumnFamily(cf.to_string()))?;
        let mut state = self.pending.write().unwrap();
        if state.frozen {
            return Err(PendingState::frozen_err());
        }
        state.record(id, key);
        let cfp = state.cf_mut(id);
        cfp.writes.remove(key);
        cfp.deletes.insert(key.to_vec());
        Ok(())
    }

    fn iterate_cf(
        &self,
        cf: &str,
        prefix: Option<&[u8]>,
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>, StateError> {
        let Some(id) = intern_cf(cf) else {
            // Unregistered CF: the overlay cannot hold entries for it; delegate
            // (the DB reports MissingColumnFamily, same as pre-C2).
            return StateBackend::iterate_cf(&self.db, cf, prefix);
        };
        // Merge RocksDB entries with pending: RocksDB first, then the parent
        // layer (bl2 exec pipeline; absent on the serial path), then this
        // overlay's own pending set — each layer's tombstones remove and its
        // writes override what sits below.
        let db_entries = StateBackend::iterate_cf(&self.db, cf, prefix)?;
        let mut merged: BTreeMap<Vec<u8>, Vec<u8>> = db_entries.into_iter().collect();
        if let Some(parent) = &self.parent {
            parent.state.overlay_into(id, prefix, &mut merged);
        }
        let state = self.pending.read().unwrap();
        state.overlay_into(id, prefix, &mut merged);
        drop(state);
        Ok(merged.into_iter().collect())
    }

    fn atomic_write(&self, ops: &[AtomicWriteOp<'_>]) -> Result<(), StateError> {
        // Intern every CF FIRST so an unregistered name rejects the whole op set
        // before any mutation (atomic even on the error path).
        let ids: Vec<CfId> = ops
            .iter()
            .map(|op| {
                let cf = match op {
                    AtomicWriteOp::Put { cf, .. } | AtomicWriteOp::Delete { cf, .. } => cf,
                };
                intern_cf(cf).ok_or_else(|| StateError::MissingColumnFamily(cf.to_string()))
            })
            .collect::<Result<_, _>>()?;
        let mut state = self.pending.write().unwrap();
        if state.frozen {
            return Err(PendingState::frozen_err());
        }
        for (op, &id) in ops.iter().zip(ids.iter()) {
            match op {
                AtomicWriteOp::Put { key, value, .. } => {
                    state.record(id, key);
                    let cfp = state.cf_mut(id);
                    cfp.deletes.remove(*key);
                    cfp.writes.insert(key.to_vec(), value.to_vec());
                }
                AtomicWriteOp::Delete { key, .. } => {
                    state.record(id, key);
                    let cfp = state.cf_mut(id);
                    cfp.writes.remove(*key);
                    cfp.deletes.insert(key.to_vec());
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
        assert_eq!(StateDb::get_cf_raw(&db, cf, b"k1").unwrap().unwrap(), b"v1");
        assert_eq!(
            overlay.pending_write_count(),
            0,
            "journal cleared on commit"
        );

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
            StateBackend::get_cf_raw(&overlay, cf, b"k")
                .unwrap()
                .unwrap(),
            b"inner"
        );

        overlay.revert_to_checkpoint();
        // Overwrite rolled back to the pre-checkpoint value; the fresh key is gone.
        assert_eq!(
            StateBackend::get_cf_raw(&overlay, cf, b"k")
                .unwrap()
                .unwrap(),
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
            StateBackend::get_cf_raw(&overlay, cf, b"k")
                .unwrap()
                .unwrap(),
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
            StateBackend::get_cf_raw(&overlay, cf, b"b")
                .unwrap()
                .unwrap(),
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

    /// C2 RED-first (missing-symbol red, T156-F1 doctrine): every registered CF
    /// name interns to a distinct id that resolves back to the SAME `&'static str`
    /// — the flush path therefore addresses byte-identical CF names — and an
    /// unregistered name does not intern.
    #[test]
    fn cf_interning_roundtrip_all_registered_names() {
        let mut seen = std::collections::HashSet::new();
        for name in crate::cf::ALL_CF_NAMES {
            let id = intern_cf(name).unwrap_or_else(|| panic!("{name} must intern"));
            assert_eq!(id.name(), *name, "intern must round-trip to the same name");
            assert!(seen.insert(id), "ids must be distinct ({name})");
        }
        assert_eq!(seen.len(), NUM_CFS);
        assert!(intern_cf("cf_not_a_real_family").is_none());
        assert!(intern_cf("").is_none());
    }

    /// C2 equivalence proof: keys/values stored in RocksDB through the interned
    /// overlay are BYTE-IDENTICAL to direct `StateDb` writes of the same
    /// `(cf, key, value)` triples — across native-root and non-root CFs, with
    /// binary keys (0x00 / 0xff bytes), an empty key, deletes, and overwrites.
    /// Also pins that the consensus-authoritative native root over the two
    /// databases is equal (identical hash preimages).
    #[test]
    fn interned_overlay_stores_identical_rocksdb_keys() {
        use crate::cf::{CF_NATIVE_NONCES, CF_NATIVE_POSITIONS, CF_STAKING_VALIDATORS};

        let triples: Vec<(&str, Vec<u8>, Vec<u8>)> = vec![
            (
                CF_NATIVE_BALANCES,
                b"\x00\x01acct".to_vec(),
                b"bal".to_vec(),
            ),
            (CF_NATIVE_BALANCES, vec![0xff; 28], b"maxkey".to_vec()),
            (CF_NATIVE_BALANCES, Vec::new(), b"emptykey".to_vec()),
            (
                CF_NATIVE_POSITIONS,
                b"trader1:mkt1".to_vec(),
                vec![0x00, 0xff, 0x7f],
            ),
            (CF_STAKING_VALIDATORS, b"val1".to_vec(), b"stake".to_vec()),
            (
                CF_NATIVE_NONCES,
                b"sender\x00\x00\x00\x01".to_vec(),
                b"h".to_vec(),
            ),
        ];
        // Overwrite + delete exercised on both paths identically.
        let overwrite = (
            CF_NATIVE_BALANCES,
            b"\x00\x01acct".to_vec(),
            b"bal2".to_vec(),
        );
        let delete = (CF_NATIVE_POSITIONS, b"trader1:mkt1".to_vec());

        // DB 1: direct StateDb writes.
        let (db_direct, _dir1) = temp_db();
        for (cf, key, value) in &triples {
            StateDb::put_cf_raw(&db_direct, cf, key, value).unwrap();
        }
        StateDb::put_cf_raw(&db_direct, overwrite.0, &overwrite.1, &overwrite.2).unwrap();
        StateDb::delete_cf_raw(&db_direct, delete.0, &delete.1).unwrap();

        // DB 2: the same mutations through the interned overlay, then flush.
        let (db_overlay, _dir2) = temp_db();
        let overlay = NativeStateOverlay::new(db_overlay.clone());
        for (cf, key, value) in &triples {
            StateBackend::put_cf_raw(&overlay, cf, key, value).unwrap();
        }
        StateBackend::put_cf_raw(&overlay, overwrite.0, &overwrite.1, &overwrite.2).unwrap();
        StateBackend::delete_cf_raw(&overlay, delete.0, &delete.1).unwrap();
        overlay.flush(&db_overlay).unwrap();

        // Every CF's full (key, value) listing must be byte-identical.
        for cf in crate::cf::ALL_CF_NAMES {
            let direct = StateBackend::iterate_cf(&db_direct, cf, None).unwrap();
            let via_overlay = StateBackend::iterate_cf(&db_overlay, cf, None).unwrap();
            assert_eq!(
                direct, via_overlay,
                "stored keys/values diverge in {cf}: interned overlay is NOT byte-identical"
            );
        }

        // And the consensus-authoritative native root agrees (same hash preimages).
        assert_eq!(
            crate::native_trie::native_root_full(&db_direct).unwrap(),
            crate::native_trie::native_root_full(&db_overlay).unwrap(),
            "native state root must be identical for direct vs interned-overlay writes"
        );
    }

    /// C2: an unregistered CF name is rejected at put/delete time with the SAME
    /// error (`MissingColumnFamily`) that pre-C2 code deferred to flush time —
    /// no such write can ever land in a `StateDb` (open registers ALL_CF_NAMES).
    #[test]
    fn unknown_cf_rejected_at_put_with_missing_cf_error() {
        let (db, _dir) = temp_db();
        let overlay = NativeStateOverlay::new(db);
        let err = StateBackend::put_cf_raw(&overlay, "cf_bogus", b"k", b"v").unwrap_err();
        assert!(matches!(err, StateError::MissingColumnFamily(ref n) if n == "cf_bogus"));
        let err = StateBackend::delete_cf_raw(&overlay, "cf_bogus", b"k").unwrap_err();
        assert!(matches!(err, StateError::MissingColumnFamily(ref n) if n == "cf_bogus"));
        assert_eq!(overlay.pending_write_count(), 0, "nothing may be buffered");
    }

    /// 3c: `CF_BOOK_ORDER_ROWS` is NODE-LOCAL — it must never contribute to the
    /// native root. Witness: (a) it has no `cf_tag`, (b) overlay writes to it do
    /// NOT appear in `native_dirty()`, (c) flushing a write to it leaves the
    /// native root oracle unchanged while the bytes ARE durable.
    #[test]
    fn book_order_rows_cf_is_excluded_from_native_root() {
        use crate::cf::CF_BOOK_ORDER_ROWS;

        assert!(
            crate::native_trie::cf_tag(CF_BOOK_ORDER_ROWS).is_none(),
            "CF_BOOK_ORDER_ROWS must not be a native-root CF"
        );

        let (db, _dir) = temp_db();
        crate::native_trie::build_native_trie_to_cf(&db).unwrap();
        let root_before = crate::native_trie::native_root_full(&db).unwrap();

        let overlay = NativeStateOverlay::new(db.clone());
        StateBackend::put_cf_raw(
            &overlay,
            CF_BOOK_ORDER_ROWS,
            b"\x00\x00\x00\x00\x00\x00\x00\x01\x01k",
            b"orderrow",
        )
        .unwrap();
        {
            let state = overlay.pending.read().unwrap();
            assert!(
                state.native_dirty().is_empty(),
                "order-row store writes must not enter the native dirty set"
            );
        }
        overlay
            .flush_with_native_trie_stats(&db, Some(1), None, None)
            .unwrap();

        assert_eq!(
            crate::native_trie::native_root_full(&db).unwrap(),
            root_before,
            "node-local order-row store must not perturb the native root"
        );
        assert!(
            StateDb::get_cf_raw(
                &db,
                CF_BOOK_ORDER_ROWS,
                b"\x00\x00\x00\x00\x00\x00\x00\x01\x01k"
            )
            .unwrap()
            .is_some(),
            "the write must still be durable"
        );
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

    /// r7 state-write-build-vs-db-split: `write_seconds` used to lump the
    /// WriteBatch *build* (serializing the pending maps — parallelizable) with
    /// the RocksDB *write* (WAL + memtable — a WriteOptions/WAL lever). The two
    /// are now reported separately and must still sum to the old total, so the
    /// existing `torus_exec_state_write_seconds` series stays comparable across
    /// the split. `batch_bytes` is the batch handed to RocksDB, measured after
    /// the trie/mirror puts and the applied-height marker were appended.
    #[test]
    fn flush_stats_split_build_and_db_write() {
        let (db, _dir) = temp_db();
        let cf = CF_NATIVE_BALANCES;
        let overlay = NativeStateOverlay::new(db.clone());
        // Enough payload that the batch is unambiguously non-empty.
        let payload = vec![0xABu8; 512];
        for i in 0u32..64 {
            StateBackend::put_cf_raw(&overlay, cf, &i.to_be_bytes(), &payload).unwrap();
        }

        let stats = overlay
            .flush_with_native_trie_stats(&db, Some(7), None, None)
            .unwrap();

        assert!(
            stats.write_build_seconds > 0.0,
            "batch build must be timed: {}",
            stats.write_build_seconds
        );
        assert!(
            stats.write_db_seconds > 0.0,
            "rocksdb write must be timed: {}",
            stats.write_db_seconds
        );
        let sum = stats.write_build_seconds + stats.write_db_seconds;
        assert!(
            (sum - stats.write_seconds).abs() < 1e-9,
            "build ({}) + db ({}) = {} must equal the legacy total {}",
            stats.write_build_seconds,
            stats.write_db_seconds,
            sum,
            stats.write_seconds
        );
        // 64 rows x 512-byte values, plus keys, trie/mirror puts and the marker.
        assert!(
            stats.batch_bytes >= 64 * 512,
            "batch_bytes {} must cover the 32 KiB of values written",
            stats.batch_bytes
        );
    }

    // ---- bl2 exec pipeline: layered overlay (FrozenPending parent) ----

    /// `get_cf_raw` resolution order is own pending -> parent layer -> DB, with a
    /// tombstone at any layer shadowing everything below it.
    #[test]
    fn layered_overlay_point_read_semantics() {
        let (db, _dir) = temp_db();
        let cf = CF_NATIVE_BALANCES;
        StateBackend::put_cf_raw(&db, cf, b"db_only", b"d").unwrap();
        StateBackend::put_cf_raw(&db, cf, b"parent_overrides", b"d").unwrap();
        StateBackend::put_cf_raw(&db, cf, b"parent_deletes", b"d").unwrap();
        StateBackend::put_cf_raw(&db, cf, b"child_overrides_all", b"d").unwrap();
        StateBackend::put_cf_raw(&db, cf, b"child_deletes_parent_put", b"d").unwrap();

        let parent_ov = NativeStateOverlay::new(db.clone());
        StateBackend::put_cf_raw(&parent_ov, cf, b"parent_overrides", b"p").unwrap();
        StateBackend::delete_cf_raw(&parent_ov, cf, b"parent_deletes").unwrap();
        StateBackend::put_cf_raw(&parent_ov, cf, b"child_overrides_all", b"p").unwrap();
        StateBackend::put_cf_raw(&parent_ov, cf, b"child_deletes_parent_put", b"p").unwrap();
        StateBackend::put_cf_raw(&parent_ov, cf, b"parent_only", b"p").unwrap();
        let parent = parent_ov.freeze(7);
        assert_eq!(parent.height(), 7);

        let child = NativeStateOverlay::with_parent(db.clone(), Some(parent.clone()));
        StateBackend::put_cf_raw(&child, cf, b"child_overrides_all", b"c").unwrap();
        StateBackend::delete_cf_raw(&child, cf, b"child_deletes_parent_put").unwrap();
        StateBackend::put_cf_raw(&child, cf, b"child_only", b"c").unwrap();

        let get = |k: &[u8]| StateBackend::get_cf_raw(&child, cf, k).unwrap();
        assert_eq!(get(b"db_only").as_deref(), Some(&b"d"[..]));
        assert_eq!(get(b"parent_overrides").as_deref(), Some(&b"p"[..]));
        assert_eq!(
            get(b"parent_deletes"),
            None,
            "parent tombstone shadows the DB"
        );
        assert_eq!(get(b"parent_only").as_deref(), Some(&b"p"[..]));
        assert_eq!(get(b"child_overrides_all").as_deref(), Some(&b"c"[..]));
        assert_eq!(
            get(b"child_deletes_parent_put"),
            None,
            "child tombstone shadows the parent put"
        );
        assert_eq!(get(b"child_only").as_deref(), Some(&b"c"[..]));
        assert_eq!(get(b"absent"), None);

        // Nothing of the parent leaks into the child's own pending set (it flushes
        // its own block's writes only).
        assert_eq!(child.pending_write_count(), 3);

        // iterate_cf merges DB < parent < child with the same shadowing rules.
        let all = StateBackend::iterate_cf(&child, cf, None).unwrap();
        let expect: Vec<(Vec<u8>, Vec<u8>)> = vec![
            (b"child_only".to_vec(), b"c".to_vec()),
            (b"child_overrides_all".to_vec(), b"c".to_vec()),
            (b"db_only".to_vec(), b"d".to_vec()),
            (b"parent_only".to_vec(), b"p".to_vec()),
            (b"parent_overrides".to_vec(), b"p".to_vec()),
        ];
        assert_eq!(all, expect);
        let pfx = StateBackend::iterate_cf(&child, cf, Some(b"parent_")).unwrap();
        assert_eq!(pfx.len(), 2);
        assert_eq!(pfx[0].0, b"parent_only");
    }

    /// A layered read of a parent that is ALREADY durable returns the same bytes the
    /// DB does — the parent is always layered, durable or not (F1/F6), so this is
    /// what makes the layering race-free.
    #[test]
    fn layered_overlay_durable_parent_is_transparent() {
        let (db, _dir) = temp_db();
        let cf = CF_NATIVE_BALANCES;
        StateBackend::put_cf_raw(&db, cf, b"k_del", b"old").unwrap();
        let parent_ov = NativeStateOverlay::new(db.clone());
        StateBackend::put_cf_raw(&parent_ov, cf, b"k_put", b"v").unwrap();
        StateBackend::delete_cf_raw(&parent_ov, cf, b"k_del").unwrap();
        let parent = parent_ov.freeze(3);
        parent
            .flush_with_native_trie_stats(&db, Some(3), None, None)
            .unwrap();

        let layered = NativeStateOverlay::with_parent(db.clone(), Some(parent));
        let plain = NativeStateOverlay::new(db.clone());
        for k in [&b"k_put"[..], b"k_del", b"nope"] {
            assert_eq!(
                StateBackend::get_cf_raw(&layered, cf, k).unwrap(),
                StateBackend::get_cf_raw(&plain, cf, k).unwrap(),
                "key {:?}",
                String::from_utf8_lossy(k)
            );
        }
        assert_eq!(
            StateBackend::iterate_cf(&layered, cf, None).unwrap(),
            StateBackend::iterate_cf(&plain, cf, None).unwrap()
        );
    }

    /// Freezing moves the pending set out; the overlay (and every clone sharing its
    /// pending Arc) rejects further writes and reads as empty-over-DB.
    #[test]
    fn freeze_rejects_later_writes_and_empties_the_overlay() {
        let (db, _dir) = temp_db();
        let cf = CF_NATIVE_BALANCES;
        let ov = NativeStateOverlay::new(db.clone());
        let clone = ov.clone();
        StateBackend::put_cf_raw(&ov, cf, b"a", b"1").unwrap();
        let frozen = ov.freeze(1);
        assert_eq!(frozen.entry_count(), 1);
        assert_eq!(ov.pending_write_count(), 0);
        assert!(StateBackend::put_cf_raw(&clone, cf, b"b", b"2").is_err());
        assert!(StateBackend::delete_cf_raw(&ov, cf, b"a").is_err());
        assert_eq!(StateBackend::get_cf_raw(&ov, cf, b"a").unwrap(), None);
    }

    /// Flushing a frozen pending set writes the SAME key/value set (state + trie +
    /// marker) as flushing the live overlay would have.
    #[test]
    fn frozen_flush_identical_to_overlay_flush() {
        use crate::cf::{CF_CONSENSUS_META, CF_NATIVE_POSITIONS, META_NATIVE_APPLIED_HEIGHT};
        let dump = |db: &StateDb| -> Vec<Vec<(Vec<u8>, Vec<u8>)>> {
            crate::cf::ALL_CF_NAMES
                .iter()
                .map(|cf| StateBackend::iterate_cf(db, cf, None).unwrap())
                .collect()
        };
        let populate = |ov: &NativeStateOverlay| {
            for i in 0..64u32 {
                let k = format!("bal{i:03}");
                StateBackend::put_cf_raw(ov, CF_NATIVE_BALANCES, k.as_bytes(), &i.to_be_bytes())
                    .unwrap();
            }
            StateBackend::put_cf_raw(ov, CF_NATIVE_POSITIONS, b"pos", b"x").unwrap();
            StateBackend::delete_cf_raw(ov, CF_NATIVE_BALANCES, b"bal001").unwrap();
        };

        let (db_a, _da) = temp_db();
        let ov_a = NativeStateOverlay::new(db_a.clone());
        populate(&ov_a);
        ov_a.flush_with_native_trie_stats(&db_a, Some(9), None, None)
            .unwrap();

        let (db_b, _db) = temp_db();
        let ov_b = NativeStateOverlay::new(db_b.clone());
        populate(&ov_b);
        let frozen = ov_b.freeze(9);
        frozen
            .flush_with_native_trie_stats(&db_b, Some(9), None, None)
            .unwrap();

        assert_eq!(dump(&db_a), dump(&db_b));
        assert_eq!(
            StateBackend::get_cf_raw(&db_b, CF_CONSENSUS_META, META_NATIVE_APPLIED_HEIGHT).unwrap(),
            Some(9u64.to_be_bytes().to_vec())
        );
        assert_eq!(
            crate::native_trie::persisted_native_root(&db_a).unwrap(),
            crate::native_trie::persisted_native_root(&db_b).unwrap()
        );
    }

    /// The 1-key marker layer (empty-block Marker job) is visible through a child
    /// overlay's point read and flushes to exactly the applied-height marker put.
    #[test]
    fn marker_only_layer_reads_and_flushes_marker() {
        use crate::cf::{CF_CONSENSUS_META, META_NATIVE_APPLIED_HEIGHT};
        let (db, _dir) = temp_db();
        let marker = FrozenPending::marker_only(42);
        assert_eq!(marker.height(), 42);
        assert_eq!(marker.entry_count(), 1);
        assert!(marker.dirty_evm_accounts().is_empty());
        let child = NativeStateOverlay::with_parent(db.clone(), Some(Arc::new(marker)));
        assert_eq!(
            StateBackend::get_cf_raw(&child, CF_CONSENSUS_META, META_NATIVE_APPLIED_HEIGHT)
                .unwrap(),
            Some(42u64.to_be_bytes().to_vec())
        );
        // DB untouched until the marker layer itself flushes.
        assert_eq!(
            StateDb::get_cf_raw(&db, CF_CONSENSUS_META, META_NATIVE_APPLIED_HEIGHT).unwrap(),
            None
        );
        let frozen = FrozenPending::marker_only(42);
        let stats = frozen
            .flush_with_native_trie_stats(&db, Some(42), None, None)
            .unwrap();
        assert_eq!(
            stats.dirty_buckets, 0,
            "marker is a non-root CF: no trie work"
        );
        assert_eq!(
            StateDb::get_cf_raw(&db, CF_CONSENSUS_META, META_NATIVE_APPLIED_HEIGHT).unwrap(),
            Some(42u64.to_be_bytes().to_vec())
        );
    }

    /// Item 6a: flushing a frozen set WITH a sidecar (the flush worker's
    /// deferred book writes) is byte-identical — every CF, the persisted
    /// native root, the marker — to flushing ONE overlay holding the union of
    /// both write sets. And the sidecar's root-CF entries really enter the
    /// root computation (negative control: dropping the sidecar changes it).
    #[test]
    fn sidecar_flush_identical_to_combined_overlay_flush() {
        use crate::cf::{CF_BOOK_ORDER_ROWS, CF_NATIVE_ORDER_BOOKS, CF_NATIVE_POSITIONS};
        let dump = |db: &StateDb| -> Vec<Vec<(Vec<u8>, Vec<u8>)>> {
            crate::cf::ALL_CF_NAMES
                .iter()
                .map(|cf| StateBackend::iterate_cf(db, cf, None).unwrap())
                .collect()
        };
        let put_main = |ov: &NativeStateOverlay| {
            for i in 0..16u32 {
                let k = format!("bal{i:03}");
                StateBackend::put_cf_raw(ov, CF_NATIVE_BALANCES, k.as_bytes(), &i.to_be_bytes())
                    .unwrap();
            }
            StateBackend::put_cf_raw(ov, CF_NATIVE_POSITIONS, b"pos", b"x").unwrap();
        };
        let put_books = |ov: &NativeStateOverlay| {
            // Root CF (level rows / meta / stops)…
            for i in 0..8u32 {
                let k = format!("lvl{i:03}");
                StateBackend::put_cf_raw(ov, CF_NATIVE_ORDER_BOOKS, k.as_bytes(), &i.to_be_bytes())
                    .unwrap();
            }
            StateBackend::delete_cf_raw(ov, CF_NATIVE_ORDER_BOOKS, b"gone").unwrap();
            // …and the node-local order-row store.
            StateBackend::put_cf_raw(ov, CF_BOOK_ORDER_ROWS, b"row1", b"r").unwrap();
        };

        // Combined reference: one overlay carries both write sets.
        let (db_a, _da) = temp_db();
        let ov = NativeStateOverlay::new(db_a.clone());
        put_main(&ov);
        put_books(&ov);
        ov.freeze(5)
            .flush_with_native_trie_stats(&db_a, Some(5), None, None)
            .unwrap();

        // Split: main frozen set + book sidecar, one flush.
        let (db_b, _db) = temp_db();
        let main = NativeStateOverlay::new(db_b.clone());
        put_main(&main);
        let side = NativeStateOverlay::new(db_b.clone());
        put_books(&side);
        main.freeze(5)
            .flush_with_sidecar_native_trie_stats(
                Some(&side.freeze(5)),
                &db_b,
                Some(5),
                None,
                None,
            )
            .unwrap();

        assert_eq!(dump(&db_a), dump(&db_b), "sidecar flush must be byte-identical");
        let root_a = crate::native_trie::persisted_native_root(&db_a).unwrap();
        assert_eq!(
            root_a,
            crate::native_trie::persisted_native_root(&db_b).unwrap()
        );

        // Negative control: without the sidecar the root differs — proof the
        // sidecar's CF_NATIVE_ORDER_BOOKS entries participate in the root.
        let (db_c, _dc) = temp_db();
        let main_only = NativeStateOverlay::new(db_c.clone());
        put_main(&main_only);
        main_only
            .freeze(5)
            .flush_with_native_trie_stats(&db_c, Some(5), None, None)
            .unwrap();
        assert_ne!(
            root_a,
            crate::native_trie::persisted_native_root(&db_c).unwrap(),
            "book rows must be root-visible"
        );
    }
}
