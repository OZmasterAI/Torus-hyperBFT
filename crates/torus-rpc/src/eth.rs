//! `eth_*` JSON-RPC namespace — Ethereum-compatible RPC methods.
//!
//! ## Missing standard endpoints (Batch EK: FIX 16)
//! The following commonly-used endpoints are not yet implemented.
//! jsonrpsee returns -32601 (Method not found) for any unlisted method.
//! - `eth_getBlockReceipts` — batch receipt retrieval
//! - `eth_feeHistory` — implemented but reward percentiles are stubbed
//! - `debug_traceTransaction` — execution tracing
//! - `eth_createAccessList` — EIP-2930 access list generation

use std::sync::atomic::Ordering::Relaxed;

use alloy_consensus::{transaction::SignerRecoverable, Transaction as _, TxEnvelope};
use alloy_primitives::{Address, Bytes, TxKind, B256, U256};
use alloy_rlp::Decodable;
use jsonrpsee::core::{async_trait, RpcResult, SubscriptionResult};
use jsonrpsee::proc_macros::rpc;
use jsonrpsee::types::ErrorObjectOwned;
use jsonrpsee::PendingSubscriptionSink;

use torus_evm::{BlockEnvCfg, TxEnv, DEFAULT_BLOCK_GAS_LIMIT};
use torus_state::cf::{
    CF_BLOCK_BODIES, CF_BLOCK_HASH_TO_NUMBER, CF_BLOCK_HEADERS, CF_RECEIPTS, CF_TX_HASH_TO_LOCATION,
};
use torus_types::{Receipt, TorusBlockBody, TorusBlockHeader};

use crate::error::RpcError;
use crate::types::*;
use crate::RpcState;

#[rpc(server, namespace = "eth")]
pub trait EthApi {
    #[method(name = "chainId")]
    async fn chain_id(&self) -> RpcResult<String>;
    #[method(name = "blockNumber")]
    async fn block_number(&self) -> RpcResult<String>;
    #[method(name = "getBalance")]
    async fn get_balance(&self, addr: String, block: String) -> RpcResult<String>;
    #[method(name = "getCode")]
    async fn get_code(&self, addr: String, block: String) -> RpcResult<String>;
    #[method(name = "getStorageAt")]
    async fn get_storage_at(&self, addr: String, index: String, block: String)
        -> RpcResult<String>;
    #[method(name = "getTransactionCount")]
    async fn get_transaction_count(&self, addr: String, block: String) -> RpcResult<String>;
    #[method(name = "gasPrice")]
    async fn gas_price(&self) -> RpcResult<String>;
    #[method(name = "maxPriorityFeePerGas")]
    async fn max_priority_fee_per_gas(&self) -> RpcResult<String>;
    #[method(name = "sendRawTransaction")]
    async fn send_raw_transaction(&self, data: String) -> RpcResult<String>;
    #[method(name = "getTransactionByHash")]
    async fn get_transaction_by_hash(&self, hash: String) -> RpcResult<Option<RpcTransaction>>;
    #[method(name = "getTransactionReceipt")]
    async fn get_transaction_receipt(&self, hash: String) -> RpcResult<Option<RpcReceipt>>;
    #[method(name = "getBlockByNumber")]
    async fn get_block_by_number(
        &self,
        number: String,
        full_txs: bool,
    ) -> RpcResult<Option<RpcBlock>>;
    #[method(name = "getBlockByHash")]
    async fn get_block_by_hash(&self, hash: String, full_txs: bool) -> RpcResult<Option<RpcBlock>>;
    #[method(name = "call")]
    async fn call(&self, tx: CallRequest, block: String) -> RpcResult<String>;
    #[method(name = "estimateGas")]
    async fn estimate_gas(&self, tx: CallRequest, block: Option<String>) -> RpcResult<String>;
    #[method(name = "getLogs")]
    async fn get_logs(&self, filter: LogFilter) -> RpcResult<Vec<RpcLog>>;
    #[method(name = "feeHistory")]
    async fn fee_history(
        &self,
        block_count: String,
        newest_block: String,
        reward_percentiles: Option<Vec<f64>>,
    ) -> RpcResult<FeeHistory>;
    #[subscription(name = "subscribe" => "subscription", unsubscribe = "unsubscribe", item = serde_json::Value)]
    async fn subscribe(
        &self,
        kind: String,
        params: Option<serde_json::Value>,
    ) -> SubscriptionResult;
}

const EMPTY_UNCLES_HASH: &str =
    "0x1dcc4de8dec75d7aab85b567b6ccd41ad312451b948a7413f0a142fd40d49347";
const ONE_GWEI: u64 = 1_000_000_000;

fn err(e: RpcError) -> ErrorObjectOwned {
    e.into()
}

/// Check if the requested block height has been pruned and return a clear error.
fn check_pruned(state: &RpcState, height: u64) -> Result<(), ErrorObjectOwned> {
    let pruned = state.pruned_up_to.load(Relaxed);
    if pruned > 0 && height < pruned {
        return Err(err(RpcError::DataPruned { block: height }));
    }
    Ok(())
}

pub(crate) fn get_header_with_hash(
    state: &RpcState,
    height: u64,
) -> Result<Option<(TorusBlockHeader, B256, Vec<u8>)>, RpcError> {
    let key = height.to_be_bytes();
    match state.state.get_cf_raw(CF_BLOCK_HEADERS, &key)? {
        Some(bytes) if bytes.len() > 32 => {
            // Format: block_hash(32) || header_json (set by commit_block, FIX 1).
            let hash = B256::from_slice(&bytes[..32]);
            let json_bytes = &bytes[32..];
            let header: TorusBlockHeader = serde_json::from_slice(json_bytes)
                .map_err(|e| RpcError::Internal(format!("header decode: {e}")))?;
            Ok(Some((header, hash, bytes)))
        }
        _ => Ok(None),
    }
}

fn get_body(state: &RpcState, height: u64) -> Result<Option<TorusBlockBody>, RpcError> {
    let key = height.to_be_bytes();
    match state.state.get_cf_raw(CF_BLOCK_BODIES, &key)? {
        Some(bytes) => {
            let body: TorusBlockBody = serde_json::from_slice(&bytes)
                .map_err(|e| RpcError::Internal(format!("body decode: {e}")))?;
            Ok(Some(body))
        }
        None => Ok(None),
    }
}

fn get_block_receipts(
    state: &RpcState,
    height: u64,
    tx_count: u32,
) -> Result<Vec<Receipt>, RpcError> {
    let mut receipts = Vec::new();
    let height_bytes = height.to_be_bytes();
    for idx in 0..tx_count {
        let mut key = [0u8; 12];
        key[..8].copy_from_slice(&height_bytes);
        key[8..12].copy_from_slice(&idx.to_be_bytes());
        if let Some(data) = state.state.get_cf_raw(CF_RECEIPTS, &key)? {
            let r: Receipt = serde_json::from_slice(&data)
                .map_err(|e| RpcError::Internal(format!("receipt decode: {e}")))?;
            receipts.push(r);
        }
    }
    Ok(receipts)
}

fn block_env_from_header(header: &TorusBlockHeader) -> BlockEnvCfg {
    BlockEnvCfg {
        number: header.height,
        timestamp: header.timestamp,
        beneficiary: header.proposer,
        gas_limit: header.evm_gas_limit,
        base_fee: header.base_fee_per_gas,
    }
}

fn build_call_tx_env(call: &CallRequest, chain_id: u64) -> Result<TxEnv, RpcError> {
    let caller = match &call.from {
        Some(f) => parse_address(f)?,
        None => Address::ZERO,
    };
    let kind = match &call.to {
        Some(t) => TxKind::Call(parse_address(t)?),
        None => TxKind::Create,
    };
    let value = match &call.value {
        Some(v) => parse_u256(v)?,
        None => U256::ZERO,
    };
    let data_hex = call.data.as_deref().or(call.input.as_deref());
    let data = match data_hex {
        Some(d) => Bytes::from(parse_bytes(d)?),
        None => Bytes::new(),
    };
    let gas_limit = match &call.gas {
        Some(g) => parse_u64(g)?,
        None => DEFAULT_BLOCK_GAS_LIMIT,
    };
    let gas_price = match &call.gas_price {
        Some(p) => parse_u128(p)?,
        None => match &call.max_fee_per_gas {
            Some(f) => parse_u128(f)?,
            None => 0,
        },
    };
    let gas_priority_fee = match &call.max_priority_fee_per_gas {
        Some(p) => Some(parse_u128(p)?),
        None => None,
    };
    let nonce = match &call.nonce {
        Some(n) => Some(parse_u64(n)?),
        None => None,
    };
    Ok(TxEnv {
        caller,
        gas_limit,
        gas_price,
        gas_priority_fee,
        kind,
        value,
        data,
        nonce: nonce.unwrap_or(0),
        chain_id: Some(chain_id),
        ..Default::default()
    })
}

/// Convert an alloy AccessList to RPC format (Batch EK: EVM-FIND-17).
fn rpc_access_list(al: &alloy_eips::eip2930::AccessList) -> Vec<RpcAccessListItem> {
    al.iter()
        .map(|item| RpcAccessListItem {
            address: hex_address(item.address),
            storage_keys: item.storage_keys.iter().map(|k| hex_b256(*k)).collect(),
        })
        .collect()
}

fn decode_envelope_and_sender(raw: &[u8]) -> Result<(TxEnvelope, Address), RpcError> {
    let envelope = TxEnvelope::decode(&mut &raw[..])
        .map_err(|e| RpcError::Internal(format!("tx rlp decode: {e}")))?;
    let sender = envelope
        .recover_signer()
        .map_err(|e| RpcError::Internal(format!("signer recovery: {e}")))?;
    Ok((envelope, sender))
}

fn build_rpc_tx(
    envelope: &TxEnvelope,
    sender: Address,
    block_hash: B256,
    block_number: u64,
    tx_index: u32,
) -> RpcTransaction {
    let tx_hash = *envelope.tx_hash();
    let to = envelope.to().map(hex_address);
    // FIX 10 (EVM-FIND-17): Include accessList for EIP-2930+ txs.
    // FIX 12 (EVM-FIND-18): Use EIP-155 v value for legacy txs with chain_id.
    let (v_val, r_val, s_val, tx_type, max_fee, max_priority, gas_price, access_list) =
        match envelope {
            TxEnvelope::Legacy(signed) => {
                let sig = signed.signature();
                let recovery_id = if sig.v() { 1u64 } else { 0u64 };
                let v = match signed.tx().chain_id {
                    Some(chain_id) => chain_id * 2 + 35 + recovery_id,
                    None => 27 + recovery_id,
                };
                (
                    v,
                    sig.r(),
                    sig.s(),
                    0u8,
                    None,
                    None,
                    Some(signed.tx().gas_price),
                    None,
                )
            }
            TxEnvelope::Eip2930(signed) => {
                let sig = signed.signature();
                let al = rpc_access_list(&signed.tx().access_list);
                (
                    if sig.v() { 1u64 } else { 0 },
                    sig.r(),
                    sig.s(),
                    1u8,
                    None,
                    None,
                    Some(signed.tx().gas_price),
                    Some(al),
                )
            }
            TxEnvelope::Eip1559(signed) => {
                let sig = signed.signature();
                let tx = signed.tx();
                let al = rpc_access_list(&tx.access_list);
                (
                    if sig.v() { 1u64 } else { 0 },
                    sig.r(),
                    sig.s(),
                    2u8,
                    Some(tx.max_fee_per_gas),
                    Some(tx.max_priority_fee_per_gas),
                    None,
                    Some(al),
                )
            }
            TxEnvelope::Eip4844(signed) => {
                let sig = signed.signature();
                let tx = signed.tx().tx();
                let al = rpc_access_list(&tx.access_list);
                (
                    if sig.v() { 1u64 } else { 0 },
                    sig.r(),
                    sig.s(),
                    3u8,
                    Some(tx.max_fee_per_gas),
                    Some(tx.max_priority_fee_per_gas),
                    None,
                    Some(al),
                )
            }
            TxEnvelope::Eip7702(signed) => {
                let sig = signed.signature();
                let tx = signed.tx();
                let al = rpc_access_list(&tx.access_list);
                (
                    if sig.v() { 1u64 } else { 0 },
                    sig.r(),
                    sig.s(),
                    4u8,
                    Some(tx.max_fee_per_gas),
                    Some(tx.max_priority_fee_per_gas),
                    None,
                    Some(al),
                )
            }
        };
    let display_gas_price = gas_price
        .map(hex_u128)
        .unwrap_or_else(|| max_fee.map(hex_u128).unwrap_or_else(|| "0x0".into()));
    RpcTransaction {
        hash: hex_b256(tx_hash),
        nonce: hex_u64(envelope.nonce()),
        block_hash: hex_b256(block_hash),
        block_number: hex_u64(block_number),
        transaction_index: hex_u64(tx_index as u64),
        from: hex_address(sender),
        to,
        value: hex_u256(envelope.value()),
        gas: hex_u64(envelope.gas_limit()),
        gas_price: display_gas_price,
        input: hex_bytes(envelope.input()),
        v: hex_u64(v_val),
        r: hex_u256(r_val),
        s: hex_u256(s_val),
        tx_type: hex_u64(tx_type as u64),
        chain_id: envelope.chain_id().map(hex_u64),
        max_fee_per_gas: max_fee.map(hex_u128),
        max_priority_fee_per_gas: max_priority.map(hex_u128),
        access_list,
    }
}

/// Empty trie root: keccak256(rlp("")) = keccak256(0x80).
const EMPTY_TRIE_ROOT: &str =
    "0x56e81f171bcc55a6ff8345e692c0f86e5b48e01b996cadc001622fb5e363b421";

fn build_rpc_block(
    header: &TorusBlockHeader,
    hash: B256,
    body_opt: Option<&TorusBlockBody>,
    full_txs: bool,
    state: &RpcState,
) -> Result<RpcBlock, RpcError> {
    let transactions = match (full_txs, body_opt) {
        (true, Some(body)) => {
            let mut txs = Vec::new();
            for (i, raw) in body.evm_transactions.iter().enumerate() {
                if let Ok((envelope, sender)) = decode_envelope_and_sender(raw) {
                    txs.push(
                        serde_json::to_value(build_rpc_tx(
                            &envelope,
                            sender,
                            hash,
                            header.height,
                            i as u32,
                        ))
                        .unwrap_or_default(),
                    );
                }
            }
            serde_json::Value::Array(txs)
        }
        (false, Some(body)) => {
            let mut hashes = Vec::new();
            for raw in &body.evm_transactions {
                if let Ok((envelope, _)) = decode_envelope_and_sender(raw) {
                    hashes.push(serde_json::Value::String(hex_b256(*envelope.tx_hash())));
                }
            }
            serde_json::Value::Array(hashes)
        }
        _ => serde_json::Value::Array(vec![]),
    };
    let size_estimate = 256u64
        + body_opt
            .map(|b| {
                b.evm_transactions
                    .iter()
                    .map(|tx| tx.len() as u64)
                    .sum::<u64>()
            })
            .unwrap_or(0);
    let parent_hash = if header.height > 0 {
        match get_header_with_hash(state, header.height - 1)? {
            Some((_, ph, _)) => hex_b256(ph),
            None => hex_b256(B256::ZERO),
        }
    } else {
        hex_b256(B256::ZERO)
    };
    Ok(RpcBlock {
        number: hex_u64(header.height),
        hash: hex_b256(hash),
        parent_hash,
        nonce: "0x0000000000000000".into(),
        sha3_uncles: EMPTY_UNCLES_HASH.into(),
        logs_bloom: hex_bloom(&header.logs_bloom),
        // FIX 11 (EVM-PF-17): Compute transactions root from tx hashes.
        // Uses keccak256(concat(tx_hashes)) as an approximation — not a full
        // Merkle Patricia Trie, but sufficient for non-light-client verification.
        transactions_root: match body_opt {
            Some(body) if !body.evm_transactions.is_empty() => {
                let mut buf = Vec::new();
                for raw in &body.evm_transactions {
                    if let Ok((env, _)) = decode_envelope_and_sender(raw) {
                        buf.extend_from_slice(env.tx_hash().as_slice());
                    }
                }
                hex_b256(alloy_primitives::keccak256(&buf))
            }
            _ => EMPTY_TRIE_ROOT.into(),
        },
        state_root: hex_b256(header.state_root),
        receipts_root: hex_b256(header.receipts_root),
        miner: hex_address(header.proposer),
        difficulty: "0x0".into(),
        total_difficulty: "0x0".into(),
        extra_data: "0x".into(),
        size: hex_u64(size_estimate),
        gas_limit: hex_u64(header.evm_gas_limit),
        gas_used: hex_u64(header.evm_gas_used),
        timestamp: hex_u64(header.timestamp),
        transactions,
        uncles: vec![],
        base_fee_per_gas: Some(hex_u64(header.base_fee_per_gas)),
        mix_hash: hex_b256(B256::ZERO),
    })
}

fn to_rpc_log(
    log: &torus_types::Log,
    block_number: u64,
    block_hash: B256,
    tx_hash: B256,
    tx_index: u32,
    log_index: u32,
) -> RpcLog {
    RpcLog {
        address: hex_address(log.address),
        topics: log.topics.iter().map(|t| hex_b256(*t)).collect(),
        data: hex_bytes(&log.data),
        block_number: hex_u64(block_number),
        block_hash: hex_b256(block_hash),
        transaction_hash: hex_b256(tx_hash),
        transaction_index: hex_u64(tx_index as u64),
        log_index: hex_u64(log_index as u64),
        removed: false,
    }
}

fn matches_address(log_addr: &Address, filter_addr: &Option<serde_json::Value>) -> bool {
    match filter_addr {
        None => true,
        Some(serde_json::Value::String(s)) => {
            parse_address(s).map(|a| a == *log_addr).unwrap_or(false)
        }
        Some(serde_json::Value::Array(arr)) => arr.iter().any(|v| {
            if let serde_json::Value::String(s) = v {
                parse_address(s).map(|a| a == *log_addr).unwrap_or(false)
            } else {
                false
            }
        }),
        _ => true,
    }
}

fn matches_topics(
    log_topics: &[B256],
    filter_topics: &Option<Vec<Option<serde_json::Value>>>,
) -> bool {
    let topics = match filter_topics {
        None => return true,
        Some(t) => t,
    };
    for (i, topic_filter) in topics.iter().enumerate() {
        let topic_filter = match topic_filter {
            None => continue,
            Some(v) => v,
        };
        let log_topic = match log_topics.get(i) {
            Some(t) => t,
            None => return false,
        };
        match topic_filter {
            serde_json::Value::String(s) => {
                if let Ok(expected) = parse_b256(s) {
                    if *log_topic != expected {
                        return false;
                    }
                } else {
                    return false;
                }
            }
            serde_json::Value::Array(arr) => {
                if !arr.iter().any(|v| {
                    if let serde_json::Value::String(s) = v {
                        parse_b256(s).map(|e| *log_topic == e).unwrap_or(false)
                    } else {
                        false
                    }
                }) {
                    return false;
                }
            }
            serde_json::Value::Null => {}
            _ => return false,
        }
    }
    true
}

#[async_trait]
impl EthApiServer for RpcState {
    async fn chain_id(&self) -> RpcResult<String> {
        Ok(hex_u64(self.chain_id))
    }
    async fn block_number(&self) -> RpcResult<String> {
        Ok(hex_u64(self.latest_height.load(Relaxed)))
    }

    async fn get_balance(&self, addr: String, block: String) -> RpcResult<String> {
        let _block = resolve_block_tag(&block, self.latest_height.load(Relaxed)).map_err(err)?;
        let address = parse_address(&addr).map_err(err)?;
        let account = self
            .state
            .get_account(&address)
            .map_err(RpcError::from)
            .map_err(err)?;
        Ok(hex_u256(account.map(|a| a.balance).unwrap_or(U256::ZERO)))
    }

    async fn get_code(&self, addr: String, block: String) -> RpcResult<String> {
        let _block = resolve_block_tag(&block, self.latest_height.load(Relaxed)).map_err(err)?;
        let address = parse_address(&addr).map_err(err)?;
        match self
            .state
            .get_account(&address)
            .map_err(RpcError::from)
            .map_err(err)?
        {
            Some(info) if info.code_hash != B256::ZERO => {
                match self
                    .state
                    .get_code(&info.code_hash)
                    .map_err(RpcError::from)
                    .map_err(err)?
                {
                    Some(bytes) => Ok(hex_bytes(&bytes)),
                    None => Ok("0x".into()),
                }
            }
            _ => Ok("0x".into()),
        }
    }

    async fn get_storage_at(
        &self,
        addr: String,
        index: String,
        block: String,
    ) -> RpcResult<String> {
        let _block = resolve_block_tag(&block, self.latest_height.load(Relaxed)).map_err(err)?;
        let address = parse_address(&addr).map_err(err)?;
        let slot = parse_u256(&index).map_err(err)?;
        let value = self
            .state
            .get_storage(&address, &slot)
            .map_err(RpcError::from)
            .map_err(err)?;
        Ok(format!("0x{}", hex::encode(value.to_be_bytes::<32>())))
    }

    async fn get_transaction_count(&self, addr: String, block: String) -> RpcResult<String> {
        let address = parse_address(&addr).map_err(err)?;
        if block == "pending" {
            return Ok(hex_u64(self.mempool.pending_nonce(&address)));
        }
        let _block = resolve_block_tag(&block, self.latest_height.load(Relaxed)).map_err(err)?;
        Ok(hex_u64(
            self.state
                .get_account(&address)
                .map_err(RpcError::from)
                .map_err(err)?
                .map(|a| a.nonce)
                .unwrap_or(0),
        ))
    }

    async fn gas_price(&self) -> RpcResult<String> {
        let latest = self.latest_height.load(Relaxed);
        if latest > 0 {
            if let Some((header, _, _)) = get_header_with_hash(self, latest).map_err(err)? {
                if header.base_fee_per_gas > 0 {
                    return Ok(hex_u64(header.base_fee_per_gas));
                }
            }
        }
        Ok(hex_u64(ONE_GWEI))
    }

    async fn max_priority_fee_per_gas(&self) -> RpcResult<String> {
        Ok(hex_u64(ONE_GWEI))
    }

    async fn send_raw_transaction(&self, data: String) -> RpcResult<String> {
        let bytes = parse_bytes(&data).map_err(err)?;
        // FIX 14 (EVM-FIND-19): Rate-limit at submission time, not commit time.
        let (_, sender) = decode_envelope_and_sender(&bytes).map_err(err)?;
        if !self.tx_submit_limiter.check_sender(&sender) {
            return Err(err(RpcError::TxSubmitRateLimit));
        }
        // Keep the raw RLP for the direct-to-leader forward (Option B); add_evm_tx consumes it.
        let raw_for_forward = bytes.clone();
        let hash = self
            .mempool
            .add_evm_tx(bytes)
            .map_err(|e| err(RpcError::Mempool(e.to_string())))?;
        // Option B (EVM tx dissemination): unicast the validated tx to the current leader so it
        // reaches the block producer even when this node is not the proposer (fixes the EVM
        // dead-end where a tx was mined only by the node it was submitted to). No-op if we are
        // the leader. The leader independently re-validates via add_evm_tx.
        self.forward_evm_to_leader(raw_for_forward);
        let _ = self.notifier.pending_txs.send(hash);
        Ok(hex_b256(hash))
    }

    async fn get_transaction_by_hash(&self, hash: String) -> RpcResult<Option<RpcTransaction>> {
        let tx_hash = parse_b256(&hash).map_err(err)?;
        let location = match self
            .state
            .get_cf_raw(CF_TX_HASH_TO_LOCATION, tx_hash.as_slice())
            .map_err(RpcError::from)
            .map_err(err)?
        {
            Some(l) => l,
            None => return Ok(None),
        };
        if location.len() != 12 {
            return Err(err(RpcError::Internal("invalid tx location".into())));
        }
        let height = u64::from_be_bytes(location[..8].try_into().unwrap());
        check_pruned(self, height)?;
        let tx_index = u32::from_be_bytes(location[8..12].try_into().unwrap());
        let body = match get_body(self, height).map_err(err)? {
            Some(b) => b,
            None => return Ok(None),
        };
        let raw = match body.evm_transactions.get(tx_index as usize) {
            Some(r) => r,
            None => return Ok(None),
        };
        let (envelope, sender) = decode_envelope_and_sender(raw).map_err(err)?;
        let block_hash = match get_header_with_hash(self, height).map_err(err)? {
            Some((_, bh, _)) => bh,
            None => B256::ZERO,
        };
        Ok(Some(build_rpc_tx(
            &envelope, sender, block_hash, height, tx_index,
        )))
    }

    async fn get_transaction_receipt(&self, hash: String) -> RpcResult<Option<RpcReceipt>> {
        let tx_hash = parse_b256(&hash).map_err(err)?;
        let location = match self
            .state
            .get_cf_raw(CF_TX_HASH_TO_LOCATION, tx_hash.as_slice())
            .map_err(RpcError::from)
            .map_err(err)?
        {
            Some(l) => l,
            None => return Ok(None),
        };
        if location.len() != 12 {
            return Err(err(RpcError::Internal("invalid tx location".into())));
        }
        let height = u64::from_be_bytes(location[..8].try_into().unwrap());
        check_pruned(self, height)?;
        let tx_index = u32::from_be_bytes(location[8..12].try_into().unwrap());
        let mut receipt_key = [0u8; 12];
        receipt_key[..8].copy_from_slice(&height.to_be_bytes());
        receipt_key[8..12].copy_from_slice(&tx_index.to_be_bytes());
        let receipt_data = match self
            .state
            .get_cf_raw(CF_RECEIPTS, &receipt_key)
            .map_err(RpcError::from)
            .map_err(err)?
        {
            Some(d) => d,
            None => return Ok(None),
        };
        let receipt: Receipt = serde_json::from_slice(&receipt_data)
            .map_err(|e| err(RpcError::Internal(format!("receipt decode: {e}"))))?;
        let body = get_body(self, height).map_err(err)?;
        let (sender, tx_type_num, tx_to) = if let Some(ref body) = body {
            if let Some(raw) = body.evm_transactions.get(tx_index as usize) {
                let (envelope, s) = decode_envelope_and_sender(raw).map_err(err)?;
                let t = match &envelope {
                    TxEnvelope::Legacy(_) => 0u8,
                    TxEnvelope::Eip2930(_) => 1,
                    TxEnvelope::Eip1559(_) => 2,
                    TxEnvelope::Eip4844(_) => 3,
                    TxEnvelope::Eip7702(_) => 4,
                };
                (s, t, envelope.to().map(hex_address))
            } else {
                (Address::ZERO, 0u8, None)
            }
        } else {
            (Address::ZERO, 0u8, None)
        };
        let block_hash = match get_header_with_hash(self, height).map_err(err)? {
            Some((_, bh, _)) => bh,
            None => B256::ZERO,
        };
        let mut log_index_offset: u32 = 0;
        if tx_index > 0 {
            for r in &get_block_receipts(self, height, tx_index).map_err(err)? {
                log_index_offset += r.logs.len() as u32;
            }
        }
        let logs: Vec<RpcLog> = receipt
            .logs
            .iter()
            .enumerate()
            .map(|(i, log)| {
                to_rpc_log(
                    log,
                    height,
                    block_hash,
                    receipt.tx_hash,
                    tx_index,
                    log_index_offset + i as u32,
                )
            })
            .collect();
        Ok(Some(RpcReceipt {
            transaction_hash: hex_b256(receipt.tx_hash),
            transaction_index: hex_u64(tx_index as u64),
            block_hash: hex_b256(block_hash),
            block_number: hex_u64(height),
            from: hex_address(sender),
            to: tx_to,
            cumulative_gas_used: hex_u64(receipt.cumulative_gas_used),
            gas_used: hex_u64(receipt.gas_used),
            contract_address: receipt.contract_address.map(hex_address),
            logs,
            logs_bloom: hex_bloom(&receipt.logs_bloom),
            status: hex_u64(if receipt.status { 1 } else { 0 }),
            effective_gas_price: hex_u64(receipt.effective_gas_price),
            tx_type: hex_u64(tx_type_num as u64),
        }))
    }

    async fn get_block_by_number(
        &self,
        number: String,
        full_txs: bool,
    ) -> RpcResult<Option<RpcBlock>> {
        let latest = self.latest_height.load(Relaxed);
        let height = resolve_block_tag(&number, latest).map_err(err)?;
        let (header, hash, _) = match get_header_with_hash(self, height).map_err(err)? {
            Some(h) => h,
            None => return Ok(None),
        };
        // Check pruning when full body data is needed
        if (full_txs || header.evm_tx_count > 0) && header.evm_tx_count > 0 {
            check_pruned(self, height)?;
        }
        let body = if full_txs || header.evm_tx_count > 0 {
            get_body(self, height).map_err(err)?
        } else {
            None
        };
        Ok(Some(
            build_rpc_block(&header, hash, body.as_ref(), full_txs, self).map_err(err)?,
        ))
    }

    async fn get_block_by_hash(&self, hash: String, full_txs: bool) -> RpcResult<Option<RpcBlock>> {
        let block_hash = parse_b256(&hash).map_err(err)?;
        let height_data = match self
            .state
            .get_cf_raw(CF_BLOCK_HASH_TO_NUMBER, block_hash.as_slice())
            .map_err(RpcError::from)
            .map_err(err)?
        {
            Some(d) => d,
            None => return Ok(None),
        };
        if height_data.len() != 8 {
            return Err(err(RpcError::Internal("invalid block hash index".into())));
        }
        self.get_block_by_number(
            hex_u64(u64::from_be_bytes(height_data[..8].try_into().unwrap())),
            full_txs,
        )
        .await
    }

    async fn call(&self, tx: CallRequest, block: String) -> RpcResult<String> {
        let latest = self.latest_height.load(Relaxed);
        let height = resolve_block_tag(&block, latest).map_err(err)?;
        // FIX 7 (EVM-PF-14): Use the resolved block height, not always latest.
        // We only support latest state — return a clear error for historical queries.
        if height != latest {
            return Err(err(RpcError::HistoricalStateUnavailable { block: height }));
        }
        let block_env = if latest > 0 {
            match get_header_with_hash(self, latest).map_err(err)? {
                Some((h, _, _)) => block_env_from_header(&h),
                None => BlockEnvCfg::default(),
            }
        } else {
            BlockEnvCfg::default()
        };
        let mut tx_env = build_call_tx_env(&tx, self.chain_id).map_err(err)?;
        // FIX 13 (EVM-PF-18): Default nonce to account's current nonce, not 0.
        if tx.nonce.is_none() {
            let caller = tx_env.caller;
            tx_env.nonce = self
                .state
                .get_account(&caller)
                .ok()
                .flatten()
                .map(|a| a.nonce)
                .unwrap_or(0);
        }
        // D2 (S392): call-simulation mode — geth semantics (no base-fee/nonce
        // checks), so bare calls work at height > 0 with a nonzero base fee.
        let (result, _) = self
            .executor
            .execute_call(&self.state, &block_env, tx_env)
            .map_err(|e| err(RpcError::Evm(e.to_string())))?;
        // FIX 15: Return revert reason when execution fails.
        if !result.success {
            return Err(err(RpcError::ExecutionReverted {
                data: if result.output.is_empty() {
                    None
                } else {
                    Some(format!("0x{}", hex::encode(&result.output)))
                },
            }));
        }
        Ok(hex_bytes(&result.output))
    }

    async fn estimate_gas(&self, tx: CallRequest, block: Option<String>) -> RpcResult<String> {
        let latest = self.latest_height.load(Relaxed);
        let height = match block {
            Some(b) => resolve_block_tag(&b, latest).map_err(err)?,
            None => latest,
        };
        // FIX 7 (EVM-PF-14): Consistent block-tag handling.
        if height != latest {
            return Err(err(RpcError::HistoricalStateUnavailable { block: height }));
        }
        let block_env = if latest > 0 {
            match get_header_with_hash(self, latest).map_err(err)? {
                Some((h, _, _)) => block_env_from_header(&h),
                None => BlockEnvCfg::default(),
            }
        } else {
            BlockEnvCfg::default()
        };
        let mut lo: u64 = 21_000;
        let mut hi: u64 = DEFAULT_BLOCK_GAS_LIMIT;
        let mut tx_env = build_call_tx_env(&tx, self.chain_id).map_err(err)?;
        // FIX 13 (EVM-PF-18): Default nonce to account's current nonce.
        if tx.nonce.is_none() {
            let caller = tx_env.caller;
            tx_env.nonce = self
                .state
                .get_account(&caller)
                .ok()
                .flatten()
                .map(|a| a.nonce)
                .unwrap_or(0);
        }
        tx_env.gas_limit = hi;
        // D2 (S392): call-simulation mode — see `call` above.
        let (result, _) = self
            .executor
            .execute_call(&self.state, &block_env, tx_env)
            .map_err(|e| err(RpcError::Evm(e.to_string())))?;
        // FIX 8 (EVM-FIND-10): Return error on revert instead of gas used.
        // FIX 15: Include revert reason in error data.
        if !result.success {
            return Err(err(RpcError::ExecutionReverted {
                data: if result.output.is_empty() {
                    None
                } else {
                    Some(format!("0x{}", hex::encode(&result.output)))
                },
            }));
        }
        while lo + 1 < hi {
            let mid = lo + (hi - lo) / 2;
            let mut tx_env = build_call_tx_env(&tx, self.chain_id).map_err(err)?;
            tx_env.gas_limit = mid;
            match self.executor.execute_call(&self.state, &block_env, tx_env) {
                Ok((r, _)) if r.success => hi = mid,
                _ => lo = mid,
            }
        }
        Ok(hex_u64(hi))
    }

    async fn get_logs(&self, filter: LogFilter) -> RpcResult<Vec<RpcLog>> {
        let latest = self.latest_height.load(Relaxed);
        let from = match &filter.from_block {
            Some(b) => resolve_block_tag(b, latest).map_err(err)?,
            None => latest,
        };
        let to = match &filter.to_block {
            Some(b) => resolve_block_tag(b, latest).map_err(err)?,
            None => latest,
        };
        // FIX 9 (EVM-PF-15): Reject queries spanning too many blocks.
        const MAX_LOG_BLOCK_RANGE: u64 = 10_000;
        let range = to.saturating_sub(from);
        if range > MAX_LOG_BLOCK_RANGE {
            return Err(err(RpcError::BlockRangeTooLarge {
                range,
                max: MAX_LOG_BLOCK_RANGE,
            }));
        }
        // Check if the requested range includes pruned blocks
        check_pruned(self, from)?;
        const MAX_LOGS: usize = 10_000;
        let mut all_logs = Vec::new();
        for height in from..=to {
            let (header, block_hash, _) = match get_header_with_hash(self, height).map_err(err)? {
                Some(h) => h,
                None => continue,
            };
            if header.evm_tx_count == 0 {
                continue;
            }
            let receipts = get_block_receipts(self, height, header.evm_tx_count).map_err(err)?;
            let mut global_log_index: u32 = 0;
            for receipt in &receipts {
                for log in &receipt.logs {
                    if !matches_address(&log.address, &filter.address) {
                        global_log_index += 1;
                        continue;
                    }
                    if !matches_topics(&log.topics, &filter.topics) {
                        global_log_index += 1;
                        continue;
                    }
                    all_logs.push(to_rpc_log(
                        log,
                        height,
                        block_hash,
                        receipt.tx_hash,
                        receipt.tx_index,
                        global_log_index,
                    ));
                    if all_logs.len() > MAX_LOGS {
                        return Err(err(RpcError::TooManyResults {
                            count: all_logs.len(),
                            limit: MAX_LOGS,
                        }));
                    }
                    global_log_index += 1;
                }
            }
        }
        Ok(all_logs)
    }

    async fn fee_history(
        &self,
        block_count: String,
        newest_block: String,
        reward_percentiles: Option<Vec<f64>>,
    ) -> RpcResult<FeeHistory> {
        const MAX_FEE_HISTORY_BLOCKS: u64 = 1024;

        let latest = self.latest_height.load(Relaxed);
        let count = parse_u64(&block_count).map_err(err)?.min(MAX_FEE_HISTORY_BLOCKS);
        let newest = resolve_block_tag(&newest_block, latest).map_err(err)?;
        let oldest = newest.saturating_sub(count.saturating_sub(1));
        let mut base_fees = Vec::new();
        let mut gas_ratios = Vec::new();
        let mut rewards: Option<Vec<Vec<String>>> = reward_percentiles.as_ref().map(|_| Vec::new());
        for height in oldest..=newest {
            match get_header_with_hash(self, height).map_err(err)? {
                Some((header, _, _)) => {
                    base_fees.push(hex_u64(header.base_fee_per_gas));
                    gas_ratios.push(if header.evm_gas_limit > 0 {
                        header.evm_gas_used as f64 / header.evm_gas_limit as f64
                    } else {
                        0.0
                    });
                    if let Some(ref mut rw) = rewards {
                        rw.push(
                            reward_percentiles
                                .as_ref()
                                .unwrap()
                                .iter()
                                .map(|_| "0x0".into())
                                .collect(),
                        );
                    }
                }
                None => {
                    base_fees.push(hex_u64(0));
                    gas_ratios.push(0.0);
                    if let Some(ref mut rw) = rewards {
                        rw.push(
                            reward_percentiles
                                .as_ref()
                                .unwrap()
                                .iter()
                                .map(|_| "0x0".into())
                                .collect(),
                        );
                    }
                }
            }
        }
        if let Some((header, _, _)) = get_header_with_hash(self, newest + 1).map_err(err)? {
            base_fees.push(hex_u64(header.base_fee_per_gas));
        } else if let Some(last_fee) = base_fees.last().cloned() {
            base_fees.push(last_fee);
        }
        Ok(FeeHistory {
            oldest_block: hex_u64(oldest),
            base_fee_per_gas: base_fees,
            gas_used_ratio: gas_ratios,
            reward: rewards,
        })
    }

    async fn subscribe(
        &self,
        pending: PendingSubscriptionSink,
        kind: String,
        params: Option<serde_json::Value>,
    ) -> SubscriptionResult {
        const MAX_SUBSCRIPTIONS: usize = 1000;

        let count = self.active_subscriptions.load(Relaxed);
        if count >= MAX_SUBSCRIPTIONS {
            pending
                .reject(ErrorObjectOwned::owned(
                    -32000,
                    "subscription limit reached",
                    None::<()>,
                ))
                .await;
            return Ok(());
        }
        self.active_subscriptions.fetch_add(1, Relaxed);

        let sink = pending.accept().await?;
        let subs = self.active_subscriptions.clone();

        match kind.as_str() {
            "newHeads" => {
                let mut rx = self.notifier.new_heads.subscribe();
                tokio::spawn(async move {
                    while let Ok(head) = rx.recv().await {
                        match jsonrpsee::SubscriptionMessage::new(
                            "eth_subscription",
                            sink.subscription_id(),
                            &head,
                        ) {
                            Ok(msg) => {
                                if sink.send(msg).await.is_err() {
                                    break;
                                }
                            }
                            Err(_) => break,
                        }
                    }
                    subs.fetch_sub(1, Relaxed);
                });
            }
            "logs" => {
                let filter: Option<LogFilter> = params.and_then(|p| serde_json::from_value(p).ok());
                let filter_addr = filter.as_ref().and_then(|f| f.address.clone());
                let mut rx = self.notifier.new_logs.subscribe();
                let subs = subs.clone();
                tokio::spawn(async move {
                    while let Ok(logs) = rx.recv().await {
                        for log_val in &logs {
                            let should_send = match &filter_addr {
                                Some(fa) => log_val
                                    .get("address")
                                    .and_then(|a| a.as_str())
                                    .and_then(|s| parse_address(s).ok())
                                    .map(|a| matches_address(&a, &Some(fa.clone())))
                                    .unwrap_or(true),
                                None => true,
                            };
                            if should_send {
                                match jsonrpsee::SubscriptionMessage::new(
                                    "eth_subscription",
                                    sink.subscription_id(),
                                    log_val,
                                ) {
                                    Ok(msg) => {
                                        if sink.send(msg).await.is_err() {
                                            subs.fetch_sub(1, Relaxed);
                                            return;
                                        }
                                    }
                                    Err(_) => {
                                        subs.fetch_sub(1, Relaxed);
                                        return;
                                    }
                                }
                            }
                        }
                    }
                    subs.fetch_sub(1, Relaxed);
                });
            }
            "newPendingTransactions" => {
                let mut rx = self.notifier.pending_txs.subscribe();
                let subs = subs.clone();
                tokio::spawn(async move {
                    while let Ok(hash) = rx.recv().await {
                        let val = serde_json::Value::String(hex_b256(hash));
                        match jsonrpsee::SubscriptionMessage::new(
                            "eth_subscription",
                            sink.subscription_id(),
                            &val,
                        ) {
                            Ok(msg) => {
                                if sink.send(msg).await.is_err() {
                                    break;
                                }
                            }
                            Err(_) => break,
                        }
                    }
                    subs.fetch_sub(1, Relaxed);
                });
            }
            _ => {
                subs.fetch_sub(1, Relaxed);
                tracing::warn!("unknown subscription kind: {kind}");
            }
        }
        Ok(())
    }
}
