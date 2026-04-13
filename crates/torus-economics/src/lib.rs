//! Torus staking, delegation, epoch rotation, and reward distribution.
//!
//! Implements tasks 1.11.1–1.11.4:
//! - Delegate/undelegate state management with unbonding queue
//! - Epoch-based validator set rotation
//! - Delegator reward distribution (fee split + permanent staking rewards)
//! - Staking query functions (for RPC wiring)

pub mod epoch;
pub mod error;
pub mod queries;
pub mod rewards;
pub mod staking;
pub mod types;

pub use epoch::EpochManager;
pub use error::EconomicsError;
pub use queries::*;
pub use rewards::{lerp_bps, RewardDistributor};
pub use staking::StakingManager;
pub use types::*;
