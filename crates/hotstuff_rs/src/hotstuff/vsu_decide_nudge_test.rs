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
    let deliver = |trees: &mut Vec<BlockTreeSingleton<MemKV>>, only: &[usize], view: u64, msg: HotStuffMessage, origin: VerifyingKey| {
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
         (x = old leader of view {}); vote phases: {:?}",
        vp + 3,
        decide_votes.iter().map(|(_, v)| v.phase).collect::<Vec<_>>()
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
    assert!(votes.is_empty(), "a nudge from a validator that leads vp + 3 in neither set is dropped: {} votes", votes.len());
}

/// Found by the full hotstuff_rs run with the nudge fix (`late_joiner_crosses_its_own_join`
/// aborted on the steady-state debug assert): a replica in the window holds the phase-blind
/// collector pair (CVS and PVS collectors). The pacemaker's AdvanceView path marks the update
/// decided (`advance_highest_pc_from_remote` on another aggregator's Decide PC) without touching
/// HotStuff's collectors, so an old-set quorum of late Decide votes still completes the PVS
/// collector, and the decided branch only debug-asserted the PC's correctness: a debug build
/// aborts, a release build acts on a Decide PC without a new-set quorum (the S430 hole). The PC
/// must be checked (and dropped) whenever the PVS collector is still there.
#[test]
fn old_set_decide_pc_collected_after_a_remote_decide_is_dropped() {
    use crate::types::crypto_primitives::Keypair;
    use crate::types::data_types::Power;

    let keys = signing_keys(&[1, 2, 3, 4, 5]);
    let old_keys = &keys[..4];
    let old_set = validator_set(old_keys);
    let joiner = keys[4].verifying_key();
    let mut updates = ValidatorSetUpdates::new();
    updates.insert(joiner, Power::new(1));
    let mut new_set = old_set.clone();
    new_set.apply_updates(&updates);

    let vote = |key: &SigningKey, view: u64, block: CryptoHash, phase: Phase| {
        (key.clone(), PhaseVote::new(&Keypair::new(key.clone()), CHAIN_ID, ViewNumber::new(view), block, phase))
    };
    let a = Block::new(BlockHeight::new(0), PhaseCertificate::genesis_pc(), CryptoHash::new([10; 32]), Data::new(vec![]));
    let qc_a = generic_pc(ViewNumber::new(1), a.hash, old_keys, &old_set);
    let u = Block::new(BlockHeight::new(1), qc_a.clone(), CryptoHash::new([11; 32]), Data::new(vec![]));
    let (mut tree, _) = steady_block_tree(&old_set);
    tree.insert(&a, None, None).unwrap();
    tree.update(&qc_a, &None).unwrap();
    tree.insert(&u, None, Some(&updates)).unwrap();
    for (view, phase) in [(2, Phase::Prepare), (3, Phase::Precommit), (4, Phase::Commit)] {
        let votes: Vec<_> = old_keys.iter().map(|k| vote(k, view, u.hash, phase)).collect();
        let pc = certify(view, &votes, &old_set);
        tree.update(&pc, &None).unwrap();
    }
    let state = tree.validator_set_state().unwrap();
    assert!(!state.update_decided() && state.committed_validator_set() == &new_set, "fixture: U committed, undecided");

    // The replica is in the Decide view 5 with the window's collector pair.
    let local = keys[0].clone();
    let (mut hotstuff, _events) = hotstuff_at_with_events(ViewNumber::new(5), local, state);

    // Another aggregator's Decide PC (a new-set quorum: four of five) arrives by AdvanceView.
    let decide_votes: Vec<_> = old_keys.iter().map(|k| vote(k, 5, u.hash, Phase::Decide)).collect();
    let remote_decide = certify(5, &decide_votes, &new_set);
    tree.advance_highest_pc_from_remote(&remote_decide, &None).unwrap();
    assert!(tree.validator_set_state().unwrap().update_decided(), "fixture: the remote Decide PC decides U");

    // Three late Decide votes of old-set members: an old-set quorum, not a new-set one.
    for (key, vote) in decide_votes.into_iter().take(3) {
        hotstuff
            .on_receive_msg(HotStuffMessage::PhaseVote(vote), &key.verifying_key(), &mut tree, &mut TreeApp)
            .expect("processing a vote must not error");
    }
    let highest = tree.highest_pc().unwrap();
    assert_eq!(
        (highest.phase, highest.view.int(), highest.signatures.len()),
        (Phase::Decide, 5, remote_decide.signatures.len()),
        "the highest PC stays the new-set Decide PC"
    );
}

// ---------------------------------------------------------------------------------------------
// s140 follow-up (open question of fix/dl-post-exclusion-stall): the first proposal after an
// update's Decide is justified by the Decide PC and comes from the leader the update installs.
// A replica that missed the update's Commit PC (it voted Commit but never got the Decide nudge,
// or only got the Decide PC by AdvanceView, which marks the update decided without committing
// it) still holds the old committed set. Checked against it, that leader is not the proposer
// (in the exclusion case the old leader of the view is the down validator itself), and
// `on_receive_msg` dropped the header or proposal before its Decide justify was looked at. It
// needs a Decide quorum without that replica: with one validator down, 4 of 4 and 5 of 5 equal
// stakes need every live vote, 7 equal stakes (quorum 5, 6 live) do not; with every validator
// live any n >= 4 can miss one.
// ---------------------------------------------------------------------------------------------

/// Validates every block (no updates) for the full-proposal path.
struct ValidApp;

impl App<MemKV> for ValidApp {
    fn produce_block(&mut self, _request: ProduceBlockRequest<MemKV>) -> ProduceBlockResponse {
        unreachable!("no replica produces a block in this test")
    }
    fn validate_block(&mut self, _request: ValidateBlockRequest<MemKV>) -> ValidateBlockResponse {
        ValidateBlockResponse::Valid { app_state_updates: None, validator_set_updates: None }
    }
    fn validate_block_for_sync(&mut self, _request: ValidateBlockRequest<MemKV>) -> ValidateBlockResponse {
        unreachable!("no block sync in this test")
    }
}

/// An update U taken to its Decide PC by `live` (all validators of `set` but the down ones),
/// with the replica `missed` (an index into `live`) never getting the Decide nudge (so never
/// U's Commit PC).
struct DecideFixture {
    live: Vec<SigningKey>,
    trees: Vec<BlockTreeSingleton<MemKV>>,
    u: Block,
    new_set: ValidatorSet,
    decide: PhaseCertificate,
    /// The Prepare view; Decide is vp + 3, the first proposal after it vp + 4.
    vp: u64,
}

fn decide_fixture(
    set: &ValidatorSet,
    live: Vec<SigningKey>,
    updates: &ValidatorSetUpdates,
    vp: u64,
    missed: usize,
) -> DecideFixture {
    let old_leader = |view: u64| select_leader(ViewNumber::new(view), set);
    let mut new_set = set.clone();
    new_set.apply_updates(updates);
    let a = Block::new(BlockHeight::new(0), PhaseCertificate::genesis_pc(), CryptoHash::new([10; 32]), Data::new(vec![]));
    let qc_a = generic_pc(ViewNumber::new(vp - 1), a.hash, &live, set);
    let u = Block::new(BlockHeight::new(1), qc_a.clone(), CryptoHash::new([11; 32]), Data::new(vec![]));
    let mut trees: Vec<BlockTreeSingleton<MemKV>> = live
        .iter()
        .map(|_| {
            let (mut tree, _) = steady_block_tree(set);
            tree.insert(&a, None, None).unwrap();
            tree.update(&qc_a, &None).unwrap();
            tree.insert(&u, None, Some(updates)).unwrap();
            tree
        })
        .collect();
    let deliver = |trees: &mut Vec<BlockTreeSingleton<MemKV>>, skip: Option<usize>, view: u64, msg: HotStuffMessage, origin: VerifyingKey| {
        let mut votes = Vec::new();
        for i in (0..live.len()).filter(|i| Some(*i) != skip) {
            let cast = run(&mut trees[i], &live[i], view, msg.clone(), &origin);
            votes.extend(cast.into_iter().map(|vote| (live[i].clone(), vote)));
        }
        votes
    };
    let nudge = |view: u64, justify: &PhaseCertificate| {
        HotStuffMessage::Nudge(Nudge { chain_id: CHAIN_ID, view: ViewNumber::new(view), justify: justify.clone() })
    };
    let prepare_votes = deliver(&mut trees, None, vp, HotStuffMessage::ProposalHeader(signed_by(header(&u, vp), &old_leader(vp))), old_leader(vp));
    assert_eq!(prepare_votes.len(), live.len(), "fixture: every live replica votes Prepare(U, {vp})");
    let prepare = certify(vp, &prepare_votes, set);
    let precommit = certify(vp + 1, &deliver(&mut trees, None, vp + 1, nudge(vp + 1, &prepare), old_leader(vp + 1)), set);
    let commit_votes = deliver(&mut trees, None, vp + 2, nudge(vp + 2, &precommit), old_leader(vp + 2));
    assert_eq!(commit_votes.len(), live.len(), "fixture: every live replica votes Commit in {}", vp + 2);
    let commit = certify(vp + 2, &commit_votes, set);
    assert_eq!(commit.phase, Phase::Commit);

    // The Decide nudge of U's leader of vp + 3 reaches everyone but `missed`.
    let decider = select_leader(ViewNumber::new(vp + 3), &new_set);
    let decide_votes: Vec<_> = deliver(&mut trees, Some(missed), vp + 3, nudge(vp + 3, &commit), decider)
        .into_iter()
        .filter(|(_, v)| v.phase == Phase::Decide)
        .collect();
    assert_eq!(decide_votes.len(), live.len() - 1, "fixture: the others vote Decide");
    let decide = certify(vp + 3, &decide_votes, &new_set);
    assert_eq!(decide.phase, Phase::Decide);
    let missed_state = trees[missed].validator_set_state().unwrap();
    assert!(missed_state.committed_validator_set() == set, "fixture: the missed replica has not committed U");
    DecideFixture { live, trees, u, new_set, decide, vp }
}

/// The first block after U's Decide, justified by `decide`.
fn first_block_after(decide: &PhaseCertificate) -> Block {
    Block::new(BlockHeight::new(2), decide.clone(), CryptoHash::new([12; 32]), Data::new(vec![]))
}

/// The header announcing the non-updating `block` in `view`.
fn plain_header(block: &Block, view: u64) -> ProposalHeader {
    ProposalHeader { has_validator_set_updates: false, ..header(block, view) }
}

/// Seven equal validators, x down: views where x is the old leader of vp + 4 (the view after
/// the Decide) and leads none of vp..=vp + 2 (Prepare, Precommit, Commit).
fn seven_with_x_leading_after_decide() -> (Vec<SigningKey>, ValidatorSet, VerifyingKey, u64) {
    let keys = signing_keys(&[1, 2, 3, 4, 5, 6, 7]);
    let set = validator_set(&keys);
    let leader = |view: u64| select_leader(ViewNumber::new(view), &set);
    for vp in 2..256u64 {
        let x = leader(vp + 4);
        if (vp..=vp + 2).all(|v| leader(v) != x) {
            return (keys, set, x, vp);
        }
    }
    panic!("no shape");
}

fn excluding(x: VerifyingKey) -> ValidatorSetUpdates {
    let mut updates = ValidatorSetUpdates::new();
    updates.exclude_leader(x);
    updates
}

/// `missed` index: a live replica that is neither the Decide nudger nor `not`.
fn missed_index(live: &[SigningKey], new_set: &ValidatorSet, vp: u64, not: &[VerifyingKey]) -> usize {
    let decider = select_leader(ViewNumber::new(vp + 3), new_set);
    live.iter()
        .position(|k| k.verifying_key() != decider && !not.contains(&k.verifying_key()))
        .unwrap()
}

/// Run the header case for the replica that missed the Commit PC; `advance_view_first` first
/// gives it the Decide PC by AdvanceView (`advance_highest_pc_from_remote`).
fn header_after_decide_is_voted_on(advance_view_first: bool) {
    let (keys, set, x, vp) = seven_with_x_leading_after_decide();
    let live: Vec<SigningKey> = keys.iter().filter(|k| k.verifying_key() != x).cloned().collect();
    let updates = excluding(x);
    let mut new_set = set.clone();
    new_set.apply_updates(&updates);
    let l = select_leader(ViewNumber::new(vp + 4), &new_set);
    assert_ne!(l, x);
    let r = missed_index(&live, &new_set, vp, &[l]);
    let mut f = decide_fixture(&set, live, &updates, vp, r);
    if advance_view_first {
        f.trees[r].advance_highest_pc_from_remote(&f.decide, &None).unwrap();
        let state = f.trees[r].validator_set_state().unwrap();
        assert!(state.update_decided() && !state.committed_validator_set().is_leader_excluded(&x), "fixture: decided, U not committed");
    }
    assert_eq!(select_leader(ViewNumber::new(f.vp + 4), f.trees[r].validator_set_state().unwrap().committed_validator_set()), x, "fixture: x leads vp + 4 in the old set");

    let b = first_block_after(&f.decide);
    f.trees[r].insert(&b, None, None).unwrap();
    let msg = HotStuffMessage::ProposalHeader(signed_by(plain_header(&b, f.vp + 4), &l));
    let votes = run(&mut f.trees[r], &f.live[r], f.vp + 4, msg, &l);
    assert!(
        f.trees[r].validator_set_state().unwrap().committed_validator_set().is_leader_excluded(&x),
        "the replica that missed Commit(U) commits U from the header's Decide justify"
    );
    assert_eq!(
        votes.iter().filter(|v| v.phase == Phase::Generic && v.block == b.hash).count(),
        1,
        "the replica that missed Commit(U) votes for the first block after U's Decide, proposed by \
         the leader U installs (x = old leader of {}); vote phases: {:?}",
        f.vp + 4,
        votes.iter().map(|v| v.phase).collect::<Vec<_>>()
    );
}

#[test]
fn decide_justified_header_of_the_installed_leader_is_voted_on_without_the_commit_pc() {
    header_after_decide_is_voted_on(false);
}

#[test]
fn decide_justified_header_is_voted_on_after_an_advance_view_decided_the_update() {
    header_after_decide_is_voted_on(true);
}

#[test]
fn decide_justified_proposal_of_the_installed_leader_is_voted_on_without_the_commit_pc() {
    let (keys, set, x, vp) = seven_with_x_leading_after_decide();
    let live: Vec<SigningKey> = keys.iter().filter(|k| k.verifying_key() != x).cloned().collect();
    let updates = excluding(x);
    let mut new_set = set.clone();
    new_set.apply_updates(&updates);
    let l = select_leader(ViewNumber::new(vp + 4), &new_set);
    let r = missed_index(&live, &new_set, vp, &[l]);
    let mut f = decide_fixture(&set, live, &updates, vp, r);

    let b = first_block_after(&f.decide);
    let proposal = crate::hotstuff::messages::Proposal::signed(
        &crate::test_keys::key_of(&l), CHAIN_ID, ViewNumber::new(f.vp + 4), b.clone(), None, None,
    );
    let (mut hotstuff, events) =
        hotstuff_at_with_events(ViewNumber::new(f.vp + 4), f.live[r].clone(), f.trees[r].validator_set_state().unwrap());
    hotstuff
        .on_receive_msg(HotStuffMessage::Proposal(proposal), &l, &mut f.trees[r], &mut ValidApp)
        .expect("processing must not error");
    let votes: Vec<PhaseVote> = events
        .try_iter()
        .filter_map(|event| match event {
            Event::PhaseVote(event) => Some(event.vote),
            _ => None,
        })
        .collect();
    assert!(f.trees[r].validator_set_state().unwrap().committed_validator_set().is_leader_excluded(&x), "U committed");
    assert_eq!(
        votes.iter().filter(|v| v.phase == Phase::Generic && v.block == b.hash).count(),
        1,
        "the replica that missed Commit(U) votes for the installed leader's full proposal"
    );
}

/// The relaxed check lets no other validator propose: a header with a correct Decide justify
/// from a validator that leads vp + 4 in neither set is still dropped (the PC itself may be
/// applied, as a NewView's would be).
#[test]
fn decide_justified_header_from_a_non_leader_is_still_dropped() {
    let (keys, set, x, vp) = seven_with_x_leading_after_decide();
    let live: Vec<SigningKey> = keys.iter().filter(|k| k.verifying_key() != x).cloned().collect();
    let updates = excluding(x);
    let mut new_set = set.clone();
    new_set.apply_updates(&updates);
    let l = select_leader(ViewNumber::new(vp + 4), &new_set);
    let impostor = live.iter().map(|k| k.verifying_key()).find(|k| *k != l && *k != x).unwrap();
    let r = missed_index(&live, &new_set, vp, &[l, impostor]);
    let mut f = decide_fixture(&set, live, &updates, vp, r);
    let b = first_block_after(&f.decide);
    f.trees[r].insert(&b, None, None).unwrap();
    let msg = HotStuffMessage::ProposalHeader(signed_by(plain_header(&b, f.vp + 4), &impostor));
    let votes = run(&mut f.trees[r], &f.live[r], f.vp + 4, msg, &impostor);
    assert!(votes.is_empty(), "a header from a validator leading vp + 4 in neither set is dropped: {} votes", votes.len());
}

/// A forged Decide justify (below the new set's quorum) is neither applied nor lets the
/// installed leader's header through.
#[test]
fn forged_decide_justify_is_not_applied() {
    let (keys, set, x, vp) = seven_with_x_leading_after_decide();
    let live: Vec<SigningKey> = keys.iter().filter(|k| k.verifying_key() != x).cloned().collect();
    let updates = excluding(x);
    let mut new_set = set.clone();
    new_set.apply_updates(&updates);
    let l = select_leader(ViewNumber::new(vp + 4), &new_set);
    let r = missed_index(&live, &new_set, vp, &[l]);
    let mut f = decide_fixture(&set, live, &updates, vp, r);

    // Four of seven signatures: drop one of the five.
    let mut forged = f.decide.clone();
    let first_signed = (0..forged.signatures.len()).find(|&i| forged.signatures.get(i).is_some()).unwrap();
    forged.signatures.set(first_signed, None);
    let b = first_block_after(&forged);
    f.trees[r].insert(&b, None, None).unwrap();
    let msg = HotStuffMessage::ProposalHeader(signed_by(plain_header(&b, f.vp + 4), &l));
    let votes = run(&mut f.trees[r], &f.live[r], f.vp + 4, msg, &l);
    assert!(!f.trees[r].validator_set_state().unwrap().committed_validator_set().is_leader_excluded(&x), "a sub-quorum Decide PC commits nothing");
    assert!(votes.is_empty(), "no vote on a block justified by a forged Decide PC");
}

/// A membership change (seven validators, an eighth joins; all live): the replica that missed
/// Commit(U) must follow the new set's leader on a new-set Decide PC, and must not accept an
/// old-set certificate in its place.
#[test]
fn join_update_decide_header_needs_a_new_set_decide_pc() {
    use crate::types::crypto_primitives::Keypair;
    use crate::types::data_types::Power;

    let keys = signing_keys(&[1, 2, 3, 4, 5, 6, 7]);
    let set = validator_set(&keys);
    let joiner = SigningKey::from_bytes(&[8; 32]).verifying_key();
    let mut updates = ValidatorSetUpdates::new();
    updates.insert(joiner, Power::new(1));
    let mut new_set = set.clone();
    new_set.apply_updates(&updates);
    let vp = (2..256u64)
        .find(|&vp| {
            let l = select_leader(ViewNumber::new(vp + 4), &new_set);
            l != select_leader(ViewNumber::new(vp + 4), &set) && l != joiner
        })
        .expect("a view whose leader the join changes");
    let l = select_leader(ViewNumber::new(vp + 4), &new_set);
    let r = missed_index(&keys, &new_set, vp, &[l]);
    let mut f = decide_fixture(&set, keys.clone(), &updates, vp, r);

    // An old-set "Decide" PC on U: five old signatures over the old set (an old quorum).
    let old_votes: Vec<_> = keys
        .iter()
        .take(5)
        .map(|k| (k.clone(), PhaseVote::new(&Keypair::new(k.clone()), CHAIN_ID, ViewNumber::new(f.vp + 3), f.u.hash, Phase::Decide)))
        .collect();
    let old_decide = certify(f.vp + 3, &old_votes, &set);
    let b_old = first_block_after(&old_decide);
    f.trees[r].insert(&b_old, None, None).unwrap();
    let votes = run(&mut f.trees[r], &f.live[r], f.vp + 4, HotStuffMessage::ProposalHeader(signed_by(plain_header(&b_old, f.vp + 4), &l)), &l);
    assert!(votes.is_empty(), "an old-set Decide PC does not let the new set's leader through");
    assert!(f.trees[r].validator_set_state().unwrap().committed_validator_set() == &set, "an old-set Decide PC commits nothing");

    let b = first_block_after(&f.decide);
    f.trees[r].insert(&b, None, None).unwrap();
    let votes = run(&mut f.trees[r], &f.live[r], f.vp + 4, HotStuffMessage::ProposalHeader(signed_by(plain_header(&b, f.vp + 4), &l)), &l);
    assert!(f.trees[r].validator_set_state().unwrap().committed_validator_set() == &f.new_set, "U committed from the new-set Decide PC");
    assert_eq!(votes.iter().filter(|v| v.phase == Phase::Generic && v.block == b.hash).count(), 1, "votes for the new set's leader");
}
