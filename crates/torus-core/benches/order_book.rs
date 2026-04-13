//! Criterion benchmarks for the order book matching engine.
//! Target: 200,000 orders/sec.

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use torus_core::order_book::{OrderBook, OrderStatus};
use torus_types::{Address, FixedPoint, OrderType, PlaceOrderParams, TimeInForce};

fn fp(n: i64) -> FixedPoint {
    FixedPoint::from_raw(n as i128 * FixedPoint::SCALE)
}

fn addr(n: u8) -> Address {
    Address::from([n; 20])
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

fn new_book() -> OrderBook {
    OrderBook::new(1, FixedPoint::from_raw(1), FixedPoint::from_raw(1))
}

/// Place a limit order with no match (pure insertion).
fn bench_place_no_match(c: &mut Criterion) {
    c.bench_function("place_limit_no_match", |b| {
        b.iter_custom(|iters| {
            let mut ob = new_book();
            let start = std::time::Instant::now();
            for i in 0..iters {
                let price = fp(1000 + (i % 1000) as i64);
                ob.place_order(black_box(limit_buy(price, fp(1))), addr((i % 200) as u8), i);
            }
            start.elapsed()
        });
    });
}

/// Place a limit order that immediately fully matches.
fn bench_place_immediate_match(c: &mut Criterion) {
    c.bench_function("place_limit_immediate_match", |b| {
        b.iter_custom(|iters| {
            let mut ob = new_book();
            for i in 0..iters {
                ob.place_order(limit_sell(fp(100), fp(1)), addr((i % 200) as u8 + 1), i);
            }
            let start = std::time::Instant::now();
            for i in 0..iters {
                ob.place_order(black_box(limit_buy(fp(100), fp(1))), addr(0), iters + i);
            }
            start.elapsed()
        });
    });
}

/// Market order sweeping 10 price levels.
fn bench_market_sweep(c: &mut Criterion) {
    c.bench_function("market_sweep_10_levels", |b| {
        b.iter_custom(|iters| {
            let mut ob = new_book();
            for i in 0..iters {
                for level in 0..10i64 {
                    ob.place_order(
                        limit_sell(fp(100 + level), fp(1)),
                        addr((i % 200) as u8 + 1),
                        i * 10 + level as u64,
                    );
                }
            }
            let start = std::time::Instant::now();
            for i in 0..iters {
                let params = PlaceOrderParams {
                    market_id: 1,
                    is_buy: true,
                    price: FixedPoint::ZERO,
                    quantity: fp(10),
                    order_type: OrderType::Market,
                    time_in_force: TimeInForce::GTC,
                    reduce_only: false,
                    client_order_id: None,
                };
                ob.place_order(black_box(params), addr(0), iters * 10 + i);
            }
            start.elapsed()
        });
    });
}

/// Cancel order by ID.
fn bench_cancel(c: &mut Criterion) {
    c.bench_function("cancel_order_by_id", |b| {
        b.iter_custom(|iters| {
            let mut ob = new_book();
            let mut ids = Vec::with_capacity(iters as usize);
            for i in 0..iters {
                let r = ob.place_order(
                    limit_buy(fp(100 + (i % 100) as i64), fp(1)),
                    addr((i % 200) as u8),
                    i,
                );
                ids.push(r.order_id);
            }
            let start = std::time::Instant::now();
            for id in ids {
                let _ = ob.cancel_order(black_box(id));
            }
            start.elapsed()
        });
    });
}

/// Cancel all orders for a single trader (50 orders).
fn bench_cancel_all(c: &mut Criterion) {
    c.bench_function("cancel_all_50_orders", |b| {
        b.iter(|| {
            let mut ob = new_book();
            let trader = addr(1);
            for i in 0..50u64 {
                ob.place_order(limit_buy(fp(90 + i as i64), fp(1)), trader, i);
            }
            ob.cancel_all(black_box(trader), None)
        });
    });
}

/// Mixed workload: 70% place, 20% cancel, 10% modify.
fn bench_mixed_workload(c: &mut Criterion) {
    c.bench_function("mixed_70_place_20_cancel_10_modify", |b| {
        b.iter_custom(|iters| {
            let mut ob = new_book();
            let mut resting_ids: Vec<u128> = Vec::new();
            let start = std::time::Instant::now();

            for i in 0..iters {
                let action = i % 10;
                if action < 7 {
                    let is_buy = i % 2 == 0;
                    let price = if is_buy {
                        fp(95 + (i % 10) as i64)
                    } else {
                        fp(105 + (i % 10) as i64)
                    };
                    let params = PlaceOrderParams {
                        market_id: 1,
                        is_buy,
                        price,
                        quantity: fp(1),
                        order_type: OrderType::Limit,
                        time_in_force: TimeInForce::GTC,
                        reduce_only: false,
                        client_order_id: None,
                    };
                    let r = ob.place_order(params, addr((i % 200) as u8), i);
                    if r.status == OrderStatus::Resting {
                        resting_ids.push(r.order_id);
                    }
                } else if action < 9 {
                    if let Some(id) = resting_ids.pop() {
                        let _ = ob.cancel_order(id);
                    }
                } else if let Some(&id) = resting_ids.last() {
                    let _ = ob.modify_order(id, None, Some(fp(2)));
                }
            }
            start.elapsed()
        });
    });
}

/// Insert into a book with 1000 existing ask levels.
fn bench_deep_book_insert(c: &mut Criterion) {
    c.bench_function("deep_book_1000_levels_insert", |b| {
        b.iter_custom(|iters| {
            let mut ob = new_book();
            for level in 0..1000i64 {
                ob.place_order(
                    limit_sell(fp(1000 + level), fp(10)),
                    addr((level % 200) as u8 + 1),
                    level as u64,
                );
            }
            let start = std::time::Instant::now();
            for i in 0..iters {
                let price = fp(500 + (i % 500) as i64);
                ob.place_order(black_box(limit_buy(price, fp(1))), addr(0), 1000 + i);
            }
            start.elapsed()
        });
    });
}

/// Single-level match throughput (hot path).
fn bench_hot_path_match(c: &mut Criterion) {
    c.bench_function("hot_path_single_level_match", |b| {
        b.iter_custom(|iters| {
            let mut ob = new_book();
            for i in 0..iters {
                ob.place_order(limit_sell(fp(100), fp(1)), addr(1), i * 2);
            }
            let start = std::time::Instant::now();
            for i in 0..iters {
                ob.place_order(black_box(limit_buy(fp(100), fp(1))), addr(0), i * 2 + 1);
            }
            start.elapsed()
        });
    });
}

criterion_group!(
    benches,
    bench_place_no_match,
    bench_place_immediate_match,
    bench_market_sweep,
    bench_cancel,
    bench_cancel_all,
    bench_mixed_workload,
    bench_deep_book_insert,
    bench_hot_path_match,
);
criterion_main!(benches);
