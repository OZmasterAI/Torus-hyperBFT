//! Staking query functions for RPC wiring (task 1.11.4).
//!
//! These are standalone pub fns that will be called from torus-rpc once
//! jsonrpsee is set up (task 1.8). No jsonrpsee dependency here.

use alloy_primitives::{Address, U256};
use torus_types::{PublicKey, ValidatorInfo};

use crate::epoch::EpochManager;
use crate::staking::StakingManager;
use crate::types::{EpochInfo, StakingInfo};
use crate::EconomicsError;

type Result<T> = std::result::Result<T, EconomicsError>;

/// Get all validators (for `torus_getValidators` RPC).
pub fn get_validators(staking: &StakingManager) -> Result<Vec<ValidatorInfo>> {
    let all = staking.all_validators()?;
    let wei = U256::from(10u64).pow(U256::from(18u64));

    Ok(all
        .into_iter()
        .map(|v| ValidatorInfo {
            address: v.address,
            pubkey: PublicKey(v.pubkey),
            power: (v.total_stake() / wei).try_into().unwrap_or(u64::MAX),
            commission_bps: v.commission_bps,
        })
        .collect())
}

/// Get epoch info (for `torus_getEpoch` RPC).
pub fn get_epoch_info(current_height: u64, epoch_length: u64) -> EpochInfo {
    let current_epoch = EpochManager::epoch_for_block(current_height, epoch_length);
    let epoch_start_block = current_epoch * epoch_length;
    let epoch_end_block = epoch_start_block + epoch_length;
    let blocks_remaining = epoch_end_block.saturating_sub(current_height);

    EpochInfo {
        current_epoch,
        epoch_start_block,
        epoch_end_block,
        blocks_remaining,
        epoch_length,
    }
}

/// Get staking info for a single address (for `torus_getStakingInfo` RPC).
pub fn get_staking_info(staking: &StakingManager, address: Address) -> Result<StakingInfo> {
    let delegations = staking.delegations_for_delegator(&address)?;

    let delegated: Vec<(Address, U256)> = delegations
        .iter()
        .map(|d| (d.validator, d.amount))
        .collect();

    let unbonding = delegations.into_iter().flat_map(|d| d.unbonding).collect();

    let permanent_stake = staking
        .get_permanent_stake(&address)?
        .map(|p| p.amount)
        .unwrap_or(U256::ZERO);

    let pending_rewards = staking
        .get_pending_rewards(&address)?
        .map(|r| r.amount)
        .unwrap_or(U256::ZERO);

    Ok(StakingInfo {
        delegated,
        permanent_stake,
        pending_rewards,
        unbonding,
    })
}

/// Get all delegations for a delegator (for `torus_getDelegations` RPC).
pub fn get_delegations(
    staking: &StakingManager,
    delegator: Address,
) -> Result<Vec<(Address, U256)>> {
    let delegations = staking.delegations_for_delegator(&delegator)?;
    Ok(delegations
        .iter()
        .map(|d| (d.validator, d.amount))
        .collect())
}

/// Get all delegations to a specific validator.
pub fn get_validator_delegations(
    staking: &StakingManager,
    validator: Address,
) -> Result<Vec<(Address, U256)>> {
    let delegations = staking.delegations_for_validator(&validator)?;
    Ok(delegations
        .iter()
        .map(|d| (d.delegator, d.amount))
        .collect())
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

    fn wei(tokens: u64) -> U256 {
        U256::from(tokens) * U256::from(10u64).pow(U256::from(18u64))
    }

    fn fund(mgr: &StakingManager, a: &Address, amount: U256) {
        let info = AccountInfo {
            balance: amount,
            ..Default::default()
        };
        mgr.state().put_account(a, &info).unwrap();
    }

    #[test]
    fn epoch_info_computation() {
        let info = get_epoch_info(250, 100);
        assert_eq!(info.current_epoch, 2);
        assert_eq!(info.epoch_start_block, 200);
        assert_eq!(info.epoch_end_block, 300);
        assert_eq!(info.blocks_remaining, 50);
        assert_eq!(info.epoch_length, 100);
    }

    #[test]
    fn query_validators_after_registration() {
        let (_dir, mgr) = setup();
        let v1 = addr(1);
        let v2 = addr(2);

        fund(&mgr, &v1, wei(50_000));
        fund(&mgr, &v2, wei(100_000));

        mgr.register_validator(v1, [1u8; 32], 500, wei(50_000))
            .unwrap();
        mgr.register_validator(v2, [2u8; 32], 300, wei(100_000))
            .unwrap();

        let validators = get_validators(&mgr).unwrap();
        assert_eq!(validators.len(), 2);
    }

    #[test]
    fn query_staking_info_after_mutations() {
        let (_dir, mgr) = setup();
        let validator = addr(1);
        let delegator = addr(2);

        fund(&mgr, &validator, wei(100_000));
        fund(&mgr, &delegator, wei(100_000));

        mgr.register_validator(validator, [1u8; 32], 500, wei(10_000))
            .unwrap();
        mgr.delegate(delegator, validator, wei(5_000)).unwrap();
        mgr.permanent_stake(delegator, wei(3_000), 100).unwrap();
        mgr.credit_rewards(delegator, wei(200)).unwrap();

        let info = get_staking_info(&mgr, delegator).unwrap();
        assert_eq!(info.delegated.len(), 1);
        assert_eq!(info.delegated[0], (validator, wei(5_000)));
        assert_eq!(info.permanent_stake, wei(3_000));
        assert_eq!(info.pending_rewards, wei(200));
        assert!(info.unbonding.is_empty());
    }

    #[test]
    fn query_delegations_round_trip() {
        let (_dir, mgr) = setup();
        let v1 = addr(1);
        let v2 = addr(2);
        let delegator = addr(3);

        fund(&mgr, &v1, wei(50_000));
        fund(&mgr, &v2, wei(50_000));
        fund(&mgr, &delegator, wei(100_000));

        mgr.register_validator(v1, [1u8; 32], 500, wei(10_000))
            .unwrap();
        mgr.register_validator(v2, [2u8; 32], 500, wei(10_000))
            .unwrap();
        mgr.delegate(delegator, v1, wei(20_000)).unwrap();
        mgr.delegate(delegator, v2, wei(30_000)).unwrap();

        let dels = get_delegations(&mgr, delegator).unwrap();
        assert_eq!(dels.len(), 2);

        // Check total.
        let total: U256 = dels.iter().map(|(_, amt)| *amt).sum();
        assert_eq!(total, wei(50_000));

        // Check validator delegations.
        let v1_dels = get_validator_delegations(&mgr, v1).unwrap();
        assert_eq!(v1_dels.len(), 1);
        assert_eq!(v1_dels[0], (delegator, wei(20_000)));
    }
}
