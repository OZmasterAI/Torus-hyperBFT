//! Lockbox E2E tests (task 2.10.4).
//!
//! Tests bidirectional EVM <-> native balance transfers via the Lockbox,
//! including atomicity, conservation invariants, and round-trip correctness.

mod common;

use alloy_primitives::Address;
use torus_bridge::native_executor::NativeExecutor;
use torus_core::lockbox::{fp_to_u256, Lockbox};
use torus_types::{FixedPoint, NativeAction, OrderType, PlaceOrderParams, TimeInForce, U256};

use crate::common::TestHarness;

// ============================================================================
// Helpers
// ============================================================================

fn evm_bal(h: &TestHarness, addr: &Address) -> U256 {
    h.state_db
        .get_account(addr)
        .unwrap()
        .map(|a| a.balance)
        .unwrap_or(U256::ZERO)
}

fn native_bal(h: &TestHarness, addr: &Address) -> FixedPoint {
    h.positions.get_native_balance(addr).unwrap().available
}

// ============================================================================
// Tests
// ============================================================================

/// Deposit EVM -> native, then use the native balance to place an order.
#[test]
fn test_deposit_evm_to_native_and_trade() {
    let h = TestHarness::new();
    let trader = TestHarness::addr(1);
    let deposit = TestHarness::fp(1_000_000);

    h.fund_evm(&trader, fp_to_u256(deposit));
    Lockbox::deposit_to_native(&h.state_db, &trader, deposit).unwrap();

    assert_eq!(evm_bal(&h, &trader), U256::ZERO);
    assert_eq!(native_bal(&h, &trader), deposit);

    // Set up market and oracle so order placement succeeds.
    let market_id = 1u64;
    h.register_market(market_id, "BTC", "USD");
    h.set_oracle_price(market_id, TestHarness::fp(50000), 1);

    let mut ctx = h.exec_context(1);
    let r = NativeExecutor::execute(
        &mut ctx,
        &trader,
        &NativeAction::PlaceOrder(PlaceOrderParams {
            market_id,
            is_buy: true,
            price: TestHarness::fp(50000),
            quantity: TestHarness::fp(1),
            order_type: OrderType::Limit,
            time_in_force: TimeInForce::GTC,
            reduce_only: false,
            client_order_id: None,
        }),
    );
    assert!(
        r.success,
        "should trade with deposited native balance: {:?}",
        r.error
    );
}

/// Withdraw native -> EVM, verify both balances are correct.
#[test]
fn test_withdraw_native_to_evm() {
    let h = TestHarness::new();
    let trader = TestHarness::addr(2);
    let initial = TestHarness::fp(5000);
    let withdraw = TestHarness::fp(3000);

    h.fund_native(&trader, initial);

    Lockbox::withdraw_from_native(&h.state_db, &trader, withdraw).unwrap();

    assert_eq!(native_bal(&h, &trader), TestHarness::fp(2000));
    assert_eq!(evm_bal(&h, &trader), fp_to_u256(withdraw));
}

/// Insufficient EVM balance -> error, no partial transfer (atomic).
#[test]
fn test_insufficient_evm_balance_atomic() {
    let h = TestHarness::new();
    let trader = TestHarness::addr(3);
    let have = TestHarness::fp(100);

    h.fund_evm(&trader, fp_to_u256(have));

    let result = Lockbox::deposit_to_native(&h.state_db, &trader, TestHarness::fp(200));
    assert!(result.is_err(), "should fail with insufficient EVM balance");

    // Both balances unchanged.
    assert_eq!(evm_bal(&h, &trader), fp_to_u256(have));
    assert_eq!(native_bal(&h, &trader), FixedPoint::ZERO);
}

/// Insufficient native balance -> error, no partial transfer (atomic).
#[test]
fn test_insufficient_native_balance_atomic() {
    let h = TestHarness::new();
    let trader = TestHarness::addr(4);
    let have = TestHarness::fp(100);

    h.fund_native(&trader, have);

    let result = Lockbox::withdraw_from_native(&h.state_db, &trader, TestHarness::fp(200));
    assert!(
        result.is_err(),
        "should fail with insufficient native balance"
    );

    // Both balances unchanged.
    assert_eq!(native_bal(&h, &trader), have);
    assert_eq!(evm_bal(&h, &trader), U256::ZERO);
}

/// Full round trip with profit: EVM -> native -> profit -> native -> EVM.
/// Final EVM balance must exceed initial.
#[test]
fn test_round_trip_with_profit() {
    let h = TestHarness::new();
    let trader = TestHarness::addr(5);
    let initial = TestHarness::fp(10000);
    let profit = TestHarness::fp(500);

    h.fund_evm(&trader, fp_to_u256(initial));
    let initial_evm = evm_bal(&h, &trader);

    // EVM -> native
    Lockbox::deposit_to_native(&h.state_db, &trader, initial).unwrap();

    // Simulate trading profit by adding to native balance.
    h.fund_native(&trader, profit);
    let total = initial + profit;

    // native -> EVM
    Lockbox::withdraw_from_native(&h.state_db, &trader, total).unwrap();

    let final_evm = evm_bal(&h, &trader);
    assert!(
        final_evm > initial_evm,
        "final EVM balance should exceed initial"
    );
    assert_eq!(final_evm, fp_to_u256(total));
    assert_eq!(native_bal(&h, &trader), FixedPoint::ZERO);
}

/// Zero amount transfers are no-ops (returns Ok, balances unchanged).
#[test]
fn test_zero_amount_transfers() {
    let h = TestHarness::new();
    let trader = TestHarness::addr(6);
    let bal = TestHarness::fp(1000);

    h.fund_evm(&trader, fp_to_u256(bal));
    h.fund_native(&trader, bal);

    // Zero deposit: no-op
    Lockbox::deposit_to_native(&h.state_db, &trader, FixedPoint::ZERO).unwrap();
    assert_eq!(evm_bal(&h, &trader), fp_to_u256(bal));
    assert_eq!(native_bal(&h, &trader), bal);

    // Zero withdraw: no-op
    Lockbox::withdraw_from_native(&h.state_db, &trader, FixedPoint::ZERO).unwrap();
    assert_eq!(evm_bal(&h, &trader), fp_to_u256(bal));
    assert_eq!(native_bal(&h, &trader), bal);
}

/// Full balance transfer with an odd amount: no rounding loss on round trip.
#[test]
fn test_full_balance_no_rounding_loss() {
    let h = TestHarness::new();
    let trader = TestHarness::addr(7);
    let amount = FixedPoint::from_raw(123_456_789_012_345_678i128);

    h.fund_evm(&trader, fp_to_u256(amount));

    // EVM -> native
    Lockbox::deposit_to_native(&h.state_db, &trader, amount).unwrap();
    assert_eq!(native_bal(&h, &trader), amount);
    assert_eq!(evm_bal(&h, &trader), U256::ZERO);

    // native -> EVM
    Lockbox::withdraw_from_native(&h.state_db, &trader, amount).unwrap();
    assert_eq!(evm_bal(&h, &trader), fp_to_u256(amount));
    assert_eq!(native_bal(&h, &trader), FixedPoint::ZERO);
}

/// Conservation invariant: EVM + native total never changes during transfers.
#[test]
fn test_conservation_invariant() {
    let h = TestHarness::new();
    let trader = TestHarness::addr(8);
    let total = TestHarness::fp(50000);
    let total_u256 = fp_to_u256(total);

    h.fund_evm(&trader, total_u256);

    let check = |h: &TestHarness, msg: &str| {
        let evm = evm_bal(h, &trader);
        let native = fp_to_u256(native_bal(h, &trader));
        assert_eq!(evm + native, total_u256, "conservation violated: {msg}");
    };

    check(&h, "initial");

    Lockbox::deposit_to_native(&h.state_db, &trader, TestHarness::fp(20000)).unwrap();
    check(&h, "after deposit 20000");

    Lockbox::deposit_to_native(&h.state_db, &trader, TestHarness::fp(15000)).unwrap();
    check(&h, "after deposit 15000 more");

    Lockbox::withdraw_from_native(&h.state_db, &trader, TestHarness::fp(10000)).unwrap();
    check(&h, "after withdraw 10000");

    Lockbox::withdraw_from_native(&h.state_db, &trader, TestHarness::fp(25000)).unwrap();
    check(&h, "after withdraw remaining");

    // All back in EVM.
    assert_eq!(evm_bal(&h, &trader), total_u256);
    assert_eq!(native_bal(&h, &trader), FixedPoint::ZERO);
}
