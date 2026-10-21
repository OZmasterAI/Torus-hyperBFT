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
    let keys = signing_keys(&[1, 2, 3, 4]);
    let set = validator_set(&keys);
    let (tree, vss) = steady_block_tree(&set);
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
