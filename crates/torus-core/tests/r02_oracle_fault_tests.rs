//! R02 (audit main-chain-deep-review-2026-10-08, option A, branch 3): an
//! oracle aggregate read that fails on THIS node (a storage error, or a
//! stored row that does not decode) is a local fault and must surface as
//! an `Err` that `CoreError::is_local_fault` accepts, never be read as "no
//! price". Legitimate absence (no row) and staleness keep today's results.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use torus_core::error::CoreError;
use torus_core::oracle::{OracleConfig, OracleManager};
use torus_state::cf::CF_NATIVE_ORACLE;
use torus_state::{AtomicWriteOp, StateBackend, StateDb, StateError};
use torus_types::{Address, FixedPoint};

/// A backend whose point reads of the oracle aggregate rows (`agg‖market`)
/// fail while `armed` (an injected read fault). Everything else delegates.
#[derive(Clone)]
struct FailingAggRead {
    inner: StateDb,
    armed: Arc<AtomicBool>,
}

impl StateBackend for FailingAggRead {
    fn get_cf_raw(&self, cf: &str, key: &[u8]) -> Result<Option<Vec<u8>>, StateError> {
        if self.armed.load(Ordering::SeqCst) && cf == CF_NATIVE_ORACLE && key.starts_with(b"agg") {
            return Err(StateError::Io(std::io::Error::other("injected aggregate read failure")));
        }
        self.inner.get_cf_raw(cf, key)
    }
    fn put_cf_raw(&self, cf: &str, key: &[u8], value: &[u8]) -> Result<(), StateError> {
        self.inner.put_cf_raw(cf, key, value)
    }
    fn delete_cf_raw(&self, cf: &str, key: &[u8]) -> Result<(), StateError> {
        self.inner.delete_cf_raw(cf, key)
    }
    fn iterate_cf(&self, cf: &str, prefix: Option<&[u8]>) -> Result<Vec<(Vec<u8>, Vec<u8>)>, StateError> {
        self.inner.iterate_cf(cf, prefix)
    }
    fn atomic_write(&self, ops: &[AtomicWriteOp<'_>]) -> Result<(), StateError> {
        self.inner.atomic_write(ops)
    }
}

fn fp(v: i64) -> FixedPoint {
    FixedPoint::from_raw(v as i128 * FixedPoint::SCALE)
}

fn addr(n: u8) -> Address {
    Address::new([n; 20])
}

fn agg_key(m: u64) -> Vec<u8> {
    [b"agg".as_slice(), &m.to_be_bytes()].concat()
}

/// One reporter holding all the stake (a single-reporter aggregate passes
/// the quorum) on a fresh DB, behind the fault switch.
fn single_reporter() -> (tempfile::TempDir, StateDb, Arc<AtomicBool>, OracleManager<FailingAggRead>) {
    let dir = tempfile::tempdir().unwrap();
    let db = StateDb::open(dir.path()).unwrap();
    let armed = Arc::new(AtomicBool::new(false));
    let backend = FailingAggRead { inner: db.clone(), armed: armed.clone() };
    let mgr = OracleManager::new(backend, OracleConfig { min_oracle_reporters: 1, ..Default::default() });
    (dir, db, armed, mgr)
}

fn stakes() -> Vec<(Address, FixedPoint)> {
    vec![(addr(1), fp(100))]
}

/// Block 1 (ts 1000) aggregates 100; block 2 (ts 1001) has one report of `p`.
fn seed_last_100_then_report(mgr: &OracleManager<FailingAggRead>, p: i64) {
    mgr.submit_price(&addr(1), 1, fp(100), 1, 1_000).unwrap();
    assert_eq!(mgr.aggregate_price(1, 1, 1_000, &stakes()).unwrap(), fp(100));
    mgr.submit_price(&addr(1), 1, fp(p), 2, 1_001).unwrap();
}

/// `get_price_opt`: no row is `Ok(None)` (absence), a row is `Ok(Some)`, a
/// failed read and an undecodable row are local-fault `Err`s.
#[test]
fn r02_get_price_opt_splits_absence_from_local_faults() {
    let (_d, db, armed, mgr) = single_reporter();
    assert!(mgr.get_price_opt(1, 1_000).unwrap().is_none(), "no row: absence");
    assert!(matches!(mgr.get_price(1, 1_000), Err(CoreError::NoOraclePrice(1))), "get_price unchanged");
    seed_last_100_then_report(&mgr, 100);
    let p = mgr.get_price_opt(1, 1_000).unwrap().expect("row present");
    assert_eq!(p.price, fp(100));
    let stale = mgr.get_price_opt(1, 1_000 + 61).unwrap().expect("a stale row is still a row");
    assert!(stale.stale && stale.usable().is_none());

    armed.store(true, Ordering::SeqCst);
    let e = mgr.get_price_opt(1, 1_000).expect_err("read failed");
    assert!(matches!(e, CoreError::State(StateError::Io(_))) && e.is_local_fault(), "{e}");
    armed.store(false, Ordering::SeqCst);

    db.put_cf_raw(CF_NATIVE_ORACLE, &agg_key(1), b"garbage").unwrap();
    let e = mgr.get_price_opt(1, 1_000).expect_err("undecodable row");
    assert!(matches!(e, CoreError::Borsh(_)) && e.is_local_fault(), "{e}");
}

/// FIX 19's 10% single-reporter cap reads the last valid aggregate. A failed
/// read must not skip the cap: `aggregate_price` returns the local fault and
/// writes nothing. RED before R02: `if let Ok` read the fault as "no last
/// price", so a 100 -> 200 jump (over the cap) was written as the new price.
#[test]
fn r02_single_reporter_cap_read_fault_propagates() {
    for p in [200, 105] {
        let (_d, db, armed, mgr) = single_reporter();
        seed_last_100_then_report(&mgr, p);
        let before = db.get_cf_raw(CF_NATIVE_ORACLE, &agg_key(1)).unwrap();
        armed.store(true, Ordering::SeqCst);
        let e = mgr.aggregate_price(1, 2, 1_001, &stakes()).expect_err("the cap's read failed");
        assert!(e.is_local_fault(), "report {p}: {e}");
        assert_eq!(db.get_cf_raw(CF_NATIVE_ORACLE, &agg_key(1)).unwrap(), before, "report {p}: nothing written");
    }
}

/// The same for a stored last aggregate that does not decode (corrupt on
/// this node): a local fault, not "no last price". RED before R02.
#[test]
fn r02_single_reporter_cap_undecodable_last_aggregate_propagates() {
    let (_d, db, _armed, mgr) = single_reporter();
    seed_last_100_then_report(&mgr, 200);
    db.put_cf_raw(CF_NATIVE_ORACLE, &agg_key(1), b"garbage").unwrap();
    let e = mgr.aggregate_price(1, 2, 1_001, &stakes()).expect_err("undecodable last aggregate");
    assert!(matches!(e, CoreError::Borsh(_)), "{e}");
    assert_eq!(db.get_cf_raw(CF_NATIVE_ORACLE, &agg_key(1)).unwrap().as_deref(), Some(b"garbage".as_slice()));
}

/// Regression: legitimate absence and staleness still skip the cap exactly
/// as before (the single report becomes the price), and a fresh last price
/// still caps (over 10%: the last price, nothing written; within: written).
#[test]
fn r02_single_reporter_cap_absence_and_staleness_unchanged() {
    // No last aggregate: the single report is the price.
    let (_d, _db, _armed, mgr) = single_reporter();
    mgr.submit_price(&addr(1), 1, fp(200), 2, 1_001).unwrap();
    assert_eq!(mgr.aggregate_price(1, 2, 1_001, &stakes()).unwrap(), fp(200));
    assert_eq!(mgr.get_price(1, 1_001).unwrap().price, fp(200));

    // A stale last aggregate (100 at ts 1000, now 1000 + 61): cap skipped.
    let (_d, _db, _armed, mgr) = single_reporter();
    mgr.submit_price(&addr(1), 1, fp(100), 1, 1_000).unwrap();
    mgr.aggregate_price(1, 1, 1_000, &stakes()).unwrap();
    mgr.submit_price(&addr(1), 1, fp(200), 2, 1_061).unwrap();
    assert_eq!(mgr.aggregate_price(1, 2, 1_061, &stakes()).unwrap(), fp(200));

    // A fresh last price caps.
    let (_d, _db, _armed, mgr) = single_reporter();
    seed_last_100_then_report(&mgr, 200);
    assert_eq!(mgr.aggregate_price(1, 2, 1_001, &stakes()).unwrap(), fp(100));
    assert_eq!(mgr.get_price(1, 1_001).unwrap().block_number, 1, "nothing written");
    let (_d, _db, _armed, mgr) = single_reporter();
    seed_last_100_then_report(&mgr, 105);
    assert_eq!(mgr.aggregate_price(1, 2, 1_001, &stakes()).unwrap(), fp(105));
}
