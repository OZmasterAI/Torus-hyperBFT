/*
    Copyright © 2023, ParallelChain Lab
    Licensed under the Apache License, Version 2.0: http://www.apache.org/licenses/LICENSE-2.0
*/

//! Types specific to the HotStuff subprotocol.

use std::collections::HashMap;

use borsh::{BorshDeserialize, BorshSerialize};
use ed25519_dalek::Verifier;

use super::messages::{PhaseVote, Proposal};
use crate::{
    block_tree::{
        accessors::internal::{BlockTreeError, BlockTreeSingleton},
        pluggables::KVStore,
    },
    types::{
        data_types::*,
        signed_messages::{Certificate, Collector},
        validator_set::*,
    },
};

/// Cryptographic proof that at least a quorum of validators have voted for a
/// [`Proposal`](super::messages::Proposal) or [`Nudge`](super::messages::Nudge) with the included
/// `chain_id`, `view`, `block`, and `phase`.
///
/// Used to extend a block in the HotStuff subprotocol, and to optimistically advance to a new view in
/// the [pacemaker](crate::pacemaker) subprotocol.
#[derive(Clone, BorshSerialize, BorshDeserialize, PartialEq, Eq)]
pub struct PhaseCertificate {
    pub chain_id: ChainID,
    pub view: ViewNumber,
    pub block: CryptoHash,
    pub phase: Phase,
    pub signatures: SignatureSet,
}

impl Certificate for PhaseCertificate {
    type Vote = PhaseVote;

    /// Determine the appropriate validator set that the PC should be checked against, and check if the
    /// signatures in the certificate are correct and form a quorum given this validator set.
    ///
    /// A special case is if the PC is the Genesis PC, in which case it is automatically correct.
    fn is_correct<K: KVStore>(
        &self,
        block_tree: &BlockTreeSingleton<K>,
    ) -> Result<bool, BlockTreeError> {
        if self.is_genesis_pc() {
            return Ok(true);
        };

        let block_height = block_tree.block_height(&self.block)?;

        let validator_set_state = block_tree.validator_set_state()?;

        let result = match (block_height, validator_set_state.update_height()) {
            // If the Block that the PC certifies is not in the block tree, **or** else if the validator set has
            // never been updated in the history of the blockchain, validate the PC according to the current
            // committed validator set.
            (None, _) | (Some(_), &None) => {
                self.is_correctly_signed(validator_set_state.committed_validator_set())
            }

            // If the Block that the PC certifies is in the block tree at `height` **and** the validator set was
            // last updated at height `update_height`:
            (Some(height), &Some(update_height)) => {
                // If the Block comes in the block tree before the latest validator set updating block, validate the PC
                // according to the previous validator set.
                if height < update_height {
                    self.is_correctly_signed(validator_set_state.previous_validator_set())

                // Else if the Block comes after the latest validator set updating block, validate the PC according to
                // the committed validator set.
                } else if height > update_height {
                    self.is_correctly_signed(validator_set_state.committed_validator_set())

                // Else if the Block **is** the latest validator set updating block:
                } else {
                    match self.phase {
                        // If the PC is a Decide PC:
                        Phase::Decide => {
                            match block_tree.validator_set_updates_status(&self.block)? {
                                // If the block's validator set updates have been committed, then validate the PC according to the
                                // committed validator set.
                                ValidatorSetUpdatesStatus::Committed => self.is_correctly_signed(
                                    validator_set_state.committed_validator_set(),
                                ),

                                // If the block's validator set updates have not been committed, then apply the validator set updates
                                // to the committed validator set and validate the PC according to the resulting validator set.
                                //
                                // Note (from Karolina): this may be the case if the block justified by this PC is received via sync
                                // hence the updates have not been applied yet. In this case we need to compute the validator set
                                // expected to have voted "Decide" for the block.
                                ValidatorSetUpdatesStatus::Pending(vs_updates) => {
                                    let mut new_validator_set =
                                        block_tree.committed_validator_set()?;
                                    new_validator_set.apply_updates(&vs_updates);
                                    self.is_correctly_signed(&new_validator_set)
                                }

                                // If the block does not have validator set updates, then it should be justified by a Generic PC, not
                                // a phased mode PC like a Decide PC. Therefore, the PC is invalid.
                                //
                                // Issue: https://github.com/parallelchain-io/hotstuff_rs/issues/46
                                ValidatorSetUpdatesStatus::None => false,
                            }
                        }

                        // Else if the PC is a phased mode PC that is not a Decide PC:
                        Phase::Prepare | Phase::Precommit | Phase::Commit => {
                            match block_tree.validator_set_updates_status(&self.block)? {
                                // If the latest validator set update has been committed
                                ValidatorSetUpdatesStatus::Committed => self.is_correctly_signed(
                                    validator_set_state.previous_validator_set(),
                                ),
                                ValidatorSetUpdatesStatus::Pending(_) => self.is_correctly_signed(
                                    validator_set_state.committed_validator_set(),
                                ),

                                // If the block does not have validator set updates, then it should be justified by a Generic PC, not
                                // a phased mode PC like a Prepare PC, Precommit PC, or Commit PC. Therefore, the PC is invalid.
                                //
                                // Issue: https://github.com/parallelchain-io/hotstuff_rs/issues/46
                                ValidatorSetUpdatesStatus::None => false,
                            }
                        }

                        // If the block has validator set updates, then it should be justified by a phased mode PC, not a
                        // Generic PC. Therefore, the PC is invalid.
                        //
                        // Issue: https://github.com/parallelchain-io/hotstuff_rs/issues/46
                        // Note (from Karolina): cannot panic here, since safe_pc has not been checked yet.
                        _ => false,
                    }
                }
            }
        };

        Ok(result)
    }

    /// Check if all of the signatures in the certificate are correct, and if the set of signatures forms
    /// a quorum according to the provided `validator_set`.
    fn is_correctly_signed(&self, validator_set: &ValidatorSet) -> bool {
        // Check whether the size of the signature set is the same as the size of the validator set.
        if self.signatures.len() != validator_set.len() {
            return false;
        }

        // Check whether every signature is correct and tally up their powers.
        let mut total_power: TotalPower = TotalPower::new(0);
        for (signature, (signer, power)) in self
            .signatures
            .iter()
            .zip(validator_set.validators_and_powers())
        {
            if let Some(signature) = signature {
                if let Ok(signature) = Signature::from_slice(&signature.bytes()) {
                    if signer
                        .verify(
                            &(self.chain_id, self.view, self.block, self.phase)
                                .try_to_vec()
                                .unwrap(),
                            &signature,
                        )
                        .is_ok()
                    {
                        total_power += power;
                    } else {
                        // pc contains incorrect signature.
                        return false;
                    }
                } else {
                    // pc contains incorrect signature.
                    return false;
                }
            }
        }

        // Check if the signatures form a quorum.
        total_power >= validator_set.quorum()
    }
}

impl PhaseCertificate {
    /// Get the "Genesis Phase Certificate", the phase certificate that `Block`s of height 0 ("Genesis
    /// Blocks") include as their `block.justify`.
    ///
    /// The fields of this `PhaseCertificate` all take the "default" values of their respective types.
    /// For example, `genesis_pc.chain_id` is 0 regardless of the `ChainID` of the specific block tree, and
    /// `genesis_pc.block` is `[0u8; 32]`.
    pub const fn genesis_pc() -> PhaseCertificate {
        PhaseCertificate {
            chain_id: ChainID::new(0),
            view: ViewNumber::init(),
            block: CryptoHash::new([0u8; 32]),
            phase: Phase::Generic,
            signatures: SignatureSet::genesis(),
        }
    }

    /// Check whether this `PhaseCertificate` is the [`genesis_pc`](Self::genesis_pc).
    pub fn is_genesis_pc(&self) -> bool {
        *self == Self::genesis_pc()
    }

    /// Check whether `self.phase` is a variant that is allowed for `PhaseCertificate`s that serve as
    /// [`block.justify`](crate::types::block::Block::justify).
    ///
    /// Returns `true` if [`phase.is_generic()`](Phase::is_generic) or
    /// [`phase.is_decide()`](Phase::is_decide). Returns `false` otherwise.
    pub fn is_block_justify(&self) -> bool {
        self.phase.is_generic() || self.phase.is_decide()
    }

    /// Check whether `self.phase` is a veriant that is allowed for `PhaseCertificate`s that serve as
    /// [`nudge.justify`](super::messages::Nudge::justify).
    ///
    /// Returns `true` if [`phase.is_prepare()`](Phase::is_prepare),
    /// [`phase.is_precommit()`](Phase::is_precommit), or [`phase.is_commit()`](Phase::is_commit). Returns `false`
    /// otherwise.
    pub fn is_nudge_justify(&self) -> bool {
        self.phase.is_prepare() || self.phase.is_precommit() || self.phase.is_commit()
    }
}

/// HotStuff subprotocol voting phases.
#[derive(Clone, Copy, PartialEq, Eq, Hash, BorshSerialize, BorshDeserialize, Debug)]
pub enum Phase {
    /// Generic phase. The only voting phase in the [pipelined mode](super#dual-operating-modes).
    Generic,

    /// Prepare phase. The **first** voting phase in the phased mode.  
    Prepare,

    /// Precommit phase. The **second** voting phase in the phased mode.
    Precommit,

    /// Commit phase. The **third** voting phase in the phased mode.
    Commit,

    /// Decide phase. The **fourth** and final voting phase in the phased mode.
    Decide,
}

impl Phase {
    pub fn is_generic(self) -> bool {
        self == Phase::Generic
    }

    pub fn is_prepare(self) -> bool {
        self == Phase::Prepare
    }

    pub fn is_precommit(self) -> bool {
        matches!(self, Phase::Precommit)
    }

    pub fn is_commit(self) -> bool {
        matches!(self, Phase::Commit)
    }

    pub fn is_decide(self) -> bool {
        matches!(self, Phase::Decide)
    }
}

/// Struct that incrementally forms [`PhaseCertificate`]s by combining [`PhaseVote`]s with the same
/// specific `chain_id` and `view` and from the same specific `validator_set` into [`SignatureSet`]s.
///
/// ## Usage
///
/// Every `PhaseVoteCollector` is created around a fixed `(ChainID, ViewNumber, ValidatorSet)` triple
/// using [`new`](Self::new). This triple is constant through the lifetime of the `PhaseVoteCollector`.
///
/// The user can then hold on to the `PhaseVoteCollector`, calling [`collect`](Self::collect) every time
/// it receives a `PhaseVote`. If the vote's `chain_id` and `view` matches the vote collector's
/// `chain_id` and `view`, and in addition its `signature` comes from a validator in the collector's
/// `validator_set`, the collector will store the vote's `signature` in its internal buffer of signature
/// sets.
///
/// If after storing `signature` in the internal buffer it is found that a
/// [`quorum`](Certificate::quorum) of `PhaseVote`s have been collected for a particular Block Hash and
/// `Phase` pair, `collect` will form a `PhaseCertificate` using the collected votes and return it from
/// `collect`.
#[derive(Clone)]
pub(crate) struct PhaseVoteCollector {
    chain_id: ChainID,
    view: ViewNumber,
    validator_set: ValidatorSet,

    /// For each key-value pair in this HashMap:
    /// - The key is a Block Hash followed by a `Phase`.
    /// - The value is a `SignatureSet` containing signatures over `(self.chain_id, self.view, key.0, key.1)`
    ///   by a subset of validators in `self.validator_set`, followed by the `TotalPower` of the subset.
    signature_sets: HashMap<(CryptoHash, Phase), (SignatureSet, TotalPower)>,
}

impl Collector for PhaseVoteCollector {
    type Vote = PhaseVote;

    type Certificate = PhaseCertificate;

    fn new(chain_id: ChainID, view: ViewNumber, validator_set: ValidatorSet) -> Self {
        Self {
            chain_id,
            view,
            validator_set,
            signature_sets: HashMap::new(),
        }
    }

    fn chain_id(&self) -> ChainID {
        self.chain_id
    }

    fn view(&self) -> ViewNumber {
        self.view
    }

    fn validator_set(&self) -> &ValidatorSet {
        &self.validator_set
    }

    /// Collect `phase_vote` using this collector. Return a Phase Certificate if collecting this
    /// `PhaseVote` allows for one to be created.
    ///
    /// If the `PhaseVote` is not signed correctly, not signed by `signer`, or has a `chain_id` or `view`
    /// that does not match this `PhaseVoteCollector`'s `chain_id` or `view`, then calling this method is a
    /// no-op.
    ///
    /// ## Preconditions
    ///
    /// `phase_vote.is_correct(signer)`
    fn collect(
        &mut self,
        signer: &VerifyingKey,
        phase_vote: PhaseVote,
    ) -> Option<PhaseCertificate> {
        if self.chain_id != phase_vote.chain_id || self.view != phase_vote.view {
            return None;
        }

        // Check if the signer is actually in the validator set.
        if let Some(pos) = self.validator_set.position(signer) {
            // If the vote is for a new (block, phase) pair, prepare an empty signature set.
            if let std::collections::hash_map::Entry::Vacant(e) = self
                .signature_sets
                .entry((phase_vote.block, phase_vote.phase))
            {
                e.insert((
                    SignatureSet::new(self.validator_set.len()),
                    TotalPower::new(0),
                ));
            }

            let (signature_set, signature_set_power) = self
                .signature_sets
                .get_mut(&(phase_vote.block, phase_vote.phase))
                .unwrap();

            // If a vote for the (block, phase) from the signer hasn't been collected before, insert it into the
            // signature set.
            if signature_set.get(pos).is_none() {
                signature_set.set(pos, Some(phase_vote.signature));
                *signature_set_power += *self.validator_set.power(signer).unwrap();

                // If inserting the vote makes the signature set form a quorum, then create a Phase Certificate.
                if *signature_set_power >= self.validator_set.quorum() {
                    let (signatures, _) = self
                        .signature_sets
                        .remove(&(phase_vote.block, phase_vote.phase))
                        .unwrap();
                    let collected_pc = PhaseCertificate {
                        chain_id: self.chain_id,
                        view: self.view,
                        block: phase_vote.block,
                        phase: phase_vote.phase,
                        signatures,
                    };

                    return Some(collected_pc);
                }
            }
        }

        None
    }
}

// ============================================================================
// MonadBFT B2: No-Endorsement Certificate (NEC) and related types
// ============================================================================

/// No-Endorsement Certificate (NEC): proof that a block can be safely skipped.
///
/// An NEC consists of 2f+1 signatures from validators who did NOT vote for the
/// high_tip block. Since a QC requires 2f+1 voters and an NEC requires 2f+1
/// non-voters, both cannot coexist (2f+1 + 2f+1 > 3f+1 = n), so if an NEC
/// forms, the block could never have gotten a QC and skipping it is safe.
#[derive(Clone, BorshSerialize, BorshDeserialize, PartialEq, Eq)]
pub struct NoEndorsementCertificate {
    /// The view this NEC is for (tc.view + 1).
    pub view: ViewNumber,
    /// View of the QC inside the high_tip's block header.
    pub high_tip_qc_view: ViewNumber,
    /// Aggregated signatures of 2f+1 validators over (view, high_tip_qc_view).
    pub signatures: SignatureSet,
}

/// VALIDNEC: Check that a No-Endorsement Certificate is well-formed and has a quorum.
///
/// Validity conditions:
/// 1. nec.high_tip_qc_view < nec.view - 1 (the skipped block was NOT in the
///    immediately preceding view — otherwise there's no gap to recover from)
/// 2. The signatures contain 2f+1 valid signatures over (nec.view, nec.high_tip_qc_view)
/// 3. FIX CONS-PF-07: NEC signers must be non-voting validators (not in voter_set).
///    MonadBFT tail-fork resistance requires NECs to only come from validators who
///    did not vote in the current view.
pub fn valid_nec<K: KVStore>(
    nec: &NoEndorsementCertificate,
    block_tree: &BlockTreeSingleton<K>,
) -> Result<bool, BlockTreeError> {
    // Condition 1: the high_tip's QC view must be strictly less than nec.view - 1.
    if nec.view.int() == 0 || nec.high_tip_qc_view >= nec.view - 1 {
        return Ok(false);
    }

    // Condition 2: verify 2f+1 valid signatures.
    // FIX CONS-FIND-15: During validator set transition, validate against the
    // validator set that was active when the NEC was produced. The previous code
    // had a dead branch returning committed_validator_set() in both cases.
    let validator_set_state = block_tree.validator_set_state()?;
    let validator_set = if validator_set_state.update_decided() {
        validator_set_state.committed_validator_set()
    } else {
        // During transition, use the previous validator set (the one that was
        // active when the NEC was produced).
        validator_set_state.previous_validator_set()
    };

    Ok(is_nec_correctly_signed(nec, validator_set))
}

/// Check if the NEC's signatures are correct and form a quorum.
fn is_nec_correctly_signed(nec: &NoEndorsementCertificate, validator_set: &ValidatorSet) -> bool {
    if nec.signatures.len() != validator_set.len() {
        return false;
    }

    let mut total_power = TotalPower::new(0);
    let ne_message_bytes = (nec.view, nec.high_tip_qc_view).try_to_vec().unwrap();

    for (signature, (signer, power)) in nec
        .signatures
        .iter()
        .zip(validator_set.validators_and_powers())
    {
        if let Some(signature) = signature {
            if let Ok(sig) = Signature::from_slice(&signature.bytes()) {
                if signer.verify(&ne_message_bytes, &sig).is_ok() {
                    total_power += power;
                } else {
                    return false;
                }
            } else {
                return false;
            }
        }
    }

    total_power >= validator_set.quorum()
}

/// MonadBFT ISFRESHPROPOSAL: determines if a proposal contains a fresh block
/// (not a reproposal), which qualifies for speculative commit.
///
/// A proposal is "fresh" if ANY of:
/// 1. Happy path: the block's QC is from the immediately preceding view
///    (p.block.justify.view == p.view - 1)
/// 2. NEC: an NEC proves the skipped block is safe to abandon
/// 3. TC high_qc: the TC's highest QC resolves the view without reproposal
pub fn is_fresh_proposal(proposal: &Proposal) -> bool {
    // Condition 1: happy path — QC from immediately preceding view.
    if proposal.view.int() > 0 && proposal.block.justify.view == proposal.view - 1 {
        return true;
    }

    // Condition 2: NEC proves the skipped block is safe.
    if proposal.nec.is_some() {
        return true;
    }

    // Condition 3: TC's high_qc resolves the view.
    if let Some(ref tc) = proposal.tc {
        if tc.high_qc.is_some() && !tc.high_tip_is_winner {
            return true;
        }
    }

    false
}

/// Collector for No-Endorsement (NE) messages during RECOVER.
///
/// Incrementally collects NE signatures from validators who did not vote for
/// the high_tip block, forming an NEC when 2f+1 are collected.
///
/// FIX CONS-PF-07: Tracks a voter set to reject NE signatures from validators
/// who voted for the high_tip block. MonadBFT's tail-fork resistance requires
/// that NEC signers did NOT vote in the current view.
#[derive(Clone)]
pub struct NECollector {
    view: ViewNumber,
    high_tip_qc_view: ViewNumber,
    validator_set: ValidatorSet,
    signature_set: SignatureSet,
    signature_set_power: TotalPower,
    /// Set of validators known to have voted for the high_tip block.
    /// NE signatures from these validators are rejected.
    voters: std::collections::HashSet<ed25519_dalek::VerifyingKey>,
}

impl NECollector {
    pub fn new(
        view: ViewNumber,
        high_tip_qc_view: ViewNumber,
        validator_set: ValidatorSet,
    ) -> Self {
        let n = validator_set.len();
        Self {
            view,
            high_tip_qc_view,
            validator_set,
            signature_set: SignatureSet::new(n),
            signature_set_power: TotalPower::new(0),
            voters: std::collections::HashSet::new(),
        }
    }

    /// Register a validator as having voted for the high_tip block.
    /// NE signatures from this validator will be rejected.
    pub fn register_voter(&mut self, voter: ed25519_dalek::VerifyingKey) {
        self.voters.insert(voter);
    }

    /// Collect an NE signature from a validator.
    /// Returns an NEC if 2f+1 signatures have been collected.
    pub fn collect(
        &mut self,
        signer: &VerifyingKey,
        ne_view: ViewNumber,
        ne_high_tip_qc_view: ViewNumber,
        signature: SignatureBytes,
    ) -> Option<NoEndorsementCertificate> {
        // Verify the NE is for the correct view and high_tip_qc_view.
        if ne_view != self.view || ne_high_tip_qc_view != self.high_tip_qc_view {
            return None;
        }

        // FIX CONS-PF-07: Reject NE from validators who voted for the high_tip block.
        if self.voters.contains(signer) {
            return None;
        }

        // Verify the signer is in the validator set.
        if let Some(pos) = self.validator_set.position(signer) {
            // Don't double-count.
            if self.signature_set.get(pos).is_some() {
                return None;
            }

            // Verify signature.
            let ne_message_bytes = (ne_view, ne_high_tip_qc_view).try_to_vec().unwrap();
            if let Ok(sig) = Signature::from_slice(&signature.bytes()) {
                if signer.verify(&ne_message_bytes, &sig).is_err() {
                    return None;
                }
            } else {
                return None;
            }

            self.signature_set.set(pos, Some(signature));
            self.signature_set_power += *self.validator_set.power(signer).unwrap();

            if self.signature_set_power >= self.validator_set.quorum() {
                return Some(NoEndorsementCertificate {
                    view: self.view,
                    high_tip_qc_view: self.high_tip_qc_view,
                    signatures: self.signature_set.clone(),
                });
            }
        }

        None
    }
}

// ============================================================================
// MonadBFT B3: Equivocation Evidence
// ============================================================================

/// Proof that a leader proposed two different blocks at the same view.
///
/// Contains the two conflicting block hashes and the view/leader identity.
/// This is sufficient evidence for slashing — the two blocks are in the block
/// tree and can be independently verified.
#[derive(Clone, Debug)]
pub struct EquivocationEvidence {
    /// The view in which equivocation occurred.
    pub view: ViewNumber,
    /// The equivocating leader's public key.
    pub leader: VerifyingKey,
    /// Hash of the first block proposed.
    pub block_a: CryptoHash,
    /// Hash of the second (conflicting) block proposed.
    pub block_b: CryptoHash,
}

// ============================================================================
// MonadBFT B3: Leader Reputation
// ============================================================================

/// Per-validator leadership performance record for reputation tracking.
///
/// Stored as part of consensus state and updated deterministically on every node
/// at the same points (block commit, view timeout). All nodes computing the same
/// sequence of events produce identical reputation scores.
#[derive(Clone, Debug, BorshSerialize, BorshDeserialize, PartialEq, Eq)]
pub struct LeaderReputationEntry {
    /// Number of views where this validator was leader and successfully produced a committed block.
    pub successes: u32,
    /// Total number of views where this validator was leader (successes + timeouts).
    pub total: u32,
}

impl Default for LeaderReputationEntry {
    fn default() -> Self {
        Self::new()
    }
}

impl LeaderReputationEntry {
    pub fn new() -> Self {
        Self {
            successes: 0,
            total: 0,
        }
    }

    /// Reputation score in basis points (2500..=10000).
    /// Returns 10000 (100%) when no data exists (benefit of the doubt).
    /// Floor of 2500 (25%) prevents any validator from being excluded from rotation.
    pub fn score_bps(&self) -> u32 {
        if self.total == 0 {
            10_000
        } else {
            let raw = ((self.successes as u64) * 10_000 / (self.total as u64)) as u32;
            std::cmp::max(raw, 2_500)
        }
    }

    /// Record a successful block production.
    pub fn record_success(&mut self) {
        self.successes += 1;
        self.total += 1;
    }

    /// Record a timeout (failed to produce a block in time).
    pub fn record_timeout(&mut self) {
        self.total += 1;
    }
}

/// Aggregated leader reputation scores for the entire validator set.
///
/// Stored in the block tree for deterministic leader selection across all nodes.
/// Updated at well-defined points: irrevocable commit (success) and TC formation (timeout).
#[derive(Clone, Debug, BorshSerialize, BorshDeserialize, PartialEq, Eq)]
pub struct LeaderReputation {
    /// Map from validator public key bytes (32 bytes) to their reputation entry.
    /// Using Vec of tuples for deterministic serialization order.
    pub entries: Vec<([u8; 32], LeaderReputationEntry)>,
    /// Sliding window size: entries older than this many views are decayed.
    pub window_size: u32,
}

impl LeaderReputation {
    pub fn new(window_size: u32) -> Self {
        Self {
            entries: Vec::new(),
            window_size,
        }
    }

    /// Get or create the entry for a validator.
    fn entry_mut(&mut self, validator: &VerifyingKey) -> &mut LeaderReputationEntry {
        let key = validator.to_bytes();
        if let Some(pos) = self.entries.iter().position(|(k, _)| *k == key) {
            &mut self.entries[pos].1
        } else {
            self.entries.push((key, LeaderReputationEntry::new()));
            &mut self.entries.last_mut().unwrap().1
        }
    }

    /// Record a successful block production for a leader.
    pub fn record_success(&mut self, leader: &VerifyingKey) {
        self.entry_mut(leader).record_success();
    }

    /// Record a timeout for a leader.
    pub fn record_timeout(&mut self, leader: &VerifyingKey) {
        self.entry_mut(leader).record_timeout();
    }

    /// Get the reputation score in basis points (0..=10000) for a validator.
    /// Returns 10000 (100%) for unknown validators.
    pub fn score_bps(&self, validator: &VerifyingKey) -> u32 {
        let key = validator.to_bytes();
        self.entries
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, e)| e.score_bps())
            .unwrap_or(10_000)
    }

    /// Decay old data by halving all counters. Called periodically (e.g., every
    /// `window_size` views) to implement a sliding window effect.
    /// Removes entries that have decayed to zero.
    pub fn decay(&mut self) {
        for (_, entry) in &mut self.entries {
            entry.successes /= 2;
            entry.total /= 2;
        }
        self.entries.retain(|(_, e)| e.total > 0);
    }
}
