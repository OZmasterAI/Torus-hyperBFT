//! Integration tests for the torus-bridge consensus-execution pipeline.
//!
//! Tests cover:
//! - RLP transaction decoding (1.5.2 dependency)
//! - Block proposal + validation + commit pipeline (1.5.1-1.5.3)
//! - State root determinism (1.5.5)
//! - Multi-block chain (state continuity)
//! - validate_block_for_sync (1.5.6)
//! - Phase 1.10.7: EIP-1559 gas accounting (tests 11-18)

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
    build_signed_transfer_with_tip(sk, to, value, nonce, max_fee_per_gas, 0)
}

fn build_signed_transfer_with_tip(
    sk: &SigningKey,
    to: Address,
    value: U256,
    nonce: u64,
    max_fee_per_gas: u128,
    max_priority_fee_per_gas: u128,
) -> Vec<u8> {
    build_signed_tx(
        sk,
        to,
        value,
        nonce,
        max_fee_per_gas,
        max_priority_fee_per_gas,
        21_000,
    )
}

/// Build a signed EIP-1559 tx with an explicit gas_limit (for refund tests).
fn build_signed_tx(
    sk: &SigningKey,
    to: Address,
    value: U256,
    nonce: u64,
    max_fee_per_gas: u128,
    max_priority_fee_per_gas: u128,
    gas_limit: u64,
) -> Vec<u8> {
    let tx = TxEip1559 {
        chain_id: TORUS_CHAIN_ID,
        nonce,
        gas_limit,
        max_fee_per_gas,
        max_priority_fee_per_gas,
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
        parent_hash: B256::new([9u8; 32]),
        timestamp: 1_700_000_000,
        proposer: Address::new([0xAA; 20]),
        state_root: B256::new([1u8; 32]),
        receipts_root: B256::new([2u8; 32]),
        logs_bloom: alloy_primitives::Bloom::ZERO,
        evm_gas_used: 21_000,
        evm_fee_revenue: 0,
        evm_gas_limit: 30_000_000,
        native_action_count: 5,
        evm_tx_count: 3,
        base_fee_per_gas: 1_000_000_000,
        epoch: 7,
        validator_set_hash: B256::new([3u8; 32]),
        sig_attestation: [0u8; 64],
    };

    // Canonical bytes must be deterministic across calls.
    let bytes1 = header.canonical_header_bytes();
    let bytes2 = header.canonical_header_bytes();
    assert_eq!(
        bytes1, bytes2,
        "canonical_header_bytes must be deterministic"
    );

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
    let validated = h
        .validator
        .validate_block(block, &h.db, &h.executor)
        .unwrap();

    let block_hash =
        BlockCommitter::commit_block(&h.db, block, &validated.bundle, &validated.receipts).unwrap();

    // Independently compute the expected hash from canonical bytes.
    let expected_hash = alloy_primitives::keccak256(&block.header.canonical_header_bytes());
    assert_eq!(
        block_hash, expected_hash,
        "commit_block must use canonical_header_bytes for block hash"
    );
}

// ============================================================================
// Phase 1.10.7: EIP-1559 Gas Accounting Tests
//
// Key invariant (verified against revm Cancun behavior):
//   - Block base_fee = calc_next_block_base_fee(parent.gas_used, parent.gas_limit, parent.base_fee)
//   - Genesis parent: gas_used=0, gas_limit=30M, base_fee=1G → block1 base_fee = 875M
//   - Sender is debited:       gas_used * max_fee_per_gas  (+value)
//   - Burned (not credited):   gas_used * base_fee
//   - Proposer (beneficiary):  gas_used * (max_fee_per_gas - base_fee)
//   - receipt.effective_gas_price = base_fee + min(priority_fee, max_fee - base_fee)
//   - Unused gas refund:       (gas_limit - gas_used) * max_fee_per_gas → credited to sender
// ============================================================================

// 11. Sender balance deducted by gas_used * max_fee_per_gas + value
//
// Uses max_fee = parent base_fee (1G), no tip.  The proposer recalculates block
// base_fee = 875M from the empty genesis parent, so the actual deduction is
// gas_used * max_fee(1G) — all of which is split between burn (875M/gas) and
// proposer (125M/gas).  The sender is always debited max_fee_per_gas * gas_used.
#[test]
fn gas_accounting_sender_balance_deducted() {
    let h = TestHarness::new();
    let sk = test_signing_key(1);
    let alice = signing_key_address(&sk);
    let bob = Address::new([0xBB; 20]);

    let ten_eth = U256::from(10_000_000_000_000_000_000u128);
    h.db.put_account(&alice, &test_account(ten_eth)).unwrap();

    // max_fee = 1 gwei (parent base_fee).  Actual block base_fee will be 875M
    // (EIP-1559 from empty genesis), which is ≤ max_fee so the tx is valid.
    let max_fee: u128 = h.parent_header.base_fee_per_gas as u128;
    let value = U256::from(1_000_000u64);
    let rlp = build_signed_transfer(&sk, bob, value, 0, max_fee);

    let proposed = h.propose(vec![rlp], Address::with_last_byte(0xFF));
    let validated = h
        .validator
        .validate_block(&proposed.block, &h.db, &h.executor)
        .unwrap();
    BlockCommitter::commit_block(
        &h.db,
        &proposed.block,
        &validated.bundle,
        &validated.receipts,
    )
    .unwrap();

    let alice_after = h.db.get_account(&alice).unwrap().unwrap();
    // Sender pays: gas_used * max_fee_per_gas + value
    let gas_cost = U256::from(21_000u128) * U256::from(max_fee);
    let expected = ten_eth - value - gas_cost;
    assert_eq!(
        alice_after.balance, expected,
        "sender should be debited value + gas_used * max_fee_per_gas"
    );
}

// 12. Priority fee (tip) to proposer = gas_used * (max_fee - actual_block_base_fee)
//
// revm credits the beneficiary with (max_fee_per_gas - base_fee) per gas used.
// Base fee is burned.  So proposer receives: gas_used * (max_fee - base_fee),
// where base_fee is the block's base_fee (875M for block 1 from genesis).
#[test]
fn gas_accounting_tip_to_proposer() {
    let h = TestHarness::new();
    let sk = test_signing_key(1);
    let alice = signing_key_address(&sk);
    let bob = Address::new([0xBB; 20]);
    let proposer = Address::new([0xFF; 20]);

    let ten_eth = U256::from(10_000_000_000_000_000_000u128);
    h.db.put_account(&alice, &test_account(ten_eth)).unwrap();

    // tip = 2 gwei on top of parent base_fee (1G).  Block base_fee will be 875M.
    let tip: u128 = 2_000_000_000;
    let max_fee: u128 = h.parent_header.base_fee_per_gas as u128 + tip;
    let rlp = build_signed_transfer_with_tip(&sk, bob, U256::from(1u64), 0, max_fee, tip);

    let proposed = h.propose(vec![rlp], proposer);
    // Read the actual block base_fee (not the parent's).
    let block_base_fee = proposed.block.header.base_fee_per_gas;
    let validated = h
        .validator
        .validate_block(&proposed.block, &h.db, &h.executor)
        .unwrap();
    BlockCommitter::commit_block(
        &h.db,
        &proposed.block,
        &validated.bundle,
        &validated.receipts,
    )
    .unwrap();

    let proposer_acct = h.db.get_account(&proposer).unwrap().unwrap();
    // Proposer receives: gas_used * (max_fee - block_base_fee)
    let proposer_per_gas = max_fee - block_base_fee as u128;
    let expected_proposer = U256::from(21_000u128) * U256::from(proposer_per_gas);
    assert_eq!(
        proposer_acct.balance, expected_proposer,
        "proposer should receive gas_used * (max_fee - block_base_fee); \
         block_base_fee={block_base_fee}, max_fee={max_fee}"
    );
}

// 13. Base fee burned: net supply reduction = gas_used * actual_block_base_fee
//
// The base fee is never credited to any account — it is permanently removed
// from the total supply.  All other ETH (value + proposer tip) is preserved.
#[test]
fn gas_accounting_base_fee_burned() {
    let h = TestHarness::new();
    let sk = test_signing_key(1);
    let alice = signing_key_address(&sk);
    let bob = Address::new([0xBB; 20]);
    let proposer = Address::new([0xFF; 20]);

    let ten_eth = U256::from(10_000_000_000_000_000_000u128);
    h.db.put_account(&alice, &test_account(ten_eth)).unwrap();

    let tip: u128 = 500_000_000; // 0.5 gwei
    let max_fee: u128 = h.parent_header.base_fee_per_gas as u128 + tip;
    let value = U256::from(1_000_000u64);
    let rlp = build_signed_transfer_with_tip(&sk, bob, value, 0, max_fee, tip);

    let proposed = h.propose(vec![rlp], proposer);
    let block_base_fee = proposed.block.header.base_fee_per_gas;
    let validated = h
        .validator
        .validate_block(&proposed.block, &h.db, &h.executor)
        .unwrap();
    BlockCommitter::commit_block(
        &h.db,
        &proposed.block,
        &validated.bundle,
        &validated.receipts,
    )
    .unwrap();

    let alice_after = h.db.get_account(&alice).unwrap().unwrap();
    let bob_after = h.db.get_account(&bob).unwrap().unwrap();
    let proposer_after = h.db.get_account(&proposer).unwrap().unwrap();

    // Total supply decreases by exactly gas_used * block_base_fee (the burned amount).
    let total_after = alice_after.balance + bob_after.balance + proposer_after.balance;
    let burned = U256::from(21_000u64) * U256::from(block_base_fee);
    assert_eq!(
        total_after,
        ten_eth - burned,
        "total supply should decrease by gas_used * block_base_fee (burned); \
         block_base_fee={block_base_fee}"
    );
}

// 14. Base fee updates correctly block-over-block following EIP-1559 formula
//
// Block 1 is built from genesis parent (gas_used=0 → base_fee decreases 12.5%).
// Block 2 is built from block 1's header.  The proposer must derive block 2's
// base_fee from block 1's actual gas_used and gas_limit — not from the genesis
// parent's base_fee.
#[test]
fn gas_accounting_base_fee_updates_between_blocks() {
    let h = TestHarness::new();
    let sk = test_signing_key(1);
    let alice = signing_key_address(&sk);
    let bob = Address::new([0xBB; 20]);

    let ten_eth = U256::from(10_000_000_000_000_000_000u128);
    h.db.put_account(&alice, &test_account(ten_eth)).unwrap();

    // Use a generous max_fee so it stays valid regardless of base_fee drift.
    let max_fee: u128 = 2_000_000_000; // 2 gwei — above any realistic block1 base_fee
    let rlp1 = build_signed_transfer(&sk, bob, U256::from(1u64), 0, max_fee);

    let proposed1 = h.propose(vec![rlp1], Address::ZERO);
    let block1 = &proposed1.block;

    // Block 1 has far less gas than the 15M target → base_fee must decrease.
    assert!(
        block1.header.base_fee_per_gas < h.parent_header.base_fee_per_gas,
        "block 1 base_fee should be less than genesis base_fee (empty genesis block)"
    );

    let validated1 = h
        .validator
        .validate_block(block1, &h.db, &h.executor)
        .unwrap();
    BlockCommitter::commit_block(&h.db, block1, &validated1.bundle, &validated1.receipts).unwrap();

    // Independently compute the expected base_fee for block 2 from block 1's header fields.
    let expected_block2_base_fee = torus_evm::calc_next_block_base_fee(
        block1.header.evm_gas_used,
        block1.header.evm_gas_limit,
        block1.header.base_fee_per_gas, // use block 1's actual base_fee, not genesis
    );

    // Build block 2 from block 1 as parent.
    let rlp2 = build_signed_transfer(&sk, bob, U256::from(1u64), 1, max_fee);
    let proposed2 = h
        .proposer
        .build_block(
            &h.db,
            &h.executor,
            &block1.header,
            vec![rlp2],
            2_000_000,
            Address::ZERO,
        )
        .unwrap();

    assert_eq!(
        proposed2.block.header.base_fee_per_gas, expected_block2_base_fee,
        "block 2 base_fee must be computed via EIP-1559 from block 1 header"
    );
    assert!(
        expected_block2_base_fee < block1.header.base_fee_per_gas,
        "block 2 base_fee should decrease further (block 1 also had minimal gas)"
    );
}

// 15. Receipt effective_gas_price = base_fee + min(priority_fee, max_fee - base_fee)
//
// Uses the actual block base_fee (not parent's) to compute the expected value.
// When tip fits within (max_fee - base_fee), effective = base_fee + tip.
#[test]
fn gas_accounting_effective_gas_price_in_receipt() {
    let h = TestHarness::new();
    let sk = test_signing_key(1);
    let alice = signing_key_address(&sk);
    let bob = Address::new([0xBB; 20]);

    let ten_eth = U256::from(10_000_000_000_000_000_000u128);
    h.db.put_account(&alice, &test_account(ten_eth)).unwrap();

    // tip = 3 gwei, max_fee = parent_base(1G) + tip = 4G.
    // Block base_fee = 875M.  Headroom = 4G - 875M = 3.125G > tip → tip not capped.
    // effective = 875M + min(3G, 3.125G) = 875M + 3G = 3.875G.
    let tip: u128 = 3_000_000_000;
    let max_fee: u128 = h.parent_header.base_fee_per_gas as u128 + tip;
    let rlp = build_signed_transfer_with_tip(&sk, bob, U256::from(1u64), 0, max_fee, tip);

    let proposed = h.propose(vec![rlp], Address::ZERO);
    let block_base_fee = proposed.block.header.base_fee_per_gas;
    let validated = h
        .validator
        .validate_block(&proposed.block, &h.db, &h.executor)
        .unwrap();

    // effective_gas_price = block_base_fee + min(tip, max_fee - block_base_fee)
    let headroom = max_fee - block_base_fee as u128;
    let effective_tip = tip.min(headroom);
    let expected_effective = block_base_fee + effective_tip as u64;
    assert_eq!(
        validated.receipts[0].effective_gas_price, expected_effective,
        "effective_gas_price = block_base_fee({block_base_fee}) + tip({tip}); \
         headroom={headroom}"
    );
}

// 16. Tip capped when max_fee barely covers base_fee
//
// max_fee = 1.5G, requested tip = 3G.  Block base_fee = 875M.
// Headroom = 1.5G - 875M = 625M < 3G → tip capped at 625M.
// Proposer receives: gas_used * (max_fee - block_base_fee) = 21000 * 625M.
// effective_gas_price = max_fee = 1.5G (since base_fee + capped_tip = 875M + 625M = 1.5G).
#[test]
fn gas_accounting_tip_capped_by_max_fee() {
    let h = TestHarness::new();
    let sk = test_signing_key(1);
    let alice = signing_key_address(&sk);
    let bob = Address::new([0xBB; 20]);
    let proposer = Address::new([0xFF; 20]);

    let ten_eth = U256::from(10_000_000_000_000_000_000u128);
    h.db.put_account(&alice, &test_account(ten_eth)).unwrap();

    // max_fee = 1.5 gwei, tip = 3 gwei (will be capped to max_fee - block_base_fee)
    let max_fee: u128 = 1_500_000_000;
    let requested_tip: u128 = 3_000_000_000;
    let rlp = build_signed_transfer_with_tip(&sk, bob, U256::from(1u64), 0, max_fee, requested_tip);

    let proposed = h.propose(vec![rlp], proposer);
    let block_base_fee = proposed.block.header.base_fee_per_gas;
    let validated = h
        .validator
        .validate_block(&proposed.block, &h.db, &h.executor)
        .unwrap();
    BlockCommitter::commit_block(
        &h.db,
        &proposed.block,
        &validated.bundle,
        &validated.receipts,
    )
    .unwrap();

    // Proposer receives: gas_used * (max_fee - block_base_fee)
    let proposer_per_gas = max_fee - block_base_fee as u128;
    let proposer_acct = h.db.get_account(&proposer).unwrap().unwrap();
    let expected_proposer = U256::from(21_000u128) * U256::from(proposer_per_gas);
    assert_eq!(
        proposer_acct.balance, expected_proposer,
        "proposer receives gas_used * (max_fee - block_base_fee); \
         max_fee={max_fee}, block_base_fee={block_base_fee}"
    );

    // effective_gas_price = block_base_fee + min(tip, max_fee - block_base_fee)
    //                     = block_base_fee + (max_fee - block_base_fee) = max_fee
    assert_eq!(
        validated.receipts[0].effective_gas_price, max_fee as u64,
        "effective_gas_price should equal max_fee when tip is capped to headroom"
    );
}

// 17. Transaction with max_fee_per_gas < base_fee is rejected
//
// EIP-1559 requires max_fee_per_gas >= block base_fee.  revm validates this
// before execution and returns InvalidTransaction.  The bridge maps this to
// BridgeError::Evm(EvmError::InvalidTransaction(...)).
#[test]
fn gas_accounting_tx_rejected_when_max_fee_below_base_fee() {
    let h = TestHarness::new();
    let sk = test_signing_key(1);
    let alice = signing_key_address(&sk);
    let bob = Address::new([0xBB; 20]);

    let ten_eth = U256::from(10_000_000_000_000_000_000u128);
    h.db.put_account(&alice, &test_account(ten_eth)).unwrap();

    // First build an empty block to discover the actual block1 base_fee.
    // Genesis: gas_used=0, limit=30M, base=1G → block1 base_fee = 875M.
    let proposed_empty = h.propose(vec![], Address::ZERO);
    let block1_base_fee = proposed_empty.block.header.base_fee_per_gas;
    assert!(block1_base_fee > 0, "base_fee must be positive");

    // Now build a transaction with max_fee strictly below block1 base_fee.
    let insufficient_max_fee: u128 = block1_base_fee as u128 - 1;
    let rlp = build_signed_transfer(&sk, bob, U256::from(1u64), 0, insufficient_max_fee);

    // Proposer skips invalid txs instead of aborting — block succeeds with 0 EVM txs.
    let result = h.proposer.build_block(
        &h.db,
        &h.executor,
        &h.parent_header,
        vec![rlp],
        1_000_000,
        Address::ZERO,
    );

    let proposed = result.expect("proposer should succeed by skipping the invalid tx");
    assert_eq!(
        proposed.block.header.evm_tx_count, 0,
        "block should contain 0 txs after skipping max_fee < base_fee tx"
    );
    assert_eq!(proposed.block.evm_transactions.len(), 0);
}

// 18. Unused gas is refunded to sender at max_fee_per_gas per gas unit
//
// When a transaction specifies gas_limit > gas_used, the unspent gas is
// refunded: refund = (gas_limit - gas_used) * max_fee_per_gas.
// For a plain ETH transfer: gas_used = 21_000 always.
// Setting gas_limit = 50_000 means 29_000 gas are refunded.
#[test]
fn gas_accounting_unused_gas_refunded_to_sender() {
    let h = TestHarness::new();
    let sk = test_signing_key(1);
    let alice = signing_key_address(&sk);
    let bob = Address::new([0xBB; 20]);

    let ten_eth = U256::from(10_000_000_000_000_000_000u128);
    h.db.put_account(&alice, &test_account(ten_eth)).unwrap();

    // max_fee = 2 gwei (above any realistic block base_fee after genesis).
    // gas_limit = 50_000, but a plain transfer only uses 21_000.
    let max_fee: u128 = 2_000_000_000;
    let gas_limit: u64 = 50_000;
    let value = U256::from(1_000u64);
    let rlp = build_signed_tx(&sk, bob, value, 0, max_fee, 0, gas_limit);

    let proposed = h.propose(vec![rlp], Address::with_last_byte(0xFF));
    let block_base_fee = proposed.block.header.base_fee_per_gas;

    // Confirm the transfer used exactly 21_000 gas.
    assert_eq!(
        proposed.block.header.evm_gas_used, 21_000,
        "transfer should use 21k gas"
    );

    let validated = h
        .validator
        .validate_block(&proposed.block, &h.db, &h.executor)
        .unwrap();
    BlockCommitter::commit_block(
        &h.db,
        &proposed.block,
        &validated.bundle,
        &validated.receipts,
    )
    .unwrap();

    let gas_used: u64 = 21_000;
    let gas_unused = gas_limit - gas_used;

    // Alice pays for gas_used * max_fee (not gas_limit * max_fee) plus value.
    // The unused gas (gas_unused * max_fee) is refunded back to Alice.
    let alice_after = h.db.get_account(&alice).unwrap().unwrap();
    let gas_cost = U256::from(gas_used as u128) * U256::from(max_fee);
    let expected_alice = ten_eth - value - gas_cost;
    assert_eq!(
        alice_after.balance, expected_alice,
        "alice should be refunded unused gas: gas_limit={gas_limit}, gas_used={gas_used}, \
         unused={gas_unused}, max_fee={max_fee}, block_base_fee={block_base_fee}"
    );

    // Sanity: the receipt records gas_used = 21_000, not gas_limit = 50_000.
    assert_eq!(
        validated.receipts[0].gas_used, gas_used,
        "receipt gas_used must reflect actual consumption, not gas_limit"
    );

    // Sanity: bob received the value.
    let bob_after = h.db.get_account(&bob).unwrap().unwrap();
    assert_eq!(
        bob_after.balance, value,
        "bob should receive the transferred value"
    );
}
