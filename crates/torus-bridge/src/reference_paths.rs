//! Item 6 Phase 2: frozen reference paths, compiled only for tests (`cfg(test)`)
//! and under the test-only `test-reference-paths` feature (plan 9.8), which
//! torus-consensus enables from its dev-dependencies so the app-level
//! differentials can switch to them. No node build compiles this module.
//! `NativeExecContext::test_cancel_all_full_scan` / `test_flush_per_row` route
//! here (both default to off).

use super::*;

/// Item 6 Phase 2 step 0.4: the full-scan cancel-all, frozen as the
/// reference for P2-1 (`NativeExecContext::test_cancel_all_full_scan` routes
/// every cancel-all here): every book in ascending id, stops then orders,
/// dirty marks and the margin sum (`cancel_orders_and_stops` as of
/// `d3ba3c0a`, without the step 0.2 counters).
pub(super) fn cancel_orders_and_stops_full_scan<T: StateBackend>(
    ctx: &mut NativeExecContext<T>,
    trader: &Address,
    market: Option<MarketId>,
) -> FixedPoint {
    let market_ids: Vec<MarketId> = match market {
        Some(m) => vec![m],
        None => {
            let mut v: Vec<MarketId> = ctx.order_books.keys().copied().collect();
            v.sort_unstable();
            v
        }
    };
    let mut total = FixedPoint::ZERO;
    for mid in market_ids {
        let Some(book) = ctx.order_books.get_mut(&mid) else {
            continue;
        };
        let stops = book.take_pending_stops(trader);
        let cancelled = book.cancel_all(*trader, market);
        if stops.is_empty() && cancelled.is_empty() {
            continue;
        }
        ctx.dirty_books.insert(mid);
        let cfg = ctx.margin_configs.get(&mid);
        total += NativeExecutor::cancelled_orders_margin(cfg, &cancelled);
        for &(price, qty) in &stops {
            total += NativeExecutor::stop_reservation(cfg, price, qty);
        }
    }
    total
}
