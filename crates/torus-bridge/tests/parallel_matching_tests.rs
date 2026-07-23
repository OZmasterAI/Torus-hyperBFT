//! Tests for per-market parallel order matching (MarketWorkerPool + execute_batch).

use alloy_primitives::Address;

use torus_bridge::market_workers::{MarketBatchResult, MarketWorkerPool, MatchRequest};
use torus_bridge::native_executor::{NativeExecContext, NativeExecutor};
use torus_core::order_book::OrderBook;
use torus_core::position::NativeBalance;
use torus_state::StateDb;
use torus_types::{
    FixedPoint, MarketId, NativeAction, OrderId, OrderType, PlaceOrderParams, TimeInForce,
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
        1,         // block_height
        1000,      // timestamp
        0,         // epoch
        100,       // epoch_length
        10,        // max_validators
        addr(99),  // proposer
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
    assert!(
        parallel_result.results[0].success,
        "order 0 failed: {:?}",
        parallel_result.results[0].error
    );
    assert!(
        parallel_result.results[1].success,
        "order 1 failed: {:?}",
        parallel_result.results[1].error
    );
    assert!(
        parallel_result.results[2].success,
        "order 2 failed: {:?}",
        parallel_result.results[2].error
    );

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
            parallel_result.results[i].success, seq_result[i].success,
            "result {i} diverges: parallel={:?} sequential={:?}",
            parallel_result.results[i], seq_result[i],
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
    assert!(
        ctx.trade_index >= 2,
        "expected at least 2 trades, got {}",
        ctx.trade_index
    );

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
    let setup_actions: Vec<(Address, NativeAction)> =
        vec![(trader_a, NativeAction::PlaceOrder(limit_buy(1, 50, 1)))];
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
    assert!(
        result.results[0].success,
        "cancel failed: {:?}",
        result.results[0].error
    );
    // PlaceOrders should succeed
    assert!(
        result.results[1].success,
        "place buy failed: {:?}",
        result.results[1].error
    );
    assert!(
        result.results[2].success,
        "place sell failed: {:?}",
        result.results[2].error
    );
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
    assert!(
        result.results[0].success,
        "market 1 order failed: {:?}",
        result.results[0].error
    );
    assert!(
        result.results[1].success,
        "market 2 order failed: {:?}",
        result.results[1].error
    );

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
    assert!(
        !result.results[1].success,
        "second order should fail (insufficient margin)"
    );
    assert!(result.results[1]
        .error
        .as_ref()
        .unwrap()
        .contains("insufficient margin"));
}

// ============================================================================
// Test 5: Worker pool handles market with zero PlaceOrders (no-op)
// ============================================================================

#[test]
fn worker_pool_empty_batch_noop() {
    let batches: HashMap<MarketId, (OrderBook, Vec<MatchRequest<'_>>)> = HashMap::new();
    let results = MarketWorkerPool::match_parallel(batches, 1000).expect("no worker panicked");
    assert!(results.is_empty());
}

#[test]
fn worker_pool_single_market_no_thread_overhead() {
    let book = OrderBook::new(1, fp(1), fp(1));
    let buy = limit_buy(1, 100, 5);
    let requests = vec![MatchRequest {
        sender: addr(1),
        params: &buy,
        order_id: 1,
    }];

    let mut batches = HashMap::new();
    batches.insert(1u64, (book, requests));

    let results = MarketWorkerPool::match_parallel(batches, 1000).expect("no worker panicked");
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

    let buy1 = limit_buy(1, 100, 5);
    let requests1 = vec![MatchRequest {
        sender: addr(1),
        params: &buy1,
        order_id: 100,
    }];

    // Market 2: buy only → rests
    let book2 = OrderBook::new(2, fp(1), fp(1));
    let buy2 = limit_buy(2, 200, 3);
    let requests2 = vec![MatchRequest {
        sender: addr(2),
        params: &buy2,
        order_id: 101,
    }];

    // Market 3: sell only → rests
    let book3 = OrderBook::new(3, fp(1), fp(1));
    let sell3 = limit_sell(3, 300, 2);
    let requests3 = vec![MatchRequest {
        sender: addr(3),
        params: &sell3,
        order_id: 102,
    }];

    batches.insert(1u64, (book1, requests1));
    batches.insert(2u64, (book2, requests2));
    batches.insert(3u64, (book3, requests3));

    let results = MarketWorkerPool::match_parallel(batches, 1000).expect("no worker panicked");
    assert_eq!(results.len(), 3);

    // Find market 1's result — it should have fills
    let m1 = results.iter().find(|r| r.market_id == 1).unwrap();
    assert_eq!(m1.results.len(), 1);
    assert!(
        !m1.results[0].result.fills.is_empty(),
        "market 1 should have fills"
    );

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
        (trader_a, NativeAction::PlaceOrder(limit_buy(1, 100, 5))), // [0] place
        (trader_a, NativeAction::CancelOrder { order_id: 999 }), // [1] cancel (will fail: not found)
        (trader_b, NativeAction::PlaceOrder(limit_sell(2, 200, 3))), // [2] place
        (trader_a, NativeAction::PlaceOrder(limit_buy(2, 200, 3))), // [3] place (fills with [2])
        (trader_b, NativeAction::PlaceOrder(limit_sell(1, 100, 5))), // [4] place (fills with [0])
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
    assert_eq!(
        ctx_batch.next_global_order_id,
        ctx_indiv.next_global_order_id
    );
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
    assert!(
        result.results[0].success,
        "order 0: {:?}",
        result.results[0].error
    );
    assert!(
        result.results[1].success,
        "order 1: {:?}",
        result.results[1].error
    );
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

// ============================================================================
// Test 10: Same-sender batch == same-sender sequential (pins per-batch semantics)
// ============================================================================
//
// Six non-crossing resting limit orders from ONE sender across two markets. In
// market 1 the sender holds BOTH buys (90/91) and sells (110/111): best bid 91 <
// best ask 110, so nothing self-crosses. Market 2 mirrors with buy@90 / sell@110.
// Running them as one execute_batch (Run A) must land the sender in exactly the
// same NativeBalance and produce the same total resting-order count as running
// them as six consecutive single-action execute_batch calls on one ctx (Run B) —
// the sequential oracle for the prod one-ctx-per-block path. This pins the
// existing semantics ahead of the per-batch balance-cache refactor.
#[test]
fn batch_same_sender_orders_equal_individual_batches() {
    let sender = addr(1);

    // All six rest (no crossing pair anywhere). Margin per order = notional/20:
    // 4.5 + 4.55 + 5.5 + 5.55 (mkt1) + 4.5 + 5.5 (mkt2) = 30.1 — funded 100_000.
    let orders = [
        limit_buy(1, 90, 1),
        limit_buy(1, 91, 1),
        limit_sell(1, 110, 1),
        limit_sell(1, 111, 1),
        limit_buy(2, 90, 1),
        limit_sell(2, 110, 1),
    ];

    // --- Run A: one execute_batch call with all six orders ---
    let (_dir_a, db_a) = open_test_db();
    let mut ctx_a = make_ctx(db_a.clone());
    fund_native(&ctx_a, &sender, fp(100_000));

    let actions_a: Vec<(Address, NativeAction)> = orders
        .iter()
        .map(|p| (sender, NativeAction::PlaceOrder(p.clone())))
        .collect();
    let res_a = NativeExecutor::execute_batch(&mut ctx_a, &actions_a);
    for (i, r) in res_a.results.iter().enumerate() {
        assert!(r.success, "batch order {i} failed: {:?}", r.error);
    }

    // --- Run B: six consecutive single-action execute_batch calls on ONE ctx ---
    let (_dir_b, db_b) = open_test_db();
    let mut ctx_b = make_ctx(db_b.clone());
    fund_native(&ctx_b, &sender, fp(100_000));

    for p in &orders {
        let res_b = NativeExecutor::execute_batch(
            &mut ctx_b,
            &[(sender, NativeAction::PlaceOrder(p.clone()))],
        );
        assert!(
            res_b.results[0].success,
            "sequential order failed: {:?}",
            res_b.results[0].error
        );
    }

    // Balances must be bit-identical across batched vs sequential.
    let bal_a = ctx_a.positions.get_native_balance(&sender).unwrap();
    let bal_b = ctx_b.positions.get_native_balance(&sender).unwrap();
    assert_eq!(
        bal_a.available, bal_b.available,
        "available diverges: A={:?} B={:?}",
        bal_a.available, bal_b.available
    );
    assert_eq!(
        bal_a.order_margin, bal_b.order_margin,
        "order_margin diverges: A={:?} B={:?}",
        bal_a.order_margin, bal_b.order_margin
    );

    // All six rest → total resting-order count identical (and equal to 6).
    let count_a: usize = ctx_a.order_books.values().map(|b| b.order_count()).sum();
    let count_b: usize = ctx_b.order_books.values().map(|b| b.order_count()).sum();
    assert_eq!(count_a, 6, "expected all six orders resting in Run A");
    assert_eq!(count_a, count_b, "resting-order count diverges A vs B");
}

// ============================================================================
// Test 11: Stale-per-batch-cache detector — multi-market settle touches A ≥3x
// ============================================================================
//
// In batch 3 (a single execute_batch) trader A's NativeBalance is mutated three
// times across two markets during Phase-4 settlement:
//   (1) release the mkt-1 sell order's margin,
//   (2) credit the mkt-1 realized PnL (-5) from closing the long,
//   (3) release the mkt-2 buy order's margin.
// This is exactly the interleaving a stale per-batch balance cache would corrupt
// by dropping one of the mutations. We derive A's exact end state from the code
// and pin it.
#[test]
fn same_sender_multi_market_settle_balance_exact() {
    let trader_a = addr(1);
    let trader_b = addr(2);

    let (_dir, db) = open_test_db();
    let mut ctx = make_ctx(db.clone());
    fund_native(&ctx, &trader_a, fp(100_000));
    fund_native(&ctx, &trader_b, fp(100_000));

    // --- Batch 1: counterparty B seeds resting liquidity (nothing self-crosses) ---
    //   mkt1 sell@100, mkt1 buy@95 (95 < 100 → no cross), mkt2 sell@50.
    let b1 = NativeExecutor::execute_batch(
        &mut ctx,
        &[
            (trader_b, NativeAction::PlaceOrder(limit_sell(1, 100, 1))),
            (trader_b, NativeAction::PlaceOrder(limit_buy(1, 95, 1))),
            (trader_b, NativeAction::PlaceOrder(limit_sell(2, 50, 1))),
        ],
    );
    for (i, r) in b1.results.iter().enumerate() {
        assert!(r.success, "batch1 order {i} failed: {:?}", r.error);
    }

    // --- Batch 2: A buys mkt1 @100 → crosses B's sell@100, fills @100 (maker px),
    //     opening A long 1@100. The order fully fills, so its 100/20 = 5 margin is
    //     released in full; apply_fill opens the position without touching
    //     `available`. Net effect on A: back to funding, zero order_margin. ---
    let b2 = NativeExecutor::execute_batch(
        &mut ctx,
        &[(trader_a, NativeAction::PlaceOrder(limit_buy(1, 100, 1)))],
    );
    assert!(
        b2.results[0].success,
        "batch2 failed: {:?}",
        b2.results[0].error
    );
    {
        let bal = ctx.positions.get_native_balance(&trader_a).unwrap();
        assert_eq!(bal.available, fp(100_000), "post-open available");
        assert_eq!(bal.order_margin, FixedPoint::ZERO, "post-open margin");
    }

    // --- Batch 3 (UNDER TEST): both actions from A in ONE execute_batch ---
    //   a. sell mkt1 @90 qty1 → crosses B's resting buy@95 → fills @95 (maker px)
    //      → fully closes A's long → realized PnL (95-100)*1 = -5 credited.
    //   b. buy  mkt2 @50 qty1 → crosses B's resting sell@50 → fills @50 → opens long.
    //
    // Derivation (fill price = MAKER price; a fully-filled order releases its FULL
    // reserved margin per native_executor Phase 4):
    //   Phase 2 reserves: sell 90/20 = 4.5, buy 50/20 = 2.5 → order_margin 7.0,
    //     available 100_000 → 99_993.
    //   Phase 4 (per-market; the running sum is order-independent):
    //     mkt1: release +4.5, then PnL credit -5 (close long @95 vs entry 100).
    //     mkt2: release +2.5 (buy fully fills; opening the long does not touch
    //           `available`).
    //   available: 99_993 + 4.5 - 5 + 2.5 = 99_995 (== funding - 5 realized loss).
    //   order_margin: 7.0 - 4.5 - 2.5 = 0 (both orders fully fill → nothing rests).
    let b3 = NativeExecutor::execute_batch(
        &mut ctx,
        &[
            (trader_a, NativeAction::PlaceOrder(limit_sell(1, 90, 1))),
            (trader_a, NativeAction::PlaceOrder(limit_buy(2, 50, 1))),
        ],
    );
    for (i, r) in b3.results.iter().enumerate() {
        assert!(r.success, "batch3 order {i} failed: {:?}", r.error);
    }

    // Assert A's exact end balance via a FRESH PositionManager read from the DB
    // (StateDb is Arc<DB>-backed, so ctx writes are immediately visible).
    let pm = torus_core::position::PositionManager::new(db.clone());
    let bal_a = pm.get_native_balance(&trader_a).unwrap();
    assert_eq!(
        bal_a.available,
        fp(99_995),
        "A available after multi-market settle"
    );
    assert_eq!(
        bal_a.order_margin,
        FixedPoint::ZERO,
        "A order_margin must return to zero — both batch-3 orders fully filled"
    );

    // Positions: mkt1 closed (deleted), mkt2 long 1 @ 50 (maker fill price).
    assert!(
        pm.get_position(&trader_a, 1).unwrap().is_none(),
        "A's mkt1 long must be fully closed"
    );
    let pos2 = pm
        .get_position(&trader_a, 2)
        .unwrap()
        .expect("A must hold a mkt2 position");
    assert!(pos2.is_long, "A's mkt2 position must be long");
    assert_eq!(pos2.size, fp(1), "A's mkt2 position size must be 1");
    assert_eq!(
        pos2.entry_price,
        fp(50),
        "A's mkt2 entry price must be maker 50"
    );
}

// ============================================================================
// Test 12: O2 contract pin — mixed pass/fail batch: per-order isolation,
// failed order consumes NO order-id, state == equivalent singles minus failed
// ============================================================================

#[test]
fn mixed_batch_pass_fail_pins_partial_per_order_contract() {
    // Run A: one batch [ok, margin-fail, ok]. Margin per order = price*qty/20
    // (default max leverage, native_executor.rs:591-599): 100*10/20 = 50.
    // Fund 120: order0 reserves 50 (70 left), order1 needs 100*40/20 = 200
    // -> FAILS, order2 reserves 50 (20 left) — isolation from the failure.
    let (_dir, db) = open_test_db();
    let mut ctx_batch = make_ctx(db.clone());
    let mm = addr(1);
    fund_native(&ctx_batch, &mm, fp(120));

    let batch = NativeAction::PlaceOrderBatch(vec![
        limit_buy(1, 100, 10), // 50 — ok
        limit_buy(2, 100, 40), // 200 — insufficient margin — FAILS
        limit_buy(3, 100, 10), // 50 — ok (must be unaffected by #1's failure)
    ]);
    let id0 = ctx_batch.next_global_order_id;
    let res = NativeExecutor::execute_batch(&mut ctx_batch, &[(mm, batch)]);

    assert_eq!(res.results.len(), 3, "flattened: one result per order");
    assert!(
        res.results[0].success,
        "order 0: {:?}",
        res.results[0].error
    );
    assert!(!res.results[1].success, "order 1 must fail on margin");
    assert!(res.results[1]
        .error
        .as_ref()
        .unwrap()
        .contains("insufficient margin"));
    assert!(
        res.results[2].success,
        "order 2 must be isolated from order 1's failure: {:?}",
        res.results[2].error
    );
    // THE contract detail G4 left untested: the margin-fail `continue`
    // (native_executor.rs:615) precedes ID assignment (:629), so a failed
    // order consumes NO global order id.
    assert_eq!(
        ctx_batch.next_global_order_id,
        id0 + 2,
        "failed order must not consume an order id"
    );

    // Run B: the two GOOD orders as singles on a fresh ctx — end state must match.
    let (_dir2, db2) = open_test_db();
    let mut ctx_singles = make_ctx(db2.clone());
    fund_native(&ctx_singles, &mm, fp(120));
    let singles: Vec<(Address, NativeAction)> = vec![
        (mm, NativeAction::PlaceOrder(limit_buy(1, 100, 10))),
        (mm, NativeAction::PlaceOrder(limit_buy(3, 100, 10))),
    ];
    let res_singles = NativeExecutor::execute_batch(&mut ctx_singles, &singles);
    assert!(res_singles.results.iter().all(|r| r.success));

    let b = ctx_batch.positions.get_native_balance(&mm).unwrap();
    let s = ctx_singles.positions.get_native_balance(&mm).unwrap();
    assert_eq!(
        b.available, s.available,
        "available: batch == singles minus failed"
    );
    assert_eq!(
        b.order_margin, s.order_margin,
        "reserved margin: batch == singles minus failed"
    );
    assert_eq!(
        ctx_batch.next_global_order_id, ctx_singles.next_global_order_id,
        "order-id consumption: batch == singles minus failed"
    );
    assert_eq!(ctx_batch.trade_index, ctx_singles.trade_index);
}

// ============================================================================
// Test 13: G1 — oversize batch is skipped WHOLESALE at exec (deterministic);
// the block continues and a valid sibling action still executes
// ============================================================================

#[test]
fn oversize_batch_skipped_deterministically_sibling_executes() {
    use torus_types::NATIVE_ORDERS_PER_BATCH_CAP;
    let (_dir, db) = open_test_db();
    let mut ctx = make_ctx(db.clone());
    let attacker = addr(1);
    let honest = addr(2);
    fund_native(&ctx, &attacker, fp(100_000_000));
    fund_native(&ctx, &honest, fp(100_000));

    // CAP+1 batch crafted DIRECTLY — models a malicious proposer / direct-push
    // body that never saw the RPC or admit checks.
    let oversize =
        NativeAction::PlaceOrderBatch(vec![limit_buy(1, 100, 1); NATIVE_ORDERS_PER_BATCH_CAP + 1]);
    let sibling = NativeAction::PlaceOrder(limit_buy(2, 50, 1));

    let id0 = ctx.next_global_order_id;
    let res = NativeExecutor::execute_batch(&mut ctx, &[(attacker, oversize), (honest, sibling)]);

    // Whole batch skipped at flatten: only the sibling's result exists, only
    // one order id consumed, zero attacker margin reserved.
    assert_eq!(
        res.results.len(),
        1,
        "oversize batch must not flatten into results"
    );
    assert!(
        res.results[0].success,
        "sibling: {:?}",
        res.results[0].error
    );
    assert_eq!(
        ctx.next_global_order_id,
        id0 + 1,
        "no ids for the skipped batch"
    );
    assert_eq!(
        ctx.positions
            .get_native_balance(&attacker)
            .unwrap()
            .order_margin,
        FixedPoint::ZERO,
        "skipped batch must reserve nothing"
    );

    // Boundary: an AT-CAP batch still executes (flattens to CAP results;
    // book-level per-trader caps may reject some — length is the invariant).
    let at_cap =
        NativeAction::PlaceOrderBatch(vec![limit_buy(1, 100, 1); NATIVE_ORDERS_PER_BATCH_CAP]);
    let res2 = NativeExecutor::execute_batch(&mut ctx, &[(attacker, at_cap)]);
    assert_eq!(res2.results.len(), NATIVE_ORDERS_PER_BATCH_CAP);

    // Empty batch: skipped by the same rule (mirrors validate_batch_size).
    let res3 = NativeExecutor::execute_batch(
        &mut ctx,
        &[(attacker, NativeAction::PlaceOrderBatch(vec![]))],
    );
    assert_eq!(res3.results.len(), 0);
}

// ============================================================================
// Test 15: capped worker model — chunking a market set across a small worker
// cap must produce per-market results identical to today's one-thread-per-market
// behavior (max_workers == market count). Books are independent, so the chunk
// layout cannot change any market's fills, statuses, or next_order_id.
// ============================================================================

/// Owned request params per market so the borrowed `MatchRequest.params` outlive
/// the batches map. Each market: crossing buy (fills), resting buy, resting sell.
fn capped_req_params(n_markets: u64) -> Vec<Vec<PlaceOrderParams>> {
    (1..=n_markets)
        .map(|m| {
            vec![
                limit_buy(m, 100, 5),  // crosses the seeded resting sell → fills
                limit_buy(m, 90, 2),   // rests (best bid 90 < seeded ask)
                limit_sell(m, 110, 1), // rests (110 > best bid)
            ]
        })
        .collect()
}

/// Fresh batches for `n_markets`; each book is seeded with resting sell liquidity
/// so the first request crosses. Order ids are globally unique and deterministic.
fn capped_batches<'a>(
    n_markets: u64,
    req_params: &'a [Vec<PlaceOrderParams>],
) -> HashMap<MarketId, (OrderBook, Vec<MatchRequest<'a>>)> {
    let mut batches = HashMap::new();
    for m in 1..=n_markets {
        let mut book = OrderBook::new(m, fp(1), fp(1));
        book.place_order(limit_sell(m, 100, 10), addr(200), 999);
        let params = &req_params[(m - 1) as usize];
        let requests = vec![
            MatchRequest {
                sender: addr(1),
                params: &params[0],
                order_id: (m * 100 + 1) as OrderId,
            },
            MatchRequest {
                sender: addr(2),
                params: &params[1],
                order_id: (m * 100 + 2) as OrderId,
            },
            MatchRequest {
                sender: addr(3),
                params: &params[2],
                order_id: (m * 100 + 3) as OrderId,
            },
        ];
        batches.insert(m, (book, requests));
    }
    batches
}

fn sorted_by_market(mut v: Vec<MarketBatchResult>) -> Vec<MarketBatchResult> {
    v.sort_by_key(|r| r.market_id);
    v
}

#[test]
fn capped_matches_uncapped_per_market() {
    let n = 16u64;
    let rp = capped_req_params(n);

    let capped =
        MarketWorkerPool::match_parallel_capped(capped_batches(n, &rp), 1000, 3).expect("no panic");
    let full = MarketWorkerPool::match_parallel_capped(capped_batches(n, &rp), 1000, n as usize)
        .expect("no panic");

    let capped = sorted_by_market(capped);
    let full = sorted_by_market(full);
    assert_eq!(capped.len(), full.len(), "same market count");

    let mut total_fills = 0usize;
    for (a, b) in capped.iter().zip(full.iter()) {
        assert_eq!(a.market_id, b.market_id);
        assert_eq!(
            a.next_order_id, b.next_order_id,
            "next_order_id diverges for market {}",
            a.market_id
        );
        assert_eq!(a.results.len(), b.results.len());
        for (ra, rb) in a.results.iter().zip(b.results.iter()) {
            assert_eq!(ra.order_id, rb.order_id);
            assert_eq!(ra.sender, rb.sender);
            assert_eq!(ra.result.status, rb.result.status);
            assert_eq!(ra.result.fills.len(), rb.result.fills.len());
            for (fa, fb) in ra.result.fills.iter().zip(rb.result.fills.iter()) {
                assert_eq!(fa.price, fb.price);
                assert_eq!(fa.quantity, fb.quantity);
            }
            total_fills += ra.result.fills.len();
        }
    }
    assert!(
        total_fills >= n as usize,
        "every market's crossing buy must fill: {total_fills}"
    );
}

#[test]
fn capped_is_deterministic() {
    let n = 16u64;
    let rp = capped_req_params(n);

    let run = || {
        let r =
            MarketWorkerPool::match_parallel_capped(capped_batches(n, &rp), 1000, 3).expect("ok");
        sorted_by_market(r)
            .into_iter()
            .map(|m| {
                (
                    m.market_id,
                    m.next_order_id,
                    m.results
                        .into_iter()
                        .map(|res| (res.order_id, res.result.status, res.result.fills.len()))
                        .collect::<Vec<_>>(),
                )
            })
            .collect::<Vec<_>>()
    };

    assert_eq!(run(), run(), "capped matching must be deterministic");
}

// ============================================================================
// Test 14 (C2): mixed batch API-equivalence pin for the borrowed flatten —
// PlaceOrderBatch + singles + non-place actions must produce outputs identical
// to the hand-flattened action list (result order, action types, success
// flags, errors, gas, balances, order-ids, trades). This is the API-level
// oracle for the clone-removal rewrite (FlatAction<'_> borrowing the caller's
// slice instead of Cow-cloning every action).
// ============================================================================

#[test]
fn mixed_batch_outputs_identical_to_hand_flattened() {
    let mm = addr(1);
    let tkr = addr(2);

    let run = |actions: &[(Address, NativeAction)]| {
        let (dir, db) = open_test_db();
        let mut ctx = make_ctx(db.clone());
        fund_native(&ctx, &mm, fp(1_000_000));
        fund_native(&ctx, &tkr, fp(1_000_000));
        let res = NativeExecutor::execute_batch(&mut ctx, actions);
        (res, ctx, dir)
    };

    // Run A: a PlaceOrderBatch, a failing non-place action, and crossing singles.
    let actions_a: Vec<(Address, NativeAction)> = vec![
        (
            mm,
            NativeAction::PlaceOrderBatch(vec![limit_buy(1, 100, 5), limit_sell(2, 200, 3)]),
        ),
        (tkr, NativeAction::CancelOrder { order_id: 999 }), // fails: not found
        (tkr, NativeAction::PlaceOrder(limit_sell(1, 100, 5))), // crosses batch buy
        (tkr, NativeAction::PlaceOrder(limit_buy(2, 200, 3))), // crosses batch sell
    ];

    // Run B: the SAME executable sequence, batch hand-flattened.
    let actions_b: Vec<(Address, NativeAction)> = vec![
        (mm, NativeAction::PlaceOrder(limit_buy(1, 100, 5))),
        (mm, NativeAction::PlaceOrder(limit_sell(2, 200, 3))),
        (tkr, NativeAction::CancelOrder { order_id: 999 }),
        (tkr, NativeAction::PlaceOrder(limit_sell(1, 100, 5))),
        (tkr, NativeAction::PlaceOrder(limit_buy(2, 200, 3))),
    ];

    let (res_a, ctx_a, _dir_a) = run(&actions_a);
    let (res_b, ctx_b, _dir_b) = run(&actions_b);

    assert_eq!(
        res_a.results.len(),
        5,
        "batch must flatten to one result per order"
    );
    assert_eq!(res_a.results.len(), res_b.results.len());
    for (i, (a, b)) in res_a.results.iter().zip(res_b.results.iter()).enumerate() {
        assert_eq!(a.action_type, b.action_type, "action_type diverges at {i}");
        assert_eq!(
            a.success, b.success,
            "success diverges at {i}: A={a:?} B={b:?}"
        );
        assert_eq!(a.error, b.error, "error diverges at {i}");
    }
    assert_eq!(res_a.total_gas, res_b.total_gas, "gas diverges");
    assert_eq!(ctx_a.next_global_order_id, ctx_b.next_global_order_id);
    assert_eq!(ctx_a.trade_index, ctx_b.trade_index);
    for who in [&mm, &tkr] {
        let bal_a = ctx_a.positions.get_native_balance(who).unwrap();
        let bal_b = ctx_b.positions.get_native_balance(who).unwrap();
        assert_eq!(bal_a.available, bal_b.available, "available diverges");
        assert_eq!(
            bal_a.order_margin, bal_b.order_margin,
            "order_margin diverges"
        );
    }
}
