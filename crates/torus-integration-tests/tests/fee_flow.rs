//! Fee flow tests (task 2.10.6).
//!
//! Tests the full fee pipeline: FeeSplitter conservation, BPS interpolation,
//! dev pool pro-rata distribution, validator commission math, and zero-fee handling.

mod common;

use alloy_primitives::{Address, U256};
use torus_economics::types::*;
use torus_economics::{lerp_bps, DevPool, FeeSplitter, RewardDistributor, StakingManager};

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

/// Full pipeline: execute block with fees -> FeeSplitter -> verify
/// burn + validators + treasury + dev_pool == total EXACTLY (conservation).
#[test]
fn test_full_fee_pipeline_conservation() {
    let h = TestHarness::new();
    let staking = StakingManager::new(h.state_db.clone());

    let proposer = addr(1);
    let d1 = addr(2);
    let treasury = addr(10);
    let dev_pool_addr = addr(11);

    staking.credit_balance(&proposer, wei(100_000)).unwrap();
    staking.credit_balance(&d1, wei(100_000)).unwrap();

    staking
        .register_validator(proposer, [1u8; 32], 1000, wei(10_000))
        .unwrap();
    staking.delegate(d1, proposer, wei(20_000)).unwrap();

    let total_fees = wei(10_000);
    let epoch = TRANSITION_EPOCHS; // 25% each bucket

    // Split fees.
    let split = FeeSplitter::split_fees(total_fees, epoch);

    // Conservation: all four buckets sum to total.
    assert_eq!(
        split.burn + split.validators + split.treasury + split.dev_pool,
        total_fees,
        "fee split must conserve total"
    );

    // Execute each part.
    FeeSplitter::execute_burn(&staking, split.burn).unwrap();
    FeeSplitter::distribute_validator_rewards(&staking, &proposer, split.validators).unwrap();
    FeeSplitter::credit_treasury(&staking, &treasury, split.treasury).unwrap();
    staking.credit_balance(&dev_pool_addr, split.dev_pool).unwrap();

    // Verify: burn recorded in supply tracker.
    let tracker = FeeSplitter::get_supply_tracker(&staking).unwrap();
    assert_eq!(tracker.cumulative_burned, split.burn);

    // Verify: treasury credited.
    assert_eq!(balance(&h, &treasury), split.treasury);

    // Verify: dev_pool credited.
    assert_eq!(balance(&h, &dev_pool_addr), split.dev_pool);

    // Verify: validator rewards = commission + delegator shares.
    let r_proposer = staking
        .get_pending_rewards(&proposer)
        .unwrap()
        .unwrap()
        .amount;
    let r_d1 = staking.get_pending_rewards(&d1).unwrap().unwrap().amount;
    assert_eq!(r_proposer + r_d1, split.validators);
}

/// Fee ratio interpolation: verify correct ratios at epoch 0 (initial),
/// epoch 912 (midpoint), epoch 1825 (final). Verify linear interpolation
/// is monotonic and exact.
#[test]
fn test_fee_ratio_interpolation() {
    // Epoch 0: initial ratios.
    let burn_0 = lerp_bps(FEE_START_BURN_BPS, FEE_END_BURN_BPS, 0, TRANSITION_EPOCHS);
    let val_0 = lerp_bps(
        FEE_START_VALIDATOR_BPS,
        FEE_END_VALIDATOR_BPS,
        0,
        TRANSITION_EPOCHS,
    );
    let trs_0 = lerp_bps(
        FEE_START_TREASURY_BPS,
        FEE_END_TREASURY_BPS,
        0,
        TRANSITION_EPOCHS,
    );
    assert_eq!(burn_0, 1000);
    assert_eq!(val_0, 0);
    assert_eq!(trs_0, 4500);
    // dev_pool implied = 10000 - 1000 - 0 - 4500 = 4500.
    assert_eq!(
        10000u32 - burn_0 as u32 - val_0 as u32 - trs_0 as u32,
        4500
    );

    // Verify FeeSplitter at epoch 0 conserves.
    let total = wei(10_000);
    let split_0 = FeeSplitter::split_fees(total, 0);
    assert_eq!(
        split_0.burn + split_0.validators + split_0.treasury + split_0.dev_pool,
        total
    );

    // Epoch 912 (midpoint): linear interpolation.
    let mid = TRANSITION_EPOCHS / 2; // 912
    let burn_mid = lerp_bps(FEE_START_BURN_BPS, FEE_END_BURN_BPS, mid, TRANSITION_EPOCHS);
    let val_mid = lerp_bps(
        FEE_START_VALIDATOR_BPS,
        FEE_END_VALIDATOR_BPS,
        mid,
        TRANSITION_EPOCHS,
    );
    let trs_mid = lerp_bps(
        FEE_START_TREASURY_BPS,
        FEE_END_TREASURY_BPS,
        mid,
        TRANSITION_EPOCHS,
    );

    // Monotonicity: burn and validator increase, treasury decreases.
    assert!(burn_mid > burn_0, "burn should increase toward midpoint");
    assert!(val_mid > val_0, "validator should increase toward midpoint");
    assert!(trs_mid < trs_0, "treasury should decrease toward midpoint");

    // Exact midpoint math:
    // burn_mid = 1000 + (1500 * 912 / 1825) = 1000 + 749 = 1749
    assert_eq!(burn_mid, 1749);
    // val_mid = 0 + (2500 * 912 / 1825) = 1249
    assert_eq!(val_mid, 1249);
    // trs_mid = 4500 - (2000 * 912 / 1825) = 4500 - 999 = 3501
    assert_eq!(trs_mid, 3501);

    // Sum of all BPS = 10000 at midpoint.
    let dev_mid = 10000u32 - burn_mid as u32 - val_mid as u32 - trs_mid as u32;
    assert_eq!(
        burn_mid as u32 + val_mid as u32 + trs_mid as u32 + dev_mid,
        10000
    );

    // Verify FeeSplitter at midpoint conserves.
    let split_mid = FeeSplitter::split_fees(total, mid);
    assert_eq!(
        split_mid.burn + split_mid.validators + split_mid.treasury + split_mid.dev_pool,
        total,
        "midpoint split must conserve"
    );

    // Epoch 1825 (final): end ratios.
    let burn_end = lerp_bps(
        FEE_START_BURN_BPS,
        FEE_END_BURN_BPS,
        TRANSITION_EPOCHS,
        TRANSITION_EPOCHS,
    );
    let val_end = lerp_bps(
        FEE_START_VALIDATOR_BPS,
        FEE_END_VALIDATOR_BPS,
        TRANSITION_EPOCHS,
        TRANSITION_EPOCHS,
    );
    let trs_end = lerp_bps(
        FEE_START_TREASURY_BPS,
        FEE_END_TREASURY_BPS,
        TRANSITION_EPOCHS,
        TRANSITION_EPOCHS,
    );
    assert_eq!(burn_end, 2500);
    assert_eq!(val_end, 2500);
    assert_eq!(trs_end, 2500);

    // Monotonicity over full range.
    assert!(burn_end > burn_mid, "burn should increase to final");
    assert!(val_end > val_mid, "validator should increase to final");
    assert!(trs_end < trs_mid, "treasury should decrease to final");

    // Verify FeeSplitter at final epoch conserves.
    let split_end = FeeSplitter::split_fees(total, TRANSITION_EPOCHS);
    assert_eq!(
        split_end.burn + split_end.validators + split_end.treasury + split_end.dev_pool,
        total
    );
}

/// Dev pool distribution: two deployers with different gas usage ->
/// pro-rata split matches usage ratio. Gas tracking resets each epoch.
#[test]
fn test_dev_pool_pro_rata_distribution() {
    let h = TestHarness::new();
    let staking = StakingManager::new(h.state_db.clone());

    let deployer_a = addr(5);
    let deployer_b = addr(6);
    let contract_a = addr(50);
    let contract_b = addr(60);

    // deployer_a: 300,000 gas, deployer_b: 700,000 gas.
    DevPool::record_gas_usage(&staking, deployer_a, contract_a, 300_000).unwrap();
    DevPool::record_gas_usage(&staking, deployer_b, contract_b, 700_000).unwrap();

    let pool_amount = wei(10_000);
    let count = DevPool::distribute(&staking, pool_amount).unwrap();
    assert_eq!(count, 2);

    // Pro-rata: A = 300k/1M = 30%, B = remainder = 70%.
    let bal_a = balance(&h, &deployer_a);
    let bal_b = balance(&h, &deployer_b);

    assert_eq!(bal_a, wei(3_000));
    assert_eq!(bal_b, wei(7_000));

    // Conservation: A + B = pool_amount.
    assert_eq!(bal_a + bal_b, pool_amount);

    // Gas tracking resets after epoch.
    DevPool::reset_epoch(&staking).unwrap();
    let entries = DevPool::all_entries(&staking).unwrap();
    assert!(entries.is_empty(), "gas tracking should reset after epoch");
}

/// Validator reward with delegator redistribution: verify commission math
/// is exact (proposer commission + each delegator share = validator portion).
#[test]
fn test_validator_reward_with_delegator_redistribution() {
    let h = TestHarness::new();
    let staking = StakingManager::new(h.state_db.clone());

    let proposer = addr(1);
    let d1 = addr(2);
    let d2 = addr(3);
    let d3 = addr(4);

    staking.credit_balance(&proposer, wei(200_000)).unwrap();
    staking.credit_balance(&d1, wei(100_000)).unwrap();
    staking.credit_balance(&d2, wei(100_000)).unwrap();
    staking.credit_balance(&d3, wei(100_000)).unwrap();

    // 20% commission.
    staking
        .register_validator(proposer, [1u8; 32], 2000, wei(10_000))
        .unwrap();
    staking.delegate(d1, proposer, wei(20_000)).unwrap();
    staking.delegate(d2, proposer, wei(30_000)).unwrap();
    staking.delegate(d3, proposer, wei(50_000)).unwrap();

    let validator_share = wei(10_000);
    FeeSplitter::distribute_validator_rewards(&staking, &proposer, validator_share).unwrap();

    // Commission = 20% of 10,000 = 2,000 -> proposer.
    // Delegator pool = 8,000.
    // d1: 20k/100k * 8000 = 1,600.
    // d2: 30k/100k * 8000 = 2,400.
    // d3: remainder = 8000 - 1600 - 2400 = 4,000.
    let r_proposer = staking
        .get_pending_rewards(&proposer)
        .unwrap()
        .unwrap()
        .amount;
    let r_d1 = staking.get_pending_rewards(&d1).unwrap().unwrap().amount;
    let r_d2 = staking.get_pending_rewards(&d2).unwrap().unwrap().amount;
    let r_d3 = staking.get_pending_rewards(&d3).unwrap().unwrap().amount;

    assert_eq!(r_proposer, wei(2_000));
    assert_eq!(r_d1, wei(1_600));
    assert_eq!(r_d2, wei(2_400));
    assert_eq!(r_d3, wei(4_000));

    // Exact conservation: commission + all delegator shares = validator share.
    assert_eq!(r_proposer + r_d1 + r_d2 + r_d3, validator_share);
}

/// Zero fees: empty block -> fee split runs without error, all splits = 0.
#[test]
fn test_zero_fees_no_error() {
    let h = TestHarness::new();
    let staking = StakingManager::new(h.state_db.clone());

    let proposer = addr(1);
    let treasury = addr(10);
    let dev_pool_addr = addr(11);

    // Zero fees: split produces all zeros.
    let split = FeeSplitter::split_fees(U256::ZERO, 0);
    assert_eq!(split.burn, U256::ZERO);
    assert_eq!(split.validators, U256::ZERO);
    assert_eq!(split.treasury, U256::ZERO);
    assert_eq!(split.dev_pool, U256::ZERO);

    // Execute each part with zero: no errors.
    FeeSplitter::execute_burn(&staking, U256::ZERO).unwrap();
    FeeSplitter::distribute_validator_rewards(&staking, &proposer, U256::ZERO).unwrap();
    FeeSplitter::credit_treasury(&staking, &treasury, U256::ZERO).unwrap();

    // RewardDistributor also handles zero fees gracefully.
    staking.credit_balance(&proposer, wei(100_000)).unwrap();
    staking
        .register_validator(proposer, [1u8; 32], 500, wei(10_000))
        .unwrap();

    RewardDistributor::distribute_block_fees(
        &staking,
        proposer,
        U256::ZERO,
        0,
        treasury,
        dev_pool_addr,
    )
    .unwrap();

    // No rewards generated.
    assert!(staking.get_pending_rewards(&proposer).unwrap().is_none());
}
