//! S395: order-book persistence — dirty-market-only save + durable global
//! order-id counter.
//!
//! `save_order_books` used to rewrite EVERY loaded book every block
//! (O(all resting orders) serialize+write), and `next_global_order_id` was
//! derived only by scanning book maxima — so draining all books reset the
//! counter across a restart (order-id reuse). These tests pin the fixed
//! behavior: only touched markets are written, and the counter survives
//! restarts independently of book contents.

use alloy_primitives::Address;
use torus_bridge::native_executor::{NativeExecContext, NativeExecutor};
use torus_state::StateDb;
use torus_types::{FixedPoint, NativeAction, OrderType, PlaceOrderParams, TimeInForce};

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
        1,        // block_height
        1000,     // timestamp
        0,        // epoch
        100,      // epoch_length
        10,       // max_validators
        addr(99), // proposer
        addr(100),
        addr(101),
    )
}

fn fund_native(ctx: &NativeExecContext, trader: &Address, amount: FixedPoint) {
    use torus_core::position::NativeBalance;
    let bal = NativeBalance {
        available: amount,
        order_margin: FixedPoint::ZERO,
    };
    ctx.positions.put_native_balance(trader, &bal).unwrap();
}

/// A deep-out-of-the-money resting buy so nothing ever matches.
fn resting_buy(market_id: u64) -> NativeAction {
    NativeAction::PlaceOrder(PlaceOrderParams {
        market_id,
        is_buy: true,
        price: fp(10),
        quantity: fp(1),
        order_type: OrderType::Limit,
        time_in_force: TimeInForce::GTC,
        reduce_only: false,
        client_order_id: None,
    })
}

#[test]
fn save_writes_only_dirty_markets() {
    let (_dir, db) = open_test_db();

    // Block 1: touch markets 1 AND 2 — both dirty, both written.
    let mut ctx = make_ctx(db.clone());
    fund_native(&ctx, &addr(1), fp(1_000_000));
    let r1 = NativeExecutor::execute(&mut ctx, &addr(1), &resting_buy(1));
    let r2 = NativeExecutor::execute(&mut ctx, &addr(1), &resting_buy(2));
    assert!(r1.success, "{:?}", r1.error);
    assert!(r2.success, "{:?}", r2.error);
    assert_eq!(ctx.save_order_books(), 2, "both touched books written");

    // Block 2 (fresh context = fresh block): touch market 1 only.
    let mut ctx = make_ctx(db.clone());
    fund_native(&ctx, &addr(2), fp(1_000_000));
    assert_eq!(ctx.order_books.len(), 2, "both books load");
    let r = NativeExecutor::execute(&mut ctx, &addr(2), &resting_buy(1));
    assert!(r.success, "{:?}", r.error);
    assert_eq!(
        ctx.save_order_books(),
        1,
        "untouched market 2 must NOT be rewritten"
    );

    // Reload: market 2's book is intact (skipping the write lost nothing).
    let ctx = make_ctx(db);
    let book2 = ctx.order_books.get(&2).expect("market 2 book still loads");
    assert_eq!(book2.market_id, 2);
}

#[test]
fn untouched_block_saves_nothing() {
    let (_dir, db) = open_test_db();

    let mut ctx = make_ctx(db.clone());
    fund_native(&ctx, &addr(1), fp(1_000_000));
    let r = NativeExecutor::execute(&mut ctx, &addr(1), &resting_buy(1));
    assert!(r.success);
    assert_eq!(ctx.save_order_books(), 1);

    // A block that executes NO order actions must not rewrite any book.
    let ctx = make_ctx(db);
    assert_eq!(ctx.order_books.len(), 1);
    assert_eq!(ctx.save_order_books(), 0, "no mutations => no writes");
}

#[test]
fn next_global_order_id_survives_book_drain_across_restart() {
    let (_dir, db) = open_test_db();

    // Place a resting order (allocates a global order id), then cancel it so
    // every book is EMPTY. Save.
    let mut ctx = make_ctx(db.clone());
    fund_native(&ctx, &addr(1), fp(1_000_000));
    let r = NativeExecutor::execute(&mut ctx, &addr(1), &resting_buy(1));
    assert!(r.success);
    let id_after_place = ctx.next_global_order_id;
    assert!(id_after_place > 1, "an id was allocated");
    let order_id = ctx
        .order_books
        .get(&1)
        .and_then(|b| b.best_bid().map(|_| ()))
        .map(|_| id_after_place - 1)
        .expect("resting order present");
    let rc = NativeExecutor::execute(&mut ctx, &addr(1), &NativeAction::CancelOrder { order_id });
    assert!(rc.success, "{:?}", rc.error);
    ctx.save_order_books();

    // Restart: books hold zero resting orders, so the legacy max-scan would
    // reset the counter to 1 and reuse order ids. The persisted counter must
    // carry it forward instead.
    let ctx = make_ctx(db);
    assert!(
        ctx.next_global_order_id >= id_after_place,
        "counter must survive a restart with drained books: got {}, want >= {}",
        ctx.next_global_order_id,
        id_after_place
    );
}
