//! Delegator reward distribution and fee split logic (tasks 1.11.3, 2.7).

use alloy_primitives::{Address, U256};
use borsh::BorshDeserialize;
use torus_state::cf::CF_TREASURY;
use torus_state::StateBackend;

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
    pub fn distribute_block_fees<T: StateBackend>(
        staking: &StakingManager<T>,
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
    fn distribute_validator_share<T: StateBackend>(
        staking: &StakingManager<T>,
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
    pub fn distribute_permanent_staking_rewards<T: StateBackend>(
        staking: &StakingManager<T>,
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

    /// Distribute validator staking inflation at epoch boundary.
    /// APY = 200 / sqrt(TotalActiveStaked_TRS). Rewards are inflationary (minted).
    /// Only active validators and their delegators receive rewards.
    pub fn distribute_validator_inflation<T: StateBackend>(
        staking: &StakingManager<T>,
        epoch_length_blocks: u64,
    ) -> Result<U256> {
        let all = staking.all_validators()?;
        let mut active: Vec<_> = all
            .into_iter()
            .filter(|v| v.status == crate::types::ValidatorStatus::Active)
            .collect();

        if active.is_empty() {
            return Ok(U256::ZERO);
        }

        // Sort by address ascending for deterministic remainder distribution.
        active.sort_by_key(|v| v.address);

        let total_active_staked: U256 = active.iter().map(|v| v.total_stake()).sum();
        if total_active_staked.is_zero() {
            return Ok(U256::ZERO);
        }

        let sqrt_staked = isqrt(total_active_staked);
        if sqrt_staked.is_zero() {
            return Ok(U256::ZERO);
        }

        let epoch_seconds = epoch_length_blocks * TARGET_BLOCK_TIME_SECS;
        let total_emission = total_active_staked
            * U256::from(VALIDATOR_INFLATION_CONSTANT)
            * U256::from(epoch_seconds)
            / (sqrt_staked * U256::from(SECONDS_PER_YEAR));

        if total_emission.is_zero() {
            return Ok(U256::ZERO);
        }

        let bps_10000 = U256::from(10_000u32);
        let mut distributed = U256::ZERO;
        let last_val_idx = active.len().saturating_sub(1);

        for (i, val) in active.iter().enumerate() {
            let val_emission = if i == last_val_idx {
                total_emission - distributed
            } else {
                total_emission * val.total_stake() / total_active_staked
            };

            if val_emission.is_zero() {
                continue;
            }
            distributed += val_emission;

            // Commission split
            let commission = val_emission * U256::from(val.commission_bps) / bps_10000;
            let delegator_pool = val_emission - commission;

            if !commission.is_zero() {
                staking.credit_rewards(val.address, commission)?;
            }

            // If no delegators, validator gets everything
            if delegator_pool.is_zero() || val.total_delegated.is_zero() {
                if !delegator_pool.is_zero() {
                    staking.credit_rewards(val.address, delegator_pool)?;
                }
                continue;
            }

            // Distribute to delegators pro-rata
            let delegations = staking.delegations_for_validator(&val.address)?;
            let total_delegated = val.total_delegated;
            let mut del_distributed = U256::ZERO;
            let last_del_idx = delegations.len().saturating_sub(1);

            for (j, del) in delegations.iter().enumerate() {
                let share = if j == last_del_idx {
                    delegator_pool - del_distributed
                } else {
                    delegator_pool * del.amount / total_delegated
                };

                if !share.is_zero() {
                    staking.credit_rewards(del.delegator, share)?;
                    del_distributed += share;
                }
            }
        }

        // Update cumulative tracker
        let prev = get_cumulative_validator_inflation(staking)?;
        put_cumulative_validator_inflation(staking, prev + total_emission)?;

        tracing::info!(
            %total_emission,
            active_validators = active.len(),
            %total_active_staked,
            "validator inflation rewards distributed"
        );

        Ok(total_emission)
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

/// Integer square root via Newton's method. Returns floor(sqrt(n)).
pub(crate) fn isqrt(n: U256) -> U256 {
    if n.is_zero() {
        return U256::ZERO;
    }
    if n < U256::from(4u64) {
        return U256::from(1u64);
    }

    // Initial guess: 2^((bits+1)/2)
    let bits = 256 - n.leading_zeros();
    let mut x = U256::from(1u64) << ((bits + 1) / 2);

    loop {
        let x_next = (x + n / x) >> 1;
        if x_next >= x {
            break;
        }
        x = x_next;
    }
    x
}

// ============================================================================
// FeeSplitter (task 2.7)
// ============================================================================

/// Static key for cumulative validator inflation minted in CF_TREASURY.
const VALIDATOR_INFLATION_KEY: &[u8] = b"validator_inflation_tracker";

fn get_cumulative_validator_inflation<T: StateBackend>(staking: &StakingManager<T>) -> Result<U256> {
    match staking
        .state()
        .get_cf_raw(CF_TREASURY, VALIDATOR_INFLATION_KEY)?
    {
        Some(data) => {
            if data.len() != 32 {
                return Err(EconomicsError::Borsh("invalid validator inflation tracker length".into()));
            }
            Ok(U256::from_be_slice(&data))
        }
        None => Ok(U256::ZERO),
    }
}

fn put_cumulative_validator_inflation<T: StateBackend>(staking: &StakingManager<T>, total: U256) -> Result<()> {
    staking
        .state()
        .put_cf_raw(CF_TREASURY, VALIDATOR_INFLATION_KEY, &total.to_be_bytes::<32>())?;
    Ok(())
}

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
    pub fn execute_burn<T: StateBackend>(staking: &StakingManager<T>, amount: U256) -> Result<()> {
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
    pub fn distribute_validator_rewards<T: StateBackend>(
        staking: &StakingManager<T>,
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
    pub fn credit_treasury<T: StateBackend>(
        staking: &StakingManager<T>,
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
    pub fn get_supply_tracker<T: StateBackend>(staking: &StakingManager<T>) -> Result<SupplyTracker> {
        match staking
            .state()
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

    fn put_supply_tracker<T: StateBackend>(staking: &StakingManager<T>, tracker: &SupplyTracker) -> Result<()> {
        let data = borsh::to_vec(tracker).map_err(|e| EconomicsError::Borsh(e.to_string()))?;
        staking
            .state()
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
        mgr.state().put_account(a, &info).unwrap();
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
            .state()
            .get_account(&treasury)
            .unwrap()
            .unwrap()
            .balance;
        let dev_pool_bal = mgr
            .state()
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
            .state()
            .get_account(&staker)
            .unwrap()
            .unwrap()
            .balance;
        assert_eq!(bal, wei(50_000) + minted);
    }

    // ====================================================================
    // isqrt tests
    // ====================================================================

    #[test]
    fn isqrt_basic() {
        assert_eq!(isqrt(U256::ZERO), U256::ZERO);
        assert_eq!(isqrt(U256::from(1u64)), U256::from(1u64));
        assert_eq!(isqrt(U256::from(4u64)), U256::from(2u64));
        assert_eq!(isqrt(U256::from(9u64)), U256::from(3u64));
        assert_eq!(isqrt(U256::from(100u64)), U256::from(10u64));
    }

    #[test]
    fn isqrt_large_u256() {
        // sqrt(10^36) = 10^18 (1M TRS in atrs)
        let val = U256::from(10u64).pow(U256::from(36u64));
        assert_eq!(isqrt(val), U256::from(10u64).pow(U256::from(18u64)));
    }

    #[test]
    fn isqrt_non_perfect() {
        assert_eq!(isqrt(U256::from(2u64)), U256::from(1u64));
        assert_eq!(isqrt(U256::from(3u64)), U256::from(1u64));
        assert_eq!(isqrt(U256::from(5u64)), U256::from(2u64));
        assert_eq!(isqrt(U256::from(99u64)), U256::from(9u64));
    }

    // ====================================================================
    // Validator inflation tests
    // ====================================================================

    fn register_active_validator(
        mgr: &StakingManager,
        n: u8,
        stake_tokens: u64,
        commission_bps: u16,
    ) -> Address {
        let a = addr(n);
        fund(mgr, &a, wei(stake_tokens));
        mgr.register_validator(a, [n; 32], commission_bps, wei(stake_tokens))
            .unwrap();
        // Mark as active
        let mut v = mgr.get_validator(&a).unwrap().unwrap();
        v.status = crate::types::ValidatorStatus::Active;
        mgr.put_validator(&a, &v).unwrap();
        a
    }

    #[test]
    fn validator_inflation_single_validator() {
        let (_dir, mgr) = setup();
        let val = register_active_validator(&mgr, 1, 100_000, 1000); // 10% commission

        let epoch_blocks = 43200u64; // ~1 day at 2s blocks
        let emission =
            RewardDistributor::distribute_validator_inflation(&mgr, epoch_blocks).unwrap();

        assert!(!emission.is_zero(), "should mint some inflation");

        // Verify validator received rewards (commission + delegator pool since no delegators)
        let pending = mgr.get_pending_rewards(&val).unwrap().unwrap();
        assert_eq!(pending.amount, emission, "sole validator gets full emission");
    }

    #[test]
    fn validator_inflation_with_delegators() {
        let (_dir, mgr) = setup();
        let val = register_active_validator(&mgr, 1, 50_000, 1000); // 10% commission
        let d1 = addr(2);
        let d2 = addr(3);
        fund(&mgr, &d1, wei(100_000));
        fund(&mgr, &d2, wei(100_000));
        mgr.delegate(d1, val, wei(30_000)).unwrap();
        mgr.delegate(d2, val, wei(70_000)).unwrap();

        let epoch_blocks = 43200u64;
        let emission =
            RewardDistributor::distribute_validator_inflation(&mgr, epoch_blocks).unwrap();

        let val_rewards = mgr.get_pending_rewards(&val).unwrap().unwrap().amount;
        let d1_rewards = mgr.get_pending_rewards(&d1).unwrap().unwrap().amount;
        let d2_rewards = mgr.get_pending_rewards(&d2).unwrap().unwrap().amount;

        // Commission = emission * 10%
        let bps = U256::from(10_000u32);
        let expected_commission = emission * U256::from(1000u32) / bps;
        assert_eq!(val_rewards, expected_commission);

        // Sum of all rewards = total emission
        assert_eq!(val_rewards + d1_rewards + d2_rewards, emission);

        // d2 gets more than d1 (70k vs 30k delegated)
        assert!(d2_rewards > d1_rewards);
    }

    #[test]
    fn validator_inflation_multiple_validators() {
        let (_dir, mgr) = setup();
        register_active_validator(&mgr, 1, 100_000, 500);
        register_active_validator(&mgr, 2, 200_000, 500);

        let epoch_blocks = 43200u64;
        let emission =
            RewardDistributor::distribute_validator_inflation(&mgr, epoch_blocks).unwrap();

        let r1 = mgr.get_pending_rewards(&addr(1)).unwrap().unwrap().amount;
        let r2 = mgr.get_pending_rewards(&addr(2)).unwrap().unwrap().amount;

        // Sum = total emission
        assert_eq!(r1 + r2, emission);

        // Validator 2 has 2x stake, should get ~2x rewards
        assert!(r2 > r1);
    }

    #[test]
    fn validator_inflation_no_active_validators() {
        let (_dir, mgr) = setup();
        // Register but leave as Candidate (default)
        let a = addr(1);
        fund(&mgr, &a, wei(100_000));
        mgr.register_validator(a, [1u8; 32], 500, wei(50_000))
            .unwrap();

        let emission =
            RewardDistributor::distribute_validator_inflation(&mgr, 43200).unwrap();
        assert_eq!(emission, U256::ZERO);
    }

    #[test]
    fn validator_inflation_zero_stake() {
        let (_dir, mgr) = setup();
        // No validators at all
        let emission =
            RewardDistributor::distribute_validator_inflation(&mgr, 43200).unwrap();
        assert_eq!(emission, U256::ZERO);
    }

    #[test]
    fn validator_inflation_apy_decreases_with_stake() {
        // Higher total stake => lower per-unit reward
        let (_dir1, mgr1) = setup();
        register_active_validator(&mgr1, 1, 100_000, 0);
        let e1 = RewardDistributor::distribute_validator_inflation(&mgr1, 43200).unwrap();

        let (_dir2, mgr2) = setup();
        register_active_validator(&mgr2, 1, 1_000_000, 0);
        let e2 = RewardDistributor::distribute_validator_inflation(&mgr2, 43200).unwrap();

        // Per-unit reward: e1/100k vs e2/1M
        let per_unit_1 = e1 * U256::from(1_000_000u64);
        let per_unit_2 = e2 * U256::from(100_000u64);
        assert!(per_unit_1 > per_unit_2, "APY should decrease with more stake");
    }

    #[test]
    fn validator_inflation_epoch_fraction() {
        let (_dir, mgr) = setup();
        register_active_validator(&mgr, 1, 100_000, 0);

        let e_half = RewardDistributor::distribute_validator_inflation(&mgr, 21600).unwrap();

        let (_dir2, mgr2) = setup();
        register_active_validator(&mgr2, 1, 100_000, 0);
        let e_full = RewardDistributor::distribute_validator_inflation(&mgr2, 43200).unwrap();

        // Double epoch length should produce double emission
        assert_eq!(e_full, e_half * U256::from(2u64));
    }

    #[test]
    fn validator_inflation_cumulative_tracker() {
        let (_dir, mgr) = setup();
        register_active_validator(&mgr, 1, 100_000, 0);

        let e1 = RewardDistributor::distribute_validator_inflation(&mgr, 43200).unwrap();
        let cum1 = get_cumulative_validator_inflation(&mgr).unwrap();
        assert_eq!(cum1, e1);

        // Second epoch
        let e2 = RewardDistributor::distribute_validator_inflation(&mgr, 43200).unwrap();
        let cum2 = get_cumulative_validator_inflation(&mgr).unwrap();
        assert_eq!(cum2, e1 + e2);
    }
}
