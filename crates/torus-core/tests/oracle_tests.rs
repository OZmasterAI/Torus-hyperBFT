//! Oracle integration tests (task 2.8b.5).

use torus_core::oracle::{OracleConfig, OracleManager};
use torus_state::StateDb;
use torus_types::{Address, FixedPoint, MarketId};

fn fp(v: i64) -> FixedPoint {
    FixedPoint::from_raw(v as i128 * FixedPoint::SCALE)
}

fn addr(n: u8) -> Address {
    Address::new([n; 20])
}

fn setup() -> (tempfile::TempDir, StateDb) {
    let dir = tempfile::tempdir().unwrap();
    let db = StateDb::open(dir.path()).unwrap();
    (dir, db)
}

fn setup_with_config(config: OracleConfig) -> (tempfile::TempDir, OracleManager) {
    let (dir, db) = setup();
    let mgr = OracleManager::new(db, config);
    (dir, mgr)
}

// ============================================================================
// Normal aggregation: 5 validators, varying stakes, verify weighted median
// ============================================================================

#[test]
fn normal_aggregation_5_validators() {
    let config = OracleConfig {
        min_oracle_reporters: 3,
        ..Default::default()
    };
    let (_dir, mgr) = setup_with_config(config);
    let market: MarketId = 1;
    let block = 100;

    // 5 validators submit prices
    for i in 0..5u8 {
        let price = fp(1000 + i as i64 * 10); // 1000, 1010, 1020, 1030, 1040
        mgr.submit_price(&addr(i + 1), market, price, block, 0)
            .unwrap();
    }

    // Stakes: v1=10, v2=20, v3=30, v4=20, v5=10 — total 90, half=45
    let stakes: Vec<(Address, FixedPoint)> = vec![
        (addr(1), fp(10)),
        (addr(2), fp(20)),
        (addr(3), fp(30)),
        (addr(4), fp(20)),
        (addr(5), fp(10)),
    ];

    let price = mgr.aggregate_price(market, block, &stakes).unwrap();
    // Sorted by price: 1000(10), 1010(20), 1020(30), 1030(20), 1040(10)
    // Cumulative: 10, 30, 60 >= 45 → median = 1020
    assert_eq!(price, fp(1020));
}

// ============================================================================
// Outlier rejection: 1 of 5 submits wildly different price
// ============================================================================

#[test]
fn outlier_rejected() {
    let config = OracleConfig {
        min_oracle_reporters: 3,
        ..Default::default()
    };
    let (_dir, mgr) = setup_with_config(config);
    let market: MarketId = 1;
    let block = 100;

    // 4 normal prices around 1000, 1 wild at 50000
    mgr.submit_price(&addr(1), market, fp(990), block, 0)
        .unwrap();
    mgr.submit_price(&addr(2), market, fp(1000), block, 0)
        .unwrap();
    mgr.submit_price(&addr(3), market, fp(1010), block, 0)
        .unwrap();
    mgr.submit_price(&addr(4), market, fp(1005), block, 0)
        .unwrap();
    mgr.submit_price(&addr(5), market, fp(50000), block, 0)
        .unwrap();

    let stakes: Vec<(Address, FixedPoint)> = (1..=5).map(|i| (addr(i), fp(10))).collect();

    let price = mgr.aggregate_price(market, block, &stakes).unwrap();
    // Outlier at 50000 should be filtered out
    // Remaining: 990, 1000, 1005, 1010 — median around 1000–1005
    assert!(price >= fp(990) && price <= fp(1010));
}

// ============================================================================
// Staleness: no submissions for MAX_ORACLE_AGE blocks → stale flag
// ============================================================================

#[test]
fn staleness_detection() {
    let config = OracleConfig {
        max_oracle_age: 50,
        min_oracle_reporters: 1,
        aggregation_window: 10,
    };
    let (_dir, mgr) = setup_with_config(config);
    let market: MarketId = 1;

    // Submit at block 100
    mgr.submit_price(&addr(1), market, fp(1000), 100, 0)
        .unwrap();
    let stakes = vec![(addr(1), fp(10))];
    mgr.aggregate_price(market, 100, &stakes).unwrap();

    // Check at block 110 — not stale (age=10 < 50)
    let op = mgr.get_price(market, 110).unwrap();
    assert!(!op.stale);
    assert_eq!(op.price, fp(1000));

    // Check at block 200 — stale (age=100 > 50)
    let op = mgr.get_price(market, 200).unwrap();
    assert!(op.stale);
    assert_eq!(op.price, fp(1000)); // Still returns last valid
}

// ============================================================================
// No data: brand new market → error
// ============================================================================

#[test]
fn no_data_returns_error() {
    let (_dir, mgr) = setup_with_config(OracleConfig::default());
    let result = mgr.get_price(999, 100);
    assert!(result.is_err());
}

// ============================================================================
// Single validator: only 1 submission, still works
// ============================================================================

#[test]
fn single_validator_works() {
    let config = OracleConfig {
        min_oracle_reporters: 1,
        ..Default::default()
    };
    let (_dir, mgr) = setup_with_config(config);
    let market: MarketId = 1;

    mgr.submit_price(&addr(1), market, fp(5000), 100, 0)
        .unwrap();
    let stakes = vec![(addr(1), fp(100))];
    let price = mgr.aggregate_price(market, 100, &stakes).unwrap();
    assert_eq!(price, fp(5000));
}

// ============================================================================
// All validators submit same price → that price
// ============================================================================

#[test]
fn all_same_price() {
    let config = OracleConfig {
        min_oracle_reporters: 3,
        ..Default::default()
    };
    let (_dir, mgr) = setup_with_config(config);
    let market: MarketId = 1;

    for i in 1..=5u8 {
        mgr.submit_price(&addr(i), market, fp(2500), 100, 0)
            .unwrap();
    }

    let stakes: Vec<(Address, FixedPoint)> = (1..=5).map(|i| (addr(i), fp(10))).collect();
    let price = mgr.aggregate_price(market, 100, &stakes).unwrap();
    assert_eq!(price, fp(2500));
}

// ============================================================================
// Stake-weight dominance: one validator has 90% stake, their price dominates
// ============================================================================

#[test]
fn stake_weight_dominance() {
    let config = OracleConfig {
        min_oracle_reporters: 3,
        ..Default::default()
    };
    let (_dir, mgr) = setup_with_config(config);
    let market: MarketId = 1;

    mgr.submit_price(&addr(1), market, fp(100), 100, 0).unwrap();
    mgr.submit_price(&addr(2), market, fp(200), 100, 0).unwrap();
    mgr.submit_price(&addr(3), market, fp(150), 100, 0).unwrap();

    // Validator 3 has 90% stake
    let stakes = vec![(addr(1), fp(5)), (addr(2), fp(5)), (addr(3), fp(90))];

    let price = mgr.aggregate_price(market, 100, &stakes).unwrap();
    // Sorted: 100(5), 150(90), 200(5). Half=50. Cumul: 5, 95>=50 → 150
    assert_eq!(price, fp(150));
}

// ============================================================================
// Overwrite: same validator submits again for same block
// ============================================================================

#[test]
fn validator_overwrites_same_block() {
    let config = OracleConfig {
        min_oracle_reporters: 1,
        ..Default::default()
    };
    let (_dir, mgr) = setup_with_config(config);
    let market: MarketId = 1;

    mgr.submit_price(&addr(1), market, fp(1000), 100, 0)
        .unwrap();
    mgr.submit_price(&addr(1), market, fp(2000), 100, 0)
        .unwrap(); // overwrite

    let stakes = vec![(addr(1), fp(10))];
    let price = mgr.aggregate_price(market, 100, &stakes).unwrap();
    assert_eq!(price, fp(2000));
}

// ============================================================================
// No f64 verification: all intermediate values are FixedPoint
// ============================================================================

#[test]
fn no_floating_point_in_aggregation() {
    let config = OracleConfig {
        min_oracle_reporters: 3,
        ..Default::default()
    };
    let (_dir, mgr) = setup_with_config(config);
    let market: MarketId = 1;

    // Use values that would cause floating-point imprecision
    mgr.submit_price(&addr(1), market, FixedPoint::from_raw(333_333_333), 100, 0)
        .unwrap();
    mgr.submit_price(&addr(2), market, FixedPoint::from_raw(333_333_334), 100, 0)
        .unwrap();
    mgr.submit_price(&addr(3), market, FixedPoint::from_raw(333_333_335), 100, 0)
        .unwrap();

    let stakes = vec![(addr(1), fp(1)), (addr(2), fp(1)), (addr(3), fp(1))];

    let price = mgr.aggregate_price(market, 100, &stakes).unwrap();
    // Exact FixedPoint median
    assert_eq!(price, FixedPoint::from_raw(333_333_334));
}

// ============================================================================
// num_reporters in OraclePrice
// ============================================================================

#[test]
fn oracle_price_reports_num_reporters() {
    let config = OracleConfig {
        min_oracle_reporters: 3,
        ..Default::default()
    };
    let (_dir, mgr) = setup_with_config(config);
    let market: MarketId = 1;

    for i in 1..=5u8 {
        mgr.submit_price(&addr(i), market, fp(1000), 100, 0)
            .unwrap();
    }

    let stakes: Vec<(Address, FixedPoint)> = (1..=5).map(|i| (addr(i), fp(10))).collect();
    mgr.aggregate_price(market, 100, &stakes).unwrap();

    let op = mgr.get_price(market, 100).unwrap();
    assert_eq!(op.num_reporters, 5);
    assert!(!op.stale);
}
