//! Bridge error types.

use std::fmt;

use alloy_primitives::B256;

/// Errors from the consensus-execution bridge.
#[derive(Debug)]
pub enum BridgeError {
    /// Failed to RLP-decode a transaction envelope.
    RlpDecode(String),
    /// Failed to recover the transaction signer.
    SignatureRecovery(String),
    /// Transaction type not supported (e.g. blob, 7702).
    UnsupportedTxType(String),
    /// EVM execution error.
    Evm(torus_evm::EvmError),
    /// State layer error.
    State(torus_state::StateError),
    /// Computed state root does not match the block header.
    StateRootMismatch { expected: B256, computed: B256 },
    /// Block structure is invalid.
    InvalidBlock(String),
    /// Serialization / deserialization error.
    Serialization(String),
    /// Native action execution error.
    NativeExecution(String),
    /// Core module error (order book, margin, liquidation, oracle).
    Core(torus_core::error::CoreError),
    /// Economics module error (staking, governance, rewards).
    Economics(torus_economics::EconomicsError),
}

impl fmt::Display for BridgeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RlpDecode(e) => write!(f, "RLP decode error: {e}"),
            Self::SignatureRecovery(e) => write!(f, "signature recovery error: {e}"),
            Self::UnsupportedTxType(t) => write!(f, "unsupported tx type: {t}"),
            Self::Evm(e) => write!(f, "EVM error: {e}"),
            Self::State(e) => write!(f, "state error: {e}"),
            Self::StateRootMismatch { expected, computed } => {
                write!(
                    f,
                    "state root mismatch: expected {expected}, computed {computed}"
                )
            }
            Self::InvalidBlock(msg) => write!(f, "invalid block: {msg}"),
            Self::Serialization(e) => write!(f, "serialization error: {e}"),
            Self::NativeExecution(e) => write!(f, "native execution error: {e}"),
            Self::Core(e) => write!(f, "core error: {e}"),
            Self::Economics(e) => write!(f, "economics error: {e}"),
        }
    }
}

impl std::error::Error for BridgeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Evm(e) => Some(e),
            Self::State(e) => Some(e),
            Self::Core(e) => Some(e),
            Self::Economics(e) => Some(e),
            _ => None,
        }
    }
}

impl From<torus_evm::EvmError> for BridgeError {
    fn from(e: torus_evm::EvmError) -> Self {
        Self::Evm(e)
    }
}

impl From<torus_state::StateError> for BridgeError {
    fn from(e: torus_state::StateError) -> Self {
        Self::State(e)
    }
}

impl From<torus_core::error::CoreError> for BridgeError {
    fn from(e: torus_core::error::CoreError) -> Self {
        Self::Core(e)
    }
}

impl From<torus_economics::EconomicsError> for BridgeError {
    fn from(e: torus_economics::EconomicsError) -> Self {
        Self::Economics(e)
    }
}
