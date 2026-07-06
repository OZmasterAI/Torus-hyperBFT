//! On-chain governance: proposal submission, voting with chain-computed weight,
//! and proposal execution (task 2.8).
//!
//! Vote weight is computed by the chain (never user-supplied):
//!   weight = delegated_stake + (permanent_stake * multiplier_num / multiplier_den)
//!
//! Stake amounts are read directly from staking column families to avoid
//! circular coupling with `StakingManager`.

use alloy_primitives::{Address, U256};
use borsh::{BorshDeserialize, BorshSerialize};
use serde::{Deserialize, Serialize};
use std::io::{self, Read, Write};
use torus_state::cf::{
    CF_FEE_CONFIG, CF_GOVERNANCE_PROPOSALS, CF_GOVERNANCE_VOTES, CF_NATIVE_MARKETS,
    CF_STAKING_DELEGATIONS, CF_STAKING_PERMANENT,
};
use torus_state::{StateBackend, StateDb};
use torus_types::FixedPoint;

use crate::error::EconomicsError;
use crate::types::{Delegation, PermanentStakeInfo};

type Result<T> = std::result::Result<T, EconomicsError>;

// ============================================================================
// Constants
// ============================================================================

/// Default voting period: ~7 days of blocks, derived from canonical block time.
pub const DEFAULT_VOTING_PERIOD_BLOCKS: u64 = 7 * 24 * 3600 / crate::types::TARGET_BLOCK_TIME_SECS;

/// Default quorum: 3300 basis points (33%) of total staked supply.
pub const DEFAULT_QUORUM_BPS: u64 = 3300;

/// Maximum proposal title length in characters.
pub const MAX_TITLE_LEN: usize = 128;

/// Maximum proposal description length in characters.
pub const MAX_DESCRIPTION_LEN: usize = 4096;

/// Key for governance parameters in CF_FEE_CONFIG.
const GOVERNANCE_PARAMS_KEY: &[u8] = b"gov_params";

/// Key for the auto-incrementing proposal counter in CF_FEE_CONFIG.
const PROPOSAL_COUNTER_KEY: &[u8] = b"gov_next_id";

// ============================================================================
// Borsh helpers for alloy / torus types
// ============================================================================

fn borsh_write_address<W: Write>(addr: &Address, writer: &mut W) -> io::Result<()> {
    writer.write_all(addr.as_slice())
}

fn borsh_read_address<R: Read>(reader: &mut R) -> io::Result<Address> {
    let mut buf = [0u8; 20];
    reader.read_exact(&mut buf)?;
    Ok(Address::new(buf))
}

fn borsh_write_u256<W: Write>(val: &U256, writer: &mut W) -> io::Result<()> {
    writer.write_all(&val.to_be_bytes::<32>())
}

fn borsh_read_u256<R: Read>(reader: &mut R) -> io::Result<U256> {
    let mut buf = [0u8; 32];
    reader.read_exact(&mut buf)?;
    Ok(U256::from_be_slice(&buf))
}

// ============================================================================
// ProposalType
// ============================================================================

/// Classification of a governance proposal.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProposalType {
    ParameterChange,
    TreasurySpend,
    MarketListing,
    TextProposal,
    /// Whitelist a candidate for validator registration (Phase 3: 3.2.1).
    ValidatorRegistration,
    /// Unlock permanently staked tokens via governance supermajority (80%).
    PermanentUnlock,
}

impl BorshSerialize for ProposalType {
    fn serialize<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        let disc: u8 = match self {
            Self::ParameterChange => 0,
            Self::TreasurySpend => 1,
            Self::MarketListing => 2,
            Self::TextProposal => 3,
            Self::ValidatorRegistration => 4,
            Self::PermanentUnlock => 5,
        };
        writer.write_all(&[disc])
    }
}

impl BorshDeserialize for ProposalType {
    fn deserialize_reader<R: Read>(reader: &mut R) -> io::Result<Self> {
        let mut buf = [0u8; 1];
        reader.read_exact(&mut buf)?;
        match buf[0] {
            0 => Ok(Self::ParameterChange),
            1 => Ok(Self::TreasurySpend),
            2 => Ok(Self::MarketListing),
            3 => Ok(Self::TextProposal),
            4 => Ok(Self::ValidatorRegistration),
            5 => Ok(Self::PermanentUnlock),
            x => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("invalid ProposalType discriminant: {x}"),
            )),
        }
    }
}

// ============================================================================
// ProposalStatus
// ============================================================================

/// Lifecycle status of a governance proposal.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProposalStatus {
    Pending,
    Active,
    Passed,
    Rejected,
    Executed,
    Expired,
}

impl BorshSerialize for ProposalStatus {
    fn serialize<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        let disc: u8 = match self {
            Self::Pending => 0,
            Self::Active => 1,
            Self::Passed => 2,
            Self::Rejected => 3,
            Self::Executed => 4,
            Self::Expired => 5,
        };
        writer.write_all(&[disc])
    }
}

impl BorshDeserialize for ProposalStatus {
    fn deserialize_reader<R: Read>(reader: &mut R) -> io::Result<Self> {
        let mut buf = [0u8; 1];
        reader.read_exact(&mut buf)?;
        match buf[0] {
            0 => Ok(Self::Pending),
            1 => Ok(Self::Active),
            2 => Ok(Self::Passed),
            3 => Ok(Self::Rejected),
            4 => Ok(Self::Executed),
            5 => Ok(Self::Expired),
            x => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("invalid ProposalStatus discriminant: {x}"),
            )),
        }
    }
}

// ============================================================================
// ExecutionPayload
// ============================================================================

/// Concrete data needed to execute a passed proposal.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ExecutionPayload {
    ParameterChange {
        param_key: String,
        new_value: String,
    },
    TreasurySpend {
        recipient: Address,
        amount: U256,
        reason: String,
    },
    MarketListing {
        market_id: u64,
        base_asset: String,
        quote_asset: String,
        lot_size: FixedPoint,
        tick_size: FixedPoint,
        initial_margin: FixedPoint,
    },
    /// Whitelist a candidate address for validator registration (Phase 3: 3.2.1).
    ValidatorRegistration { candidate: Address },
    /// Unlock permanently staked tokens via governance supermajority vote.
    PermanentUnlock { staker: Address, amount: U256 },
}

impl BorshSerialize for ExecutionPayload {
    fn serialize<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        match self {
            Self::ParameterChange {
                param_key,
                new_value,
            } => {
                writer.write_all(&[0u8])?;
                BorshSerialize::serialize(param_key, writer)?;
                BorshSerialize::serialize(new_value, writer)?;
            }
            Self::TreasurySpend {
                recipient,
                amount,
                reason,
            } => {
                writer.write_all(&[1u8])?;
                borsh_write_address(recipient, writer)?;
                borsh_write_u256(amount, writer)?;
                BorshSerialize::serialize(reason, writer)?;
            }
            Self::MarketListing {
                market_id,
                base_asset,
                quote_asset,
                lot_size,
                tick_size,
                initial_margin,
            } => {
                writer.write_all(&[2u8])?;
                BorshSerialize::serialize(market_id, writer)?;
                BorshSerialize::serialize(base_asset, writer)?;
                BorshSerialize::serialize(quote_asset, writer)?;
                BorshSerialize::serialize(&lot_size.raw(), writer)?;
                BorshSerialize::serialize(&tick_size.raw(), writer)?;
                BorshSerialize::serialize(&initial_margin.raw(), writer)?;
            }
            Self::ValidatorRegistration { candidate } => {
                writer.write_all(&[3u8])?;
                borsh_write_address(candidate, writer)?;
            }
            Self::PermanentUnlock { staker, amount } => {
                writer.write_all(&[4u8])?;
                borsh_write_address(staker, writer)?;
                borsh_write_u256(amount, writer)?;
            }
        }
        Ok(())
    }
}

impl BorshDeserialize for ExecutionPayload {
    fn deserialize_reader<R: Read>(reader: &mut R) -> io::Result<Self> {
        let mut disc = [0u8; 1];
        reader.read_exact(&mut disc)?;
        match disc[0] {
            0 => {
                let param_key = String::deserialize_reader(reader)?;
                let new_value = String::deserialize_reader(reader)?;
                Ok(Self::ParameterChange {
                    param_key,
                    new_value,
                })
            }
            1 => {
                let recipient = borsh_read_address(reader)?;
                let amount = borsh_read_u256(reader)?;
                let reason = String::deserialize_reader(reader)?;
                Ok(Self::TreasurySpend {
                    recipient,
                    amount,
                    reason,
                })
            }
            2 => {
                let market_id = u64::deserialize_reader(reader)?;
                let base_asset = String::deserialize_reader(reader)?;
                let quote_asset = String::deserialize_reader(reader)?;
                let lot_size = FixedPoint::from_raw(i128::deserialize_reader(reader)?);
                let tick_size = FixedPoint::from_raw(i128::deserialize_reader(reader)?);
                let initial_margin = FixedPoint::from_raw(i128::deserialize_reader(reader)?);
                Ok(Self::MarketListing {
                    market_id,
                    base_asset,
                    quote_asset,
                    lot_size,
                    tick_size,
                    initial_margin,
                })
            }
            3 => {
                let candidate = borsh_read_address(reader)?;
                Ok(Self::ValidatorRegistration { candidate })
            }
            4 => {
                let staker = borsh_read_address(reader)?;
                let amount = borsh_read_u256(reader)?;
                Ok(Self::PermanentUnlock { staker, amount })
            }
            x => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("invalid ExecutionPayload discriminant: {x}"),
            )),
        }
    }
}

// ============================================================================
// Proposal
// ============================================================================

/// Full governance proposal stored in CF_GOVERNANCE_PROPOSALS.
/// Key: proposal_id as u64 BE (8 bytes).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Proposal {
    pub id: u64,
    pub proposer: Address,
    pub title: String,
    pub description: String,
    pub proposal_type: ProposalType,
    pub status: ProposalStatus,
    pub votes_for: U256,
    pub votes_against: U256,
    pub start_block: u64,
    pub end_block: u64,
    /// Block after which a passed proposal can be executed (FIX 15: timelock).
    pub executable_after: u64,
    /// Block at which voting power is snapshotted (FIX 16: flash-vote defense).
    pub snapshot_block: u64,
    pub execution_payload: Option<ExecutionPayload>,
}

impl BorshSerialize for Proposal {
    fn serialize<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        BorshSerialize::serialize(&self.id, writer)?;
        borsh_write_address(&self.proposer, writer)?;
        BorshSerialize::serialize(&self.title, writer)?;
        BorshSerialize::serialize(&self.description, writer)?;
        BorshSerialize::serialize(&self.proposal_type, writer)?;
        BorshSerialize::serialize(&self.status, writer)?;
        borsh_write_u256(&self.votes_for, writer)?;
        borsh_write_u256(&self.votes_against, writer)?;
        BorshSerialize::serialize(&self.start_block, writer)?;
        BorshSerialize::serialize(&self.end_block, writer)?;
        BorshSerialize::serialize(&self.executable_after, writer)?;
        BorshSerialize::serialize(&self.snapshot_block, writer)?;
        match &self.execution_payload {
            None => writer.write_all(&[0u8])?,
            Some(payload) => {
                writer.write_all(&[1u8])?;
                BorshSerialize::serialize(payload, writer)?;
            }
        }
        Ok(())
    }
}

impl BorshDeserialize for Proposal {
    fn deserialize_reader<R: Read>(reader: &mut R) -> io::Result<Self> {
        let id = u64::deserialize_reader(reader)?;
        let proposer = borsh_read_address(reader)?;
        let title = String::deserialize_reader(reader)?;
        let description = String::deserialize_reader(reader)?;
        let proposal_type = ProposalType::deserialize_reader(reader)?;
        let status = ProposalStatus::deserialize_reader(reader)?;
        let votes_for = borsh_read_u256(reader)?;
        let votes_against = borsh_read_u256(reader)?;
        let start_block = u64::deserialize_reader(reader)?;
        let end_block = u64::deserialize_reader(reader)?;
        let executable_after = u64::deserialize_reader(reader)?;
        let snapshot_block = u64::deserialize_reader(reader)?;
        let mut opt_flag = [0u8; 1];
        reader.read_exact(&mut opt_flag)?;
        let execution_payload = if opt_flag[0] == 0 {
            None
        } else {
            Some(ExecutionPayload::deserialize_reader(reader)?)
        };
        Ok(Self {
            id,
            proposer,
            title,
            description,
            proposal_type,
            status,
            votes_for,
            votes_against,
            start_block,
            end_block,
            executable_after,
            snapshot_block,
            execution_payload,
        })
    }
}

// ============================================================================
// Vote
// ============================================================================

/// A vote record stored in CF_GOVERNANCE_VOTES.
/// Key: proposal_id(8 BE) ++ voter(20) = 28 bytes.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Vote {
    pub voter: Address,
    pub proposal_id: u64,
    pub support: bool,
    pub weight: U256,
    pub block_number: u64,
}

impl BorshSerialize for Vote {
    fn serialize<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        borsh_write_address(&self.voter, writer)?;
        BorshSerialize::serialize(&self.proposal_id, writer)?;
        BorshSerialize::serialize(&self.support, writer)?;
        borsh_write_u256(&self.weight, writer)?;
        BorshSerialize::serialize(&self.block_number, writer)?;
        Ok(())
    }
}

impl BorshDeserialize for Vote {
    fn deserialize_reader<R: Read>(reader: &mut R) -> io::Result<Self> {
        let voter = borsh_read_address(reader)?;
        let proposal_id = u64::deserialize_reader(reader)?;
        let support = bool::deserialize_reader(reader)?;
        let weight = borsh_read_u256(reader)?;
        let block_number = u64::deserialize_reader(reader)?;
        Ok(Self {
            voter,
            proposal_id,
            support,
            weight,
            block_number,
        })
    }
}

// ============================================================================
// GovernanceParams
// ============================================================================

/// Configurable governance parameters stored in CF_FEE_CONFIG.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GovernanceParams {
    pub voting_period_blocks: u64,
    /// Quorum as basis points of total staked supply (e.g. 3300 = 33%).
    pub quorum_bps: u64,
    pub min_proposal_stake: U256,
    /// Numerator of the permanent stake weight multiplier (default 3).
    pub permanent_weight_multiplier_num: u64,
    /// Denominator of the permanent stake weight multiplier (default 2).
    pub permanent_weight_multiplier_den: u64,
    pub treasury_address: Address,
    /// Timelock blocks: proposals must wait this many blocks after passing before execution.
    pub timelock_blocks: u64,
    /// Supermajority threshold for PermanentUnlock proposals (bps, default 8000 = 80%).
    pub permanent_unlock_threshold_bps: u64,
}

impl GovernanceParams {
    /// Return default governance parameters.
    pub fn defaults(treasury_address: Address) -> Self {
        Self {
            voting_period_blocks: DEFAULT_VOTING_PERIOD_BLOCKS,
            quorum_bps: DEFAULT_QUORUM_BPS,
            min_proposal_stake: U256::from(1_000u64) * U256::from(10u64).pow(U256::from(18u64)),
            permanent_weight_multiplier_num: 3,
            permanent_weight_multiplier_den: 2,
            treasury_address,
            timelock_blocks: 24 * 3600 / crate::types::TARGET_BLOCK_TIME_SECS,
            permanent_unlock_threshold_bps: crate::types::DEFAULT_PERMANENT_UNLOCK_THRESHOLD_BPS,
        }
    }
}

impl BorshSerialize for GovernanceParams {
    fn serialize<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        BorshSerialize::serialize(&self.voting_period_blocks, writer)?;
        BorshSerialize::serialize(&self.quorum_bps, writer)?;
        borsh_write_u256(&self.min_proposal_stake, writer)?;
        BorshSerialize::serialize(&self.permanent_weight_multiplier_num, writer)?;
        BorshSerialize::serialize(&self.permanent_weight_multiplier_den, writer)?;
        borsh_write_address(&self.treasury_address, writer)?;
        BorshSerialize::serialize(&self.timelock_blocks, writer)?;
        BorshSerialize::serialize(&self.permanent_unlock_threshold_bps, writer)?;
        Ok(())
    }
}

impl BorshDeserialize for GovernanceParams {
    fn deserialize_reader<R: Read>(reader: &mut R) -> io::Result<Self> {
        let voting_period_blocks = u64::deserialize_reader(reader)?;
        let quorum_bps = u64::deserialize_reader(reader)?;
        let min_proposal_stake = borsh_read_u256(reader)?;
        let permanent_weight_multiplier_num = u64::deserialize_reader(reader)?;
        let permanent_weight_multiplier_den = u64::deserialize_reader(reader)?;
        let treasury_address = borsh_read_address(reader)?;
        let timelock_blocks = u64::deserialize_reader(reader)?;
        // Backward-compatible: default if field absent (pre-upgrade data).
        let permanent_unlock_threshold_bps = u64::deserialize_reader(reader)
            .unwrap_or(crate::types::DEFAULT_PERMANENT_UNLOCK_THRESHOLD_BPS);
        Ok(Self {
            voting_period_blocks,
            quorum_bps,
            min_proposal_stake,
            permanent_weight_multiplier_num,
            permanent_weight_multiplier_den,
            treasury_address,
            timelock_blocks,
            permanent_unlock_threshold_bps,
        })
    }
}

// ============================================================================
// ProposalOutcome
// ============================================================================

/// Result of finalizing a proposal.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProposalOutcome {
    Passed(u64),
    Rejected(u64),
    Executed(u64),
}

// ============================================================================
// GovernanceManager
// ============================================================================

/// Manages on-chain governance: proposals, voting, and execution.
///
/// Reads stake data directly from staking column families to compute
/// chain-verified vote weights without coupling to `StakingManager`.
#[derive(Clone)]
pub struct GovernanceManager<T: StateBackend = StateDb> {
    state: T,
}

impl<T: StateBackend> GovernanceManager<T> {
    pub fn new(state: T) -> Self {
        Self { state }
    }

    pub fn state(&self) -> &T {
        &self.state
    }

    // ========================================================================
    // FIX 13: Parameter change validation (ECON-PF-12)
    // ========================================================================

    /// Validate a parameter change against the allowlist of modifiable parameters.
    fn validate_param_change(key: &str, value: &str) -> Result<()> {
        match key {
            "maintenance_margin_bps" => {
                let v: u64 = value
                    .parse()
                    .map_err(|_| EconomicsError::InvalidParameterValue {
                        key: key.to_string(),
                        reason: "must be a valid u64".to_string(),
                    })?;
                if !(25..=5000).contains(&v) {
                    return Err(EconomicsError::InvalidParameterValue {
                        key: key.to_string(),
                        reason: "must be between 25 and 5000 (0.25% - 50%)".to_string(),
                    });
                }
            }
            "max_leverage" => {
                let v: u32 = value
                    .parse()
                    .map_err(|_| EconomicsError::InvalidParameterValue {
                        key: key.to_string(),
                        reason: "must be a valid u32".to_string(),
                    })?;
                if v == 0 || v > 200 {
                    return Err(EconomicsError::InvalidParameterValue {
                        key: key.to_string(),
                        reason: "must be between 1 and 200".to_string(),
                    });
                }
            }
            "voting_period_blocks" => {
                let v: u64 = value
                    .parse()
                    .map_err(|_| EconomicsError::InvalidParameterValue {
                        key: key.to_string(),
                        reason: "must be a valid u64".to_string(),
                    })?;
                if !(1000..=1_000_000).contains(&v) {
                    return Err(EconomicsError::InvalidParameterValue {
                        key: key.to_string(),
                        reason: "must be between 1000 and 1000000 blocks".to_string(),
                    });
                }
            }
            "quorum_bps" => {
                let v: u64 = value
                    .parse()
                    .map_err(|_| EconomicsError::InvalidParameterValue {
                        key: key.to_string(),
                        reason: "must be a valid u64".to_string(),
                    })?;
                if !(1000..=6700).contains(&v) {
                    return Err(EconomicsError::InvalidParameterValue {
                        key: key.to_string(),
                        reason: "must be between 1000 and 6700 (10% - 67%)".to_string(),
                    });
                }
            }
            "permanent_weight_multiplier_num" => {
                let v: u64 = value
                    .parse()
                    .map_err(|_| EconomicsError::InvalidParameterValue {
                        key: key.to_string(),
                        reason: "must be a valid u64".to_string(),
                    })?;
                if v == 0 || v > 100 {
                    return Err(EconomicsError::InvalidParameterValue {
                        key: key.to_string(),
                        reason: "must be between 1 and 100".to_string(),
                    });
                }
            }
            "permanent_weight_multiplier_den" => {
                let v: u64 = value
                    .parse()
                    .map_err(|_| EconomicsError::InvalidParameterValue {
                        key: key.to_string(),
                        reason: "must be a valid u64".to_string(),
                    })?;
                if v == 0 || v > 100 {
                    return Err(EconomicsError::InvalidParameterValue {
                        key: key.to_string(),
                        reason: "must be between 1 and 100 (zero disallowed)".to_string(),
                    });
                }
            }
            "liquidation_penalty_bps" => {
                let v: u64 = value
                    .parse()
                    .map_err(|_| EconomicsError::InvalidParameterValue {
                        key: key.to_string(),
                        reason: "must be a valid u64".to_string(),
                    })?;
                if v > 1000 {
                    return Err(EconomicsError::InvalidParameterValue {
                        key: key.to_string(),
                        reason: "must be between 0 and 1000 (0% - 10%)".to_string(),
                    });
                }
            }
            "timelock_blocks" => {
                let v: u64 = value
                    .parse()
                    .map_err(|_| EconomicsError::InvalidParameterValue {
                        key: key.to_string(),
                        reason: "must be a valid u64".to_string(),
                    })?;
                // FIX MED-NEW-16: Enforce minimum timelock to prevent governance from
                // disabling the delay entirely. 10 blocks ≈ 20s at 2s block time.
                if !(10..=100_000).contains(&v) {
                    return Err(EconomicsError::InvalidParameterValue {
                        key: key.to_string(),
                        reason: "must be between 10 and 100000 blocks".to_string(),
                    });
                }
            }
            "permanent_unlock_threshold_bps" => {
                let v: u64 = value
                    .parse()
                    .map_err(|_| EconomicsError::InvalidParameterValue {
                        key: key.to_string(),
                        reason: "must be a valid u64".to_string(),
                    })?;
                if !(5000..=10000).contains(&v) {
                    return Err(EconomicsError::InvalidParameterValue {
                        key: key.to_string(),
                        reason: "must be between 5000 and 10000 (50% - 100%)".to_string(),
                    });
                }
            }
            _ => return Err(EconomicsError::ParameterNotModifiable(key.to_string())),
        }
        Ok(())
    }

    // ========================================================================
    // 2.8.1: Proposal submission
    // ========================================================================

    /// Submit a new governance proposal. Returns the auto-assigned proposal ID.
    ///
    /// The proposer must have total stake (delegated + permanent) >= min_proposal_stake.
    /// The proposal is immediately set to Active status.
    pub fn submit_proposal(
        &self,
        proposer: Address,
        title: String,
        description: String,
        execution_payload: Option<ExecutionPayload>,
        current_block: u64,
    ) -> Result<u64> {
        if title.len() > MAX_TITLE_LEN {
            return Err(EconomicsError::TitleTooLong {
                len: title.len(),
                max: MAX_TITLE_LEN,
            });
        }
        if description.len() > MAX_DESCRIPTION_LEN {
            return Err(EconomicsError::DescriptionTooLong {
                len: description.len(),
                max: MAX_DESCRIPTION_LEN,
            });
        }

        let params = self.get_governance_params()?;

        // Verify proposer has minimum stake (delegated + permanent, unweighted).
        let total_stake =
            self.total_delegated_for(&proposer)? + self.permanent_stake_for(&proposer)?;
        if total_stake < params.min_proposal_stake {
            return Err(EconomicsError::InsufficientProposalStake {
                have: total_stake,
                need: params.min_proposal_stake,
            });
        }

        // FIX 13: Validate parameter changes at submission time (defense-in-depth).
        if let Some(ExecutionPayload::ParameterChange {
            ref param_key,
            ref new_value,
        }) = execution_payload
        {
            Self::validate_param_change(param_key, new_value)?;
        }

        // Derive proposal type from payload.
        let proposal_type = match &execution_payload {
            Some(ExecutionPayload::ParameterChange { .. }) => ProposalType::ParameterChange,
            Some(ExecutionPayload::TreasurySpend { .. }) => ProposalType::TreasurySpend,
            Some(ExecutionPayload::MarketListing { .. }) => ProposalType::MarketListing,
            Some(ExecutionPayload::ValidatorRegistration { .. }) => {
                ProposalType::ValidatorRegistration
            }
            Some(ExecutionPayload::PermanentUnlock { .. }) => ProposalType::PermanentUnlock,
            None => ProposalType::TextProposal,
        };

        let id = self.next_proposal_id()?;

        let proposal = Proposal {
            id,
            proposer,
            title,
            description,
            proposal_type,
            status: ProposalStatus::Active,
            votes_for: U256::ZERO,
            votes_against: U256::ZERO,
            start_block: current_block,
            end_block: current_block + params.voting_period_blocks,
            executable_after: 0,
            snapshot_block: current_block,
            execution_payload,
        };

        self.put_proposal(&proposal)?;

        // FIX ECON-FIND-16: Snapshot all voter weights at proposal creation time.
        // Prevents flash-vote attacks by fixing voting power at snapshot_block.
        self.snapshot_voter_weights(id, &params)?;

        tracing::info!(id, %proposer, "governance proposal submitted");
        Ok(id)
    }

    // ========================================================================
    // 2.8.2: Voting with chain-computed weight
    // ========================================================================

    /// Cast a vote on a proposal. Vote weight is chain-computed from the voter's
    /// staking state: `delegated + permanent * multiplier_num / multiplier_den`.
    pub fn cast_vote(
        &self,
        voter: Address,
        proposal_id: u64,
        support: bool,
        current_block: u64,
    ) -> Result<()> {
        let mut proposal = self
            .get_proposal_raw(proposal_id)?
            .ok_or(EconomicsError::ProposalNotFound(proposal_id))?;

        if proposal.status != ProposalStatus::Active {
            return Err(EconomicsError::ProposalNotActive(proposal_id));
        }

        // Verify within voting period.
        if current_block > proposal.end_block {
            return Err(EconomicsError::ProposalNotActive(proposal_id));
        }

        // Verify not already voted.
        let vkey = vote_key(proposal_id, &voter);
        if self.state.get_cf_raw(CF_GOVERNANCE_VOTES, &vkey)?.is_some() {
            return Err(EconomicsError::AlreadyVoted { voter, proposal_id });
        }

        // Compute vote weight (chain-computed, never user-supplied).
        // FIX ECON-FIND-16: Use snapshotted weight from proposal creation time.
        let params = self.get_governance_params()?;
        let weight = self.compute_vote_weight_at(&voter, &params, proposal_id)?;
        if weight.is_zero() {
            return Err(EconomicsError::NoVotingWeight(voter));
        }

        // Update vote tallies.
        if support {
            proposal.votes_for += weight;
        } else {
            proposal.votes_against += weight;
        }
        self.put_proposal(&proposal)?;

        // Store vote record.
        let vote = Vote {
            voter,
            proposal_id,
            support,
            weight,
            block_number: current_block,
        };
        let data = borsh::to_vec(&vote).map_err(|e| EconomicsError::Borsh(e.to_string()))?;
        self.state.put_cf_raw(CF_GOVERNANCE_VOTES, &vkey, &data)?;

        tracing::debug!(%voter, proposal_id, support, %weight, "vote cast");
        Ok(())
    }

    // ========================================================================
    // FIX 20: Abstain vote support (ECON-PF-03)
    // ========================================================================

    /// Cast an abstain vote. Records the vote to prevent double-voting and
    /// contributes to quorum, but does not affect the yes/no tally.
    pub fn cast_vote_abstain(
        &self,
        voter: Address,
        proposal_id: u64,
        current_block: u64,
    ) -> Result<()> {
        let proposal = self
            .get_proposal_raw(proposal_id)?
            .ok_or(EconomicsError::ProposalNotFound(proposal_id))?;

        if proposal.status != ProposalStatus::Active {
            return Err(EconomicsError::ProposalNotActive(proposal_id));
        }

        if current_block > proposal.end_block {
            return Err(EconomicsError::ProposalNotActive(proposal_id));
        }

        // Verify not already voted.
        let vkey = vote_key(proposal_id, &voter);
        if self.state.get_cf_raw(CF_GOVERNANCE_VOTES, &vkey)?.is_some() {
            return Err(EconomicsError::AlreadyVoted { voter, proposal_id });
        }

        let params = self.get_governance_params()?;
        let weight = self.compute_vote_weight(&voter, &params)?;
        if weight.is_zero() {
            return Err(EconomicsError::NoVotingWeight(voter));
        }

        // Store vote record (support=false as placeholder, weight recorded for quorum)
        let vote = Vote {
            voter,
            proposal_id,
            support: false, // abstain -- does not affect tally
            weight,
            block_number: current_block,
        };
        let data = borsh::to_vec(&vote).map_err(|e| EconomicsError::Borsh(e.to_string()))?;
        self.state.put_cf_raw(CF_GOVERNANCE_VOTES, &vkey, &data)?;

        // NOTE: We intentionally do NOT update proposal.votes_for or votes_against
        tracing::debug!(%voter, proposal_id, "abstain vote cast");
        Ok(())
    }

    // ========================================================================
    // 2.8.5: Proposal finalization and execution
    // ========================================================================

    /// Finalize a proposal whose voting period has ended.
    ///
    /// FIX 15: Passed proposals now enter a timelock instead of immediate execution.
    /// FIX 20: Quorum is based on total vote weight (yes + no + abstain).
    pub fn finalize_proposal(
        &self,
        proposal_id: u64,
        current_block: u64,
    ) -> Result<ProposalOutcome> {
        let mut proposal = self
            .get_proposal_raw(proposal_id)?
            .ok_or(EconomicsError::ProposalNotFound(proposal_id))?;

        if proposal.status != ProposalStatus::Active {
            return Err(EconomicsError::ProposalNotActive(proposal_id));
        }

        if current_block <= proposal.end_block {
            return Err(EconomicsError::VotingNotEnded(proposal_id));
        }

        let params = self.get_governance_params()?;

        // Compute quorum: quorum_bps / 10000 of total staked supply.
        let total_staked = self.total_staked_supply()?;
        let quorum = total_staked * U256::from(params.quorum_bps) / U256::from(10_000u64);

        // FIX 20: Use total vote weight (yes + no + abstain) for quorum check.
        let total_votes = self.total_vote_weight(proposal_id)?;

        // PermanentUnlock requires supermajority: votes_for / total_vote_weight >= threshold_bps.
        // Abstains count toward total_vote_weight (conservative: makes passage harder).
        // All other proposal types keep simple majority.
        let passed = if proposal.proposal_type == ProposalType::PermanentUnlock {
            let threshold = U256::from(params.permanent_unlock_threshold_bps);
            total_votes >= quorum
                && !total_votes.is_zero()
                && proposal.votes_for * U256::from(10_000u64) >= threshold * total_votes
        } else {
            proposal.votes_for > proposal.votes_against && total_votes >= quorum
        };

        if !passed {
            proposal.status = ProposalStatus::Rejected;
            self.put_proposal(&proposal)?;
            tracing::info!(proposal_id, "proposal rejected");
            return Ok(ProposalOutcome::Rejected(proposal_id));
        }

        // FIX 15: Passed -- set timelock instead of immediate execution.
        proposal.status = ProposalStatus::Passed;
        proposal.executable_after = current_block + params.timelock_blocks;
        self.put_proposal(&proposal)?;
        tracing::info!(
            proposal_id,
            executable_after = proposal.executable_after,
            "proposal passed, timelock started"
        );
        Ok(ProposalOutcome::Passed(proposal_id))
    }

    /// Execute a passed proposal after its timelock has expired (FIX 15).
    pub fn execute_proposal(
        &self,
        proposal_id: u64,
        current_block: u64,
    ) -> Result<ProposalOutcome> {
        let mut proposal = self
            .get_proposal_raw(proposal_id)?
            .ok_or(EconomicsError::ProposalNotFound(proposal_id))?;

        if proposal.status != ProposalStatus::Passed {
            return Err(EconomicsError::ProposalNotPassed(proposal_id));
        }

        if current_block < proposal.executable_after {
            return Err(EconomicsError::TimelockNotExpired {
                proposal_id,
                executable_after: proposal.executable_after,
                current_block,
            });
        }

        let params = self.get_governance_params()?;

        if let Some(ref payload) = proposal.execution_payload {
            self.execute_payload(payload, &params, current_block)?;
            proposal.status = ProposalStatus::Executed;
            self.put_proposal(&proposal)?;
            tracing::info!(proposal_id, "proposal executed after timelock");
            return Ok(ProposalOutcome::Executed(proposal_id));
        }

        // Text-only proposal already in Passed status -- no execution needed
        Ok(ProposalOutcome::Passed(proposal_id))
    }

    /// Process all active proposals whose voting period has ended,
    /// and execute passed proposals whose timelock has expired (FIX 15).
    /// Called once per block during block validation.
    pub fn process_pending_proposals(&self, current_block: u64) -> Result<Vec<ProposalOutcome>> {
        // FIX MED-NEW-07: Skip full CF scan when no proposals have ever been created.
        let proposal_count = match self.state.get_cf_raw(CF_FEE_CONFIG, PROPOSAL_COUNTER_KEY)? {
            Some(data) if data.len() == 8 => u64::from_be_bytes(data.try_into().unwrap()),
            _ => 0,
        };
        if proposal_count == 0 {
            return Ok(Vec::new());
        }

        let mut outcomes = Vec::new();

        // Finalize active proposals whose voting period has ended.
        let active = self.get_proposals_by_status(ProposalStatus::Active)?;
        for proposal in active {
            if current_block > proposal.end_block {
                let outcome = self.finalize_proposal(proposal.id, current_block)?;
                outcomes.push(outcome);
            }
        }

        // Execute passed proposals whose timelock has expired.
        let passed = self.get_proposals_by_status(ProposalStatus::Passed)?;
        for proposal in passed {
            if current_block >= proposal.executable_after && proposal.execution_payload.is_some() {
                let outcome = self.execute_proposal(proposal.id, current_block)?;
                outcomes.push(outcome);
            }
        }

        Ok(outcomes)
    }

    // ========================================================================
    // Proposal execution helpers
    // ========================================================================

    fn execute_payload(
        &self,
        payload: &ExecutionPayload,
        params: &GovernanceParams,
        current_block: u64,
    ) -> Result<()> {
        match payload {
            ExecutionPayload::ParameterChange {
                param_key,
                new_value,
            } => {
                // FIX 13: Validate parameter change at execution time (defense-in-depth).
                Self::validate_param_change(param_key, new_value)?;
                self.state
                    .put_cf_raw(CF_FEE_CONFIG, param_key.as_bytes(), new_value.as_bytes())?;
                tracing::info!(param_key, new_value, "governance parameter updated");
            }
            ExecutionPayload::TreasurySpend {
                recipient,
                amount,
                reason,
            } => {
                // Debit treasury account.
                let mut treasury_acct = self
                    .state
                    .get_account(&params.treasury_address)?
                    .unwrap_or_default();
                if treasury_acct.balance < *amount {
                    return Err(EconomicsError::InsufficientTreasury {
                        have: treasury_acct.balance,
                        need: *amount,
                    });
                }
                treasury_acct.balance -= *amount;
                self.state
                    .put_account(&params.treasury_address, &treasury_acct)?;

                // Credit recipient.
                let mut recipient_acct = self.state.get_account(recipient)?.unwrap_or_default();
                recipient_acct.balance += *amount;
                self.state.put_account(recipient, &recipient_acct)?;

                tracing::info!(%recipient, %amount, reason, "treasury spend executed");
            }
            ExecutionPayload::MarketListing {
                market_id,
                base_asset,
                quote_asset,
                lot_size,
                tick_size,
                initial_margin,
            } => {
                let key = market_id.to_be_bytes();
                let mut data = Vec::new();
                BorshSerialize::serialize(base_asset, &mut data)
                    .map_err(|e| EconomicsError::Borsh(e.to_string()))?;
                BorshSerialize::serialize(quote_asset, &mut data)
                    .map_err(|e| EconomicsError::Borsh(e.to_string()))?;
                BorshSerialize::serialize(&lot_size.raw(), &mut data)
                    .map_err(|e| EconomicsError::Borsh(e.to_string()))?;
                BorshSerialize::serialize(&tick_size.raw(), &mut data)
                    .map_err(|e| EconomicsError::Borsh(e.to_string()))?;
                BorshSerialize::serialize(&initial_margin.raw(), &mut data)
                    .map_err(|e| EconomicsError::Borsh(e.to_string()))?;
                self.state.put_cf_raw(CF_NATIVE_MARKETS, &key, &data)?;

                tracing::info!(
                    market_id,
                    base_asset,
                    quote_asset,
                    "market listed via governance"
                );
            }
            ExecutionPayload::ValidatorRegistration { candidate } => {
                use crate::types::{ValidatorWhitelistEntry, WHITELIST_EXPIRY_BLOCKS};

                let entry = ValidatorWhitelistEntry {
                    candidate: *candidate,
                    approved_at_block: current_block,
                    expires_at_block: current_block + WHITELIST_EXPIRY_BLOCKS,
                };
                self.put_validator_whitelist(candidate, &entry)?;
                tracing::info!(
                    %candidate,
                    expires = current_block + WHITELIST_EXPIRY_BLOCKS,
                    "validator registration whitelisted via governance"
                );
            }
            ExecutionPayload::PermanentUnlock { staker, amount } => {
                let staking = crate::staking::StakingManager::new(self.state.clone());
                staking.governance_unlock_permanent_stake(*staker, *amount)?;
                tracing::info!(
                    %staker, %amount,
                    "permanent stake unlocked via governance"
                );
            }
        }
        Ok(())
    }

    // ========================================================================
    // 2.8.6: Query functions
    // ========================================================================

    pub fn get_proposal(&self, proposal_id: u64) -> Result<Option<Proposal>> {
        self.get_proposal_raw(proposal_id)
    }

    pub fn get_proposals_by_status(&self, status: ProposalStatus) -> Result<Vec<Proposal>> {
        let entries = self.state.iterate_cf(CF_GOVERNANCE_PROPOSALS, None)?;
        let mut proposals = Vec::new();
        for (key, value) in &entries {
            if key.len() != 8 {
                continue;
            }
            let proposal = Proposal::try_from_slice(value)
                .map_err(|e| EconomicsError::Borsh(e.to_string()))?;
            if proposal.status == status {
                proposals.push(proposal);
            }
        }
        Ok(proposals)
    }

    pub fn get_vote(&self, proposal_id: u64, voter: &Address) -> Result<Option<Vote>> {
        let vkey = vote_key(proposal_id, voter);
        match self.state.get_cf_raw(CF_GOVERNANCE_VOTES, &vkey)? {
            Some(data) => Ok(Some(
                Vote::try_from_slice(&data).map_err(|e| EconomicsError::Borsh(e.to_string()))?,
            )),
            None => Ok(None),
        }
    }

    pub fn get_voter_history(&self, voter: &Address) -> Result<Vec<Vote>> {
        let entries = self.state.iterate_cf(CF_GOVERNANCE_VOTES, None)?;
        let mut votes = Vec::new();
        for (key, value) in &entries {
            // Skip snapshot weight entries (32-byte keys starting with "snap").
            if key.starts_with(b"snap") {
                continue;
            }
            let vote =
                Vote::try_from_slice(value).map_err(|e| EconomicsError::Borsh(e.to_string()))?;
            if vote.voter == *voter {
                votes.push(vote);
            }
        }
        Ok(votes)
    }

    /// FIX MED-NEW-15: Returns error if governance params were never initialized,
    /// instead of silently defaulting to treasury_address = Address::ZERO (which
    /// would permanently burn any treasury spend proposals).
    pub fn get_governance_params(&self) -> Result<GovernanceParams> {
        match self
            .state
            .get_cf_raw(CF_FEE_CONFIG, GOVERNANCE_PARAMS_KEY)?
        {
            Some(data) => Ok(GovernanceParams::try_from_slice(&data)
                .map_err(|e| EconomicsError::Borsh(e.to_string()))?),
            None => Err(EconomicsError::GovernanceNotInitialized),
        }
    }

    pub fn set_governance_params(&self, params: &GovernanceParams) -> Result<()> {
        let data = borsh::to_vec(params).map_err(|e| EconomicsError::Borsh(e.to_string()))?;
        self.state
            .put_cf_raw(CF_FEE_CONFIG, GOVERNANCE_PARAMS_KEY, &data)?;
        Ok(())
    }

    // ========================================================================
    // Validator whitelist (Phase 3: 3.2.1)
    // ========================================================================

    /// Check if a candidate address is whitelisted for validator registration.
    pub fn is_whitelisted(&self, candidate: &Address, current_block: u64) -> Result<bool> {
        match self.get_validator_whitelist(candidate)? {
            Some(entry) => Ok(current_block <= entry.expires_at_block),
            None => Ok(false),
        }
    }

    /// Get a validator whitelist entry.
    pub fn get_validator_whitelist(
        &self,
        candidate: &Address,
    ) -> Result<Option<crate::types::ValidatorWhitelistEntry>> {
        use crate::types::ValidatorWhitelistEntry;
        use torus_state::cf::CF_CONSENSUS_META;

        let key = whitelist_key(candidate);
        match self.state.get_cf_raw(CF_CONSENSUS_META, &key)? {
            Some(data) => Ok(Some(
                ValidatorWhitelistEntry::try_from_slice(&data)
                    .map_err(|e| EconomicsError::Borsh(e.to_string()))?,
            )),
            None => Ok(None),
        }
    }

    /// Store a validator whitelist entry.
    fn put_validator_whitelist(
        &self,
        candidate: &Address,
        entry: &crate::types::ValidatorWhitelistEntry,
    ) -> Result<()> {
        use torus_state::cf::CF_CONSENSUS_META;

        let key = whitelist_key(candidate);
        let data = borsh::to_vec(entry).map_err(|e| EconomicsError::Borsh(e.to_string()))?;
        self.state.put_cf_raw(CF_CONSENSUS_META, &key, &data)?;
        Ok(())
    }

    /// Consume (delete) a validator whitelist entry after registration.
    pub fn consume_whitelist(&self, candidate: &Address) -> Result<()> {
        use torus_state::cf::CF_CONSENSUS_META;

        let key = whitelist_key(candidate);
        self.state.delete_cf_raw(CF_CONSENSUS_META, &key)?;
        Ok(())
    }

    // ========================================================================
    // Private helpers
    // ========================================================================

    /// Compute vote weight: delegated + permanent * multiplier_num / multiplier_den.
    /// FIX 14: Guards against division by zero in permanent_weight_multiplier_den.
    fn compute_vote_weight(&self, voter: &Address, params: &GovernanceParams) -> Result<U256> {
        let delegated = self.total_delegated_for(voter)?;
        let permanent = self.permanent_stake_for(voter)?;
        let den = params.permanent_weight_multiplier_den;
        if den == 0 {
            return Err(EconomicsError::InvalidParameterValue {
                key: "permanent_weight_multiplier_den".to_string(),
                reason: "denominator must not be zero".to_string(),
            });
        }
        let weighted_permanent =
            permanent * U256::from(params.permanent_weight_multiplier_num) / U256::from(den);
        Ok(delegated + weighted_permanent)
    }

    /// FIX ECON-FIND-16: Compute vote weight from the snapshot taken at proposal creation.
    ///
    /// Looks up the voter's snapshotted weight (stored in CF_GOVERNANCE_VOTES under a
    /// `snap_` prefixed key). Falls back to live state for backward compatibility with
    /// proposals created before the snapshot mechanism was added.
    fn compute_vote_weight_at(
        &self,
        voter: &Address,
        params: &GovernanceParams,
        proposal_id: u64,
    ) -> Result<U256> {
        // Try to load snapshotted weight first.
        let snap_key = snapshot_weight_key(proposal_id, voter);
        if let Some(data) = self.state.get_cf_raw(CF_GOVERNANCE_VOTES, &snap_key)? {
            if data.len() == 32 {
                return Ok(U256::from_be_slice(&data));
            }
        }
        // Fallback: compute from live state (pre-snapshot proposals).
        self.compute_vote_weight(voter, params)
    }

    /// FIX ECON-FIND-16: Snapshot all voter weights at proposal creation time.
    ///
    /// Iterates all delegations and permanent stakes, computes each unique voter's
    /// weight, and stores it under a `snap_` prefixed key in CF_GOVERNANCE_VOTES.
    ///
    /// AUDIT MED-NEW-08: O(all_delegations + all_permanent_stakes) per proposal.
    /// Rate-limited by min_proposal_stake requirement. A lazy evaluation approach
    /// (compute weight on-demand at vote time) would eliminate this scan but requires
    /// a larger refactor of the voting flow.
    fn snapshot_voter_weights(&self, proposal_id: u64, params: &GovernanceParams) -> Result<()> {
        use std::collections::HashMap;

        let mut voter_delegated: HashMap<Address, U256> = HashMap::new();

        // Sum delegated amounts per delegator (key = delegator(20) + validator(20)).
        let delegation_entries = self.state.iterate_cf(CF_STAKING_DELEGATIONS, None)?;
        for (key, value) in &delegation_entries {
            if key.len() < 20 {
                continue;
            }
            let delegator = Address::from_slice(&key[..20]);
            let delegation = Delegation::try_from_slice(value)
                .map_err(|e| EconomicsError::Borsh(e.to_string()))?;
            *voter_delegated.entry(delegator).or_insert(U256::ZERO) += delegation.amount;
        }

        // Merge permanent stake data.
        let mut voter_permanent: HashMap<Address, U256> = HashMap::new();
        let permanent_entries = self.state.iterate_cf(CF_STAKING_PERMANENT, None)?;
        for (key, value) in &permanent_entries {
            if key.len() < 20 {
                continue;
            }
            let staker = Address::from_slice(&key[..20]);
            let info = PermanentStakeInfo::try_from_slice(value)
                .map_err(|e| EconomicsError::Borsh(e.to_string()))?;
            voter_permanent.insert(staker, info.amount);
        }

        // Compute and store weighted vote power for each unique voter.
        let den = if params.permanent_weight_multiplier_den == 0 {
            1u64
        } else {
            params.permanent_weight_multiplier_den
        };
        let mut all_voters: std::collections::HashSet<Address> =
            voter_delegated.keys().copied().collect();
        all_voters.extend(voter_permanent.keys());

        for voter in all_voters {
            let delegated = voter_delegated.get(&voter).copied().unwrap_or(U256::ZERO);
            let permanent = voter_permanent.get(&voter).copied().unwrap_or(U256::ZERO);
            let weighted_permanent =
                permanent * U256::from(params.permanent_weight_multiplier_num) / U256::from(den);
            let weight = delegated + weighted_permanent;
            if !weight.is_zero() {
                let snap_key = snapshot_weight_key(proposal_id, &voter);
                self.state.put_cf_raw(
                    CF_GOVERNANCE_VOTES,
                    &snap_key,
                    &weight.to_be_bytes::<32>(),
                )?;
            }
        }

        Ok(())
    }

    /// Total vote weight cast on a proposal (yes + no + abstain) (FIX 20).
    fn total_vote_weight(&self, proposal_id: u64) -> Result<U256> {
        let prefix = proposal_id.to_be_bytes();
        let entries = self.state.iterate_cf(CF_GOVERNANCE_VOTES, Some(&prefix))?;
        let mut total = U256::ZERO;
        for (_key, value) in &entries {
            let vote =
                Vote::try_from_slice(value).map_err(|e| EconomicsError::Borsh(e.to_string()))?;
            total += vote.weight;
        }
        Ok(total)
    }

    /// Sum total staked supply across all delegations and permanent stakes.
    fn total_staked_supply(&self) -> Result<U256> {
        let mut total = U256::ZERO;

        let delegation_entries = self.state.iterate_cf(CF_STAKING_DELEGATIONS, None)?;
        for (_key, value) in &delegation_entries {
            let delegation = Delegation::try_from_slice(value)
                .map_err(|e| EconomicsError::Borsh(e.to_string()))?;
            total += delegation.amount;
        }

        let permanent_entries = self.state.iterate_cf(CF_STAKING_PERMANENT, None)?;
        for (_key, value) in &permanent_entries {
            let info = PermanentStakeInfo::try_from_slice(value)
                .map_err(|e| EconomicsError::Borsh(e.to_string()))?;
            total += info.amount;
        }

        Ok(total)
    }

    /// Total delegated stake for a voter (prefix scan on delegator address).
    fn total_delegated_for(&self, voter: &Address) -> Result<U256> {
        let prefix = voter.as_slice();
        let entries = self
            .state
            .iterate_cf(CF_STAKING_DELEGATIONS, Some(prefix))?;
        let mut total = U256::ZERO;
        for (_key, value) in &entries {
            let delegation = Delegation::try_from_slice(value)
                .map_err(|e| EconomicsError::Borsh(e.to_string()))?;
            total += delegation.amount;
        }
        Ok(total)
    }

    /// Permanent stake for a voter.
    fn permanent_stake_for(&self, voter: &Address) -> Result<U256> {
        match self
            .state
            .get_cf_raw(CF_STAKING_PERMANENT, voter.as_slice())?
        {
            Some(data) => {
                let info = PermanentStakeInfo::try_from_slice(&data)
                    .map_err(|e| EconomicsError::Borsh(e.to_string()))?;
                Ok(info.amount)
            }
            None => Ok(U256::ZERO),
        }
    }

    /// Get and increment the proposal counter. Returns the new ID (starts at 1).
    fn next_proposal_id(&self) -> Result<u64> {
        let current = match self.state.get_cf_raw(CF_FEE_CONFIG, PROPOSAL_COUNTER_KEY)? {
            Some(data) if data.len() == 8 => u64::from_be_bytes(data.try_into().unwrap()),
            _ => 0,
        };
        let next = current + 1;
        self.state
            .put_cf_raw(CF_FEE_CONFIG, PROPOSAL_COUNTER_KEY, &next.to_be_bytes())?;
        Ok(next)
    }

    // ========================================================================
    // CF accessors
    // ========================================================================

    fn get_proposal_raw(&self, id: u64) -> Result<Option<Proposal>> {
        let key = id.to_be_bytes();
        match self.state.get_cf_raw(CF_GOVERNANCE_PROPOSALS, &key)? {
            Some(data) => Ok(Some(
                Proposal::try_from_slice(&data)
                    .map_err(|e| EconomicsError::Borsh(e.to_string()))?,
            )),
            None => Ok(None),
        }
    }

    fn put_proposal(&self, proposal: &Proposal) -> Result<()> {
        let key = proposal.id.to_be_bytes();
        let data = borsh::to_vec(proposal).map_err(|e| EconomicsError::Borsh(e.to_string()))?;
        self.state
            .put_cf_raw(CF_GOVERNANCE_PROPOSALS, &key, &data)?;
        Ok(())
    }
}

/// Build the 28-byte vote key: proposal_id(8 BE) ++ voter(20).
pub fn vote_key(proposal_id: u64, voter: &Address) -> [u8; 28] {
    let mut key = [0u8; 28];
    key[..8].copy_from_slice(&proposal_id.to_be_bytes());
    key[8..28].copy_from_slice(voter.as_slice());
    key
}

/// FIX ECON-FIND-16: Build 32-byte snapshot weight key: "snap" ++ proposal_id(8 BE) ++ voter(20).
/// Uses a different length (32 bytes) than vote_key (28 bytes) to avoid key collisions.
fn snapshot_weight_key(proposal_id: u64, voter: &Address) -> [u8; 32] {
    let mut key = [0u8; 32];
    key[..4].copy_from_slice(b"snap");
    key[4..12].copy_from_slice(&proposal_id.to_be_bytes());
    key[12..32].copy_from_slice(voter.as_slice());
    key
}

/// Build key for validator whitelist: "validator_whitelist:" ++ address(20).
fn whitelist_key(candidate: &Address) -> Vec<u8> {
    let mut key = b"validator_whitelist:".to_vec();
    key.extend_from_slice(candidate.as_slice());
    key
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use torus_state::StateDb;

    fn setup() -> (tempfile::TempDir, GovernanceManager) {
        let dir = tempfile::tempdir().unwrap();
        let db = StateDb::open(dir.path()).unwrap();
        let gm = GovernanceManager::new(db);
        gm.set_governance_params(&GovernanceParams::defaults(Address::ZERO))
            .unwrap();
        (dir, gm)
    }

    fn addr(n: u8) -> Address {
        Address::new([n; 20])
    }

    #[test]
    fn reject_disallowed_parameter_key() {
        let (_dir, _gm) = setup();
        let result = GovernanceManager::<StateDb>::validate_param_change("evil_key", "666");
        assert!(matches!(
            result,
            Err(EconomicsError::ParameterNotModifiable(_))
        ));
    }

    #[test]
    fn reject_out_of_range_parameter() {
        let (_dir, _gm) = setup();
        let result =
            GovernanceManager::<StateDb>::validate_param_change("maintenance_margin_bps", "0");
        assert!(matches!(
            result,
            Err(EconomicsError::InvalidParameterValue { .. })
        ));

        let result =
            GovernanceManager::<StateDb>::validate_param_change("maintenance_margin_bps", "10000");
        assert!(matches!(
            result,
            Err(EconomicsError::InvalidParameterValue { .. })
        ));

        let result =
            GovernanceManager::<StateDb>::validate_param_change("maintenance_margin_bps", "500");
        assert!(result.is_ok());
    }

    #[test]
    fn reject_zero_multiplier_den() {
        let (_dir, _gm) = setup();
        let result = GovernanceManager::<StateDb>::validate_param_change(
            "permanent_weight_multiplier_den",
            "0",
        );
        assert!(matches!(
            result,
            Err(EconomicsError::InvalidParameterValue { .. })
        ));
    }

    #[test]
    fn div_by_zero_guard_in_vote_weight() {
        let (_dir, gm) = setup();
        let mut params = GovernanceParams::defaults(Address::ZERO);
        params.permanent_weight_multiplier_den = 0;
        let result = gm.compute_vote_weight(&addr(1), &params);
        assert!(result.is_err());
    }

    #[test]
    fn proposal_has_executable_after_and_snapshot() {
        let (_dir, _gm) = setup();
        let proposal = Proposal {
            id: 1,
            proposer: addr(1),
            title: "Test".into(),
            description: "Test desc".into(),
            proposal_type: ProposalType::TextProposal,
            status: ProposalStatus::Active,
            votes_for: U256::ZERO,
            votes_against: U256::ZERO,
            start_block: 100,
            end_block: 200,
            executable_after: 0,
            snapshot_block: 100,
            execution_payload: None,
        };
        let data = borsh::to_vec(&proposal).unwrap();
        let restored = Proposal::try_from_slice(&data).unwrap();
        assert_eq!(restored.executable_after, 0);
        assert_eq!(restored.snapshot_block, 100);
    }

    // ====================================================================
    // PermanentUnlock governance tests
    // ====================================================================

    fn wei_gov(tokens: u64) -> U256 {
        U256::from(tokens) * U256::from(10u64).pow(U256::from(18u64))
    }

    /// Setup with short voting period and low quorum for testing.
    fn setup_unlock() -> (tempfile::TempDir, GovernanceManager) {
        let dir = tempfile::tempdir().unwrap();
        let db = StateDb::open(dir.path()).unwrap();
        let gm = GovernanceManager::new(db);
        let mut params = GovernanceParams::defaults(Address::ZERO);
        params.voting_period_blocks = 100;
        params.timelock_blocks = 10;
        params.min_proposal_stake = wei_gov(100);
        params.quorum_bps = 100; // 1% for test simplicity
        gm.set_governance_params(&params).unwrap();
        (dir, gm)
    }

    /// Give voter a delegation entry for voting weight.
    fn give_delegation(gm: &GovernanceManager, voter: Address, amount: U256) {
        let validator = Address::new([0xFF; 20]);
        let key = crate::staking::delegation_key(&voter, &validator);
        let d = Delegation {
            delegator: voter,
            validator,
            amount,
            unbonding: vec![],
        };
        let data = borsh::to_vec(&d).unwrap();
        gm.state()
            .put_cf_raw(CF_STAKING_DELEGATIONS, &key, &data)
            .unwrap();
    }

    /// Create a permanent stake entry directly in the CF.
    fn give_permanent_stake(gm: &GovernanceManager, staker: Address, amount: U256) {
        let info = PermanentStakeInfo {
            staker,
            amount,
            locked_at_block: 1,
        };
        let data = borsh::to_vec(&info).unwrap();
        gm.state()
            .put_cf_raw(CF_STAKING_PERMANENT, staker.as_slice(), &data)
            .unwrap();
    }

    #[test]
    fn permanent_unlock_passes_at_supermajority() {
        let (_dir, gm) = setup_unlock();
        let staker = addr(10);
        let voters: Vec<Address> = (1..=5).map(addr).collect();

        for v in &voters {
            give_delegation(&gm, *v, wei_gov(1000));
        }
        give_permanent_stake(&gm, staker, wei_gov(5000));

        let id = gm
            .submit_proposal(
                voters[0],
                "Unlock staker".into(),
                "desc".into(),
                Some(ExecutionPayload::PermanentUnlock {
                    staker,
                    amount: wei_gov(5000),
                }),
                0,
            )
            .unwrap();

        // 4/5 vote for = 80% (exact supermajority threshold)
        for v in &voters[..4] {
            gm.cast_vote(*v, id, true, 50).unwrap();
        }
        gm.cast_vote(voters[4], id, false, 50).unwrap();

        let outcome = gm.finalize_proposal(id, 101).unwrap();
        assert_eq!(outcome, ProposalOutcome::Passed(id));

        let outcome = gm.execute_proposal(id, 112).unwrap();
        assert_eq!(outcome, ProposalOutcome::Executed(id));

        // Permanent stake removed, balance credited.
        let mgr = crate::staking::StakingManager::new(gm.state().clone());
        assert!(mgr.get_permanent_stake(&staker).unwrap().is_none());
        let acct = gm.state().get_account(&staker).unwrap().unwrap();
        assert_eq!(acct.balance, wei_gov(5000));
    }

    #[test]
    fn permanent_unlock_fails_below_supermajority() {
        let (_dir, gm) = setup_unlock();
        let staker = addr(10);
        let voters: Vec<Address> = (1..=5).map(addr).collect();

        for v in &voters {
            give_delegation(&gm, *v, wei_gov(1000));
        }
        give_permanent_stake(&gm, staker, wei_gov(5000));

        let id = gm
            .submit_proposal(
                voters[0],
                "Unlock staker".into(),
                "desc".into(),
                Some(ExecutionPayload::PermanentUnlock {
                    staker,
                    amount: wei_gov(5000),
                }),
                0,
            )
            .unwrap();

        // 3/5 vote for = 60% (majority but below 80% supermajority)
        for v in &voters[..3] {
            gm.cast_vote(*v, id, true, 50).unwrap();
        }
        for v in &voters[3..] {
            gm.cast_vote(*v, id, false, 50).unwrap();
        }

        let outcome = gm.finalize_proposal(id, 101).unwrap();
        assert_eq!(outcome, ProposalOutcome::Rejected(id));

        // Permanent stake unchanged.
        let mgr = crate::staking::StakingManager::new(gm.state().clone());
        let info = mgr.get_permanent_stake(&staker).unwrap().unwrap();
        assert_eq!(info.amount, wei_gov(5000));
    }

    #[test]
    fn normal_proposal_still_passes_with_simple_majority() {
        let (_dir, gm) = setup_unlock();
        let voters: Vec<Address> = (1..=5).map(addr).collect();

        for v in &voters {
            give_delegation(&gm, *v, wei_gov(1000));
        }

        let id = gm
            .submit_proposal(
                voters[0],
                "Change param".into(),
                "desc".into(),
                Some(ExecutionPayload::ParameterChange {
                    param_key: "maintenance_margin_bps".into(),
                    new_value: "500".into(),
                }),
                0,
            )
            .unwrap();

        // 3/5 = 60% simple majority → should pass for non-PermanentUnlock
        for v in &voters[..3] {
            gm.cast_vote(*v, id, true, 50).unwrap();
        }
        for v in &voters[3..] {
            gm.cast_vote(*v, id, false, 50).unwrap();
        }

        let outcome = gm.finalize_proposal(id, 101).unwrap();
        assert_eq!(outcome, ProposalOutcome::Passed(id));
    }

    #[test]
    fn permanent_unlock_partial() {
        let (_dir, gm) = setup_unlock();
        let staker = addr(10);
        give_permanent_stake(&gm, staker, wei_gov(10000));

        let mgr = crate::staking::StakingManager::new(gm.state().clone());
        mgr.governance_unlock_permanent_stake(staker, wei_gov(3000))
            .unwrap();

        // Remainder stays locked.
        let info = mgr.get_permanent_stake(&staker).unwrap().unwrap();
        assert_eq!(info.amount, wei_gov(7000));

        // Unlocked amount credited to balance.
        let acct = gm.state().get_account(&staker).unwrap().unwrap();
        assert_eq!(acct.balance, wei_gov(3000));
    }

    #[test]
    fn permanent_unlock_full() {
        let (_dir, gm) = setup_unlock();
        let staker = addr(10);
        give_permanent_stake(&gm, staker, wei_gov(5000));

        let mgr = crate::staking::StakingManager::new(gm.state().clone());
        mgr.governance_unlock_permanent_stake(staker, wei_gov(5000))
            .unwrap();

        // Entry completely removed.
        assert!(mgr.get_permanent_stake(&staker).unwrap().is_none());
        let acct = gm.state().get_account(&staker).unwrap().unwrap();
        assert_eq!(acct.balance, wei_gov(5000));
    }

    #[test]
    fn permanent_unlock_with_accrued_rewards() {
        let (_dir, gm) = setup_unlock();
        let staker = addr(10);
        give_permanent_stake(&gm, staker, wei_gov(5000));

        let mgr = crate::staking::StakingManager::new(gm.state().clone());
        mgr.credit_rewards(staker, wei_gov(200)).unwrap();

        mgr.governance_unlock_permanent_stake(staker, wei_gov(5000))
            .unwrap();

        // Balance = principal (5000) + accrued rewards (200).
        let acct = gm.state().get_account(&staker).unwrap().unwrap();
        assert_eq!(acct.balance, wei_gov(5200));

        // Rewards entry cleared.
        assert!(mgr.get_pending_rewards(&staker).unwrap().is_none());
    }

    #[test]
    fn permanent_unlock_staker_not_found() {
        let (_dir, gm) = setup_unlock();
        let staker = addr(10);

        let mgr = crate::staking::StakingManager::new(gm.state().clone());
        let result = mgr.governance_unlock_permanent_stake(staker, wei_gov(1000));
        assert!(matches!(
            result,
            Err(EconomicsError::PermanentStakeNotFound(_))
        ));
    }

    #[test]
    fn permanent_unlock_amount_exceeds_stake() {
        let (_dir, gm) = setup_unlock();
        let staker = addr(10);
        give_permanent_stake(&gm, staker, wei_gov(5000));

        let mgr = crate::staking::StakingManager::new(gm.state().clone());
        let result = mgr.governance_unlock_permanent_stake(staker, wei_gov(6000));
        assert!(matches!(
            result,
            Err(EconomicsError::PermanentUnlockExceedsStake { .. })
        ));

        // Stake unchanged.
        let info = mgr.get_permanent_stake(&staker).unwrap().unwrap();
        assert_eq!(info.amount, wei_gov(5000));
    }
}
