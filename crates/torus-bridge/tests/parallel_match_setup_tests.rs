//! TORUS_PARALLEL_MATCH_SETUP (exp/archy-s58-parallel-match-setup): with the
//! flag on, each market's Phase 3 match inputs (the reduce-only positions its
//! book polices, the checked takers' account margins, the match requests) are
//! built inside that market's pool match job instead of serially on the exec
//! thread. The setup only reads frozen state, so THE contract is byte
//! identity with the flag off: every action result field, total gas, the
//! native CFs, fills and their extras, trade_index, next_global_order_id and
//! the native state root, on every batch path (sequential / parallel settle,
//! serial / sharded Phase 2).
//!
//! The scenario covers the setup's inputs: reduce-only orders (taker and
//! resting, including resting reduce-only orders of traders with nothing in
//! the batch, whose positions the setup reads), checked takers in their D2
//! pool market and outside it (taker-only), unchecked takers, market orders,
//! stops, cloids (duplicates too), FOK / IOC / PostOnly, markets without a
//! book (with and without a market row), marked markets with tiered configs
//! (the setup's mark and tiers) and single-market batches (inline path).

use alloy_primitives::{Address, B256};

use torus_bridge::native_executor::{
    with_fast_sell_top_ups, with_parallel_match_setup, with_settle_defer_results,
    NativeExecContext, NativeExecutor,
};
use torus_bridge::state_root::compute_native_state_root;
use torus_core::margin::{MarginTier, MarketMarginConfig};
use torus_core::position::NativeBalance;
use torus_state::action_status::FailureReason;
use torus_state::cf::{
    CF_BOOK_ORDER_ROWS, CF_NATIVE_BALANCES, CF_NATIVE_MARKETS, CF_NATIVE_ORDER_BOOKS,
    CF_NATIVE_POSITIONS, CF_NATIVE_TRADES, CF_NATIVE_USER_TRADES,
};
use torus_state::trade_rows::{FillExtras, TradeFill};
use torus_state::{StateBackend, StateDb};
use torus_types::{FixedPoint, MarketId, NativeAction, OrderType, PlaceOrderParams, TimeInForce};

const S: i128 = FixedPoint::SCALE;

fn addr(n: u8) -> Address {
    Address::new([n; 20])
}

fn fp(v: i64) -> FixedPoint {
    FixedPoint::from_raw(v as i128 * S)
}

fn order(
    market_id: MarketId,
    is_buy: bool,
    price: FixedPoint,
    qty: FixedPoint,
    tif: TimeInForce,
) -> PlaceOrderParams {
    PlaceOrderParams {
        market_id,
        is_buy,
        price,
        quantity: qty,
        order_type: OrderType::Limit,
        time_in_force: tif,
        reduce_only: false,
        client_order_id: None,
    }
}

fn gtc(market_id: MarketId, is_buy: bool, price: i64, qty: i64) -> PlaceOrderParams {
    order(market_id, is_buy, fp(price), fp(qty), TimeInForce::GTC)
}

fn ro(mut p: PlaceOrderParams) -> PlaceOrderParams {
    p.reduce_only = true;
    p
}

fn place(sender: Address, p: PlaceOrderParams) -> (Address, NativeAction) {
    (sender, NativeAction::PlaceOrder(p))
}

/// The batch path a run takes.
#[derive(Clone, Copy, Debug)]
enum Path {
    /// `execute_batch_settle_workers(_, _, parallel, None)` (serial Phase 2).
    Settle(bool),
    /// `execute_batch_engine_mode(_, _, threads)`.
    Engine(usize),
}

const PATHS: [Path; 4] = [
    Path::Settle(false),
    Path::Settle(true),
    Path::Engine(0),
    Path::Engine(4),
];

/// How the flag is set for a run.
#[derive(Clone, Copy, Debug)]
enum Flag {
    /// Pinned off on the calling thread.
    Off,
    /// Whatever the process env says (unset in the default test run).
    Env,
    /// Pinned on on the calling thread.
    On,
}

type ActionOut = (&'static str, bool, Option<String>, u64, FailureReason);

/// Full observable outcome of a run: byte-identical or bust.
#[derive(PartialEq, Eq, Debug)]
struct Fingerprint {
    /// Every `NativeActionResult` field, per batch.
    results: Vec<Vec<ActionOut>>,
    total_gas: Vec<u64>,
    cf_dump: Vec<(String, Vec<u8>, Vec<u8>)>,
    fills: Vec<Vec<TradeFill>>,
    extras: Vec<Vec<FillExtras>>,
    trade_index: u32,
    next_global_order_id: u128,
    state_root: B256,
}

fn state_dump(ctx: &NativeExecContext) -> Vec<(String, Vec<u8>, Vec<u8>)> {
    let cfs = [
        CF_NATIVE_BALANCES,
        CF_NATIVE_POSITIONS,
        CF_NATIVE_ORDER_BOOKS,
        CF_NATIVE_MARKETS,
        CF_NATIVE_TRADES,
        CF_NATIVE_USER_TRADES,
        CF_BOOK_ORDER_ROWS,
    ];
    let mut out = Vec::new();
    for cf in cfs {
        for (k, v) in ctx.state.iterate_cf(cf, None).expect("iterate cf") {
            out.push((cf.to_string(), k, v));
        }
    }
    out
}

fn set_mark(ctx: &NativeExecContext, market_id: MarketId, price: FixedPoint) {
    let reporters = [addr(150), addr(151), addr(152)];
    for v in &reporters {
        ctx.oracle
            .submit_price(v, market_id, price, ctx.block_height, ctx.timestamp)
            .unwrap();
    }
    let stakes: Vec<(Address, FixedPoint)> = reporters.iter().map(|v| (*v, fp(1))).collect();
    let agg = ctx
        .oracle
        .aggregate_price(market_id, ctx.block_height, ctx.timestamp, &stakes)
        .unwrap();
    assert_eq!(agg, price, "test oracle aggregates to the mark");
}

/// The genesis / governance market row: base, quote, lot, tick, initial margin.
fn market_row(tick: i128, lot: i128) -> Vec<u8> {
    borsh::to_vec(&("BTC".to_string(), "USD".to_string(), lot, tick, 5 * S)).unwrap()
}

fn run_once(batches: &[Vec<(Address, NativeAction)>], path: Path) -> Fingerprint {
    let dir = tempfile::tempdir().expect("temp dir");
    let db = StateDb::open(dir.path()).expect("open db");
    let mut ctx = NativeExecContext::new(db, 1, 1000, 0, 100, 10, addr(99), addr(100), addr(101));
    ctx.record_fills = true;
    for n in 1..=48u8 {
        let bal = NativeBalance {
            available: fp(1_000_000),
            order_margin: FixedPoint::ZERO,
        };
        ctx.positions.put_native_balance(&addr(n), &bal).unwrap();
    }
    // Tiered configs + marks: the setup's tiers and its fill-charge mark.
    for m in [1u64, 2] {
        let mut c = MarketMarginConfig::new(m, 999);
        c.tiers = vec![
            MarginTier {
                max_notional: fp(2_000),
                max_leverage: 20,
            },
            MarginTier {
                max_notional: FixedPoint::MAX,
                max_leverage: 5,
            },
        ];
        ctx.margin_configs.insert(m, c);
        set_mark(&ctx, m, fp(100));
    }
    // Market 9 has a row (tick 2) but no book until the storm creates it.
    ctx.state
        .put_cf_raw(
            CF_NATIVE_MARKETS,
            &9u64.to_be_bytes(),
            &market_row(2 * S, S),
        )
        .unwrap();

    let mut results = Vec::new();
    let mut total_gas = Vec::new();
    let mut fills = Vec::new();
    let mut extras = Vec::new();
    for batch in batches {
        let r = match path {
            Path::Settle(parallel) => {
                NativeExecutor::execute_batch_settle_workers(&mut ctx, batch, parallel, None)
            }
            Path::Engine(threads) => {
                NativeExecutor::execute_batch_engine_mode(&mut ctx, batch, threads)
            }
        };
        assert!(
            ctx.fatal_error.is_none(),
            "{path:?}: no batch here may go fatal: {:?}",
            ctx.fatal_error
        );
        results.push(
            r.results
                .iter()
                .map(|a| {
                    (
                        a.action_type,
                        a.success,
                        a.error.clone().map(String::from),
                        a.gas_used,
                        a.reason,
                    )
                })
                .collect(),
        );
        total_gas.push(r.total_gas);
        let (f, e) = ctx.take_pending_fills_and_extras();
        assert_eq!(e.len(), f.len(), "extras aligned with fills");
        fills.push(f);
        extras.push(e);
    }
    ctx.save_order_books();
    Fingerprint {
        results,
        total_gas,
        cf_dump: state_dump(&ctx),
        fills,
        extras,
        trade_index: ctx.trade_index,
        next_global_order_id: ctx.next_global_order_id,
        state_root: compute_native_state_root(&ctx.state).expect("state root"),
    }
}

fn run(batches: &[Vec<(Address, NativeAction)>], path: Path, flag: Flag) -> Fingerprint {
    match flag {
        Flag::Off => with_parallel_match_setup(false, || run_once(batches, path)),
        Flag::Env => run_once(batches, path),
        Flag::On => with_parallel_match_setup(true, || run_once(batches, path)),
    }
}

/// Tiny deterministic LCG so the fuzz scenario is reproducible with no deps.
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

/// Fixed position holders (their reduce-only orders rest in batch 3; batch 4
/// trades against them while they send nothing).
const LONG_1: u8 = 40;
const SHORT_2: u8 = 41;
const LONG_3: u8 = 42;
/// A sender whose checked takers span markets 4, 5, 6 in one batch: its D2
/// pool is market 4's, the others are taker-only.
const SPREAD: u8 = 43;

/// One random storm order across `markets`.
fn storm_order(rng: &mut Lcg, markets: &[MarketId], i: u64) -> (Address, NativeAction) {
    let m = markets[rng.below(markets.len() as u64) as usize];
    let sender = addr((1 + rng.below(32)) as u8);
    let is_buy = rng.below(2) == 0;
    let tif = match rng.below(10) {
        0 => TimeInForce::IOC,
        1 => TimeInForce::FOK,
        2 => TimeInForce::PostOnly,
        _ => TimeInForce::GTC,
    };
    let price = 96 + 2 * rng.below(5) as i64;
    let qty = FixedPoint::from_raw(
        (1 + rng.below(4)) as i128 * S + rng.below(S as u64 / 4) as i128 / 1000 * 1000,
    );
    let mut p = order(m, is_buy, fp(price), qty, tif);
    match i % 23 {
        // Market order with its price cap (checked taker).
        3 => {
            p.order_type = OrderType::Market;
            p.time_in_force = TimeInForce::IOC;
            p.price = if is_buy { fp(110) } else { fp(90) };
        }
        // Stops: fire on later fills, placed after settlement.
        5 => {
            p.order_type = OrderType::StopMarket {
                trigger: if is_buy { fp(102) } else { fp(98) },
            };
            p.price = if is_buy { fp(112) } else { fp(88) };
        }
        8 => {
            p.order_type = OrderType::StopLimit {
                trigger: if is_buy { fp(102) } else { fp(98) },
                limit: if is_buy { fp(104) } else { fp(96) },
            };
            p.price = FixedPoint::ZERO;
        }
        // Reduce-only taker (mostly no position: rejected; else clamped).
        11 | 17 => p.reduce_only = true,
        // Dust (below lot) and off-tick: pre-book rejects.
        13 => p.quantity = FixedPoint::from_raw(S / 2),
        19 => p.price = FixedPoint::from_raw(price as i128 * S + 7),
        _ => {}
    }
    // Cloids on a quarter of the orders; few values, so duplicates happen.
    if rng.below(4) == 0 {
        p.client_order_id = Some(rng.below(4) as u128);
    }
    place(sender, p)
}

fn scenario(seed: u64) -> Vec<Vec<(Address, NativeAction)>> {
    let mut rng = Lcg(seed);
    let books: [MarketId; 6] = [1, 2, 3, 4, 5, 6];

    // 1. Resting liquidity on both sides of 100.
    let mut b1 = Vec::new();
    for &m in &books {
        for lvl in 0..4i64 {
            b1.push(place(
                addr((1 + rng.below(32)) as u8),
                gtc(m, true, 97 - 2 * lvl, 2 + rng.below(3) as i64),
            ));
            b1.push(place(
                addr((1 + rng.below(32)) as u8),
                gtc(m, false, 101 + 2 * lvl, 2 + rng.below(3) as i64),
            ));
        }
    }

    // 2. Positions for the holders, the spread sender, and a storm that
    //    also creates market 9's book (row: tick 2) and market 11's (no row).
    let mut b2 = vec![
        place(addr(LONG_1), gtc(1, true, 103, 4)),
        place(addr(SHORT_2), gtc(2, false, 97, 4)),
        place(addr(LONG_3), gtc(3, true, 103, 3)),
        place(
            addr(SPREAD),
            order(4, false, fp(97), fp(1), TimeInForce::IOC),
        ),
        place(
            addr(SPREAD),
            order(5, false, fp(97), fp(1), TimeInForce::GTC),
        ),
        place(
            addr(SPREAD),
            order(6, true, fp(103), fp(1), TimeInForce::FOK),
        ),
    ];
    let storm_markets: [MarketId; 8] = [1, 2, 3, 4, 5, 6, 9, 11];
    for i in 0..220 {
        b2.push(storm_order(&mut rng, &storm_markets, i));
    }

    // 3. The holders' reduce-only orders rest away from the touch.
    let mut b3 = vec![
        place(addr(LONG_1), ro(gtc(1, false, 115, 2))),
        place(addr(SHORT_2), ro(gtc(2, true, 85, 2))),
        place(addr(LONG_3), ro(gtc(3, false, 116, 1))),
        // Oversize: clamped to the position.
        place(addr(LONG_1), ro(gtc(1, false, 117, 50))),
    ];
    for i in 0..40 {
        b3.push(storm_order(&mut rng, &[4, 5, 6], i));
    }

    // 4. Others sweep into the resting reduce-only orders; the holders send
    //    nothing, so the setup reads their positions (not Phase 2's).
    let mut b4 = vec![
        place(addr(5), gtc(1, true, 118, 30)),
        place(addr(6), gtc(2, false, 84, 30)),
        place(addr(7), order(3, true, fp(116), fp(20), TimeInForce::IOC)),
        place(addr(8), gtc(4, true, 99, 1)),
    ];
    for i in 0..30 {
        b4.push(storm_order(&mut rng, &[4, 5, 6, 9], i));
    }

    // 5. A second storm across everything.
    let mut b5 = Vec::new();
    for i in 0..160 {
        b5.push(storm_order(&mut rng, &storm_markets, i));
    }

    // 6. Single-market batch (the inline path even with the flag on).
    let mut b6 = Vec::new();
    for i in 0..24 {
        b6.push(storm_order(&mut rng, &[3], i));
    }

    // 7. Single-market batch in a market with no book (created inline).
    let b7 = vec![
        place(addr(9), gtc(12, true, 99, 1)),
        place(addr(10), gtc(12, false, 99, 1)),
    ];

    vec![b1, b2, b3, b4, b5, b6, b7]
}

/// Flag pinned off == process env (unset by default) == pinned on, on every
/// path; the pinned-on run repeats to catch schedule-dependent results.
fn assert_flag_invariant(batches: &[Vec<(Address, NativeAction)>], reps: usize) -> Fingerprint {
    let mut golden = None;
    for path in PATHS {
        let off = run(batches, path, Flag::Off);
        assert_eq!(
            run(batches, path, Flag::Env),
            off,
            "{path:?}: env run differs from flag off"
        );
        for rep in 0..reps {
            assert_eq!(
                run(batches, path, Flag::On),
                off,
                "{path:?} rep {rep}: flag on differs from flag off"
            );
        }
        golden.get_or_insert(off);
    }
    golden.expect("at least one path")
}

#[test]
fn flag_on_is_byte_identical_on_every_path() {
    let batches = scenario(0x5E7_0B5E7);
    let golden = assert_flag_invariant(&batches, 3);

    // The scenario really exercises what the setup builds.
    assert!(
        golden.trade_index > 60,
        "scenario too weak: {} trades",
        golden.trade_index
    );
    let filled_markets: std::collections::BTreeSet<u64> =
        golden.fills.iter().flatten().map(|f| f.market).collect();
    for m in [1u64, 2, 3, 4, 5, 6, 9, 11, 12] {
        assert!(
            filled_markets.contains(&m),
            "no fill in market {m}: {filled_markets:?}"
        );
    }
    // Batch 4 fills the holders' resting reduce-only orders as makers.
    for (holder, m) in [(LONG_1, 1u64), (SHORT_2, 2), (LONG_3, 3)] {
        assert!(
            golden.fills[3]
                .iter()
                .any(|f| f.maker == addr(holder) && f.market == m),
            "batch 4: no fill against holder {holder}'s reduce-only order in market {m}"
        );
    }
    let ok = |b: usize| golden.results[b].iter().filter(|r| r.1).count();
    let reduce_only_rested = golden.results[2][..4].iter().filter(|r| r.1).count();
    assert_eq!(
        reduce_only_rested,
        4,
        "batch 3 reduce-only orders: {:?}",
        &golden.results[2][..4]
    );
    assert!(
        ok(1) > 100 && ok(1) < golden.results[1].len(),
        "batch 2 needs passes and rejects"
    );
}

#[test]
fn flag_on_is_byte_identical_second_seed() {
    let batches = scenario(0xD15C_0BAD);
    let golden = assert_flag_invariant(&batches, 2);
    assert!(
        golden.trade_index > 60,
        "scenario too weak: {} trades",
        golden.trade_index
    );
}

/// exp/archy-s58-bundle-dmt: the bundle's three bridge flags (D
/// `TORUS_SETTLE_DEFER_RESULTS`, M `TORUS_PARALLEL_MATCH_SETUP`, T
/// `TORUS_FAST_SELL_TOP_UPS`) pinned on together equal all three pinned off
/// on this file's match-setup scenarios, on every batch path.
#[test]
fn s58_bundle_dmt_identical_to_flags_off() {
    let all = |on: bool, f: &dyn Fn() -> Fingerprint| {
        with_settle_defer_results(on, || {
            with_parallel_match_setup(on, || with_fast_sell_top_ups(on, f))
        })
    };
    for seed in [0x5E7_0B5E7u64, 0xD15C_0BAD] {
        let batches = scenario(seed);
        for path in PATHS {
            let off = all(false, &|| run_once(&batches, path));
            let on = all(true, &|| run_once(&batches, path));
            assert_eq!(off, on, "seed {seed:#x} {path:?}: D+M+T vs flags off");
        }
    }
}
