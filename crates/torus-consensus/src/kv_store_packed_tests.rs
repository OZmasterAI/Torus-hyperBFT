use super::*;
use std::panic::{catch_unwind, AssertUnwindSafe};
use tempfile::TempDir;

// Independent original staging representation and RocksDB replay.
enum LegacyOp {
    Set(Vec<u8>, Vec<u8>),
    Delete(Vec<u8>),
}

fn old_write(store: &RocksKVStore, ops: &[LegacyOp]) {
    let cf = store
        .db
        .cf_handle(CF_NAME)
        .expect("cf_consensus_meta missing");
    let mut batch = rocksdb::WriteBatch::default();
    for op in ops {
        match op {
            LegacyOp::Set(key, value) => batch.put_cf(cf, key, value),
            LegacyOp::Delete(key) => batch.delete_cf(cf, key),
        }
    }
    store.db.write(batch).expect("RocksDB write failed");
}

fn pack(ops: &[LegacyOp]) -> RocksWriteBatch {
    let mut batch = RocksWriteBatch::new();
    for op in ops {
        match op {
            LegacyOp::Set(key, value) => batch.set(key, value),
            LegacyOp::Delete(key) => batch.delete(key),
        }
    }
    batch
}

fn dump(store: &RocksKVStore) -> Vec<(Vec<u8>, Vec<u8>)> {
    let cf = store.db.cf_handle(CF_NAME).unwrap();
    store
        .db
        .iterator_cf(cf, rocksdb::IteratorMode::Start)
        .map(|item| {
            let (key, value) = item.unwrap();
            (key.to_vec(), value.to_vec())
        })
        .collect()
}

fn panic_message(panic: Box<dyn std::any::Any + Send>) -> String {
    if let Some(message) = panic.downcast_ref::<String>() {
        message.clone()
    } else if let Some(message) = panic.downcast_ref::<&str>() {
        message.to_string()
    } else {
        panic!("unexpected panic payload")
    }
}

#[test]
fn packed_batch_matches_legacy_order_and_raw_cf_bytes() {
    let old_dir = TempDir::new().unwrap();
    let new_dir = TempDir::new().unwrap();
    let old = RocksKVStore::open(old_dir.path());
    let mut new = RocksKVStore::open(new_dir.path());
    let mut ops = vec![
        LegacyOp::Set(vec![], vec![]),
        LegacyOp::Set(b"repeat".to_vec(), b"first".to_vec()),
        LegacyOp::Delete(b"repeat".to_vec()),
        LegacyOp::Set(b"repeat".to_vec(), b"last".to_vec()),
        LegacyOp::Set(b"deleted".to_vec(), b"value".to_vec()),
        LegacyOp::Delete(b"deleted".to_vec()),
    ];
    // Force arena growth while overwriting binary keys and interleaving deletes.
    for i in 0..96u8 {
        let key = vec![0, i % 11, 255];
        if i % 4 == 0 {
            ops.push(LegacyOp::Delete(key));
        } else {
            ops.push(LegacyOp::Set(
                key,
                vec![i; if i % 5 == 0 { 4096 } else { i as usize }],
            ));
        }
    }
    for part in ops.chunks(17) {
        let packed = pack(part);
        let logical_bytes: usize = part
            .iter()
            .map(|op| match op {
                LegacyOp::Set(k, v) => k.len() + v.len(),
                LegacyOp::Delete(k) => k.len(),
            })
            .sum();
        assert_eq!(packed.bytes.len(), logical_bytes);
        assert_eq!(packed.ops.len(), part.len());
        old_write(&old, part);
        new.write(packed);
        assert_eq!(dump(&new), dump(&old));
    }
    assert_eq!(new.get(b"repeat"), Some(b"last".to_vec()));
    assert_eq!(new.get(b"deleted"), None);
    assert_eq!(new.get(b""), Some(vec![]));
    new.write(RocksWriteBatch::new());
    old_write(&old, &[]);
    assert_eq!(dump(&new), dump(&old));
}

#[test]
fn packed_batch_owns_input_and_preserves_snapshot_clone_reopen_and_drop() {
    let dir = TempDir::new().unwrap();
    {
        let mut store = RocksKVStore::open(dir.path());
        let mut batch = RocksWriteBatch::new();
        let mut key = b"key".to_vec();
        let mut value = b"before".to_vec();
        batch.set(&key, &value);
        key.fill(0);
        value.fill(0);
        assert!(store.get(b"key").is_none(), "staging must not write");
        store.write(batch);
        let reader = store.clone();
        let snapshot = reader.snapshot();
        let mut update = RocksWriteBatch::new();
        update.delete(b"key");
        update.set(b"key", b"after");
        update.set(b"empty", b"");
        store.write(update);
        assert_eq!(snapshot.get(b"key"), Some(b"before".to_vec()));
        assert_eq!(reader.get(b"key"), Some(b"after".to_vec()));
        let mut abandoned = RocksWriteBatch::new();
        abandoned.delete(b"key");
        abandoned.set(b"never-written", b"value");
        drop(abandoned);
        assert_eq!(store.get(b"key"), Some(b"after".to_vec()));
        assert!(store.get(b"never-written").is_none());
    }
    let reopened = RocksKVStore::open(dir.path());
    assert_eq!(reopened.get(b"key"), Some(b"after".to_vec()));
    assert_eq!(reopened.get(b"empty"), Some(vec![]));
    assert!(reopened.get(b"never-written").is_none());
}

#[test]
fn packed_batch_missing_cf_panics_only_at_write_even_when_empty() {
    let dir = TempDir::new().unwrap();
    let mut opts = Options::default();
    opts.create_if_missing(true);
    let db = Arc::new(DB::open(&opts, dir.path()).unwrap());
    let mut store = RocksKVStore::new(db);
    for ops in [
        vec![],
        vec![
            LegacyOp::Set(b"k".to_vec(), b"v".to_vec()),
            LegacyOp::Delete(b"k".to_vec()),
        ],
    ] {
        let packed = pack(&ops); // No CF lookup or write during construction.
        let expected = catch_unwind(AssertUnwindSafe(|| old_write(&store, &ops))).unwrap_err();
        let actual = catch_unwind(AssertUnwindSafe(|| store.write(packed))).unwrap_err();
        assert_eq!(panic_message(actual), panic_message(expected));
    }
}

#[test]
fn packed_batch_read_only_write_failure_keeps_original_panic_and_rows() {
    let dir = TempDir::new().unwrap();
    {
        let store = RocksKVStore::open(dir.path());
        old_write(
            &store,
            &[LegacyOp::Set(b"existing".to_vec(), b"old".to_vec())],
        );
    }
    let db = DB::open_cf_for_read_only(&Options::default(), dir.path(), [CF_NAME], false).unwrap();
    let mut store = RocksKVStore::new(Arc::new(db));
    let ops = vec![
        LegacyOp::Delete(b"existing".to_vec()),
        LegacyOp::Set(b"new".to_vec(), b"value".to_vec()),
    ];
    let expected = catch_unwind(AssertUnwindSafe(|| old_write(&store, &ops))).unwrap_err();
    let actual = catch_unwind(AssertUnwindSafe(|| store.write(pack(&ops)))).unwrap_err();
    let expected = panic_message(expected);
    assert!(expected.starts_with("RocksDB write failed"));
    assert_eq!(panic_message(actual), expected);
    assert_eq!(store.get(b"existing"), Some(b"old".to_vec()));
    assert_eq!(store.get(b"new"), None);
}

/// s65 item A: only batches carrying the vote-state key are timed into
/// `torus_vote_state_write_seconds`; every other consensus write is not.
#[test]
fn vote_state_writes_are_timed_and_other_writes_are_not() {
    use hotstuff_rs::block_tree::variables::HIGHEST_VIEW_PHASE_VOTED;
    let dir = TempDir::new().unwrap();
    let metrics = Arc::new(torus_telemetry::Metrics::new());
    let mut store = RocksKVStore::open(dir.path()).with_metrics(metrics.clone());

    store.write(pack(&[LegacyOp::Set(b"other".to_vec(), b"v".to_vec())]));
    assert!(metrics
        .encode()
        .contains("torus_vote_state_write_seconds_count 0"));

    store.write(pack(&[
        LegacyOp::Set(HIGHEST_VIEW_PHASE_VOTED.to_vec(), 7u64.to_le_bytes().to_vec()),
        LegacyOp::Set(b"last-voted".to_vec(), b"x".to_vec()),
    ]));
    assert!(metrics
        .encode()
        .contains("torus_vote_state_write_seconds_count 1"));
    assert_eq!(
        store.get(&HIGHEST_VIEW_PHASE_VOTED),
        Some(7u64.to_le_bytes().to_vec()),
        "timing must not change what is written"
    );
}
