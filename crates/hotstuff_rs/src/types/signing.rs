//! The bytes every consensus signature is made over (docs/plans/tc-required-and-tip-signing.md §3).
//!
//! Each kind of signed consensus message has its own domain tag, prepended to the borsh encoding of the
//! signed fields, so a signature made for one kind can never verify as another:
//!
//! | Message | Signed bytes |
//! |---|---|
//! | proposal header (F1) | [`DOMAIN_PROPOSAL_HEADER`] `‖ (chain_id, view, block_hash)` |
//! | phase vote | [`DOMAIN_PHASE_VOTE`] `‖ (chain_id, view, block, phase)` |
//! | timeout vote (F3) | [`DOMAIN_TIMEOUT_VOTE`] `‖ (chain_id, view, Option<(tip_hash, tip_view)>, Option<(qc_view, qc_block)>)` |
//! | no-endorsement | [`DOMAIN_NO_ENDORSEMENT`] `‖ (chain_id, view, high_tip_qc_view)` |
//!
//! Without the tags, a leader's header signature over `(chain_id, view, block_hash)` had the same
//! 48-byte layout as its own timeout vote with a tip and no QC, and a no-endorsement over
//! `(view, qc_view)` the same 16-byte layout as a bare timeout vote `(chain_id, view)`.
//!
//! The tags are part of the genesis format: changing one after genesis would need a height-gated
//! switch. No tag is a prefix of another, and each body is a fixed borsh layout (options carry a
//! presence byte), so every signed byte string decodes one way.

use borsh::BorshSerialize;

use crate::hotstuff::types::Phase;

use super::{
    crypto_primitives::{Signer, SigningKey},
    data_types::{ChainID, CryptoHash, SignatureBytes, ViewNumber},
};

/// Domain tag of a leader's proposal-header signature (F1).
pub const DOMAIN_PROPOSAL_HEADER: &[u8] = b"torus-hotstuff/v1/proposal-header";

/// Domain tag of a phase vote.
pub const DOMAIN_PHASE_VOTE: &[u8] = b"torus-hotstuff/v1/phase-vote";

/// Domain tag of a timeout vote.
pub const DOMAIN_TIMEOUT_VOTE: &[u8] = b"torus-hotstuff/v1/timeout-vote";

/// Domain tag of a no-endorsement (NE) attestation.
pub const DOMAIN_NO_ENDORSEMENT: &[u8] = b"torus-hotstuff/v1/no-endorsement";

/// Every domain tag.
pub const DOMAINS: [&[u8]; 4] = [
    DOMAIN_PROPOSAL_HEADER,
    DOMAIN_PHASE_VOTE,
    DOMAIN_TIMEOUT_VOTE,
    DOMAIN_NO_ENDORSEMENT,
];

/// What a timeout vote signs about its local tip: `(block hash, view)` (F3).
pub type TipSummary = (CryptoHash, ViewNumber);

/// What a timeout vote signs about its highest QC: `(view, block hash)`.
pub type QcSummary = (ViewNumber, CryptoHash);

// Phase-C skeleton: the legacy (untagged) layouts; the tags land with the checks.
#[allow(dead_code)]
fn tagged(domain: &[u8], body: impl BorshSerialize) -> Vec<u8> {
    let mut bytes = domain.to_vec();
    body.serialize(&mut bytes).expect("borsh into a Vec cannot fail");
    bytes
}

/// The bytes a leader signs for its proposal (and header) of `block_hash` in `view` (F1). The block
/// hash commits the height, justify and data hash ([`Block::hash`](super::block::Block::hash)).
pub fn proposal_header_bytes(chain_id: ChainID, view: ViewNumber, block_hash: &CryptoHash) -> Vec<u8> {
    (chain_id, view, block_hash).try_to_vec().unwrap()
}

/// The bytes a validator signs to phase-vote for `block` in `view`.
pub fn phase_vote_bytes(chain_id: ChainID, view: ViewNumber, block: &CryptoHash, phase: Phase) -> Vec<u8> {
    (chain_id, view, block, phase).try_to_vec().unwrap()
}

/// The bytes a validator signs to time out of `view`, binding its local tip's hash and view (F3)
/// and its highest QC's view and block.
pub fn timeout_vote_bytes(
    chain_id: ChainID,
    view: ViewNumber,
    tip: Option<TipSummary>,
    highest_qc: Option<QcSummary>,
) -> Vec<u8> {
    let mut bytes = (chain_id, view).try_to_vec().unwrap();
    if let Some((hash, _)) = tip {
        bytes.extend_from_slice(&hash.bytes());
    }
    if let Some((qc_view, qc_block)) = highest_qc {
        bytes.extend_from_slice(&qc_view.int().to_be_bytes());
        bytes.extend_from_slice(&qc_block.bytes());
    }
    bytes
}

/// The bytes a validator signs to attest that it did not vote for the high tip whose QC is of
/// `high_tip_qc_view`, for the recovery in `view`.
pub fn no_endorsement_bytes(_chain_id: ChainID, view: ViewNumber, high_tip_qc_view: ViewNumber) -> Vec<u8> {
    (view, high_tip_qc_view).try_to_vec().unwrap()
}

/// Sign [`proposal_header_bytes`] with `key` (the leader of `view`).
pub fn sign_proposal_header(
    key: &SigningKey,
    chain_id: ChainID,
    view: ViewNumber,
    block_hash: &CryptoHash,
) -> SignatureBytes {
    SignatureBytes::new(key.sign(&proposal_header_bytes(chain_id, view, block_hash)).to_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn domain_tags_are_distinct_and_prefix_free() {
        for (i, a) in DOMAINS.iter().enumerate() {
            for (j, b) in DOMAINS.iter().enumerate() {
                if i != j {
                    assert!(!b.starts_with(a), "{:?} is a prefix of {:?}", a, b);
                }
            }
        }
    }

    #[test]
    fn every_signed_layout_starts_with_its_tag() {
        let (chain, view, hash) = (ChainID::new(7), ViewNumber::new(9), CryptoHash::new([3; 32]));
        assert!(proposal_header_bytes(chain, view, &hash).starts_with(DOMAIN_PROPOSAL_HEADER));
        assert!(phase_vote_bytes(chain, view, &hash, Phase::Generic).starts_with(DOMAIN_PHASE_VOTE));
        assert!(timeout_vote_bytes(chain, view, None, None).starts_with(DOMAIN_TIMEOUT_VOTE));
        assert!(no_endorsement_bytes(chain, view, view).starts_with(DOMAIN_NO_ENDORSEMENT));
    }

    /// The tip and QC summaries cannot be confused: a vote with only a tip
    /// and one with only a QC sign different bytes even over the same hash
    /// and view.
    #[test]
    fn timeout_vote_summaries_are_unambiguous() {
        let (chain, view, hash) = (ChainID::new(1), ViewNumber::new(4), CryptoHash::new([5; 32]));
        let tip_only = timeout_vote_bytes(chain, view, Some((hash, view)), None);
        let qc_only = timeout_vote_bytes(chain, view, None, Some((view, hash)));
        assert_ne!(tip_only, qc_only);
    }
}
