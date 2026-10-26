//! s140 (progress_and_validator_set_update_test: the [1, 1, 1] stall alone, and under load the
//! steady-state debug assert "locally-collected steady-state PC must be correct by construction"):
//! a Decide PC collected by a replica that has not committed the update it decides.
//!
//! The singleton set {n0} adds n1 and n2 with one update U. n0 alone certifies every phase: it
//! commits U on its Commit PC (view 9) and votes Decide in view 10. Its Decide vote goes to the
//! leader of view 11 in the new set, n2. n2 learnt the Commit PC only by AdvanceView, while U's
//! body was still in flight, so its committed set is still {n0} (decided, no update window): its
//! collector pair is a lone collector over {n0}. The pair is phase-blind, so n0's one Decide vote
//! is an {n0} quorum and completes a Decide PC with one signature. But only the post-update set
//! votes Decide (`roles::is_phase_voter`): a Decide PC on U carries a quorum of {n0, n1, n2}.
//!
//! - U not in the tree yet (its header pending): `is_correct` checks a PC on an unknown block
//!   against the committed set {n0}, so the PC passed. n2 took it as its highest PC (view 10).
//!   Once U's body arrived and U committed, every replica (n2 too) rejected it, so n2's proposal
//!   for view 11 was dropped by all, and the correct three-signature Decide PC of view 10, from
//!   n0's AdvanceView, never replaced it (same view): no PC after view 9, 120 s.
//! - U in the tree, Pending: `is_correct` takes the Decide-on-pending-update path (post-update set)
//!   and the PC fails: the debug assert aborts; a release build acts on the PC (commits U on it,
//!   adopts a highest PC no other replica accepts).
//!
//! A locally collected Decide PC is correct by construction only when this replica has committed
//! the update (its committed set is then the post-update set): otherwise it is dropped.

use crate::app::{App, ProduceBlockRequest, ProduceBlockResponse, ValidateBlockRequest, ValidateBlockResponse};
use crate::block_tree::accessors::internal::BlockTreeSingleton;
use crate::hotstuff::header_fast_path_regression_test::{
    generic_pc, hotstuff_at, signing_keys, steady_block_tree, validator_set, MemKV,
};
use crate::hotstuff::messages::{HotStuffMessage, PhaseVote, ProposalHeader};
use crate::hotstuff::types::{Phase, PhaseCertificate, PhaseVoteCollector};
use crate::test_keys::signed_by;
use crate::types::block::Block;
use crate::types::crypto_primitives::{Keypair, SigningKey};
use crate::types::data_types::{BlockHeight, ChainID, CryptoHash, Data, Power, SignatureBytes, ViewNumber};
use crate::types::signed_messages::Collector;
use crate::types::update_sets::ValidatorSetUpdates;
use crate::types::validator_set::ValidatorSet;

const CHAIN_ID: ChainID = ChainID::new(0);

/// Bodies stay in flight: `validate_block` answers `MissingData`, so a header's block is not
/// inserted and stays pending.
struct InFlightApp;

impl App<MemKV> for InFlightApp {
    fn produce_block(&mut self, _request: ProduceBlockRequest<MemKV>) -> ProduceBlockResponse {
        unreachable!("no replica produces a block in this test")
    }
    fn validate_block(&mut self, _request: ValidateBlockRequest<MemKV>) -> ValidateBlockResponse {
        ValidateBlockResponse::MissingData
    }
    fn validate_block_for_sync(&mut self, _request: ValidateBlockRequest<MemKV>) -> ValidateBlockResponse {
        unreachable!("no block sync in this test")
    }
}

fn vote(key: &SigningKey, view: u64, block: CryptoHash, phase: Phase) -> PhaseVote {
    PhaseVote::new(&Keypair::new(key.clone()), CHAIN_ID, ViewNumber::new(view), block, phase)
}

fn certify(view: u64, block: CryptoHash, phase: Phase, keys: &[SigningKey], set: &ValidatorSet) -> PhaseCertificate {
    let mut collector = PhaseVoteCollector::new(CHAIN_ID, ViewNumber::new(view), set.clone());
    keys.iter()
        .filter_map(|key| collector.collect(&key.verifying_key(), vote(key, view, block, phase)))
        .next()
        .expect("the votes form a quorum")
}

/// The update shape of the test: {n0} adds n1 and n2.
struct Shape {
    keys: Vec<SigningKey>,
    old_set: ValidatorSet,
    new_set: ValidatorSet,
    updates: ValidatorSetUpdates,
    a: Block,
    qc_a: PhaseCertificate,
    u: Block,
    /// U's Commit PC (view 9), by n0 alone.
    commit: PhaseCertificate,
}

fn shape() -> Shape {
    let keys = signing_keys(&[1, 2, 3]);
    let old_set = validator_set(&keys[..1]);
    let mut updates = ValidatorSetUpdates::new();
    updates.insert(keys[1].verifying_key(), Power::new(1));
    updates.insert(keys[2].verifying_key(), Power::new(1));
    let mut new_set = old_set.clone();
    new_set.apply_updates(&updates);
    let a = Block::new(BlockHeight::new(0), PhaseCertificate::genesis_pc(), CryptoHash::new([10; 32]), Data::new(vec![]));
    let qc_a = generic_pc(ViewNumber::new(6), a.hash, &keys[..1], &old_set);
    let u = Block::new(BlockHeight::new(1), qc_a.clone(), CryptoHash::new([11; 32]), Data::new(vec![]));
    let commit = certify(9, u.hash, Phase::Commit, &keys[..1], &old_set);
    Shape { keys, old_set, new_set, updates, a, qc_a, u, commit }
}

/// n2's tree: A committed-chain tip, U's Commit PC learnt by AdvanceView only (highest PC, nothing
/// committed), and U itself in the tree (`u_in_tree`) or not.
fn n2_tree(s: &Shape, u_in_tree: bool) -> BlockTreeSingleton<MemKV> {
    let (mut tree, _) = steady_block_tree(&s.old_set);
    tree.insert(&s.a, None, None).unwrap();
    tree.update(&s.qc_a, &None).unwrap();
    if u_in_tree {
        tree.insert(&s.u, None, Some(&s.updates)).unwrap();
    }
    tree.advance_highest_pc_from_remote(&s.commit, &None).unwrap();
    let state = tree.validator_set_state().unwrap();
    assert!(
        state.update_decided() && state.committed_validator_set() == &s.old_set,
        "fixture: n2 still holds the old set, decided (no update window)"
    );
    tree
}

/// The header announcing U in view 7, signed by n0.
fn u_header(s: &Shape) -> ProposalHeader {
    let header = ProposalHeader {
        chain_id: CHAIN_ID,
        view: ViewNumber::new(7),
        block_hash: s.u.hash,
        height: s.u.height,
        data_hash: s.u.data_hash,
        justify: s.u.justify.clone(),
        tc: None,
        nec: None,
        has_validator_set_updates: true,
        leader_signature: SignatureBytes::new([0; 64]),
    };
    signed_by(header, &s.keys[0].verifying_key())
}

fn assert_highest_pc_is_the_commit_pc(tree: &BlockTreeSingleton<MemKV>, s: &Shape) {
    let highest = tree.highest_pc().unwrap();
    assert_eq!(
        (highest.phase, highest.view.int()),
        (Phase::Commit, 9),
        "n0's lone Decide vote must not make a Decide PC here: a Decide PC on U needs a quorum of \
         the post-update set ({} validators), got {} signature(s) over a set of {}",
        s.new_set.len(),
        highest.signatures.iter().filter(|sig| sig.is_some()).count(),
        highest.signatures.len(),
    );
}

/// U's body in flight (header pending): the old-set Decide PC used to pass `is_correct` (unknown
/// block, committed set) and became n2's highest PC.
#[test]
fn old_set_decide_pc_on_an_update_whose_body_is_in_flight_is_dropped() {
    let s = shape();
    let mut tree = n2_tree(&s, false);
    let n0 = s.keys[0].verifying_key();
    let mut hotstuff = hotstuff_at(ViewNumber::new(10), s.keys[2].clone(), tree.validator_set_state().unwrap());
    hotstuff
        .on_receive_msg(HotStuffMessage::ProposalHeader(u_header(&s)), &n0, &mut tree, &mut InFlightApp)
        .expect("processing U's header must not error");
    assert!(!tree.contains(&s.u.hash), "fixture: U's body is still in flight");

    hotstuff
        .on_receive_msg(HotStuffMessage::PhaseVote(vote(&s.keys[0], 10, s.u.hash, Phase::Decide)), &n0, &mut tree, &mut InFlightApp)
        .expect("processing a vote must not error");
    assert_highest_pc_is_the_commit_pc(&tree, &s);
    assert!(tree.validator_set_state().unwrap().committed_validator_set() == &s.old_set, "U is not committed by it");
}

/// U in the tree, Pending: the old-set Decide PC fails `is_correct` (Decide on a pending update:
/// post-update set). Debug builds aborted on the steady-state assert; release builds acted on it.
#[test]
fn old_set_decide_pc_on_a_pending_update_is_dropped() {
    let s = shape();
    let mut tree = n2_tree(&s, true);
    let n0 = s.keys[0].verifying_key();
    let mut hotstuff = hotstuff_at(ViewNumber::new(10), s.keys[2].clone(), tree.validator_set_state().unwrap());
    hotstuff
        .on_receive_msg(HotStuffMessage::PhaseVote(vote(&s.keys[0], 10, s.u.hash, Phase::Decide)), &n0, &mut tree, &mut InFlightApp)
        .expect("processing a vote must not error");
    assert_highest_pc_is_the_commit_pc(&tree, &s);
    assert!(tree.validator_set_state().unwrap().committed_validator_set() == &s.old_set, "U is not committed by it");
}

/// Control: a replica that committed U (the update window: post-update committed set) still
/// collects the post-update set's Decide votes into U's Decide PC and applies it.
#[test]
fn decide_pc_collected_after_committing_the_update_is_applied() {
    let s = shape();
    let (mut tree, _) = steady_block_tree(&s.old_set);
    tree.insert(&s.a, None, None).unwrap();
    tree.update(&s.qc_a, &None).unwrap();
    tree.insert(&s.u, None, Some(&s.updates)).unwrap();
    tree.update(&s.commit, &None).unwrap();
    let state = tree.validator_set_state().unwrap();
    assert!(!state.update_decided() && state.committed_validator_set() == &s.new_set, "fixture: U committed, undecided");

    let mut hotstuff = hotstuff_at(ViewNumber::new(10), s.keys[2].clone(), state);
    for key in &s.keys {
        hotstuff
            .on_receive_msg(HotStuffMessage::PhaseVote(vote(key, 10, s.u.hash, Phase::Decide)), &key.verifying_key(), &mut tree, &mut InFlightApp)
            .expect("processing a vote must not error");
    }
    let highest = tree.highest_pc().unwrap();
    assert_eq!((highest.phase, highest.view.int(), highest.signatures.len()), (Phase::Decide, 10, 3), "U's Decide PC is formed and applied");
    assert!(tree.validator_set_state().unwrap().update_decided(), "U is decided");
}
