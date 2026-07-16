//! P2 funnel item 5 — exec-side order fate counters.
//!
//! Every order reaching the native executor must land in exactly one fate
//! bucket so the offered→executed ledger closes:
//!   - `torus_orders_placed`  — entered a book as resting
//!     (Resting / PartiallyFilled / PendingTrigger at placement)
//!   - `torus_orders_rejected{reason}` — exec-side death
//!     (insufficient_margin / balance_error / fill_failed / engine_rejected /
//!      cancelled_unfilled / batch_cap_skipped)
//!   - fully filled taker orders are neither: they show up in
//!     `torus_orders_matched` (which counts FILL EVENTS, not orders — one
//!     order sweeping 3 makers increments it 3×, pinned below).

use alloy_primitives::Address;
use std::sync::Arc;

use torus_bridge::native_executor::{NativeExecContext, NativeExecutor};
use torus_core::position::NativeBalance;
use torus_state::StateDb;
use torus_types::{FixedPoint, MarketId, NativeAction, OrderType, PlaceOrderParams, TimeInForce};

// ---- Helpers (mirrors parallel_matching_tests.rs) ----

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

fn make_ctx(state_db: StateDb) -> (NativeExecContext, Arc<torus_telemetry::Metrics>) {
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
    let metrics = Arc::new(torus_telemetry::Metrics::new());
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

fn limit(market_id: MarketId, is_buy: bool, price: i64, qty: i64) -> PlaceOrderParams {
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

fn rejected(metrics: &torus_telemetry::Metrics, reason: &str) -> u64 {
    metrics
        .orders_rejected
        .get_or_create(&vec![("reason".to_string(), reason.to_string())])
        .get()
}

// ---- Batch path (execute_batch — the production hot path) ----

#[test]
fn batch_resting_order_counts_placed() {
    let (_dir, db) = open_test_db();
    let (mut ctx, metrics) = make_ctx(db);
    let trader = addr(1);
    fund_native(&ctx, &trader, fp(100_000));

    // No counterparty: the order must rest → placed.
    let actions = vec![(trader, NativeAction::PlaceOrder(limit(1, true, 100, 5)))];
    let res = NativeExecutor::execute_batch(&mut ctx, &actions);
    assert!(res.results[0].success);

    assert_eq!(metrics.orders_placed.get(), 1, "resting order → placed");
    assert_eq!(metrics.orders_matched.get(), 0);
}

#[test]
fn batch_insufficient_margin_counts_rejected() {
    let (_dir, db) = open_test_db();
    let (mut ctx, metrics) = make_ctx(db);
    let trader = addr(1);
    fund_native(&ctx, &trader, fp(10)); // needs 100*10/20 = 50

    let actions = vec![(trader, NativeAction::PlaceOrder(limit(1, true, 100, 10)))];
    let res = NativeExecutor::execute_batch(&mut ctx, &actions);
    assert!(!res.results[0].success);

    assert_eq!(
        rejected(&metrics, "insufficient_margin"),
        1,
        "margin death must be counted"
    );
    assert_eq!(metrics.orders_placed.get(), 0);
}

#[test]
fn batch_crossing_pair_places_maker_only_and_matched_counts_fills() {
    let (_dir, db) = open_test_db();
    let (mut ctx, metrics) = make_ctx(db);
    let a = addr(1);
    let b = addr(2);
    fund_native(&ctx, &a, fp(100_000));
    fund_native(&ctx, &b, fp(100_000));

    // Buy rests first (→ placed), sell crosses it and fully fills (→ neither
    // placed nor rejected; 1 fill event lands on orders_matched).
    let actions = vec![
        (a, NativeAction::PlaceOrder(limit(1, true, 100, 5))),
        (b, NativeAction::PlaceOrder(limit(1, false, 100, 5))),
    ];
    let res = NativeExecutor::execute_batch(&mut ctx, &actions);
    assert!(res.results[0].success && res.results[1].success);

    assert_eq!(metrics.orders_placed.get(), 1, "only the maker rested");
    assert_eq!(
        metrics.orders_matched.get(),
        1,
        "orders_matched counts FILL EVENTS (1 fill for this pair)"
    );
    assert_eq!(rejected(&metrics, "engine_rejected"), 0);
    assert_eq!(rejected(&metrics, "cancelled_unfilled"), 0);
}

#[test]
fn batch_taker_sweeping_three_makers_counts_three_fills() {
    let (_dir, db) = open_test_db();
    let (mut ctx, metrics) = make_ctx(db);
    let maker = addr(1);
    let taker = addr(2);
    fund_native(&ctx, &maker, fp(100_000));
    fund_native(&ctx, &taker, fp(100_000));

    // Three resting sells, one buy sweeps all three → 3 fill events for ONE
    // taker order (pins the per-fill semantics of torus_orders_matched).
    let seed = vec![
        (maker, NativeAction::PlaceOrder(limit(1, false, 100, 1))),
        (maker, NativeAction::PlaceOrder(limit(1, false, 101, 1))),
        (maker, NativeAction::PlaceOrder(limit(1, false, 102, 1))),
    ];
    let res = NativeExecutor::execute_batch(&mut ctx, &seed);
    assert!(res.results.iter().all(|r| r.success));
    assert_eq!(metrics.orders_placed.get(), 3);

    let sweep = vec![(taker, NativeAction::PlaceOrder(limit(1, true, 102, 3)))];
    let res = NativeExecutor::execute_batch(&mut ctx, &sweep);
    assert!(res.results[0].success);

    assert_eq!(
        metrics.orders_matched.get(),
        3,
        "one taker order × 3 maker fills = 3 (per-fill, NOT per-order)"
    );
    assert_eq!(
        metrics.orders_placed.get(),
        3,
        "taker fully filled, not placed"
    );
}

#[test]
fn batch_market_order_on_empty_book_counts_engine_rejected() {
    let (_dir, db) = open_test_db();
    let (mut ctx, metrics) = make_ctx(db);
    let trader = addr(1);
    fund_native(&ctx, &trader, fp(100_000));

    let market = PlaceOrderParams {
        order_type: OrderType::Market,
        ..limit(1, true, 0, 5)
    };
    let _ = NativeExecutor::execute_batch(&mut ctx, &[(trader, NativeAction::PlaceOrder(market))]);

    // OrderStatus::Rejected (empty book for Market) or Cancelled-with-no-fill —
    // either way the order died at the engine and must be counted, not silent.
    let engine_deaths =
        rejected(&metrics, "engine_rejected") + rejected(&metrics, "cancelled_unfilled");
    assert_eq!(
        engine_deaths, 1,
        "market order on empty book must land in an engine-death bucket"
    );
    assert_eq!(metrics.orders_placed.get(), 0);
}

#[test]
fn oversize_batch_counts_batch_cap_skipped_orders() {
    use torus_types::NATIVE_ORDERS_PER_BATCH_CAP;
    let (_dir, db) = open_test_db();
    let (mut ctx, metrics) = make_ctx(db);
    let attacker = addr(1);
    fund_native(&ctx, &attacker, fp(100_000_000));

    let n = NATIVE_ORDERS_PER_BATCH_CAP + 1;
    let oversize = NativeAction::PlaceOrderBatch(vec![limit(1, true, 100, 1); n]);
    let _ = NativeExecutor::execute_batch(&mut ctx, &[(attacker, oversize)]);

    assert_eq!(
        rejected(&metrics, "batch_cap_skipped"),
        n as u64,
        "every order in a wholesale-skipped batch must be counted"
    );
}

// ---- Single-action path (exec_place_order via NativeExecutor::execute) ----

#[test]
fn single_path_counts_placed_and_rejects() {
    let (_dir, db) = open_test_db();
    let (mut ctx, metrics) = make_ctx(db);
    let trader = addr(1);
    fund_native(&ctx, &trader, fp(60)); // enough for one 50-margin order

    // Rests → placed.
    let ok = NativeExecutor::execute(
        &mut ctx,
        &trader,
        &NativeAction::PlaceOrder(limit(1, true, 100, 10)),
    );
    assert!(ok.success);
    assert_eq!(metrics.orders_placed.get(), 1);

    // Second identical order: only 10 left of 50 needed → insufficient margin.
    let fail = NativeExecutor::execute(
        &mut ctx,
        &trader,
        &NativeAction::PlaceOrder(limit(2, true, 100, 10)),
    );
    assert!(!fail.success);
    assert_eq!(rejected(&metrics, "insufficient_margin"), 1);
}
