/*
    Copyright © 2023, ParallelChain Lab
    Licensed under the Apache License, Version 2.0: http://www.apache.org/licenses/LICENSE-2.0
    MonadBFT extensions added by Torus Project.
*/

//! Types specific to the Pacemaker protocol.

use borsh::{BorshDeserialize, BorshSerialize};
use ed25519_dalek::Verifier;

use crate::hotstuff::{
    messages::{is_header_signed_by, Proposal, ProposalHeader},
    types::{LeaderReputation, PhaseCertificate},
};
use crate::pacemaker::implementation::{
    reputation_leader_selection_enabled, select_leader, select_leader_with_reputation,
};
use crate::types::block::Block;

use crate::{
    block_tree::{
        accessors::internal::{BlockTreeError, BlockTreeSingleton},
        pluggables::KVStore,
    },
    pacemaker::messages::TimeoutVote,
    types::{
        data_types::*,
        signed_messages::{Certificate, Collector},
        signing::{timeout_vote_bytes, QcSummary, TipSummary},
        validator_set::*,
    },
};

/// A replica's tip: the signed header of the latest fresh proposal it voted for (MonadBFT;
/// docs/plans/tc-required-and-tip-signing.md §3 F2).
#[derive(Clone, BorshSerialize, BorshDeserialize, PartialEq, Eq)]
pub struct TipInfo {
    pub block_hash: CryptoHash,
    pub block_height: BlockHeight,
    pub block_justify: PhaseCertificate,
    pub block_data_hash: CryptoHash,
    pub view: ViewNumber,
    /// leader(`view`)'s signature over [`proposal_header_bytes`]`(chain_id, view, block_hash)`.
    pub leader_signature: SignatureBytes,
}

impl TipInfo {
    /// The tip `header` describes, carrying its leader signature.
    pub fn from_header(header: &ProposalHeader) -> TipInfo {
        TipInfo {
            block_hash: header.block_hash,
            block_height: header.height,
            block_justify: header.justify.clone(),
            block_data_hash: header.data_hash,
            view: header.view,
            leader_signature: header.leader_signature,
        }
    }

    /// The tip `proposal` describes, carrying its leader signature.
    pub fn from_proposal(proposal: &Proposal) -> TipInfo {
        TipInfo {
            block_hash: proposal.block.hash,
            block_height: proposal.block.height,
            block_justify: proposal.block.justify.clone(),
            block_data_hash: proposal.block.data_hash,
            view: proposal.view,
            leader_signature: proposal.leader_signature,
        }
    }

    /// What a timeout vote signs about this tip (F3).
    pub fn summary(&self) -> TipSummary {
        (self.block_hash, self.view)
    }

    /// F2: whether this tip is a real proposal header of chain `chain_id`:
    ///
    /// - its height, justify and data hash recompute its block hash ([`Block::hash`]), so the
    ///   fields the protocol reads from a tip (`block_justify` above all) are the block's own;
    /// - `leader_signature` is leader(`view`)'s signature over `(chain_id, view, block_hash)`
    ///   ([`proposal_header_bytes`](crate::types::signing::proposal_header_bytes)), with
    ///   leader(`view`) taken in the committed or the previous validator set (as for TCs). A tip
    ///   signed under an older set fails (open question 4: drop, not archive).
    ///
    /// Costs one hash and one or two signature checks.
    pub fn is_correct<K: KVStore>(
        &self,
        chain_id: ChainID,
        block_tree: &BlockTreeSingleton<K>,
    ) -> Result<bool, BlockTreeError> {
        let validator_set_state = block_tree.validator_set_state()?;
        Ok(self.is_correct_under(chain_id, &validator_set_state, tip_leader_reputation(block_tree).as_ref()))
    }

    /// [`is_correct`](Self::is_correct) under a validator-set state (and leader reputation, see
    /// [`tip_leader_reputation`]) the caller already read.
    pub(crate) fn is_correct_under(
        &self,
        chain_id: ChainID,
        validator_set_state: &ValidatorSetState,
        reputation: Option<&LeaderReputation>,
    ) -> bool {
        if Block::hash(self.block_height, &self.block_justify, &self.block_data_hash) != self.block_hash {
            return false;
        }
        let leader_in = |set: &ValidatorSet| {
            (!set.is_empty() && set.total_power().int() > 0).then(|| match reputation {
                Some(rep) => select_leader_with_reputation(self.view, set, rep),
                None => select_leader(self.view, set),
            })
        };
        let committed = leader_in(validator_set_state.committed_validator_set());
        let previous = leader_in(validator_set_state.previous_validator_set()).filter(|leader| Some(*leader) != committed);
        [committed, previous].into_iter().flatten().any(|leader| {
            is_header_signed_by(chain_id, self.view, &self.block_hash, &self.leader_signature, &leader)
        })
    }
}

/// The leader reputation a tip's leader is selected with: read only while reputation-weighted
/// selection is on (off by default), as the proposer check does.
pub(crate) fn tip_leader_reputation<K: KVStore>(block_tree: &BlockTreeSingleton<K>) -> Option<LeaderReputation> {
    if reputation_leader_selection_enabled() {
        block_tree.leader_reputation().ok()
    } else {
        None
    }
}

/// What one signer of a [`TimeoutCertificate`] signed besides `(chain_id, view)`: its tip's
/// `(hash, view)` and its highest QC's `(view, block)` ([`timeout_vote_bytes`]).
pub type VoterMetadata = (Option<TipSummary>, Option<QcSummary>);

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

    /// Per-voter signed metadata for TC signature verification, at the same positions as
    /// `signatures`: what each signer signed about its tip and its highest QC. This enables TC
    /// verification to reconstruct per-voter message bytes.
    pub voter_metadata: Vec<VoterMetadata>,
}

impl TimeoutCertificate {
    /// Whether `high_tip`, `high_qc` and `high_tip_is_winner` agree with the
    /// per-voter metadata of the positions that carry a signature (the
    /// signatures over that metadata are checked by the caller,
    /// [`is_correctly_signed`](Certificate::is_correctly_signed)):
    ///
    /// - `high_tip` is a tip some signer signed (hash and view, F3), at the
    ///   highest tip view any signer signed, and is present iff any signer
    ///   signed one; no signed tip is newer than the view this TC times out;
    /// - `high_qc` is the highest `(view, block)` QC summary any signer
    ///   signed, and is present iff any signer signed one;
    /// - `high_tip_is_winner` is the collector's rule over the two.
    ///
    /// That `high_tip` is a real header (F2) is checked by
    /// [`is_correct`](Certificate::is_correct), which has the validator sets.
    fn summaries_are_signed(&self) -> bool {
        let signed = || {
            self.signatures
                .iter()
                .enumerate()
                .filter(|(_, signature)| signature.is_some())
                .filter_map(|(idx, _)| self.voter_metadata.get(idx))
        };
        let max_tip = signed().filter_map(|(tip, _)| *tip).map(|(_, view)| view).max();
        let tip_ok = max_tip.is_none_or(|view| view <= self.view)
            && match &self.high_tip {
                Some(tip) => max_tip == Some(tip.view) && signed().any(|(summary, _)| *summary == Some(tip.summary())),
                None => max_tip.is_none(),
            };
        let max_qc = signed().filter_map(|(_, qc)| *qc).map(|(view, _)| view).max();
        let qc_ok = match &self.high_qc {
            Some(qc) => {
                max_qc == Some(qc.view) && signed().any(|(_, summary)| *summary == Some((qc.view, qc.block)))
            }
            None => max_qc.is_none(),
        };
        let winner = match (&self.high_tip, &self.high_qc) {
            (Some(tip), Some(qc)) => tip.view > qc.view,
            (Some(_), None) => true,
            _ => false,
        };
        tip_ok && qc_ok && winner == self.high_tip_is_winner
    }
}

impl Certificate for TimeoutCertificate {
    type Vote = TimeoutVote;

    /// Checks if the signatures in the TC are correct and form a quorum for an appropriate validator set.
    ///
    /// During the speculation phase, i.e., when the new validator set has been committed, but the old
    /// validator set is still active, a TC is correct if it is correctly signed by a quorum from either
    /// of the two validator sets.
    ///
    /// TC-required phase B: the TC's `high_qc` must itself be a correct QC. A timeout vote binds its
    /// highest QC only by the `(view, block)` summary it signs, so without this check one byzantine
    /// voter could make a forged high-view QC the TC's high QC, and VALIDPROPOSAL V3 would then
    /// reject every case-3 proposal backed by that TC. Honest collectors drop such votes
    /// (`Pacemaker::on_receive_timeout_vote`); this rejects a TC built by anyone else.
    fn is_correct<K: KVStore>(
        &self,
        block_tree: &BlockTreeSingleton<K>,
    ) -> Result<bool, BlockTreeError> {
        let validator_set_state = block_tree.validator_set_state()?;
        let signed = if validator_set_state.update_decided() {
            self.is_correctly_signed(validator_set_state.committed_validator_set())
        } else {
            self.is_correctly_signed(validator_set_state.committed_validator_set())
                || self.is_correctly_signed(validator_set_state.previous_validator_set())
        };
        if !signed {
            return Ok(false);
        }
        // F2: the high tip is a real header (its view, which the signers signed, is the one its
        // leader signed).
        if let Some(tip) = &self.high_tip {
            let reputation = tip_leader_reputation(block_tree);
            if !tip.is_correct_under(self.chain_id, &validator_set_state, reputation.as_ref()) {
                return Ok(false);
            }
        }
        // This replica's own highest PC was verified when it was accepted; skip its n
        // signature checks (the common case: the TC's signers report the same QC).
        match &self.high_qc {
            Some(high_qc) => Ok(*high_qc == block_tree.highest_pc()? || high_qc.is_correct(block_tree)?),
            None => Ok(true),
        }
    }

    /// Checks if all of the signatures in the certificate are correct, and if the set of signatures forms
    /// a quorum.
    fn is_correctly_signed(&self, validator_set: &ValidatorSet) -> bool {
        // Check whether the size of the signature set (and of the per-signer metadata) is the same
        // as the size of the validator set.
        if self.signatures.len() != validator_set.len() || self.voter_metadata.len() != validator_set.len() {
            return false;
        }

        // The summaries the protocol acts on must be what the signers signed.
        if !self.summaries_are_signed() {
            return false;
        }

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
                    let (tip, qc) = self.voter_metadata[idx];
                    let msg = timeout_vote_bytes(self.chain_id, self.view, tip, qc);

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
    /// Per-voter signed metadata, indexed by validator position: enables TC signature verification.
    voter_metadata: Vec<VoterMetadata>,
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
                if pos < self.voter_metadata.len() {
                    self.voter_metadata[pos] = vote.signed_summaries();
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
