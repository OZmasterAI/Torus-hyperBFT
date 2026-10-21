//! Pre-testnet "TC validation gap" (fix/tc-validation-gap).
//!
//! Two parts:
//!
//! (a) The header-first path (`on_receive_proposal_header` -> `cast_header_vote`,
//!     the production path for every proposal, re-proposals included) never
//!     validated the proposal's timeout certificate or no-endorsement
//!     certificate, while the full-proposal path did. Both paths now run one
//!     shared check ([`valid_proposal_certificates`](crate::hotstuff::types)),
//!     and the same table of cases below runs through both.
//!
//! (b) `TimeoutCertificate::is_correct` verified the per-voter signatures over
//!     `voter_metadata`, but never tied `high_tip`, `high_qc` or
//!     `high_tip_is_winner` (the fields the protocol acts on) to that signed
//!     metadata: anyone relaying a correct TC could rewrite them.
//!
//! RED-first: on main (9f8fb795) every `*_is_rejected` case fails on the
//! header path, the forged-summary and replayed-NEC cases also fail on the
//! full path, and every `tc_summary_*` test fails.

use std::sync::mpsc::Receiver;

use borsh::BorshSerialize;

use crate::app::{
    App, ProduceBlockRequest, ProduceBlockResponse, ValidateBlockRequest, ValidateBlockResponse,
};
use crate::block_tree::accessors::internal::BlockTreeSingleton;
use crate::events::Event;
use crate::hotstuff::header_fast_path_regression_test::{
    generic_pc, hotstuff_at_with_events, proposer_for, signing_keys, steady_block_tree,
    validator_set, MemKV, NullNetwork,
};
use crate::hotstuff::implementation::HotStuff;
use crate::hotstuff::messages::{BlockDataResponse, HotStuffMessage, Proposal, ProposalHeader};
use crate::hotstuff::types::{NoEndorsementCertificate, PhaseCertificate};
use crate::pacemaker::messages::PacemakerMessage;
use crate::pacemaker::types::{TimeoutCertificate, TimeoutVoteCollector, TipInfo};
use crate::types::block::Block;
use crate::types::crypto_primitives::{Keypair, SigningKey, VerifyingKey};
use crate::types::data_types::{
    BlockHeight, ChainID, CryptoHash, Data, SignatureBytes, SignatureSet, ViewNumber,
};
use crate::types::signed_messages::{Certificate, Collector};
use crate::types::validator_set::ValidatorSet;

const CHAIN_ID: ChainID = ChainID::new(0);
const VIEW: u64 = 5;

struct ValidApp;

impl App<MemKV> for ValidApp {
    fn produce_block(&mut self, _: ProduceBlockRequest<MemKV>) -> ProduceBlockResponse {
        unreachable!("replica-side test")
    }
    fn validate_block(&mut self, _: ValidateBlockRequest<MemKV>) -> ValidateBlockResponse {
        ValidateBlockResponse::Valid { app_state_updates: None, validator_set_updates: None }
    }
    fn validate_block_for_sync(&mut self, _: ValidateBlockRequest<MemKV>) -> ValidateBlockResponse {
        unreachable!("replica-side test")
    }
}

fn keys() -> Vec<SigningKey> {
    signing_keys(&[1, 2, 3, 4])
}

/// A safe block (genesis justify) distinguished by its data hash.
fn block(tag: u8) -> Block {
    Block::new(BlockHeight::new(0), PhaseCertificate::genesis_pc(), CryptoHash::new([tag; 32]), Data::new(vec![]))
}

fn tip(block: &Block, view: u64) -> TipInfo {
    TipInfo {
        block_hash: block.hash,
        block_height: block.height,
        block_justify: block.justify.clone(),
        block_data_hash: block.data_hash,
        view: ViewNumber::new(view),
    }
}

/// A TC exactly as the `TimeoutVoteCollector` forms it from correctly signed
/// timeout votes: signer `i` reports `tips[i]` and `qcs[i]`.
fn collected_tc(
    signers: &[SigningKey],
    set: &ValidatorSet,
    view: u64,
    tips: &[Option<TipInfo>],
    qcs: &[Option<PhaseCertificate>],
) -> TimeoutCertificate {
    let mut collector = TimeoutVoteCollector::new(CHAIN_ID, ViewNumber::new(view), set.clone());
    let mut tc = None;
    for (i, key) in signers.iter().enumerate() {
        let PacemakerMessage::TimeoutVote(vote) = PacemakerMessage::timeout_vote(
            &Keypair::new(key.clone()),
            CHAIN_ID,
            ViewNumber::new(view),
            None,
            tips[i].clone(),
            qcs[i].clone(),
        ) else {
            unreachable!()
        };
        tc = collector.collect(&key.verifying_key(), vote).or(tc);
    }
    tc.expect("signers form a quorum")
}

/// The honest re-proposal TC for view VIEW - 1: three of four signers
/// report `tip_block` as their local tip and no QC, so the tip wins.
fn reproposal_tc(tip_block: &Block) -> TimeoutCertificate {
    let keys = keys();
    let set = validator_set(&keys);
    let tips = vec![Some(tip(tip_block, VIEW - 1)); 3];
    collected_tc(&keys[..3], &set, VIEW - 1, &tips, &[None, None, None])
}

/// An NEC for `view`, signed by three of four validators.
fn nec(view: u64, high_tip_qc_view: u64) -> NoEndorsementCertificate {
    let keys = keys();
    let set = validator_set(&keys);
    let message = (ViewNumber::new(view), ViewNumber::new(high_tip_qc_view)).try_to_vec().unwrap();
    let mut signatures = SignatureSet::new(set.len());
    for key in &keys[..3] {
        let pos = set.position(&key.verifying_key()).unwrap();
        signatures.set(pos, Some(Keypair::new(key.clone()).sign(&message)));
    }
    NoEndorsementCertificate {
        view: ViewNumber::new(view),
        high_tip_qc_view: ViewNumber::new(high_tip_qc_view),
        signatures,
    }
}

/// A non-leader replica at VIEW, and leader(VIEW).
struct Replica {
    hotstuff: HotStuff<NullNetwork>,
    tree: BlockTreeSingleton<MemKV>,
    leader: VerifyingKey,
    events: Receiver<Event>,
}

fn replica() -> Replica {
    let keys = keys();
    let set = validator_set(&keys);
    let (tree, vss) = steady_block_tree(&set);
    let view = ViewNumber::new(VIEW);
    let leader = proposer_for(view, &keys, &vss, &tree);
    let local = keys.iter().find(|key| key.verifying_key() != leader).unwrap().clone();
    let (hotstuff, events) = hotstuff_at_with_events(view, local, vss);
    Replica { hotstuff, tree, leader, events }
}

impl Replica {
    fn voted(&self) -> bool {
        self.events.try_iter().any(|event| matches!(event, Event::PhaseVote(_)))
    }
}

/// Does a replica phase-vote for `block` proposed at VIEW with these
/// certificates, sent header-first (header, then the body)?
fn header_path_votes(block: &Block, tc: Option<TimeoutCertificate>, nec: Option<NoEndorsementCertificate>) -> bool {
    let mut r = replica();
    let header = ProposalHeader {
        chain_id: CHAIN_ID,
        view: ViewNumber::new(VIEW),
        block_hash: block.hash,
        height: block.height,
        data_hash: block.data_hash,
        justify: block.justify.clone(),
        tc,
        nec,
        has_validator_set_updates: false,
    };
    let leader = r.leader;
    r.hotstuff
        .on_receive_msg(HotStuffMessage::ProposalHeader(header), &leader, &mut r.tree, &mut ValidApp)
        .unwrap();
    let body = BlockDataResponse { view: ViewNumber::new(VIEW), block: block.clone() };
    r.hotstuff
        .on_receive_msg(HotStuffMessage::BlockDataResponse(body), &leader, &mut r.tree, &mut ValidApp)
        .unwrap();
    r.voted()
}

/// The same proposal sent as a full `Proposal`.
fn full_path_votes(block: &Block, tc: Option<TimeoutCertificate>, nec: Option<NoEndorsementCertificate>) -> bool {
    let mut r = replica();
    let proposal = Proposal { chain_id: CHAIN_ID, view: ViewNumber::new(VIEW), block: block.clone(), tc, nec };
    let leader = r.leader;
    r.hotstuff
        .on_receive_msg(HotStuffMessage::Proposal(proposal), &leader, &mut r.tree, &mut ValidApp)
        .unwrap();
    r.voted()
}

type Certs = (Option<TimeoutCertificate>, Option<NoEndorsementCertificate>);

/// Run one case through both paths: both must give `expect_vote`.
fn assert_both_paths(name: &str, block: &Block, certs: Certs, expect_vote: bool) {
    let (tc, nec) = certs;
    let header = header_path_votes(block, tc.clone(), nec.clone());
    let full = full_path_votes(block, tc, nec);
    assert!(
        header == expect_vote && full == expect_vote,
        "{name}: expected vote={expect_vote}, header path voted={header}, full path voted={full}"
    );
}

fn corrupt_one_signature(tc: &mut TimeoutCertificate) {
    let pos = tc.signatures.iter().position(Option::is_some).unwrap();
    let mut bytes = tc.signatures.get(pos).unwrap().bytes();
    bytes[0] ^= 1;
    tc.signatures.set(pos, Some(SignatureBytes::new(bytes)));
}

// ---------------------------------------------------------------------------
// Controls: honest certificates are voted on through both paths.
// ---------------------------------------------------------------------------

#[test]
fn honest_proposals_are_voted_on_both_paths() {
    let b = block(1);
    assert_both_paths("no certificates", &b, (None, None), true);
    assert_both_paths("honest re-proposal", &b, (Some(reproposal_tc(&b)), None), true);
    // Fresh proposal after RECOVER: the TC's tip (C) lost to an NEC for this
    // view over C's QC view, so the leader proposes a new block B.
    let c = block(2);
    let honest_nec = nec(VIEW, c.justify.view.int());
    assert_both_paths("honest NEC proposal", &b, (Some(reproposal_tc(&c)), Some(honest_nec)), true);
}

// ---------------------------------------------------------------------------
// (a) Invalid, wrongly signed or mismatched certificates.
// ---------------------------------------------------------------------------

#[test]
fn wrongly_signed_tc_is_rejected() {
    let b = block(1);
    let mut tc = reproposal_tc(&b);
    corrupt_one_signature(&mut tc);
    assert_both_paths("TC with a bad signature", &b, (Some(tc), None), false);
}

#[test]
fn tc_below_quorum_is_rejected() {
    let b = block(1);
    let keys = keys();
    let set = validator_set(&keys);
    // Two of three signatures: drop one from a quorum TC.
    let mut tc = reproposal_tc(&b);
    let pos = tc.signatures.iter().position(Option::is_some).unwrap();
    tc.signatures.set(pos, None);
    tc.voter_metadata[pos] = (None, None);
    assert!(!tc.is_correctly_signed(&set), "fixture: below quorum");
    assert_both_paths("TC below quorum", &b, (Some(tc), None), false);
}

#[test]
fn tc_for_another_view_is_rejected() {
    let b = block(1);
    let keys = keys();
    let set = validator_set(&keys);
    let tips = vec![Some(tip(&b, VIEW - 3)); 3];
    let stale = collected_tc(&keys[..3], &set, VIEW - 2, &tips, &[None, None, None]);
    assert_both_paths("TC for view - 2", &b, (Some(stale), None), false);
}

#[test]
fn tc_whose_tip_is_another_block_is_rejected() {
    let b = block(1);
    let c = block(2);
    assert_both_paths("re-proposal of B under a TC whose tip is C", &b, (Some(reproposal_tc(&c)), None), false);
}

#[test]
fn invalid_nec_is_rejected() {
    let b = block(1);
    let c = block(2);
    let mut bad = nec(VIEW, c.justify.view.int());
    let pos = bad.signatures.iter().position(Option::is_some).unwrap();
    let mut bytes = bad.signatures.get(pos).unwrap().bytes();
    bytes[0] ^= 1;
    bad.signatures.set(pos, Some(SignatureBytes::new(bytes)));
    assert_both_paths("NEC with a bad signature", &b, (Some(reproposal_tc(&c)), Some(bad)), false);
}

/// A correct NEC from another view must not excuse a re-proposal TC whose
/// tip is not the proposed block.
#[test]
fn nec_from_another_view_is_rejected() {
    let b = block(1);
    let c = block(2);
    let replayed = nec(VIEW + 1, c.justify.view.int());
    assert_both_paths("NEC for view + 1", &b, (Some(reproposal_tc(&c)), Some(replayed)), false);
}

/// The NEC must be over the QC view of the TC's high tip.
#[test]
fn nec_for_another_tip_is_rejected() {
    let keys = keys();
    let set = validator_set(&keys);
    let b = block(1);
    let parent = block(3);
    let qc = generic_pc(ViewNumber::new(2), parent.hash, &keys[..3], &set);
    let c = Block::new(BlockHeight::new(1), qc, CryptoHash::new([2; 32]), Data::new(vec![]));
    let tips = vec![Some(tip(&c, VIEW - 1)); 3];
    let tc = collected_tc(&keys[..3], &set, VIEW - 1, &tips, &[None, None, None]);
    // The TC's tip C has a QC at view 2; this NEC is over QC view 0.
    assert_both_paths("NEC over another QC view", &b, (Some(tc), Some(nec(VIEW, 0))), false);
}

#[test]
fn nec_without_tc_is_rejected() {
    let b = block(1);
    assert_both_paths("NEC without a TC", &b, (None, Some(nec(VIEW, 0))), false);
}

/// A TC that does not make the proposal a re-proposal (its tip lost) is still
/// a certificate the proposal carries: it must be correct and for VIEW - 1.
#[test]
fn bad_tc_on_a_fresh_proposal_is_rejected() {
    let b = block(1);
    let mut tc = reproposal_tc(&b);
    corrupt_one_signature(&mut tc);
    tc.high_tip = None;
    tc.high_tip_is_winner = false;
    for meta in tc.voter_metadata.iter_mut() {
        meta.0 = None;
    }
    assert_both_paths("fresh proposal with a wrongly signed TC", &b, (Some(tc), None), false);
}

// ---------------------------------------------------------------------------
// (b) high_tip / high_qc / high_tip_is_winner must be what the TC signers signed.
// ---------------------------------------------------------------------------

/// The signers reported tip C; the relayer rewrote the TC's high tip to B.
#[test]
fn forged_high_tip_is_rejected() {
    let b = block(1);
    let c = block(2);
    let mut tc = reproposal_tc(&c);
    tc.high_tip = Some(tip(&b, VIEW - 1));
    assert_both_paths("re-proposal under a rewritten high tip", &b, (Some(tc), None), false);
}

#[test]
fn tc_summary_rewritten_high_tip_is_not_correct() {
    let keys = keys();
    let set = validator_set(&keys);
    let (tree, _) = steady_block_tree(&set);
    let b = block(1);
    let c = block(2);
    let honest = reproposal_tc(&c);
    assert!(honest.is_correct(&tree).unwrap(), "control: collector TC is correct");

    let mut forged = honest.clone();
    forged.high_tip = Some(tip(&b, VIEW - 1));
    assert!(!forged.is_correct(&tree).unwrap(), "high tip no signer reported");
}

/// Dropping the signed winning tip turns a re-proposal view into a fresh one.
#[test]
fn tc_summary_hidden_high_tip_is_not_correct() {
    let keys = keys();
    let set = validator_set(&keys);
    let (tree, _) = steady_block_tree(&set);
    let mut hidden = reproposal_tc(&block(2));
    hidden.high_tip = None;
    hidden.high_tip_is_winner = false;
    assert!(!hidden.is_correct(&tree).unwrap(), "signed tips but no high tip");
}

#[test]
fn tc_summary_flipped_winner_is_not_correct() {
    let keys = keys();
    let set = validator_set(&keys);
    let (tree, _) = steady_block_tree(&set);
    let mut flipped = reproposal_tc(&block(2));
    flipped.high_tip_is_winner = false;
    assert!(!flipped.is_correct(&tree).unwrap(), "tip with no QC must win");
}

#[test]
fn tc_summary_high_qc_must_be_the_highest_signed_qc() {
    let keys = keys();
    let set = validator_set(&keys);
    let (tree, _) = steady_block_tree(&set);
    let low = generic_pc(ViewNumber::new(2), block(7).hash, &keys[..3], &set);
    let high = generic_pc(ViewNumber::new(3), block(8).hash, &keys[..3], &set);
    let tc = collected_tc(
        &keys[..3],
        &set,
        VIEW - 1,
        &[None, None, None],
        &[Some(low.clone()), Some(high.clone()), Some(low.clone())],
    );
    assert!(tc.is_correct(&tree).unwrap(), "control: collector TC is correct");
    assert!(tc.high_qc.as_ref() == Some(&high), "control: collector keeps the highest QC");

    let mut lower = tc.clone();
    lower.high_qc = Some(low);
    assert!(!lower.is_correct(&tree).unwrap(), "a lower signed QC is not the high QC");

    let mut unsigned = tc.clone();
    unsigned.high_qc = Some(generic_pc(ViewNumber::new(3), block(9).hash, &keys[..3], &set));
    assert!(!unsigned.is_correct(&tree).unwrap(), "a QC no signer reported");

    let mut dropped = tc.clone();
    dropped.high_qc = None;
    assert!(!dropped.is_correct(&tree).unwrap(), "signed QCs but no high QC");
}
