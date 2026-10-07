//! Typed reject reasons (v2 action status): every failed
//! `NativeActionResult` carries the executor's own `FailureReason`, set
//! where the check failed, never parsed from the message. One expectation
//! per reason code, on the single-action path and the batch engine with
//! serial (0) and sharded (2, 4) Phase 2. The messages are M1's texts
//! ("order rejected: price P is not a multiple of the tick T", ...).

use alloy_primitives::{Address, U256};

use torus_bridge::native_executor::{NativeActionResult, NativeExecContext, NativeExecutor};
use torus_core::position::{NativeBalance, OPEN_ORDER_BASE_LIMIT};
use torus_state::action_status::FailureReason;
use torus_state::cf::CF_NATIVE_MARKETS;
use torus_state::StateDb;
use torus_types::{FixedPoint, MarketId, NativeAction, OrderType, PlaceOrderParams, TimeInForce};

const S: i128 = FixedPoint::SCALE;

fn addr(n: u8) -> Address {
    Address::new([n; 20])
}

fn fp(v: i64) -> FixedPoint {
    FixedPoint::from_raw(v as i128 * S)
}

fn fund_native(ctx: &NativeExecContext, trader: &Address, amount: FixedPoint) {
    let bal = NativeBalance { available: amount, order_margin: FixedPoint::ZERO };
    ctx.positions.put_native_balance(trader, &bal).unwrap();
}

fn order(market_id: MarketId, is_buy: bool, price: FixedPoint, qty: FixedPoint) -> PlaceOrderParams {
    PlaceOrderParams {
        market_id,
        is_buy,
        price,
        quantity: qty,
        order_type: OrderType::Limit,
        time_in_force: TimeInForce::GTC,
        reduce_only: false,
        client_order_id: None,
    }
}

fn gtc(market_id: MarketId, is_buy: bool, price: i64, qty: i64) -> PlaceOrderParams {
    order(market_id, is_buy, fp(price), fp(qty))
}

fn stop_limit(market_id: MarketId, is_buy: bool, trigger: i64, limit: FixedPoint, qty: i64) -> PlaceOrderParams {
    PlaceOrderParams {
        order_type: OrderType::StopLimit { trigger: fp(trigger), limit },
        ..order(market_id, is_buy, FixedPoint::ZERO, fp(qty))
    }
}

fn place(sender: Address, p: PlaceOrderParams) -> (Address, NativeAction) {
    (sender, NativeAction::PlaceOrder(p))
}

fn modify(sender: Address, order_id: u128, price: Option<FixedPoint>, qty: Option<FixedPoint>) -> (Address, NativeAction) {
    (sender, NativeAction::ModifyOrder { order_id, new_price: price, new_qty: qty })
}

/// The genesis / governance market row: base, quote, lot, tick, initial margin.
fn market_row(tick: i128, lot: i128) -> Vec<u8> {
    borsh::to_vec(&("BTC".to_string(), "USD".to_string(), lot, tick, 5 * S)).unwrap()
}

/// `None` = the single-action path, `Some(t)` = `execute_batch_engine_mode(_, _, t)`.
type Mode = Option<usize>;
const MODES: [Mode; 4] = [None, Some(0), Some(2), Some(4)];

fn exec(ctx: &mut NativeExecContext, mode: Mode, block: &[(Address, NativeAction)]) -> Vec<NativeActionResult> {
    let r: Vec<_> = match mode {
        None => block.iter().map(|(s, a)| NativeExecutor::execute(ctx, s, a)).collect(),
        Some(t) => NativeExecutor::execute_batch_engine_mode(ctx, block, t).results,
    };
    assert!(ctx.fatal_error.is_none(), "{:?}", ctx.fatal_error);
    r
}

/// Market 1 with tick 2 and lot 2 (from its row); addr(1) rich, addr(2)
/// holding 1.
fn fresh() -> (tempfile::TempDir, NativeExecContext) {
    let dir = tempfile::tempdir().unwrap();
    let db = StateDb::open(dir.path()).unwrap();
    db.put_cf_raw(CF_NATIVE_MARKETS, &1u64.to_be_bytes(), &market_row(2 * S, 2 * S)).unwrap();
    let ctx = NativeExecContext::new(db, 1, 1000, 0, 100, 10, addr(99), addr(100), addr(101));
    fund_native(&ctx, &addr(1), fp(1_000_000));
    fund_native(&ctx, &addr(2), fp(1));
    (dir, ctx)
}

/// Each entry: `Some((reason, message part))` = must fail with exactly that
/// reason, `None` = must succeed (reason stays `Other`).
fn check(mode: Mode, results: &[NativeActionResult], want: &[Option<(FailureReason, &str)>]) {
    assert_eq!(results.len(), want.len(), "{mode:?}");
    for (i, (r, w)) in results.iter().zip(want).enumerate() {
        match w {
            Some((reason, text)) => {
                assert!(!r.success, "{mode:?} #{i}: must fail: {r:?}");
                assert_eq!(r.reason, *reason, "{mode:?} #{i}: {r:?}");
                let msg = r.error.as_deref().unwrap_or("");
                assert!(msg.contains(text), "{mode:?} #{i}: {msg:?} lacks {text:?}");
            }
            None => {
                assert!(r.success, "{mode:?} #{i}: {r:?}");
                assert_eq!(r.reason, FailureReason::Other, "{mode:?} #{i}: a success has no reason");
            }
        }
    }
}

/// Placement rejects before the book: off-tick limit, off-tick stop-limit
/// LIMIT (row 40), dust (lot), non-positive limit / market cap / stop-limit
/// limit (row 41), an overflowing notional, the account margin check, and
/// a cancel of an unknown order (other). Same reasons on every path.
#[test]
fn placement_rejects_carry_their_reason_on_every_path() {
    let (a, poor) = (addr(1), addr(2));
    let huge = FixedPoint::from_raw((i128::MAX / (2 * S)) * (2 * S));
    let block = vec![
        place(a, gtc(1, true, 61, 2)),                                     // 0
        place(a, stop_limit(1, true, 70, fp(61), 2)),                      // 1
        place(a, gtc(1, true, 60, 1)),                                     // 2
        place(a, gtc(1, true, 0, 2)),                                      // 3
        place(a, gtc(1, false, -2, 2)),                                    // 4
        place(
            a,
            PlaceOrderParams {
                order_type: OrderType::Market,
                time_in_force: TimeInForce::IOC,
                ..gtc(1, true, 0, 2)
            },
        ),                                                                 // 5
        place(a, stop_limit(1, true, 70, FixedPoint::ZERO, 2)),            // 6
        place(a, order(1, true, huge, fp(2))),                             // 7
        place(poor, gtc(1, true, 100, 1000)),                              // 8
        (a, NativeAction::CancelOrder { order_id: 999_999 }),             // 9
        place(a, gtc(1, true, 50, 2)),                                     // 10: rests
    ];
    let want = [
        Some((FailureReason::Tick, "order rejected: price 61.00000000 is not a multiple of the tick 2.00000000")),
        Some((FailureReason::Tick, "order rejected: price 61.00000000 is not a multiple of the tick 2.00000000")),
        Some((FailureReason::Lot, "order rejected: quantity 1.00000000 below the lot size 2.00000000")),
        Some((FailureReason::Price, "limit order requires a positive price")),
        Some((FailureReason::Price, "limit order requires a positive price")),
        Some((FailureReason::Price, "market order requires a positive price cap")),
        Some((FailureReason::Price, "stop-limit order requires a positive limit price")),
        Some((FailureReason::Price, "order notional overflows")),
        Some((FailureReason::Margin, "insufficient margin")),
        Some((FailureReason::Other, "not found")),
        None,
    ];
    for mode in MODES {
        let (_d, mut ctx) = fresh();
        let r = exec(&mut ctx, mode, &block);
        check(mode, &r, &want);
    }
}

/// The open-order limit (plain and the stricter reduce-only / stop rule)
/// reports `OpenLimit` on every path.
#[test]
fn open_order_limit_carries_its_reason_on_every_path() {
    let a = addr(1);
    for mode in MODES {
        let (_d, mut ctx) = fresh();
        // Market 3 has no row: tick / lot 1.
        let fill: Vec<_> = (0..OPEN_ORDER_BASE_LIMIT as i64).map(|k| place(a, gtc(3, true, 1 + k, 1))).collect();
        for chunk in fill.chunks(250) {
            assert!(exec(&mut ctx, mode, chunk).iter().all(|r| r.success), "{mode:?}");
        }
        let r = exec(
            &mut ctx,
            mode,
            &[
                place(a, gtc(3, true, 1, 1)),
                place(a, stop_limit(3, true, 5000, fp(5000), 1)),
                place(a, PlaceOrderParams { time_in_force: TimeInForce::IOC, ..gtc(3, false, 2000, 1) }),
            ],
        );
        check(
            mode,
            &r,
            &[
                Some((FailureReason::OpenLimit, "open order limit reached: 1000 open orders, limit 1000")),
                Some((FailureReason::OpenLimit, "open order limit: reduce-only and stop orders need fewer than")),
                // IOC never rests: no slot needed. Row 50: nothing to fill
                // against, so it is rejected (HL `iocCancelRejected`).
                Some((FailureReason::IocCancel, "order rejected: IOC order could not immediately match")),
            ],
        );
    }
}

/// Row 50: an order the book refuses, or cancels without a fill, is
/// rejected with its HL reason on the single-action path, the sequential
/// settle (engine 0) and the parallel settle's pass B (engine 2, 4), with
/// the executed order's gas (1000) kept; an IOC that partly fills stays
/// executed. Market 3 (tick / lot 1): bid 100 x 2, ask 110 x 2, last trade
/// 105; addr(1) is flat there.
#[test]
fn book_rejections_carry_their_hl_reason_on_every_path() {
    let a = addr(1);
    let with = |tif, p: PlaceOrderParams| PlaceOrderParams { time_in_force: tif, ..p };
    let block = vec![
        place(a, with(TimeInForce::IOC, gtc(3, true, 105, 1))), // 0
        place(a, with(TimeInForce::PostOnly, gtc(3, true, 110, 1))), // 1
        place(
            a,
            PlaceOrderParams { order_type: OrderType::Market, ..with(TimeInForce::IOC, gtc(3, true, 106, 1)) },
        ), // 2: cap under the ask
        place(a, PlaceOrderParams { reduce_only: true, ..gtc(3, false, 100, 1) }), // 3
        place(a, with(TimeInForce::FOK, gtc(3, true, 110, 5))), // 4
        place(
            a,
            PlaceOrderParams { order_type: OrderType::StopMarket { trigger: fp(104) }, ..gtc(3, true, 200, 1) },
        ), // 5: trigger under the last trade
        place(a, with(TimeInForce::IOC, gtc(3, true, 110, 3))), // 6: fills 2
    ];
    let want = [
        Some((FailureReason::IocCancel, "order rejected: IOC order could not immediately match")),
        Some((FailureReason::BadAloPx, "order rejected: post-only order would have immediately matched")),
        Some((FailureReason::MarketNoLiquidity, "order rejected: no liquidity for the market order")),
        Some((FailureReason::ReduceOnly, "reduce-only order rejected")),
        Some((FailureReason::FokCancel, "order rejected: FOK order could not be filled completely")),
        Some((FailureReason::BadTriggerPx, "order rejected: stop trigger")),
        None,
    ];
    for mode in MODES {
        let (_d, mut ctx) = fresh();
        for t in [3, 4, 5] {
            fund_native(&ctx, &addr(t), fp(1_000_000));
        }
        let setup = [
            place(addr(4), gtc(3, false, 105, 1)),
            place(addr(5), gtc(3, true, 105, 1)),
            place(addr(3), gtc(3, true, 100, 2)),
            place(addr(3), gtc(3, false, 110, 2)),
        ];
        for action in &setup {
            assert!(exec(&mut ctx, mode, std::slice::from_ref(action))[0].success, "{mode:?}");
        }
        let r = exec(&mut ctx, mode, &block);
        check(mode, &r, &want);
        for (i, r) in r.iter().enumerate() {
            // The single path refuses a reduce-only order before the book
            // (`reduce_only_violation`, no gas, as before row 50).
            let gas = if mode.is_none() && i == 3 { 0 } else { 1000 };
            assert_eq!(r.gas_used, gas, "{mode:?} #{i}: a book outcome keeps the executed order's gas");
        }
        let book = &ctx.order_books[&3];
        assert_eq!(book.best_ask(), None, "{mode:?}: the partial IOC took the ask");
        assert_eq!(book.best_bid(), Some(fp(100)), "{mode:?}: nothing rejected rests");
    }
}

/// ModifyOrder: off-tick new price (tick), new quantity under the lot or
/// not positive (lot), new price not positive / overflowing (price), the
/// account check (margin), unknown order (other).
#[test]
fn modify_rejects_carry_their_reason_on_every_path() {
    let (a, poor) = (addr(1), addr(2));
    let huge = FixedPoint::from_raw((i128::MAX / (2 * S)) * (2 * S));
    for mode in MODES {
        let (_d, mut ctx) = fresh();
        let id_a = ctx.next_global_order_id;
        // poor: 1 available; 10 x 2 at 20x needs exactly 1.
        let r = exec(&mut ctx, mode, &[place(a, gtc(1, true, 50, 2)), place(poor, gtc(1, true, 10, 2))]);
        assert!(r.iter().all(|r| r.success), "{mode:?}: {r:?}");
        let id_poor = id_a + 1;
        let r = exec(
            &mut ctx,
            mode,
            &[
                modify(a, id_a, Some(fp(51)), None),
                modify(a, id_a, None, Some(fp(1))),
                modify(a, id_a, None, Some(FixedPoint::ZERO)),
                modify(a, id_a, Some(FixedPoint::ZERO), None),
                modify(a, id_a, Some(huge), None),
                modify(poor, id_poor, None, Some(fp(2000))),
                modify(a, 999_999, Some(fp(52)), None),
                modify(a, id_a, Some(fp(52)), None),
            ],
        );
        check(
            mode,
            &r,
            &[
                Some((FailureReason::Tick, "modify rejected: price 51.00000000 is not a multiple of the tick 2.00000000")),
                Some((FailureReason::Lot, "modify rejected: quantity 1.00000000 below the lot size 2.00000000")),
                Some((FailureReason::Lot, "modify rejected: quantity must be positive")),
                Some((FailureReason::Price, "modify rejected: price must be positive")),
                Some((FailureReason::Price, "order notional overflows")),
                Some((FailureReason::Margin, "insufficient margin for modify")),
                Some((FailureReason::Other, "not found")),
                None,
            ],
        );
    }
}

/// Single-action path only (the batch engine never hands a whole
/// PlaceOrderBatch to it; consensus records those): an empty batch is
/// `BatchCap`, a batch with a bad order carries that order's reason.
#[test]
fn single_path_batch_summary_carries_a_reason() {
    let a = addr(1);
    let (_d, mut ctx) = fresh();
    let r = exec(
        &mut ctx,
        None,
        &[
            (a, NativeAction::PlaceOrderBatch(vec![])),
            (a, NativeAction::PlaceOrderBatch(vec![gtc(1, true, 50, 2), gtc(1, true, 50, 1), gtc(1, true, 48, 2)])),
        ],
    );
    check(
        None,
        &r,
        &[
            Some((FailureReason::BatchCap, "outside [1, 1024]")),
            Some((FailureReason::Lot, "2/3 orders placed")),
        ],
    );
}

/// A withdrawal refused by the account margin rule reports `Margin`.
#[test]
fn withdrawal_under_margin_carries_margin_reason() {
    let (a, b) = (addr(1), addr(3));
    for mode in MODES {
        let (_d, mut ctx) = fresh();
        fund_native(&ctx, &b, fp(150));
        // b long 10 @100 (IM 50, 10% floor 100): 51 of 150 is too much.
        let r = exec(&mut ctx, mode, &[place(a, gtc(1, false, 100, 10))]);
        assert!(r[0].success, "{mode:?}");
        let r = exec(&mut ctx, mode, &[place(b, gtc(1, true, 100, 10))]);
        assert!(r[0].success, "{mode:?}: {:?}", r[0]);
        let amount = U256::from((51 * S) as u128);
        let r = exec(&mut ctx, mode, &[(b, NativeAction::TransferToSpot { amount })]);
        check(mode, &r, &[Some((FailureReason::Margin, "under-margined"))]);
    }
}
