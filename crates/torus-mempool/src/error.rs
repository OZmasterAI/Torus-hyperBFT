use alloy_primitives::{Address, B256, U256};
use std::fmt;

/// Errors returned by mempool operations.
#[derive(Debug)]
pub enum MempoolError {
    /// Failed to decode RLP-encoded transaction.
    Decode(String),
    /// Failed to recover sender from signature.
    SignatureRecovery(String),
    /// Transaction nonce is below the sender's current state nonce.
    NonceTooLow {
        sender: Address,
        have: u64,
        minimum: u64,
    },
    /// Sender cannot afford the transaction.
    InsufficientBalance {
        sender: Address,
        have: U256,
        need: U256,
    },
    /// Wrong chain ID.
    InvalidChainId { have: Option<u64>, want: u64 },
    /// Transaction already exists in the pool.
    DuplicateTx(B256),
    /// Pool is full and the transaction doesn't outbid the cheapest.
    PoolFull,
    /// Replacement transaction doesn't meet the minimum gas price bump.
    ReplacementUnderpriced { need_min: u128, got: u128 },
    /// Transaction gas limit exceeds block gas limit.
    GasLimitExceeded { tx_gas: u64, block_gas: u64 },
    /// State read error during validation.
    State(String),
}

impl fmt::Display for MempoolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Decode(e) => write!(f, "tx decode failed: {e}"),
            Self::SignatureRecovery(e) => write!(f, "signature recovery failed: {e}"),
            Self::NonceTooLow {
                sender,
                have,
                minimum,
            } => write!(
                f,
                "nonce too low for {sender}: have {have}, minimum {minimum}"
            ),
            Self::InsufficientBalance { sender, have, need } => {
                write!(
                    f,
                    "insufficient balance for {sender}: have {have}, need {need}"
                )
            }
            Self::InvalidChainId { have, want } => {
                write!(f, "wrong chain_id: have {have:?}, want {want}")
            }
            Self::DuplicateTx(hash) => write!(f, "duplicate tx: {hash}"),
            Self::PoolFull => write!(f, "mempool full"),
            Self::ReplacementUnderpriced { need_min, got } => {
                write!(f, "replacement underpriced: need >= {need_min}, got {got}")
            }
            Self::GasLimitExceeded { tx_gas, block_gas } => {
                write!(f, "tx gas {tx_gas} exceeds block gas {block_gas}")
            }
            Self::State(e) => write!(f, "state error: {e}"),
        }
    }
}

impl std::error::Error for MempoolError {}
