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
    /// Native action pool is full.
    NativePoolFull,
    /// Duplicate native action (same sender + action content + nonce).
    DuplicateNativeAction,
    /// Sender has too many pending native actions in the pool.
    NativeSenderQueueFull { sender: Address },
    /// FIX EVM-FIND-05: Native action validation failed (chain ID, nonce freshness, signature).
    NativeValidationFailed(String),
    /// FIX EVM-FIND-08: Transaction nonce is too far in the future.
    NonceTooFar { sender: Address, have: u64, max: u64 },
    /// D3 (S392): blob (type 3) / set-code (type 4) transactions are not supported.
    UnsupportedTxType { tx_type: u8 },
    /// D4 (S392): max fee per gas is below the current base fee.
    FeeTooLow { max_fee: u128, base_fee: u64 },
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
            Self::NativePoolFull => write!(f, "native action pool full"),
            Self::DuplicateNativeAction => write!(f, "duplicate native action"),
            Self::NativeSenderQueueFull { sender } => {
                write!(f, "too many pending native actions for {sender}")
            }
            Self::NativeValidationFailed(e) => {
                write!(f, "native action validation failed: {e}")
            }
            Self::NonceTooFar { sender, have, max } => {
                write!(f, "nonce too far in future for {sender}: have {have}, max {max}")
            }
            Self::UnsupportedTxType { tx_type } => {
                write!(f, "transaction type not supported: type {tx_type}")
            }
            Self::FeeTooLow { max_fee, base_fee } => {
                write!(
                    f,
                    "max fee per gas ({max_fee}) below current base fee ({base_fee})"
                )
            }
        }
    }
}

impl std::error::Error for MempoolError {}
