//! Consensus-execution bridge for Torus-hyperBFT.
//!
//! Connects the hotstuff_rs consensus layer to the EVM execution engine,
//! implementing block proposal, validation, and commitment pipelines.

pub mod committer;
pub mod decode;
pub mod error;
pub mod market_workers;
pub mod native_executor;
pub mod proposer;
pub mod state_root;
pub mod validator;

pub use committer::BlockCommitter;
pub use decode::{decode_all_txs, decode_rlp_tx, DecodedTx};
pub use error::BridgeError;
pub use native_executor::{
    sort_native_actions, ActionCategory, NativeActionResult, NativeBatchResult, NativeExecContext,
    NativeExecutor,
};
pub use proposer::{genesis_parent_header, BlockProposer, ProposedBlock};
pub use validator::{BlockValidator, ValidatedBlock, merge_bundle_into};
pub use revm::database::BundleState;
