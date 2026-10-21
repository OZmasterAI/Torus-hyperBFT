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
use crate::pacemaker::select_leader;
use crate::pacemaker::types::{TimeoutCertificate, TimeoutVoteCollector, TipInfo};
use crate::test_keys::{key_of, signed_by, tip_signed_by};
use crate::types::signing::{no_endorsement_bytes, phase_vote_bytes, sign_proposal_header};
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

/// The tip of a voter that voted for `block` proposed at `view` by
/// leader(`view`) (F2: a signed header).
fn tip(block: &Block, view: u64) -> TipInfo {
    let leader = select_leader(ViewNumber::new(view), &validator_set(&keys()));
    tip_signed_by(header_of(block, view), &leader)
}

/// The unsigned header announcing `block` at `view` (sign it before sending).
fn header_of(block: &Block, view: u64) -> ProposalHeader {
    ProposalHeader {
        chain_id: CHAIN_ID,
        view: ViewNumber::new(view),
        block_hash: block.hash,
        height: block.height,
        data_hash: block.data_hash,
        justify: block.justify.clone(),
        tc: None,
        nec: None,
        has_validator_set_updates: false,
        leader_signature: SignatureBytes::new([0; 64]),
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
    let message = no_endorsement_bytes(CHAIN_ID, ViewNumber::new(view), ViewNumber::new(high_tip_qc_view));
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
    header_path_votes_on(replica(), block, tc, nec)
}

/// [`header_path_votes`] on a prepared replica `r`.
fn header_path_votes_on(
    mut r: Replica,
    block: &Block,
    tc: Option<TimeoutCertificate>,
    nec: Option<NoEndorsementCertificate>,
) -> bool {
    let leader = r.leader;
    let header = signed_by(ProposalHeader { tc, nec, ..header_of(block, VIEW) }, &leader);
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
    full_path_votes_on(replica(), block, tc, nec)
}

/// [`full_path_votes`] on a prepared replica `r`.
fn full_path_votes_on(
    mut r: Replica,
    block: &Block,
    tc: Option<TimeoutCertificate>,
    nec: Option<NoEndorsementCertificate>,
) -> bool {
    let leader = r.leader;
    let proposal = Proposal::signed(&key_of(&leader), CHAIN_ID, ViewNumber::new(VIEW), block.clone(), tc, nec);
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

// ---------------------------------------------------------------------------
// s134 review test gaps (fix/tc-require-tc).
// ---------------------------------------------------------------------------

/// The winner rule is strict: a tip no newer than the high QC loses (the tie
/// goes to the QC). A TC whose signed QC view equals its tip view, with the
/// flag forged to true, is not correct.
#[test]
fn tc_summary_tip_tied_with_high_qc_does_not_win() {
    let keys = keys();
    let set = validator_set(&keys);
    let (tree, _) = steady_block_tree(&set);
    let c = block(2);
    let qc = generic_pc(ViewNumber::new(VIEW - 1), block(3).hash, &keys[..3], &set);
    let tips = vec![Some(tip(&c, VIEW - 1)); 3];
    let qcs = vec![Some(qc); 3];
    let tc = collected_tc(&keys[..3], &set, VIEW - 1, &tips, &qcs);
    assert!(tc.is_correct(&tree).unwrap(), "control: collector TC is correct");
    assert!(!tc.high_tip_is_winner, "control: the collector gives a tie to the QC");

    let mut forged = tc.clone();
    forged.high_tip_is_winner = true;
    assert!(!forged.is_correct(&tree).unwrap(), "a tip tied with the high QC must not win");
}

/// The header path checks a header's TC against the HEADER's view, not the
/// replica's current view: headers bypass receive-side view filtering.
/// Replica at VIEW; a header from leader(VIEW + 2).
#[test]
fn header_path_checks_the_tc_against_the_header_view() {
    let b = block(1);
    let keys = keys();
    let set = validator_set(&keys);
    let header_view = VIEW + 2;
    // `true` iff the header passed the certificate check (the
    // ReceiveProposalHeader event is published right after it).
    let passes = |tc: TimeoutCertificate| {
        let mut r = replica();
        let vss = r.tree.validator_set_state().unwrap();
        let leader = proposer_for(ViewNumber::new(header_view), &keys, &vss, &r.tree);
        let header = signed_by(ProposalHeader { tc: Some(tc), ..header_of(&b, header_view) }, &leader);
        r.hotstuff
            .on_receive_msg(HotStuffMessage::ProposalHeader(header), &leader, &mut r.tree, &mut ValidApp)
            .unwrap();
        r.events.try_iter().any(|event| matches!(event, Event::ReceiveProposalHeader(_)))
    };
    let tips = |view: u64| vec![Some(tip(&b, view)); 3];
    let none = [None, None, None];
    let for_header = collected_tc(&keys[..3], &set, header_view - 1, &tips(header_view - 1), &none);
    let for_local = collected_tc(&keys[..3], &set, VIEW - 1, &tips(VIEW - 1), &none);
    assert!(passes(for_header), "TC for header.view - 1 must pass");
    assert!(!passes(for_local), "TC for the local view - 1 is not a TC for header.view - 1");
}

/// An NEC signed by `signers` of `set`, for `view` over `high_tip_qc_view`.
fn nec_signed_by(signers: &[SigningKey], set: &ValidatorSet, view: u64, high_tip_qc_view: u64) -> NoEndorsementCertificate {
    let message = no_endorsement_bytes(CHAIN_ID, ViewNumber::new(view), ViewNumber::new(high_tip_qc_view));
    let mut signatures = SignatureSet::new(set.len());
    for key in signers {
        let pos = set.position(&key.verifying_key()).unwrap();
        signatures.set(pos, Some(Keypair::new(key.clone()).sign(&message)));
    }
    NoEndorsementCertificate {
        view: ViewNumber::new(view),
        high_tip_qc_view: ViewNumber::new(high_tip_qc_view),
        signatures,
    }
}

/// During a validator-set change (update committed, not yet decided) the
/// leader's NE collector uses the committed set, so an NEC from the committed
/// set must validate; one from the previous set still does (as for TCs, either
/// active set). Once the update is decided only the committed set counts.
#[test]
fn nec_from_either_active_set_is_valid_during_a_validator_set_change() {
    use crate::hotstuff::types::valid_nec;
    use crate::types::update_sets::AppStateUpdates;
    use crate::types::validator_set::ValidatorSetState;

    let old_keys = signing_keys(&[1, 2, 3, 4]);
    let old_set = validator_set(&old_keys);
    let new_keys = signing_keys(&[11, 12, 13, 14]);
    let new_set = validator_set(&new_keys);
    let other_keys = signing_keys(&[21, 22, 23, 24]);
    let other_set = validator_set(&other_keys);
    let tree = |decided: bool| {
        let vss = ValidatorSetState::new(new_set.clone(), old_set.clone(), Some(BlockHeight::new(2)), decided);
        let mut tree = BlockTreeSingleton::new(MemKV::default());
        tree.initialize(&AppStateUpdates::new(), &vss).unwrap();
        tree
    };
    let from_new = nec_signed_by(&new_keys[..3], &new_set, VIEW, 1);
    let from_old = nec_signed_by(&old_keys[..3], &old_set, VIEW, 1);
    let from_other = nec_signed_by(&other_keys[..3], &other_set, VIEW, 1);

    let transition = tree(false);
    assert!(valid_nec(&from_new, CHAIN_ID, &transition).unwrap(), "committed-set NEC (what the leader collects)");
    assert!(valid_nec(&from_old, CHAIN_ID, &transition).unwrap(), "previous-set NEC");
    assert!(!valid_nec(&from_other, CHAIN_ID, &transition).unwrap(), "NEC from neither set");

    let decided = tree(true);
    assert!(valid_nec(&from_new, CHAIN_ID, &decided).unwrap(), "committed-set NEC after the update is decided");
    assert!(!valid_nec(&from_old, CHAIN_ID, &decided).unwrap(), "previous-set NEC after the update is decided");
}

// ---------------------------------------------------------------------------
// docs/plans/tc-required-and-tip-signing.md §2, VALIDPROPOSAL V1 (always on
// from phase B): a block must justify on VIEW - 1 (or the genesis PC), or
// carry a TC for VIEW - 1.
//
// RED-first (phase B): on phase A (3e5317ff) validators are lax without the
// test-only switch, so every `*_rejects_*` / `*_not_correct` case below
// fails.
// ---------------------------------------------------------------------------

/// A replica at VIEW (as [`replica`]) holding `parent` (genesis-justified).
fn replica_holding(parent: &Block) -> Replica {
    let mut r = replica();
    r.tree.insert(parent, None, None).unwrap();
    r
}

/// Does a replica holding `parent` vote for `child` carrying `tc`, on both
/// paths? Panics if the paths disagree.
fn votes_for_child_of(parent: &Block, child: &Block, tc: Option<TimeoutCertificate>) -> bool {
    let header = header_path_votes_on(replica_holding(parent), child, tc.clone(), None);
    let full = full_path_votes_on(replica_holding(parent), child, tc, None);
    assert_eq!(header, full, "header and full paths disagree (justify view {})", child.justify.view.int());
    header
}

/// The genesis-justified parent the V1 / V3 cases extend.
fn parent() -> Block {
    block(40)
}

/// A child of [`parent`] justified by a QC of `justify_view`.
fn child(justify_view: u64, tag: u8) -> Block {
    let keys = keys();
    let set = validator_set(&keys);
    let qc = generic_pc(ViewNumber::new(justify_view), parent().hash, &keys[..3], &set);
    Block::new(BlockHeight::new(1), qc, CryptoHash::new([tag; 32]), Data::new(vec![]))
}

/// Does a replica vote for a child of [`parent`] justified by a QC of
/// `justify_view`, carrying `tc`, on both paths?
fn child_votes(justify_view: u64, tc: Option<TimeoutCertificate>) -> bool {
    votes_for_child_of(&parent(), &child(justify_view, 41), tc)
}

/// A TC for `view` with no tips and no QCs (a fresh-proposal TC).
fn bare_tc(view: u64) -> TimeoutCertificate {
    let keys = keys();
    let set = validator_set(&keys);
    collected_tc(&keys[..3], &set, view, &[None, None, None], &[None, None, None])
}

#[test]
fn v1_rejects_a_stale_justify_without_a_tc() {
    assert!(!child_votes(VIEW - 3, None), "justify view VIEW - 3, no TC");
    assert!(!child_votes(VIEW - 3, Some(bare_tc(VIEW - 2))), "TC for VIEW - 2");
}

#[test]
fn v1_accepts_a_previous_view_justify_or_a_previous_view_tc() {
    assert!(child_votes(VIEW - 1, None), "justify on VIEW - 1");
    assert!(child_votes(VIEW - 3, Some(bare_tc(VIEW - 1))), "stale justify with TC(VIEW - 1)");
    // The genesis PC is exempt.
    let b = block(1);
    let header = header_path_votes_on(replica(), &b, None, None);
    let full = full_path_votes_on(replica(), &b, None, None);
    assert!(header && full, "genesis justify: header voted={header}, full voted={full}");
}

// ---------------------------------------------------------------------------
// V3 (phase B, amended): TC attached, no NEC, high QC `q` wins => the block
// justifies on a view >= q.view, or is q's block (a validator-set-update
// re-proposal of the block q certifies).
// ---------------------------------------------------------------------------

/// A case-3 TC for VIEW - 1: three signers, the first reports `high_qc`,
/// the others no QC and nobody a tip, so the high QC wins.
fn high_qc_tc(high_qc: PhaseCertificate) -> TimeoutCertificate {
    let keys = keys();
    let set = validator_set(&keys);
    let tc = collected_tc(&keys[..3], &set, VIEW - 1, &[None, None, None], &[Some(high_qc), None, None]);
    assert!(!tc.high_tip_is_winner && tc.high_qc.is_some(), "fixture: the high QC wins");
    tc
}

#[test]
fn v3_rejects_a_case_3_block_below_the_tc_high_qc() {
    let keys = keys();
    let set = validator_set(&keys);
    // q certifies another block at VIEW - 2; the proposal justifies on VIEW - 3.
    let q = generic_pc(ViewNumber::new(VIEW - 2), block(43).hash, &keys[..3], &set);
    assert!(!child_votes(VIEW - 3, Some(high_qc_tc(q))), "justify VIEW - 3 < q.view VIEW - 2");
}

#[test]
fn v3_accepts_a_case_3_block_at_or_above_the_tc_high_qc() {
    let keys = keys();
    let set = validator_set(&keys);
    let at = generic_pc(ViewNumber::new(VIEW - 2), parent().hash, &keys[..3], &set);
    assert!(child_votes(VIEW - 2, Some(high_qc_tc(at))), "justify == q");
    let below = generic_pc(ViewNumber::new(VIEW - 3), block(43).hash, &keys[..3], &set);
    assert!(child_votes(VIEW - 2, Some(high_qc_tc(below))), "justify above q");
}

/// The V3 amendment: a validator-set-update re-proposal of the block q
/// certifies justifies below q.view and must still be voted on.
#[test]
fn v3_accepts_a_reproposal_of_the_block_the_high_qc_certifies() {
    let keys = keys();
    let set = validator_set(&keys);
    let x = child(VIEW - 3, 42);
    let q = generic_pc(ViewNumber::new(VIEW - 2), x.hash, &keys[..3], &set);
    assert!(votes_for_child_of(&parent(), &x, Some(high_qc_tc(q))), "B is q's block");
}

// ---------------------------------------------------------------------------
// Phase-A open question 2: a timeout vote's highest QC is only bound by its
// (view, block) summary, so a byzantine voter can report a forged high-view
// QC. A TC whose high QC is not itself a correct QC is not correct (and the
// pacemaker drops such votes before collecting, see prev_view_collector_test).
// ---------------------------------------------------------------------------

/// A QC for `view` over `block` signed by one validator only: below quorum,
/// what a lone byzantine voter can make.
fn forged_qc(view: u64, block_hash: CryptoHash) -> PhaseCertificate {
    let keys = keys();
    let set = validator_set(&keys);
    generic_pc(ViewNumber::new(view), block_hash, &keys[..1], &set)
}

#[test]
fn tc_with_a_forged_high_qc_is_not_correct() {
    let keys = keys();
    let set = validator_set(&keys);
    let (tree, _) = steady_block_tree(&set);
    let honest = generic_pc(ViewNumber::new(VIEW - 3), parent().hash, &keys[..3], &set);
    let forged = forged_qc(VIEW - 2, block(44).hash);
    let tc = collected_tc(&keys[..3], &set, VIEW - 1, &[None, None, None], &[Some(forged.clone()), Some(honest), None]);
    assert!(tc.high_qc.as_ref() == Some(&forged), "fixture: the forged QC is the highest summary");
    assert!(!tc.is_correct(&tree).unwrap(), "a TC whose high QC is not a correct QC");
}

/// With the forged-QC TC attached, neither the block above nor the block
/// below the forged QC is voted on (the TC itself is rejected).
#[test]
fn proposal_carrying_a_forged_high_qc_tc_is_rejected() {
    let keys = keys();
    let set = validator_set(&keys);
    let honest = generic_pc(ViewNumber::new(VIEW - 3), parent().hash, &keys[..3], &set);
    let forged = forged_qc(VIEW - 2, block(44).hash);
    let tc = collected_tc(&keys[..3], &set, VIEW - 1, &[None, None, None], &[Some(forged), Some(honest), None]);
    assert!(!child_votes(VIEW - 3, Some(tc.clone())), "justify on the honest QC");
    assert!(!child_votes(VIEW - 2, Some(tc)), "justify at the forged view");
}

// ---------------------------------------------------------------------------
// Tail-fork attempt (unit, both paths): B got votes at VIEW - 1, its QC was
// withheld. A fresh block without TC(VIEW - 1) is rejected (V1); with a TC
// whose high tip is B it must be B (V2); B re-proposed under that TC passes.
// ---------------------------------------------------------------------------

#[test]
fn tail_fork_attempt_fresh_block_rejected_tip_reproposed() {
    let b = child(VIEW - 2, 45);
    let fresh = child(VIEW - 2, 46);
    let keys = keys();
    let set = validator_set(&keys);
    let tips = vec![Some(tip(&b, VIEW - 1)); 3];
    let tc = collected_tc(&keys[..3], &set, VIEW - 1, &tips, &[None, None, None]);
    assert!(!votes_for_child_of(&parent(), &fresh, None), "fresh block, no TC");
    assert!(!votes_for_child_of(&parent(), &fresh, Some(tc.clone())), "fresh block under B's TC");
    assert!(votes_for_child_of(&parent(), &b, Some(tc)), "B re-proposed under its TC");
}

#[test]
fn carries_required_certificate_table() {
    use crate::hotstuff::types::carries_required_certificate;
    let keys = keys();
    let set = validator_set(&keys);
    let qc = |view: u64| generic_pc(ViewNumber::new(view), block(40).hash, &keys[..3], &set);
    let v = ViewNumber::new(VIEW);
    assert!(carries_required_certificate(v, &qc(VIEW - 1), None));
    assert!(carries_required_certificate(v, &PhaseCertificate::genesis_pc(), None));
    assert!(!carries_required_certificate(v, &qc(VIEW - 2), None));
    assert!(carries_required_certificate(v, &qc(VIEW - 2), Some(&bare_tc(VIEW - 1))));
    assert!(!carries_required_certificate(v, &qc(VIEW - 2), Some(&bare_tc(VIEW - 2))));
    assert!(!carries_required_certificate(v, &qc(VIEW), None), "a justify for the view itself");
}

// ---------------------------------------------------------------------------
// Phase C (docs/plans/tc-required-and-tip-signing.md §3): leader-signed
// headers (F1), signed tips (F2), signed tip views (F3), tip checks before
// collecting (F4, prev_view_collector_test), local tip from the header (F5),
// and domain tags on every consensus signature.
//
// RED-first: on the phase-C skeleton (signatures carried, nothing checked,
// untagged layouts) every test in this section fails.
// ---------------------------------------------------------------------------

/// `tip` claiming `view` instead of the view its leader signed.
fn with_view(mut tip: TipInfo, view: u64) -> TipInfo {
    tip.view = ViewNumber::new(view);
    tip
}

/// F2 under the steady four-validator set.
fn tip_is_correct(tip: &TipInfo) -> bool {
    let (tree, _) = steady_block_tree(&validator_set(&keys()));
    tip.is_correct(CHAIN_ID, &tree).unwrap()
}

/// A validator that is not leader(`view`).
fn non_leader(view: u64) -> SigningKey {
    let leader = select_leader(ViewNumber::new(view), &validator_set(&keys()));
    keys().into_iter().find(|key| key.verifying_key() != leader).unwrap()
}

#[test]
fn f2_honest_tip_is_correct() {
    assert!(tip_is_correct(&tip(&block(1), VIEW - 1)));
    assert!(tip_is_correct(&tip(&child(VIEW - 3, 60), VIEW - 2)));
}

#[test]
fn f2_tip_with_a_forged_view_is_not_correct() {
    let honest = tip(&block(1), VIEW - 3);
    assert!(!tip_is_correct(&with_view(honest, VIEW - 1)), "view the leader did not sign");
}

#[test]
fn f2_tip_whose_header_fields_do_not_hash_to_its_block_is_not_correct() {
    let keys = keys();
    let set = validator_set(&keys);
    let honest = tip(&child(VIEW - 3, 61), VIEW - 2);
    let mut justify = honest.clone();
    justify.block_justify = generic_pc(ViewNumber::new(VIEW - 4), parent().hash, &keys[..3], &set);
    assert!(!tip_is_correct(&justify), "block_justify that is not the block's justify");
    let mut height = honest.clone();
    height.block_height = BlockHeight::new(7);
    assert!(!tip_is_correct(&height), "height that is not the block's");
    let mut data = honest;
    data.block_data_hash = CryptoHash::new([62; 32]);
    assert!(!tip_is_correct(&data), "data hash that is not the block's");
}

#[test]
fn f2_tip_signed_by_a_non_leader_is_not_correct() {
    let b = block(1);
    let view = VIEW - 1;
    let forged = TipInfo::from_header(&header_of(&b, view).signed(&non_leader(view)));
    assert!(!tip_is_correct(&forged));
}

/// A voter re-signs its own timeout vote with a forged tip view: its vote is
/// correctly signed, but the tip fails F2, so the TC is not correct and the
/// re-proposal it would force is rejected.
#[test]
fn f2_tc_carrying_a_tip_with_a_forged_view_is_rejected() {
    let keys = keys();
    let set = validator_set(&keys);
    let b = block(1);
    let forged = with_view(tip(&b, VIEW - 3), VIEW - 1);
    let tips = vec![Some(forged.clone()), Some(forged.clone()), Some(forged)];
    let tc = collected_tc(&keys[..3], &set, VIEW - 1, &tips, &[None, None, None]);
    let (tree, _) = steady_block_tree(&set);
    assert!(!tc.is_correct(&tree).unwrap(), "high tip fails F2");
    assert_both_paths("re-proposal under a TC with a forged tip view", &b, (Some(tc), None), false);
}

/// F3: the TC's high tip must be the highest tip view any signer signed. A
/// relayer swapping in a lower (genuine, signed) tip must be caught.
#[test]
fn f3_tc_whose_high_tip_is_not_the_highest_signed_view_is_not_correct() {
    let keys = keys();
    let set = validator_set(&keys);
    let (tree, _) = steady_block_tree(&set);
    let low = tip(&block(2), VIEW - 3);
    let high = tip(&block(1), VIEW - 1);
    let tc = collected_tc(&keys[..3], &set, VIEW - 1, &[Some(low.clone()), Some(high.clone()), None], &[None, None, None]);
    assert!(tc.high_tip.as_ref() == Some(&high), "fixture: the collector keeps the highest tip");
    assert!(tc.is_correct(&tree).unwrap(), "control: collector TC is correct");
    let mut lower = tc.clone();
    lower.high_tip = Some(low);
    assert!(!lower.is_correct(&tree).unwrap(), "a lower signed tip is not the high tip");
    assert_both_paths("re-proposal of the lower tip", &block(2), (Some(lower), None), false);
}

/// F3: the tip view is signed. The signers reported B at VIEW - 3; B was
/// also (genuinely) proposed at VIEW - 1, but nobody signed that tip: a
/// relayer cannot raise the high tip's view to it.
#[test]
fn f3_tc_high_tip_view_must_be_the_signed_view() {
    let keys = keys();
    let set = validator_set(&keys);
    let (tree, _) = steady_block_tree(&set);
    let b = block(1);
    let tips = vec![Some(tip(&b, VIEW - 3)); 3];
    let tc = collected_tc(&keys[..3], &set, VIEW - 1, &tips, &[None, None, None]);
    assert!(tc.is_correct(&tree).unwrap(), "control: collector TC is correct");
    let mut raised = tc.clone();
    raised.high_tip = Some(tip(&b, VIEW - 1));
    assert!(tip_is_correct(raised.high_tip.as_ref().unwrap()), "fixture: the raised tip passes F2");
    assert!(!raised.is_correct(&tree).unwrap(), "a tip view no signer signed");
}

/// F3: the tip view is bound by each signer's signature, not only by the
/// metadata next to it: a relayer rewriting both the high tip and every
/// signer's metadata to a later (genuine) header of the same block breaks
/// the signatures.
#[test]
fn f3_tip_view_is_bound_by_the_signatures() {
    let keys = keys();
    let set = validator_set(&keys);
    let (tree, _) = steady_block_tree(&set);
    let b = block(1);
    let tips = vec![Some(tip(&b, VIEW - 3)); 3];
    let tc = collected_tc(&keys[..3], &set, VIEW - 1, &tips, &[None, None, None]);
    let mut rewritten = tc.clone();
    let raised = tip(&b, VIEW - 1);
    rewritten.high_tip = Some(raised.clone());
    for (summary, _) in rewritten.voter_metadata.iter_mut() {
        if summary.is_some() {
            *summary = Some(raised.summary());
        }
    }
    assert!(!rewritten.is_correct(&tree).unwrap(), "tip views the signers did not sign");
}

/// F3: a signed tip can be no newer than the view the TC times out.
#[test]
fn f3_tc_with_a_tip_from_a_later_view_is_not_correct() {
    let keys = keys();
    let set = validator_set(&keys);
    let (tree, _) = steady_block_tree(&set);
    let future = tip(&block(1), VIEW + 1);
    let tc = collected_tc(&keys[..3], &set, VIEW - 1, &[Some(future), None, None], &[None, None, None]);
    assert!(!tc.is_correct(&tree).unwrap(), "tip view VIEW + 1 in TC(VIEW - 1)");
}

/// F1: a header (or full proposal) must carry its proposer's signature over
/// its own chain, view and block.
#[test]
fn f1_proposal_without_its_leaders_signature_is_rejected() {
    let b = block(1);
    let leader_key = key_of(&replica().leader);
    let view = ViewNumber::new(VIEW);
    let cases = [
        ("signed by another validator", sign_proposal_header(&non_leader(VIEW), CHAIN_ID, view, &b.hash)),
        ("leader signature for another view", sign_proposal_header(&leader_key, CHAIN_ID, view - 1, &b.hash)),
        ("leader signature for another chain", sign_proposal_header(&leader_key, ChainID::new(1), view, &b.hash)),
        ("leader signature for another block", sign_proposal_header(&leader_key, CHAIN_ID, view, &block(2).hash)),
    ];
    for (name, signature) in cases {
        let mut r = replica();
        let leader = r.leader;
        let header = ProposalHeader { leader_signature: signature, ..header_of(&b, VIEW) };
        r.hotstuff.on_receive_msg(HotStuffMessage::ProposalHeader(header), &leader, &mut r.tree, &mut ValidApp).unwrap();
        let body = BlockDataResponse { view, block: b.clone() };
        r.hotstuff.on_receive_msg(HotStuffMessage::BlockDataResponse(body), &leader, &mut r.tree, &mut ValidApp).unwrap();
        assert!(!r.voted(), "header path, {name}");

        let mut r = replica();
        let mut proposal = Proposal::signed(&leader_key, CHAIN_ID, view, b.clone(), None, None);
        proposal.leader_signature = signature;
        r.hotstuff.on_receive_msg(HotStuffMessage::Proposal(proposal), &leader, &mut r.tree, &mut ValidApp).unwrap();
        assert!(!r.voted(), "full path, {name}");
    }
}

/// F1: an unsigned header cannot be used to frame the leader: it is not
/// recorded as an equivocation.
#[test]
fn f1_unsigned_conflicting_header_is_not_equivocation() {
    let mut r = replica();
    let leader = r.leader;
    let b = block(1);
    let c = block(2);
    let honest = signed_by(header_of(&b, VIEW), &leader);
    r.hotstuff.on_receive_msg(HotStuffMessage::ProposalHeader(honest), &leader, &mut r.tree, &mut ValidApp).unwrap();
    let mut forged = header_of(&c, VIEW);
    forged.leader_signature = sign_proposal_header(&non_leader(VIEW), CHAIN_ID, ViewNumber::new(VIEW), &c.hash);
    r.hotstuff.on_receive_msg(HotStuffMessage::ProposalHeader(forged), &leader, &mut r.tree, &mut ValidApp).unwrap();
    assert!(
        !r.events.try_iter().any(|event| matches!(event, Event::EquivocationDetected(_))),
        "a header the leader did not sign is not its equivocation"
    );
}

/// F5: voting for a fresh header sets the local tip to that signed header.
#[test]
fn f5_header_vote_sets_the_local_tip_from_the_header() {
    let r = replica();
    let leader = r.leader;
    let b = block(1);
    assert!(header_path_votes_on_tree(r, &b, None, None, |tree| {
        let expected = TipInfo::from_header(&signed_by(header_of(&b, VIEW), &leader));
        assert!(tree.local_tip().unwrap() == Some(expected), "local tip is the voted header");
    }));
}

/// F5: voting for a re-proposal keeps the (verified) TC high tip, with its
/// original view and leader signature.
#[test]
fn f5_reproposal_vote_copies_the_tc_high_tip() {
    let b = block(1);
    let tc = reproposal_tc(&b);
    let high_tip = tc.high_tip.clone().unwrap();
    assert!(header_path_votes_on_tree(replica(), &b, Some(tc), None, |tree| {
        assert!(tree.local_tip().unwrap() == Some(high_tip.clone()), "local tip is the TC's high tip");
    }));
}

/// [`header_path_votes_on`], then `check` the replica's block tree.
fn header_path_votes_on_tree(
    r: Replica,
    block: &Block,
    tc: Option<TimeoutCertificate>,
    nec: Option<NoEndorsementCertificate>,
    check: impl FnOnce(&BlockTreeSingleton<MemKV>),
) -> bool {
    let mut r = r;
    let leader = r.leader;
    let header = signed_by(ProposalHeader { tc, nec, ..header_of(block, VIEW) }, &leader);
    r.hotstuff
        .on_receive_msg(HotStuffMessage::ProposalHeader(header), &leader, &mut r.tree, &mut ValidApp)
        .unwrap();
    let body = BlockDataResponse { view: ViewNumber::new(VIEW), block: block.clone() };
    r.hotstuff
        .on_receive_msg(HotStuffMessage::BlockDataResponse(body), &leader, &mut r.tree, &mut ValidApp)
        .unwrap();
    let voted = r.voted();
    check(&r.tree);
    voted
}

/// F5: a nudge no longer writes the local tip (it stored the nudge's QC as
/// the block's justify, breaking F2's hash recompute).
#[test]
fn f5_nudge_does_not_set_the_local_tip() {
    use crate::hotstuff::messages::Nudge;
    use crate::hotstuff::types::Phase;
    use crate::types::update_sets::ValidatorSetUpdates;
    use crate::types::data_types::Power;
    let keys = keys();
    let set = validator_set(&keys);
    let mut r = replica();
    let vsu = block(70);
    let mut updates = ValidatorSetUpdates::new();
    updates.insert(keys[0].verifying_key(), Power::new(2));
    r.tree.insert(&vsu, None, Some(&updates)).unwrap();
    let message = phase_vote_bytes(CHAIN_ID, ViewNumber::new(VIEW - 1), &vsu.hash, Phase::Prepare);
    let mut signatures = SignatureSet::new(set.len());
    for key in &keys[..3] {
        signatures.set(set.position(&key.verifying_key()).unwrap(), Some(Keypair::new(key.clone()).sign(&message)));
    }
    let prepare = PhaseCertificate {
        chain_id: CHAIN_ID,
        view: ViewNumber::new(VIEW - 1),
        block: vsu.hash,
        phase: Phase::Prepare,
        signatures,
    };
    let leader = r.leader;
    r.hotstuff
        .on_receive_msg(HotStuffMessage::Nudge(Nudge::new(CHAIN_ID, ViewNumber::new(VIEW), prepare)), &leader, &mut r.tree, &mut ValidApp)
        .unwrap();
    assert!(r.voted(), "fixture: the replica votes Precommit on the nudge");
    assert!(r.tree.local_tip().unwrap().is_none(), "a nudge must not set the local tip");
}

// Domain tags ---------------------------------------------------------------

/// The bytes of every kind of consensus signature, over field values chosen
/// so that the untagged layouts coincided (header = timeout vote with tip;
/// NE `(0, VIEW)` = bare timeout vote `(chain 0, VIEW)`).
fn every_signed_layout() -> Vec<(&'static str, Vec<u8>)> {
    use crate::hotstuff::types::Phase;
    use crate::types::signing::{phase_vote_bytes, proposal_header_bytes, timeout_vote_bytes};
    let (chain, view, hash) = (CHAIN_ID, ViewNumber::new(VIEW), CryptoHash::new([7; 32]));
    vec![
        ("header", proposal_header_bytes(chain, view, &hash)),
        ("phase vote", phase_vote_bytes(chain, view, &hash, Phase::Generic)),
        ("bare timeout vote", timeout_vote_bytes(chain, view, None, None)),
        ("timeout vote with tip", timeout_vote_bytes(chain, view, Some((hash, view)), None)),
        ("timeout vote with QC", timeout_vote_bytes(chain, view, None, Some((view, hash)))),
        ("timeout vote with tip and QC", timeout_vote_bytes(chain, view, Some((hash, view)), Some((view, hash)))),
        ("no-endorsement", no_endorsement_bytes(chain, ViewNumber::new(0), view)),
    ]
}

/// No signature of one kind verifies as any other kind's message.
#[test]
fn every_signature_type_is_domain_separated() {
    let all = every_signed_layout();
    for (i, (kind, bytes)) in all.iter().enumerate() {
        for (j, (other, other_bytes)) in all.iter().enumerate() {
            if i != j {
                assert!(bytes != other_bytes, "{kind} and {other} sign the same bytes");
            }
        }
    }
}

/// A leader's header signature replayed as its timeout vote for the same
/// view with that block as its tip (the untagged layouts were identical).
#[test]
fn header_signature_replayed_as_a_timeout_vote_is_rejected() {
    use crate::pacemaker::messages::TimeoutVote;
    use crate::types::signed_messages::SignedMessage;
    let b = block(1);
    let leader = key_of(&select_leader(ViewNumber::new(VIEW), &validator_set(&keys())));
    let header = header_of(&b, VIEW).signed(&leader);
    let replayed = TimeoutVote {
        chain_id: CHAIN_ID,
        view: ViewNumber::new(VIEW),
        signature: header.leader_signature,
        highest_tc: None,
        local_tip: Some(TipInfo::from_header(&header)),
        highest_qc: None,
    };
    assert!(!replayed.is_correct(&leader.verifying_key()), "header signature accepted as a timeout vote");
}

/// An NE replayed as a bare timeout vote: NE `(view, qc_view)` and a bare
/// vote `(chain_id, view)` were both untagged u64 pairs.
#[test]
fn no_endorsement_replayed_as_a_timeout_vote_is_rejected() {
    use crate::pacemaker::messages::TimeoutVote;
    use crate::types::signed_messages::SignedMessage;
    let key = keys()[0].clone();
    // NE for recovery view 0 over QC view VIEW, signed for CHAIN_ID (0).
    let ne = Keypair::new(key.clone()).sign(&no_endorsement_bytes(CHAIN_ID, ViewNumber::new(0), ViewNumber::new(VIEW)));
    let replayed = TimeoutVote {
        chain_id: ChainID::new(0),
        view: ViewNumber::new(VIEW),
        signature: ne,
        highest_tc: None,
        local_tip: None,
        highest_qc: None,
    };
    assert!(!replayed.is_correct(&key.verifying_key()), "NE accepted as a timeout vote");
}

// Cost --------------------------------------------------------------------

/// Phase C per-block cost (report, not a check): one header signature per
/// proposal, one header verification per received header or proposal, and
/// F2 (block-hash recompute + one or two verifications) per tip in a timeout
/// vote and per TC high tip. Run in release:
/// `cargo nextest run --release -p hotstuff_rs -E 'test(tip_signing_cost)' --run-ignored only --no-capture`.
#[test]
#[ignore]
fn tip_signing_cost_report() {
    use std::time::Instant;
    const N: u32 = 2000;
    let keys = keys();
    let set = validator_set(&keys);
    let (tree, _) = steady_block_tree(&set);
    let b = child(VIEW - 2, 63);
    let leader = key_of(&select_leader(ViewNumber::new(VIEW - 1), &set));
    let view = ViewNumber::new(VIEW - 1);
    let time = |name: &str, f: &mut dyn FnMut()| {
        f();
        let start = Instant::now();
        for _ in 0..N {
            f();
        }
        let per = start.elapsed() / N;
        eprintln!("tip_signing_cost {name}: {per:?} per op");
    };
    let mut signature = SignatureBytes::new([0; 64]);
    time("sign header (leader, per proposal)", &mut || {
        signature = sign_proposal_header(&leader, CHAIN_ID, view, &b.hash);
    });
    let header = ProposalHeader { leader_signature: signature, ..header_of(&b, VIEW - 1) };
    time("verify header signature (per header / proposal)", &mut || {
        assert!(header.is_signed_by(&leader.verifying_key()));
    });
    time("Block::hash recompute (3-of-4 QC justify)", &mut || {
        assert!(Block::hash(b.height, &b.justify, &b.data_hash) == b.hash);
    });
    let t = TipInfo::from_header(&header);
    time("F2 TipInfo::is_correct (per vote tip, per TC high tip)", &mut || {
        assert!(t.is_correct(CHAIN_ID, &tree).unwrap());
    });
    let with_tip = reproposal_tc(&b);
    let without_tip = bare_tc(VIEW - 1);
    time("TimeoutCertificate::is_correct, 3 signers, no tip", &mut || {
        assert!(without_tip.is_correct(&tree).unwrap());
    });
    time("TimeoutCertificate::is_correct, 3 signers, high tip (F2 + F3)", &mut || {
        assert!(with_tip.is_correct(&tree).unwrap());
    });
}
