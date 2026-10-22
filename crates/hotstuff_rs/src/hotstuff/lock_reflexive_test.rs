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
        safe_block(&f.u, None, &f.block_tree, CHAIN_ID).expect("safe_block"),
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
        !safe_block(&sibling, None, &block_tree, CHAIN_ID).expect("safe_block"),
        "a conflicting sibling of the locked block must not satisfy the lock"
    );
}

/// The locked fixture plus a sibling U' (same parent and justify QC(A), another hash, inserted)
/// and `highest` as the replica's highest PC.
fn locked_with_sibling(highest: impl FnOnce(&LockedOnUpdate, CryptoHash) -> PhaseCertificate) -> (LockedOnUpdate, Block) {
    let mut f = locked_on_update_block();
    let sibling = Block::new(BlockHeight::new(1), f.qc_a.clone(), CryptoHash::new([12u8; 32]), Data::new(vec![]));
    let mut updates = ValidatorSetUpdates::new();
    updates.insert(f.keys[2].verifying_key(), Power::new(2));
    f.block_tree.insert(&sibling, None, Some(&updates)).expect("insert sibling");
    let pc = highest(&f, sibling.hash);
    let mut wb: BlockTreeWriteBatch<MemWb> = BlockTreeWriteBatch::new();
    wb.set_highest_pc(&pc).expect("set_highest_pc");
    f.block_tree.write(wb);
    (f, sibling)
}

/// Codex P1 on 8af2b949 (the sibling livelock): a sibling U' that a PC NEWER than the lock
/// certifies (Prepare(U', 4) > Precommit(U, 3), this replica's highest PC) satisfies the lock,
/// as a block justified by such a PC does (standard HotStuff unlock): that PC exists, so no
/// block conflicting with U' can have been committed before its view, U included.
///
/// RED before the fix: the lock looked at U''s stale justify and the locked block only.
#[test]
fn sibling_certified_above_the_lock_by_the_highest_pc_is_safe() {
    let (f, sibling) = locked_with_sibling(|f, sibling| phase_pc(4, sibling, Phase::Prepare, &f.keys[1..], &f.set));
    assert!(
        safe_block(&sibling, None, &f.block_tree, CHAIN_ID).expect("safe_block"),
        "a sibling certified above the lock must satisfy it"
    );
}

/// A PC on the sibling at the lock's view (only a Byzantine equivocation can form one: one honest
/// vote per view) does not unlock: the rule is strictly newer than the lock, like `pc_to_lock`.
#[test]
fn sibling_certified_at_the_lock_view_is_refused() {
    let (f, sibling) = locked_with_sibling(|f, sibling| phase_pc(3, sibling, Phase::Prepare, &f.keys[1..], &f.set));
    assert!(
        !safe_block(&sibling, None, &f.block_tree, CHAIN_ID).expect("safe_block"),
        "a PC on the sibling that is not newer than the lock must not unlock"
    );
}

/// A highest PC above the lock on ANOTHER block does not admit the sibling: the PC must certify
/// the proposed block itself (U' extends U''s certificate, nothing else).
#[test]
fn sibling_with_a_newer_pc_on_another_block_is_refused() {
    let (f, sibling) = locked_with_sibling(|f, _| phase_pc(4, f.u.hash, Phase::Prepare, &f.keys[1..], &f.set));
    assert!(
        !safe_block(&sibling, None, &f.block_tree, CHAIN_ID).expect("safe_block"),
        "a newer PC on U does not make U' safe"
    );
}

/// The certified arm also takes the attached TC's high QC (verified with the TC): this replica
/// never learned Prepare(U', 4) (its highest PC is its lock), the TC carrying the re-proposal
/// reports it. A TC high QC on U' at the lock's view does not unlock.
#[test]
fn sibling_certified_above_the_lock_by_the_tc_high_qc_is_safe() {
    let (f, sibling) = locked_with_sibling(|f, _| phase_pc(3, f.u.hash, Phase::Precommit, &f.keys[..3], &f.set));
    let prepare4 = phase_pc(4, sibling.hash, Phase::Prepare, &f.keys[1..], &f.set);
    let prepare3 = phase_pc(3, sibling.hash, Phase::Prepare, &f.keys[1..], &f.set);
    assert!(!safe_block(&sibling, None, &f.block_tree, CHAIN_ID).expect("safe_block"), "no certificate above the lock");
    assert!(
        safe_block(&sibling, Some(&prepare4), &f.block_tree, CHAIN_ID).expect("safe_block"),
        "the TC's high QC certifies the sibling above the lock"
    );
    assert!(
        !safe_block(&sibling, Some(&prepare3), &f.block_tree, CHAIN_ID).expect("safe_block"),
        "a TC high QC at the lock's view does not unlock"
    );
}

/// Header path (production): the leader of view 5 re-proposes U' under TC(4), whose high QC is
/// Prepare(U', 4); the replica locked on Precommit(U, 3), which never learned Prepare(U', 4),
/// votes Prepare for it. Without a newer certificate the same header is refused
/// (`header_sibling_of_the_locked_block_is_not_voted`).
#[test]
fn header_reproposal_of_a_sibling_certified_by_the_tc_is_voted() {
    let (f, sibling) = locked_with_sibling(|f, _| phase_pc(3, f.u.hash, Phase::Precommit, &f.keys[..3], &f.set));
    let mut block_tree = f.block_tree;
    let view = 5;
    let prepare4 = phase_pc(4, sibling.hash, Phase::Prepare, &f.keys[1..], &f.set);
    let tc = tc_reporting(view - 1, &prepare4, &f.keys[1..], &f.set);
    let header = reproposal_header(&sibling, f.qc_a.clone(), view, tc);
    let leader = proposer_for(ViewNumber::new(view), &f.keys, &f.vss, &block_tree);
    let mut hotstuff = hotstuff_at(ViewNumber::new(view), f.keys[0].clone(), f.vss.clone());
    hotstuff
        .on_receive_msg(HotStuffMessage::ProposalHeader(signed_by(header, &leader)), &leader, &mut block_tree, &mut TreeApp)
        .expect("processing the sibling's re-proposal header must not error");
    assert_eq!(
        block_tree.highest_view_voted().expect("highest_view_voted"),
        Some(ViewNumber::new(view)),
        "the replica locked on U must vote for U' certified above its lock"
    );
    assert_eq!(block_tree.locked_pc().unwrap().block, f.u.hash, "voting Prepare moves no lock");
}

/// A descendant of the locked block (justified by a PC on U, at a view not above the lock) is
/// safe through the "extends" arm, unchanged by the fix.
#[test]
fn descendant_of_the_locked_block_is_safe() {
    let f = locked_on_update_block();
    let decide_u = phase_pc(3, f.u.hash, Phase::Decide, &f.keys[..3], &f.set);
    let child = Block::new(BlockHeight::new(2), decide_u, CryptoHash::new([13u8; 32]), Data::new(vec![]));
    assert!(
        safe_block(&child, None, &f.block_tree, CHAIN_ID).expect("safe_block"),
        "a child of the locked block must satisfy the lock"
    );
}

/// The reflexive case covers a COMMITTED locked block too (Codex P1 on 93948e2b): a replica that
/// committed U through a Commit PC the others missed must still vote for U's re-proposal, or the
/// others (who re-certify U in later views) can never form a quorum without the Byzantine voter.
///
/// RED before the fix: committed locked blocks were excluded.
#[test]
fn reproposal_of_a_committed_locked_block_is_safe() {
    let f = locked_on_update_block();
    let mut block_tree = f.block_tree;
    let mut wb: BlockTreeWriteBatch<MemWb> = BlockTreeWriteBatch::new();
    wb.set_highest_committed_block(&f.u.hash).expect("set_highest_committed_block");
    block_tree.write(wb);
    assert!(
        safe_block(&f.u, None, &block_tree, CHAIN_ID).expect("safe_block"),
        "a proposal of the committed locked block itself must satisfy the lock"
    );
}

/// A sibling of a COMMITTED locked block, with the locked block's stale justify, is still refused.
#[test]
fn sibling_of_a_committed_locked_block_is_refused() {
    let f = locked_on_update_block();
    let mut block_tree = f.block_tree;
    let mut wb: BlockTreeWriteBatch<MemWb> = BlockTreeWriteBatch::new();
    wb.set_highest_committed_block(&f.u.hash).expect("set_highest_committed_block");
    block_tree.write(wb);
    let sibling = Block::new(BlockHeight::new(1), f.qc_a.clone(), CryptoHash::new([12u8; 32]), Data::new(vec![]));
    assert!(
        !safe_block(&sibling, None, &block_tree, CHAIN_ID).expect("safe_block"),
        "a sibling of the committed locked block must not satisfy the lock"
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
    assert!(safe_proposal_lock(&other, &stale, None, &block_tree).expect("lock"));

    let lock = phase_pc(3, locked, Phase::Precommit, &keys[..3], &set);
    let mut wb: BlockTreeWriteBatch<MemWb> = BlockTreeWriteBatch::new();
    wb.set_locked_pc(&lock).expect("set_locked_pc");
    block_tree.write(wb);
    assert!(
        safe_proposal_lock(&locked, &stale, None, &block_tree).expect("lock"),
        "the uncommitted locked block (body pending) satisfies the lock with its stale justify"
    );
    assert!(
        !safe_proposal_lock(&other, &stale, None, &block_tree).expect("lock"),
        "another block with a stale justify that does not extend the lock must fail"
    );
}

// ---------------------------------------------------------------------------
// Codex P1 on 93948e2b: a partially committed update (mixed commit).
// ---------------------------------------------------------------------------

/// A TC for `view` from `signers`' timeout votes, each reporting `highest` as its highest PC.
fn tc_reporting(view: u64, highest: &PhaseCertificate, signers: &[SigningKey], set: &ValidatorSet) -> TimeoutCertificate {
    use crate::pacemaker::messages::PacemakerMessage;
    use crate::pacemaker::types::TimeoutVoteCollector;
    use crate::types::signed_messages::Collector;
    let mut collector = TimeoutVoteCollector::new(CHAIN_ID, ViewNumber::new(view), set.clone());
    let mut tc = None;
    for key in signers {
        let PacemakerMessage::TimeoutVote(vote) = PacemakerMessage::timeout_vote(
            &Keypair::new(key.clone()),
            CHAIN_ID,
            ViewNumber::new(view),
            None,
            None,
            Some(highest.clone()),
        ) else {
            unreachable!()
        };
        tc = collector.collect(&key.verifying_key(), vote).or(tc);
    }
    tc.expect("signers form a quorum")
}

/// One honest replica's block tree in the mixed-commit sequence. 4 equal validators, keys[3] is
/// the Byzantine one. A <- U (U validator-set-updating, justified by `u_justify`); every replica
/// saw U's Precommit PC (view 3) and locked on U. U's Commit PC (view 4) reached only the
/// replicas with `committed`. The uncommitted honest replicas then voted Prepare for U's
/// re-proposal in view 7 (TC(6)); with the Byzantine vote that is Prepare(U, 7), which reaches
/// every honest replica (a TC / sync summary) and replaces Commit(4) as the committed replica's
/// highest PC. View 8's nudge of Prepare(7) is lost, so the leader of view 9 re-proposes U.
struct MixedCommit {
    keys: Vec<SigningKey>,
    block_tree: BlockTreeSingleton<MemKV>,
    u: Block,
    /// TC(8), reporting Prepare(U, 7).
    tc8: TimeoutCertificate,
}

/// `a_updates`: A's own validator-set updates (A then has a Decide PC at view 2, which is U's
/// justify); `None`: A is a plain block and U justifies on QC(A) at view 1.
fn mixed_commit_replica(replica: usize, committed: bool, a_updates: bool) -> MixedCommit {
    use crate::types::signed_messages::Certificate;
    let keys = signing_keys(&[1, 2, 3, 4]);
    let set = validator_set(&keys);
    let (mut block_tree, _vss) = steady_block_tree(&set);
    let a = Block::new(BlockHeight::new(0), PhaseCertificate::genesis_pc(), CryptoHash::new([10u8; 32]), Data::new(vec![]));
    let u_justify = if a_updates {
        // keys[2] signs both the honest-quorum PCs (keys[..3]) and Prepare(7) (keys[1..]), so
        // both stay a quorum under A's powers.
        let mut updates = ValidatorSetUpdates::new();
        updates.insert(keys[2].verifying_key(), Power::new(2));
        block_tree.insert(&a, None, Some(&updates)).expect("insert A");
        let decide_a = phase_pc(2, a.hash, Phase::Decide, &keys[..3], &set);
        block_tree.update(&decide_a, &None).expect("A's Decide PC");
        decide_a
    } else {
        block_tree.insert(&a, None, None).expect("insert A");
        generic_pc(ViewNumber::new(1), a.hash, &keys[..3], &set)
    };
    let set = block_tree.committed_validator_set().expect("committed set");
    let u = Block::new(BlockHeight::new(1), u_justify, CryptoHash::new([11u8; 32]), Data::new(vec![]));
    let mut updates = ValidatorSetUpdates::new();
    updates.insert(keys[1].verifying_key(), Power::new(2));
    block_tree.insert(&u, None, Some(&updates)).expect("insert U");
    block_tree.update(&phase_pc(3, u.hash, Phase::Precommit, &keys[..3], &set), &None).expect("U's Precommit PC");
    if committed {
        block_tree.update(&phase_pc(4, u.hash, Phase::Commit, &keys[..3], &set), &None).expect("U's Commit PC");
        assert_eq!(block_tree.highest_committed_block().unwrap(), Some(u.hash), "fixture: U committed");
        assert!(!block_tree.validator_set_state().unwrap().update_decided(), "fixture: U's update not decided");
    }
    // Prepare(U, 7): the uncommitted honest replicas 1, 2 and the Byzantine keys[3].
    let prepare7 = phase_pc(7, u.hash, Phase::Prepare, &keys[1..], &set);
    assert!(prepare7.is_correct(CHAIN_ID, &block_tree).unwrap(), "fixture: Prepare(7) verifies at replica {replica}");
    block_tree.advance_highest_pc_from_remote(&prepare7, &None).expect("Prepare(7) via a TC summary");
    let tc8 = tc_reporting(8, &prepare7, &keys[..3], &set);
    MixedCommit { keys, block_tree, u, tc8 }
}

/// The re-proposal of U in view 9 (justify: U's own; TC(8)) as a header, signed by leader(9).
fn reproposal_in_view_9(f: &MixedCommit) -> (ProposalHeader, crate::types::crypto_primitives::VerifyingKey) {
    let vss = f.block_tree.validator_set_state().unwrap();
    let leader = proposer_for(ViewNumber::new(9), &f.keys, &vss, &f.block_tree);
    let header = reproposal_header(&f.u, f.u.justify.clone(), 9, f.tc8.clone());
    (signed_by(header, &leader), leader)
}

/// Codex P1 on 93948e2b, header path (production): in the mixed-commit sequence the committed
/// honest replica and the two uncommitted ones all vote Prepare for U's re-proposal in view 9,
/// so the honest replicas alone are a quorum (the Byzantine one withholds). The re-proposal is
/// what every later leader sends (`repropose_block` on Prepare(7)).
///
/// RED before the fix: the committed replica refused (committed locked blocks were excluded), 2
/// honest votes against a quorum of 3, and every later view repeats it.
#[test]
fn mixed_commit_reproposal_gets_every_honest_vote() {
    let mut votes = Vec::new();
    for (replica, committed) in [(0, true), (1, false), (2, false)] {
        let mut f = mixed_commit_replica(replica, committed, false);
        assert_eq!(
            crate::block_tree::invariants::repropose_block(ViewNumber::new(9), &f.block_tree).unwrap(),
            Some(f.u.hash),
            "fixture: leaders re-propose U on Prepare(7)"
        );
        let (header, leader) = reproposal_in_view_9(&f);
        let vss = f.block_tree.validator_set_state().unwrap();
        let mut hotstuff = hotstuff_at(ViewNumber::new(9), f.keys[replica].clone(), vss);
        hotstuff
            .on_receive_msg(HotStuffMessage::ProposalHeader(header), &leader, &mut f.block_tree, &mut TreeApp)
            .expect("processing the re-proposal header must not error");
        if f.block_tree.highest_view_voted().unwrap() == Some(ViewNumber::new(9)) {
            votes.push(replica);
        }
        if committed {
            assert_eq!(f.block_tree.highest_committed_block().unwrap(), Some(f.u.hash), "U stays the committed tip");
            assert!(!f.block_tree.validator_set_state().unwrap().update_decided(), "voting decides nothing");
        }
    }
    assert_eq!(votes, vec![0, 1, 2], "every honest replica votes Prepare for U in view 9 (quorum 3 without the Byzantine)");
}

/// Codex P1, header path, when U's justify is a Decide PC (A was itself validator-set-updating):
/// the committed replica votes for U's re-proposal without re-applying that stale justify, which
/// would mark U's still-undecided update as decided (`update_locks_only` sets the decided flag on
/// any Decide PC).
#[test]
fn committed_locked_block_vote_does_not_reapply_a_decide_justify() {
    let mut f = mixed_commit_replica(0, true, true);
    assert!(f.u.justify.phase.is_decide(), "fixture: U justifies on A's Decide PC");
    let (header, leader) = reproposal_in_view_9(&f);
    let vss = f.block_tree.validator_set_state().unwrap();
    let mut hotstuff = hotstuff_at(ViewNumber::new(9), f.keys[0].clone(), vss);
    hotstuff
        .on_receive_msg(HotStuffMessage::ProposalHeader(header), &leader, &mut f.block_tree, &mut TreeApp)
        .expect("processing the re-proposal header must not error");
    assert_eq!(f.block_tree.highest_view_voted().unwrap(), Some(ViewNumber::new(9)), "the committed replica votes");
    assert!(
        !f.block_tree.validator_set_state().unwrap().update_decided(),
        "U's update must stay undecided: only U's own Decide PC decides it"
    );
}

/// The app of the full-proposal test: a committed block must not be validated (or inserted) again.
struct NoRevalidationApp;

impl App<MemKV> for NoRevalidationApp {
    fn produce_block(&mut self, _request: ProduceBlockRequest<MemKV>) -> ProduceBlockResponse {
        unreachable!("produce_block is not reached on the proposal path under test")
    }
    fn validate_block(&mut self, _request: ValidateBlockRequest<MemKV>) -> ValidateBlockResponse {
        panic!("a committed block must not be validated (and re-inserted) again")
    }
    fn validate_block_for_sync(&mut self, _request: ValidateBlockRequest<MemKV>) -> ValidateBlockResponse {
        unreachable!("validate_block_for_sync is not reached on the proposal path under test")
    }
}

/// Codex P1, full-proposal path: the committed replica votes Prepare for U's full re-proposal in
/// view 9 and leaves the block tree as it was: U is not validated or inserted again (its pending
/// validator-set updates would come back, A would list U twice), its justify is not re-applied.
#[test]
fn full_reproposal_of_a_committed_locked_block_is_voted_without_reinsertion() {
    let mut f = mixed_commit_replica(0, true, true);
    let vss = f.block_tree.validator_set_state().unwrap();
    let leader = proposer_for(ViewNumber::new(9), &f.keys, &vss, &f.block_tree);
    let proposal = crate::hotstuff::messages::Proposal::signed(
        &crate::test_keys::key_of(&leader),
        CHAIN_ID,
        ViewNumber::new(9),
        f.u.clone(),
        Some(f.tc8.clone()),
        None,
    );
    let children_before = f.block_tree.children(&f.u.justify.block).unwrap().vec().clone();
    let mut hotstuff = hotstuff_at(ViewNumber::new(9), f.keys[0].clone(), vss);
    hotstuff
        .on_receive_msg(HotStuffMessage::Proposal(proposal), &leader, &mut f.block_tree, &mut NoRevalidationApp)
        .expect("processing the full re-proposal must not error");
    assert_eq!(f.block_tree.highest_view_voted().unwrap(), Some(ViewNumber::new(9)), "the committed replica votes");
    assert_eq!(f.block_tree.children(&f.u.justify.block).unwrap().vec(), &children_before, "U is not re-inserted");
    assert!(
        matches!(
            f.block_tree.validator_set_updates_status(&f.u.hash).unwrap(),
            crate::types::validator_set::ValidatorSetUpdatesStatus::Committed
        ),
        "U's updates stay committed"
    );
    assert_eq!(f.block_tree.highest_committed_block().unwrap(), Some(f.u.hash));
    assert!(!f.block_tree.validator_set_state().unwrap().update_decided(), "the stale Decide justify is not re-applied");
}
