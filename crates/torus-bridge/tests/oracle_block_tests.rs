//! Item 2 (option A, time-based): oracle aggregation into block execution.
//! `docs/plans/oracle-aggregation.md` and `docs/plans/oracle-aggregation-impl.md`.

use alloy_primitives::{Address, U256};

use torus_bridge::native_executor::{NativeActionResult, NativeExecContext, NativeExecutor};
use torus_core::oracle::{MAX_ORACLE_PRICES_PER_SUBMISSION, MAX_ORACLE_PRICE_RAW};
use torus_core::position::NativeBalance;
use torus_economics::{StakingManager, ValidatorState, ValidatorStatus, MIN_SELF_DELEGATION};
use torus_state::cf::{CF_NATIVE_MARKETS, CF_NATIVE_ORACLE};
use torus_state::{NativeStateOverlay, StateBackend, StateDb};
use torus_types::{
    FixedPoint, MarketId, NativeAction, OracleSubmission, OrderType, PlaceOrderParams,
    TimeInForce,
};

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

/// The mark as AccountReader::mark sees it in `ctx` (+ the stamp block).
fn mark<T: StateBackend>(ctx: &NativeExecContext<T>, m: MarketId) -> Option<(FixedPoint, u64)> {
    let p = ctx.oracle.get_price(m, ctx.timestamp).ok()?;
    p.usable().map(|px| (px, p.block_number))
}

/// Block `height` on `db`: the oracle step, then `actions` (batch path).
fn run_block(
    db: &StateDb,
    height: u64,
    actions: &[(Address, NativeAction)],
) -> (Vec<NativeActionResult>, Vec<NativeActionResult>) {
    let mut ctx = ctx_at(db.clone(), height);
    let agg = NativeExecutor::begin_block_oracle(&mut ctx);
    let res = NativeExecutor::execute_batch(&mut ctx, actions).results;
    assert!(ctx.fatal_error.is_none(), "{:?}", ctx.fatal_error);
    (agg, res)
}

fn round(price_m1: i64) -> Vec<(Address, NativeAction)> {
    [V1, V2, V3].iter().map(|&v| submit(v, &[(1, fp(price_m1))])).collect()
}

fn fund(ctx: &NativeExecContext, who: Address, amount: i64) {
    ctx.positions
        .put_native_balance(&who, &NativeBalance { available: fp(amount), order_margin: FixedPoint::ZERO })
        .unwrap();
}

/// Stake weighting (V3 = 3x): 100 / 101 / 102 -> 102 (simple median: 101).
/// One result per LISTED market; an unlisted market's rows are never aggregated.
#[test]
fn block_start_aggregates_every_listed_market_from_earlier_blocks() {
    let (_d, db) = oracle_db();
    let (_, r) = run_block(&db, 5, &[
        submit(V1, &[(1, fp(100)), (2, fp(10))]),
        submit(V2, &[(1, fp(101)), (2, fp(10))]),
        submit(V3, &[(1, fp(102)), (2, fp(10))]),
    ]);
    assert!(r.iter().all(|x| x.success), "{r:?}");
    assert!(ctx_at(db.clone(), 5).oracle.get_price(1, 1_005).is_err(), "not aggregated in block 5");

    let mut ctx = ctx_at(db.clone(), 6);
    for v in [V1, V2, V3] {
        ctx.oracle.submit_price(&addr(v), 3, fp(7), 5, 1_005).unwrap(); // unlisted, planted
    }
    let agg = NativeExecutor::begin_block_oracle(&mut ctx);
    assert_eq!(agg.len(), 2);
    assert!(agg.iter().all(|x| x.success), "{agg:?}");
    assert_eq!(mark(&ctx, 1), Some((fp(102), 6)));
    assert_eq!(mark(&ctx, 2), Some((fp(10), 6)));
    assert_eq!(ctx.oracle.get_price(1, 1_006).unwrap().num_reporters, 3);
    assert!(ctx.oracle.get_price(3, 1_006).is_err(), "unlisted market 3 is never aggregated");
}

/// Timing: a submission of block 5 (ts 1005) counts through ts 1015 (age 10) and
/// is deleted at ts 1016; the aggregate (stamped 1015) is usable through ts 1075.
#[test]
fn a_submission_counts_for_ten_seconds_then_is_pruned() {
    let (_d, db) = oracle_db();
    run_block(&db, 5, &round(100));
    for h in 6..=15 {
        let (agg, _) = run_block(&db, h, &[]);
        assert!(agg[0].success, "block {h}: {:?}", agg[0].error);
        assert_eq!(mark(&ctx_at(db.clone(), h), 1), Some((fp(100), h)), "block {h}: fresh");
    }
    assert_eq!(sub_rows(&db), 3);
    run_block(&db, 16, &[]);
    assert_eq!(sub_rows(&db), 0, "age 11: deleted at block start");
    let p = ctx_at(db.clone(), 16).oracle.get_price(1, 1_016).unwrap();
    assert_eq!((p.block_number, p.timestamp), (15, 1_015), "not re-stamped without a quorum");
    assert_eq!(mark(&ctx_at(db.clone(), 75), 1), Some((fp(100), 15)), "age 60");
    assert_eq!(mark(&ctx_at(db.clone(), 76), 1), None, "age 61: stale");
}

/// Only the block-start step writes the aggregate: submissions EARLIER in the
/// same batch do not move the mark a placement reserves at.
#[test]
fn every_reader_in_a_block_sees_the_block_start_mark() {
    let (_d, db) = oracle_db();
    run_block(&db, 5, &round(100));
    let mut ctx = ctx_at(db.clone(), 6);
    NativeExecutor::begin_block_oracle(&mut ctx);
    assert_eq!(mark(&ctx, 1), Some((fp(100), 6)));
    let taker = addr(20);
    fund(&ctx, taker, 40);
    let market_buy = PlaceOrderParams {
        market_id: 1,
        is_buy: true,
        price: fp(200), // cap
        quantity: fp(10),
        order_type: OrderType::Market,
        time_in_force: TimeInForce::IOC,
        reduce_only: false,
        client_order_id: None,
    };
    let mut actions = round(300);
    actions.push((taker, NativeAction::PlaceOrder(market_buy)));
    let r = NativeExecutor::execute_batch(&mut ctx, &actions).results;
    assert!(r[..3].iter().all(|x| x.success), "{r:?}");
    // reserved at the block-start mark: 10 x 100 / 20 = 50 > 40 (at 300 it would be the cap: 100)
    assert_rejected(&r[3], "need 50.00000000");
    assert_eq!(mark(&ctx, 1), Some((fp(100), 6)), "same-block submissions never move the mark");
    let mut next = ctx_at(db.clone(), 7);
    NativeExecutor::begin_block_oracle(&mut next);
    assert_eq!(mark(&next, 1), Some((fp(300), 7)));
}

/// Market 2's stored aggregate is corrupt (its 2-reporter fallback read fails),
/// market 3 has no data: both are error RESULTS; market 1 aggregates and the
/// block goes on.
#[test]
fn aggregation_errors_are_per_market_and_never_abort_the_block() {
    let (_d, db) = oracle_db();
    list_market(&db, 3);
    run_block(&db, 5, &[
        submit(V1, &[(1, fp(100)), (2, fp(10))]),
        submit(V2, &[(1, fp(100)), (2, fp(10))]),
        submit(V3, &[(1, fp(100))]),
    ]);
    db.put_cf_raw(CF_NATIVE_ORACLE, &[b"agg".as_slice(), &2u64.to_be_bytes()].concat(), &[1, 2, 3])
        .unwrap();
    let mut ctx = ctx_at(db.clone(), 6);
    let agg = NativeExecutor::begin_block_oracle(&mut ctx);
    assert_eq!(agg.iter().map(|r| r.success).collect::<Vec<_>>(), vec![true, false, false]);
    assert!(ctx.fatal_error.is_none());
    assert_eq!(mark(&ctx, 1), Some((fp(100), 6)));
    let buyer = addr(21);
    fund(&ctx, buyer, 1_000);
    let bid = PlaceOrderParams {
        market_id: 1,
        is_buy: true,
        price: fp(90),
        quantity: fp(1),
        order_type: OrderType::Limit,
        time_in_force: TimeInForce::GTC,
        reduce_only: false,
        client_order_id: None,
    };
    let r = NativeExecutor::execute_batch(&mut ctx, &[(buyer, NativeAction::PlaceOrder(bid))]).results;
    assert!(r[0].success, "{:?}", r[0].error);
}

/// Weights = whole-token stake of ACTIVE validators: a jailed validator's
/// (planted) row is ignored; a stake beyond u64::MAX tokens saturates.
#[test]
fn stakes_are_whole_token_power_of_active_validators() {
    let (_d, db) = oracle_db();
    put_validator(&db, 4, U256::MAX, ValidatorStatus::Active);
    put_validator(&db, 5, MIN_SELF_DELEGATION * U256::from(1_000u32), ValidatorStatus::Jailed);
    let seed = ctx_at(db.clone(), 5);
    for (v, p) in [(V1, 100), (V2, 101), (V3, 102), (4, 103), (5, 50_000)] {
        seed.oracle.submit_price(&addr(v), 1, fp(p), 5, 1_005).unwrap();
    }
    let mut ctx = ctx_at(db.clone(), 6);
    NativeExecutor::begin_block_oracle(&mut ctx);
    assert_eq!(mark(&ctx, 1), Some((fp(103), 6)), "saturated whale dominates; jailed row ignored");
    assert_eq!(ctx.oracle.get_price(1, 1_006).unwrap().num_reporters, 4);
}

/// Through the handler, 3 validators x 2 markets every block: one row per pair
/// (<= 6 rows ever), all gone 11 s after the last submission.
#[test]
fn rows_stay_one_per_validator_and_market_and_are_pruned() {
    let (_d, db) = oracle_db();
    for h in 1..=40u64 {
        let subs: Vec<_> =
            [V1, V2, V3].iter().map(|&v| submit(v, &[(1, fp(100)), (2, fp(10))])).collect();
        let (_, r) = run_block(&db, h, &subs);
        assert!(r.iter().all(|x| x.success));
        assert!(sub_rows(&db) <= 6, "block {h}");
    }
    for h in 41..=51 {
        run_block(&db, h, &[]);
    }
    assert_eq!(sub_rows(&db), 0);
}

/// Without submission rows the step writes NOTHING (existing roots unchanged).
#[test]
fn block_start_writes_nothing_without_submissions() {
    let (_d, db) = oracle_db();
    let overlay = NativeStateOverlay::new(db.clone());
    let mut ctx = ctx_at(overlay.clone(), 7);
    let agg = NativeExecutor::begin_block_oracle(&mut ctx);
    assert!(agg.iter().all(|r| !r.success));
    assert_eq!(overlay.pending_write_count(), 0);
}

/// The due-check must see a not-yet-durable parent layer (pipelined exec).
#[test]
fn oracle_due_reads_through_the_parent_layer() {
    let (_d, db) = oracle_db();
    assert!(!NativeExecutor::oracle_due(&db).unwrap());
    let o1 = NativeStateOverlay::new(db.clone());
    let (s, a) = submit(V1, &[(1, fp(100))]);
    assert!(NativeExecutor::execute(&mut ctx_at(o1.clone(), 1), &s, &a).success);
    let o2 = NativeStateOverlay::with_parent(db.clone(), Some(o1.freeze(1)));
    assert!(NativeExecutor::oracle_due(&o2).unwrap(), "parent-layer rows count");
    assert!(!NativeExecutor::oracle_due(&db).unwrap(), "the DB alone does not have them yet");
}
