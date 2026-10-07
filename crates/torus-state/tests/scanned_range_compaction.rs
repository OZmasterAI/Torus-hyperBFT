//! s99 (c), the s89 fix B pattern for the order ranges the reader precompiles
//! scan: a block flush whose batch deletes rows of `CF_NATIVE_ORDER_BOOKS`
//! (mode 1 order rows, mode 2 level / stop rows) or `CF_NATIVE_ORDERS`
//! compacts the span of those deletes on a background thread, so the
//! tombstones stop costing every later getOrderBook / getOpenOrders scan of
//! that range. Node-local: the reads return the same rows before and after,
//! and nothing is charged for the tombstones (gas never depends on them).

use rocksdb::perf::{set_perf_stats, PerfContext, PerfMetric, PerfStatsLevel};
use torus_state::cf::{CF_NATIVE_ORDERS, CF_NATIVE_ORDER_BOOKS, CF_NATIVE_POSITIONS};
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

/// A market's order row: market(8) ‖ 0x01 ‖ order_id(16).
fn book_key(market: u64, id: u128) -> Vec<u8> {
    [&market.to_be_bytes()[..], &[1u8], &id.to_be_bytes()].concat()
}

/// A trader's cf_native_orders row: trader(20) ‖ market(8) ‖ order_id(16).
fn orders_key(id: u128) -> Vec<u8> {
    [&[7u8; 20][..], &1u64.to_be_bytes(), &id.to_be_bytes()].concat()
}

/// Rows written by one block flush and pushed to an SST, then deleted by the
/// next block's flush (serial path or the pipelined frozen set); one row of
/// each range stays live.
fn churn(db: &StateDb, cf: &'static str, key: impl Fn(u128) -> Vec<u8>, frozen: bool) {
    let ov = NativeStateOverlay::new(db.clone());
    for id in 0..=500u128 {
        ov.put_cf_raw(cf, &key(id), b"row").unwrap();
    }
    ov.flush_with_native_trie_stats(db, None, None, None)
        .unwrap();
    db.inner().flush_cf(db.cf_handle(cf).unwrap()).unwrap();
    db.wait_background_compaction();

    let ov = NativeStateOverlay::new(db.clone());
    for id in 1..=500u128 {
        ov.delete_cf_raw(cf, &key(id)).unwrap();
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

#[test]
fn a_delete_flush_compacts_the_scanned_order_ranges() {
    for frozen in [false, true] {
        for (cf, prefix, key) in [
            (
                CF_NATIVE_ORDER_BOOKS,
                9u64.to_be_bytes().to_vec(),
                Box::new(|id| book_key(9, id)) as Box<dyn Fn(u128) -> Vec<u8>>,
            ),
            (
                CF_NATIVE_ORDERS,
                orders_key(0)[..28].to_vec(),
                Box::new(orders_key),
            ),
        ] {
            let (db, _dir) = temp_db();
            churn(&db, cf, &key, frozen);
            let (done, failed) = db.wait_background_compaction();
            assert_eq!(failed, 0, "{cf} frozen={frozen}");
            assert!(
                done >= 1,
                "{cf} frozen={frozen}: the delete flush scheduled no compaction"
            );
            let (rows, skipped) = deletes_skipped(|| {
                StateBackend::iterate_cf_prefix_from(&db, cf, &prefix, &prefix, 64).unwrap()
            });
            assert_eq!(
                skipped, 0,
                "{cf} frozen={frozen}: the scan still walks {skipped} tombstones"
            );
            assert_eq!(
                rows,
                vec![(key(0), b"row".to_vec())],
                "{cf}: same rows as before the compaction"
            );
            // The overlay view (what the EVM reader scans) is clean too.
            let ov = NativeStateOverlay::new(db.clone());
            let (_, skipped) =
                deletes_skipped(|| ov.iterate_cf_prefix_from(cf, &prefix, &prefix, 64).unwrap());
            assert_eq!(skipped, 0, "{cf} frozen={frozen}: overlay scan");
        }
    }
}

/// Deletes in a CF no reader scans by order range (positions) schedule
/// nothing; neither does a flush that only writes order rows.
#[test]
fn other_deletes_and_plain_writes_schedule_no_compaction() {
    let (db, _dir) = temp_db();
    let ov = NativeStateOverlay::new(db.clone());
    ov.put_cf_raw(CF_NATIVE_ORDER_BOOKS, &book_key(1, 1), b"row")
        .unwrap();
    ov.put_cf_raw(CF_NATIVE_ORDERS, &orders_key(1), b"row")
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
