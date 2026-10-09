use std::fmt;

use torus_state::StateError;

/// Errors from the EVM execution layer.
#[derive(Debug)]
pub enum EvmError {
    /// Pre-execution validation failed (invalid nonce, insufficient funds, etc.).
    InvalidTransaction(String),
    /// Database error from the state layer.
    Database(StateError),
    /// Block gas limit would be exceeded by including this transaction.
    BlockGasLimitExceeded { cumulative: u64, limit: u64 },
    /// Internal EVM error.
    Internal(String),
    /// R02 branch 5 (owner option A): a precompile hit a LOCAL storage fault
    /// it must not answer around (`CoreError::PrecompileLocalFault`: the read
    /// failed on this node). The execution was aborted (no result); the
    /// consensus app fail-stops the node on it. Never a revert, never
    /// produced by a deterministic error.
    LocalFault(String),
}

impl fmt::Display for EvmError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidTransaction(msg) => write!(f, "invalid transaction: {msg}"),
            Self::Database(e) => write!(f, "database error: {e}"),
            Self::BlockGasLimitExceeded { cumulative, limit } => {
                write!(f, "block gas limit exceeded: {cumulative} > {limit}")
            }
            Self::Internal(msg) => write!(f, "internal evm error: {msg}"),
            Self::LocalFault(msg) => write!(f, "local storage fault: {msg}"),
        }
    }
}

impl std::error::Error for EvmError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Database(e) => Some(e),
            _ => None,
        }
    }
}

impl From<StateError> for EvmError {
    fn from(e: StateError) -> Self {
        Self::Database(e)
    }
}
