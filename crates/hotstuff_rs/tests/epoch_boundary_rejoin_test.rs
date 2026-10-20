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
//! Certificates and commits must resume (never `RECOVERY_BOUND` without one), every validator
//! must commit again and end past E.
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
    poll::{wait_for_scenario, Scenario},
};

const POLL_INTERVAL: Duration = Duration::from_millis(250);

const MAX_VIEW_TIME: Duration = Duration::from_millis(1500);

const EPOCH_LENGTH: u32 = 10;

/// The second epoch-change view, so the chain has committed blocks before it.
const EPOCH_CHANGE_VIEW: u64 = 2 * EPOCH_LENGTH as u64;

/// Blocks every validator must commit after the restart.
const COMMITS_AFTER_RESTART: u64 = 2;

/// How long the cluster may go without a new certificate or commit after the restart. The s85
/// wedge forms neither (the replicas still in E cannot certify it; the restarted one runs alone on
/// local timeouts), so it fails here as it did under the old fixed 90 s bound.
const RECOVERY_BOUND: Duration = Duration::from_secs(90);

/// Total time recovery may take while certificates keep forming (a loaded host).
const RECOVERY_CEILING: Duration = Duration::from_secs(240);

/// Setup waits (running the cluster up to the state a case needs) fail when the cluster stops
/// moving, not at a fixed deadline: idle they take ~20 s, on a loaded host (load ~270 on 18 cores)
/// past 120 s while still progressing. `SETUP_STALL` bounds a view at deep backoff with margin.
const SETUP_STALL: Duration = Duration::from_secs(180);
const SETUP_CEILING: Duration = Duration::from_secs(300);

/// Fresh clusters a case may start when a run misses its scenario (see case 2).
/// At 4x CPU oversubscription about half the runs miss case 2's scenario.
const SETUP_ATTEMPTS: usize = 5;

/// Grows whenever any validator enters a view, certifies or commits.
fn progress(nodes: &[Node]) -> u64 {
    nodes
        .iter()
        .map(|n| n.highest_view_entered().int())
        .sum::<u64>()
        + certified(nodes)
}

/// Grows whenever any validator certifies or commits (not on bare view changes, which the s85
/// wedge keeps making).
fn certified(nodes: &[Node]) -> u64 {
    nodes
        .iter()
        .map(|n| n.highest_pc_view() + n.committed_height().unwrap_or(0))
        .sum()
}

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

    /// Run the cluster until `observe` reports the scenario reached (`true`) or missed (`false`);
    /// panics if it stops progressing (see [`SETUP_STALL`]).
    fn wait_for_setup(&self, context: &str, mut observe: impl FnMut(&Self) -> Scenario) -> bool {
        wait_for_scenario(
            SETUP_CEILING,
            SETUP_STALL,
            POLL_INTERVAL,
            context,
            || observe(self),
            || progress(&self.nodes),
            || describe(&self.nodes),
        )
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
        let recovered = wait_for_scenario(
            RECOVERY_CEILING,
            RECOVERY_BOUND,
            POLL_INTERVAL,
            &format!(
                "every validator to leave view {EPOCH_CHANGE_VIEW} and commit height >= {target} \
                 (committed {committed_before} at the restart of n{c})"
            ),
            || {
                let done = self.nodes.iter().all(|n| {
                    n.highest_view_entered() > e && n.committed_height().unwrap_or(0) >= target
                });
                match done {
                    true => Scenario::Reached,
                    false => Scenario::Pending,
                }
            },
            || certified(&self.nodes),
            || describe(&self.nodes),
        );
        assert!(recovered);
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

    // Every validator must enter E (an epoch-change view is never skipped) and none can leave it
    // (C is silent), so this cannot be missed.
    assert!(cluster.wait_for_setup(
        &format!("all three validators to enter the epoch-change view {EPOCH_CHANGE_VIEW}"),
        |cl| match cl.nodes.iter().all(|n| n.highest_view_entered() == e) {
            true => Scenario::Reached,
            false => Scenario::Pending,
        },
    ));
    cluster.restart_and_expect_recovery(c);
}

/// Case 2, the vote left: C (not a collector of E's votes) votes for E and its vote reaches the
/// collector, so the others certify E and enter E + 1; from that vote on nothing reaches C, so it
/// stays in E without QC(E). Before the fix C restarted at E + 1, in the others' epoch; with the
/// fix it re-enters E and must catch up while the others wait in E + 1 for its vote.
///
/// The scenario needs C's phase vote for E to certify E on the others. On a loaded host E can
/// time out first and the three leave it on TC(E); the run is then past the state for good (seen:
/// every node at view 94 when the old 120 s setup wait expired; at 7x CPU oversubscription, every
/// node at view 26 with PC 18). Such a run is discarded and a fresh cluster started,
/// up to `SETUP_ATTEMPTS` times; the recovery assertion only ever runs on a run that hit the case.
#[test]
fn restart_after_the_vote_for_an_epoch_change_view_left() {
    for attempt in 1..=SETUP_ATTEMPTS {
        let mut cluster = Cluster::new();
        // leader(E + 2) is the backup collector of E's votes: its own vote to itself is dropped by
        // the isolation, so only the primary collector leader(E + 1) certifies E. Not leader(E):
        // the others may still need to fetch E's body from its proposer.
        let c = cluster.leader(EPOCH_CHANGE_VIEW + 2);
        assert_ne!(c, cluster.leader(EPOCH_CHANGE_VIEW + 1));
        assert_ne!(c, cluster.leader(EPOCH_CHANGE_VIEW));
        let e = ViewNumber::new(EPOCH_CHANGE_VIEW);
        cluster
            .filter
            .isolate_after_vote(cluster.keypairs[c].verifying_key(), e);
        cluster.start();

        let context =
            format!("n{c}'s vote for {EPOCH_CHANGE_VIEW} to certify it on the others only");
        let reached = cluster.wait_for_setup(&context, |cl| {
            let certified_on_others_only = cl.nodes.iter().enumerate().all(|(i, n)| {
                if i == c {
                    n.highest_view_entered() == e && n.highest_pc_view() < e.int()
                } else {
                    n.highest_view_entered() > e && n.highest_pc_view() >= e.int()
                }
            });
            if certified_on_others_only {
                return Scenario::Reached;
            }
            // Views never go back, so the run is past the scenario for good when
            // - C left E: whether it never voted for E (E ended first), or it voted but a
            //   TC(E) already in its inbox took it out; or
            // - C is isolated in E but the others left E without QC(E) (on TC(E): E timed out
            //   before C's vote reached the collector), so nobody can ever certify E.
            //   Each node's view is read before its PC (the PC is written before the view it
            //   opens, e.g. `on_receive_advance_view`), so a node entering E + 1 on QC(E) is
            //   not taken for this; were it, the cost is one fresh attempt, not a pass.
            let c_left_e = cl.nodes[c].highest_view_entered() > e;
            let others_left_e_without_qc = cl.filter.is_isolated()
                && cl
                    .nodes
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| *i != c)
                    .any(|(_, n)| n.highest_view_entered() > e && n.highest_pc_view() < e.int());
            if c_left_e || others_left_e_without_qc {
                Scenario::Missed
            } else {
                Scenario::Pending
            }
        });
        if !reached {
            eprintln!(
                "attempt {attempt}/{SETUP_ATTEMPTS}: view {EPOCH_CHANGE_VIEW} ended without n{c}'s \
                 vote certifying it (host load?); starting a fresh cluster. State: {}",
                describe(&cluster.nodes)
            );
            continue;
        }
        assert!(
            cluster.filter.is_isolated(),
            "n{c} sent its vote for {EPOCH_CHANGE_VIEW}"
        );
        cluster.restart_and_expect_recovery(c);
        return;
    }
    panic!(
        "{SETUP_ATTEMPTS} runs in a row left view {EPOCH_CHANGE_VIEW} without the isolated node's \
         vote certifying it: the scenario was never set up"
    );
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

    assert!(cluster.wait_for_setup(
        &format!("n{c} alone to certify {EPOCH_CHANGE_VIEW} while the others stay in it"),
        |cl| {
            let alone = cl.nodes.iter().enumerate().all(|(i, n)| {
                if i == c {
                    n.highest_pc_view() >= e.int() && n.highest_view_entered() > e
                } else {
                    n.highest_view_entered() == e && n.highest_pc_view() < e.int()
                }
            });
            match alone {
                true => Scenario::Reached,
                false => Scenario::Pending,
            }
        },
    ));
    cluster.restart_and_expect_recovery(c);
}
