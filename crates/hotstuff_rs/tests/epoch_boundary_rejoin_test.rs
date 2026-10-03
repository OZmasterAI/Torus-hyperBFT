//! s85 epoch-boundary rejoin: a validator restarted around an epoch-change view must not wedge
//! the chain.
//!
//! # Setup
//!
//! Three validators at EQUAL stake: `ValidatorSet::quorum` = `floor(2 * total / 3) + 1` is then all
//! three, so every QC and every TC needs every validator. A view `v` with
//! `v % epoch_length == 0` is an epoch-change view: a replica leaves it only with a QC or a TC for
//! it (on timeout it broadcasts a TimeoutVote and extends the view). Phase votes for view `w` go to
//! `leader(w + 1)` and, as a backup, to `leader(w + 2)`.
//!
//! # The bug (s85 drill, view 12400)
//!
//! val2 entered the epoch-change view E and was stopped before its vote left. It restarted at
//! `highest_view_with_progress + 1` = E + 1, in the next epoch, with no certificate for E. val0 and
//! val1 stayed in E: a QC(E) needs val2's phase vote, but val2 only votes in its current view; a
//! TC(E) needs val2's TimeoutVote(E), but val2 drops pacemaker messages below its view; and
//! round-skip never crosses an epoch. val2 ran alone through its epoch on local timeouts (with the
//! view-timeout backoff, about 19 min here), so the chain stopped committing. The fix boots it
//! back into E when it holds no QC or TC for E (`pacemaker::boot_view`).
//!
//! # The cases
//!
//! Each case stops one validator C around E and restarts it on its own store. C keeps its inbox
//! (`NetworkStub` clone): whatever the filter let through while it was down is still delivered.
//! Commits must resume within a bound, and every validator must end past E.
//! 1. C's vote for E never leaves (the drill). With the fix C re-enters E.
//! 2. C's vote for E leaves and the others certify E, but C never learns QC(E). With the fix C
//!    re-enters E while the others are in E + 1 and wait for its vote there.
//! 3. C collects QC(E) itself (it is leader(E + 1)) and stops before the others hear of it. It
//!    holds a certificate, so it boots past E with or without the fix.

use std::time::Duration;

use hotstuff_rs::{
    pacemaker::select_leader,
    types::{
        crypto_primitives::SigningKey,
        data_types::{Power, ViewNumber},
        update_sets::{AppStateUpdates, ValidatorSetUpdates},
        validator_set::ValidatorSet,
    },
};

mod common;

use common::{
    network::{mock_network_with_filter, FilterHandle, NetworkStub},
    node::Node,
    number_app::{NumberApp, NumberAppTransaction},
    poll::wait_until,
};

const POLL_INTERVAL: Duration = Duration::from_millis(250);

const MAX_VIEW_TIME: Duration = Duration::from_millis(1500);

const EPOCH_LENGTH: u32 = 10;

/// The second epoch-change view, so the chain has committed blocks before it.
const EPOCH_CHANGE_VIEW: u64 = 2 * EPOCH_LENGTH as u64;

/// Blocks every validator must commit after the restart.
const COMMITS_AFTER_RESTART: u64 = 2;

/// How long commits may take to resume after the restart.
const RECOVERY_BOUND: Duration = Duration::from_secs(90);

/// Three equal-stake validators on a filtered mock network.
struct Cluster {
    keypairs: Vec<SigningKey>,
    vs_updates: ValidatorSetUpdates,
    init_as: AppStateUpdates,
    filter: FilterHandle,
    stubs: Vec<NetworkStub>,
    nodes: Vec<Node>,
}

impl Cluster {
    fn new() -> Cluster {
        let keypairs: Vec<SigningKey> =
            (1..=3u8).map(|i| SigningKey::from_bytes(&[i; 32])).collect();
        let mut vs_updates = ValidatorSetUpdates::new();
        for kp in &keypairs {
            vs_updates.insert(kp.verifying_key(), Power::new(1_000_000));
        }
        let (stubs, filter) =
            mock_network_with_filter(keypairs.iter().map(|kp| kp.verifying_key()));
        Cluster {
            keypairs,
            vs_updates,
            init_as: NumberApp::initial_app_state(),
            filter,
            stubs,
            nodes: Vec::new(),
        }
    }

    /// The index of `leader(view)`.
    fn leader(&self, view: u64) -> usize {
        let mut vs = ValidatorSet::new();
        vs.apply_updates(&self.vs_updates);
        let leader = select_leader(ViewNumber::new(view), &vs);
        self.keypairs
            .iter()
            .position(|kp| kp.verifying_key() == leader)
            .unwrap()
    }

    fn node(&self, i: usize, kv_store: Option<common::mem_db::MemDB>) -> Node {
        Node::new_with_epoch_length(
            self.keypairs[i].clone(),
            self.stubs[i].clone(),
            self.init_as.clone(),
            self.vs_updates.clone(),
            MAX_VIEW_TIME,
            EPOCH_LENGTH,
            kv_store,
        )
    }

    /// Start all three (arm the filter first).
    fn start(&mut self) {
        self.nodes = (0..3).map(|i| self.node(i, None)).collect();
        for node in self.nodes.iter_mut() {
            node.submit_transaction(NumberAppTransaction::Increment);
        }
    }

    fn wait(&self, timeout: Duration, context: &str, condition: impl FnMut(&[Node]) -> bool) {
        let mut condition = condition;
        wait_until(
            timeout,
            POLL_INTERVAL,
            context,
            || condition(&self.nodes),
            || describe(&self.nodes),
        );
    }

    /// Stop node `c` (dropping it shuts its replica down), heal the network, restart `c` on its
    /// own store, and wait for every validator to leave E and commit again.
    fn restart_and_expect_recovery(&mut self, c: usize) {
        let committed_before = self
            .nodes
            .iter()
            .map(|n| n.committed_height().unwrap_or(0))
            .max()
            .unwrap();
        let store = self.nodes[c].kv_store();
        drop(self.nodes.remove(c));
        self.filter.heal();
        let restarted = self.node(c, Some(store));
        self.nodes.insert(c, restarted);

        let e = ViewNumber::new(EPOCH_CHANGE_VIEW);
        let target = committed_before + COMMITS_AFTER_RESTART;
        self.wait(
            RECOVERY_BOUND,
            &format!(
                "every validator to leave view {EPOCH_CHANGE_VIEW} and commit height >= {target} \
                 (committed {committed_before} at the restart of n{c})"
            ),
            |nodes| {
                nodes.iter().all(|n| {
                    n.highest_view_entered() > e && n.committed_height().unwrap_or(0) >= target
                })
            },
        );
    }
}

fn describe(nodes: &[Node]) -> String {
    nodes
        .iter()
        .enumerate()
        .map(|(i, n)| {
            format!(
                "n{i}{{committed={:?}, highest_pc_view={}, view={}}}",
                n.committed_height(),
                n.highest_pc_view(),
                n.highest_view_entered(),
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Case 1, the drill: everything C sends from E on is dropped, so C enters E but its vote never
/// leaves. Nobody can leave E. Before the fix C restarted at E + 1 and the others stayed in E.
#[test]
fn restart_inside_an_epoch_change_view_does_not_wedge_equal_stake_validators() {
    let mut cluster = Cluster::new();
    let c = 2;
    let e = ViewNumber::new(EPOCH_CHANGE_VIEW);
    cluster
        .filter
        .silence_from_view(cluster.keypairs[c].verifying_key(), e);
    cluster.start();

    cluster.wait(
        Duration::from_secs(120),
        &format!("all three validators to enter the epoch-change view {EPOCH_CHANGE_VIEW}"),
        |nodes| nodes.iter().all(|n| n.highest_view_entered() == e),
    );
    cluster.restart_and_expect_recovery(c);
}

/// Case 2, the vote left: C (not a collector of E's votes) votes for E and its vote reaches the
/// collector, so the others certify E and enter E + 1; from that vote on nothing reaches C, so it
/// stays in E without QC(E). Before the fix C restarted at E + 1, in the others' epoch; with the
/// fix it re-enters E and must catch up while the others wait in E + 1 for its vote.
#[test]
fn restart_after_the_vote_for_an_epoch_change_view_left() {
    let mut cluster = Cluster::new();
    // leader(E + 2) is the backup collector of E's votes: its own vote to itself is dropped by the
    // isolation, so only the primary collector leader(E + 1) certifies E. Not leader(E): the others
    // may still need to fetch E's body from its proposer.
    let c = cluster.leader(EPOCH_CHANGE_VIEW + 2);
    assert_ne!(c, cluster.leader(EPOCH_CHANGE_VIEW + 1));
    assert_ne!(c, cluster.leader(EPOCH_CHANGE_VIEW));
    let e = ViewNumber::new(EPOCH_CHANGE_VIEW);
    cluster
        .filter
        .isolate_after_vote(cluster.keypairs[c].verifying_key(), e);
    cluster.start();

    cluster.wait(
        Duration::from_secs(120),
        &format!("n{c}'s vote for {EPOCH_CHANGE_VIEW} to certify it on the others only"),
        |nodes| {
            nodes.iter().enumerate().all(|(i, n)| {
                if i == c {
                    n.highest_view_entered() == e && n.highest_pc_view() < e.int()
                } else {
                    n.highest_view_entered() > e && n.highest_pc_view() >= e.int()
                }
            })
        },
    );
    assert!(cluster.filter.is_isolated(), "n{c} sent its vote for {EPOCH_CHANGE_VIEW}");
    cluster.restart_and_expect_recovery(c);
}

/// Case 3, C collected QC(E): C is leader(E + 1), and what it sends to others that could carry a
/// certificate for E or later is dropped (its block-data traffic and loopback are not), so it
/// certifies E alone, persists QC(E) and enters E + 1 while the others stay in E. It boots past E (it holds a certificate). The others must learn QC(E)
/// or form TC(E).
#[test]
fn restart_after_collecting_the_qc_for_an_epoch_change_view_alone() {
    let mut cluster = Cluster::new();
    let c = cluster.leader(EPOCH_CHANGE_VIEW + 1);
    let e = ViewNumber::new(EPOCH_CHANGE_VIEW);
    cluster
        .filter
        .certify_alone(cluster.keypairs[c].verifying_key(), e);
    cluster.start();

    cluster.wait(
        Duration::from_secs(120),
        &format!("n{c} alone to certify {EPOCH_CHANGE_VIEW} while the others stay in it"),
        |nodes| {
            nodes.iter().enumerate().all(|(i, n)| {
                if i == c {
                    n.highest_pc_view() >= e.int() && n.highest_view_entered() > e
                } else {
                    n.highest_view_entered() == e && n.highest_pc_view() < e.int()
                }
            })
        },
    );
    cluster.restart_and_expect_recovery(c);
}
