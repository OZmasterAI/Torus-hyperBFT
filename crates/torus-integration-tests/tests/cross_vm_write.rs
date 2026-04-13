//! Cross-VM write precompile tests (task 2.10.3).
//!
//! Tests that CoreWriter (0x0810) and CoreWriterStaking (0x0811) queue actions
//! for delayed execution, and that NativeExecutor::drain_core_writer processes
//! them correctly in the next block.

mod common;

use alloy_primitives::Address;
use torus_bridge::native_executor::NativeExecutor;
use torus_core::precompiles::{
    abi, execute_precompile, precompile_address, CoreWriterQueue, QueuedAction, QueuedActionKind,
    ADDR_CORE_WRITER, ADDR_CORE_WRITER_STAKING,
};
use torus_types::{FixedPoint, NativeAction, OrderType, PlaceOrderParams, TimeInForce};

use crate::common::TestHarness;

// ============================================================================
// Helpers
// ============================================================================

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
// CoreWriter (0x0810) — delayed order execution
// ============================================================================

/// CoreWriter queues order for next block, not current block.
#[test]
fn test_core_writer_delayed_execution() {
    let h = TestHarness::new();
    let trader = TestHarness::addr(1);
    let market_id = 1u64;
    let current_block = 10u64;

    h.fund_native(&trader, TestHarness::fp(1_000_000));

    // Enqueue a PlaceOrder via CoreWriterQueue::enqueue (simulates EVM calling CoreWriter)
    let qa = QueuedAction {
        trader,
        kind: QueuedActionKind::PlaceOrder {
            market_id,
            side: 0, // buy
            order_type: 0, // limit
            price: TestHarness::fp(50000),
            quantity: TestHarness::fp(2),
            time_in_force: 0, // GTC
        },
        block_queued: current_block,
    };
    CoreWriterQueue::enqueue(&h.state_db, &qa).unwrap();

    // Drain in current block (10) — should return nothing (action targets block 11)
    let mut ctx_current = h.exec_context(current_block);
    let results_current = NativeExecutor::drain_core_writer(&mut ctx_current);
    assert!(results_current.is_empty(), "actions should NOT execute in current block");

    // Drain in next block (11) — should execute
    let mut ctx_next = h.exec_context(current_block + 1);
    let results_next = NativeExecutor::drain_core_writer(&mut ctx_next);
    assert_eq!(results_next.len(), 1, "should drain 1 action in next block");
    assert!(results_next[0].success, "queued order should execute successfully");
}

/// CoreWriter placeOrder via precompile call.
#[test]
fn test_core_writer_precompile_call() {
    let h = TestHarness::new();
    let caller = TestHarness::addr(5);
    let market_id = 1u64;
    let current_block = 20u64;

    h.fund_native(&caller, TestHarness::fp(500_000));

    // Build ABI calldata for placeOrder(bytes32,uint8,uint8,uint128,uint128,uint8)
    let addr = precompile_address(ADDR_CORE_WRITER);
    let input = call_data(
        "placeOrder(bytes32,uint8,uint8,uint128,uint128,uint8)",
        &[
            abi::encode_market_id(market_id),
            abi::encode_u8(0),  // side: buy
            abi::encode_u8(0),  // order_type: limit
            abi::encode_u128(TestHarness::fp(45000).raw() as u128), // price
            abi::encode_u128(TestHarness::fp(1).raw() as u128),     // quantity
            abi::encode_u8(0),  // time_in_force: GTC
        ],
    );

    let result = execute_precompile(&addr, &input, &caller, &h.state_db, current_block).unwrap();
    // CoreWriter returns a u64 sequence number in 32 bytes
    assert_eq!(result.len(), 32);

    // Verify the action is queued for block 21 (current + 1)
    let mut ctx_next = h.exec_context(current_block + 1);
    let drained = NativeExecutor::drain_core_writer(&mut ctx_next);
    assert_eq!(drained.len(), 1, "should have 1 queued action");
    assert!(drained[0].success);
}

// ============================================================================
// CoreWriterStaking (0x0811) — delayed delegation
// ============================================================================

/// CoreWriterStaking queues delegation for next block.
#[test]
fn test_core_writer_staking_delayed_delegation() {
    let h = TestHarness::new();
    let delegator = TestHarness::addr(3);
    let validator = TestHarness::addr(99);
    let current_block = 15u64;

    // Enqueue a Delegate via CoreWriterQueue
    let qa = QueuedAction {
        trader: delegator,
        kind: QueuedActionKind::Delegate {
            validator,
            amount: TestHarness::fp(1000),
        },
        block_queued: current_block,
    };
    CoreWriterQueue::enqueue(&h.state_db, &qa).unwrap();

    // Current block: nothing should drain
    let mut ctx_current = h.exec_context(current_block);
    let results = NativeExecutor::drain_core_writer(&mut ctx_current);
    assert!(results.is_empty());

    // Next block: delegation should execute
    let mut ctx_next = h.exec_context(current_block + 1);
    let results = NativeExecutor::drain_core_writer(&mut ctx_next);
    assert_eq!(results.len(), 1);
    // Note: delegation may fail because the validator isn't registered,
    // but the action was successfully dequeued and dispatched.
    assert_eq!(results[0].action_type, "delegate");
}

// ============================================================================
// Execution ordering: native vs CoreWriter
// ============================================================================

/// Native orders execute in current block; CoreWriter orders execute in next block.
#[test]
fn test_execution_ordering_native_vs_core_writer() {
    let h = TestHarness::new();
    let native_trader = TestHarness::addr(1);
    let evm_trader = TestHarness::addr(2);
    let market_id = 1u64;
    let block = 50u64;

    h.fund_native(&native_trader, TestHarness::fp(1_000_000));
    h.fund_native(&evm_trader, TestHarness::fp(1_000_000));

    // 1. Execute native buy order in block 50
    let mut ctx = h.exec_context(block);
    let r = NativeExecutor::execute(
        &mut ctx,
        &native_trader,
        &NativeAction::PlaceOrder(PlaceOrderParams {
            market_id,
            is_buy: true,
            price: TestHarness::fp(50000),
            quantity: TestHarness::fp(5),
            order_type: OrderType::Limit,
            time_in_force: TimeInForce::GTC,
            reduce_only: false,
            client_order_id: None,
        }),
    );
    assert!(r.success, "native order should execute in current block");

    // 2. Queue CoreWriter sell order in block 50 (targets block 51)
    let qa = QueuedAction {
        trader: evm_trader,
        kind: QueuedActionKind::PlaceOrder {
            market_id,
            side: 1, // sell
            order_type: 0,
            price: TestHarness::fp(50000),
            quantity: TestHarness::fp(5),
            time_in_force: 0,
        },
        block_queued: block,
    };
    CoreWriterQueue::enqueue(&h.state_db, &qa).unwrap();

    // 3. In block 50, native buy is resting, CoreWriter order hasn't executed yet
    assert!(ctx.order_books.get(&market_id).unwrap().best_bid().is_some(), "native buy should be resting");
    let drained_50 = NativeExecutor::drain_core_writer(&mut ctx);
    assert!(drained_50.is_empty(), "no CoreWriter actions in block 50");

    // 4. In block 51, the CoreWriter order executes and matches the resting buy.
    //    Need a fresh context for block 51 — but the resting buy is in-memory only.
    //    This test verifies the timing separation at the queue level.
    let mut ctx_51 = h.exec_context(block + 1);
    let drained_51 = NativeExecutor::drain_core_writer(&mut ctx_51);
    assert_eq!(drained_51.len(), 1, "CoreWriter order should drain in block 51");
    assert!(drained_51[0].success);
}

/// Invalid CoreWriter action fails without affecting other queued actions.
#[test]
fn test_invalid_core_writer_action_isolation() {
    let h = TestHarness::new();
    let trader_a = TestHarness::addr(1);
    let trader_b = TestHarness::addr(2);
    let current_block = 30u64;

    h.fund_native(&trader_b, TestHarness::fp(1_000_000));

    // Queue an invalid cancel (nonexistent order) and a valid order
    let invalid = QueuedAction {
        trader: trader_a,
        kind: QueuedActionKind::CancelOrder {
            order_id: 99999, // doesn't exist
        },
        block_queued: current_block,
    };
    let valid = QueuedAction {
        trader: trader_b,
        kind: QueuedActionKind::PlaceOrder {
            market_id: 1,
            side: 0,
            order_type: 0,
            price: TestHarness::fp(40000),
            quantity: TestHarness::fp(1),
            time_in_force: 0,
        },
        block_queued: current_block,
    };

    CoreWriterQueue::enqueue(&h.state_db, &invalid).unwrap();
    CoreWriterQueue::enqueue(&h.state_db, &valid).unwrap();

    // Drain in next block: both should execute (invalid one fails gracefully)
    let mut ctx = h.exec_context(current_block + 1);
    let results = NativeExecutor::drain_core_writer(&mut ctx);
    assert_eq!(results.len(), 2);
    assert!(!results[0].success, "invalid cancel should fail");
    assert!(results[1].success, "valid order should succeed despite earlier failure");
}
