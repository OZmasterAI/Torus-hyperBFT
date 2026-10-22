//! s114 (archy section 53c.4): `torus_orders_rejected_reason_total{reason}`
//! counts every rejected PlaceOrder once under its reason, on the
//! single-action path and the batch engine with serial (0) and sharded (2,
//! 4) Phase 2. The pre-book account margin check is split: `margin_position`
//! (the order fails even with the sender's open-order reservations and the
//! batch's earlier commitments released) vs `margin_reservation` (it would
//! pass with them released). The funnel counters (`orders_rejected_margin`,
//! `_open_limit`, `_book`, `_cancelled`, `_other`) are unchanged: their sum
//! equals the labelled total in every scenario here.

use std::sync::Arc;

use alloy_primitives::Address;

use torus_bridge::native_executor::{NativeActionResult, NativeExecContext, NativeExecutor};
use torus_core::position::{NativeBalance, OPEN_ORDER_BASE_LIMIT};
use torus_state::cf::CF_NATIVE_MARKETS;
use torus_state::StateDb;
use torus_telemetry::{Metrics, OrderRejectReason as R};
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

/// Market 1 with tick 2 and lot 2 (from its row; market 3 has no row: tick
/// / lot 1); addr(1) rich, addr(2) holding 1; metrics attached.
fn fresh() -> (tempfile::TempDir, NativeExecContext, Arc<Metrics>) {
    // Owner s138: a fixture from before the never-marked rule (test-only seam).
    torus_core::oracle::test_treat_unmarked_as_marked();
    let dir = tempfile::tempdir().unwrap();
    let db = StateDb::open(dir.path()).unwrap();
    // Pre-testnet P0: orders need a listed market (placeholder rows,
    // tick / lot 1, as the old implicit book).
    for m in 1..=64u64 {
        db.put_cf_raw(torus_state::cf::CF_NATIVE_MARKETS, &m.to_be_bytes(), b"listed").unwrap();
    }
    db.put_cf_raw(CF_NATIVE_MARKETS, &1u64.to_be_bytes(), &market_row(2 * S, 2 * S)).unwrap();
    let mut ctx = NativeExecContext::new(db, 1, 1000, 0, 100, 10, addr(99), addr(100), addr(101));
    fund_native(&ctx, &addr(1), fp(1_000_000));
    fund_native(&ctx, &addr(2), fp(1));
    let m = Arc::new(Metrics::new());
    ctx.metrics = Some(m.clone());
    (dir, ctx, m)
}

/// The non-zero reason series, in label order.
fn by_reason(m: &Metrics) -> Vec<(&'static str, u64)> {
    R::ALL.iter().map(|r| (r.as_str(), m.orders_rejected_reason[*r as usize].get())).filter(|(_, n)| *n > 0).collect()
}

/// The funnel's dead orders: every reject counter before s114.
fn funnel_rejects(m: &Metrics) -> u64 {
    m.orders_rejected_margin.get()
        + m.orders_rejected_open_limit.get()
        + m.orders_rejected_book.get()
        + m.orders_rejected_cancelled.get()
        + m.orders_rejected_other.get()
}

/// The labelled total equals the funnel's reject total (each rejected order
/// counted once in both), and the failed PlaceOrder results.
fn assert_totals(mode: Mode, m: &Metrics, failed_orders: u64) {
    let total: u64 = m.orders_rejected_reason.iter().map(|c| c.get()).sum();
    assert_eq!(total, funnel_rejects(m), "{mode:?}: labelled total vs funnel rejects");
    assert_eq!(total, failed_orders, "{mode:?}: labelled total vs failed PlaceOrders");
}

fn failed(r: &[NativeActionResult]) -> u64 {
    r.iter().filter(|r| !r.success && r.action_type == "place_order").count() as u64
}

/// Placement rejects before the book: off-tick limit and stop-limit limit
/// (tick), dust (lot), non-positive prices and an overflowing notional
/// (price), the account check of a flat sender with no reservations
/// (margin_position). A failed cancel is not an order reject.
#[test]
fn placement_rejects_count_under_their_reason_on_every_path() {
    let (a, poor) = (addr(1), addr(2));
    let huge = FixedPoint::from_raw((i128::MAX / (2 * S)) * (2 * S));
    let block = vec![
        place(a, gtc(1, true, 61, 2)),
        place(a, stop_limit(1, true, 70, fp(61), 2)),
        place(a, gtc(1, true, 60, 1)),
        place(a, gtc(1, true, 0, 2)),
        place(a, gtc(1, false, -2, 2)),
        place(
            a,
            PlaceOrderParams { order_type: OrderType::Market, time_in_force: TimeInForce::IOC, ..gtc(1, true, 0, 2) },
        ),
        place(a, stop_limit(1, true, 70, FixedPoint::ZERO, 2)),
        place(a, order(1, true, huge, fp(2))),
        place(poor, gtc(1, true, 100, 1000)),
        (a, NativeAction::CancelOrder { order_id: 999_999 }),
        place(a, gtc(1, true, 50, 2)),
    ];
    for mode in MODES {
        let (_d, mut ctx, m) = fresh();
        let r = exec(&mut ctx, mode, &block);
        assert_eq!(
            by_reason(&m),
            vec![("margin_position", 1), ("tick", 2), ("lot", 1), ("price", 5)],
            "{mode:?}: {r:?}"
        );
        assert_eq!(m.orders_rejected_margin.get(), 1, "{mode:?}");
        assert_totals(mode, &m, failed(&r));
    }
}

/// The open-order limit (plain and the reduce-only / stop rule): open_limit.
#[test]
fn open_order_limit_counts_under_open_limit_on_every_path() {
    let a = addr(1);
    for mode in MODES {
        let (_d, mut ctx, m) = fresh();
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
        assert_eq!(by_reason(&m), vec![("open_limit", 2), ("ioc_cancel", 1)], "{mode:?}: {r:?}");
        assert_eq!(m.orders_rejected_open_limit.get(), 2, "{mode:?}");
        assert_totals(mode, &m, failed(&r));
    }
}

/// Row 50's book rejects, each under its HL reason. Market 3: bid 100 x 2,
/// ask 110 x 2, last trade 105; addr(1) flat there.
#[test]
fn book_rejects_count_under_their_reason_on_every_path() {
    let a = addr(1);
    let with = |tif, p: PlaceOrderParams| PlaceOrderParams { time_in_force: tif, ..p };
    let block = vec![
        place(a, with(TimeInForce::IOC, gtc(3, true, 105, 1))),
        place(a, with(TimeInForce::PostOnly, gtc(3, true, 110, 1))),
        place(a, PlaceOrderParams { order_type: OrderType::Market, ..with(TimeInForce::IOC, gtc(3, true, 106, 1)) }),
        place(a, PlaceOrderParams { reduce_only: true, ..gtc(3, false, 100, 1) }),
        place(a, with(TimeInForce::FOK, gtc(3, true, 110, 5))),
        place(a, PlaceOrderParams { order_type: OrderType::StopMarket { trigger: fp(104) }, ..gtc(3, true, 200, 1) }),
        place(a, with(TimeInForce::IOC, gtc(3, true, 110, 3))), // fills 2: not a reject
    ];
    for mode in MODES {
        let (_d, mut ctx, m) = fresh();
        for t in [3, 4, 5] {
            fund_native(&ctx, &addr(t), fp(1_000_000));
        }
        for action in [
            place(addr(4), gtc(3, false, 105, 1)),
            place(addr(5), gtc(3, true, 105, 1)),
            place(addr(3), gtc(3, true, 100, 2)),
            place(addr(3), gtc(3, false, 110, 2)),
        ] {
            assert!(exec(&mut ctx, mode, std::slice::from_ref(&action))[0].success, "{mode:?}");
        }
        let r = exec(&mut ctx, mode, &block);
        assert_eq!(
            by_reason(&m),
            vec![
                ("reduce_only", 1),
                ("ioc_cancel", 1),
                ("bad_alo_px", 1),
                ("market_no_liquidity", 1),
                ("fok_cancel", 1),
                ("bad_trigger_px", 1),
            ],
            "{mode:?}: {r:?}"
        );
        assert_totals(mode, &m, failed(&r));
    }
}

/// The margin split. Market 3 (20x, tick / lot 1):
/// - P (60) is long 5 @100 (position IM 25) and rests a bid 2 @50 (reserves
///   5): free = 60 - 25 - 5 = 30 (less any fee). A bid 13 @50 needs 32.5:
///   it passes with the 5 released -> `margin_reservation`. A bid 20 @50
///   needs 50 > 35 -> `margin_position`.
/// - U (15), one block: a bid 4 @50 rests (reserves 10, free 5); a second
///   bid 4 @50 needs 10 and passes with the 10 released ->
///   `margin_reservation` (in Phase 2 the reservation is this batch's own);
///   a bid 8 @50 needs 20 > 15 -> `margin_position`.
#[test]
fn margin_rejects_split_position_vs_reservation_on_every_path() {
    let (mk, p, u) = (addr(1), addr(6), addr(7));
    for mode in MODES {
        let (_d, mut ctx, m) = fresh();
        fund_native(&ctx, &p, fp(60));
        fund_native(&ctx, &u, fp(15));
        for action in [place(mk, gtc(3, false, 100, 5)), place(p, gtc(3, true, 100, 5)), place(p, gtc(3, true, 50, 2))]
        {
            let r = exec(&mut ctx, mode, std::slice::from_ref(&action));
            assert!(r[0].success, "{mode:?}: {r:?}");
        }
        assert_eq!(by_reason(&m), vec![], "{mode:?}: setup rejects nothing");

        let r = exec(&mut ctx, mode, &[place(p, gtc(3, true, 50, 13)), place(p, gtc(3, true, 50, 20))]);
        assert!(r.iter().all(|r| !r.success), "{mode:?}: {r:?}");
        assert_eq!(by_reason(&m), vec![("margin_position", 1), ("margin_reservation", 1)], "{mode:?}: {r:?}");

        let r = exec(
            &mut ctx,
            mode,
            &[place(u, gtc(3, true, 50, 4)), place(u, gtc(3, true, 50, 4)), place(u, gtc(3, true, 50, 8))],
        );
        assert!(r[0].success && !r[1].success && !r[2].success, "{mode:?}: {r:?}");
        assert_eq!(by_reason(&m), vec![("margin_position", 2), ("margin_reservation", 2)], "{mode:?}: {r:?}");
        assert_eq!(m.orders_rejected_margin.get(), 4, "{mode:?}: the funnel counter is unsplit");
        assert_totals(mode, &m, 4);
    }
}

/// s114 cloid: a placement whose cloid is live for its sender is counted
/// under `duplicate_cloid` (funnel: `orders_rejected_other`), on the single
/// path (exec_place_order) and the batch engine (Phase 1b): against a
/// resting order in the same market and in another market, and against an
/// earlier placement of the same block. Another trader's cloid is free.
#[test]
fn duplicate_cloids_count_under_duplicate_cloid_on_every_path() {
    let (a, b) = (addr(1), addr(3));
    let with = |cloid: u128, p: PlaceOrderParams| PlaceOrderParams { client_order_id: Some(cloid), ..p };
    let block = vec![
        place(a, with(7, gtc(3, true, 50, 1))),
        place(a, with(7, gtc(1, true, 50, 2))),
        place(b, with(7, gtc(3, true, 50, 1))),
        place(a, with(9, gtc(3, true, 51, 1))),
        place(a, with(9, gtc(3, true, 52, 1))),
    ];
    for mode in MODES {
        let (_d, mut ctx, m) = fresh();
        fund_native(&ctx, &b, fp(1_000_000));
        let setup = place(a, with(7, gtc(3, true, 40, 1)));
        assert!(exec(&mut ctx, mode, std::slice::from_ref(&setup))[0].success, "{mode:?}");
        let r = exec(&mut ctx, mode, &block);
        let ok: Vec<bool> = r.iter().map(|r| r.success).collect();
        assert_eq!(ok, [false, false, true, true, false], "{mode:?}: {r:?}");
        assert_eq!(by_reason(&m), vec![("duplicate_cloid", 3)], "{mode:?}: {r:?}");
        assert_eq!(m.orders_rejected_other.get(), 3, "{mode:?}");
        assert_totals(mode, &m, failed(&r));
    }
}

/// Metrics stay observability only: results are identical without them.
#[test]
fn reason_counters_do_not_change_results() {
    let run = |metered: bool| {
        let (_d, mut ctx, _m) = fresh();
        if !metered {
            ctx.metrics = None;
        }
        fund_native(&ctx, &addr(7), fp(15));
        let block = [
            place(addr(7), gtc(3, true, 50, 4)),
            place(addr(7), gtc(3, true, 50, 4)),
            place(addr(7), gtc(3, true, 50, 8)),
        ];
        let r = exec(&mut ctx, Some(2), &block);
        r.into_iter().map(|r| (r.success, r.reason, r.error.map(String::from))).collect::<Vec<_>>()
    };
    assert_eq!(run(true), run(false));
}
