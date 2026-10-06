//! s94 bad-debt probe (TESTS ONLY, evidence for an owner decision; plan 9.11
//! design check, route (1)): fills far from the mark.
//!
//! Suspected route: placement and match-time margin value a fill at its FILL
//! price (`order_book.rs` `maker_fill_fits` / `TakerMarginLimit::need`,
//! `native_executor.rs` `account_check` at the limit price), while the
//! account is valued at the MARK afterwards; no placement price band vs the
//! mark exists (RPC intake checks market / tick / lot only). Two accounts of
//! one owner: A buys at 2x the mark from B. A reserves IM at the fill price
//! but loses (fill - mark) x qty at the mark; B holds the matching gain.
//!
//! These tests pin the CURRENT behaviour. Tests marked
//! `DOCUMENTS CURRENT BEHAVIOUR (s94 bad-debt probe)` assert the bug and are
//! expected to flip when a price band (or fills valued at the mark) lands.
//!
//! Numbers (market 1, 20x default tiers: IM 5%, MM 2.5% of notional; mark
//! 100; qty 10; A and B each funded exactly the IM at the fill price):
//!   buy @200: A AV 100 - 1,000 = -900 (= -9x its margin), B AV +1,100.
//!   End-of-block step: A is ADL'd at the mark (no previous mark / previous
//!   mark 100 < bankruptcy 190), its -900 goes to the liquidator vault; B
//!   (the only short) closes at 100 and holds 1,100 cash = 100 deposit + A's
//!   100 + the vault's 900.
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
// Q1: accepted at placement, executed
// ============================================================================

/// Q1: a buy at 2x the mark (200) and a sell at 0.5x (50) are accepted and
/// FILL at that price on all three PlaceOrder paths, A maker or taker.
/// Afterwards, at the mark: buy @200: A equity 100 - 1,000 = -900, free
/// -950 (IM at the mark 50), class ADL; B equity 1,100. Sell @50: A equity
/// 25 - 500 = -475, free -525; B equity 525.
// DOCUMENTS CURRENT BEHAVIOUR (s94 bad-debt probe): expected to flip when a price band lands
#[test]
fn fills_at_2x_and_half_the_mark_are_accepted_and_executed_on_every_path() {
    // (a_buys, price, A equity, A free, B equity)
    let cases = [(true, 200, -900, -950, 1_100), (false, 50, -475, -525, 525)];
    for path in PATHS {
        for (a_buys, price, a_eq, a_free, b_eq) in cases {
            for a_maker in [true, false] {
                let what = format!("{path:?} a_buys={a_buys} price={price} a_maker={a_maker}");
                let (_d, ctx) = off_mark_fill(path, a_buys, price, a_maker);
                let sign = if a_buys { 1 } else { -1 };
                assert_eq!(pos(&ctx, &a(), M), fp(sign * Q), "{what}: A filled");
                assert_eq!(pos(&ctx, &b(), M), fp(-sign * Q), "{what}: B filled");
                assert_eq!(entry(&ctx, &a(), M), fp(price), "{what}: at the off-mark price");
                assert_eq!(ab(&ctx, &a()), (im_at(price), FixedPoint::ZERO), "{what}: A's cash, no order margin left");
                let va = view(&ctx, &a());
                assert_eq!(va.equity(), fp(a_eq), "{what}: A equity at the mark");
                assert_eq!(va.free(), fp(a_free), "{what}: A free margin at the mark");
                assert_eq!(va.maintenance, fp_cents(2_500), "{what}: A MM at the mark");
                assert_eq!(classify(&va), Some(Health::Adl), "{what}");
                assert_eq!(view(&ctx, &b()).equity(), fp(b_eq), "{what}: B equity at the mark");
            }
        }
    }
}

/// Q1, the partial block: the book itself. With an innocent ask resting at
/// 101, A's bid at 200 takes it AT 101 (price-time priority): A ends long
/// 10 @101, healthy (equity 90 >= MM 25). The route needs the opposite side
/// of the book EMPTY between the mark and the attack price (a thin market),
/// or the attacker must first buy everything in between.
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
        assert!(run(&mut ctx, path, a(), limit(M, true, 200, Q)).success);
        assert_eq!(pos(&ctx, &a(), M), fp(Q), "{path:?}");
        assert_eq!(entry(&ctx, &a(), M), fp(101), "{path:?}: filled at the resting ask");
        assert_eq!(classify(&view(&ctx, &a())), Some(Health::Healthy), "{path:?}");
    }
}

// ============================================================================
// Q2 + Q3 + Q4: liquidation, who pays, what B can withdraw
// ============================================================================

/// Q3 + Q4 (block path, A maker): right after the fill (before the end-of-
/// block step) B withdraws its whole deposit (100: equity 1,100 - 100 >=
/// transfer margin max(50, 100)); 1 more is refused by the CASH bound
/// (available 0; UPnL is not cash). The step ADLs A (AV -900 < 0) against B,
/// the only short, at the mark 100 (no previous mark; bankruptcy price 190
/// is better for A, so no clamp): B closes +1,000 -> available 1,000; A ends
/// (0, 0); A's -900 moves to the LIQUIDATOR VAULT. Next block B withdraws
/// the 1,000. Total out: 1,100 for 200 deposited (A 100 + B 100); the
/// vault is left at -900 (value conserved, the deficit sits in the vault).
// DOCUMENTS CURRENT BEHAVIOUR (s94 bad-debt probe): expected to flip when a price band lands
#[test]
fn counterparty_withdraws_1100_of_200_deposited_and_the_vault_is_left_at_minus_900() {
    let (_d, mut ctx) = off_mark_fill(Path::Batch, true, 200, true);
    let r = to_spot(&mut ctx, &b(), fp(100));
    assert!(r.success, "B's deposit, at once: {:?}", r.error);
    let r = to_spot(&mut ctx, &b(), fp(1));
    assert!(!r.success, "unrealized gain is not cash");
    assert!(r.error.as_deref().unwrap_or("").to_lowercase().contains("insufficient"), "{:?}", r.error);
    let marks: BTreeMap<MarketId, FixedPoint> = [(M, fp(MARK))].into();
    let before = total_value(&ctx, &marks);
    NativeExecutor::run_liquidations(&mut ctx); // end of the fill block
    assert!(ctx.fatal_error.is_none(), "{:?}", ctx.fatal_error);
    assert_eq!(pos(&ctx, &a(), M), FixedPoint::ZERO, "A ADL'd in the fill block");
    assert_eq!(pos(&ctx, &b(), M), FixedPoint::ZERO, "B was the ADL counterparty");
    assert_eq!(ab(&ctx, &a()), (FixedPoint::ZERO, FixedPoint::ZERO));
    assert_eq!(ab(&ctx, &b()), (fp(1_000), FixedPoint::ZERO), "closed at the mark 100");
    assert_eq!(bal(&ctx, &LIQUIDATOR_VAULT).available, fp(-900), "the vault takes A's deficit");
    assert_eq!(total_value(&ctx, &marks), before, "value conserved");
    assert!(liq_rows(&ctx, 0x06).is_empty(), "the vault's -900 (no positions: not valued) leaves no pending row");
    let mut next = ctx_at(ctx.state.clone(), 2);
    let r = to_spot(&mut next, &b(), fp(1_000));
    assert!(r.success, "next block: B withdraws the gain: {:?}", r.error);
    assert_eq!(ab(&next, &b()), (FixedPoint::ZERO, FixedPoint::ZERO));
    assert_eq!(bal(&next, &LIQUIDATOR_VAULT).available, fp(-900));
}

/// Q4, who pays with an innocent short in the market: C short 10 @100 (vs
/// D long), funded 200 -> ADL rank 1 x 1,000 / 200 = 5 > B's 2 x 1,000 /
/// 1,100: C is ADL'd FIRST, at the mark 100 = its entry: C realizes 0 and
/// loses its position (no money). B keeps short 10 @200 (UPnL +1,000). The
/// vault still takes A's -900. A previous mark (100, stored by a step at
/// block 1) gives the same price (100 < bankruptcy 190).
// DOCUMENTS CURRENT BEHAVIOUR (s94 bad-debt probe): expected to flip when a price band lands
#[test]
fn an_innocent_higher_ranked_short_is_adld_at_the_mark_and_the_vault_still_pays_900() {
    let d_ = addr(0x0D);
    let (_d, mut ctx) = off_mark_fill(Path::Batch, true, 200, true);
    fund(&ctx, &c(), fp(200));
    fund(&ctx, &d_, fp(1_000));
    ctx.positions.apply_fill(&d_, M, true, fp(Q), fp(MARK), MarginType::Cross).unwrap();
    ctx.positions.apply_fill(&c(), M, false, fp(Q), fp(MARK), MarginType::Cross).unwrap();
    // A previous mark row (as every live block after the first has).
    ctx.state
        .put_cf_raw(CF_NATIVE_LIQUIDATION, &[[0x03u8].as_slice(), &M.to_be_bytes()].concat(), &fp(MARK).raw().to_be_bytes())
        .unwrap();
    NativeExecutor::run_liquidations(&mut ctx);
    assert!(ctx.fatal_error.is_none(), "{:?}", ctx.fatal_error);
    assert_eq!(pos(&ctx, &a(), M), FixedPoint::ZERO);
    assert_eq!(pos(&ctx, &c(), M), FixedPoint::ZERO, "C ranked first: closed");
    assert_eq!(ab(&ctx, &c()), (fp(200), FixedPoint::ZERO), "C closed at its entry: no PnL");
    assert_eq!(pos(&ctx, &b(), M), fp(-Q), "B keeps its short");
    assert_eq!(view(&ctx, &b()).equity(), fp(1_100));
    assert_eq!(bal(&ctx, &LIQUIDATOR_VAULT).available, fp(-900));
}

/// Q4, the round-robin (production budgets LIQ_SCAN_PER_BLOCK 2,048 /
/// LIQ_ACT_PER_BLOCK 64, no touched-first priority): 64 stage-1 accounts
/// whose keys sort before A use the whole act budget of the fill block's
/// step, so A is NOT liquidated in the fill block (cursor = the 64th). In
/// the gap B withdraws its deposit; the next block's step ADLs A (against B
/// at the previous mark 100) and the vault still ends at -900. A's delay is
/// one block per 64 under-MM accounts ahead of it in the round.
#[test]
fn with_64_under_mm_accounts_ahead_the_buyer_is_liquidated_one_block_later() {
    let (_d, mut ctx) = off_mark_fill(Path::Batch, true, 200, true);
    // Decoys in market 2 (mark 990): long 10 @1,000 on 345 -> AV 245 < MM
    // 247.5, >= 2/3 MM: stage 1 into an empty book (stays under MM).
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
    NativeExecutor::run_liquidations(&mut ctx); // end of the fill block
    assert!(ctx.fatal_error.is_none(), "{:?}", ctx.fatal_error);
    assert_eq!(pos(&ctx, &a(), M), fp(Q), "act budget used up before A: not liquidated");
    let cursor = liq_rows(&ctx, 0x04).first().map(|(_, v)| Address::from_slice(v));
    assert_eq!(cursor, Some(decoys[63]), "the pass was cut after the 64th decoy");
    assert!(to_spot(&mut ctx, &b(), fp(100)).success, "in the gap: B's deposit");
    NativeExecutor::run_liquidations(&mut ctx); // next block's step
    assert!(ctx.fatal_error.is_none(), "{:?}", ctx.fatal_error);
    assert_eq!(pos(&ctx, &a(), M), FixedPoint::ZERO, "next block: A ADL'd");
    assert_eq!(ab(&ctx, &b()), (fp(1_000), FixedPoint::ZERO), "B closed at the previous mark 100");
    assert_eq!(bal(&ctx, &LIQUIDATOR_VAULT).available, fp(-900));
}

// ============================================================================
// Q5: where it starts to bite, both sides
// ============================================================================

/// Expected outcome of the end-of-block step for A.
#[derive(Clone, Copy, Debug, PartialEq)]
enum After {
    /// A untouched.
    Kept,
    /// Stage 1 into an empty book: nothing fills, A keeps the position, pending.
    Pending,
    /// Backstop: position (at the mark) + collateral to the vault.
    Vault,
    /// ADL: closed against B at the mark, deficit to the vault.
    Deleveraged,
}

/// Q5 sweep (block path, A maker, mark 100, A funded exactly the IM at the
/// fill price). Buy at p: A's AV = p/2 - 10 (p - 100) = 1,000 - 9.5p, MM
/// 25: healthy to p <= 102.63 (+2.6%), stage 1 to 103.51, backstop to
/// 105.26 (+5.3%), ADL (bad debt) above. Sell at p: AV = 10.5p - 1,000:
/// healthy from 97.62 (-2.4%), ADL below 95.24 (-4.8%). Vault after the
/// step: backstop -> A's position at the mark (A realizes the loss) + A's
/// remaining collateral = A's AV (>= 0); ADL -> A's AV (negative): 1.2x:
/// -140; 0.8x: -160; 2x: -900; 0.5x: -475.
// DOCUMENTS CURRENT BEHAVIOUR (s94 bad-debt probe): expected to flip when a price band lands
#[test]
fn deviation_sweep_bad_debt_starts_above_5_percent_off_the_mark() {
    use After::*;
    use Health::{Adl, Backstop, Healthy, Stage1};
    // (a_buys, price, class, A AV in cents, after, vault available in cents)
    let cases: [(bool, i64, Health, i64, After, i64); 14] = [
        (true, 102, Healthy, 3_100, Kept, 0),
        (true, 103, Stage1, 2_150, Pending, 0),
        (true, 104, Backstop, 1_200, Vault, 1_200),
        (true, 105, Backstop, 250, Vault, 250),
        (true, 106, Adl, -700, Deleveraged, -700),
        (true, 120, Adl, -14_000, Deleveraged, -14_000),
        (true, 200, Adl, -90_000, Deleveraged, -90_000),
        (false, 98, Healthy, 2_900, Kept, 0),
        (false, 97, Stage1, 1_850, Pending, 0),
        (false, 96, Backstop, 800, Vault, 800),
        (false, 95, Adl, -250, Deleveraged, -250),
        (false, 94, Adl, -1_300, Deleveraged, -1_300),
        (false, 80, Adl, -16_000, Deleveraged, -16_000),
        (false, 50, Adl, -47_500, Deleveraged, -47_500),
    ];
    let mut table = String::new();
    for (a_buys, price, class, av, after, vault) in cases {
        let what = format!("a_buys={a_buys} price={price}");
        let (_d, mut ctx) = off_mark_fill(Path::Batch, a_buys, price, true);
        let va = view(&ctx, &a());
        assert_eq!(classify(&va), Some(class), "{what}");
        assert_eq!(va.equity(), fp_cents(av), "{what}: A's AV at the mark");
        NativeExecutor::run_liquidations(&mut ctx);
        assert!(ctx.fatal_error.is_none(), "{what}: {:?}", ctx.fatal_error);
        let sign = if a_buys { 1 } else { -1 };
        let a_pos = pos(&ctx, &a(), M);
        match after {
            Kept | Pending => assert_eq!(a_pos, fp(sign * Q), "{what}: A keeps its position"),
            Vault => {
                assert_eq!(a_pos, FixedPoint::ZERO, "{what}");
                assert_eq!(pos(&ctx, &LIQUIDATOR_VAULT, M), fp(sign * Q), "{what}: the vault holds it");
                assert_eq!(entry(&ctx, &LIQUIDATOR_VAULT, M), fp(MARK), "{what}: at the mark");
            }
            Deleveraged => {
                assert_eq!(a_pos, FixedPoint::ZERO, "{what}");
                assert_eq!(pos(&ctx, &b(), M), FixedPoint::ZERO, "{what}: B closed against A");
                let gain = fp((price - MARK).abs() * Q);
                assert_eq!(bal(&ctx, &b()).available, im_at(price) + gain, "{what}: B realized |p - mark| x q");
            }
        }
        assert_eq!(liq_rows(&ctx, 0x06).iter().any(|(k, _)| k[1..] == *a().as_slice()), after == Pending, "{what}: A pending");
        assert_eq!(bal(&ctx, &LIQUIDATOR_VAULT).available, fp_cents(vault), "{what}: vault");
        table.push_str(&format!("{what}: {class:?} AV {} -> {after:?}, vault {}\n", va.equity(), fp_cents(vault)));
    }
    println!("{table}");
}
