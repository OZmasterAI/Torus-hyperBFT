//! Trading fees (docs/plans/trading-fees.md, P0.0 feature `trading_fees`).
//!
//! With the header bit on, every book fill charges the taker
//! `notional * taker_ppm / 1e6` (rounded up) and the maker
//! `notional * maker_ppm / 1e6` (a negative maker rate is a rebate, rounded
//! toward zero) from their native `available`, credits the exact sum to the
//! treasury's native balance and adds each side's fee to its `cfee` row.
//! With the bit off nothing changes (byte-identical state). The serial
//! settle, the C3 parallel settle, the L3 engine and the L2.1 streamed
//! settle must give the same state and state root.

use alloy_primitives::{Address, B256};

use torus_bridge::native_executor::{
    settle_streamed_calls_on_this_thread, with_settle_streamed, NativeExecContext, NativeExecutor,
};
use torus_bridge::state_root::compute_native_state_root;
use torus_core::position::NativeBalance;
use torus_state::cf::{
    CF_FEE_CONFIG, CF_NATIVE_BALANCES, CF_NATIVE_MARKETS, CF_NATIVE_ORDER_BOOKS,
    CF_NATIVE_POSITIONS, CF_NATIVE_TRADES, CF_NATIVE_USER_TRADES,
};
use torus_state::{StateBackend, StateDb};
use torus_types::trading_fees::{cum_fee_key, MAKER_FEE_PARAM, TAKER_FEE_PARAM};
use torus_types::{
    ActivationSchedule, Feature, FixedPoint, MarketId, NativeAction, OrderType, PlaceOrderParams,
    TimeInForce,
};

const TREASURY: u8 = 100;
const S: i128 = FixedPoint::SCALE;

fn open_test_db() -> (tempfile::TempDir, StateDb) {
    let dir = tempfile::tempdir().expect("create temp dir");
    let db = StateDb::open(dir.path()).expect("open db");
    // Placeholder listed markets (tick / lot 1, no margin config), as the
    // other differential tests list them.
    for m in 1..=16u64 {
        db.put_cf_raw(CF_NATIVE_MARKETS, &m.to_be_bytes(), b"listed")
            .expect("list market");
    }
    (dir, db)
}

fn addr(n: u8) -> Address {
    Address::new([n; 20])
}

fn fp(v: i64) -> FixedPoint {
    FixedPoint::from_raw(v as i128 * S)
}

fn make_ctx(db: StateDb, fees_on: bool) -> NativeExecContext {
    let mut ctx = NativeExecContext::new(
        db,
        1,
        1000,
        0,
        100,
        10,
        addr(99),
        addr(TREASURY),
        addr(101),
    );
    ctx.features = if fees_on { Feature::TradingFees.bit() } else { 0 };
    for n in 1..=32u8 {
        let bal = NativeBalance {
            available: fp(1_000_000),
            order_margin: FixedPoint::ZERO,
        };
        ctx.positions.put_native_balance(&addr(n), &bal).unwrap();
    }
    ctx
}

fn set_rates(db: &StateDb, taker: &str, maker: &str) {
    db.put_cf_raw(CF_FEE_CONFIG, TAKER_FEE_PARAM.as_bytes(), taker.as_bytes())
        .unwrap();
    db.put_cf_raw(CF_FEE_CONFIG, MAKER_FEE_PARAM.as_bytes(), maker.as_bytes())
        .unwrap();
}

fn order(market_id: MarketId, is_buy: bool, price: i64, qty: i64, tif: TimeInForce) -> PlaceOrderParams {
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

fn place(sender: u8, market: MarketId, is_buy: bool, price: i64, qty: i64) -> (Address, NativeAction) {
    (
        addr(sender),
        NativeAction::PlaceOrder(order(market, is_buy, price, qty, TimeInForce::GTC)),
    )
}

fn available(ctx: &NativeExecContext, n: u8) -> i128 {
    ctx.positions.get_native_balance(&addr(n)).unwrap().available.raw()
}

fn treasury_row(db: &StateDb) -> Option<Vec<u8>> {
    db.get_cf_raw(CF_NATIVE_BALANCES, addr(TREASURY).as_slice()).unwrap()
}

fn cum_fee(db: &StateDb, n: u8) -> Option<i128> {
    db.get_cf_raw(CF_NATIVE_BALANCES, &cum_fee_key(&addr(n)))
        .unwrap()
        .map(|v| i128::from_be_bytes(v.as_slice().try_into().expect("16-byte cfee row")))
}

fn run_serial(ctx: &mut NativeExecContext, batch: &[(Address, NativeAction)]) {
    let r = NativeExecutor::execute_batch_engine_mode(ctx, batch, 0);
    assert!(ctx.fatal_error.is_none(), "{:?}", ctx.fatal_error);
    for a in &r.results {
        assert!(a.success, "{:?}", a.error);
    }
}

/// One fill of 10 @ 100 (notional 1000): the maker (1) rests a sell, the
/// taker (2) buys. Returns the context after the batch.
fn one_fill(fees_on: bool, rates: Option<(&str, &str)>) -> (tempfile::TempDir, StateDb, NativeExecContext) {
    let (dir, db) = open_test_db();
    if let Some((t, m)) = rates {
        set_rates(&db, t, m);
    }
    let mut ctx = make_ctx(db.clone(), fees_on);
    run_serial(&mut ctx, &[place(1, 1, false, 100, 10), place(2, 1, true, 100, 10)]);
    (dir, db, ctx)
}

#[test]
fn a_fill_charges_default_rates_and_credits_the_treasury() {
    let (_d0, db_off, off) = one_fill(false, None);
    let (_d1, db_on, on) = one_fill(true, None);
    // Defaults: taker 450 ppm, maker 150 ppm of 1000.
    let (taker_fee, maker_fee) = (45 * S / 100, 15 * S / 100);
    assert_eq!(available(&off, 2) - available(&on, 2), taker_fee, "taker pays 0.45");
    assert_eq!(available(&off, 1) - available(&on, 1), maker_fee, "maker pays 0.15");
    assert_eq!(available(&on, TREASURY), taker_fee + maker_fee, "treasury gets the sum");
    assert_eq!(cum_fee(&db_on, 2), Some(taker_fee));
    assert_eq!(cum_fee(&db_on, 1), Some(maker_fee));
    // Bit off: no treasury row, no fee rows.
    assert_eq!(treasury_row(&db_off), None);
    assert_eq!(cum_fee(&db_off, 1), None);
    assert_eq!(cum_fee(&db_off, 2), None);
}

#[test]
fn governance_rates_apply_and_a_rebate_is_clamped_to_the_taker_fee() {
    // Taker 100 ppm, maker -300 ppm (clamped to -100): the maker earns what
    // the taker pays, the treasury nets 0 (no row written).
    let (_d0, _db_off, off) = one_fill(false, None);
    let (_d1, db_on, on) = one_fill(true, Some(("100", "-300")));
    assert_eq!(available(&off, 2) - available(&on, 2), 10 * S / 100);
    assert_eq!(available(&on, 1) - available(&off, 1), 10 * S / 100, "rebate");
    assert_eq!(cum_fee(&db_on, 1), Some(-10 * S / 100));
    assert_eq!(treasury_row(&db_on), None, "net 0: the treasury is not touched");
}

#[test]
fn zero_rates_with_the_bit_on_change_nothing() {
    let (_d0, db_off, _off) = one_fill(false, None);
    let (_d1, db_on, _on) = one_fill(true, Some(("0", "0")));
    assert_eq!(dump(&db_off), dump(&db_on));
}

#[test]
fn bit_off_ignores_rate_rows_byte_identical() {
    let (_d0, db_a, _a) = one_fill(false, None);
    let (_d1, db_b, _b) = one_fill(false, Some(("10000", "10000")));
    assert_eq!(dump(&db_a), dump(&db_b));
    assert_eq!(
        compute_native_state_root(&db_a).unwrap(),
        compute_native_state_root(&db_b).unwrap()
    );
}

/// A closing fill credits its PnL and debits its fee in the same update.
#[test]
fn a_closing_fill_nets_pnl_and_fee() {
    let run = |fees_on: bool| {
        let (dir, db) = open_test_db();
        let mut ctx = make_ctx(db.clone(), fees_on);
        // 2 opens long 10 @ 100 against 1, then closes selling 10 @ 110 to 3.
        run_serial(&mut ctx, &[place(1, 1, false, 100, 10), place(2, 1, true, 100, 10)]);
        run_serial(&mut ctx, &[place(3, 1, true, 110, 10), place(2, 1, false, 110, 10)]);
        (dir, db, ctx)
    };
    let (_d0, _db_off, off) = run(false);
    let (_d1, db_on, on) = run(true);
    // Trader 2: taker twice (notional 1000 then 1100) at 450 ppm.
    let fees_2 = 45 * S / 100 + 495 * S / 1000;
    assert_eq!(available(&off, 2) - available(&on, 2), fees_2);
    assert_eq!(cum_fee(&db_on, 2), Some(fees_2));
    // Maker 3: 1100 at 150 ppm.
    assert_eq!(available(&off, 3) - available(&on, 3), 165 * S / 1000);
    let all = fees_2 + 15 * S / 100 + 165 * S / 1000;
    assert_eq!(available(&on, TREASURY), all);
}

/// P0.0: the rule follows the executed block's header bits. Blocks below
/// the activation height H charge nothing; from H every fill pays.
#[test]
fn fees_start_exactly_at_the_activation_height() {
    const H: u64 = 3;
    let schedule = ActivationSchedule::merge(
        &[(Feature::TradingFees, H)].into_iter().collect(),
        &[],
    )
    .unwrap();
    let (_dir, db) = open_test_db();
    let mut ctx = make_ctx(db.clone(), false);
    let mut treasury_by_height = Vec::new();
    for h in 1..=4u64 {
        ctx.block_height = h;
        ctx.features = schedule.active_at(h);
        run_serial(&mut ctx, &[place(1, 1, false, 100, 10), place(2, 1, true, 100, 10)]);
        treasury_by_height.push(available(&ctx, TREASURY));
    }
    let per_block = 60 * S / 100;
    assert_eq!(treasury_by_height, vec![0, 0, per_block, 2 * per_block]);
    assert_eq!(cum_fee(&db, 2), Some(2 * 45 * S / 100));
}

/// The single-action path (`execute`: the path of triggered stops and
/// stage-1 liquidation orders) charges the same fees as the batch path.
#[test]
fn single_action_path_charges_the_same_fees() {
    let (_d0, _db_b, batch) = one_fill(true, None);
    let (_dir, db) = open_test_db();
    let mut single = make_ctx(db.clone(), true);
    for (who, a) in [place(1, 1, false, 100, 10), place(2, 1, true, 100, 10)] {
        let r = NativeExecutor::execute(&mut single, &who, &a);
        assert!(r.success, "{:?}", r.error);
    }
    for n in [1, 2, TREASURY] {
        assert_eq!(available(&single, n), available(&batch, n), "trader {n}");
    }
    assert_eq!(cum_fee(&db, 1), Some(15 * S / 100));
    assert_eq!(cum_fee(&db, 2), Some(45 * S / 100));
}

/// Stream extras carry each side's fee (s80 `userFills`).
#[test]
fn fill_extras_carry_the_fees() {
    let (_dir, db) = open_test_db();
    let mut ctx = make_ctx(db, true);
    ctx.record_fills = true;
    run_serial(&mut ctx, &[place(1, 1, false, 100, 10), place(2, 1, true, 100, 10)]);
    let (fills, extras) = ctx.take_pending_fills_and_extras();
    assert_eq!((fills.len(), extras.len()), (1, 1));
    assert_eq!((extras[0].taker_fee_raw, extras[0].maker_fee_raw), (45 * S / 100, 15 * S / 100));
}

// ---- Determinism across settle paths ----

type Dump = Vec<(String, Vec<u8>, Vec<u8>)>;

fn dump(db: &StateDb) -> Dump {
    let mut out = Vec::new();
    for cf in [
        CF_NATIVE_BALANCES,
        CF_NATIVE_POSITIONS,
        CF_NATIVE_ORDER_BOOKS,
        CF_NATIVE_TRADES,
        CF_NATIVE_USER_TRADES,
    ] {
        for (k, v) in db.iterate_cf(cf, None).unwrap() {
            out.push((cf.to_string(), k, v));
        }
    }
    out
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

/// Several blocks of crossing orders over 6 markets and 24 traders: opens,
/// closes and flips, so PnL and fee events interleave on the same balances.
fn fuzz_blocks(seed: u64) -> Vec<Vec<(Address, NativeAction)>> {
    let mut rng = Lcg(seed);
    (0..6)
        .map(|_| {
            (0..120)
                .map(|_| {
                    let who = 1 + rng.below(24) as u8;
                    let market = 1 + rng.below(6);
                    let is_buy = rng.below(2) == 0;
                    let price = 95 + rng.below(11) as i64;
                    let qty = 1 + rng.below(9) as i64;
                    place(who, market, is_buy, price, qty)
                })
                .collect()
        })
        .collect()
}

#[derive(Clone, Copy, Debug)]
enum Path {
    Serial,
    ParallelSettle,
    Engine,
    EngineStreamed,
}

fn run_path(blocks: &[Vec<(Address, NativeAction)>], path: Path, fees_on: bool) -> (Dump, B256, i128) {
    let (_dir, db) = open_test_db();
    set_rates(&db, "450", "-100");
    let mut ctx = make_ctx(db.clone(), fees_on);
    let streamed_before = settle_streamed_calls_on_this_thread();
    for b in blocks {
        match path {
            Path::Serial => {
                NativeExecutor::execute_batch_engine_mode(&mut ctx, b, 0);
            }
            Path::ParallelSettle => {
                with_settle_streamed(false, || {
                    NativeExecutor::execute_batch_settle_workers(&mut ctx, b, true, Some(3))
                });
            }
            Path::Engine => {
                with_settle_streamed(false, || {
                    NativeExecutor::execute_batch_engine_mode(&mut ctx, b, 4)
                });
            }
            Path::EngineStreamed => {
                with_settle_streamed(true, || {
                    NativeExecutor::execute_batch_engine_mode(&mut ctx, b, 4)
                });
            }
        }
        assert!(ctx.fatal_error.is_none(), "{path:?}: {:?}", ctx.fatal_error);
    }
    if matches!(path, Path::EngineStreamed) {
        assert!(
            settle_streamed_calls_on_this_thread() > streamed_before,
            "the streamed arm must have streamed"
        );
    }
    ctx.save_order_books();
    let treasury = available(&ctx, TREASURY);
    (dump(&db), compute_native_state_root(&db).unwrap(), treasury)
}

#[test]
fn every_settle_path_gives_the_same_state_with_fees() {
    for seed in [1u64, 7, 42] {
        let blocks = fuzz_blocks(seed);
        let serial = run_path(&blocks, Path::Serial, true);
        assert!(serial.2 > 0, "seed {seed}: fees were charged");
        let off = run_path(&blocks, Path::Serial, false);
        assert_ne!(off.1, serial.1, "seed {seed}: the bit changes the state root");
        for path in [Path::ParallelSettle, Path::Engine, Path::EngineStreamed] {
            let other = run_path(&blocks, path, true);
            assert_eq!(other.1, serial.1, "seed {seed} {path:?}: state root");
            assert!(other.0 == serial.0, "seed {seed} {path:?}: state dump differs");
        }
    }
}

/// Conservation: whatever the traders lost to fees the treasury holds, to
/// the raw unit (no dust), and the `cfee` rows sum to it.
#[test]
fn fees_are_conserved_to_the_raw_unit() {
    let blocks = fuzz_blocks(9);
    let (_d0, db_off) = open_test_db();
    let (_d1, db_on) = open_test_db();
    set_rates(&db_on, "450", "-100");
    let mut off = make_ctx(db_off.clone(), false);
    let mut on = make_ctx(db_on.clone(), true);
    for b in &blocks {
        NativeExecutor::execute_batch_engine_mode(&mut off, b, 0);
        NativeExecutor::execute_batch_engine_mode(&mut on, b, 0);
    }
    let mut paid = 0i128;
    let mut rows = 0i128;
    for n in 1..=32u8 {
        let bal = |c: &NativeExecContext| {
            let b = c.positions.get_native_balance(&addr(n)).unwrap();
            b.available.raw() + b.order_margin.raw()
        };
        paid += bal(&off) - bal(&on);
        rows += cum_fee(&db_on, n).unwrap_or(0);
    }
    let treasury = available(&on, TREASURY);
    assert!(treasury > 0);
    assert_eq!(paid, treasury, "debits == treasury credit");
    assert_eq!(rows, treasury, "cfee rows sum to the treasury credit");
}

// ---- Codex s138 #2: an unpaid fee on a flat account ----

const VAULT: Address = torus_core::liquidation::LIQUIDATOR_VAULT;

fn collateral(ctx: &NativeExecContext, a: &Address) -> i128 {
    let b = ctx.positions.get_native_balance(a).unwrap();
    b.available.raw() + b.order_margin.raw()
}

/// Every native balance the scenario touches (traders, treasury, vault).
fn total_collateral(ctx: &NativeExecContext) -> i128 {
    let mut all: Vec<Address> = (1..=32u8).map(addr).collect();
    all.extend([addr(TREASURY), VAULT]);
    all.iter().map(|a| collateral(ctx, a)).sum()
}

/// How the closing block runs.
#[derive(Clone, Copy, Debug)]
enum CloseRun {
    Batch(Path),
    Single,
}

fn exec_on(ctx: &mut NativeExecContext, batch: &[(Address, NativeAction)], run: CloseRun) {
    match run {
        CloseRun::Batch(Path::Serial) => {
            NativeExecutor::execute_batch_engine_mode(ctx, batch, 0);
        }
        CloseRun::Batch(Path::ParallelSettle) => {
            with_settle_streamed(false, || {
                NativeExecutor::execute_batch_settle_workers(ctx, batch, true, Some(3))
            });
        }
        CloseRun::Batch(Path::Engine) => {
            with_settle_streamed(false, || NativeExecutor::execute_batch_engine_mode(ctx, batch, 4));
        }
        CloseRun::Batch(Path::EngineStreamed) => {
            with_settle_streamed(true, || NativeExecutor::execute_batch_engine_mode(ctx, batch, 4));
        }
        CloseRun::Single => {
            for (who, a) in batch {
                NativeExecutor::execute(ctx, who, a);
            }
        }
    }
    assert!(ctx.fatal_error.is_none(), "{run:?}: {:?}", ctx.fatal_error);
}

/// Trader 2 opens long 10 @ 100 against 1, is left with 0.10 cash, then
/// closes all 10 @ `close_px` as the taker against 3 (maker), with the
/// default rates. A filler pair (4 / 5) trades on market 2 in the same
/// block so the parallel paths have two markets. Returns the context after
/// the closing block and the total collateral before it.
fn flat_close(close_px: i64, run: CloseRun) -> (tempfile::TempDir, StateDb, NativeExecContext, i128) {
    flat_close_with(None, 10 * S / 100, close_px, run)
}

/// [`flat_close`] with governance `rates` and trader 2's `cash` (raw) before
/// the close.
fn flat_close_with(
    rates: Option<(&str, &str)>,
    cash: i128,
    close_px: i64,
    run: CloseRun,
) -> (tempfile::TempDir, StateDb, NativeExecContext, i128) {
    let (dir, db) = open_test_db();
    if let Some((t, m)) = rates {
        set_rates(&db, t, m);
    }
    let mut ctx = make_ctx(db.clone(), true);
    run_serial(&mut ctx, &[place(1, 1, false, 100, 10), place(2, 1, true, 100, 10)]);
    let mut bal = ctx.positions.get_native_balance(&addr(2)).unwrap();
    bal.available = FixedPoint::from_raw(cash);
    bal.order_margin = FixedPoint::ZERO;
    ctx.positions.put_native_balance(&addr(2), &bal).unwrap();
    let before = total_collateral(&ctx);
    exec_on(
        &mut ctx,
        &[
            place(3, 1, true, close_px, 10),
            place(4, 2, false, 50, 4),
            place(2, 1, false, close_px, 10),
            place(5, 2, true, 50, 4),
        ],
        run,
    );
    assert!(
        ctx.positions.positions_for_trader(&addr(2)).unwrap().is_empty(),
        "{run:?}: trader 2 is flat"
    );
    (dir, db, ctx, before)
}

fn all_runs() -> [CloseRun; 5] {
    [
        CloseRun::Batch(Path::Serial),
        CloseRun::Batch(Path::ParallelSettle),
        CloseRun::Batch(Path::Engine),
        CloseRun::Batch(Path::EngineStreamed),
        CloseRun::Single,
    ]
}

/// The close realizes no PnL; the 0.45 taker fee exceeds the 0.10 cash.
/// The flat trader pays what it holds (0.10) and ends at exactly zero; the
/// unpaid 0.35 is never credited (treasury and `cfee` take only what was
/// paid), and no collateral appears or disappears. RED before: available
/// -0.35 on an account no liquidation scan visits, treasury +0.45.
#[test]
fn a_flat_trader_pays_at_most_its_cash_and_the_treasury_only_what_was_paid() {
    let mut first: Option<(Dump, B256)> = None;
    for run in all_runs() {
        let (_d, db, ctx, before) = flat_close(100, run);
        assert_eq!(collateral(&ctx, &addr(2)), 0, "{run:?}: flat at zero, not negative");
        // Block 1: 0.45 + 0.15; block 2: 2 paid 0.10, maker 3 0.15, market 2
        // (notional 200): 4 maker 0.03, 5 taker 0.09.
        let want = (45 + 15 + 10 + 15 + 3 + 9) * S / 100;
        assert_eq!(available(&ctx, TREASURY), want, "{run:?}: treasury");
        assert_eq!(cum_fee(&db, 2), Some(55 * S / 100), "{run:?}: cfee = paid");
        assert_eq!(collateral(&ctx, &VAULT), 0, "{run:?}: nothing for the vault");
        assert_eq!(total_collateral(&ctx), before, "{run:?}: conserved");
        let state = (dump(&db), compute_native_state_root(&db).unwrap());
        if let CloseRun::Batch(_) = run {
            match &first {
                None => first = Some(state),
                Some(f) => {
                    assert_eq!(state.1, f.1, "{run:?}: state root");
                    assert!(state.0 == f.0, "{run:?}: state dump differs");
                }
            }
        }
    }
}

/// The batch paths agree byte for byte on `db` (the first run is kept).
fn same_state_across_batch_paths(first: &mut Option<(Dump, B256)>, run: CloseRun, db: &StateDb) {
    let state = (dump(db), compute_native_state_root(db).unwrap());
    if let CloseRun::Batch(_) = run {
        match first {
            None => *first = Some(state),
            Some(f) => {
                assert_eq!(state.1, f.1, "{run:?}: state root");
                assert!(state.0 == f.0, "{run:?}: state dump differs");
            }
        }
    }
}

/// Owner s138 ("fees only", [claude-review] P2): the close realizes a 100
/// loss on 0.10 cash. Only the fee (0.405) is waived (not credited, not in
/// `cfee`); the loss-only debt (-99.90) stays on the flat account as on
/// main (repaid by its next deposit), nothing moves to the vault. RED
/// before: the debt moved to the vault (D9) and the account showed 0.
#[test]
fn a_flat_loss_debt_stays_on_the_account_and_only_the_fee_is_waived() {
    let mut first: Option<(Dump, B256)> = None;
    for run in all_runs() {
        let (_d, db, ctx, before) = flat_close(90, run);
        assert_eq!(collateral(&ctx, &addr(2)), -9_990 * S / 100, "{run:?}: the loss debt stays");
        assert_eq!(collateral(&ctx, &VAULT), 0, "{run:?}: nothing for the vault");
        // Block 1: 0.60; block 2: maker 3 pays 0.135 (900 @ 150 ppm), market 2 0.12.
        let want = 60 * S / 100 + 135 * S / 1000 + 12 * S / 100;
        assert_eq!(available(&ctx, TREASURY), want, "{run:?}: treasury");
        assert_eq!(cum_fee(&db, 2), Some(45 * S / 100), "{run:?}: nothing paid on the close");
        // The realized 100 left the cash pool (trader 1's short holds it as
        // unrealized gain); the waived fee was never debited.
        assert_eq!(total_collateral(&ctx), before - 100 * S, "{run:?}: conserved");
        same_state_across_batch_paths(&mut first, run, &db);
    }
}

/// Owner example: cash 50, the close realizes a 70 loss, fee 0 (zero rates,
/// bit on): the balance stays at -20 (main's behaviour), the vault and the
/// treasury are untouched. RED before: -20 moved to the vault.
#[test]
fn a_flat_loss_without_fee_keeps_mains_debt() {
    let mut first: Option<(Dump, B256)> = None;
    for run in all_runs() {
        let (_d, db, ctx, before) = flat_close_with(Some(("0", "0")), 50 * S, 93, run);
        assert_eq!(collateral(&ctx, &addr(2)), -20 * S, "{run:?}: debt stays");
        assert_eq!(collateral(&ctx, &VAULT), 0, "{run:?}: vault untouched");
        assert_eq!(treasury_row(&db), None, "{run:?}: no fee, no treasury row");
        assert_eq!(total_collateral(&ctx), before - 70 * S, "{run:?}: conserved");
        same_state_across_batch_paths(&mut first, run, &db);
    }
}

/// Owner example: cash 50, loss 50 (close @ 95, notional 950), fee 0.4275:
/// the cash pays the loss first, the whole fee is waived (treasury not
/// credited), the account ends at 0. Mixed case (cash 50.10): the loss
/// first, then the 0.10 left pays part of the fee, the shortfall (0.3275)
/// is waived, the account ends at 0.
#[test]
fn a_flat_loss_is_paid_first_and_the_fee_is_waived_up_to_the_shortfall() {
    for (cash, paid) in [(50 * S, 0), (50 * S + 10 * S / 100, 10 * S / 100)] {
        let mut first: Option<(Dump, B256)> = None;
        for run in all_runs() {
            let (_d, db, ctx, before) = flat_close_with(None, cash, 95, run);
            assert_eq!(collateral(&ctx, &addr(2)), 0, "{run:?} cash {cash}: ends at 0");
            assert_eq!(collateral(&ctx, &VAULT), 0, "{run:?}: vault untouched");
            // Block 1: 0.60; block 2: 2 paid `paid`, maker 3 0.1425
            // (950 @ 150 ppm), market 2 0.12.
            let want = 60 * S / 100 + paid + 1_425 * S / 10_000 + 12 * S / 100;
            assert_eq!(available(&ctx, TREASURY), want, "{run:?} cash {cash}: treasury");
            assert_eq!(cum_fee(&db, 2), Some(45 * S / 100 + paid), "{run:?}: cfee = paid");
            assert_eq!(total_collateral(&ctx), before - 50 * S, "{run:?}: conserved");
            same_state_across_batch_paths(&mut first, run, &db);
        }
    }
}

/// [claude-review] P3-1: with a maker rebate (taker 450, maker -300 ppm) a
/// flat taker's waived fee must never make the treasury pay out. The
/// rebate part of a taker fee (0.30 of 0.45) is never waived (the maker
/// was paid it); only the treasury's share (0.15) is. Trader 2 (0.10 cash,
/// no PnL) is left owing 0.20; every fill nets the treasury >= 0. RED
/// before: 0.35 waived, the treasury netted -0.20 on the close and ended
/// at -0.02.
#[test]
fn a_waived_taker_fee_never_makes_the_treasury_pay_a_rebate() {
    let mut first: Option<(Dump, B256)> = None;
    for run in all_runs() {
        let (_d, db, ctx, before) = flat_close_with(Some(("450", "-300")), 10 * S / 100, 100, run);
        assert_eq!(collateral(&ctx, &addr(2)), -20 * S / 100, "{run:?}: owes the rebate part");
        assert_eq!(cum_fee(&db, 2), Some(45 * S / 100 + 30 * S / 100), "{run:?}: cfee = paid");
        assert_eq!(cum_fee(&db, 3), Some(-30 * S / 100), "{run:?}: maker keeps its rebate");
        // Block 1: 0.45 - 0.30; block 2: close nets 0 (0.30 paid - 0.30
        // rebate), market 2 (200): 0.09 - 0.06.
        let want = 15 * S / 100 + 3 * S / 100;
        assert_eq!(available(&ctx, TREASURY), want, "{run:?}: treasury never pays out");
        assert_eq!(collateral(&ctx, &VAULT), 0, "{run:?}");
        assert_eq!(total_collateral(&ctx), before, "{run:?}: conserved");
        same_state_across_batch_paths(&mut first, run, &db);
    }
}

/// A full rebate (maker -450 = -taker): nothing of the taker fee is the
/// treasury's, so nothing is waived; the treasury is never touched.
#[test]
fn a_full_rebate_waives_nothing() {
    for run in all_runs() {
        let (_d, db, ctx, before) = flat_close_with(Some(("450", "-450")), 10 * S / 100, 100, run);
        assert_eq!(collateral(&ctx, &addr(2)), -35 * S / 100, "{run:?}");
        assert_eq!(treasury_row(&db), None, "{run:?}: net 0 on every fill");
        assert_eq!(total_collateral(&ctx), before, "{run:?}: conserved");
    }
}

// ---- [claude-review] P3-2: a fill failing after earlier fills applied ----

/// The armed `(cf, key)` row of [`FailOneWrite`].
type ArmedRow = std::sync::Arc<std::sync::Mutex<Option<(&'static str, Vec<u8>)>>>;

/// A backend whose writes of one armed `(cf, key)` row fail (a local
/// fault); everything else delegates.
#[derive(Clone)]
struct FailOneWrite {
    inner: StateDb,
    armed: ArmedRow,
}

impl FailOneWrite {
    fn hit(&self, cf: &str, key: &[u8]) -> bool {
        self.armed
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|(c, k)| *c == cf && k.as_slice() == key)
    }
    fn fail() -> torus_state::StateError {
        torus_state::StateError::Io(std::io::Error::other("injected write failure"))
    }
}

impl StateBackend for FailOneWrite {
    fn get_cf_raw(&self, cf: &str, key: &[u8]) -> Result<Option<Vec<u8>>, torus_state::StateError> {
        self.inner.get_cf_raw(cf, key)
    }
    fn put_cf_raw(&self, cf: &str, key: &[u8], value: &[u8]) -> Result<(), torus_state::StateError> {
        if self.hit(cf, key) {
            return Err(Self::fail());
        }
        self.inner.put_cf_raw(cf, key, value)
    }
    fn delete_cf_raw(&self, cf: &str, key: &[u8]) -> Result<(), torus_state::StateError> {
        self.inner.delete_cf_raw(cf, key)
    }
    fn iterate_cf(
        &self,
        cf: &str,
        prefix: Option<&[u8]>,
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>, torus_state::StateError> {
        self.inner.iterate_cf(cf, prefix)
    }
    fn atomic_write(&self, ops: &[torus_state::AtomicWriteOp<'_>]) -> Result<(), torus_state::StateError> {
        for op in ops {
            if let torus_state::AtomicWriteOp::Put { cf, key, .. } = op {
                if self.hit(cf, key) {
                    return Err(Self::fail());
                }
            }
        }
        self.inner.atomic_write(ops)
    }
}

/// Single path: makers 1 (10 @ 100) and 3 (10 @ 101) rest; taker 2 buys 20
/// @ 101. Maker 3's side of fill 1 fails (its `cum_volume` write) after
/// fill 0 and the taker's side of fill 1 applied. Every applied side pays
/// its fee, as the per-side charging did: taker 2 both fills, maker 1 fill
/// 0, maker 3 nothing; the treasury gets the sum. RED before: the early
/// return skipped `charge_order_fees`, so no applied fill paid.
#[test]
fn single_path_charges_every_applied_fill_side_when_a_later_fill_fails() {
    let (_dir, db) = open_test_db();
    let backend = FailOneWrite { inner: db.clone(), armed: Default::default() };
    let mut ctx = NativeExecContext::new(
        backend.clone(),
        1,
        1000,
        0,
        100,
        10,
        addr(99),
        addr(TREASURY),
        addr(101),
    );
    ctx.features = Feature::TradingFees.bit();
    for n in 1..=3u8 {
        let bal = NativeBalance { available: fp(1_000_000), order_margin: FixedPoint::ZERO };
        ctx.positions.put_native_balance(&addr(n), &bal).unwrap();
    }
    for (who, a) in [place(1, 1, false, 100, 10), place(3, 1, false, 101, 10)] {
        let r = NativeExecutor::execute(&mut ctx, &who, &a);
        assert!(r.success, "{:?}", r.error);
    }
    *backend.armed.lock().unwrap() =
        Some((CF_NATIVE_BALANCES, torus_core::position::cum_volume_key(&addr(3)).to_vec()));
    let (who, a) = place(2, 1, true, 101, 20);
    let r = NativeExecutor::execute(&mut ctx, &who, &a);
    assert!(
        r.error.as_deref().is_some_and(|e| e.starts_with("maker fill failed")),
        "{:?}",
        r.error
    );
    assert!(ctx.take_fatal_error().is_some(), "a local fault fail-stops");
    // Fill 0: 1000 (taker 0.45, maker 0.15); fill 1: 1010 (taker 0.4545).
    let (t0, m0, t1) = (45 * S / 100, 15 * S / 100, 4_545 * S / 10_000);
    assert_eq!(cum_fee(&db, 2), Some(t0 + t1), "taker: both applied sides");
    assert_eq!(cum_fee(&db, 1), Some(m0), "maker of fill 0");
    assert_eq!(cum_fee(&db, 3), None, "maker 3's side did not apply");
    let bal = |n: u8| ctx.positions.get_native_balance(&addr(n)).unwrap().available.raw();
    assert_eq!(bal(TREASURY), t0 + m0 + t1, "treasury: the applied sides' sum");
    assert_eq!(bal(1), fp(1_000_000).raw() - m0);
}

// ---- Codex s138 #1: the fee keys follow the feature bit ----

fn wei(tokens: u64) -> alloy_primitives::U256 {
    alloy_primitives::U256::from(tokens)
        * alloy_primitives::U256::from(10u64).pow(alloy_primitives::U256::from(18u64))
}

/// Governance with a 10-block timelock and a proposer (2) with stake.
fn governance_db() -> (tempfile::TempDir, StateDb) {
    use revm::state::AccountInfo;
    use torus_economics::governance::GovernanceParams;
    let (d, db) = open_test_db();
    let ctx = make_ctx(db.clone(), false);
    ctx.governance
        .set_governance_params(&GovernanceParams {
            voting_period_blocks: 100,
            quorum_bps: 3300,
            min_proposal_stake: wei(100),
            permanent_weight_multiplier_num: 3,
            permanent_weight_multiplier_den: 2,
            treasury_address: addr(TREASURY),
            timelock_blocks: 10,
            permanent_unlock_threshold_bps: 8000,
        })
        .unwrap();
    let val = addr(1);
    let min = torus_economics::MIN_SELF_DELEGATION;
    db.put_account(&val, &AccountInfo { balance: min, ..Default::default() }).unwrap();
    ctx.staking.register_validator(val, [1; 32], 500, min).unwrap();
    db.put_account(&addr(2), &AccountInfo { balance: wei(500), ..Default::default() })
        .unwrap();
    ctx.staking.delegate(addr(2), val, wei(500)).unwrap();
    (d, db)
}

fn submit_fee_key(db: &StateDb, fees_on: bool, height: u64, key: &str, value: &str) -> torus_bridge::native_executor::NativeActionResult {
    let mut ctx = make_ctx(db.clone(), fees_on);
    ctx.block_height = height;
    let action = NativeAction::SubmitProposal(torus_types::Proposal {
        title: "fees".into(),
        description: "fees".into(),
        action: torus_types::ProposalAction::ParameterChange { key: key.into(), value: value.into() },
    });
    NativeExecutor::execute(&mut ctx, &addr(2), &action)
}

/// Bit off: a fee key proposal is refused exactly as before the feature
/// (`not modifiable`), so no proposal state exists that the base rejects.
/// Bit on: range-checked, and a passed proposal executed in a fee block
/// writes the row. A proposal executed in a block without the bit writes
/// nothing. RED before: accepted with the bit off.
#[test]
fn fee_keys_need_the_feature() {
    let (_d, db) = governance_db();
    for key in [TAKER_FEE_PARAM, MAKER_FEE_PARAM] {
        let off = submit_fee_key(&db, false, 3, key, "100");
        assert!(!off.success);
        assert!(off.error.as_deref().unwrap_or("").contains("not modifiable"), "{:?}", off.error);
    }
    let bad = submit_fee_key(&db, true, 3, TAKER_FEE_PARAM, "10001");
    assert!(!bad.success && bad.error.as_deref().unwrap_or("").contains("between"), "{:?}", bad.error);
    let on = submit_fee_key(&db, true, 3, TAKER_FEE_PARAM, "300");
    assert!(on.success, "{:?}", on.error);
    let run_governance = |fees_on: bool| {
        let mut ctx = make_ctx(db.clone(), fees_on);
        ctx.block_height = 120;
        ctx.governance.cast_vote(addr(2), 1, true, 10).ok();
        ctx.governance.finalize_proposal(1, 104).ok();
        let _ = NativeExecutor::process_governance(&mut ctx);
        assert!(ctx.fatal_error.is_none());
    };
    // Executed in a block without the bit: refused, no row.
    run_governance(false);
    assert_eq!(db.get_cf_raw(CF_FEE_CONFIG, TAKER_FEE_PARAM.as_bytes()).unwrap(), None);
    let on2 = submit_fee_key(&db, true, 130, TAKER_FEE_PARAM, "300");
    assert!(on2.success, "{:?}", on2.error);
    let mut ctx = make_ctx(db.clone(), true);
    ctx.block_height = 250;
    ctx.governance.cast_vote(addr(2), 2, true, 131).unwrap();
    ctx.governance.finalize_proposal(2, 231).unwrap();
    let _ = NativeExecutor::process_governance(&mut ctx);
    assert!(ctx.fatal_error.is_none());
    drop(ctx);
    let stored = db.get_cf_raw(CF_FEE_CONFIG, TAKER_FEE_PARAM.as_bytes()).unwrap();
    assert_eq!(stored.as_deref(), Some(b"300".as_slice()));
}
