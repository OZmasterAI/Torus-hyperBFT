//! Dead-leader skip phase 3 (docs/plans/dead-leader-skip.md §4 "Multi-node"): b1 end to end on a
//! cluster of real [`TorusApp`]s.
//!
//! The validators run in one process as in `p00_cluster_tests`: hotstuff_rs replicas over the
//! in-process [`ChannelNetwork`], each with a real [`TorusApp`] (state DB + consensus DB, the node
//! layout, the execution pipeline, the running state hash), stopped, restarted and crashed like
//! processes. Unlike there, every state DB is seeded with the genesis validators (staking rows
//! and the installed epoch set, as `torus-genesis` does) and dead-leader skip is on (K = 3), so
//! the whole loop runs: the commit path carries each justify QC's evidence, execution attributes
//! leader slots (A1) and plans exclusions and re-admissions at boundaries (A2 / A3), consensus
//! emits the delta in a real validator-set update (A4) and hotstuff's `select_leader` skips the
//! excluded validator.
//!
//! The expected heights come from the committed QC chain read from a live replica's block tree
//! ([`Cluster::chain`]): the height whose execution brings the dead validator's run to K fixes
//! the boundary that plans its exclusion and the height of the update. Every test ends with the
//! running-hash write sets of the nodes that ran compared height by height.
//!
//! The tests whose exclusion update is committed while the dead validator still leads were
//! `#[ignore]`d until the update-window vote rule landed (fix/vsu-window-flake f407d49c: an
//! update block proposed two views before a silent leader's slot gets its Precommit PC locked at
//! the backup aggregator, which then refused the block's re-proposal; with one of four down every
//! vote is needed and the window livelocked). With the reflexive lock (batch 12) and the sibling
//! unlock rule (batch C) they run again (batch C: 7 tests, 5 of 5 stress runs). Still open: at
//! 2:2:1 the update cannot commit (`dl_cluster_2_2_1_stake_one_down_is_excluded`, ignored; run it
//! with `--run-ignored all`).

use super::*;

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use hotstuff_rs::pacemaker::select_leader;
use hotstuff_rs::replica::{Configuration, Replica, ReplicaSpec};
use hotstuff_rs::types::data_types::{BlockHeight, BufferSize, ChainID, EpochLength, ViewNumber};
use hotstuff_rs::types::validator_set::ValidatorSet as HsSet;
use tempfile::TempDir;
use torus_economics::leader_stats::{self, LeaderStats};
use torus_state::running_hash::{self, WriteEntry};
use torus_types::{ActivationSchedule, Feature, U256};

use crate::leader_skip::{LeaderEvidence, QC_GENERIC};
use crate::{ChannelNetwork, GenesisConfig};

/// Epoch length in heights (a multiple of the test EVM interval).
const L: u64 = 10;
/// K, the genesis default.
const K: u32 = 3;
const WAIT: Duration = Duration::from_secs(240);
const POLL: Duration = Duration::from_millis(10);
/// Stake per weight unit, whole TRS (the minimum self-delegation): power = weight * 10,000.
const TRS_PER_WEIGHT: u64 = 10_000;

/// How a node is started.
#[derive(Clone)]
struct Run {
    k: u32,
    cap: u32,
    hold: Option<Arc<ExecHold>>,
    keep_manifests: bool,
}

impl Run {
    /// Dead-leader skip on: K = 3, the default j cap.
    fn on() -> Self {
        Self {
            k: K,
            cap: torus_types::DEFAULT_LEADER_EXCLUSION_CAP,
            hold: None,
            keep_manifests: false,
        }
    }

    fn k(mut self, k: u32) -> Self {
        self.k = k;
        self
    }

    fn cap(mut self, cap: u32) -> Self {
        self.cap = cap;
        self
    }

    fn held(mut self, hold: &Arc<ExecHold>) -> Self {
        self.hold = Some(hold.clone());
        self
    }

    fn keep_manifests(mut self) -> Self {
        self.keep_manifests = true;
        self
    }
}

fn chain_config(n: usize, run: &Run) -> ChainConfig {
    let schedule =
        ActivationSchedule::merge(&[(Feature::RunningStateHash, 1)].into_iter().collect(), &[])
            .expect("test schedule");
    ChainConfig {
        chain_id: TORUS_CHAIN_ID,
        chain_name: "dead-leader-cluster".to_string(),
        evm_gas_limit: torus_types::DEFAULT_EVM_GAS_LIMIT,
        evm_block_interval: TEST_EVM_BLOCK_INTERVAL,
        evm_big_block_interval: torus_types::DEFAULT_EVM_BIG_BLOCK_INTERVAL,
        evm_big_gas_limit: torus_types::DEFAULT_EVM_BIG_GAS_LIMIT,
        base_fee_per_gas: 1_000_000_000,
        epoch_length: L,
        max_validators: n as u32,
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
        book_mode: Default::default(),
        leader_miss_threshold: run.k,
        leader_exclusion_cap: run.cap,
    }
}

struct Node {
    dir: TempDir,
    replica: Option<Replica<RocksKVStore>>,
    state: Option<StateDb>,
    exec_failed: Arc<AtomicBool>,
    /// Every flushed height's running-hash write set, across restarts.
    writes: BTreeMap<u64, Vec<WriteEntry>>,
    /// `read_running_hash` when the node last stopped.
    stored_hash: Option<(u64, [u8; 32])>,
}

struct Cluster {
    keys: Vec<ed25519_dalek::SigningKey>,
    weights: Vec<u64>,
    networks: Vec<ChannelNetwork>,
    genesis: GenesisConfig,
    nodes: Vec<Node>,
}

/// The address of validator `i`.
fn addr(i: usize) -> [u8; 20] {
    [0x70 + i as u8; 20]
}

impl Cluster {
    /// One validator per weight (stake `weight * 10,000` TRS, hotstuff power the same in whole
    /// TRS, as `torus-genesis` maps stake to power).
    fn new(weights: &[u64]) -> Self {
        torus_state::native_trie::force_native_trie_maintenance_on_for_tests();
        // `DL_TEST_LOG=info` (or any `EnvFilter` directive) logs the cluster.
        if let Ok(filter) = std::env::var("DL_TEST_LOG") {
            let _ = tracing_subscriber::fmt()
                .with_env_filter(tracing_subscriber::EnvFilter::new(filter))
                .with_test_writer()
                .try_init();
        }
        let keys: Vec<_> = (0..weights.len())
            .map(|i| ed25519_dalek::SigningKey::from_bytes(&[0x60 + i as u8; 32]))
            .collect();
        let vks: Vec<_> = keys.iter().map(|k| k.verifying_key()).collect();
        let genesis = GenesisConfig {
            chain_id: TORUS_CHAIN_ID,
            validators: vks
                .iter()
                .zip(weights)
                .map(|(vk, w)| (*vk, w * TRS_PER_WEIGHT))
                .collect(),
        };
        let nodes = (0..weights.len())
            .map(|_| Node {
                dir: TempDir::new().unwrap(),
                replica: None,
                state: None,
                exec_failed: Default::default(),
                writes: BTreeMap::new(),
                stored_hash: None,
            })
            .collect();
        Self {
            networks: ChannelNetwork::create_test_network(&vks),
            keys,
            weights: weights.to_vec(),
            genesis,
            nodes,
        }
    }

    fn n(&self) -> usize {
        self.keys.len()
    }

    fn vk(&self, i: usize) -> VerifyingKey {
        self.keys[i].verifying_key()
    }

    /// The genesis hotstuff set (no exclusion).
    fn genesis_set(&self) -> HsSet {
        self.genesis.validator_set()
    }

    /// Validator `i`'s position in the set (the QC signature bitmap's order).
    fn position(&self, i: usize) -> usize {
        let vk = self.vk(i);
        self.genesis_set()
            .validators()
            .position(|v| *v == vk)
            .expect("a member")
    }

    /// The genesis state of a fresh data dir, as `torus_genesis::Genesis::initialize` writes it:
    /// the validators' staking rows and the installed epoch set.
    fn seed_genesis(&self, state: &StateDb) {
        let staking = StakingManager::new(state.clone());
        let wei = U256::from(10u64).pow(U256::from(18u64));
        for (i, weight) in self.weights.iter().enumerate() {
            let address = Address::new(addr(i));
            let validator = torus_economics::types::ValidatorState {
                address,
                pubkey: self.vk(i).to_bytes(),
                commission_bps: 0,
                self_stake: U256::from(weight * TRS_PER_WEIGHT) * wei,
                total_delegated: U256::ZERO,
                status: torus_economics::types::ValidatorStatus::Active,
                jailed_until: None,
                last_commission_change_block: None,
                oracle_signer: None,
            };
            staking.put_validator(&address, &validator).unwrap();
        }
        let set = EpochManager::compute_new_validator_set(&staking, u32::MAX, 0).unwrap();
        staking.record_genesis_epoch_set(&set).unwrap();
    }

    /// Start (or restart) node `i` on its data dir: boot replay runs in `TorusApp` construction,
    /// before the replica starts.
    fn start(&mut self, i: usize, run: &Run) {
        assert!(self.nodes[i].replica.is_none(), "node {i} is running");
        let dir = self.nodes[i].dir.path().to_path_buf();
        let fresh = !dir.join("consensus").exists();
        let state = StateDb::open(&dir.join("state")).expect("open state db");
        let consensus = StateDb::open_consensus(&dir.join("consensus")).expect("open consensus db");
        let kv = RocksKVStore::new(consensus.db_arc());
        if fresh {
            self.seed_genesis(&state);
            Replica::initialize(
                kv.clone(),
                self.genesis.initial_app_state(),
                self.genesis.validator_set_state(),
            );
        }
        running_hash::capture_begin(&state);
        let options = AppOptions {
            known_features: torus_types::KNOWN_FEATURES,
            exec_hold: run.hold.clone(),
            keep_manifests: run.keep_manifests,
        };
        let app = TorusApp::try_new_with_options(
            state.clone(),
            consensus,
            &chain_config(self.n(), run),
            None,
            None,
            Some(self.keys[i].clone()),
            options,
        )
        .expect("TorusApp startup");
        // A (re)started validator gets nothing sent while it was down.
        self.networks[i].discard_pending();
        let node = &mut self.nodes[i];
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

    /// Stop node `i` (the replica and its app: the execution thread drains and joins, a held one
    /// dies at its hold) and keep what it captured.
    fn stop(&mut self, i: usize) {
        let node = &mut self.nodes[i];
        drop(node.replica.take().expect("node is running"));
        let state = node.state.take().unwrap();
        node.stored_hash = running_hash::read_running_hash(&state);
        for (height, writes) in running_hash::capture_take(&state) {
            if let Some(before) = node.writes.get(&height) {
                assert_eq!(
                    before, &writes,
                    "node {i}: height {height} hashed twice (before and after a restart) with \
                     different write sets"
                );
            }
            node.writes.insert(height, writes);
        }
    }

    fn stop_all(&mut self) {
        for i in 0..self.n() {
            if self.nodes[i].replica.is_some() {
                self.stop(i);
            }
        }
    }

    fn running(&self) -> Vec<usize> {
        (0..self.n())
            .filter(|&i| self.nodes[i].replica.is_some())
            .collect()
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

    fn failed(&self, i: usize) -> bool {
        self.nodes[i].exec_failed.load(Ordering::SeqCst)
    }

    fn replica(&self, i: usize) -> &Replica<RocksKVStore> {
        self.nodes[i].replica.as_ref().expect("node is running")
    }

    /// Node `i`'s committed hotstuff validator set.
    fn hs_set(&self, i: usize) -> HsSet {
        self.replica(i)
            .block_tree_camera()
            .snapshot()
            .committed_validator_set()
            .expect("committed validator set")
    }

    /// The (Torus) height of the last validator-set-updating block node `i` knows. Hotstuff
    /// numbers blocks from 0, the Torus header from 1: Torus height = hotstuff height + 1.
    fn update_height(&self, i: usize) -> Option<u64> {
        self.replica(i)
            .block_tree_camera()
            .snapshot()
            .validator_set_update_block_height()
            .expect("update height")
            .map(|h| h.int() + 1)
    }

    fn excluded(&self, i: usize, v: usize) -> bool {
        self.hs_set(i).is_leader_excluded(&self.vk(v))
    }

    /// The Torus height of node `i`'s highest committed hotstuff block (0 = none). The state
    /// DB's committed header can be a little ahead: the commit hook runs before hotstuff's batch.
    fn hs_committed(&self, i: usize) -> u64 {
        let snapshot = self.replica(i).block_tree_camera().snapshot();
        snapshot
            .highest_committed_block()
            .expect("highest committed block")
            .and_then(|hash| snapshot.block_height(&hash).expect("its height"))
            .map_or(0, |h| h.int() + 1)
    }

    /// The justify QC evidence of every committed block of Torus height `1..=top` in node `i`'s
    /// block tree (hotstuff height `h - 1`).
    fn chain(&self, i: usize, top: u64) -> Vec<(u64, LeaderEvidence)> {
        let snapshot = self.replica(i).block_tree_camera().snapshot();
        (1..=top)
            .map(|h| {
                let hash = snapshot
                    .block_at_height(BlockHeight::new(h - 1))
                    .expect("block at height")
                    .unwrap_or_else(|| panic!("node {i}: no committed block at Torus height {h}"));
                let justify = snapshot.block_justify(&hash).expect("justify");
                (h, LeaderEvidence::of(&justify))
            })
            .collect()
    }

    fn stats(&self, i: usize, v: usize) -> LeaderStats {
        leader_stats::leader_stats(self.db(i), &addr(v)).unwrap()
    }

    /// The addresses of node `i`'s installed exclusions (execution's `lx_current`).
    fn installed(&self, i: usize) -> Vec<[u8; 20]> {
        leader_stats::installed_leader_exclusions(self.db(i))
            .unwrap()
            .iter()
            .map(|m| m.address)
            .collect()
    }

    fn proposer(&self, i: usize, height: u64) -> Address {
        header_at(self.db(i), height)
            .expect("stored header")
            .proposer
    }

    fn status(&self) -> String {
        (0..self.n())
            .map(|i| match self.nodes[i].replica {
                Some(_) => format!(
                    "node {i}: applied {} committed {} failed {} update {:?} excluded {:?}",
                    self.applied(i),
                    self.committed(i),
                    self.failed(i),
                    self.update_height(i),
                    (0..self.n())
                        .filter(|&v| self.excluded(i, v))
                        .collect::<Vec<_>>()
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
            for i in self.running() {
                assert!(!self.failed(i), "node {i} fail-stopped: {}", self.status());
            }
            std::thread::sleep(POLL);
        }
    }

    /// Wait until every node in `nodes` committed and applied `height`.
    fn wait_applied(&self, nodes: &[usize], height: u64) {
        self.wait_until(&format!("nodes {nodes:?} applied {height}"), |c| {
            nodes.iter().all(|&i| c.applied(i) >= height)
        });
    }

    /// Wait until every node in `nodes` has validator `v` leader-excluded (`excluded`) or not
    /// in its committed hotstuff set, by an update above `after`; returns that update's height.
    fn wait_exclusion(&self, nodes: &[usize], v: usize, excluded: bool, after: u64) -> u64 {
        let what = if excluded { "excluded" } else { "re-admitted" };
        self.wait_until(
            &format!("validator {v} {what} by an update above {after} on {nodes:?}"),
            |c| {
                nodes.iter().all(|&i| {
                    c.excluded(i, v) == excluded && c.update_height(i).is_some_and(|h| h > after)
                })
            },
        );
        let heights: Vec<_> = nodes.iter().map(|&i| self.update_height(i)).collect();
        assert!(
            heights.iter().all(|h| *h == heights[0]),
            "one update height on every node: {heights:?}"
        );
        heights[0].unwrap()
    }

    /// `h_n` of node `i` for every height it hashed, recomputed from its write sets (contiguous
    /// from 1) and checked against the hash it stored when it last stopped.
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

    /// Every stopped node in `nodes` hashed `1..=top` with identical write sets and `h_n`.
    fn assert_agree(&self, nodes: &[usize], top: u64) {
        let hashes: Vec<_> = nodes.iter().map(|&i| self.hashes(i)).collect();
        let a = nodes[0];
        for height in 1..=top {
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
        }
    }

    /// The highest height every node in `nodes` applied.
    fn min_applied(&self, nodes: &[usize]) -> u64 {
        nodes.iter().map(|&i| self.applied(i)).min().unwrap()
    }

    /// The validator-statistics rows (`leader_stats:v:` keys) in node `i`'s write set at `height`.
    fn validator_rows(&self, i: usize, height: u64) -> Vec<Vec<u8>> {
        let meta = running_hash::hashed_cf_id(CF_CONSENSUS_META).unwrap();
        self.nodes[i]
            .writes
            .get(&height)
            .map(|writes| {
                writes
                    .iter()
                    .filter(|(cf, key, _)| *cf == meta && key.starts_with(b"leader_stats:v:"))
                    .map(|(_, key, _)| key.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Whether node `i`'s write set at `height` touches a `leader_stats:` or `epoch_vset:lx_` row.
    fn wrote_leader_rows(&self, i: usize, height: u64) -> bool {
        let meta = running_hash::hashed_cf_id(CF_CONSENSUS_META).unwrap();
        self.nodes[i].writes.get(&height).is_some_and(|writes| {
            writes.iter().any(|(cf, key, _)| {
                *cf == meta
                    && (key.starts_with(b"leader_stats:") || key.starts_with(b"epoch_vset:lx_"))
            })
        })
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

/// A1 over a committed QC chain, as execution runs it (the oracle): `(height, evidence)` in
/// height order, leader(v) over `set`. Returns the first height whose execution brings
/// validator `dead`'s run of missed leader slots to `k` (`None`: never).
fn kth_miss_height(
    qcs: &[(u64, LeaderEvidence)],
    set: &HsSet,
    dead: &VerifyingKey,
    k: u32,
) -> Option<u64> {
    let leader = |v: u64| select_leader(ViewNumber::new(v), set);
    let mut cursor: Option<(u64, bool)> = None;
    let mut run = 0u32;
    for (height, ev) in qcs {
        if ev.phase == QC_GENERIC {
            match cursor {
                Some((c, false)) if ev.view > c => {
                    run += (c + 1..ev.view).filter(|v| leader(*v) == *dead).count() as u32;
                    if leader(ev.view) == *dead {
                        run = 0;
                    }
                }
                None if leader(ev.view) == *dead => run = 0,
                _ => {}
            }
        }
        cursor = Some((ev.view, ev.phase != QC_GENERIC));
        if run >= k {
            return Some(*height);
        }
    }
    None
}

/// The views no QC of the chain certifies between consecutive generic QCs of the heights
/// `from..` (the views the leaders of which timed out), and the views of that span that `dead`
/// leads under `set`.
fn gaps_and_slots(
    qcs: &[(u64, LeaderEvidence)],
    from: u64,
    set: &HsSet,
    dead: &VerifyingKey,
) -> (Vec<u64>, usize) {
    let tail: Vec<_> = qcs.iter().filter(|(h, _)| *h >= from).collect();
    let mut gaps = Vec::new();
    for pair in tail.windows(2) {
        let (a, b) = (&pair[0].1, &pair[1].1);
        if a.phase == QC_GENERIC && b.phase == QC_GENERIC && b.view > a.view {
            gaps.extend(a.view + 1..b.view);
        }
    }
    let (first, last) = (tail[0].1.view, tail[tail.len() - 1].1.view);
    let slots = (first..=last)
        .filter(|v| select_leader(ViewNumber::new(*v), set) == *dead)
        .count();
    (gaps, slots)
}

/// The first boundary at or above `height`.
fn boundary_at_or_above(height: u64) -> u64 {
    height.div_ceil(L) * L
}

/// One validator `dead` never runs. A1-A4 on the cluster: its exclusion takes effect at exactly
/// the update the QC chain predicts (the plan of the first boundary at or above the height whose
/// execution brings its run to K, i.e. within 2L heights of its K-th miss), on every live
/// replica; from then on no view is lost to it, its statistics stay those the exclusion wrote,
/// and it is never re-admitted while down (three more boundaries pass). The live replicas'
/// running-hash write sets agree at every height.
fn one_down(weights: &[u64], dead: usize) {
    let mut c = Cluster::new(weights);
    let live: Vec<usize> = (0..c.n()).filter(|&i| i != dead).collect();
    for &i in &live {
        c.start(i, &Run::on());
    }
    let x = c.vk(dead);
    let update = c.wait_exclusion(&live, dead, true, 0);
    // Three more boundaries: the re-admission floor (update + L) and two after it pass.
    let target = update + 3 * L + 2;
    c.wait_applied(&live, target);
    let a = live[0];
    let qcs = c.chain(a, c.hs_committed(a));
    let set = c.genesis_set();
    let kth = kth_miss_height(&qcs, &set, &x, K).expect("the dead validator missed K slots");
    let planned_at = boundary_at_or_above(kth);
    assert_eq!(
        update,
        planned_at + L,
        "the exclusion update is the plan of the first boundary at or above the K-th miss \
         (height {kth}): {}",
        c.status()
    );
    assert!(update - kth < 2 * L, "within 2L heights of the K-th miss");
    for &i in &live {
        assert_eq!(
            c.stats(i, dead),
            LeaderStats {
                miss_run: 0,
                j: 1,
                excluded_at: Some(update),
                readmit_after: update + L,
                seen_at: None,
            },
            "node {i}: the dead validator's statistics are the exclusion's (no miss after it)"
        );
        assert_eq!(c.installed(i), vec![addr(dead)], "node {i}: installed");
        assert!(c.excluded(i, dead), "node {i}: still excluded");
        assert_eq!(
            c.update_height(i),
            Some(update),
            "node {i}: no later update (never re-admitted while down)"
        );
        for &v in &live {
            assert!(
                c.stats(i, v).miss_run < K && c.stats(i, v).excluded_at.is_none(),
                "node {i}: live validator {v} never planned for exclusion: {:?}",
                c.stats(i, v)
            );
        }
    }
    // No view lost to the dead validator after the update (its Decide and the first block after
    // it are no slots): fewer gap views than half its slots under the old schedule.
    let (gaps, slots) = gaps_and_slots(&qcs, update + 2, &set, &x);
    assert!(
        slots >= 4,
        "fixture: the span after the update holds dead slots ({slots})"
    );
    assert!(
        gaps.len() * 2 < slots,
        "views lost after the exclusion: {gaps:?} ({slots} dead-validator slots under the old \
         schedule)"
    );
    let top = c.min_applied(&live);
    c.stop_all();
    c.assert_agree(&live, top);
    // The gap rule: the update block's later phases ran in views that are no leader slots. The
    // first block after it (justified by the Decide PC) and the one after that attribute nothing:
    // no validator row in their write sets.
    for h in [update + 1, update + 2] {
        assert_eq!(
            c.validator_rows(a, h),
            Vec::<Vec<u8>>::new(),
            "height {h} (after the update at {update}) attributed leader slots"
        );
    }
}

#[test]
fn dl_cluster_one_down_at_rotation_position_0() {
    one_down(&[1, 1, 1, 1], 0);
}

#[test]
fn dl_cluster_one_down_at_rotation_position_1() {
    one_down(&[1, 1, 1, 1], 1);
}

#[test]
fn dl_cluster_one_down_at_rotation_position_2() {
    one_down(&[1, 1, 1, 1], 2);
}

#[test]
fn dl_cluster_one_down_at_rotation_position_3() {
    one_down(&[1, 1, 1, 1], 3);
}

/// The bench shape (s84 one_down_liveness at stakes 2:2:1): the stake-1 validator is down. The
/// two stake-2 validators are a quorum and the eligible power after its exclusion (4 of 5) still
/// is, so it should be excluded within 2L heights of its K-th miss and its slots stop costing a
/// timeout.
///
/// RED (s138, open): the chain halts at the exclusion update instead. A validator-set update
/// runs Prepare, Precommit and Commit in consecutive views (hotstuff_rs `safe_nudge`,
/// `repropose_block`), but with IWRR [A, B, C] the dead validator leads every third view, so
/// three consecutive live views never come; and after the Precommit QC both live validators are
/// locked on the update block and refuse its re-proposal (its justify is below the lock). Any
/// validator-set update wedges this shape (`leader_exclusion_multi_node_test`
/// `power_update_wedges_2_2_1_with_the_stake_one_validator_down`); b1 emits one exactly while a
/// validator is down. Owner decision needed (docs/plans/dead-leader-skip.md §8).
#[test]
#[ignore = "RED: a validator-set update cannot commit at 2:2:1 with the stake-1 validator down (docs/plans/dead-leader-skip.md §8)"]
fn dl_cluster_2_2_1_stake_one_down_is_excluded() {
    one_down(&[2, 2, 1], 2);
}

/// The first boundary at or above `from` whose window `(H - L, H]` holds a committed generic QC
/// carrying the signature of the validator at bitmap position `pos` (A3's evidence).
fn first_seen_boundary(qcs: &[(u64, LeaderEvidence)], from: u64, pos: usize) -> Option<u64> {
    let top = qcs.last()?.0;
    (boundary_at_or_above(from.max(L))..=top)
        .step_by(L as usize)
        .find(|&h| {
            qcs.iter().any(|(height, ev)| {
                *height > h - L
                    && *height <= h
                    && ev.phase == QC_GENERIC
                    && ev.signers.get(pos) == Some(&true)
            })
        })
}

/// A3 on the cluster: validator `x` runs, stops, is excluded, restarts on its own store and is
/// re-admitted by the plan of the first boundary at or above its exclusion whose window holds a
/// committed QC with its signature (the floor `excluded_at + L` is then reached); it proposes
/// committed blocks again, its first success resets its run and j, and it is not excluded again
/// (two more boundaries pass). All four nodes' write sets agree.
#[test]
fn dl_cluster_restarted_validator_is_readmitted_and_proposes() {
    let mut c = Cluster::new(&[1, 1, 1, 1]);
    let x = 2;
    let all = [0, 1, 2, 3];
    let live = [0, 1, 3];
    for i in all {
        c.start(i, &Run::on());
    }
    c.wait_applied(&all, 3);
    c.stop(x);
    let excluded = c.wait_exclusion(&live, x, true, 0);
    c.start(x, &Run::on());
    let readmitted = c.wait_exclusion(&all, x, false, excluded);
    let qcs = c.chain(0, c.hs_committed(0));
    let seen = first_seen_boundary(&qcs, excluded, c.position(x))
        .expect("a committed QC carries the restarted validator's signature");
    assert_eq!(
        readmitted,
        seen + L,
        "re-admitted by the plan of the first boundary >= {excluded} whose window holds its \
         signature"
    );
    c.wait_until(
        "the re-admitted validator proposes a committed block",
        |c| (readmitted + 1..=c.committed(0)).any(|h| c.proposer(0, h) == Address::new(addr(x))),
    );
    c.wait_applied(&all, readmitted + 2 * L + 2);
    for i in all {
        assert_eq!(
            c.stats(i, x),
            LeaderStats::default(),
            "node {i}: re-admitted, its success reset the run and j"
        );
        assert!(c.installed(i).is_empty(), "node {i}: nothing installed");
        assert!(!c.excluded(i, x), "node {i}: not excluded again");
        assert_eq!(
            c.update_height(i),
            Some(readmitted),
            "node {i}: no update after the re-admission"
        );
    }
    let top = c.min_applied(&all);
    c.stop_all();
    c.assert_agree(&all, top);
}

/// The re-admission floor `L * 2^j` with the j cap at 1 (genesis `leader_exclusion_cap`):
/// validator `x` is down, excluded, restarted until its re-admission is planned, killed again
/// before it takes effect (no success in between, so j is kept), three times. The floors are
/// L, 2L, 2L (not 4L: the cap) and j stays 1; no re-admission comes before its floor.
#[test]
fn dl_cluster_readmission_floor_doubles_up_to_the_cap() {
    let mut c = Cluster::new(&[1, 1, 1, 1]);
    let run = Run::on().cap(1);
    let x = 1;
    let live = [0, 2, 3];
    for i in live {
        c.start(i, &run);
    }
    let mut after = 0;
    let mut floors = Vec::new();
    for cycle in 0..3 {
        let excluded = c.wait_exclusion(&live, x, true, after);
        c.wait_applied(&[0], excluded);
        let st = c.stats(0, x);
        assert_eq!(st.excluded_at, Some(excluded), "cycle {cycle}: {st:?}");
        floors.push((st.j, st.readmit_after - excluded));
        if cycle == 2 {
            break;
        }
        c.start(x, &run);
        // Its re-admission is planned (the plan boundary's execution resets `excluded_at`);
        // it dies before the update applies, so it never leads in between.
        c.wait_until("the re-admission planned", |c| {
            c.stats(0, x).excluded_at.is_none()
        });
        c.stop(x);
        let readmitted = c.wait_exclusion(&live, x, false, excluded);
        assert!(
            readmitted >= st.readmit_after,
            "cycle {cycle}: re-admitted at {readmitted}, before the floor {}",
            st.readmit_after
        );
        after = readmitted;
    }
    assert_eq!(
        floors,
        vec![(1, L), (1, 2 * L), (1, 2 * L)],
        "(j, readmit_after - excluded_at) per exclusion: the floor doubles once, then the cap"
    );
    let top = c.min_applied(&live);
    c.stop_all();
    c.assert_agree(&live, top);
}

/// The boundary whose exclusion plan (written by node `i`'s execution) excludes validator `v`.
fn planned_exclusion(c: &Cluster, i: usize, v: usize) -> Option<u64> {
    let key = c.vk(v).to_bytes();
    (1..=20u64).map(|k| k * L).find(|&b| {
        leader_stats::leader_exclusion_plan(c.db(i), b)
            .unwrap()
            .is_some_and(|p| p.exclude.contains(&key))
    })
}

/// A crash with execution behind (validator `dead` never runs): validator `y`'s execution is held
/// at 6 while its consensus commits past boundary L (the heights that attribute the dead
/// validator's misses; boundary L plans with them); it crashes there, its manifests pruned (the
/// headers are durable). Restarted, its boot replay executes those heights from the durable
/// headers and the leader evidence written in their batch, without a fail-stop, and its write
/// sets (leader statistics and the boundary's plan rows included) agree with the uncrashed
/// nodes' at every height.
#[test]
fn dl_cluster_crash_with_execution_behind_replays_the_same_rows() {
    let mut c = Cluster::new(&[1, 1, 1, 1]);
    let y = 1;
    let live = [0, 1, 2];
    let hold = ExecHold::new(6);
    c.start(0, &Run::on());
    c.start(2, &Run::on());
    c.start(y, &Run::on().held(&hold));
    c.wait_until(
        "y commits past boundary L while its execution is held",
        |c| c.committed(y) >= L + 3,
    );
    assert_eq!(c.applied(y), 5, "y executed up to the hold");
    let committed = c.committed(y);
    c.stop(y);
    c.start(y, &Run::on());
    assert!(
        c.applied(y) >= committed,
        "the boot replay ran the held heights: {}",
        c.status()
    );
    c.wait_applied(&live, committed);
    let top = c.min_applied(&live);
    c.stop_all();
    assert!(
        (6..=committed).all(|h| c.wrote_leader_rows(y, h)),
        "every replayed height wrote its leader rows (the cursor at least)"
    );
    c.assert_agree(&live, top);
}

/// A crash between the exclusion plan and its update (validator `dead` never runs): validator
/// `z` stops right after the plan is written, before the update commits (the chain stalls without
/// it), and restarts. The exclusion lands at the planned update and the write sets agree.
///
/// RED (s138, open): after the restart the update block's phases are interrupted by the dead
/// validator's slot right after its Precommit PC; the backup aggregator locks on it and refuses
/// the block's re-proposal (its justify is below the lock), and with three live validators of
/// four every vote is needed: the window livelocks at the update. The same wedge as
/// fix/vsu-window-flake f407d49c (`validator_set_update_survives_a_silent_leader_after_the_precommit_pc`);
/// the vote-rule fix it names (a proposal of the locked block itself satisfies the lock) makes
/// this test pass (the reflexive lock, on main since batch 12).
#[test]
fn dl_cluster_crash_between_the_exclusion_plan_and_its_update() {
    let mut c = Cluster::new(&[1, 1, 1, 1]);
    let (dead, z) = (3, 2);
    let live = [0, 1, 2];
    for i in live {
        c.start(i, &Run::on());
    }
    c.wait_until("the exclusion planned", |c| {
        planned_exclusion(c, 0, dead).is_some()
    });
    let planned = planned_exclusion(&c, 0, dead).unwrap();
    assert!(
        c.hs_committed(0) < planned,
        "scenario: z crashes before the update at {planned}: {}",
        c.status()
    );
    c.stop(z);
    std::thread::sleep(Duration::from_secs(1));
    c.start(z, &Run::on());
    let update = c.wait_exclusion(&live, dead, true, 0);
    assert_eq!(update, planned, "the planned update");
    c.wait_applied(&live, update + 2);
    let top = c.min_applied(&live);
    c.stop_all();
    c.assert_agree(&live, top);
}

/// Leader-evidence durability across a power loss: validator `y` (manifests never pruned) keeps
/// committing past boundary L while its execution is held at 6 (validator `dead` never runs, so
/// the committed heights attribute its misses and boundary L plans with them), then loses power:
/// every unexecuted height's state-DB rows (header, body, leader evidence) are gone, its
/// consensus DB keeps the commit manifests and hotstuff's delivered height (hotstuff never
/// re-delivers them). Restarted, it parks those heights on their manifests and, at the next
/// commit, executes them with the evidence the manifest envelopes carry (no fail-stop); its write
/// sets agree with the others' at every height.
#[test]
fn dl_cluster_power_loss_keeps_the_leader_evidence_in_the_manifests() {
    let mut c = Cluster::new(&[1, 1, 1, 1]);
    let y = 1;
    let live = [0, 1, 2];
    let hold = ExecHold::new(6);
    c.start(0, &Run::on());
    c.start(2, &Run::on());
    c.start(y, &Run::on().held(&hold).keep_manifests());
    // Stop before the chain needs y's plan for 2L (y's vote is needed: the dead validator is
    // the fourth), so its next vote is for a block it can validate and the commit it makes
    // drains the parked heights.
    c.wait_until(
        "y commits past boundary L while its execution is held",
        |c| c.committed(y) >= L + 3,
    );
    assert_eq!(c.applied(y), 5, "y executed up to the hold");
    let committed = c.committed(y);
    c.stop(y);
    // Power loss: the unsynced state-DB rows of every unexecuted height go; the consensus DB
    // (manifests) stays.
    let dir = c.nodes[y].dir.path().to_path_buf();
    {
        let state = StateDb::open(&dir.join("state")).unwrap();
        let consensus = StateDb::open_consensus(&dir.join("consensus")).unwrap();
        for h in 6..=committed {
            let manifest = load_commit_manifest(&consensus, h).expect("manifest kept");
            let (evidence, _) = crate::leader_skip::split_manifest(&manifest).unwrap();
            assert!(
                evidence.is_some(),
                "height {h}: the manifest carries the evidence"
            );
            let key = h.to_be_bytes();
            state.delete_cf_raw(CF_BLOCK_HEADERS, &key).unwrap();
            state.delete_cf_raw(CF_BLOCK_BODIES, &key).unwrap();
            state
                .delete_cf_raw(CF_CONSENSUS_META, &crate::leader_skip::leader_qc_key(h))
                .unwrap();
        }
    }
    c.start(y, &Run::on().keep_manifests());
    assert!(!c.failed(y), "boot fail-stopped: {}", c.status());
    assert_eq!(c.applied(y), 5, "the lost heights are parked, not replayed");
    c.wait_applied(&live, committed + 1);
    let top = c.min_applied(&live);
    c.stop_all();
    assert!(
        (6..=committed).all(|h| c.wrote_leader_rows(y, h)),
        "every parked height wrote its leader rows"
    );
    c.assert_agree(&live, top);
}

/// s138, found by the power-loss test (not dead-leader specific, run with K = 0): the same power
/// loss (validator 3 never runs), but `y`
/// stops only once the chain needs its plan for boundary 2L (committed 2L - 2, applied 5: the
/// plan source, boundary L, is among the lost heights). Restarted, `y` parks 6..=committed on
/// their manifests; they drain only when the next height commits, but that height is boundary 2L
/// (or needs its QC), which `y` cannot validate before it executed boundary L
/// (`EpochPlanNotReady` -> `MissingData`), and the dead validator is the fourth, so `y`'s vote is
/// needed: nothing ever commits again. RED on 16acaa48: stuck at committed 18 for 240 s.
#[test]
fn dl_cluster_power_loss_parked_plan_source_drains_for_the_boundary() {
    let mut c = Cluster::new(&[1, 1, 1, 1]);
    let y = 1;
    let live = [0, 1, 2];
    let hold = ExecHold::new(6);
    // Dead-leader skip off: no exclusion update to wait on at 2L, the bug is not dead-leader's.
    let off = Run::on().k(0);
    c.start(0, &off);
    c.start(2, &off);
    c.start(y, &off.clone().held(&hold).keep_manifests());
    c.wait_until("y commits up to the boundary that needs its plan", |c| {
        c.committed(y) >= 2 * L - 2
    });
    assert_eq!(c.applied(y), 5, "y executed up to the hold");
    let committed = c.committed(y);
    c.stop(y);
    let dir = c.nodes[y].dir.path().to_path_buf();
    {
        let state = StateDb::open(&dir.join("state")).unwrap();
        for h in 6..=committed {
            let key = h.to_be_bytes();
            state.delete_cf_raw(CF_BLOCK_HEADERS, &key).unwrap();
            state.delete_cf_raw(CF_BLOCK_BODIES, &key).unwrap();
            state
                .delete_cf_raw(CF_CONSENSUS_META, &crate::leader_skip::leader_qc_key(h))
                .unwrap();
        }
    }
    c.start(y, &off.clone().keep_manifests());
    c.wait_applied(&live, 2 * L + 1);
    let top = c.min_applied(&live);
    c.stop_all();
    c.assert_agree(&live, top);
}

/// Mixed configurations: validator 3 runs K = 0 (dead-leader skip off), the others K = 3.
/// All four live: consensus does not notice (every node commits the same headers), but the
/// states differ from height 1 on (validator 3 writes no leader statistics), so the running
/// hashes differ: a mixed config is caught by the state-hash attestation, never by consensus.
/// With validator 0 down as well, the K = 3 nodes put the exclusion delta in the update at the
/// planned boundary and validator 3 does not (another validator-set update for the same block):
/// no quorum votes either way, so the chain halts below that height, without a fork.
#[test]
fn dl_cluster_mixed_k_config_diverges_in_state_and_halts_at_the_exclusion() {
    let mut c = Cluster::new(&[1, 1, 1, 1]);
    let all = [0, 1, 2, 3];
    for i in 0..3 {
        c.start(i, &Run::on());
    }
    c.start(3, &Run::on().k(0));
    c.wait_applied(&all, 8);
    for h in 1..=8u64 {
        let row = c
            .db(0)
            .get_cf_raw(CF_BLOCK_HEADERS, &h.to_be_bytes())
            .unwrap();
        for i in 1..4 {
            assert_eq!(
                c.db(i)
                    .get_cf_raw(CF_BLOCK_HEADERS, &h.to_be_bytes())
                    .unwrap(),
                row,
                "header {h}: node {i}"
            );
        }
    }
    c.stop(0);
    let rest = [1, 2, 3];
    c.wait_until("the K = 3 nodes plan the exclusion of validator 0", |c| {
        planned_exclusion(c, 1, 0).is_some()
    });
    let planned = planned_exclusion(&c, 1, 0).unwrap();
    c.wait_until(
        "the chain reaches the height below the planned update",
        |c| rest.iter().all(|&i| c.committed(i) >= planned - 3),
    );
    std::thread::sleep(Duration::from_secs(8));
    for i in rest {
        assert!(
            c.committed(i) < planned,
            "node {i} committed the update height {planned}: {}",
            c.status()
        );
        assert!(!c.failed(i), "node {i} failed");
    }
    let top = rest.iter().map(|&i| c.committed(i)).min().unwrap();
    for h in 1..=top {
        let row = c
            .db(1)
            .get_cf_raw(CF_BLOCK_HEADERS, &h.to_be_bytes())
            .unwrap();
        for i in [2, 3] {
            assert_eq!(
                c.db(i)
                    .get_cf_raw(CF_BLOCK_HEADERS, &h.to_be_bytes())
                    .unwrap(),
                row,
                "no fork at {h}: node {i}"
            );
        }
    }
    c.stop_all();
    assert!(
        c.wrote_leader_rows(1, 1),
        "K = 3: the cursor row at height 1"
    );
    let top = c.nodes[3].writes.keys().max().copied().unwrap();
    for h in 1..=top {
        assert!(!c.wrote_leader_rows(3, h), "K = 0: no leader row at {h}");
    }
    assert_ne!(
        c.hashes(1).get(&1),
        c.hashes(3).get(&1),
        "the running hashes differ from height 1 (the K = 3 cursor row)"
    );
}
