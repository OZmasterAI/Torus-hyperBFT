//! Chaos tests (task 2.10.8).
//!
//! Tests graceful handling of abnormal conditions: crash recovery, replay
//! determinism, execution ordering, duplicate actions, empty blocks, and
//! mixed success/failure scenarios.
//!
//! Slow tests are marked `#[ignore]`. Fast correctness tests run normally.

mod common;

use alloy_primitives::Address;
use torus_bridge::native_executor::{
    classify_action, sort_native_actions, NativeExecContext, NativeExecutor,
};
use torus_bridge::state_root::compute_native_state_root;
use torus_core::position::PositionManager;
use torus_state::StateDb;
use torus_types::{
    FixedPoint, NativeAction, OracleSubmission, OrderType, PlaceOrderParams, TimeInForce,
    VoteOption, U256,
};

use crate::common::TestHarness;

// ============================================================================
// Helpers
// ============================================================================

fn fp(n: i64) -> FixedPoint {
    TestHarness::fp(n)
}

fn addr(n: u8) -> Address {
    TestHarness::addr(n)
}

/// Generate a deterministic diverse workload for a single block.
fn generate_diverse_block(block: usize) -> Vec<(Address, NativeAction)> {
    let mut actions = Vec::new();
    let base = block * 100;

    // Orders: buy + sell at varying prices
    for i in 0..3 {
        let price_offset = (base + i) % 50;
        actions.push((
            addr(((base + i) % 10 + 1) as u8),
            NativeAction::PlaceOrder(PlaceOrderParams {
                market_id: 1,
                is_buy: i % 2 == 0,
                price: fp(49500 + price_offset as i64 * 10),
                quantity: fp(1),
                order_type: OrderType::Limit,
                time_in_force: TimeInForce::GTC,
                reduce_only: false,
                client_order_id: None,
            }),
        ));
    }

    // Cancel attempt (may fail)
    actions.push((
        addr(1),
        NativeAction::CancelOrder {
            order_id: (base as u128 + 1) % 200 + 1,
        },
    ));

    // Oracle submission
    actions.push((
        addr(11),
        NativeAction::SubmitOraclePrices(OracleSubmission {
            prices: vec![(1, fp(50000 + block as i64))],
            timestamp: 1_700_000_000 + block as u64,
        }),
    ));

    // Lockbox transfer (deposit to native)
    actions.push((
        addr(12),
        NativeAction::TransferToPerp {
            amount: U256::from(0u64),
        },
    ));

    actions
}

// ============================================================================
// Slow tests — #[ignore]
// ============================================================================

/// State recovery after crash: execute 10 blocks, drop all in-memory state,
/// reopen StateDb from same path, reconstruct managers, execute block 11.
/// Verify: consistent state, no data loss.
#[test]
#[ignore]
fn test_state_recovery_after_crash() {
    let tmp_dir = tempfile::TempDir::new().unwrap();
    let path = tmp_dir.path().to_path_buf();

    let trader_a = addr(1);
    let trader_b = addr(2);
    let market = 1u64;
    let initial = fp(1_000_000);

    // Phase 1: Execute 10 blocks of trading activity, then "crash" (drop everything).
    let state_root_before;
    {
        let state_db = StateDb::open(&path).unwrap();
        let positions = PositionManager::new(state_db.clone());

        // Fund traders.
        let mut bal_a = positions.get_native_balance(&trader_a).unwrap();
        bal_a.available = bal_a.available + initial;
        positions.put_native_balance(&trader_a, &bal_a).unwrap();

        let mut bal_b = positions.get_native_balance(&trader_b).unwrap();
        bal_b.available = bal_b.available + initial;
        positions.put_native_balance(&trader_b, &bal_b).unwrap();

        let mut ctx = NativeExecContext::new(
            state_db.clone(),
            1,
            1_700_000_001,
            0,
            100,
            100,
            Address::ZERO,
            Address::ZERO,
            Address::ZERO,
        );

        // Execute 10 blocks with matching buy/sell orders.
        for block in 0..10 {
            let buy = NativeAction::PlaceOrder(PlaceOrderParams {
                market_id: market,
                is_buy: true,
                price: fp(50000),
                quantity: fp(1),
                order_type: OrderType::Limit,
                time_in_force: TimeInForce::GTC,
                reduce_only: false,
                client_order_id: None,
            });
            let sell = NativeAction::PlaceOrder(PlaceOrderParams {
                market_id: market,
                is_buy: false,
                price: fp(50000),
                quantity: fp(1),
                order_type: OrderType::Limit,
                time_in_force: TimeInForce::GTC,
                reduce_only: false,
                client_order_id: None,
            });
            NativeExecutor::execute_batch(&mut ctx, &[(trader_a, buy), (trader_b, sell)]);
            ctx.block_height += 1;
            ctx.timestamp += 1;
            let _ = block;
        }

        state_root_before = compute_native_state_root(&state_db).unwrap();

        // "Crash" — drop state_db, positions, ctx. Only the file system persists.
    }

    // Phase 2: Reopen from disk. Verify state survived.
    {
        let state_db = StateDb::open(&path).unwrap();
        let positions = PositionManager::new(state_db.clone());

        // State root should be identical.
        let state_root_after = compute_native_state_root(&state_db).unwrap();
        assert_eq!(
            state_root_before, state_root_after,
            "State root must survive crash"
        );

        // Trader balances should survive.
        let bal_a = positions.get_native_balance(&trader_a).unwrap();
        assert!(
            bal_a.available > FixedPoint::ZERO,
            "trader_a balance should survive crash"
        );

        let bal_b = positions.get_native_balance(&trader_b).unwrap();
        assert!(
            bal_b.available > FixedPoint::ZERO,
            "trader_b balance should survive crash"
        );

        // Positions should survive.
        let pos_a = positions
            .get_position(&trader_a, market)
            .unwrap()
            .expect("trader_a position should survive");
        assert_eq!(pos_a.size, fp(10), "A should have accumulated 10 units");

        // Execute block 11 successfully on the recovered state.
        let mut ctx = NativeExecContext::new(
            state_db.clone(),
            11,
            1_700_000_011,
            0,
            100,
            100,
            Address::ZERO,
            Address::ZERO,
            Address::ZERO,
        );

        let buy = NativeAction::PlaceOrder(PlaceOrderParams {
            market_id: market,
            is_buy: true,
            price: fp(50000),
            quantity: fp(1),
            order_type: OrderType::Limit,
            time_in_force: TimeInForce::GTC,
            reduce_only: false,
            client_order_id: None,
        });
        let sell = NativeAction::PlaceOrder(PlaceOrderParams {
            market_id: market,
            is_buy: false,
            price: fp(50000),
            quantity: fp(1),
            order_type: OrderType::Limit,
            time_in_force: TimeInForce::GTC,
            reduce_only: false,
            client_order_id: None,
        });
        let batch = NativeExecutor::execute_batch(&mut ctx, &[(trader_a, buy), (trader_b, sell)]);
        assert!(batch.results[0].success, "Block 11 buy should succeed");
        assert!(batch.results[1].success, "Block 11 sell should succeed");

        // Position grows to 11.
        let pos_a = positions
            .get_position(&trader_a, market)
            .unwrap()
            .expect("position after block 11");
        assert_eq!(pos_a.size, fp(11));
    }
}

/// Replay determinism: execute 20 blocks with diverse workload,
/// record state root after each block, reset to genesis, replay all 20,
/// verify all 20 state roots match byte-for-byte.
#[test]
#[ignore]
fn test_replay_determinism() {
    let num_blocks = 20;
    let initial_funding = fp(10_000_000);
    let num_traders = 12; // traders 1-10 for orders, 11 for oracle, 12 for lockbox

    // Helper: run the full workload and return state roots after each block.
    let run_workload = || -> Vec<alloy_primitives::B256> {
        let tmp_dir = tempfile::TempDir::new().unwrap();
        let state_db = StateDb::open(tmp_dir.path()).unwrap();
        let positions = PositionManager::new(state_db.clone());

        // Fund all traders.
        for i in 1..=num_traders {
            let mut bal = positions.get_native_balance(&addr(i)).unwrap();
            bal.available = bal.available + initial_funding;
            positions.put_native_balance(&addr(i), &bal).unwrap();
        }

        let mut ctx = NativeExecContext::new(
            state_db.clone(),
            1,
            1_700_000_001,
            0,
            100,
            100,
            Address::ZERO,
            Address::ZERO,
            Address::ZERO,
        );

        let mut roots = Vec::with_capacity(num_blocks);

        for block in 0..num_blocks {
            let actions = generate_diverse_block(block);
            NativeExecutor::execute_batch(&mut ctx, &actions);

            // Process governance + fee distribution (deterministic block-level ops).
            NativeExecutor::process_governance(&mut ctx);
            NativeExecutor::distribute_fees(&mut ctx, 0);

            let root = compute_native_state_root(&state_db).unwrap();
            roots.push(root);

            ctx.block_height += 1;
            ctx.timestamp += 1;
        }

        roots
    };

    // Run 1: record state roots.
    let roots_1 = run_workload();
    assert_eq!(roots_1.len(), num_blocks);

    // Run 2: replay on fresh state.
    let roots_2 = run_workload();
    assert_eq!(roots_2.len(), num_blocks);

    // Verify all state roots match byte-for-byte.
    for (i, (r1, r2)) in roots_1.iter().zip(roots_2.iter()).enumerate() {
        assert_eq!(
            r1, r2,
            "State root mismatch at block {i}: run1={r1} != run2={r2}"
        );
    }
}

// ============================================================================
// Fast tests — no #[ignore]
// ============================================================================

/// Phase A cross-cutting determinism gate: under REAL native execution (NativeExecutor on a
/// NativeStateOverlay, committed via `flush_with_native_trie` — the consensus commit path), the
/// incrementally-maintained native bucketed-Merkle root must stay byte-identical to the full-scan
/// oracle after EVERY block. This is the compile-time twin of the runtime oracle: a HARD CI failure
/// on any divergence. Unlike the torus-state unit corpus (synthetic (cf,key) writes), this exercises
/// the actual native-CF write patterns of orders / cancels / oracle / lockbox / governance / fees.
#[test]
fn native_incremental_root_matches_full_scan_under_real_execution() {
    use torus_state::native_trie::{
        build_native_trie_to_cf, native_root_full, persisted_native_root,
    };
    use torus_state::NativeStateOverlay;

    let tmp = tempfile::TempDir::new().unwrap();
    let state_db = StateDb::open(tmp.path()).unwrap();

    // Fund traders, then build the native trie base over the funded state.
    let positions = PositionManager::new(state_db.clone());
    for i in 1..=12u8 {
        let mut bal = positions.get_native_balance(&addr(i)).unwrap();
        bal.available = bal.available + fp(10_000_000);
        positions.put_native_balance(&addr(i), &bal).unwrap();
    }
    build_native_trie_to_cf(&state_db).unwrap();
    assert_eq!(
        persisted_native_root(&state_db).unwrap(),
        native_root_full(&state_db).unwrap(),
        "post-migration: persisted native root must equal full scan"
    );

    for block in 0..15usize {
        // Mirror the consensus native block: execute on an overlay, then flush + maintain the trie
        // in one atomic batch.
        let overlay = NativeStateOverlay::new(state_db.clone());
        let mut ctx = NativeExecContext::new(
            overlay.clone(),
            (block + 1) as u64,
            1_700_000_001 + block as u64,
            0,
            100,
            100,
            Address::ZERO,
            Address::ZERO,
            Address::ZERO,
        );
        let actions = generate_diverse_block(block);
        NativeExecutor::execute_batch(&mut ctx, &actions);
        NativeExecutor::process_governance(&mut ctx);
        NativeExecutor::distribute_fees(&mut ctx, 0);
        overlay.flush_with_native_trie(&state_db).unwrap();

        assert_eq!(
            persisted_native_root(&state_db).unwrap(),
            native_root_full(&state_db).unwrap(),
            "block {block}: incremental native root != full-scan oracle (real execution)"
        );
    }
}

/// Execution ordering: verify tech-req section 2.2 ordering is enforced.
/// Cancels before new orders, non-GTC before GTC.
#[test]
fn test_execution_ordering_enforced() {
    // Build a mixed set of actions in "random" submission order.
    let actions: Vec<(Address, NativeAction)> = vec![
        // GTC limit order (should be post-EVM, category=GtcOrder)
        (
            addr(1),
            NativeAction::PlaceOrder(PlaceOrderParams {
                market_id: 1,
                is_buy: true,
                price: fp(50000),
                quantity: fp(1),
                order_type: OrderType::Limit,
                time_in_force: TimeInForce::GTC,
                reduce_only: false,
                client_order_id: None,
            }),
        ),
        // Cancel (should be pre-EVM, category=Cancellation)
        (addr(2), NativeAction::CancelOrder { order_id: 1 }),
        // IOC order (should be pre-EVM, category=NonGtcOrder)
        (
            addr(3),
            NativeAction::PlaceOrder(PlaceOrderParams {
                market_id: 1,
                is_buy: false,
                price: fp(50000),
                quantity: fp(1),
                order_type: OrderType::Limit,
                time_in_force: TimeInForce::IOC,
                reduce_only: false,
                client_order_id: None,
            }),
        ),
        // Oracle submission (should be post-EVM, category=Oracle)
        (
            addr(4),
            NativeAction::SubmitOraclePrices(OracleSubmission {
                prices: vec![(1, fp(50000))],
                timestamp: 1_700_000_000,
            }),
        ),
        // Market order (should be pre-EVM, category=NonGtcOrder)
        (
            addr(5),
            NativeAction::PlaceOrder(PlaceOrderParams {
                market_id: 1,
                is_buy: true,
                price: fp(999_999),
                quantity: fp(1),
                order_type: OrderType::Market,
                time_in_force: TimeInForce::IOC,
                reduce_only: false,
                client_order_id: None,
            }),
        ),
        // Lockbox deposit (should be post-EVM, category=Lockbox)
        (
            addr(6),
            NativeAction::TransferToPerp {
                amount: U256::from(100u64),
            },
        ),
        // Governance vote (should be post-EVM, category=Governance)
        (
            addr(7),
            NativeAction::Vote {
                proposal_id: 1,
                option: VoteOption::Yes,
            },
        ),
        // Another cancel (pre-EVM)
        (addr(8), NativeAction::CancelOrder { order_id: 2 }),
    ];

    let (pre_evm, post_evm) = sort_native_actions(&actions);

    // Pre-EVM group: cancellations first, then non-GTC orders.
    // Should have: 2 cancels + 2 non-GTC (IOC + Market) = 4 actions.
    assert_eq!(pre_evm.len(), 4, "pre-EVM should have 4 actions");

    // Verify ordering within pre-EVM: cancellations (category 0) before non-GTC (category 1).
    let pre_categories: Vec<_> = pre_evm.iter().map(|(_, a)| classify_action(a)).collect();
    for window in pre_categories.windows(2) {
        assert!(
            window[0] <= window[1],
            "Pre-EVM actions not sorted: {:?} > {:?}",
            window[0],
            window[1]
        );
    }

    // Post-EVM group: GTC orders, then lockbox, oracle, governance.
    // Should have: 1 GTC + 1 lockbox + 1 oracle + 1 governance = 4 actions.
    assert_eq!(post_evm.len(), 4, "post-EVM should have 4 actions");

    let post_categories: Vec<_> = post_evm.iter().map(|(_, a)| classify_action(a)).collect();
    for window in post_categories.windows(2) {
        assert!(
            window[0] <= window[1],
            "Post-EVM actions not sorted: {:?} > {:?}",
            window[0],
            window[1]
        );
    }
}

/// Duplicate action handling: same cancel action twice -> second fails,
/// no double-execution or panic.
#[test]
fn test_duplicate_action_handling() {
    let h = TestHarness::new();
    let trader = addr(1);
    let market = 1u64;

    h.fund_native(&trader, fp(100_000));

    let mut ctx = h.exec_context(1);

    // Place an order to get a valid order_id.
    let place = NativeAction::PlaceOrder(PlaceOrderParams {
        market_id: market,
        is_buy: true,
        price: fp(50000),
        quantity: fp(1),
        order_type: OrderType::Limit,
        time_in_force: TimeInForce::GTC,
        reduce_only: false,
        client_order_id: None,
    });
    let r = NativeExecutor::execute(&mut ctx, &trader, &place);
    assert!(r.success);

    // Cancel the same order twice in one batch.
    let cancel = NativeAction::CancelOrder { order_id: 1 };
    let batch =
        NativeExecutor::execute_batch(&mut ctx, &[(trader, cancel.clone()), (trader, cancel)]);

    assert_eq!(batch.results.len(), 2);
    // First cancel succeeds.
    assert!(batch.results[0].success, "First cancel should succeed");
    // Second cancel fails (order already gone) — but no panic.
    assert!(
        !batch.results[1].success,
        "Second cancel should fail gracefully"
    );
}

/// Empty block: zero actions, zero EVM txs -> block valid, state root computable,
/// liquidation check runs (finds nothing), fee split with zero fees works.
#[test]
fn test_empty_block() {
    let h = TestHarness::new();
    let mut ctx = h.exec_context(1);

    // Execute empty batch.
    let batch = NativeExecutor::execute_batch(&mut ctx, &[]);
    assert_eq!(batch.results.len(), 0);
    assert_eq!(batch.total_gas, 0);

    // Block-level operations should run without error.
    let gov_results = NativeExecutor::process_governance(&mut ctx);
    // No pending proposals, so empty results (not an error).
    let _ = gov_results;

    // Fee distribution with zero fees.
    let fee_result = NativeExecutor::distribute_fees(&mut ctx, 0);
    assert!(
        fee_result.success,
        "Zero-fee distribution should succeed: {:?}",
        fee_result.error
    );

    // Epoch boundary check (block 1 with epoch_length 100 is not a boundary).
    let epoch_result = NativeExecutor::process_epoch_boundary(&mut ctx);
    assert!(
        epoch_result.is_none(),
        "Block 1 should not be an epoch boundary"
    );

    // State root should be computable.
    let root = compute_native_state_root(&h.state_db).unwrap();
    // Empty DB gives EMPTY_ROOT_HASH, non-empty gives a keccak hash.
    // Either way, it should not panic.
    assert_eq!(root.len(), 32, "State root should be 32 bytes");
}

/// All actions fail: every action in block will fail (zero balance, non-existent IDs, etc.)
/// Verify: no panics, state unchanged, block still valid.
#[test]
fn test_all_actions_fail() {
    let h = TestHarness::new();
    let unfunded = addr(99); // no balance

    // Record initial state root.
    let root_before = compute_native_state_root(&h.state_db).unwrap();

    let mut ctx = h.exec_context(1);

    let failing_actions: Vec<(Address, NativeAction)> = vec![
        // Cancel non-existent order
        (unfunded, NativeAction::CancelOrder { order_id: 99999 }),
        // Cancel all on empty book
        (
            unfunded,
            NativeAction::CancelAllOrders { market_id: Some(1) },
        ),
        // Withdraw with zero balance
        (
            unfunded,
            NativeAction::TransferToSpot {
                amount: U256::from(1000u64),
            },
        ),
        // Claim rewards with no rewards
        (unfunded, NativeAction::ClaimRewards),
    ];

    let batch = NativeExecutor::execute_batch(&mut ctx, &failing_actions);
    assert_eq!(batch.results.len(), 4);

    // Cancel of non-existent order should fail.
    assert!(!batch.results[0].success);

    // CancelAll on empty book succeeds (no-op).
    // This is correct behavior — cancelling nothing is not an error.

    // Withdraw with zero balance should fail.
    assert!(!batch.results[2].success);

    // Claim rewards with no rewards should fail.
    assert!(!batch.results[3].success);

    // State root should be unchanged (only CancelAll might have run as a no-op).
    let root_after = compute_native_state_root(&h.state_db).unwrap();
    assert_eq!(
        root_before, root_after,
        "State should not change when all actions fail"
    );
}

/// Mixed success/failure: block with 3 valid + 2 invalid actions.
/// Valid ones succeed, invalid ones fail, state reflects only successful ones.
#[test]
fn test_mixed_success_failure() {
    let h = TestHarness::new();
    let buyer = addr(1);
    let seller = addr(2);
    let unfunded = addr(99);
    let market = 1u64;

    h.fund_native(&buyer, fp(1_000_000));
    h.fund_native(&seller, fp(1_000_000));

    let mut ctx = h.exec_context(1);

    let actions: Vec<(Address, NativeAction)> = vec![
        // VALID: buyer places buy order at 50000
        (
            buyer,
            NativeAction::PlaceOrder(PlaceOrderParams {
                market_id: market,
                is_buy: true,
                price: fp(50000),
                quantity: fp(1),
                order_type: OrderType::Limit,
                time_in_force: TimeInForce::GTC,
                reduce_only: false,
                client_order_id: None,
            }),
        ),
        // INVALID: cancel non-existent order
        (unfunded, NativeAction::CancelOrder { order_id: 9999 }),
        // VALID: buyer places another buy order at 49000
        (
            buyer,
            NativeAction::PlaceOrder(PlaceOrderParams {
                market_id: market,
                is_buy: true,
                price: fp(49000),
                quantity: fp(2),
                order_type: OrderType::Limit,
                time_in_force: TimeInForce::GTC,
                reduce_only: false,
                client_order_id: None,
            }),
        ),
        // INVALID: withdraw with zero balance
        (
            unfunded,
            NativeAction::TransferToSpot {
                amount: U256::from(1000u64),
            },
        ),
        // VALID: seller matches buyer's order at 50000
        (
            seller,
            NativeAction::PlaceOrder(PlaceOrderParams {
                market_id: market,
                is_buy: false,
                price: fp(50000),
                quantity: fp(1),
                order_type: OrderType::Limit,
                time_in_force: TimeInForce::GTC,
                reduce_only: false,
                client_order_id: None,
            }),
        ),
    ];

    let batch = NativeExecutor::execute_batch(&mut ctx, &actions);
    assert_eq!(batch.results.len(), 5);

    // Check individual results.
    assert!(batch.results[0].success, "First buy should succeed");
    assert!(!batch.results[1].success, "Cancel should fail");
    assert!(batch.results[2].success, "Second buy should succeed");
    assert!(!batch.results[3].success, "Withdraw should fail");
    assert!(batch.results[4].success, "Sell should succeed");

    // State should reflect only successful actions:
    // - Buy at 50000 matched by sell -> position created
    // - Buy at 49000 resting on book
    let book = ctx.order_books.get(&market).unwrap();
    assert_eq!(book.order_count(), 1, "One resting order (buy at 49000)");
    assert_eq!(book.best_bid(), Some(fp(49000)));

    // Buyer and seller should have positions from the match.
    let pos_buyer = h
        .positions
        .get_position(&buyer, market)
        .unwrap()
        .expect("buyer position");
    assert!(pos_buyer.is_long);
    assert_eq!(pos_buyer.size, fp(1));

    let pos_seller = h
        .positions
        .get_position(&seller, market)
        .unwrap()
        .expect("seller position");
    assert!(!pos_seller.is_long);
    assert_eq!(pos_seller.size, fp(1));

    book.verify_invariants();
}
