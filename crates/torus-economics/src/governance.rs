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
use torus_state::StateDb;
use torus_types::FixedPoint;

use crate::error::EconomicsError;
use crate::types::{Delegation, PermanentStakeInfo};

type Result<T> = std::result::Result<T, EconomicsError>;

// ============================================================================
// Constants
// ============================================================================

/// Default voting period: ~7 days at 6-second blocks.
pub const DEFAULT_VOTING_PERIOD_BLOCKS: u64 = 100_800;

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
}

impl BorshSerialize for ProposalType {
    fn serialize<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        let disc: u8 = match self {
            Self::ParameterChange => 0,
            Self::TreasurySpend => 1,
            Self::MarketListing => 2,
            Self::TextProposal => 3,
            Self::ValidatorRegistration => 4,
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
    ValidatorRegistration {
        candidate: Address,
    },
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
        Ok(Self {
            voting_period_blocks,
            quorum_bps,
            min_proposal_stake,
            permanent_weight_multiplier_num,
            permanent_weight_multiplier_den,
            treasury_address,
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
pub struct GovernanceManager {
    state_db: StateDb,
}

impl GovernanceManager {
    pub fn new(state_db: StateDb) -> Self {
        Self { state_db }
    }

    pub fn state_db(&self) -> &StateDb {
        &self.state_db
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

        // Derive proposal type from payload.
        let proposal_type = match &execution_payload {
            Some(ExecutionPayload::ParameterChange { .. }) => ProposalType::ParameterChange,
            Some(ExecutionPayload::TreasurySpend { .. }) => ProposalType::TreasurySpend,
            Some(ExecutionPayload::MarketListing { .. }) => ProposalType::MarketListing,
            Some(ExecutionPayload::ValidatorRegistration { .. }) => {
                ProposalType::ValidatorRegistration
            }
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
            execution_payload,
        };

        self.put_proposal(&proposal)?;

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
        if self
            .state_db
            .get_cf_raw(CF_GOVERNANCE_VOTES, &vkey)?
            .is_some()
        {
            return Err(EconomicsError::AlreadyVoted {
                voter,
                proposal_id,
            });
        }

        // Compute vote weight (chain-computed, never user-supplied).
        let params = self.get_governance_params()?;
        let weight = self.compute_vote_weight(&voter, &params)?;
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
        self.state_db
            .put_cf_raw(CF_GOVERNANCE_VOTES, &vkey, &data)?;

        tracing::debug!(%voter, proposal_id, support, %weight, "vote cast");
        Ok(())
    }

    // ========================================================================
    // 2.8.5: Proposal finalization and execution
    // ========================================================================

    /// Finalize a proposal whose voting period has ended.
    ///
    /// - Passed: votes_for > votes_against AND votes_for >= quorum
    /// - If passed with an execution payload, the payload is applied
    /// - TextProposal: marked as Passed with no execution
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

        let passed = proposal.votes_for > proposal.votes_against && proposal.votes_for >= quorum;

        if !passed {
            proposal.status = ProposalStatus::Rejected;
            self.put_proposal(&proposal)?;
            tracing::info!(proposal_id, "proposal rejected");
            return Ok(ProposalOutcome::Rejected(proposal_id));
        }

        // Execute if there's a payload.
        if let Some(ref payload) = proposal.execution_payload {
            self.execute_payload(payload, &params, current_block)?;
            proposal.status = ProposalStatus::Executed;
            self.put_proposal(&proposal)?;
            tracing::info!(proposal_id, "proposal executed");
            return Ok(ProposalOutcome::Executed(proposal_id));
        }

        // TextProposal: no execution, just mark as Passed.
        proposal.status = ProposalStatus::Passed;
        self.put_proposal(&proposal)?;
        tracing::info!(proposal_id, "text proposal passed");
        Ok(ProposalOutcome::Passed(proposal_id))
    }

    /// Process all active proposals whose voting period has ended.
    /// Called once per block during block validation.
    pub fn process_pending_proposals(
        &self,
        current_block: u64,
    ) -> Result<Vec<ProposalOutcome>> {
        let active = self.get_proposals_by_status(ProposalStatus::Active)?;
        let mut outcomes = Vec::new();
        for proposal in active {
            if current_block > proposal.end_block {
                let outcome = self.finalize_proposal(proposal.id, current_block)?;
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
                self.state_db
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
                    .state_db
                    .get_account(&params.treasury_address)?
                    .unwrap_or_default();
                if treasury_acct.balance < *amount {
                    return Err(EconomicsError::InsufficientTreasury {
                        have: treasury_acct.balance,
                        need: *amount,
                    });
                }
                treasury_acct.balance -= *amount;
                self.state_db
                    .put_account(&params.treasury_address, &treasury_acct)?;

                // Credit recipient.
                let mut recipient_acct =
                    self.state_db.get_account(recipient)?.unwrap_or_default();
                recipient_acct.balance += *amount;
                self.state_db.put_account(recipient, &recipient_acct)?;

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
                self.state_db.put_cf_raw(CF_NATIVE_MARKETS, &key, &data)?;

                tracing::info!(market_id, base_asset, quote_asset, "market listed via governance");
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
        let db = self.state_db.inner();
        let cf = db.cf_handle(CF_GOVERNANCE_PROPOSALS).ok_or_else(|| {
            EconomicsError::State(torus_state::StateError::MissingColumnFamily(
                CF_GOVERNANCE_PROPOSALS.to_string(),
            ))
        })?;
        let iter = db.iterator_cf(cf, rocksdb::IteratorMode::Start);
        let mut proposals = Vec::new();
        for item in iter {
            let (key, value) = item
                .map_err(|e| EconomicsError::State(torus_state::StateError::RocksDb(e)))?;
            // Proposal keys are exactly 8 bytes (u64 BE).
            if key.len() != 8 {
                continue;
            }
            let proposal = Proposal::try_from_slice(&value)
                .map_err(|e| EconomicsError::Borsh(e.to_string()))?;
            if proposal.status == status {
                proposals.push(proposal);
            }
        }
        Ok(proposals)
    }

    pub fn get_vote(&self, proposal_id: u64, voter: &Address) -> Result<Option<Vote>> {
        let vkey = vote_key(proposal_id, voter);
        match self.state_db.get_cf_raw(CF_GOVERNANCE_VOTES, &vkey)? {
            Some(data) => Ok(Some(
                Vote::try_from_slice(&data)
                    .map_err(|e| EconomicsError::Borsh(e.to_string()))?,
            )),
            None => Ok(None),
        }
    }

    pub fn get_voter_history(&self, voter: &Address) -> Result<Vec<Vote>> {
        let db = self.state_db.inner();
        let cf = db.cf_handle(CF_GOVERNANCE_VOTES).ok_or_else(|| {
            EconomicsError::State(torus_state::StateError::MissingColumnFamily(
                CF_GOVERNANCE_VOTES.to_string(),
            ))
        })?;
        let iter = db.iterator_cf(cf, rocksdb::IteratorMode::Start);
        let mut votes = Vec::new();
        for item in iter {
            let (_key, value) = item
                .map_err(|e| EconomicsError::State(torus_state::StateError::RocksDb(e)))?;
            let vote = Vote::try_from_slice(&value)
                .map_err(|e| EconomicsError::Borsh(e.to_string()))?;
            if vote.voter == *voter {
                votes.push(vote);
            }
        }
        Ok(votes)
    }

    pub fn get_governance_params(&self) -> Result<GovernanceParams> {
        match self
            .state_db
            .get_cf_raw(CF_FEE_CONFIG, GOVERNANCE_PARAMS_KEY)?
        {
            Some(data) => Ok(GovernanceParams::try_from_slice(&data)
                .map_err(|e| EconomicsError::Borsh(e.to_string()))?),
            None => Ok(GovernanceParams::defaults(Address::ZERO)),
        }
    }

    pub fn set_governance_params(&self, params: &GovernanceParams) -> Result<()> {
        let data =
            borsh::to_vec(params).map_err(|e| EconomicsError::Borsh(e.to_string()))?;
        self.state_db
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
        match self.state_db.get_cf_raw(CF_CONSENSUS_META, &key)? {
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
        self.state_db.put_cf_raw(CF_CONSENSUS_META, &key, &data)?;
        Ok(())
    }

    /// Consume (delete) a validator whitelist entry after registration.
    pub fn consume_whitelist(&self, candidate: &Address) -> Result<()> {
        use torus_state::cf::CF_CONSENSUS_META;

        let key = whitelist_key(candidate);
        self.state_db.delete_cf_raw(CF_CONSENSUS_META, &key)?;
        Ok(())
    }

    // ========================================================================
    // Private helpers
    // ========================================================================

    /// Compute vote weight: delegated + permanent * multiplier_num / multiplier_den.
    fn compute_vote_weight(
        &self,
        voter: &Address,
        params: &GovernanceParams,
    ) -> Result<U256> {
        let delegated = self.total_delegated_for(voter)?;
        let permanent = self.permanent_stake_for(voter)?;
        let weighted_permanent = permanent
            * U256::from(params.permanent_weight_multiplier_num)
            / U256::from(params.permanent_weight_multiplier_den);
        Ok(delegated + weighted_permanent)
    }

    /// Sum total staked supply across all delegations and permanent stakes.
    fn total_staked_supply(&self) -> Result<U256> {
        let db = self.state_db.inner();
        let mut total = U256::ZERO;

        // Sum all delegation amounts.
        if let Some(cf) = db.cf_handle(CF_STAKING_DELEGATIONS) {
            for item in db.iterator_cf(cf, rocksdb::IteratorMode::Start) {
                let (_key, value) = item.map_err(|e| {
                    EconomicsError::State(torus_state::StateError::RocksDb(e))
                })?;
                let delegation = Delegation::try_from_slice(&value)
                    .map_err(|e| EconomicsError::Borsh(e.to_string()))?;
                total += delegation.amount;
            }
        }

        // Sum all permanent stakes.
        if let Some(cf) = db.cf_handle(CF_STAKING_PERMANENT) {
            for item in db.iterator_cf(cf, rocksdb::IteratorMode::Start) {
                let (_key, value) = item.map_err(|e| {
                    EconomicsError::State(torus_state::StateError::RocksDb(e))
                })?;
                let info = PermanentStakeInfo::try_from_slice(&value)
                    .map_err(|e| EconomicsError::Borsh(e.to_string()))?;
                total += info.amount;
            }
        }

        Ok(total)
    }

    /// Total delegated stake for a voter (prefix scan on delegator address).
    fn total_delegated_for(&self, voter: &Address) -> Result<U256> {
        let db = self.state_db.inner();
        let cf = db.cf_handle(CF_STAKING_DELEGATIONS).ok_or_else(|| {
            EconomicsError::State(torus_state::StateError::MissingColumnFamily(
                CF_STAKING_DELEGATIONS.to_string(),
            ))
        })?;
        let prefix = voter.as_slice();
        let iter = db.prefix_iterator_cf(cf, prefix);
        let mut total = U256::ZERO;
        for item in iter {
            let (key, value) = item
                .map_err(|e| EconomicsError::State(torus_state::StateError::RocksDb(e)))?;
            if !key.starts_with(prefix) {
                break;
            }
            let delegation = Delegation::try_from_slice(&value)
                .map_err(|e| EconomicsError::Borsh(e.to_string()))?;
            total += delegation.amount;
        }
        Ok(total)
    }

    /// Permanent stake for a voter.
    fn permanent_stake_for(&self, voter: &Address) -> Result<U256> {
        match self
            .state_db
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
        let current = match self
            .state_db
            .get_cf_raw(CF_FEE_CONFIG, PROPOSAL_COUNTER_KEY)?
        {
            Some(data) if data.len() == 8 => u64::from_be_bytes(data.try_into().unwrap()),
            _ => 0,
        };
        let next = current + 1;
        self.state_db
            .put_cf_raw(CF_FEE_CONFIG, PROPOSAL_COUNTER_KEY, &next.to_be_bytes())?;
        Ok(next)
    }

    // ========================================================================
    // CF accessors
    // ========================================================================

    fn get_proposal_raw(&self, id: u64) -> Result<Option<Proposal>> {
        let key = id.to_be_bytes();
        match self
            .state_db
            .get_cf_raw(CF_GOVERNANCE_PROPOSALS, &key)?
        {
            Some(data) => Ok(Some(
                Proposal::try_from_slice(&data)
                    .map_err(|e| EconomicsError::Borsh(e.to_string()))?,
            )),
            None => Ok(None),
        }
    }

    fn put_proposal(&self, proposal: &Proposal) -> Result<()> {
        let key = proposal.id.to_be_bytes();
        let data =
            borsh::to_vec(proposal).map_err(|e| EconomicsError::Borsh(e.to_string()))?;
        self.state_db
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

/// Build key for validator whitelist: "validator_whitelist:" ++ address(20).
fn whitelist_key(candidate: &Address) -> Vec<u8> {
    let mut key = b"validator_whitelist:".to_vec();
    key.extend_from_slice(candidate.as_slice());
    key
}
