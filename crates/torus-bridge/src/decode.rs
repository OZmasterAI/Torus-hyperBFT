//! RLP transaction decoding — convert signed EVM transaction bytes into revm `TxEnv`.

use alloy_consensus::transaction::SignerRecoverable;
use alloy_consensus::TxEnvelope;
use alloy_primitives::{Address, B256};
use alloy_rlp::Decodable;
use revm::context::TxEnv;

use crate::error::BridgeError;

/// A decoded EVM transaction with its hash and recovered sender.
#[derive(Debug)]
pub struct DecodedTx {
    /// Transaction environment for revm execution.
    pub tx_env: TxEnv,
    /// Transaction hash (keccak256 of the signed RLP).
    pub tx_hash: B256,
    /// Recovered sender address.
    pub sender: Address,
}

/// Decode a single RLP-encoded signed EVM transaction.
pub fn decode_rlp_tx(rlp_bytes: &[u8]) -> Result<DecodedTx, BridgeError> {
    let envelope = TxEnvelope::decode(&mut &rlp_bytes[..])
        .map_err(|e| BridgeError::RlpDecode(format!("{e}")))?;

    let sender = envelope
        .recover_signer()
        .map_err(|e| BridgeError::SignatureRecovery(format!("{e}")))?;

    let tx_hash = *envelope.tx_hash();
    let tx_env = envelope_to_tx_env(&envelope, sender)?;

    Ok(DecodedTx {
        tx_env,
        tx_hash,
        sender,
    })
}

/// Decode a batch of RLP-encoded signed EVM transactions.
pub fn decode_all_txs(rlp_txs: &[Vec<u8>]) -> Result<Vec<DecodedTx>, BridgeError> {
    rlp_txs.iter().map(|b| decode_rlp_tx(b)).collect()
}

/// Convert an alloy `TxEnvelope` into a revm `TxEnv`.
fn envelope_to_tx_env(envelope: &TxEnvelope, sender: Address) -> Result<TxEnv, BridgeError> {
    let tx_env = match envelope {
        TxEnvelope::Legacy(signed) => {
            let tx = signed.tx();
            TxEnv {
                caller: sender,
                gas_limit: tx.gas_limit,
                gas_price: tx.gas_price,
                kind: tx.to,
                value: tx.value,
                data: tx.input.clone(),
                nonce: tx.nonce,
                chain_id: tx.chain_id,
                ..Default::default()
            }
        }
        TxEnvelope::Eip2930(signed) => {
            let tx = signed.tx();
            TxEnv {
                caller: sender,
                gas_limit: tx.gas_limit,
                gas_price: tx.gas_price,
                kind: tx.to,
                value: tx.value,
                data: tx.input.clone(),
                nonce: tx.nonce,
                chain_id: Some(tx.chain_id),
                access_list: tx.access_list.clone(), // FIX CONS-FIND-31
                ..Default::default()
            }
        }
        TxEnvelope::Eip1559(signed) => {
            let tx = signed.tx();
            TxEnv {
                caller: sender,
                gas_limit: tx.gas_limit,
                gas_price: tx.max_fee_per_gas,
                gas_priority_fee: Some(tx.max_priority_fee_per_gas),
                kind: tx.to,
                value: tx.value,
                data: tx.input.clone(),
                nonce: tx.nonce,
                chain_id: Some(tx.chain_id),
                access_list: tx.access_list.clone(), // FIX CONS-FIND-31
                ..Default::default()
            }
        }
        _ => {
            return Err(BridgeError::UnsupportedTxType(
                "only Legacy, EIP-2930, and EIP-1559 transactions are supported".into(),
            ))
        }
    };
    Ok(tx_env)
}
