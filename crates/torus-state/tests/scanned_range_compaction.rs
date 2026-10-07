//! s99 (c), the s89 fix B pattern for the book rows getOrderBook scans: block
//! flushes that delete rows of a market in `CF_NATIVE_ORDER_BOOKS` count them
//! per market, and once a market's uncompacted deletes reach
//! `SCANNED_DELETES_COMPACTION_THRESHOLD` (64) their span is compacted on a
//! background thread, so the tombstones stop costing every later scan of that
//! market. Below the threshold they linger (at most 63 per market).
//! Node-local: the reads return the same rows before and after, and nothing
//! is charged for the tombstones (gas never depends on them).

use rocksdb::perf::{set_perf_stats, PerfContext, PerfMetric, PerfStatsLevel};
use torus_state::cf::{CF_NATIVE_ORDER_BOOKS, CF_NATIVE_POSITIONS};
use torus_state::db::StateDb;
use torus_state::{NativeStateOverlay, StateBackend};

fn temp_db() -> (StateDb, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("create tempdir");
    let db = StateDb::open(dir.path()).expect("open db");
    (db, dir)
}

/// Tombstones the current thread's iterators skipped while running `f`.
fn deletes_skipped<R>(f: impl FnOnce() -> R) -> (R, u64) {
    set_perf_stats(PerfStatsLevel::EnableCount);
    let mut ctx = PerfContext::default();
    ctx.reset();
    let r = f();
    let n = ctx.metric(PerfMetric::InternalDeleteSkippedCount);
    set_perf_stats(PerfStatsLevel::Disable);
    (r, n)
}

/// Market 9's level row at `price`: market(8) ‖ 0x03 ‖ bid ‖ price(16).
fn level_key(price: u128) -> Vec<u8> {
    [&9u64.to_be_bytes()[..], &[3u8, 0], &price.to_be_bytes()].concat()
}

type Rows = Vec<(Vec<u8>, Vec<u8>)>;

const PREFIX: [u8; 10] = [0, 0, 0, 0, 0, 0, 0, 9, 3, 0];

/// One block flush writing `prices` (and the live row at price 0), pushed to
/// an SST; then one deleting `prices` (serial path or pipelined frozen set).
fn churn(db: &StateDb, prices: std::ops::Range<u128>, frozen: bool) {
    let ov = NativeStateOverlay::new(db.clone());
    ov.put_cf_raw(CF_NATIVE_ORDER_BOOKS, &level_key(0), b"row")
        .unwrap();
    for p in prices.clone() {
        ov.put_cf_raw(CF_NATIVE_ORDER_BOOKS, &level_key(p), b"row")
            .unwrap();
    }
    ov.flush_with_native_trie_stats(db, None, None, None)
        .unwrap();
    db.inner()
        .flush_cf(db.cf_handle(CF_NATIVE_ORDER_BOOKS).unwrap())
        .unwrap();
    db.wait_background_compaction();

    let ov = NativeStateOverlay::new(db.clone());
    for p in prices {
        ov.delete_cf_raw(CF_NATIVE_ORDER_BOOKS, &level_key(p))
            .unwrap();
    }
    if frozen {
        ov.freeze(2)
            .flush_with_native_trie_stats(db, None, None, None)
            .unwrap();
    } else {
        ov.flush_with_native_trie_stats(db, None, None, None)
            .unwrap();
    }
}

/// The scan getOrderBook does (bids of market 9): rows and tombstones walked,
/// on the DB and through an overlay.
fn scan(db: &StateDb) -> (Rows, u64, u64) {
    let (rows, on_db) = deletes_skipped(|| {
        StateBackend::iterate_cf_prefix_from(db, CF_NATIVE_ORDER_BOOKS, &PREFIX, &PREFIX, 64)
            .unwrap()
    });
    let ov = NativeStateOverlay::new(db.clone());
    let (_, on_ov) = deletes_skipped(|| {
        ov.iterate_cf_prefix_from(CF_NATIVE_ORDER_BOOKS, &PREFIX, &PREFIX, 64)
            .unwrap()
    });
    (rows, on_db, on_ov)
}

#[test]
fn a_delete_flush_compacts_the_market() {
    for frozen in [false, true] {
        let (db, _dir) = temp_db();
        churn(&db, 1..501, frozen);
        let (done, failed) = db.wait_background_compaction();
        assert_eq!(failed, 0, "frozen={frozen}");
        assert!(
            done >= 1,
            "frozen={frozen}: the delete flush scheduled no compaction"
        );
        let (rows, on_db, on_ov) = scan(&db);
        assert_eq!(
            (on_db, on_ov),
            (0, 0),
            "frozen={frozen}: the scan still walks tombstones"
        );
        assert_eq!(
            rows,
            vec![(level_key(0), b"row".to_vec())],
            "same rows as before the compaction"
        );
    }
}

/// 40 deletes stay (below 64; the scan walks them, gas unaffected); 30 more
/// in a later flush bring the market to 70 and both flushes' tombstones go.
#[test]
fn deletes_below_the_threshold_linger_until_it_is_reached() {
    let (db, _dir) = temp_db();
    churn(&db, 1..41, false);
    assert_eq!(db.wait_background_compaction(), (0, 0));
    let (rows, on_db, _) = scan(&db);
    assert_eq!(on_db, 40, "40 lingering tombstones");
    assert_eq!(rows.len(), 1);
    churn(&db, 100..130, false);
    assert_eq!(db.wait_background_compaction(), (1, 0));
    let (rows, on_db, on_ov) = scan(&db);
    assert_eq!((on_db, on_ov), (0, 0), "both flushes' tombstones compacted");
    assert_eq!(rows, vec![(level_key(0), b"row".to_vec())]);
}

/// Deletes in a CF no reader scans (positions) schedule nothing; neither does
/// a flush that only writes book rows.
#[test]
fn other_deletes_and_plain_writes_schedule_no_compaction() {
    let (db, _dir) = temp_db();
    let ov = NativeStateOverlay::new(db.clone());
    ov.put_cf_raw(CF_NATIVE_ORDER_BOOKS, &level_key(1), b"row")
        .unwrap();
    ov.put_cf_raw(CF_NATIVE_POSITIONS, b"pos", b"row").unwrap();
    ov.flush_with_native_trie_stats(&db, None, None, None)
        .unwrap();
    assert_eq!(db.wait_background_compaction(), (0, 0));
    let ov = NativeStateOverlay::new(db.clone());
    ov.delete_cf_raw(CF_NATIVE_POSITIONS, b"pos").unwrap();
    ov.flush_with_native_trie_stats(&db, None, None, None)
        .unwrap();
    assert_eq!(db.wait_background_compaction(), (0, 0));
}
