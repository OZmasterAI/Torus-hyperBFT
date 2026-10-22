//! Certificates of another chain signed by the same validator keys
//! (fix/tc-embedded-chain-id).
//!
//! Every vote signs its `chain_id`, so a phase certificate formed on chain 7 is a
//! correctly signed quorum of our validators, just for chain 7. `safe_pc` rejects
//! such a PC (predicate 1), but several paths never reach `safe_pc`:
//!
//! - the high QC a `TimeoutCertificate` carries (`TimeoutCertificate::is_correct`
//!   checked only its signatures), which VALIDPROPOSAL V3 and the leader's case 3
//!   act on (the leader made it its highest PC);
//! - the justify of a signed tip (F2 checked the header signature and the hash
//!   only);
//! - a timeout vote's highest QC and fallback TC (pacemaker), and a synced block's
//!   justify (block sync), covered in their own modules.
//!
//! RED-first: on main (87eea189) every test here fails.

use crate::block_tree::accessors::internal::BlockTreeSingleton;
use crate::hotstuff::header_fast_path_regression_test::{generic_pc, signing_keys, steady_block_tree, validator_set, MemKV};
use crate::hotstuff::types::{Phase, PhaseCertificate};
use crate::pacemaker::messages::PacemakerMessage;
use crate::pacemaker::select_leader;
use crate::pacemaker::types::{TimeoutCertificate, TimeoutVoteCollector, TipInfo};
use crate::test_keys::tip_signed_by;
use crate::types::block::Block;
use crate::types::crypto_primitives::{Keypair, SigningKey};
use crate::types::data_types::{BlockHeight, ChainID, CryptoHash, Data, SignatureBytes, SignatureSet, ViewNumber};
use crate::types::signed_messages::{Certificate, Collector};
use crate::types::signing::phase_vote_bytes;
use crate::types::validator_set::ValidatorSet;

const CHAIN_ID: ChainID = ChainID::new(0);
const FOREIGN: ChainID = ChainID::new(7);

fn keys() -> Vec<SigningKey> {
    signing_keys(&[1, 2, 3, 4])
}

/// A Generic PC for `block` at `view` signed by `signers` on chain `chain_id`.
pub(crate) fn pc_on(
    chain_id: ChainID,
    view: u64,
    block: CryptoHash,
    signers: &[SigningKey],
    set: &ValidatorSet,
) -> PhaseCertificate {
    let view = ViewNumber::new(view);
    let message = phase_vote_bytes(chain_id, view, &block, Phase::Generic);
    let mut signatures = SignatureSet::new(set.len());
    for key in signers {
        let pos = set.position(&key.verifying_key()).unwrap();
        signatures.set(pos, Some(Keypair::new(key.clone()).sign(&message)));
    }
    PhaseCertificate { chain_id, view, block, phase: Phase::Generic, signatures }
}

/// A TC of chain `chain_id` for `view` from `signers`, signer `i` reporting `qcs[i]`.
pub(crate) fn tc_on(
    chain_id: ChainID,
    view: u64,
    signers: &[SigningKey],
    set: &ValidatorSet,
    qcs: &[Option<PhaseCertificate>],
) -> TimeoutCertificate {
    let mut collector = TimeoutVoteCollector::new(chain_id, ViewNumber::new(view), set.clone());
    let mut tc = None;
    for (key, qc) in signers.iter().zip(qcs) {
        let PacemakerMessage::TimeoutVote(vote) = PacemakerMessage::timeout_vote(
            &Keypair::new(key.clone()),
            chain_id,
            ViewNumber::new(view),
            None,
            None,
            qc.clone(),
        ) else {
            unreachable!()
        };
        tc = collector.collect(&key.verifying_key(), vote).or(tc);
    }
    tc.expect("signers form a quorum")
}

// The checks under test, in one place (RED: main's `is_correct` took no chain ID).
fn tc_correct(tc: &TimeoutCertificate, tree: &BlockTreeSingleton<MemKV>) -> bool {
    tc.is_correct(tree).unwrap()
}

fn pc_correct(pc: &PhaseCertificate, _chain_id: ChainID, tree: &BlockTreeSingleton<MemKV>) -> bool {
    pc.is_correct(tree).unwrap()
}

fn block_correct(block: &Block, _chain_id: ChainID, tree: &BlockTreeSingleton<MemKV>) -> bool {
    block.is_correct(tree).unwrap()
}

fn tip_correct(tip: &TipInfo, chain_id: ChainID, tree: &BlockTreeSingleton<MemKV>) -> bool {
    tip.is_correct(chain_id, tree).unwrap()
}

/// The control: a TC whose high QC is one of ours is correct.
#[test]
fn tc_with_a_high_qc_of_our_chain_is_correct() {
    let keys = keys();
    let set = validator_set(&keys);
    let (tree, _) = steady_block_tree(&set);
    let ours = generic_pc(ViewNumber::new(3), CryptoHash::new([9; 32]), &keys[..3], &set);
    let tc = tc_on(CHAIN_ID, 4, &keys[..3], &set, &[None, None, Some(ours)]);
    assert!(tc_correct(&tc, &tree));
}

/// A correctly signed chain-7 QC (higher view than any of ours) as the high QC
/// of a chain-0 TC: the TC is not correct.
#[test]
fn tc_with_a_high_qc_of_another_chain_is_not_correct() {
    let keys = keys();
    let set = validator_set(&keys);
    let (tree, _) = steady_block_tree(&set);
    let ours = generic_pc(ViewNumber::new(3), CryptoHash::new([9; 32]), &keys[..3], &set);
    let foreign = pc_on(FOREIGN, 900, CryptoHash::new([10; 32]), &keys[..3], &set);
    assert!(foreign.is_correctly_signed(&set), "precondition: a real quorum, of chain 7");
    let tc = tc_on(CHAIN_ID, 4, &keys[..3], &set, &[Some(ours.clone()), Some(ours), Some(foreign)]);
    assert_eq!(tc.high_qc.as_ref().unwrap().chain_id, FOREIGN, "precondition: chain 7's QC is the high QC");
    assert!(!tc_correct(&tc, &tree), "a TC must not carry another chain's QC as its high QC");
}

/// A chain-7 TC is not a correct certificate for chain 0, whatever it carries.
#[test]
fn tc_of_another_chain_is_not_correct() {
    let keys = keys();
    let set = validator_set(&keys);
    let (tree, _) = steady_block_tree(&set);
    let tc = tc_on(FOREIGN, 4, &keys[..3], &set, &[None, None, None]);
    assert!(!tc_correct(&tc, &tree));
}

/// A PC of another chain is not correct for ours; the genesis PC (chain 0 by
/// definition) is correct for every chain.
#[test]
fn pc_of_another_chain_is_not_correct_and_genesis_is_chain_neutral() {
    let keys = keys();
    let set = validator_set(&keys);
    let (tree, _) = steady_block_tree(&set);
    let foreign = pc_on(FOREIGN, 5, CryptoHash::new([10; 32]), &keys[..3], &set);
    assert!(!pc_correct(&foreign, CHAIN_ID, &tree));
    assert!(pc_correct(&foreign, FOREIGN, &tree), "the same PC is correct on its own chain");
    assert!(pc_correct(&PhaseCertificate::genesis_pc(), FOREIGN, &tree));
    assert!(pc_correct(&PhaseCertificate::genesis_pc(), CHAIN_ID, &tree));
}

/// A block justified by another chain's QC is not a correct block of ours.
#[test]
fn block_justified_by_another_chains_qc_is_not_correct() {
    let keys = keys();
    let set = validator_set(&keys);
    let (tree, _) = steady_block_tree(&set);
    let parent = CryptoHash::new([10; 32]);
    let foreign = pc_on(FOREIGN, 5, parent, &keys[..3], &set);
    let block = Block::new(BlockHeight::new(1), foreign, CryptoHash::new([11; 32]), Data::new(vec![]));
    assert!(!block_correct(&block, CHAIN_ID, &tree));
    let ours = pc_on(CHAIN_ID, 5, parent, &keys[..3], &set);
    let block = Block::new(BlockHeight::new(1), ours, CryptoHash::new([11; 32]), Data::new(vec![]));
    assert!(block_correct(&block, CHAIN_ID, &tree));
}

/// F2: a tip whose block justifies on another chain's QC is not a real header
/// of ours, even though leader(view) signed it for chain 0.
#[test]
fn tip_justified_by_another_chains_qc_is_not_correct() {
    let keys = keys();
    let set = validator_set(&keys);
    let (tree, _) = steady_block_tree(&set);
    let tip_for = |justify: PhaseCertificate| -> TipInfo {
        let block = Block::new(BlockHeight::new(1), justify, CryptoHash::new([12; 32]), Data::new(vec![]));
        let view = ViewNumber::new(6);
        let header = crate::hotstuff::messages::ProposalHeader {
            chain_id: CHAIN_ID,
            view,
            block_hash: block.hash,
            height: block.height,
            data_hash: block.data_hash,
            justify: block.justify.clone(),
            tc: None,
            nec: None,
            has_validator_set_updates: false,
            leader_signature: SignatureBytes::new([0; 64]),
        };
        tip_signed_by(header, &select_leader(view, &set))
    };
    let parent = CryptoHash::new([10; 32]);
    assert!(tip_correct(&tip_for(pc_on(CHAIN_ID, 5, parent, &keys[..3], &set)), CHAIN_ID, &tree));
    assert!(
        !tip_correct(&tip_for(pc_on(FOREIGN, 5, parent, &keys[..3], &set)), CHAIN_ID, &tree),
        "a tip must not justify on another chain's QC"
    );
}
