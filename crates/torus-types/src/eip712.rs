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
        NativeAction::CreateSession { session_pubkey, expiry, scope } => {
            hash_create_session(session_pubkey, *expiry, *scope, nonce)
        }
        NativeAction::RevokeSession { session_pubkey } => {
            hash_revoke_session(session_pubkey, nonce)
        }
    }
}

// ---------- order book ----------

/// Append the 9 EIP-712 order fields (no typehash, no nonce) to `buf`.
///
/// Shared by `hash_place_order` (single, nonce-bearing) and `hash_place_order_item`
/// (batch element, nonce at the batch level) so the two paths cannot diverge.
fn append_place_order_fields(buf: &mut Vec<u8>, p: &PlaceOrderParams) {
    let (coid, has_coid) = p.client_order_id.map_or((0, false), |id| (id, true));
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
}

fn hash_place_order(p: &PlaceOrderParams, nonce: u64) -> B256 {
    let th = keccak256(
        "PlaceOrder(uint64 marketId,bool isBuy,int128 price,int128 quantity,\
         uint8 orderType,uint8 timeInForce,bool reduceOnly,\
         uint64 clientOrderId,bool hasClientOrderId,uint64 nonce)",
    );
    let mut buf = Vec::with_capacity(11 * 32);
    buf.extend_from_slice(&th.0);
    append_place_order_fields(&mut buf, p);
    buf.extend_from_slice(&encode_u64(nonce));
    keccak256(&buf)
}

/// EIP-712 struct hash for one order *inside* a batch (no nonce — the nonce is
/// bound once at the batch level). Distinct typehash from `PlaceOrder` so a single
/// order and a batch element can never collide.
fn hash_place_order_item(p: &PlaceOrderParams) -> B256 {
    let th = keccak256(
        "PlaceOrderItem(uint64 marketId,bool isBuy,int128 price,int128 quantity,\
         uint8 orderType,uint8 timeInForce,bool reduceOnly,\
         uint64 clientOrderId,bool hasClientOrderId)",
    );
    let mut buf = Vec::with_capacity(10 * 32);
    buf.extend_from_slice(&th.0);
    append_place_order_fields(&mut buf, p);
    keccak256(&buf)
}

/// EIP-712 struct hash for a batch of orders under one signature + one nonce.
///
/// Follows the codebase's array convention (cf. `hash_submit_oracle_prices`):
/// the dynamic array is folded into a single `bytes32 ordersHash =
/// keccak256(item_hash_1 || item_hash_2 || ...)`. `count` is bound explicitly so
/// truncation/extension changes the hash even if it weren't already implied.
fn hash_place_order_batch(orders: &[PlaceOrderParams], nonce: u64) -> B256 {
    let th = keccak256("PlaceOrderBatch(bytes32 ordersHash,uint64 count,uint64 nonce)");
    let mut acc = Vec::with_capacity(orders.len() * 32);
    for p in orders {
        acc.extend_from_slice(&hash_place_order_item(p).0);
    }
    let orders_hash = keccak256(&acc);
    let mut buf = Vec::with_capacity(4 * 32);
    buf.extend_from_slice(&th.0);
    buf.extend_from_slice(&encode_bytes32(&orders_hash));
    buf.extend_from_slice(&encode_u64(orders.len() as u64));
    buf.extend_from_slice(&encode_u64(nonce));
    keccak256(&buf)
}

fn hash_cancel_order(order_id: OrderId, nonce: u64) -> B256 {
    let th = keccak256("CancelOrder(uint128 orderId,uint64 nonce)");
    let mut buf = Vec::with_capacity(3 * 32);
    buf.extend_from_slice(&th.0);
    buf.extend_from_slice(&encode_u128(order_id));
    buf.extend_from_slice(&encode_u64(nonce));
    keccak256(&buf)
}

fn hash_cancel_all_orders(market_id: Option<MarketId>, nonce: u64) -> B256 {
    let th = keccak256("CancelAllOrders(uint64 marketId,bool hasMarketId,uint64 nonce)");
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
    let th = keccak256(
        "ModifyOrder(uint128 orderId,int128 newPrice,bool hasNewPrice,\
         int128 newQty,bool hasNewQty,uint64 nonce)",
    );
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
    let th = keccak256("TransferToPerp(uint256 amount,uint64 nonce)");
    let mut buf = Vec::with_capacity(3 * 32);
    buf.extend_from_slice(&th.0);
    buf.extend_from_slice(&encode_u256(amount));
    buf.extend_from_slice(&encode_u64(nonce));
    keccak256(&buf)
}

fn hash_transfer_to_spot(amount: &U256, nonce: u64) -> B256 {
    let th = keccak256("TransferToSpot(uint256 amount,uint64 nonce)");
    let mut buf = Vec::with_capacity(3 * 32);
    buf.extend_from_slice(&th.0);
    buf.extend_from_slice(&encode_u256(amount));
    buf.extend_from_slice(&encode_u64(nonce));
    keccak256(&buf)
}

fn hash_withdraw(amount: &U256, to: &Address, nonce: u64) -> B256 {
    let th = keccak256("Withdraw(uint256 amount,address to,uint64 nonce)");
    let mut buf = Vec::with_capacity(4 * 32);
    buf.extend_from_slice(&th.0);
    buf.extend_from_slice(&encode_u256(amount));
    buf.extend_from_slice(&encode_address(to));
    buf.extend_from_slice(&encode_u64(nonce));
    keccak256(&buf)
}

// ---------- staking ----------

fn hash_delegate(validator: &Address, amount: &U256, nonce: u64) -> B256 {
    let th = keccak256("Delegate(address validator,uint256 amount,uint64 nonce)");
    let mut buf = Vec::with_capacity(4 * 32);
    buf.extend_from_slice(&th.0);
    buf.extend_from_slice(&encode_address(validator));
    buf.extend_from_slice(&encode_u256(amount));
    buf.extend_from_slice(&encode_u64(nonce));
    keccak256(&buf)
}

fn hash_undelegate(validator: &Address, amount: &U256, nonce: u64) -> B256 {
    let th = keccak256("Undelegate(address validator,uint256 amount,uint64 nonce)");
    let mut buf = Vec::with_capacity(4 * 32);
    buf.extend_from_slice(&th.0);
    buf.extend_from_slice(&encode_address(validator));
    buf.extend_from_slice(&encode_u256(amount));
    buf.extend_from_slice(&encode_u64(nonce));
    keccak256(&buf)
}

fn hash_permanent_stake(amount: &U256, nonce: u64) -> B256 {
    let th = keccak256("PermanentStake(uint256 amount,uint64 nonce)");
    let mut buf = Vec::with_capacity(3 * 32);
    buf.extend_from_slice(&th.0);
    buf.extend_from_slice(&encode_u256(amount));
    buf.extend_from_slice(&encode_u64(nonce));
    keccak256(&buf)
}

fn hash_claim_rewards(nonce: u64) -> B256 {
    let th = keccak256("ClaimRewards(uint64 nonce)");
    let mut buf = Vec::with_capacity(2 * 32);
    buf.extend_from_slice(&th.0);
    buf.extend_from_slice(&encode_u64(nonce));
    keccak256(&buf)
}

// ---------- governance ----------

fn hash_submit_proposal(proposal: &Proposal, nonce: u64) -> B256 {
    let th = keccak256(
        "SubmitProposal(string title,string description,bytes32 actionHash,uint64 nonce)",
    );
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
    let th = keccak256("Vote(uint64 proposalId,uint8 option,uint64 nonce)");
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
    let th = keccak256("SubmitOraclePrices(bytes32 pricesHash,uint64 timestamp,uint64 nonce)");
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
    let th = keccak256("RegisterValidator(bytes32 pubkey,uint16 commission,uint64 nonce)");
    let pk = B256::from(pubkey.0);
    let mut buf = Vec::with_capacity(4 * 32);
    buf.extend_from_slice(&th.0);
    buf.extend_from_slice(&encode_bytes32(&pk));
    buf.extend_from_slice(&encode_u16(commission));
    buf.extend_from_slice(&encode_u64(nonce));
    keccak256(&buf)
}

fn hash_update_commission(new_rate: u16, nonce: u64) -> B256 {
    let th = keccak256("UpdateCommission(uint16 newRate,uint64 nonce)");
    let mut buf = Vec::with_capacity(3 * 32);
    buf.extend_from_slice(&th.0);
    buf.extend_from_slice(&encode_u16(new_rate));
    buf.extend_from_slice(&encode_u64(nonce));
    keccak256(&buf)
}

fn hash_jail_vote(target: &Address, nonce: u64) -> B256 {
    let th = keccak256("JailVote(address target,uint64 nonce)");
    let mut buf = Vec::with_capacity(3 * 32);
    buf.extend_from_slice(&th.0);
    buf.extend_from_slice(&encode_address(target));
    buf.extend_from_slice(&encode_u64(nonce));
    keccak256(&buf)
}

fn hash_unjail_self(nonce: u64) -> B256 {
    let th = keccak256("UnjailSelf(uint64 nonce)");
    let mut buf = Vec::with_capacity(2 * 32);
    buf.extend_from_slice(&th.0);
    buf.extend_from_slice(&encode_u64(nonce));
    keccak256(&buf)
}

fn hash_rotate_validator_key(new_pubkey: &PublicKey, nonce: u64) -> B256 {
    let th = keccak256("RotateValidatorKey(bytes32 newPubkey,uint64 nonce)");
    let pk = B256::from(new_pubkey.0);
    let mut buf = Vec::with_capacity(3 * 32);
    buf.extend_from_slice(&th.0);
    buf.extend_from_slice(&encode_bytes32(&pk));
    buf.extend_from_slice(&encode_u64(nonce));
    keccak256(&buf)
}

// ---------- admin ----------

fn hash_update_market_params(market_id: MarketId, params: &MarketParams, nonce: u64) -> B256 {
    let th = keccak256("UpdateMarketParams(uint64 marketId,bytes32 paramsHash,uint64 nonce)");
    let ph = hash_market_params(params);
    let mut buf = Vec::with_capacity(4 * 32);
    buf.extend_from_slice(&th.0);
    buf.extend_from_slice(&encode_u64(market_id));
    buf.extend_from_slice(&encode_bytes32(&ph));
    buf.extend_from_slice(&encode_u64(nonce));
    keccak256(&buf)
}

fn hash_list_market(listing: &MarketListing, nonce: u64) -> B256 {
    let th = keccak256("ListMarket(bytes32 listingHash,uint64 nonce)");
    let lh = hash_market_listing(listing);
    let mut buf = Vec::with_capacity(3 * 32);
    buf.extend_from_slice(&th.0);
    buf.extend_from_slice(&encode_bytes32(&lh));
    buf.extend_from_slice(&encode_u64(nonce));
    keccak256(&buf)
}

fn hash_delist_market(market_id: MarketId, nonce: u64) -> B256 {
    let th = keccak256("DelistMarket(uint64 marketId,uint64 nonce)");
    let mut buf = Vec::with_capacity(3 * 32);
    buf.extend_from_slice(&th.0);
    buf.extend_from_slice(&encode_u64(market_id));
    buf.extend_from_slice(&encode_u64(nonce));
    keccak256(&buf)
}

// FIX ECON-FIND-15: EIP-712 type hash for TopUpSelfStake.
fn hash_top_up_self_stake(amount: &U256, nonce: u64) -> B256 {
    let th = keccak256("TopUpSelfStake(uint256 amount,uint64 nonce)");
    let mut buf = Vec::with_capacity(3 * 32);
    buf.extend_from_slice(&th.0);
    buf.extend_from_slice(&encode_u256(amount));
    buf.extend_from_slice(&encode_u64(nonce));
    keccak256(&buf)
}

fn hash_create_session(pubkey: &[u8; 32], expiry: u64, scope: SessionScope, nonce: u64) -> B256 {
    let th = keccak256("CreateSession(bytes32 sessionPubkey,uint64 expiry,uint8 scope,uint64 nonce)");
    let mut buf = Vec::with_capacity(5 * 32);
    buf.extend_from_slice(&th.0);
    buf.extend_from_slice(pubkey);
    buf.extend_from_slice(&encode_u64(expiry));
    buf.extend_from_slice(&encode_u8(scope as u8));
    buf.extend_from_slice(&encode_u64(nonce));
    keccak256(&buf)
}

fn hash_revoke_session(pubkey: &[u8; 32], nonce: u64) -> B256 {
    let th = keccak256("RevokeSession(bytes32 sessionPubkey,uint64 nonce)");
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
        ProposalAction::ValidatorRegistration { candidate } => {
            keccak256(encode_address(candidate))
        }
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
            ActionSignature::Session { session_pubkey, sig } => {
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
            ActionSignature::Session { session_pubkey, sig } => {
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
                let session = session_lookup(session_pubkey)
                    .ok_or(Eip712Error::SessionNotFound)?;
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
/// verification stays sequential — its ed25519 work is already batched, and
/// keeping `session_lookup` off the worker threads avoids forcing a `Sync` bound
/// on callers.
pub fn batch_verify_native_actions(
    actions: &[SignedNativeAction],
    timestamp: u64,
    session_lookup: impl Fn(&[u8; 32]) -> Option<crate::SessionData>,
) -> Vec<Option<Address>> {
    use rayon::prelude::*;

    // Phase 1 (parallel): recover EIP-712 senders. Each ecrecover is pure,
    // independent crypto with no shared/borrowed state; `collect` over an indexed
    // parallel iterator preserves order, so the output is deterministic.
    let mut senders: Vec<Option<Address>> = actions
        .par_iter()
        .map(|action| match &action.signature {
            ActionSignature::Eip712(_) => action.recover_sender().ok(),
            ActionSignature::Session { .. } => None,
        })
        .collect();

    // Phase 2 (sequential): resolve session actions (needs `session_lookup`) and
    // assemble the ed25519 batch.
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
            continue; // EIP-712 already resolved in phase 1
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
                // Tentatively valid; cleared below if the batch ed25519 verify fails.
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

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Ed25519Sig, OrderType, SessionData, SessionScope, TimeInForce};

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
        let orders = vec![mk(1, true, Some(1)), mk(2, false, Some(2)), mk(3, true, None)];

        // ONE signature covers all N orders (the throughput keystone).
        let signed =
            sign_native_action(NativeAction::PlaceOrderBatch(orders.clone()), TEST_NONCE, &key);
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
                sig: Ed25519Sig(sig.to_bytes()),
            },
        }
    }

    fn make_session(owner: Address) -> SessionData {
        SessionData {
            owner,
            expiry: u64::MAX,
            scope: SessionScope::Trading,
            created_at: 0,
        }
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
            sign_action_with_session(NativeAction::CancelOrder { order_id: 1 }, TEST_NONCE, &ed_key), // 1: session valid
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
        assert_eq!(senders[1], actions[1].resolve_sender(TEST_NONCE, lookup).ok());
        // Invalid action -> None.
        assert_eq!(senders[2], None);
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
                2 => sign_action_with_session(NativeAction::CancelOrder { order_id: 1 }, nonce, &ed_key),
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
            if pk == &pubkey { Some(session.clone()) } else { None }
        });
        assert_eq!(got, expected);

        // Determinism: a second run is identical (no thread-order dependence).
        let again = batch_verify_native_actions(&actions, TEST_NONCE, |pk| {
            if pk == &pubkey { Some(session.clone()) } else { None }
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
            if pk == &pubkey { Some(session.clone()) } else { None }
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
            if pk == &pubkey { Some(session.clone()) } else { None }
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
            if pk == &pubkey { Some(session.clone()) } else { None }
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
            if pk == &pubkey { Some(session.clone()) } else { None }
        }));
        assert_eq!(invalid, vec![0]);
    }
}
