//! s74 boot-profile µbench: the cold order-book load a restarted node pays on
//! its first replayed block (`torus_exec_load_books_seconds` 4.3-4.7 s at
//! ~1.0M resting orders in the s74 crash cells).
//!
//! Builds a real mode-3 (`LevelAuthorityChunked`, the bench's
//! `TORUS_BOOK_ROWS=3`) book store through the executor, overlay-backed like
//! production, flushes it, then constructs a fresh ctx with an EMPTY resident
//! holder — which runs `load_order_books_levels` — and prints its phase split
//! (`NativeExecContext::load_timings`).
//!
//! Run in RELEASE (debug-build numbers were ~55x off in s450); compare
//! `TORUS_LOAD_BOOKS_WORKERS=1` (serial) against the default (host threads):
//!   cargo test --release -p torus-bridge --test load_books_ubench -- --ignored --nocapture

use std::time::Instant;

use alloy_primitives::Address;

use torus_bridge::native_executor::{BookMode, NativeExecContext, NativeExecutor, ResidentBooks};
use torus_core::position::NativeBalance;
use torus_state::{NativeStateOverlay, StateDb};
use torus_types::{FixedPoint, NativeAction, OrderType, PlaceOrderParams, TimeInForce};

const N_MARKETS: u64 = 10;
/// Distinct bid price levels per market.
const LEVELS: u64 = 400;
/// `MAX_ORDERS_PER_TRADER_PER_MARKET`.
const PER_TRADER: u64 = 200;
/// Traders placing per block (x 200 orders x 10 markets = 20k orders/block).
const TRADERS_PER_BLOCK: u64 = 10;

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

/// A mode-3 store with `per_market` resting bids in each of the 10 markets.
fn build(per_market: u64) -> (tempfile::TempDir, StateDb) {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = StateDb::open(dir.path()).expect("open db");
    let mut holder = ResidentBooks::default();
    let traders = per_market / PER_TRADER;
    for (block, first) in (0..traders).step_by(TRADERS_PER_BLOCK as usize).enumerate() {
        let ov = NativeStateOverlay::new(db.clone());
        let mut c = ctx(ov.clone(), block as u64 + 1, &mut holder);
        let mut batch = Vec::new();
        for t in first..(first + TRADERS_PER_BLOCK).min(traders) {
            let who = trader(t);
            let funds = NativeBalance { available: fp(1_000_000_000_000), order_margin: FixedPoint::ZERO };
            c.positions.put_native_balance(&who, &funds).unwrap();
            for market_id in 1..=N_MARKETS {
                for k in 0..PER_TRADER {
                    let price = 300 + (t * PER_TRADER + k) % LEVELS;
                    batch.push((
                        who,
                        NativeAction::PlaceOrder(PlaceOrderParams {
                            market_id,
                            is_buy: true,
                            price: fp(price),
                            quantity: fp(1),
                            order_type: OrderType::Limit,
                            time_in_force: TimeInForce::GTC,
                            reduce_only: false,
                            client_order_id: None,
                        }),
                    ));
                }
            }
        }
        let _ = NativeExecutor::execute_batch(&mut c, &batch);
        c.save_order_books();
        c.stash_resident(&mut holder);
        ov.flush(&db).unwrap();
    }
    (dir, db)
}

fn profile(per_market: u64) {
    let t = Instant::now();
    let (_dir, db) = build(per_market);
    println!(
        "\n=== {} markets x {} resting bids over {} levels (built in {:.1} s) ===",
        N_MARKETS,
        per_market,
        LEVELS,
        t.elapsed().as_secs_f64()
    );
    println!(
        "{:>3} | {:>8} {:>7} | {:>7} {:>8} {:>11} {:>10} {:>9} | {:>8}",
        "rep", "orders", "levels", "root_ms", "store_ms", "rebuild_cpu", "verify_cpu", "books_ms", "total_ms"
    );
    for rep in 0..3 {
        let mut empty = ResidentBooks::default();
        let t = Instant::now();
        let c = ctx(NativeStateOverlay::new(db.clone()), 1_000_000, &mut empty);
        let total = t.elapsed().as_millis();
        let lt = c.load_timings;
        assert_eq!(lt.orders, per_market * N_MARKETS, "every resting order reloaded");
        assert_eq!(c.resting_order_count() as u64, per_market * N_MARKETS);
        let ms = |ns: u128| ns / 1_000_000;
        println!(
            "{:>3} | {:>8} {:>7} | {:>7} {:>8} {:>11} {:>10} {:>9} | {:>8}",
            rep,
            lt.orders,
            lt.levels,
            ms(lt.root_scan_ns),
            ms(lt.store_scan_ns),
            ms(lt.rebuild_ns),
            ms(lt.verify_ns),
            ms(lt.books_wall_ns),
            total
        );
    }
}

#[test]
#[ignore = "perf µbench; run with --release -- --ignored --nocapture"]
fn load_books_boot_profile() {
    for per_market in [10_000, 100_000] {
        profile(per_market);
    }
}
