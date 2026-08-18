//! RocksDB-backed KVStore for hotstuff_rs consensus-internal storage.
//!
//! Scoped to the `cf_consensus_meta` column family. All block tree state
//! (blocks, PCs, TCs, validator set, app state) is stored here by the library.
//!
//! ## Split store (view-legs trim, r2)
//!
//! By default the node keeps this store in ITS OWN RocksDB instance
//! (`<data-dir>/consensus-kv`) instead of a column family of the shared state
//! DB. RocksDB serialises writers per DB instance into write groups: a small
//! block-tree write arriving while the exec thread is inserting a multi-MB
//! state batch (books flush / body persist, ~100+ ms) waits for that whole
//! group to finish. Measured on the dev box (`torus-state`
//! `write_coupling_probe`): the same 4 KiB write costs 0.2 ms alone, 38 ms
//! mean / 120 ms p90 when sharing the DB with an exec-like writer, and 0.2 ms
//! again in a separate instance. Every view does ~5 such writes on the
//! consensus thread (view entered, locks, vote state, insert, update), so the
//! coupling was a direct contributor to `propose_build` / `insert_persist`.
//!
//! Semantics are unchanged: the block tree never needed atomicity with state
//! CFs (its writes were always separate batches; `app_state_updates` is
//! `None`), and neither store fsyncs per write. Existing nodes migrate once at
//! open: when the split store is empty and the legacy CF holds a block tree,
//! every hotstuff key (single-byte prefixes `0..=23`, see
//! `hotstuff_rs::block_tree::variables`) is copied over; the ASCII app keys
//! that also live in `cf_consensus_meta` (`native_applied_height`,
//! `pending_rotation:*`, `validator_whitelist:*`) stay where they are.
//! `TORUS_CONSENSUS_KV_SPLIT=0` keeps the legacy shared-CF layout.
//!
//! The same split instance also hosts the native-DA body/shard CFs (see
//! `open_split_db` and `torus_state::StateDb::open_consensus_split`): the
//! leader's pre-proposal body mirror (a ~2 MB `put_batch` at cap 100 x bs400)
//! and the follower's pre-read mirror flush are consensus-thread writes with
//! the same exposure. Pre-split bodies remain readable through
//! `NativeDaStore::with_legacy_fallback`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use hotstuff_rs::block_tree::pluggables::{KVGet, KVStore, WriteBatch};
use rocksdb::{ColumnFamilyDescriptor, Options, DB};
use torus_state::StateDb;
use torus_telemetry::Metrics;

const CF_NAME: &str = "cf_consensus_meta";

/// Sub-directory of the node data dir holding the split hotstuff store.
pub const CONSENSUS_KV_SUBDIR: &str = "consensus-kv";

/// Highest first byte a hotstuff block-tree key can carry (see
/// `hotstuff_rs::block_tree::variables`, `0..=23` today; anything below the
/// first printable ASCII byte is reserved for the library so future variables
/// migrate too). App keys sharing the legacy CF are ASCII text.
const HOTSTUFF_KEY_PREFIX_MAX: u8 = 0x1f;

/// `TORUS_CONSENSUS_KV_SPLIT`: `0`/`false`/`off`/`no` keeps hotstuff's KVStore
/// in the shared state DB (legacy layout); anything else (default) opens the
/// split instance. Node-local storage layout — never consensus-visible.
pub fn consensus_kv_split_enabled() -> bool {
    consensus_kv_split_from_env(std::env::var("TORUS_CONSENSUS_KV_SPLIT").ok().as_deref())
}

fn consensus_kv_split_from_env(raw: Option<&str>) -> bool {
    match raw.map(|v| v.trim().to_ascii_lowercase()) {
        Some(v) if v == "0" || v == "false" || v == "off" || v == "no" => false,
        _ => true,
    }
}

/// The split store's DB handle, published by [`RocksKVStore::open_for_node`]
/// so the commit-boundary WAL fsync (Task B / Option C, `sync_committed_wal`)
/// can cover the hotstuff frontier too now that it no longer shares the state
/// DB's WAL. `None` under the legacy layout (one WAL, nothing extra to sync).
static SPLIT_DB: std::sync::OnceLock<Arc<DB>> = std::sync::OnceLock::new();

/// fsync the split hotstuff store's WAL (no-op under the legacy shared layout).
/// Called by the app's commit-boundary sync when `TORUS_SYNC_WAL_ON_COMMIT` is
/// on, right after the state DB's own WAL fsync.
pub fn sync_split_store_wal() -> Result<(), rocksdb::Error> {
    match SPLIT_DB.get() {
        Some(db) => db.flush_wal(true),
        None => Ok(()),
    }
}

/// Open the node's consensus-side split RocksDB instance under
/// `data_dir/consensus-kv` — the hotstuff block tree plus the native-DA
/// body/shard CFs — or `None` when `TORUS_CONSENSUS_KV_SPLIT=0` (legacy: all
/// of it stays in the shared state DB). Publishes the handle for the
/// commit-boundary WAL fsync. Open it ONCE per process (RocksDB LOCK).
pub fn open_split_db(data_dir: &Path) -> Result<Option<StateDb>, String> {
    if !consensus_kv_split_enabled() {
        tracing::info!(
            "consensus split store: OFF (TORUS_CONSENSUS_KV_SPLIT=0) — hotstuff KVStore and native DA stay in the shared state DB"
        );
        return Ok(None);
    }
    let split = open_split_db_at(data_dir)?;
    // First open wins; a later (re)open in-process keeps syncing the
    // original handle, which is the same on-disk WAL anyway.
    let _ = SPLIT_DB.set(split.db_arc());
    Ok(Some(split))
}

fn open_split_db_at(data_dir: &Path) -> Result<StateDb, String> {
    let path = RocksKVStore::split_path(data_dir);
    std::fs::create_dir_all(&path).map_err(|e| format!("create {}: {e}", path.display()))?;
    let split = StateDb::open_consensus_split(&path)
        .map_err(|e| format!("open consensus split store {}: {e}", path.display()))?;
    tracing::info!(path = %path.display(), "consensus split store: ON (hotstuff block tree + native DA)");
    Ok(split)
}

/// True iff `key` belongs to the hotstuff block tree (vs. an app key that
/// merely shares the legacy `cf_consensus_meta` CF).
fn is_hotstuff_key(key: &[u8]) -> bool {
    key.first().map_or(false, |b| *b <= HOTSTUFF_KEY_PREFIX_MAX)
}

/// RocksDB-backed [`KVStore`] for hotstuff_rs.
///
/// Wraps an `Arc<DB>` scoped to the `cf_consensus_meta` column family.
/// Satisfies `Clone + Send + 'static` as required by the library.
#[derive(Clone)]
pub struct RocksKVStore {
    db: Arc<DB>,
    /// Optional write-latency witness (`torus_consensus_kv_write_seconds`).
    metrics: Option<Arc<Metrics>>,
}

impl RocksKVStore {
    /// Create from a shared database handle (production use).
    ///
    /// The database must already have the `cf_consensus_meta` column family
    /// (e.g., opened via `torus_state::StateDb`).
    pub fn new(db: Arc<DB>) -> Self {
        Self { db, metrics: None }
    }

    /// Attach the metrics registry so every [`KVStore::write`] observes its
    /// wall time on `torus_consensus_kv_write_seconds`.
    pub fn with_metrics(mut self, metrics: Arc<Metrics>) -> Self {
        self.metrics = Some(metrics);
        self
    }

    /// Open a standalone consensus database (useful for testing).
    ///
    /// Creates a RocksDB instance with only the `cf_consensus_meta` column family.
    pub fn open(path: &Path) -> Self {
        Self::try_open(path).expect("open consensus DB")
    }

    fn try_open(path: &Path) -> Result<Self, rocksdb::Error> {
        let mut opts = Options::default();
        opts.create_if_missing(true);
        opts.create_missing_column_families(true);
        // Small, hot, rewritten-every-block keyset (same sizing the legacy CF
        // used, S405): flush early so the skiplist stays short.
        let mut cf_opts = Options::default();
        cf_opts.set_write_buffer_size(8 * 1024 * 1024);
        cf_opts.set_max_write_buffer_number(4);
        let cf = ColumnFamilyDescriptor::new(CF_NAME, cf_opts);
        let db = DB::open_cf_descriptors(&opts, path, vec![cf])?;
        Ok(Self {
            db: Arc::new(db),
            metrics: None,
        })
    }

    /// Path of the split store under `data_dir`.
    pub fn split_path(data_dir: &Path) -> PathBuf {
        data_dir.join(CONSENSUS_KV_SUBDIR)
    }

    /// Open the node's hotstuff store: the split instance under
    /// `data_dir/consensus-kv` (default), migrating the block tree out of the
    /// legacy shared CF on first open, or the legacy shared-CF store when
    /// `TORUS_CONSENSUS_KV_SPLIT=0`. Offline tooling entry point — the node
    /// itself calls [`open_split_db`] once and hands the same instance to both
    /// [`RocksKVStore::from_split`] and its native-DA store.
    pub fn open_for_node(data_dir: &Path, legacy: Arc<DB>) -> Result<Self, String> {
        match open_split_db(data_dir)? {
            Some(split) => Self::from_split(&split, &legacy),
            None => Ok(Self::new(legacy)),
        }
    }

    /// The hotstuff store inside an already-open split instance (see
    /// [`open_split_db`]). When it holds no hotstuff keys and `legacy` (the
    /// shared state DB) does, the block tree is copied over first (one-time
    /// migration; the legacy keys are left in place and simply never read
    /// again). Idempotent: an already-migrated store is used as-is.
    pub fn from_split(split: &StateDb, legacy: &Arc<DB>) -> Result<Self, String> {
        let store = Self::new(split.db_arc());
        let migrated = store
            .migrate_from_legacy(legacy)
            .map_err(|e| format!("migrate hotstuff block tree into the split store: {e}"))?;
        match migrated {
            Some(n) => tracing::warn!(
                keys = n,
                "hotstuff KVStore: migrated block tree from the shared state DB into the split store (one-time)"
            ),
            None => tracing::info!("hotstuff KVStore: split RocksDB instance"),
        }
        Ok(store)
    }

    /// Test/tooling helper: force-open the split instance under `data_dir`
    /// (ignoring the env knob) and return its hotstuff store.
    pub fn open_split(data_dir: &Path, legacy: &Arc<DB>) -> Result<Self, String> {
        let split = open_split_db_at(data_dir)?;
        Self::from_split(&split, legacy)
    }

    /// Read-only view of the node's hotstuff store for offline tooling
    /// (`torus-unwedge --inspect`): the split instance when its directory
    /// exists and the split is enabled, else the legacy shared handle. Works
    /// against a LIVE node (no LOCK contention), seeing data as of the last
    /// flush like `StateDb::open_read_only`.
    pub fn open_read_only_for_node(data_dir: &Path, legacy: Arc<DB>) -> Result<Self, String> {
        let path = Self::split_path(data_dir);
        if !consensus_kv_split_enabled() || !path.is_dir() {
            return Ok(Self::new(legacy));
        }
        let opts = Options::default();
        let cf = ColumnFamilyDescriptor::new(CF_NAME, Options::default());
        let db = DB::open_cf_descriptors_read_only(&opts, &path, vec![cf], false)
            .map_err(|e| format!("read-only open {}: {e}", path.display()))?;
        Ok(Self {
            db: Arc::new(db),
            metrics: None,
        })
    }

    /// Copy every hotstuff key from `legacy`'s `cf_consensus_meta` into this
    /// store iff this store has none yet and `legacy` has some. Returns the
    /// number of keys copied (`None` when no migration was needed).
    fn migrate_from_legacy(&self, legacy: &Arc<DB>) -> Result<Option<usize>, rocksdb::Error> {
        if self.has_hotstuff_keys() {
            return Ok(None);
        }
        let Some(legacy_cf) = legacy.cf_handle(CF_NAME) else {
            return Ok(None);
        };
        let dst_cf = self
            .db
            .cf_handle(CF_NAME)
            .expect("cf_consensus_meta missing");
        let mut batch = rocksdb::WriteBatch::default();
        let mut copied = 0usize;
        for item in legacy.iterator_cf(legacy_cf, rocksdb::IteratorMode::Start) {
            let (k, v) = item?;
            if !is_hotstuff_key(&k) {
                continue;
            }
            batch.put_cf(dst_cf, &k, &v);
            copied += 1;
            if batch.len() >= 10_000 {
                self.db.write(std::mem::take(&mut batch))?;
            }
        }
        if copied == 0 {
            return Ok(None);
        }
        if !batch.is_empty() {
            self.db.write(batch)?;
        }
        // Make the migrated tree durable before the node builds on it.
        self.db.flush_wal(true)?;
        Ok(Some(copied))
    }

    /// Does this store hold any hotstuff key at all?
    fn has_hotstuff_keys(&self) -> bool {
        let Some(cf) = self.db.cf_handle(CF_NAME) else {
            return false;
        };
        self.db
            .iterator_cf(cf, rocksdb::IteratorMode::Start)
            .flatten()
            .any(|(k, _)| is_hotstuff_key(&k))
    }
}

impl KVGet for RocksKVStore {
    fn get(&self, key: &[u8]) -> Option<Vec<u8>> {
        let cf = self.db.cf_handle(CF_NAME)?;
        self.db.get_cf(cf, key).ok().flatten()
    }
}

// --- WriteBatch ---

enum WriteOp {
    Set(Vec<u8>, Vec<u8>),
    Delete(Vec<u8>),
}

/// Buffered write batch. Operations are collected and applied atomically
/// when passed to [`KVStore::write`].
pub struct RocksWriteBatch {
    ops: Vec<WriteOp>,
}

impl WriteBatch for RocksWriteBatch {
    fn new() -> Self {
        Self { ops: Vec::new() }
    }

    fn set(&mut self, key: &[u8], value: &[u8]) {
        self.ops.push(WriteOp::Set(key.to_vec(), value.to_vec()));
    }

    fn delete(&mut self, key: &[u8]) {
        self.ops.push(WriteOp::Delete(key.to_vec()));
    }
}

// --- Snapshot ---

/// Point-in-time read snapshot of the consensus column family.
pub struct RocksSnapshot<'a> {
    snap: rocksdb::SnapshotWithThreadMode<'a, DB>,
    db: &'a DB,
}

impl KVGet for RocksSnapshot<'_> {
    fn get(&self, key: &[u8]) -> Option<Vec<u8>> {
        let cf = self.db.cf_handle(CF_NAME)?;
        self.snap.get_cf(cf, key).ok().flatten()
    }
}

// --- KVStore ---

impl KVStore for RocksKVStore {
    type WriteBatch = RocksWriteBatch;
    type Snapshot<'a> = RocksSnapshot<'a>;

    fn write(&mut self, wb: RocksWriteBatch) {
        let cf = self
            .db
            .cf_handle(CF_NAME)
            .expect("cf_consensus_meta missing");
        let mut batch = rocksdb::WriteBatch::default();
        for op in wb.ops {
            match op {
                WriteOp::Set(k, v) => batch.put_cf(cf, &k, &v),
                WriteOp::Delete(k) => batch.delete_cf(cf, &k),
            }
        }
        let timer = std::time::Instant::now();
        self.db.write(batch).expect("RocksDB write failed");
        if let Some(ref m) = self.metrics {
            m.consensus_kv_write_seconds
                .observe(timer.elapsed().as_secs_f64());
        }
    }

    fn clear(&mut self) {
        let cf = self
            .db
            .cf_handle(CF_NAME)
            .expect("cf_consensus_meta missing");
        let iter = self.db.iterator_cf(cf, rocksdb::IteratorMode::Start);
        let mut batch = rocksdb::WriteBatch::default();
        for (key, _) in iter.flatten() {
            batch.delete_cf(cf, &key);
        }
        if !batch.is_empty() {
            self.db.write(batch).expect("RocksDB clear failed");
        }
    }

    fn snapshot(&self) -> RocksSnapshot<'_> {
        let db_ref: &DB = &self.db;
        RocksSnapshot {
            snap: db_ref.snapshot(),
            db: db_ref,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn kv_store_basic_operations() {
        let dir = TempDir::new().unwrap();
        let mut store = RocksKVStore::open(dir.path());

        assert!(store.get(b"key1").is_none());

        let mut wb = RocksWriteBatch::new();
        wb.set(b"key1", b"value1");
        wb.set(b"key2", b"value2");
        store.write(wb);

        assert_eq!(store.get(b"key1").unwrap(), b"value1");
        assert_eq!(store.get(b"key2").unwrap(), b"value2");

        // Snapshot reads
        {
            let snap = store.snapshot();
            assert_eq!(snap.get(b"key1").unwrap(), b"value1");
        }

        // Delete
        let mut wb = RocksWriteBatch::new();
        wb.delete(b"key1");
        store.write(wb);
        assert!(store.get(b"key1").is_none());
        assert_eq!(store.get(b"key2").unwrap(), b"value2");

        // Clear
        store.clear();
        assert!(store.get(b"key2").is_none());
    }

    #[test]
    fn kv_store_clone_shares_data() {
        let dir = TempDir::new().unwrap();
        let mut store = RocksKVStore::open(dir.path());

        let mut wb = RocksWriteBatch::new();
        wb.set(b"shared", b"data");
        store.write(wb);

        let clone = store.clone();
        assert_eq!(clone.get(b"shared").unwrap(), b"data");
    }

    /// Legacy shared-CF store: a `torus_state::StateDb` (all CFs) seeded with
    /// hotstuff-shaped keys (single-byte prefixes) AND the ASCII app keys that
    /// share `cf_consensus_meta`.
    fn legacy_with_block_tree(dir: &Path) -> torus_state::StateDb {
        let db = torus_state::StateDb::open(dir).unwrap();
        let cf = torus_state::cf::CF_CONSENSUS_META;
        db.put_cf_raw(cf, &[8u8], &7u64.to_le_bytes()).unwrap(); // HIGHEST_VIEW_ENTERED
        db.put_cf_raw(cf, &[0u8, 0xaa, 0xbb, 4], b"block-field").unwrap(); // BLOCKS + hash + field
        db.put_cf_raw(cf, &[23u8], &3u64.to_le_bytes()).unwrap(); // APP_FED_BLOCK_HEIGHT
        db.put_cf_raw(cf, b"native_applied_height", &2u64.to_be_bytes())
            .unwrap();
        db.put_cf_raw(cf, b"pending_rotation:xyz", b"rot").unwrap();
        db
    }

    #[test]
    fn split_store_migrates_hotstuff_keys_once_and_leaves_app_keys() {
        let dir = TempDir::new().unwrap();
        let legacy = legacy_with_block_tree(dir.path());

        let store = RocksKVStore::open_split(dir.path(), &legacy.db_arc()).unwrap();
        // Hotstuff keys copied verbatim.
        assert_eq!(store.get(&[8u8]).unwrap(), 7u64.to_le_bytes());
        assert_eq!(store.get(&[0u8, 0xaa, 0xbb, 4]).unwrap(), b"block-field");
        assert_eq!(store.get(&[23u8]).unwrap(), 3u64.to_le_bytes());
        // ASCII app keys are NOT the block tree: never copied.
        assert!(store.get(b"native_applied_height").is_none());
        assert!(store.get(b"pending_rotation:xyz").is_none());
        // Legacy keys untouched (still readable by the app through the state DB).
        assert_eq!(
            legacy
                .get_cf_raw(torus_state::cf::CF_CONSENSUS_META, b"native_applied_height")
                .unwrap()
                .unwrap(),
            2u64.to_be_bytes()
        );
        assert_eq!(
            legacy
                .get_cf_raw(torus_state::cf::CF_CONSENSUS_META, &[8u8])
                .unwrap()
                .unwrap(),
            7u64.to_le_bytes()
        );

        // Advance the split store, then mutate the legacy tree behind its back:
        // a re-open must NOT re-migrate (the split store is authoritative now).
        let mut store = store;
        let mut wb = RocksWriteBatch::new();
        wb.set(&[8u8], &9u64.to_le_bytes());
        store.write(wb);
        drop(store);
        legacy
            .put_cf_raw(torus_state::cf::CF_CONSENSUS_META, &[8u8], &1u64.to_le_bytes())
            .unwrap();
        legacy
            .put_cf_raw(torus_state::cf::CF_CONSENSUS_META, &[9u8], b"late-highest-pc")
            .unwrap();
        let reopened = RocksKVStore::open_split(dir.path(), &legacy.db_arc()).unwrap();
        assert_eq!(reopened.get(&[8u8]).unwrap(), 9u64.to_le_bytes());
        assert!(reopened.get(&[9u8]).is_none());
    }

    #[test]
    fn split_store_fresh_when_legacy_has_no_block_tree() {
        let dir = TempDir::new().unwrap();
        let legacy = torus_state::StateDb::open(dir.path()).unwrap();
        // Only an app key in the legacy CF: nothing to migrate, store starts empty.
        legacy
            .put_cf_raw(
                torus_state::cf::CF_CONSENSUS_META,
                b"native_applied_height",
                &0u64.to_be_bytes(),
            )
            .unwrap();
        let store = RocksKVStore::open_split(dir.path(), &legacy.db_arc()).unwrap();
        assert!(!store.has_hotstuff_keys());
        assert!(RocksKVStore::split_path(dir.path()).is_dir());
    }

    #[test]
    fn split_env_knob_defaults_on_and_accepts_off_spellings() {
        assert!(consensus_kv_split_from_env(None));
        assert!(consensus_kv_split_from_env(Some("1")));
        assert!(consensus_kv_split_from_env(Some("yes")));
        for off in ["0", "false", "OFF", " no "] {
            assert!(!consensus_kv_split_from_env(Some(off)), "{off}");
        }
    }

    #[test]
    fn hotstuff_key_classifier() {
        assert!(is_hotstuff_key(&[0]));
        assert!(is_hotstuff_key(&[23, 1, 2]));
        assert!(is_hotstuff_key(&[0x1f]));
        assert!(!is_hotstuff_key(&[0x20]));
        assert!(!is_hotstuff_key(b"native_applied_height"));
        assert!(!is_hotstuff_key(b""));
    }

    #[test]
    fn write_observes_kv_write_histogram() {
        let dir = TempDir::new().unwrap();
        let metrics = Arc::new(Metrics::new());
        let mut store = RocksKVStore::open(dir.path()).with_metrics(metrics.clone());
        let mut wb = RocksWriteBatch::new();
        wb.set(b"k", b"v");
        store.write(wb);
        let text = metrics.encode();
        let count = text
            .lines()
            .find(|l| l.starts_with("torus_consensus_kv_write_seconds_count "))
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(0.0);
        assert_eq!(count, 1.0, "{text}");
    }
}
