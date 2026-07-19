//! C1 — PositionCache: write-back cache for Position rows.
//!
//! The cache mirrors the executor's BalanceCache pattern: position
//! read-modify-writes during a batch hit an in-memory map, and dirty entries
//! flush to the backend once at batch end in deterministic (sorted-key) order.
//!
//! The key correctness bar: a fill sequence applied through
//! `apply_fill_cached` + a caller-side PnL credit must produce byte-identical
//! CF_NATIVE_POSITIONS / CF_NATIVE_BALANCES contents vs. the same sequence
//! applied through the classic uncached `apply_fill`.

use std::sync::{Arc, Mutex};

use torus_core::position::{
    position_key, MarginType, NativeBalance, PositionCache, PositionManager,
};
use torus_state::cf::{CF_NATIVE_BALANCES, CF_NATIVE_POSITIONS};
use torus_state::{AtomicWriteOp, StateBackend, StateDb, StateError};
use torus_types::{Address, FixedPoint, MarketId};

// ---- Helpers ----

fn setup() -> (tempfile::TempDir, PositionManager) {
    let dir = tempfile::tempdir().unwrap();
    let db = StateDb::open(dir.path()).unwrap();
    (dir, PositionManager::new(db))
}

fn addr(n: u8) -> Address {
    Address::new([n; 20])
}

fn fp(v: i64) -> FixedPoint {
    FixedPoint::from_raw(v as i128 * FixedPoint::SCALE)
}

/// One fill: (trader, market, is_buy, qty, price).
type FillSpec = (u8, MarketId, bool, i64, i64);

/// Reference path: the classic per-fill overlay read-modify-write.
fn run_reference(pm: &PositionManager, fills: &[FillSpec]) {
    for &(t, m, is_buy, qty, price) in fills {
        pm.apply_fill(&addr(t), m, is_buy, fp(qty), fp(price), MarginType::Cross)
            .unwrap();
    }
}

/// Cached path: positions through the PositionCache, realized-PnL events
/// credited by the caller (as the executor does through its BalanceCache),
/// single flush at the end.
fn run_cached(pm: &PositionManager, fills: &[FillSpec]) {
    let mut cache = PositionCache::new();
    for &(t, m, is_buy, qty, price) in fills {
        let trader = addr(t);
        if let Some(pnl) = pm
            .apply_fill_cached(&mut cache, &trader, m, is_buy, fp(qty), fp(price), MarginType::Cross)
            .unwrap()
        {
            let mut bal = pm.get_native_balance(&trader).unwrap();
            bal.available += pnl;
            pm.put_native_balance(&trader, &bal).unwrap();
        }
    }
    cache.flush_all(pm).unwrap();
}

/// Full raw contents of a CF, for byte-identical comparison.
fn dump_cf(pm: &PositionManager, cf: &str) -> Vec<(Vec<u8>, Vec<u8>)> {
    pm.state().iterate_cf(cf, None).unwrap()
}

fn assert_state_identical(a: &PositionManager, b: &PositionManager) {
    assert_eq!(
        dump_cf(a, CF_NATIVE_POSITIONS),
        dump_cf(b, CF_NATIVE_POSITIONS),
        "CF_NATIVE_POSITIONS diverged between cached and reference paths"
    );
    assert_eq!(
        dump_cf(a, CF_NATIVE_BALANCES),
        dump_cf(b, CF_NATIVE_BALANCES),
        "CF_NATIVE_BALANCES diverged between cached and reference paths"
    );
}

// ============================================================================
// Differential: cached path == classic path, all transition branches
// ============================================================================

#[test]
fn cached_fills_match_reference_all_branches() {
    // Covers: open, increase (weighted entry), partial close (PnL credit),
    // full close (delete + PnL), reopen, flip (close + open opposite),
    // multiple traders and markets interleaved.
    let fills: Vec<FillSpec> = vec![
        (1, 1, true, 5, 100),   // A m1: open long 5 @ 100
        (2, 1, false, 3, 100),  // B m1: open short 3 @ 100
        (1, 1, true, 3, 110),   // A m1: increase -> 8 @ weighted
        (1, 1, false, 2, 120),  // A m1: partial close (PnL +)
        (2, 1, false, 1, 105),  // B m1: increase short
        (1, 1, false, 6, 90),   // A m1: full close (PnL -)
        (1, 2, false, 4, 200),  // A m2: open short
        (1, 2, true, 10, 195),  // A m2: flip short 4 -> long 6 (PnL +)
        (3, 2, true, 7, 210),   // C m2: open long
        (2, 1, true, 4, 95),    // B m1: full close short (PnL +)
        (3, 2, false, 7, 210),  // C m2: full close, zero PnL (still touches balance row)
    ];

    let (_d1, pm_ref) = setup();
    let (_d2, pm_cached) = setup();

    // Seed identical starting balances so PnL credits land on real rows.
    for t in [1u8, 2, 3] {
        for pm in [&pm_ref, &pm_cached] {
            pm.put_native_balance(
                &addr(t),
                &NativeBalance {
                    available: fp(10_000),
                    order_margin: FixedPoint::ZERO,
                },
            )
            .unwrap();
        }
    }

    run_reference(&pm_ref, &fills);
    run_cached(&pm_cached, &fills);

    assert_state_identical(&pm_cached, &pm_ref);
}

#[test]
fn cached_partial_fills_match_reference() {
    // Repeated partial closes down to a sliver, never fully closing.
    let fills: Vec<FillSpec> = vec![
        (1, 7, true, 10, 50),
        (1, 7, false, 3, 55),
        (1, 7, false, 3, 45),
        (1, 7, false, 3, 50),
    ];
    let (_d1, pm_ref) = setup();
    let (_d2, pm_cached) = setup();
    run_reference(&pm_ref, &fills);
    run_cached(&pm_cached, &fills);
    assert_state_identical(&pm_cached, &pm_ref);

    // The sliver survives with the right size.
    let pos = pm_cached.get_position(&addr(1), 7).unwrap().unwrap();
    assert_eq!(pos.size, fp(1));
    assert!(pos.is_long);
}

// ============================================================================
// Cache semantics
// ============================================================================

#[test]
fn cache_is_read_your_writes_and_defers_backend_writes() {
    let (_d, pm) = setup();
    let trader = addr(1);
    let mut cache = PositionCache::new();

    // Open through the cache: backend must NOT see it before flush.
    let pnl = pm
        .apply_fill_cached(&mut cache, &trader, 1, true, fp(5), fp(100), MarginType::Cross)
        .unwrap();
    assert!(pnl.is_none(), "open has no PnL event");
    assert!(
        pm.get_position(&trader, 1).unwrap().is_none(),
        "backend saw a write before flush"
    );

    // Second fill must read the cached position (read-your-writes).
    pm.apply_fill_cached(&mut cache, &trader, 1, true, fp(3), fp(110), MarginType::Cross)
        .unwrap();

    cache.flush_all(&pm).unwrap();
    let pos = pm.get_position(&trader, 1).unwrap().unwrap();
    assert_eq!(pos.size, fp(8), "increase applied on cached state");
}

#[test]
fn full_close_within_batch_flushes_tombstone() {
    let (_d, pm) = setup();
    let trader = addr(1);

    // Pre-existing position in the backend.
    pm.apply_fill(&trader, 3, true, fp(4), fp(100), MarginType::Cross)
        .unwrap();

    let mut cache = PositionCache::new();
    let pnl = pm
        .apply_fill_cached(&mut cache, &trader, 3, false, fp(4), fp(100), MarginType::Cross)
        .unwrap();
    assert_eq!(pnl, Some(FixedPoint::ZERO), "flat close still emits a PnL event");

    // Backend still holds the row until flush.
    assert!(pm.get_position(&trader, 3).unwrap().is_some());
    cache.flush_all(&pm).unwrap();
    assert!(pm.get_position(&trader, 3).unwrap().is_none());
}

#[test]
fn open_then_close_within_batch_leaves_no_row() {
    let (_d, pm) = setup();
    let trader = addr(9);
    let mut cache = PositionCache::new();

    pm.apply_fill_cached(&mut cache, &trader, 5, true, fp(2), fp(100), MarginType::Cross)
        .unwrap();
    pm.apply_fill_cached(&mut cache, &trader, 5, false, fp(2), fp(101), MarginType::Cross)
        .unwrap();
    cache.flush_all(&pm).unwrap();

    assert!(pm.get_position(&trader, 5).unwrap().is_none());
    assert!(dump_cf(&pm, CF_NATIVE_POSITIONS).is_empty());
}

// ============================================================================
// Deterministic flush order
// ============================================================================

/// StateBackend wrapper that records every put/delete key on the positions CF.
#[derive(Clone)]
struct RecordingBackend {
    inner: StateDb,
    ops: Arc<Mutex<Vec<Vec<u8>>>>,
}

impl StateBackend for RecordingBackend {
    fn get_cf_raw(&self, cf: &str, key: &[u8]) -> Result<Option<Vec<u8>>, StateError> {
        self.inner.get_cf_raw(cf, key)
    }
    fn put_cf_raw(&self, cf: &str, key: &[u8], value: &[u8]) -> Result<(), StateError> {
        if cf == CF_NATIVE_POSITIONS {
            self.ops.lock().unwrap().push(key.to_vec());
        }
        self.inner.put_cf_raw(cf, key, value)
    }
    fn delete_cf_raw(&self, cf: &str, key: &[u8]) -> Result<(), StateError> {
        if cf == CF_NATIVE_POSITIONS {
            self.ops.lock().unwrap().push(key.to_vec());
        }
        self.inner.delete_cf_raw(cf, key)
    }
    fn iterate_cf(
        &self,
        cf: &str,
        prefix: Option<&[u8]>,
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>, StateError> {
        StateBackend::iterate_cf(&self.inner, cf, prefix)
    }
    fn atomic_write(&self, ops: &[AtomicWriteOp<'_>]) -> Result<(), StateError> {
        self.inner.atomic_write(ops)
    }
}

#[test]
fn flush_writes_in_sorted_key_order() {
    let dir = tempfile::tempdir().unwrap();
    let db = StateDb::open(dir.path()).unwrap();
    let ops = Arc::new(Mutex::new(Vec::new()));
    let pm = PositionManager::new(RecordingBackend {
        inner: db,
        ops: ops.clone(),
    });

    let mut cache = PositionCache::new();
    // Touch keys in deliberately unsorted order across traders and markets.
    for &(t, m) in &[(9u8, 2u64), (1, 5), (9, 1), (1, 2), (5, 9)] {
        pm.apply_fill_cached(&mut cache, &addr(t), m, true, fp(1), fp(100), MarginType::Cross)
            .unwrap();
    }
    cache.flush_all(&pm).unwrap();

    let recorded = ops.lock().unwrap().clone();
    assert_eq!(recorded.len(), 5, "one write per dirty key");
    let expected: Vec<Vec<u8>> = {
        let mut keys: Vec<Vec<u8>> = [(1u8, 2u64), (1, 5), (5, 9), (9, 1), (9, 2)]
            .iter()
            .map(|&(t, m)| position_key(&addr(t), m).to_vec())
            .collect();
        keys.sort();
        keys
    };
    assert_eq!(recorded, expected, "flush order must be sorted by position key");

    // Flushing twice must not re-write clean entries.
    cache.flush_all(&pm).unwrap();
    assert_eq!(ops.lock().unwrap().len(), 5, "second flush re-wrote clean entries");
}
