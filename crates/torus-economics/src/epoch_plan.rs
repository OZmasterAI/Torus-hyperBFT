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
use torus_state::cf::{CF_CONSENSUS_META, CF_FEE_CONFIG};
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

/// Item 7 step 2 (F3): `CF_CONSENSUS_META` row of the gas the EVM blocks since the last boundary
/// used: small lane ‖ big lane (u64 BE each). Hashed (`running_hash::META_CONSENSUS_PREFIXES`).
pub const EVM_EPOCH_GAS_KEY: &[u8] = b"evm_epoch_gas";

/// Item 7 step 2 (F3): the epoch's EVM gas so far, `(small, big)`; no row = `(0, 0)`.
pub fn epoch_evm_gas<T: StateBackend>(state: &T) -> Result<(u64, u64)> {
    let row = state.get_cf_raw(CF_CONSENSUS_META, EVM_EPOCH_GAS_KEY)?;
    Ok(match row {
        Some(v) if v.len() == 16 => (
            u64::from_be_bytes(v[..8].try_into().expect("8 bytes")),
            u64::from_be_bytes(v[8..].try_into().expect("8 bytes")),
        ),
        _ => (0, 0),
    })
}

/// Item 7 step 2 (F3): add one block's EVM gas to the epoch row.
pub fn add_epoch_evm_gas<T: StateBackend>(state: &T, gas: u64, big_gas: u64) -> Result<()> {
    let (small, big) = epoch_evm_gas(state)?;
    let mut v = [0u8; 16];
    v[..8].copy_from_slice(&small.saturating_add(gas).to_be_bytes());
    v[8..].copy_from_slice(&big.saturating_add(big_gas).to_be_bytes());
    state.put_cf_raw(CF_CONSENSUS_META, EVM_EPOCH_GAS_KEY, &v)?;
    Ok(())
}

/// Item 7 step 2 (F3): the base fees after the epoch `[from, to)` ran with `window` (its header
/// values) and used `(gas, big_gas)`: EIP-1559 with the epoch as one block. A lane's gas limit is
/// its limit times its EVM heights in the window (small lane: every EVM height, big ones included;
/// big lane: big heights), so the target is half of it; at most +/-12.5% per epoch, never below
/// `EVM_MIN_BASE_FEE`.
pub fn next_base_fees(
    window: &torus_types::EvmHeaderValues,
    from: u64,
    to: u64,
    gas: u64,
    big_gas: u64,
) -> (u64, u64) {
    let (mut evm_heights, mut big_heights) = (0u64, 0u64);
    for h in from.max(1)..to {
        if torus_types::is_evm_height(h, window.block_interval, window.big_block_interval) {
            evm_heights += 1;
        }
        if torus_types::is_big_evm_height(h, window.big_block_interval) {
            big_heights += 1;
        }
    }
    let next = |used: u64, heights: u64, limit: u64, fee: u64| {
        torus_types::eip1559::calc_next_block_base_fee(used, heights.saturating_mul(limit), fee)
            .max(torus_types::EVM_MIN_BASE_FEE)
    };
    (
        next(gas, evm_heights, window.gas_limit, window.base_fee),
        next(
            big_gas,
            big_heights,
            window.big_gas_limit,
            window.big_base_fee,
        ),
    )
}

/// One validator of a planned set, exactly as installed in hotstuff.
#[derive(BorshSerialize, BorshDeserialize, Clone, Debug, PartialEq, Eq)]
pub struct PlanMember {
    pub address: [u8; 20],
    pub pubkey: [u8; 32],
    pub power: u64,
    pub commission_bps: u16,
}

/// Item 7 steps 1-2: EVM header values from `boundary` on (`None` = keep the parent's).
#[derive(BorshSerialize, BorshDeserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PlanEvm {
    pub gas_limit: Option<u64>,
    pub block_interval: Option<u64>,
    /// Item 7 step 2: M and the big lane's gas limit (governance), and both base fees (F3,
    /// set by the previous boundary's execution, B9).
    pub big_block_interval: Option<u64>,
    pub big_gas_limit: Option<u64>,
    pub base_fee: Option<u64>,
    pub big_base_fee: Option<u64>,
}

impl PlanEvm {
    /// The header values from this boundary on: each set value, else the parent's.
    pub fn resolve(&self, parent: &torus_types::TorusBlockHeader) -> torus_types::EvmHeaderValues {
        torus_types::EvmHeaderValues {
            gas_limit: self.gas_limit.unwrap_or(parent.evm_gas_limit),
            block_interval: self.block_interval.unwrap_or(parent.evm_block_interval),
            big_block_interval: self
                .big_block_interval
                .unwrap_or(parent.evm_big_block_interval),
            big_gas_limit: self.big_gas_limit.unwrap_or(parent.evm_big_gas_limit),
            base_fee: self.base_fee.unwrap_or(parent.base_fee_per_gas),
            big_base_fee: self.big_base_fee.unwrap_or(parent.big_base_fee_per_gas),
        }
    }
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
    /// Item 7 step 1: the governance EVM values (header `evm_gas_limit`,
    /// `evm_block_interval`) from `boundary` on, set by
    /// [`EpochManager::execute_planned_rotation`].
    pub evm: PlanEvm,
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

/// s80 epoch-plan-logical-ready: the `CF_CONSENSUS_META` key of the plan for
/// `boundary` (execution reads its own row back to hand it to consensus).
pub fn epoch_plan_key(boundary: u64) -> Vec<u8> {
    plan_key(boundary)
}

/// s80 epoch-plan-logical-ready: decode a plan row's bytes exactly as
/// [`StakingManager::epoch_rotation_plan`] does.
pub fn decode_epoch_rotation_plan(bytes: &[u8]) -> Result<EpochRotationPlan> {
    decode(bytes)
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
                evm: PlanEvm::default(),
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
                evm: PlanEvm::default(),
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
            evm: PlanEvm::default(),
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
    /// `height` if one exists, then store the plan for `height + epoch_length`
    /// (item 7 step 1: with the governance EVM values of this post-state).
    /// The plan of the PREVIOUS boundary is pruned here, not the one just
    /// applied: consensus may still re-read `plan(height)` (re-validation of
    /// that boundary) until the next boundary executes.
    /// Item 7 step 2 (F3): with `fee_window` (the closing epoch's EVM header values, i.e. the
    /// boundary's parent header) the epoch's EVM gas row sets plan(`height + epoch_length`)'s
    /// two base fees; the row is deleted either way.
    /// Returns the set installed at `height` (`None` = unchanged).
    pub fn execute_planned_rotation<T: StateBackend>(
        staking: &StakingManager<T>,
        max_validators: u32,
        height: u64,
        epoch_length: u64,
        fee_window: Option<torus_types::EvmHeaderValues>,
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
        let mut next = Self::plan_rotation(
            staking,
            max_validators,
            height.saturating_add(epoch_length),
            epoch_length,
        )?;
        let gov = |key: &str, range: &std::ops::RangeInclusive<u64>| -> Result<Option<u64>> {
            let stored = staking.state().get_cf_raw(CF_FEE_CONFIG, key.as_bytes())?;
            Ok(torus_types::gov_u64_in(stored.as_deref(), range))
        };
        next.evm = PlanEvm {
            gas_limit: gov(
                torus_types::EVM_BLOCK_GAS_LIMIT_PARAM,
                &torus_types::EVM_BLOCK_GAS_LIMIT_RANGE,
            )?,
            block_interval: gov(
                torus_types::EVM_BLOCK_INTERVAL_PARAM,
                &torus_types::EVM_BLOCK_INTERVAL_RANGE,
            )?,
            // Item 7 step 2: M is 0 (off) or in its range, so the range check is a predicate.
            big_block_interval: gov(torus_types::EVM_BIG_BLOCK_INTERVAL_PARAM, &(0..=u64::MAX))?
                .filter(|&m| torus_types::evm_big_block_interval_ok(m)),
            big_gas_limit: gov(
                torus_types::EVM_BIG_BLOCK_GAS_LIMIT_PARAM,
                &torus_types::EVM_BIG_BLOCK_GAS_LIMIT_RANGE,
            )?,
            base_fee: None,
            big_base_fee: None,
        };
        // Item 7 step 2 (F3): the epoch [height - L, height) sets plan(height + L)'s fees; its row
        // starts over (this block's own EVM gas is added after, into the next epoch).
        let (gas, big_gas) = epoch_evm_gas(staking.state())?;
        if let Some(window) = fee_window {
            let (fee, big_fee) = next_base_fees(
                &window,
                height.saturating_sub(epoch_length),
                height,
                gas,
                big_gas,
            );
            next.evm.base_fee = Some(fee);
            next.evm.big_base_fee = Some(big_fee);
        }
        if (gas, big_gas) != (0, 0) {
            staking
                .state()
                .delete_cf_raw(CF_CONSENSUS_META, EVM_EPOCH_GAS_KEY)?;
        }
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

    /// s80: the public key / decode accessors read the row exactly as
    /// `epoch_rotation_plan` does.
    #[test]
    fn s80_plan_key_and_decode_match_epoch_rotation_plan() {
        let (_d, mgr) = setup();
        for n in 1..=4 {
            put(&mgr, n, 100, ValidatorStatus::Active);
        }
        EpochManager::execute_planned_rotation(&mgr, 4, 4, 4, None).unwrap();
        assert_eq!(epoch_plan_key(8), plan_key(8));
        let bytes = mgr
            .state()
            .get_cf_raw(CF_CONSENSUS_META, &epoch_plan_key(8))
            .unwrap()
            .expect("plan row for 8");
        assert_eq!(
            Some(decode_epoch_rotation_plan(&bytes).unwrap()),
            mgr.epoch_rotation_plan(8).unwrap()
        );
        assert!(decode_epoch_rotation_plan(&bytes[..bytes.len() - 1]).is_err());
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
        let first = EpochManager::execute_planned_rotation(&mgr, 4, 4, 4, None).unwrap();
        assert!(first.is_none(), "no plan for the first planned boundary");
        let plan8 = mgr
            .epoch_rotation_plan(8)
            .unwrap()
            .expect("plan for 8 stored");
        let installed = EpochManager::execute_planned_rotation(&mgr, 4, 8, 4, None).unwrap();
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
        EpochManager::execute_planned_rotation(&mgr, 4, 12, 4, None).unwrap();
        assert_eq!(
            mgr.epoch_rotation_plan(8).unwrap(),
            None,
            "pruned one boundary later"
        );
    }

    /// Write the two EVM governance rows (`CF_FEE_CONFIG`, decimal).
    fn put_evm_params(mgr: &StakingManager, gas_limit: &[u8], block_interval: &[u8]) {
        let state = mgr.state();
        state
            .put_cf_raw(
                CF_FEE_CONFIG,
                torus_types::EVM_BLOCK_GAS_LIMIT_PARAM.as_bytes(),
                gas_limit,
            )
            .unwrap();
        state
            .put_cf_raw(
                CF_FEE_CONFIG,
                torus_types::EVM_BLOCK_INTERVAL_PARAM.as_bytes(),
                block_interval,
            )
            .unwrap();
    }

    /// Item 7 step 1: execution of boundary H copies the governance EVM rows
    /// into plan(H + L). They do not touch the validator set. RED before: the
    /// plan had no EVM values.
    #[test]
    fn plan_carries_governance_evm_params() {
        let (_d, mgr) = setup();
        for n in 1..=4 {
            put(&mgr, n, 100, ValidatorStatus::Active);
        }
        let genesis = EpochManager::compute_new_validator_set(&mgr, u32::MAX, 0).unwrap();
        mgr.record_genesis_epoch_set(&genesis).unwrap();
        put_evm_params(&mgr, b"5000000", b"10");
        EpochManager::execute_planned_rotation(&mgr, 4, 4, 4, None).unwrap();
        let plan8 = mgr
            .epoch_rotation_plan(8)
            .unwrap()
            .expect("plan for 8 stored");
        assert_eq!(
            plan8.evm,
            PlanEvm {
                gas_limit: Some(5_000_000),
                block_interval: Some(10),
                ..PlanEvm::default()
            }
        );
        assert!(!plan8.changed, "EVM values leave the validator set alone");
    }

    /// Item 7 step 2 (F3): the epoch counts as one EIP-1559 block per lane: target = half the
    /// lane's gas over its EVM heights in the window, +/-12.5% at most, never below 1 gwei.
    #[test]
    fn next_base_fees_follow_the_epochs_gas_per_lane() {
        use torus_types::EVM_MIN_BASE_FEE;
        let w = torus_types::EvmHeaderValues {
            gas_limit: 3_000_000,
            block_interval: 5,
            big_block_interval: 50,
            big_gas_limit: 10_000_000,
            base_fee: 2_000_000_000,
            big_base_fee: 2_000_000_000,
        };
        // [100, 200): 20 small-cadence heights (100 and 150 are big too), 2 big heights.
        // Targets: 20 x 3M / 2 = 30M (small), 2 x 10M / 2 = 10M (big).
        assert_eq!(
            next_base_fees(&w, 100, 200, 30_000_000, 10_000_000),
            (2_000_000_000, 2_000_000_000)
        );
        assert_eq!(
            next_base_fees(&w, 100, 200, 60_000_000, 20_000_000),
            (2_250_000_000, 2_250_000_000)
        );
        assert_eq!(
            next_base_fees(&w, 100, 200, 0, 0),
            (1_750_000_000, 1_750_000_000)
        );
        let low = torus_types::EvmHeaderValues {
            base_fee: 1_000_000_000,
            big_base_fee: 1_000_000_000,
            ..w
        };
        assert_eq!(
            next_base_fees(&low, 100, 200, 0, 0),
            (EVM_MIN_BASE_FEE, EVM_MIN_BASE_FEE),
            "floor"
        );
        let off = torus_types::EvmHeaderValues {
            big_block_interval: 0,
            ..w
        };
        assert_eq!(
            next_base_fees(&off, 100, 200, 30_000_000, 5).1,
            2_000_000_000,
            "no big height: unchanged"
        );
        // M = 7, N = 5 over [1, 15): EVM heights 5, 7, 10, 14 (4), big 7, 14 (2).
        let mixed = torus_types::EvmHeaderValues {
            big_block_interval: 7,
            ..w
        };
        assert_eq!(
            next_base_fees(&mixed, 1, 15, 6_000_000, 10_000_000),
            (2_000_000_000, 2_000_000_000)
        );
        // Gas over a lane's window limit (60M small, 20M big) counts as the limit: exactly +12.5%
        // (unclamped: 2,500,000,000 and 2,375,000,000).
        assert_eq!(
            next_base_fees(&w, 100, 200, 90_000_000, 25_000_000),
            (2_250_000_000, 2_250_000_000)
        );
        assert_eq!(
            next_base_fees(&w, 100, 200, u64::MAX, u64::MAX),
            (2_250_000_000, 2_250_000_000)
        );
        let top = torus_types::EvmHeaderValues {
            base_fee: u64::MAX - 1,
            big_base_fee: u64::MAX - 1,
            ..w
        };
        assert_eq!(
            next_base_fees(&top, 100, 200, 60_000_000, 20_000_000),
            (u64::MAX, u64::MAX),
            "saturates"
        );
    }

    /// Item 7 step 2 (F3): execution of boundary H turns the epoch's EVM gas row into
    /// plan(H + L)'s two base fees (with the closing epoch's header values) and deletes the row;
    /// without a window the fees stay `None` and the row is still reset. The row is hashed
    /// consensus state.
    #[test]
    fn boundary_turns_the_epoch_gas_into_plan_fees_and_resets_the_row() {
        use torus_state::running_hash::{hashed_cf_id, key_is_hashed};
        let meta = hashed_cf_id(CF_CONSENSUS_META).unwrap();
        assert!(key_is_hashed(meta, EVM_EPOCH_GAS_KEY));
        let window = torus_types::EvmHeaderValues {
            gas_limit: 3_000_000,
            block_interval: 1,
            big_block_interval: 0,
            big_gas_limit: 10_000_000,
            base_fee: 2_000_000_000,
            big_base_fee: 2_000_000_000,
        };
        for fee_window in [Some(window), None] {
            let (_d, mgr) = setup();
            for n in 1..=4 {
                put(&mgr, n, 100, ValidatorStatus::Active);
            }
            let genesis = EpochManager::compute_new_validator_set(&mgr, u32::MAX, 0).unwrap();
            mgr.record_genesis_epoch_set(&genesis).unwrap();
            add_epoch_evm_gas(mgr.state(), 9_000_000, 0).unwrap();
            assert_eq!(epoch_evm_gas(mgr.state()).unwrap(), (9_000_000, 0));
            EpochManager::execute_planned_rotation(&mgr, 4, 4, 4, fee_window).unwrap();
            let plan8 = mgr
                .epoch_rotation_plan(8)
                .unwrap()
                .expect("plan for 8 stored");
            if fee_window.is_some() {
                // [0, 4) at N = 1: 3 EVM heights, target 4.5M, used 2x: +12.5%. M = 0: unchanged.
                assert_eq!(plan8.evm.base_fee, Some(2_250_000_000));
                assert_eq!(plan8.evm.big_base_fee, Some(2_000_000_000));
            } else {
                assert_eq!((plan8.evm.base_fee, plan8.evm.big_base_fee), (None, None));
            }
            assert_eq!(
                epoch_evm_gas(mgr.state()).unwrap(),
                (0, 0),
                "{fee_window:?}"
            );
            assert_eq!(
                mgr.state()
                    .get_cf_raw(CF_CONSENSUS_META, EVM_EPOCH_GAS_KEY)
                    .unwrap(),
                None,
                "row deleted"
            );
        }
    }

    /// No row, or a row outside its governance range (1M..=10M, 2..=100) or
    /// not a number: the plan keeps the chain's values (`None`).
    #[test]
    fn plan_without_or_with_invalid_evm_params_keeps_the_chain_values() {
        let stored = [
            None,
            Some("999999"),
            Some("10000001"),
            Some("1"),
            Some("101"),
            Some("oops"),
        ];
        for stored in stored {
            let (_d, mgr) = setup();
            for n in 1..=4 {
                put(&mgr, n, 100, ValidatorStatus::Active);
            }
            if let Some(v) = stored {
                put_evm_params(&mgr, v.as_bytes(), v.as_bytes());
            }
            EpochManager::execute_planned_rotation(&mgr, 4, 4, 4, None).unwrap();
            let plan8 = mgr
                .epoch_rotation_plan(8)
                .unwrap()
                .expect("plan for 8 stored");
            assert_eq!(plan8.evm, PlanEvm::default(), "{stored:?}");
        }
    }

    /// Write the two big-lane governance rows (`CF_FEE_CONFIG`, decimal).
    fn put_big_params(mgr: &StakingManager, big_block_interval: &[u8], big_gas_limit: &[u8]) {
        let state = mgr.state();
        state
            .put_cf_raw(
                CF_FEE_CONFIG,
                torus_types::EVM_BIG_BLOCK_INTERVAL_PARAM.as_bytes(),
                big_block_interval,
            )
            .unwrap();
        state
            .put_cf_raw(
                CF_FEE_CONFIG,
                torus_types::EVM_BIG_BLOCK_GAS_LIMIT_PARAM.as_bytes(),
                big_gas_limit,
            )
            .unwrap();
    }

    /// Execute boundary 4 with both big-lane rows set (any may be unparsable)
    /// and return plan(8)'s EVM values.
    fn plan8_evm_with_big_params(big_block_interval: &str, big_gas_limit: &str) -> PlanEvm {
        let (_d, mgr) = setup();
        for n in 1..=4 {
            put(&mgr, n, 100, ValidatorStatus::Active);
        }
        put_big_params(
            &mgr,
            big_block_interval.as_bytes(),
            big_gas_limit.as_bytes(),
        );
        EpochManager::execute_planned_rotation(&mgr, 4, 4, 4, None).unwrap();
        mgr.epoch_rotation_plan(8)
            .unwrap()
            .expect("plan for 8 stored")
            .evm
    }

    /// Item 7 step 2: execution of boundary H copies the governance big-lane
    /// rows (M, big gas limit) into plan(H + L). RED before: the plan had no
    /// big-lane values.
    #[test]
    fn plan_carries_governance_big_lane_params() {
        let (_d, mgr) = setup();
        for n in 1..=4 {
            put(&mgr, n, 100, ValidatorStatus::Active);
        }
        let genesis = EpochManager::compute_new_validator_set(&mgr, u32::MAX, 0).unwrap();
        mgr.record_genesis_epoch_set(&genesis).unwrap();
        put_big_params(&mgr, b"190", b"15000000");
        EpochManager::execute_planned_rotation(&mgr, 4, 4, 4, None).unwrap();
        let plan8 = mgr
            .epoch_rotation_plan(8)
            .unwrap()
            .expect("plan for 8 stored");
        assert_eq!(plan8.evm.big_block_interval, Some(190));
        assert_eq!(plan8.evm.big_gas_limit, Some(15_000_000));
        assert_eq!(plan8.evm.gas_limit, None);
        assert_eq!(plan8.evm.block_interval, None);
        assert_eq!(plan8.evm.base_fee, None, "F3 (B9) sets the fees");
        assert_eq!(plan8.evm.big_base_fee, None, "F3 (B9) sets the fees");
        assert!(!plan8.changed, "EVM values leave the validator set alone");
    }

    /// M: 0 switches big blocks off (a value); 1..=29, above 10,000 or not a
    /// number keep the chain's M. Big limit: outside 1M..=30M or not a number
    /// keeps the chain's limit.
    #[test]
    fn plan_big_lane_values_outside_their_range_keep_the_chain_values() {
        let m_cases = [
            ("0", Some(0)),
            ("30", Some(30)),
            ("10000", Some(10_000)),
            ("1", None),
            ("29", None),
            ("10001", None),
            ("x", None),
        ];
        for (stored, want) in m_cases {
            let evm = plan8_evm_with_big_params(stored, "x");
            assert_eq!(evm.big_block_interval, want, "M {stored:?}");
            assert_eq!(evm.big_gas_limit, None, "M {stored:?}");
        }
        let limit_cases = [
            ("999999", None),
            ("30000001", None),
            ("x", None),
            ("1000000", Some(1_000_000)),
            ("30000000", Some(30_000_000)),
        ];
        for (stored, want) in limit_cases {
            let evm = plan8_evm_with_big_params("x", stored);
            assert_eq!(evm.big_gas_limit, want, "big limit {stored:?}");
            assert_eq!(evm.big_block_interval, None, "big limit {stored:?}");
        }
    }

    /// `PlanEvm::resolve` takes each set value, else the parent's.
    #[test]
    fn plan_resolve_takes_set_values_else_the_parents() {
        let parent = torus_types::TorusBlockHeader {
            height: 7,
            parent_hash: alloy_primitives::B256::ZERO,
            timestamp: 0,
            proposer: Address::ZERO,
            state_root: alloy_primitives::B256::ZERO,
            receipts_root: alloy_primitives::B256::ZERO,
            logs_bloom: alloy_primitives::Bloom::ZERO,
            evm_gas_used: 0,
            evm_fee_revenue: 0,
            evm_gas_limit: 3_000_000,
            evm_block_interval: 11,
            evm_big_block_interval: 50,
            evm_big_gas_limit: 10_000_000,
            native_action_count: 0,
            evm_tx_count: 0,
            base_fee_per_gas: 1_000_000_000,
            big_base_fee_per_gas: 1_500_000_000,
            epoch: 0,
            validator_set_hash: alloy_primitives::B256::ZERO,
            sig_attestation: [0u8; 64],
        };
        let plan = PlanEvm {
            big_block_interval: Some(190),
            base_fee: Some(2_000_000_000),
            ..Default::default()
        };
        assert_eq!(
            plan.resolve(&parent),
            torus_types::EvmHeaderValues {
                gas_limit: 3_000_000,
                block_interval: 11,
                big_block_interval: 190,
                big_gas_limit: 10_000_000,
                base_fee: 2_000_000_000,
                big_base_fee: 1_500_000_000,
            }
        );
        assert_eq!(
            PlanEvm::default().resolve(&parent),
            torus_types::EvmHeaderValues::of(&parent),
            "an empty plan keeps every parent value"
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
