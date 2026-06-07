//! Phase C Task 6: the RARE native-DA pull-fallback.
//!
//! A `CompactBlock` whose native-action bodies are absent locally but available
//! from a peer must be recovered by fetching the bodies by-hash, inserting them
//! into the durable DA store, and reconstructing — without stalling or blacklisting
//! (livelock root cause, mem 28e1a821). And it must NOT fire when the bodies are
//! already local: push covers the common case, so pull stays rare (mem a6cf33a9).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use alloy_primitives::{Address, Bloom, B256, U256};

use torus_consensus::{NativeDaFetcher, TorusApp};
use torus_mempool::{Mempool, MempoolConfig};
use torus_state::db::StateDb;
use torus_types::{
    compute_action_hash, ActionSignature, ChainConfig, CompactBlock, NativeAction, Signature,
    SignedNativeAction, TorusBlockHeader,
};

/// Mock pull transport standing in for `/torus/native-da/1.0`. `fetch` records the
/// call; `drain` hands over the bodies the peer "holds" (staged by the test) once,
/// modelling a successful response landing on the inbound queue.
struct MockFetcher {
    fetch_calls: AtomicUsize,
    staged: Mutex<Vec<Vec<u8>>>,
}

impl MockFetcher {
    fn new(bodies: Vec<Vec<u8>>) -> Self {
        Self {
            fetch_calls: AtomicUsize::new(0),
            staged: Mutex::new(bodies),
        }
    }
    fn fetch_calls(&self) -> usize {
        self.fetch_calls.load(Ordering::SeqCst)
    }
}

impl NativeDaFetcher for MockFetcher {
    fn fetch(&self, _hashes: Vec<[u8; 32]>) {
        self.fetch_calls.fetch_add(1, Ordering::SeqCst);
    }
    fn drain(&self) -> Vec<Vec<u8>> {
        std::mem::take(&mut *self.staged.lock().unwrap())
    }
}

fn test_config() -> ChainConfig {
    ChainConfig {
        chain_id: 1337,
        chain_name: "torus-test".to_string(),
        evm_gas_limit: 30_000_000,
        base_fee_per_gas: 1_000_000_000,
        epoch_length: 100,
        max_validators: 4,
        min_stake: U256::ZERO,
        fee_burn_bps: 1000,
        fee_validator_bps: 0,
        fee_treasury_bps: 4500,
        fee_dev_pool_bps: 4500,
        treasury_address: Address::ZERO,
        dev_pool_address: Address::ZERO,
        timeout_base_ms: 500,
    }
}

/// A native-action body with a dummy signature. `compute_action_hash` omits the
/// signature field (mem c5c97fd9), so this hashes/stores identically to a real one.
fn make_action(nonce: u64) -> SignedNativeAction {
    SignedNativeAction {
        action: NativeAction::ClaimRewards,
        nonce,
        signature: ActionSignature::Eip712(Signature { v: 27, r: [0u8; 32], s: [0u8; 32] }),
    }
}

/// A `CompactBlock` referencing `body` by hash, with an otherwise-minimal header.
fn compact_referencing(body: &SignedNativeAction, height: u64) -> CompactBlock {
    CompactBlock {
        header: TorusBlockHeader {
            height,
            timestamp: 1000 + height,
            proposer: Address::ZERO,
            state_root: B256::ZERO,
            receipts_root: B256::ZERO,
            logs_bloom: Bloom::ZERO,
            evm_gas_used: 0,
            evm_fee_revenue: 0,
            evm_gas_limit: 30_000_000,
            native_action_count: 1,
            evm_tx_count: 0,
            base_fee_per_gas: 1_000_000_000,
            epoch: 0,
            validator_set_hash: B256::ZERO,
            sig_attestation: [0u8; 64],
        },
        native_action_hashes: vec![compute_action_hash(body)],
        evm_transactions: vec![],
        core_writer_actions: vec![],
    }
}

#[test]
fn native_da_pull_fallback_recovers() {
    let dir = tempfile::tempdir().expect("tempdir");
    let state_db = StateDb::open(dir.path()).expect("open db");
    let mempool = Arc::new(Mempool::new(state_db.clone(), MempoolConfig::default()));

    // A body referenced by a CompactBlock, absent from BOTH the mempool pool and the
    // durable DA store — the "missing-everywhere-locally" miss the pull recovers.
    let body = make_action(7);
    let body_hash = compute_action_hash(&body);
    let compact = compact_referencing(&body, 1);
    assert!(
        mempool.get_native_da(&body_hash).is_none(),
        "precondition: body absent from the local DA store"
    );

    let mut app = TorusApp::new(state_db.clone(), &test_config(), None, Some(mempool.clone()), None);

    // The peer holds the body: stage its on-wire bytes (bincode) in the mock fetcher.
    let body_bytes = bincode::serialize(&body).expect("serialize body");
    let fetcher = Arc::new(MockFetcher::new(vec![body_bytes]));
    app.set_native_da_fetcher(fetcher.clone());

    // MISS -> pull fires exactly once, the fetched body is absorbed into the DA store.
    let fired = app.pull_compact_bodies_if_missing(&compact);
    assert!(fired, "pull-fallback must fire on a reconstruction miss");
    assert_eq!(fetcher.fetch_calls(), 1, "exactly one fetch issued for the miss");
    assert!(
        mempool.get_native_da(&body_hash).is_some(),
        "the fetched body must be absorbed into the durable DA store"
    );

    // RARITY: with the body now local, a repeat does NOT fetch (push covers the
    // common case — pull must stay rare, mem a6cf33a9).
    let refired = app.pull_compact_bodies_if_missing(&compact);
    assert!(!refired, "pull-fallback must NOT fire when bodies are already local");
    assert_eq!(fetcher.fetch_calls(), 1, "no extra fetch when bodies are already local");
}

#[test]
fn native_da_pull_noop_without_fetcher() {
    // With no transport wired (consensus-only configuration), a miss must not panic
    // and simply reports that a pull was attempted; nothing is recovered.
    let dir = tempfile::tempdir().expect("tempdir");
    let state_db = StateDb::open(dir.path()).expect("open db");
    let mempool = Arc::new(Mempool::new(state_db.clone(), MempoolConfig::default()));

    let body = make_action(11);
    let body_hash = compute_action_hash(&body);
    let compact = compact_referencing(&body, 1);

    let app = TorusApp::new(state_db.clone(), &test_config(), None, Some(mempool.clone()), None);

    let fired = app.pull_compact_bodies_if_missing(&compact);
    assert!(fired, "a miss still reports a pull attempt");
    assert!(
        mempool.get_native_da(&body_hash).is_none(),
        "nothing recovered without a transport"
    );
}
