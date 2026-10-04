//! s89 fix B: after a flush whose batch deleted oracle submission rows
//! (`prune_submissions` when the feed pauses: every `sub‖market‖validator` row
//! of CF_NATIVE_ORACLE), the node compacts `[sub, suc)` on a background thread,
//! so the tombstones stop costing every later scan of the `sub` prefix — the
//! `oracle_due` -> `has_submissions` -> `prefix_exists("sub")` check on every
//! block walked all of them (~5 ms per empty block at 300 markets x 3
//! reporters). Fix A cannot help there: the tombstones are INSIDE the prefix.
//!
//! Node-local: the reads return the same rows before and after.

use rocksdb::perf::{set_perf_stats, PerfContext, PerfMetric, PerfStatsLevel};
use torus_state::cf::CF_NATIVE_ORACLE;
use torus_state::db::StateDb;
use torus_state::{NativeStateOverlay, StateBackend};

const MARKETS: u64 = 300;
const REPORTERS: u8 = 3;

fn temp_db() -> (StateDb, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("create tempdir");
    let db = StateDb::open(dir.path()).expect("open db");
    (db, dir)
}

fn sub_key(market: u64, reporter: u8) -> Vec<u8> {
    let mut k = b"sub".to_vec();
    k.extend_from_slice(&market.to_be_bytes());
    k.push(reporter);
    k
}

/// Tombstones the current thread's iterators skipped while running `f`.
fn deletes_skipped(f: impl FnOnce()) -> u64 {
    set_perf_stats(PerfStatsLevel::EnableCount);
    let mut ctx = PerfContext::default();
    ctx.reset();
    f();
    let n = ctx.metric(PerfMetric::InternalDeleteSkippedCount);
    set_perf_stats(PerfStatsLevel::Disable);
    n
}

/// Submissions for every market (plus a non-submission row) written through an
/// overlay flush and pushed to an SST file, as on a node that has been fed.
fn seed(db: &StateDb) {
    let ov = NativeStateOverlay::new(db.clone());
    for m in 1..=MARKETS {
        for r in 0..REPORTERS {
            ov.put_cf_raw(CF_NATIVE_ORACLE, &sub_key(m, r), b"price").unwrap();
        }
    }
    ov.put_cf_raw(CF_NATIVE_ORACLE, b"agg-keep", b"aggregate").unwrap();
    ov.flush_with_native_trie_stats(db, None, None, None).unwrap();
    db.inner()
        .flush_cf(db.cf_handle(CF_NATIVE_ORACLE).unwrap())
        .unwrap();
}

/// The prune: delete every submission row in one block and flush it, through
/// the serial path (live overlay) or the pipelined path (frozen set).
fn prune_and_flush(db: &StateDb, frozen: bool) {
    let ov = NativeStateOverlay::new(db.clone());
    for m in 1..=MARKETS {
        for r in 0..REPORTERS {
            ov.delete_cf_raw(CF_NATIVE_ORACLE, &sub_key(m, r)).unwrap();
        }
    }
    if frozen {
        ov.freeze(2)
            .flush_with_native_trie_stats(db, None, None, None)
            .unwrap();
    } else {
        ov.flush_with_native_trie_stats(db, None, None, None).unwrap();
    }
}

/// Tombstones `prefix_exists("sub")` walks.
fn due_check_skips(db: &StateDb) -> u64 {
    deletes_skipped(|| {
        assert!(!StateBackend::prefix_exists(db, CF_NATIVE_ORACLE, b"sub").unwrap());
    })
}

#[test]
fn prune_flush_compacts_the_submission_range() {
    for frozen in [false, true] {
        let (db, _dir) = temp_db();
        seed(&db);
        prune_and_flush(&db, frozen);
        let path = if frozen { "frozen" } else { "serial" };
        assert_eq!(db.wait_background_compaction(), (1, 0), "{path}: one compaction ran");
        let skipped = due_check_skips(&db);
        assert_eq!(
            skipped, 0,
            "{path}: prefix_exists(sub) still walks {skipped} tombstones after the prune flush"
        );
        // Same reads as before the compaction.
        assert!(StateBackend::iterate_cf(&db, CF_NATIVE_ORACLE, Some(b"sub")).unwrap().is_empty());
        assert_eq!(
            StateBackend::get_cf_raw(&db, CF_NATIVE_ORACLE, b"agg-keep").unwrap().as_deref(),
            Some(&b"aggregate"[..])
        );
        // The overlay view (what oracle_due reads) is clean too.
        let ov = NativeStateOverlay::new(db.clone());
        let skipped = deletes_skipped(|| {
            assert!(!ov.prefix_exists(CF_NATIVE_ORACLE, b"sub").unwrap());
        });
        assert_eq!(skipped, 0, "{path}: overlay prefix_exists(sub)");
    }
}
