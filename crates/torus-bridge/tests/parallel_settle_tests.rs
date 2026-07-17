//! C3 (perf/exec-scaleup): deterministic parallel Phase-4 settlement.
//!
//! Phase-3 matching is already parallel per market; Phase 4 (margin release,
//! fill application, PnL credit, trade persistence) ran on one thread. C3
//! splits settlement into a parallel per-market PURE compute pass (position
//! transitions, PnL event streams, release amounts, trade KV bytes — all
//! market-local or balance-independent) and a single-threaded deterministic
//! apply pass that performs every cross-market mutation (NativeBalance ops
//! with their order-sensitive `.min(order_margin)` clamps, trade_index
//! assignment) in exactly the sequential order.
//!
//! THE contract under test: for any batch, parallel settlement must produce
//! BYTE-IDENTICAL state to sequential settlement — balances, positions,
//! books, trade history, counters, and per-action results. The 50x rerun
//! hunts scheduling nondeterminism.

use alloy_primitives::Address;

use torus_bridge::native_executor::{NativeExecContext, NativeExecutor};
use torus_core::position::NativeBalance;
use torus_state::cf::{
    CF_NATIVE_BALANCES, CF_NATIVE_MARKETS, CF_NATIVE_ORDER_BOOKS, CF_NATIVE_POSITIONS,
    CF_NATIVE_TRADES, CF_NATIVE_USER_TRADES,
};
use torus_state::{StateBackend, StateDb};
use torus_types::{
    FixedPoint, MarketId, NativeAction, OrderType, PlaceOrderParams, TimeInForce,
};

// ---- Helpers (same idiom as maker_margin_release_tests.rs) ----

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

#[allow(clippy::too_many_arguments)]
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

/// Full observable outcome of a run: every native CF Phase 4 can touch
/// (consensus CFs AND node-local trade-history CFs), plus counters and the
/// per-action result surface. Byte-identical or bust.
#[derive(PartialEq, Eq, Debug)]
struct RunFingerprint {
    cf_dump: Vec<(String, Vec<u8>, Vec<u8>)>,
    results: Vec<Vec<(bool, Option<String>)>>,
    total_gas: Vec<u64>,
    trade_index: u32,
    next_global_order_id: u128,
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

/// Run `batches` through one context (fresh DB) with the given settle mode,
/// then fingerprint the world.
fn run_batches(batches: &[Vec<(Address, NativeAction)>], parallel: bool) -> RunFingerprint {
    let (_dir, db) = open_test_db();
    let mut ctx = make_ctx(db);
    for n in 1..=32u8 {
        fund_native(&ctx, &addr(n), fp(1_000_000));
    }

    let mut results = Vec::new();
    let mut total_gas = Vec::new();
    for batch in batches {
        let r = NativeExecutor::execute_batch_settle_mode(&mut ctx, batch, parallel);
        assert!(
            ctx.fatal_error.is_none(),
            "no batch here may go fatal: {:?}",
            ctx.fatal_error
        );
        results.push(
            r.results
                .iter()
                .map(|a| (a.success, a.error.clone()))
                .collect(),
        );
        total_gas.push(r.total_gas);
    }
    ctx.save_order_books();

    RunFingerprint {
        cf_dump: state_dump(&ctx),
        results,
        total_gas,
        trade_index: ctx.trade_index,
        next_global_order_id: ctx.next_global_order_id,
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

/// Heavy cross-market scenario: 6 markets, 24 senders (every sender trades on
/// several markets — cross-margin), two batches (seeded resting book, then a
/// crossing storm), plus a single-market third batch. Mixes GTC/IOC/FOK/
/// PostOnly, self-trade cancels, sub-lot dust rejects, tick-violation rejects,
/// multi-level sweeps, and in-batch open→close flows that realize PnL.
fn fuzz_batches(seed: u64) -> Vec<Vec<(Address, NativeAction)>> {
    let mut rng = Lcg(seed);
    let markets: [MarketId; 6] = [1, 2, 3, 4, 5, 6];

    // Batch 1: resting liquidity — makers ladder both sides of 100.
    let mut batch1 = Vec::new();
    for &m in &markets {
        for lvl in 0..4i64 {
            let maker = addr((1 + rng.below(24)) as u8);
            batch1.push(place(maker, gtc(m, true, 97 + lvl, 1 + rng.below(4) as i64)));
            let maker2 = addr((1 + rng.below(24)) as u8);
            batch1.push(place(
                maker2,
                gtc(m, false, 101 + lvl, 1 + rng.below(4) as i64),
            ));
        }
    }

    // Batch 2: the storm. ~200 orders crossing hard.
    let mut batch2 = Vec::new();
    for i in 0..200u64 {
        let m = markets[rng.below(6) as usize];
        let sender = addr((1 + rng.below(24)) as u8);
        let is_buy = rng.below(2) == 0;
        let tif = match rng.below(10) {
            0 => TimeInForce::IOC,
            1 => TimeInForce::FOK,
            2 => TimeInForce::PostOnly,
            _ => TimeInForce::GTC,
        };
        // Prices straddle the book so plenty of orders cross (buys up to 104,
        // sells down to 96), with dust in the quantity to exercise FixedPoint
        // truncation in reserve/release telescoping.
        let price = if is_buy {
            96 + rng.below(9) as i64
        } else {
            96 + rng.below(9) as i64
        };
        let qty_raw = (1 + rng.below(5)) as i128 * FixedPoint::SCALE
            + (rng.below(FixedPoint::SCALE as u64 / 2)) as i128;
        let qty = FixedPoint::from_raw(qty_raw);

        match i % 29 {
            // Sub-lot dust order — rejected by the book, margin reserved then
            // fully released (release-on-reject path).
            7 => batch2.push(place(
                sender,
                order(
                    m,
                    is_buy,
                    fp(price),
                    FixedPoint::from_raw(FixedPoint::SCALE / 2),
                    TimeInForce::GTC,
                ),
            )),
            // Tick-violation price — rejected by the book.
            13 => batch2.push(place(
                sender,
                order(
                    m,
                    is_buy,
                    FixedPoint::from_raw(price as i128 * FixedPoint::SCALE + 7),
                    qty,
                    TimeInForce::GTC,
                ),
            )),
            // Same-sender both sides on one market → self-trade cancels (STP
            // maker release path).
            19 => {
                batch2.push(place(sender, order(m, true, fp(100), qty, TimeInForce::GTC)));
                batch2.push(place(sender, order(m, false, fp(100), qty, tif)));
            }
            _ => batch2.push(place(sender, order(m, is_buy, fp(price), qty, tif))),
        }
    }

    // Batch 3: single-market batch (parallel path must handle the 1-market
    // degenerate case identically).
    let mut batch3 = Vec::new();
    for _ in 0..10 {
        let sender = addr((1 + rng.below(24)) as u8);
        let is_buy = rng.below(2) == 0;
        batch3.push(place(sender, gtc(3, is_buy, 99 + rng.below(3) as i64, 2)));
    }

    vec![batch1, batch2, batch3]
}

// ============================================================================
// 1. Differential determinism: parallel == sequential, byte-identical, 50x
// ============================================================================

#[test]
fn parallel_settle_state_identical_to_sequential_50x() {
    let batches = fuzz_batches(0xC3C3_5EED);
    let golden = run_batches(&batches, false);

    // Sanity: the scenario actually exercises settlement (fills happened).
    assert!(
        golden.trade_index > 20,
        "scenario too weak: only {} trades",
        golden.trade_index
    );

    // 50 parallel runs — any scheduling nondeterminism in the settle pipeline
    // shows up as a fingerprint mismatch.
    for run in 0..50 {
        let par = run_batches(&batches, true);
        assert_eq!(
            golden, par,
            "parallel settle diverged from sequential on run {run}"
        );
    }
}

/// A second seed, fewer reps — different fill/reject mix, same contract.
#[test]
fn parallel_settle_state_identical_second_seed() {
    let batches = fuzz_batches(0xDEAD_BEEF_0042);
    let golden = run_batches(&batches, false);
    assert!(golden.trade_index > 20, "scenario too weak");
    for run in 0..5 {
        let par = run_batches(&batches, true);
        assert_eq!(
            golden, par,
            "parallel settle diverged from sequential on run {run} (seed 2)"
        );
    }
}

// ============================================================================
// 2. Cross-market same-sender batch: cross-margin releases + PnL land exactly
// ============================================================================

#[test]
fn cross_market_same_sender_batch_exact_balances() {
    // Trader A trades on THREE markets in one batch; balances are cross-market
    // (single NativeBalance row), so every release/credit for A funnels through
    // one row — the clamp ordering is exactly what parallel settle must pin.
    let a = addr(1);
    let b = addr(2);
    let c = addr(3);
    let d = addr(4);

    // Batch 1 (seed): makers rest on three markets; A rests a far bid on
    // market 4 (order margin that must NOT be disturbed: 80*5/20 = 20).
    let batch1 = vec![
        place(b, gtc(1, false, 100, 4)), // B asks 4 @ 100 on mkt 1
        place(c, gtc(2, true, 200, 3)),  // C bids 3 @ 200 on mkt 2
        place(d, gtc(3, false, 50, 10)), // D asks 10 @ 50 on mkt 3
        place(a, gtc(4, true, 80, 5)),   // A far bid on mkt 4 (rests)
    ];

    // Batch 2: A takes on all three markets in ONE batch.
    //   mkt1: A buys 4 @ 100  (taker, fully filled)  — opens long 4 @ 100
    //   mkt2: A sells 3 @ 200 (taker, fully filled)  — opens short 3 @ 200
    //   mkt3: A buys 6 @ 50   (taker, partial: 6 of D's 10) — opens long 6 @ 50
    let batch2 = vec![
        place(a, gtc(1, true, 100, 4)),
        place(a, gtc(2, false, 200, 3)),
        place(a, gtc(3, true, 50, 6)),
    ];

    let run = |parallel: bool| -> (NativeBalance, NativeBalance, NativeBalance, NativeBalance) {
        let (_dir, db) = open_test_db();
        let mut ctx = make_ctx(db);
        for t in [a, b, c, d] {
            fund_native(&ctx, &t, fp(10_000));
        }
        let r1 = NativeExecutor::execute_batch_settle_mode(&mut ctx, &batch1, parallel);
        for (i, r) in r1.results.iter().enumerate() {
            assert!(r.success, "batch1[{i}] failed: {:?}", r.error);
        }
        let r2 = NativeExecutor::execute_batch_settle_mode(&mut ctx, &batch2, parallel);
        for (i, r) in r2.results.iter().enumerate() {
            assert!(r.success, "batch2[{i}] failed: {:?}", r.error);
        }
        (
            ctx.positions.get_native_balance(&a).unwrap(),
            ctx.positions.get_native_balance(&b).unwrap(),
            ctx.positions.get_native_balance(&c).unwrap(),
            ctx.positions.get_native_balance(&d).unwrap(),
        )
    };

    let (sa, sb, sc, sd) = run(false);
    let (pa, pb, pc, pd) = run(true);

    // Parallel == sequential, field by field.
    for (name, s, p) in [("A", &sa, &pa), ("B", &sb, &pb), ("C", &sc, &pc), ("D", &sd, &pd)] {
        assert_eq!(s.available, p.available, "{name}: available diverged");
        assert_eq!(s.order_margin, p.order_margin, "{name}: order_margin diverged");
    }

    // And the sequential values themselves are the hand-computed truth:
    // A: reserves+releases net to zero on all three filled taker orders
    //    (fills release exactly the reserve); only the mkt4 resting bid holds
    //    margin: 80*5/20 = 20. No closes → no PnL. available = 10000 - 20.
    assert_eq!(sa.order_margin, fp(20), "A: only the mkt4 resting bid margin");
    assert_eq!(sa.available, fp(10_000 - 20), "A: available");
    // B fully consumed as maker (4@100: 20 reserved, all released).
    assert_eq!(sb.order_margin, fp(0), "B: fully released");
    assert_eq!(sb.available, fp(10_000), "B: available restored");
    // C fully consumed as maker (3@200: 30 reserved, all released).
    assert_eq!(sc.order_margin, fp(0), "C: fully released");
    assert_eq!(sc.available, fp(10_000), "C: available restored");
    // D partially consumed as maker: reserved 50*10/20=25, released for 6
    // filled (25 - 50*4/20 = 15), remainder 4 still resting → 10 held.
    assert_eq!(sd.order_margin, fp(10), "D: remainder reservation held");
    assert_eq!(sd.available, fp(10_000 - 10), "D: available");
}

// ============================================================================
// 3. In-batch open→close across markets: realized PnL order pinned
// ============================================================================

#[test]
fn pnl_realizing_cross_market_flow_exact() {
    // Trader A opens AND closes within one batch on two markets at different
    // prices — realized PnL credits flow through the balance cache in fill
    // order. Counterparties open positions (no PnL). Both settle modes must
    // land A on exactly the same, hand-computed balance.
    let a = addr(1);
    let b = addr(2);
    let c = addr(3);

    // Seed: B asks 5 @ 100 (mkt1), B bids 5 @ 110 (mkt1) would self-trade —
    // use C for the exit side. C bids 5 @ 110 on mkt1: A buys 5@100 from B,
    // then sells 5@110 to C → +50 realized. On mkt2 the mirror at a loss:
    // B asks 4 @ 200, C bids 4 @ 195 → A buys 4@200, sells 4@195 → -20.
    let batch1 = vec![
        place(b, gtc(1, false, 100, 5)),
        place(c, gtc(1, true, 110, 5)),
        place(b, gtc(2, false, 200, 4)),
        place(c, gtc(2, true, 195, 4)),
    ];
    // NOTE batch1 already crosses: C's bid 110 crosses B's ask 100 on mkt1
    // (C takes B's 5@100), and C's bid 195 does NOT cross B's ask 200. So the
    // books after batch1: mkt1 has C long 5 (pos), no resting asks; mkt2 has
    // B's ask 4@200 and C's bid 4@195 resting. Trade A's flow in batch 2
    // against what actually rests.
    let batch2 = vec![
        place(a, gtc(2, true, 200, 4)),  // A buys 4 @ 200 (takes B's ask)
        place(a, gtc(2, false, 195, 4)), // A sells 4 @ 195 (hits C's bid) → close: -20
    ];

    let run = |parallel: bool| -> NativeBalance {
        let (_dir, db) = open_test_db();
        let mut ctx = make_ctx(db);
        for t in [a, b, c] {
            fund_native(&ctx, &t, fp(10_000));
        }
        let r1 = NativeExecutor::execute_batch_settle_mode(&mut ctx, &batch1, parallel);
        for (i, r) in r1.results.iter().enumerate() {
            assert!(r.success, "batch1[{i}] failed: {:?}", r.error);
        }
        let r2 = NativeExecutor::execute_batch_settle_mode(&mut ctx, &batch2, parallel);
        for (i, r) in r2.results.iter().enumerate() {
            assert!(r.success, "batch2[{i}] failed: {:?}", r.error);
        }
        ctx.positions.get_native_balance(&a).unwrap()
    };

    let seq = run(false);
    let par = run(true);
    assert_eq!(seq.available, par.available, "A available diverged");
    assert_eq!(seq.order_margin, par.order_margin, "A order_margin diverged");

    // Hand-computed: A opened long 4@200 then fully closed at 195 → realized
    // -20. All order margin reserved was released (both orders fully filled).
    assert_eq!(seq.order_margin, fp(0), "A holds no reservations");
    assert_eq!(seq.available, fp(10_000 - 20), "A realized exactly -20");
}

// ============================================================================
// 4. Default mode (env-driven) still settles a multi-market batch correctly
// ============================================================================

#[test]
fn default_mode_multi_market_settles() {
    // Whatever TORUS_PARALLEL_SETTLE says in this environment (default ON),
    // the plain execute_batch entry point must settle a cross-market batch
    // with the same observable outcome as explicit sequential mode.
    let batches = fuzz_batches(0x0BAD_F00D);
    let golden = run_batches(&batches, false);

    let (_dir, db) = open_test_db();
    let mut ctx = make_ctx(db);
    for n in 1..=32u8 {
        fund_native(&ctx, &addr(n), fp(1_000_000));
    }
    let mut results = Vec::new();
    let mut total_gas = Vec::new();
    for batch in &batches {
        let r = NativeExecutor::execute_batch(&mut ctx, batch);
        results.push(
            r.results
                .iter()
                .map(|a| (a.success, a.error.clone()))
                .collect::<Vec<_>>(),
        );
        total_gas.push(r.total_gas);
    }
    ctx.save_order_books();

    assert_eq!(golden.cf_dump, state_dump(&ctx), "default-mode state diverged");
    assert_eq!(golden.results, results, "default-mode results diverged");
    assert_eq!(golden.total_gas, total_gas, "default-mode gas diverged");
    assert_eq!(golden.trade_index, ctx.trade_index);
    assert_eq!(golden.next_global_order_id, ctx.next_global_order_id);
}
