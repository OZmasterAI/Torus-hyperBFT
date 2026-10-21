//! P0.0 phase 5: multi-node activation tests (docs/plans/p00-activation-height-gating.md,
//! "4. Test plan", items 1-4).
//!
//! Four validators run in one process: hotstuff_rs replicas over the in-process
//! [`ChannelNetwork`], each with a real [`TorusApp`] (state DB + consensus DB, the
//! node layout, execution pipeline, running state hash), stopped, restarted and
//! wiped like processes. The test feature [`Feature::TestStateRule`] (torus-types
//! `test-features`, test builds only) has a state-visible rule,
//! [`test_state_rule`]: every block whose header carries its bit bumps a hashed
//! row, so the bit's height shows in the state and in every node's `h_n`.
//!
//! An "old binary" is a node whose known-feature set lacks the test feature
//! ([`AppOptions::known_features`]) and whose schedule does not name it (a
//! release that predates it). A node whose execution lags its commits and
//! then dies is an [`ExecHold`].
//!
//! Every node records the running-hash write set of each flushed height
//! (`running_hash::capture_*`); `h_n` is recomputed from it and checked against
//! the hash the node stored.

use super::*;

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use hotstuff_rs::replica::{Configuration, Replica, ReplicaSpec};
use hotstuff_rs::types::data_types::{BufferSize, ChainID, EpochLength};
use tempfile::TempDir;
use torus_state::cf::CF_TREASURY;
use torus_state::running_hash::{self, WriteEntry};
use torus_types::{ActivationSchedule, Feature};

use crate::{ChannelNetwork, GenesisConfig};

/// The row [`test_state_rule`] writes: `count u64 BE ‖ height u64 BE`, the
/// number of blocks executed under the rule and the last one's height.
const RULE_KEY: &[u8] = b"p00_test_state_rule";

/// The test feature's rule, run by the block pipeline (live and crash replay)
/// for every block before its actions: with the header bit on, count the block.
pub(super) fn test_state_rule<T: StateBackend>(ctx: &NativeExecContext<T>) {
    if !ctx.feature_active(Feature::TestStateRule) {
        return;
    }
    let count = ctx
        .state
        .get_cf_raw(CF_TREASURY, RULE_KEY)
        .expect("test rule read")
        .map_or(0, |v| u64::from_be_bytes(v[..8].try_into().unwrap()));
    let mut value = (count + 1).to_be_bytes().to_vec();
    value.extend_from_slice(&ctx.block_height.to_be_bytes());
    ctx.state
        .put_cf_raw(CF_TREASURY, RULE_KEY, &value)
        .expect("test rule write");
}

const N: usize = 4;
const X: Feature = Feature::TestStateRule;
const RSH: u128 = Feature::RunningStateHash.bit();
const WAIT: Duration = Duration::from_secs(90);

fn schedule(x_at: Option<u64>) -> ActivationSchedule {
    let mut pairs = vec![(Feature::RunningStateHash, 1)];
    pairs.extend(x_at.map(|h| (X, h)));
    ActivationSchedule::merge(&pairs.into_iter().collect(), &[]).expect("test schedule")
}

/// How a node is started: its schedule, known features and execution hold.
#[derive(Clone)]
struct Binary {
    schedule: ActivationSchedule,
    known: u128,
    hold: Option<Arc<ExecHold>>,
}

impl Binary {
    /// A release that knows the test feature, scheduled at `x_at`.
    fn new(x_at: u64) -> Self {
        Self {
            schedule: schedule(Some(x_at)),
            known: torus_types::KNOWN_FEATURES,
            hold: None,
        }
    }

    /// A release from before the test feature: neither knows nor schedules it.
    fn old() -> Self {
        Self {
            schedule: schedule(None),
            known: torus_types::KNOWN_FEATURES & !X.bit(),
            hold: None,
        }
    }

    fn held(mut self, hold: &Arc<ExecHold>) -> Self {
        self.hold = Some(hold.clone());
        self
    }
}

fn chain_config(schedule: ActivationSchedule) -> ChainConfig {
    ChainConfig {
        chain_id: TORUS_CHAIN_ID,
        chain_name: "p00-cluster".to_string(),
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
        activations: schedule,
    }
}

struct Node {
    dir: TempDir,
    replica: Option<Replica<RocksKVStore>>,
    state: Option<StateDb>,
    refusals: Arc<std::sync::Mutex<Vec<(&'static str, u64)>>>,
    exec_failed: Arc<AtomicBool>,
    /// Every flushed height's running-hash write set, across restarts.
    writes: BTreeMap<u64, Vec<WriteEntry>>,
    /// `read_running_hash` when the node last stopped.
    stored_hash: Option<(u64, [u8; 32])>,
}

struct Cluster {
    keys: Vec<ed25519_dalek::SigningKey>,
    networks: Vec<ChannelNetwork>,
    genesis: GenesisConfig,
    nodes: Vec<Node>,
}

impl Cluster {
    fn new() -> Self {
        // Process-global (see `make_test_config_and_db`): one mode for every node.
        torus_state::native_trie::force_native_trie_maintenance_on_for_tests();
        if std::env::var_os("P00_TEST_LOG").is_some() {
            let _ = tracing_subscriber::fmt()
                .with_max_level(tracing::Level::INFO)
                .with_test_writer()
                .try_init();
        }
        let keys: Vec<_> = (0..N)
            .map(|i| ed25519_dalek::SigningKey::from_bytes(&[0x50 + i as u8; 32]))
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
                refusals: Default::default(),
                exec_failed: Default::default(),
                writes: BTreeMap::new(),
                stored_hash: None,
            })
            .collect();
        Self {
            networks: ChannelNetwork::create_test_network(&vks),
            keys,
            genesis,
            nodes,
        }
    }

    /// Start every node on `binary`.
    fn start_all(&mut self, binary: &Binary) {
        for i in 0..N {
            self.start(i, binary);
        }
    }

    /// Start (or restart) node `i` on its data dir: boot replay runs in
    /// `TorusApp` construction, before the replica starts.
    fn start(&mut self, i: usize, binary: &Binary) {
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
        running_hash::capture_begin(&state);
        let options = AppOptions {
            known_features: binary.known,
            exec_hold: binary.hold.clone(),
        };
        let app = TorusApp::try_new_with_options(
            state.clone(),
            consensus,
            &chain_config(binary.schedule.clone()),
            None,
            None,
            Some(self.keys[i].clone()),
            options,
        )
        .expect("TorusApp startup");
        // A (re)started validator gets nothing sent while it was down.
        self.networks[i].discard_pending();
        let node = &mut self.nodes[i];
        node.refusals = app.test_upgrade_refusals.clone();
        node.exec_failed = app.exec_failed_handle();
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
    }

    /// Stop node `i` (the replica and its app: the execution thread drains and
    /// joins) and keep what it captured.
    fn stop(&mut self, i: usize) {
        let node = &mut self.nodes[i];
        drop(node.replica.take().expect("node is running"));
        let state = node.state.take().unwrap();
        node.stored_hash = running_hash::read_running_hash(&state);
        for (height, writes) in running_hash::capture_take(&state) {
            if let Some(before) = node.writes.get(&height) {
                assert_eq!(
                    before, &writes,
                    "node {i}: height {height} hashed twice (before and after a restart) \
                     with different write sets"
                );
            }
            node.writes.insert(height, writes);
        }
    }

    fn stop_all(&mut self) {
        for i in 0..N {
            if self.nodes[i].replica.is_some() {
                self.stop(i);
            }
        }
    }

    /// Node `i` restarts from an empty data dir.
    fn wipe(&mut self, i: usize) {
        assert!(self.nodes[i].replica.is_none());
        let node = &mut self.nodes[i];
        node.dir = TempDir::new().unwrap();
        node.writes.clear();
        node.stored_hash = None;
    }

    fn db(&self, i: usize) -> &StateDb {
        self.nodes[i].state.as_ref().expect("node is running")
    }

    /// The durable native applied height (0 = none).
    fn applied(&self, i: usize) -> u64 {
        running_hash::read_applied_height(self.db(i)).unwrap_or(0)
    }

    /// The highest committed height this node stored (0 = none).
    fn committed(&self, i: usize) -> u64 {
        find_last_committed_height(self.db(i)).unwrap_or(0)
    }

    fn status(&self) -> String {
        (0..N)
            .map(|i| match self.nodes[i].state {
                Some(_) => format!(
                    "node {i}: applied {} committed {} failed {}",
                    self.applied(i),
                    self.committed(i),
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

    /// Wait until every running node in `nodes` applied `height`.
    fn wait_applied(&self, nodes: &[usize], height: u64) {
        self.wait_until(&format!("nodes {nodes:?} applied {height}"), |c| {
            nodes.iter().all(|&i| c.applied(i) >= height)
        });
    }

    /// The stored committed header row (block hash ‖ header JSON) at `height`.
    fn header_row(&self, i: usize, height: u64) -> Option<Vec<u8>> {
        self.db(i)
            .get_cf_raw(CF_BLOCK_HEADERS, &height.to_be_bytes())
            .expect("header read")
    }

    fn header(&self, i: usize, height: u64) -> TorusBlockHeader {
        header_at(self.db(i), height).expect("stored header")
    }

    /// `h_n` of node `i` for every height it hashed, recomputed from its write
    /// sets (the chain starts at height 1, every node's running-hash activation);
    /// checked against the hash it stored when it last stopped.
    fn hashes(&self, i: usize) -> BTreeMap<u64, [u8; 32]> {
        let node = &self.nodes[i];
        let mut out = BTreeMap::new();
        let mut prev = [0u8; 32];
        for (n, (&height, writes)) in node.writes.iter().enumerate() {
            assert_eq!(
                height,
                n as u64 + 1,
                "node {i}: hashed heights are contiguous from 1"
            );
            prev = running_hash::next_running_hash(&prev, height, writes);
            out.insert(height, prev);
        }
        if let Some((height, hash)) = node.stored_hash {
            assert_eq!(out.get(&height), Some(&hash), "node {i}: stored h_{height}");
        }
        out
    }

    /// The test rule's row in node `i`'s write set at `height` (`None`: the
    /// block did not write it).
    fn rule_write(&self, i: usize, height: u64) -> Option<(u64, u64)> {
        let id = running_hash::hashed_cf_id(CF_TREASURY).unwrap();
        self.nodes[i]
            .writes
            .get(&height)?
            .iter()
            .find_map(|(cf, key, value)| {
                (*cf == id && key == RULE_KEY).then(|| {
                    let v = value.as_ref().expect("the rule never deletes");
                    (
                        u64::from_be_bytes(v[..8].try_into().unwrap()),
                        u64::from_be_bytes(v[8..16].try_into().unwrap()),
                    )
                })
            })
    }

    /// Every stopped node in `nodes` hashed `1..=top` with identical write sets
    /// and `h_n`, and the test rule ran exactly at `x_at..=top` (`None`: never).
    fn assert_agree(&self, nodes: &[usize], top: u64, x_at: Option<u64>) {
        let hashes: Vec<_> = nodes.iter().map(|&i| self.hashes(i)).collect();
        for height in 1..=top {
            let a = nodes[0];
            let writes = self.nodes[a].writes.get(&height);
            assert!(writes.is_some(), "node {a}: height {height} hashed");
            for (k, &b) in nodes.iter().enumerate().skip(1) {
                assert_eq!(
                    self.nodes[b].writes.get(&height),
                    writes,
                    "write set at {height}: node {b} vs node {a}"
                );
                assert_eq!(
                    hashes[k].get(&height),
                    hashes[0].get(&height),
                    "h_{height}: node {b} vs node {a}"
                );
            }
            for &i in nodes {
                let want = x_at
                    .filter(|&h| height >= h)
                    .map(|h| (height - h + 1, height));
                assert_eq!(
                    self.rule_write(i, height),
                    want,
                    "node {i}: the test rule at {height}"
                );
            }
        }
    }

    /// The running nodes in `nodes` store the same committed header at every
    /// height `1..=top` (no fork), carrying the test bit iff the height is at
    /// or above `x_at`.
    fn assert_headers(&self, nodes: &[usize], top: u64, x_at: Option<u64>) {
        for height in 1..=top {
            let row = self.header_row(nodes[0], height);
            assert!(row.is_some(), "node {}: header {height}", nodes[0]);
            for &i in &nodes[1..] {
                assert_eq!(self.header_row(i, height), row, "header {height}: node {i}");
            }
            let want = RSH
                | if x_at.is_some_and(|h| height >= h) {
                    X.bit()
                } else {
                    0
                };
            assert_eq!(
                self.header(nodes[0], height).features,
                want,
                "bits at {height}"
            );
        }
    }

    fn min_applied(&self, nodes: &[usize]) -> u64 {
        nodes.iter().map(|&i| self.applied(i)).min().unwrap()
    }

    fn refusals(&self, i: usize) -> Vec<(&'static str, u64)> {
        self.nodes[i].refusals.lock().unwrap().clone()
    }
}

impl Drop for Cluster {
    fn drop(&mut self) {
        // Replicas first (their threads hold the DBs), even on a failed assert.
        for node in &mut self.nodes {
            drop(node.replica.take());
            if let Some(state) = node.state.take() {
                running_hash::capture_take(&state);
            }
        }
    }
}

const ALL: [usize; N] = [0, 1, 2, 3];

/// Test plan 1 (crossing): four upgraded nodes, the test feature at `H`. Every
/// node runs the old rule below `H` and the new one from `H` (the headers carry
/// the bit from exactly `H`), with identical write sets and running hashes at
/// every height before and after.
#[test]
fn p00_crossing_every_node_switches_at_h_with_identical_running_hashes() {
    const H: u64 = 6;
    let mut c = Cluster::new();
    c.start_all(&Binary::new(H));
    c.wait_applied(&ALL, H + 6);
    let top = c.min_applied(&ALL);
    c.assert_headers(&ALL, top, Some(H));
    for i in ALL {
        assert!(c.refusals(i).is_empty(), "node {i}: {:?}", c.refusals(i));
        assert!(
            !c.nodes[i].exec_failed.load(Ordering::SeqCst),
            "node {i} failed"
        );
    }
    c.stop_all();
    c.assert_agree(&ALL, top, Some(H));
}

/// Test plan 2 (mixed binaries, 3 of 4 upgraded): node 3 runs a release that
/// does not know the test feature. Below `H` all four agree; at `H` the chain
/// goes on with the three upgraded nodes, and the old node refuses the block
/// (`UPGRADE REQUIRED` at insertion / before the vote) and stays at an applied
/// height below `H` that no longer moves; its committed history is a prefix
/// of the others' (no fork) and it never executes a block it cannot. Upgraded
/// (restarted on the new release), it catches up and agrees at every height.
#[test]
fn p00_mixed_binaries_three_of_four_go_on_and_the_old_node_halts_below_h() {
    const H: u64 = 6;
    const NEW: [usize; 3] = [0, 1, 2];
    let mut c = Cluster::new();
    for i in NEW {
        c.start(i, &Binary::new(H));
    }
    c.start(3, &Binary::old());
    c.wait_applied(&NEW, H + 4);
    c.wait_until("the old node refuses H", |c| {
        c.refusals(3).iter().any(|r| r.1 == H)
    });
    c.wait_until("the old node executed what it committed", |c| {
        c.applied(3) == c.committed(3)
    });
    let halted = c.applied(3);
    let halted_committed = c.committed(3);
    let tip = c.min_applied(&NEW);
    // The chain goes on without the old node; the old node does not move.
    c.wait_applied(&NEW, tip + 4);
    assert_eq!(
        c.applied(3),
        halted,
        "the old node's applied height is unchanged"
    );
    assert_eq!(
        c.committed(3),
        halted_committed,
        "the old node commits nothing more"
    );
    assert!(halted < H && halted_committed < H, "{}", c.status());
    assert!(
        !c.nodes[3].exec_failed.load(Ordering::SeqCst),
        "nothing unknown reached execution"
    );
    for (site, height) in c.refusals(3) {
        assert!(matches!(site, "vote" | "insert" | "propose"), "{site}");
        assert!(height >= H, "refused {height} < H ({site})");
    }
    for i in NEW {
        assert!(c.refusals(i).is_empty(), "node {i}: {:?}", c.refusals(i));
        assert!(
            !c.nodes[i].exec_failed.load(Ordering::SeqCst),
            "node {i} failed"
        );
    }
    let top = c.min_applied(&NEW);
    c.assert_headers(&NEW, top, Some(H));
    c.assert_headers(&[0, 3], halted_committed, Some(H));
    // The operator upgrades the old node: it catches up from below H.
    c.stop(3);
    assert!(c.nodes[3].writes.keys().all(|&h| h <= halted));
    c.start(3, &Binary::new(H));
    c.wait_applied(&ALL, top + 4);
    let top = c.min_applied(&ALL);
    c.assert_headers(&ALL, top, Some(H));
    c.stop_all();
    c.assert_agree(&ALL, top, Some(H));
}

/// Test plan 2 (mixed binaries, 2 of 4 upgraded): with two old nodes no block
/// at `H` gets a quorum, so the chain stalls below `H` on every node, without
/// a fork: no node stores a header at `H`, and all store the same headers below.
#[test]
fn p00_mixed_binaries_two_of_four_stall_below_h_without_a_fork() {
    const H: u64 = 6;
    let mut c = Cluster::new();
    c.start(0, &Binary::new(H));
    c.start(1, &Binary::new(H));
    c.start(2, &Binary::old());
    c.start(3, &Binary::old());
    // Commits up to the last block whose successor still had a quorum.
    c.wait_applied(&ALL, H - 3);
    c.wait_until("an old node refuses H", |c| {
        [2, 3]
            .iter()
            .any(|&i| c.refusals(i).iter().any(|r| r.1 == H))
    });
    let stalled: Vec<u64> = ALL.iter().map(|&i| c.committed(i)).collect();
    std::thread::sleep(Duration::from_secs(5));
    let after: Vec<u64> = ALL.iter().map(|&i| c.committed(i)).collect();
    assert!(
        after.iter().all(|&h| h < H),
        "stalled below H: {}",
        c.status()
    );
    for i in ALL {
        assert!(
            c.header_row(i, H).is_none(),
            "node {i} stored a header at H"
        );
        assert!(
            !c.nodes[i].exec_failed.load(Ordering::SeqCst),
            "node {i} failed"
        );
    }
    // No fork: every node's committed headers are a prefix of the longest.
    let longest = (0..N).max_by_key(|&i| after[i]).unwrap();
    for i in ALL {
        c.assert_headers(&[longest, i], after[i], Some(H));
    }
    assert!(after.iter().zip(&stalled).all(|(a, s)| a >= s));
    let top = ALL.iter().map(|&i| c.applied(i)).min().unwrap();
    c.stop_all();
    c.assert_agree(&ALL, top, Some(H));
}

/// Test plan 3 (crash across `H`): node 3's execution stops before `hold`
/// while its consensus commits on past `H`, then it dies (applied `hold - 1`,
/// committed above `H`). Restarted, its boot replay crosses `H` by the stored
/// headers and it rejoins; every height matches the uncrashed nodes.
fn crash_across_h_replay_matches(hold_at: u64) {
    const H: u64 = 8;
    let mut c = Cluster::new();
    let hold = ExecHold::new(hold_at);
    for i in 0..3 {
        c.start(i, &Binary::new(H));
    }
    c.start(3, &Binary::new(H).held(&hold));
    c.wait_until("node 3 commits past H while its execution is held", |c| {
        c.committed(3) >= hold_at.max(H) + 3
    });
    assert_eq!(c.applied(3), hold_at - 1, "node 3 executed up to the hold");
    let committed = c.committed(3);
    c.stop(3);
    // Restart: the boot replay re-executes (applied, committed] across H.
    c.start(3, &Binary::new(H));
    assert!(committed > H && committed > hold_at, "{}", c.status());
    assert!(
        c.applied(3) >= committed,
        "the boot replay ran the whole gap: {}",
        c.status()
    );
    c.wait_applied(&ALL, committed + 4);
    let top = c.min_applied(&ALL);
    c.assert_headers(&ALL, top, Some(H));
    for i in ALL {
        assert!(c.refusals(i).is_empty(), "node {i}: {:?}", c.refusals(i));
    }
    c.stop_all();
    c.assert_agree(&ALL, top, Some(H));
}

#[test]
fn p00_crash_applied_two_below_h_replay_matches() {
    crash_across_h_replay_matches(7); // applied H-2
}

#[test]
fn p00_crash_applied_just_below_h_replay_matches() {
    crash_across_h_replay_matches(8); // applied H-1
}

#[test]
fn p00_crash_applied_above_h_replay_matches() {
    crash_across_h_replay_matches(10); // applied H+1
}

/// Test plan 3, an old binary after the crash (a downgrade): node 3 committed
/// past `H` but executed only below it, and restarts on a release that does
/// not know the test feature. Its boot replay executes up to `H - 1` and
/// fail-stops before `H` (`UPGRADE REQUIRED`): nothing of `H` is executed or
/// flushed, and the applied height stays `H - 1`. Upgraded again, it replays
/// on from `H`, rejoins and agrees at every height.
#[test]
fn p00_crash_then_old_binary_replay_fail_stops_before_h() {
    const H: u64 = 8;
    let mut c = Cluster::new();
    let hold = ExecHold::new(H - 1);
    for i in 0..3 {
        c.start(i, &Binary::new(H));
    }
    c.start(3, &Binary::new(H).held(&hold));
    c.wait_until("node 3 commits past H", |c| c.committed(3) >= H + 3);
    assert_eq!(c.applied(3), H - 2);
    c.stop(3);
    c.start(3, &Binary::old());
    assert!(
        c.nodes[3].exec_failed.load(Ordering::SeqCst),
        "the replay fail-stopped"
    );
    assert_eq!(c.applied(3), H - 1, "replayed below H only");
    let others = [0, 1, 2];
    c.wait_applied(&others, H + 4);
    assert_eq!(c.applied(3), H - 1, "the old node does not execute on");
    c.stop(3);
    assert!(
        c.nodes[3].writes.keys().all(|&h| h < H),
        "nothing of H flushed"
    );
    // The operator upgrades it: the replay runs on from H and it rejoins.
    c.start(3, &Binary::new(H));
    assert!(!c.nodes[3].exec_failed.load(Ordering::SeqCst));
    let tip = c.min_applied(&others);
    c.wait_applied(&ALL, tip + 4);
    let top = c.min_applied(&ALL);
    c.stop_all();
    c.assert_agree(&ALL, top, Some(H));
}

/// Test plan 4 (replay from genesis with a newer binary that moves a future
/// height): the chain runs with the test feature scheduled far ahead; every
/// node then upgrades to a release that moves it to `H2`, still above every
/// committed height, and node 3 replays the chain from genesis on an empty
/// data dir (block sync). History is unchanged: every `h_n` below the upgrade
/// equals the one before it, on every node and on the replayed one; the
/// feature switches on at `H2` (not at the old height) on all four.
#[test]
fn p00_replay_from_genesis_with_a_moved_future_height_keeps_history() {
    const FAR: u64 = 900;
    let mut c = Cluster::new();
    c.start_all(&Binary::new(FAR));
    c.wait_applied(&ALL, 5);
    c.stop_all();
    let before: Vec<_> = ALL.iter().map(|&i| c.hashes(i)).collect();
    let committed_before = c.nodes.iter().map(|n| n.writes.len() as u64).max().unwrap();
    let h2 = committed_before + 6;
    c.wipe(3);
    c.start_all(&Binary::new(h2));
    // Every node committed below h2 before the upgrade.
    for i in 0..3 {
        assert!(c.committed(i) < h2, "{}", c.status());
    }
    c.wait_applied(&ALL, h2 + 4);
    let top = c.min_applied(&ALL);
    c.assert_headers(&ALL, top, Some(h2));
    for i in ALL {
        assert!(c.refusals(i).is_empty(), "node {i}: {:?}", c.refusals(i));
    }
    c.stop_all();
    c.assert_agree(&ALL, top, Some(h2));
    let after: Vec<_> = ALL.iter().map(|&i| c.hashes(i)).collect();
    for i in 0..3 {
        for (height, hash) in &before[i] {
            assert_eq!(
                after[i].get(height),
                Some(hash),
                "node {i}: h_{height} changed"
            );
            assert_eq!(
                after[3].get(height),
                Some(hash),
                "replayed node: h_{height}"
            );
        }
    }
}

/// Test plan 4 / doc section 1 ("a certified header whose known bits differ
/// from the node's own schedule: execute by the header"): node 3 committed
/// blocks past `H` but executed only below it, then restarts on a binary
/// that knows the test feature but schedules it elsewhere (far ahead). Its
/// boot replay executes the stored blocks by their header bits (history
/// unchanged, no fail-stop); live, it refuses the next blocks (their bits
/// differ from its schedule) and stays behind while the other three go on.
#[test]
fn p00_a_node_with_another_schedule_replays_by_the_header_bits_then_refuses() {
    const H: u64 = 8;
    let mut c = Cluster::new();
    let hold = ExecHold::new(H - 1);
    for i in 0..3 {
        c.start(i, &Binary::new(H));
    }
    c.start(3, &Binary::new(H).held(&hold));
    c.wait_until("node 3 commits past H", |c| c.committed(3) >= H + 3);
    let committed = c.committed(3);
    c.stop(3);
    c.start(3, &Binary::new(900));
    assert!(
        !c.nodes[3].exec_failed.load(Ordering::SeqCst),
        "executes by the header"
    );
    assert!(
        c.applied(3) >= committed,
        "the replay ran the gap across H: {}",
        c.status()
    );
    let others = [0, 1, 2];
    c.wait_applied(&others, committed + 4);
    // Live, it inserts no block whose bits differ from its schedule: it stops.
    let stuck = c.committed(3);
    let tip = c.min_applied(&others);
    c.wait_applied(&others, tip + 4);
    assert_eq!(
        c.committed(3),
        stuck,
        "node 3 does not follow: {}",
        c.status()
    );
    assert!(stuck < c.min_applied(&others));
    assert!(!c.nodes[3].exec_failed.load(Ordering::SeqCst));
    let replayed = c.applied(3);
    c.stop_all();
    c.assert_agree(&ALL, replayed, Some(H));
}
