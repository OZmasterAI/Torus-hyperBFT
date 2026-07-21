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

const MARKETS: u64 = 10;
const SENDERS: u8 = 40;

/// Seed batch: resting ladders both sides of 100 on every market
/// (~1200 resting orders), makers spread across senders.
fn seed_batch(rng: &mut Lcg) -> Vec<(Address, NativeAction)> {
    let mut b = Vec::new();
    for m in 1..=MARKETS {
        for lvl in 0..30i64 {
            for _ in 0..2 {
                let maker = addr((1 + rng.below(SENDERS as u64)) as u8);
                b.push(place(
                    maker,
                    order(m, true, 70 - lvl, 1 + rng.below(3) as i64, TimeInForce::GTC),
                ));
                let maker2 = addr((1 + rng.below(SENDERS as u64)) as u8);
                b.push(place(
                    maker2,
                    order(m, false, 131 + lvl, 1 + rng.below(3) as i64, TimeInForce::GTC),
                ));
            }
        }
    }
    b
}

/// The measured storm: 400 orders, ~10 markets, ~40 senders, prices
/// straddling the resting ladders so a large fraction cross and sweep
/// levels (realistic cap-400 matched flow).
fn storm_batch(rng: &mut Lcg) -> Vec<(Address, NativeAction)> {
    let mut b = Vec::new();
    for _ in 0..400 {
        let m = 1 + rng.below(MARKETS);
        let sender = addr((1 + rng.below(SENDERS as u64)) as u8);
        let is_buy = rng.below(2) == 0;
        let tif = if rng.below(5) == 0 {
            TimeInForce::IOC
        } else {
            TimeInForce::GTC
        };
        // Buys 128..137 cross the ask ladder (131+); sells 64..73 cross the
        // bid ladder (70-); the rest rest near mid.
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

fn bench_one(threads: usize, iters: usize) -> (f64, f64, u32) {
    let mut samples_ms: Vec<f64> = Vec::with_capacity(iters);
    let mut trades = 0u32;
    for it in 0..iters {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = StateDb::open(dir.path()).expect("open db");
        let mut ctx = make_ctx(db);
        for n in 1..=SENDERS {
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
        let seed = seed_batch(&mut rng);
        let storm = storm_batch(&mut rng);
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

#[test]
#[ignore = "µbench — run with --ignored --nocapture on a quiet box"]
fn bench_engine_parallel_cap400() {
    const ITERS: usize = 15;
    println!("engine µbench: 400-order storm, {MARKETS} markets, {SENDERS} senders, {ITERS} iters");
    for threads in [0usize, 2, 4, 8] {
        let (min, median, trades) = bench_one(threads, ITERS);
        let label = if threads == 0 {
            "serial ".to_string()
        } else {
            format!("thr={threads:<4}")
        };
        println!("  {label} min={min:7.3} ms  median={median:7.3} ms  (trades/blk={trades})");
    }
}
