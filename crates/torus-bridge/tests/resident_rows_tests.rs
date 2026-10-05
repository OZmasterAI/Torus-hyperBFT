//! Item 6 Phase 1 (C1): the resident-rows lifecycle (`begin_resident` /
//! `end_resident`) and its staleness guard (P7): R is reused only by the
//! direct successor of the block it reflects, with the applied-height marker
//! agreeing; anything else (marker mismatch, skipped height, a fatal block, a
//! failed flush, a clone of R still alive) rebuilds R from the DB + parent
//! layer, so R is never stale. Not gated by `TORUS_RESIDENT_BOOKS`.

use std::sync::Arc;

use torus_bridge::native_executor::{begin_resident, end_resident, ResidentBooks};
use torus_state::cf::{
    CF_CONSENSUS_META, CF_NATIVE_BALANCES, CF_NATIVE_POSITIONS, META_NATIVE_APPLIED_HEIGHT,
};
use torus_state::resident_rows::RESIDENT_CFS;
use torus_state::{NativeStateOverlay, ResidentRows, StateBackend, StateDb};
use torus_telemetry::Metrics;

fn open_db() -> (tempfile::TempDir, StateDb) {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = StateDb::open(dir.path()).expect("open db");
    (dir, db)
}

fn pos_key(t: u8, m: u64) -> Vec<u8> {
    [&[t; 20][..], &m.to_be_bytes()].concat()
}

fn seed(db: &StateDb) {
    for t in 1..=5u8 {
        db.put_cf_raw(CF_NATIVE_BALANCES, &[t; 20], &[b'b', t]).unwrap();
        db.put_cf_raw(CF_NATIVE_POSITIONS, &pos_key(t, 1), &[b'p', t]).unwrap();
    }
    db.put_cf_raw(CF_NATIVE_POSITIONS, &[b"cvlm".as_slice(), &[1u8; 20]].concat(), b"vol").unwrap();
}

/// R as a dump of both CFs.
fn dump_rows(rows: &ResidentRows) -> Vec<Vec<(Vec<u8>, Vec<u8>)>> {
    RESIDENT_CFS
        .iter()
        .map(|cf| rows.rows(cf).unwrap().iter().map(|(k, v)| (k.clone(), v.clone())).collect())
        .collect()
}

fn dump_db(db: &StateDb) -> Vec<Vec<(Vec<u8>, Vec<u8>)>> {
    RESIDENT_CFS.iter().map(|cf| StateBackend::iterate_cf(db, cf, None).unwrap()).collect()
}

/// One serial block at `h`: begin, write `writes` (balance key byte, value),
/// flush with the marker, end. Returns whether R was rebuilt.
fn serial_block(db: &StateDb, holder: &mut ResidentBooks, h: u64, writes: &[(u8, &[u8])]) -> bool {
    let mut overlay = NativeStateOverlay::new(db.clone());
    let block = begin_resident(Some(holder), &mut overlay, h, None);
    assert!(block.attached() && overlay.has_resident());
    for (t, v) in writes {
        overlay.put_cf_raw(CF_NATIVE_BALANCES, &[*t; 20], v).unwrap();
    }
    overlay.delete_cf_raw(CF_NATIVE_POSITIONS, &pos_key(h as u8, 1)).unwrap();
    let delta = overlay.own_pending_delta();
    overlay.flush_with_native_trie_and_marker(db, h).unwrap();
    let rebuilt = block.rebuilt();
    end_resident(holder, block, &mut overlay, delta, true, None);
    rebuilt
}

fn assert_r_equals_db(holder: &ResidentBooks, db: &StateDb, what: &str) {
    let rows = holder.rows().unwrap_or_else(|| panic!("{what}: holder drained"));
    assert_eq!(dump_rows(rows), dump_db(db), "{what}: R != DB scan");
    // Item 6 C7: undecodable values and `cvlm` keys included.
    assert_eq!(holder.trader_positions_match_rows(), Some(true), "{what}: decoded positions != R");
}

#[test]
fn successor_with_matching_marker_reuses_r() {
    let (_d, db) = open_db();
    seed(&db);
    let mut holder = ResidentBooks::default();
    assert!(serial_block(&db, &mut holder, 1, &[(1, b"x1")]), "cold start builds");
    assert_eq!(holder.rows_height(), Some(1));
    assert_r_equals_db(&holder, &db, "after 1");
    for h in 2..=5 {
        assert!(!serial_block(&db, &mut holder, h, &[(h as u8, b"xh"), (9, b"new")]), "h{h}: reuse");
        assert_r_equals_db(&holder, &db, &format!("after {h}"));
    }
    assert_eq!(holder.rows_builds(), 1);
    assert_eq!(holder.rows_shared_fallbacks(), 0);
}

/// P7: the DB's applied marker disagrees with the slot (failed flush /
/// out-of-band progress) -> rebuild; the rebuilt R holds the out-of-band row.
#[test]
fn marker_mismatch_rebuilds_and_never_serves_stale_rows() {
    let (_d, db) = open_db();
    seed(&db);
    let mut holder = ResidentBooks::default();
    serial_block(&db, &mut holder, 1, &[]);
    serial_block(&db, &mut holder, 2, &[]);
    db.put_cf_raw(CF_NATIVE_BALANCES, &[0x77; 20], b"out-of-band").unwrap();
    db.put_cf_raw(CF_CONSENSUS_META, META_NATIVE_APPLIED_HEIGHT, &7u64.to_be_bytes()).unwrap();
    let mut overlay = NativeStateOverlay::new(db.clone());
    let block = begin_resident(Some(&mut holder), &mut overlay, 3, None);
    assert!(block.rebuilt(), "marker 7 != slot height 2");
    assert_eq!(
        overlay.get_cf_raw(CF_NATIVE_BALANCES, &[0x77; 20]).unwrap(),
        Some(b"out-of-band".to_vec()),
        "rebuilt R sees the DB"
    );
    assert_eq!(holder.rows_builds(), 2);
}

/// P7: a skipped height (the slot is not the block's predecessor) rebuilds.
#[test]
fn skipped_height_rebuilds() {
    let (_d, db) = open_db();
    seed(&db);
    let mut holder = ResidentBooks::default();
    serial_block(&db, &mut holder, 1, &[]);
    // Block 2 applied without the holder (marker 2 in the DB, a row changed).
    db.put_cf_raw(CF_NATIVE_BALANCES, &[2u8; 20], b"applied-elsewhere").unwrap();
    db.put_cf_raw(CF_CONSENSUS_META, META_NATIVE_APPLIED_HEIGHT, &2u64.to_be_bytes()).unwrap();
    assert!(serial_block(&db, &mut holder, 3, &[]), "slot 1 vs block 3: rebuild");
    assert_r_equals_db(&holder, &db, "after 3");
    // No marker row at all: the height sequence alone guards (like the books).
    let (_d2, db2) = open_db();
    seed(&db2);
    let mut holder2 = ResidentBooks::default();
    let mut overlay = NativeStateOverlay::new(db2.clone());
    let block = begin_resident(Some(&mut holder2), &mut overlay, 1, None);
    let delta = overlay.own_pending_delta();
    overlay.flush(&db2).unwrap();
    end_resident(&mut holder2, block, &mut overlay, delta, true, None);
    let mut overlay = NativeStateOverlay::new(db2.clone());
    assert!(!begin_resident(Some(&mut holder2), &mut overlay, 2, None).rebuilt(), "no marker: successor reuses");
    let mut overlay = NativeStateOverlay::new(db2.clone());
    assert!(begin_resident(Some(&mut holder2), &mut overlay, 5, None).rebuilt(), "drained by the take above");
}

/// P7: a fatal block returns before `end_resident` — the slot was taken, so
/// the next block rebuilds. A failed flush (`ok = false`) drains it too.
#[test]
fn fatal_block_and_failed_flush_drain_the_slot() {
    let (_d, db) = open_db();
    seed(&db);
    let mut holder = ResidentBooks::default();
    serial_block(&db, &mut holder, 1, &[]);
    {
        let mut overlay = NativeStateOverlay::new(db.clone());
        let block = begin_resident(Some(&mut holder), &mut overlay, 2, None);
        assert!(!block.rebuilt());
        overlay.put_cf_raw(CF_NATIVE_BALANCES, &[1u8; 20], b"never-flushed").unwrap();
        // fatal: no flush, no end_resident
    }
    assert_eq!(holder.rows_height(), None, "taken by the fatal block");
    assert!(serial_block(&db, &mut holder, 2, &[]), "re-executed block 2 rebuilds");
    assert_r_equals_db(&holder, &db, "after the replayed 2");

    let mut overlay = NativeStateOverlay::new(db.clone());
    let block = begin_resident(Some(&mut holder), &mut overlay, 3, None);
    overlay.put_cf_raw(CF_NATIVE_BALANCES, &[1u8; 20], b"flush-failed").unwrap();
    let delta = overlay.own_pending_delta();
    end_resident(&mut holder, block, &mut overlay, delta, false, None);
    assert_eq!(holder.rows_height(), None, "ok = false drains");
    assert!(serial_block(&db, &mut holder, 3, &[]), "rebuild after a failed flush");
    assert_r_equals_db(&holder, &db, "after 3");
}

/// `Arc::get_mut` fallback: a clone of R still alive at `end_resident` (an
/// overlay clone kept by mistake) leaves the slot empty and is counted; the
/// next block rebuilds.
#[test]
fn live_clone_of_r_falls_back_to_a_rebuild() {
    let (_d, db) = open_db();
    seed(&db);
    let mut holder = ResidentBooks::default();
    serial_block(&db, &mut holder, 1, &[]);
    let mut overlay = NativeStateOverlay::new(db.clone());
    let block = begin_resident(Some(&mut holder), &mut overlay, 2, None);
    let leaked = overlay.clone();
    let delta = overlay.own_pending_delta();
    overlay.flush_with_native_trie_and_marker(&db, 2).unwrap();
    end_resident(&mut holder, block, &mut overlay, delta, true, None);
    assert_eq!(holder.rows_shared_fallbacks(), 1);
    assert_eq!(holder.rows_height(), None);
    drop(leaked);
    assert!(serial_block(&db, &mut holder, 3, &[]));
    assert_r_equals_db(&holder, &db, "after 3");
}

/// `begin_resident(None, ..)` = today's path: nothing attached, reads go to
/// the DB, `end_resident` leaves the holder alone.
#[test]
fn no_holder_is_todays_path() {
    let (_d, db) = open_db();
    seed(&db);
    let mut holder = ResidentBooks::default();
    let mut overlay = NativeStateOverlay::new(db.clone());
    let block = begin_resident(None, &mut overlay, 1, None);
    assert!(!block.attached() && !overlay.has_resident());
    let delta = overlay.own_pending_delta();
    end_resident(&mut holder, block, &mut overlay, delta, true, None);
    assert_eq!(holder.rows_height(), None);
    assert_eq!(holder.rows_builds(), 0);
}

/// Pipelined shape: block h+1's overlay layers over block h's frozen set
/// while the DB lags one block; R (built from DB + parent, then advanced by
/// each block's own delta) equals the fully flushed DB after every flush.
#[test]
fn pipelined_r_tracks_db_plus_parent() {
    let (_d, db) = open_db();
    seed(&db);
    let mut holder = ResidentBooks::default();
    let mut parent: Option<Arc<torus_state::FrozenPending>> = None;
    for h in 1..=6u64 {
        let mut overlay = NativeStateOverlay::with_parent(db.clone(), parent.clone());
        let block = begin_resident(Some(&mut holder), &mut overlay, h, None);
        assert_eq!(block.rebuilt(), h == 1, "h{h}");
        overlay.put_cf_raw(CF_NATIVE_BALANCES, &[h as u8; 20], &h.to_be_bytes()).unwrap();
        overlay.put_cf_raw(CF_NATIVE_POSITIONS, &pos_key(h as u8 + 10, 2), b"open").unwrap();
        overlay.delete_cf_raw(CF_NATIVE_POSITIONS, &pos_key(h as u8, 1)).unwrap();
        overlay.put_cf_raw(CF_CONSENSUS_META, META_NATIVE_APPLIED_HEIGHT, &h.to_be_bytes()).unwrap();
        let delta = overlay.own_pending_delta();
        let frozen = overlay.freeze(h);
        end_resident(&mut holder, block, &mut overlay, delta, true, None);
        if let Some(p) = parent.take() {
            p.flush_with_native_trie_stats(&db, Some(p.height()), None, None).unwrap();
        }
        parent = Some(frozen);
    }
    parent.unwrap().flush_with_native_trie_stats(&db, Some(6), None, None).unwrap();
    assert_r_equals_db(&holder, &db, "after 6");
    assert_eq!(holder.rows_builds(), 1);
}

/// Non-native blocks: `advance_untouched` advances the rows slot of a direct
/// successor (also with the books' kill switch off) and drains it otherwise;
/// `invalidate` drops it.
#[test]
fn advance_untouched_and_invalidate() {
    let (_d, db) = open_db();
    seed(&db);
    let mut holder = ResidentBooks::default();
    serial_block(&db, &mut holder, 1, &[]);
    holder.advance_untouched_with(true, 2);
    assert_eq!(holder.rows_height(), Some(2));
    holder.advance_untouched_with(false, 3);
    assert_eq!(holder.rows_height(), Some(3), "rows advance regardless of the books kill switch");
    holder.advance_untouched_with(true, 5);
    assert_eq!(holder.rows_height(), None, "skipped height drains");
    serial_block(&db, &mut holder, 6, &[]);
    assert!(holder.rows().is_some());
    holder.invalidate();
    assert_eq!(holder.rows_height(), None);
}

#[test]
fn metrics_count_rebuilds_and_size() {
    let (_d, db) = open_db();
    seed(&db);
    let metrics = Metrics::new();
    let mut holder = ResidentBooks::default();
    for h in 1..=3u64 {
        let mut overlay = NativeStateOverlay::new(db.clone());
        let block = begin_resident(Some(&mut holder), &mut overlay, h, Some(&metrics));
        overlay.put_cf_raw(CF_NATIVE_BALANCES, &[0x40 + h as u8; 20], b"grow").unwrap();
        let delta = overlay.own_pending_delta();
        overlay.flush_with_native_trie_and_marker(&db, h).unwrap();
        end_resident(&mut holder, block, &mut overlay, delta, true, Some(&metrics));
    }
    let rows = holder.rows().unwrap();
    assert_eq!(metrics.exec_resident_rows_rebuilds.get(), 1);
    assert_eq!(metrics.exec_resident_rows.get(), rows.len() as i64);
    assert_eq!(metrics.exec_resident_rows_bytes.get(), rows.bytes() as i64);
    let text = metrics.encode();
    assert!(text.contains("torus_exec_resident_rows_build_seconds_count 1"), "{text}");
}

/// C6a (B0): R's CFs are read without the parent layer, so R is reused only
/// when the overlay's parent is the frozen set of the block R reflects. A
/// pipelined chain (parent = the previous block's frozen set, flushed one
/// block later) reuses R and reads exactly DB + parent; a parent of another
/// height (never on the node) rebuilds R from DB + that parent.
#[test]
fn pipelined_reuse_requires_the_parent_of_the_slot_height() {
    let (_d, db) = open_db();
    seed(&db);
    let mut holder = ResidentBooks::default();
    let mut parent: Option<Arc<torus_state::FrozenPending>> = None;
    for h in 1..=4u64 {
        let mut overlay = NativeStateOverlay::with_parent(db.clone(), parent.clone());
        let block = begin_resident(Some(&mut holder), &mut overlay, h, None);
        assert_eq!(block.rebuilt(), h == 1, "h{h}");
        // Through R == through DB + parent (an overlay without R).
        let plain = NativeStateOverlay::with_parent(db.clone(), parent.clone());
        for cf in RESIDENT_CFS {
            assert_eq!(overlay.iterate_cf(cf, None).unwrap(), plain.iterate_cf(cf, None).unwrap(), "h{h} {cf}");
        }
        overlay.put_cf_raw(CF_NATIVE_BALANCES, &[h as u8; 20], b"pipelined").unwrap();
        overlay.delete_cf_raw(CF_NATIVE_POSITIONS, &pos_key(h as u8 + 1, 1)).unwrap();
        let delta = overlay.own_pending_delta();
        overlay.put_cf_raw(CF_CONSENSUS_META, META_NATIVE_APPLIED_HEIGHT, &h.to_be_bytes()).unwrap();
        let frozen = overlay.freeze(h);
        end_resident(&mut holder, block, &mut overlay, delta, true, None);
        if let Some(p) = parent.take() {
            p.flush_with_native_trie_stats(&db, None, None, None).unwrap();
        }
        parent = Some(frozen);
    }
    assert_eq!(holder.rows_builds(), 1, "reused along the chain");
    // Block 4 durable (marker 4 in the DB: height and marker agree), then
    // block 5 over a parent of height 3 (slot at 4): rebuilt, and the
    // rebuilt R holds that parent's rows.
    parent.take().unwrap().flush_with_native_trie_stats(&db, None, None, None).unwrap();
    let odd = NativeStateOverlay::new(db.clone());
    odd.put_cf_raw(CF_NATIVE_BALANCES, &[0x55; 20], b"odd-parent").unwrap();
    let mut overlay = NativeStateOverlay::with_parent(db.clone(), Some(odd.freeze(3)));
    let block = begin_resident(Some(&mut holder), &mut overlay, 5, None);
    assert!(block.rebuilt(), "parent 3 != slot 4");
    assert_eq!(overlay.get_cf_raw(CF_NATIVE_BALANCES, &[0x55; 20]).unwrap(), Some(b"odd-parent".to_vec()));
    assert_eq!(holder.rows_builds(), 2);
}
