//! s94 bad-debt route (plan 9.11 design check, route (1)): fills far from
//! the mark. Probe 77a3b21 pinned the bug; these tests pin the fix.
//!
//! The route: placement and match-time margin valued a fill at its FILL
//! price (`order_book.rs` `maker_fill_fits` / `TakerMarginLimit::need`,
//! `native_executor.rs` `account_check` at the limit price), while the
//! account is valued at the MARK afterwards, and nothing banded the order
//! price against the mark. Two accounts of one owner: A buys at 2x the mark
//! from B; A reserved IM at the fill price but lost (fill − mark) × qty at
//! the mark, B held the matching gain.
//!
//! Probe numbers (market 1, 20x default tiers: IM 5%, MM 2.5% of notional;
//! mark 100; qty 10; A and B each funded exactly the IM at the fill price):
//! buy @200: A AV 100 − 1,000 = −900 (−9x its margin), ADL'd at block end,
//! its −900 left in the liquidator vault; B withdrew 1,100 for 200
//! deposited. Bad debt from +5.3% (buy) / −4.8% (sell).
//!
//! Fix, option 1 (s94): every fill's loss against the mark beyond its own IM
//! above maintenance is charged at match time (`margin::mark_loss`): a maker
//! that cannot pay it is margin-cancelled, a taker is cut to what it can
//! pay. With A funded at the IM, A only fills where it stays healthy.
//! Option 2: orders more than ±50% from the reference are rejected at
//! placement, so the probe's 2x / 0.5x prices never reach the book
//! (price_band_tests.rs); the tests here use prices inside the band.
//!
//! Helpers copied from liquidation_tests.rs and account_margin_tests.rs.

use std::collections::BTreeMap;

use alloy_primitives::{Address, U256};

use torus_bridge::native_executor::{NativeActionResult, NativeExecContext, NativeExecutor};
use torus_core::liquidation::{classify, Health, LIQUIDATOR_VAULT};
use torus_core::margin::AccountView;
use torus_core::position::{MarginType, NativeBalance, Position};
use torus_state::cf::{CF_NATIVE_BALANCES, CF_NATIVE_LIQUIDATION, CF_NATIVE_MARKETS, CF_NATIVE_POSITIONS};
use torus_state::{StateBackend, StateDb};
use torus_types::{FixedPoint, MarketId, NativeAction, OrderType, PlaceOrderParams, TimeInForce};

// ---- helpers (copied) ----

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

/// `v / 100` as a FixedPoint (exact: 2 decimals).
fn fp_cents(v: i64) -> FixedPoint {
    FixedPoint::from_raw(v as i128 * (FixedPoint::SCALE / 100))
}

/// Block `height`, timestamp `1_000 + height` (one second per block).
fn ctx_at(db: StateDb, height: u64) -> NativeExecContext {
    NativeExecContext::new(db, height, 1_000 + height, 0, 1_000, 10, addr(99), addr(100), addr(101))
}

/// Markets listed with the test-fixture row (undecodable => default 20x config).
fn liq_db(markets: &[MarketId]) -> (tempfile::TempDir, StateDb) {
    let (dir, db) = open_test_db();
    for m in markets {
        db.put_cf_raw(CF_NATIVE_MARKETS, &m.to_be_bytes(), b"listed").unwrap();
    }
    (dir, db)
}

fn fund(ctx: &NativeExecContext, t: &Address, amount: FixedPoint) {
    ctx.positions
        .put_native_balance(t, &NativeBalance { available: amount, order_margin: FixedPoint::ZERO })
        .unwrap();
}

fn bal(ctx: &NativeExecContext, t: &Address) -> NativeBalance {
    ctx.positions.get_native_balance(t).unwrap()
}

/// (available, order_margin) — `NativeBalance` has no `PartialEq`.
fn ab(ctx: &NativeExecContext, t: &Address) -> (FixedPoint, FixedPoint) {
    let b = bal(ctx, t);
    (b.available, b.order_margin)
}

fn pos(ctx: &NativeExecContext, t: &Address, m: MarketId) -> FixedPoint {
    match ctx.positions.get_position(t, m).unwrap() {
        Some(p) if p.is_long => p.size,
        Some(p) => -p.size,
        None => FixedPoint::ZERO,
    }
}

fn entry(ctx: &NativeExecContext, t: &Address, m: MarketId) -> FixedPoint {
    ctx.positions.get_position(t, m).unwrap().expect("position").entry_price
}

fn all_positions(ctx: &NativeExecContext) -> Vec<Position> {
    ctx.state
        .iterate_cf(CF_NATIVE_POSITIONS, None)
        .unwrap()
        .into_iter()
        .map(|(_, v)| borsh::from_slice(&v).unwrap())
        .collect()
}

/// Σ over every balance row (available + order margin) + Σ UPnL at `marks`.
fn total_value(ctx: &NativeExecContext, marks: &BTreeMap<MarketId, FixedPoint>) -> FixedPoint {
    let mut v = FixedPoint::ZERO;
    for (k, b) in ctx.state.iterate_cf(CF_NATIVE_BALANCES, None).unwrap() {
        if k.len() == 20 {
            let b: NativeBalance = borsh::from_slice(&b).unwrap();
            v += b.available + b.order_margin;
        }
    }
    for p in all_positions(ctx) {
        v += p.unrealized_pnl(marks[&p.market_id]);
    }
    v
}

fn limit(m: MarketId, is_buy: bool, price: i64, qty: i64) -> PlaceOrderParams {
    PlaceOrderParams {
        market_id: m,
        is_buy,
        price: fp(price),
        quantity: fp(qty),
        order_type: OrderType::Limit,
        time_in_force: TimeInForce::GTC,
        reduce_only: false,
        client_order_id: None,
    }
}

/// The aggregated mark of `m` at the context's block (3 equal reporters).
fn set_mark(ctx: &NativeExecContext, m: MarketId, price: FixedPoint) {
    let reporters = [addr(150), addr(151), addr(152)];
    for v in &reporters {
        ctx.oracle.submit_price(v, m, price, ctx.block_height, ctx.timestamp).unwrap();
    }
    let stakes: Vec<(Address, FixedPoint)> = reporters.iter().map(|v| (*v, fp(1))).collect();
    assert_eq!(ctx.oracle.aggregate_price(m, ctx.block_height, ctx.timestamp, &stakes).unwrap(), price);
}

fn to_spot(ctx: &mut NativeExecContext, t: &Address, a: FixedPoint) -> NativeActionResult {
    NativeExecutor::execute(ctx, t, &NativeAction::TransferToSpot { amount: U256::from(a.raw() as u128) })
}

fn liq_rows(ctx: &NativeExecContext, tag: u8) -> Vec<(Vec<u8>, Vec<u8>)> {
    ctx.state.iterate_cf(CF_NATIVE_LIQUIDATION, Some(&[tag])).unwrap()
}

#[derive(Clone, Copy, Debug)]
enum Path {
    /// `NativeExecutor::execute` per action (exec_place_order).
    Single,
    /// `execute_batch` canonical serial Phase-2 prepare (the block path).
    Batch,
    /// `execute_batch` sharded parallel Phase-2 prepare (+ parallel settle).
    Parallel,
}

const PATHS: [Path; 3] = [Path::Single, Path::Batch, Path::Parallel];

/// Filler sender for the parallel path (needs >= 2 distinct senders).
fn filler() -> Address {
    addr(200)
}

fn run(ctx: &mut NativeExecContext, path: Path, t: Address, p: PlaceOrderParams) -> NativeActionResult {
    let a = (t, NativeAction::PlaceOrder(p));
    match path {
        Path::Single => NativeExecutor::execute(ctx, &a.0, &a.1),
        Path::Batch => NativeExecutor::execute_batch_engine_mode(ctx, &[a], 1).results.remove(0),
        Path::Parallel => {
            let f = (filler(), NativeAction::PlaceOrder(limit(9, true, 1, 1)));
            NativeExecutor::execute_batch_engine_mode(ctx, &[a, f], 4).results.remove(0)
        }
    }
}

// ---- probe fixture ----

const M: MarketId = 1;
const MARK: i64 = 100;
const Q: i64 = 10;

/// The account that trades at the off-mark price (it takes the loss).
fn a() -> Address {
    addr(0x0A)
}

/// Its counterparty, same owner (it takes the gain).
fn b() -> Address {
    addr(0x0B)
}

/// An innocent third party.
fn c() -> Address {
    addr(0x0C)
}

/// IM of `Q` at `price` (20x: 5%) = `price / 2`, exact in cents.
fn im_at(price: i64) -> FixedPoint {
    fp_cents(price * Q * 100 / 20)
}

/// Account view at the mark (what liquidation / withdrawal read).
fn view(ctx: &NativeExecContext, t: &Address) -> AccountView {
    let ps = ctx.positions.positions_for_trader(t).unwrap();
    AccountView::build(&bal(ctx, t), &ps, |_| Some(fp(MARK)), |m| {
        ctx.margin_configs.get(&m).map(|c| c.tiers.as_slice())
    })
    .unwrap()
}

/// Mark 100 in market 1. A and B each funded exactly the IM of `Q` at
/// `price`. A trades `Q` at `price` (`a_buys`: A buys, B sells), as the
/// maker (A rests first) or the taker (B rests first). Both placements
/// must be accepted (asserted here).
fn off_mark_fill(path: Path, a_buys: bool, price: i64, a_maker: bool) -> (tempfile::TempDir, NativeExecContext) {
    let (d, db) = liq_db(&[M]);
    let mut ctx = ctx_at(db, 1);
    set_mark(&ctx, M, fp(MARK));
    fund(&ctx, &a(), im_at(price));
    fund(&ctx, &b(), im_at(price));
    fund(&ctx, &filler(), fp(1_000));
    let ao = (a(), limit(M, a_buys, price, Q));
    let bo = (b(), limit(M, !a_buys, price, Q));
    let (first, second) = if a_maker { (ao, bo) } else { (bo, ao) };
    for (t, p) in [first, second] {
        let r = run(&mut ctx, path, t, p);
        assert!(r.success, "{path:?} a_buys={a_buys} price={price} a_maker={a_maker}: {t}: {:?}", r.error);
    }
    (d, ctx)
}

// ============================================================================
// Q1: off-mark fills are refused (option 1: the mark charge at match time)
// ============================================================================

/// Off-mark prices inside the band: (a_buys, price). Buy 1.5x / 1.2x /
/// 1.06x, sell 0.5x (the band's edge) / 0.8x / 0.94x; all beyond the healthy
/// bound (buy > 102.63, sell < 97.62). The probe's 2x is refused at
/// placement by the band (price_band_tests.rs).
const OFF_MARK: [(bool, i64); 6] = [(true, 150), (true, 120), (true, 106), (false, 50), (false, 80), (false, 94)];

/// Was `fills_at_2x_and_half_the_mark_are_accepted_and_executed_on_every_path`
/// (probe: A filled 10 @200 → AV −900; @50 → AV −475). Now, on every
/// PlaceOrder path, A maker or taker, nothing fills: as a maker (its GTC
/// bid / ask) A is margin-cancelled whole (its reservation released); as a
/// GTC buy taker (not match-checked before s94) it is cut at once and its
/// order cancelled; as a GTC sell taker (match-checked) its reservation is
/// all held by its own fill + remainder (IM(10p)), so nothing is left for
/// the loss. A stays healthy and the end-of-block step leaves the vault at
/// 0. (A taker that never rests fills what its reservation pays: see the
/// IOC test below.)
#[test]
fn off_mark_fills_are_refused_on_every_path() {
    for path in PATHS {
        for (a_buys, price) in OFF_MARK {
            for a_maker in [true, false] {
                let what = format!("{path:?} a_buys={a_buys} price={price} a_maker={a_maker}");
                let (_d, mut ctx) = off_mark_fill(path, a_buys, price, a_maker);
                assert_eq!(pos(&ctx, &a(), M), FixedPoint::ZERO, "{what}: A's fill");
                assert_eq!(pos(&ctx, &b(), M), FixedPoint::ZERO, "{what}: B's fill");
                assert_eq!(ab(&ctx, &a()).1, FixedPoint::ZERO, "{what}: A holds no order margin (cancelled)");
                let va = view(&ctx, &a());
                assert!(va.equity() >= va.maintenance, "{what}: A healthy at the mark: {va:?}");
                assert_eq!(classify(&va), Some(Health::Healthy), "{what}");
                NativeExecutor::run_liquidations(&mut ctx);
                assert!(ctx.fatal_error.is_none(), "{what}: {:?}", ctx.fatal_error);
                assert_eq!(bal(&ctx, &LIQUIDATOR_VAULT).available, FixedPoint::ZERO, "{what}: vault");
                assert_eq!(ab(&ctx, &a()), (im_at(price), FixedPoint::ZERO), "{what}: A keeps its deposit");
            }
        }
    }
}

/// Option 1, a checked IOC buy taker: IOC buy 10 @120 against B's ask 10
/// @120, A funded its reservation IM(1,200) = 60. Per unit: IM 6 + loss 20
/// − tolerance 3.5 = 22.5 → 2 units fit (45 <= 60), the rest is cancelled.
/// A: long 2 @120, AV 60 − 40 = 20 >= MM 5. Every path.
#[test]
fn a_checked_ioc_buy_through_the_mark_fills_what_its_reservation_pays() {
    for path in PATHS {
        let (_d, db) = liq_db(&[M]);
        let mut ctx = ctx_at(db, 1);
        set_mark(&ctx, M, fp(MARK));
        fund(&ctx, &a(), im_at(120));
        fund(&ctx, &b(), im_at(120));
        fund(&ctx, &filler(), fp(1_000));
        assert!(run(&mut ctx, path, b(), limit(M, false, 120, Q)).success, "{path:?}");
        let ioc = PlaceOrderParams { time_in_force: TimeInForce::IOC, ..limit(M, true, 120, Q) };
        assert!(run(&mut ctx, path, a(), ioc).success, "{path:?}");
        assert_eq!(pos(&ctx, &a(), M), fp(2), "{path:?}");
        let va = view(&ctx, &a());
        assert_eq!(va.equity(), fp(20), "{path:?}");
        assert_eq!(classify(&va), Some(Health::Healthy), "{path:?}");
    }
}

/// Q1, the partial block: the book itself. With an innocent ask resting at
/// 101, A's bid at 150 (the probe's 200 is outside the band now) takes it AT
/// 101 (price-time priority): A ends long
/// 10 @101, healthy (equity 90 >= MM 25): a fill near the mark is not
/// charged (loss 10 <= tolerance 50.5 − 25).
#[test]
fn a_through_mark_bid_takes_resting_asks_near_the_mark_first() {
    for path in PATHS {
        let (_d, db) = liq_db(&[M]);
        let mut ctx = ctx_at(db, 1);
        set_mark(&ctx, M, fp(MARK));
        fund(&ctx, &a(), im_at(200));
        fund(&ctx, &c(), fp(1_000));
        fund(&ctx, &filler(), fp(1_000));
        assert!(run(&mut ctx, path, c(), limit(M, false, 101, Q)).success);
        assert!(run(&mut ctx, path, a(), limit(M, true, 150, Q)).success);
        assert_eq!(pos(&ctx, &a(), M), fp(Q), "{path:?}");
        assert_eq!(entry(&ctx, &a(), M), fp(101), "{path:?}: filled at the resting ask");
        assert_eq!(classify(&view(&ctx, &a())), Some(Health::Healthy), "{path:?}");
    }
}

// ============================================================================
// Q3 + Q4: nothing for the vault, nothing extra for B
// ============================================================================

/// Was `counterparty_withdraws_1100_of_200_deposited_and_the_vault_is_left_at_minus_900`.
/// Block path, A maker @120 (the probe used 200, now refused by the band):
/// A's bid is margin-cancelled when B's sell arrives (its 60 released), B's
/// sell rests holding B's 60. B cannot take out more than it put in:
/// nothing while its order rests; after cancelling, its 60 and not 1 more.
/// The step leaves the vault at 0; value conserved.
#[test]
fn the_counterparty_gets_back_only_its_deposit_and_the_vault_stays_at_zero() {
    let (_d, mut ctx) = off_mark_fill(Path::Batch, true, 120, true);
    assert_eq!(ab(&ctx, &a()), (fp(60), FixedPoint::ZERO), "A's bid cancelled, reservation released");
    assert_eq!(ab(&ctx, &b()), (FixedPoint::ZERO, fp(60)), "B's sell rests");
    assert!(!to_spot(&mut ctx, &b(), fp(1)).success, "B's deposit is in its resting order");
    let marks: BTreeMap<MarketId, FixedPoint> = [(M, fp(MARK))].into();
    let before = total_value(&ctx, &marks);
    NativeExecutor::run_liquidations(&mut ctx);
    assert!(ctx.fatal_error.is_none(), "{:?}", ctx.fatal_error);
    assert_eq!(bal(&ctx, &LIQUIDATOR_VAULT).available, FixedPoint::ZERO);
    assert_eq!(total_value(&ctx, &marks), before, "value conserved");
    let cancel = NativeAction::CancelAllOrders { market_id: None };
    assert!(NativeExecutor::execute(&mut ctx, &b(), &cancel).success);
    assert!(to_spot(&mut ctx, &b(), fp(60)).success, "B's own deposit");
    let r = to_spot(&mut ctx, &b(), fp(1));
    assert!(!r.success, "nothing more: {:?}", r.error);
    assert_eq!(bal(&ctx, &LIQUIDATOR_VAULT).available, FixedPoint::ZERO);
}

/// Was `an_innocent_higher_ranked_short_is_adld_at_the_mark_and_the_vault_still_pays_900`:
/// (A @120 now) with A's fill refused nobody is under water, so the end-of-block step
/// ADLs no one — the innocent short C keeps its position — and the vault
/// stays at 0.
#[test]
fn an_innocent_short_keeps_its_position_and_the_vault_pays_nothing() {
    let d_ = addr(0x0D);
    let (_d, mut ctx) = off_mark_fill(Path::Batch, true, 120, true);
    fund(&ctx, &c(), fp(200));
    fund(&ctx, &d_, fp(1_000));
    ctx.positions.apply_fill(&d_, M, true, fp(Q), fp(MARK), MarginType::Cross).unwrap();
    ctx.positions.apply_fill(&c(), M, false, fp(Q), fp(MARK), MarginType::Cross).unwrap();
    ctx.state
        .put_cf_raw(CF_NATIVE_LIQUIDATION, &[[0x03u8].as_slice(), &M.to_be_bytes()].concat(), &fp(MARK).raw().to_be_bytes())
        .unwrap();
    NativeExecutor::run_liquidations(&mut ctx);
    assert!(ctx.fatal_error.is_none(), "{:?}", ctx.fatal_error);
    assert_eq!(pos(&ctx, &a(), M), FixedPoint::ZERO, "A never filled");
    assert_eq!(pos(&ctx, &c(), M), fp(-Q), "C untouched");
    assert_eq!(ab(&ctx, &c()), (fp(200), FixedPoint::ZERO));
    assert_eq!(bal(&ctx, &LIQUIDATOR_VAULT).available, FixedPoint::ZERO);
}

/// Was `with_64_under_mm_accounts_ahead_the_buyer_is_liquidated_one_block_later`
/// (the round-robin delay is a liquidation property, unchanged): 64 stage-1
/// decoys sorting before A still use the fill block's whole act budget
/// (cursor = the 64th), but A has nothing to liquidate (its fill was
/// refused), and after two steps the vault is still at 0.
#[test]
fn with_64_under_mm_accounts_ahead_nothing_is_left_for_the_vault() {
    let (_d, mut ctx) = off_mark_fill(Path::Batch, true, 120, true);
    ctx.state.put_cf_raw(CF_NATIVE_MARKETS, &2u64.to_be_bytes(), b"listed").unwrap();
    let s = addr(0x50);
    fund(&ctx, &s, fp(10_000_000));
    let decoys: Vec<Address> = (0..64u8)
        .map(|i| {
            let mut k = [0x01u8; 20];
            k[19] = i;
            Address::new(k)
        })
        .collect();
    for t in &decoys {
        fund(&ctx, t, fp(345));
        ctx.positions.apply_fill(t, 2, true, fp(10), fp(1_000), MarginType::Cross).unwrap();
        ctx.positions.apply_fill(&s, 2, false, fp(10), fp(1_000), MarginType::Cross).unwrap();
    }
    set_mark(&ctx, 2, fp(990));
    assert!(decoys.iter().all(|t| *t < a()), "the decoys sort before A");
    NativeExecutor::run_liquidations(&mut ctx);
    assert!(ctx.fatal_error.is_none(), "{:?}", ctx.fatal_error);
    let cursor = liq_rows(&ctx, 0x04).first().map(|(_, v)| Address::from_slice(v));
    assert_eq!(cursor, Some(decoys[63]), "the pass was cut after the 64th decoy");
    assert_eq!(pos(&ctx, &a(), M), FixedPoint::ZERO, "A never filled");
    NativeExecutor::run_liquidations(&mut ctx);
    assert!(ctx.fatal_error.is_none(), "{:?}", ctx.fatal_error);
    assert_eq!(bal(&ctx, &LIQUIDATOR_VAULT).available, FixedPoint::ZERO);
}

// ============================================================================
// Q5: where fills stop, both sides
// ============================================================================

/// Was `deviation_sweep_bad_debt_starts_above_5_percent_off_the_mark` (probe:
/// healthy to +2.6% / −2.4%, stage 1, backstop, ADL with vault 106: −7, 120:
/// −140, 200: −900, 95: −2.5, 80: −160, 50: −475; 200 is now refused at
/// placement by the band, 50 is its edge). Block path, A maker
/// funded exactly the IM at the fill price: A fills while the fill keeps it
/// healthy at the mark (buy <= 102, sell >= 98: the charge is 0 there) and
/// is margin-cancelled beyond (the charge needs free margin A has not got).
/// The vault stays at 0 at every price.
#[test]
fn deviation_sweep_fills_stop_where_the_account_would_turn_unhealthy() {
    // (a_buys, price, A fills, A AV after in cents)
    let cases: [(bool, i64, bool, i64); 14] = [
        // (true, 200, ..): outside the band, see price_band_tests.rs
        (true, 102, true, 3_100),
        (true, 103, false, 5_150),
        (true, 104, false, 5_200),
        (true, 105, false, 5_250),
        (true, 106, false, 5_300),
        (true, 120, false, 6_000),
        (true, 150, false, 7_500),
        (false, 98, true, 2_900),
        (false, 97, false, 4_850),
        (false, 96, false, 4_800),
        (false, 95, false, 4_750),
        (false, 94, false, 4_700),
        (false, 80, false, 4_000),
        (false, 50, false, 2_500),
    ];
    let mut table = String::new();
    for (a_buys, price, fills, av) in cases {
        let what = format!("a_buys={a_buys} price={price}");
        let (_d, mut ctx) = off_mark_fill(Path::Batch, a_buys, price, true);
        let sign = if a_buys { 1 } else { -1 };
        assert_eq!(pos(&ctx, &a(), M), fp(if fills { sign * Q } else { 0 }), "{what}");
        let va = view(&ctx, &a());
        assert_eq!(va.equity(), fp_cents(av), "{what}: A's AV at the mark");
        assert_eq!(classify(&va), Some(Health::Healthy), "{what}");
        NativeExecutor::run_liquidations(&mut ctx);
        assert!(ctx.fatal_error.is_none(), "{what}: {:?}", ctx.fatal_error);
        assert_eq!(bal(&ctx, &LIQUIDATOR_VAULT).available, FixedPoint::ZERO, "{what}: vault");
        table.push_str(&format!("{what}: filled {fills} AV {}\n", va.equity()));
    }
    println!("{table}");
}

// ============================================================================
// 9.11 interaction: one batch, several books, one maker snapshot each
// ============================================================================

/// Option 1 lets a maker's fill draw its free margin (the mark charge), and
/// each book of a batch checks makers against their OWN copy of the
/// snapshot (plan 9.11): across k books one batch can charge up to k x the
/// snapshot free. Pinned: maker M, flat, bids 10 @103 in markets 1, 2, 3
/// (mark 100 each; reservations 3 x 51.5), free 3.5 left. One batch with a
/// sell into each: every book sees free 3.5 and charges its 3.5 (loss 30 −
/// tolerance 26.5) — 10.5 in total, 7 = (k − 1) x 3.5 more than M's free.
/// With free 3.49 no book fills. M ends long 3 x 10 @103: AV 158 − 90 = 68,
/// MM 75 (stage 1, not under water). The band (option 2) caps a fill's loss
/// at 50% of its notional, which caps what one fill can charge.
#[test]
fn one_batch_charges_a_makers_snapshot_free_once_per_book() {
    let mk = addr(0x4D);
    let markets: [MarketId; 3] = [1, 2, 3];
    for (free, fills) in [(fp_cents(350), true), (fp_cents(349), false)] {
        for threads in [1usize, 4] {
            let what = format!("free {free} threads {threads}");
            let (_d, db) = liq_db(&markets);
            let mut ctx = ctx_at(db, 1);
            for m in markets {
                set_mark(&ctx, m, fp(MARK));
            }
            fund(&ctx, &mk, fp_cents(3 * 5_150) + free);
            let bids: Vec<PlaceOrderParams> = markets.iter().map(|&m| limit(m, true, 103, Q)).collect();
            let r = NativeExecutor::execute_batch_engine_mode(&mut ctx, &[(mk, NativeAction::PlaceOrderBatch(bids))], 1);
            assert!(r.results.iter().all(|r| r.success), "{what}: {:?}", r.results);
            assert_eq!(ab(&ctx, &mk), (free, fp_cents(3 * 5_150)), "{what}: three bids rest");
            let sells: Vec<(Address, NativeAction)> = markets
                .iter()
                .map(|&m| {
                    let s = addr(0x60 + m as u8);
                    fund(&ctx, &s, fp(1_000));
                    (s, NativeAction::PlaceOrder(limit(m, false, 103, Q)))
                })
                .collect();
            let r = NativeExecutor::execute_batch_engine_mode(&mut ctx, &sells, threads);
            assert!(r.results.iter().all(|r| r.success), "{what}: {:?}", r.results);
            for m in markets {
                assert_eq!(pos(&ctx, &mk, m), if fills { fp(Q) } else { FixedPoint::ZERO }, "{what}: market {m}");
            }
            let ps = ctx.positions.positions_for_trader(&mk).unwrap();
            let v = AccountView::build(&bal(&ctx, &mk), &ps, |_| Some(fp(MARK)), |m| {
                ctx.margin_configs.get(&m).map(|c| c.tiers.as_slice())
            })
            .unwrap();
            if fills {
                assert_eq!(v.equity(), fp(68), "{what}");
                assert_eq!(v.free(), fp(-82), "{what}: 3 x 3.5 charged against 3.5");
                assert_eq!(classify(&v), Some(Health::Stage1), "{what}");
            } else {
                assert_eq!(v.equity(), fp_cents(3 * 5_150) + free, "{what}: all three cancelled, nothing lost");
            }
        }
    }
}
