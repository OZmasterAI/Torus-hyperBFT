//! Delegate/undelegate state management (task 1.11.1).
//!
//! All staking state is persisted via `StateDb::put_cf_raw` into the four
//! staking column families defined in `torus-state/src/cf.rs`.

use alloy_primitives::{Address, U256};
use borsh::BorshDeserialize;
use torus_state::cf::{
    CF_CONSENSUS_META, CF_JAIL_VOTES, CF_SLASH_RECORDS, CF_STAKING_DELEGATIONS,
    CF_STAKING_PERMANENT, CF_STAKING_REWARDS, CF_STAKING_VALIDATORS,
};
use torus_state::StateDb;

use crate::rewards::FeeSplitter;

use crate::error::EconomicsError;
use crate::types::*;

type Result<T> = std::result::Result<T, EconomicsError>;

/// Manages validator registration, delegation, undelegation, permanent staking,
/// and reward claiming against the RocksDB staking column families.
#[derive(Clone)]
pub struct StakingManager {
    state_db: StateDb,
}

impl StakingManager {
    pub fn new(state_db: StateDb) -> Self {
        Self { state_db }
    }

    pub fn state_db(&self) -> &StateDb {
        &self.state_db
    }

    // ========================================================================
    // Validator registration
    // ========================================================================

    /// Register a new validator. Debits `self_stake` from the sender's balance.
    pub fn register_validator(
        &self,
        sender: Address,
        pubkey: [u8; 32],
        commission_bps: u16,
        self_stake: U256,
    ) -> Result<()> {
        if commission_bps > MAX_COMMISSION_BPS {
            return Err(EconomicsError::CommissionTooHigh {
                rate_bps: commission_bps,
            });
        }
        if self_stake < MIN_SELF_DELEGATION {
            return Err(EconomicsError::BelowMinSelfDelegation {
                amount: self_stake,
                minimum: MIN_SELF_DELEGATION,
            });
        }

        // Check not already registered.
        if self.get_validator(&sender)?.is_some() {
            return Err(EconomicsError::ValidatorAlreadyRegistered(sender));
        }

        // Debit balance.
        self.debit_balance(&sender, self_stake)?;

        let state = ValidatorState {
            address: sender,
            pubkey,
            commission_bps,
            self_stake,
            total_delegated: U256::ZERO,
            status: ValidatorStatus::Candidate,
            jailed_until: None,
            last_commission_change_block: None,
        };
        self.put_validator(&sender, &state)?;

        tracing::info!(%sender, %self_stake, "validator registered");
        Ok(())
    }

    // ========================================================================
    // Delegation
    // ========================================================================

    /// Delegate tokens from `delegator` to `validator`.
    pub fn delegate(&self, delegator: Address, validator: Address, amount: U256) -> Result<()> {
        if amount.is_zero() {
            return Ok(());
        }

        // Validator must exist and not be jailed or tombstoned.
        let mut val = self
            .get_validator(&validator)?
            .ok_or(EconomicsError::ValidatorNotFound(validator))?;
        // BUG FIX (3.1): Block delegations to Jailed validators, not just Tombstoned
        if val.status == ValidatorStatus::Jailed {
            return Err(EconomicsError::ValidatorJailed(validator));
        }
        if val.status == ValidatorStatus::Tombstoned {
            return Err(EconomicsError::ValidatorTombstoned(validator));
        }

        // Debit delegator balance.
        self.debit_balance(&delegator, amount)?;

        // Update or create delegation record.
        let del_key = delegation_key(&delegator, &validator);
        let mut delegation = self.get_delegation_raw(&del_key)?.unwrap_or(Delegation {
            delegator,
            validator,
            amount: U256::ZERO,
            unbonding: vec![],
        });
        delegation.amount += amount;
        self.put_delegation_raw(&del_key, &delegation)?;

        // Update validator total_delegated.
        val.total_delegated += amount;
        self.put_validator(&validator, &val)?;

        tracing::debug!(%delegator, %validator, %amount, "delegated");
        Ok(())
    }

    /// Undelegate tokens. Creates an unbonding entry with release after UNBONDING_PERIOD blocks.
    pub fn undelegate(
        &self,
        delegator: Address,
        validator: Address,
        amount: U256,
        current_block: u64,
    ) -> Result<()> {
        if amount.is_zero() {
            return Ok(());
        }

        let mut val = self
            .get_validator(&validator)?
            .ok_or(EconomicsError::ValidatorNotFound(validator))?;

        let del_key = delegation_key(&delegator, &validator);
        let mut delegation =
            self.get_delegation_raw(&del_key)?
                .ok_or(EconomicsError::DelegationNotFound {
                    delegator,
                    validator,
                })?;

        if delegation.amount < amount {
            return Err(EconomicsError::InsufficientDelegation {
                have: delegation.amount,
                amount,
            });
        }

        // Reduce active delegation.
        delegation.amount -= amount;

        // FIX ECON-FIND-30: Cap unbonding entries to prevent DoS via queue bloat.
        const MAX_UNBONDING_ENTRIES: usize = 100;
        if delegation.unbonding.len() >= MAX_UNBONDING_ENTRIES {
            return Err(EconomicsError::TooManyUnbondingEntries { delegator, validator });
        }

        // Add unbonding entry.
        delegation.unbonding.push(UnbondingEntry {
            amount,
            release_block: current_block + UNBONDING_PERIOD,
        });
        self.put_delegation_raw(&del_key, &delegation)?;

        // Update validator total_delegated.
        val.total_delegated -= amount;
        self.put_validator(&validator, &val)?;

        tracing::debug!(%delegator, %validator, %amount, "undelegated, unbonding started");
        Ok(())
    }

    /// Process matured unbonding entries for a specific delegation.
    /// Credits released tokens back to the delegator's balance.
    pub fn process_unbonding(
        &self,
        delegator: Address,
        validator: Address,
        current_block: u64,
    ) -> Result<U256> {
        let del_key = delegation_key(&delegator, &validator);
        let mut delegation = match self.get_delegation_raw(&del_key)? {
            Some(d) => d,
            None => return Ok(U256::ZERO),
        };

        let mut released = U256::ZERO;
        let mut remaining = Vec::new();

        for entry in delegation.unbonding.drain(..) {
            if current_block >= entry.release_block {
                released += entry.amount;
            } else {
                remaining.push(entry);
            }
        }
        delegation.unbonding = remaining;

        if !released.is_zero() {
            self.credit_balance(&delegator, released)?;
        }

        // Clean up empty delegation records.
        if delegation.amount.is_zero() && delegation.unbonding.is_empty() {
            self.delete_delegation_raw(&del_key)?;
        } else {
            self.put_delegation_raw(&del_key, &delegation)?;
        }

        Ok(released)
    }

    // ========================================================================
    // Permanent staking
    // ========================================================================

    /// Lock tokens permanently. Irreversible unless governance votes to unlock
    /// (80% supermajority via `PermanentUnlock` proposal).
    pub fn permanent_stake(&self, staker: Address, amount: U256, current_block: u64) -> Result<()> {
        if amount.is_zero() {
            return Ok(());
        }

        self.debit_balance(&staker, amount)?;

        let mut info = self
            .get_permanent_stake(&staker)?
            .unwrap_or(PermanentStakeInfo {
                staker,
                amount: U256::ZERO,
                locked_at_block: current_block,
            });
        info.amount += amount;
        self.put_permanent_stake(&staker, &info)?;

        tracing::info!(%staker, %amount, "permanently staked");
        Ok(())
    }

    /// Unlock permanently staked tokens via governance vote. Only callable from
    /// within the crate (governance execution), never from user transactions.
    ///
    /// Mid-epoch timing: the staker receives full rewards for the last completed
    /// epoch (already distributed). They will not appear in the next epoch's
    /// `distribute_permanent_staking_rewards` since their record is removed/reduced.
    ///
    /// Vote weight: the staker's 1.5x governance weight automatically decreases
    /// for future proposals since `compute_vote_weight()` reads live permanent stake.
    /// Past proposal votes are unaffected (ECON-FIND-16 snapshots).
    pub(crate) fn governance_unlock_permanent_stake(
        &self,
        staker: Address,
        amount: U256,
    ) -> Result<()> {
        if amount.is_zero() {
            return Err(EconomicsError::InvalidParameterValue {
                key: "amount".to_string(),
                reason: "permanent unlock amount must be > 0".to_string(),
            });
        }

        let info = self
            .get_permanent_stake(&staker)?
            .ok_or(EconomicsError::PermanentStakeNotFound(staker))?;

        if amount > info.amount {
            return Err(EconomicsError::PermanentUnlockExceedsStake {
                amount,
                stake: info.amount,
            });
        }

        // Update or remove the permanent stake entry.
        if amount == info.amount {
            self.delete_permanent_stake(&staker)?;
        } else {
            let updated = PermanentStakeInfo {
                staker,
                amount: info.amount - amount,
                locked_at_block: info.locked_at_block,
            };
            self.put_permanent_stake(&staker, &updated)?;
        }

        // Claim any accrued pending rewards (from delegation etc.) if present.
        let claimed_rewards = match self.get_pending_rewards(&staker)? {
            Some(rewards) if !rewards.amount.is_zero() => {
                let r = rewards.amount;
                self.delete_pending_rewards(&staker)?;
                r
            }
            _ => U256::ZERO,
        };

        // Credit unlocked principal + any claimed rewards to liquid balance.
        self.credit_balance(&staker, amount + claimed_rewards)?;

        tracing::info!(
            %staker, %amount, %claimed_rewards,
            "permanent stake unlocked via governance"
        );
        Ok(())
    }

    // ========================================================================
    // Reward claiming
    // ========================================================================

    /// Claim all pending rewards for `address`, crediting them to the account balance.
    pub fn claim_rewards(&self, address: Address) -> Result<U256> {
        let rewards = self
            .get_pending_rewards(&address)?
            .ok_or(EconomicsError::NoRewards(address))?;

        if rewards.amount.is_zero() {
            return Err(EconomicsError::NoRewards(address));
        }

        let amount = rewards.amount;
        self.credit_balance(&address, amount)?;
        self.delete_pending_rewards(&address)?;

        tracing::debug!(%address, %amount, "rewards claimed");
        Ok(amount)
    }

    // ========================================================================
    // Commission updates
    // ========================================================================

    /// Update validator commission rate. Enforces max 5000 bps, max 100 bps change,
    /// and cooldown period between changes (Phase 3: 3.2.3).
    pub fn update_commission(
        &self,
        validator: Address,
        new_rate: u16,
        current_block: u64,
    ) -> Result<()> {
        if new_rate > MAX_COMMISSION_BPS {
            return Err(EconomicsError::CommissionTooHigh { rate_bps: new_rate });
        }

        let mut val = self
            .get_validator(&validator)?
            .ok_or(EconomicsError::ValidatorNotFound(validator))?;

        let delta = new_rate.abs_diff(val.commission_bps);

        if delta > MAX_COMMISSION_CHANGE_BPS {
            return Err(EconomicsError::CommissionChangeTooLarge { delta });
        }

        // BUG FIX (3.2): Enforce commission cooldown — prevents rapid ramping
        if let Some(last_change) = val.last_commission_change_block {
            if current_block < last_change + COMMISSION_COOLDOWN_BLOCKS {
                return Err(EconomicsError::CommissionCooldownNotExpired(validator));
            }
        }

        val.commission_bps = new_rate;
        val.last_commission_change_block = Some(current_block);
        self.put_validator(&validator, &val)?;

        tracing::info!(%validator, new_rate, "commission updated");
        Ok(())
    }

    // ========================================================================
    // Slashing (Phase 3: 3.1.1)
    // ========================================================================

    /// Slash a validator by `fraction_bps` basis points. Reduces self_stake and
    /// every delegation proportionally. Burns slashed tokens. Auto-jails if
    /// self_stake drops below MIN_SELF_DELEGATION.
    pub fn slash(
        &self,
        validator_addr: Address,
        fraction_bps: u16,
        reason: SlashReason,
        current_block: u64,
    ) -> Result<U256> {
        let mut val = self
            .get_validator(&validator_addr)?
            .ok_or(EconomicsError::ValidatorNotFound(validator_addr))?;

        let bps_10000 = U256::from(10_000u32);
        let fraction = U256::from(fraction_bps);

        // Slash self_stake
        let self_slash = val.self_stake * fraction / bps_10000;
        val.self_stake -= self_slash;
        let mut total_slashed = self_slash;

        // Slash all delegations proportionally
        let delegations = self.delegations_for_validator(&validator_addr)?;
        let mut total_del_slashed = U256::ZERO;

        // FIX ECON-FIND-14: Always include delegation in accounting, even if del_slash
        // rounds to zero. Skipping zero-slash delegations caused total_slashed to
        // undercount the expected slash amount.
        for del in &delegations {
            let del_slash = del.amount * fraction / bps_10000;
            if !del_slash.is_zero() {
                let mut updated_del = del.clone();
                updated_del.amount -= del_slash;
                let del_key = delegation_key(&del.delegator, &validator_addr);
                self.put_delegation_raw(&del_key, &updated_del)?;
            }
            total_del_slashed += del_slash;
            total_slashed += del_slash;
        }

        // FIX ECON-FIND-27: Recompute total_delegated from actual delegation amounts
        // instead of using a running counter, to avoid dust rounding divergence.
        let updated_delegations = self.delegations_for_validator(&validator_addr)?;
        val.total_delegated = updated_delegations
            .iter()
            .map(|d| d.amount)
            .fold(U256::ZERO, |a, b| a + b);

        // Auto-jail if self_stake drops below minimum
        if val.self_stake < MIN_SELF_DELEGATION
            && val.status != ValidatorStatus::Tombstoned
            && val.status != ValidatorStatus::Jailed
        {
            val.status = ValidatorStatus::Jailed;
            val.jailed_until = Some(current_block + JAIL_DURATION_BLOCKS);
            tracing::info!(%validator_addr, "auto-jailed: self_stake below minimum after slash");
        }

        self.put_validator(&validator_addr, &val)?;

        // Burn slashed amount
        if !total_slashed.is_zero() {
            FeeSplitter::execute_burn(self, total_slashed)?;
        }

        // Record slash for audit
        self.put_slash_record(
            &validator_addr,
            current_block,
            &SlashRecord {
                validator: validator_addr,
                slash_fraction_bps: fraction_bps,
                slashed_amount: total_slashed,
                reason,
                block_height: current_block,
            },
        )?;

        tracing::info!(
            %validator_addr, %total_slashed, fraction_bps,
            "validator slashed"
        );
        Ok(total_slashed)
    }

    // ========================================================================
    // Jailing (Phase 3: 3.1.2)
    // ========================================================================

    /// Jail a validator with a cooldown duration.
    pub fn jail_validator(
        &self,
        validator_addr: &Address,
        duration_blocks: u64,
        current_block: u64,
    ) -> Result<()> {
        let mut val = self
            .get_validator(validator_addr)?
            .ok_or(EconomicsError::ValidatorNotFound(*validator_addr))?;

        if val.status == ValidatorStatus::Tombstoned {
            return Err(EconomicsError::ValidatorTombstoned(*validator_addr));
        }

        val.status = ValidatorStatus::Jailed;
        val.jailed_until = Some(current_block + duration_blocks);
        self.put_validator(validator_addr, &val)?;

        tracing::info!(%validator_addr, until = current_block + duration_blocks, "validator jailed");
        Ok(())
    }

    /// Tombstone a validator permanently (for double-sign). Cannot be undone.
    pub fn tombstone_validator(&self, validator_addr: &Address) -> Result<()> {
        let mut val = self
            .get_validator(validator_addr)?
            .ok_or(EconomicsError::ValidatorNotFound(*validator_addr))?;

        val.status = ValidatorStatus::Tombstoned;
        val.jailed_until = None;
        self.put_validator(validator_addr, &val)?;

        tracing::info!(%validator_addr, "validator tombstoned");
        Ok(())
    }

    /// Record a jail vote from `voter` against `target`. Returns true if
    /// the >2/3 threshold was reached and the target was jailed.
    pub fn record_jail_vote(
        &self,
        voter: Address,
        target: Address,
        current_block: u64,
    ) -> Result<bool> {
        // Voter must be an active validator
        let voter_val = self
            .get_validator(&voter)?
            .ok_or(EconomicsError::ValidatorNotFound(voter))?;
        if voter_val.status != ValidatorStatus::Active {
            return Err(EconomicsError::ValidatorNotFound(voter));
        }

        // Target must exist and not be tombstoned
        let target_val = self
            .get_validator(&target)?
            .ok_or(EconomicsError::ValidatorNotFound(target))?;
        if target_val.status == ValidatorStatus::Tombstoned {
            return Err(EconomicsError::ValidatorTombstoned(target));
        }

        // Store vote
        let vkey = jail_vote_key(&target, &voter);
        let vote = JailVoteRecord {
            voter,
            target,
            stake_weight: voter_val.total_stake(),
            block_height: current_block,
        };
        let data = borsh::to_vec(&vote).map_err(|e| EconomicsError::Borsh(e.to_string()))?;
        self.state_db.put_cf_raw(CF_JAIL_VOTES, &vkey, &data)?;

        // Tally all non-expired votes for this target
        let total_vote_weight = self.tally_jail_votes(&target, current_block)?;

        // Compute total active validator stake for 2/3 threshold
        let total_active_stake = self.total_active_stake()?;

        // Check >2/3 threshold: vote_weight * 3 > active_stake * 2
        if total_vote_weight * U256::from(3u64) > total_active_stake * U256::from(2u64) {
            self.slash(target, DOWNTIME_SLASH_BPS, SlashReason::JailVote, current_block)?;
            self.jail_validator(&target, JAIL_DURATION_BLOCKS, current_block)?;
            tracing::info!(%target, "jail vote threshold reached, validator jailed");
            return Ok(true);
        }

        Ok(false)
    }

    /// Tally non-expired jail votes for a target. Cleans up expired votes.
    fn tally_jail_votes(&self, target: &Address, current_block: u64) -> Result<U256> {
        let db = self.state_db.inner();
        let cf = db.cf_handle(CF_JAIL_VOTES).ok_or_else(|| {
            EconomicsError::State(torus_state::StateError::MissingColumnFamily(
                CF_JAIL_VOTES.to_string(),
            ))
        })?;

        let prefix = target.as_slice();
        let iter = db.prefix_iterator_cf(cf, prefix);
        let mut total_weight = U256::ZERO;
        let mut expired_keys = Vec::new();

        for item in iter {
            let (key, value) =
                item.map_err(|e| EconomicsError::State(torus_state::StateError::RocksDb(e)))?;
            if !key.starts_with(prefix) {
                break;
            }
            let vote = JailVoteRecord::try_from_slice(&value)
                .map_err(|e| EconomicsError::Borsh(e.to_string()))?;

            if current_block > vote.block_height + JAIL_VOTE_EXPIRY_BLOCKS {
                expired_keys.push(key.to_vec());
                continue;
            }

            // FIX ECON-FIND-24: Use current stake, not stored vote-time stake.
            let current_weight = match self.get_validator(&vote.voter) {
                Ok(Some(v)) if v.status == ValidatorStatus::Active => v.total_stake(),
                _ => U256::ZERO, // Voter no longer active — don't count
            };
            total_weight += current_weight;
        }

        // Clean up expired votes
        for key in &expired_keys {
            self.state_db.delete_cf_raw(CF_JAIL_VOTES, key)?;
        }

        Ok(total_weight)
    }

    /// Compute total stake of all Active validators.
    fn total_active_stake(&self) -> Result<U256> {
        let all = self.all_validators()?;
        Ok(all
            .iter()
            .filter(|v| v.status == ValidatorStatus::Active)
            .map(|v| v.total_stake())
            .fold(U256::ZERO, |acc, s| acc + s))
    }

    /// Clear all jail votes targeting a validator (called on unjail).
    pub fn clear_jail_votes(&self, target: &Address) -> Result<()> {
        let db = self.state_db.inner();
        let cf = db.cf_handle(CF_JAIL_VOTES).ok_or_else(|| {
            EconomicsError::State(torus_state::StateError::MissingColumnFamily(
                CF_JAIL_VOTES.to_string(),
            ))
        })?;

        let prefix = target.as_slice();
        let iter = db.prefix_iterator_cf(cf, prefix);
        let mut keys = Vec::new();

        for item in iter {
            let (key, _) =
                item.map_err(|e| EconomicsError::State(torus_state::StateError::RocksDb(e)))?;
            if !key.starts_with(prefix) {
                break;
            }
            keys.push(key.to_vec());
        }

        for key in &keys {
            self.state_db.delete_cf_raw(CF_JAIL_VOTES, key)?;
        }

        Ok(())
    }

    // ========================================================================
    // Unjail (Phase 3: 3.1.3)
    // ========================================================================

    /// Unjail a validator. Requirements:
    /// - Status must be Jailed (NOT Tombstoned)
    /// - Cooldown expired (current_block >= jailed_until)
    /// - self_stake >= MIN_SELF_DELEGATION
    /// Sets status to Candidate (must wait for next epoch to become Active).
    pub fn unjail(&self, validator_addr: &Address, current_block: u64) -> Result<()> {
        let mut val = self
            .get_validator(validator_addr)?
            .ok_or(EconomicsError::ValidatorNotFound(*validator_addr))?;

        if val.status == ValidatorStatus::Tombstoned {
            return Err(EconomicsError::ValidatorTombstoned(*validator_addr));
        }
        if val.status != ValidatorStatus::Jailed {
            return Err(EconomicsError::ValidatorNotJailed(*validator_addr));
        }

        // Check cooldown
        if let Some(jailed_until) = val.jailed_until {
            if current_block < jailed_until {
                return Err(EconomicsError::UnjailCooldownNotExpired(*validator_addr));
            }
        }

        // Check self_stake
        if val.self_stake < MIN_SELF_DELEGATION {
            return Err(EconomicsError::UnjailInsufficientStake {
                address: *validator_addr,
                have: val.self_stake,
                minimum: MIN_SELF_DELEGATION,
            });
        }

        // Set to Candidate — must wait for next epoch to re-enter active set
        val.status = ValidatorStatus::Candidate;
        val.jailed_until = None;
        self.put_validator(validator_addr, &val)?;

        // Clear pending jail votes
        self.clear_jail_votes(validator_addr)?;

        tracing::info!(%validator_addr, "validator unjailed → Candidate");
        Ok(())
    }

    // ========================================================================
    // Self-stake top-up (ECON-FIND-28)
    // ========================================================================

    /// Top up a validator's self_stake. Allows recovery from a slash that
    /// dropped self_stake below MIN_SELF_DELEGATION. Works even when jailed.
    pub fn top_up_self_stake(&self, validator_addr: Address, amount: U256) -> Result<()> {
        if amount.is_zero() {
            return Ok(());
        }
        let mut val = self
            .get_validator(&validator_addr)?
            .ok_or(EconomicsError::ValidatorNotFound(validator_addr))?;
        // Allow top-up even when jailed (the whole point is to recover)
        self.debit_balance(&validator_addr, amount)?;
        val.self_stake += amount;
        self.put_validator(&validator_addr, &val)?;
        tracing::info!(%validator_addr, %amount, "self-stake topped up");
        Ok(())
    }

    // ========================================================================
    // Slash record persistence
    // ========================================================================

    fn put_slash_record(
        &self,
        validator: &Address,
        block_height: u64,
        record: &SlashRecord,
    ) -> Result<()> {
        let key = slash_record_key(validator, block_height);
        let data = borsh::to_vec(record).map_err(|e| EconomicsError::Borsh(e.to_string()))?;
        self.state_db
            .put_cf_raw(CF_SLASH_RECORDS, &key, &data)?;
        Ok(())
    }

    // ========================================================================
    // Low-level CF accessors
    // ========================================================================

    pub fn get_validator(&self, address: &Address) -> Result<Option<ValidatorState>> {
        match self
            .state_db
            .get_cf_raw(CF_STAKING_VALIDATORS, address.as_slice())?
        {
            Some(data) => Ok(Some(
                ValidatorState::try_from_slice(&data)
                    .map_err(|e| EconomicsError::Borsh(e.to_string()))?,
            )),
            None => Ok(None),
        }
    }

    pub fn put_validator(&self, address: &Address, state: &ValidatorState) -> Result<()> {
        let data = borsh::to_vec(state).map_err(|e| EconomicsError::Borsh(e.to_string()))?;
        self.state_db
            .put_cf_raw(CF_STAKING_VALIDATORS, address.as_slice(), &data)?;
        Ok(())
    }

    fn get_delegation_raw(&self, key: &[u8; 40]) -> Result<Option<Delegation>> {
        match self.state_db.get_cf_raw(CF_STAKING_DELEGATIONS, key)? {
            Some(data) => Ok(Some(
                Delegation::try_from_slice(&data)
                    .map_err(|e| EconomicsError::Borsh(e.to_string()))?,
            )),
            None => Ok(None),
        }
    }

    fn put_delegation_raw(&self, key: &[u8; 40], delegation: &Delegation) -> Result<()> {
        let data = borsh::to_vec(delegation).map_err(|e| EconomicsError::Borsh(e.to_string()))?;
        self.state_db
            .put_cf_raw(CF_STAKING_DELEGATIONS, key, &data)?;
        Ok(())
    }

    fn delete_delegation_raw(&self, key: &[u8; 40]) -> Result<()> {
        self.state_db.delete_cf_raw(CF_STAKING_DELEGATIONS, key)?;
        Ok(())
    }

    pub fn get_permanent_stake(&self, address: &Address) -> Result<Option<PermanentStakeInfo>> {
        match self
            .state_db
            .get_cf_raw(CF_STAKING_PERMANENT, address.as_slice())?
        {
            Some(data) => Ok(Some(
                PermanentStakeInfo::try_from_slice(&data)
                    .map_err(|e| EconomicsError::Borsh(e.to_string()))?,
            )),
            None => Ok(None),
        }
    }

    fn put_permanent_stake(&self, address: &Address, info: &PermanentStakeInfo) -> Result<()> {
        let data = borsh::to_vec(info).map_err(|e| EconomicsError::Borsh(e.to_string()))?;
        self.state_db
            .put_cf_raw(CF_STAKING_PERMANENT, address.as_slice(), &data)?;
        Ok(())
    }

    fn delete_permanent_stake(&self, address: &Address) -> Result<()> {
        self.state_db
            .delete_cf_raw(CF_STAKING_PERMANENT, address.as_slice())?;
        Ok(())
    }

    pub fn get_pending_rewards(&self, address: &Address) -> Result<Option<PendingRewards>> {
        match self
            .state_db
            .get_cf_raw(CF_STAKING_REWARDS, address.as_slice())?
        {
            Some(data) => Ok(Some(
                PendingRewards::try_from_slice(&data)
                    .map_err(|e| EconomicsError::Borsh(e.to_string()))?,
            )),
            None => Ok(None),
        }
    }

    pub fn credit_rewards(&self, address: Address, amount: U256) -> Result<()> {
        let mut rewards = self
            .get_pending_rewards(&address)?
            .unwrap_or(PendingRewards {
                address,
                amount: U256::ZERO,
            });
        rewards.amount += amount;
        let data = borsh::to_vec(&rewards).map_err(|e| EconomicsError::Borsh(e.to_string()))?;
        self.state_db
            .put_cf_raw(CF_STAKING_REWARDS, address.as_slice(), &data)?;
        Ok(())
    }

    fn delete_pending_rewards(&self, address: &Address) -> Result<()> {
        self.state_db
            .delete_cf_raw(CF_STAKING_REWARDS, address.as_slice())?;
        Ok(())
    }

    // ========================================================================
    // Balance helpers (operate on EVM account balances)
    // ========================================================================

    fn debit_balance(&self, address: &Address, amount: U256) -> Result<()> {
        let mut account = self.state_db.get_account(address)?.unwrap_or_default();

        if account.balance < amount {
            return Err(EconomicsError::InsufficientBalance {
                have: account.balance,
                need: amount,
            });
        }
        account.balance -= amount;
        self.state_db.put_account(address, &account)?;
        Ok(())
    }

    pub fn credit_balance(&self, address: &Address, amount: U256) -> Result<()> {
        let mut account = self.state_db.get_account(address)?.unwrap_or_default();
        account.balance += amount;
        self.state_db.put_account(address, &account)?;
        Ok(())
    }

    // ========================================================================
    // Iteration helpers
    // ========================================================================

    /// Read all validators from CF_STAKING_VALIDATORS.
    pub fn all_validators(&self) -> Result<Vec<ValidatorState>> {
        let db = self.state_db.inner();
        let cf = db.cf_handle(CF_STAKING_VALIDATORS).ok_or_else(|| {
            EconomicsError::State(torus_state::StateError::MissingColumnFamily(
                CF_STAKING_VALIDATORS.to_string(),
            ))
        })?;
        let iter = db.iterator_cf(cf, rocksdb::IteratorMode::Start);
        let mut validators = Vec::new();
        for item in iter {
            let (_key, value) =
                item.map_err(|e| EconomicsError::State(torus_state::StateError::RocksDb(e)))?;
            let state = ValidatorState::try_from_slice(&value)
                .map_err(|e| EconomicsError::Borsh(e.to_string()))?;
            validators.push(state);
        }
        Ok(validators)
    }

    /// FIX CONS-FIND-01: Look up a validator by their consensus public key.
    /// Returns the validator's Ethereum address and state if found.
    pub fn find_validator_by_pubkey(&self, pubkey: &[u8; 32]) -> Result<Option<ValidatorState>> {
        let all = self.all_validators()?;
        Ok(all.into_iter().find(|v| &v.pubkey == pubkey))
    }

    /// Read all delegations for a specific delegator (prefix scan on delegator address).
    pub fn delegations_for_delegator(&self, delegator: &Address) -> Result<Vec<Delegation>> {
        let db = self.state_db.inner();
        let cf = db.cf_handle(CF_STAKING_DELEGATIONS).ok_or_else(|| {
            EconomicsError::State(torus_state::StateError::MissingColumnFamily(
                CF_STAKING_DELEGATIONS.to_string(),
            ))
        })?;
        let prefix = delegator.as_slice();
        let iter = db.prefix_iterator_cf(cf, prefix);
        let mut delegations = Vec::new();
        for item in iter {
            let (key, value) =
                item.map_err(|e| EconomicsError::State(torus_state::StateError::RocksDb(e)))?;
            if !key.starts_with(prefix) {
                break;
            }
            let delegation = Delegation::try_from_slice(&value)
                .map_err(|e| EconomicsError::Borsh(e.to_string()))?;
            delegations.push(delegation);
        }
        Ok(delegations)
    }

    /// Read all delegations for a specific validator.
    ///
    /// AUDIT: ECON-FIND-30 -- O(n) scan over all delegations. The key layout
    /// `delegator(20) ++ validator(20)` prevents prefix-based lookup by validator.
    /// A secondary index would fix this but requires schema migration. Acceptable
    /// for now: only called during slashing, not on the per-block hot path.
    pub fn delegations_for_validator(&self, validator: &Address) -> Result<Vec<Delegation>> {
        let db = self.state_db.inner();
        let cf = db.cf_handle(CF_STAKING_DELEGATIONS).ok_or_else(|| {
            EconomicsError::State(torus_state::StateError::MissingColumnFamily(
                CF_STAKING_DELEGATIONS.to_string(),
            ))
        })?;
        let iter = db.iterator_cf(cf, rocksdb::IteratorMode::Start);
        let mut delegations = Vec::new();
        for item in iter {
            let (key, value) =
                item.map_err(|e| EconomicsError::State(torus_state::StateError::RocksDb(e)))?;
            // Key is delegator(20) ++ validator(20). Check the validator suffix.
            if key.len() == 40 && &key[20..] == validator.as_slice() {
                let delegation = Delegation::try_from_slice(&value)
                    .map_err(|e| EconomicsError::Borsh(e.to_string()))?;
                delegations.push(delegation);
            }
        }
        Ok(delegations)
    }

    // ========================================================================
    // Key Rotation (Phase 3: 3.1.8)
    // ========================================================================

    /// Submit a key rotation request. The rotation takes effect at the next epoch
    /// boundary. Must be signed by the current key (verified by caller).
    pub fn submit_key_rotation(
        &self,
        validator_addr: Address,
        new_pubkey: [u8; 32],
        current_epoch: u64,
        current_block: u64,
    ) -> Result<()> {
        let val = self
            .get_validator(&validator_addr)?
            .ok_or(EconomicsError::ValidatorNotFound(validator_addr))?;

        // Cannot rotate while jailed or tombstoned
        if val.status == ValidatorStatus::Jailed {
            return Err(EconomicsError::CannotRotateWhileJailed(validator_addr));
        }
        if val.status == ValidatorStatus::Tombstoned {
            return Err(EconomicsError::ValidatorTombstoned(validator_addr));
        }

        // Check no pending rotation already exists
        if self.get_pending_rotation(&validator_addr)?.is_some() {
            return Err(EconomicsError::KeyRotationAlreadyPending(validator_addr));
        }

        // Check new pubkey isn't already in use
        let all_validators = self.all_validators()?;
        for v in &all_validators {
            if v.pubkey == new_pubkey {
                return Err(EconomicsError::PubkeyAlreadyInUse);
            }
        }

        // Store pending rotation
        let rotation = PendingKeyRotation {
            validator: validator_addr,
            new_pubkey,
            effective_epoch: current_epoch + 1,
            submitted_at_block: current_block,
        };
        self.put_pending_rotation(&validator_addr, &rotation)?;

        tracing::info!(
            %validator_addr,
            effective_epoch = current_epoch + 1,
            "key rotation submitted"
        );
        Ok(())
    }

    /// Apply all pending key rotations for the given epoch. Called at epoch boundary.
    /// Returns the list of validators whose keys were rotated.
    /// Apply all pending key rotations for the given epoch. Called at epoch boundary.
    ///
    /// FIX CONS-FIND-09: This function deletes rotations from DB. The caller
    /// (epoch_validator_set_updates) MUST cache results per height to prevent
    /// double-application when called from both produce_block and validate_block.
    pub fn apply_pending_rotations(&self, epoch: u64) -> Result<Vec<(Address, [u8; 32])>> {
        let mut applied = Vec::new();

        // Scan all pending rotations
        let rotations = self.all_pending_rotations()?;

        for rotation in rotations {
            if rotation.effective_epoch == epoch {
                // Apply: update the validator's pubkey
                let mut val = match self.get_validator(&rotation.validator)? {
                    Some(v) => v,
                    None => continue,
                };
                let old_pubkey = val.pubkey;
                val.pubkey = rotation.new_pubkey;
                self.put_validator(&rotation.validator, &val)?;

                // Remove the pending rotation
                self.delete_pending_rotation(&rotation.validator)?;

                applied.push((rotation.validator, rotation.new_pubkey));
                tracing::info!(
                    validator = %rotation.validator,
                    old_pubkey = hex::encode(old_pubkey),
                    new_pubkey = hex::encode(rotation.new_pubkey),
                    "key rotation applied at epoch {epoch}"
                );
            }
        }

        Ok(applied)
    }

    /// Get a pending key rotation for a validator.
    pub fn get_pending_rotation(&self, addr: &Address) -> Result<Option<PendingKeyRotation>> {
        let key = pending_rotation_key(addr);
        match self.state_db.get_cf_raw(CF_CONSENSUS_META, &key)? {
            Some(data) => Ok(Some(
                PendingKeyRotation::try_from_slice(&data)
                    .map_err(|e| EconomicsError::Borsh(e.to_string()))?,
            )),
            None => Ok(None),
        }
    }

    fn put_pending_rotation(&self, addr: &Address, rotation: &PendingKeyRotation) -> Result<()> {
        let key = pending_rotation_key(addr);
        let data = borsh::to_vec(rotation).map_err(|e| EconomicsError::Borsh(e.to_string()))?;
        self.state_db.put_cf_raw(CF_CONSENSUS_META, &key, &data)?;
        Ok(())
    }

    fn delete_pending_rotation(&self, addr: &Address) -> Result<()> {
        let key = pending_rotation_key(addr);
        self.state_db.delete_cf_raw(CF_CONSENSUS_META, &key)?;
        Ok(())
    }

    fn all_pending_rotations(&self) -> Result<Vec<PendingKeyRotation>> {
        let db = self.state_db.inner();
        let cf = db.cf_handle(CF_CONSENSUS_META).ok_or_else(|| {
            EconomicsError::State(torus_state::StateError::MissingColumnFamily(
                CF_CONSENSUS_META.to_string(),
            ))
        })?;
        let prefix = b"pending_rotation:";
        let iter = db.prefix_iterator_cf(cf, prefix);
        let mut rotations = Vec::new();
        for item in iter {
            let (key, value) =
                item.map_err(|e| EconomicsError::State(torus_state::StateError::RocksDb(e)))?;
            if !key.starts_with(prefix) {
                break;
            }
            let rotation = PendingKeyRotation::try_from_slice(&value)
                .map_err(|e| EconomicsError::Borsh(e.to_string()))?;
            rotations.push(rotation);
        }
        Ok(rotations)
    }

    /// Read all permanent stakes (full scan).
    pub fn all_permanent_stakes(&self) -> Result<Vec<PermanentStakeInfo>> {
        let db = self.state_db.inner();
        let cf = db.cf_handle(CF_STAKING_PERMANENT).ok_or_else(|| {
            EconomicsError::State(torus_state::StateError::MissingColumnFamily(
                CF_STAKING_PERMANENT.to_string(),
            ))
        })?;
        let iter = db.iterator_cf(cf, rocksdb::IteratorMode::Start);
        let mut stakes = Vec::new();
        for item in iter {
            let (_key, value) =
                item.map_err(|e| EconomicsError::State(torus_state::StateError::RocksDb(e)))?;
            let info = PermanentStakeInfo::try_from_slice(&value)
                .map_err(|e| EconomicsError::Borsh(e.to_string()))?;
            stakes.push(info);
        }
        Ok(stakes)
    }
}

/// Build the 40-byte delegation key: delegator(20) ++ validator(20).
pub fn delegation_key(delegator: &Address, validator: &Address) -> [u8; 40] {
    let mut key = [0u8; 40];
    key[..20].copy_from_slice(delegator.as_slice());
    key[20..].copy_from_slice(validator.as_slice());
    key
}

/// Build the 28-byte slash record key: validator(20) ++ block_height(8 BE).
pub fn slash_record_key(validator: &Address, block_height: u64) -> [u8; 28] {
    let mut key = [0u8; 28];
    key[..20].copy_from_slice(validator.as_slice());
    key[20..28].copy_from_slice(&block_height.to_be_bytes());
    key
}

/// Build the 40-byte jail vote key: target(20) ++ voter(20).
pub fn jail_vote_key(target: &Address, voter: &Address) -> [u8; 40] {
    let mut key = [0u8; 40];
    key[..20].copy_from_slice(target.as_slice());
    key[20..].copy_from_slice(voter.as_slice());
    key
}

/// Build key for pending key rotation: "pending_rotation:" ++ address(20).
fn pending_rotation_key(addr: &Address) -> Vec<u8> {
    let mut key = b"pending_rotation:".to_vec();
    key.extend_from_slice(addr.as_slice());
    key
}

mod hex {
    pub fn encode(bytes: impl AsRef<[u8]>) -> String {
        bytes.as_ref().iter().map(|b| format!("{b:02x}")).collect()
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use revm::state::AccountInfo;

    fn setup() -> (tempfile::TempDir, StakingManager) {
        let dir = tempfile::tempdir().unwrap();
        let db = StateDb::open(dir.path()).unwrap();
        (dir, StakingManager::new(db))
    }

    fn fund_account(mgr: &StakingManager, addr: &Address, amount: U256) {
        let info = AccountInfo {
            balance: amount,
            ..Default::default()
        };
        mgr.state_db().put_account(addr, &info).unwrap();
    }

    fn addr(n: u8) -> Address {
        Address::new([n; 20])
    }

    #[test]
    fn register_validator_and_delegate_round_trip() {
        let (_dir, mgr) = setup();
        let validator = addr(1);
        let delegator = addr(2);

        // Fund both accounts.
        fund_account(
            &mgr,
            &validator,
            U256::from(20_000u64) * U256::from(10u64).pow(U256::from(18u64)),
        );
        fund_account(
            &mgr,
            &delegator,
            U256::from(5_000u64) * U256::from(10u64).pow(U256::from(18u64)),
        );

        let self_stake = U256::from(10_000u64) * U256::from(10u64).pow(U256::from(18u64));
        mgr.register_validator(validator, [1u8; 32], 500, self_stake)
            .unwrap();

        let val = mgr.get_validator(&validator).unwrap().unwrap();
        assert_eq!(val.self_stake, self_stake);
        assert_eq!(val.status, ValidatorStatus::Candidate);

        // Delegate.
        let del_amount = U256::from(1_000u64) * U256::from(10u64).pow(U256::from(18u64));
        mgr.delegate(delegator, validator, del_amount).unwrap();

        let val = mgr.get_validator(&validator).unwrap().unwrap();
        assert_eq!(val.total_delegated, del_amount);

        // Check delegator balance was debited.
        let acct = mgr.state_db().get_account(&delegator).unwrap().unwrap();
        assert_eq!(
            acct.balance,
            U256::from(4_000u64) * U256::from(10u64).pow(U256::from(18u64))
        );
    }

    #[test]
    fn insufficient_balance_rejected() {
        let (_dir, mgr) = setup();
        let validator = addr(1);

        fund_account(&mgr, &validator, U256::from(100u64));

        let result = mgr.register_validator(validator, [1u8; 32], 500, MIN_SELF_DELEGATION);
        assert!(matches!(
            result,
            Err(EconomicsError::InsufficientBalance { .. })
        ));
    }

    #[test]
    fn below_min_self_delegation_rejected() {
        let (_dir, mgr) = setup();
        let validator = addr(1);

        fund_account(&mgr, &validator, U256::from(10u64).pow(U256::from(25u64)));

        let too_small = MIN_SELF_DELEGATION - U256::from(1u64);
        let result = mgr.register_validator(validator, [1u8; 32], 500, too_small);
        assert!(matches!(
            result,
            Err(EconomicsError::BelowMinSelfDelegation { .. })
        ));
    }

    #[test]
    fn undelegate_and_unbonding_timing() {
        let (_dir, mgr) = setup();
        let validator = addr(1);
        let delegator = addr(2);

        let big = U256::from(100_000u64) * U256::from(10u64).pow(U256::from(18u64));
        fund_account(&mgr, &validator, big);
        fund_account(&mgr, &delegator, big);

        let self_stake = MIN_SELF_DELEGATION;
        mgr.register_validator(validator, [1u8; 32], 500, self_stake)
            .unwrap();

        let del_amount = U256::from(5_000u64) * U256::from(10u64).pow(U256::from(18u64));
        mgr.delegate(delegator, validator, del_amount).unwrap();

        // Undelegate at block 1000.
        let undel_amount = U256::from(2_000u64) * U256::from(10u64).pow(U256::from(18u64));
        mgr.undelegate(delegator, validator, undel_amount, 1000)
            .unwrap();

        // Process before release: nothing should return.
        let released = mgr
            .process_unbonding(delegator, validator, 1000 + UNBONDING_PERIOD - 1)
            .unwrap();
        assert_eq!(released, U256::ZERO);

        // Process at release block: should credit balance.
        let released = mgr
            .process_unbonding(delegator, validator, 1000 + UNBONDING_PERIOD)
            .unwrap();
        assert_eq!(released, undel_amount);
    }

    #[test]
    fn permanent_stake_irreversible() {
        let (_dir, mgr) = setup();
        let staker = addr(3);

        let balance = U256::from(50_000u64) * U256::from(10u64).pow(U256::from(18u64));
        fund_account(&mgr, &staker, balance);

        let stake_amount = U256::from(10_000u64) * U256::from(10u64).pow(U256::from(18u64));
        mgr.permanent_stake(staker, stake_amount, 100).unwrap();

        let info = mgr.get_permanent_stake(&staker).unwrap().unwrap();
        assert_eq!(info.amount, stake_amount);
        assert_eq!(info.locked_at_block, 100);

        // Balance reduced.
        let acct = mgr.state_db().get_account(&staker).unwrap().unwrap();
        assert_eq!(acct.balance, balance - stake_amount);
    }

    #[test]
    fn claim_rewards() {
        let (_dir, mgr) = setup();
        let user = addr(4);

        fund_account(&mgr, &user, U256::ZERO);

        // Manually credit rewards.
        let reward = U256::from(500u64) * U256::from(10u64).pow(U256::from(18u64));
        mgr.credit_rewards(user, reward).unwrap();

        let claimed = mgr.claim_rewards(user).unwrap();
        assert_eq!(claimed, reward);

        // Balance should be credited.
        let acct = mgr.state_db().get_account(&user).unwrap().unwrap();
        assert_eq!(acct.balance, reward);

        // Claiming again should fail.
        assert!(matches!(
            mgr.claim_rewards(user),
            Err(EconomicsError::NoRewards(_))
        ));
    }

    fn wei(tokens: u64) -> U256 {
        U256::from(tokens) * U256::from(10u64).pow(U256::from(18u64))
    }

    // ====================================================================
    // Slashing tests (3.1.1)
    // ====================================================================

    #[test]
    fn slash_reduces_stake_proportionally() {
        let (_dir, mgr) = setup();
        let validator = addr(1);
        let delegator = addr(2);

        fund_account(&mgr, &validator, wei(100_000));
        fund_account(&mgr, &delegator, wei(100_000));

        mgr.register_validator(validator, [1u8; 32], 500, wei(50_000))
            .unwrap();
        mgr.delegate(delegator, validator, wei(50_000)).unwrap();

        // Slash 5% (500 bps)
        let slashed = mgr
            .slash(validator, 500, SlashReason::DoubleSign, 100)
            .unwrap();

        let val = mgr.get_validator(&validator).unwrap().unwrap();
        // self_stake: 50_000 * 0.95 = 47_500
        assert_eq!(val.self_stake, wei(47_500));
        // total_delegated: 50_000 * 0.95 = 47_500
        assert_eq!(val.total_delegated, wei(47_500));
        // Total slashed: 100_000 * 0.05 = 5_000
        assert_eq!(slashed, wei(5_000));
    }

    #[test]
    fn slash_burns_tokens() {
        let (_dir, mgr) = setup();
        let validator = addr(1);

        fund_account(&mgr, &validator, wei(100_000));
        mgr.register_validator(validator, [1u8; 32], 500, wei(50_000))
            .unwrap();

        let slashed = mgr
            .slash(validator, 500, SlashReason::Downtime, 100)
            .unwrap();

        // Verify burn was recorded
        let tracker = crate::rewards::FeeSplitter::get_supply_tracker(&mgr).unwrap();
        assert_eq!(tracker.cumulative_burned, slashed);
    }

    #[test]
    fn slash_below_min_triggers_auto_jail() {
        let (_dir, mgr) = setup();
        let validator = addr(1);

        // Register with exactly the minimum stake
        fund_account(&mgr, &validator, MIN_SELF_DELEGATION);
        mgr.register_validator(validator, [1u8; 32], 500, MIN_SELF_DELEGATION)
            .unwrap();

        // Slash 5% — brings self_stake below minimum
        mgr.slash(validator, 500, SlashReason::Downtime, 100)
            .unwrap();

        let val = mgr.get_validator(&validator).unwrap().unwrap();
        assert_eq!(val.status, ValidatorStatus::Jailed);
        assert!(val.jailed_until.is_some());
    }

    // ====================================================================
    // Jailing tests (3.1.2)
    // ====================================================================

    #[test]
    fn jail_sets_status_and_jailed_until() {
        let (_dir, mgr) = setup();
        let validator = addr(1);

        fund_account(&mgr, &validator, wei(100_000));
        mgr.register_validator(validator, [1u8; 32], 500, wei(50_000))
            .unwrap();

        mgr.jail_validator(&validator, JAIL_DURATION_BLOCKS, 1000)
            .unwrap();

        let val = mgr.get_validator(&validator).unwrap().unwrap();
        assert_eq!(val.status, ValidatorStatus::Jailed);
        assert_eq!(val.jailed_until, Some(1000 + JAIL_DURATION_BLOCKS));
    }

    #[test]
    fn tombstone_is_permanent() {
        let (_dir, mgr) = setup();
        let validator = addr(1);

        fund_account(&mgr, &validator, wei(100_000));
        mgr.register_validator(validator, [1u8; 32], 500, wei(50_000))
            .unwrap();

        mgr.tombstone_validator(&validator).unwrap();

        let val = mgr.get_validator(&validator).unwrap().unwrap();
        assert_eq!(val.status, ValidatorStatus::Tombstoned);
        assert_eq!(val.jailed_until, None);

        // Cannot unjail a tombstoned validator
        let err = mgr.unjail(&validator, 999_999).unwrap_err();
        assert!(matches!(err, EconomicsError::ValidatorTombstoned(_)));
    }

    #[test]
    fn delegate_to_jailed_rejected() {
        let (_dir, mgr) = setup();
        let validator = addr(1);
        let delegator = addr(2);

        fund_account(&mgr, &validator, wei(100_000));
        fund_account(&mgr, &delegator, wei(100_000));

        mgr.register_validator(validator, [1u8; 32], 500, wei(50_000))
            .unwrap();
        mgr.jail_validator(&validator, JAIL_DURATION_BLOCKS, 100)
            .unwrap();

        let err = mgr.delegate(delegator, validator, wei(1_000)).unwrap_err();
        assert!(matches!(err, EconomicsError::ValidatorJailed(_)));
    }

    #[test]
    fn undelegate_from_jailed_allowed() {
        let (_dir, mgr) = setup();
        let validator = addr(1);
        let delegator = addr(2);

        fund_account(&mgr, &validator, wei(100_000));
        fund_account(&mgr, &delegator, wei(100_000));

        mgr.register_validator(validator, [1u8; 32], 500, wei(50_000))
            .unwrap();
        mgr.delegate(delegator, validator, wei(10_000)).unwrap();

        // Jail the validator
        mgr.jail_validator(&validator, JAIL_DURATION_BLOCKS, 100)
            .unwrap();

        // Delegators can still undelegate from jailed validators
        mgr.undelegate(delegator, validator, wei(10_000), 200)
            .unwrap();

        let val = mgr.get_validator(&validator).unwrap().unwrap();
        assert_eq!(val.total_delegated, U256::ZERO);
    }

    // ====================================================================
    // Unjail tests (3.1.3)
    // ====================================================================

    #[test]
    fn unjail_after_cooldown_succeeds() {
        let (_dir, mgr) = setup();
        let validator = addr(1);

        fund_account(&mgr, &validator, wei(100_000));
        mgr.register_validator(validator, [1u8; 32], 500, wei(50_000))
            .unwrap();

        mgr.jail_validator(&validator, JAIL_DURATION_BLOCKS, 1000)
            .unwrap();

        // Unjail after cooldown
        let unjail_block = 1000 + JAIL_DURATION_BLOCKS;
        mgr.unjail(&validator, unjail_block).unwrap();

        let val = mgr.get_validator(&validator).unwrap().unwrap();
        assert_eq!(val.status, ValidatorStatus::Candidate);
        assert_eq!(val.jailed_until, None);
    }

    #[test]
    fn unjail_before_cooldown_rejected() {
        let (_dir, mgr) = setup();
        let validator = addr(1);

        fund_account(&mgr, &validator, wei(100_000));
        mgr.register_validator(validator, [1u8; 32], 500, wei(50_000))
            .unwrap();

        mgr.jail_validator(&validator, JAIL_DURATION_BLOCKS, 1000)
            .unwrap();

        // Try to unjail before cooldown
        let err = mgr.unjail(&validator, 1000 + JAIL_DURATION_BLOCKS - 1).unwrap_err();
        assert!(matches!(err, EconomicsError::UnjailCooldownNotExpired(_)));
    }

    #[test]
    fn unjail_insufficient_stake_rejected() {
        let (_dir, mgr) = setup();
        let validator = addr(1);

        // Register with minimum stake
        fund_account(&mgr, &validator, MIN_SELF_DELEGATION);
        mgr.register_validator(validator, [1u8; 32], 500, MIN_SELF_DELEGATION)
            .unwrap();

        // Slash to bring below minimum, then jail
        mgr.slash(validator, 500, SlashReason::Downtime, 100).unwrap();

        let val = mgr.get_validator(&validator).unwrap().unwrap();
        assert_eq!(val.status, ValidatorStatus::Jailed);

        // Try to unjail — insufficient stake
        let err = mgr
            .unjail(&validator, 100 + JAIL_DURATION_BLOCKS + 1)
            .unwrap_err();
        assert!(matches!(
            err,
            EconomicsError::UnjailInsufficientStake { .. }
        ));
    }

    #[test]
    fn unjail_tombstoned_rejected() {
        let (_dir, mgr) = setup();
        let validator = addr(1);

        fund_account(&mgr, &validator, wei(100_000));
        mgr.register_validator(validator, [1u8; 32], 500, wei(50_000))
            .unwrap();
        mgr.tombstone_validator(&validator).unwrap();

        let err = mgr.unjail(&validator, 999_999).unwrap_err();
        assert!(matches!(err, EconomicsError::ValidatorTombstoned(_)));
    }

    #[test]
    fn full_jail_unjail_lifecycle() {
        let (_dir, mgr) = setup();
        let validator = addr(1);

        fund_account(&mgr, &validator, wei(100_000));
        mgr.register_validator(validator, [1u8; 32], 500, wei(50_000))
            .unwrap();

        // Jail at block 1000
        mgr.jail_validator(&validator, JAIL_DURATION_BLOCKS, 1000)
            .unwrap();
        let val = mgr.get_validator(&validator).unwrap().unwrap();
        assert_eq!(val.status, ValidatorStatus::Jailed);

        // Wait for cooldown, then unjail
        let unjail_block = 1000 + JAIL_DURATION_BLOCKS;
        mgr.unjail(&validator, unjail_block).unwrap();
        let val = mgr.get_validator(&validator).unwrap().unwrap();
        assert_eq!(val.status, ValidatorStatus::Candidate);
        assert_eq!(val.jailed_until, None);
    }

    #[test]
    fn jail_vote_accumulation_and_threshold() {
        let (_dir, mgr) = setup();
        // Create 4 validators with equal stake
        let v1 = addr(1);
        let v2 = addr(2);
        let v3 = addr(3);
        let target = addr(4);

        for v in [v1, v2, v3, target] {
            fund_account(&mgr, &v, wei(100_000));
            mgr.register_validator(v, [v.0[0]; 32], 500, wei(25_000))
                .unwrap();
        }
        // Set all to Active status
        for v in [v1, v2, v3, target] {
            let mut val = mgr.get_validator(&v).unwrap().unwrap();
            val.status = ValidatorStatus::Active;
            mgr.put_validator(&v, &val).unwrap();
        }

        // v1 votes to jail target — not enough (25% < 66.7%)
        let jailed = mgr.record_jail_vote(v1, target, 100).unwrap();
        assert!(!jailed);

        // v2 votes too — still not enough (50% < 66.7%)
        let jailed = mgr.record_jail_vote(v2, target, 101).unwrap();
        assert!(!jailed);

        // v3 votes — threshold reached (75% > 66.7%)
        let jailed = mgr.record_jail_vote(v3, target, 102).unwrap();
        assert!(jailed);

        let val = mgr.get_validator(&target).unwrap().unwrap();
        assert_eq!(val.status, ValidatorStatus::Jailed);
    }

    #[test]
    fn expired_jail_votes_ignored() {
        let (_dir, mgr) = setup();
        let v1 = addr(1);
        let v2 = addr(2);
        let v3 = addr(3);
        let target = addr(4);

        for v in [v1, v2, v3, target] {
            fund_account(&mgr, &v, wei(100_000));
            mgr.register_validator(v, [v.0[0]; 32], 500, wei(25_000))
                .unwrap();
        }
        for v in [v1, v2, v3, target] {
            let mut val = mgr.get_validator(&v).unwrap().unwrap();
            val.status = ValidatorStatus::Active;
            mgr.put_validator(&v, &val).unwrap();
        }

        // v1 votes early
        mgr.record_jail_vote(v1, target, 100).unwrap();
        // v2 votes early
        mgr.record_jail_vote(v2, target, 101).unwrap();

        // Much later (past expiry), v3 votes — old votes expired, not enough
        let late_block = 100 + JAIL_VOTE_EXPIRY_BLOCKS + 1;
        let jailed = mgr.record_jail_vote(v3, target, late_block).unwrap();
        assert!(!jailed);
    }

    #[test]
    fn commission_update_constraints() {
        let (_dir, mgr) = setup();
        let validator = addr(1);

        let big = U256::from(100_000u64) * U256::from(10u64).pow(U256::from(18u64));
        fund_account(&mgr, &validator, big);

        mgr.register_validator(validator, [1u8; 32], 500, MIN_SELF_DELEGATION)
            .unwrap();

        // Valid: change by 100 bps.
        mgr.update_commission(validator, 600, 100).unwrap();
        let val = mgr.get_validator(&validator).unwrap().unwrap();
        assert_eq!(val.commission_bps, 600);

        // Invalid: change by more than 100 bps.
        let result = mgr.update_commission(validator, 800, 100 + COMMISSION_COOLDOWN_BLOCKS);
        assert!(matches!(
            result,
            Err(EconomicsError::CommissionChangeTooLarge { .. })
        ));

        // Invalid: exceeds max.
        let result = mgr.update_commission(validator, 5001, 100 + COMMISSION_COOLDOWN_BLOCKS);
        assert!(matches!(
            result,
            Err(EconomicsError::CommissionTooHigh { .. })
        ));

        // Invalid: cooldown not expired.
        let result = mgr.update_commission(validator, 650, 100 + 1);
        assert!(matches!(
            result,
            Err(EconomicsError::CommissionCooldownNotExpired(..))
        ));

        // Valid: after cooldown.
        mgr.update_commission(validator, 650, 100 + COMMISSION_COOLDOWN_BLOCKS)
            .unwrap();
        let val = mgr.get_validator(&validator).unwrap().unwrap();
        assert_eq!(val.commission_bps, 650);
    }

    // ====================================================================
    // FIX 27: Slash dust rounding — total_delegated recomputed
    // ====================================================================

    #[test]
    fn slash_dust_delegation_total_delegated_stays_consistent() {
        let (_dir, mgr) = setup();
        let validator = addr(1);
        let delegator = addr(2);

        fund_account(&mgr, &validator, wei(100_000));
        fund_account(&mgr, &delegator, wei(100_000));

        mgr.register_validator(validator, [1u8; 32], 500, wei(50_000))
            .unwrap();

        // Delegate a dust amount (1 wei) that will round to zero when slashed
        mgr.delegate(delegator, validator, U256::from(1u64))
            .unwrap();

        // Slash 10 bps (0.1%) — 1 * 10 / 10000 = 0, rounds to zero
        mgr.slash(validator, 10, SlashReason::Downtime, 100)
            .unwrap();

        let val = mgr.get_validator(&validator).unwrap().unwrap();
        let delegations = mgr.delegations_for_validator(&validator).unwrap();
        let sum_delegated: U256 = delegations.iter().map(|d| d.amount).fold(U256::ZERO, |a, b| a + b);

        // total_delegated must match actual sum of delegation amounts
        assert_eq!(val.total_delegated, sum_delegated,
            "total_delegated diverged from sum of delegations after dust slash");
    }

    // ====================================================================
    // FIX 28: top_up_self_stake allows recovery after slash
    // ====================================================================

    #[test]
    fn top_up_self_stake_basic() {
        let (_dir, mgr) = setup();
        let validator = addr(1);

        fund_account(&mgr, &validator, wei(100_000));
        mgr.register_validator(validator, [1u8; 32], 500, wei(50_000))
            .unwrap();

        let top_up = wei(5_000);
        mgr.top_up_self_stake(validator, top_up).unwrap();

        let val = mgr.get_validator(&validator).unwrap().unwrap();
        assert_eq!(val.self_stake, wei(55_000));
    }

    #[test]
    fn top_up_self_stake_while_jailed_then_unjail() {
        let (_dir, mgr) = setup();
        let validator = addr(1);

        // Register with minimum self_stake, extra balance for top-up
        fund_account(&mgr, &validator, wei(100_000));
        mgr.register_validator(validator, [1u8; 32], 500, MIN_SELF_DELEGATION)
            .unwrap();

        // Slash 5% to drop below MIN_SELF_DELEGATION (auto-jails)
        mgr.slash(validator, 500, SlashReason::Downtime, 100)
            .unwrap();
        let val = mgr.get_validator(&validator).unwrap().unwrap();
        assert_eq!(val.status, ValidatorStatus::Jailed);
        assert!(val.self_stake < MIN_SELF_DELEGATION);

        // Top up while jailed to restore self_stake above minimum
        let deficit = MIN_SELF_DELEGATION - val.self_stake + U256::from(1u64);
        mgr.top_up_self_stake(validator, deficit).unwrap();

        let val = mgr.get_validator(&validator).unwrap().unwrap();
        assert!(val.self_stake >= MIN_SELF_DELEGATION);

        // Now unjail should succeed after cooldown
        let unjail_block = 100 + JAIL_DURATION_BLOCKS;
        mgr.unjail(&validator, unjail_block).unwrap();

        let val = mgr.get_validator(&validator).unwrap().unwrap();
        assert_eq!(val.status, ValidatorStatus::Candidate);
    }

    #[test]
    fn top_up_self_stake_zero_is_noop() {
        let (_dir, mgr) = setup();
        let validator = addr(1);

        fund_account(&mgr, &validator, wei(100_000));
        mgr.register_validator(validator, [1u8; 32], 500, wei(50_000))
            .unwrap();

        mgr.top_up_self_stake(validator, U256::ZERO).unwrap();
        let val = mgr.get_validator(&validator).unwrap().unwrap();
        assert_eq!(val.self_stake, wei(50_000));
    }

    #[test]
    fn top_up_self_stake_insufficient_balance() {
        let (_dir, mgr) = setup();
        let validator = addr(1);

        // Fund exactly enough for registration, nothing left over
        fund_account(&mgr, &validator, MIN_SELF_DELEGATION);
        mgr.register_validator(validator, [1u8; 32], 500, MIN_SELF_DELEGATION)
            .unwrap();

        let result = mgr.top_up_self_stake(validator, wei(1));
        assert!(matches!(result, Err(EconomicsError::InsufficientBalance { .. })));
    }

    // ====================================================================
    // FIX 29: Jail vote uses current stake weight
    // ====================================================================

    #[test]
    fn jail_vote_uses_current_stake_not_vote_time() {
        let (_dir, mgr) = setup();
        let v1 = addr(1);
        let v2 = addr(2);
        let v3 = addr(3);
        let target = addr(4);

        for v in [v1, v2, v3, target] {
            fund_account(&mgr, &v, wei(200_000));
            mgr.register_validator(v, [v.0[0]; 32], 500, wei(25_000))
                .unwrap();
        }
        for v in [v1, v2, v3, target] {
            let mut val = mgr.get_validator(&v).unwrap().unwrap();
            val.status = ValidatorStatus::Active;
            mgr.put_validator(&v, &val).unwrap();
        }

        // v1 and v2 vote at blocks 100-101
        mgr.record_jail_vote(v1, target, 100).unwrap();
        mgr.record_jail_vote(v2, target, 101).unwrap();

        // Now slash v1 and v2 heavily, reducing their stake
        mgr.slash(v1, 9000, SlashReason::Downtime, 102).unwrap();
        mgr.slash(v2, 9000, SlashReason::Downtime, 103).unwrap();

        // v3 votes — but with v1+v2 slashed, their current weight is small,
        // so total should NOT reach 2/3 of active stake anymore
        // (v1: 2500, v2: 2500, v3: 25000 = 30000 vs total active ~55000+25000=80000ish)
        // Actually let's verify the behavior by checking the vote passes or not
        // The key point: it uses current stake, not vote-time stake.
        let jailed = mgr.record_jail_vote(v3, target, 104).unwrap();

        // With old code (stored weights): v1(25000) + v2(25000) + v3(25000) = 75000
        //   vs total_active = ~55000 (slashed validators still active).
        //   75000*3 > 55000*2 → would jail
        // With fix (current weights): v1(~2500) + v2(~2500) + v3(25000) = ~30000
        //   vs total_active = ~55000.
        //   30000*3 = 90000 vs 55000*2 = 110000 → should NOT jail
        assert!(!jailed, "jail vote should not pass when voters' stakes were slashed");
    }

    // ====================================================================
    // FIX 30: Unbonding queue length cap
    // ====================================================================

    #[test]
    fn unbonding_queue_cap_enforced() {
        let (_dir, mgr) = setup();
        let validator = addr(1);
        let delegator = addr(2);

        fund_account(&mgr, &validator, wei(100_000));
        fund_account(&mgr, &delegator, wei(100_000));

        mgr.register_validator(validator, [1u8; 32], 500, wei(50_000))
            .unwrap();
        mgr.delegate(delegator, validator, wei(50_000)).unwrap();

        // Fill up unbonding queue to the cap
        let small = U256::from(1u64);
        for i in 0..100u64 {
            mgr.undelegate(delegator, validator, small, 1000 + i)
                .unwrap();
        }

        // 101st should fail
        let result = mgr.undelegate(delegator, validator, small, 2000);
        assert!(
            matches!(result, Err(EconomicsError::TooManyUnbondingEntries { .. })),
            "expected TooManyUnbondingEntries error"
        );
    }

    #[test]
    fn unbonding_queue_cap_allows_after_processing() {
        let (_dir, mgr) = setup();
        let validator = addr(1);
        let delegator = addr(2);

        fund_account(&mgr, &validator, wei(100_000));
        fund_account(&mgr, &delegator, wei(100_000));

        mgr.register_validator(validator, [1u8; 32], 500, wei(50_000))
            .unwrap();
        mgr.delegate(delegator, validator, wei(50_000)).unwrap();

        // Fill up to cap
        let small = U256::from(1u64);
        for i in 0..100u64 {
            mgr.undelegate(delegator, validator, small, 1000 + i)
                .unwrap();
        }

        // Process all matured unbondings (release all of them)
        let release_block = 1100 + UNBONDING_PERIOD;
        mgr.process_unbonding(delegator, validator, release_block)
            .unwrap();

        // Now we should be able to undelegate again
        mgr.undelegate(delegator, validator, small, release_block + 1)
            .unwrap();
    }
}
