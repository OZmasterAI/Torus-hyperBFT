//! R02 branch 4: the margin releases Phase 2's balance cache shields from the
//! backend in a real block (the batch settle's taker release and
//! `sell_top_ups` read a sender whose balance Phase 2 always cached), driven
//! here with a fresh cache so the balance read reaches a failing backend. A
//! LOCAL fault must fail-stop (`take_fatal_error`), never skip the release /
//! top-up. Sequential and parallel settle are called directly (pinned).
//! Block-level tests: `tests/r02_margin_releases_tests.rs`.

use super::*;
use crate::market_workers::{MarketBatchResult, MatchResult};
use torus_state::cf::CF_NATIVE_BALANCES;
use torus_state::{AtomicWriteOp, StateError};

/// Point reads of `armed`'s balance row fail once armed; `hits` counts them.
#[derive(Clone)]
struct FailBal {
    inner: StateDb,
    armed: Arc<std::sync::Mutex<Option<Address>>>,
    hits: Arc<std::sync::Mutex<usize>>,
}

impl StateBackend for FailBal {
    fn get_cf_raw(&self, cf: &str, key: &[u8]) -> Result<Option<Vec<u8>>, StateError> {
        if cf == CF_NATIVE_BALANCES
            && self
                .armed
                .lock()
                .unwrap()
                .is_some_and(|a| a.as_slice() == key)
        {
            *self.hits.lock().unwrap() += 1;
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

const NOW: u64 = 1_001;
const TRADER: Address = Address::new([1; 20]);

fn fp(v: i64) -> FixedPoint {
    FixedPoint::from_raw(v as i128 * FixedPoint::SCALE)
}

/// A context over a failing backend; `TRADER` holds `margin` as order margin.
fn setup(margin: FixedPoint) -> (tempfile::TempDir, FailBal, NativeExecContext<FailBal>) {
    let dir = tempfile::tempdir().unwrap();
    let backend = FailBal {
        inner: StateDb::open(dir.path()).unwrap(),
        armed: Arc::default(),
        hits: Arc::default(),
    };
    let ctx = NativeExecContext::new(
        backend.clone(),
        2,
        NOW,
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
                available: fp(1_000_000) - margin,
                order_margin: margin,
            },
        )
        .unwrap();
    (dir, backend, ctx)
}

fn ioc_buy() -> PlaceOrderParams {
    PlaceOrderParams {
        market_id: 1,
        is_buy: true,
        price: fp(100),
        quantity: fp(1),
        order_type: OrderType::Limit,
        time_in_force: TimeInForce::IOC,
        reduce_only: false,
        client_order_id: None,
    }
}

/// A balance cache's rows: (trader, available, order margin, dirty), sorted.
type Rows = Vec<(Address, FixedPoint, FixedPoint, bool)>;

fn cache_rows(c: &BalanceCache) -> Rows {
    let mut v: Vec<_> = c
        .map
        .iter()
        .map(|(a, b)| (*a, b.balance.available, b.balance.order_margin, b.dirty))
        .collect();
    v.sort_unstable_by_key(|x| x.0);
    v
}

/// Settle one IOC buy that met an empty book (no fill, nothing rests: its
/// whole reservation is released) through the pinned loop, with a fresh
/// balance cache. Returns the result, the cache rows and the fail-stop.
fn settle_ioc(parallel: bool, fault: bool) -> (bool, Rows, Option<String>, usize) {
    let params = ioc_buy();
    let reserved = NativeExecutor::reserve_for_qty_cfg(None, params.price, params.quantity);
    assert!(reserved > FixedPoint::ZERO);
    let (_d, backend, mut ctx) = setup(reserved);
    let mut book = OrderBook::new(1, FixedPoint::ONE, FixedPoint::ONE);
    book.set_next_order_id(1);
    let result = book.place_order(params.clone(), TRADER, NOW);
    assert!(
        result.fills.is_empty() && result.rested_qty == FixedPoint::ZERO,
        "nothing fills or rests"
    );
    let next_order_id = book.next_order_id();
    let market_results = vec![MarketBatchResult {
        market_id: 1,
        book,
        results: vec![MatchResult {
            sender: TRADER,
            order_id: 1,
            result,
        }],
        next_order_id,
    }];
    let mut market_batches: HashMap<MarketId, Vec<PreparedOrder<'_>>> = HashMap::default();
    market_batches.insert(
        1,
        vec![PreparedOrder {
            index: 0,
            sender: TRADER,
            params: &params,
            order_id: 1,
            margin_reserved: reserved,
            checked_pos_net: None,
            pre_pos: None,
            top_up_candidate: false,
            res_price: params.price,
        }],
    );
    let mut results = vec![NativeActionResult::ok("pending", 0)];
    let mut gas = 0u64;
    let (mut bal_cache, mut pos_cache, mut vol_cache) = (
        BalanceCache::new(),
        PositionCache::new(),
        VolumeCache::default(),
    );
    if fault {
        *backend.armed.lock().unwrap() = Some(TRADER);
    }
    if parallel {
        NativeExecutor::settle_market_results_parallel(
            &mut ctx,
            market_results,
            &market_batches,
            &mut results,
            &mut gas,
            &mut bal_cache,
            &mut pos_cache,
            &mut vol_cache,
            1,
        );
        assert_eq!(
            ctx.phase_accum.settle_fallbacks, 0,
            "no fallback to sequential"
        );
    } else {
        NativeExecutor::settle_market_results_sequential(
            &mut ctx,
            market_results,
            &market_batches,
            &mut results,
            &mut gas,
            &mut bal_cache,
            &mut pos_cache,
            &mut vol_cache,
        );
    }
    let hits = *backend.hits.lock().unwrap();
    (
        results[0].success,
        cache_rows(&bal_cache),
        ctx.take_fatal_error(),
        hits,
    )
}

/// The batch settle's taker release: the sender's balance read fails. RED
/// before R02 (both loops): nothing released, no fail-stop.
#[test]
fn r02_settle_taker_release_read_fault_fail_stops() {
    for parallel in [false, true] {
        let (_ok, _rows, fatal, hits) = settle_ioc(parallel, true);
        assert!(hits > 0, "parallel {parallel}: fault injected");
        let reason = fatal.unwrap_or_else(|| panic!("parallel {parallel}: must fail-stop"));
        assert!(
            reason.starts_with("settle taker release balance read"),
            "parallel {parallel}: {reason}"
        );
        assert!(
            reason.contains("injected balance read failure"),
            "parallel {parallel}: {reason}"
        );
    }
}

/// Control: no fault, both loops release the whole reservation alike.
#[test]
fn r02_settle_taker_release_without_fault_unchanged() {
    let seq = settle_ioc(false, false);
    let par = settle_ioc(true, false);
    assert_eq!(seq.2, None);
    assert_eq!(
        seq.1,
        vec![(TRADER, fp(1_000_000), FixedPoint::ZERO, true)],
        "released in full"
    );
    assert_eq!(
        (seq.0, seq.1, seq.2),
        (par.0, par.1, par.2),
        "sequential == parallel"
    );
}

/// `sell_top_ups` with one candidate: a sell at 90 in market 1 (bid floor
/// 100) whose sender pools in market 2. Returns the counts, the cache rows,
/// the fail-stop and the injected hits.
fn top_up(fault: bool) -> ([u64; 3], Rows, Option<String>, usize) {
    let params = PlaceOrderParams {
        is_buy: false,
        price: fp(90),
        time_in_force: TimeInForce::GTC,
        ..ioc_buy()
    };
    let reserved = NativeExecutor::reserve_for_qty_cfg(None, params.price, params.quantity);
    let (_d, backend, mut ctx) = setup(reserved);
    let mut markets: HashMap<MarketId, Phase2Market<'_>> = HashMap::default();
    markets.insert(
        1,
        Phase2Market {
            shape: (FixedPoint::ONE, FixedPoint::ONE),
            bid_floor: Some(fp(100)),
            cfg: None,
            mark: None,
            band: None,
            orders: 1,
        },
    );
    let mut pools: HashMap<Address, (MarketId, FixedPoint)> = HashMap::default();
    pools.insert(TRADER, (2, FixedPoint::ZERO));
    let mut market_batches: HashMap<MarketId, Vec<PreparedOrder<'_>>> = HashMap::default();
    market_batches.insert(
        1,
        vec![PreparedOrder {
            index: 0,
            sender: TRADER,
            params: &params,
            order_id: 1,
            margin_reserved: reserved,
            checked_pos_net: None,
            pre_pos: None,
            top_up_candidate: true,
            res_price: params.price,
        }],
    );
    let mut bal_cache = BalanceCache::new();
    if fault {
        *backend.armed.lock().unwrap() = Some(TRADER);
    }
    let counts = NativeExecutor::sell_top_ups(
        &ctx.positions,
        &markets,
        &HashMap::default(),
        &pools,
        &HashMap::default(),
        &mut market_batches,
        &mut bal_cache,
    );
    let hits = *backend.hits.lock().unwrap();
    (counts, cache_rows(&bal_cache), ctx.take_fatal_error(), hits)
}

/// `sell_top_ups`: the sender's balance read fails. RED before R02: no
/// top-up, no fail-stop.
#[test]
fn r02_sell_top_up_read_fault_fail_stops() {
    let (counts, rows, fatal, hits) = top_up(true);
    assert!(hits > 0, "fault injected");
    assert_eq!(counts, [0, 0, 0], "no top-up meanwhile (unchanged)");
    assert!(rows.is_empty());
    let reason = fatal.expect("must fail-stop");
    assert!(reason.starts_with("sell top-up balance read"), "{reason}");
    assert!(reason.contains("injected balance read failure"), "{reason}");
}

/// Control: no fault, the candidate is topped up in full, as today.
#[test]
fn r02_sell_top_up_without_fault_unchanged() {
    let (counts, rows, fatal, hits) = top_up(false);
    assert_eq!((fatal, hits), (None, 0));
    assert_eq!(counts, [1, 0, 0], "topped up in full");
    assert_eq!(rows.len(), 1);
    assert!(
        rows[0].2 > NativeExecutor::reserve_for_qty_cfg(None, fp(90), fp(1)),
        "{rows:?}"
    );
}
