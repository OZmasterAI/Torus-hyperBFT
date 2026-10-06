//! Oracle integration tests (task 2.8b.5).

use torus_core::oracle::{valid_oracle_price, OracleConfig, OracleManager, MAX_ORACLE_PRICE_RAW};
use torus_state::cf::CF_NATIVE_ORACLE;
use torus_state::{StateBackend, StateDb};
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

    let price = mgr.aggregate_price(market, block, 0, &stakes).unwrap();
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

    let price = mgr.aggregate_price(market, block, 0, &stakes).unwrap();
    // Outlier at 50000 should be filtered out
    // Remaining: 990, 1000, 1005, 1010 — median around 1000–1005
    assert!(price >= fp(990) && price <= fp(1010));
}

// ============================================================================
// Staleness: max_age_secs after the fresh aggregate's block timestamp -> stale flag
// ============================================================================

#[test]
fn staleness_detection() {
    let config = OracleConfig {
        max_age_secs: 50,
        min_oracle_reporters: 1,
        window_secs: 10,
    };
    let (_dir, mgr) = setup_with_config(config);
    let market: MarketId = 1;

    // Submit at block 100, block timestamp 1_000
    mgr.submit_price(&addr(1), market, fp(1000), 100, 1_000)
        .unwrap();
    let stakes = vec![(addr(1), fp(10))];
    mgr.aggregate_price(market, 100, 1_000, &stakes).unwrap();

    // At ts 1_010 — not stale (age 10 <= 50)
    let op = mgr.get_price(market, 1_010).unwrap();
    assert!(!op.stale);
    assert_eq!(op.price, fp(1000));

    // At ts 1_100 — stale (age 100 > 50)
    let op = mgr.get_price(market, 1_100).unwrap();
    assert!(op.stale);
    assert_eq!(op.price, fp(1000)); // Still returns last valid
}

// ============================================================================
// No data: brand new market → error
// ============================================================================

#[test]
fn no_data_returns_error() {
    let (_dir, mgr) = setup_with_config(OracleConfig::default());
    let result = mgr.get_price(999, 0);
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
    let price = mgr.aggregate_price(market, 100, 0, &stakes).unwrap();
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
    let price = mgr.aggregate_price(market, 100, 0, &stakes).unwrap();
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

    let price = mgr.aggregate_price(market, 100, 0, &stakes).unwrap();
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
    let price = mgr.aggregate_price(market, 100, 0, &stakes).unwrap();
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

    let price = mgr.aggregate_price(market, 100, 0, &stakes).unwrap();
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
    mgr.aggregate_price(market, 100, 0, &stakes).unwrap();

    let op = mgr.get_price(market, 0).unwrap();
    assert_eq!(op.num_reporters, 5);
    assert!(!op.stale);
}

// ============================================================================
// Item 2 (option A, time-based): windows in block-timestamp seconds, one row per
// (market, validator), usable(), prune by time, bounded inputs, exact 3xMAD.
// ============================================================================

fn sub_rows(db: &StateDb) -> usize {
    StateBackend::iterate_cf(db, CF_NATIVE_ORACLE, Some(b"sub")).unwrap().len()
}

fn three_equal() -> Vec<(Address, FixedPoint)> {
    (1..=3u8).map(|v| (addr(v), fp(1))).collect()
}

fn mgr_on(db: &StateDb) -> OracleManager {
    OracleManager::new(db.clone(), OracleConfig::default())
}

/// One row per (market, validator): a later submission replaces the earlier.
#[test]
fn a_validator_has_one_row_per_market_its_latest() {
    let (_dir, db) = setup();
    let mgr = mgr_on(&db);
    mgr.submit_price(&addr(1), 1, fp(100), 5, 1_005).unwrap();
    mgr.submit_price(&addr(1), 1, fp(200), 6, 1_006).unwrap();
    mgr.submit_price(&addr(1), 2, fp(7), 6, 1_006).unwrap();
    assert_eq!(sub_rows(&db), 2);
    for v in 2..=3u8 {
        mgr.submit_price(&addr(v), 1, fp(200), 6, 1_006).unwrap();
    }
    assert_eq!(mgr.aggregate_price(1, 7, 1_007, &three_equal()).unwrap(), fp(200));
}

/// Window: a row counts iff now - ts <= 10; a fresh aggregate records (block, now).
#[test]
fn a_submission_counts_for_ten_seconds() {
    let (_dir, db) = setup();
    let mgr = mgr_on(&db);
    for v in 1..=3u8 {
        mgr.submit_price(&addr(v), 1, fp(100), 5, 1_000).unwrap();
    }
    assert_eq!(mgr.aggregate_price(1, 6, 1_010, &three_equal()).unwrap(), fp(100), "age 10");
    let p = mgr.get_price(1, 1_010).unwrap();
    assert_eq!((p.block_number, p.timestamp), (6, 1_010));
    // age 11: no row counts -> the last price is returned, NOT re-stamped
    assert_eq!(mgr.aggregate_price(1, 7, 1_011, &three_equal()).unwrap(), fp(100));
    assert_eq!(mgr.get_price(1, 1_011).unwrap().timestamp, 1_010);
}

/// Staleness: usable while now - agg.ts <= 60.
#[test]
fn usable_is_the_time_based_mark_rule() {
    let (_dir, db) = setup();
    let mgr = mgr_on(&db);
    for v in 1..=3u8 {
        mgr.submit_price(&addr(v), 1, fp(100), 10, 2_000).unwrap();
    }
    mgr.aggregate_price(1, 10, 2_000, &three_equal()).unwrap();
    assert_eq!(mgr.get_price(1, 2_060).unwrap().usable(), Some(fp(100)), "age 60");
    assert_eq!(mgr.get_price(1, 2_061).unwrap().usable(), None, "age 61");
    assert_eq!(mgr.get_price(1, 1_500).unwrap().usable(), Some(fp(100)), "clock behind: age 0");
}

/// Prune: every row with now - ts > 10 goes (all markets); undecodable rows go.
#[test]
fn prune_deletes_every_row_older_than_the_window() {
    let (_dir, db) = setup();
    let mgr = mgr_on(&db);
    for (v, ts) in [(1u8, 1_000u64), (2, 1_005), (3, 1_010)] {
        for m in [1u64, 7] {
            mgr.submit_price(&addr(v), m, fp(100), 1, ts).unwrap();
        }
    }
    db.put_cf_raw(CF_NATIVE_ORACLE, &[b"sub".as_slice(), &[9u8; 28]].concat(), &[1, 2])
        .unwrap();
    assert_eq!(mgr.prune_submissions(1_015).unwrap(), 3, "v1 x 2 markets (age 15) + corrupt row");
    assert_eq!(sub_rows(&db), 4);
    assert_eq!(mgr.prune_submissions(1_015).unwrap(), 0, "idempotent");
}

/// Aggregation never deletes (pruning is the block-start step).
#[test]
fn aggregate_price_is_read_only_on_submissions() {
    let (_dir, db) = setup();
    let mgr = mgr_on(&db);
    for v in 1..=3u8 {
        mgr.submit_price(&addr(v), 1, fp(100), 1, 1_000).unwrap();
    }
    assert!(mgr.aggregate_price(1, 50, 1_050, &three_equal()).is_err(), "nothing in the window");
    assert_eq!(sub_rows(&db), 3);
}

/// Out-of-range rows (only writable by bypassing the handler) are ignored —
/// before: `mean_abs_deviation` overflowed and PANICKED.
#[test]
fn out_of_range_rows_are_ignored_not_a_panic() {
    let (_dir, db) = setup();
    let mgr = mgr_on(&db);
    for v in 1..=3u8 {
        mgr.submit_price(&addr(v), 1, fp(100), 100, 0).unwrap();
    }
    mgr.submit_price(&addr(4), 1, FixedPoint::from_raw(i128::MAX), 100, 0).unwrap();
    mgr.submit_price(&addr(5), 1, fp(-5), 100, 0).unwrap();
    // v4 / v5 are not counted as reporting: v1..v3 must hold > 2/3 (30 of 32).
    let stakes: Vec<_> = (1..=5u8).map(|v| (addr(v), fp(if v <= 3 { 10 } else { 1 }))).collect();
    assert_eq!(mgr.aggregate_price(1, 100, 0, &stakes).unwrap(), fp(100));
    assert_eq!(mgr.get_price(1, 0).unwrap().num_reporters, 3);
}

/// Guard (GREEN): the extreme VALID inputs never overflow — 100 validators
/// alternating the smallest / largest price, all at u64::MAX power.
#[test]
fn extreme_valid_inputs_aggregate_without_panic() {
    let (_dir, db) = setup();
    let mgr = mgr_on(&db);
    let max = FixedPoint::from_raw(MAX_ORACLE_PRICE_RAW);
    let whale = FixedPoint::from_raw(u64::MAX as i128 * FixedPoint::SCALE);
    for v in 1..=100u8 {
        let p = if v % 2 == 0 { max } else { FixedPoint::from_raw(1) };
        mgr.submit_price(&addr(v), 1, p, 100, 0).unwrap();
    }
    let stakes: Vec<_> = (1..=100u8).map(|v| (addr(v), whale)).collect();
    let _ = mgr.aggregate_price(1, 100, 0, &stakes);
    assert!(valid_oracle_price(max));
    assert!(!valid_oracle_price(FixedPoint::from_raw(MAX_ORACLE_PRICE_RAW + 1)));
    assert!(!valid_oracle_price(FixedPoint::ZERO));
    assert!(!valid_oracle_price(fp(-1)));
}

/// R9: 100 / 100 / 101 — 3 x MAD is exactly 1, so 101 must be KEPT. The
/// truncated FixedPoint MAD (0.33333333 x 3 = 0.99999999) cut it, leaving 2
/// reporters (< 3) and no price.
#[test]
fn two_equal_prices_and_one_other_keep_all_three() {
    let (_dir, db) = setup();
    let mgr = mgr_on(&db);
    for (v, p) in [(1u8, 100), (2, 100), (3, 101)] {
        mgr.submit_price(&addr(v), 1, fp(p), 100, 0).unwrap();
    }
    assert_eq!(mgr.aggregate_price(1, 100, 0, &three_equal()).unwrap(), fp(100));
    assert_eq!(mgr.get_price(1, 0).unwrap().num_reporters, 3);
}

/// Defence in depth (rev. 3): every age clamps at 0. Rows / an aggregate stamped
/// AFTER `now` (possible in committed history, see T0b's flag) have age 0: the
/// rows are not pruned and count, the aggregate is not stale. No panic.
#[test]
fn ages_clamp_at_zero_when_timestamps_run_backwards() {
    let (_dir, db) = setup();
    let mgr = mgr_on(&db);
    for v in 1..=3u8 {
        mgr.submit_price(&addr(v), 1, fp(100), 5, 2_000).unwrap();
    }
    assert_eq!(mgr.prune_submissions(1_000).unwrap(), 0, "future rows: age 0, kept");
    assert_eq!(mgr.aggregate_price(1, 6, 1_000, &three_equal()).unwrap(), fp(100), "they count");
    let p = mgr.get_price(1, 500).unwrap();
    assert!(!p.stale, "aggregate stamped after now: age 0");
    assert_eq!(p.usable(), Some(fp(100)));
}

// ============================================================================
// Stake quorum (review M1): a FRESH aggregate needs >= 3 reporters AND the
// counted reporters' stake > 2/3 of the Active stake passed in — else the last
// price is kept (it ages to stale). Counted = window + stake filter AND the
// outlier cut: the stake that actually sets the median.
// ============================================================================

fn equal_stakes(n: u8) -> Vec<(Address, FixedPoint)> {
    (1..=n).map(|v| (addr(v), fp(1))).collect()
}

/// Validators 1..=n submit `price` at `ts` (block 1); block 2 aggregates at `ts`.
fn seed_fresh(mgr: &OracleManager, stakes: &[(Address, FixedPoint)], price: i64, ts: u64) {
    for (a, _) in stakes {
        mgr.submit_price(a, 1, fp(price), 1, ts).unwrap();
    }
    assert_eq!(mgr.aggregate_price(1, 2, ts, stakes).unwrap(), fp(price), "seed");
    assert_eq!(mgr.get_price(1, ts).unwrap().timestamp, ts);
}

/// 10 equal: 3 colluders @200 + 2 honest @100 = 50% of stake -> no fresh
/// price; the last (100, stamped block 2 / ts 1000) is kept.
#[test]
fn quorum_three_colluders_and_two_honest_of_ten_is_not_fresh() {
    let (_dir, db) = setup();
    let mgr = mgr_on(&db);
    let stakes = equal_stakes(10);
    seed_fresh(&mgr, &stakes, 100, 1_000);
    for (v, p) in [(1u8, 200), (2, 200), (3, 200), (4, 100), (5, 100)] {
        mgr.submit_price(&addr(v), 1, fp(p), 3, 1_020).unwrap();
    }
    assert_eq!(mgr.aggregate_price(1, 4, 1_020, &stakes).unwrap(), fp(100), "last price kept");
    let p = mgr.get_price(1, 1_020).unwrap();
    assert_eq!((p.price, p.block_number, p.timestamp), (fp(100), 2, 1_000), "not re-stamped");
}

/// 7 of 10 equal (70% > 2/3) -> fresh.
#[test]
fn quorum_seven_of_ten_is_fresh() {
    let (_dir, db) = setup();
    let mgr = mgr_on(&db);
    let stakes = equal_stakes(10);
    seed_fresh(&mgr, &stakes, 100, 1_000);
    for v in 1..=7u8 {
        mgr.submit_price(&addr(v), 1, fp(150), 3, 1_020).unwrap();
    }
    assert_eq!(mgr.aggregate_price(1, 4, 1_020, &stakes).unwrap(), fp(150));
    let p = mgr.get_price(1, 1_020).unwrap();
    assert_eq!((p.block_number, p.timestamp, p.num_reporters), (4, 1_020, 7));
}

/// Exactly 2/3 is NOT a quorum (strict): 6 of 9 equal; no prior price -> error.
#[test]
fn quorum_exactly_two_thirds_is_not_fresh() {
    let (_dir, db) = setup();
    let mgr = mgr_on(&db);
    for v in 1..=6u8 {
        mgr.submit_price(&addr(v), 1, fp(100), 1, 1_000).unwrap();
    }
    assert!(mgr.aggregate_price(1, 2, 1_000, &equal_stakes(9)).is_err());
    assert!(mgr.get_price(1, 1_000).is_err(), "nothing written");
    mgr.submit_price(&addr(7), 1, fp(100), 1, 1_000).unwrap();
    assert_eq!(mgr.aggregate_price(1, 2, 1_000, &equal_stakes(9)).unwrap(), fp(100), "7 of 9");
}

/// Unequal stakes: the quorum is stake, not head count.
#[test]
fn quorum_is_stake_weighted_not_head_count() {
    // 70 / 10 / 10 / 10: three small validators (30%) are not a quorum ...
    let (_dir, db) = setup();
    let mgr = mgr_on(&db);
    let stakes = vec![(addr(1), fp(70)), (addr(2), fp(10)), (addr(3), fp(10)), (addr(4), fp(10))];
    for v in 2..=4u8 {
        mgr.submit_price(&addr(v), 1, fp(100), 1, 1_000).unwrap();
    }
    assert!(mgr.aggregate_price(1, 2, 1_000, &stakes).is_err(), "3 reporters, 30% stake");
    // ... the whale + two small ones (90%) are; the weighted median is the whale's.
    mgr.submit_price(&addr(1), 1, fp(120), 1, 1_000).unwrap();
    mgr.submit_price(&addr(4), 1, fp(80), 1, 1_000).unwrap();
    assert_eq!(mgr.aggregate_price(1, 2, 1_000, &stakes).unwrap(), fp(120));

    // 2 / 2 / 2 / 3 (total 9): {2,2,2} = 6 = exactly 2/3 -> no; {2,2,3} = 7 -> yes.
    let (_dir2, db2) = setup();
    let mgr = mgr_on(&db2);
    let stakes = vec![(addr(1), fp(2)), (addr(2), fp(2)), (addr(3), fp(2)), (addr(4), fp(3))];
    for v in 1..=3u8 {
        mgr.submit_price(&addr(v), 1, fp(100), 1, 1_000).unwrap();
    }
    assert!(mgr.aggregate_price(1, 2, 1_000, &stakes).is_err(), "6 of 9");
    let (_dir3, db3) = setup();
    let mgr = mgr_on(&db3);
    for v in 2..=4u8 {
        mgr.submit_price(&addr(v), 1, fp(100), 1, 1_000).unwrap();
    }
    assert_eq!(mgr.aggregate_price(1, 2, 1_000, &stakes).unwrap(), fp(100), "7 of 9");
}

/// Review M1 scenario: honest feeders go down after ts 1000; 3 attackers (30%)
/// keep submitting the old price every second. Before: 3 reporters re-stamped
/// the price every block and it never went stale. Now the honest rows count
/// through ts 1010 (window), so the last fresh stamp is 1010: stale at 1071.
#[test]
fn quorum_three_attackers_cannot_keep_a_price_alive() {
    let (_dir, db) = setup();
    let mgr = mgr_on(&db);
    let stakes = equal_stakes(10);
    seed_fresh(&mgr, &stakes, 100, 1_000);
    for ts in 1_001..=1_100u64 {
        for v in 1..=3u8 {
            mgr.submit_price(&addr(v), 1, fp(100), ts, ts).unwrap();
        }
        mgr.prune_submissions(ts).unwrap();
        let _ = mgr.aggregate_price(1, ts + 1, ts, &stakes);
    }
    assert_eq!(mgr.get_price(1, 1_070).unwrap().usable(), Some(fp(100)), "age 60");
    let p = mgr.get_price(1, 1_071).unwrap();
    assert_eq!((p.usable(), p.timestamp), (None, 1_010), "age 61: stale, not re-stamped");
}

/// The quorum is measured on the set AFTER the outlier cut. The cut uses the
/// UNWEIGHTED median, so 7 low-stake attackers (7%) can cut 3 honest whales
/// (93%) as outliers; counting before the cut would then let 7% set the price.
#[test]
fn quorum_is_counted_after_the_outlier_cut() {
    let (_dir, db) = setup();
    let mgr = mgr_on(&db);
    let mut stakes: Vec<_> = (1..=7u8).map(|v| (addr(v), fp(1))).collect();
    stakes.extend((8..=10u8).map(|v| (addr(v), fp(31))));
    for v in 1..=10u8 {
        mgr.submit_price(&addr(v), 1, fp(if v <= 7 { 200 } else { 100 }), 1, 1_000).unwrap();
    }
    assert!(mgr.aggregate_price(1, 2, 1_000, &stakes).is_err(), "7% after the cut: no price");
    assert!(mgr.get_price(1, 1_000).is_err(), "nothing written");
}

/// Liveness side of the same rule: 8 of 10 equal report, one wild -> 7 kept
/// (70%) -> fresh.
#[test]
fn quorum_survives_one_cut_outlier_with_enough_stake_left() {
    let (_dir, db) = setup();
    let mgr = mgr_on(&db);
    for v in 1..=7u8 {
        mgr.submit_price(&addr(v), 1, fp(100), 1, 1_000).unwrap();
    }
    mgr.submit_price(&addr(8), 1, fp(50_000), 1, 1_000).unwrap();
    assert_eq!(mgr.aggregate_price(1, 2, 1_000, &equal_stakes(10)).unwrap(), fp(100));
    assert_eq!(mgr.get_price(1, 1_000).unwrap().num_reporters, 7);
}
