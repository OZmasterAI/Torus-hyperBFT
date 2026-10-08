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
    /// Replacement transaction doesn't raise both the max fee and the max
    /// priority fee by the minimum bump (geth rule).
    ReplacementUnderpriced {
        need_max_fee: u128,
        got_max_fee: u128,
        need_priority_fee: u128,
        got_priority_fee: u128,
    },
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
    /// Anti-spam item A: the sender (session owner for session actions) holds
    /// less than the node's `TORUS_INGRESS_MIN_COLLATERAL` TRS. Not retryable
    /// until the account is funded.
    UnfundedSender { sender: Address, min_trs: u64 },
    /// FIX EVM-FIND-08: Transaction nonce is too far in the future.
    NonceTooFar {
        sender: Address,
        have: u64,
        max: u64,
    },
    /// D3 (S392): blob (type 3) / set-code (type 4) transactions are not supported.
    UnsupportedTxType { tx_type: u8 },
    /// D4 (S392): max fee per gas is below the current base fee.
    FeeTooLow { max_fee: u128, base_fee: u64 },
    /// Review #2 (s104): EIP-1559 max priority fee above the max fee (revm
    /// rejects it at execution).
    PriorityFeeAboveMaxFee { priority_fee: u128, max_fee: u128 },
    /// Gas limit below the tx's intrinsic gas (revm skips such a tx at
    /// execution, which would strand the sender's nonce).
    IntrinsicGasTooLow { need: u64, got: u64 },
    /// EIP-3860: contract-creation initcode above the size limit (revm
    /// rejects it at execution).
    InitCodeTooLarge { size: usize, max: usize },
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
            Self::ReplacementUnderpriced {
                need_max_fee,
                got_max_fee,
                need_priority_fee,
                got_priority_fee,
            } => write!(
                f,
                "replacement transaction underpriced: need max fee >= {need_max_fee} \
                 and priority fee >= {need_priority_fee}, got {got_max_fee} and {got_priority_fee}"
            ),
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
            Self::UnfundedSender { sender, min_trs } => write!(
                f,
                "account not funded: {sender} holds less than {min_trs} TRS (spot + perp); \
                 deposit before sending actions"
            ),
            Self::NonceTooFar { sender, have, max } => {
                write!(
                    f,
                    "nonce too far in future for {sender}: have {have}, max {max}"
                )
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
            Self::PriorityFeeAboveMaxFee {
                priority_fee,
                max_fee,
            } => write!(
                f,
                "max priority fee per gas ({priority_fee}) above max fee per gas ({max_fee})"
            ),
            Self::IntrinsicGasTooLow { need, got } => {
                write!(f, "intrinsic gas too low: have {got}, want {need}")
            }
            Self::InitCodeTooLarge { size, max } => {
                write!(
                    f,
                    "max initcode size exceeded: code size {size}, limit {max}"
                )
            }
        }
    }
}

impl std::error::Error for MempoolError {}
