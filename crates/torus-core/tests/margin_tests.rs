//! Margin engine integration tests (task 2.2.6).

use torus_core::error::CoreError;
use torus_core::margin::{
    default_margin_tiers, effective_max_leverage, MarginEngine, MarketMarginConfig,
};
use torus_core::position::{MarginType, NativeBalance, Position, PositionManager};
use torus_state::StateDb;
use torus_types::{Address, FixedPoint};

fn setup() -> (tempfile::TempDir, PositionManager) {
    let dir = tempfile::tempdir().unwrap();
    let db = StateDb::open(dir.path()).unwrap();
    (dir, PositionManager::new(db))
}

fn addr(n: u8) -> Address {
    Address::new([n; 20])
}

fn fp(v: i64) -> FixedPoint {
    FixedPoint::from_raw(v as i128 * FixedPoint::SCALE)
}

// ============================================================================
// Cross-margin: multiple positions offsetting each other
// ============================================================================

#[test]
fn cross_margin_multiple_positions_offset() {
    let (_dir, pm) = setup();
    let trader = addr(1);

    pm.put_native_balance(
        &trader,
        &NativeBalance {
            available: fp(20_000),
            order_margin: FixedPoint::ZERO,
        },
    )
    .unwrap();

    pm.put_position(&Position {
        trader,
        market_id: 1,
        is_long: true,
        size: fp(1),
        entry_price: fp(50_000),
        realized_pnl: FixedPoint::ZERO,
        isolated_margin: FixedPoint::ZERO,
        margin_type: MarginType::Cross,
    })
    .unwrap();

    pm.put_position(&Position {
        trader,
        market_id: 2,
        is_long: false,
        size: fp(10),
        entry_price: fp(3_000),
        realized_pnl: FixedPoint::ZERO,
        isolated_margin: FixedPoint::ZERO,
        margin_type: MarginType::Cross,
    })
    .unwrap();

    let oracle_prices = vec![(1u64, fp(51_000)), (2u64, fp(2_900))];
    let equity = MarginEngine::cross_margin_equity(&pm, &trader, &oracle_prices).unwrap();
    assert_eq!(equity, fp(22_000));
}

#[test]
fn cross_margin_with_loss() {
    let (_dir, pm) = setup();
    let trader = addr(1);

    pm.put_native_balance(
        &trader,
        &NativeBalance {
            available: fp(10_000),
            order_margin: FixedPoint::ZERO,
        },
    )
    .unwrap();

    pm.put_position(&Position {
        trader,
        market_id: 1,
        is_long: true,
        size: fp(1),
        entry_price: fp(50_000),
        realized_pnl: FixedPoint::ZERO,
        isolated_margin: FixedPoint::ZERO,
        margin_type: MarginType::Cross,
    })
    .unwrap();

    let oracle_prices = vec![(1u64, fp(45_000))];
    let equity = MarginEngine::cross_margin_equity(&pm, &trader, &oracle_prices).unwrap();
    assert_eq!(equity, fp(5_000));
}

// ============================================================================
// Isolated: one position liquidated, others unaffected
// ============================================================================

#[test]
fn isolated_positions_independent() {
    let pos_healthy = Position {
        trader: addr(1),
        market_id: 1,
        is_long: true,
        size: fp(1),
        entry_price: fp(50_000),
        realized_pnl: FixedPoint::ZERO,
        isolated_margin: fp(5_000),
        margin_type: MarginType::Isolated,
    };

    let pos_underwater = Position {
        trader: addr(1),
        market_id: 2,
        is_long: true,
        size: fp(10),
        entry_price: fp(3_000),
        realized_pnl: FixedPoint::ZERO,
        isolated_margin: fp(1_000),
        margin_type: MarginType::Isolated,
    };

    let config1 = MarketMarginConfig::new(1, 20);
    let config2 = MarketMarginConfig::new(2, 20);

    assert!(MarginEngine::check_isolated_maintenance(
        &pos_healthy,
        fp(51_000),
        &config1
    ));

    assert!(!MarginEngine::check_isolated_maintenance(
        &pos_underwater,
        fp(2_000),
        &config2
    ));
}

// ============================================================================
// Tier boundaries
// ============================================================================

#[test]
fn tier_boundary_leverage_limits() {
    let (_dir, pm) = setup();
    let trader = addr(1);

    pm.put_native_balance(
        &trader,
        &NativeBalance {
            available: fp(1_000_000),
            order_margin: FixedPoint::ZERO,
        },
    )
    .unwrap();

    let config = MarketMarginConfig::new(1, 50);
    let oracle_prices = vec![(1u64, fp(50_000))];

    // 2 BTC * 50k = 100k -> tier 1, max 50x -> OK
    assert!(MarginEngine::check_initial_margin(
        &pm,
        &trader,
        1,
        true,
        fp(50_000),
        fp(2),
        50,
        MarginType::Cross,
        &config,
        &oracle_prices,
    )
    .is_ok());

    // 3 BTC * 50k = 150k -> tier 2, max 20x -> 50x DENIED
    assert!(matches!(
        MarginEngine::check_initial_margin(
            &pm,
            &trader,
            1,
            true,
            fp(50_000),
            fp(3),
            50,
            MarginType::Cross,
            &config,
            &oracle_prices,
        ),
        Err(CoreError::MaxLeverageExceeded { .. })
    ));

    // 3 BTC * 50k = 150k, 20x -> OK
    assert!(MarginEngine::check_initial_margin(
        &pm,
        &trader,
        1,
        true,
        fp(50_000),
        fp(3),
        20,
        MarginType::Cross,
        &config,
        &oracle_prices,
    )
    .is_ok());
}

// ============================================================================
// Double-check: price moves between submission and match
// ============================================================================

#[test]
fn double_check_price_move() {
    let (_dir, pm) = setup();
    let trader = addr(1);

    pm.put_native_balance(
        &trader,
        &NativeBalance {
            available: fp(5_100),
            order_margin: FixedPoint::ZERO,
        },
    )
    .unwrap();

    let config = MarketMarginConfig::new(1, 50);
    let oracle_prices = vec![(1u64, fp(50_000))];

    assert!(MarginEngine::check_initial_margin(
        &pm,
        &trader,
        1,
        true,
        fp(50_000),
        fp(1),
        10,
        MarginType::Cross,
        &config,
        &oracle_prices,
    )
    .is_ok());

    assert!(matches!(
        MarginEngine::check_margin_at_match(
            &pm,
            &trader,
            1,
            true,
            fp(60_000),
            fp(1),
            10,
            MarginType::Cross,
            &config,
            &oracle_prices,
        ),
        Err(CoreError::InsufficientMargin { .. })
    ));
}

// ============================================================================
// Zero and minimum values
// ============================================================================

#[test]
fn zero_balance_fails() {
    let (_dir, pm) = setup();
    let trader = addr(1);

    pm.put_native_balance(
        &trader,
        &NativeBalance {
            available: FixedPoint::ZERO,
            order_margin: FixedPoint::ZERO,
        },
    )
    .unwrap();

    let config = MarketMarginConfig::new(1, 50);
    let oracle_prices = vec![(1u64, fp(50_000))];

    assert!(matches!(
        MarginEngine::check_initial_margin(
            &pm,
            &trader,
            1,
            true,
            fp(50_000),
            fp(1),
            10,
            MarginType::Cross,
            &config,
            &oracle_prices,
        ),
        Err(CoreError::InsufficientMargin { .. })
    ));
}

// ============================================================================
// Max leverage per tier
// ============================================================================

#[test]
fn max_leverage_per_tier() {
    let tiers = default_margin_tiers();

    assert_eq!(effective_max_leverage(&tiers, fp(50_000)), 50);
    assert_eq!(effective_max_leverage(&tiers, fp(100_000)), 50);
    assert_eq!(effective_max_leverage(&tiers, fp(100_001)), 20);
    assert_eq!(effective_max_leverage(&tiers, fp(500_000)), 20);
    assert_eq!(effective_max_leverage(&tiers, fp(1_000_000)), 20);
    assert_eq!(effective_max_leverage(&tiers, fp(1_000_001)), 10);
    assert_eq!(effective_max_leverage(&tiers, fp(10_000_000)), 10);
    assert_eq!(effective_max_leverage(&tiers, fp(10_000_001)), 5);
    assert_eq!(effective_max_leverage(&tiers, fp(100_000_000)), 5);
}

// ============================================================================
// Position PnL: increase averages entry price
// ============================================================================

#[test]
fn position_increase_averages_entry_price() {
    let (_dir, pm) = setup();
    let trader = addr(1);

    pm.put_native_balance(
        &trader,
        &NativeBalance {
            available: fp(100_000),
            order_margin: FixedPoint::ZERO,
        },
    )
    .unwrap();

    pm.apply_fill(&trader, 1, true, fp(2), fp(50_000), MarginType::Cross)
        .unwrap();

    let pos = pm.get_position(&trader, 1).unwrap().unwrap();
    assert_eq!(pos.size, fp(2));
    assert_eq!(pos.entry_price, fp(50_000));

    pm.apply_fill(&trader, 1, true, fp(1), fp(53_000), MarginType::Cross)
        .unwrap();

    let pos = pm.get_position(&trader, 1).unwrap().unwrap();
    assert_eq!(pos.size, fp(3));
    assert_eq!(pos.entry_price, fp(51_000));
}

// ============================================================================
// Partial close realizes PnL
// ============================================================================

#[test]
fn partial_close_realizes_pnl() {
    let (_dir, pm) = setup();
    let trader = addr(1);

    pm.put_native_balance(
        &trader,
        &NativeBalance {
            available: fp(100_000),
            order_margin: FixedPoint::ZERO,
        },
    )
    .unwrap();

    pm.apply_fill(&trader, 1, true, fp(10), fp(50_000), MarginType::Cross)
        .unwrap();

    pm.apply_fill(&trader, 1, false, fp(4), fp(55_000), MarginType::Cross)
        .unwrap();

    let pos = pm.get_position(&trader, 1).unwrap().unwrap();
    assert_eq!(pos.size, fp(6));
    assert_eq!(pos.entry_price, fp(50_000));
    assert_eq!(pos.realized_pnl, fp(20_000));

    let bal = pm.get_native_balance(&trader).unwrap();
    assert_eq!(bal.available, fp(120_000));
}

// ============================================================================
// Full close and flip
// ============================================================================

#[test]
fn full_close_and_flip() {
    let (_dir, pm) = setup();
    let trader = addr(1);

    pm.put_native_balance(
        &trader,
        &NativeBalance {
            available: fp(100_000),
            order_margin: FixedPoint::ZERO,
        },
    )
    .unwrap();

    pm.apply_fill(&trader, 1, true, fp(5), fp(50_000), MarginType::Cross)
        .unwrap();

    pm.apply_fill(&trader, 1, false, fp(8), fp(52_000), MarginType::Cross)
        .unwrap();

    let pos = pm.get_position(&trader, 1).unwrap().unwrap();
    assert!(!pos.is_long);
    assert_eq!(pos.size, fp(3));
    assert_eq!(pos.entry_price, fp(52_000));

    let bal = pm.get_native_balance(&trader).unwrap();
    assert_eq!(bal.available, fp(110_000));
}

// ============================================================================
// Unrealized PnL computation
// ============================================================================

#[test]
fn unrealized_pnl_long() {
    let pos = Position {
        trader: addr(1),
        market_id: 1,
        is_long: true,
        size: fp(2),
        entry_price: fp(50_000),
        realized_pnl: FixedPoint::ZERO,
        isolated_margin: FixedPoint::ZERO,
        margin_type: MarginType::Cross,
    };

    assert_eq!(pos.unrealized_pnl(fp(52_000)), fp(4_000));
    assert_eq!(pos.unrealized_pnl(fp(48_000)), fp(-4_000));
}

#[test]
fn unrealized_pnl_short() {
    let pos = Position {
        trader: addr(1),
        market_id: 1,
        is_long: false,
        size: fp(3),
        entry_price: fp(50_000),
        realized_pnl: FixedPoint::ZERO,
        isolated_margin: FixedPoint::ZERO,
        margin_type: MarginType::Cross,
    };

    assert_eq!(pos.unrealized_pnl(fp(48_000)), fp(6_000));
    assert_eq!(pos.unrealized_pnl(fp(52_000)), fp(-6_000));
}
