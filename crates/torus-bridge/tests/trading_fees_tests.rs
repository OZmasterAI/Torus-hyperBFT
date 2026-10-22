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
