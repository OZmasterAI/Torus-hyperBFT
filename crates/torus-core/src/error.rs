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

    /// A reader precompile's work would exceed the budget its caller's gas
    /// pays for ([`crate::precompiles::ReadMeter`]); the EVM call runs out of gas.
    #[error("precompile out of gas")]
    PrecompileOutOfGas,

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

    /// R02 branch 3: consensus bytes that every validator holds identically
    /// and decodes alike fail to decode as the type a reader expects (the
    /// classic getOrderBook reads a production whole-book blob as the legacy
    /// `OrderBookSnapshot`). NOT a local fault. Prints exactly like `Borsh`,
    /// so the EVM revert bytes are unchanged.
    #[error("borsh error: {0}")]
    DeterministicDecode(String),

    #[error("missing column family: {0}")]
    MissingCf(&'static str),

    /// R02 branch 5 (owner option A): a precompile hit a LOCAL storage fault
    /// that it must not answer around (0x0800 getPosition: the UPnL oracle
    /// read failed on this node). Local. Never a revert: the EVM precompile
    /// provider records it, aborts the execution (`EvmError::LocalFault`)
    /// and the consensus app fail-stops the node.
    #[error("precompile local fault: {0}")]
    PrecompileLocalFault(String),

    /// The persisted order-book layout is not one this read serves (unknown
    /// mode marker, mixed layouts, a key shape this build cannot read,
    /// getOrderBook under the order-row layout): the same on every validator
    /// with the same chain and build, so NOT a local fault (R02 owner s106).
    /// Readers MUST surface this instead of reporting an empty book — a
    /// silently empty book is indistinguishable from "no orders" and has
    /// already shipped one production bug.
    #[error("order-book layout error: {0}")]
    BookLayout(String),

    /// R02 branch 3: a persisted book row that does not decode or contradicts
    /// another on THIS node (corrupt meta / order / level / stop / classic
    /// row, missing meta row, ids or seqs that disagree, a stale or lost
    /// node-local order store). A local fault. Same message text as
    /// `BookLayout`, so RPC errors and EVM revert data are unchanged.
    #[error("order-book layout error: {0}")]
    BookCorrupt(String),

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

impl CoreError {
    /// R02: a LOCAL fault — this node's storage failed, bytes it stored do
    /// not decode, or a column family is missing — so its post-state for the
    /// block cannot be trusted and the node must fail-stop. Same rule as
    /// `torus_economics::EconomicsError::is_local_fault`. Every other variant
    /// is a validation, user, oracle or invariant error that every validator
    /// hits alike on the same state; halting on those would stop the chain.
    /// `BookLayout` is not one: it reports a read that does not serve the
    /// chain's layout (`getOrderBook` under order rows), which every
    /// validator hits alike; the book loaders fail-stop on it themselves.
    /// `BookCorrupt` (a book row corrupt on this node) is one.
    pub fn is_local_fault(&self) -> bool {
        matches!(
            self,
            Self::State(_)
                | Self::Borsh(_)
                | Self::MissingCf(_)
                | Self::BookCorrupt(_)
                | Self::PrecompileLocalFault(_)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use torus_state::StateError;

    /// R02: the expected class of every variant. Exhaustive on purpose (no
    /// `_` arm): a new variant does not compile here until someone decides
    /// whether it is a local fault.
    fn expected_local(e: &CoreError) -> bool {
        use CoreError::*;
        match e {
            State(_) | Borsh(_) | MissingCf(_) | BookCorrupt(_) | PrecompileLocalFault(_) => true,
            OrderNotFound(_)
            | InvalidQuantity
            | InvalidPrice
            | InvalidTickSize { .. }
            | InvalidTriggerPrice
            | MaxOrdersExceeded { .. }
            | InvalidOraclePrice { .. }
            | DustOrder { .. }
            | InsufficientMargin { .. }
            | MaxLeverageExceeded { .. }
            | NotLiquidatable { .. }
            | OraclePriceStale { .. }
            | NoOraclePrice(_)
            | InsufficientOracleSubmissions { .. }
            | InsufficientNativeBalance { .. }
            | InsufficientEvmBalance { .. }
            | InvalidPrecompileInput(_)
            | PrecompileOutOfGas
            | UnknownSelector(_)
            | MarketNotFound(_)
            | BookLayout(_)
            | DeterministicDecode(_)
            | StaleOraclePrice(_)
            | Overflow(_)
            | InvalidInput(_) => false,
        }
    }

    #[test]
    fn only_storage_decode_and_missing_cf_errors_are_local_faults() {
        let fp = FixedPoint::ONE;
        let every_variant = [
            CoreError::OrderNotFound(1),
            CoreError::InvalidQuantity,
            CoreError::InvalidPrice,
            CoreError::InvalidTickSize {
                price: fp,
                tick_size: fp,
            },
            CoreError::InvalidTriggerPrice,
            CoreError::MaxOrdersExceeded { count: 1, max: 1 },
            CoreError::InvalidOraclePrice { market_id: 1 },
            CoreError::DustOrder {
                qty: fp,
                lot_size: fp,
            },
            CoreError::InsufficientMargin {
                required: fp,
                available: fp,
            },
            CoreError::MaxLeverageExceeded {
                leverage: 2,
                max_leverage: 1,
                notional: fp,
            },
            CoreError::NotLiquidatable {
                equity: fp,
                maintenance: fp,
            },
            CoreError::OraclePriceStale {
                market_id: 1,
                last_block: 1,
                current_block: 2,
            },
            CoreError::NoOraclePrice(1),
            CoreError::InsufficientOracleSubmissions { got: 0, need: 1 },
            CoreError::InsufficientNativeBalance { have: fp, need: fp },
            CoreError::InsufficientEvmBalance {
                have: U256::ZERO,
                need: U256::from(1u8),
            },
            CoreError::InvalidPrecompileInput("x".into()),
            CoreError::PrecompileOutOfGas,
            CoreError::UnknownSelector(0),
            CoreError::MarketNotFound(1),
            CoreError::State(StateError::InvalidData("x".into())),
            CoreError::State(StateError::Io(std::io::Error::other("x"))),
            CoreError::State(StateError::MissingColumnFamily("x".into())),
            CoreError::Borsh("x".into()),
            CoreError::DeterministicDecode("x".into()),
            CoreError::MissingCf("x"),
            CoreError::PrecompileLocalFault("x".into()),
            CoreError::BookLayout("x".into()),
            CoreError::BookCorrupt("x".into()),
            CoreError::StaleOraclePrice(1),
            CoreError::Overflow("x".into()),
            CoreError::InvalidInput("x".into()),
        ];
        let local: Vec<String> = every_variant
            .iter()
            .filter(|e| e.is_local_fault())
            .map(|e| e.to_string())
            .collect();
        assert_eq!(local.len(), 7, "{local:?}");
        for e in &every_variant {
            assert_eq!(e.is_local_fault(), expected_local(e), "{e}");
        }
    }
}
