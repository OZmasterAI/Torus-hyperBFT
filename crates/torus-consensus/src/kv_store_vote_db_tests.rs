//! s67: vote state lives in its own small RocksDB so the pre-send vote save does
//! not queue behind exec batches in the shared state DB's write group.

use super::*;
use hotstuff_rs::block_tree::variables::{HIGHEST_VIEW_PHASE_VOTED, LAST_VOTED_PROPOSAL};
use std::panic::{catch_unwind, AssertUnwindSafe};
use tempfile::TempDir;

fn batch(ops: &[(&[u8], &[u8])]) -> RocksWriteBatch {
    let mut wb = RocksWriteBatch::new();
    for (key, value) in ops {
        wb.set(key, value);
    }
    wb
}

fn main_get(db: &DB, key: &[u8]) -> Option<Vec<u8>> {
    db.get_cf(db.cf_handle(CF_NAME).unwrap(), key).unwrap()
}

#[test]
fn vote_keys_go_to_vote_db_and_other_keys_stay_in_main_db() {
    let dir = TempDir::new().unwrap();
    let vote_db = open_vote_db(&dir.path().join("vote_state")).unwrap();
    let metrics = Arc::new(torus_telemetry::Metrics::new());
    let mut store = RocksKVStore::open(&dir.path().join("main"))
        .with_vote_db(vote_db.clone())
        .with_metrics(metrics.clone());

    store.write(batch(&[
        (&HIGHEST_VIEW_PHASE_VOTED, b"view7"),
        (&LAST_VOTED_PROPOSAL, b"prop7"),
    ]));
    store.write(batch(&[(b"block", b"b")]));

    assert_eq!(
        vote_db.get(HIGHEST_VIEW_PHASE_VOTED).unwrap(),
        Some(b"view7".to_vec())
    );
    assert_eq!(
        vote_db.get(LAST_VOTED_PROPOSAL).unwrap(),
        Some(b"prop7".to_vec())
    );
    assert_eq!(vote_db.get(b"block").unwrap(), None);
    assert_eq!(main_get(&store.db, &HIGHEST_VIEW_PHASE_VOTED), None);
    assert_eq!(main_get(&store.db, &LAST_VOTED_PROPOSAL), None);
    assert_eq!(main_get(&store.db, b"block"), Some(b"b".to_vec()));

    assert_eq!(
        store.get(&HIGHEST_VIEW_PHASE_VOTED),
        Some(b"view7".to_vec())
    );
    assert_eq!(
        store.snapshot().get(&LAST_VOTED_PROPOSAL),
        Some(b"prop7".to_vec())
    );
    assert_eq!(store.snapshot().get(b"block"), Some(b"b".to_vec()));
    assert!(metrics
        .encode()
        .contains("torus_vote_state_write_seconds_count 1"));
}

#[test]
fn vote_reads_fall_back_to_main_db_until_vote_db_has_them() {
    let dir = TempDir::new().unwrap();
    // A node that voted before the upgrade: vote state is in the main DB.
    let mut legacy = RocksKVStore::open(&dir.path().join("main"));
    legacy.write(batch(&[
        (&HIGHEST_VIEW_PHASE_VOTED, b"view5"),
        (&LAST_VOTED_PROPOSAL, b"prop5"),
    ]));

    let vote_db = open_vote_db(&dir.path().join("vote_state")).unwrap();
    let mut store = RocksKVStore::new(legacy.db.clone()).with_vote_db(vote_db);
    assert_eq!(
        store.get(&HIGHEST_VIEW_PHASE_VOTED),
        Some(b"view5".to_vec())
    );
    assert_eq!(
        store.snapshot().get(&LAST_VOTED_PROPOSAL),
        Some(b"prop5".to_vec())
    );

    store.write(batch(&[
        (&HIGHEST_VIEW_PHASE_VOTED, b"view9"),
        (&LAST_VOTED_PROPOSAL, b"prop9"),
    ]));
    assert_eq!(
        store.get(&HIGHEST_VIEW_PHASE_VOTED),
        Some(b"view9".to_vec())
    );
    assert_eq!(
        store.snapshot().get(&LAST_VOTED_PROPOSAL),
        Some(b"prop9".to_vec())
    );
}

#[test]
fn batch_mixing_vote_and_other_keys_panics_before_writing() {
    let dir = TempDir::new().unwrap();
    let vote_db = open_vote_db(&dir.path().join("vote_state")).unwrap();
    let mut store = RocksKVStore::open(&dir.path().join("main")).with_vote_db(vote_db.clone());

    let result = catch_unwind(AssertUnwindSafe(|| {
        store.write(batch(&[
            (&HIGHEST_VIEW_PHASE_VOTED, b"view3"),
            (b"block", b"b"),
        ]))
    }));
    assert!(result.is_err(), "a mixed batch must not be split");
    assert_eq!(vote_db.get(HIGHEST_VIEW_PHASE_VOTED).unwrap(), None);
    assert_eq!(main_get(&store.db, b"block"), None);
}

/// The vote DB sits inside the StateDb directory so wiping data_dir wipes both.
/// A leftover, higher highest_view_voted on a fresh chain would stop the node
/// from voting.
#[test]
fn vote_db_inside_state_db_dir_survives_compaction_and_reopen() {
    let dir = TempDir::new().unwrap();
    let vote_path = dir.path().join("vote_state");
    {
        let state_db = torus_state::StateDb::open(dir.path()).unwrap();
        let vote_db = open_vote_db(&vote_path).unwrap();
        let mut store = RocksKVStore::new(state_db.db_arc()).with_vote_db(vote_db.clone());
        store.write(batch(&[(&HIGHEST_VIEW_PHASE_VOTED, b"view4")]));
        store.write(batch(&[(b"block", b"b")]));

        let db = state_db.db_arc();
        let cf = db.cf_handle(CF_NAME).unwrap();
        db.flush_cf(cf).unwrap();
        db.compact_range_cf(cf, None::<&[u8]>, None::<&[u8]>);
        vote_db.flush().unwrap();
    }
    assert!(vote_path.is_dir());

    let state_db = torus_state::StateDb::open(dir.path()).unwrap();
    let store =
        RocksKVStore::new(state_db.db_arc()).with_vote_db(open_vote_db(&vote_path).unwrap());
    assert_eq!(
        store.get(&HIGHEST_VIEW_PHASE_VOTED),
        Some(b"view4".to_vec())
    );
    assert_eq!(store.get(b"block"), Some(b"b".to_vec()));
}

#[test]
fn clear_clears_both_dbs() {
    let dir = TempDir::new().unwrap();
    let vote_db = open_vote_db(&dir.path().join("vote_state")).unwrap();
    let mut store = RocksKVStore::open(&dir.path().join("main")).with_vote_db(vote_db.clone());
    store.write(batch(&[(&HIGHEST_VIEW_PHASE_VOTED, b"view2")]));
    store.write(batch(&[(b"block", b"b")]));

    store.clear();
    assert_eq!(vote_db.get(HIGHEST_VIEW_PHASE_VOTED).unwrap(), None);
    assert_eq!(store.get(&HIGHEST_VIEW_PHASE_VOTED), None);
    assert_eq!(store.get(b"block"), None);
}
