//! Torus staking, delegation, epoch rotation, reward distribution, fee split,
//! developer pool, and on-chain governance.
//!
//! Implements tasks 1.11.1–1.11.4 and Phase 2 tasks 2.6–2.8:
//! - Delegate/undelegate state management with unbonding queue
//! - Epoch-based validator set rotation
//! - Delegator reward distribution (fee split + permanent staking rewards)
//! - Staking query functions (for RPC wiring)
//! - FeeSplitter with BPS interpolation, burn, treasury, validator rewards (2.7)
//! - Developer pool gas tracking and pro-rata distribution (2.7.5)
//! - On-chain governance: proposals, voting, execution (2.8)

pub mod dev_pool;
pub mod epoch;
pub mod error;
pub mod governance;
pub mod queries;
pub mod rewards;
pub mod staking;
pub mod types;

pub use dev_pool::DevPool;
pub use epoch::EpochManager;
pub use error::EconomicsError;
pub use governance::{GovernanceManager, GovernanceParams};
pub use queries::*;
pub use rewards::{lerp_bps, FeeSplitter, RewardDistributor};
pub use staking::StakingManager;
pub use types::*;
