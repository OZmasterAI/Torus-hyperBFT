use super::*;
use crate::market_workers::{MarketBatchResult, MatchResult};
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use torus_core::order_book::Fill;
use torus_state::backend::AtomicWriteOp;
use torus_state::cf::{CF_NATIVE_BALANCES, CF_NATIVE_POSITIONS};
use torus_state::StateError;

thread_local! {
    static ENABLED: Cell<Option<bool>> = const { Cell::new(None) };
    static REPORTS: RefCell<Vec<SettlePassBDiagnostic>> = const { RefCell::new(Vec::new()) };
}

pub(super) fn enabled_override() -> Option<bool> {
    ENABLED.with(Cell::get)
}
pub(super) fn record(report: SettlePassBDiagnostic) {
    REPORTS.with(|r| r.borrow_mut().push(report));
}

fn capture<R>(enabled: bool, work: impl FnOnce() -> R) -> (R, Vec<SettlePassBDiagnostic>) {
    struct Restore(Option<bool>);
    impl Drop for Restore {
        fn drop(&mut self) {
            ENABLED.with(|mode| mode.set(self.0));
        }
    }
    let _restore = Restore(ENABLED.with(|mode| mode.replace(Some(enabled))));
    REPORTS.with(|reports| reports.borrow_mut().clear());
    let result = work();
    (
        result,
        REPORTS.with(|reports| std::mem::take(&mut *reports.borrow_mut())),
    )
}

#[derive(Default)]
struct MemoryState {
    rows: BTreeMap<(String, Vec<u8>), Vec<u8>>,
    fail_taker_balances: bool,
    fail_positions: bool,
}

#[derive(Clone, Default)]
struct MemoryBackend(Arc<Mutex<MemoryState>>);

impl StateBackend for MemoryBackend {
    fn get_cf_raw(&self, cf: &str, key: &[u8]) -> Result<Option<Vec<u8>>, StateError> {
        let state = self.0.lock().unwrap();
        if (state.fail_taker_balances && cf == CF_NATIVE_BALANCES && key == addr(1).as_slice())
            || (state.fail_positions && cf == CF_NATIVE_POSITIONS)
        {
            return Err(StateError::InvalidData(
                "injected diagnostic fixture read failure".into(),
            ));
        }
        Ok(state.rows.get(&(cf.into(), key.to_vec())).cloned())
    }
    fn put_cf_raw(&self, cf: &str, key: &[u8], value: &[u8]) -> Result<(), StateError> {
        self.0
            .lock()
            .unwrap()
            .rows
            .insert((cf.into(), key.to_vec()), value.to_vec());
        Ok(())
    }
    fn delete_cf_raw(&self, cf: &str, key: &[u8]) -> Result<(), StateError> {
        self.0
            .lock()
            .unwrap()
            .rows
            .remove(&(cf.into(), key.to_vec()));
        Ok(())
    }
    fn iterate_cf(
        &self,
        cf: &str,
        prefix: Option<&[u8]>,
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>, StateError> {
        Ok(self
            .0
            .lock()
            .unwrap()
            .rows
            .iter()
            .filter(|((name, key), _)| name == cf && prefix.map_or(true, |p| key.starts_with(p)))
            .map(|((_, key), value)| (key.clone(), value.clone()))
            .collect())
    }
    fn atomic_write(&self, ops: &[AtomicWriteOp<'_>]) -> Result<(), StateError> {
        assert!(ops.is_empty(), "fixture expects ordinary manager writes");
        Ok(())
    }
}

fn addr(n: u8) -> Address {
    Address::from([n; 20])
}
fn fp(n: i64) -> FixedPoint {
    FixedPoint::from_raw(n as i128 * FixedPoint::SCALE)
}
fn params(market_id: MarketId) -> PlaceOrderParams {
    PlaceOrderParams {
        market_id,
        is_buy: false,
        price: fp(100),
        quantity: fp(2),
        order_type: OrderType::Limit,
        time_in_force: TimeInForce::IOC,
        reduce_only: false,
        client_order_id: None,
    }
}

fn market_result(market_id: MarketId, fills: Vec<Fill>) -> MarketBatchResult {
    MarketBatchResult {
        market_id,
        book: OrderBook::new(market_id, FixedPoint::ONE, FixedPoint::ONE),
        next_order_id: 400 + market_id as u128,
        results: vec![MatchResult {
            sender: addr(1),
            order_id: 300 + market_id as u128,
            result: PlaceResult {
                order_id: 300 + market_id as u128,
                status: OrderStatus::Filled,
                fills,
                self_trade_cancels: vec![],
            },
        }],
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Fingerprint {
    rows: BTreeMap<(String, Vec<u8>), Vec<u8>>,
    results: Vec<(&'static str, bool, Option<String>, u64)>,
    gas: u64,
    trade_index: u32,
    next_order_id: u128,
    deferred_trades: Vec<RawCfKv>,
}

fn run_settlement(
    enabled: bool,
    fail_balance: bool,
    fail_positions: bool,
    defer_trades: bool,
) -> (Fingerprint, Vec<SettlePassBDiagnostic>) {
    capture(enabled, || {
        let backend = MemoryBackend::default();
        let mut ctx = NativeExecContext::new_with_mode(
            backend.clone(),
            7,
            1000,
            0,
            100,
            4,
            addr(99),
            addr(100),
            addr(101),
            BookMode::LevelAuthorityChunked,
            None,
        );
        ctx.defer_trades = defer_trades;
        assert!(ctx.fatal_error.is_none());
        let orders = [params(1), params(2)];
        let mut market_batches = HashMap::new();
        let mut market_results = Vec::new();
        let mut seed = PositionCache::new();
        for (index, params) in orders.iter().enumerate() {
            let market = params.market_id;
            // Open a long position, without a balance row. Both closing fills
            // below produce Some(ZERO), so zero-PnL events must materialize it.
            assert!(ctx
                .positions
                .apply_fill_cached(
                    &mut seed,
                    &addr(1),
                    market,
                    true,
                    fp(2),
                    fp(100),
                    MarginType::Cross
                )
                .unwrap()
                .is_none());
            market_batches.insert(
                market,
                vec![PreparedOrder {
                    index,
                    sender: addr(1),
                    params,
                    order_id: 300 + market as u128,
                    margin_reserved: fp(10),
                }],
            );
            let fills = [2, 3]
                .into_iter()
                .map(|maker| Fill {
                    maker_order_id: market as u128 * 10 + maker as u128,
                    taker_order_id: 300 + market as u128,
                    price: fp(100),
                    quantity: fp(1),
                    maker: addr(maker),
                    taker: addr(1),
                    maker_side: Side::Buy,
                    timestamp: 1000,
                })
                .collect();
            market_results.push(market_result(market, fills));
        }
        seed.flush_all(&ctx.positions).unwrap();
        {
            let mut state = backend.0.lock().unwrap();
            state.fail_taker_balances = fail_balance;
            state.fail_positions = fail_positions;
        }
        let mut results = vec![NativeActionResult::ok("pending", 0); 2];
        let mut gas = 0;
        let mut balances = BalanceCache::new();
        let mut positions = PositionCache::new();
        NativeExecutor::settle_market_results_parallel(
            &mut ctx,
            market_results,
            &market_batches,
            &mut results,
            &mut gas,
            &mut balances,
            &mut positions,
            2,
        );
        balances.flush_all(&ctx.positions).unwrap();
        positions.flush_all(&ctx.positions).unwrap();
        ctx.save_order_books();
        assert!(ctx.fatal_error.is_none());
        let rows = backend.0.lock().unwrap().rows.clone();
        Fingerprint {
            rows,
            results: results
                .into_iter()
                .map(|r| (r.action_type, r.success, r.error, r.gas_used))
                .collect(),
            gas,
            trade_index: ctx.trade_index,
            next_order_id: ctx.next_global_order_id,
            deferred_trades: ctx.pending_trades,
        }
    })
}

fn assert_partition(report: &SettlePassBDiagnostic) {
    assert!(report.timing_valid);
    assert_eq!(
        report.pass_b_ns,
        report.position_merge_ns
            + report.balance_apply_ns
            + report.trade_route_ns
            + report.residual_ns
    );
}

#[test]
fn exact_toggle_and_invalid_partition_are_explicit() {
    for raw in [
        None,
        Some(""),
        Some("0"),
        Some("true"),
        Some(" 1"),
        Some("1 "),
    ] {
        assert!(!parse_settle_passb_diagnostic(raw));
    }
    assert!(parse_settle_passb_diagnostic(Some("1")));
    let mut report = SettlePassBDiagnostic {
        position_merge_ns: 3,
        balance_apply_ns: 7,
        trade_route_ns: 11,
        ..SettlePassBDiagnostic::default()
    };
    report.finish(30);
    assert_partition(&report);
    assert_eq!(report.residual_ns, 9);
    report.finish(20);
    assert!(
        !report.timing_valid,
        "an impossible sum must never be reported valid"
    );
}

#[test]
fn planned_concentration_counts_zero_events_and_reports_top_four_only() {
    let order = params(1);
    let batches = HashMap::from([(
        1,
        vec![PreparedOrder {
            index: 0,
            sender: addr(1),
            params: &order,
            order_id: 301,
            margin_reserved: fp(1),
        }],
    )]);
    let pnl_events = [5, 4, 3, 2, 1, 1]
        .into_iter()
        .enumerate()
        .flat_map(|(i, count)| {
            std::iter::repeat(("taker", addr(i as u8 + 1), FixedPoint::ZERO)).take(count)
        })
        .collect();
    let plan = MarketSettlePlan {
        orders: vec![OrderSettlePlan {
            margin_release: fp(1),
            pnl_events,
            fill_error: None,
            trades: vec![],
        }],
        pos_cache: PositionCache::new(),
        maker_releases: vec![(addr(6), FixedPoint::ZERO)],
    };
    let report = SettlePassBDiagnostic::inventory(&[market_result(1, vec![])], &[plan], &batches);
    assert_eq!(report.planned_balance_events, 18);
    assert_eq!(report.planned_unique_senders, 6);
    assert_eq!(report.planned_top1_events, 6);
    assert_eq!(report.planned_top4_events, 15);
    assert_eq!(
        report.balance_attempts, 0,
        "inventory never applies a balance event"
    );
}

#[test]
fn enabled_diagnostic_preserves_all_rows_ids_gas_and_both_trade_routes() {
    for deferred in [false, true] {
        let (control, off_reports) = run_settlement(false, false, false, deferred);
        let (observed, reports) = run_settlement(true, false, false, deferred);
        assert_eq!(control, observed);
        assert!(off_reports.is_empty());
        assert_eq!(reports.len(), 1);
        let report = &reports[0];
        assert_partition(report);
        assert_eq!(
            (
                report.markets_merged,
                report.orders_visited,
                report.failed_orders
            ),
            (2, 2, 0)
        );
        assert_eq!(
            (
                report.balance_attempts,
                report.balance_read_errors,
                report.trade_route_calls
            ),
            (10, 0, 4)
        );
        assert_eq!(
            (
                report.planned_balance_events,
                report.planned_unique_senders,
                report.planned_top1_events,
                report.planned_top4_events
            ),
            (10, 3, 6, 10)
        );
        assert_eq!((observed.gas, observed.trade_index), (2000, 4));
        assert_eq!(
            observed
                .rows
                .get(&(CF_NATIVE_BALANCES.into(), addr(1).as_slice().to_vec())),
            Some(&borsh::to_vec(&NativeBalance::default()).unwrap()),
            "zero PnL/clamped releases still materialize"
        );
    }
}

#[test]
fn failed_balance_reads_close_span_before_continue_and_truncate_actual_counts() {
    let (control, _) = run_settlement(false, true, false, false);
    let (observed, reports) = run_settlement(true, true, false, false);
    assert_eq!(control, observed);
    assert_eq!(reports.len(), 1);
    let report = &reports[0];
    assert_partition(report);
    assert_eq!(
        (
            report.orders_visited,
            report.failed_orders,
            report.trade_route_calls
        ),
        (2, 2, 0)
    );
    assert_eq!(
        (report.balance_attempts, report.balance_read_errors),
        (8, 4)
    );
    assert_eq!(
        report.planned_balance_events, 10,
        "planned includes later zero PnL skipped after first error"
    );
    assert_eq!((observed.gas, observed.trade_index), (0, 0));
    assert!(observed
        .results
        .iter()
        .all(|(_, ok, error, _)| !ok && error.as_ref().unwrap().starts_with("taker fill failed:")));
}

#[test]
fn position_failure_falls_back_without_mislabeling_sequential_work_as_parallel_pass_b() {
    let (control, off_reports) = run_settlement(false, false, true, false);
    let (observed, on_reports) = run_settlement(true, false, true, false);
    assert_eq!(control, observed);
    assert!(off_reports.is_empty() && on_reports.is_empty());
    assert!(observed.results.iter().all(|(_, ok, _, _)| !ok));
}
