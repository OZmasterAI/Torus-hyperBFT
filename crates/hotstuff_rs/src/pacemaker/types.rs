/*
    Copyright © 2023, ParallelChain Lab
    Licensed under the Apache License, Version 2.0: http://www.apache.org/licenses/LICENSE-2.0
    MonadBFT extensions added by Torus Project.
*/

//! Types specific to the Pacemaker protocol.

use borsh::{BorshDeserialize, BorshSerialize};
use ed25519_dalek::Verifier;

use crate::hotstuff::types::PhaseCertificate;

use crate::{
    block_tree::{
        accessors::internal::{BlockTreeError, BlockTreeSingleton},
        pluggables::KVStore,
    },
    pacemaker::messages::TimeoutVote,
    types::{
        data_types::*,
        signed_messages::{Certificate, Collector},
        validator_set::*,
    },
};

/// Compact representation of a block header for timeout messages (MonadBFT).
#[derive(Clone, BorshSerialize, BorshDeserialize, PartialEq, Eq)]
pub struct TipInfo {
    pub block_hash: CryptoHash,
    pub block_height: BlockHeight,
    pub block_justify: PhaseCertificate,
    pub block_data_hash: CryptoHash,
    pub view: ViewNumber,
}

/// Cryptographic proof that at least a quorum of validators have sent a [`TimeoutVote`] for the same
/// view.
#[derive(Clone, BorshSerialize, BorshDeserialize, PartialEq, Eq)]
pub struct TimeoutCertificate {
    /// Chain ID of the blockchain that the validators that produced this `TimeoutCertificate` extends.
    pub chain_id: ChainID,

    /// View Number of the view that timed out.
    pub view: ViewNumber,

    /// Signatures of the `TimeoutVote`s that were collected to produce this `TimeoutCertificate`.
    pub signatures: SignatureSet,
    // MonadBFT fields
    pub high_tip: Option<TipInfo>,
    pub high_qc: Option<PhaseCertificate>,
    pub high_tip_is_winner: bool,

    /// Per-voter signed metadata for TC signature verification.
    /// Each entry corresponds to the same position as in `signatures`:
    /// (optional local_tip block hash, optional (highest_qc view, highest_qc block hash)).
    /// This enables TC verification to reconstruct per-voter message bytes.
    #[allow(clippy::type_complexity)]
    pub voter_metadata: Vec<(Option<CryptoHash>, Option<(ViewNumber, CryptoHash)>)>,
}

impl Certificate for TimeoutCertificate {
    type Vote = TimeoutVote;

    /// Checks if the signatures in the TC are correct and form a quorum for an appropriate validator set.
    ///
    /// During the speculation phase, i.e., when the new validator set has been committed, but the old
    /// validator set is still active, a TC is correct if it is correctly signed by a quorum from either
    /// of the two validator sets.
    fn is_correct<K: KVStore>(
        &self,
        block_tree: &BlockTreeSingleton<K>,
    ) -> Result<bool, BlockTreeError> {
        let validator_set_state = block_tree.validator_set_state()?;
        if validator_set_state.update_decided() {
            Ok(self.is_correctly_signed(validator_set_state.committed_validator_set()))
        } else {
            Ok(
                self.is_correctly_signed(validator_set_state.committed_validator_set())
                    || self.is_correctly_signed(validator_set_state.previous_validator_set()),
            )
        }
    }

    /// Checks if all of the signatures in the certificate are correct, and if the set of signatures forms
    /// a quorum.
    fn is_correctly_signed(&self, validator_set: &ValidatorSet) -> bool {
        // Check whether the size of the signature set is the same as the size of the validator set.
        if self.signatures.len() != validator_set.len() {
            return false;
        }

        let base_msg = (self.chain_id, self.view).try_to_vec().unwrap();

        // Check whether every signature is correct and tally up their powers.
        let mut total_power: TotalPower = TotalPower::new(0);
        for (idx, (signature, (_signer, power))) in self
            .signatures
            .iter()
            .zip(validator_set.validators_and_powers())
            .enumerate()
        {
            if let Some(signature) = signature {
                if let Ok(signature) = Signature::from_slice(&signature.bytes()) {
                    // Reconstruct the per-voter message bytes including their
                    // local_tip and highest_qc metadata (bound by their signature).
                    let msg = if idx < self.voter_metadata.len() {
                        let (ref tip_hash, ref qc_summary) = self.voter_metadata[idx];
                        let mut m = base_msg.clone();
                        if let Some(ref hash) = tip_hash {
                            m.extend_from_slice(&hash.bytes());
                        }
                        if let Some((qc_view, ref qc_block)) = qc_summary {
                            m.extend_from_slice(&qc_view.int().to_be_bytes());
                            m.extend_from_slice(&qc_block.bytes());
                        }
                        m
                    } else {
                        // Backward compatibility: no voter_metadata, use base message.
                        base_msg.clone()
                    };

                    if _signer.verify(&msg, &signature).is_ok() {
                        total_power += power;
                    } else {
                        // tc contains incorrect signature.
                        return false;
                    }
                } else {
                    // tc contains incorrect signature.
                    return false;
                }
            }
        }

        // Check if the signatures form a quorum.
        total_power >= validator_set.quorum()
    }
}

impl TimeoutCertificate {
    /// Pre-testnet P1 (re-proposal vote): whether the signers whose SIGNED
    /// local tip (`voter_metadata`, bound by their signature, see
    /// [`Certificate::is_correctly_signed`]) is `block_hash` hold more than
    /// the faulty power (`total - quorum + 1`, "f + 1") of the validator set
    /// this TC verifies under — so at least one honest replica voted for
    /// that block (its local tip is set only by a vote). The `high_tip`
    /// field itself is not signed, so it never counts. Precondition: the TC
    /// [is correct](Certificate::is_correct).
    ///
    /// During a validator-set transition the TC is correct under EITHER set,
    /// so tip power is counted per set only when the TC is
    /// [correctly signed](Certificate::is_correctly_signed) under that same
    /// set (signatures authenticated, quorum reached), against that set's own
    /// faulty threshold: one set's signers never borrow the other set's
    /// positional weights.
    pub fn tip_vouched<K: KVStore>(
        &self,
        block_hash: &CryptoHash,
        block_tree: &BlockTreeSingleton<K>,
    ) -> Result<bool, BlockTreeError> {
        let validator_set_state = block_tree.validator_set_state()?;
        let committed = self
            .tip_power_exceeds_faulty(block_hash, validator_set_state.committed_validator_set());
        Ok(committed
            || (!validator_set_state.update_decided()
                && self.tip_power_exceeds_faulty(
                    block_hash,
                    validator_set_state.previous_validator_set(),
                )))
    }

    fn tip_power_exceeds_faulty(
        &self,
        block_hash: &CryptoHash,
        validator_set: &ValidatorSet,
    ) -> bool {
        if self.signatures.len() != validator_set.len() {
            return false;
        }
        let mut power: u128 = 0;
        for (idx, (signature, (_, p))) in self
            .signatures
            .iter()
            .zip(validator_set.validators_and_powers())
            .enumerate()
        {
            let signed_tip = self
                .voter_metadata
                .get(idx)
                .and_then(|(tip, _)| tip.as_ref());
            if signature.is_some() && signed_tip == Some(block_hash) {
                power += u128::from(p.int());
            }
        }
        let total = validator_set.total_power().int();
        let faulty = total - validator_set.quorum().int();
        // Verify last (ed25519 per signer): count only signers authenticated
        // under this same set.
        power > faulty && self.is_correctly_signed(validator_set)
    }
}

/// Struct that incrementally forms [`TimeoutCertificate`]s by combining [`TimeoutVote`]s with the
/// same specific `chain_id` and `view` and from the same `validator_set` into a [`SignatureSet`].
#[derive(Clone, PartialEq)]
pub(crate) struct TimeoutVoteCollector {
    chain_id: ChainID,
    view: ViewNumber,
    validator_set: ValidatorSet,
    signature_set_power: TotalPower,
    signature_set: SignatureSet,
    high_tip: Option<TipInfo>,
    high_qc: Option<PhaseCertificate>,
    /// Per-voter metadata: (optional local_tip hash, optional (highest_qc view, highest_qc block)).
    /// Indexed by validator position, enables TC signature verification.
    #[allow(clippy::type_complexity)]
    voter_metadata: Vec<(Option<CryptoHash>, Option<(ViewNumber, CryptoHash)>)>,
}

impl Collector for TimeoutVoteCollector {
    type Vote = TimeoutVote;

    type Certificate = TimeoutCertificate;

    fn new(chain_id: ChainID, view: ViewNumber, validator_set: ValidatorSet) -> Self {
        let n = validator_set.len();
        Self {
            chain_id,
            view,
            validator_set,
            signature_set_power: TotalPower::new(0),
            signature_set: SignatureSet::new(n),
            high_tip: None,
            high_qc: None,
            voter_metadata: vec![(None, None); n],
        }
    }

    fn validator_set(&self) -> &ValidatorSet {
        &self.validator_set
    }

    fn chain_id(&self) -> ChainID {
        self.chain_id
    }

    fn view(&self) -> ViewNumber {
        self.view
    }

    /// Adds the timeout vote to a signature set if it has the correct view and chain id. Returning a Quorum
    /// Certificate if adding the vote allows for one to be created.
    ///
    /// If the timeout vote is not signed correctly, or doesn't match the collector's view, or the signer is
    /// not part of its validator set, then this is a no-op.
    ///
    /// # Preconditions
    ///
    /// `vote.is_correct(signer)`
    fn collect(&mut self, signer: &VerifyingKey, vote: TimeoutVote) -> Option<TimeoutCertificate> {
        if self.chain_id != vote.chain_id || self.view != vote.view {
            return None;
        }

        // Check if the signer is actually in the validator set.
        if let Some(pos) = self.validator_set.position(signer) {
            // If the vote has not been collected before, insert its signature into the signature set.
            if self.signature_set.get(pos).is_none() {
                self.signature_set.set(pos, Some(vote.signature));
                self.signature_set_power += *self.validator_set.power(signer).unwrap();

                // Store per-voter signed metadata for TC signature verification.
                let tip_hash = vote.local_tip.as_ref().map(|t| t.block_hash);
                let qc_summary = vote.highest_qc.as_ref().map(|q| (q.view, q.block));
                if pos < self.voter_metadata.len() {
                    self.voter_metadata[pos] = (tip_hash, qc_summary);
                }

                // MonadBFT: track high_tip and high_qc
                if let Some(ref tip) = vote.local_tip {
                    if self.high_tip.is_none() || tip.view > self.high_tip.as_ref().unwrap().view {
                        self.high_tip = Some(tip.clone());
                    }
                }
                if let Some(ref qc) = vote.highest_qc {
                    if self.high_qc.is_none() || qc.view > self.high_qc.as_ref().unwrap().view {
                        self.high_qc = Some(qc.clone());
                    }
                }

                if self.signature_set_power >= self.validator_set.quorum() {
                    let max_tip_view = self.high_tip.as_ref().map(|t| t.view);
                    let max_qc_view = self.high_qc.as_ref().map(|q| q.view);
                    let high_tip_is_winner = match (max_tip_view, max_qc_view) {
                        (Some(tv), Some(qv)) => tv > qv,
                        (Some(_), None) => true,
                        _ => false,
                    };
                    let collected_tc = TimeoutCertificate {
                        chain_id: self.chain_id,
                        view: self.view,
                        signatures: self.signature_set.clone(),
                        high_tip: self.high_tip.clone(),
                        high_qc: self.high_qc.clone(),
                        high_tip_is_winner,
                        voter_metadata: self.voter_metadata.clone(),
                    };
                    return Some(collected_tc);
                }
            }
        }

        None
    }
}

/// Pre-testnet P1 (stop-review WARN): during a validator-set transition a TC
/// is correct under EITHER set, so [`TimeoutCertificate::tip_vouched`] must
/// count tip power only under a set that authenticates the signatures (and
/// reaches its quorum), with that set's own faulty threshold. Otherwise a TC
/// signed by the previous set could borrow the committed set's positional
/// weights (same size, different members and powers) and vouch falsely.
#[cfg(test)]
mod tip_vouched_tests {
    use super::*;
    use crate::hotstuff::header_fast_path_regression_test::{signing_keys, MemKV};
    use crate::types::crypto_primitives::{Keypair, SigningKey};
    use crate::types::update_sets::AppStateUpdates;

    const VIEW: u64 = 1;
    const BLOCK: CryptoHash = CryptoHash::new([51; 32]);

    /// `powers[i]` goes to the validator at POSITION `i` (sorted by key).
    fn set_with(keys: &[SigningKey], powers: &[u64]) -> ValidatorSet {
        let mut sorted: Vec<_> = keys.iter().map(|k| k.verifying_key()).collect();
        sorted.sort_by_key(|vk| vk.to_bytes());
        let mut vs = ValidatorSet::new();
        for (vk, p) in sorted.iter().zip(powers) {
            vs.put(vk, Power::new(*p));
        }
        vs
    }

    /// A TC signed by the validators at `positions` of `set`; signed tip
    /// `BLOCK` for the first `tips` of them, none for the rest.
    fn tc(
        set: &ValidatorSet,
        keys: &[SigningKey],
        positions: &[usize],
        tips: usize,
    ) -> TimeoutCertificate {
        let view = ViewNumber::new(VIEW);
        let mut signatures = SignatureSet::new(set.len());
        let mut voter_metadata = vec![(None, None); set.len()];
        for (i, &pos) in positions.iter().enumerate() {
            let vk = set.validators().nth(pos).copied().unwrap();
            let sk = keys.iter().find(|k| k.verifying_key() == vk).unwrap();
            let tip = (i < tips).then_some(BLOCK);
            let mut msg = (ChainID::new(0), view).try_to_vec().unwrap();
            if let Some(hash) = tip {
                msg.extend_from_slice(&hash.bytes());
            }
            signatures.set(pos, Some(Keypair::new(sk.clone()).sign(&msg)));
            voter_metadata[pos] = (tip, None);
        }
        TimeoutCertificate {
            chain_id: ChainID::new(0),
            view,
            signatures,
            high_tip: None,
            high_qc: None,
            high_tip_is_winner: true,
            voter_metadata,
        }
    }

    fn tree(committed: &ValidatorSet, previous: &ValidatorSet) -> BlockTreeSingleton<MemKV> {
        let vss = ValidatorSetState::new(
            committed.clone(),
            previous.clone(),
            Some(BlockHeight::new(0)),
            false,
        );
        let mut tree = BlockTreeSingleton::new(MemKV::default());
        tree.initialize(&AppStateUpdates::new(), &vss).unwrap();
        tree
    }

    /// Previous set: 4 x power 1. Committed set: 4 OTHER validators, power
    /// 100 at position 0. A previous-set TC (3 signers, one signed tip at
    /// position 0, not > f = 1) must not vouch via the committed set's
    /// weights, whose signatures it never authenticated.
    #[test]
    fn tc_of_one_set_does_not_borrow_the_other_sets_weights() {
        let prev_keys = signing_keys(&[1, 2, 3, 4]);
        let comm_keys = signing_keys(&[5, 6, 7, 8]);
        let previous = set_with(&prev_keys, &[1, 1, 1, 1]);
        let committed = set_with(&comm_keys, &[100, 1, 1, 1]);
        let tree = tree(&committed, &previous);
        let tc = tc(&previous, &prev_keys, &[0, 1, 2], 1);
        assert!(
            tc.is_correct(&tree).unwrap(),
            "correct under the previous set"
        );
        assert!(!tc.is_correctly_signed(&committed));
        assert!(
            !tc.tip_vouched(&BLOCK, &tree).unwrap(),
            "1 of 4 previous-set power is not > f; committed weights must not count"
        );
    }

    /// Same, reversed: a committed-set TC must not vouch via the previous
    /// set's weights.
    #[test]
    fn committed_tc_does_not_borrow_previous_weights() {
        let prev_keys = signing_keys(&[1, 2, 3, 4]);
        let comm_keys = signing_keys(&[5, 6, 7, 8]);
        let previous = set_with(&prev_keys, &[100, 1, 1, 1]);
        let committed = set_with(&comm_keys, &[1, 1, 1, 1]);
        let tree = tree(&committed, &previous);
        let tc = tc(&committed, &comm_keys, &[0, 1, 2], 1);
        assert!(tc.is_correct(&tree).unwrap());
        assert!(!tc.tip_vouched(&BLOCK, &tree).unwrap());
    }

    /// Control: under the authenticating set, > f signed tips still vouch,
    /// in either direction of the transition.
    #[test]
    fn authenticated_tips_over_f_still_vouch_during_transition() {
        let prev_keys = signing_keys(&[1, 2, 3, 4]);
        let comm_keys = signing_keys(&[5, 6, 7, 8]);
        let previous = set_with(&prev_keys, &[1, 1, 1, 1]);
        let committed = set_with(&comm_keys, &[100, 1, 1, 1]);
        let tree = tree(&committed, &previous);
        let prev_tc = tc(&previous, &prev_keys, &[0, 1, 2], 2);
        assert!(prev_tc.tip_vouched(&BLOCK, &tree).unwrap());
        // Committed-set TC: power 100 (position 0) + 1 signs, tip only at
        // position 1 (power 1): not > f; tip at position 0 (100): > f.
        let low = tc(&committed, &comm_keys, &[1, 0], 1);
        assert!(low.is_correct(&tree).unwrap());
        assert!(!low.tip_vouched(&BLOCK, &tree).unwrap());
        let high = tc(&committed, &comm_keys, &[0, 1], 1);
        assert!(high.tip_vouched(&BLOCK, &tree).unwrap());
    }
}
