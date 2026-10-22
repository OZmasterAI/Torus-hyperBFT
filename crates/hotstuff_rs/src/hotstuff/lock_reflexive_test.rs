//! s138: the lock rule is reflexive. A proposal of the locked block itself satisfies the lock,
//! as in PODC'19 HotStuff's `safeNode` ("node extends lockedQC.node", reflexive), even though
//! the block's own justify (its parent's PC) is older than the lock and does not extend it.
//!
//! Without it a validator-set-updating block U whose Precommit PC formed at one replica (which
//! locks on U) and whose next view then timed out can never be certified again: a Precommit PC
//! is nudged only in the view right after it (`safe_nudge` 4), so later leaders re-propose U
//! (`repropose_block`), and the replica locked on U refuses U itself (the update-window livelock,
//! `validator_set_update_survives_a_silent_leader_after_the_precommit_pc`).
//!
//! The block hash binds its justify (`Block::hash(height, justify, data_hash)`), so "the locked
//! block itself" always comes with the locked block's own justify; any other block carrying that
//! stale justify is a sibling and stays refused.

use crate::block_tree::accessors::internal::{BlockTreeSingleton, BlockTreeWriteBatch};
use crate::block_tree::invariants::{safe_block, safe_proposal_lock};
use crate::hotstuff::header_fast_path_regression_test::{
    bare_tc, generic_pc, hotstuff_at, proposer_for, signing_keys, steady_block_tree, validator_set,
    MemKV, MemWb,
};
use crate::hotstuff::messages::{HotStuffMessage, ProposalHeader};
use crate::hotstuff::types::{Phase, PhaseCertificate};
use crate::app::{App, ProduceBlockRequest, ProduceBlockResponse, ValidateBlockRequest, ValidateBlockResponse};
use crate::pacemaker::types::TimeoutCertificate;
use crate::test_keys::signed_by;
use crate::types::block::Block;
use crate::types::crypto_primitives::{Keypair, SigningKey};
use crate::types::data_types::{
    BlockHeight, ChainID, CryptoHash, Data, Power, SignatureBytes, SignatureSet, ViewNumber,
};
use crate::types::signing::phase_vote_bytes;
use crate::types::update_sets::ValidatorSetUpdates;
use crate::types::validator_set::{ValidatorSet, ValidatorSetState};

const CHAIN_ID: ChainID = ChainID::new(0);

/// A quorum-signed PC of `phase` for `block` in `view`.
fn phase_pc(view: u64, block: CryptoHash, phase: Phase, signers: &[SigningKey], set: &ValidatorSet) -> PhaseCertificate {
    let view = ViewNumber::new(view);
    let message = phase_vote_bytes(CHAIN_ID, view, &block, phase);
    let mut signatures = SignatureSet::new(set.len());
    for sk in signers {
        let pos = set.position(&sk.verifying_key()).expect("signer in the set");
        signatures.set(pos, Some(Keypair::new(sk.clone()).sign(&message)));
    }
    PhaseCertificate { chain_id: CHAIN_ID, view, block, phase, signatures }
}

/// A <- U, U validator-set-updating, the replica locked on U's Precommit PC (view 3), which is
/// also its highest PC. U justifies on QC(A) at view 1.
struct LockedOnUpdate {
    keys: Vec<SigningKey>,
    set: ValidatorSet,
    vss: ValidatorSetState,
    block_tree: BlockTreeSingleton<MemKV>,
    qc_a: PhaseCertificate,
    u: Block,
}

fn locked_on_update_block() -> LockedOnUpdate {
    let keys = signing_keys(&[1, 2, 3, 4]);
    let set = validator_set(&keys);
    let (mut block_tree, vss) = steady_block_tree(&set);
    let a = Block::new(BlockHeight::new(0), PhaseCertificate::genesis_pc(), CryptoHash::new([10u8; 32]), Data::new(vec![]));
    block_tree.insert(&a, None, None).expect("insert A");
    let qc_a = generic_pc(ViewNumber::new(1), a.hash, &keys[..3], &set);
    let u = Block::new(BlockHeight::new(1), qc_a.clone(), CryptoHash::new([11u8; 32]), Data::new(vec![]));
    let mut updates = ValidatorSetUpdates::new();
    updates.insert(keys[1].verifying_key(), Power::new(2));
    block_tree.insert(&u, None, Some(&updates)).expect("insert U");
    let lock = phase_pc(3, u.hash, Phase::Precommit, &keys[..3], &set);
    let mut wb: BlockTreeWriteBatch<MemWb> = BlockTreeWriteBatch::new();
    wb.set_locked_pc(&lock).expect("set_locked_pc");
    wb.set_highest_pc(&lock).expect("set_highest_pc");
    block_tree.write(wb);
    LockedOnUpdate { keys, set, vss, block_tree, qc_a, u }
}

/// The re-proposal of the locked block U (its own justify, QC(A) at view 1, is older than the
/// lock at view 3 and does not extend U) satisfies the lock.
///
/// RED before the fix: the lock clause looked only at the justify.
#[test]
fn reproposal_of_the_locked_block_is_safe() {
    let f = locked_on_update_block();
    assert!(
        safe_block(&f.u, &f.block_tree, CHAIN_ID).expect("safe_block"),
        "a proposal of the locked block itself must satisfy the lock"
    );
}

/// A sibling of the locked block at the same height, carrying the same stale justify (QC(A),
/// view 1 < lock view 3), conflicts with the lock and is refused: the reflexive case is the
/// locked block's hash only, and that hash binds U's own justify.
#[test]
fn sibling_of_the_locked_block_with_its_stale_justify_is_refused() {
    let f = locked_on_update_block();
    let sibling = Block::new(BlockHeight::new(1), f.qc_a.clone(), CryptoHash::new([12u8; 32]), Data::new(vec![]));
    let mut updates = ValidatorSetUpdates::new();
    updates.insert(f.keys[2].verifying_key(), Power::new(2));
    let mut block_tree = f.block_tree;
    block_tree.insert(&sibling, None, Some(&updates)).expect("insert sibling");
    assert!(
        !safe_block(&sibling, &block_tree, CHAIN_ID).expect("safe_block"),
        "a conflicting sibling of the locked block must not satisfy the lock"
    );
}

/// A descendant of the locked block (justified by a PC on U, at a view not above the lock) is
/// safe through the "extends" arm, unchanged by the fix.
#[test]
fn descendant_of_the_locked_block_is_safe() {
    let f = locked_on_update_block();
    let decide_u = phase_pc(3, f.u.hash, Phase::Decide, &f.keys[..3], &f.set);
    let child = Block::new(BlockHeight::new(2), decide_u, CryptoHash::new([13u8; 32]), Data::new(vec![]));
    assert!(
        safe_block(&child, &f.block_tree, CHAIN_ID).expect("safe_block"),
        "a child of the locked block must satisfy the lock"
    );
}

/// The reflexive case covers an UNCOMMITTED locked block only. Once the locked block is
/// committed (a Commit / Decide PC locks on it and commits it in the same update) no honest
/// leader re-proposes it (a Commit PC is nudged in any view), and re-processing a committed
/// block's stale justify (re-insert, Decide flag) is not needed: the old rule stays.
#[test]
fn reproposal_of_a_committed_locked_block_is_refused() {
    let f = locked_on_update_block();
    let mut block_tree = f.block_tree;
    let mut wb: BlockTreeWriteBatch<MemWb> = BlockTreeWriteBatch::new();
    wb.set_highest_committed_block(&f.u.hash).expect("set_highest_committed_block");
    block_tree.write(wb);
    assert!(
        !safe_block(&f.u, &block_tree, CHAIN_ID).expect("safe_block"),
        "a committed locked block's stale justify must not satisfy the lock"
    );
}

/// The blocks under test are already in the tree, so no body is validated; any other body is
/// parked (`MissingData`).
struct TreeApp;

impl App<MemKV> for TreeApp {
    fn produce_block(&mut self, _request: ProduceBlockRequest<MemKV>) -> ProduceBlockResponse {
        unreachable!("produce_block is not reached on the header path under test")
    }
    fn validate_block(&mut self, _request: ValidateBlockRequest<MemKV>) -> ValidateBlockResponse {
        ValidateBlockResponse::MissingData
    }
    fn validate_block_for_sync(&mut self, _request: ValidateBlockRequest<MemKV>) -> ValidateBlockResponse {
        unreachable!("validate_block_for_sync is not reached on the header path under test")
    }
}

fn reproposal_header(block: &Block, justify: PhaseCertificate, view: u64, tc: TimeoutCertificate) -> ProposalHeader {
    ProposalHeader {
        chain_id: CHAIN_ID,
        view: ViewNumber::new(view),
        block_hash: block.hash,
        height: block.height,
        data_hash: block.data_hash,
        justify,
        tc: Some(tc),
        nec: None,
        has_validator_set_updates: true,
        leader_signature: SignatureBytes::new([0; 64]),
    }
}

/// Header path (the production path; `on_receive_proposal_header` and `cast_header_vote`): the
/// leader of view 5 re-proposes U under TC(4); the replica locked on U's Precommit PC votes
/// Prepare for it.
///
/// RED before the fix: the header was dropped (`is_safe=false`), no vote in view 5.
#[test]
fn header_reproposal_of_the_locked_block_is_voted() {
    let f = locked_on_update_block();
    let mut block_tree = f.block_tree;
    let view = 5;
    let tc = bare_tc(ViewNumber::new(view - 1), &f.keys[..3], &f.set);
    let header = reproposal_header(&f.u, f.qc_a.clone(), view, tc);
    let leader = proposer_for(ViewNumber::new(view), &f.keys, &f.vss, &block_tree);
    let mut hotstuff = hotstuff_at(ViewNumber::new(view), f.keys[0].clone(), f.vss.clone());
    hotstuff
        .on_receive_msg(HotStuffMessage::ProposalHeader(signed_by(header, &leader)), &leader, &mut block_tree, &mut TreeApp)
        .expect("processing the re-proposal header must not error");
    assert_eq!(
        block_tree.highest_view_voted().expect("highest_view_voted"),
        Some(ViewNumber::new(view)),
        "the replica locked on U must vote for U's re-proposal"
    );
}

/// Header path: a header that names U's hash with a different justify is dropped on the hash
/// check, before any lock rule (the hash binds the justify), so the reflexive case cannot be
/// borrowed by a header that does not carry U's own justify.
#[test]
fn header_with_the_locked_hash_and_another_justify_is_dropped() {
    let f = locked_on_update_block();
    let mut block_tree = f.block_tree;
    let view = 5;
    let tc = bare_tc(ViewNumber::new(view - 1), &f.keys[..3], &f.set);
    let other_justify = generic_pc(ViewNumber::new(2), f.qc_a.block, &f.keys[..3], &f.set);
    let header = reproposal_header(&f.u, other_justify, view, tc);
    let leader = proposer_for(ViewNumber::new(view), &f.keys, &f.vss, &block_tree);
    let mut hotstuff = hotstuff_at(ViewNumber::new(view), f.keys[0].clone(), f.vss.clone());
    hotstuff
        .on_receive_msg(HotStuffMessage::ProposalHeader(signed_by(header, &leader)), &leader, &mut block_tree, &mut TreeApp)
        .expect("processing the header must not error");
    assert_eq!(block_tree.highest_view_voted().expect("highest_view_voted"), None, "no vote for a header whose hash does not bind its justify");
}

/// Header path: a sibling of U re-using U's stale justify is not voted (the lock holds).
#[test]
fn header_sibling_of_the_locked_block_is_not_voted() {
    let f = locked_on_update_block();
    let mut block_tree = f.block_tree;
    let sibling = Block::new(BlockHeight::new(1), f.qc_a.clone(), CryptoHash::new([12u8; 32]), Data::new(vec![]));
    let mut updates = ValidatorSetUpdates::new();
    updates.insert(f.keys[2].verifying_key(), Power::new(2));
    block_tree.insert(&sibling, None, Some(&updates)).expect("insert sibling");
    let view = 5;
    let tc = bare_tc(ViewNumber::new(view - 1), &f.keys[..3], &f.set);
    let header = reproposal_header(&sibling, f.qc_a.clone(), view, tc);
    let leader = proposer_for(ViewNumber::new(view), &f.keys, &f.vss, &block_tree);
    let mut hotstuff = hotstuff_at(ViewNumber::new(view), f.keys[0].clone(), f.vss.clone());
    hotstuff
        .on_receive_msg(HotStuffMessage::ProposalHeader(signed_by(header, &leader)), &leader, &mut block_tree, &mut TreeApp)
        .expect("processing the sibling header must not error");
    assert_eq!(block_tree.highest_view_voted().expect("highest_view_voted"), None, "the lock on U must refuse its sibling");
}

/// `safe_proposal_lock` (the header fast path's pending-justify branch and `cast_header_vote`)
/// needs no tree membership: a locked block whose body is still in flight is not committed, so
/// its re-proposal satisfies the lock; another hash with the same stale justify does not.
#[test]
fn proposal_lock_without_the_block_in_the_tree() {
    let keys = signing_keys(&[1, 2, 3, 4]);
    let set = validator_set(&keys);
    let (mut block_tree, _vss) = steady_block_tree(&set);
    let locked = CryptoHash::new([21u8; 32]); // NOT in the block tree (body in flight)
    let other = CryptoHash::new([22u8; 32]);
    let stale = generic_pc(ViewNumber::new(1), CryptoHash::new([20u8; 32]), &keys[..3], &set);

    // Genesis lock: any PC above view 0 satisfies it (view arm).
    assert!(safe_proposal_lock(&other, &stale, &block_tree).expect("lock"));

    let lock = phase_pc(3, locked, Phase::Precommit, &keys[..3], &set);
    let mut wb: BlockTreeWriteBatch<MemWb> = BlockTreeWriteBatch::new();
    wb.set_locked_pc(&lock).expect("set_locked_pc");
    block_tree.write(wb);
    assert!(
        safe_proposal_lock(&locked, &stale, &block_tree).expect("lock"),
        "the uncommitted locked block (body pending) satisfies the lock with its stale justify"
    );
    assert!(
        !safe_proposal_lock(&other, &stale, &block_tree).expect("lock"),
        "another block with a stale justify that does not extend the lock must fail"
    );
}
