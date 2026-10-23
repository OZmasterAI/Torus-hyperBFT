//! Item 6 Phase 1 (C1) tests: resident rows R (`build`, `apply`) and the
//! overlay reading R's two CFs from R in place of the DB.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use super::{ResidentRows, RESIDENT_CFS};
use crate::backend::{NativeStateOverlay, StateBackend};
use crate::cf::{
    CF_NATIVE_BALANCES, CF_NATIVE_LIQUIDATION, CF_NATIVE_MARKETS, CF_NATIVE_ORACLE, CF_NATIVE_ORDERS, CF_NATIVE_POSITIONS,
};
use crate::db::StateDb;

fn temp_db() -> (StateDb, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("create tempdir");
    let db = StateDb::open(dir.path()).expect("open db");
    (db, dir)
}

fn pos_key(trader: u8, market: u64) -> Vec<u8> {
    [&[trader; 20][..], &market.to_be_bytes()].concat()
}

/// `cvlm` (24 bytes) and keys of other odd lengths, in both CFs.
fn odd_keys() -> Vec<Vec<u8>> {
    vec![
        [b"cvlm".as_slice(), &[7u8; 20]].concat(),
        [b"cvlm".as_slice(), &[9u8; 20]].concat(),
        vec![0x01],
        vec![0xff; 33],
        [&[3u8; 20][..], &[0u8; 9]].concat(),
    ]
}

fn rows_of(rows: &ResidentRows, cf: &str) -> Vec<(Vec<u8>, Vec<u8>)> {
    rows.rows(cf).expect("resident CF").iter().map(|(k, v)| (k.clone(), v.clone())).collect()
}

fn bytes_of(rows: &ResidentRows) -> usize {
    RESIDENT_CFS
        .iter()
        .flat_map(|cf| rows.rows(cf).unwrap().iter())
        .map(|(k, v)| k.len() + v.len())
        .sum()
}

/// `build` over DB + parent layer == `iterate_cf(cf, None)` of both CFs through
/// that overlay — `cvlm` and odd-length keys included; other CFs are not in R.
#[test]
fn build_equals_iterate_cf_of_both_cfs() {
    let (db, _dir) = temp_db();
    for t in 1..=6u8 {
        for m in 1..=3u64 {
            db.put_cf_raw(CF_NATIVE_POSITIONS, &pos_key(t, m), &[t, m as u8]).unwrap();
        }
        db.put_cf_raw(CF_NATIVE_BALANCES, &[t; 20], &[b'b', t]).unwrap();
    }
    for k in odd_keys() {
        db.put_cf_raw(CF_NATIVE_POSITIONS, &k, b"odd-p").unwrap();
        db.put_cf_raw(CF_NATIVE_BALANCES, &k, b"odd-b").unwrap();
    }
    db.put_cf_raw(CF_NATIVE_ORDERS, &[1u8; 44], b"order").unwrap();
    // Item 6 E2-E4: the further R CFs, any key shape.
    for cf in &RESIDENT_CFS[2..] {
        for k in odd_keys().iter().chain([pos_key(1, 1), vec![0x02; 21]].iter()) {
            db.put_cf_raw(cf, k, &[b"x".as_slice(), k].concat()).unwrap();
        }
    }
    let parent = NativeStateOverlay::new(db.clone());
    for cf in &RESIDENT_CFS[2..] {
        parent.put_cf_raw(cf, &[0x04; 3], b"parent-new").unwrap();
        parent.delete_cf_raw(cf, &[0x02; 21]).unwrap();
    }
    parent.put_cf_raw(CF_NATIVE_POSITIONS, &pos_key(9, 1), b"parent-new").unwrap();
    parent.put_cf_raw(CF_NATIVE_POSITIONS, &pos_key(1, 1), b"parent-over").unwrap();
    parent.delete_cf_raw(CF_NATIVE_POSITIONS, &pos_key(2, 2)).unwrap();
    parent.delete_cf_raw(CF_NATIVE_BALANCES, &[3u8; 20]).unwrap();
    parent.delete_cf_raw(CF_NATIVE_BALANCES, &odd_keys()[0]).unwrap();
    parent.put_cf_raw(CF_NATIVE_BALANCES, &[0xEE; 20], b"parent-bal").unwrap();
    let overlay = NativeStateOverlay::with_parent(db.clone(), Some(parent.freeze(1)));

    let rows = ResidentRows::build(&overlay).unwrap();
    let mut total = 0;
    for cf in RESIDENT_CFS {
        let want = overlay.iterate_cf(cf, None).unwrap();
        assert!(!want.is_empty(), "{cf}: non-vacuous");
        assert_eq!(rows_of(&rows, cf), want, "{cf}");
        total += want.len();
    }
    assert_eq!(rows.len(), total);
    assert_eq!(rows.bytes(), bytes_of(&rows));
    assert!(rows.rows(CF_NATIVE_ORDERS).is_none(), "only R's CFs are resident");
    assert!(rows.rows(CF_NATIVE_POSITIONS).unwrap().contains_key(&odd_keys()[1]), "cvlm kept");
    assert!(!rows.rows(CF_NATIVE_BALANCES).unwrap().contains_key(&odd_keys()[0]), "parent tombstone");
}

/// `apply(own_pending_delta)` == flushing the block's writes into a DB that
/// held R: puts (new and overwrite), deletes (present and absent), a delete
/// then re-put, a put then delete, cvlm / odd keys; other CFs never enter the
/// delta. Re-applying the same delta changes nothing.
#[test]
fn apply_puts_deletes_and_re_puts() {
    let (db, _dir) = temp_db();
    for t in 1..=4u8 {
        db.put_cf_raw(CF_NATIVE_POSITIONS, &pos_key(t, 1), &[t]).unwrap();
        db.put_cf_raw(CF_NATIVE_BALANCES, &[t; 20], &[t, t]).unwrap();
    }
    for k in odd_keys() {
        db.put_cf_raw(CF_NATIVE_POSITIONS, &k, b"odd").unwrap();
    }
    let rows = Arc::new(ResidentRows::build(&db).unwrap());
    let mut overlay = NativeStateOverlay::new(db.clone());
    overlay.attach_resident(rows.clone());
    let p = CF_NATIVE_POSITIONS;
    let b = CF_NATIVE_BALANCES;
    overlay.put_cf_raw(p, &pos_key(5, 1), b"new").unwrap();
    overlay.put_cf_raw(p, &pos_key(1, 1), b"overwrite").unwrap();
    overlay.delete_cf_raw(p, &pos_key(2, 1)).unwrap();
    overlay.delete_cf_raw(p, &pos_key(8, 8)).unwrap(); // absent
    overlay.delete_cf_raw(p, &pos_key(3, 1)).unwrap();
    overlay.put_cf_raw(p, &pos_key(3, 1), b"re-put").unwrap();
    overlay.put_cf_raw(p, &pos_key(6, 1), b"gone").unwrap();
    overlay.delete_cf_raw(p, &pos_key(6, 1)).unwrap();
    overlay.delete_cf_raw(p, &odd_keys()[0]).unwrap();
    overlay.put_cf_raw(p, &odd_keys()[3], b"odd-new").unwrap();
    overlay.put_cf_raw(b, &odd_keys()[1], b"cvlm-bal").unwrap();
    overlay.put_cf_raw(b, &[4u8; 20], b"bal-over").unwrap();
    overlay.delete_cf_raw(b, &[1u8; 20]).unwrap();
    overlay.put_cf_raw(CF_NATIVE_ORDERS, &[2u8; 44], b"not-resident").unwrap();

    let delta = overlay.own_pending_delta();
    assert_eq!(delta.len(), 11, "R-CF keys only, one entry per key");
    let mut applied = (*rows).clone();
    applied.apply(&delta);
    assert!(overlay.detach_resident().is_some());
    overlay.flush(&db).unwrap();
    for cf in RESIDENT_CFS {
        assert_eq!(rows_of(&applied, cf), db.iterate_cf(cf, None).unwrap(), "{cf}");
    }
    assert_eq!(applied.bytes(), bytes_of(&applied));
    assert_eq!(applied.len(), RESIDENT_CFS.iter().map(|cf| db.iterate_cf(cf, None).unwrap().len()).sum::<usize>());
    let again = {
        let mut r = applied.clone();
        r.apply(&delta);
        r
    };
    assert_eq!(again, applied, "apply is idempotent");
}

/// `layer_touches`: the default (`StateDb`, overlay without R) is "assume
/// dirty"; with R attached only the overlay's OWN pending writes / tombstones
/// count (the parent is already in R).
#[test]
fn layer_touches_own_pending_only_with_resident() {
    let (db, _dir) = temp_db();
    let p = CF_NATIVE_POSITIONS;
    assert!(db.layer_touches(p, &[1u8; 20]), "StateDb: assume dirty");
    let parent = NativeStateOverlay::new(db.clone());
    parent.put_cf_raw(p, &pos_key(1, 1), b"parent").unwrap();
    let mut overlay = NativeStateOverlay::with_parent(db.clone(), Some(parent.freeze(1)));
    assert!(overlay.layer_touches(p, &[9u8; 20]), "no R: assume dirty");
    overlay.attach_resident(Arc::new(ResidentRows::build(&overlay).unwrap()));
    overlay.put_cf_raw(p, &pos_key(2, 1), b"own").unwrap();
    overlay.delete_cf_raw(p, &pos_key(3, 1)).unwrap();
    assert!(!overlay.layer_touches(p, &[1u8; 20]), "parent write: already in R");
    assert!(overlay.layer_touches(p, &[2u8; 20]), "own write");
    assert!(overlay.layer_touches(p, &[3u8; 20]), "own tombstone");
    assert!(!overlay.layer_touches(p, &[4u8; 20]), "untouched");
    assert!(!overlay.layer_touches(CF_NATIVE_BALANCES, &[2u8; 20]), "other CF");
}

/// Overlay with R: over random DB / R / parent / pending states (R already
/// holding the parent, as on the node), every read method on R's two CFs
/// (`get_cf_raw`, `iterate_cf` with `None` and every prefix,
/// `iterate_cf_from` over starts x limits, `prefix_exists`) equals the same
/// read on a DB into which R (for its CFs; the DB rows for any other CF),
/// then the parent, then the pending set were flushed. R is authoritative: a
/// DB row of R's CFs that R lacks is invisible. A non-R CF reads the DB.
#[test]
fn overlay_with_resident_equals_flushed_db_over_random_layers() {
    let mut seed: u64 = 0x5EED_00C1;
    let mut rnd = |n: u64| {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (seed >> 33) % n
    };
    const ALPHABET: [u8; 4] = [0x00, 0x01, 0xfe, 0xff];
    let cfs: Vec<&str> = RESIDENT_CFS.iter().copied().chain([CF_NATIVE_ORDERS]).collect();
    let (mut checked, mut hidden) = (0usize, 0usize);
    for round in 0..40 {
        let (db, _d1) = temp_db();
        let (flushed, _d2) = temp_db();
        let mut keys: BTreeSet<Vec<u8>> = BTreeSet::new();
        for _ in 0..4 + rnd(14) {
            let len = 1 + rnd(4) as usize;
            keys.insert((0..len).map(|_| ALPHABET[rnd(4) as usize]).collect());
        }
        // Per (cf, key): in DB, in R, parent op, pending op (0 none, 1 put, 2 delete).
        let ops: Vec<(&str, Vec<u8>, bool, bool, u64, u64)> = cfs
            .iter()
            .flat_map(|cf| keys.iter().map(move |k| (*cf, k.clone())))
            .map(|(cf, k)| (cf, k, rnd(2) == 1, rnd(2) == 1, rnd(3), rnd(3)))
            .collect();
        let mut r_rows = ResidentRows::default();
        let seed_delta = NativeStateOverlay::new(flushed.clone());
        for (cf, k, in_db, in_r, _, _) in &ops {
            let resident = RESIDENT_CFS.contains(cf);
            if *in_db {
                db.put_cf_raw(cf, k, &[b"db".as_slice(), k].concat()).unwrap();
                if !resident {
                    flushed.put_cf_raw(cf, k, &[b"db".as_slice(), k].concat()).unwrap();
                }
            }
            if resident && *in_r {
                seed_delta.put_cf_raw(cf, k, &[b"r".as_slice(), k].concat()).unwrap();
                flushed.put_cf_raw(cf, k, &[b"r".as_slice(), k].concat()).unwrap();
            }
            hidden += usize::from(resident && *in_db && !*in_r);
        }
        r_rows.apply(&seed_delta.own_pending_delta());
        seed_delta.discard_tx();
        let apply = |ov: &NativeStateOverlay, layer: u8| {
            for (cf, k, _, _, pa, pe) in &ops {
                let v = [&[layer], k.as_slice()].concat();
                match if layer == 0 { *pa } else { *pe } {
                    1 => {
                        ov.put_cf_raw(cf, k, &v).unwrap();
                        flushed.put_cf_raw(cf, k, &v).unwrap();
                    }
                    2 => {
                        ov.delete_cf_raw(cf, k).unwrap();
                        flushed.delete_cf_raw(cf, k).unwrap();
                    }
                    _ => {}
                }
            }
        };
        // C6a (B0): R holds the previous block's post-state, the parent layer
        // included (`end_resident` applied the parent's own delta to R).
        let parent = (round % 4 != 0).then(|| {
            let p = NativeStateOverlay::new(db.clone());
            apply(&p, 0);
            r_rows.apply(&p.own_pending_delta());
            p.freeze(1)
        });
        let mut overlay = NativeStateOverlay::with_parent(db.clone(), parent);
        overlay.attach_resident(Arc::new(r_rows));
        apply(&overlay, 1);

        let mut prefixes: BTreeSet<Vec<u8>> = BTreeSet::new();
        prefixes.insert(Vec::new());
        for k in &keys {
            for l in 1..=k.len() {
                prefixes.insert(k[..l].to_vec());
            }
            prefixes.insert([k.as_slice(), &[0xff]].concat());
            prefixes.insert([k.as_slice(), &[0x00]].concat());
        }
        for cf in cfs.iter().copied() {
            let want_all = StateBackend::iterate_cf(&flushed, cf, None).unwrap();
            assert_eq!(overlay.iterate_cf(cf, None).unwrap(), want_all, "round {round} {cf}: None");
            for k in keys.iter().chain(prefixes.iter()) {
                assert_eq!(
                    overlay.get_cf_raw(cf, k).unwrap(),
                    StateBackend::get_cf_raw(&flushed, cf, k).unwrap(),
                    "round {round} {cf}: get {k:?}"
                );
            }
            for p in &prefixes {
                let want = StateBackend::iterate_cf(&flushed, cf, Some(p)).unwrap();
                assert_eq!(overlay.iterate_cf(cf, Some(p)).unwrap(), want, "round {round} {cf}: prefix {p:?}");
                assert_eq!(
                    overlay.prefix_exists(cf, p).unwrap(),
                    StateBackend::prefix_exists(&flushed, cf, p).unwrap(),
                    "round {round} {cf}: exists {p:?}"
                );
                for limit in [0usize, 1, 2, 5, usize::MAX] {
                    assert_eq!(
                        overlay.iterate_cf_from(cf, p, limit).unwrap(),
                        StateBackend::iterate_cf_from(&flushed, cf, p, limit).unwrap(),
                        "round {round} {cf}: from {p:?} limit {limit}"
                    );
                }
                checked += 1;
            }
        }
    }
    assert!(hidden > 20, "DB rows absent from R exercised: {hidden}");
    assert!(checked > 1000, "non-vacuous: {checked}");
}

/// The reads above on R's CFs never fall through to the DB: a DB holding a
/// row R lacks returns nothing for it through every read method.
#[test]
fn db_row_absent_from_resident_is_invisible() {
    let (db, _dir) = temp_db();
    let k = pos_key(1, 1);
    db.put_cf_raw(CF_NATIVE_POSITIONS, &k, b"db-only").unwrap();
    let mut overlay = NativeStateOverlay::new(db.clone());
    overlay.attach_resident(Arc::new(ResidentRows::default()));
    assert_eq!(overlay.get_cf_raw(CF_NATIVE_POSITIONS, &k).unwrap(), None);
    assert!(overlay.iterate_cf(CF_NATIVE_POSITIONS, None).unwrap().is_empty());
    assert!(overlay.iterate_cf(CF_NATIVE_POSITIONS, Some(&[1u8; 20])).unwrap().is_empty());
    assert!(overlay.iterate_cf_from(CF_NATIVE_POSITIONS, &[], 10).unwrap().is_empty());
    assert!(!overlay.prefix_exists(CF_NATIVE_POSITIONS, &[1u8; 20]).unwrap());
    let rows: BTreeMap<Vec<u8>, Vec<u8>> = overlay.iterate_cf(CF_NATIVE_ORDERS, None).unwrap().into_iter().collect();
    assert!(rows.is_empty());
    let detached = overlay.detach_resident();
    assert!(detached.is_some());
    assert_eq!(overlay.get_cf_raw(CF_NATIVE_POSITIONS, &k).unwrap(), Some(b"db-only".to_vec()), "detached: DB again");
}

/// Every read of `cf` the overlay offers, over `keys` and `prefixes`, as one
/// comparable dump (C6a).
fn read_dump(ov: &NativeStateOverlay, cf: &str, keys: &BTreeSet<Vec<u8>>, prefixes: &BTreeSet<Vec<u8>>) -> Vec<String> {
    let mut out = vec![format!("all {:?}", ov.iterate_cf(cf, None).unwrap())];
    for k in keys.iter().chain(prefixes.iter()) {
        out.push(format!("get {k:?} {:?}", ov.get_cf_raw(cf, k).unwrap()));
    }
    for p in prefixes {
        out.push(format!("pfx {p:?} {:?}", ov.iterate_cf(cf, Some(p)).unwrap()));
        out.push(format!("exists {p:?} {:?}", ov.prefix_exists(cf, p).unwrap()));
        for limit in [0usize, 1, 2, 5, usize::MAX] {
            out.push(format!("from {p:?} {limit} {:?}", ov.iterate_cf_from(cf, p, limit).unwrap()));
        }
    }
    out
}

/// C6a (B0): with R attached, R's two CFs read own pending -> R and never the
/// parent layer. Random DB / R / parent / pending layers (keys over a small
/// alphabet, every op mix), two forms of R:
/// * R already holds the parent (the node: `end_resident` applied it): the
///   overlay with the parent, the same overlay without it and a DB flushed
///   with R + parent + pending agree on all four read methods;
/// * R does not hold the parent (never on the node): the overlay with the
///   parent still reads exactly like the one without — the parent is not
///   consulted for R's CFs.
/// A non-R CF keeps reading the parent (non-vacuous: it differs).
#[test]
fn resident_cfs_skip_the_parent_layer() {
    let mut seed: u64 = 0x5EED_00C6;
    let mut rnd = |n: u64| {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (seed >> 33) % n
    };
    const ALPHABET: [u8; 4] = [0x00, 0x01, 0xfe, 0xff];
    let cfs: Vec<&str> = RESIDENT_CFS.iter().copied().chain([CF_NATIVE_ORDERS]).collect();
    let (mut compared, mut other_cf_differs, mut parent_touched) = (0usize, 0usize, 0usize);
    for round in 0..48 {
        let r_holds_parent = round % 2 == 0;
        let (db, _d1) = temp_db();
        let (flushed, _d2) = temp_db();
        let mut keys: BTreeSet<Vec<u8>> = BTreeSet::new();
        for _ in 0..4 + rnd(14) {
            let len = 1 + rnd(4) as usize;
            keys.insert((0..len).map(|_| ALPHABET[rnd(4) as usize]).collect());
        }
        // Per (cf, key): in DB, in R, parent op, pending op (0 none, 1 put, 2 delete).
        let ops: Vec<(&str, Vec<u8>, bool, bool, u64, u64)> = cfs
            .iter()
            .flat_map(|cf| keys.iter().map(move |k| (*cf, k.clone())))
            .map(|(cf, k)| (cf, k, rnd(2) == 1, rnd(2) == 1, rnd(3), rnd(3)))
            .collect();
        let mut r_rows = ResidentRows::default();
        let seed_delta = NativeStateOverlay::new(flushed.clone());
        for (cf, k, in_db, in_r, _, _) in &ops {
            let resident = RESIDENT_CFS.contains(cf);
            if *in_db {
                db.put_cf_raw(cf, k, &[b"db".as_slice(), k].concat()).unwrap();
                if !resident {
                    flushed.put_cf_raw(cf, k, &[b"db".as_slice(), k].concat()).unwrap();
                }
            }
            if resident && *in_r {
                seed_delta.put_cf_raw(cf, k, &[b"r".as_slice(), k].concat()).unwrap();
                flushed.put_cf_raw(cf, k, &[b"r".as_slice(), k].concat()).unwrap();
            }
        }
        r_rows.apply(&seed_delta.own_pending_delta());
        seed_delta.discard_tx();
        let layer = |ov: &NativeStateOverlay, layer: u8, into_flushed: bool| {
            for (cf, k, _, _, pa, pe) in &ops {
                let v = [&[layer], k.as_slice()].concat();
                match if layer == 0 { *pa } else { *pe } {
                    1 => {
                        ov.put_cf_raw(cf, k, &v).unwrap();
                        if into_flushed {
                            flushed.put_cf_raw(cf, k, &v).unwrap();
                        }
                    }
                    2 => {
                        ov.delete_cf_raw(cf, k).unwrap();
                        if into_flushed {
                            flushed.delete_cf_raw(cf, k).unwrap();
                        }
                    }
                    _ => {}
                }
            }
        };
        let p = NativeStateOverlay::new(db.clone());
        layer(&p, 0, r_holds_parent);
        if r_holds_parent {
            r_rows.apply(&p.own_pending_delta());
        }
        parent_touched += usize::from(!p.own_pending_delta().is_empty());
        let parent = p.freeze(1);
        let r = Arc::new(r_rows);
        let mut with_parent = NativeStateOverlay::with_parent(db.clone(), Some(parent));
        with_parent.attach_resident(r.clone());
        let mut without = NativeStateOverlay::with_parent(db.clone(), None);
        without.attach_resident(r);
        layer(&with_parent, 1, true);
        layer(&without, 1, false);

        let mut prefixes: BTreeSet<Vec<u8>> = BTreeSet::new();
        prefixes.insert(Vec::new());
        for k in &keys {
            for l in 1..=k.len() {
                prefixes.insert(k[..l].to_vec());
            }
            prefixes.insert([k.as_slice(), &[0xff]].concat());
            prefixes.insert([k.as_slice(), &[0x00]].concat());
        }
        for cf in cfs.iter().copied() {
            let a = read_dump(&with_parent, cf, &keys, &prefixes);
            let b = read_dump(&without, cf, &keys, &prefixes);
            if !RESIDENT_CFS.contains(&cf) {
                other_cf_differs += usize::from(a != b);
                continue;
            }
            for (x, y) in a.iter().zip(b.iter()) {
                assert_eq!(x, y, "round {round} {cf} (R holds parent: {r_holds_parent}): with vs without parent");
            }
            assert_eq!(a.len(), b.len());
            compared += a.len();
            if r_holds_parent {
                let want = read_dump(&NativeStateOverlay::new(flushed.clone()), cf, &keys, &prefixes);
                assert_eq!(a, want, "round {round} {cf}: R + parent + pending == flushed DB");
            }
        }
    }
    assert!(compared > 10_000, "non-vacuous: {compared}");
    assert!(parent_touched > 40, "parent layers with R-CF writes: {parent_touched}");
    assert!(other_cf_differs > 10, "a non-R CF still reads the parent: {other_cf_differs}");
}

/// C6a (B0), the direct form: a parent-layer write or tombstone of R's CFs
/// that R lacks is not visible through the overlay (R is the whole state
/// below own pending); in another CF it is.
#[test]
fn parent_rows_of_resident_cfs_are_not_read() {
    let (db, _dir) = temp_db();
    let k = pos_key(1, 1);
    db.put_cf_raw(CF_NATIVE_POSITIONS, &pos_key(2, 1), b"db").unwrap();
    let mut rows = ResidentRows::default();
    let seed = NativeStateOverlay::new(db.clone());
    seed.put_cf_raw(CF_NATIVE_POSITIONS, &pos_key(2, 1), b"r").unwrap();
    rows.apply(&seed.own_pending_delta());
    let parent = NativeStateOverlay::new(db.clone());
    parent.put_cf_raw(CF_NATIVE_POSITIONS, &k, b"parent").unwrap();
    parent.delete_cf_raw(CF_NATIVE_POSITIONS, &pos_key(2, 1)).unwrap();
    parent.put_cf_raw(CF_NATIVE_ORDERS, &[1u8; 44], b"parent-order").unwrap();
    let mut overlay = NativeStateOverlay::with_parent(db.clone(), Some(parent.freeze(1)));
    overlay.attach_resident(Arc::new(rows));
    let p = CF_NATIVE_POSITIONS;
    assert_eq!(overlay.get_cf_raw(p, &k).unwrap(), None);
    assert_eq!(overlay.get_cf_raw(p, &pos_key(2, 1)).unwrap(), Some(b"r".to_vec()));
    assert_eq!(overlay.iterate_cf(p, None).unwrap(), vec![(pos_key(2, 1), b"r".to_vec())]);
    assert!(overlay.iterate_cf(p, Some(&[1u8; 20])).unwrap().is_empty());
    assert_eq!(overlay.iterate_cf_from(p, &[], 10).unwrap(), vec![(pos_key(2, 1), b"r".to_vec())]);
    assert!(!overlay.prefix_exists(p, &[1u8; 20]).unwrap());
    assert!(overlay.prefix_exists(p, &[2u8; 20]).unwrap());
    assert_eq!(overlay.get_cf_raw(CF_NATIVE_ORDERS, &[1u8; 44]).unwrap(), Some(b"parent-order".to_vec()), "other CF: parent");
    // Own pending still overrides R in every method.
    overlay.put_cf_raw(p, &[1u8; 20 + 8], b"own").unwrap();
    overlay.delete_cf_raw(p, &pos_key(2, 1)).unwrap();
    assert_eq!(overlay.iterate_cf(p, None).unwrap(), vec![([1u8; 28].to_vec(), b"own".to_vec())]);
    assert!(!overlay.prefix_exists(p, &[2u8; 20]).unwrap());
    assert!(overlay.prefix_exists(p, &[1u8; 20]).unwrap());
}

/// C6b: `resident_changes` lists exactly the keys under the prefix that the
/// overlay's OWN pending set writes or deletes, each with R's row and the
/// current row (what `get_cf_raw` returns); the parent layer is in R. No R,
/// or a CF outside R: `None` (the `StateBackend` default too).
#[test]
fn resident_changes_lists_own_pending_keys_with_r_and_current_rows() {
    use super::ResidentChange;
    let (db, _dir) = temp_db();
    let p = CF_NATIVE_POSITIONS;
    assert!(db.resident_changes(p, &[1u8; 20]).is_none(), "StateDb: not available");
    let parent = NativeStateOverlay::new(db.clone());
    parent.put_cf_raw(p, &pos_key(1, 1), b"parent-1-1").unwrap();
    parent.put_cf_raw(p, &pos_key(1, 2), b"parent-1-2").unwrap();
    let mut overlay = NativeStateOverlay::with_parent(db.clone(), Some(parent.freeze(1)));
    assert!(overlay.resident_changes(p, &[1u8; 20]).is_none(), "no R");
    overlay.attach_resident(Arc::new(ResidentRows::build(&overlay).unwrap()));
    assert_eq!(overlay.resident_changes(p, &[1u8; 20]), Some(vec![]), "parent writes are in R");
    overlay.put_cf_raw(p, &pos_key(1, 2), b"own-1-2").unwrap();
    overlay.delete_cf_raw(p, &pos_key(1, 1)).unwrap();
    overlay.put_cf_raw(p, &pos_key(1, 3), b"own-1-3").unwrap();
    overlay.delete_cf_raw(p, &pos_key(1, 4)).unwrap();
    overlay.put_cf_raw(p, &pos_key(2, 1), b"other-trader").unwrap();
    overlay.put_cf_raw(CF_NATIVE_BALANCES, &[1u8; 20], b"bal").unwrap();
    let mut got = overlay.resident_changes(p, &[1u8; 20]).unwrap();
    got.sort_by(|a, b| a.key.cmp(&b.key));
    let ch = |m: u64, resident: Option<&[u8]>, current: Option<&[u8]>| ResidentChange {
        key: pos_key(1, m),
        resident: resident.map(<[u8]>::to_vec),
        current: current.map(<[u8]>::to_vec),
    };
    assert_eq!(
        got,
        vec![
            ch(1, Some(b"parent-1-1"), None),
            ch(2, Some(b"parent-1-2"), Some(b"own-1-2")),
            ch(3, None, Some(b"own-1-3")),
            ch(4, None, None),
        ]
    );
    for c in &got {
        assert_eq!(overlay.get_cf_raw(p, &c.key).unwrap(), c.current, "current == get_cf_raw");
    }
    assert!(overlay.resident_changes(CF_NATIVE_ORDERS, &[1u8; 20]).is_none(), "not an R CF");
    assert_eq!(overlay.resident_changes(CF_NATIVE_BALANCES, &[1u8; 20]).map(|v| v.len()), Some(1));
}

/// C6c: `layer_keys` = every key of the CF the overlay's OWN pending set
/// writes or deletes (sorted); with R only (else / `StateDb`: `None`). A
/// prefix touches iff some listed key starts with it (`layer_touches`).
#[test]
fn layer_keys_are_own_pending_keys_with_resident() {
    let (db, _dir) = temp_db();
    let p = CF_NATIVE_POSITIONS;
    assert!(db.layer_keys(p).is_none());
    let parent = NativeStateOverlay::new(db.clone());
    parent.put_cf_raw(p, &pos_key(1, 1), b"parent").unwrap();
    let mut overlay = NativeStateOverlay::with_parent(db.clone(), Some(parent.freeze(1)));
    assert!(overlay.layer_keys(p).is_none(), "no R");
    overlay.attach_resident(Arc::new(ResidentRows::build(&overlay).unwrap()));
    assert_eq!(overlay.layer_keys(p), Some(vec![]), "the parent is in R");
    overlay.put_cf_raw(p, &pos_key(3, 1), b"w").unwrap();
    overlay.delete_cf_raw(p, &pos_key(2, 7)).unwrap();
    overlay.put_cf_raw(p, &[9u8; 5], b"short").unwrap();
    overlay.put_cf_raw(CF_NATIVE_BALANCES, &[4u8; 20], b"b").unwrap();
    let keys = overlay.layer_keys(p).unwrap();
    assert_eq!(keys, vec![pos_key(2, 7), pos_key(3, 1), vec![9u8; 5]]);
    for t in 0..=10u8 {
        assert_eq!(overlay.layer_touches(p, &[t; 20]), keys.iter().any(|k| k.starts_with(&[t; 20])), "trader {t}");
    }
    assert!(overlay.layer_keys(CF_NATIVE_ORDERS).is_none(), "not an R CF");
    assert_eq!(overlay.layer_keys(CF_NATIVE_BALANCES), Some(vec![[4u8; 20].to_vec()]));
}

/// Item 6 Phase 4 step 4: `own_pending_row` (the typed loads' first step).
/// The default (`StateDb`) and an overlay without R: `Unknown` (read through
/// `get_cf_raw`). With R: the overlay's OWN pending write / tombstone as
/// bytes (`Pending`, equal to `get_cf_raw`), else `Resident` (then
/// `get_cf_raw` is R's row: a parent write is already in R and is not
/// pending here). A CF R does not hold: `Unknown`. Then over random R /
/// parent / pending states, every key of both CFs agrees with `get_cf_raw`.
#[test]
fn own_pending_row_matches_get_cf_raw() {
    use crate::backend::OwnPendingRow;
    let (db, _dir) = temp_db();
    let p = CF_NATIVE_POSITIONS;
    assert_eq!(db.own_pending_row(p, &pos_key(1, 1)), OwnPendingRow::Unknown, "StateDb");
    db.put_cf_raw(p, &pos_key(5, 1), b"db").unwrap();
    let parent = NativeStateOverlay::new(db.clone());
    parent.put_cf_raw(p, &pos_key(1, 1), b"parent").unwrap();
    let mut overlay = NativeStateOverlay::with_parent(db.clone(), Some(parent.freeze(1)));
    overlay.put_cf_raw(p, &pos_key(2, 1), b"own-before-R").unwrap();
    assert_eq!(overlay.own_pending_row(p, &pos_key(2, 1)), OwnPendingRow::Unknown, "no R");
    overlay.attach_resident(Arc::new(ResidentRows::build(&overlay).unwrap()));
    overlay.put_cf_raw(p, &pos_key(2, 1), b"own").unwrap();
    overlay.delete_cf_raw(p, &pos_key(3, 1)).unwrap();
    overlay.put_cf_raw(CF_NATIVE_ORDERS, &[1u8; 8], b"o").unwrap();
    assert_eq!(overlay.own_pending_row(p, &pos_key(2, 1)), OwnPendingRow::Pending(Some(b"own".to_vec())));
    assert_eq!(overlay.own_pending_row(p, &pos_key(3, 1)), OwnPendingRow::Pending(None), "own tombstone");
    assert_eq!(overlay.own_pending_row(p, &pos_key(1, 1)), OwnPendingRow::Resident, "parent write: in R");
    assert_eq!(overlay.get_cf_raw(p, &pos_key(1, 1)).unwrap(), Some(b"parent".to_vec()));
    assert_eq!(overlay.own_pending_row(p, &pos_key(5, 1)), OwnPendingRow::Resident, "a DB row: in R");
    assert_eq!(overlay.own_pending_row(p, &pos_key(4, 1)), OwnPendingRow::Resident, "absent");
    assert_eq!(overlay.own_pending_row(CF_NATIVE_BALANCES, &pos_key(2, 1)), OwnPendingRow::Resident, "other CF");
    assert_eq!(overlay.own_pending_row(CF_NATIVE_ORDERS, &[1u8; 8]), OwnPendingRow::Unknown, "not an R CF");

    let mut seed: u64 = 0x0057_E940;
    let mut rnd = |n: u64| {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (seed >> 33) % n
    };
    let (mut pending, mut resident) = (0usize, 0usize);
    for _round in 0..30 {
        let (db, _d) = temp_db();
        let keys: Vec<Vec<u8>> = (0..12u8).map(|t| pos_key(t % 4, u64::from(t / 4))).chain(odd_keys()).collect();
        for cf in [p, CF_NATIVE_BALANCES] {
            for k in &keys {
                if rnd(2) == 0 {
                    db.put_cf_raw(cf, k, &[rnd(200) as u8; 3]).unwrap();
                }
            }
        }
        let parent = NativeStateOverlay::new(db.clone());
        let mut overlay_ops = Vec::new();
        for cf in [p, CF_NATIVE_BALANCES] {
            for k in &keys {
                match rnd(3) {
                    1 => parent.put_cf_raw(cf, k, &[0xAA, rnd(200) as u8]).unwrap(),
                    2 => parent.delete_cf_raw(cf, k).unwrap(),
                    _ => {}
                }
                overlay_ops.push((cf, k.clone(), rnd(3)));
            }
        }
        let mut overlay = NativeStateOverlay::with_parent(db.clone(), Some(parent.freeze(1)));
        let rows = Arc::new(ResidentRows::build(&overlay).unwrap());
        overlay.attach_resident(rows.clone());
        for (cf, k, op) in &overlay_ops {
            match op {
                1 => overlay.put_cf_raw(cf, k, &[0xBB, rnd(200) as u8]).unwrap(),
                2 => overlay.delete_cf_raw(cf, k).unwrap(),
                _ => {}
            }
        }
        for (cf, k, op) in &overlay_ops {
            let now = overlay.get_cf_raw(cf, k).unwrap();
            match overlay.own_pending_row(cf, k) {
                OwnPendingRow::Pending(row) => {
                    assert_ne!(*op, 0, "{cf} {k:?}: pending without an own write");
                    assert_eq!(row, now, "{cf} {k:?}: pending bytes == get_cf_raw");
                    pending += 1;
                }
                OwnPendingRow::Resident => {
                    assert_eq!(*op, 0, "{cf} {k:?}: an own write reads as resident");
                    assert_eq!(rows.rows(cf).unwrap().get(k).cloned(), now, "{cf} {k:?}: R's row == get_cf_raw");
                    resident += 1;
                }
                OwnPendingRow::Unknown => panic!("{cf} {k:?}: unknown with R attached"),
            }
        }
    }
    assert!(pending > 100 && resident > 100, "both outcomes covered ({pending} / {resident})");
}

/// Item 6 E2-E4: R's column families — positions and balances (C1), the
/// liquidation rows (E2: cooldown / pending / previous marks / cursor) and
/// the oracle rows (E3: submissions, aggregates, signer index) and the market
/// rows (E4: listings, margin configs, the order-id counter / book-mode
/// marker).
#[test]
fn resident_cfs_are_the_native_hot_cfs() {
    assert_eq!(
        RESIDENT_CFS.to_vec(),
        vec![CF_NATIVE_POSITIONS, CF_NATIVE_BALANCES, CF_NATIVE_LIQUIDATION, CF_NATIVE_ORACLE, CF_NATIVE_MARKETS]
    );
}

/// Item 6 cut 5: the frozen set's delta (`FrozenPending::resident_delta`, the
/// worker's) equals the overlay's own delta taken just before `freeze`, on
/// random writes and tombstones of R's CFs and others, over a parent layer,
/// with nested checkpoints rolled back and committed.
#[test]
fn frozen_resident_delta_equals_the_delta_before_freeze() {
    let (db, _dir) = temp_db();
    let mut seed = 0xa409_3822_299f_31d0u64;
    let mut rnd = move |n: u64| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed % n
    };
    let cfs = [CF_NATIVE_POSITIONS, CF_NATIVE_BALANCES, CF_NATIVE_LIQUIDATION, CF_NATIVE_ORACLE, CF_NATIVE_MARKETS, CF_NATIVE_ORDERS];
    let mut nonempty = 0;
    for round in 0..200u64 {
        let parent = (round % 3 != 0).then(|| {
            let p = NativeStateOverlay::new(db.clone());
            for _ in 0..rnd(20) {
                let cf = cfs[rnd(cfs.len() as u64) as usize];
                p.put_cf_raw(cf, &pos_key(rnd(5) as u8, rnd(4)), b"parent").unwrap();
            }
            p.freeze(round)
        });
        let overlay = NativeStateOverlay::with_parent(db.clone(), parent);
        for step in 0..rnd(60) {
            let cf = cfs[rnd(cfs.len() as u64) as usize];
            let key = if rnd(8) == 0 { odd_keys()[rnd(5) as usize].clone() } else { pos_key(rnd(6) as u8, rnd(5)) };
            match rnd(5) {
                0 => overlay.delete_cf_raw(cf, &key).unwrap(),
                1 => {
                    overlay.checkpoint();
                    overlay.put_cf_raw(cf, &key, b"rolled back").unwrap();
                    overlay.revert_to_checkpoint();
                }
                2 => {
                    overlay.checkpoint();
                    overlay.put_cf_raw(cf, &key, b"committed").unwrap();
                    overlay.commit_checkpoint();
                }
                _ => overlay.put_cf_raw(cf, &key, &step.to_be_bytes()).unwrap(),
            }
        }
        let before = overlay.own_pending_delta();
        nonempty += usize::from(!before.is_empty());
        let frozen = overlay.freeze(round + 1);
        assert_eq!(frozen.resident_delta(), before, "round {round}");
    }
    assert!(nonempty > 100, "{nonempty}");
}

// ======================================================================
// Item 6 Phase 4 step 2 (plan Task 2.1): R's first two CFs from a
// byte-level source (`Users` in torus-bridge), the other three raw.
// ======================================================================

use super::{ResidentRowIter, ResidentSource};
use crate::checkpoint_layer::{CheckpointLayer, DeferredLayer};
use std::borrow::Cow;
use std::sync::atomic::{AtomicUsize, Ordering};

/// A source for R's positions (slot 0) and balances (slot 1) that hands out
/// owned rows built per read, as `Users` does; counts its reads.
#[derive(Debug, Default)]
struct TestSource {
    cfs: [BTreeMap<Vec<u8>, Vec<u8>>; 2],
    reads: AtomicUsize,
}

impl TestSource {
    /// The rows of R's first two CFs as `rows` (raw R) holds them.
    fn of(rows: &ResidentRows) -> Self {
        Self {
            cfs: [
                rows.rows(CF_NATIVE_POSITIONS).expect("raw R").clone(),
                rows.rows(CF_NATIVE_BALANCES).expect("raw R").clone(),
            ],
            reads: AtomicUsize::new(0),
        }
    }

    /// Every row of both CFs as `backend` sees them.
    fn from_backend<B: StateBackend>(backend: &B) -> Self {
        Self {
            cfs: [CF_NATIVE_POSITIONS, CF_NATIVE_BALANCES]
                .map(|cf| backend.iterate_cf(cf, None).unwrap().into_iter().collect()),
            reads: AtomicUsize::new(0),
        }
    }
}

impl ResidentSource for TestSource {
    fn get(&self, slot: usize, key: &[u8]) -> Option<Vec<u8>> {
        self.reads.fetch_add(1, Ordering::Relaxed);
        self.cfs[slot].get(key).cloned()
    }

    fn range<'a>(&'a self, slot: usize, start: &[u8], prefix: &[u8]) -> ResidentRowIter<'a> {
        self.reads.fetch_add(1, Ordering::Relaxed);
        let prefix = prefix.to_vec();
        Box::new(
            self.cfs[slot]
                .range::<[u8], _>((std::ops::Bound::Included(start), std::ops::Bound::Unbounded))
                .take_while(move |(k, _)| k.starts_with(&prefix))
                .map(|(k, v)| (Cow::Owned(k.clone()), Cow::Owned(v.clone()))),
        )
    }
}

/// Every read of `cf` the overlay offers (the step 2 set: point, length,
/// full and prefix scans, seeks with and without a prefix bound, prefix
/// checks, and the own-pending introspection R enables), over `keys` and
/// `prefixes`, as one comparable dump.
fn full_read_dump(ov: &NativeStateOverlay, cf: &str, keys: &BTreeSet<Vec<u8>>, prefixes: &BTreeSet<Vec<u8>>) -> Vec<String> {
    let mut out = read_dump(ov, cf, keys, prefixes);
    for k in keys.iter().chain(prefixes.iter()) {
        out.push(format!("len {k:?} {:?}", ov.get_cf_len(cf, k).unwrap()));
    }
    for p in prefixes {
        out.push(format!("touches {p:?} {}", ov.layer_touches(cf, p)));
        out.push(format!("changes {p:?} {:?}", ov.resident_changes(cf, p)));
        let starts = [p.clone(), [p.as_slice(), &[0x00]].concat(), [p.as_slice(), &[0xff]].concat(), p[..p.len().saturating_sub(1)].to_vec()];
        for s in starts.iter().chain(keys.iter()) {
            for limit in [1usize, 3, usize::MAX] {
                out.push(format!("pfrom {p:?} {s:?} {limit} {:?}", ov.iterate_cf_prefix_from(cf, p, s, limit).unwrap()));
            }
        }
    }
    out.push(format!("keys {:?}", ov.layer_keys(cf)));
    out
}

/// Plan Task 2.1: for every layering (own pending, parent, L, C, DB) and
/// random keys, an overlay whose positions and balances come from a source
/// answers `get_cf_raw`, `get_cf_len`, `iterate_cf`, `iterate_cf_from`,
/// `iterate_cf_prefix_from`, `prefix_exists`, `layer_touches`, `layer_keys`
/// and `resident_changes` exactly as the same rows held raw in R; the other
/// R CFs (raw in both) and a non-R CF too.
#[test]
fn resident_source_reads_match_raw_rows() {
    let mut seed: u64 = 0x5EED_0402;
    let mut rnd = |n: u64| {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (seed >> 33) % n
    };
    const ALPHABET: [u8; 4] = [0x00, 0x01, 0xfe, 0xff];
    let cfs: Vec<&str> = RESIDENT_CFS.iter().copied().chain([CF_NATIVE_ORDERS]).collect();
    let (mut compared, mut source_reads, mut layered) = (0usize, 0usize, 0usize);
    for round in 0..32 {
        let (db, _d) = temp_db();
        let mut keys: BTreeSet<Vec<u8>> = BTreeSet::new();
        for _ in 0..4 + rnd(12) {
            let len = 1 + rnd(4) as usize;
            keys.insert((0..len).map(|_| ALPHABET[rnd(4) as usize]).collect());
        }
        // Per (cf, key): in DB, in R, parent / L / C / pending op (0 none, 1 put, 2 delete).
        let ops: Vec<(&str, Vec<u8>, bool, bool, [u64; 4])> = cfs
            .iter()
            .flat_map(|cf| keys.iter().map(move |k| (*cf, k.clone())))
            .map(|(cf, k)| (cf, k, rnd(2) == 1, rnd(2) == 1, [rnd(3), rnd(3), rnd(3), rnd(3)]))
            .collect();
        let mut raw = ResidentRows::default();
        let seed_delta = NativeStateOverlay::new(db.clone());
        for (cf, k, in_db, in_r, _) in &ops {
            if *in_db {
                db.put_cf_raw(cf, k, &[b"db".as_slice(), k].concat()).unwrap();
            }
            if RESIDENT_CFS.contains(cf) && *in_r {
                seed_delta.put_cf_raw(cf, k, &[b"r".as_slice(), k].concat()).unwrap();
            }
        }
        raw.apply(&seed_delta.own_pending_delta());
        seed_delta.discard_tx();
        let layer_of = |layer: usize, height: u64| {
            let ov = NativeStateOverlay::new(db.clone());
            for (cf, k, _, _, op) in &ops {
                match op[layer] {
                    1 => ov.put_cf_raw(cf, k, &[&[layer as u8], k.as_slice()].concat()).unwrap(),
                    2 => ov.delete_cf_raw(cf, k).unwrap(),
                    _ => {}
                }
            }
            ov.freeze(height)
        };
        let parent = (round % 4 != 0).then(|| layer_of(0, 9));
        let ckpt = |layer: usize, height: u64| {
            (round % 3 != 0).then(|| {
                let mut l = CheckpointLayer::new();
                l.merge(&layer_of(layer, height));
                Arc::new(l)
            })
        };
        let (l, c) = (ckpt(1, 8), ckpt(2, 7));
        layered += usize::from(parent.is_some() && l.is_some());
        let source = Arc::new(TestSource::of(&raw));
        let mut from_source = raw.clone();
        from_source.set_source(source.clone());
        assert!(from_source.has_source() && !raw.has_source());
        assert!(from_source.rows(CF_NATIVE_POSITIONS).is_none(), "the source replaces the raw map");
        let overlay = |r: ResidentRows| {
            let mut ov = NativeStateOverlay::with_layers(db.clone(), parent.clone(), l.clone(), c.clone());
            ov.attach_resident(Arc::new(r));
            for (cf, k, _, _, op) in &ops {
                match op[3] {
                    1 => ov.put_cf_raw(cf, k, &[b"own".as_slice(), k].concat()).unwrap(),
                    2 => ov.delete_cf_raw(cf, k).unwrap(),
                    _ => {}
                }
            }
            ov
        };
        let (a, b) = (overlay(raw), overlay(from_source));
        let mut prefixes: BTreeSet<Vec<u8>> = BTreeSet::new();
        prefixes.insert(Vec::new());
        for k in &keys {
            for n in 1..=k.len() {
                prefixes.insert(k[..n].to_vec());
            }
            prefixes.insert([k.as_slice(), &[0xff]].concat());
        }
        for cf in cfs.iter().copied() {
            let (x, y) = (full_read_dump(&a, cf, &keys, &prefixes), full_read_dump(&b, cf, &keys, &prefixes));
            assert_eq!(x.len(), y.len());
            for (x, y) in x.iter().zip(y.iter()) {
                assert_eq!(x, y, "round {round} {cf}: raw R vs source");
            }
            compared += x.len();
        }
        source_reads += source.reads.load(Ordering::Relaxed);
    }
    assert!(compared > 20_000, "non-vacuous: {compared}");
    assert!(source_reads > 5_000, "the source served the reads: {source_reads}");
    assert!(layered > 10, "parent and L both present: {layered}");
}

/// Plan rows 9 / 12 (merge wait): with R's source attached, no read of the
/// positions or balances CF goes past R, so none reaches L's deferred part:
/// with that part lost (its merge died: a read reaching it errors) every
/// read method answers, and with the part still being merged no read
/// blocks. Without the source (taken off) the same reads reach L
/// (non-vacuous: they error / block).
#[test]
fn resident_source_reads_never_reach_the_deferred_part() {
    use std::time::Duration;
    let (db, _dir) = temp_db();
    for t in 1..=4u8 {
        db.put_cf_raw(CF_NATIVE_POSITIONS, &pos_key(t, 1), &[b'p', t]).unwrap();
        db.put_cf_raw(CF_NATIVE_BALANCES, &[t; 20], &[b'b', t]).unwrap();
        db.put_cf_raw(CF_NATIVE_LIQUIDATION, &[t; 3], b"liq").unwrap();
    }
    let parent = NativeStateOverlay::new(db.clone());
    parent.put_cf_raw(CF_NATIVE_POSITIONS, &pos_key(5, 1), b"parent").unwrap();
    let parent = parent.freeze(4);
    let built = NativeStateOverlay::with_parent(db.clone(), Some(parent.clone()));
    let source: Arc<dyn ResidentSource> = Arc::new(TestSource::from_backend(&built));
    let r = ResidentRows::build_with_source(&built, source).unwrap();
    let reads = |ov: &NativeStateOverlay| -> Result<usize, crate::error::StateError> {
        let mut n = 0;
        for cf in [CF_NATIVE_POSITIONS, CF_NATIVE_BALANCES] {
            for t in 0..=6u8 {
                n += usize::from(ov.get_cf_raw(cf, &pos_key(t, 1))?.is_some());
                n += usize::from(ov.get_cf_raw(cf, &[t; 20])?.is_some());
                n += usize::from(ov.get_cf_len(cf, &[t; 20])?.is_some());
                n += ov.iterate_cf(cf, Some(&[t; 20]))?.len();
                n += usize::from(ov.prefix_exists(cf, &[t; 20])?);
                n += ov.iterate_cf_prefix_from(cf, &[t; 20], &pos_key(t, 0), 4)?.len();
            }
            n += ov.iterate_cf(cf, None)?.len();
            n += ov.iterate_cf_from(cf, &[], 100)?.len();
        }
        Ok(n)
    };
    let split = |deferred: Arc<DeferredLayer>, r: Option<ResidentRows>| {
        let mut main = CheckpointLayer::new();
        let part = main.split_deferred();
        drop(part);
        let mut ov = NativeStateOverlay::with_split_layers(db.clone(), Some(parent.clone()), Some(Arc::new(main)), Some(deferred), None);
        if let Some(r) = r {
            ov.attach_resident(Arc::new(r));
        }
        ov
    };
    // Lost part: a read reaching it is an error.
    let lost = || {
        let (tx, rx) = std::sync::mpsc::sync_channel::<CheckpointLayer>(1);
        drop(tx);
        Arc::new(DeferredLayer::pending(rx))
    };
    let with_source = split(lost(), Some(r.clone()));
    assert!(reads(&with_source).unwrap() > 20, "every read answered from R");
    let mut detached = r.clone();
    assert!(detached.take_source().is_some());
    assert!(!detached.has_source());
    assert!(detached.dump(CF_NATIVE_POSITIONS).is_none(), "no source: R does not hold the CF");
    assert!(reads(&split(lost(), Some(detached))).is_err(), "without the source the reads reach L");
    assert!(reads(&split(lost(), None)).is_err(), "no R: the reads reach L");
    // Part still being merged: no read blocks, none counted.
    let (_tx, rx) = std::sync::mpsc::sync_channel::<CheckpointLayer>(1);
    let deferred = Arc::new(DeferredLayer::pending(rx));
    let ov = split(deferred.clone(), Some(r.clone()));
    let (done_tx, done) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = done_tx.send(reads(&ov).map_err(|e| e.to_string()));
    });
    assert!(done.recv_timeout(Duration::from_secs(10)).expect("no read waits for the deferred part").unwrap() > 20);
    assert_eq!(deferred.take_read_waits().0, 0, "no read blocked");
    assert!(!deferred.is_settled());
}

/// Plan Task 2.1: `build_with_source` holds the liquidation, oracle and
/// market rows raw (== `iterate_cf`) and the source for positions and
/// balances (no raw map; `dump` reads the source); `len` / `bytes` count
/// the raw rows only; `apply` leaves the source's CFs to the source's owner;
/// taking the source off leaves those CFs to the layers below (the overlay
/// then reads them like an overlay without R); setting it again restores
/// R. A source R and a raw R with the same rows are equal.
#[test]
fn build_with_source_holds_three_cfs_raw_and_the_source() {
    let (db, _dir) = temp_db();
    for cf in RESIDENT_CFS {
        for k in odd_keys().iter().chain([pos_key(1, 1), pos_key(2, 3), vec![2u8; 20]].iter()) {
            db.put_cf_raw(cf, k, &[cf.as_bytes(), k].concat()).unwrap();
        }
    }
    let parent = NativeStateOverlay::new(db.clone());
    for cf in RESIDENT_CFS {
        parent.put_cf_raw(cf, &[0x04; 3], b"parent-new").unwrap();
        parent.delete_cf_raw(cf, &pos_key(1, 1)).unwrap();
    }
    let overlay = NativeStateOverlay::with_parent(db.clone(), Some(parent.freeze(1)));
    let source = Arc::new(TestSource::from_backend(&overlay));
    let mut r = ResidentRows::build_with_source(&overlay, source.clone()).unwrap();
    assert!(r.has_source());
    let mut raw_total = (0, 0);
    for (slot, cf) in RESIDENT_CFS.iter().enumerate() {
        let want = overlay.iterate_cf(cf, None).unwrap();
        assert!(!want.is_empty(), "{cf}: non-vacuous");
        assert_eq!(r.dump(cf).unwrap(), want, "{cf}: dump");
        if slot < 2 {
            assert!(r.rows(cf).is_none(), "{cf}: not held raw");
        } else {
            assert_eq!(rows_of(&r, cf), want, "{cf}: raw");
            raw_total.0 += want.len();
            raw_total.1 += want.iter().map(|(k, v)| k.len() + v.len()).sum::<usize>();
        }
    }
    assert_eq!((r.len(), r.bytes()), raw_total, "len / bytes: the raw rows only");
    assert_eq!(r, ResidentRows::build(&overlay).unwrap(), "source R == raw R over the same rows");
    // A delta over every R CF: the raw CFs take it, the source CFs do not.
    let block = NativeStateOverlay::new(db.clone());
    for cf in RESIDENT_CFS {
        block.put_cf_raw(cf, &[0x05; 4], b"block").unwrap();
        block.delete_cf_raw(cf, &vec![2u8; 20]).unwrap();
    }
    r.apply(&block.own_pending_delta());
    for (slot, cf) in RESIDENT_CFS.iter().enumerate() {
        let dump = r.dump(cf).unwrap();
        let written = dump.iter().any(|(k, _)| k == &[0x05; 4]);
        assert_eq!(written, slot >= 2, "{cf}: applied iff raw");
        if slot < 2 {
            assert_eq!(dump, overlay.iterate_cf(cf, None).unwrap(), "{cf}: the source's rows, untouched");
        }
    }
    // Taken off: the overlay reads the two CFs below R (here: the DB).
    let taken = r.take_source().expect("attached");
    assert!(Arc::ptr_eq(&taken, &(source.clone() as Arc<dyn ResidentSource>)));
    assert!(r.take_source().is_none());
    let mut ov = NativeStateOverlay::new(db.clone());
    ov.attach_resident(Arc::new(r.clone()));
    for cf in RESIDENT_CFS {
        let below = StateBackend::iterate_cf(&db, cf, None).unwrap();
        let read = ov.iterate_cf(cf, None).unwrap();
        assert_eq!(read == below, resident_slot_is_source(cf), "{cf}: below R iff a source CF");
    }
    assert!(ov.layer_keys(CF_NATIVE_POSITIONS).is_none(), "not resident without the source");
    assert!(ov.layer_keys(CF_NATIVE_LIQUIDATION).is_some());
    r.set_source(taken);
    assert_eq!(r.dump(CF_NATIVE_BALANCES).unwrap(), overlay.iterate_cf(CF_NATIVE_BALANCES, None).unwrap());
}

fn resident_slot_is_source(cf: &str) -> bool {
    cf == CF_NATIVE_POSITIONS || cf == CF_NATIVE_BALANCES
}

/// Item 6 Phase 5 step 2 r2: an in-place source of R's positions (slot 0)
/// and balances (slot 1), as the node's record set: a write goes in at once
/// and marks its key dirty; [`ResidentSource::drain_deferred`] takes the
/// dirty rows at the call and hands them out when the returned job runs,
/// logging the job's thread.
#[derive(Debug, Default)]
struct InPlaceSource {
    cfs: std::sync::Mutex<[BTreeMap<Vec<u8>, Vec<u8>>; 2]>,
    dirty: std::sync::Mutex<[BTreeSet<Vec<u8>>; 2]>,
    jobs: Arc<std::sync::Mutex<Vec<std::thread::ThreadId>>>,
}

impl InPlaceSource {
    fn from_backend<B: StateBackend>(backend: &B) -> Self {
        let cfs = [CF_NATIVE_POSITIONS, CF_NATIVE_BALANCES]
            .map(|cf| backend.iterate_cf(cf, None).unwrap().into_iter().collect());
        Self { cfs: std::sync::Mutex::new(cfs), ..Self::default() }
    }

    fn jobs(&self) -> Vec<std::thread::ThreadId> {
        self.jobs.lock().unwrap().clone()
    }
}

impl ResidentSource for InPlaceSource {
    fn get(&self, slot: usize, key: &[u8]) -> Option<Vec<u8>> {
        self.cfs.lock().unwrap()[slot].get(key).cloned()
    }

    fn range<'a>(&'a self, slot: usize, start: &[u8], prefix: &[u8]) -> ResidentRowIter<'a> {
        let rows: Vec<(Vec<u8>, Vec<u8>)> = self.cfs.lock().unwrap()[slot]
            .range::<[u8], _>((std::ops::Bound::Included(start), std::ops::Bound::Unbounded))
            .take_while(|(k, _)| k.starts_with(prefix))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        Box::new(rows.into_iter().map(|(k, v)| (Cow::Owned(k), Cow::Owned(v))))
    }

    fn in_place(&self) -> bool {
        true
    }

    fn put_row(&self, slot: usize, key: &[u8], value: Option<&[u8]>) {
        let mut cfs = self.cfs.lock().unwrap();
        match value {
            Some(v) => cfs[slot].insert(key.to_vec(), v.to_vec()),
            None => cfs[slot].remove(key),
        };
        self.dirty.lock().unwrap()[slot].insert(key.to_vec());
    }

    fn dirty_keys(&self, slot: usize) -> Vec<Vec<u8>> {
        self.dirty.lock().unwrap()[slot].iter().cloned().collect()
    }

    fn touches(&self, slot: usize, prefix: &[u8]) -> bool {
        self.dirty.lock().unwrap()[slot]
            .range::<[u8], _>((std::ops::Bound::Included(prefix), std::ops::Bound::Unbounded))
            .next()
            .is_some_and(|k| k.starts_with(prefix))
    }

    fn drain_deferred(&self) -> super::SourceDrain {
        let dirty = std::mem::take(&mut *self.dirty.lock().unwrap());
        let cfs = self.cfs.lock().unwrap();
        let taken: [super::SourceRows; 2] =
            [0usize, 1].map(|s| dirty[s].iter().map(|k| (k.clone(), cfs[s].get(k).cloned())).collect());
        let jobs = self.jobs.clone();
        Box::new(move || {
            jobs.lock().unwrap().push(std::thread::current().id());
            taken
        })
    }

    fn drain(&self) -> [super::SourceRows; 2] {
        self.drain_deferred()()
    }
}

/// One block's writes for the r2 tests: positions and balances (writes,
/// rewrites, deletes, a delete of an absent key) and rows of CFs outside
/// the source (R's raw CFs and one outside R).
fn r2_block(ov: &NativeStateOverlay) {
    for t in 1..=6u8 {
        ov.put_cf_raw(CF_NATIVE_POSITIONS, &pos_key(t, 1), &[b'n', t]).unwrap();
        ov.put_cf_raw(CF_NATIVE_POSITIONS, &pos_key(t, 2), &[b'm', t, t]).unwrap();
        ov.put_cf_raw(CF_NATIVE_BALANCES, &[t; 20], &[b'B', t]).unwrap();
    }
    ov.put_cf_raw(CF_NATIVE_POSITIONS, &pos_key(2, 2), b"rewritten").unwrap();
    ov.delete_cf_raw(CF_NATIVE_POSITIONS, &pos_key(3, 1)).unwrap();
    ov.delete_cf_raw(CF_NATIVE_POSITIONS, &pos_key(9, 9)).unwrap();
    ov.delete_cf_raw(CF_NATIVE_BALANCES, &[4; 20]).unwrap();
    ov.put_cf_raw(CF_NATIVE_LIQUIDATION, &[7; 3], b"liq").unwrap();
    ov.put_cf_raw(CF_NATIVE_ORACLE, &[8; 5], b"orc").unwrap();
    ov.put_cf_raw(CF_NATIVE_ORDERS, &[9; 6], b"ord").unwrap();
}

/// The r2 tests' DB: base rows of every CF they write.
fn r2_db() -> (StateDb, tempfile::TempDir) {
    let (db, dir) = temp_db();
    for t in 1..=4u8 {
        db.put_cf_raw(CF_NATIVE_POSITIONS, &pos_key(t, 1), &[b'p', t]).unwrap();
        db.put_cf_raw(CF_NATIVE_BALANCES, &[t; 20], &[b'b', t]).unwrap();
        db.put_cf_raw(CF_NATIVE_LIQUIDATION, &[t; 3], b"base").unwrap();
    }
    (db, dir)
}

/// Every CF's rows of a DB (the flush comparison).
fn db_dump(db: &StateDb) -> Vec<(String, Vec<(Vec<u8>, Vec<u8>)>)> {
    crate::cf::ALL_CF_NAMES
        .iter()
        .map(|cf| (cf.to_string(), StateBackend::iterate_cf(db, cf, None).unwrap()))
        .collect()
}

/// Item 6 Phase 5 step 2 r2 (plan revisit: "the drain encoding moves to the
/// flush worker"): with an in-place source attached, `freeze` takes the
/// source's rows of the block but does not run the drain job: the frozen
/// set's positions and balances stay pending, and nothing that reads only
/// other CFs (the parent-layer reads of the next block, the EVM-account
/// list, R's raw delta, a lookup of another CF) runs it. The first reader
/// of the two CFs runs it, on its own thread (here a stand-in for the flush
/// worker's digest), exactly once; every later read is served from it.
#[test]
fn freeze_leaves_the_in_place_drain_to_its_first_reader() {
    let (db, _dir) = r2_db();
    let source = Arc::new(InPlaceSource::from_backend(&db));
    let mut ov = NativeStateOverlay::new(db.clone());
    let r = ResidentRows::build_with_source(&ov, source.clone()).unwrap();
    ov.attach_resident(Arc::new(r));
    r2_block(&ov);
    assert_eq!(ov.pending_rows(CF_NATIVE_POSITIONS), 0, "written through to the source");
    let frozen = ov.freeze(1);
    assert!(source.jobs().is_empty(), "freeze ran the drain job: {:?}", source.jobs());
    assert!(frozen.typed_pending(), "the typed rows are serialized later");
    // Readers of other CFs only.
    let next = NativeStateOverlay::with_parent(db.clone(), Some(frozen.clone()));
    assert_eq!(next.get_cf_raw(CF_NATIVE_ORDERS, &[9; 6]).unwrap().as_deref(), Some(&b"ord"[..]));
    assert_eq!(next.get_cf_raw(CF_NATIVE_LIQUIDATION, &[7; 3]).unwrap().as_deref(), Some(&b"liq"[..]));
    assert_eq!(next.iterate_cf(CF_NATIVE_ORACLE, None).unwrap().len(), 1);
    assert_eq!(frozen.lookup_row(CF_NATIVE_ORDERS, &[9; 6]), Some(Some(b"ord".to_vec())));
    assert!(frozen.dirty_evm_accounts().is_empty());
    let raw = frozen.resident_delta_raw();
    assert!(raw.keys(CF_NATIVE_LIQUIDATION).count() == 1 && raw.keys(CF_NATIVE_POSITIONS).count() == 0);
    assert!(source.jobs().is_empty(), "a reader of another CF ran the drain job: {:?}", source.jobs());
    assert!(frozen.typed_pending());
    // The first reader of the two CFs, on another thread (the flush worker).
    let worker = {
        let frozen = frozen.clone();
        std::thread::spawn(move || {
            let n = frozen.rows(CF_NATIVE_POSITIONS).len();
            (std::thread::current().id(), n)
        })
        .join()
        .unwrap()
    };
    assert_eq!(source.jobs(), vec![worker.0], "the job ran once, on the first reader's thread");
    assert_eq!(worker.1, 13, "positions rows: 12 written, 1 deleted present, 1 absent deleted (once each)");
    assert!(!frozen.typed_pending());
    // Later reads (this thread, the next block's parent reads) reuse it.
    assert_eq!(frozen.rows(CF_NATIVE_BALANCES).len(), 6);
    assert_eq!(next.get_cf_raw(CF_NATIVE_POSITIONS, &pos_key(2, 2)).unwrap().as_deref(), Some(&b"rewritten"[..]));
    assert_eq!(source.jobs().len(), 1, "exactly once");
}

/// Item 6 Phase 5 step 2 r2: a frozen set whose positions and balances are
/// serialized later answers every reader exactly as the same block frozen
/// with every row pending (the Phase 4 / serial shape): each CF's rows, the
/// byte measure, the entry count, `touches_only`, the ingress slice, R's
/// delta, point lookups, the next block's parent-layer reads, the
/// checkpoint layer merge, and the flush (DB rows, applied marker and
/// running hash, on two DBs). Each case starts from a fresh frozen set, so
/// every accessor is also the one that runs the job.
#[test]
fn deferred_typed_rows_read_as_eager_rows() {
    let (db, _dir) = r2_db();
    crate::running_hash::configure_activation(&db, Some(1)).unwrap();
    let eager = {
        let ov = NativeStateOverlay::new(db.clone());
        r2_block(&ov);
        ov.freeze(1)
    };
    assert!(!eager.typed_pending());
    let deferred = || {
        let source = Arc::new(InPlaceSource::from_backend(&db));
        let mut ov = NativeStateOverlay::new(db.clone());
        let r = ResidentRows::build_with_source(&ov, source.clone()).unwrap();
        ov.attach_resident(Arc::new(r));
        r2_block(&ov);
        let frozen = ov.freeze(1);
        (frozen, source)
    };
    let cases: Vec<(&str, Box<dyn Fn(&crate::backend::FrozenPending) -> String>)> = vec![
        ("rows", Box::new(|f| format!("{:?}", crate::cf::ALL_CF_NAMES.iter().map(|cf| f.rows(cf)).collect::<Vec<_>>()))),
        ("state_bytes", Box::new(|f| f.state_bytes().to_string())),
        ("entry_count", Box::new(|f| f.entry_count().to_string())),
        (
            "touches_only",
            Box::new(|f| {
                format!(
                    "{} {} {}",
                    f.touches_only(&[CF_NATIVE_POSITIONS, CF_NATIVE_BALANCES]),
                    f.touches_only(&[CF_NATIVE_LIQUIDATION, CF_NATIVE_ORACLE, CF_NATIVE_ORDERS]),
                    f.touches_only(&crate::cf::ALL_CF_NAMES),
                )
            }),
        ),
        (
            "slice",
            Box::new(|f| {
                let s = f.slice(&crate::read_view::INGRESS_CFS);
                format!("{} {:?}", s.state_bytes(), crate::cf::ALL_CF_NAMES.iter().map(|cf| s.rows(cf)).collect::<Vec<_>>())
            }),
        ),
        ("resident_delta", Box::new(|f| format!("{:?}", f.resident_delta()))),
        (
            "lookup_row",
            Box::new(|f| {
                let keys = [pos_key(2, 2), pos_key(3, 1), pos_key(9, 9), pos_key(4, 1), vec![4; 20], vec![1; 20]];
                format!("{:?}", keys.iter().map(|k| (f.lookup_row(CF_NATIVE_POSITIONS, k), f.lookup_row(CF_NATIVE_BALANCES, k))).collect::<Vec<_>>())
            }),
        ),
    ];
    for (name, read) in &cases {
        let (frozen, source) = deferred();
        assert!(frozen.typed_pending(), "{name}: pending before the read");
        assert_eq!(read(&frozen), read(&eager), "{name}: deferred != eager");
        assert_eq!(source.jobs().len(), 1, "{name}: the read ran the job once");
    }
    // The next block's parent-layer reads.
    {
        let (frozen, source) = deferred();
        let (a, b) = (
            NativeStateOverlay::with_parent(db.clone(), Some(frozen)),
            NativeStateOverlay::with_parent(db.clone(), Some(eager.clone())),
        );
        for cf in [CF_NATIVE_POSITIONS, CF_NATIVE_BALANCES, CF_NATIVE_LIQUIDATION, CF_NATIVE_ORDERS] {
            assert_eq!(a.iterate_cf(cf, None).unwrap(), b.iterate_cf(cf, None).unwrap(), "{cf}: parent reads");
            for k in [pos_key(2, 2), pos_key(3, 1), pos_key(9, 9), vec![4; 20], vec![5; 20]] {
                assert_eq!(a.get_cf_raw(cf, &k).unwrap(), b.get_cf_raw(cf, &k).unwrap(), "{cf} {k:?}");
            }
        }
        assert_eq!(source.jobs().len(), 1, "parent reads ran the job once");
    }
    // The checkpoint layer merge (over an earlier block's layer).
    {
        let (frozen, source) = deferred();
        let base = {
            let ov = NativeStateOverlay::new(db.clone());
            ov.put_cf_raw(CF_NATIVE_POSITIONS, &pos_key(9, 9), b"older").unwrap();
            ov.put_cf_raw(CF_NATIVE_BALANCES, &[6; 20], b"older").unwrap();
            ov.freeze(0)
        };
        let merged = |f: &crate::backend::FrozenPending| {
            let mut layer = CheckpointLayer::new();
            layer.merge(&base);
            layer.merge(f);
            let keys = [pos_key(2, 2), pos_key(3, 1), pos_key(9, 9), vec![4; 20], vec![6; 20], vec![7; 3]];
            let rows: Vec<_> = [CF_NATIVE_POSITIONS, CF_NATIVE_BALANCES, CF_NATIVE_LIQUIDATION]
                .iter()
                .flat_map(|cf| keys.iter().map(move |k| (*cf, k.clone())))
                .map(|(cf, k)| layer.lookup_row(cf, &k))
                .collect();
            (layer.bytes(), rows)
        };
        assert_eq!(merged(&frozen), merged(&eager), "checkpoint layer merge");
        assert_eq!(source.jobs().len(), 1, "the merge ran the job once");
    }
    // The flush on two DBs (rows, the applied marker, the running hash).
    {
        let (frozen, source) = deferred();
        let flushed = |f: &crate::backend::FrozenPending| {
            let (db2, dir) = r2_db();
            crate::running_hash::configure_activation(&db2, Some(1)).unwrap();
            f.flush_on_worker(None, &db2, Some(1), None, None, None).unwrap();
            let out = (db_dump(&db2), crate::running_hash::read_running_hash(&db2));
            drop(dir);
            out
        };
        let (a, b) = (flushed(&frozen), flushed(&eager));
        assert!(a.1.is_some(), "the chain started at height 1");
        assert_eq!(a, b, "flush: DB rows and running hash");
        assert_eq!(source.jobs().len(), 1, "the flush ran the job once");
    }
}
