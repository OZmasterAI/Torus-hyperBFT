//! F1 (s517): account-level margin, Hyperliquid cross margin (computed).
//!
//! `docs/plans/account-level-margin-f1.md` (decisions s517 are binding) and
//! `docs/plans/account-level-margin-f1-impl.md`. Pins:
//!   - withdrawals (TransferToSpot, Withdraw{to}) keep the account's transfer
//!     margin, SAFE variant: `amount <= available` AND equity − order margin −
//!     amount >= max(Σ position IM, 10% × Σ position notional);
//!   - placement is checked against the ACCOUNT's free margin (UPnL counts,
//!     position-size tier, closing is free) — the only placement gate (D1,
//!     strict HL: `available` may go negative);
//!   - the match-time budget is the order's reservation + the sender's running
//!     free margin (exclusive per batch, D2).
//!
//! Every scenario runs through all three PlaceOrder paths (`per_path!`):
//! single-action `execute`, `execute_batch` serial Phase 2, and the sharded
//! parallel Phase 2. Helpers copied from `market_order_margin_tests.rs`.

use alloy_primitives::{Address, U256};

use torus_bridge::native_executor::{NativeActionResult, NativeExecContext, NativeExecutor};
use torus_core::margin::{MarginTier, MarketMarginConfig};
use torus_core::position::NativeBalance;
use torus_state::StateDb;
use torus_types::{FixedPoint, MarketId, NativeAction, OrderType, PlaceOrderParams, TimeInForce};

// ---- Helpers (copied from market_order_margin_tests.rs) ----

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

/// `v / 10,000` as a FixedPoint (exact: 4 decimals).
fn fp_e4(v: i64) -> FixedPoint {
    FixedPoint::from_raw(v as i128 * (FixedPoint::SCALE / 10_000))
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

fn limit(market_id: MarketId, is_buy: bool, price: i64, qty: i64) -> PlaceOrderParams {
    PlaceOrderParams {
        market_id,
        is_buy,
        price: fp(price),
        quantity: fp(qty),
        order_type: OrderType::Limit,
        time_in_force: TimeInForce::GTC,
        reduce_only: false,
        client_order_id: None,
    }
}

fn market(market_id: MarketId, is_buy: bool, cap: FixedPoint, qty: i64) -> PlaceOrderParams {
    PlaceOrderParams {
        market_id,
        is_buy,
        price: cap,
        quantity: fp(qty),
        order_type: OrderType::Market,
        time_in_force: TimeInForce::IOC,
        reduce_only: false,
        client_order_id: None,
    }
}

fn place(sender: Address, p: PlaceOrderParams) -> (Address, NativeAction) {
    (sender, NativeAction::PlaceOrder(p))
}

fn bal(ctx: &NativeExecContext, trader: &Address) -> NativeBalance {
    ctx.positions.get_native_balance(trader).unwrap()
}

fn assert_bal(
    ctx: &NativeExecContext,
    trader: &Address,
    avail: FixedPoint,
    margin: FixedPoint,
    what: &str,
) {
    let b = bal(ctx, trader);
    assert_eq!(b.available, avail, "{what}: available");
    assert_eq!(b.order_margin, margin, "{what}: order_margin");
}

const FUNDING: i64 = 1_000;

/// Filler sender for the parallel-prepare path (needs >= 2 distinct senders):
/// a tiny resting bid in an unrelated market.
fn filler() -> Address {
    addr(200)
}

#[derive(Clone, Copy, Debug)]
enum Path {
    /// `NativeExecutor::execute` per action (exec_place_order).
    Single,
    /// `execute_batch` canonical serial Phase-2 prepare.
    Batch,
    /// `execute_batch` sharded parallel Phase-2 prepare (+ parallel settle).
    Parallel,
}

fn run(ctx: &mut NativeExecContext, path: Path, actions: &[(Address, NativeAction)]) -> Vec<NativeActionResult> {
    match path {
        Path::Single => actions
            .iter()
            .map(|(s, a)| NativeExecutor::execute(ctx, s, a))
            .collect(),
        Path::Batch => NativeExecutor::execute_batch_engine_mode(ctx, actions, 1).results,
        Path::Parallel => {
            let mut v = actions.to_vec();
            v.push(place(filler(), limit(9, true, 1, 1)));
            let mut r = NativeExecutor::execute_batch_engine_mode(ctx, &v, 4).results;
            r.pop();
            r
        }
    }
}

fn fresh(path: Path, traders: &[Address]) -> (tempfile::TempDir, NativeExecContext) {
    let (dir, db) = open_test_db();
    let ctx = make_ctx(db);
    for t in traders {
        fund_native(&ctx, t, fp(FUNDING));
    }
    if matches!(path, Path::Parallel) {
        fund_native(&ctx, &filler(), fp(FUNDING));
    }
    (dir, ctx)
}

/// Publish `price` as market `market_id`'s aggregated oracle (mark) price at
/// the context's block: three equally staked reporters, so the stake-weighted
/// median is exactly `price`.
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

// ---- F1 helpers ----

/// Signed position size (+long / -short / 0 flat) of `t` in market `m`.
fn pos_in(ctx: &NativeExecContext, t: &Address, m: MarketId) -> FixedPoint {
    match ctx.positions.get_position(t, m).unwrap() {
        Some(p) if p.is_long => p.size,
        Some(p) => -p.size,
        None => FixedPoint::ZERO,
    }
}

/// Remaining quantities of `t`'s resting orders in market `m`.
fn resting_in(ctx: &NativeExecContext, t: &Address, m: MarketId) -> Vec<FixedPoint> {
    ctx.order_books
        .get(&m)
        .map(|b| b.orders_for_trader(t).iter().map(|o| o.remaining_qty).collect())
        .unwrap_or_default()
}

fn abs(x: FixedPoint) -> FixedPoint {
    if x < FixedPoint::ZERO {
        -x
    } else {
        x
    }
}

fn raw(x: FixedPoint) -> U256 {
    U256::from(x.raw() as u128)
}

fn to_spot(t: Address, a: FixedPoint) -> (Address, NativeAction) {
    (t, NativeAction::TransferToSpot { amount: raw(a) })
}

fn withdraw_to(t: Address, a: FixedPoint) -> (Address, NativeAction) {
    (t, NativeAction::Withdraw { amount: raw(a), to: addr(77) })
}

fn tiered(ctx: &mut NativeExecContext, m: MarketId, tiers: Vec<MarginTier>) {
    let mut c = MarketMarginConfig::new(m, 999);
    c.tiers = tiers;
    ctx.margin_configs.insert(m, c);
}

/// `t` long 10 @100 in market 1 (20x: IM 50, notional 1,000) with `avail`
/// available and no order margin; counterparty addr(3).
fn long_10(ctx: &mut NativeExecContext, path: Path, t: Address, avail: FixedPoint) {
    fund_native(ctx, &addr(3), fp(FUNDING));
    let r = run(ctx, path, &[place(addr(3), limit(1, false, 100, 10))]);
    assert!(r[0].success, "{path:?}: {:?}", r[0].error);
    fund_native(ctx, &t, avail);
    let r = run(ctx, path, &[place(t, limit(1, true, 100, 10))]);
    assert!(r[0].success, "{path:?}: {:?}", r[0].error);
    assert_eq!(pos_in(ctx, &t, 1), fp(10), "{path:?}");
    assert_bal(ctx, &t, avail, FixedPoint::ZERO, "after open");
}

/// One `#[test]` per PlaceOrder path: `<case>::single|batch|parallel`.
macro_rules! per_path {
    ($case:ident) => {
        mod $case {
            use super::*;
            #[test]
            fn single() {
                super::$case(Path::Single)
            }
            #[test]
            fn batch() {
                super::$case(Path::Batch)
            }
            #[test]
            fn parallel() {
                super::$case(Path::Parallel)
            }
        }
    };
}

// ============================================================================
// Withdrawals (s517 decision 5, D3 = SAFE variant)
// ============================================================================

/// F1 (s517): long 10 @100 on 150: equity 150, required max(IM 50, 10% ×
/// 1,000) = 100. 50 leaves exactly 100 (ok); 51 is rejected (was: allowed,
/// only `available >= amount` was checked). TransferToSpot and Withdraw{to}.
fn withdraw_keeps_the_transfer_margin(path: Path) {
    for (amount, ok) in [(50, true), (51, false)] {
        for to_evm in [false, true] {
            let t = addr(2);
            let (_d, mut ctx) = fresh(path, &[]);
            long_10(&mut ctx, path, t, fp(150));
            let a = if to_evm { withdraw_to(t, fp(amount)) } else { to_spot(t, fp(amount)) };
            let r = run(&mut ctx, path, &[a]);
            let what = format!("{path:?} to_evm={to_evm} amount={amount}");
            assert_eq!(r[0].success, ok, "{what}: {:?}", r[0].error);
            if !ok {
                assert!(r[0].error.as_deref().unwrap_or("").contains("under-margined"), "{what}: {:?}", r[0].error);
            }
            assert_bal(&ctx, &t, fp(if ok { 150 - amount } else { 150 }), FixedPoint::ZERO, &what);
        }
    }
}
per_path!(withdraw_keeps_the_transfer_margin);

/// F1 (s517): at 5x the position IM (200) dominates the 10% floor (100):
/// 250 → 50 ok, 51 not.
fn withdraw_position_im_dominates_at_low_leverage(path: Path) {
    for (amount, ok) in [(50, true), (51, false)] {
        let t = addr(2);
        let (_d, mut ctx) = fresh(path, &[]);
        tiered(&mut ctx, 1, vec![MarginTier { max_notional: FixedPoint::MAX, max_leverage: 5 }]);
        long_10(&mut ctx, path, t, fp(250));
        let r = run(&mut ctx, path, &[to_spot(t, fp(amount))]);
        assert_eq!(r[0].success, ok, "{path:?} {amount}: {:?}", r[0].error);
    }
}
per_path!(withdraw_position_im_dominates_at_low_leverage);

/// F1 (s517): UPnL counts toward equity but never makes more than
/// `available` withdrawable. Mark 120: equity 150 + 200, required max(60, 120).
fn withdraw_counts_upnl_but_not_beyond_available(path: Path) {
    for (amount, ok) in [(150, true), (151, false)] {
        let t = addr(2);
        let (_d, mut ctx) = fresh(path, &[]);
        long_10(&mut ctx, path, t, fp(150));
        set_mark(&ctx, 1, fp(120));
        let r = run(&mut ctx, path, &[to_spot(t, fp(amount))]);
        assert_eq!(r[0].success, ok, "{path:?} {amount}: {:?}", r[0].error);
    }
}
per_path!(withdraw_counts_upnl_but_not_beyond_available);

/// F1 (s517) regression: flat accounts withdraw everything.
fn withdraw_flat_account_everything(path: Path) {
    let t = addr(2);
    let (_d, mut ctx) = fresh(path, &[t]);
    let r = run(&mut ctx, path, &[to_spot(t, fp(FUNDING))]);
    assert!(r[0].success, "{path:?}: {:?}", r[0].error);
    assert_bal(&ctx, &t, FixedPoint::ZERO, FixedPoint::ZERO, "flat");
}
per_path!(withdraw_flat_account_everything);

/// F1 (s517 D3, SAFE variant): 5x everywhere. Long 5 @100 in m1 (IM 100,
/// 10% floor 50), a resting bid 5 @100 in m2 (reservation 100), 100
/// available: withdrawing 100 would leave only the reservation behind the
/// position → REJECTED (the unsafe variant, equity − amount = 100 >= 100,
/// allowed it; if the bid then filled, IM 200 would sit on 100).
fn withdraw_cannot_use_resting_reservations_as_collateral(path: Path) {
    let t = addr(2);
    let (_d, mut ctx) = fresh(path, &[addr(3)]);
    let five_x = vec![MarginTier { max_notional: FixedPoint::MAX, max_leverage: 5 }];
    tiered(&mut ctx, 1, five_x.clone());
    tiered(&mut ctx, 2, five_x);
    fund_native(&ctx, &t, fp(200));
    run(&mut ctx, path, &[place(addr(3), limit(1, false, 100, 5))]);
    assert!(run(&mut ctx, path, &[place(t, limit(1, true, 100, 5))])[0].success, "{path:?}");
    assert!(run(&mut ctx, path, &[place(t, limit(2, true, 100, 5))])[0].success, "{path:?}");
    assert_bal(&ctx, &t, fp(100), fp(100), "position + resting bid");
    for amount in [100, 1] {
        let r = run(&mut ctx, path, &[to_spot(t, fp(amount))]);
        assert!(!r[0].success, "{path:?} amount={amount}: must be rejected");
        assert!(r[0].error.as_deref().unwrap_or("").contains("under-margined"), "{path:?}: {:?}", r[0].error);
    }
    assert_bal(&ctx, &t, fp(100), fp(100), "unchanged");
}
per_path!(withdraw_cannot_use_resting_reservations_as_collateral);

// ============================================================================
// Placement: the account check is the ONLY gate (D1, strict HL)
// ============================================================================

/// F1 repro (design doc): 100 at 20x, market sell 20 @100 (IM 100) twice in
/// successive blocks. Per-order margin gave the second the same 100 again
/// (~40x). Account-level: the second is rejected; still short 20.
fn forty_x_sequential(path: Path) {
    let (maker, t) = (addr(1), addr(2));
    let (_d, mut ctx) = fresh(path, &[maker]);
    fund_native(&ctx, &t, fp(100));
    run(&mut ctx, path, &[place(maker, limit(1, true, 100, 40))]);
    let sell = market(1, false, fp(1), 20);
    let r = run(&mut ctx, path, &[place(t, sell.clone())]);
    assert!(r[0].success, "{path:?}: {:?}", r[0].error);
    assert_eq!(pos_in(&ctx, &t, 1), -fp(20), "{path:?}: first opens 20x");
    let r = run(&mut ctx, path, &[place(t, sell)]);
    assert!(!r[0].success, "{path:?}: second must be rejected");
    assert!(r[0].error.as_deref().unwrap_or("").starts_with("insufficient margin"), "{path:?}: {:?}", r[0].error);
    assert_eq!(pos_in(&ctx, &t, 1), -fp(20), "{path:?}");
    assert_bal(&ctx, &t, fp(100), FixedPoint::ZERO, "no reservation left");
}
per_path!(forty_x_sequential);

/// F1: the same two sells in ONE run (single path: two blocks; batch: one
/// batch — sell A takes 19 on the exclusive pool, sell B the last 1). Never
/// past 20.
fn forty_x_one_batch(path: Path) {
    let (maker, t) = (addr(1), addr(2));
    let (_d, mut ctx) = fresh(path, &[maker]);
    fund_native(&ctx, &t, fp(100));
    run(&mut ctx, path, &[place(maker, limit(1, true, 100, 40))]);
    let sell = market(1, false, fp(1), 20);
    run(&mut ctx, path, &[place(t, sell.clone()), place(t, sell)]);
    assert_eq!(pos_in(&ctx, &t, 1), -fp(20), "{path:?}: at most 20x");
    assert_bal(&ctx, &t, fp(100), FixedPoint::ZERO, "released");
}
per_path!(forty_x_one_batch);

/// F1: two checked market sells of one sender in two markets in one run
/// cannot both spend the same 100 (per-order margin opened 39-40 on 100).
fn cross_market_cannot_double_spend(path: Path) {
    let (m1, m2, t) = (addr(1), addr(3), addr(2));
    let (_d, mut ctx) = fresh(path, &[m1, m2]);
    fund_native(&ctx, &t, fp(100));
    run(&mut ctx, path, &[place(m1, limit(1, true, 100, 20)), place(m2, limit(2, true, 100, 20))]);
    run(&mut ctx, path, &[place(t, market(1, false, fp(1), 20)), place(t, market(2, false, fp(1), 20))]);
    let open = abs(pos_in(&ctx, &t, 1)) + abs(pos_in(&ctx, &t, 2));
    assert!(open <= fp(20), "{path:?}: opened {open} on 100 at 20x");
    assert_bal(&ctx, &t, fp(100), FixedPoint::ZERO, "released");
}
per_path!(cross_market_cannot_double_spend);

fn tiers_20_then_5() -> Vec<MarginTier> {
    vec![
        MarginTier { max_notional: fp(1_000), max_leverage: 20 },
        MarginTier { max_notional: FixedPoint::MAX, max_leverage: 5 },
    ]
}

/// F1: tiers <= 1,000 at 20x, above 5x; 200 funded; GTC buys of 5 @100 (IM
/// 25 each at its own tier). The 3rd makes the POSITION 1,500 (IM 300): +250
/// > free 150 → rejected. One block per order.
fn position_tier_per_block(path: Path) {
    let (maker, t) = (addr(1), addr(2));
    let (_d, mut ctx) = fresh(path, &[maker]);
    tiered(&mut ctx, 1, tiers_20_then_5());
    fund_native(&ctx, &t, fp(200));
    run(&mut ctx, path, &[place(maker, limit(1, false, 100, 20))]);
    let mut ok = Vec::new();
    for _ in 0..3 {
        ok.push(run(&mut ctx, path, &[place(t, limit(1, true, 100, 5))])[0].success);
    }
    assert_eq!(ok, vec![true, true, false], "{path:?}");
    assert_eq!(pos_in(&ctx, &t, 1), fp(10), "{path:?}");
}
per_path!(position_tier_per_block);

/// F1: the same three buys in ONE run (batch: the Phase-2 projection
/// charges the 3rd at the projected position's tier).
fn position_tier_one_run(path: Path) {
    let (maker, t) = (addr(1), addr(2));
    let (_d, mut ctx) = fresh(path, &[maker]);
    tiered(&mut ctx, 1, tiers_20_then_5());
    fund_native(&ctx, &t, fp(200));
    run(&mut ctx, path, &[place(maker, limit(1, false, 100, 20))]);
    let buy = limit(1, true, 100, 5);
    let r = run(&mut ctx, path, &[place(t, buy.clone()), place(t, buy.clone()), place(t, buy)]);
    let ok: Vec<bool> = r.iter().map(|x| x.success).collect();
    assert_eq!(ok, vec![true, true, false], "{path:?}: {r:?}");
    assert_eq!(pos_in(&ctx, &t, 1), fp(10), "{path:?}");
}
per_path!(position_tier_one_run);

/// F1: funded 100, long 10 @100 in m1 (IM 50). GTC buy `qty` @100 in m2.
/// mark 110: UPnL +100, IM 55 → free 145 ≥ 100 (qty 20) ✓;
/// mark 100: free 50 < 100 ✗, qty 10 (50) ✓; mark 95: free 2.5 < 5 (qty 1) ✗.
fn upnl_counts(path: Path) {
    for (mark, qty, ok) in [(110, 20, true), (100, 20, false), (100, 10, true), (95, 1, false)] {
        let t = addr(2);
        let (_d, mut ctx) = fresh(path, &[addr(4)]);
        long_10(&mut ctx, path, t, fp(100));
        set_mark(&ctx, 1, fp(mark));
        run(&mut ctx, path, &[place(addr(4), limit(2, false, 100, qty))]);
        let r = run(&mut ctx, path, &[place(t, limit(2, true, 100, qty))]);
        assert_eq!(r[0].success, ok, "{path:?} mark={mark} qty={qty}: {:?}", r[0].error);
    }
}
per_path!(upnl_counts);

/// F1 (s517 #2): no oracle — the position is valued at ENTRY (UPnL 0), not
/// at the last trade: long 10 @100 on 100 → free 50 even after others trade
/// at 120.
fn no_mark_values_at_entry(path: Path) {
    for (qty, ok) in [(10, true), (11, false)] {
        let t = addr(2);
        let (_d, mut ctx) = fresh(path, &[addr(4), addr(5), addr(6)]);
        long_10(&mut ctx, path, t, fp(100));
        run(&mut ctx, path, &[place(addr(5), limit(1, false, 120, 1))]);
        run(&mut ctx, path, &[place(addr(6), limit(1, true, 120, 1))]);
        run(&mut ctx, path, &[place(addr(4), limit(2, false, 100, qty))]);
        let r = run(&mut ctx, path, &[place(t, limit(2, true, 100, qty))]);
        assert_eq!(r[0].success, ok, "{path:?} qty={qty}: {:?}", r[0].error);
    }
}
per_path!(no_mark_values_at_entry);

/// s517 D1 (STRICT HL): unrealized profit funds a reservation beyond cash.
/// Funded 100, long 10 @100 in m1; mark 150: UPnL +500, IM 75 → free 525.
/// A resting GTC bid 60 @100 in m2 reserves 300 > available 100: ACCEPTED,
/// available goes to −200 (the old `available >= reservation` gate rejected
/// it). Without a mark (entry fallback, UPnL 0): free 50 < 300 → rejected.
fn upnl_funds_a_reservation_beyond_cash(path: Path) {
    for with_mark in [true, false] {
        let t = addr(2);
        let (_d, mut ctx) = fresh(path, &[]);
        long_10(&mut ctx, path, t, fp(100));
        if with_mark {
            set_mark(&ctx, 1, fp(150));
        }
        let r = run(&mut ctx, path, &[place(t, limit(2, true, 100, 60))]);
        let what = format!("{path:?} mark={with_mark}");
        assert_eq!(r[0].success, with_mark, "{what}: {:?}", r[0].error);
        if with_mark {
            assert_bal(&ctx, &t, -fp(200), fp(300), &what);
            assert_eq!(resting_in(&ctx, &t, 2), vec![fp(60)], "{what}");
        } else {
            assert!(r[0].error.as_deref().unwrap_or("").starts_with("insufficient margin"), "{what}");
            assert_bal(&ctx, &t, fp(100), FixedPoint::ZERO, &what);
        }
    }
}
per_path!(upnl_funds_a_reservation_beyond_cash);

/// s517 D1: a negative-cash account withdraws nothing (cash bound `amount
/// <= available`, lockbox) on either withdrawal action; closing the
/// profitable position realizes the UPnL and brings `available` back >= 0
/// with collateral conserved: −200 − 75 (the closing sell's reservation) +
/// 500 (PnL 10 × (150 − 100)) + 75 (released) = 300, order margin still
/// 300 (the m2 bid), total 600 = 100 funded + 500.
fn negative_cash_cannot_withdraw_and_closing_restores_it(path: Path) {
    let (t, bidder) = (addr(2), addr(5));
    let (_d, mut ctx) = fresh(path, &[bidder]);
    long_10(&mut ctx, path, t, fp(100));
    set_mark(&ctx, 1, fp(150));
    assert!(run(&mut ctx, path, &[place(t, limit(2, true, 100, 60))])[0].success, "{path:?}");
    for a in [to_spot(t, fp(1)), withdraw_to(t, fp(1))] {
        let r = run(&mut ctx, path, &[a]);
        assert!(!r[0].success, "{path:?}: negative cash must not withdraw");
    }
    assert_bal(&ctx, &t, -fp(200), fp(300), "unchanged");
    run(&mut ctx, path, &[place(bidder, limit(1, true, 150, 10))]);
    let r = run(&mut ctx, path, &[place(t, limit(1, false, 150, 10))]);
    assert!(r[0].success, "{path:?}: closing is always allowed: {:?}", r[0].error);
    assert_eq!(pos_in(&ctx, &t, 1), FixedPoint::ZERO, "{path:?}");
    assert_bal(&ctx, &t, fp(300), fp(300), "PnL realized, bid still reserved");
}
per_path!(negative_cash_cannot_withdraw_and_closing_restores_it);

// ============================================================================
// Decision s517 (T7): Phase 2 credits the IM that the sender's EARLIER
// orders of the batch are projected to release ONLY to match-checked orders
// ============================================================================

/// `t` long 20 @100 in market 1 (20x: IM 100) with 200 available and no
/// order margin; counterparty addr(3), whose ask is fully consumed.
fn long_20_on_200(ctx: &mut NativeExecContext, path: Path, t: Address) {
    fund_native(ctx, &addr(3), fp(FUNDING));
    assert!(run(ctx, path, &[place(addr(3), limit(1, false, 100, 20))])[0].success, "{path:?}");
    fund_native(ctx, &t, fp(200));
    assert!(run(ctx, path, &[place(t, limit(1, true, 100, 20))])[0].success, "{path:?}");
    assert_eq!(pos_in(ctx, &t, 1), fp(20), "{path:?}");
    assert_bal(ctx, &t, fp(200), FixedPoint::ZERO, "after open");
}

/// Decision s517 (strict side): long 20 @100 (IM 100), 200 available. One
/// run = [GTC sell 20 @110 that RESTS (no bids; reserves 110 → available
/// 90), GTC buy 10 @100 in m2 (need IM 50)]. The buy is unchecked at match,
/// so it gets NO credit for the sell's projected release: free = 90 − 100 =
/// −10 < 50 → rejected (with the credit, 90 + 0 ≥ 50 would have passed —
/// and the sell released nothing). Same on every path.
fn unchecked_order_gets_no_projected_release_credit(path: Path) {
    let t = addr(2);
    let (_d, mut ctx) = fresh(path, &[]);
    long_20_on_200(&mut ctx, path, t);
    let r = run(&mut ctx, path, &[place(t, limit(1, false, 110, 20)), place(t, limit(2, true, 100, 10))]);
    assert!(r[0].success, "{path:?}: {:?}", r[0].error);
    assert!(!r[1].success, "{path:?}: the GTC buy must be rejected");
    assert!(r[1].error.as_deref().unwrap_or("").starts_with("insufficient margin"), "{path:?}: {:?}", r[1].error);
    assert_eq!(resting_in(&ctx, &t, 1), vec![fp(20)], "{path:?}: the sell rests");
    assert!(resting_in(&ctx, &t, 2).is_empty(), "{path:?}");
    assert_eq!(pos_in(&ctx, &t, 1), fp(20), "{path:?}");
    assert_bal(&ctx, &t, fp(90), fp(110), &format!("{path:?}"));
}
per_path!(unchecked_order_gets_no_projected_release_credit);

/// Decision s517 (checked side): same long, maker addr(4) asks 5 @100 in
/// m1. One run = [GTC sell 20 @110 that rests (available 90), market BUY 1
/// cap 100 (reserves 5; increases the long)]. Batch paths: the market buy
/// is match-checked, so Phase 2 credits the sell's projected release (100)
/// and ACCEPTS it (need 5 on the projected flat position <= 90 − 100 + 100
/// = 90). At match the real position is still long 20: need IM(2,100) −
/// IM(2,000) = 5 > budget 5 + pool (85 − 100 = −15) = −10 → nothing fills,
/// the reservation comes back. Single path: the sell rests first, the buy
/// sees the real long (need 5 > free −10) and is rejected at placement.
/// Either way: still long 20, maker's ask untouched, 90 / 110. Row 50:
/// either way the buy is rejected for margin (batch paths: at match, with
/// the executed order's gas; single path: at placement, no gas).
fn checked_order_credit_is_bounded_at_match(path: Path) {
    let (t, mk) = (addr(2), addr(4));
    let (_d, mut ctx) = fresh(path, &[mk]);
    long_20_on_200(&mut ctx, path, t);
    assert!(run(&mut ctx, path, &[place(mk, limit(1, false, 100, 5))])[0].success, "{path:?}");
    let r = run(&mut ctx, path, &[place(t, limit(1, false, 110, 20)), place(t, market(1, true, fp(100), 1))]);
    assert!(r[0].success, "{path:?}: {:?}", r[0].error);
    let accepted = !matches!(path, Path::Single);
    assert!(!r[1].success && r[1].reason == torus_state::action_status::FailureReason::Margin, "{path:?}: {:?}", r[1]);
    assert_eq!(r[1].gas_used, if accepted { 1000 } else { 0 }, "{path:?}: accepted at placement?");
    assert_eq!(pos_in(&ctx, &t, 1), fp(20), "{path:?}: nothing released, nothing opened");
    assert_eq!(resting_in(&ctx, &mk, 1), vec![fp(5)], "{path:?}: maker's ask untouched");
    assert_eq!(resting_in(&ctx, &t, 1), vec![fp(20)], "{path:?}: the sell rests");
    assert_bal(&ctx, &t, fp(90), fp(110), &format!("{path:?}"));
}
per_path!(checked_order_credit_is_bounded_at_match);

// ============================================================================
// Makers: HL `marginCanceled` (s517 decision 4)
// ============================================================================

/// F1 (s517 #4, HL marginCanceled): M (100) rests bid 10 @100 in m1 (50
/// reserved), then opens long 10 @100 in m2 (IM 50). Mark m2 at 90: UPnL
/// −100, IM 45 → free −95. T's market sell 10 in m1 cancels M's bid (its
/// reservation comes back) and fills M2's bid @99. At mark 100 (free 0) M's
/// bid fills (IM 50 == its share).
fn maker_margin_cancel(path: Path) {
    for (mark, cancelled) in [(90, true), (100, false)] {
        let (m, cp, m2, t) = (addr(1), addr(3), addr(4), addr(2));
        let (_d, mut ctx) = fresh(path, &[cp, m2, t]);
        fund_native(&ctx, &m, fp(100));
        assert!(run(&mut ctx, path, &[place(m, limit(1, true, 100, 10))])[0].success);
        run(&mut ctx, path, &[place(cp, limit(2, false, 100, 10))]);
        assert!(run(&mut ctx, path, &[place(m, limit(2, true, 100, 10))])[0].success);
        assert_eq!(pos_in(&ctx, &m, 2), fp(10));
        set_mark(&ctx, 2, fp(mark));
        run(&mut ctx, path, &[place(m2, limit(1, true, 99, 10))]);
        let metrics = metered(&mut ctx);
        let r = run(&mut ctx, path, &[place(t, market(1, false, fp(1), 10))]);
        let what = format!("{path:?} mark={mark}");
        assert!(r[0].success, "{what}: {:?}", r[0].error);
        assert_eq!(metrics.maker_margin_cancels.get(), u64::from(cancelled), "{what}: s92 counter");
        assert!(sell_cuts(&metrics).is_empty(), "{what}: the taker fitted");
        assert_eq!(pos_in(&ctx, &t, 1), -fp(10), "{what}");
        assert!(resting_in(&ctx, &m, 1).is_empty(), "{what}: M's bid gone either way");
        assert_eq!(pos_in(&ctx, &m, 1), if cancelled { FixedPoint::ZERO } else { fp(10) }, "{what}");
        assert_eq!(pos_in(&ctx, &m2, 1), if cancelled { fp(10) } else { FixedPoint::ZERO }, "{what}");
        assert_bal(&ctx, &m, fp(100), FixedPoint::ZERO, &format!("{what}: reservation released"));
    }
}
per_path!(maker_margin_cancel);

// ============================================================================
// Review fixes s517
// ============================================================================

/// Review fix 1 (s517): Phase 2 admits an order by its POSITION-tier need
/// but debits only the order-tier reservation; the part of the need beyond
/// it must stay committed for the sender's later orders of the batch.
/// Tiers <= 1,000 at 20x, above 5x; mark 100; long 10 (IM 50) on 300 →
/// free 250. One run of two GTC buys 5 @100 crossing asks: #1 needs
/// IM(1,500) − IM(1,000) = 250 <= 250 (reserves 25, commits 225 more); #2
/// needs IM(2,000) − IM(1,500) = 100 > 0 left → REJECTED on every path
/// (was: 100 <= 275 − 50 → long 20, IM 400 on equity 300).
fn batch_commits_the_position_tier_need(path: Path) {
    let t = addr(2);
    let (_d, mut ctx) = fresh(path, &[addr(3)]);
    tiered(&mut ctx, 1, tiers_20_then_5());
    run(&mut ctx, path, &[place(addr(3), limit(1, false, 100, 20))]);
    fund_native(&ctx, &t, fp(FUNDING));
    assert!(run(&mut ctx, path, &[place(t, limit(1, true, 100, 10))])[0].success, "{path:?}");
    fund_native(&ctx, &t, fp(300));
    set_mark(&ctx, 1, fp(100));
    let buy = limit(1, true, 100, 5);
    let r = run(&mut ctx, path, &[place(t, buy.clone()), place(t, buy)]);
    let ok: Vec<bool> = r.iter().map(|x| x.success).collect();
    assert_eq!(ok, vec![true, false], "{path:?}: {r:?}");
    assert!(r[1].error.as_deref().unwrap_or("").starts_with("insufficient margin"), "{path:?}");
    assert_eq!(pos_in(&ctx, &t, 1), fp(15), "{path:?}");
    assert_bal(&ctx, &t, fp(300), FixedPoint::ZERO, &format!("{path:?}"));
}
per_path!(batch_commits_the_position_tier_need);

/// Review fix 2 (s517): a sender's D2 taker pool (0 outside the market of
/// its first checked taker) is NOT its maker free margin. M (1,000) long 5
/// @100 in m2 (tiers 20x <= 1,000, 5x above) rests bid 10 @100 there. One
/// run: M market-sells 1 in m1 (its first checked taker → m1 gets the
/// pool) and 1 in m2 (closing, fills addr 4's bid @101); then X market-
/// sells 10 in m2 into M's bid. M's fill costs IM(1,400) − IM(400) − its
/// share IM(1,000) = 260 − 50 = 210 <= its snapshot free (~925) → FILLS
/// (was: checked against the m2 pool 0 → marginCanceled).
fn maker_uses_its_snapshot_not_the_taker_pool(path: Path) {
    let (m, cp, b, x) = (addr(1), addr(3), addr(4), addr(6));
    let (_d, mut ctx) = fresh(path, &[m, cp, b, x]);
    tiered(&mut ctx, 2, tiers_20_then_5());
    run(&mut ctx, path, &[place(cp, limit(2, false, 100, 5))]);
    assert!(run(&mut ctx, path, &[place(m, limit(2, true, 100, 5))])[0].success, "{path:?}");
    assert!(run(&mut ctx, path, &[place(m, limit(2, true, 100, 10))])[0].success, "{path:?}");
    run(&mut ctx, path, &[place(b, limit(1, true, 100, 1)), place(b, limit(2, true, 101, 1))]);
    let r = run(
        &mut ctx,
        path,
        &[
            place(m, market(1, false, fp(1), 1)),
            place(m, market(2, false, fp(1), 1)),
            place(x, market(2, false, fp(1), 10)),
        ],
    );
    assert!(r.iter().all(|x| x.success), "{path:?}: {r:?}");
    assert_eq!(pos_in(&ctx, &x, 2), -fp(10), "{path:?}: X filled against M's bid");
    assert_eq!(pos_in(&ctx, &m, 2), fp(14), "{path:?}: 5 − 1 + 10");
    assert!(resting_in(&ctx, &m, 2).is_empty(), "{path:?}");
}
per_path!(maker_uses_its_snapshot_not_the_taker_pool);

/// Review fix 4 (s517): in the book holding a sender's D2 taker pool, its
/// resting makers are checked against that SAME running entry (not the
/// pre-batch snapshot). m1 tiers 20x <= 1,000, 5x above. S (242.5) rests
/// bid 5 @90 (22.5) → pre-batch free 220. One run: S market-buys 11 cap 100
/// into asks @100 (need IM(1,100) = 220 = its whole pool → running free 0),
/// then T market-sells 5 into S's bid: S's cost IM(990 + 450) − IM(990) −
/// share 22.5 = 216 > 0 → S's bid is margin-cancelled (was: 216 <= the
/// snapshot 220 → filled, long 16 on 242.5). Single path: after the buy S
/// is long 11 at entry 100 (free 0), cost 67.5 → cancelled too. Every path:
/// S long 11, T flat, S's bid gone, all of S's reservations released.
fn maker_in_its_pool_market_shares_the_pool(path: Path) {
    let (s, cp, t) = (addr(1), addr(3), addr(2));
    let (_d, mut ctx) = fresh(path, &[cp, t]);
    tiered(&mut ctx, 1, tiers_20_then_5());
    fund_native(&ctx, &s, fp_cents(24_250));
    assert!(run(&mut ctx, path, &[place(s, limit(1, true, 90, 5))])[0].success, "{path:?}");
    run(&mut ctx, path, &[place(cp, limit(1, false, 100, 11))]);
    assert_bal(&ctx, &s, fp(220), fp_cents(2_250), "setup");
    let r = run(&mut ctx, path, &[place(s, market(1, true, fp(100), 11)), place(t, market(1, false, fp(1), 5))]);
    assert!(r[0].success, "{path:?}: {:?}", r[0].error);
    assert_eq!(pos_in(&ctx, &s, 1), fp(11), "{path:?}: the bid must not fill");
    assert_eq!(pos_in(&ctx, &t, 1), FixedPoint::ZERO, "{path:?}");
    assert!(resting_in(&ctx, &s, 1).is_empty(), "{path:?}: bid margin-cancelled");
    assert_bal(&ctx, &s, fp_cents(24_250), FixedPoint::ZERO, &format!("{path:?}"));
}
per_path!(maker_in_its_pool_market_shares_the_pool);

// ============================================================================
// Option B (s87): outside its D2 pool market a batch sell reserves for the
// best bid of the start of Phase 2 (`max(limit | mark-or-cap, best bid)`);
// the resting row and every release stay at the limit. B2: a taker-only
// budget gets the makers' +1 raw rounding allowance.
// ============================================================================

/// `ctx` with funnel metrics on (`orders_rejected_cancelled` etc.).
fn metered(ctx: &mut NativeExecContext) -> std::sync::Arc<torus_telemetry::Metrics> {
    let m = std::sync::Arc::new(torus_telemetry::Metrics::new());
    ctx.metrics = Some(m.clone());
    m
}

fn with_tif(mut p: PlaceOrderParams, tif: TimeInForce) -> PlaceOrderParams {
    p.time_in_force = tif;
    p
}

/// T's first checked order of the batch (so m1 is its D2 pool market): a
/// GTC sell 1 @200 in m1, where nothing bids — it rests and keeps 10.
fn pool_order() -> PlaceOrderParams {
    limit(1, false, 200, 1)
}

fn is_margin_reject(r: &NativeActionResult) -> bool {
    !r.success && r.error.as_deref().unwrap_or("").starts_with("insufficient margin")
}

/// Option B (bench repro, design §7.1 #1). M bids 10 @101 in m1 and m2; T
/// (101) sells 10 @100 GTC in both in one run. Single path: both fill at 101
/// (after the first, free 50.5 >= need 50; match need 50.5). Batch before B:
/// m2 is taker-only with budget 50 < 50.5 → 0 fills, Cancelled. B: m2
/// reserves 101 × 10 / 20 = 50.5 and fills 10; the pool (m1) keeps 0.5,
/// exactly what the m1 sell needs beyond its own 50.
/// B-blind (s92): the m2 sell is also topped up to reserve(101.101, 10) =
/// 50.5505, AHEAD of the pool (by design): at 101 the pool keeps 0.4495 and
/// the m1 sell fills 8 (IM(1,000 + q) <= 50.4495); funded 101.0505 both fill
/// 10 again. The single path is unchanged (no top-up).
fn non_pool_limit_sell_fills_at_a_better_bid(path: Path) {
    for (funded, m1) in [(fp(101), -fp(8)), (fp_e4(1_010_505), -fp(10))] {
        let (mk, t) = (addr(1), addr(2));
        let (_d, mut ctx) = fresh(path, &[mk]);
        let metrics = metered(&mut ctx);
        run(&mut ctx, path, &[place(mk, limit(1, true, 101, 10)), place(mk, limit(2, true, 101, 10))]);
        fund_native(&ctx, &t, funded);
        let r = run(&mut ctx, path, &[place(t, limit(1, false, 100, 10)), place(t, limit(2, false, 100, 10))]);
        let what = format!("{path:?} funded {funded}");
        let m1 = if matches!(path, Path::Single) { -fp(10) } else { m1 };
        assert!(r.iter().all(|x| x.success), "{what}: {r:?}");
        assert_eq!((pos_in(&ctx, &t, 1), pos_in(&ctx, &t, 2)), (m1, -fp(10)), "{what}");
        assert!(resting_in(&ctx, &t, 1).is_empty() && resting_in(&ctx, &t, 2).is_empty(), "{what}");
        assert_bal(&ctx, &t, funded, FixedPoint::ZERO, &what);
        assert_eq!(metrics.orders_rejected_cancelled.get(), 0, "{what}");
    }
}
per_path!(non_pool_limit_sell_fills_at_a_better_bid);

/// Option B: the same shape with an IOC and a FOK sell in m2 (never rest:
/// reserve the opening quantity). Before B the IOC filled 9 (lot 1: 45.45 <=
/// 50) and the FOK was rejected whole; with B both fill 10 like the single path.
/// B-blind (s92): funded 101.0505 (+ the m2 top-up to reserve(101.101, 10),
/// taken ahead of the pool; at 101 the m1 sell would fill 8).
fn non_pool_ioc_and_fok_sell_fill_at_a_better_bid(path: Path) {
    for tif in [TimeInForce::IOC, TimeInForce::FOK] {
        let (mk, t) = (addr(1), addr(2));
        let (_d, mut ctx) = fresh(path, &[mk]);
        run(&mut ctx, path, &[place(mk, limit(1, true, 101, 10)), place(mk, limit(2, true, 101, 10))]);
        fund_native(&ctx, &t, fp_e4(1_010_505));
        let r = run(
            &mut ctx,
            path,
            &[place(t, limit(1, false, 100, 10)), place(t, with_tif(limit(2, false, 100, 10), tif))],
        );
        let what = format!("{path:?} {tif:?}");
        assert!(r.iter().all(|x| x.success), "{what}: {r:?}");
        assert_eq!((pos_in(&ctx, &t, 1), pos_in(&ctx, &t, 2)), (-fp(10), -fp(10)), "{what}");
        assert_bal(&ctx, &t, fp_e4(1_010_505), FixedPoint::ZERO, &what);
    }
}
per_path!(non_pool_ioc_and_fok_sell_fill_at_a_better_bid);

/// Option B for market sells: mark 100 in m2, M bids 10 @100 in m1 and 10
/// @110 in m2; T (105) market-sells 10 (cap 1) in m1 (its pool; no mark:
/// reserves at the cap, 0.5) and in m2. Before B the m2 sell reserved at
/// the mark (50) and its fills @110 (55) were cut after 9. B: it reserves
/// at max(mark, 110) = 55 and fills 10, exactly like the single path
/// (free after m1: 105 − 50 = 55). B-blind (s92): funded 105.055 (+ the m2
/// top-up to reserve(110.11, 10) = 55.055, taken ahead of the pool).
fn non_pool_market_sell_reserves_at_the_start_best_bid_above_the_mark(path: Path) {
    let (mk, t) = (addr(1), addr(2));
    let (_d, mut ctx) = fresh(path, &[mk]);
    set_mark(&ctx, 2, fp(100));
    run(&mut ctx, path, &[place(mk, limit(1, true, 100, 10)), place(mk, limit(2, true, 110, 10))]);
    fund_native(&ctx, &t, fp_e4(1_050_550));
    let r = run(&mut ctx, path, &[place(t, market(1, false, fp(1), 10)), place(t, market(2, false, fp(1), 10))]);
    assert!(r.iter().all(|x| x.success), "{path:?}: {r:?}");
    assert_eq!((pos_in(&ctx, &t, 1), pos_in(&ctx, &t, 2)), (-fp(10), -fp(10)), "{path:?}");
    assert_bal(&ctx, &t, fp_e4(1_050_550), FixedPoint::ZERO, &format!("{path:?}"));
}
per_path!(non_pool_market_sell_reserves_at_the_start_best_bid_above_the_mark);

/// Option B, partial fill: M bids 4 @101 in m2; T (200) = [pool order, GTC
/// sell 10 @100 in m2]. B reserves 50.5, fills 4 @101 (need IM(404 + 600)
/// = 50.2) and rests 6 @100 holding exactly reserve(100, 6) = 30 (release
/// 50.5 − 30). Before B: 0 fills, Cancelled. A cancel-all then returns
/// every reservation (no stranded margin).
fn non_pool_sell_partial_fill_rests_at_its_limit_with_the_exact_reservation(path: Path) {
    let (mk, t) = (addr(1), addr(2));
    let (_d, mut ctx) = fresh(path, &[mk]);
    run(&mut ctx, path, &[place(mk, limit(2, true, 101, 4))]);
    fund_native(&ctx, &t, fp(200));
    let r = run(&mut ctx, path, &[place(t, pool_order()), place(t, limit(2, false, 100, 10))]);
    assert!(r.iter().all(|x| x.success), "{path:?}: {r:?}");
    assert_eq!(pos_in(&ctx, &t, 2), -fp(4), "{path:?}");
    assert_eq!(resting_in(&ctx, &t, 2), vec![fp(6)], "{path:?}");
    assert_bal(&ctx, &t, fp(160), fp(40), &format!("{path:?}: 10 (m1) + reserve(100, 6)"));
    run(&mut ctx, path, &[(t, NativeAction::CancelAllOrders { market_id: None })]);
    assert_bal(&ctx, &t, fp(200), FixedPoint::ZERO, &format!("{path:?}: all released"));
}
per_path!(non_pool_sell_partial_fill_rests_at_its_limit_with_the_exact_reservation);

/// Option B design §7.2 #5: test 1 with T funded 100.25. Single path: after
/// the m1 fill free is 49.75 < 50 → the m2 sell is rejected. B: the m2 gate
/// needs 50.5 > 50.25 → rejected at placement too, and the m1 sell keeps
/// its pool (0.25 + 50 >= 50.5) and fills 10. Before B: m2 admitted, m1's
/// pool 0.25 → m1 filled only 5 and m2 nothing.
fn tight_non_pool_sell_is_rejected_at_placement_like_the_single_path(path: Path) {
    let (mk, t) = (addr(1), addr(2));
    let (_d, mut ctx) = fresh(path, &[mk]);
    run(&mut ctx, path, &[place(mk, limit(1, true, 101, 10)), place(mk, limit(2, true, 101, 10))]);
    fund_native(&ctx, &t, fp_cents(10_025));
    let r = run(&mut ctx, path, &[place(t, limit(1, false, 100, 10)), place(t, limit(2, false, 100, 10))]);
    assert!(r[0].success, "{path:?}: {:?}", r[0].error);
    assert!(is_margin_reject(&r[1]), "{path:?}: {:?}", r[1]);
    assert_eq!((pos_in(&ctx, &t, 1), pos_in(&ctx, &t, 2)), (-fp(10), FixedPoint::ZERO), "{path:?}");
    assert_bal(&ctx, &t, fp_cents(10_025), FixedPoint::ZERO, &format!("{path:?}"));
}
per_path!(tight_non_pool_sell_is_rejected_at_placement_like_the_single_path);

/// Option B §7.2 #6: m2 tiers 20x <= 1,000, 5x above. Flat T sells 9.9 @100
/// (990: 49.5 at 20x) into a bid @110 (1,089: 217.8 at 5x). B reserves
/// IM(1,089) and fills 9.9 (before B: 1 — IM(990 + 10 q) crosses the tier
/// for q > 1).
fn non_pool_sell_tier_crossing_at_the_better_bid_is_covered(path: Path) {
    let (mk, t) = (addr(1), addr(2));
    let (_d, mut ctx) = fresh(path, &[mk, t]);
    tiered(&mut ctx, 2, tiers_20_then_5());
    let q = fp_cents(990);
    run(&mut ctx, path, &[place(mk, PlaceOrderParams { quantity: q, ..limit(2, true, 110, 1) })]);
    let r = run(&mut ctx, path, &[place(t, pool_order()), place(t, PlaceOrderParams { quantity: q, ..limit(2, false, 100, 1) })]);
    assert!(r.iter().all(|x| x.success), "{path:?}: {r:?}");
    assert_eq!(pos_in(&ctx, &t, 2), -q, "{path:?}");
    assert_bal(&ctx, &t, fp(990), fp(10), &format!("{path:?}"));
}
per_path!(non_pool_sell_tier_crossing_at_the_better_bid_is_covered);

/// Option B §7.2 #7 (review 4 F-1 shapes, not gameable): bids placed
/// EARLIER IN THE BATCH never move a non-pool sell's placement-checked
/// reservation — only the start-of-batch best bid does (B-blind's δ
/// top-up, s92, also reads only that bid: +0.0505 here, after Phase 2,
/// never a gate). M bids 10 @101 in m2; T (61) = [pool order
/// (10), <X's high bid>, sell 10 @100 in m2]. reserve(101, 10) = 50.5 <= 51
/// is admitted and fills 10 @101; a bound at X's 120 (60 > 51) would reject
/// it. X's bid is: unfunded (rejected), IOC (nothing to hit, cancelled),
/// off-tick (book reject), or [deep low bid 100 @1, high 1 @1000] with
/// margin for the first only.
fn in_batch_high_bid_does_not_move_a_non_pool_sell_reservation(path: Path) {
    for shape in ["unfunded", "ioc", "off_tick", "deep_low_then_high"] {
        let (mk, t, x) = (addr(1), addr(2), addr(5));
        let (_d, mut ctx) = fresh(path, &[mk]);
        run(&mut ctx, path, &[place(mk, limit(2, true, 101, 10))]);
        fund_native(&ctx, &t, fp(61));
        let mut actions = vec![place(t, pool_order())];
        match shape {
            "unfunded" => actions.push(place(x, limit(2, true, 120, 10))),
            "ioc" => {
                fund_native(&ctx, &x, fp(FUNDING));
                actions.push(place(x, with_tif(limit(2, true, 120, 10), TimeInForce::IOC)));
            }
            "off_tick" => {
                fund_native(&ctx, &x, fp(FUNDING));
                actions.push(place(x, PlaceOrderParams { price: fp_cents(12_050), ..limit(2, true, 120, 10) }));
            }
            _ => {
                fund_native(&ctx, &x, fp(10));
                actions.push(place(x, limit(2, true, 1, 100)));
                actions.push(place(x, limit(2, true, 1_000, 1)));
            }
        }
        actions.push(place(t, limit(2, false, 100, 10)));
        let r = run(&mut ctx, path, &actions);
        let what = format!("{path:?} {shape}");
        assert!(r.last().unwrap().success, "{what}: {r:?}");
        assert_eq!(pos_in(&ctx, &t, 2), -fp(10), "{what}");
        assert_eq!(pos_in(&ctx, &x, 2), FixedPoint::ZERO, "{what}");
        assert_bal(&ctx, &t, fp(51), fp(10), &what);
    }
}
per_path!(in_batch_high_bid_does_not_move_a_non_pool_sell_reservation);

// ---- Same-batch bids under B-blind (s92; the s87 / s89 same-batch bound is gone) ----
// The s87 / s89 bound topped a non-pool sell up for the highest bid placed
// EARLIER IN THE SAME BATCH that would rest. B-blind (below) replaces it: the
// top-up is reserve(B0 x (1 + 10 bps), qty) from the start-of-Phase-2 best
// bid B0 only, so no other trader's order of the batch changes it. The
// shapes the bound was tested on are kept with their intent (no other
// trader raises a reservation or drains a pool) and re-pinned: with B0 =
// 101 the δ top-up is reserve(101.101, 10) − 50.5 = 0.0505 and covers bids
// up to 101.101 (lower holds at the limit stretch it further).

/// One `#[test]` per batch path (Batch, Parallel) — shapes where the single
/// path's account budget differs by design.
macro_rules! batch_paths {
    ($case:ident) => {
        mod $case {
            use super::*;
            #[test]
            fn batch() {
                super::$case(Path::Batch)
            }
            #[test]
            fn parallel() {
                super::$case(Path::Parallel)
            }
        }
    };
}

/// Was "fills at a funded same-batch bid" (s87 bound). M bids 10 @101 and
/// asks 10 @110 in m2; B's funded GTC bid 10 @105 sorts before T's non-pool
/// sell 10 @100. The bound topped the sell up to reserve(105, 10) = 52.5.
/// B-blind tops it up only to 50.5505 (B0 = 101): it fills 2 @105 (IM(105 q
/// + 100 (10 − q)) <= 50.5505) and is cut at 105, 4 ticks above its
/// reservation price 101.101 → `[non-pool][partial][3-5]`. The single path
/// (account budget) fills 10.
fn non_pool_sell_at_a_funded_same_batch_bid_beyond_delta_fills_partly(path: Path) {
    let (mk, t, b) = (addr(1), addr(2), addr(4));
    let (_d, mut ctx) = fresh(path, &[mk, t, b]);
    let metrics = metered(&mut ctx);
    run(&mut ctx, path, &[place(mk, limit(2, true, 101, 10)), place(mk, limit(2, false, 110, 10))]);
    let r = run(&mut ctx, path, &[place(t, pool_order()), place(b, limit(2, true, 105, 10)), place(t, limit(2, false, 100, 10))]);
    assert!(r.iter().all(|x| x.success), "{path:?}: {r:?}");
    assert_eq!(pos_in(&ctx, &t, 2), -fp(2), "{path:?}");
    assert_eq!(pos_in(&ctx, &b, 2), fp(2), "{path:?}: filled at B's bid");
    assert_eq!(resting_in(&ctx, &b, 2), vec![fp(8)], "{path:?}");
    assert_eq!(resting_in(&ctx, &mk, 2), vec![fp(10), fp(10)], "{path:?}: M untouched");
    assert_bal(&ctx, &t, fp(990), fp(10), &format!("{path:?}: top-up released"));
    assert_bal(&ctx, &b, fp(958), fp(42), &format!("{path:?}: reserve(105, 8)"));
    assert_eq!(sell_cuts(&metrics), vec![(1, 1, 2, 1)], "{path:?}");
    assert_eq!(top_ups(&metrics), [1, 0, 0], "{path:?}");
}
batch_paths!(non_pool_sell_at_a_funded_same_batch_bid_beyond_delta_fills_partly);

/// Was "counts only up to the start best ask" (s87 cap). M bids 10 @101 in
/// m1 and m2 and asks 10 @102 in m2; T (102) = [sell 10 @100 in m1 (pool;
/// reserves 50, needs 50.5 at 101), X's funded bid 11 @104 in m2 (eats the
/// ask 10 @102 and RESTS 1 @104), sell 10 @100 in m2 (50.5 at B0 = 101)].
/// B-blind ignores X's bid: top-up 0.0505, so the pool keeps 1.4495 and m1
/// fills 10; the m2 sell (budget 50.5505) fills 1 @104 + 7 @101 (IM(1,004 +
/// q) <= 50.5505) and is cut. The single path (account budget) fills both.
fn same_batch_bid_lifting_the_start_ask_does_not_top_up(path: Path) {
    let (mk, t, x) = (addr(1), addr(2), addr(5));
    let (_d, mut ctx) = fresh(path, &[mk, x]);
    run(&mut ctx, path, &[place(mk, limit(1, true, 101, 10)), place(mk, limit(2, true, 101, 10)), place(mk, limit(2, false, 102, 10))]);
    fund_native(&ctx, &t, fp(102));
    let r = run(&mut ctx, path, &[place(t, limit(1, false, 100, 10)), place(x, limit(2, true, 104, 11)), place(t, limit(2, false, 100, 10))]);
    let m2 = if matches!(path, Path::Single) { -fp(10) } else { -fp(8) };
    assert!(r.iter().all(|x| x.success), "{path:?}: {r:?}");
    assert_eq!((pos_in(&ctx, &t, 1), pos_in(&ctx, &t, 2)), (-fp(10), m2), "{path:?}");
    assert_eq!(pos_in(&ctx, &x, 2), fp(11), "{path:?}: X lifted the ask, then T hit its rest");
    assert_bal(&ctx, &t, fp(102), FixedPoint::ZERO, &format!("{path:?}"));
}
per_path!(same_batch_bid_lifting_the_start_ask_does_not_top_up);

/// s89 review shapes, unchanged intent: a bid that never rests changes
/// nothing. M bids 10 @101 in m1 and m2 and asks 10 @102 in m2; T (101.0505:
/// 101 + the δ top-up) = [sell 10 @100 in m1 (pool), <X's bid in m2>, sell
/// 10 @100 in m2]. (P) a PostOnly bid 1 @104 crosses the ask: the book
/// rejects it (also when it is larger than the ask depth: 11 @104). (G) a
/// GTC bid 1 @104 the asks eat whole: fills 1 @102, never rests. Both
/// sells fill 10 @101 exactly like the control run without X's bid.
fn same_batch_bid_that_never_rests_does_not_top_up(path: Path) {
    let mut outcomes = Vec::new();
    for shape in ["control", "post_only_crossing", "post_only_crossing_deep", "gtc_eaten"] {
        let (mk, t, x) = (addr(1), addr(2), addr(5));
        let (_d, mut ctx) = fresh(path, &[mk, x]);
        run(&mut ctx, path, &[place(mk, limit(1, true, 101, 10)), place(mk, limit(2, true, 101, 10)), place(mk, limit(2, false, 102, 10))]);
        fund_native(&ctx, &t, fp_e4(1_010_505));
        let mut actions = vec![place(t, limit(1, false, 100, 10))];
        match shape {
            "post_only_crossing" => actions.push(place(x, with_tif(limit(2, true, 104, 1), TimeInForce::PostOnly))),
            "post_only_crossing_deep" => actions.push(place(x, with_tif(limit(2, true, 104, 11), TimeInForce::PostOnly))),
            "gtc_eaten" => actions.push(place(x, limit(2, true, 104, 1))),
            _ => {}
        }
        actions.push(place(t, limit(2, false, 100, 10)));
        let r = run(&mut ctx, path, &actions);
        let what = format!("{path:?} {shape}");
        assert!(r[0].success && r.last().unwrap().success, "{what}: {r:?}");
        assert_eq!((pos_in(&ctx, &t, 1), pos_in(&ctx, &t, 2)), (-fp(10), -fp(10)), "{what}");
        assert_eq!(pos_in(&ctx, &x, 2), if shape == "gtc_eaten" { fp(1) } else { FixedPoint::ZERO }, "{what}");
        assert!(resting_in(&ctx, &x, 2).is_empty(), "{what}: X's bid never rests");
        assert_bal(&ctx, &t, fp_e4(1_010_505), FixedPoint::ZERO, &what);
        let b = bal(&ctx, &t);
        outcomes.push((pos_in(&ctx, &t, 1), pos_in(&ctx, &t, 2), b.available, b.order_margin));
    }
    assert!(outcomes.iter().all(|o| *o == outcomes[0]), "{path:?}: == control: {outcomes:?}");
}
per_path!(same_batch_bid_that_never_rests_does_not_top_up);

/// s89 review shapes around an earlier same-batch ask. M bids 10 @101 in m1
/// and m2 and asks 10 @110 in m2; T = [sell 10 @100 in m1 (pool), Z's GTC
/// ask 1 @104 in m2 (rests), X's bid 1 @p in m2, sell 10 @100 in m2 (50.5 at
/// B0, top-up 0.0505)]. A PostOnly bid at p = 104 or 105 crosses Z's ask
/// (book reject); a GTC bid 1 @104 fills Z's ask whole: none rests, and the
/// m2 sell fills 10 @101 with the pool intact. A PostOnly bid at p = 103
/// rests: the s89 bound counted it (top-up 1.0), B-blind does not — the m2
/// sell (budget 50.5505) fills 1 @103 + 8 @101 (IM(1,003 + q) <= 50.5505)
/// and is cut; X's bid never moved T's reservation (single path: 10).
fn bid_crossing_an_earlier_same_batch_ask_does_not_top_up(path: Path) {
    use TimeInForce::{PostOnly, GTC};
    for (tif, p, funded) in [(PostOnly, 103, fp(102)), (PostOnly, 104, fp(102)), (PostOnly, 105, fp_cents(10_250)), (GTC, 104, fp(102))] {
        let (mk, t, x, z) = (addr(1), addr(2), addr(5), addr(6));
        let (_d, mut ctx) = fresh(path, &[mk, x, z]);
        run(&mut ctx, path, &[place(mk, limit(1, true, 101, 10)), place(mk, limit(2, true, 101, 10)), place(mk, limit(2, false, 110, 10))]);
        fund_native(&ctx, &t, funded);
        let r = run(
            &mut ctx,
            path,
            &[
                place(t, limit(1, false, 100, 10)),
                place(z, limit(2, false, 104, 1)),
                place(x, with_tif(limit(2, true, p, 1), tif)),
                place(t, limit(2, false, 100, 10)),
            ],
        );
        let what = format!("{path:?} {tif:?} @{p}");
        assert!(r[0].success && r[1].success && r[3].success, "{what}: {r:?}");
        let m2 = if p == 103 && !matches!(path, Path::Single) { -fp(9) } else { -fp(10) };
        assert_eq!((pos_in(&ctx, &t, 1), pos_in(&ctx, &t, 2)), (-fp(10), m2), "{what}");
        let (x_pos, z_rest) = match (tif, p) {
            (PostOnly, 103) => (fp(1), vec![fp(1)]), // rested, then T hit it
            (PostOnly, _) => (FixedPoint::ZERO, vec![fp(1)]),
            _ => (fp(1), vec![]), // X's GTC bid lifted Z's ask
        };
        assert_eq!(pos_in(&ctx, &x, 2), x_pos, "{what}");
        assert_eq!(resting_in(&ctx, &z, 2), z_rest, "{what}: Z's ask");
        assert_bal(&ctx, &t, funded, FixedPoint::ZERO, &what);
    }
}
per_path!(bid_crossing_an_earlier_same_batch_ask_does_not_top_up);

/// No ask at the start of the batch (the s89 bound then counted nothing). M
/// bids 10 @101 in m1 and m2; T (101) = [sell 10 @100 in m1 (pool), X's
/// funded bid 1 @102 in m2 (rests), sell 10 @100 in m2]. B-blind tops the
/// m2 sell up by 0.0505 (B0 = 101) AHEAD of the pool (by design): the pool
/// keeps 0.4495 and m1 fills 8 (was 10); the m2 sell (budget 50.5505) fills
/// 1 @102 + 9 @101 (IM(1,002 + q) <= 50.5505; was 1 + 8). Batch paths only:
/// the single path's budget is the account.
fn same_batch_bid_does_not_count_without_a_start_ask(path: Path) {
    let (mk, t, x) = (addr(1), addr(2), addr(5));
    let (_d, mut ctx) = fresh(path, &[mk, x]);
    run(&mut ctx, path, &[place(mk, limit(1, true, 101, 10)), place(mk, limit(2, true, 101, 10))]);
    fund_native(&ctx, &t, fp(101));
    let r = run(&mut ctx, path, &[place(t, limit(1, false, 100, 10)), place(x, limit(2, true, 102, 1)), place(t, limit(2, false, 100, 10))]);
    assert!(r.iter().all(|x| x.success), "{path:?}: {r:?}");
    assert_eq!((pos_in(&ctx, &t, 1), pos_in(&ctx, &t, 2)), (-fp(8), -fp(10)), "{path:?}");
    assert_bal(&ctx, &t, fp(101), FixedPoint::ZERO, &format!("{path:?}"));
}
batch_paths!(same_batch_bid_does_not_count_without_a_start_ask);

/// Review 4 F-1 / s89 griefer shapes (unchanged numbers under B-blind): bids
/// that cannot rest funded at their price never raise a non-pool sell's
/// reservation. M bids 10 @101 in m1 and m2 and asks 10 @130 in m2; T
/// (110.40) = [sell 10 @100 in m1 (pool), <X's bid @120>, sell 10 @100 in
/// m2]. Topped up for 120 (the old bound's failure mode: 9.5 <= free 9.9)
/// the pool would keep 0.4 and the m1 sell would fill 8; B-blind takes only
/// 0.0505, so both fill 10 @101 on every path. X's bid: unfunded, IOC, FOK,
/// market (cap 120), off-tick, dust (< lot), reduce-only, stop-limit, or
/// [deep low bid 100 @1, 1 @120] with margin for the first only; with M
/// asking @120, a PostOnly bid 10 @120 (crosses: book reject) and a GTC bid
/// 10 @120 the asks eat whole (fills 10, never rests).
fn in_batch_griefer_bids_do_not_top_up_a_non_pool_sell(path: Path) {
    for shape in [
        "unfunded",
        "ioc",
        "fok",
        "market",
        "off_tick",
        "dust",
        "reduce_only",
        "stop_limit",
        "deep_low_then_high",
        "post_only_crossing",
        "gtc_eaten",
    ] {
        let (mk, t, x) = (addr(1), addr(2), addr(5));
        let (_d, mut ctx) = fresh(path, &[mk]);
        let ask = if matches!(shape, "post_only_crossing" | "gtc_eaten") { 120 } else { 130 };
        run(&mut ctx, path, &[place(mk, limit(1, true, 101, 10)), place(mk, limit(2, true, 101, 10)), place(mk, limit(2, false, ask, 10))]);
        fund_native(&ctx, &t, fp_cents(11_040));
        if shape != "unfunded" {
            fund_native(&ctx, &x, fp(if shape == "deep_low_then_high" { 10 } else { FUNDING }));
        }
        let high = limit(2, true, 120, 10);
        let mut actions = vec![place(t, limit(1, false, 100, 10))];
        match shape {
            "unfunded" => actions.push(place(x, high)),
            "ioc" => actions.push(place(x, with_tif(high, TimeInForce::IOC))),
            "fok" => actions.push(place(x, with_tif(high, TimeInForce::FOK))),
            "market" => actions.push(place(x, market(2, true, fp(120), 10))),
            "off_tick" => actions.push(place(x, PlaceOrderParams { price: fp_cents(12_050), ..high })),
            "dust" => actions.push(place(x, PlaceOrderParams { quantity: fp_cents(50), ..high })),
            "reduce_only" => actions.push(place(x, PlaceOrderParams { reduce_only: true, ..high })),
            "stop_limit" => actions.push(place(
                x,
                PlaceOrderParams { order_type: OrderType::StopLimit { trigger: fp(125), limit: fp(120) }, ..high },
            )),
            "post_only_crossing" => actions.push(place(x, with_tif(high, TimeInForce::PostOnly))),
            "gtc_eaten" => actions.push(place(x, high)),
            _ => {
                actions.push(place(x, limit(2, true, 1, 100)));
                actions.push(place(x, limit(2, true, 120, 1)));
            }
        }
        actions.push(place(t, limit(2, false, 100, 10)));
        let r = run(&mut ctx, path, &actions);
        let what = format!("{path:?} {shape}");
        assert!(r[0].success && r.last().unwrap().success, "{what}: {r:?}");
        assert_eq!((pos_in(&ctx, &t, 1), pos_in(&ctx, &t, 2)), (-fp(10), -fp(10)), "{what}");
        let x_fills = if shape == "gtc_eaten" { fp(10) } else { FixedPoint::ZERO };
        assert_eq!(pos_in(&ctx, &x, 2), x_fills, "{what}");
        assert_bal(&ctx, &t, fp_cents(11_040), FixedPoint::ZERO, &what);
    }
}
per_path!(in_batch_griefer_bids_do_not_top_up_a_non_pool_sell);

/// Was "soft and all or nothing" (s87). M bids 10 @101 in m2 and m3 and asks
/// 10 @130 in m2; B's funded bid 10 @105 in m2; T = [pool order (10), sell 10
/// @100 in m2, sell 10 @100 in m3]. The bound topped the m2 sell up for 105
/// only when the free margin covered all of it (T 114). B-blind ignores B's
/// bid: whatever T's free margin (112: 1.0 left, 114: 3.0), both sells get
/// their δ top-up (0.0505 each), m3 fills 10 and the m2 sell (budget
/// 50.5505) fills 2 @105 and is cut.
fn beyond_delta_bid_is_not_covered_whatever_the_free_margin(path: Path) {
    for funded in [112, 114] {
        let (mk, t, b) = (addr(1), addr(2), addr(4));
        let (_d, mut ctx) = fresh(path, &[mk, b]);
        run(&mut ctx, path, &[place(mk, limit(2, true, 101, 10)), place(mk, limit(3, true, 101, 10)), place(mk, limit(2, false, 130, 10))]);
        fund_native(&ctx, &t, fp(funded));
        let metrics = metered(&mut ctx);
        let r = run(
            &mut ctx,
            path,
            &[place(t, pool_order()), place(b, limit(2, true, 105, 10)), place(t, limit(2, false, 100, 10)), place(t, limit(3, false, 100, 10))],
        );
        let what = format!("{path:?} T={funded}");
        assert!(r.iter().all(|x| x.success), "{what}: {r:?}");
        assert_eq!((pos_in(&ctx, &t, 2), pos_in(&ctx, &t, 3)), (-fp(2), -fp(10)), "{what}");
        assert_bal(&ctx, &t, fp(funded - 10), fp(10), &what);
        assert_eq!(top_ups(&metrics), [2, 0, 0], "{what}");
    }
}
batch_paths!(beyond_delta_bid_is_not_covered_whatever_the_free_margin);

// ---- B-blind (s92, owner decisions; replaces the s87 / s89 same-batch bound) ----
// After Phase 2, each non-pool sell that takes a bid floor is topped up
// towards reserve(B0 x (1 + 10 bps), qty) — B0 its market's best bid at the
// start of Phase 2 — from its sender's free margin left after the whole
// Phase-2 fold, partially (min(extra, free left)), in flat batch order. No
// input from any other trader's orders of the batch; never a placement gate.
// Default margin 20x, tick 1: with B0 = 1,000 a 1-lot sell reserved at
// 1,000 (50) is topped up to reserve(1,001, 1) = 50.05.

/// s92 top-up counters `[full, partial, none]`.
fn top_ups(m: &torus_telemetry::Metrics) -> [u64; 3] {
    [m.sell_top_ups_full.get(), m.sell_top_ups_partial.get(), m.sell_top_ups_none.get()]
}

/// B-blind (bench shape: 1-lot sells priced below the best bid, bids of the
/// same batch a tick above it). M bids 1 @1,000 in m2 (B0); B's bid 1 @1,001
/// in m2 sorts before T's non-pool sell 1 @999. Before B-blind the sell
/// reserved at B0 (50); its fill @1,001 needs 50.05 → 0 fills, Cancelled (no
/// start ask: the s89 bound never fired). Now it is topped up to 50.05 and
/// fills 1 @1,001, like the single path.
fn non_pool_sell_fills_at_a_same_batch_bid_within_delta(path: Path) {
    let (mk, t, b) = (addr(1), addr(2), addr(4));
    let (_d, mut ctx) = fresh(path, &[mk, t, b]);
    run(&mut ctx, path, &[place(mk, limit(2, true, 1_000, 1))]);
    let metrics = metered(&mut ctx);
    let r = run(&mut ctx, path, &[place(t, pool_order()), place(b, limit(2, true, 1_001, 1)), place(t, limit(2, false, 999, 1))]);
    assert!(r.iter().all(|x| x.success), "{path:?}: {r:?}");
    assert_eq!(pos_in(&ctx, &t, 2), -fp(1), "{path:?}");
    assert_eq!(pos_in(&ctx, &b, 2), fp(1), "{path:?}: filled at B's bid");
    assert_eq!(resting_in(&ctx, &mk, 2), vec![fp(1)], "{path:?}: M untouched");
    assert_bal(&ctx, &t, fp(990), fp(10), &format!("{path:?}: top-up released"));
    assert_eq!(metrics.orders_rejected_cancelled.get(), 0, "{path:?}");
    assert!(sell_cuts(&metrics).is_empty(), "{path:?}");
    let want = if matches!(path, Path::Single) { [0, 0, 0] } else { [1, 0, 0] };
    assert_eq!(top_ups(&metrics), want, "{path:?}");
}
per_path!(non_pool_sell_fills_at_a_same_batch_bid_within_delta);

/// B-blind: a same-batch bid beyond B0 x (1 + δ) still cuts the non-pool
/// sell, and the cut is bucketed by its hit price minus the topped-up
/// reservation price 1,001: B's bid @1,002 (needs 50.10 > 50.05) → 1 tick
/// `[non-pool][zero][1-2]`; @1,040 → 39 ticks `[non-pool][zero][> 30]`.
fn non_pool_sell_at_a_same_batch_bid_beyond_delta_is_cut(path: Path) {
    for (bid, bucket) in [(1_002, 1), (1_040, 5)] {
        let (mk, t, b) = (addr(1), addr(2), addr(4));
        let (_d, mut ctx) = fresh(path, &[mk, t, b]);
        run(&mut ctx, path, &[place(mk, limit(2, true, 1_000, 1))]);
        let metrics = metered(&mut ctx);
        let r = run(&mut ctx, path, &[place(t, pool_order()), place(b, limit(2, true, bid, 1)), place(t, limit(2, false, 999, 1))]);
        let what = format!("{path:?} bid {bid}");
        assert!(r[0].success && r[1].success, "{what}: {r:?}");
        // Row 50: cut before its first fill = rejected (perpMarginRejected).
        assert!(!r[2].success && r[2].reason == torus_state::action_status::FailureReason::Margin, "{what}: {r:?}");
        assert_eq!(pos_in(&ctx, &t, 2), FixedPoint::ZERO, "{what}: cut with no fill");
        assert_eq!(resting_in(&ctx, &b, 2), vec![fp(1)], "{what}: B's bid untouched");
        assert_bal(&ctx, &t, fp(990), fp(10), &what);
        assert_eq!(sell_cuts(&metrics), vec![(1, 0, bucket, 1)], "{what}");
        assert_eq!(top_ups(&metrics), [1, 0, 0], "{what}");
    }
}
batch_paths!(non_pool_sell_at_a_same_batch_bid_beyond_delta_is_cut);

/// B-blind, partial: M bids 1 @1,000 in m2 and m3, B bids 1 @1,001 in both
/// (same batch, before T's sells). T (110.08) = [pool order (10), sell 1 @999
/// in m<first>, sell 1 @999 in m<second>]: free after the fold 0.08 → the
/// first sell in flat order gets its whole 0.05 and fills @1,001, the second
/// only 0.03 (budget 50.03 < 50.05: cut with no fill, bucketed against its
/// Phase-2 price 1,000). Swapping the two sells swaps the outcomes.
fn partial_top_up_goes_in_flat_order(path: Path) {
    for (first, second) in [(2, 3), (3, 2)] {
        let (mk, t, b) = (addr(1), addr(2), addr(4));
        let (_d, mut ctx) = fresh(path, &[mk, b]);
        run(&mut ctx, path, &[place(mk, limit(2, true, 1_000, 1)), place(mk, limit(3, true, 1_000, 1))]);
        fund_native(&ctx, &t, fp_cents(11_008));
        let metrics = metered(&mut ctx);
        let r = run(
            &mut ctx,
            path,
            &[
                place(b, limit(2, true, 1_001, 1)),
                place(b, limit(3, true, 1_001, 1)),
                place(t, pool_order()),
                place(t, limit(first, false, 999, 1)),
                place(t, limit(second, false, 999, 1)),
            ],
        );
        let what = format!("{path:?} first m{first}");
        assert!(r[..4].iter().all(|x| x.success), "{what}: {r:?}");
        // Row 50: the second sell, cut with no fill, is rejected for margin.
        assert!(!r[4].success && r[4].reason == torus_state::action_status::FailureReason::Margin, "{what}: {r:?}");
        assert_eq!((pos_in(&ctx, &t, first), pos_in(&ctx, &t, second)), (-fp(1), FixedPoint::ZERO), "{what}");
        assert_bal(&ctx, &t, fp_cents(10_008), fp(10), &what);
        assert_eq!(top_ups(&metrics), [1, 1, 0], "{what}");
        assert_eq!(sell_cuts(&metrics), vec![(1, 0, 1, 1)], "{what}");
    }
}
batch_paths!(partial_top_up_goes_in_flat_order);

/// B-blind never gates a placement: T (110) has no free margin left after
/// the fold, so neither non-pool sell gets a top-up (`none` 2); both are
/// accepted and fill at the start bid 1,000 within their own reservation.
fn top_up_never_rejects_a_placement(path: Path) {
    let (mk, t) = (addr(1), addr(2));
    let (_d, mut ctx) = fresh(path, &[mk]);
    run(&mut ctx, path, &[place(mk, limit(2, true, 1_000, 1)), place(mk, limit(3, true, 1_000, 1))]);
    fund_native(&ctx, &t, fp(110));
    let metrics = metered(&mut ctx);
    let r = run(&mut ctx, path, &[place(t, pool_order()), place(t, limit(2, false, 999, 1)), place(t, limit(3, false, 999, 1))]);
    assert!(r.iter().all(|x| x.success), "{path:?}: {r:?}");
    assert_eq!((pos_in(&ctx, &t, 2), pos_in(&ctx, &t, 3)), (-fp(1), -fp(1)), "{path:?}");
    assert_bal(&ctx, &t, fp(100), fp(10), &format!("{path:?}"));
    assert_eq!(top_ups(&metrics), [0, 0, 2], "{path:?}");
    assert!(sell_cuts(&metrics).is_empty(), "{path:?}");
}
batch_paths!(top_up_never_rejects_a_placement);

/// B-blind is griefing-free: no other trader's order of the batch changes a
/// non-pool sell's reservation, so none can drain its sender's pool. M bids
/// 10 @110 in m1 and 1 @1,000 in m2 and asks 1 @1,100 in m2. T (105.05) =
/// [sell 10 @100 in m1 (its pool: reserves 50, needs 55 at 110), <X's
/// orders in m2>, sell 1 @999 in m2 (50 at B0, top-up 0.05)]: the pool keeps
/// exactly 5 and the m1 sell fills 10 in every shape. X: a funded GTC bid
/// 1 @1,050 that rests below the start ask (the s89 bound topped T up by
/// reserve(1,050, 1) − 50 = 2.5 for it, leaving the pool 2.55: m1 filled
/// 5), a PostOnly bid @1,050, an IOC bid @1,100 (lifts the ask), an unfunded
/// bid, and 20 funded bids @1,001-1,020 (s89: top-up 1.0, m1 filled 8).
fn no_other_sender_changes_a_non_pool_sell_reservation(path: Path) {
    let mut m1_outcomes = Vec::new();
    for shape in ["control", "gtc_rests", "post_only", "ioc", "unfunded", "many"] {
        let (mk, t, x) = (addr(1), addr(2), addr(5));
        let (_d, mut ctx) = fresh(path, &[mk]);
        run(
            &mut ctx,
            path,
            &[place(mk, limit(1, true, 110, 10)), place(mk, limit(2, true, 1_000, 1)), place(mk, limit(2, false, 1_100, 1))],
        );
        fund_native(&ctx, &t, fp_cents(10_505));
        if shape != "unfunded" {
            fund_native(&ctx, &x, fp(FUNDING * 10));
        }
        let metrics = metered(&mut ctx);
        let mut actions = vec![place(t, limit(1, false, 100, 10))];
        match shape {
            "gtc_rests" | "unfunded" => actions.push(place(x, limit(2, true, 1_050, 1))),
            "post_only" => actions.push(place(x, with_tif(limit(2, true, 1_050, 1), TimeInForce::PostOnly))),
            "ioc" => actions.push(place(x, with_tif(limit(2, true, 1_100, 1), TimeInForce::IOC))),
            "many" => actions.extend((1_001..=1_020).map(|p| place(x, limit(2, true, p, 1)))),
            _ => {}
        }
        actions.push(place(t, limit(2, false, 999, 1)));
        let r = run(&mut ctx, path, &actions);
        let what = format!("{path:?} {shape}");
        // Row 50: the m2 sell either fills or, cut before its first fill
        // (some shapes), is rejected for margin at match (gas kept).
        let last = r.last().unwrap();
        let cut = !last.success && last.reason == torus_state::action_status::FailureReason::Margin && last.gas_used == 1000;
        assert!(r[0].success && (last.success || cut), "{what}: {r:?}");
        assert_eq!(pos_in(&ctx, &t, 1), -fp(10), "{what}: the pool kept its 5");
        assert_eq!(top_ups(&metrics), [1, 0, 0], "{what}: the same top-up in every shape");
        m1_outcomes.push((pos_in(&ctx, &t, 1), resting_in(&ctx, &t, 1)));
    }
    assert!(m1_outcomes.iter().all(|o| *o == m1_outcomes[0]), "{path:?}: {m1_outcomes:?}");
}
batch_paths!(no_other_sender_changes_a_non_pool_sell_reservation);

/// Option B, unchanged: buys of a multi-market sender in non-pool markets
/// (GTC, IOC, FOK limit buys @105 and a market buy cap 110 at mark 100, all
/// into asks 10 @100) fill exactly as before.
fn non_pool_buys_unchanged(path: Path) {
    let (mk, t) = (addr(1), addr(2));
    let (_d, mut ctx) = fresh(path, &[mk]);
    set_mark(&ctx, 5, fp(100));
    let asks: Vec<_> = (2..=5).map(|m| place(mk, limit(m, false, 100, 10))).collect();
    run(&mut ctx, path, &asks);
    fund_native(&ctx, &t, fp(250));
    let r = run(
        &mut ctx,
        path,
        &[
            place(t, pool_order()),
            place(t, limit(2, true, 105, 10)),
            place(t, with_tif(limit(3, true, 105, 10), TimeInForce::IOC)),
            place(t, with_tif(limit(4, true, 105, 10), TimeInForce::FOK)),
            place(t, market(5, true, fp(110), 10)),
        ],
    );
    assert!(r.iter().all(|x| x.success), "{path:?}: {r:?}");
    for m in 2..=5 {
        assert_eq!(pos_in(&ctx, &t, m), fp(10), "{path:?} m{m}");
    }
    assert_bal(&ctx, &t, fp(240), fp(10), &format!("{path:?}"));
}
per_path!(non_pool_buys_unchanged);

/// Option B, unchanged: a sell in the sender's POOL market — the pool-
/// defining order itself, and a later sell in the same market — still
/// reserves at its limit and fills from its pool (M bids 10 @101 in m1):
/// funded 50.25 (resp. 60.25 with a resting 1 @200 first), the 10 @100 sell
/// is admitted (50 <= 50.25) and fills 5: need IM(101 q + 100 (10 − q)) <=
/// 50.25 (the unfilled rest is held at the limit). B applied to it would
/// reject it at placement (50.5 > 50.25).
fn pool_market_sell_unchanged_when_it_is_the_first_checked_order(path: Path) {
    for first in [false, true] {
        let (mk, t) = (addr(1), addr(2));
        let (_d, mut ctx) = fresh(path, &[mk]);
        run(&mut ctx, path, &[place(mk, limit(1, true, 101, 10))]);
        let mut actions = Vec::new();
        if first {
            fund_native(&ctx, &t, fp_cents(6_025));
            actions.push(place(t, pool_order()));
        } else {
            fund_native(&ctx, &t, fp_cents(5_025));
        }
        actions.push(place(t, limit(1, false, 100, 10)));
        let r = run(&mut ctx, path, &actions);
        let what = format!("{path:?} after_pool_order={first}");
        assert!(r.iter().all(|x| x.success), "{what}: {r:?}");
        assert_eq!(pos_in(&ctx, &t, 1), -fp(5), "{what}");
    }
}
per_path!(pool_market_sell_unchanged_when_it_is_the_first_checked_order);

/// Option B, unchanged: no better bid (best bid 99 < limit, or no bid at
/// all) — the non-pool sell rests at its limit holding reserve(100, 10).
fn non_pool_sell_without_a_better_bid_reserves_at_its_limit(path: Path) {
    for bid in [true, false] {
        let (mk, t) = (addr(1), addr(2));
        let (_d, mut ctx) = fresh(path, &[mk, t]);
        if bid {
            run(&mut ctx, path, &[place(mk, limit(2, true, 99, 10))]);
        }
        let r = run(&mut ctx, path, &[place(t, pool_order()), place(t, limit(2, false, 100, 10))]);
        let what = format!("{path:?} bid={bid}");
        assert!(r.iter().all(|x| x.success), "{what}: {r:?}");
        assert_eq!(resting_in(&ctx, &t, 2), vec![fp(10)], "{what}");
        assert_bal(&ctx, &t, fp(940), fp(60), &what);
    }
}
per_path!(non_pool_sell_without_a_better_bid_reserves_at_its_limit);

/// Option B, unchanged: a PostOnly sell at or below the best bid is still a
/// BOOK reject (never a margin reject), and a reduce-only sell (T long 10
/// in m2) still closes unchecked into the bid @101.
fn non_pool_post_only_and_reduce_only_sells_unchanged(path: Path) {
    let (mk, t) = (addr(1), addr(2));
    let (_d, mut ctx) = fresh(path, &[mk, t]);
    run(&mut ctx, path, &[place(mk, limit(2, true, 101, 10))]);
    let r = run(&mut ctx, path, &[place(t, pool_order()), place(t, with_tif(limit(2, false, 100, 10), TimeInForce::PostOnly))]);
    assert!(r[0].success, "{path:?}: {:?}", r[0].error);
    assert!(!is_margin_reject(&r[1]), "{path:?}: {:?}", r[1]);
    assert!(resting_in(&ctx, &t, 2).is_empty(), "{path:?}");
    assert_eq!(pos_in(&ctx, &t, 2), FixedPoint::ZERO, "{path:?}");
    assert_bal(&ctx, &t, fp(990), fp(10), &format!("{path:?}: post-only"));
    // Reduce-only: T long 10 in m2 (seeded), sells 10 reduce-only.
    ctx.positions
        .apply_fill(&t, 2, true, fp(10), fp(100), torus_core::position::MarginType::Cross)
        .unwrap();
    let ro = PlaceOrderParams { reduce_only: true, ..limit(2, false, 100, 10) };
    let r = run(&mut ctx, path, &[place(t, limit(1, false, 200, 1)), place(t, ro)]);
    assert!(r.iter().all(|x| x.success), "{path:?}: {r:?}");
    assert_eq!(pos_in(&ctx, &t, 2), FixedPoint::ZERO, "{path:?}: closed");
}
per_path!(non_pool_post_only_and_reduce_only_sells_unchanged);

/// Option B, unchanged: a stop-limit sell in a non-pool market fired by the
/// batch's fills is placed through the single path (budget = the account).
/// M bids 5 @101 and 5 @99 in m2; T has a stop-limit sell 5 (trigger 101,
/// limit 95; pending reservation 23.75). Batch [T pool order, Y market sell
/// 5]: Y fills @101, the stop fires and sells 5 @99. The pending reservation
/// is released exactly: T keeps only the pool order's 10.
fn triggered_stop_limit_sell_in_a_non_pool_market_unchanged(path: Path) {
    let (mk, t, y) = (addr(1), addr(2), addr(6));
    let (_d, mut ctx) = fresh(path, &[mk, t, y]);
    run(&mut ctx, path, &[place(mk, limit(2, true, 101, 5)), place(mk, limit(2, true, 99, 5))]);
    let stop = PlaceOrderParams {
        order_type: OrderType::StopLimit { trigger: fp(101), limit: fp(95) },
        ..limit(2, false, 95, 5)
    };
    assert!(run(&mut ctx, path, &[place(t, stop)])[0].success, "{path:?}");
    assert_bal(&ctx, &t, fp_cents(97_625), fp_cents(2_375), "pending stop");
    let r = run(&mut ctx, path, &[place(t, pool_order()), place(y, market(2, false, fp(1), 5))]);
    assert!(r.iter().all(|x| x.success), "{path:?}: {r:?}");
    assert_eq!(pos_in(&ctx, &y, 2), -fp(5), "{path:?}");
    assert_eq!(pos_in(&ctx, &t, 2), -fp(5), "{path:?}: the fired stop sold into the 99 bid");
    assert_bal(&ctx, &t, fp(990), fp(10), &format!("{path:?}"));
}
per_path!(triggered_stop_limit_sell_in_a_non_pool_market_unchanged);

/// Option B, unchanged A5 identity for modify: after the partial-fill case
/// (6 @100 resting from a B-reserved sell), modify to 8 @102 → the order
/// holds reserve(102, 8) = 40.8.
fn modify_of_a_resting_non_pool_sell_keeps_the_a5_identity(path: Path) {
    let (mk, t) = (addr(1), addr(2));
    let (_d, mut ctx) = fresh(path, &[mk]);
    run(&mut ctx, path, &[place(mk, limit(2, true, 101, 4))]);
    fund_native(&ctx, &t, fp(200));
    run(&mut ctx, path, &[place(t, pool_order()), place(t, limit(2, false, 100, 10))]);
    let id = ctx.order_books[&2].orders_for_trader(&t)[0].id;
    let r = run(&mut ctx, path, &[(t, NativeAction::ModifyOrder { order_id: id, new_price: Some(fp(102)), new_qty: Some(fp(8)) })]);
    assert!(r[0].success, "{path:?}: {:?}", r[0].error);
    assert_eq!(resting_in(&ctx, &t, 2), vec![fp(8)], "{path:?}");
    assert_bal(&ctx, &t, fp_cents(14_920), fp_cents(5_080), &format!("{path:?}: 10 + 40.8"));
    run(&mut ctx, path, &[(t, NativeAction::CancelAllOrders { market_id: None })]);
    assert_bal(&ctx, &t, fp(200), FixedPoint::ZERO, &format!("{path:?}: all released"));
}
per_path!(modify_of_a_resting_non_pool_sell_keeps_the_a5_identity);

/// B2 (s87): a taker-only budget gets the makers' +1 raw rounding
/// allowance. T short 1 in m2 at entry 100 + 19 raw (notional ≡ 19 mod 20
/// raw, no mark); GTC sell (1 + 1 raw) @101 into a bid at exactly 101 (no
/// better bid: B does not apply). Its need IM(A + x) − IM(A) is 1 raw
/// above the floor(x/20) it reserved: before B2 it was Cancelled with 0
/// fills at its own limit price (the bench's 158 at-limit cases).
fn taker_only_rounding_does_not_cancel_a_sell_at_its_limit(path: Path) {
    let (mk, t) = (addr(1), addr(2));
    let (_d, mut ctx) = fresh(path, &[mk, t]);
    let metrics = metered(&mut ctx);
    let one = FixedPoint::from_raw(1);
    ctx.positions
        .apply_fill(&t, 2, false, fp(1), fp(100) + FixedPoint::from_raw(19), torus_core::position::MarginType::Cross)
        .unwrap();
    let q = fp(1) + one;
    run(&mut ctx, path, &[place(mk, PlaceOrderParams { quantity: q, ..limit(2, true, 101, 1) })]);
    let r = run(&mut ctx, path, &[place(t, pool_order()), place(t, PlaceOrderParams { quantity: q, ..limit(2, false, 101, 1) })]);
    assert!(r.iter().all(|x| x.success), "{path:?}: {r:?}");
    assert_eq!(pos_in(&ctx, &t, 2), -(fp(2) + one), "{path:?}");
    assert!(resting_in(&ctx, &t, 2).is_empty(), "{path:?}");
    assert_eq!(metrics.orders_rejected_cancelled.get(), 0, "{path:?}");
}
per_path!(taker_only_rounding_does_not_cancel_a_sell_at_its_limit);

// ---- s92 margin-cut counters (observability only) ----

/// Non-zero s92 sell-cut counters as `(non_pool, partial, tick bucket, count)`.
fn sell_cuts(m: &torus_telemetry::Metrics) -> Vec<(usize, usize, usize, u64)> {
    let mut out = Vec::new();
    for (np, by_fill) in m.sell_margin_cuts.iter().enumerate() {
        for (partial, buckets) in by_fill.iter().enumerate() {
            for (b, c) in buckets.iter().enumerate() {
                if c.get() > 0 {
                    out.push((np, partial, b, c.get()));
                }
            }
        }
    }
    out
}

/// s92 counters: a POOL sell cut part-way. M bids 10 @101 in m1; T (50.25)
/// sells 10 @100 there (its first checked order, so its pool; single path:
/// the account): fills 5 @101, then its margin runs out at 101 = 1 tick
/// above the price it reserved at (100) → `[pool][partial][1-2 ticks]`.
fn sell_cut_counter_pool_partial(path: Path) {
    let (mk, t) = (addr(1), addr(2));
    let (_d, mut ctx) = fresh(path, &[mk]);
    run(&mut ctx, path, &[place(mk, limit(1, true, 101, 10))]);
    fund_native(&ctx, &t, fp_cents(5_025));
    let metrics = metered(&mut ctx);
    let r = run(&mut ctx, path, &[place(t, limit(1, false, 100, 10))]);
    assert!(r[0].success, "{path:?}: {r:?}");
    assert_eq!(pos_in(&ctx, &t, 1), -fp(5), "{path:?}");
    assert_eq!(sell_cuts(&metrics), vec![(0, 1, 1, 1)], "{path:?}");
    assert_eq!(metrics.orders_cancelled_partial_fill.get(), 1, "{path:?}");
}
per_path!(sell_cut_counter_pool_partial);

/// s92 counters: a NON-POOL sell cut with zero fills. m2 has no bid at the
/// start of the batch (so no bid floor); T (1,000) = [pool order in m1, X's
/// bid 1 @1005 in m2 (rests), sell 1 @1000 in m2]. The sell's budget is its
/// reservation at 1,000 (50); the fill @1005 needs 50.25 → cancelled with no
/// fill, 5 ticks above its reservation price → `[non-pool][zero][3-5]`.
/// Batch paths only: the single path's budget is the account.
/// Row 50: the sell is rejected (`Margin`, HL `perpMarginRejected`) with the
/// executed order's gas, on the sequential settle (Batch) and pass B
/// (Parallel).
fn sell_cut_counter_non_pool_zero_fill(path: Path) {
    let (t, x) = (addr(2), addr(5));
    let (_d, mut ctx) = fresh(path, &[t, x]);
    let metrics = metered(&mut ctx);
    let r = run(&mut ctx, path, &[place(t, pool_order()), place(x, limit(2, true, 1_005, 1)), place(t, limit(2, false, 1_000, 1))]);
    assert!(r[0].success && r[1].success, "{path:?}: {r:?}");
    assert!(!r[2].success, "{path:?}: {:?}", r[2]);
    assert_eq!(r[2].reason, torus_state::action_status::FailureReason::Margin, "{path:?}");
    assert!(r[2].error.as_deref().is_some_and(|e| e.starts_with("insufficient margin")), "{path:?}: {:?}", r[2]);
    assert_eq!(r[2].gas_used, 1000, "{path:?}");
    assert_eq!(pos_in(&ctx, &t, 2), FixedPoint::ZERO, "{path:?}");
    assert_eq!(resting_in(&ctx, &x, 2), vec![fp(1)], "{path:?}: X's bid untouched");
    assert_eq!(sell_cuts(&metrics), vec![(1, 0, 2, 1)], "{path:?}");
    assert_eq!(metrics.orders_rejected_cancelled.get(), 1, "{path:?}");
}
batch_paths!(sell_cut_counter_non_pool_zero_fill);

/// s92 counters: reduce-only cuts. T long 4 in m1 rests a reduce-only sell
/// 4 @110; its plain sell 4 @100 into M's bid closes the position, so the
/// post-fill sweep cancels the reduce-only order: one cut.
fn reduce_only_cut_counter(path: Path) {
    let (mk, t) = (addr(1), addr(2));
    let (_d, mut ctx) = fresh(path, &[mk, t]);
    ctx.positions
        .apply_fill(&t, 1, true, fp(4), fp(100), torus_core::position::MarginType::Cross)
        .unwrap();
    let ro = PlaceOrderParams { reduce_only: true, ..limit(1, false, 110, 4) };
    assert!(run(&mut ctx, path, &[place(t, ro)])[0].success, "{path:?}");
    run(&mut ctx, path, &[place(mk, limit(1, true, 100, 4))]);
    let metrics = metered(&mut ctx);
    let r = run(&mut ctx, path, &[place(t, limit(1, false, 100, 4))]);
    assert!(r[0].success, "{path:?}: {r:?}");
    assert_eq!(pos_in(&ctx, &t, 1), FixedPoint::ZERO, "{path:?}");
    assert!(resting_in(&ctx, &t, 1).is_empty(), "{path:?}: the reduce-only sell is cut");
    assert_eq!(metrics.reduce_only_cuts.get(), 1, "{path:?}");
    assert_eq!(metrics.maker_margin_cancels.get(), 0, "{path:?}");
}
per_path!(reduce_only_cut_counter);

/// Option B §7.3 #15 (A5 telescoping under B): a seeded multi-market run
/// shaped like golden scenario A (40 senders x 12 markets, thin to rich
/// balances, aggressive GTC / IOC / market sells and buys, reduce-only
/// orders, cancel-alls), 8 blocks, serial and engine-forced (4 threads).
/// After every block each sender's `order_margin` equals Σ reserve(price,
/// remaining) over its resting rows exactly — B raises a non-pool sell's
/// reservation, but every release is computed at the limit, so none is
/// stranded or over-released. Non-vacuous for B-blind (s92: top-ups
/// happen on both paths), and value is conserved: Σ
/// (available + order_margin) + Σ signed size × (mid − entry) stays the
/// funding total (no fees, no mark: no liquidation; every fill has two
/// sides, so the reference price cancels) up to the entry-averaging
/// rounding (measured <= 6 raw; any top-up is >= IM(1 × tick) = 0.05).
#[test]
fn order_margin_matches_resting_reservations_under_option_b() {
    struct Lcg(u64);
    impl Lcg {
        fn below(&mut self, n: u64) -> u64 {
            self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (self.0 >> 11) % n
        }
    }
    let senders: Vec<Address> = (0..40u8).map(|i| addr(10 + i)).collect();
    let funding = |i: usize| fp([1_600, 4_000, 12_000, 100_000_000][i % 4]);
    let funded = (0..senders.len()).fold(FixedPoint::ZERO, |acc, i| acc + funding(i));
    let mut totals = Vec::new();
    for threads in [1usize, 4] {
        let (_d, db) = open_test_db();
        let mut ctx = make_ctx(db);
        for (i, s) in senders.iter().enumerate() {
            fund_native(&ctx, s, funding(i));
        }
        let mut rng = Lcg(0x5EED_0087_0B0B);
        let (mut fills, mut checked) = (0u32, 0usize);
        for block in 0..8 {
            let mut actions = Vec::new();
            for _ in 0..30 {
                let si = rng.below(senders.len() as u64);
                let s = senders[si as usize];
                if rng.below(100) < 4 {
                    actions.push((s, NativeAction::CancelAllOrders { market_id: None }));
                    continue;
                }
                let orders: Vec<PlaceOrderParams> = (0..1 + rng.below(24))
                    .map(|_| {
                        let m = 1 + rng.below(12);
                        let mid = 30_000i64;
                        let mut is_buy = (si + m).is_multiple_of(2);
                        let aggressive = rng.below(2) == 0;
                        let d = 1 + rng.below(5) as i64;
                        let mut p = limit(m, is_buy, if is_buy == aggressive { mid + d } else { mid - d }, 1);
                        p.quantity = fp(1) + FixedPoint::from_raw(rng.below(3) as i128 * FixedPoint::SCALE / 2);
                        match rng.below(100) {
                            0..=9 => {
                                p.order_type = OrderType::Market;
                                p.time_in_force = TimeInForce::IOC;
                                p.price = fp(if is_buy { mid + 60 } else { mid - 60 });
                            }
                            10..=14 => p.time_in_force = TimeInForce::IOC,
                            15..=17 => p.time_in_force = TimeInForce::FOK,
                            18..=22 => {
                                is_buy = !is_buy;
                                p.is_buy = is_buy;
                                p.reduce_only = true;
                                p.price = fp(if is_buy { mid + 2 } else { mid - 2 });
                            }
                            // Bids inside the spread that rest (what the s89
                            // bound counted; kept for the same load).
                            23..=32 if is_buy => {
                                p.price = fp(mid);
                                p.time_in_force = if rng.below(2) == 0 { TimeInForce::PostOnly } else { TimeInForce::GTC };
                            }
                            _ => {}
                        }
                        p
                    })
                    .collect();
                actions.push((s, NativeAction::PlaceOrderBatch(orders)));
            }
            let r = NativeExecutor::execute_batch_engine_mode(&mut ctx, &actions, threads);
            assert!(ctx.fatal_error.is_none(), "threads={threads} block={block}");
            assert!(r.results.iter().any(|x| x.success), "threads={threads} block={block}");
            fills = fills.max(ctx.trade_index);
            for s in &senders {
                let resting = ctx.order_books.values().flat_map(|b| b.orders_for_trader(s)).fold(FixedPoint::ZERO, |acc, o| {
                    acc + torus_core::margin::order_initial_margin(None, o.price * o.remaining_qty)
                });
                assert_eq!(bal(&ctx, s).order_margin, resting, "threads={threads} block={block} sender {s}");
                checked += 1;
            }
            let value = senders.iter().fold(FixedPoint::ZERO, |acc, s| {
                let b = bal(&ctx, s);
                (1..=12u64).fold(acc + b.available + b.order_margin, |acc, m| {
                    acc + pos_in(&ctx, s, m) * fp(30_000) - ctx.positions.get_position(s, m).unwrap().map_or(FixedPoint::ZERO, |p| {
                        let notional = p.size * p.entry_price;
                        if p.is_long {
                            notional
                        } else {
                            -notional
                        }
                    })
                })
            });
            let drift = abs(value - funded);
            assert!(drift <= FixedPoint::from_raw(100), "threads={threads} block={block}: value drift {drift}");
        }
        assert!(fills > 0 && checked == 8 * senders.len(), "threads={threads}: non-vacuous");
        assert!(ctx.phase_accum.sell_top_ups > 0, "threads={threads}: B-blind top-ups happened");
        let state: Vec<(FixedPoint, FixedPoint)> =
            senders.iter().map(|s| bal(&ctx, s)).map(|b| (b.available, b.order_margin)).collect();
        totals.push(state);
    }
    assert_eq!(totals[0], totals[1], "serial == engine");
}
