//! E2E trading integration tests (task 2.10.1).
//!
//! Tests the full order lifecycle through NativeExecutor → OrderBook → PositionManager,
//! then verifies state via RPC where applicable.

mod common;

use alloy_primitives::Address;
use jsonrpsee::core::client::ClientT;
use jsonrpsee::core::params::ObjectParams;
use torus_bridge::native_executor::NativeExecutor;
use torus_types::{FixedPoint, NativeAction, OrderType, PlaceOrderParams, TimeInForce, U256};

use crate::common::TestHarness;

// ============================================================================
// Helpers
// ============================================================================

fn buy_order(market_id: u64, price: FixedPoint, qty: FixedPoint) -> NativeAction {
    NativeAction::PlaceOrder(PlaceOrderParams {
        market_id,
        is_buy: true,
        price,
        quantity: qty,
        order_type: OrderType::Limit,
        time_in_force: TimeInForce::GTC,
        reduce_only: false,
        client_order_id: None,
    })
}

fn sell_order(market_id: u64, price: FixedPoint, qty: FixedPoint) -> NativeAction {
    NativeAction::PlaceOrder(PlaceOrderParams {
        market_id,
        is_buy: false,
        price,
        quantity: qty,
        order_type: OrderType::Limit,
        time_in_force: TimeInForce::GTC,
        reduce_only: false,
        client_order_id: None,
    })
}

fn hex_addr(a: &Address) -> String {
    format!("0x{}", alloy_primitives::hex::encode(a.as_slice()))
}

// ============================================================================
// Tests
// ============================================================================

/// Full order lifecycle: buy + sell match → positions created.
#[test]
fn test_full_order_lifecycle() {
    let h = TestHarness::new();
    let mut ctx = h.exec_context(1);
    let trader_a = TestHarness::addr(1);
    let trader_b = TestHarness::addr(2);
    let market = 1u64;

    h.fund_native(&trader_a, TestHarness::fp(100_000));
    h.fund_native(&trader_b, TestHarness::fp(100_000));

    // Place buy at 50000, qty 1
    let r1 = NativeExecutor::execute(
        &mut ctx,
        &trader_a,
        &buy_order(market, TestHarness::fp(50000), TestHarness::fp(1)),
    );
    assert!(r1.success, "buy order should succeed");

    // Place matching sell at 50000, qty 1
    let r2 = NativeExecutor::execute(
        &mut ctx,
        &trader_b,
        &sell_order(market, TestHarness::fp(50000), TestHarness::fp(1)),
    );
    assert!(r2.success, "sell order should succeed");

    // Both traders should have positions
    let pos_a = h
        .positions
        .get_position(&trader_a, market)
        .unwrap()
        .expect("trader_a position");
    assert!(pos_a.is_long);
    assert_eq!(pos_a.size, TestHarness::fp(1));
    assert_eq!(pos_a.entry_price, TestHarness::fp(50000));

    let pos_b = h
        .positions
        .get_position(&trader_b, market)
        .unwrap()
        .expect("trader_b position");
    assert!(!pos_b.is_long);
    assert_eq!(pos_b.size, TestHarness::fp(1));
    assert_eq!(pos_b.entry_price, TestHarness::fp(50000));
}

/// Partial fill: large buy matched by smaller sell.
#[test]
fn test_partial_fill() {
    let h = TestHarness::new();
    let mut ctx = h.exec_context(1);
    let buyer = TestHarness::addr(1);
    let seller = TestHarness::addr(2);
    let market = 1u64;

    h.fund_native(&buyer, TestHarness::fp(1_000_000));
    h.fund_native(&seller, TestHarness::fp(1_000_000));

    // Place large buy: 10 units at 50000
    NativeExecutor::execute(
        &mut ctx,
        &buyer,
        &buy_order(market, TestHarness::fp(50000), TestHarness::fp(10)),
    );

    // Match with smaller sell: 3 units at 50000
    NativeExecutor::execute(
        &mut ctx,
        &seller,
        &sell_order(market, TestHarness::fp(50000), TestHarness::fp(3)),
    );

    // Buyer should have a position for 3 filled units
    let pos_buyer = h
        .positions
        .get_position(&buyer, market)
        .unwrap()
        .expect("buyer position");
    assert!(pos_buyer.is_long);
    assert_eq!(pos_buyer.size, TestHarness::fp(3));

    // Seller should also have 3
    let pos_seller = h
        .positions
        .get_position(&seller, market)
        .unwrap()
        .expect("seller position");
    assert_eq!(pos_seller.size, TestHarness::fp(3));

    // The remaining 7 units should still be resting on the book
    let book = ctx.order_books.get(&market).expect("order book exists");
    assert!(
        book.best_bid().is_some(),
        "resting buy orders should remain"
    );
}

/// Cancel order: place then cancel, verify gone from book.
#[test]
fn test_cancel_order() {
    let h = TestHarness::new();
    let mut ctx = h.exec_context(1);
    let trader = TestHarness::addr(1);
    let market = 1u64;

    h.fund_native(&trader, TestHarness::fp(100_000));

    // Place a GTC buy order
    let r = NativeExecutor::execute(
        &mut ctx,
        &trader,
        &buy_order(market, TestHarness::fp(45000), TestHarness::fp(2)),
    );
    assert!(r.success);

    // The order should be resting (no matching sell)
    let book = ctx.order_books.get(&market).unwrap();
    assert!(book.best_bid().is_some());

    // Find the order id — the first order placed gets id=1
    let order_id = 1u128;

    // Cancel it
    let cancel = NativeAction::CancelOrder { order_id };
    let cr = NativeExecutor::execute(&mut ctx, &trader, &cancel);
    assert!(cr.success, "cancel should succeed");

    // Book should be empty now
    let book = ctx.order_books.get(&market).unwrap();
    assert!(
        book.best_bid().is_none(),
        "bids should be empty after cancel"
    );

    // No position should exist (order was never filled)
    assert!(h.positions.get_position(&trader, market).unwrap().is_none());
}

/// Multiple markets: orders on different markets don't interfere.
#[test]
fn test_multiple_markets() {
    let h = TestHarness::new();
    let mut ctx = h.exec_context(1);
    let trader_a = TestHarness::addr(1);
    let trader_b = TestHarness::addr(2);

    h.fund_native(&trader_a, TestHarness::fp(1_000_000));
    h.fund_native(&trader_b, TestHarness::fp(1_000_000));

    let market_1 = 1u64;
    let market_2 = 2u64;

    // Place buy on market 1
    NativeExecutor::execute(
        &mut ctx,
        &trader_a,
        &buy_order(market_1, TestHarness::fp(50000), TestHarness::fp(5)),
    );
    // Place sell on market 2 (should NOT match the buy on market 1)
    NativeExecutor::execute(
        &mut ctx,
        &trader_b,
        &sell_order(market_2, TestHarness::fp(50000), TestHarness::fp(5)),
    );

    // Neither should have positions (orders are on different markets)
    assert!(h
        .positions
        .get_position(&trader_a, market_1)
        .unwrap()
        .is_none());
    assert!(h
        .positions
        .get_position(&trader_b, market_2)
        .unwrap()
        .is_none());

    // Both books should have resting orders
    assert!(!ctx.order_books.get(&market_1).unwrap().best_bid().is_none());
    assert!(ctx.order_books.get(&market_2).unwrap().best_ask().is_some());
}

/// Batch execution: execute actions as a batch and verify ordering.
#[test]
fn test_batch_execution() {
    let h = TestHarness::new();
    let mut ctx = h.exec_context(1);
    let trader_a = TestHarness::addr(1);
    let trader_b = TestHarness::addr(2);
    let market = 1u64;

    h.fund_native(&trader_a, TestHarness::fp(1_000_000));
    h.fund_native(&trader_b, TestHarness::fp(1_000_000));

    let actions: Vec<(Address, NativeAction)> = vec![
        (
            trader_a,
            buy_order(market, TestHarness::fp(50000), TestHarness::fp(5)).clone(),
        ),
        (
            trader_b,
            sell_order(market, TestHarness::fp(50000), TestHarness::fp(5)).clone(),
        ),
    ];

    let batch_result = NativeExecutor::execute_batch(&mut ctx, &actions);
    assert_eq!(batch_result.results.len(), 2);
    assert!(batch_result.results[0].success);
    assert!(batch_result.results[1].success);

    // Both positions should exist from the match
    let pos_a = h
        .positions
        .get_position(&trader_a, market)
        .unwrap()
        .expect("position a");
    let pos_b = h
        .positions
        .get_position(&trader_b, market)
        .unwrap()
        .expect("position b");
    assert_eq!(pos_a.size, TestHarness::fp(5));
    assert_eq!(pos_b.size, TestHarness::fp(5));
}

/// RPC consistency: position created via NativeExecutor matches RPC torus_getPosition.
#[tokio::test]
async fn test_rpc_position_consistency() {
    let h = TestHarness::new();
    let mut ctx = h.exec_context(1);
    let trader_a = TestHarness::addr(1);
    let trader_b = TestHarness::addr(2);
    let market = 1u64;

    h.fund_native(&trader_a, TestHarness::fp(100_000));
    h.fund_native(&trader_b, TestHarness::fp(100_000));

    // Execute matching orders
    NativeExecutor::execute(
        &mut ctx,
        &trader_a,
        &buy_order(market, TestHarness::fp(50000), TestHarness::fp(2)),
    );
    NativeExecutor::execute(
        &mut ctx,
        &trader_b,
        &sell_order(market, TestHarness::fp(50000), TestHarness::fp(2)),
    );

    // Verify position exists in DB
    let pos = h
        .positions
        .get_position(&trader_a, market)
        .unwrap()
        .expect("position");
    assert_eq!(pos.size, TestHarness::fp(2));

    // Start RPC and query the same position
    let (_handle, client) = h.start_rpc().await;

    let mut params = ObjectParams::new();
    params.insert("trader", hex_addr(&trader_a)).unwrap();
    params.insert("market_id", "0x1").unwrap();
    let rpc_pos: serde_json::Value = client.request("torus_getPosition", params).await.unwrap();

    // RPC should return a position
    assert!(!rpc_pos.is_null(), "RPC should return a position");
    let side = rpc_pos["side"].as_str().unwrap();
    assert_eq!(side, "long");

    // Size should be 2.0 in FixedPoint raw hex
    let size_hex = rpc_pos["size"].as_str().unwrap();
    let size_raw = i128::from_str_radix(size_hex.strip_prefix("0x").unwrap(), 16).unwrap();
    assert_eq!(size_raw, TestHarness::fp(2).raw() as i128);
}

/// RPC consistency: balances match direct state reads.
#[tokio::test]
async fn test_rpc_balances_consistency() {
    let h = TestHarness::new();
    let trader = TestHarness::addr(3);

    h.fund_native(&trader, TestHarness::fp(5000));
    h.fund_evm(&trader, U256::from(1_000_000_000_000_000_000u128)); // 1 ETH in wei

    let (_handle, client) = h.start_rpc().await;

    let rpc_bal: serde_json::Value = client
        .request("torus_getBalances", vec![hex_addr(&trader)])
        .await
        .unwrap();

    // native_balance should be 5000 in FixedPoint hex
    let native_hex = rpc_bal["nativeBalance"].as_str().unwrap();
    let native_raw = i128::from_str_radix(native_hex.strip_prefix("0x").unwrap(), 16).unwrap();
    assert_eq!(native_raw, TestHarness::fp(5000).raw() as i128);

    // EVM balance should be non-zero
    let evm_hex = rpc_bal["evmBalance"].as_str().unwrap();
    assert_ne!(evm_hex, "0x0");
}
