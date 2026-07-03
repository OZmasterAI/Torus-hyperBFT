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
/// Checks: RLP decode, tx type, chain ID, gas limit, fee floor, signature
/// recovery, nonce, balance.
pub fn validate_evm_tx(
    raw_rlp: &[u8],
    state: &StateDb,
    chain_id: u64,
    block_gas_limit: u64,
    base_fee: u64,
) -> Result<EvmPoolEntry, MempoolError> {
    let tx = decode_tx(raw_rlp)?;

    // D3 (S392): only Legacy/EIP-2930/EIP-1559 execute on Torus. Blob (type 3)
    // and set-code (type 4) envelopes are rejected before signature recovery so
    // they can never reach the pool or a proposed block.
    match &tx {
        TxEnvelope::Legacy(_) | TxEnvelope::Eip2930(_) | TxEnvelope::Eip1559(_) => {}
        other => {
            let tx_type = match other {
                TxEnvelope::Eip4844(_) => 3,
                TxEnvelope::Eip7702(_) => 4,
                _ => 0xFF,
            };
            return Err(MempoolError::UnsupportedTxType { tx_type });
        }
    }

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

    // D4 (S392): dynamic fee floor. A tx whose max fee can't cover the current
    // base fee would be silently skipped at execution (GasPriceLessThanBasefee)
    // and strand the sender's nonce — reject it up front instead. `base_fee`
    // tracks committed headers (frozen at 1 gwei until the fee market unfreezes).
    let (max_fee, priority_fee) = extract_gas_price(&tx);
    if max_fee < base_fee as u128 {
        return Err(MempoolError::FeeTooLow { max_fee, base_fee });
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

    // FIX EVM-FIND-08: Reject nonces too far in the future to prevent griefing.
    // Without this, an attacker can fill a sender's per-account queue with
    // far-future nonces, blocking legitimate transactions.
    const MAX_NONCE_GAP: u64 = 64;
    if tx_nonce > state_nonce + MAX_NONCE_GAP {
        return Err(MempoolError::NonceTooFar {
            sender,
            have: tx_nonce,
            max: state_nonce + MAX_NONCE_GAP,
        });
    }

    // Balance check: gas_limit * max_fee_per_gas + value
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
