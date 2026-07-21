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

/// THE in-vivo regime: `torus_exec_resting_orders = 195024` at the pegged cell
/// (offered 300k/s ≫ matched 2.4k/s ⇒ ~99% of orders rest, the book grows
/// monotonically). Reproduce it: 10 markets × 40 price levels, one new resting
/// bid per level per block (400 orders/block, no matching), so each level's
/// queue depth == block number. save_books touches all 40 levels every block
/// and re-hashes each over its full (growing) queue — level_row_data is
/// O(orders at level). Watch lvls_us climb toward the in-vivo 107 ms as
/// resting → ~195k. THIS is the attribution.
#[test]
#[ignore = "perf µbench; run with --ignored --nocapture"]
fn savebooks_deep_book_accumulate() {
    let (_d, db) = open_db();
    let mut holder = ResidentBooks::default();
    const LEVELS: i64 = 40;
    hdr("deep book accumulate (resting grows; depth/level == block#)");
    let sample: &[u64] = &[1, 5, 25, 50, 100, 200, 300, 400, 490];
    for h in 1..=500u64 {
        let ov = NativeStateOverlay::new(db.clone());
        let mut ctx = make_ctx(ov.clone(), h, &mut holder);
        if h == 1 {
            fund(&ctx, &addr(1), fp(1_000_000_000_000));
        }
        // 40 bids/market at fixed prices (300..339) — never cross, always rest.
        let mut batch = Vec::with_capacity(400);
        for m in 1..=N_MARKETS {
            for lvl in 0..LEVELS {
                batch.push(place(addr(1), gtc(m, true, 300 + lvl, 1)));
            }
        }
        let _ = NativeExecutor::execute_batch(&mut ctx, &batch);
        let take = sample.contains(&h);
        ctx.collect_save_timings = take;
        let t = Instant::now();
        let w = ctx.save_order_books();
        let us = t.elapsed().as_micros();
        if take {
            row(h, &ctx, us, w);
        }
        ctx.stash_resident(&mut holder);
        ov.flush(&db).unwrap();
    }
}

/// Realistic-depth sweep (multi-trader, bypasses the 200/trader/market cap).
/// Seed DEPTH resting orders at each of 40 levels across all 10 markets (=
/// 400·DEPTH total resting, mirroring the in-vivo 195k), then in the next block
/// touch every level once and time the save. save_books re-hashes 400 levels ×
/// DEPTH orders (level_row_data is O(orders at level)) — the whole span.
/// Run single-threaded (`--test-threads=1`) for uncontended numbers.
#[test]
#[ignore = "perf µbench; run with --ignored --nocapture"]
fn savebooks_depth_sweep() {
    const LEVELS: i64 = 40;
    println!("\n=== realistic depth sweep (10 mkts × 40 levels; touch all levels) ===");
    println!(
        "{:>6} | {:>9} | {:>8} {:>8} {:>8} {:>8} | {:>9}",
        "depth", "resting", "rows_us", "lvls_us", "stops_us", "meta_us", "SAVE_us"
    );
    println!("{}", "-".repeat(72));
    for depth in [1usize, 5, 25, 50, 100, 200, 400] {
        let (_d, db) = open_db();
        let mut holder = ResidentBooks::default();
        // Fund a trader pool big enough for the deepest seed (≤200/trader/market).
        let n_traders = ((LEVELS as usize * depth).div_ceil(200)).max(1) as u16 + 1;
        // Block 1: seed. trader index rotates every 200 orders within a market.
        let ov = NativeStateOverlay::new(db.clone());
        let mut ctx = make_ctx(ov.clone(), 1, &mut holder);
        for t in 1..=n_traders {
            fund(&ctx, &addr(t as u8), fp(1_000_000_000_000));
        }
        let mut seed = Vec::new();
        for m in 1..=N_MARKETS {
            let mut slot = 0usize;
            for lvl in 0..LEVELS {
                for _ in 0..depth {
                    let t = (slot / 200) as u16 + 1;
                    slot += 1;
                    seed.push(place(addr(t as u8), gtc(m, true, 300 + lvl, 1)));
                }
            }
        }
        let _ = NativeExecutor::execute_batch(&mut ctx, &seed);
        let resting = ctx.resting_order_count();
        ctx.save_order_books();
        ctx.stash_resident(&mut holder);
        ov.flush(&db).unwrap();
        // Block 2: touch every level once (one fresh order/level/market).
        let ov = NativeStateOverlay::new(db.clone());
        let mut ctx = make_ctx(ov.clone(), 2, &mut holder);
        fund(&ctx, &addr(255), fp(1_000_000_000_000));
        let mut touch = Vec::new();
        for m in 1..=N_MARKETS {
            for lvl in 0..LEVELS {
                touch.push(place(addr(255), gtc(m, true, 300 + lvl, 1)));
            }
        }
        let _ = NativeExecutor::execute_batch(&mut ctx, &touch);
        ctx.collect_save_timings = true;
        let t = Instant::now();
        ctx.save_order_books();
        let us = t.elapsed().as_micros();
        let s = ctx.last_save_timings;
        println!(
            "{:>6} | {:>9} | {:>8} {:>8} {:>8} {:>8} | {:>9}",
            depth,
            resting,
            s.rows_ns / 1000,
            s.levels_ns / 1000,
            s.stops_ns / 1000,
            s.meta_ns / 1000,
            us,
        );
        ov.flush(&db).unwrap();
    }
}

/// L3 level-hash cache A/B (perf/l3-levelhash-cache): the depth-5 / 2000-frame
/// shape (the 106.6 ms attribution cell) under two workloads × cache off/on:
///   append — each block appends 1 fresh resting order at every level (the
///            dominant resting-heavy case; cache-on should collapse lvls_us);
///   invalidate — each block does an in-place qty-decrease ModifyOrder on the
///            FRONT order of every level (every prefix invalidated ⇒ full
///            rehash; cache-on must be parity with cache-off, no regression).
/// Byte-identity is enforced by asserting identical level-row CF bytes
/// between the off and on universes after every block.
#[test]
#[ignore = "perf µbench; run with --ignored --nocapture"]
fn savebooks_levelcache_ab() {
    const LEVELS: i64 = 40;
    const DEPTH: usize = 5;
    // 5 append blocks × 40 levels = 200 orders/market for the append trader —
    // exactly the per-trader-per-market cap; a 6th block would be rejected.
    const MEASURED: u64 = 5;

    // One universe: seed depth-5 books, then MEASURED workload blocks.
    // Returns (per-block lvls_us, per-block SAVE_us, level-row CF dump).
    let run = |cache_mb: usize, invalidate: bool| -> (Vec<u128>, Vec<u128>, Vec<Vec<(Vec<u8>, Vec<u8>)>>) {
        let (_d, db) = open_db();
        let mut holder = ResidentBooks::default();
        // Seed block: DEPTH resting bids at each of LEVELS levels, all markets.
        let ov = NativeStateOverlay::new(db.clone());
        let mut ctx = make_ctx(ov.clone(), 1, &mut holder);
        ctx.level_hash_cache_bytes = cache_mb * 1024 * 1024;
        for t in 1..=3u8 {
            fund(&ctx, &addr(t), fp(1_000_000_000_000));
        }
        fund(&ctx, &addr(255), fp(1_000_000_000_000));
        let mut seed = Vec::new();
        for m in 1..=N_MARKETS {
            let mut slot = 0usize;
            for lvl in 0..LEVELS {
                for _ in 0..DEPTH {
                    let t = (slot / 200) as u8 + 1;
                    slot += 1;
                    seed.push(place(addr(t), gtc(m, true, 300 + lvl, 10)));
                }
            }
        }
        let _ = NativeExecutor::execute_batch(&mut ctx, &seed);
        ctx.save_order_books();
        ctx.stash_resident(&mut holder);
        ov.flush(&db).unwrap();

        let mut lvls_us = Vec::new();
        let mut save_us = Vec::new();
        let mut dumps = Vec::new();
        for b in 1..=MEASURED {
            let ov = NativeStateOverlay::new(db.clone());
            let mut ctx = make_ctx(ov.clone(), 1 + b, &mut holder);
            ctx.level_hash_cache_bytes = cache_mb * 1024 * 1024;
            let mut batch = Vec::new();
            if invalidate {
                // In-place qty decrease on the FRONT order of every level.
                // Seed ids are sequential in batch order starting at 1.
                for m in 1..=N_MARKETS {
                    for lvl in 0..LEVELS {
                        let front_id: u128 =
                            ((m - 1) as u128) * (LEVELS as u128) * (DEPTH as u128)
                                + (lvl as u128) * (DEPTH as u128)
                                + 1;
                        batch.push((
                            addr(1),
                            torus_types::NativeAction::ModifyOrder {
                                order_id: front_id,
                                new_price: None,
                                new_qty: Some(fp(10 - b as i64)),
                            },
                        ));
                    }
                }
            } else {
                // Append one fresh resting order at every level.
                for m in 1..=N_MARKETS {
                    for lvl in 0..LEVELS {
                        batch.push(place(addr(255), gtc(m, true, 300 + lvl, 1)));
                    }
                }
            }
            let _ = NativeExecutor::execute_batch(&mut ctx, &batch);
            ctx.collect_save_timings = true;
            let t = Instant::now();
            ctx.save_order_books();
            let us = t.elapsed().as_micros();
            lvls_us.push(ctx.last_save_timings.levels_ns / 1000);
            save_us.push(us);
            ctx.stash_resident(&mut holder);
            ov.flush(&db).unwrap();
            // Level rows are root-CF entries with 26-byte keys, tag 0x03.
            let rows: Vec<(Vec<u8>, Vec<u8>)> =
                torus_state::StateBackend::iterate_cf(
                    &db,
                    torus_state::cf::CF_NATIVE_ORDER_BOOKS,
                    None,
                )
                .unwrap()
                .into_iter()
                .filter(|(k, _)| k.len() == 26 && k[8] == 0x03)
                .collect();
            dumps.push(rows);
        }
        (lvls_us, save_us, dumps)
    };

    for (label, invalidate) in [("append-heavy", false), ("worst-case invalidate", true)] {
        let (off_lvls, off_save, off_dumps) = run(0, invalidate);
        let (on_lvls, on_save, on_dumps) = run(64, invalidate);
        assert_eq!(
            off_dumps, on_dumps,
            "{label}: level-row CF bytes diverged between cache off/on"
        );
        // The workload must actually be changing level rows every block
        // (guards against silently-failing ModifyOrder targets).
        for w in off_dumps.windows(2) {
            assert_ne!(w[0], w[1], "{label}: workload block changed no level row");
        }
        println!("\n=== levelcache A/B — {label} (depth {DEPTH}, {LEVELS} lvls × {N_MARKETS} mkts) ===");
        println!(
            "{:>4} | {:>10} {:>10} | {:>10} {:>10}",
            "blk", "off_lvls", "on_lvls", "off_SAVE", "on_SAVE"
        );
        println!("{}", "-".repeat(56));
        for i in 0..off_lvls.len() {
            println!(
                "{:>4} | {:>10} {:>10} | {:>10} {:>10}",
                i + 1,
                off_lvls[i],
                on_lvls[i],
                off_save[i],
                on_save[i]
            );
        }
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
