//! RocksDB-backed KVStore for hotstuff_rs consensus-internal storage.
//!
//! Scoped to the `cf_consensus_meta` column family. All block tree state
//! (blocks, PCs, TCs, validator set, app state) is stored here by the library.

use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use hotstuff_rs::block_tree::pluggables::{KVGet, KVStore, WriteBatch};
use hotstuff_rs::block_tree::variables::HIGHEST_VIEW_PHASE_VOTED;
use rocksdb::{ColumnFamilyDescriptor, Options, DB};

const CF_NAME: &str = "cf_consensus_meta";

/// RocksDB-backed [`KVStore`] for hotstuff_rs.
///
/// Wraps an `Arc<DB>` scoped to the `cf_consensus_meta` column family.
/// Satisfies `Clone + Send + 'static` as required by the library.
#[derive(Clone)]
pub struct RocksKVStore {
    db: Arc<DB>,
    /// When set, vote-state writes are timed into
    /// `torus_vote_state_write_seconds` (s65 item A).
    metrics: Option<Arc<torus_telemetry::Metrics>>,
}

impl RocksKVStore {
    /// Create from a shared database handle (production use).
    ///
    /// The database must already have the `cf_consensus_meta` column family
    /// (e.g., opened via `torus_state::StateDb`).
    pub fn new(db: Arc<DB>) -> Self {
        Self { db, metrics: None }
    }

    /// Time vote-state writes (the batch carrying `HIGHEST_VIEW_PHASE_VOTED`),
    /// which since f05b20e happen before the vote is sent.
    pub fn with_metrics(mut self, metrics: Arc<torus_telemetry::Metrics>) -> Self {
        self.metrics = Some(metrics);
        self
    }

    /// Open a standalone consensus database (useful for testing).
    ///
    /// Creates a RocksDB instance with only the `cf_consensus_meta` column family.
    pub fn open(path: &Path) -> Self {
        let mut opts = Options::default();
        opts.create_if_missing(true);
        opts.create_missing_column_families(true);
        let cf = ColumnFamilyDescriptor::new(CF_NAME, Options::default());
        let db = DB::open_cf_descriptors(&opts, path, vec![cf]).expect("open consensus DB");
        Self {
            db: Arc::new(db),
            metrics: None,
        }
    }
}

impl KVGet for RocksKVStore {
    fn get(&self, key: &[u8]) -> Option<Vec<u8>> {
        let cf = self.db.cf_handle(CF_NAME)?;
        // R01b: a read error is a local storage fault, never "absent" (hotstuff
        // would act on a block / PC / view it reads as never written). The
        // trait is infallible, so fail here; on the consensus threads the
        // node's panic hook turns this into a fail-stop (exit 70).
        self.db
            .get_cf(cf, key)
            .unwrap_or_else(|e| panic!("consensus KV read failed: {e}"))
    }
}

// --- WriteBatch ---

enum WriteOp {
    Set {
        start: usize,
        key_end: usize,
        end: usize,
    },
    Delete {
        start: usize,
        end: usize,
    },
}

/// Buffered write batch. Operations are collected and applied atomically
/// when passed to [`KVStore::write`].
pub struct RocksWriteBatch {
    bytes: Vec<u8>,
    ops: Vec<WriteOp>,
}

impl WriteBatch for RocksWriteBatch {
    fn new() -> Self {
        Self {
            bytes: Vec::new(),
            ops: Vec::new(),
        }
    }

    fn set(&mut self, key: &[u8], value: &[u8]) {
        // Keys and values remain owned by this batch, but share one arena.
        // Reserve once so a single row cannot trigger separate key/value growth.
        self.bytes.reserve(key.len().saturating_add(value.len()));
        let start = self.bytes.len();
        self.bytes.extend_from_slice(key);
        let key_end = self.bytes.len();
        self.bytes.extend_from_slice(value);
        self.ops.push(WriteOp::Set {
            start,
            key_end,
            end: self.bytes.len(),
        });
    }

    fn delete(&mut self, key: &[u8]) {
        let start = self.bytes.len();
        self.bytes.extend_from_slice(key);
        self.ops.push(WriteOp::Delete {
            start,
            end: self.bytes.len(),
        });
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
        // R01b: as `RocksKVStore::get`, an error is a fault, not `None`.
        self.snap
            .get_cf(cf, key)
            .unwrap_or_else(|e| panic!("consensus KV snapshot read failed: {e}"))
    }
}

// --- KVStore ---

impl KVStore for RocksKVStore {
    type WriteBatch = RocksWriteBatch;
    type Snapshot<'a> = RocksSnapshot<'a>;

    fn write(&mut self, wb: RocksWriteBatch) {
        let started = self
            .metrics
            .as_ref()
            .filter(|_| {
                wb.ops.iter().any(|op| {
                    matches!(op, WriteOp::Set { start, key_end, .. }
                        if wb.bytes[*start..*key_end] == HIGHEST_VIEW_PHASE_VOTED[..])
                })
            })
            .map(|m| (m, Instant::now()));
        let cf = self
            .db
            .cf_handle(CF_NAME)
            .expect("cf_consensus_meta missing");
        let mut batch = rocksdb::WriteBatch::default();
        for op in wb.ops {
            match op {
                WriteOp::Set {
                    start,
                    key_end,
                    end,
                } => {
                    batch.put_cf(cf, &wb.bytes[start..key_end], &wb.bytes[key_end..end]);
                }
                WriteOp::Delete { start, end } => batch.delete_cf(cf, &wb.bytes[start..end]),
            }
        }
        self.db.write(batch).expect("RocksDB write failed");
        if let Some((m, t)) = started {
            m.vote_state_write_seconds
                .observe(t.elapsed().as_secs_f64());
        }
    }

    fn clear(&mut self) {
        let cf = self
            .db
            .cf_handle(CF_NAME)
            .expect("cf_consensus_meta missing");
        let iter = self.db.iterator_cf(cf, rocksdb::IteratorMode::Start);
        let mut batch = rocksdb::WriteBatch::default();
        // R01b: an iterator error must fail, not silently skip rows (which
        // would leave them behind while `clear` reports success).
        for item in iter {
            let (key, _) =
                item.unwrap_or_else(|e| panic!("consensus KV clear: iterator read failed: {e}"));
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
#[path = "kv_store_packed_tests.rs"]
mod packed_tests;

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

    /// R01b: a store whose only SST has a corrupted data block, reopened cold,
    /// so every read of `key` (and a full scan) hits a checksum error.
    fn corrupted_store(key: &[u8]) -> (TempDir, RocksKVStore) {
        let dir = TempDir::new().unwrap();
        {
            let mut store = RocksKVStore::open(dir.path());
            let mut wb = RocksWriteBatch::new();
            wb.set(key, &[0xab; 4096]);
            store.write(wb);
            let cf = store.db.cf_handle(CF_NAME).unwrap();
            store.db.flush_cf(cf).unwrap();
        }
        let ssts: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.extension().is_some_and(|x| x == "sst"))
            .collect();
        assert_eq!(ssts.len(), 1, "one flushed SST expected: {ssts:?}");
        // The first data block starts at offset 0; garbling it breaks its
        // checksum without touching the footer / index RocksDB reads at open.
        let mut bytes = std::fs::read(&ssts[0]).unwrap();
        bytes[..64].fill(0xff);
        std::fs::write(&ssts[0], bytes).unwrap();
        let store = RocksKVStore::open(dir.path());
        (dir, store)
    }

    fn panic_text(p: Box<dyn std::any::Any + Send>) -> String {
        p.downcast_ref::<String>()
            .cloned()
            .or_else(|| p.downcast_ref::<&str>().map(|s| s.to_string()))
            .unwrap_or_default()
    }

    /// R01b: a RocksDB read error must not read as "absent" (`None`): hotstuff
    /// would treat a missing block / PC / view as never written. It fails
    /// instead (a panic, which the node's hook turns into exit 70 on the
    /// consensus threads).
    #[test]
    fn kv_store_read_error_is_not_none() {
        let (_dir, store) = corrupted_store(b"k");
        let err = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| store.get(b"k")))
            .expect_err("a read error must not return");
        assert!(panic_text(err).contains("consensus KV read failed"));
    }

    #[test]
    fn kv_snapshot_read_error_is_not_none() {
        let (_dir, store) = corrupted_store(b"k");
        let snap = store.snapshot();
        let err = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| snap.get(b"k")))
            .expect_err("a snapshot read error must not return");
        assert!(panic_text(err).contains("consensus KV snapshot read failed"));
    }

    /// R01b: `clear` must not skip rows it failed to read (it used to
    /// `flatten()` iterator errors away and report success).
    #[test]
    fn kv_clear_iterator_error_fails() {
        let (_dir, mut store) = corrupted_store(b"k");
        let err = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| store.clear()))
            .expect_err("an iterator error must not be dropped");
        assert!(panic_text(err).contains("consensus KV clear: iterator read failed"));
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
}
