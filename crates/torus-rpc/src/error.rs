//! JSON-RPC error types and conversion to jsonrpsee error objects.

use jsonrpsee::types::ErrorObjectOwned;
use torus_state::StateError;

/// Errors that can occur within the RPC layer.
#[derive(Debug, thiserror::Error)]
pub enum RpcError {
    #[error("state error: {0}")]
    State(#[from] StateError),

    #[error("evm error: {0}")]
    Evm(String),

    #[error("mempool error: {0}")]
    Mempool(String),

    #[error("invalid params: {0}")]
    InvalidParams(String),

    #[error("block not found")]
    BlockNotFound,

    #[error("transaction not found")]
    TxNotFound,

    #[error("query returned too many results ({count}), max {limit}")]
    TooManyResults { count: usize, limit: usize },

    #[error("internal error: {0}")]
    Internal(String),

    #[error("historical data unavailable: block {block} has been pruned. Connect to an archive node for historical queries.")]
    DataPruned { block: u64 },

    /// EVM execution reverted (Batch EK: EVM-FIND-10).
    #[error("execution reverted")]
    ExecutionReverted { data: Option<String> },

    /// Block range too large for eth_getLogs (Batch EK: EVM-PF-15).
    #[error("block range {range} exceeds maximum of {max}")]
    BlockRangeTooLarge { range: u64, max: u64 },

    /// Historical state not available — only latest supported (Batch EK: EVM-PF-14).
    #[error("historical state not available at block {block}, only 'latest' is supported")]
    HistoricalStateUnavailable { block: u64 },

    /// Transaction submission rate limit exceeded (Batch EK: EVM-FIND-19).
    #[error("rate limit exceeded for sender")]
    TxSubmitRateLimit,
}

impl From<RpcError> for ErrorObjectOwned {
    fn from(err: RpcError) -> Self {
        match &err {
            RpcError::InvalidParams(_) => {
                ErrorObjectOwned::owned(-32602, err.to_string(), None::<()>)
            }
            RpcError::BlockNotFound | RpcError::TxNotFound => {
                ErrorObjectOwned::owned(-32001, err.to_string(), None::<()>)
            }
            RpcError::TooManyResults { .. } | RpcError::BlockRangeTooLarge { .. } => {
                ErrorObjectOwned::owned(-32005, err.to_string(), None::<()>)
            }
            RpcError::DataPruned { .. } | RpcError::HistoricalStateUnavailable { .. } => {
                ErrorObjectOwned::owned(-32000, err.to_string(), None::<()>)
            }
            RpcError::ExecutionReverted { ref data } => {
                // EIP-3: code 3 with revert data
                ErrorObjectOwned::owned(3, err.to_string(), data.clone())
            }
            RpcError::TxSubmitRateLimit => {
                ErrorObjectOwned::owned(-32005, err.to_string(), None::<()>)
            }
            RpcError::State(_)
            | RpcError::Evm(_)
            | RpcError::Mempool(_)
            | RpcError::Internal(_) => ErrorObjectOwned::owned(-32603, err.to_string(), None::<()>),
        }
    }
}
