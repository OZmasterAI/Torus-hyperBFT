//! Developer pool: gas usage tracking per deployer and pro-rata distribution (task 2.7.5).

use alloy_primitives::{Address, U256};
use borsh::BorshDeserialize;
use torus_state::cf::CF_DEV_POOL;
use torus_state::StateBackend;

use crate::staking::StakingManager;
use crate::types::*;
use crate::EconomicsError;

type Result<T> = std::result::Result<T, EconomicsError>;

/// Tracks gas consumed by each deployer's contracts per epoch and distributes
/// the dev pool portion of fees proportionally.
pub struct DevPool;

impl DevPool {
    /// Record gas usage for a deployer's contract in the current epoch.
    pub fn record_gas_usage(
        staking: &StakingManager,
        deployer: Address,
        contract: Address,
        gas_used: u64,
    ) -> Result<()> {
        if gas_used == 0 {
            return Ok(());
        }

        let mut entry = Self::get_entry(staking, &deployer)?.unwrap_or(DevPoolEntry {
            deployer,
            total_gas_used: 0,
            contracts: vec![],
        });

        entry.total_gas_used = entry.total_gas_used.saturating_add(gas_used);

        // Track unique contracts.
        if !entry.contracts.contains(&contract) {
            entry.contracts.push(contract);
        }

        Self::put_entry(staking, &deployer, &entry)?;
        Ok(())
    }

    /// Distribute `pool_amount` to deployers proportional to their gas usage share.
    /// Returns the number of deployers who received rewards.
    pub fn distribute(staking: &StakingManager, pool_amount: U256) -> Result<u32> {
        if pool_amount.is_zero() {
            return Ok(0);
        }

        let entries = Self::all_entries(staking)?;
        if entries.is_empty() {
            return Ok(0);
        }

        let total_gas: u64 = entries.iter().map(|e| e.total_gas_used).sum();
        if total_gas == 0 {
            return Ok(0);
        }

        let mut distributed = U256::ZERO;
        let last_idx = entries.len().saturating_sub(1);
        let mut count = 0u32;

        for (i, entry) in entries.iter().enumerate() {
            if entry.total_gas_used == 0 {
                continue;
            }

            let share = if i == last_idx {
                // Last deployer gets remainder to avoid rounding dust.
                pool_amount - distributed
            } else {
                pool_amount * U256::from(entry.total_gas_used) / U256::from(total_gas)
            };

            if !share.is_zero() {
                staking.credit_balance(&entry.deployer, share)?;
                distributed += share;
                count += 1;
            }
        }

        tracing::debug!(
            %pool_amount,
            deployers = entries.len(),
            %total_gas,
            "dev pool distributed"
        );

        Ok(count)
    }

    /// Reset gas tracking at epoch boundary — deletes all entries.
    pub fn reset_epoch(staking: &StakingManager) -> Result<()> {
        let entries = Self::all_entries(staking)?;
        for entry in &entries {
            staking
                .state()
                .delete_cf_raw(CF_DEV_POOL, entry.deployer.as_slice())?;
        }
        tracing::debug!(cleared = entries.len(), "dev pool epoch reset");
        Ok(())
    }

    /// Get a single deployer's entry.
    pub fn get_entry(staking: &StakingManager, deployer: &Address) -> Result<Option<DevPoolEntry>> {
        match staking
            .state()
            .get_cf_raw(CF_DEV_POOL, deployer.as_slice())?
        {
            Some(data) => Ok(Some(
                DevPoolEntry::try_from_slice(&data)
                    .map_err(|e| EconomicsError::Borsh(e.to_string()))?,
            )),
            None => Ok(None),
        }
    }

    fn put_entry(staking: &StakingManager, deployer: &Address, entry: &DevPoolEntry) -> Result<()> {
        let data = borsh::to_vec(entry).map_err(|e| EconomicsError::Borsh(e.to_string()))?;
        staking
            .state()
            .put_cf_raw(CF_DEV_POOL, deployer.as_slice(), &data)?;
        Ok(())
    }

    /// Read all dev pool entries (full scan of CF_DEV_POOL).
    pub fn all_entries(staking: &StakingManager) -> Result<Vec<DevPoolEntry>> {
        let entries = staking.state().iterate_cf(CF_DEV_POOL, None)?;
        let mut result = Vec::new();
        for (_key, value) in &entries {
            let entry = DevPoolEntry::try_from_slice(value)
                .map_err(|e| EconomicsError::Borsh(e.to_string()))?;
            result.push(entry);
        }
        Ok(result)
    }
}
