//! Classic parallel book save µbench (exp/archy-s58-parallel-classic-book-save,
//! `TORUS_PARALLEL_BOOK_SAVE`). Gate before any bench cell: the parallel
//! 300-book save must take <= 40% of the serial save.
//!
//! Production shape: overlay-backed `NativeStateOverlay` ctx in `Classic`
//! mode with a resident-books holder (a fresh overlay and ctx per "block",
//! as `app.rs`), every book touched once per block (one new resting order,
//! so the journals hold a block's worth of entries) and marked dirty. Each
//! rep times `save_order_books()` serial (flag pinned off) and parallel
//! (pinned on, gate 0), interleaved so drift hits both alike.
//!
//! Shapes:
//!   `classic_save_300m` — 300 books x ~2.3k resting orders (the 300m 600 s
//!                         cell late: ~690k resting).
//!   `classic_save_10m`  — 10 books x ~64k resting orders.
//!
//! Run (release, one test at a time), with jemalloc preloaded so the
//! allocator matches the node's (glibc page-faults every fresh blob buffer
//! and roughly doubles the serial 300-book save; jemalloc reproduces the
//! production 21-26 ms):
//! `cargo test --release -p torus-bridge --test classic_book_save_ubench --no-run`, then
//! `LD_PRELOAD=/usr/lib/libjemalloc.so.2 <test binary> --ignored --nocapture --test-threads=1`
//!
//! archy, 2026-10-15, jemalloc preloaded, sibling builds running (load ~100):
//! 300 books x 2.3k: serial 25.0-26.0 ms, parallel 9.1-9.2 ms (35.5-36.9%,
//! gate PASS); 10 books x 64k: serial 20.0-20.6 ms, parallel 8.3 ms
//! (40.3-41.7%). glibc: 300 books 49-61 ms -> 8.7-11.4 ms.

use std::time::Instant;

use alloy_primitives::Address;

use torus_bridge::native_executor::{
    with_parallel_book_save, BookMode, NativeExecContext, ResidentBooks,
};
use torus_core::order_book::OrderBook;
use torus_state::{NativeStateOverlay, StateDb};
use torus_types::{FixedPoint, MarketId, OrderType, PlaceOrderParams, TimeInForce};

const REPS: usize = 20;
const WARMUP: usize = 2;
/// Price levels per side (bids 1..=LEVELS below 10_000, asks above).
const LEVELS: i64 = 100;

fn fp(v: i64) -> FixedPoint {
    FixedPoint::from_raw(v as i128 * FixedPoint::SCALE)
}

/// Many distinct traders (orders spread as in a bench cell).
fn trader(i: u32) -> Address {
    let mut b = [0x5Au8; 20];
    b[..4].copy_from_slice(&i.to_be_bytes());
    Address::new(b)
}

fn gtc(market: MarketId, is_buy: bool, price: i64) -> PlaceOrderParams {
    PlaceOrderParams {
        market_id: market,
        is_buy,
        price: fp(price),
        quantity: fp(1),
        order_type: OrderType::Limit,
        time_in_force: TimeInForce::GTC,
        reduce_only: false,
        client_order_id: None,
    }
}

/// Non-crossing GTC book: `orders` alternating bids / asks over `LEVELS`
/// levels a side, from rotating traders.
fn build_book(market: MarketId, orders: usize) -> OrderBook {
    let mut book = OrderBook::new(market, fp(1), fp(1));
    for i in 0..orders {
        let lvl = 1 + (i as i64 / 2) % LEVELS;
        let is_buy = i % 2 == 0;
        let price = if is_buy { 10_000 - lvl } else { 10_000 + lvl };
        book.place_order(gtc(market, is_buy, price), trader(i as u32 % 4_096), 1_000);
    }
    assert_eq!(book.order_count(), orders);
    book
}

fn new_ctx(
    db: &StateDb,
    height: u64,
    holder: &mut ResidentBooks,
) -> NativeExecContext<NativeStateOverlay> {
    NativeExecContext::new_with_mode(
        NativeStateOverlay::new(db.clone()),
        height,
        1_000 + height,
        0,
        100,
        10,
        Address::new([99; 20]),
        Address::new([100; 20]),
        Address::new([101; 20]),
        BookMode::Classic,
        Some(holder),
    )
}

fn pct(sorted: &[f64], p: f64) -> f64 {
    sorted[((sorted.len() - 1) as f64 * p).round() as usize]
}

fn run_shape(label: &str, books: u64, orders_per_book: usize) {
    let dir = tempfile::tempdir().unwrap();
    let db = StateDb::open(dir.path()).unwrap();
    let mut holder = ResidentBooks::default();
    let mut height = 1u64;
    {
        let mut ctx = new_ctx(&db, height, &mut holder);
        for m in 1..=books {
            ctx.insert_order_book(m, build_book(m, orders_per_book));
        }
        ctx.stash_resident(&mut holder);
    }
    let blob_mb = {
        let ctx = new_ctx(&db, height + 1, &mut holder);
        height += 1;
        let bytes: usize = ctx
            .order_books()
            .values()
            .map(|b| borsh::to_vec(b).unwrap().len())
            .sum();
        let mut ctx = ctx;
        ctx.stash_resident(&mut holder);
        bytes as f64 / 1e6
    };

    // One "block": fresh overlay + ctx, touch every book once, time the save.
    let mut block = |on: bool, rep: usize| -> (f64, usize, usize) {
        height += 1;
        let mut ctx = new_ctx(&db, height, &mut holder);
        let resting = ctx.resting_order_count();
        let px = 10_000 - LEVELS - 1 - (rep as i64 % 50);
        ctx.for_each_order_book_mut(|m, b| {
            b.place_order(gtc(m, true, px), trader(9_000 + rep as u32), 1_000);
        });
        for m in 1..=books {
            ctx.dirty_books.insert(m);
        }
        let t = Instant::now();
        let written = with_parallel_book_save(on, 0, || ctx.save_order_books());
        let ms = t.elapsed().as_secs_f64() * 1e3;
        assert_eq!(written, books as usize);
        assert!(ctx.fatal_error.is_none(), "{:?}", ctx.fatal_error);
        let workers = ctx.last_save_workers();
        ctx.stash_resident(&mut holder);
        (ms, workers, resting)
    };

    let (mut ser, mut par) = (Vec::new(), Vec::new());
    let mut workers = 0;
    let mut resting = 0;
    for rep in 0..WARMUP + REPS {
        let (s, w1, r) = block(false, rep);
        let (p, w2, _) = block(true, rep);
        assert_eq!(w1, 1);
        workers = w2;
        resting = r;
        if rep >= WARMUP {
            ser.push(s);
            par.push(p);
        }
    }
    ser.sort_by(f64::total_cmp);
    par.sort_by(f64::total_cmp);
    let (ms, mp) = (pct(&ser, 0.5), pct(&par, 0.5));
    println!(
        "\n=== {label}: {books} books x ~{orders_per_book} orders ({resting} resting, {blob_mb:.1} MB of blobs), {REPS} reps ===\n\
         serial   ms: p10 {:>7.2}  median {:>7.2}  p90 {:>7.2}\n\
         parallel ms: p10 {:>7.2}  median {:>7.2}  p90 {:>7.2}  (jobs {workers})\n\
         parallel/serial median = {:.1}%  speedup {:.2}x  gate(<= 40%): {}",
        pct(&ser, 0.1),
        ms,
        pct(&ser, 0.9),
        pct(&par, 0.1),
        mp,
        pct(&par, 0.9),
        100.0 * mp / ms,
        ms / mp,
        if mp <= 0.40 * ms { "PASS" } else { "FAIL" },
    );
}

#[test]
#[ignore = "perf µbench; run with --release --ignored --nocapture --test-threads=1"]
fn classic_save_300m() {
    run_shape("300m late-cell shape", 300, 2_300);
}

#[test]
#[ignore = "perf µbench; run with --release --ignored --nocapture --test-threads=1"]
fn classic_save_10m() {
    run_shape("10m deep-book shape", 10, 64_000);
}
