//! Test helpers: the signing keys unit tests use, and leader signatures for
//! test-built proposal headers (docs/plans/tc-required-and-tip-signing.md §3 F1).

use crate::hotstuff::messages::ProposalHeader;
use crate::pacemaker::types::TipInfo;
use crate::types::crypto_primitives::{SigningKey, VerifyingKey};

/// The signing key of `vk`, for test keys made from one repeated seed byte
/// (`SigningKey::from_bytes(&[seed; 32])`, as `signing_keys` and the other unit
/// fixtures make them).
pub(crate) fn key_of(vk: &VerifyingKey) -> SigningKey {
    (0..=u8::MAX)
        .map(|seed| SigningKey::from_bytes(&[seed; 32]))
        .find(|key| key.verifying_key() == *vk)
        .expect("test validator keys are SigningKey::from_bytes(&[seed; 32])")
}

/// `header` signed by `leader` (one of the seed keys).
pub(crate) fn signed_by(header: ProposalHeader, leader: &VerifyingKey) -> ProposalHeader {
    header.signed(&key_of(leader))
}

/// The tip a voter holds after voting for `header` signed by `leader`.
pub(crate) fn tip_signed_by(header: ProposalHeader, leader: &VerifyingKey) -> TipInfo {
    TipInfo::from_header(&signed_by(header, leader))
}

/// The header announcing `block` at `view`, not yet signed (no TC, NEC or
/// validator-set updates).
pub(crate) fn unsigned_header(
    chain_id: crate::types::data_types::ChainID,
    block: &crate::types::block::Block,
    view: crate::types::data_types::ViewNumber,
) -> ProposalHeader {
    ProposalHeader {
        chain_id,
        view,
        block_hash: block.hash,
        height: block.height,
        data_hash: block.data_hash,
        justify: block.justify.clone(),
        tc: None,
        nec: None,
        has_validator_set_updates: false,
        leader_signature: crate::types::data_types::SignatureBytes::new([0; 64]),
    }
}

/// The header announcing `block` at `view`, signed by leader(`view`) of the
/// four equal seed validators `[1; 32]` .. `[4; 32]` (most unit fixtures).
pub(crate) fn seed_leader_header(
    chain_id: crate::types::data_types::ChainID,
    block: &crate::types::block::Block,
    view: crate::types::data_types::ViewNumber,
) -> ProposalHeader {
    let mut set = crate::types::validator_set::ValidatorSet::new();
    for seed in 1..=4u8 {
        set.put(&SigningKey::from_bytes(&[seed; 32]).verifying_key(), crate::types::data_types::Power::new(1));
    }
    let leader = crate::pacemaker::select_leader(view, &set);
    unsigned_header(chain_id, block, view).signed(&key_of(&leader))
}
