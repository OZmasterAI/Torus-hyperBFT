//! s515 review F4: `claim_unbonded` must be all-or-nothing.
//!
//! It releases matured entries across every delegation of the sender. Pre-fix
//! it called `process_unbonding` per validator, each writing its delegation
//! row and crediting the balance — so a failure on the SECOND delegation left
//! the first release written while the action reported an error.

use alloy_primitives::{Address, U256};
use revm::state::AccountInfo;
use torus_economics::{Delegation, StakingManager, UnbondingEntry};
use torus_state::cf::{CF_ACCOUNTS, CF_STAKING_DELEGATIONS};
use torus_state::error::StateError;
use torus_state::{AtomicWriteOp, StateBackend, StateDb};

/// `StateDb` that fails every write touching one poisoned delegation row —
/// atomically: an `atomic_write` containing it applies nothing.
#[derive(Clone)]
struct PoisonedRow {
    inner: StateDb,
    poison: Vec<u8>,
}

impl PoisonedRow {
    fn hit(&self, cf: &str, key: &[u8]) -> bool {
        cf == CF_STAKING_DELEGATIONS && key == self.poison.as_slice()
    }
    fn fail() -> StateError {
        StateError::InvalidData("injected write failure".to_string())
    }
}

impl StateBackend for PoisonedRow {
    fn get_cf_raw(&self, cf: &str, key: &[u8]) -> Result<Option<Vec<u8>>, StateError> {
        StateBackend::get_cf_raw(&self.inner, cf, key)
    }
    fn put_cf_raw(&self, cf: &str, key: &[u8], value: &[u8]) -> Result<(), StateError> {
        if self.hit(cf, key) {
            return Err(Self::fail());
        }
        StateBackend::put_cf_raw(&self.inner, cf, key, value)
    }
    fn delete_cf_raw(&self, cf: &str, key: &[u8]) -> Result<(), StateError> {
        if self.hit(cf, key) {
            return Err(Self::fail());
        }
        StateBackend::delete_cf_raw(&self.inner, cf, key)
    }
    fn iterate_cf(
        &self,
        cf: &str,
        prefix: Option<&[u8]>,
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>, StateError> {
        StateBackend::iterate_cf(&self.inner, cf, prefix)
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
        StateBackend::atomic_write(&self.inner, ops)
    }
}

fn addr(n: u8) -> Address {
    Address::new([n; 20])
}

fn key(delegator: &Address, validator: &Address) -> Vec<u8> {
    [delegator.as_slice(), validator.as_slice()].concat()
}

fn put_delegation(db: &StateDb, d: &Delegation) {
    db.put_cf_raw(
        CF_STAKING_DELEGATIONS,
        &key(&d.delegator, &d.validator),
        &borsh::to_vec(d).unwrap(),
    )
    .unwrap();
}

fn dump(db: &StateDb) -> Vec<(Vec<u8>, Vec<u8>)> {
    let mut v = StateBackend::iterate_cf(db, CF_STAKING_DELEGATIONS, None).unwrap();
    v.extend(StateBackend::iterate_cf(db, CF_ACCOUNTS, None).unwrap());
    v
}

/// Delegator with matured unbonding at two validators (keys sort v1 < v2).
fn setup() -> (tempfile::TempDir, StateDb, Address, Address, Address) {
    let dir = tempfile::tempdir().unwrap();
    let db = StateDb::open(dir.path()).unwrap();
    let (delegator, v1, v2) = (addr(1), addr(10), addr(20));
    db.put_account(
        &delegator,
        &AccountInfo {
            balance: U256::from(5u64),
            ..Default::default()
        },
    )
    .unwrap();
    for (v, amount) in [(v1, 100u64), (v2, 200u64)] {
        put_delegation(
            &db,
            &Delegation {
                delegator,
                validator: v,
                amount: U256::from(7u64),
                unbonding: vec![
                    UnbondingEntry {
                        amount: U256::from(amount),
                        release_block: 10,
                    },
                    UnbondingEntry {
                        amount: U256::from(1u64),
                        release_block: 1_000,
                    },
                ],
            },
        );
    }
    (dir, db, delegator, v1, v2)
}

#[test]
fn claim_unbonded_failure_on_second_delegation_changes_nothing() {
    let (_dir, db, delegator, _v1, v2) = setup();
    let before = dump(&db);
    let mgr = StakingManager::new(PoisonedRow {
        inner: db.clone(),
        poison: key(&delegator, &v2),
    });
    assert!(mgr.claim_unbonded(delegator, 50).is_err(), "injected failure must surface");
    assert_eq!(dump(&db), before, "a failed claim must leave no partial release behind");
}

#[test]
fn claim_unbonded_releases_every_matured_entry() {
    let (_dir, db, delegator, v1, v2) = setup();
    let mgr = StakingManager::new(db.clone());
    assert_eq!(mgr.claim_unbonded(delegator, 50).unwrap(), U256::from(300u64));
    assert_eq!(db.get_account(&delegator).unwrap().unwrap().balance, U256::from(305u64));
    let ds = mgr.delegations_for_delegator(&delegator).unwrap();
    assert_eq!(ds.iter().map(|d| d.validator).collect::<Vec<_>>(), vec![v1, v2]);
    for d in ds {
        assert_eq!(d.amount, U256::from(7u64));
        assert_eq!(d.unbonding.len(), 1, "the unmatured entry stays");
        assert_eq!(d.unbonding[0].release_block, 1_000);
    }
    assert!(mgr.claim_unbonded(delegator, 50).is_err(), "nothing left matured");
}

/// Row 74 review: an overflow in `claim_unbonded` is the same on every node,
/// so it must NOT be a local fault (that would halt the whole chain). It used
/// to be `State(InvalidData)`. The message text stays the same.
fn assert_overflow_not_local(err: torus_economics::EconomicsError) {
    assert!(
        matches!(err, torus_economics::EconomicsError::Overflow(_)),
        "want Overflow, got {err:?}"
    );
    assert!(!err.is_local_fault(), "an overflow is deterministic, not a local fault");
    assert_eq!(
        err.to_string(),
        "state error: invalid data: claim_unbonded: released amount overflows"
    );
}

#[test]
fn claim_unbonded_sum_overflow_is_not_a_local_fault() {
    let (_dir, db, delegator, v1, _v2) = setup();
    put_delegation(
        &db,
        &Delegation {
            delegator,
            validator: v1,
            amount: U256::ZERO,
            unbonding: vec![UnbondingEntry {
                amount: U256::MAX,
                release_block: 10,
            }],
        },
    );
    let before = dump(&db);
    let mgr = StakingManager::new(db.clone());
    assert_overflow_not_local(mgr.claim_unbonded(delegator, 50).unwrap_err());
    assert_eq!(dump(&db), before, "an overflow must change nothing");
}

#[test]
fn claim_unbonded_balance_overflow_is_not_a_local_fault() {
    let (_dir, db, delegator, _v1, _v2) = setup();
    db.put_account(
        &delegator,
        &AccountInfo {
            balance: U256::MAX,
            ..Default::default()
        },
    )
    .unwrap();
    let before = dump(&db);
    let mgr = StakingManager::new(db.clone());
    assert_overflow_not_local(mgr.claim_unbonded(delegator, 50).unwrap_err());
    assert_eq!(dump(&db), before, "an overflow must change nothing");
}
