//! Liquidation safety tests (task 2.3.5).

use torus_core::liquidation::{LiquidationEngine, LiquidationMethod};
use torus_core::margin::MarketMarginConfig;
use torus_core::position::{MarginType, NativeBalance, Position, PositionManager};
use torus_state::StateDb;
use torus_types::{Address, FixedPoint, MarketId};

fn fp(v: i64) -> FixedPoint {
    FixedPoint::from_raw(v as i128 * FixedPoint::SCALE)
}

fn addr(n: u8) -> Address {
    Address::new([n; 20])
}

fn setup() -> (tempfile::TempDir, PositionManager) {
    let dir = tempfile::tempdir().unwrap();
    let db = StateDb::open(dir.path()).unwrap();
    (dir, PositionManager::new(db))
}

fn make_cross_long(
    pm: &PositionManager,
    trader: &Address,
    market_id: MarketId,
    size: FixedPoint,
    entry_price: FixedPoint,
) {
    pm.put_position(&Position {
        trader: *trader,
        market_id,
        is_long: true,
        size,
        entry_price,
        realized_pnl: FixedPoint::ZERO,
        isolated_margin: FixedPoint::ZERO,
        margin_type: MarginType::Cross,
    })
    .unwrap();
}

fn set_balance(pm: &PositionManager, trader: &Address, amount: FixedPoint) {
    pm.put_native_balance(
        trader,
        &NativeBalance {
            available: amount,
            order_margin: FixedPoint::ZERO,
        },
    )
    .unwrap();
}

// ============================================================================
// ForceClose when equity covers loss
// ============================================================================

#[test]
fn force_close_equity_covers_loss() {
    let (_dir, pm) = setup();
    let trader = addr(1);
    let market: MarketId = 1;
    let config = MarketMarginConfig::new(market, 20);

    // Trader has 1,000 balance, long 1 BTC at 50,000
    set_balance(&pm, &trader, fp(1_000));
    make_cross_long(&pm, &trader, market, fp(1), fp(50_000));

    // Price drops to 48,500 → PnL = -1,500 → equity = -500 < maintenance
    let oracle_prices = vec![(market, fp(48_500))];
    let traders = vec![trader];

    let liqs =
        LiquidationEngine::check_liquidations(&pm, &traders, &config, &oracle_prices).unwrap();
    assert!(!liqs.is_empty());

    let result =
        LiquidationEngine::execute_liquidation(&pm, &liqs[0], fp(48_500)).unwrap();
    assert_eq!(result.method, LiquidationMethod::ForceClose);
    assert_eq!(result.size, fp(1));

    // Position deleted
    assert!(pm.get_position(&trader, market).unwrap().is_none());

    // PnL = -1500, balance was 1000 → -500 → clamped to 0, deficit = 500
    assert_eq!(result.remaining_deficit, fp(500));
    let bal = pm.get_native_balance(&trader).unwrap();
    assert_eq!(bal.available, FixedPoint::ZERO);
}

// ============================================================================
// ADL trigger when equity doesn't cover loss
// ============================================================================

#[test]
fn adl_covers_deficit() {
    let (_dir, pm) = setup();
    let liquidated = addr(1);
    let profitable1 = addr(2);
    let profitable2 = addr(3);
    let market: MarketId = 1;

    // Profitable: long 2 BTC at 45k (profit at 50k = 10k)
    make_cross_long(&pm, &profitable1, market, fp(2), fp(45_000));
    set_balance(&pm, &profitable1, fp(5_000));

    // Profitable: long 1 BTC at 48k (profit at 50k = 2k)
    make_cross_long(&pm, &profitable2, market, fp(1), fp(48_000));
    set_balance(&pm, &profitable2, fp(3_000));

    let oracle_price = fp(50_000);
    let loss = fp(500);

    let traders = vec![liquidated, profitable1, profitable2];
    let results =
        LiquidationEngine::auto_deleverage(&pm, market, loss, oracle_price, &traders).unwrap();

    assert!(!results.is_empty());

    let total_realized: FixedPoint = results
        .iter()
        .map(|r| r.realized_pnl)
        .fold(FixedPoint::ZERO, |a, b| a + b);
    assert!(total_realized > FixedPoint::ZERO);
}

// ============================================================================
// Socialized loss when ADL is insufficient
// ============================================================================

#[test]
fn socialized_loss_spreads_deficit() {
    let (_dir, pm) = setup();
    let trader1 = addr(1);
    let trader2 = addr(2);
    let market: MarketId = 1;

    make_cross_long(&pm, &trader1, market, fp(3), fp(50_000));
    set_balance(&pm, &trader1, fp(10_000));

    make_cross_long(&pm, &trader2, market, fp(1), fp(50_000));
    set_balance(&pm, &trader2, fp(5_000));

    let remaining_loss = fp(400);
    let traders = vec![trader1, trader2];

    LiquidationEngine::socialize_loss(&pm, market, remaining_loss, &traders).unwrap();

    let bal1 = pm.get_native_balance(&trader1).unwrap();
    let bal2 = pm.get_native_balance(&trader2).unwrap();

    // trader1: 10000 - 400*(3/4) = 9700
    // trader2: 5000 - 400*(1/4) = 4900
    assert_eq!(bal1.available, fp(9_700));
    assert_eq!(bal2.available, fp(4_900));
}

// ============================================================================
// Multiple positions (cross-margin liquidation)
// ============================================================================

#[test]
fn cross_margin_multiple_positions() {
    let (_dir, pm) = setup();
    let trader = addr(1);
    let config = MarketMarginConfig::new(1, 20);

    make_cross_long(&pm, &trader, 1, fp(1), fp(50_000));
    pm.put_position(&Position {
        trader,
        market_id: 2,
        is_long: false,
        size: fp(1),
        entry_price: fp(3_000),
        realized_pnl: FixedPoint::ZERO,
        isolated_margin: FixedPoint::ZERO,
        margin_type: MarginType::Cross,
    })
    .unwrap();

    set_balance(&pm, &trader, fp(500));

    let oracle_prices = vec![(1u64, fp(48_500)), (2u64, fp(3_500))];
    let traders = vec![trader];

    let liqs =
        LiquidationEngine::check_liquidations(&pm, &traders, &config, &oracle_prices).unwrap();

    assert_eq!(liqs.len(), 2);
    assert!(liqs.iter().all(|l| l.margin_type == MarginType::Cross));
}

// ============================================================================
// Isolated-margin: only one position liquidated, others untouched
// ============================================================================

#[test]
fn isolated_only_affected_position_liquidated() {
    let (_dir, pm) = setup();
    let trader = addr(1);
    let config = MarketMarginConfig::new(1, 20);

    pm.put_position(&Position {
        trader,
        market_id: 1,
        is_long: true,
        size: fp(1),
        entry_price: fp(50_000),
        realized_pnl: FixedPoint::ZERO,
        isolated_margin: fp(100),
        margin_type: MarginType::Isolated,
    })
    .unwrap();

    pm.put_position(&Position {
        trader,
        market_id: 2,
        is_long: true,
        size: fp(1),
        entry_price: fp(3_000),
        realized_pnl: FixedPoint::ZERO,
        isolated_margin: fp(10_000),
        margin_type: MarginType::Isolated,
    })
    .unwrap();

    set_balance(&pm, &trader, fp(50_000));

    let oracle_prices = vec![(1u64, fp(45_000)), (2u64, fp(3_500))];
    let traders = vec![trader];

    let liqs =
        LiquidationEngine::check_liquidations(&pm, &traders, &config, &oracle_prices).unwrap();

    assert_eq!(liqs.len(), 1);
    assert_eq!(liqs[0].market_id, 1);
    assert_eq!(liqs[0].margin_type, MarginType::Isolated);

    assert!(pm.get_position(&trader, 2).unwrap().is_some());
}

// ============================================================================
// Edge: only position in market (no ADL targets)
// ============================================================================

#[test]
fn adl_no_profitable_traders() {
    let (_dir, pm) = setup();
    let trader = addr(1);
    let market: MarketId = 1;

    let loss = fp(500);
    let traders = vec![trader];

    let results =
        LiquidationEngine::auto_deleverage(&pm, market, loss, fp(50_000), &traders).unwrap();

    assert!(results.is_empty());
}

// ============================================================================
// Edge: all traders are losing (no profitable traders for ADL)
// ============================================================================

#[test]
fn adl_all_traders_losing() {
    let (_dir, pm) = setup();
    let market: MarketId = 1;

    for i in 1..=3u8 {
        make_cross_long(&pm, &addr(i), market, fp(1), fp(50_000));
        set_balance(&pm, &addr(i), fp(5_000));
    }

    let traders: Vec<Address> = (1..=3).map(addr).collect();
    let loss = fp(1_000);

    let results =
        LiquidationEngine::auto_deleverage(&pm, market, loss, fp(45_000), &traders).unwrap();

    assert!(results.is_empty());
}

// ============================================================================
// No negative equity after full liquidation cycle
// ============================================================================

#[test]
fn no_negative_equity_after_full_cycle() {
    let (_dir, pm) = setup();
    let liquidated = addr(1);
    let profitable = addr(2);
    let market: MarketId = 1;
    let config = MarketMarginConfig::new(market, 20);

    make_cross_long(&pm, &liquidated, market, fp(1), fp(50_000));
    set_balance(&pm, &liquidated, fp(1_000));

    pm.put_position(&Position {
        trader: profitable,
        market_id: market,
        is_long: false,
        size: fp(1),
        entry_price: fp(55_000),
        realized_pnl: FixedPoint::ZERO,
        isolated_margin: FixedPoint::ZERO,
        margin_type: MarginType::Cross,
    })
    .unwrap();
    set_balance(&pm, &profitable, fp(10_000));

    let oracle_price = fp(48_500);
    let oracle_prices = vec![(market, oracle_price)];
    let traders = vec![liquidated, profitable];

    let liqs =
        LiquidationEngine::check_liquidations(&pm, &traders, &config, &oracle_prices).unwrap();

    for liq in &liqs {
        let result = LiquidationEngine::execute_liquidation(&pm, liq, oracle_price).unwrap();

        if result.remaining_deficit > FixedPoint::ZERO {
            LiquidationEngine::auto_deleverage(
                &pm,
                market,
                result.remaining_deficit,
                oracle_price,
                &traders,
            )
            .unwrap();
        }
    }

    let bal = pm.get_native_balance(&liquidated).unwrap();
    assert!(bal.available >= FixedPoint::ZERO);
    assert!(pm.get_position(&liquidated, market).unwrap().is_none());
}

// ============================================================================
// Insurance fund absorbs loss before socializing
// ============================================================================

#[test]
fn insurance_fund_absorbs_loss() {
    let (_dir, pm) = setup();
    let trader1 = addr(1);
    let trader2 = addr(2);
    let market: MarketId = 1;

    make_cross_long(&pm, &trader1, market, fp(1), fp(50_000));
    set_balance(&pm, &trader1, fp(10_000));

    make_cross_long(&pm, &trader2, market, fp(1), fp(50_000));
    set_balance(&pm, &trader2, fp(10_000));

    let state_db = pm.state_db();
    let amount = fp(1_000);
    state_db
        .put_cf_raw(
            torus_state::cf::CF_NATIVE_BALANCES,
            b"insurance_fund",
            &amount.raw().to_be_bytes(),
        )
        .unwrap();

    let traders = vec![trader1, trader2];
    let loss = fp(500);

    LiquidationEngine::socialize_loss(&pm, market, loss, &traders).unwrap();

    let bal1 = pm.get_native_balance(&trader1).unwrap();
    let bal2 = pm.get_native_balance(&trader2).unwrap();
    assert_eq!(bal1.available, fp(10_000));
    assert_eq!(bal2.available, fp(10_000));
}
