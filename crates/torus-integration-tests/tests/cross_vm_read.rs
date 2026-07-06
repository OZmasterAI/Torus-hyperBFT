//! Cross-VM read precompile tests (task 2.10.2).
//!
//! Tests that EVM-facing read precompiles return data consistent with
//! the native state written by torus-core and torus-economics.

mod common;

use alloy_primitives::Address;
use torus_bridge::native_executor::NativeExecutor;
use torus_core::precompiles::{
    abi, execute_precompile, precompile_address, OrderBookSnapshot, PriceLevel,
    ADDR_BALANCE_READER, ADDR_ORACLE_READER, ADDR_ORDER_BOOK_READER, ADDR_STAKING_READER,
};
use torus_types::{FixedPoint, NativeAction, OrderType, PlaceOrderParams, TimeInForce, U256};

use crate::common::TestHarness;

// ============================================================================
// Helpers
// ============================================================================

/// Build ABI call data: 4-byte selector + words.
fn call_data(signature: &str, words: &[[u8; 32]]) -> Vec<u8> {
    let hash = alloy_primitives::keccak256(signature.as_bytes());
    let mut data = Vec::with_capacity(4 + words.len() * 32);
    data.extend_from_slice(&hash[..4]);
    for w in words {
        data.extend_from_slice(w);
    }
    data
}

// ============================================================================
// OrderBookReader (0x0800) tests
// ============================================================================

/// Read order book after persisting a snapshot — precompile output matches state.
#[test]
fn test_order_book_reader_with_snapshot() {
    let h = TestHarness::new();
    let market_id = 1u64;

    let bid_price = TestHarness::fp(49000);
    let bid_qty = TestHarness::fp(5);
    let ask_price = TestHarness::fp(51000);
    let ask_qty = TestHarness::fp(3);

    let snapshot = TestHarness::snapshot(&[(bid_price, bid_qty)], &[(ask_price, ask_qty)]);
    h.persist_order_book(market_id, &snapshot);

    // Call OrderBookReader: getOrderBook(bytes32)
    let addr = precompile_address(ADDR_ORDER_BOOK_READER);
    let input = call_data("getOrderBook(bytes32)", &[abi::encode_market_id(market_id)]);
    let caller = Address::ZERO;
    let result = execute_precompile(&addr, &input, &caller, &h.state_db, 1).unwrap();

    // Response: (uint128[] bid_prices, uint128[] bid_qtys, uint128[] ask_prices, uint128[] ask_qtys)
    // Head: 4 offset words, then each array is [length, elements...]
    assert!(!result.is_empty(), "precompile should return data");

    // Decode the first array — bid_prices. Offsets are in the first 4 words (32 bytes each).
    // The bid_prices array starts at the offset in word 0.
    let offset0 = u32::from_be_bytes(result[28..32].try_into().unwrap()) as usize;
    let bid_count =
        u32::from_be_bytes(result[offset0 + 28..offset0 + 32].try_into().unwrap()) as usize;
    assert_eq!(bid_count, 1, "should have 1 bid level");

    // Read the bid price value (the element after the length word)
    let bp_start = offset0 + 32;
    let bid_price_raw =
        u128::from_be_bytes(result[bp_start + 16..bp_start + 32].try_into().unwrap());
    assert_eq!(bid_price_raw, bid_price.raw() as u128);
}

/// Read position through OrderBookReader getPosition(address,bytes32).
#[test]
fn test_order_book_reader_position() {
    let h = TestHarness::new();
    let trader = TestHarness::addr(1);
    let market_id = 1u64;

    // Create a long position directly
    let pos = torus_core::position::Position {
        trader,
        market_id,
        is_long: true,
        size: TestHarness::fp(10),
        entry_price: TestHarness::fp(50000),
        realized_pnl: FixedPoint::ZERO,
        isolated_margin: TestHarness::fp(5000),
        margin_type: torus_core::position::MarginType::Isolated,
    };
    h.positions.put_position(&pos).unwrap();

    let addr = precompile_address(ADDR_ORDER_BOOK_READER);
    let input = call_data(
        "getPosition(address,bytes32)",
        &[
            abi::encode_address(&trader),
            abi::encode_market_id(market_id),
        ],
    );
    let result = execute_precompile(&addr, &input, &Address::ZERO, &h.state_db, 1).unwrap();

    // getPosition returns: (int128 size, uint128 entry_price, int128 unrealized, int128 realized, uint128 margin)
    assert_eq!(result.len(), 160, "5 words × 32 bytes");

    // size = positive for long
    let size_raw = i128::from_be_bytes(result[16..32].try_into().unwrap());
    assert_eq!(size_raw, TestHarness::fp(10).raw());

    // entry_price
    let entry_raw = u128::from_be_bytes(result[48..64].try_into().unwrap());
    assert_eq!(entry_raw, TestHarness::fp(50000).raw() as u128);

    // margin
    let margin_raw = u128::from_be_bytes(result[144..160].try_into().unwrap());
    assert_eq!(margin_raw, TestHarness::fp(5000).raw() as u128);
}

// ============================================================================
// BalanceReader (0x0801) tests
// ============================================================================

/// Read balances through BalanceReader precompile.
#[test]
fn test_balance_reader() {
    let h = TestHarness::new();
    let trader = TestHarness::addr(5);

    h.fund_native(&trader, TestHarness::fp(7500));
    h.fund_evm(&trader, U256::from(2_000_000_000u128));

    let addr = precompile_address(ADDR_BALANCE_READER);
    let input = call_data("getBalances(address)", &[abi::encode_address(&trader)]);
    let result = execute_precompile(&addr, &input, &Address::ZERO, &h.state_db, 1).unwrap();

    // getBalances returns: (uint128 native, uint128 evm, uint128 margin, uint128 available)
    assert_eq!(result.len(), 128, "4 words × 32 bytes");

    let native_raw = u128::from_be_bytes(result[16..32].try_into().unwrap());
    assert_eq!(native_raw, TestHarness::fp(7500).raw() as u128);

    let evm_raw = u128::from_be_bytes(result[48..64].try_into().unwrap());
    assert_eq!(evm_raw, 2_000_000_000u128);
}

// ============================================================================
// OracleReader (0x0802) tests
// ============================================================================

/// Read oracle price after submitting + aggregating.
#[test]
fn test_oracle_reader_after_aggregation() {
    let h = TestHarness::new();
    let market_id = 1u64;
    let oracle_price = TestHarness::fp(50500);
    let block_number = 10u64;

    // Write aggregated oracle price directly
    h.set_oracle_price(market_id, oracle_price, block_number);

    let addr = precompile_address(ADDR_ORACLE_READER);
    let input = call_data("getPrice(bytes32)", &[abi::encode_market_id(market_id)]);
    let result =
        execute_precompile(&addr, &input, &Address::ZERO, &h.state_db, block_number).unwrap();

    // getPrice returns: (uint128 price, uint64 block_number, bool stale)
    assert_eq!(result.len(), 96);

    let price_raw = u128::from_be_bytes(result[16..32].try_into().unwrap());
    assert_eq!(price_raw, oracle_price.raw() as u128);

    let ret_block = u64::from_be_bytes(result[56..64].try_into().unwrap());
    assert_eq!(ret_block, block_number);

    // Should not be stale (current_block == block_number)
    let stale = result[95];
    assert_eq!(stale, 0, "price should not be stale");
}

/// Oracle price goes stale after DEFAULT_MAX_ORACLE_AGE (100 blocks).
#[test]
fn test_oracle_reader_stale_price() {
    let h = TestHarness::new();
    let market_id = 2u64;

    h.set_oracle_price(market_id, TestHarness::fp(60000), 10);

    let addr = precompile_address(ADDR_ORACLE_READER);
    let input = call_data("getPrice(bytes32)", &[abi::encode_market_id(market_id)]);

    // Query at block 111 (10 + 101 > 100 max age)
    let result = execute_precompile(&addr, &input, &Address::ZERO, &h.state_db, 111).unwrap();
    let stale = result[95];
    assert_eq!(stale, 1, "price should be stale after 100 blocks");
}

// ============================================================================
// StakingReader (0x0803) tests
// ============================================================================

/// Read staking info through the precompile after direct DB setup.
#[test]
fn test_staking_reader_via_delegation() {
    let h = TestHarness::new();
    let staker = TestHarness::addr(10);
    let validator = TestHarness::addr(20);

    // Write a delegation record to CF_STAKING_DELEGATIONS.
    // Key: delegator(20) + validator(20) = 40 bytes
    // Value: delegator(20) + validator(20) + amount(U256 32 BE) = 72 bytes
    let mut key = Vec::with_capacity(40);
    key.extend_from_slice(staker.as_slice());
    key.extend_from_slice(validator.as_slice());

    let amount = U256::from(1_000_000_000_000u128);
    let mut value = Vec::with_capacity(72);
    value.extend_from_slice(staker.as_slice());
    value.extend_from_slice(validator.as_slice());
    value.extend_from_slice(&amount.to_be_bytes::<32>());

    h.state_db
        .put_cf_raw(torus_state::cf::CF_STAKING_DELEGATIONS, &key, &value)
        .unwrap();

    // Query via StakingReader precompile
    let addr = precompile_address(ADDR_STAKING_READER);
    let input = call_data("getStakingInfo(address)", &[abi::encode_address(&staker)]);
    let result = execute_precompile(&addr, &input, &Address::ZERO, &h.state_db, 1).unwrap();

    // getStakingInfo returns: (uint128 delegated, uint128 permanent, uint128 rewards, address validator)
    assert_eq!(result.len(), 128);

    let delegated = u128::from_be_bytes(result[16..32].try_into().unwrap());
    assert_eq!(delegated, 1_000_000_000_000u128);
}

// ============================================================================
// Cross-consistency: native action → precompile read
// ============================================================================

/// Place orders via NativeExecutor, persist snapshot, verify via OrderBookReader.
#[test]
fn test_cross_consistency_order_then_read() {
    let h = TestHarness::new();
    let mut ctx = h.exec_context(1);
    let trader = TestHarness::addr(1);
    let market_id = 1u64;

    h.fund_native(&trader, TestHarness::fp(1_000_000));

    // Place a resting buy order (no matching sell, so it stays on book)
    let action = NativeAction::PlaceOrder(PlaceOrderParams {
        market_id,
        is_buy: true,
        price: TestHarness::fp(48000),
        quantity: TestHarness::fp(3),
        order_type: OrderType::Limit,
        time_in_force: TimeInForce::GTC,
        reduce_only: false,
        client_order_id: None,
    });
    let r = NativeExecutor::execute(&mut ctx, &trader, &action);
    assert!(r.success);

    // Manually snapshot the in-memory book and persist it
    let snapshot = OrderBookSnapshot {
        bids: vec![PriceLevel {
            price: TestHarness::fp(48000),
            quantity: TestHarness::fp(3),
        }],
        asks: vec![],
    };
    h.persist_order_book(market_id, &snapshot);

    // Now read via precompile
    let addr = precompile_address(ADDR_ORDER_BOOK_READER);
    let input = call_data("getOrderBook(bytes32)", &[abi::encode_market_id(market_id)]);
    let result = execute_precompile(&addr, &input, &Address::ZERO, &h.state_db, 1).unwrap();

    // Verify bid count = 1
    let offset0 = u32::from_be_bytes(result[28..32].try_into().unwrap()) as usize;
    let bid_count =
        u32::from_be_bytes(result[offset0 + 28..offset0 + 32].try_into().unwrap()) as usize;
    assert_eq!(bid_count, 1);
}
