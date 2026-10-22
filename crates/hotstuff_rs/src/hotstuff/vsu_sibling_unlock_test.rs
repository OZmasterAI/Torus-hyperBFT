//! s138 (Codex torus-adversarial P1 on 8af2b949): a conflicting validator-set-update sibling U'
//! must not strand the honest replicas when one of them is locked on U.
//!
//! 4 equal validators, one Byzantine (leader of view 3). Prepare(U, 2) and Precommit(U, 3) form;
//! only the backup aggregator, leader(5), learns Precommit(U, 3) and locks on U. The Byzantine
//! leader signs a header of U' (a sibling of U on the same parent and justify) for its view 3
//! and reports it as its tip, so TC(3) (two unlocked honest replicas reporting Prepare(U, 2),
//! plus the Byzantine one) is valid under V1-V4 and its high tip wins. Honest leader(4)
//! re-proposes U'; the locked replica refuses, the other two and the Byzantine one certify
//! Prepare(U', 4), which reaches everyone. View 5's nudge is lost.
//!
//! From then on every TC's high QC is Prepare(U', 4), so leaders re-propose U' with its stale
//! justify QC(A, 1) (`repropose_block`; V3 admits "the high QC's block"). The replica locked on
//! Precommit(U, 3) refused it forever (the justify is older than its lock and does not extend U,
//! and U' is not its locked block), and with the Byzantine one withholding, two honest votes
//! never make a quorum of three.
//!
//! The fix (standard HotStuff unlock, `safe_proposal_lock`): a proposed block that a PC newer
//! than the lock certifies (here Prepare(U', 4), view 4 > 3, the TC's high QC and this replica's
//! highest PC) satisfies the lock, as a block justified by such a PC does.

use crate::app::{App, ProduceBlockRequest, ProduceBlockResponse, ValidateBlockRequest, ValidateBlockResponse};
use crate::block_tree::accessors::internal::BlockTreeSingleton;
use crate::block_tree::invariants::repropose_block;
use crate::events::Event;
use crate::hotstuff::header_fast_path_regression_test::{
    generic_pc, hotstuff_at_with_events, proposer_for, signing_keys, steady_block_tree, validator_set, MemKV,
};
use crate::hotstuff::messages::{HotStuffMessage, NewView, Nudge, PhaseVote, ProposalHeader};
use crate::hotstuff::types::{valid_proposal_certificates, Phase, PhaseCertificate, PhaseVoteCollector};
use crate::pacemaker::types::{TimeoutCertificate, TimeoutVoteCollector, TipInfo};
use crate::test_keys::signed_by;
use crate::types::block::Block;
use crate::types::crypto_primitives::{Keypair, SigningKey, VerifyingKey};
use crate::types::data_types::{BlockHeight, ChainID, CryptoHash, Data, Power, SignatureBytes, ViewNumber};
use crate::types::signed_messages::Collector;
use crate::types::update_sets::ValidatorSetUpdates;
use crate::types::validator_set::ValidatorSet;

const CHAIN_ID: ChainID = ChainID::new(0);

/// Every block under test is already in the tree, so no body is validated.
struct TreeApp;

impl App<MemKV> for TreeApp {
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

/// The header announcing the validator-set-updating `block` in `view`, carrying `tc`.
fn header(block: &Block, view: u64, tc: Option<TimeoutCertificate>) -> ProposalHeader {
    ProposalHeader {
        chain_id: CHAIN_ID,
        view: ViewNumber::new(view),
        block_hash: block.hash,
        height: block.height,
        data_hash: block.data_hash,
        justify: block.justify.clone(),
        tc,
        nec: None,
        has_validator_set_updates: true,
        leader_signature: SignatureBytes::new([0; 64]),
    }
}

/// `key`'s replica (block tree `tree`) processes `msg` from `origin` in `view`; returns the phase
/// votes it cast.
fn run(
    tree: &mut BlockTreeSingleton<MemKV>,
    key: &SigningKey,
    view: u64,
    msg: HotStuffMessage,
    origin: &VerifyingKey,
) -> Vec<PhaseVote> {
    let (mut hotstuff, events) =
        hotstuff_at_with_events(ViewNumber::new(view), key.clone(), tree.validator_set_state().unwrap());
    hotstuff.on_receive_msg(msg, origin, tree, &mut TreeApp).expect("processing must not error");
    events
        .try_iter()
        .filter_map(|event| match event {
            Event::PhaseVote(event) => Some(event.vote),
            _ => None,
        })
        .collect()
}

/// The PC that `votes` form in `view` (as the `PhaseVoteCollector` forms it).
fn certify(view: u64, votes: Vec<(SigningKey, PhaseVote)>, set: &ValidatorSet) -> PhaseCertificate {
    let mut collector = PhaseVoteCollector::new(CHAIN_ID, ViewNumber::new(view), set.clone());
    votes
        .into_iter()
        .filter_map(|(key, vote)| collector.collect(&key.verifying_key(), vote))
        .next()
        .expect("the votes form a quorum")
}

/// TC(`view`) from timeout votes, each signer reporting its tip and highest QC.
fn timeout_certificate(
    view: u64,
    entries: Vec<(SigningKey, Option<TipInfo>, Option<PhaseCertificate>)>,
    set: &ValidatorSet,
) -> TimeoutCertificate {
    use crate::pacemaker::messages::PacemakerMessage;
    let mut collector = TimeoutVoteCollector::new(CHAIN_ID, ViewNumber::new(view), set.clone());
    let mut tc = None;
    for (key, tip, qc) in entries {
        let PacemakerMessage::TimeoutVote(vote) =
            PacemakerMessage::timeout_vote(&Keypair::new(key.clone()), CHAIN_ID, ViewNumber::new(view), None, tip, qc)
        else {
            unreachable!()
        };
        tc = collector.collect(&key.verifying_key(), vote).or(tc);
    }
    tc.expect("the timeout votes form a quorum")
}

/// The Codex P1 trace (every certificate is real: signed votes the replicas cast, a TC that
/// passes V1-V4), then a synchronous suffix: the Byzantine validator is silent (no votes, no
/// proposals in the views it leads), every honest leader does what `propose_from_highest_pc`
/// does (re-propose the highest PC's block under TC(v-1), or nudge it), and every PC the honest
/// votes form reaches every honest replica. All three honest replicas must commit U'.
///
/// RED before the fix: from view 6 on, every re-proposal of U' gets two honest votes (the
/// replica locked on Precommit(U, 3) refuses), no PC forms, and nothing is ever committed.
#[test]
fn locked_replica_votes_for_a_sibling_certified_above_its_lock() {
    let keys = signing_keys(&[1, 2, 3, 4]);
    let set = validator_set(&keys);
    let (template, vss) = steady_block_tree(&set);
    let byzantine_vk = proposer_for(ViewNumber::new(3), &keys, &vss, &template);
    let byzantine = keys.iter().find(|k| k.verifying_key() == byzantine_vk).unwrap().clone();
    let honest: Vec<SigningKey> = keys.iter().filter(|k| k.verifying_key() != byzantine_vk).cloned().collect();

    // A <- U and A <- U' (siblings on the same justify QC(A, 1), both validator-set-updating).
    let a = Block::new(BlockHeight::new(0), PhaseCertificate::genesis_pc(), CryptoHash::new([10; 32]), Data::new(vec![]));
    let qc_a = generic_pc(ViewNumber::new(1), a.hash, &honest, &set);
    let u = Block::new(BlockHeight::new(1), qc_a.clone(), CryptoHash::new([11; 32]), Data::new(vec![]));
    let u_prime = Block::new(BlockHeight::new(1), qc_a.clone(), CryptoHash::new([12; 32]), Data::new(vec![]));
    let mut trees: Vec<BlockTreeSingleton<MemKV>> = honest
        .iter()
        .map(|_| {
            let (mut tree, _) = steady_block_tree(&set);
            tree.insert(&a, None, None).unwrap();
            tree.update(&qc_a, &None).unwrap();
            let mut updates = ValidatorSetUpdates::new();
            updates.insert(honest[0].verifying_key(), Power::new(2));
            tree.insert(&u, None, Some(&updates)).unwrap();
            tree.insert(&u_prime, None, Some(&updates)).unwrap();
            tree
        })
        .collect();

    // Every honest replica casts Prepare(U, 2) and Precommit(U, 3).
    let leader2 = proposer_for(ViewNumber::new(2), &keys, &vss, &trees[0]);
    let mut votes = Vec::new();
    for (i, tree) in trees.iter_mut().enumerate() {
        let msg = HotStuffMessage::ProposalHeader(signed_by(header(&u, 2, None), &leader2));
        let cast = run(tree, &honest[i], 2, msg, &leader2);
        assert_eq!(cast.len(), 1, "replica {i} votes Prepare(U, 2)");
        votes.push((honest[i].clone(), cast[0].clone()));
    }
    let prepare2 = certify(2, votes, &set);
    let mut votes = Vec::new();
    for (i, tree) in trees.iter_mut().enumerate() {
        let msg = HotStuffMessage::Nudge(Nudge { chain_id: CHAIN_ID, view: ViewNumber::new(3), justify: prepare2.clone() });
        let cast = run(tree, &honest[i], 3, msg, &byzantine_vk);
        assert_eq!(cast.len(), 1, "replica {i} votes Precommit(U, 3)");
        votes.push((honest[i].clone(), cast[0].clone()));
    }
    let precommit3 = certify(3, votes.clone(), &set);

    // The backup aggregator leader(5) alone collects Precommit(U, 3) and locks on U.
    let backup = proposer_for(ViewNumber::new(5), &keys, &vss, &trees[0]);
    let locked = honest.iter().position(|k| k.verifying_key() == backup).expect("leader(5) is honest");
    let (mut aggregator, events) = hotstuff_at_with_events(
        ViewNumber::new(3),
        honest[locked].clone(),
        trees[locked].validator_set_state().unwrap(),
    );
    for (key, vote) in votes {
        aggregator
            .on_receive_msg(HotStuffMessage::PhaseVote(vote), &key.verifying_key(), &mut trees[locked], &mut TreeApp)
            .unwrap();
    }
    assert!(events
        .try_iter()
        .any(|e| matches!(e, Event::CollectPC(e) if e.phase_certificate == precommit3)));
    assert_eq!(trees[locked].locked_pc().unwrap().block, u.hash, "leader(5) is locked on U");

    // The Byzantine leader(3) reports its own signed header of U' as its tip(3), which beats
    // Prepare(U, 2) in TC(3).
    let tip = TipInfo::from_header(&signed_by(header(&u_prime, 3, None), &byzantine_vk));
    assert!(tip.is_correct(CHAIN_ID, &trees[0]).unwrap(), "a correctly signed tip of leader(3)");
    let mut entries: Vec<_> = honest
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != locked)
        .map(|(_, k)| (k.clone(), None, Some(prepare2.clone())))
        .collect();
    entries.push((byzantine.clone(), Some(tip), Some(prepare2.clone())));
    let tc3 = timeout_certificate(3, entries, &set);
    assert!(tc3.high_tip_is_winner);

    // Honest leader(4) re-proposes U' (V2). The locked replica refuses it; the other two and
    // the Byzantine one certify Prepare(U', 4), which every honest replica learns.
    let leader4 = proposer_for(ViewNumber::new(4), &keys, &vss, &trees[0]);
    assert_ne!(leader4, backup);
    let mut votes = Vec::new();
    for (i, tree) in trees.iter_mut().enumerate() {
        assert!(valid_proposal_certificates(CHAIN_ID, ViewNumber::new(4), &u_prime.hash, &qc_a, Some(&tc3), None, tree).unwrap());
        let msg = HotStuffMessage::ProposalHeader(signed_by(header(&u_prime, 4, Some(tc3.clone())), &leader4));
        let cast = run(tree, &honest[i], 4, msg, &leader4);
        assert_eq!(cast.len(), usize::from(i != locked), "only the unlocked replicas vote for U' in view 4");
        votes.extend(cast.into_iter().map(|vote| (honest[i].clone(), vote)));
    }
    votes.push((byzantine.clone(), PhaseVote::new(&Keypair::new(byzantine.clone()), CHAIN_ID, ViewNumber::new(4), u_prime.hash, Phase::Prepare)));
    let prepare4 = certify(4, votes, &set);
    for (i, tree) in trees.iter_mut().enumerate() {
        let msg = HotStuffMessage::NewView(NewView { chain_id: CHAIN_ID, view: ViewNumber::new(4), highest_pc: prepare4.clone() });
        run(tree, &honest[i], 4, msg, &backup);
        assert!(tree.highest_pc().unwrap() == prepare4, "replica {i} learns Prepare(U', 4)");
    }
    assert!(trees[locked].locked_pc().unwrap() == precommit3, "the lock on U stays");

    // View 5's nudge is lost. Synchronous suffix from view 6.
    let mut previous_view_failed = true;
    let mut log = Vec::new();
    for view in 6..=30u64 {
        if trees.iter().all(|tree| tree.highest_committed_block().unwrap() == Some(u_prime.hash)) {
            break;
        }
        let leader = proposer_for(ViewNumber::new(view), &keys, &vss, &trees[0]);
        if leader == byzantine_vk {
            log.push(format!("view {view}: Byzantine leader silent"));
            previous_view_failed = true;
            continue;
        }
        let tc = previous_view_failed.then(|| {
            let entries = honest
                .iter()
                .enumerate()
                .map(|(i, k)| (k.clone(), trees[i].local_tip().unwrap(), trees[i].highest_qc_for_timeout().unwrap()))
                .collect();
            timeout_certificate(view - 1, entries, &set)
        });
        let li = honest.iter().position(|k| k.verifying_key() == leader).unwrap();
        let msg = match repropose_block(ViewNumber::new(view), &trees[li]).unwrap() {
            Some(block_hash) => {
                let block = trees[li].block(&block_hash).unwrap().unwrap();
                assert!(tc.is_some(), "a re-proposal with a stale justify needs TC(v-1)");
                HotStuffMessage::ProposalHeader(signed_by(header(&block, view, tc), &leader))
            }
            None => {
                let justify = trees[li].highest_pc().unwrap();
                assert!(justify.phase.is_prepare() || justify.phase.is_precommit() || justify.phase.is_commit());
                HotStuffMessage::Nudge(Nudge { chain_id: CHAIN_ID, view: ViewNumber::new(view), justify })
            }
        };
        let mut votes = Vec::new();
        for (i, tree) in trees.iter_mut().enumerate() {
            let cast = run(tree, &honest[i], view, msg.clone(), &leader);
            votes.extend(cast.into_iter().map(|vote| (honest[i].clone(), vote)));
        }
        log.push(format!("view {view}: {} honest votes", votes.len()));
        if votes.len() < honest.len() {
            previous_view_failed = true;
            continue;
        }
        let pc = certify(view, votes, &set);
        for (i, tree) in trees.iter_mut().enumerate() {
            let msg = HotStuffMessage::NewView(NewView { chain_id: CHAIN_ID, view: ViewNumber::new(view), highest_pc: pc.clone() });
            run(tree, &honest[i], view, msg, &leader);
        }
        previous_view_failed = false;
    }
    for (i, tree) in trees.iter().enumerate() {
        assert_eq!(
            tree.highest_committed_block().unwrap(),
            Some(u_prime.hash),
            "replica {i} (locked on U: {}) must commit U' in the synchronous suffix; {log:?}",
            i == locked
        );
    }
}
