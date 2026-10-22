//! Perp funding engine (docs/plans/funding-engine.md, P0.0 `funding_engine`).
//!
//! - Settlement on touch: a fill pays the position's funding since its entry
//!   index in cash, through every settle path (single action, sequential,
//!   forced parallel at caps 1 / 2 / 8, the engine, L2.1 streamed on and
//!   off), and untouched positions keep it pending.
//! - Pending funding counts in margin: liquidation sees it, and the stage-1
//!   close settles it.
//! - Determinism: a multi-market block stream with premium samples and
//!   hourly (here 600 s) settlements gives byte-identical rows and native
//!   root in every settle mode, with and without the resident rows / sums
//!   cache, and across a restart (and a crash-replay) mid-interval.
//! - Activation at H: blocks before H are byte-identical to a run that never
//!   activates; from H the rows exist.
//! - Governance keys are accepted only while the feature is active.
//! - Cost: the block step's reads and writes do not depend on the number of
//!   positions (O(markets)); it never reads a position or balance row.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use alloy_primitives::{Address, B256};

use torus_bridge::native_executor::{
    begin_resident, end_resident, with_settle_streamed, NativeActionResult, NativeBatchResult, NativeExecContext,
    NativeExecutor, ResidentBooks,
};
use torus_bridge::state_root::compute_native_state_root;
use torus_core::funding::{self, FundingParams, FundingRow, INDEX_SCALE};
use torus_core::margin::AccountView;
use torus_core::position::{MarginType, NativeBalance, Position, PositionManager};
use torus_state::cf::{
    CF_CONSENSUS_META, CF_FEE_CONFIG, CF_NATIVE_BALANCES, CF_NATIVE_MARKETS, CF_NATIVE_ORACLE, CF_NATIVE_POSITIONS,
    META_NATIVE_APPLIED_HEIGHT,
};
use torus_state::{AtomicWriteOp, NativeStateOverlay, StateBackend, StateDb, StateError};
use torus_types::{
    Feature, FixedPoint, MarketId, NativeAction, OrderType, PlaceOrderParams, Proposal, ProposalAction, TimeInForce,
};

const FUND: u128 = Feature::FundingEngine.bit();
const E18: i128 = INDEX_SCALE;

fn addr(n: u8) -> Address {
    Address::new([n; 20])
}

fn fp(v: i64) -> FixedPoint {
    FixedPoint::from_raw(i128::from(v) * FixedPoint::SCALE)
}

fn order(m: MarketId, is_buy: bool, px: i64, qty: i64, tif: TimeInForce) -> PlaceOrderParams {
    PlaceOrderParams {
        market_id: m,
        is_buy,
        price: fp(px),
        quantity: fp(qty),
        order_type: OrderType::Limit,
        time_in_force: tif,
        reduce_only: false,
        client_order_id: None,
    }
}

fn place(t: Address, p: PlaceOrderParams) -> (Address, NativeAction) {
    (t, NativeAction::PlaceOrder(p))
}

/// The stored aggregate row: price(16) ‖ block(8) ‖ reporters(4) ‖ ts(8).
fn set_mark<S: StateBackend>(s: &S, m: MarketId, price: FixedPoint, ts: u64) {
    let row = [price.raw().to_be_bytes().as_slice(), &7u64.to_be_bytes(), &3u32.to_be_bytes(), &ts.to_be_bytes()]
        .concat();
    s.put_cf_raw(CF_NATIVE_ORACLE, &[b"agg".as_slice(), &m.to_be_bytes()].concat(), &row).unwrap();
}

fn set_param(db: &StateDb, key: &str, v: u64) {
    db.put_cf_raw(CF_FEE_CONFIG, key.as_bytes(), v.to_string().as_bytes()).unwrap();
}

/// `markets` listed (placeholder rows: tick / lot 1, default 20x), traders
/// 1..=`traders` funded with `fund`.
fn setup(markets: u64, traders: u8, fund: i64) -> (tempfile::TempDir, StateDb) {
    let dir = tempfile::tempdir().unwrap();
    let db = StateDb::open(dir.path()).unwrap();
    for m in 1..=markets {
        db.put_cf_raw(CF_NATIVE_MARKETS, &m.to_be_bytes(), b"listed").unwrap();
    }
    let pm = PositionManager::new(db.clone());
    for n in 1..=traders {
        pm.put_native_balance(&addr(n), &NativeBalance { available: fp(fund), order_margin: FixedPoint::ZERO })
            .unwrap();
    }
    (dir, db)
}

fn ctx_at<S: StateBackend>(state: S, h: u64, ts: u64, features: u128) -> NativeExecContext<S> {
    let mut ctx = NativeExecContext::new(state, h, ts, 0, 1_000, 10, addr(99), addr(100), addr(101));
    ctx.features = features;
    ctx
}

fn row_of<S: StateBackend>(s: &S, m: MarketId) -> Option<FundingRow> {
    funding::read_row(s, m).unwrap()
}

fn cash<S: StateBackend>(s: &S, t: &Address) -> FixedPoint {
    let b = PositionManager::new(s.clone()).get_native_balance(t).unwrap();
    b.available + b.order_margin
}

fn position<S: StateBackend>(s: &S, t: &Address, m: MarketId) -> Option<Position> {
    PositionManager::new(s.clone()).get_position(t, m).unwrap()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    /// Every action through `NativeExecutor::execute` (the single path).
    Single,
    Seq,
    Par(usize),
    Engine(usize),
}

fn exec<S: StateBackend>(ctx: &mut NativeExecContext<S>, batch: &[(Address, NativeAction)], mode: Mode, streamed: bool) -> Vec<NativeActionResult> {
    with_settle_streamed(streamed, || match mode {
        Mode::Single => batch.iter().map(|(t, a)| NativeExecutor::execute(ctx, t, a)).collect(),
        Mode::Seq => NativeExecutor::execute_batch_settle_workers(ctx, batch, false, Some(1)).results,
        Mode::Par(cap) => NativeExecutor::execute_batch_settle_workers(ctx, batch, true, Some(cap)).results,
        Mode::Engine(t) => {
            let r: NativeBatchResult = NativeExecutor::execute_batch_engine_mode(ctx, batch, t);
            r.results
        }
    })
}

const MODES: [(Mode, bool); 8] = [
    (Mode::Single, false),
    (Mode::Seq, false),
    (Mode::Par(1), false),
    (Mode::Par(2), true),
    (Mode::Par(8), false),
    (Mode::Par(8), true),
    (Mode::Engine(1), false),
    (Mode::Engine(4), true),
];

// ---- Settlement on touch ----

/// Block 1 (ts 1000): A buys 10 @ 100 from B (and a filler pair trades market
/// 2, so the batch paths settle two markets). The block-start step created
/// the rows and took one sample on the empty book: premium 0. Block 2 at ts
/// 3600 settles the first interval at the interest rate (0.01% / 8 h →
/// 12.5 ppm) × mark 100 = index 1.25e15, so A owes 10 × 0.00125 = 0.0125
/// and B is owed as much. Block 2: A sells 4 @ 100 to C (closing at its
/// entry: trade PnL 0). A pays exactly 0.0125 in cash, its entry index moves
/// to the index; B (untouched) keeps it pending; C opens at the index.
/// RED before the funding engine: no rows, nothing paid.
#[test]
fn a_fill_settles_the_positions_funding_in_cash_in_every_settle_path() {
    for (mode, streamed) in MODES {
        let label = format!("{mode:?} streamed {streamed}");
        let (_d, db) = setup(2, 8, 1_000_000);
        let (a, b, c, f1, f2) = (addr(1), addr(2), addr(3), addr(4), addr(5));
        // Block 1.
        let mut ctx = ctx_at(db.clone(), 1, 1_000, FUND);
        set_mark(&ctx.state, 1, fp(100), 1_000);
        set_mark(&ctx.state, 2, fp(100), 1_000);
        let _ = NativeExecutor::begin_block_oracle(&mut ctx);
        let r = exec(
            &mut ctx,
            &[
                place(b, order(1, false, 100, 10, TimeInForce::GTC)),
                place(f1, order(2, false, 100, 1, TimeInForce::GTC)),
                place(a, order(1, true, 100, 10, TimeInForce::IOC)),
                place(f2, order(2, true, 100, 1, TimeInForce::IOC)),
            ],
            mode,
            streamed,
        );
        assert!(r.iter().all(|x| x.success), "{label}: {r:?}");
        assert!(ctx.fatal_error.is_none(), "{label}: {:?}", ctx.fatal_error);
        ctx.save_order_books();
        drop(ctx);
        let r1 = row_of(&db, 1).expect("row created at activation");
        assert_eq!((r1.cum_index, r1.samples, r1.premium_sum, r1.interval_start), (0, 1, 0, 0), "{label}");
        let pa = position(&db, &a, 1).unwrap();
        assert_eq!(pa.funding.map(|f| f.entry_index), Some(0), "{label}: opened at index 0 (v3)");
        let a_cash = cash(&db, &a);

        // Block 2: settle, then A's partial close touches its position.
        let mut ctx = ctx_at(db.clone(), 2, 3_600, FUND);
        set_mark(&ctx.state, 1, fp(100), 3_600);
        set_mark(&ctx.state, 2, fp(100), 3_600);
        let _ = NativeExecutor::begin_block_oracle(&mut ctx);
        let idx = row_of(&ctx.state, 1).unwrap().cum_index;
        assert_eq!(idx, E18 / 800, "{label}: 12.5 ppm × 100 = 0.00125 per unit");
        assert_eq!(position(&ctx.state, &b, 1).unwrap().funding_owed(Some(idx)), -FixedPoint::from_raw(1_250_000));
        let r = exec(
            &mut ctx,
            &[
                place(c, order(1, true, 100, 4, TimeInForce::GTC)),
                place(f2, order(2, false, 100, 1, TimeInForce::GTC)),
                place(a, order(1, false, 100, 4, TimeInForce::IOC)),
                place(f1, order(2, true, 100, 1, TimeInForce::IOC)),
            ],
            mode,
            streamed,
        );
        assert!(r.iter().all(|x| x.success), "{label}: {r:?}");
        assert!(ctx.fatal_error.is_none(), "{label}: {:?}", ctx.fatal_error);
        ctx.save_order_books();
        drop(ctx);
        let paid = FixedPoint::from_raw(1_250_000); // 0.0125
        assert_eq!(cash(&db, &a), a_cash - paid, "{label}: A paid its funding in cash");
        let pa = position(&db, &a, 1).unwrap();
        assert_eq!(pa.size, fp(6), "{label}");
        assert_eq!(pa.funding.map(|f| (f.entry_index, f.paid)), Some((idx, paid)), "{label}");
        assert_eq!(pa.entry_price, fp(100), "{label}: entry price untouched by funding");
        let pb = position(&db, &b, 1).unwrap();
        assert_eq!(pb.funding.map(|f| f.entry_index), Some(0), "{label}: B untouched");
        assert_eq!(pb.funding_since_open(Some(idx)), -paid, "{label}: B's receipt is pending");
        let pc = position(&db, &c, 1).unwrap();
        assert_eq!(pc.funding.map(|f| (f.entry_index, f.paid)), Some((idx, FixedPoint::ZERO)), "{label}");
    }
}

/// A touch moves the owed amount from UPnL to cash: the account's equity
/// (cash + funded UPnL) is the same before and after a fill at the mark.
#[test]
fn a_touch_is_equity_neutral() {
    let (_d, db) = setup(1, 4, 1_000_000);
    let pm = {
        let mut pm = PositionManager::new(db.clone());
        pm.set_funding(Some(Arc::new([(1u64, 0i128)].into_iter().collect())));
        pm
    };
    pm.apply_fill(&addr(1), 1, true, fp(10), fp(100), MarginType::Cross).unwrap();
    pm.apply_fill(&addr(2), 1, false, fp(10), fp(100), MarginType::Cross).unwrap();
    let idx = 7 * E18 / 3; // inexact on purpose
    let mut pm = pm;
    pm.set_funding(Some(Arc::new([(1u64, idx)].into_iter().collect())));
    let equity = |t: &Address| {
        let bal = pm.get_native_balance(t).unwrap();
        let ps = pm.positions_for_trader(t).unwrap();
        AccountView::build_funded(&bal, &ps, |_| Some(fp(100)), |_| Ok(Some(idx)), |_| None).unwrap().equity()
    };
    let before = [equity(&addr(1)), equity(&addr(2))];
    assert_ne!(before[0], fp(1_000_000), "pending funding is in the long's equity");
    // Increase (no close part): the cash change is the funding alone.
    pm.apply_fill(&addr(1), 1, true, fp(1), fp(100), MarginType::Cross).unwrap();
    pm.apply_fill(&addr(2), 1, false, fp(1), fp(100), MarginType::Cross).unwrap();
    let after = [equity(&addr(1)), equity(&addr(2))];
    assert_eq!(before, after, "settling on touch never changes equity");
    assert_ne!(pm.get_native_balance(&addr(1)).unwrap().available, fp(1_000_000), "paid in cash");
    // Funding never creates value: Σ cash moved <= 0.
    let total = cash(&db, &addr(1)) + cash(&db, &addr(2));
    assert!(total <= fp(2_000_000), "{total:?}");
}

// ---- Liquidation ----

/// t: long 10 @ 1000 with 350 of collateral, mark 990: equity 250 >= MM
/// 247.5 (20x), healthy without funding. With 6 per unit of pending funding
/// (owed 60) its equity is 190 < MM: stage 1 closes it into the book at 985,
/// and the close settles the funding: t keeps 350 − 150 − 60 = 140. RED
/// before the funding engine: the pending funding was not in the account
/// value, no liquidation.
#[test]
fn pending_funding_counts_in_liquidation_and_the_close_settles_it() {
    for with_funding in [false, true] {
        let (_d, db) = setup(1, 0, 0);
        let mut ctx = ctx_at(db.clone(), 1, 1_001, if with_funding { FUND } else { 0 });
        let (t, s, m) = (addr(1), addr(2), addr(3));
        for (who, amount) in [(t, 350), (s, 1_000_000), (m, 1_000_000)] {
            ctx.positions
                .put_native_balance(&who, &NativeBalance { available: fp(amount), order_margin: FixedPoint::ZERO })
                .unwrap();
        }
        ctx.positions.apply_fill(&t, 1, true, fp(10), fp(1_000), MarginType::Cross).unwrap();
        ctx.positions.apply_fill(&s, 1, false, fp(10), fp(1_000), MarginType::Cross).unwrap();
        let r = NativeExecutor::execute(&mut ctx, &m, &NativeAction::PlaceOrder(order(1, true, 985, 10, TimeInForce::GTC)));
        assert!(r.success, "{:?}", r.error);
        set_mark(&ctx.state, 1, fp(990), 1_001);
        if with_funding {
            let mut row = FundingRow::new(1_001, &FundingParams::default());
            row.cum_index = 6 * E18;
            funding::write_row(&ctx.state, 1, &row).unwrap();
            ctx.positions.set_funding(Some(Arc::new([(1u64, 6 * E18)].into_iter().collect())));
        }
        NativeExecutor::run_liquidations(&mut ctx);
        assert!(ctx.fatal_error.is_none(), "{:?}", ctx.fatal_error);
        let left = position(&ctx.state, &t, 1).map(|p| p.size);
        if with_funding {
            assert_eq!(left, None, "liquidated");
            assert_eq!(cash(&ctx.state, &t), fp(140), "350 − 150 trade loss − 60 funding");
            let bought = position(&ctx.state, &m, 1).unwrap();
            assert_eq!(bought.funding.map(|f| f.entry_index), Some(6 * E18), "the buyer starts at the index");
        } else {
            assert_eq!(left, Some(fp(10)), "healthy without funding");
        }
    }
}

/// Codex P1 (s138): a stale mark must not erase funding debt in the
/// production withdrawal check. t: cash 1000, long 1 @ 100 opened at index
/// 0, index 800 per unit (owes 800): equity 200 with or without a mark, so a
/// withdrawal of 800 is refused as under-margined in both cases. RED before
/// the fix: without a mark the funding term was dropped (equity 1000) and
/// the withdrawal went through.
#[test]
fn a_stale_mark_keeps_pending_funding_in_the_withdrawal_check() {
    for fresh in [true, false] {
        let (_d, db) = setup(1, 0, 0);
        let mut ctx = ctx_at(db.clone(), 1, 1_000, FUND);
        let (t, s) = (addr(1), addr(2));
        for (who, amount) in [(t, 1_000), (s, 1_000_000)] {
            ctx.positions
                .put_native_balance(&who, &NativeBalance { available: fp(amount), order_margin: FixedPoint::ZERO })
                .unwrap();
        }
        let mut row = FundingRow::new(1_000, &FundingParams::default());
        funding::write_row(&ctx.state, 1, &row).unwrap();
        ctx.positions.set_funding(Some(Arc::new([(1u64, 0i128)].into_iter().collect())));
        ctx.positions.apply_fill(&t, 1, true, fp(1), fp(100), MarginType::Cross).unwrap();
        ctx.positions.apply_fill(&s, 1, false, fp(1), fp(100), MarginType::Cross).unwrap();
        row.cum_index = 800 * E18;
        funding::write_row(&ctx.state, 1, &row).unwrap();
        ctx.positions.set_funding(Some(Arc::new([(1u64, 800 * E18)].into_iter().collect())));
        if fresh {
            set_mark(&ctx.state, 1, fp(100), 1_000);
        }
        let w = NativeAction::Withdraw { amount: wei(800), to: addr(50) };
        let r = NativeExecutor::execute(&mut ctx, &t, &w);
        assert!(!r.success, "fresh {fresh}: withdrew 800 against 200 of equity");
        let why = r.error.clone().unwrap_or_default();
        assert!(why.contains("under-margined"), "fresh {fresh}: {why}");
        assert_eq!(cash(&ctx.state, &t), fp(1_000), "fresh {fresh}: nothing left the account");
    }
}

/// Codex P1 (s138): a drain spanning a funding settlement. Bankrupt u's long
/// 10 @ 100 sits in the long escrow (obligation row at block 1, index 0);
/// the index moves to 6 per unit; the liquidation step's drain closes the
/// escrow against c at 100. The escrow is exempt (adl-budget §8): it pays
/// nothing, so it ends at exactly 0 with no dust; its share (60) is charged
/// to the vault at the touch, c still receives its 60 and the cash total is
/// unchanged (zero-sum). RED before the fix: the escrow paid the 60 and the
/// drain swept it to the vault as 60 tokens of "dust".
#[test]
fn a_drain_across_a_funding_settlement_leaves_the_escrow_exempt() {
    use torus_core::liquidation::{put_obligation, transfer, Obligation, ADL_ESCROW_LONG, LIQUIDATOR_VAULT};
    let (_d, db) = setup(1, 0, 0);
    let mut ctx = ctx_at(db.clone(), 2, 1_000, FUND);
    let metrics = Arc::new(torus_telemetry::Metrics::new());
    ctx.metrics = Some(metrics.clone());
    let (u, c) = (addr(1), addr(2));
    for (who, amount) in [(u, 1_000), (c, 1_000_000)] {
        ctx.positions
            .put_native_balance(&who, &NativeBalance { available: fp(amount), order_margin: FixedPoint::ZERO })
            .unwrap();
    }
    let mut row = FundingRow::new(1_000, &FundingParams::default());
    funding::write_row(&ctx.state, 1, &row).unwrap();
    ctx.positions.set_funding(Some(Arc::new([(1u64, 0i128)].into_iter().collect())));
    ctx.positions.apply_fill(&u, 1, true, fp(10), fp(100), MarginType::Cross).unwrap();
    ctx.positions.apply_fill(&c, 1, false, fp(10), fp(100), MarginType::Cross).unwrap();
    // Block B = 1: u's long moves to the escrow, its obligation is queued.
    transfer(&ctx.positions, &u, &ADL_ESCROW_LONG, 1, fp(10), fp(100)).unwrap();
    let o = Obligation { height: 1, market: 1, is_long: true, trader: u, size: fp(10), price: fp(100) };
    put_obligation(&ctx.state, &o).unwrap();
    let total = |s: &StateDb| [u, c, ADL_ESCROW_LONG, LIQUIDATOR_VAULT].iter().map(|a| cash(s, a)).fold(FixedPoint::ZERO, |a, x| a + x);
    let before = total(&db);
    // A funding settlement before the drain.
    row.cum_index = 6 * E18;
    funding::write_row(&ctx.state, 1, &row).unwrap();
    ctx.positions.set_funding(Some(Arc::new([(1u64, 6 * E18)].into_iter().collect())));
    set_mark(&ctx.state, 1, fp(100), 1_000);
    NativeExecutor::run_liquidations(&mut ctx);
    assert!(ctx.fatal_error.is_none(), "{:?}", ctx.fatal_error);
    assert!(position(&db, &ADL_ESCROW_LONG, 1).is_none(), "drained");
    assert_eq!(position(&db, &c, 1).map(|p| p.size), None, "c closed against the escrow");
    assert_eq!(cash(&db, &ADL_ESCROW_LONG), FixedPoint::ZERO);
    assert_eq!(metrics.liquidation_adl_dust.get(), 0.0, "no funding swept as dust");
    assert_eq!(cash(&db, &c), fp(1_000_000) + fp(60), "c receives its funding");
    assert_eq!(cash(&db, &LIQUIDATOR_VAULT), -fp(60), "the vault carries the escrow's share");
    assert_eq!(total(&db), before, "zero-sum");
}

// ---- Determinism / activation / restart ----

/// One block: its timestamp, each market's mark, its two batches.
struct Block {
    ts: u64,
    marks: Vec<(MarketId, i64)>,
    batches: [Vec<(Address, NativeAction)>; 2],
}

struct Lcg(u64);
impl Lcg {
    fn below(&mut self, n: u64) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (self.0 >> 33) % n
    }
}

const MARKETS: u64 = 4;
const TRADERS: u8 = 24;
const BLOCKS: u64 = 24;

/// A ladder on every market, then crossing storms (GTC / IOC, opens,
/// reduces, flips, closes), marks wandering 95..=105 around a book at
/// 96..=104 (premium samples of both signs), 250 s per block: a sample every
/// block and (interval 600 s) a settlement about every other block.
fn stream(seed: u64) -> Vec<Block> {
    let mut rng = Lcg(seed);
    let mut ladder = Vec::new();
    for m in 1..=MARKETS {
        for lvl in 0..4i64 {
            ladder.push(place(addr(1 + rng.below(u64::from(TRADERS)) as u8), order(m, true, 97 + lvl, 1 + rng.below(4) as i64, TimeInForce::GTC)));
            ladder.push(place(addr(1 + rng.below(u64::from(TRADERS)) as u8), order(m, false, 101 + lvl, 1 + rng.below(4) as i64, TimeInForce::GTC)));
        }
    }
    let storm = |rng: &mut Lcg| -> Vec<(Address, NativeAction)> {
        (0..30)
            .map(|i| {
                let m = 1 + rng.below(MARKETS);
                let t = addr(1 + rng.below(u64::from(TRADERS)) as u8);
                let tif = if rng.below(3) == 0 { TimeInForce::IOC } else { TimeInForce::GTC };
                if i % 17 == 16 {
                    (t, NativeAction::CancelAllOrders { market_id: Some(m) })
                } else {
                    place(t, order(m, rng.below(2) == 0, 96 + rng.below(9) as i64, 1 + rng.below(5) as i64, tif))
                }
            })
            .collect()
    };
    (1..=BLOCKS)
        .map(|h| {
            let marks = (1..=MARKETS).map(|m| (m, 95 + rng.below(11) as i64)).collect();
            let first = if h == 1 { ladder.clone() } else { storm(&mut rng) };
            let second = storm(&mut rng);
            Block { ts: 1_000 + 250 * h, marks, batches: [first, second] }
        })
        .collect()
}

#[derive(Clone, Copy, Debug)]
struct Opts {
    mode: Mode,
    streamed: bool,
    resident: bool,
    /// First height with the feature bit (`None`: never).
    activate_at: Option<u64>,
    /// Drop the resident rows (a node restart) before this height.
    restart_at: Option<u64>,
    /// Execute this height once without committing it (a crash before the
    /// flush), restart, and execute it again.
    crash_at: Option<u64>,
}

impl Opts {
    fn new(mode: Mode, streamed: bool) -> Self {
        Self { mode, streamed, resident: false, activate_at: Some(1), restart_at: None, crash_at: None }
    }
}

type Dump = Vec<(&'static str, Vec<u8>, Vec<u8>)>;

struct RunOut {
    /// Every hashed native row after each block.
    dumps: Vec<Dump>,
    results: Vec<Vec<(bool, Option<String>)>>,
    root: B256,
}

const DUMP_CFS: [&str; 5] = [CF_NATIVE_POSITIONS, CF_NATIVE_BALANCES, CF_NATIVE_ORACLE, CF_NATIVE_MARKETS, CF_FEE_CONFIG];

fn dump(db: &StateDb) -> Dump {
    DUMP_CFS
        .iter()
        .flat_map(|cf| db.iterate_cf(cf, None).unwrap().into_iter().map(move |(k, v)| (*cf, k, v)))
        .collect()
}

fn stream_db() -> (tempfile::TempDir, StateDb) {
    let (d, db) = setup(MARKETS, TRADERS, 1_000_000);
    set_param(&db, torus_types::FUNDING_INTERVAL_PARAM, 600);
    set_param(&db, torus_types::FUNDING_IMPACT_NOTIONAL_PARAM, 150);
    (d, db)
}

fn run(blocks: &[Block], o: Opts) -> RunOut {
    let (_d, db) = stream_db();
    let mut holder = ResidentBooks::default();
    let mut out = RunOut { dumps: Vec::new(), results: Vec::new(), root: B256::ZERO };
    for (i, b) in blocks.iter().enumerate() {
        let h = i as u64 + 1;
        if o.restart_at == Some(h) {
            holder = ResidentBooks::default();
        }
        let attempts = if o.crash_at == Some(h) { 2 } else { 1 };
        for attempt in 0..attempts {
            let commit = attempt + 1 == attempts;
            let mut overlay = NativeStateOverlay::new(db.clone());
            let mut rb = begin_resident(o.resident.then_some(&mut holder), &mut overlay, h, None);
            let features = if o.activate_at.is_some_and(|a| h >= a) { FUND } else { 0 };
            let mut ctx = ctx_at(overlay.clone(), h, b.ts, features);
            ctx.attach_resident_block(&mut rb);
            for &(m, px) in &b.marks {
                set_mark(&ctx.state, m, fp(px), b.ts);
            }
            let _ = NativeExecutor::begin_block_oracle(&mut ctx);
            let mut results = Vec::new();
            for batch in &b.batches {
                let r = exec(&mut ctx, batch, o.mode, o.streamed);
                results.extend(r.iter().map(|x| (x.success, x.error.as_deref().map(str::to_string))));
                assert!(ctx.fatal_error.is_none(), "{o:?} block {h}: {:?}", ctx.fatal_error);
            }
            assert!(ctx.reader_fault.get().is_none(), "{o:?} block {h}: {:?}", ctx.reader_fault.get());
            ctx.save_order_books();
            ctx.detach_resident_block(&mut rb);
            drop(ctx);
            if !commit {
                // Crash before the flush: nothing of the block reaches the DB,
                // and the node restarts with no resident rows.
                drop(rb);
                drop(overlay);
                holder = ResidentBooks::default();
                continue;
            }
            overlay.put_cf_raw(CF_CONSENSUS_META, META_NATIVE_APPLIED_HEIGHT, &h.to_be_bytes()).unwrap();
            let delta = if rb.attached() { overlay.own_pending_delta() } else { Default::default() };
            let frozen = overlay.freeze(h);
            end_resident(&mut holder, rb, &mut overlay, delta, true, None);
            frozen.flush_with_native_trie_stats(&db, None, None, None).expect("flush");
            out.results.push(results);
            out.dumps.push(dump(&db));
        }
    }
    out.root = compute_native_state_root(&db).expect("root");
    out
}

/// The stream must actually exercise the engine: every market settled at a
/// non-zero index, some position rows are v3 with funding paid.
fn assert_funding_happened(out: &RunOut) {
    let last = out.dumps.last().unwrap();
    for m in 1..=MARKETS {
        let key = funding::funding_key(m);
        let row = last.iter().find(|(cf, k, _)| *cf == CF_NATIVE_ORACLE && k.as_slice() == key).expect("fnd row");
        let row = FundingRow::decode(&row.2).unwrap();
        assert_ne!(row.cum_index, 0, "market {m} index");
        assert!(row.last_settle_ts > 0, "market {m} settled");
    }
    let paid = last
        .iter()
        .filter(|(cf, _, _)| *cf == CF_NATIVE_POSITIONS)
        .filter_map(|(_, _, v)| borsh::from_slice::<Position>(v).ok())
        .filter(|p| p.funding.is_some_and(|f| { f.paid } != FixedPoint::ZERO))
        .count();
    assert!(paid > 0, "some position settled funding on touch");
}

fn assert_same(a: &RunOut, b: &RunOut, label: &str) {
    assert_eq!(a.dumps.len(), b.dumps.len(), "{label}: blocks");
    for (h, (x, y)) in a.dumps.iter().zip(&b.dumps).enumerate() {
        assert_eq!(a.results[h], b.results[h], "{label}: block {} results", h + 1);
        assert!(x == y, "{label}: block {} rows differ", h + 1);
    }
    assert_eq!(a.root, b.root, "{label}: native root");
}

/// RED before the funding engine: no funding happened.
#[test]
fn funding_stream_is_identical_in_every_settle_mode_and_with_resident_rows() {
    let blocks = stream(0xF0_0D);
    let reference = run(&blocks, Opts::new(Mode::Seq, false));
    assert_funding_happened(&reference);
    for (mode, streamed) in [
        (Mode::Par(1), false),
        (Mode::Par(2), true),
        (Mode::Par(8), false),
        (Mode::Par(8), true),
        (Mode::Engine(1), false),
        (Mode::Engine(4), true),
    ] {
        for resident in [false, true] {
            let o = Opts { resident, ..Opts::new(mode, streamed) };
            assert_same(&reference, &run(&blocks, o), &format!("{o:?}"));
        }
    }
}

/// A restart (resident rows dropped) mid-interval and a crash before a
/// block's flush (the block re-executed) change nothing.
#[test]
fn funding_survives_restart_and_crash_replay_mid_interval() {
    let blocks = stream(0xC0FFEE);
    let reference = run(&blocks, Opts { resident: true, ..Opts::new(Mode::Par(2), true) });
    assert_funding_happened(&reference);
    // ts = 1000 + 250 h, interval 600: h = 7 (2750) and h = 11 (3750) are
    // inside an interval (samples accumulated, not settled).
    for (restart_at, crash_at) in [(Some(7), None), (None, Some(11)), (Some(3), Some(7))] {
        let o = Opts { resident: true, restart_at, crash_at, ..Opts::new(Mode::Par(2), true) };
        assert_same(&reference, &run(&blocks, o), &format!("{o:?}"));
    }
}

/// Before H every block is byte-identical to a run that never activates (no
/// funding row, every position row v2); from H the rows exist. RED before
/// the funding engine: no rows from H.
#[test]
fn activation_at_h_leaves_earlier_blocks_untouched() {
    const H: u64 = 9;
    let blocks = stream(0xAC7);
    let never = run(&blocks, Opts { activate_at: None, ..Opts::new(Mode::Par(2), true) });
    let at_h = run(&blocks, Opts { activate_at: Some(H), ..Opts::new(Mode::Par(2), true) });
    for h in 1..H as usize {
        assert!(never.dumps[h - 1] == at_h.dumps[h - 1], "block {h} differs before activation");
        assert_eq!(never.results[h - 1], at_h.results[h - 1], "block {h} results");
    }
    for d in &never.dumps {
        assert!(d.iter().all(|(cf, k, _)| !(*cf == CF_NATIVE_ORACLE && k.starts_with(b"fnd"))), "no rows without the bit");
        assert!(d.iter().filter(|(cf, _, _)| *cf == CF_NATIVE_POSITIONS).all(|(_, _, v)| v.len() == 111 && v[0] == 2), "v2 rows only");
    }
    let rows_at = |h: usize| at_h.dumps[h - 1].iter().filter(|(cf, k, _)| *cf == CF_NATIVE_ORACLE && k.starts_with(b"fnd")).count();
    assert_eq!(rows_at(H as usize - 1), 0);
    assert_eq!(rows_at(H as usize), MARKETS as usize, "one row per listed market from H");
    assert_ne!(never.root, at_h.root);
}

// ---- Governance keys ----

fn wei(tokens: u64) -> alloy_primitives::U256 {
    alloy_primitives::U256::from(tokens) * alloy_primitives::U256::from(10u64).pow(alloy_primitives::U256::from(18u64))
}

/// Governance with a 10-block timelock and a proposer (addr 2) with stake.
fn governance_db() -> (tempfile::TempDir, StateDb) {
    use revm::state::AccountInfo;
    use torus_economics::governance::GovernanceParams;
    let (d, db) = setup(1, 0, 0);
    let ctx = ctx_at(db.clone(), 1, 1_000, 0);
    ctx.governance
        .set_governance_params(&GovernanceParams {
            voting_period_blocks: 100,
            quorum_bps: 3300,
            min_proposal_stake: wei(100),
            permanent_weight_multiplier_num: 3,
            permanent_weight_multiplier_den: 2,
            treasury_address: addr(100),
            timelock_blocks: 10,
            permanent_unlock_threshold_bps: 8000,
        })
        .unwrap();
    let val = addr(1);
    let min = torus_economics::MIN_SELF_DELEGATION;
    db.put_account(&val, &AccountInfo { balance: min, ..Default::default() }).unwrap();
    ctx.staking.register_validator(val, [1; 32], 500, min).unwrap();
    db.put_account(&addr(2), &AccountInfo { balance: wei(500), ..Default::default() }).unwrap();
    ctx.staking.delegate(addr(2), val, wei(500)).unwrap();
    (d, db)
}

fn submit_key(db: &StateDb, features: u128, key: &str, value: &str) -> NativeActionResult {
    let mut ctx = ctx_at(db.clone(), 3, 1_000, features);
    let action = NativeAction::SubmitProposal(Proposal {
        title: "funding".into(),
        description: "funding".into(),
        action: ProposalAction::ParameterChange { key: key.into(), value: value.into() },
    });
    NativeExecutor::execute(&mut ctx, &addr(2), &action)
}

/// With the bit off a funding key proposal is refused exactly as on main
/// (governance does not know the key); with it on the key is range-checked,
/// and a passed proposal writes the key row the funding step reads. RED
/// before the funding engine: refused with the bit on too.
#[test]
fn funding_keys_need_the_feature() {
    let (_d, db) = governance_db();
    let off = submit_key(&db, 0, torus_types::FUNDING_INTERVAL_PARAM, "1800");
    assert!(!off.success);
    assert!(off.error.as_deref().unwrap_or("").contains("not modifiable"), "{:?}", off.error);
    let bad = submit_key(&db, FUND, torus_types::FUNDING_INTERVAL_PARAM, "5");
    assert!(!bad.success && bad.error.as_deref().unwrap_or("").contains("between"), "{:?}", bad.error);
    let on = submit_key(&db, FUND, torus_types::FUNDING_INTERVAL_PARAM, "1800");
    assert!(on.success, "{:?}", on.error);
    let mut ctx = ctx_at(db.clone(), 120, 2_000, FUND);
    ctx.governance.cast_vote(addr(2), 1, true, 10).unwrap();
    ctx.governance.finalize_proposal(1, 104).unwrap();
    let _ = NativeExecutor::process_governance(&mut ctx);
    assert!(ctx.fatal_error.is_none());
    drop(ctx);
    let stored = db.get_cf_raw(CF_FEE_CONFIG, torus_types::FUNDING_INTERVAL_PARAM.as_bytes()).unwrap();
    assert_eq!(stored.as_deref(), Some(b"1800".as_slice()));
    assert_eq!(FundingParams::read(&db).unwrap().interval_secs, 1_800);
}

// ---- Cost: O(markets) per block ----

/// Counts point reads, scans and writes per CF over a `StateDb`.
#[derive(Clone)]
struct Counting {
    inner: StateDb,
    counts: Arc<Mutex<BTreeMap<(String, &'static str), usize>>>,
}

impl Counting {
    fn bump(&self, cf: &str, what: &'static str) {
        *self.counts.lock().unwrap().entry((cf.to_string(), what)).or_default() += 1;
    }
}

impl StateBackend for Counting {
    fn get_cf_raw(&self, cf: &str, key: &[u8]) -> Result<Option<Vec<u8>>, StateError> {
        self.bump(cf, "get");
        self.inner.get_cf_raw(cf, key)
    }
    fn put_cf_raw(&self, cf: &str, key: &[u8], value: &[u8]) -> Result<(), StateError> {
        self.bump(cf, "put");
        self.inner.put_cf_raw(cf, key, value)
    }
    fn delete_cf_raw(&self, cf: &str, key: &[u8]) -> Result<(), StateError> {
        self.bump(cf, "delete");
        self.inner.delete_cf_raw(cf, key)
    }
    fn iterate_cf(&self, cf: &str, prefix: Option<&[u8]>) -> Result<Vec<(Vec<u8>, Vec<u8>)>, StateError> {
        self.bump(cf, "scan");
        self.inner.iterate_cf(cf, prefix)
    }
    fn atomic_write(&self, ops: &[AtomicWriteOp<'_>]) -> Result<(), StateError> {
        for op in ops {
            match op {
                AtomicWriteOp::Put { cf, .. } => self.bump(cf, "put"),
                AtomicWriteOp::Delete { cf, .. } => self.bump(cf, "delete"),
            }
        }
        self.inner.atomic_write(ops)
    }
}

/// The funding step of a settling + sampling block over `n` open positions
/// in market 1 (of 4): its backend accesses per CF.
fn step_accesses(n: u32) -> BTreeMap<(String, &'static str), usize> {
    let (_d, db) = setup(4, 0, 0);
    let pm = PositionManager::new(db.clone());
    for i in 0..n {
        let mut t = [0u8; 20];
        t[..4].copy_from_slice(&i.to_be_bytes());
        pm.apply_fill(&Address::new(t), 1, i % 2 == 0, fp(1), fp(100), MarginType::Cross).unwrap();
    }
    let params = FundingParams::default();
    for m in 1..=4 {
        // Mid-interval with samples: this block settles AND samples.
        let mut row = FundingRow::new(1_000, &params);
        row.samples = 3;
        row.premium_sum = 3 * E18 / 1_000;
        row.last_oracle = fp(100);
        row.last_sample_ts = 3_599;
        funding::write_row(&db, m, &row).unwrap();
        set_mark(&db, m, fp(100), 3_600);
    }
    let counting = Counting { inner: db.clone(), counts: Arc::default() };
    let mut ctx = ctx_at(counting.clone(), 2, 3_600, FUND);
    counting.counts.lock().unwrap().clear();
    NativeExecutor::begin_block_funding(&mut ctx, &[1, 2, 3, 4]);
    assert!(ctx.fatal_error.is_none(), "{:?}", ctx.fatal_error);
    assert!(row_of(&db, 1).unwrap().last_settle_ts == 3_600, "the step settled");
    let c = counting.counts.lock().unwrap().clone();
    c
}

/// RED before the funding engine: the step did nothing (no settlement).
#[test]
fn the_block_step_is_o_markets_whatever_the_position_count() {
    let small = step_accesses(10);
    let large = step_accesses(5_000);
    assert_eq!(small, large, "same accesses with 10 and 5000 positions");
    for ((cf, what), n) in &small {
        assert!(cf != CF_NATIVE_POSITIONS && cf != CF_NATIVE_BALANCES, "{cf} {what} x{n}: the step never reads accounts");
    }
    let funding_rows = small.get(&(CF_NATIVE_ORACLE.to_string(), "put")).copied().unwrap_or(0);
    assert_eq!(funding_rows, 4, "one row write per market");
    let total: usize = small.values().sum();
    assert!(total <= 4 * 4 + torus_types::FUNDING_PARAMS.len() + 4, "O(markets): {small:?}");
}
