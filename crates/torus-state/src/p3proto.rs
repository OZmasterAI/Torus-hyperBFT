//! THROWAWAY PROTOTYPE (branch `proto/p3-coalesce`, NOT for merge): Phase 3
//! checkpoint-cost measurement (owner request s111, results doc section 41).
//!
//! `TORUS_P3_PROTO` (node env, read once at boot by the node):
//! - unset / anything else: exact-today (nothing here is ever constructed; every
//!   `StateDb` hook is one `OnceLock::get` returning `None`).
//! - `all`: every write to a state CF (the rows of the per-block atomic batch:
//!   every registered CF except bodies / headers / commit manifest / receipts /
//!   logs / trades / DA / shards / state-hash votes / trie CFs) goes into an
//!   in-memory, last-value-coalesced layer instead of RocksDB. Every
//!   `TORUS_P3_PROTO_INTERVAL` (default 100) blocks, at heights `h % interval
//!   == 0`, the layer is written as ONE RocksDB write batch. The applied-height
//!   marker, running hash and checkpoint rows live in `cf_consensus_meta`, so
//!   they ride the same checkpoint batch: after a crash the DB is exactly the
//!   state at the last checkpoint height S, and `replay_committed` rebuilds
//!   `(S, committed]` from the block bodies (written at commit, not deferred).
//! - `books`: only `cf_native_order_books` (Classic book blobs) is deferred; all
//!   other rows (and the marker) keep their per-block write. The books
//!   checkpoint batch also writes `META_P3_BOOKS_CKPT` = the height the books
//!   are at. A crash between checkpoints leaves books at S < marker n; that
//!   state is NOT recoverable (other rows cannot be rolled back), so boot fails
//!   closed ([`Deferred::enable`] returns an error) instead of running on stale
//!   books. Graceful shutdown (SIGINT / SIGTERM) flushes and is safe.
//!
//! Reads: every `StateDb` read (point gets, `StateBackend` scans, the overlay's
//! DB fallback scans) consults the layer first: `active` (newest) then
//! `flushing` (the layer a checkpoint is writing; kept readable until its batch
//! is durable) then RocksDB. One `RwLock` per CF, so absorbing positions never
//! blocks a book read.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::Instant;

use rocksdb::WriteBatch;

use crate::cf::*;
use crate::db::StateDb;
use crate::error::StateError;

/// `cf_consensus_meta` key: the height the durable Classic book blobs are at
/// (books arm only).
pub const META_P3_BOOKS_CKPT: &[u8] = b"p3proto_books_ckpt";

const NUM_CFS: usize = ALL_CF_NAMES.len();

/// Which rows are coalesced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Every state CF of the per-block batch.
    All,
    /// Only Classic book blobs (`cf_native_order_books`).
    Books,
}

/// Parse `TORUS_P3_PROTO`. `None` = off (exact-today).
pub fn parse_mode(raw: Option<&str>) -> Option<Mode> {
    match raw.map(str::trim) {
        Some("all") => Some(Mode::All),
        Some("books") => Some(Mode::Books),
        _ => None,
    }
}

/// `TORUS_P3_PROTO` from the environment.
pub fn mode_from_env() -> Option<Mode> {
    parse_mode(std::env::var("TORUS_P3_PROTO").ok().as_deref())
}

/// `TORUS_P3_PROTO_INTERVAL` (blocks, default 100, `< 1` / garbage = 100).
pub fn interval_from_env() -> u64 {
    std::env::var("TORUS_P3_PROTO_INTERVAL")
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .filter(|&n| n >= 1)
        .unwrap_or(100)
}

/// CFs never deferred in the `all` arm: not state rows (bodies, headers,
/// manifest, receipts / logs, trades, DA, shards, votes) or the trie mirrors
/// (maintenance is off by default; the trie code reads RocksDB directly).
const NOT_STATE: &[&str] = &[
    CF_BLOCK_HEADERS,
    CF_BLOCK_BODIES,
    CF_COMMIT_MANIFEST,
    CF_BLOCK_HASH_TO_NUMBER,
    CF_RECEIPTS,
    CF_LOGS,
    CF_LOGS_BLOOM,
    CF_TX_HASH_TO_LOCATION,
    CF_NATIVE_TRADES,
    CF_NATIVE_USER_TRADES,
    CF_NATIVE_PENDING,
    CF_NATIVE_SHARDS,
    CF_STATE_HASH_VOTES,
    CF_TRIE_NODES,
    CF_TRIE_ACCOUNTS,
    CF_TRIE_STORAGE,
    CF_HASHED_ACCOUNTS,
    CF_HASHED_STORAGE,
    CF_NATIVE_TRIE,
    CF_NATIVE_HASHED,
];

/// Index of a registered CF in [`ALL_CF_NAMES`].
pub(crate) fn cf_index(name: &str) -> Option<usize> {
    static TABLE: OnceLock<HashMap<&'static str, usize>> = OnceLock::new();
    TABLE
        .get_or_init(|| ALL_CF_NAMES.iter().enumerate().map(|(i, n)| (*n, i)).collect())
        .get(name)
        .copied()
}

/// Whether `mode` defers CF `name`.
pub fn covers(mode: Mode, name: &str) -> bool {
    match mode {
        Mode::All => cf_index(name).is_some() && !NOT_STATE.contains(&name),
        Mode::Books => name == CF_NATIVE_ORDER_BOOKS,
    }
}

/// Last-value rows of one CF: `Some(v)` = put, `None` = delete (tombstone).
pub(crate) type Rows = BTreeMap<Vec<u8>, Option<Vec<u8>>>;

#[derive(Default)]
pub(crate) struct Slot {
    pub(crate) active: Rows,
    pub(crate) flushing: Option<Arc<Rows>>,
}

impl Slot {
    /// The newest layered value: `Some(hit)` if a layer holds the key.
    pub(crate) fn lookup(&self, key: &[u8]) -> Option<Option<&[u8]>> {
        if let Some(v) = self.active.get(key) {
            return Some(v.as_deref());
        }
        self.flushing.as_ref().and_then(|f| f.get(key)).map(|v| v.as_deref())
    }
}

/// One staged write (CF index, key, value-or-delete).
pub(crate) type Staged = (usize, Vec<u8>, Option<Vec<u8>>);

struct BlockState {
    /// Highest height absorbed by a block write.
    last_height: Option<u64>,
    /// Blocks absorbed since the last checkpoint.
    blocks_since: u64,
    /// The in-flight checkpoint write (one at a time).
    inflight: Option<std::thread::JoinHandle<Result<(), String>>>,
    /// Absorb seconds since the last checkpoint (stats).
    absorb_secs: f64,
}

/// Counters for the bench analysis (also logged at every checkpoint).
#[derive(Default)]
pub struct Stats {
    pub checkpoints: AtomicU64,
    pub checkpoint_rows: AtomicU64,
    pub checkpoint_bytes: AtomicU64,
    pub checkpoint_build_us: AtomicU64,
    pub checkpoint_write_us: AtomicU64,
    pub absorbed_rows: AtomicU64,
    pub absorb_us: AtomicU64,
}

/// The deferred-write layer shared by every clone of a [`StateDb`].
pub struct Deferred {
    mode: Mode,
    interval: u64,
    covered: [bool; NUM_CFS],
    pub(crate) slots: Vec<RwLock<Slot>>,
    /// RocksDB column family id -> index in [`ALL_CF_NAMES`].
    cf_ids: HashMap<u32, usize>,
    block: Mutex<BlockState>,
    closed: AtomicBool,
    pub stats: Stats,
}

/// Collects a WriteBatch's operations with their RocksDB CF ids.
struct Decoder(Vec<(u32, Vec<u8>, Option<Vec<u8>>)>);

impl rocksdb::WriteBatchIteratorCf for Decoder {
    fn put_cf(&mut self, cf_id: u32, key: &[u8], value: &[u8]) {
        self.0.push((cf_id, key.to_vec(), Some(value.to_vec())));
    }
    fn delete_cf(&mut self, cf_id: u32, key: &[u8]) {
        self.0.push((cf_id, key.to_vec(), None));
    }
    fn merge_cf(&mut self, cf_id: u32, _key: &[u8], _value: &[u8]) {
        // No merge operator is configured on any Torus CF.
        panic!("p3proto: merge op in a state batch (cf id {cf_id})");
    }
}

/// `WriteBatch` has no CF-id accessor: learn each CF's id by decoding a probe
/// batch with one put per registered CF.
fn learn_cf_ids(db: &StateDb) -> Result<HashMap<u32, usize>, StateError> {
    let mut probe = WriteBatch::default();
    for name in ALL_CF_NAMES {
        probe.put_cf(db.cf_handle(name)?, b"", b"");
    }
    let mut d = Decoder(Vec::new());
    probe.iterate_cf(&mut d);
    assert_eq!(d.0.len(), NUM_CFS);
    Ok(d.0.into_iter().enumerate().map(|(i, (id, _, _))| (id, i)).collect())
}

impl Deferred {
    /// Build the layer for `db`. Books arm: fail closed if a previous books
    /// run crashed between checkpoints (books older than the applied marker),
    /// then record the current marker as the books height.
    pub fn enable(db: &StateDb, mode: Mode, interval: u64) -> Result<Self, StateError> {
        let mut covered = [false; NUM_CFS];
        for (i, name) in ALL_CF_NAMES.iter().enumerate() {
            covered[i] = covers(mode, name);
        }
        let cf_ids = learn_cf_ids(db)?;
        let read_u64 = |key: &[u8]| -> Result<Option<u64>, StateError> {
            Ok(db
                .get_cf_raw(CF_CONSENSUS_META, key)?
                .and_then(|v| v.as_slice().try_into().ok().map(u64::from_be_bytes)))
        };
        let applied = read_u64(META_NATIVE_APPLIED_HEIGHT)?;
        if mode == Mode::Books {
            let ckpt = read_u64(META_P3_BOOKS_CKPT)?;
            if let Some(c) = ckpt {
                if Some(c) != applied {
                    return Err(StateError::InvalidData(format!(
                        "p3proto books arm: durable book blobs are at height {c} but the applied \
                         marker is at {applied:?} (the previous books-arm run crashed between \
                         book checkpoints). Not recoverable: wipe the data dir"
                    )));
                }
            }
            db.put_cf_raw(
                CF_CONSENSUS_META,
                META_P3_BOOKS_CKPT,
                &applied.unwrap_or(0).to_be_bytes(),
            )?;
            db.sync_wal()?;
        }
        tracing::info!(?mode, interval, ?applied, "p3proto: coalesced checkpoint layer enabled");
        Ok(Self {
            mode,
            interval,
            covered,
            slots: (0..NUM_CFS).map(|_| RwLock::new(Slot::default())).collect(),
            cf_ids,
            block: Mutex::new(BlockState {
                last_height: applied,
                blocks_since: 0,
                inflight: None,
                absorb_secs: 0.0,
            }),
            closed: AtomicBool::new(false),
            stats: Stats::default(),
        })
    }

    /// Not yet closed by the shutdown flush.
    pub fn is_live(&self) -> bool {
        !self.closed.load(Ordering::Acquire)
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// The layer index for `cf` if this arm defers it and the layer is live.
    #[inline]
    pub(crate) fn slot_of(&self, cf: &str) -> Option<usize> {
        if self.closed.load(Ordering::Acquire) {
            return None;
        }
        cf_index(cf).filter(|&i| self.covered[i])
    }

    /// Whether the CF at `idx` is deferred.
    #[inline]
    pub(crate) fn covers_idx(&self, idx: usize) -> bool {
        self.covered[idx]
    }

    /// Point lookup in the layers.
    pub(crate) fn lookup(&self, idx: usize, key: &[u8]) -> Option<Option<Vec<u8>>> {
        let slot = self.slots[idx].read().unwrap();
        slot.lookup(key).map(|v| v.map(<[u8]>::to_vec))
    }

    /// Split a batch: deferred ops into `staged` (in batch order), the rest into
    /// `rest` (in batch order).
    fn split(&self, db: &StateDb, batch: &WriteBatch, staged: &mut Vec<Staged>, rest: &mut WriteBatch) -> Result<(), StateError> {
        if batch.is_empty() {
            return Ok(());
        }
        let mut d = Decoder(Vec::with_capacity(batch.len()));
        batch.iterate_cf(&mut d);
        if d.0.len() != batch.len() {
            // `iterate_cf` reports puts / deletes / merges only: the batch
            // also holds range deletes, which a rebuilt batch would drop.
            return Err(StateError::InvalidData(
                "p3proto: range delete in a per-block state batch (unsupported)".into(),
            ));
        }
        for (id, key, value) in d.0 {
            match self.cf_ids.get(&id) {
                Some(&i) if self.covered[i] => staged.push((i, key, value)),
                other => {
                    // Unregistered CF id (e.g. "default") or not deferred.
                    let cf = match other {
                        Some(&i) => db.cf_handle(ALL_CF_NAMES[i])?,
                        None => {
                            return Err(StateError::InvalidData(format!(
                                "p3proto: write to an unregistered CF id {id}"
                            )))
                        }
                    };
                    match value {
                        Some(v) => rest.put_cf(cf, key, v),
                        None => rest.delete_cf(cf, key),
                    }
                }
            }
        }
        Ok(())
    }

    fn absorb(&self, staged: Vec<Staged>) -> f64 {
        if staged.is_empty() {
            return 0.0;
        }
        let t = Instant::now();
        let n = staged.len() as u64;
        // Group by CF (stable: keeps per-key last-wins order), then one lock
        // per CF. `cf_consensus_meta` (marker, running hash) is absorbed LAST
        // so a reader that sees the marker sees the block's rows.
        let mut by_cf: Vec<Vec<(Vec<u8>, Option<Vec<u8>>)>> = (0..NUM_CFS).map(|_| Vec::new()).collect();
        for (i, k, v) in staged {
            by_cf[i].push((k, v));
        }
        let meta = cf_index(CF_CONSENSUS_META).expect("meta cf");
        let order = (0..NUM_CFS).filter(|&i| i != meta).chain(std::iter::once(meta));
        for i in order {
            let rows = std::mem::take(&mut by_cf[i]);
            if rows.is_empty() {
                continue;
            }
            let mut slot = self.slots[i].write().unwrap();
            for (k, v) in rows {
                slot.active.insert(k, v);
            }
        }
        let secs = t.elapsed().as_secs_f64();
        self.stats.absorbed_rows.fetch_add(n, Ordering::Relaxed);
        self.stats.absorb_us.fetch_add((secs * 1e6) as u64, Ordering::Relaxed);
        secs
    }

    /// A non-block write (`StateDb::write` / `put_cf_raw` / ...): deferred CFs
    /// into the layer, the rest written now. Returns `false` when the layer is
    /// closed (the caller writes everything itself).
    pub(crate) fn generic_write(&self, db: &StateDb, batch: &WriteBatch, opts: Option<&rocksdb::WriteOptions>) -> Result<bool, StateError> {
        let mut st = self.block.lock().unwrap();
        if self.closed.load(Ordering::Acquire) {
            return Ok(false);
        }
        let raw = |b: &WriteBatch| -> Result<(), StateError> {
            // Write the caller's batch itself (exact bytes, range deletes kept).
            let copy = WriteBatch::from_data(b.data());
            match opts {
                Some(o) => db.raw_write_opt(copy, o),
                None => db.raw_write(copy),
            }
        };
        let mut d = Decoder(Vec::with_capacity(batch.len()));
        batch.iterate_cf(&mut d);
        if d.0.len() != batch.len() {
            // Range deletes (invisible to `iterate_cf`; e.g. the startup
            // trade-history wipe): empty the layer first so no buffered row
            // can shadow the deleted range, then write the batch as is.
            self.checkpoint(db, &mut st, true)?;
            raw(batch)?;
            return Ok(true);
        }
        let covered_any = d
            .0
            .iter()
            .any(|(id, _, _)| self.cf_ids.get(id).is_some_and(|&i| self.covered[i]));
        if !covered_any {
            raw(batch)?;
            return Ok(true);
        }
        drop(d);
        let mut staged = Vec::new();
        let mut rest = WriteBatch::default();
        self.split(db, batch, &mut staged, &mut rest)?;
        if !rest.is_empty() {
            match opts {
                Some(o) => db.raw_write_opt(rest, o)?,
                None => db.raw_write(rest)?,
            }
        }
        self.absorb(staged);
        Ok(true)
    }

    /// Single-key variant of [`Self::generic_write`] for a deferred CF.
    pub(crate) fn put_one(&self, idx: usize, key: &[u8], value: Option<&[u8]>) -> bool {
        let _g = self.block.lock().unwrap();
        if self.closed.load(Ordering::Acquire) {
            return false;
        }
        self.absorb(vec![(idx, key.to_vec(), value.map(<[u8]>::to_vec))]);
        true
    }

    /// The per-block atomic write (`flush_pending_after_batch`): `prefix`
    /// (EVM batch) ops first, then `staged` (the pending sets' deferred rows,
    /// already extracted), then `batch` (non-deferred pending rows + meta tail).
    /// Deferred rows go to the layer, the rest is written now as one batch.
    /// Triggers a checkpoint at `height % interval == 0`.
    pub(crate) fn block_write(
        &self,
        db: &StateDb,
        height: Option<u64>,
        prefix: WriteBatch,
        staged: Vec<Staged>,
        batch: WriteBatch,
    ) -> Result<(), StateError> {
        let mut st = self.block.lock().unwrap();
        if self.closed.load(Ordering::Acquire) {
            // After the shutdown flush: today's single atomic write.
            let mut all = prefix;
            let mut d = Decoder(Vec::new());
            batch.iterate_cf(&mut d);
            if d.0.len() != batch.len() {
                return Err(StateError::InvalidData(
                    "p3proto: range delete in a per-block state batch (unsupported)".into(),
                ));
            }
            for (i, k, v) in staged {
                let cf = db.cf_handle(ALL_CF_NAMES[i])?;
                match v {
                    Some(v) => all.put_cf(cf, k, v),
                    None => all.delete_cf(cf, k),
                }
            }
            for (id, k, v) in d.0 {
                let i = *self.cf_ids.get(&id).ok_or_else(|| {
                    StateError::InvalidData(format!("p3proto: write to an unregistered CF id {id}"))
                })?;
                let cf = db.cf_handle(ALL_CF_NAMES[i])?;
                match v {
                    Some(v) => all.put_cf(cf, k, v),
                    None => all.delete_cf(cf, k),
                }
            }
            if let (Mode::Books, Some(h)) = (self.mode, height) {
                all.put_cf(db.cf_handle(CF_CONSENSUS_META)?, META_P3_BOOKS_CKPT, h.to_be_bytes());
            }
            return db.raw_write(all);
        }
        let mut layer = Vec::with_capacity(staged.len() + 16);
        let mut rest = WriteBatch::default();
        self.split(db, &prefix, &mut layer, &mut rest)?;
        layer.extend(staged);
        self.split(db, &batch, &mut layer, &mut rest)?;
        if !rest.is_empty() {
            db.raw_write(rest)?;
        }
        st.absorb_secs += self.absorb(layer);
        if let Some(h) = height {
            st.last_height = Some(st.last_height.map_or(h, |l| l.max(h)));
            st.blocks_since += 1;
            if h % self.interval == 0 {
                self.checkpoint(db, &mut st, false)?;
            }
        }
        Ok(())
    }

    /// Swap every deferred CF's `active` into `flushing` and write it as ONE
    /// batch: on a background thread (`sync` = false, the periodic checkpoint)
    /// or inline (`sync` = true, shutdown).
    fn checkpoint(&self, db: &StateDb, st: &mut BlockState, sync: bool) -> Result<(), StateError> {
        if let Some(h) = st.inflight.take() {
            match h.join() {
                Ok(Ok(())) => {}
                Ok(Err(e)) => return Err(StateError::InvalidData(format!("p3proto checkpoint failed: {e}"))),
                Err(_) => return Err(StateError::InvalidData("p3proto checkpoint thread panicked".into())),
            }
        }
        let mut layers: Vec<(usize, Arc<Rows>)> = Vec::new();
        for i in 0..NUM_CFS {
            if !self.covered[i] {
                continue;
            }
            let mut slot = self.slots[i].write().unwrap();
            debug_assert!(slot.flushing.is_none());
            if slot.active.is_empty() {
                continue;
            }
            let rows = Arc::new(std::mem::take(&mut slot.active));
            slot.flushing = Some(rows.clone());
            layers.push((i, rows));
        }
        let books_ckpt = (self.mode == Mode::Books).then_some(st.last_height.unwrap_or(0));
        let height = st.last_height;
        let blocks = std::mem::take(&mut st.blocks_since);
        let absorb_secs = std::mem::take(&mut st.absorb_secs);
        let job = {
            let db = db.clone();
            move || -> Result<(), String> {
                let deferred = db.p3().expect("p3 layer");
                let t_build = Instant::now();
                let mut batch = WriteBatch::default();
                let mut rows_n = 0u64;
                for (i, rows) in &layers {
                    let cf = db.cf_handle(ALL_CF_NAMES[*i]).map_err(|e| e.to_string())?;
                    for (k, v) in rows.iter() {
                        match v {
                            Some(v) => batch.put_cf(cf, k, v),
                            None => batch.delete_cf(cf, k),
                        }
                    }
                    rows_n += rows.len() as u64;
                }
                if let Some(h) = books_ckpt {
                    let cf = db.cf_handle(CF_CONSENSUS_META).map_err(|e| e.to_string())?;
                    batch.put_cf(cf, META_P3_BOOKS_CKPT, h.to_be_bytes());
                }
                let build = t_build.elapsed().as_secs_f64();
                let bytes = batch.size_in_bytes() as u64;
                let t_write = Instant::now();
                db.raw_write(batch).map_err(|e| e.to_string())?;
                let write = t_write.elapsed().as_secs_f64();
                // Durable: the flushing layers are no longer needed for reads.
                for (i, _) in &layers {
                    deferred.slots[*i].write().unwrap().flushing = None;
                }
                let s = &deferred.stats;
                s.checkpoints.fetch_add(1, Ordering::Relaxed);
                s.checkpoint_rows.fetch_add(rows_n, Ordering::Relaxed);
                s.checkpoint_bytes.fetch_add(bytes, Ordering::Relaxed);
                s.checkpoint_build_us.fetch_add((build * 1e6) as u64, Ordering::Relaxed);
                s.checkpoint_write_us.fetch_add((write * 1e6) as u64, Ordering::Relaxed);
                tracing::info!(
                    height = ?height,
                    blocks,
                    rows = rows_n,
                    bytes,
                    build_ms = build * 1e3,
                    write_ms = write * 1e3,
                    absorb_ms = absorb_secs * 1e3,
                    "p3proto checkpoint"
                );
                Ok(())
            }
        };
        if sync {
            job().map_err(StateError::InvalidData)
        } else {
            st.inflight = Some(
                std::thread::Builder::new()
                    .name("p3-ckpt".into())
                    .spawn(job)
                    .map_err(StateError::Io)?,
            );
            Ok(())
        }
    }

    /// Graceful shutdown: wait for the in-flight checkpoint, write the layer
    /// inline, then close it (later writes go straight to RocksDB, one batch
    /// per block as today). Idempotent.
    pub fn shutdown_flush(&self, db: &StateDb) -> Result<(), StateError> {
        let mut st = self.block.lock().unwrap();
        if self.closed.load(Ordering::Acquire) {
            return Ok(());
        }
        self.checkpoint(db, &mut st, true)?;
        self.closed.store(true, Ordering::Release);
        db.sync_wal()?;
        tracing::info!(height = ?st.last_height, "p3proto: shutdown flush done, layer closed");
        Ok(())
    }

    /// Wait for the in-flight checkpoint (tests).
    pub fn wait_checkpoint(&self) -> Result<(), StateError> {
        let mut st = self.block.lock().unwrap();
        if let Some(h) = st.inflight.take() {
            h.join()
                .map_err(|_| StateError::InvalidData("p3proto checkpoint thread panicked".into()))?
                .map_err(StateError::InvalidData)?;
        }
        Ok(())
    }

    /// Rows currently held in the layers (tests / metrics).
    pub fn layered_rows(&self) -> usize {
        self.slots
            .iter()
            .map(|s| {
                let s = s.read().unwrap();
                s.active.len() + s.flushing.as_ref().map_or(0, |f| f.len())
            })
            .sum()
    }
}

/// Merged forward scan: the layers of one CF (`active` over `flushing`) over a
/// RocksDB iterator, all from the same start key. Tombstones hide rows.
pub(crate) struct MergedIter<'a, I> {
    active: std::iter::Peekable<std::collections::btree_map::Range<'a, Vec<u8>, Option<Vec<u8>>>>,
    flushing: Option<std::iter::Peekable<std::collections::btree_map::Range<'a, Vec<u8>, Option<Vec<u8>>>>>,
    db: I,
    db_head: Option<(Box<[u8]>, Box<[u8]>)>,
    db_started: bool,
    prefix: Option<&'a [u8]>,
}

impl<'a, I> MergedIter<'a, I>
where
    I: Iterator<Item = Result<(Box<[u8]>, Box<[u8]>), rocksdb::Error>>,
{
    pub(crate) fn new(slot: &'a Slot, start: &[u8], prefix: Option<&'a [u8]>, db: I) -> Self {
        let range = (std::ops::Bound::Included(start.to_vec()), std::ops::Bound::Unbounded);
        Self {
            active: slot.active.range(range.clone()).peekable(),
            flushing: slot.flushing.as_ref().map(|f| f.range(range).peekable()),
            db,
            db_head: None,
            db_started: false,
            prefix,
        }
    }
}

impl<I> Iterator for MergedIter<'_, I>
where
    I: Iterator<Item = Result<(Box<[u8]>, Box<[u8]>), rocksdb::Error>>,
{
    type Item = Result<(Vec<u8>, Vec<u8>), StateError>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if !self.db_started {
                self.db_started = true;
                match self.db.next() {
                    Some(Ok(kv)) => self.db_head = Some(kv),
                    Some(Err(e)) => return Some(Err(e.into())),
                    None => {}
                }
            }
            let a = self.active.peek().map(|(k, _)| k.as_slice());
            let f = self.flushing.as_mut().and_then(|f| f.peek()).map(|(k, _)| k.as_slice());
            let d = self.db_head.as_ref().map(|(k, _)| &**k);
            let key = [a, f, d].into_iter().flatten().min()?.to_vec();
            if let Some(p) = self.prefix {
                if !key.starts_with(p) {
                    return None;
                }
            }
            let mut value: Option<Option<Vec<u8>>> = None;
            if self.active.peek().is_some_and(|(k, _)| **k == key) {
                let (_, v) = self.active.next().unwrap();
                value = Some(v.clone());
            }
            if let Some(fl) = self.flushing.as_mut() {
                if fl.peek().is_some_and(|(k, _)| **k == key) {
                    let (_, v) = fl.next().unwrap();
                    if value.is_none() {
                        value = Some(v.clone());
                    }
                }
            }
            if self.db_head.as_ref().is_some_and(|(k, _)| **k == *key) {
                let (_, v) = self.db_head.take().unwrap();
                if value.is_none() {
                    value = Some(Some(v.into_vec()));
                }
                match self.db.next() {
                    Some(Ok(kv)) => self.db_head = Some(kv),
                    Some(Err(e)) => return Some(Err(e.into())),
                    None => {}
                }
            }
            if let Some(Some(v)) = value {
                return Some(Ok((key, v)));
            }
        }
    }
}

#[cfg(test)]
#[path = "p3proto_tests.rs"]
mod tests;
