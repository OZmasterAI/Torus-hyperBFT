//! Delegator reward distribution and fee split logic (tasks 1.11.3, 2.7).

use alloy_primitives::{Address, U256};
use borsh::BorshDeserialize;
use torus_state::cf::CF_TREASURY;

use crate::staking::StakingManager;
use crate::types::*;
use crate::EconomicsError;

type Result<T> = std::result::Result<T, EconomicsError>;

/// Distributes block fees and permanent staking rewards.
pub struct RewardDistributor;

impl RewardDistributor {
    /// Distribute block fees according to the fee split schedule.
    ///
    /// 1. Compute BPS ratios via linear interpolation from start to end over transition_epochs.
    /// 2. Burn portion is destroyed (no-op).
    /// 3. Validator share goes to proposer's delegators pro-rata (minus commission).
    /// 4. Treasury and dev_pool credited to configured addresses.
    pub fn distribute_block_fees(
        staking: &StakingManager,
        proposer: Address,
        total_fees: U256,
        epoch: u64,
        treasury_address: Address,
        dev_pool_address: Address,
    ) -> Result<()> {
        if total_fees.is_zero() {
            return Ok(());
        }

        let bps_10000 = U256::from(10_000u32);

        let burn_bps = lerp_bps(
            FEE_START_BURN_BPS,
            FEE_END_BURN_BPS,
            epoch,
            TRANSITION_EPOCHS,
        );
        let validator_bps = lerp_bps(
            FEE_START_VALIDATOR_BPS,
            FEE_END_VALIDATOR_BPS,
            epoch,
            TRANSITION_EPOCHS,
        );
        let treasury_bps = lerp_bps(
            FEE_START_TREASURY_BPS,
            FEE_END_TREASURY_BPS,
            epoch,
            TRANSITION_EPOCHS,
        );

        let burn = total_fees * U256::from(burn_bps) / bps_10000;
        let validator_share = total_fees * U256::from(validator_bps) / bps_10000;
        let treasury = total_fees * U256::from(treasury_bps) / bps_10000;
        let dev_pool = total_fees - burn - validator_share - treasury;

        if !validator_share.is_zero() {
            Self::distribute_validator_share(staking, &proposer, validator_share)?;
        }

        if !treasury.is_zero() {
            staking.credit_balance(&treasury_address, treasury)?;
        }

        if !dev_pool.is_zero() {
            staking.credit_balance(&dev_pool_address, dev_pool)?;
        }

        Ok(())
    }

    /// Distribute the validator's fee share to their delegators pro-rata.
    fn distribute_validator_share(
        staking: &StakingManager,
        proposer: &Address,
        validator_share: U256,
    ) -> Result<()> {
        let val = match staking.get_validator(proposer)? {
            Some(v) => v,
            None => {
                staking.credit_balance(proposer, validator_share)?;
                return Ok(());
            }
        };

        let bps_10000 = U256::from(10_000u32);
        let commission = validator_share * U256::from(val.commission_bps) / bps_10000;
        let delegator_pool = validator_share - commission;

        if !commission.is_zero() {
            staking.credit_rewards(*proposer, commission)?;
        }

        if delegator_pool.is_zero() || val.total_delegated.is_zero() {
            if !delegator_pool.is_zero() {
                staking.credit_rewards(*proposer, delegator_pool)?;
            }
            return Ok(());
        }

        let delegations = staking.delegations_for_validator(proposer)?;
        let total_delegated = val.total_delegated;

        let mut distributed = U256::ZERO;
        let last_idx = delegations.len().saturating_sub(1);

        for (i, del) in delegations.iter().enumerate() {
            let share = if i == last_idx {
                delegator_pool - distributed
            } else {
                delegator_pool * del.amount / total_delegated
            };

            if !share.is_zero() {
                staking.credit_rewards(del.delegator, share)?;
                distributed += share;
            }
        }

        Ok(())
    }

    /// Distribute permanent staking rewards at epoch boundary.
    /// Rate = 500 bps (5% APY). Rewards are inflationary (minted).
    pub fn distribute_permanent_staking_rewards(
        staking: &StakingManager,
        blocks_in_epoch: u64,
    ) -> Result<U256> {
        let all_stakes = staking.all_permanent_stakes()?;
        if all_stakes.is_empty() {
            return Ok(U256::ZERO);
        }

        let mut total_minted = U256::ZERO;

        for stake_info in &all_stakes {
            if stake_info.amount.is_zero() {
                continue;
            }

            // reward = stake_amount * APY_BPS * blocks_in_epoch / (BLOCKS_PER_YEAR * 10000)
            let reward = stake_info.amount
                * U256::from(PERMANENT_STAKE_APY_BPS)
                * U256::from(blocks_in_epoch)
                / (U256::from(BLOCKS_PER_YEAR) * U256::from(10_000u64));

            if !reward.is_zero() {
                staking.credit_balance(&stake_info.staker, reward)?;
                total_minted += reward;
            }
        }

        tracing::info!(%total_minted, stakers = all_stakes.len(), "permanent staking rewards distributed");
        Ok(total_minted)
    }
}

/// Integer-only linear interpolation in basis points.
pub fn lerp_bps(start: u16, end: u16, numerator: u64, denominator: u64) -> u16 {
    if denominator == 0 || numerator >= denominator {
        return end;
    }
    if start <= end {
        let delta = (end - start) as u64;
        start + ((delta * numerator) / denominator) as u16
    } else {
        let delta = (start - end) as u64;
        start - ((delta * numerator) / denominator) as u16
    }
}

// ============================================================================
// FeeSplitter (task 2.7)
// ============================================================================

/// Static key for the supply tracker in CF_TREASURY.
const SUPPLY_TRACKER_KEY: &[u8] = b"supply_tracker";

/// Splits block fees into burn/validators/treasury/dev_pool with BPS interpolation,
/// executes burn, credits treasury, and distributes validator rewards with commission.
pub struct FeeSplitter;

impl FeeSplitter {
    /// Split total fees into four buckets using interpolated BPS ratios.
    /// burn + validators + treasury are computed via BPS; dev_pool gets the remainder
    /// so the sum is exactly `total_fees` (no rounding loss).
    pub fn split_fees(total_fees: U256, epoch: u64) -> FeeSplitResult {
        if total_fees.is_zero() {
            return FeeSplitResult {
                burn: U256::ZERO,
                validators: U256::ZERO,
                treasury: U256::ZERO,
                dev_pool: U256::ZERO,
            };
        }

        let bps_10000 = U256::from(10_000u32);

        let burn_bps = lerp_bps(FEE_START_BURN_BPS, FEE_END_BURN_BPS, epoch, TRANSITION_EPOCHS);
        let validator_bps = lerp_bps(
            FEE_START_VALIDATOR_BPS,
            FEE_END_VALIDATOR_BPS,
            epoch,
            TRANSITION_EPOCHS,
        );
        let treasury_bps = lerp_bps(
            FEE_START_TREASURY_BPS,
            FEE_END_TREASURY_BPS,
            epoch,
            TRANSITION_EPOCHS,
        );

        let burn = total_fees * U256::from(burn_bps) / bps_10000;
        let validators = total_fees * U256::from(validator_bps) / bps_10000;
        let treasury = total_fees * U256::from(treasury_bps) / bps_10000;
        let dev_pool = total_fees - burn - validators - treasury;

        FeeSplitResult {
            burn,
            validators,
            treasury,
            dev_pool,
        }
    }

    /// Burn tokens by deducting from the cumulative supply tracker.
    /// No balance is credited anywhere — tokens are destroyed.
    pub fn execute_burn(staking: &StakingManager, amount: U256) -> Result<()> {
        if amount.is_zero() {
            return Ok(());
        }
        let mut tracker = Self::get_supply_tracker(staking)?;
        tracker.cumulative_burned += amount;
        Self::put_supply_tracker(staking, &tracker)?;
        tracing::info!(%amount, cumulative = %tracker.cumulative_burned, "tokens burned");
        Ok(())
    }

    /// Distribute the validator portion of fees to the block proposer.
    /// Proposer keeps commission_rate; remaining goes to delegators pro-rata.
    pub fn distribute_validator_rewards(
        staking: &StakingManager,
        proposer: &Address,
        amount: U256,
    ) -> Result<()> {
        if amount.is_zero() {
            return Ok(());
        }

        let val = match staking.get_validator(proposer)? {
            Some(v) => v,
            None => {
                // No validator record — credit full amount to proposer balance.
                staking.credit_balance(proposer, amount)?;
                return Ok(());
            }
        };

        let bps_10000 = U256::from(10_000u32);
        let commission = amount * U256::from(val.commission_bps) / bps_10000;
        let delegator_pool = amount - commission;

        // Proposer keeps commission.
        if !commission.is_zero() {
            staking.credit_rewards(*proposer, commission)?;
        }

        // If no delegators, proposer gets everything.
        if delegator_pool.is_zero() || val.total_delegated.is_zero() {
            if !delegator_pool.is_zero() {
                staking.credit_rewards(*proposer, delegator_pool)?;
            }
            return Ok(());
        }

        let delegations = staking.delegations_for_validator(proposer)?;
        let total_delegated = val.total_delegated;

        let mut distributed = U256::ZERO;
        let last_idx = delegations.len().saturating_sub(1);

        for (i, del) in delegations.iter().enumerate() {
            let share = if i == last_idx {
                // Last delegator gets remainder to avoid rounding dust.
                delegator_pool - distributed
            } else {
                delegator_pool * del.amount / total_delegated
            };
            if !share.is_zero() {
                staking.credit_rewards(del.delegator, share)?;
                distributed += share;
            }
        }

        Ok(())
    }

    /// Credit treasury address and update cumulative tracker.
    pub fn credit_treasury(
        staking: &StakingManager,
        treasury_address: &Address,
        amount: U256,
    ) -> Result<()> {
        if amount.is_zero() {
            return Ok(());
        }
        staking.credit_balance(treasury_address, amount)?;
        let mut tracker = Self::get_supply_tracker(staking)?;
        tracker.cumulative_treasury += amount;
        Self::put_supply_tracker(staking, &tracker)?;
        Ok(())
    }

    /// Get the supply tracker from CF_TREASURY.
    pub fn get_supply_tracker(staking: &StakingManager) -> Result<SupplyTracker> {
        match staking
            .state_db()
            .get_cf_raw(CF_TREASURY, SUPPLY_TRACKER_KEY)?
        {
            Some(data) => Ok(SupplyTracker::try_from_slice(&data)
                .map_err(|e| EconomicsError::Borsh(e.to_string()))?),
            None => Ok(SupplyTracker {
                cumulative_burned: U256::ZERO,
                cumulative_treasury: U256::ZERO,
            }),
        }
    }

    fn put_supply_tracker(staking: &StakingManager, tracker: &SupplyTracker) -> Result<()> {
        let data = borsh::to_vec(tracker).map_err(|e| EconomicsError::Borsh(e.to_string()))?;
        staking
            .state_db()
            .put_cf_raw(CF_TREASURY, SUPPLY_TRACKER_KEY, &data)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::staking::StakingManager;
    use revm::state::AccountInfo;
    use torus_state::StateDb;

    fn setup() -> (tempfile::TempDir, StakingManager) {
        let dir = tempfile::tempdir().unwrap();
        let db = StateDb::open(dir.path()).unwrap();
        (dir, StakingManager::new(db))
    }

    fn addr(n: u8) -> Address {
        Address::new([n; 20])
    }

    fn fund(mgr: &StakingManager, a: &Address, amount: U256) {
        let info = AccountInfo {
            balance: amount,
            ..Default::default()
        };
        mgr.state_db().put_account(a, &info).unwrap();
    }

    fn wei(tokens: u64) -> U256 {
        U256::from(tokens) * U256::from(10u64).pow(U256::from(18u64))
    }

    #[test]
    fn lerp_bps_at_epoch_0() {
        assert_eq!(
            lerp_bps(FEE_START_BURN_BPS, FEE_END_BURN_BPS, 0, TRANSITION_EPOCHS),
            1000
        );
        assert_eq!(
            lerp_bps(
                FEE_START_VALIDATOR_BPS,
                FEE_END_VALIDATOR_BPS,
                0,
                TRANSITION_EPOCHS
            ),
            0
        );
        assert_eq!(
            lerp_bps(
                FEE_START_TREASURY_BPS,
                FEE_END_TREASURY_BPS,
                0,
                TRANSITION_EPOCHS
            ),
            4500
        );
    }

    #[test]
    fn lerp_bps_at_transition() {
        assert_eq!(
            lerp_bps(
                FEE_START_BURN_BPS,
                FEE_END_BURN_BPS,
                TRANSITION_EPOCHS,
                TRANSITION_EPOCHS
            ),
            2500
        );
        assert_eq!(
            lerp_bps(
                FEE_START_VALIDATOR_BPS,
                FEE_END_VALIDATOR_BPS,
                TRANSITION_EPOCHS,
                TRANSITION_EPOCHS
            ),
            2500
        );
        assert_eq!(
            lerp_bps(
                FEE_START_TREASURY_BPS,
                FEE_END_TREASURY_BPS,
                TRANSITION_EPOCHS,
                TRANSITION_EPOCHS
            ),
            2500
        );
    }

    #[test]
    fn lerp_bps_midpoint() {
        let mid = TRANSITION_EPOCHS / 2;
        let burn = lerp_bps(1000, 2500, mid, TRANSITION_EPOCHS);
        // 1000 + (1500 * 912 / 1825) = 1000 + 749 = 1749
        assert!(burn >= 1749 && burn <= 1751, "burn at midpoint = {burn}");
    }

    #[test]
    fn fee_split_epoch_0() {
        let (_dir, mgr) = setup();
        let proposer = addr(1);
        let treasury = addr(10);
        let dev_pool = addr(11);

        fund(&mgr, &proposer, wei(100_000));
        mgr.register_validator(proposer, [1u8; 32], 500, wei(10_000))
            .unwrap();

        let total_fees = wei(100);
        // Epoch 0: 10% burn, 0% validator, 45% treasury, 45% dev_pool.
        RewardDistributor::distribute_block_fees(&mgr, proposer, total_fees, 0, treasury, dev_pool)
            .unwrap();

        let treasury_bal = mgr
            .state_db()
            .get_account(&treasury)
            .unwrap()
            .unwrap()
            .balance;
        let dev_pool_bal = mgr
            .state_db()
            .get_account(&dev_pool)
            .unwrap()
            .unwrap()
            .balance;

        assert_eq!(treasury_bal, wei(45));
        assert_eq!(dev_pool_bal, wei(45));
    }

    #[test]
    fn delegator_pro_rata_with_commission() {
        let (_dir, mgr) = setup();
        let proposer = addr(1);
        let d1 = addr(2);
        let d2 = addr(3);
        let treasury = addr(10);
        let dev_pool = addr(11);

        fund(&mgr, &proposer, wei(100_000));
        fund(&mgr, &d1, wei(100_000));
        fund(&mgr, &d2, wei(100_000));

        mgr.register_validator(proposer, [1u8; 32], 1000, wei(10_000))
            .unwrap(); // 10% commission
        mgr.delegate(d1, proposer, wei(30_000)).unwrap();
        mgr.delegate(d2, proposer, wei(70_000)).unwrap();

        // At transition end: validator gets 25% of fees.
        let total_fees = wei(10_000);
        RewardDistributor::distribute_block_fees(
            &mgr,
            proposer,
            total_fees,
            TRANSITION_EPOCHS,
            treasury,
            dev_pool,
        )
        .unwrap();

        // Validator share = 2500. Commission = 250 → proposer.
        // Delegator pool = 2250. d1: 675, d2: 1575.
        let r1 = mgr.get_pending_rewards(&d1).unwrap().unwrap();
        let r2 = mgr.get_pending_rewards(&d2).unwrap().unwrap();
        let rv = mgr.get_pending_rewards(&proposer).unwrap().unwrap();

        assert_eq!(r1.amount, wei(675));
        assert_eq!(r2.amount, wei(1575));
        assert_eq!(rv.amount, wei(250));
    }

    #[test]
    fn permanent_staking_rewards() {
        let (_dir, mgr) = setup();
        let staker = addr(5);
        fund(&mgr, &staker, wei(100_000));
        mgr.permanent_stake(staker, wei(50_000), 0).unwrap();

        let blocks_in_epoch = 100_000u64;
        let minted =
            RewardDistributor::distribute_permanent_staking_rewards(&mgr, blocks_in_epoch).unwrap();

        let expected = wei(50_000) * U256::from(500u64) * U256::from(100_000u64)
            / (U256::from(BLOCKS_PER_YEAR) * U256::from(10_000u64));
        assert_eq!(minted, expected);
        assert!(!minted.is_zero());

        // Staker balance = 50k remaining + minted rewards.
        let bal = mgr
            .state_db()
            .get_account(&staker)
            .unwrap()
            .unwrap()
            .balance;
        assert_eq!(bal, wei(50_000) + minted);
    }
}
