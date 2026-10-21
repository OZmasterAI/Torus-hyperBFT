//! Section 71.8 option A: the HotStuff store and the DA store live in their
//! own RocksDB (`<data_dir>/consensus-db`), so their small writes do not
//! queue behind the state checkpoint batch in the state DB's write queue.

use super::*;
use crate::native_da::NativeDaStore;
use torus_types::{compute_action_hash, SignedNativeAction};

fn dummy_action(nonce: u64) -> SignedNativeAction {
    use torus_types::{ActionSignature, NativeAction, Signature};
    SignedNativeAction {
        action: NativeAction::ClaimRewards,
        nonce,
        signature: ActionSignature::Eip712(Signature {
            v: 27,
            r: [0u8; 32],
            s: [0u8; 32],
        }),
    }
}

fn stats_tuning() -> DbTuning {
    DbTuning {
        stats_level: 1,
        ..DbTuning::default()
    }
}

/// `rocksdb.wal.synced` of a DB opened with statistics.
fn wal_syncs(db: &StateDb) -> u64 {
    db.wal_sync_count().expect("opened with statistics")
}

#[test]
fn consensus_db_path_is_inside_the_data_dir() {
    let data = Path::new("/srv/node/data");
    assert_eq!(consensus_db_path(data), data.join("consensus-db"));
    assert_eq!(CONSENSUS_DB_DIR, "consensus-db");
}

/// The consensus DB holds exactly the HotStuff CF and the two DA CFs, and
/// it lives inside the state DB's directory: RocksDB must tolerate the
/// subdirectory through flush, compaction and reopen of both DBs.
#[test]
fn consensus_db_has_only_its_cfs_and_survives_reopen_inside_the_state_dir() {
    let dir = tempfile::tempdir().unwrap();
    let cpath = consensus_db_path(dir.path());
    {
        let state = StateDb::open(dir.path()).unwrap();
        let cdb = StateDb::open_consensus(&cpath).unwrap();
        for cf in CONSENSUS_DB_CFS {
            assert!(
                cdb.cf_handle(cf).is_ok(),
                "{cf} missing in the consensus DB"
            );
        }
        assert!(
            cdb.cf_handle(CF_ACCOUNTS).is_err(),
            "the consensus DB must not carry state CFs"
        );
        assert_eq!(
            CONSENSUS_DB_CFS,
            [
                CF_CONSENSUS_META,
                CF_NATIVE_PENDING,
                CF_NATIVE_SHARDS,
                CF_COMMIT_MANIFEST
            ]
        );
        state.put_cf_raw(CF_ACCOUNTS, b"acct", b"s").unwrap();
        state
            .put_cf_raw(CF_CONSENSUS_META, b"native_applied_height", b"7")
            .unwrap();
        cdb.put_cf_raw(CF_CONSENSUS_META, &[5], b"vset").unwrap();
        cdb.put_cf_raw(CF_NATIVE_PENDING, &[1u8; 32], b"body")
            .unwrap();
        for (db, cfs) in [(&state, &ALL_CF_NAMES[..]), (&cdb, &CONSENSUS_DB_CFS[..])] {
            for cf in cfs {
                let h = db.cf_handle(cf).unwrap();
                db.inner().flush_cf(h).unwrap();
                db.inner().compact_range_cf(h, None::<&[u8]>, None::<&[u8]>);
            }
        }
    }
    let state = StateDb::open(dir.path()).unwrap();
    let cdb = StateDb::open_consensus(&cpath).unwrap();
    assert_eq!(
        state.get_cf_raw(CF_ACCOUNTS, b"acct").unwrap(),
        Some(b"s".to_vec())
    );
    assert_eq!(
        state
            .get_cf_raw(CF_CONSENSUS_META, b"native_applied_height")
            .unwrap(),
        Some(b"7".to_vec())
    );
    assert_eq!(state.get_cf_raw(CF_CONSENSUS_META, &[5]).unwrap(), None);
    assert_eq!(
        cdb.get_cf_raw(CF_CONSENSUS_META, &[5]).unwrap(),
        Some(b"vset".to_vec())
    );
    assert_eq!(
        cdb.get_cf_raw(CF_CONSENSUS_META, b"native_applied_height")
            .unwrap(),
        None
    );
    assert_eq!(
        cdb.get_cf_raw(CF_NATIVE_PENDING, &[1u8; 32]).unwrap(),
        Some(b"body".to_vec())
    );
    // Read-only open (torus-unwedge --inspect) sees the same rows.
    drop(cdb);
    let ro = StateDb::open_consensus_read_only(&cpath).unwrap();
    assert_eq!(
        ro.get_cf_raw(CF_CONSENSUS_META, &[5]).unwrap(),
        Some(b"vset".to_vec())
    );
}

/// T5.3: a consensus DB created before the commit-manifest CF moved there
/// still opens read-only (`torus-unwedge --inspect`), without that CF; a
/// read-write open adds it.
#[test]
fn pre_manifest_consensus_db_opens_read_only_and_gains_the_cf_read_write() {
    let dir = tempfile::tempdir().unwrap();
    {
        let old = StateDb::open_cfs(
            dir.path(),
            &DbTuning::default(),
            book_cf_target_file_bytes(),
            &[CF_CONSENSUS_META, CF_NATIVE_PENDING, CF_NATIVE_SHARDS],
            CONSENSUS_DB_WRITE_BUFFER_BYTES,
            CONSENSUS_BLOCK_CACHE_BYTES,
            crate::prune_filter::PruneMode::Range,
        )
        .unwrap();
        old.put_cf_raw(CF_CONSENSUS_META, &[5], b"vset").unwrap();
    }
    let ro = StateDb::open_consensus_read_only(dir.path()).unwrap();
    assert_eq!(
        ro.get_cf_raw(CF_CONSENSUS_META, &[5]).unwrap(),
        Some(b"vset".to_vec())
    );
    assert!(ro.cf_handle(CF_COMMIT_MANIFEST).is_err());
    drop(ro);
    let rw = StateDb::open_consensus(dir.path()).unwrap();
    assert!(rw.cf_handle(CF_COMMIT_MANIFEST).is_ok());
}

/// A `NativeDaStore` over the consensus DB writes its bodies (and shards)
/// there only; the state DB's DA CFs stay empty. Bodies survive a reopen.
#[test]
fn da_store_over_the_consensus_db_writes_only_there_and_survives_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let cpath = consensus_db_path(dir.path());
    let actions = [dummy_action(1), dummy_action(2)];
    let hashes: Vec<_> = actions.iter().map(compute_action_hash).collect();
    {
        let state = StateDb::open(dir.path()).unwrap();
        let cdb = StateDb::open_consensus(&cpath).unwrap();
        let da = NativeDaStore::new(cdb.clone());
        da.put_batch(&actions).unwrap();
        da.put(&dummy_action(3)).unwrap();
        da.put_shards_batch(
            &actions[..1],
            crate::erasure::ErasureParams::for_validator_set(4),
        )
        .unwrap();
        for cf in [CF_NATIVE_PENDING, CF_NATIVE_SHARDS] {
            assert_eq!(
                state.iterate_cf_count(cf),
                0,
                "{cf}: DA write reached the state DB"
            );
            assert!(
                cdb.iterate_cf_count(cf) > 0,
                "{cf}: DA write missing in the consensus DB"
            );
        }
    }
    let cdb = StateDb::open_consensus(&cpath).unwrap();
    let da = NativeDaStore::new(cdb);
    let got = da.get_batch(&hashes).unwrap();
    assert!(
        got.iter().all(Option::is_some),
        "DA bodies lost across reopen"
    );
    assert!(da
        .get(&compute_action_hash(&dummy_action(3)))
        .unwrap()
        .is_some());
}

/// The commit-boundary and shutdown durability point (`sync_wal`) must
/// cover the consensus DB's WAL too: with the split, the HotStuff frontier
/// and the DA bodies are no longer in the state DB's WAL.
#[test]
fn sync_wal_also_syncs_the_attached_wal_peer() {
    let dir = tempfile::tempdir().unwrap();
    let state = StateDb::open_with_tuning(dir.path(), &stats_tuning()).unwrap();
    let cdb = StateDb::open_consensus_with_tuning(&consensus_db_path(dir.path()), &stats_tuning())
        .unwrap();
    state.attach_wal_peer(&cdb).unwrap();
    cdb.put_cf_raw(CF_CONSENSUS_META, &[23], b"app-fed")
        .unwrap();
    state
        .put_cf_raw(CF_BLOCK_HEADERS, &1u64.to_be_bytes(), b"hdr")
        .unwrap();
    let (peer_before, own_before) = (wal_syncs(&cdb), wal_syncs(&state));
    // A clone made before or after the attach shares the peer.
    state.clone().sync_wal().unwrap();
    assert!(wal_syncs(&cdb) > peer_before, "consensus DB WAL not synced");
    assert!(wal_syncs(&state) > own_before, "state DB WAL not synced");
}

/// Without a peer, `sync_wal` syncs only its own WAL (today's behaviour);
/// a second attach is refused (one consensus DB per node).
#[test]
fn wal_peer_attaches_once_and_is_optional() {
    let dir = tempfile::tempdir().unwrap();
    let state = StateDb::open_with_tuning(dir.path(), &stats_tuning()).unwrap();
    let cdb = StateDb::open_consensus_with_tuning(&consensus_db_path(dir.path()), &stats_tuning())
        .unwrap();
    let before = wal_syncs(&cdb);
    state.sync_wal().unwrap();
    assert_eq!(
        wal_syncs(&cdb),
        before,
        "no peer attached, yet it was synced"
    );
    state.attach_wal_peer(&cdb).unwrap();
    assert!(
        state.attach_wal_peer(&cdb).is_err(),
        "second attach must fail"
    );
}

impl StateDb {
    /// Rows in `cf` (test helper).
    fn iterate_cf_count(&self, cf: &str) -> usize {
        let h = self.cf_handle(cf).unwrap();
        self.inner()
            .iterator_cf(h, rocksdb::IteratorMode::Start)
            .map(|r| r.unwrap())
            .count()
    }
}
