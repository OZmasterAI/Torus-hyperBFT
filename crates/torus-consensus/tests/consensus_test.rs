//! Integration test: 4-node in-process consensus.
//!
//! Verifies that four validators running hotstuff_rs with our trait implementations
//! can reach consensus, produce blocks, and commit them.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ed25519_dalek::SigningKey;
use tempfile::TempDir;

use hotstuff_rs::events::CommitBlockEvent;
use hotstuff_rs::replica::{Configuration, Replica, ReplicaSpec};
use hotstuff_rs::types::data_types::{BufferSize, ChainID, EpochLength};

use torus_consensus::{ChannelNetwork, GenesisConfig, RocksKVStore, TorusApp};

const NUM_VALIDATORS: usize = 4;
const CHAIN_ID: u64 = 7777;

fn make_signing_keys(n: usize) -> Vec<SigningKey> {
    (1..=n)
        .map(|i| {
            let mut secret = [0u8; 32];
            secret[0] = i as u8;
            SigningKey::from_bytes(&secret)
        })
        .collect()
}

#[test]
fn four_node_consensus_produces_and_commits_blocks() {
    let signing_keys = make_signing_keys(NUM_VALIDATORS);
    let verifying_keys: Vec<_> = signing_keys.iter().map(|sk| sk.verifying_key()).collect();

    // Genesis
    let genesis = GenesisConfig {
        chain_id: CHAIN_ID,
        validators: verifying_keys.iter().map(|vk| (*vk, 1)).collect(),
    };

    // Per-validator temp dirs and KV stores
    let tempdirs: Vec<TempDir> = (0..NUM_VALIDATORS)
        .map(|_| TempDir::new().unwrap())
        .collect();
    let kv_stores: Vec<RocksKVStore> = tempdirs
        .iter()
        .map(|td| RocksKVStore::open(td.path()))
        .collect();

    // Initialize each replica's block tree with genesis state
    for kv_store in &kv_stores {
        Replica::initialize(
            kv_store.clone(),
            genesis.initial_app_state(),
            genesis.validator_set_state(),
        );
    }

    // Channel network mesh
    let networks = ChannelNetwork::create_test_network(&verifying_keys);

    // Per-validator commit counters
    let commit_counts: Vec<Arc<AtomicU64>> = (0..NUM_VALIDATORS)
        .map(|_| Arc::new(AtomicU64::new(0)))
        .collect();

    // Start all replicas
    let _replicas: Vec<Replica<RocksKVStore>> = (0..NUM_VALIDATORS)
        .map(|i| {
            // Configuration matches hotstuff_rs reference tests (tests/common/node.rs).
            // Key: block_sync_server_advertise_time must be long to prevent
            // BlockSyncAdvertise messages from triggering sync and blocking
            // the algorithm loop during active consensus.
            let config = Configuration::builder()
                .me(signing_keys[i].clone())
                .chain_id(ChainID::new(CHAIN_ID))
                .epoch_length(EpochLength::new(50))
                .max_view_time(Duration::from_millis(2000))
                .progress_msg_buffer_capacity(BufferSize::new(1024))
                .block_sync_request_limit(10)
                .block_sync_server_advertise_time(Duration::new(10, 0))
                .block_sync_response_timeout(Duration::new(3, 0))
                .block_sync_blacklist_expiry_time(Duration::new(10, 0))
                .block_sync_trigger_min_view_difference(2)
                .block_sync_trigger_timeout(Duration::new(60, 0))
                .log_events(false)
                .build();

            let count = commit_counts[i].clone();

            ReplicaSpec::builder()
                .app(TorusApp::stub())
                .network(networks[i].clone())
                .kv_store(kv_stores[i].clone())
                .configuration(config)
                .on_commit_block(move |_event: &CommitBlockEvent| {
                    count.fetch_add(1, Ordering::SeqCst);
                })
                .build()
                .start()
        })
        .collect();

    // Wait for each validator to commit at least 2 blocks
    let target_per_validator = 2u64;
    let timeout = Duration::from_secs(60);
    let start = Instant::now();

    loop {
        if start.elapsed() > timeout {
            break;
        }
        if commit_counts
            .iter()
            .all(|c| c.load(Ordering::SeqCst) >= target_per_validator)
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    // Verify
    for (i, count) in commit_counts.iter().enumerate() {
        let commits = count.load(Ordering::SeqCst);
        assert!(
            commits >= target_per_validator,
            "Validator {i} only committed {commits} blocks (expected >= {target_per_validator})"
        );
    }

    let total: u64 = commit_counts.iter().map(|c| c.load(Ordering::SeqCst)).sum();
    eprintln!(
        "Consensus test passed: {total} total commits across {NUM_VALIDATORS} validators in {:?}",
        start.elapsed()
    );
}

#[test]
fn genesis_config_builds_valid_validator_set() {
    let signing_keys = make_signing_keys(4);
    let verifying_keys: Vec<_> = signing_keys.iter().map(|sk| sk.verifying_key()).collect();

    let genesis = GenesisConfig {
        chain_id: CHAIN_ID,
        validators: verifying_keys.iter().map(|vk| (*vk, 10)).collect(),
    };

    let vs = genesis.validator_set();
    assert_eq!(vs.len(), 4);
    for vk in &verifying_keys {
        assert!(vs.contains(vk));
    }

    let vss = genesis.validator_set_state();
    assert_eq!(
        vss.committed_validator_set().len(),
        vss.previous_validator_set().len()
    );
    assert!(vss.update_height().is_none());
    assert!(vss.update_decided());
}
