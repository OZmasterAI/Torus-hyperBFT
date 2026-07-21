//! L3 save-books attribution µbench (perf/l3-savebooks, TEMPORARY).
//!
//! Times `save_order_books` broken into its four mode-2 (LevelAuthority)
//! per-market sub-steps (row journal, level journal+hash, stop-row diff, meta
//! read), OVERLAY-BACKED exactly like production (`app.rs` builds a
//! `NativeStateOverlay`, so save-time `put_cf_raw` is a cheap in-RAM insert and
//! the real RocksDB writes are deferred to the SEPARATE `exec_flush` span — a
//! StateDb-direct ctx would wrongly bill those writes to save_books).
//!
//! Three probes:
//!   `savebooks_cell_shape`   — the pegged cap-400 cell: 10 markets, ~400
//!                              orders/block, heavy matching, resting≈0.
//!   `savebooks_depth_sweep`  — level_row_data is O(orders at touched level);
//!                              sweep standing depth to characterize the hash.
//!   `savebooks_read_probe`   — isolate the two per-market RocksDB reads.
//!
//! Run: `cargo test -p torus-bridge --test l3_savebooks_ubench -- --ignored --nocapture`

use std::time::Instant;

use alloy_primitives::Address;

use torus_bridge::native_executor::{BookMode, NativeExecContext, NativeExecutor, ResidentBooks};
use torus_core::position::NativeBalance;
use torus_state::{NativeStateOverlay, StateDb};
use torus_types::{
    FixedPoint, MarketId, NativeAction, OrderType, PlaceOrderParams, TimeInForce,
};

const N_MARKETS: u64 = 10;

fn open_db() -> (tempfile::TempDir, StateDb) {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = StateDb::open(dir.path()).expect("open db");
    (dir, db)
}

fn addr(n: u8) -> Address {
    Address::new([n; 20])
}
fn fp(v: i64) -> FixedPoint {
    FixedPoint::from_raw(v as i128 * FixedPoint::SCALE)
}

/// Overlay-backed ctx (production shape). Returns the ctx plus a cloned overlay
/// handle (Arc-shared pending) for flushing to the db after save.
fn make_ctx<'a>(
    ov: NativeStateOverlay,
    height: u64,
    holder: &'a mut ResidentBooks,
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
        BookMode::LevelAuthority,
        Some(holder),
    )
}

fn fund(ctx: &NativeExecContext<NativeStateOverlay>, t: &Address, amt: FixedPoint) {
    let bal = NativeBalance { available: amt, order_margin: FixedPoint::ZERO };
    ctx.positions.put_native_balance(t, &bal).unwrap();
}

fn place(sender: Address, p: PlaceOrderParams) -> (Address, NativeAction) {
    (sender, NativeAction::PlaceOrder(p))
}
fn gtc(m: MarketId, is_buy: bool, price: i64, qty: i64) -> PlaceOrderParams {
    PlaceOrderParams {
        market_id: m,
        is_buy,
        price: fp(price),
        quantity: fp(qty),
        order_type: OrderType::Limit,
        time_in_force: TimeInForce::GTC,
        reduce_only: false,
        client_order_id: None,
    }
}

fn hdr(title: &str) {
    println!("\n=== {title} ===");
    println!(
        "{:>5} | {:>4} | {:>8} {:>8} {:>8} {:>8} | {:>9} | {:>6} {:>7}",
        "blk", "mkt", "rows_us", "lvls_us", "stops_us", "meta_us", "SAVE_us", "writes", "resting"
    );
    println!("{}", "-".repeat(84));
}
fn row(h: u64, ctx: &NativeExecContext<NativeStateOverlay>, save_us: u128, writes: usize) {
    let s = ctx.last_save_timings;
    println!(
        "{:>5} | {:>4} | {:>8} {:>8} {:>8} {:>8} | {:>9} | {:>6} {:>7}",
        h,
        s.markets,
        s.rows_ns / 1000,
        s.levels_ns / 1000,
        s.stops_ns / 1000,
        s.meta_ns / 1000,
        save_us,
        writes,
        ctx.resting_order_count(),
    );
}

/// THE cell: resting≈0. Per market: place 20 sells then 20 buys at the same
/// prices so they fully cross in-block (different senders → no STP). Direction
/// alternates each block so positions oscillate near 0 (funding never depletes).
/// ~400 orders/block, ~200 trades, resting drains to 0 every block.
fn cell_batch(h: u64) -> Vec<(Address, NativeAction)> {
    let a = addr(1);
    let b = addr(2);
    let (maker, taker) = if h % 2 == 0 { (a, b) } else { (b, a) };
    let mut batch = Vec::with_capacity(400);
    for m in 1..=N_MARKETS {
        // maker rests 20 asks across 20 levels; taker buys them all out.
        for lvl in 0..20i64 {
            batch.push(place(maker, gtc(m, false, 100 + lvl, 2)));
        }
        for lvl in 0..20i64 {
            batch.push(place(taker, gtc(m, true, 100 + lvl, 2)));
        }
    }
    batch
}

#[test]
#[ignore = "perf µbench; run with --ignored --nocapture"]
fn savebooks_cell_shape() {
    let (_d, db) = open_db();
    let mut holder = ResidentBooks::default();
    hdr("cell shape (resting≈0, overlay-backed = production save_books span)");
    let sample: &[u64] = &[2, 5, 10, 50, 100, 300, 600, 999];
    for h in 1..=1000u64 {
        let ov = NativeStateOverlay::new(db.clone());
        let mut ctx = make_ctx(ov.clone(), h, &mut holder);
        if h == 1 {
            fund(&ctx, &addr(1), fp(5_000_000_000));
            fund(&ctx, &addr(2), fp(5_000_000_000));
        }
        let _ = NativeExecutor::execute_batch(&mut ctx, &cell_batch(h));
        let take = sample.contains(&h);
        ctx.collect_save_timings = take;
        let t = Instant::now();
        let w = ctx.save_order_books();
        let us = t.elapsed().as_micros();
        if take {
            row(h, &ctx, us, w);
        }
        ctx.stash_resident(&mut holder);
        ov.flush(&db).unwrap(); // defer real writes to "flush", grow the LSM
    }
}

/// Characterize level_row_data (O(orders at touched level)): seed a standing
/// book of DEPTH orders per level, then touch one order at each level and time
/// the resulting re-hash. Shows the scaling hazard when resting depth is NOT 0.
#[test]
#[ignore = "perf µbench; run with --ignored --nocapture"]
fn savebooks_depth_sweep() {
    println!("\n=== level-hash depth sweep (single market, touch 1 order/level) ===");
    println!("{:>6} | {:>7} | {:>9} | {:>10}", "depth", "levels", "lvls_us", "us/level");
    println!("{}", "-".repeat(44));
    for depth in [1usize, 2, 4, 8, 16, 32, 64] {
        let (_d, db) = open_db();
        let mut holder = ResidentBooks::default();
        let levels = 20i64;
        // Block 1: seed `depth` resting bids at each of `levels` price levels.
        let ov = NativeStateOverlay::new(db.clone());
        let mut ctx = make_ctx(ov.clone(), 1, &mut holder);
        fund(&ctx, &addr(1), fp(50_000_000_000));
        let mut seed = Vec::new();
        for lvl in 0..levels {
            for _ in 0..depth {
                seed.push(place(addr(1), gtc(1, true, 50 + lvl, 2)));
            }
        }
        let _ = NativeExecutor::execute_batch(&mut ctx, &seed);
        ctx.save_order_books();
        ctx.stash_resident(&mut holder);
        ov.flush(&db).unwrap();
        // Block 2: add ONE order to each level (touches the level → full re-hash).
        let ov = NativeStateOverlay::new(db.clone());
        let mut ctx = make_ctx(ov.clone(), 2, &mut holder);
        fund(&ctx, &addr(1), fp(50_000_000_000));
        let mut touch = Vec::new();
        for lvl in 0..levels {
            touch.push(place(addr(1), gtc(1, true, 50 + lvl, 2)));
        }
        let _ = NativeExecutor::execute_batch(&mut ctx, &touch);
        ctx.collect_save_timings = true;
        ctx.save_order_books();
        let s = ctx.last_save_timings;
        let per = s.levels_ns / 1000 / levels as u128;
        println!("{:>6} | {:>7} | {:>9} | {:>10}", depth, levels, s.levels_ns / 1000, per);
        ov.flush(&db).unwrap();
    }
}

/// Isolate the two per-market RocksDB reads (diff_stop_rows seek +
/// write_meta_if_moved point read) vs LSM churn — resting≈0 so writes are ~nil.
#[test]
#[ignore = "perf µbench; run with --ignored --nocapture"]
fn savebooks_read_probe() {
    let (_d, db) = open_db();
    let mut holder = ResidentBooks::default();
    println!("\n=== read probe (10 mkts, resting≈0): does stops/meta grow with churn? ===");
    println!("{:>5} | {:>8} | {:>8} | {:>8}", "blk", "stops_us", "meta_us", "SAVE_us");
    println!("{}", "-".repeat(40));
    let cps: &[u64] = &[10, 50, 100, 200, 400, 700, 1000];
    for h in 1..=1000u64 {
        let ov = NativeStateOverlay::new(db.clone());
        let mut ctx = make_ctx(ov.clone(), h, &mut holder);
        if h == 1 {
            fund(&ctx, &addr(1), fp(5_000_000_000));
            fund(&ctx, &addr(2), fp(5_000_000_000));
        }
        let _ = NativeExecutor::execute_batch(&mut ctx, &cell_batch(h));
        let take = cps.contains(&h);
        ctx.collect_save_timings = take;
        let t = Instant::now();
        let _ = ctx.save_order_books();
        let us = t.elapsed().as_micros();
        if take {
            let s = ctx.last_save_timings;
            println!("{:>5} | {:>8} | {:>8} | {:>8}", h, s.stops_ns / 1000, s.meta_ns / 1000, us);
        }
        ctx.stash_resident(&mut holder);
        ov.flush(&db).unwrap();
    }
}
