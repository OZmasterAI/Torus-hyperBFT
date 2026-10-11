//! R02 branch 7: the applied-marker half of the resident staleness guards
//! (`begin_resident`'s rows slot and the constructor's books holder share
//! `resident_marker_ok`). A failed marker read means "stale" (rebuild),
//! never "no marker" (reuse). `begin_resident` takes a concrete
//! `NativeStateOverlay`, so the rule is pinned here on the shared helper;
//! the constructor's path is driven end to end in
//! `tests/r02_load_save_tests.rs`.
//!
//! Also the D2 pools' balance read (`NativeExecutor::d2_pools`), which a
//! real block never sends to the backend (Phase 2 cached the sender).

use super::*;
use torus_state::cf::{CF_CONSENSUS_META, META_NATIVE_APPLIED_HEIGHT};
use torus_state::{AtomicWriteOp, StateError};

/// Point reads of the applied marker fail when `fail` is set.
#[derive(Clone)]
struct FailMarker {
    inner: StateDb,
    fail: bool,
}

impl StateBackend for FailMarker {
    fn get_cf_raw(&self, cf: &str, key: &[u8]) -> Result<Option<Vec<u8>>, StateError> {
        if self.fail && cf == CF_CONSENSUS_META && key == META_NATIVE_APPLIED_HEIGHT {
            return Err(StateError::Io(std::io::Error::other(
                "injected marker read failure",
            )));
        }
        self.inner.get_cf_raw(cf, key)
    }
    fn put_cf_raw(&self, cf: &str, key: &[u8], value: &[u8]) -> Result<(), StateError> {
        self.inner.put_cf_raw(cf, key, value)
    }
    fn delete_cf_raw(&self, cf: &str, key: &[u8]) -> Result<(), StateError> {
        self.inner.delete_cf_raw(cf, key)
    }
    fn iterate_cf(
        &self,
        cf: &str,
        prefix: Option<&[u8]>,
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>, StateError> {
        self.inner.iterate_cf(cf, prefix)
    }
    fn atomic_write(&self, ops: &[AtomicWriteOp<'_>]) -> Result<(), StateError> {
        self.inner.atomic_write(ops)
    }
}

fn backend(marker: Option<&[u8]>, fail: bool) -> (tempfile::TempDir, FailMarker) {
    let dir = tempfile::tempdir().unwrap();
    let inner = StateDb::open(dir.path()).unwrap();
    if let Some(m) = marker {
        inner
            .put_cf_raw(CF_CONSENSUS_META, META_NATIVE_APPLIED_HEIGHT, m)
            .unwrap();
    }
    (dir, FailMarker { inner, fail })
}

/// A failed marker read is the `Err` and fails the guard (rebuild).
#[test]
fn r02_applied_marker_read_fault_is_stale() {
    let (_d, b) = backend(Some(&5u64.to_be_bytes()), true);
    let marker = applied_marker(&b);
    assert!(marker.is_err(), "{marker:?}");
    assert!(
        !resident_marker_ok(&marker, 5),
        "a fault never passes the guard"
    );
}

/// Unchanged: no row and a wrong-length row read as absent (the guard
/// passes on the height check alone); a row equal to the held height
/// passes, any other fails.
#[test]
fn r02_applied_marker_without_fault_unchanged() {
    let (_d, none) = backend(None, false);
    assert_eq!(applied_marker(&none).unwrap(), None);
    assert!(resident_marker_ok(&applied_marker(&none), 5));

    let (_d, short) = backend(Some(b"short"), false);
    assert_eq!(
        applied_marker(&short).unwrap(),
        None,
        "wrong length: absent"
    );
    assert!(resident_marker_ok(&applied_marker(&short), 5));

    let (_d, five) = backend(Some(&5u64.to_be_bytes()), false);
    assert_eq!(applied_marker(&five).unwrap(), Some(5));
    assert!(resident_marker_ok(&applied_marker(&five), 5));
    assert!(!resident_marker_ok(&applied_marker(&five), 4));
}

// ---------------------------------------------------------------------------
// D2 pools: the pooled sender's balance read (Phase 2 always caches it in a
// real block, so a fresh cache drives the read to the failing backend).
// ---------------------------------------------------------------------------

/// Point reads of `armed`'s balance row fail once armed.
#[derive(Clone)]
struct FailBal {
    inner: StateDb,
    armed: Arc<std::sync::Mutex<Option<Address>>>,
}

impl StateBackend for FailBal {
    fn get_cf_raw(&self, cf: &str, key: &[u8]) -> Result<Option<Vec<u8>>, StateError> {
        if cf == torus_state::cf::CF_NATIVE_BALANCES
            && self
                .armed
                .lock()
                .unwrap()
                .is_some_and(|a| a.as_slice() == key)
        {
            return Err(StateError::Io(std::io::Error::other(
                "injected balance read failure",
            )));
        }
        self.inner.get_cf_raw(cf, key)
    }
    fn put_cf_raw(&self, cf: &str, key: &[u8], value: &[u8]) -> Result<(), StateError> {
        self.inner.put_cf_raw(cf, key, value)
    }
    fn delete_cf_raw(&self, cf: &str, key: &[u8]) -> Result<(), StateError> {
        self.inner.delete_cf_raw(cf, key)
    }
    fn iterate_cf(
        &self,
        cf: &str,
        prefix: Option<&[u8]>,
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>, StateError> {
        self.inner.iterate_cf(cf, prefix)
    }
    fn atomic_write(&self, ops: &[AtomicWriteOp<'_>]) -> Result<(), StateError> {
        self.inner.atomic_write(ops)
    }
}

const TRADER: Address = Address::new([1; 20]);

fn fp(v: i64) -> FixedPoint {
    FixedPoint::from_raw(v as i128 * FixedPoint::SCALE)
}

/// Pools by `(sender, market)`, sorted.
type Pools = Vec<((Address, MarketId), FixedPoint)>;

/// `d2_pools` for `TRADER` (pool market 1, pos_net 5, excess 2, available
/// 100) with a fresh balance cache. Returns the pools and the fail-stop.
fn pools(fault: bool) -> (Pools, Option<String>) {
    let dir = tempfile::tempdir().unwrap();
    let backend = FailBal {
        inner: StateDb::open(dir.path()).unwrap(),
        armed: Arc::default(),
    };
    let mut ctx = NativeExecContext::new(
        backend.clone(),
        2,
        1_001,
        0,
        1_000,
        10,
        Address::ZERO,
        Address::ZERO,
        Address::ZERO,
    );
    ctx.positions
        .put_native_balance(
            &TRADER,
            &NativeBalance {
                available: fp(100),
                order_margin: FixedPoint::ZERO,
            },
        )
        .unwrap();
    let mut takers: HashMap<Address, (MarketId, FixedPoint)> = HashMap::default();
    takers.insert(TRADER, (1, fp(5)));
    let mut excess: HashMap<Address, FixedPoint> = HashMap::default();
    excess.insert(TRADER, fp(2));
    if fault {
        *backend.armed.lock().unwrap() = Some(TRADER);
    }
    let mut cache = BalanceCache::new();
    let out = NativeExecutor::d2_pools(
        &ctx.positions,
        &ctx.reader_fault,
        &takers,
        &excess,
        &mut cache,
    );
    let mut out: Vec<_> = out.into_iter().collect();
    out.sort_unstable_by_key(|x| x.0);
    (out, ctx.take_fatal_error())
}

/// The pooled sender's balance read fails. RED before R02: its pool read
/// as `0 + pos_net - excess`, no fail-stop.
#[test]
fn r02_d2_pool_balance_read_fault_fail_stops() {
    let (_pools, fatal) = pools(true);
    let reason = fatal.expect("must fail-stop");
    assert!(reason.starts_with("D2 pool balance read"), "{reason}");
    assert!(reason.contains("injected balance read failure"), "{reason}");
}

/// Control: `available + pos_net - excess`, as today.
#[test]
fn r02_d2_pool_without_fault_unchanged() {
    let (pools, fatal) = pools(false);
    assert_eq!(fatal, None);
    assert_eq!(pools, vec![((TRADER, 1), fp(103))]);
}
