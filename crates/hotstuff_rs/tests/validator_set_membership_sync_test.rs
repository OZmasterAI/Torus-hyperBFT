//! s138 validator-set membership sync (docs/plans/disjoint-vset-livelock.md §3.3 D2, §6 tests).
//!
//! An update U changes `S_old` to `S_new`. A replica checks every PC against the validator set its
//! own committed state names. A replica that has not committed U (a laggard, a late joiner, a node
//! syncing from genesis) checks U's Decide PC, and every PC after it, against `S_old`. When the
//! member list changed (or the Decide signers lack an `S_old` quorum under the old powers) that
//! check fails, block sync blacklists the server that sent U's child, and the replica never
//! crosses U.
//!
//! Each test runs a mock-network cluster (`NumberApp`, 2 s max view time), applies one update while
//! some replica is cut off, then reconnects it and waits for it to cross the update.
//!
//! | test | scenario | d3de0abb |
//! |---|---|---|
//! | T1 | add 1 to 4; every Decide vote of the first window view dropped | GREEN (D1 fixed) |
//! | T3 | add 1 to 4; a continuing member deaf from U's Precommit-PC nudge on (holds U, locked) | RED (D2) |
//! | T4 | add 1 to 4; the joiner cut off through the window | RED (D2) |
//! | T5 | add 1 to 4; a fresh node started from genesis after the decide | RED (D2) |
//! | T6a | power 1 -> 2 for one of 4; a member deaf from U's Precommit-PC nudge on | GREEN |
//! | T6b | power 1 -> 10 for one of 4; same; the Decide PC holds no old-set quorum | RED (ignored: needs U's Commit PC) |
//!
//! A laggard that is an old-set member stays in the window until it has voted Commit (its votes
//! still leave once it is deaf). A dead old-set member in the window can livelock the window on
//! d3de0abb (seen in 3 of 9 runs of an earlier T6a: Precommit PC formed, no Commit votes, U
//! re-proposed without enough votes), a separate issue these tests keep out of the way.
//! | T10 | remove 1 of 4, then add 1 to 3 with the joiner cut off through the window | GREEN |

use std::{
    collections::HashSet,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

use ed25519_dalek::VerifyingKey;
use rand_core::OsRng;

use hotstuff_rs::{
    hotstuff::{messages::HotStuffMessage, types::Phase},
    networking::messages::{Message, ProgressMessage},
    types::{
        crypto_primitives::SigningKey,
        data_types::{Power, ViewNumber},
        update_sets::ValidatorSetUpdates,
    },
};

mod common;

use common::{
    logging::log_with_context,
    network::{mock_network_with_filter, FilterHandle, NetworkStub, Rewrite},
    node::Node,
    number_app::{NumberApp, NumberAppTransaction},
    poll::wait_until,
};

const POLL_INTERVAL: Duration = Duration::from_millis(250);

/// Short views, so the views a cut-off leader owns time out quickly.
const MAX_VIEW_TIME: Duration = Duration::from_millis(2000);

/// Bound on the cluster applying (committing and deciding) an update without the cut-off replica.
const UPDATE_TIMEOUT: Duration = Duration::from_secs(120);

/// Bound on a reconnected replica crossing the update and catching up. Block sync fires within
/// one advertisement period (10 s) plus its response timeout; this leaves room for a loaded host.
const CATCH_UP_TIMEOUT: Duration = Duration::from_secs(150);

/// Blocks the new set must commit after the decide before the cut-off replica is reconnected.
const PROGRESS_BLOCKS: u64 = 3;

/// A mock-network cluster whose replicas can be started late and cut off.
struct Cluster {
    keys: Vec<SigningKey>,
    stubs: Vec<Option<NetworkStub>>,
    nodes: Vec<Option<Node>>,
    filter: FilterHandle,
    init_vs: ValidatorSetUpdates,
}

impl Cluster {
    /// `n` replicas on one mock network. Genesis validators: `genesis` (index, power). No replica
    /// is started.
    fn new(n: usize, genesis: &[(usize, u64)]) -> Self {
        let mut csprg = OsRng {};
        let keys: Vec<SigningKey> = (0..n).map(|_| SigningKey::generate(&mut csprg)).collect();
        let (stubs, filter) = mock_network_with_filter(keys.iter().map(|k| k.verifying_key()));
        let mut init_vs = ValidatorSetUpdates::new();
        for &(i, power) in genesis {
            init_vs.insert(keys[i].verifying_key(), Power::new(power));
        }
        Self {
            keys,
            stubs: stubs.into_iter().map(Some).collect(),
            nodes: (0..n).map(|_| None).collect(),
            filter,
            init_vs,
        }
    }

    /// Start replica `i` from genesis.
    fn start(&mut self, i: usize) {
        let stub = self.stubs[i].take().expect("each replica starts once");
        self.nodes[i] = Some(Node::new_with_max_view_time(
            self.keys[i].clone(),
            stub,
            NumberApp::initial_app_state(),
            self.init_vs.clone(),
            MAX_VIEW_TIME,
        ));
    }

    fn vk(&self, i: usize) -> VerifyingKey {
        self.keys[i].verifying_key()
    }

    fn node(&self, i: usize) -> &Node {
        self.nodes[i].as_ref().expect("replica started")
    }

    fn node_mut(&mut self, i: usize) -> &mut Node {
        self.nodes[i].as_mut().expect("replica started")
    }

    fn set_validator(&mut self, via: usize, target: usize, power: u64) {
        let target = self.vk(target).to_bytes();
        self.node_mut(via)
            .submit_transaction(NumberAppTransaction::SetValidator(
                target,
                Power::new(power),
            ));
    }

    fn delete_validator(&mut self, via: usize, target: usize) {
        let target = self.vk(target).to_bytes();
        self.node_mut(via)
            .submit_transaction(NumberAppTransaction::DeleteValidator(target));
    }

    fn committed_height(&self, i: usize) -> u64 {
        self.node(i).committed_height().unwrap_or(0)
    }

    /// One line per started replica: committed set (size, power of `watch`), decided, committed
    /// height, highest view entered.
    fn describe(&self, watch: usize) -> String {
        let watch = self.vk(watch);
        self.nodes
            .iter()
            .enumerate()
            .filter_map(|(i, node)| node.as_ref().map(|node| (i, node)))
            .map(|(i, node)| {
                let set = node.committed_validator_set();
                format!(
                    "n{i}: set={} p(watch)={:?} decided={} h={:?} view={}",
                    set.len(),
                    set.power(&watch).map(|p| p.int()),
                    node.validator_set_update_decided(),
                    node.committed_height(),
                    node.highest_view_entered().int(),
                )
            })
            .collect::<Vec<_>>()
            .join("; ")
    }

    /// Whether replica `i` has committed a set of `len` validators in which `watch` has `power`
    /// (`None`: `watch` is not a member).
    fn has_set(&self, i: usize, len: usize, watch: usize, power: Option<u64>) -> bool {
        let set = self.node(i).committed_validator_set();
        set.len() == len && set.power(&self.vk(watch)).map(|p| p.int()) == power
    }

    /// Wait until every replica in `live` committed and decided the update (a set of `len`
    /// with `watch` at `power`), then until each of them committed `PROGRESS_BLOCKS` more blocks.
    /// Returns the highest committed height among `live` at that point.
    fn wait_applied(&self, live: &[usize], len: usize, watch: usize, power: Option<u64>) -> u64 {
        wait_until(
            UPDATE_TIMEOUT,
            POLL_INTERVAL,
            "the live replicas to commit and decide the update",
            || {
                live.iter().all(|&i| {
                    self.has_set(i, len, watch, power)
                        && self.node(i).validator_set_update_decided()
                })
            },
            || self.describe(watch),
        );
        let decided_at = live
            .iter()
            .map(|&i| self.committed_height(i))
            .max()
            .unwrap();
        wait_until(
            UPDATE_TIMEOUT,
            POLL_INTERVAL,
            "the new validator set to commit blocks after the decide",
            || {
                live.iter()
                    .all(|&i| self.committed_height(i) >= decided_at + PROGRESS_BLOCKS)
            },
            || self.describe(watch),
        );
        live.iter()
            .map(|&i| self.committed_height(i))
            .max()
            .unwrap()
    }

    /// Wait until replica `lagger` committed the update (a set of `len` with `watch` at `power`)
    /// and reached committed height `target`.
    fn wait_caught_up(
        &self,
        lagger: usize,
        len: usize,
        watch: usize,
        power: Option<u64>,
        target: u64,
    ) {
        wait_until(
            CATCH_UP_TIMEOUT,
            POLL_INTERVAL,
            &format!("n{lagger} to cross the update and reach committed height {target}"),
            || self.has_set(lagger, len, watch, power) && self.committed_height(lagger) >= target,
            || self.describe(watch),
        );
    }

    /// Wait until every replica in `nodes` committed at least `height`.
    fn wait_height(&self, nodes: &[usize], height: u64) {
        wait_until(
            UPDATE_TIMEOUT,
            POLL_INTERVAL,
            &format!("replicas {nodes:?} to reach committed height {height}"),
            || nodes.iter().all(|&i| self.committed_height(i) >= height),
            || self.describe(nodes[0]),
        );
    }
}

/// The phase vote `message` carries, if any.
fn phase_vote(message: &Message) -> Option<(ViewNumber, Phase)> {
    match message {
        Message::ProgressMessage(ProgressMessage::HotStuffMessage(HotStuffMessage::PhaseVote(
            vote,
        ))) => Some((vote.view, vote.phase)),
        _ => None,
    }
}

/// Drop everything sent to or by a node of `cut`, except what a node sends itself.
fn cut_off(cut: Vec<VerifyingKey>) -> Rewrite {
    let cut: HashSet<VerifyingKey> = cut.into_iter().collect();
    Arc::new(move |sender, recipient, message| {
        if sender != recipient && (cut.contains(sender) || cut.contains(recipient)) {
            None
        } else {
            Some(message)
        }
    })
}

/// Whether `message` is a Nudge carrying a Precommit PC (the nudge that makes replicas lock on U
/// and vote Commit).
fn is_precommit_nudge(message: &Message) -> bool {
    matches!(
        message,
        Message::ProgressMessage(ProgressMessage::HotStuffMessage(HotStuffMessage::Nudge(nudge)))
            if nudge.justify.phase == Phase::Precommit
    )
}

/// Deliver nothing to `laggard` from the moment U's Precommit-PC nudge reaches it (that nudge
/// included): it holds U, locks on U, votes Commit (what it sends still leaves), and never learns
/// U's Commit PC. Sets `deaf` when the nudge passes. Also drops what `drop` names.
fn deaf_after_precommit_nudge(
    laggard: VerifyingKey,
    deaf: Arc<AtomicBool>,
    drop: impl Fn(&VerifyingKey, &Message) -> bool + Send + Sync + 'static,
) -> Rewrite {
    Arc::new(move |sender, recipient, message| {
        if drop(sender, &message) {
            return None;
        }
        if *recipient == laggard {
            if deaf.load(Ordering::Relaxed) {
                return None;
            }
            if is_precommit_nudge(&message) {
                deaf.store(true, Ordering::Relaxed);
            }
        }
        Some(message)
    })
}

/// T1 `vsu_window_retries_after_a_lost_decide_view`: every Decide vote of the first view in which
/// one is sent is dropped, so the first window view yields no Decide PC. d3de0abb retries the window
/// in the next view with a TC (L2 HighQcWins nudges again); main stalled for good (D1).
#[test]
fn vsu_window_retries_after_a_lost_decide_view() {
    let mut cluster = Cluster::new(5, &[(0, 1), (1, 1), (2, 1), (3, 1)]);
    for i in 0..5 {
        cluster.start(i);
    }
    cluster.wait_height(&[0, 1, 2, 3, 4], 2);

    let first_decide_view: Arc<Mutex<Option<ViewNumber>>> = Arc::new(Mutex::new(None));
    let dropped = Arc::new(AtomicU64::new(0));
    let (latch, count) = (first_decide_view.clone(), dropped.clone());
    cluster.filter.rewrite(Arc::new(move |_, _, message| {
        if let Some((view, Phase::Decide)) = phase_vote(&message) {
            let mut first = latch.lock().unwrap();
            if *first.get_or_insert(view) == view {
                count.fetch_add(1, Ordering::Relaxed);
                return None;
            }
        }
        Some(message)
    }));

    log_with_context(
        None,
        "T1: adding n4 (power 1) with the first window view's Decide votes dropped.",
    );
    cluster.set_validator(0, 4, 1);
    cluster.wait_applied(&[0, 1, 2, 3, 4], 5, 4, Some(1));
    assert!(
        dropped.load(Ordering::Relaxed) > 0,
        "the scenario needs the first window view's Decide votes dropped (view {:?})",
        first_decide_view.lock().unwrap()
    );
}

/// T3 `laggard_crosses_a_membership_change`: n3, a continuing member, goes deaf when U's
/// Precommit-PC nudge reaches it: it holds U, is locked on U and votes Commit, but never learns U's
/// Commit PC. The others commit and decide U (`{n0..n4}`) and move on. Reconnected, n3 must commit
/// U and catch up.
#[test]
fn laggard_crosses_a_membership_change() {
    let mut cluster = Cluster::new(5, &[(0, 1), (1, 1), (2, 1), (3, 1)]);
    for i in 0..5 {
        cluster.start(i);
    }
    cluster.wait_height(&[0, 1, 2, 3, 4], 2);

    let deaf = Arc::new(AtomicBool::new(false));
    cluster.filter.rewrite(deaf_after_precommit_nudge(
        cluster.vk(3),
        deaf.clone(),
        |_, _| false,
    ));
    log_with_context(
        None,
        "T3: adding n4 (power 1); n3 goes deaf at U's Precommit-PC nudge.",
    );
    cluster.set_validator(0, 4, 1);
    let target = cluster.wait_applied(&[0, 1, 2, 4], 5, 4, Some(1));
    assert!(
        deaf.load(Ordering::Relaxed),
        "n3 must have received U's Precommit-PC nudge"
    );
    assert!(
        cluster.has_set(3, 4, 4, None),
        "n3 must not have committed U while deaf"
    );

    cluster.filter.heal();
    log_with_context(None, "T3: n3 reconnected.");
    cluster.wait_caught_up(3, 5, 4, Some(1), target);
}

/// T4 `late_joiner_crosses_its_own_join`: the joiner n4 is cut off through the window. The four
/// continuing members commit and decide U alone. Reconnected, n4 must commit U (its own join) and
/// catch up.
#[test]
fn late_joiner_crosses_its_own_join() {
    let mut cluster = Cluster::new(5, &[(0, 1), (1, 1), (2, 1), (3, 1)]);
    for i in 0..5 {
        cluster.start(i);
    }
    cluster.wait_height(&[0, 1, 2, 3, 4], 2);

    cluster.filter.rewrite(cut_off(vec![cluster.vk(4)]));
    log_with_context(None, "T4: adding n4 (power 1) with n4 cut off.");
    cluster.set_validator(0, 4, 1);
    let target = cluster.wait_applied(&[0, 1, 2, 3], 5, 4, Some(1));

    cluster.filter.heal();
    log_with_context(None, "T4: n4 reconnected.");
    cluster.wait_caught_up(4, 5, 4, Some(1), target);
}

/// T5 `fresh_node_syncs_across_a_membership_change`: after n4's join is decided, n5 (never a
/// validator) starts from genesis and must sync past U to the tip.
#[test]
fn fresh_node_syncs_across_a_membership_change() {
    let mut cluster = Cluster::new(6, &[(0, 1), (1, 1), (2, 1), (3, 1)]);
    for i in 0..5 {
        cluster.start(i);
    }
    // n5 is not running yet: keep its inbox empty.
    cluster.filter.rewrite(cut_off(vec![cluster.vk(5)]));
    cluster.wait_height(&[0, 1, 2, 3, 4], 2);

    log_with_context(None, "T5: adding n4 (power 1).");
    cluster.set_validator(0, 4, 1);
    let target = cluster.wait_applied(&[0, 1, 2, 3, 4], 5, 4, Some(1));

    cluster.filter.heal();
    cluster.start(5);
    log_with_context(None, "T5: n5 started from genesis.");
    cluster.wait_caught_up(5, 5, 4, Some(1), target);
}

/// T6a `laggard_crosses_a_power_change` (small): n3's power 1 -> 2; n0 goes deaf at U's
/// Precommit-PC nudge. The Decide signers (n1, n2, n3) hold an old-set quorum under the old powers,
/// so n0 crosses on today's check.
#[test]
fn laggard_crosses_a_small_power_change() {
    let mut cluster = Cluster::new(4, &[(0, 1), (1, 1), (2, 1), (3, 1)]);
    for i in 0..4 {
        cluster.start(i);
    }
    cluster.wait_height(&[0, 1, 2, 3], 2);

    let deaf = Arc::new(AtomicBool::new(false));
    cluster.filter.rewrite(deaf_after_precommit_nudge(
        cluster.vk(0),
        deaf.clone(),
        |_, _| false,
    ));
    log_with_context(
        None,
        "T6a: n3 power 1 -> 2; n0 goes deaf at U's Precommit-PC nudge.",
    );
    cluster.set_validator(1, 3, 2);
    let target = cluster.wait_applied(&[1, 2, 3], 4, 3, Some(2));
    assert!(
        deaf.load(Ordering::Relaxed),
        "n0 must have received U's Precommit-PC nudge"
    );

    cluster.filter.heal();
    log_with_context(None, "T6a: n0 reconnected.");
    cluster.wait_caught_up(0, 4, 3, Some(2), target);
}

/// T6b `laggard_crosses_a_power_change` (large): n3's power 1 -> 10 (n3 alone is a quorum of the
/// new set, 10 of 13); n0 goes deaf at U's Precommit-PC nudge. n1's and n2's Decide votes are
/// dropped, so the Decide PC is n3's alone: power 1 under the old powers, below the old quorum (3 of
/// 4), and no more than the old set's fault bound (1). n0 cannot tell from the chain that the old
/// set committed U.
///
/// Stays RED with the s138 fix: n3 alone may be the old set's one tolerated fault, and the chain
/// carries no old-set signature on U, so n0 cannot tell U from a U* n3 built for itself. Crossing
/// needs U's Commit PC served to n0 (persisted by servers: a DB and protocol change).
#[test]
#[ignore = "needs U's Commit PC (format change): Decide PC signed only by a member holding the old fault bound"]
fn laggard_crosses_a_large_power_change() {
    let mut cluster = Cluster::new(4, &[(0, 1), (1, 1), (2, 1), (3, 1)]);
    for i in 0..4 {
        cluster.start(i);
    }
    cluster.wait_height(&[0, 1, 2, 3], 2);

    let quiet: HashSet<VerifyingKey> = [cluster.vk(1), cluster.vk(2)].into_iter().collect();
    let deaf = Arc::new(AtomicBool::new(false));
    cluster.filter.rewrite(deaf_after_precommit_nudge(
        cluster.vk(0),
        deaf.clone(),
        move |sender, message| {
            quiet.contains(sender) && matches!(phase_vote(message), Some((_, Phase::Decide)))
        },
    ));
    log_with_context(
        None,
        "T6b: n3 power 1 -> 10; n0 deaf at U's Precommit-PC nudge; n1/n2 Decide votes dropped.",
    );
    cluster.set_validator(1, 3, 10);
    let target = cluster.wait_applied(&[1, 2, 3], 4, 3, Some(10));
    assert!(
        deaf.load(Ordering::Relaxed),
        "n0 must have received U's Precommit-PC nudge"
    );

    cluster.filter.heal();
    log_with_context(None, "T6b: n0 reconnected.");
    cluster.wait_caught_up(0, 4, 3, Some(10), target);
}

/// T10 `removal_and_equal_add_complete_with_joiners_silent`: removing n3 from four completes; then
/// adding n4 (power 1) to the remaining three completes with n4 cut off through the window
/// (`P_new(C) = 3 >= q_new = 3`: the continuing members finish alone).
#[test]
fn removal_and_equal_add_complete_with_joiners_silent() {
    let mut cluster = Cluster::new(5, &[(0, 1), (1, 1), (2, 1), (3, 1)]);
    for i in 0..5 {
        cluster.start(i);
    }
    cluster.wait_height(&[0, 1, 2, 3, 4], 2);

    log_with_context(None, "T10: removing n3.");
    cluster.delete_validator(0, 3);
    cluster.wait_applied(&[0, 1, 2], 3, 3, None);

    cluster.filter.rewrite(cut_off(vec![cluster.vk(4)]));
    log_with_context(None, "T10: adding n4 (power 1) with n4 cut off.");
    cluster.set_validator(1, 4, 1);
    cluster.wait_applied(&[0, 1, 2], 4, 4, Some(1));
}
