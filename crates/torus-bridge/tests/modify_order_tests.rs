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
