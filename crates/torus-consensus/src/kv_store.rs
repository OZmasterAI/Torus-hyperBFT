//! RocksDB-backed KVStore for hotstuff_rs consensus-internal storage.
//!
//! Scoped to the `cf_consensus_meta` column family. All block tree state
//! (blocks, PCs, TCs, validator set, app state) is stored here by the library.

use std::path::Path;
use std::sync::Arc;

use hotstuff_rs::block_tree::pluggables::{KVGet, KVStore, WriteBatch};
use rocksdb::{ColumnFamilyDescriptor, Options, DB};

const CF_NAME: &str = "cf_consensus_meta";

/// RocksDB-backed [`KVStore`] for hotstuff_rs.
///
/// Wraps an `Arc<DB>` scoped to the `cf_consensus_meta` column family.
/// Satisfies `Clone + Send + 'static` as required by the library.
#[derive(Clone)]
pub struct RocksKVStore {
    db: Arc<DB>,
}

impl RocksKVStore {
    /// Create from a shared database handle (production use).
    ///
    /// The database must already have the `cf_consensus_meta` column family
    /// (e.g., opened via `torus_state::StateDb`).
    pub fn new(db: Arc<DB>) -> Self {
        Self { db }
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
        Self { db: Arc::new(db) }
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
        self.db.write(batch).expect("RocksDB write failed");
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
}
