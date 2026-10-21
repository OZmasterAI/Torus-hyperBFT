//! Integration tests for the torus-evm execution layer.

use alloy_primitives::{Address, Bloom, Bytes, B256, U256};
use revm::context::TxEnv;
use revm::primitives::TxKind;
use revm::state::AccountInfo;
use torus_core::precompiles::CoreWriterQueue;
use torus_evm::{calc_next_block_base_fee, BigLane, BlockEnvCfg, EvmExecutor, TORUS_CHAIN_ID};
use torus_state::StateDb;

/// Keccak256 of empty bytes.
const KECCAK_EMPTY: B256 = B256::new([
    0xc5, 0xd2, 0x46, 0x01, 0x86, 0xf7, 0x23, 0x3c, 0x92, 0x7e, 0x7d, 0xb2, 0xdc, 0xc7, 0x03, 0xc0,
    0xe5, 0x00, 0xb6, 0x53, 0xca, 0x82, 0x27, 0x3b, 0x7b, 0xfa, 0xd8, 0x04, 0x5d, 0x85, 0xa4, 0x70,
]);

/// Use addresses >= 0x10 to avoid precompile range (0x01-0x09).
const ALICE: Address = Address::new([0xAA; 20]);
const BOB: Address = Address::new([0xBB; 20]);
const CAROL: Address = Address::new([0xCC; 20]);

fn test_account(balance: U256) -> AccountInfo {
    AccountInfo {
        balance,
        nonce: 0,
        code_hash: KECCAK_EMPTY,
        code: None,
        account_id: None,
    }
}

fn open_test_db() -> (tempfile::TempDir, StateDb) {
    let dir = tempfile::tempdir().expect("create temp dir");
    let db = StateDb::open(dir.path()).expect("open db");
    (dir, db)
}

/// [`open_test_db`] with the CoreWriter order delay K pinned to 1 (item 7 step 2): for the
/// writer-precompile tests that read the order rows at `block + 1`.
fn open_test_db_k1() -> (tempfile::TempDir, StateDb) {
    let (dir, db) = open_test_db();
    db.put_cf_raw(
        torus_state::cf::CF_FEE_CONFIG,
        b"core_writer_order_delay",
        b"1",
    )
    .expect("pin K = 1");
    (dir, db)
}

fn default_block_cfg() -> BlockEnvCfg {
    BlockEnvCfg {
        number: 1,
        timestamp: 1_000_000,
        beneficiary: Address::with_last_byte(0xFF),
        gas_limit: 30_000_000,
        base_fee: 1_000_000_000, // 1 gwei
        features: 0,
    }
}

fn transfer_tx(from: Address, to: Address, value: U256, nonce: u64, gas_price: u128) -> TxEnv {
    TxEnv {
        caller: from,
        gas_limit: 21_000,
        gas_price,
        kind: TxKind::Call(to),
        value,
        nonce,
        chain_id: Some(TORUS_CHAIN_ID),
        ..Default::default()
    }
}

// ---------------------------------------------------------------------------
// D6 (S392): CREATE2 — first repo-wide coverage; Uniswap-style pair address
// math depends on it.
// ---------------------------------------------------------------------------
#[test]
fn create2_deploys_at_deterministic_address() {
    // A creation tx whose init code runs CREATE2(salt=42) on a 5-byte child
    // init code and RETURNs the created address as its "runtime code": the
    // call output then carries the child address with no second tx or state
    // persistence needed. It must equal
    // keccak256(0xff ++ deployer ++ salt ++ keccak256(child_init))[12..].
    let (_dir, db) = open_test_db();
    let ten_eth = U256::from(10_000_000_000_000_000_000u128);
    db.put_account(&ALICE, &test_account(ten_eth)).unwrap();

    let executor = EvmExecutor::new(TORUS_CHAIN_ID);
    let block_cfg = default_block_cfg();

    // PUSH1 0, PUSH1 0, RETURN — child deploys with empty runtime.
    let child_init: [u8; 5] = [0x60, 0x00, 0x60, 0x00, 0xf3];
    #[rustfmt::skip]
    let init_code: Vec<u8> = vec![
        0x64, 0x60, 0x00, 0x60, 0x00, 0xf3, // PUSH5 <child init>
        0x60, 0x00, 0x52,                   // MSTORE at 0 (right-aligned: mem[27..32])
        0x60, 0x2a,                         // PUSH1 42  (salt)
        0x60, 0x05,                         // PUSH1 5   (size)
        0x60, 0x1b,                         // PUSH1 27  (offset)
        0x60, 0x00,                         // PUSH1 0   (value)
        0xf5,                               // CREATE2
        0x60, 0x00, 0x52,                   // MSTORE created address at 0
        0x60, 0x20, 0x60, 0x00, 0xf3,       // RETURN mem[0..32]
    ];

    let tx = TxEnv {
        caller: ALICE,
        gas_limit: 1_000_000,
        gas_price: block_cfg.base_fee as u128,
        kind: TxKind::Create,
        value: U256::ZERO,
        data: Bytes::from(init_code),
        nonce: 0,
        chain_id: Some(TORUS_CHAIN_ID),
        ..Default::default()
    };
    let (result, _bundle) = executor.execute_tx(&db, &block_cfg, tx).unwrap();
    assert!(result.success, "CREATE2 deployer tx must succeed");

    let deployer = result.contract_address.expect("deployer address");
    assert_eq!(
        result.output.len(),
        32,
        "output is the returned address word"
    );
    let created = Address::from_slice(&result.output[12..32]);
    assert_ne!(
        created,
        Address::ZERO,
        "CREATE2 must not fail (returns 0 on failure)"
    );

    let salt = B256::from(U256::from(42u64));
    let expected = deployer.create2(salt, alloy_primitives::keccak256(child_init));
    assert_eq!(
        created, expected,
        "CREATE2 address must follow keccak256(0xff ++ deployer ++ salt ++ init_hash)"
    );
}

// ---------------------------------------------------------------------------
// D2 (S392): call-simulation mode — geth/reth eth_call semantics
// ---------------------------------------------------------------------------
#[test]
fn call_mode_allows_bare_call_from_empty_account() {
    let (_dir, db) = open_test_db();
    let executor = EvmExecutor::new(TORUS_CHAIN_ID);
    let block_cfg = default_block_cfg(); // height 1, base_fee = 1 gwei

    // Never-funded caller; omitted fee fields => gas_price 0 < base_fee.
    let empty = Address::new([0xEE; 20]);
    let tx = transfer_tx(empty, BOB, U256::ZERO, 0, 0);
    let (result, _) = executor
        .execute_call(&db, &block_cfg, tx)
        .expect("call mode must disable the base-fee check");
    assert!(result.success);

    // The consensus path must still reject the same underpriced tx.
    let tx = transfer_tx(empty, BOB, U256::ZERO, 0, 0);
    assert!(
        executor.execute_tx(&db, &block_cfg, tx).is_err(),
        "execute_tx must keep enforcing the base fee"
    );
}

#[test]
fn call_mode_ignores_nonce_mismatch() {
    let (_dir, db) = open_test_db();
    let executor = EvmExecutor::new(TORUS_CHAIN_ID);
    let block_cfg = default_block_cfg();

    // State nonce is 0; a stale or arbitrary nonce must not fail a simulation.
    let empty = Address::new([0xEE; 20]);
    let tx = transfer_tx(empty, BOB, U256::ZERO, 7, 0);
    let (result, _) = executor
        .execute_call(&db, &block_cfg, tx)
        .expect("nonce check disabled in call mode");
    assert!(result.success);
}

// ---------------------------------------------------------------------------
// 1. Simple ETH transfer
// ---------------------------------------------------------------------------
#[test]
fn simple_eth_transfer() {
    let (_dir, db) = open_test_db();

    let ten_eth = U256::from(10_000_000_000_000_000_000u128);
    db.put_account(&ALICE, &test_account(ten_eth)).unwrap();

    let executor = EvmExecutor::new(TORUS_CHAIN_ID);
    let block_cfg = default_block_cfg();
    let one_eth = U256::from(1_000_000_000_000_000_000u128);
    let tx = transfer_tx(ALICE, BOB, one_eth, 0, block_cfg.base_fee as u128);

    let (result, bundle) = executor.execute_tx(&db, &block_cfg, tx).unwrap();

    assert!(result.success, "transfer should succeed");
    assert_eq!(result.gas_used, 21_000, "simple transfer costs 21k gas");
    assert!(result.logs.is_empty(), "no logs for plain transfer");
    assert!(result.contract_address.is_none());

    // Bundle should have state changes for sender, receiver, and coinbase.
    assert!(!bundle.state.is_empty());
}

// ---------------------------------------------------------------------------
// 2. Contract deployment
// ---------------------------------------------------------------------------
#[test]
fn contract_deployment() {
    let (_dir, db) = open_test_db();

    let balance = U256::from(10_000_000_000_000_000_000u128);
    db.put_account(&ALICE, &test_account(balance)).unwrap();

    // Init code: stores 0xFF in memory[0], returns 1 byte as runtime code.
    // PUSH1 0xFF | PUSH1 0x00 | MSTORE8 | PUSH1 0x01 | PUSH1 0x00 | RETURN
    let init_code = hex::decode("60ff60005360016000f3").unwrap();

    let tx = TxEnv {
        caller: ALICE,
        gas_limit: 100_000,
        gas_price: 1_000_000_000,
        kind: TxKind::Create,
        value: U256::ZERO,
        data: Bytes::from(init_code),
        nonce: 0,
        chain_id: Some(TORUS_CHAIN_ID),
        ..Default::default()
    };

    let executor = EvmExecutor::new(TORUS_CHAIN_ID);
    let block_cfg = default_block_cfg();
    let (result, _bundle) = executor.execute_tx(&db, &block_cfg, tx).unwrap();

    assert!(result.success, "deployment should succeed");
    assert!(
        result.contract_address.is_some(),
        "should return contract address"
    );
    assert!(result.gas_used > 21_000, "CREATE costs more than base tx");
}

// ---------------------------------------------------------------------------
// 3. Revert handling
// ---------------------------------------------------------------------------
#[test]
fn revert_during_create() {
    let (_dir, db) = open_test_db();

    let balance = U256::from(10_000_000_000_000_000_000u128);
    db.put_account(&ALICE, &test_account(balance)).unwrap();

    // Init code: immediately reverts.
    // PUSH1 0x00 | PUSH1 0x00 | REVERT
    let init_code = hex::decode("60006000fd").unwrap();

    let tx = TxEnv {
        caller: ALICE,
        gas_limit: 100_000,
        gas_price: 1_000_000_000,
        kind: TxKind::Create,
        value: U256::ZERO,
        data: Bytes::from(init_code),
        nonce: 0,
        chain_id: Some(TORUS_CHAIN_ID),
        ..Default::default()
    };

    let executor = EvmExecutor::new(TORUS_CHAIN_ID);
    let block_cfg = default_block_cfg();
    let (result, _bundle) = executor.execute_tx(&db, &block_cfg, tx).unwrap();

    assert!(!result.success, "should revert");
    assert!(result.contract_address.is_none(), "no contract on revert");
    assert!(result.logs.is_empty(), "no logs on revert");
    assert!(result.gas_used > 0, "gas is still consumed on revert");
}

// ---------------------------------------------------------------------------
// 4. Out-of-gas (halt)
// ---------------------------------------------------------------------------
#[test]
fn out_of_gas_halt() {
    let (_dir, db) = open_test_db();

    let balance = U256::from(10_000_000_000_000_000_000u128);
    db.put_account(&ALICE, &test_account(balance)).unwrap();

    // Init code: infinite loop (JUMPDEST PUSH1 0x00 JUMP).
    let init_code = hex::decode("5b600056").unwrap();

    // Give just barely above intrinsic gas so execution hits OOG.
    // Intrinsic = 21000 + 32000 (create) + calldata_gas + initcode_overhead.
    // 4 bytes: 5b(NZ,16) 60(NZ,16) 00(Z,4) 56(NZ,16) = 52 gas.
    // Initcode overhead: 2 * ceil(4/32) = 2.
    // Total intrinsic ~ 53054.
    // Give 53200 → ~146 gas for execution, not enough for many iterations.
    let tx = TxEnv {
        caller: ALICE,
        gas_limit: 53_200,
        gas_price: 1_000_000_000,
        kind: TxKind::Create,
        value: U256::ZERO,
        data: Bytes::from(init_code),
        nonce: 0,
        chain_id: Some(TORUS_CHAIN_ID),
        ..Default::default()
    };

    let executor = EvmExecutor::new(TORUS_CHAIN_ID);
    let block_cfg = default_block_cfg();
    let (result, _bundle) = executor.execute_tx(&db, &block_cfg, tx).unwrap();

    assert!(!result.success, "infinite loop should OOG");
    assert!(result.contract_address.is_none());
    // All gas should be consumed.
    assert_eq!(result.gas_used, 53_200, "OOG consumes entire gas limit");
}

// ---------------------------------------------------------------------------
// 5. Block execution with multiple transactions
// ---------------------------------------------------------------------------
#[test]
fn block_execution_multiple_txs() {
    let (_dir, db) = open_test_db();

    let balance = U256::from(10_000_000_000_000_000_000u128);
    db.put_account(&ALICE, &test_account(balance)).unwrap();

    let block_cfg = default_block_cfg();
    let gas_price = block_cfg.base_fee as u128;

    let txs = vec![
        transfer_tx(ALICE, BOB, U256::from(1_000_000_000u64), 0, gas_price),
        transfer_tx(ALICE, CAROL, U256::from(2_000_000_000u64), 1, gas_price),
    ];

    let executor = EvmExecutor::new(TORUS_CHAIN_ID);
    let result = executor.execute_block(&db, &block_cfg, txs, false).unwrap();

    assert_eq!(result.receipts.len(), 2);
    assert_eq!(result.gas_used, 42_000); // 21000 * 2

    // Both receipts should be success.
    assert!(result.receipts[0].status);
    assert!(result.receipts[1].status);

    // Cumulative gas should be correct.
    assert_eq!(result.receipts[0].cumulative_gas_used, 21_000);
    assert_eq!(result.receipts[1].cumulative_gas_used, 42_000);

    // tx_index should be sequential.
    assert_eq!(result.receipts[0].tx_index, 0);
    assert_eq!(result.receipts[1].tx_index, 1);

    // Bundle should contain state changes.
    assert!(!result.bundle.state.is_empty());
}

/// Item 7 step 2: an EIP-1559 transfer at max fee `max_fee`, tip 1 wei.
fn transfer_1559(from: Address, to: Address, nonce: u64, max_fee: u128) -> TxEnv {
    TxEnv {
        tx_type: 2,
        gas_priority_fee: Some(1),
        ..transfer_tx(from, to, U256::from(1u64), nonce, max_fee)
    }
}

/// Item 7 step 2: a big-lane sender's tx runs at the big base fee, the other at the block's;
/// the result splits the gas and sums the fee revenue per lane. RED before: one base fee.
#[test]
fn big_lane_sender_pays_the_big_base_fee() {
    let (_dir, db) = open_test_db();
    let balance = U256::from(10_000_000_000_000_000_000u128);
    db.put_account(&ALICE, &test_account(balance)).unwrap();
    db.put_account(&BOB, &test_account(balance)).unwrap();
    // A and B funded; transfers at max fee 5 gwei, tip 1 wei; block base fee 1 gwei.
    let cfg = BlockEnvCfg {
        base_fee: 1_000_000_000,
        ..default_block_cfg()
    };
    let big = BigLane {
        base_fee: 3_000_000_000,
        senders: [BOB].into_iter().collect(),
    };
    let txs = vec![
        transfer_1559(ALICE, CAROL, 0, 5_000_000_000),
        transfer_1559(BOB, CAROL, 0, 5_000_000_000),
    ];
    let r = EvmExecutor::new(TORUS_CHAIN_ID)
        .execute_block_lanes(
            &db,
            torus_state::NativeStateOverlay::new(db.clone()),
            &cfg,
            txs,
            false,
            &big,
        )
        .unwrap();
    assert_eq!(r.receipts[0].effective_gas_price, 1_000_000_001);
    assert_eq!(r.receipts[1].effective_gas_price, 3_000_000_001);
    assert_eq!(r.gas_used, 42_000);
    assert_eq!(r.big_gas_used, 21_000);
    assert_eq!(
        r.fee_revenue,
        21_000 * 1_000_000_000 + 21_000 * 3_000_000_000
    );
    // B paid the big fee: 21k x (3 gwei + 1 wei) + the 1 wei value.
    let paid = |a: &Address| balance - r.bundle.state[a].info.as_ref().unwrap().balance;
    assert_eq!(paid(&BOB), U256::from(21_000u64 * 3_000_000_001 + 1));
    assert_eq!(paid(&ALICE), U256::from(21_000u64 * 1_000_000_001 + 1));
    // One lane (the default): both at the block's fee, no big gas.
    let one = EvmExecutor::new(TORUS_CHAIN_ID)
        .execute_block(
            &db,
            &cfg,
            vec![
                transfer_1559(ALICE, CAROL, 0, 5_000_000_000),
                transfer_1559(BOB, CAROL, 0, 5_000_000_000),
            ],
            false,
        )
        .unwrap();
    assert_eq!(one.receipts[1].effective_gas_price, 1_000_000_001);
    assert_eq!(
        (one.big_gas_used, one.fee_revenue),
        (0, 42_000 * 1_000_000_000)
    );
}

/// Item 7 step 2: a big-lane tx whose max fee is under the big base fee is invalid: with
/// `skip_invalid` it gets no receipt and the small-lane tx still runs.
#[test]
fn big_lane_tx_below_the_big_base_fee_is_skipped() {
    let (_dir, db) = open_test_db();
    let balance = U256::from(10_000_000_000_000_000_000u128);
    db.put_account(&ALICE, &test_account(balance)).unwrap();
    db.put_account(&BOB, &test_account(balance)).unwrap();
    let cfg = BlockEnvCfg {
        base_fee: 1_000_000_000,
        ..default_block_cfg()
    };
    let big = BigLane {
        base_fee: 3_000_000_000,
        senders: [BOB].into_iter().collect(),
    };
    let txs = vec![
        transfer_1559(BOB, CAROL, 0, 2_000_000_000),
        transfer_1559(ALICE, CAROL, 0, 2_000_000_000),
    ];
    let r = EvmExecutor::new(TORUS_CHAIN_ID)
        .execute_block_lanes(
            &db,
            torus_state::NativeStateOverlay::new(db.clone()),
            &cfg,
            txs,
            true,
            &big,
        )
        .unwrap();
    assert_eq!(r.included_indices, vec![1]);
    assert_eq!(r.receipts.len(), 1);
    assert_eq!(r.receipts[0].effective_gas_price, 1_000_000_001);
    assert_eq!((r.gas_used, r.big_gas_used), (21_000, 0));
    assert_eq!(r.fee_revenue, 21_000 * 1_000_000_000);
}

// ---------------------------------------------------------------------------
// 6. EIP-1559 base fee across blocks
// ---------------------------------------------------------------------------
#[test]
fn eip1559_base_fee_across_blocks() {
    let gas_limit = 30_000_000u64;
    let initial_base_fee = 1_000_000_000u64; // 1 gwei

    // Block 1: full (30M gas used) → base fee increases by 12.5%.
    let next = calc_next_block_base_fee(gas_limit, gas_limit, initial_base_fee);
    assert_eq!(next, 1_125_000_000);

    // Block 2: empty (0 gas used) → base fee decreases by 12.5%.
    let next2 = calc_next_block_base_fee(0, gas_limit, next);
    assert_eq!(next2, 984_375_000);

    // Block 3: at target (15M used) → no change.
    let next3 = calc_next_block_base_fee(15_000_000, gas_limit, next2);
    assert_eq!(next3, next2);
}

// ---------------------------------------------------------------------------
// 7. Block gas limit enforcement
// ---------------------------------------------------------------------------
#[test]
fn block_gas_limit_exceeded() {
    let (_dir, db) = open_test_db();

    let balance = U256::from(10_000_000_000_000_000_000u128);
    db.put_account(&ALICE, &test_account(balance)).unwrap();

    // Set a very tight gas limit (only room for 1 transfer).
    let block_cfg = BlockEnvCfg {
        gas_limit: 25_000,
        ..default_block_cfg()
    };

    let gas_price = block_cfg.base_fee as u128;
    let txs = vec![
        transfer_tx(ALICE, BOB, U256::from(1u64), 0, gas_price),
        transfer_tx(ALICE, BOB, U256::from(1u64), 1, gas_price),
    ];

    let executor = EvmExecutor::new(TORUS_CHAIN_ID);
    let err = executor
        .execute_block(&db, &block_cfg, txs, false)
        .unwrap_err();

    match err {
        torus_evm::EvmError::BlockGasLimitExceeded { .. } => {}
        other => panic!("expected BlockGasLimitExceeded, got: {other}"),
    }
}

// ---------------------------------------------------------------------------
// 8. Bloom filter: transfers produce zero bloom (no logs)
// ---------------------------------------------------------------------------
#[test]
fn transfer_block_has_zero_bloom() {
    let (_dir, db) = open_test_db();

    let balance = U256::from(10_000_000_000_000_000_000u128);
    db.put_account(&ALICE, &test_account(balance)).unwrap();

    let block_cfg = default_block_cfg();
    let gas_price = block_cfg.base_fee as u128;
    let txs = vec![transfer_tx(ALICE, BOB, U256::from(1u64), 0, gas_price)];

    let executor = EvmExecutor::new(TORUS_CHAIN_ID);
    let result = executor.execute_block(&db, &block_cfg, txs, false).unwrap();

    // Plain ETH transfers emit no logs → bloom is all zeros.
    assert_eq!(result.logs_bloom, Bloom::ZERO);
}

// ---------------------------------------------------------------------------
// 9. Torus precompile: BalanceReader via EVM
// ---------------------------------------------------------------------------
#[test]
fn precompile_balance_reader_via_evm() {
    let (_dir, db) = open_test_db();

    // Fund ALICE for gas.
    let ten_eth = U256::from(10_000_000_000_000_000_000u128);
    db.put_account(&ALICE, &test_account(ten_eth)).unwrap();

    // Write BOB's EVM balance directly to cf_accounts (72-byte record:
    // balance(32 BE) + nonce(8) + code_hash(32)).
    let bob_evm_balance = U256::from(5_000_000_000_000_000_000u128);
    let mut account_data = vec![0u8; 72];
    account_data[..32].copy_from_slice(&bob_evm_balance.to_be_bytes::<32>());
    account_data[40..72].copy_from_slice(KECCAK_EMPTY.as_slice());
    db.put_cf_raw("cf_accounts", BOB.as_slice(), &account_data)
        .unwrap();

    // BalanceReader precompile at 0x0801.
    let precompile_addr = Address::new([
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x08, 0x01,
    ]);

    // Build calldata: getBalances(address) selector + BOB padded.
    let sig_hash = alloy_primitives::keccak256("getBalances(address)".as_bytes());
    let mut calldata = Vec::with_capacity(36);
    calldata.extend_from_slice(&sig_hash[..4]);
    let mut padded = [0u8; 32];
    padded[12..32].copy_from_slice(BOB.as_slice());
    calldata.extend_from_slice(&padded);

    let block_cfg = default_block_cfg();
    let tx = TxEnv {
        caller: ALICE,
        gas_limit: 100_000,
        gas_price: block_cfg.base_fee as u128,
        kind: TxKind::Call(precompile_addr),
        value: U256::ZERO,
        data: Bytes::from(calldata),
        nonce: 0,
        chain_id: Some(TORUS_CHAIN_ID),
        ..Default::default()
    };

    let executor = EvmExecutor::new(TORUS_CHAIN_ID);
    let (result, _bundle) = executor.execute_tx(&db, &block_cfg, tx).unwrap();

    assert!(result.success, "precompile call should succeed");
    // BalanceReader returns (uint128, uint128, uint128, uint128) = 128 bytes.
    assert_eq!(
        result.output.len(),
        128,
        "BalanceReader returns 4 uint128 values"
    );

    // Second word (offset 32..64) = EVM balance (u128 in high 16 bytes of word).
    let evm_bal = u128::from_be_bytes(result.output[48..64].try_into().unwrap());
    assert_eq!(
        evm_bal, 5_000_000_000_000_000_000u128,
        "EVM balance should match what we wrote"
    );
}

/// Item 2 (T8): the EVM hands the block's header timestamp to the precompiles —
/// 0x0802 getPrice is stale iff `block_cfg.timestamp − aggregate ts > 60`.
#[test]
fn precompile_oracle_reader_uses_the_block_timestamp() {
    let block_cfg = default_block_cfg(); // number 1, timestamp 1_000_000
    for (row_ts, want_stale) in [(block_cfg.timestamp - 60, 0u8), (block_cfg.timestamp - 61, 1)] {
        let (_dir, db) = open_test_db();
        db.put_account(&ALICE, &test_account(U256::from(10u128.pow(19))))
            .unwrap();
        // aggregate row: price(16) || block(8) || reporters(4) || ts(8)
        let mut row = Vec::with_capacity(36);
        row.extend_from_slice(&(50_000i128 * 100_000_000).to_be_bytes());
        row.extend_from_slice(&500u64.to_be_bytes()); // block number: ignored by the rule
        row.extend_from_slice(&3u32.to_be_bytes());
        row.extend_from_slice(&row_ts.to_be_bytes());
        db.put_cf_raw("cf_native_oracle", &[b"agg".as_slice(), &1u64.to_be_bytes()].concat(), &row)
            .unwrap();
        let mut calldata = alloy_primitives::keccak256("getPrice(bytes32)".as_bytes())[..4].to_vec();
        calldata.extend_from_slice(&U256::from(1u8).to_be_bytes::<32>());
        let tx = TxEnv {
            caller: ALICE,
            gas_limit: 100_000,
            gas_price: block_cfg.base_fee as u128,
            // OracleReader precompile at 0x0802.
            kind: TxKind::Call(Address::new([
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x08, 0x02,
            ])),
            data: Bytes::from(calldata),
            chain_id: Some(TORUS_CHAIN_ID),
            ..Default::default()
        };
        let (result, _bundle) =
            EvmExecutor::new(TORUS_CHAIN_ID).execute_tx(&db, &block_cfg, tx).unwrap();
        assert!(result.success, "row ts {row_ts}");
        assert_eq!(result.output.len(), 96);
        assert_eq!(result.output[95], want_stale, "row ts {row_ts}");
    }
}

// ---------------------------------------------------------------------------
// 10. Torus precompile: unknown selector reverts
// ---------------------------------------------------------------------------
#[test]
fn precompile_unknown_selector_reverts() {
    let (_dir, db) = open_test_db();

    let ten_eth = U256::from(10_000_000_000_000_000_000u128);
    db.put_account(&ALICE, &test_account(ten_eth)).unwrap();

    // OrderBookReader precompile at 0x0800 — call with bogus selector.
    let precompile_addr = Address::new([
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x08, 0x00,
    ]);

    let block_cfg = default_block_cfg();
    let tx = TxEnv {
        caller: ALICE,
        gas_limit: 100_000,
        gas_price: block_cfg.base_fee as u128,
        kind: TxKind::Call(precompile_addr),
        value: U256::ZERO,
        data: Bytes::from(vec![0xDE, 0xAD, 0xBE, 0xEF]),
        nonce: 0,
        chain_id: Some(TORUS_CHAIN_ID),
        ..Default::default()
    };

    let executor = EvmExecutor::new(TORUS_CHAIN_ID);
    let (result, _bundle) = executor.execute_tx(&db, &block_cfg, tx).unwrap();

    // Precompile errors map to revert.
    assert!(!result.success, "bogus selector should revert");
}

// ---------------------------------------------------------------------------
// 11. Torus precompile: gas metering
// ---------------------------------------------------------------------------
#[test]
fn precompile_charges_correct_gas() {
    let (_dir, db) = open_test_db();

    let ten_eth = U256::from(10_000_000_000_000_000_000u128);
    db.put_account(&ALICE, &test_account(ten_eth)).unwrap();

    // BalanceReader (read-only) getBalances: GAS_PRECOMPILE_READ (16,400) + 4
    // answer words x 20 = 16,480 (s99 pricing).
    let precompile_addr = Address::new([
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x08, 0x01,
    ]);

    let sig_hash = alloy_primitives::keccak256("getBalances(address)".as_bytes());
    let mut calldata = Vec::with_capacity(36);
    calldata.extend_from_slice(&sig_hash[..4]);
    let mut padded = [0u8; 32];
    padded[12..32].copy_from_slice(BOB.as_slice());
    calldata.extend_from_slice(&padded);

    let block_cfg = default_block_cfg();
    let tx = TxEnv {
        caller: ALICE,
        gas_limit: 100_000,
        gas_price: block_cfg.base_fee as u128,
        kind: TxKind::Call(precompile_addr),
        value: U256::ZERO,
        data: Bytes::from(calldata),
        nonce: 0,
        chain_id: Some(TORUS_CHAIN_ID),
        ..Default::default()
    };

    let executor = EvmExecutor::new(TORUS_CHAIN_ID);
    let (result, _bundle) = executor.execute_tx(&db, &block_cfg, tx).unwrap();

    assert!(result.success);
    // Gas = 21000 (base) + 16,480 (precompile read) + calldata costs.
    // Just verify gas_used includes base tx + precompile cost.
    assert!(
        result.gas_used >= 21_000 + 16_480,
        "gas should include base tx (21k) + precompile read (16.48k), got {}",
        result.gas_used,
    );
}

/// s99: getOpenOrders was removed from 0x0800. Its selector reverts like any
/// unknown selector and the tx pays exactly the reader base (16,400) on top of
/// its intrinsic gas (Cancun: 21,000 + 16 per nonzero / 4 per zero calldata
/// byte).
#[test]
fn removed_get_open_orders_reverts_for_exactly_the_base() {
    use torus_core::precompiles::GAS_PRECOMPILE_READ;
    let (_dir, db) = open_test_db();
    db.put_account(&ALICE, &test_account(U256::from(10u128.pow(19))))
        .unwrap();
    let mut data =
        alloy_primitives::keccak256("getOpenOrders(address,bytes32)".as_bytes())[..4].to_vec();
    data.extend_from_slice(&[0u8; 12]);
    data.extend_from_slice(BOB.as_slice());
    data.extend_from_slice(&U256::from(1u64).to_be_bytes::<32>());
    let intrinsic: u64 = 21_000
        + data
            .iter()
            .map(|&b| if b == 0 { 4 } else { 16 })
            .sum::<u64>();
    let block_cfg = default_block_cfg();
    let tx = TxEnv {
        caller: ALICE,
        gas_limit: 100_000,
        gas_price: block_cfg.base_fee as u128,
        // OrderBookReader precompile at 0x0800.
        kind: TxKind::Call(Address::new([
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x08, 0x00,
        ])),
        data: Bytes::from(data),
        chain_id: Some(TORUS_CHAIN_ID),
        ..Default::default()
    };
    let r = EvmExecutor::new(TORUS_CHAIN_ID)
        .execute_tx(&db, &block_cfg, tx)
        .unwrap()
        .0;
    assert!(!r.success, "the removed selector reverts");
    assert_eq!(GAS_PRECOMPILE_READ, 16_400);
    assert_eq!(r.gas_used, intrinsic + GAS_PRECOMPILE_READ);
}

/// Item 7 step 1 (owner s122): getOrderBook was removed from 0x0800 like
/// getOpenOrders. Its selector reverts like any unknown selector and the tx
/// pays exactly the reader base (16,400) on top of its intrinsic gas.
#[test]
fn removed_get_order_book_reverts_for_exactly_the_base() {
    use torus_core::precompiles::GAS_PRECOMPILE_READ;
    let (_dir, db) = open_test_db();
    db.put_account(&ALICE, &test_account(U256::from(10u128.pow(19))))
        .unwrap();
    let mut data = alloy_primitives::keccak256("getOrderBook(bytes32)".as_bytes())[..4].to_vec();
    data.extend_from_slice(&U256::from(1u64).to_be_bytes::<32>());
    let intrinsic: u64 = 21_000
        + data
            .iter()
            .map(|&b| if b == 0 { 4 } else { 16 })
            .sum::<u64>();
    let block_cfg = default_block_cfg();
    let tx = TxEnv {
        caller: ALICE,
        gas_limit: 100_000,
        gas_price: block_cfg.base_fee as u128,
        // OrderBookReader precompile at 0x0800.
        kind: TxKind::Call(Address::new([
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x08, 0x00,
        ])),
        data: Bytes::from(data),
        chain_id: Some(TORUS_CHAIN_ID),
        ..Default::default()
    };
    let r = EvmExecutor::new(TORUS_CHAIN_ID)
        .execute_tx(&db, &block_cfg, tx)
        .unwrap()
        .0;
    assert!(!r.success, "the removed selector reverts");
    assert_eq!(r.gas_used, intrinsic + GAS_PRECOMPILE_READ);
}

/// Item 7 step 2 (owner decision 3, s125): a `bbo(bytes32)` call at 0x080e
/// costs the reader base (its point read inside) + 2 returned words =
/// 16,440 gas on top of the intrinsic gas, and returns the row's prices.
#[test]
fn bbo_call_costs_exactly_16440_gas() {
    let (_dir, db) = open_test_db();
    db.put_account(&ALICE, &test_account(U256::from(10u128.pow(19))))
        .unwrap();
    let (bid, ask) = (99i128 * 10i128.pow(18), 101i128 * 10i128.pow(18));
    let mut row = bid.to_be_bytes().to_vec();
    row.extend_from_slice(&ask.to_be_bytes());
    db.put_cf_raw(torus_state::cf::CF_NATIVE_BBO, &1u64.to_be_bytes(), &row)
        .unwrap();
    let mut data = alloy_primitives::keccak256("bbo(bytes32)".as_bytes())[..4].to_vec();
    data.extend_from_slice(&U256::from(1u64).to_be_bytes::<32>());
    let intrinsic: u64 = 21_000
        + data
            .iter()
            .map(|&b| if b == 0 { 4 } else { 16 })
            .sum::<u64>();
    let block_cfg = default_block_cfg();
    let tx = TxEnv {
        caller: ALICE,
        gas_limit: 100_000,
        gas_price: block_cfg.base_fee as u128,
        // BboReader precompile at 0x080e.
        kind: TxKind::Call(Address::new([
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x08, 0x0e,
        ])),
        data: Bytes::from(data),
        chain_id: Some(TORUS_CHAIN_ID),
        ..Default::default()
    };
    let r = EvmExecutor::new(TORUS_CHAIN_ID)
        .execute_tx(&db, &block_cfg, tx)
        .unwrap()
        .0;
    assert!(r.success, "bbo answers");
    assert_eq!(r.gas_used, intrinsic + 16_440);
    assert_eq!(r.output.len(), 64);
    assert_eq!(
        u128::from_be_bytes(r.output[16..32].try_into().unwrap()),
        bid as u128
    );
    assert_eq!(
        u128::from_be_bytes(r.output[48..64].try_into().unwrap()),
        ask as u128
    );
}

/// HL-parity: a reader precompile's gas scales with its work, not a flat
/// base: 500 per row read plus 20 per 32-byte word returned (s99).
/// `getMarkets()` reads 1 row and returns 2 words per market: 200 markets cost
/// exactly 200 x 500 + 400 x 20 more than none, and a gas limit that only
/// covers the empty answer runs out of gas.
#[test]
fn reader_precompile_gas_scales_with_returned_words() {
    use torus_core::precompiles::{GAS_PRECOMPILE_READ_PER_ROW, GAS_PRECOMPILE_READ_PER_WORD};
    let block_cfg = default_block_cfg();
    let get_markets = |db: &StateDb, gas_limit: u64| {
        let tx = TxEnv {
            caller: ALICE,
            gas_limit,
            gas_price: block_cfg.base_fee as u128,
            // BalanceReader precompile at 0x0801.
            kind: TxKind::Call(Address::new([
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x08, 0x01,
            ])),
            data: Bytes::from(alloy_primitives::keccak256("getMarkets()".as_bytes())[..4].to_vec()),
            chain_id: Some(TORUS_CHAIN_ID),
            ..Default::default()
        };
        EvmExecutor::new(TORUS_CHAIN_ID).execute_tx(db, &block_cfg, tx).unwrap().0
    };
    let funded_db = |markets: u64| {
        let (dir, db) = open_test_db();
        db.put_account(&ALICE, &test_account(U256::from(10u128.pow(19)))).unwrap();
        for m in 1..=markets {
            db.put_cf_raw("cf_native_markets", &m.to_be_bytes(), &[1u8]).unwrap();
        }
        (dir, db)
    };

    let (_d0, empty) = funded_db(0);
    let r0 = get_markets(&empty, 1_000_000);
    assert!(r0.success);
    assert_eq!(r0.output.len(), 4 * 32, "two empty arrays: 2 offsets + 2 lengths");

    let (_d1, full) = funded_db(200);
    let r1 = get_markets(&full, 1_000_000);
    assert!(r1.success);
    assert_eq!(r1.output.len(), (4 + 400) * 32);
    assert_eq!(
        r1.gas_used - r0.gas_used,
        200 * GAS_PRECOMPILE_READ_PER_ROW + 400 * GAS_PRECOMPILE_READ_PER_WORD,
        "gas grows by 500 per row read and 20 per word returned"
    );

    let (_d2, full2) = funded_db(200);
    let short = get_markets(&full2, r0.gas_used + 1_000);
    assert!(!short.success, "gas for the empty answer does not pay for 200 markets");
}

/// Runtime code: `calls` times CALL(gas = `stipend`, 0x0801, getMarkets()),
/// summing the success flags; returns the sum as one word.
fn get_markets_loop_code(calls: usize, stipend: u32) -> Vec<u8> {
    let sel = alloy_primitives::keccak256("getMarkets()".as_bytes());
    // mem[0..4] = selector: PUSH4 sel PUSH1 224 SHL PUSH1 0 MSTORE
    let mut c = vec![0x63, sel[0], sel[1], sel[2], sel[3], 0x60, 0xe0, 0x1b, 0x60, 0x00, 0x52];
    c.extend_from_slice(&[0x60, 0x00]); // running sum
    let s = stipend.to_be_bytes();
    for _ in 0..calls {
        // retSize, retOffset, argsSize 4, argsOffset 0, value 0, addr, gas
        c.extend_from_slice(&[0x60, 0x00, 0x60, 0x00, 0x60, 0x04, 0x60, 0x00, 0x60, 0x00]);
        c.extend_from_slice(&[0x61, 0x08, 0x01]);
        c.extend_from_slice(&[0x63, s[0], s[1], s[2], s[3]]);
        c.extend_from_slice(&[0xf1, 0x01]); // CALL, ADD to the sum
    }
    // mem[0..32] = sum; RETURN(0, 32)
    c.extend_from_slice(&[0x60, 0x00, 0x52, 0x60, 0x20, 0x60, 0x00, 0xf3]);
    c
}

/// Review (blocking): the attack is a contract looping CALLs into a reader
/// with a tight stipend. Each call with a stipend below the work of a
/// 200-market getMarkets (200 rows + 404 words) runs out of gas and costs the caller only
/// its stipend; the node's work per call is bounded by that stipend
/// (torus-bridge precompile_work_bound_tests). With enough gas every call
/// succeeds.
#[test]
fn tight_stipend_reader_calls_run_out_of_gas() {
    let block_cfg = default_block_cfg();
    let contract = Address::with_last_byte(0x77);
    let run = |stipend: u32| {
        let (dir, db) = open_test_db();
        db.put_account(&ALICE, &test_account(U256::from(10u128.pow(19)))).unwrap();
        for m in 1..=200u64 {
            db.put_cf_raw("cf_native_markets", &m.to_be_bytes(), &[1u8]).unwrap();
        }
        install_contract(&db, &contract, &get_markets_loop_code(20, stipend));
        let tx = TxEnv {
            caller: ALICE,
            gas_limit: 10_000_000,
            gas_price: block_cfg.base_fee as u128,
            kind: TxKind::Call(contract),
            chain_id: Some(TORUS_CHAIN_ID),
            ..Default::default()
        };
        let r = EvmExecutor::new(TORUS_CHAIN_ID).execute_tx(&db, &block_cfg, tx).unwrap().0;
        drop(dir);
        assert!(r.success, "the outer call survives its inner out-of-gas calls");
        (U256::from_be_slice(&r.output).to::<u64>(), r.gas_used)
    };

    let (ok, gas) = run(16_400 + 500 * 20);
    assert_eq!(ok, 0, "every tight call ran out of gas");
    assert!(
        gas < 21_000 + 20 * (26_400 + 1_000),
        "each call costs about its stipend, got {gas}"
    );

    let (ok, _) = run(16_400 + 500 * 200 + 20 * 404 + 1_000);
    assert_eq!(ok, 20, "with enough gas every call succeeds");
}

/// Re-review: the exact boundary through the EVM. getMarkets over 200 markets
/// is 200 rows + 404 words: a stipend of exactly 16,400 + 200 x 500 + 404 x 20
/// = 124,480 succeeds, one gas less runs out of gas.
#[test]
fn reader_stipend_exact_boundary() {
    let block_cfg = default_block_cfg();
    let contract = Address::with_last_byte(0x78);
    let exact = 124_480u32;
    for (stipend, want) in [(exact, 1u64), (exact - 1, 0)] {
        let (_dir, db) = open_test_db();
        db.put_account(&ALICE, &test_account(U256::from(10u128.pow(19)))).unwrap();
        for m in 1..=200u64 {
            db.put_cf_raw("cf_native_markets", &m.to_be_bytes(), &[1u8]).unwrap();
        }
        install_contract(&db, &contract, &get_markets_loop_code(1, stipend));
        let tx = TxEnv {
            caller: ALICE,
            gas_limit: 1_000_000,
            gas_price: block_cfg.base_fee as u128,
            kind: TxKind::Call(contract),
            chain_id: Some(TORUS_CHAIN_ID),
            ..Default::default()
        };
        let r = EvmExecutor::new(TORUS_CHAIN_ID).execute_tx(&db, &block_cfg, tx).unwrap().0;
        assert!(r.success);
        assert_eq!(U256::from_be_slice(&r.output).to::<u64>(), want, "stipend {stipend}");
    }
}

// ---------------------------------------------------------------------------
// 12. Standard Ethereum precompile (ecrecover) still works
// ---------------------------------------------------------------------------
#[test]
fn standard_precompile_ecrecover_still_works() {
    let (_dir, db) = open_test_db();

    let ten_eth = U256::from(10_000_000_000_000_000_000u128);
    db.put_account(&ALICE, &test_account(ten_eth)).unwrap();

    // ecrecover at 0x01. Send 128 bytes of zeros — it returns empty output
    // (invalid signature) but the tx succeeds, confirming EthPrecompiles
    // still functions alongside TorusPrecompiles.
    let ecrecover_addr = Address::with_last_byte(0x01);

    let block_cfg = default_block_cfg();
    let tx = TxEnv {
        caller: ALICE,
        gas_limit: 100_000,
        gas_price: block_cfg.base_fee as u128,
        kind: TxKind::Call(ecrecover_addr),
        value: U256::ZERO,
        data: Bytes::from(vec![0u8; 128]),
        nonce: 0,
        chain_id: Some(TORUS_CHAIN_ID),
        ..Default::default()
    };

    let executor = EvmExecutor::new(TORUS_CHAIN_ID);
    let (result, _bundle) = executor.execute_tx(&db, &block_cfg, tx).unwrap();

    // ecrecover with all-zero inputs returns empty (invalid sig) but succeeds.
    assert!(result.success, "ecrecover should not crash");
}

// ---------------------------------------------------------------------------
// 13. EIP-1559: tx rejected when gas_price < base_fee
// ---------------------------------------------------------------------------
#[test]
fn eip1559_tx_rejected_when_gas_price_below_base_fee() {
    let (_dir, db) = open_test_db();

    let balance = U256::from(10_000_000_000_000_000_000u128);
    db.put_account(&ALICE, &test_account(balance)).unwrap();

    // Block base_fee = 1 gwei.  Set tx gas_price = 0.5 gwei (below base_fee).
    let block_cfg = default_block_cfg(); // base_fee = 1_000_000_000
    let insufficient_gas_price: u128 = block_cfg.base_fee as u128 / 2; // 500M < 1G

    let tx = TxEnv {
        caller: ALICE,
        gas_limit: 21_000,
        gas_price: insufficient_gas_price,
        kind: TxKind::Call(BOB),
        value: U256::from(1u64),
        nonce: 0,
        chain_id: Some(TORUS_CHAIN_ID),
        ..Default::default()
    };

    let executor = EvmExecutor::new(TORUS_CHAIN_ID);
    let err = executor.execute_tx(&db, &block_cfg, tx).unwrap_err();

    match err {
        torus_evm::EvmError::InvalidTransaction(msg) => {
            assert!(
                msg.to_lowercase().contains("gaspricelessthanbasefee")
                    || msg.contains("GasPriceLessThanBasefee"),
                "error must cite GasPriceLessThanBasefee, got: {msg}"
            );
        }
        other => panic!("expected InvalidTransaction, got: {other}"),
    }
}

// ---------------------------------------------------------------------------
// 14. EIP-1559: unused gas is refunded to sender at gas_price per unit
// ---------------------------------------------------------------------------
#[test]
fn eip1559_unused_gas_refunded() {
    let (_dir, db) = open_test_db();

    // Fund ALICE generously.
    let balance = U256::from(10_000_000_000_000_000_000u128);
    db.put_account(&ALICE, &test_account(balance)).unwrap();

    let block_cfg = default_block_cfg(); // base_fee = 1 gwei
    let gas_price: u128 = block_cfg.base_fee as u128; // 1 gwei, no tip
    let gas_limit: u64 = 50_000; // much more than 21_000 needed by a transfer

    let value = U256::from(1_000u64);
    let tx = TxEnv {
        caller: ALICE,
        gas_limit,
        gas_price,
        kind: TxKind::Call(BOB),
        value,
        nonce: 0,
        chain_id: Some(TORUS_CHAIN_ID),
        ..Default::default()
    };

    let executor = EvmExecutor::new(TORUS_CHAIN_ID);
    let (result, bundle) = executor.execute_tx(&db, &block_cfg, tx).unwrap();

    // A plain ETH transfer uses exactly 21_000 gas regardless of gas_limit.
    assert!(result.success, "transfer should succeed");
    assert_eq!(result.gas_used, 21_000, "transfer always uses 21k gas");

    // The bundle must carry state changes (sender debit, receiver credit, coinbase).
    assert!(!bundle.state.is_empty(), "bundle should have state changes");

    // Execute again via execute_block to inspect receipts for gas_used accounting.
    let block_cfg2 = BlockEnvCfg {
        number: 2,
        ..block_cfg.clone()
    };
    db.put_account(&ALICE, &test_account(balance)).unwrap(); // reset alice
    let tx2 = TxEnv {
        caller: ALICE,
        gas_limit,
        gas_price,
        kind: TxKind::Call(BOB),
        value,
        nonce: 0,
        chain_id: Some(TORUS_CHAIN_ID),
        ..Default::default()
    };
    let block_result = executor
        .execute_block(&db, &block_cfg2, vec![tx2], false)
        .unwrap();

    // receipt.gas_used = 21_000 (actual), not 50_000 (limit).
    assert_eq!(
        block_result.receipts[0].gas_used, 21_000,
        "receipt gas_used must reflect actual consumption, not gas_limit"
    );

    // Total block gas = 21_000 (the 29_000 unspent were refunded to sender).
    assert_eq!(
        block_result.gas_used, 21_000,
        "block gas_used must be actual gas, refund does not count as block gas"
    );
}

// ---------------------------------------------------------------------------
// T4.4: EVM revert must revert writer-precompile side effects (per-tx journal)
// ---------------------------------------------------------------------------

/// Runtime bytecode for a proxy contract that forwards its calldata to the
/// CoreWriter precompile (0x0810) and then executes `tail` (STOP or REVERT).
fn core_writer_proxy_code(tail: &[u8]) -> Vec<u8> {
    #[rustfmt::skip]
    let mut code = vec![
        0x36,             // CALLDATASIZE
        0x5f, 0x5f,       // PUSH0 PUSH0
        0x37,             // CALLDATACOPY   mem[0..cds] = calldata
        0x5f,             // PUSH0          retSize
        0x5f,             // PUSH0          retOffset
        0x36,             // CALLDATASIZE   argsSize
        0x5f,             // PUSH0          argsOffset
        0x5f,             // PUSH0          value
        0x61, 0x08, 0x10, // PUSH2 0x0810   CoreWriter precompile
        0x5a,             // GAS
        0xf1,             // CALL
        0x50,             // POP
    ];
    code.extend_from_slice(tail);
    code
}

/// Install `code` as `address`'s runtime bytecode.
fn install_contract(db: &StateDb, address: &Address, code: &[u8]) {
    let code_hash = alloy_primitives::keccak256(code);
    db.put_code(&code_hash, code).unwrap();
    db.put_account(
        address,
        &AccountInfo {
            balance: U256::ZERO,
            nonce: 1,
            code_hash,
            code: None,
            account_id: None,
        },
    )
    .unwrap();
}

/// ABI calldata for CoreWriter.cancelOrder(bytes32).
fn cancel_order_calldata(order_id: u128) -> Vec<u8> {
    let sig_hash = alloy_primitives::keccak256("cancelOrder(bytes32)".as_bytes());
    let mut calldata = Vec::with_capacity(36);
    calldata.extend_from_slice(&sig_hash[..4]);
    let mut word = [0u8; 32];
    word[16..32].copy_from_slice(&order_id.to_be_bytes());
    calldata.extend_from_slice(&word);
    calldata
}

/// T4.4 RED-FIRST: a tx that calls a writer precompile and then REVERTS must leave
/// NO durable native-state write. Before journaling, CoreWriter's enqueue hit
/// RocksDB directly during EVM execution and survived the revert
/// (pending_count == 1); with the per-tx journal it is discarded (== 0).
#[test]
fn reverting_tx_discards_writer_precompile_side_effects() {
    let (_dir, db) = open_test_db_k1();
    let ten_eth = U256::from(10_000_000_000_000_000_000u128);
    db.put_account(&ALICE, &test_account(ten_eth)).unwrap();

    // Proxy calls CoreWriter, then PUSH0 PUSH0 REVERT.
    let contract = Address::new([0xC0; 20]);
    install_contract(&db, &contract, &core_writer_proxy_code(&[0x5f, 0x5f, 0xfd]));

    let block_cfg = default_block_cfg(); // number = 1
    let tx = TxEnv {
        caller: ALICE,
        gas_limit: 300_000,
        gas_price: block_cfg.base_fee as u128,
        kind: TxKind::Call(contract),
        value: U256::ZERO,
        data: Bytes::from(cancel_order_calldata(42)),
        nonce: 0,
        chain_id: Some(TORUS_CHAIN_ID),
        ..Default::default()
    };

    let executor = EvmExecutor::new(TORUS_CHAIN_ID);
    let (result, _bundle) = executor.execute_tx(&db, &block_cfg, tx).unwrap();
    assert!(!result.success, "proxy contract always reverts");

    // The enqueue targeted block 2 (current 1 + anti-frontrunning delay). The tx
    // reverted, so the native side effect must have been reverted with it.
    assert_eq!(
        CoreWriterQueue::pending_count(&db, block_cfg.number + 1).unwrap(),
        0,
        "reverted tx must leave no durable CoreWriter action"
    );
}

/// T4.4 parity guard: the SAME precompile call in a SUCCESSFUL tx stays durable —
/// journaling must not change committed behavior for successful txs (this passes
/// both before and after the journal change, pinning state-root determinism).
#[test]
fn successful_tx_persists_writer_precompile_side_effects() {
    let (_dir, db) = open_test_db_k1();
    let ten_eth = U256::from(10_000_000_000_000_000_000u128);
    db.put_account(&ALICE, &test_account(ten_eth)).unwrap();

    // Proxy calls CoreWriter, then STOP.
    let contract = Address::new([0xC0; 20]);
    install_contract(&db, &contract, &core_writer_proxy_code(&[0x00]));

    let block_cfg = default_block_cfg(); // number = 1
    let tx = TxEnv {
        caller: ALICE,
        gas_limit: 300_000,
        gas_price: block_cfg.base_fee as u128,
        kind: TxKind::Call(contract),
        value: U256::ZERO,
        data: Bytes::from(cancel_order_calldata(42)),
        nonce: 0,
        chain_id: Some(TORUS_CHAIN_ID),
        ..Default::default()
    };

    let executor = EvmExecutor::new(TORUS_CHAIN_ID);
    let (result, _bundle) = executor.execute_tx(&db, &block_cfg, tx).unwrap();
    assert!(result.success, "proxy with STOP tail must succeed");

    assert_eq!(
        CoreWriterQueue::pending_count(&db, block_cfg.number + 1).unwrap(),
        1,
        "successful tx must keep its CoreWriter action durable"
    );
}

/// T4.4 block-path variant (the consensus path): in a block with one reverting and
/// one successful writer-precompile tx, only the successful tx's action survives.
#[test]
fn block_execution_journals_writer_precompiles_per_tx() {
    let (_dir, db) = open_test_db_k1();
    let ten_eth = U256::from(10_000_000_000_000_000_000u128);
    db.put_account(&ALICE, &test_account(ten_eth)).unwrap();

    let reverter = Address::new([0xC1; 20]);
    let stopper = Address::new([0xC2; 20]);
    install_contract(&db, &reverter, &core_writer_proxy_code(&[0x5f, 0x5f, 0xfd]));
    install_contract(&db, &stopper, &core_writer_proxy_code(&[0x00]));

    let block_cfg = default_block_cfg(); // number = 1
    let make_tx = |to: Address, nonce: u64| TxEnv {
        caller: ALICE,
        gas_limit: 300_000,
        gas_price: block_cfg.base_fee as u128,
        kind: TxKind::Call(to),
        value: U256::ZERO,
        data: Bytes::from(cancel_order_calldata(42)),
        nonce,
        chain_id: Some(TORUS_CHAIN_ID),
        ..Default::default()
    };

    let executor = EvmExecutor::new(TORUS_CHAIN_ID);
    let result = executor
        .execute_block(
            &db,
            &block_cfg,
            vec![make_tx(reverter, 0), make_tx(stopper, 1)],
            false,
        )
        .unwrap();

    assert!(!result.receipts[0].status, "first tx reverts");
    assert!(result.receipts[1].status, "second tx succeeds");

    // F1 (s515, ported b5ef142) / consensus bug (c): nothing is durable until
    // the block's batch is written, which carries the queue rows with the bundle.
    assert_eq!(
        CoreWriterQueue::pending_count(&db, block_cfg.number + 1).unwrap(),
        0,
        "queue rows must not be durable before the block's batch is written"
    );
    let (_root, batch) = torus_state::incremental::evm_block_batch_incremental(
        &db,
        &result.bundle,
        None,
        Some(&result.native_writes),
    )
    .unwrap();
    db.write(batch).unwrap();
    assert_eq!(
        CoreWriterQueue::pending_count(&db, block_cfg.number + 1).unwrap(),
        1,
        "only the successful tx's CoreWriter action may be durable"
    );
}

/// Runtime bytecode for an OUTER contract A that forwards its calldata to `callee` via
/// CALL, POPs (swallows) the sub-call's success flag — the raw-EVM equivalent of a
/// Solidity `try { callee.foo() } catch {}` — then STOPs, so A itself returns success
/// regardless of whether the inner call reverted.
fn call_and_swallow_revert_code(callee: &Address) -> Vec<u8> {
    #[rustfmt::skip]
    let mut code = vec![
        0x36,             // CALLDATASIZE
        0x5f, 0x5f,       // PUSH0 PUSH0
        0x37,             // CALLDATACOPY   mem[0..cds] = calldata
        0x5f,             // PUSH0          retSize
        0x5f,             // PUSH0          retOffset
        0x36,             // CALLDATASIZE   argsSize
        0x5f,             // PUSH0          argsOffset
        0x5f,             // PUSH0          value
        0x73,             // PUSH20         <callee address>
    ];
    code.extend_from_slice(callee.as_slice());
    #[rustfmt::skip]
    code.extend_from_slice(&[
        0x5a,             // GAS
        0xf1,             // CALL
        0x50,             // POP            swallow success flag ("catch")
        0x00,             // STOP           A returns success
    ]);
    code
}

/// T4.4 revert-safety — CAUGHT INNER-FRAME revert (prior-review finding C, RED).
///
/// Contract A CALLs sub-contract B, which invokes the CoreWriter precompile and then
/// REVERTs; A swallows B's failure (the raw-EVM analogue of a Solidity try/catch) and
/// STOPs, so the TOP-LEVEL transaction SUCCEEDS. B's native action was enqueued inside a
/// reverted call frame, so it must NOT be durable despite the top-level success.
///
/// Before frame-level journaling the native override journal was a flat per-tx buffer with
/// no call-frame checkpoints: the inner-frame write survived B's revert and was committed
/// by the top-level success (pending_count == 1), corrupting native state. With per-frame
/// checkpoint/revert it is discarded (== 0). This is the test that would have caught the bug.
#[test]
fn caught_inner_frame_revert_discards_writer_precompile_side_effects() {
    let (_dir, db) = open_test_db_k1();
    let ten_eth = U256::from(10_000_000_000_000_000_000u128);
    db.put_account(&ALICE, &test_account(ten_eth)).unwrap();

    // B (inner): calls CoreWriter, then PUSH0 PUSH0 REVERT.
    let inner = Address::new([0xB0; 20]);
    install_contract(&db, &inner, &core_writer_proxy_code(&[0x5f, 0x5f, 0xfd]));
    // A (outer): CALLs B, swallows the revert, STOPs (success).
    let outer = Address::new([0xA0; 20]);
    install_contract(&db, &outer, &call_and_swallow_revert_code(&inner));

    let block_cfg = default_block_cfg(); // number = 1
    let tx = TxEnv {
        caller: ALICE,
        gas_limit: 1_000_000,
        gas_price: block_cfg.base_fee as u128,
        kind: TxKind::Call(outer),
        value: U256::ZERO,
        data: Bytes::from(cancel_order_calldata(42)),
        nonce: 0,
        chain_id: Some(TORUS_CHAIN_ID),
        ..Default::default()
    };

    let executor = EvmExecutor::new(TORUS_CHAIN_ID);
    let (result, _bundle) = executor.execute_tx(&db, &block_cfg, tx).unwrap();
    assert!(
        result.success,
        "outer contract swallows the inner revert and STOPs => top-level success"
    );

    // The inner frame that enqueued the CoreWriter action REVERTED, so — despite the
    // top-level success — the native side effect must have been rolled back with it.
    assert_eq!(
        CoreWriterQueue::pending_count(&db, block_cfg.number + 1).unwrap(),
        0,
        "native write from a caught inner-frame revert must not be durable"
    );
}

/// s114 (CoreWriter follow-up 1): the block execution reports, for each
/// queue row a successful tx queued, that tx's receipt and the row's call
/// index within the tx, in queue order. A reverted tx, a caught inner-frame
/// revert and a skipped (invalid) tx queue nothing and report nothing; a tx
/// with two calls reports both, call index 0 then 1; the receipt index skips
/// the skipped tx (it has no receipt).
#[test]
fn block_execution_reports_the_tx_and_call_of_each_queued_row() {
    use torus_evm::CoreWriterOrigin;
    let (_dir, db) = open_test_db_k1();
    let ten_eth = U256::from(10_000_000_000_000_000_000u128);
    db.put_account(&ALICE, &test_account(ten_eth)).unwrap();

    let stopper = Address::new([0xC2; 20]);
    let reverter = Address::new([0xC1; 20]);
    let twice = Address::new([0xC3; 20]);
    let outer = Address::new([0xA0; 20]);
    install_contract(&db, &stopper, &core_writer_proxy_code(&[0x00]));
    install_contract(&db, &reverter, &core_writer_proxy_code(&[0x5f, 0x5f, 0xfd]));
    // Two CoreWriter calls, then STOP.
    install_contract(
        &db,
        &twice,
        &core_writer_proxy_code(&core_writer_proxy_code(&[0x00])),
    );
    // Calls the reverter, swallows its revert, STOPs: a caught inner revert.
    install_contract(&db, &outer, &call_and_swallow_revert_code(&reverter));

    let block_cfg = default_block_cfg(); // number = 1
    let make_tx = |to: Address, nonce: u64| TxEnv {
        caller: ALICE,
        gas_limit: 1_000_000,
        gas_price: block_cfg.base_fee as u128,
        kind: TxKind::Call(to),
        value: U256::ZERO,
        data: Bytes::from(cancel_order_calldata(42)),
        nonce,
        chain_id: Some(TORUS_CHAIN_ID),
        ..Default::default()
    };
    let txs = vec![
        make_tx(stopper, 0),  // receipt 0: one row
        make_tx(reverter, 1), // receipt 1: reverted, no row
        make_tx(stopper, 9),  // nonce gap: skipped, no receipt
        make_tx(twice, 2),    // receipt 2: two rows
        make_tx(outer, 3),    // receipt 3: caught inner revert, no row
        make_tx(stopper, 4),  // receipt 4: one row
    ];

    let result = EvmExecutor::new(TORUS_CHAIN_ID)
        .execute_block(&db, &block_cfg, txs, true)
        .unwrap();
    assert_eq!(result.included_indices, vec![0, 1, 3, 4, 5]);
    let status: Vec<bool> = result.receipts.iter().map(|r| r.status).collect();
    assert_eq!(status, vec![true, false, true, true, true]);

    let origin = |seq, receipt, call_index| CoreWriterOrigin {
        target_block: 2,
        seq,
        receipt,
        call_index,
    };
    assert_eq!(
        result.core_writer_origins,
        vec![
            origin(0, 0, 0),
            origin(1, 2, 0),
            origin(2, 2, 1),
            origin(3, 4, 0)
        ]
    );
    // The origins describe exactly the queue rows the block's batch writes.
    let (_root, batch) = torus_state::incremental::evm_block_batch_incremental(
        &db,
        &result.bundle,
        None,
        Some(&result.native_writes),
    )
    .unwrap();
    db.write(batch).unwrap();
    assert_eq!(CoreWriterQueue::pending_count(&db, 2).unwrap(), 4);
}

/// s114: a block whose txs call no writer precompile reports no origin.
#[test]
fn block_without_writer_calls_reports_no_origin() {
    let (_dir, db) = open_test_db();
    let ten_eth = U256::from(10_000_000_000_000_000_000u128);
    db.put_account(&ALICE, &test_account(ten_eth)).unwrap();
    let block_cfg = default_block_cfg();
    let tx = TxEnv {
        caller: ALICE,
        gas_limit: 21_000,
        gas_price: block_cfg.base_fee as u128,
        kind: TxKind::Call(Address::new([0x42; 20])),
        value: U256::from(1u64),
        nonce: 0,
        chain_id: Some(TORUS_CHAIN_ID),
        ..Default::default()
    };
    let result = EvmExecutor::new(TORUS_CHAIN_ID)
        .execute_block(&db, &block_cfg, vec![tx], false)
        .unwrap();
    assert!(result.receipts[0].status);
    assert!(result.core_writer_origins.is_empty());
}

// ---------------------------------------------------------------------------
// EVM-PF-05: Lockbox precompile (0x0820) vs revm's block-level State cache.
//
// Each block below is executed through `execute_block` and then committed with
// `evm_block_batch_incremental` (bundle + queue rows, one batch) — the consensus commit path
// (torus-consensus app.rs). Fees are zeroed (base_fee 0, gas_price 0) so value
// conservation is exact.
//
// Pre-fix (RED) the precompile read-modify-wrote the caller's CF_ACCOUNTS record
// mid-block; revm's cached caller account then overwrote it at bundle commit
// (deposit minted native value, withdraw destroyed EVM value, and the deposit saw
// the stale pre-block balance). Post-fix (Hyperliquid model) the precompile never
// touches CF_ACCOUNTS or native balances: the EVM leg moves through revm's own
// journal (deposit = payable call, value burned in-frame) and the native leg is a
// CoreWriter action queued for the next block. The native half of these flows
// (drain_core_writer) is covered in torus-bridge/tests/lockbox_queue_tests.rs.
// ---------------------------------------------------------------------------

use torus_core::lockbox::WEI_PER_NATIVE_UNIT;
use torus_core::precompiles::{QueuedAction, QueuedActionKind};

const LOCKBOX: Address = Address::new([
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x08, 0x20,
]);

/// `n` whole native tokens in wei (18 decimals).
fn tokens_wei(n: u64) -> U256 {
    U256::from(n) * U256::from(1_000_000_000_000_000_000u128)
}

fn zero_fee_block_cfg(number: u64) -> BlockEnvCfg {
    BlockEnvCfg {
        number,
        timestamp: 1_000_000 + number,
        beneficiary: Address::with_last_byte(0xFF),
        gas_limit: 30_000_000,
        base_fee: 0,
        features: 0,
    }
}

fn lockbox_calldata(sig: &str, amount: U256) -> Vec<u8> {
    let sig_hash = alloy_primitives::keccak256(sig.as_bytes());
    let mut calldata = Vec::with_capacity(36);
    calldata.extend_from_slice(&sig_hash[..4]);
    calldata.extend_from_slice(&amount.to_be_bytes::<32>());
    calldata
}

fn deposit_calldata(wei: U256) -> Vec<u8> {
    lockbox_calldata("depositToNative(uint128)", wei)
}

fn withdraw_calldata(wei: U256) -> Vec<u8> {
    lockbox_calldata("withdrawFromNative(uint128)", wei)
}

fn call_tx(from: Address, to: Address, data: Vec<u8>, value: U256, nonce: u64) -> TxEnv {
    TxEnv {
        caller: from,
        gas_limit: 1_000_000,
        gas_price: 0,
        kind: TxKind::Call(to),
        value,
        data: Bytes::from(data),
        nonce,
        chain_id: Some(TORUS_CHAIN_ID),
        ..Default::default()
    }
}

fn evm_balance(db: &StateDb, who: &Address) -> U256 {
    db.get_account(who)
        .unwrap()
        .map(|a| a.balance)
        .unwrap_or(U256::ZERO)
}

fn native_raw(db: &StateDb, who: &Address) -> i128 {
    torus_core::position::PositionManager::new(db.clone())
        .get_native_balance(who)
        .unwrap()
        .available
        .raw()
}

fn seed_native(db: &StateDb, who: &Address, raw: i128) {
    torus_core::position::PositionManager::new(db.clone())
        .put_native_balance(
            who,
            &torus_core::position::NativeBalance {
                available: torus_types::FixedPoint::from_raw(raw),
                order_margin: torus_types::FixedPoint::ZERO,
            },
        )
        .unwrap();
}

/// Actions queued for `block` (non-destructive read of CF_CORE_WRITER_QUEUE).
fn queued_for(db: &StateDb, block: u64) -> Vec<QueuedAction> {
    // Drain through a throwaway overlay: the deletes stay in the overlay, the DB
    // is untouched.
    CoreWriterQueue::drain(&torus_state::NativeStateOverlay::new(db.clone()), block)
        .unwrap()
        .into_iter()
        .map(|a| a.expect("decodable queue row"))
        .collect()
}

/// Net native credit (in wei) still pending in the queue for `block`:
/// deposits add, withdrawals subtract (they move native value back to EVM).
fn pending_native_wei(db: &StateDb, block: u64) -> (U256, U256) {
    let (mut credit, mut debit) = (U256::ZERO, U256::ZERO);
    for qa in queued_for(db, block) {
        match qa.kind {
            QueuedActionKind::LockboxDeposit { amount } => {
                credit += U256::from(amount.raw() as u128) * U256::from(WEI_PER_NATIVE_UNIT)
            }
            QueuedActionKind::LockboxWithdraw { amount } => {
                debit += U256::from(amount.raw() as u128) * U256::from(WEI_PER_NATIVE_UNIT)
            }
            _ => {}
        }
    }
    (credit, debit)
}

/// Total value in wei: every EVM balance the tests touch + native balances
/// (scaled ×10^10) + the lockbox address itself (must stay 0 — burned in-frame).
fn total_wei(db: &StateDb, who: &[Address]) -> U256 {
    let mut total = U256::ZERO;
    for a in who {
        total += evm_balance(db, a);
        total += U256::from(native_raw(db, a) as u128) * U256::from(WEI_PER_NATIVE_UNIT);
    }
    total + evm_balance(db, &LOCKBOX) + evm_balance(db, &Address::with_last_byte(0xFF))
}

/// Execute `txs` as block `number` and commit the bundle via the consensus
/// incremental commit path. Returns the receipt statuses.
fn run_and_commit_block(db: &StateDb, number: u64, txs: Vec<TxEnv>) -> Vec<bool> {
    let executor = EvmExecutor::new(TORUS_CHAIN_ID);
    let result = executor
        .execute_block(db, &zero_fee_block_cfg(number), txs, false)
        .unwrap();
    let (_root, batch) = torus_state::incremental::evm_block_batch_incremental(
        db,
        &result.bundle,
        None,
        Some(&result.native_writes),
    )
    .unwrap();
    db.write(batch).unwrap();
    result.receipts.iter().map(|r| r.status).collect()
}

/// Block A: deposit-to-native (payable, with sub-unit dust), THEN an ordinary
/// transfer by the same caller. revm debits the deposit value like any value
/// transfer; the value is burned (0x0820 holds nothing); the native credit
/// floor(value / 10^10) is queued for block 2; the dust is burned.
#[test]
fn lockbox_block_a_deposit_then_transfer_conserves_value() {
    let (_dir, db) = open_test_db();
    let e0 = tokens_wei(1_000);
    db.put_account(&ALICE, &test_account(e0)).unwrap();
    let parties = [ALICE, BOB];
    let before = total_wei(&db, &parties);

    let dust = U256::from(1_234_567u64); // < 10^10 wei: burned, not credited
    let v = tokens_wei(300) + dust;
    let t = tokens_wei(100);
    let statuses = run_and_commit_block(
        &db,
        1,
        vec![
            call_tx(ALICE, LOCKBOX, deposit_calldata(v), v, 0),
            transfer_tx(ALICE, BOB, t, 1, 0),
        ],
    );
    assert_eq!(statuses, vec![true, true]);

    assert_eq!(evm_balance(&db, &ALICE), e0 - v - t, "EVM debited for deposit AND transfer");
    assert_eq!(evm_balance(&db, &BOB), t);
    assert_eq!(evm_balance(&db, &LOCKBOX), U256::ZERO, "deposit value burned, not held");
    assert_eq!(native_raw(&db, &ALICE), 0, "native leg is NOT applied mid-block");

    let queued = queued_for(&db, 2);
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0].trader, ALICE);
    assert!(matches!(
        queued[0].kind,
        QueuedActionKind::LockboxDeposit { amount } if amount.raw() == 300 * 100_000_000
    ));

    let (credit, _) = pending_native_wei(&db, 2);
    assert_eq!(
        total_wei(&db, &parties) + credit + dust,
        before,
        "conservation: EVM + native + queued credit + burned dust"
    );
}

/// Block B: ALICE receives a transfer, THEN deposits it. The deposit value is
/// checked by revm against ALICE's in-block balance (no stale CF_ACCOUNTS read).
#[test]
fn lockbox_block_b_transfer_in_then_deposit_sees_fresh_balance() {
    let (_dir, db) = open_test_db();
    let e0 = tokens_wei(1_000);
    db.put_account(&BOB, &test_account(e0)).unwrap();
    db.put_account(&ALICE, &test_account(U256::ZERO)).unwrap();
    let parties = [ALICE, BOB];
    let before = total_wei(&db, &parties);

    let t = tokens_wei(500);
    let v = tokens_wei(400);
    let statuses = run_and_commit_block(
        &db,
        1,
        vec![
            transfer_tx(BOB, ALICE, t, 0, 0),
            call_tx(ALICE, LOCKBOX, deposit_calldata(v), v, 0),
        ],
    );
    assert_eq!(statuses, vec![true, true], "deposit of in-block funds must succeed");
    assert_eq!(evm_balance(&db, &ALICE), t - v);
    assert_eq!(evm_balance(&db, &LOCKBOX), U256::ZERO);
    let (credit, _) = pending_native_wei(&db, 2);
    assert_eq!(credit, v);
    assert_eq!(total_wei(&db, &parties) + credit, before, "value conservation");
}

/// Block C: withdraw-from-native, THEN a transfer by the same caller. The
/// withdraw is queued only — no mid-block CF_ACCOUNTS / native write for revm to
/// clobber; the native debit + EVM credit happen next block via the drain.
#[test]
fn lockbox_block_c_withdraw_then_transfer_conserves_value() {
    let (_dir, db) = open_test_db();
    let e0 = tokens_wei(1_000);
    db.put_account(&ALICE, &test_account(e0)).unwrap();
    let n0: i128 = 900 * 100_000_000;
    seed_native(&db, &ALICE, n0);
    let parties = [ALICE, BOB];
    let before = total_wei(&db, &parties);

    let w = tokens_wei(200);
    let t = tokens_wei(100);
    let statuses = run_and_commit_block(
        &db,
        1,
        vec![
            call_tx(ALICE, LOCKBOX, withdraw_calldata(w), U256::ZERO, 0),
            transfer_tx(ALICE, BOB, t, 1, 0),
        ],
    );
    assert_eq!(statuses, vec![true, true]);
    assert_eq!(native_raw(&db, &ALICE), n0, "native debit is deferred to the drain");
    assert_eq!(evm_balance(&db, &ALICE), e0 - t, "EVM credit is deferred to the drain");
    let queued = queued_for(&db, 2);
    assert_eq!(queued.len(), 1);
    assert!(matches!(
        queued[0].kind,
        QueuedActionKind::LockboxWithdraw { amount } if amount.raw() == 200 * 100_000_000
    ));
    assert_eq!(total_wei(&db, &parties), before, "nothing moves until the drain");
}

/// Runtime bytecode that forwards its calldata AND its callvalue to `target`
/// via CALL, POPs the success flag, then executes `tail` (STOP or REVERT).
fn forward_with_value_code(target: &Address, tail: &[u8]) -> Vec<u8> {
    #[rustfmt::skip]
    let mut code = vec![
        0x36,             // CALLDATASIZE
        0x5f, 0x5f,       // PUSH0 PUSH0
        0x37,             // CALLDATACOPY   mem[0..cds] = calldata
        0x5f,             // PUSH0          retSize
        0x5f,             // PUSH0          retOffset
        0x36,             // CALLDATASIZE   argsSize
        0x5f,             // PUSH0          argsOffset
        0x34,             // CALLVALUE      value
        0x73,             // PUSH20         <target>
    ];
    code.extend_from_slice(target.as_slice());
    #[rustfmt::skip]
    code.extend_from_slice(&[
        0x5a,             // GAS
        0xf1,             // CALL
        0x50,             // POP
    ]);
    code.extend_from_slice(tail);
    code
}

/// A deposit made inside a CAUGHT reverted sub-call must neither enqueue nor
/// lose value: revm returns the wei to the frame that sent it, and the native
/// journal checkpoint drops the queued credit.
#[test]
fn lockbox_deposit_in_reverted_subcall_enqueues_nothing_and_loses_nothing() {
    let (_dir, db) = open_test_db();
    let e0 = tokens_wei(1_000);
    db.put_account(&ALICE, &test_account(e0)).unwrap();

    // inner: forwards value+calldata to 0x0820, then REVERTs.
    let inner = Address::new([0xB1; 20]);
    install_contract(&db, &inner, &forward_with_value_code(&LOCKBOX, &[0x5f, 0x5f, 0xfd]));
    // outer: forwards value+calldata to inner, swallows its failure, STOPs.
    let outer = Address::new([0xA1; 20]);
    install_contract(&db, &outer, &forward_with_value_code(&inner, &[0x00]));

    let parties = [ALICE, inner, outer];
    let before = total_wei(&db, &parties);
    let v = tokens_wei(7);
    let statuses = run_and_commit_block(
        &db,
        1,
        vec![call_tx(ALICE, outer, deposit_calldata(v), v, 0)],
    );
    assert_eq!(statuses, vec![true], "outer swallows the inner revert");

    assert!(queued_for(&db, 2).is_empty(), "reverted frame must not enqueue");
    assert_eq!(evm_balance(&db, &ALICE), e0 - v);
    assert_eq!(evm_balance(&db, &outer), v, "value returned to outer by the inner revert");
    assert_eq!(evm_balance(&db, &LOCKBOX), U256::ZERO);
    assert_eq!(total_wei(&db, &parties), before, "no value lost or minted");
}

/// Wrong-context / malformed lockbox calls revert and leave no trace.
#[test]
fn lockbox_rejects_bad_calls_without_side_effects() {
    let (_dir, db) = open_test_db_k1();
    let e0 = tokens_wei(1_000);
    db.put_account(&ALICE, &test_account(e0)).unwrap();
    seed_native(&db, &ALICE, 50 * 100_000_000);

    let one = tokens_wei(1);
    let txs = vec![
        // amount argument must equal msg.value
        call_tx(ALICE, LOCKBOX, deposit_calldata(one), U256::ZERO, 0),
        // all-dust deposit would credit nothing: rejected, not burned
        call_tx(
            ALICE,
            LOCKBOX,
            deposit_calldata(U256::from(WEI_PER_NATIVE_UNIT - 1)),
            U256::from(WEI_PER_NATIVE_UNIT - 1),
            1,
        ),
        // withdraw is not payable
        call_tx(ALICE, LOCKBOX, withdraw_calldata(one), one, 2),
        // withdraw amount must be a whole number of native units (10^10 wei)
        call_tx(ALICE, LOCKBOX, withdraw_calldata(one + U256::from(1u8)), U256::ZERO, 3),
        // value sent to a non-payable Torus precompile (CoreWriter) is refused
        call_tx(
            ALICE,
            Address::new([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x08, 0x10]),
            cancel_order_calldata(1),
            one,
            4,
        ),
    ];
    let statuses = run_and_commit_block(&db, 1, txs);
    assert_eq!(statuses, vec![false; 5], "every bad call reverts");
    assert_eq!(evm_balance(&db, &ALICE), e0, "reverted value returned");
    assert_eq!(native_raw(&db, &ALICE), 50 * 100_000_000);
    assert!(queued_for(&db, 2).is_empty());
}

/// Runtime bytecode that DELEGATECALLs `target` with its calldata, then STOPs
/// if the delegatecall succeeded and REVERTs otherwise.
fn delegatecall_code(target: &Address) -> Vec<u8> {
    #[rustfmt::skip]
    let mut code = vec![
        0x36,             // CALLDATASIZE
        0x5f, 0x5f,       // PUSH0 PUSH0
        0x37,             // CALLDATACOPY
        0x5f,             // PUSH0          retSize
        0x5f,             // PUSH0          retOffset
        0x36,             // CALLDATASIZE   argsSize
        0x5f,             // PUSH0          argsOffset
        0x73,             // PUSH20         <target>
    ];
    code.extend_from_slice(target.as_slice());
    #[rustfmt::skip]
    code.extend_from_slice(&[
        0x5a,             // GAS
        0xf4,             // DELEGATECALL
        0x60, 0x25,       // PUSH1 37 (JUMPDEST offset)
        0x57,             // JUMPI
        0x5f, 0x5f, 0xfd, // PUSH0 PUSH0 REVERT
        0x5b,             // JUMPDEST (ok)
        0x00,             // STOP
    ]);
    code
}

/// DELEGATECALL into 0x0820 would run the lockbox with msg.sender = the EOA
/// that called the delegating contract (so any contract could move a caller's
/// native funds); it must revert.
#[test]
fn lockbox_rejects_delegatecall() {
    let (_dir, db) = open_test_db();
    db.put_account(&ALICE, &test_account(tokens_wei(10))).unwrap();
    seed_native(&db, &ALICE, 50 * 100_000_000);
    let proxy = Address::new([0xD1; 20]);
    install_contract(&db, &proxy, &delegatecall_code(&LOCKBOX));

    let statuses = run_and_commit_block(
        &db,
        1,
        vec![call_tx(ALICE, proxy, withdraw_calldata(tokens_wei(1)), U256::ZERO, 0)],
    );
    assert_eq!(statuses, vec![false]);
    assert!(queued_for(&db, 2).is_empty());
}

/// Runtime bytecode that forwards its calldata to `target` with call-family
/// `opcode` (CALL 0xf1 / CALLCODE 0xf2 / DELEGATECALL 0xf4, zero value), then
/// STOPs if the inner call succeeded and REVERTs otherwise.
fn forwarding_code(target: &Address, opcode: u8) -> Vec<u8> {
    let with_value = matches!(opcode, 0xf1 | 0xf2);
    #[rustfmt::skip]
    let mut code = vec![
        0x36,             // CALLDATASIZE
        0x5f, 0x5f,       // PUSH0 PUSH0
        0x37,             // CALLDATACOPY
        0x5f,             // PUSH0          retSize
        0x5f,             // PUSH0          retOffset
        0x36,             // CALLDATASIZE   argsSize
        0x5f,             // PUSH0          argsOffset
    ];
    if with_value {
        code.push(0x5f); // PUSH0          value
    }
    code.push(0x73); // PUSH20 <target>
    code.extend_from_slice(target.as_slice());
    let jumpdest = code.len() as u8 + 8;
    #[rustfmt::skip]
    code.extend_from_slice(&[
        0x5a,             // GAS
        opcode,
        0x60, jumpdest,   // PUSH1 <ok>
        0x57,             // JUMPI
        0x5f, 0x5f, 0xfd, // PUSH0 PUSH0 REVERT
        0x5b,             // JUMPDEST (ok)
        0x00,             // STOP
    ]);
    code
}

/// PRE-EXISTING HIGH (s515 review): every writer precompile acts for
/// msg.sender, and DELEGATECALL / CALLCODE hand the precompile the ORIGINAL
/// caller — so any contract a user calls could place/cancel orders or
/// (un)delegate as that user. Only a plain CALL may reach a writer; a
/// forwarding contract that CALLs acts as itself.
#[test]
fn writer_precompiles_reject_delegatecall_and_callcode() {
    let writer = |id: u8| {
        Address::new([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x08, id])
    };
    let undelegate = {
        let sig = alloy_primitives::keccak256("undelegate(address,uint128)".as_bytes());
        let mut data = sig[..4].to_vec();
        data.extend_from_slice(&[0u8; 12]);
        data.extend_from_slice(Address::with_last_byte(0x77).as_slice());
        data.extend_from_slice(&U256::from(1u64).to_be_bytes::<32>());
        data
    };
    let cases = [
        ("CoreWriter 0x0810", writer(0x10), cancel_order_calldata(7)),
        ("CoreWriterStaking 0x0811", writer(0x11), undelegate),
    ];
    for (name, target, data) in cases {
        for (opcode, op_name, ok) in [(0xf4, "DELEGATECALL", false), (0xf2, "CALLCODE", false), (0xf1, "CALL", true)] {
            let (_dir, db) = open_test_db_k1();
            db.put_account(&ALICE, &test_account(tokens_wei(10))).unwrap();
            let proxy = Address::new([0xD2; 20]);
            install_contract(&db, &proxy, &forwarding_code(&target, opcode));

            let statuses = run_and_commit_block(
                &db,
                1,
                vec![call_tx(ALICE, proxy, data.clone(), U256::ZERO, 0)],
            );
            let queued = queued_for(&db, 2);
            if ok {
                assert_eq!(statuses, vec![true], "{name} via {op_name} must work");
                assert_eq!(queued.len(), 1, "{name} via {op_name}: one queued action");
                assert_eq!(queued[0].trader, proxy, "{name}: a CALL acts as the contract");
            } else {
                assert_eq!(statuses, vec![false], "{name} via {op_name} must revert");
                assert!(queued.is_empty(), "{name} via {op_name}: nothing queued as ALICE");
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Review #5 (s104): persisted contract code must be the ORIGINAL runtime bytes.
// revm's analyzed legacy Bytecode carries a trailing STOP pad; storing
// `Bytecode::bytes()` made every later block (and a restarted node) see a
// longer code: EXTCODESIZE / EXTCODECOPY / eth_getCode diverged from the
// deploy block and from Ethereum.
// ---------------------------------------------------------------------------

/// Runtime that does not end in STOP, so revm pads it: PUSH0 PUSH0 RETURN.
const UNPADDED_RUNTIME: [u8; 3] = [0x5f, 0x5f, 0xf3];

/// Probe: for the address in calldata[0..32] returns 96 bytes
/// (EXTCODESIZE, EXTCODEHASH, keccak256(EXTCODECOPY(addr, 0, size))).
#[rustfmt::skip]
const CODE_PROBE: [u8; 27] = [
    0x5f, 0x35,       // PUSH0 CALLDATALOAD          [addr]
    0x80, 0x3b,       // DUP1 EXTCODESIZE            [size, addr]
    0x80, 0x5f, 0x52, // DUP1 PUSH0 MSTORE           mem[0x00] = size
    0x80,             // DUP1                        [size, size, addr]
    0x5f,             // PUSH0                       offset 0
    0x60, 0x60,       // PUSH1 0x60                  destOffset
    0x84,             // DUP5                        addr
    0x3c,             // EXTCODECOPY                 mem[0x60..] = code
    0x60, 0x60, 0x20, // PUSH1 0x60 SHA3             [keccak(code), addr]
    0x60, 0x40, 0x52, // PUSH1 0x40 MSTORE           mem[0x40] = keccak(code)
    0x3f,             // EXTCODEHASH                 [codehash]
    0x60, 0x20, 0x52, // PUSH1 0x20 MSTORE           mem[0x20] = codehash
    0x60, 0x60, 0x5f, 0xf3, // PUSH1 0x60 PUSH0 RETURN
];

fn probe_code(db: &StateDb, number: u64, nonce: u64, probe: Address, target: Address) -> Vec<u8> {
    let mut calldata = [0u8; 32];
    calldata[12..].copy_from_slice(target.as_slice());
    let mut cfg = default_block_cfg();
    cfg.number = number;
    let tx = TxEnv {
        caller: ALICE,
        gas_limit: 200_000,
        gas_price: 1_000_000_000,
        kind: TxKind::Call(probe),
        data: Bytes::copy_from_slice(&calldata),
        nonce,
        chain_id: Some(TORUS_CHAIN_ID),
        ..Default::default()
    };
    let (r, _) = EvmExecutor::new(TORUS_CHAIN_ID)
        .execute_tx(db, &cfg, tx)
        .unwrap();
    assert!(r.success, "probe call must succeed");
    r.output
}

#[test]
fn persisted_code_is_original_runtime_across_commit_and_restart() {
    use revm::DatabaseRef;
    let dir = tempfile::tempdir().unwrap();
    let probe = Address::with_last_byte(0x70);
    let runtime_hash = alloy_primitives::keccak256(UNPADDED_RUNTIME);
    let mut want = vec![0u8; 96];
    want[31] = UNPADDED_RUNTIME.len() as u8;
    want[32..64].copy_from_slice(runtime_hash.as_slice());
    want[64..].copy_from_slice(runtime_hash.as_slice());

    let target = {
        let db = StateDb::open(dir.path()).unwrap();
        db.put_account(&ALICE, &test_account(U256::from(10u128.pow(19))))
            .unwrap();
        install_contract(&db, &probe, &CODE_PROBE);

        // Init: CODECOPY the 3 trailing runtime bytes to mem[0], RETURN them.
        let mut init = vec![0x60, 0x03, 0x60, 0x0a, 0x5f, 0x39, 0x60, 0x03, 0x5f, 0xf3];
        init.extend_from_slice(&UNPADDED_RUNTIME);
        let deploy = TxEnv {
            caller: ALICE,
            gas_limit: 200_000,
            gas_price: 1_000_000_000,
            kind: TxKind::Create,
            data: Bytes::from(init),
            nonce: 0,
            chain_id: Some(TORUS_CHAIN_ID),
            ..Default::default()
        };
        let result = EvmExecutor::new(TORUS_CHAIN_ID)
            .execute_block(&db, &default_block_cfg(), vec![deploy], false)
            .unwrap();
        assert!(result.receipts[0].status, "deploy must succeed");
        let target = ALICE.create(0);
        assert_eq!(
            result.bundle.state[&target]
                .info
                .as_ref()
                .unwrap()
                .code_hash,
            runtime_hash
        );

        // Pending-parent path (StateOverlay) before the block is durable.
        let overlay = torus_state::StateOverlay::from_bundle(db.clone(), &result.bundle);
        let pending = overlay.code_by_hash_ref(runtime_hash).unwrap();
        assert_eq!(
            pending.original_byte_slice(),
            UNPADDED_RUNTIME,
            "overlay code"
        );
        assert_eq!(pending.len(), UNPADDED_RUNTIME.len(), "overlay code length");

        // The consensus commit path.
        let (_root, batch) = torus_state::incremental::evm_block_batch_incremental(
            &db,
            &result.bundle,
            None,
            Some(&result.native_writes),
        )
        .unwrap();
        db.write(batch).unwrap();

        let stored = db.get_code(&runtime_hash).unwrap().expect("code stored");
        assert_eq!(
            stored, UNPADDED_RUNTIME,
            "CF_CODE must hold the original runtime"
        );
        assert_eq!(alloy_primitives::keccak256(&stored), runtime_hash);
        // A never-restarted node in the next block.
        assert_eq!(
            probe_code(&db, 2, 1, probe, target),
            want,
            "next block, same process"
        );
        target
    };

    // Restart: reopen the DB and rebuild the code from storage.
    let db = StateDb::open(dir.path()).unwrap();
    let reloaded = db.code_by_hash_ref(runtime_hash).unwrap();
    assert_eq!(reloaded.original_byte_slice(), UNPADDED_RUNTIME);
    assert_eq!(
        reloaded.hash_slow(),
        runtime_hash,
        "reloaded code hashes to its key"
    );
    assert_eq!(probe_code(&db, 2, 1, probe, target), want, "after restart");
}

/// Convenience module for hex decoding in tests.
mod hex {
    pub fn decode(s: &str) -> Result<Vec<u8>, String> {
        if !s.len().is_multiple_of(2) {
            return Err("odd length".into());
        }
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|e| e.to_string()))
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Item 7 option 0 task 1.5: the block's EVM part over the block overlay's layers
// ---------------------------------------------------------------------------

/// Task 1.5 (RED: `execute_block_lanes` read only a `&StateDb` and built its journal over
/// the DB): the same block over a DB holding every row, and over an `EvmBlockState` whose
/// overlay holds those rows only in its layers (parent: the sender and a queued row; L: the
/// contract, its code, the order delay, a tombstone over a stale DB account; C: a storage
/// slot and the header BLOCKHASH reads; the DB under them has stale or no rows), gives the
/// same receipts, bundle, journal rows and CoreWriter origins. The contract copies slot 1 to
/// slot 2, stores BLOCKHASH(0) in slot 3 and forwards a cancelOrder to CoreWriter, whose
/// `next_sequence` must see the parent's queued row (seq 1).
#[test]
fn execute_block_lanes_over_layers_equals_db() {
    use std::sync::Arc;
    use torus_state::cf::{
        CF_ACCOUNTS, CF_BLOCK_HEADERS, CF_CODE, CF_CORE_WRITER_QUEUE, CF_FEE_CONFIG, CF_STORAGE,
    };
    use torus_state::checkpoint_layer::CheckpointLayer;
    use torus_state::db::{encode_account_info, storage_key};
    use torus_state::{EvmBlockState, NativeStateOverlay, StateBackend};

    let contract = Address::repeat_byte(0xC7);
    let mut code = vec![
        0x60, 0x01, 0x54, 0x60, 0x02, 0x55, 0x60, 0x00, 0x40, 0x60, 0x03, 0x55,
    ];
    code.extend(core_writer_proxy_code(&[0x00]));
    let code_hash = alloy_primitives::keccak256(&code);
    let header_hash = B256::repeat_byte(0x5A);
    let ten_eth = U256::from(10_000_000_000_000_000_000u128);
    let acct = |balance: U256, nonce: u64, code_hash: B256| {
        encode_account_info(&AccountInfo {
            balance,
            nonce,
            code_hash,
            code: None,
            account_id: None,
        })
        .to_vec()
    };
    let queued = [2u64.to_be_bytes(), 0u64.to_be_bytes()].concat();
    let parent_rows: Vec<(&str, Vec<u8>, Vec<u8>)> = vec![
        (CF_ACCOUNTS, ALICE.to_vec(), acct(ten_eth, 0, KECCAK_EMPTY)),
        (
            CF_CORE_WRITER_QUEUE,
            queued.clone(),
            b"an earlier row".to_vec(),
        ),
    ];
    let l_rows: Vec<(&str, Vec<u8>, Vec<u8>)> = vec![
        (
            CF_ACCOUNTS,
            contract.to_vec(),
            acct(U256::ZERO, 1, code_hash),
        ),
        (CF_CODE, code_hash.to_vec(), code.clone()),
        (
            CF_FEE_CONFIG,
            b"core_writer_order_delay".to_vec(),
            b"1".to_vec(),
        ),
    ];
    let c_rows: Vec<(&str, Vec<u8>, Vec<u8>)> = vec![
        (
            CF_STORAGE,
            storage_key(&contract, &U256::from(1)).to_vec(),
            U256::from(0x77).to_be_bytes::<32>().to_vec(),
        ),
        (
            CF_BLOCK_HEADERS,
            0u64.to_be_bytes().to_vec(),
            [header_hash.as_slice(), b"rest"].concat(),
        ),
    ];

    let cfg = BlockEnvCfg {
        number: 1,
        ..default_block_cfg()
    };
    let txs = || {
        vec![
            TxEnv {
                caller: ALICE,
                gas_limit: 300_000,
                gas_price: 2_000_000_000,
                kind: TxKind::Call(contract),
                data: Bytes::from(cancel_order_calldata(7)),
                nonce: 0,
                chain_id: Some(TORUS_CHAIN_ID),
                ..Default::default()
            },
            transfer_tx(ALICE, CAROL, U256::from(1u64), 1, 2_000_000_000),
        ]
    };
    let exec = EvmExecutor::new(TORUS_CHAIN_ID);
    let journal_bytes = |db: &StateDb, j: &NativeStateOverlay| {
        let mut b = rocksdb::WriteBatch::default();
        j.append_pending_to_batch(db, &mut b).unwrap();
        b.data().to_vec()
    };

    // Every row in the DB.
    let (_da, db_a) = open_test_db();
    for (cf, k, v) in parent_rows.iter().chain(&l_rows).chain(&c_rows) {
        db_a.put_cf_raw(cf, k, v).unwrap();
    }
    let a = exec
        .execute_block_lanes(
            &db_a,
            NativeStateOverlay::new(db_a.clone()),
            &cfg,
            txs(),
            false,
            &BigLane::default(),
        )
        .unwrap();

    // The rows only in the layers; the DB under them is stale.
    let (_db_dir, db_b) = open_test_db();
    db_b.put_cf_raw(
        CF_ACCOUNTS,
        ALICE.as_slice(),
        &acct(U256::from(5u64), 9, KECCAK_EMPTY),
    )
    .unwrap();
    db_b.put_cf_raw(
        CF_ACCOUNTS,
        CAROL.as_slice(),
        &acct(U256::from(3u64), 0, KECCAK_EMPTY),
    )
    .unwrap();
    let frozen = |h: u64, rows: &[(&str, Vec<u8>, Vec<u8>)], tombstones: &[(&str, Vec<u8>)]| {
        let ov = NativeStateOverlay::new(db_b.clone());
        for (cf, k, v) in rows {
            ov.put_cf_raw(cf, k, v).unwrap();
        }
        for (cf, k) in tombstones {
            ov.delete_cf_raw(cf, k).unwrap();
        }
        ov.freeze(h)
    };
    let mut l = CheckpointLayer::new();
    l.merge(&frozen(0, &l_rows, &[(CF_ACCOUNTS, CAROL.to_vec())]));
    let mut c = CheckpointLayer::new();
    c.merge(&frozen(0, &c_rows, &[]));
    let overlay = NativeStateOverlay::with_layers(
        db_b.clone(),
        Some(frozen(0, &parent_rows, &[])),
        Some(Arc::new(l)),
        Some(Arc::new(c)),
    );
    let EvmBlockState { db, journal } = EvmBlockState::new(&overlay);
    let b = exec
        .execute_block_lanes(db, journal, &cfg, txs(), false, &BigLane::default())
        .unwrap();

    assert_eq!(format!("{:?}", a.receipts), format!("{:?}", b.receipts));
    assert!(a.receipts.iter().all(|r| r.status), "{:?}", a.receipts);
    assert_eq!(a.bundle, b.bundle);
    assert_eq!(a.core_writer_origins, b.core_writer_origins);
    assert_eq!(a.core_writer_origins.len(), 1);
    assert_eq!(
        a.core_writer_origins[0].seq, 1,
        "next_sequence saw the parent's row"
    );
    assert_eq!(
        journal_bytes(&db_a, &a.native_writes),
        journal_bytes(&db_b, &b.native_writes)
    );
    assert_eq!(
        a.native_writes.pending_write_count(),
        2,
        "one queue row and the last-order cursor row"
    );
    let slot = |i: u64| b.bundle.state[&contract].storage[&U256::from(i)].present_value;
    assert_eq!(slot(2), U256::from(0x77), "the C slot");
    assert_eq!(
        slot(3),
        U256::from_be_bytes(header_hash.0),
        "BLOCKHASH from C"
    );
    assert_eq!(
        b.bundle.state[&CAROL].original_info, None,
        "L's tombstone hides the stale DB account"
    );
    // Nothing read through the overlay was written to it.
    assert_eq!(overlay.pending_write_count(), 0);
    assert!(
        StateBackend::get_cf_raw(&db_b, CF_CORE_WRITER_QUEUE, &queued)
            .unwrap()
            .is_none()
    );
}

// ---------------------------------------------------------------------------
// P0.0 phase 2: a block runs at the spec of its header's feature bits.
// ---------------------------------------------------------------------------

/// Prague with `RunningStateHash` (id 0), a stand-in for a future spec feature.
const PRAGUE_WITH_BIT_0: torus_evm::EvmSpecRule = torus_evm::EvmSpecRule {
    base: torus_evm::SpecId::CANCUN,
    upgrades: &[(torus_types::Feature::RunningStateHash, torus_evm::SpecId::PRAGUE)],
};

/// A call carrying 1000 nonzero calldata bytes at gas limit 50000: valid under
/// Cancun (intrinsic 37000), invalid from Prague (EIP-7623 floor 61000).
fn floor_tx(nonce: u64) -> TxEnv {
    TxEnv {
        caller: ALICE,
        gas_limit: 50_000,
        gas_price: 1_000_000_000,
        kind: TxKind::Call(BOB),
        data: Bytes::from(vec![0xffu8; 1000]),
        nonce,
        chain_id: Some(TORUS_CHAIN_ID),
        ..Default::default()
    }
}

/// RED before P0.0 phase 2: the block ran at Cancun whatever its bits.
#[test]
fn block_runs_at_the_spec_of_its_feature_bits() {
    let rsh = torus_types::Feature::RunningStateHash.bit();
    for (rule, bits, included) in [
        (PRAGUE_WITH_BIT_0, 0, true),
        (PRAGUE_WITH_BIT_0, rsh, false),
        (torus_evm::BUILTIN_EVM_SPEC_RULE, rsh, true),
    ] {
        let (_dir, db) = open_test_db();
        db.put_account(&ALICE, &test_account(U256::from(10u128.pow(18))))
            .unwrap();
        let executor = EvmExecutor::with_spec_rule(TORUS_CHAIN_ID, rule);
        let cfg = BlockEnvCfg {
            features: bits,
            ..default_block_cfg()
        };
        // The committed path skips an invalid tx; the strict path refuses the block.
        let skip = executor
            .execute_block(&db, &cfg, vec![floor_tx(0)], true)
            .unwrap();
        assert_eq!(
            skip.included_indices.len(),
            usize::from(included),
            "bits {bits:#x}, base {:?}",
            rule.base
        );
        let strict = executor.execute_block(&db, &cfg, vec![floor_tx(0)], false);
        assert_eq!(strict.is_ok(), included, "strict, bits {bits:#x}");
        let overlay = torus_state::StateOverlay::new(db.clone());
        let over = executor
            .execute_block_with_overlay(&overlay, &cfg, vec![floor_tx(0)], true)
            .unwrap();
        assert_eq!(over.included_indices.len(), usize::from(included), "overlay");
        let one = executor
            .execute_tx(&db, &cfg, floor_tx(0))
            .is_ok_and(|(r, _)| r.success);
        assert_eq!(one, included, "execute_tx, bits {bits:#x}");
    }
}
