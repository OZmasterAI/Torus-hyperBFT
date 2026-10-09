//! R02 branch 5 (owner option A): 0x0800 getPosition's UPnL oracle read.
//!
//! A LOCAL storage fault in that read (the read failed on this node) is
//! never answered around: before R02 it read as UPnL 0 (a consensus-visible
//! answer only the faulty node gives). The precompile provider records it
//! and aborts the EVM execution with `EvmError::LocalFault`, which the
//! consensus app turns into a fail-stop (torus-consensus `app.rs`
//! `r02_evm_*`). Absence (no aggregate row, a stale or short one) keeps UPnL
//! 0 with byte-identical output and gas; an ordinary precompile revert stays
//! a revert.
//!
//! The fault is real: the DB is opened without the oracle column family, so
//! the aggregate read fails with `MissingColumnFamily` (a `StateError`).

use alloy_primitives::{Address, Bytes, B256, U256};
use revm::context::TxEnv;
use revm::primitives::TxKind;
use revm::state::AccountInfo;
use torus_core::position::{MarginType, Position, PositionManager};
use torus_evm::{BlockEnvCfg, EvmError, EvmExecutor, TORUS_CHAIN_ID};
use torus_state::cf::{ALL_CF_NAMES, CF_NATIVE_ORACLE};
use torus_state::StateDb;
use torus_types::FixedPoint;

const KECCAK_EMPTY: B256 = B256::new([
    0xc5, 0xd2, 0x46, 0x01, 0x86, 0xf7, 0x23, 0x3c, 0x92, 0x7e, 0x7d, 0xb2, 0xdc, 0xc7, 0x03, 0xc0,
    0xe5, 0x00, 0xb6, 0x53, 0xca, 0x82, 0x27, 0x3b, 0x7b, 0xfa, 0xd8, 0x04, 0x5d, 0x85, 0xa4, 0x70,
]);
const ALICE: Address = Address::new([0xAA; 20]);
const MARKET: u64 = 1;
/// OrderBookReader (getPosition) at 0x0800.
const READER: Address = Address::new([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x08, 0x00]);

/// getPosition(ALICE, MARKET) output and the tx's gas on main b88b0c90 with
/// no oracle row (UPnL 0), captured there before the change (the same
/// test, run before the precompile change).
const MAIN_OUTPUT_HEX: &str = "000000000000000000000000000000000000000000000000000000000bebc20000000000000000000000000000000000000000000000000000000002540be40000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000003b9aca00";
const MAIN_GAS_USED: u64 = 38_072;

fn block_cfg() -> BlockEnvCfg {
    BlockEnvCfg {
        number: 1,
        timestamp: 1_000_000,
        beneficiary: Address::with_last_byte(0xFF),
        gas_limit: 30_000_000,
        base_fee: 1_000_000_000,
    }
}

fn fp(v: i64) -> FixedPoint {
    FixedPoint::from_raw(v as i128 * FixedPoint::SCALE)
}

/// ALICE funded, long 2 of MARKET at 100.
fn seed(db: &StateDb) {
    let funded = AccountInfo {
        balance: U256::from(10u128.pow(19)),
        nonce: 0,
        code_hash: KECCAK_EMPTY,
        code: None,
        account_id: None,
    };
    db.put_account(&ALICE, &funded).unwrap();
    PositionManager::new(db.clone())
        .put_position(&Position {
            trader: ALICE,
            market_id: MARKET,
            is_long: true,
            size: fp(2),
            entry_price: fp(100),
            cost_basis: fp(200),
            realized_pnl: FixedPoint::ZERO,
            isolated_margin: fp(10),
            margin_type: MarginType::Cross,
        })
        .unwrap();
}

/// A healthy DB (every column family).
fn healthy_db() -> (tempfile::TempDir, StateDb) {
    let dir = tempfile::tempdir().unwrap();
    let db = StateDb::open(dir.path()).unwrap();
    seed(&db);
    (dir, db)
}

/// A DB without the oracle column family: the aggregate read fails on this
/// node (`StateError::MissingColumnFamily`), every other read works.
fn faulty_db() -> (tempfile::TempDir, StateDb) {
    let dir = tempfile::tempdir().unwrap();
    let mut opts = rocksdb::Options::default();
    opts.create_if_missing(true);
    opts.create_missing_column_families(true);
    let cfs = ALL_CF_NAMES.iter().copied().filter(|cf| *cf != CF_NATIVE_ORACLE);
    let db = StateDb::from_existing_db(rocksdb::DB::open_cf(&opts, dir.path(), cfs).unwrap());
    seed(&db);
    (dir, db)
}

fn get_position_calldata() -> Vec<u8> {
    let mut data = alloy_primitives::keccak256(b"getPosition(address,bytes32)")[..4].to_vec();
    let mut word = [0u8; 32];
    word[12..].copy_from_slice(ALICE.as_slice());
    data.extend_from_slice(&word);
    data.extend_from_slice(&U256::from(MARKET).to_be_bytes::<32>());
    data
}

fn tx(nonce: u64, data: Vec<u8>) -> TxEnv {
    TxEnv {
        caller: ALICE,
        gas_limit: 200_000,
        gas_price: block_cfg().base_fee as u128,
        kind: TxKind::Call(READER),
        data: Bytes::from(data),
        nonce,
        chain_id: Some(TORUS_CHAIN_ID),
        ..Default::default()
    }
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn assert_local_fault(r: Result<impl std::fmt::Debug, EvmError>, what: &str) {
    match r {
        Err(EvmError::LocalFault(msg)) => assert!(msg.contains("getPosition oracle read"), "{what}: {msg}"),
        other => panic!("{what}: expected EvmError::LocalFault, got {other:?}"),
    }
}

/// The oracle read fails on this node: the block's EVM execution aborts
/// with `EvmError::LocalFault` (block, overlay and single-tx paths). RED
/// before R02: the tx succeeded with UPnL 0.
#[test]
fn r02_get_position_oracle_read_fault_aborts_with_local_fault() {
    let (_d, db) = faulty_db();
    let ex = EvmExecutor::new(TORUS_CHAIN_ID);
    assert_local_fault(ex.execute_block(&db, &block_cfg(), vec![tx(0, get_position_calldata())], true), "block");
    let overlay = torus_state::StateOverlay::new(db.clone());
    assert_local_fault(
        ex.execute_block_with_overlay(&overlay, &block_cfg(), vec![tx(0, get_position_calldata())], true),
        "overlay",
    );
    assert_local_fault(ex.execute_tx(&db, &block_cfg(), tx(0, get_position_calldata())).map(|(r, _)| r), "tx");
}

/// A fault in a later tx aborts the whole block (no partial result).
#[test]
fn r02_local_fault_in_a_later_tx_aborts_the_block() {
    let (_d, db) = faulty_db();
    let txs = vec![tx(0, vec![0xDE, 0xAD, 0xBE, 0xEF]), tx(1, get_position_calldata())];
    assert_local_fault(EvmExecutor::new(TORUS_CHAIN_ID).execute_block(&db, &block_cfg(), txs, true), "block");
}

/// Absence (no aggregate row; a stale one; a short, undecodable one: every
/// validator reads the same bytes): UPnL 0, output and gas byte-identical
/// to main (captured with no row).
#[test]
fn r02_get_position_without_usable_oracle_row_unchanged() {
    let now = block_cfg().timestamp;
    let stale = [fp(150).raw().to_be_bytes().as_slice(), &1u64.to_be_bytes(), &3u32.to_be_bytes(), &(now - 61).to_be_bytes()].concat();
    for row in [None, Some(stale), Some(vec![1, 2, 3])] {
        let what = format!("row {row:?}");
        let (_d, db) = healthy_db();
        if let Some(row) = &row {
            db.put_cf_raw(CF_NATIVE_ORACLE, &[b"agg".as_slice(), &MARKET.to_be_bytes()].concat(), row).unwrap();
        }
        let ex = EvmExecutor::new(TORUS_CHAIN_ID);
        let (r, _) = ex.execute_tx(&db, &block_cfg(), tx(0, get_position_calldata())).unwrap();
        assert!(r.success, "{what}");
        assert_eq!(&r.output[64..96], &[0u8; 32], "{what}: UPnL word is 0");
        assert_eq!((hex(&r.output), r.gas_used), (MAIN_OUTPUT_HEX.to_string(), MAIN_GAS_USED), "{what}: vs main");
        let b = ex.execute_block(&db, &block_cfg(), vec![tx(0, get_position_calldata())], true).unwrap();
        assert_eq!((b.receipts.len(), b.receipts[0].status, b.gas_used), (1, true, MAIN_GAS_USED), "{what}");
    }
}

/// An ordinary precompile revert (unknown selector) stays a revert receipt,
/// on a healthy DB and on the faulty one (the oracle is never read): no
/// local fault.
#[test]
fn r02_ordinary_precompile_revert_is_not_a_local_fault() {
    for (what, (_d, db)) in [("healthy", healthy_db()), ("faulty", faulty_db())] {
        let b = EvmExecutor::new(TORUS_CHAIN_ID)
            .execute_block(&db, &block_cfg(), vec![tx(0, vec![0xDE, 0xAD, 0xBE, 0xEF])], true)
            .unwrap_or_else(|e| panic!("{what}: {e}"));
        assert_eq!((b.receipts.len(), b.receipts[0].status), (1, false), "{what}");
    }
}
