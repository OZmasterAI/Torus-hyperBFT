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

fn loadavg() -> String {
    std::fs::read_to_string("/proc/loadavg")
        .map(|s| s.split_whitespace().take(3).collect::<Vec<_>>().join(" "))
        .unwrap_or_default()
}

/// `books` books; each of `traders` traders rests `per_trader` bids spread
/// over the books by a fixed xorshift. Returns the books and the mean number
/// of distinct traders per book.
fn spread_books(books: u64, traders: u64, per_trader: u64) -> (Vec<OrderBook>, usize) {
    let mut out: Vec<OrderBook> = (1..=books).map(|m| OrderBook::new(m, fp(1), fp(1))).collect();
    let mut present = vec![std::collections::HashSet::new(); books as usize];
    let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
    for t in 0..traders {
        for k in 0..per_trader {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            let m = x % books;
            let price = 1 + ((t + k) % 400) as i64;
            out[m as usize].place_order(order(m + 1, true, price, TimeInForce::GTC), trader(t), 0);
            present[m as usize].insert(t);
        }
    }
    let per_book = present.iter().map(|p| p.len()).sum::<usize>() / books as usize;
    (out, per_book)
}

/// Times each `(name, max_threads, work_per_thread)` candidate `reps` times,
/// round-robin (so a load change hits every candidate alike), and prints
/// min / median / p90 ms; all candidates must give the same counts.
fn compare_splits(label: &str, refs: &[&OrderBook], senders: u64, every: u64) {
    let cap = std::thread::available_parallelism().map_or(1, |n| n.get());
    let idx: HashMap<Address, usize> =
        (0..senders).map(|s| (trader(s * every), s as usize)).collect();
    let candidates = [
        ("serial", 0usize, usize::MAX),
        ("pre-fix 25k", cap, 25_000),
        ("100k", cap, 100_000),
        ("8 workers", 8, 1),
    ];
    let reps = 41;
    let mut ms = vec![Vec::with_capacity(reps); candidates.len()];
    let want = open_order_counts(refs, &idx, 0, 0);
    for _ in 0..reps {
        for (c, &(_, threads, per)) in candidates.iter().enumerate() {
            let t0 = Instant::now();
            let counts = open_order_counts(refs, &idx, threads, per);
            ms[c].push(t0.elapsed().as_secs_f64() * 1e3);
            assert_eq!(counts, want);
        }
    }
    let sum: usize = want.iter().sum();
    for (c, (name, _, _)) in candidates.iter().enumerate() {
        ms[c].sort_by(f64::total_cmp);
        println!(
            "{label}, {senders} senders (sum {sum}), {name:>11}: min {:6.2}  median {:6.2}  \
             p90 {:6.2} ms  load {}",
            ms[c][0],
            ms[c][reps / 2],
            ms[c][reps * 9 / 10],
            loadavg()
        );
    }
}

/// The s84 bench cell's count (300 markets, 5000 senders, batches of 400
/// orders spread uniformly over the markets, budget 900): the
/// `torus_exec_resting_orders` gauge sits at ~300k (median) to ~600k (peak),
/// so each of the 5000 traders rests ~60-120 orders spread over the 300
/// books. A native block carries ~365 actions, about a third of them
/// cancel-alls, so a count sees ~250 distinct GTC senders (at most 400, the
/// block's action cap). Candidates: serial, the pre-fix split (25k probes
/// per worker, up to available_parallelism workers), a 100k split and 8
/// workers regardless of work; plus a 1000-market shape (400k probes) and
/// the bare spawn/join on empty books.
#[test]
#[ignore = "release cost probe; run with --release -- --ignored --nocapture"]
fn open_order_count_bench_shape() {
    for (label, per_trader) in [("median", 60u64), ("peak", 120)] {
        let (books, traders) = spread_books(300, 5000, per_trader);
        let orders = books.iter().map(OrderBook::order_count).sum::<usize>() / 300;
        let refs: Vec<&OrderBook> = books.iter().collect();
        let label = format!("bench {label} ({orders} orders, {traders} traders/book)");
        compare_splits(&label, &refs, 250, 20);
        compare_splits(&label, &refs, 400, 12);
    }
    let (books, traders) = spread_books(1000, 5000, 200);
    let refs: Vec<&OrderBook> = books.iter().collect();
    compare_splits(&format!("1000 books ({traders} traders/book)"), &refs, 400, 12);
    drop(books);
    let empty: Vec<OrderBook> = (1..=300).map(|m| OrderBook::new(m, fp(1), fp(1))).collect();
    let refs: Vec<&OrderBook> = empty.iter().collect();
    let idx: HashMap<Address, usize> = (0..250).map(|s| (trader(s), s as usize)).collect();
    let cap = std::thread::available_parallelism().map_or(1, |n| n.get());
    for threads in [3, cap] {
        let mut ms: Vec<f64> = (0..41)
            .map(|_| {
                let t0 = Instant::now();
                open_order_counts(&refs, &idx, threads, 0);
                t0.elapsed().as_secs_f64() * 1e3
            })
            .collect();
        ms.sort_by(f64::total_cmp);
        println!(
            "bare spawn/join of {threads} workers (empty books): min {:.3}  median {:.3}  \
             p90 {:.3} ms  load {}",
            ms[0],
            ms[20],
            ms[36],
            loadavg()
        );
    }
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
