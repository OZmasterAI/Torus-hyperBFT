//! L3-ENG µbench (run explicitly, quiet box):
//!   cargo test -p torus-bridge --release --test engine_parallel_bench -- --ignored --nocapture
//!
//! Cap-400 block shape: 10 markets, 40 senders, seeded resting ladders
//! (~1200 resting orders), then a 400-order crossing storm measured through
//! `execute_batch_engine_mode` at threads {0, 2, 4, 8}. Fresh DB + identical
//! seed per iteration; only the storm call is timed. Prints per-thread-count
//! min/median ms — the serial-vs-parallel delta for the Layer-3 ledger.

use alloy_primitives::Address;
use torus_bridge::native_executor::{NativeExecContext, NativeExecutor};
use torus_core::position::NativeBalance;
use torus_state::StateDb;
use torus_types::{FixedPoint, MarketId, NativeAction, OrderType, PlaceOrderParams, TimeInForce};

fn addr(n: u8) -> Address {
    Address::new([n; 20])
}

fn fp(v: i64) -> FixedPoint {
    FixedPoint::from_raw(v as i128 * FixedPoint::SCALE)
}

fn make_ctx(state_db: StateDb) -> NativeExecContext {
    NativeExecContext::new(
        state_db,
        1,
        1000,
        0,
        100,
        10,
        addr(99),
        addr(100),
        addr(101),
    )
}

#[allow(clippy::too_many_arguments)]
fn order(
    market_id: MarketId,
    is_buy: bool,
    price: i64,
    qty: i64,
    tif: TimeInForce,
) -> PlaceOrderParams {
    PlaceOrderParams {
        market_id,
        is_buy,
        price: fp(price),
        quantity: fp(qty),
        order_type: OrderType::Limit,
        time_in_force: tif,
        reduce_only: false,
        client_order_id: None,
    }
}

fn place(sender: Address, p: PlaceOrderParams) -> (Address, NativeAction) {
    (sender, NativeAction::PlaceOrder(p))
}

struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 11
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// One benchmark shape: `markets` markets, `senders` distinct senders,
/// `storm_n` orders in the measured storm, resting ladders `seed_levels`
/// deep per side per market.
struct Shape {
    markets: u64,
    senders: u8,
    storm_n: usize,
    seed_levels: i64,
}

/// Seed batch: resting ladders both sides of 100 on every market,
/// makers spread across senders.
fn seed_batch(rng: &mut Lcg, sh: &Shape) -> Vec<(Address, NativeAction)> {
    let mut b = Vec::new();
    for m in 1..=sh.markets {
        for lvl in 0..sh.seed_levels {
            for _ in 0..2 {
                let maker = addr((1 + rng.below(sh.senders as u64)) as u8);
                b.push(place(
                    maker,
                    order(m, true, 70 - lvl, 1 + rng.below(3) as i64, TimeInForce::GTC),
                ));
                let maker2 = addr((1 + rng.below(sh.senders as u64)) as u8);
                b.push(place(
                    maker2,
                    order(m, false, 131 + lvl, 1 + rng.below(3) as i64, TimeInForce::GTC),
                ));
            }
        }
    }
    b
}

/// The measured storm: `storm_n` orders across the shape's markets/senders,
/// prices straddling the resting ladders so a large fraction cross and sweep
/// levels (realistic matched flow).
fn storm_batch(rng: &mut Lcg, sh: &Shape) -> Vec<(Address, NativeAction)> {
    let mut b = Vec::new();
    for _ in 0..sh.storm_n {
        let m = 1 + rng.below(sh.markets);
        let sender = addr((1 + rng.below(sh.senders as u64)) as u8);
        let is_buy = rng.below(2) == 0;
        let tif = if rng.below(5) == 0 {
            TimeInForce::IOC
        } else {
            TimeInForce::GTC
        };
        // Buys 128.. cross the ask ladder (131-lvl side); sells 64.. cross
        // the bid ladder; the rest rest near mid.
        let price = if rng.below(3) == 0 {
            if is_buy {
                95 + rng.below(10) as i64
            } else {
                105 + rng.below(10) as i64
            }
        } else if is_buy {
            128 + rng.below(10) as i64
        } else {
            64 + rng.below(10) as i64
        };
        b.push(place(sender, order(m, is_buy, price, 1 + rng.below(4) as i64, tif)));
    }
    b
}

fn bench_one(sh: &Shape, threads: usize, iters: usize) -> (f64, f64, u32) {
    let mut samples_ms: Vec<f64> = Vec::with_capacity(iters);
    let mut trades = 0u32;
    for it in 0..iters {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = StateDb::open(dir.path()).expect("open db");
        let mut ctx = make_ctx(db);
        for n in 1..=sh.senders {
            ctx.positions
                .put_native_balance(
                    &addr(n),
                    &NativeBalance {
                        available: fp(100_000_000),
                        order_margin: FixedPoint::ZERO,
                    },
                )
                .unwrap();
        }
        // Identical seed per iteration (and per thread count).
        let mut rng = Lcg(0xBE4C_4001 + it as u64);
        let seed = seed_batch(&mut rng, sh);
        let storm = storm_batch(&mut rng, sh);
        NativeExecutor::execute_batch_engine_mode(&mut ctx, &seed, threads);
        assert!(ctx.fatal_error.is_none());

        let t0 = std::time::Instant::now();
        NativeExecutor::execute_batch_engine_mode(&mut ctx, &storm, threads);
        let dt = t0.elapsed();
        assert!(ctx.fatal_error.is_none());
        samples_ms.push(dt.as_secs_f64() * 1e3);
        trades = ctx.trade_index;
    }
    samples_ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let min = samples_ms[0];
    let median = samples_ms[samples_ms.len() / 2];
    (min, median, trades)
}

fn run_shape(name: &str, sh: &Shape, thread_counts: &[usize], iters: usize) {
    println!(
        "engine µbench [{name}]: {} orders, {} markets, {} senders, {iters} iters",
        sh.storm_n, sh.markets, sh.senders
    );
    for &threads in thread_counts {
        let (min, median, trades) = bench_one(sh, threads, iters);
        let label = if threads == 0 {
            "serial ".to_string()
        } else {
            format!("thr={threads:<4}")
        };
        println!("  {label} min={min:8.3} ms  median={median:8.3} ms  (trades/blk={trades})");
    }
}

#[test]
#[ignore = "µbench — run with --ignored --nocapture on a quiet box"]
fn bench_engine_parallel_cap400() {
    run_shape(
        "cap-400",
        &Shape {
            markets: 10,
            senders: 40,
            storm_n: 400,
            seed_levels: 30,
        },
        &[0, 2, 4, 8],
        15,
    );
}

/// Uncapped-shape cells: does TORUS_PARALLEL_ENGINE become the lever once
/// per-market work is large? In-vivo uncapped blocks carry ~27k orders and
/// the engine was 0.3-0.4 s/blk.
#[test]
#[ignore = "µbench — run with --ignored --nocapture on a quiet box"]
fn bench_engine_parallel_large() {
    run_shape(
        "large-5k",
        &Shape {
            markets: 10,
            senders: 100,
            storm_n: 5_000,
            seed_levels: 40,
        },
        &[0, 4, 8, 16],
        7,
    );
    run_shape(
        "large-25k",
        &Shape {
            markets: 20,
            senders: 200,
            storm_n: 25_000,
            seed_levels: 40,
        },
        &[0, 4, 8, 16],
        5,
    );
}
