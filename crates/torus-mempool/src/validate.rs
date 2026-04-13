use alloy_consensus::transaction::SignerRecoverable;
use alloy_consensus::{Transaction, TxEnvelope};
use alloy_primitives::{Address, U256};
use alloy_rlp::Decodable;

use torus_state::StateDb;

use crate::error::MempoolError;
use crate::evm_pool::EvmPoolEntry;

/// Decode raw bytes into a signed transaction envelope (EIP-2718).
pub fn decode_tx(raw: &[u8]) -> Result<TxEnvelope, MempoolError> {
    TxEnvelope::decode(&mut &raw[..]).map_err(|e| MempoolError::Decode(format!("{e}")))
}

/// Recover the sender address from a decoded transaction.
pub fn recover_sender(tx: &TxEnvelope) -> Result<Address, MempoolError> {
    tx.recover_signer()
        .map_err(|e| MempoolError::SignatureRecovery(format!("{e}")))
}

/// Extract (max_fee_per_gas, max_priority_fee_per_gas) uniformly across tx types.
fn extract_gas_price(tx: &TxEnvelope) -> (u128, u128) {
    let max_fee = tx.max_fee_per_gas();
    let priority_fee = tx.max_priority_fee_per_gas().unwrap_or(max_fee);
    (max_fee, priority_fee)
}

/// Decode, validate, and extract metadata from a raw EVM transaction.
///
/// Checks: RLP decode, chain ID, gas limit, signature recovery, nonce, balance.
pub fn validate_evm_tx(
    raw_rlp: &[u8],
    state: &StateDb,
    chain_id: u64,
    block_gas_limit: u64,
) -> Result<EvmPoolEntry, MempoolError> {
    let tx = decode_tx(raw_rlp)?;

    // Chain ID
    if tx.chain_id() != Some(chain_id) {
        return Err(MempoolError::InvalidChainId {
            have: tx.chain_id(),
            want: chain_id,
        });
    }

    // Gas limit
    let gas_limit = tx.gas_limit();
    if gas_limit > block_gas_limit {
        return Err(MempoolError::GasLimitExceeded {
            tx_gas: gas_limit,
            block_gas: block_gas_limit,
        });
    }

    // Sender recovery
    let sender = recover_sender(&tx)?;

    // State lookup (nonce + balance)
    let account = state
        .get_account(&sender)
        .map_err(|e| MempoolError::State(e.to_string()))?;
    let (state_nonce, balance) = match account {
        Some(info) => (info.nonce, info.balance),
        None => (0, U256::ZERO),
    };

    // Nonce check
    let tx_nonce = tx.nonce();
    if tx_nonce < state_nonce {
        return Err(MempoolError::NonceTooLow {
            sender,
            have: tx_nonce,
            minimum: state_nonce,
        });
    }

    // Balance check: gas_limit * max_fee_per_gas + value
    let (max_fee, priority_fee) = extract_gas_price(&tx);
    let value = tx.value();
    let total_cost = U256::from(gas_limit) * U256::from(max_fee) + value;

    if balance < total_cost {
        return Err(MempoolError::InsufficientBalance {
            sender,
            have: balance,
            need: total_cost,
        });
    }

    let hash = *tx.tx_hash();

    Ok(EvmPoolEntry {
        hash,
        sender,
        nonce: tx_nonce,
        max_fee_per_gas: max_fee,
        max_priority_fee: priority_fee,
        gas_limit,
        value,
        raw_rlp: raw_rlp.to_vec(),
    })
}
