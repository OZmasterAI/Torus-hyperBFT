//! P3 PROTOTYPE tests: the coalesced layer is read-your-writes, last-value,
//! checkpoints at the interval, flushes on shutdown, and a crash leaves the DB
//! exactly at the last checkpoint (marker, running hash and rows agree).

use super::*;
use crate::backend::{NativeStateOverlay, StateBackend};
use crate::running_hash::{configure_activation, read_running_hash};

fn temp_db() -> (StateDb, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let db = StateDb::open(dir.path()).unwrap();
    (db, dir)
}

fn reopen(dir: &tempfile::TempDir) -> StateDb {
    StateDb::open(dir.path()).unwrap()
}

fn pos_key(i: u64) -> Vec<u8> {
    let mut k = b"pos:".to_vec();
    k.extend_from_slice(&i.to_be_bytes());
    k
}

/// One block's flush through the real per-block path: a position row that
/// changes every block (`hot`), a new position row per block, a book blob
/// rewritten every block, a balance put at 3 and deleted at 5.
fn flush_block(db: &StateDb, h: u64) {
    let ov = NativeStateOverlay::new(db.clone());
    ov.put_cf_raw(CF_NATIVE_POSITIONS, b"hot", &h.to_be_bytes()).unwrap();
    ov.put_cf_raw(CF_NATIVE_POSITIONS, &pos_key(h), &[h as u8; 3]).unwrap();
    ov.put_cf_raw(CF_NATIVE_ORDER_BOOKS, b"book:1", &[h as u8; 64]).unwrap();
    if h == 3 {
        ov.put_cf_raw(CF_NATIVE_BALANCES, b"bal:x", b"v").unwrap();
    }
    if h == 5 {
        ov.delete_cf_raw(CF_NATIVE_BALANCES, b"bal:x").unwrap();
    }
    ov.flush_with_native_trie_stats(db, Some(h), None, None).unwrap();
}

fn durable_u64(db: &StateDb, cf: &str, key: &[u8]) -> Option<u64> {
    db.get_cf_raw_durable(cf, key)
        .unwrap()
        .map(|v| u64::from_be_bytes(v.as_slice().try_into().unwrap()))
}

fn applied_durable(db: &StateDb) -> Option<u64> {
    durable_u64(db, CF_CONSENSUS_META, META_NATIVE_APPLIED_HEIGHT)
}

#[test]
fn p3proto_parse_mode_values() {
    assert_eq!(parse_mode(None), None);
    assert_eq!(parse_mode(Some("")), None);
    assert_eq!(parse_mode(Some("1")), None);
    assert_eq!(parse_mode(Some("all")), Some(Mode::All));
    assert_eq!(parse_mode(Some(" books ")), Some(Mode::Books));
    assert!(covers(Mode::All, CF_NATIVE_POSITIONS));
    assert!(covers(Mode::All, CF_CONSENSUS_META));
    assert!(!covers(Mode::All, CF_BLOCK_BODIES));
    assert!(!covers(Mode::All, CF_NATIVE_TRADES));
    assert!(covers(Mode::Books, CF_NATIVE_ORDER_BOOKS));
    assert!(!covers(Mode::Books, CF_NATIVE_POSITIONS));
    assert!(!covers(Mode::Books, CF_CONSENSUS_META));
}

#[test]
fn p3proto_reads_see_buffered_writes_on_every_read_path() {
    let (db, _d) = temp_db();
    // A durable row the layer will override and one it will tombstone.
    db.put_cf_raw(CF_NATIVE_POSITIONS, b"pos:a", b"db").unwrap();
    db.put_cf_raw(CF_NATIVE_POSITIONS, b"pos:b", b"db").unwrap();
    db.enable_p3_proto(Mode::All, 100).unwrap();
    let ov = NativeStateOverlay::new(db.clone());
    ov.put_cf_raw(CF_NATIVE_POSITIONS, b"pos:a", b"layer").unwrap();
    ov.delete_cf_raw(CF_NATIVE_POSITIONS, b"pos:b").unwrap();
    ov.put_cf_raw(CF_NATIVE_POSITIONS, b"pos:c", b"new").unwrap();
    ov.flush_with_native_trie_stats(&db, Some(1), None, None).unwrap();

    // Nothing reached RocksDB.
    assert_eq!(db.get_cf_raw_durable(CF_NATIVE_POSITIONS, b"pos:a").unwrap(), Some(b"db".to_vec()));
    assert_eq!(db.get_cf_raw_durable(CF_NATIVE_POSITIONS, b"pos:c").unwrap(), None);
    assert_eq!(applied_durable(&db), None);

    // Point reads.
    assert_eq!(db.get_cf_raw(CF_NATIVE_POSITIONS, b"pos:a").unwrap(), Some(b"layer".to_vec()));
    assert_eq!(db.get_cf_raw(CF_NATIVE_POSITIONS, b"pos:b").unwrap(), None);
    assert!(!db.exists_cf_raw(CF_NATIVE_POSITIONS, b"pos:b").unwrap());
    assert_eq!(StateBackend::get_cf_len(&db, CF_NATIVE_POSITIONS, b"pos:c").unwrap(), Some(3));
    assert_eq!(
        db.multi_get_cf_raw(CF_NATIVE_POSITIONS, &[b"pos:a", b"pos:b"]).unwrap(),
        vec![Some(b"layer".to_vec()), None]
    );
    assert_eq!(crate::running_hash::read_applied_height(&db), Some(1));
    // Scans.
    let want = vec![(b"pos:a".to_vec(), b"layer".to_vec()), (b"pos:c".to_vec(), b"new".to_vec())];
    assert_eq!(StateBackend::iterate_cf(&db, CF_NATIVE_POSITIONS, Some(b"pos:")).unwrap(), want);
    assert_eq!(StateBackend::iterate_cf(&db, CF_NATIVE_POSITIONS, None).unwrap(), want);
    assert_eq!(StateBackend::iterate_cf_from(&db, CF_NATIVE_POSITIONS, b"pos:b", 10).unwrap(), want[1..]);
    assert_eq!(
        StateBackend::iterate_cf_prefix_from(&db, CF_NATIVE_POSITIONS, b"pos:", b"pos:", 1).unwrap(),
        want[..1]
    );
    assert!(StateBackend::prefix_exists(&db, CF_NATIVE_POSITIONS, b"pos:c").unwrap());
    assert!(!StateBackend::prefix_exists(&db, CF_NATIVE_POSITIONS, b"pos:b").unwrap());
    // Through a fresh overlay (E's DB fallback paths).
    let ov2 = NativeStateOverlay::new(db.clone());
    assert_eq!(ov2.get_cf_raw(CF_NATIVE_POSITIONS, b"pos:a").unwrap(), Some(b"layer".to_vec()));
    assert_eq!(ov2.iterate_cf(CF_NATIVE_POSITIONS, Some(b"pos:")).unwrap(), want);
    assert_eq!(ov2.iterate_cf_from(CF_NATIVE_POSITIONS, b"pos:", 10).unwrap(), want);
    assert_eq!(ov2.iterate_cf_prefix_from(CF_NATIVE_POSITIONS, b"pos:", b"pos:b", 10).unwrap(), want[1..]);
    assert!(!ov2.prefix_exists(CF_NATIVE_POSITIONS, b"pos:b").unwrap());
    assert!(ov2.prefix_exists(CF_NATIVE_POSITIONS, b"pos:").unwrap());
    // Direct (non-block) writes to a deferred CF are buffered and readable too.
    db.put_cf_raw(CF_NATIVE_POSITIONS, b"pos:d", b"direct").unwrap();
    assert_eq!(db.get_cf_raw_durable(CF_NATIVE_POSITIONS, b"pos:d").unwrap(), None);
    assert_eq!(db.get_cf_raw(CF_NATIVE_POSITIONS, b"pos:d").unwrap(), Some(b"direct".to_vec()));
    // Non-deferred CFs are written straight through.
    db.put_cf_raw(CF_BLOCK_BODIES, b"b1", b"body").unwrap();
    assert_eq!(db.get_cf_raw_durable(CF_BLOCK_BODIES, b"b1").unwrap(), Some(b"body".to_vec()));
}

#[test]
fn p3proto_last_value_coalescing_writes_each_key_once() {
    let (db, _d) = temp_db();
    db.enable_p3_proto(Mode::All, 100).unwrap();
    for h in 1..=10 {
        flush_block(&db, h);
    }
    let p3 = db.p3().unwrap();
    let pos = cf_index(CF_NATIVE_POSITIONS).unwrap();
    {
        let slot = p3.slots[pos].read().unwrap();
        // "hot" written 10 times, held once with the last value; 10 new keys.
        assert_eq!(slot.active.len(), 11);
        assert_eq!(slot.active.get(b"hot".as_slice()), Some(&Some(10u64.to_be_bytes().to_vec())));
    }
    let books = cf_index(CF_NATIVE_ORDER_BOOKS).unwrap();
    assert_eq!(p3.slots[books].read().unwrap().active.len(), 1);
    // The balance put at 3 and deleted at 5 is a single tombstone.
    let bal = cf_index(CF_NATIVE_BALANCES).unwrap();
    assert_eq!(p3.slots[bal].read().unwrap().active.get(b"bal:x".as_slice()), Some(&None));
    db.p3_shutdown_flush().unwrap();
    // The checkpoint wrote each distinct key once (11 positions + 1 book +
    // 1 balance + a few meta rows), not 10 blocks x 3+ rows.
    let rows = p3.stats.checkpoint_rows.load(Ordering::Relaxed);
    assert!(rows < 20, "checkpoint rows {rows}");
    assert_eq!(durable_u64(&db, CF_NATIVE_POSITIONS, b"hot"), Some(10));
    assert_eq!(db.get_cf_raw_durable(CF_NATIVE_ORDER_BOOKS, b"book:1").unwrap(), Some(vec![10u8; 64]));
    assert_eq!(db.get_cf_raw_durable(CF_NATIVE_BALANCES, b"bal:x").unwrap(), None);
}

#[test]
fn p3proto_checkpoint_every_interval_blocks() {
    let (db, _d) = temp_db();
    db.enable_p3_proto(Mode::All, 100).unwrap();
    for h in 1..=99 {
        flush_block(&db, h);
    }
    let p3 = db.p3().unwrap().clone();
    p3.wait_checkpoint().unwrap();
    assert_eq!(applied_durable(&db), None, "nothing durable before height 100");
    assert_eq!(p3.stats.checkpoints.load(Ordering::Relaxed), 0);
    flush_block(&db, 100);
    p3.wait_checkpoint().unwrap();
    assert_eq!(applied_durable(&db), Some(100));
    assert_eq!(durable_u64(&db, CF_NATIVE_POSITIONS, b"hot"), Some(100));
    assert_eq!(p3.stats.checkpoints.load(Ordering::Relaxed), 1);
    assert_eq!(p3.layered_rows(), 0, "flushing layer dropped once durable");
    flush_block(&db, 101);
    assert_eq!(applied_durable(&db), Some(100));
    assert_eq!(crate::running_hash::read_applied_height(&db), Some(101));
}

#[test]
fn p3proto_shutdown_flush_writes_everything_and_closes_the_layer() {
    let (db, _d) = temp_db();
    db.enable_p3_proto(Mode::All, 100).unwrap();
    for h in 1..=37 {
        flush_block(&db, h);
    }
    db.p3_shutdown_flush().unwrap();
    assert_eq!(applied_durable(&db), Some(37));
    assert_eq!(db.p3().unwrap().layered_rows(), 0);
    // After close: per-block writes are durable immediately (today's path).
    flush_block(&db, 38);
    assert_eq!(applied_durable(&db), Some(38));
    assert_eq!(durable_u64(&db, CF_NATIVE_POSITIONS, b"hot"), Some(38));
    db.p3_shutdown_flush().unwrap(); // idempotent
}

/// The same blocks with the layer off: the reference rows and running hash.
fn reference(upto: u64) -> (Vec<(Vec<u8>, Vec<u8>)>, Option<(u64, [u8; 32])>) {
    let (db, _d) = temp_db();
    configure_activation(&db, Some(1)).unwrap();
    for h in 1..=upto {
        flush_block(&db, h);
    }
    (
        StateBackend::iterate_cf(&db, CF_NATIVE_POSITIONS, None).unwrap(),
        read_running_hash(&db),
    )
}

#[test]
fn p3proto_running_hash_and_state_identical_to_layer_off() {
    let (db, _d) = temp_db();
    configure_activation(&db, Some(1)).unwrap();
    db.enable_p3_proto(Mode::All, 100).unwrap();
    for h in 1..=130 {
        flush_block(&db, h);
    }
    let (rows, hash) = reference(130);
    assert_eq!(hash.map(|(h, _)| h), Some(130));
    assert_eq!(read_running_hash(&db), hash, "per-block digest chain unchanged");
    assert_eq!(StateBackend::iterate_cf(&db, CF_NATIVE_POSITIONS, None).unwrap(), rows);
}

#[test]
fn p3proto_crash_recovery_all_arm_lands_exactly_on_the_last_checkpoint() {
    let dir = tempfile::tempdir().unwrap();
    {
        let db = reopen(&dir);
        configure_activation(&db, Some(1)).unwrap();
        db.enable_p3_proto(Mode::All, 100).unwrap();
        for h in 1..=150 {
            flush_block(&db, h);
        }
        db.p3().unwrap().wait_checkpoint().unwrap();
        // Crash: dropped without the shutdown flush (blocks 101..150 lost).
    }
    let db = reopen(&dir);
    let (rows100, hash100) = reference(100);
    // Marker, running hash and rows all at 100: consistent.
    assert_eq!(crate::running_hash::read_applied_height(&db), Some(100));
    assert_eq!(read_running_hash(&db), hash100);
    assert_eq!(StateBackend::iterate_cf(&db, CF_NATIVE_POSITIONS, None).unwrap(), rows100);
    // Replay 101..150 (as replay_committed would from the bodies) with the
    // layer on again: same final state and hash as an uninterrupted run.
    configure_activation(&db, Some(1)).unwrap();
    db.enable_p3_proto(Mode::All, 100).unwrap();
    for h in 101..=150 {
        flush_block(&db, h);
    }
    let (rows150, hash150) = reference(150);
    assert_eq!(read_running_hash(&db), hash150);
    assert_eq!(StateBackend::iterate_cf(&db, CF_NATIVE_POSITIONS, None).unwrap(), rows150);
}

#[test]
fn p3proto_books_arm_defers_only_books_and_fails_closed_after_a_crash() {
    let dir = tempfile::tempdir().unwrap();
    {
        let db = reopen(&dir);
        db.enable_p3_proto(Mode::Books, 100).unwrap();
        for h in 1..=120 {
            flush_block(&db, h);
        }
        db.p3().unwrap().wait_checkpoint().unwrap();
        // Positions and the marker are durable every block; books at 100.
        assert_eq!(applied_durable(&db), Some(120));
        assert_eq!(durable_u64(&db, CF_NATIVE_POSITIONS, b"hot"), Some(120));
        assert_eq!(db.get_cf_raw_durable(CF_NATIVE_ORDER_BOOKS, b"book:1").unwrap(), Some(vec![100u8; 64]));
        assert_eq!(db.get_cf_raw(CF_NATIVE_ORDER_BOOKS, b"book:1").unwrap(), Some(vec![120u8; 64]));
        assert_eq!(durable_u64(&db, CF_CONSENSUS_META, META_P3_BOOKS_CKPT), Some(100));
        // crash
    }
    let db = reopen(&dir);
    let err = db.enable_p3_proto(Mode::Books, 100).unwrap_err();
    assert!(err.to_string().contains("books"), "{err}");
}

#[test]
fn p3proto_books_arm_graceful_restart_is_consistent() {
    let dir = tempfile::tempdir().unwrap();
    {
        let db = reopen(&dir);
        db.enable_p3_proto(Mode::Books, 100).unwrap();
        for h in 1..=120 {
            flush_block(&db, h);
        }
        db.p3_shutdown_flush().unwrap();
        // A block that lands after the shutdown flush stays consistent too.
        flush_block(&db, 121);
    }
    let db = reopen(&dir);
    assert_eq!(applied_durable(&db), Some(121));
    assert_eq!(durable_u64(&db, CF_CONSENSUS_META, META_P3_BOOKS_CKPT), Some(121));
    assert_eq!(db.get_cf_raw_durable(CF_NATIVE_ORDER_BOOKS, b"book:1").unwrap(), Some(vec![121u8; 64]));
    db.enable_p3_proto(Mode::Books, 100).unwrap();
}

#[test]
fn p3proto_books_arm_crash_before_first_checkpoint_is_detected() {
    let dir = tempfile::tempdir().unwrap();
    {
        let db = reopen(&dir);
        db.enable_p3_proto(Mode::Books, 100).unwrap();
        for h in 1..=10 {
            flush_block(&db, h);
        }
    }
    let db = reopen(&dir);
    assert!(db.enable_p3_proto(Mode::Books, 100).is_err());
}

#[test]
fn p3proto_merged_iter_orders_and_hides_tombstones_across_both_layers() {
    let (db, _d) = temp_db();
    for k in [b"a", b"c", b"e", b"g"] {
        db.put_cf_raw(CF_NATIVE_POSITIONS, k, b"db").unwrap();
    }
    db.enable_p3_proto(Mode::All, 100).unwrap();
    let p3 = db.p3().unwrap();
    let idx = cf_index(CF_NATIVE_POSITIONS).unwrap();
    {
        let mut slot = p3.slots[idx].write().unwrap();
        let mut fl = Rows::new();
        fl.insert(b"b".to_vec(), Some(b"fl".to_vec()));
        fl.insert(b"c".to_vec(), None); // flushing tombstone over DB
        fl.insert(b"e".to_vec(), Some(b"fl".to_vec()));
        slot.flushing = Some(Arc::new(fl));
        slot.active.insert(b"e".to_vec(), None); // active tombstone over flushing
        slot.active.insert(b"c".to_vec(), Some(b"act".to_vec())); // active over flushing tombstone
        slot.active.insert(b"f".to_vec(), Some(b"act".to_vec()));
    }
    let got = StateBackend::iterate_cf(&db, CF_NATIVE_POSITIONS, None).unwrap();
    let want: Vec<(Vec<u8>, Vec<u8>)> = [("a", "db"), ("b", "fl"), ("c", "act"), ("f", "act"), ("g", "db")]
        .iter()
        .map(|(k, v)| (k.as_bytes().to_vec(), v.as_bytes().to_vec()))
        .collect();
    assert_eq!(got, want);
    assert_eq!(db.get_cf_raw(CF_NATIVE_POSITIONS, b"e").unwrap(), None);
    assert_eq!(db.get_cf_raw(CF_NATIVE_POSITIONS, b"b").unwrap(), Some(b"fl".to_vec()));
}

/// Codex stop review: `iterate_cf` does not report range deletes, so a batch
/// rebuilt from it dropped them (the startup trade-history wipe kept its rows).
#[test]
fn p3proto_range_deletes_survive_on_uncovered_and_covered_cfs() {
    let (db, _d) = temp_db();
    db.put_cf_raw(CF_NATIVE_TRADES, b"legacy", b"legacy").unwrap();
    db.put_cf_raw(CF_NATIVE_POSITIONS, b"pos:old", b"db").unwrap();
    db.enable_p3_proto(Mode::All, 100).unwrap();
    // Uncovered CF (trades): the range delete lands.
    let mut b = WriteBatch::default();
    b.delete_range_cf(db.cf_handle(CF_NATIVE_TRADES).unwrap(), &[][..], &[0xFFu8; 64][..]);
    db.write_with(b, &rocksdb::WriteOptions::default()).unwrap();
    assert_eq!(db.get_cf_raw(CF_NATIVE_TRADES, b"legacy").unwrap(), None);
    // Covered CF with a buffered row in the range: neither layer nor DB row survives.
    flush_block(&db, 1);
    db.put_cf_raw(CF_NATIVE_POSITIONS, b"pos:new", b"layer").unwrap();
    let mut b = WriteBatch::default();
    b.delete_range_cf(db.cf_handle(CF_NATIVE_POSITIONS).unwrap(), b"pos:".as_slice(), b"pos;".as_slice());
    db.write(b).unwrap();
    assert_eq!(db.get_cf_raw(CF_NATIVE_POSITIONS, b"pos:old").unwrap(), None);
    assert_eq!(db.get_cf_raw(CF_NATIVE_POSITIONS, b"pos:new").unwrap(), None);
    assert!(StateBackend::iterate_cf(&db, CF_NATIVE_POSITIONS, Some(b"pos:")).unwrap().is_empty());
    // The layer was checkpointed first: block 1's marker is durable.
    assert_eq!(applied_durable(&db), Some(1));
}
