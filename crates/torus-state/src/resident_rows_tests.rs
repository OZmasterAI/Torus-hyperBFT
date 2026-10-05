//! Item 6 Phase 1 (C1) tests: resident rows R (`build`, `apply`) and the
//! overlay reading R's two CFs from R in place of the DB.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use super::{ResidentRows, RESIDENT_CFS};
use crate::backend::{NativeStateOverlay, StateBackend};
use crate::cf::{CF_NATIVE_BALANCES, CF_NATIVE_LIQUIDATION, CF_NATIVE_ORDERS, CF_NATIVE_POSITIONS};
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

/// Item 6 E2-E4: R's column families — positions and balances (C1) and the
/// liquidation rows (E2: cooldown / pending / previous marks / cursor).
#[test]
fn resident_cfs_are_the_native_hot_cfs() {
    assert_eq!(RESIDENT_CFS.to_vec(), vec![CF_NATIVE_POSITIONS, CF_NATIVE_BALANCES, CF_NATIVE_LIQUIDATION]);
}
