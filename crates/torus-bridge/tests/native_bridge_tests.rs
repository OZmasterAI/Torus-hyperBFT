//! Determinism and ordering tests for the native action execution pipeline.
//!
//! Task 2.5.5: Tests for native + EVM block execution, ordering, and state root consistency.

use alloy_primitives::{Address, B256, U256};
use revm::state::AccountInfo;

use torus_bridge::native_executor::{
    classify_action, sort_native_actions, ActionCategory, NativeExecContext, NativeExecutor,
};
use torus_bridge::state_root::compute_native_state_root;
use torus_bridge::BlockValidator;
use torus_evm::{EvmExecutor, TORUS_CHAIN_ID};
use torus_state::StateDb;
use torus_types::{
    FixedPoint, NativeAction, OrderType, PlaceOrderParams, TimeInForce, TorusBlock,
    TorusBlockHeader,
};

const KECCAK_EMPTY: B256 = B256::new([
    0xc5, 0xd2, 0x46, 0x01, 0x86, 0xf7, 0x23, 0x3c, 0x92, 0x7e, 0x7d, 0xb2, 0xdc, 0xc7, 0x03, 0xc0,
    0xe5, 0x00, 0xb6, 0x53, 0xca, 0x82, 0x27, 0x3b, 0x7b, 0xfa, 0xd8, 0x04, 0x5d, 0x85, 0xa4, 0x70,
]);

// ---- Test helpers ----

fn open_test_db() -> (tempfile::TempDir, StateDb) {
    let dir = tempfile::tempdir().expect("create temp dir");
    let db = StateDb::open(dir.path()).expect("open db");
    (dir, db)
}

fn addr(n: u8) -> Address {
    Address::new([n; 20])
}

/// Finding #18 — single seam that absorbs the `sort_native_actions` signature
/// change (owned `(Address, NativeAction)` -> borrowed `(Address, &NativeAction)`),
/// so every ordering test drives the SAME entry point. The refactor changes ONLY
/// this wrapper's body; the ordering oracle assertions are unchanged.
fn call_sort(
    actions: &[(Address, NativeAction)],
) -> (Vec<(Address, NativeAction)>, Vec<(Address, NativeAction)>) {
    let refs: Vec<(Address, &NativeAction)> = actions.iter().map(|(s, a)| (*s, a)).collect();
    sort_native_actions(&refs)
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

/// Fund a trader with native balance for order margin (Batch IJ: FIX 2 requires margin).
fn fund_native(ctx: &NativeExecContext, trader: &Address, amount: FixedPoint) {
    use torus_core::position::NativeBalance;
    let bal = NativeBalance {
        available: amount,
        order_margin: FixedPoint::ZERO,
    };
    ctx.positions.put_native_balance(trader, &bal).unwrap();
}

// ============================================================================
// Test: Action classification
// ============================================================================

#[test]
fn classify_cancel_order() {
    let action = NativeAction::CancelOrder { order_id: 42 };
    assert_eq!(classify_action(&action), ActionCategory::Cancellation);
}

#[test]
fn classify_cancel_all() {
    let action = NativeAction::CancelAllOrders { market_id: None };
    assert_eq!(classify_action(&action), ActionCategory::Cancellation);
}

#[test]
fn classify_gtc_limit_order() {
    let action = NativeAction::PlaceOrder(PlaceOrderParams {
        market_id: 1,
        is_buy: true,
        price: fp(50_000),
        quantity: fp(1),
        order_type: OrderType::Limit,
        time_in_force: TimeInForce::GTC,
        reduce_only: false,
        client_order_id: None,
    });
    assert_eq!(classify_action(&action), ActionCategory::GtcOrder);
}

#[test]
fn classify_ioc_order() {
    let action = NativeAction::PlaceOrder(PlaceOrderParams {
        market_id: 1,
        is_buy: true,
        price: fp(50_000),
        quantity: fp(1),
        order_type: OrderType::Limit,
        time_in_force: TimeInForce::IOC,
        reduce_only: false,
        client_order_id: None,
    });
    assert_eq!(classify_action(&action), ActionCategory::NonGtcOrder);
}

#[test]
fn classify_market_order() {
    let action = NativeAction::PlaceOrder(PlaceOrderParams {
        market_id: 1,
        is_buy: true,
        price: fp(0),
        quantity: fp(1),
        order_type: OrderType::Market,
        time_in_force: TimeInForce::IOC,
        reduce_only: false,
        client_order_id: None,
    });
    assert_eq!(classify_action(&action), ActionCategory::NonGtcOrder);
}

#[test]
fn classify_delegate() {
    let action = NativeAction::Delegate {
        validator: addr(1),
        amount: U256::from(1000u64),
    };
    assert_eq!(classify_action(&action), ActionCategory::Staking);
}

#[test]
fn classify_oracle() {
    let action = NativeAction::SubmitOraclePrices(torus_types::OracleSubmission {
        prices: vec![(1, fp(50_000))],
        timestamp: 1000,
    });
    assert_eq!(classify_action(&action), ActionCategory::Oracle);
}

#[test]
fn classify_governance_vote() {
    let action = NativeAction::Vote {
        proposal_id: 1,
        option: torus_types::VoteOption::Yes,
    };
    assert_eq!(classify_action(&action), ActionCategory::Governance);
}

#[test]
fn classify_lockbox_transfer() {
    let action = NativeAction::TransferToPerp {
        amount: U256::from(1000u64),
    };
    assert_eq!(classify_action(&action), ActionCategory::Lockbox);
}

// ============================================================================
// Test: Execution ordering
// ============================================================================

#[test]
fn sort_cancels_before_orders() {
    let actions = vec![
        (
            addr(1),
            NativeAction::PlaceOrder(PlaceOrderParams {
                market_id: 1,
                is_buy: true,
                price: fp(100),
                quantity: fp(1),
                order_type: OrderType::Limit,
                time_in_force: TimeInForce::GTC,
                reduce_only: false,
                client_order_id: None,
            }),
        ),
        (addr(2), NativeAction::CancelOrder { order_id: 1 }),
        (
            addr(3),
            NativeAction::PlaceOrder(PlaceOrderParams {
                market_id: 1,
                is_buy: false,
                price: fp(200),
                quantity: fp(1),
                order_type: OrderType::Limit,
                time_in_force: TimeInForce::IOC,
                reduce_only: false,
                client_order_id: None,
            }),
        ),
    ];

    let (pre_evm, post_evm) = call_sort(&actions);

    // Pre-EVM: cancel (Cancellation) + IOC order (NonGtcOrder)
    assert_eq!(pre_evm.len(), 2);
    assert!(matches!(pre_evm[0].1, NativeAction::CancelOrder { .. }));
    assert!(matches!(pre_evm[1].1, NativeAction::PlaceOrder(_)));

    // Post-EVM: GTC limit order
    assert_eq!(post_evm.len(), 1);
    assert!(matches!(post_evm[0].1, NativeAction::PlaceOrder(_)));
}

#[test]
fn sort_mixed_actions_correct_category_order() {
    let actions = vec![
        (
            addr(1),
            NativeAction::Delegate {
                validator: addr(10),
                amount: U256::from(100u64),
            },
        ),
        (
            addr(2),
            NativeAction::SubmitOraclePrices(torus_types::OracleSubmission {
                prices: vec![(1, fp(500))],
                timestamp: 1000,
            }),
        ),
        (addr(3), NativeAction::CancelOrder { order_id: 5 }),
        (
            addr(4),
            NativeAction::TransferToPerp {
                amount: U256::from(50u64),
            },
        ),
    ];

    let (pre_evm, post_evm) = call_sort(&actions);

    // Pre-EVM: only the cancel
    assert_eq!(pre_evm.len(), 1);
    assert!(matches!(pre_evm[0].1, NativeAction::CancelOrder { .. }));

    // Post-EVM: lockbox, oracle, staking (in category order)
    assert_eq!(post_evm.len(), 3);
    // Lockbox < Oracle < Staking in ActionCategory ordering
    assert!(matches!(post_evm[0].1, NativeAction::TransferToPerp { .. }));
    assert!(matches!(post_evm[1].1, NativeAction::SubmitOraclePrices(_)));
    assert!(matches!(post_evm[2].1, NativeAction::Delegate { .. }));
}

// ============================================================================
// Test: NativeExecutor batch execution
// ============================================================================

#[test]
fn execute_batch_continues_on_failure() {
    let (_dir, state_db) = open_test_db();
    let mut ctx = make_ctx(state_db);

    // Cancel a non-existent order (should fail) + delegate (should also fail since
    // no validator registered, but won't panic — just returns error result)
    let actions = vec![
        (addr(1), NativeAction::CancelOrder { order_id: 999 }),
        (addr(2), NativeAction::ClaimRewards),
    ];

    let result = NativeExecutor::execute_batch(&mut ctx, &actions);

    // Both actions attempted — batch did not stop on first failure.
    assert_eq!(result.results.len(), 2);
    assert!(
        !result.results[0].success,
        "cancel of non-existent should fail"
    );
    assert!(
        !result.results[1].success,
        "claim with no rewards should fail"
    );
}

#[test]
fn execute_place_order_succeeds() {
    let (_dir, state_db) = open_test_db();
    let mut ctx = make_ctx(state_db);
    fund_native(&ctx, &addr(1), fp(1_000_000));

    let action = NativeAction::PlaceOrder(PlaceOrderParams {
        market_id: 1,
        is_buy: true,
        price: fp(50_000),
        quantity: fp(1),
        order_type: OrderType::Limit,
        time_in_force: TimeInForce::GTC,
        reduce_only: false,
        client_order_id: None,
    });

    let result = NativeExecutor::execute(&mut ctx, &addr(1), &action);
    assert!(result.success, "place_order should succeed");
    assert_eq!(result.action_type, "place_order");
    assert!(result.gas_used > 0);
}

#[test]
fn execute_cancel_nonexistent_order_fails() {
    let (_dir, state_db) = open_test_db();
    let mut ctx = make_ctx(state_db);

    let action = NativeAction::CancelOrder { order_id: 12345 };
    let result = NativeExecutor::execute(&mut ctx, &addr(1), &action);
    assert!(!result.success);
    assert!(result.error.is_some());
}

// ============================================================================
// Test: Determinism — same block → same state root
// ============================================================================

#[test]
fn deterministic_native_execution() {
    // Execute the same actions on two independent state instances.
    let (_dir1, db1) = open_test_db();
    let (_dir2, db2) = open_test_db();

    let actions = vec![
        (
            addr(1),
            NativeAction::PlaceOrder(PlaceOrderParams {
                market_id: 1,
                is_buy: true,
                price: fp(100),
                quantity: fp(10),
                order_type: OrderType::Limit,
                time_in_force: TimeInForce::GTC,
                reduce_only: false,
                client_order_id: None,
            }),
        ),
        (
            addr(2),
            NativeAction::PlaceOrder(PlaceOrderParams {
                market_id: 1,
                is_buy: false,
                price: fp(100),
                quantity: fp(5),
                order_type: OrderType::Limit,
                time_in_force: TimeInForce::GTC,
                reduce_only: false,
                client_order_id: None,
            }),
        ),
    ];

    let mut ctx1 = make_ctx(db1.clone());
    let mut ctx2 = make_ctx(db2.clone());
    fund_native(&ctx1, &addr(1), fp(1_000_000));
    fund_native(&ctx1, &addr(2), fp(1_000_000));
    fund_native(&ctx2, &addr(1), fp(1_000_000));
    fund_native(&ctx2, &addr(2), fp(1_000_000));

    let result1 = NativeExecutor::execute_batch(&mut ctx1, &actions);
    let result2 = NativeExecutor::execute_batch(&mut ctx2, &actions);

    // Same results.
    assert_eq!(result1.results.len(), result2.results.len());
    for (r1, r2) in result1.results.iter().zip(result2.results.iter()) {
        assert_eq!(r1.success, r2.success);
        assert_eq!(r1.action_type, r2.action_type);
    }

    // Same native state root.
    let root1 = compute_native_state_root(&db1).unwrap();
    let root2 = compute_native_state_root(&db2).unwrap();
    assert_eq!(
        root1, root2,
        "deterministic execution should yield identical native roots"
    );
}

// ============================================================================
// Test: EVM-only block still works (no native actions)
// ============================================================================

#[test]
fn empty_native_actions_evm_only() {
    let (_dir, state_db) = open_test_db();
    let evm_executor = EvmExecutor::new(TORUS_CHAIN_ID);
    let validator = BlockValidator::new(TORUS_CHAIN_ID, 100, 4, Address::ZERO, Address::ZERO);

    let block = TorusBlock {
        header: TorusBlockHeader {
            height: 1,
            timestamp: 1000,
            proposer: addr(99),
            state_root: B256::ZERO,
            receipts_root: B256::ZERO,
            logs_bloom: alloy_primitives::Bloom::ZERO,
            evm_gas_used: 0,
            evm_fee_revenue: 0,
            evm_gas_limit: 30_000_000,
            native_action_count: 0,
            evm_tx_count: 0,
            base_fee_per_gas: 1_000_000_000,
            epoch: 0,
            validator_set_hash: B256::ZERO,
            sig_attestation: [0u8; 64],
        },
        native_actions: vec![],
        evm_transactions: vec![],
        core_writer_actions: vec![],
    };

    // FIX CONS-PF-02: validate_block_with_native now recovers senders from
    // SignedNativeActions in the block (no separate senders parameter).
    let result = validator.validate_block_with_native(&block, &state_db, &evm_executor);

    // Will fail on state root mismatch (B256::ZERO != computed root), which is
    // expected — the test verifies the pipeline runs without panic.
    assert!(result.is_err());
    if let Err(e) = &result {
        // Should be a state root mismatch, not a crash.
        assert!(
            e.to_string().contains("state root mismatch"),
            "expected state root mismatch, got: {e}"
        );
    }
}

// ============================================================================
// Test: Native-only block (no EVM txs)
// ============================================================================

#[test]
fn native_only_block_no_evm() {
    let (_dir, state_db) = open_test_db();
    let mut ctx = make_ctx(state_db);
    fund_native(&ctx, &addr(1), fp(1_000_000));

    // Execute native actions with no EVM transactions.
    let actions = vec![(
        addr(1),
        NativeAction::PlaceOrder(PlaceOrderParams {
            market_id: 1,
            is_buy: true,
            price: fp(1000),
            quantity: fp(5),
            order_type: OrderType::Limit,
            time_in_force: TimeInForce::GTC,
            reduce_only: false,
            client_order_id: None,
        }),
    )];

    let result = NativeExecutor::execute_batch(&mut ctx, &actions);
    assert_eq!(result.results.len(), 1);
    assert!(result.results[0].success);
}

// ============================================================================
// Test: Place + Cancel ordering
// ============================================================================

#[test]
fn cancel_executes_before_new_orders() {
    let (_dir, state_db) = open_test_db();
    let mut ctx = make_ctx(state_db);
    fund_native(&ctx, &addr(1), fp(1_000_000));
    fund_native(&ctx, &addr(2), fp(1_000_000));

    // First place an order to create something to cancel.
    let place = NativeAction::PlaceOrder(PlaceOrderParams {
        market_id: 1,
        is_buy: true,
        price: fp(100),
        quantity: fp(10),
        order_type: OrderType::Limit,
        time_in_force: TimeInForce::GTC,
        reduce_only: false,
        client_order_id: None,
    });
    NativeExecutor::execute(&mut ctx, &addr(1), &place);

    // Now create a batch with both a cancel and a new order.
    // When sorted, the cancel should execute first.
    let actions = vec![
        (
            addr(2),
            NativeAction::PlaceOrder(PlaceOrderParams {
                market_id: 1,
                is_buy: false,
                price: fp(100),
                quantity: fp(5),
                order_type: OrderType::Limit,
                time_in_force: TimeInForce::GTC,
                reduce_only: false,
                client_order_id: None,
            }),
        ),
        (addr(1), NativeAction::CancelOrder { order_id: 1 }),
    ];

    let (pre_evm, post_evm) = call_sort(&actions);

    // Cancel is in pre_evm, GTC order is in post_evm.
    assert_eq!(pre_evm.len(), 1);
    assert!(matches!(pre_evm[0].1, NativeAction::CancelOrder { .. }));
    assert_eq!(post_evm.len(), 1);
    assert!(matches!(post_evm[0].1, NativeAction::PlaceOrder(_)));

    // Execute pre_evm first (cancel), then post_evm (new order).
    let cancel_result = NativeExecutor::execute_batch(&mut ctx, &pre_evm);
    assert!(
        cancel_result.results[0].success,
        "cancel should succeed since order 1 exists"
    );

    let place_result = NativeExecutor::execute_batch(&mut ctx, &post_evm);
    assert!(place_result.results[0].success, "new order should succeed");
}

// ============================================================================
// Test: Batch gas accumulation
// ============================================================================

#[test]
fn batch_accumulates_gas() {
    let (_dir, state_db) = open_test_db();
    let mut ctx = make_ctx(state_db);
    fund_native(&ctx, &addr(1), fp(1_000_000));
    fund_native(&ctx, &addr(2), fp(1_000_000));

    let actions = vec![
        (
            addr(1),
            NativeAction::PlaceOrder(PlaceOrderParams {
                market_id: 1,
                is_buy: true,
                price: fp(100),
                quantity: fp(1),
                order_type: OrderType::Limit,
                time_in_force: TimeInForce::GTC,
                reduce_only: false,
                client_order_id: None,
            }),
        ),
        (
            addr(2),
            NativeAction::PlaceOrder(PlaceOrderParams {
                market_id: 1,
                is_buy: false,
                price: fp(200),
                quantity: fp(1),
                order_type: OrderType::Limit,
                time_in_force: TimeInForce::GTC,
                reduce_only: false,
                client_order_id: None,
            }),
        ),
    ];

    let result = NativeExecutor::execute_batch(&mut ctx, &actions);
    assert!(
        result.total_gas > 0,
        "batch should accumulate gas from individual actions"
    );
    assert_eq!(
        result.total_gas,
        result.results.iter().map(|r| r.gas_used).sum::<u64>()
    );
}

// ============================================================================
// Test: Composite state root changes with native state
// ============================================================================

#[test]
fn composite_root_changes_with_native_state() {
    let (_dir, state_db) = open_test_db();

    // Compute root before any native actions.
    let root_before = compute_native_state_root(&state_db).unwrap();

    // Execute a native action that writes to state (deposit to native).
    // First, fund an EVM account.
    let account = AccountInfo {
        balance: U256::from(100_000_000_000u64), // 1000 in FixedPoint scale
        nonce: 0,
        code_hash: KECCAK_EMPTY,
        code: None,
        account_id: None,
    };
    state_db.put_account(&addr(1), &account).unwrap();

    // Now deposit to native balance via lockbox (1.0 in FixedPoint = 100_000_000 raw).
    let fp_amount = FixedPoint::from_raw(FixedPoint::SCALE);
    torus_core::lockbox::Lockbox::deposit_to_native(&state_db, &addr(1), fp_amount).unwrap();

    // Compute root after native state change.
    let root_after = compute_native_state_root(&state_db).unwrap();
    assert_ne!(
        root_before, root_after,
        "native root should change after deposit_to_native"
    );
}

// ============================================================================
// Test: Fee distribution at end of block
// ============================================================================

#[test]
fn fee_distribution_zero_fees() {
    let (_dir, state_db) = open_test_db();
    let mut ctx = make_ctx(state_db);

    // Zero fees should succeed without error.
    let result = NativeExecutor::distribute_fees(&mut ctx, 0);
    assert!(result.success);
    assert_eq!(result.gas_used, 0);
}

// ============================================================================
// Test: Epoch boundary detection
// ============================================================================

#[test]
fn epoch_boundary_not_triggered() {
    let (_dir, state_db) = open_test_db();
    let mut ctx = make_ctx(state_db);
    ctx.block_height = 50;
    ctx.epoch_length = 100;

    let result = NativeExecutor::process_epoch_boundary(&mut ctx);
    assert!(result.is_none(), "should not trigger at non-boundary block");
}

#[test]
fn epoch_boundary_triggered() {
    let (_dir, state_db) = open_test_db();
    let mut ctx = make_ctx(state_db);
    ctx.block_height = 100;
    ctx.epoch_length = 100;

    let result = NativeExecutor::process_epoch_boundary(&mut ctx);
    assert!(result.is_some(), "should trigger at epoch boundary");
}

// ============================================================================
// FIX 1 TEST: Proposer/validator pipeline parity
// ============================================================================

#[test]
fn proposer_validator_pipeline_parity() {
    // Both proposer and validator should produce identical state roots
    // for the same inputs, now that the proposer includes phases 4-7.
    use torus_bridge::BlockProposer;

    let (_dir, state_db) = open_test_db();
    let evm_executor = EvmExecutor::new(TORUS_CHAIN_ID);
    let proposer = BlockProposer::new(TORUS_CHAIN_ID, 100, 4, Address::ZERO, Address::ZERO);
    let validator = BlockValidator::new(TORUS_CHAIN_ID, 100, 4, Address::ZERO, Address::ZERO);

    let parent = torus_bridge::genesis_parent_header();

    // Build a block with the proposer (empty native + EVM actions).
    let proposed = proposer
        .build_block_with_native(
            &state_db,
            &evm_executor,
            &parent,
            vec![], // no native actions
            vec![], // no EVM transactions
            1000,
            addr(99),
        )
        .expect("proposer should succeed");

    // Validate the proposed block with the validator.
    // FIX CONS-PF-02: senders recovered from block's SignedNativeActions.
    let validated = validator.validate_block_with_native(&proposed.block, &state_db, &evm_executor);

    // The validator should accept the block (state roots match).
    match validated {
        Ok(v) => assert_eq!(
            v.state_root, proposed.block.header.state_root,
            "proposer and validator state roots must match"
        ),
        Err(e) => panic!(
            "validator rejected proposer's block: {e}. \
             This indicates pipeline divergence between proposer and validator."
        ),
    }
}

// ============================================================================
// FIX 2 TEST: Canonical action bytes determinism
// ============================================================================

#[test]
fn canonical_bytes_deterministic_across_calls() {
    // The canonical_bytes encoding must produce identical output for the same action.
    let action = NativeAction::PlaceOrder(PlaceOrderParams {
        market_id: 1,
        is_buy: true,
        price: fp(50_000),
        quantity: fp(10),
        order_type: OrderType::Limit,
        time_in_force: TimeInForce::GTC,
        reduce_only: false,
        client_order_id: Some(42),
    });

    let bytes1 = action.canonical_bytes();
    let bytes2 = action.canonical_bytes();
    assert_eq!(bytes1, bytes2, "canonical_bytes must be deterministic");

    // Different actions must produce different bytes.
    let action2 = NativeAction::CancelOrder { order_id: 1 };
    let bytes3 = action2.canonical_bytes();
    assert_ne!(
        bytes1, bytes3,
        "different actions must have different canonical bytes"
    );
}

#[test]
fn canonical_bytes_distinguishes_variants() {
    // Ensure each variant has a unique encoding (no collisions).
    let actions = vec![
        NativeAction::ClaimRewards,
        NativeAction::UnjailSelf,
        NativeAction::CancelOrder { order_id: 0 },
        NativeAction::CancelAllOrders { market_id: None },
        NativeAction::TransferToPerp { amount: U256::ZERO },
        NativeAction::TransferToSpot { amount: U256::ZERO },
    ];

    let bytes: Vec<Vec<u8>> = actions.iter().map(|a| a.canonical_bytes()).collect();
    for i in 0..bytes.len() {
        for j in (i + 1)..bytes.len() {
            assert_ne!(
                bytes[i], bytes[j],
                "actions at index {i} and {j} produced identical canonical bytes"
            );
        }
    }
}

#[test]
fn sort_uses_canonical_bytes_not_debug() {
    // Verify that action sorting uses canonical bytes (the function compiles and runs
    // without relying on Debug formatting).
    let actions = vec![
        (
            addr(1),
            NativeAction::Delegate {
                validator: addr(10),
                amount: U256::from(100u64),
            },
        ),
        (
            addr(2),
            NativeAction::Delegate {
                validator: addr(20),
                amount: U256::from(200u64),
            },
        ),
    ];

    // sort_native_actions internally uses action_sort_key which now calls canonical_bytes.
    let (pre, post) = call_sort(&actions);
    // Both are staking actions (post-EVM).
    assert!(pre.is_empty());
    assert_eq!(post.len(), 2);

    // Run again — must produce the same order.
    let (pre2, post2) = call_sort(&actions);
    assert_eq!(pre.len(), pre2.len());
    for (a, b) in post.iter().zip(post2.iter()) {
        assert_eq!(
            a.0, b.0,
            "deterministic sort must produce same sender order"
        );
    }
}

// ====================================================================
// Finding #18 — sort_native_actions ORDERING ORACLE (characterization).
//
// Execution order is consensus state: any deviation is a determinism bug.
// This oracle derives the EXACT expected (pre_evm, post_evm) ordering ONLY
// from the public sort contract — split by category into pre/post, then a
// STABLE sort by (category, sender, keccak(canonical_bytes)) with ties
// broken by original input index (the stability `sort_by` guarantees).
// It pins the ordering the CURRENT owned-signature implementation produces
// and is kept as the oracle across the borrow refactor: the refactored
// borrowed-input path must reproduce the IDENTICAL execution order.
// ====================================================================

/// Project a sort output to the consensus-visible ordering key sequence:
/// (sender, content-hash) per position.
fn projection(v: &[(Address, NativeAction)]) -> Vec<(Address, B256)> {
    v.iter()
        .map(|(s, a)| (*s, alloy_primitives::keccak256(a.canonical_bytes())))
        .collect()
}

/// Independent oracle for the expected (pre_evm, post_evm) ordering.
fn expected_split(
    actions: &[(Address, NativeAction)],
) -> (Vec<(Address, B256)>, Vec<(Address, B256)>) {
    let mut pre: Vec<(ActionCategory, Address, B256, usize)> = Vec::new();
    let mut post: Vec<(ActionCategory, Address, B256, usize)> = Vec::new();
    for (i, (s, a)) in actions.iter().enumerate() {
        let cat = classify_action(a);
        let hash = alloy_primitives::keccak256(a.canonical_bytes());
        let entry = (cat, *s, hash, i);
        match cat {
            ActionCategory::Cancellation | ActionCategory::NonGtcOrder => pre.push(entry),
            _ => post.push(entry),
        }
    }
    // STABLE sort by (category, sender, hash); ties preserve input order (index)
    // exactly like the impl's `sort_by` (a stable sort) over the same key.
    let sort_key = |e: &(ActionCategory, Address, B256, usize)| (e.0, e.1, e.2);
    pre.sort_by(|x, y| sort_key(x).cmp(&sort_key(y)).then(x.3.cmp(&y.3)));
    post.sort_by(|x, y| sort_key(x).cmp(&sort_key(y)).then(x.3.cmp(&y.3)));
    (
        pre.into_iter().map(|e| (e.1, e.2)).collect(),
        post.into_iter().map(|e| (e.1, e.2)).collect(),
    )
}

fn assert_matches_oracle(actions: &[(Address, NativeAction)]) {
    let (pre, post) = call_sort(actions);
    let (exp_pre, exp_post) = expected_split(actions);
    assert_eq!(
        projection(&pre),
        exp_pre,
        "pre_evm execution order must match the ordering oracle"
    );
    assert_eq!(
        projection(&post),
        exp_post,
        "post_evm execution order must match the ordering oracle"
    );
    // Every input action appears exactly once across the two groups.
    assert_eq!(
        pre.len() + post.len(),
        actions.len(),
        "sort must neither drop nor duplicate actions"
    );
}

fn place(market_id: u64, price: i64, tif: TimeInForce, ot: OrderType) -> NativeAction {
    NativeAction::PlaceOrder(PlaceOrderParams {
        market_id,
        is_buy: true,
        price: fp(price),
        quantity: fp(1),
        order_type: ot,
        time_in_force: tif,
        reduce_only: false,
        client_order_id: None,
    })
}

#[test]
fn sort_oracle_empty() {
    assert_matches_oracle(&[]);
}

#[test]
fn sort_oracle_single_action() {
    assert_matches_oracle(&[(addr(1), NativeAction::CancelOrder { order_id: 7 })]);
    assert_matches_oracle(&[(addr(2), place(1, 100, TimeInForce::GTC, OrderType::Limit))]);
}

#[test]
fn sort_oracle_multi_sender_interleaved_classes() {
    // Multiple senders, interleaved pre-EVM (cancel, IOC/market) and post-EVM
    // (GTC, lockbox, oracle, governance, staking) classes.
    let actions = vec![
        (addr(3), place(1, 100, TimeInForce::GTC, OrderType::Limit)), // post: GtcOrder
        (addr(1), NativeAction::CancelOrder { order_id: 5 }),          // pre: Cancellation
        (
            addr(2),
            NativeAction::TransferToPerp {
                amount: U256::from(50u64),
            },
        ), // post: Lockbox
        (addr(5), place(2, 200, TimeInForce::IOC, OrderType::Limit)), // pre: NonGtcOrder
        (
            addr(4),
            NativeAction::SubmitOraclePrices(torus_types::OracleSubmission {
                prices: vec![(1, fp(500))],
                timestamp: 1000,
            }),
        ), // post: Oracle
        (
            addr(6),
            NativeAction::Delegate {
                validator: addr(10),
                amount: U256::from(1u64),
            },
        ), // post: Staking
        (addr(2), place(1, 90, TimeInForce::FOK, OrderType::Limit)),  // pre: NonGtcOrder
        (addr(1), place(3, 300, TimeInForce::PostOnly, OrderType::Limit)), // post: GtcOrder
    ];
    assert_matches_oracle(&actions);
}

#[test]
fn sort_oracle_duplicate_senders_and_tie_cases() {
    // Duplicate senders AND exact ties (identical (sender, action) => equal sort
    // key): stability must preserve their relative INPUT order. Include several
    // identical entries so a non-stable sort would visibly reorder them.
    let dup = place(1, 100, TimeInForce::GTC, OrderType::Limit);
    let cancel = NativeAction::CancelOrder { order_id: 1 };
    let actions = vec![
        (addr(1), dup.clone()),
        (addr(1), dup.clone()), // tie with previous (same sender+action)
        (addr(1), cancel.clone()),
        (addr(1), dup.clone()), // tie again
        (addr(2), cancel.clone()),
        (addr(2), cancel.clone()), // tie (same sender+action)
        (addr(1), place(1, 100, TimeInForce::GTC, OrderType::Limit)), // == dup: tie
        (addr(2), dup.clone()),
    ];
    assert_matches_oracle(&actions);
}

#[test]
fn sort_oracle_all_categories_many_senders() {
    // Broad coverage across all categories and 12 senders in shuffled input.
    let mut actions: Vec<(Address, NativeAction)> = Vec::new();
    for i in 0..12u8 {
        let s = addr(i + 1);
        let a = match i % 6 {
            0 => NativeAction::CancelOrder { order_id: i as u128 },
            1 => place(1, 100 + i as i64, TimeInForce::GTC, OrderType::Limit),
            2 => place(2, 100 + i as i64, TimeInForce::IOC, OrderType::Limit),
            3 => NativeAction::TransferToPerp {
                amount: U256::from(i as u64),
            },
            4 => NativeAction::Delegate {
                validator: addr(100 + i),
                amount: U256::from(i as u64),
            },
            _ => place(3, 100 + i as i64, TimeInForce::GTC, OrderType::Market), // NonGtc (market)
        };
        actions.push((s, a));
    }
    assert_matches_oracle(&actions);
}
