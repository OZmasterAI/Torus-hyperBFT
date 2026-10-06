//! Executor-level tests for `NativeAction::ClaimUnbonded` (parity audit BUG 3).
//!
//! `undelegate` parks stake in `Delegation.unbonding`; before this action nothing
//! in production ever released it. `ClaimUnbonded` is the user-initiated claim of
//! every matured entry across all of the sender's delegations.

use alloy_primitives::{Address, U256};
use revm::state::AccountInfo;

use torus_bridge::native_executor::{
    classify_action, ActionCategory, NativeExecContext, NativeExecutor,
};
use torus_core::precompiles::{
    execute_precompile, precompile_address, QueuedActionKind, ADDR_CORE_WRITER_STAKING,
};
use torus_economics::staking::delegation_key;
use torus_economics::{MIN_SELF_DELEGATION, UNBONDING_PERIOD};
use torus_state::cf::CF_STAKING_DELEGATIONS;
use torus_state::StateDb;
use torus_types::NativeAction;

fn open_test_db() -> (tempfile::TempDir, StateDb) {
    let dir = tempfile::tempdir().expect("create temp dir");
    let db = StateDb::open(dir.path()).expect("open db");
    (dir, db)
}

fn addr(n: u8) -> Address {
    Address::new([n; 20])
}

fn wei(tokens: u64) -> U256 {
    U256::from(tokens) * U256::from(10u64).pow(U256::from(18u64))
}

fn ctx_at(db: &StateDb, height: u64) -> NativeExecContext {
    NativeExecContext::new(
        db.clone(),
        height,
        1000 + height, // timestamp
        0,             // epoch
        100_000_000,   // epoch_length (never hit an epoch boundary here)
        10,            // max_validators
        addr(99),      // proposer
        addr(100),     // treasury
        addr(101),     // dev_pool
    )
}

fn balance(db: &StateDb, a: &Address) -> U256 {
    db.get_account(a).unwrap().unwrap_or_default().balance
}

fn fund(db: &StateDb, a: &Address, amount: U256) {
    let info = AccountInfo {
        balance: amount,
        ..Default::default()
    };
    db.put_account(a, &info).unwrap();
}

const VALIDATOR: u8 = 1;
const DELEGATOR: u8 = 2;

/// Register validator 1, fund delegator 2 and delegate `amount` via the executor.
fn setup_delegation(db: &StateDb, amount: U256) {
    let val = addr(VALIDATOR);
    fund(db, &val, MIN_SELF_DELEGATION);
    let ctx = ctx_at(db, 1);
    ctx.staking
        .register_validator(val, [VALIDATOR; 32], 500, MIN_SELF_DELEGATION)
        .unwrap();

    fund(db, &addr(DELEGATOR), amount);
    let mut ctx = ctx_at(db, 1);
    let r = NativeExecutor::execute(
        &mut ctx,
        &addr(DELEGATOR),
        &NativeAction::Delegate {
            validator: val,
            amount,
        },
    );
    assert!(r.success, "delegate failed: {:?}", r.error);
    assert_eq!(balance(db, &addr(DELEGATOR)), U256::ZERO);
}

fn undelegate_at(db: &StateDb, height: u64, amount: U256) {
    let mut ctx = ctx_at(db, height);
    let r = NativeExecutor::execute(
        &mut ctx,
        &addr(DELEGATOR),
        &NativeAction::Undelegate {
            validator: addr(VALIDATOR),
            amount,
        },
    );
    assert!(r.success, "undelegate failed: {:?}", r.error);
}

fn claim_at(db: &StateDb, height: u64) -> torus_bridge::native_executor::NativeActionResult {
    let mut ctx = ctx_at(db, height);
    NativeExecutor::execute(&mut ctx, &addr(DELEGATOR), &NativeAction::ClaimUnbonded)
}

fn delegation_row(db: &StateDb) -> Option<Vec<u8>> {
    db.get_cf_raw(
        CF_STAKING_DELEGATIONS,
        &delegation_key(&addr(DELEGATOR), &addr(VALIDATOR)),
    )
    .unwrap()
}

#[test]
fn claim_unbonded_classified_as_staking() {
    assert_eq!(
        classify_action(&NativeAction::ClaimUnbonded),
        ActionCategory::Staking
    );
}

#[test]
fn claim_unbonded_before_maturity_errors_and_credits_nothing() {
    let (_dir, db) = open_test_db();
    setup_delegation(&db, wei(100));
    let h = 10;
    undelegate_at(&db, h, wei(100));
    let row_before = delegation_row(&db);
    assert!(row_before.is_some());

    let r = claim_at(&db, h + UNBONDING_PERIOD - 1);
    assert!(!r.success, "claim before maturity must fail");
    assert_eq!(r.action_type, "claim_unbonded");
    assert!(
        r.error
            .as_deref()
            .unwrap_or("")
            .contains("no matured unbonding"),
        "unexpected error: {:?}",
        r.error
    );
    assert_eq!(balance(&db, &addr(DELEGATOR)), U256::ZERO);
    assert_eq!(delegation_row(&db), row_before, "state must be untouched");
}

#[test]
fn claim_unbonded_at_maturity_credits_and_deletes_record() {
    let (_dir, db) = open_test_db();
    setup_delegation(&db, wei(100));
    let h = 10;
    undelegate_at(&db, h, wei(100));

    let r = claim_at(&db, h + UNBONDING_PERIOD);
    assert!(r.success, "claim at maturity failed: {:?}", r.error);
    assert_eq!(balance(&db, &addr(DELEGATOR)), wei(100));
    assert!(
        delegation_row(&db).is_none(),
        "fully released, zero-amount delegation must be deleted"
    );

    // Second claim: nothing left.
    let r = claim_at(&db, h + UNBONDING_PERIOD + 1);
    assert!(!r.success);
    assert_eq!(balance(&db, &addr(DELEGATOR)), wei(100));
}

#[test]
fn claim_unbonded_partial_maturity_releases_only_matured_entries() {
    let (_dir, db) = open_test_db();
    setup_delegation(&db, wei(100));
    let h1 = 10;
    let h2 = 50;
    undelegate_at(&db, h1, wei(30));
    undelegate_at(&db, h2, wei(20));

    // Only the first entry has matured.
    let r = claim_at(&db, h1 + UNBONDING_PERIOD);
    assert!(r.success, "partial claim failed: {:?}", r.error);
    assert_eq!(balance(&db, &addr(DELEGATOR)), wei(30));
    assert!(
        delegation_row(&db).is_some(),
        "active stake + 1 entry remain"
    );

    // Between maturities: nothing new to claim.
    let r = claim_at(&db, h2 + UNBONDING_PERIOD - 1);
    assert!(!r.success);
    assert_eq!(balance(&db, &addr(DELEGATOR)), wei(30));

    let r = claim_at(&db, h2 + UNBONDING_PERIOD);
    assert!(r.success, "second claim failed: {:?}", r.error);
    assert_eq!(balance(&db, &addr(DELEGATOR)), wei(50));
    // 50 still actively delegated -> record kept.
    assert!(delegation_row(&db).is_some());
}

#[test]
fn claim_unbonded_spans_all_delegations_of_sender() {
    let (_dir, db) = open_test_db();
    setup_delegation(&db, wei(100));
    // Second validator (3) with its own delegation from the same delegator.
    let val2 = addr(3);
    fund(&db, &val2, MIN_SELF_DELEGATION);
    let ctx = ctx_at(&db, 1);
    ctx.staking
        .register_validator(val2, [3; 32], 500, MIN_SELF_DELEGATION)
        .unwrap();
    fund(&db, &addr(DELEGATOR), wei(40));
    ctx.staking
        .delegate(addr(DELEGATOR), val2, wei(40))
        .unwrap();

    undelegate_at(&db, 10, wei(100));
    let mut ctx = ctx_at(&db, 10);
    let r = NativeExecutor::execute(
        &mut ctx,
        &addr(DELEGATOR),
        &NativeAction::Undelegate {
            validator: val2,
            amount: wei(40),
        },
    );
    assert!(r.success);

    let r = claim_at(&db, 10 + UNBONDING_PERIOD);
    assert!(r.success, "{:?}", r.error);
    assert_eq!(balance(&db, &addr(DELEGATOR)), wei(140));
}

#[test]
fn claim_unbonded_with_no_delegations_errors() {
    let (_dir, db) = open_test_db();
    let r = claim_at(&db, 1_000);
    assert!(!r.success);
    assert_eq!(balance(&db, &addr(DELEGATOR)), U256::ZERO);
}

#[test]
fn claim_unbonded_via_core_writer_executes_next_block() {
    let (_dir, db) = open_test_db();
    setup_delegation(&db, wei(100));
    let h = 10;
    undelegate_at(&db, h, wei(100));

    // EVM contract (the delegator) calls CoreWriterStaking.claimUnbonded() in block B.
    let b = h + UNBONDING_PERIOD;
    let sel = alloy_primitives::keccak256("claimUnbonded()".as_bytes());
    let input = sel[..4].to_vec();
    let out = execute_precompile(
        &precompile_address(ADDR_CORE_WRITER_STAKING),
        &input,
        &addr(DELEGATOR),
        &db,
        b,
        0,
    )
    .expect("claimUnbonded() selector must be accepted");
    assert_eq!(out.len(), 32, "returns a single ABI word");

    // Same block: nothing drained, nothing credited.
    let mut ctx = ctx_at(&db, b);
    let results = NativeExecutor::drain_core_writer(&mut ctx).unwrap();
    assert!(results.is_empty());
    assert_eq!(balance(&db, &addr(DELEGATOR)), U256::ZERO);

    // Next block: queued ClaimUnbonded executes for the caller.
    let mut ctx = ctx_at(&db, b + 1);
    let results = NativeExecutor::drain_core_writer(&mut ctx).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].action_type, "claim_unbonded");
    assert!(results[0].success, "{:?}", results[0].error);
    assert_eq!(balance(&db, &addr(DELEGATOR)), wei(100));
    assert!(delegation_row(&db).is_none());
}

#[test]
fn queued_claim_unbonded_borsh_round_trips() {
    use borsh::BorshDeserialize;
    let kind = QueuedActionKind::ClaimUnbonded;
    let bytes = borsh::to_vec(&kind).unwrap();
    assert_eq!(bytes, vec![7], "appended tag; existing tags 0-6 unchanged");
    let back = QueuedActionKind::try_from_slice(&bytes).unwrap();
    assert!(matches!(back, QueuedActionKind::ClaimUnbonded));
}
