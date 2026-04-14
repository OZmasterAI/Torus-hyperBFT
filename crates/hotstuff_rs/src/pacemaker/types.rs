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

                    if _signer
                        .verify(&msg, &signature)
                        .is_ok()
                    {
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
