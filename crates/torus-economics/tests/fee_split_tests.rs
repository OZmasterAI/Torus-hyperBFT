//! Integration tests for fee split, burn, validator commission, and dev pool (task 2.7.6).

use alloy_primitives::{Address, U256};
use revm::state::AccountInfo;
use torus_economics::{
    lerp_bps, DevPool, FeeSplitResult, FeeSplitter, StakingManager, FEE_END_BURN_BPS,
    FEE_END_TREASURY_BPS, FEE_END_VALIDATOR_BPS, FEE_START_BURN_BPS, FEE_START_TREASURY_BPS,
    FEE_START_VALIDATOR_BPS, TRANSITION_EPOCHS,
};
use torus_state::StateDb;

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

// ============================================================================
// 2.7.1: Fee split sums to total (no rounding loss)
// ============================================================================

#[test]
fn fee_split_sums_to_total_epoch_0() {
    let total = wei(1_000);
    let result = FeeSplitter::split_fees(total, 0);
    assert_eq!(
        result.burn + result.validators + result.treasury + result.dev_pool,
        total,
        "fee split must sum exactly to total"
    );
}

#[test]
fn fee_split_sums_to_total_epoch_end() {
    let total = wei(1_000);
    let result = FeeSplitter::split_fees(total, TRANSITION_EPOCHS);
    assert_eq!(
        result.burn + result.validators + result.treasury + result.dev_pool,
        total,
    );
}

#[test]
fn fee_split_sums_to_total_odd_amount() {
    // Use an odd amount that doesn't divide evenly by 10000.
    let total = U256::from(999_999_999_999_999_997u64);
    let result = FeeSplitter::split_fees(total, 500);
    assert_eq!(
        result.burn + result.validators + result.treasury + result.dev_pool,
        total,
    );
}

// ============================================================================
// 2.7.1: BPS interpolation at key epochs
// ============================================================================

#[test]
fn fee_split_at_epoch_0_initial_ratios() {
    let total = wei(10_000);
    let result = FeeSplitter::split_fees(total, 0);

    // Epoch 0: 10% burn, 0% validator, 45% treasury, 45% dev_pool.
    assert_eq!(result.burn, wei(1_000));
    assert_eq!(result.validators, U256::ZERO);
    assert_eq!(result.treasury, wei(4_500));
    assert_eq!(result.dev_pool, wei(4_500));
}

#[test]
fn fee_split_at_epoch_1825_final_ratios() {
    let total = wei(10_000);
    let result = FeeSplitter::split_fees(total, TRANSITION_EPOCHS);

    // Epoch 1825: 25% burn, 25% validator, 25% treasury, 25% dev_pool.
    assert_eq!(result.burn, wei(2_500));
    assert_eq!(result.validators, wei(2_500));
    assert_eq!(result.treasury, wei(2_500));
    assert_eq!(result.dev_pool, wei(2_500));
}

#[test]
fn fee_split_at_midpoint_linear_interpolation() {
    let mid = TRANSITION_EPOCHS / 2; // 912

    let burn_bps = lerp_bps(FEE_START_BURN_BPS, FEE_END_BURN_BPS, mid, TRANSITION_EPOCHS);
    let validator_bps = lerp_bps(
        FEE_START_VALIDATOR_BPS,
        FEE_END_VALIDATOR_BPS,
        mid,
        TRANSITION_EPOCHS,
    );
    let treasury_bps = lerp_bps(
        FEE_START_TREASURY_BPS,
        FEE_END_TREASURY_BPS,
        mid,
        TRANSITION_EPOCHS,
    );

    // burn: 1000 + (1500 * 912 / 1825) = 1000 + 749 = 1749
    assert_eq!(burn_bps, 1749);
    // validator: 0 + (2500 * 912 / 1825) = 1249
    assert_eq!(validator_bps, 1249);
    // treasury: 4500 - (2000 * 912 / 1825) = 4500 - 999 = 3501
    assert_eq!(treasury_bps, 3501);

    let total = wei(10_000);
    let result = FeeSplitter::split_fees(total, mid);
    assert_eq!(
        result.burn + result.validators + result.treasury + result.dev_pool,
        total,
    );
}

#[test]
fn fee_split_beyond_transition_uses_final_ratios() {
    let total = wei(10_000);
    let result = FeeSplitter::split_fees(total, TRANSITION_EPOCHS + 1000);
    assert_eq!(result.burn, wei(2_500));
    assert_eq!(result.validators, wei(2_500));
    assert_eq!(result.treasury, wei(2_500));
    assert_eq!(result.dev_pool, wei(2_500));
}

// ============================================================================
// 2.7.1: Zero fees edge case
// ============================================================================

#[test]
fn zero_fees_all_splits_zero() {
    let result = FeeSplitter::split_fees(U256::ZERO, 100);
    assert_eq!(
        result,
        FeeSplitResult {
            burn: U256::ZERO,
            validators: U256::ZERO,
            treasury: U256::ZERO,
            dev_pool: U256::ZERO,
        }
    );
}

// ============================================================================
// 2.7.2: Burn mechanism
// ============================================================================

#[test]
fn burn_deducts_from_supply_tracker() {
    let (_dir, mgr) = setup();

    FeeSplitter::execute_burn(&mgr, wei(500)).unwrap();
    let tracker = FeeSplitter::get_supply_tracker(&mgr).unwrap();
    assert_eq!(tracker.cumulative_burned, wei(500));

    FeeSplitter::execute_burn(&mgr, wei(300)).unwrap();
    let tracker = FeeSplitter::get_supply_tracker(&mgr).unwrap();
    assert_eq!(tracker.cumulative_burned, wei(800));
}

#[test]
fn burn_zero_is_noop() {
    let (_dir, mgr) = setup();
    FeeSplitter::execute_burn(&mgr, U256::ZERO).unwrap();
    let tracker = FeeSplitter::get_supply_tracker(&mgr).unwrap();
    assert_eq!(tracker.cumulative_burned, U256::ZERO);
}

// ============================================================================
// 2.7.3: Validator commission + delegator redistribution
// ============================================================================

#[test]
fn validator_commission_proposer_keeps_commission() {
    let (_dir, mgr) = setup();
    let proposer = addr(1);
    let d1 = addr(2);
    let d2 = addr(3);

    fund(&mgr, &proposer, wei(100_000));
    fund(&mgr, &d1, wei(100_000));
    fund(&mgr, &d2, wei(100_000));

    // 10% commission.
    mgr.register_validator(proposer, [1u8; 32], 1000, wei(10_000))
        .unwrap();
    mgr.delegate(d1, proposer, wei(30_000)).unwrap();
    mgr.delegate(d2, proposer, wei(70_000)).unwrap();

    let reward_amount = wei(10_000);
    FeeSplitter::distribute_validator_rewards(&mgr, &proposer, reward_amount).unwrap();

    // Commission = 10% of 10_000 = 1_000 -> proposer pending rewards.
    let proposer_rewards = mgr.get_pending_rewards(&proposer).unwrap().unwrap();
    assert_eq!(proposer_rewards.amount, wei(1_000));

    // Delegator pool = 9_000.
    // d1: 30k/100k * 9000 = 2700
    // d2: 70k/100k * 9000 = 6300
    let d1_rewards = mgr.get_pending_rewards(&d1).unwrap().unwrap();
    let d2_rewards = mgr.get_pending_rewards(&d2).unwrap().unwrap();
    assert_eq!(d1_rewards.amount, wei(2_700));
    assert_eq!(d2_rewards.amount, wei(6_300));
}

#[test]
fn single_delegator_gets_all_non_commission() {
    let (_dir, mgr) = setup();
    let proposer = addr(1);
    let d1 = addr(2);

    fund(&mgr, &proposer, wei(100_000));
    fund(&mgr, &d1, wei(100_000));

    mgr.register_validator(proposer, [1u8; 32], 2000, wei(10_000))
        .unwrap(); // 20% commission
    mgr.delegate(d1, proposer, wei(50_000)).unwrap();

    FeeSplitter::distribute_validator_rewards(&mgr, &proposer, wei(5_000)).unwrap();

    // Commission = 20% of 5000 = 1000.
    let proposer_rewards = mgr.get_pending_rewards(&proposer).unwrap().unwrap();
    assert_eq!(proposer_rewards.amount, wei(1_000));

    // Single delegator gets all remaining = 4000.
    let d1_rewards = mgr.get_pending_rewards(&d1).unwrap().unwrap();
    assert_eq!(d1_rewards.amount, wei(4_000));
}

#[test]
fn no_validator_record_credits_proposer_directly() {
    let (_dir, mgr) = setup();
    let proposer = addr(1);
    fund(&mgr, &proposer, U256::ZERO);

    FeeSplitter::distribute_validator_rewards(&mgr, &proposer, wei(1_000)).unwrap();

    // No validator record -> full amount goes to balance.
    let acct = mgr.state().get_account(&proposer).unwrap().unwrap();
    assert_eq!(acct.balance, wei(1_000));
}

// ============================================================================
// 2.7.4: Treasury accumulation
// ============================================================================

#[test]
fn treasury_credit_and_tracking() {
    let (_dir, mgr) = setup();
    let treasury = addr(10);
    fund(&mgr, &treasury, U256::ZERO);

    FeeSplitter::credit_treasury(&mgr, &treasury, wei(2_000)).unwrap();
    let bal = mgr
        .state()
        .get_account(&treasury)
        .unwrap()
        .unwrap()
        .balance;
    assert_eq!(bal, wei(2_000));

    let tracker = FeeSplitter::get_supply_tracker(&mgr).unwrap();
    assert_eq!(tracker.cumulative_treasury, wei(2_000));

    // Second credit accumulates.
    FeeSplitter::credit_treasury(&mgr, &treasury, wei(3_000)).unwrap();
    let bal = mgr
        .state()
        .get_account(&treasury)
        .unwrap()
        .unwrap()
        .balance;
    assert_eq!(bal, wei(5_000));

    let tracker = FeeSplitter::get_supply_tracker(&mgr).unwrap();
    assert_eq!(tracker.cumulative_treasury, wei(5_000));
}

// ============================================================================
// 2.7.5: Developer pool
// ============================================================================

#[test]
fn dev_pool_two_deployers_pro_rata() {
    let (_dir, mgr) = setup();
    let deployer1 = addr(1);
    let deployer2 = addr(2);
    let contract1 = addr(100);
    let contract2 = addr(101);

    fund(&mgr, &deployer1, U256::ZERO);
    fund(&mgr, &deployer2, U256::ZERO);

    // Deployer1 uses 300k gas, deployer2 uses 700k gas.
    DevPool::record_gas_usage(&mgr, deployer1, contract1, 300_000).unwrap();
    DevPool::record_gas_usage(&mgr, deployer2, contract2, 700_000).unwrap();

    let pool_amount = wei(1_000);
    let count = DevPool::distribute(&mgr, pool_amount).unwrap();
    assert_eq!(count, 2);

    let bal1 = mgr
        .state()
        .get_account(&deployer1)
        .unwrap()
        .unwrap()
        .balance;
    let bal2 = mgr
        .state()
        .get_account(&deployer2)
        .unwrap()
        .unwrap()
        .balance;

    // 300/1000 * 1000 = 300, 700/1000 * 1000 = 700.
    assert_eq!(bal1, wei(300));
    assert_eq!(bal2, wei(700));
    assert_eq!(bal1 + bal2, pool_amount);
}

#[test]
fn dev_pool_gas_tracking_resets_each_epoch() {
    let (_dir, mgr) = setup();
    let deployer = addr(1);
    let contract = addr(100);

    DevPool::record_gas_usage(&mgr, deployer, contract, 500_000).unwrap();

    let entry = DevPool::get_entry(&mgr, &deployer).unwrap().unwrap();
    assert_eq!(entry.total_gas_used, 500_000);

    DevPool::reset_epoch(&mgr).unwrap();

    // After reset, no entries.
    assert!(DevPool::get_entry(&mgr, &deployer).unwrap().is_none());
    let all = DevPool::all_entries(&mgr).unwrap();
    assert!(all.is_empty());
}

#[test]
fn dev_pool_multiple_contracts_same_deployer() {
    let (_dir, mgr) = setup();
    let deployer = addr(1);
    let c1 = addr(100);
    let c2 = addr(101);

    DevPool::record_gas_usage(&mgr, deployer, c1, 100_000).unwrap();
    DevPool::record_gas_usage(&mgr, deployer, c2, 200_000).unwrap();
    // Same contract again -- gas accumulates, contract not duplicated.
    DevPool::record_gas_usage(&mgr, deployer, c1, 50_000).unwrap();

    let entry = DevPool::get_entry(&mgr, &deployer).unwrap().unwrap();
    assert_eq!(entry.total_gas_used, 350_000);
    assert_eq!(entry.contracts.len(), 2);
}

#[test]
fn dev_pool_zero_gas_is_noop() {
    let (_dir, mgr) = setup();
    let deployer = addr(1);
    let contract = addr(100);

    DevPool::record_gas_usage(&mgr, deployer, contract, 0).unwrap();
    assert!(DevPool::get_entry(&mgr, &deployer).unwrap().is_none());
}

#[test]
fn dev_pool_distribute_zero_amount() {
    let (_dir, mgr) = setup();
    let deployer = addr(1);
    let contract = addr(100);

    DevPool::record_gas_usage(&mgr, deployer, contract, 100_000).unwrap();
    let count = DevPool::distribute(&mgr, U256::ZERO).unwrap();
    assert_eq!(count, 0);
}

#[test]
fn dev_pool_distribute_no_entries() {
    let (_dir, mgr) = setup();
    let count = DevPool::distribute(&mgr, wei(1_000)).unwrap();
    assert_eq!(count, 0);
}
