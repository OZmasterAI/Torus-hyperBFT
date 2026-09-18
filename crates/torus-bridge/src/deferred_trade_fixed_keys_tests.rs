//! Explicit per-context representation fixtures; no global environment mutation.
use super::*;
use crate::market_workers::{MarketBatchResult, MatchResult};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use torus_core::order_book::Fill;
use torus_state::backend::AtomicWriteOp;
use torus_state::cf::{CF_NATIVE_BALANCES, CF_NATIVE_POSITIONS};
use torus_state::StateError;

fn materialized(mut result: Fingerprint) -> Fingerprint {
    for (cf, key, value) in std::mem::take(&mut result.deferred_trades) {
        result.rows.insert((cf.into(), key), value);
    }
    result
}

#[test]
fn fixed_trade_keys_exact_flag() {
    assert!(deferred_trade_fixed_keys_flag(Some("1")));
    for value in [
        None,
        Some("0"),
        Some("true"),
        Some("01"),
        Some(" 1"),
        Some("1 "),
    ] {
        assert!(!deferred_trade_fixed_keys_flag(value));
    }
}

#[test]
fn fixed_trade_keys_full_serial_parallel_inline_deferred_parity_and_zero_pnl() {
    let reference = materialized(run_settlement(false, false, false, false, false));
    assert_eq!((reference.gas, reference.trade_index), (2000, 4));
    assert!(
        reference
            .rows
            .contains_key(&(CF_NATIVE_BALANCES.into(), addr(1).as_slice().to_vec())),
        "zero-PnL closes must still materialize the cold balance row"
    );
    for parallel in [false, true] {
        for deferred in [false, true] {
            let raw = run_settlement(false, false, false, deferred, parallel);
            let fixed = run_settlement(true, false, false, deferred, parallel);
            assert_eq!(
                fixed, raw,
                "mode parity parallel={parallel} deferred={deferred}"
            );
            assert_eq!(materialized(fixed), reference, "canonical rows/results/IDs");
        }
    }
}

#[test]
fn fixed_trade_keys_balance_error_skips_trades_without_changing_failure_prefix() {
    for parallel in [false, true] {
        for deferred in [false, true] {
            let raw = run_settlement(false, true, false, deferred, parallel);
            let fixed = run_settlement(true, true, false, deferred, parallel);
            // Compare representation modes within each settle algorithm. Existing
            // serial/parallel position behavior on a bad balance read differs.
            assert_eq!(fixed, raw);
            assert_eq!((fixed.gas, fixed.trade_index), (0, 0));
            assert!(fixed.deferred_trades.is_empty());
            assert!(fixed
                .results
                .iter()
                .all(|(_, ok, error, _)| !ok && error.is_some()));
            assert!(!fixed
                .rows
                .keys()
                .any(|(cf, _)| cf == CF_NATIVE_TRADES || cf == CF_NATIVE_USER_TRADES));
        }
    }
}

#[test]
fn fixed_trade_keys_position_failure_fallback_preserves_outcome() {
    for deferred in [false, true] {
        let raw = run_settlement(false, false, true, deferred, true);
        let fixed = run_settlement(true, false, true, deferred, true);
        assert_eq!(fixed, raw);
        assert!(fixed.deferred_trades.is_empty());
        assert_eq!(fixed.trade_index, 0);
    }
}

#[test]
fn fixed_trade_keys_compatibility_and_typed_drains_preserve_mode_and_append_order() {
    let mut ctx = NativeExecContext::new_with_mode(
        MemoryBackend::default(),
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
    ctx.pending_trades = DeferredTradeBatch::empty(true);
    ctx.defer_trades = true;
    let fill = Fill {
        maker_order_id: 1,
        taker_order_id: 2,
        price: fp(100),
        quantity: fp(1),
        maker: addr(2),
        taker: addr(1),
        maker_side: Side::Sell,
        timestamp: 1000,
    };
    let expected: Vec<_> = (0..3)
        .flat_map(|index| {
            let kvs = TradeKvs::build(1, 7, 1000, index, &fill);
            [
                (CF_NATIVE_TRADES, kvs.trade_key.to_vec(), kvs.trade_data),
                (
                    CF_NATIVE_USER_TRADES,
                    kvs.maker_key.to_vec(),
                    kvs.maker_data,
                ),
                (
                    CF_NATIVE_USER_TRADES,
                    kvs.taker_key.to_vec(),
                    kvs.taker_data,
                ),
            ]
        })
        .collect();
    NativeExecutor::persist_trade(&mut ctx, 1, &fill);
    NativeExecutor::persist_trade(&mut ctx, 1, &fill);
    assert_eq!(ctx.take_pending_trades(), expected[..6]);
    assert!(matches!(&ctx.pending_trades, DeferredTradeBatch::Fixed(rows) if rows.is_empty()));
    NativeExecutor::persist_trade(&mut ctx, 1, &fill);
    let typed = ctx.take_pending_trade_batch();
    assert!(matches!(&typed, DeferredTradeBatch::Fixed(rows) if rows.len() == 3));
    assert_eq!(typed.into_raw(), expected[6..]);
    assert_eq!(ctx.trade_index, 3);
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
    fixed_keys: bool,
    fail_balance: bool,
    fail_positions: bool,
    defer_trades: bool,
    parallel: bool,
) -> Fingerprint {
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
    ctx.pending_trades = DeferredTradeBatch::empty(fixed_keys);
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
                // Keep the cold row absent until the zero-PnL event: a
                // preceding margin release would also materialize it.
                margin_reserved: FixedPoint::ZERO,
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
    if parallel {
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
    } else {
        NativeExecutor::settle_market_results_sequential(
            &mut ctx,
            market_results,
            &market_batches,
            &mut results,
            &mut gas,
            &mut balances,
            &mut positions,
        );
    }
    balances.flush_all(&ctx.positions).unwrap();
    positions.flush_all(&ctx.positions).unwrap();
    ctx.save_order_books();
    assert!(ctx.fatal_error.is_none());
    let rows = backend.0.lock().unwrap().rows.clone();
    let batch = ctx.take_pending_trade_batch();
    assert_eq!(matches!(&batch, DeferredTradeBatch::Fixed(_)), fixed_keys);
    assert!(ctx.take_pending_trades().is_empty());
    Fingerprint {
        rows,
        results: results
            .into_iter()
            .map(|r| (r.action_type, r.success, r.error, r.gas_used))
            .collect(),
        gas,
        trade_index: ctx.trade_index,
        next_order_id: ctx.next_global_order_id,
        deferred_trades: batch.into_raw(),
    }
}
