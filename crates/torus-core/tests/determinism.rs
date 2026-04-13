//! Determinism tests for OrderBook (task 2.1b.4).
//!
//! Same input sequence on two independent OrderBook instances must produce
//! identical results and final state.

use torus_core::order_book::{OrderBook, PlaceResult};
use torus_types::{Address, FixedPoint, OrderType, PlaceOrderParams, TimeInForce};

fn fp(n: i64) -> FixedPoint {
    FixedPoint::from_raw(n as i128 * FixedPoint::SCALE)
}

fn addr(n: u8) -> Address {
    Address::from([n; 20])
}

fn new_book() -> OrderBook {
    OrderBook::new(1, FixedPoint::from_raw(1), FixedPoint::from_raw(1))
}

fn limit_buy(price: FixedPoint, qty: FixedPoint) -> PlaceOrderParams {
    PlaceOrderParams {
        market_id: 1,
        is_buy: true,
        price,
        quantity: qty,
        order_type: OrderType::Limit,
        time_in_force: TimeInForce::GTC,
        reduce_only: false,
        client_order_id: None,
    }
}

fn limit_sell(price: FixedPoint, qty: FixedPoint) -> PlaceOrderParams {
    PlaceOrderParams {
        market_id: 1,
        is_buy: false,
        price,
        quantity: qty,
        order_type: OrderType::Limit,
        time_in_force: TimeInForce::GTC,
        reduce_only: false,
        client_order_id: None,
    }
}

fn market_buy(qty: FixedPoint) -> PlaceOrderParams {
    PlaceOrderParams {
        market_id: 1,
        is_buy: true,
        price: FixedPoint::ZERO,
        quantity: qty,
        order_type: OrderType::Market,
        time_in_force: TimeInForce::GTC,
        reduce_only: false,
        client_order_id: None,
    }
}

enum Op {
    Place(PlaceOrderParams, Address, u64),
    Cancel(u128),
    Modify(u128, Option<FixedPoint>, Option<FixedPoint>),
    CancelAll(Address),
}

fn run_sequence(book: &mut OrderBook, ops: &[Op]) -> Vec<PlaceResult> {
    let mut results = Vec::new();
    for op in ops {
        match op {
            Op::Place(params, trader, ts) => {
                results.push(book.place_order(params.clone(), *trader, *ts));
            }
            Op::Cancel(id) => {
                let _ = book.cancel_order(*id);
            }
            Op::Modify(id, price, qty) => {
                let _ = book.modify_order(*id, *price, *qty);
            }
            Op::CancelAll(trader) => {
                book.cancel_all(*trader, None);
            }
        }
    }
    results
}

fn assert_results_eq(a: &PlaceResult, b: &PlaceResult, ctx: &str) {
    assert_eq!(a.order_id, b.order_id, "{ctx}: order_id mismatch");
    assert_eq!(a.status, b.status, "{ctx}: status mismatch");
    assert_eq!(a.fills, b.fills, "{ctx}: fills mismatch");
    assert_eq!(
        a.self_trade_cancels, b.self_trade_cancels,
        "{ctx}: self_trade_cancels mismatch"
    );
}

fn assert_books_eq(a: &OrderBook, b: &OrderBook) {
    assert_eq!(a.best_bid(), b.best_bid(), "best_bid mismatch");
    assert_eq!(a.best_ask(), b.best_ask(), "best_ask mismatch");
    assert_eq!(a.order_count(), b.order_count(), "order_count mismatch");
    assert_eq!(a.bid_levels(), b.bid_levels(), "bid_levels mismatch");
    assert_eq!(a.ask_levels(), b.ask_levels(), "ask_levels mismatch");
    assert_eq!(
        a.last_trade_price(),
        b.last_trade_price(),
        "last_trade_price mismatch"
    );
    assert_eq!(
        a.pending_stop_count(),
        b.pending_stop_count(),
        "pending_stop_count mismatch"
    );
}

fn run_determinism_test(ops: Vec<Op>) {
    let mut book1 = new_book();
    let mut book2 = new_book();

    let results1 = run_sequence(&mut book1, &ops);
    let results2 = run_sequence(&mut book2, &ops);

    assert_eq!(results1.len(), results2.len());
    for (i, (r1, r2)) in results1.iter().zip(results2.iter()).enumerate() {
        assert_results_eq(r1, r2, &format!("op {i}"));
    }
    assert_books_eq(&book1, &book2);
    book1.verify_invariants();
    book2.verify_invariants();
}

#[test]
fn determinism_basic_sequence() {
    run_determinism_test(vec![
        Op::Place(limit_sell(fp(100), fp(10)), addr(1), 1),
        Op::Place(limit_sell(fp(101), fp(5)), addr(2), 2),
        Op::Place(limit_buy(fp(99), fp(8)), addr(3), 3),
        Op::Place(limit_buy(fp(100), fp(3)), addr(4), 4),
        Op::Place(limit_buy(fp(101), fp(12)), addr(5), 5),
        Op::Cancel(3),
        Op::Place(limit_sell(fp(98), fp(5)), addr(6), 6),
    ]);
}

#[test]
fn determinism_with_cancels_and_modifications() {
    run_determinism_test(vec![
        Op::Place(limit_buy(fp(99), fp(10)), addr(1), 1),
        Op::Place(limit_buy(fp(100), fp(10)), addr(2), 2),
        Op::Place(limit_sell(fp(101), fp(10)), addr(3), 3),
        Op::Place(limit_sell(fp(102), fp(10)), addr(4), 4),
        Op::Modify(1, Some(fp(98)), Some(fp(5))),
        Op::Modify(3, None, Some(fp(7))),
        Op::Place(market_buy(fp(15)), addr(5), 5),
        Op::CancelAll(addr(1)),
        Op::Place(limit_sell(fp(99), fp(5)), addr(6), 6),
    ]);
}

#[test]
fn determinism_self_trade_sequence() {
    run_determinism_test(vec![
        Op::Place(limit_sell(fp(100), fp(10)), addr(1), 1),
        Op::Place(limit_sell(fp(100), fp(5)), addr(2), 2),
        Op::Place(limit_buy(fp(100), fp(20)), addr(1), 3),
        Op::Place(limit_sell(fp(99), fp(5)), addr(3), 4),
    ]);
}

#[test]
fn determinism_large_sequence() {
    let mut ops = Vec::new();

    for i in 0..100u64 {
        ops.push(Op::Place(
            limit_buy(fp(90 + (i % 10) as i64), fp(1 + (i % 5) as i64)),
            addr((i % 20) as u8 + 1),
            i,
        ));
        ops.push(Op::Place(
            limit_sell(fp(110 + (i % 10) as i64), fp(1 + (i % 5) as i64)),
            addr((i % 20) as u8 + 21),
            100 + i,
        ));
    }

    for i in (1..=50u128).step_by(3) {
        ops.push(Op::Cancel(i));
    }

    for i in 0..20u64 {
        ops.push(Op::Place(
            limit_buy(fp(115), fp(5)),
            addr((i % 10) as u8 + 50),
            300 + i,
        ));
    }

    for i in (51..=100u128).step_by(5) {
        ops.push(Op::Modify(i, None, Some(fp(2))));
    }

    run_determinism_test(ops);
}

#[test]
fn determinism_stop_orders() {
    run_determinism_test(vec![
        Op::Place(limit_sell(fp(105), fp(10)), addr(1), 1),
        Op::Place(limit_buy(fp(95), fp(10)), addr(2), 2),
        Op::Place(
            PlaceOrderParams {
                market_id: 1,
                is_buy: true,
                price: FixedPoint::ZERO,
                quantity: fp(5),
                order_type: OrderType::StopMarket { trigger: fp(100) },
                time_in_force: TimeInForce::GTC,
                reduce_only: false,
                client_order_id: None,
            },
            addr(3),
            3,
        ),
        Op::Place(limit_buy(fp(105), fp(1)), addr(4), 4),
    ]);
}
