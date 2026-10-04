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
/// Either way: still long 20, maker's ask untouched, 90 / 110.
fn checked_order_credit_is_bounded_at_match(path: Path) {
    let (t, mk) = (addr(2), addr(4));
    let (_d, mut ctx) = fresh(path, &[mk]);
    long_20_on_200(&mut ctx, path, t);
    assert!(run(&mut ctx, path, &[place(mk, limit(1, false, 100, 5))])[0].success, "{path:?}");
    let r = run(&mut ctx, path, &[place(t, limit(1, false, 110, 20)), place(t, market(1, true, fp(100), 1))]);
    assert!(r[0].success, "{path:?}: {:?}", r[0].error);
    let accepted = !matches!(path, Path::Single);
    assert_eq!(r[1].success, accepted, "{path:?}: {:?}", r[1].error);
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
        let r = run(&mut ctx, path, &[place(t, market(1, false, fp(1), 10))]);
        let what = format!("{path:?} mark={mark}");
        assert!(r[0].success, "{what}: {:?}", r[0].error);
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
fn non_pool_limit_sell_fills_at_a_better_bid(path: Path) {
    let (mk, t) = (addr(1), addr(2));
    let (_d, mut ctx) = fresh(path, &[mk]);
    let metrics = metered(&mut ctx);
    run(&mut ctx, path, &[place(mk, limit(1, true, 101, 10)), place(mk, limit(2, true, 101, 10))]);
    fund_native(&ctx, &t, fp(101));
    let r = run(&mut ctx, path, &[place(t, limit(1, false, 100, 10)), place(t, limit(2, false, 100, 10))]);
    assert!(r.iter().all(|x| x.success), "{path:?}: {r:?}");
    assert_eq!((pos_in(&ctx, &t, 1), pos_in(&ctx, &t, 2)), (-fp(10), -fp(10)), "{path:?}");
    assert!(resting_in(&ctx, &t, 1).is_empty() && resting_in(&ctx, &t, 2).is_empty(), "{path:?}");
    assert_bal(&ctx, &t, fp(101), FixedPoint::ZERO, &format!("{path:?}"));
    assert_eq!(metrics.orders_rejected_cancelled.get(), 0, "{path:?}");
}
per_path!(non_pool_limit_sell_fills_at_a_better_bid);

/// Option B: the same shape with an IOC and a FOK sell in m2 (never rest:
/// reserve the opening quantity). Before B the IOC filled 9 (lot 1: 45.45 <=
/// 50) and the FOK was rejected whole; with B both fill 10 like the single path.
fn non_pool_ioc_and_fok_sell_fill_at_a_better_bid(path: Path) {
    for tif in [TimeInForce::IOC, TimeInForce::FOK] {
        let (mk, t) = (addr(1), addr(2));
        let (_d, mut ctx) = fresh(path, &[mk]);
        run(&mut ctx, path, &[place(mk, limit(1, true, 101, 10)), place(mk, limit(2, true, 101, 10))]);
        fund_native(&ctx, &t, fp(101));
        let r = run(
            &mut ctx,
            path,
            &[place(t, limit(1, false, 100, 10)), place(t, with_tif(limit(2, false, 100, 10), tif))],
        );
        let what = format!("{path:?} {tif:?}");
        assert!(r.iter().all(|x| x.success), "{what}: {r:?}");
        assert_eq!((pos_in(&ctx, &t, 1), pos_in(&ctx, &t, 2)), (-fp(10), -fp(10)), "{what}");
        assert_bal(&ctx, &t, fp(101), FixedPoint::ZERO, &what);
    }
}
per_path!(non_pool_ioc_and_fok_sell_fill_at_a_better_bid);

/// Option B for market sells: mark 100 in m2, M bids 10 @100 in m1 and 10
/// @110 in m2; T (105) market-sells 10 (cap 1) in m1 (its pool; no mark:
/// reserves at the cap, 0.5) and in m2. Before B the m2 sell reserved at
/// the mark (50) and its fills @110 (55) were cut after 9. B: it reserves
/// at max(mark, 110) = 55 and fills 10, exactly like the single path
/// (free after m1: 105 − 50 = 55).
fn non_pool_market_sell_reserves_at_the_start_best_bid_above_the_mark(path: Path) {
    let (mk, t) = (addr(1), addr(2));
    let (_d, mut ctx) = fresh(path, &[mk]);
    set_mark(&ctx, 2, fp(100));
    run(&mut ctx, path, &[place(mk, limit(1, true, 100, 10)), place(mk, limit(2, true, 110, 10))]);
    fund_native(&ctx, &t, fp(105));
    let r = run(&mut ctx, path, &[place(t, market(1, false, fp(1), 10)), place(t, market(2, false, fp(1), 10))]);
    assert!(r.iter().all(|x| x.success), "{path:?}: {r:?}");
    assert_eq!((pos_in(&ctx, &t, 1), pos_in(&ctx, &t, 2)), (-fp(10), -fp(10)), "{path:?}");
    assert_bal(&ctx, &t, fp(105), FixedPoint::ZERO, &format!("{path:?}"));
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
/// reservation — only the start-of-batch best bid does (the same-batch bound
/// below is a top-up after Phase 2, never a gate; m2 has no ask here, so it
/// adds nothing). M bids 10 @101 in m2; T (61) = [pool order
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

// ---- Same-batch bid bound (s87, owner decision 1) ----
// After Phase 2, a non-pool sell is topped up from its sender's free margin
// left after the whole Phase-2 fold (what would otherwise go to its D2 pool)
// to cover the highest bid placed EARLIER IN THE SAME BATCH that can rest:
// accepted in Phase 2 (funded), a GTC / PostOnly limit, not reduce-only, on
// the tick, at least a lot, capped at the market's start-of-batch best ask
// (no ask: nothing counts). All or nothing; never a placement gate.

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

/// Same-batch bound (bench repro): M bids 10 @101 and asks 10 @110 in m2;
/// B's funded GTC bid 10 @105 sorts before T's non-pool sell 10 @100. B
/// reserved the sell at 101 (50.5); its first fill @105 needs 52.5 → 0
/// fills, Cancelled. Now it is topped up to reserve(105, 10) = 52.5 from
/// T's free margin and fills 10 @105, like the single path.
fn non_pool_sell_fills_at_a_funded_same_batch_bid(path: Path) {
    let (mk, t, b) = (addr(1), addr(2), addr(4));
    let (_d, mut ctx) = fresh(path, &[mk, t, b]);
    let metrics = metered(&mut ctx);
    run(&mut ctx, path, &[place(mk, limit(2, true, 101, 10)), place(mk, limit(2, false, 110, 10))]);
    let r = run(&mut ctx, path, &[place(t, pool_order()), place(b, limit(2, true, 105, 10)), place(t, limit(2, false, 100, 10))]);
    assert!(r.iter().all(|x| x.success), "{path:?}: {r:?}");
    assert_eq!(pos_in(&ctx, &t, 2), -fp(10), "{path:?}");
    assert_eq!(pos_in(&ctx, &b, 2), fp(10), "{path:?}: filled at B's bid");
    assert_eq!(resting_in(&ctx, &mk, 2), vec![fp(10), fp(10)], "{path:?}: M untouched");
    assert_bal(&ctx, &t, fp(990), fp(10), &format!("{path:?}: top-up released"));
    assert_bal(&ctx, &b, fp(FUNDING), FixedPoint::ZERO, &format!("{path:?}"));
    assert_eq!(metrics.orders_rejected_cancelled.get(), 0, "{path:?}");
}
per_path!(non_pool_sell_fills_at_a_funded_same_batch_bid);

/// Same-batch bound, cap: M bids 10 @101 in m1 and m2 and asks 10 @102 in
/// m2; T (102) = [sell 10 @100 in m1 (pool; reserves 50, needs 50.5 at
/// 101), X's funded bid 1 @104 in m2 (crosses the ask: fills 1 @102, never
/// rests), sell 10 @100 in m2 (B: 50.5)]. The bound is capped at the ask:
/// top-up reserve(102, 10) − 50.5 = 0.5 leaves the pool 1 and both sells
/// fill 10 @101. Uncapped (104: top-up 1.5) the pool would be 0 and the m1
/// sell would fill nothing.
fn same_batch_bid_counts_only_up_to_the_start_best_ask(path: Path) {
    let (mk, t, x) = (addr(1), addr(2), addr(5));
    let (_d, mut ctx) = fresh(path, &[mk, x]);
    run(&mut ctx, path, &[place(mk, limit(1, true, 101, 10)), place(mk, limit(2, true, 101, 10)), place(mk, limit(2, false, 102, 10))]);
    fund_native(&ctx, &t, fp(102));
    let r = run(&mut ctx, path, &[place(t, limit(1, false, 100, 10)), place(x, limit(2, true, 104, 1)), place(t, limit(2, false, 100, 10))]);
    assert!(r.iter().all(|x| x.success), "{path:?}: {r:?}");
    assert_eq!((pos_in(&ctx, &t, 1), pos_in(&ctx, &t, 2)), (-fp(10), -fp(10)), "{path:?}");
    assert_eq!(pos_in(&ctx, &x, 2), fp(1), "{path:?}: X lifted the ask");
    assert_bal(&ctx, &t, fp(102), FixedPoint::ZERO, &format!("{path:?}"));
}
per_path!(same_batch_bid_counts_only_up_to_the_start_best_ask);

/// Same-batch bound, no ask at the start of the batch: nothing counts. M
/// bids 10 @101 in m1 and m2 (no asks); T (101) = [sell 10 @100 in m1
/// (pool), X's funded bid 1 @102 in m2 (rests), sell 10 @100 in m2]. No
/// top-up: the pool keeps 0.5 and m1 fills 10; the m2 sell (budget 50.5)
/// fills 1 @102 and 8 @101 (IM(102 + 101 q + 100 (9 − q)) <= 50.5) and the
/// last 1 is cancelled. Counted, the top-up 0.5 would empty the pool (m1:
/// 0 fills). Batch paths only: the single path's budget is the account.
fn same_batch_bid_does_not_count_without_a_start_ask(path: Path) {
    let (mk, t, x) = (addr(1), addr(2), addr(5));
    let (_d, mut ctx) = fresh(path, &[mk, x]);
    run(&mut ctx, path, &[place(mk, limit(1, true, 101, 10)), place(mk, limit(2, true, 101, 10))]);
    fund_native(&ctx, &t, fp(101));
    let r = run(&mut ctx, path, &[place(t, limit(1, false, 100, 10)), place(x, limit(2, true, 102, 1)), place(t, limit(2, false, 100, 10))]);
    assert!(r.iter().all(|x| x.success), "{path:?}: {r:?}");
    assert_eq!((pos_in(&ctx, &t, 1), pos_in(&ctx, &t, 2)), (-fp(10), -fp(9)), "{path:?}");
    assert_bal(&ctx, &t, fp(101), FixedPoint::ZERO, &format!("{path:?}"));
}
batch_paths!(same_batch_bid_does_not_count_without_a_start_ask);

/// Same-batch bound vs review 4 F-1 (not gameable): bids that cannot rest
/// funded at their price never top up a non-pool sell. M bids 10 @101 in
/// m1 and m2 and asks 10 @130 in m2; T (110.40) = [sell 10 @100 in m1
/// (pool), <X's bid @120>, sell 10 @100 in m2]. Counted, the top-up
/// reserve(120, 10) − 50.5 = 9.5 <= free 9.9 would leave the pool 0.4 and
/// the m1 sell would fill 8 (IM(1,000 + q) <= 50.4); not counted, both fill
/// 10 @101 on every path. X's bid: unfunded, IOC, FOK, market (cap 120),
/// off-tick, dust (< lot), reduce-only, stop-limit, or [deep low bid 100 @1,
/// 1 @120] with margin for the first only.
fn in_batch_griefer_bids_do_not_top_up_a_non_pool_sell(path: Path) {
    for shape in ["unfunded", "ioc", "fok", "market", "off_tick", "dust", "reduce_only", "stop_limit", "deep_low_then_high"] {
        let (mk, t, x) = (addr(1), addr(2), addr(5));
        let (_d, mut ctx) = fresh(path, &[mk]);
        run(&mut ctx, path, &[place(mk, limit(1, true, 101, 10)), place(mk, limit(2, true, 101, 10)), place(mk, limit(2, false, 130, 10))]);
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
        assert_eq!(pos_in(&ctx, &x, 2), FixedPoint::ZERO, "{what}");
        assert_bal(&ctx, &t, fp_cents(11_040), FixedPoint::ZERO, &what);
    }
}
per_path!(in_batch_griefer_bids_do_not_top_up_a_non_pool_sell);

/// Same-batch bound, soft: the top-up is never a placement gate and takes
/// only free margin left after the sender's whole Phase-2 fold, all or
/// nothing. M bids 10 @101 in m2 and m3 and asks 10 @130 in m2; B's funded
/// bid 10 @105 in m2; T = [pool order (10), sell 10 @100 in m2, sell 10
/// @100 in m3] (each 50.5 at the start bid). T 112 (free 1 < top-up 2): no
/// top-up — nothing rejected, m3 fills 10, the m2 sell (budget 50.5) fills
/// 2 @105 (IM(105 q + 100 (10 − q)) <= 50.5) and the rest is cancelled, as
/// before the bound. T 114 (free 3): topped up, m2 fills 10 @105.
fn same_batch_top_up_is_soft_and_all_or_nothing(path: Path) {
    for (funded, m2) in [(112, -fp(2)), (114, -fp(10))] {
        let (mk, t, b) = (addr(1), addr(2), addr(4));
        let (_d, mut ctx) = fresh(path, &[mk, b]);
        run(&mut ctx, path, &[place(mk, limit(2, true, 101, 10)), place(mk, limit(3, true, 101, 10)), place(mk, limit(2, false, 130, 10))]);
        fund_native(&ctx, &t, fp(funded));
        let r = run(
            &mut ctx,
            path,
            &[place(t, pool_order()), place(b, limit(2, true, 105, 10)), place(t, limit(2, false, 100, 10)), place(t, limit(3, false, 100, 10))],
        );
        let what = format!("{path:?} T={funded}");
        assert!(r.iter().all(|x| x.success), "{what}: {r:?}");
        assert_eq!((pos_in(&ctx, &t, 2), pos_in(&ctx, &t, 3)), (m2, -fp(10)), "{what}");
        assert_bal(&ctx, &t, fp(funded - 10), fp(10), &what);
    }
}
batch_paths!(same_batch_top_up_is_soft_and_all_or_nothing);

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

/// Option B §7.3 #15 (A5 telescoping under B): a seeded multi-market run
/// shaped like golden scenario A (40 senders x 12 markets, thin to rich
/// balances, aggressive GTC / IOC / market sells and buys, reduce-only
/// orders, cancel-alls), 8 blocks, serial and engine-forced (4 threads).
/// After every block each sender's `order_margin` equals Σ reserve(price,
/// remaining) over its resting rows exactly — B raises a non-pool sell's
/// reservation, but every release is computed at the limit, so none is
/// stranded or over-released.
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
    let mut totals = Vec::new();
    for threads in [1usize, 4] {
        let (_d, db) = open_test_db();
        let mut ctx = make_ctx(db);
        for (i, s) in senders.iter().enumerate() {
            fund_native(&ctx, s, fp([1_600, 4_000, 12_000, 100_000_000][i % 4]));
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
        }
        assert!(fills > 0 && checked == 8 * senders.len(), "threads={threads}: non-vacuous");
        let state: Vec<(FixedPoint, FixedPoint)> =
            senders.iter().map(|s| bal(&ctx, s)).map(|b| (b.available, b.order_margin)).collect();
        totals.push(state);
    }
    assert_eq!(totals[0], totals[1], "serial == engine");
}
