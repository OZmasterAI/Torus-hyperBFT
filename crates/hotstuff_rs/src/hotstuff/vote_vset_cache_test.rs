//! perf/vote-vset-cache (bench 9v2, perf section 136): `on_receive_phase_vote` refreshed the phase
//! vote collectors from the block tree's validator-set state before counting every vote (s140
//! review P1: block sync commits and decides an update with `block_tree.update` in the middle of a
//! view, and nothing else refreshes the collectors). Each refresh is a `validator_set_state` read:
//! ~5 KV gets, 2 validator-set deserializations and 2 full set comparisons, on the consensus
//! thread, per vote.
//!
//! The collectors must still be over the current state whenever a vote is counted, but the state
//! only has to be read again once it has changed: the block tree keeps a generation of its
//! validator-set state (bumped by every write that touches it, block sync's included) and
//! HotStuff refreshes only when the generation differs from the one its collectors were synced to.
//!
//! - A steady view's votes read the state once (the first vote after construction), not per vote,
//!   and a block-tree write that touches no validator-set key does not cost a read.
//! - An update committed and decided out of band in the middle of a view is seen by the next vote
//!   (no old-set PC), and the votes after it do not read again.
//! - Generations are unique across block-tree instances: collectors synced against one tree are
//!   refreshed when the votes are counted against another (a re-opened store), even if both trees
//!   made the same number of validator-set writes.

use crate::app::{App, ProduceBlockRequest, ProduceBlockResponse, ValidateBlockRequest, ValidateBlockResponse};
use crate::block_tree::accessors::internal::validator_set_state_reads;
use crate::hotstuff::header_fast_path_regression_test::{
    generic_pc, hotstuff_at, signing_keys, steady_block_tree, validator_set, MemKV,
};
use crate::hotstuff::messages::{HotStuffMessage, PhaseVote};
use crate::hotstuff::types::{Phase, PhaseCertificate, PhaseVoteCollector};
use crate::types::block::Block;
use crate::types::crypto_primitives::{Keypair, SigningKey};
use crate::types::data_types::{BlockHeight, ChainID, CryptoHash, Data, Power, ViewNumber};
use crate::types::signed_messages::Collector;
use crate::types::update_sets::ValidatorSetUpdates;

const CHAIN_ID: ChainID = ChainID::new(0);

/// Votes never reach the app.
struct NoApp;

impl App<MemKV> for NoApp {
    fn produce_block(&mut self, _request: ProduceBlockRequest<MemKV>) -> ProduceBlockResponse {
        unreachable!("no replica produces a block in this test")
    }
    fn validate_block(&mut self, _request: ValidateBlockRequest<MemKV>) -> ValidateBlockResponse {
        unreachable!("no block is validated in this test")
    }
    fn validate_block_for_sync(&mut self, _request: ValidateBlockRequest<MemKV>) -> ValidateBlockResponse {
        unreachable!("no block sync in this test")
    }
}

fn vote(key: &SigningKey, view: u64, block: CryptoHash, phase: Phase) -> PhaseVote {
    PhaseVote::new(&Keypair::new(key.clone()), CHAIN_ID, ViewNumber::new(view), block, phase)
}

fn send_vote(
    hotstuff: &mut crate::hotstuff::implementation::HotStuff<crate::hotstuff::header_fast_path_regression_test::NullNetwork>,
    tree: &mut crate::block_tree::accessors::internal::BlockTreeSingleton<MemKV>,
    key: &SigningKey,
    view: u64,
    block: CryptoHash,
    phase: Phase,
) {
    hotstuff
        .on_receive_msg(HotStuffMessage::PhaseVote(vote(key, view, block, phase)), &key.verifying_key(), tree, &mut NoApp)
        .expect("processing a vote must not error");
}

fn certify(view: u64, block: CryptoHash, phase: Phase, keys: &[SigningKey], set: &crate::types::validator_set::ValidatorSet) -> PhaseCertificate {
    let mut collector = PhaseVoteCollector::new(CHAIN_ID, ViewNumber::new(view), set.clone());
    keys.iter()
        .filter_map(|key| collector.collect(&key.verifying_key(), vote(key, view, block, phase)))
        .next()
        .expect("the votes form a quorum")
}

#[test]
fn steady_view_votes_read_the_validator_set_state_once() {
    let keys = signing_keys(&[1, 2, 3, 4, 5, 6, 7, 8, 9, 10]);
    let set = validator_set(&keys);
    let (mut tree, vss) = steady_block_tree(&set);
    let mut hotstuff = hotstuff_at(ViewNumber::new(5), keys[0].clone(), vss);
    let block = CryptoHash::new([7; 32]);

    let before = validator_set_state_reads::count();
    // Six of ten (quorum seven): no PC forms, so only the collect path is measured.
    for (i, key) in keys[..6].iter().enumerate() {
        if i == 3 {
            // A block-tree write that touches no validator-set key, mid-view.
            tree.set_highest_view_entered(ViewNumber::new(5)).unwrap();
        }
        send_vote(&mut hotstuff, &mut tree, key, 5, block, Phase::Generic);
    }
    let reads = validator_set_state_reads::count() - before;
    assert!(tree.highest_pc().unwrap().is_genesis_pc(), "fixture: six of ten votes form no PC");
    assert_eq!(
        reads, 1,
        "6 votes in a steady view must read the validator-set state once (the first vote after \
         construction), not once per vote: got {reads} reads"
    );
}

#[test]
fn an_out_of_band_update_mid_view_is_seen_by_the_next_vote_only() {
    // {n0} adds n1 and n2 with one update U.
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
    let decide = certify(10, u.hash, Phase::Decide, &keys, &new_set);

    let (mut tree, _) = steady_block_tree(&old_set);
    tree.insert(&a, None, None).unwrap();
    tree.update(&qc_a, &None).unwrap();
    let mut hotstuff = hotstuff_at(ViewNumber::new(10), keys[2].clone(), tree.validator_set_state().unwrap());

    let before = validator_set_state_reads::count();
    // n1 is not in the old set: its vote is counted by no collector, but it syncs them.
    send_vote(&mut hotstuff, &mut tree, &keys[1], 10, u.hash, Phase::Decide);
    assert_eq!(validator_set_state_reads::count() - before, 1, "the first vote syncs the collectors");

    // Block sync commits and decides U out of band, mid-view.
    tree.insert(&u, None, Some(&updates)).unwrap();
    tree.update(&commit, &None).unwrap();
    tree.update(&decide, &None).unwrap();
    let state = tree.validator_set_state().unwrap();
    assert!(state.update_decided() && state.committed_validator_set() == &new_set, "fixture: U committed and decided");

    let before = validator_set_state_reads::count();
    send_vote(&mut hotstuff, &mut tree, &keys[0], 10, u.hash, Phase::Decide);
    assert!(
        tree.highest_pc().unwrap() == decide,
        "n0's lone vote must be counted over the post-update set (no 1-signature old-set Decide PC)"
    );
    assert_eq!(validator_set_state_reads::count() - before, 1, "the vote after the update re-syncs the collectors");

    let before = validator_set_state_reads::count();
    send_vote(&mut hotstuff, &mut tree, &keys[2], 10, u.hash, Phase::Decide);
    assert!(tree.highest_pc().unwrap() == decide, "two of three post-update votes form no PC");
    let reads = validator_set_state_reads::count() - before;
    assert_eq!(reads, 0, "the state is unchanged since the last sync: the next vote must not read it, got {reads}");
}

#[test]
fn collectors_synced_against_one_tree_are_refreshed_against_another() {
    let keys = signing_keys(&[1, 2, 3]);
    let old_set = validator_set(&keys[..1]);
    let new_set = validator_set(&keys);
    // Both trees made the same validator-set writes (one `initialize`), over different sets.
    let (mut old_tree, old_vss) = steady_block_tree(&old_set);
    let (mut new_tree, _) = steady_block_tree(&new_set);
    let mut hotstuff = hotstuff_at(ViewNumber::new(11), keys[2].clone(), old_vss);
    let block = CryptoHash::new([12; 32]);

    // n1 is not in the old set: no collector counts it, but it syncs them against `old_tree`.
    send_vote(&mut hotstuff, &mut old_tree, &keys[1], 11, block, Phase::Generic);
    // n0's vote alone is an old-set quorum, and no quorum of the new set.
    send_vote(&mut hotstuff, &mut new_tree, &keys[0], 11, block, Phase::Generic);
    assert!(
        new_tree.highest_pc().unwrap().is_genesis_pc(),
        "the collectors must be over `new_tree`'s set when n0's vote is counted"
    );
}
