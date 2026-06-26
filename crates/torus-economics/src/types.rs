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
    /// Block height of last commission change (Phase 3: 3.2.3 cooldown).
    pub last_commission_change_block: Option<u64>,
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
        BorshSerialize::serialize(&self.last_commission_change_block, writer)?;
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
        let last_commission_change_block = Option::<u64>::deserialize_reader(reader)?;
        Ok(Self {
            address,
            pubkey,
            commission_bps,
            self_stake,
            total_delegated,
            status,
            jailed_until,
            last_commission_change_block,
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
        // FIX ECON-PF-09: Safe length conversion instead of silent truncation via `as u32`
        let unbonding_len: u32 = self.unbonding.len().try_into().map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidData, "unbonding vec exceeds u32::MAX")
        })?;
        BorshSerialize::serialize(&unbonding_len, writer)?;
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

/// FIX ECON-PF-16: Canonical target block time in seconds. All block-count
/// constants derive from this. Matches max_view_time (2s) in node config.
pub const TARGET_BLOCK_TIME_SECS: u64 = 2;

/// Unbonding period: ~7 days of blocks.
pub const UNBONDING_PERIOD: u64 = 7 * 24 * 3600 / TARGET_BLOCK_TIME_SECS;

/// Maximum commission rate: 5000 bps (50%).
pub const MAX_COMMISSION_BPS: u16 = 5000;

/// Maximum commission change per update: 100 bps (1%).
pub const MAX_COMMISSION_CHANGE_BPS: u16 = 100;

/// Blocks per year, derived from TARGET_BLOCK_TIME_SECS.
pub const BLOCKS_PER_YEAR: u64 = 365 * 24 * 3600 / TARGET_BLOCK_TIME_SECS;

/// Permanent staking APY: 500 bps (5%).
pub const PERMANENT_STAKE_APY_BPS: u64 = 500;

/// Validator staking inflation APY: 500 bps (5%), matching permanent stake.
pub const VALIDATOR_INFLATION_APY_BPS: u64 = 500;

/// Seconds per year (365 days). Used for epoch fraction calculation.
pub const SECONDS_PER_YEAR: u64 = 365 * 24 * 3600;

/// Default supermajority threshold for PermanentUnlock proposals: 8000 bps (80%).
pub const DEFAULT_PERMANENT_UNLOCK_THRESHOLD_BPS: u64 = 8000;

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
// Slashing & Jailing Constants (Phase 3: 3.1)
// ============================================================================

/// Double-sign slash fraction: 500 bps (5%).
pub const DOUBLE_SIGN_SLASH_BPS: u16 = 500;

/// Downtime slash fraction: 10 bps (0.1%).
pub const DOWNTIME_SLASH_BPS: u16 = 10;

/// Double-sign evidence window: 100 views.
pub const EVIDENCE_WINDOW_VIEWS: u64 = 100;

/// Downtime detection window: 1000 blocks.
pub const DOWNTIME_WINDOW_BLOCKS: u64 = 1000;

/// Downtime signing threshold: 50%.
pub const DOWNTIME_THRESHOLD_PCT: u64 = 50;

/// Jail duration for downtime: ~2 days of blocks.
pub const JAIL_DURATION_BLOCKS: u64 = 2 * 24 * 3600 / TARGET_BLOCK_TIME_SECS;

/// Jail vote expiry: ~1 day of blocks.
pub const JAIL_VOTE_EXPIRY_BLOCKS: u64 = 24 * 3600 / TARGET_BLOCK_TIME_SECS;

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

// ============================================================================
// Slashing Types (Phase 3: 3.1)
// ============================================================================

/// Reason for a validator slash event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SlashReason {
    DoubleSign,
    Downtime,
    JailVote,
    InvalidAttestation,
}

impl BorshSerialize for SlashReason {
    fn serialize<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        let disc: u8 = match self {
            Self::DoubleSign => 0,
            Self::Downtime => 1,
            Self::JailVote => 2,
            Self::InvalidAttestation => 3,
        };
        writer.write_all(&[disc])
    }
}

impl BorshDeserialize for SlashReason {
    fn deserialize_reader<R: Read>(reader: &mut R) -> io::Result<Self> {
        let mut buf = [0u8; 1];
        reader.read_exact(&mut buf)?;
        match buf[0] {
            0 => Ok(Self::DoubleSign),
            1 => Ok(Self::Downtime),
            2 => Ok(Self::JailVote),
            3 => Ok(Self::InvalidAttestation),
            x => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("invalid SlashReason discriminant: {x}"),
            )),
        }
    }
}

/// Audit record of a slash event. Stored in CF_SLASH_RECORDS.
/// Key: validator(20) ++ block_height(8 BE) = 28 bytes.
#[derive(Clone, Debug)]
pub struct SlashRecord {
    pub validator: Address,
    pub slash_fraction_bps: u16,
    pub slashed_amount: U256,
    pub reason: SlashReason,
    pub block_height: u64,
}

impl BorshSerialize for SlashRecord {
    fn serialize<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        borsh_write_address(&self.validator, writer)?;
        BorshSerialize::serialize(&self.slash_fraction_bps, writer)?;
        borsh_write_u256(&self.slashed_amount, writer)?;
        BorshSerialize::serialize(&self.reason, writer)?;
        BorshSerialize::serialize(&self.block_height, writer)?;
        Ok(())
    }
}

impl BorshDeserialize for SlashRecord {
    fn deserialize_reader<R: Read>(reader: &mut R) -> io::Result<Self> {
        let validator = borsh_read_address(reader)?;
        let slash_fraction_bps = u16::deserialize_reader(reader)?;
        let slashed_amount = borsh_read_u256(reader)?;
        let reason = SlashReason::deserialize_reader(reader)?;
        let block_height = u64::deserialize_reader(reader)?;
        Ok(Self {
            validator,
            slash_fraction_bps,
            slashed_amount,
            reason,
            block_height,
        })
    }
}

// ============================================================================
// Key Rotation Types (Phase 3: 3.1.8)
// ============================================================================

/// Pending key rotation for a validator. Stored in CF_CONSENSUS_META with
/// key prefix "pending_rotation:" ++ validator address (20 bytes).
#[derive(Clone, Debug)]
pub struct PendingKeyRotation {
    pub validator: Address,
    pub new_pubkey: [u8; 32],
    pub effective_epoch: u64,
    pub submitted_at_block: u64,
}

impl BorshSerialize for PendingKeyRotation {
    fn serialize<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        borsh_write_address(&self.validator, writer)?;
        writer.write_all(&self.new_pubkey)?;
        BorshSerialize::serialize(&self.effective_epoch, writer)?;
        BorshSerialize::serialize(&self.submitted_at_block, writer)?;
        Ok(())
    }
}

impl BorshDeserialize for PendingKeyRotation {
    fn deserialize_reader<R: Read>(reader: &mut R) -> io::Result<Self> {
        let validator = borsh_read_address(reader)?;
        let mut new_pubkey = [0u8; 32];
        reader.read_exact(&mut new_pubkey)?;
        let effective_epoch = u64::deserialize_reader(reader)?;
        let submitted_at_block = u64::deserialize_reader(reader)?;
        Ok(Self {
            validator,
            new_pubkey,
            effective_epoch,
            submitted_at_block,
        })
    }
}

// FIX ECON-FIND-29: Removed dead constant KEY_ROTATION_COOLDOWN_EPOCHS.
// submit_key_rotation only checks for existing pending rotation, not cooldown.
// If cooldown enforcement is desired, add a `last_rotation_epoch` field to
// ValidatorState and check it in submit_key_rotation.

/// Commission change cooldown: ~2 days of blocks.
pub const COMMISSION_COOLDOWN_BLOCKS: u64 = 2 * 24 * 3600 / TARGET_BLOCK_TIME_SECS;

/// Validator whitelist expiry: ~7 days of blocks.
pub const WHITELIST_EXPIRY_BLOCKS: u64 = 7 * 24 * 3600 / TARGET_BLOCK_TIME_SECS;

/// Minimum number of active validators for BFT liveness (3f+1 where f=1).
pub const MIN_ACTIVE_VALIDATORS: usize = 4;

/// A jail vote record. Stored in CF_JAIL_VOTES.
/// Key: target(20) ++ voter(20) = 40 bytes.
#[derive(Clone, Debug)]
pub struct JailVoteRecord {
    pub voter: Address,
    pub target: Address,
    pub stake_weight: U256,
    pub block_height: u64,
}

impl BorshSerialize for JailVoteRecord {
    fn serialize<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        borsh_write_address(&self.voter, writer)?;
        borsh_write_address(&self.target, writer)?;
        borsh_write_u256(&self.stake_weight, writer)?;
        BorshSerialize::serialize(&self.block_height, writer)?;
        Ok(())
    }
}

impl BorshDeserialize for JailVoteRecord {
    fn deserialize_reader<R: Read>(reader: &mut R) -> io::Result<Self> {
        let voter = borsh_read_address(reader)?;
        let target = borsh_read_address(reader)?;
        let stake_weight = borsh_read_u256(reader)?;
        let block_height = u64::deserialize_reader(reader)?;
        Ok(Self {
            voter,
            target,
            stake_weight,
            block_height,
        })
    }
}

// ============================================================================
// Validator Whitelist (Phase 3: 3.2.1)
// ============================================================================

/// Governance-approved validator candidate. Stored in CF_CONSENSUS_META
/// with key prefix "validator_whitelist:" ++ candidate address (20 bytes).
#[derive(Clone, Debug)]
pub struct ValidatorWhitelistEntry {
    pub candidate: Address,
    pub approved_at_block: u64,
    pub expires_at_block: u64,
}

impl BorshSerialize for ValidatorWhitelistEntry {
    fn serialize<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        borsh_write_address(&self.candidate, writer)?;
        BorshSerialize::serialize(&self.approved_at_block, writer)?;
        BorshSerialize::serialize(&self.expires_at_block, writer)?;
        Ok(())
    }
}

impl BorshDeserialize for ValidatorWhitelistEntry {
    fn deserialize_reader<R: Read>(reader: &mut R) -> io::Result<Self> {
        let candidate = borsh_read_address(reader)?;
        let approved_at_block = u64::deserialize_reader(reader)?;
        let expires_at_block = u64::deserialize_reader(reader)?;
        Ok(Self {
            candidate,
            approved_at_block,
            expires_at_block,
        })
    }
}
