//! RLP transaction decoding — convert signed EVM transaction bytes into revm `TxEnv`.

use alloy_consensus::transaction::SignerRecoverable;
use alloy_consensus::TxEnvelope;
use alloy_primitives::{Address, B256};
use alloy_rlp::Decodable;
use revm::context::{TransactionType, TxEnv};

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

/// The gas limit `rlp_bytes` declares, or `None` for a tx execution skips
/// before running it: not an envelope, or a type Torus does not execute (the
/// same types [`decode_rlp_tx`] refuses). No signature recovery: this runs
/// before every vote (item 7 step 0), so a supported tx with a bad signature
/// still counts its gas (the mempool never admits one).
pub fn declared_gas_limit(rlp_bytes: &[u8]) -> Option<u64> {
    match TxEnvelope::decode(&mut &rlp_bytes[..]).ok()? {
        TxEnvelope::Legacy(s) => Some(s.tx().gas_limit),
        TxEnvelope::Eip2930(s) => Some(s.tx().gas_limit),
        TxEnvelope::Eip1559(s) => Some(s.tx().gas_limit),
        _ => None,
    }
}

/// Decode a batch of RLP-encoded signed EVM transactions.
pub fn decode_all_txs(rlp_txs: &[Vec<u8>]) -> Result<Vec<DecodedTx>, BridgeError> {
    rlp_txs.iter().map(|b| decode_rlp_tx(b)).collect()
}

/// Decode a batch leniently (D3, S392): an undecodable or unsupported tx is
/// logged and skipped, returning each successfully decoded tx with its
/// original index in `rlp_txs`. Used on the committed-block execution path,
/// where one bad tx must never void the whole block's EVM effects.
pub fn decode_txs_lossy(rlp_txs: &[Vec<u8>]) -> Vec<(usize, DecodedTx)> {
    rlp_txs
        .iter()
        .enumerate()
        .filter_map(|(i, raw)| match decode_rlp_tx(raw) {
            Ok(d) => Some((i, d)),
            Err(e) => {
                tracing::warn!(tx_index = i, %e, "skipping undecodable EVM tx in committed block");
                None
            }
        })
        .collect()
}

/// Convert an alloy `TxEnvelope` into a revm `TxEnv`.
fn envelope_to_tx_env(envelope: &TxEnvelope, sender: Address) -> Result<TxEnv, BridgeError> {
    let tx_env = match envelope {
        TxEnvelope::Legacy(signed) => {
            let tx = signed.tx();
            TxEnv {
                tx_type: TransactionType::Legacy as u8,
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
                tx_type: TransactionType::Eip2930 as u8,
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
            // tx_type 2 makes revm charge the effective price
            // min(max_fee, base_fee + tip), the price the receipt reports.
            // Left at the default (legacy) revm charged max_fee_per_gas.
            TxEnv {
                tx_type: TransactionType::Eip1559 as u8,
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
