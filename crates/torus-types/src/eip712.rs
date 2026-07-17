//! EIP-712 typed structured data signing for native actions.
//!
//! Each [`NativeAction`] variant has its own EIP-712 type hash and ABI-encoded
//! struct representation.  The nonce (timestamp-based, milliseconds) is included
//! in every struct for replay protection.
//!
//! Domain: `{ name: "Torus", version: "1", chainId: 7778, verifyingContract: 0x0 }`

use alloy_primitives::{keccak256, Address, B256, U256};
use ed25519_dalek::{Verifier, VerifyingKey as Ed25519VerifyingKey};
use k256::ecdsa::{RecoveryId, SigningKey, VerifyingKey};
use std::sync::LazyLock;

use crate::{
    ActionSignature, FixedPoint, MarketId, MarketListing, MarketParams, NativeAction,
    OracleSubmission, OrderId, OrderType, PlaceOrderParams, Proposal, ProposalAction, PublicKey,
    SessionScope, Signature, SignedNativeAction, VoteOption,
};

// ============================================================================
// Constants
// ============================================================================

/// Torus chain ID embedded in the EIP-712 domain separator.
pub const TORUS_CHAIN_ID: u64 = 7778;

/// Maximum allowed nonce drift from current time (60 seconds).
pub const NONCE_WINDOW_MS: u64 = 60_000;

// ============================================================================
// Errors
// ============================================================================

/// Errors from EIP-712 signing, recovery, and validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Eip712Error {
    /// Signature bytes could not be decoded.
    InvalidSignature,
    /// Public key recovery from the prehash failed.
    RecoveryFailed,
    /// Nonce is older than the allowed window.
    NonceTooOld,
    /// Nonce is further in the future than the allowed window.
    NonceTooFuture,
    /// Node's chain ID does not match the domain separator chain ID.
    ChainIdMismatch { expected: u64, got: u64 },
    /// Session key signature verification failed.
    SessionSignatureInvalid,
    /// Session key not found in state.
    SessionNotFound,
    /// Session key has expired.
    SessionExpired,
    /// Action not allowed by session scope.
    SessionScopeViolation,
    /// Action requires EIP-712 signature (cannot use session key).
    RequiresEip712,
    /// Max active sessions exceeded.
    MaxSessionsExceeded,
    /// Session expiry exceeds maximum allowed (24h).
    ExpiryTooFar,
}

impl std::fmt::Display for Eip712Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidSignature => write!(f, "invalid signature encoding"),
            Self::RecoveryFailed => write!(f, "public key recovery failed"),
            Self::NonceTooOld => write!(f, "nonce too old (>60s)"),
            Self::NonceTooFuture => write!(f, "nonce too far in future (>60s)"),
            Self::ChainIdMismatch { expected, got } => {
                write!(f, "chain ID mismatch: expected {expected}, got {got}")
            }
            Self::SessionSignatureInvalid => write!(f, "ed25519 session signature invalid"),
            Self::SessionNotFound => write!(f, "session key not found"),
            Self::SessionExpired => write!(f, "session key expired"),
            Self::SessionScopeViolation => write!(f, "action not allowed by session scope"),
            Self::RequiresEip712 => write!(f, "action requires EIP-712 ECDSA signature"),
            Self::MaxSessionsExceeded => write!(f, "max 5 active sessions per address"),
            Self::ExpiryTooFar => write!(f, "session expiry exceeds 24h maximum"),
        }
    }
}

impl std::error::Error for Eip712Error {}

// ============================================================================
// ABI Encoding Helpers (each value left-padded to 32 bytes, big-endian)
// ============================================================================

fn encode_u8(v: u8) -> [u8; 32] {
    let mut buf = [0u8; 32];
    buf[31] = v;
    buf
}

fn encode_u16(v: u16) -> [u8; 32] {
    let mut buf = [0u8; 32];
    buf[30..].copy_from_slice(&v.to_be_bytes());
    buf
}

fn encode_u64(v: u64) -> [u8; 32] {
    let mut buf = [0u8; 32];
    buf[24..].copy_from_slice(&v.to_be_bytes());
    buf
}

fn encode_u128(v: u128) -> [u8; 32] {
    let mut buf = [0u8; 32];
    buf[16..].copy_from_slice(&v.to_be_bytes());
    buf
}

/// Two's complement sign-extension of i128 to int256 (32 bytes).
fn encode_i128(v: i128) -> [u8; 32] {
    let fill = if v < 0 { 0xff } else { 0x00 };
    let mut buf = [fill; 32];
    buf[16..].copy_from_slice(&v.to_be_bytes());
    buf
}

fn encode_bool(v: bool) -> [u8; 32] {
    let mut buf = [0u8; 32];
    if v {
        buf[31] = 1;
    }
    buf
}

fn encode_address(addr: &Address) -> [u8; 32] {
    let mut buf = [0u8; 32];
    buf[12..].copy_from_slice(addr.as_slice());
    buf
}

fn encode_u256(v: &U256) -> [u8; 32] {
    v.to_be_bytes::<32>()
}

fn encode_bytes32(v: &B256) -> [u8; 32] {
    v.0
}

/// EIP-712: dynamic types (string, bytes) are hashed before encoding.
fn encode_string(s: &str) -> [u8; 32] {
    keccak256(s.as_bytes()).0
}

// ============================================================================
// Domain Separator
// ============================================================================

/// Precomputed Torus EIP-712 domain separator.
///
/// The domain is entirely compile-time constant (name "Torus", version "1",
/// `TORUS_CHAIN_ID`, zero verifying contract), so the 4 constituent keccaks + the
/// final one are computed exactly once on first use instead of per verify call.
/// The value is byte-identical to the pre-hoist inline computation (asserted in
/// `hoisted_domain_separator_matches_fresh`).
static DOMAIN_SEPARATOR: LazyLock<B256> = LazyLock::new(|| {
    let type_hash = keccak256(
        "EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)",
    );
    let mut buf = Vec::with_capacity(5 * 32);
    buf.extend_from_slice(&type_hash.0);
    buf.extend_from_slice(&encode_string("Torus"));
    buf.extend_from_slice(&encode_string("1"));
    buf.extend_from_slice(&encode_u256(&U256::from(TORUS_CHAIN_ID)));
    buf.extend_from_slice(&encode_address(&Address::ZERO));
    keccak256(&buf)
});

/// Compute the EIP-712 domain separator for Torus.
///
/// ```text
/// keccak256(
///   typeHash("EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)")
///   || keccak256("Torus")
///   || keccak256("1")
///   || uint256(7778)
///   || address(0x0)
/// )
/// ```
pub fn eip712_domain_separator() -> B256 {
    *DOMAIN_SEPARATOR
}

// ============================================================================
// Signing Hash
// ============================================================================

/// `keccak256("\x19\x01" || domainSeparator || structHash)`
pub fn eip712_signing_hash(domain_separator: B256, struct_hash: B256) -> B256 {
    let mut buf = Vec::with_capacity(2 + 32 + 32);
    buf.push(0x19);
    buf.push(0x01);
    buf.extend_from_slice(domain_separator.as_slice());
    buf.extend_from_slice(struct_hash.as_slice());
    keccak256(&buf)
}

// ============================================================================
// Struct Hash — one function per NativeAction variant
// ============================================================================

/// Compute the EIP-712 struct hash for a [`NativeAction`] with its nonce.
pub fn eip712_struct_hash(action: &NativeAction, nonce: u64) -> B256 {
    match action {
        NativeAction::PlaceOrder(p) => hash_place_order(p, nonce),
        NativeAction::PlaceOrderBatch(orders) => hash_place_order_batch(orders, nonce),
        NativeAction::CancelOrder { order_id } => hash_cancel_order(*order_id, nonce),
        NativeAction::CancelAllOrders { market_id } => hash_cancel_all_orders(*market_id, nonce),
        NativeAction::ModifyOrder {
            order_id,
            new_price,
            new_qty,
        } => hash_modify_order(*order_id, *new_price, *new_qty, nonce),
        NativeAction::TransferToPerp { amount } => hash_transfer_to_perp(amount, nonce),
        NativeAction::TransferToSpot { amount } => hash_transfer_to_spot(amount, nonce),
        NativeAction::Withdraw { amount, to } => hash_withdraw(amount, to, nonce),
        NativeAction::Delegate { validator, amount } => hash_delegate(validator, amount, nonce),
        NativeAction::Undelegate { validator, amount } => hash_undelegate(validator, amount, nonce),
        NativeAction::PermanentStake { amount } => hash_permanent_stake(amount, nonce),
        NativeAction::ClaimRewards => hash_claim_rewards(nonce),
        NativeAction::SubmitProposal(proposal) => hash_submit_proposal(proposal, nonce),
        NativeAction::Vote {
            proposal_id,
            option,
        } => hash_vote(*proposal_id, option, nonce),
        NativeAction::SubmitOraclePrices(sub) => hash_submit_oracle_prices(sub, nonce),
        NativeAction::RegisterValidator { pubkey, commission } => {
            hash_register_validator(pubkey, *commission, nonce)
        }
        NativeAction::UpdateCommission { new_rate } => hash_update_commission(*new_rate, nonce),
        NativeAction::JailVote { target } => hash_jail_vote(target, nonce),
        NativeAction::UnjailSelf => hash_unjail_self(nonce),
        NativeAction::RotateValidatorKey { new_pubkey } => {
            hash_rotate_validator_key(new_pubkey, nonce)
        }
        NativeAction::UpdateMarketParams { market_id, params } => {
            hash_update_market_params(*market_id, params, nonce)
        }
        NativeAction::ListMarket(listing) => hash_list_market(listing, nonce),
        NativeAction::DelistMarket { market_id } => hash_delist_market(*market_id, nonce),
        NativeAction::TopUpSelfStake { amount } => hash_top_up_self_stake(amount, nonce),
        NativeAction::CreateSession {
            session_pubkey,
            expiry,
            scope,
        } => hash_create_session(session_pubkey, *expiry, *scope, nonce),
        NativeAction::RevokeSession { session_pubkey } => {
            hash_revoke_session(session_pubkey, nonce)
        }
    }
}

// ---------- order book ----------

/// Encoded byte length of the 9 EIP-712 order fields (9 × 32-byte ABI words).
const PLACE_ORDER_FIELDS_LEN: usize = 9 * 32;

/// ABI-encode the 9 EIP-712 order fields (no typehash, no nonce) into a fixed
/// stack buffer — no heap allocation per order.
///
/// Shared by `hash_place_order` (single, nonce-bearing) and `hash_place_order_item`
/// (batch element, nonce at the batch level) so the two paths cannot diverge. The
/// output is byte-identical to the pre-hoist `append_place_order_fields` (same
/// field order, same encoders), asserted in `place_order_fields_encoding_matches`.
fn encode_place_order_fields(p: &PlaceOrderParams) -> [u8; PLACE_ORDER_FIELDS_LEN] {
    let (coid, has_coid) = p.client_order_id.map_or((0, false), |id| (id, true));
    let mut buf = [0u8; PLACE_ORDER_FIELDS_LEN];
    buf[0..32].copy_from_slice(&encode_u64(p.market_id));
    buf[32..64].copy_from_slice(&encode_bool(p.is_buy));
    buf[64..96].copy_from_slice(&encode_i128(p.price.raw()));
    buf[96..128].copy_from_slice(&encode_i128(p.quantity.raw()));
    buf[128..160].copy_from_slice(&encode_u8(match p.order_type {
        OrderType::Limit => 0,
        OrderType::Market => 1,
        OrderType::StopMarket { .. } => 2,
        OrderType::StopLimit { .. } => 3,
    }));
    buf[160..192].copy_from_slice(&encode_u8(p.time_in_force as u8));
    buf[192..224].copy_from_slice(&encode_bool(p.reduce_only));
    buf[224..256].copy_from_slice(&encode_u64(coid));
    buf[256..288].copy_from_slice(&encode_bool(has_coid));
    buf
}

fn hash_place_order(p: &PlaceOrderParams, nonce: u64) -> B256 {
    static TH: LazyLock<B256> = LazyLock::new(|| {
        keccak256(
            "PlaceOrder(uint64 marketId,bool isBuy,int128 price,int128 quantity,\
             uint8 orderType,uint8 timeInForce,bool reduceOnly,\
             uint64 clientOrderId,bool hasClientOrderId,uint64 nonce)",
        )
    });
    // typehash (32) + 9 fields (288) + nonce (32) = 352 bytes, no heap alloc.
    let mut buf = [0u8; 11 * 32];
    buf[0..32].copy_from_slice(TH.as_slice());
    buf[32..320].copy_from_slice(&encode_place_order_fields(p));
    buf[320..352].copy_from_slice(&encode_u64(nonce));
    keccak256(buf)
}

/// EIP-712 struct hash for one order *inside* a batch (no nonce — the nonce is
/// bound once at the batch level). Distinct typehash from `PlaceOrder` so a single
/// order and a batch element can never collide.
fn hash_place_order_item(p: &PlaceOrderParams) -> B256 {
    static TH: LazyLock<B256> = LazyLock::new(|| {
        keccak256(
            "PlaceOrderItem(uint64 marketId,bool isBuy,int128 price,int128 quantity,\
             uint8 orderType,uint8 timeInForce,bool reduceOnly,\
             uint64 clientOrderId,bool hasClientOrderId)",
        )
    });
    // typehash (32) + 9 fields (288) = 320 bytes on the stack — the per-order Vec
    // that dominated the b400 batch hash path is gone.
    let mut buf = [0u8; 10 * 32];
    buf[0..32].copy_from_slice(TH.as_slice());
    buf[32..320].copy_from_slice(&encode_place_order_fields(p));
    keccak256(buf)
}

/// EIP-712 struct hash for a batch of orders under one signature + one nonce.
///
/// Follows the codebase's array convention (cf. `hash_submit_oracle_prices`):
/// the dynamic array is folded into a single `bytes32 ordersHash =
/// keccak256(item_hash_1 || item_hash_2 || ...)`. `count` is bound explicitly so
/// truncation/extension changes the hash even if it weren't already implied.
fn hash_place_order_batch(orders: &[PlaceOrderParams], nonce: u64) -> B256 {
    static TH: LazyLock<B256> =
        LazyLock::new(|| keccak256("PlaceOrderBatch(bytes32 ordersHash,uint64 count,uint64 nonce)"));
    let mut acc = Vec::with_capacity(orders.len() * 32);
    for p in orders {
        acc.extend_from_slice(&hash_place_order_item(p).0);
    }
    let orders_hash = keccak256(&acc);
    let mut buf = [0u8; 4 * 32];
    buf[0..32].copy_from_slice(TH.as_slice());
    buf[32..64].copy_from_slice(&encode_bytes32(&orders_hash));
    buf[64..96].copy_from_slice(&encode_u64(orders.len() as u64));
    buf[96..128].copy_from_slice(&encode_u64(nonce));
    keccak256(buf)
}

fn hash_cancel_order(order_id: OrderId, nonce: u64) -> B256 {
    static TH: LazyLock<B256> =
        LazyLock::new(|| keccak256("CancelOrder(uint128 orderId,uint64 nonce)"));
    let th = &*TH;
    let mut buf = Vec::with_capacity(3 * 32);
    buf.extend_from_slice(&th.0);
    buf.extend_from_slice(&encode_u128(order_id));
    buf.extend_from_slice(&encode_u64(nonce));
    keccak256(&buf)
}

fn hash_cancel_all_orders(market_id: Option<MarketId>, nonce: u64) -> B256 {
    static TH: LazyLock<B256> =
        LazyLock::new(|| keccak256("CancelAllOrders(uint64 marketId,bool hasMarketId,uint64 nonce)"));
    let th = &*TH;
    let (mid, has) = market_id.map_or((0, false), |id| (id, true));
    let mut buf = Vec::with_capacity(4 * 32);
    buf.extend_from_slice(&th.0);
    buf.extend_from_slice(&encode_u64(mid));
    buf.extend_from_slice(&encode_bool(has));
    buf.extend_from_slice(&encode_u64(nonce));
    keccak256(&buf)
}

fn hash_modify_order(
    order_id: OrderId,
    new_price: Option<FixedPoint>,
    new_qty: Option<FixedPoint>,
    nonce: u64,
) -> B256 {
    static TH: LazyLock<B256> = LazyLock::new(|| {
        keccak256(
            "ModifyOrder(uint128 orderId,int128 newPrice,bool hasNewPrice,\
             int128 newQty,bool hasNewQty,uint64 nonce)",
        )
    });
    let th = &*TH;
    let (price, hp) = new_price.map_or((0, false), |p| (p.raw(), true));
    let (qty, hq) = new_qty.map_or((0, false), |q| (q.raw(), true));
    let mut buf = Vec::with_capacity(7 * 32);
    buf.extend_from_slice(&th.0);
    buf.extend_from_slice(&encode_u128(order_id));
    buf.extend_from_slice(&encode_i128(price));
    buf.extend_from_slice(&encode_bool(hp));
    buf.extend_from_slice(&encode_i128(qty));
    buf.extend_from_slice(&encode_bool(hq));
    buf.extend_from_slice(&encode_u64(nonce));
    keccak256(&buf)
}

// ---------- transfers ----------

fn hash_transfer_to_perp(amount: &U256, nonce: u64) -> B256 {
    static TH: LazyLock<B256> =
        LazyLock::new(|| keccak256("TransferToPerp(uint256 amount,uint64 nonce)"));
    let th = &*TH;
    let mut buf = Vec::with_capacity(3 * 32);
    buf.extend_from_slice(&th.0);
    buf.extend_from_slice(&encode_u256(amount));
    buf.extend_from_slice(&encode_u64(nonce));
    keccak256(&buf)
}

fn hash_transfer_to_spot(amount: &U256, nonce: u64) -> B256 {
    static TH: LazyLock<B256> =
        LazyLock::new(|| keccak256("TransferToSpot(uint256 amount,uint64 nonce)"));
    let th = &*TH;
    let mut buf = Vec::with_capacity(3 * 32);
    buf.extend_from_slice(&th.0);
    buf.extend_from_slice(&encode_u256(amount));
    buf.extend_from_slice(&encode_u64(nonce));
    keccak256(&buf)
}

fn hash_withdraw(amount: &U256, to: &Address, nonce: u64) -> B256 {
    static TH: LazyLock<B256> =
        LazyLock::new(|| keccak256("Withdraw(uint256 amount,address to,uint64 nonce)"));
    let th = &*TH;
    let mut buf = Vec::with_capacity(4 * 32);
    buf.extend_from_slice(&th.0);
    buf.extend_from_slice(&encode_u256(amount));
    buf.extend_from_slice(&encode_address(to));
    buf.extend_from_slice(&encode_u64(nonce));
    keccak256(&buf)
}

// ---------- staking ----------

fn hash_delegate(validator: &Address, amount: &U256, nonce: u64) -> B256 {
    static TH: LazyLock<B256> =
        LazyLock::new(|| keccak256("Delegate(address validator,uint256 amount,uint64 nonce)"));
    let th = &*TH;
    let mut buf = Vec::with_capacity(4 * 32);
    buf.extend_from_slice(&th.0);
    buf.extend_from_slice(&encode_address(validator));
    buf.extend_from_slice(&encode_u256(amount));
    buf.extend_from_slice(&encode_u64(nonce));
    keccak256(&buf)
}

fn hash_undelegate(validator: &Address, amount: &U256, nonce: u64) -> B256 {
    static TH: LazyLock<B256> =
        LazyLock::new(|| keccak256("Undelegate(address validator,uint256 amount,uint64 nonce)"));
    let th = &*TH;
    let mut buf = Vec::with_capacity(4 * 32);
    buf.extend_from_slice(&th.0);
    buf.extend_from_slice(&encode_address(validator));
    buf.extend_from_slice(&encode_u256(amount));
    buf.extend_from_slice(&encode_u64(nonce));
    keccak256(&buf)
}

fn hash_permanent_stake(amount: &U256, nonce: u64) -> B256 {
    static TH: LazyLock<B256> =
        LazyLock::new(|| keccak256("PermanentStake(uint256 amount,uint64 nonce)"));
    let th = &*TH;
    let mut buf = Vec::with_capacity(3 * 32);
    buf.extend_from_slice(&th.0);
    buf.extend_from_slice(&encode_u256(amount));
    buf.extend_from_slice(&encode_u64(nonce));
    keccak256(&buf)
}

fn hash_claim_rewards(nonce: u64) -> B256 {
    static TH: LazyLock<B256> = LazyLock::new(|| keccak256("ClaimRewards(uint64 nonce)"));
    let th = &*TH;
    let mut buf = Vec::with_capacity(2 * 32);
    buf.extend_from_slice(&th.0);
    buf.extend_from_slice(&encode_u64(nonce));
    keccak256(&buf)
}

// ---------- governance ----------

fn hash_submit_proposal(proposal: &Proposal, nonce: u64) -> B256 {
    static TH: LazyLock<B256> = LazyLock::new(|| {
        keccak256("SubmitProposal(string title,string description,bytes32 actionHash,uint64 nonce)")
    });
    let th = &*TH;
    let action_hash = hash_proposal_action(&proposal.action);
    let mut buf = Vec::with_capacity(5 * 32);
    buf.extend_from_slice(&th.0);
    buf.extend_from_slice(&encode_string(&proposal.title));
    buf.extend_from_slice(&encode_string(&proposal.description));
    buf.extend_from_slice(&encode_bytes32(&action_hash));
    buf.extend_from_slice(&encode_u64(nonce));
    keccak256(&buf)
}

fn hash_vote(proposal_id: u64, option: &VoteOption, nonce: u64) -> B256 {
    static TH: LazyLock<B256> =
        LazyLock::new(|| keccak256("Vote(uint64 proposalId,uint8 option,uint64 nonce)"));
    let th = &*TH;
    let opt = match option {
        VoteOption::Yes => 0u8,
        VoteOption::No => 1,
        VoteOption::Abstain => 2,
    };
    let mut buf = Vec::with_capacity(4 * 32);
    buf.extend_from_slice(&th.0);
    buf.extend_from_slice(&encode_u64(proposal_id));
    buf.extend_from_slice(&encode_u8(opt));
    buf.extend_from_slice(&encode_u64(nonce));
    keccak256(&buf)
}

// ---------- oracle ----------

fn hash_submit_oracle_prices(sub: &OracleSubmission, nonce: u64) -> B256 {
    static TH: LazyLock<B256> = LazyLock::new(|| {
        keccak256("SubmitOraclePrices(bytes32 pricesHash,uint64 timestamp,uint64 nonce)")
    });
    let th = &*TH;
    let prices_hash = hash_price_vec(&sub.prices);
    let mut buf = Vec::with_capacity(4 * 32);
    buf.extend_from_slice(&th.0);
    buf.extend_from_slice(&encode_bytes32(&prices_hash));
    buf.extend_from_slice(&encode_u64(sub.timestamp));
    buf.extend_from_slice(&encode_u64(nonce));
    keccak256(&buf)
}

// ---------- validator ----------

fn hash_register_validator(pubkey: &PublicKey, commission: u16, nonce: u64) -> B256 {
    static TH: LazyLock<B256> = LazyLock::new(|| {
        keccak256("RegisterValidator(bytes32 pubkey,uint16 commission,uint64 nonce)")
    });
    let th = &*TH;
    let pk = B256::from(pubkey.0);
    let mut buf = Vec::with_capacity(4 * 32);
    buf.extend_from_slice(&th.0);
    buf.extend_from_slice(&encode_bytes32(&pk));
    buf.extend_from_slice(&encode_u16(commission));
    buf.extend_from_slice(&encode_u64(nonce));
    keccak256(&buf)
}

fn hash_update_commission(new_rate: u16, nonce: u64) -> B256 {
    static TH: LazyLock<B256> =
        LazyLock::new(|| keccak256("UpdateCommission(uint16 newRate,uint64 nonce)"));
    let th = &*TH;
    let mut buf = Vec::with_capacity(3 * 32);
    buf.extend_from_slice(&th.0);
    buf.extend_from_slice(&encode_u16(new_rate));
    buf.extend_from_slice(&encode_u64(nonce));
    keccak256(&buf)
}

fn hash_jail_vote(target: &Address, nonce: u64) -> B256 {
    static TH: LazyLock<B256> = LazyLock::new(|| keccak256("JailVote(address target,uint64 nonce)"));
    let th = &*TH;
    let mut buf = Vec::with_capacity(3 * 32);
    buf.extend_from_slice(&th.0);
    buf.extend_from_slice(&encode_address(target));
    buf.extend_from_slice(&encode_u64(nonce));
    keccak256(&buf)
}

fn hash_unjail_self(nonce: u64) -> B256 {
    static TH: LazyLock<B256> = LazyLock::new(|| keccak256("UnjailSelf(uint64 nonce)"));
    let th = &*TH;
    let mut buf = Vec::with_capacity(2 * 32);
    buf.extend_from_slice(&th.0);
    buf.extend_from_slice(&encode_u64(nonce));
    keccak256(&buf)
}

fn hash_rotate_validator_key(new_pubkey: &PublicKey, nonce: u64) -> B256 {
    static TH: LazyLock<B256> =
        LazyLock::new(|| keccak256("RotateValidatorKey(bytes32 newPubkey,uint64 nonce)"));
    let th = &*TH;
    let pk = B256::from(new_pubkey.0);
    let mut buf = Vec::with_capacity(3 * 32);
    buf.extend_from_slice(&th.0);
    buf.extend_from_slice(&encode_bytes32(&pk));
    buf.extend_from_slice(&encode_u64(nonce));
    keccak256(&buf)
}

// ---------- admin ----------

fn hash_update_market_params(market_id: MarketId, params: &MarketParams, nonce: u64) -> B256 {
    static TH: LazyLock<B256> = LazyLock::new(|| {
        keccak256("UpdateMarketParams(uint64 marketId,bytes32 paramsHash,uint64 nonce)")
    });
    let th = &*TH;
    let ph = hash_market_params(params);
    let mut buf = Vec::with_capacity(4 * 32);
    buf.extend_from_slice(&th.0);
    buf.extend_from_slice(&encode_u64(market_id));
    buf.extend_from_slice(&encode_bytes32(&ph));
    buf.extend_from_slice(&encode_u64(nonce));
    keccak256(&buf)
}

fn hash_list_market(listing: &MarketListing, nonce: u64) -> B256 {
    static TH: LazyLock<B256> =
        LazyLock::new(|| keccak256("ListMarket(bytes32 listingHash,uint64 nonce)"));
    let th = &*TH;
    let lh = hash_market_listing(listing);
    let mut buf = Vec::with_capacity(3 * 32);
    buf.extend_from_slice(&th.0);
    buf.extend_from_slice(&encode_bytes32(&lh));
    buf.extend_from_slice(&encode_u64(nonce));
    keccak256(&buf)
}

fn hash_delist_market(market_id: MarketId, nonce: u64) -> B256 {
    static TH: LazyLock<B256> =
        LazyLock::new(|| keccak256("DelistMarket(uint64 marketId,uint64 nonce)"));
    let th = &*TH;
    let mut buf = Vec::with_capacity(3 * 32);
    buf.extend_from_slice(&th.0);
    buf.extend_from_slice(&encode_u64(market_id));
    buf.extend_from_slice(&encode_u64(nonce));
    keccak256(&buf)
}

// FIX ECON-FIND-15: EIP-712 type hash for TopUpSelfStake.
fn hash_top_up_self_stake(amount: &U256, nonce: u64) -> B256 {
    static TH: LazyLock<B256> =
        LazyLock::new(|| keccak256("TopUpSelfStake(uint256 amount,uint64 nonce)"));
    let th = &*TH;
    let mut buf = Vec::with_capacity(3 * 32);
    buf.extend_from_slice(&th.0);
    buf.extend_from_slice(&encode_u256(amount));
    buf.extend_from_slice(&encode_u64(nonce));
    keccak256(&buf)
}

fn hash_create_session(pubkey: &[u8; 32], expiry: u64, scope: SessionScope, nonce: u64) -> B256 {
    static TH: LazyLock<B256> = LazyLock::new(|| {
        keccak256("CreateSession(bytes32 sessionPubkey,uint64 expiry,uint8 scope,uint64 nonce)")
    });
    let th = &*TH;
    let mut buf = Vec::with_capacity(5 * 32);
    buf.extend_from_slice(&th.0);
    buf.extend_from_slice(pubkey);
    buf.extend_from_slice(&encode_u64(expiry));
    buf.extend_from_slice(&encode_u8(scope as u8));
    buf.extend_from_slice(&encode_u64(nonce));
    keccak256(&buf)
}

fn hash_revoke_session(pubkey: &[u8; 32], nonce: u64) -> B256 {
    static TH: LazyLock<B256> =
        LazyLock::new(|| keccak256("RevokeSession(bytes32 sessionPubkey,uint64 nonce)"));
    let th = &*TH;
    let mut buf = Vec::with_capacity(3 * 32);
    buf.extend_from_slice(&th.0);
    buf.extend_from_slice(pubkey);
    buf.extend_from_slice(&encode_u64(nonce));
    keccak256(&buf)
}

// ---------- nested-type helpers ----------

fn hash_proposal_action(action: &ProposalAction) -> B256 {
    match action {
        ProposalAction::UpdateMarketParams { market_id, params } => {
            let mut buf = Vec::with_capacity(2 * 32);
            buf.extend_from_slice(&encode_u64(*market_id));
            buf.extend_from_slice(&hash_market_params(params).0);
            keccak256(&buf)
        }
        ProposalAction::ListMarket(listing) => hash_market_listing(listing),
        ProposalAction::DelistMarket { market_id } => keccak256(encode_u64(*market_id)),
        ProposalAction::ParameterChange { key, value } => {
            let mut buf = Vec::with_capacity(2 * 32);
            buf.extend_from_slice(&encode_string(key));
            buf.extend_from_slice(&encode_string(value));
            keccak256(&buf)
        }
        ProposalAction::ValidatorRegistration { candidate } => keccak256(encode_address(candidate)),
    }
}

fn hash_market_params(p: &MarketParams) -> B256 {
    let mut buf = Vec::with_capacity(5 * 32);
    buf.extend_from_slice(&encode_i128(p.tick_size.raw()));
    buf.extend_from_slice(&encode_i128(p.lot_size.raw()));
    buf.extend_from_slice(&encode_u64(p.max_leverage as u64));
    buf.extend_from_slice(&encode_u64(p.maintenance_margin_bps as u64));
    buf.extend_from_slice(&encode_u64(p.max_funding_rate_bps as u64));
    keccak256(&buf)
}

fn hash_market_listing(l: &MarketListing) -> B256 {
    let mut buf = Vec::with_capacity(6 * 32);
    buf.extend_from_slice(&encode_string(&l.base_asset));
    buf.extend_from_slice(&encode_string(&l.quote_asset));
    buf.extend_from_slice(&encode_i128(l.tick_size.raw()));
    buf.extend_from_slice(&encode_i128(l.lot_size.raw()));
    buf.extend_from_slice(&encode_u64(l.max_leverage as u64));
    buf.extend_from_slice(&encode_u64(l.maintenance_margin_bps as u64));
    keccak256(&buf)
}

fn hash_price_vec(prices: &[(MarketId, FixedPoint)]) -> B256 {
    let mut buf = Vec::with_capacity(prices.len() * 2 * 32);
    for (market_id, price) in prices {
        buf.extend_from_slice(&encode_u64(*market_id));
        buf.extend_from_slice(&encode_i128(price.raw()));
    }
    keccak256(&buf)
}

// ============================================================================
// Address Recovery
// ============================================================================

/// Derive an Ethereum address from an uncompressed secp256k1 public key.
fn pubkey_to_address(vk: &VerifyingKey) -> Address {
    let uncompressed = vk.to_encoded_point(false);
    // Skip the 0x04 prefix, hash the 64-byte (x || y) coordinates.
    let hash = keccak256(&uncompressed.as_bytes()[1..]);
    Address::from_slice(&hash[12..])
}

/// Recover the signer's Ethereum address from a prehash and our [`Signature`].
fn ecrecover(hash: &B256, sig: &Signature) -> Result<Address, Eip712Error> {
    let mut sig_bytes = [0u8; 64];
    sig_bytes[..32].copy_from_slice(&sig.r);
    sig_bytes[32..].copy_from_slice(&sig.s);

    let k256_sig = k256::ecdsa::Signature::from_slice(&sig_bytes)
        .map_err(|_| Eip712Error::InvalidSignature)?;

    // v ∈ {27, 28} → recovery id ∈ {0, 1}
    let recid =
        RecoveryId::from_byte(sig.v.wrapping_sub(27)).ok_or(Eip712Error::InvalidSignature)?;

    let vk = VerifyingKey::recover_from_prehash(hash.as_slice(), &k256_sig, recid)
        .map_err(|_| Eip712Error::RecoveryFailed)?;

    Ok(pubkey_to_address(&vk))
}

// ============================================================================
// Public API
// ============================================================================

/// Sign a [`NativeAction`] with a secp256k1 private key, producing a
/// [`SignedNativeAction`] with an EIP-712 signature.
pub fn sign_native_action(
    action: NativeAction,
    nonce: u64,
    key: &SigningKey,
) -> SignedNativeAction {
    let domain = eip712_domain_separator();
    let struct_hash = eip712_struct_hash(&action, nonce);
    let signing_hash = eip712_signing_hash(domain, struct_hash);

    let (k256_sig, recid) = key
        .sign_prehash_recoverable(signing_hash.as_slice())
        .expect("signing with a valid key cannot fail");

    let sig_bytes = k256_sig.to_bytes();
    let mut r = [0u8; 32];
    let mut s = [0u8; 32];
    r.copy_from_slice(&sig_bytes[..32]);
    s.copy_from_slice(&sig_bytes[32..]);

    SignedNativeAction {
        action,
        nonce,
        signature: ActionSignature::Eip712(Signature {
            v: recid.to_byte() + 27,
            r,
            s,
        }),
    }
}

/// Sign a native action with an ed25519 **session key** instead of the owner's
/// EIP-712 ECDSA key. Produces the SAME signing hash as [`sign_native_action`]
/// (EIP-712 domain + struct hash) but signs it with ed25519, so the node authorizes
/// it via its batched `verify_batch` + session-state lookup instead of a per-action
/// secp256k1 `ecrecover` — the fast path that makes session keys cheap at ingress.
///
/// The session pubkey must already be registered on-chain (via a `CreateSession`
/// action) and its scope must permit `action`, or the node rejects it at
/// `resolve_sender`. This is a client-side signing helper only — it changes no
/// validation or consensus behavior.
pub fn sign_native_action_with_session(
    action: NativeAction,
    nonce: u64,
    session_key: &ed25519_dalek::SigningKey,
) -> SignedNativeAction {
    use ed25519_dalek::Signer;
    let domain = eip712_domain_separator();
    let struct_hash = eip712_struct_hash(&action, nonce);
    let signing_hash = eip712_signing_hash(domain, struct_hash);
    let sig = session_key.sign(signing_hash.as_slice());
    SignedNativeAction {
        action,
        nonce,
        signature: ActionSignature::Session {
            session_pubkey: session_key.verifying_key().to_bytes(),
            sig: crate::Ed25519Sig(sig.to_bytes()),
        },
    }
}

/// Maximum session key expiry: 24 hours in milliseconds.
pub const MAX_SESSION_EXPIRY_MS: u64 = 24 * 60 * 60 * 1000;

/// Maximum active sessions per address.
pub const MAX_SESSIONS_PER_ADDRESS: usize = 5;

/// Actions that MUST use EIP-712 signature (cannot use session keys).
pub fn requires_eip712(action: &NativeAction) -> bool {
    matches!(
        action,
        NativeAction::CreateSession { .. }
            | NativeAction::RevokeSession { .. }
            | NativeAction::Withdraw { .. }
            | NativeAction::Delegate { .. }
            | NativeAction::Undelegate { .. }
            | NativeAction::PermanentStake { .. }
            | NativeAction::ClaimRewards
    )
}

impl SignedNativeAction {
    /// Recover the sender's Ethereum address from the EIP-712 signature.
    /// Returns error if the signature is a session key (use `resolve_sender` with state instead).
    pub fn recover_sender(&self) -> Result<Address, Eip712Error> {
        match &self.signature {
            ActionSignature::Eip712(sig) => {
                let domain = eip712_domain_separator();
                let struct_hash = eip712_struct_hash(&self.action, self.nonce);
                let signing_hash = eip712_signing_hash(domain, struct_hash);
                ecrecover(&signing_hash, sig)
            }
            ActionSignature::Session { .. } => Err(Eip712Error::RequiresEip712),
        }
    }

    /// Verify ed25519 session signature and return the session pubkey.
    /// Does NOT check session state (expiry, scope) — caller must do that.
    pub fn verify_session_signature(&self) -> Result<[u8; 32], Eip712Error> {
        match &self.signature {
            ActionSignature::Session {
                session_pubkey,
                sig,
            } => {
                let vk = Ed25519VerifyingKey::from_bytes(session_pubkey)
                    .map_err(|_| Eip712Error::SessionSignatureInvalid)?;
                let ed_sig = ed25519_dalek::Signature::from_bytes(&sig.0);
                let domain = eip712_domain_separator();
                let struct_hash = eip712_struct_hash(&self.action, self.nonce);
                let signing_hash = eip712_signing_hash(domain, struct_hash);
                vk.verify(signing_hash.as_slice(), &ed_sig)
                    .map_err(|_| Eip712Error::SessionSignatureInvalid)?;
                Ok(*session_pubkey)
            }
            ActionSignature::Eip712(_) => Err(Eip712Error::InvalidSignature),
        }
    }

    /// Resolve the sender address using state-based session lookup.
    /// For Eip712: recovers ECDSA sender directly.
    /// For Session: verifies ed25519, then looks up session owner via the provided closure.
    pub fn resolve_sender<F>(
        &self,
        current_timestamp: u64,
        session_lookup: F,
    ) -> Result<Address, Eip712Error>
    where
        F: FnOnce(&[u8; 32]) -> Option<crate::SessionData>,
    {
        match &self.signature {
            ActionSignature::Eip712(sig) => {
                let domain = eip712_domain_separator();
                let struct_hash = eip712_struct_hash(&self.action, self.nonce);
                let signing_hash = eip712_signing_hash(domain, struct_hash);
                ecrecover(&signing_hash, sig)
            }
            ActionSignature::Session {
                session_pubkey,
                sig,
            } => {
                if requires_eip712(&self.action) {
                    return Err(Eip712Error::RequiresEip712);
                }
                let vk = Ed25519VerifyingKey::from_bytes(session_pubkey)
                    .map_err(|_| Eip712Error::SessionSignatureInvalid)?;
                let ed_sig = ed25519_dalek::Signature::from_bytes(&sig.0);
                let domain = eip712_domain_separator();
                let struct_hash = eip712_struct_hash(&self.action, self.nonce);
                let signing_hash = eip712_signing_hash(domain, struct_hash);
                vk.verify(signing_hash.as_slice(), &ed_sig)
                    .map_err(|_| Eip712Error::SessionSignatureInvalid)?;
                let session = session_lookup(session_pubkey).ok_or(Eip712Error::SessionNotFound)?;
                if current_timestamp > session.expiry {
                    return Err(Eip712Error::SessionExpired);
                }
                if !session.scope.allows(&self.action) {
                    return Err(Eip712Error::SessionScopeViolation);
                }
                Ok(session.owner)
            }
        }
    }

    /// Full validation: chain ID guard, nonce freshness, and sender recovery.
    ///
    /// `current_time_ms` — current wall-clock time in milliseconds.
    /// `expected_chain_id` — the chain ID this node is configured for.
    pub fn validate(
        &self,
        current_time_ms: u64,
        expected_chain_id: u64,
    ) -> Result<Address, Eip712Error> {
        if expected_chain_id != TORUS_CHAIN_ID {
            return Err(Eip712Error::ChainIdMismatch {
                expected: TORUS_CHAIN_ID,
                got: expected_chain_id,
            });
        }

        // Nonce must be within +-60 s of current time.
        if self.nonce.saturating_add(NONCE_WINDOW_MS) < current_time_ms {
            return Err(Eip712Error::NonceTooOld);
        }
        if self.nonce > current_time_ms.saturating_add(NONCE_WINDOW_MS) {
            return Err(Eip712Error::NonceTooFuture);
        }

        self.recover_sender()
    }

    /// Full validation with session key support.
    pub fn validate_with_sessions<F>(
        &self,
        current_time_ms: u64,
        expected_chain_id: u64,
        session_lookup: F,
    ) -> Result<Address, Eip712Error>
    where
        F: FnOnce(&[u8; 32]) -> Option<crate::SessionData>,
    {
        if expected_chain_id != TORUS_CHAIN_ID {
            return Err(Eip712Error::ChainIdMismatch {
                expected: TORUS_CHAIN_ID,
                got: expected_chain_id,
            });
        }

        if self.nonce.saturating_add(NONCE_WINDOW_MS) < current_time_ms {
            return Err(Eip712Error::NonceTooOld);
        }
        if self.nonce > current_time_ms.saturating_add(NONCE_WINDOW_MS) {
            return Err(Eip712Error::NonceTooFuture);
        }

        self.resolve_sender(current_time_ms, session_lookup)
    }
}

// ============================================================================
// Batch Verification
// ============================================================================

/// Batch-verify native action signatures AND resolve each sender in one pass.
///
/// Returns a vector parallel to `actions`: `Some(sender)` for a valid action
/// (the EIP-712-recovered address, or the session owner), `None` for an invalid
/// one (bad signature, missing/expired/out-of-scope session, or a session key
/// used for an action that requires a full EIP-712 signature).
///
/// The resolved sender is exactly what `SignedNativeAction::resolve_sender`
/// would return, so callers reuse it directly instead of recovering a second
/// time — secp256k1 ecrecover happens once per action, not twice.
///
/// The EIP-712 ecrecovers (the dominant per-action cost) run in parallel via
/// rayon; the result is order-preserving and identical to a serial run. Session
/// verification's per-action work (EIP-712 struct/signing-hash + ed25519 key
/// decompression — the exec_verify hot path for the session-signed workload) also
/// runs in parallel, after a short SEQUENTIAL, deduped pre-pass that calls
/// `session_lookup` once per unique pubkey. That pre-pass keeps the deliberately
/// non-`Sync` (and possibly side-effecting) `session_lookup` off the worker
/// threads — so callers keep their non-`Sync` closures — while the pure hashing
/// fans out. The ed25519 batch is assembled in index order, so `verify_batch`
/// sees an identical message/key sequence and the output is byte-identical to a
/// serial run.
pub fn batch_verify_native_actions(
    actions: &[SignedNativeAction],
    timestamp: u64,
    session_lookup: impl Fn(&[u8; 32]) -> Option<crate::SessionData>,
) -> Vec<Option<Address>> {
    // No exec trust-cache: every action is fully verified — exactly today's
    // behavior. The execution path uses `batch_verify_native_actions_cached`;
    // passing a never-hitting lookup here makes "cache off" provably identical.
    batch_verify_native_actions_cached(actions, timestamp, session_lookup, |_| None)
}

/// Trust-cache-aware variant of [`batch_verify_native_actions`].
///
/// `verified_lookup(key)` returns a previously locally-verified sender for an
/// action's signature-committing [`verified_cache_key`]. On a HIT the secp256k1
/// recover is skipped and the cached sender reused — and because the cache holds
/// only locally-verified senders keyed by the FULL signature, that sender is
/// exactly what a fresh recover would return, so this can never change the
/// resolved sender or fork the chain. A MISS (or a non-cacheable session action,
/// for which `verified_cache_key` returns `None`) falls through to full recover +
/// session resolution. Cache consultation is a sequential pre-pass, kept off the
/// rayon workers exactly like `session_lookup`.
pub fn batch_verify_native_actions_cached(
    actions: &[SignedNativeAction],
    timestamp: u64,
    session_lookup: impl Fn(&[u8; 32]) -> Option<crate::SessionData>,
    verified_lookup: impl Fn(&alloy_primitives::B256) -> Option<Address>,
) -> Vec<Option<Address>> {
    use rayon::prelude::*;

    // Pre-pass (sequential): consult the trust-cache. For a locally-verified
    // EIP-712 action a HIT yields the sender a fresh recover would produce, so the
    // dominant ecrecover below is skipped. `verified_cache_key` returns None for
    // session / non-EIP-712 actions, which are never short-circuited.
    let cache_hits: Vec<Option<Address>> = actions
        .iter()
        .map(|action| crate::verified_cache_key(action).and_then(|k| verified_lookup(&k)))
        .collect();

    // Phase 1 (parallel): recover EIP-712 senders for cache MISSES only. Each
    // ecrecover is pure, independent crypto with no shared/borrowed state; `collect`
    // over an indexed parallel iterator preserves order, so the output is
    // deterministic and identical to a serial run.
    let mut senders: Vec<Option<Address>> = actions
        .par_iter()
        .zip(cache_hits.par_iter())
        .map(|(action, hit)| {
            if hit.is_some() {
                return *hit; // trust-cache HIT: reuse sender, skip recover
            }
            match &action.signature {
                ActionSignature::Eip712(_) => action.recover_sender().ok(),
                ActionSignature::Session { .. } => None,
            }
        })
        .collect();

    // Phase 2: resolve session actions and assemble the ed25519 batch.
    let domain = eip712_domain_separator();

    // Phase 2a — session-resolve pre-pass (SEQUENTIAL, deduped). Consult
    // `session_lookup` exactly ONCE per unique session pubkey, and only for the
    // session actions the serial loop would have consulted (i.e. NOT the
    // `requires_eip712` ones, which it skipped before ever calling lookup). This
    // keeps the deliberately non-`Sync` (and possibly side-effecting, e.g. the
    // app.rs session-owner cache-populate) closure off the rayon workers, and the
    // dedup means N actions sharing one pubkey cost one lookup — the caller's
    // cache-populate is idempotent, so dedup only removes redundant reads and can
    // never change the resolved result. `None` entries are cached too, so an
    // unresolvable pubkey is looked up once, not once per action.
    let mut resolved_sessions: std::collections::HashMap<[u8; 32], Option<crate::SessionData>> =
        std::collections::HashMap::new();
    for action in actions.iter() {
        if let ActionSignature::Session { session_pubkey, .. } = &action.signature {
            if requires_eip712(&action.action) {
                continue;
            }
            if !resolved_sessions.contains_key(session_pubkey) {
                let data = session_lookup(session_pubkey);
                resolved_sessions.insert(*session_pubkey, data);
            }
        }
    }

    // Phase 2b — per-action verify prep (PARALLEL, order-preserving). For each
    // session action, check expiry/scope against the pre-resolved session,
    // decompress the ed25519 verifying key, and compute the EIP-712 struct/signing
    // hash (the dominant keccak cost). Every step is pure and reads only the
    // now-immutable `resolved_sessions` map (`SessionData` is `Sync`), so an
    // indexed `par_iter` collect is deterministic and byte-identical to the serial
    // loop. Non-session / requires_eip712 / unresolved / bad-scope / bad-key
    // actions yield `None` — exactly the serial loop's `continue`/`_ => {}` skips.
    struct EdItem {
        message: Vec<u8>,
        signature: ed25519_dalek::Signature,
        key: Ed25519VerifyingKey,
        owner: Address,
    }
    let per_index: Vec<Option<EdItem>> = actions
        .par_iter()
        .map(|action| {
            let ActionSignature::Session {
                session_pubkey,
                sig,
            } = &action.signature
            else {
                return None; // EIP-712 already resolved in phase 1
            };
            if requires_eip712(&action.action) {
                return None;
            }
            let Some(Some(session)) = resolved_sessions.get(session_pubkey) else {
                return None; // no session / lookup returned None
            };
            if !(timestamp <= session.expiry && session.scope.allows(&action.action)) {
                return None; // expired or out of scope
            }
            let Ok(vk) = Ed25519VerifyingKey::from_bytes(session_pubkey) else {
                return None;
            };
            let struct_hash = eip712_struct_hash(&action.action, action.nonce);
            let signing_hash = eip712_signing_hash(domain, struct_hash);
            Some(EdItem {
                message: signing_hash.0.to_vec(),
                signature: ed25519_dalek::Signature::from_bytes(&sig.0),
                key: vk,
                owner: session.owner,
            })
        })
        .collect();

    // Assemble the ed25519 batch in index order — the SAME push order as the
    // serial loop, so `verify_batch` sees an identical message/key sequence — and
    // set the tentative session-owner sender (cleared below if the batch fails).
    let mut ed_indices = Vec::new();
    let mut ed_messages = Vec::new();
    let mut ed_signatures = Vec::new();
    let mut ed_keys = Vec::new();
    for (i, item) in per_index.into_iter().enumerate() {
        if let Some(item) = item {
            ed_indices.push(i);
            ed_messages.push(item.message);
            ed_signatures.push(item.signature);
            ed_keys.push(item.key);
            senders[i] = Some(item.owner);
        }
    }

    if !ed_indices.is_empty() {
        let msg_refs: Vec<&[u8]> = ed_messages.iter().map(|m| m.as_slice()).collect();
        if ed25519_dalek::verify_batch(&msg_refs, &ed_signatures, &ed_keys).is_err() {
            for j in 0..ed_indices.len() {
                if ed_keys[j]
                    .verify(&ed_messages[j], &ed_signatures[j])
                    .is_err()
                {
                    senders[ed_indices[j]] = None;
                }
            }
        }
    }

    senders
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{OrderType, SessionData, SessionScope, TimeInForce};

    fn test_key() -> SigningKey {
        let mut bytes = [0u8; 32];
        bytes[31] = 1;
        SigningKey::from_slice(&bytes).unwrap()
    }

    fn test_key_2() -> SigningKey {
        let mut bytes = [0u8; 32];
        bytes[31] = 2;
        SigningKey::from_slice(&bytes).unwrap()
    }

    fn signer_address(key: &SigningKey) -> Address {
        let vk = key.verifying_key();
        pubkey_to_address(vk)
    }

    const TEST_NONCE: u64 = 1_700_000_000_000;

    // --- determinism ---

    #[test]
    fn domain_separator_is_deterministic_and_nonzero() {
        let a = eip712_domain_separator();
        let b = eip712_domain_separator();
        assert_eq!(a, b);
        assert_ne!(a, B256::ZERO);
    }

    // --- Feature #14: hoisted-constant-keccak byte-identity oracles ---
    //
    // Each test recomputes the expected hash from raw `keccak256` calls on the
    // literal type strings (independent of the hoisted `LazyLock` statics and the
    // stack-buffer field encoder), so a HIT proves the hoist is byte-identical to
    // the pre-hoist inline computation. The unchanged ABI leaf encoders
    // (`encode_u64` etc.) are reused only to assemble the oracle — they are not
    // the code under test.

    /// A representative order with a client order id (exercises `has_coid = true`).
    fn oracle_order() -> PlaceOrderParams {
        PlaceOrderParams {
            market_id: 7,
            is_buy: true,
            price: FixedPoint::from_raw(-1_234_567_890),
            quantity: FixedPoint::from_raw(98_765_432_100),
            order_type: OrderType::Limit,
            time_in_force: TimeInForce::GTC,
            reduce_only: true,
            client_order_id: Some(0xDEAD_BEEF),
        }
    }

    /// An order without a client order id (exercises `has_coid = false`).
    fn oracle_order_no_coid() -> PlaceOrderParams {
        PlaceOrderParams {
            market_id: 3,
            is_buy: false,
            price: FixedPoint::from_raw(500),
            quantity: FixedPoint::from_raw(9),
            order_type: OrderType::Market,
            time_in_force: TimeInForce::IOC,
            reduce_only: false,
            client_order_id: None,
        }
    }

    /// Verbatim pre-hoist field assembly (the old `append_place_order_fields`),
    /// used as the independent oracle for the new stack-buffer encoder.
    fn oracle_fields(p: &PlaceOrderParams) -> Vec<u8> {
        let (coid, has_coid) = p.client_order_id.map_or((0, false), |id| (id, true));
        let mut buf = Vec::new();
        buf.extend_from_slice(&encode_u64(p.market_id));
        buf.extend_from_slice(&encode_bool(p.is_buy));
        buf.extend_from_slice(&encode_i128(p.price.raw()));
        buf.extend_from_slice(&encode_i128(p.quantity.raw()));
        buf.extend_from_slice(&encode_u8(match p.order_type {
            OrderType::Limit => 0,
            OrderType::Market => 1,
            OrderType::StopMarket { .. } => 2,
            OrderType::StopLimit { .. } => 3,
        }));
        buf.extend_from_slice(&encode_u8(p.time_in_force as u8));
        buf.extend_from_slice(&encode_bool(p.reduce_only));
        buf.extend_from_slice(&encode_u64(coid));
        buf.extend_from_slice(&encode_bool(has_coid));
        buf
    }

    #[test]
    fn hoisted_domain_separator_matches_fresh() {
        // Independent 5-field recomputation via raw keccak (no hoisted static).
        let type_hash = keccak256(
            "EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)",
        );
        let mut buf = Vec::new();
        buf.extend_from_slice(&type_hash.0);
        buf.extend_from_slice(&keccak256("Torus").0);
        buf.extend_from_slice(&keccak256("1").0);
        buf.extend_from_slice(&U256::from(TORUS_CHAIN_ID).to_be_bytes::<32>());
        buf.extend_from_slice(&[0u8; 32]); // address(0), left-padded to 32 bytes
        assert_eq!(eip712_domain_separator(), keccak256(&buf));
    }

    #[test]
    fn place_order_fields_encoding_matches() {
        for p in [oracle_order(), oracle_order_no_coid()] {
            let got = encode_place_order_fields(&p);
            assert_eq!(got.len(), 9 * 32, "9 ABI words");
            assert_eq!(got.as_slice(), oracle_fields(&p).as_slice());
        }
    }

    #[test]
    fn hoisted_place_order_struct_hash_matches_fresh() {
        let p = oracle_order();
        let th = keccak256(
            "PlaceOrder(uint64 marketId,bool isBuy,int128 price,int128 quantity,\
             uint8 orderType,uint8 timeInForce,bool reduceOnly,\
             uint64 clientOrderId,bool hasClientOrderId,uint64 nonce)",
        );
        let mut buf = Vec::new();
        buf.extend_from_slice(&th.0);
        buf.extend_from_slice(&oracle_fields(&p));
        buf.extend_from_slice(&encode_u64(TEST_NONCE));
        let expected = keccak256(&buf);
        assert_eq!(
            eip712_struct_hash(&NativeAction::PlaceOrder(p), TEST_NONCE),
            expected
        );
    }

    #[test]
    fn hoisted_place_order_batch_struct_hash_matches_fresh() {
        let orders = vec![oracle_order(), oracle_order_no_coid(), oracle_order()];

        // Fresh per-item hashes (PlaceOrderItem typehash, no nonce).
        let item_th = keccak256(
            "PlaceOrderItem(uint64 marketId,bool isBuy,int128 price,int128 quantity,\
             uint8 orderType,uint8 timeInForce,bool reduceOnly,\
             uint64 clientOrderId,bool hasClientOrderId)",
        );
        let mut acc = Vec::new();
        for p in &orders {
            let mut item = Vec::new();
            item.extend_from_slice(&item_th.0);
            item.extend_from_slice(&oracle_fields(p));
            acc.extend_from_slice(&keccak256(&item).0);
        }
        let orders_hash = keccak256(&acc);

        let batch_th = keccak256("PlaceOrderBatch(bytes32 ordersHash,uint64 count,uint64 nonce)");
        let mut buf = Vec::new();
        buf.extend_from_slice(&batch_th.0);
        buf.extend_from_slice(&orders_hash.0);
        buf.extend_from_slice(&encode_u64(orders.len() as u64));
        buf.extend_from_slice(&encode_u64(TEST_NONCE));
        let expected = keccak256(&buf);

        assert_eq!(
            eip712_struct_hash(&NativeAction::PlaceOrderBatch(orders), TEST_NONCE),
            expected
        );
    }

    #[test]
    fn hoisted_cancel_order_struct_hash_matches_fresh() {
        let th = keccak256("CancelOrder(uint128 orderId,uint64 nonce)");
        let mut buf = Vec::new();
        buf.extend_from_slice(&th.0);
        buf.extend_from_slice(&encode_u128(42));
        buf.extend_from_slice(&encode_u64(TEST_NONCE));
        let expected = keccak256(&buf);
        assert_eq!(
            eip712_struct_hash(&NativeAction::CancelOrder { order_id: 42 }, TEST_NONCE),
            expected
        );
    }

    #[test]
    fn struct_hash_deterministic() {
        let action = NativeAction::CancelOrder { order_id: 42 };
        let h1 = eip712_struct_hash(&action, TEST_NONCE);
        let h2 = eip712_struct_hash(&action, TEST_NONCE);
        assert_eq!(h1, h2);
        assert_ne!(h1, B256::ZERO);
    }

    #[test]
    fn different_nonce_different_hash() {
        let action = NativeAction::CancelOrder { order_id: 42 };
        assert_ne!(
            eip712_struct_hash(&action, 1000),
            eip712_struct_hash(&action, 2000),
        );
    }

    #[test]
    fn different_action_different_hash() {
        let h1 = eip712_struct_hash(&NativeAction::CancelOrder { order_id: 1 }, TEST_NONCE);
        let h2 = eip712_struct_hash(&NativeAction::CancelOrder { order_id: 2 }, TEST_NONCE);
        assert_ne!(h1, h2);
    }

    #[test]
    fn different_variant_different_hash() {
        let h1 = eip712_struct_hash(&NativeAction::ClaimRewards, TEST_NONCE);
        let h2 = eip712_struct_hash(&NativeAction::UnjailSelf, TEST_NONCE);
        assert_ne!(h1, h2);
    }

    // --- round-trip: sign -> recover ---

    #[test]
    fn sign_and_recover_place_order() {
        let key = test_key();
        let expected = signer_address(&key);

        let action = NativeAction::PlaceOrder(PlaceOrderParams {
            market_id: 1,
            is_buy: true,
            price: FixedPoint::from_raw(1_000_000_000),
            quantity: FixedPoint::from_raw(100_000_000),
            order_type: OrderType::Limit,
            time_in_force: TimeInForce::GTC,
            reduce_only: false,
            client_order_id: Some(42),
        });

        let signed = sign_native_action(action, TEST_NONCE, &key);
        assert_eq!(signed.recover_sender().unwrap(), expected);
    }

    #[test]
    fn sign_and_recover_place_order_batch_single_signature() {
        let key = test_key();
        let expected = signer_address(&key);

        let mk = |market_id: u64, is_buy: bool, coid: Option<u64>| PlaceOrderParams {
            market_id,
            is_buy,
            price: FixedPoint::from_raw(1_000_000_000),
            quantity: FixedPoint::from_raw(100_000_000),
            order_type: OrderType::Limit,
            time_in_force: TimeInForce::GTC,
            reduce_only: false,
            client_order_id: coid,
        };
        let orders = vec![
            mk(1, true, Some(1)),
            mk(2, false, Some(2)),
            mk(3, true, None),
        ];

        // ONE signature covers all N orders (the throughput keystone).
        let signed = sign_native_action(
            NativeAction::PlaceOrderBatch(orders.clone()),
            TEST_NONCE,
            &key,
        );
        assert_eq!(signed.recover_sender().unwrap(), expected);

        // Tampering with ANY order in the batch breaks the signature
        // (ecrecover yields a different address, so it won't match `expected`).
        let mut tampered = signed.clone();
        if let NativeAction::PlaceOrderBatch(ref mut v) = tampered.action {
            v[1].price = FixedPoint::from_raw(999_999_999);
        }
        assert_ne!(tampered.recover_sender().unwrap(), expected);

        // Reordering the batch also invalidates it (order is signed).
        let mut reordered = signed.clone();
        if let NativeAction::PlaceOrderBatch(ref mut v) = reordered.action {
            v.swap(0, 1);
        }
        assert_ne!(reordered.recover_sender().unwrap(), expected);

        // A batch-of-one is a distinct EIP-712 struct from the single PlaceOrder
        // of the same order (different typehash) — no cross-variant collision.
        let single = NativeAction::PlaceOrder(orders[0].clone());
        let batch_of_one = NativeAction::PlaceOrderBatch(vec![orders[0].clone()]);
        assert_ne!(
            eip712_struct_hash(&single, TEST_NONCE),
            eip712_struct_hash(&batch_of_one, TEST_NONCE),
        );
    }

    #[test]
    fn sign_and_recover_all_variants() {
        let key = test_key();
        let expected = signer_address(&key);

        let actions: Vec<NativeAction> = vec![
            NativeAction::PlaceOrder(PlaceOrderParams {
                market_id: 1,
                is_buy: false,
                price: FixedPoint::ONE,
                quantity: FixedPoint::ONE,
                order_type: OrderType::Market,
                time_in_force: TimeInForce::IOC,
                reduce_only: true,
                client_order_id: None,
            }),
            NativeAction::CancelOrder { order_id: 999 },
            NativeAction::CancelAllOrders { market_id: Some(5) },
            NativeAction::CancelAllOrders { market_id: None },
            NativeAction::ModifyOrder {
                order_id: 456,
                new_price: Some(FixedPoint::ONE),
                new_qty: None,
            },
            NativeAction::TransferToPerp {
                amount: U256::from(1000),
            },
            NativeAction::TransferToSpot {
                amount: U256::from(2000),
            },
            NativeAction::Withdraw {
                amount: U256::from(500),
                to: Address::ZERO,
            },
            NativeAction::Delegate {
                validator: Address::ZERO,
                amount: U256::from(100),
            },
            NativeAction::Undelegate {
                validator: Address::ZERO,
                amount: U256::from(50),
            },
            NativeAction::PermanentStake {
                amount: U256::from(1000),
            },
            NativeAction::ClaimRewards,
            NativeAction::SubmitProposal(Proposal {
                title: "Test".into(),
                description: "A test proposal".into(),
                action: ProposalAction::DelistMarket { market_id: 1 },
            }),
            NativeAction::Vote {
                proposal_id: 1,
                option: VoteOption::Yes,
            },
            NativeAction::SubmitOraclePrices(OracleSubmission {
                prices: vec![(1, FixedPoint::ONE), (2, FixedPoint::from_raw(50_000_000))],
                timestamp: TEST_NONCE,
            }),
            NativeAction::RegisterValidator {
                pubkey: PublicKey([0xab; 32]),
                commission: 500,
            },
            NativeAction::UpdateCommission { new_rate: 300 },
            NativeAction::JailVote {
                target: Address::ZERO,
            },
            NativeAction::UnjailSelf,
            NativeAction::UpdateMarketParams {
                market_id: 1,
                params: MarketParams {
                    tick_size: FixedPoint::ONE,
                    lot_size: FixedPoint::ONE,
                    max_leverage: 20,
                    maintenance_margin_bps: 500,
                    max_funding_rate_bps: 100,
                },
            },
            NativeAction::ListMarket(MarketListing {
                base_asset: "BTC".into(),
                quote_asset: "USD".into(),
                tick_size: FixedPoint::ONE,
                lot_size: FixedPoint::ONE,
                max_leverage: 50,
                maintenance_margin_bps: 300,
            }),
            NativeAction::DelistMarket { market_id: 99 },
        ];

        for (i, action) in actions.into_iter().enumerate() {
            let signed = sign_native_action(action, TEST_NONCE, &key);
            let recovered = signed.recover_sender().unwrap();
            assert_eq!(recovered, expected, "variant index {i} failed round-trip");
        }
    }

    // --- different signers ---

    #[test]
    fn different_keys_produce_different_addresses() {
        let k1 = test_key();
        let k2 = test_key_2();

        let action = NativeAction::ClaimRewards;
        let s1 = sign_native_action(action.clone(), TEST_NONCE, &k1);
        let s2 = sign_native_action(action, TEST_NONCE, &k2);

        assert_ne!(s1.recover_sender().unwrap(), s2.recover_sender().unwrap());
    }

    // --- tamper detection ---

    #[test]
    fn tampered_action_recovers_wrong_address() {
        let key = test_key();
        let expected = signer_address(&key);

        let signed = sign_native_action(
            NativeAction::TransferToPerp {
                amount: U256::from(100),
            },
            TEST_NONCE,
            &key,
        );
        assert_eq!(signed.recover_sender().unwrap(), expected);

        // Change the amount — the recovered address should differ.
        let tampered = SignedNativeAction {
            action: NativeAction::TransferToPerp {
                amount: U256::from(999),
            },
            nonce: signed.nonce,
            signature: signed.signature.clone(),
        };
        let addr = tampered.recover_sender().unwrap();
        assert_ne!(addr, expected);
    }

    // --- nonce validation ---

    #[test]
    fn validate_nonce_too_old() {
        let key = test_key();
        let now = 1_700_000_100_000u64;
        let stale = now - NONCE_WINDOW_MS - 1;

        let signed = sign_native_action(NativeAction::ClaimRewards, stale, &key);
        assert_eq!(
            signed.validate(now, TORUS_CHAIN_ID),
            Err(Eip712Error::NonceTooOld)
        );
    }

    #[test]
    fn validate_nonce_too_future() {
        let key = test_key();
        let now = 1_700_000_000_000u64;
        let future = now + NONCE_WINDOW_MS + 1;

        let signed = sign_native_action(NativeAction::ClaimRewards, future, &key);
        assert_eq!(
            signed.validate(now, TORUS_CHAIN_ID),
            Err(Eip712Error::NonceTooFuture)
        );
    }

    #[test]
    fn validate_nonce_within_window() {
        let key = test_key();
        let now = 1_700_000_000_000u64;

        // Exact current time.
        let s = sign_native_action(NativeAction::ClaimRewards, now, &key);
        assert!(s.validate(now, TORUS_CHAIN_ID).is_ok());

        // Boundary: exactly 60 s in the past.
        let s = sign_native_action(NativeAction::ClaimRewards, now - NONCE_WINDOW_MS, &key);
        assert!(s.validate(now, TORUS_CHAIN_ID).is_ok());

        // Boundary: exactly 60 s in the future.
        let s = sign_native_action(NativeAction::ClaimRewards, now + NONCE_WINDOW_MS, &key);
        assert!(s.validate(now, TORUS_CHAIN_ID).is_ok());
    }

    // --- chain ID ---

    #[test]
    fn validate_chain_id_mismatch() {
        let key = test_key();
        let signed = sign_native_action(NativeAction::ClaimRewards, TEST_NONCE, &key);
        assert_eq!(
            signed.validate(TEST_NONCE, 1),
            Err(Eip712Error::ChainIdMismatch {
                expected: TORUS_CHAIN_ID,
                got: 1,
            })
        );
    }

    #[test]
    fn validate_correct_chain_id_succeeds() {
        let key = test_key();
        let signed = sign_native_action(NativeAction::ClaimRewards, TEST_NONCE, &key);
        let addr = signed.validate(TEST_NONCE, TORUS_CHAIN_ID).unwrap();
        assert_eq!(addr, signer_address(&key));
    }

    // --- signature encoding ---

    #[test]
    fn signature_v_is_27_or_28() {
        let key = test_key();
        let signed = sign_native_action(NativeAction::ClaimRewards, TEST_NONCE, &key);
        match &signed.signature {
            ActionSignature::Eip712(sig) => assert!(sig.v == 27 || sig.v == 28),
            _ => panic!("expected Eip712 signature"),
        }
    }

    // --- batch verification ---

    fn sign_action_with_session(
        action: NativeAction,
        nonce: u64,
        session_key: &ed25519_dalek::SigningKey,
    ) -> SignedNativeAction {
        // Delegates to the production signer so the tests exercise the public API.
        sign_native_action_with_session(action, nonce, session_key)
    }

    fn make_session(owner: Address) -> SessionData {
        SessionData {
            owner,
            expiry: u64::MAX,
            scope: SessionScope::Trading,
            created_at: 0,
        }
    }

    #[test]
    fn sign_native_action_with_session_resolves_to_owner() {
        // The bench's lever-1 primitive: an ed25519 session key signs the SAME
        // EIP-712 hash, and the chain resolves it to the registered owner WITHOUT
        // a per-action secp256k1 ecrecover (the ingress-verify cost we're killing).
        let ed_key = ed25519_dalek::SigningKey::from_bytes(&[7u8; 32]);
        let pubkey = ed_key.verifying_key().to_bytes();
        let owner = Address::from([0x22; 20]);
        let session = SessionData {
            owner,
            expiry: u64::MAX,
            scope: SessionScope::Full,
            created_at: 0,
        };
        let signed = sign_native_action_with_session(
            NativeAction::CancelOrder { order_id: 7 },
            TEST_NONCE,
            &ed_key,
        );
        // Carries a valid ed25519 signature over the EIP-712 signing hash.
        assert_eq!(signed.verify_session_signature().unwrap(), pubkey);
        // Resolves to the registered owner via the session lookup — no ecrecover.
        let got = signed
            .resolve_sender(0, |pk| (pk == &pubkey).then(|| session.clone()))
            .expect("session-signed action resolves to owner");
        assert_eq!(got, owner);
    }

    /// O2/G3: least-privilege market makers must be able to batch under
    /// Trading scope; the narrow TransfersOnly scope must still reject.
    /// Covers BOTH enforcement points: `resolve_sender` (ingress, :823) and
    /// `batch_verify_native_actions` (exec/consensus, :985) — one `allows()`.
    #[test]
    fn trading_scope_allows_place_order_batch_transfers_only_rejects() {
        let ed_key = ed25519_dalek::SigningKey::from_bytes(&[44u8; 32]);
        let pubkey = ed_key.verifying_key().to_bytes();
        let owner = Address::from([0x44; 20]);

        let params = crate::PlaceOrderParams {
            market_id: 1,
            is_buy: true,
            price: FixedPoint::from_raw(6_500_000_000_000),
            quantity: FixedPoint::from_raw(10_000_000),
            order_type: OrderType::Limit,
            time_in_force: TimeInForce::GTC,
            reduce_only: false,
            client_order_id: None,
        };
        let signed = sign_action_with_session(
            NativeAction::PlaceOrderBatch(vec![params.clone(), params]),
            TEST_NONCE,
            &ed_key,
        );

        // Ingress path: Trading scope resolves to the owner.
        let trading = make_session(owner); // scope: Trading
        let got = signed
            .resolve_sender(0, |pk| (pk == &pubkey).then(|| trading.clone()))
            .expect("Trading scope must allow PlaceOrderBatch");
        assert_eq!(got, owner);

        // Exec/consensus path agrees.
        let senders =
            batch_verify_native_actions(std::slice::from_ref(&signed), TEST_NONCE, |pk| {
                (pk == &pubkey).then(|| trading.clone())
            });
        assert_eq!(senders[0], Some(owner));

        // TransfersOnly still rejects with the precise scope error.
        let transfers = SessionData {
            owner,
            expiry: u64::MAX,
            scope: SessionScope::TransfersOnly,
            created_at: 0,
        };
        let err = signed
            .resolve_sender(0, |pk| (pk == &pubkey).then(|| transfers.clone()))
            .unwrap_err();
        assert!(matches!(err, Eip712Error::SessionScopeViolation));
    }

    // ------------------------------------------------------------------
    // Session-owner cache: prove `batch_verify_native_actions_cached` is
    // TRANSPARENT to a memoizing session_lookup (the P3 session-owner cache).
    // A HIT (cache) must yield byte-identical accept/reject + resolved owner to a
    // MISS (direct get_session), and must STILL enforce expiry / scope / ed25519.
    // The memoizing closure below mirrors the production one in app.rs exactly:
    // HIT -> cached SessionData; MISS -> authoritative store + populate.
    // ------------------------------------------------------------------

    /// Authoritative "state DB" for tests: session_pubkey -> SessionData.
    type SessionStore = std::collections::HashMap<[u8; 32], SessionData>;

    /// Build a memoizing session_lookup identical in shape to the app.rs closure.
    /// `db_reads` counts MISSes (authoritative reads) so a test can assert that N
    /// same-session actions cost exactly ONE read (1 miss + N-1 hits).
    fn memoizing_lookup<'a>(
        store: &'a SessionStore,
        cache: &'a std::cell::RefCell<SessionStore>,
        db_reads: &'a std::cell::Cell<usize>,
    ) -> impl Fn(&[u8; 32]) -> Option<SessionData> + 'a {
        move |pk| {
            if let Some(d) = cache.borrow().get(pk) {
                return Some(d.clone()); // HIT: no DB read
            }
            db_reads.set(db_reads.get() + 1); // MISS: authoritative read
            let d = store.get(pk).cloned()?;
            cache.borrow_mut().insert(*pk, d.clone());
            Some(d)
        }
    }

    fn sign_order(nonce: u64, ed_key: &ed25519_dalek::SigningKey) -> SignedNativeAction {
        sign_action_with_session(
            NativeAction::CancelOrder {
                order_id: nonce as u128,
            },
            nonce,
            ed_key,
        )
    }

    /// (i) Same session across N actions => exactly 1 DB read (1 miss + N-1 hits),
    /// all resolve to the identical owner, and the cached run is byte-identical to
    /// the uncached (direct-lookup) run.
    #[test]
    fn session_cache_n_actions_one_read_identical_to_uncached() {
        let ed_key = ed25519_dalek::SigningKey::from_bytes(&[9u8; 32]);
        let pubkey = ed_key.verifying_key().to_bytes();
        let owner = Address::from([0xAB; 20]);

        let mut store = SessionStore::new();
        store.insert(pubkey, make_session(owner)); // scope: Trading, expiry: MAX

        let actions: Vec<SignedNativeAction> =
            (1..=5).map(|n| sign_order(n, &ed_key)).collect();
        let ts = 1_000;

        // Uncached reference: direct lookup, every call reads the store.
        let uncached =
            batch_verify_native_actions(&actions, ts, |pk| store.get(pk).cloned());

        // Cached: memoizing lookup.
        let cache = std::cell::RefCell::new(SessionStore::new());
        let db_reads = std::cell::Cell::new(0usize);
        let cached = batch_verify_native_actions(
            &actions,
            ts,
            memoizing_lookup(&store, &cache, &db_reads),
        );

        assert_eq!(cached, uncached, "cache HIT run must equal uncached run");
        assert!(
            cached.iter().all(|s| *s == Some(owner)),
            "every action resolves to the identical owner"
        );
        assert_eq!(
            db_reads.get(),
            1,
            "5 same-session actions => exactly 1 authoritative read (1 miss + 4 hits)"
        );
    }

    /// (ii) An EXPIRED session is rejected on the HIT path exactly as uncached — a
    /// cached SessionData does NOT bypass the `timestamp <= expiry` check.
    #[test]
    fn session_cache_hit_rejects_expired_exactly_as_uncached() {
        let ed_key = ed25519_dalek::SigningKey::from_bytes(&[11u8; 32]);
        let pubkey = ed_key.verifying_key().to_bytes();
        let owner = Address::from([0xCD; 20]);
        let expiry = 5_000u64;

        let mut store = SessionStore::new();
        store.insert(
            pubkey,
            SessionData {
                owner,
                expiry,
                scope: SessionScope::Trading,
                created_at: 0,
            },
        );

        let action = sign_order(1, &ed_key);
        let cache = std::cell::RefCell::new(SessionStore::new());
        let db_reads = std::cell::Cell::new(0usize);

        // First: verify BEFORE expiry -> accepted, populates the cache.
        let before = batch_verify_native_actions(
            std::slice::from_ref(&action),
            expiry, // timestamp == expiry: still valid (<=)
            memoizing_lookup(&store, &cache, &db_reads),
        );
        assert_eq!(before[0], Some(owner), "valid before/at expiry");
        assert_eq!(db_reads.get(), 1, "populated from the DB");

        // Now verify AFTER expiry. The entry is a cache HIT (no new DB read), yet the
        // action must be REJECTED because expiry is re-checked against the cached data.
        let after_cached = batch_verify_native_actions(
            std::slice::from_ref(&action),
            expiry + 1,
            memoizing_lookup(&store, &cache, &db_reads),
        );
        assert_eq!(db_reads.get(), 1, "second verify was a cache HIT (no DB read)");
        assert_eq!(
            after_cached[0], None,
            "expired session rejected on the HIT path"
        );

        // Identical to the uncached path at the same timestamp.
        let after_uncached = batch_verify_native_actions(
            std::slice::from_ref(&action),
            expiry + 1,
            |pk| store.get(pk).cloned(),
        );
        assert_eq!(after_cached, after_uncached, "hit == miss for expired");
    }

    /// (iii) A forged action carrying a VALID session pubkey but a BAD ed25519
    /// signature is rejected on the HIT path — caching owner-resolution never skips
    /// signature verification.
    #[test]
    fn session_cache_hit_rejects_forged_ed25519_signature() {
        let ed_key = ed25519_dalek::SigningKey::from_bytes(&[13u8; 32]);
        let pubkey = ed_key.verifying_key().to_bytes();
        let owner = Address::from([0xEF; 20]);

        let mut store = SessionStore::new();
        store.insert(pubkey, make_session(owner));

        // Pre-warm the cache with a legitimate action so the session is a HIT.
        let legit = sign_order(1, &ed_key);
        let cache = std::cell::RefCell::new(SessionStore::new());
        let db_reads = std::cell::Cell::new(0usize);
        let warm = batch_verify_native_actions(
            std::slice::from_ref(&legit),
            1_000,
            memoizing_lookup(&store, &cache, &db_reads),
        );
        assert_eq!(warm[0], Some(owner));
        assert_eq!(db_reads.get(), 1);

        // Forge: keep the (cached) session pubkey, corrupt the ed25519 signature.
        let mut forged = sign_order(2, &ed_key);
        if let ActionSignature::Session { sig, .. } = &mut forged.signature {
            sig.0[0] ^= 0xFF;
        } else {
            panic!("expected a session signature");
        }

        let out = batch_verify_native_actions(
            std::slice::from_ref(&forged),
            1_000,
            memoizing_lookup(&store, &cache, &db_reads),
        );
        assert_eq!(db_reads.get(), 1, "session pubkey was a cache HIT (no DB read)");
        assert_eq!(
            out[0], None,
            "forged ed25519 rejected even though the session pubkey HIT the cache"
        );
    }

    /// (ii-b) A REVOKED session is rejected once the cache entry is invalidated —
    /// exactly as uncached. Models app.rs's post-flush `invalidate_session`: the
    /// authoritative store deletes the session AND the cache entry is removed, so
    /// the next verify MISSes and resolves to None (rejected). Also asserts that
    /// WITHOUT invalidation a stale HIT would wrongly accept — proving invalidation
    /// is the load-bearing correctness mechanism.
    #[test]
    fn session_cache_revoked_rejected_after_invalidation() {
        let ed_key = ed25519_dalek::SigningKey::from_bytes(&[15u8; 32]);
        let pubkey = ed_key.verifying_key().to_bytes();
        let owner = Address::from([0x77; 20]);

        let mut store = SessionStore::new();
        store.insert(pubkey, make_session(owner));

        let action = sign_order(1, &ed_key);
        let cache = std::cell::RefCell::new(SessionStore::new());
        let db_reads = std::cell::Cell::new(0usize);

        // Warm the cache with a valid resolution.
        let warm = batch_verify_native_actions(
            std::slice::from_ref(&action),
            1_000,
            memoizing_lookup(&store, &cache, &db_reads),
        );
        assert_eq!(warm[0], Some(owner));

        // Revoke: authoritative delete_session.
        store.remove(&pubkey);

        // Danger demo: if the cache is NOT invalidated, a stale HIT wrongly accepts.
        let stale = batch_verify_native_actions(
            std::slice::from_ref(&action),
            1_000,
            memoizing_lookup(&store, &cache, &db_reads),
        );
        assert_eq!(
            stale[0],
            Some(owner),
            "un-invalidated cache would serve a revoked session (why invalidation is required)"
        );

        // Correct behavior: invalidate (app.rs post-flush) => next verify MISSes =>
        // authoritative store returns None => rejected, identical to uncached.
        cache.borrow_mut().remove(&pubkey);
        let db_reads2 = std::cell::Cell::new(0usize);
        let after = batch_verify_native_actions(
            std::slice::from_ref(&action),
            1_000,
            memoizing_lookup(&store, &cache, &db_reads2),
        );
        let uncached = batch_verify_native_actions(
            std::slice::from_ref(&action),
            1_000,
            |pk| store.get(pk).cloned(),
        );
        assert_eq!(after[0], None, "revoked session rejected after invalidation");
        assert_eq!(after, uncached, "hit(invalidated) == miss for revoked");
        assert_eq!(db_reads2.get(), 1, "invalidation forced a fresh authoritative read");
    }

    /// (iv) Determinism: for a MIXED batch (valid / expired / scope-violation /
    /// forged), the memoized (cache) run is element-for-element identical to the
    /// uncached run.
    #[test]
    fn session_cache_determinism_hit_equals_miss_mixed_batch() {
        let key_ok = ed25519_dalek::SigningKey::from_bytes(&[21u8; 32]);
        let key_exp = ed25519_dalek::SigningKey::from_bytes(&[22u8; 32]);
        let key_scope = ed25519_dalek::SigningKey::from_bytes(&[23u8; 32]);
        let key_forge = ed25519_dalek::SigningKey::from_bytes(&[24u8; 32]);
        let pk_ok = key_ok.verifying_key().to_bytes();
        let pk_exp = key_exp.verifying_key().to_bytes();
        let pk_scope = key_scope.verifying_key().to_bytes();
        let pk_forge = key_forge.verifying_key().to_bytes();
        let owner = Address::from([0x33; 20]);
        let ts = 10_000u64;

        let mut store = SessionStore::new();
        store.insert(pk_ok, make_session(owner)); // Trading, MAX expiry
        store.insert(
            pk_exp,
            SessionData {
                owner,
                expiry: ts - 1, // already expired at ts
                scope: SessionScope::Trading,
                created_at: 0,
            },
        );
        store.insert(
            pk_scope,
            SessionData {
                owner,
                expiry: u64::MAX,
                scope: SessionScope::TransfersOnly, // does NOT allow CancelOrder
                created_at: 0,
            },
        );
        store.insert(pk_forge, make_session(owner));

        let a_ok = sign_order(1, &key_ok);
        let a_exp = sign_order(2, &key_exp);
        let a_scope = sign_order(3, &key_scope);
        let mut a_forge = sign_order(4, &key_forge);
        if let ActionSignature::Session { sig, .. } = &mut a_forge.signature {
            sig.0[10] ^= 0xFF;
        }
        // Repeat a_ok to exercise the intra-batch HIT.
        let a_ok2 = sign_order(5, &key_ok);
        let actions = vec![a_ok, a_exp, a_scope, a_forge, a_ok2];

        let uncached = batch_verify_native_actions(&actions, ts, |pk| store.get(pk).cloned());

        let cache = std::cell::RefCell::new(SessionStore::new());
        let db_reads = std::cell::Cell::new(0usize);
        let cached =
            batch_verify_native_actions(&actions, ts, memoizing_lookup(&store, &cache, &db_reads));

        assert_eq!(cached, uncached, "cache run identical to uncached for mixed batch");
        // Expected shape: ok, reject(exp), reject(scope), reject(forge), ok.
        assert_eq!(cached[0], Some(owner));
        assert_eq!(cached[1], None, "expired rejected");
        assert_eq!(cached[2], None, "out-of-scope rejected");
        assert_eq!(cached[3], None, "forged rejected");
        assert_eq!(cached[4], Some(owner));
        // pk_ok resolved once then HIT on the repeat: 4 distinct pubkeys => 4 reads.
        assert_eq!(db_reads.get(), 4, "one read per distinct session pubkey");
    }

    /// Collapse the positional sender vector back to the indices of invalid
    /// actions, so the existing index-based assertions stay meaningful.
    fn invalid_indices(senders: &[Option<Address>]) -> Vec<usize> {
        senders
            .iter()
            .enumerate()
            .filter(|(_, s)| s.is_none())
            .map(|(i, _)| i)
            .collect()
    }

    /// The contract that makes the double-recovery removal safe: the sender
    /// `batch_verify_native_actions` returns for a valid action is EXACTLY what
    /// `recover_sender` (EIP-712) / `resolve_sender` (session) would return, and
    /// invalid actions come back as `None`.
    #[test]
    fn batch_verify_returns_resolved_senders() {
        let key = test_key();
        let ed_key = ed25519_dalek::SigningKey::from_bytes(&[42u8; 32]);
        let pubkey = ed_key.verifying_key().to_bytes();
        let owner = Address::from([0x11; 20]);
        let session = make_session(owner);

        // A different session key whose owner is NOT registered -> invalid.
        let ed_key_unknown = ed25519_dalek::SigningKey::from_bytes(&[43u8; 32]);

        let actions = vec![
            sign_native_action(NativeAction::ClaimRewards, TEST_NONCE, &key), // 0: EIP-712 valid
            sign_action_with_session(
                NativeAction::CancelOrder { order_id: 1 },
                TEST_NONCE,
                &ed_key,
            ), // 1: session valid
            sign_action_with_session(
                NativeAction::CancelOrder { order_id: 2 },
                TEST_NONCE,
                &ed_key_unknown,
            ), // 2: session not found -> invalid
        ];

        let lookup = |pk: &[u8; 32]| {
            if pk == &pubkey {
                Some(session.clone())
            } else {
                None
            }
        };
        let senders = batch_verify_native_actions(&actions, TEST_NONCE, lookup);

        // EIP-712: recovered address == signer, and == recover_sender().
        assert_eq!(senders[0], Some(signer_address(&key)));
        assert_eq!(senders[0], actions[0].recover_sender().ok());
        // Session: resolved sender == session owner, and == resolve_sender().
        assert_eq!(senders[1], Some(owner));
        assert_eq!(
            senders[1],
            actions[1].resolve_sender(TEST_NONCE, lookup).ok()
        );
        // Invalid action -> None.
        assert_eq!(senders[2], None);
    }

    /// Incident regression (mem 41e06912): the non-attested `validate_block` branch
    /// used `recover_sender`, which returns `Err` for EVERY session action — so it
    /// blanket-rejected session-signed orders and wedged the chain in a reject loop.
    /// The fix routes that branch through `batch_verify_native_actions` + a session
    /// lookup. This pins the exact accept/reject flip: `recover_sender` still errors
    /// on a session action, but `batch_verify` RESOLVES it when the session is
    /// registered and still returns `None` when it is not.
    #[test]
    fn session_action_errors_on_recover_but_resolves_via_batch_verify() {
        let ed_key = ed25519_dalek::SigningKey::from_bytes(&[7u8; 32]);
        let pubkey = ed_key.verifying_key().to_bytes();
        let owner = Address::from([0x33; 20]);
        let session = make_session(owner);
        let action = sign_action_with_session(
            NativeAction::CancelOrder { order_id: 9 },
            TEST_NONCE,
            &ed_key,
        );

        // The bug's mechanism: the old non-attested path can't verify sessions.
        assert!(action.recover_sender().is_err());

        // Fix: a registered session resolves to its owner (ACCEPT).
        let with_session =
            batch_verify_native_actions(std::slice::from_ref(&action), TEST_NONCE, |pk| {
                (pk == &pubkey).then(|| session.clone())
            });
        assert_eq!(with_session[0], Some(owner));

        // ...but an unregistered session still fails (REJECT — no security hole).
        let no_session =
            batch_verify_native_actions(std::slice::from_ref(&action), TEST_NONCE, |_| None);
        assert_eq!(no_session[0], None);
    }

    /// #2 parallelization guard: the parallel batch verify must reproduce the
    /// exact per-action ground truth, identically across runs, regardless of
    /// rayon thread scheduling (index order is preserved by `collect`).
    #[test]
    fn batch_verify_parallel_matches_serial_for_large_mixed_batch() {
        let key = test_key();
        let key2 = test_key_2();
        let ed_key = ed25519_dalek::SigningKey::from_bytes(&[42u8; 32]);
        let pubkey = ed_key.verifying_key().to_bytes();
        let owner = Address::from([0x11; 20]);
        let session = make_session(owner);
        let ed_key_unknown = ed25519_dalek::SigningKey::from_bytes(&[99u8; 32]);

        // 64 actions cycling: EIP-712(key), EIP-712(key2), valid session, invalid session.
        let mut actions = Vec::new();
        for i in 0..64u64 {
            let nonce = TEST_NONCE + i;
            actions.push(match i % 4 {
                0 => sign_native_action(NativeAction::ClaimRewards, nonce, &key),
                1 => sign_native_action(NativeAction::UnjailSelf, nonce, &key2),
                2 => sign_action_with_session(
                    NativeAction::CancelOrder { order_id: 1 },
                    nonce,
                    &ed_key,
                ),
                _ => sign_action_with_session(
                    NativeAction::CancelOrder { order_id: 1 },
                    nonce,
                    &ed_key_unknown,
                ),
            });
        }

        // Ground truth, computed independently of the batch function.
        let expected: Vec<Option<Address>> = (0..64u64)
            .map(|i| match i % 4 {
                0 => Some(signer_address(&key)),
                1 => Some(signer_address(&key2)),
                2 => Some(owner),
                _ => None,
            })
            .collect();

        let got = batch_verify_native_actions(&actions, TEST_NONCE, |pk| {
            if pk == &pubkey {
                Some(session.clone())
            } else {
                None
            }
        });
        assert_eq!(got, expected);

        // Determinism: a second run is identical (no thread-order dependence).
        let again = batch_verify_native_actions(&actions, TEST_NONCE, |pk| {
            if pk == &pubkey {
                Some(session.clone())
            } else {
                None
            }
        });
        assert_eq!(got, again);
    }

    #[test]
    fn batch_verify_all_valid_eip712() {
        let key = test_key();
        let actions = vec![
            sign_native_action(NativeAction::ClaimRewards, TEST_NONCE, &key),
            sign_native_action(NativeAction::UnjailSelf, TEST_NONCE + 1, &key),
        ];
        let invalid = invalid_indices(&batch_verify_native_actions(&actions, TEST_NONCE, |_| None));
        assert!(invalid.is_empty());
    }

    #[test]
    fn batch_verify_all_valid_session() {
        let ed_key = ed25519_dalek::SigningKey::from_bytes(&[42u8; 32]);
        let pubkey = ed_key.verifying_key().to_bytes();
        let session = make_session(Address::from([0x11; 20]));
        let actions = vec![
            sign_action_with_session(
                NativeAction::CancelOrder { order_id: 1 },
                TEST_NONCE,
                &ed_key,
            ),
            sign_action_with_session(
                NativeAction::CancelOrder { order_id: 2 },
                TEST_NONCE + 1,
                &ed_key,
            ),
        ];
        let invalid = invalid_indices(&batch_verify_native_actions(&actions, TEST_NONCE, |pk| {
            if pk == &pubkey {
                Some(session.clone())
            } else {
                None
            }
        }));
        assert!(invalid.is_empty());
    }

    #[test]
    fn batch_verify_mixed_valid_and_invalid() {
        let key = test_key();
        let ed_key = ed25519_dalek::SigningKey::from_bytes(&[42u8; 32]);
        let pubkey = ed_key.verifying_key().to_bytes();
        let session = make_session(Address::from([0x11; 20]));

        let actions = vec![
            sign_native_action(NativeAction::ClaimRewards, TEST_NONCE, &key),
            sign_action_with_session(
                NativeAction::CancelOrder { order_id: 1 },
                TEST_NONCE,
                &ed_key,
            ),
            sign_action_with_session(
                NativeAction::CancelOrder { order_id: 2 },
                TEST_NONCE,
                &ed_key,
            ),
        ];

        let invalid = invalid_indices(&batch_verify_native_actions(&actions, TEST_NONCE, |_| None));
        assert_eq!(invalid, vec![1, 2]);

        let invalid = invalid_indices(&batch_verify_native_actions(&actions, TEST_NONCE, |pk| {
            if pk == &pubkey {
                Some(session.clone())
            } else {
                None
            }
        }));
        assert!(invalid.is_empty());
    }

    #[test]
    fn batch_verify_detects_tampered_session_sig() {
        let ed_key = ed25519_dalek::SigningKey::from_bytes(&[42u8; 32]);
        let pubkey = ed_key.verifying_key().to_bytes();
        let session = make_session(Address::from([0x11; 20]));

        let mut action = sign_action_with_session(
            NativeAction::CancelOrder { order_id: 1 },
            TEST_NONCE,
            &ed_key,
        );
        if let ActionSignature::Session { ref mut sig, .. } = action.signature {
            sig.0[0] ^= 0xff;
        }

        let invalid = invalid_indices(&batch_verify_native_actions(&[action], TEST_NONCE, |pk| {
            if pk == &pubkey {
                Some(session.clone())
            } else {
                None
            }
        }));
        assert_eq!(invalid, vec![0]);
    }

    #[test]
    fn batch_verify_rejects_session_for_eip712_required_action() {
        let ed_key = ed25519_dalek::SigningKey::from_bytes(&[42u8; 32]);
        let pubkey = ed_key.verifying_key().to_bytes();
        let session = SessionData {
            owner: Address::from([0x11; 20]),
            expiry: u64::MAX,
            scope: SessionScope::Full,
            created_at: 0,
        };

        let action = sign_action_with_session(
            NativeAction::Withdraw {
                amount: alloy_primitives::U256::from(100),
                to: Address::ZERO,
            },
            TEST_NONCE,
            &ed_key,
        );

        let invalid = invalid_indices(&batch_verify_native_actions(&[action], TEST_NONCE, |pk| {
            if pk == &pubkey {
                Some(session.clone())
            } else {
                None
            }
        }));
        assert_eq!(invalid, vec![0]);
    }

    // ---- trust-cache short-circuit (double-verify-trust-cache T4) ----

    #[test]
    fn batch_verify_cache_hit_skips_recover() {
        // Valid EIP-712 action; the cache returns a SENTINEL distinct from the real
        // signer. If recovery had run it would yield `real`; getting `sentinel`
        // proves the HIT short-circuited the secp256k1 recover.
        let key = test_key();
        let action = sign_native_action(NativeAction::ClaimRewards, TEST_NONCE, &key);
        let real = signer_address(&key);
        let ck = crate::verified_cache_key(&action).expect("eip712 is cacheable");
        let sentinel = Address::from([0xAB; 20]);
        assert_ne!(sentinel, real);

        let got = batch_verify_native_actions_cached(
            std::slice::from_ref(&action),
            TEST_NONCE,
            |_| None,
            |k| if *k == ck { Some(sentinel) } else { None },
        );
        assert_eq!(
            got[0],
            Some(sentinel),
            "cache HIT must be reused verbatim (recover skipped)"
        );

        // MISS (empty cache) -> full recover yields the real signer (today's path).
        let miss = batch_verify_native_actions_cached(&[action], TEST_NONCE, |_| None, |_| None);
        assert_eq!(miss[0], Some(real), "cache MISS must full-recover");
    }

    #[test]
    fn batch_verify_cache_invalid_action_is_none() {
        // A session with no entry in lookup is unresolvable. Session actions are
        // never cacheable (`verified_cache_key` -> None), so the cache cannot mask
        // this: the result stays None — the slashing signal.
        let ed_key = ed25519_dalek::SigningKey::from_bytes(&[7u8; 32]);
        let action = sign_action_with_session(
            NativeAction::CancelOrder { order_id: 1 },
            TEST_NONCE,
            &ed_key,
        );
        assert!(
            crate::verified_cache_key(&action).is_none(),
            "session actions are not cacheable"
        );
        let got = batch_verify_native_actions_cached(&[action], TEST_NONCE, |_| None, |_| None);
        assert_eq!(got[0], None, "unresolvable action -> None (slash input)");
    }

    // ------------------------------------------------------------------
    // Phase-2 parallelization characterization (Task 2, perf/p3-throughput).
    //
    // `reference_impl` below is a byte-for-byte copy of the ORIGINAL (serial
    // Phase-2) `batch_verify_native_actions_cached` as it stood before the
    // parallel refactor. Every case asserts the production function's output is
    // bit-identical to this reference across the full matrix of session shapes,
    // so the refactor is proven output-preserving (no consensus/hash change).
    // ------------------------------------------------------------------

    /// Verbatim pre-refactor implementation (serial Phase 2). Kept as the
    /// characterization oracle; must never be "optimized".
    fn reference_impl(
        actions: &[SignedNativeAction],
        timestamp: u64,
        session_lookup: impl Fn(&[u8; 32]) -> Option<crate::SessionData>,
        verified_lookup: impl Fn(&alloy_primitives::B256) -> Option<Address>,
    ) -> Vec<Option<Address>> {
        use rayon::prelude::*;

        let cache_hits: Vec<Option<Address>> = actions
            .iter()
            .map(|action| crate::verified_cache_key(action).and_then(|k| verified_lookup(&k)))
            .collect();

        let mut senders: Vec<Option<Address>> = actions
            .par_iter()
            .zip(cache_hits.par_iter())
            .map(|(action, hit)| {
                if hit.is_some() {
                    return *hit;
                }
                match &action.signature {
                    ActionSignature::Eip712(_) => action.recover_sender().ok(),
                    ActionSignature::Session { .. } => None,
                }
            })
            .collect();

        let domain = eip712_domain_separator();

        let mut ed_indices = Vec::new();
        let mut ed_messages = Vec::new();
        let mut ed_signatures = Vec::new();
        let mut ed_keys = Vec::new();

        for (i, action) in actions.iter().enumerate() {
            let ActionSignature::Session {
                session_pubkey,
                sig,
            } = &action.signature
            else {
                continue;
            };
            if requires_eip712(&action.action) {
                continue;
            }
            match session_lookup(session_pubkey) {
                Some(session)
                    if timestamp <= session.expiry && session.scope.allows(&action.action) =>
                {
                    let Ok(vk) = Ed25519VerifyingKey::from_bytes(session_pubkey) else {
                        continue;
                    };
                    let struct_hash = eip712_struct_hash(&action.action, action.nonce);
                    let signing_hash = eip712_signing_hash(domain, struct_hash);

                    ed_indices.push(i);
                    ed_messages.push(signing_hash.0.to_vec());
                    ed_signatures.push(ed25519_dalek::Signature::from_bytes(&sig.0));
                    ed_keys.push(vk);
                    senders[i] = Some(session.owner);
                }
                _ => {}
            }
        }

        if !ed_indices.is_empty() {
            let msg_refs: Vec<&[u8]> = ed_messages.iter().map(|m| m.as_slice()).collect();
            if ed25519_dalek::verify_batch(&msg_refs, &ed_signatures, &ed_keys).is_err() {
                for j in 0..ed_indices.len() {
                    if ed_keys[j]
                        .verify(&ed_messages[j], &ed_signatures[j])
                        .is_err()
                    {
                        senders[ed_indices[j]] = None;
                    }
                }
            }
        }

        senders
    }

    /// Assert production == reference for a given batch + session store, on both
    /// the cache-off and (trivial) cache-on paths.
    fn assert_matches_reference(actions: &[SignedNativeAction], ts: u64, store: &SessionStore) {
        let lookup = |pk: &[u8; 32]| store.get(pk).cloned();
        let got = batch_verify_native_actions_cached(actions, ts, lookup, |_| None);
        let want = reference_impl(actions, ts, lookup, |_| None);
        assert_eq!(got, want, "parallel Phase-2 output must equal serial reference");
    }

    fn ed(seed: u8) -> ed25519_dalek::SigningKey {
        ed25519_dalek::SigningKey::from_bytes(&[seed; 32])
    }

    #[test]
    fn phase2_char_empty_batch() {
        let store = SessionStore::new();
        assert_matches_reference(&[], TEST_NONCE, &store);
    }

    #[test]
    fn phase2_char_all_valid_session() {
        let k = ed(11);
        let pk = k.verifying_key().to_bytes();
        let owner = Address::from([0x11; 20]);
        let mut store = SessionStore::new();
        store.insert(pk, make_session(owner));
        let actions: Vec<_> = (0..8u64)
            .map(|i| sign_order(TEST_NONCE + i, &k))
            .collect();
        assert_matches_reference(&actions, TEST_NONCE, &store);
    }

    #[test]
    fn phase2_char_expired_session() {
        let k = ed(12);
        let pk = k.verifying_key().to_bytes();
        let mut store = SessionStore::new();
        store.insert(
            pk,
            SessionData {
                owner: Address::from([0x12; 20]),
                expiry: TEST_NONCE - 1, // expired at `ts`
                scope: SessionScope::Trading,
                created_at: 0,
            },
        );
        let actions = vec![sign_order(TEST_NONCE, &k)];
        assert_matches_reference(&actions, TEST_NONCE, &store);
    }

    #[test]
    fn phase2_char_out_of_scope_action() {
        let k = ed(13);
        let pk = k.verifying_key().to_bytes();
        let mut store = SessionStore::new();
        // TransfersOnly does NOT allow CancelOrder (what sign_order builds).
        store.insert(
            pk,
            SessionData {
                owner: Address::from([0x13; 20]),
                expiry: u64::MAX,
                scope: SessionScope::TransfersOnly,
                created_at: 0,
            },
        );
        let actions = vec![sign_order(TEST_NONCE, &k)];
        assert_matches_reference(&actions, TEST_NONCE, &store);
    }

    #[test]
    fn phase2_char_forged_sig_in_valid_batch() {
        // Exercises the verify_batch-failure -> per-sig fallback path.
        let k = ed(14);
        let pk = k.verifying_key().to_bytes();
        let owner = Address::from([0x14; 20]);
        let mut store = SessionStore::new();
        store.insert(pk, make_session(owner));

        let mut actions: Vec<_> = (0..6u64)
            .map(|i| sign_order(TEST_NONCE + i, &k))
            .collect();
        // Corrupt the ed25519 signature on index 3.
        if let ActionSignature::Session { ref mut sig, .. } = actions[3].signature {
            sig.0[0] ^= 0xFF;
        }
        assert_matches_reference(&actions, TEST_NONCE, &store);
    }

    #[test]
    fn phase2_char_mixed_eip712_and_session() {
        let key = test_key();
        let k = ed(15);
        let pk = k.verifying_key().to_bytes();
        let owner = Address::from([0x15; 20]);
        let mut store = SessionStore::new();
        store.insert(pk, make_session(owner));

        let mut actions = Vec::new();
        for i in 0..12u64 {
            let nonce = TEST_NONCE + i;
            actions.push(if i % 2 == 0 {
                sign_native_action(NativeAction::UnjailSelf, nonce, &key)
            } else {
                sign_order(nonce, &k)
            });
        }
        assert_matches_reference(&actions, TEST_NONCE, &store);
    }

    #[test]
    fn phase2_char_many_actions_share_one_pubkey() {
        // The dedup pre-pass must resolve one lookup for all N; output identical.
        let k = ed(16);
        let pk = k.verifying_key().to_bytes();
        let owner = Address::from([0x16; 20]);
        let mut store = SessionStore::new();
        store.insert(pk, make_session(owner));
        let actions: Vec<_> = (0..32u64)
            .map(|i| sign_order(TEST_NONCE + i, &k))
            .collect();
        assert_matches_reference(&actions, TEST_NONCE, &store);
    }

    #[test]
    fn phase2_char_same_pubkey_lookup_none() {
        // Many actions under a pubkey the store does NOT know -> all None.
        let k = ed(17);
        let store = SessionStore::new(); // empty: every lookup returns None
        let actions: Vec<_> = (0..10u64)
            .map(|i| sign_order(TEST_NONCE + i, &k))
            .collect();
        assert_matches_reference(&actions, TEST_NONCE, &store);
    }

    #[test]
    fn phase2_char_requires_eip712_session_action() {
        // A session-signed action that REQUIRES full EIP-712 (ClaimRewards) must be
        // rejected (None) and must never trigger a session_lookup — the parallel
        // path skips it exactly like the serial one.
        let k = ed(18);
        let pk = k.verifying_key().to_bytes();
        let mut store = SessionStore::new();
        store.insert(pk, make_session(Address::from([0x18; 20])));
        let action = sign_action_with_session(NativeAction::ClaimRewards, TEST_NONCE, &k);
        let actions = vec![action];
        assert_matches_reference(&actions, TEST_NONCE, &store);
    }

    #[test]
    fn phase2_char_dedup_lookup_count_matches_unique_pubkeys() {
        // Prove the dedup contract directly: N actions across 3 unique pubkeys ->
        // exactly 3 session_lookup calls (independent of the closure's own memo).
        let keys = [ed(20), ed(21), ed(22)];
        let owners = [
            Address::from([0x20; 20]),
            Address::from([0x21; 20]),
            Address::from([0x22; 20]),
        ];
        let mut store = SessionStore::new();
        for (k, o) in keys.iter().zip(owners.iter()) {
            store.insert(k.verifying_key().to_bytes(), make_session(*o));
        }
        let mut actions = Vec::new();
        for i in 0..30u64 {
            actions.push(sign_order(TEST_NONCE + i, &keys[(i % 3) as usize]));
        }
        let calls = std::cell::Cell::new(0usize);
        let lookup = |pk: &[u8; 32]| {
            calls.set(calls.get() + 1);
            store.get(pk).cloned()
        };
        let got = batch_verify_native_actions_cached(&actions, TEST_NONCE, lookup, |_| None);
        assert_eq!(
            calls.get(),
            3,
            "dedup pre-pass must call session_lookup once per unique pubkey"
        );
        // And still correct: every action resolves to its owner.
        let want: Vec<Option<Address>> = (0..30u64)
            .map(|i| Some(owners[(i % 3) as usize]))
            .collect();
        assert_eq!(got, want);
    }

    #[test]
    fn phase2_char_large_mixed_stress() {
        // Broad stress: valid/expired/out-of-scope/unknown/eip712/forged/requires-eip712
        // all interleaved, several pubkeys shared, against the serial oracle.
        let key = test_key();
        let key2 = test_key_2();
        let good = ed(30);
        let good_pk = good.verifying_key().to_bytes();
        let expired = ed(31);
        let expired_pk = expired.verifying_key().to_bytes();
        let oos = ed(32);
        let oos_pk = oos.verifying_key().to_bytes();
        let unknown = ed(33); // never inserted
        let good2 = ed(34);
        let good2_pk = good2.verifying_key().to_bytes();

        let mut store = SessionStore::new();
        store.insert(good_pk, make_session(Address::from([0x30; 20])));
        store.insert(good2_pk, make_session(Address::from([0x34; 20])));
        store.insert(
            expired_pk,
            SessionData {
                owner: Address::from([0x31; 20]),
                expiry: TEST_NONCE - 5,
                scope: SessionScope::Trading,
                created_at: 0,
            },
        );
        store.insert(
            oos_pk,
            SessionData {
                owner: Address::from([0x32; 20]),
                expiry: u64::MAX,
                scope: SessionScope::TransfersOnly,
                created_at: 0,
            },
        );

        let mut actions = Vec::new();
        for i in 0..80u64 {
            let n = TEST_NONCE + i;
            let a = match i % 8 {
                0 => sign_native_action(NativeAction::UnjailSelf, n, &key),
                1 => sign_order(n, &good),
                2 => sign_order(n, &expired),
                3 => sign_order(n, &oos),
                4 => sign_order(n, &unknown),
                5 => sign_native_action(NativeAction::ClaimRewards, n, &key2),
                6 => sign_order(n, &good2),
                _ => sign_action_with_session(NativeAction::ClaimRewards, n, &good), // requires_eip712 via session
            };
            actions.push(a);
        }
        // Forge a couple of the otherwise-good session sigs to hit the fallback.
        if let ActionSignature::Session { ref mut sig, .. } = actions[9].signature {
            sig.0[5] ^= 0xAA;
        }
        if let ActionSignature::Session { ref mut sig, .. } = actions[49].signature {
            sig.0[10] ^= 0x55;
        }
        assert_matches_reference(&actions, TEST_NONCE, &store);
    }
}
