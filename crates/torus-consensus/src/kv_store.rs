//! RocksDB-backed KVStore for hotstuff_rs consensus-internal storage.
//!
//! Scoped to the `cf_consensus_meta` column family. All block tree state
//! (blocks, PCs, TCs, validator set, app state) is stored here by the library.

use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use hotstuff_rs::block_tree::pluggables::{KVGet, KVStore, WriteBatch};
use hotstuff_rs::block_tree::variables::{HIGHEST_VIEW_PHASE_VOTED, LAST_VOTED_PROPOSAL};
use rocksdb::{ColumnFamilyDescriptor, Options, DB};

const CF_NAME: &str = "cf_consensus_meta";

/// The two keys written before a vote is sent (s67: kept in their own DB).
fn is_vote_key(key: &[u8]) -> bool {
    key == HIGHEST_VIEW_PHASE_VOTED || key == LAST_VOTED_PROPOSAL
}

/// Open the small vote-state DB (default CF only). It gets its own WAL and write
/// group, so the pre-send vote save never queues behind exec batches in the
/// shared state DB.
pub fn open_vote_db(path: &Path) -> Result<Arc<DB>, rocksdb::Error> {
    let mut opts = Options::default();
    opts.create_if_missing(true);
    opts.set_write_buffer_size(4 * 1024 * 1024);
    Ok(Arc::new(DB::open(&opts, path)?))
}

/// RocksDB-backed [`KVStore`] for hotstuff_rs.
///
/// Wraps an `Arc<DB>` scoped to the `cf_consensus_meta` column family.
/// Satisfies `Clone + Send + 'static` as required by the library.
#[derive(Clone)]
pub struct RocksKVStore {
    db: Arc<DB>,
    /// When set, the vote keys are written here instead of `db`; reads fall
    /// back to `db` for vote state written before the upgrade (s67).
    vote_db: Option<Arc<DB>>,
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
        Self {
            db,
            vote_db: None,
            metrics: None,
        }
    }

    /// Keep the vote keys in `vote_db` (see [`open_vote_db`]).
    pub fn with_vote_db(mut self, vote_db: Arc<DB>) -> Self {
        self.vote_db = Some(vote_db);
        self
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
            vote_db: None,
            metrics: None,
        }
    }
}

/// Vote-key read from the vote DB, if attached and holding the key. A read
/// error panics: falling back to the main DB could return an older vote state
/// and allow a double vote. (hotstuff_rs never deletes the vote keys, so a
/// fallback never resurrects a deleted value.)
fn get_vote(vote_db: Option<&DB>, key: &[u8]) -> Option<Vec<u8>> {
    vote_db
        .filter(|_| is_vote_key(key))
        .and_then(|db| db.get(key).expect("vote DB read failed"))
}

impl KVGet for RocksKVStore {
    fn get(&self, key: &[u8]) -> Option<Vec<u8>> {
        if let Some(value) = get_vote(self.vote_db.as_deref(), key) {
            return Some(value);
        }
        let cf = self.db.cf_handle(CF_NAME)?;
        self.db.get_cf(cf, key).ok().flatten()
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
    /// Vote keys are read live from here (not snapshotted): they only grow.
    vote_db: Option<&'a DB>,
}

impl KVGet for RocksSnapshot<'_> {
    fn get(&self, key: &[u8]) -> Option<Vec<u8>> {
        if let Some(value) = get_vote(self.vote_db, key) {
            return Some(value);
        }
        let cf = self.db.cf_handle(CF_NAME)?;
        self.snap.get_cf(cf, key).ok().flatten()
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
        if let Some(vote_db) = &self.vote_db {
            let op_key = |op: &WriteOp| match *op {
                WriteOp::Set { start, key_end, .. } => &wb.bytes[start..key_end],
                WriteOp::Delete { start, end } => &wb.bytes[start..end],
            };
            let votes = wb.ops.iter().filter(|op| is_vote_key(op_key(op))).count();
            if votes > 0 {
                // hotstuff_rs writes the vote keys in batches of their own;
                // splitting a mixed batch across two DBs would not be atomic.
                assert_eq!(votes, wb.ops.len(), "batch mixes vote keys with other keys");
                let mut batch = rocksdb::WriteBatch::default();
                for op in &wb.ops {
                    match *op {
                        WriteOp::Set {
                            start,
                            key_end,
                            end,
                        } => batch.put(&wb.bytes[start..key_end], &wb.bytes[key_end..end]),
                        WriteOp::Delete { start, end } => batch.delete(&wb.bytes[start..end]),
                    }
                }
                vote_db.write(batch).expect("vote DB write failed");
                if let Some((m, t)) = started {
                    m.vote_state_write_seconds
                        .observe(t.elapsed().as_secs_f64());
                }
                return;
            }
        }
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
        for (key, _) in iter.flatten() {
            batch.delete_cf(cf, &key);
        }
        if !batch.is_empty() {
            self.db.write(batch).expect("RocksDB clear failed");
        }
        if let Some(vote_db) = &self.vote_db {
            let mut batch = rocksdb::WriteBatch::default();
            for (key, _) in vote_db.iterator(rocksdb::IteratorMode::Start).flatten() {
                batch.delete(&key);
            }
            vote_db.write(batch).expect("vote DB clear failed");
        }
    }

    fn snapshot(&self) -> RocksSnapshot<'_> {
        let db_ref: &DB = &self.db;
        RocksSnapshot {
            snap: db_ref.snapshot(),
            db: db_ref,
            vote_db: self.vote_db.as_deref(),
        }
    }
}

#[cfg(test)]
#[path = "kv_store_packed_tests.rs"]
mod packed_tests;

#[cfg(test)]
#[path = "kv_store_vote_db_tests.rs"]
mod vote_db_tests;

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
}
