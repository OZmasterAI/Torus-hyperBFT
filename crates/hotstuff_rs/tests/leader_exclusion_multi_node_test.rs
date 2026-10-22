//! Dead-leader skip phase 3 (docs/plans/dead-leader-skip.md §4 "Multi-node"), hotstuff_rs side:
//! an exclusion-only validator-set update (`ValidatorSetUpdates::exclude_leader`) of a validator
//! that is down, committed by a running cluster under the TC-required rules, and the validator's
//! re-admission once it is back. The Torus app emits these updates from its epoch plan
//! (torus-consensus `dead_leader_cluster_tests` runs that loop end to end); here a NumberApp
//! transaction carries them, so the update's proposer, and with it its view, is chosen.
//!
//! Short pacemaker epochs (10 views): epoch-change views fall before, inside and after the
//! update.

use std::{
    collections::BTreeSet,
    sync::{Arc, Mutex},
    time::Duration,
};

use hotstuff_rs::{
    hotstuff::messages::HotStuffMessage,
    networking::messages::{Message, ProgressMessage},
    pacemaker::{messages::PacemakerMessage, select_leader},
    types::{
        crypto_primitives::{SigningKey, VerifyingKey},
        data_types::{Power, ViewNumber},
        update_sets::ValidatorSetUpdates,
        validator_set::ValidatorSet,
    },
};

mod common;

use common::{
    network::{mock_network_with_filter, NetworkStub},
    node::Node,
    number_app::{NumberApp, NumberAppTransaction},
    poll::wait_until,
};

const POLL_INTERVAL: Duration = Duration::from_millis(250);
const MAX_VIEW_TIME: Duration = Duration::from_millis(1500);
const EPOCH_LENGTH: u32 = 10;
const BOUND: Duration = Duration::from_secs(240);

/// What the cluster sent: the views of every timeout vote, and `(sender, view)` of every proposal
/// header.
#[derive(Default)]
struct Log {
    timeout_views: BTreeSet<u64>,
    headers: BTreeSet<(usize, u64)>,
}

struct Cluster {
    keypairs: Vec<SigningKey>,
    vs_updates: ValidatorSetUpdates,
    stubs: Vec<NetworkStub>,
    nodes: Vec<Option<Node>>,
    log: Arc<Mutex<Log>>,
}

impl Cluster {
    /// One validator per power, keys `[1; 32]` .. `[n; 32]`; the network logs what is sent.
    fn new(powers: &[u64]) -> Cluster {
        let keypairs: Vec<SigningKey> = (1..=powers.len() as u8).map(|i| SigningKey::from_bytes(&[i; 32])).collect();
        let mut vs_updates = ValidatorSetUpdates::new();
        for (kp, p) in keypairs.iter().zip(powers) {
            vs_updates.insert(kp.verifying_key(), Power::new(*p));
        }
        let keys: Vec<VerifyingKey> = keypairs.iter().map(|kp| kp.verifying_key()).collect();
        let (stubs, filter) = mock_network_with_filter(keys.iter().copied());
        let log = Arc::new(Mutex::new(Log::default()));
        let log_in = log.clone();
        filter.rewrite(Arc::new(move |sender, _recipient, message| {
            let mut log = log_in.lock().unwrap();
            match &message {
                Message::ProgressMessage(ProgressMessage::PacemakerMessage(PacemakerMessage::TimeoutVote(tv))) => {
                    log.timeout_views.insert(tv.view.int());
                }
                Message::ProgressMessage(ProgressMessage::HotStuffMessage(HotStuffMessage::ProposalHeader(h))) => {
                    let i = keys.iter().position(|k| k == sender).unwrap();
                    log.headers.insert((i, h.view.int()));
                }
                _ => {}
            }
            Some(message)
        }));
        let nodes = (0..keypairs.len()).map(|_| None).collect();
        Cluster {
            keypairs,
            vs_updates,
            stubs,
            nodes,
            log,
        }
    }

    fn key(&self, i: usize) -> VerifyingKey {
        self.keypairs[i].verifying_key()
    }

    fn genesis_set(&self) -> ValidatorSet {
        let mut vs = ValidatorSet::new();
        vs.apply_updates(&self.vs_updates);
        vs
    }

    /// The index of `leader(view)` over `set`.
    fn leader(&self, view: u64, set: &ValidatorSet) -> usize {
        let leader = select_leader(ViewNumber::new(view), set);
        self.keypairs.iter().position(|kp| kp.verifying_key() == leader).unwrap()
    }

    /// Start validator `i` on a fresh store (it block-syncs what it missed).
    fn start(&mut self, i: usize) {
        let mut node = Node::new_with_epoch_length(
            self.keypairs[i].clone(),
            self.stubs[i].clone(),
            NumberApp::initial_app_state(),
            self.vs_updates.clone(),
            MAX_VIEW_TIME,
            EPOCH_LENGTH,
            None,
        );
        node.submit_transaction(NumberAppTransaction::Increment);
        self.nodes[i] = Some(node);
    }

    fn node(&mut self, i: usize) -> &mut Node {
        self.nodes[i].as_mut().expect("running")
    }

    fn live(&self) -> impl Iterator<Item = &Node> {
        self.nodes.iter().flatten()
    }

    fn max_committed(&self) -> u64 {
        self.live().map(|n| n.committed_height().unwrap_or(0)).max().unwrap_or(0)
    }

    fn max_view(&self) -> u64 {
        self.live().map(|n| n.highest_view_entered().int()).max().unwrap_or(0)
    }

    fn describe(&self) -> String {
        self.nodes
            .iter()
            .enumerate()
            .map(|(i, n)| match n {
                Some(n) => format!(
                    "n{i}{{committed={:?}, highest_pc_view={}, view={}, excluded={:?}}}",
                    n.committed_height(),
                    n.highest_pc_view(),
                    n.highest_view_entered(),
                    n.committed_validator_set().leader_excluded().count(),
                ),
                None => format!("n{i}{{down}}"),
            })
            .collect::<Vec<_>>()
            .join(", ")
    }

    fn wait_for_commits(&self, height: u64, context: &str) {
        wait_until(
            BOUND,
            POLL_INTERVAL,
            &format!("every live validator to commit height >= {height} ({context})"),
            || self.live().all(|n| n.committed_height().unwrap_or(0) >= height),
            || self.describe(),
        );
    }

    /// Wait until every live validator's committed set has validator `x` leader-excluded
    /// (`excluded`) or not.
    fn wait_for_exclusion(&self, x: usize, excluded: bool) {
        let key = self.key(x);
        wait_until(
            BOUND,
            POLL_INTERVAL,
            &format!("every live validator to commit n{x} leader-excluded = {excluded}"),
            || self.live().all(|n| n.committed_validator_set().is_leader_excluded(&key) == excluded),
            || self.describe(),
        );
    }

    fn assert_no_tc_less_proposals(&self) {
        let tc_less: Vec<u64> = self.live().map(|n| n.tc_less_proposals()).collect();
        assert!(
            tc_less.iter().all(|&count| count == 0),
            "proposals without QC(v-1) or TC(v-1) per live node: {tc_less:?}"
        );
    }
}

/// The validator that leads right after `x` in the genesis rotation of four equal validators: an
/// update it proposes in view v runs Prepare (v), Precommit (v + 1) and Commit (v + 2) before x's
/// slot (v + 3); the Commit PC needs no consecutive view. (Proposed two views before x's slot, the
/// Precommit PC's backup aggregator locks on it and refuses the block's re-proposal: the window
/// livelock of fix/vsu-window-flake f407d49c, open.)
fn proposer_after(c: &Cluster, x: usize) -> usize {
    let set = c.genesis_set();
    let v = (0..4u64).find(|v| c.leader(*v, &set) == x).unwrap();
    c.leader(v + 1, &set)
}

/// Run four equal validators with `x` down and commit its exclusion through `p`. Returns the
/// highest view entered when the exclusion was seen committed.
fn exclude_down_validator(c: &mut Cluster, x: usize) -> u64 {
    let p = proposer_after(c, x);
    for i in (0..4).filter(|&i| i != x) {
        c.start(i);
    }
    c.wait_for_commits(2, "before the update");
    let key = c.key(x).to_bytes();
    c.node(p).submit_transaction(NumberAppTransaction::ExcludeLeader(key));
    c.wait_for_exclusion(x, true);
    c.max_view()
}

/// An exclusion-only update of the down validator `x` commits (Prepare, Precommit, Commit,
/// Decide, through x's timeout views and the TC-required rules: no TC-less proposal), every live
/// validator applies it to its committed set (voting power unchanged), and from then on x leads
/// no view: views time out no more where x's slots were (the old schedule's), while the live
/// validators keep committing across pacemaker epoch changes.
#[test]
fn exclusion_of_a_down_validator_commits_and_its_slots_stop_timing_out() {
    let mut c = Cluster::new(&[1, 1, 1, 1]);
    let x = 3;
    let decided_view = exclude_down_validator(&mut c, x);
    for n in c.live() {
        let set = n.committed_validator_set();
        assert!(set.is_leader_excluded(&c.key(x)));
        assert_eq!(set.power(&c.key(x)).map(|p| p.int()), Some(1), "voting power kept");
    }
    let before = c.log.lock().unwrap().timeout_views.clone();
    let genesis = c.genesis_set();
    let x_slots_before: Vec<u64> = (1..decided_view).filter(|v| c.leader(*v, &genesis) == x).collect();
    assert!(
        x_slots_before.iter().filter(|v| before.contains(v)).count() * 2 >= x_slots_before.len(),
        "fixture: x's slots timed out before its exclusion: slots {x_slots_before:?}, timeouts {before:?}"
    );

    // Two pacemaker epochs after the update.
    let from = decided_view + 2;
    let after = c.max_committed();
    c.wait_for_commits(after + 2 * EPOCH_LENGTH as u64, "after the update");
    let to = c.max_view();
    let log = c.log.lock().unwrap();
    let x_slots: Vec<u64> = (from..to).filter(|v| c.leader(*v, &genesis) == x).collect();
    let lost: Vec<u64> = log.timeout_views.range(from..to).copied().collect();
    assert!(x_slots.len() >= 4, "fixture: the span holds x's old slots: {x_slots:?}");
    assert!(
        lost.len() * 4 < x_slots.len(),
        "views timing out after the exclusion: {lost:?} (x's old slots {x_slots:?})"
    );
    drop(log);
    c.assert_no_tc_less_proposals();
}

/// The excluded validator comes back (fresh store: it block-syncs, including the exclusion) and
/// an update re-admits it: it leads again (sends proposal headers for views it leads) and the
/// cluster keeps committing.
#[test]
fn readmitted_validator_proposes_again() {
    let mut c = Cluster::new(&[1, 1, 1, 1]);
    let x = 3;
    let p = proposer_after(&c, x);
    exclude_down_validator(&mut c, x);
    c.start(x);
    c.wait_for_exclusion(x, true);
    let key = c.key(x).to_bytes();
    c.node(p).submit_transaction(NumberAppTransaction::ReadmitLeader(key));
    c.wait_for_exclusion(x, false);
    let readmitted_view = c.max_view();
    let after = c.max_committed();
    c.wait_for_commits(after + 4, "after the re-admission");
    wait_until(
        BOUND,
        POLL_INTERVAL,
        "the re-admitted validator to propose",
        || c.log.lock().unwrap().headers.iter().any(|(i, view)| *i == x && *view > readmitted_view),
        || c.describe(),
    );
    c.assert_no_tc_less_proposals();
}

/// 2:2:1 with the stake-1 validator down (the bench shape, s84 `one_down_liveness_test`): `update`
/// is submitted to validator 0 after height 3, and must commit.
fn update_at_2_2_1_with_the_stake_one_validator_down(update: impl FnOnce(&Cluster) -> NumberAppTransaction, applied: impl Fn(&Cluster, &ValidatorSet) -> bool) {
    let mut c = Cluster::new(&[2_000_000, 2_000_000, 1_000_000]);
    c.start(0);
    c.start(1);
    c.wait_for_commits(3, "before the update");
    let tx = update(&c);
    c.node(0).submit_transaction(tx);
    wait_until(
        Duration::from_secs(150),
        POLL_INTERVAL,
        "both live validators to commit the update",
        || c.live().all(|n| applied(&c, &n.committed_validator_set())),
        || c.describe(),
    );
    let after = c.max_committed();
    c.wait_for_commits(after + 3, "after the update");
}

/// RED (s138, open): at 2:2:1 with the stake-1 validator C down no validator-set update commits,
/// an exclusion of C included, so b1 halts the bench shape at its first exclusion. With IWRR
/// [A, B, C] C leads every third view, but an update needs Prepare, Precommit and Commit in three
/// consecutive views (`safe_nudge` 4, `repropose_block`): Prepare (v) and Precommit (v + 1), C's
/// view v + 2 times out, the block is re-proposed, and so on. On top, both live validators lock on
/// the Precommit PC and refuse the re-proposal (the f407d49c vote rule): stuck at height 3. With the
/// vote-rule prototype (a proposal of the locked block satisfies the lock) it livelocks instead:
/// Prepare, Precommit, timeout, re-proposal, without end (s138, views 10..105 in 150 s).
#[test]
#[ignore = "RED: no validator-set update commits at 2:2:1 with the stake-1 validator down (docs/plans/dead-leader-skip.md §8)"]
fn exclusion_update_commits_at_2_2_1_with_the_stake_one_validator_down() {
    update_at_2_2_1_with_the_stake_one_validator_down(
        |c| NumberAppTransaction::ExcludeLeader(c.key(2).to_bytes()),
        |c, set| set.is_leader_excluded(&c.key(2)),
    );
}

/// The same with a power update (the pre-existing case: any validator-set update).
#[test]
#[ignore = "RED: no validator-set update commits at 2:2:1 with the stake-1 validator down (docs/plans/dead-leader-skip.md §8)"]
fn power_update_commits_at_2_2_1_with_the_stake_one_validator_down() {
    update_at_2_2_1_with_the_stake_one_validator_down(
        |c| NumberAppTransaction::SetValidator(c.key(0).to_bytes(), Power::new(2_000_001)),
        |c, set| set.power(&c.key(0)).map(|p| p.int()) == Some(2_000_001),
    );
}
