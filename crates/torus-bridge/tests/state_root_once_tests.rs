//! T4.1: the EVM state root is computed exactly ONCE per committed block.
//!
//! The catchup/commit path used to run the StateRoot engine up to 3x per block:
//! validate_block_for_catchup computed a root whose TrieUpdates were discarded,
//! commit_evm_bundle_incremental recomputed the same (root, updates) pair, and
//! resync_evm_accounts ran a third pass over the conservative dirty list. The
//! validation-time pair is now plumbed through ValidatedBlock and reused by the
//! commit; these tests pin the run count (via the thread-local engine-run
//! counter) AND that the committed root is byte-identical to the recompute path.

use alloy_consensus::{SignableTransaction, TxEip1559, TxEnvelope};
use alloy_primitives::{Address, Bytes, Signature as AlloySig, TxKind, B256, U256};
use alloy_rlp::Encodable;
use k256::ecdsa::SigningKey;
use revm::database::BundleState;
use revm::state::AccountInfo;

use torus_bridge::BlockValidator;
use torus_evm::{EvmExecutor, TORUS_CHAIN_ID};
use torus_state::incremental::{
    build_trie_to_cf, commit_evm_bundle_incremental, evm_root_engine_runs, incremental_evm_root,
    resync_evm_accounts,
};
use torus_state::trie::{compute_composite_root, EMPTY_ROOT_HASH};
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

/// Sign a 21k-gas transfer at exactly the 1-gwei base fee; returns the raw tx.
fn signed_transfer(key: &SigningKey, nonce: u64, value: U256) -> Vec<u8> {
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
    let mut buf = Vec::new();
    envelope.encode(&mut buf);
    buf
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

/// RED-first for T4.1: exactly ONE StateRoot engine run across catchup
/// validation + commit, and the committed root is byte-identical to the old
/// recompute-everything path on the same fixed block.
///
/// RED on pre-T4.1 behavior: validation discarded its TrieUpdates and the
/// commit recomputed the pair, so the engine-run delta across
/// validate+commit was 2 (and 3 with the conservative resync), not 1.
#[test]
fn catchup_root_computed_once_and_identical_to_recompute_path() {
    let alice = SigningKey::from_slice(&[0x11; 32]).unwrap();
    let empty = || BundleState::builder(0..=0).build();
    let make_block = || block_with_txs(vec![signed_transfer(&alice, 0, U256::from(1000))]);

    // ---- Path A (new): one run at validation, reused by the commit ----
    let (_dir, db) = open_test_db();
    fund(&db, &address_from_key(&alice));
    // Incremental base ready before any block, as ensure_trie_built does at boot.
    build_trie_to_cf(&db).unwrap();

    let runs_before = evm_root_engine_runs();
    let validated = validator()
        .validate_block_for_catchup(&make_block(), &db, &EvmExecutor::new(TORUS_CHAIN_ID))
        .expect("catchup validation");
    assert_eq!(
        evm_root_engine_runs() - runs_before,
        1,
        "catchup validation must run the StateRoot engine exactly once"
    );

    let pair = validated
        .evm_root_updates
        .expect("catchup validation must plumb (root, TrieUpdates) for commit reuse");
    assert_eq!(
        validated.state_root,
        compute_composite_root(pair.0, EMPTY_ROOT_HASH),
        "plumbed EVM root must underlie the composite state root"
    );

    let runs_before_commit = evm_root_engine_runs();
    let root_a = commit_evm_bundle_incremental(&db, &validated.bundle, Some(pair))
        .expect("commit with reused pair");
    assert_eq!(
        evm_root_engine_runs() - runs_before_commit,
        0,
        "commit must reuse the validation-time pair: root computed ONCE per committed block"
    );

    // ---- Path B (old 3x): identical DB, recompute at commit + conservative resync ----
    let (_dir2, db2) = open_test_db();
    fund(&db2, &address_from_key(&alice));
    build_trie_to_cf(&db2).unwrap();

    let validated2 = validator()
        .validate_block_for_catchup(&make_block(), &db2, &EvmExecutor::new(TORUS_CHAIN_ID))
        .expect("catchup validation (recompute path)");
    let root_b = commit_evm_bundle_incremental(&db2, &validated2.bundle, None)
        .expect("commit with recompute");
    // The old app.rs resync: seed_from_bundle marked every bundle account dirty.
    let dirty: Vec<Address> = validated2.bundle.state.keys().copied().collect();
    resync_evm_accounts(&db2, &dirty).expect("conservative resync");

    // Consensus-visible determinism: both paths commit the SAME root and trie.
    assert_eq!(root_a, root_b, "reused-pair root != recomputed root");
    assert_eq!(validated.state_root, validated2.state_root);
    assert_eq!(
        incremental_evm_root(&db, &empty()).unwrap().0,
        incremental_evm_root(&db2, &empty()).unwrap().0,
        "committed tries diverged between the once and 3x paths"
    );
}
