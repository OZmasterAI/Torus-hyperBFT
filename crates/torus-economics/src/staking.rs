//! Delegate/undelegate state management (task 1.11.1).
//!
//! All staking state is persisted via `StateDb::put_cf_raw` into the four
//! staking column families defined in `torus-state/src/cf.rs`.

use alloy_primitives::{Address, U256};
use borsh::BorshDeserialize;
use torus_state::cf::{
    CF_STAKING_DELEGATIONS, CF_STAKING_PERMANENT, CF_STAKING_REWARDS, CF_STAKING_VALIDATORS,
};
use torus_state::StateDb;

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

        // Validator must exist and not be tombstoned.
        let mut val = self
            .get_validator(&validator)?
            .ok_or(EconomicsError::ValidatorNotFound(validator))?;
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

    /// Lock tokens permanently. Irreversible.
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

    /// Update validator commission rate. Enforces max 5000 bps and max 100 bps change.
    pub fn update_commission(&self, validator: Address, new_rate: u16) -> Result<()> {
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

        val.commission_bps = new_rate;
        self.put_validator(&validator, &val)?;
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

    /// Read all delegations for a specific validator (full scan, filter by validator).
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

    #[test]
    fn commission_update_constraints() {
        let (_dir, mgr) = setup();
        let validator = addr(1);

        let big = U256::from(100_000u64) * U256::from(10u64).pow(U256::from(18u64));
        fund_account(&mgr, &validator, big);

        mgr.register_validator(validator, [1u8; 32], 500, MIN_SELF_DELEGATION)
            .unwrap();

        // Valid: change by 100 bps.
        mgr.update_commission(validator, 600).unwrap();
        let val = mgr.get_validator(&validator).unwrap().unwrap();
        assert_eq!(val.commission_bps, 600);

        // Invalid: change by more than 100 bps.
        let result = mgr.update_commission(validator, 800);
        assert!(matches!(
            result,
            Err(EconomicsError::CommissionChangeTooLarge { .. })
        ));

        // Invalid: exceeds max.
        let result = mgr.update_commission(validator, 5001);
        assert!(matches!(
            result,
            Err(EconomicsError::CommissionTooHigh { .. })
        ));
    }
}
