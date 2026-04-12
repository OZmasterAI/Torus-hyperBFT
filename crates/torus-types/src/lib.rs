//! Shared types for the Torus-hyperBFT blockchain.
//!
//! Leaf crate with no internal dependencies. All other crates depend on this.

pub use alloy_primitives::{Address, Bloom, Bytes, B256, U256};

use serde::{Deserialize, Serialize};

// ============================================================================
// Identifiers
// ============================================================================

/// Unique order identifier (128-bit for global uniqueness).
pub type OrderId = u128;

/// Market identifier.
pub type MarketId = u64;

// ============================================================================
// Fixed-Point Numeric Type (§5.1)
// ============================================================================

/// Fixed-point decimal with 8 implicit decimal places.
/// Stored as i128 internally. 1.00000000 = 100_000_000.
/// Follows Hyperliquid's pattern of implicit decimal scaling.
///
/// All prices and quantities use this type for full cross-validator
/// determinism. No floating-point arithmetic anywhere in consensus-critical code.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct FixedPoint(i128);

impl FixedPoint {
    pub const DECIMALS: u32 = 8;
    pub const SCALE: i128 = 100_000_000; // 10^8
    pub const ZERO: Self = Self(0);
    pub const ONE: Self = Self(Self::SCALE);

    pub fn from_raw(raw: i128) -> Self {
        Self(raw)
    }

    pub fn raw(&self) -> i128 {
        self.0
    }
}

impl std::ops::Mul for FixedPoint {
    type Output = Self;

    /// Multiply two FixedPoints: (a * b) / SCALE.
    /// Uses ethnum::i256 to avoid i128 overflow on intermediate product.
    fn mul(self, other: Self) -> Self {
        use ethnum::i256;
        Self((i256::from(self.0) * i256::from(other.0) / i256::from(Self::SCALE)).as_i128())
    }
}

impl std::ops::Div for FixedPoint {
    type Output = Self;

    /// Divide two FixedPoints: (a * SCALE) / b.
    /// Uses ethnum::i256 to avoid i128 overflow on intermediate product.
    fn div(self, other: Self) -> Self {
        use ethnum::i256;
        Self((i256::from(self.0) * i256::from(Self::SCALE) / i256::from(other.0)).as_i128())
    }
}

// ============================================================================
// Cryptographic Primitives
// ============================================================================

/// EIP-712 secp256k1 signature (65 bytes: r[32] || s[32] || v[1]).
/// Used for both EVM transactions and native action signing.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Signature {
    pub v: u8,
    pub r: [u8; 32],
    pub s: [u8; 32],
}

/// Ed25519 public key (32 bytes) for validator consensus signing.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PublicKey(pub [u8; 32]);

// ============================================================================
// Block Types (§3.2)
// ============================================================================

/// Canonical Torus block — serialized into hotstuff_rs Data field.
/// Each TorusBlock becomes a single Datum in the library's Vec<Datum>.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TorusBlock {
    pub header: TorusBlockHeader,
    pub native_actions: Vec<NativeAction>,
    /// EVM transactions (RLP-encoded, signed).
    pub evm_transactions: Vec<Vec<u8>>,
    /// CoreWriter action queue (from previous block's EVM).
    pub core_writer_actions: Vec<CoreWriterAction>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TorusBlockHeader {
    /// Block height — populated from hotstuff_rs Block.height during bridge processing.
    pub height: u64,
    pub timestamp: u64,
    pub proposer: Address,
    /// State root after executing all actions in this block.
    pub state_root: B256,
    /// Receipts root (EVM transactions only).
    pub receipts_root: B256,
    /// Logs bloom (EVM transactions only).
    pub logs_bloom: Bloom,
    /// Gas used by EVM transactions.
    pub evm_gas_used: u64,
    /// Gas limit for EVM transactions in this block.
    pub evm_gas_limit: u64,
    /// Native action count.
    pub native_action_count: u32,
    /// EVM transaction count.
    pub evm_tx_count: u32,
    /// Base fee per gas (EIP-1559).
    pub base_fee_per_gas: u64,
    /// Epoch number (validator set epoch).
    pub epoch: u64,
    /// Validator set hash for this epoch.
    pub validator_set_hash: B256,
}

/// Block body — the non-header payload, stored separately in cf_block_bodies.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TorusBlockBody {
    pub native_actions: Vec<NativeAction>,
    pub evm_transactions: Vec<Vec<u8>>,
    pub core_writer_actions: Vec<CoreWriterAction>,
}

impl TorusBlock {
    pub fn body(&self) -> TorusBlockBody {
        TorusBlockBody {
            native_actions: self.native_actions.clone(),
            evm_transactions: self.evm_transactions.clone(),
            core_writer_actions: self.core_writer_actions.clone(),
        }
    }
}

// ============================================================================
// Native Action Types (§5.6)
// ============================================================================

/// All native (non-EVM) actions processed by torus-core.
/// Analogous to Hyperliquid's ~70 HyperCore action types.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum NativeAction {
    // === Order Book ===
    PlaceOrder(PlaceOrderParams),
    CancelOrder {
        order_id: OrderId,
    },
    CancelAllOrders {
        market_id: Option<MarketId>,
    },
    ModifyOrder {
        order_id: OrderId,
        new_price: Option<FixedPoint>,
        new_qty: Option<FixedPoint>,
    },

    // === Transfers ===
    /// Spot -> Perp balance.
    TransferToPerp {
        amount: U256,
    },
    /// Perp -> Spot balance.
    TransferToSpot {
        amount: U256,
    },
    /// Withdraw TRS to EVM address.
    Withdraw {
        amount: U256,
        to: Address,
    },

    // === Staking ===
    Delegate {
        validator: Address,
        amount: U256,
    },
    Undelegate {
        validator: Address,
        amount: U256,
    },
    /// Irreversible permanent lock.
    PermanentStake {
        amount: U256,
    },
    ClaimRewards,

    // === Governance ===
    SubmitProposal(Proposal),
    Vote {
        proposal_id: u64,
        option: VoteOption,
    },

    // === Oracle ===
    SubmitOraclePrices(OracleSubmission),

    // === Validator ===
    RegisterValidator {
        pubkey: PublicKey,
        commission: u16,
    },
    UpdateCommission {
        new_rate: u16,
    },
    JailVote {
        target: Address,
    },
    UnjailSelf,

    // === Admin (governance-gated) ===
    UpdateMarketParams {
        market_id: MarketId,
        params: MarketParams,
    },
    ListMarket(MarketListing),
    DelistMarket {
        market_id: MarketId,
    },
}

/// Signed native action envelope. Submitted via torus_submitNativeAction RPC.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SignedNativeAction {
    pub action: NativeAction,
    /// Millisecond timestamp nonce (not sequential — allows out-of-order processing).
    pub nonce: u64,
    /// EIP-712 signature (r, s, v) over the typed data hash.
    pub signature: Signature,
}

// ============================================================================
// Order Types
// ============================================================================

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PlaceOrderParams {
    pub market_id: MarketId,
    pub is_buy: bool,
    pub price: FixedPoint,
    pub quantity: FixedPoint,
    pub order_type: OrderType,
    pub time_in_force: TimeInForce,
    pub reduce_only: bool,
    pub client_order_id: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum OrderType {
    Limit,
    Market,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TimeInForce {
    /// Good-til-cancelled.
    GTC,
    /// Immediate-or-cancel.
    IOC,
    /// Fill-or-kill.
    FOK,
}

// ============================================================================
// Governance Types
// ============================================================================

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Proposal {
    pub title: String,
    pub description: String,
    pub action: ProposalAction,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ProposalAction {
    UpdateMarketParams {
        market_id: MarketId,
        params: MarketParams,
    },
    ListMarket(MarketListing),
    DelistMarket {
        market_id: MarketId,
    },
    ParameterChange {
        key: String,
        value: String,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum VoteOption {
    Yes,
    No,
    Abstain,
}

// ============================================================================
// Oracle Types
// ============================================================================

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OracleSubmission {
    pub prices: Vec<(MarketId, FixedPoint)>,
    pub timestamp: u64,
}

// ============================================================================
// Market Types
// ============================================================================

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MarketParams {
    pub tick_size: FixedPoint,
    pub lot_size: FixedPoint,
    pub max_leverage: u32,
    pub maintenance_margin_bps: u32,
    pub max_funding_rate_bps: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MarketListing {
    pub base_asset: String,
    pub quote_asset: String,
    pub tick_size: FixedPoint,
    pub lot_size: FixedPoint,
    pub max_leverage: u32,
    pub maintenance_margin_bps: u32,
}

// ============================================================================
// CoreWriter Actions (§10.3)
// ============================================================================

/// Write actions queued from EVM to native state via the CoreWriter precompile.
/// Delayed by one block to prevent frontrunning.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum CoreWriterAction {
    PlaceOrder {
        market_id: MarketId,
        is_buy: bool,
        price: FixedPoint,
        quantity: FixedPoint,
        order_type: OrderType,
        time_in_force: TimeInForce,
    },
    CancelOrder {
        order_id: OrderId,
    },
    Delegate {
        validator: Address,
        amount: U256,
    },
    PermanentStake {
        amount: U256,
    },
}

// ============================================================================
// State Types (§6)
// ============================================================================

/// Account/storage changes from execution.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StateDiff {
    pub address: Address,
    pub balance: Option<U256>,
    pub nonce: Option<u64>,
    pub storage: Vec<(B256, B256)>,
    pub code: Option<Vec<u8>>,
}

// ============================================================================
// Chain Config
// ============================================================================

/// Chain parameters — loaded from genesis, immutable after launch.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChainConfig {
    pub chain_id: u64,
    pub chain_name: String,
    pub evm_gas_limit: u64,
    pub base_fee_per_gas: u64,
    pub epoch_length: u64,
    pub max_validators: u32,
    pub min_stake: U256,
    /// Fee split ratios in basis points (must sum to 10_000).
    pub fee_burn_bps: u32,
    pub fee_validator_bps: u32,
    pub fee_treasury_bps: u32,
    pub fee_dev_pool_bps: u32,
}

// ============================================================================
// Receipt / Log (§4)
// ============================================================================

/// EVM transaction execution receipt.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Receipt {
    pub tx_hash: B256,
    pub block_number: u64,
    pub block_hash: B256,
    pub tx_index: u32,
    pub cumulative_gas_used: u64,
    pub gas_used: u64,
    pub contract_address: Option<Address>,
    pub logs: Vec<Log>,
    pub logs_bloom: Bloom,
    pub status: bool,
    pub effective_gas_price: u64,
}

/// Event log (ERC-20 Transfer, etc.).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Log {
    pub address: Address,
    /// topic[0] = event signature hash.
    pub topics: Vec<B256>,
    pub data: Vec<u8>,
}

// ============================================================================
// Validator Types
// ============================================================================

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ValidatorInfo {
    pub address: Address,
    pub pubkey: PublicKey,
    pub power: u64,
    /// Commission rate in basis points.
    pub commission_bps: u16,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ValidatorSet {
    pub validators: Vec<ValidatorInfo>,
    pub epoch: u64,
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_point_basic_arithmetic() {
        let one = FixedPoint::ONE;
        let two = FixedPoint::from_raw(2 * FixedPoint::SCALE);

        assert_eq!(one * two, two);
        assert_eq!(two / two, one);
        assert_eq!(FixedPoint::ZERO * one, FixedPoint::ZERO);
    }

    #[test]
    fn fixed_point_precision() {
        // 1.5 * 2.0 = 3.0
        let a = FixedPoint::from_raw(150_000_000);
        let b = FixedPoint::from_raw(200_000_000);
        let expected = FixedPoint::from_raw(300_000_000);
        assert_eq!(a * b, expected);
    }

    #[test]
    fn fixed_point_division() {
        // 10.0 / 3.0 = 3.33333333 (truncated)
        let ten = FixedPoint::from_raw(10 * FixedPoint::SCALE);
        let three = FixedPoint::from_raw(3 * FixedPoint::SCALE);
        let result = ten / three;
        assert_eq!(result.raw(), 333_333_333);
    }
}
