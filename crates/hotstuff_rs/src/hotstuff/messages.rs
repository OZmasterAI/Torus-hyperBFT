/*
    Copyright © 2023, ParallelChain Lab
    Licensed under the Apache License, Version 2.0: http://www.apache.org/licenses/LICENSE-2.0
*/

//! Messages sent between replicas as part of the HotStuff subprotocol.

use std::mem;

use borsh::{BorshDeserialize, BorshSerialize};

use std::collections::HashMap;

use crate::{
    networking::{messages::ProgressMessage, receiving::Cacheable},
    types::{
        block::*,
        crypto_primitives::Keypair,
        data_types::*,
        signed_messages::{SignedMessage, Vote},
    },
};

use crate::pacemaker::types::TimeoutCertificate;

use super::types::{NoEndorsementCertificate, Phase, PhaseCertificate};

/// Every kind of message sent between replicas as part of the HotStuff subprotocol.
#[derive(Clone, BorshSerialize, BorshDeserialize)]
pub enum HotStuffMessage {
    /// See [`Proposal`].
    Proposal(Proposal),

    /// See [`Nudge`].
    Nudge(Nudge),

    /// See [`PhaseVote`].
    PhaseVote(PhaseVote),

    /// See [`NewView`].
    NewView(NewView),

    // MonadBFT B2: NEC recovery messages
    /// Request for the high_tip block from validators during RECOVER.
    ProposalRequest(ProposalRequest),

    /// Response containing the requested block.
    ProposalResponse(ProposalResponse),

    /// Request for No-Endorsement attestations during RECOVER.
    NERequest(NERequest),

    /// A single No-Endorsement attestation from a validator who did not vote
    /// for the high_tip block.
    NE(NEMessage),

    // Hybrid pipelining: header-first propagation
    /// Lightweight proposal header (gossip). Validators vote on this before body fetch.
    ProposalHeader(ProposalHeader),

    /// Request for the full block body (point-to-point to proposer).
    BlockDataRequest(BlockDataRequest),

    /// Response with full block body (point-to-point back to requester).
    BlockDataResponse(BlockDataResponse),
}

impl HotStuffMessage {
    /// Get the `ChainID` associated with the `HotStuffMessage`.
    pub fn chain_id(&self) -> ChainID {
        match self {
            HotStuffMessage::Proposal(Proposal { chain_id, .. }) => *chain_id,
            HotStuffMessage::Nudge(Nudge { chain_id, .. }) => *chain_id,
            HotStuffMessage::PhaseVote(PhaseVote { chain_id, .. }) => *chain_id,
            HotStuffMessage::NewView(NewView { chain_id, .. }) => *chain_id,
            HotStuffMessage::ProposalRequest(ProposalRequest { chain_id, .. }) => *chain_id,
            HotStuffMessage::ProposalResponse(ProposalResponse { chain_id, .. }) => *chain_id,
            HotStuffMessage::NERequest(NERequest { chain_id, .. }) => *chain_id,
            HotStuffMessage::NE(NEMessage { chain_id, .. }) => *chain_id,
            HotStuffMessage::ProposalHeader(h) => h.chain_id,
            HotStuffMessage::BlockDataRequest(r) => r.chain_id,
            HotStuffMessage::BlockDataResponse(r) => r.block.justify.chain_id,
        }
    }

    /// Get the view number associated with a given [`HotStuffMessage`].
    pub fn view(&self) -> ViewNumber {
        match self {
            HotStuffMessage::Proposal(Proposal { view, .. }) => *view,
            HotStuffMessage::Nudge(Nudge { view, .. }) => *view,
            HotStuffMessage::PhaseVote(PhaseVote { view, .. }) => *view,
            HotStuffMessage::NewView(NewView { view, .. }) => *view,
            HotStuffMessage::ProposalRequest(ProposalRequest { view, .. }) => *view,
            HotStuffMessage::ProposalResponse(ProposalResponse { view, .. }) => *view,
            HotStuffMessage::NERequest(NERequest { view, .. }) => *view,
            HotStuffMessage::NE(NEMessage { view, .. }) => *view,
            HotStuffMessage::ProposalHeader(h) => h.view,
            HotStuffMessage::BlockDataRequest(r) => r.view,
            HotStuffMessage::BlockDataResponse(r) => r.view,
        }
    }

    /// Returns true for hybrid pipelining messages that bypass view filtering.
    /// ProposalHeaders must bypass because bodies may still be in-flight when
    /// the view advances (e.g. rapid timeouts at startup).
    pub fn is_block_data_msg(&self) -> bool {
        matches!(
            self,
            HotStuffMessage::BlockDataRequest(_)
                | HotStuffMessage::BlockDataResponse(_)
                | HotStuffMessage::ProposalHeader(_)
        )
    }

    /// Returns the number of bytes required to store a given instance of the [`HotStuffMessage`] enum.
    pub fn size(&self) -> u64 {
        match self {
            HotStuffMessage::Proposal(_) => mem::size_of::<Proposal>() as u64,
            HotStuffMessage::Nudge(_) => mem::size_of::<Nudge>() as u64,
            HotStuffMessage::PhaseVote(_) => mem::size_of::<PhaseVote>() as u64,
            HotStuffMessage::NewView(_) => mem::size_of::<NewView>() as u64,
            HotStuffMessage::ProposalRequest(_) => mem::size_of::<ProposalRequest>() as u64,
            HotStuffMessage::ProposalResponse(_) => mem::size_of::<ProposalResponse>() as u64,
            HotStuffMessage::NERequest(_) => mem::size_of::<NERequest>() as u64,
            HotStuffMessage::NE(_) => mem::size_of::<NEMessage>() as u64,
            HotStuffMessage::ProposalHeader(_) => mem::size_of::<ProposalHeader>() as u64,
            HotStuffMessage::BlockDataRequest(_) => mem::size_of::<BlockDataRequest>() as u64,
            HotStuffMessage::BlockDataResponse(_) => mem::size_of::<BlockDataResponse>() as u64,
        }
    }
}

impl Cacheable for HotStuffMessage {
    fn size(&self) -> u64 {
        self.size()
    }

    fn view(&self) -> ViewNumber {
        self.view()
    }
}

impl From<Proposal> for HotStuffMessage {
    fn from(proposal: Proposal) -> Self {
        HotStuffMessage::Proposal(proposal)
    }
}

impl From<Nudge> for HotStuffMessage {
    fn from(nudge: Nudge) -> Self {
        HotStuffMessage::Nudge(nudge)
    }
}

impl From<PhaseVote> for HotStuffMessage {
    fn from(vote: PhaseVote) -> Self {
        HotStuffMessage::PhaseVote(vote)
    }
}

impl From<NewView> for HotStuffMessage {
    fn from(new_view: NewView) -> Self {
        HotStuffMessage::NewView(new_view)
    }
}

impl Into<ProgressMessage> for HotStuffMessage {
    fn into(self) -> ProgressMessage {
        ProgressMessage::HotStuffMessage(self)
    }
}

/// Message broadcasted by a leader in `view` to propose to other validators that the block tree
/// identified by `chain_id` be extended by inserting `block`.
#[derive(Clone, BorshSerialize, BorshDeserialize)]
pub struct Proposal {
    /// `ChainID` of the block tree that this `Proposal` should extend.
    pub chain_id: ChainID,

    /// Current `ViewNumber` of the [proposer](super::roles::is_proposer) that created this
    /// `Proposal`.
    pub view: ViewNumber,

    /// A `Block` extending the chain identified by `chain_id`.
    pub block: Block,
    // MonadBFT: TC from the previous view (present for reproposals)
    pub tc: Option<TimeoutCertificate>,
    // MonadBFT B2: NEC proving the skipped block is safe to abandon
    pub nec: Option<NoEndorsementCertificate>,
}

impl Proposal {
    /// Returns true if this proposal is a reproposal (MonadBFT Case 4).
    pub fn is_reproposal(&self) -> bool {
        self.tc.as_ref().map_or(false, |tc| tc.high_tip_is_winner)
    }
}

/// Message broadcasted by a leader in `view` to "nudge" other validators to participate in the voting
/// phase after `justify.phase` in order to make progress in committing a **validator-set-updating**
/// block in the block tree identified by `chain_id`.
///
/// # Permissible variants of `justify.phase`
///
/// `nudge.justify.phase` must be `Prepare`, `Precommit`, or `Commit`. This invariant is enforced in
/// two places:
/// 1. When a validator creates a `Nudge` using [`new`](Self::new).
/// 2. When a replica receives a `Nudge` and checks the
///    [`safe_nudge`](crate::block_tree::invariants::safe_nudge) predicate.
#[derive(Clone, BorshSerialize, BorshDeserialize)]
pub struct Nudge {
    /// `ChainID` of the block tree that `justify.block` is part of.
    pub chain_id: ChainID,

    /// Current `ViewNumber` of the [proposer](super::roles::is_proposer) that created this
    /// `Nudge`.
    pub view: ViewNumber,

    /// [`PhaseVote`]s for this `Nudge` should be for the `Phase` immediately after `justify.phase`. E.g.,
    /// if `justify.phase == Precommit`, then `phase_vote.phase` should be `Commit`.
    pub justify: PhaseCertificate,
}

impl Nudge {
    /// Create a new `Nudge` message containing the given `chain_id`, `view`, and `justify`-ing PC.
    ///
    /// # Panics
    ///
    /// `justify.phase` must be `Prepare` or `Precommit`. This function panics otherwise.
    pub fn new(chain_id: ChainID, view: ViewNumber, justify: PhaseCertificate) -> Self {
        assert!(
            justify.phase.is_prepare() || justify.phase.is_precommit() || justify.phase.is_commit()
        );

        Self {
            chain_id,
            view,
            justify,
        }
    }
}

/// Message sent by a validator to [a leader of `view + 1`](super::roles::phase_vote_recipient) to
/// indicate that the validator agrees to a specific [`Proposal`] or [`Nudge`].
#[derive(Clone, BorshSerialize, BorshDeserialize)]
pub struct PhaseVote {
    /// `chain_id` field of the `Proposal` or `Nudge` associated with this `PhaseVote`.
    pub chain_id: ChainID,

    /// `view` field of the `Proposal` or `Nudge` associated with this `PhaseVote`.
    pub view: ViewNumber,

    /// `block` field of the `Proposal` or `Nudge` associated with this `PhaseVote`.
    pub block: CryptoHash,

    /// `phase` field of the `Proposal` or `Nudge` associated with this `PhaseVote`.
    pub phase: Phase,

    /// Digital signature formed using
    pub signature: SignatureBytes,
}

impl PhaseVote {
    /// Create a `PhaseVote` for the given `chain_id`, `view`, `block`, and `phase` by signing over the values
    /// with the provided `keypair`.
    pub(crate) fn new(
        keypair: &Keypair,
        chain_id: ChainID,
        view: ViewNumber,
        block: CryptoHash,
        phase: Phase,
    ) -> Self {
        let message_bytes = &(chain_id, view, block, phase).try_to_vec().unwrap();
        let signature = keypair.sign(message_bytes);

        Self {
            chain_id,
            view,
            block,
            phase,
            signature,
        }
    }
}

impl SignedMessage for PhaseVote {
    fn message_bytes(&self) -> Vec<u8> {
        (self.chain_id, self.view, self.block, self.phase)
            .try_to_vec()
            .unwrap()
    }

    fn signature_bytes(&self) -> SignatureBytes {
        self.signature
    }
}

impl Vote for PhaseVote {
    fn chain_id(&self) -> ChainID {
        self.chain_id
    }

    fn view(&self) -> ViewNumber {
        self.view
    }
}

/// Message sent by a replica to [the leaders of the next view](super::roles::new_view_recipients) on
/// view timeout to make them aware of the replica's `highest_pc`.
///
/// # `NewView` and view synchronization
///
/// In the original HotStuff protocol, the leader of the next view keeps track of the number of
/// `NewView` messages collected in the current view with the aim of advancing to the next view once a
/// quorum of `NewView` messages are seen. This behavior implements a rudimentary view synchronization
/// mechanism, which is helpful in the original HotStuff protocol because it did not come with a
/// "fully-featured" BFT view synchronization mechanism.
///
/// HotStuff-rs, on the other hand, *does* include a separate BFT view synchronization mechanism (in the
/// form of the [Pacemaker](crate::pacemaker) module). Therefore, we deem this behavior unnecessary and
/// do not implement it.
#[derive(Clone, BorshSerialize, BorshDeserialize)]
pub struct NewView {
    /// `ChainID` of the block tree that `highest_pc.block` is part of.
    pub chain_id: ChainID,

    /// The `view` that the replica sending this `NewView` is exiting.
    pub view: ViewNumber,

    /// The sending replica's `highest_pc`.
    pub highest_pc: PhaseCertificate,
}

// ============================================================================
// MonadBFT B2: NEC recovery message types
// ============================================================================

/// Request for the high_tip block during RECOVER (Algorithm 7).
/// Sent point-to-point to κ = f+1 validators.
#[derive(Clone, BorshSerialize, BorshDeserialize)]
pub struct ProposalRequest {
    pub chain_id: ChainID,
    /// The view the requesting leader is in (tc.view + 1).
    pub view: ViewNumber,
    pub tc: TimeoutCertificate,
}

/// Response containing the requested block.
/// Sent point-to-point back to the requesting leader.
#[derive(Clone, BorshSerialize, BorshDeserialize)]
pub struct ProposalResponse {
    pub chain_id: ChainID,
    /// The view this response is for (tc.view + 1).
    pub view: ViewNumber,
    pub proposal: Proposal,
}

/// Request for No-Endorsement attestations during RECOVER.
/// Broadcast to ALL validators.
#[derive(Clone, BorshSerialize, BorshDeserialize)]
pub struct NERequest {
    pub chain_id: ChainID,
    /// The view this NE request is for (tc.view + 1).
    pub view: ViewNumber,
    pub tc: TimeoutCertificate,
}

/// A single No-Endorsement attestation from a validator.
/// Sent point-to-point to the leader performing RECOVER.
#[derive(Clone, BorshSerialize, BorshDeserialize)]
pub struct NEMessage {
    pub chain_id: ChainID,
    /// The view this NE is for (tc.view + 1).
    pub view: ViewNumber,
    /// View of the QC inside the high_tip's block header.
    pub high_tip_qc_view: ViewNumber,
    /// Signature over (view, high_tip_qc_view).
    pub signature: SignatureBytes,
}

impl From<ProposalRequest> for HotStuffMessage {
    fn from(req: ProposalRequest) -> Self {
        HotStuffMessage::ProposalRequest(req)
    }
}

impl From<ProposalResponse> for HotStuffMessage {
    fn from(resp: ProposalResponse) -> Self {
        HotStuffMessage::ProposalResponse(resp)
    }
}

impl From<NERequest> for HotStuffMessage {
    fn from(req: NERequest) -> Self {
        HotStuffMessage::NERequest(req)
    }
}

impl From<NEMessage> for HotStuffMessage {
    fn from(ne: NEMessage) -> Self {
        HotStuffMessage::NE(ne)
    }
}

// ============================================================================
// Hybrid pipelining: header-first propagation types
// ============================================================================

/// Lightweight proposal header broadcast via gossip (<1KB without body data).
/// Validators vote on this before fetching the full block body.
#[derive(Clone, BorshSerialize, BorshDeserialize, PartialEq, Eq)]
pub struct ProposalHeader {
    pub chain_id: ChainID,
    pub view: ViewNumber,
    pub block_hash: CryptoHash,
    pub height: BlockHeight,
    pub data_hash: CryptoHash,
    pub justify: PhaseCertificate,
    pub tc: Option<TimeoutCertificate>,
    pub nec: Option<NoEndorsementCertificate>,
    pub has_validator_set_updates: bool,
}

impl ProposalHeader {
    pub fn from_proposal(p: &Proposal, has_validator_set_updates: bool) -> Self {
        ProposalHeader {
            chain_id: p.chain_id,
            view: p.view,
            block_hash: p.block.hash,
            height: p.block.height,
            data_hash: p.block.data_hash,
            justify: p.block.justify.clone(),
            tc: p.tc.clone(),
            nec: p.nec.clone(),
            has_validator_set_updates,
        }
    }
}

impl From<ProposalHeader> for HotStuffMessage {
    fn from(header: ProposalHeader) -> Self {
        HotStuffMessage::ProposalHeader(header)
    }
}

/// Request for the full block body (sent point-to-point to the proposer).
#[derive(Clone, BorshSerialize, BorshDeserialize, PartialEq, Eq)]
pub struct BlockDataRequest {
    pub chain_id: ChainID,
    pub view: ViewNumber,
    pub block_hash: CryptoHash,
}

impl From<BlockDataRequest> for HotStuffMessage {
    fn from(req: BlockDataRequest) -> Self {
        HotStuffMessage::BlockDataRequest(req)
    }
}

/// Response containing the full block (sent point-to-point back to requester).
#[derive(Clone, BorshSerialize, BorshDeserialize)]
pub struct BlockDataResponse {
    pub view: ViewNumber,
    pub block: Block,
}

impl From<BlockDataResponse> for HotStuffMessage {
    fn from(resp: BlockDataResponse) -> Self {
        HotStuffMessage::BlockDataResponse(resp)
    }
}

/// Tracks pending block body fetches for the hybrid propagation protocol.
/// Maps block_hash → (header, set of peers we've requested from).
pub type PendingHeaders = HashMap<CryptoHash, ProposalHeader>;
pub type PendingBodies = HashMap<CryptoHash, Block>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hotstuff::types::PhaseCertificate;
    use crate::types::data_types::*;
    use borsh::BorshDeserialize;

    fn genesis_pc() -> PhaseCertificate {
        PhaseCertificate::genesis_pc()
    }

    #[test]
    fn proposal_header_roundtrip_borsh() {
        let header = ProposalHeader {
            chain_id: ChainID::new(1),
            view: ViewNumber::new(5),
            block_hash: CryptoHash::new([0xAA; 32]),
            height: BlockHeight::new(3),
            data_hash: CryptoHash::new([0xBB; 32]),
            justify: genesis_pc(),
            tc: None,
            nec: None,
            has_validator_set_updates: false,
        };
        let bytes = header.try_to_vec().unwrap();
        assert!(
            bytes.len() < 1024,
            "header serialized to {} bytes, must be <1KB",
            bytes.len()
        );
        let decoded = ProposalHeader::try_from_slice(&bytes).unwrap();
        assert!(header == decoded, "roundtrip mismatch");
    }

    #[test]
    fn proposal_header_from_proposal() {
        let pc = genesis_pc();
        let block = Block::new(
            BlockHeight::new(1),
            pc.clone(),
            CryptoHash::new([0xCC; 32]),
            Data::new(vec![Datum::new(vec![1, 2, 3])]),
        );
        let proposal = Proposal {
            chain_id: ChainID::new(1),
            view: ViewNumber::new(5),
            block: block.clone(),
            tc: None,
            nec: None,
        };
        let header = ProposalHeader::from_proposal(&proposal, false);
        assert!(header.chain_id == proposal.chain_id);
        assert_eq!(header.view, proposal.view);
        assert_eq!(header.block_hash, block.hash);
        assert_eq!(header.height, block.height);
        assert_eq!(header.data_hash, block.data_hash);
        assert!(header.justify == block.justify);
        assert!(!header.has_validator_set_updates);
    }

    #[test]
    fn block_data_request_roundtrip_borsh() {
        let req = BlockDataRequest {
            chain_id: ChainID::new(1),
            view: ViewNumber::new(1),
            block_hash: CryptoHash::new([0xDD; 32]),
        };
        let bytes = req.try_to_vec().unwrap();
        assert!(
            bytes.len() < 100,
            "request serialized to {} bytes",
            bytes.len()
        );
        let decoded = BlockDataRequest::try_from_slice(&bytes).unwrap();
        assert!(req == decoded, "roundtrip mismatch");
    }

    #[test]
    fn block_data_response_roundtrip_borsh() {
        let block = Block::new(
            BlockHeight::new(1),
            genesis_pc(),
            CryptoHash::new([0xCC; 32]),
            Data::new(vec![Datum::new(vec![0u8; 4000])]),
        );
        let resp = BlockDataResponse {
            view: ViewNumber::new(1),
            block: block.clone(),
        };
        let bytes = resp.try_to_vec().unwrap();
        // Block-data responses ride the dedicated /torus/block-data protocol
        // (16 MB codec cap in torus-network), NOT /torus/direct — 4 MB here is
        // just a conservative bound for this small fixture.
        assert!(
            bytes.len() < 4 * 1024 * 1024,
            "response must stay well under the block-data codec cap"
        );
        let decoded = BlockDataResponse::try_from_slice(&bytes).unwrap();
        assert_eq!(decoded.block.hash, block.hash);
    }

    #[test]
    fn proposal_header_much_smaller_than_full_proposal() {
        let big_data = Data::new(vec![Datum::new(vec![0u8; 100_000])]);
        let block = Block::new(
            BlockHeight::new(1),
            genesis_pc(),
            CryptoHash::new([0xCC; 32]),
            big_data,
        );
        let proposal = Proposal {
            chain_id: ChainID::new(1),
            view: ViewNumber::new(5),
            block,
            tc: None,
            nec: None,
        };
        let proposal_bytes = proposal.try_to_vec().unwrap();
        let header = ProposalHeader::from_proposal(&proposal, false);
        let header_bytes = header.try_to_vec().unwrap();
        assert!(
            header_bytes.len() * 10 < proposal_bytes.len(),
            "header ({} bytes) should be at least 10x smaller than proposal ({} bytes)",
            header_bytes.len(),
            proposal_bytes.len()
        );
    }

    #[test]
    fn hotstuff_message_variants_for_hybrid_pipelining() {
        let header = ProposalHeader {
            chain_id: ChainID::new(1),
            view: ViewNumber::new(5),
            block_hash: CryptoHash::new([0xAA; 32]),
            height: BlockHeight::new(3),
            data_hash: CryptoHash::new([0xBB; 32]),
            justify: genesis_pc(),
            tc: None,
            nec: None,
            has_validator_set_updates: false,
        };
        let msg: HotStuffMessage = header.into();
        assert!(msg.chain_id() == ChainID::new(1));
        assert_eq!(msg.view(), ViewNumber::new(5));

        let req = BlockDataRequest {
            chain_id: ChainID::new(2),
            view: ViewNumber::new(3),
            block_hash: CryptoHash::new([0xDD; 32]),
        };
        let msg: HotStuffMessage = req.into();
        assert!(msg.chain_id() == ChainID::new(2));
        assert_eq!(msg.view(), ViewNumber::new(3));
    }
}
