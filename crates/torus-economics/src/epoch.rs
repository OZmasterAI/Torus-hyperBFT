//! Epoch-based validator set rotation (task 1.11.2).

use alloy_primitives::{Address, B256, U256};
use sha3::{Digest, Keccak256};
use torus_types::{PublicKey, ValidatorInfo, ValidatorSet};

use crate::staking::StakingManager;
use crate::types::ValidatorStatus;
use crate::EconomicsError;

type Result<T> = std::result::Result<T, EconomicsError>;

/// Manages epoch transitions and validator set computation.
pub struct EpochManager;

impl EpochManager {
    /// Returns true if `block_height` is an epoch boundary.
    pub fn is_epoch_boundary(block_height: u64, epoch_length: u64) -> bool {
        if epoch_length == 0 || block_height == 0 {
            return false;
        }
        block_height.is_multiple_of(epoch_length)
    }

    /// Compute the epoch number for a given block height.
    pub fn epoch_for_block(block_height: u64, epoch_length: u64) -> u64 {
        if epoch_length == 0 {
            return 0;
        }
        block_height / epoch_length
    }

    /// Read all validators, rank by total stake, return top `max_validators` as the new set.
    pub fn compute_new_validator_set(
        staking: &StakingManager,
        max_validators: u32,
        epoch: u64,
    ) -> Result<ValidatorSet> {
        let all = staking.all_validators()?;

        let mut eligible: Vec<_> = all
            .into_iter()
            .filter(|v| {
                v.status == ValidatorStatus::Active || v.status == ValidatorStatus::Candidate
            })
            .collect();

        // Sort by total_stake descending, address ascending for determinism.
        eligible.sort_by(|a, b| {
            b.total_stake()
                .cmp(&a.total_stake())
                .then_with(|| a.address.cmp(&b.address))
        });

        eligible.truncate(max_validators as usize);

        let wei = U256::from(10u64).pow(U256::from(18u64));
        let validators: Vec<ValidatorInfo> = eligible
            .iter()
            .map(|v| ValidatorInfo {
                address: v.address,
                pubkey: PublicKey(v.pubkey),
                power: (v.total_stake() / wei).try_into().unwrap_or(u64::MAX),
                commission_bps: v.commission_bps,
            })
            .collect();

        Ok(ValidatorSet { validators, epoch })
    }

    /// Deterministic hash: `keccak256(epoch ++ sorted_validators)`.
    pub fn validator_set_hash(set: &ValidatorSet) -> B256 {
        let mut data = Vec::new();
        data.extend_from_slice(&set.epoch.to_be_bytes());
        for v in &set.validators {
            data.extend_from_slice(v.address.as_slice());
            data.extend_from_slice(&v.pubkey.0);
            data.extend_from_slice(&v.power.to_be_bytes());
            data.extend_from_slice(&v.commission_bps.to_be_bytes());
        }
        B256::new(Keccak256::digest(&data).into())
    }

    /// Diff old vs new validator sets for hotstuff_rs ValidatorSetUpdates.
    pub fn compute_validator_set_diff(
        old_set: &ValidatorSet,
        new_set: &ValidatorSet,
    ) -> ValidatorSetDiff {
        use std::collections::HashMap;

        let old_map: HashMap<Address, &ValidatorInfo> =
            old_set.validators.iter().map(|v| (v.address, v)).collect();
        let new_map: HashMap<Address, &ValidatorInfo> =
            new_set.validators.iter().map(|v| (v.address, v)).collect();

        let mut inserts = Vec::new();
        let mut deletes = Vec::new();

        for (addr, new_v) in &new_map {
            match old_map.get(addr) {
                Some(old_v) if old_v.power == new_v.power => {}
                _ => inserts.push((*new_v).clone()),
            }
        }

        for addr in old_map.keys() {
            if !new_map.contains_key(addr) {
                deletes.push(*addr);
            }
        }

        ValidatorSetDiff { inserts, deletes }
    }

    /// Update validator statuses after epoch rotation.
    pub fn update_validator_statuses(
        staking: &StakingManager,
        new_set: &ValidatorSet,
    ) -> Result<()> {
        let active_addrs: std::collections::HashSet<Address> =
            new_set.validators.iter().map(|v| v.address).collect();

        for mut val in staking.all_validators()? {
            if val.status == ValidatorStatus::Jailed || val.status == ValidatorStatus::Tombstoned {
                continue;
            }
            let new_status = if active_addrs.contains(&val.address) {
                ValidatorStatus::Active
            } else {
                ValidatorStatus::Candidate
            };
            if val.status != new_status {
                val.status = new_status;
                staking.put_validator(&val.address, &val)?;
            }
        }
        Ok(())
    }
}

/// Result of diffing two validator sets.
pub struct ValidatorSetDiff {
    pub inserts: Vec<ValidatorInfo>,
    pub deletes: Vec<Address>,
}

impl ValidatorSetDiff {
    pub fn is_empty(&self) -> bool {
        self.inserts.is_empty() && self.deletes.is_empty()
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

    fn fund_and_register(mgr: &StakingManager, n: u8, stake_tokens: u64) -> Address {
        let addr = Address::new([n; 20]);
        let stake = U256::from(stake_tokens) * U256::from(10u64).pow(U256::from(18u64));
        let info = AccountInfo {
            balance: stake,
            ..Default::default()
        };
        mgr.state_db().put_account(&addr, &info).unwrap();
        mgr.register_validator(addr, [n; 32], 500, stake).unwrap();
        addr
    }

    #[test]
    fn epoch_boundary_detection() {
        assert!(!EpochManager::is_epoch_boundary(0, 100));
        assert!(!EpochManager::is_epoch_boundary(50, 100));
        assert!(EpochManager::is_epoch_boundary(100, 100));
        assert!(EpochManager::is_epoch_boundary(200, 100));
        assert!(!EpochManager::is_epoch_boundary(150, 100));
        assert!(!EpochManager::is_epoch_boundary(100, 0));

        assert_eq!(EpochManager::epoch_for_block(0, 100), 0);
        assert_eq!(EpochManager::epoch_for_block(99, 100), 0);
        assert_eq!(EpochManager::epoch_for_block(100, 100), 1);
        assert_eq!(EpochManager::epoch_for_block(250, 100), 2);
    }

    #[test]
    fn validator_ranking_by_stake() {
        let (_dir, mgr) = setup();
        fund_and_register(&mgr, 1, 50_000);
        fund_and_register(&mgr, 2, 100_000);
        fund_and_register(&mgr, 3, 30_000);
        fund_and_register(&mgr, 4, 80_000);

        let set = EpochManager::compute_new_validator_set(&mgr, 2, 1).unwrap();
        assert_eq!(set.validators.len(), 2);
        assert_eq!(set.validators[0].address, Address::new([2; 20]));
        assert_eq!(set.validators[1].address, Address::new([4; 20]));
        assert_eq!(set.epoch, 1);
    }

    #[test]
    fn validator_set_hash_deterministic() {
        let set = ValidatorSet {
            validators: vec![
                ValidatorInfo {
                    address: Address::new([1; 20]),
                    pubkey: PublicKey([1; 32]),
                    power: 100,
                    commission_bps: 500,
                },
                ValidatorInfo {
                    address: Address::new([2; 20]),
                    pubkey: PublicKey([2; 32]),
                    power: 200,
                    commission_bps: 300,
                },
            ],
            epoch: 5,
        };
        let h1 = EpochManager::validator_set_hash(&set);
        let h2 = EpochManager::validator_set_hash(&set);
        assert_eq!(h1, h2);
        assert_ne!(h1, B256::ZERO);
    }

    #[test]
    fn validator_set_diff_computation() {
        let old = ValidatorSet {
            validators: vec![
                ValidatorInfo {
                    address: Address::new([1; 20]),
                    pubkey: PublicKey([1; 32]),
                    power: 100,
                    commission_bps: 500,
                },
                ValidatorInfo {
                    address: Address::new([2; 20]),
                    pubkey: PublicKey([2; 32]),
                    power: 200,
                    commission_bps: 300,
                },
            ],
            epoch: 1,
        };
        let new = ValidatorSet {
            validators: vec![
                ValidatorInfo {
                    address: Address::new([2; 20]),
                    pubkey: PublicKey([2; 32]),
                    power: 250,
                    commission_bps: 300,
                },
                ValidatorInfo {
                    address: Address::new([3; 20]),
                    pubkey: PublicKey([3; 32]),
                    power: 150,
                    commission_bps: 400,
                },
            ],
            epoch: 2,
        };
        let diff = EpochManager::compute_validator_set_diff(&old, &new);
        assert_eq!(diff.inserts.len(), 2);
        assert_eq!(diff.deletes.len(), 1);
        assert_eq!(diff.deletes[0], Address::new([1; 20]));
    }
}
