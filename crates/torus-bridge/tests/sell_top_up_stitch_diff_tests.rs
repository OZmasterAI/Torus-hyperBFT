//! s58 (perf experiment sell-top-up-stitch-list): `TORUS_FAST_SELL_TOP_UPS=1`
//! (B-blind sell top-ups from the candidate list recorded at the Phase-2
//! stitch, a per-sender run cache, the batched s92 counter loop) must leave
//! BYTE-IDENTICAL state to the flag off: every CF row, per-action results,
//! gas, trade index, next order id, the native state root, and the top-up
//! and margin-cut counters. Both arms are pinned per thread
//! ([`with_fast_sell_top_ups`]), so the env does not leak in. Covers the
//! serial stitch (`execute_batch_settle_workers`, sequential and parallel
//! settle) and the sharded stitch (`execute_batch_engine_mode(4)`), across
//! two `execute_batch` calls per run. The unit-level differential (the scan
//! vs the list on hand-built batches, faults included) is
//! `src/sell_top_up_stitch_tests.rs`.

use std::sync::Arc;

use alloy_primitives::{Address, B256};

use torus_bridge::native_executor::{
    parse_fast_sell_top_ups, parse_parallel_match_setup, parse_settle_defer_results,
    with_fast_sell_top_ups, with_parallel_match_setup, with_settle_defer_results,
    NativeExecContext, NativeExecutor,
};
use torus_bridge::state_root::compute_native_state_root;
use torus_core::position::NativeBalance;
use torus_state::cf::{
    CF_BOOK_ORDER_ROWS, CF_NATIVE_BALANCES, CF_NATIVE_MARKETS, CF_NATIVE_ORDER_BOOKS,
    CF_NATIVE_POSITIONS, CF_NATIVE_TRADES, CF_NATIVE_USER_TRADES,
};
use torus_state::{StateBackend, StateDb};
use torus_telemetry::Metrics;
use torus_types::{FixedPoint, MarketId, NativeAction, OrderType, PlaceOrderParams, TimeInForce};

fn addr(n: u8) -> Address {
    Address::new([n; 20])
}

fn fp(v: i64) -> FixedPoint {
    FixedPoint::from_raw(v as i128 * FixedPoint::SCALE)
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

fn place(sender: Address, p: PlaceOrderParams) -> (Address, NativeAction) {
    (sender, NativeAction::PlaceOrder(p))
}

/// How a run executes its batches.
#[derive(Clone, Copy, Debug)]
enum Mode {
    /// `execute_batch_settle_workers`: serial prepare and stitch.
    Settle { parallel: bool },
    /// `execute_batch_engine_mode(threads)`: sharded prepare, serial stitch.
    Engine(usize),
}

/// Everything a run leaves behind.
#[derive(PartialEq, Eq, Debug)]
struct RunFingerprint {
    cf_dump: Vec<(String, Vec<u8>, Vec<u8>)>,
    results: Vec<Vec<(bool, Option<String>)>>,
    total_gas: Vec<u64>,
    trade_index: u32,
    next_global_order_id: u128,
    state_root: B256,
    /// sell_top_ups full / partial / none, maker margin cancels, reduce-only
    /// cuts, every sell-cut bucket.
    counters: Vec<u64>,
    /// `phase_accum.sell_top_ups`.
    top_ups: u64,
}

impl RunFingerprint {
    fn top_up_counts(&self) -> [u64; 3] {
        [self.counters[0], self.counters[1], self.counters[2]]
    }
}

fn state_dump(db: &StateDb) -> Vec<(String, Vec<u8>, Vec<u8>)> {
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
        for (k, v) in db.iterate_cf(cf, None).expect("iterate cf") {
            out.push((cf.to_string(), k, v));
        }
    }
    out
}

/// Publish `price` as `market_id`'s aggregated oracle mark (as in
/// engine_parallel_tests.rs).
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
    assert_eq!(agg, price);
}

/// One scenario: funding, marked markets and the batches (one
/// `execute_batch` call each).
struct Scenario {
    funding: Vec<(Address, FixedPoint)>,
    marks: Vec<(MarketId, FixedPoint)>,
    batches: Vec<Vec<(Address, NativeAction)>>,
}

fn run(s: &Scenario, mode: Mode, fast: bool) -> RunFingerprint {
    // Owner s138: a fixture from before the never-marked rule (test-only seam).
    torus_core::oracle::test_treat_unmarked_as_marked();
    with_fast_sell_top_ups(fast, || {
        let dir = tempfile::tempdir().expect("temp dir");
        let db = StateDb::open(dir.path()).expect("open db");
        // Pre-testnet P0: orders need a listed market (placeholder rows,
        // tick / lot 1, as the old implicit book).
        for m in 1..=64u64 {
            db.put_cf_raw(torus_state::cf::CF_NATIVE_MARKETS, &m.to_be_bytes(), b"listed").unwrap();
        }
        let mut ctx =
            NativeExecContext::new(db, 1, 1000, 0, 100, 10, addr(99), addr(100), addr(101));
        let metrics = Arc::new(Metrics::new());
        ctx.metrics = Some(metrics.clone());
        for (t, a) in &s.funding {
            ctx.positions
                .put_native_balance(
                    t,
                    &NativeBalance {
                        available: *a,
                        order_margin: FixedPoint::ZERO,
                    },
                )
                .unwrap();
        }
        for (m, p) in &s.marks {
            set_mark(&ctx, *m, *p);
        }
        let mut results = Vec::new();
        let mut total_gas = Vec::new();
        for batch in &s.batches {
            let r = match mode {
                Mode::Settle { parallel } => {
                    NativeExecutor::execute_batch_settle_workers(&mut ctx, batch, parallel, Some(2))
                }
                Mode::Engine(threads) => {
                    NativeExecutor::execute_batch_engine_mode(&mut ctx, batch, threads)
                }
            };
            assert!(ctx.fatal_error.is_none(), "{:?}", ctx.fatal_error);
            results.push(
                r.results
                    .iter()
                    .map(|a| (a.success, a.error.clone().map(String::from)))
                    .collect(),
            );
            total_gas.push(r.total_gas);
        }
        ctx.save_order_books();
        let mut counters = vec![
            metrics.sell_top_ups_full.get(),
            metrics.sell_top_ups_partial.get(),
            metrics.sell_top_ups_none.get(),
            metrics.maker_margin_cancels.get(),
            metrics.reduce_only_cuts.get(),
        ];
        counters.extend(
            metrics
                .sell_margin_cuts
                .iter()
                .flatten()
                .flatten()
                .map(|c| c.get()),
        );
        RunFingerprint {
            cf_dump: state_dump(&ctx.state),
            results,
            total_gas,
            trade_index: ctx.trade_index,
            next_global_order_id: ctx.next_global_order_id,
            state_root: compute_native_state_root(&ctx.state).expect("state root"),
            counters,
            top_ups: ctx.phase_accum.sell_top_ups,
        }
    })
}

const MODES: [Mode; 3] = [
    Mode::Settle { parallel: false },
    Mode::Settle { parallel: true },
    Mode::Engine(4),
];

/// Flag on == flag off in every mode (and every mode == the serial one).
fn assert_identical(s: &Scenario, what: &str) -> RunFingerprint {
    let golden = run(s, MODES[0], false);
    for mode in MODES {
        let off = run(s, mode, false);
        let on = run(s, mode, true);
        assert_eq!(on, off, "{what}: {mode:?}: flag on vs off");
        assert_eq!(off, golden, "{what}: {mode:?} vs serial");
    }
    golden
}

struct Lcg(u64);
impl Lcg {
    fn below(&mut self, n: u64) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (self.0 >> 33) % n
    }
}

/// B-blind shapes at random: makers ladder bids (not in market 6) and asks
/// in six markets (two marked); 24 thin to rich senders each send a pool
/// order (resting or crossing) then sells into other markets' bids (GTC,
/// IOC, FOK, market, PostOnly, reduce-only, below and above the top-up
/// price) and the odd buy, interleaved (runs, batches, cancel-alls), in two
/// storms (two `execute_batch` calls, the second on the first's state).
fn fuzz_scenario(seed: u64) -> Scenario {
    let mut rng = Lcg(seed);
    let (mk, mk2) = (addr(1), addr(2));
    let mut funding = vec![(mk, fp(10_000_000)), (mk2, fp(10_000_000))];
    let mut ladder = Vec::new();
    for m in 1..=6u64 {
        for (i, px) in [99i64, 100, 101].into_iter().enumerate() {
            if m != 6 {
                ladder.push(place(
                    if i == 1 { mk2 } else { mk },
                    gtc(m, true, px, 2 + rng.below(4) as i64),
                ));
            }
            ladder.push(place(mk, gtc(m, false, px + 5, 3)));
        }
    }
    let senders: Vec<Address> = (0..24u8).map(|i| addr(10 + i)).collect();
    // A third of the senders are tuned for the first storm: a resting pool
    // sell (reserves 10), then 1-4 GTC sells of 1 into other markets' start
    // bid 101 (Phase 2 reserves each at the bid, 5.05; its top-up to
    // 101.101 is 0.00505 more), funded with exactly that plus 0 to 1.5
    // top-ups per sell: full, partial and no top-ups. The rest: thin to
    // rich at cent granularity, random orders.
    let tuned: Vec<bool> = senders.iter().map(|_| rng.below(3) == 0).collect();
    for (s, _) in senders.iter().zip(&tuned).filter(|(_, t)| !**t) {
        let a = match rng.below(5) {
            0 => fp(1_000),
            _ => FixedPoint::from_raw(rng.below(3_000) as i128 * FixedPoint::SCALE / 100),
        };
        funding.push((*s, a));
    }
    let mut storms = Vec::new();
    for storm_no in 0..2 {
        let mut queues: Vec<(Address, Vec<PlaceOrderParams>)> = Vec::new();
        for (s, &tuned) in senders.iter().zip(&tuned) {
            let pool = 1 + rng.below(6);
            if tuned && storm_no == 0 {
                let mut q = vec![gtc(pool, false, 200, 1)];
                for _ in 0..1 + rng.below(4) {
                    let m = (pool + rng.below(4)) % 5 + 1; // 1..=5, never the pool
                    q.push(gtc(m, false, 95 + rng.below(6) as i64, 1));
                }
                let n = q.len() as u64 - 1;
                let raw = 10 * FixedPoint::SCALE
                    + i128::from(n) * 505_000_000
                    + i128::from(rng.below(n * 757_500 + 1));
                funding.push((*s, FixedPoint::from_raw(raw)));
                queues.push((*s, q));
                continue;
            }
            let mut q = vec![if rng.below(2) == 0 {
                gtc(pool, false, 150 + rng.below(50) as i64, 1)
            } else {
                gtc(pool, false, 97, 1)
            }];
            for _ in 0..1 + rng.below(6) {
                let m = 1 + rng.below(6);
                let qty = fp(1 + rng.below(4) as i64);
                let px = fp(88 + rng.below(16) as i64);
                q.push(match rng.below(10) {
                    0 => order(m, false, px, qty, TimeInForce::IOC),
                    1 => order(m, false, px, qty, TimeInForce::FOK),
                    2 => PlaceOrderParams {
                        order_type: OrderType::Market,
                        time_in_force: TimeInForce::IOC,
                        ..order(m, false, fp(80), qty, TimeInForce::IOC)
                    },
                    3 => order(m, false, px, qty, TimeInForce::PostOnly),
                    4 => PlaceOrderParams {
                        reduce_only: true,
                        ..order(m, false, px, qty, TimeInForce::GTC)
                    },
                    5 => order(m, true, fp(95 + rng.below(9) as i64), qty, TimeInForce::GTC),
                    _ => order(m, false, px, qty, TimeInForce::GTC),
                });
            }
            queues.push((*s, q));
        }
        let mut storm = Vec::new();
        while queues.iter().any(|q| !q.1.is_empty()) {
            let live: Vec<usize> = (0..queues.len())
                .filter(|&i| !queues[i].1.is_empty())
                .collect();
            let i = live[rng.below(live.len() as u64) as usize];
            let s = queues[i].0;
            match rng.below(10) {
                0 | 1 => {
                    let n = queues[i].1.len().min(1 + rng.below(3) as usize);
                    let orders: Vec<PlaceOrderParams> = queues[i].1.drain(..n).collect();
                    storm.push((s, NativeAction::PlaceOrderBatch(orders)));
                }
                2 if rng.below(4) == 0 => storm.push((
                    s,
                    NativeAction::CancelAllOrders {
                        market_id: Some(1 + rng.below(6)),
                    },
                )),
                _ => {
                    // A run: this sender's next one or two orders back to back.
                    for _ in 0..1 + rng.below(2) {
                        if !queues[i].1.is_empty() {
                            let p = queues[i].1.remove(0);
                            storm.push(place(s, p));
                        }
                    }
                }
            }
        }
        storms.push(storm);
    }
    let mut batches = vec![ladder];
    batches.extend(storms);
    Scenario {
        funding,
        marks: vec![(1, fp(100)), (3, fp(98))],
        batches,
    }
}

/// 10 fuzzed scenarios: flag on == flag off, every mode.
#[test]
fn fast_top_ups_identical_on_fuzzed_blocks() {
    let mut totals = [0u64; 3];
    let mut cuts = 0u64;
    for seed in 0..10u64 {
        let golden = assert_identical(&fuzz_scenario(0xB1_1D00 + seed), &format!("seed {seed}"));
        for (t, c) in totals.iter_mut().zip(golden.top_up_counts()) {
            *t += c;
        }
        cuts += golden.counters[5..].iter().sum::<u64>();
        assert_eq!(golden.top_ups, golden.counters[0] + golden.counters[1]);
    }
    let [full, partial, none] = totals;
    assert!(
        full > 20 && partial > 3 && none > 3 && cuts > 0,
        "non-vacuous: {totals:?}, {cuts} sell cuts"
    );
}

/// The engine_parallel_tests B-blind shapes (`b_blind_shapes_identical`:
/// full, partial and no top-ups, δ shapes, griefer bids, same-batch asks).
#[test]
fn fast_top_ups_identical_on_b_blind_shapes() {
    let mk = addr(51);
    let (t1, t2, t3, t4, b, x, y) = (
        addr(52),
        addr(53),
        addr(54),
        addr(55),
        addr(56),
        addr(57),
        addr(58),
    );
    let (t5, t6, t7, z) = (addr(59), addr(60), addr(61), addr(62));
    let (t8, t9, t10) = (addr(63), addr(64), addr(65));
    let post_only = |m: MarketId, p: i64| order(m, true, fp(p), fp(1), TimeInForce::PostOnly);
    let mut b1: Vec<_> = [1u64, 2, 3, 4, 5, 7, 9, 10, 11, 12, 13, 14, 15]
        .iter()
        .map(|&m| place(mk, gtc(m, true, 101, 10)))
        .collect();
    b1.extend(
        [
            (2u64, 110),
            (4, 102),
            (5, 130),
            (7, 130),
            (11, 102),
            (13, 102),
            (15, 110),
        ]
        .map(|(m, p)| place(mk, gtc(m, false, p, 10))),
    );
    b1.extend((16u64..=20).map(|m| place(mk, gtc(m, true, 1_000, 1))));
    let b2 = vec![
        place(t1, gtc(1, false, 200, 1)),
        place(b, gtc(2, true, 105, 10)),
        place(t2, gtc(3, false, 100, 10)),
        place(y, gtc(5, true, 120, 10)),
        place(t1, gtc(2, false, 100, 10)),
        place(x, gtc(4, true, 104, 1)),
        place(x, order(5, true, fp(120), fp(10), TimeInForce::IOC)),
        place(
            x,
            order(
                5,
                true,
                FixedPoint::from_raw(fp(120).raw() + 1),
                fp(10),
                TimeInForce::GTC,
            ),
        ),
        place(t2, gtc(4, false, 100, 10)),
        place(t3, gtc(6, false, 200, 1)),
        place(t3, gtc(5, false, 100, 10)),
        place(t4, gtc(8, false, 200, 1)),
        place(b, gtc(7, true, 105, 10)),
        place(t4, gtc(7, false, 100, 10)),
        place(t4, gtc(9, false, 100, 10)),
        place(t5, gtc(10, false, 100, 10)),
        place(x, post_only(11, 104)),
        place(t5, gtc(11, false, 100, 10)),
        place(t6, gtc(12, false, 100, 10)),
        place(x, gtc(13, true, 104, 11)),
        place(t6, gtc(13, false, 100, 10)),
        place(t7, gtc(14, false, 100, 10)),
        place(z, gtc(15, false, 104, 1)),
        place(x, post_only(15, 104)),
        place(t7, gtc(15, false, 100, 10)),
        place(t8, gtc(6, false, 200, 1)),
        place(b, gtc(16, true, 1_001, 1)),
        place(b, gtc(17, true, 1_001, 1)),
        place(b, gtc(18, true, 1_001, 1)),
        place(t8, gtc(16, false, 999, 1)),
        place(t9, gtc(6, false, 200, 1)),
        place(t9, gtc(17, false, 999, 1)),
        place(t9, gtc(18, false, 999, 1)),
        place(t10, gtc(6, false, 200, 1)),
        place(t10, gtc(19, false, 999, 1)),
        place(t10, gtc(20, false, 999, 1)),
    ];
    let mut funding = vec![(mk, fp(1_000_000))];
    for (t, a) in [
        (t1, 1_000),
        (t2, 102),
        (t3, 1_000),
        (t4, 112),
        (b, 1_000),
        (x, 1_000),
        (t5, 101),
        (t6, 102),
        (t7, 102),
        (z, 1_000),
        (t8, 1_000),
        (t10, 110),
    ] {
        funding.push((t, fp(a)));
    }
    funding.push((t9, FixedPoint::from_raw(fp(110).raw() + fp(8).raw() / 100)));
    let golden = assert_identical(
        &Scenario {
            funding,
            marks: vec![],
            batches: vec![b1, b2],
        },
        "b-blind shapes",
    );
    let [full, partial, none] = golden.top_up_counts();
    assert!(
        full > 0 && partial > 0 && none > 0,
        "non-vacuous: {:?}",
        golden.top_up_counts()
    );
}

/// exp/archy-s58-bundle-dmt: the bundle's three bridge flags (D
/// `TORUS_SETTLE_DEFER_RESULTS`, M `TORUS_PARALLEL_MATCH_SETUP`, T
/// `TORUS_FAST_SELL_TOP_UPS`) at their default (unset: on) equal all three
/// at `0` (off) on this file's fuzzed top-up scenarios, in every mode
/// (state, results, gas, counters, state root). Each flag is pinned to its
/// parse of the env value: `run` pins T; D and M are pinned around it.
#[test]
fn s58_bundle_dmt_identical_to_flags_off() {
    // On by default (unset), all off at "0": else the comparison is vacuous.
    for (env, want) in [(None, true), (Some("0"), false)] {
        assert_eq!(
            [
                parse_settle_defer_results(env),
                parse_parallel_match_setup(env),
                parse_fast_sell_top_ups(env),
            ],
            [want; 3],
            "D, M, T at {env:?}"
        );
    }
    let mut top_ups = 0;
    for seed in 0..4u64 {
        let s = fuzz_scenario(0xB1_1D00 + seed);
        for mode in MODES {
            let dmt = |env: Option<&str>| {
                with_settle_defer_results(parse_settle_defer_results(env), || {
                    with_parallel_match_setup(parse_parallel_match_setup(env), || {
                        run(&s, mode, parse_fast_sell_top_ups(env))
                    })
                })
            };
            let off = dmt(Some("0"));
            let on = dmt(None);
            assert_eq!(on, off, "seed {seed} {mode:?}: D+M+T vs flags off");
            top_ups += off.top_ups;
        }
    }
    assert!(top_ups > 0, "non-vacuous: no sell top-ups");
}
