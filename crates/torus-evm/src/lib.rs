//! EVM execution engine for the Torus-hyperBFT blockchain.
//!
//! Wraps [revm](https://crates.io/crates/revm) (v36, Cancun spec) to execute
//! EVM transactions against a [`torus_state::StateDb`] backend.
//!
//! # Public API
//!
//! - [`EvmExecutor`] — configure and run single-tx or block-level execution.
//! - [`calc_next_block_base_fee`] — EIP-1559 base fee calculation.
//! - [`logs_bloom`] — bloom filter generation from EVM logs.

pub mod bloom;
pub mod eip1559;
pub mod error;
pub mod executor;
pub mod precompile_provider;

pub use bloom::logs_bloom;
pub use eip1559::calc_next_block_base_fee;
pub use error::EvmError;
pub use executor::{
    BlockEnvCfg, BlockExecResult, EvmExecutor, TxExecResult, DEFAULT_BLOCK_GAS_LIMIT,
    TORUS_CHAIN_ID,
};
pub use precompile_provider::TorusPrecompiles;

// Re-export key revm types used in the public API.
pub use revm::context::TxEnv;
pub use revm::database::BundleState;
