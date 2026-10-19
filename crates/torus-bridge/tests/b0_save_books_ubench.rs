//! b0 µbench (docs/plans/deep-book-save-options.md section 4): what the exec
//! thread (E) pays for `save_books` in book mode 3 (`LevelAuthorityChunked`,
//! `TORUS_BOOK_ROWS=3`) when checkpoints are on (b0: the whole save on E,
//! `save_order_books`) vs today's per-block flush (the deferred save: pass 1
//! on E, `save_order_books_deferred`; pass 2 on the flush worker W,
//! `apply_deferred_book_save`).
//!
//! Two shapes, 10 markets, 400 bid levels per market:
//!   deep    — 100,000 resting bids per market (1M resting);
//!   shallow — 1,000 resting bids per market (10k resting).
//! Every measured block: per market one taker sell crossing `FILLS` resting
//! bids (qty 1 each) and a maker placing `FILLS` new bids (depth stays put).
//! The two save paths alternate block by block on ONE chain (same books,
//! same state; both write the same bytes), overlay-backed like production
//! and flushed after every block.
//!
//! Run in RELEASE:
//!   cargo test --release -p torus-bridge --test b0_save_books_ubench -- --ignored --nocapture

use std::time::Instant;

use alloy_primitives::Address;

use torus_bridge::native_executor::{
    apply_deferred_book_save, BookMode, NativeExecContext, NativeExecutor, ResidentBooks,
};
use torus_core::position::NativeBalance;
use torus_state::{NativeStateOverlay, StateDb};
use torus_types::{FixedPoint, NativeAction, OrderType, PlaceOrderParams, TimeInForce};

const N_MARKETS: u64 = 10;
const LEVELS: u64 = 400;
/// Orders per trader per market at build time (x 10 markets = the 1000
/// open-order limit).
const PER_TRADER: u64 = 100;
const TRADERS_PER_BLOCK: u64 = 20;
/// Fills per market per measured block (and new bids placed).
const FILLS: u64 = 100;
/// Measured blocks per arm (the arms alternate).
const BLOCKS_PER_ARM: usize = 50;

fn addr(n: u8) -> Address {
    Address::new([n; 20])
}

fn trader(i: u64) -> Address {
    let mut bytes = [0xAA; 20];
    bytes[..8].copy_from_slice(&(i + 1).to_be_bytes());
    Address::new(bytes)
}

fn fp(v: u64) -> FixedPoint {
    FixedPoint::from_raw(v as i128 * FixedPoint::SCALE)
}

fn ctx(
    ov: NativeStateOverlay,
    height: u64,
    holder: &mut ResidentBooks,
) -> NativeExecContext<NativeStateOverlay> {
    NativeExecContext::new_with_mode(
        ov,
        height,
        1000 + height,
        0,
        100,
        10,
        addr(99),
        addr(100),
        addr(101),
        BookMode::LevelAuthorityChunked,
        Some(holder),
    )
}

fn order(market_id: u64, is_buy: bool, price: u64, qty: u64) -> NativeAction {
    NativeAction::PlaceOrder(PlaceOrderParams {
        market_id,
        is_buy,
        price: fp(price),
        quantity: fp(qty),
        order_type: OrderType::Limit,
        time_in_force: TimeInForce::GTC,
        reduce_only: false,
        client_order_id: None,
    })
}

fn fund(c: &NativeExecContext<NativeStateOverlay>, who: &Address) {
    let funds = NativeBalance {
        available: fp(1_000_000_000_000),
        order_margin: FixedPoint::ZERO,
    };
    c.positions.put_native_balance(who, &funds).unwrap();
}

/// A mode-3 store with `per_market` resting bids in each market; returns the
/// DB, the warm resident holder and the next height.
fn build(per_market: u64) -> (tempfile::TempDir, StateDb, ResidentBooks, u64) {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = StateDb::open(dir.path()).expect("open db");
    let mut holder = ResidentBooks::default();
    let traders = per_market / PER_TRADER;
    let mut height = 1;
    for first in (0..traders).step_by(TRADERS_PER_BLOCK as usize) {
        let ov = NativeStateOverlay::new(db.clone());
        let mut c = ctx(ov.clone(), height, &mut holder);
        let mut batch = Vec::new();
        for t in first..(first + TRADERS_PER_BLOCK).min(traders) {
            let who = trader(t);
            fund(&c, &who);
            for market_id in 1..=N_MARKETS {
                for k in 0..PER_TRADER {
                    batch.push((
                        who,
                        order(market_id, true, 300 + (t * PER_TRADER + k) % LEVELS, 1),
                    ));
                }
            }
        }
        let _ = NativeExecutor::execute_batch(&mut c, &batch);
        c.save_order_books();
        assert_eq!(c.take_fatal_error(), None);
        c.stash_resident(&mut holder);
        ov.flush(&db).unwrap();
        height += 1;
    }
    (dir, db, holder, height)
}

fn stats(ms: &mut [f64]) -> (f64, f64, f64) {
    ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let mean = ms.iter().sum::<f64>() / ms.len() as f64;
    (mean, ms[ms.len() / 2], ms[ms.len() * 9 / 10])
}

fn profile(shape: &str, per_market: u64) {
    let t = Instant::now();
    let (_dir, db, mut holder, mut height) = build(per_market);
    println!(
        "\n=== {shape}: {N_MARKETS} markets x {per_market} resting bids over {LEVELS} levels \
         (built in {:.1} s); {} fills per block ===",
        t.elapsed().as_secs_f64(),
        FILLS * N_MARKETS
    );
    let taker = addr(7);
    let (mut b0_e, mut def_e, mut def_w) = (Vec::new(), Vec::new(), Vec::new());
    let mut resting = 0;
    for i in 0..2 * BLOCKS_PER_ARM {
        let ov = NativeStateOverlay::new(db.clone());
        let mut c = ctx(ov.clone(), height, &mut holder);
        let maker = trader(1_000_000 + i as u64);
        fund(&c, &maker);
        fund(&c, &taker);
        let mut batch = Vec::new();
        for market_id in 1..=N_MARKETS {
            batch.push((taker, order(market_id, false, 1, FILLS)));
            for k in 0..FILLS {
                batch.push((
                    maker,
                    order(market_id, true, 300 + (i as u64 * FILLS + k) % LEVELS, 1),
                ));
            }
        }
        let _ = NativeExecutor::execute_batch(&mut c, &batch);
        // A B A B ...: odd blocks b0 (whole save on E), even blocks deferred.
        if i % 2 == 1 {
            let t = Instant::now();
            c.save_order_books();
            b0_e.push(t.elapsed().as_secs_f64() * 1e3);
        } else {
            let t = Instant::now();
            let save = c.save_order_books_deferred().expect("mode 3 defers");
            def_e.push(t.elapsed().as_secs_f64() * 1e3);
            let t = Instant::now();
            apply_deferred_book_save(&ov, None, save).expect("pass 2");
            def_w.push(t.elapsed().as_secs_f64() * 1e3);
        }
        assert_eq!(c.take_fatal_error(), None);
        resting = c.resting_order_count();
        c.stash_resident(&mut holder);
        ov.flush(&db).unwrap();
        height += 1;
    }
    println!("resting after the run: {resting}");
    println!(
        "{:<34} | {:>8} {:>8} {:>8} | {:>12}",
        "save_books, ms per block", "mean", "p50", "p90", "mean per 1k fills"
    );
    let per_k = |mean: f64| mean / (FILLS * N_MARKETS) as f64 * 1e3;
    for (name, v) in [
        ("b0: E (save_order_books)", &mut b0_e),
        ("deferred: E (pass 1, drain)", &mut def_e),
        ("deferred: W (pass 2, writes)", &mut def_w),
    ] {
        let (mean, p50, p90) = stats(v);
        println!(
            "{name:<34} | {mean:>8.3} {p50:>8.3} {p90:>8.3} | {:>12.4}",
            per_k(mean)
        );
    }
}

#[test]
#[ignore = "perf µbench; run with --release -- --ignored --nocapture"]
fn b0_save_books_on_exec_thread_deep_vs_shallow() {
    profile("shallow", 1_000);
    profile("deep", 100_000);
}
