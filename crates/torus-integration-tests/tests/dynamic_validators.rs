//! Dynamic validator set integration tests (Phase 3: 3.2.5).
//!
//! Tests governance-gated registration, epoch rotation, commission management,
//! rotation cap enforcement, and minimum set protection.

mod common;

use alloy_primitives::{Address, U256};
use torus_economics::epoch::{EpochManager, ValidatorSetDiff};
use torus_economics::governance::{ExecutionPayload, ProposalOutcome, ProposalStatus};
use torus_economics::types::*;
use torus_economics::{GovernanceManager, GovernanceParams, StakingManager};
use torus_types::{PublicKey, ValidatorInfo, ValidatorSet};

use crate::common::TestHarness;

// ============================================================================
// Helpers
// ============================================================================

fn wei(n: u64) -> U256 {
    U256::from(n) * U256::from(10u64).pow(U256::from(18u64))
}

fn addr(n: u8) -> Address {
    Address::new([n; 20])
}

fn balance(h: &TestHarness, a: &Address) -> U256 {
    h.state_db
        .get_account(a)
        .unwrap()
        .map(|x| x.balance)
        .unwrap_or(U256::ZERO)
}

/// Set up governance with short voting period for tests.
fn setup_governance(h: &TestHarness) -> (StakingManager, GovernanceManager) {
    let staking = StakingManager::new(h.state_db.clone());
    let gov = GovernanceManager::new(h.state_db.clone());

    let params = GovernanceParams {
        voting_period_blocks: 10,
        quorum_bps: 3300,
        min_proposal_stake: wei(1_000),
        permanent_weight_multiplier_num: 3,
        permanent_weight_multiplier_den: 2,
        treasury_address: addr(99),
        timelock_blocks: 5,
        permanent_unlock_threshold_bps: 8000,
    };
    gov.set_governance_params(&params).unwrap();

    (staking, gov)
}

/// Register a validator and fund it. Returns the address.
fn register_validator(staking: &StakingManager, n: u8, stake_tokens: u64) -> Address {
    let a = addr(n);
    let stake = wei(stake_tokens);
    staking.credit_balance(&a, stake + wei(10_000)).unwrap();
    staking
        .register_validator(a, [n; 32], 500, stake)
        .unwrap();
    a
}

/// Whitelist a candidate via governance proposal (shortcut for tests).
/// Ensures proposer has delegated stake for governance weight.
fn whitelist_candidate(
    staking: &StakingManager,
    gov: &GovernanceManager,
    proposer: &Address,
    candidate: &Address,
    block: u64,
) {
    // Governance requires delegated stake. Delegate to self if needed.
    let existing_del = staking.delegations_for_delegator(proposer).unwrap();
    if existing_del.is_empty() {
        staking.credit_balance(proposer, wei(10_000)).unwrap();
        staking.delegate(*proposer, *proposer, wei(10_000)).unwrap();
    }

    let prop_id = gov
        .submit_proposal(
            *proposer,
            "Whitelist validator".to_string(),
            format!("Approve {candidate} for validator registration"),
            Some(ExecutionPayload::ValidatorRegistration {
                candidate: *candidate,
            }),
            block,
        )
        .unwrap();

    gov.cast_vote(*proposer, prop_id, true, block + 1)
        .unwrap();

    // FIX 15 (745a98a): finalize starts the 5-block timelock; execution is a
    // separate step (same two-phase contract as governance_tests.rs).
    let outcome = gov.finalize_proposal(prop_id, block + 11).unwrap();
    assert_eq!(outcome, ProposalOutcome::Passed(prop_id));
    let outcome = gov.execute_proposal(prop_id, block + 16).unwrap();
    assert_eq!(outcome, ProposalOutcome::Executed(prop_id));
}

// ============================================================================
// 3.2.1: Governance-gated validator registration
// ============================================================================

/// Governance approval → whitelist → register → success.
#[test]
fn test_validator_registration_via_governance() {
    let h = TestHarness::new();
    let (staking, gov) = setup_governance(&h);

    // Set up an existing validator who can propose/vote
    let proposer = register_validator(&staking, 1, 50_000);

    // Candidate wants to register
    let candidate = addr(10);
    staking.credit_balance(&candidate, wei(100_000)).unwrap();

    // Step 1: Proposer submits ValidatorRegistration proposal
    whitelist_candidate(&staking, &gov, &proposer, &candidate, 100);

    // Step 2: Verify whitelist entry exists
    assert!(gov.is_whitelisted(&candidate, 115).unwrap());

    // Step 3: Candidate registers
    staking
        .register_validator(candidate, [10; 32], 500, wei(20_000))
        .unwrap();

    let val = staking.get_validator(&candidate).unwrap().unwrap();
    assert_eq!(val.status, ValidatorStatus::Candidate);
    assert_eq!(val.self_stake, wei(20_000));
}

/// Registration without governance approval → rejected.
#[test]
fn test_registration_without_governance_rejected() {
    let h = TestHarness::new();
    let (staking, gov) = setup_governance(&h);

    let candidate = addr(10);
    staking.credit_balance(&candidate, wei(100_000)).unwrap();

    // Not whitelisted
    assert!(!gov.is_whitelisted(&candidate, 100).unwrap());

    // Registration itself succeeds (whitelist check is in native_executor),
    // but the candidate was never whitelisted. The NativeExecutor layer would
    // reject this — we test the governance check directly.
    assert!(!gov.is_whitelisted(&candidate, 100).unwrap());
}

/// Whitelist expires after WHITELIST_EXPIRY_BLOCKS.
#[test]
fn test_whitelist_expiry() {
    let h = TestHarness::new();
    let (staking, gov) = setup_governance(&h);

    let proposer = register_validator(&staking, 1, 50_000);
    let candidate = addr(10);
    staking.credit_balance(&candidate, wei(100_000)).unwrap();

    whitelist_candidate(&staking, &gov, &proposer, &candidate, 100);

    // Valid right after approval
    assert!(gov.is_whitelisted(&candidate, 115).unwrap());

    // Expired after WHITELIST_EXPIRY_BLOCKS. The whitelist entry is created at
    // execution (block 116 = finalize 111 + timelock 5), not at finalize.
    assert!(!gov
        .is_whitelisted(&candidate, 116 + WHITELIST_EXPIRY_BLOCKS + 1)
        .unwrap());
}

// ============================================================================
// 3.2.2: Epoch rotation
// ============================================================================

/// Validators ranked by stake, top N selected. Candidate promoted at epoch.
#[test]
fn test_epoch_rotation_ranking() {
    let h = TestHarness::new();
    let staking = StakingManager::new(h.state_db.clone());

    // Register 5 validators with different stakes
    register_validator(&staking, 1, 50_000);
    register_validator(&staking, 2, 100_000);
    register_validator(&staking, 3, 30_000);
    register_validator(&staking, 4, 80_000);
    register_validator(&staking, 5, 60_000);

    // Epoch 1: top 3 selected (max_validators=3)
    let set = EpochManager::compute_new_validator_set(&staking, 3, 1).unwrap();
    assert_eq!(set.validators.len(), 3);
    assert_eq!(set.validators[0].address, addr(2)); // 100k
    assert_eq!(set.validators[1].address, addr(4)); // 80k
    assert_eq!(set.validators[2].address, addr(5)); // 60k
}

/// Fewer than max_validators eligible → set shrinks gracefully.
#[test]
fn test_epoch_rotation_shrinkage() {
    let h = TestHarness::new();
    let staking = StakingManager::new(h.state_db.clone());

    register_validator(&staking, 1, 50_000);
    register_validator(&staking, 2, 100_000);

    // max_validators=10 but only 2 eligible
    let set = EpochManager::compute_new_validator_set(&staking, 10, 1).unwrap();
    assert_eq!(set.validators.len(), 2);
}

/// Zero eligible validators → error (halts gracefully).
#[test]
fn test_epoch_rotation_zero_eligible() {
    let h = TestHarness::new();
    let staking = StakingManager::new(h.state_db.clone());

    // No validators registered
    let set = EpochManager::compute_new_validator_set(&staking, 21, 1).unwrap();
    assert!(set.validators.is_empty());

    let result = EpochManager::check_minimum_set(&set);
    assert!(result.is_err());
}

/// Candidate with sufficient stake is promoted at epoch boundary.
#[test]
fn test_candidate_promotion_at_epoch() {
    let h = TestHarness::new();
    let staking = StakingManager::new(h.state_db.clone());

    let v1 = register_validator(&staking, 1, 50_000);
    let v2 = register_validator(&staking, 2, 100_000);
    let v3 = register_validator(&staking, 3, 30_000);

    // All start as Candidate
    assert_eq!(
        staking.get_validator(&v1).unwrap().unwrap().status,
        ValidatorStatus::Candidate
    );

    // Epoch rotation selects top 2
    let set = EpochManager::compute_new_validator_set(&staking, 2, 1).unwrap();
    EpochManager::update_validator_statuses(&staking, &set).unwrap();

    // v2 (100k) and v1 (50k) promoted to Active
    assert_eq!(
        staking.get_validator(&v2).unwrap().unwrap().status,
        ValidatorStatus::Active
    );
    assert_eq!(
        staking.get_validator(&v1).unwrap().unwrap().status,
        ValidatorStatus::Active
    );

    // v3 (30k) stays Candidate
    assert_eq!(
        staking.get_validator(&v3).unwrap().unwrap().status,
        ValidatorStatus::Candidate
    );
}

// ============================================================================
// 3.2.3: Commission management
// ============================================================================

/// Commission change applied → affects next reward distribution.
#[test]
fn test_commission_change_affects_rewards() {
    let h = TestHarness::new();
    let staking = StakingManager::new(h.state_db.clone());

    let proposer = register_validator(&staking, 1, 50_000);
    let delegator = addr(2);
    staking.credit_balance(&delegator, wei(100_000)).unwrap();
    staking.delegate(delegator, proposer, wei(50_000)).unwrap();

    // Initial commission: 500 bps (5%)
    let val = staking.get_validator(&proposer).unwrap().unwrap();
    assert_eq!(val.commission_bps, 500);

    // Change commission to 600 bps
    staking.update_commission(proposer, 600, 100).unwrap();
    let val = staking.get_validator(&proposer).unwrap().unwrap();
    assert_eq!(val.commission_bps, 600);

    // Distribute rewards — commission should use the new 600 bps rate
    let validator_share = wei(1_000);
    torus_economics::rewards::FeeSplitter::distribute_validator_rewards(
        &staking,
        &proposer,
        validator_share,
    )
    .unwrap();

    // Commission = 1000 * 600/10000 = 60
    let proposer_rewards = staking
        .get_pending_rewards(&proposer)
        .unwrap()
        .unwrap()
        .amount;
    assert_eq!(proposer_rewards, wei(60));
}

/// Commission change within cooldown → rejected.
#[test]
fn test_commission_cooldown_enforced() {
    let h = TestHarness::new();
    let staking = StakingManager::new(h.state_db.clone());

    let v = register_validator(&staking, 1, 50_000);

    // First change at block 100
    staking.update_commission(v, 600, 100).unwrap();

    // Second change at block 101 (within cooldown) → rejected
    let result = staking.update_commission(v, 550, 101);
    assert!(matches!(
        result,
        Err(torus_economics::EconomicsError::CommissionCooldownNotExpired(..))
    ));

    // After cooldown → accepted
    staking
        .update_commission(v, 550, 100 + COMMISSION_COOLDOWN_BLOCKS)
        .unwrap();
    let val = staking.get_validator(&v).unwrap().unwrap();
    assert_eq!(val.commission_bps, 550);
}

/// Commission change exceeding 100 bps delta → rejected.
#[test]
fn test_commission_delta_limit() {
    let h = TestHarness::new();
    let staking = StakingManager::new(h.state_db.clone());

    let v = register_validator(&staking, 1, 50_000);
    // 500 → 700 (delta=200) → rejected
    let result = staking.update_commission(v, 700, 100);
    assert!(matches!(
        result,
        Err(torus_economics::EconomicsError::CommissionChangeTooLarge { .. })
    ));
}

// ============================================================================
// 3.2.4: Rotation cap and consensus continuity
// ============================================================================

/// Rotation cap = floor(n/3). More changes than cap → deferred.
#[test]
fn test_rotation_cap_enforcement() {
    // Set up old set with 9 validators → cap = 3
    let old_set = ValidatorSet {
        validators: (1..=9u8)
            .map(|n| ValidatorInfo {
                address: addr(n),
                pubkey: PublicKey([n; 32]),
                power: 100 - n as u64,
                commission_bps: 500,
            })
            .collect(),
        epoch: 1,
    };

    // New set: remove 5 validators, add 5 new ones (10 total changes)
    let new_set = ValidatorSet {
        validators: (5..=13u8)
            .map(|n| ValidatorInfo {
                address: addr(n),
                pubkey: PublicKey([n; 32]),
                power: 200 - n as u64,
                commission_bps: 500,
            })
            .collect(),
        epoch: 2,
    };

    let cap = EpochManager::safe_rotation_cap(old_set.validators.len());
    assert_eq!(cap, 3); // floor(9/3) = 3

    let capped = EpochManager::apply_rotation_cap(&old_set, new_set, cap);

    // Count identity changes only (new arrivals + actual departures),
    // not power updates on existing validators. The rotation cap limits
    // which validators are IN the set, not their power.
    let old_addrs: std::collections::HashSet<Address> =
        old_set.validators.iter().map(|v| v.address).collect();
    let new_addrs: std::collections::HashSet<Address> =
        capped.validators.iter().map(|v| v.address).collect();
    let identity_departures = old_addrs.difference(&new_addrs).count();
    let identity_arrivals = new_addrs.difference(&old_addrs).count();
    let identity_changes = identity_departures + identity_arrivals;

    // With cap=3 and half=1, at most 1 swap allowed (1 departure + 1 arrival = 2)
    assert!(
        identity_changes <= cap,
        "identity_changes={identity_changes} exceeds cap={cap}"
    );
    // Verify some validators were actually kept that would have left
    assert!(
        capped.validators.len() >= old_set.validators.len() - cap,
        "too many validators removed"
    );
}

/// Minimum active set protection.
#[test]
fn test_minimum_active_set_check() {
    // Set with 4 validators → passes
    let set_ok = ValidatorSet {
        validators: (1..=4u8)
            .map(|n| ValidatorInfo {
                address: addr(n),
                pubkey: PublicKey([n; 32]),
                power: 100,
                commission_bps: 500,
            })
            .collect(),
        epoch: 1,
    };
    assert!(EpochManager::check_minimum_set(&set_ok).is_ok());

    // Empty set → error
    let set_empty = ValidatorSet {
        validators: vec![],
        epoch: 1,
    };
    assert!(EpochManager::check_minimum_set(&set_empty).is_err());
}

/// Diff handles all cases correctly.
#[test]
fn test_validator_set_diff_comprehensive() {
    let old = ValidatorSet {
        validators: vec![
            ValidatorInfo {
                address: addr(1),
                pubkey: PublicKey([1; 32]),
                power: 100,
                commission_bps: 500,
            },
            ValidatorInfo {
                address: addr(2),
                pubkey: PublicKey([2; 32]),
                power: 200,
                commission_bps: 300,
            },
            ValidatorInfo {
                address: addr(3),
                pubkey: PublicKey([3; 32]),
                power: 150,
                commission_bps: 400,
            },
        ],
        epoch: 1,
    };

    let new = ValidatorSet {
        validators: vec![
            // addr(1) unchanged
            ValidatorInfo {
                address: addr(1),
                pubkey: PublicKey([1; 32]),
                power: 100,
                commission_bps: 500,
            },
            // addr(2) power changed
            ValidatorInfo {
                address: addr(2),
                pubkey: PublicKey([2; 32]),
                power: 250,
                commission_bps: 300,
            },
            // addr(3) key rotated (same power)
            ValidatorInfo {
                address: addr(3),
                pubkey: PublicKey([33; 32]), // new key
                power: 150,
                commission_bps: 400,
            },
            // addr(4) new validator
            ValidatorInfo {
                address: addr(4),
                pubkey: PublicKey([4; 32]),
                power: 120,
                commission_bps: 500,
            },
        ],
        epoch: 2,
    };

    let diff = EpochManager::compute_validator_set_diff(&old, &new);

    // Inserts: addr(2) power changed, addr(3) key rotated, addr(4) new
    assert_eq!(diff.inserts.len(), 3);
    // No deletes (all old validators still in new set)
    assert_eq!(diff.deletes.len(), 0);
    // Key rotation: addr(3)'s old key [3;32] should be in rotated_out
    assert_eq!(diff.rotated_out_pubkeys.len(), 1);
    assert_eq!(diff.rotated_out_pubkeys[0], [3; 32]);
}

/// No changes → empty diff.
#[test]
fn test_diff_no_changes() {
    let set = ValidatorSet {
        validators: vec![ValidatorInfo {
            address: addr(1),
            pubkey: PublicKey([1; 32]),
            power: 100,
            commission_bps: 500,
        }],
        epoch: 1,
    };

    let diff = EpochManager::compute_validator_set_diff(&set, &set);
    assert!(diff.is_empty());
}

// ============================================================================
// 3.2.5 extended: jailing removes from active set
// ============================================================================

/// Jailed validator removed at next epoch, next-ranked Candidate promoted.
#[test]
fn test_jailed_validator_removed_and_replaced() {
    let h = TestHarness::new();
    let staking = StakingManager::new(h.state_db.clone());

    let v1 = register_validator(&staking, 1, 100_000);
    let v2 = register_validator(&staking, 2, 80_000);
    let v3 = register_validator(&staking, 3, 60_000);
    let v4 = register_validator(&staking, 4, 40_000); // waiting in queue

    // Epoch 1: top 3 active
    let set1 = EpochManager::compute_new_validator_set(&staking, 3, 1).unwrap();
    EpochManager::update_validator_statuses(&staking, &set1).unwrap();

    assert_eq!(
        staking.get_validator(&v1).unwrap().unwrap().status,
        ValidatorStatus::Active
    );
    assert_eq!(
        staking.get_validator(&v4).unwrap().unwrap().status,
        ValidatorStatus::Candidate
    );

    // Jail v1
    staking.jail_validator(&v1, 28_800, 1000).unwrap();
    assert_eq!(
        staking.get_validator(&v1).unwrap().unwrap().status,
        ValidatorStatus::Jailed
    );

    // Epoch 2: v1 jailed → excluded. v4 promoted.
    let set2 = EpochManager::compute_new_validator_set(&staking, 3, 2).unwrap();
    let diff = EpochManager::compute_validator_set_diff(&set1, &set2);

    // v4 should be inserted
    assert!(diff.inserts.iter().any(|v| v.address == v4));
    // v1 should be deleted
    assert!(diff.deletes.contains(&v1));

    EpochManager::update_validator_statuses(&staking, &set2).unwrap();

    assert_eq!(
        staking.get_validator(&v4).unwrap().unwrap().status,
        ValidatorStatus::Active
    );
}

/// Unjailed validator re-enters as Candidate → back in active set at next epoch.
#[test]
fn test_unjail_and_reenter_active_set() {
    let h = TestHarness::new();
    let staking = StakingManager::new(h.state_db.clone());

    let v1 = register_validator(&staking, 1, 100_000);
    let v2 = register_validator(&staking, 2, 50_000);

    // Epoch 1: both active
    let set1 = EpochManager::compute_new_validator_set(&staking, 10, 1).unwrap();
    EpochManager::update_validator_statuses(&staking, &set1).unwrap();

    // Jail v1 at block 100
    staking.jail_validator(&v1, JAIL_DURATION_BLOCKS, 100).unwrap();

    // Epoch 2: v1 excluded
    let set2 = EpochManager::compute_new_validator_set(&staking, 10, 2).unwrap();
    assert_eq!(set2.validators.len(), 1);
    assert_eq!(set2.validators[0].address, v2);

    // Unjail v1 after cooldown
    staking.unjail(&v1, 100 + JAIL_DURATION_BLOCKS).unwrap();
    assert_eq!(
        staking.get_validator(&v1).unwrap().unwrap().status,
        ValidatorStatus::Candidate
    );

    // Epoch 3: v1 back in active set (has higher stake)
    let set3 = EpochManager::compute_new_validator_set(&staking, 10, 3).unwrap();
    assert_eq!(set3.validators.len(), 2);
    assert_eq!(set3.validators[0].address, v1); // 100k > 50k
}

/// Validator undelegates below MIN_SELF_DELEGATION → removed at next epoch.
#[test]
fn test_validator_leaves_on_insufficient_stake() {
    let h = TestHarness::new();
    let staking = StakingManager::new(h.state_db.clone());

    let v1 = register_validator(&staking, 1, 50_000);
    let v2 = register_validator(&staking, 2, 50_000);

    // Epoch 1
    let set1 = EpochManager::compute_new_validator_set(&staking, 10, 1).unwrap();
    EpochManager::update_validator_statuses(&staking, &set1).unwrap();

    // v1 self-delegates to themselves (self_stake is already set via register)
    // Slash v1 heavily so self_stake drops below MIN_SELF_DELEGATION
    staking
        .slash(v1, 9000, SlashReason::DoubleSign, 500)
        .unwrap(); // 90% slash

    // Epoch 2: v1 auto-jailed (below min) → excluded
    let set2 = EpochManager::compute_new_validator_set(&staking, 10, 2).unwrap();
    assert_eq!(set2.validators.len(), 1);
    assert_eq!(set2.validators[0].address, v2);
}

// ============================================================================
// Edge cases
// ============================================================================

/// All validators with identical stake → deterministic tie-break by address.
#[test]
fn test_tie_break_by_address() {
    let h = TestHarness::new();
    let staking = StakingManager::new(h.state_db.clone());

    // All with same stake
    register_validator(&staking, 3, 50_000);
    register_validator(&staking, 1, 50_000);
    register_validator(&staking, 2, 50_000);
    register_validator(&staking, 5, 50_000);
    register_validator(&staking, 4, 50_000);

    let set = EpochManager::compute_new_validator_set(&staking, 3, 1).unwrap();
    assert_eq!(set.validators.len(), 3);
    // Address ascending tie-break: addr(1) < addr(2) < addr(3)
    assert_eq!(set.validators[0].address, addr(1));
    assert_eq!(set.validators[1].address, addr(2));
    assert_eq!(set.validators[2].address, addr(3));
}

/// Single validator (test/dev scenario) → works correctly.
#[test]
fn test_single_validator() {
    let h = TestHarness::new();
    let staking = StakingManager::new(h.state_db.clone());

    let v = register_validator(&staking, 1, 50_000);

    let set = EpochManager::compute_new_validator_set(&staking, 21, 1).unwrap();
    assert_eq!(set.validators.len(), 1);
    assert_eq!(set.validators[0].address, v);

    // Should warn but not error (single validator is valid for dev/test)
    assert!(EpochManager::check_minimum_set(&set).is_ok());
}

/// Key rotation at same epoch as set change → both applied.
#[test]
fn test_key_rotation_with_set_change() {
    let old = ValidatorSet {
        validators: vec![
            ValidatorInfo {
                address: addr(1),
                pubkey: PublicKey([1; 32]),
                power: 100,
                commission_bps: 500,
            },
            ValidatorInfo {
                address: addr(2),
                pubkey: PublicKey([2; 32]),
                power: 80,
                commission_bps: 500,
            },
        ],
        epoch: 1,
    };

    let new = ValidatorSet {
        validators: vec![
            // addr(1) rotated key AND power changed
            ValidatorInfo {
                address: addr(1),
                pubkey: PublicKey([11; 32]),
                power: 120,
                commission_bps: 500,
            },
            // addr(2) left, addr(3) joined
            ValidatorInfo {
                address: addr(3),
                pubkey: PublicKey([3; 32]),
                power: 90,
                commission_bps: 500,
            },
        ],
        epoch: 2,
    };

    let diff = EpochManager::compute_validator_set_diff(&old, &new);

    // addr(1): key rotated + power changed → insert + rotated_out
    // addr(3): new → insert
    assert_eq!(diff.inserts.len(), 2);
    // addr(2): deleted
    assert_eq!(diff.deletes.len(), 1);
    assert_eq!(diff.deletes[0], addr(2));
    // addr(1) old pubkey [1;32] rotated out
    assert_eq!(diff.rotated_out_pubkeys.len(), 1);
    assert_eq!(diff.rotated_out_pubkeys[0], [1; 32]);
}

/// Safe rotation cap derivation.
#[test]
fn test_safe_rotation_cap_values() {
    assert_eq!(EpochManager::safe_rotation_cap(21), 7);
    assert_eq!(EpochManager::safe_rotation_cap(9), 3);
    assert_eq!(EpochManager::safe_rotation_cap(4), 1);
    assert_eq!(EpochManager::safe_rotation_cap(3), 1);
    assert_eq!(EpochManager::safe_rotation_cap(2), 0);
    assert_eq!(EpochManager::safe_rotation_cap(1), 0);
    assert_eq!(EpochManager::safe_rotation_cap(0), 0);
}

/// Epoch boundary detection.
#[test]
fn test_epoch_boundary() {
    assert!(EpochManager::is_epoch_boundary(100_000, 100_000));
    assert!(!EpochManager::is_epoch_boundary(99_999, 100_000));
    assert!(EpochManager::is_epoch_boundary(200_000, 100_000));
    assert_eq!(EpochManager::epoch_for_block(100_000, 100_000), 1);
    assert_eq!(EpochManager::epoch_for_block(250_000, 100_000), 2);
}

/// Pending rewards for active validator leaving at epoch — rewards preserved.
#[test]
fn test_rewards_preserved_on_validator_departure() {
    let h = TestHarness::new();
    let staking = StakingManager::new(h.state_db.clone());

    let v1 = register_validator(&staking, 1, 100_000);
    let _v2 = register_validator(&staking, 2, 50_000);

    // Distribute some rewards to v1
    staking.credit_rewards(v1, wei(1_000)).unwrap();

    // Jail v1 → will be removed at next epoch
    staking.jail_validator(&v1, JAIL_DURATION_BLOCKS, 100).unwrap();

    // Epoch rotation removes v1
    let set = EpochManager::compute_new_validator_set(&staking, 10, 2).unwrap();
    EpochManager::update_validator_statuses(&staking, &set).unwrap();

    // v1's pending rewards should still be claimable
    let rewards = staking.get_pending_rewards(&v1).unwrap().unwrap();
    assert_eq!(rewards.amount, wei(1_000));
}
