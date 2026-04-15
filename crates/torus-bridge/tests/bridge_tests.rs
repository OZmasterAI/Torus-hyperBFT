//! Integration tests for the torus-bridge consensus-execution pipeline.
//!
//! Tests cover:
//! - RLP transaction decoding (1.5.2 dependency)
//! - Block proposal + validation + commit pipeline (1.5.1-1.5.3)
//! - State root determinism (1.5.5)
//! - Multi-block chain (state continuity)
//! - validate_block_for_sync (1.5.6)

use alloy_consensus::TxEip1559;
use alloy_primitives::{Address, Bytes, Signature as AlloySig, TxKind, B256, U256};
use alloy_rlp::Encodable;
use k256::ecdsa::SigningKey;
use revm::state::AccountInfo;

use torus_bridge::{
    genesis_parent_header, BlockCommitter, BlockProposer, BlockValidator, ProposedBlock,
};
use torus_evm::{EvmExecutor, TORUS_CHAIN_ID};
use torus_state::StateDb;
use torus_types::TorusBlockHeader;

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

fn test_account(balance: U256) -> AccountInfo {
    AccountInfo {
        balance,
        nonce: 0,
        code_hash: KECCAK_EMPTY,
        code: None,
        account_id: None,
    }
}

fn test_signing_key(seed: u8) -> SigningKey {
    let mut secret = [0u8; 32];
    secret[31] = seed;
    secret[0] = 0x01; // ensure nonzero
    SigningKey::from_bytes((&secret).into()).expect("valid key")
}

fn signing_key_address(sk: &SigningKey) -> Address {
    use k256::ecdsa::VerifyingKey;
    let vk = VerifyingKey::from(sk);
    let uncompressed = vk.to_encoded_point(false);
    let hash = alloy_primitives::keccak256(&uncompressed.as_bytes()[1..]);
    Address::from_slice(&hash[12..])
}

/// Build and RLP-encode a signed EIP-1559 transfer transaction.
fn build_signed_transfer(
    sk: &SigningKey,
    to: Address,
    value: U256,
    nonce: u64,
    max_fee_per_gas: u128,
) -> Vec<u8> {
    let tx = TxEip1559 {
        chain_id: TORUS_CHAIN_ID,
        nonce,
        gas_limit: 21_000,
        max_fee_per_gas,
        max_priority_fee_per_gas: 0,
        to: TxKind::Call(to),
        value,
        access_list: Default::default(),
        input: Bytes::new(),
    };

    // Compute signing hash.
    use alloy_consensus::SignableTransaction;
    let sig_hash = tx.signature_hash();

    // Sign with k256.
    let (sig, rec_id) = sk
        .sign_prehash_recoverable(sig_hash.as_ref())
        .expect("sign");
    let sig_bytes: [u8; 64] = sig.to_bytes().into();
    let r = U256::from_be_slice(&sig_bytes[..32]);
    let s = U256::from_be_slice(&sig_bytes[32..]);
    let v = rec_id.is_y_odd();
    let prim_sig = AlloySig::new(r, s, v);

    let signed = tx.into_signed(prim_sig);
    let envelope = alloy_consensus::TxEnvelope::Eip1559(signed);

    let mut buf = Vec::new();
    envelope.encode(&mut buf);
    buf
}

struct TestHarness {
    _dir: tempfile::TempDir,
    db: StateDb,
    proposer: BlockProposer,
    validator: BlockValidator,
    executor: EvmExecutor,
    parent_header: TorusBlockHeader,
}

impl TestHarness {
    fn new() -> Self {
        let (dir, db) = open_test_db();
        Self {
            _dir: dir,
            db,
            proposer: BlockProposer::new(TORUS_CHAIN_ID, 100, 4, Address::ZERO, Address::ZERO),
            validator: BlockValidator::new(TORUS_CHAIN_ID, 100, 4, Address::ZERO, Address::ZERO),
            executor: EvmExecutor::new(TORUS_CHAIN_ID),
            parent_header: genesis_parent_header(),
        }
    }

    fn propose(&self, evm_txs: Vec<Vec<u8>>, proposer: Address) -> ProposedBlock {
        self.proposer
            .build_block(
                &self.db,
                &self.executor,
                &self.parent_header,
                evm_txs,
                1_000_000,
                proposer,
            )
            .expect("proposal should succeed")
    }
}

// ---- Tests ----

// 1. RLP decode round-trip
#[test]
fn decode_signed_eip1559_transfer() {
    let sk = test_signing_key(1);
    let sender = signing_key_address(&sk);
    let bob = Address::new([0xBB; 20]);

    let rlp = build_signed_transfer(&sk, bob, U256::from(1_000u64), 0, 1_000_000_000);

    let decoded = torus_bridge::decode_rlp_tx(&rlp).expect("decode should succeed");

    assert_eq!(decoded.sender, sender, "recovered sender should match");
    assert_eq!(decoded.tx_env.caller, sender);
    assert_eq!(decoded.tx_env.value, U256::from(1_000u64));
    assert_eq!(decoded.tx_env.gas_limit, 21_000);
    assert_ne!(decoded.tx_hash, B256::ZERO, "tx hash should be nonzero");
}

// 2. Build and validate an empty block
#[test]
fn propose_and_validate_empty_block() {
    let h = TestHarness::new();
    let proposer_addr = Address::with_last_byte(0xFF);

    let proposed = h.propose(vec![], proposer_addr);
    let block = &proposed.block;

    assert_eq!(block.header.height, 1);
    assert_eq!(block.header.evm_tx_count, 0);
    assert_eq!(block.header.evm_gas_used, 0);
    assert_ne!(block.header.state_root, B256::ZERO);

    // Validate
    let validated = h
        .validator
        .validate_block(block, &h.db, &h.executor)
        .expect("empty block should validate");
    assert_eq!(validated.state_root, block.header.state_root);
}

// 3. Propose, validate, and commit a block with a transfer
#[test]
fn full_pipeline_single_transfer() {
    let h = TestHarness::new();
    let sk = test_signing_key(1);
    let alice = signing_key_address(&sk);
    let bob = Address::new([0xBB; 20]);

    // Fund Alice.
    let ten_eth = U256::from(10_000_000_000_000_000_000u128);
    h.db.put_account(&alice, &test_account(ten_eth)).unwrap();

    let base_fee = h.parent_header.base_fee_per_gas as u128;
    let rlp = build_signed_transfer(&sk, bob, U256::from(1_000_000u64), 0, base_fee);

    // Propose
    let proposed = h.propose(vec![rlp], Address::with_last_byte(0xFF));
    let block = &proposed.block;
    assert_eq!(block.header.evm_tx_count, 1);
    assert_eq!(block.header.evm_gas_used, 21_000);

    // Validate
    let validated = h
        .validator
        .validate_block(block, &h.db, &h.executor)
        .expect("block with transfer should validate");
    assert_eq!(validated.state_root, block.header.state_root);
    assert_eq!(validated.receipts.len(), 1);
    assert!(validated.receipts[0].status, "transfer should succeed");

    // Commit
    let block_hash =
        BlockCommitter::commit_block(&h.db, block, &validated.bundle, &validated.receipts)
            .expect("commit should succeed");
    assert_ne!(block_hash, B256::ZERO);

    // Verify state was persisted: Bob should have received value.
    let bob_acct = h.db.get_account(&bob).unwrap();
    assert!(
        bob_acct.is_some(),
        "Bob's account should exist after commit"
    );
    assert_eq!(bob_acct.unwrap().balance, U256::from(1_000_000u64));

    // Verify block stored in CF.
    let stored =
        h.db.get_cf_raw(torus_state::cf::CF_BLOCK_HEADERS, &1u64.to_be_bytes())
            .unwrap();
    assert!(stored.is_some(), "block header should be stored");
}

// 4. State root determinism — two independent validators agree
#[test]
fn state_root_determinism_two_validators() {
    let sk = test_signing_key(1);
    let alice = signing_key_address(&sk);
    let bob = Address::new([0xBB; 20]);
    let ten_eth = U256::from(10_000_000_000_000_000_000u128);

    // Validator 1
    let (dir1, db1) = open_test_db();
    db1.put_account(&alice, &test_account(ten_eth)).unwrap();

    // Validator 2 (independent DB, same initial state)
    let (dir2, db2) = open_test_db();
    db2.put_account(&alice, &test_account(ten_eth)).unwrap();

    let parent = genesis_parent_header();
    let proposer = BlockProposer::new(TORUS_CHAIN_ID, 100, 4, Address::ZERO, Address::ZERO);
    let executor = EvmExecutor::new(TORUS_CHAIN_ID);
    let validator = BlockValidator::new(TORUS_CHAIN_ID, 100, 4, Address::ZERO, Address::ZERO);

    let base_fee = parent.base_fee_per_gas as u128;
    let rlp = build_signed_transfer(&sk, bob, U256::from(1_000_000u64), 0, base_fee);

    // Propose on validator 1
    let proposed = proposer
        .build_block(
            &db1,
            &executor,
            &parent,
            vec![rlp.clone()],
            1_000_000,
            Address::ZERO,
        )
        .expect("propose on v1");

    // Validate on validator 1
    let v1 = validator
        .validate_block(&proposed.block, &db1, &executor)
        .expect("validate on v1");

    // Validate on validator 2
    let v2 = validator
        .validate_block(&proposed.block, &db2, &executor)
        .expect("validate on v2");

    assert_eq!(
        v1.state_root, v2.state_root,
        "both validators must compute the same state root"
    );
    assert_eq!(v1.receipts.len(), v2.receipts.len());

    drop(dir1);
    drop(dir2);
}

// 5. Multi-block chain — state carries forward
#[test]
fn multi_block_chain() {
    let h = TestHarness::new();
    let sk = test_signing_key(1);
    let alice = signing_key_address(&sk);
    let bob = Address::new([0xBB; 20]);

    let ten_eth = U256::from(10_000_000_000_000_000_000u128);
    h.db.put_account(&alice, &test_account(ten_eth)).unwrap();

    let base_fee = h.parent_header.base_fee_per_gas as u128;
    let mut parent = h.parent_header.clone();

    // Block 1: Alice → Bob (1M wei)
    let rlp1 = build_signed_transfer(&sk, bob, U256::from(1_000_000u64), 0, base_fee);
    let proposed1 = h
        .proposer
        .build_block(
            &h.db,
            &h.executor,
            &parent,
            vec![rlp1],
            1_000_000,
            Address::ZERO,
        )
        .unwrap();
    let v1 = h
        .validator
        .validate_block(&proposed1.block, &h.db, &h.executor)
        .unwrap();
    BlockCommitter::commit_block(&h.db, &proposed1.block, &v1.bundle, &v1.receipts).unwrap();
    parent = proposed1.block.header.clone();

    // Block 2: Alice → Bob (2M wei, nonce=1)
    let rlp2 = build_signed_transfer(&sk, bob, U256::from(2_000_000u64), 1, base_fee);
    let proposed2 = h
        .proposer
        .build_block(
            &h.db,
            &h.executor,
            &parent,
            vec![rlp2],
            2_000_000,
            Address::ZERO,
        )
        .unwrap();
    let v2 = h
        .validator
        .validate_block(&proposed2.block, &h.db, &h.executor)
        .unwrap();
    BlockCommitter::commit_block(&h.db, &proposed2.block, &v2.bundle, &v2.receipts).unwrap();

    // Verify cumulative state.
    let bob_acct = h.db.get_account(&bob).unwrap().expect("Bob exists");
    assert_eq!(
        bob_acct.balance,
        U256::from(3_000_000u64),
        "Bob should have 1M + 2M = 3M wei"
    );

    // Both blocks stored.
    assert!(h
        .db
        .get_cf_raw(torus_state::cf::CF_BLOCK_HEADERS, &1u64.to_be_bytes())
        .unwrap()
        .is_some());
    assert!(h
        .db
        .get_cf_raw(torus_state::cf::CF_BLOCK_HEADERS, &2u64.to_be_bytes())
        .unwrap()
        .is_some());
}

// 6. validate_block_for_sync returns same result
#[test]
fn validate_block_for_sync_matches() {
    let h = TestHarness::new();
    let sk = test_signing_key(1);
    let alice = signing_key_address(&sk);
    let bob = Address::new([0xBB; 20]);

    let ten_eth = U256::from(10_000_000_000_000_000_000u128);
    h.db.put_account(&alice, &test_account(ten_eth)).unwrap();

    let base_fee = h.parent_header.base_fee_per_gas as u128;
    let rlp = build_signed_transfer(&sk, bob, U256::from(1_000u64), 0, base_fee);
    let proposed = h.propose(vec![rlp], Address::ZERO);

    let v_normal = h
        .validator
        .validate_block(&proposed.block, &h.db, &h.executor)
        .expect("normal validate");
    let v_sync = h
        .validator
        .validate_block_for_sync(&proposed.block, &h.db, &h.executor)
        .expect("sync validate");

    assert_eq!(v_normal.state_root, v_sync.state_root);
}

// 7. State root mismatch is detected
#[test]
fn state_root_mismatch_detected() {
    let h = TestHarness::new();
    let sk = test_signing_key(1);
    let alice = signing_key_address(&sk);
    let bob = Address::new([0xBB; 20]);

    let ten_eth = U256::from(10_000_000_000_000_000_000u128);
    h.db.put_account(&alice, &test_account(ten_eth)).unwrap();

    let base_fee = h.parent_header.base_fee_per_gas as u128;
    let rlp = build_signed_transfer(&sk, bob, U256::from(1_000u64), 0, base_fee);
    let mut proposed = h.propose(vec![rlp], Address::ZERO);

    // Tamper with state root.
    proposed.block.header.state_root = B256::ZERO;

    let result = h
        .validator
        .validate_block(&proposed.block, &h.db, &h.executor);
    assert!(result.is_err(), "tampered state root should be detected");

    match result.unwrap_err() {
        torus_bridge::BridgeError::StateRootMismatch { .. } => {}
        other => panic!("expected StateRootMismatch, got: {other}"),
    }
}

// 8. Gas mismatch is detected
#[test]
fn gas_mismatch_detected() {
    let h = TestHarness::new();
    let sk = test_signing_key(1);
    let alice = signing_key_address(&sk);
    let bob = Address::new([0xBB; 20]);

    let ten_eth = U256::from(10_000_000_000_000_000_000u128);
    h.db.put_account(&alice, &test_account(ten_eth)).unwrap();

    let base_fee = h.parent_header.base_fee_per_gas as u128;
    let rlp = build_signed_transfer(&sk, bob, U256::from(1_000u64), 0, base_fee);
    let mut proposed = h.propose(vec![rlp], Address::ZERO);

    // Tamper with gas used.
    proposed.block.header.evm_gas_used = 999_999;

    let result = h
        .validator
        .validate_block(&proposed.block, &h.db, &h.executor);
    assert!(result.is_err(), "tampered gas should be detected");
}

// ============================================================================
// FIX 3 TEST: Block hash uses canonical encoding, not serde_json
// ============================================================================

// 9. Block hash is deterministic via canonical_header_bytes
#[test]
fn block_hash_uses_canonical_encoding() {
    use torus_types::TorusBlockHeader;

    let header = TorusBlockHeader {
        height: 42,
        timestamp: 1_700_000_000,
        proposer: Address::new([0xAA; 20]),
        state_root: B256::new([1u8; 32]),
        receipts_root: B256::new([2u8; 32]),
        logs_bloom: alloy_primitives::Bloom::ZERO,
        evm_gas_used: 21_000,
        evm_gas_limit: 30_000_000,
        native_action_count: 5,
        evm_tx_count: 3,
        base_fee_per_gas: 1_000_000_000,
        epoch: 7,
        validator_set_hash: B256::new([3u8; 32]),
    };

    // Canonical bytes must be deterministic across calls.
    let bytes1 = header.canonical_header_bytes();
    let bytes2 = header.canonical_header_bytes();
    assert_eq!(bytes1, bytes2, "canonical_header_bytes must be deterministic");

    // Hash from canonical bytes must be deterministic.
    let hash1 = alloy_primitives::keccak256(&bytes1);
    let hash2 = alloy_primitives::keccak256(&bytes2);
    assert_eq!(hash1, hash2);
    assert_ne!(hash1, B256::ZERO);

    // Canonical hash must differ from serde_json hash (different encoding).
    let json_bytes = serde_json::to_vec(&header).unwrap();
    let json_hash = alloy_primitives::keccak256(&json_bytes);
    assert_ne!(
        hash1, json_hash,
        "canonical hash should differ from JSON hash (different encoding)"
    );
}

// 10. Commit uses canonical hash for block identity
#[test]
fn commit_block_hash_is_canonical() {
    let h = TestHarness::new();
    let sk = test_signing_key(1);
    let alice = signing_key_address(&sk);
    let bob = Address::new([0xBB; 20]);

    let ten_eth = U256::from(10_000_000_000_000_000_000u128);
    h.db.put_account(&alice, &test_account(ten_eth)).unwrap();

    let base_fee = h.parent_header.base_fee_per_gas as u128;
    let rlp = build_signed_transfer(&sk, bob, U256::from(1_000u64), 0, base_fee);
    let proposed = h.propose(vec![rlp], Address::ZERO);
    let block = &proposed.block;
    let validated = h.validator.validate_block(block, &h.db, &h.executor).unwrap();

    let block_hash =
        BlockCommitter::commit_block(&h.db, block, &validated.bundle, &validated.receipts)
            .unwrap();

    // Independently compute the expected hash from canonical bytes.
    let expected_hash = alloy_primitives::keccak256(&block.header.canonical_header_bytes());
    assert_eq!(
        block_hash, expected_hash,
        "commit_block must use canonical_header_bytes for block hash"
    );
}
