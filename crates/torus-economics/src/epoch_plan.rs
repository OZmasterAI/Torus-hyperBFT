//! Epoch rotation decided at EXECUTION (consensus bug (b),
//! `docs/plans/consensus-bug-b-epoch-race.md`).
//!
//! The consensus thread never reads or writes timing-dependent staking state:
//!
//! * execution of boundary `H` applies the plan stored for `H` (key rotations +
//!   statuses) and computes the plan for the next boundary `H + L` from its own
//!   post-state ([`EpochManager::plan_rotation`]);
//! * consensus at boundary `H` only READS the plan for `H` (written by the
//!   execution of `H - L`) and turns it into hotstuff updates.
//!
//! Both rows live in `CF_CONSENSUS_META` under [`EPOCH_VSET_PREFIX`] (hashed by
//! the running state hash) and are written only by execution, inside the
//! boundary block's flush batch. Genesis records the hotstuff genesis set as
//! the installed set ([`StakingManager::record_genesis_epoch_set`]); the first
//! boundary `L` has no plan (no change), its execution plans `2L`.

use alloy_primitives::Address;
use borsh::{BorshDeserialize, BorshSerialize};
use torus_state::cf::CF_CONSENSUS_META;
use torus_state::StateBackend;
use torus_types::{PublicKey, ValidatorInfo, ValidatorSet};

use crate::epoch::EpochManager;
use crate::error::EconomicsError;
use crate::staking::StakingManager;
type Result<T> = std::result::Result<T, EconomicsError>;

/// Key prefix of every epoch-rotation row (consensus state).
pub const EPOCH_VSET_PREFIX: &[u8] = b"epoch_vset:";
const PLAN_PREFIX: &[u8] = b"epoch_vset:plan:";
const CURRENT_KEY: &[u8] = b"epoch_vset:current";

/// One validator of a planned set, exactly as installed in hotstuff.
#[derive(BorshSerialize, BorshDeserialize, Clone, Debug, PartialEq, Eq)]
pub struct PlanMember {
    pub address: [u8; 20],
    pub pubkey: [u8; 32],
    pub power: u64,
    pub commission_bps: u16,
}

/// The validator set that takes effect at `boundary`, computed by the
/// execution of the previous boundary.
#[derive(BorshSerialize, BorshDeserialize, Clone, Debug, PartialEq, Eq)]
pub struct EpochRotationPlan {
    /// Boundary height at which hotstuff and the staking rows switch.
    pub boundary: u64,
    /// The full set from `boundary` on (rank order: power desc, address asc).
    pub members: Vec<PlanMember>,
    /// Consensus keys to remove from hotstuff (sorted): every registered key
    /// (including the old key of a rotating validator) and every key of the
    /// previous set that is not a member key.
    pub deletes: Vec<[u8; 32]>,
    /// Key rotations applied to the staking rows at `boundary`.
    pub rotations: Vec<([u8; 20], [u8; 32])>,
    /// `false` = keep the installed set (no hotstuff update at `boundary`).
    pub changed: bool,
}

impl EpochRotationPlan {
    /// The members as a [`ValidatorSet`] labelled `epoch`.
    pub fn validator_set(&self, epoch: u64) -> ValidatorSet {
        members_to_set(&self.members, epoch)
    }
}

fn plan_key(boundary: u64) -> Vec<u8> {
    let mut key = PLAN_PREFIX.to_vec();
    key.extend_from_slice(&boundary.to_be_bytes());
    key
}

fn members_to_set(members: &[PlanMember], epoch: u64) -> ValidatorSet {
    ValidatorSet {
        validators: members
            .iter()
            .map(|m| ValidatorInfo {
                address: Address::from(m.address),
                pubkey: PublicKey(m.pubkey),
                power: m.power,
                commission_bps: m.commission_bps,
            })
            .collect(),
        epoch,
    }
}

fn set_to_members(set: &ValidatorSet) -> Vec<PlanMember> {
    set.validators
        .iter()
        .map(|v| PlanMember {
            address: v.address.0 .0,
            pubkey: v.pubkey.0,
            power: v.power,
            commission_bps: v.commission_bps,
        })
        .collect()
}

fn encode<B: BorshSerialize>(value: &B) -> Result<Vec<u8>> {
    borsh::to_vec(value).map_err(|e| EconomicsError::Borsh(e.to_string()))
}

fn decode<B: BorshDeserialize>(bytes: &[u8]) -> Result<B> {
    B::try_from_slice(bytes).map_err(|e| EconomicsError::Borsh(e.to_string()))
}

impl<T: StateBackend> StakingManager<T> {
    /// The plan that takes effect at `boundary`, if one was computed.
    pub fn epoch_rotation_plan(&self, boundary: u64) -> Result<Option<EpochRotationPlan>> {
        match self
            .state()
            .get_cf_raw(CF_CONSENSUS_META, &plan_key(boundary))?
        {
            Some(bytes) => Ok(Some(decode(&bytes)?)),
            None => Ok(None),
        }
    }

    fn put_epoch_rotation_plan(&self, plan: &EpochRotationPlan) -> Result<()> {
        self.state()
            .put_cf_raw(CF_CONSENSUS_META, &plan_key(plan.boundary), &encode(plan)?)?;
        Ok(())
    }

    /// The set installed in hotstuff by the last applied plan (`None` before
    /// the first one).
    pub fn current_epoch_set(&self) -> Result<Option<Vec<PlanMember>>> {
        match self.state().get_cf_raw(CF_CONSENSUS_META, CURRENT_KEY)? {
            Some(bytes) => Ok(Some(decode(&bytes)?)),
            None => Ok(None),
        }
    }

    /// [`Self::current_epoch_set`] as a [`ValidatorSet`] (epoch label 0).
    pub fn current_epoch_validator_set(&self) -> Result<Option<ValidatorSet>> {
        Ok(self.current_epoch_set()?.map(|m| members_to_set(&m, 0)))
    }

    /// Genesis: record `set` (the hotstuff genesis set) as the installed set,
    /// the base the first plan is computed against.
    pub fn record_genesis_epoch_set(&self, set: &ValidatorSet) -> Result<()> {
        self.state()
            .put_cf_raw(CF_CONSENSUS_META, CURRENT_KEY, &encode(&set_to_members(set))?)?;
        Ok(())
    }
}

impl EpochManager {
    /// Compute the plan for `target_boundary` from the CURRENT state (called by
    /// the execution of the previous boundary, after its own plan was applied).
    /// Pure function of the state: no writes.
    pub fn plan_rotation<T: StateBackend>(
        staking: &StakingManager<T>,
        max_validators: u32,
        target_boundary: u64,
        epoch_length: u64,
    ) -> Result<EpochRotationPlan> {
        let epoch = Self::epoch_for_block(target_boundary, epoch_length);

        // Due rotations (`<=`: a rotation that missed its exact epoch is applied
        // late, never dropped). Substituted in the computed set only.
        let rotations: Vec<([u8; 20], [u8; 32])> = staking
            .all_pending_rotations()?
            .into_iter()
            .filter(|r| r.effective_epoch <= epoch)
            .map(|r| (r.validator.0 .0, r.new_pubkey))
            .collect();
        let rotated = |set: &mut ValidatorSet| {
            for v in &mut set.validators {
                if let Some((_, pk)) = rotations.iter().find(|(a, _)| *a == v.address.0 .0) {
                    v.pubkey = PublicKey(*pk);
                }
            }
        };

        // No recorded set only on a DB not built from a genesis: diff against
        // nothing (install the computed set).
        let current = staking.current_epoch_set()?;
        let old_set = members_to_set(current.as_deref().unwrap_or_default(), epoch);

        let mut new_set = Self::compute_new_validator_set(staking, max_validators, epoch)?;
        rotated(&mut new_set);
        let cap = Self::safe_rotation_cap(old_set.validators.len());
        if cap > 0 {
            new_set = Self::apply_rotation_cap(&old_set, new_set, cap);
        }
        new_set = Self::enforce_minimum_floor(&old_set, new_set);
        // Re-seated / kept validators come from the old set: give them their
        // rotated key too, so hotstuff and the row agree at `target_boundary`.
        rotated(&mut new_set);

        if Self::check_minimum_set(&new_set).is_err() {
            return Ok(EpochRotationPlan {
                boundary: target_boundary,
                members: set_to_members(&old_set),
                deletes: Vec::new(),
                rotations: Vec::new(),
                changed: false,
            });
        }

        let members = set_to_members(&new_set);
        let changed = current.as_ref() != Some(&members);
        if !changed {
            return Ok(EpochRotationPlan {
                boundary: target_boundary,
                members,
                deletes: Vec::new(),
                rotations: Vec::new(),
                changed,
            });
        }
        let member_keys: std::collections::BTreeSet<[u8; 32]> =
            members.iter().map(|m| m.pubkey).collect();
        let mut deletes: std::collections::BTreeSet<[u8; 32]> = staking
            .all_validators()?
            .iter()
            .map(|v| v.pubkey)
            .chain(old_set.validators.iter().map(|v| v.pubkey.0))
            .collect();
        deletes.retain(|pk| !member_keys.contains(pk));
        Ok(EpochRotationPlan {
            boundary: target_boundary,
            members,
            deletes: deletes.into_iter().collect(),
            rotations,
            changed,
        })
    }

    /// Apply `plan` to the staking rows at its boundary: key rotations, then
    /// statuses (members Active, others Candidate; Jailed / Tombstoned
    /// untouched), record the installed set.
    pub fn apply_rotation_plan<T: StateBackend>(
        staking: &StakingManager<T>,
        plan: &EpochRotationPlan,
    ) -> Result<()> {
        for (addr, pubkey) in &plan.rotations {
            let addr = Address::from(*addr);
            if let Some(mut val) = staking.get_validator(&addr)? {
                val.pubkey = *pubkey;
                staking.put_validator(&addr, &val)?;
            }
            if staking
                .get_pending_rotation(&addr)?
                .is_some_and(|r| r.new_pubkey == *pubkey)
            {
                staking.delete_pending_rotation(&addr)?;
            }
        }
        Self::update_validator_statuses(staking, &plan.validator_set(0))?;
        staking
            .state()
            .put_cf_raw(CF_CONSENSUS_META, CURRENT_KEY, &encode(&plan.members)?)?;
        Ok(())
    }

    /// Execution of boundary `height`: apply the plan for
    /// `height` if one exists, then store the plan for `height + epoch_length`.
    /// The plan of the PREVIOUS boundary is pruned here, not the one just
    /// applied: consensus may still re-read `plan(height)` (re-validation of
    /// that boundary) until the next boundary executes.
    /// Returns the set installed at `height` (`None` = unchanged).
    pub fn execute_planned_rotation<T: StateBackend>(
        staking: &StakingManager<T>,
        max_validators: u32,
        height: u64,
        epoch_length: u64,
    ) -> Result<Option<ValidatorSet>> {
        let mut installed = None;
        let previous = plan_key(height.saturating_sub(epoch_length));
        if staking
            .state()
            .get_cf_raw(CF_CONSENSUS_META, &previous)?
            .is_some()
        {
            staking
                .state()
                .delete_cf_raw(CF_CONSENSUS_META, &previous)?;
        }
        if let Some(plan) = staking.epoch_rotation_plan(height)? {
            Self::apply_rotation_plan(staking, &plan)?;
            if plan.changed {
                installed = Some(plan.validator_set(Self::epoch_for_block(height, epoch_length)));
            }
        }
        let next = Self::plan_rotation(
            staking,
            max_validators,
            height.saturating_add(epoch_length),
            epoch_length,
        )?;
        staking.put_epoch_rotation_plan(&next)?;
        Ok(installed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ValidatorState, ValidatorStatus};
    use alloy_primitives::U256;
    use torus_state::StateDb;

    fn setup() -> (tempfile::TempDir, StakingManager) {
        let dir = tempfile::tempdir().unwrap();
        let db = StateDb::open(dir.path()).unwrap();
        (dir, StakingManager::new(db))
    }

    fn put(mgr: &StakingManager, n: u8, tokens: u64, status: ValidatorStatus) -> Address {
        let addr = Address::new([n; 20]);
        let row = ValidatorState {
            address: addr,
            pubkey: [n; 32],
            commission_bps: 500,
            self_stake: U256::from(tokens) * U256::from(10u64).pow(U256::from(18u64)),
            total_delegated: U256::ZERO,
            status,
            jailed_until: None,
            last_commission_change_block: None,
            oracle_signer: None,
        };
        mgr.put_validator(&addr, &row).unwrap();
        addr
    }

    fn keys(plan: &EpochRotationPlan) -> Vec<[u8; 32]> {
        plan.members.iter().map(|m| m.pubkey).collect()
    }

    /// The plan rows are consensus state: hashed by the running state hash.
    #[test]
    fn plan_rows_are_hashed_consensus_meta_keys() {
        use torus_state::running_hash::{hashed_cf_id, key_is_hashed, META_CONSENSUS_PREFIXES};
        assert!(META_CONSENSUS_PREFIXES.contains(&EPOCH_VSET_PREFIX));
        let meta = hashed_cf_id(CF_CONSENSUS_META).unwrap();
        assert!(key_is_hashed(meta, &plan_key(8)));
        assert!(key_is_hashed(meta, CURRENT_KEY));
    }

    /// A DB not built from a genesis has no recorded set: the plan installs
    /// the computed set.
    #[test]
    fn plan_without_a_recorded_set_installs_the_computed_set() {
        let (_d, mgr) = setup();
        for n in 1..=4 {
            put(&mgr, n, 100, ValidatorStatus::Active);
        }
        put(&mgr, 5, 50, ValidatorStatus::Candidate);
        let plan = EpochManager::plan_rotation(&mgr, 4, 8, 4).unwrap();
        assert_eq!(plan.boundary, 8);
        assert!(plan.changed, "no installed set recorded");
        assert_eq!(keys(&plan), vec![[1; 32], [2; 32], [3; 32], [4; 32]]);
        assert_eq!(plan.deletes, vec![[5; 32]], "registered non-member key");
    }

    /// The first plan is computed against the set hotstuff runs from genesis
    /// (recorded by `Genesis::initialize`), not the rows' statuses at the
    /// first boundary: nothing moved = no change (a spurious update would
    /// switch hotstuff to its 4-phase set-change protocol), and a validator
    /// jailed in epoch 0 is re-seated by the BFT floor of that 4-validator set.
    #[test]
    fn first_plan_diffs_against_the_recorded_genesis_set() {
        let (_d, mgr) = setup();
        for n in 1..=4 {
            put(&mgr, n, 100, ValidatorStatus::Active);
        }
        let genesis = EpochManager::compute_new_validator_set(&mgr, u32::MAX, 0).unwrap();
        mgr.record_genesis_epoch_set(&genesis).unwrap();
        let plan = EpochManager::plan_rotation(&mgr, 4, 8, 4).unwrap();
        assert!(!plan.changed, "nothing moved since genesis");

        let v4 = Address::new([4; 20]);
        let mut row = mgr.get_validator(&v4).unwrap().unwrap();
        row.status = ValidatorStatus::Jailed;
        mgr.put_validator(&v4, &row).unwrap();
        let plan = EpochManager::plan_rotation(&mgr, 4, 8, 4).unwrap();
        assert_eq!(keys(&plan).len(), 4, "floor of the genesis set: {plan:?}");
        assert!(!plan.changed);
    }

    #[test]
    fn plan_is_unchanged_once_installed_and_nothing_moved() {
        let (_d, mgr) = setup();
        for n in 1..=4 {
            put(&mgr, n, 100, ValidatorStatus::Active);
        }
        let first = EpochManager::execute_planned_rotation(&mgr, 4, 4, 4).unwrap();
        assert!(first.is_none(), "no plan for the first planned boundary");
        let plan8 = mgr
            .epoch_rotation_plan(8)
            .unwrap()
            .expect("plan for 8 stored");
        let installed = EpochManager::execute_planned_rotation(&mgr, 4, 8, 4).unwrap();
        assert!(installed.is_some(), "bootstrap plan installs");
        assert_eq!(
            mgr.current_epoch_set().unwrap(),
            Some(plan8.members.clone())
        );
        assert_eq!(
            mgr.current_epoch_validator_set()
                .unwrap()
                .map(|s| s.validators.len()),
            Some(4)
        );
        let plan12 = mgr.epoch_rotation_plan(12).unwrap().unwrap();
        assert!(!plan12.changed);
        assert!(plan12.deletes.is_empty() && plan12.rotations.is_empty());
        assert_eq!(
            mgr.epoch_rotation_plan(8).unwrap(),
            Some(plan8),
            "kept until 12 executes"
        );
        EpochManager::execute_planned_rotation(&mgr, 4, 12, 4).unwrap();
        assert_eq!(
            mgr.epoch_rotation_plan(8).unwrap(),
            None,
            "pruned one boundary later"
        );
    }

    #[test]
    fn due_rotation_is_planned_and_applied_at_the_boundary() {
        let (_d, mgr) = setup();
        for n in 1..=4 {
            put(&mgr, n, 100, ValidatorStatus::Active);
        }
        let v1 = Address::new([1; 20]);
        // Submitted in epoch 1 (blocks 4..8), effective epoch 2 = boundary 8.
        mgr.submit_key_rotation(v1, [0x99; 32], 1, 5).unwrap();
        let plan = EpochManager::plan_rotation(&mgr, 4, 8, 4).unwrap();
        assert!(keys(&plan).contains(&[0x99; 32]), "rotated key installed");
        assert!(
            plan.deletes.contains(&[1; 32]),
            "old key removed from hotstuff"
        );
        assert_eq!(plan.rotations, vec![([1; 20], [0x99; 32])]);
        // Planning writes nothing.
        assert_eq!(mgr.get_validator(&v1).unwrap().unwrap().pubkey, [1; 32]);
        assert!(mgr.get_pending_rotation(&v1).unwrap().is_some());

        EpochManager::apply_rotation_plan(&mgr, &plan).unwrap();
        assert_eq!(mgr.get_validator(&v1).unwrap().unwrap().pubkey, [0x99; 32]);
        assert!(mgr.get_pending_rotation(&v1).unwrap().is_none());
    }

    #[test]
    fn late_rotation_is_applied_one_boundary_later_not_dropped() {
        let (_d, mgr) = setup();
        for n in 1..=4 {
            put(&mgr, n, 100, ValidatorStatus::Active);
        }
        let v2 = Address::new([2; 20]);
        // Effective epoch 1 (boundary 4) but planned only at boundary 4 for 8.
        mgr.submit_key_rotation(v2, [0x77; 32], 0, 3).unwrap();
        let plan = EpochManager::plan_rotation(&mgr, 4, 8, 4).unwrap();
        assert_eq!(plan.rotations, vec![([2; 20], [0x77; 32])]);
    }

    #[test]
    fn statuses_follow_the_plan_and_jailed_rows_are_untouched() {
        let (_d, mgr) = setup();
        for n in 1..=6 {
            put(&mgr, n, 100, ValidatorStatus::Active);
        }
        let strong = put(&mgr, 7, 500, ValidatorStatus::Candidate);
        let jailed = put(&mgr, 8, 900, ValidatorStatus::Jailed);
        // 7 eligible for 6 seats (cap 2 changes): the weakest (address 6) leaves.
        let plan = EpochManager::plan_rotation(&mgr, 6, 8, 4).unwrap();
        assert!(plan.members.iter().any(|m| m.address == strong.0 .0));
        assert!(!plan.members.iter().any(|m| m.address == jailed.0 .0));
        EpochManager::apply_rotation_plan(&mgr, &plan).unwrap();
        let status = |a: &Address| mgr.get_validator(a).unwrap().unwrap().status;
        assert_eq!(status(&strong), ValidatorStatus::Active);
        assert_eq!(status(&Address::new([6; 20])), ValidatorStatus::Candidate);
        assert_eq!(status(&jailed), ValidatorStatus::Jailed);
    }
}
