//! s58 settle-passb-deferred-results-scatter (`TORUS_SETTLE_DEFER_RESULTS=1`):
//! the parallel settle's pass B records each placed order's book reject in a
//! 1-byte-per-action side Vec and writes every placed result in ONE
//! sequential pass after the market loop, instead of one scattered 56-byte
//! write per order (markets ascend, so consecutive writes jump ~a market's
//! worth of slots at 300 markets).
//!
//! THE contract under test: the flag changes WHEN `results[i]` is written,
//! never WHAT. For fuzzed multi-market batches (interleaved senders,
//! IOC / FOK / PostOnly rejects, STP self-trades, dust and tick rejects,
//! with and without a fill subscriber), at pass-A worker caps 1, 2 and 8,
//! the flag-on run must equal the flag-off run in every field of every
//! `NativeActionResult` (action_type, success, error, gas_used, reason —
//! `parallel_settle_tests`' fingerprint compares only success / error), the
//! total gas, every native CF, the drained fills and extras, the counters
//! and the native state root.
//!
//! The flag is pinned per thread (`with_settle_defer_results`) so both arms
//! run in one process whatever the env says; the unpinned (`plain`) run
//! follows the env and must match too.

use alloy_primitives::{Address, B256};

use torus_bridge::native_executor::{
    parse_fast_sell_top_ups, parse_parallel_match_setup, parse_settle_defer_results,
    with_fast_sell_top_ups, with_parallel_match_setup, with_settle_defer_results,
    NativeActionResult, NativeExecContext, NativeExecutor,
};
use torus_bridge::state_root::compute_native_state_root;
use torus_core::position::NativeBalance;
use torus_state::action_status::FailureReason;
use torus_state::cf::{
    CF_NATIVE_BALANCES, CF_NATIVE_MARKETS, CF_NATIVE_ORDER_BOOKS, CF_NATIVE_POSITIONS,
    CF_NATIVE_TRADES, CF_NATIVE_USER_TRADES,
};
use torus_state::trade_rows::{FillExtras, TradeFill};
use torus_state::{StateBackend, StateDb};
use torus_types::{FixedPoint, MarketId, NativeAction, OrderType, PlaceOrderParams, TimeInForce};

// ---- Helpers (copied from parallel_settle_tests.rs) ----

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

fn make_ctx(state_db: StateDb) -> NativeExecContext {
    NativeExecContext::new(
        state_db,
        1,         // block_height
        1000,      // timestamp
        0,         // epoch
        100,       // epoch_length
        10,        // max_validators
        addr(99),  // proposer
        addr(100), // treasury
        addr(101), // dev_pool
    )
}

fn fund_native(ctx: &NativeExecContext, trader: &Address, amount: FixedPoint) {
    let bal = NativeBalance {
        available: amount,
        order_margin: FixedPoint::ZERO,
    };
    ctx.positions.put_native_balance(trader, &bal).unwrap();
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

/// Every field of one `NativeActionResult`.
type ResultRow = (&'static str, bool, Option<String>, u64, FailureReason);

fn row(a: &NativeActionResult) -> ResultRow {
    (
        a.action_type,
        a.success,
        a.error.clone().map(String::from),
        a.gas_used,
        a.reason,
    )
}

/// Full observable outcome of a run, the whole result surface included.
#[derive(PartialEq, Eq, Debug)]
struct Fingerprint {
    cf_dump: Vec<(String, Vec<u8>, Vec<u8>)>,
    results: Vec<Vec<ResultRow>>,
    total_gas: Vec<u64>,
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
    ];
    let mut out = Vec::new();
    for cf in cfs {
        let entries = ctx.state.iterate_cf(cf, None).expect("iterate cf");
        for (k, v) in entries {
            out.push((cf.to_string(), k, v));
        }
    }
    out
}

/// Run `batches` through one fresh context on the forced parallel settle
/// with the pass-A cap pinned. Panics if a batch goes fatal or a parallel
/// settle falls back to the sequential loop (pass B must be what runs).
fn run(batches: &[Vec<(Address, NativeAction)>], cap: usize, record_fills: bool) -> Fingerprint {
    let (_dir, db) = open_test_db();
    let mut ctx = make_ctx(db);
    ctx.record_fills = record_fills;
    for n in 1..=32u8 {
        fund_native(&ctx, &addr(n), fp(1_000_000));
    }

    let mut results = Vec::new();
    let mut total_gas = Vec::new();
    let mut fills = Vec::new();
    let mut extras = Vec::new();
    for batch in batches {
        let r = NativeExecutor::execute_batch_settle_workers(&mut ctx, batch, true, Some(cap));
        assert!(
            ctx.fatal_error.is_none(),
            "no batch here may go fatal: {:?}",
            ctx.fatal_error
        );
        results.push(r.results.iter().map(row).collect());
        total_gas.push(r.total_gas);
        let (f, e) = ctx.take_pending_fills_and_extras();
        assert_eq!(
            e.len(),
            if record_fills { f.len() } else { 0 },
            "extras: aligned or empty"
        );
        fills.push(f);
        extras.push(e);
    }
    // Pass B ran: the parallel path (pass A > 0) with no sequential fallback.
    assert!(
        ctx.phase_accum.settle_pass_a_ns > 0,
        "the parallel settle never ran"
    );
    assert_eq!(
        ctx.phase_accum.settle_fallbacks, 0,
        "a parallel settle fell back"
    );
    ctx.save_order_books();

    Fingerprint {
        cf_dump: state_dump(&ctx),
        results,
        total_gas,
        fills,
        extras,
        trade_index: ctx.trade_index,
        next_global_order_id: ctx.next_global_order_id,
        state_root: compute_native_state_root(&ctx.state).expect("state root"),
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

/// Multi-market scenario (adapted from `parallel_settle_tests::fuzz_batches`):
/// `markets` markets, 24 interleaved senders. Batch 1 rests a ladder on
/// every market; batch 2 is a crossing storm drawing a market PER ORDER
/// (the bench shape that scatters pass B's writes), mixing GTC / IOC / FOK /
/// PostOnly (book rejects: `result.reject` is `Some`), dust and tick
/// rejects, STP self-trades and IOCs far from the book; batch 3 storms again
/// on a book the first two left behind.
fn fuzz_batches(seed: u64, markets: u64, storm: u64) -> Vec<Vec<(Address, NativeAction)>> {
    let mut rng = Lcg(seed);
    let markets: Vec<MarketId> = (1..=markets).collect();

    let mut batch1 = Vec::new();
    for &m in &markets {
        for lvl in 0..4i64 {
            let maker = addr((1 + rng.below(24)) as u8);
            batch1.push(place(
                maker,
                gtc(m, true, 97 + lvl, 1 + rng.below(4) as i64),
            ));
            let maker2 = addr((1 + rng.below(24)) as u8);
            batch1.push(place(
                maker2,
                gtc(m, false, 101 + lvl, 1 + rng.below(4) as i64),
            ));
        }
    }

    let storm_batch = |rng: &mut Lcg| {
        let mut batch = Vec::new();
        for i in 0..storm {
            let m = markets[rng.below(markets.len() as u64) as usize];
            let sender = addr((1 + rng.below(24)) as u8);
            let is_buy = rng.below(2) == 0;
            let tif = match rng.below(10) {
                0 | 1 => TimeInForce::IOC,
                2 => TimeInForce::FOK,
                3 | 4 => TimeInForce::PostOnly,
                _ => TimeInForce::GTC,
            };
            let price = 96 + rng.below(9) as i64;
            let qty_raw = (1 + rng.below(5)) as i128 * FixedPoint::SCALE
                + (rng.below(FixedPoint::SCALE as u64 / 2)) as i128;
            let qty = FixedPoint::from_raw(qty_raw);

            match i % 31 {
                // Sub-lot dust: rejected by the book.
                7 => batch.push(place(
                    sender,
                    order(
                        m,
                        is_buy,
                        fp(price),
                        FixedPoint::from_raw(FixedPoint::SCALE / 2),
                        TimeInForce::GTC,
                    ),
                )),
                // Off-tick price: rejected.
                13 => batch.push(place(
                    sender,
                    order(
                        m,
                        is_buy,
                        FixedPoint::from_raw(price as i128 * FixedPoint::SCALE + 7),
                        qty,
                        TimeInForce::GTC,
                    ),
                )),
                // Same sender both sides of one market: STP cancels.
                19 => {
                    batch.push(place(
                        sender,
                        order(m, true, fp(100), qty, TimeInForce::GTC),
                    ));
                    batch.push(place(sender, order(m, false, fp(100), qty, tif)));
                }
                // IOC far from the book: IocCancel.
                23 => batch.push(place(
                    sender,
                    order(
                        m,
                        is_buy,
                        fp(if is_buy { 50 } else { 150 }),
                        qty,
                        TimeInForce::IOC,
                    ),
                )),
                // FOK larger than the book: FokCancel.
                29 => batch.push(place(
                    sender,
                    order(
                        m,
                        is_buy,
                        fp(if is_buy { 104 } else { 96 }),
                        fp(200),
                        TimeInForce::FOK,
                    ),
                )),
                _ => batch.push(place(sender, order(m, is_buy, fp(price), qty, tif))),
            }
        }
        batch
    };
    let batch2 = storm_batch(&mut rng);
    let batch3 = storm_batch(&mut rng);
    vec![batch1, batch2, batch3]
}

/// Placed-order book rejects in a run (`gas_used` charged, not a success):
/// the rows the deferred pass materializes with `Some(reason)`.
fn placed_rejects(f: &Fingerprint) -> Vec<FailureReason> {
    f.results
        .iter()
        .flatten()
        .filter(|r| r.0 == "place_order" && !r.1 && r.3 > 0 && r.4 != FailureReason::Fill)
        .map(|r| r.4)
        .collect()
}

fn assert_defer_equivalent(seed: u64, markets: u64, storm: u64) {
    let batches = fuzz_batches(seed, markets, storm);
    for record_fills in [true, false] {
        for cap in [1usize, 2, 8] {
            let off = with_settle_defer_results(false, || run(&batches, cap, record_fills));
            // The scenario must exercise what the deferred pass writes:
            // fills, placed successes AND placed book rejects of several kinds.
            assert!(
                off.trade_index > 20,
                "scenario too weak: {} trades",
                off.trade_index
            );
            let rejects = placed_rejects(&off);
            assert!(
                rejects.len() >= 10,
                "too few placed rejects: {}",
                rejects.len()
            );
            for want in [FailureReason::BadAloPx, FailureReason::IocCancel] {
                assert!(rejects.contains(&want), "scenario never hits {want:?}");
            }
            assert!(
                off.results
                    .iter()
                    .flatten()
                    .filter(|r| r.0 == "place_order" && r.1)
                    .count()
                    > 20,
                "too few placed successes"
            );

            let on = with_settle_defer_results(true, || run(&batches, cap, record_fills));
            assert_eq!(
                off, on,
                "deferred results diverged (seed {seed:#x}, {markets} markets, cap {cap}, record_fills {record_fills})"
            );
            let plain = run(&batches, cap, record_fills);
            assert_eq!(
                off, plain,
                "unpinned (env) run diverged (seed {seed:#x}, cap {cap}, record_fills {record_fills})"
            );
        }
    }
}

#[test]
fn settle_defer_results_identical_six_markets() {
    assert_defer_equivalent(0xC3C3_5EED, 6, 200);
}

#[test]
fn settle_defer_results_identical_second_seed() {
    assert_defer_equivalent(0xDEAD_BEEF_0042, 3, 160);
}

/// Wider: 24 markets, a market per order, so pass B's writes are scattered
/// across the whole results slice (the 300-market bench shape, scaled).
#[test]
fn settle_defer_results_identical_wide_scatter() {
    assert_defer_equivalent(0x5E77_1E00_0058, 24, 600);
}

/// Back to back on one context with the flag flipping between calls: the
/// deferred buffer is per call (`execute_batch` runs twice per block, pre-
/// and post-EVM), so no call sees another's records.
#[test]
fn settle_defer_results_flag_flip_between_calls() {
    let batches = fuzz_batches(0xF11F_0058, 5, 220);
    let off = with_settle_defer_results(false, || run(&batches, 2, true));

    let (_dir, db) = open_test_db();
    let mut ctx = make_ctx(db);
    ctx.record_fills = true;
    for n in 1..=32u8 {
        fund_native(&ctx, &addr(n), fp(1_000_000));
    }
    let mut results = Vec::new();
    let mut total_gas = Vec::new();
    for (i, batch) in batches.iter().enumerate() {
        let r = with_settle_defer_results(i % 2 == 1, || {
            NativeExecutor::execute_batch_settle_workers(&mut ctx, batch, true, Some(2))
        });
        assert!(ctx.fatal_error.is_none());
        results.push(r.results.iter().map(row).collect::<Vec<_>>());
        total_gas.push(r.total_gas);
        let _ = ctx.take_pending_fills_and_extras();
    }
    assert_eq!(off.results, results);
    assert_eq!(off.total_gas, total_gas);
}

/// exp/archy-s58-bundle-dmt: the bundle's three bridge flags (D
/// `TORUS_SETTLE_DEFER_RESULTS`, M `TORUS_PARALLEL_MATCH_SETUP`, T
/// `TORUS_FAST_SELL_TOP_UPS`) at their default (unset: on) equal all three
/// at `0` (off) on this file's pass-B scenarios (every result field, gas,
/// CFs, fills, counters, state root), at every pass-A cap. Each flag is
/// pinned to its parse of the env value (env vars race across test threads).
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
    let all = |env: Option<&str>, f: &dyn Fn() -> Fingerprint| {
        with_settle_defer_results(parse_settle_defer_results(env), || {
            with_parallel_match_setup(parse_parallel_match_setup(env), || {
                with_fast_sell_top_ups(parse_fast_sell_top_ups(env), f)
            })
        })
    };
    for (seed, markets, storm) in [(0xC3C3_5EED, 6, 200), (0x5E77_1E00_0058, 24, 600)] {
        let batches = fuzz_batches(seed, markets, storm);
        for record_fills in [true, false] {
            for cap in [1usize, 2, 8] {
                let off = all(Some("0"), &|| run(&batches, cap, record_fills));
                let on = all(None, &|| run(&batches, cap, record_fills));
                assert_eq!(
                    off, on,
                    "D+M+T diverged (seed {seed:#x}, {markets} markets, cap {cap}, record_fills {record_fills})"
                );
            }
        }
    }
}
