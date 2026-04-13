//! `torus_*` JSON-RPC namespace — native exchange, staking, governance endpoints.

use std::sync::atomic::Ordering;

use borsh::BorshDeserialize;
use jsonrpsee::core::{async_trait, RpcResult};
use jsonrpsee::proc_macros::rpc;
use jsonrpsee::types::ErrorObjectOwned;
use rocksdb::IteratorMode;

use torus_core::oracle::{OracleConfig, OracleManager};
use torus_core::position::PositionManager;
use torus_core::precompiles::OrderBookSnapshot;
use torus_economics::PermanentStakeInfo;
use torus_state::cf::{
    CF_NATIVE_MARKETS, CF_NATIVE_ORDER_BOOKS, CF_NATIVE_TRADES, CF_STAKING_PERMANENT,
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
    async fn get_markets(&self) -> RpcResult<Vec<RpcMarketInfo>>;

    #[method(name = "getTradeHistory")]
    async fn get_trade_history(
        &self,
        market_id: String,
        limit: Option<u32>,
    ) -> RpcResult<Vec<RpcTrade>>;

    // --- 2.9.3: Staking (stub — batch 7b) ---
    #[method(name = "getStakingInfo")]
    async fn get_staking_info(&self, address: String) -> RpcResult<serde_json::Value>;

    #[method(name = "getValidators")]
    async fn get_validators(&self) -> RpcResult<serde_json::Value>;

    #[method(name = "getEpoch")]
    async fn get_epoch(&self) -> RpcResult<serde_json::Value>;

    #[method(name = "getDelegations")]
    async fn get_delegations(&self, delegator: String) -> RpcResult<serde_json::Value>;

    // --- 2.9.4: Submission (stub — batch 7b) ---
    #[method(name = "submitNativeAction")]
    async fn submit_native_action(&self, signed_action: String) -> RpcResult<String>;

    // --- 2.9.5: Governance (stub — batch 7b) ---
    #[method(name = "getProposal")]
    async fn get_proposal(&self, proposal_id: u64) -> RpcResult<serde_json::Value>;

    #[method(name = "getProposals")]
    async fn get_proposals(&self, status: Option<String>) -> RpcResult<serde_json::Value>;

    #[method(name = "getGovernanceParams")]
    async fn get_governance_params(&self) -> RpcResult<serde_json::Value>;
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

    async fn get_markets(&self) -> RpcResult<Vec<RpcMarketInfo>> {
        let db = self.state.inner();
        let cf = db
            .cf_handle(CF_NATIVE_MARKETS)
            .ok_or_else(|| RpcError::Internal("missing CF_NATIVE_MARKETS".into()))
            .map_err(ErrorObjectOwned::from)?;

        let iter = db.iterator_cf(cf, IteratorMode::Start);
        let mut markets = Vec::new();

        for item in iter {
            let (key, value) = item
                .map_err(|e| RpcError::Internal(format!("rocksdb: {e}")))
                .map_err(ErrorObjectOwned::from)?;
            if key.len() != 8 {
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
        }

        markets.sort_by(|a, b| a.market_id.cmp(&b.market_id));
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

        let iter = db.prefix_iterator_cf(cf, &prefix);
        let mut trades = Vec::new();

        for item in iter {
            let (key, value) = item
                .map_err(|e| RpcError::Internal(format!("rocksdb: {e}")))
                .map_err(ErrorObjectOwned::from)?;
            if !key.starts_with(&prefix) {
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
        }

        // Most recent first (keys are chronological ascending)
        trades.reverse();
        trades.truncate(limit);
        Ok(trades)
    }

    // === 2.9.3: Staking stubs ===

    async fn get_staking_info(&self, _address: String) -> RpcResult<serde_json::Value> {
        Err(ErrorObjectOwned::owned(
            -32601,
            "torus_getStakingInfo not yet implemented",
            None::<()>,
        ))
    }

    async fn get_validators(&self) -> RpcResult<serde_json::Value> {
        Err(ErrorObjectOwned::owned(
            -32601,
            "torus_getValidators not yet implemented",
            None::<()>,
        ))
    }

    async fn get_epoch(&self) -> RpcResult<serde_json::Value> {
        Err(ErrorObjectOwned::owned(
            -32601,
            "torus_getEpoch not yet implemented",
            None::<()>,
        ))
    }

    async fn get_delegations(&self, _delegator: String) -> RpcResult<serde_json::Value> {
        Err(ErrorObjectOwned::owned(
            -32601,
            "torus_getDelegations not yet implemented",
            None::<()>,
        ))
    }

    // === 2.9.4: Submission stub ===

    async fn submit_native_action(&self, _signed_action: String) -> RpcResult<String> {
        Err(ErrorObjectOwned::owned(
            -32601,
            "torus_submitNativeAction not yet implemented",
            None::<()>,
        ))
    }

    // === 2.9.5: Governance stubs ===

    async fn get_proposal(&self, _proposal_id: u64) -> RpcResult<serde_json::Value> {
        Err(ErrorObjectOwned::owned(
            -32601,
            "torus_getProposal not yet implemented",
            None::<()>,
        ))
    }

    async fn get_proposals(&self, _status: Option<String>) -> RpcResult<serde_json::Value> {
        Err(ErrorObjectOwned::owned(
            -32601,
            "torus_getProposals not yet implemented",
            None::<()>,
        ))
    }

    async fn get_governance_params(&self) -> RpcResult<serde_json::Value> {
        Err(ErrorObjectOwned::owned(
            -32601,
            "torus_getGovernanceParams not yet implemented",
            None::<()>,
        ))
    }
}
