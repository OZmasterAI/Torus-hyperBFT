//! s140 (bench 9p smoke, ozarchy-s138-9p-smkA-smoke-s0): the Decide nudge of a leader-exclusion
//! update, sent by the leader the update itself installs, must not be dropped by the replicas
//! that have not committed the update yet.
//!
//! Four equal validators, x down. The update U (exclude x) runs Prepare (vp), Precommit
//! (vp + 1) and Commit (vp + 2). x leads vp + 3 in the old rotation, so the Commit votes of
//! vp + 2 reach only the backup aggregator p = old leader(vp + 4), which forms Commit(U, vp + 2),
//! commits U and enters vp + 3. Once x is excluded, p also leads vp + 3 (IWRR over the three
//! eligible validators), so p nudges there with the Commit PC. The other two replicas have not
//! seen that PC: their committed set is still the old one, where leader(vp + 3) is x, so the
//! proposer check in `on_receive_msg` dropped p's nudge before its justify was looked at. No
//! Decide vote, no quorum (three of four needed), and the two parked in vp + 3 under a
//! commit-lag-backed-off deadline while p timed out alone and ran ahead: ~36 s without a commit
//! in the smoke (views 3649-3653; old rotation [val1, val3, val0, val2], new [val1, val0, val2],
//! 3649 = 1 mod 12).
//!
//! The fix: a nudge whose justify is a correct and safe Commit PC is applied first (as a
//! NewView's highest PC is, from any validator), then the proposer is checked against the
//! validator-set state that PC leaves.

use std::time::{Duration, Instant};

use crate::app::{App, ProduceBlockRequest, ProduceBlockResponse, ValidateBlockRequest, ValidateBlockResponse};
use crate::block_tree::accessors::internal::BlockTreeSingleton;
use crate::events::Event;
use crate::hotstuff::header_fast_path_regression_test::{
    generic_pc, hotstuff_at_with_events, signing_keys, steady_block_tree, validator_set, MemKV,
};
use crate::hotstuff::messages::{HotStuffMessage, Nudge, PhaseVote, ProposalHeader};
use crate::hotstuff::types::{Phase, PhaseCertificate, PhaseVoteCollector};
use crate::pacemaker::implementation::{select_leader, ViewInfo};
use crate::test_keys::signed_by;
use crate::types::block::Block;
use crate::types::crypto_primitives::{SigningKey, VerifyingKey};
use crate::types::data_types::{BlockHeight, ChainID, CryptoHash, Data, SignatureBytes, ViewNumber};
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

/// The PC that `votes` form in `view` over `set` (as the `PhaseVoteCollector` forms it).
fn certify(view: u64, votes: &[(SigningKey, PhaseVote)], set: &ValidatorSet) -> PhaseCertificate {
    let mut collector = PhaseVoteCollector::new(CHAIN_ID, ViewNumber::new(view), set.clone());
    votes
        .iter()
        .filter_map(|(key, vote)| collector.collect(&key.verifying_key(), vote.clone()))
        .next()
        .expect("the votes form a quorum")
}

/// The views and roles of the smoke trace for four equal validators `keys`: the down validator
/// x, the Prepare view vp (so x leads vp + 3 in the old rotation) and p, which is the old
/// leader of vp + 4 and the new leader of vp + 3 once x is excluded.
fn smoke_shape(keys: &[SigningKey], set: &ValidatorSet) -> (VerifyingKey, u64, VerifyingKey) {
    let leader = |view: u64, set: &ValidatorSet| select_leader(ViewNumber::new(view), set);
    for vp in 2..64u64 {
        let x = leader(vp + 3, set);
        let mut updates = ValidatorSetUpdates::new();
        updates.exclude_leader(x);
        let mut new_set = set.clone();
        new_set.apply_updates(&updates);
        let p = leader(vp + 4, set);
        if leader(vp + 3, &new_set) == p && keys.iter().any(|k| k.verifying_key() == x) {
            return (x, vp, p);
        }
    }
    panic!("no view in which the backup aggregator also leads the next view after the exclusion");
}

/// The header announcing the validator-set-updating `block` in `view`.
fn header(block: &Block, view: u64) -> ProposalHeader {
    ProposalHeader {
        chain_id: CHAIN_ID,
        view: ViewNumber::new(view),
        block_hash: block.hash,
        height: block.height,
        data_hash: block.data_hash,
        justify: block.justify.clone(),
        tc: None,
        nec: None,
        has_validator_set_updates: true,
        leader_signature: SignatureBytes::new([0; 64]),
    }
}

#[test]
fn decide_nudge_from_the_leader_the_update_installs_is_voted_on() {
    let keys = signing_keys(&[1, 2, 3, 4]);
    let set = validator_set(&keys);
    let (x, vp, p) = smoke_shape(&keys, &set);
    let live: Vec<SigningKey> = keys.iter().filter(|k| k.verifying_key() != x).cloned().collect();
    let p_index = live.iter().position(|k| k.verifying_key() == p).unwrap();
    let leader = |view: u64| select_leader(ViewNumber::new(view), &set);

    // A (QC(A) in vp - 1, so vp's proposal needs no TC) and U = exclude x, justified by QC(A).
    let a = Block::new(BlockHeight::new(0), PhaseCertificate::genesis_pc(), CryptoHash::new([10; 32]), Data::new(vec![]));
    let qc_a = generic_pc(ViewNumber::new(vp - 1), a.hash, &live, &set);
    let u = Block::new(BlockHeight::new(1), qc_a.clone(), CryptoHash::new([11; 32]), Data::new(vec![]));
    let mut updates = ValidatorSetUpdates::new();
    updates.exclude_leader(x);
    let mut trees: Vec<BlockTreeSingleton<MemKV>> = live
        .iter()
        .map(|_| {
            let (mut tree, _) = steady_block_tree(&set);
            tree.insert(&a, None, None).unwrap();
            tree.update(&qc_a, &None).unwrap();
            tree.insert(&u, None, Some(&updates)).unwrap();
            tree
        })
        .collect();
    let mut deliver = |trees: &mut Vec<BlockTreeSingleton<MemKV>>, only: &[usize], view: u64, msg: HotStuffMessage, origin: VerifyingKey| {
        let mut votes = Vec::new();
        for &i in only {
            let cast = run(&mut trees[i], &live[i], view, msg.clone(), &origin);
            votes.extend(cast.into_iter().map(|vote| (live[i].clone(), vote)));
        }
        votes
    };
    let all: Vec<usize> = (0..live.len()).collect();

    // Prepare (vp), Precommit (vp + 1), Commit (vp + 2): every live replica votes each phase.
    let prepare_votes = deliver(&mut trees, &all, vp, HotStuffMessage::ProposalHeader(signed_by(header(&u, vp), &leader(vp))), leader(vp));
    assert_eq!(prepare_votes.len(), 3, "every live replica votes Prepare(U, {vp})");
    let prepare = certify(vp, &prepare_votes, &set);
    assert_eq!(prepare.phase, Phase::Prepare);
    let nudge = |view: u64, justify: &PhaseCertificate| {
        HotStuffMessage::Nudge(Nudge { chain_id: CHAIN_ID, view: ViewNumber::new(view), justify: justify.clone() })
    };
    let precommit_votes = deliver(&mut trees, &all, vp + 1, nudge(vp + 1, &prepare), leader(vp + 1));
    assert_eq!(precommit_votes.len(), 3, "every live replica votes Precommit in {}", vp + 1);
    let precommit = certify(vp + 1, &precommit_votes, &set);
    let commit_votes = deliver(&mut trees, &all, vp + 2, nudge(vp + 2, &precommit), leader(vp + 2));
    assert_eq!(commit_votes.len(), 3, "every live replica votes Commit in {}", vp + 2);
    let commit = certify(vp + 2, &commit_votes, &set);
    assert_eq!(commit.phase, Phase::Commit);

    // Only p (the backup aggregator; x would have been the primary) learns Commit(U, vp + 2):
    // it commits U, x is excluded in its committed set, and p leads vp + 3 there.
    trees[p_index].update(&commit, &None).unwrap();
    let p_state = trees[p_index].validator_set_state().unwrap();
    assert!(p_state.committed_validator_set().is_leader_excluded(&x), "fixture: p committed U");
    assert_eq!(select_leader(ViewNumber::new(vp + 3), p_state.committed_validator_set()), p, "fixture: p leads {} after U", vp + 3);

    // p's own leader rules nudge with the Commit PC in vp + 3.
    let p_nudge = {
        let tree = &mut trees[p_index];
        let (mut hotstuff, events) =
            hotstuff_at_with_events(ViewNumber::new(vp + 2), live[p_index].clone(), tree.validator_set_state().unwrap());
        hotstuff
            .enter_view(ViewInfo::new(ViewNumber::new(vp + 3), Instant::now() + Duration::from_secs(3600)), tree, &mut TreeApp)
            .expect("p enters vp + 3");
        events
            .try_iter()
            .find_map(|event| match event {
                Event::Nudge(event) => Some(HotStuffMessage::Nudge(event.nudge)),
                _ => None,
            })
            .expect("p nudges with Commit(U) in vp + 3")
    };
    if let HotStuffMessage::Nudge(n) = &p_nudge {
        assert_eq!((n.justify.phase, n.justify.view.int()), (Phase::Commit, vp + 2));
    }

    // The two replicas that never saw Commit(U) must take p's nudge: vote Decide and commit U.
    let others: Vec<usize> = (0..live.len()).filter(|&i| i != p_index).collect();
    for &i in &others {
        let state = trees[i].validator_set_state().unwrap();
        assert!(!state.committed_validator_set().is_leader_excluded(&x), "fixture: replica {i} has not committed U");
        assert_eq!(select_leader(ViewNumber::new(vp + 3), state.committed_validator_set()), x, "fixture: x leads {} before U", vp + 3);
    }
    let decide_votes = deliver(&mut trees, &others, vp + 3, p_nudge, p);
    assert_eq!(
        decide_votes.iter().filter(|(_, v)| v.phase == Phase::Decide && v.block == u.hash).count(),
        others.len(),
        "the replicas without Commit(U) must vote Decide on the nudge of the leader U installs \
         (x = old leader of view {}); votes: {decide_votes:?}",
        vp + 3
    );
    for &i in &others {
        assert!(
            trees[i].validator_set_state().unwrap().committed_validator_set().is_leader_excluded(&x),
            "replica {i} commits U from the nudge's justify"
        );
    }
}

/// The relaxed check must not let any validator nudge: a Commit PC whose update does not make
/// the sender a leader of the view leaves the nudge dropped (the PC itself is still applied, as
/// a NewView's would be).
#[test]
fn commit_nudge_from_a_non_leader_is_still_dropped() {
    let keys = signing_keys(&[1, 2, 3, 4]);
    let set = validator_set(&keys);
    let (x, vp, p) = smoke_shape(&keys, &set);
    let live: Vec<SigningKey> = keys.iter().filter(|k| k.verifying_key() != x).cloned().collect();
    let leader = |view: u64| select_leader(ViewNumber::new(view), &set);

    let a = Block::new(BlockHeight::new(0), PhaseCertificate::genesis_pc(), CryptoHash::new([10; 32]), Data::new(vec![]));
    let qc_a = generic_pc(ViewNumber::new(vp - 1), a.hash, &live, &set);
    let u = Block::new(BlockHeight::new(1), qc_a.clone(), CryptoHash::new([11; 32]), Data::new(vec![]));
    let mut updates = ValidatorSetUpdates::new();
    updates.exclude_leader(x);
    let mut trees: Vec<BlockTreeSingleton<MemKV>> = live
        .iter()
        .map(|_| {
            let (mut tree, _) = steady_block_tree(&set);
            tree.insert(&a, None, None).unwrap();
            tree.update(&qc_a, &None).unwrap();
            tree.insert(&u, None, Some(&updates)).unwrap();
            tree
        })
        .collect();
    let mut cast = |view: u64, msg: HotStuffMessage, origin: VerifyingKey| {
        let mut votes = Vec::new();
        for (i, tree) in trees.iter_mut().enumerate() {
            votes.extend(run(tree, &live[i], view, msg.clone(), &origin).into_iter().map(|v| (live[i].clone(), v)));
        }
        votes
    };
    let nudge = |view: u64, justify: &PhaseCertificate| {
        HotStuffMessage::Nudge(Nudge { chain_id: CHAIN_ID, view: ViewNumber::new(view), justify: justify.clone() })
    };
    let prepare = certify(vp, &cast(vp, HotStuffMessage::ProposalHeader(signed_by(header(&u, vp), &leader(vp))), leader(vp)), &set);
    let precommit = certify(vp + 1, &cast(vp + 1, nudge(vp + 1, &prepare), leader(vp + 1)), &set);
    let commit = certify(vp + 2, &cast(vp + 2, nudge(vp + 2, &precommit), leader(vp + 2)), &set);

    // Neither the old leader of vp + 3 (x) nor the new one (p): a third live validator.
    let mut new_set = set.clone();
    new_set.apply_updates(&updates);
    let impostor = live
        .iter()
        .map(|k| k.verifying_key())
        .find(|k| *k != p && *k != select_leader(ViewNumber::new(vp + 3), &new_set))
        .expect("a live validator that leads vp + 3 in neither set");
    let votes = cast(vp + 3, nudge(vp + 3, &commit), impostor);
    assert!(votes.is_empty(), "a nudge from a validator that leads vp + 3 in neither set is dropped: {votes:?}");
}
