//! O3: deferred trade-history writes (CF_NATIVE_TRADES / CF_NATIVE_USER_TRADES).
//!
//! These CFs are node-local (NOT in the native consensus root), so their writes
//! can leave the execution critical path: with `ctx.defer_trades` set, fills
//! buffer raw KVs in the context instead of PUTting into the state backend, and
//! the caller hands them to a background writer. The deferred KVs must be
//! byte-identical to what the inline path writes.

use alloy_primitives::Address;

use torus_bridge::native_executor::{NativeExecContext, NativeExecutor};
use torus_core::position::NativeBalance;
use torus_state::cf::{CF_NATIVE_TRADES, CF_NATIVE_USER_TRADES};
use torus_state::{StateBackend, StateDb};
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

fn make_ctx(state_db: StateDb) -> NativeExecContext {
    NativeExecContext::new(
        state_db,
        1,         // block_height
        1000,      // timestamp
        0,         // epoch
        100,       // epoch_length
        10,        // max_validators
        addr(99),  // proposer
        addr(100), // treasury
        addr(101), // dev_pool
    )
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

/// Crossing flow on two markets: 3 fills total (2 on market 1, 1 on market 2).
fn crossing_actions() -> Vec<(Address, NativeAction)> {
    vec![
        (addr(1), NativeAction::PlaceOrder(limit(1, false, 100, 5))),
        (addr(2), NativeAction::PlaceOrder(limit(1, false, 101, 4))),
        // Buy 9 @ 101 crosses both resting sells -> 2 fills on market 1.
        (addr(3), NativeAction::PlaceOrder(limit(1, true, 101, 9))),
        (addr(1), NativeAction::PlaceOrder(limit(2, false, 200, 2))),
        // Buy 2 @ 200 crosses -> 1 fill on market 2.
        (addr(4), NativeAction::PlaceOrder(limit(2, true, 200, 2))),
    ]
}

fn run_batch(ctx: &mut NativeExecContext) {
    let actions = crossing_actions();
    for t in 1..=4u8 {
        fund_native(ctx, &addr(t), fp(1_000_000));
    }
    let result = NativeExecutor::execute_batch(ctx, &actions);
    for (i, r) in result.results.iter().enumerate() {
        assert!(r.success, "action {i} failed: {:?}", r.error);
    }
}

/// All rows of one CF, straight from the backend.
fn cf_rows<T: StateBackend>(state: &T, cf: &str) -> Vec<(Vec<u8>, Vec<u8>)> {
    state.iterate_cf(cf, None).expect("iterate cf")
}

// ============================================================================
// Baseline: default (defer off) writes trade rows inline, as before O3.
// ============================================================================

#[test]
fn defer_off_persists_trades_inline() {
    let (_dir, db) = open_test_db();
    let mut ctx = make_ctx(db.clone());
    run_batch(&mut ctx);

    let trades = cf_rows(&db, CF_NATIVE_TRADES);
    let user_trades = cf_rows(&db, CF_NATIVE_USER_TRADES);
    assert_eq!(trades.len(), 3, "3 fills -> 3 trade rows");
    assert_eq!(user_trades.len(), 6, "3 fills -> maker + taker rows each");
    assert!(
        ctx.take_pending_trades().is_empty(),
        "inline mode must not buffer"
    );
}

// ============================================================================
// Deferred: nothing hits the backend; buffered KVs are byte-identical to the
// inline path's rows.
// ============================================================================

#[test]
fn defer_on_buffers_trades_and_bytes_match_inline() {
    // Inline reference run.
    let (_dir_a, db_a) = open_test_db();
    let mut ctx_a = make_ctx(db_a.clone());
    run_batch(&mut ctx_a);
    let ref_trades = cf_rows(&db_a, CF_NATIVE_TRADES);
    let ref_user_trades = cf_rows(&db_a, CF_NATIVE_USER_TRADES);

    // Deferred run on a fresh DB.
    let (_dir_b, db_b) = open_test_db();
    let mut ctx_b = make_ctx(db_b.clone());
    ctx_b.defer_trades = true;
    run_batch(&mut ctx_b);

    assert!(
        cf_rows(&db_b, CF_NATIVE_TRADES).is_empty(),
        "deferred mode must not write CF_NATIVE_TRADES during exec"
    );
    assert!(
        cf_rows(&db_b, CF_NATIVE_USER_TRADES).is_empty(),
        "deferred mode must not write CF_NATIVE_USER_TRADES during exec"
    );

    let pending = ctx_b.take_pending_trades();
    assert_eq!(
        pending.len(),
        9,
        "3 fills x (1 trade + maker + taker) = 9 buffered KVs"
    );
    assert!(
        ctx_b.take_pending_trades().is_empty(),
        "take_pending_trades must drain the buffer"
    );

    // Apply the buffered KVs (as the background writer would) and compare.
    for (cf, key, value) in &pending {
        db_b.put_cf_raw(cf, key, value).unwrap();
    }
    assert_eq!(
        cf_rows(&db_b, CF_NATIVE_TRADES),
        ref_trades,
        "deferred CF_NATIVE_TRADES rows must be byte-identical to inline"
    );
    assert_eq!(
        cf_rows(&db_b, CF_NATIVE_USER_TRADES),
        ref_user_trades,
        "deferred CF_NATIVE_USER_TRADES rows must be byte-identical to inline"
    );
}

// ============================================================================
// Deferred buffer accumulates across execute_batch calls (app.rs runs pre_evm
// and post_evm batches on one ctx and takes once, after all phases).
// ============================================================================

#[test]
fn pending_trades_accumulate_across_batches() {
    let (_dir, db) = open_test_db();
    let mut ctx = make_ctx(db.clone());
    ctx.defer_trades = true;

    for t in 1..=2u8 {
        fund_native(&ctx, &addr(t), fp(1_000_000));
    }
    let batch1: Vec<(Address, NativeAction)> = vec![
        (addr(1), NativeAction::PlaceOrder(limit(1, false, 100, 5))),
        (addr(2), NativeAction::PlaceOrder(limit(1, true, 100, 5))),
    ];
    let batch2: Vec<(Address, NativeAction)> = vec![
        (addr(1), NativeAction::PlaceOrder(limit(1, false, 100, 3))),
        (addr(2), NativeAction::PlaceOrder(limit(1, true, 100, 3))),
    ];
    let r1 = NativeExecutor::execute_batch(&mut ctx, &batch1);
    let r2 = NativeExecutor::execute_batch(&mut ctx, &batch2);
    assert!(r1.results.iter().all(|r| r.success));
    assert!(r2.results.iter().all(|r| r.success));

    let pending = ctx.take_pending_trades();
    assert_eq!(
        pending.len(),
        6,
        "2 fills x 3 KVs, accumulated across calls"
    );

    // Distinct keys — trade_index kept advancing across batches.
    let mut keys: Vec<&(&str, Vec<u8>, Vec<u8>)> = pending.iter().collect();
    keys.sort_by(|a, b| (a.0, &a.1).cmp(&(b.0, &b.1)));
    keys.dedup_by(|a, b| a.0 == b.0 && a.1 == b.1);
    assert_eq!(keys.len(), 6, "all buffered keys must be distinct");
}
