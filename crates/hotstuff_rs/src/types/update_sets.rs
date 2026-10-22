//! Types that store updates to the App-mutable states.

use std::{
    collections::{hash_map, hash_set, HashMap, HashSet},
    hash::Hash,
};

use borsh::{BorshDeserialize, BorshSerialize};

use super::{
    crypto_primitives::{SignatureError, VerifyingKey},
    data_types::Power,
};

/// Generic set of key-value updates that are committed when a particular block is committed.
///
/// This generic type currently forms the basis of [`AppStateUpdates`] and of the power part of
/// [`ValidatorSetUpdates`].
///
/// # Uniqueness of Key between `inserts` and `deletes`
#[derive(Clone, BorshSerialize, BorshDeserialize)]
pub struct UpdateSet<K: Eq + Hash, V: Eq + Hash> {
    /// Insertion updates that will be committed when a `Block` is committed.
    inserts: HashMap<K, V>,

    /// Deletion updates that will be committed when a `Block` is committed.
    deletes: HashSet<K>,
}

impl<K: Eq + Hash, V: Eq + Hash> Default for UpdateSet<K, V>
where
    K:,
{
    fn default() -> Self {
        Self::new()
    }
}

impl<K: Eq + Hash, V: Eq + Hash> UpdateSet<K, V>
where
    K:,
{
    /// Create a new `UpdateSet` with empty `inserts` and `deletes`.
    pub fn new() -> Self {
        Self {
            inserts: HashMap::new(),
            deletes: HashSet::new(),
        }
    }

    /// Schedule the insertion of a `key`-`value` pair when the block that corresponds to this `UpdateSet`
    /// gets committed.
    ///
    /// This cancels the deletion of `key`, if it has been scheduled using [`delete`](Self::delete).
    pub fn insert(&mut self, key: K, value: V) {
        self.deletes.remove(&key);
        self.inserts.insert(key, value);
    }

    /// Schedule the deletion of `key` when the block that corresponds to this `UpdateSet` gets committed.
    ///
    /// This cancels the insertion of `key`, if it has been scheduled using [`insert`](Self::insert).
    pub fn delete(&mut self, key: K) {
        self.inserts.remove(&key);
        self.deletes.insert(key);
    }

    /// Get whether the `UpdateSet` is scheduled to insert a value to `key` when the block that corresponds
    /// to this `UpdateSet` gets committed, and if so, returns a reference to that value.
    pub fn get_insert(&self, key: &K) -> Option<&V> {
        self.inserts.get(key)
    }

    /// Check whether the `UpdateSet` is scheduled to delete `key` when the block that corresponds to this
    /// `UpdateSet` gets committed.
    pub fn contains_delete(&self, key: &K) -> bool {
        self.deletes.contains(key)
    }

    /// Get an iterator over all of the key-value pairs that this `UpdateSet` will insert.
    pub fn inserts(&self) -> hash_map::Iter<'_, K, V> {
        self.inserts.iter()
    }

    /// Get an iterator over all of the keys that this `UpdateSet` will delete.
    pub fn deletes(&self) -> hash_set::Iter<'_, K> {
        self.deletes.iter()
    }
}

/// Set of key-value updates committed to the App State when a block is committed.
pub type AppStateUpdates = UpdateSet<Vec<u8>, Vec<u8>>;

/// Set of updates to the validator set that are applied when a block is committed.
///
/// Two parts:
/// - **power updates**: validators inserted (with their new power) or deleted, as before;
/// - **leader exclusion delta** (dead-leader skip, docs/plans/dead-leader-skip.md): validators to add
///   to ([`exclude_leader`](Self::exclude_leader)) or remove from
///   ([`readmit_leader`](Self::readmit_leader)) the validator set's
///   [`leader_excluded`](super::validator_set::ValidatorSet::leader_excluded) set. An excluded
///   validator keeps its voting power; it only stops being selected as leader.
///
/// An update with only an exclusion delta is still a validator-set update: it goes through the same
/// Prepare/Precommit/Commit/Decide phases, so `leader(view)` changes only at a validator-set update.
#[derive(Clone, Default)]
pub struct ValidatorSetUpdates {
    power_updates: UpdateSet<VerifyingKey, Power>,
    leader_excludes: HashSet<VerifyingKey>,
    leader_readmits: HashSet<VerifyingKey>,
}

impl ValidatorSetUpdates {
    /// Create a new `ValidatorSetUpdates` with no power updates and an empty exclusion delta.
    pub fn new() -> Self {
        Self::default()
    }

    /// Schedule the insertion of `validator` with `power` (or a change of its power).
    ///
    /// This cancels the deletion of `validator`, if it has been scheduled using [`delete`](Self::delete).
    pub fn insert(&mut self, validator: VerifyingKey, power: Power) {
        self.power_updates.insert(validator, power)
    }

    /// Schedule the deletion of `validator`.
    ///
    /// This cancels the insertion of `validator`, if it has been scheduled using [`insert`](Self::insert).
    pub fn delete(&mut self, validator: VerifyingKey) {
        self.power_updates.delete(validator)
    }

    /// Get the power this update will give `validator`, if it inserts it.
    pub fn get_insert(&self, validator: &VerifyingKey) -> Option<&Power> {
        self.power_updates.get_insert(validator)
    }

    /// Check whether this update deletes `validator`.
    pub fn contains_delete(&self, validator: &VerifyingKey) -> bool {
        self.power_updates.contains_delete(validator)
    }

    /// Iterator over the validators this update inserts, with their new powers.
    pub fn inserts(&self) -> hash_map::Iter<'_, VerifyingKey, Power> {
        self.power_updates.inserts()
    }

    /// Iterator over the validators this update deletes.
    pub fn deletes(&self) -> hash_set::Iter<'_, VerifyingKey> {
        self.power_updates.deletes()
    }

    /// Schedule `validator` to be added to the leader-excluded set.
    ///
    /// This cancels its re-admission, if it has been scheduled using [`readmit_leader`](Self::readmit_leader).
    pub fn exclude_leader(&mut self, validator: VerifyingKey) {
        self.leader_readmits.remove(&validator);
        self.leader_excludes.insert(validator);
    }

    /// Schedule `validator` to be removed from the leader-excluded set.
    ///
    /// This cancels its exclusion, if it has been scheduled using [`exclude_leader`](Self::exclude_leader).
    pub fn readmit_leader(&mut self, validator: VerifyingKey) {
        self.leader_excludes.remove(&validator);
        self.leader_readmits.insert(validator);
    }

    /// Iterator over the validators this update adds to the leader-excluded set.
    pub fn leader_excludes(&self) -> hash_set::Iter<'_, VerifyingKey> {
        self.leader_excludes.iter()
    }

    /// Iterator over the validators this update removes from the leader-excluded set.
    pub fn leader_readmits(&self) -> hash_set::Iter<'_, VerifyingKey> {
        self.leader_readmits.iter()
    }

    /// Whether this update carries a non-empty leader exclusion delta.
    pub fn has_leader_exclusion_updates(&self) -> bool {
        !self.leader_excludes.is_empty() || !self.leader_readmits.is_empty()
    }

    /// The power part in its serializable form (unchanged format).
    pub(crate) fn power_updates_bytes(&self) -> ValidatorSetUpdatesBytes {
        ValidatorSetUpdatesBytes {
            inserts: self.inserts().map(|(k, v)| (k.to_bytes(), *v)).collect(),
            deletes: self.deletes().map(|k| k.to_bytes()).collect(),
        }
    }

    /// The exclusion delta in its serializable form (keys sorted ascending).
    pub(crate) fn leader_exclusion_updates_bytes(&self) -> LeaderExclusionUpdatesBytes {
        let sorted = |set: &HashSet<VerifyingKey>| {
            let mut keys: Vec<VerifyingKeyBytes> = set.iter().map(|k| k.to_bytes()).collect();
            keys.sort_unstable();
            keys
        };
        LeaderExclusionUpdatesBytes {
            excludes: sorted(&self.leader_excludes),
            readmits: sorted(&self.leader_readmits),
        }
    }

    /// Rebuild from the serialized power part and (optional) exclusion delta.
    pub(crate) fn try_from_bytes(
        power_updates: ValidatorSetUpdatesBytes,
        leader_exclusions: Option<LeaderExclusionUpdatesBytes>,
    ) -> Result<Self, SignatureError> {
        let mut updates = ValidatorSetUpdates {
            power_updates: UpdateSet {
                inserts: power_updates
                    .inserts()
                    .map(|(vk_bytes, &power)| {
                        VerifyingKey::from_bytes(vk_bytes).map(|vk| (vk, power))
                    })
                    .collect::<Result<HashMap<VerifyingKey, Power>, SignatureError>>()?,
                deletes: power_updates
                    .deletes()
                    .map(VerifyingKey::from_bytes)
                    .collect::<Result<HashSet<VerifyingKey>, SignatureError>>()?,
            },
            leader_excludes: HashSet::new(),
            leader_readmits: HashSet::new(),
        };
        if let Some(delta) = leader_exclusions {
            for k in &delta.excludes {
                updates.leader_excludes.insert(VerifyingKey::from_bytes(k)?);
            }
            for k in &delta.readmits {
                updates.leader_readmits.insert(VerifyingKey::from_bytes(k)?);
            }
        }
        Ok(updates)
    }
}

/// Serializable form of the power part of [ValidatorSetUpdates] (format unchanged from before the
/// leader exclusion delta).
///
/// ## Rationale
///
/// See the [related section](super::validator_set::ValidatorSetBytes#rationale) about `ValidatorSetBytes`.
pub type ValidatorSetUpdatesBytes = UpdateSet<VerifyingKeyBytes, Power>;

/// Serializable form of the leader exclusion delta of [ValidatorSetUpdates]: keys sorted ascending.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct LeaderExclusionUpdatesBytes {
    pub(crate) excludes: Vec<VerifyingKeyBytes>,
    pub(crate) readmits: Vec<VerifyingKeyBytes>,
}

/// Internal type used for serializing and deserializing values of type [`VerifyingKey`].
pub type VerifyingKeyBytes = [u8; 32];
