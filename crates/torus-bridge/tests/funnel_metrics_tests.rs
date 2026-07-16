//! Funnel-truth metrics tests (perf A1): every PlaceOrder outcome must land in
//! exactly one funnel counter, so GET /metrics can explain where the ~398/400
//! orders of a PlaceOrderBatch die inside execute_batch.
//!
//! Counters under test (registered in torus-telemetry, incremented in
//! torus-bridge::native_executor):
//!   - orders_placed_accepted  (status Filled | PartiallyFilled | Resting)
//!   - orders_resting          (status Resting | PartiallyFilled)
//!   - orders_rejected_margin  (insufficient available balance for the reserve)
//!   - orders_rejected_book    (OrderStatus::Rejected from the matching engine)
//!   - orders_rejected_cancelled (IOC/FOK/Market cancelled on arrival, zero fills)
//!   - orders_cancelled_partial_fill (IOC/Market remainder cancelled after fills)
//!   - orders_self_trade_cancels (resting makers auto-cancelled by STP)
//!   - orders_rejected_other   (balance read/write errors, fill application errors)
//!
//! Counters are observability only: NativeActionResult and all consensus state
//! must be byte-identical with metrics attached or absent.

use std::sync::Arc;

use alloy_primitives::Address;

use torus_bridge::native_executor::{NativeExecContext, NativeExecutor};
use torus_core::position::NativeBalance;
use torus_state::StateDb;
use torus_telemetry::Metrics;
use torus_types::{
    FixedPoint, MarketId, NativeAction, OrderType, PlaceOrderParams, TimeInForce,
};

// ---- Helpers (same idiom as parallel_matching_tests.rs) ----

fn open_test_db() -> (tempfile::TempDir, StateDb) {
    let dir = tempfile::tempdir().expect("create temp dir");
    let db = StateDb::open(dir.path()).expect("open db");
    (dir, db)
}

fn addr(n: u8) -> Address {
    Address::new([n; 20])
}

fn fp(v: i64) -> FixedPoint {
    FixedPoint::from_raw(v as i128 * FixedPoint::SCALE)
}

fn make_ctx(state_db: StateDb) -> (NativeExecContext, Arc<Metrics>) {
    let mut ctx = NativeExecContext::new(
        state_db,
        1,         // block_height
        1000,      // timestamp
        0,         // epoch
        100,       // epoch_length
        10,        // max_validators
        addr(99),  // proposer
        addr(100), // treasury
        addr(101), // dev_pool
    );
    let metrics = Arc::new(Metrics::new());
    ctx.metrics = Some(metrics.clone());
    (ctx, metrics)
}

fn fund_native(ctx: &NativeExecContext, trader: &Address, amount: FixedPoint) {
    let bal = NativeBalance {
        available: amount,
        order_margin: FixedPoint::ZERO,
    };
    ctx.positions.put_native_balance(trader, &bal).unwrap();
}

fn limit_order(market_id: MarketId, is_buy: bool, price: i64, qty: i64) -> PlaceOrderParams {
    PlaceOrderParams {
        market_id,
        is_buy,
        price: fp(price),
        quantity: fp(qty),
        order_type: OrderType::Limit,
        time_in_force: TimeInForce::GTC,
        reduce_only: false,
        client_order_id: None,
    }
}

fn market_order(market_id: MarketId, is_buy: bool, qty: i64) -> PlaceOrderParams {
    PlaceOrderParams {
        market_id,
        is_buy,
        price: FixedPoint::ZERO,
        quantity: fp(qty),
        order_type: OrderType::Market,
        time_in_force: TimeInForce::IOC,
        reduce_only: false,
        client_order_id: None,
    }
}

fn ioc_limit(market_id: MarketId, is_buy: bool, price: i64, qty: i64) -> PlaceOrderParams {
    PlaceOrderParams {
        market_id,
        is_buy,
        price: fp(price),
        quantity: fp(qty),
        order_type: OrderType::Limit,
        time_in_force: TimeInForce::IOC,
        reduce_only: false,
        client_order_id: None,
    }
}

/// Assert the full funnel-counter state in one call so tests document every
/// counter, not just the one they exercise.
#[allow(clippy::too_many_arguments)]
fn assert_funnel(
    m: &Metrics,
    accepted: u64,
    resting: u64,
    rej_margin: u64,
    rej_book: u64,
    rej_cancelled: u64,
    partial_cancel: u64,
    self_trade: u64,
    rej_other: u64,
) {
    assert_eq!(m.orders_placed_accepted.get(), accepted, "orders_placed_accepted");
    assert_eq!(m.orders_resting.get(), resting, "orders_resting");
    assert_eq!(m.orders_rejected_margin.get(), rej_margin, "orders_rejected_margin");
    assert_eq!(m.orders_rejected_book.get(), rej_book, "orders_rejected_book");
    assert_eq!(m.orders_rejected_cancelled.get(), rej_cancelled, "orders_rejected_cancelled");
    assert_eq!(m.orders_cancelled_partial_fill.get(), partial_cancel, "orders_cancelled_partial_fill");
    assert_eq!(m.orders_self_trade_cancels.get(), self_trade, "orders_self_trade_cancels");
    assert_eq!(m.orders_rejected_other.get(), rej_other, "orders_rejected_other");
}

// ============================================================================
// (a) Clean match through execute_batch
// ============================================================================

#[test]
fn batch_clean_match_counts_accepted_and_matched() {
    let (_dir, db) = open_test_db();
    let (mut ctx, m) = make_ctx(db);

    let a = addr(1);
    let b = addr(2);
    fund_native(&ctx, &a, fp(100_000));
    fund_native(&ctx, &b, fp(100_000));

    // A's buy rests first, B's sell crosses it -> 1 fill.
    let actions: Vec<(Address, NativeAction)> = vec![
        (a, NativeAction::PlaceOrder(limit_order(1, true, 100, 5))),
        (b, NativeAction::PlaceOrder(limit_order(1, false, 100, 5))),
    ];
    let res = NativeExecutor::execute_batch(&mut ctx, &actions);
    assert!(res.results.iter().all(|r| r.success), "{:?}", res.results);

    // A: Resting at match time (accepted + resting); B: Filled (accepted).
    assert_eq!(m.orders_matched.get(), 1, "orders_matched (fills)");
    assert_funnel(&m, 2, 1, 0, 0, 0, 0, 0, 0);
}

// ============================================================================
// (b) Resting limit order through execute_batch
// ============================================================================

#[test]
fn batch_resting_limit_counts_accepted_and_resting() {
    let (_dir, db) = open_test_db();
    let (mut ctx, m) = make_ctx(db);

    let a = addr(1);
    fund_native(&ctx, &a, fp(100_000));

    let actions: Vec<(Address, NativeAction)> = vec![(
        a,
        NativeAction::PlaceOrder(limit_order(1, true, 100, 5)),
    )];
    let res = NativeExecutor::execute_batch(&mut ctx, &actions);
    assert!(res.results[0].success);

    assert_eq!(m.orders_matched.get(), 0);
    assert_funnel(&m, 1, 1, 0, 0, 0, 0, 0, 0);
}

// ============================================================================
// (c) Margin-insufficient order through execute_batch
// ============================================================================

#[test]
fn batch_margin_insufficient_counts_rejected_margin() {
    let (_dir, db) = open_test_db();
    let (mut ctx, m) = make_ctx(db);

    // Unfunded trader: zero available balance, limit order needs notional/20.
    let a = addr(1);
    let actions: Vec<(Address, NativeAction)> = vec![(
        a,
        NativeAction::PlaceOrder(limit_order(1, true, 100, 5)),
    )];
    let res = NativeExecutor::execute_batch(&mut ctx, &actions);
    assert!(!res.results[0].success, "margin reject must err the action");
    assert!(
        res.results[0]
            .error
            .as_deref()
            .unwrap_or("")
            .contains("insufficient margin"),
        "{:?}",
        res.results[0].error
    );

    assert_funnel(&m, 0, 0, 1, 0, 0, 0, 0, 0);
}

// ============================================================================
// (d) Book-Rejected order through execute_batch (market order, empty book)
// ============================================================================

#[test]
fn batch_book_rejected_counts_rejected_book_not_accepted() {
    let (_dir, db) = open_test_db();
    let (mut ctx, m) = make_ctx(db);

    let a = addr(1);
    fund_native(&ctx, &a, fp(100_000));

    // Market order into an empty book -> OrderStatus::Rejected from the engine.
    let actions: Vec<(Address, NativeAction)> = vec![(
        a,
        NativeAction::PlaceOrder(market_order(1, true, 5)),
    )];
    let res = NativeExecutor::execute_batch(&mut ctx, &actions);
    // Documents the funnel gap under investigation: the action still reports
    // success even though the book rejected the order.
    assert!(res.results[0].success);

    assert_eq!(m.orders_matched.get(), 0);
    assert_funnel(&m, 0, 0, 0, 1, 0, 0, 0, 0);
}

// ============================================================================
// IOC cancelled-on-arrival (zero fills) through execute_batch
// ============================================================================

#[test]
fn batch_ioc_no_fill_counts_rejected_cancelled() {
    let (_dir, db) = open_test_db();
    let (mut ctx, m) = make_ctx(db);

    let a = addr(1);
    fund_native(&ctx, &a, fp(100_000));

    // IOC limit with no counterparty: status Cancelled, zero fills.
    let actions: Vec<(Address, NativeAction)> = vec![(
        a,
        NativeAction::PlaceOrder(ioc_limit(1, true, 100, 5)),
    )];
    let res = NativeExecutor::execute_batch(&mut ctx, &actions);
    assert!(res.results[0].success);

    assert_funnel(&m, 0, 0, 0, 0, 1, 0, 0, 0);
}

// ============================================================================
// Self-trade prevention through execute_batch
// ============================================================================

#[test]
fn batch_self_trade_cancel_counts_self_trade() {
    let (_dir, db) = open_test_db();
    let (mut ctx, m) = make_ctx(db);

    let a = addr(1);
    fund_native(&ctx, &a, fp(100_000));

    // A rests a buy, then A sells into itself: STP cancels the resting maker,
    // the sell then rests (no fill).
    let actions: Vec<(Address, NativeAction)> = vec![
        (a, NativeAction::PlaceOrder(limit_order(1, true, 100, 5))),
        (a, NativeAction::PlaceOrder(limit_order(1, false, 100, 5))),
    ];
    let res = NativeExecutor::execute_batch(&mut ctx, &actions);
    assert!(res.results.iter().all(|r| r.success), "{:?}", res.results);

    assert_eq!(m.orders_matched.get(), 0, "STP must not produce fills");
    assert_eq!(m.orders_self_trade_cancels.get(), 1, "one resting maker cancelled");
    // Both orders were accepted (each rested at its own match time).
    assert_eq!(m.orders_placed_accepted.get(), 2);
}

// ============================================================================
// Same funnel counters on the sequential exec_place_order path
// ============================================================================

#[test]
fn sequential_path_counts_resting_margin_and_book_rejects() {
    let (_dir, db) = open_test_db();
    let (mut ctx, m) = make_ctx(db);

    let funded = addr(1);
    let unfunded = addr(2);
    fund_native(&ctx, &funded, fp(100_000));

    // Resting limit order.
    let r = NativeExecutor::execute(
        &mut ctx,
        &funded,
        &NativeAction::PlaceOrder(limit_order(1, true, 100, 5)),
    );
    assert!(r.success);
    assert_funnel(&m, 1, 1, 0, 0, 0, 0, 0, 0);

    // Margin-insufficient order.
    let r = NativeExecutor::execute(
        &mut ctx,
        &unfunded,
        &NativeAction::PlaceOrder(limit_order(1, true, 100, 5)),
    );
    assert!(!r.success);
    assert_funnel(&m, 1, 1, 1, 0, 0, 0, 0, 0);

    // Book-rejected market order (empty opposite side: buy needs asks).
    let r = NativeExecutor::execute(
        &mut ctx,
        &funded,
        &NativeAction::PlaceOrder(market_order(1, true, 5)),
    );
    assert!(r.success, "book reject still reports action success today");
    assert_funnel(&m, 1, 1, 1, 1, 0, 0, 0, 0);
}

// ============================================================================
// Counters must not perturb execution results (observability only)
// ============================================================================

#[test]
fn metrics_do_not_change_results() {
    // Same scenario with and without metrics attached: identical results.
    let run = |with_metrics: bool| -> Vec<(bool, Option<String>)> {
        let (_dir, db) = open_test_db();
        let (mut ctx, _m) = make_ctx(db);
        if !with_metrics {
            ctx.metrics = None;
        }
        let a = addr(1);
        let b = addr(2);
        fund_native(&ctx, &a, fp(100_000));
        fund_native(&ctx, &b, fp(100_000));
        let actions: Vec<(Address, NativeAction)> = vec![
            (a, NativeAction::PlaceOrder(limit_order(1, true, 100, 5))),
            (b, NativeAction::PlaceOrder(limit_order(1, false, 100, 5))),
            (addr(3), NativeAction::PlaceOrder(limit_order(1, true, 100, 5))), // margin reject
            (a, NativeAction::PlaceOrder(market_order(2, true, 5))),           // book reject
        ];
        NativeExecutor::execute_batch(&mut ctx, &actions)
            .results
            .into_iter()
            .map(|r| (r.success, r.error))
            .collect()
    };
    assert_eq!(run(true), run(false));
}
