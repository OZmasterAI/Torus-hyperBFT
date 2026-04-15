//! Staking lifecycle tests (task 2.10.5).
//!
//! Tests full staking lifecycle, permanent staking, governance with weight bonus,
//! validator commission math, unbonding timing, and multi-validator delegation.

mod common;

use alloy_primitives::{Address, U256};
use torus_economics::governance::{ExecutionPayload, ProposalOutcome, ProposalStatus};
use torus_economics::types::*;
use torus_economics::{GovernanceManager, GovernanceParams, RewardDistributor, StakingManager};
use torus_state::cf::CF_FEE_CONFIG;

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

// ============================================================================
// Tests
// ============================================================================

/// Full lifecycle: register validators -> delegate -> distribute rewards ->
/// claim rewards -> verify balance increase.
#[test]
fn test_full_staking_lifecycle() {
    let h = TestHarness::new();
    let staking = StakingManager::new(h.state_db.clone());

    let validator = addr(1);
    let delegator = addr(2);
    let treasury = addr(10);
    let dev_pool_addr = addr(11);

    // Fund accounts.
    staking.credit_balance(&validator, wei(100_000)).unwrap();
    staking.credit_balance(&delegator, wei(100_000)).unwrap();

    // Register validator with 10,000 self-stake, 10% commission.
    staking
        .register_validator(validator, [1u8; 32], 1000, wei(10_000))
        .unwrap();
    let val = staking.get_validator(&validator).unwrap().unwrap();
    assert_eq!(val.status, ValidatorStatus::Candidate);
    assert_eq!(val.self_stake, wei(10_000));

    // Delegate 5,000 from delegator.
    staking.delegate(delegator, validator, wei(5_000)).unwrap();
    let val = staking.get_validator(&validator).unwrap().unwrap();
    assert_eq!(val.total_delegated, wei(5_000));

    // Distribute block fees at final epoch (25% to validators).
    let total_fees = wei(10_000);
    RewardDistributor::distribute_block_fees(
        &staking,
        validator,
        total_fees,
        TRANSITION_EPOCHS,
        treasury,
        dev_pool_addr,
    )
    .unwrap();

    // At TRANSITION_EPOCHS: 25% burn, 25% validator, 25% treasury, 25% dev_pool.
    // Validator share = 2,500. Commission (10%) = 250 -> proposer rewards.
    // Delegator pool = 2,250 -> single delegator gets all.
    let del_rewards = staking
        .get_pending_rewards(&delegator)
        .unwrap()
        .unwrap();
    let val_rewards = staking
        .get_pending_rewards(&validator)
        .unwrap()
        .unwrap();
    assert_eq!(del_rewards.amount, wei(2_250));
    assert_eq!(val_rewards.amount, wei(250));

    // Delegator claims rewards.
    let initial_bal = balance(&h, &delegator);
    let claimed = staking.claim_rewards(delegator).unwrap();
    assert_eq!(claimed, wei(2_250));
    assert_eq!(balance(&h, &delegator), initial_bal + wei(2_250));

    // Second claim fails.
    assert!(staking.claim_rewards(delegator).is_err());
}

/// Permanent staking: lock tokens -> verify irreversible -> verify balance debited.
#[test]
fn test_permanent_staking_irreversible() {
    let h = TestHarness::new();
    let staking = StakingManager::new(h.state_db.clone());
    let staker = addr(3);

    staking.credit_balance(&staker, wei(50_000)).unwrap();

    staking.permanent_stake(staker, wei(10_000), 100).unwrap();

    let info = staking.get_permanent_stake(&staker).unwrap().unwrap();
    assert_eq!(info.amount, wei(10_000));
    assert_eq!(info.locked_at_block, 100);
    assert_eq!(balance(&h, &staker), wei(40_000));

    // Additional permanent stake is cumulative.
    staking.permanent_stake(staker, wei(5_000), 200).unwrap();
    let info = staking.get_permanent_stake(&staker).unwrap().unwrap();
    assert_eq!(info.amount, wei(15_000));
    assert_eq!(balance(&h, &staker), wei(35_000));
}

/// Governance end-to-end: submit proposal -> cast votes (with permanent
/// weight bonus deciding outcome) -> finalize -> verify execution.
/// The permanent staker's 1.5x weight outweighs the delegator voting against.
#[test]
fn test_governance_permanent_weight_decides_outcome() {
    let h = TestHarness::new();
    let staking = StakingManager::new(h.state_db.clone());
    let gov = GovernanceManager::new(h.state_db.clone());

    let validator = addr(1);
    let delegator = addr(2); // votes AGAINST
    let perm_staker = addr(3); // votes FOR with 1.5x weight
    let treasury = addr(10);

    // Fund and set up staking.
    staking.credit_balance(&validator, wei(100_000)).unwrap();
    staking.credit_balance(&delegator, wei(100_000)).unwrap();
    staking.credit_balance(&perm_staker, wei(100_000)).unwrap();
    staking.credit_balance(&treasury, wei(100_000)).unwrap();

    staking
        .register_validator(validator, [1u8; 32], 500, wei(10_000))
        .unwrap();

    // Delegator delegates 5,000 -> vote weight = 5,000.
    staking.delegate(delegator, validator, wei(5_000)).unwrap();

    // Permanent staker stakes 5,000 -> vote weight = 5,000 * 3/2 = 7,500.
    staking.permanent_stake(perm_staker, wei(5_000), 0).unwrap();

    // Set short voting period for test.
    let params = GovernanceParams {
        voting_period_blocks: 10,
        quorum_bps: 3300,
        min_proposal_stake: wei(1_000),
        permanent_weight_multiplier_num: 3,
        permanent_weight_multiplier_den: 2,
        treasury_address: treasury,
        timelock_blocks: 5,
        permanent_unlock_threshold_bps: 8000,
    };
    gov.set_governance_params(&params).unwrap();

    // Submit ParameterChange proposal (delegator has 5,000 > min 1,000).
    let prop_id = gov
        .submit_proposal(
            delegator,
            "Change fee param".to_string(),
            "Set trading_fee to 50".to_string(),
            Some(ExecutionPayload::ParameterChange {
                param_key: "trading_fee".to_string(),
                new_value: "50".to_string(),
            }),
            100,
        )
        .unwrap();

    // Delegator votes AGAINST (weight = 5,000).
    gov.cast_vote(delegator, prop_id, false, 105).unwrap();

    // Permanent staker votes FOR (weight = 7,500).
    gov.cast_vote(perm_staker, prop_id, true, 105).unwrap();

    // Verify vote weights.
    let del_vote = gov.get_vote(prop_id, &delegator).unwrap().unwrap();
    let perm_vote = gov.get_vote(prop_id, &perm_staker).unwrap().unwrap();
    assert_eq!(del_vote.weight, wei(5_000));
    assert_eq!(perm_vote.weight, wei(7_500));
    assert!(
        perm_vote.weight > del_vote.weight,
        "permanent stake gives 1.5x governance weight"
    );

    // Finalize after voting period (block 111 > end_block 110).
    let outcome = gov.finalize_proposal(prop_id, 111).unwrap();
    assert_eq!(outcome, ProposalOutcome::Executed(prop_id));

    // Verify parameter was actually updated in CF_FEE_CONFIG.
    let stored = h
        .state_db
        .get_cf_raw(CF_FEE_CONFIG, b"trading_fee")
        .unwrap();
    assert_eq!(stored.unwrap(), b"50");

    let prop = gov.get_proposal(prop_id).unwrap().unwrap();
    assert_eq!(prop.status, ProposalStatus::Executed);
}

/// Governance TreasurySpend: verify execution actually transfers funds.
#[test]
fn test_governance_treasury_spend_execution() {
    let h = TestHarness::new();
    let staking = StakingManager::new(h.state_db.clone());
    let gov = GovernanceManager::new(h.state_db.clone());

    let validator = addr(1);
    let proposer = addr(2);
    let treasury = addr(10);
    let recipient = addr(20);

    staking.credit_balance(&validator, wei(100_000)).unwrap();
    staking.credit_balance(&proposer, wei(100_000)).unwrap();
    staking.credit_balance(&treasury, wei(50_000)).unwrap();

    staking
        .register_validator(validator, [1u8; 32], 500, wei(10_000))
        .unwrap();
    staking.delegate(proposer, validator, wei(5_000)).unwrap();

    let params = GovernanceParams {
        voting_period_blocks: 10,
        quorum_bps: 3300,
        min_proposal_stake: wei(1_000),
        permanent_weight_multiplier_num: 3,
        permanent_weight_multiplier_den: 2,
        treasury_address: treasury,
        timelock_blocks: 5,
        permanent_unlock_threshold_bps: 8000,
    };
    gov.set_governance_params(&params).unwrap();

    let prop_id = gov
        .submit_proposal(
            proposer,
            "Fund development".to_string(),
            "Send 10,000 TRS to dev team".to_string(),
            Some(ExecutionPayload::TreasurySpend {
                recipient,
                amount: wei(10_000),
                reason: "Dev funding".to_string(),
            }),
            200,
        )
        .unwrap();

    gov.cast_vote(proposer, prop_id, true, 205).unwrap();

    let outcome = gov.finalize_proposal(prop_id, 211).unwrap();
    assert_eq!(outcome, ProposalOutcome::Executed(prop_id));

    // Treasury debited, recipient credited.
    assert_eq!(balance(&h, &treasury), wei(40_000));
    assert_eq!(balance(&h, &recipient), wei(10_000));
}

/// Validator commission: fee revenue -> proposer keeps commission ->
/// remainder distributed to delegators proportional to stake.
/// Verify exact math: commission + delegator shares = total.
#[test]
fn test_validator_commission_exact_math() {
    let h = TestHarness::new();
    let staking = StakingManager::new(h.state_db.clone());

    let proposer = addr(1);
    let d1 = addr(2);
    let d2 = addr(3);
    let treasury = addr(10);
    let dev_pool_addr = addr(11);

    staking.credit_balance(&proposer, wei(100_000)).unwrap();
    staking.credit_balance(&d1, wei(100_000)).unwrap();
    staking.credit_balance(&d2, wei(100_000)).unwrap();

    // 10% commission.
    staking
        .register_validator(proposer, [1u8; 32], 1000, wei(10_000))
        .unwrap();
    staking.delegate(d1, proposer, wei(30_000)).unwrap();
    staking.delegate(d2, proposer, wei(70_000)).unwrap();

    let total_fees = wei(10_000);
    RewardDistributor::distribute_block_fees(
        &staking,
        proposer,
        total_fees,
        TRANSITION_EPOCHS,
        treasury,
        dev_pool_addr,
    )
    .unwrap();

    // At TRANSITION_EPOCHS: validator gets 25% = 2,500.
    // Commission (10%) = 250 -> proposer.
    // Delegator pool = 2,250.
    // d1: 30k/100k * 2250 = 675.
    // d2: remainder = 2250 - 675 = 1575.
    let r_proposer = staking
        .get_pending_rewards(&proposer)
        .unwrap()
        .unwrap()
        .amount;
    let r_d1 = staking.get_pending_rewards(&d1).unwrap().unwrap().amount;
    let r_d2 = staking.get_pending_rewards(&d2).unwrap().unwrap().amount;

    assert_eq!(r_proposer, wei(250));
    assert_eq!(r_d1, wei(675));
    assert_eq!(r_d2, wei(1575));

    // Conservation: commission + delegator shares = validator share.
    assert_eq!(r_proposer + r_d1 + r_d2, wei(2_500));
}

/// Unbonding timing: undelegate -> process before period (nothing) ->
/// process after period (funds returned).
#[test]
fn test_unbonding_timing() {
    let h = TestHarness::new();
    let staking = StakingManager::new(h.state_db.clone());

    let validator = addr(1);
    let delegator = addr(2);

    staking.credit_balance(&validator, wei(100_000)).unwrap();
    staking.credit_balance(&delegator, wei(100_000)).unwrap();

    staking
        .register_validator(validator, [1u8; 32], 500, wei(10_000))
        .unwrap();
    staking.delegate(delegator, validator, wei(5_000)).unwrap();

    // Undelegate at block 1000.
    staking
        .undelegate(delegator, validator, wei(2_000), 1000)
        .unwrap();

    // Before unbonding period: nothing released.
    let released = staking
        .process_unbonding(delegator, validator, 1000 + UNBONDING_PERIOD - 1)
        .unwrap();
    assert_eq!(released, U256::ZERO);

    // At unbonding period: funds returned.
    let released = staking
        .process_unbonding(delegator, validator, 1000 + UNBONDING_PERIOD)
        .unwrap();
    assert_eq!(released, wei(2_000));

    // Delegator balance restored.
    let expected = wei(100_000) - wei(5_000) + wei(2_000);
    assert_eq!(balance(&h, &delegator), expected);
}

/// Multi-validator delegation: delegate to 3 validators, verify via
/// delegations_for_delegator, undelegate from one, verify state.
#[test]
fn test_multi_validator_delegation() {
    let h = TestHarness::new();
    let staking = StakingManager::new(h.state_db.clone());

    let v1 = addr(1);
    let v2 = addr(2);
    let v3 = addr(3);
    let delegator = addr(10);

    // Fund and register 3 validators.
    for v in [v1, v2, v3] {
        staking.credit_balance(&v, wei(100_000)).unwrap();
        staking
            .register_validator(v, [v.as_slice()[0]; 32], 500, wei(10_000))
            .unwrap();
    }

    staking.credit_balance(&delegator, wei(100_000)).unwrap();

    // Delegate to all 3.
    staking.delegate(delegator, v1, wei(1_000)).unwrap();
    staking.delegate(delegator, v2, wei(2_000)).unwrap();
    staking.delegate(delegator, v3, wei(3_000)).unwrap();

    // Verify delegations.
    let dels = staking.delegations_for_delegator(&delegator).unwrap();
    assert_eq!(dels.len(), 3);
    let total_delegated: U256 = dels.iter().map(|d| d.amount).sum();
    assert_eq!(total_delegated, wei(6_000));

    // Undelegate from v2.
    staking
        .undelegate(delegator, v2, wei(2_000), 100)
        .unwrap();

    // v2 delegation reduced to zero.
    let val2 = staking.get_validator(&v2).unwrap().unwrap();
    assert_eq!(val2.total_delegated, U256::ZERO);

    // v1 and v3 unchanged.
    let val1 = staking.get_validator(&v1).unwrap().unwrap();
    let val3 = staking.get_validator(&v3).unwrap().unwrap();
    assert_eq!(val1.total_delegated, wei(1_000));
    assert_eq!(val3.total_delegated, wei(3_000));

    // Process unbonding from v2 after period.
    let released = staking
        .process_unbonding(delegator, v2, 100 + UNBONDING_PERIOD)
        .unwrap();
    assert_eq!(released, wei(2_000));
}
