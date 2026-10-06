//! s515 review F3: `ModifyOrder` hardening.
//!
//! Pre-fix the executor dispatched `ModifyOrder` without the sender, so any
//! account could reprice / resize anyone's resting order (margin charged to
//! the victim); `price * qty` used the panicking Mul on user input (chain
//! halt); nothing validated the new price / quantity (zero, off-tick, below
//! lot, crossing the book); the margin check ran AFTER the book was modified
//! (insufficient margin left the order modified with no reservation); a
//! reduce-only order could be grown past its position; and the margin delta
//! used an ad-hoc formula instead of placement's `reserve_for_qty_cfg`.
//!
//! Every scenario runs through the single-action path and the batch Phase-1
//! path (a `ModifyOrder` is a non-place action in `execute_batch`).

use alloy_primitives::Address;

use torus_bridge::native_executor::{NativeActionResult, NativeExecContext, NativeExecutor};
use torus_core::position::NativeBalance;
use torus_state::StateDb;
use torus_types::{
    FixedPoint, MarketId, NativeAction, OrderType, PlaceOrderParams, TimeInForce,
};

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
        1,
        1000,
        0,
        100,
        10,
        addr(99),
        addr(100),
        addr(101),
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

fn place(sender: Address, p: PlaceOrderParams) -> (Address, NativeAction) {
    (sender, NativeAction::PlaceOrder(p))
}

fn modify(
    sender: Address,
    order_id: u128,
    new_price: Option<FixedPoint>,
    new_qty: Option<FixedPoint>,
) -> (Address, NativeAction) {
    (
        sender,
        NativeAction::ModifyOrder {
            order_id,
            new_price,
            new_qty,
        },
    )
}

fn bal(ctx: &NativeExecContext, trader: &Address) -> NativeBalance {
    ctx.positions.get_native_balance(trader).unwrap()
}

fn assert_bal(ctx: &NativeExecContext, trader: &Address, avail: FixedPoint, margin: FixedPoint, what: &str) {
    let b = bal(ctx, trader);
    assert_eq!(b.available, avail, "{what}: available");
    assert_eq!(b.order_margin, margin, "{what}: order_margin");
}

/// `(price, remaining)` of a resting order in market 1.
fn order(ctx: &NativeExecContext, id: u128) -> Option<(FixedPoint, FixedPoint)> {
    ctx.order_books
        .get(&1)
        .and_then(|b| b.get_order(id))
        .map(|o| (o.price, o.remaining_qty))
}

const FUNDING: i64 = 1_000;

#[derive(Clone, Copy, Debug)]
enum Path {
    Single,
    Batch,
}

const PATHS: [Path; 2] = [Path::Single, Path::Batch];

fn run(ctx: &mut NativeExecContext, path: Path, actions: &[(Address, NativeAction)]) -> Vec<NativeActionResult> {
    match path {
        Path::Single => actions
            .iter()
            .map(|(s, a)| NativeExecutor::execute(ctx, s, a))
            .collect(),
        Path::Batch => NativeExecutor::execute_batch_engine_mode(ctx, actions, 1).results,
    }
}

fn fresh(traders: &[Address]) -> (tempfile::TempDir, NativeExecContext) {
    let (dir, db) = open_test_db();
    let ctx = make_ctx(db);
    for t in traders {
        fund_native(&ctx, t, fp(FUNDING));
    }
    (dir, ctx)
}

/// Place one order through `path`, returning its id.
fn place_one(ctx: &mut NativeExecContext, path: Path, sender: Address, p: PlaceOrderParams) -> u128 {
    let id = ctx.next_global_order_id;
    let r = run(ctx, path, &[place(sender, p)]);
    assert!(r[0].success, "{path:?}: place failed: {:?}", r[0].error);
    id
}

/// Owner bid 100 x 4 resting (reserved 100*4/20 = 20).
fn owner_with_bid(path: Path) -> (tempfile::TempDir, NativeExecContext, Address, u128) {
    let owner = addr(1);
    let (d, mut ctx) = fresh(&[owner, addr(2), addr(3)]);
    let id = place_one(&mut ctx, path, owner, limit(1, true, 100, 4));
    assert_bal(&ctx, &owner, fp(FUNDING - 20), fp(20), "owner reserved");
    (d, ctx, owner, id)
}

#[test]
fn modify_by_other_trader_is_rejected_and_changes_nothing() {
    for path in PATHS {
        let (_d, mut ctx, owner, id) = owner_with_bid(path);
        let attacker = addr(2);
        for (p, q) in [
            (Some(fp(50)), None),
            (None, Some(fp(8))),
            (None, Some(fp(1))),
            (Some(fp(90)), Some(fp(2))),
        ] {
            let r = run(&mut ctx, path, &[modify(attacker, id, p, q)]);
            assert!(!r[0].success, "{path:?}: foreign modify must be rejected");
            let err = r[0].error.as_deref().unwrap_or("");
            assert!(err.contains("belongs to"), "{path:?}: unexpected error {err:?}");
            assert_eq!(order(&ctx, id), Some((fp(100), fp(4))), "{path:?}: order untouched");
            assert_bal(&ctx, &owner, fp(FUNDING - 20), fp(20), "owner untouched");
            assert_bal(&ctx, &attacker, fp(FUNDING), FixedPoint::ZERO, "attacker untouched");
        }
    }
}

#[test]
fn owner_modify_reprices_and_resizes_with_exact_margin() {
    for path in PATHS {
        let (_d, mut ctx, owner, id) = owner_with_bid(path);
        // Price 90, qty 6 -> 90*6/20 = 27.
        let r = run(&mut ctx, path, &[modify(owner, id, Some(fp(90)), Some(fp(6)))]);
        assert!(r[0].success, "{path:?}: {:?}", r[0].error);
        assert_eq!(order(&ctx, id), Some((fp(90), fp(6))), "{path:?}");
        assert_bal(&ctx, &owner, fp(FUNDING - 27), fp(27), "grown");
        // Qty-only decrease -> 90*2/20 = 9.
        let r = run(&mut ctx, path, &[modify(owner, id, None, Some(fp(2)))]);
        assert!(r[0].success, "{path:?}: {:?}", r[0].error);
        assert_eq!(order(&ctx, id), Some((fp(90), fp(2))), "{path:?}");
        assert_bal(&ctx, &owner, fp(FUNDING - 9), fp(9), "shrunk");
    }
}

#[test]
fn modify_with_overflowing_notional_is_rejected_without_panic() {
    // Whole units (on-tick, >= lot); x 4 (or x 100) overflows i128 raw.
    let huge = FixedPoint::from_raw(i128::MAX / 2 / FixedPoint::SCALE * FixedPoint::SCALE);
    for path in PATHS {
        let (_d, mut ctx, owner, id) = owner_with_bid(path);
        for (p, q) in [(Some(huge), None), (None, Some(huge)), (Some(huge), Some(huge))] {
            let r = run(&mut ctx, path, &[modify(owner, id, p, q)]);
            assert!(!r[0].success, "{path:?}: overflow must be rejected");
            let err = r[0].error.as_deref().unwrap_or("");
            assert!(err.contains("overflow"), "{path:?}: unexpected error {err:?}");
            assert_eq!(order(&ctx, id), Some((fp(100), fp(4))), "{path:?}: order untouched");
            assert_bal(&ctx, &owner, fp(FUNDING - 20), fp(20), "owner untouched");
        }
    }
}

#[test]
fn modify_with_invalid_price_or_quantity_is_rejected() {
    let half = FixedPoint::from_raw(FixedPoint::SCALE / 2);
    for path in PATHS {
        let (_d, mut ctx, owner, id) = owner_with_bid(path);
        for (p, q, what) in [
            (Some(FixedPoint::ZERO), None, "price"),
            (Some(-fp(5)), None, "price"),
            (Some(fp(100) + half), None, "tick"),
            (None, Some(FixedPoint::ZERO), "quantity"),
            (None, Some(half), "lot"),
            (None, None, "nothing to modify"),
        ] {
            let r = run(&mut ctx, path, &[modify(owner, id, p, q)]);
            assert!(!r[0].success, "{path:?} {what}: must be rejected");
            let err = r[0].error.as_deref().unwrap_or("");
            assert!(err.contains(what), "{path:?} {what}: unexpected error {err:?}");
            assert_eq!(order(&ctx, id), Some((fp(100), fp(4))), "{path:?} {what}: untouched");
            assert_bal(&ctx, &owner, fp(FUNDING - 20), fp(20), "owner untouched");
        }
    }
}

/// A modify that would cross the book is rejected (it never matches).
#[test]
fn modify_that_would_cross_is_rejected() {
    for path in PATHS {
        let (_d, mut ctx, owner, id) = owner_with_bid(path);
        let maker = addr(3);
        let ask = place_one(&mut ctx, path, maker, limit(1, false, 110, 2));
        for price in [110, 120] {
            let r = run(&mut ctx, path, &[modify(owner, id, Some(fp(price)), None)]);
            assert!(!r[0].success, "{path:?}: crossing modify must be rejected");
            let err = r[0].error.as_deref().unwrap_or("");
            assert!(err.contains("cross"), "{path:?}: unexpected error {err:?}");
        }
        assert_eq!(order(&ctx, id), Some((fp(100), fp(4))), "{path:?}: bid untouched");
        assert_eq!(order(&ctx, ask), Some((fp(110), fp(2))), "{path:?}: ask untouched");
        assert_bal(&ctx, &owner, fp(FUNDING - 20), fp(20), "owner untouched");
        // Just below the ask is fine.
        let r = run(&mut ctx, path, &[modify(owner, id, Some(fp(109)), None)]);
        assert!(r[0].success, "{path:?}: {:?}", r[0].error);
    }
}

/// Insufficient margin: rejected BEFORE the book changes — order and balances
/// exactly as before.
#[test]
fn modify_with_insufficient_margin_changes_nothing() {
    for path in PATHS {
        let owner = addr(1);
        let (_d, mut ctx) = fresh(&[]);
        fund_native(&ctx, &owner, fp(25));
        let id = place_one(&mut ctx, path, owner, limit(1, true, 100, 4)); // reserves 20
        assert_bal(&ctx, &owner, fp(5), fp(20), "reserved");
        // 100 * 6 / 20 = 30 -> delta 10 > 5 available.
        let r = run(&mut ctx, path, &[modify(owner, id, None, Some(fp(6)))]);
        assert!(!r[0].success, "{path:?}: must be rejected");
        let err = r[0].error.as_deref().unwrap_or("");
        assert!(err.contains("insufficient margin"), "{path:?}: unexpected error {err:?}");
        assert_eq!(order(&ctx, id), Some((fp(100), fp(4))), "{path:?}: book unchanged");
        assert_bal(&ctx, &owner, fp(5), fp(20), "balances unchanged");
    }
}

/// A reduce-only order cannot be grown past the position: clamped to it
/// (Hyperliquid resizes), reserving only the clamped size.
#[test]
fn reduce_only_modify_is_clamped_to_position() {
    for path in PATHS {
        let t = addr(1);
        let maker = addr(2);
        let (_d, mut ctx) = fresh(&[t, maker]);
        // t goes long 5 @100.
        place_one(&mut ctx, path, maker, limit(1, false, 100, 5));
        let r = run(&mut ctx, path, &[place(t, limit(1, true, 100, 5))]);
        assert!(r[0].success, "{path:?}: {:?}", r[0].error);
        // Reduce-only sell 2 @200 rests (200*2/20 = 20).
        let ro = place_one(
            &mut ctx,
            path,
            t,
            PlaceOrderParams {
                reduce_only: true,
                ..limit(1, false, 200, 2)
            },
        );
        let before = bal(&ctx, &t);
        let r = run(&mut ctx, path, &[modify(t, ro, None, Some(fp(50)))]);
        assert!(r[0].success, "{path:?}: {:?}", r[0].error);
        assert_eq!(order(&ctx, ro), Some((fp(200), fp(5))), "{path:?}: clamped to position");
        // 200*5/20 = 50 reserved in total -> +30.
        let after = bal(&ctx, &t);
        assert_eq!(after.order_margin - before.order_margin, fp(30), "{path:?}");
        assert_eq!(before.available - after.available, fp(30), "{path:?}");
    }
}

/// Row 52 (s94 B): modifying a reduce-only order when the position is gone
/// is refused with its own reason, `ReduceOnly` (was `Other`).
#[test]
fn reduce_only_modify_without_position_has_its_own_reason() {
    use torus_state::action_status::FailureReason;
    for path in PATHS {
        let t = addr(1);
        let maker = addr(2);
        let (_d, mut ctx) = fresh(&[t, maker]);
        place_one(&mut ctx, path, maker, limit(1, false, 100, 5));
        let r = run(&mut ctx, path, &[place(t, limit(1, true, 100, 5))]);
        assert!(r[0].success, "{path:?}: {:?}", r[0].error);
        let ro = place_one(&mut ctx, path, t, PlaceOrderParams { reduce_only: true, ..limit(1, false, 200, 2) });
        // The position is gone; the resting reduce-only order is not re-policed yet.
        ctx.positions.delete_position(&t, 1).unwrap();
        let r = run(&mut ctx, path, &[modify(t, ro, None, Some(fp(1)))]);
        assert!(!r[0].success, "{path:?}: must be rejected");
        assert_eq!(r[0].error.as_deref(), Some("reduce-only order rejected: no position to reduce"), "{path:?}");
        assert_eq!(r[0].reason, FailureReason::ReduceOnly, "{path:?}");
    }
}

/// Reserve, modify (up, down, reprice), cancel: the order's whole reservation
/// comes back — order_margin returns exactly to 0, no drift.
#[test]
fn reserve_modify_cancel_returns_margin_exactly() {
    // A price / qty whose notional / 20 has a remainder in raw units.
    let odd = FixedPoint::from_raw(3 * FixedPoint::SCALE + 7);
    for path in PATHS {
        let owner = addr(1);
        let (_d, mut ctx) = fresh(&[owner]);
        let id = place_one(&mut ctx, path, owner, limit(1, true, 97, 3));
        for (p, q) in [
            (None, Some(odd)),
            (Some(fp(93)), None),
            (None, Some(fp(2))),
            (Some(fp(99)), Some(fp(7))),
            (None, Some(fp(1))),
        ] {
            let r = run(&mut ctx, path, &[modify(owner, id, p, q)]);
            assert!(r[0].success, "{path:?}: {:?}", r[0].error);
        }
        let r = run(&mut ctx, path, &[(owner, NativeAction::CancelOrder { order_id: id })]);
        assert!(r[0].success, "{path:?}: {:?}", r[0].error);
        assert_bal(&ctx, &owner, fp(FUNDING), FixedPoint::ZERO, "all margin back");
    }
}

/// s515 review 3 (item 5a): only a NEW price is tick-checked. A resting order
/// whose price is off the book's current tick (tick changed after it rested,
/// or a legacy row) can still be resized; repricing it must land on the tick.
#[test]
fn qty_only_modify_of_off_tick_order_is_accepted() {
    for path in PATHS {
        let (_d, mut ctx, owner, id) = owner_with_bid(path);
        ctx.order_books.get_mut(&1).unwrap().tick_size = fp(3); // 100 is now off-tick
        let r = run(&mut ctx, path, &[modify(owner, id, None, Some(fp(2)))]);
        assert!(r[0].success, "{path:?}: qty-only modify: {:?}", r[0].error);
        assert_eq!(order(&ctx, id), Some((fp(100), fp(2))), "{path:?}");
        // Same price re-sent is not a new price either.
        let r = run(&mut ctx, path, &[modify(owner, id, Some(fp(100)), Some(fp(3)))]);
        assert!(r[0].success, "{path:?}: unchanged price: {:?}", r[0].error);
        assert_eq!(order(&ctx, id), Some((fp(100), fp(3))), "{path:?}");
        // A new off-tick price is still rejected; an on-tick one is accepted.
        let r = run(&mut ctx, path, &[modify(owner, id, Some(fp(98)), None)]);
        assert!(!r[0].success, "{path:?}: new off-tick price must be rejected");
        let r = run(&mut ctx, path, &[modify(owner, id, Some(fp(99)), None)]);
        assert!(r[0].success, "{path:?}: {:?}", r[0].error);
        assert_eq!(order(&ctx, id), Some((fp(99), fp(3))), "{path:?}");
        // 99*3/20 = 14.85 reserved.
        let r = run(&mut ctx, path, &[(owner, NativeAction::CancelOrder { order_id: id })]);
        assert!(r[0].success, "{path:?}: {:?}", r[0].error);
        assert_bal(&ctx, &owner, fp(FUNDING), FixedPoint::ZERO, "all margin back");
    }
}

/// s515 review 3 (item 5b): the reduce-only clamp mirrors placement — the
/// lot applies to the REQUESTED quantity, the clamp to the position may land
/// below the lot (placement rests such an order too; it closes the position
/// exactly).
#[test]
fn reduce_only_modify_clamp_below_lot_mirrors_placement() {
    for path in PATHS {
        let t = addr(1);
        let maker = addr(2);
        let (_d, mut ctx) = fresh(&[t, maker]);
        // t goes long 5 @100, then the lot becomes 10.
        place_one(&mut ctx, path, maker, limit(1, false, 100, 5));
        let r = run(&mut ctx, path, &[place(t, limit(1, true, 100, 5))]);
        assert!(r[0].success, "{path:?}: {:?}", r[0].error);
        ctx.order_books.get_mut(&1).unwrap().lot_size = fp(10);
        let ro_sell = |qty| PlaceOrderParams {
            reduce_only: true,
            ..limit(1, false, 200, qty)
        };
        // Placement: 20 (>= lot) clamped to the position 5 (< lot) rests.
        let ro = place_one(&mut ctx, path, t, ro_sell(20));
        assert_eq!(order(&ctx, ro), Some((fp(200), fp(5))), "{path:?}: placement clamp");
        // Modify: same convention.
        let r = run(&mut ctx, path, &[modify(t, ro, Some(fp(210)), Some(fp(30)))]);
        assert!(r[0].success, "{path:?}: {:?}", r[0].error);
        assert_eq!(order(&ctx, ro), Some((fp(210), fp(5))), "{path:?}: modify clamp");
        // A REQUESTED quantity below the lot is rejected, as placement does.
        let r = run(&mut ctx, path, &[modify(t, ro, None, Some(fp(4)))]);
        assert!(!r[0].success, "{path:?}: requested qty below lot");
        assert_eq!(order(&ctx, ro), Some((fp(210), fp(5))), "{path:?}: unchanged");
        let before = ctx.next_global_order_id;
        let r = run(&mut ctx, path, &[place(t, ro_sell(4))]);
        let rests = ctx
            .order_books
            .get(&1)
            .unwrap()
            .get_order(before)
            .is_some();
        assert!(!r[0].success || !rests, "{path:?}: placement rejects it too");
    }
}

// ============================================================================
// F1 (s517): a modify's extra reservation is checked against the ACCOUNT
// ============================================================================

/// Publish `price` as market `market_id`'s aggregated oracle (mark) price
/// (copied from market_order_margin_tests.rs).
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

/// `t` (addr 2) funded 100, long 10 @100 in m2 (IM 50) against addr 3, and
/// a resting bid 1 @90 in m1 (reserves 4.5) → available 95.5. Returns the
/// bid's id.
fn long_m2_with_bid_m1(ctx: &mut NativeExecContext) -> u128 {
    let (t, cp) = (addr(2), addr(3));
    fund_native(ctx, &cp, fp(1_000));
    fund_native(ctx, &t, fp(100));
    NativeExecutor::execute(ctx, &cp, &NativeAction::PlaceOrder(limit(2, false, 100, 10)));
    assert!(NativeExecutor::execute(ctx, &t, &NativeAction::PlaceOrder(limit(2, true, 100, 10))).success);
    assert!(NativeExecutor::execute(ctx, &t, &NativeAction::PlaceOrder(limit(1, true, 90, 1))).success);
    ctx.order_books[&1].orders_for_trader(&t)[0].id
}

/// F1 (s517): a modify's extra reservation must fit the account's free
/// margin, not just `available`. Free = 95.5 − 50 = 45.5: qty 11 (+45) ok;
/// qty 12 (+49.5) rejected (was accepted: 49.5 <= available 95.5).
#[test]
fn modify_extra_reservation_needs_account_free_margin() {
    for (qty, ok) in [(11, true), (12, false)] {
        let (_d, db) = open_test_db();
        let mut ctx = make_ctx(db);
        let id = long_m2_with_bid_m1(&mut ctx);
        let (s, a) = modify(addr(2), id, None, Some(fp(qty)));
        let r = NativeExecutor::execute(&mut ctx, &s, &a);
        assert_eq!(r.success, ok, "qty {qty}: {:?}", r.error);
    }
}

/// F1 / D1 (strict HL): UPnL funds a modify beyond cash. Mark m2 at 150:
/// UPnL +500, IM 75 → free 95.5 + 425 = 520.5. Modify the bid to 100 @90
/// (reservation 450, extra 445.5 > available 95.5): ACCEPTED, available
/// −350 (the removed `available < extra` gate rejected it).
#[test]
fn modify_extra_reservation_can_be_funded_by_upnl() {
    let (_d, db) = open_test_db();
    let mut ctx = make_ctx(db);
    let id = long_m2_with_bid_m1(&mut ctx);
    set_mark(&ctx, 2, fp(150));
    let (s, a) = modify(addr(2), id, None, Some(fp(100)));
    let r = NativeExecutor::execute(&mut ctx, &s, &a);
    assert!(r.success, "{:?}", r.error);
    assert_bal(&ctx, &addr(2), -fp(350), fp(450), "UPnL-funded");
}

/// Review fix 3 (s517): a modify is gated on the POSITION-tier need change
/// (`placement_need(new) − placement_need(old)`), not the reservation delta.
/// m1 tiers 20x <= 1,000, 5x above. `t` (100) long 5 @100 in m1 (IM 25),
/// bid 4 @100 (need IM(900) − IM(500) = 20, reserves 20) → available 80,
/// free 55. Bid → 6: need IM(1,100) − 25 = 195, +175 > 55 → REJECTED (the
/// reservation delta is only 10; a fresh buy 6 @100 on this account would
/// need 195 too). Bid → 5: need IM(1,000) − 25 = 25, +5 <= 55 → accepted.
#[test]
fn modify_is_gated_on_the_position_tier_need() {
    for (qty, ok) in [(6, false), (5, true)] {
        let (_d, db) = open_test_db();
        let mut ctx = make_ctx(db);
        let mut c = torus_core::margin::MarketMarginConfig::new(1, 999);
        c.tiers = vec![
            torus_core::margin::MarginTier { max_notional: fp(1_000), max_leverage: 20 },
            torus_core::margin::MarginTier { max_notional: FixedPoint::MAX, max_leverage: 5 },
        ];
        ctx.margin_configs.insert(1, c);
        let (t, cp) = (addr(2), addr(3));
        fund_native(&ctx, &cp, fp(1_000));
        fund_native(&ctx, &t, fp(100));
        NativeExecutor::execute(&mut ctx, &cp, &NativeAction::PlaceOrder(limit(1, false, 100, 5)));
        assert!(NativeExecutor::execute(&mut ctx, &t, &NativeAction::PlaceOrder(limit(1, true, 100, 5))).success);
        assert!(NativeExecutor::execute(&mut ctx, &t, &NativeAction::PlaceOrder(limit(1, true, 100, 4))).success);
        assert_bal(&ctx, &t, fp(80), fp(20), "setup");
        let id = ctx.order_books[&1].orders_for_trader(&t)[0].id;
        let (s, a) = modify(t, id, None, Some(fp(qty)));
        let r = NativeExecutor::execute(&mut ctx, &s, &a);
        assert_eq!(r.success, ok, "qty {qty}: {:?}", r.error);
    }
}

/// Review fix 5 (s517): the modify gate credits the old order's full
/// reservation (cancel-and-replace equivalence), not just its need. Flat
/// 10x: long 10 @100 (IM 100) with 200 available (free 100); GTC sell 15
/// @100 rests (need IM(1,500) − 100 = 50, reserves 150 → available 50,
/// free −50). Reprice to 101: need 50.5 − max(50, 150) < 0 → ACCEPTED
/// (was: 50.5 − 50 = 0.5 > −50 → rejected, though cancel + place passes:
/// 50.5 <= 100). Extra reservation 1.5 → 48.5 / 151.5.
#[test]
fn modify_credits_the_old_reservation() {
    let (_d, db) = open_test_db();
    let mut ctx = make_ctx(db);
    let mut c = torus_core::margin::MarketMarginConfig::new(1, 999);
    c.tiers = vec![torus_core::margin::MarginTier { max_notional: FixedPoint::MAX, max_leverage: 10 }];
    ctx.margin_configs.insert(1, c);
    let (t, cp) = (addr(2), addr(3));
    fund_native(&ctx, &cp, fp(1_000));
    fund_native(&ctx, &t, fp(200));
    NativeExecutor::execute(&mut ctx, &cp, &NativeAction::PlaceOrder(limit(1, false, 100, 10)));
    assert!(NativeExecutor::execute(&mut ctx, &t, &NativeAction::PlaceOrder(limit(1, true, 100, 10))).success);
    assert!(NativeExecutor::execute(&mut ctx, &t, &NativeAction::PlaceOrder(limit(1, false, 100, 15))).success);
    assert_bal(&ctx, &t, fp(50), fp(150), "setup");
    let id = ctx.order_books[&1].orders_for_trader(&t)[0].id;
    let (s, a) = modify(t, id, Some(fp(101)), None);
    let r = NativeExecutor::execute(&mut ctx, &s, &a);
    assert!(r.success, "{:?}", r.error);
    assert_bal(&ctx, &t, FixedPoint::from_raw(4_850_000_000), FixedPoint::from_raw(15_150_000_000), "repriced");
}
