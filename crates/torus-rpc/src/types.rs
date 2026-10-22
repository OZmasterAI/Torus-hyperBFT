//! JSON-RPC response/request types and hex encoding/decoding helpers.

use alloy_primitives::{Address, Bloom, B256, U256};
use serde::{Deserialize, Serialize};
use torus_state::action_status::Outcome;
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

/// Ethereum JSON-RPC QUANTITY: compact hex with no leading zero digits
/// ("0x0" for zero), unlike the fixed-width DATA helpers below.
pub fn hex_u256(v: U256) -> String {
    format!("{:#x}", v)
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

/// Resolve an Ethereum block tag ("latest", "earliest", "pending", "safe",
/// "finalized", or hex number) to a concrete block number. HotStuff BFT: a
/// committed block is final, so "safe" and "finalized" are the head too.
pub fn resolve_block_tag(tag: &str, latest: u64) -> Result<u64, RpcError> {
    match tag {
        "latest" | "pending" | "safe" | "finalized" => Ok(latest),
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
    /// EIP-2930/1559 access list (Batch EK: EVM-FIND-17).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub access_list: Option<Vec<RpcAccessListItem>>,
}

/// A single entry in an EIP-2930 access list.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcAccessListItem {
    pub address: String,
    pub storage_keys: Vec<String>,
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
    /// EIP-2718 type as a hex quantity (geth `type`); inferred when absent.
    #[serde(rename = "type")]
    pub tx_type: Option<String>,
    /// EIP-2930 access list (geth `accessList`).
    pub access_list: Option<Vec<RpcAccessListItem>>,
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
// FixedPoint decimal helper
// ============================================================================

/// Format a FixedPoint as a decimal string (`"123.45000000"`) for `torus_*`
/// outputs (s80). `eth_*` outputs stay hex.
pub fn dec_fp(v: FixedPoint) -> String {
    v.to_string()
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
    /// Cross: `crate::account::cross_liquidation_price`, `null` when no
    /// mark of this market crosses maintenance (docs/api/position.md).
    /// Isolated: the simplified `entry ∓ isolated margin / size` estimate.
    pub liquidation_price: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcBalances {
    /// Native cash + order margin (collateral, no UPnL).
    pub native_balance: String,
    pub evm_balance: String,
    /// Fields below: `crate::account::AccountFigures`, docs/api/balances.md.
    pub account_value: String,
    pub total_margin_used: String,
    pub maintenance_margin_used: String,
    pub cash_balance: String,
    pub withdrawable: String,
    pub free_margin: String,
    pub permanent_stake: String,
}

/// `torus_getLiquidatorVault`: the liquidator vault's committed cash and
/// open positions.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcLiquidatorVault {
    /// `LIQUIDATOR_VAULT`, hex.
    pub address: String,
    /// Native ledger cash, signed decimal string (as `torus_getBalances` `cashBalance`).
    pub available_balance: String,
    /// `-availableBalance` when it is negative, else 0 (the
    /// `torus_liquidator_vault_deficit` gauge).
    pub deficit: String,
    /// Positions with a non-zero size the vault holds.
    pub open_positions: u64,
}

/// `torus_getUserFees`: the trading fee rates in force and the trader's
/// lifetime net fees.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcUserFees {
    /// Taker fee, ppm of notional (450 = 0.045 %).
    pub taker_fee_ppm: i64,
    /// Maker fee, ppm of notional (negative = rebate).
    pub maker_fee_ppm: i64,
    /// Lifetime net fees paid (rebates subtracted), signed decimal string.
    pub cum_fees: String,
}

/// `torus_getUserLimits`: the per-user open-order limit and its inputs.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcUserLimits {
    /// Resting orders plus pending stops, summed over all markets.
    pub open_orders: u64,
    /// `min(1000 + floor(cumVolume / 5,000,000), 5000)`.
    pub open_order_limit: u32,
    /// Lifetime traded notional (maker + taker), decimal string.
    pub cum_volume: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcMarketInfo {
    pub market_id: String,
    pub base_asset: String,
    pub quote_asset: String,
    pub lot_size: String,
    pub tick_size: String,
    /// `"active"`, or `"delisted"` once a governance delist executed.
    pub status: String,
    /// Delist follow-up (node-local read): a governance delist executed (the
    /// market is reduce-only, then settled).
    #[serde(default)]
    pub delisted: bool,
    /// The height the delist executed (hex), when delisted.
    #[serde(default)]
    pub delist_height: Option<String>,
    /// The grace end (hex): the settlement row's `close_at`, the first block
    /// the settlement step works on the market. `null` before the delist;
    /// kept after the walk (owner s134: the kept settlement record).
    #[serde(default)]
    pub close_at: Option<String>,
    /// `"none"` (not delisted), `"grace"` (reduce-only until `closeAt`),
    /// `"settling"` (from `closeAt`: waiting for a price, closing positions,
    /// or draining ADL obligations) or `"settled"` (complete: the oracle is
    /// closed, prices for it are ignored).
    #[serde(default)]
    pub settlement: String,
    /// The settlement price, once the settlement step fixed it; kept in
    /// state after the walk completes (owner s134: the kept settlement
    /// record), so a settled market still shows it.
    #[serde(default)]
    pub close_price: Option<String>,
    /// A governance close price (`SetDelistClosePrice`) waiting to be used
    /// as the settlement price at the first settlement block.
    #[serde(default)]
    pub governance_close_price: Option<String>,
    /// The height the settlement completed (oracle-closed marker), hex.
    #[serde(default)]
    pub settled_height: Option<String>,
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

// ============================================================================
// Trading RPC Response Types (getOpenOrders, getOpenInterest, getMarkPrice, getUserTrades)
// ============================================================================

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcOpenOrder {
    pub order_id: String,
    pub market_id: String,
    pub side: String,
    pub price: String,
    pub remaining_qty: String,
    pub original_qty: String,
    pub order_type: String,
    pub time_in_force: String,
    pub reduce_only: bool,
    pub client_order_id: Option<String>,
    pub timestamp: u64,
}

/// s114: `torus_getOrderByCloid` — the trader's open order with that client
/// order id. `status` is `"open"` (resting) or `"triggerPending"` (a pending
/// stop: `order.price` is its limit or price cap, `triggerPrice` its trigger).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcOrderByCloid {
    pub status: String,
    pub order: RpcOpenOrder,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger_price: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcOpenInterest {
    pub market_id: String,
    pub long_oi: String,
    pub short_oi: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcMarkPrice {
    pub market_id: String,
    pub mark_price: String,
    pub index_price: String,
    pub last_trade_price: String,
    pub timestamp: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcUserTrade {
    pub trade_id: String,
    pub market_id: String,
    pub side: String,
    pub price: String,
    pub quantity: String,
    pub role: String,
    pub block_number: String,
    pub timestamp: String,
}

/// One `newTrades` stream row (s80): a public trade plus both parties.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcStreamTrade {
    #[serde(flatten)]
    pub trade: RpcTrade,
    pub maker: String,
    pub taker: String,
}

/// One `userFills` stream row (s80): one fill from one party's view.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcUserFill {
    pub trade_id: String,
    pub market_id: String,
    pub side: String,
    pub price: String,
    pub quantity: String,
    pub role: String,
    pub order_id: String,
    pub start_position: String,
    pub closed_pnl: String,
    /// Trading fee the user paid on this fill (negative = rebate). Absent
    /// for blocks before `trading_fees` is active (the row is then exactly
    /// the pre-feature row) and in pre-feature rows (Codex s138 #3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fee: Option<String>,
    pub dir: String,
    pub block_number: String,
    pub timestamp: String,
}

// ============================================================================
// Torus Staking RPC Response Types (2.9.3)
// ============================================================================

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcStakingInfo {
    pub delegated: Vec<RpcDelegation>,
    pub permanent_stake: String,
    pub pending_rewards: String,
    pub unbonding: Vec<RpcUnbonding>,
}

/// Running state hash (`torus_getStateHash`): this node's checkpoint at
/// `height`, the on-chain validator attestations and the quorum hash (`null`
/// until > 2/3 of active stake agreed on one hash).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcStateHash {
    pub height: String,
    pub local_hash: String,
    pub votes: Vec<RpcStateHashVote>,
    pub quorum_hash: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcStateHashVote {
    pub validator: String,
    pub hash: String,
    /// `hash` equals this node's local checkpoint.
    pub matches_local: bool,
}

/// P0.0 phase 6 (`torus_getFeatureSchedule`): this node's activation schedule
/// and the features its binary knows. Heights and bitmaps are hex strings.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcFeatureSchedule {
    pub chain_id: String,
    /// The latest block this RPC serves; `active` / `activeBits` are read at it.
    pub height: String,
    /// The feature bits a header at `height` carries under this schedule.
    pub active_bits: String,
    /// The bits of every feature this binary knows (`KNOWN_FEATURES`).
    pub known_bits: String,
    /// Every feature this binary knows, in id order.
    pub known_features: Vec<RpcKnownFeature>,
    /// Every scheduled feature (genesis `consensus.activations` united with
    /// this binary's built-in table for the chain id), in id order.
    pub schedule: Vec<RpcScheduledFeature>,
    /// The next activation above `height` (`null`: none scheduled).
    pub next_activation_height: Option<String>,
    /// Blocks from `height` to `nextActivationHeight` (`null`: none scheduled).
    pub blocks_until_activation: Option<String>,
    /// This node met a header with a feature its binary does not know
    /// (`UPGRADE REQUIRED`; `null`: never since the start).
    pub upgrade_required: Option<RpcUpgradeRequired>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcKnownFeature {
    /// The genesis name (`consensus.activations` key).
    pub name: String,
    /// The fixed id: the bit position in a header's `features`.
    pub id: u8,
    /// `1 << id`, hex.
    pub bit: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcScheduledFeature {
    pub name: String,
    pub id: u8,
    pub bit: String,
    /// As configured (0 and 1 both mean block 1).
    pub activation_height: String,
    /// On at `height`.
    pub active: bool,
    /// This binary's built-in table names it for the chain id (else genesis only).
    pub built_in: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcUpgradeRequired {
    /// The lowest unknown feature id at `height`.
    pub feature: u32,
    /// The lowest block height met with a feature this binary does not know.
    pub height: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcDelegation {
    pub validator: String,
    pub amount: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcUnbonding {
    pub amount: String,
    pub release_block: String,
}

/// Per-item outcome of a `torus_submitNativeActions` batch call: exactly one of
/// `hash` (accepted) or `error` is set, in submission order.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcSubmitResult {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcValidatorInfo {
    pub address: String,
    pub pubkey: String,
    pub power: String,
    pub commission_bps: u16,
    pub status: String,
    /// s517: the registered hot oracle signer (`SetOracleSigner`); omitted
    /// when none is set. The price feeder checks its registration here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oracle_signer: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcEpochInfo {
    pub current_epoch: String,
    pub epoch_start_block: String,
    pub epoch_end_block: String,
    pub blocks_remaining: String,
    pub epoch_length: String,
}

// ============================================================================
// Torus Governance RPC Response Types (2.9.5)
// ============================================================================

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcProposal {
    pub id: u64,
    pub proposer: String,
    pub title: String,
    pub description: String,
    pub proposal_type: String,
    pub status: String,
    pub votes_for: String,
    pub votes_against: String,
    pub start_block: String,
    pub end_block: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcGovernanceParams {
    pub voting_period_blocks: String,
    pub quorum_bps: String,
    pub min_proposal_stake: String,
    pub permanent_weight_multiplier: String,
    pub treasury_address: String,
}

// ============================================================================
// Treasury Info RPC Response Types
// ============================================================================

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcTreasuryInfo {
    pub treasury_address: String,
    pub treasury_balance: String,
    pub cumulative_burned: String,
    pub cumulative_treasury: String,
    pub current_fee_split: RpcFeeSplit,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcFeeSplit {
    pub burn_bps: u16,
    pub validator_bps: u16,
    pub treasury_bps: u16,
    pub dev_pool_bps: u16,
}

// ============================================================================
// Leader Info Response (direct-to-leader)
// ============================================================================

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcLeaderInfo {
    pub address: String,
    pub peer_id: String,
    pub view: String,
}

// ============================================================================
// Block Body Response (for explorer indexing)
// ============================================================================

/// v2: one native action that executed and failed (`torus_getBlockBody`).
/// For a PlaceOrderBatch, `order` is the first failing order's position in
/// the batch (whose reason / message these are) and `failedOrders` how many of
/// its orders failed; a batch skipped whole (empty / over the cap) has
/// `failedOrders` 0. Any other action: `order` 0, `failedOrders` 1.
/// Row 50: or one that was rejected — `status` `"failed"` or `"rejected"`
/// (the action's `nativeActionStatus`). s100: `reason` is always the stable
/// lowercase name (`margin`, `tick`, `ioc_cancel`, ...); a rejected entry
/// also has `rejectStatus`, the Hyperliquid `*Rejected` order status name
/// (`iocCancelRejected`, ...). `failedOrders` counts a batch's orders that
/// did not execute, failed or rejected.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcActionFailure {
    pub index: u32,
    /// Row 50: `"failed"` or `"rejected"`; absent from a pre-row-50 node.
    #[serde(default)]
    pub status: String,
    /// The stable lowercase reason name (`FailureReason::as_str`). A row 50
    /// node before s100 sent the HL name here for a rejected entry.
    pub reason: String,
    /// s100: the HL `*Rejected` name of a rejected entry
    /// (`FailureReason::hl_rejected_name`); absent for a failed one and
    /// from an older node.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reject_status: Option<String>,
    pub message: String,
    pub order: u32,
    pub failed_orders: u32,
}

/// Per native action `"executed"` / `"skipped"` / `"failed"` / `"rejected"`.
pub fn native_action_labels(s: &torus_state::action_status::BlockActionStatus) -> Vec<String> {
    (0..s.native_skipped.len())
        .map(|i| s.native_label(i).to_string())
        .collect()
}

/// The record's native failures and rejections, RPC-shaped.
pub fn native_action_failures(
    s: &torus_state::action_status::BlockActionStatus,
) -> Vec<RpcActionFailure> {
    s.native_failed
        .iter()
        .map(|f| RpcActionFailure {
            index: f.index,
            status: f.outcome.as_str().to_string(),
            reason: f.reason.as_str().to_string(),
            reject_status: match f.outcome {
                Outcome::Rejected => Some(
                    f.reason
                        .hl_rejected_name()
                        .unwrap_or(f.reason.as_str())
                        .to_string(),
                ),
                _ => None,
            },
            message: f.message.clone(),
            order: f.order,
            failed_orders: f.failed_orders,
        })
        .collect()
}

/// s112 (v4 record): the result of one CoreWriter / lockbox queue row the
/// block drained (`torus_getBlockBody` `coreWriterResults`, queue order). A
/// row is identified by (this block, `seq`); `queuedBlock` is the block whose
/// EVM queued it (normally this block - 1) and `trader` the precompile caller
/// (`msg.sender`), both `null` for a row that did not decode (`kind`
/// `"undecodable"`). `status` is `"executed"`, `"failed"` or `"rejected"`
/// (a CoreWriter order refused with an HL `*Rejected` status, named in
/// `rejectStatus`); `reason` (stable lowercase name) and `message` are
/// absent / empty for an executed row.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcCoreWriterResult {
    pub seq: u64,
    pub kind: String,
    pub trader: Option<String>,
    pub queued_block: Option<String>,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reject_status: Option<String>,
    pub message: String,
    /// s114: the order id an executed `place_order` row's order got
    /// (resting or filled), hex like every order id; absent otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oid: Option<String>,
    /// s114: the client order id the row carried (a `place_order` with one,
    /// a `cancel_by_cloid`), `0x` + 32 hex digits, whatever its status;
    /// absent otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cloid: Option<String>,
    /// s114: hash of the EVM tx (in `queuedBlock`) whose precompile call
    /// queued this row (node-local `cf_core_writer_tx`); `null` when this
    /// node has no link (an undecodable row, or a row queued without one).
    pub tx_hash: Option<String>,
    /// s114: the row's position among the rows its tx queued (0 = the tx's
    /// first successful writer-precompile call); `null` with `txHash`.
    pub call_index: Option<u32>,
}

/// The record's CoreWriter results, RPC-shaped. `link(seq)` is this
/// block's queue row `seq`'s EVM tx link, if the node has one.
pub fn core_writer_results(
    s: &torus_state::action_status::BlockActionStatus,
    link: impl Fn(u64) -> Option<torus_state::corewriter_tx::CoreWriterTxLink>,
) -> Vec<RpcCoreWriterResult> {
    use torus_state::action_status::CoreWriterKind;
    s.core_writer
        .iter()
        .map(|r| {
            let known = r.kind != CoreWriterKind::Undecodable;
            let executed = r.outcome == Outcome::Executed;
            let tx = known.then(|| link(r.seq)).flatten();
            RpcCoreWriterResult {
                seq: r.seq,
                kind: r.kind.as_str().to_string(),
                trader: known.then(|| hex_address(r.trader)),
                queued_block: known.then(|| hex_u64(r.queued_block)),
                status: r.outcome.as_str().to_string(),
                reason: (!executed).then(|| r.reason.as_str().to_string()),
                reject_status: match r.outcome {
                    Outcome::Rejected => Some(
                        r.reason
                            .hl_rejected_name()
                            .unwrap_or(r.reason.as_str())
                            .to_string(),
                    ),
                    _ => None,
                },
                message: r.message.clone(),
                oid: r.oid.map(hex_u128),
                cloid: r.cloid.map(torus_types::cloid_serde::to_hex),
                tx_hash: tx.map(|l| hex_b256(l.tx_hash)),
                call_index: tx.map(|l| l.call_index),
            }
        })
        .collect()
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcBlockBody {
    pub block_number: String,
    pub native_actions: Vec<serde_json::Value>,
    pub native_action_count: u32,
    /// s84: per native action (same order as `native_actions`), `"executed"`
    /// or `"skipped"` (failed its validity check at execution: bad signature
    /// or session, replayed nonce; no state change). `null` while the block
    /// has not executed on this node, or for a block executed before the
    /// record existed.
    /// v2: `"failed"` too — executed, but the executor refused it (open-order
    /// limit, lot size, a modify's margin, ...); see `native_action_failures`.
    /// Row 50: `"rejected"` — an order refused with an HL `*Rejected`
    /// status (IOC / market / FOK without a fill, crossing PostOnly,
    /// reduce-only that cannot reduce, margin, bad stop trigger, off-tick
    /// price, outside the price band).
    #[serde(default)]
    pub native_action_status: Option<Vec<String>>,
    /// v2: every `"failed"` (row 50: and `"rejected"`) native action,
    /// ascending `index`. `null` like
    /// `native_action_status`; empty for a block recorded before v2.
    #[serde(default)]
    pub native_action_failures: Option<Vec<RpcActionFailure>>,
    /// s84: every EVM transaction of the body, in body order, executed or
    /// skipped (the eth methods list only the executed ones). Each is the eth
    /// transaction object with `transactionIndex` = body position (NOT the eth
    /// index), or only `{"hash"}` (keccak of the raw bytes) for a tx that does
    /// not decode or whose signer does not recover.
    #[serde(default)]
    pub evm_transactions: Vec<serde_json::Value>,
    /// s84: per EVM transaction (same order as `evm_transactions`),
    /// `"executed"` (has a receipt, reverted or not) or `"skipped"`
    /// (undecodable or refused by the EVM; no receipt, no state change).
    /// `null` like `native_action_status`.
    #[serde(default)]
    pub evm_transaction_status: Option<Vec<String>>,
    /// s112 (v4): one result per CoreWriter / lockbox queue row this block
    /// drained, queue order (see [`RpcCoreWriterResult`]). `null` like
    /// `native_action_status`; empty when the block drained nothing.
    #[serde(default)]
    pub core_writer_results: Option<Vec<RpcCoreWriterResult>>,
}

#[cfg(test)]
mod action_failure_tests {
    use super::*;
    use torus_state::action_status::{BlockActionStatus, FailureReason, NativeActionFailure};

    /// Every stored reason code reaches `nativeActionFailures` as its stable
    /// name, read from the record (not from the message).
    #[test]
    fn every_reason_code_is_visible_by_name() {
        let all = [
            (FailureReason::Other, "other"),
            (FailureReason::Margin, "margin"),
            (FailureReason::OpenLimit, "open_limit"),
            (FailureReason::Tick, "tick"),
            (FailureReason::Lot, "lot"),
            (FailureReason::Price, "price"),
            (FailureReason::BatchCap, "batch_cap"),
            (FailureReason::Fill, "fill"),
            (FailureReason::PriceBand, "price_band"),
            (FailureReason::ReduceOnly, "reduce_only"),
        ];
        let status = BlockActionStatus {
            core_writer: vec![],
            evm_skipped: vec![],
            native_skipped: vec![false; all.len()],
            native_failed: all
                .iter()
                .enumerate()
                .map(|(i, (r, _))| NativeActionFailure::new(i as u32, 0, 1, *r, "msg".into()))
                .collect(),
        };
        let stored = BlockActionStatus::decode(&status.encode()).unwrap();
        let rpc = native_action_failures(&stored);
        let names: Vec<&str> = rpc.iter().map(|f| f.reason.as_str()).collect();
        let want: Vec<&str> = all.iter().map(|(_, n)| *n).collect();
        assert_eq!(names, want);
        assert!(rpc.iter().all(|f| f.status == "failed"));
        assert!(native_action_labels(&stored).iter().all(|l| l == "failed"));
    }

    /// Row 50: a rejected entry reaches the RPC as status `"rejected"` with
    /// its Hyperliquid `orderStatus` name in `rejectStatus` (s100: `reason`
    /// is always the stable lowercase name); a failed one has no
    /// `rejectStatus` (a margin failure of a modify stays `"margin"`).
    #[test]
    fn rejected_entries_carry_hyperliquid_names() {
        use torus_state::action_status::Outcome;
        let rejected = [
            (FailureReason::IocCancel, "iocCancelRejected"),
            (FailureReason::BadAloPx, "badAloPxRejected"),
            (FailureReason::MarketNoLiquidity, "marketOrderNoLiquidityRejected"),
            (FailureReason::ReduceOnly, "reduceOnlyRejected"),
            (FailureReason::Margin, "perpMarginRejected"),
            (FailureReason::FokCancel, "fokCancelRejected"),
            (FailureReason::BadTriggerPx, "badTriggerPxRejected"),
            // Review S3 (owner): off-tick and price-band placements.
            (FailureReason::Tick, "tickRejected"),
            (FailureReason::PriceBand, "oracleRejected"),
        ];
        let mut native_failed: Vec<_> = rejected
            .iter()
            .enumerate()
            .map(|(i, (r, _))| NativeActionFailure {
                outcome: Outcome::Rejected,
                ..NativeActionFailure::new(i as u32, 0, 1, *r, "msg".into())
            })
            .collect();
        native_failed.push(NativeActionFailure::new(
            9,
            0,
            1,
            FailureReason::Margin,
            "modify".into(),
        ));
        let status = BlockActionStatus {
            evm_skipped: vec![],
            native_skipped: vec![false; 11],
            native_failed,
            core_writer: vec![],
        };
        let stored = BlockActionStatus::decode(&status.encode()).unwrap();
        let rpc = native_action_failures(&stored);
        let got: Vec<_> = rpc
            .iter()
            .map(|f| {
                (
                    f.status.as_str(),
                    f.reason.as_str(),
                    f.reject_status.as_deref(),
                )
            })
            .collect();
        let mut want: Vec<_> = rejected
            .iter()
            .map(|(r, n)| ("rejected", r.as_str(), Some(*n)))
            .collect();
        want.push(("failed", "margin", None));
        assert_eq!(got, want);
        let labels = native_action_labels(&stored);
        assert_eq!(labels[..9], ["rejected"; 9]);
        assert_eq!(labels[9..], ["failed", "executed"]);
    }

    /// s100: the JSON of a rejected and a failed entry. `reason` is the
    /// stable lowercase name for both; `rejectStatus` (the HL name) only on
    /// the rejected one, absent from the failed one. An older node's JSON
    /// (no `rejectStatus`) still reads.
    #[test]
    fn reject_status_json() {
        use torus_state::action_status::Outcome;
        let ioc = FailureReason::IocCancel.book_reject_message();
        let status = BlockActionStatus {
            core_writer: vec![],
            evm_skipped: vec![],
            native_skipped: vec![false; 2],
            native_failed: vec![
                NativeActionFailure {
                    outcome: Outcome::Rejected,
                    ..NativeActionFailure::new(0, 0, 1, FailureReason::IocCancel, ioc.into())
                },
                NativeActionFailure::new(1, 0, 1, FailureReason::Margin, "modify margin".into()),
            ],
        };
        let stored = BlockActionStatus::decode(&status.encode()).unwrap();
        let json = serde_json::to_value(native_action_failures(&stored)).unwrap();
        assert_eq!(
            json,
            serde_json::json!([
                {
                    "index": 0,
                    "status": "rejected",
                    "reason": "ioc_cancel",
                    "rejectStatus": "iocCancelRejected",
                    "message": ioc,
                    "order": 0,
                    "failedOrders": 1
                },
                {
                    "index": 1,
                    "status": "failed",
                    "reason": "margin",
                    "message": "modify margin",
                    "order": 0,
                    "failedOrders": 1
                }
            ])
        );
        let old: RpcActionFailure = serde_json::from_value(serde_json::json!({
            "index": 0,
            "status": "rejected",
            "reason": "iocCancelRejected",
            "message": "m",
            "order": 0,
            "failedOrders": 1
        }))
        .unwrap();
        assert_eq!(old.reject_status, None);
        assert_eq!(old.reason, "iocCancelRejected");
    }
}
