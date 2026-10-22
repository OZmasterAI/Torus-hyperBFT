//! Shared types for the Torus-hyperBFT blockchain.
//!
//! Leaf crate with no internal dependencies. All other crates depend on this.

pub use alloy_primitives::{Address, Bloom, Bytes, B256, U256};

use serde::{Deserialize, Serialize};

pub mod book_mode;
pub mod eip1559;
pub mod eip712;
pub mod features;

pub use features::{ActivationSchedule, Feature, KNOWN_FEATURES};

// ============================================================================
// Identifiers
// ============================================================================

/// Unique order identifier (128-bit for global uniqueness).
pub type OrderId = u128;

/// Market identifier.
pub type MarketId = u64;

/// s114: a client order id (cloid), 128-bit as in Hyperliquid. Unique among
/// one trader's open orders (resting and pending stops) across all markets.
pub type Cloid = u128;

/// s114: serde form of a [`Cloid`]. JSON (and every human-readable format)
/// writes `0x` + 32 lowercase hex digits, the Hyperliquid form, so a cloid
/// above 2^64 survives `serde_json::Value` and JavaScript clients; it reads
/// that form, any `0x` hex of 1-32 digits, or a non-negative integer.
/// Binary formats (bincode wire / DA) keep the plain 16-byte integer.
pub mod cloid_serde {
    use super::Cloid;
    use serde::{de, Deserialize, Deserializer, Serialize, Serializer};

    /// The JSON text of a cloid: `0x` + 32 lowercase hex digits.
    pub fn to_hex(cloid: Cloid) -> String {
        format!("0x{cloid:032x}")
    }

    /// Parse `0x` + 1-32 hex digits.
    pub fn from_hex(s: &str) -> Result<Cloid, String> {
        let digits = s
            .strip_prefix("0x")
            .or_else(|| s.strip_prefix("0X"))
            .ok_or_else(|| format!("cloid {s:?}: expected 0x + hex"))?;
        if digits.is_empty() || digits.len() > 32 {
            return Err(format!("cloid {s:?}: expected 1-32 hex digits"));
        }
        Cloid::from_str_radix(digits, 16).map_err(|e| format!("cloid {s:?}: {e}"))
    }

    struct Repr(Cloid);

    impl Serialize for Repr {
        fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
            if s.is_human_readable() {
                s.serialize_str(&to_hex(self.0))
            } else {
                s.serialize_u128(self.0)
            }
        }
    }

    struct HumanVisitor;

    impl de::Visitor<'_> for HumanVisitor {
        type Value = Cloid;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("a cloid: 0x + up to 32 hex digits, or a non-negative integer")
        }
        fn visit_str<E: de::Error>(self, v: &str) -> Result<Cloid, E> {
            from_hex(v).map_err(E::custom)
        }
        fn visit_u64<E: de::Error>(self, v: u64) -> Result<Cloid, E> {
            Ok(v.into())
        }
        fn visit_u128<E: de::Error>(self, v: u128) -> Result<Cloid, E> {
            Ok(v)
        }
        fn visit_i64<E: de::Error>(self, v: i64) -> Result<Cloid, E> {
            Cloid::try_from(v).map_err(|_| E::custom("cloid must not be negative"))
        }
    }

    impl<'de> Deserialize<'de> for Repr {
        fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
            if d.is_human_readable() {
                d.deserialize_any(HumanVisitor).map(Repr)
            } else {
                Cloid::deserialize(d).map(Repr)
            }
        }
    }

    pub fn serialize<S: Serializer>(cloid: &Cloid, s: S) -> Result<S::Ok, S::Error> {
        Repr(*cloid).serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Cloid, D::Error> {
        Repr::deserialize(d).map(|r| r.0)
    }

    /// `Option<Cloid>`: `null` / absent is `None`.
    pub mod option {
        use super::{Cloid, Repr};
        use serde::{Deserialize, Deserializer, Serialize, Serializer};

        pub fn serialize<S: Serializer>(cloid: &Option<Cloid>, s: S) -> Result<S::Ok, S::Error> {
            cloid.map(Repr).serialize(s)
        }

        pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Cloid>, D::Error> {
            Ok(Option::<Repr>::deserialize(d)?.map(|r| r.0))
        }
    }
}

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
    ///
    /// Item 6 P2: when the raw product fits i128 it is divided in i128 —
    /// the same truncating quotient (|p / SCALE| < |p|, so it fits), bit for
    /// bit; otherwise the i256 path decides (a quotient that fits, or the
    /// overflow error).
    pub fn checked_mul(self, rhs: Self) -> Result<Self, ArithmeticError> {
        if let Some(p) = self.0.checked_mul(rhs.0) {
            return Ok(FixedPoint(p / Self::SCALE));
        }
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
        // Item 6 P2: i128 when `self × SCALE` fits (same truncating
        // quotient; `checked_div` leaves MIN / -1 to the i256 path).
        if let Some(q) = self.0.checked_mul(Self::SCALE).and_then(|n| n.checked_div(rhs.0)) {
            return Ok(FixedPoint(q));
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

/// Parse a decimal string: `"1.5"`, `".5"`, `"100"`, `"-1.5"`, `"0.00000001"`.
/// Up to 8 fractional digits; the inverse of `Display`.
/// Rejects empty strings, non-digits (including hex and `+`), more than one
/// `.` and more than 8 decimal places.
impl std::str::FromStr for FixedPoint {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.is_empty() {
            return Err("empty decimal string".into());
        }
        let (negative, body) = match s.strip_prefix('-') {
            Some(rest) => (true, rest),
            None => (false, s),
        };
        let (whole_str, frac_str) = body.split_once('.').unwrap_or((body, ""));
        if whole_str.is_empty() && frac_str.is_empty() {
            return Err("invalid decimal: missing digits".into());
        }
        if !whole_str
            .bytes()
            .chain(frac_str.bytes())
            .all(|b| b.is_ascii_digit())
        {
            return Err(format!("invalid decimal: {s:?}"));
        }
        if frac_str.len() > Self::DECIMALS as usize {
            return Err(format!("too many decimal places (max {})", Self::DECIMALS));
        }
        let whole: i128 = if whole_str.is_empty() {
            0
        } else {
            whole_str
                .parse()
                .map_err(|e| format!("invalid whole part: {e}"))?
        };
        let frac: i128 = if frac_str.is_empty() {
            0
        } else {
            format!("{frac_str:0<8}")
                .parse()
                .map_err(|e| format!("invalid fraction: {e}"))?
        };
        let raw = whole
            .checked_mul(Self::SCALE)
            .and_then(|w| w.checked_add(frac))
            .ok_or("decimal overflow")?;
        Ok(Self(if negative { -raw } else { raw }))
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

/// Review M3 (s517): proof that whoever registers a hot oracle signer holds
/// its key — the signer's EIP-712 signature over
/// `OracleSignerProof(address validator,uint64 chainId,uint64 nonce)`
/// (`eip712::sign_oracle_signer_proof`). The validator field stops another
/// validator replaying the proof; chain id (struct + domain) binds the chain;
/// `nonce` makes each proof distinct (the wallet uses the action's nonce).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OracleSignerProof {
    pub nonce: u64,
    pub signature: Signature,
}

/// Ed25519 public key (32 bytes) for validator consensus signing.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PublicKey(pub [u8; 32]);

/// Scope of actions a session key is authorized to perform.
///
/// Session keys only ever trade, like Hyperliquid agent wallets: every other
/// action (transfers, withdraw, staking, governance, validator, oracle,
/// session management) needs the owner's EIP-712 signature
/// ([`crate::eip712::requires_eip712`]). The variants and their encoding are
/// unchanged so stored sessions and old `CreateSession` actions still decode.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionScope {
    /// The trading actions ([`NativeAction::is_trading`]).
    Trading,
    /// Nothing: transfers need the owner key. Kept for encoding compatibility.
    TransfersOnly,
    /// Same as `Trading`. Kept for encoding compatibility.
    Full,
}

impl SessionScope {
    pub fn allows(&self, action: &NativeAction) -> bool {
        // LOCKSTEP DEPLOY: scope is enforced on the CONSENSUS path too (exec
        // batch verify). A node with different rules resolves different
        // senders and diverges, so the whole fleet must run the same rules.
        match self {
            SessionScope::Trading | SessionScope::Full => action.is_trading(),
            SessionScope::TransfersOnly => false,
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
    /// `keccak256(genesis_parent_header(..).canonical_header_bytes())`.
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
    /// EVM fee revenue in wei handed to the native fee distributor: the
    /// base-fee part only, `sum(gas_used * min(effective_gas_price,
    /// base_fee_per_gas))` over this block's receipts
    /// (`torus_bridge::proposer::compute_fee_revenue`). The tip part
    /// (`effective_gas_price - base_fee_per_gas`) is not included: revm already
    /// paid it to the block beneficiary (the proposer).
    pub evm_fee_revenue: u128,
    /// Gas limit for EVM transactions in this block.
    pub evm_gas_limit: u64,
    /// Item 7 step 1: N. EVM txs only in blocks with `height % N == 0`. Equals the parent's,
    /// except at an epoch boundary, where the epoch plan may change it.
    pub evm_block_interval: u64,
    /// Item 7 step 2: M. A block with M > 0 and `height % M == 0` is a big EVM block; 0 = off.
    /// Equals the parent's, except at an epoch boundary (the epoch plan).
    pub evm_big_block_interval: u64,
    /// Item 7 step 2: the big lane's gas limit, added to `evm_gas_limit` at a big height.
    pub evm_big_gas_limit: u64,
    /// Native action count.
    pub native_action_count: u32,
    /// EVM transaction count.
    pub evm_tx_count: u32,
    /// Base fee per gas (EIP-1559).
    pub base_fee_per_gas: u64,
    /// Item 7 step 2 (F3): the big lane's base fee. Changes only at an epoch boundary.
    pub big_base_fee_per_gas: u64,
    /// Epoch number (validator set epoch).
    pub epoch: u64,
    /// Validator set hash for this epoch.
    pub validator_set_hash: B256,
    /// P0.0: the bitmap of the [`Feature`]s active at this height
    /// (`ActivationSchedule::active_at`; bit = feature id). Hashed (appended to
    /// the canonical bytes when non-zero, so a header without bits keeps its
    /// pre-P0.0 hash) and checked against each voter's own schedule
    /// (`check_parent_link`); execution reads the bits from here.
    #[serde(default)]
    pub features: u128,
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
        buf.extend_from_slice(&self.evm_block_interval.to_be_bytes());
        buf.extend_from_slice(&self.evm_big_block_interval.to_be_bytes());
        buf.extend_from_slice(&self.evm_big_gas_limit.to_be_bytes());
        buf.extend_from_slice(&self.native_action_count.to_be_bytes());
        buf.extend_from_slice(&self.evm_tx_count.to_be_bytes());
        buf.extend_from_slice(&self.base_fee_per_gas.to_be_bytes());
        buf.extend_from_slice(&self.big_base_fee_per_gas.to_be_bytes());
        buf.extend_from_slice(&self.epoch.to_be_bytes());
        buf.extend_from_slice(self.validator_set_hash.as_slice());
        // P0.0: the feature bits are agreed per block (`check_parent_link`).
        // Appended only when a feature is active, so a header without bits keeps
        // its pre-P0.0 bytes and block hash. Every other field is fixed-width:
        // the length tells the two encodings apart.
        if self.features != 0 {
            buf.extend_from_slice(&self.features.to_be_bytes());
        }
        buf
    }

    /// Item 7 step 2: this block is a big EVM block.
    pub fn is_big_evm_height(&self) -> bool {
        is_big_evm_height(self.height, self.evm_big_block_interval)
    }

    /// Item 7 step 2: EVM txs may ride this block (small cadence or a big height).
    pub fn is_evm_height(&self) -> bool {
        is_evm_height(
            self.height,
            self.evm_block_interval,
            self.evm_big_block_interval,
        )
    }

    /// Item 7 step 2: the EVM gas this block may declare and execute: the small limit, plus the
    /// big limit at a big height.
    pub fn evm_block_gas_cap(&self) -> u64 {
        if self.is_big_evm_height() {
            self.evm_gas_limit.saturating_add(self.evm_big_gas_limit)
        } else {
            self.evm_gas_limit
        }
    }

    /// Item 7 step 2: most raw EVM tx bytes this block may carry (vote rule).
    pub fn evm_block_byte_cap(&self) -> usize {
        if self.is_big_evm_height() {
            MAX_EVM_BIG_BLOCK_TX_BYTES
        } else {
            MAX_EVM_BLOCK_TX_BYTES
        }
    }
}

/// Item 7 step 2: the six EVM header values the boundary rule pins: the parent's, at an epoch
/// boundary the plan's (`torus_economics::PlanEvm::resolve`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EvmHeaderValues {
    pub gas_limit: u64,
    pub block_interval: u64,
    pub big_block_interval: u64,
    pub big_gas_limit: u64,
    pub base_fee: u64,
    pub big_base_fee: u64,
}

impl EvmHeaderValues {
    pub fn of(h: &TorusBlockHeader) -> Self {
        Self {
            gas_limit: h.evm_gas_limit,
            block_interval: h.evm_block_interval,
            big_block_interval: h.evm_big_block_interval,
            big_gas_limit: h.evm_big_gas_limit,
            base_fee: h.base_fee_per_gas,
            big_base_fee: h.big_base_fee_per_gas,
        }
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
        let mut hash_scratch = Vec::new();
        let hashes = block
            .native_actions
            .iter()
            .map(|action| compute_action_hash_with_scratch(action, &mut hash_scratch))
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

/// Item 7 step 1: genesis default EVM gas budget per EVM block (header `evm_gas_limit`).
pub const DEFAULT_EVM_GAS_LIMIT: u64 = 3_000_000;
/// Item 7 step 1: genesis default N; EVM txs only in blocks with `height % N == 0`.
/// s123 owner: 11 for benches and testing (was 5): prime, so it shares no factor with any
/// validator count below 11; retune before testnet.
pub const DEFAULT_EVM_BLOCK_INTERVAL: u64 = 11;
/// Item 7 step 1: most raw EVM tx bytes per block (vote rule), so also per tx (admission).
pub const MAX_EVM_BLOCK_TX_BYTES: usize = 256 * 1024;
/// Cheapest tx's intrinsic gas: `evm_gas_limit / EVM_MIN_TX_GAS` caps a block's EVM tx count.
pub const EVM_MIN_TX_GAS: u64 = 21_000;
/// Item 7 step 2 (owner decision 8): most raw EVM tx bytes at a big EVM height (vote rule), so also
/// per big-lane tx (admission). Fits the 1 MiB consensus message (`o5_feed_gates.rs`).
pub const MAX_EVM_BIG_BLOCK_TX_BYTES: usize = 768 * 1024;
/// Item 7 step 2 (owner decision 1): genesis default M; 0 = big blocks off.
pub const DEFAULT_EVM_BIG_BLOCK_INTERVAL: u64 = 0;
/// Item 7 step 2 (owner decision 2): genesis default big-lane gas limit (10-15M at switch-on).
pub const DEFAULT_EVM_BIG_GAS_LIMIT: u64 = 10_000_000;
/// Item 7 step 2 (F3): neither lane's base fee drops below this (today's fixed 1 gwei).
pub const EVM_MIN_BASE_FEE: u64 = 1_000_000_000;
/// Item 7 step 2 (owner decision 4): CoreWriter order actions wait this many blocks when the
/// governance key is absent or invalid; every other kind waits one.
pub const DEFAULT_CORE_WRITER_ORDER_DELAY: u64 = 10;
/// Item 7 step 2 (owner decision 4): most CoreWriter rows one block drains; a row whose target is
/// full queues for the next block with room (`CoreWriterQueue::enqueue`).
pub const CORE_WRITER_DRAIN_CAP: u64 = 200;

/// s94 option 2 (HL "Order price too far from oracle"): the governance key
/// (`CF_FEE_CONFIG`, a decimal string, `ParameterChange`) of the chain-wide
/// placement price band, in basis points of the market's reference price.
/// Lives here because governance (torus-economics) validates it and the
/// executor / RPC read it (torus-core), and both depend on torus-types.
pub const PRICE_BAND_PARAM: &str = "price_band_bps";
/// The band when the key is absent or invalid: ±50% of the reference.
pub const PRICE_BAND_DEFAULT_BPS: u64 = 5_000;
/// Valid governance values: ±1% .. ±90%.
pub const PRICE_BAND_BPS_RANGE: std::ops::RangeInclusive<u64> = 100..=9_000;
/// The stale-mark fallback reference is clamped to ± this of the last mark.
pub const PRICE_BAND_FALLBACK_CLAMP_BPS: u64 = 1_000;

/// The band in force for a stored [`PRICE_BAND_PARAM`] value: the value
/// when it is a decimal in [`PRICE_BAND_BPS_RANGE`], else the default.
pub fn price_band_bps(stored: Option<&[u8]>) -> u64 {
    stored
        .and_then(|b| std::str::from_utf8(b).ok())
        .and_then(|s| s.parse::<u64>().ok())
        .filter(|v| PRICE_BAND_BPS_RANGE.contains(v))
        .unwrap_or(PRICE_BAND_DEFAULT_BPS)
}

/// Item 7 step 1: governance keys (`CF_FEE_CONFIG`, decimal, `ParameterChange`). Execution of
/// boundary H copies them into the plan for H + L; absent or invalid = the chain keeps its value.
pub const EVM_BLOCK_GAS_LIMIT_PARAM: &str = "evm_block_gas_limit";
pub const EVM_BLOCK_GAS_LIMIT_RANGE: std::ops::RangeInclusive<u64> = 1_000_000..=10_000_000;
pub const EVM_BLOCK_INTERVAL_PARAM: &str = "evm_block_interval";
pub const EVM_BLOCK_INTERVAL_RANGE: std::ops::RangeInclusive<u64> = 2..=100;

/// Item 7 step 2: governance keys of the big lane (copied into the epoch plan like the two above)
/// and of the CoreWriter order delay (read when a row is queued).
pub const EVM_BIG_BLOCK_INTERVAL_PARAM: &str = "evm_big_block_interval";
/// A non-zero M must lie here; 0 switches big blocks off ([`evm_big_block_interval_ok`]).
pub const EVM_BIG_BLOCK_INTERVAL_RANGE: std::ops::RangeInclusive<u64> = 30..=10_000;
pub const EVM_BIG_BLOCK_GAS_LIMIT_PARAM: &str = "evm_big_block_gas_limit";
/// Owner decision 2: up to HL's 30M; growing there takes a vote, not a release.
pub const EVM_BIG_BLOCK_GAS_LIMIT_RANGE: std::ops::RangeInclusive<u64> = 1_000_000..=30_000_000;
pub const CORE_WRITER_ORDER_DELAY_PARAM: &str = "core_writer_order_delay";
pub const CORE_WRITER_ORDER_DELAY_RANGE: std::ops::RangeInclusive<u64> = 1..=100;

/// Delist option C (owner, s131): the governance key (`CF_FEE_CONFIG`, a
/// decimal string, `ParameterChange`) of the grace period N, in blocks: a
/// governance delist executed at height H leaves the market reduce-only until
/// H + N, then every remaining position in it is force-closed at the oracle
/// mark. N is read once, when the delist executes (a later change does not
/// move a running grace period). 0 = force-close in the delist's own block.
pub const DELIST_GRACE_PARAM: &str = "delist_grace_blocks";
/// N when the key is absent or invalid: 24 h at the 2 s target block time
/// (the governance timelock's default).
pub const DELIST_GRACE_DEFAULT_BLOCKS: u64 = 43_200;
/// Valid governance values: 0 .. 7 days at 2 s blocks.
pub const DELIST_GRACE_BLOCKS_RANGE: std::ops::RangeInclusive<u64> = 0..=302_400;

/// The grace period in force for a stored [`DELIST_GRACE_PARAM`] value: the
/// value when it is a decimal in [`DELIST_GRACE_BLOCKS_RANGE`], else the
/// default.
pub fn delist_grace_blocks(stored: Option<&[u8]>) -> u64 {
    gov_u64_in(stored, &DELIST_GRACE_BLOCKS_RANGE).unwrap_or(DELIST_GRACE_DEFAULT_BLOCKS)
}

/// Item 7 step 2: a valid M is 0 (off) or inside [`EVM_BIG_BLOCK_INTERVAL_RANGE`].
pub fn evm_big_block_interval_ok(m: u64) -> bool {
    m == 0 || EVM_BIG_BLOCK_INTERVAL_RANGE.contains(&m)
}

/// Item 7 step 2: a big EVM height: M > 0 and `height % M == 0`.
pub fn is_big_evm_height(height: u64, big_interval: u64) -> bool {
    big_interval > 0 && height.is_multiple_of(big_interval)
}

/// Item 7 step 2: EVM txs may ride `height`: the small cadence (`height % N == 0`) or a big
/// height. N = 0 (never a valid header) carries none.
pub fn is_evm_height(height: u64, interval: u64, big_interval: u64) -> bool {
    height.checked_rem(interval) == Some(0) || is_big_evm_height(height, big_interval)
}

/// A stored governance value: the decimal when it is in `range`, else `None`.
pub fn gov_u64_in(stored: Option<&[u8]>, range: &std::ops::RangeInclusive<u64>) -> Option<u64> {
    stored
        .and_then(|b| std::str::from_utf8(b).ok())
        .and_then(|s| s.parse::<u64>().ok())
        .filter(|v| range.contains(v))
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

    // === Running state hash (docs/plans/running-state-hash-impl.md) ===
    /// Validator-only: the sender's running state hash `h_height` at checkpoint
    /// `height` (`height % 100 == 0`). Appended LAST so every existing bincode
    /// discriminant is unchanged.
    AttestStateHash {
        height: u64,
        hash: B256,
    },

    // === Staking (appended — keep existing serde variant indices stable) ===
    /// Release every matured unbonding entry across all of the sender's
    /// delegations back to their balance (explicit claim, Hyperliquid-style).
    ClaimUnbonded,

    // === Oracle (appended — s517 hot oracle signer) ===
    /// Set the sender validator's hot oracle signer, or clear it with
    /// `Address::ZERO`. EIP-712 only (never a session key). The signer may then
    /// submit `SubmitOraclePrices` on the validator's behalf — nothing else
    /// (docs/plans/oracle-feeder.md §1). Rotation replaces it.
    ///
    /// `proof` (review M3) is required to set a signer and ignored when
    /// clearing: without it, another validator could squat a signer address.
    SetOracleSigner {
        signer: Address,
        proof: Option<OracleSignerProof>,
    },

    // === Order Book (appended — s114 cloid) ===
    /// Cancel the sender's open order (resting, or a pending stop) whose
    /// client order id is `cloid` in `market_id` (Hyperliquid
    /// `cancelByCloid`: asset + cloid).
    CancelByCloid {
        market_id: MarketId,
        #[serde(with = "cloid_serde")]
        cloid: Cloid,
    },

    // === EVM (appended: item 7 step 2) ===
    /// HL `evmUserModify`: the sender's EVM lane. `true` = big blocks: while they are on (header
    /// M > 0) its EVM txs go to the big pool and pay the big base fee. EIP-712 only (not a trading
    /// action, so no session key). Lanes are proposer routing and fees, never a vote rule.
    EvmUserModify {
        using_big_blocks: bool,
    },
}

impl NativeAction {
    /// Place, batch, cancel, cancel-by-cloid, cancel-all and modify: the only
    /// actions a session key may sign.
    pub fn is_trading(&self) -> bool {
        matches!(
            self,
            NativeAction::PlaceOrder(_)
                | NativeAction::PlaceOrderBatch(_)
                | NativeAction::CancelOrder { .. }
                | NativeAction::ModifyOrder { .. }
                | NativeAction::CancelAllOrders { .. }
                | NativeAction::CancelByCloid { .. }
        )
    }

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
        self.append_canonical_bytes(&mut buf);
        buf
    }

    /// Append the exact canonical encoding to an existing buffer, preserving
    /// its prefix. Callers that need replacement rather than concatenation
    /// must clear the buffer first. No serde or wire format is involved.
    pub fn append_canonical_bytes(&self, buf: &mut Vec<u8>) {
        match self {
            NativeAction::PlaceOrder(p) => {
                buf.push(0);
                Self::encode_place_order_params(buf, p);
            }
            NativeAction::PlaceOrderBatch(orders) => {
                buf.push(25);
                buf.extend_from_slice(&(orders.len() as u32).to_be_bytes());
                for p in orders {
                    Self::encode_place_order_params(buf, p);
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
                    ProposalAction::SetDelistClosePrice { market_id, price } => {
                        buf.push(5);
                        buf.extend_from_slice(&market_id.to_be_bytes());
                        buf.extend_from_slice(&price.raw().to_be_bytes());
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
            NativeAction::AttestStateHash { height, hash } => {
                buf.push(26);
                buf.extend_from_slice(&height.to_be_bytes());
                buf.extend_from_slice(hash.as_slice());
            }
            NativeAction::ClaimUnbonded => {
                buf.push(27);
            }
            NativeAction::CancelByCloid { market_id, cloid } => {
                buf.push(29);
                buf.extend_from_slice(&market_id.to_be_bytes());
                buf.extend_from_slice(&cloid.to_be_bytes());
            }
            NativeAction::SetOracleSigner { signer, proof } => {
                buf.push(28);
                buf.extend_from_slice(signer.as_slice());
                match proof {
                    None => buf.push(0),
                    Some(p) => {
                        buf.push(1);
                        buf.extend_from_slice(&p.nonce.to_be_bytes());
                        buf.push(p.signature.v);
                        buf.extend_from_slice(&p.signature.r);
                        buf.extend_from_slice(&p.signature.s);
                    }
                }
            }
            NativeAction::EvmUserModify { using_big_blocks } => {
                buf.push(30);
                buf.push(u8::from(*using_big_blocks));
            }
        }
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
    compute_action_hash_with_scratch(action, &mut Vec::with_capacity(64))
}

/// Same content address as [`compute_action_hash`], reusing a caller-owned
/// canonical-preimage buffer. Clears prior contents before encoding and leaves
/// the complete signed preimage in `data`. Capacity is retained; prefer a
/// batch-local buffer so a single large action is not retained indefinitely.
/// This is not a signature verifier or a replacement for `verified_cache_key`.
pub fn compute_action_hash_with_scratch(action: &SignedNativeAction, data: &mut Vec<u8>) -> B256 {
    data.clear();
    action.action.append_canonical_bytes(data);
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
    alloy_primitives::keccak256(&*data)
}

#[cfg(test)]
mod action_hash_scratch_tests;

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
    if !matches!(&action.signature, ActionSignature::Eip712(_)) {
        return None;
    }
    verified_cache_key_with_scratch(action, &mut Vec::with_capacity(64))
}

/// Same trust-cache identity and eligibility as [`verified_cache_key`], using
/// a batch-local preimage buffer. Clears the buffer even for an ineligible
/// session action. This encoding intentionally differs from the content hash.
pub fn verified_cache_key_with_scratch(
    action: &SignedNativeAction,
    data: &mut Vec<u8>,
) -> Option<B256> {
    data.clear();
    let ActionSignature::Eip712(sig) = &action.signature else {
        return None;
    };
    action.action.append_canonical_bytes(data);
    data.extend_from_slice(&action.nonce.to_be_bytes());
    data.push(sig.v);
    data.extend_from_slice(&sig.r);
    data.extend_from_slice(&sig.s);
    Some(alloy_primitives::keccak256(&*data))
}

/// Canonical length from which [`sort_hash_and_verified_cache_key_with_scratch`]
/// shares one absorb between both keys: one Keccak-256 rate block. Below it
/// both preimages need the same permutations either way, so two one-shot
/// hashes are cheaper than forking a streaming hasher
/// (`shared_absorb_crossover_ubench`).
pub const SHARED_ABSORB_MIN_BYTES: usize = 136;

/// Both per-action exec keys with the action's canonical bytes `C` encoded
/// once and, from [`SHARED_ABSORB_MIN_BYTES`] on, absorbed once (section 92
/// step 8e: each key was a separate encode + full keccak of `C`).
///
/// Returns `(sort_hash, cache_key)`:
/// - `sort_hash = keccak256(C)`: the content hash of the anti-MEV sort key
///   (`torus_bridge::native_executor::sort_native_actions_indexed`);
/// - `cache_key = keccak256(C || nonce || v || r || s)`, exactly
///   [`verified_cache_key`] (`None` for a session action).
///
/// Keccak-256 absorbs its input sequentially, so the sponge state after `C` is
/// the common prefix of both preimages: one copy of it is finalized for the
/// sort hash, the other absorbs the signature tail for the cache key. Both
/// values are byte-identical to hashing each preimage separately (pinned by
/// the frozen-encoder tests on both sides of the threshold). `data` is a
/// scratch buffer; its contents afterwards are unspecified.
pub fn sort_hash_and_verified_cache_key_with_scratch(
    action: &SignedNativeAction,
    data: &mut Vec<u8>,
) -> (B256, Option<B256>) {
    data.clear();
    action.action.append_canonical_bytes(data);
    let ActionSignature::Eip712(sig) = &action.signature else {
        return (alloy_primitives::keccak256(&*data), None);
    };
    let mut tail = [0u8; 8 + 1 + 32 + 32];
    tail[..8].copy_from_slice(&action.nonce.to_be_bytes());
    tail[8] = sig.v;
    tail[9..41].copy_from_slice(&sig.r);
    tail[41..].copy_from_slice(&sig.s);
    if data.len() < SHARED_ABSORB_MIN_BYTES {
        let sort_hash = alloy_primitives::keccak256(&*data);
        data.extend_from_slice(&tail);
        return (sort_hash, Some(alloy_primitives::keccak256(&*data)));
    }
    // Fork the sponge after `C`: one copy finalizes keccak256(C), the other
    // absorbs the tail of `verified_cache_key_with_scratch`'s preimage.
    let mut hasher = alloy_primitives::Keccak256::new();
    hasher.update(&*data);
    let sort_hash = hasher.clone().finalize();
    hasher.update(tail);
    (sort_hash, Some(hasher.finalize()))
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
    /// s114: u128 (16 bytes in the canonical encoding, `uint128` in
    /// EIP-712); JSON form: [`cloid_serde`].
    #[serde(default, with = "cloid_serde::option")]
    pub client_order_id: Option<Cloid>,
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
    /// Delist decisions (owner s134, 1c), appended: the settlement price of
    /// the delisted market `market_id`. Valid at submission when `price` > 0
    /// and the market has a row; executes only while the market is delisted
    /// and its settlement price is not fixed (grace period, `close_at`, a
    /// settlement still waiting for a price after `close_at`, or the delist's
    /// own block), else the proposal fails. Stored in [`delist_close_price_key`]; at the first
    /// settlement block it wins over the mark and the last aggregate.
    SetDelistClosePrice {
        market_id: MarketId,
        price: FixedPoint,
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

/// Pre-testnet P1: the `CF_NATIVE_MARKETS` key of a delisted market's flag
/// row: the 8-byte big-endian market id followed by `b'D'`. Nine bytes, so
/// every market-row scan (8-byte keys only) skips it. Value: the delist
/// height (8 bytes BE). Written by a governance delist in the proposal's
/// atomic write and never deleted; the market row itself stays (positions,
/// marks and liquidation keep working, orders are reduce-only).
pub fn market_delisted_key(market_id: MarketId) -> [u8; 9] {
    let mut key = [0u8; 9];
    key[..8].copy_from_slice(&market_id.to_be_bytes());
    key[8] = b'D';
    key
}

/// The rejection text of an order that is not reduce-only on a delisted
/// market: the executor's (`FailureReason::MarketDelisted`) and RPC
/// ingress's (node-local early answer), one source.
pub fn market_delisted_reason(market_id: MarketId) -> String {
    format!("market {market_id} is delisted: only reduce-only orders")
}

/// Delist decisions (owner s134, 3b): the `CF_NATIVE_MARKETS` key of a
/// delisted market's oracle-closed marker: the 8-byte big-endian market id
/// followed by `b'S'` (nine bytes, skipped by every market-row scan, like
/// [`market_delisted_key`]). Value: the height it was written (8 bytes BE).
/// Written once the market's settlement is complete (no settlement row, and
/// the liquidator vault and both ADL escrows flat in it); from then on its
/// oracle submissions are ignored and it is not aggregated. Never deleted.
pub fn market_oracle_closed_key(market_id: MarketId) -> [u8; 9] {
    let mut key = [0u8; 9];
    key[..8].copy_from_slice(&market_id.to_be_bytes());
    key[8] = b'S';
    key
}

/// Delist option C: the `CF_NATIVE_LIQUIDATION` tag of a delisted market's
/// settlement row (`0x08 ‖ market(8)`, torus-core `liquidation.rs`).
pub const DELIST_SETTLE_TAG: u8 = 0x08;
/// Delist decisions (owner s134, 1c): the `CF_NATIVE_LIQUIDATION` tag of a
/// governance close price (`0x09 ‖ market(8)` -> price raw (16, BE)).
pub const DELIST_CLOSE_PRICE_TAG: u8 = 0x09;

/// Delist keep-close-price (owner s134): the `CF_NATIVE_LIQUIDATION` tag of
/// a settled market's kept settlement record (`0x0A ‖ market(8)` ->
/// `close_at(8, BE) ‖ price raw(16, BE) ‖ settled_height(8, BE)`), written in
/// the block the settlement walk ends (the block that deletes the `0x08`
/// row). Never deleted, never read by consensus: the close price and close
/// height stay queryable after the settlement. 0x0A, not 0x08: no prefix scan
/// of active settlement rows sees it.
pub const DELIST_SETTLED_TAG: u8 = 0x0A;

/// The key of `market_id`'s kept settlement record (`0x0A ‖ market`).
pub fn delist_settled_key(market_id: MarketId) -> [u8; 9] {
    let mut k = [0u8; 9];
    k[0] = DELIST_SETTLED_TAG;
    k[1..].copy_from_slice(&market_id.to_be_bytes());
    k
}

/// The key of `market_id`'s delist settlement row (`0x08 ‖ market`).
pub fn delist_settlement_key(market_id: MarketId) -> [u8; 9] {
    let mut k = [0u8; 9];
    k[0] = DELIST_SETTLE_TAG;
    k[1..].copy_from_slice(&market_id.to_be_bytes());
    k
}

/// The key of `market_id`'s governance close price (`0x09 ‖ market`).
pub fn delist_close_price_key(market_id: MarketId) -> [u8; 9] {
    let mut k = [0u8; 9];
    k[0] = DELIST_CLOSE_PRICE_TAG;
    k[1..].copy_from_slice(&market_id.to_be_bytes());
    k
}

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
    /// Item 7 step 1: genesis N (`evm.block_interval`), the genesis parent
    /// header's `evm_block_interval`. Absent in a legacy config = 11
    /// ([`DEFAULT_EVM_BLOCK_INTERVAL`]).
    #[serde(default = "default_evm_block_interval")]
    pub evm_block_interval: u64,
    /// Item 7 step 2: genesis M (`evm.big_block_interval`); absent = 0.
    #[serde(default)]
    pub evm_big_block_interval: u64,
    /// Item 7 step 2: genesis big-lane gas limit (`evm.big_gas_limit`); absent = 10M.
    #[serde(default = "default_evm_big_gas_limit")]
    pub evm_big_gas_limit: u64,
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
    /// S470: exponent cap of the COMMIT-LAG view-timeout backoff, which
    /// stretches view deadlines while the QC frontier outruns the commit
    /// frontier (the header-pipeline commit wedge; the Task A stall term is
    /// blind to it). 0 (the default, and the value for any genesis file that
    /// omits the field) disables the term entirely — deadline schedule
    /// byte-identical to pre-S470.
    ///
    /// FLEET-UNIFORM: consensus-liveness critical — every validator of a
    /// chain must use the same value (genesis-sourced). Divergent values
    /// across a fleet = divergent view-deadline schedules = degraded liveness
    /// (premature Bracha timeouts), though never a safety violation.
    #[serde(default)]
    pub commit_lag_backoff_cap: u32,
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
    /// P0.0: when each [`Feature`] switches on: genesis `consensus.activations`,
    /// its old `state_hash_activation_height` alias and the built-in table for
    /// `chain_id`. CHAIN-WIDE: the header `features` rule checks every block
    /// against it. Empty = no feature ever active.
    #[serde(default)]
    pub activations: ActivationSchedule,
    /// Order-book storage mode (owner s131; genesis `consensus.book_mode`).
    /// CHAIN-WIDE and fixed for the chain's life: modes 1-3 change the state
    /// format, so every node of a chain takes its mode from here (the node
    /// env `TORUS_BOOK_ROWS` may only repeat it). Absent = Classic.
    #[serde(default, skip_serializing_if = "book_mode::BookMode::is_classic")]
    pub book_mode: book_mode::BookMode,
}

fn default_timeout_base_ms() -> u64 {
    1200
}

fn default_evm_block_interval() -> u64 {
    DEFAULT_EVM_BLOCK_INTERVAL
}

fn default_evm_big_gas_limit() -> u64 {
    DEFAULT_EVM_BIG_GAS_LIMIT
}

fn default_backoff_factor() -> u32 {
    2
}

fn default_backoff_cap() -> u32 {
    8
}

impl ChainConfig {
    /// L6.1: the first height of window-committed books
    /// (docs/plans/l61-window-committed-books.md), `None` = not scheduled:
    /// the schedule's [`Feature::BookWindowCommit`] height, 0 read as 1
    /// (genesis `consensus.activations.book_window_commit`, or the bool
    /// `consensus.book_window_commit`: from the running hash's height). Only
    /// an expectation, as [`Self::state_hash_activation_height`]: execution
    /// runs the window rule where the committed headers carry the bit. The
    /// boot-time L3.1 gate reads it.
    pub fn book_window_activation(&self) -> Option<u64> {
        self.activations
            .height_of(Feature::BookWindowCommit)
            .map(|h| h.max(1))
    }

    /// FIX CONS-PF-04: Validate chain configuration parameters.
    pub fn validate(&self) -> Result<(), String> {
        if self.epoch_length == 0 {
            return Err("epoch_length must be >= 1".to_string());
        }
        if self.max_validators == 0 {
            return Err("max_validators must be >= 1".to_string());
        }
        let book_window = self.book_window_activation().is_some();
        if book_window && self.state_hash_activation_height().is_none() {
            return Err(
                "book_window_commit needs the running state hash (activations.running_state_hash): \
                 it only changes the running hash"
                    .to_string(),
            );
        }
        if book_window && !self.book_mode.is_classic() {
            return Err(format!(
                "book_window_commit (L6.1) needs book_mode classic, not {}",
                self.book_mode.name()
            ));
        }
        Ok(())
    }

    /// The running state hash height this node's schedule expects
    /// (docs/plans/running-state-hash-impl.md): `activations.running_state_hash`
    /// (the genesis `consensus.state_hash_activation_height` is its alias), 0
    /// read as 1; `None` = not scheduled. P0.0 phase 3: derived from the
    /// schedule only, and only an expectation: execution starts the hash at
    /// the first committed header carrying the `RunningStateHash` bit, never
    /// at this height (`torus_state::running_hash::configure_activation`).
    pub fn state_hash_activation_height(&self) -> Option<u64> {
        self.activations
            .height_of(Feature::RunningStateHash)
            .map(|h| h.max(1))
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

    /// L6.1 under P0.0: `ChainConfig::book_window_activation` is the
    /// schedule's `book_window_commit` height (0 read as 1), whatever else is
    /// scheduled; `validate` refuses it without the running hash or with a
    /// row book mode. RED before the wiring: it read the old bool.
    #[test]
    fn book_window_activation_is_the_schedule_height() {
        let sched = |pairs: &[(Feature, u64)]| {
            ActivationSchedule::merge(&pairs.iter().copied().collect(), &[]).unwrap()
        };
        // Built from JSON so the test names no field beyond the schedule.
        let base: ChainConfig = serde_json::from_value(serde_json::json!({
            "chain_id": 1, "chain_name": "t", "evm_gas_limit": 1, "evm_block_interval": 1,
            "evm_big_block_interval": 0, "evm_big_gas_limit": 1, "base_fee_per_gas": 1,
            "epoch_length": 10, "max_validators": 4, "min_stake": "0x0",
            "fee_burn_bps": 10000, "fee_validator_bps": 0, "fee_treasury_bps": 0,
            "fee_dev_pool_bps": 0,
        }))
        .unwrap();
        let with = |pairs: &[(Feature, u64)]| ChainConfig {
            activations: sched(pairs),
            ..base.clone()
        };
        let rsh = Feature::RunningStateHash;
        let win = Feature::BookWindowCommit;
        assert_eq!(base.book_window_activation(), None);
        assert_eq!(with(&[(rsh, 1)]).book_window_activation(), None);
        assert_eq!(with(&[(rsh, 1), (win, 1)]).book_window_activation(), Some(1));
        assert_eq!(with(&[(rsh, 0), (win, 0)]).book_window_activation(), Some(1));
        assert_eq!(with(&[(rsh, 1), (win, 250)]).book_window_activation(), Some(250));
        assert!(with(&[(rsh, 1), (win, 250)]).validate().is_ok());
        assert!(with(&[(win, 5)]).validate().is_err(), "L6.1 without the running hash");
        let rows = ChainConfig {
            book_mode: book_mode::BookMode::LevelAuthorityChunked,
            ..with(&[(rsh, 1), (win, 1)])
        };
        assert!(rows.validate().is_err(), "L6.1 needs Classic");
        let rows_off = ChainConfig {
            book_mode: book_mode::BookMode::LevelAuthorityChunked,
            ..with(&[(rsh, 1)])
        };
        assert!(rows_off.validate().is_ok());
    }

    /// Item 7 step 2: EVM txs ride the small cadence (`h % N == 0`) or a big height
    /// (M > 0, `h % M == 0`); M = 0 switches big heights off.
    #[test]
    fn evm_height_rules_cover_the_small_and_big_cadence() {
        assert!(
            is_evm_height(10, 5, 0) && !is_big_evm_height(10, 0),
            "M = 0: off"
        );
        assert!(!is_evm_height(11, 5, 0));
        assert!(
            is_evm_height(7, 5, 7) && is_big_evm_height(7, 7),
            "a big height off the small cadence"
        );
        assert!(is_evm_height(35, 5, 7) && is_big_evm_height(35, 7));
        assert!(!is_evm_height(8, 5, 7));
        assert!(!is_evm_height(10, 0, 0), "N = 0 carries nothing, no panic");
    }

    /// Item 7 step 2: a valid M is 0 (off) or 30..=10,000; the big-height byte cap is 768 KiB.
    #[test]
    fn big_block_interval_is_zero_or_in_range() {
        for (m, ok) in [
            (0, true),
            (1, false),
            (29, false),
            (30, true),
            (190, true),
            (10_000, true),
            (10_001, false),
        ] {
            assert_eq!(evm_big_block_interval_ok(m), ok, "{m}");
        }
        assert_eq!(MAX_EVM_BIG_BLOCK_TX_BYTES, 786_432);
    }

    /// s517 oracle feeder S1: `SetOracleSigner` is canonical tag 28 (27 is
    /// ClaimUnbonded after the s87 rebase; 26 is main's AttestStateHash) + the
    /// 20-byte signer, and survives a serde JSON round trip.
    #[test]
    fn set_oracle_signer_canonical_bytes_and_json_roundtrip() {
        let signer = Address::repeat_byte(0x5a);
        let a = NativeAction::SetOracleSigner { signer, proof: None };
        let mut want = vec![28u8];
        want.extend_from_slice(signer.as_slice());
        want.push(0); // review M3: no proof
        assert_eq!(a.canonical_bytes(), want);
        let json = serde_json::to_string(&a).unwrap();
        let back: NativeAction = serde_json::from_str(&json).unwrap();
        assert_eq!(back.canonical_bytes(), want);
        let clear = NativeAction::SetOracleSigner { signer: Address::ZERO, proof: None };
        assert_ne!(clear.canonical_bytes(), want);
        // With a proof: 1 ‖ nonce(8 BE) ‖ v ‖ r ‖ s.
        let sig = Signature { v: 27, r: [1; 32], s: [2; 32] };
        let p = NativeAction::SetOracleSigner {
            signer,
            proof: Some(OracleSignerProof { nonce: 5, signature: sig }),
        };
        let b = p.canonical_bytes();
        assert_eq!(b.len(), 1 + 20 + 1 + 8 + 65);
        assert_eq!(&b[21..30], &[1, 0, 0, 0, 0, 0, 0, 0, 5]);
        assert_eq!(b[30], 27);
        let back: NativeAction = serde_json::from_str(&serde_json::to_string(&p).unwrap()).unwrap();
        assert_eq!(back.canonical_bytes(), b);
    }

    /// Item 7 step 2: `EvmUserModify` is canonical tag 30 + the flag byte, bincode variant 30
    /// (appended last), EIP-712 only (no session scope allows it) and survives serde JSON.
    #[test]
    fn evm_user_modify_is_canonical_tag_30_and_eip712_only() {
        let on = NativeAction::EvmUserModify {
            using_big_blocks: true,
        };
        let off = NativeAction::EvmUserModify {
            using_big_blocks: false,
        };
        assert_eq!(on.canonical_bytes(), vec![30, 1]);
        assert_eq!(off.canonical_bytes(), vec![30, 0]);
        for a in [&on, &off] {
            assert!(!a.is_trading());
            assert!(crate::eip712::requires_eip712(a));
            for scope in [
                SessionScope::Trading,
                SessionScope::TransfersOnly,
                SessionScope::Full,
            ] {
                assert!(!scope.allows(a), "{scope:?} must not allow EvmUserModify");
            }
            let bytes = bincode::serialize(a).unwrap();
            assert_eq!(&bytes[..4], &30u32.to_le_bytes());
            let back: NativeAction = bincode::deserialize(&bytes).unwrap();
            assert_eq!(back.canonical_bytes(), a.canonical_bytes());
            let json = serde_json::to_string(a).unwrap();
            let back: NativeAction = serde_json::from_str(&json).unwrap();
            assert_eq!(back.canonical_bytes(), a.canonical_bytes());
        }
    }

    /// S470 knob default: a ChainConfig serialized before the field existed
    /// (no `commit_lag_backoff_cap` key) must deserialize with the commit-lag
    /// backoff OFF (0) — an un-upgraded genesis runs the exact pre-S470
    /// deadline schedule.
    #[test]
    fn chain_config_commit_lag_backoff_cap_defaults_to_zero() {
        let json = r#"{
            "chain_id": 1,
            "chain_name": "t",
            "evm_gas_limit": 1,
            "base_fee_per_gas": 1,
            "epoch_length": 1,
            "max_validators": 1,
            "min_stake": "0x0",
            "fee_burn_bps": 0,
            "fee_validator_bps": 0,
            "fee_treasury_bps": 0,
            "fee_dev_pool_bps": 0
        }"#;
        let config: ChainConfig = serde_json::from_str(json).expect("legacy config must parse");
        assert_eq!(config.commit_lag_backoff_cap, 0);
        // The pre-existing backoff defaults are unchanged by S470.
        assert_eq!(config.backoff_factor, 2);
        assert_eq!(config.backoff_cap, 8);
    }

    /// s69: a ChainConfig without `timeout_base_ms` gets the 1200 ms base
    /// (s68/s69 A/B: 5/5 pairs won over 500, dead views -85%).
    #[test]
    fn chain_config_timeout_base_defaults_to_1200() {
        let json = r#"{
            "chain_id": 1,
            "chain_name": "t",
            "evm_gas_limit": 1,
            "base_fee_per_gas": 1,
            "epoch_length": 1,
            "max_validators": 1,
            "min_stake": "0x0",
            "fee_burn_bps": 0,
            "fee_validator_bps": 0,
            "fee_treasury_bps": 0,
            "fee_dev_pool_bps": 0
        }"#;
        let config: ChainConfig = serde_json::from_str(json).expect("legacy config must parse");
        assert_eq!(config.timeout_base_ms, 1200);
    }

    /// s123 owner: N = 11 for benches and testing (was 5). A ChainConfig
    /// without `evm_block_interval` gets it; the gas default stays 3M.
    #[test]
    fn chain_config_evm_block_interval_defaults_to_11() {
        let json = r#"{
            "chain_id": 1,
            "chain_name": "t",
            "evm_gas_limit": 1,
            "base_fee_per_gas": 1,
            "epoch_length": 1,
            "max_validators": 1,
            "min_stake": "0x0",
            "fee_burn_bps": 0,
            "fee_validator_bps": 0,
            "fee_treasury_bps": 0,
            "fee_dev_pool_bps": 0
        }"#;
        let config: ChainConfig = serde_json::from_str(json).expect("legacy config must parse");
        assert_eq!(config.evm_block_interval, 11);
        // Item 7 step 2: big blocks off, a 10M big lane.
        assert_eq!(
            (config.evm_big_block_interval, config.evm_big_gas_limit),
            (0, 10_000_000)
        );
        assert_eq!(DEFAULT_EVM_BLOCK_INTERVAL, 11);
        assert_eq!(DEFAULT_EVM_GAS_LIMIT, 3_000_000);
    }

    /// Item 7 step 1: a stored governance value counts only when it is a
    /// decimal inside the key's range.
    #[test]
    fn gov_u64_in_parses_in_range_else_none() {
        let r = 2..=100u64;
        assert_eq!(gov_u64_in(None, &r), None);
        assert_eq!(gov_u64_in(Some(b"abc"), &r), None);
        assert_eq!(gov_u64_in(Some(b"1"), &r), None);
        assert_eq!(gov_u64_in(Some(b"101"), &r), None);
        assert_eq!(gov_u64_in(Some(b"5"), &r), Some(5));
    }

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

    #[test]
    fn fixed_point_from_str_parses_decimals() {
        let p = |s: &str| s.parse::<FixedPoint>().map(|f| f.raw());
        assert_eq!(p("123.45"), Ok(12_345_000_000));
        assert_eq!(p("-0.5"), Ok(-50_000_000));
        assert_eq!(p(".5"), Ok(50_000_000));
        assert_eq!(p("100"), Ok(10_000_000_000));
        assert_eq!(p("0.00000001"), Ok(1));
        for bad in [
            "",
            "-",
            ".",
            "1.123456789",
            "1.2.3",
            "0x10",
            "abc",
            "--5",
            "1.-5",
            "+5",
        ] {
            assert!(p(bad).is_err(), "{bad:?} must be rejected");
        }
    }

    #[test]
    fn fixed_point_display_round_trips_through_from_str() {
        for raw in [
            -150_000_000,
            0,
            12_345_000_000,
            -1,
            i128::MAX,
            i128::MIN + 1,
        ] {
            let x = FixedPoint::from_raw(raw);
            assert_eq!(x.to_string().parse::<FixedPoint>(), Ok(x));
        }
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

    /// Item 6 P2: the i256 reference `checked_mul` / `checked_div` (the
    /// pre-P2 bodies, verbatim).
    fn ref_mul(a: i128, b: i128) -> Result<i128, ArithmeticError> {
        use ethnum::i256;
        let result = i256::from(a) * i256::from(b) / i256::from(FixedPoint::SCALE);
        if result > i256::from(i128::MAX) || result < i256::from(i128::MIN) {
            return Err(ArithmeticError::Overflow);
        }
        Ok(result.as_i128())
    }

    fn ref_div(a: i128, b: i128) -> Result<i128, ArithmeticError> {
        if b == 0 {
            return Err(ArithmeticError::DivisionByZero);
        }
        use ethnum::i256;
        let result = i256::from(a) * i256::from(FixedPoint::SCALE) / i256::from(b);
        if result > i256::from(i128::MAX) || result < i256::from(i128::MIN) {
            return Err(ArithmeticError::Overflow);
        }
        Ok(result.as_i128())
    }

    /// Operands for the P2 differential tests: the i128 edges, ±SCALE
    /// multiples, values around sqrt(i128::MAX) and i128::MAX / SCALE, and
    /// random values of every bit length and both signs.
    fn p2_operands() -> Vec<i128> {
        let s = FixedPoint::SCALE;
        let root = 13_043_817_825_332_782_212i128; // floor(sqrt(i128::MAX))
        let q = i128::MAX / s;
        let mut v = vec![
            0, 1, 2, 7, s - 1, s, s + 1, 2 * s, 99_999_999, 123_456_789_012,
            i128::MAX, i128::MAX - 1, i128::MIN, i128::MIN + 1,
            i128::MAX / 2, i128::MIN / 2, root - 1, root, root + 1,
            q - 1, q, q + 1, i128::MAX / 3, 1 << 64, (1 << 64) - 1, 1 << 100,
        ];
        let neg: Vec<i128> = v.iter().filter(|x| **x != i128::MIN).map(|x| -x).collect();
        v.extend(neg);
        let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut next = || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };
        for _ in 0..600 {
            let bits = (next() % 128) as u32;
            let raw = ((u128::from(next()) << 64) | u128::from(next())) >> (127 - bits);
            let r = raw as i128;
            v.push(if next() & 1 == 1 { r.wrapping_neg() } else { r });
        }
        v
    }

    /// Item 6 P2: the i128 fast path of `checked_mul` is bit-identical to the
    /// i256 path on every pair — including products that overflow i128
    /// whose quotient fits (fallback) and quotients that overflow (Err).
    #[test]
    fn p2_checked_mul_matches_i256_reference() {
        let ops = p2_operands();
        let (mut fast, mut fallback_ok, mut overflow) = (0, 0, 0);
        for &a in &ops {
            for &b in &ops {
                let want = ref_mul(a, b);
                let got = FixedPoint(a).checked_mul(FixedPoint(b)).map(|r| r.0);
                assert_eq!(got, want, "checked_mul({a}, {b})");
                match (a.checked_mul(b), want) {
                    (Some(_), _) => fast += 1,
                    (None, Ok(_)) => fallback_ok += 1,
                    (None, Err(_)) => overflow += 1,
                }
            }
        }
        // Every class is exercised.
        assert!(fast > 1000 && fallback_ok > 1000 && overflow > 1000, "{fast} {fallback_ok} {overflow}");
    }

    /// Item 6 P2: the same for `checked_div` (i128 `a * SCALE / b` when the
    /// numerator fits and the division does not overflow).
    #[test]
    fn p2_checked_div_matches_i256_reference() {
        let ops = p2_operands();
        let (mut fast, mut fallback_ok, mut overflow) = (0, 0, 0);
        for &a in &ops {
            for &b in &ops {
                let want = ref_div(a, b);
                let got = FixedPoint(a).checked_div(FixedPoint(b)).map(|r| r.0);
                assert_eq!(got, want, "checked_div({a}, {b})");
                match (a.checked_mul(FixedPoint::SCALE), want) {
                    (_, Err(ArithmeticError::DivisionByZero)) => {}
                    (Some(_), _) => fast += 1,
                    (None, Ok(_)) => fallback_ok += 1,
                    (None, Err(_)) => overflow += 1,
                }
            }
        }
        assert!(fast > 1000 && fallback_ok > 1000 && overflow > 1000, "{fast} {fallback_ok} {overflow}");
        // MIN / -1 overflows on both paths (the numerator a * SCALE can
        // never be i128::MIN — not a multiple of SCALE — so the i128
        // division itself never overflows; `checked_div` guards it anyway).
        assert_eq!(FixedPoint::MIN.checked_div(FixedPoint(-1)), Err(ArithmeticError::Overflow));
        assert_eq!(ref_div(i128::MIN, -1), Err(ArithmeticError::Overflow));
        assert_ne!(i128::MIN % FixedPoint::SCALE, 0);
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
            evm_block_interval: DEFAULT_EVM_BLOCK_INTERVAL,
            evm_big_block_interval: DEFAULT_EVM_BIG_BLOCK_INTERVAL,
            evm_big_gas_limit: DEFAULT_EVM_BIG_GAS_LIMIT,
            native_action_count: 0,
            evm_tx_count: 0,
            base_fee_per_gas: 1_000_000_000,
            big_base_fee_per_gas: 1_000_000_000,
            epoch: 0,
            validator_set_hash: B256::ZERO,
            features: 0,
            sig_attestation: [0u8; 64],
        };
        header.sig_attestation = [42u8; 64];
        let json = serde_json::to_vec(&header).unwrap();
        let decoded: TorusBlockHeader = serde_json::from_slice(&json).unwrap();
        assert_eq!(decoded.sig_attestation, [42u8; 64]);
    }

    #[test]
    fn header_without_attestation_defaults_to_zero() {
        let json = br#"{"height":1,"timestamp":1000,"proposer":"0x0000000000000000000000000000000000000000","state_root":"0x0000000000000000000000000000000000000000000000000000000000000000","receipts_root":"0x0000000000000000000000000000000000000000000000000000000000000000","logs_bloom":"0x00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000","evm_gas_used":0,"evm_fee_revenue":0,"evm_gas_limit":30000000,"evm_block_interval":5,"evm_big_block_interval":0,"evm_big_gas_limit":10000000,"native_action_count":0,"evm_tx_count":0,"base_fee_per_gas":1000000000,"big_base_fee_per_gas":1000000000,"epoch":0,"validator_set_hash":"0x0000000000000000000000000000000000000000000000000000000000000000"}"#;
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
            evm_block_interval: DEFAULT_EVM_BLOCK_INTERVAL,
            evm_big_block_interval: DEFAULT_EVM_BIG_BLOCK_INTERVAL,
            evm_big_gas_limit: DEFAULT_EVM_BIG_GAS_LIMIT,
            native_action_count: 0,
            evm_tx_count: 0,
            base_fee_per_gas: 1_000_000_000,
            big_base_fee_per_gas: 1_000_000_000,
            epoch: 0,
            validator_set_hash: B256::ZERO,
            features: 0,
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
            evm_block_interval: DEFAULT_EVM_BLOCK_INTERVAL,
            evm_big_block_interval: DEFAULT_EVM_BIG_BLOCK_INTERVAL,
            evm_big_gas_limit: DEFAULT_EVM_BIG_GAS_LIMIT,
            native_action_count: 0,
            evm_tx_count: 0,
            base_fee_per_gas: 1_000_000_000,
            big_base_fee_per_gas: 1_000_000_000,
            epoch: 0,
            validator_set_hash: B256::ZERO,
            features: 0,
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

    /// P0.0: the feature bits are part of the block identity (two headers
    /// differing only in them hash differently) and survive the bincode wire
    /// format and the JSON header record. RED before phase 1: not hashed.
    #[test]
    fn features_in_canonical_bytes_and_round_trip() {
        let a = test_header();
        let mut b = a.clone();
        b.features = 1;
        assert_ne!(a.canonical_header_bytes(), b.canonical_header_bytes());
        // No bits: the pre-P0.0 encoding (and block hash); bits: 16 BE bytes appended.
        let base = a.canonical_header_bytes();
        assert_eq!(base.len(), 8 + 32 + 8 + 20 + 32 + 256 + 8 + 16 + 8 * 4 + 4 + 4 + 8 * 3 + 32);
        assert_eq!(b.canonical_header_bytes(), [&base[..], &1u128.to_be_bytes()[..]].concat());
        let mut c = a.clone();
        c.features = 1 << 127;
        assert_ne!(b.canonical_header_bytes(), c.canonical_header_bytes());
        let wire: TorusBlockHeader =
            bincode::deserialize(&bincode::serialize(&c).unwrap()).unwrap();
        assert_eq!(wire.features, 1 << 127);
        // No bits too: the field is always on the bincode wire (never skipped).
        let wire: TorusBlockHeader =
            bincode::deserialize(&bincode::serialize(&a).unwrap()).unwrap();
        assert_eq!(wire.canonical_header_bytes(), base);
        let json: TorusBlockHeader =
            serde_json::from_slice(&serde_json::to_vec(&c).unwrap()).unwrap();
        assert_eq!(json.features, 1 << 127);
    }

    /// Item 7 step 1: N is part of the block identity, so two headers that
    /// differ only in it hash differently.
    #[test]
    fn evm_block_interval_in_canonical_bytes() {
        let a = test_header();
        let mut b = a.clone();
        b.evm_block_interval += 1;
        assert_ne!(a.canonical_header_bytes(), b.canonical_header_bytes());
    }

    /// Item 7 step 1: bincode 1 is the header wire format (proposal datums,
    /// `datum_header`); N survives a round trip.
    #[test]
    fn header_bincode_round_trip_keeps_evm_block_interval() {
        let mut header = test_header();
        header.evm_block_interval = 7;
        // Item 7 step 2: and the big lane's three values.
        header.evm_big_block_interval = 190;
        header.evm_big_gas_limit = 15_000_000;
        header.big_base_fee_per_gas = 2_000_000_000;
        let bytes = bincode::serialize(&header).unwrap();
        let back: TorusBlockHeader = bincode::deserialize(&bytes).unwrap();
        assert_eq!(back.evm_block_interval, 7);
        assert_eq!(
            (
                back.evm_big_block_interval,
                back.evm_big_gas_limit,
                back.big_base_fee_per_gas
            ),
            (190, 15_000_000, 2_000_000_000)
        );
    }

    /// Item 7 step 2: M, the big gas limit and the big base fee are block identity.
    #[test]
    fn big_lane_fields_in_canonical_bytes() {
        let a = test_header();
        type Change = fn(&mut TorusBlockHeader);
        let changes: [Change; 3] = [
            |h| h.evm_big_block_interval += 1,
            |h| h.evm_big_gas_limit += 1,
            |h| h.big_base_fee_per_gas += 1,
        ];
        for change in changes {
            let mut b = a.clone();
            change(&mut b);
            assert_ne!(a.canonical_header_bytes(), b.canonical_header_bytes());
        }
    }

    /// Item 7 step 2: a big height adds the big limit to the gas cap and lifts the byte cap
    /// to 768 KiB; off a big height (or with M = 0) the caps are step 1's.
    #[test]
    fn header_caps_add_the_big_lane_at_big_heights() {
        let mut h = test_header();
        h.evm_gas_limit = 3_000_000;
        h.evm_block_interval = 5;
        h.evm_big_block_interval = 50;
        h.evm_big_gas_limit = 10_000_000;
        h.height = 100;
        assert!(h.is_big_evm_height() && h.is_evm_height());
        assert_eq!(h.evm_block_gas_cap(), 13_000_000);
        assert_eq!(h.evm_block_byte_cap(), MAX_EVM_BIG_BLOCK_TX_BYTES);
        h.height = 105;
        assert!(!h.is_big_evm_height() && h.is_evm_height());
        assert_eq!(h.evm_block_gas_cap(), 3_000_000);
        assert_eq!(h.evm_block_byte_cap(), MAX_EVM_BLOCK_TX_BYTES);
        h.evm_big_block_interval = 0;
        h.height = 100;
        assert_eq!(h.evm_block_gas_cap(), 3_000_000, "M = 0: big blocks off");
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
