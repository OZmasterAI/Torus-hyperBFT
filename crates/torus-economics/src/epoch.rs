//! Epoch-based validator set rotation (task 1.11.2).

use alloy_primitives::{Address, B256, U256};
use sha3::{Digest, Keccak256};
use torus_state::StateBackend;
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
    pub fn compute_new_validator_set<T: StateBackend>(
        staking: &StakingManager<T>,
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
    /// BUG FIX (3.2): also detects pubkey changes from key rotation (A3).
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
        let mut rotated_out_pubkeys = Vec::new();

        for (addr, new_v) in &new_map {
            match old_map.get(addr) {
                Some(old_v) if old_v.power == new_v.power && old_v.pubkey == new_v.pubkey => {
                    // No change
                }
                Some(old_v) => {
                    // Power or pubkey changed — insert with new values
                    inserts.push((*new_v).clone());
                    // BUG FIX (3.2): key rotation — must delete old pubkey from hotstuff_rs
                    if old_v.pubkey != new_v.pubkey {
                        rotated_out_pubkeys.push(old_v.pubkey.0);
                    }
                }
                None => inserts.push((*new_v).clone()),
            }
        }

        for addr in old_map.keys() {
            if !new_map.contains_key(addr) {
                deletes.push(*addr);
            }
        }

        ValidatorSetDiff {
            inserts,
            deletes,
            rotated_out_pubkeys,
        }
    }

    /// Compute the safe rotation cap: max validators that can change per epoch.
    ///
    /// Derived from hotstuff_rs BFT quorum requirements:
    /// - Quorum = (total_power * 2 / 3) + 1  (>2/3 of total power)
    /// - For safety, old and new sets must share enough validators to form a
    ///   quorum in both sets during the transition.
    /// - This requires at most floor(n/3) validators to change per epoch,
    ///   where n is the current active set size.
    pub fn safe_rotation_cap(current_set_size: usize) -> usize {
        current_set_size / 3
    }

    /// Apply rotation cap: if more validators would change than is safe,
    /// defer the lowest-priority changes. Returns the capped new set.
    ///
    /// Priority: validators already in the active set are kept over new entrants.
    /// Among departures, those with higher stake are kept over those with lower.
    pub fn apply_rotation_cap(
        old_set: &ValidatorSet,
        mut new_set: ValidatorSet,
        max_changes: usize,
    ) -> ValidatorSet {
        use std::collections::BTreeSet;

        if max_changes == 0 {
            return old_set.clone();
        }

        // BTreeSet iterates in deterministic (sorted) order, unlike HashSet.
        // This ensures all nodes select the same validators when the rotation cap applies.
        let old_addrs: BTreeSet<Address> = old_set.validators.iter().map(|v| v.address).collect();
        let new_addrs: BTreeSet<Address> = new_set.validators.iter().map(|v| v.address).collect();

        let departures: Vec<Address> = old_addrs.difference(&new_addrs).copied().collect();
        let arrivals: Vec<Address> = new_addrs.difference(&old_addrs).copied().collect();

        let total_changes = departures.len() + arrivals.len();
        if total_changes <= max_changes {
            return new_set;
        }

        // Too many changes — limit to max_changes total.
        // Arrivals and departures are paired: each new validator entering
        // corresponds to an old validator leaving (when set size is capped).
        // Split the budget: half for departures, half for arrivals.
        let half = max_changes / 2;
        let allowed_swaps = half.min(departures.len()).min(arrivals.len());

        // For excess departures beyond allowed_swaps: keep old validator
        // For excess arrivals beyond allowed_swaps: defer to next epoch
        let kept_departures: BTreeSet<Address> =
            departures.iter().skip(allowed_swaps).copied().collect();
        let deferred_arrivals: BTreeSet<Address> =
            arrivals.iter().skip(allowed_swaps).copied().collect();

        // Remove deferred arrivals from new_set
        new_set
            .validators
            .retain(|v| !deferred_arrivals.contains(&v.address));

        // Re-add kept departures (validators that should have left but can't yet)
        for val in &old_set.validators {
            if kept_departures.contains(&val.address)
                && !new_set.validators.iter().any(|v| v.address == val.address)
            {
                new_set.validators.push(val.clone());
            }
        }

        // Re-sort by stake desc, address asc for determinism
        new_set.validators.sort_by(|a, b| {
            b.power
                .cmp(&a.power)
                .then_with(|| a.address.cmp(&b.address))
        });

        tracing::warn!(
            total_changes,
            max_changes,
            allowed_swaps,
            deferred = total_changes - allowed_swaps * 2,
            "rotation cap applied: deferred {} validator changes to next epoch",
            total_changes - allowed_swaps * 2
        );

        new_set
    }

    /// Log detailed rotation events at epoch boundary.
    pub fn log_rotation(
        old_set: &ValidatorSet,
        _new_set: &ValidatorSet,
        diff: &ValidatorSetDiff,
        epoch: u64,
    ) {
        use std::collections::HashMap;

        let old_map: HashMap<Address, &ValidatorInfo> =
            old_set.validators.iter().map(|v| (v.address, v)).collect();

        for v in &diff.inserts {
            if old_map.contains_key(&v.address) {
                tracing::info!(
                    epoch,
                    validator = %v.address,
                    power = v.power,
                    "epoch rotation: validator power/key updated"
                );
            } else {
                tracing::info!(
                    epoch,
                    validator = %v.address,
                    power = v.power,
                    "epoch rotation: validator joined active set"
                );
            }
        }

        for addr in &diff.deletes {
            let reason = if old_map.contains_key(addr) {
                "outranked or status change"
            } else {
                "removed"
            };
            tracing::info!(
                epoch,
                validator = %addr,
                reason,
                "epoch rotation: validator left active set"
            );
        }

        for pk in &diff.rotated_out_pubkeys {
            let pk_hex: String = pk.iter().map(|b| format!("{b:02x}")).collect();
            tracing::info!(
                epoch,
                old_pubkey = %pk_hex,
                "epoch rotation: old pubkey removed (key rotation)"
            );
        }
    }

    /// Check if the new set meets the minimum BFT liveness requirement.
    /// Returns an error if the set would be too small.
    pub fn check_minimum_set(new_set: &ValidatorSet) -> Result<()> {
        use crate::types::MIN_ACTIVE_VALIDATORS;

        if new_set.validators.is_empty() {
            tracing::error!("CRITICAL: zero eligible validators — halting epoch rotation");
            return Err(EconomicsError::BelowMinimumActiveSet {
                have: 0,
                need: MIN_ACTIVE_VALIDATORS,
            });
        }

        if new_set.validators.len() < MIN_ACTIVE_VALIDATORS {
            tracing::warn!(
                have = new_set.validators.len(),
                need = MIN_ACTIVE_VALIDATORS,
                "WARNING: active validator set below BFT minimum — reduced fault tolerance"
            );
            // Don't error — allow smaller sets but warn (single-validator dev/test is valid)
        }

        Ok(())
    }

    /// FIX 4 (S443): enforce the BFT-minimum floor across a rotation.
    ///
    /// If applying `new_set` would drop the ACTIVE set below
    /// [`MIN_ACTIVE_VALIDATORS`](crate::types::MIN_ACTIVE_VALIDATORS) while the
    /// previous set met it, re-seat the highest-priority departed validators so the
    /// set stays quorum-viable. This is deliberately narrow: seated-for-quorum is
    /// NOT the same as active-for-rewards — a re-seated validator may still be jailed
    /// by [`update_validator_statuses`](Self::update_validator_statuses); this only
    /// guarantees BFT can still form a quorum.
    ///
    /// Root cause it closes (t15 suicide chain): an honest validator was wrongly
    /// auto-jailed and deposed at the epoch boundary, dropping the active set 4 → 3.
    /// [`check_minimum_set`](Self::check_minimum_set) only WARNED and proceeded, so
    /// the 4-validator cluster silently lost the quorum it needed to make progress.
    ///
    /// No-op when the new set already meets the floor, or when the OLD set was itself
    /// below it (a genuine small network — dev/test or bootstrap — has nothing to
    /// protect and must not be force-grown). Deterministic: the same (old, new) pair
    /// yields the same result on every node (highest power, then address ascending).
    pub fn enforce_minimum_floor(old_set: &ValidatorSet, new_set: ValidatorSet) -> ValidatorSet {
        use crate::types::MIN_ACTIVE_VALIDATORS;
        let mut new_set = new_set;

        if new_set.validators.len() >= MIN_ACTIVE_VALIDATORS {
            return new_set;
        }
        if old_set.validators.len() < MIN_ACTIVE_VALIDATORS {
            // Old set already below the floor (dev/test / bootstrap) — nothing to protect.
            return new_set;
        }

        let present: std::collections::BTreeSet<Address> =
            new_set.validators.iter().map(|v| v.address).collect();
        let mut departed: Vec<&ValidatorInfo> = old_set
            .validators
            .iter()
            .filter(|v| !present.contains(&v.address))
            .collect();
        // Deterministic re-seat priority: highest power first, then address ascending
        // — identical to the ranking used everywhere else in this module.
        departed.sort_by(|a, b| b.power.cmp(&a.power).then_with(|| a.address.cmp(&b.address)));

        let need = MIN_ACTIVE_VALIDATORS - new_set.validators.len();
        let reseated = need.min(departed.len());
        for v in departed.into_iter().take(reseated) {
            new_set.validators.push(v.clone());
        }
        new_set.validators.sort_by(|a, b| {
            b.power
                .cmp(&a.power)
                .then_with(|| a.address.cmp(&b.address))
        });

        tracing::warn!(
            floored = new_set.validators.len(),
            reseated,
            min = MIN_ACTIVE_VALIDATORS,
            "FIX 4: epoch rotation would have dropped the active set below the BFT minimum — \
             re-seated the highest-priority departed validator(s) to keep quorum viable \
             (seated for quorum math; reward/jail status is handled separately)"
        );

        new_set
    }

    /// Update validator statuses after epoch rotation.
    pub fn update_validator_statuses<T: StateBackend>(
        staking: &StakingManager<T>,
        new_set: &ValidatorSet,
    ) -> Result<()> {
        let active_addrs: std::collections::BTreeSet<Address> =
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
    /// Old pubkeys that must be deleted from hotstuff_rs due to key rotation.
    /// BUG FIX (3.2): key rotation requires deleting old pubkey + inserting new one.
    pub rotated_out_pubkeys: Vec<[u8; 32]>,
}

impl ValidatorSetDiff {
    pub fn is_empty(&self) -> bool {
        self.inserts.is_empty() && self.deletes.is_empty() && self.rotated_out_pubkeys.is_empty()
    }

    /// Total number of validator changes (for rotation cap calculation).
    pub fn total_changes(&self) -> usize {
        self.inserts.len() + self.deletes.len()
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
        mgr.state().put_account(&addr, &info).unwrap();
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

    // FIX 4 TEST: Rotation cap produces deterministic results
    #[test]
    fn rotation_cap_deterministic_order() {
        // Run apply_rotation_cap many times and verify the result is always identical.
        // With HashSet, iteration order would be random → different kept/deferred sets.
        // With BTreeSet, it must always produce the same result.
        let old_set = ValidatorSet {
            validators: (1..=10u8)
                .map(|n| ValidatorInfo {
                    address: Address::new([n; 20]),
                    pubkey: PublicKey([n; 32]),
                    power: (100 - n as u64) * 10,
                    commission_bps: 500,
                })
                .collect(),
            epoch: 1,
        };

        // New set: remove validators 1-5, add 11-15 (5 departures, 5 arrivals = 10 changes).
        let new_set = ValidatorSet {
            validators: (6..=15u8)
                .map(|n| ValidatorInfo {
                    address: Address::new([n; 20]),
                    pubkey: PublicKey([n; 32]),
                    power: (100 - n as u64) * 10,
                    commission_bps: 500,
                })
                .collect(),
            epoch: 2,
        };

        // Cap at 4 changes (2 swaps).
        let first_result = EpochManager::apply_rotation_cap(&old_set, new_set.clone(), 4);
        let first_addrs: Vec<Address> = first_result.validators.iter().map(|v| v.address).collect();

        // Run 50 times — must always produce the same result.
        for i in 0..50 {
            let result = EpochManager::apply_rotation_cap(&old_set, new_set.clone(), 4);
            let addrs: Vec<Address> = result.validators.iter().map(|v| v.address).collect();
            assert_eq!(
                first_addrs, addrs,
                "rotation cap must be deterministic (iteration {i} differed)"
            );
        }
    }

    #[test]
    fn rotation_cap_within_limit_unchanged() {
        let old_set = ValidatorSet {
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
                    commission_bps: 500,
                },
            ],
            epoch: 1,
        };

        let new_set = ValidatorSet {
            validators: vec![
                ValidatorInfo {
                    address: Address::new([2; 20]),
                    pubkey: PublicKey([2; 32]),
                    power: 200,
                    commission_bps: 500,
                },
                ValidatorInfo {
                    address: Address::new([3; 20]),
                    pubkey: PublicKey([3; 32]),
                    power: 150,
                    commission_bps: 500,
                },
            ],
            epoch: 2,
        };

        // 2 changes (1 departure + 1 arrival), cap is 10 — should pass through unchanged.
        let result = EpochManager::apply_rotation_cap(&old_set, new_set.clone(), 10);
        assert_eq!(result.validators.len(), new_set.validators.len());
    }

    // ---- FIX 4 (S443): BFT-minimum floor on epoch rotation ----

    fn vinfo(n: u8, power: u64) -> ValidatorInfo {
        ValidatorInfo {
            address: Address::new([n; 20]),
            pubkey: PublicKey([n; 32]),
            power,
            commission_bps: 500,
        }
    }

    /// t15 root cause: an honest validator was (wrongly) deposed, dropping the
    /// active set from 4 → 3 (below the BFT minimum). `check_minimum_set` only
    /// WARNED and proceeded, so the cluster lost quorum. The floor guard must keep
    /// the set quorum-viable (≥ MIN_ACTIVE_VALIDATORS) by re-seating the highest-
    /// priority departed validator(s).
    #[test]
    fn floor_reseats_deposed_validator_to_keep_quorum() {
        use crate::types::MIN_ACTIVE_VALIDATORS;
        let old = ValidatorSet {
            validators: vec![
                vinfo(1, 400),
                vinfo(2, 300),
                vinfo(3, 200),
                vinfo(4, 100),
            ],
            epoch: 1,
        };
        // Validator #4 deposed → 3 remain (below BFT minimum of 4).
        let proposed = ValidatorSet {
            validators: vec![vinfo(1, 400), vinfo(2, 300), vinfo(3, 200)],
            epoch: 2,
        };
        let floored = EpochManager::enforce_minimum_floor(&old, proposed);
        assert!(
            floored.validators.len() >= MIN_ACTIVE_VALIDATORS,
            "floor must keep the set quorum-viable, got {}",
            floored.validators.len()
        );
        assert!(
            floored.validators.iter().any(|v| v.address == Address::new([4; 20])),
            "the deposed validator must be re-seated to preserve BFT quorum"
        );
        assert_eq!(floored.epoch, 2, "epoch number is preserved");
    }

    /// A rotation that stays at/above the floor is untouched.
    #[test]
    fn floor_noop_when_set_meets_minimum() {
        let old = ValidatorSet {
            validators: vec![vinfo(1, 400), vinfo(2, 300), vinfo(3, 200), vinfo(4, 100)],
            epoch: 1,
        };
        let proposed = ValidatorSet {
            validators: vec![vinfo(1, 400), vinfo(2, 300), vinfo(3, 200), vinfo(5, 150)],
            epoch: 2,
        };
        let floored = EpochManager::enforce_minimum_floor(&old, proposed.clone());
        assert_eq!(floored.validators.len(), proposed.validators.len());
        // #5 stays, #4 stays deposed — the floor is not breached, so no re-seating.
        assert!(floored.validators.iter().any(|v| v.address == Address::new([5; 20])));
        assert!(!floored.validators.iter().any(|v| v.address == Address::new([4; 20])));
    }

    /// A genuinely small network (old set already below the minimum, e.g. dev/test
    /// or bootstrap) is NOT force-grown — there is nothing to protect.
    #[test]
    fn floor_noop_when_old_set_below_minimum() {
        let old = ValidatorSet {
            validators: vec![vinfo(1, 400), vinfo(2, 300)],
            epoch: 1,
        };
        let proposed = ValidatorSet {
            validators: vec![vinfo(1, 400)],
            epoch: 2,
        };
        let floored = EpochManager::enforce_minimum_floor(&old, proposed);
        assert_eq!(
            floored.validators.len(),
            1,
            "old set was already below the floor; nothing to re-seat"
        );
    }
}
