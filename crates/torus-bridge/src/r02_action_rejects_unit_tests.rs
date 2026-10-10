//! R02 branch 6a: the batch settle's TAKER fill arm. In a real block Phase 2
//! has read the sender's position (backend) and cached its balance, so a
//! backend fault never reaches the settle's taker side through the public
//! API; driven here with fresh caches so the PnL credit's balance read hits
//! a failing backend. A LOCAL fault must fail-stop (`take_fatal_error`), the
//! same step in the sequential loop and in parallel pass B (no fallback);
//! the result keeps today's "taker fill failed". Block-level tests:
//! `tests/r02_action_rejects_tests.rs`.

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
const TAKER: Address = Address::new([1; 20]);
const MAKER: Address = Address::new([2; 20]);

fn fp(v: i64) -> FixedPoint {
    FixedPoint::from_raw(v as i128 * FixedPoint::SCALE)
}

fn limit(is_buy: bool, price: i64, tif: TimeInForce) -> PlaceOrderParams {
    PlaceOrderParams {
        market_id: 1,
        is_buy,
        price: fp(price),
        quantity: fp(1),
        order_type: OrderType::Limit,
        time_in_force: tif,
        reduce_only: false,
        client_order_id: None,
    }
}

/// `TAKER` long 1 @ 100 in market 1 sells it (IOC, nothing reserved) into
/// `MAKER`'s bid at 101 (realized PnL +1 for the taker), settled by the
/// pinned loop with fresh caches. Returns the result, the fail-stop, the
/// injected hits and the fallbacks.
fn settle_closing_taker(parallel: bool, fault: bool) -> (NativeActionResult, Option<String>, usize, u64) {
    let dir = tempfile::tempdir().unwrap();
    let backend = FailBal {
        inner: StateDb::open(dir.path()).unwrap(),
        armed: Arc::default(),
        hits: Arc::default(),
    };
    let mut ctx = NativeExecContext::new(
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
    for t in [TAKER, MAKER] {
        ctx.positions
            .put_native_balance(
                &t,
                &NativeBalance {
                    available: fp(1_000_000),
                    order_margin: FixedPoint::ZERO,
                },
            )
            .unwrap();
    }
    ctx.positions
        .apply_fill(&TAKER, 1, true, fp(1), fp(100), MarginType::Cross)
        .unwrap();
    let mut book = OrderBook::new(1, FixedPoint::ONE, FixedPoint::ONE);
    book.set_next_order_id(1);
    book.place_order(limit(true, 101, TimeInForce::GTC), MAKER, NOW);
    let params = limit(false, 101, TimeInForce::IOC);
    let result = book.place_order(params.clone(), TAKER, NOW);
    assert_eq!(result.fills.len(), 1, "the IOC sell fills");
    let next_order_id = book.next_order_id();
    let market_results = vec![MarketBatchResult {
        market_id: 1,
        book,
        results: vec![MatchResult {
            sender: TAKER,
            order_id: 2,
            result,
        }],
        next_order_id,
    }];
    let mut market_batches: HashMap<MarketId, Vec<PreparedOrder<'_>>> = HashMap::default();
    market_batches.insert(
        1,
        vec![PreparedOrder {
            index: 0,
            sender: TAKER,
            params: &params,
            order_id: 2,
            margin_reserved: FixedPoint::ZERO,
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
        *backend.armed.lock().unwrap() = Some(TAKER);
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
    let fallbacks = ctx.phase_accum.settle_fallbacks;
    (results.remove(0), ctx.take_fatal_error(), hits, fallbacks)
}

/// The taker's PnL credit reads its balance and the read fails. RED before
/// R02 (both loops): "taker fill failed", no fail-stop.
#[test]
fn r02_settle_taker_fill_balance_read_fault_fail_stops() {
    for parallel in [false, true] {
        let (r, fatal, hits, fallbacks) = settle_closing_taker(parallel, true);
        assert!(hits > 0, "parallel {parallel}: fault injected");
        assert_eq!(fallbacks, 0, "parallel {parallel}: pass B handles it");
        assert!(
            r.error.as_deref().is_some_and(|e| e.starts_with("taker fill failed")),
            "parallel {parallel}: result shape kept: {r:?}"
        );
        let reason = fatal.unwrap_or_else(|| panic!("parallel {parallel}: must fail-stop"));
        assert!(
            reason.starts_with("settle taker fill: "),
            "parallel {parallel}: {reason}"
        );
        assert!(
            reason.contains("injected balance read failure"),
            "parallel {parallel}: {reason}"
        );
    }
}

/// Control: no fault, both loops settle the closing fill alike.
#[test]
fn r02_settle_taker_fill_without_fault_unchanged() {
    let seq = settle_closing_taker(false, false);
    let par = settle_closing_taker(true, false);
    assert!(seq.0.success, "{:?}", seq.0);
    assert_eq!((seq.1.clone(), seq.2), (None, 0));
    assert_eq!(
        (seq.0.success, seq.0.error.clone(), seq.1),
        (par.0.success, par.0.error.clone(), par.1),
        "sequential == parallel"
    );
}
