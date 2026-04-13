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
            RpcError::TooManyResults { .. } => {
                ErrorObjectOwned::owned(-32005, err.to_string(), None::<()>)
            }
            RpcError::State(_)
            | RpcError::Evm(_)
            | RpcError::Mempool(_)
            | RpcError::Internal(_) => {
                ErrorObjectOwned::owned(-32603, err.to_string(), None::<()>)
            }
        }
    }
}
