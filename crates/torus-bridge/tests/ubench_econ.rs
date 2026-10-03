//! s87 µbench: econ-shaped load like tools/bench-throughput `--econ` (GTC
//! limits only, one side per (sender, market), mid = 20 x target, band 5,
//! cross 0.5, cancel-all 5% + open-order budget 900, batches of 400 orders per
//! action), run block by block through NativeStateOverlay layered over the
//! previous block's frozen set (the node's pipelined exec path).
//! Prints engine ms per 1k fills (execute_batch [+ run_liquidations]).
//!
//! `UB_MARKS=1`: markets 1..=UB_MARKETS are listed and three Active validators
//! submit the mid (`TARGET * LEV`) for every market each block, so
//! `begin_block_oracle` aggregates a usable mark (a fed chain: margin and the
//! liquidation scan value accounts). Default: no listed market, no marks.
//!
//!   cargo test -p torus-bridge --release --test ubench_econ -- --ignored --nocapture

use alloy_primitives::{Address, U256};
use std::collections::HashMap;
use std::sync::Arc;
use torus_bridge::native_executor::{NativeExecContext, NativeExecutor};
use torus_core::position::NativeBalance;
use torus_economics::{StakingManager, ValidatorState, ValidatorStatus, MIN_SELF_DELEGATION};
use torus_state::cf::CF_NATIVE_MARKETS;
use torus_state::{NativeStateOverlay, StateDb};
use torus_types::{FixedPoint, NativeAction, OrderType, PlaceOrderParams, TimeInForce};

fn env(k: &str, d: u64) -> u64 {
    std::env::var(k).ok().and_then(|v| v.parse().ok()).unwrap_or(d)
}

fn sender(i: u64) -> Address {
    let mut b = [0xA7u8; 20];
    b[12..20].copy_from_slice(&(i + 1).to_be_bytes());
    Address::new(b)
}

fn special(n: u8) -> Address {
    Address::new([n; 20])
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
    fn chance(&mut self, p_milli: u64) -> bool {
        self.below(1000) < p_milli
    }
}

const TARGET: i128 = 1500;
const LEV: i128 = 20;
const BAND: u64 = 5;
const REPORTERS: [u8; 3] = [150, 151, 152];

/// `UB_MARKS=1`: three Active validators and markets `1..=markets` listed.
fn feed_setup(db: &StateDb, markets: u64) {
    for n in REPORTERS {
        StakingManager::new(db.clone())
            .put_validator(
                &special(n),
                &ValidatorState {
                    address: special(n),
                    pubkey: [n; 32],
                    commission_bps: 0,
                    self_stake: MIN_SELF_DELEGATION,
                    total_delegated: U256::ZERO,
                    status: ValidatorStatus::Active,
                    jailed_until: None,
                    last_commission_change_block: None,
                    oracle_signer: None,
                },
            )
            .unwrap();
    }
    for m in 1..=markets {
        db.put_cf_raw(CF_NATIVE_MARKETS, &m.to_be_bytes(), b"listed").unwrap();
    }
}

fn econ_order(rng: &mut Lcg, s: u64, market_id: u64) -> PlaceOrderParams {
    let is_buy = s.wrapping_add(market_id).is_multiple_of(2);
    let aggressive = rng.chance(500);
    let d = 1 + rng.below(BAND) as i128;
    let mid = TARGET * LEV;
    let units = if is_buy == aggressive { mid + d } else { mid - d };
    let price = FixedPoint::from_raw(units * FixedPoint::SCALE);
    let target = FixedPoint::from_raw(TARGET * FixedPoint::SCALE);
    let lev = FixedPoint::from_raw(LEV * FixedPoint::SCALE);
    let mut quantity = target * lev / price;
    if quantity < FixedPoint::ONE {
        quantity = FixedPoint::ONE;
    }
    PlaceOrderParams {
        market_id,
        is_buy,
        price,
        quantity,
        order_type: OrderType::Limit,
        time_in_force: TimeInForce::GTC,
        reduce_only: false,
        client_order_id: None,
    }
}

struct Gen {
    rng: Lcg,
    senders: u64,
    markets: u64,
    batch: u64,
    budget: u64,
    open: HashMap<u64, u64>,
}

impl Gen {
    fn block(&mut self, actions: u64) -> Vec<(Address, NativeAction)> {
        let mut out = Vec::with_capacity(actions as usize);
        for _ in 0..actions {
            let s = self.rng.below(self.senders);
            let open = self.open.entry(s).or_insert(0);
            let a = if *open + self.batch > self.budget || self.rng.chance(50) {
                *open = 0;
                NativeAction::CancelAllOrders { market_id: None }
            } else {
                *open += self.batch;
                let orders = (0..self.batch)
                    .map(|_| {
                        let m = 1 + self.rng.below(self.markets);
                        econ_order(&mut self.rng, s, m)
                    })
                    .collect();
                NativeAction::PlaceOrderBatch(orders)
            };
            out.push((sender(s), a));
        }
        out
    }
}

struct Sample {
    exec_ms: f64,
    tail_ms: f64,
    margin_ms: f64,
    match_ms: f64,
    settle_ms: f64,
    fills: u64,
}

fn run_once(seed: u64) -> (Vec<Sample>, u64, u64, u64) {
    let senders = env("UB_SENDERS", 600);
    let markets = env("UB_MARKETS", 300);
    let actions = env("UB_ACTIONS", 60);
    let warm = env("UB_WARM", 40);
    let measure = env("UB_MEASURE", 6);
    let fed = env("UB_MARKS", 0) == 1;

    let dir = tempfile::tempdir().expect("tempdir");
    let db = StateDb::open(dir.path()).expect("open db");
    {
        let ctx = NativeExecContext::new(
            db.clone(), 1, 1000, 0, 1_000_000, 100, special(99), special(100), special(101),
        );
        for i in 0..senders {
            ctx.positions
                .put_native_balance(
                    &sender(i),
                    &NativeBalance {
                        available: FixedPoint::from_raw(100_000_000 * FixedPoint::SCALE),
                        order_margin: FixedPoint::ZERO,
                    },
                )
                .unwrap();
        }
    }
    if fed {
        feed_setup(&db, markets);
    }
    let metrics = Arc::new(torus_telemetry::Metrics::new());
    let mut gen = Gen {
        rng: Lcg(0x5EED_0087 ^ seed),
        senders,
        markets,
        batch: env("UB_BATCH", 400),
        budget: env("UB_BUDGET", 900),
        open: HashMap::new(),
    };
    let mut books = HashMap::new();
    let mut next_id: u128 = 1;
    let mut parent = None;
    let mut samples = Vec::new();
    let mut rc_measured = 0u64;
    let mut placed_measured = 0u64;
    for h in 1..=(warm + measure) {
        let block = gen.block(actions);
        let overlay = NativeStateOverlay::with_parent(db.clone(), parent.clone());
        let mut ctx = NativeExecContext::new(
            overlay.clone(),
            h + 1,
            1000 + h,
            0,
            1_000_000,
            100,
            special(99),
            special(100),
            special(101),
        );
        ctx.order_books = std::mem::take(&mut books);
        ctx.next_global_order_id = next_id;
        ctx.metrics = Some(metrics.clone());
        if fed {
            // Before the timed block start, stamped with this block (s85 feeder M1b).
            let mark = FixedPoint::from_raw(TARGET * LEV * FixedPoint::SCALE);
            for m in 1..=markets {
                for n in REPORTERS {
                    ctx.oracle.submit_price(&special(n), m, mark, ctx.block_height, ctx.timestamp).unwrap();
                }
            }
        }
        let rc0 = metrics.orders_rejected_cancelled.get();
        let pa0 = metrics.orders_placed_accepted.get();
        let t0 = std::time::Instant::now();
        let agg = NativeExecutor::begin_block_oracle(&mut ctx);
        assert!(!fed || agg.iter().all(|r| r.success), "mark aggregation: {agg:?}");
        NativeExecutor::execute_batch(&mut ctx, &block);
        let exec = t0.elapsed();
        let t1 = std::time::Instant::now();
        let _ = NativeExecutor::run_liquidations(&mut ctx);
        let tail = t1.elapsed();
        assert!(ctx.fatal_error.is_none(), "{:?}", ctx.fatal_error);
        let pa = ctx.phase_accum;
        let fills = ctx.trade_index as u64;
        books = std::mem::take(&mut ctx.order_books);
        next_id = ctx.next_global_order_id;
        drop(ctx);
        if h > warm {
            rc_measured += metrics.orders_rejected_cancelled.get() - rc0;
            placed_measured += metrics.orders_placed_accepted.get() - pa0;
            samples.push(Sample {
                exec_ms: exec.as_secs_f64() * 1e3,
                tail_ms: tail.as_secs_f64() * 1e3,
                margin_ms: pa.margin_ns as f64 / 1e6,
                match_ms: pa.match_ns as f64 / 1e6,
                settle_ms: pa.settle_ns as f64 / 1e6,
                fills,
            });
        }
        let frozen = overlay.freeze(h);
        if let Some(p) = parent.take() {
            let p: Arc<torus_state::FrozenPending> = p;
            p.flush_with_native_trie_stats(&db, None, None, None).expect("flush");
        }
        parent = Some(frozen);
    }
    let resting: usize = books.values().map(|b: &torus_core::order_book::OrderBook| b.order_count()).sum();
    (samples, rc_measured, placed_measured, resting as u64)
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[v.len() / 2]
}

#[test]
#[ignore = "µbench — run with --ignored --nocapture on a quiet box"]
fn ubench_econ() {
    let runs = env("UB_RUNS", 3);
    let mut per_run = Vec::new();
    for r in 0..runs {
        let (s, rc, placed, resting) = run_once(r);
        let fills: u64 = s.iter().map(|x| x.fills).sum();
        let exec: f64 = s.iter().map(|x| x.exec_ms).sum();
        let tail: f64 = s.iter().map(|x| x.tail_ms).sum();
        let margin: f64 = s.iter().map(|x| x.margin_ms).sum();
        let mtch: f64 = s.iter().map(|x| x.match_ms).sum();
        let settle: f64 = s.iter().map(|x| x.settle_ms).sum();
        let n = s.len() as f64;
        let k = fills as f64 / 1e3;
        let per1k = (exec + tail) / k;
        println!(
            "UB run={r} blocks={} fills/blk={:.0} engine_ms/blk={:.1} engine_ms/1k_fills={:.2} \
             [exec/1k={:.2} margin/1k={:.2} match/1k={:.2} settle/1k={:.2} tail(liq)/1k={:.2}] \
             rejected_cancelled={} accepted={} resting_end={}",
            s.len(),
            fills as f64 / n,
            (exec + tail) / n,
            per1k,
            exec / k,
            margin / k,
            mtch / k,
            settle / k,
            tail / k,
            rc,
            placed,
            resting
        );
        per_run.push(per1k);
    }
    println!("UB marks={} MEDIAN engine_ms/1k_fills={:.2} runs={:?}", env("UB_MARKS", 0) == 1, median(per_run.clone()), per_run);
}
