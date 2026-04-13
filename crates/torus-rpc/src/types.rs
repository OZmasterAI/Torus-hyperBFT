//! JSON-RPC response/request types and hex encoding/decoding helpers.

use alloy_primitives::{Address, Bloom, B256, U256};
use serde::{Deserialize, Serialize};
use torus_types::FixedPoint;

use crate::error::RpcError;

// ============================================================================
// Hex encoding helpers — all return "0x..." strings
// ============================================================================

pub fn hex_u64(v: u64) -> String {
    format!("{:#x}", v)
}

pub fn hex_u128(v: u128) -> String {
    format!("{:#x}", v)
}

pub fn hex_u256(v: U256) -> String {
    if v.is_zero() {
        return "0x0".to_string();
    }
    let bytes = v.to_be_bytes::<32>();
    // Skip leading zeros.
    let start = bytes.iter().position(|&b| b != 0).unwrap_or(31);
    format!("0x{}", hex::encode(&bytes[start..]))
}

pub fn hex_b256(v: B256) -> String {
    format!("0x{}", hex::encode(v.as_slice()))
}

pub fn hex_address(v: Address) -> String {
    format!("0x{}", hex::encode(v.as_slice()))
}

pub fn hex_bytes(data: &[u8]) -> String {
    format!("0x{}", hex::encode(data))
}

pub fn hex_bloom(bloom: &Bloom) -> String {
    format!("0x{}", hex::encode(bloom.as_slice()))
}

// ============================================================================
// Hex decoding helpers — parse "0x..." strings
// ============================================================================

pub fn parse_address(s: &str) -> Result<Address, RpcError> {
    let s = s.strip_prefix("0x").unwrap_or(s);
    let bytes = hex::decode(s).map_err(|e| RpcError::InvalidParams(format!("bad address: {e}")))?;
    if bytes.len() != 20 {
        return Err(RpcError::InvalidParams(format!(
            "address must be 20 bytes, got {}",
            bytes.len()
        )));
    }
    Ok(Address::from_slice(&bytes))
}

pub fn parse_b256(s: &str) -> Result<B256, RpcError> {
    let s = s.strip_prefix("0x").unwrap_or(s);
    let bytes = hex::decode(s).map_err(|e| RpcError::InvalidParams(format!("bad hash: {e}")))?;
    if bytes.len() != 32 {
        return Err(RpcError::InvalidParams(format!(
            "hash must be 32 bytes, got {}",
            bytes.len()
        )));
    }
    Ok(B256::from_slice(&bytes))
}

pub fn parse_u256(s: &str) -> Result<U256, RpcError> {
    let s = s.strip_prefix("0x").unwrap_or(s);
    if s.is_empty() {
        return Ok(U256::ZERO);
    }
    U256::from_str_radix(s, 16).map_err(|e| RpcError::InvalidParams(format!("bad u256: {e}")))
}

pub fn parse_u64(s: &str) -> Result<u64, RpcError> {
    let s = s.strip_prefix("0x").unwrap_or(s);
    u64::from_str_radix(s, 16).map_err(|e| RpcError::InvalidParams(format!("bad u64: {e}")))
}

pub fn parse_u128(s: &str) -> Result<u128, RpcError> {
    let s = s.strip_prefix("0x").unwrap_or(s);
    u128::from_str_radix(s, 16).map_err(|e| RpcError::InvalidParams(format!("bad u128: {e}")))
}

pub fn parse_bytes(s: &str) -> Result<Vec<u8>, RpcError> {
    let s = s.strip_prefix("0x").unwrap_or(s);
    hex::decode(s).map_err(|e| RpcError::InvalidParams(format!("bad hex: {e}")))
}

/// Resolve an Ethereum block tag ("latest", "earliest", "pending", or hex number)
/// to a concrete block number.
pub fn resolve_block_tag(tag: &str, latest: u64) -> Result<u64, RpcError> {
    match tag {
        "latest" | "pending" => Ok(latest),
        "earliest" => Ok(0),
        s => parse_u64(s),
    }
}

// ============================================================================
// Response types
// ============================================================================

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcBlock {
    pub number: String,
    pub hash: String,
    pub parent_hash: String,
    pub nonce: String,
    pub sha3_uncles: String,
    pub logs_bloom: String,
    pub transactions_root: String,
    pub state_root: String,
    pub receipts_root: String,
    pub miner: String,
    pub difficulty: String,
    pub total_difficulty: String,
    pub extra_data: String,
    pub size: String,
    pub gas_limit: String,
    pub gas_used: String,
    pub timestamp: String,
    pub transactions: serde_json::Value,
    pub uncles: Vec<String>,
    pub base_fee_per_gas: Option<String>,
    pub mix_hash: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcTransaction {
    pub hash: String,
    pub nonce: String,
    pub block_hash: String,
    pub block_number: String,
    pub transaction_index: String,
    pub from: String,
    pub to: Option<String>,
    pub value: String,
    pub gas: String,
    pub gas_price: String,
    pub input: String,
    pub v: String,
    pub r: String,
    pub s: String,
    #[serde(rename = "type")]
    pub tx_type: String,
    pub chain_id: Option<String>,
    pub max_fee_per_gas: Option<String>,
    pub max_priority_fee_per_gas: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcReceipt {
    pub transaction_hash: String,
    pub transaction_index: String,
    pub block_hash: String,
    pub block_number: String,
    pub from: String,
    pub to: Option<String>,
    pub cumulative_gas_used: String,
    pub gas_used: String,
    pub contract_address: Option<String>,
    pub logs: Vec<RpcLog>,
    pub logs_bloom: String,
    pub status: String,
    pub effective_gas_price: String,
    #[serde(rename = "type")]
    pub tx_type: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcLog {
    pub address: String,
    pub topics: Vec<String>,
    pub data: String,
    pub block_number: String,
    pub block_hash: String,
    pub transaction_hash: String,
    pub transaction_index: String,
    pub log_index: String,
    pub removed: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FeeHistory {
    pub oldest_block: String,
    pub base_fee_per_gas: Vec<String>,
    pub gas_used_ratio: Vec<f64>,
    pub reward: Option<Vec<Vec<String>>>,
}

// ============================================================================
// Request types
// ============================================================================

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CallRequest {
    pub from: Option<String>,
    pub to: Option<String>,
    pub gas: Option<String>,
    pub gas_price: Option<String>,
    pub max_fee_per_gas: Option<String>,
    pub max_priority_fee_per_gas: Option<String>,
    pub value: Option<String>,
    pub data: Option<String>,
    pub input: Option<String>,
    pub nonce: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LogFilter {
    pub from_block: Option<String>,
    pub to_block: Option<String>,
    pub address: Option<serde_json::Value>,
    pub topics: Option<Vec<Option<serde_json::Value>>>,
}

// ============================================================================
// FixedPoint hex helper
// ============================================================================

/// Format a FixedPoint as a hex string of its raw i128 value.
pub fn hex_fp(v: FixedPoint) -> String {
    let raw = v.raw();
    if raw >= 0 {
        format!("{:#x}", raw as u128)
    } else {
        format!("-{:#x}", (-raw) as u128)
    }
}

// ============================================================================
// Torus Native RPC Response Types
// ============================================================================

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcOrderBook {
    pub market_id: String,
    pub bids: Vec<RpcPriceLevel>,
    pub asks: Vec<RpcPriceLevel>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcPriceLevel {
    pub price: String,
    pub quantity: String,
    pub order_count: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcPosition {
    pub market_id: String,
    pub side: String,
    pub size: String,
    pub entry_price: String,
    pub unrealized_pnl: String,
    pub realized_pnl: String,
    pub margin: String,
    pub margin_mode: String,
    pub liquidation_price: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcBalances {
    pub native_balance: String,
    pub evm_balance: String,
    pub total_margin_used: String,
    pub available_balance: String,
    pub permanent_stake: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcMarketInfo {
    pub market_id: String,
    pub base_asset: String,
    pub quote_asset: String,
    pub lot_size: String,
    pub tick_size: String,
    pub status: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcTrade {
    pub trade_id: String,
    pub market_id: String,
    pub price: String,
    pub quantity: String,
    pub side: String,
    pub block_number: String,
    pub timestamp: String,
}
