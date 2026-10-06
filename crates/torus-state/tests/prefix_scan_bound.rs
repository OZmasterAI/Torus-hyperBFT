//! s89 fix A: every RocksDB prefix scan is bounded above by the prefix
//! successor (`ReadOptions::set_iterate_upper_bound`).
//!
//! Without the bound RocksDB has no idea where the prefix ends (no prefix
//! extractor is configured), so after the last live key under the prefix it
//! keeps walking tombstones until it finds the next LIVE key — which the caller
//! then rejects with `starts_with`. On the crab branch `prune_submissions`
//! deletes all `sub‖market‖validator` oracle rows, and every per-market
//! `collect_submissions` scan walked the tombstones of all later markets
//! (~600 ms per native block at 300 markets x 3 reporters).
//!
//! * `prefix_scans_match_brute_force` — the results are byte-identical to a
//!   brute-force filter of a model map (random keys and deletes, keys right
//!   after the prefix, 0xff edge prefixes, tombstones in memtable and SST).
//! * `prefix_scan_skips_no_tombstones_past_prefix` — deterministic WORK test:
//!   RocksDB's perf context counts the tombstones an iterator skipped
//!   (`internal_delete_skipped_count`); a prefix scan must skip none of the
//!   tombstones that sit AFTER the prefix. A positive control proves the
//!   counter is live (tombstones INSIDE the prefix are still counted).

use std::collections::BTreeMap;

use rocksdb::perf::{set_perf_stats, PerfContext, PerfMetric, PerfStatsLevel};
use torus_state::cf::{CF_NATIVE_ORACLE, CF_STORAGE};
use torus_state::db::StateDb;
use torus_state::{NativeStateOverlay, StateBackend};

fn temp_db() -> (StateDb, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("create tempdir");
    let db = StateDb::open(dir.path()).expect("open db");
    (db, dir)
}

fn flush(db: &StateDb, cf: &str) {
    let h = db.cf_handle(cf).unwrap();
    db.inner().flush_cf(h).unwrap();
}

struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 11
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// Bytes that sit on every edge of a successor computation.
const ALPHABET: [u8; 6] = [0x00, 0x01, 0x7f, 0xfe, 0xff, 0x62];

fn random_key(rng: &mut Lcg, max_len: u64) -> Vec<u8> {
    let len = 1 + rng.below(max_len);
    (0..len)
        .map(|_| ALPHABET[rng.below(ALPHABET.len() as u64) as usize])
        .collect()
}

/// Every prefix of length 0..=2 over the alphabet, plus a few longer 0xff runs.
fn edge_prefixes() -> Vec<Vec<u8>> {
    let mut out = vec![vec![]];
    for &a in &ALPHABET {
        out.push(vec![a]);
        for &b in &ALPHABET {
            out.push(vec![a, b]);
        }
    }
    out.push(vec![0xff, 0xff, 0xff]);
    out.push(vec![0x01, 0xff, 0xff]);
    out.push(vec![0xfe, 0xff, 0xff, 0xff]);
    out
}

fn model_scan(model: &BTreeMap<Vec<u8>, Vec<u8>>, prefix: &[u8]) -> Vec<(Vec<u8>, Vec<u8>)> {
    model
        .iter()
        .filter(|(k, _)| k.starts_with(prefix))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

fn check_all_prefixes(
    db: &StateDb,
    overlay: &NativeStateOverlay,
    db_model: &BTreeMap<Vec<u8>, Vec<u8>>,
    ov_model: &BTreeMap<Vec<u8>, Vec<u8>>,
    round: usize,
) {
    for p in edge_prefixes() {
        let want = model_scan(db_model, &p);
        let got = StateBackend::iterate_cf(db, CF_NATIVE_ORACLE, Some(&p)).unwrap();
        assert_eq!(got, want, "round {round}: StateDb::iterate_cf prefix {p:02x?}");
        assert_eq!(
            StateBackend::prefix_exists(db, CF_NATIVE_ORACLE, &p).unwrap(),
            !want.is_empty(),
            "round {round}: StateDb::prefix_exists prefix {p:02x?}"
        );
        let want_ov = model_scan(ov_model, &p);
        let got_ov = overlay.iterate_cf(CF_NATIVE_ORACLE, Some(&p)).unwrap();
        assert_eq!(got_ov, want_ov, "round {round}: overlay iterate_cf prefix {p:02x?}");
        assert_eq!(
            overlay.prefix_exists(CF_NATIVE_ORACLE, &p).unwrap(),
            !want_ov.is_empty(),
            "round {round}: overlay prefix_exists prefix {p:02x?}"
        );
    }
}

#[test]
fn prefix_scans_match_brute_force() {
    let (db, _dir) = temp_db();
    let mut rng = Lcg(0x5EED_0089);
    let mut model: BTreeMap<Vec<u8>, Vec<u8>> = BTreeMap::new();
    for round in 0..12 {
        // Random puts and deletes (deletes hit live keys and never-written keys).
        for _ in 0..300 {
            let k = random_key(&mut rng, 5);
            if rng.below(3) == 0 {
                db.delete_cf_raw(CF_NATIVE_ORACLE, &k).unwrap();
                model.remove(&k);
            } else {
                let v = rng.next().to_be_bytes().to_vec();
                db.put_cf_raw(CF_NATIVE_ORACLE, &k, &v).unwrap();
                model.insert(k, v);
            }
        }
        // Delete whole runs right after a live prefix (the oracle-prune shape).
        let p = random_key(&mut rng, 2);
        let doomed: Vec<Vec<u8>> = model
            .keys()
            .filter(|k| k.as_slice() > p.as_slice() && !k.starts_with(&p))
            .take(40)
            .cloned()
            .collect();
        for k in doomed {
            db.delete_cf_raw(CF_NATIVE_ORACLE, &k).unwrap();
            model.remove(&k);
        }
        // Alternate tombstones in the memtable and in SST files.
        if round % 2 == 1 {
            flush(&db, CF_NATIVE_ORACLE);
        }
        // An overlay with its own pending writes and tombstones over the DB.
        let overlay = NativeStateOverlay::new(db.clone());
        let mut ov_model = model.clone();
        for _ in 0..40 {
            let k = random_key(&mut rng, 4);
            if rng.below(2) == 0 {
                overlay.delete_cf_raw(CF_NATIVE_ORACLE, &k).unwrap();
                ov_model.remove(&k);
            } else {
                overlay.put_cf_raw(CF_NATIVE_ORACLE, &k, b"ov").unwrap();
                ov_model.insert(k, b"ov".to_vec());
            }
        }
        check_all_prefixes(&db, &overlay, &model, &ov_model, round);
    }
}

/// `StateDb::account_storage` (20-byte address prefix) on adjacent and
/// all-0xff addresses, with the neighbouring account's slots deleted.
#[test]
fn account_storage_matches_brute_force_on_edge_addresses() {
    use alloy_primitives::{Address, U256};
    let (db, _dir) = temp_db();
    let addrs = [
        Address::repeat_byte(0x00),
        Address::with_last_byte(0x01),
        Address::with_last_byte(0x02),
        {
            let mut a = [0x01u8; 20];
            a[19] = 0xff;
            Address::new(a)
        },
        {
            let mut a = [0x01u8; 20];
            a[18] = 0x02;
            a[19] = 0x00;
            Address::new(a)
        },
        Address::repeat_byte(0xff),
    ];
    let mut model: BTreeMap<Address, BTreeMap<U256, U256>> = BTreeMap::new();
    for (i, a) in addrs.iter().enumerate() {
        for s in 0..20u64 {
            let mut key = a.as_slice().to_vec();
            key.extend_from_slice(&U256::from(s).to_be_bytes::<32>());
            let v = U256::from(i as u64 * 100 + s + 1);
            db.put_cf_raw(CF_STORAGE, &key, &v.to_be_bytes::<32>()).unwrap();
            model.entry(*a).or_default().insert(U256::from(s), v);
        }
    }
    flush(&db, CF_STORAGE);
    // Every other account loses all its slots (tombstones right after a live account).
    for a in addrs.iter().skip(1).step_by(2) {
        for s in 0..20u64 {
            let mut key = a.as_slice().to_vec();
            key.extend_from_slice(&U256::from(s).to_be_bytes::<32>());
            db.delete_cf_raw(CF_STORAGE, &key).unwrap();
        }
        model.remove(a);
    }
    for a in &addrs {
        let want: Vec<(U256, U256)> = model
            .get(a)
            .map(|m| m.iter().map(|(k, v)| (*k, *v)).collect())
            .unwrap_or_default();
        assert_eq!(db.account_storage(a).unwrap(), want, "address {a}");
    }
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

fn sub_key(market: u64, reporter: u8) -> Vec<u8> {
    let mut k = b"sub".to_vec();
    k.extend_from_slice(&market.to_be_bytes());
    k.push(reporter);
    k
}

fn sub_prefix(market: u64) -> Vec<u8> {
    let mut k = b"sub".to_vec();
    k.extend_from_slice(&market.to_be_bytes());
    k
}

#[test]
fn prefix_scan_skips_no_tombstones_past_prefix() {
    const MARKETS: u64 = 300;
    const REPORTERS: u8 = 3;
    let n_tomb = (MARKETS - 1) * REPORTERS as u64; // rows of markets 2..=MARKETS
    for flush_tombstones in [false, true] {
        let (db, _dir) = temp_db();
        for m in 1..=MARKETS {
            for r in 0..REPORTERS {
                db.put_cf_raw(CF_NATIVE_ORACLE, &sub_key(m, r), b"price").unwrap();
            }
        }
        flush(&db, CF_NATIVE_ORACLE);
        // Prune every market but the first (the rows right after its prefix).
        for m in 2..=MARKETS {
            for r in 0..REPORTERS {
                db.delete_cf_raw(CF_NATIVE_ORACLE, &sub_key(m, r)).unwrap();
            }
        }
        if flush_tombstones {
            flush(&db, CF_NATIVE_ORACLE);
        }
        let tag = if flush_tombstones { "sst" } else { "memtable" };

        // Positive control: tombstones INSIDE a scanned prefix are skipped and
        // counted (proves the perf counter is live on this thread). The last
        // market has nothing after it, so this holds with or without the bound.
        let inside = deletes_skipped(|| {
            assert!(StateBackend::iterate_cf(&db, CF_NATIVE_ORACLE, Some(&sub_prefix(MARKETS)))
                .unwrap()
                .is_empty());
        });
        assert_eq!(inside, REPORTERS as u64, "{tag}: control, the last market's own tombstones");

        // Market 1 still has live rows: the scan returns them and stops at the bound.
        let p1 = sub_prefix(1);
        let skipped = deletes_skipped(|| {
            let rows = StateBackend::iterate_cf(&db, CF_NATIVE_ORACLE, Some(&p1)).unwrap();
            assert_eq!(rows.len(), REPORTERS as usize);
        });
        assert_eq!(
            skipped, 0,
            "{tag}: StateDb::iterate_cf(market 1) walked {skipped} of the {n_tomb} tombstones after its prefix"
        );

        // Market 2 is fully pruned: existence checks must not walk markets 3.. either.
        let p2 = sub_prefix(2);
        let skipped = deletes_skipped(|| {
            assert!(!StateBackend::prefix_exists(&db, CF_NATIVE_ORACLE, &p2).unwrap());
        });
        assert_eq!(skipped, REPORTERS as u64, "{tag}: StateDb::prefix_exists(market 2)");

        let overlay = NativeStateOverlay::new(db.clone());
        let skipped = deletes_skipped(|| {
            assert!(!overlay.prefix_exists(CF_NATIVE_ORACLE, &p2).unwrap());
        });
        assert_eq!(skipped, REPORTERS as u64, "{tag}: NativeStateOverlay::prefix_exists(market 2)");
        let skipped = deletes_skipped(|| {
            let rows = overlay.iterate_cf(CF_NATIVE_ORACLE, Some(&p1)).unwrap();
            assert_eq!(rows.len(), REPORTERS as usize);
        });
        assert_eq!(skipped, 0, "{tag}: NativeStateOverlay::iterate_cf(market 1)");
    }
}

#[test]
fn account_storage_skips_no_tombstones_past_prefix() {
    use alloy_primitives::{Address, U256};
    let (db, _dir) = temp_db();
    let live = Address::with_last_byte(0x01);
    let mut key = live.as_slice().to_vec();
    key.extend_from_slice(&U256::from(7u64).to_be_bytes::<32>());
    db.put_cf_raw(CF_STORAGE, &key, &U256::from(9u64).to_be_bytes::<32>()).unwrap();
    // 500 slots of the NEXT address, written then deleted.
    let next = Address::with_last_byte(0x02);
    for s in 0..500u64 {
        let mut k = next.as_slice().to_vec();
        k.extend_from_slice(&U256::from(s).to_be_bytes::<32>());
        db.put_cf_raw(CF_STORAGE, &k, &[1u8; 32]).unwrap();
    }
    flush(&db, CF_STORAGE);
    for s in 0..500u64 {
        let mut k = next.as_slice().to_vec();
        k.extend_from_slice(&U256::from(s).to_be_bytes::<32>());
        db.delete_cf_raw(CF_STORAGE, &k).unwrap();
    }
    let skipped = deletes_skipped(|| {
        assert_eq!(
            db.account_storage(&live).unwrap(),
            vec![(U256::from(7u64), U256::from(9u64))]
        );
    });
    assert_eq!(skipped, 0, "account_storage walked {skipped} tombstones of the next address");
}

/// The EVM hashed-storage cursor (state-root path) is bounded to its account's
/// 32-byte prefix: seeking, stepping off the end and the emptiness probe skip
/// none of the next account's deleted slots.
#[test]
fn hashed_storage_cursor_skips_no_tombstones_past_prefix() {
    use alloy_primitives::{B256, U256};
    use reth_trie::hashed_cursor::{HashedCursor, HashedCursorFactory, HashedStorageCursor};
    use torus_state::cf::CF_HASHED_STORAGE;
    use torus_state::trie_cursor::RocksHashedCursorFactory;

    let (db, _dir) = temp_db();
    let live = B256::repeat_byte(0x11);
    let empty = B256::with_last_byte(0x05);
    let mut next = [0x11u8; 32];
    next[31] = 0x12;
    let mut after_empty = [0u8; 32];
    after_empty[31] = 0x06;
    let slot = |a: &[u8], s: u64| {
        let mut k = a.to_vec();
        k.extend_from_slice(&B256::from(U256::from(s)).0);
        k
    };
    db.put_cf_raw(CF_HASHED_STORAGE, &slot(live.as_slice(), 1), &[7u8; 32]).unwrap();
    for s in 0..500u64 {
        db.put_cf_raw(CF_HASHED_STORAGE, &slot(&next, s), &[1u8; 32]).unwrap();
        db.put_cf_raw(CF_HASHED_STORAGE, &slot(&after_empty, s), &[1u8; 32]).unwrap();
    }
    flush(&db, CF_HASHED_STORAGE);
    for s in 0..500u64 {
        db.delete_cf_raw(CF_HASHED_STORAGE, &slot(&next, s)).unwrap();
        db.delete_cf_raw(CF_HASHED_STORAGE, &slot(&after_empty, s)).unwrap();
    }

    let factory = RocksHashedCursorFactory::new(&db);
    let mut cur = factory.hashed_storage_cursor(live).unwrap();
    let skipped = deletes_skipped(|| {
        let first = cur.seek(B256::ZERO).unwrap().expect("live slot");
        assert_eq!(first.0, B256::from(U256::from(1u64)));
        assert!(cur.next().unwrap().is_none());
        assert!(!cur.is_storage_empty().unwrap());
    });
    assert_eq!(skipped, 0, "hashed storage cursor walked {skipped} tombstones of the next account");

    cur.set_hashed_address(empty);
    let skipped = deletes_skipped(|| {
        assert!(cur.is_storage_empty().unwrap());
        assert!(cur.seek(B256::ZERO).unwrap().is_none());
    });
    assert_eq!(skipped, 0, "emptiness probe walked {skipped} tombstones of the next account");
}
