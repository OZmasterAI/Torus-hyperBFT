//! L3 save-books attribution µbench (perf/l3-savebooks, TEMPORARY).
//!
//! Reproduces the pegged cap-400 cell shape in isolation — 10 markets, ~400
//! orders/block, heavy matching, resting≈0 — under mode-2 (LevelAuthority) +
//! resident books, and times `save_order_books` broken into its four
//! per-market sub-steps (row journal, level journal+hash, stop-row diff, meta
//! read). The point is UNCONTENDED numbers plus the growth curve as
//! CF_NATIVE_ORDER_BOOKS fills with level-row tombstones, so the deschedule
//! share of the ~107 ms in-vivo wall can be computed and the read-amplification
//! hypothesis (per-market RocksDB seek + point read every block) tested.
//!
//! Run: `cargo test -p torus-bridge --test l3_savebooks_ubench -- --ignored --nocapture`

use std::time::Instant;

use alloy_primitives::Address;

use torus_bridge::native_executor::{BookMode, NativeExecContext, NativeExecutor, ResidentBooks};
use torus_core::position::NativeBalance;
use torus_state::StateDb;
use torus_types::{
    FixedPoint, MarketId, NativeAction, OrderType, PlaceOrderParams, TimeInForce,
};

const N_MARKETS: u64 = 10;

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

fn make_ctx(
    db: StateDb,
    height: u64,
    holder: &mut ResidentBooks,
) -> NativeExecContext {
    NativeExecContext::new_with_mode(
        db,
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

fn fund(ctx: &NativeExecContext, trader: &Address, amount: FixedPoint) {
    let bal = NativeBalance {
        available: amount,
        order_margin: FixedPoint::ZERO,
    };
    ctx.positions.put_native_balance(trader, &bal).unwrap();
}

fn place(sender: Address, p: PlaceOrderParams) -> (Address, NativeAction) {
    (sender, NativeAction::PlaceOrder(p))
}

fn gtc(market_id: MarketId, is_buy: bool, price: i64, qty: i64) -> PlaceOrderParams {
    PlaceOrderParams {
        market_id,
        is_buy,
        price: fp(price),
        quantity: fp(qty),
        order_type: OrderType::Limit,
        time_in_force: TimeInForce::GTC,
        reduce_only: false,
        client_order_id: None,
    }
}

/// Build one block's batch across all 10 markets. `rest_side` alternates: on
/// even blocks the maker rests fresh bids across 20 price levels (writes level
/// rows); on odd blocks the taker sweeps the previous block's bids (deletes
/// level rows → tombstones) AND rests a fresh band. Net: ~400 orders/block,
/// heavy matching, resting oscillates near-zero, and CF_NATIVE_ORDER_BOOKS
/// churns level-row writes+deletes every block — the in-vivo signature.
fn block_batch(rest_bids: bool) -> Vec<(Address, NativeAction)> {
    let maker = addr(1);
    let taker = addr(2);
    let mut batch = Vec::with_capacity(400);
    for m in 1..=N_MARKETS {
        if rest_bids {
            // 20 resting bids spread over 20 levels (90..109) — these persist.
            for lvl in 0..20i64 {
                batch.push(place(maker, gtc(m, true, 90 + lvl, 2)));
            }
            // 20 resting asks above the book (200..219) — also persist.
            for lvl in 0..20i64 {
                batch.push(place(maker, gtc(m, false, 200 + lvl, 2)));
            }
        } else {
            // 20 asks that sweep the previous block's bids (90..109) — matches,
            // deletes those bid level rows. 20 buys sweeping the resting asks
            // (200..219). Fully crossing → resting drains to ~0.
            for lvl in 0..20i64 {
                batch.push(place(taker, gtc(m, false, 90 + lvl, 2)));
            }
            for lvl in 0..20i64 {
                batch.push(place(taker, gtc(m, true, 200 + lvl, 2)));
            }
        }
    }
    batch
}

#[test]
#[ignore = "perf µbench; run explicitly with --ignored --nocapture"]
fn savebooks_span_breakdown() {
    let (_dir, db) = open_test_db();
    let mut holder = ResidentBooks::default();

    // Warm/churn for many blocks; sample the save breakdown at a spread of
    // heights to expose growth as the CF's memtable/LSM fills with tombstones.
    const BLOCKS: u64 = 600;
    let sample_at: &[u64] = &[2, 5, 10, 25, 50, 100, 200, 300, 400, 500, 599];

    println!(
        "\n{:>5} | {:>6} | {:>8} {:>8} {:>8} {:>8} {:>8} | {:>9} | {:>6} {:>7}",
        "blk", "mkts", "rows_us", "lvls_us", "stops_us", "meta_us", "other_us",
        "TOTAL_us", "writes", "resting"
    );
    println!("{}", "-".repeat(96));

    for h in 1..=BLOCKS {
        let mut ctx = make_ctx(db.clone(), h, &mut holder);
        if h == 1 {
            fund(&ctx, &addr(1), fp(1_000_000_000));
            fund(&ctx, &addr(2), fp(1_000_000_000));
        }
        let batch = block_batch(h % 2 == 0);
        let _ = NativeExecutor::execute_batch(&mut ctx, &batch);

        let sample = sample_at.contains(&h);
        ctx.collect_save_timings = sample;

        let t = Instant::now();
        let writes = ctx.save_order_books();
        let total_us = t.elapsed().as_micros();
        let resting = ctx.resting_order_count();

        if sample {
            let s = ctx.last_save_timings;
            println!(
                "{:>5} | {:>6} | {:>8} {:>8} {:>8} {:>8} {:>8} | {:>9} | {:>6} {:>7}",
                h,
                s.markets,
                s.rows_ns / 1000,
                s.levels_ns / 1000,
                s.stops_ns / 1000,
                s.meta_ns / 1000,
                s.other_ns / 1000,
                total_us,
                writes,
                resting,
            );
        }
        ctx.stash_resident(&mut holder);
    }
    println!();
}

/// Isolate the pure READ cost: after churning the CF, time save_order_books on
/// a block whose books have ~0 dirty writes. If save time is dominated by
/// stops+meta and grows with churn, the per-market RocksDB reads are the cause.
#[test]
#[ignore = "perf µbench; run explicitly with --ignored --nocapture"]
fn savebooks_read_cost_vs_churn() {
    let (_dir, db) = open_test_db();
    let mut holder = ResidentBooks::default();

    println!("\nchurn_blocks | stops_us | meta_us | rows_us | lvls_us | total_us (10 mkts, ~0 net writes)");
    println!("{}", "-".repeat(90));

    let checkpoints: &[u64] = &[10, 50, 100, 200, 400, 600, 800];
    let mut next_cp = 0usize;

    for h in 1..=800u64 {
        let mut ctx = make_ctx(db.clone(), h, &mut holder);
        if h == 1 {
            fund(&ctx, &addr(1), fp(2_000_000_000));
            fund(&ctx, &addr(2), fp(2_000_000_000));
        }
        let batch = block_batch(h % 2 == 0);
        let _ = NativeExecutor::execute_batch(&mut ctx, &batch);

        let at_cp = next_cp < checkpoints.len() && checkpoints[next_cp] == h;
        ctx.collect_save_timings = at_cp;
        let t = Instant::now();
        let _ = ctx.save_order_books();
        let total_us = t.elapsed().as_micros();
        if at_cp {
            let s = ctx.last_save_timings;
            println!(
                "{:>12} | {:>8} | {:>7} | {:>7} | {:>7} | {:>8}",
                h,
                s.stops_ns / 1000,
                s.meta_ns / 1000,
                s.rows_ns / 1000,
                s.levels_ns / 1000,
                total_us,
            );
            next_cp += 1;
        }
        ctx.stash_resident(&mut holder);
    }
    println!();
}
