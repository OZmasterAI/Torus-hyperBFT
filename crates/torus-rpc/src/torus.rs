//! `torus_*` JSON-RPC namespace — native exchange, staking, governance endpoints.

use std::sync::atomic::Ordering::{self, Relaxed};

use alloy_primitives::keccak256;
use borsh::BorshDeserialize;
use jsonrpsee::core::{async_trait, RpcResult, SubscriptionResult};
use jsonrpsee::proc_macros::rpc;
use jsonrpsee::types::ErrorObjectOwned;
use jsonrpsee::PendingSubscriptionSink;
use rocksdb::IteratorMode;

use torus_core::oracle::{OracleConfig, OracleManager};
use torus_core::order_book::OrderBook;
use torus_core::position::PositionManager;
use torus_core::precompiles::OrderBookSnapshot;
use torus_economics::governance::{GovernanceManager, ProposalStatus, ProposalType};
use torus_economics::rewards::FeeSplitter;
use torus_economics::staking::StakingManager;
use torus_economics::types::{
    ValidatorStatus, FEE_END_BURN_BPS, FEE_END_DEV_POOL_BPS, FEE_END_TREASURY_BPS,
    FEE_END_VALIDATOR_BPS, FEE_START_BURN_BPS, FEE_START_DEV_POOL_BPS, FEE_START_TREASURY_BPS,
    FEE_START_VALIDATOR_BPS, TRANSITION_EPOCHS,
};
use torus_economics::{lerp_bps, PermanentStakeInfo};
use torus_state::cf::{
    CF_BLOCK_BODIES, CF_GOVERNANCE_PROPOSALS, CF_NATIVE_MARKETS, CF_NATIVE_ORDER_BOOKS,
    CF_NATIVE_POSITIONS, CF_NATIVE_TRADES, CF_NATIVE_USER_TRADES, CF_STAKING_PERMANENT,
};
use torus_types::FixedPoint;

use crate::error::RpcError;
use crate::types::*;
use crate::RpcState;

// ============================================================================
// Internal deserialization helpers
// ============================================================================

/// Market data stored in CF_NATIVE_MARKETS by the governance module.
/// Format: base_asset(String) + quote_asset(String) + lot_size(i128) + tick_size(i128) + initial_margin(i128).
#[derive(BorshDeserialize)]
struct StoredMarket {
    base_asset: String,
    quote_asset: String,
    lot_size_raw: i128,
    tick_size_raw: i128,
    #[allow(dead_code)]
    initial_margin_raw: i128,
}

/// Trade data stored in CF_NATIVE_TRADES.
/// Key: market_id(8 BE) + block_number(8 BE) + trade_index(4 BE).
#[derive(BorshDeserialize)]
pub(crate) struct StoredTrade {
    pub trade_id: u128,
    pub price_raw: i128,
    pub quantity_raw: i128,
    pub side: u8,
    pub block_number: u64,
    pub timestamp: u64,
}

/// Per-user trade data stored in CF_NATIVE_USER_TRADES.
/// Key: trader(20) + (u64::MAX - block)(8 BE) + trade_index(4 BE).
#[derive(BorshDeserialize)]
pub(crate) struct StoredUserTrade {
    pub trade_id: u128,
    pub market_id: u64,
    pub price_raw: i128,
    pub quantity_raw: i128,
    pub side: u8,
    pub role: u8,
    pub block_number: u64,
    pub timestamp: u64,
}

// ============================================================================
// Trait definition
// ============================================================================

#[rpc(server, namespace = "torus")]
pub trait TorusApi {
    // --- 2.9.1: Trading reads ---
    #[method(name = "getOrderBook")]
    async fn get_order_book(&self, market_id: String) -> RpcResult<RpcOrderBook>;

    #[method(name = "getPosition")]
    async fn get_position(
        &self,
        trader: String,
        market_id: String,
    ) -> RpcResult<Option<RpcPosition>>;

    #[method(name = "getBalances")]
    async fn get_balances(&self, trader: String) -> RpcResult<RpcBalances>;

    // --- 2.9.2: Market info ---
    #[method(name = "getMarkets")]
    async fn get_markets(
        &self,
        offset: Option<u32>,
        limit: Option<u32>,
    ) -> RpcResult<Vec<RpcMarketInfo>>;

    #[method(name = "getTradeHistory")]
    async fn get_trade_history(
        &self,
        market_id: String,
        limit: Option<u32>,
    ) -> RpcResult<Vec<RpcTrade>>;

    // --- 2.9.3: Staking ---
    #[method(name = "getStakingInfo")]
    async fn get_staking_info(&self, address: String) -> RpcResult<RpcStakingInfo>;

    #[method(name = "getValidators")]
    async fn get_validators(&self) -> RpcResult<Vec<RpcValidatorInfo>>;

    #[method(name = "getEpoch")]
    async fn get_epoch(&self) -> RpcResult<RpcEpochInfo>;

    #[method(name = "getDelegations")]
    async fn get_delegations(&self, delegator: String) -> RpcResult<Vec<RpcDelegation>>;

    // --- 2.9.4: Submission ---
    #[method(name = "submitNativeAction")]
    async fn submit_native_action(&self, signed_action: String) -> RpcResult<String>;

    // --- 2.9.5: Governance ---
    #[method(name = "getProposal")]
    async fn get_proposal(&self, proposal_id: u64) -> RpcResult<Option<RpcProposal>>;

    #[method(name = "getProposals")]
    async fn get_proposals(&self, status: Option<String>) -> RpcResult<Vec<RpcProposal>>;

    #[method(name = "getGovernanceParams")]
    async fn get_governance_params(&self) -> RpcResult<RpcGovernanceParams>;

    // --- Treasury info (for explorer) ---
    #[method(name = "getTreasuryInfo")]
    async fn get_treasury_info(&self) -> RpcResult<RpcTreasuryInfo>;

    // --- Block body (native actions for explorer indexing) ---
    #[method(name = "getBlockBody")]
    async fn get_block_body(&self, block_number: u64) -> RpcResult<Option<RpcBlockBody>>;

    // --- Phase 7B: All trades in a single block (for explorer indexer) ---
    #[method(name = "getBlockTrades")]
    async fn get_block_trades(&self, block_number: u64) -> RpcResult<Vec<RpcTrade>>;

    // --- Phase 7B: Time-range trade query (forward, inclusive) ---
    #[method(name = "getTradeHistoryRange")]
    async fn get_trade_history_range(
        &self,
        market_id: String,
        from_block: String,
        to_block: String,
        limit: Option<u64>,
    ) -> RpcResult<Vec<RpcTrade>>;

    // --- Trading app endpoints ---
    #[method(name = "getOpenOrders")]
    async fn get_open_orders(
        &self,
        trader: String,
        market_id: Option<String>,
    ) -> RpcResult<Vec<RpcOpenOrder>>;

    #[method(name = "getOpenInterest")]
    async fn get_open_interest(&self, market_id: String) -> RpcResult<RpcOpenInterest>;

    #[method(name = "getMarkPrice")]
    async fn get_mark_price(&self, market_id: String) -> RpcResult<RpcMarkPrice>;

    #[method(name = "getUserTrades")]
    async fn get_user_trades(
        &self,
        trader: String,
        market_id: Option<String>,
        limit: Option<u32>,
    ) -> RpcResult<Vec<RpcUserTrade>>;

    // --- Phase 7B: Trade streaming subscription ---
    #[subscription(name = "subscribe" => "subscription", unsubscribe = "unsubscribe", item = serde_json::Value)]
    async fn subscribe(
        &self,
        sub_type: String,
        params: Option<serde_json::Value>,
    ) -> SubscriptionResult;
}

// ============================================================================
// Implementation
// ============================================================================

#[async_trait]
impl TorusApiServer for RpcState {
    // === 2.9.1: Trading reads ===

    async fn get_order_book(&self, market_id: String) -> RpcResult<RpcOrderBook> {
        let mid = parse_u64(&market_id).map_err(ErrorObjectOwned::from)?;
        let key = mid.to_be_bytes();

        let snapshot = match self.state.get_cf_raw(CF_NATIVE_ORDER_BOOKS, &key) {
            Ok(Some(data)) => OrderBookSnapshot::try_from_slice(&data)
                .map_err(|e| RpcError::Internal(format!("borsh decode: {e}")))
                .map_err(ErrorObjectOwned::from)?,
            Ok(None) => {
                return Ok(RpcOrderBook {
                    market_id,
                    bids: vec![],
                    asks: vec![],
                });
            }
            Err(e) => return Err(RpcError::State(e).into()),
        };

        let bids: Vec<RpcPriceLevel> = snapshot
            .bids
            .iter()
            .map(|lvl| RpcPriceLevel {
                price: hex_fp(lvl.price),
                quantity: hex_fp(lvl.quantity),
                order_count: 0,
            })
            .collect();

        let asks: Vec<RpcPriceLevel> = snapshot
            .asks
            .iter()
            .map(|lvl| RpcPriceLevel {
                price: hex_fp(lvl.price),
                quantity: hex_fp(lvl.quantity),
                order_count: 0,
            })
            .collect();

        Ok(RpcOrderBook {
            market_id,
            bids,
            asks,
        })
    }

    async fn get_position(
        &self,
        trader: String,
        market_id: String,
    ) -> RpcResult<Option<RpcPosition>> {
        let addr = parse_address(&trader).map_err(ErrorObjectOwned::from)?;
        let mid = parse_u64(&market_id).map_err(ErrorObjectOwned::from)?;

        let pm = PositionManager::new(self.state.clone());
        let pos = pm
            .get_position(&addr, mid)
            .map_err(|e| RpcError::Internal(e.to_string()))
            .map_err(ErrorObjectOwned::from)?;

        match pos {
            None => Ok(None),
            Some(p) => {
                let side = if p.is_long { "long" } else { "short" };
                let margin_mode = match p.margin_type {
                    torus_core::position::MarginType::Cross => "cross",
                    torus_core::position::MarginType::Isolated => "isolated",
                };

                // Compute unrealized PnL using oracle price; fall back to entry price.
                let current_block = self.latest_height.load(Ordering::Relaxed);
                let oracle =
                    OracleManager::new(self.state.clone(), OracleConfig::default());
                let mark_price = oracle
                    .get_price(mid, current_block)
                    .map(|op| op.price)
                    .unwrap_or(p.entry_price);
                let unrealized = p.unrealized_pnl(mark_price);

                // Simplified liquidation price estimate.
                let liquidation_price =
                    if p.size > FixedPoint::ZERO && p.isolated_margin > FixedPoint::ZERO {
                        if p.is_long {
                            p.entry_price - p.isolated_margin / p.size
                        } else {
                            p.entry_price + p.isolated_margin / p.size
                        }
                    } else {
                        FixedPoint::ZERO
                    };

                Ok(Some(RpcPosition {
                    market_id,
                    side: side.to_string(),
                    size: hex_fp(p.size),
                    entry_price: hex_fp(p.entry_price),
                    unrealized_pnl: hex_fp(unrealized),
                    realized_pnl: hex_fp(p.realized_pnl),
                    margin: hex_fp(p.isolated_margin),
                    margin_mode: margin_mode.to_string(),
                    liquidation_price: hex_fp(liquidation_price),
                }))
            }
        }
    }

    async fn get_balances(&self, trader: String) -> RpcResult<RpcBalances> {
        let addr = parse_address(&trader).map_err(ErrorObjectOwned::from)?;

        let pm = PositionManager::new(self.state.clone());
        let native_bal = pm
            .get_native_balance(&addr)
            .map_err(|e| RpcError::Internal(e.to_string()))
            .map_err(ErrorObjectOwned::from)?;

        // EVM balance
        let evm_balance = self
            .state
            .get_account(&addr)
            .map_err(RpcError::State)
            .map_err(ErrorObjectOwned::from)?
            .map(|a| a.balance)
            .unwrap_or_default();

        // Sum isolated margin from all open positions
        let positions = pm
            .positions_for_trader(&addr)
            .map_err(|e| RpcError::Internal(e.to_string()))
            .map_err(ErrorObjectOwned::from)?;
        let mut total_margin = native_bal.order_margin;
        for pos in &positions {
            total_margin = total_margin + pos.isolated_margin;
        }

        // Permanent stake from CF_STAKING_PERMANENT
        let permanent_stake =
            match self.state.get_cf_raw(CF_STAKING_PERMANENT, addr.as_slice()) {
                Ok(Some(data)) => PermanentStakeInfo::try_from_slice(&data)
                    .map(|info| hex_u256(info.amount))
                    .unwrap_or_else(|_| "0x0".to_string()),
                _ => "0x0".to_string(),
            };

        let native_total = native_bal.available + native_bal.order_margin;

        Ok(RpcBalances {
            native_balance: hex_fp(native_total),
            evm_balance: hex_u256(evm_balance),
            total_margin_used: hex_fp(total_margin),
            available_balance: hex_fp(native_bal.available),
            permanent_stake,
        })
    }

    // === 2.9.2: Market info ===

    async fn get_markets(
        &self,
        offset: Option<u32>,
        limit: Option<u32>,
    ) -> RpcResult<Vec<RpcMarketInfo>> {
        const MAX_MARKETS_PER_PAGE: usize = 500;
        let offset = offset.unwrap_or(0) as usize;
        let limit = limit.unwrap_or(100).min(MAX_MARKETS_PER_PAGE as u32) as usize;

        let db = self.state.inner();
        let cf = db
            .cf_handle(CF_NATIVE_MARKETS)
            .ok_or_else(|| RpcError::Internal("missing CF_NATIVE_MARKETS".into()))
            .map_err(ErrorObjectOwned::from)?;

        let iter = db.iterator_cf(cf, IteratorMode::Start);
        let mut markets = Vec::new();
        let mut scanned = 0usize;

        for item in iter {
            let (key, value) = item
                .map_err(|e| RpcError::Internal(format!("rocksdb: {e}")))
                .map_err(ErrorObjectOwned::from)?;
            if key.len() != 8 {
                continue;
            }

            scanned += 1;

            // Skip entries before offset without deserializing
            if scanned <= offset {
                continue;
            }

            let mid = u64::from_be_bytes(key[..8].try_into().unwrap());

            let market = StoredMarket::try_from_slice(&value)
                .map_err(|e| RpcError::Internal(format!("borsh decode market: {e}")))
                .map_err(ErrorObjectOwned::from)?;

            markets.push(RpcMarketInfo {
                market_id: hex_u64(mid),
                base_asset: market.base_asset,
                quote_asset: market.quote_asset,
                lot_size: hex_fp(FixedPoint::from_raw(market.lot_size_raw)),
                tick_size: hex_fp(FixedPoint::from_raw(market.tick_size_raw)),
                status: "active".to_string(),
            });

            if markets.len() >= limit {
                break;
            }
        }

        // Keys are u64 big-endian — RocksDB lexicographic order == numeric order. No sort needed.
        Ok(markets)
    }

    async fn get_trade_history(
        &self,
        market_id: String,
        limit: Option<u32>,
    ) -> RpcResult<Vec<RpcTrade>> {
        let mid = parse_u64(&market_id).map_err(ErrorObjectOwned::from)?;
        let limit = limit.unwrap_or(100).min(1000) as usize;
        let prefix = mid.to_be_bytes();

        let db = self.state.inner();
        let cf = match db.cf_handle(CF_NATIVE_TRADES) {
            Some(cf) => cf,
            None => return Ok(vec![]),
        };

        let pruned_up_to = self.pruned_up_to.load(Relaxed);

        // Reverse-iterate from the upper bound of this market's key range.
        // Key format: market_id(8) + block_number(8) + trade_index(4) = 20 bytes.
        let mut upper = [0xFFu8; 20];
        upper[..8].copy_from_slice(&prefix);
        let iter = db.iterator_cf(
            cf,
            rocksdb::IteratorMode::From(&upper, rocksdb::Direction::Reverse),
        );
        let mut trades = Vec::with_capacity(limit);

        for item in iter {
            let (key, value) = item
                .map_err(|e| RpcError::Internal(format!("rocksdb: {e}")))
                .map_err(ErrorObjectOwned::from)?;
            if !key.starts_with(&prefix) {
                break;
            }

            // Key: market_id(8) + block_number(8) + trade_index(4)
            // Since we iterate newest-first, once we hit a pruned block all
            // remaining entries are older — stop immediately.
            if key.len() >= 16 && pruned_up_to > 0 {
                let block = u64::from_be_bytes(key[8..16].try_into().unwrap());
                if block < pruned_up_to {
                    break;
                }
            }

            let trade = StoredTrade::try_from_slice(&value)
                .map_err(|e| RpcError::Internal(format!("borsh decode trade: {e}")))
                .map_err(ErrorObjectOwned::from)?;

            trades.push(RpcTrade {
                trade_id: hex_u128(trade.trade_id),
                market_id: market_id.clone(),
                price: hex_fp(FixedPoint::from_raw(trade.price_raw)),
                quantity: hex_fp(FixedPoint::from_raw(trade.quantity_raw)),
                side: if trade.side == 0 {
                    "buy".to_string()
                } else {
                    "sell".to_string()
                },
                block_number: hex_u64(trade.block_number),
                timestamp: hex_u64(trade.timestamp),
            });

            if trades.len() >= limit {
                break;
            }
        }

        // Already in descending order (most recent first) from reverse iteration.
        Ok(trades)
    }

    // === 2.9.3: Staking ===

    async fn get_staking_info(&self, address: String) -> RpcResult<RpcStakingInfo> {
        let addr = parse_address(&address).map_err(ErrorObjectOwned::from)?;
        let staking = StakingManager::new(self.state.clone());
        let info = torus_economics::queries::get_staking_info(&staking, addr)
            .map_err(|e| RpcError::Internal(e.to_string()))
            .map_err(ErrorObjectOwned::from)?;

        let delegated = info
            .delegated
            .iter()
            .map(|(v, amt)| RpcDelegation {
                validator: hex_address(*v),
                amount: hex_u256(*amt),
            })
            .collect();

        let unbonding = info
            .unbonding
            .iter()
            .map(|u| RpcUnbonding {
                amount: hex_u256(u.amount),
                release_block: hex_u64(u.release_block),
            })
            .collect();

        Ok(RpcStakingInfo {
            delegated,
            permanent_stake: hex_u256(info.permanent_stake),
            pending_rewards: hex_u256(info.pending_rewards),
            unbonding,
        })
    }

    async fn get_validators(&self) -> RpcResult<Vec<RpcValidatorInfo>> {
        let staking = StakingManager::new(self.state.clone());
        let validators = torus_economics::queries::get_validators(&staking)
            .map_err(|e| RpcError::Internal(e.to_string()))
            .map_err(ErrorObjectOwned::from)?;

        // Also fetch full ValidatorState for status info.
        let all_states = staking
            .all_validators()
            .map_err(|e| RpcError::Internal(e.to_string()))
            .map_err(ErrorObjectOwned::from)?;

        let result = validators
            .into_iter()
            .map(|v| {
                let status = all_states
                    .iter()
                    .find(|s| s.address == v.address)
                    .map(|s| match s.status {
                        ValidatorStatus::Candidate => "candidate",
                        ValidatorStatus::Active => "active",
                        ValidatorStatus::Jailed => "jailed",
                        ValidatorStatus::Tombstoned => "tombstoned",
                    })
                    .unwrap_or("unknown");

                RpcValidatorInfo {
                    address: hex_address(v.address),
                    pubkey: format!("0x{}", hex::encode(v.pubkey.0)),
                    power: hex_u64(v.power),
                    commission_bps: v.commission_bps,
                    status: status.to_string(),
                }
            })
            .collect();

        Ok(result)
    }

    async fn get_epoch(&self) -> RpcResult<RpcEpochInfo> {
        let current_height = self.latest_height.load(Ordering::Relaxed);
        let epoch_length = self.epoch_length;
        let info = torus_economics::queries::get_epoch_info(current_height, epoch_length);

        Ok(RpcEpochInfo {
            current_epoch: hex_u64(info.current_epoch),
            epoch_start_block: hex_u64(info.epoch_start_block),
            epoch_end_block: hex_u64(info.epoch_end_block),
            blocks_remaining: hex_u64(info.blocks_remaining),
            epoch_length: hex_u64(info.epoch_length),
        })
    }

    async fn get_delegations(&self, delegator: String) -> RpcResult<Vec<RpcDelegation>> {
        let addr = parse_address(&delegator).map_err(ErrorObjectOwned::from)?;
        let staking = StakingManager::new(self.state.clone());
        let delegations = torus_economics::queries::get_delegations(&staking, addr)
            .map_err(|e| RpcError::Internal(e.to_string()))
            .map_err(ErrorObjectOwned::from)?;

        Ok(delegations
            .into_iter()
            .map(|(v, amt)| RpcDelegation {
                validator: hex_address(v),
                amount: hex_u256(amt),
            })
            .collect())
    }

    // === 2.9.4: Submission ===

    async fn submit_native_action(&self, signed_action: String) -> RpcResult<String> {
        // Decode hex → bytes.
        let bytes = parse_bytes(&signed_action).map_err(ErrorObjectOwned::from)?;

        // Deserialize JSON bytes → SignedNativeAction (serde, not borsh).
        let action: torus_types::SignedNativeAction = serde_json::from_slice(&bytes)
            .map_err(|e| RpcError::InvalidParams(format!("invalid action encoding: {e}")))
            .map_err(ErrorObjectOwned::from)?;

        // Resolve sender — supports both EIP-712 ECDSA and ed25519 session keys.
        let current_time_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock before epoch")
            .as_millis() as u64;

        let state_ref = &self.state;
        let sender = action
            .validate_with_sessions(current_time_ms, self.chain_id, |pubkey| {
                state_ref.get_session(pubkey).ok().flatten()
            })
            .map_err(|e| RpcError::InvalidParams(format!("signature verification failed: {e}")))
            .map_err(ErrorObjectOwned::from)?;

        // Compute action hash for the receipt.
        let action_bytes = serde_json::to_vec(&action)
            .map_err(|e| RpcError::Internal(format!("serialize action: {e}")))
            .map_err(ErrorObjectOwned::from)?;
        let hash = keccak256(&action_bytes);

        // Submit to mempool with pre-resolved sender (bypasses validate() which
        // only handles EIP-712; we already validated above with session support).
        self.mempool
            .add_native_action_presigned(sender, action)
            .map_err(|e| ErrorObjectOwned::from(RpcError::Internal(format!("mempool: {e}"))))?;

        Ok(hex_b256(hash))
    }

    // === 2.9.5: Governance ===

    async fn get_proposal(&self, proposal_id: u64) -> RpcResult<Option<RpcProposal>> {
        let gov = GovernanceManager::new(self.state.clone());
        let proposal = gov
            .get_proposal(proposal_id)
            .map_err(|e| RpcError::Internal(e.to_string()))
            .map_err(ErrorObjectOwned::from)?;

        Ok(proposal.map(map_proposal))
    }

    async fn get_proposals(&self, status: Option<String>) -> RpcResult<Vec<RpcProposal>> {
        let gov = GovernanceManager::new(self.state.clone());

        let proposals = match status {
            Some(s) => {
                let ps = parse_proposal_status(&s).map_err(ErrorObjectOwned::from)?;
                gov.get_proposals_by_status(ps)
                    .map_err(|e| RpcError::Internal(e.to_string()))
                    .map_err(ErrorObjectOwned::from)?
            }
            None => {
                // Return all proposals by iterating the CF directly.
                let db = self.state.inner();
                let cf = db
                    .cf_handle(CF_GOVERNANCE_PROPOSALS)
                    .ok_or_else(|| {
                        RpcError::Internal("missing CF_GOVERNANCE_PROPOSALS".into())
                    })
                    .map_err(ErrorObjectOwned::from)?;
                let iter = db.iterator_cf(cf, IteratorMode::Start);
                let mut all = Vec::new();
                for item in iter {
                    let (key, value) = item
                        .map_err(|e| RpcError::Internal(format!("rocksdb: {e}")))
                        .map_err(ErrorObjectOwned::from)?;
                    if key.len() != 8 {
                        continue;
                    }
                    let proposal =
                        torus_economics::governance::Proposal::try_from_slice(&value)
                            .map_err(|e| {
                                RpcError::Internal(format!("borsh decode proposal: {e}"))
                            })
                            .map_err(ErrorObjectOwned::from)?;
                    all.push(proposal);
                }
                all
            }
        };

        Ok(proposals.into_iter().map(map_proposal).collect())
    }

    async fn get_governance_params(&self) -> RpcResult<RpcGovernanceParams> {
        let gov = GovernanceManager::new(self.state.clone());
        let params = gov
            .get_governance_params()
            .map_err(|e| RpcError::Internal(e.to_string()))
            .map_err(ErrorObjectOwned::from)?;

        Ok(RpcGovernanceParams {
            voting_period_blocks: hex_u64(params.voting_period_blocks),
            quorum_bps: hex_u64(params.quorum_bps),
            min_proposal_stake: hex_u256(params.min_proposal_stake),
            permanent_weight_multiplier: format!(
                "{}/{}",
                params.permanent_weight_multiplier_num,
                params.permanent_weight_multiplier_den
            ),
            treasury_address: hex_address(params.treasury_address),
        })
    }

    async fn get_treasury_info(&self) -> RpcResult<RpcTreasuryInfo> {
        let gov = GovernanceManager::new(self.state.clone());
        let params = gov
            .get_governance_params()
            .map_err(|e| RpcError::Internal(e.to_string()))
            .map_err(ErrorObjectOwned::from)?;

        let treasury_address = params.treasury_address;

        // Treasury balance from account state.
        let treasury_balance = self
            .state
            .get_account(&treasury_address)
            .map_err(RpcError::State)
            .map_err(ErrorObjectOwned::from)?
            .map(|a| a.balance)
            .unwrap_or_default();

        // Cumulative burn/treasury from SupplyTracker.
        let staking = StakingManager::new(self.state.clone());
        let tracker = FeeSplitter::get_supply_tracker(&staking)
            .map_err(|e| RpcError::Internal(e.to_string()))
            .map_err(ErrorObjectOwned::from)?;

        // Current fee split BPS using lerp.
        let current_height = self.latest_height.load(Ordering::Relaxed);
        let epoch_length = self.epoch_length;
        let epoch_info =
            torus_economics::queries::get_epoch_info(current_height, epoch_length);
        let epoch = epoch_info.current_epoch;

        let burn_bps =
            lerp_bps(FEE_START_BURN_BPS, FEE_END_BURN_BPS, epoch, TRANSITION_EPOCHS);
        let validator_bps = lerp_bps(
            FEE_START_VALIDATOR_BPS,
            FEE_END_VALIDATOR_BPS,
            epoch,
            TRANSITION_EPOCHS,
        );
        let treasury_bps = lerp_bps(
            FEE_START_TREASURY_BPS,
            FEE_END_TREASURY_BPS,
            epoch,
            TRANSITION_EPOCHS,
        );
        let dev_pool_bps = lerp_bps(
            FEE_START_DEV_POOL_BPS,
            FEE_END_DEV_POOL_BPS,
            epoch,
            TRANSITION_EPOCHS,
        );

        Ok(RpcTreasuryInfo {
            treasury_address: hex_address(treasury_address),
            treasury_balance: hex_u256(treasury_balance),
            cumulative_burned: hex_u256(tracker.cumulative_burned),
            cumulative_treasury: hex_u256(tracker.cumulative_treasury),
            current_fee_split: RpcFeeSplit {
                burn_bps,
                validator_bps,
                treasury_bps,
                dev_pool_bps,
            },
        })
    }

    async fn get_block_body(&self, block_number: u64) -> RpcResult<Option<RpcBlockBody>> {
        let key = block_number.to_be_bytes();
        let data = match self.state.get_cf_raw(CF_BLOCK_BODIES, &key) {
            Ok(Some(d)) => d,
            Ok(None) => return Ok(None),
            Err(e) => return Err(ErrorObjectOwned::from(RpcError::State(e))),
        };
        let body: torus_types::TorusBlockBody = serde_json::from_slice(&data)
            .map_err(|e| RpcError::Internal(format!("body decode: {e}")))
            .map_err(ErrorObjectOwned::from)?;
        let native_actions: Vec<serde_json::Value> = body
            .native_actions
            .iter()
            .map(|a| serde_json::to_value(a).unwrap_or_default())
            .collect();
        Ok(Some(RpcBlockBody {
            block_number: hex_u64(block_number),
            native_actions,
            native_action_count: body.native_actions.len() as u32,
        }))
    }

    // === Phase 7B: All trades in a single block ===

    async fn get_block_trades(&self, block_number: u64) -> RpcResult<Vec<RpcTrade>> {
        let trades_json = crate::scan_trades_for_block(&self.state, block_number);
        let mut trades = Vec::with_capacity(trades_json.len());
        for t in trades_json {
            trades.push(RpcTrade {
                trade_id: t
                    .get("tradeId")
                    .and_then(|v| v.as_str())
                    .unwrap_or("0x0")
                    .to_string(),
                market_id: t
                    .get("marketId")
                    .and_then(|v| v.as_str())
                    .unwrap_or("0x0")
                    .to_string(),
                price: t
                    .get("price")
                    .and_then(|v| v.as_str())
                    .unwrap_or("0x0")
                    .to_string(),
                quantity: t
                    .get("quantity")
                    .and_then(|v| v.as_str())
                    .unwrap_or("0x0")
                    .to_string(),
                side: t
                    .get("side")
                    .and_then(|v| v.as_str())
                    .unwrap_or("buy")
                    .to_string(),
                block_number: t
                    .get("blockNumber")
                    .and_then(|v| v.as_str())
                    .unwrap_or("0x0")
                    .to_string(),
                timestamp: t
                    .get("timestamp")
                    .and_then(|v| v.as_str())
                    .unwrap_or("0x0")
                    .to_string(),
            });
        }
        Ok(trades)
    }

    // === Phase 7B: Time-range trade query ===

    async fn get_trade_history_range(
        &self,
        market_id: String,
        from_block: String,
        to_block: String,
        limit: Option<u64>,
    ) -> RpcResult<Vec<RpcTrade>> {
        let mid = parse_u64(&market_id).map_err(ErrorObjectOwned::from)?;
        let from = parse_u64(&from_block).map_err(ErrorObjectOwned::from)?;
        let to = parse_u64(&to_block).map_err(ErrorObjectOwned::from)?;
        let limit = limit.unwrap_or(1000).min(5000) as usize;

        let db = self.state.inner();
        let cf = match db.cf_handle(CF_NATIVE_TRADES) {
            Some(cf) => cf,
            None => return Ok(vec![]),
        };

        // Build start key: market_id(8) + from_block(8) + 0x00000000
        let mut start_key = [0u8; 20];
        start_key[..8].copy_from_slice(&mid.to_be_bytes());
        start_key[8..16].copy_from_slice(&from.to_be_bytes());

        // Build end key: market_id(8) + (to_block+1)(8)
        let end_block = to.saturating_add(1);
        let mut end_prefix = [0u8; 16];
        end_prefix[..8].copy_from_slice(&mid.to_be_bytes());
        end_prefix[8..16].copy_from_slice(&end_block.to_be_bytes());

        let iter = db.iterator_cf(
            cf,
            rocksdb::IteratorMode::From(&start_key, rocksdb::Direction::Forward),
        );
        let mut trades = Vec::with_capacity(limit.min(256));

        for item in iter {
            let (key, value) = item
                .map_err(|e| RpcError::Internal(format!("rocksdb: {e}")))
                .map_err(ErrorObjectOwned::from)?;

            // Stop if we've left this market or past the end block
            if key.len() < 16 || key[..8] != mid.to_be_bytes() {
                break;
            }
            if key[..16] >= end_prefix[..] {
                break;
            }

            let trade = StoredTrade::try_from_slice(&value)
                .map_err(|e| RpcError::Internal(format!("borsh decode trade: {e}")))
                .map_err(ErrorObjectOwned::from)?;

            trades.push(RpcTrade {
                trade_id: hex_u128(trade.trade_id),
                market_id: market_id.clone(),
                price: hex_fp(FixedPoint::from_raw(trade.price_raw)),
                quantity: hex_fp(FixedPoint::from_raw(trade.quantity_raw)),
                side: if trade.side == 0 {
                    "buy".to_string()
                } else {
                    "sell".to_string()
                },
                block_number: hex_u64(trade.block_number),
                timestamp: hex_u64(trade.timestamp),
            });

            if trades.len() >= limit {
                break;
            }
        }

        Ok(trades)
    }

    // === Trading app endpoints ===

    async fn get_open_orders(
        &self,
        trader: String,
        market_id: Option<String>,
    ) -> RpcResult<Vec<RpcOpenOrder>> {
        let trader_addr = parse_address(&trader).map_err(ErrorObjectOwned::from)?;

        let db = self.state.inner();
        let cf = match db.cf_handle(CF_NATIVE_ORDER_BOOKS) {
            Some(cf) => cf,
            None => return Ok(vec![]),
        };

        let mut orders = Vec::new();

        if let Some(ref mid_str) = market_id {
            // Single market: read one OrderBook
            let mid = parse_u64(mid_str).map_err(ErrorObjectOwned::from)?;
            let key = mid.to_be_bytes();
            if let Some(data) = db.get_cf(cf, &key)
                .map_err(|e| RpcError::Internal(format!("rocksdb: {e}")))
                .map_err(ErrorObjectOwned::from)?
            {
                let book = OrderBook::try_from_slice(&data)
                    .map_err(|e| RpcError::Internal(format!("borsh decode order book: {e}")))
                    .map_err(ErrorObjectOwned::from)?;
                for order in book.orders_for_trader(&trader_addr) {
                    if orders.len() >= 500 {
                        break;
                    }
                    orders.push(order_to_rpc(order, mid));
                }
            }
        } else {
            // All markets: iterate CF_NATIVE_ORDER_BOOKS
            let iter = db.iterator_cf(cf, IteratorMode::Start);
            for item in iter {
                if orders.len() >= 500 {
                    break;
                }
                let (key, value) = item
                    .map_err(|e| RpcError::Internal(format!("rocksdb: {e}")))
                    .map_err(ErrorObjectOwned::from)?;
                if key.len() != 8 {
                    continue;
                }
                let mid = u64::from_be_bytes(key[..8].try_into().unwrap());
                let book = match OrderBook::try_from_slice(&value) {
                    Ok(b) => b,
                    Err(_) => continue,
                };
                for order in book.orders_for_trader(&trader_addr) {
                    if orders.len() >= 500 {
                        break;
                    }
                    orders.push(order_to_rpc(order, mid));
                }
            }
        }

        Ok(orders)
    }

    async fn get_open_interest(&self, market_id: String) -> RpcResult<RpcOpenInterest> {
        let mid = parse_u64(&market_id).map_err(ErrorObjectOwned::from)?;

        let db = self.state.inner();
        let cf = match db.cf_handle(CF_NATIVE_POSITIONS) {
            Some(cf) => cf,
            None => {
                return Ok(RpcOpenInterest {
                    market_id,
                    long_oi: hex_fp(FixedPoint::ZERO),
                    short_oi: hex_fp(FixedPoint::ZERO),
                });
            }
        };

        // Position key: trader(20) + market_id(8). Full scan, filter by market.
        let mut long_oi = FixedPoint::ZERO;
        let mut short_oi = FixedPoint::ZERO;
        let iter = db.iterator_cf(cf, IteratorMode::Start);
        for item in iter {
            let (key, value) = item
                .map_err(|e| RpcError::Internal(format!("rocksdb: {e}")))
                .map_err(ErrorObjectOwned::from)?;
            if key.len() != 28 {
                continue;
            }
            let pos_market = u64::from_be_bytes(key[20..28].try_into().unwrap());
            if pos_market != mid {
                continue;
            }
            let pos = torus_core::position::Position::try_from_slice(&value)
                .map_err(|e| RpcError::Internal(format!("borsh decode position: {e}")))
                .map_err(ErrorObjectOwned::from)?;
            if pos.is_long {
                long_oi = long_oi + pos.size;
            } else {
                short_oi = short_oi + pos.size;
            }
        }

        Ok(RpcOpenInterest {
            market_id,
            long_oi: hex_fp(long_oi),
            short_oi: hex_fp(short_oi),
        })
    }

    async fn get_mark_price(&self, market_id: String) -> RpcResult<RpcMarkPrice> {
        let mid = parse_u64(&market_id).map_err(ErrorObjectOwned::from)?;
        let current_block = self.latest_height.load(Ordering::Relaxed);

        let oracle = OracleManager::new(self.state.clone(), OracleConfig::default());
        let (mark_price, index_price, timestamp) = match oracle.get_price(mid, current_block) {
            Ok(op) => (op.price, op.price, op.block_number),
            Err(_) => (FixedPoint::ZERO, FixedPoint::ZERO, 0),
        };

        // Last trade price from the order book
        let last_trade_price = match self.state.get_cf_raw(CF_NATIVE_ORDER_BOOKS, &mid.to_be_bytes()) {
            Ok(Some(data)) => {
                OrderBook::try_from_slice(&data)
                    .ok()
                    .and_then(|book| book.last_trade_price())
                    .unwrap_or(FixedPoint::ZERO)
            }
            _ => FixedPoint::ZERO,
        };

        Ok(RpcMarkPrice {
            market_id,
            mark_price: hex_fp(mark_price),
            index_price: hex_fp(index_price),
            last_trade_price: hex_fp(last_trade_price),
            timestamp,
        })
    }

    async fn get_user_trades(
        &self,
        trader: String,
        market_id: Option<String>,
        limit: Option<u32>,
    ) -> RpcResult<Vec<RpcUserTrade>> {
        let trader_addr = parse_address(&trader).map_err(ErrorObjectOwned::from)?;
        let limit = limit.unwrap_or(100).min(1000) as usize;
        let market_filter = match market_id {
            Some(ref s) => Some(parse_u64(s).map_err(ErrorObjectOwned::from)?),
            None => None,
        };

        let db = self.state.inner();
        let cf = match db.cf_handle(CF_NATIVE_USER_TRADES) {
            Some(cf) => cf,
            None => return Ok(vec![]),
        };

        // Forward prefix scan: keys encode descending block order via
        // (u64::MAX - block), so forward iteration returns newest first.
        let prefix = trader_addr.as_slice();
        let iter = db.iterator_cf(
            cf,
            rocksdb::IteratorMode::From(prefix, rocksdb::Direction::Forward),
        );

        let mut trades = Vec::with_capacity(limit.min(256));
        for item in iter {
            let (key, value) = item
                .map_err(|e| RpcError::Internal(format!("rocksdb: {e}")))
                .map_err(ErrorObjectOwned::from)?;
            if !key.starts_with(prefix) {
                break;
            }

            let trade = StoredUserTrade::try_from_slice(&value)
                .map_err(|e| RpcError::Internal(format!("borsh decode user trade: {e}")))
                .map_err(ErrorObjectOwned::from)?;

            if let Some(mf) = market_filter {
                if trade.market_id != mf {
                    continue;
                }
            }

            trades.push(RpcUserTrade {
                trade_id: hex_u128(trade.trade_id),
                market_id: hex_u64(trade.market_id),
                side: if trade.side == 0 { "buy".to_string() } else { "sell".to_string() },
                price: hex_fp(FixedPoint::from_raw(trade.price_raw)),
                quantity: hex_fp(FixedPoint::from_raw(trade.quantity_raw)),
                role: if trade.role == 0 { "maker".to_string() } else { "taker".to_string() },
                block_number: hex_u64(trade.block_number),
                timestamp: hex_u64(trade.timestamp),
            });

            if trades.len() >= limit {
                break;
            }
        }

        Ok(trades)
    }

    // === Phase 7B: Trade streaming subscription ===

    async fn subscribe(
        &self,
        pending: PendingSubscriptionSink,
        sub_type: String,
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

        match sub_type.as_str() {
            "newTrades" => {
                // Optional marketId filter from params
                let market_filter: Option<String> = params
                    .as_ref()
                    .and_then(|p| p.get("marketId"))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_lowercase());

                let mut rx = self.notifier.new_trades.subscribe();
                tokio::spawn(async move {
                    while let Ok(trades) = rx.recv().await {
                        for trade in &trades {
                            // Apply marketId filter if specified
                            let should_send = match &market_filter {
                                Some(filter) => trade
                                    .get("marketId")
                                    .and_then(|v| v.as_str())
                                    .map(|m| m.to_lowercase() == *filter)
                                    .unwrap_or(true),
                                None => true,
                            };
                            if should_send {
                                match jsonrpsee::SubscriptionMessage::new(
                                    "torus_subscription",
                                    sink.subscription_id(),
                                    trade,
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
            _ => {
                subs.fetch_sub(1, Relaxed);
                tracing::warn!("unknown torus subscription kind: {sub_type}");
            }
        }

        Ok(())
    }
}

// ============================================================================
// Helpers
// ============================================================================

fn map_proposal(p: torus_economics::governance::Proposal) -> RpcProposal {
    let proposal_type = match p.proposal_type {
        ProposalType::ParameterChange => "ParameterChange",
        ProposalType::TreasurySpend => "TreasurySpend",
        ProposalType::MarketListing => "MarketListing",
        ProposalType::TextProposal => "TextProposal",
        ProposalType::ValidatorRegistration => "ValidatorRegistration",
        ProposalType::PermanentUnlock => "PermanentUnlock",
    };

    let status = match p.status {
        ProposalStatus::Pending => "Pending",
        ProposalStatus::Active => "Active",
        ProposalStatus::Passed => "Passed",
        ProposalStatus::Rejected => "Rejected",
        ProposalStatus::Executed => "Executed",
        ProposalStatus::Expired => "Expired",
    };

    RpcProposal {
        id: p.id,
        proposer: hex_address(p.proposer),
        title: p.title,
        description: p.description,
        proposal_type: proposal_type.to_string(),
        status: status.to_string(),
        votes_for: hex_u256(p.votes_for),
        votes_against: hex_u256(p.votes_against),
        start_block: hex_u64(p.start_block),
        end_block: hex_u64(p.end_block),
    }
}

fn order_to_rpc(order: &torus_core::order_book::Order, market_id: u64) -> RpcOpenOrder {
    use torus_types::{OrderType, Side, TimeInForce};

    let side = match order.side {
        Side::Buy => "buy",
        Side::Sell => "sell",
    };
    let order_type = match order.order_type {
        OrderType::Limit => "limit",
        OrderType::Market => "market",
        OrderType::StopMarket { .. } => "stop_market",
        OrderType::StopLimit { .. } => "stop_limit",
    };
    let time_in_force = match order.time_in_force {
        TimeInForce::GTC => "gtc",
        TimeInForce::IOC => "ioc",
        TimeInForce::FOK => "fok",
        TimeInForce::PostOnly => "post_only",
    };

    RpcOpenOrder {
        order_id: hex_u128(order.id),
        market_id: hex_u64(market_id),
        side: side.to_string(),
        price: hex_fp(order.price),
        remaining_qty: hex_fp(order.remaining_qty),
        original_qty: hex_fp(order.original_qty),
        order_type: order_type.to_string(),
        time_in_force: time_in_force.to_string(),
        reduce_only: order.reduce_only,
        client_order_id: order.client_order_id.map(|id| hex_u64(id)),
        timestamp: order.timestamp,
    }
}

fn parse_proposal_status(s: &str) -> Result<ProposalStatus, RpcError> {
    match s.to_lowercase().as_str() {
        "pending" => Ok(ProposalStatus::Pending),
        "active" => Ok(ProposalStatus::Active),
        "passed" => Ok(ProposalStatus::Passed),
        "rejected" => Ok(ProposalStatus::Rejected),
        "executed" => Ok(ProposalStatus::Executed),
        "expired" => Ok(ProposalStatus::Expired),
        _ => Err(RpcError::InvalidParams(format!(
            "unknown proposal status: {s}"
        ))),
    }
}
