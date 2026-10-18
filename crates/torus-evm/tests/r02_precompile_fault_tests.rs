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

// ============================================================================
// F1 (s128, item 7 option 0 plan 2.2): every LOCAL storage fault a precompile
// hits is a fail-stop, not only `CoreError::PrecompileLocalFault`. A reader's
// point read or a writer's enqueue read that fails with `?` returns
// `CoreError::State` (a `CoreError::is_local_fault` class). Before the fix
// that was a revert on this node only (receipt false, gas spent, journal rows
// dropped): a divergence. Deterministic precompile errors (bad calldata, an
// unknown selector, value to a non-payable precompile, out of gas, "not
// found", a writer in eth_call) stay reverts, even on a faulty DB.
// ============================================================================

mod f1 {
    use super::*;
    use torus_core::precompiles::{
        precompile_address, ADDR_BALANCE_READER, ADDR_CORE_WRITER, ADDR_CORE_WRITER_STAKING,
        ADDR_LOCKBOX, ADDR_ORACLE_READER, ADDR_ORDER_BOOK_READER, ADDR_STAKING_READER,
    };
    use torus_state::cf::{
        CF_CORE_WRITER_QUEUE, CF_FEE_CONFIG, CF_NATIVE_BALANCES, CF_NATIVE_POSITIONS,
        CF_STAKING_PERMANENT,
    };

    /// A DB without the column family `missing` (every read of it fails on
    /// this node with `StateError::MissingColumnFamily`); ALICE funded.
    fn db_without(missing: &str) -> (tempfile::TempDir, StateDb) {
        let dir = tempfile::tempdir().unwrap();
        let mut opts = rocksdb::Options::default();
        opts.create_if_missing(true);
        opts.create_missing_column_families(true);
        let cfs = ALL_CF_NAMES.iter().copied().filter(|cf| *cf != missing);
        let db = StateDb::from_existing_db(rocksdb::DB::open_cf(&opts, dir.path(), cfs).unwrap());
        let funded = AccountInfo {
            balance: U256::from(10u128.pow(19)),
            nonce: 0,
            code_hash: KECCAK_EMPTY,
            code: None,
            account_id: None,
        };
        db.put_account(&ALICE, &funded).unwrap();
        (dir, db)
    }

    fn call(sig: &str, words: &[[u8; 32]]) -> Vec<u8> {
        let mut data = alloy_primitives::keccak256(sig.as_bytes())[..4].to_vec();
        for w in words {
            data.extend_from_slice(w);
        }
        data
    }

    fn addr_word(a: Address) -> [u8; 32] {
        let mut w = [0u8; 32];
        w[12..].copy_from_slice(a.as_slice());
        w
    }

    fn u_word(v: u128) -> [u8; 32] {
        U256::from(v).to_be_bytes::<32>()
    }

    fn tx_to(id: u16, data: Vec<u8>, value: U256, gas_limit: u64) -> TxEnv {
        TxEnv {
            caller: ALICE,
            gas_limit,
            gas_price: block_cfg().base_fee as u128,
            kind: TxKind::Call(precompile_address(id)),
            data: Bytes::from(data),
            value,
            nonce: 0,
            chain_id: Some(TORUS_CHAIN_ID),
            ..Default::default()
        }
    }

    fn expect_local_fault<T: std::fmt::Debug>(r: Result<T, EvmError>, cf: &str, what: &str) {
        match r {
            Err(EvmError::LocalFault(msg)) => {
                assert!(msg.contains(cf), "{what}: fault message names {cf}: {msg}")
            }
            other => panic!("{what}: expected EvmError::LocalFault, got {other:?}"),
        }
    }

    /// (case, the missing CF, precompile, calldata): each call's first read
    /// of `cf` is a `?` on a point read / scan that the CF's absence fails.
    fn fault_cases() -> Vec<(&'static str, &'static str, u16, Vec<u8>)> {
        let alice = addr_word(ALICE);
        let market = u_word(MARKET as u128);
        vec![
            ("0x0800 getPosition position read", CF_NATIVE_POSITIONS, ADDR_ORDER_BOOK_READER, call("getPosition(address,bytes32)", &[alice, market])),
            ("0x0801 getBalances balance read", CF_NATIVE_BALANCES, ADDR_BALANCE_READER, call("getBalances(address)", &[alice])),
            ("0x0802 getPrice aggregate read", CF_NATIVE_ORACLE, ADDR_ORACLE_READER, call("getPrice(bytes32)", &[market])),
            ("0x0803 getStakingInfo permanent read", CF_STAKING_PERMANENT, ADDR_STAKING_READER, call("getStakingInfo(address)", &[alice])),
            ("0x0810 placeOrder delay read", CF_FEE_CONFIG, ADDR_CORE_WRITER, call("placeOrder(bytes32,uint8,uint8,uint128,uint128,uint8)", &[market, u_word(0), u_word(0), u_word(1), u_word(1), u_word(0)])),
            ("0x0810 cancelOrder queue read", CF_CORE_WRITER_QUEUE, ADDR_CORE_WRITER, call("cancelOrder(bytes32)", &[u_word(1)])),
            ("0x0811 claimRewards queue read", CF_CORE_WRITER_QUEUE, ADDR_CORE_WRITER_STAKING, call("claimRewards()", &[])),
            ("0x0820 withdrawFromNative queue read", CF_CORE_WRITER_QUEUE, ADDR_LOCKBOX, call("withdrawFromNative(uint128)", &[u_word(10u128.pow(10))])),
        ]
    }

    /// A precompile read that fails on this node aborts the block's EVM
    /// execution with `EvmError::LocalFault` on every consensus path (block,
    /// overlay, single tx): no receipt, no bundle. RED before F1: a revert
    /// receipt (status false) on this node only.
    #[test]
    fn f1_precompile_read_fault_is_a_local_fault() {
        for (what, cf, id, data) in fault_cases() {
            let (_d, db) = db_without(cf);
            let ex = EvmExecutor::new(TORUS_CHAIN_ID);
            let tx = || tx_to(id, data.clone(), U256::ZERO, 200_000);
            expect_local_fault(ex.execute_block(&db, &block_cfg(), vec![tx()], true), cf, &format!("{what} (block)"));
            let overlay = torus_state::StateOverlay::new(db.clone());
            expect_local_fault(
                ex.execute_block_with_overlay(&overlay, &block_cfg(), vec![tx()], true),
                cf,
                &format!("{what} (overlay)"),
            );
            expect_local_fault(ex.execute_tx(&db, &block_cfg(), tx()).map(|(r, _)| r), cf, &format!("{what} (tx)"));
        }
    }

    /// eth_call (read-only provider): a reader's read fault is an error, not
    /// a revert answer (RPC has no fail-stop; it returns the error).
    #[test]
    fn f1_reader_read_fault_in_eth_call_is_an_error() {
        for (what, cf, id, data) in fault_cases().into_iter().filter(|c| c.2 < ADDR_CORE_WRITER) {
            let (_d, db) = db_without(cf);
            let r = EvmExecutor::new(TORUS_CHAIN_ID).execute_call(&db, &block_cfg(), tx_to(id, data, U256::ZERO, 200_000));
            expect_local_fault(r.map(|(r, _)| r), cf, &format!("{what} (eth_call)"));
        }
    }

    /// The same faulty DBs, calls that fail deterministically BEFORE any
    /// read of the missing CF (or that find nothing on a healthy DB): each
    /// stays a revert receipt / failed result, never a local fault.
    #[test]
    fn f1_deterministic_precompile_errors_stay_reverts() {
        let alice = addr_word(ALICE);
        let market = u_word(MARKET as u128);
        let place = |side: u128| call("placeOrder(bytes32,uint8,uint8,uint128,uint128,uint8)", &[market, u_word(side), u_word(0), u_word(1), u_word(1), u_word(0)]);
        let cases: Vec<(&str, Option<&str>, TxEnv)> = vec![
            ("short calldata", Some(CF_NATIVE_BALANCES), tx_to(ADDR_BALANCE_READER, vec![0xDE, 0xAD], U256::ZERO, 200_000)),
            ("unknown selector", Some(CF_NATIVE_BALANCES), tx_to(ADDR_BALANCE_READER, vec![0xDE, 0xAD, 0xBE, 0xEF], U256::ZERO, 200_000)),
            ("missing word", Some(CF_NATIVE_BALANCES), tx_to(ADDR_BALANCE_READER, call("getBalances(address)", &[]), U256::ZERO, 200_000)),
            ("value to a non-payable reader", Some(CF_NATIVE_BALANCES), tx_to(ADDR_BALANCE_READER, call("getBalances(address)", &[alice]), U256::from(1), 200_000)),
            ("out of gas before the read", Some(CF_NATIVE_BALANCES), tx_to(ADDR_BALANCE_READER, call("getBalances(address)", &[alice]), U256::ZERO, 25_000)),
            ("invalid side before the enqueue", Some(CF_FEE_CONFIG), tx_to(ADDR_CORE_WRITER, place(2), U256::ZERO, 200_000)),
            ("no oracle price (not found)", None, tx_to(ADDR_ORACLE_READER, call("getPrice(bytes32)", &[market]), U256::ZERO, 200_000)),
        ];
        for (what, missing, tx) in cases {
            let (_d, db) = match missing {
                Some(cf) => db_without(cf),
                None => db_without("no-such-cf"),
            };
            let b = EvmExecutor::new(TORUS_CHAIN_ID)
                .execute_block(&db, &block_cfg(), vec![tx], true)
                .unwrap_or_else(|e| panic!("{what}: {e:?}"));
            assert_eq!((b.receipts.len(), b.receipts[0].status), (1, false), "{what}");
        }
        // A writer in eth_call is denied before it reads: a failed result.
        let (_d, db) = db_without(CF_CORE_WRITER_QUEUE);
        let (r, _) = EvmExecutor::new(TORUS_CHAIN_ID)
            .execute_call(&db, &block_cfg(), tx_to(ADDR_CORE_WRITER, call("cancelOrder(bytes32)", &[u_word(1)]), U256::ZERO, 200_000))
            .expect("writer in eth_call: a revert, not an error");
        assert!(!r.success, "writer in eth_call reverts");
    }

    /// Healthy control: the fault cases succeed on a DB with every CF (so
    /// the fault, not the call, is what fails above).
    #[test]
    fn f1_fault_cases_succeed_on_a_healthy_db() {
        for (what, _cf, id, data) in fault_cases() {
            if id == ADDR_ORACLE_READER {
                continue; // no aggregate row: a deterministic NoOraclePrice revert
            }
            let (_d, db) = db_without("no-such-cf");
            let b = EvmExecutor::new(TORUS_CHAIN_ID)
                .execute_block(&db, &block_cfg(), vec![tx_to(id, data, U256::ZERO, 200_000)], true)
                .unwrap_or_else(|e| panic!("{what}: {e:?}"));
            assert_eq!((b.receipts.len(), b.receipts[0].status), (1, true), "{what}");
        }
    }

    // ------------------------------------------------------------------------
    // F1 Codex review P1: the SCAN readers. `scan_prefix_metered` read a
    // missing CF as empty, so getMarkets / getAllPrices / getStakingInfo /
    // getValidators answered "nothing" with a SUCCESS receipt and a gas
    // charge only this node computes (Codex repro: getMarkets healthy
    // status=true gas=38084; missing cf_native_markets status=true
    // gas=37544). A scan of a CF this node lacks is a local fault like a
    // point read; an existing but empty CF is still the normal empty answer.
    // ------------------------------------------------------------------------

    /// (case, the scanned CF, precompile, calldata, the empty-CF answer).
    type ScanCase = (&'static str, &'static str, u16, Vec<u8>, Vec<u8>);

    /// Every reader that scans (`scan_prefix_metered`).
    fn scan_cases() -> Vec<ScanCase> {
        use torus_core::precompiles::abi::encode_arrays_response;
        use torus_state::cf::{CF_NATIVE_MARKETS, CF_STAKING_DELEGATIONS, CF_STAKING_VALIDATORS};
        vec![
            (
                "0x0801 getMarkets scan",
                CF_NATIVE_MARKETS,
                ADDR_BALANCE_READER,
                call("getMarkets()", &[]),
                encode_arrays_response(&[&[], &[]]),
            ),
            (
                "0x0802 getAllPrices scan",
                CF_NATIVE_ORACLE,
                ADDR_ORACLE_READER,
                call("getAllPrices()", &[]),
                encode_arrays_response(&[&[], &[], &[]]),
            ),
            (
                "0x0803 getStakingInfo delegation scan",
                CF_STAKING_DELEGATIONS,
                ADDR_STAKING_READER,
                call("getStakingInfo(address)", &[addr_word(ALICE)]),
                vec![0u8; 128],
            ),
            (
                "0x0803 getValidators scan",
                CF_STAKING_VALIDATORS,
                ADDR_STAKING_READER,
                call("getValidators()", &[]),
                encode_arrays_response(&[&[], &[], &[]]),
            ),
        ]
    }

    /// A scan of a CF this node's DB lacks aborts the execution with
    /// `EvmError::LocalFault` on every consensus path (block, overlay, tx)
    /// and in eth_call: no receipt, never an "empty" success. RED before the
    /// fix: a success receipt with node-dependent gas.
    #[test]
    fn f1_precompile_scan_fault_is_a_local_fault() {
        for (what, cf, id, data, _) in scan_cases() {
            let (_d, db) = db_without(cf);
            let ex = EvmExecutor::new(TORUS_CHAIN_ID);
            let tx = || tx_to(id, data.clone(), U256::ZERO, 200_000);
            expect_local_fault(
                ex.execute_block(&db, &block_cfg(), vec![tx()], true),
                cf,
                &format!("{what} (block)"),
            );
            let overlay = torus_state::StateOverlay::new(db.clone());
            expect_local_fault(
                ex.execute_block_with_overlay(&overlay, &block_cfg(), vec![tx()], true),
                cf,
                &format!("{what} (overlay)"),
            );
            expect_local_fault(
                ex.execute_tx(&db, &block_cfg(), tx()).map(|(r, _)| r),
                cf,
                &format!("{what} (tx)"),
            );
            expect_local_fault(
                ex.execute_call(&db, &block_cfg(), tx()).map(|(r, _)| r),
                cf,
                &format!("{what} (eth_call)"),
            );
        }
    }

    /// Control: on a DB where the scanned CF exists but is empty, each scan
    /// reader still succeeds with the normal empty answer (block receipt
    /// true; eth_call output byte-identical to the empty encoding), the same
    /// gas on the block and eth_call paths.
    #[test]
    fn f1_scan_of_an_empty_cf_is_the_empty_answer() {
        for (what, _cf, id, data, empty) in scan_cases() {
            let (_d, db) = db_without("no-such-cf");
            let ex = EvmExecutor::new(TORUS_CHAIN_ID);
            let b = ex
                .execute_block(
                    &db,
                    &block_cfg(),
                    vec![tx_to(id, data.clone(), U256::ZERO, 200_000)],
                    true,
                )
                .unwrap_or_else(|e| panic!("{what}: {e:?}"));
            assert_eq!(
                (b.receipts.len(), b.receipts[0].status),
                (1, true),
                "{what}"
            );
            let (r, _) = ex
                .execute_call(&db, &block_cfg(), tx_to(id, data, U256::ZERO, 200_000))
                .unwrap_or_else(|e| panic!("{what} (eth_call): {e:?}"));
            assert!(r.success, "{what} (eth_call)");
            assert_eq!(r.output, empty, "{what}: the empty answer");
        }
    }

    /// Out of gas before the scan stays a revert, even on a DB without the
    /// scanned CF (the meter refuses before any read).
    #[test]
    fn f1_scan_out_of_gas_before_the_read_stays_a_revert() {
        for (what, cf, id, data, _) in scan_cases() {
            let (_d, db) = db_without(cf);
            let b = EvmExecutor::new(TORUS_CHAIN_ID)
                .execute_block(
                    &db,
                    &block_cfg(),
                    vec![tx_to(id, data, U256::ZERO, 25_000)],
                    true,
                )
                .unwrap_or_else(|e| panic!("{what}: {e:?}"));
            assert_eq!(
                (b.receipts.len(), b.receipts[0].status),
                (1, false),
                "{what}"
            );
        }
    }
}
