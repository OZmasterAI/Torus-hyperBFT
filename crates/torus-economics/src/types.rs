//! Borsh-serializable staking types stored in RocksDB column families.
//!
//! `alloy_primitives::U256` and `Address` do not implement `BorshSerialize` /
//! `BorshDeserialize`, so we implement borsh manually for each type.

use alloy_primitives::{Address, U256};
use borsh::{BorshDeserialize, BorshSerialize};
use serde::{Deserialize, Serialize};
use std::io::{self, Read, Write};

// ============================================================================
// Borsh helpers for alloy types
// ============================================================================

fn borsh_write_u256<W: Write>(val: &U256, writer: &mut W) -> io::Result<()> {
    writer.write_all(&val.to_be_bytes::<32>())
}

fn borsh_read_u256<R: Read>(reader: &mut R) -> io::Result<U256> {
    let mut buf = [0u8; 32];
    reader.read_exact(&mut buf)?;
    Ok(U256::from_be_slice(&buf))
}

fn borsh_write_address<W: Write>(addr: &Address, writer: &mut W) -> io::Result<()> {
    writer.write_all(addr.as_slice())
}

fn borsh_read_address<R: Read>(reader: &mut R) -> io::Result<Address> {
    let mut buf = [0u8; 20];
    reader.read_exact(&mut buf)?;
    Ok(Address::new(buf))
}

// ============================================================================
// Validator State (CF_STAKING_VALIDATORS)
// ============================================================================

/// Status in the validator state machine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ValidatorStatus {
    Candidate,
    Active,
    Jailed,
    Tombstoned,
}

impl BorshSerialize for ValidatorStatus {
    fn serialize<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        let disc: u8 = match self {
            Self::Candidate => 0,
            Self::Active => 1,
            Self::Jailed => 2,
            Self::Tombstoned => 3,
        };
        writer.write_all(&[disc])
    }
}

impl BorshDeserialize for ValidatorStatus {
    fn deserialize_reader<R: Read>(reader: &mut R) -> io::Result<Self> {
        let mut buf = [0u8; 1];
        reader.read_exact(&mut buf)?;
        match buf[0] {
            0 => Ok(Self::Candidate),
            1 => Ok(Self::Active),
            2 => Ok(Self::Jailed),
            3 => Ok(Self::Tombstoned),
            x => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("invalid ValidatorStatus discriminant: {x}"),
            )),
        }
    }
}

/// Full validator state persisted in CF_STAKING_VALIDATORS.
/// Key: Address (20 bytes). Value: borsh-encoded ValidatorState.
#[derive(Clone, Debug)]
pub struct ValidatorState {
    pub address: Address,
    pub pubkey: [u8; 32],
    pub commission_bps: u16,
    pub self_stake: U256,
    pub total_delegated: U256,
    pub status: ValidatorStatus,
    pub jailed_until: Option<u64>,
}

impl ValidatorState {
    pub fn total_stake(&self) -> U256 {
        self.self_stake + self.total_delegated
    }
}

impl BorshSerialize for ValidatorState {
    fn serialize<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        borsh_write_address(&self.address, writer)?;
        writer.write_all(&self.pubkey)?;
        BorshSerialize::serialize(&self.commission_bps, writer)?;
        borsh_write_u256(&self.self_stake, writer)?;
        borsh_write_u256(&self.total_delegated, writer)?;
        BorshSerialize::serialize(&self.status, writer)?;
        BorshSerialize::serialize(&self.jailed_until, writer)?;
        Ok(())
    }
}

impl BorshDeserialize for ValidatorState {
    fn deserialize_reader<R: Read>(reader: &mut R) -> io::Result<Self> {
        let address = borsh_read_address(reader)?;
        let mut pubkey = [0u8; 32];
        reader.read_exact(&mut pubkey)?;
        let commission_bps = u16::deserialize_reader(reader)?;
        let self_stake = borsh_read_u256(reader)?;
        let total_delegated = borsh_read_u256(reader)?;
        let status = ValidatorStatus::deserialize_reader(reader)?;
        let jailed_until = Option::<u64>::deserialize_reader(reader)?;
        Ok(Self {
            address,
            pubkey,
            commission_bps,
            self_stake,
            total_delegated,
            status,
            jailed_until,
        })
    }
}

// ============================================================================
// Delegation (CF_STAKING_DELEGATIONS)
// ============================================================================

/// A single delegation record.
/// Key: delegator(20) ++ validator(20) = 40 bytes. Value: borsh-encoded.
#[derive(Clone, Debug)]
pub struct Delegation {
    pub delegator: Address,
    pub validator: Address,
    pub amount: U256,
    pub unbonding: Vec<UnbondingEntry>,
}

impl BorshSerialize for Delegation {
    fn serialize<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        borsh_write_address(&self.delegator, writer)?;
        borsh_write_address(&self.validator, writer)?;
        borsh_write_u256(&self.amount, writer)?;
        BorshSerialize::serialize(&(self.unbonding.len() as u32), writer)?;
        for entry in &self.unbonding {
            BorshSerialize::serialize(entry, writer)?;
        }
        Ok(())
    }
}

impl BorshDeserialize for Delegation {
    fn deserialize_reader<R: Read>(reader: &mut R) -> io::Result<Self> {
        let delegator = borsh_read_address(reader)?;
        let validator = borsh_read_address(reader)?;
        let amount = borsh_read_u256(reader)?;
        let len = u32::deserialize_reader(reader)? as usize;
        let mut unbonding = Vec::with_capacity(len);
        for _ in 0..len {
            unbonding.push(UnbondingEntry::deserialize_reader(reader)?);
        }
        Ok(Self {
            delegator,
            validator,
            amount,
            unbonding,
        })
    }
}

/// An unbonding entry: tokens being unlocked after undelegation.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UnbondingEntry {
    pub amount: U256,
    pub release_block: u64,
}

impl BorshSerialize for UnbondingEntry {
    fn serialize<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        borsh_write_u256(&self.amount, writer)?;
        BorshSerialize::serialize(&self.release_block, writer)?;
        Ok(())
    }
}

impl BorshDeserialize for UnbondingEntry {
    fn deserialize_reader<R: Read>(reader: &mut R) -> io::Result<Self> {
        let amount = borsh_read_u256(reader)?;
        let release_block = u64::deserialize_reader(reader)?;
        Ok(Self {
            amount,
            release_block,
        })
    }
}

// ============================================================================
// Permanent Staking (CF_STAKING_PERMANENT)
// ============================================================================

/// Permanent (irreversible) stake info.
/// Key: Address (20 bytes). Value: borsh-encoded.
#[derive(Clone, Debug)]
pub struct PermanentStakeInfo {
    pub staker: Address,
    pub amount: U256,
    pub locked_at_block: u64,
}

impl BorshSerialize for PermanentStakeInfo {
    fn serialize<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        borsh_write_address(&self.staker, writer)?;
        borsh_write_u256(&self.amount, writer)?;
        BorshSerialize::serialize(&self.locked_at_block, writer)?;
        Ok(())
    }
}

impl BorshDeserialize for PermanentStakeInfo {
    fn deserialize_reader<R: Read>(reader: &mut R) -> io::Result<Self> {
        let staker = borsh_read_address(reader)?;
        let amount = borsh_read_u256(reader)?;
        let locked_at_block = u64::deserialize_reader(reader)?;
        Ok(Self {
            staker,
            amount,
            locked_at_block,
        })
    }
}

// ============================================================================
// Pending Rewards (CF_STAKING_REWARDS)
// ============================================================================

/// Accumulated unclaimed rewards for an address.
/// Key: Address (20 bytes). Value: borsh-encoded.
#[derive(Clone, Debug)]
pub struct PendingRewards {
    pub address: Address,
    pub amount: U256,
}

impl BorshSerialize for PendingRewards {
    fn serialize<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        borsh_write_address(&self.address, writer)?;
        borsh_write_u256(&self.amount, writer)?;
        Ok(())
    }
}

impl BorshDeserialize for PendingRewards {
    fn deserialize_reader<R: Read>(reader: &mut R) -> io::Result<Self> {
        let address = borsh_read_address(reader)?;
        let amount = borsh_read_u256(reader)?;
        Ok(Self { address, amount })
    }
}

// ============================================================================
// Query Response Types (for RPC, task 1.11.4)
// ============================================================================

/// Epoch information for RPC responses.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EpochInfo {
    pub current_epoch: u64,
    pub epoch_start_block: u64,
    pub epoch_end_block: u64,
    pub blocks_remaining: u64,
    pub epoch_length: u64,
}

/// Staking summary for a single address.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StakingInfo {
    pub delegated: Vec<(Address, U256)>,
    pub permanent_stake: U256,
    pub pending_rewards: U256,
    pub unbonding: Vec<UnbondingEntry>,
}

// ============================================================================
// Constants
// ============================================================================

/// Minimum self-delegation for validator registration: 10,000 TRS = 10^22 wei.
pub const MIN_SELF_DELEGATION: U256 = {
    // 10_000 * 10^18 = 10^22 = 0x21E19E0C9BAB2400000
    U256::from_limbs([0x19E0_C9BA_B240_0000, 0x0000_0000_0000_021E, 0, 0])
};

/// Unbonding period: 604,800 blocks (~7 days at 1 block/sec).
pub const UNBONDING_PERIOD: u64 = 604_800;

/// Maximum commission rate: 5000 bps (50%).
pub const MAX_COMMISSION_BPS: u16 = 5000;

/// Maximum commission change per update: 100 bps (1%).
pub const MAX_COMMISSION_CHANGE_BPS: u16 = 100;

/// Blocks per year (~1 block/sec): 31,536,000.
pub const BLOCKS_PER_YEAR: u64 = 31_536_000;

/// Permanent staking APY: 500 bps (5%).
pub const PERMANENT_STAKE_APY_BPS: u64 = 500;

/// Fee split transition epochs (5 years).
pub const TRANSITION_EPOCHS: u64 = 1825;

/// Fee split start ratios (bps).
pub const FEE_START_BURN_BPS: u16 = 1000;
pub const FEE_START_VALIDATOR_BPS: u16 = 0;
pub const FEE_START_TREASURY_BPS: u16 = 4500;
pub const FEE_START_DEV_POOL_BPS: u16 = 4500;

/// Fee split end ratios (bps).
pub const FEE_END_BURN_BPS: u16 = 2500;
pub const FEE_END_VALIDATOR_BPS: u16 = 2500;
pub const FEE_END_TREASURY_BPS: u16 = 2500;
pub const FEE_END_DEV_POOL_BPS: u16 = 2500;

// ============================================================================
// Fee Split Result (2.7)
// ============================================================================

/// Result of splitting fees into four buckets. All values sum exactly to the input.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FeeSplitResult {
    pub burn: U256,
    pub validators: U256,
    pub treasury: U256,
    pub dev_pool: U256,
}

// ============================================================================
// Dev Pool Entry (2.7.5)
// ============================================================================

/// Tracks gas usage for a single deployer in one epoch.
/// Key: deployer Address (20 bytes). Value: borsh-encoded.
#[derive(Clone, Debug)]
pub struct DevPoolEntry {
    pub deployer: Address,
    pub total_gas_used: u64,
    pub contracts: Vec<Address>,
}

impl BorshSerialize for DevPoolEntry {
    fn serialize<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        borsh_write_address(&self.deployer, writer)?;
        BorshSerialize::serialize(&self.total_gas_used, writer)?;
        BorshSerialize::serialize(&(self.contracts.len() as u32), writer)?;
        for addr in &self.contracts {
            borsh_write_address(addr, writer)?;
        }
        Ok(())
    }
}

impl BorshDeserialize for DevPoolEntry {
    fn deserialize_reader<R: Read>(reader: &mut R) -> io::Result<Self> {
        let deployer = borsh_read_address(reader)?;
        let total_gas_used = u64::deserialize_reader(reader)?;
        let len = u32::deserialize_reader(reader)? as usize;
        let mut contracts = Vec::with_capacity(len);
        for _ in 0..len {
            contracts.push(borsh_read_address(reader)?);
        }
        Ok(Self {
            deployer,
            total_gas_used,
            contracts,
        })
    }
}

// ============================================================================
// Supply Tracker (2.7.2 — burn tracking)
// ============================================================================

/// Tracks cumulative burned and treasury amounts.
/// Key: static "supply" key in CF_TREASURY. Value: borsh-encoded.
#[derive(Clone, Debug)]
pub struct SupplyTracker {
    pub cumulative_burned: U256,
    pub cumulative_treasury: U256,
}

impl BorshSerialize for SupplyTracker {
    fn serialize<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        borsh_write_u256(&self.cumulative_burned, writer)?;
        borsh_write_u256(&self.cumulative_treasury, writer)?;
        Ok(())
    }
}

impl BorshDeserialize for SupplyTracker {
    fn deserialize_reader<R: Read>(reader: &mut R) -> io::Result<Self> {
        let cumulative_burned = borsh_read_u256(reader)?;
        let cumulative_treasury = borsh_read_u256(reader)?;
        Ok(Self {
            cumulative_burned,
            cumulative_treasury,
        })
    }
}
