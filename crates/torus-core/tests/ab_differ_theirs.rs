//! AB-DIFFER (c): standalone timing of `order_book_store::save_book_delta`
//! (journal-in-book idiom) at depth D for an N-op block — the counterpart to
//! our (b) `save_book_rows_journaled` in ab_differ_microbench.rs.
//!
//! Both time the DIFFER writing rows into a buffering `NativeStateOverlay`
//! (no RocksDB flush inside the timed section), so (b) vs (c) is apples-to-apples.
//! MICROBENCH wall-clock, VPS only, release, no devnet.
//!
//!   AB_DIFFER=1 cargo test -p torus-core --release --test ab_differ_theirs -- --ignored --nocapture

use std::time::Instant;

use alloy_primitives::Address;
use torus_core::order_book::OrderBook;
use torus_core::order_book_store::{save_book_delta, save_book_full};
use torus_state::{NativeStateOverlay, StateDb};
use torus_types::{FixedPoint, OrderType, PlaceOrderParams, TimeInForce};

const N_MARKETS: u64 = 10;
const BLOCK_N: usize = 400;

fn fp(v: i64) -> FixedPoint {
    FixedPoint::from_raw(v as i128 * FixedPoint::SCALE)
}
fn addr(n: u8) -> Address {
    Address::new([n; 20])
}
fn depths() -> Vec<usize> {
    std::env::var("AB_DEPTHS")
        .ok()
        .map(|s| s.split(',').filter_map(|x| x.trim().parse().ok()).collect::<Vec<_>>())
        .filter(|v: &Vec<usize>| !v.is_empty())
        .unwrap_or_else(|| vec![16_000, 200_000, 700_000])
}

fn build_book(market_id: u64, orders: usize) -> (OrderBook, Vec<u128>) {
    let mut book = OrderBook::new(market_id, fp(1), fp(1));
    let mut ids = Vec::with_capacity(orders);
    let levels: i64 = 512;
    for i in 0..orders {
        let r = book.place_order(
            PlaceOrderParams {
                market_id,
                is_buy: true,
                price: fp((i as i64 % levels) + 1),
                quantity: fp(10),
                order_type: OrderType::Limit,
                time_in_force: TimeInForce::GTC,
                reduce_only: false,
                client_order_id: None,
            },
            addr((i % 200) as u8 + 1),
            1000,
        );
        ids.push(r.order_id);
    }
    (book, ids)
}

fn place_new(book: &mut OrderBook, market_id: u64, price: i64) {
    book.place_order(
        PlaceOrderParams {
            market_id,
            is_buy: true,
            price: fp(price),
            quantity: fp(10),
            order_type: OrderType::Limit,
            time_in_force: TimeInForce::GTC,
            reduce_only: false,
            client_order_id: None,
        },
        addr(7),
        1001,
    );
}

fn bench_theirs(d: usize) {
    let per_market = (d / N_MARKETS as usize).max(1);
    let dir = tempfile::tempdir().unwrap();
    let db = StateDb::open(dir.path()).unwrap();
    let ov = NativeStateOverlay::new(db.clone());

    let mut books: Vec<(u64, OrderBook, Vec<u128>)> = Vec::new();
    for m in 0..N_MARKETS {
        let (mut book, ids) = build_book(m, per_market);
        // Seed rows (full write) — populates row_exists, clears journal.
        save_book_full(&ov, &mut book).unwrap();
        books.push((m, book, ids));
    }

    // N-op block: cancels + new places across markets (journal auto-records).
    let mut next_price = 10_000i64;
    for j in 0..BLOCK_N {
        let (m, book, ids) = &mut books[j % N_MARKETS as usize];
        if j % 2 == 0 && !ids.is_empty() {
            let victim = ids[(j * 2_654_435usize) % ids.len()];
            let _ = book.cancel_order(victim);
        } else {
            next_price += 1;
            place_new(book, *m, next_price);
        }
    }

    // Timed: their delta save across all touched markets.
    let t = Instant::now();
    let mut rows = 0usize;
    for (_m, book, _ids) in &mut books {
        let stats = save_book_delta(&ov, book).unwrap();
        rows += stats.rows_written + stats.rows_deleted;
    }
    let secs = t.elapsed().as_secs_f64();
    println!(
        "DIFFER D={d} THEIRS(c) save_book_delta={:.4}ms rows={rows} block_ops={BLOCK_N}",
        secs * 1e3
    );
}

#[test]
#[ignore = "AB-DIFFER (c) microbench — VPS only, release"]
fn ab_differ_theirs() {
    assert_eq!(std::env::var("AB_DIFFER").ok().as_deref(), Some("1"), "set AB_DIFFER=1");
    println!("==== AB-DIFFER THEIRS (c) ====");
    for d in depths() {
        bench_theirs(d);
    }
    println!("==== END ====");
}
