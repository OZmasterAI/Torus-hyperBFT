//! s515 parity-audit BUG 1: market orders reserved ZERO margin and ignored
//! their price entirely at match time.
//!
//! Hyperliquid parity: a market order is an aggressive IOC limit — its
//! `price` is a REQUIRED slippage cap (worst acceptable price). These tests pin:
//!   - price <= 0 on Market / StopMarket is rejected with a clear error;
//!   - margin is reserved at the cap (insufficient margin → rejected);
//!   - matching never crosses the cap (buy: no asks above it, sell: no bids
//!     below it); the unfilled remainder is cancelled, never rests, and the
//!     whole unused reservation is released;
//!   - a triggered StopMarket respects its cap and its fills settle.
//!
//! Every scenario runs through all three PlaceOrder paths: the single-action
//! `execute` (exec_place_order), `execute_batch` serial Phase 2, and the
//! sharded parallel Phase 2 (`execute_batch_engine_mode` threads >= 2 with
//! >= 2 senders in the batch).

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

fn stop_market(
    market_id: MarketId,
    is_buy: bool,
    trigger: i64,
    cap: FixedPoint,
    qty: i64,
) -> PlaceOrderParams {
    PlaceOrderParams {
        market_id,
        is_buy,
        price: cap,
        quantity: fp(qty),
        order_type: OrderType::StopMarket { trigger: fp(trigger) },
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

/// Signed position size (+long / -short / 0 flat) of `trader` in market 1.
fn pos(ctx: &NativeExecContext, trader: &Address) -> FixedPoint {
    match ctx.positions.get_position(trader, 1).unwrap() {
        Some(p) if p.is_long => p.size,
        Some(p) => -p.size,
        None => FixedPoint::ZERO,
    }
}

/// Remaining quantities of `trader`'s resting orders in market 1.
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

const PATHS: [Path; 3] = [Path::Single, Path::Batch, Path::Parallel];

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

// ============================================================================
// (a) price <= 0 is rejected for Market and StopMarket
// ============================================================================

#[test]
fn market_order_with_zero_price_rejected() {
    for path in PATHS {
        let maker = addr(1);
        let taker = addr(2);
        let (_d, mut ctx) = fresh(path, &[maker, taker]);
        let r = run(&mut ctx, path, &[place(maker, limit(1, false, 100, 4))]);
        assert!(r[0].success, "{path:?}: {:?}", r[0].error);

        let r = run(&mut ctx, path, &[place(taker, market(1, true, FixedPoint::ZERO, 4))]);
        assert!(!r[0].success, "{path:?}: zero-cap market order must be rejected");
        let err = r[0].error.as_deref().unwrap_or("");
        assert!(err.contains("price cap"), "{path:?}: unexpected error {err:?}");
        assert_eq!(pos(&ctx, &taker), FixedPoint::ZERO, "{path:?}: no fill");
        assert_eq!(resting(&ctx, &maker), vec![fp(4)], "{path:?}: ask untouched");
        assert_bal(&ctx, &taker, fp(FUNDING), FixedPoint::ZERO, "taker");
    }
}

#[test]
fn stop_market_with_zero_price_rejected() {
    for path in PATHS {
        let t = addr(2);
        let (_d, mut ctx) = fresh(path, &[t]);
        let r = run(
            &mut ctx,
            path,
            &[place(t, stop_market(1, true, 100, FixedPoint::ZERO, 4))],
        );
        assert!(!r[0].success, "{path:?}: zero-cap stop-market must be rejected");
        let err = r[0].error.as_deref().unwrap_or("");
        assert!(err.contains("price cap"), "{path:?}: unexpected error {err:?}");
        assert_bal(&ctx, &t, fp(FUNDING), FixedPoint::ZERO, "trader");
    }
}

// ============================================================================
// (b) margin is reserved at the cap
// ============================================================================

#[test]
fn market_buy_exceeding_margin_rejected() {
    for path in PATHS {
        let maker = addr(1);
        let taker = addr(2);
        let (_d, mut ctx) = fresh(path, &[maker]);
        // Small balance: 10 < 100*10/20 = 50 needed at the cap.
        fund_native(&ctx, &taker, fp(10));
        let r = run(&mut ctx, path, &[place(maker, limit(1, false, 100, 10))]);
        assert!(r[0].success, "{path:?}: {:?}", r[0].error);

        let r = run(&mut ctx, path, &[place(taker, market(1, true, fp(100), 10))]);
        assert!(!r[0].success, "{path:?}: must be rejected");
        let err = r[0].error.as_deref().unwrap_or("");
        assert!(err.contains("insufficient margin"), "{path:?}: unexpected error {err:?}");
        assert_eq!(pos(&ctx, &taker), FixedPoint::ZERO, "{path:?}: no position opened");
        assert_eq!(resting(&ctx, &maker), vec![fp(10)], "{path:?}: ask untouched");
        assert_bal(&ctx, &taker, fp(10), FixedPoint::ZERO, "taker");
    }
}

// ============================================================================
// (c) the cap is enforced at match time
// ============================================================================

#[test]
fn market_buy_cap_below_best_ask_does_not_fill() {
    for path in PATHS {
        let maker = addr(1);
        let taker = addr(2);
        let (_d, mut ctx) = fresh(path, &[maker, taker]);
        run(&mut ctx, path, &[place(maker, limit(1, false, 100, 4))]);

        let r = run(&mut ctx, path, &[place(taker, market(1, true, fp(99), 4))]);
        assert!(r[0].success, "{path:?}: {:?}", r[0].error);
        assert_eq!(pos(&ctx, &taker), FixedPoint::ZERO, "{path:?}: nothing within cap");
        assert_eq!(resting(&ctx, &maker), vec![fp(4)], "{path:?}: ask untouched");
        assert!(resting(&ctx, &taker).is_empty(), "{path:?}: market never rests");
        assert_bal(&ctx, &taker, fp(FUNDING), FixedPoint::ZERO, "taker released");
        assert_bal(&ctx, &maker, fp(FUNDING - 20), fp(20), "maker still reserved");
    }
}

#[test]
fn market_sell_cap_above_best_bid_does_not_fill() {
    for path in PATHS {
        let maker = addr(1);
        let taker = addr(2);
        let (_d, mut ctx) = fresh(path, &[maker, taker]);
        run(&mut ctx, path, &[place(maker, limit(1, true, 100, 4))]);

        let r = run(&mut ctx, path, &[place(taker, market(1, false, fp(101), 4))]);
        assert!(r[0].success, "{path:?}: {:?}", r[0].error);
        assert_eq!(pos(&ctx, &taker), FixedPoint::ZERO, "{path:?}: nothing within cap");
        assert_eq!(resting(&ctx, &maker), vec![fp(4)], "{path:?}: bid untouched");
        assert_bal(&ctx, &taker, fp(FUNDING), FixedPoint::ZERO, "taker released");
    }
}

#[test]
fn market_buy_cap_partially_through_book_fills_up_to_cap_only() {
    for path in PATHS {
        let m1 = addr(1);
        let m2 = addr(2);
        let m3 = addr(3);
        let taker = addr(4);
        let (_d, mut ctx) = fresh(path, &[m1, m2, m3, taker]);
        let r = run(
            &mut ctx,
            path,
            &[
                place(m1, limit(1, false, 100, 3)),
                place(m2, limit(1, false, 101, 3)),
                place(m3, limit(1, false, 103, 3)),
            ],
        );
        assert!(r.iter().all(|x| x.success), "{path:?}: {r:?}");

        // Cap 101: sweeps 100 and 101 (6), must NOT touch 103.
        let r = run(&mut ctx, path, &[place(taker, market(1, true, fp(101), 9))]);
        assert!(r[0].success, "{path:?}: {:?}", r[0].error);
        assert_eq!(pos(&ctx, &taker), fp(6), "{path:?}: filled only within cap");
        assert!(resting(&ctx, &taker).is_empty(), "{path:?}: remainder cancelled, never rests");
        assert_eq!(resting(&ctx, &m3), vec![fp(3)], "{path:?}: 103 ask untouched");
        // Taker: reservation 101*9/20 fully released (fills open a position,
        // no fees/PnL in this harness).
        assert_bal(&ctx, &taker, fp(FUNDING), FixedPoint::ZERO, "taker");
        assert_bal(&ctx, &m1, fp(FUNDING), FixedPoint::ZERO, "m1 consumed");
        assert_bal(&ctx, &m2, fp(FUNDING), FixedPoint::ZERO, "m2 consumed");
        // 103*3/20 = 15.45 still reserved.
        assert_bal(&ctx, &m3, fp(FUNDING) - fp_cents(1545), fp_cents(1545), "m3 resting");
    }
}

// ============================================================================
// (d) triggered StopMarket respects its cap, and its fills settle
// ============================================================================

#[test]
fn triggered_stop_market_respects_cap() {
    for path in PATHS {
        let m1 = addr(1);
        let m2 = addr(2);
        let x = addr(3);
        let t = addr(4);
        let (_d, mut ctx) = fresh(path, &[m1, m2, x, t]);
        let r = run(
            &mut ctx,
            path,
            &[
                place(m1, limit(1, false, 100, 1)),
                place(m2, limit(1, false, 105, 5)),
                // Buy stop: trigger 100, cap 101 → reserve 101*5/20 = 25.25.
                place(t, stop_market(1, true, 100, fp(101), 5)),
            ],
        );
        assert!(r.iter().all(|x| x.success), "{path:?}: {r:?}");
        assert_bal(&ctx, &t, fp(FUNDING) - fp_cents(2525), fp_cents(2525), "stop reserved");

        // Trade at 100 triggers the stop; best ask left is 105 > cap 101.
        let r = run(&mut ctx, path, &[place(x, limit(1, true, 100, 1))]);
        assert!(r[0].success, "{path:?}: {:?}", r[0].error);
        assert_eq!(pos(&ctx, &x), fp(1), "{path:?}: trigger trade settled");
        assert_eq!(pos(&ctx, &t), FixedPoint::ZERO, "{path:?}: nothing within cap");
        assert_eq!(resting(&ctx, &m2), vec![fp(5)], "{path:?}: 105 ask untouched");
        assert_eq!(ctx.order_books[&1].pending_stop_count(), 0, "{path:?}: stop consumed");
        assert_bal(&ctx, &t, fp(FUNDING), FixedPoint::ZERO, "stop reservation released");
    }
}

#[test]
fn triggered_stop_market_fills_settle_positions_and_margin() {
    for path in PATHS {
        let m1 = addr(1);
        let m2 = addr(2);
        let x = addr(3);
        let t = addr(4);
        let (_d, mut ctx) = fresh(path, &[m1, m2, x, t]);
        run(
            &mut ctx,
            path,
            &[
                place(m1, limit(1, false, 100, 1)),
                place(m2, limit(1, false, 105, 5)),
                place(t, stop_market(1, true, 100, fp(106), 5)),
            ],
        );

        let r = run(&mut ctx, path, &[place(x, limit(1, true, 100, 1))]);
        assert!(r[0].success, "{path:?}: {:?}", r[0].error);
        // The triggered market buy fills 5 @105 (within cap 106) — and the
        // fills must reach positions + margin (they used to be dropped).
        assert_eq!(pos(&ctx, &t), fp(5), "{path:?}: triggered stop filled");
        assert_eq!(pos(&ctx, &m2), -fp(5), "{path:?}: maker side settled");
        assert!(resting(&ctx, &m2).is_empty(), "{path:?}: ask consumed");
        assert_bal(&ctx, &t, fp(FUNDING), FixedPoint::ZERO, "stop taker released");
        assert_bal(&ctx, &m2, fp(FUNDING), FixedPoint::ZERO, "maker released");
    }
}

// ============================================================================
// (e) F2 (s515 review): price × qty overflow is a rejection, never a panic
// ============================================================================

/// A cap of 1e22 with qty 1e9 has a notional past `i128::MAX` raw. The
/// reservation used to `expect()` on the multiplication — every validator
/// panicked on the same action (the parallel prepare fell back to serial and
/// panicked again): a chain halt. It must be rejected with balances untouched.
#[test]
fn market_order_notional_overflow_rejected_without_panic() {
    let huge_cap = FixedPoint::from_raw(10i128.pow(22) * FixedPoint::SCALE);
    for path in PATHS {
        for is_buy in [true, false] {
            let maker = addr(1);
            let taker = addr(2);
            let (_d, mut ctx) = fresh(path, &[maker, taker]);
            let r = run(&mut ctx, path, &[place(maker, limit(1, !is_buy, 100, 4))]);
            assert!(r[0].success, "{path:?}: {:?}", r[0].error);

            for p in [
                market(1, is_buy, huge_cap, 1_000_000_000),
                stop_market(1, is_buy, if is_buy { 200 } else { 50 }, huge_cap, 1_000_000_000),
                PlaceOrderParams {
                    price: huge_cap,
                    ..limit(1, is_buy, 1, 1_000_000_000)
                },
            ] {
                let what = format!("{path:?} buy={is_buy} {:?}", p.order_type);
                let r = run(&mut ctx, path, &[place(taker, p)]);
                assert!(!r[0].success, "{what}: overflowing notional must be rejected");
                let err = r[0].error.as_deref().unwrap_or("");
                assert!(err.contains("overflow"), "{what}: unexpected error {err:?}");
                assert_eq!(pos(&ctx, &taker), FixedPoint::ZERO, "{what}: no fill");
                assert_bal(&ctx, &taker, fp(FUNDING), FixedPoint::ZERO, &what);
            }
            assert_eq!(resting(&ctx, &maker), vec![fp(4)], "{path:?}: maker untouched");
            assert!(resting(&ctx, &taker).is_empty(), "{path:?}: nothing rests");
        }
    }
}

// ============================================================================
// (f) F5 (s515 review): a market SELL reserves at max(cap, best bid)
// ============================================================================

/// A sell's cap is its LOWEST acceptable price (clients send ~1), so
/// reserving at the cap reserved ~nothing while the order opened a short at
/// the bids. Fills happen at bids <= best bid, so max(cap, best bid) bounds
/// the notional: 100 * 4 / 20 = 20 > 10 available → rejected.
#[test]
fn market_sell_reserves_at_best_bid_not_cap() {
    for path in PATHS {
        let maker = addr(1);
        let taker = addr(2);
        let (_d, mut ctx) = fresh(path, &[maker]);
        fund_native(&ctx, &taker, fp(10));
        let r = run(&mut ctx, path, &[place(maker, limit(1, true, 100, 4))]);
        assert!(r[0].success, "{path:?}: {:?}", r[0].error);

        let r = run(&mut ctx, path, &[place(taker, market(1, false, fp(1), 4))]);
        assert!(!r[0].success, "{path:?}: must be rejected for margin");
        let err = r[0].error.as_deref().unwrap_or("");
        assert!(err.contains("insufficient margin"), "{path:?}: unexpected error {err:?}");
        assert_eq!(pos(&ctx, &taker), FixedPoint::ZERO, "{path:?}: no short opened");
        assert_eq!(resting(&ctx, &maker), vec![fp(4)], "{path:?}: bid untouched");
        assert_bal(&ctx, &taker, fp(10), FixedPoint::ZERO, "taker");

        // With enough margin the same order fills and releases everything.
        fund_native(&ctx, &taker, fp(FUNDING));
        let r = run(&mut ctx, path, &[place(taker, market(1, false, fp(1), 4))]);
        assert!(r[0].success, "{path:?}: {:?}", r[0].error);
        assert_eq!(pos(&ctx, &taker), -fp(4), "{path:?}: short opened at the bid");
        assert_bal(&ctx, &taker, fp(FUNDING), FixedPoint::ZERO, "taker released");
    }
}

/// F2 (s515 review, second pass): the batch Phase-2 basis read the PRE-batch
/// best bid, but an earlier in-batch bid can raise it before the market sell
/// matches. Pre-batch best bid 100; another sender's GTC bid at 150 sorts
/// first; the taker's PlaceOrderBatch holds a market sell (cap 1) of 4:
/// 150 * 4 / 20 = 30 > 25 available → rejected (it reserved 20 at 100 and
/// then filled at 150).
#[test]
fn batch_market_sell_reserves_at_earlier_in_batch_bid() {
    for path in PATHS {
        let maker = addr(1);
        let bidder = addr(3);
        let taker = addr(2);
        let (_d, mut ctx) = fresh(path, &[maker, bidder]);
        fund_native(&ctx, &taker, fp(25));
        let r = run(&mut ctx, path, &[place(maker, limit(1, true, 100, 4))]);
        assert!(r[0].success, "{path:?}: {:?}", r[0].error);

        let sell = NativeAction::PlaceOrderBatch(vec![market(1, false, fp(1), 4)]);
        let r = run(
            &mut ctx,
            path,
            &[place(bidder, limit(1, true, 150, 4)), (taker, sell.clone())],
        );
        assert!(r[0].success, "{path:?}: bid {:?}", r[0].error);
        assert!(!r[1].success, "{path:?}: market sell must be rejected for margin");
        assert_eq!(pos(&ctx, &taker), FixedPoint::ZERO, "{path:?}: no short opened");
        assert_eq!(resting(&ctx, &bidder), vec![fp(4)], "{path:?}: 150 bid untouched");
        assert_bal(&ctx, &taker, fp(25), FixedPoint::ZERO, "taker");

        // Enough margin for 150: fills against the in-batch bid, all released.
        fund_native(&ctx, &taker, fp(30));
        let r = run(
            &mut ctx,
            path,
            &[place(bidder, limit(1, true, 150, 4)), (taker, sell)],
        );
        assert!(r.iter().all(|x| x.success), "{path:?}: {r:?}");
        assert_eq!(pos(&ctx, &taker), -fp(4), "{path:?}: short opened at 150");
        assert_bal(&ctx, &taker, fp(30), FixedPoint::ZERO, "taker released");
    }
}

// ============================================================================
// (g) F2 (s515 review 3): only a bid that can REST before the sell raises the
//     batch market-sell reservation (funded, valid, resting TIF, not consumed
//     by pre-batch asks) - otherwise any account could grief every later
//     market sell in the batch with an unfundable / non-resting high bid.
// ============================================================================

/// Pre-batch: maker bid 100 x 4, then `setup` (one block each). The batch is
/// `[ahead, taker market sell 4]`. Taker holds 25: enough at 100 (20), not at
/// 150 (30). addr(3) / addr(4) are funded, addr(0) (sorts first) holds nothing.
fn run_sell_after(
    path: Path,
    setup: &[(Address, NativeAction)],
    ahead: (Address, NativeAction),
) -> (Vec<NativeActionResult>, NativeExecContext, tempfile::TempDir) {
    let (d, mut ctx) = fresh(path, &[addr(1), addr(3), addr(4)]);
    fund_native(&ctx, &addr(2), fp(25));
    let r = run(&mut ctx, path, &[place(addr(1), limit(1, true, 100, 4))]);
    assert!(r[0].success, "{path:?}: {:?}", r[0].error);
    for a in setup {
        let r = run(&mut ctx, path, std::slice::from_ref(a));
        assert!(r[0].success, "{path:?}: setup {:?}", r[0].error);
    }
    let sell = NativeAction::PlaceOrderBatch(vec![market(1, false, fp(1), 4)]);
    let r = run(&mut ctx, path, &[ahead, (addr(2), sell)]);
    (r, ctx, d)
}

fn assert_sell_filled_at_100(path: Path, r: &[NativeActionResult], ctx: &NativeExecContext) {
    let taker = addr(2);
    assert!(r[1].success, "{path:?}: market sell must pass: {:?}", r[1].error);
    assert_eq!(pos(ctx, &taker), -fp(4), "{path:?}: short opened at the 100 bid");
    assert_bal(ctx, &taker, fp(25), FixedPoint::ZERO, "taker released");
}

fn ioc(mut p: PlaceOrderParams) -> PlaceOrderParams {
    p.time_in_force = TimeInForce::IOC;
    p
}

#[test]
fn batch_unfunded_ioc_high_bid_does_not_raise_market_sell_reservation() {
    for path in PATHS {
        let bid = ioc(limit(1, true, 1_000_000, 1));
        let (r, ctx, _d) = run_sell_after(path, &[], place(addr(0), bid));
        assert!(!r[0].success, "{path:?}: unfunded bid must be rejected");
        assert_sell_filled_at_100(path, &r, &ctx);
    }
}

#[test]
fn batch_funded_ioc_bid_does_not_raise_market_sell_reservation() {
    for path in PATHS {
        let bid = ioc(limit(1, true, 150, 1));
        let (r, ctx, _d) = run_sell_after(path, &[], place(addr(3), bid));
        assert!(r[0].success, "{path:?}: IOC bid (cancelled) {:?}", r[0].error);
        assert!(resting(&ctx, &addr(3)).is_empty(), "{path:?}: IOC never rests");
        assert_sell_filled_at_100(path, &r, &ctx);
    }
}

#[test]
fn batch_off_tick_or_dust_high_bid_does_not_raise_market_sell_reservation() {
    for path in PATHS {
        // Off-tick (tick 1.0) and dust (qty < lot 1.0): rejected by the book.
        for bid in [
            PlaceOrderParams { price: fp_cents(15_050), ..limit(1, true, 0, 1) },
            PlaceOrderParams { quantity: fp_cents(50), ..limit(1, true, 150, 0) },
        ] {
            let (r, ctx, _d) = run_sell_after(path, &[], place(addr(3), bid));
            // (A book-level reject is a successful action with status Rejected.)
            assert!(resting(&ctx, &addr(3)).is_empty(), "{path:?}: invalid bid never rests");
            assert_sell_filled_at_100(path, &r, &ctx);
        }
    }
}

#[test]
fn batch_unfunded_gtc_high_bid_does_not_raise_market_sell_reservation() {
    for path in PATHS {
        let (r, ctx, _d) = run_sell_after(path, &[], place(addr(0), limit(1, true, 150, 4)));
        assert!(!r[0].success, "{path:?}: unfunded GTC bid must be rejected for margin");
        assert!(resting(&ctx, &addr(0)).is_empty(), "{path:?}");
        assert_sell_filled_at_100(path, &r, &ctx);
    }
}

/// A funded GTC bid above the pre-batch best ask (120 x 10) fills against it
/// and cannot rest at 150: the sell still reserves at 100.
#[test]
fn batch_bid_through_best_ask_that_cannot_rest_is_ignored() {
    for path in PATHS {
        let setup = [place(addr(4), limit(1, false, 120, 10))];
        let (r, ctx, _d) = run_sell_after(path, &setup, place(addr(3), limit(1, true, 150, 1)));
        assert!(r[0].success, "{path:?}: {:?}", r[0].error);
        assert!(resting(&ctx, &addr(3)).is_empty(), "{path:?}: bid filled at 120");
        assert_eq!(pos(&ctx, &addr(3)), fp(1), "{path:?}");
        assert_sell_filled_at_100(path, &r, &ctx);
    }
}

/// Soundness of the ask bound: a bid that EXHAUSTS the asks (120 x 1, bid
/// 150 x 4 -> 3 rest at 150), or whose own ask is STP-cancelled, does rest at
/// 150 - the sell must still reserve at 150 (30 > 25 -> rejected).
#[test]
fn batch_bid_through_best_ask_that_rests_still_counts() {
    for path in PATHS {
        for (ask_owner, bid_qty) in [(addr(4), 4), (addr(3), 1)] {
            let setup = [place(ask_owner, limit(1, false, 120, 1))];
            let ahead = place(addr(3), limit(1, true, 150, bid_qty));
            let (r, ctx, _d) = run_sell_after(path, &setup, ahead);
            let what = format!("{path:?} ask_owner={ask_owner}");
            assert!(r[0].success, "{what}: {:?}", r[0].error);
            assert!(!resting(&ctx, &addr(3)).is_empty(), "{what}: bid rests at 150");
            assert!(!r[1].success, "{what}: sell must reserve at 150");
            assert_eq!(pos(&ctx, &addr(2)), FixedPoint::ZERO, "{what}");
            assert_bal(&ctx, &addr(2), fp(25), FixedPoint::ZERO, "taker");
        }
    }
}
