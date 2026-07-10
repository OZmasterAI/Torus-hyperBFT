//! D3/D5 (S392): per-tx decode isolation and receipt alignment on the
//! committed-block execution path.
//!
//! A block consensus has already committed must never lose ALL its EVM
//! effects to one bad transaction (whole-block poisoning, app.rs:285-288),
//! and when a tx is skipped mid-block every surviving receipt must still
//! carry the hash and original body index of the tx that produced it.

use alloy_consensus::{SignableTransaction, TxEip1559, TxEnvelope};
use alloy_primitives::{Address, Bytes, Signature as AlloySig, TxKind, B256, U256};
use alloy_rlp::Encodable;
use k256::ecdsa::SigningKey;
use revm::state::AccountInfo;

use torus_bridge::{BlockCommitter, BlockValidator};
use torus_evm::{EvmExecutor, TORUS_CHAIN_ID};
use torus_state::StateDb;
use torus_types::{TorusBlock, TorusBlockHeader};

const KECCAK_EMPTY: B256 = B256::new([
    0xc5, 0xd2, 0x46, 0x01, 0x86, 0xf7, 0x23, 0x3c, 0x92, 0x7e, 0x7d, 0xb2, 0xdc, 0xc7, 0x03, 0xc0,
    0xe5, 0x00, 0xb6, 0x53, 0xca, 0x82, 0x27, 0x3b, 0x7b, 0xfa, 0xd8, 0x04, 0x5d, 0x85, 0xa4, 0x70,
]);

fn open_test_db() -> (tempfile::TempDir, StateDb) {
    let dir = tempfile::tempdir().expect("create temp dir");
    let db = StateDb::open(dir.path()).expect("open db");
    (dir, db)
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

/// Sign a 21k-gas transfer at exactly the 1-gwei base fee; returns (raw, hash).
fn signed_transfer(key: &SigningKey, nonce: u64, value: U256) -> (Vec<u8>, B256) {
    let tx = TxEip1559 {
        chain_id: TORUS_CHAIN_ID,
        nonce,
        max_fee_per_gas: 1_000_000_000,
        max_priority_fee_per_gas: 0,
        gas_limit: 21_000,
        to: TxKind::Call(Address::repeat_byte(0x42)),
        value,
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
    let hash = *envelope.tx_hash();
    let mut buf = Vec::new();
    envelope.encode(&mut buf);
    (buf, hash)
}

/// Header for a committed (catchup-validated) block — root checks are skipped
/// on that path, so roots can stay zero.
fn block_with_txs(evm_transactions: Vec<Vec<u8>>) -> TorusBlock {
    TorusBlock {
        header: TorusBlockHeader {
            height: 1,
            parent_hash: B256::ZERO,
            timestamp: 1000,
            proposer: Address::repeat_byte(0x99),
            state_root: B256::ZERO,
            receipts_root: B256::ZERO,
            logs_bloom: alloy_primitives::Bloom::ZERO,
            evm_gas_used: 0,
            evm_fee_revenue: 0,
            evm_gas_limit: 30_000_000,
            native_action_count: 0,
            evm_tx_count: evm_transactions.len() as u32,
            base_fee_per_gas: 1_000_000_000,
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
fn undecodable_tx_skips_only_itself_on_catchup() {
    let (_dir, db) = open_test_db();
    let alice = SigningKey::from_slice(&[0x11; 32]).unwrap();
    let bob = SigningKey::from_slice(&[0x22; 32]).unwrap();
    fund(&db, &address_from_key(&alice));
    fund(&db, &address_from_key(&bob));

    let (t1, h1) = signed_transfer(&alice, 0, U256::from(1000));
    let (t2, h2) = signed_transfer(&bob, 0, U256::from(2000));
    // Type-3 prefix + garbage: fails envelope decode entirely.
    let junk = vec![0x03, 0xde, 0xad, 0xbe, 0xef];

    let block = block_with_txs(vec![t1, junk, t2]);
    let validated = validator()
        .validate_block_for_catchup(&block, &db, &EvmExecutor::new(TORUS_CHAIN_ID))
        .expect("one bad tx must not poison the whole block's EVM execution");

    assert_eq!(
        validated.receipts.len(),
        2,
        "both valid txs execute + receipt"
    );
    assert_eq!(validated.receipts[0].tx_hash, h1);
    assert_eq!(validated.receipts[1].tx_hash, h2);
    assert_eq!(validated.receipts[0].tx_index, 0);
    assert_eq!(
        validated.receipts[1].tx_index, 2,
        "receipt keeps the ORIGINAL body index, not the dense exec index"
    );
}

#[test]
fn exec_skipped_tx_leaves_aligned_receipts() {
    let (_dir, db) = open_test_db();
    let alice = SigningKey::from_slice(&[0x11; 32]).unwrap();
    let mallory = SigningKey::from_slice(&[0x33; 32]).unwrap(); // never funded
    let bob = SigningKey::from_slice(&[0x22; 32]).unwrap();
    fund(&db, &address_from_key(&alice));
    fund(&db, &address_from_key(&bob));

    let (t1, h1) = signed_transfer(&alice, 0, U256::from(1000));
    let (t2, h2) = signed_transfer(&mallory, 0, U256::from(1)); // exec-time skip: no funds
    let (t3, h3) = signed_transfer(&bob, 0, U256::from(2000));

    let block = block_with_txs(vec![t1, t2, t3]);
    let validated = validator()
        .validate_block_for_catchup(&block, &db, &EvmExecutor::new(TORUS_CHAIN_ID))
        .expect("catchup execution must succeed with one exec-skipped tx");

    assert_eq!(validated.receipts.len(), 2, "skipped tx gets NO receipt");
    assert_eq!(validated.receipts[0].tx_hash, h1);
    assert_eq!(
        validated.receipts[1].tx_hash, h3,
        "receipt after the skip must carry the LATER tx's hash, not the skipped one's"
    );
    assert_ne!(validated.receipts[1].tx_hash, h2);
    assert_eq!(validated.receipts[1].tx_index, 2);
}

#[test]
fn tx_location_index_follows_receipts_under_skips() {
    let (_dir, db) = open_test_db();
    let alice = SigningKey::from_slice(&[0x11; 32]).unwrap();
    let mallory = SigningKey::from_slice(&[0x33; 32]).unwrap(); // never funded
    let bob = SigningKey::from_slice(&[0x22; 32]).unwrap();
    fund(&db, &address_from_key(&alice));
    fund(&db, &address_from_key(&bob));

    let (t1, _h1) = signed_transfer(&alice, 0, U256::from(1000));
    let (t2, h2) = signed_transfer(&mallory, 0, U256::from(1));
    let (t3, h3) = signed_transfer(&bob, 0, U256::from(2000));

    let block = block_with_txs(vec![t1, t2, t3]);
    let validated = validator()
        .validate_block_for_catchup(&block, &db, &EvmExecutor::new(TORUS_CHAIN_ID))
        .unwrap();
    BlockCommitter::commit_block_metadata(&db, &block, &validated.receipts).unwrap();

    // t3's location must point at its ORIGINAL body slot (height 1, index 2) so
    // eth_getTransactionByHash / getTransactionReceipt resolve body + receipt.
    let loc = db
        .get_cf_raw(torus_state::cf::CF_TX_HASH_TO_LOCATION, h3.as_slice())
        .unwrap()
        .expect("included tx must have a location index");
    assert_eq!(u64::from_be_bytes(loc[..8].try_into().unwrap()), 1);
    assert_eq!(u32::from_be_bytes(loc[8..12].try_into().unwrap()), 2);

    // Receipt stored under (height, original index) — present at 2, absent at 1.
    let mut key = [0u8; 12];
    key[..8].copy_from_slice(&1u64.to_be_bytes());
    key[8..12].copy_from_slice(&2u32.to_be_bytes());
    assert!(db
        .get_cf_raw(torus_state::cf::CF_RECEIPTS, &key)
        .unwrap()
        .is_some());
    key[8..12].copy_from_slice(&1u32.to_be_bytes());
    assert!(
        db.get_cf_raw(torus_state::cf::CF_RECEIPTS, &key)
            .unwrap()
            .is_none(),
        "skipped tx gets no receipt (honest null)"
    );

    // The skipped tx must have NO location entry.
    assert!(db
        .get_cf_raw(torus_state::cf::CF_TX_HASH_TO_LOCATION, h2.as_slice())
        .unwrap()
        .is_none());
}
