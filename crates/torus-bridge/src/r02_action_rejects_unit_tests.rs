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
fn settle_closing_taker(
    parallel: bool,
    fault: bool,
) -> (NativeActionResult, Option<String>, usize, u64) {
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
            r.error
                .as_deref()
                .is_some_and(|e| e.starts_with("taker fill failed")),
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

// ---------------------------------------------------------------------------
// Codex P2: a TRANSIENT position fault in parallel pass A.
// ---------------------------------------------------------------------------

/// An armed `(cf, key)` row.
type ArmedRow = (&'static str, Vec<u8>);

/// The armed `(cf, key)` point read fails ONCE (a transient local fault);
/// later reads succeed. `hits` counts the injected failures.
#[derive(Clone)]
struct OneShot {
    inner: StateDb,
    armed: Arc<std::sync::Mutex<Option<ArmedRow>>>,
    hits: Arc<std::sync::Mutex<usize>>,
}

impl StateBackend for OneShot {
    fn get_cf_raw(&self, cf: &str, key: &[u8]) -> Result<Option<Vec<u8>>, StateError> {
        let mut armed = self.armed.lock().unwrap();
        if armed
            .as_ref()
            .is_some_and(|(c, k)| *c == cf && k.as_slice() == key)
        {
            *armed = None;
            *self.hits.lock().unwrap() += 1;
            return Err(StateError::Io(std::io::Error::other(
                "one-shot position IO failure",
            )));
        }
        drop(armed);
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

/// Markets 1 and 2: in each, `MAKER` rests a bid at 100 and `TAKER` sells
/// into it (IOC, nothing reserved). `MAKER`'s market-1 position read fails
/// once, armed right before the pinned settle (`workers`: `None` =
/// sequential, `Some(w)` = parallel with pass A over at most `w` chunks).
/// Returns the fail-stop, the injected hits and the fallbacks.
fn settle_one_shot_maker_fault(workers: Option<usize>) -> (Option<String>, usize, u64) {
    let dir = tempfile::tempdir().unwrap();
    let backend = OneShot {
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
    let params = limit(false, 100, TimeInForce::IOC);
    let mut market_results = Vec::new();
    let mut market_batches: HashMap<MarketId, Vec<PreparedOrder<'_>>> = HashMap::default();
    for (i, market_id) in [1 as MarketId, 2].into_iter().enumerate() {
        let mut book = OrderBook::new(market_id, FixedPoint::ONE, FixedPoint::ONE);
        book.set_next_order_id(1);
        let p = PlaceOrderParams {
            market_id,
            ..limit(true, 100, TimeInForce::GTC)
        };
        book.place_order(p, MAKER, NOW);
        let result = book.place_order(
            PlaceOrderParams {
                market_id,
                ..params.clone()
            },
            TAKER,
            NOW,
        );
        assert_eq!(
            result.fills.len(),
            1,
            "market {market_id}: the IOC sell fills"
        );
        let next_order_id = book.next_order_id();
        market_results.push(MarketBatchResult {
            market_id,
            book,
            results: vec![MatchResult {
                sender: TAKER,
                order_id: 2,
                result,
            }],
            next_order_id,
        });
        market_batches.insert(
            market_id,
            vec![PreparedOrder {
                index: i,
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
    }
    let mut results = vec![NativeActionResult::ok("pending", 0); 2];
    let mut gas = 0u64;
    let (mut bal_cache, mut pos_cache, mut vol_cache) = (
        BalanceCache::new(),
        PositionCache::new(),
        VolumeCache::default(),
    );
    *backend.armed.lock().unwrap() = Some((
        torus_state::cf::CF_NATIVE_POSITIONS,
        torus_core::position::position_key(&MAKER, 1).to_vec(),
    ));
    match workers {
        Some(w) => NativeExecutor::settle_market_results_parallel(
            &mut ctx,
            market_results,
            &market_batches,
            &mut results,
            &mut gas,
            &mut bal_cache,
            &mut pos_cache,
            &mut vol_cache,
            w,
        ),
        None => NativeExecutor::settle_market_results_sequential(
            &mut ctx,
            market_results,
            &market_batches,
            &mut results,
            &mut gas,
            &mut bal_cache,
            &mut pos_cache,
            &mut vol_cache,
        ),
    }
    let hits = *backend.hits.lock().unwrap();
    (
        ctx.take_fatal_error(),
        hits,
        ctx.phase_accum.settle_fallbacks,
    )
}

/// Codex P2: the maker's position read fails once. Sequential latches
/// "settle maker fill"; parallel pass A hits it, falls back, and the
/// sequential retry succeeds. RED before the fix: parallel committed with no
/// fail-stop. Both modes must fail-stop identically.
#[test]
fn r02_parallel_settle_transient_position_fault_fail_stops_like_sequential() {
    let (seq, hits, fallbacks) = settle_one_shot_maker_fault(None);
    assert_eq!((hits, fallbacks), (1, 0), "sequential: fault injected once");
    let seq = seq.expect("sequential must fail-stop");
    assert!(seq.starts_with("settle maker fill: "), "{seq}");
    assert!(seq.contains("one-shot position IO failure"), "{seq}");
    for w in [2, 4] {
        let (par, hits, fallbacks) = settle_one_shot_maker_fault(Some(w));
        assert_eq!(hits, 1, "workers {w}: fault injected once (in pass A)");
        assert_eq!(fallbacks, 1, "workers {w}: fell back to sequential");
        assert_eq!(
            par.as_deref(),
            Some(seq.as_str()),
            "workers {w}: same fail-stop"
        );
    }
}
