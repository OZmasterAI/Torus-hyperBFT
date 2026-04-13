//! Error types for the economics crate.

use alloy_primitives::{Address, U256};
use torus_state::StateError;

/// Errors from staking, delegation, and reward operations.
#[derive(Debug, thiserror::Error)]
pub enum EconomicsError {
    #[error("state error: {0}")]
    State(#[from] StateError),

    #[error("insufficient balance: have {have}, need {need}")]
    InsufficientBalance { have: U256, need: U256 },

    #[error("validator {0} not found")]
    ValidatorNotFound(Address),

    #[error("validator {0} already registered")]
    ValidatorAlreadyRegistered(Address),

    #[error("delegation not found: {delegator} -> {validator}")]
    DelegationNotFound {
        delegator: Address,
        validator: Address,
    },

    #[error("insufficient delegation: have {have}, want to undelegate {amount}")]
    InsufficientDelegation { have: U256, amount: U256 },

    #[error("self-stake {amount} below minimum {minimum}")]
    BelowMinSelfDelegation { amount: U256, minimum: U256 },

    #[error("commission {rate_bps} bps exceeds max 5000 bps")]
    CommissionTooHigh { rate_bps: u16 },

    #[error("commission change {delta} bps exceeds max 100 bps per update")]
    CommissionChangeTooLarge { delta: u16 },

    #[error("validator {0} is jailed")]
    ValidatorJailed(Address),

    #[error("validator {0} is tombstoned")]
    ValidatorTombstoned(Address),

    #[error("borsh serialization error: {0}")]
    Borsh(String),

    #[error("no rewards to claim for {0}")]
    NoRewards(Address),

    #[error("permanent stake amount must be non-zero")]
    ZeroPermanentStake,

    #[error("deployer {0} not found in dev pool")]
    DeployerNotFound(Address),

    // Governance errors (task 2.8)

    #[error("proposal {0} not found")]
    ProposalNotFound(u64),

    #[error("proposal {0} is not active")]
    ProposalNotActive(u64),

    #[error("already voted on proposal {proposal_id}")]
    AlreadyVoted { voter: Address, proposal_id: u64 },

    #[error("insufficient stake to submit proposal: have {have}, need {need}")]
    InsufficientProposalStake { have: U256, need: U256 },

    #[error("proposal title too long: {len} chars, max {max}")]
    TitleTooLong { len: usize, max: usize },

    #[error("proposal description too long: {len} chars, max {max}")]
    DescriptionTooLong { len: usize, max: usize },

    #[error("voter {0} has no voting weight")]
    NoVotingWeight(Address),

    #[error("proposal {0} voting period has not ended")]
    VotingNotEnded(u64),

    #[error("treasury insufficient balance: have {have}, need {need}")]
    InsufficientTreasury { have: U256, need: U256 },
}
