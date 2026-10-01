//! s517 oracle feeder (item 2, option B), commit 1: the validator hot oracle
//! signer (`SetOracleSigner`). `docs/plans/oracle-feeder.md` §1 and
//! `docs/plans/oracle-feeder-impl.md` S2/S3.

use alloy_primitives::{hex, Address, U256};

use torus_bridge::native_executor::{NativeActionResult, NativeExecContext, NativeExecutor};
use torus_core::position::NativeBalance;
use torus_economics::{StakingManager, ValidatorState, ValidatorStatus, MIN_SELF_DELEGATION};
use torus_state::cf::{
    CF_JAIL_VOTES, CF_NATIVE_BALANCES, CF_NATIVE_MARKETS, CF_NATIVE_ORACLE, CF_NATIVE_ORDERS,
    CF_NATIVE_POSITIONS, CF_SESSIONS, CF_SLASH_RECORDS, CF_STAKING_DELEGATIONS,
    CF_STAKING_PERMANENT, CF_STAKING_REWARDS, CF_STAKING_VALIDATORS,
};
use torus_state::{StateBackend, StateDb};
use torus_types::{
    FixedPoint, MarketId, NativeAction, OracleSubmission, OrderType, PlaceOrderParams,
    TimeInForce,
};

// ---- helpers copied from oracle_block_tests.rs:17-96 (+ oracle_signer) ----

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

fn ctx_at<T: StateBackend>(state: T, height: u64) -> NativeExecContext<T> {
    NativeExecContext::new(state, height, 1_000 + height, 0, 1_000, 10, addr(99), addr(100), addr(101))
}

const V1: u8 = 1;
const V2: u8 = 2;
const V3: u8 = 3; // 3x stake
const S: Address = Address::new([50; 20]);
const S2: Address = Address::new([51; 20]);

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
                oracle_signer: None,
            },
        )
        .unwrap();
}

/// Change only the status of an existing record (keeps `oracle_signer`).
fn put_validator_status(db: &StateDb, n: u8, status: ValidatorStatus) {
    let sm = StakingManager::new(db.clone());
    let mut v = sm.get_validator(&addr(n)).unwrap().unwrap();
    v.status = status;
    sm.put_validator(&addr(n), &v).unwrap();
}

fn list_market(db: &StateDb, id: MarketId) {
    db.put_cf_raw(CF_NATIVE_MARKETS, &id.to_be_bytes(), b"listed").unwrap();
}

fn oracle_db() -> (tempfile::TempDir, StateDb) {
    let (dir, db) = open_test_db();
    put_validator(&db, V1, MIN_SELF_DELEGATION, ValidatorStatus::Active);
    put_validator(&db, V2, MIN_SELF_DELEGATION, ValidatorStatus::Active);
    put_validator(&db, V3, MIN_SELF_DELEGATION * U256::from(3u8), ValidatorStatus::Active);
    list_market(&db, 1);
    list_market(&db, 2);
    (dir, db)
}

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

fn set(sender: u8, signer: Address) -> (Address, NativeAction) {
    (addr(sender), NativeAction::SetOracleSigner { signer })
}

fn signer_of(db: &StateDb, v: u8) -> Option<Address> {
    StakingManager::new(db.clone()).get_validator(&addr(v)).unwrap().unwrap().oracle_signer
}

fn index(db: &StateDb, s: Address) -> Option<Vec<u8>> {
    db.get_cf_raw(CF_NATIVE_ORACLE, &torus_state::cf::oracle_signer_key(&s)).unwrap()
}

fn submit_from(sender: Address, prices: &[(MarketId, FixedPoint)]) -> (Address, NativeAction) {
    (
        sender,
        NativeAction::SubmitOraclePrices(OracleSubmission { prices: prices.to_vec(), timestamp: 0 }),
    )
}

fn submit(sender: u8, prices: &[(MarketId, FixedPoint)]) -> (Address, NativeAction) {
    submit_from(addr(sender), prices)
}

fn mark<T: StateBackend>(ctx: &NativeExecContext<T>, m: MarketId) -> Option<FixedPoint> {
    ctx.oracle.get_price(m, ctx.timestamp).ok()?.usable()
}

fn run_block(db: &StateDb, height: u64, actions: &[(Address, NativeAction)]) -> Vec<NativeActionResult> {
    let mut ctx = ctx_at(db.clone(), height);
    NativeExecutor::begin_block_oracle(&mut ctx);
    let res = NativeExecutor::execute_batch(&mut ctx, actions).results;
    assert!(ctx.fatal_error.is_none(), "{:?}", ctx.fatal_error);
    res
}

// ---- S2: SetOracleSigner ----

#[test]
fn validator_sets_and_rotates_its_signer() {
    let (_d, db) = oracle_db();
    assert!(exec(&db, 1, set(V1, S)).success);
    assert_eq!(signer_of(&db, V1), Some(S));
    assert_eq!(index(&db, S).as_deref(), Some(addr(V1).as_slice()));
    assert!(exec(&db, 2, set(V1, S)).success, "re-setting the same signer is a no-op");
    assert_eq!(index(&db, S).as_deref(), Some(addr(V1).as_slice()));
    assert!(exec(&db, 3, set(V1, S2)).success);
    assert_eq!(signer_of(&db, V1), Some(S2));
    assert_eq!(index(&db, S), None, "rotation deletes the old index entry");
    assert_eq!(index(&db, S2).as_deref(), Some(addr(V1).as_slice()));
    assert!(exec(&db, 4, set(V1, Address::ZERO)).success);
    assert_eq!(signer_of(&db, V1), None);
    assert_eq!(index(&db, S2), None);
    assert!(exec(&db, 5, set(V1, Address::ZERO)).success, "clearing an unset signer is a no-op");
}

#[test]
fn non_validator_cannot_set_a_signer() {
    let (_d, db) = oracle_db();
    assert_rejected(&exec(&db, 1, set(77, S)), "not a registered validator");
    assert_eq!(index(&db, S), None);
}

#[test]
fn a_signer_serves_one_validator_and_is_not_a_validator() {
    let (_d, db) = oracle_db();
    assert!(exec(&db, 1, set(V1, S)).success);
    assert_rejected(&exec(&db, 2, set(V2, S)), "already serves");
    assert_eq!(signer_of(&db, V2), None);
    assert_eq!(index(&db, S).as_deref(), Some(addr(V1).as_slice()));
    assert_rejected(&exec(&db, 3, set(V2, addr(V3))), "is a validator");
    assert_rejected(&exec(&db, 4, set(V2, addr(V2))), "is a validator"); // self
    assert_eq!(signer_of(&db, V2), None);
    assert_eq!(index(&db, addr(V3)), None);
}

#[test]
fn tombstoned_validator_cannot_set_but_candidate_can() {
    let (_d, db) = oracle_db();
    put_validator(&db, 4, MIN_SELF_DELEGATION, ValidatorStatus::Tombstoned);
    put_validator(&db, 5, MIN_SELF_DELEGATION, ValidatorStatus::Candidate);
    put_validator(&db, 6, MIN_SELF_DELEGATION, ValidatorStatus::Jailed);
    assert_rejected(&exec(&db, 1, set(4, S)), "tombstoned");
    assert_eq!(index(&db, S), None);
    assert!(exec(&db, 2, set(5, S)).success);
    assert_eq!(signer_of(&db, 5), Some(S));
    assert!(exec(&db, 3, set(6, S2)).success, "jailed may (re)set its signer");
}

// ---- S3: the signer reports for its validator, and nothing else ----

#[test]
fn signer_submission_is_keyed_and_weighted_by_its_validator() {
    let (_d, db) = oracle_db();
    assert!(exec(&db, 1, set(V3, S)).success);
    assert!(exec(&db, 2, submit_from(S, &[(1, fp(100))])).success);
    let rows = db.iterate_cf(CF_NATIVE_ORACLE, Some(b"sub")).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(&rows[0].0[11..31], addr(V3).as_slice(), "row key is \"sub\"‖market(8)‖VALIDATOR");

    // Weighting: V1=100 and V2=101 directly, V3 (3x stake) 110 via its signer.
    // Stake-weighted (1,1,3) -> 110; were V3 weighted as 1 it would be 101.
    let r = run_block(&db, 5, &[
        submit(V1, &[(1, fp(100))]),
        submit(V2, &[(1, fp(101))]),
        submit_from(S, &[(1, fp(110))]),
    ]);
    assert!(r.iter().all(|x| x.success), "{r:?}");
    let mut ctx = ctx_at(db.clone(), 6);
    NativeExecutor::begin_block_oracle(&mut ctx);
    assert_eq!(mark(&ctx, 1), Some(fp(110)));
    assert_eq!(ctx.oracle.get_price(1, 1_006).unwrap().num_reporters, 3);
}

#[test]
fn unknown_signer_and_jailed_validators_signer_are_rejected() {
    let (_d, db) = oracle_db();
    assert_rejected(
        &exec(&db, 1, submit_from(S, &[(1, fp(100))])),
        "not a registered validator or oracle signer",
    );
    assert!(exec(&db, 2, set(V1, S)).success);
    put_validator_status(&db, V1, ValidatorStatus::Jailed); // keeps oracle_signer
    assert_rejected(&exec(&db, 3, submit_from(S, &[(1, fp(100))])), "not active");
    assert_eq!(sub_rows(&db), 0);
}

/// A stale index entry (the record no longer names the signer) never resolves.
#[test]
fn stale_index_entry_does_not_resolve() {
    let (_d, db) = oracle_db();
    db.put_cf_raw(CF_NATIVE_ORACLE, &torus_state::cf::oracle_signer_key(&S), addr(V1).as_slice())
        .unwrap();
    assert_rejected(&exec(&db, 1, submit_from(S, &[(1, fp(100))])), "oracle signer");
    assert_eq!(sub_rows(&db), 0);
}

#[test]
fn old_signer_is_rejected_after_rotation() {
    let (_d, db) = oracle_db();
    assert!(exec(&db, 1, set(V1, S)).success);
    assert!(exec(&db, 2, set(V1, S2)).success);
    assert_rejected(&exec(&db, 3, submit_from(S, &[(1, fp(100))])), "oracle signer");
    assert!(exec(&db, 4, submit_from(S2, &[(1, fp(100))])).success);
    let rows = db.iterate_cf(CF_NATIVE_ORACLE, Some(b"sub")).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(&rows[0].0[11..31], addr(V1).as_slice());
}

/// Everything of V1 an action could touch: rows whose key carries V1's address
/// in the native account / staking CFs, plus V1's resting orders in memory.
fn dump_validator_state<T: StateBackend>(ctx: &NativeExecContext<T>, v: Address) -> Vec<String> {
    let mut out = Vec::new();
    for cf in [
        CF_NATIVE_BALANCES,
        CF_NATIVE_POSITIONS,
        CF_NATIVE_ORDERS,
        CF_STAKING_VALIDATORS,
        CF_STAKING_DELEGATIONS,
        CF_STAKING_PERMANENT,
        CF_STAKING_REWARDS,
        CF_SESSIONS,
        CF_JAIL_VOTES,
        CF_SLASH_RECORDS,
    ] {
        for (k, val) in ctx.state.iterate_cf(cf, None).unwrap() {
            if k.windows(20).any(|w| w == v.as_slice()) {
                out.push(format!("{cf} {} {}", hex::encode(&k), hex::encode(&val)));
            }
        }
    }
    let mut markets: Vec<_> = ctx.order_books.keys().copied().collect();
    markets.sort_unstable();
    for m in markets {
        for o in ctx.order_books[&m].orders_for_trader(&v) {
            out.push(format!("order {m} {o:?}"));
        }
    }
    out
}

/// D-S1: the signer has NO authority over the validator's account. Every
/// handler keys off `sender` (= the signer's own address).
#[test]
fn signer_cannot_act_for_its_validator() {
    let (_d, db) = oracle_db();
    assert!(exec(&db, 1, set(V1, S)).success);
    let mut ctx = ctx_at(db.clone(), 2);
    for who in [addr(V1), S] {
        ctx.positions
            .put_native_balance(&who, &NativeBalance { available: fp(1_000), order_margin: FixedPoint::ZERO })
            .unwrap();
    }
    let order = |is_buy: bool, price: i64| {
        NativeAction::PlaceOrder(PlaceOrderParams {
            market_id: 1,
            is_buy,
            price: fp(price),
            quantity: fp(1),
            order_type: OrderType::Limit,
            time_in_force: TimeInForce::GTC,
            reduce_only: false,
            client_order_id: None,
        })
    };
    let r = NativeExecutor::execute(&mut ctx, &addr(V1), &order(true, 90));
    assert!(r.success, "{:?}", r.error);
    let v1_order = ctx.order_books[&1].orders_for_trader(&addr(V1))[0].id;
    let before = dump_validator_state(&ctx, addr(V1));
    assert!(before.iter().any(|l| l.starts_with("order 1")), "{before:?}");

    for action in [
        order(false, 200),
        NativeAction::CancelOrder { order_id: v1_order },
        NativeAction::CancelAllOrders { market_id: None },
        NativeAction::ModifyOrder { order_id: v1_order, new_price: Some(fp(80)), new_qty: None },
        NativeAction::Withdraw { amount: U256::from(1u8), to: S },
        NativeAction::TransferToSpot { amount: U256::from(1u8) },
        NativeAction::ClaimRewards,
        NativeAction::ClaimUnbonded,
        NativeAction::UnjailSelf,
        NativeAction::UpdateCommission { new_rate: 1 },
    ] {
        let _ = NativeExecutor::execute(&mut ctx, &S, &action);
    }
    // A signer cannot rotate (or clear) the signer of its validator.
    for signer in [S2, Address::ZERO] {
        let r = NativeExecutor::execute(&mut ctx, &S, &NativeAction::SetOracleSigner { signer });
        assert_rejected(&r, "not a registered validator");
    }
    assert_eq!(dump_validator_state(&ctx, addr(V1)), before, "validator state byte-identical");
    assert_eq!(signer_of(&db, V1), Some(S));
    assert_eq!(index(&db, S).as_deref(), Some(addr(V1).as_slice()));
}
