//! A read of this node's DB that fails INSIDE revm (account, storage, code or
//! block-hash read through `DatabaseRef`) is a local fault: revm returns
//! `EVMError::Database` and the executor returns `EvmError::LocalFault`, on
//! which the consensus app fail-stops (torus-consensus `app.rs`
//! `evm_db_read_fault_fail_stops_block`). Before the fix it was
//! `EvmError::Internal`: the app logged it and ran the block without its EVM
//! writes on that node only (a divergence).
//!
//! The fault is real: the DB is opened without `CF_STORAGE`, so revm's SLOAD
//! read fails with `StateError::MissingColumnFamily`. Every other read works:
//! a tx that never reads storage executes normally, and deterministic errors
//! (an invalid tx, an over-gas block) keep their mapping.

use alloy_primitives::{keccak256, Address, Bytes, B256, U256};
use revm::context::TxEnv;
use revm::primitives::TxKind;
use revm::state::AccountInfo;
use torus_evm::{BlockEnvCfg, EvmError, EvmExecutor, TORUS_CHAIN_ID};
use torus_state::cf::{ALL_CF_NAMES, CF_STORAGE};
use torus_state::{StateDb, StateOverlay};

const KECCAK_EMPTY: B256 = B256::new([
    0xc5, 0xd2, 0x46, 0x01, 0x86, 0xf7, 0x23, 0x3c, 0x92, 0x7e, 0x7d, 0xb2, 0xdc, 0xc7, 0x03, 0xc0,
    0xe5, 0x00, 0xb6, 0x53, 0xca, 0x82, 0x27, 0x3b, 0x7b, 0xfa, 0xd8, 0x04, 0x5d, 0x85, 0xa4, 0x70,
]);
const ALICE: Address = Address::new([0xAA; 20]);
const BOB: Address = Address::new([0xBB; 20]);
/// A contract whose code reads storage slot 0: `PUSH1 0 SLOAD STOP`.
const READER: Address = Address::new([0xC0; 20]);
const SLOAD_CODE: [u8; 4] = [0x60, 0x00, 0x54, 0x00];

fn block_cfg() -> BlockEnvCfg {
    BlockEnvCfg {
        number: 1,
        timestamp: 1_000_000,
        beneficiary: Address::with_last_byte(0xFF),
        gas_limit: 30_000_000,
        base_fee: 1_000_000_000,
    }
}

fn seed(db: &StateDb) {
    let funded = AccountInfo {
        balance: U256::from(10u128.pow(19)),
        nonce: 0,
        code_hash: KECCAK_EMPTY,
        code: None,
        account_id: None,
    };
    db.put_account(&ALICE, &funded).unwrap();
    let code_hash = keccak256(SLOAD_CODE);
    db.put_code(&code_hash, &SLOAD_CODE).unwrap();
    let contract = AccountInfo {
        balance: U256::ZERO,
        nonce: 1,
        code_hash,
        code: None,
        account_id: None,
    };
    db.put_account(&READER, &contract).unwrap();
}

fn healthy_db() -> (tempfile::TempDir, StateDb) {
    let dir = tempfile::tempdir().unwrap();
    let db = StateDb::open(dir.path()).unwrap();
    seed(&db);
    (dir, db)
}

/// A DB without `CF_STORAGE`: revm's storage read fails on this node.
fn faulty_db() -> (tempfile::TempDir, StateDb) {
    let dir = tempfile::tempdir().unwrap();
    let mut opts = rocksdb::Options::default();
    opts.create_if_missing(true);
    opts.create_missing_column_families(true);
    let cfs = ALL_CF_NAMES.iter().copied().filter(|cf| *cf != CF_STORAGE);
    let db = StateDb::from_existing_db(rocksdb::DB::open_cf(&opts, dir.path(), cfs).unwrap());
    seed(&db);
    (dir, db)
}

fn tx(nonce: u64, to: Address) -> TxEnv {
    TxEnv {
        caller: ALICE,
        gas_limit: 100_000,
        gas_price: block_cfg().base_fee as u128,
        kind: TxKind::Call(to),
        data: Bytes::new(),
        nonce,
        chain_id: Some(TORUS_CHAIN_ID),
        ..Default::default()
    }
}

fn assert_db_local_fault(r: Result<impl std::fmt::Debug, EvmError>, what: &str) {
    match r {
        Err(EvmError::LocalFault(msg)) => {
            assert!(msg.contains("evm database read"), "{what}: {msg}");
            assert!(msg.contains(CF_STORAGE), "{what}: {msg}");
        }
        other => panic!("{what}: expected EvmError::LocalFault, got {other:?}"),
    }
}

/// revm's storage read fails on this node: block, overlay, single-tx and
/// call-simulation (RPC eth_call) paths all return `EvmError::LocalFault`.
/// RED before: `EvmError::Internal("database: ...")`.
#[test]
fn evm_db_read_fault_is_local_fault_on_every_path() {
    let (_d, db) = faulty_db();
    let ex = EvmExecutor::new(TORUS_CHAIN_ID);
    assert_db_local_fault(
        ex.execute_block(&db, &block_cfg(), vec![tx(0, READER)], true),
        "block skip_invalid",
    );
    assert_db_local_fault(
        ex.execute_block(&db, &block_cfg(), vec![tx(0, READER)], false),
        "block strict",
    );
    let overlay = StateOverlay::new(db.clone());
    assert_db_local_fault(
        ex.execute_block_with_overlay(&overlay, &block_cfg(), vec![tx(0, READER)], false),
        "overlay",
    );
    assert_db_local_fault(
        ex.execute_tx(&db, &block_cfg(), tx(0, READER))
            .map(|(r, _)| r),
        "tx",
    );
    assert_db_local_fault(
        ex.execute_call(&db, &block_cfg(), tx(0, READER))
            .map(|(r, _)| r),
        "call",
    );
}

/// A fault in a later tx aborts the whole block (no partial result): a
/// plain transfer first, then the storage read.
#[test]
fn evm_db_read_fault_in_a_later_tx_aborts_the_block() {
    let (_d, db) = faulty_db();
    let txs = vec![tx(0, BOB), tx(1, READER)];
    assert_db_local_fault(
        EvmExecutor::new(TORUS_CHAIN_ID).execute_block(&db, &block_cfg(), txs, true),
        "block",
    );
}

/// No false positives: on the faulty DB a tx that never reads storage runs
/// normally; on a healthy DB the storage-reading tx succeeds.
#[test]
fn evm_without_a_failed_read_is_unchanged() {
    let ex = EvmExecutor::new(TORUS_CHAIN_ID);
    let (_d, db) = faulty_db();
    let b = ex
        .execute_block(&db, &block_cfg(), vec![tx(0, BOB)], true)
        .unwrap();
    assert_eq!((b.receipts.len(), b.receipts[0].status), (1, true));
    let (_d2, db2) = healthy_db();
    let b = ex
        .execute_block(&db2, &block_cfg(), vec![tx(0, READER)], true)
        .unwrap();
    assert_eq!((b.receipts.len(), b.receipts[0].status), (1, true));
}

/// Deterministic errors keep their variants on the faulty DB too: an
/// invalid tx (nonce too high, strict) is `InvalidTransaction`, skipped
/// under `skip_invalid`; an over-gas block is `BlockGasLimitExceeded`.
#[test]
fn evm_deterministic_errors_unchanged() {
    let ex = EvmExecutor::new(TORUS_CHAIN_ID);
    for (what, (_d, db)) in [("healthy", healthy_db()), ("faulty", faulty_db())] {
        match ex.execute_block(&db, &block_cfg(), vec![tx(7, BOB)], false) {
            Err(EvmError::InvalidTransaction(m)) => {
                assert!(m.contains("NonceTooHigh"), "{what}: {m}")
            }
            other => panic!("{what}: expected InvalidTransaction, got {other:?}"),
        }
        let b = ex
            .execute_block(&db, &block_cfg(), vec![tx(7, BOB)], true)
            .unwrap();
        assert!(b.receipts.is_empty(), "{what}: invalid tx skipped");
        let mut cfg = block_cfg();
        cfg.gas_limit = 30_000;
        let transfer = |nonce| TxEnv {
            gas_limit: 21_000,
            ..tx(nonce, BOB)
        };
        match ex.execute_block(&db, &cfg, vec![transfer(0), transfer(1)], true) {
            Err(EvmError::BlockGasLimitExceeded { cumulative, limit }) => {
                assert_eq!((cumulative, limit), (42_000, 30_000), "{what}")
            }
            other => panic!("{what}: expected BlockGasLimitExceeded, got {other:?}"),
        }
    }
}
