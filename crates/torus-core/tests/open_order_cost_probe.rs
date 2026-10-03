//! Release-mode cost probe for the per-user open-order limit's hot spots:
//! counting senders' open orders over all books (Phase 2, every
//! execute_batch) and removing one trader's orders from a deep book.
//!
//!   cargo test --release -p torus-core --test open_order_cost_probe -- --ignored --nocapture

use std::collections::HashMap;
use std::time::Instant;

use torus_core::order_book::{open_order_counts, OrderBook};
use torus_types::{Address, FixedPoint, OrderType, PlaceOrderParams, TimeInForce};

fn fp(v: i64) -> FixedPoint {
    FixedPoint::from_raw(v as i128 * FixedPoint::SCALE)
}

fn trader(n: u64) -> Address {
    let mut b = [0u8; 20];
    b[..8].copy_from_slice(&n.to_be_bytes());
    Address::from(b)
}

fn order(market_id: u64, is_buy: bool, price: i64, tif: TimeInForce) -> PlaceOrderParams {
    PlaceOrderParams {
        market_id,
        is_buy,
        price: fp(price),
        quantity: fp(1),
        order_type: if price == 0 {
            OrderType::Market
        } else {
            OrderType::Limit
        },
        time_in_force: tif,
        reduce_only: false,
        client_order_id: None,
    }
}

/// `markets` books; book m holds `per` bids of every trader in `owners(m)`.
fn books(markets: u64, per: u64, owners: impl Fn(u64) -> Vec<u64>) -> Vec<OrderBook> {
    (1..=markets)
        .map(|m| {
            let mut b = OrderBook::new(m, fp(1), fp(1));
            for t in owners(m) {
                for k in 0..per {
                    let price = 1 + ((t * per + k) % 400) as i64;
                    b.place_order(order(m, true, price, TimeInForce::GTC), trader(t), 0);
                }
            }
            b
        })
        .collect()
}

fn time_counts(label: &str, books: &[OrderBook], senders: u64) {
    let idx: HashMap<Address, usize> = (0..senders).map(|s| (trader(s), s as usize)).collect();
    let refs: Vec<&OrderBook> = books.iter().collect();
    for threads in [0, 8] {
        let mut best = f64::MAX;
        let mut total = 0;
        for _ in 0..5 {
            let t0 = Instant::now();
            let counts = open_order_counts(&refs, &idx, threads, 25_000);
            best = best.min(t0.elapsed().as_secs_f64() * 1e3);
            total = counts.iter().sum::<usize>();
        }
        println!("{label}: {senders} senders, {threads} threads -> {best:.1} ms (sum {total})");
    }
}

#[test]
#[ignore = "release cost probe; run with --release -- --ignored --nocapture"]
fn open_order_count_cost() {
    // Dense: every book holds all 5000 traders (uniform econ shape).
    let dense = books(300, 3, |_| (0..5000).collect());
    time_counts("dense 300 books x 5000 traders", &dense, 5000);
    time_counts("dense 300 books x 5000 traders", &dense, 400);
    drop(dense);
    // Sparse: each book holds 50 traders (5000 senders x 3 markets / 300).
    let sparse = books(300, 3, |m| (0..50).map(|j| ((m - 1) * 50 + j) % 5000).collect());
    time_counts("sparse 300 books x 50 traders", &sparse, 5000);
    // Idle: 290 of 300 books empty.
    let idle = books(300, 3, |m| if m <= 10 { (0..5000).collect() } else { vec![] });
    time_counts("idle 290 of 300 books empty", &idle, 5000);
}

#[test]
#[ignore = "release cost probe; run with --release -- --ignored --nocapture"]
fn one_traders_order_removal_cost() {
    const N: u64 = 5000;
    let maker = trader(1);
    let fresh = || {
        let mut b = OrderBook::new(1, fp(1), fp(1));
        let ids: Vec<_> = (0..N)
            .map(|k| {
                let price = 1000 + (k % 400) as i64;
                b.place_order(order(1, false, price, TimeInForce::GTC), maker, 0).order_id
            })
            .collect();
        (b, ids)
    };

    // CancelOrder of all N, newest first (worst case for a front-to-back scan).
    let (mut b, ids) = fresh();
    let t0 = Instant::now();
    for id in ids.iter().rev() {
        b.cancel_order(*id).unwrap();
    }
    println!("cancel_order x{N} (one trader): {:.1} ms", t0.elapsed().as_secs_f64() * 1e3);

    // Maker fills: market buys from another trader sweep all N asks.
    let (mut b, _) = fresh();
    let t0 = Instant::now();
    for _ in 0..N {
        b.place_order(order(1, true, 0, TimeInForce::IOC), trader(2), 0);
    }
    assert_eq!(b.open_order_count(&maker), 0);
    println!("maker fills x{N} (one trader): {:.1} ms", t0.elapsed().as_secs_f64() * 1e3);

    // Self-trade cancels: the maker's own market buys STP-cancel its asks.
    let (mut b, _) = fresh();
    let t0 = Instant::now();
    b.place_order(
        PlaceOrderParams {
            quantity: fp(N as i64),
            ..order(1, true, 0, TimeInForce::IOC)
        },
        maker,
        0,
    );
    assert_eq!(b.open_order_count(&maker), 0);
    println!("STP cancels x{N} (one order): {:.1} ms", t0.elapsed().as_secs_f64() * 1e3);
}

#[test]
#[ignore = "release cost probe; run with --release -- --ignored --nocapture"]
fn trader_churn_cost() {
    // Bench shape per block: ~200 senders each rest ~3 orders in each of 300
    // books, then cancel-all (Phase 1, `cancel_all_many`) clears them.
    const BOOKS: u64 = 300;
    const SENDERS: u64 = 200;
    const PER: u64 = 3;
    let senders: Vec<Address> = (0..SENDERS).map(trader).collect();
    let mut best_place = f64::MAX;
    let mut best_cancel = f64::MAX;
    for _ in 0..5 {
        let mut books: Vec<OrderBook> =
            (1..=BOOKS).map(|m| OrderBook::new(m, fp(1), fp(1))).collect();
        let t0 = Instant::now();
        for (m, b) in books.iter_mut().enumerate() {
            for (s, t) in senders.iter().enumerate() {
                for k in 0..PER {
                    let price = 1 + ((s as u64 * PER + k + m as u64) % 400) as i64;
                    b.place_order(order(m as u64 + 1, true, price, TimeInForce::GTC), *t, 0);
                }
            }
        }
        best_place = best_place.min(t0.elapsed().as_secs_f64() * 1e3);
        let t0 = Instant::now();
        let mut cancelled = 0;
        for b in books.iter_mut() {
            cancelled += b.cancel_all_many(&senders).iter().map(Vec::len).sum::<usize>();
        }
        best_cancel = best_cancel.min(t0.elapsed().as_secs_f64() * 1e3);
        assert_eq!(cancelled as u64, BOOKS * SENDERS * PER);
    }
    println!(
        "churn {BOOKS} books x {SENDERS} senders x {PER}: place {best_place:.1} ms, \
         cancel_all_many {best_cancel:.1} ms"
    );
}
