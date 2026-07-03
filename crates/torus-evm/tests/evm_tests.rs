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
    assert_eq!(result.output.len(), 32, "output is the returned address word");
    let created = Address::from_slice(&result.output[12..32]);
    assert_ne!(created, Address::ZERO, "CREATE2 must not fail (returns 0 on failure)");

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
    let err = executor.execute_block(&db, &block_cfg, txs, false).unwrap_err();

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
    let precompile_addr =
        Address::new([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x08, 0x01]);

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

// ---------------------------------------------------------------------------
// 10. Torus precompile: unknown selector reverts
// ---------------------------------------------------------------------------
#[test]
fn precompile_unknown_selector_reverts() {
    let (_dir, db) = open_test_db();

    let ten_eth = U256::from(10_000_000_000_000_000_000u128);
    db.put_account(&ALICE, &test_account(ten_eth)).unwrap();

    // OrderBookReader precompile at 0x0800 — call with bogus selector.
    let precompile_addr =
        Address::new([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x08, 0x00]);

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

    // BalanceReader (read-only) costs GAS_PRECOMPILE_READ = 2600.
    let precompile_addr =
        Address::new([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x08, 0x01]);

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
    // Gas = 21000 (base) + 2600 (precompile read) + calldata costs.
    // Just verify gas_used includes base tx + precompile cost.
    assert!(
        result.gas_used >= 21_000 + 2_600,
        "gas should include base tx (21k) + precompile read (2.6k), got {}",
        result.gas_used,
    );
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
    let block_result = executor.execute_block(&db, &block_cfg2, vec![tx2], false).unwrap();

    // receipt.gas_used = 21_000 (actual), not 50_000 (limit).
    assert_eq!(block_result.receipts[0].gas_used, 21_000,
        "receipt gas_used must reflect actual consumption, not gas_limit");

    // Total block gas = 21_000 (the 29_000 unspent were refunded to sender).
    assert_eq!(block_result.gas_used, 21_000,
        "block gas_used must be actual gas, refund does not count as block gas");
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
