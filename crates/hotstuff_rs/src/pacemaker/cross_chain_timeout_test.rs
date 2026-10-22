//! Timeout votes and TCs carrying another chain's certificates
//! (fix/tc-embedded-chain-id; see `hotstuff/cross_chain_certificate_test.rs`).
//!
//! The network layer drops a message whose own `chain_id` is not ours, but a
//! chain-0 timeout vote can carry a chain-7 QC (as its highest QC) or a chain-7 TC
//! (as its fallback TC), and a chain-0 AdvanceView a chain-0 TC whose high QC is
//! chain 7's. Those embedded certificates were checked for signatures only.
//!
//! RED-first: on main (87eea189) every test here fails.

use std::time::Duration;

use super::*;
use crate::hotstuff::cross_chain_certificate_test::{pc_on, tc_on};
use crate::hotstuff::header_fast_path_regression_test::{generic_pc, signing_keys, steady_block_tree, validator_set, MemKV};
use crate::networking::network::Network;
use crate::types::crypto_primitives::SigningKey;
use crate::types::data_types::CryptoHash;

const CHAIN: ChainID = ChainID::new(0);
const FOREIGN: ChainID = ChainID::new(7);

#[derive(Clone, Default)]
struct NullNetwork;

impl Network for NullNetwork {
    fn init_validator_set(&mut self, _: ValidatorSet) {}
    fn update_validator_set(&mut self, _: crate::types::update_sets::ValidatorSetUpdates) {}
    fn broadcast(&mut self, _: Message) {}
    fn send(&mut self, _: VerifyingKey, _: Message) {}
    fn recv(&mut self) -> Option<(VerifyingKey, Message)> {
        None
    }
}

struct Fixture {
    keys: Vec<SigningKey>,
    tree: BlockTreeSingleton<MemKV>,
    pacemaker: Pacemaker<NullNetwork>,
}

/// Four validators of power 1 (quorum 3, f = 1); the local replica is the
/// fourth, at view 5.
fn fixture() -> Fixture {
    let keys = signing_keys(&[1, 2, 3, 4]);
    let set = validator_set(&keys);
    let (tree, vss) = steady_block_tree(&set);
    let config = PacemakerConfiguration {
        chain_id: CHAIN,
        keypair: Keypair::new(keys[3].clone()),
        epoch_length: EpochLength::new(100_000),
        max_view_time: Duration::from_secs(3600),
        backoff_factor: 2,
        backoff_cap: 8,
        commit_lag_cap: 0,
    };
    let pacemaker =
        Pacemaker::new(config, SenderHandle::new(NullNetwork), ViewNumber::new(5), &vss, None).unwrap();
    Fixture { keys, tree, pacemaker }
}

impl Fixture {
    fn set(&self) -> ValidatorSet {
        validator_set(&self.keys)
    }

    /// Deliver validator `i`'s chain-0 timeout vote for `view`.
    fn timeout_vote(&mut self, i: usize, view: u64, highest_tc: Option<TimeoutCertificate>, qc: Option<PhaseCertificate>) {
        let msg = PacemakerMessage::timeout_vote(
            &Keypair::new(self.keys[i].clone()),
            CHAIN,
            ViewNumber::new(view),
            highest_tc,
            None,
            qc,
        );
        let origin = self.keys[i].verifying_key();
        self.pacemaker.on_receive_msg(msg, &origin, &mut self.tree).unwrap();
    }
}

/// A Byzantine voter reports a correctly signed chain-7 QC as its highest QC.
/// The honest collector must drop that vote, so the two honest votes alone
/// form no TC, and no TC carries chain 7's QC.
#[test]
fn timeout_vote_with_another_chains_high_qc_is_dropped() {
    let mut f = fixture();
    let set = f.set();
    let ours = generic_pc(ViewNumber::new(3), CryptoHash::new([9; 32]), &f.keys[..3], &set);
    let foreign = pc_on(FOREIGN, 900, CryptoHash::new([10; 32]), &f.keys[..3], &set);
    f.timeout_vote(0, 5, None, Some(foreign));
    f.timeout_vote(1, 5, None, Some(ours.clone()));
    f.timeout_vote(2, 5, None, Some(ours));
    let tc = f.tree.highest_tc().unwrap();
    assert!(
        tc.as_ref().and_then(|tc| tc.high_qc.as_ref()).is_none_or(|qc| qc.chain_id == CHAIN),
        "a TC formed with chain 7's QC as its high QC"
    );
    assert!(tc.is_none(), "the Byzantine vote must not count toward the quorum");
}

/// A Byzantine voter attaches a real chain-7 TC of a far view as its fallback
/// TC. It must not become the highest TC or move this replica's view.
#[test]
fn fallback_tc_of_another_chain_does_not_move_the_view() {
    let mut f = fixture();
    let set = f.set();
    let foreign = tc_on(FOREIGN, 1_000, &f.keys[..3], &set, &[None, None, None]);
    f.timeout_vote(0, 5, Some(foreign), None);
    assert_eq!(f.pacemaker.query().view, ViewNumber::new(5), "a chain-7 TC moved the view");
    assert!(f.tree.highest_tc().unwrap().is_none());
}

/// An AdvanceView with a chain-0 TC whose high QC is chain 7's: the TC is not
/// correct, so it neither becomes the highest TC (which the next leader would
/// act on, case 3 adopting the high QC as its highest PC) nor moves the view.
#[test]
fn advance_view_with_a_tc_carrying_another_chains_qc_is_ignored() {
    let mut f = fixture();
    let set = f.set();
    let ours = generic_pc(ViewNumber::new(3), CryptoHash::new([9; 32]), &f.keys[..3], &set);
    let foreign = pc_on(FOREIGN, 900, CryptoHash::new([10; 32]), &f.keys[..3], &set);
    let tc = tc_on(CHAIN, 5, &f.keys[..3], &set, &[Some(ours.clone()), Some(ours), Some(foreign)]);
    let msg = PacemakerMessage::advance_view(ProgressCertificate::TimeoutCertificate(tc));
    let origin = f.keys[0].verifying_key();
    f.pacemaker.on_receive_msg(msg, &origin, &mut f.tree).unwrap();
    assert!(f.tree.highest_tc().unwrap().is_none(), "the TC with chain 7's high QC was accepted");
    assert_eq!(f.pacemaker.query().view, ViewNumber::new(5));
}
