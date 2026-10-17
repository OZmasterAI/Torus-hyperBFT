//! Item 7 step 2 (B8): execution charges each sender its lane's base fee, and a big height
//! executes small + big gas. `docs/plans/item7-step2-impl.md` B8.
//!
//! Lane at execution (owner answer 7): the sender's `EvmUserModify` row when the block executes;
//! big iff the row exists and the executing header's M > 0.

use alloy_consensus::{SignableTransaction, TxEip1559, TxEnvelope};
use alloy_primitives::{Address, Bytes, Signature as AlloySig, TxKind, B256, U256};
use alloy_rlp::Encodable;
use k256::ecdsa::SigningKey;
use revm::state::AccountInfo;

use torus_bridge::BlockValidator;
use torus_evm::{EvmExecutor, TORUS_CHAIN_ID};
use torus_state::cf::{evm_lane_key, CF_NATIVE_ORACLE};
use torus_state::StateDb;
use torus_types::{TorusBlock, TorusBlockHeader};

const KECCAK_EMPTY: B256 = B256::new([
    0xc5, 0xd2, 0x46, 0x01, 0x86, 0xf7, 0x23, 0x3c, 0x92, 0x7e, 0x7d, 0xb2, 0xdc, 0xc7, 0x03, 0xc0,
    0xe5, 0x00, 0xb6, 0x53, 0xca, 0x82, 0x27, 0x3b, 0x7b, 0xfa, 0xd8, 0x04, 0x5d, 0x85, 0xa4, 0x70,
]);
const GWEI: u64 = 1_000_000_000;

fn open_test_db() -> (tempfile::TempDir, StateDb) {
    let dir = tempfile::tempdir().expect("create temp dir");
    let db = StateDb::open(dir.path()).expect("open db");
    (dir, db)
}

fn key(n: u8) -> SigningKey {
    SigningKey::from_slice(&[n; 32]).unwrap()
}

fn address_from_key(key: &SigningKey) -> Address {
    let pubkey = key.verifying_key().to_encoded_point(false);
    let hash = alloy_primitives::keccak256(&pubkey.as_bytes()[1..]);
    Address::from_slice(&hash[12..])
}

fn fund(db: &StateDb, addr: &Address) {
    db.put_account(
        addr,
        &AccountInfo {
            balance: U256::from(1_000_000_000_000_000_000u128), // 1 ETH
            nonce: 0,
            code_hash: KECCAK_EMPTY,
            code: None,
            account_id: None,
        },
    )
    .unwrap();
}

/// A transfer at max fee 5 gwei, no tip, with `gas_limit`.
fn signed_transfer(key: &SigningKey, nonce: u64, gas_limit: u64) -> Vec<u8> {
    let tx = TxEip1559 {
        chain_id: TORUS_CHAIN_ID,
        nonce,
        max_fee_per_gas: 5 * u128::from(GWEI),
        max_priority_fee_per_gas: 0,
        gas_limit,
        to: TxKind::Call(Address::repeat_byte(0x42)),
        value: U256::from(1u64),
        input: Bytes::new(),
        access_list: Default::default(),
    };
    let sig_hash = tx.signature_hash();
    let (sig, recid) = key.sign_prehash_recoverable(sig_hash.as_slice()).unwrap();
    let signature = AlloySig::new(
        U256::from_be_slice(sig.r().to_bytes().as_slice()),
        U256::from_be_slice(sig.s().to_bytes().as_slice()),
        recid.is_y_odd(),
    );
    let envelope = TxEnvelope::Eip1559(tx.into_signed(signature));
    let mut buf = Vec::new();
    envelope.encode(&mut buf);
    buf
}

/// A committed block at `height`: N = 5, M = `m`, small 3M, big 10M, small fee 1 gwei, big fee
/// 3 gwei. Roots stay zero (the catch-up path skips the root checks).
fn block(height: u64, m: u64, evm_transactions: Vec<Vec<u8>>) -> TorusBlock {
    TorusBlock {
        header: TorusBlockHeader {
            height,
            parent_hash: B256::ZERO,
            timestamp: 1000 + height,
            proposer: Address::repeat_byte(0x99),
            state_root: B256::ZERO,
            receipts_root: B256::ZERO,
            logs_bloom: alloy_primitives::Bloom::ZERO,
            evm_gas_used: 0,
            evm_fee_revenue: 0,
            evm_gas_limit: 3_000_000,
            evm_block_interval: 5,
            evm_big_block_interval: m,
            evm_big_gas_limit: 10_000_000,
            native_action_count: 0,
            evm_tx_count: evm_transactions.len() as u32,
            base_fee_per_gas: GWEI,
            big_base_fee_per_gas: 3 * GWEI,
            epoch: 0,
            validator_set_hash: B256::ZERO,
            sig_attestation: [0u8; 64],
        },
        native_actions: vec![],
        evm_transactions,
        core_writer_actions: vec![],
    }
}

fn validator() -> BlockValidator {
    BlockValidator::new(TORUS_CHAIN_ID, 100, 4, Address::ZERO, Address::ZERO)
}

#[test]
fn catchup_validation_charges_each_sender_its_lane_while_big_blocks_are_on() {
    let (ka, kb) = (key(0x11), key(0x22));
    let (a, b) = (address_from_key(&ka), address_from_key(&kb));
    let run = |m: u64| {
        let (_dir, db) = open_test_db();
        fund(&db, &a);
        fund(&db, &b);
        db.put_cf_raw(CF_NATIVE_ORACLE, &evm_lane_key(&b), &[1])
            .unwrap();
        let txs = vec![
            signed_transfer(&ka, 0, 21_000),
            signed_transfer(&kb, 0, 21_000),
        ];
        let v = validator()
            .validate_block_for_catchup(&block(100, m, txs), &db, &EvmExecutor::new(TORUS_CHAIN_ID))
            .unwrap();
        assert_eq!(v.receipts.len(), 2);
        (
            v.fee_revenue,
            v.gas_used,
            v.big_gas_used,
            v.receipts[1].effective_gas_price,
        )
    };
    let g = u128::from(GWEI);
    assert_eq!(
        run(50),
        (21_000 * g + 21_000 * 3 * g, 42_000, 21_000, 3 * GWEI)
    );
    // M = 0: big blocks off, the row is ignored; both pay the small fee.
    assert_eq!(run(0), (42_000 * g, 42_000, 0, GWEI));
}

#[test]
fn a_tx_above_the_small_limit_runs_only_at_a_big_height() {
    let k = key(0x33);
    let sender = address_from_key(&k);
    let run = |height: u64| {
        let (_dir, db) = open_test_db();
        fund(&db, &sender);
        let v = validator()
            .validate_block_for_catchup(
                &block(height, 50, vec![signed_transfer(&k, 0, 5_000_000)]),
                &db,
                &EvmExecutor::new(TORUS_CHAIN_ID),
            )
            .unwrap();
        v.receipts.len()
    };
    // Height 100: cap 3M + 10M, the 5M-gas-limit tx runs.
    assert_eq!(run(100), 1);
    // Height 105 (small cadence only): cap 3M, revm refuses it (above the block gas limit).
    assert_eq!(run(105), 0);
}
