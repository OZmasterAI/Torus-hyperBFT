//! 8k L2.1 µbench: exec wall of one `execute_batch` call (the forced
//! parallel settle) at ~16.8k fills (s94's standard block) and ~65k fills
//! (the 8n / 8q deep-book block), `TORUS_SETTLE_STREAMED` off vs on, with
//! L2.2 (`TORUS_SETTLE_NO_MERGE`) off and on. 300 markets, a market per
//! order (interleaved), 6,000 traders; each market rests a 20-level ladder
//! of 8 one-lot makers per side, and the measured call is a storm of IOC
//! takers of 3 lots crossing the ladder (3 fills each, alternating sides, so
//! positions open, extend and close). Every repetition builds a fresh DB and
//! ladder (not timed) and times the storm call only; arms interleave ABBA.
//! `#[ignore]` (perf, not a gate); run in release:
//!
//! ```text
//! cargo test --release -p torus-bridge --test ubench_settle_streamed -- --ignored --nocapture
//! ```
//!
//! Env: `UBENCH_REPS` (default 8 per arm and shape).

use std::time::{Duration, Instant};

use alloy_primitives::Address;
use torus_bridge::native_executor::{
    with_settle_no_merge, with_settle_streamed, NativeExecContext, NativeExecutor,
};
use torus_core::position::{NativeBalance, PositionManager};
use torus_state::{NativeStateOverlay, StateDb};
use torus_types::{FixedPoint, MarketId, NativeAction, OrderType, PlaceOrderParams, TimeInForce};

const MARKETS: u64 = 300;
const TRADERS: u32 = 6_000;
const LEVELS: i64 = 20;
const PER_LEVEL: usize = 8;

fn addr(n: u32) -> Address {
    let mut b = [0u8; 20];
    b[..4].copy_from_slice(&(n + 1).to_be_bytes());
    b[19] = 0xA7;
    Address::new(b)
}

fn fp(v: i64) -> FixedPoint {
    FixedPoint::from_raw(v as i128 * FixedPoint::SCALE)
}

fn place(
    sender: Address,
    market_id: MarketId,
    is_buy: bool,
    price: i64,
    qty: i64,
    tif: TimeInForce,
) -> (Address, NativeAction) {
    (
        sender,
        NativeAction::PlaceOrder(PlaceOrderParams {
            market_id,
            is_buy,
            price: fp(price),
            quantity: fp(qty),
            order_type: OrderType::Limit,
            time_in_force: tif,
            reduce_only: false,
            client_order_id: None,
        }),
    )
}

/// The resting ladder: per market, asks at 1,001.. and bids at 999.. (20
/// levels, 8 one-lot makers each), makers round-robin over the traders.
fn ladder() -> Vec<(Address, NativeAction)> {
    let mut out = Vec::new();
    let mut t = 0u32;
    for m in 1..=MARKETS {
        for lvl in 0..LEVELS {
            for _ in 0..PER_LEVEL {
                out.push(place(
                    addr(t % TRADERS),
                    m,
                    false,
                    1_001 + lvl,
                    1,
                    TimeInForce::GTC,
                ));
                t += 7;
                out.push(place(
                    addr(t % TRADERS),
                    m,
                    true,
                    999 - lvl,
                    1,
                    TimeInForce::GTC,
                ));
                t += 7;
            }
        }
    }
    out
}

/// `per_market` IOC takers of 3 lots per market, markets interleaved (a
/// market per order), sides alternating per market, senders round-robin.
fn storm(per_market: usize) -> Vec<(Address, NativeAction)> {
    let mut out = Vec::new();
    let mut t = 3u32;
    for k in 0..per_market {
        for m in 1..=MARKETS {
            let buy = (k + m as usize) % 2 == 0;
            let price = if buy { 1_001 + LEVELS } else { 999 - LEVELS };
            out.push(place(addr(t % TRADERS), m, buy, price, 3, TimeInForce::IOC));
            t += 13;
        }
    }
    out
}

struct Sample {
    wall: Duration,
    fills: usize,
    match_ns: u128,
    settle_ns: u128,
    pass_b_ns: u128,
    fingerprint: (u64, usize, usize),
}

fn one(ladder: &[(Address, NativeAction)], storm: &[(Address, NativeAction)]) -> Sample {
    let dir = tempfile::tempdir().expect("temp dir");
    let db = StateDb::open(dir.path()).expect("open db");
    let pm = PositionManager::new(db.clone());
    for n in 0..TRADERS {
        pm.put_native_balance(
            &addr(n),
            &NativeBalance {
                available: fp(1_000_000_000),
                order_margin: FixedPoint::ZERO,
            },
        )
        .unwrap();
    }
    let overlay = NativeStateOverlay::new(db.clone());
    let mut ctx = NativeExecContext::new(
        overlay.clone(),
        1,
        1_000,
        0,
        100,
        10,
        addr(TRADERS + 1),
        addr(TRADERS + 2),
        addr(TRADERS + 3),
    );
    let r = NativeExecutor::execute_batch_settle_workers(&mut ctx, ladder, true, None);
    assert!(r.results.iter().all(|a| a.success), "the ladder rests");
    assert!(ctx.fatal_error.is_none());
    let fills_before = ctx.take_pending_trade_fills().len();
    assert_eq!(fills_before, 0, "the ladder does not cross");
    let accum = ctx.phase_accum;
    let t = Instant::now();
    let r = NativeExecutor::execute_batch_settle_workers(&mut ctx, storm, true, None);
    let wall = t.elapsed();
    assert!(ctx.fatal_error.is_none());
    let fills = ctx.take_pending_trade_fills().len();
    let ok = r.results.iter().filter(|a| a.success).count();
    Sample {
        wall,
        fills,
        match_ns: ctx.phase_accum.match_ns - accum.match_ns,
        settle_ns: ctx.phase_accum.settle_ns - accum.settle_ns,
        pass_b_ns: ctx.phase_accum.settle_pass_b_ns - accum.settle_pass_b_ns,
        fingerprint: (r.total_gas, ok, overlay.dirty_native_keys().len()),
    }
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = v.len();
    if n % 2 == 1 {
        v[n / 2]
    } else {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    }
}

fn ms(ns: u128) -> f64 {
    ns as f64 / 1e6
}

#[test]
#[ignore]
fn ubench_settle_streamed_exec_wall() {
    let reps: usize = std::env::var("UBENCH_REPS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(8);
    let ladder = ladder();
    println!(
        "L2.1 µbench: {MARKETS} markets, {TRADERS} traders, ladder {} orders, host parallelism {}, reps {reps} per arm",
        ladder.len(),
        std::thread::available_parallelism().map_or(1, |n| n.get())
    );
    for (shape, per_market) in [("16.8k", 19usize), ("65k", 72)] {
        let storm = storm(per_market);
        for no_merge in [false, true] {
            let mut arms: [Vec<Sample>; 2] = [Vec::new(), Vec::new()];
            // Warm-up (pool threads, allocator), one per arm, not counted.
            for on in [false, true] {
                with_settle_no_merge(no_merge, || {
                    with_settle_streamed(on, || one(&ladder, &storm))
                });
            }
            for k in 0..reps {
                // ABBA
                let order = if k % 2 == 0 {
                    [false, true]
                } else {
                    [true, false]
                };
                for on in order {
                    let s = with_settle_no_merge(no_merge, || {
                        with_settle_streamed(on, || one(&ladder, &storm))
                    });
                    arms[usize::from(on)].push(s);
                }
            }
            let fp0 = arms[0][0].fingerprint;
            for s in arms.iter().flatten() {
                assert_eq!(
                    s.fingerprint, fp0,
                    "same gas, successes and overlay set in every run"
                );
            }
            let row = |v: &[Sample]| {
                (
                    median(v.iter().map(|s| s.wall.as_secs_f64() * 1e3).collect()),
                    median(v.iter().map(|s| ms(s.match_ns)).collect()),
                    median(v.iter().map(|s| ms(s.settle_ns)).collect()),
                    median(v.iter().map(|s| ms(s.pass_b_ns)).collect()),
                    v[0].fills,
                )
            };
            let (off, on) = (row(&arms[0]), row(&arms[1]));
            println!(
                "shape {shape} (fills {}), L2.2 {no_merge}: wall ms off {:.2} on {:.2} (on - off {:+.2}, on / off {:.3}); \
                 match ms {:.2} -> {:.2}; settle ms {:.2} -> {:.2}; pass B ms {:.2} -> {:.2}",
                off.4,
                off.0,
                on.0,
                on.0 - off.0,
                on.0 / off.0,
                off.1,
                on.1,
                off.2,
                on.2,
                off.3,
                on.3,
            );
            let walls = |v: &[Sample]| {
                let mut w: Vec<f64> = v.iter().map(|s| s.wall.as_secs_f64() * 1e3).collect();
                w.sort_by(|a, b| a.partial_cmp(b).unwrap());
                w.iter()
                    .map(|x| format!("{x:.1}"))
                    .collect::<Vec<_>>()
                    .join(" ")
            };
            println!("    off walls: {}", walls(&arms[0]));
            println!("    on  walls: {}", walls(&arms[1]));
        }
    }
}
