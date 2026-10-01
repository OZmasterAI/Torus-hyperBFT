//! Item 3: Hyperliquid-style liquidation — `docs/plans/liquidation.md`,
//! `docs/plans/liquidation-impl.md`. Helpers copied from oracle_block_tests.rs,
//! account_margin_tests.rs and market_order_margin_tests.rs.

use alloy_primitives::Address;

use torus_bridge::native_executor::{NativeExecContext, NativeExecutor};
use torus_core::position::NativeBalance;
use torus_state::cf::{CF_NATIVE_BALANCES, CF_NATIVE_MARKETS, CF_NATIVE_POSITIONS};
use torus_state::{StateBackend, StateDb};
use torus_types::{FixedPoint, MarketId, NativeAction, OrderType, PlaceOrderParams, TimeInForce};

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

/// Block `height`, timestamp `1_000 + height` (one second per block).
fn ctx_at(db: StateDb, height: u64) -> NativeExecContext {
    NativeExecContext::new(db, height, 1_000 + height, 0, 1_000, 10, addr(99), addr(100), addr(101))
}

fn market_row(initial_margin: FixedPoint) -> Vec<u8> {
    borsh::to_vec(&("BTC".to_string(), "USDC".to_string(), fp(1).raw(), fp(1).raw(), initial_margin.raw()))
        .unwrap()
}

fn fund(ctx: &NativeExecContext, t: &Address, amount: FixedPoint) {
    ctx.positions
        .put_native_balance(t, &NativeBalance { available: amount, order_margin: FixedPoint::ZERO })
        .unwrap();
}

fn limit(m: MarketId, is_buy: bool, price: i64, qty: i64) -> PlaceOrderParams {
    PlaceOrderParams {
        market_id: m,
        is_buy,
        price: fp(price),
        quantity: fp(qty),
        order_type: OrderType::Limit,
        time_in_force: TimeInForce::GTC,
        reduce_only: false,
        client_order_id: None,
    }
}

/// The aggregated mark of `m` at the context's block (3 equal reporters).
fn set_mark(ctx: &NativeExecContext, m: MarketId, price: FixedPoint) {
    let reporters = [addr(150), addr(151), addr(152)];
    for v in &reporters {
        ctx.oracle.submit_price(v, m, price, ctx.block_height, ctx.timestamp).unwrap();
    }
    let stakes: Vec<(Address, FixedPoint)> = reporters.iter().map(|v| (*v, fp(1))).collect();
    assert_eq!(ctx.oracle.aggregate_price(m, ctx.block_height, ctx.timestamp, &stakes).unwrap(), price);
}

// ---- T4: margin configs from market rows ----

/// F2/F8: configs come from CF_NATIVE_MARKETS — max leverage = floor(100 /
/// initial_margin %), one flat tier; undecodable / non-positive rows: none.
#[test]
fn margin_configs_load_from_market_rows() {
    let (_d, db) = open_test_db();
    let rows: [(u64, Vec<u8>); 6] = [
        (1, market_row(fp(5))),             // 20x
        (2, market_row(fp(2))),             // 50x
        (3, market_row(fp(3))),             // 33.3 -> 33x
        (4, market_row(fp(200))),           // 0.5 -> clamped to 1x
        (5, market_row(FixedPoint::ZERO)),  // none
        (6, b"listed".to_vec()),            // undecodable: none
    ];
    for (m, row) in &rows {
        db.put_cf_raw(CF_NATIVE_MARKETS, &m.to_be_bytes(), row).unwrap();
    }
    let ctx = ctx_at(db, 1);
    assert!(ctx.fatal_error.is_none());
    let cfg = |m: MarketId| {
        ctx.margin_configs.get(&m).map(|c| {
            assert_eq!(c.tiers.len(), 1, "one flat tier");
            assert_eq!(c.tiers[0].max_notional, FixedPoint::MAX);
            assert_eq!(c.tiers[0].max_leverage, c.max_leverage);
            c.max_leverage
        })
    };
    assert_eq!([cfg(1), cfg(2), cfg(3), cfg(4), cfg(5), cfg(6)], [Some(20), Some(50), Some(33), Some(1), None, None]);
}

/// D11: for a 20x market the loaded config is byte-identical to no config
/// (today's production IM) — single and sharded batch paths.
#[test]
fn twenty_x_market_config_is_byte_identical_to_no_config() {
    let run = |row: &[u8], workers: usize| {
        let (dir, db) = open_test_db();
        db.put_cf_raw(CF_NATIVE_MARKETS, &1u64.to_be_bytes(), row).unwrap();
        let mut ctx = ctx_at(db.clone(), 1);
        for t in [addr(1), addr(2), addr(3), addr(200)] {
            fund(&ctx, &t, fp(1_000_000));
        }
        set_mark(&ctx, 1, fp(1_000));
        let mkt = |is_buy: bool, cap: i64, qty: i64| PlaceOrderParams {
            order_type: OrderType::Market,
            time_in_force: TimeInForce::IOC,
            ..limit(1, is_buy, cap, qty)
        };
        let acts = vec![
            (addr(1), NativeAction::PlaceOrder(limit(1, true, 990, 7))),
            (addr(2), NativeAction::PlaceOrder(limit(1, false, 1_010, 5))),
            (addr(3), NativeAction::PlaceOrder(mkt(false, 900, 4))),
            (addr(3), NativeAction::PlaceOrder(mkt(true, 1_100, 6))),
            (addr(200), NativeAction::PlaceOrder(limit(9, true, 1, 1))), // filler (2 senders)
        ];
        let r = NativeExecutor::execute_batch_engine_mode(&mut ctx, &acts, workers).results;
        assert!(r.iter().all(|x| x.success));
        let dump = |cf| db.iterate_cf(cf, None).unwrap();
        (dump(CF_NATIVE_BALANCES), dump(CF_NATIVE_POSITIONS), dir)
    };
    for workers in [1, 4] {
        let (b1, p1, _d1) = run(&market_row(fp(5)), workers);
        let (b2, p2, _d2) = run(b"listed", workers);
        assert_eq!(b1, b2, "balances, {workers} workers");
        assert_eq!(p1, p2, "positions, {workers} workers");
    }
}
