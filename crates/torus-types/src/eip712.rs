//! EIP-712 typed structured data signing for native actions.
//!
//! Each [`NativeAction`] variant has its own EIP-712 type hash and ABI-encoded
//! struct representation.  The nonce (timestamp-based, milliseconds) is included
//! in every struct for replay protection.
//!
//! Domain: `{ name: "Torus", version: "1", chainId: 7778, verifyingContract: 0x0 }`

use alloy_primitives::{keccak256, Address, B256, U256};
use k256::ecdsa::{RecoveryId, SigningKey, VerifyingKey};

use crate::{
    FixedPoint, MarketId, MarketListing, MarketParams, NativeAction, OracleSubmission, OrderId,
    OrderType, PlaceOrderParams, Proposal, ProposalAction, PublicKey, Signature,
    SignedNativeAction, VoteOption,
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
///   || uint256(7777)
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
    }
}

// ---------- order book ----------

fn hash_place_order(p: &PlaceOrderParams, nonce: u64) -> B256 {
    let th = keccak256(
        "PlaceOrder(uint64 marketId,bool isBuy,int128 price,int128 quantity,\
         uint8 orderType,uint8 timeInForce,bool reduceOnly,\
         uint64 clientOrderId,bool hasClientOrderId,uint64 nonce)",
    );
    let (coid, has_coid) = p.client_order_id.map_or((0, false), |id| (id, true));
    let mut buf = Vec::with_capacity(11 * 32);
    buf.extend_from_slice(&th.0);
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
        signature: Signature {
            v: recid.to_byte() + 27,
            r,
            s,
        },
    }
}

impl SignedNativeAction {
    /// Recover the sender's Ethereum address from the EIP-712 signature.
    pub fn recover_sender(&self) -> Result<Address, Eip712Error> {
        let domain = eip712_domain_separator();
        let struct_hash = eip712_struct_hash(&self.action, self.nonce);
        let signing_hash = eip712_signing_hash(domain, struct_hash);
        ecrecover(&signing_hash, &self.signature)
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
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{OrderType, TimeInForce};

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
        assert!(signed.signature.v == 27 || signed.signature.v == 28);
    }
}
