//! Phase A of docs/plans/tc-required-and-tip-signing.md, C1/C3: the pacemaker
//! keeps collecting timeout votes for the view it just left, so leader(v)
//! forms TC(v-1) after it timed out of v-1 itself.
//!
//! RED-first: before phase A `update_view` replaced the only collector, so a
//! TimeoutVote for `cur - 1` never reached a collector for its view.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::*;
use crate::hotstuff::header_fast_path_regression_test::{
    signing_keys, steady_block_tree, validator_set, MemKV,
};
use crate::hotstuff::types::PhaseCertificate;
use crate::types::crypto_primitives::SigningKey;
use crate::types::data_types::CryptoHash;
use crate::pacemaker::types::TipInfo;
use crate::pacemaker::implementation::select_leader;

const CHAIN: ChainID = ChainID::new(0);

#[derive(Clone, Default)]
struct RecordingNetwork {
    broadcasts: Arc<Mutex<Vec<Message>>>,
}

impl Network for RecordingNetwork {
    fn init_validator_set(&mut self, _: ValidatorSet) {}
    fn update_validator_set(&mut self, _: crate::types::update_sets::ValidatorSetUpdates) {}
    fn broadcast(&mut self, message: Message) {
        self.broadcasts.lock().unwrap().push(message);
    }
    fn send(&mut self, _: VerifyingKey, _: Message) {}
    fn recv(&mut self) -> Option<(VerifyingKey, Message)> {
        None
    }
}

impl RecordingNetwork {
    fn timeout_votes_sent(&self) -> Vec<ViewNumber> {
        use crate::networking::messages::ProgressMessage;
        self.broadcasts
            .lock()
            .unwrap()
            .iter()
            .filter_map(|m| match m {
                Message::ProgressMessage(ProgressMessage::PacemakerMessage(
                    PacemakerMessage::TimeoutVote(tv),
                )) => Some(tv.view),
                _ => None,
            })
            .collect()
    }
}

struct Fixture {
    keys: Vec<SigningKey>,
    vss: ValidatorSetState,
    tree: BlockTreeSingleton<MemKV>,
    pacemaker: Pacemaker<RecordingNetwork>,
    net: RecordingNetwork,
}

/// Four validators of power 1 (quorum 3, f = 1); the local replica is the
/// fourth, at `init_view`.
fn fixture(init_view: u64) -> Fixture {
    fixture_with_epoch_length(init_view, 100_000)
}

/// [`fixture`] with epoch-change views every `epoch_length` views.
fn fixture_with_epoch_length(init_view: u64, epoch_length: u32) -> Fixture {
    let keys = signing_keys(&[1, 2, 3, 4]);
    let set = validator_set(&keys);
    let (tree, vss) = steady_block_tree(&set);
    let config = PacemakerConfiguration {
        chain_id: CHAIN,
        keypair: Keypair::new(keys[3].clone()),
        epoch_length: EpochLength::new(epoch_length),
        max_view_time: Duration::from_millis(500),
        backoff_factor: 2,
        backoff_cap: 8,
        commit_lag_cap: 0,
    };
    let net = RecordingNetwork::default();
    let pacemaker =
        Pacemaker::new(config, SenderHandle::new(net.clone()), ViewNumber::new(init_view), &vss, None)
            .unwrap();
    Fixture { keys, vss, tree, pacemaker, net }
}

impl Fixture {
    fn enter(&mut self, view: u64) {
        self.pacemaker
            .update_view(ViewNumber::new(view), &self.vss, ViewNumber::new(0), ViewNumber::new(0))
            .unwrap();
    }

    /// Deliver a TimeoutVote for `view` from validator `i`.
    fn timeout_vote(&mut self, i: usize, view: u64) {
        let msg = PacemakerMessage::timeout_vote(
            &Keypair::new(self.keys[i].clone()),
            CHAIN,
            ViewNumber::new(view),
            None,
            None,
            None,
        );
        let origin = self.keys[i].verifying_key();
        self.pacemaker.on_receive_msg(msg, &origin, &mut self.tree).unwrap();
    }

    fn highest_tc_view(&self) -> Option<u64> {
        self.tree.highest_tc().unwrap().map(|tc| tc.view.int())
    }
}

#[test]
fn timeout_votes_for_the_view_just_left_form_a_tc() {
    let mut f = fixture(5);
    f.enter(6);
    for i in 0..3 {
        f.timeout_vote(i, 5);
    }
    assert_eq!(f.highest_tc_view(), Some(5), "TC(5) must form at view 6");
    assert_eq!(f.pacemaker.query().view, ViewNumber::new(6), "TC(5) does not move view 6");
}

#[test]
fn votes_collected_before_leaving_count_toward_the_tc() {
    let mut f = fixture(5);
    f.timeout_vote(0, 5);
    f.timeout_vote(1, 5);
    assert_eq!(f.highest_tc_view(), None, "two of four: no quorum yet");
    f.enter(6);
    f.timeout_vote(2, 5);
    assert_eq!(f.highest_tc_view(), Some(5), "the outgoing collector is kept");
}

#[test]
fn a_skip_gets_a_fresh_collector_for_the_previous_view() {
    let mut f = fixture(3);
    f.enter(6);
    for i in 0..3 {
        f.timeout_vote(i, 5);
    }
    assert_eq!(f.highest_tc_view(), Some(5));
}

#[test]
fn timeout_votes_two_views_back_are_not_collected() {
    let mut f = fixture(5);
    f.enter(6);
    f.enter(7);
    for i in 0..3 {
        f.timeout_vote(i, 5);
    }
    assert_eq!(f.highest_tc_view(), None, "only cur - 1 is kept");
}

/// C3: the Bracha echo stays restricted to `view >= cur`.
#[test]
fn no_bracha_echo_for_the_previous_view() {
    let mut f = fixture(5);
    f.enter(6);
    f.timeout_vote(0, 5);
    f.timeout_vote(1, 5);
    assert!(f.net.timeout_votes_sent().is_empty(), "echoed {:?}", f.net.timeout_votes_sent());
    // Control: the same votes for the current view are echoed.
    f.timeout_vote(0, 6);
    f.timeout_vote(1, 6);
    assert_eq!(f.net.timeout_votes_sent(), vec![ViewNumber::new(6)]);
}

// ---------------------------------------------------------------------------
// Phase B (phase-A open question 2): a timeout vote binds its highest QC only
// by the (view, block) summary it signs, so a byzantine voter can report a
// forged high-view QC. The pacemaker drops such a vote before collecting it;
// otherwise it would become the TC's high QC and, under V3, block every
// case-3 proposal backed by that TC.
//
// RED-first: on phase A (3e5317ff) the forged vote is collected, TC(5) forms
// with three votes and carries the forged QC.
// ---------------------------------------------------------------------------

impl Fixture {
    /// Deliver a TimeoutVote for `view` from validator `i` reporting `highest_qc`.
    fn timeout_vote_with_qc(&mut self, i: usize, view: u64, highest_qc: Option<PhaseCertificate>) {
        let msg = PacemakerMessage::timeout_vote(
            &Keypair::new(self.keys[i].clone()),
            CHAIN,
            ViewNumber::new(view),
            None,
            None,
            highest_qc,
        );
        let origin = self.keys[i].verifying_key();
        self.pacemaker.on_receive_msg(msg, &origin, &mut self.tree).unwrap();
    }
}

/// A Generic QC for `view` over `block` signed by `signers`.
fn qc_signed_by(f: &Fixture, view: u64, block: u8, signers: std::ops::Range<usize>) -> PhaseCertificate {
    crate::hotstuff::header_fast_path_regression_test::generic_pc(
        ViewNumber::new(view),
        CryptoHash::new([block; 32]),
        &f.keys[signers],
        f.vss.committed_validator_set(),
    )
}

#[test]
fn a_timeout_vote_with_a_forged_high_qc_is_not_collected() {
    for current in [5, 6] {
        let mut f = fixture(5);
        if current == 6 {
            f.enter(6); // the previous-view collector (C3) as well
        }
        let honest = qc_signed_by(&f, 3, 7, 0..3);
        let forged = qc_signed_by(&f, 4, 8, 0..1);
        f.timeout_vote_with_qc(0, 5, Some(forged));
        f.timeout_vote_with_qc(1, 5, Some(honest.clone()));
        f.timeout_vote_with_qc(2, 5, None);
        assert_eq!(f.highest_tc_view(), None, "at view {current}: the forged vote must not count");
        f.timeout_vote_with_qc(3, 5, None);
        let tc = f.tree.highest_tc().unwrap().expect("three honest votes form TC(5)");
        assert!(tc.high_qc.as_ref() == Some(&honest), "at view {current}: the TC's high QC is the honest one");
        assert!(tc.is_correct(&f.tree).unwrap(), "at view {current}: the TC is correct");
    }
}

/// A correct high QC is collected as before.
#[test]
fn a_timeout_vote_with_a_correct_high_qc_is_collected() {
    let mut f = fixture(5);
    let honest = qc_signed_by(&f, 3, 7, 0..3);
    f.timeout_vote_with_qc(0, 5, Some(honest.clone()));
    f.timeout_vote_with_qc(1, 5, None);
    f.timeout_vote_with_qc(2, 5, None);
    let tc = f.tree.highest_tc().unwrap().expect("TC(5)");
    assert!(tc.high_qc.as_ref() == Some(&honest));
}

// ---------------------------------------------------------------------------
// Found in phase B (phase-A bug): a TC for the view just left (C3), collected
// after entering view v, went out in an AdvanceView that marked v as "already
// advanced" (`last_advance_view = v`). When TC(v) then formed, no AdvanceView
// carried it, and nothing else calls `update_view(v + 1)` for a TC: in an
// epoch-change view (which extends on timeout instead of advancing) every
// replica stayed in v with TC(v) in hand. Seen as the 2:2:1 / 4x1 one-down
// clusters and the VSU test wedging at view 50 (DEFAULT_EPOCH_LENGTH) when
// they were slow enough to reach it.
//
// RED-first: on 42d94443 no AdvanceView with TC(6) is broadcast and the
// replica stays in view 6.
// ---------------------------------------------------------------------------

impl RecordingNetwork {
    fn advance_views_sent(&self) -> Vec<AdvanceView> {
        use crate::networking::messages::ProgressMessage;
        self.broadcasts
            .lock()
            .unwrap()
            .iter()
            .filter_map(|m| match m {
                Message::ProgressMessage(ProgressMessage::PacemakerMessage(PacemakerMessage::AdvanceView(av))) => {
                    Some(av.clone())
                }
                _ => None,
            })
            .collect()
    }
}

#[test]
fn a_previous_view_tc_does_not_stop_the_current_view_tc_from_advancing() {
    // View 6 is an epoch-change view: on timeout it extends, so only a TC(6)
    // (through an AdvanceView) moves the replica to 7.
    let mut f = fixture_with_epoch_length(5, 6);
    f.enter(6);
    for i in 0..3 {
        f.timeout_vote(i, 5);
    }
    assert_eq!(f.highest_tc_view(), Some(5), "TC(5) from the previous-view collector");
    for i in 0..3 {
        f.timeout_vote(i, 6);
    }
    assert_eq!(f.highest_tc_view(), Some(6), "TC(6) collected");
    let tc6 = f
        .net
        .advance_views_sent()
        .into_iter()
        .find(|av| av.progress_certificate.view() == ViewNumber::new(6))
        .expect("an AdvanceView carrying TC(6) must be broadcast");
    // Its loopback copy moves this replica on.
    let me = f.keys[3].verifying_key();
    f.pacemaker
        .on_receive_msg(PacemakerMessage::AdvanceView(tc6), &me, &mut f.tree)
        .unwrap();
    assert_eq!(f.pacemaker.query().view, ViewNumber::new(7), "TC(6) must move the replica out of view 6");
}

// ---------------------------------------------------------------------------
// Phase C, F4 (docs/plans/tc-required-and-tip-signing.md §3): the pacemaker
// checks a timeout vote's tip (F2: its header hashes to its block and
// leader(view) signed it; and it is no newer than the vote) before echoing
// or collecting the vote. A vote with an invalid tip is dropped, so the TC
// forms from the honest votes and its high tip is a real header.
//
// RED-first: on the phase-C skeleton the forged vote is collected and TC(5)
// forms from three votes with the forged tip as its high tip.
// ---------------------------------------------------------------------------

impl Fixture {
    /// Deliver a TimeoutVote for `view` from validator `i` reporting `tip`.
    fn timeout_vote_with_tip(&mut self, i: usize, view: u64, tip: Option<TipInfo>) {
        let msg = PacemakerMessage::timeout_vote(
            &Keypair::new(self.keys[i].clone()),
            CHAIN,
            ViewNumber::new(view),
            None,
            tip,
            None,
        );
        let origin = self.keys[i].verifying_key();
        self.pacemaker.on_receive_msg(msg, &origin, &mut self.tree).unwrap();
    }

    /// The tip of a vote for a genesis-justified block `seed` proposed at
    /// `view`, signed by `signer` (leader(`view`) unless forging).
    fn tip_signed_by(&self, seed: u8, view: u64, signer: &VerifyingKey) -> TipInfo {
        let block = crate::types::block::Block::new(
            crate::types::data_types::BlockHeight::new(0),
            PhaseCertificate::genesis_pc(),
            CryptoHash::new([seed; 32]),
            crate::types::data_types::Data::new(vec![]),
        );
        let header = crate::test_keys::unsigned_header(CHAIN, &block, ViewNumber::new(view));
        crate::test_keys::tip_signed_by(header, signer)
    }

    fn leader(&self, view: u64) -> VerifyingKey {
        select_leader(ViewNumber::new(view), self.vss.committed_validator_set())
    }
}

#[test]
fn a_timeout_vote_with_an_invalid_tip_is_not_collected() {
    let probe = fixture(5);
    let honest = probe.tip_signed_by(7, 4, &probe.leader(4));
    let mut forged_view = probe.tip_signed_by(8, 3, &probe.leader(3));
    forged_view.view = ViewNumber::new(4);
    let non_leader = probe.keys.iter().map(|k| k.verifying_key()).find(|vk| *vk != probe.leader(4)).unwrap();
    let wrong_signer = probe.tip_signed_by(8, 4, &non_leader);
    let mut wrong_justify = probe.tip_signed_by(8, 4, &probe.leader(4));
    wrong_justify.block_height = crate::types::data_types::BlockHeight::new(3);
    let from_the_future = probe.tip_signed_by(8, 6, &probe.leader(6));
    for (name, forged) in [
        ("forged view", forged_view),
        ("signed by a non-leader", wrong_signer),
        ("header fields not hashing to the block", wrong_justify),
        ("tip newer than the vote", from_the_future),
    ] {
        for current in [5, 6] {
            let mut f = fixture(5);
            if current == 6 {
                f.enter(6); // the previous-view collector (C3) as well
            }
            f.timeout_vote_with_tip(0, 5, Some(forged.clone()));
            f.timeout_vote_with_tip(1, 5, Some(honest.clone()));
            f.timeout_vote_with_tip(2, 5, None);
            assert_eq!(f.highest_tc_view(), None, "{name}, at view {current}: the forged vote must not count");
            f.timeout_vote_with_tip(3, 5, None);
            let tc = f.tree.highest_tc().unwrap().expect("three honest votes form TC(5)");
            assert!(tc.high_tip.as_ref() == Some(&honest), "{name}, at view {current}: the high tip is the honest one");
            assert!(tc.is_correct(&f.tree).unwrap(), "{name}, at view {current}: the TC is correct");
        }
    }
}

/// Control: a valid tip is collected and becomes the high tip.
#[test]
fn a_timeout_vote_with_a_valid_tip_is_collected() {
    let mut f = fixture(5);
    let honest = f.tip_signed_by(7, 4, &f.leader(4));
    f.timeout_vote_with_tip(0, 5, Some(honest.clone()));
    f.timeout_vote_with_tip(1, 5, None);
    f.timeout_vote_with_tip(2, 5, None);
    let tc = f.tree.highest_tc().unwrap().expect("TC(5)");
    assert!(tc.high_tip.as_ref() == Some(&honest));
    assert!(tc.is_correct(&f.tree).unwrap());
}

/// F4 makes every replica drop a vote whose tip fails F2, so an honest replica
/// whose stored tip no longer passes F2 (signed under a validator set older
/// than the previous one) must send its timeout votes without it, or none of
/// its votes would count. Here the stored tip's signer is not leader(4).
#[test]
fn own_timeout_vote_omits_a_tip_that_fails_f2() {
    let mut f = fixture(5);
    let non_leader = f.keys.iter().map(|k| k.verifying_key()).find(|vk| *vk != f.leader(4)).unwrap();
    let stale = f.tip_signed_by(9, 4, &non_leader);
    f.tree.set_local_tip(&stale).unwrap();
    // Two of four (above f = 1) time out of 5: the local replica echoes its own vote.
    f.timeout_vote_with_tip(0, 5, None);
    f.timeout_vote_with_tip(1, 5, None);
    let own: Vec<TimeoutVote> = f
        .net
        .broadcasts
        .lock()
        .unwrap()
        .iter()
        .filter_map(|m| match m {
            Message::ProgressMessage(crate::networking::messages::ProgressMessage::PacemakerMessage(
                PacemakerMessage::TimeoutVote(tv),
            )) => Some(tv.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(own.len(), 1, "fixture: the Bracha echo sent one own timeout vote");
    assert!(own[0].local_tip.is_none(), "a tip failing F2 is not reported");
    // The vote is collectible: with two more votes it completes TC(5).
    let msg = PacemakerMessage::TimeoutVote(own[0].clone());
    let me = f.keys[3].verifying_key();
    f.pacemaker.on_receive_msg(msg, &me, &mut f.tree).unwrap();
    f.timeout_vote_with_tip(2, 5, None);
    assert_eq!(f.highest_tc_view(), Some(5));
}

/// Control: a valid stored tip is reported.
#[test]
fn own_timeout_vote_reports_a_valid_tip() {
    let mut f = fixture(5);
    let tip = f.tip_signed_by(9, 4, &f.leader(4));
    f.tree.set_local_tip(&tip).unwrap();
    f.timeout_vote_with_tip(0, 5, None);
    f.timeout_vote_with_tip(1, 5, None);
    let reported = f.net.broadcasts.lock().unwrap().iter().any(|m| {
        matches!(m, Message::ProgressMessage(crate::networking::messages::ProgressMessage::PacemakerMessage(
            PacemakerMessage::TimeoutVote(tv),
        )) if tv.local_tip.as_ref() == Some(&tip))
    });
    assert!(reported);
}

// ---------------------------------------------------------------------------
// TC phase C review (docs/plans/tc-required-and-tip-signing.md §4): the
// validator-set update window. The previous set is the seed validators 1..4,
// the committed set replaces 4 by 5; the local replica is 4 (previous set
// only). While the update is undecided, a view's one fresh proposer is the
// previous set's leader up to the update view and the committed set's after
// it (`ValidatorSetState::fresh_proposer_set`).
//
// RED-first: on the review skeleton (92105bb2) both tests fail: F4 collects
// the vote carrying X, and the timeout blames the committed set's leader.
// ---------------------------------------------------------------------------

fn window_sets() -> (ValidatorSet, ValidatorSet) {
    (validator_set(&signing_keys(&[1, 2, 3, 4])), validator_set(&signing_keys(&[1, 2, 3, 5])))
}

/// The first view from `from` whose leaders in the two sets differ.
fn window_split_view(from: u64) -> u64 {
    let (old, new) = window_sets();
    (from..from + 64)
        .find(|v| select_leader(ViewNumber::new(*v), &old) != select_leader(ViewNumber::new(*v), &new))
        .unwrap()
}

/// A window fixture at `init_view` (update committed, undecided, update view `update_view`).
fn window_fixture(init_view: u64, update_view: u64) -> Fixture {
    window_fixture_as(false, init_view, update_view)
}

/// [`window_fixture`] with the update decided (`decided`) or not.
fn window_fixture_as(decided: bool, init_view: u64, update_view: u64) -> Fixture {
    let (old, new) = window_sets();
    let vss = ValidatorSetState::new(new, old, Some(crate::types::data_types::BlockHeight::new(0)), decided)
        .with_update_view(Some(ViewNumber::new(update_view)));
    let mut tree = BlockTreeSingleton::new(MemKV::default());
    tree.initialize(&crate::types::update_sets::AppStateUpdates::new(), &vss).unwrap();
    let keys = signing_keys(&[1, 2, 3, 4]);
    let config = PacemakerConfiguration {
        chain_id: CHAIN,
        keypair: Keypair::new(keys[3].clone()),
        epoch_length: EpochLength::new(100_000),
        max_view_time: Duration::from_millis(500),
        backoff_factor: 2,
        backoff_cap: 8,
        commit_lag_cap: 0,
    };
    let net = RecordingNetwork::default();
    let pacemaker =
        Pacemaker::new(config, SenderHandle::new(net.clone()), ViewNumber::new(init_view), &vss, None).unwrap();
    Fixture { keys, vss, tree, pacemaker, net }
}

/// After the update view, X (signed by the previous set's leader of v) is not a tip: a timeout
/// vote reporting it is dropped, so TC(v) forms from the votes reporting B (the committed set's
/// leader's block) and names B.
#[test]
fn window_timeout_vote_reporting_the_previous_sets_tip_after_the_update_view_is_not_collected() {
    let (old, new) = window_sets();
    let v = window_split_view(2);
    let mut f = window_fixture(v, 1);
    let tip_b = f.tip_signed_by(7, v, &select_leader(ViewNumber::new(v), &new));
    let tip_x = f.tip_signed_by(8, v, &select_leader(ViewNumber::new(v), &old));
    f.timeout_vote_with_tip(0, v, Some(tip_x));
    f.timeout_vote_with_tip(1, v, Some(tip_b.clone()));
    f.timeout_vote_with_tip(2, v, Some(tip_b.clone()));
    assert_eq!(f.highest_tc_view(), None, "the vote reporting X must not count");
    f.timeout_vote_with_tip(3, v, None);
    let tc = f.tree.highest_tc().unwrap().expect("three votes without X form TC(v)");
    assert!(tc.high_tip.as_ref() == Some(&tip_b), "TC(v) names B");
    assert!(tc.is_correct(&f.tree).unwrap());
}

/// The timeout of a view is recorded against that view's fresh proposer (reputation
/// bookkeeping): the previous set's leader up to the update view, the committed set's after it.
#[test]
fn window_timeout_blames_the_views_fresh_proposer() {
    let (old, new) = window_sets();
    let early = window_split_view(1);
    let late = window_split_view(early + 1);
    for (view, update_view, blamed, spared) in [
        (early, early, &old, &new),
        (late, early, &new, &old),
    ] {
        let mut f = window_fixture(view, update_view);
        for i in 0..3 {
            f.timeout_vote(i, view);
        }
        assert_eq!(f.highest_tc_view(), Some(view), "fixture: TC({view}) forms");
        let reputation = f.tree.leader_reputation().unwrap();
        let blamed = select_leader(ViewNumber::new(view), blamed);
        let spared = select_leader(ViewNumber::new(view), spared);
        assert!(reputation.score_bps(&blamed) < 10_000, "view {view}: its fresh proposer is blamed");
        assert_eq!(reputation.score_bps(&spared), 10_000, "view {view}: the other set's leader is not");
    }
}

/// s136 ([claude-review] P3): a replica that has applied the update's Decide PC still collects a
/// timeout vote reporting a tip the previous set's leader signed at the update view (the honest
/// tip of every replica that has not voted for a fresh block since), and the TC it forms is
/// correct. On c300a015 the decided replica dropped such votes.
#[test]
fn window_decided_replica_collects_a_pre_update_tip() {
    let (old, _) = window_sets();
    let u = window_split_view(1);
    let v = u + 1;
    for decided in [false, true] {
        let mut f = window_fixture_as(decided, v, u);
        let tip = f.tip_signed_by(9, u, &select_leader(ViewNumber::new(u), &old));
        for i in 0..3 {
            f.timeout_vote_with_tip(i, v, Some(tip.clone()));
        }
        let tc = f.tree.highest_tc().unwrap().unwrap_or_else(|| panic!("decided={decided}: TC({v}) forms"));
        assert!(tc.high_tip.as_ref() == Some(&tip), "decided={decided}: TC({v}) names the pre-update tip");
        assert!(tc.is_correct(&f.tree).unwrap(), "decided={decided}: and is correct");
    }
}

/// s136: the timeout of a view up to the update view blames the previous set's leader whether or
/// not the update is decided; after it, the committed set's.
#[test]
fn window_decided_timeout_blames_the_views_fresh_proposer() {
    let (old, new) = window_sets();
    let early = window_split_view(1);
    let late = window_split_view(early + 1);
    for (view, blamed, spared) in [(early, &old, &new), (late, &new, &old)] {
        let mut f = window_fixture_as(true, view, early);
        for i in 0..3 {
            f.timeout_vote(i, view);
        }
        assert_eq!(f.highest_tc_view(), Some(view), "fixture: TC({view}) forms");
        let reputation = f.tree.leader_reputation().unwrap();
        let blamed = select_leader(ViewNumber::new(view), blamed);
        let spared = select_leader(ViewNumber::new(view), spared);
        assert!(reputation.score_bps(&blamed) < 10_000, "view {view}: its fresh proposer is blamed");
        assert_eq!(reputation.score_bps(&spared), 10_000, "view {view}: the other set's leader is not");
    }
}

// ---------------------------------------------------------------------------
// tc-liveness (s136): every timeout vote carries its sender's highest TC, and
// the pacemaker checked that TC's signatures again for every vote (plus its
// high QC and high tip). With tens of milliseconds per check (a debug build on
// a loaded host) the algorithm thread fell seconds behind and the cluster
// stopped committing (p00_cluster_tests under `cargo test`). A TC whose
// signatures this replica already checked (here: as the votes it collected
// into TC(5)) costs no further check.
//
// RED-first: on 56923063 each of the four votes below costs 1 + 3 checks.
// ---------------------------------------------------------------------------

impl Fixture {
    /// Deliver a TimeoutVote for `view` from validator `i` carrying `highest_tc`.
    fn timeout_vote_with_tc(&mut self, i: usize, view: u64, highest_tc: Option<crate::pacemaker::types::TimeoutCertificate>) {
        let msg = PacemakerMessage::timeout_vote(
            &Keypair::new(self.keys[i].clone()),
            CHAIN,
            ViewNumber::new(view),
            highest_tc,
            None,
            None,
        );
        let origin = self.keys[i].verifying_key();
        self.pacemaker.on_receive_msg(msg, &origin, &mut self.tree).unwrap();
    }
}

#[test]
fn timeout_votes_carrying_a_checked_tc_cost_one_signature_check_each() {
    use crate::types::crypto_primitives::signature_checks_on_this_thread;
    let mut f = fixture(5);
    for i in 0..3 {
        f.timeout_vote(i, 5);
    }
    let tc5 = f.tree.highest_tc().unwrap().expect("TC(5)");
    f.enter(6);
    let before = signature_checks_on_this_thread();
    for i in 0..4 {
        f.timeout_vote_with_tc(i, 6, Some(tc5.clone()));
    }
    assert_eq!(f.highest_tc_view(), Some(6), "the votes still form TC(6)");
    assert_eq!(
        signature_checks_on_this_thread() - before,
        4,
        "one check per vote (its own signature); TC(5)'s signatures were already checked"
    );
    let before = signature_checks_on_this_thread();
    assert!(tc5.is_correct(&f.tree).unwrap());
    assert_eq!(signature_checks_on_this_thread() - before, 0, "TC(5) again: no check");
}
