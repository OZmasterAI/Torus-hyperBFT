//! Error types for the torus-core execution engine.

use thiserror::Error;
use torus_types::{FixedPoint, MarketId, OrderId, U256};

#[derive(Debug, Error)]
pub enum CoreError {
    #[error("order not found: {0}")]
    OrderNotFound(OrderId),

    #[error("invalid quantity: must be positive")]
    InvalidQuantity,

    #[error("invalid price: must be positive for limit orders")]
    InvalidPrice,

    #[error("price {price} not aligned to tick size {tick_size}")]
    InvalidTickSize {
        price: FixedPoint,
        tick_size: FixedPoint,
    },

    #[error("invalid trigger price for stop order")]
    InvalidTriggerPrice,

    #[error("max orders per trader exceeded: {count} >= {max}")]
    MaxOrdersExceeded { count: usize, max: usize },

    #[error("invalid oracle price for market {market_id}: must be positive")]
    InvalidOraclePrice { market_id: MarketId },

    #[error("dust order: quantity {qty} below lot size {lot_size}")]
    DustOrder {
        qty: FixedPoint,
        lot_size: FixedPoint,
    },

    // Margin errors
    #[error("insufficient margin: required {required}, available {available}")]
    InsufficientMargin {
        required: FixedPoint,
        available: FixedPoint,
    },

    #[error("max leverage exceeded: {leverage}x > {max_leverage}x for notional {notional}")]
    MaxLeverageExceeded {
        leverage: u32,
        max_leverage: u32,
        notional: FixedPoint,
    },

    // Liquidation errors
    #[error("position not liquidatable: equity {equity} >= maintenance {maintenance}")]
    NotLiquidatable {
        equity: FixedPoint,
        maintenance: FixedPoint,
    },

    // Oracle errors
    #[error("oracle price stale for market {market_id}: last update block {last_block}, current {current_block}")]
    OraclePriceStale {
        market_id: MarketId,
        last_block: u64,
        current_block: u64,
    },

    #[error("no oracle price for market {0}")]
    NoOraclePrice(MarketId),

    #[error("insufficient oracle submissions: got {got}, need {need}")]
    InsufficientOracleSubmissions { got: usize, need: usize },

    // Lockbox errors
    #[error("insufficient native balance: have {have}, need {need}")]
    InsufficientNativeBalance { have: FixedPoint, need: FixedPoint },

    #[error("insufficient EVM balance: have {have}, need {need}")]
    InsufficientEvmBalance { have: U256, need: U256 },

    // Precompile errors
    #[error("invalid precompile input: {0}")]
    InvalidPrecompileInput(String),

    #[error("unknown precompile function selector: {0:#010x}")]
    UnknownSelector(u32),

    // Market errors
    #[error("market not found: {0}")]
    MarketNotFound(MarketId),

    // Persistence errors
    #[error("state error: {0}")]
    State(#[from] torus_state::StateError),

    #[error("borsh error: {0}")]
    Borsh(String),

    #[error("missing column family: {0}")]
    MissingCf(&'static str),

    // FIX ECON-FIND-20: Stale oracle fallback price
    #[error("stale oracle price for market {0}")]
    StaleOraclePrice(MarketId),

    // FIX ECON-FIND-26: Overflow on u128 → i128 cast
    #[error("overflow: {0}")]
    Overflow(String),

    // FIX ECON-FIND-27: Invalid enum discriminant from ABI input
    #[error("invalid input: {0}")]
    InvalidInput(String),
}
