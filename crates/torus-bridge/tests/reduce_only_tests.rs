//! s515 parity-audit BUG 2: `reduce_only` was stored on every order but never
//! enforced anywhere.
//!
//! Hyperliquid parity: a reduce-only order can only ever REDUCE the trader's
//! position in that market —
//!   (a) placement with no position, or on the increasing side, is rejected;
//!       an oversize reduce-only order is clamped to the position size;
//!   (b) a triggered reduce-only stop (TP/SL) is re-checked against the
//!       position at trigger time;
//!   (c) resting reduce-only orders are shrunk / cancelled whenever the
//!       position shrinks, closes, or flips (their margin released exactly);
//!   (d) at match time a reduce-only order (taker or maker) never fills more
//!       than the position it reduces.
//!
//! Every scenario runs through the single-action path, `execute_batch` serial
//! Phase 2, and the sharded parallel Phase 2.

use alloy_primitives::Address;

use torus_bridge::native_executor::{NativeActionResult, NativeExecContext, NativeExecutor};
use torus_core::position::NativeBalance;
use torus_state::StateDb;
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

fn ro(mut p: PlaceOrderParams) -> PlaceOrderParams {
    p.reduce_only = true;
    p
}

fn ro_stop_sell(trigger: i64, cap: i64, qty: i64) -> PlaceOrderParams {
    PlaceOrderParams {
        market_id: 1,
        is_buy: false,
        price: fp(cap),
        quantity: fp(qty),
        order_type: OrderType::StopMarket { trigger: fp(trigger) },
        time_in_force: TimeInForce::IOC,
        reduce_only: true,
        client_order_id: None,
    }
}

fn place(sender: Address, p: PlaceOrderParams) -> (Address, NativeAction) {
    (sender, NativeAction::PlaceOrder(p))
}

fn assert_bal(
    ctx: &NativeExecContext,
    trader: &Address,
    avail: FixedPoint,
    margin: FixedPoint,
    what: &str,
) {
    let b = ctx.positions.get_native_balance(trader).unwrap();
    assert_eq!(b.available, avail, "{what}: available");
    assert_eq!(b.order_margin, margin, "{what}: order_margin");
}

/// Signed position size (+long / -short / 0 flat) of `trader` in market 1.
fn pos(ctx: &NativeExecContext, trader: &Address) -> FixedPoint {
    match ctx.positions.get_position(trader, 1).unwrap() {
        Some(p) if p.is_long => p.size,
        Some(p) => -p.size,
        None => FixedPoint::ZERO,
    }
}

/// Remaining quantities of `trader`'s resting orders in market 1 (sorted).
fn resting(ctx: &NativeExecContext, trader: &Address) -> Vec<FixedPoint> {
    ctx.order_books
        .get(&1)
        .map(|b| {
            let mut v: Vec<_> = b
                .orders_for_trader(trader)
                .iter()
                .map(|o| o.remaining_qty)
                .collect();
            v.sort();
            v
        })
        .unwrap_or_default()
}

const FUNDING: i64 = 1_000;

fn filler() -> Address {
    addr(200)
}

#[derive(Clone, Copy, Debug)]
enum Path {
    Single,
    Batch,
    Parallel,
}

const PATHS: [Path; 3] = [Path::Single, Path::Batch, Path::Parallel];

fn run(ctx: &mut NativeExecContext, path: Path, actions: &[(Address, NativeAction)]) -> Vec<NativeActionResult> {
    let r = match path {
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
    };
    assert!(ctx.fatal_error.is_none(), "{path:?}: fatal {:?}", ctx.fatal_error);
    r
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

/// `t` goes long 4 @100 against maker `m` (two separate calls).
fn open_long_4(ctx: &mut NativeExecContext, path: Path, t: Address, m: Address) {
    let r = run(ctx, path, &[place(m, limit(1, false, 100, 4))]);
    assert!(r[0].success, "{path:?}: {:?}", r[0].error);
    let r = run(ctx, path, &[place(t, limit(1, true, 100, 4))]);
    assert!(r[0].success, "{path:?}: {:?}", r[0].error);
    assert_eq!(pos(ctx, &t), fp(4), "{path:?}: setup long 4");
}

// ============================================================================
// (a) placement
// ============================================================================

#[test]
fn reduce_only_without_position_rejected() {
    for path in PATHS {
        let t = addr(1);
        let m = addr(2);
        let (_d, mut ctx) = fresh(path, &[t, m]);
        run(&mut ctx, path, &[place(m, limit(1, true, 100, 4))]);

        let r = run(&mut ctx, path, &[place(t, ro(limit(1, false, 100, 4)))]);
        if matches!(path, Path::Single) {
            assert!(!r[0].success, "single: must be rejected");
            let err = r[0].error.as_deref().unwrap_or("");
            assert!(err.contains("reduce-only"), "unexpected error {err:?}");
            // Row 52 (s94 B): its own reason (was `Other`).
            assert_eq!(r[0].reason, torus_state::action_status::FailureReason::ReduceOnly);
        }
        assert_eq!(pos(&ctx, &t), FixedPoint::ZERO, "{path:?}: must not open a short");
        assert_eq!(resting(&ctx, &m), vec![fp(4)], "{path:?}: bid untouched");
        assert!(resting(&ctx, &t).is_empty(), "{path:?}: nothing rests");
        assert_bal(&ctx, &t, fp(FUNDING), FixedPoint::ZERO, "trader");
    }
}

#[test]
fn reduce_only_on_increasing_side_rejected() {
    for path in PATHS {
        let t = addr(1);
        let m1 = addr(2);
        let m2 = addr(3);
        let (_d, mut ctx) = fresh(path, &[t, m1, m2]);
        open_long_4(&mut ctx, path, t, m1);
        run(&mut ctx, path, &[place(m2, limit(1, false, 100, 4))]);

        let r = run(&mut ctx, path, &[place(t, ro(limit(1, true, 100, 2)))]);
        if matches!(path, Path::Single) {
            assert!(!r[0].success, "single: must be rejected");
            let err = r[0].error.as_deref().unwrap_or("");
            assert!(err.contains("reduce-only"), "unexpected error {err:?}");
            // Row 52 (s94 B): its own reason (was `Other`).
            assert_eq!(r[0].reason, torus_state::action_status::FailureReason::ReduceOnly);
        }
        assert_eq!(pos(&ctx, &t), fp(4), "{path:?}: long must not increase");
        assert_eq!(resting(&ctx, &m2), vec![fp(4)], "{path:?}: ask untouched");
        assert_bal(&ctx, &t, fp(FUNDING), FixedPoint::ZERO, "trader");
    }
}

#[test]
fn reduce_only_exact_close_fills_to_flat() {
    for path in PATHS {
        let t = addr(1);
        let m1 = addr(2);
        let m2 = addr(3);
        let (_d, mut ctx) = fresh(path, &[t, m1, m2]);
        open_long_4(&mut ctx, path, t, m1);
        run(&mut ctx, path, &[place(m2, limit(1, true, 100, 4))]);

        let r = run(&mut ctx, path, &[place(t, ro(limit(1, false, 100, 4)))]);
        assert!(r[0].success, "{path:?}: {:?}", r[0].error);
        assert_eq!(pos(&ctx, &t), FixedPoint::ZERO, "{path:?}: closed");
        assert_eq!(pos(&ctx, &m2), fp(4), "{path:?}: counterparty");
        assert_bal(&ctx, &t, fp(FUNDING), FixedPoint::ZERO, "trader");
    }
}

#[test]
fn reduce_only_oversize_is_clamped_to_position() {
    for path in PATHS {
        let t = addr(1);
        let m1 = addr(2);
        let m2 = addr(3);
        let (_d, mut ctx) = fresh(path, &[t, m1, m2]);
        open_long_4(&mut ctx, path, t, m1);
        run(&mut ctx, path, &[place(m2, limit(1, true, 100, 5))]);

        // RO sell 5 against a long 4: Hyperliquid clamps to the position.
        let r = run(&mut ctx, path, &[place(t, ro(limit(1, false, 100, 5)))]);
        assert!(r[0].success, "{path:?}: {:?}", r[0].error);
        assert_eq!(pos(&ctx, &t), FixedPoint::ZERO, "{path:?}: flat, never short");
        assert!(resting(&ctx, &t).is_empty(), "{path:?}: clamped remainder must not rest");
        assert_eq!(resting(&ctx, &m2), vec![fp(1)], "{path:?}: only 4 of the bid consumed");
        assert_bal(&ctx, &t, fp(FUNDING), FixedPoint::ZERO, "trader");
    }
}

#[test]
fn reduce_only_resting_oversize_is_clamped_to_position() {
    for path in PATHS {
        let t = addr(1);
        let m1 = addr(2);
        let (_d, mut ctx) = fresh(path, &[t, m1]);
        open_long_4(&mut ctx, path, t, m1);

        // RO sell 6 @110 rests, clamped to 4: reserve 110*4/20 = 22.
        let r = run(&mut ctx, path, &[place(t, ro(limit(1, false, 110, 6)))]);
        assert!(r[0].success, "{path:?}: {:?}", r[0].error);
        assert_eq!(resting(&ctx, &t), vec![fp(4)], "{path:?}: resized to position");
        assert_bal(&ctx, &t, fp(FUNDING - 22), fp(22), "trader");
    }
}

#[test]
fn reduce_only_after_open_in_same_batch_is_accepted() {
    // Freshest position: the RO close sees the fill of an EARLIER order in
    // the same block on every path (no pre-block staleness in batch mode).
    for path in PATHS {
        let t = addr(1);
        let m1 = addr(2);
        let m2 = addr(3);
        let (_d, mut ctx) = fresh(path, &[t, m1, m2]);
        let r = run(
            &mut ctx,
            path,
            &[
                place(m1, limit(1, false, 100, 4)),
                place(t, limit(1, true, 100, 4)),
                place(m2, limit(1, true, 100, 4)),
                place(t, ro(limit(1, false, 100, 6))),
            ],
        );
        assert!(r.iter().all(|x| x.success), "{path:?}: {r:?}");
        assert_eq!(pos(&ctx, &t), FixedPoint::ZERO, "{path:?}: open then close");
        assert!(resting(&ctx, &t).is_empty(), "{path:?}");
        assert_bal(&ctx, &t, fp(FUNDING), FixedPoint::ZERO, "trader");
    }
}

// ============================================================================
// (c) resting reduce-only orders track the position
// ============================================================================

#[test]
fn resting_reduce_only_cancelled_when_position_closed() {
    for path in PATHS {
        let t = addr(1);
        let m1 = addr(2);
        let m2 = addr(3);
        let (_d, mut ctx) = fresh(path, &[t, m1, m2]);
        open_long_4(&mut ctx, path, t, m1);
        run(&mut ctx, path, &[place(t, ro(limit(1, false, 110, 4)))]);
        assert_bal(&ctx, &t, fp(FUNDING - 22), fp(22), "RO reserved");
        run(&mut ctx, path, &[place(m2, limit(1, true, 100, 4))]);

        // Close the position with a plain (non-RO) sell.
        let r = run(&mut ctx, path, &[place(t, limit(1, false, 100, 4))]);
        assert!(r[0].success, "{path:?}: {:?}", r[0].error);
        assert_eq!(pos(&ctx, &t), FixedPoint::ZERO, "{path:?}");
        assert!(resting(&ctx, &t).is_empty(), "{path:?}: resting RO cancelled");
        assert_bal(&ctx, &t, fp(FUNDING), FixedPoint::ZERO, "RO margin released");
    }
}

#[test]
fn resting_reduce_only_shrunk_when_position_reduced() {
    for path in PATHS {
        let t = addr(1);
        let m1 = addr(2);
        let m2 = addr(3);
        let (_d, mut ctx) = fresh(path, &[t, m1, m2]);
        open_long_4(&mut ctx, path, t, m1);
        run(&mut ctx, path, &[place(t, ro(limit(1, false, 110, 4)))]);
        run(&mut ctx, path, &[place(m2, limit(1, true, 100, 2))]);

        let r = run(&mut ctx, path, &[place(t, limit(1, false, 100, 2))]);
        assert!(r[0].success, "{path:?}: {:?}", r[0].error);
        assert_eq!(pos(&ctx, &t), fp(2), "{path:?}");
        assert_eq!(resting(&ctx, &t), vec![fp(2)], "{path:?}: RO shrunk to 2");
        // 110*2/20 = 11 still reserved; the cut 11 released.
        assert_bal(&ctx, &t, fp(FUNDING - 11), fp(11), "RO margin partially released");
    }
}

#[test]
fn resting_reduce_only_cancelled_when_position_flips() {
    for path in PATHS {
        let t = addr(1);
        let m1 = addr(2);
        let m2 = addr(3);
        let (_d, mut ctx) = fresh(path, &[t, m1, m2]);
        open_long_4(&mut ctx, path, t, m1);
        run(&mut ctx, path, &[place(t, ro(limit(1, false, 110, 4)))]);
        run(&mut ctx, path, &[place(m2, limit(1, true, 100, 6))]);

        let r = run(&mut ctx, path, &[place(t, limit(1, false, 100, 6))]);
        assert!(r[0].success, "{path:?}: {:?}", r[0].error);
        assert_eq!(pos(&ctx, &t), -fp(2), "{path:?}: flipped short");
        assert!(resting(&ctx, &t).is_empty(), "{path:?}: RO sell would increase short");
        assert_bal(&ctx, &t, fp(FUNDING), FixedPoint::ZERO, "all margin released");
    }
}

// ============================================================================
// (d) match time: a reduce-only MAKER never fills past the position
// ============================================================================

#[test]
fn reduce_only_maker_capped_within_one_sweep() {
    for path in PATHS {
        let t = addr(1);
        let m1 = addr(2);
        let x = addr(3);
        let (_d, mut ctx) = fresh(path, &[t, m1, x]);
        open_long_4(&mut ctx, path, t, m1);
        // Plain sell 3 @100 and RO sell 4 @101 (valid: 4 <= long 4).
        run(
            &mut ctx,
            path,
            &[
                place(t, limit(1, false, 100, 3)),
                place(t, ro(limit(1, false, 101, 4))),
            ],
        );
        // One taker sweeps both: after the plain 3 the position is 1, so the
        // RO maker may fill only 1 and the rest is cut.
        let r = run(&mut ctx, path, &[place(x, limit(1, true, 101, 7))]);
        assert!(r[0].success, "{path:?}: {:?}", r[0].error);
        assert_eq!(pos(&ctx, &t), FixedPoint::ZERO, "{path:?}: never flips short");
        assert_eq!(pos(&ctx, &x), fp(4), "{path:?}");
        assert!(resting(&ctx, &t).is_empty(), "{path:?}: RO remainder cut");
        assert_eq!(resting(&ctx, &x), vec![fp(3)], "{path:?}: taker rests the rest");
        // Realized PnL: 3 closed @100 (0) + 1 closed @101 (+1) vs entry 100.
        assert_bal(&ctx, &t, fp(FUNDING + 1), FixedPoint::ZERO, "maker margin released");
    }
}

// ============================================================================
// (b) triggered reduce-only stops are re-checked at trigger time
// ============================================================================

#[test]
fn triggered_reduce_only_stop_after_position_closed_is_not_placed() {
    for path in PATHS {
        let t = addr(1);
        let m1 = addr(2);
        let m2 = addr(3);
        let m3 = addr(4);
        let m4 = addr(5);
        let x = addr(6);
        let (_d, mut ctx) = fresh(path, &[t, m1, m2, m3, m4, x]);
        open_long_4(&mut ctx, path, t, m1);
        // SL: sell stop trigger 95, cap 90 → reserve 90*4/20 = 18.
        let r = run(&mut ctx, path, &[place(t, ro_stop_sell(95, 90, 4))]);
        assert!(r[0].success, "{path:?}: {:?}", r[0].error);
        assert_bal(&ctx, &t, fp(FUNDING - 18), fp(18), "stop reserved");

        // Close the position at 100 (does not trigger 95).
        run(&mut ctx, path, &[place(m2, limit(1, true, 100, 4))]);
        run(&mut ctx, path, &[place(t, limit(1, false, 100, 4))]);
        assert_eq!(pos(&ctx, &t), FixedPoint::ZERO, "{path:?}: closed");

        // Trade at 95 fires the stop; the position is gone → not placed.
        run(
            &mut ctx,
            path,
            &[place(m3, limit(1, true, 95, 1)), place(m4, limit(1, true, 94, 4))],
        );
        let r = run(&mut ctx, path, &[place(x, limit(1, false, 95, 1))]);
        assert!(r[0].success, "{path:?}: {:?}", r[0].error);
        assert_eq!(ctx.order_books[&1].pending_stop_count(), 0, "{path:?}: stop fired");
        assert_eq!(pos(&ctx, &t), FixedPoint::ZERO, "{path:?}: must not open a short");
        assert_eq!(resting(&ctx, &m4), vec![fp(4)], "{path:?}: bid @94 untouched");
        assert_bal(&ctx, &t, fp(FUNDING), FixedPoint::ZERO, "stop reservation released");
    }
}

#[test]
fn triggered_reduce_only_stop_with_position_closes_it() {
    for path in PATHS {
        let t = addr(1);
        let m1 = addr(2);
        let m3 = addr(4);
        let m4 = addr(5);
        let x = addr(6);
        let (_d, mut ctx) = fresh(path, &[t, m1, m3, m4, x]);
        open_long_4(&mut ctx, path, t, m1);
        // RO stop for 6 against long 4 → clamped to 4 at trigger time.
        run(&mut ctx, path, &[place(t, ro_stop_sell(95, 90, 6))]);
        run(
            &mut ctx,
            path,
            &[place(m3, limit(1, true, 95, 1)), place(m4, limit(1, true, 94, 10))],
        );
        let r = run(&mut ctx, path, &[place(x, limit(1, false, 95, 1))]);
        assert!(r[0].success, "{path:?}: {:?}", r[0].error);
        assert_eq!(pos(&ctx, &t), FixedPoint::ZERO, "{path:?}: SL closed the long");
        assert_eq!(pos(&ctx, &m4), fp(4), "{path:?}: counterparty");
        assert_eq!(resting(&ctx, &m4), vec![fp(6)], "{path:?}: only 4 consumed");
        // Realized PnL: long 4 @100 closed @94 → -24.
        assert_bal(&ctx, &t, fp(FUNDING - 24), FixedPoint::ZERO, "stop settled");
    }
}

// ============================================================================
// F4 (s515 review): single-action vs batch agreement
// ============================================================================

/// (A) The batch path skipped the executor's reduce-only pre-check and the
/// book's stop branch returned before its own check, so a reduce-only
/// StopMarket from a FLAT trader rested as PendingTrigger in batch mode but
/// was rejected in single mode. Every path must reject it.
#[test]
fn reduce_only_stop_from_flat_trader_rejected_on_every_path() {
    for path in PATHS {
        let t = addr(1);
        let (_d, mut ctx) = fresh(path, &[t]);
        let r = run(&mut ctx, path, &[place(t, ro_stop_sell(95, 90, 4))]);
        // Batch results report book-level rejections as processed (as in
        // reduce_only_without_position_rejected); the book state is the test.
        if matches!(path, Path::Single) {
            assert!(!r[0].success, "single: flat reduce-only stop must be rejected");
        }
        let pending = ctx.order_books.get(&1).map_or(0, |b| b.pending_stop_count());
        assert_eq!(pending, 0, "{path:?}: nothing pending");
        assert_bal(&ctx, &t, fp(FUNDING), FixedPoint::ZERO, "trader");
    }
}

/// (B) A doomed reduce-only order (flat trader) reserved its FULL quantity in
/// batch Phase 2, starving the same sender's later order in the block, which
/// the single path accepts. 150 @100 would reserve 750, leaving 250 < 300.
#[test]
fn doomed_reduce_only_does_not_starve_later_order_in_batch() {
    for path in PATHS {
        let t = addr(1);
        let (_d, mut ctx) = fresh(path, &[t]);
        let r = run(
            &mut ctx,
            path,
            &[
                place(t, ro(limit(1, false, 100, 150))),
                place(t, limit(1, true, 100, 60)),
            ],
        );
        if matches!(path, Path::Single) {
            assert!(!r[0].success, "single: flat reduce-only rejected");
        }
        assert!(r[1].success, "{path:?}: later order must not be starved: {:?}", r[1].error);
        assert_eq!(resting(&ctx, &t), vec![fp(60)], "{path:?}");
        assert_bal(&ctx, &t, fp(FUNDING - 300), fp(300), "only the bid reserved");
    }
}

/// (B) Same for an oversize reduce-only order that the book clamps to the
/// position: it reserves (and rests) for the clamped quantity only.
#[test]
fn clamped_reduce_only_does_not_starve_later_order_in_batch() {
    for path in PATHS {
        let t = addr(1);
        let m1 = addr(2);
        let (_d, mut ctx) = fresh(path, &[t, m1]);
        open_long_4(&mut ctx, path, t, m1);
        // RO sell 150 @100 clamps to 4 (reserve 20); bid 60 @90 reserves 270.
        let r = run(
            &mut ctx,
            path,
            &[
                place(t, ro(limit(1, false, 100, 150))),
                place(t, limit(1, true, 90, 60)),
            ],
        );
        assert!(r.iter().all(|x| x.success), "{path:?}: {r:?}");
        assert_eq!(resting(&ctx, &t), vec![fp(4), fp(60)], "{path:?}");
        assert_bal(&ctx, &t, fp(FUNDING - 290), fp(290), "clamped RO + bid reserved");
    }
}

// ============================================================================
// F6 (s515 review): fully filled reduce-only makers leave the index
// ============================================================================

#[test]
fn fully_filled_reduce_only_maker_leaves_the_index() {
    use torus_core::order_book::OrderBook;
    let mut book = OrderBook::new(1, FixedPoint::ONE, FixedPoint::ONE);
    // No policing positions installed: the maker rests as-is.
    let r = book.place_order(ro(limit(1, false, 100, 4)), addr(1), 1);
    assert!(book.get_order(r.order_id).is_some(), "reduce-only maker rests");
    assert!(book.has_reduce_only_orders());

    let r = book.place_order(limit(1, true, 100, 4), addr(2), 2);
    assert_eq!(r.fills.len(), 1, "maker fully filled");
    assert_eq!(book.order_count(), 0);
    assert!(
        !book.has_reduce_only_orders(),
        "stale index entry keeps every later placement loading positions"
    );
    assert!(book.reduce_only_traders().is_empty());
}

// ============================================================================
// (e) modify (plan row 22): a reduce-only modify re-fits the sender's resting
// reduce-only orders like a placement does — the modify is applied (clamped
// to the position), then the sweep runs, oldest order id first (a modify
// keeps the id); every cut releases its margin.
// ============================================================================

fn modify(
    sender: Address,
    order_id: u128,
    new_price: Option<i64>,
    new_qty: Option<i64>,
) -> (Address, NativeAction) {
    (
        sender,
        NativeAction::ModifyOrder {
            order_id,
            new_price: new_price.map(fp),
            new_qty: new_qty.map(fp),
        },
    )
}

/// `(id, price, remaining)` of `trader`'s resting orders in market 1, by id.
fn resting_by_id(ctx: &NativeExecContext, trader: &Address) -> Vec<(u128, FixedPoint, FixedPoint)> {
    let mut v: Vec<_> = ctx.order_books[&1]
        .orders_for_trader(trader)
        .iter()
        .map(|o| (o.id, o.price, o.remaining_qty))
        .collect();
    v.sort();
    v
}

/// Long 4, then RO sells A 2 @110 and B 2 @120 (A older): ids `(a, b)`.
fn long_4_with_two_ro(
    ctx: &mut NativeExecContext,
    path: Path,
    t: Address,
    m: Address,
) -> (u128, u128) {
    open_long_4(ctx, path, t, m);
    let r = run(ctx, path, &[place(t, ro(limit(1, false, 110, 2)))]);
    assert!(r[0].success, "{path:?}: {:?}", r[0].error);
    let r = run(ctx, path, &[place(t, ro(limit(1, false, 120, 2)))]);
    assert!(r[0].success, "{path:?}: {:?}", r[0].error);
    let v = resting_by_id(ctx, &t);
    assert_eq!(v.len(), 2, "{path:?}: setup two RO orders");
    // 110*2/20 + 120*2/20 = 11 + 12
    assert_bal(ctx, &t, fp(FUNDING - 23), fp(23), "setup");
    (v[0].0, v[1].0)
}

#[test]
fn reduce_only_modify_growing_older_order_cancels_younger() {
    for path in PATHS {
        let (t, m) = (addr(1), addr(2));
        let (_d, mut ctx) = fresh(path, &[t, m]);
        let (a, _b) = long_4_with_two_ro(&mut ctx, path, t, m);

        let r = run(&mut ctx, path, &[modify(t, a, None, Some(4))]);
        assert!(r[0].success, "{path:?}: {:?}", r[0].error);
        assert_eq!(
            resting_by_id(&ctx, &t),
            vec![(a, fp(110), fp(4))],
            "{path:?}: B swept"
        );
        assert_bal(&ctx, &t, fp(FUNDING - 22), fp(22), "B's margin released");
        assert!(ctx.order_books[&1].has_reduce_only_orders(), "{path:?}");
    }
}

#[test]
fn reduce_only_modify_reprice_and_grow_cancels_younger() {
    for path in PATHS {
        let (t, m) = (addr(1), addr(2));
        let (_d, mut ctx) = fresh(path, &[t, m]);
        let (a, _b) = long_4_with_two_ro(&mut ctx, path, t, m);

        // Price change: re-inserted (newest in time), same id, so still first.
        let r = run(&mut ctx, path, &[modify(t, a, Some(115), Some(4))]);
        assert!(r[0].success, "{path:?}: {:?}", r[0].error);
        assert_eq!(
            resting_by_id(&ctx, &t),
            vec![(a, fp(115), fp(4))],
            "{path:?}: B swept"
        );
        // 115*4/20 = 23
        assert_bal(&ctx, &t, fp(FUNDING - 23), fp(23), "B's margin released");
    }
}

#[test]
fn reduce_only_modify_growing_younger_order_keeps_only_the_budget_left() {
    for path in PATHS {
        let (t, m) = (addr(1), addr(2));
        let (_d, mut ctx) = fresh(path, &[t, m]);
        let (a, b) = long_4_with_two_ro(&mut ctx, path, t, m);

        // Clamped to the position (4) alone it would be 3; A keeps 2 first.
        let r = run(&mut ctx, path, &[modify(t, b, None, Some(3))]);
        assert!(r[0].success, "{path:?}: {:?}", r[0].error);
        assert_eq!(
            resting_by_id(&ctx, &t),
            vec![(a, fp(110), fp(2)), (b, fp(120), fp(2))],
            "{path:?}: B cut back to 2"
        );
        assert_bal(
            &ctx,
            &t,
            fp(FUNDING - 23),
            fp(23),
            "the cut's margin released",
        );
    }
}

#[test]
fn reduce_only_modify_of_order_past_the_budget_cancels_it() {
    for path in PATHS {
        let (t, m) = (addr(1), addr(2));
        let (_d, mut ctx) = fresh(path, &[t, m]);
        open_long_4(&mut ctx, path, t, m);
        let r = run(&mut ctx, path, &[place(t, ro(limit(1, false, 110, 4)))]);
        assert!(r[0].success, "{path:?}: {:?}", r[0].error);
        let a = resting_by_id(&ctx, &t)[0].0;
        // A leftover the pre-fix modify could leave: B rests past the budget
        // A uses up (placed on the book unpoliced, its margin reserved by hand).
        // Order ids are global: give B one no book (the Parallel filler's
        // market 9 included) has used.
        let book = ctx.order_books.get_mut(&1).unwrap();
        book.set_next_order_id(1_000_000);
        let b = book
            .place_order(ro(limit(1, false, 120, 2)), t, 1000)
            .order_id;
        assert_eq!(b, 1_000_000, "{path:?}");
        let mut bal = ctx.positions.get_native_balance(&t).unwrap();
        bal.available -= fp(12);
        bal.order_margin += fp(12);
        ctx.positions.put_native_balance(&t, &bal).unwrap();
        assert_bal(&ctx, &t, fp(FUNDING - 34), fp(34), "setup");

        let r = run(&mut ctx, path, &[modify(t, b, Some(125), None)]);
        assert!(r[0].success, "{path:?}: {:?}", r[0].error);
        assert_eq!(
            resting_by_id(&ctx, &t),
            vec![(a, fp(110), fp(4))],
            "{path:?}: B swept"
        );
        assert_bal(&ctx, &t, fp(FUNDING - 22), fp(22), "B's margin released");
    }
}

#[test]
fn reduce_only_modify_that_changes_nothing_still_sweeps() {
    for path in PATHS {
        let (t, m) = (addr(1), addr(2));
        let (_d, mut ctx) = fresh(path, &[t, m]);
        open_long_4(&mut ctx, path, t, m);
        let r = run(&mut ctx, path, &[place(t, ro(limit(1, false, 110, 4)))]);
        assert!(r[0].success, "{path:?}: {:?}", r[0].error);
        let a = resting_by_id(&ctx, &t)[0].0;
        // A leftover past the budget A uses up (as in the test above).
        let book = ctx.order_books.get_mut(&1).unwrap();
        book.set_next_order_id(1_000_000);
        book.place_order(ro(limit(1, false, 120, 2)), t, 1000);
        let mut bal = ctx.positions.get_native_balance(&t).unwrap();
        bal.available -= fp(12);
        bal.order_margin += fp(12);
        ctx.positions.put_native_balance(&t, &bal).unwrap();

        // Same quantity, and one clamped back to it (6 -> position 4).
        for qty in [4, 6] {
            let r = run(&mut ctx, path, &[modify(t, a, None, Some(qty))]);
            assert!(r[0].success, "{path:?}: {:?}", r[0].error);
            assert_eq!(
                resting_by_id(&ctx, &t),
                vec![(a, fp(110), fp(4))],
                "{path:?} {qty}: B swept"
            );
            assert_bal(&ctx, &t, fp(FUNDING - 22), fp(22), "B's margin released");
        }
    }
}
