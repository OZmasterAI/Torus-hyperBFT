//! Fail-stop vs the hotstuff commit feed (s134, mem d59e61ea / 90070325).
//!
//! Once `exec_failed` latches, [`TorusApp::on_committed_block`] refuses every
//! committed block before any durable write (no manifest, no header). The node
//! binary exits(70) a few hundred ms later, but in that window hotstuff still
//! commits blocks already in its tree. Before the fix its commit feed counted
//! them delivered (`APP_FED_BLOCK_HEIGHT`), so after the restart the feed
//! resumed above them, the app never received them, and execution froze at the
//! first refused height forever while the node kept voting.
//!
//! Four validators run in one process (hotstuff_rs replicas over the in-process
//! [`ChannelNetwork`], each with a real [`TorusApp`] on a state DB + consensus DB,
//! the node layout). A node is stopped and restarted like a process: the
//! replica and app are dropped and a new app opens the same data dirs (boot
//! replay runs in its construction).

use super::*;

use std::time::{Duration, Instant};

use hotstuff_rs::block_tree::accessors::internal::BlockTreeSingleton;
use hotstuff_rs::replica::{Configuration, Replica, ReplicaSpec};
use hotstuff_rs::types::data_types::{BlockHeight, BufferSize, ChainID, EpochLength};
use tempfile::TempDir;

use crate::{ChannelNetwork, GenesisConfig};

const N: usize = 4;
/// The node that fail-stops.
const SICK: usize = 3;
const WAIT: Duration = Duration::from_secs(120);

fn chain_config() -> ChainConfig {
    ChainConfig {
        chain_id: TORUS_CHAIN_ID,
        chain_name: "failstop-feed".to_string(),
        evm_gas_limit: torus_types::DEFAULT_EVM_GAS_LIMIT,
        evm_block_interval: TEST_EVM_BLOCK_INTERVAL,
        evm_big_block_interval: torus_types::DEFAULT_EVM_BIG_BLOCK_INTERVAL,
        evm_big_gas_limit: torus_types::DEFAULT_EVM_BIG_GAS_LIMIT,
        base_fee_per_gas: 1_000_000_000,
        epoch_length: 1_000,
        max_validators: N as u32,
        min_stake: torus_economics::MIN_SELF_DELEGATION,
        fee_burn_bps: 1000,
        fee_validator_bps: 0,
        fee_treasury_bps: 4500,
        fee_dev_pool_bps: 4500,
        treasury_address: Address::ZERO,
        dev_pool_address: Address::ZERO,
        timeout_base_ms: 500,
        backoff_factor: 2,
        backoff_cap: 8,
        commit_lag_backoff_cap: 0,
        reputation_leader_selection: false,
        exec_trust_cache: false,
        // P0.0: an empty schedule (no feature, no header bits; the running
        // state hash off, as `state_hash_activation_height: None` was), so
        // the fail-stop here is the injected one, never `UPGRADE REQUIRED`.
        activations: Default::default(),
        book_mode: Default::default(),
    }
}

struct Node {
    dir: TempDir,
    replica: Option<Replica<RocksKVStore>>,
    state: Option<StateDb>,
    consensus: Option<StateDb>,
    exec_failed: Arc<AtomicBool>,
    /// The app's [`TorusApp::test_failstop_at`] seam.
    failstop_at: Arc<AtomicU64>,
}

struct Cluster {
    keys: Vec<ed25519_dalek::SigningKey>,
    networks: Vec<ChannelNetwork>,
    genesis: GenesisConfig,
    nodes: Vec<Node>,
}

impl Cluster {
    fn new() -> Self {
        torus_state::native_trie::force_native_trie_maintenance_on_for_tests();
        if std::env::var_os("FAILSTOP_TEST_LOG").is_some() {
            let _ = tracing_subscriber::fmt()
                .with_max_level(tracing::Level::INFO)
                .with_test_writer()
                .try_init();
        }
        let keys: Vec<_> = (0..N)
            .map(|i| ed25519_dalek::SigningKey::from_bytes(&[0x70 + i as u8; 32]))
            .collect();
        let vks: Vec<_> = keys.iter().map(|k| k.verifying_key()).collect();
        let genesis = GenesisConfig {
            chain_id: TORUS_CHAIN_ID,
            validators: vks.iter().map(|vk| (*vk, 1)).collect(),
        };
        let nodes = (0..N)
            .map(|_| Node {
                dir: TempDir::new().unwrap(),
                replica: None,
                state: None,
                consensus: None,
                exec_failed: Default::default(),
                failstop_at: Default::default(),
            })
            .collect();
        let mut cluster = Self {
            networks: ChannelNetwork::create_test_network(&vks),
            keys,
            genesis,
            nodes,
        };
        for i in 0..N {
            cluster.start(i);
        }
        cluster
    }

    /// Start (or restart) node `i` on its data dirs, as the node binary does.
    fn start(&mut self, i: usize) {
        assert!(self.nodes[i].replica.is_none(), "node {i} is running");
        let dir = self.nodes[i].dir.path().to_path_buf();
        let fresh = !dir.join("consensus").exists();
        let state = StateDb::open(&dir.join("state")).expect("open state db");
        let consensus = StateDb::open_consensus(&dir.join("consensus")).expect("open consensus db");
        let kv = RocksKVStore::new(consensus.db_arc());
        if fresh {
            Replica::initialize(
                kv.clone(),
                self.genesis.initial_app_state(),
                self.genesis.validator_set_state(),
            );
        }
        let app = TorusApp::try_new_with_manifest_db(
            state.clone(),
            consensus.clone(),
            &chain_config(),
            None,
            None,
            Some(self.keys[i].clone()),
        )
        .expect("TorusApp startup");
        // A (re)started validator gets nothing that was sent while it was down.
        let mut inbox = self.networks[i].clone();
        while hotstuff_rs::networking::network::Network::recv(&mut inbox).is_some() {}
        let config = Configuration::builder()
            .me(self.keys[i].clone())
            .chain_id(ChainID::new(TORUS_CHAIN_ID))
            .epoch_length(EpochLength::new(1_000))
            .max_view_time(Duration::from_millis(1_000))
            .progress_msg_buffer_capacity(BufferSize::new(1 << 20))
            .block_sync_request_limit(64)
            .block_sync_server_advertise_time(Duration::from_millis(300))
            .block_sync_response_timeout(Duration::from_secs(2))
            .block_sync_blacklist_expiry_time(Duration::from_secs(2))
            .block_sync_trigger_min_view_difference(2)
            .block_sync_trigger_timeout(Duration::from_secs(2))
            .log_events(false)
            .build();
        let node = &mut self.nodes[i];
        node.exec_failed = app.exec_failed_handle();
        node.failstop_at = app.test_failstop_at.clone();
        node.replica = Some(
            ReplicaSpec::builder()
                .app(app)
                .network(self.networks[i].clone())
                .kv_store(kv)
                .configuration(config)
                .build()
                .start(),
        );
        node.state = Some(state);
        node.consensus = Some(consensus);
    }

    /// Stop node `i`: the replica, then its app (the execution thread drains
    /// and joins), then every DB handle, so a restart can reopen the dirs.
    fn stop(&mut self, i: usize) {
        let node = &mut self.nodes[i];
        drop(node.replica.take().expect("node is running"));
        node.state = None;
        node.consensus = None;
    }

    /// Node `i`'s hotstuff block tree over its (open) consensus DB.
    fn tree(&self, i: usize) -> BlockTreeSingleton<RocksKVStore> {
        let db = self.nodes[i].consensus.as_ref().expect("node is open");
        // SAFETY: read-only use in a test (and the single small frontier
        // write `craft_fed_frontier` does while the replica is stopped).
        unsafe { BlockTreeSingleton::new_unsafe(RocksKVStore::new(db.db_arc())) }
    }

    /// Hotstuff's committed frontier on node `i` (hotstuff heights).
    fn hs_committed(&self, i: usize) -> u64 {
        self.tree(i)
            .highest_committed_block_height()
            .expect("read committed frontier")
            .map_or(0, |h| h.int())
    }

    /// Hotstuff's fed frontier (`APP_FED_BLOCK_HEIGHT`) on node `i`.
    fn hs_fed(&self, i: usize) -> Option<u64> {
        self.tree(i)
            .app_fed_block_height()
            .expect("read fed frontier")
            .map(|h| h.int())
    }

    /// The app's durable applied height (Torus heights, 0 = none).
    fn applied(&self, i: usize) -> u64 {
        let db = self.nodes[i].state.as_ref().expect("node is open");
        native_applied_height(db).ok().flatten().unwrap_or(0)
    }

    /// The app's committed-header frontier (Torus heights, 0 = none).
    fn headers(&self, i: usize) -> u64 {
        let db = self.nodes[i].state.as_ref().expect("node is open");
        find_last_committed_height(db).unwrap_or(0)
    }

    fn status(&self) -> String {
        (0..N)
            .map(|i| match self.nodes[i].state {
                Some(_) => format!(
                    "node {i}: applied {} headers {} hs_committed {} hs_fed {:?} failed {}",
                    self.applied(i),
                    self.headers(i),
                    self.hs_committed(i),
                    self.hs_fed(i),
                    self.nodes[i].exec_failed.load(Ordering::SeqCst)
                ),
                None => format!("node {i}: stopped"),
            })
            .collect::<Vec<_>>()
            .join("; ")
    }

    fn wait_until(&self, what: &str, cond: impl Fn(&Self) -> bool) {
        let start = Instant::now();
        while !cond(self) {
            assert!(
                start.elapsed() < WAIT,
                "timed out waiting for {what}: {}",
                self.status()
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn healthy_applied(&self) -> u64 {
        (0..N)
            .filter(|&i| i != SICK)
            .map(|i| self.applied(i))
            .min()
            .unwrap()
    }

    /// Fail-stop node `SICK` while its consensus keeps running (the node
    /// binary's exit window, made wide), then stop it. The latch fires right
    /// before the commit callback of its next committed block (the execution
    /// thread latching just before it), so at least that block is committed
    /// by hotstuff and refused by the app; after the latch the app refuses
    /// new proposals, and its hotstuff commits at most the few blocks already
    /// in its tree (as in production).
    fn failstop_sick_node(&mut self) {
        self.wait_until("every node applied 3 blocks", |c| {
            (0..N).all(|i| c.applied(i) >= 3)
        });
        let at = self.hs_committed(SICK) + 1;
        self.nodes[SICK].failstop_at.store(at, Ordering::SeqCst);
        self.wait_until("the sick node latches", |c| {
            c.nodes[SICK].exec_failed.load(Ordering::SeqCst)
        });
        let healthy = self.healthy_applied();
        self.wait_until("the healthy nodes run 4 more blocks", |c| {
            c.healthy_applied() >= healthy + 4
        });
        assert!(
            self.hs_committed(SICK) >= at,
            "precondition: the sick node committed the block it refused: {}",
            self.status()
        );
        self.stop(SICK);
    }

    /// Restart node `SICK` and wait until its execution passes what the
    /// healthy nodes had applied at the restart (it catches up and keeps
    /// executing new blocks).
    fn restart_sick_node_and_catch_up(&mut self) {
        let target = self.healthy_applied() + 3;
        self.start(SICK);
        self.wait_until(&format!("the restarted node applied {target}"), |c| {
            c.applied(SICK) >= target
        });
        assert!(
            !self.nodes[SICK].exec_failed.load(Ordering::SeqCst),
            "the restarted node fail-stopped: {}",
            self.status()
        );
    }

    fn stop_all(&mut self) {
        for i in 0..N {
            if self.nodes[i].replica.is_some() {
                self.stop(i);
            }
        }
    }
}

/// (2, process-like restart) A node whose execution fail-stopped while its
/// consensus still committed blocks restarts and catches up with the others.
/// Before the fix it stayed frozen at its last executed height forever: the
/// feed had counted the refused blocks delivered and resumed above them.
#[test]
fn failstopped_node_restarts_and_catches_up() {
    let mut c = Cluster::new();
    c.failstop_sick_node();

    // Reopen the stopped node's DBs to check what the feed recorded: no block
    // the latched app refused may count as delivered.
    let dir = c.nodes[SICK].dir.path().to_path_buf();
    c.nodes[SICK].state = Some(StateDb::open(&dir.join("state")).unwrap());
    c.nodes[SICK].consensus = Some(StateDb::open_consensus(&dir.join("consensus")).unwrap());
    let (fed, committed) = (c.hs_fed(SICK), c.hs_committed(SICK));
    assert!(
        fed.is_some_and(|f| f < committed),
        "the feed counted blocks the fail-stopped app refused as delivered: {}",
        c.status()
    );
    c.nodes[SICK].state = None;
    c.nodes[SICK].consensus = None;

    c.restart_sick_node_and_catch_up();
    c.stop_all();
}

/// (3) Boot safety net on a crafted DB: the fed frontier is above the app's
/// header frontier and those heights have no commit manifest (what a binary
/// without the feed fix leaves behind). The boot pass lowers the frontier to
/// the app's and re-delivers the blocks from the local tree; the node catches
/// up.
#[test]
fn boot_clamp_heals_a_fed_frontier_left_above_the_app() {
    let mut c = Cluster::new();
    c.failstop_sick_node();

    let dir = c.nodes[SICK].dir.path().to_path_buf();
    c.nodes[SICK].state = Some(StateDb::open(&dir.join("state")).unwrap());
    c.nodes[SICK].consensus = Some(StateDb::open_consensus(&dir.join("consensus")).unwrap());
    let committed = c.hs_committed(SICK);
    let headers = c.headers(SICK);
    // The crafted condition: every committed height counted delivered, the
    // top ones above the header frontier with no commit manifest (refused).
    c.tree(SICK)
        .advance_app_fed_block_height(BlockHeight::new(committed))
        .unwrap();
    let consensus = c.nodes[SICK].consensus.as_ref().unwrap();
    let top_torus = crate::da_gc::torus_height_of(committed).unwrap();
    assert!(
        top_torus > headers && load_commit_manifest(consensus, top_torus).is_none(),
        "crafted DB: fed {committed} (Torus {top_torus}) must be above the header frontier \
         {headers} with no manifest (manifests {:?})",
        manifest_heights_above(consensus, headers)
    );
    c.nodes[SICK].state = None;
    c.nodes[SICK].consensus = None;

    c.restart_sick_node_and_catch_up();
    c.stop_all();
}
