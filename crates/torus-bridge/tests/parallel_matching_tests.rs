//! Tests for per-market parallel order matching (MarketWorkerPool + execute_batch).

use alloy_primitives::Address;

use torus_bridge::market_workers::{MarketWorkerPool, MatchRequest};
use torus_bridge::native_executor::{NativeExecContext, NativeExecutor};
use torus_core::order_book::OrderBook;
use torus_core::position::NativeBalance;
use torus_state::StateDb;
use torus_types::{
    FixedPoint, MarketId, NativeAction, OrderType, PlaceOrderParams, TimeInForce,
};

use std::collections::HashMap;

// ---- Helpers ----

fn open_test_db() -> (tempfile::TempDir, StateDb) {
    let dir = tempfile::tempdir().expect("create temp dir");
    let db = StateDb::open(dir.path()).expect("open db");
    (dir, db)
}

fn addr(n: u8) -> Address {
    Address::new([n; 20])
}

fn fp(v: i64) -> FixedPoint {
    FixedPoint::from_raw(v as i128 * FixedPoint::SCALE)
}

fn make_ctx(state_db: StateDb) -> NativeExecContext {
    NativeExecContext::new(
        state_db,
        1,        // block_height
        1000,     // timestamp
        0,        // epoch
        100,      // epoch_length
        10,       // max_validators
        addr(99), // proposer
        addr(100), // treasury
        addr(101), // dev_pool
    )
}

fn fund_native(ctx: &NativeExecContext, trader: &Address, amount: FixedPoint) {
    let bal = NativeBalance {
        available: amount,
        order_margin: FixedPoint::ZERO,
    };
    ctx.positions.put_native_balance(trader, &bal).unwrap();
}

fn limit_buy(market_id: MarketId, price: i64, qty: i64) -> PlaceOrderParams {
    PlaceOrderParams {
        market_id,
        is_buy: true,
        price: fp(price),
        quantity: fp(qty),
        order_type: OrderType::Limit,
        time_in_force: TimeInForce::GTC,
        reduce_only: false,
        client_order_id: None,
    }
}

fn limit_sell(market_id: MarketId, price: i64, qty: i64) -> PlaceOrderParams {
    PlaceOrderParams {
        market_id,
        is_buy: false,
        price: fp(price),
        quantity: fp(qty),
        order_type: OrderType::Limit,
        time_in_force: TimeInForce::GTC,
        reduce_only: false,
        client_order_id: None,
    }
}

// ============================================================================
// Test 1: Parallel produces same results as sequential (determinism)
// ============================================================================

#[test]
fn parallel_matching_deterministic_same_as_sequential() {
    let (_dir, db) = open_test_db();
    let mut ctx = make_ctx(db.clone());

    let trader_a = addr(1);
    let trader_b = addr(2);
    fund_native(&ctx, &trader_a, fp(100_000));
    fund_native(&ctx, &trader_b, fp(100_000));

    // Market 1: A buys, B sells — should fill
    // Market 2: A buys (resting, no seller)
    let actions: Vec<(Address, NativeAction)> = vec![
        (trader_a, NativeAction::PlaceOrder(limit_buy(1, 100, 5))),
        (trader_b, NativeAction::PlaceOrder(limit_sell(1, 100, 5))),
        (trader_a, NativeAction::PlaceOrder(limit_buy(2, 200, 3))),
    ];

    // Run parallel (the new execute_batch)
    let parallel_result = NativeExecutor::execute_batch(&mut ctx, &actions);

    // All should succeed
    assert!(parallel_result.results[0].success, "order 0 failed: {:?}", parallel_result.results[0].error);
    assert!(parallel_result.results[1].success, "order 1 failed: {:?}", parallel_result.results[1].error);
    assert!(parallel_result.results[2].success, "order 2 failed: {:?}", parallel_result.results[2].error);

    // Now run the same scenario sequentially on a fresh context
    let (_dir2, db2) = open_test_db();
    let mut ctx2 = make_ctx(db2.clone());
    fund_native(&ctx2, &trader_a, fp(100_000));
    fund_native(&ctx2, &trader_b, fp(100_000));

    let seq_result = {
        let mut results = Vec::new();
        for (sender, action) in &actions {
            results.push(NativeExecutor::execute(&mut ctx2, sender, action));
        }
        results
    };

    // Both should produce same success/failure pattern
    for i in 0..actions.len() {
        assert_eq!(
            parallel_result.results[i].success,
            seq_result[i].success,
            "result {i} diverges: parallel={:?} sequential={:?}",
            parallel_result.results[i],
            seq_result[i],
        );
    }

    // Global order IDs should be consistent
    assert_eq!(ctx.next_global_order_id, ctx2.next_global_order_id);
}

// ============================================================================
// Test 2: Multi-market fills settle correctly
// ============================================================================

#[test]
fn multi_market_fills_settle_correctly() {
    let (_dir, db) = open_test_db();
    let mut ctx = make_ctx(db.clone());

    let trader_a = addr(1);
    let trader_b = addr(2);
    let trader_c = addr(3);
    fund_native(&ctx, &trader_a, fp(500_000));
    fund_native(&ctx, &trader_b, fp(500_000));
    fund_native(&ctx, &trader_c, fp(500_000));

    // Market 1: A buys 10 @ 100, B sells 10 @ 100 → fill
    // Market 2: A buys 5 @ 200, C sells 5 @ 200 → fill
    let actions: Vec<(Address, NativeAction)> = vec![
        (trader_a, NativeAction::PlaceOrder(limit_buy(1, 100, 10))),
        (trader_b, NativeAction::PlaceOrder(limit_sell(1, 100, 10))),
        (trader_a, NativeAction::PlaceOrder(limit_buy(2, 200, 5))),
        (trader_c, NativeAction::PlaceOrder(limit_sell(2, 200, 5))),
    ];

    let result = NativeExecutor::execute_batch(&mut ctx, &actions);

    for (i, r) in result.results.iter().enumerate() {
        assert!(r.success, "action {i} failed: {:?}", r.error);
    }

    // Verify trade_index advanced for fills on both markets
    assert!(ctx.trade_index >= 2, "expected at least 2 trades, got {}", ctx.trade_index);

    // Verify order books exist for both markets
    assert!(ctx.order_books.contains_key(&1));
    assert!(ctx.order_books.contains_key(&2));
}

// ============================================================================
// Test 3: Mixed batch — PlaceOrders + Cancels + Staking all execute
// ============================================================================

#[test]
fn mixed_batch_place_orders_and_other_actions() {
    let (_dir, db) = open_test_db();
    let mut ctx = make_ctx(db.clone());

    let trader_a = addr(1);
    let trader_b = addr(2);
    fund_native(&ctx, &trader_a, fp(500_000));
    fund_native(&ctx, &trader_b, fp(500_000));

    // First, place a resting order so we can cancel it
    let setup_actions: Vec<(Address, NativeAction)> = vec![
        (trader_a, NativeAction::PlaceOrder(limit_buy(1, 50, 1))),
    ];
    let setup = NativeExecutor::execute_batch(&mut ctx, &setup_actions);
    assert!(setup.results[0].success);

    // Now mixed batch: cancel (non-PlaceOrder) + new PlaceOrders + staking
    let actions: Vec<(Address, NativeAction)> = vec![
        // Cancel the resting order (Phase 1 — sequential)
        (trader_a, NativeAction::CancelOrder { order_id: 1 }),
        // PlaceOrders (Phase 3 — parallel)
        (trader_a, NativeAction::PlaceOrder(limit_buy(1, 100, 5))),
        (trader_b, NativeAction::PlaceOrder(limit_sell(1, 100, 5))),
        // Staking action (Phase 1 — sequential)
        (trader_a, NativeAction::ClaimRewards),
    ];

    let result = NativeExecutor::execute_batch(&mut ctx, &actions);

    // Cancel should succeed (order 1 existed)
    assert!(result.results[0].success, "cancel failed: {:?}", result.results[0].error);
    // PlaceOrders should succeed
    assert!(result.results[1].success, "place buy failed: {:?}", result.results[1].error);
    assert!(result.results[2].success, "place sell failed: {:?}", result.results[2].error);
    // ClaimRewards may fail (no staking setup) but shouldn't panic
    assert_eq!(result.results[3].action_type, "claim_rewards");
}

// ============================================================================
// Test 4: Same trader placing orders on two markets — margin reserved for both
// ============================================================================

#[test]
fn same_trader_two_markets_margin_reserved() {
    let (_dir, db) = open_test_db();
    let mut ctx = make_ctx(db.clone());

    let trader = addr(1);
    // Fund with exactly enough for both orders' margin requirements
    // price=100 qty=10 → notional=1000, margin=1000/20=50
    // price=200 qty=5  → notional=1000, margin=1000/20=50
    // Total needed: 100
    fund_native(&ctx, &trader, fp(100));

    let actions: Vec<(Address, NativeAction)> = vec![
        (trader, NativeAction::PlaceOrder(limit_buy(1, 100, 10))),
        (trader, NativeAction::PlaceOrder(limit_buy(2, 200, 5))),
    ];

    let result = NativeExecutor::execute_batch(&mut ctx, &actions);

    // Both should succeed since total margin (100) equals available balance
    assert!(result.results[0].success, "market 1 order failed: {:?}", result.results[0].error);
    assert!(result.results[1].success, "market 2 order failed: {:?}", result.results[1].error);

    // Available balance should be zero (all reserved)
    let bal = ctx.positions.get_native_balance(&trader).unwrap();
    assert_eq!(bal.available, FixedPoint::ZERO);
    assert_eq!(bal.order_margin, fp(100));
}

#[test]
fn same_trader_insufficient_margin_second_order_fails() {
    let (_dir, db) = open_test_db();
    let mut ctx = make_ctx(db.clone());

    let trader = addr(1);
    // Fund with enough for only one order's margin (50)
    fund_native(&ctx, &trader, fp(50));

    let actions: Vec<(Address, NativeAction)> = vec![
        (trader, NativeAction::PlaceOrder(limit_buy(1, 100, 10))), // needs 50
        (trader, NativeAction::PlaceOrder(limit_buy(2, 200, 5))),  // needs 50 — insufficient
    ];

    let result = NativeExecutor::execute_batch(&mut ctx, &actions);

    assert!(result.results[0].success, "first order should succeed");
    assert!(!result.results[1].success, "second order should fail (insufficient margin)");
    assert!(result.results[1].error.as_ref().unwrap().contains("insufficient margin"));
}

// ============================================================================
// Test 5: Worker pool handles market with zero PlaceOrders (no-op)
// ============================================================================

#[test]
fn worker_pool_empty_batch_noop() {
    let batches: HashMap<MarketId, (OrderBook, Vec<MatchRequest>)> = HashMap::new();
    let results = MarketWorkerPool::match_parallel(batches, 1000);
    assert!(results.is_empty());
}

#[test]
fn worker_pool_single_market_no_thread_overhead() {
    let book = OrderBook::new(1, fp(1), fp(1));
    let requests = vec![MatchRequest {
        sender: addr(1),
        params: limit_buy(1, 100, 5),
        order_id: 1,
    }];

    let mut batches = HashMap::new();
    batches.insert(1u64, (book, requests));

    let results = MarketWorkerPool::match_parallel(batches, 1000);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].market_id, 1);
    assert_eq!(results[0].results.len(), 1);
}

#[test]
fn worker_pool_multiple_markets_parallel() {
    let mut batches = HashMap::new();

    // Market 1: buy + sell → should fill
    let mut book1 = OrderBook::new(1, fp(1), fp(1));
    // Pre-place a sell at 100
    book1.place_order(limit_sell(1, 100, 5), addr(10), 999);

    let requests1 = vec![MatchRequest {
        sender: addr(1),
        params: limit_buy(1, 100, 5),
        order_id: 100,
    }];

    // Market 2: buy only → rests
    let book2 = OrderBook::new(2, fp(1), fp(1));
    let requests2 = vec![MatchRequest {
        sender: addr(2),
        params: limit_buy(2, 200, 3),
        order_id: 101,
    }];

    // Market 3: sell only → rests
    let book3 = OrderBook::new(3, fp(1), fp(1));
    let requests3 = vec![MatchRequest {
        sender: addr(3),
        params: limit_sell(3, 300, 2),
        order_id: 102,
    }];

    batches.insert(1u64, (book1, requests1));
    batches.insert(2u64, (book2, requests2));
    batches.insert(3u64, (book3, requests3));

    let results = MarketWorkerPool::match_parallel(batches, 1000);
    assert_eq!(results.len(), 3);

    // Find market 1's result — it should have fills
    let m1 = results.iter().find(|r| r.market_id == 1).unwrap();
    assert_eq!(m1.results.len(), 1);
    assert!(!m1.results[0].result.fills.is_empty(), "market 1 should have fills");

    // Market 2 and 3 should have no fills (resting)
    let m2 = results.iter().find(|r| r.market_id == 2).unwrap();
    assert!(m2.results[0].result.fills.is_empty());

    let m3 = results.iter().find(|r| r.market_id == 3).unwrap();
    assert!(m3.results[0].result.fills.is_empty());
}

// ============================================================================
// Test 6: Result ordering preserved — results[i] corresponds to actions[i]
// ============================================================================

#[test]
fn result_ordering_matches_input_ordering() {
    let (_dir, db) = open_test_db();
    let mut ctx = make_ctx(db.clone());

    let trader_a = addr(1);
    let trader_b = addr(2);
    fund_native(&ctx, &trader_a, fp(1_000_000));
    fund_native(&ctx, &trader_b, fp(1_000_000));

    // Interleave PlaceOrders from different markets with non-PlaceOrder actions
    let actions: Vec<(Address, NativeAction)> = vec![
        (trader_a, NativeAction::PlaceOrder(limit_buy(1, 100, 5))),   // [0] place
        (trader_a, NativeAction::CancelOrder { order_id: 999 }),       // [1] cancel (will fail: not found)
        (trader_b, NativeAction::PlaceOrder(limit_sell(2, 200, 3))),   // [2] place
        (trader_a, NativeAction::PlaceOrder(limit_buy(2, 200, 3))),   // [3] place (fills with [2])
        (trader_b, NativeAction::PlaceOrder(limit_sell(1, 100, 5))),  // [4] place (fills with [0])
    ];

    let result = NativeExecutor::execute_batch(&mut ctx, &actions);

    assert_eq!(result.results.len(), 5);

    // [0] place_order — success
    assert_eq!(result.results[0].action_type, "place_order");
    assert!(result.results[0].success);

    // [1] cancel — fails (order 999 doesn't exist)
    assert_eq!(result.results[1].action_type, "cancel_order");
    assert!(!result.results[1].success);

    // [2] place_order — success
    assert_eq!(result.results[2].action_type, "place_order");
    assert!(result.results[2].success);

    // [3] place_order — success
    assert_eq!(result.results[3].action_type, "place_order");
    assert!(result.results[3].success);

    // [4] place_order — success
    assert_eq!(result.results[4].action_type, "place_order");
    assert!(result.results[4].success);
}

// ============================================================================
// Test 7: Global order IDs are unique across markets
// ============================================================================

#[test]
fn global_order_ids_unique_across_markets() {
    let (_dir, db) = open_test_db();
    let mut ctx = make_ctx(db.clone());

    let trader = addr(1);
    fund_native(&ctx, &trader, fp(1_000_000));

    let actions: Vec<(Address, NativeAction)> = vec![
        (trader, NativeAction::PlaceOrder(limit_buy(1, 100, 1))),
        (trader, NativeAction::PlaceOrder(limit_buy(2, 200, 1))),
        (trader, NativeAction::PlaceOrder(limit_buy(3, 300, 1))),
    ];

    let initial_id = ctx.next_global_order_id;
    let result = NativeExecutor::execute_batch(&mut ctx, &actions);

    for r in &result.results {
        assert!(r.success);
    }

    // Should have consumed exactly 3 IDs
    assert_eq!(ctx.next_global_order_id, initial_id + 3);
}

// ============================================================================
// Test 8: PlaceOrderBatch flattens to identical state vs individual orders (B4)
// ============================================================================

#[test]
fn place_order_batch_matches_individual_orders() {
    // --- Run A: one signed batch of 4 orders across 3 markets (one crossing pair) ---
    let (_dir, db) = open_test_db();
    let mut ctx_batch = make_ctx(db.clone());
    let mm = addr(1);
    fund_native(&ctx_batch, &mm, fp(1_000_000));

    let batch = NativeAction::PlaceOrderBatch(vec![
        limit_buy(1, 100, 5),
        limit_buy(2, 200, 3),
        limit_sell(1, 100, 5), // crosses the market-1 buy above
        limit_buy(3, 50, 2),
    ]);
    let initial_id = ctx_batch.next_global_order_id;
    let _ = NativeExecutor::execute_batch(&mut ctx_batch, &[(mm, batch)]);

    // --- Run B: the same 4 orders as individual PlaceOrder actions, same order ---
    let (_dir2, db2) = open_test_db();
    let mut ctx_indiv = make_ctx(db2.clone());
    fund_native(&ctx_indiv, &mm, fp(1_000_000));

    let indiv: Vec<(Address, NativeAction)> = vec![
        (mm, NativeAction::PlaceOrder(limit_buy(1, 100, 5))),
        (mm, NativeAction::PlaceOrder(limit_buy(2, 200, 3))),
        (mm, NativeAction::PlaceOrder(limit_sell(1, 100, 5))),
        (mm, NativeAction::PlaceOrder(limit_buy(3, 50, 2))),
    ];
    let result_indiv = NativeExecutor::execute_batch(&mut ctx_indiv, &indiv);
    for r in &result_indiv.results {
        assert!(r.success, "individual order failed: {:?}", r.error);
    }

    // Identical effect: 4 order IDs consumed, same trade count, same reserved margin.
    assert_eq!(
        ctx_batch.next_global_order_id,
        initial_id + 4,
        "batch must consume exactly 4 global order IDs"
    );
    assert_eq!(ctx_batch.next_global_order_id, ctx_indiv.next_global_order_id);
    assert_eq!(ctx_batch.trade_index, ctx_indiv.trade_index);
    assert_eq!(
        ctx_batch
            .positions
            .get_native_balance(&mm)
            .unwrap()
            .order_margin,
        ctx_indiv
            .positions
            .get_native_balance(&mm)
            .unwrap()
            .order_margin,
        "batched vs individual reserved margin must match"
    );
}

// ============================================================================
// Test 9: A failing order inside a batch is isolated (batch is not all-or-nothing)
// ============================================================================

#[test]
fn place_order_batch_failure_isolated() {
    let (_dir, db) = open_test_db();
    let mut ctx = make_ctx(db.clone());
    let mm = addr(1);
    // Enough margin for 2 of 3 orders (each needs 50); third must fail.
    fund_native(&ctx, &mm, fp(120));

    let batch = NativeAction::PlaceOrderBatch(vec![
        limit_buy(1, 100, 10), // margin 50
        limit_buy(2, 200, 5),  // margin 50
        limit_buy(3, 100, 10), // margin 50 — only 20 left → fails
    ]);
    let result = NativeExecutor::execute_batch(&mut ctx, &[(mm, batch)]);

    // Flattened → one result per order.
    assert_eq!(result.results.len(), 3);
    assert!(result.results[0].success, "order 0: {:?}", result.results[0].error);
    assert!(result.results[1].success, "order 1: {:?}", result.results[1].error);
    assert!(!result.results[2].success, "order 2 should fail on margin");
    assert!(result.results[2]
        .error
        .as_ref()
        .unwrap()
        .contains("insufficient margin"));

    // First two reservations survived the third's failure.
    assert_eq!(
        ctx.positions.get_native_balance(&mm).unwrap().order_margin,
        fp(100)
    );
}
