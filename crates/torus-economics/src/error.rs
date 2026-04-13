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
}
