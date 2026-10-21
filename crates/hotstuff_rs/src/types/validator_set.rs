/*
    Copyright © 2023, ParallelChain Lab
    Licensed under the Apache License, Version 2.0: http://www.apache.org/licenses/LICENSE-2.0
*/

//! Types that store information about validator sets or updates to validator sets.

use borsh::{BorshDeserialize, BorshSerialize};
use ed25519_dalek::ed25519::Error;
use std::{collections::HashMap, slice};

use super::{
    data_types::{BlockHeight, Power, TotalPower},
    update_sets::{
        LeaderExclusionUpdatesBytes, ValidatorSetUpdates, ValidatorSetUpdatesBytes,
        VerifyingKeyBytes,
    },
};

pub use ed25519_dalek::{Signature, SigningKey, VerifyingKey};

/// Stores the identities of validators and their voting powers.
///
/// ## Ordering of validators
///
/// `ValidatorSet` internally maintains the list of validators in ascending order of their
/// `VerifyingKey`s, and avails the methods [`validators`](ValidatorSet::validators),
/// [`validators_and_powers`](ValidatorSet::validators_and_powers), and
/// [`position`](ValidatorSet::position) that users can use to get them in this order.
///
/// ## Limits to total power
///
/// Users must make sure that the total power of the validator set does not exceed `u128::MAX/2`.
///
/// ## Leader exclusion
///
/// [`leader_excluded`](Self::leader_excluded) is a sorted subset of the validators that leader
/// selection skips (dead-leader skip, docs/plans/dead-leader-skip.md): an excluded validator keeps
/// its voting power, so quorums and voting are unchanged; it only stops leading views. It changes
/// only through [`ValidatorSetUpdates`] (the exclusion delta), so every replica agrees on it. Empty
/// set: leader selection is exactly what it was before the set existed.
#[derive(Clone, PartialEq)]
pub struct ValidatorSet {
    // The verifying keys of validators are included here in ascending order.
    validators: Vec<VerifyingKey>,
    powers: HashMap<VerifyingKey, Power>,
    // Members excluded from leader selection, ascending order; always a subset of `validators`.
    leader_excluded: Vec<VerifyingKey>,
}

impl Default for ValidatorSet {
    // Create an empty validator set.
    fn default() -> Self {
        ValidatorSet::new()
    }
}

impl ValidatorSet {
    /// Create an empty validator set.
    pub fn new() -> ValidatorSet {
        Self {
            validators: Vec::new(),
            powers: HashMap::new(),
            leader_excluded: Vec::new(),
        }
    }

    /// Put a `validator` with the specified `power` into the validator set, placing them in a position that
    /// preserves the [ordering of validators](Self#order-of-validators).
    ///
    /// If `validator` already exists in the validator set, this function updates its power instead.
    pub fn put(&mut self, validator: &VerifyingKey, power: Power) {
        if !self.contains(validator) {
            let validator_bytes = validator.to_bytes();
            let insert_pos = self
                .validators
                .binary_search_by(|v| v.to_bytes().cmp(&validator_bytes))
                .unwrap_err();
            self.validators.insert(insert_pos, *validator);
        }

        self.powers.insert(*validator, power);
    }

    /// Remove `validator` from the validator set, if it actually is in the validator set.
    ///
    /// If a validator is removed, then its `VerifyingKey` is returned with its power in the validator set
    /// before the removal. This `VerifyingKey` will be exactly equal to `validator`. If a validator is not
    /// removed, then this function will return `None`.
    pub fn remove(&mut self, validator: &VerifyingKey) -> Option<(VerifyingKey, Power)> {
        let validator_bytes = validator.to_bytes();
        if let Ok(pos) = self
            .validators
            .binary_search_by(|v| v.to_bytes().cmp(&validator_bytes))
        {
            self.validators.remove(pos);
            self.readmit_leader(validator);
            self.powers.remove_entry(validator)
        } else {
            None
        }
    }

    /// Apply validator set `updates` to the validator set. This entails calling `put` for all of the
    /// validators inserted, calling `remove` for all of the validators deleted, then applying the
    /// leader exclusion delta: `exclude_leader` for every exclusion (members only) and
    /// `readmit_leader` for every re-admission.
    pub fn apply_updates(&mut self, updates: &ValidatorSetUpdates) {
        for (peer, new_power) in updates.inserts() {
            self.put(peer, *new_power);
        }

        for peer in updates.deletes() {
            self.remove(peer);
        }

        for peer in updates.leader_readmits() {
            self.readmit_leader(peer);
        }

        for peer in updates.leader_excludes() {
            self.exclude_leader(peer);
        }
    }

    /// Add `validator` to the [leader-excluded](Self#leader-exclusion) set. Returns `false` (and does
    /// nothing) if `validator` is not in the validator set or is already excluded.
    pub fn exclude_leader(&mut self, validator: &VerifyingKey) -> bool {
        if !self.contains(validator) {
            return false;
        }
        let validator_bytes = validator.to_bytes();
        match self
            .leader_excluded
            .binary_search_by(|v| v.to_bytes().cmp(&validator_bytes))
        {
            Ok(_) => false,
            Err(pos) => {
                self.leader_excluded.insert(pos, *validator);
                true
            }
        }
    }

    /// Remove `validator` from the [leader-excluded](Self#leader-exclusion) set. Returns whether it was
    /// excluded.
    pub fn readmit_leader(&mut self, validator: &VerifyingKey) -> bool {
        let validator_bytes = validator.to_bytes();
        match self
            .leader_excluded
            .binary_search_by(|v| v.to_bytes().cmp(&validator_bytes))
        {
            Ok(pos) => {
                self.leader_excluded.remove(pos);
                true
            }
            Err(_) => false,
        }
    }

    /// Whether `validator` is in the [leader-excluded](Self#leader-exclusion) set.
    pub fn is_leader_excluded(&self, validator: &VerifyingKey) -> bool {
        let validator_bytes = validator.to_bytes();
        self.leader_excluded
            .binary_search_by(|v| v.to_bytes().cmp(&validator_bytes))
            .is_ok()
    }

    /// Iterator over the [leader-excluded](Self#leader-exclusion) validators in ascending order.
    pub fn leader_excluded(&self) -> slice::Iter<'_, VerifyingKey> {
        self.leader_excluded.iter()
    }

    /// Sum of the powers of the validators that are not [leader-excluded](Self#leader-exclusion).
    pub fn leader_eligible_power(&self) -> TotalPower {
        let mut eligible = self.total_power();
        for v in &self.leader_excluded {
            if let Some(p) = self.powers.get(v) {
                eligible = TotalPower::new(eligible.int() - p.int() as u128);
            }
        }
        eligible
    }

    /// Whether leader selection skips the [leader-excluded](Self#leader-exclusion) validators: the set
    /// is non-empty and the eligible power still reaches a [quorum](Self::quorum). When the eligible
    /// power falls below a quorum, selection falls back to all members.
    pub fn leader_exclusion_active(&self) -> bool {
        !self.leader_excluded.is_empty() && self.leader_eligible_power() >= self.quorum()
    }

    /// Get the power of the specified `validator` inside the validator set.
    pub fn power(&self, validator: &VerifyingKey) -> Option<&Power> {
        self.powers.get(validator)
    }

    /// Get the sum of the powers of all of the validators inside the validator set.
    pub fn total_power(&self) -> TotalPower {
        let mut total_power = TotalPower::new(0);
        for power in self.powers.values() {
            total_power += *power
        }
        total_power
    }

    /// Check whether the validator set contains `validator`.
    pub fn contains(&self, validator: &VerifyingKey) -> bool {
        self.powers.contains_key(validator)
    }

    /// Get an iterator through validators' verifying keys which walks through them in ascending order.
    pub fn validators(&self) -> slice::Iter<'_, VerifyingKey> {
        self.validators.iter()
    }

    /// Get a vector containing each validator and its power, in ascending order of the validators' verifying keys.
    pub fn validators_and_powers(&self) -> Vec<(VerifyingKey, Power)> {
        self.validators()
            .map(|v| (*v, *self.power(v).unwrap()))
            .collect()
    }

    /// Get the number of validators currently in the validator set.
    pub fn len(&self) -> usize {
        self.validators.len()
    }

    /// Check whether the validator set is empty (i.e., `self.len() == 0`).
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Get the index of the given `validator` in the [sorted order](Self#order-of-validators) of
    /// `VerifyingKey`s in the validator set, if it is actually in the validator set.
    pub fn position(&self, validator: &VerifyingKey) -> Option<usize> {
        let validator_bytes = validator.to_bytes();
        self.validators
            .binary_search_by(|v| v.to_bytes().cmp(&validator_bytes))
            .ok()
    }

    /// Compute the total power that a certificate must match or exceed (`>=`) in order to count as a quorum
    /// under the validator set.
    pub(crate) fn quorum(&self) -> TotalPower {
        const TOTAL_POWER_OVERFLOW: &str =
            "Validator set power exceeds u128::MAX/2. Read the itemdoc for `ValidatorSet`.";

        TotalPower::new(
            (self
                .total_power()
                .int()
                .checked_mul(2)
                .expect(TOTAL_POWER_OVERFLOW)
                / 3)
                + 1,
        )
    }
}

/// Intermediate representation of [`ValidatorSet`] for safe serialization and deserialization.
///
/// To serialize an instance of `ValidatorSet`, convert it a `ValidatorSetBytes` using the former type's
/// implementation of `Into<ValidatorSetBytes>`, then, serialize the `ValidatorSetBytes` using Borsh.
/// Reverse the steps to deserialize a `ValidatorSet`.
///
/// ## Rationale
///
/// This type exists because it is not straightforward to implement `BorshSerialize` and
/// `BorshDeserialize` on `ValidatorSet`, since the latter type internally contains
/// [`ed25519_dalek::VerifyingKey`], which does not implement the Borsh traits.
///
/// This type is internally exactly like `ValidatorSet`, but replaces `VerifyingKey` with
/// `VerifyingKeyBytes`, and so is straightforward to serialize and deserialize. However, this also means
/// that instances of this type are not guaranteed to contain "valid" Ed25519 verifying keys, and
/// therefore conversion from this type into `ValidatorSet` using `TryFrom` is fallible.
///
/// ## Encoding ([`encode`](Self::encode) / [`decode`](Self::decode))
///
/// The Borsh encoding of `(validators, powers)` exactly as before the leader-excluded set existed,
/// followed, only when the set is non-empty, by the Borsh encoding of the sorted
/// `leader_excluded: Vec<VerifyingKeyBytes>`. So a set with no exclusions encodes to the same bytes
/// as before, and bytes stored before the field existed decode with an empty set.
#[derive(Clone)]
pub(crate) struct ValidatorSetBytes {
    base: ValidatorSetBaseBytes,
    // The leader-excluded verifying keys, ascending order.
    leader_excluded: Vec<VerifyingKeyBytes>,
}

/// The pre-exclusion part of [`ValidatorSetBytes`] (its Borsh format is unchanged).
#[derive(Clone, BorshSerialize, BorshDeserialize)]
struct ValidatorSetBaseBytes {
    // The verifying keys of validators are included here in ascending order.
    validators: Vec<VerifyingKeyBytes>,
    powers: HashMap<VerifyingKeyBytes, Power>,
}

impl ValidatorSetBytes {
    /// Serialize (see the [encoding](Self#encoding-encode--decode)).
    pub(crate) fn encode(&self) -> std::io::Result<Vec<u8>> {
        let mut bytes = self.base.try_to_vec()?;
        if !self.leader_excluded.is_empty() {
            let mut sorted = self.leader_excluded.clone();
            sorted.sort_unstable();
            sorted.serialize(&mut bytes)?;
        }
        Ok(bytes)
    }

    /// Deserialize bytes written by [`encode`](Self::encode), or by the encoding before the
    /// leader-excluded set existed. Rejects trailing bytes and a present-but-empty or unsorted set
    /// (each set has exactly one encoding).
    pub(crate) fn decode(bytes: &[u8]) -> std::io::Result<Self> {
        let mut buf = bytes;
        let base = ValidatorSetBaseBytes::deserialize(&mut buf)?;
        let leader_excluded = if buf.is_empty() {
            Vec::new()
        } else {
            let invalid = |msg: &str| std::io::Error::new(std::io::ErrorKind::InvalidData, msg);
            let excluded = Vec::<VerifyingKeyBytes>::deserialize(&mut buf)?;
            if !buf.is_empty() {
                return Err(invalid(
                    "validator set: trailing bytes after the leader-excluded set",
                ));
            }
            if excluded.is_empty() {
                return Err(invalid(
                    "validator set: an empty leader-excluded set must be omitted",
                ));
            }
            if excluded.windows(2).any(|w| w[0] >= w[1]) {
                return Err(invalid(
                    "validator set: leader-excluded keys must be strictly ascending",
                ));
            }
            excluded
        };
        Ok(Self {
            base,
            leader_excluded,
        })
    }
}

impl TryFrom<ValidatorSetBytes> for ValidatorSet {
    type Error = ed25519_dalek::SignatureError;

    fn try_from(value: ValidatorSetBytes) -> Result<Self, Self::Error> {
        let ValidatorSetBytes {
            base: value,
            leader_excluded,
        } = value;
        let new_validators = value
            .validators
            .iter()
            .flat_map(VerifyingKey::from_bytes)
            .collect();

        let mut new_powers = <HashMap<VerifyingKey, Power>>::new();
        let convert_and_insert = |(k, v): (&[u8; 32], &Power)| -> Result<(), Error> {
            let pk = VerifyingKey::from_bytes(k)?;
            new_powers.insert(pk, *v);
            Ok(())
        };
        let _ = value
            .powers
            .keys()
            .zip(value.powers.values())
            .try_for_each(convert_and_insert);

        let mut new_validator_set = Self {
            validators: new_validators,
            powers: new_powers,
            leader_excluded: Vec::new(),
        };
        for k in &leader_excluded {
            new_validator_set.exclude_leader(&VerifyingKey::from_bytes(k)?);
        }
        Ok(new_validator_set)
    }
}

impl From<&ValidatorSet> for ValidatorSetBytes {
    fn from(val: &ValidatorSet) -> Self {
        let new_validators = val.validators.iter().map(|pk| pk.to_bytes()).collect();

        let mut new_powers = <HashMap<VerifyingKeyBytes, Power>>::new();
        val.powers
            .keys()
            .zip(val.powers.values())
            .for_each(|(k, v)| {
                new_powers.insert(k.to_bytes(), *v); // Safety: Insert should always return None
            });

        ValidatorSetBytes {
            base: ValidatorSetBaseBytes {
                validators: new_validators,
                powers: new_powers,
            },
            leader_excluded: val.leader_excluded.iter().map(|pk| pk.to_bytes()).collect(),
        }
    }
}

/// Collection of basic information about the current (i.e., committed) validator set, and the validator
/// set history.
///
/// It is a protocol invariant that a Hotstuff-rs replica only needs to know the previous validator set
/// to validate PCs and TCs, hence we only store the previous validator set in the history.
#[derive(Clone)]
pub struct ValidatorSetState {
    committed_validator_set: ValidatorSet,
    previous_validator_set: ValidatorSet,
    update_height: Option<BlockHeight>,
    update_decided: bool,
}

impl ValidatorSetState {
    /// Create a new `ValidatorSetState`.
    pub fn new(
        committed_validator_set: ValidatorSet,
        previous_validator_set: ValidatorSet,
        update_height: Option<BlockHeight>,
        update_decided: bool,
    ) -> Self {
        Self {
            committed_validator_set,
            previous_validator_set,
            update_height,
            update_decided,
        }
    }

    /// Get the committed validator set.
    pub fn committed_validator_set(&self) -> &ValidatorSet {
        &self.committed_validator_set
    }

    /// Get the previous validator set.
    pub fn previous_validator_set(&self) -> &ValidatorSet {
        &self.previous_validator_set
    }

    /// Get the height of the block that caused the most recently committed (but perhaps not decided)
    /// validator set update.
    pub fn update_height(&self) -> &Option<BlockHeight> {
        &self.update_height
    }

    /// Get whether or not the latest validator set update (the one from
    /// [`previous_validator_set`](Self::previous_validator_set) to
    /// [`committed_validator_set`](Self::committed_validator_set)) has been decided.
    pub fn update_decided(&self) -> bool {
        self.update_decided
    }
}

/// Wraps around [`ValidatorSetUpdates`], providing additional information on whether the updates have
/// already been applied or not.
///
/// The ["Validator Set Updates Status" state variable](crate::block_tree::variables#validator-set) in
/// the block tree stores a mapping from blocks to their associated `ValidatorSetUpdatesStatus`.
pub enum ValidatorSetUpdatesStatus {
    /// The block does not update the validator set.
    None,

    /// The block's validator set updates have not been applied to the committed validator set yet.
    Pending(ValidatorSetUpdates),

    /// The block's validator set updates have been applied to the committed validator set.
    Committed,
}

impl ValidatorSetUpdatesStatus {
    /// Check whether the updates status is `Pending` or `Committed`.
    pub fn contains_updates(&self) -> bool {
        match self {
            Self::None => false,
            Self::Pending(_) | Self::Committed => true,
        }
    }

    /// Check whether the updates status is `Pending`.
    pub fn is_pending(&self) -> bool {
        matches!(self, Self::Pending(_))
    }
}

/// Intermediate representation of [`ValidatorSetUpdatesStatus`] for safe serialization and
/// deserialization.
///
/// ## Rationale
///
/// See the [related section](ValidatorSetBytes#rationale) about `ValidatorSetBytes`.
///
/// ## Variants and format
///
/// `None`, `Pending` and `Committed` keep their Borsh tags and formats. A pending update that carries
/// a leader exclusion delta is stored as `PendingWithLeaderExclusions` (a new tag, appended last);
/// one without stays `Pending`, so its bytes are the same as before the delta existed. Use
/// [`pending`](Self::pending) to pick the variant.
#[derive(Clone, BorshSerialize, BorshDeserialize)]
pub enum ValidatorSetUpdatesStatusBytes {
    None,
    Pending(ValidatorSetUpdatesBytes),
    Committed,
    PendingWithLeaderExclusions(ValidatorSetUpdatesBytes, LeaderExclusionUpdatesBytes),
}

impl ValidatorSetUpdatesStatusBytes {
    /// The serializable form of `ValidatorSetUpdatesStatus::Pending(updates)`.
    pub(crate) fn pending(updates: &ValidatorSetUpdates) -> Self {
        if updates.has_leader_exclusion_updates() {
            Self::PendingWithLeaderExclusions(
                updates.power_updates_bytes(),
                updates.leader_exclusion_updates_bytes(),
            )
        } else {
            Self::Pending(updates.power_updates_bytes())
        }
    }
}

impl TryFrom<ValidatorSetUpdatesStatusBytes> for ValidatorSetUpdatesStatus {
    type Error = ed25519_dalek::SignatureError;

    fn try_from(value: ValidatorSetUpdatesStatusBytes) -> Result<Self, Self::Error> {
        Ok(match value {
            ValidatorSetUpdatesStatusBytes::None => ValidatorSetUpdatesStatus::None,
            ValidatorSetUpdatesStatusBytes::Pending(vs_updates_bytes) => {
                ValidatorSetUpdatesStatus::Pending(ValidatorSetUpdates::try_from_bytes(
                    vs_updates_bytes,
                    None,
                )?)
            }
            ValidatorSetUpdatesStatusBytes::PendingWithLeaderExclusions(
                vs_updates_bytes,
                leader_exclusions,
            ) => ValidatorSetUpdatesStatus::Pending(ValidatorSetUpdates::try_from_bytes(
                vs_updates_bytes,
                Some(leader_exclusions),
            )?),
            ValidatorSetUpdatesStatusBytes::Committed => ValidatorSetUpdatesStatus::Committed,
        })
    }
}

#[cfg(test)]
mod leader_exclusion_tests {
    //! Dead-leader skip, phase 1: the leader-excluded set, its exclusion delta in
    //! `ValidatorSetUpdates`, and their storage encodings (docs/plans/dead-leader-skip.md §3 A4, §4).

    use super::*;
    use crate::types::update_sets::ValidatorSetUpdatesBytes;
    use borsh::{BorshDeserialize, BorshSerialize};

    fn vk(seed: u8) -> VerifyingKey {
        SigningKey::from_bytes(&[seed; 32]).verifying_key()
    }

    fn set(members: &[(u8, u64)]) -> ValidatorSet {
        let mut vs = ValidatorSet::new();
        for (seed, power) in members {
            vs.put(&vk(*seed), Power::new(*power));
        }
        vs
    }

    /// The `ValidatorSetBytes` layout before the leader-excluded set existed (base 40cea7fc).
    #[derive(BorshSerialize, BorshDeserialize)]
    struct LegacyValidatorSetBytes {
        validators: Vec<VerifyingKeyBytes>,
        powers: HashMap<VerifyingKeyBytes, Power>,
    }

    /// The `ValidatorSetUpdatesStatusBytes` layout before the exclusion delta existed.
    #[derive(BorshSerialize, BorshDeserialize)]
    enum LegacyStatusBytes {
        None,
        Pending(ValidatorSetUpdatesBytes),
        Committed,
    }

    fn legacy_bytes(vs: &ValidatorSet) -> Vec<u8> {
        LegacyValidatorSetBytes {
            validators: vs.validators().map(|v| v.to_bytes()).collect(),
            powers: vs
                .validators_and_powers()
                .into_iter()
                .map(|(v, p)| (v.to_bytes(), p))
                .collect(),
        }
        .try_to_vec()
        .unwrap()
    }

    fn round_trip(vs: &ValidatorSet) -> ValidatorSet {
        let bytes = ValidatorSetBytes::from(vs).encode().unwrap();
        ValidatorSet::try_from(ValidatorSetBytes::decode(&bytes).unwrap()).unwrap()
    }

    fn excluded(vs: &ValidatorSet) -> Vec<VerifyingKey> {
        vs.leader_excluded().copied().collect()
    }

    #[test]
    fn exclude_and_readmit_keep_a_sorted_subset_of_members() {
        let mut vs = set(&[(1, 5), (2, 5), (3, 5), (4, 5)]);
        assert!(vs.exclude_leader(&vk(3)));
        assert!(vs.exclude_leader(&vk(1)));
        assert!(!vs.exclude_leader(&vk(1)), "already excluded");
        assert!(!vs.exclude_leader(&vk(9)), "not a member");
        let mut want = vec![vk(1), vk(3)];
        want.sort_by_key(|k| k.to_bytes());
        assert_eq!(excluded(&vs), want);
        assert!(vs.is_leader_excluded(&vk(3)));
        assert!(!vs.is_leader_excluded(&vk(2)));
        assert_eq!(vs.leader_eligible_power().int(), 10);

        assert!(vs.readmit_leader(&vk(3)));
        assert!(!vs.readmit_leader(&vk(3)));
        assert_eq!(excluded(&vs), vec![vk(1)]);

        // Removing a member drops it from the excluded set too.
        assert!(vs.remove(&vk(1)).is_some());
        assert_eq!(excluded(&vs), Vec::<VerifyingKey>::new());
        // Re-inserting it does not bring the exclusion back.
        vs.put(&vk(1), Power::new(5));
        assert!(!vs.is_leader_excluded(&vk(1)));
    }

    #[test]
    fn apply_updates_applies_the_exclusion_delta() {
        let mut vs = set(&[(1, 2), (2, 2), (3, 1)]);
        vs.exclude_leader(&vk(2));

        let mut updates = ValidatorSetUpdates::new();
        updates.exclude_leader(vk(3));
        updates.readmit_leader(vk(2));
        updates.exclude_leader(vk(9)); // not a member: ignored
        assert!(updates.has_leader_exclusion_updates());
        vs.apply_updates(&updates);
        assert_eq!(excluded(&vs), vec![vk(3)]);
        assert_eq!(vs.total_power().int(), 5, "powers untouched");

        // Exclusion and re-admission of the same key cancel each other, latest wins.
        let mut u = ValidatorSetUpdates::new();
        u.exclude_leader(vk(1));
        u.readmit_leader(vk(1));
        assert_eq!(u.leader_excludes().count(), 0);
        assert_eq!(u.leader_readmits().count(), 1);
        u.exclude_leader(vk(1));
        assert_eq!(u.leader_readmits().count(), 0);

        // A new member can be inserted and excluded in the same update.
        let mut u = ValidatorSetUpdates::new();
        u.insert(vk(4), Power::new(1));
        u.exclude_leader(vk(4));
        vs.apply_updates(&u);
        assert!(vs.is_leader_excluded(&vk(4)));

        // A deleted member leaves the excluded set even if the update also excludes it.
        let mut u = ValidatorSetUpdates::new();
        u.delete(vk(3));
        u.exclude_leader(vk(3));
        vs.apply_updates(&u);
        assert!(!vs.is_leader_excluded(&vk(3)));
        assert!(!vs.contains(&vk(3)));

        // A power-only update leaves the excluded set alone.
        let mut u = ValidatorSetUpdates::new();
        u.insert(vk(1), Power::new(7));
        assert!(!u.has_leader_exclusion_updates());
        vs.apply_updates(&u);
        assert!(vs.is_leader_excluded(&vk(4)));
    }

    #[test]
    fn validator_set_with_exclusions_round_trips() {
        let mut vs = set(&[(1, 3), (2, 2), (3, 2), (4, 1)]);
        vs.exclude_leader(&vk(4));
        vs.exclude_leader(&vk(2));
        let back = round_trip(&vs);
        assert_eq!(excluded(&back), excluded(&vs));
        assert!(back == vs);
    }

    /// Empty set: the encoding is byte-identical to the pre-exclusion layout, and bytes stored
    /// before the field existed decode with an empty set.
    #[test]
    fn empty_exclusion_set_encodes_like_the_legacy_layout() {
        let vs = set(&[(1, 3), (2, 2), (3, 2)]);
        let bytes = ValidatorSetBytes::from(&vs).encode().unwrap();
        assert_eq!(bytes, legacy_bytes(&vs));

        let decoded =
            ValidatorSet::try_from(ValidatorSetBytes::decode(&legacy_bytes(&vs)).unwrap()).unwrap();
        assert!(decoded == vs);
        assert_eq!(decoded.leader_excluded().count(), 0);

        // With exclusions the encoding is the legacy bytes plus the sorted excluded keys.
        let mut ex = vs.clone();
        ex.exclude_leader(&vk(3));
        ex.exclude_leader(&vk(1));
        let bytes = ValidatorSetBytes::from(&ex).encode().unwrap();
        let mut keys = vec![vk(1).to_bytes(), vk(3).to_bytes()];
        keys.sort_unstable();
        let mut want = legacy_bytes(&vs);
        want.extend(keys.try_to_vec().unwrap());
        assert_eq!(bytes, want);
    }

    /// Each set has exactly one encoding: a present-but-empty set, an unsorted or duplicated set,
    /// and trailing bytes are rejected.
    #[test]
    fn non_canonical_exclusion_encodings_are_rejected() {
        let vs = set(&[(1, 3), (2, 2), (3, 2)]);
        let legacy = legacy_bytes(&vs);

        let mut empty_present = legacy.clone();
        empty_present.extend(Vec::<VerifyingKeyBytes>::new().try_to_vec().unwrap());
        assert!(ValidatorSetBytes::decode(&empty_present).is_err());

        let mut keys = vec![vk(1).to_bytes(), vk(3).to_bytes()];
        keys.sort_unstable();
        keys.reverse();
        let mut unsorted = legacy.clone();
        unsorted.extend(keys.try_to_vec().unwrap());
        assert!(ValidatorSetBytes::decode(&unsorted).is_err());

        let mut dup = legacy.clone();
        dup.extend(
            vec![vk(1).to_bytes(), vk(1).to_bytes()]
                .try_to_vec()
                .unwrap(),
        );
        assert!(ValidatorSetBytes::decode(&dup).is_err());

        let mut trailing = legacy.clone();
        trailing.extend(vec![vk(1).to_bytes()].try_to_vec().unwrap());
        trailing.push(0);
        assert!(ValidatorSetBytes::decode(&trailing).is_err());
    }

    fn status_round_trip(updates: &ValidatorSetUpdates) -> (Vec<u8>, ValidatorSetUpdates) {
        let bytes = ValidatorSetUpdatesStatusBytes::pending(updates)
            .try_to_vec()
            .unwrap();
        let decoded = ValidatorSetUpdatesStatusBytes::deserialize(&mut &bytes[..]).unwrap();
        match ValidatorSetUpdatesStatus::try_from(decoded).unwrap() {
            ValidatorSetUpdatesStatus::Pending(u) => (bytes, u),
            _ => panic!("expected Pending"),
        }
    }

    fn sorted(it: impl Iterator<Item = VerifyingKey>) -> Vec<[u8; 32]> {
        let mut v: Vec<[u8; 32]> = it.map(|k| k.to_bytes()).collect();
        v.sort_unstable();
        v
    }

    /// The pending update round-trips its exclusion delta through the stored status.
    #[test]
    fn pending_update_with_exclusion_delta_round_trips() {
        let mut updates = ValidatorSetUpdates::new();
        updates.insert(vk(1), Power::new(4));
        updates.delete(vk(2));
        updates.exclude_leader(vk(3));
        updates.exclude_leader(vk(5));
        updates.readmit_leader(vk(4));
        let (_, back) = status_round_trip(&updates);
        assert_eq!(
            sorted(back.leader_excludes().copied()),
            sorted([vk(3), vk(5)].into_iter())
        );
        assert_eq!(
            sorted(back.leader_readmits().copied()),
            sorted([vk(4)].into_iter())
        );
        assert_eq!(back.get_insert(&vk(1)), Some(&Power::new(4)));
        assert!(back.contains_delete(&vk(2)));

        // An exclusion-only update round-trips too.
        let mut only = ValidatorSetUpdates::new();
        only.exclude_leader(vk(7));
        let (_, back) = status_round_trip(&only);
        assert_eq!(back.leader_excludes().count(), 1);
        assert_eq!(back.inserts().count() + back.deletes().count(), 0);
    }

    /// A pending update without a delta is stored exactly as before (legacy `Pending` tag), and a
    /// legacy stored status still decodes.
    #[test]
    fn pending_update_without_delta_keeps_the_legacy_bytes() {
        let mut updates = ValidatorSetUpdates::new();
        updates.insert(vk(1), Power::new(4));
        updates.delete(vk(2));
        let (bytes, _) = status_round_trip(&updates);
        let legacy = LegacyStatusBytes::Pending(updates.power_updates_bytes())
            .try_to_vec()
            .unwrap();
        assert_eq!(bytes, legacy);

        for legacy in [LegacyStatusBytes::None, LegacyStatusBytes::Committed] {
            let b = legacy.try_to_vec().unwrap();
            let decoded = ValidatorSetUpdatesStatusBytes::deserialize(&mut &b[..]).unwrap();
            assert!(!ValidatorSetUpdatesStatus::try_from(decoded)
                .unwrap()
                .is_pending());
        }
    }
}
