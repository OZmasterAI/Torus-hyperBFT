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

    /// Store a consumed value. Backends that retain writes may take ownership
    /// of its allocation; the default preserves the borrowed write contract.
    fn put_cf_raw_owned(&self, cf: &str, key: &[u8], value: Vec<u8>) -> Result<(), StateError> {
        self.put_cf_raw(cf, key, &value)
    }

    fn delete_cf_raw(&self, cf: &str, key: &[u8]) -> Result<(), StateError>;

    /// Iterate entries in a column family. `prefix: Some(p)` returns only keys
    /// starting with `p`; `None` returns all entries. Results are in sorted key order.
    #[allow(clippy::type_complexity)]
    fn iterate_cf(
        &self,
        cf: &str,
        prefix: Option<&[u8]>,
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>, StateError>;

    /// Review H3 (s517): the first `limit` live entries with key `>= start`, in
    /// key order — a bounded seek (`StateDb` and `NativeStateOverlay` never
    /// materialise the CF). Equals `iterate_cf(cf, None)` filtered to
    /// `k >= start`, truncated to `limit`.
    #[allow(clippy::type_complexity)]
    fn iterate_cf_from(
        &self,
        cf: &str,
        start: &[u8],
        limit: usize,
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>, StateError> {
        Ok(self
            .iterate_cf(cf, None)?
            .into_iter()
            .filter(|(k, _)| k.as_slice() >= start)
            .take(limit)
            .collect())
    }

    /// Whether any key starting with `prefix` exists — `!iterate_cf(prefix)
    /// .is_empty()`. `StateDb` and `NativeStateOverlay` stop at the first live
    /// key instead of loading every row.
    fn prefix_exists(&self, cf: &str, prefix: &[u8]) -> Result<bool, StateError> {
        Ok(!self.iterate_cf(cf, Some(prefix))?.is_empty())
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

    /// Review H3: RocksDB seek to `start`, at most `limit` rows read.
    fn iterate_cf_from(
        &self,
        cf: &str,
        start: &[u8],
        limit: usize,
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>, StateError> {
        let mut out = Vec::new();
        if limit == 0 {
            return Ok(out);
        }
        let db = self.inner();
        let cf_handle = db
            .cf_handle(cf)
            .ok_or_else(|| StateError::MissingColumnFamily(cf.to_string()))?;
        let mode = rocksdb::IteratorMode::From(start, rocksdb::Direction::Forward);
        for item in db.iterator_cf(cf_handle, mode) {
            let (key, value) = item?;
            out.push((key.to_vec(), value.to_vec()));
            if out.len() == limit {
                break;
            }
        }
        Ok(out)
    }

    /// First key at/after `prefix` only (keys are sorted: if it does not
    /// start with `prefix`, none does).
    fn prefix_exists(&self, cf: &str, prefix: &[u8]) -> Result<bool, StateError> {
        let db = self.inner();
        let cf_handle = db
            .cf_handle(cf)
            .ok_or_else(|| StateError::MissingColumnFamily(cf.to_string()))?;
        match db.prefix_iterator_cf(cf_handle, prefix).next() {
            Some(item) => Ok(item?.0.starts_with(prefix)),
            None => Ok(false),
        }
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
    /// Running state hash: consensus writes of this block that were made
    /// durable OUTSIDE its atomic batch (see [`HashExtras`]). Hashed under this
    /// set's own writes, never read through and never written by the flush.
    hash_extras: Option<Box<PendingState>>,
}

impl PendingState {
    fn new() -> Self {
        Self {
            cfs: std::array::from_fn(|_| CfPending::default()),
            journal_log: Vec::new(),
            checkpoints: Vec::new(),
            frozen: false,
            hash_extras: None,
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
        self.hash_extras = None;
    }

    /// Running state hash: fold `other`'s hashed-CF mutations on top of this
    /// set (later wins, as when `other` became durable after it).
    fn absorb_hashed(&mut self, other: &PendingState) {
        for &(_, id) in hashed_cf_order() {
            let src = other.cf(id);
            let dst = self.cf_mut(id);
            for (k, v) in &src.writes {
                dst.deletes.remove(k);
                dst.writes.insert(k.clone(), v.clone());
            }
            for k in &src.deletes {
                dst.writes.remove(k);
                dst.deletes.insert(k.clone());
            }
        }
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

/// Running state hash: the hashed CFs as `(frozen hash cf_id, CfId)`, in
/// ascending `cf_id` order — the canonical iteration order of the hash.
fn hashed_cf_order() -> &'static [(u8, CfId)] {
    static ORDER: OnceLock<Vec<(u8, CfId)>> = OnceLock::new();
    ORDER.get_or_init(|| {
        let mut v: Vec<(u8, CfId)> = crate::running_hash::HASHED_CFS
            .iter()
            .map(|(id, name)| (*id, intern_cf(name).expect("hashed CF is registered")))
            .collect();
        v.sort_by_key(|(id, _)| *id);
        v
    })
}

/// One CF's pending mutations as a single key-sorted stream. Writes and
/// tombstones are disjoint by invariant; should a key ever be in both, the
/// tombstone is reported (the batch appends deletes after puts, so it wins).
fn cf_stream<'a>(c: &'a CfPending) -> impl Iterator<Item = (&'a [u8], Option<&'a [u8]>)> + 'a {
    let mut w = c.writes.iter().peekable();
    let mut d = c.deletes.iter().peekable();
    std::iter::from_fn(move || {
        let take_write = match (w.peek(), d.peek()) {
            (None, None) => return None,
            (Some(_), None) => true,
            (None, Some(_)) => false,
            (Some((wk, _)), Some(dk)) => wk.as_slice() < dk.as_slice(),
        };
        if take_write {
            let (k, v) = w.next()?;
            Some((k.as_slice(), Some(v.as_slice())))
        } else {
            let k = d.next()?;
            if w.peek().is_some_and(|(wk, _)| *wk == k) {
                w.next();
            }
            Some((k.as_slice(), None))
        }
    })
}

/// Running state hash: visit the consensus write set of `layers` (lowest
/// priority first — a later layer's entry for a key overrides an earlier
/// one, the order the writes became durable in) in canonical
/// `(cf_id, key)` order, key filters applied. Node-local / derived CFs and
/// keys never reach `f`. A CF written in several layers is a streaming k-way
/// merge of the layers' sorted streams (k <= 3: extras, pending set,
/// sidecar) — nothing is materialized.
fn for_each_consensus_write<'a>(
    layers: &[&'a PendingState],
    mut f: impl FnMut(u8, &'a [u8], Option<&'a [u8]>),
) {
    use crate::running_hash::key_is_hashed;
    for &(cf_id, id) in hashed_cf_order() {
        let mut streams: Vec<_> = layers
            .iter()
            .map(|l| l.cf(id))
            .filter(|c| !c.is_empty())
            .map(|c| cf_stream(c).peekable())
            .collect();
        loop {
            // Smallest head key; on ties the LAST (highest-priority) layer's
            // entry wins and every layer holding the key advances.
            let mut best: Option<(&'a [u8], Option<&'a [u8]>)> = None;
            for s in streams.iter_mut() {
                if let Some(&(k, v)) = s.peek() {
                    if best.is_none_or(|(bk, _)| k <= bk) {
                        best = Some((k, v));
                    }
                }
            }
            let Some((k, v)) = best else { break };
            for s in streams.iter_mut() {
                if s.peek().is_some_and(|(sk, _)| *sk == k) {
                    s.next();
                }
            }
            if key_is_hashed(cf_id, k) {
                f(cf_id, k, v);
            }
        }
    }
}

/// Running state hash: a block's consensus writes that became durable OUTSIDE
/// its atomic flush batch (Task 0: buffered slashes, EVM writer-precompile
/// side effects, the EVM bundle commit, the epoch-boundary staking snapshot).
/// Attached to the block's pending set ([`NativeStateOverlay::set_hash_extras`],
/// [`FrozenPending::with_hash_extras`]) they enter `D(n)` UNDER the block's own
/// overlay writes (the overlay is flushed later, so it wins per key). Hash-only:
/// never written, never read through. Writes to CFs outside the hashed set are
/// dropped on entry.
pub struct HashExtras {
    state: PendingState,
}

impl Default for HashExtras {
    fn default() -> Self {
        Self::new()
    }
}

impl HashExtras {
    pub fn new() -> Self {
        Self {
            state: PendingState::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.state.is_empty()
    }

    fn hashed_id(cf: &str) -> Option<CfId> {
        intern_cf(cf).filter(|id| hashed_cf_order().iter().any(|(_, h)| h == id))
    }

    /// Record a put (later calls override earlier ones per key).
    pub fn put(&mut self, cf: &str, key: &[u8], value: &[u8]) {
        if let Some(id) = Self::hashed_id(cf) {
            let c = self.state.cf_mut(id);
            c.deletes.remove(key);
            c.writes.insert(key.to_vec(), value.to_vec());
        }
    }

    /// Record a deletion.
    pub fn delete(&mut self, cf: &str, key: &[u8]) {
        if let Some(id) = Self::hashed_id(cf) {
            let c = self.state.cf_mut(id);
            c.writes.remove(key);
            c.deletes.insert(key.to_vec());
        }
    }

    /// Fold `later` on top (its entries win per key).
    pub fn extend(&mut self, later: HashExtras) {
        self.state.absorb_hashed(&later.state);
    }

    /// Fold `overlay`'s pending writes on top: an EVM block's block-scoped
    /// writer-precompile journal (consensus bug (c)) — the same entries the
    /// per-tx `commit_tx` used to record while armed.
    pub fn add_overlay_pending(&mut self, overlay: &NativeStateOverlay) {
        let pending = overlay.pending.read().unwrap();
        self.state.absorb_hashed(&pending);
    }

    /// The EVM bundle's plain-state change set — exactly the puts / deletes
    /// [`crate::incremental::apply_bundle_plain`] (and the fallback
    /// `commit_pending_bundle`) write to `cf_accounts` / `cf_storage` /
    /// `cf_code` (pinned by `evm_bundle_extras_match_apply_bundle_plain`).
    pub fn add_evm_bundle(&mut self, bundle: &revm::database::BundleState) {
        use crate::cf::{CF_ACCOUNTS, CF_CODE, CF_STORAGE};
        use crate::db::{encode_account_info, storage_key};
        for (address, acct) in &bundle.state {
            match &acct.info {
                Some(info) => {
                    self.put(CF_ACCOUNTS, address.as_slice(), &encode_account_info(info));
                    for (slot, sv) in &acct.storage {
                        let key = storage_key(address, slot);
                        if sv.present_value.is_zero() {
                            self.delete(CF_STORAGE, &key);
                        } else {
                            self.put(CF_STORAGE, &key, &sv.present_value.to_be_bytes::<32>());
                        }
                    }
                }
                None => {
                    if acct.original_info.is_some() {
                        self.delete(CF_ACCOUNTS, address.as_slice());
                    }
                    for slot in acct.storage.keys() {
                        self.delete(CF_STORAGE, &storage_key(address, slot));
                    }
                }
            }
        }
        for (code_hash, bytecode) in &bundle.contracts {
            self.put(CF_CODE, code_hash.as_slice(), bytecode.bytes().as_ref());
        }
    }
}

thread_local! {
    static OUT_OF_BATCH: std::cell::RefCell<Option<HashExtras>> =
        const { std::cell::RefCell::new(None) };
}

/// Running state hash: while armed on a thread, every successful
/// [`NativeStateOverlay::commit_tx`] on that thread (EVM writer-precompile
/// side effects, the buffered-slash loop) is also recorded, in durable order,
/// as [`HashExtras`]. The exec thread arms it around the block's EVM / slash
/// section; other threads never record. Disarmed on drop.
pub struct OutOfBatchRecorder {
    _thread_bound: std::marker::PhantomData<*const ()>,
}

impl OutOfBatchRecorder {
    pub fn begin() -> Self {
        OUT_OF_BATCH.with(|r| *r.borrow_mut() = Some(HashExtras::new()));
        Self {
            _thread_bound: std::marker::PhantomData,
        }
    }

    /// Stop recording and return what was recorded.
    pub fn finish(self) -> HashExtras {
        OUT_OF_BATCH.with(|r| r.borrow_mut().take()).unwrap_or_default()
    }
}

impl Drop for OutOfBatchRecorder {
    fn drop(&mut self) {
        OUT_OF_BATCH.with(|r| *r.borrow_mut() = None);
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

    /// Attach the block's out-of-batch consensus writes (see [`HashExtras`]).
    pub fn with_hash_extras(mut self, extras: HashExtras) -> Self {
        self.state.hash_extras = (!extras.is_empty()).then(|| Box::new(extras.state));
        self
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
        self.flush_with_sidecar_native_trie_stats(
            None,
            target,
            applied_height,
            trie_cache,
            member_cache,
        )
    }

    /// Mirror of [`NativeStateOverlay::flush_after_batch_with_native_trie_stats`]
    /// (consensus bug (c): an EVM-only block's bundle rides its marker flush).
    pub fn flush_after_batch_with_native_trie_stats(
        &self,
        prefix: WriteBatch,
        target: &StateDb,
        applied_height: Option<u64>,
        trie_cache: Option<&mut crate::native_trie::NativeTrieCache>,
        member_cache: Option<&mut crate::native_trie::NativeMemberCache>,
    ) -> Result<NativeFlushStats, StateError> {
        flush_pending_after_batch(
            prefix,
            &self.state,
            None,
            target,
            applied_height,
            trie_cache,
            member_cache,
            crate::native_trie::native_trie_maintenance_enabled(),
        )
    }

    /// Deferred book save (port of item 6a): [`Self::flush_with_native_trie_stats`]
    /// with an optional SIDECAR pending set folded in — the flush worker's
    /// worker-side book writes. The sidecar's entries join the SAME atomic batch
    /// and the SAME native-root dirty set as this frozen set's own entries, so
    /// the persisted bytes, the state root and the marker atomicity are exactly
    /// what the serial save-on-exec-thread would have produced. The sidecar is
    /// appended AFTER the main set (and overrides it in the dirty map) — in
    /// practice the key sets are disjoint: under deferral the exec thread writes
    /// NO book-CF key into its overlay. Neither set is mutated (`&self`).
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
            crate::native_trie::native_trie_maintenance_enabled(),
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

impl std::fmt::Debug for NativeStateOverlay {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeStateOverlay")
            .field("pending_writes", &self.pending_write_count())
            .field("parent_height", &self.parent_height())
            .finish()
    }
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

    /// Attach the block's out-of-batch consensus writes (see [`HashExtras`]);
    /// they travel with the pending set through [`Self::freeze`].
    pub fn set_hash_extras(&self, extras: HashExtras) {
        let mut state = self.pending.write().unwrap();
        debug_assert!(!state.frozen, "hash extras set on a frozen overlay");
        state.hash_extras = (!extras.is_empty()).then(|| Box::new(extras.state));
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
        OUT_OF_BATCH.with(|r| {
            if let Some(acc) = r.borrow_mut().as_mut() {
                acc.state.absorb_hashed(&state);
            }
        });
        state.clear();
        Ok(())
    }

    /// T4.4: Drop all pending writes without persisting them — the per-transaction
    /// REVERT for writer-precompile side effects (calling EVM tx reverted or halted).
    pub fn discard_tx(&self) {
        let mut state = self.pending.write().unwrap();
        state.clear();
    }

    /// F1 (s515, ported from `fix/parity-audit-bugs` b5ef142): open a transaction
    /// scope inside a BLOCK-scoped journal. Pending writes of earlier transactions
    /// stay; everything written from here on is undo-logged so
    /// [`revert_tx`](Self::revert_tx) can drop exactly this tx's writes. Resets the
    /// frame checkpoint stack (it must not leak across txs).
    pub fn begin_tx(&self) {
        let mut state = self.pending.write().unwrap();
        state.journal_log.clear();
        state.checkpoints.clear();
        state.checkpoints.push(0);
    }

    /// F1 (s515): close the transaction scope KEEPING its writes pending in the
    /// block journal (nothing is persisted — the block commit writes them).
    pub fn keep_tx(&self) {
        let mut state = self.pending.write().unwrap();
        state.journal_log.clear();
        state.checkpoints.clear();
    }

    /// F1 (s515): close the transaction scope DROPPING its writes; earlier
    /// transactions' pending writes are untouched.
    pub fn revert_tx(&self) {
        {
            let mut state = self.pending.write().unwrap();
            state.checkpoints.clear();
            state.checkpoints.push(0);
        }
        self.revert_to_checkpoint();
    }

    /// F1 (s515): append every pending write/delete to `batch` WITHOUT writing or
    /// clearing — lets a caller persist this overlay in the same atomic
    /// `WriteBatch` as other state (an EVM block's writer-precompile queue rows
    /// ride the block's batch, consensus bug (c)).
    pub fn append_pending_to_batch(
        &self,
        target: &StateDb,
        batch: &mut WriteBatch,
    ) -> Result<(), StateError> {
        let state = self.pending.read().unwrap();
        state.append_to_batch(target.inner(), batch)
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
    /// (`Some`) and deletes (`None`) hitting the 7 native-root CFs only (mirrors `dirty_evm_accounts`).
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
        self.flush_after_batch_with_native_trie_stats(
            WriteBatch::default(),
            target,
            applied_height,
            trie_cache,
            member_cache,
        )
    }

    /// [`Self::flush_with_native_trie_stats`] whose ONE atomic write starts
    /// with `prefix` (consensus bug (c): the block's EVM bundle + writer rows),
    /// so the prefix is durable iff this block's native state and applied-height
    /// marker are. The overlay's writes follow the prefix in the batch (a later
    /// op on the same key wins), exactly as when the prefix was written first.
    /// Not hashed: the caller records the prefix's consensus writes as
    /// [`HashExtras`].
    pub fn flush_after_batch_with_native_trie_stats(
        &self,
        prefix: WriteBatch,
        target: &StateDb,
        applied_height: Option<u64>,
        trie_cache: Option<&mut crate::native_trie::NativeTrieCache>,
        member_cache: Option<&mut crate::native_trie::NativeMemberCache>,
    ) -> Result<NativeFlushStats, StateError> {
        let state = self.pending.read().unwrap();
        flush_pending_after_batch(
            prefix,
            &state,
            None,
            target,
            applied_height,
            trie_cache,
            member_cache,
            crate::native_trie::native_trie_maintenance_enabled(),
        )
    }
}

/// Running state hash: blocks with at least this many pending entries digest
/// on a scoped thread, overlapped with the batch build (below it, inline).
const PARALLEL_DIGEST_MIN_ENTRIES: usize = 4096;

/// Running state hash: digest of a block's consensus write set over `layers`
/// (see [`for_each_consensus_write`]) and its compute seconds; with the test
/// capture armed, also the materialized write set.
fn block_digest(
    layers: &[&PendingState],
) -> (crate::running_hash::BlockDigest, Option<Vec<crate::running_hash::WriteEntry>>, f64) {
    let timer = std::time::Instant::now();
    let mut digest = crate::running_hash::BlockDigest::new();
    let mut capture = crate::running_hash::capture_active().then(Vec::new);
    for_each_consensus_write(layers, |id, k, v| {
        digest.push(id, k, v);
        if let Some(c) = capture.as_mut() {
            c.push((id, k.to_vec(), v.map(<[u8]>::to_vec)));
        }
    });
    (digest, capture, timer.elapsed().as_secs_f64())
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
    maintain_trie: bool,
) -> Result<NativeFlushStats, StateError> {
    flush_pending_after_batch(
        WriteBatch::default(),
        state,
        sidecar,
        target,
        applied_height,
        trie_cache,
        member_cache,
        maintain_trie,
    )
}

/// [`flush_pending_with_native_trie_stats`] whose batch starts with `prefix`
/// (see [`NativeStateOverlay::flush_after_batch_with_native_trie_stats`]).
#[allow(clippy::too_many_arguments)]
fn flush_pending_after_batch(
    prefix: WriteBatch,
    state: &PendingState,
    sidecar: Option<&PendingState>,
    target: &StateDb,
    applied_height: Option<u64>,
    trie_cache: Option<&mut crate::native_trie::NativeTrieCache>,
    member_cache: Option<&mut crate::native_trie::NativeMemberCache>,
    maintain_trie: bool,
) -> Result<NativeFlushStats, StateError> {
    std::thread::scope(|scope| {
        let raw = target.inner();

        // Running state hash: h_n is a digest of the block's consensus write
        // set — out-of-batch extras, then the pending set, then the
        // deferred-book sidecar on top — computed from the pending maps, never
        // from the batch, so trie maintenance (on / off / failed) cannot change
        // it. Large blocks digest on a scoped thread while this one builds the
        // batch (both only read the maps); joined below, before the hash joins
        // the SAME batch as the applied-height marker.
        // Chain-wide activation + validity (review findings 1 / 5): digest
        // only when this height extends a valid chain.
        let step = applied_height.map(|h| (h, crate::running_hash::chain_step(target, h)));
        let hashing = matches!(
            step,
            Some((_, crate::running_hash::ChainStep::Start | crate::running_hash::ChainStep::Continue(_)))
        );
        let digest_job = hashing.then(|| {
            let mut layers: Vec<&PendingState> = Vec::with_capacity(3);
            layers.extend(state.hash_extras.as_deref());
            layers.push(state);
            layers.extend(sidecar);
            let entries: usize = layers
                .iter()
                .flat_map(|l| l.cfs.iter())
                .map(|c| c.writes.len() + c.deletes.len())
                .sum();
            if entries >= PARALLEL_DIGEST_MIN_ENTRIES {
                Err(scope.spawn(move || block_digest(&layers)))
            } else {
                Ok(block_digest(&layers))
            }
        });

        let build_timer = std::time::Instant::now();
        let mut batch = prefix;
        state.append_to_batch(raw, &mut batch)?;
        // Deferred book save: the sidecar (worker-side book writes) joins the
        // SAME atomic batch, appended after the main set (last-wins on the — in
        // practice disjoint — key sets).
        if let Some(side) = sidecar {
            side.append_to_batch(raw, &mut batch)?;
        }

        // Native-root dirty map, folded into the SAME batch. BORROWED from the
        // pending maps (s450): `state`'s read guard is held for this whole
        // function and the trie only reads the set, so nothing is copied out.
        let mut dirty = state.native_dirty_ref();
        // Deferred book save: the sidecar's root-CF entries enter the ROOT
        // computation exactly as if the exec thread had written them into its
        // overlay (same canonical (tag, key) order — BTreeMap merge).
        if let Some(side) = sidecar {
            dirty.extend(side.native_dirty_ref());
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
            dirty_entries_by_cf: [0; 7],
            state_hash_seconds: 0.0,
            state_hash_entries: 0,
        };
        // 3c funnel attribution: dirty-entry composition per cf_tag.
        for (tag, _) in dirty.keys() {
            if let Some(slot) = stats.dirty_entries_by_cf.get_mut(*tag as usize) {
                *slot += 1;
            }
        }
        let trie_result = if dirty.is_empty() {
            Ok(())
        } else if !maintain_trie {
            // s83 Option 0 (`TORUS_NATIVE_TRIE_MAINTENANCE=0`): no trie/mirror ops; instead mark
            // the trie stale in the SAME batch, so state, marker and sentinel commit together. The
            // caches are left untouched — they are only reachable through `apply_native_dirty`,
            // which the once-per-process mode never calls, and a boot rebuild moves the persisted
            // root they self-authenticate against.
            let cf = raw.cf_handle(crate::cf::CF_CONSENSUS_META).ok_or_else(|| {
                StateError::MissingColumnFamily(crate::cf::CF_CONSENSUS_META.to_string())
            })?;
            batch.put_cf(cf, crate::cf::META_NATIVE_TRIE_STALE, [1u8]);
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

        // Running state hash: join the digest and append h_n (+ checkpoint),
        // or the unverified marker, to the batch.
        if let Some((height, step)) = step {
            let hash = match digest_job {
                Some(job) => {
                    let (digest, capture, digest_secs) = match job {
                        Ok(done) => done,
                        Err(handle) => handle.join().map_err(|_| {
                            StateError::InvalidData("running state hash digest thread panicked".into())
                        })?,
                    };
                    stats.state_hash_entries = digest.entries();
                    stats.state_hash_seconds = digest_secs;
                    let prev = match step {
                        crate::running_hash::ChainStep::Continue(prev) => prev,
                        _ => [0; 32],
                    };
                    if let Some(c) = capture {
                        crate::running_hash::capture_record(target, height, c);
                    }
                    Some(digest.chain(&prev, height))
                }
                None => None,
            };
            crate::running_hash::append_to_batch(target, &mut batch, height, step, hash.as_ref())?;
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
                    (Ok(()), None) => {} // empty dirty set / maintenance skipped — trie untouched
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
    })
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
    pub dirty_entries_by_cf: [usize; 7],
    /// Running state hash: time to digest the block's consensus write set (0
    /// when the flush carries no applied height). Large blocks digest on a
    /// scoped thread overlapped with the batch build, so this is compute time,
    /// not necessarily added flush wall time.
    pub state_hash_seconds: f64,
    /// Running state hash: hashed entries this flush.
    pub state_hash_entries: usize,
}

/// A layer's pending-write keys starting with `prefix`, in key order.
fn writes_under<'a>(
    writes: &'a BTreeMap<Vec<u8>, Vec<u8>>,
    prefix: &'a [u8],
) -> impl Iterator<Item = &'a Vec<u8>> {
    writes
        .range::<[u8], _>((std::ops::Bound::Included(prefix), std::ops::Bound::Unbounded))
        .map(|(k, _)| k)
        .take_while(move |k| k.starts_with(prefix))
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

    fn put_cf_raw_owned(&self, cf: &str, key: &[u8], value: Vec<u8>) -> Result<(), StateError> {
        let id = intern_cf(cf).ok_or_else(|| StateError::MissingColumnFamily(cf.to_string()))?;
        let mut state = self.pending.write().unwrap();
        if state.frozen {
            return Err(PendingState::frozen_err());
        }
        state.record(id, key);
        let cfp = state.cf_mut(id);
        cfp.deletes.remove(key);
        cfp.writes.insert(key.to_vec(), value);
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

    /// Review H3: the merged `iterate_cf` from `start`, without materialising
    /// it — a k-way merge of the DB iterator (seeked to `start`) and the
    /// parent / pending write ranges; each candidate key resolves by the
    /// layered point-read rule (pending, then parent, then DB), so tombstones
    /// in either layer hide it. Reads at most `limit` live rows plus the
    /// tombstoned DB keys in between.
    fn iterate_cf_from(
        &self,
        cf: &str,
        start: &[u8],
        limit: usize,
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>, StateError> {
        let Some(id) = intern_cf(cf) else {
            return StateBackend::iterate_cf_from(&self.db, cf, start, limit);
        };
        let mut out = Vec::new();
        if limit == 0 {
            return Ok(out);
        }
        let range = (std::ops::Bound::Included(start), std::ops::Bound::Unbounded);
        let pending = self.pending.read().unwrap();
        let parent = self.parent.as_ref().map(|p| &p.state);
        let mut pw = pending.cf(id).writes.range::<[u8], _>(range).peekable();
        let mut aw = parent
            .into_iter()
            .flat_map(|s| s.cf(id).writes.range::<[u8], _>(range))
            .peekable();
        let db = self.db.inner();
        let cf_handle = db
            .cf_handle(cf)
            .ok_or_else(|| StateError::MissingColumnFamily(cf.to_string()))?;
        let mut dbi = db.iterator_cf(
            cf_handle,
            rocksdb::IteratorMode::From(start, rocksdb::Direction::Forward),
        );
        let mut dbh = dbi.next().transpose()?;
        while out.len() < limit {
            let heads = [
                pw.peek().map(|(k, _)| k.as_slice()),
                aw.peek().map(|(k, _)| k.as_slice()),
                dbh.as_ref().map(|(k, _)| &**k),
            ];
            let Some(key) = heads.into_iter().flatten().min().map(<[u8]>::to_vec) else {
                break;
            };
            let value = match pending.lookup(id, &key) {
                Some(hit) => hit.map(<[u8]>::to_vec),
                None => match parent.and_then(|s| s.lookup(id, &key)) {
                    Some(hit) => hit.map(<[u8]>::to_vec),
                    None => dbh
                        .as_ref()
                        .filter(|(k, _)| **k == *key)
                        .map(|(_, v)| v.to_vec()),
                },
            };
            if pw.peek().is_some_and(|(k, _)| **k == key) {
                pw.next();
            }
            if aw.peek().is_some_and(|(k, _)| **k == key) {
                aw.next();
            }
            if dbh.as_ref().is_some_and(|(k, _)| **k == *key) {
                dbh = dbi.next().transpose()?;
            }
            if let Some(v) = value {
                out.push((key, v));
            }
        }
        Ok(out)
    }

    /// Same answer as the merged `iterate_cf`, without materialising it: a key
    /// is live per the layered point-read rule (pending, then parent, then DB).
    /// Checks pending / parent writes under `prefix`, then walks DB keys and
    /// stops at the first one no layer tombstones — it skips at most the
    /// tombstones under `prefix`.
    fn prefix_exists(&self, cf: &str, prefix: &[u8]) -> Result<bool, StateError> {
        let Some(id) = intern_cf(cf) else {
            return StateBackend::prefix_exists(&self.db, cf, prefix);
        };
        let live = |key: &[u8]| -> bool {
            if let Some(hit) = self.pending.read().unwrap().lookup(id, key) {
                return hit.is_some();
            }
            match self.parent.as_ref().and_then(|p| p.state.lookup(id, key)) {
                Some(hit) => hit.is_some(),
                None => true,
            }
        };
        if writes_under(&self.pending.read().unwrap().cf(id).writes, prefix).next().is_some() {
            return Ok(true);
        }
        if let Some(parent) = &self.parent {
            if writes_under(&parent.state.cf(id).writes, prefix).any(|k| live(k)) {
                return Ok(true);
            }
        }
        let db = self.db.inner();
        let cf_handle = db
            .cf_handle(cf)
            .ok_or_else(|| StateError::MissingColumnFamily(cf.to_string()))?;
        for item in db.prefix_iterator_cf(cf_handle, prefix) {
            let (key, _) = item?;
            if !key.starts_with(prefix) {
                break;
            }
            if live(&key) {
                return Ok(true);
            }
        }
        Ok(false)
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

    /// `prefix_exists` == `!iterate_cf(prefix).is_empty()` for every layering
    /// of two keys over DB / parent (frozen) / pending: absent, write, tombstone.
    #[test]
    fn prefix_exists_matches_iterate_cf_over_every_layering() {
        let (db, _dir) = temp_db();
        let cf = CF_NATIVE_BALANCES;
        // per key: (in DB, parent op, pending op); op 0 = none, 1 = write, 2 = delete
        let states: Vec<(bool, u8, u8)> = (0..18u8).map(|i| (i % 2 == 1, (i / 2) % 3, i / 6)).collect();
        let prefix = |c: usize| [b'p', (c >> 8) as u8, c as u8].to_vec();
        let key = |c: usize, k: u8| [prefix(c), vec![k]].concat();
        let combos: Vec<[(bool, u8, u8); 2]> =
            states.iter().flat_map(|&a| states.iter().map(move |&b| [a, b])).collect();
        for (c, keys) in combos.iter().enumerate() {
            for (k, &(in_db, _, _)) in keys.iter().enumerate() {
                if in_db {
                    db.put_cf_raw(cf, &key(c, k as u8), b"db").unwrap();
                }
            }
        }
        let apply = |ov: &NativeStateOverlay, layer: usize| {
            for (c, keys) in combos.iter().enumerate() {
                for (k, st) in keys.iter().enumerate() {
                    match if layer == 0 { st.1 } else { st.2 } {
                        1 => ov.put_cf_raw(cf, &key(c, k as u8), b"w").unwrap(),
                        2 => ov.delete_cf_raw(cf, &key(c, k as u8)).unwrap(),
                        _ => {}
                    }
                }
            }
        };
        let parent = NativeStateOverlay::new(db.clone());
        apply(&parent, 0);
        let overlay = NativeStateOverlay::with_parent(db.clone(), Some(parent.freeze(1)));
        apply(&overlay, 1);
        let (mut yes, mut no) = (0, 0);
        for (c, combo) in combos.iter().enumerate() {
            let p = prefix(c);
            let want = !overlay.iterate_cf(cf, Some(&p)).unwrap().is_empty();
            assert_eq!(overlay.prefix_exists(cf, &p).unwrap(), want, "overlay combo {c}: {combo:?}");
            let want_db = !StateBackend::iterate_cf(&db, cf, Some(&p)).unwrap().is_empty();
            assert_eq!(StateBackend::prefix_exists(&db, cf, &p).unwrap(), want_db, "db combo {c}");
            if want { yes += 1 } else { no += 1 }
        }
        assert!(yes > 0 && no > 0, "non-vacuous: {yes} / {no}");
        assert!(!overlay.prefix_exists(cf, b"q").unwrap(), "no key under the prefix");
    }

    /// Review H3 (s517): `iterate_cf_from(start, limit)` == the first `limit`
    /// entries of the merged `iterate_cf` with key >= `start`, for every
    /// layering of two keys over DB / parent (frozen) / pending (absent, write,
    /// tombstone), many starts (before / on / between / after keys) and
    /// limits — on the overlay and on the bare DB.
    #[test]
    fn iterate_cf_from_matches_iterate_cf_over_every_layering() {
        let (db, _dir) = temp_db();
        let cf = CF_NATIVE_BALANCES;
        let states: Vec<(bool, u8, u8)> = (0..18u8).map(|i| (i % 2 == 1, (i / 2) % 3, i / 6)).collect();
        let prefix = |c: usize| [b'p', (c >> 8) as u8, c as u8].to_vec();
        let key = |c: usize, k: u8| [prefix(c), vec![k * 2 + 1]].concat();
        let combos: Vec<[(bool, u8, u8); 2]> =
            states.iter().flat_map(|&a| states.iter().map(move |&b| [a, b])).collect();
        for (c, keys) in combos.iter().enumerate() {
            for (k, &(in_db, _, _)) in keys.iter().enumerate() {
                if in_db {
                    db.put_cf_raw(cf, &key(c, k as u8), &[b'd', c as u8, k as u8]).unwrap();
                }
            }
        }
        let apply = |ov: &NativeStateOverlay, layer: u8| {
            for (c, keys) in combos.iter().enumerate() {
                for (k, st) in keys.iter().enumerate() {
                    match if layer == 0 { st.1 } else { st.2 } {
                        1 => ov.put_cf_raw(cf, &key(c, k as u8), &[b'w', layer, c as u8, k as u8]).unwrap(),
                        2 => ov.delete_cf_raw(cf, &key(c, k as u8)).unwrap(),
                        _ => {}
                    }
                }
            }
        };
        let parent = NativeStateOverlay::new(db.clone());
        apply(&parent, 0);
        let overlay = NativeStateOverlay::with_parent(db.clone(), Some(parent.freeze(1)));
        apply(&overlay, 1);
        let all = overlay.iterate_cf(cf, None).unwrap();
        let all_db = StateBackend::iterate_cf(&db, cf, None).unwrap();
        assert!(!all.is_empty() && all.len() < combos.len() * 2, "non-vacuous");
        let expect = |rows: &[(Vec<u8>, Vec<u8>)], start: &[u8], limit: usize| -> Vec<(Vec<u8>, Vec<u8>)> {
            rows.iter().filter(|(k, _)| k.as_slice() >= start).take(limit).cloned().collect()
        };
        let mut starts: Vec<Vec<u8>> = vec![Vec::new(), b"q".to_vec()];
        for c in (0..combos.len()).step_by(7) {
            for k in 0..5u8 {
                starts.push([prefix(c), vec![k]].concat()); // before / on / between keys
            }
        }
        for start in &starts {
            for limit in [0usize, 1, 2, 3, 17, usize::MAX] {
                assert_eq!(
                    overlay.iterate_cf_from(cf, start, limit).unwrap(),
                    expect(&all, start, limit),
                    "overlay start {start:?} limit {limit}"
                );
                assert_eq!(
                    StateBackend::iterate_cf_from(&db, cf, start, limit).unwrap(),
                    expect(&all_db, start, limit),
                    "db start {start:?} limit {limit}"
                );
            }
        }
    }

    #[test]
    fn owned_put_retains_value_allocation_through_freeze() {
        let (db, _dir) = temp_db();
        let overlay = NativeStateOverlay::new(db);
        let cf = CF_NATIVE_BALANCES;
        let id = intern_cf(cf).unwrap();
        let mut key = b"owned".to_vec();
        let mut value = Vec::with_capacity(4096);
        value.extend_from_slice(b"serialized-body");
        let allocation = (value.as_ptr(), value.capacity());
        overlay.put_cf_raw_owned(cf, &key, value).unwrap();
        key.fill(0); // key remains borrowed at the API and is still copied.
        {
            let state = overlay.pending.read().unwrap();
            let stored = state.cf(id).writes.get(b"owned".as_slice()).unwrap();
            assert_eq!((stored.as_ptr(), stored.capacity()), allocation);
            assert_eq!(stored, b"serialized-body");
        }
        let frozen = overlay.freeze(1);
        let stored = frozen.state.cf(id).writes.get(b"owned".as_slice()).unwrap();
        assert_eq!((stored.as_ptr(), stored.capacity()), allocation);
    }

    #[test]
    fn owned_put_mixed_writes_deletes_and_nested_rollback_match_borrowed() {
        let (db, _dir) = temp_db();
        let cf = CF_NATIVE_BALANCES;
        db.put_cf_raw(cf, b"base", b"database").unwrap();
        let borrowed = NativeStateOverlay::new(db.clone());
        let mixed = NativeStateOverlay::new(db);
        for (overlay, owned) in [(&borrowed, false), (&mixed, true)] {
            let put = |key: &[u8], value: &[u8]| {
                if owned {
                    overlay.put_cf_raw_owned(cf, key, value.to_vec()).unwrap();
                } else {
                    overlay.put_cf_raw(cf, key, value).unwrap();
                }
            };
            overlay.put_cf_raw(cf, b"old", b"original").unwrap();
            overlay.delete_cf_raw(cf, b"base").unwrap();
            put(b"retained", b"owned-before-checkpoint");
            overlay.checkpoint();
            put(b"old", b"overwrite");
            put(b"base", b"resurrect");
            overlay.delete_cf_raw(cf, b"retained").unwrap();
            put(b"retained", b"resurrect-owned");
            overlay.checkpoint();
            overlay
                .put_cf_raw(cf, b"old", b"borrowed-overwrite")
                .unwrap();
            put(b"fresh", b"inner");
            overlay.commit_checkpoint();
            overlay.revert_to_checkpoint();
            assert_eq!(
                overlay.get_cf_raw(cf, b"old").unwrap(),
                Some(b"original".to_vec())
            );
            assert_eq!(
                overlay.get_cf_raw(cf, b"retained").unwrap(),
                Some(b"owned-before-checkpoint".to_vec())
            );
            assert_eq!(overlay.get_cf_raw(cf, b"base").unwrap(), None);
            assert_eq!(overlay.get_cf_raw(cf, b"fresh").unwrap(), None);
            // Retain a successful owned overwrite and an empty value.
            overlay.checkpoint();
            put(b"old", b"committed");
            put(b"empty", b"");
            overlay.commit_checkpoint();
        }
        assert_eq!(
            borrowed.iterate_cf(cf, None).unwrap(),
            mixed.iterate_cf(cf, None).unwrap()
        );
        assert_eq!(borrowed.pending_write_count(), mixed.pending_write_count());
        mixed.discard_tx();
        assert_eq!(
            mixed.get_cf_raw(cf, b"base").unwrap(),
            Some(b"database".to_vec())
        );
        assert_eq!(mixed.get_cf_raw(cf, b"old").unwrap(), None);
    }

    #[test]
    fn owned_put_parent_reads_and_frozen_flush_match_borrowed_root_and_rows() {
        let (old_db, _old_dir) = temp_db();
        let (owned_db, _owned_dir) = temp_db();
        for (db, owned) in [(&old_db, false), (&owned_db, true)] {
            let cf = CF_NATIVE_BALANCES;
            db.put_cf_raw(cf, b"deleted", b"base").unwrap();
            crate::native_trie::build_native_trie_to_cf(db).unwrap();
            let put = |overlay: &NativeStateOverlay, key: &[u8], value: &[u8]| {
                if owned {
                    overlay.put_cf_raw_owned(cf, key, value.to_vec()).unwrap();
                } else {
                    overlay.put_cf_raw(cf, key, value).unwrap();
                }
            };
            let parent_overlay = NativeStateOverlay::new(db.clone());
            put(&parent_overlay, b"parent", b"previous-block");
            put(&parent_overlay, b"shadow", b"parent-value");
            parent_overlay.delete_cf_raw(cf, b"deleted").unwrap();
            let parent = parent_overlay.freeze(1);
            let child = NativeStateOverlay::with_parent(db.clone(), Some(parent.clone()));
            assert_eq!(
                child.get_cf_raw(cf, b"parent").unwrap(),
                Some(b"previous-block".to_vec())
            );
            assert_eq!(child.get_cf_raw(cf, b"deleted").unwrap(), None);
            child.checkpoint();
            put(&child, b"parent", b"reverted-child");
            child.revert_to_checkpoint();
            assert_eq!(
                child.get_cf_raw(cf, b"parent").unwrap(),
                Some(b"previous-block".to_vec())
            );
            put(&child, b"shadow", b"child-value");
            put(&child, b"deleted", b"revived");
            put(&child, b"empty", b"");
            child.delete_cf_raw(cf, b"parent").unwrap();
            parent
                .flush_with_native_trie_stats(db, Some(1), None, None)
                .unwrap();
            if owned {
                child
                    .freeze(2)
                    .flush_with_native_trie_stats(db, Some(2), None, None)
                    .unwrap();
            } else {
                child
                    .flush_with_native_trie_stats(db, Some(2), None, None)
                    .unwrap();
            }
            assert_eq!(
                crate::native_trie::persisted_native_root(db).unwrap(),
                crate::native_trie::native_root_full(db).unwrap()
            );
        }
        // Includes the state rows, trie/bucket rows and applied-height marker.
        for cf in crate::cf::ALL_CF_NAMES {
            assert_eq!(
                StateBackend::iterate_cf(&old_db, cf, None).unwrap(),
                StateBackend::iterate_cf(&owned_db, cf, None).unwrap(),
                "CF {cf}"
            );
        }
    }

    #[test]
    fn owned_put_default_delegation_and_overlay_errors_match_borrowed() {
        let (db, _dir) = temp_db();
        let cf = CF_NATIVE_BALANCES;
        // StateDb has no owned override: exercise the default trait delegation.
        StateBackend::put_cf_raw_owned(&db, cf, b"key", b"owned".to_vec()).unwrap();
        assert_eq!(db.get_cf_raw(cf, b"key").unwrap(), Some(b"owned".to_vec()));
        StateBackend::put_cf_raw_owned(&db, cf, b"key", vec![]).unwrap();
        assert_eq!(db.get_cf_raw(cf, b"key").unwrap(), Some(vec![]));
        let missing = "missing-owned-cf";
        assert_eq!(
            StateBackend::put_cf_raw_owned(&db, missing, b"k", vec![1])
                .unwrap_err()
                .to_string(),
            db.put_cf_raw(missing, b"k", &[1]).unwrap_err().to_string()
        );
        let overlay = NativeStateOverlay::new(db);
        for frozen in [false, true] {
            if frozen {
                overlay.freeze(1);
            }
            assert_eq!(
                overlay
                    .put_cf_raw_owned(missing, b"k", vec![1])
                    .unwrap_err()
                    .to_string(),
                overlay
                    .put_cf_raw(missing, b"k", &[1])
                    .unwrap_err()
                    .to_string()
            );
            if frozen {
                let clone = overlay.clone();
                assert_eq!(
                    clone
                        .put_cf_raw_owned(cf, b"k", vec![1])
                        .unwrap_err()
                        .to_string(),
                    overlay.put_cf_raw(cf, b"k", &[1]).unwrap_err().to_string()
                );
            }
            assert_eq!(overlay.pending_write_count(), 0);
        }
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

    /// F1 (s515, ported from b5ef142): block-scoped journal — a reverted tx
    /// drops only its own writes (including overwrites of an earlier tx's key);
    /// kept txs stay pending and nothing reaches the DB until the caller
    /// batches them.
    #[test]
    fn block_journal_tx_scopes_keep_and_revert() {
        let (db, _dir) = temp_db();
        let cf = CF_NATIVE_BALANCES;
        let overlay = NativeStateOverlay::new(db.clone());

        overlay.begin_tx();
        StateBackend::put_cf_raw(&overlay, cf, b"k1", b"a").unwrap();
        overlay.keep_tx();

        overlay.begin_tx();
        StateBackend::put_cf_raw(&overlay, cf, b"k1", b"b").unwrap();
        StateBackend::put_cf_raw(&overlay, cf, b"k2", b"c").unwrap();
        overlay.checkpoint(); // an inner frame left open must not leak
        overlay.revert_tx();

        let k1 = StateBackend::get_cf_raw(&overlay, cf, b"k1").unwrap();
        assert_eq!(k1.as_deref(), Some(&b"a"[..]));
        assert!(StateBackend::get_cf_raw(&overlay, cf, b"k2").unwrap().is_none());
        assert!(StateDb::get_cf_raw(&db, cf, b"k1").unwrap().is_none(), "nothing durable yet");

        let mut batch = WriteBatch::default();
        overlay.append_pending_to_batch(&db, &mut batch).unwrap();
        db.write(batch).unwrap();
        assert_eq!(StateDb::get_cf_raw(&db, cf, b"k1").unwrap().as_deref(), Some(&b"a"[..]));
        assert!(StateDb::get_cf_raw(&db, cf, b"k2").unwrap().is_none());
    }

    /// Consensus bug (c): a prefix batch rides the flush's ONE write — the
    /// overlay's writes win on a shared key (as when the prefix was written
    /// first) and the applied-height marker lands with it.
    #[test]
    fn flush_after_batch_lands_prefix_with_overlay_and_marker() {
        use crate::cf::{CF_ACCOUNTS, CF_CONSENSUS_META, META_NATIVE_APPLIED_HEIGHT};
        let (db, _dir) = temp_db();
        let prefix_with = |k: &[u8], v: &[u8]| {
            let mut b = WriteBatch::default();
            b.put_cf(db.cf_handle(CF_ACCOUNTS).unwrap(), k, v);
            b
        };
        let overlay = NativeStateOverlay::new(db.clone());
        StateBackend::put_cf_raw(&overlay, CF_ACCOUNTS, b"shared", b"native").unwrap();
        let mut prefix = prefix_with(b"shared", b"evm");
        prefix.put_cf(db.cf_handle(CF_ACCOUNTS).unwrap(), b"evm-only", b"x");
        overlay
            .flush_after_batch_with_native_trie_stats(prefix, &db, Some(7), None, None)
            .unwrap();
        let get = |cf, k: &[u8]| StateDb::get_cf_raw(&db, cf, k).unwrap();
        assert_eq!(get(CF_ACCOUNTS, b"shared").as_deref(), Some(&b"native"[..]));
        assert_eq!(get(CF_ACCOUNTS, b"evm-only").as_deref(), Some(&b"x"[..]));
        assert_eq!(
            get(CF_CONSENSUS_META, META_NATIVE_APPLIED_HEIGHT),
            Some(7u64.to_be_bytes().to_vec())
        );

        // Marker-only frozen set: same contract.
        FrozenPending::marker_only(8)
            .flush_after_batch_with_native_trie_stats(prefix_with(b"m", b"y"), &db, Some(8), None, None)
            .unwrap();
        assert_eq!(get(CF_ACCOUNTS, b"m").as_deref(), Some(&b"y"[..]));
        assert_eq!(
            get(CF_CONSENSUS_META, META_NATIVE_APPLIED_HEIGHT),
            Some(8u64.to_be_bytes().to_vec())
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

    /// s63 (port of item 6a, 4298728): flushing a frozen set WITH a sidecar
    /// (the flush worker's deferred book writes) is byte-identical — every CF,
    /// the persisted native root, the marker — to flushing ONE overlay holding
    /// the union of both write sets. The sidecar's root-CF entries really enter
    /// the root computation (negative control: dropping the sidecar changes
    /// it), and neither frozen set is mutated by the flush (read-only / Sync).
    #[test]
    fn sidecar_flush_identical_to_combined_overlay_flush() {
        use crate::cf::{
            CF_BOOK_ORDER_ROWS, CF_CONSENSUS_META, CF_NATIVE_ORDER_BOOKS, CF_NATIVE_POSITIONS,
            META_NATIVE_APPLIED_HEIGHT,
        };
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
            // Root CF (level rows / meta / stops)...
            for i in 0..8u32 {
                let k = format!("lvl{i:03}");
                StateBackend::put_cf_raw(ov, CF_NATIVE_ORDER_BOOKS, k.as_bytes(), &i.to_be_bytes())
                    .unwrap();
            }
            StateBackend::delete_cf_raw(ov, CF_NATIVE_ORDER_BOOKS, b"gone").unwrap();
            // ...and the node-local order-row store (owned put, as pass 2 does).
            StateBackend::put_cf_raw_owned(ov, CF_BOOK_ORDER_ROWS, b"row1", b"r".to_vec())
                .unwrap();
        };
        // A pre-existing book row the sidecar deletes (tombstone must land).
        let seed = |db: &StateDb| {
            StateDb::put_cf_raw(db, CF_NATIVE_ORDER_BOOKS, b"gone", b"old").unwrap();
            crate::native_trie::build_native_trie_to_cf(db).unwrap();
        };

        // Combined reference: one overlay carries both write sets.
        let (db_a, _da) = temp_db();
        seed(&db_a);
        let ov = NativeStateOverlay::new(db_a.clone());
        put_main(&ov);
        put_books(&ov);
        ov.freeze(5)
            .flush_with_native_trie_stats(&db_a, Some(5), None, None)
            .unwrap();

        // Split: main frozen set + book sidecar, one flush.
        let (db_b, _db) = temp_db();
        seed(&db_b);
        let main = NativeStateOverlay::new(db_b.clone());
        put_main(&main);
        let side = NativeStateOverlay::new(db_b.clone());
        put_books(&side);
        let main_frozen = main.freeze(5);
        let side_frozen = side.freeze(5);
        let (main_n, side_n) = (main_frozen.entry_count(), side_frozen.entry_count());
        main_frozen
            .flush_with_sidecar_native_trie_stats(Some(&side_frozen), &db_b, Some(5), None, None)
            .unwrap();
        assert_eq!(main_frozen.entry_count(), main_n, "flush must not mutate the frozen set");
        assert_eq!(side_frozen.entry_count(), side_n, "flush must not mutate the sidecar");

        assert_eq!(dump(&db_a), dump(&db_b), "sidecar flush must be byte-identical");
        let root_a = crate::native_trie::persisted_native_root(&db_a).unwrap();
        assert_eq!(root_a, crate::native_trie::persisted_native_root(&db_b).unwrap());
        assert_eq!(
            StateDb::get_cf_raw(&db_b, CF_CONSENSUS_META, META_NATIVE_APPLIED_HEIGHT).unwrap(),
            Some(5u64.to_be_bytes().to_vec()),
            "marker rides the same batch"
        );

        // `None` sidecar == the plain flush (the pre-port entry point).
        let (db_n, _dn) = temp_db();
        seed(&db_n);
        let plain = NativeStateOverlay::new(db_n.clone());
        put_main(&plain);
        plain
            .freeze(5)
            .flush_with_sidecar_native_trie_stats(None, &db_n, Some(5), None, None)
            .unwrap();

        // Negative control: without the sidecar the root differs — proof the
        // sidecar's CF_NATIVE_ORDER_BOOKS entries participate in the root.
        let (db_c, _dc) = temp_db();
        seed(&db_c);
        let main_only = NativeStateOverlay::new(db_c.clone());
        put_main(&main_only);
        main_only
            .freeze(5)
            .flush_with_native_trie_stats(&db_c, Some(5), None, None)
            .unwrap();
        assert_eq!(dump(&db_n), dump(&db_c), "None sidecar must equal the plain flush");
        assert_ne!(
            root_a,
            crate::native_trie::persisted_native_root(&db_c).unwrap(),
            "book rows must be root-visible"
        );
    }

    // ---- s83 Option 0: `TORUS_NATIVE_TRIE_MAINTENANCE` (mode passed explicitly, env-free) ----

    fn s83_dump(db: &StateDb, cf: &str) -> Vec<(Vec<u8>, Vec<u8>)> {
        StateBackend::iterate_cf(db, cf, None).unwrap()
    }

    /// Pre-block native state + a built trie (the boot state `ensure_native_trie_built` leaves).
    fn s83_seed(db: &StateDb) {
        use crate::cf::CF_NATIVE_POSITIONS;
        db.put_cf_raw(CF_NATIVE_BALANCES, b"\x00\x01base", b"b0").unwrap();
        db.put_cf_raw(CF_NATIVE_POSITIONS, b"\x00\x02gone", b"p0").unwrap();
        assert!(crate::native_trie::ensure_native_trie_built(db, true).unwrap());
    }

    /// Blocks `from..=to` (root-CF puts + a delete + a non-root nonce) flushed through the one
    /// flush implementation with an EXPLICIT maintenance mode.
    fn s83_blocks(db: &StateDb, from: u64, to: u64, maintain: bool) -> Vec<NativeFlushStats> {
        use crate::cf::{CF_NATIVE_NONCES, CF_NATIVE_POSITIONS};
        (from..=to)
            .map(|h| {
                let ov = NativeStateOverlay::new(db.clone());
                ov.put_cf_raw(CF_NATIVE_BALANCES, &[0, h as u8, b'a'], &h.to_be_bytes())
                    .unwrap();
                ov.put_cf_raw(CF_NATIVE_POSITIONS, &[1, h as u8], b"pos").unwrap();
                ov.put_cf_raw(CF_NATIVE_NONCES, &[h as u8], b"n").unwrap();
                if h == 2 {
                    ov.delete_cf_raw(CF_NATIVE_POSITIONS, b"\x00\x02gone").unwrap();
                }
                let state = ov.pending.read().unwrap();
                flush_pending_with_native_trie_stats(&state, None, db, Some(h), None, None, maintain)
                    .unwrap()
            })
            .collect()
    }

    /// Skip mode writes NO trie/mirror rows, sets the stale sentinel in the same batch, and leaves
    /// every other byte (native state, nonces, applied-height marker) identical to maintain mode.
    #[test]
    fn s83_maintenance_off_skips_trie_sets_sentinel_state_identical() {
        use crate::cf::{
            CF_CONSENSUS_META, CF_NATIVE_HASHED, CF_NATIVE_TRIE, META_NATIVE_APPLIED_HEIGHT,
            META_NATIVE_TRIE_STALE,
        };
        use crate::native_trie::is_native_trie_stale;
        let (db_on, _a) = temp_db();
        let (db_off, _b) = temp_db();
        s83_seed(&db_on);
        s83_seed(&db_off);
        let base_trie = s83_dump(&db_off, CF_NATIVE_TRIE);
        let base_mirror = s83_dump(&db_off, CF_NATIVE_HASHED);

        let on = s83_blocks(&db_on, 1, 3, true);
        let off = s83_blocks(&db_off, 1, 3, false);

        assert!(!is_native_trie_stale(&db_on).unwrap(), "maintain mode never marks stale");
        assert!(is_native_trie_stale(&db_off).unwrap(), "skip mode marks the trie stale");
        assert_ne!(s83_dump(&db_on, CF_NATIVE_TRIE), base_trie, "control: maintain mode wrote");
        assert_eq!(s83_dump(&db_off, CF_NATIVE_TRIE), base_trie, "skip: no trie writes");
        assert_eq!(s83_dump(&db_off, CF_NATIVE_HASHED), base_mirror, "skip: no mirror writes");

        for cf in crate::cf::ALL_CF_NAMES {
            if [CF_NATIVE_TRIE, CF_NATIVE_HASHED, CF_CONSENSUS_META].contains(cf) {
                continue;
            }
            assert_eq!(s83_dump(&db_on, cf), s83_dump(&db_off, cf), "CF {cf}");
        }
        // Meta: identical except for the sentinel (applied-height marker included).
        let meta_off: Vec<_> = s83_dump(&db_off, CF_CONSENSUS_META)
            .into_iter()
            .filter(|(k, _)| k.as_slice() != META_NATIVE_TRIE_STALE)
            .collect();
        assert_eq!(s83_dump(&db_on, CF_CONSENSUS_META), meta_off);
        assert_eq!(
            StateDb::get_cf_raw(&db_off, CF_CONSENSUS_META, META_NATIVE_APPLIED_HEIGHT).unwrap(),
            Some(3u64.to_be_bytes().to_vec())
        );

        // Stats stay sane: no bucket work, dirty-set attribution unchanged.
        for (a, b) in on.iter().zip(&off) {
            assert!(a.dirty_buckets > 0);
            assert_eq!((b.dirty_buckets, b.bucket_scans, b.member_hits), (0, 0, 0));
            assert_eq!(a.dirty_entries_by_cf, b.dirty_entries_by_cf);
        }
    }

    /// Restart with maintenance ON after a skip run: the boot helper rebuilds, clears the sentinel,
    /// and lands on exactly the maintained arm's root AND bytes (trie + mirror included). With
    /// maintenance still OFF at boot nothing is rebuilt.
    #[test]
    fn s83_boot_rebuild_after_skip_matches_maintained_run() {
        use crate::native_trie::{
            ensure_native_trie_built, is_native_trie_stale, native_root_full, persisted_native_root,
        };
        let (db_on, _a) = temp_db();
        let (db_off, _b) = temp_db();
        s83_seed(&db_on);
        s83_seed(&db_off);
        s83_blocks(&db_on, 1, 3, true);
        s83_blocks(&db_off, 1, 3, false);

        assert!(!ensure_native_trie_built(&db_off, false).unwrap(), "OFF at boot: no rebuild");
        assert!(is_native_trie_stale(&db_off).unwrap());
        assert!(!ensure_native_trie_built(&db_on, true).unwrap(), "fresh trie: no-op");

        assert!(ensure_native_trie_built(&db_off, true).unwrap(), "stale + ON: rebuild");
        assert!(!is_native_trie_stale(&db_off).unwrap(), "rebuild clears the sentinel");
        let root = persisted_native_root(&db_off).unwrap();
        assert_eq!(root, native_root_full(&db_off).unwrap());
        assert_eq!(root, persisted_native_root(&db_on).unwrap());
        for cf in crate::cf::ALL_CF_NAMES {
            assert_eq!(s83_dump(&db_on, cf), s83_dump(&db_off, cf), "CF {cf}");
        }

        // Incremental maintenance resumes cleanly on the rebuilt trie.
        s83_blocks(&db_on, 4, 5, true);
        s83_blocks(&db_off, 4, 5, true);
        assert_eq!(
            persisted_native_root(&db_off).unwrap(),
            native_root_full(&db_off).unwrap()
        );
        for cf in crate::cf::ALL_CF_NAMES {
            assert_eq!(s83_dump(&db_on, cf), s83_dump(&db_off, cf), "CF {cf}");
        }
    }

    /// A stale trie can never pass as fresh: an incremental apply on top of it (maintain mode
    /// without the boot rebuild) leaves the sentinel set; only a full rebuild clears it.
    #[test]
    fn s83_stale_sentinel_survives_maintained_flush_until_rebuild() {
        use crate::native_trie::{is_native_trie_stale, native_root_full, persisted_native_root};
        let (db, _d) = temp_db();
        s83_seed(&db);
        s83_blocks(&db, 1, 2, false);
        s83_blocks(&db, 3, 3, true);
        assert!(is_native_trie_stale(&db).unwrap(), "sentinel is sticky across applies");
        assert_ne!(persisted_native_root(&db).unwrap(), native_root_full(&db).unwrap());
        crate::native_trie::build_native_trie_to_cf(&db).unwrap();
        assert!(!is_native_trie_stale(&db).unwrap());
        assert_eq!(persisted_native_root(&db).unwrap(), native_root_full(&db).unwrap());
    }

    // ======================================================================
    // Running state hash (Task 3)
    // ======================================================================

    use crate::running_hash::{
        checkpoint_heights, hashed_cf_id, next_running_hash, read_checkpoint, read_running_hash,
        WriteEntry, STATE_HASH_CHECKPOINT_INTERVAL, STATE_HASH_CHECKPOINT_RETAIN,
    };

    /// A DB whose chain config enables the running hash from height 1.
    fn rsh_db() -> (StateDb, tempfile::TempDir) {
        let (db, d) = temp_db();
        crate::running_hash::configure_activation(&db, Some(1)).unwrap();
        (db, d)
    }

    /// The canonical hashed write set of `(cf, key, value)` triples (later wins).
    fn rsh_expected(writes: &[(&str, Vec<u8>, Option<Vec<u8>>)]) -> Vec<WriteEntry> {
        let mut m: BTreeMap<(u8, Vec<u8>), Option<Vec<u8>>> = BTreeMap::new();
        for (cf, k, v) in writes {
            if let Some(id) = hashed_cf_id(cf) {
                m.insert((id, k.clone()), v.clone());
            }
        }
        m.into_iter().map(|((id, k), v)| (id, k, v)).collect()
    }

    /// Block `h`'s writes for the long-run test: native puts / deletes, an
    /// EVM account, a derived-CF write and the applied-height marker (both
    /// excluded), a node-local markets key (excluded).
    fn rsh_block_writes(h: u64) -> Vec<(&'static str, Vec<u8>, Option<Vec<u8>>)> {
        use crate::cf::{CF_ACCOUNTS, CF_BLOCK_HEADERS, CF_NATIVE_MARKETS, CF_NATIVE_NONCES};
        if h % 10 != 1 {
            return Vec::new(); // most blocks are empty (marker-only)
        }
        let mut w = vec![
            (CF_NATIVE_BALANCES, vec![(h % 7) as u8], Some(h.to_be_bytes().to_vec())),
            (CF_NATIVE_NONCES, h.to_be_bytes().to_vec(), Some(vec![1])),
            (CF_BLOCK_HEADERS, h.to_be_bytes().to_vec(), Some(b"hdr".to_vec())),
            (crate::cf::CF_CONSENSUS_META, crate::cf::META_NATIVE_APPLIED_HEIGHT.to_vec(), Some(h.to_be_bytes().to_vec())),
            (CF_NATIVE_MARKETS, b"__book_mode__".to_vec(), Some(vec![0])),
        ];
        if h % 3 == 0 {
            w.push((CF_ACCOUNTS, vec![0xab; 20], Some(h.to_le_bytes().to_vec())));
        }
        if h % 4 == 1 {
            w.push((CF_NATIVE_BALANCES, vec![((h + 3) % 7) as u8], None));
        }
        w
    }

    fn rsh_overlay(db: &StateDb, writes: &[(&str, Vec<u8>, Option<Vec<u8>>)]) -> NativeStateOverlay {
        let ov = NativeStateOverlay::new(db.clone());
        for (cf, k, v) in writes {
            match v {
                Some(v) => ov.put_cf_raw(cf, k, v).unwrap(),
                None => ov.delete_cf_raw(cf, k).unwrap(),
            }
        }
        ov
    }

    /// Task 3: flushing N blocks stores `h_n` = the function over each block's
    /// hashed writes, block by block (empty blocks included); checkpoints land at
    /// every multiple of the interval and only the newest RETAIN survive.
    #[test]
    fn running_hash_flush_matches_function_block_by_block() {
        let (db, _d) = rsh_db();
        let last = STATE_HASH_CHECKPOINT_INTERVAL * (STATE_HASH_CHECKPOINT_RETAIN + 2);
        let mut expect = [0u8; 32];
        let mut at_checkpoint = BTreeMap::new();
        for h in 1..=last {
            let writes = rsh_block_writes(h);
            let ov = rsh_overlay(&db, &writes);
            ov.flush_with_native_trie_stats(&db, Some(h), None, None).unwrap();
            expect = next_running_hash(&expect, h, &rsh_expected(&writes));
            assert_eq!(read_running_hash(&db), Some((h, expect)), "height {h}");
            if h % STATE_HASH_CHECKPOINT_INTERVAL == 0 {
                at_checkpoint.insert(h, expect);
            }
        }
        let kept: Vec<u64> = at_checkpoint
            .keys()
            .copied()
            .skip(at_checkpoint.len() - STATE_HASH_CHECKPOINT_RETAIN as usize)
            .collect();
        assert_eq!(checkpoint_heights(&db), kept, "newest {STATE_HASH_CHECKPOINT_RETAIN} checkpoints");
        for (h, hash) in &at_checkpoint {
            let want = kept.contains(h).then_some(*hash);
            assert_eq!(read_checkpoint(&db, *h), want, "checkpoint {h}");
        }
    }

    /// Consensus bug (b) follow-up: validator rows are written only by
    /// execution (inside the flush batch), so they are hashed — a stake /
    /// status change of a validator row at a height alters that height's
    /// hash. RED while `cf_staking_validators` was excluded.
    #[test]
    fn running_hash_covers_validator_rows() {
        use crate::cf::CF_STAKING_VALIDATORS;
        let flush = |row: Option<&[u8]>| {
            let (db, _d) = rsh_db();
            let writes = match row {
                Some(r) => vec![(CF_STAKING_VALIDATORS, vec![0x11; 20], Some(r.to_vec()))],
                None => vec![],
            };
            rsh_overlay(&db, &writes)
                .flush_with_native_trie_stats(&db, Some(1), None, None)
                .unwrap();
            read_running_hash(&db).unwrap()
        };
        let none = flush(None);
        let stake = flush(Some(b"stake=1000,status=Active"));
        assert_ne!(stake, none, "a validator row write is hashed");
        assert_ne!(flush(Some(b"stake=1001,status=Active")), stake, "stake change");
        assert_ne!(flush(Some(b"stake=1000,status=Candid")), stake, "status change");
    }

    /// Task 3: the hash depends only on the hashed write set — not on trie
    /// maintenance, not on how the writes are split between the frozen set and
    /// the deferred-book sidecar, not on excluded CFs.
    #[test]
    fn running_hash_independent_of_trie_maintenance_sidecar_and_excluded_cfs() {
        use crate::cf::{CF_BOOK_ORDER_ROWS, CF_NATIVE_ORDER_BOOKS, CF_NATIVE_TRADES};
        let blocks: Vec<Vec<(&str, Vec<u8>, Option<Vec<u8>>)>> = (1..=8u64)
            .map(|h| {
                let mut w = rsh_block_writes(h * 10 + 1);
                w.push((CF_NATIVE_ORDER_BOOKS, vec![h as u8, 1], Some(vec![h as u8])));
                w
            })
            .collect();
        let run = |maintain: bool, split_sidecar: bool, extra_excluded: bool| {
            let (db, d) = rsh_db();
            for (i, w) in blocks.iter().enumerate() {
                let h = i as u64 + 1;
                let (main, side): (Vec<_>, Vec<_>) = w
                    .iter()
                    .cloned()
                    .partition(|(cf, _, _)| !split_sidecar || *cf != CF_NATIVE_ORDER_BOOKS);
                let ov = rsh_overlay(&db, &main);
                if extra_excluded {
                    ov.put_cf_raw(CF_BOOK_ORDER_ROWS, &[h as u8], b"row").unwrap();
                    ov.put_cf_raw(CF_NATIVE_TRADES, &[h as u8], b"t").unwrap();
                }
                let frozen = ov.freeze(h);
                let sidecar = rsh_overlay(&db, &side).freeze(h);
                flush_pending_with_native_trie_stats(
                    &frozen.state,
                    split_sidecar.then_some(&sidecar.state),
                    &db,
                    Some(h),
                    None,
                    None,
                    maintain,
                )
                .unwrap();
            }
            let out = read_running_hash(&db).unwrap();
            drop(d);
            out
        };
        let base = run(true, false, false);
        assert_eq!(base.0, 8);
        assert_eq!(run(false, false, false), base, "trie maintenance off");
        assert_eq!(run(true, true, false), base, "sidecar split");
        assert_eq!(run(false, true, false), base, "sidecar split, maintenance off");
        assert_eq!(run(true, false, true), base, "excluded CF writes");
    }

    /// Task 3: hash-only extras (consensus writes made durable OUTSIDE the
    /// batch) enter the hash under the block's own writes; the overlay wins
    /// per key; extras are never written by the flush.
    #[test]
    fn running_hash_extras_are_hashed_under_the_overlay_and_never_written() {
        use crate::cf::{CF_ACCOUNTS, CF_SLASH_RECORDS, CF_STORAGE};
        let (db, _d) = rsh_db();
        let mut extras = HashExtras::new();
        extras.put(CF_SLASH_RECORDS, b"val", b"slashed");
        extras.put(CF_ACCOUNTS, &[1; 20], b"evm-old");
        extras.delete(CF_STORAGE, b"slot");
        extras.put(crate::cf::CF_BLOCK_HEADERS, b"x", b"ignored");
        let ov = rsh_overlay(&db, &[(CF_ACCOUNTS, vec![1; 20], Some(b"evm-new".to_vec()))]);
        ov.set_hash_extras(extras);
        ov.flush_with_native_trie_stats(&db, Some(1), None, None).unwrap();
        let want = rsh_expected(&[
            (CF_SLASH_RECORDS, b"val".to_vec(), Some(b"slashed".to_vec())),
            (CF_STORAGE, b"slot".to_vec(), None),
            (CF_ACCOUNTS, vec![1; 20], Some(b"evm-new".to_vec())),
        ]);
        assert_eq!(read_running_hash(&db), Some((1, next_running_hash(&[0; 32], 1, &want))));
        assert_eq!(db.get_cf_raw(CF_SLASH_RECORDS, b"val").unwrap(), None, "extras are hash-only");
        assert_eq!(db.get_cf_raw(CF_ACCOUNTS, &[1; 20]).unwrap(), Some(b"evm-new".to_vec()));

        // Marker-only block with extras (EVM-only / empty boundary block).
        let mut extras = HashExtras::new();
        extras.put(CF_SLASH_RECORDS, b"val", b"v2");
        FrozenPending::marker_only(2)
            .with_hash_extras(extras)
            .flush_with_native_trie_stats(&db, Some(2), None, None)
            .unwrap();
        let h1 = read_running_hash(&db).map(|_| ()).and(Some(next_running_hash(&[0; 32], 1, &want))).unwrap();
        let want2 = rsh_expected(&[(CF_SLASH_RECORDS, b"val".to_vec(), Some(b"v2".to_vec()))]);
        assert_eq!(read_running_hash(&db), Some((2, next_running_hash(&h1, 2, &want2))));
    }

    /// Task 3: per-tx `commit_tx` writes (EVM writer precompiles, slashes) made
    /// while an [`OutOfBatchRecorder`] is armed on this thread are returned as
    /// hash extras, in durable order; unarmed threads record nothing.
    #[test]
    fn out_of_batch_recorder_captures_commit_tx_writes() {
        use crate::cf::{CF_CORE_WRITER_QUEUE, CF_NATIVE_TRADES};
        let (db, _d) = rsh_db();
        let tx = |w: &[(&str, Vec<u8>, Option<Vec<u8>>)]| rsh_overlay(&db, w).commit_tx(&db).unwrap();
        tx(&[(CF_CORE_WRITER_QUEUE, b"before".to_vec(), Some(b"x".to_vec()))]); // not armed
        let rec = OutOfBatchRecorder::begin();
        tx(&[
            (CF_CORE_WRITER_QUEUE, b"q1".to_vec(), Some(b"a".to_vec())),
            (CF_NATIVE_TRADES, b"t".to_vec(), Some(b"node-local".to_vec())),
        ]);
        tx(&[(CF_CORE_WRITER_QUEUE, b"q1".to_vec(), None)]); // later tx deletes it
        tx(&[(CF_CORE_WRITER_QUEUE, b"q2".to_vec(), Some(b"b".to_vec()))]);
        let extras = rec.finish();
        tx(&[(CF_CORE_WRITER_QUEUE, b"after".to_vec(), Some(b"y".to_vec()))]); // disarmed
        FrozenPending::marker_only(1)
            .with_hash_extras(extras)
            .flush_with_native_trie_stats(&db, Some(1), None, None)
            .unwrap();
        let want = rsh_expected(&[
            (CF_CORE_WRITER_QUEUE, b"q1".to_vec(), None),
            (CF_CORE_WRITER_QUEUE, b"q2".to_vec(), Some(b"b".to_vec())),
        ]);
        assert_eq!(read_running_hash(&db), Some((1, next_running_hash(&[0; 32], 1, &want))));
    }

    /// Task 3: the EVM bundle's plain-state change set as hash extras is exactly
    /// what `apply_bundle_plain` writes (accounts, storage incl. zeroed slots,
    /// code, destroyed accounts).
    #[test]
    fn evm_bundle_extras_match_apply_bundle_plain() {
        use crate::cf::{CF_ACCOUNTS, CF_CODE, CF_STORAGE};
        use revm::database::{BundleAccount, BundleState};
        use revm::state::{AccountInfo, Bytecode};
        let (db, _d) = rsh_db();
        // Pre-state so deletions are observable.
        let gone = Address::repeat_byte(0x02);
        db.put_cf_raw(CF_ACCOUNTS, gone.as_slice(), b"old").unwrap();
        let code = Bytecode::new_raw(alloy_primitives::Bytes::from_static(&[0x60, 0x00]));
        let live = Address::repeat_byte(0x01);
        let mut bundle = BundleState::default();
        let mut storage = revm::database::states::StorageWithOriginalValues::default();
        storage.insert(alloy_primitives::U256::from(1), revm::database::states::StorageSlot::new_changed(alloy_primitives::U256::ZERO, alloy_primitives::U256::from(7)));
        storage.insert(alloy_primitives::U256::from(2), revm::database::states::StorageSlot::new_changed(alloy_primitives::U256::from(5), alloy_primitives::U256::ZERO));
        bundle.state.insert(
            live,
            BundleAccount::new(
                None,
                Some(AccountInfo { balance: alloy_primitives::U256::from(9), nonce: 1, code_hash: code.hash_slow(), code: Some(code.clone()), account_id: None }),
                storage,
                revm::database::AccountStatus::Changed,
            ),
        );
        bundle.state.insert(
            gone,
            BundleAccount::new(Some(AccountInfo::default()), None, Default::default(), revm::database::AccountStatus::Destroyed),
        );
        bundle.contracts.insert(code.hash_slow(), code.clone());

        let mut extras = HashExtras::new();
        extras.add_evm_bundle(&bundle);
        let mut batch = WriteBatch::default();
        crate::incremental::apply_bundle_plain(&db, &mut batch, &bundle).unwrap();
        db.write(batch).unwrap();
        // Every extras entry equals the DB after apply_bundle_plain, and every
        // EVM key apply_bundle_plain touched is in extras.
        let mut seen = 0;
        for cf in [CF_ACCOUNTS, CF_STORAGE, CF_CODE] {
            let id = intern_cf(cf).unwrap();
            let c = extras.state.cf(id);
            for (k, v) in &c.writes {
                assert_eq!(db.get_cf_raw(cf, k).unwrap().as_deref(), Some(v.as_slice()), "{cf}");
                seen += 1;
            }
            for k in &c.deletes {
                assert_eq!(db.get_cf_raw(cf, k).unwrap(), None, "{cf} delete");
                seen += 1;
            }
        }
        // live account + 2 slots + code + destroyed account
        assert_eq!(seen, 5);
    }

    fn flush_block(db: &StateDb, h: u64) {
        let writes = rsh_block_writes(h);
        rsh_overlay(db, &writes).flush_with_native_trie_stats(db, Some(h), None, None).unwrap();
    }

    /// Review finding 5: a skipped height (a failed serial flush, after which
    /// the next block's flush advanced the applied marker past it) must never
    /// be chained over from the stale `h_{n-2}`: the stored hash stays at the
    /// last valid height and no later checkpoint is ever written.
    #[test]
    fn running_hash_gap_is_never_chained_over() {
        let (db, _d) = rsh_db();
        for h in 1..=5 {
            flush_block(&db, h);
        }
        let h5 = read_running_hash(&db).unwrap();
        assert_eq!(h5.0, 5);
        // Height 6's batch was lost; 7.. flush normally.
        for h in 7..=STATE_HASH_CHECKPOINT_INTERVAL + 1 {
            flush_block(&db, h);
        }
        assert_eq!(read_running_hash(&db), Some(h5), "no hash chained over the gap");
        assert!(checkpoint_heights(&db).is_empty(), "no checkpoint after the gap");
        assert_eq!(
            crate::running_hash::read_unverified_since(&db),
            Some(7),
            "the node is hash-unverified from the first height it could not chain"
        );
    }

    /// Review finding 1: the activation height is CHAIN-WIDE (chain config),
    /// not "the first height this DB happened to flush": nodes upgraded at
    /// different applied heights below it, and a node synced from genesis,
    /// produce identical hashes from the activation height on. A node whose
    /// DB is already ABOVE the activation height has no valid `h_{n-1}`: it is
    /// hash-unverified and never writes a hash or checkpoint. No activation
    /// configured = disabled (nothing hashed).
    #[test]
    fn running_hash_activation_is_chain_wide() {
        use crate::running_hash::{configure_activation, read_activation_height, read_unverified_since};
        let act = 21u64;
        let last = 2 * STATE_HASH_CHECKPOINT_INTERVAL + 5;
        let mut expect = [0u8; 32];
        for h in act..=last {
            expect = next_running_hash(&expect, h, &rsh_expected(&rsh_block_writes(h)));
        }
        // Disabled.
        let (off, _d0) = temp_db();
        for h in 1..=last {
            flush_block(&off, h);
        }
        assert_eq!(read_running_hash(&off), None, "no activation configured: nothing hashed");
        assert!(checkpoint_heights(&off).is_empty());
        // Upgraded at applied 0 (synced from genesis), 7, act - 1.
        for upgraded_at in [0, 7, act - 1] {
            let (db, _d) = rsh_db();
            for h in 1..=upgraded_at {
                flush_block(&db, h);
            }
            configure_activation(&db, Some(act)).unwrap();
            for h in upgraded_at + 1..=last {
                flush_block(&db, h);
            }
            assert_eq!(read_running_hash(&db), Some((last, expect)), "upgraded at {upgraded_at}");
            assert_eq!(read_activation_height(&db), Some(act));
            assert_eq!(read_unverified_since(&db), None);
            assert_eq!(checkpoint_heights(&db), vec![100, 200]);
        }
        // Upgraded above the activation height.
        let (late, _d) = temp_db();
        for h in 1..=act + 3 {
            flush_block(&late, h);
        }
        configure_activation(&late, Some(act)).unwrap();
        for h in act + 4..=last {
            flush_block(&late, h);
        }
        assert_eq!(read_unverified_since(&late), Some(act + 4));
        assert_eq!(read_running_hash(&late), None);
        assert!(checkpoint_heights(&late).is_empty(), "never a checkpoint to attest");
    }

    /// A configuration change (another activation height, or disabling)
    /// discards the stored chain — its hashes belong to another chain
    /// definition — and an unchanged configuration keeps it.
    #[test]
    fn running_hash_configure_activation_resets_only_on_change() {
        use crate::running_hash::{configure_activation, read_configured_activation, read_unverified_since};
        let (db, _d) = rsh_db();
        for h in 1..=STATE_HASH_CHECKPOINT_INTERVAL + 3 {
            flush_block(&db, h);
        }
        let before = read_running_hash(&db);
        configure_activation(&db, Some(1)).unwrap();
        assert_eq!(read_running_hash(&db), before, "same config: chain kept");
        assert_eq!(checkpoint_heights(&db), vec![100]);
        configure_activation(&db, Some(150)).unwrap();
        assert_eq!(read_configured_activation(&db), Some(150));
        assert_eq!(read_running_hash(&db), None, "new activation: old chain discarded");
        assert!(checkpoint_heights(&db).is_empty());
        for h in STATE_HASH_CHECKPOINT_INTERVAL + 4..=160 {
            flush_block(&db, h);
        }
        let mut expect = [0u8; 32];
        for h in 150..=160 {
            expect = next_running_hash(&expect, h, &rsh_expected(&rsh_block_writes(h)));
        }
        assert_eq!(read_running_hash(&db), Some((160, expect)));
        assert_eq!(read_unverified_since(&db), None);
        configure_activation(&db, None).unwrap();
        assert_eq!(read_configured_activation(&db), None);
        assert_eq!(read_running_hash(&db), None, "disabled: chain discarded");
        flush_block(&db, 161);
        assert_eq!(read_running_hash(&db), None, "disabled: nothing hashed");
    }

    /// Review finding 7: the multi-layer merge is a streaming k-way merge;
    /// it must visit exactly what a full sorted map of all layers (later
    /// layer wins per key, tombstones included) would.
    #[test]
    fn running_hash_k_way_merge_matches_sorted_map() {
        use crate::cf::{CF_ACCOUNTS, CF_NATIVE_POSITIONS};
        let layer = |seed: u64| {
            let mut s = PendingState::new();
            for cf in [CF_NATIVE_POSITIONS, CF_ACCOUNTS, CF_NATIVE_BALANCES] {
                let id = intern_cf(cf).unwrap();
                let c = s.cf_mut(id);
                for i in 0..40u64 {
                    let x = (i * 7 + seed * 13) % 23;
                    let key = vec![(x % 11) as u8, (x / 11) as u8];
                    if (i + seed) % 3 == 0 {
                        c.writes.remove(&key);
                        c.deletes.insert(key);
                    } else {
                        c.deletes.remove(&key);
                        c.writes.insert(key, vec![seed as u8, i as u8]);
                    }
                }
            }
            s
        };
        let (a, b, c) = (layer(1), layer(2), layer(3));
        for layers in [vec![&a], vec![&a, &b], vec![&b, &a], vec![&a, &b, &c], vec![&c, &a, &b]] {
            let mut got = Vec::new();
            for_each_consensus_write(&layers, |id, k, v| got.push((id, k.to_vec(), v.map(<[u8]>::to_vec))));
            let mut want: BTreeMap<(u8, Vec<u8>), Option<Vec<u8>>> = BTreeMap::new();
            for l in &layers {
                for &(cf_id, id) in hashed_cf_order() {
                    let p = l.cf(id);
                    for (k, v) in &p.writes {
                        want.insert((cf_id, k.clone()), Some(v.clone()));
                    }
                    for k in &p.deletes {
                        want.insert((cf_id, k.clone()), None);
                    }
                }
            }
            let want: Vec<_> = want.into_iter().map(|((id, k), v)| (id, k, v)).collect();
            assert_eq!(got, want, "{} layers", layers.len());
        }
    }

    /// A block above `PARALLEL_DIGEST_MIN_ENTRIES` digests on the scoped
    /// thread: same hash as the function, same as the inline path.
    #[test]
    fn running_hash_parallel_digest_matches_function() {
        use crate::cf::{CF_ACCOUNTS, CF_NATIVE_POSITIONS};
        let (db, _d) = rsh_db();
        let mut writes = Vec::new();
        for i in 0..(PARALLEL_DIGEST_MIN_ENTRIES as u32 + 100) {
            let cf = if i % 3 == 0 { CF_NATIVE_POSITIONS } else { CF_ACCOUNTS };
            let v = (i % 5 != 0).then(|| i.to_be_bytes().to_vec());
            writes.push((cf, i.to_le_bytes().to_vec(), v));
        }
        let ov = rsh_overlay(&db, &writes);
        let stats = ov.flush_with_native_trie_stats(&db, Some(1), None, None).unwrap();
        assert_eq!(stats.state_hash_entries, writes.len());
        let want = next_running_hash(&[0; 32], 1, &rsh_expected(&writes));
        assert_eq!(read_running_hash(&db), Some((1, want)));
    }

    /// Running state hash cost per block (Task 9 input). Release only:
    /// `cargo test --release -p torus-state --lib running_hash_cost -- --ignored --nocapture`.
    /// N consensus rows per block spread over the native CFs (32-byte keys,
    /// 96-byte values, 1 in 8 a tombstone) plus derived-CF noise; reports the
    /// flush's own `state_hash_seconds` (stream + digest + chain + META puts)
    /// and the whole flush wall under root skip.
    #[test]
    #[ignore]
    fn running_hash_cost_per_block_release() {
        use crate::cf::{
            CF_BOOK_ORDER_ROWS, CF_NATIVE_NONCES, CF_NATIVE_ORDER_BOOKS, CF_NATIVE_POSITIONS,
        };
        let cfs = [CF_NATIVE_BALANCES, CF_NATIVE_POSITIONS, CF_NATIVE_ORDER_BOOKS, CF_NATIVE_NONCES];
        for rows in [10_000usize, 60_000] {
            let (db, _d) = rsh_db();
            // Baseline flushes go to a DB without the running hash (no
            // activation configured), so the hashed DB's heights stay
            // consecutive (a skipped height would stop its chain).
            let (base_db, _bd) = temp_db();
            let mut samples = Vec::new();
            let mut baseline = Vec::new();
            for h in 1..=24u64 {
                let ov = NativeStateOverlay::new(db.clone());
                for i in 0..rows {
                    let mut key = [0u8; 32];
                    key[..8].copy_from_slice(&(i as u64).to_be_bytes());
                    key[8..16].copy_from_slice(&h.to_be_bytes());
                    let cf = cfs[i % cfs.len()];
                    if i % 8 == 7 {
                        ov.delete_cf_raw(cf, &key).unwrap();
                    } else {
                        ov.put_cf_raw(cf, &key, &[(i % 251) as u8; 96]).unwrap();
                    }
                    if i % 4 == 0 {
                        ov.put_cf_raw(CF_BOOK_ORDER_ROWS, &key, &[1; 96]).unwrap();
                    }
                }
                let t = std::time::Instant::now();
                // Root skip (TORUS_NATIVE_TRIE_MAINTENANCE=0): the configuration
                // the running hash replaces the per-block root for.
                // Odd heights hash (applied height set), even ones flush the
                // same-sized set without any applied height into the
                // un-hashed DB: no hash, no marker — the baseline for the
                // EXPOSED hashing cost.
                let hashed = h % 2 == 1;
                let state = ov.pending.read().unwrap();
                let stats = flush_pending_with_native_trie_stats(
                    &state,
                    None,
                    if hashed { &db } else { &base_db },
                    hashed.then_some(h.div_ceil(2)),
                    None,
                    None,
                    false,
                )
                .unwrap();
                let wall = t.elapsed().as_secs_f64() * 1e3;
                if h > 4 && hashed {
                    assert_eq!(stats.state_hash_entries, rows);
                    samples.push((stats.state_hash_seconds * 1e3, wall));
                } else if h > 4 {
                    baseline.push(wall);
                }
            }
            let median = |mut v: Vec<f64>| {
                v.sort_by(|a, b| a.partial_cmp(b).unwrap());
                v[v.len() / 2]
            };
            println!(
                "running_hash_cost rows={rows}: digest median {:.2} ms (min {:.2}, max {:.2}); \
                 flush wall (root skip) median {:.2} ms with hash vs {:.2} ms without ({} samples each)",
                median(samples.iter().map(|s| s.0).collect()),
                samples.iter().map(|s| s.0).fold(f64::MAX, f64::min),
                samples.iter().map(|s| s.0).fold(0.0, f64::max),
                median(samples.iter().map(|s| s.1).collect()),
                median(baseline.clone()),
                samples.len()
            );
        }
    }
}
