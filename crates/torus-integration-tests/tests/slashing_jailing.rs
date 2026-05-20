//! Integration tests for slashing, jailing, and unjail (Phase 3: 3.1).
//!
//! Covers:
//! - Full double-sign lifecycle: detect → slash 5% → tombstone → delegators undelegate
//! - Full downtime lifecycle: detect → slash 0.1% → jail → wait → unjail → re-enter
//! - Jail vote lifecycle: vote → threshold → jail → wait → unjail
//! - Epoch transition with jailed validator: active set shrinks, rewards skip jailed

mod common;

use alloy_primitives::{Address, U256};
use revm::state::AccountInfo;

use torus_bridge::native_executor::{NativeExecContext, NativeExecutor};
use torus_consensus::slashing::{DoubleSignDetector, DowntimeTracker};
use torus_economics::types::*;
use torus_economics::{EpochManager, StakingManager};
use torus_state::StateDb;
use torus_types::eip712::TORUS_CHAIN_ID;
use torus_types::NativeAction;

fn setup() -> (tempfile::TempDir, StakingManager) {
    let dir = tempfile::tempdir().unwrap();
    let db = StateDb::open(dir.path()).unwrap();
    (dir, StakingManager::new(db))
}

fn addr(n: u8) -> Address {
    Address::new([n; 20])
}

fn wei(tokens: u64) -> U256 {
    U256::from(tokens) * U256::from(10u64).pow(U256::from(18u64))
}

fn fund(mgr: &StakingManager, a: &Address, amount: U256) {
    let info = AccountInfo {
        balance: amount,
        ..Default::default()
    };
    mgr.state().put_account(a, &info).unwrap();
}

fn register_and_activate(mgr: &StakingManager, v: Address, stake_tokens: u64) {
    fund(mgr, &v, wei(stake_tokens));
    mgr.register_validator(v, [v.0[0]; 32], 500, wei(stake_tokens))
        .unwrap();
    let mut val = mgr.get_validator(&v).unwrap().unwrap();
    val.status = ValidatorStatus::Active;
    mgr.put_validator(&v, &val).unwrap();
}

// ============================================================================
// Test 1: Full double-sign lifecycle
// ============================================================================

#[test]
fn full_double_sign_lifecycle() {
    let (_dir, mgr) = setup();

    let v1 = addr(1);
    let v2 = addr(2);
    let delegator = addr(10);

    // Register validators
    register_and_activate(&mgr, v1, 50_000);
    register_and_activate(&mgr, v2, 50_000);

    // Delegator delegates to v1
    fund(&mgr, &delegator, wei(20_000));
    mgr.delegate(delegator, v1, wei(20_000)).unwrap();

    let val_before = mgr.get_validator(&v1).unwrap().unwrap();
    assert_eq!(val_before.self_stake, wei(50_000));
    assert_eq!(val_before.total_delegated, wei(20_000));

    // --- Double-sign detected ---
    // Slash 5%
    let slashed = mgr
        .slash(v1, DOUBLE_SIGN_SLASH_BPS, SlashReason::DoubleSign, 100)
        .unwrap();
    assert_eq!(slashed, wei(3_500)); // 70_000 * 5% = 3_500

    // Tombstone
    mgr.tombstone_validator(&v1).unwrap();

    let val_after = mgr.get_validator(&v1).unwrap().unwrap();
    assert_eq!(val_after.status, ValidatorStatus::Tombstoned);
    assert_eq!(val_after.self_stake, wei(47_500)); // 50_000 * 0.95
    assert_eq!(val_after.total_delegated, wei(19_000)); // 20_000 * 0.95

    // Delegator can still undelegate from tombstoned validator
    mgr.undelegate(delegator, v1, wei(19_000), 200).unwrap();
    let val_final = mgr.get_validator(&v1).unwrap().unwrap();
    assert_eq!(val_final.total_delegated, U256::ZERO);

    // Cannot unjail tombstoned
    assert!(mgr.unjail(&v1, 999_999).is_err());

    // New delegations rejected
    fund(&mgr, &delegator, wei(1_000));
    assert!(mgr.delegate(delegator, v1, wei(1_000)).is_err());
}

// ============================================================================
// Test 2: Full downtime lifecycle
// ============================================================================

#[test]
fn full_downtime_lifecycle() {
    let (_dir, mgr) = setup();

    let good_val = addr(1);
    let bad_val = addr(2);

    register_and_activate(&mgr, good_val, 50_000);
    register_and_activate(&mgr, bad_val, 50_000);

    // --- Downtime detection ---
    let mut tracker = DowntimeTracker::new(DOWNTIME_WINDOW_BLOCKS);

    // good_val signs all blocks, bad_val signs none
    for h in 1..=100 {
        tracker.record_signer(h, [good_val.0[0]; 32]);
    }

    let down = tracker.detect_downtime(
        &[[good_val.0[0]; 32], [bad_val.0[0]; 32]],
        DOWNTIME_THRESHOLD_PCT,
    );
    assert!(down.contains(&[bad_val.0[0]; 32]));
    assert!(!down.contains(&[good_val.0[0]; 32]));

    // --- Slash for downtime (0.1%) ---
    let slashed = mgr
        .slash(bad_val, DOWNTIME_SLASH_BPS, SlashReason::Downtime, 100)
        .unwrap();
    // 50_000 * 0.001 = 50 tokens
    assert_eq!(slashed, wei(50));

    // Jail with cooldown
    mgr.jail_validator(&bad_val, JAIL_DURATION_BLOCKS, 100)
        .unwrap();

    let val = mgr.get_validator(&bad_val).unwrap().unwrap();
    assert_eq!(val.status, ValidatorStatus::Jailed);
    assert_eq!(val.jailed_until, Some(100 + JAIL_DURATION_BLOCKS));

    // Cannot unjail before cooldown
    assert!(mgr.unjail(&bad_val, 100 + JAIL_DURATION_BLOCKS - 1).is_err());

    // Unjail after cooldown
    let unjail_block = 100 + JAIL_DURATION_BLOCKS;
    mgr.unjail(&bad_val, unjail_block).unwrap();

    let val = mgr.get_validator(&bad_val).unwrap().unwrap();
    assert_eq!(val.status, ValidatorStatus::Candidate);
    assert_eq!(val.jailed_until, None);
}

// ============================================================================
// Test 3: Jail vote lifecycle
// ============================================================================

#[test]
fn jail_vote_lifecycle() {
    let (_dir, mgr) = setup();

    let v1 = addr(1);
    let v2 = addr(2);
    let v3 = addr(3);
    let target = addr(4);

    // 4 validators with equal 25k stake each
    for v in [v1, v2, v3, target] {
        register_and_activate(&mgr, v, 25_000);
    }

    // --- Jail vote via NativeAction ---
    let state_db = mgr.state().clone();
    let mut ctx = NativeExecContext::new(
        state_db, 100, 1_700_000_100, 0, 100, 100,
        addr(99), addr(98), addr(97),
    );

    // v1 votes
    let r1 = NativeExecutor::execute(
        &mut ctx,
        &v1,
        &NativeAction::JailVote { target },
    );
    assert!(r1.success);

    // v2 votes
    let r2 = NativeExecutor::execute(
        &mut ctx,
        &v2,
        &NativeAction::JailVote { target },
    );
    assert!(r2.success);

    // v3 votes — threshold reached (75% > 66.7%)
    let r3 = NativeExecutor::execute(
        &mut ctx,
        &v3,
        &NativeAction::JailVote { target },
    );
    assert!(r3.success);

    // Verify target is jailed
    let val = ctx.staking.get_validator(&target).unwrap().unwrap();
    assert_eq!(val.status, ValidatorStatus::Jailed);

    // --- Unjail via NativeAction ---
    // Advance past cooldown
    ctx.block_height = 100 + JAIL_DURATION_BLOCKS;
    let r_unjail = NativeExecutor::execute(
        &mut ctx,
        &target,
        &NativeAction::UnjailSelf,
    );
    assert!(r_unjail.success);

    let val = ctx.staking.get_validator(&target).unwrap().unwrap();
    assert_eq!(val.status, ValidatorStatus::Candidate);
}

// ============================================================================
// Test 4: Epoch transition with jailed validator
// ============================================================================

#[test]
fn epoch_transition_excludes_jailed_validators() {
    let (_dir, mgr) = setup();

    let v1 = addr(1);
    let v2 = addr(2);
    let v3 = addr(3);

    // Register 3 validators
    for v in [v1, v2, v3] {
        register_and_activate(&mgr, v, 50_000);
    }

    // Jail v2
    mgr.jail_validator(&v2, JAIL_DURATION_BLOCKS, 100).unwrap();

    // Compute new validator set — should exclude v2
    let set = EpochManager::compute_new_validator_set(&mgr, 10, 1).unwrap();

    // Only v1 and v3 should be in the set (Jailed excluded)
    assert_eq!(set.validators.len(), 2);
    let addrs: Vec<Address> = set.validators.iter().map(|v| v.address).collect();
    assert!(addrs.contains(&v1));
    assert!(addrs.contains(&v3));
    assert!(!addrs.contains(&v2));

    // Update statuses — v1 and v3 become Active, v2 stays Jailed
    EpochManager::update_validator_statuses(&mgr, &set).unwrap();

    let val1 = mgr.get_validator(&v1).unwrap().unwrap();
    let val2 = mgr.get_validator(&v2).unwrap().unwrap();
    let val3 = mgr.get_validator(&v3).unwrap().unwrap();
    assert_eq!(val1.status, ValidatorStatus::Active);
    assert_eq!(val2.status, ValidatorStatus::Jailed); // unchanged
    assert_eq!(val3.status, ValidatorStatus::Active);

    // Unjail v2 — becomes Candidate, not Active
    let unjail_block = 100 + JAIL_DURATION_BLOCKS;
    mgr.unjail(&v2, unjail_block).unwrap();
    let val2 = mgr.get_validator(&v2).unwrap().unwrap();
    assert_eq!(val2.status, ValidatorStatus::Candidate);

    // Next epoch — v2 re-enters active set as Candidate
    let set2 = EpochManager::compute_new_validator_set(&mgr, 10, 2).unwrap();
    assert_eq!(set2.validators.len(), 3);
    EpochManager::update_validator_statuses(&mgr, &set2).unwrap();
    let val2 = mgr.get_validator(&v2).unwrap().unwrap();
    assert_eq!(val2.status, ValidatorStatus::Active);
}

// ============================================================================
// Test 5: Double-sign detection with DoubleSignDetector
// ============================================================================

#[test]
fn double_sign_detection_produces_verifiable_evidence() {
    use ed25519_dalek::{Signer, SigningKey};
    use torus_consensus::slashing::ObservedPhaseVote;

    let sk = SigningKey::from_bytes(&[42u8; 32]);
    let chain_id = TORUS_CHAIN_ID;

    let mut detector = DoubleSignDetector::new(chain_id, EVIDENCE_WINDOW_VIEWS);

    // Build two conflicting votes at same view
    let mk_vote = |block_hash: [u8; 32]| {
        let mut msg = Vec::with_capacity(49);
        msg.extend_from_slice(&chain_id.to_be_bytes());
        msg.extend_from_slice(&50u64.to_be_bytes());
        msg.extend_from_slice(&block_hash);
        msg.push(0);
        let sig = sk.sign(&msg);
        ObservedPhaseVote {
            chain_id,
            view: 50,
            block_hash,
            phase: 0,
            signer: sk.verifying_key().to_bytes(),
            signature: sig.to_bytes(),
        }
    };

    let vote_a = mk_vote([0xAA; 32]);
    let vote_b = mk_vote([0xBB; 32]);

    assert!(detector.record_vote(&vote_a).is_none());

    let evidence = detector.record_vote(&vote_b).expect("should detect double sign");
    assert!(evidence.verify(), "evidence must be independently verifiable");
    assert_eq!(evidence.view, 50);
    assert_ne!(evidence.vote_a.block_hash, evidence.vote_b.block_hash);
}
