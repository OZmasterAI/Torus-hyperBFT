//! s138 option 1 (docs/plans/vset-membership-sync-remainder.md): the record a replica keeps for each
//! validator-set update it committed on the update's Commit PC.
//!
//! An updating block U is committed by its Commit PC, signed by a quorum of the set before U
//! (`S_old`). Blocks carry only their parent's justify, so U's child carries U's Decide PC, signed by
//! the set after U. A replica that missed U's Commit PC (a laggard, a joiner, a node syncing from
//! genesis) can check that Decide PC only when its signers hold more than `S_old`'s fault bound of
//! old power ([`PhaseCertificate::is_correct`](crate::hotstuff::types::PhaseCertificate)); a disjoint
//! replacement or a large power shift fails that check. The Commit PC is the evidence such a replica
//! needs, and nothing else keeps it once the highest PC moves on. So the replica that commits U on
//! its Commit PC stores the PC here ([`VALIDATOR_SET_UPDATE_RECORDS`](super::variables::VALIDATOR_SET_UPDATE_RECORDS)
//! `++ height(U)`), in the same write batch as the commit, and its block-sync server ends a batch at
//! U and sends the PC as the batch's terminal PC.
//!
//! Records are never pruned: a node syncing from genesis needs every one.

use borsh::{BorshDeserialize, BorshSerialize};

use crate::{
    hotstuff::types::PhaseCertificate,
    types::{data_types::BlockHeight, update_sets::ValidatorSetUpdatesBytes},
};

use super::variables::{concat, VALIDATOR_SET_UPDATE_RECORDS};

/// What a replica stores for a validator-set update U it committed on U's Commit PC.
#[derive(Clone, BorshSerialize, BorshDeserialize)]
pub struct ValidatorSetUpdateRecord {
    /// U's validator-set updates (`S_new = S_old ⊕ updates`).
    pub updates: ValidatorSetUpdatesBytes,
    /// U's Commit PC (`commit_pc.block` is U's hash), signed by a quorum of `S_old`.
    pub commit_pc: PhaseCertificate,
}

/// The key of the record of the update committed at `height`: `VALIDATOR_SET_UPDATE_RECORDS ++
/// borsh(height)` (8 bytes, little-endian).
pub fn validator_set_update_record_key(height: BlockHeight) -> Vec<u8> {
    concat(&VALIDATOR_SET_UPDATE_RECORDS, &height.int().to_le_bytes())
}
