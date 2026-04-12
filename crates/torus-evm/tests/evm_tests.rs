//! Integration tests for the torus-evm execution layer.

use alloy_primitives::{Address, Bloom, Bytes, B256, U256};
use revm::context::TxEnv;
use revm::primitives::TxKind;
use revm::state::AccountInfo;
use torus_evm::{calc_next_block_base_fee, BlockEnvCfg, EvmExecutor, TORUS_CHAIN_ID};
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

fn default_block_cfg() -> BlockEnvCfg {
    BlockEnvCfg {
        number: 1,
        timestamp: 1_000_000,
        beneficiary: Address::with_last_byte(0xFF),
        gas_limit: 30_000_000,
        base_fee: 1_000_000_000, // 1 gwei
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
    let result = executor.execute_block(&db, &block_cfg, txs).unwrap();

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
    let err = executor.execute_block(&db, &block_cfg, txs).unwrap_err();

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
    let result = executor.execute_block(&db, &block_cfg, txs).unwrap();

    // Plain ETH transfers emit no logs → bloom is all zeros.
    assert_eq!(result.logs_bloom, Bloom::ZERO);
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
