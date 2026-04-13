//! Integration tests for permanent staking (task 2.6.5).

use alloy_primitives::{Address, U256};
use revm::state::AccountInfo;
use torus_economics::{EconomicsError, RewardDistributor, StakingManager};
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
    mgr.state_db().put_account(a, &info).unwrap();
}

// ============================================================================
// 2.6.1: Lock permanent stake from liquid balance
// ============================================================================

#[test]
fn lock_permanent_stake_debits_balance() {
    let (_dir, mgr) = setup();
    let staker = addr(1);
    fund(&mgr, &staker, wei(100_000));

    mgr.permanent_stake(staker, wei(25_000), 50).unwrap();

    // Balance should be debited.
    let acct = mgr.state_db().get_account(&staker).unwrap().unwrap();
    assert_eq!(acct.balance, wei(75_000));

    // Permanent stake should be credited.
    let info = mgr.get_permanent_stake(&staker).unwrap().unwrap();
    assert_eq!(info.amount, wei(25_000));
    assert_eq!(info.locked_at_block, 50);
}

#[test]
fn lock_permanent_stake_accumulates() {
    let (_dir, mgr) = setup();
    let staker = addr(1);
    fund(&mgr, &staker, wei(100_000));

    mgr.permanent_stake(staker, wei(10_000), 100).unwrap();
    mgr.permanent_stake(staker, wei(20_000), 200).unwrap();

    let info = mgr.get_permanent_stake(&staker).unwrap().unwrap();
    assert_eq!(info.amount, wei(30_000));

    let acct = mgr.state_db().get_account(&staker).unwrap().unwrap();
    assert_eq!(acct.balance, wei(70_000));
}

// ============================================================================
// 2.6.1: Permanent stake is irreversible (no unlock)
// ============================================================================

#[test]
fn permanent_stake_has_no_unlock_method() {
    // StakingManager has no `unlock_permanent_stake` or `undelegate_permanent`
    // method. The only way to interact with permanent stake is `permanent_stake()`
    // which only adds. This test verifies the stake persists and cannot be reduced.
    let (_dir, mgr) = setup();
    let staker = addr(1);
    fund(&mgr, &staker, wei(50_000));

    mgr.permanent_stake(staker, wei(30_000), 0).unwrap();

    let info = mgr.get_permanent_stake(&staker).unwrap().unwrap();
    assert_eq!(info.amount, wei(30_000));

    // Additional stake only increases.
    mgr.permanent_stake(staker, wei(5_000), 100).unwrap();
    let info = mgr.get_permanent_stake(&staker).unwrap().unwrap();
    assert_eq!(info.amount, wei(35_000));
}

// ============================================================================
// 2.6.2 + 2.6.3: Distribute rewards, non-auto-compounding
// ============================================================================

#[test]
fn permanent_staking_rewards_go_to_liquid_balance() {
    let (_dir, mgr) = setup();
    let staker = addr(1);
    fund(&mgr, &staker, wei(100_000));

    mgr.permanent_stake(staker, wei(50_000), 0).unwrap();

    let balance_before = mgr
        .state_db()
        .get_account(&staker)
        .unwrap()
        .unwrap()
        .balance;
    assert_eq!(balance_before, wei(50_000));

    let blocks_in_epoch = 100_000u64;
    let minted =
        RewardDistributor::distribute_permanent_staking_rewards(&mgr, blocks_in_epoch).unwrap();

    // Verify rewards are non-zero.
    assert!(!minted.is_zero());

    // Verify rewards went to liquid balance (non-auto-compounding).
    let balance_after = mgr
        .state_db()
        .get_account(&staker)
        .unwrap()
        .unwrap()
        .balance;
    assert_eq!(balance_after, balance_before + minted);

    // Permanent stake should NOT have changed (not auto-compounded).
    let info = mgr.get_permanent_stake(&staker).unwrap().unwrap();
    assert_eq!(info.amount, wei(50_000));
}

#[test]
fn permanent_staking_rewards_proportional_distribution() {
    let (_dir, mgr) = setup();
    let s1 = addr(1);
    let s2 = addr(2);

    fund(&mgr, &s1, wei(100_000));
    fund(&mgr, &s2, wei(100_000));

    // s1 stakes 30k, s2 stakes 70k.
    mgr.permanent_stake(s1, wei(30_000), 0).unwrap();
    mgr.permanent_stake(s2, wei(70_000), 0).unwrap();

    let bal_s1_before = mgr.state_db().get_account(&s1).unwrap().unwrap().balance;
    let bal_s2_before = mgr.state_db().get_account(&s2).unwrap().unwrap().balance;

    let blocks_in_epoch = 200_000u64;
    let total_minted =
        RewardDistributor::distribute_permanent_staking_rewards(&mgr, blocks_in_epoch).unwrap();

    let reward_s1 = mgr.state_db().get_account(&s1).unwrap().unwrap().balance - bal_s1_before;
    let reward_s2 = mgr.state_db().get_account(&s2).unwrap().unwrap().balance - bal_s2_before;

    // s2 should get ~2.33x more than s1 (70/30 ratio).
    assert!(reward_s2 > reward_s1);
    // Total should match.
    assert_eq!(reward_s1 + reward_s2, total_minted);

    // Check exact proportions: reward = stake * 500 * blocks / (31536000 * 10000).
    let expected_s1 = wei(30_000) * U256::from(500u64) * U256::from(200_000u64)
        / (U256::from(31_536_000u64) * U256::from(10_000u64));
    let expected_s2 = wei(70_000) * U256::from(500u64) * U256::from(200_000u64)
        / (U256::from(31_536_000u64) * U256::from(10_000u64));
    assert_eq!(reward_s1, expected_s1);
    assert_eq!(reward_s2, expected_s2);
}

// ============================================================================
// Edge cases
// ============================================================================

#[test]
fn lock_zero_amount_is_noop() {
    let (_dir, mgr) = setup();
    let staker = addr(1);
    fund(&mgr, &staker, wei(10_000));

    // Zero permanent stake is a no-op (no error, no state change).
    mgr.permanent_stake(staker, U256::ZERO, 0).unwrap();

    assert!(mgr.get_permanent_stake(&staker).unwrap().is_none());
    let acct = mgr.state_db().get_account(&staker).unwrap().unwrap();
    assert_eq!(acct.balance, wei(10_000));
}

#[test]
fn lock_more_than_balance_fails() {
    let (_dir, mgr) = setup();
    let staker = addr(1);
    fund(&mgr, &staker, wei(5_000));

    let result = mgr.permanent_stake(staker, wei(10_000), 0);
    assert!(matches!(
        result,
        Err(EconomicsError::InsufficientBalance { .. })
    ));
}

#[test]
fn no_permanent_stakers_distributes_nothing() {
    let (_dir, mgr) = setup();
    let minted =
        RewardDistributor::distribute_permanent_staking_rewards(&mgr, 100_000).unwrap();
    assert_eq!(minted, U256::ZERO);
}
