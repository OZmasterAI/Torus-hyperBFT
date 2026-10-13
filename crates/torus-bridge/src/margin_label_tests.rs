//! s118 (Phase 3 step 2 follow-up, review of c88d3073): the funnel label of a
//! pre-book margin reject ([`margin_reject_label`]: `margin_position` vs
//! `margin_reservation`) re-runs the account check with the reservations
//! released. That second check only feeds the reason metric, so it runs
//! only with metrics attached; without them every placement runs exactly one
//! account check. The results (reason and message, consensus-visible) are
//! the same either way, on the single-action path and the batch engine
//! (serial and sharded Phase 2).

use super::*;
use std::sync::Arc;
use torus_core::position::NativeBalance;
use torus_telemetry::Metrics;
use torus_types::{OrderType, PlaceOrderParams, TimeInForce};

fn addr(n: u8) -> Address {
    Address::new([n; 20])
}

fn fp(v: i64) -> FixedPoint {
    FixedPoint::from_raw(v as i128 * FixedPoint::SCALE)
}

fn gtc(market_id: MarketId, is_buy: bool, price: i64, qty: i64) -> PlaceOrderParams {
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

fn place(sender: Address, order: PlaceOrderParams) -> (Address, NativeAction) {
    (sender, NativeAction::PlaceOrder(order))
}

fn fund(ctx: &NativeExecContext, trader: &Address, amount: i64) {
    let bal = NativeBalance {
        available: fp(amount),
        order_margin: FixedPoint::ZERO,
    };
    ctx.positions.put_native_balance(trader, &bal).unwrap();
}

/// `None` = the single-action path, `Some(t)` = the batch engine with `t`
/// Phase 2 threads (0 = serial).
type Mode = Option<usize>;
const MODES: [Mode; 4] = [None, Some(0), Some(2), Some(4)];

fn exec(
    ctx: &mut NativeExecContext,
    mode: Mode,
    block: &[(Address, NativeAction)],
) -> Vec<NativeActionResult> {
    let r: Vec<_> = match mode {
        None => block
            .iter()
            .map(|(s, a)| NativeExecutor::execute(ctx, s, a))
            .collect(),
        Some(t) => NativeExecutor::execute_batch_engine_mode(ctx, block, t).results,
    };
    assert!(ctx.fatal_error.is_none(), "{:?}", ctx.fatal_error);
    r
}

fn checks() -> u64 {
    ACCOUNT_CHECKS.with(|c| c.get())
}

/// What a client and the action status see of a result.
type Seen = (bool, FailureReason, Option<String>);

/// The reject scenario of `reject_reason_metrics_tests`'
/// `margin_rejects_split_position_vs_reservation_on_every_path` (market 3,
/// 20x, tick / lot 1): P (60) long 5 @100 resting a bid 2 @50 sends a bid
/// 13 @50 (reservation) and 20 @50 (position); U (15) sends three bids 4,
/// 4, 8 @50 in one block (the first rests; reservation, position). Every
/// reject has a reservation, so with metrics each runs the label's second
/// check. Returns the two reject blocks' results, the account checks they
/// ran (all on this thread: one sender per block, so no Phase 2 worker) and
/// the non-zero margin reason series.
fn margin_rejects(mode: Mode, metrics: bool) -> (Vec<Seen>, u64, Vec<(&'static str, u64)>) {
    let (mk, p, u) = (addr(1), addr(6), addr(7));
    let dir = tempfile::tempdir().unwrap();
    let db = StateDb::open(dir.path()).unwrap();
    let mut ctx = NativeExecContext::new(db, 1, 1000, 0, 100, 10, addr(99), addr(100), addr(101));
    fund(&ctx, &mk, 1_000_000);
    fund(&ctx, &p, 60);
    fund(&ctx, &u, 15);
    let m = Arc::new(Metrics::new());
    if metrics {
        ctx.metrics = Some(m.clone());
    }
    for action in [
        place(mk, gtc(3, false, 100, 5)),
        place(p, gtc(3, true, 100, 5)),
        place(p, gtc(3, true, 50, 2)),
    ] {
        let r = exec(&mut ctx, mode, std::slice::from_ref(&action));
        assert!(r[0].success, "{mode:?}: {r:?}");
    }
    let before = checks();
    let mut seen = Vec::new();
    for block in [
        vec![
            place(p, gtc(3, true, 50, 13)),
            place(p, gtc(3, true, 50, 20)),
        ],
        vec![
            place(u, gtc(3, true, 50, 4)),
            place(u, gtc(3, true, 50, 4)),
            place(u, gtc(3, true, 50, 8)),
        ],
    ] {
        for r in exec(&mut ctx, mode, &block) {
            seen.push((r.success, r.reason, r.error.map(|e| e.into_owned())));
        }
    }
    let ran = checks() - before;
    let series = [
        torus_telemetry::OrderRejectReason::MarginPosition,
        torus_telemetry::OrderRejectReason::MarginReservation,
    ]
    .iter()
    .map(|r| (r.as_str(), m.orders_rejected_reason[*r as usize].get()))
    .filter(|(_, n)| *n > 0)
    .collect();
    (seen, ran, series)
}

/// Metrics off: one account check per placement (5), none for the label;
/// the results are the ones with metrics on, reason and message included.
#[test]
fn metrics_off_margin_reject_runs_no_label_check() {
    for mode in MODES {
        let (off, ran, _) = margin_rejects(mode, false);
        assert_eq!(ran, 5, "{mode:?}: one account check per placement");
        let (on, _, _) = margin_rejects(mode, true);
        assert_eq!(off, on, "{mode:?}: results equal with and without metrics");
        let rejects: Vec<_> = off.iter().filter(|s| !s.0).collect();
        assert_eq!(rejects.len(), 4, "{mode:?}: {off:?}");
        assert!(
            rejects
                .iter()
                .all(|s| s.1 == FailureReason::Margin && s.2.is_some()),
            "{mode:?}: {off:?}"
        );
    }
}

/// Metrics on: the label's second check runs once per margin reject (4 on
/// top of the 5) and splits the rejects as before: 2 position, 2
/// reservation.
#[test]
fn metrics_on_margin_reject_label_splits_as_before() {
    for mode in MODES {
        let (_, ran, series) = margin_rejects(mode, true);
        assert_eq!(ran, 5 + 4, "{mode:?}: one label check per margin reject");
        assert_eq!(
            series,
            vec![("margin_position", 2), ("margin_reservation", 2)],
            "{mode:?}"
        );
    }
}
