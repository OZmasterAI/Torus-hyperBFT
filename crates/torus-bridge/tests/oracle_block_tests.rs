//! Item 2 (option A, time-based): oracle aggregation into block execution.
//! `docs/plans/oracle-aggregation.md` and `docs/plans/oracle-aggregation-impl.md`.

use alloy_primitives::{Address, U256};

use torus_bridge::native_executor::{NativeActionResult, NativeExecContext, NativeExecutor};
use torus_core::oracle::{MAX_ORACLE_PRICES_PER_SUBMISSION, MAX_ORACLE_PRICE_RAW};
use torus_economics::{StakingManager, ValidatorState, ValidatorStatus, MIN_SELF_DELEGATION};
use torus_state::cf::{CF_NATIVE_MARKETS, CF_NATIVE_ORACLE};
use torus_state::{StateBackend, StateDb};
use torus_types::{FixedPoint, MarketId, NativeAction, OracleSubmission};

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

/// Context at block `height`, timestamp `1_000 + height` (one second per block,
/// so seconds and blocks line up in the assertions); epoch length 1000.
fn ctx_at<T: StateBackend>(state: T, height: u64) -> NativeExecContext<T> {
    NativeExecContext::new(state, height, 1_000 + height, 0, 1_000, 10, addr(99), addr(100), addr(101))
}

const V1: u8 = 1;
const V2: u8 = 2;
const V3: u8 = 3; // 3x stake

fn put_validator(db: &StateDb, n: u8, stake: U256, status: ValidatorStatus) {
    StakingManager::new(db.clone())
        .put_validator(
            &addr(n),
            &ValidatorState {
                address: addr(n),
                pubkey: [n; 32],
                commission_bps: 0,
                self_stake: stake,
                total_delegated: U256::ZERO,
                status,
                jailed_until: None,
                last_commission_change_block: None,
            },
        )
        .unwrap();
}

fn list_market(db: &StateDb, id: MarketId) {
    db.put_cf_raw(CF_NATIVE_MARKETS, &id.to_be_bytes(), b"listed").unwrap();
}

/// V1, V2 (1x MIN_SELF_DELEGATION), V3 (3x) Active; markets 1 and 2 listed.
fn oracle_db() -> (tempfile::TempDir, StateDb) {
    let (dir, db) = open_test_db();
    put_validator(&db, V1, MIN_SELF_DELEGATION, ValidatorStatus::Active);
    put_validator(&db, V2, MIN_SELF_DELEGATION, ValidatorStatus::Active);
    put_validator(&db, V3, MIN_SELF_DELEGATION * U256::from(3u8), ValidatorStatus::Active);
    list_market(&db, 1);
    list_market(&db, 2);
    (dir, db)
}

fn submit(sender: u8, prices: &[(MarketId, FixedPoint)]) -> (Address, NativeAction) {
    (
        addr(sender),
        NativeAction::SubmitOraclePrices(OracleSubmission { prices: prices.to_vec(), timestamp: 0 }),
    )
}

/// One action through the single-action path at block `height`.
fn exec(db: &StateDb, height: u64, (sender, action): (Address, NativeAction)) -> NativeActionResult {
    NativeExecutor::execute(&mut ctx_at(db.clone(), height), &sender, &action)
}

fn sub_rows<T: StateBackend>(s: &T) -> usize {
    s.iterate_cf(CF_NATIVE_ORACLE, Some(b"sub")).unwrap().len()
}

fn assert_rejected(r: &NativeActionResult, needle: &str) {
    assert!(!r.success, "expected a rejection containing {needle:?}");
    let e = r.error.as_deref().unwrap_or("");
    assert!(e.contains(needle), "error {e:?} lacks {needle:?}");
}

/// Every bad entry rejects the WHOLE action and nothing is written — not even
/// the valid entries before it (there is no per-action rollback, NE:3291).
#[test]
fn submission_hardening_rejects_the_whole_action_and_writes_nothing() {
    let (_d, db) = oracle_db();
    let over_max = FixedPoint::from_raw(MAX_ORACLE_PRICE_RAW + 1);
    let oversize: Vec<_> =
        (0..=MAX_ORACLE_PRICES_PER_SUBMISSION as u64).map(|m| (m, fp(1))).collect();
    let cases: Vec<(Vec<(MarketId, FixedPoint)>, &str)> = vec![
        (vec![(1, fp(100)), (9, fp(100))], "market 9 is not listed"),
        (vec![(1, fp(100)), (2, FixedPoint::ZERO)], "invalid oracle price"),
        (vec![(1, fp(-1))], "invalid oracle price"),
        (vec![(1, over_max)], "invalid oracle price"),
        (vec![(1, fp(100)), (1, fp(101))], "duplicate market 1"),
        (vec![], "1..=256 prices"),
        (oversize, "1..=256 prices"),
    ];
    for (prices, needle) in cases {
        assert_rejected(&exec(&db, 5, submit(V1, &prices)), needle);
        assert_eq!(sub_rows(&db), 0, "{needle}: no row written");
    }
}

#[test]
fn submission_at_the_cap_over_listed_markets_is_accepted() {
    let (_d, db) = oracle_db();
    for m in 3..=MAX_ORACLE_PRICES_PER_SUBMISSION as u64 {
        list_market(&db, m);
    }
    let prices: Vec<_> =
        (1..=MAX_ORACLE_PRICES_PER_SUBMISSION as u64).map(|m| (m, fp(m as i64))).collect();
    let r = exec(&db, 5, submit(V1, &prices));
    assert!(r.success, "{:?}", r.error);
    assert_eq!(sub_rows(&db), MAX_ORACLE_PRICES_PER_SUBMISSION);
}

/// Pin (GREEN today): only ACTIVE validators may submit.
#[test]
fn only_active_validators_may_submit() {
    let (_d, db) = oracle_db();
    put_validator(&db, 4, MIN_SELF_DELEGATION, ValidatorStatus::Candidate);
    put_validator(&db, 5, MIN_SELF_DELEGATION, ValidatorStatus::Jailed);
    for (who, needle) in [(4, "not active"), (5, "not active"), (6, "not a registered validator")] {
        assert_rejected(&exec(&db, 5, submit(who, &[(1, fp(100))])), needle);
    }
    assert_eq!(sub_rows(&db), 0);
}
