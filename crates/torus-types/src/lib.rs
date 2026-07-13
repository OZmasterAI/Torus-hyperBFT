//! Shared types for the Torus-hyperBFT blockchain.
//!
//! Leaf crate with no internal dependencies. All other crates depend on this.

pub use alloy_primitives::{Address, Bloom, Bytes, B256, U256};

use serde::{Deserialize, Serialize};

pub mod eip712;

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

/// Arithmetic error for checked FixedPoint operations.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArithmeticError {
    Overflow,
    Underflow,
    DivisionByZero,
}

impl std::fmt::Display for ArithmeticError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Overflow => write!(f, "arithmetic overflow"),
            Self::Underflow => write!(f, "arithmetic underflow"),
            Self::DivisionByZero => write!(f, "division by zero"),
        }
    }
}

impl std::error::Error for ArithmeticError {}

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
    pub const MAX: Self = Self(i128::MAX);
    pub const MIN: Self = Self(i128::MIN);

    pub fn from_raw(raw: i128) -> Self {
        Self(raw)
    }

    pub fn raw(&self) -> i128 {
        self.0
    }

    /// Checked addition. Returns `Err(ArithmeticError::Overflow)` on overflow.
    pub fn checked_add(self, rhs: Self) -> Result<Self, ArithmeticError> {
        self.0
            .checked_add(rhs.0)
            .map(FixedPoint)
            .ok_or(ArithmeticError::Overflow)
    }

    /// Checked subtraction. Returns `Err(ArithmeticError::Underflow)` on underflow.
    pub fn checked_sub(self, rhs: Self) -> Result<Self, ArithmeticError> {
        self.0
            .checked_sub(rhs.0)
            .map(FixedPoint)
            .ok_or(ArithmeticError::Underflow)
    }

    /// Checked multiplication with i256 intermediate to avoid overflow.
    pub fn checked_mul(self, rhs: Self) -> Result<Self, ArithmeticError> {
        use ethnum::i256;
        let result = i256::from(self.0) * i256::from(rhs.0) / i256::from(Self::SCALE);
        if result > i256::from(i128::MAX) || result < i256::from(i128::MIN) {
            return Err(ArithmeticError::Overflow);
        }
        Ok(FixedPoint(result.as_i128()))
    }

    /// Checked division with i256 intermediate.
    /// Returns `Err(ArithmeticError::DivisionByZero)` on zero divisor.
    pub fn checked_div(self, rhs: Self) -> Result<Self, ArithmeticError> {
        if rhs.0 == 0 {
            return Err(ArithmeticError::DivisionByZero);
        }
        use ethnum::i256;
        let result = i256::from(self.0) * i256::from(Self::SCALE) / i256::from(rhs.0);
        if result > i256::from(i128::MAX) || result < i256::from(i128::MIN) {
            return Err(ArithmeticError::Overflow);
        }
        Ok(FixedPoint(result.as_i128()))
    }
}

impl std::ops::Mul for FixedPoint {
    type Output = Self;
    fn mul(self, rhs: Self) -> Self {
        self.checked_mul(rhs)
            .expect("FixedPoint multiplication overflow")
    }
}

impl std::ops::Div for FixedPoint {
    type Output = Self;
    fn div(self, rhs: Self) -> Self {
        self.checked_div(rhs).expect("FixedPoint division error")
    }
}

impl std::ops::Add for FixedPoint {
    type Output = Self;
    fn add(self, rhs: Self) -> Self {
        self.checked_add(rhs).expect("FixedPoint addition overflow")
    }
}

impl std::ops::Sub for FixedPoint {
    type Output = Self;
    fn sub(self, rhs: Self) -> Self {
        self.checked_sub(rhs)
            .expect("FixedPoint subtraction underflow")
    }
}

impl std::ops::Neg for FixedPoint {
    type Output = Self;
    fn neg(self) -> Self {
        Self(-self.0)
    }
}

impl std::ops::AddAssign for FixedPoint {
    fn add_assign(&mut self, rhs: Self) {
        *self = self.checked_add(rhs).expect("FixedPoint addition overflow");
    }
}

impl std::ops::SubAssign for FixedPoint {
    fn sub_assign(&mut self, rhs: Self) {
        *self = self
            .checked_sub(rhs)
            .expect("FixedPoint subtraction underflow");
    }
}

impl std::fmt::Display for FixedPoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let whole = self.0 / Self::SCALE;
        let frac = (self.0 % Self::SCALE).unsigned_abs();
        if self.0 < 0 && whole == 0 {
            write!(f, "-0.{frac:08}")
        } else {
            write!(f, "{whole}.{frac:08}")
        }
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

/// Scope of actions a session key is authorized to perform.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionScope {
    /// PlaceOrder, PlaceOrderBatch, CancelOrder, ModifyOrder, CancelAllOrders only.
    Trading,
    /// TransferToPerp, TransferToSpot only.
    TransfersOnly,
    /// Everything except CreateSession, RevokeSession, Withdraw, Delegate,
    /// Undelegate, PermanentStake, ClaimRewards.
    Full,
}

impl SessionScope {
    pub fn allows(&self, action: &NativeAction) -> bool {
        match self {
            // LOCKSTEP-DEPLOY WARNING (O2/G3): scope is enforced on the
            // CONSENSUS path too (eip712.rs batch verify guard) — on an old
            // validator a Trading-scoped PlaceOrderBatch resolves to None,
            // which the exec pipeline treats as an invalid signature and
            // SLASHES + TOMBSTONES the block's PROPOSER (app.rs invalid-
            // attestation arm). The ENTIRE fleet must run this change before
            // any client signs batches under a Trading-scoped session.
            SessionScope::Trading => matches!(
                action,
                NativeAction::PlaceOrder(_)
                    | NativeAction::PlaceOrderBatch(_)
                    | NativeAction::CancelOrder { .. }
                    | NativeAction::ModifyOrder { .. }
                    | NativeAction::CancelAllOrders { .. }
            ),
            SessionScope::TransfersOnly => matches!(
                action,
                NativeAction::TransferToPerp { .. } | NativeAction::TransferToSpot { .. }
            ),
            SessionScope::Full => !matches!(
                action,
                NativeAction::CreateSession { .. }
                    | NativeAction::RevokeSession { .. }
                    | NativeAction::Withdraw { .. }
                    | NativeAction::Delegate { .. }
                    | NativeAction::Undelegate { .. }
                    | NativeAction::PermanentStake { .. }
                    | NativeAction::ClaimRewards
            ),
        }
    }
}

/// Ed25519 signature (64 bytes) with custom serde as two 32-byte halves.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ed25519Sig(pub [u8; 64]);

impl Serialize for Ed25519Sig {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeTuple;
        let mut seq = serializer.serialize_tuple(64)?;
        for byte in &self.0 {
            seq.serialize_element(byte)?;
        }
        seq.end()
    }
}

impl<'de> Deserialize<'de> for Ed25519Sig {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = Ed25519Sig;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                write!(f, "64 bytes")
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> Result<Ed25519Sig, A::Error> {
                let mut buf = [0u8; 64];
                for (i, byte) in buf.iter_mut().enumerate() {
                    *byte = seq
                        .next_element()?
                        .ok_or_else(|| serde::de::Error::invalid_length(i, &"64 bytes"))?;
                }
                Ok(Ed25519Sig(buf))
            }
        }
        deserializer.deserialize_tuple(64, Visitor)
    }
}

/// Signature type for native actions — either traditional EIP-712 ECDSA or ed25519 session key.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ActionSignature {
    /// Traditional EIP-712 ECDSA signature (secp256k1).
    Eip712(Signature),
    /// Ed25519 session key signature.
    Session {
        session_pubkey: [u8; 32],
        sig: Ed25519Sig,
    },
}

/// Stored session key data (persisted in state DB).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionData {
    pub owner: Address,
    pub expiry: u64,
    pub scope: SessionScope,
    pub created_at: u64,
}

mod attestation_bytes {
    use serde::{Deserializer, Serializer};

    pub fn default() -> [u8; 64] {
        [0u8; 64]
    }

    pub fn serialize<S: Serializer>(data: &[u8; 64], s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeTuple;
        let mut tup = s.serialize_tuple(64)?;
        for byte in data {
            tup.serialize_element(byte)?;
        }
        tup.end()
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; 64], D::Error> {
        struct V;
        impl<'de> serde::de::Visitor<'de> for V {
            type Value = [u8; 64];
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                write!(f, "64 bytes")
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> Result<[u8; 64], A::Error> {
                let mut buf = [0u8; 64];
                for (i, byte) in buf.iter_mut().enumerate() {
                    *byte = seq
                        .next_element()?
                        .ok_or_else(|| serde::de::Error::invalid_length(i, &"64 bytes"))?;
                }
                Ok(buf)
            }
        }
        d.deserialize_tuple(64, V)
    }
}

// ============================================================================
// Block Types (§3.2)
// ============================================================================

/// Canonical Torus block — serialized into hotstuff_rs Data field.
/// Each TorusBlock becomes a single Datum in the library's Vec<Datum>.
///
/// FIX CONS-PF-02: native_actions contains `SignedNativeAction` (with EIP-712
/// signatures) so that validators can independently recover senders and verify
/// authorization. Previously stored bare `NativeAction` which made the proposer
/// a trusted party for native action authorization.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TorusBlock {
    pub header: TorusBlockHeader,
    pub native_actions: Vec<SignedNativeAction>,
    /// EVM transactions (RLP-encoded, signed).
    pub evm_transactions: Vec<Vec<u8>>,
    /// CoreWriter action queue (from previous block's EVM).
    pub core_writer_actions: Vec<CoreWriterAction>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TorusBlockHeader {
    /// Block height — populated from hotstuff_rs Block.height during bridge processing.
    pub height: u64,
    /// Keccak canonical hash of the PARENT block's header
    /// (`keccak256(parent.canonical_header_bytes())`).
    ///
    /// This is the ancestry commitment that makes block identity depend on the
    /// chain that produced it: two blocks at the same height that descend from
    /// DIFFERENT parents now hash differently, so a hash-vs-hash fork check at a
    /// height is sound. Genesis (`genesis_parent_header`, height 0) uses
    /// `B256::ZERO`; the first real block (height 1) commits to
    /// `keccak256(genesis_parent_header().canonical_header_bytes())`.
    ///
    /// NOTE: this is the keccak canonical header hash (what RPC `parentHash` and
    /// the fork checker compare), which is DISTINCT from the hotstuff consensus
    /// `justify.block` id (`hash(height, justify, data_hash)`). Both identify the
    /// same parent block: consensus safety is enforced by the justify QC, while
    /// RPC/EVM-level ancestry soundness is enforced by this field. An honest
    /// proposer keeps the two consistent (this field == keccak hash of the block
    /// the justify QC points at).
    ///
    /// `#[serde(default)]` so pre-hardening stored/RPC header JSON still
    /// deserializes (as `B256::ZERO`); moot on the approved fresh-genesis launch.
    #[serde(default)]
    pub parent_hash: B256,
    pub timestamp: u64,
    pub proposer: Address,
    /// State root. NOT part of `canonical_header_bytes` (block identity).
    ///
    /// This chain is commit-then-execute: headers are built PRE-execution
    /// (`app.rs::produce_block`), so the real post-execution state root of THIS
    /// block is not known when the header is hashed, and the already-computed
    /// T4.1 post-exec root (`execute_committed_block`) is not persisted per height
    /// for a parent-state-root lookup either. Rather than commit to a
    /// perpetually-`0x0` value and pretend it is a state commitment, the field is
    /// excluded from the hash preimage. Re-adding it under parent-state-root
    /// semantics (header commits to the state root AFTER executing the PARENT)
    /// requires persisting each block's post-exec composite root and guaranteeing
    /// parent execution precedes child proposal — tracked as follow-up.
    pub state_root: B256,
    /// Receipts root (EVM transactions only).
    pub receipts_root: B256,
    /// Logs bloom (EVM transactions only).
    pub logs_bloom: Bloom,
    /// Gas used by EVM transactions.
    pub evm_gas_used: u64,
    /// Total EVM fee revenue in wei: sum(gas_used * effective_gas_price).
    pub evm_fee_revenue: u128,
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
    #[serde(with = "attestation_bytes", default = "attestation_bytes::default")]
    pub sig_attestation: [u8; 64],
}

impl TorusBlockHeader {
    /// Canonical byte encoding for deterministic block hashing.
    ///
    /// Uses explicit big-endian encoding of each field in a fixed order.
    /// Unlike `serde_json`, this is stable across serde versions and struct reordering.
    pub fn canonical_header_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(256);
        buf.extend_from_slice(&self.height.to_be_bytes());
        // Ancestry commitment: the block hash now depends on the parent it was
        // built on (see `parent_hash` field docs). Placed right after height so
        // the (height, parent_hash) pair uniquely anchors the block in the chain.
        buf.extend_from_slice(self.parent_hash.as_slice());
        buf.extend_from_slice(&self.timestamp.to_be_bytes());
        buf.extend_from_slice(self.proposer.as_slice());
        // NOTE: `state_root` is intentionally NOT hashed — it is a perpetual-0x0
        // dead field under commit-then-execute (see field docs). Excluding it
        // keeps the block identity honest instead of committing to a fake state
        // commitment.
        buf.extend_from_slice(self.receipts_root.as_slice());
        buf.extend_from_slice(self.logs_bloom.as_slice());
        buf.extend_from_slice(&self.evm_gas_used.to_be_bytes());
        buf.extend_from_slice(&self.evm_fee_revenue.to_be_bytes());
        buf.extend_from_slice(&self.evm_gas_limit.to_be_bytes());
        buf.extend_from_slice(&self.native_action_count.to_be_bytes());
        buf.extend_from_slice(&self.evm_tx_count.to_be_bytes());
        buf.extend_from_slice(&self.base_fee_per_gas.to_be_bytes());
        buf.extend_from_slice(&self.epoch.to_be_bytes());
        buf.extend_from_slice(self.validator_set_hash.as_slice());
        buf
    }
}

/// Block body — the non-header payload, stored separately in cf_block_bodies.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TorusBlockBody {
    pub native_actions: Vec<SignedNativeAction>,
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

/// Compact block for consensus proposals — carries action hashes instead of full payloads.
/// Validators reconstruct the full `TorusBlock` from their local mempool.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CompactBlock {
    pub header: TorusBlockHeader,
    pub native_action_hashes: Vec<B256>,
    pub evm_transactions: Vec<Vec<u8>>,
    pub core_writer_actions: Vec<CoreWriterAction>,
}

impl CompactBlock {
    pub fn from_block(block: &TorusBlock) -> Self {
        let hashes = block
            .native_actions
            .iter()
            .map(compute_action_hash)
            .collect();
        Self {
            header: block.header.clone(),
            native_action_hashes: hashes,
            evm_transactions: block.evm_transactions.clone(),
            core_writer_actions: block.core_writer_actions.clone(),
        }
    }
}

// ============================================================================
// Native Action Types (§5.6)
// ============================================================================

/// Max orders a single `PlaceOrderBatch` may carry — the consensus-facing
/// safety ceiling. SINGLE SOURCE OF TRUTH for every enforcement layer:
/// RPC ingress + gossip/DA admission (torus-mempool `rate_limit` re-exports
/// this) and the deterministic exec-side skip (torus-bridge
/// `NativeExecutor::execute_batch` flatten). Lives here because torus-bridge
/// cannot depend on torus-mempool (both depend on torus-types).
pub const NATIVE_ORDERS_PER_BATCH_CAP: usize = 1024;

/// The consensus batch-size validity rule, as one function so every
/// enforcement layer (RPC `validate_batch_size`, gossip/DA admission, and the
/// exec-side skip) agrees byte-for-byte. A `PlaceOrderBatch` is valid iff it
/// carries `[1, NATIVE_ORDERS_PER_BATCH_CAP]` orders — empty and oversize both
/// fail. Callers own their own error text; only the predicate is shared, so the
/// rule can never drift between the layer that admits and the layer that skips.
#[inline]
pub fn batch_len_within_cap(len: usize) -> bool {
    (1..=NATIVE_ORDERS_PER_BATCH_CAP).contains(&len)
}

/// All native (non-EVM) actions processed by torus-core.
/// Analogous to Hyperliquid's ~70 HyperCore action types.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum NativeAction {
    // === Order Book ===
    PlaceOrder(PlaceOrderParams),
    /// Many orders under one signature + one nonce (market-maker batch submission).
    /// Throughput keystone (Phase B): a single secp256k1 ecrecover and a single
    /// `(sender, nonce)` cover every order, and the per-block action cap counts the
    /// whole batch as one action — so `orders/block = action_cap * batch_size`.
    PlaceOrderBatch(Vec<PlaceOrderParams>),
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

    /// FIX ECON-FIND-15: Top up validator's self-stake from their balance.
    TopUpSelfStake {
        amount: U256,
    },

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
    RotateValidatorKey {
        new_pubkey: PublicKey,
    },

    // === Session Keys ===
    CreateSession {
        session_pubkey: [u8; 32],
        expiry: u64,
        scope: SessionScope,
    },
    RevokeSession {
        session_pubkey: [u8; 32],
    },

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

impl NativeAction {
    /// Append a single order's canonical body (no variant tag) to `buf`.
    ///
    /// Shared by the `PlaceOrder` and `PlaceOrderBatch` encodings so a batched
    /// order is byte-identical to the same order submitted singly.
    fn encode_place_order_params(buf: &mut Vec<u8>, p: &PlaceOrderParams) {
        buf.extend_from_slice(&p.market_id.to_be_bytes());
        buf.push(p.is_buy as u8);
        buf.extend_from_slice(&p.price.raw().to_be_bytes());
        buf.extend_from_slice(&p.quantity.raw().to_be_bytes());
        match &p.order_type {
            OrderType::Limit => buf.push(0),
            OrderType::Market => buf.push(1),
            OrderType::StopMarket { trigger } => {
                buf.push(2);
                buf.extend_from_slice(&trigger.raw().to_be_bytes());
            }
            OrderType::StopLimit { trigger, limit } => {
                buf.push(3);
                buf.extend_from_slice(&trigger.raw().to_be_bytes());
                buf.extend_from_slice(&limit.raw().to_be_bytes());
            }
        }
        buf.push(match p.time_in_force {
            TimeInForce::GTC => 0,
            TimeInForce::IOC => 1,
            TimeInForce::FOK => 2,
            TimeInForce::PostOnly => 3,
        });
        buf.push(p.reduce_only as u8);
        match p.client_order_id {
            Some(id) => {
                buf.push(1);
                buf.extend_from_slice(&id.to_be_bytes());
            }
            None => buf.push(0),
        }
    }

    /// Deterministic canonical byte encoding for consensus-critical hashing.
    ///
    /// Unlike `Debug` formatting or `serde_json`, this encoding is guaranteed stable
    /// across compiler versions, serde versions, and struct field reordering.
    /// Each variant has a unique 1-byte tag followed by fixed-width big-endian fields.
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(64);
        match self {
            NativeAction::PlaceOrder(p) => {
                buf.push(0);
                Self::encode_place_order_params(&mut buf, p);
            }
            NativeAction::PlaceOrderBatch(orders) => {
                buf.push(25);
                buf.extend_from_slice(&(orders.len() as u32).to_be_bytes());
                for p in orders {
                    Self::encode_place_order_params(&mut buf, p);
                }
            }
            NativeAction::CancelOrder { order_id } => {
                buf.push(1);
                buf.extend_from_slice(&order_id.to_be_bytes());
            }
            NativeAction::CancelAllOrders { market_id } => {
                buf.push(2);
                match market_id {
                    Some(id) => {
                        buf.push(1);
                        buf.extend_from_slice(&id.to_be_bytes());
                    }
                    None => buf.push(0),
                }
            }
            NativeAction::ModifyOrder {
                order_id,
                new_price,
                new_qty,
            } => {
                buf.push(3);
                buf.extend_from_slice(&order_id.to_be_bytes());
                match new_price {
                    Some(p) => {
                        buf.push(1);
                        buf.extend_from_slice(&p.raw().to_be_bytes());
                    }
                    None => buf.push(0),
                }
                match new_qty {
                    Some(q) => {
                        buf.push(1);
                        buf.extend_from_slice(&q.raw().to_be_bytes());
                    }
                    None => buf.push(0),
                }
            }
            NativeAction::TransferToPerp { amount } => {
                buf.push(4);
                buf.extend_from_slice(&amount.to_be_bytes::<32>());
            }
            NativeAction::TransferToSpot { amount } => {
                buf.push(5);
                buf.extend_from_slice(&amount.to_be_bytes::<32>());
            }
            NativeAction::Withdraw { amount, to } => {
                buf.push(6);
                buf.extend_from_slice(&amount.to_be_bytes::<32>());
                buf.extend_from_slice(to.as_slice());
            }
            NativeAction::Delegate { validator, amount } => {
                buf.push(7);
                buf.extend_from_slice(validator.as_slice());
                buf.extend_from_slice(&amount.to_be_bytes::<32>());
            }
            NativeAction::Undelegate { validator, amount } => {
                buf.push(8);
                buf.extend_from_slice(validator.as_slice());
                buf.extend_from_slice(&amount.to_be_bytes::<32>());
            }
            NativeAction::PermanentStake { amount } => {
                buf.push(9);
                buf.extend_from_slice(&amount.to_be_bytes::<32>());
            }
            NativeAction::ClaimRewards => {
                buf.push(10);
            }
            NativeAction::SubmitProposal(p) => {
                buf.push(11);
                buf.extend_from_slice(&(p.title.len() as u32).to_be_bytes());
                buf.extend_from_slice(p.title.as_bytes());
                buf.extend_from_slice(&(p.description.len() as u32).to_be_bytes());
                buf.extend_from_slice(p.description.as_bytes());
                match &p.action {
                    ProposalAction::UpdateMarketParams { market_id, params } => {
                        buf.push(0);
                        buf.extend_from_slice(&market_id.to_be_bytes());
                        buf.extend_from_slice(&params.tick_size.raw().to_be_bytes());
                        buf.extend_from_slice(&params.lot_size.raw().to_be_bytes());
                        buf.extend_from_slice(&params.max_leverage.to_be_bytes());
                        buf.extend_from_slice(&params.maintenance_margin_bps.to_be_bytes());
                        buf.extend_from_slice(&params.max_funding_rate_bps.to_be_bytes());
                    }
                    ProposalAction::ListMarket(l) => {
                        buf.push(1);
                        buf.extend_from_slice(&(l.base_asset.len() as u32).to_be_bytes());
                        buf.extend_from_slice(l.base_asset.as_bytes());
                        buf.extend_from_slice(&(l.quote_asset.len() as u32).to_be_bytes());
                        buf.extend_from_slice(l.quote_asset.as_bytes());
                        buf.extend_from_slice(&l.tick_size.raw().to_be_bytes());
                        buf.extend_from_slice(&l.lot_size.raw().to_be_bytes());
                        buf.extend_from_slice(&l.max_leverage.to_be_bytes());
                        buf.extend_from_slice(&l.maintenance_margin_bps.to_be_bytes());
                    }
                    ProposalAction::DelistMarket { market_id } => {
                        buf.push(2);
                        buf.extend_from_slice(&market_id.to_be_bytes());
                    }
                    ProposalAction::ParameterChange { key, value } => {
                        buf.push(3);
                        buf.extend_from_slice(&(key.len() as u32).to_be_bytes());
                        buf.extend_from_slice(key.as_bytes());
                        buf.extend_from_slice(&(value.len() as u32).to_be_bytes());
                        buf.extend_from_slice(value.as_bytes());
                    }
                    ProposalAction::ValidatorRegistration { candidate } => {
                        buf.push(4);
                        buf.extend_from_slice(candidate.as_slice());
                    }
                }
            }
            NativeAction::Vote {
                proposal_id,
                option,
            } => {
                buf.push(12);
                buf.extend_from_slice(&proposal_id.to_be_bytes());
                buf.push(match option {
                    VoteOption::Yes => 0,
                    VoteOption::No => 1,
                    VoteOption::Abstain => 2,
                });
            }
            NativeAction::SubmitOraclePrices(s) => {
                buf.push(13);
                buf.extend_from_slice(&(s.prices.len() as u32).to_be_bytes());
                for (mid, price) in &s.prices {
                    buf.extend_from_slice(&mid.to_be_bytes());
                    buf.extend_from_slice(&price.raw().to_be_bytes());
                }
                buf.extend_from_slice(&s.timestamp.to_be_bytes());
            }
            NativeAction::RegisterValidator { pubkey, commission } => {
                buf.push(14);
                buf.extend_from_slice(&pubkey.0);
                buf.extend_from_slice(&commission.to_be_bytes());
            }
            NativeAction::UpdateCommission { new_rate } => {
                buf.push(15);
                buf.extend_from_slice(&new_rate.to_be_bytes());
            }
            NativeAction::JailVote { target } => {
                buf.push(16);
                buf.extend_from_slice(target.as_slice());
            }
            NativeAction::UnjailSelf => {
                buf.push(17);
            }
            NativeAction::RotateValidatorKey { new_pubkey } => {
                buf.push(18);
                buf.extend_from_slice(&new_pubkey.0);
            }
            NativeAction::UpdateMarketParams { market_id, params } => {
                buf.push(19);
                buf.extend_from_slice(&market_id.to_be_bytes());
                buf.extend_from_slice(&params.tick_size.raw().to_be_bytes());
                buf.extend_from_slice(&params.lot_size.raw().to_be_bytes());
                buf.extend_from_slice(&params.max_leverage.to_be_bytes());
                buf.extend_from_slice(&params.maintenance_margin_bps.to_be_bytes());
                buf.extend_from_slice(&params.max_funding_rate_bps.to_be_bytes());
            }
            NativeAction::ListMarket(l) => {
                buf.push(20);
                buf.extend_from_slice(&(l.base_asset.len() as u32).to_be_bytes());
                buf.extend_from_slice(l.base_asset.as_bytes());
                buf.extend_from_slice(&(l.quote_asset.len() as u32).to_be_bytes());
                buf.extend_from_slice(l.quote_asset.as_bytes());
                buf.extend_from_slice(&l.tick_size.raw().to_be_bytes());
                buf.extend_from_slice(&l.lot_size.raw().to_be_bytes());
                buf.extend_from_slice(&l.max_leverage.to_be_bytes());
                buf.extend_from_slice(&l.maintenance_margin_bps.to_be_bytes());
            }
            NativeAction::DelistMarket { market_id } => {
                buf.push(21);
                buf.extend_from_slice(&market_id.to_be_bytes());
            }
            NativeAction::TopUpSelfStake { amount } => {
                buf.push(22);
                buf.extend_from_slice(&amount.to_be_bytes::<32>());
            }
            NativeAction::CreateSession {
                session_pubkey,
                expiry,
                scope,
            } => {
                buf.push(23);
                buf.extend_from_slice(session_pubkey);
                buf.extend_from_slice(&expiry.to_be_bytes());
                buf.push(match scope {
                    SessionScope::Trading => 0,
                    SessionScope::TransfersOnly => 1,
                    SessionScope::Full => 2,
                });
            }
            NativeAction::RevokeSession { session_pubkey } => {
                buf.push(24);
                buf.extend_from_slice(session_pubkey);
            }
        }
        buf
    }
}

/// Signed native action envelope. Submitted via torus_submitNativeAction RPC.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SignedNativeAction {
    pub action: NativeAction,
    /// Millisecond timestamp nonce (not sequential — allows out-of-order processing).
    pub nonce: u64,
    /// EIP-712 ECDSA or ed25519 session key signature.
    pub signature: ActionSignature,
}

/// Deterministic content address of a signed native action, used for dedup, DA
/// keying, and compact-block references.
///
/// Preimage = `NativeAction::canonical_bytes()` || `nonce` (be) || signature.
///
/// F-2 (consensus format change, fresh-genesis only): the signature bytes are
/// bound into the preimage with a per-variant domain-separation tag. Before this,
/// the hash omitted the signature, so two actions with an identical payload+nonce
/// but different signatures collided under the same content address. A Byzantine
/// peer answering a DA heal-pull could serve the committed payload+nonce with a
/// swapped/invalid signature that still hash-matched the quorum-agreed reference,
/// diverging the healing node's native state (missed slash or wrong sender). By
/// committing to the full signed bytes, `compute_action_hash(swapped_body)` no
/// longer equals the referenced hash, so the substituted body is rejected on
/// reconstruction. This RE-KEYS every stored/dedup hash — all validators must run
/// this build and relaunch on fresh genesis with wiped data dirs.
pub fn compute_action_hash(action: &SignedNativeAction) -> B256 {
    let mut data = action.action.canonical_bytes();
    data.extend_from_slice(&action.nonce.to_be_bytes());
    // Domain-separated signature commitment. The leading tag byte keeps the two
    // variants' encodings disjoint so an EIP-712 action can never share a preimage
    // with a session action.
    match &action.signature {
        ActionSignature::Eip712(sig) => {
            data.push(0x00);
            data.push(sig.v);
            data.extend_from_slice(&sig.r);
            data.extend_from_slice(&sig.s);
        }
        ActionSignature::Session {
            session_pubkey,
            sig,
        } => {
            data.push(0x01);
            data.extend_from_slice(session_pubkey);
            data.extend_from_slice(&sig.0);
        }
    }
    alloy_primitives::keccak256(&data)
}

/// Signature-committing key for the exec trust-cache, or `None` when the action
/// is not eligible for caching.
///
/// This is kept DISTINCT from `compute_action_hash` (which, since F-2, also
/// commits to the full signature) because the trust-cache must additionally
/// EXCLUDE session actions: this key returns `None` for them so they are always
/// fully re-verified. For EIP-712 actions it commits to the full signature, so
/// any signature difference yields a different key => cache MISS => full recover +
/// slash (safe); it never lets a warm-cache node reuse a sender a cold-cache node
/// would not recover (a consensus FORK / missed slash).
///
/// Only EIP-712 actions are cacheable: their sender is a pure, stateless function
/// of the signature (`ecrecover`). A session action resolves through exec-time
/// state (session existence, expiry, scope), so a cached hit could diverge from a
/// fresh resolve — `None` is returned for those and they are always fully
/// re-verified. Used IDENTICALLY at populate (mempool) and read (`batch_verify_native_actions`).
pub fn verified_cache_key(action: &SignedNativeAction) -> Option<B256> {
    let ActionSignature::Eip712(sig) = &action.signature else {
        return None;
    };
    let mut data = action.action.canonical_bytes();
    data.extend_from_slice(&action.nonce.to_be_bytes());
    data.push(sig.v);
    data.extend_from_slice(&sig.r);
    data.extend_from_slice(&sig.s);
    Some(alloy_primitives::keccak256(&data))
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
    StopMarket {
        trigger: FixedPoint,
    },
    StopLimit {
        trigger: FixedPoint,
        limit: FixedPoint,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TimeInForce {
    /// Good-til-cancelled.
    GTC,
    /// Immediate-or-cancel.
    IOC,
    /// Fill-or-kill.
    FOK,
    /// Post-only: rejected if would immediately match.
    PostOnly,
}

/// Order side.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Side {
    Buy,
    Sell,
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
    /// Whitelist a candidate address for validator registration (Phase 3: 3.2.1).
    ValidatorRegistration {
        candidate: Address,
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
    /// Treasury address for fee distribution and governance withdrawals.
    #[serde(default)]
    pub treasury_address: Address,
    /// Dev pool address for fee distribution.
    #[serde(default)]
    pub dev_pool_address: Address,
    /// Consensus view timeout in milliseconds (from genesis).
    #[serde(default = "default_timeout_base_ms")]
    pub timeout_base_ms: u64,
    /// Base of the multiplicative view-timeout backoff applied while views
    /// outrun the QC frontier (Task A). Consensus-liveness critical — every
    /// validator of a chain must use the same value (genesis-sourced).
    #[serde(default = "default_backoff_factor")]
    pub backoff_factor: u32,
    /// Exponent cap of the view-timeout backoff; 0 disables backoff entirely
    /// (multiplier constantly 1 — runtime kill switch). Fleet-wide, like
    /// `backoff_factor`.
    #[serde(default = "default_backoff_cap")]
    pub backoff_cap: u32,
    /// MonadBFT B3: weight leader selection by observed leader reputation.
    /// Consensus-critical — every validator of a chain must use the same value.
    /// Off by default: locally-observed reputation diverges across replicas
    /// and divergent leader schedules halt finalization.
    #[serde(default)]
    pub reputation_leader_selection: bool,
    /// Node-local: consult the exec trust-cache to skip redundant secp256k1
    /// recovery on cache HITs at execution. Deterministic (a HIT returns the same
    /// sender a fresh recover would), so it never affects consensus or state; off
    /// by default, enabled per-node via `--exec-trust-cache` for A/B measurement
    /// and safe rollback.
    #[serde(default)]
    pub exec_trust_cache: bool,
}

fn default_timeout_base_ms() -> u64 {
    500
}

fn default_backoff_factor() -> u32 {
    2
}

fn default_backoff_cap() -> u32 {
    8
}

impl ChainConfig {
    /// FIX CONS-PF-04: Validate chain configuration parameters.
    pub fn validate(&self) -> Result<(), String> {
        if self.epoch_length == 0 {
            return Err("epoch_length must be >= 1".to_string());
        }
        if self.max_validators == 0 {
            return Err("max_validators must be >= 1".to_string());
        }
        Ok(())
    }
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

    // ---- Checked arithmetic tests (ECON-PF-01, ECON-PF-02) ----

    #[test]
    fn checked_add_overflow_returns_error() {
        let result = FixedPoint::MAX.checked_add(FixedPoint::from_raw(1));
        assert_eq!(result, Err(ArithmeticError::Overflow));
    }

    #[test]
    fn checked_sub_underflow_returns_error() {
        let result = FixedPoint::MIN.checked_sub(FixedPoint::from_raw(1));
        assert_eq!(result, Err(ArithmeticError::Underflow));
    }

    #[test]
    fn checked_div_by_zero_returns_error() {
        let result = FixedPoint::ONE.checked_div(FixedPoint::ZERO);
        assert_eq!(result, Err(ArithmeticError::DivisionByZero));
    }

    #[test]
    fn checked_arithmetic_normal_operations() {
        let one = FixedPoint::ONE;
        let two = FixedPoint::from_raw(2 * FixedPoint::SCALE);

        assert_eq!(one.checked_add(one).unwrap(), two);
        assert_eq!(two.checked_sub(one).unwrap(), one);
        assert_eq!(one.checked_mul(two).unwrap(), two);
        assert_eq!(two.checked_div(two).unwrap(), one);
    }

    #[test]
    #[should_panic(expected = "FixedPoint addition overflow")]
    fn add_operator_panics_on_overflow() {
        let _ = FixedPoint::MAX + FixedPoint::from_raw(1);
    }

    #[test]
    #[should_panic(expected = "FixedPoint subtraction underflow")]
    fn sub_operator_panics_on_underflow() {
        let _ = FixedPoint::MIN - FixedPoint::from_raw(1);
    }

    #[test]
    #[should_panic(expected = "FixedPoint division error")]
    fn div_operator_panics_on_zero() {
        let _ = FixedPoint::ONE / FixedPoint::ZERO;
    }

    // ---- PlaceOrderBatch (Phase B, Task B1) ----

    fn sample_order(market_id: MarketId, qty_raw: i128) -> PlaceOrderParams {
        PlaceOrderParams {
            market_id,
            is_buy: true,
            price: FixedPoint::from_raw(100 * FixedPoint::SCALE),
            quantity: FixedPoint::from_raw(qty_raw),
            order_type: OrderType::Limit,
            time_in_force: TimeInForce::GTC,
            reduce_only: false,
            client_order_id: None,
        }
    }

    #[test]
    fn place_order_batch_canonical_bytes_deterministic_and_distinct() {
        let p1 = sample_order(1, 10 * FixedPoint::SCALE);
        let p2 = sample_order(2, 20 * FixedPoint::SCALE);
        let batch = NativeAction::PlaceOrderBatch(vec![p1.clone(), p2.clone()]);

        // Deterministic: same batch encodes identically every call.
        assert_eq!(batch.canonical_bytes(), batch.canonical_bytes());

        // Unique tag byte (next free after 0..=24).
        assert_eq!(batch.canonical_bytes()[0], 25);

        // Order is significant — reordering changes the bytes.
        let reversed = NativeAction::PlaceOrderBatch(vec![p2.clone(), p1.clone()]);
        assert_ne!(batch.canonical_bytes(), reversed.canonical_bytes());

        // A batch is never byte-equal to a single PlaceOrder (tag + count framing).
        let single = NativeAction::PlaceOrder(p1.clone());
        assert_ne!(batch.canonical_bytes(), single.canonical_bytes());

        // A batch's per-order encoding matches the single-order encoding (sans tag),
        // proving the shared encoder is used (no divergence between paths).
        let single_body = &single.canonical_bytes()[1..];
        let batch_bytes = batch.canonical_bytes();
        // batch layout: [tag=25][u32 count][order1 body][order2 body]
        assert_eq!(&batch_bytes[1..5], &2u32.to_be_bytes());
        assert_eq!(&batch_bytes[5..5 + single_body.len()], single_body);

        // Empty batch is encodable and distinct from a populated one.
        let empty = NativeAction::PlaceOrderBatch(vec![]);
        assert_eq!(&empty.canonical_bytes(), &[25u8, 0, 0, 0, 0]);
        assert_ne!(empty.canonical_bytes(), batch.canonical_bytes());

        // Serde round-trip (bincode is the block wire format) preserves identity.
        let encoded = bincode::serialize(&batch).unwrap();
        let back: NativeAction = bincode::deserialize(&encoded).unwrap();
        assert_eq!(back.canonical_bytes(), batch.canonical_bytes());
    }

    #[test]
    fn header_with_attestation_serializes() {
        let mut header = TorusBlockHeader {
            height: 1,
            parent_hash: B256::ZERO,
            timestamp: 1000,
            proposer: Address::ZERO,
            state_root: B256::ZERO,
            receipts_root: B256::ZERO,
            logs_bloom: alloy_primitives::Bloom::ZERO,
            evm_gas_used: 0,
            evm_fee_revenue: 0,
            evm_gas_limit: 30_000_000,
            native_action_count: 0,
            evm_tx_count: 0,
            base_fee_per_gas: 1_000_000_000,
            epoch: 0,
            validator_set_hash: B256::ZERO,
            sig_attestation: [0u8; 64],
        };
        header.sig_attestation = [42u8; 64];
        let json = serde_json::to_vec(&header).unwrap();
        let decoded: TorusBlockHeader = serde_json::from_slice(&json).unwrap();
        assert_eq!(decoded.sig_attestation, [42u8; 64]);
    }

    #[test]
    fn header_without_attestation_defaults_to_zero() {
        let json = br#"{"height":1,"timestamp":1000,"proposer":"0x0000000000000000000000000000000000000000","state_root":"0x0000000000000000000000000000000000000000000000000000000000000000","receipts_root":"0x0000000000000000000000000000000000000000000000000000000000000000","logs_bloom":"0x00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000","evm_gas_used":0,"evm_fee_revenue":0,"evm_gas_limit":30000000,"native_action_count":0,"evm_tx_count":0,"base_fee_per_gas":1000000000,"epoch":0,"validator_set_hash":"0x0000000000000000000000000000000000000000000000000000000000000000"}"#;
        let decoded: TorusBlockHeader = serde_json::from_slice(json).unwrap();
        assert_eq!(decoded.sig_attestation, [0u8; 64]);
    }

    fn test_header() -> TorusBlockHeader {
        TorusBlockHeader {
            height: 1,
            parent_hash: B256::ZERO,
            timestamp: 1000,
            proposer: Address::ZERO,
            state_root: B256::ZERO,
            receipts_root: B256::ZERO,
            logs_bloom: Bloom::ZERO,
            evm_gas_used: 0,
            evm_fee_revenue: 0,
            evm_gas_limit: 30_000_000,
            native_action_count: 0,
            evm_tx_count: 0,
            base_fee_per_gas: 1_000_000_000,
            epoch: 0,
            validator_set_hash: B256::ZERO,
            sig_attestation: [0u8; 64],
        }
    }

    #[test]
    fn compact_block_roundtrip_bincode() {
        let action = SignedNativeAction {
            action: NativeAction::ClaimRewards,
            nonce: 100,
            signature: ActionSignature::Eip712(Signature {
                v: 27,
                r: [0u8; 32],
                s: [0u8; 32],
            }),
        };
        let block = TorusBlock {
            header: test_header(),
            native_actions: vec![action.clone(); 10],
            evm_transactions: vec![vec![1, 2, 3]],
            core_writer_actions: vec![],
        };
        let compact = CompactBlock::from_block(&block);
        assert_eq!(compact.native_action_hashes.len(), 10);
        assert_eq!(
            compact.native_action_hashes[0],
            compute_action_hash(&action)
        );

        let encoded = bincode::serialize(&compact).unwrap();
        let decoded: CompactBlock = bincode::deserialize(&encoded).unwrap();
        assert_eq!(decoded.native_action_hashes.len(), 10);
        assert_eq!(
            decoded.native_action_hashes[0],
            compact.native_action_hashes[0]
        );
        assert_eq!(decoded.evm_transactions, compact.evm_transactions);
    }

    #[test]
    fn compact_block_smaller_than_full_block() {
        let actions: Vec<SignedNativeAction> = (0..100)
            .map(|i| SignedNativeAction {
                action: NativeAction::CancelOrder { order_id: i },
                nonce: i as u64,
                signature: ActionSignature::Eip712(Signature {
                    v: 27,
                    r: [0u8; 32],
                    s: [0u8; 32],
                }),
            })
            .collect();
        let block = TorusBlock {
            header: test_header(),
            native_actions: actions,
            evm_transactions: vec![],
            core_writer_actions: vec![],
        };
        let compact = CompactBlock::from_block(&block);

        let full_size = bincode::serialize(&block).unwrap().len();
        let compact_size = bincode::serialize(&compact).unwrap().len();
        assert!(
            compact_size < full_size,
            "compact {compact_size} should be smaller than full {full_size}"
        );
    }

    #[test]
    fn compact_block_empty_actions() {
        let block = TorusBlock {
            header: test_header(),
            native_actions: vec![],
            evm_transactions: vec![],
            core_writer_actions: vec![],
        };
        let compact = CompactBlock::from_block(&block);
        assert!(compact.native_action_hashes.is_empty());
    }

    #[test]
    fn compute_action_hash_deterministic() {
        let action = SignedNativeAction {
            action: NativeAction::ClaimRewards,
            nonce: 12345,
            signature: ActionSignature::Eip712(Signature {
                v: 27,
                r: [0u8; 32],
                s: [0u8; 32],
            }),
        };
        let h1 = compute_action_hash(&action);
        let h2 = compute_action_hash(&action);
        assert_eq!(h1, h2);
        assert_ne!(h1, B256::ZERO);
    }

    #[test]
    fn compute_action_hash_differs_by_nonce() {
        let a1 = SignedNativeAction {
            action: NativeAction::ClaimRewards,
            nonce: 1,
            signature: ActionSignature::Eip712(Signature {
                v: 27,
                r: [0u8; 32],
                s: [0u8; 32],
            }),
        };
        let a2 = SignedNativeAction {
            action: NativeAction::ClaimRewards,
            nonce: 2,
            signature: ActionSignature::Eip712(Signature {
                v: 27,
                r: [0u8; 32],
                s: [0u8; 32],
            }),
        };
        assert_ne!(compute_action_hash(&a1), compute_action_hash(&a2));
    }

    #[test]
    fn compute_action_hash_differs_by_action() {
        let a1 = SignedNativeAction {
            action: NativeAction::ClaimRewards,
            nonce: 1,
            signature: ActionSignature::Eip712(Signature {
                v: 27,
                r: [0u8; 32],
                s: [0u8; 32],
            }),
        };
        let a2 = SignedNativeAction {
            action: NativeAction::CancelOrder { order_id: 42 },
            nonce: 1,
            signature: ActionSignature::Eip712(Signature {
                v: 27,
                r: [0u8; 32],
                s: [0u8; 32],
            }),
        };
        assert_ne!(compute_action_hash(&a1), compute_action_hash(&a2));
    }

    /// F-2 (consensus format change): the content address MUST commit to the
    /// signature. Two actions with an identical payload+nonce but different
    /// EIP-712 signatures must hash DIFFERENTLY, otherwise a Byzantine peer can
    /// substitute the signature bytes of a DA-fetched body under the same hash.
    #[test]
    fn compute_action_hash_differs_by_eip712_signature() {
        let base = SignedNativeAction {
            action: NativeAction::ClaimRewards,
            nonce: 7,
            signature: ActionSignature::Eip712(Signature {
                v: 27,
                r: [1u8; 32],
                s: [2u8; 32],
            }),
        };
        // Same payload+nonce, one signature byte flipped (r differs).
        let mut swapped_r = base.clone();
        swapped_r.signature = ActionSignature::Eip712(Signature {
            v: 27,
            r: [0xAAu8; 32],
            s: [2u8; 32],
        });
        // s differs.
        let mut swapped_s = base.clone();
        swapped_s.signature = ActionSignature::Eip712(Signature {
            v: 27,
            r: [1u8; 32],
            s: [0xBBu8; 32],
        });
        // recovery id (v) differs.
        let mut swapped_v = base.clone();
        swapped_v.signature = ActionSignature::Eip712(Signature {
            v: 28,
            r: [1u8; 32],
            s: [2u8; 32],
        });

        let h = compute_action_hash(&base);
        assert_ne!(h, compute_action_hash(&swapped_r), "r must be bound");
        assert_ne!(h, compute_action_hash(&swapped_s), "s must be bound");
        assert_ne!(h, compute_action_hash(&swapped_v), "v must be bound");
    }

    /// F-2: session (ed25519) signatures must also be bound into the hash.
    #[test]
    fn compute_action_hash_differs_by_session_signature() {
        let base = SignedNativeAction {
            action: NativeAction::ClaimRewards,
            nonce: 9,
            signature: ActionSignature::Session {
                session_pubkey: [3u8; 32],
                sig: Ed25519Sig([4u8; 64]),
            },
        };
        // Same payload+nonce+pubkey, different signature bytes.
        let mut swapped_sig = base.clone();
        swapped_sig.signature = ActionSignature::Session {
            session_pubkey: [3u8; 32],
            sig: Ed25519Sig([5u8; 64]),
        };
        // Same payload+nonce+sig, different session pubkey (different signer).
        let mut swapped_pubkey = base.clone();
        swapped_pubkey.signature = ActionSignature::Session {
            session_pubkey: [6u8; 32],
            sig: Ed25519Sig([4u8; 64]),
        };
        let h = compute_action_hash(&base);
        assert_ne!(h, compute_action_hash(&swapped_sig), "session sig must be bound");
        assert_ne!(
            h,
            compute_action_hash(&swapped_pubkey),
            "session pubkey must be bound"
        );
    }

    /// F-2: domain separation between signature variants — an EIP-712 action and
    /// a session action must never share a content address even if their bound
    /// bytes would otherwise line up.
    #[test]
    fn compute_action_hash_domain_separates_signature_variants() {
        let eip712 = SignedNativeAction {
            action: NativeAction::ClaimRewards,
            nonce: 11,
            signature: ActionSignature::Eip712(Signature {
                v: 0,
                r: [0u8; 32],
                s: [0u8; 32],
            }),
        };
        let session = SignedNativeAction {
            action: NativeAction::ClaimRewards,
            nonce: 11,
            signature: ActionSignature::Session {
                session_pubkey: [0u8; 32],
                sig: Ed25519Sig([0u8; 64]),
            },
        };
        assert_ne!(
            compute_action_hash(&eip712),
            compute_action_hash(&session),
            "signature variants must be domain-separated"
        );
    }

    #[test]
    fn attestation_excluded_from_canonical_bytes() {
        let h1 = TorusBlockHeader {
            height: 1,
            parent_hash: B256::ZERO,
            timestamp: 1000,
            proposer: Address::ZERO,
            state_root: B256::ZERO,
            receipts_root: B256::ZERO,
            logs_bloom: alloy_primitives::Bloom::ZERO,
            evm_gas_used: 0,
            evm_fee_revenue: 0,
            evm_gas_limit: 30_000_000,
            native_action_count: 0,
            evm_tx_count: 0,
            base_fee_per_gas: 1_000_000_000,
            epoch: 0,
            validator_set_hash: B256::ZERO,
            sig_attestation: [0u8; 64],
        };
        let mut h2 = h1.clone();
        h2.sig_attestation = [0xff; 64];
        assert_eq!(h1.canonical_header_bytes(), h2.canonical_header_bytes());
    }

    /// RED before this fix: `canonical_header_bytes` did not include any parent
    /// linkage, so two headers identical except for `parent_hash` produced the
    /// SAME preimage (and thus the same block hash). This is the root cause of
    /// the devnet fork-detection unsoundness (two different height-110 blocks
    /// yielded byte-identical height-111 hashes). It must now hold that differing
    /// only in `parent_hash` yields DIFFERENT canonical bytes / block hashes.
    #[test]
    fn parent_hash_included_in_canonical_bytes() {
        let h1 = test_header();
        let mut h2 = h1.clone();
        h2.parent_hash = B256::repeat_byte(0xAB);
        assert_ne!(
            h1.canonical_header_bytes(),
            h2.canonical_header_bytes(),
            "headers differing only in parent_hash must hash differently"
        );
        assert_ne!(
            alloy_primitives::keccak256(h1.canonical_header_bytes()),
            alloy_primitives::keccak256(h2.canonical_header_bytes()),
        );
    }

    /// Genesis zero-parent case: `genesis_parent_header`-style header (height 0)
    /// carries `parent_hash == B256::ZERO`, and its canonical bytes commit to
    /// that zero parent so the height-1 block can anchor to it deterministically.
    #[test]
    fn genesis_header_has_zero_parent_hash() {
        let mut genesis = test_header();
        genesis.height = 0;
        genesis.parent_hash = B256::ZERO;
        // The canonical bytes begin with height(8) || parent_hash(32); assert the
        // parent_hash region is exactly zero for genesis.
        let bytes = genesis.canonical_header_bytes();
        assert_eq!(&bytes[8..40], &[0u8; 32], "genesis parent_hash must be zero");
    }

    /// `state_root` is intentionally excluded from the block identity (dead field
    /// under commit-then-execute). Two headers differing ONLY in `state_root`
    /// must therefore produce identical canonical bytes — pinning that the
    /// perpetual-0x0 field can never silently fork or alter block identity.
    #[test]
    fn state_root_excluded_from_canonical_bytes() {
        let h1 = test_header();
        let mut h2 = h1.clone();
        h2.state_root = B256::repeat_byte(0x11);
        assert_eq!(
            h1.canonical_header_bytes(),
            h2.canonical_header_bytes(),
            "state_root must NOT affect block identity (excluded from preimage)"
        );
    }
}
