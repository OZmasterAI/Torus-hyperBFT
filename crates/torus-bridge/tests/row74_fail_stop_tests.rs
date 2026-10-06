//! Row 74 (owner s94, 9.14 C; scope from the s96 design check): a LOCAL
//! storage fault in governance / staking / the end-of-block tail latches
//! `ctx.fatal_error` (the node fail-stops before the block's flush, like the
//! liquidation step), while errors every validator hits alike keep today's
//! result and never halt. The action / step result itself is unchanged.

use alloy_primitives::{Address, U256};
use revm::state::AccountInfo;

use torus_bridge::native_executor::{NativeActionResult, NativeExecContext, NativeExecutor};
use torus_economics::governance::{vote_key, GovernanceParams};
use torus_economics::staking::delegation_key;
use torus_economics::MIN_SELF_DELEGATION;
use torus_state::cf::{
    CF_ACCOUNTS, CF_FEE_CONFIG, CF_GOVERNANCE_PROPOSALS, CF_GOVERNANCE_VOTES,
    CF_STAKING_DELEGATIONS,
};
use torus_state::{AtomicWriteOp, NativeStateOverlay, StateBackend, StateDb, StateError};
use torus_types::{NativeAction, Proposal, ProposalAction, VoteOption};

/// The governance module's private row keys in `CF_FEE_CONFIG`.
const GOV_PARAMS_KEY: &[u8] = b"gov_params";
const PROPOSAL_COUNTER_KEY: &[u8] = b"gov_next_id";

/// A backend whose writes to one `(cf, key)` fail (an injected storage
/// fault): a plain put / delete of it, and any `atomic_write` containing it
/// (which then applies nothing). Same as row 75's `PoisonedKey` in
/// `torus-economics/tests/governance_tests.rs`, over any inner backend.
#[derive(Clone)]
struct PoisonedKey<B: StateBackend> {
    inner: B,
    cf: &'static str,
    key: Vec<u8>,
}

impl<B: StateBackend> PoisonedKey<B> {
    fn new(inner: B, cf: &'static str, key: &[u8]) -> Self {
        Self {
            inner,
            cf,
            key: key.to_vec(),
        }
    }
    fn hit(&self, cf: &str, key: &[u8]) -> bool {
        cf == self.cf && key == self.key.as_slice()
    }
    fn fail() -> StateError {
        StateError::InvalidData("injected write failure".into())
    }
}

impl<B: StateBackend> StateBackend for PoisonedKey<B> {
    fn get_cf_raw(&self, cf: &str, key: &[u8]) -> Result<Option<Vec<u8>>, StateError> {
        self.inner.get_cf_raw(cf, key)
    }
    fn put_cf_raw(&self, cf: &str, key: &[u8], value: &[u8]) -> Result<(), StateError> {
        if self.hit(cf, key) {
            return Err(Self::fail());
        }
        self.inner.put_cf_raw(cf, key, value)
    }
    fn delete_cf_raw(&self, cf: &str, key: &[u8]) -> Result<(), StateError> {
        if self.hit(cf, key) {
            return Err(Self::fail());
        }
        self.inner.delete_cf_raw(cf, key)
    }
    fn iterate_cf(
        &self,
        cf: &str,
        prefix: Option<&[u8]>,
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>, StateError> {
        self.inner.iterate_cf(cf, prefix)
    }
    fn atomic_write(&self, ops: &[AtomicWriteOp<'_>]) -> Result<(), StateError> {
        let poisoned = ops.iter().any(|op| match op {
            AtomicWriteOp::Put { cf, key, .. } | AtomicWriteOp::Delete { cf, key } => {
                self.hit(cf, key)
            }
        });
        if poisoned {
            return Err(Self::fail());
        }
        self.inner.atomic_write(ops)
    }
}

const INJECTED: &str = "state error: invalid data: injected write failure";

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

fn make_ctx<B: StateBackend>(state: B, height: u64) -> NativeExecContext<B> {
    NativeExecContext::new(
        state,
        height,
        1000,
        0,
        100,
        10,
        addr(99),
        addr(100),
        addr(101),
    )
}

fn fund(db: &StateDb, who: Address, balance: U256) {
    db.put_account(
        &who,
        &AccountInfo {
            balance,
            ..Default::default()
        },
    )
    .unwrap();
}

/// Governance params, validator `addr(1)`, and `addr(2)` delegating 500 to it
/// (enough stake to propose and to vote). Returns the delegator.
fn setup_staker(db: &StateDb) -> Address {
    let ctx = make_ctx(db.clone(), 1);
    ctx.governance
        .set_governance_params(&GovernanceParams {
            voting_period_blocks: 100,
            quorum_bps: 3300,
            min_proposal_stake: wei(100),
            permanent_weight_multiplier_num: 3,
            permanent_weight_multiplier_den: 2,
            treasury_address: addr(100),
            timelock_blocks: 10,
            permanent_unlock_threshold_bps: 8000,
        })
        .unwrap();
    fund(db, addr(1), MIN_SELF_DELEGATION);
    ctx.staking
        .register_validator(addr(1), [1; 32], 500, MIN_SELF_DELEGATION)
        .unwrap();
    fund(db, addr(2), wei(500));
    ctx.staking.delegate(addr(2), addr(1), wei(500)).unwrap();
    addr(2)
}

/// Text proposal 1 by `proposer`, voting open until block 101.
fn submit_text_proposal(db: &StateDb, proposer: &Address) {
    let mut ctx = make_ctx(db.clone(), 1);
    let r = NativeExecutor::execute(
        &mut ctx,
        proposer,
        &NativeAction::SubmitProposal(Proposal {
            title: "t".into(),
            description: "d".into(),
            action: ProposalAction::UpdateMarketParams {
                market_id: 1,
                params: torus_types::MarketParams {
                    tick_size: torus_types::FixedPoint::ONE,
                    lot_size: torus_types::FixedPoint::ONE,
                    max_leverage: 10,
                    maintenance_margin_bps: 500,
                    max_funding_rate_bps: 100,
                },
            },
        }),
    );
    assert!(r.success, "{:?}", r.error);
}

fn dump(db: &StateDb) -> Vec<Vec<(Vec<u8>, Vec<u8>)>> {
    torus_state::cf::ALL_CF_NAMES
        .iter()
        .map(|cf| StateBackend::iterate_cf(db, cf, None).unwrap())
        .collect()
}

fn assert_err(r: &NativeActionResult, action_type: &str, error: &str) {
    assert_eq!(r.action_type, action_type);
    assert!(!r.success);
    assert_eq!(r.error.as_deref(), Some(error));
    assert_eq!(r.gas_used, 0);
    assert_eq!(format!("{:?}", r.reason), "Other");
}

// ----------------------------------------------------------------------------
// Governance step
// ----------------------------------------------------------------------------

/// A proposal row that does not decode (a local fault: the stored bytes are
/// not what this node wrote) aborts `process_pending_proposals` with a Borsh
/// error: the step's result is unchanged, and the node now halts.
#[test]
fn governance_decode_fault_latches_fatal_error() {
    let (_dir, db) = open_test_db();
    db.put_cf_raw(CF_FEE_CONFIG, PROPOSAL_COUNTER_KEY, &2u64.to_be_bytes())
        .unwrap();
    db.put_cf_raw(CF_GOVERNANCE_PROPOSALS, &1u64.to_be_bytes(), b"\xff")
        .unwrap();
    let mut ctx = make_ctx(db.clone(), 5);
    let results = NativeExecutor::process_governance(&mut ctx);
    assert_eq!(results.len(), 1);
    assert!(!results[0].success);
    assert!(results[0]
        .error
        .as_deref()
        .unwrap()
        .starts_with("borsh serialization error"));
    let fatal = ctx
        .fatal_error
        .as_deref()
        .expect("a storage / decode fault must fail-stop");
    assert!(fatal.contains("governance"), "{fatal}");
}

/// `GovernanceNotInitialized` is hit by every validator alike: today's single
/// error result, byte for byte, and no halt (it would stop the whole chain).
#[test]
fn governance_not_initialized_does_not_halt() {
    let (_dir, db) = open_test_db();
    let proposer = setup_staker(&db);
    submit_text_proposal(&db, &proposer);
    db.delete_cf_raw(CF_FEE_CONFIG, GOV_PARAMS_KEY).unwrap();
    let mut ctx = make_ctx(db.clone(), 200);
    let results = NativeExecutor::process_governance(&mut ctx);
    assert_eq!(results.len(), 1);
    assert_err(
        &results[0],
        "governance_process",
        "governance params not initialized — call set_governance_params at genesis",
    );
    assert!(ctx.fatal_error.is_none(), "{:?}", ctx.fatal_error);
}

// ----------------------------------------------------------------------------
// Staking / governance actions
// ----------------------------------------------------------------------------

/// `delegate` debits the balance, then fails writing the delegation row: the
/// action result is today's, and the node halts (the debit sits in the
/// block's overlay, which the committer then never flushes).
#[test]
fn delegate_write_fault_latches_fatal_error_result_unchanged() {
    let (_dir, db) = open_test_db();
    setup_staker(&db);
    let delegator = addr(3);
    fund(&db, delegator, wei(50));
    let faulty = PoisonedKey::new(
        db.clone(),
        CF_STAKING_DELEGATIONS,
        &delegation_key(&delegator, &addr(1)),
    );
    let mut ctx = make_ctx(faulty, 2);
    let r = NativeExecutor::execute(
        &mut ctx,
        &delegator,
        &NativeAction::Delegate {
            validator: addr(1),
            amount: wei(10),
        },
    );
    assert_err(&r, "delegate", INJECTED);
    let fatal = ctx
        .fatal_error
        .as_deref()
        .expect("a storage fault must fail-stop");
    assert!(
        fatal.contains("delegate") && fatal.contains("injected"),
        "{fatal}"
    );
}

/// Insufficient balance is a user error every validator sees alike: today's
/// result, no halt.
#[test]
fn delegate_insufficient_balance_does_not_halt() {
    let (_dir, db) = open_test_db();
    setup_staker(&db);
    let mut ctx = make_ctx(db.clone(), 2);
    let r = NativeExecutor::execute(
        &mut ctx,
        &addr(3),
        &NativeAction::Delegate {
            validator: addr(1),
            amount: wei(10),
        },
    );
    assert_err(
        &r,
        "delegate",
        "insufficient balance: have 0, need 10000000000000000000",
    );
    assert!(ctx.fatal_error.is_none(), "{:?}", ctx.fatal_error);
}

/// `cast_vote` writes the tally, then the vote row. A fault on the vote row
/// leaves the tally written in the block's overlay only; the node halts, so
/// that overlay is never flushed: the DB is untouched.
#[test]
fn vote_fault_between_tally_and_vote_row_halts_before_any_commit() {
    let (_dir, db) = open_test_db();
    let voter = setup_staker(&db);
    submit_text_proposal(&db, &voter);
    let before = dump(&db);

    let overlay = NativeStateOverlay::new(db.clone());
    let faulty = PoisonedKey::new(overlay.clone(), CF_GOVERNANCE_VOTES, &vote_key(1, &voter));
    let mut ctx = make_ctx(faulty, 2);
    let r = NativeExecutor::execute(
        &mut ctx,
        &voter,
        &NativeAction::Vote {
            proposal_id: 1,
            option: VoteOption::Yes,
        },
    );
    assert_err(&r, "vote", INJECTED);
    assert!(ctx.fatal_error.is_some(), "a storage fault must fail-stop");

    // The partial write (tally without vote row) exists only in the overlay.
    let staged = ctx.governance.get_proposal(1).unwrap().unwrap();
    assert!(
        staged.votes_for > U256::ZERO,
        "tally was written before the fault"
    );
    assert!(overlay
        .get_cf_raw(CF_GOVERNANCE_VOTES, &vote_key(1, &voter))
        .unwrap()
        .is_none());
    assert_eq!(dump(&db), before, "nothing reached the DB");
}

/// A deterministic vote error (no such proposal) keeps today's result.
#[test]
fn vote_on_missing_proposal_does_not_halt() {
    let (_dir, db) = open_test_db();
    let voter = setup_staker(&db);
    let mut ctx = make_ctx(db.clone(), 2);
    let r = NativeExecutor::execute(
        &mut ctx,
        &voter,
        &NativeAction::Vote {
            proposal_id: 9,
            option: VoteOption::Yes,
        },
    );
    assert_err(&r, "vote", "proposal 9 not found");
    assert!(ctx.fatal_error.is_none(), "{:?}", ctx.fatal_error);
}

// ----------------------------------------------------------------------------
// Block tail: fees
// ----------------------------------------------------------------------------

/// `distribute_fees` credits the treasury (epoch 0: no validator share), then
/// fails crediting the dev pool: the result is today's, and the node halts.
#[test]
fn distribute_fees_write_fault_latches_fatal_error() {
    let (_dir, db) = open_test_db();
    let faulty = PoisonedKey::new(db.clone(), CF_ACCOUNTS, addr(101).as_slice());
    let mut ctx = make_ctx(faulty, 2);
    let r = NativeExecutor::distribute_fees(&mut ctx, 1_000_000);
    assert_err(&r, "fee_distribution", INJECTED);
    let fatal = ctx
        .fatal_error
        .as_deref()
        .expect("a storage fault must fail-stop");
    assert!(fatal.contains("fee_distribution"), "{fatal}");
}

/// The same fees with a healthy backend: success, no halt.
#[test]
fn distribute_fees_ok_does_not_halt() {
    let (_dir, db) = open_test_db();
    let mut ctx = make_ctx(db.clone(), 2);
    let r = NativeExecutor::distribute_fees(&mut ctx, 1_000_000);
    assert!(r.success, "{:?}", r.error);
    assert!(ctx.fatal_error.is_none());
}
