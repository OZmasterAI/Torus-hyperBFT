//! Cross-VM precompiles — ABI-encoded bridges between EVM and native state (task 2.4).
//!
//! Precompile address map:
//!   0x0800 — OrderBookReader (read)
//!   0x0801 — BalanceReader (read)
//!   0x0802 — OracleReader (read)
//!   0x0803 — StakingReader (read)
//!   0x0810 — CoreWriter (write: orders, delayed)
//!   0x0811 — CoreWriterStaking (write: staking, delayed)
//!   0x0820 — Lockbox (write: asset transfers, immediate)

use std::io::{self, Read as IoRead, Write as IoWrite};

use alloy_primitives::keccak256;
use borsh::{BorshDeserialize, BorshSerialize};
use torus_state::cf::*;
use torus_state::StateDb;
use torus_types::{Address, FixedPoint, MarketId, OrderId, U256};

use crate::error::CoreError;
use crate::lockbox::Lockbox;
use crate::position::{
    borsh_read_address, borsh_read_fp, borsh_write_address, borsh_write_fp, NativeBalance, Position,
};

// ============================================================================
// Precompile Address Constants
// ============================================================================

pub const ADDR_ORDER_BOOK_READER: u16 = 0x0800;
pub const ADDR_BALANCE_READER: u16 = 0x0801;
pub const ADDR_ORACLE_READER: u16 = 0x0802;
pub const ADDR_STAKING_READER: u16 = 0x0803;
pub const ADDR_CORE_WRITER: u16 = 0x0810;
pub const ADDR_CORE_WRITER_STAKING: u16 = 0x0811;
pub const ADDR_LOCKBOX: u16 = 0x0820;

/// FIX EVM-PF-10: Gas costs for Torus precompiles.
/// Read-only queries (cold SLOAD equivalent).
pub const GAS_PRECOMPILE_READ: u64 = 2_600;
/// State-mutating writes (SSTORE equivalent range).
pub const GAS_PRECOMPILE_WRITE: u64 = 20_000;
/// Complex operations (governance, liquidation).
pub const GAS_PRECOMPILE_COMPLEX: u64 = 50_000;

/// Gas cost for a precompile by address ID.
pub const fn precompile_gas(id: u16) -> u64 {
    match id {
        0x0800..=0x0803 => GAS_PRECOMPILE_READ,
        0x0810..=0x0811 => GAS_PRECOMPILE_WRITE,
        0x0820 => GAS_PRECOMPILE_WRITE,
        _ => 0,
    }
}

/// Convert a precompile ID to a 20-byte Ethereum address.
pub const fn precompile_address(id: u16) -> Address {
    let b = id.to_be_bytes();
    Address::new([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, b[0], b[1]])
}

/// Check whether an address is a Torus precompile.
pub fn is_precompile(address: &Address) -> bool {
    let bytes = address.as_slice();
    bytes[..18].iter().all(|&b| b == 0) && {
        let id = u16::from_be_bytes([bytes[18], bytes[19]]);
        matches!(id, 0x0800..=0x0803 | 0x0810..=0x0811 | 0x0820)
    }
}

// ============================================================================
// ABI Encoding / Decoding
// ============================================================================

pub mod abi {
    //! Solidity ABI encoding/decoding helpers (no alloy-sol-types dependency).

    use torus_types::{Address, FixedPoint, MarketId, OrderId};

    /// Extract the 4-byte function selector.
    pub fn selector(input: &[u8]) -> Result<u32, super::CoreError> {
        if input.len() < 4 {
            return Err(super::CoreError::InvalidPrecompileInput(
                "input too short for selector".into(),
            ));
        }
        Ok(u32::from_be_bytes([input[0], input[1], input[2], input[3]]))
    }

    /// Get the Nth 32-byte word from ABI params (after the 4-byte selector).
    pub fn word(input: &[u8], n: usize) -> Result<[u8; 32], super::CoreError> {
        let start = 4 + n * 32;
        let end = start + 32;
        if end > input.len() {
            return Err(super::CoreError::InvalidPrecompileInput(format!(
                "need {} bytes, got {}",
                end,
                input.len()
            )));
        }
        let mut w = [0u8; 32];
        w.copy_from_slice(&input[start..end]);
        Ok(w)
    }

    // ---- Decoders ----

    pub fn decode_address(w: &[u8; 32]) -> Address {
        Address::from_slice(&w[12..32])
    }

    pub fn decode_u128(w: &[u8; 32]) -> u128 {
        u128::from_be_bytes(w[16..32].try_into().unwrap())
    }

    pub fn decode_u8(w: &[u8; 32]) -> u8 {
        w[31]
    }

    pub fn decode_market_id(w: &[u8; 32]) -> MarketId {
        u64::from_be_bytes(w[24..32].try_into().unwrap())
    }

    pub fn decode_order_id(w: &[u8; 32]) -> OrderId {
        u128::from_be_bytes(w[16..32].try_into().unwrap())
    }

    // ---- Encoders ----

    pub fn encode_u128(val: u128) -> [u8; 32] {
        let mut w = [0u8; 32];
        w[16..32].copy_from_slice(&val.to_be_bytes());
        w
    }

    pub fn encode_i128(val: i128) -> [u8; 32] {
        let mut w = [0u8; 32];
        if val < 0 {
            w[..16].fill(0xFF);
        }
        w[16..32].copy_from_slice(&val.to_be_bytes());
        w
    }

    pub fn encode_address(addr: &Address) -> [u8; 32] {
        let mut w = [0u8; 32];
        w[12..32].copy_from_slice(addr.as_slice());
        w
    }

    pub fn encode_bool(val: bool) -> [u8; 32] {
        let mut w = [0u8; 32];
        w[31] = u8::from(val);
        w
    }

    pub fn encode_u64(val: u64) -> [u8; 32] {
        let mut w = [0u8; 32];
        w[24..32].copy_from_slice(&val.to_be_bytes());
        w
    }

    pub fn encode_u8(val: u8) -> [u8; 32] {
        let mut w = [0u8; 32];
        w[31] = val;
        w
    }

    pub fn encode_u32(val: u32) -> [u8; 32] {
        let mut w = [0u8; 32];
        w[28..32].copy_from_slice(&val.to_be_bytes());
        w
    }

    pub fn encode_market_id(id: MarketId) -> [u8; 32] {
        encode_u64(id)
    }

    pub fn encode_order_id(id: OrderId) -> [u8; 32] {
        encode_u128(id)
    }

    pub fn encode_fp_as_u128(fp: FixedPoint) -> [u8; 32] {
        encode_u128(fp.raw() as u128)
    }

    pub fn encode_fp_as_i128(fp: FixedPoint) -> [u8; 32] {
        encode_i128(fp.raw())
    }

    /// Build an ABI-encoded response containing only dynamic arrays.
    ///
    /// For `(T[] a, T[] b, ...)`, the encoding is:
    /// - Head: one offset word per array
    /// - Tail: `[length, elem0, elem1, ...]` for each array
    pub fn encode_arrays_response(arrays: &[&[[u8; 32]]]) -> Vec<u8> {
        let n = arrays.len();
        let head_size = n * 32;

        let mut offsets = Vec::with_capacity(n);
        let mut curr = head_size;
        for arr in arrays {
            offsets.push(curr);
            curr += 32 + arr.len() * 32;
        }

        let mut out = Vec::with_capacity(curr);
        for off in &offsets {
            out.extend_from_slice(&encode_u32(*off as u32));
        }
        for arr in arrays {
            out.extend_from_slice(&encode_u32(arr.len() as u32));
            for elem in *arr {
                out.extend_from_slice(elem);
            }
        }
        out
    }
}

/// Compute a Solidity function selector from its signature string.
fn selector_for(sig: &str) -> u32 {
    let hash = keccak256(sig.as_bytes());
    u32::from_be_bytes([hash[0], hash[1], hash[2], hash[3]])
}

// ============================================================================
// Main Dispatch
// ============================================================================

/// Execute a precompile call. Returns ABI-encoded output bytes.
///
/// * `address` — 20-byte precompile address
/// * `input` — ABI-encoded input (4-byte selector + params)
/// * `caller` — msg.sender (used by CoreWriter and Lockbox)
/// * `state_db` — state access
/// * `current_block` — current block number
pub fn execute_precompile(
    address: &Address,
    input: &[u8],
    caller: &Address,
    state_db: &StateDb,
    current_block: u64,
) -> Result<Vec<u8>, CoreError> {
    let bytes = address.as_slice();
    if !bytes[..18].iter().all(|&b| b == 0) {
        return Err(CoreError::InvalidPrecompileInput(
            "invalid precompile address prefix".into(),
        ));
    }
    let id = u16::from_be_bytes([bytes[18], bytes[19]]);

    match id {
        ADDR_ORDER_BOOK_READER => order_book_reader(input, state_db),
        ADDR_BALANCE_READER => balance_reader(input, state_db),
        ADDR_ORACLE_READER => oracle_reader(input, state_db, current_block),
        ADDR_STAKING_READER => staking_reader(input, state_db),
        ADDR_CORE_WRITER => core_writer(input, caller, state_db, current_block),
        ADDR_CORE_WRITER_STAKING => core_writer_staking(input, caller, state_db, current_block),
        ADDR_LOCKBOX => lockbox_precompile(input, caller, state_db),
        _ => Err(CoreError::InvalidPrecompileInput(format!(
            "unknown precompile 0x{id:04x}"
        ))),
    }
}

// ============================================================================
// OrderBookReader (0x0800) — tasks 2.4.1
// ============================================================================

fn order_book_reader(input: &[u8], state_db: &StateDb) -> Result<Vec<u8>, CoreError> {
    let sel = abi::selector(input)?;

    if sel == selector_for("getOrderBook(bytes32)") {
        let market_id = abi::decode_market_id(&abi::word(input, 0)?);
        read_order_book(state_db, market_id)
    } else if sel == selector_for("getPosition(address,bytes32)") {
        let trader = abi::decode_address(&abi::word(input, 0)?);
        let market_id = abi::decode_market_id(&abi::word(input, 1)?);
        read_position(state_db, &trader, market_id)
    } else if sel == selector_for("getOpenOrders(address,bytes32)") {
        let trader = abi::decode_address(&abi::word(input, 0)?);
        let market_id = abi::decode_market_id(&abi::word(input, 1)?);
        read_open_orders(state_db, &trader, market_id)
    } else {
        Err(CoreError::UnknownSelector(sel))
    }
}

/// getOrderBook → (uint128[] bid_prices, uint128[] bid_qtys, uint128[] ask_prices, uint128[] ask_qtys)
fn read_order_book(state_db: &StateDb, market_id: MarketId) -> Result<Vec<u8>, CoreError> {
    let key = market_id.to_be_bytes();
    let snapshot = match state_db.get_cf_raw(CF_NATIVE_ORDER_BOOKS, &key)? {
        Some(data) => OrderBookSnapshot::try_from_slice(&data)
            .map_err(|e| CoreError::Borsh(e.to_string()))?,
        None => OrderBookSnapshot {
            bids: vec![],
            asks: vec![],
        },
    };

    let bid_prices: Vec<[u8; 32]> = snapshot.bids.iter().map(|l| abi::encode_fp_as_u128(l.price)).collect();
    let bid_qtys: Vec<[u8; 32]> = snapshot.bids.iter().map(|l| abi::encode_fp_as_u128(l.quantity)).collect();
    let ask_prices: Vec<[u8; 32]> = snapshot.asks.iter().map(|l| abi::encode_fp_as_u128(l.price)).collect();
    let ask_qtys: Vec<[u8; 32]> = snapshot.asks.iter().map(|l| abi::encode_fp_as_u128(l.quantity)).collect();

    Ok(abi::encode_arrays_response(&[&bid_prices, &bid_qtys, &ask_prices, &ask_qtys]))
}

/// getPosition → (int128 size, uint128 entry_price, int128 unrealized_pnl, int128 realized_pnl, uint128 margin)
fn read_position(
    state_db: &StateDb,
    trader: &Address,
    market_id: MarketId,
) -> Result<Vec<u8>, CoreError> {
    let key = crate::position::position_key(trader, market_id);
    let pos = match state_db.get_cf_raw(CF_NATIVE_POSITIONS, &key)? {
        Some(data) => {
            Position::try_from_slice(&data).map_err(|e| CoreError::Borsh(e.to_string()))?
        }
        None => {
            // No position — return zeros
            let mut out = Vec::with_capacity(160);
            for _ in 0..5 {
                out.extend_from_slice(&[0u8; 32]);
            }
            return Ok(out);
        }
    };

    // Compute unrealized PnL using oracle price
    let unrealized_pnl = match get_oracle_price_fp(state_db, market_id) {
        Ok(mark) => pos.unrealized_pnl(mark),
        Err(_) => FixedPoint::ZERO,
    };

    // Signed size: positive for long, negative for short
    let signed_size = if pos.is_long {
        pos.size.raw()
    } else {
        -pos.size.raw()
    };

    let mut out = Vec::with_capacity(160);
    out.extend_from_slice(&abi::encode_i128(signed_size));
    out.extend_from_slice(&abi::encode_fp_as_u128(pos.entry_price));
    out.extend_from_slice(&abi::encode_fp_as_i128(unrealized_pnl));
    out.extend_from_slice(&abi::encode_fp_as_i128(pos.realized_pnl));
    out.extend_from_slice(&abi::encode_fp_as_u128(pos.isolated_margin));
    Ok(out)
}

/// getOpenOrders → (bytes32[] order_ids, uint128[] prices, uint128[] quantities, uint8[] sides)
fn read_open_orders(
    state_db: &StateDb,
    trader: &Address,
    market_id: MarketId,
) -> Result<Vec<u8>, CoreError> {
    let db = state_db.inner();
    let cf = db
        .cf_handle(CF_NATIVE_ORDERS)
        .ok_or(CoreError::MissingCf(CF_NATIVE_ORDERS))?;

    // Prefix: trader(20) + market_id(8)
    let mut prefix = [0u8; 28];
    prefix[..20].copy_from_slice(trader.as_slice());
    prefix[20..28].copy_from_slice(&market_id.to_be_bytes());

    let iter = db.prefix_iterator_cf(cf, &prefix);
    let mut order_ids = Vec::new();
    let mut prices = Vec::new();
    let mut quantities = Vec::new();
    let mut sides = Vec::new();

    for item in iter {
        let (key, value) =
            item.map_err(|e| CoreError::State(torus_state::StateError::RocksDb(e)))?;
        if !key.starts_with(&prefix) {
            break;
        }
        if let Ok(order) = StoredOrder::try_from_slice(&value) {
            order_ids.push(abi::encode_order_id(order.order_id));
            prices.push(abi::encode_fp_as_u128(order.price));
            quantities.push(abi::encode_fp_as_u128(order.remaining_qty));
            sides.push(abi::encode_u8(order.side));
        }
    }

    Ok(abi::encode_arrays_response(&[&order_ids, &prices, &quantities, &sides]))
}

// ============================================================================
// BalanceReader (0x0801) — tasks 2.4.1
// ============================================================================

fn balance_reader(input: &[u8], state_db: &StateDb) -> Result<Vec<u8>, CoreError> {
    let sel = abi::selector(input)?;

    if sel == selector_for("getBalances(address)") {
        let trader = abi::decode_address(&abi::word(input, 0)?);
        read_balances(state_db, &trader)
    } else if sel == selector_for("getMarkets()") {
        read_markets(state_db)
    } else {
        Err(CoreError::UnknownSelector(sel))
    }
}

/// getBalances → (uint128 native_balance, uint128 evm_balance, uint128 total_margin_used, uint128 available)
fn read_balances(state_db: &StateDb, trader: &Address) -> Result<Vec<u8>, CoreError> {
    // Native balance from CF_NATIVE_BALANCES
    let native_bal = match state_db.get_cf_raw(CF_NATIVE_BALANCES, trader.as_slice())? {
        Some(data) => {
            NativeBalance::try_from_slice(&data).map_err(|e| CoreError::Borsh(e.to_string()))?
        }
        None => NativeBalance::default(),
    };

    // EVM balance from CF_ACCOUNTS (first 32 bytes)
    let evm_balance = match state_db.get_cf_raw(CF_ACCOUNTS, trader.as_slice())? {
        Some(data) if data.len() >= 32 => {
            let val = U256::from_be_slice(&data[..32]);
            // Truncate to u128 for ABI encoding
            let bytes = val.to_be_bytes::<32>();
            u128::from_be_bytes(bytes[16..32].try_into().unwrap())
        }
        _ => 0u128,
    };

    let mut out = Vec::with_capacity(128);
    out.extend_from_slice(&abi::encode_fp_as_u128(native_bal.available));
    out.extend_from_slice(&abi::encode_u128(evm_balance));
    out.extend_from_slice(&abi::encode_fp_as_u128(native_bal.order_margin));
    // available = native_available (native balance not locked in orders)
    out.extend_from_slice(&abi::encode_fp_as_u128(native_bal.available));
    Ok(out)
}

/// getMarkets → (bytes32[] market_ids, bool[] active)
fn read_markets(state_db: &StateDb) -> Result<Vec<u8>, CoreError> {
    let db = state_db.inner();
    let cf = db
        .cf_handle(CF_NATIVE_MARKETS)
        .ok_or(CoreError::MissingCf(CF_NATIVE_MARKETS))?;

    let iter = db.iterator_cf(cf, rocksdb::IteratorMode::Start);
    let mut market_ids = Vec::new();
    let mut active_flags = Vec::new();

    for item in iter {
        let (key, value) =
            item.map_err(|e| CoreError::State(torus_state::StateError::RocksDb(e)))?;
        if key.len() == 8 {
            let mid = u64::from_be_bytes(key[..8].try_into().unwrap());
            market_ids.push(abi::encode_market_id(mid));
            let is_active = value.first().copied().unwrap_or(0) != 0;
            active_flags.push(abi::encode_bool(is_active));
        }
    }

    Ok(abi::encode_arrays_response(&[&market_ids, &active_flags]))
}

// ============================================================================
// OracleReader (0x0802) — tasks 2.4.2
// ============================================================================

/// Max oracle age in blocks before price is considered stale.
const DEFAULT_MAX_ORACLE_AGE: u64 = 100;

fn oracle_reader(
    input: &[u8],
    state_db: &StateDb,
    current_block: u64,
) -> Result<Vec<u8>, CoreError> {
    let sel = abi::selector(input)?;

    if sel == selector_for("getPrice(bytes32)") {
        let market_id = abi::decode_market_id(&abi::word(input, 0)?);
        read_oracle_price(state_db, market_id, current_block)
    } else if sel == selector_for("getAllPrices()") {
        read_all_oracle_prices(state_db, current_block)
    } else {
        Err(CoreError::UnknownSelector(sel))
    }
}

/// getPrice → (uint128 price, uint64 block_number, bool stale)
fn read_oracle_price(
    state_db: &StateDb,
    market_id: MarketId,
    current_block: u64,
) -> Result<Vec<u8>, CoreError> {
    let key = oracle_agg_key(market_id);
    match state_db.get_cf_raw(CF_NATIVE_ORACLE, &key)? {
        Some(data) if data.len() >= 28 => {
            let price = FixedPoint::from_raw(i128::from_be_bytes(
                data[..16].try_into().unwrap(),
            ));
            let block_number = u64::from_be_bytes(data[16..24].try_into().unwrap());
            let stale = current_block.saturating_sub(block_number) > DEFAULT_MAX_ORACLE_AGE;

            let mut out = Vec::with_capacity(96);
            out.extend_from_slice(&abi::encode_fp_as_u128(price));
            out.extend_from_slice(&abi::encode_u64(block_number));
            out.extend_from_slice(&abi::encode_bool(stale));
            Ok(out)
        }
        _ => Err(CoreError::NoOraclePrice(market_id)),
    }
}

/// getAllPrices → (bytes32[] market_ids, uint128[] prices, bool[] stale_flags)
fn read_all_oracle_prices(
    state_db: &StateDb,
    current_block: u64,
) -> Result<Vec<u8>, CoreError> {
    let db = state_db.inner();
    let cf = db
        .cf_handle(CF_NATIVE_ORACLE)
        .ok_or(CoreError::MissingCf(CF_NATIVE_ORACLE))?;

    let prefix = b"agg";
    let iter = db.prefix_iterator_cf(cf, prefix);

    let mut market_ids = Vec::new();
    let mut prices = Vec::new();
    let mut stale_flags = Vec::new();

    for item in iter {
        let (key, value) =
            item.map_err(|e| CoreError::State(torus_state::StateError::RocksDb(e)))?;
        if !key.starts_with(prefix) {
            break;
        }
        if key.len() == 11 && value.len() >= 28 {
            let mid = u64::from_be_bytes(key[3..11].try_into().unwrap());
            let price = FixedPoint::from_raw(i128::from_be_bytes(
                value[..16].try_into().unwrap(),
            ));
            let block_number = u64::from_be_bytes(value[16..24].try_into().unwrap());
            let stale = current_block.saturating_sub(block_number) > DEFAULT_MAX_ORACLE_AGE;

            market_ids.push(abi::encode_market_id(mid));
            prices.push(abi::encode_fp_as_u128(price));
            stale_flags.push(abi::encode_bool(stale));
        }
    }

    Ok(abi::encode_arrays_response(&[&market_ids, &prices, &stale_flags]))
}

/// Build the oracle aggregated price key: "agg" + market_id(8 BE).
fn oracle_agg_key(market_id: MarketId) -> Vec<u8> {
    let mut key = Vec::with_capacity(11);
    key.extend_from_slice(b"agg");
    key.extend_from_slice(&market_id.to_be_bytes());
    key
}

/// Read the oracle price as FixedPoint (helper for other precompiles).
fn get_oracle_price_fp(state_db: &StateDb, market_id: MarketId) -> Result<FixedPoint, CoreError> {
    let key = oracle_agg_key(market_id);
    match state_db.get_cf_raw(CF_NATIVE_ORACLE, &key)? {
        Some(data) if data.len() >= 16 => Ok(FixedPoint::from_raw(i128::from_be_bytes(
            data[..16].try_into().unwrap(),
        ))),
        _ => Err(CoreError::NoOraclePrice(market_id)),
    }
}

// ============================================================================
// StakingReader (0x0803) — tasks 2.4.2
// ============================================================================

fn staking_reader(input: &[u8], state_db: &StateDb) -> Result<Vec<u8>, CoreError> {
    let sel = abi::selector(input)?;

    if sel == selector_for("getStakingInfo(address)") {
        let staker = abi::decode_address(&abi::word(input, 0)?);
        read_staking_info(state_db, &staker)
    } else if sel == selector_for("getValidators()") {
        read_validators(state_db)
    } else {
        Err(CoreError::UnknownSelector(sel))
    }
}

/// getStakingInfo → (uint128 delegated, uint128 permanent, uint128 rewards_pending, address validator)
fn read_staking_info(state_db: &StateDb, staker: &Address) -> Result<Vec<u8>, CoreError> {
    // Read delegation info: scan CF_STAKING_DELEGATIONS with prefix = staker(20)
    let db = state_db.inner();
    let mut total_delegated = U256::ZERO;
    let mut first_validator = Address::ZERO;

    if let Some(cf) = db.cf_handle(CF_STAKING_DELEGATIONS) {
        let prefix = staker.as_slice();
        let iter = db.prefix_iterator_cf(cf, prefix);
        for item in iter {
            let (key, value) =
                item.map_err(|e| CoreError::State(torus_state::StateError::RocksDb(e)))?;
            if !key.starts_with(prefix) || key.len() != 40 {
                break;
            }
            // Value starts with: delegator(20) + validator(20) + amount(U256 32 BE)
            if value.len() >= 72 {
                let amount = U256::from_be_slice(&value[40..72]);
                if !amount.is_zero() && first_validator == Address::ZERO {
                    first_validator = Address::from_slice(&value[20..40]);
                }
                total_delegated += amount;
            }
        }
    }

    // Truncate U256 to u128 for ABI
    let delegated_u128 = u256_to_u128_saturating(total_delegated);

    // Read permanent stake: CF_STAKING_PERMANENT, key = staker(20)
    let permanent = match state_db.get_cf_raw(CF_STAKING_PERMANENT, staker.as_slice())? {
        // Format: staker(20) + amount(U256 32 BE) + locked_at_block(8 LE borsh)
        Some(data) if data.len() >= 52 => {
            let amount = U256::from_be_slice(&data[20..52]);
            u256_to_u128_saturating(amount)
        }
        _ => 0u128,
    };

    // Read pending rewards: CF_STAKING_REWARDS, key = staker(20)
    let rewards = match state_db.get_cf_raw(CF_STAKING_REWARDS, staker.as_slice())? {
        // Format: address(20) + amount(U256 32 BE)
        Some(data) if data.len() >= 52 => {
            let amount = U256::from_be_slice(&data[20..52]);
            u256_to_u128_saturating(amount)
        }
        _ => 0u128,
    };

    let mut out = Vec::with_capacity(128);
    out.extend_from_slice(&abi::encode_u128(delegated_u128));
    out.extend_from_slice(&abi::encode_u128(permanent));
    out.extend_from_slice(&abi::encode_u128(rewards));
    out.extend_from_slice(&abi::encode_address(&first_validator));
    Ok(out)
}

/// getValidators → (address[] validators, uint128[] stakes, uint128[] commissions)
fn read_validators(state_db: &StateDb) -> Result<Vec<u8>, CoreError> {
    let db = state_db.inner();
    let cf = db
        .cf_handle(CF_STAKING_VALIDATORS)
        .ok_or(CoreError::MissingCf(CF_STAKING_VALIDATORS))?;

    let iter = db.iterator_cf(cf, rocksdb::IteratorMode::Start);
    let mut validators = Vec::new();
    let mut stakes = Vec::new();
    let mut commissions = Vec::new();

    for item in iter {
        let (key, value) =
            item.map_err(|e| CoreError::State(torus_state::StateError::RocksDb(e)))?;
        if key.len() != 20 {
            continue;
        }
        // ValidatorState borsh: address(20) + pubkey(32) + commission_bps(u16 LE) + self_stake(U256 32 BE) + total_delegated(U256 32 BE) + ...
        if value.len() >= 118 {
            let addr = Address::from_slice(&value[..20]);
            let commission_bps = u16::from_le_bytes([value[52], value[53]]);
            let self_stake = U256::from_be_slice(&value[54..86]);
            let total_delegated = U256::from_be_slice(&value[86..118]);
            let total = self_stake + total_delegated;

            validators.push(abi::encode_address(&addr));
            stakes.push(abi::encode_u128(u256_to_u128_saturating(total)));
            commissions.push(abi::encode_u128(commission_bps as u128));
        }
    }

    Ok(abi::encode_arrays_response(&[&validators, &stakes, &commissions]))
}

/// Saturating conversion from U256 to u128.
fn u256_to_u128_saturating(val: U256) -> u128 {
    let bytes = val.to_be_bytes::<32>();
    // If any of the top 16 bytes are non-zero, saturate to u128::MAX
    if bytes[..16].iter().any(|&b| b != 0) {
        u128::MAX
    } else {
        u128::from_be_bytes(bytes[16..32].try_into().unwrap())
    }
}

// ============================================================================
// CoreWriter (0x0810) — task 2.4.3
// ============================================================================

fn core_writer(
    input: &[u8],
    caller: &Address,
    state_db: &StateDb,
    current_block: u64,
) -> Result<Vec<u8>, CoreError> {
    let sel = abi::selector(input)?;

    if sel == selector_for("placeOrder(bytes32,uint8,uint8,uint128,uint128,uint8)") {
        let market_id = abi::decode_market_id(&abi::word(input, 0)?);
        let side = abi::decode_u8(&abi::word(input, 1)?);
        let order_type = abi::decode_u8(&abi::word(input, 2)?);
        let price = abi::decode_u128(&abi::word(input, 3)?);
        let quantity = abi::decode_u128(&abi::word(input, 4)?);
        let time_in_force = abi::decode_u8(&abi::word(input, 5)?);

        let action = QueuedAction {
            trader: *caller,
            kind: QueuedActionKind::PlaceOrder {
                market_id,
                side,
                order_type,
                price: FixedPoint::from_raw(price as i128),
                quantity: FixedPoint::from_raw(quantity as i128),
                time_in_force,
            },
            block_queued: current_block,
        };

        let seq = CoreWriterQueue::enqueue(state_db, &action)?;
        // Return a deterministic order ID: block(8) + seq(8) packed into u128
        let order_id: u128 = ((current_block + 1) as u128) << 64 | seq as u128;
        Ok(abi::encode_order_id(order_id).to_vec())
    } else if sel == selector_for("cancelOrder(bytes32)") {
        let order_id = abi::decode_order_id(&abi::word(input, 0)?);

        let action = QueuedAction {
            trader: *caller,
            kind: QueuedActionKind::CancelOrder { order_id },
            block_queued: current_block,
        };
        CoreWriterQueue::enqueue(state_db, &action)?;
        Ok(abi::encode_bool(true).to_vec())
    } else if sel == selector_for("cancelAll(bytes32)") {
        let market_id = abi::decode_market_id(&abi::word(input, 0)?);

        let action = QueuedAction {
            trader: *caller,
            kind: QueuedActionKind::CancelAll { market_id },
            block_queued: current_block,
        };
        CoreWriterQueue::enqueue(state_db, &action)?;
        // Return 0 (actual count determined at execution time)
        Ok(abi::encode_u32(0).to_vec())
    } else {
        Err(CoreError::UnknownSelector(sel))
    }
}

// ============================================================================
// CoreWriterStaking (0x0811) — task 2.4.4
// ============================================================================

fn core_writer_staking(
    input: &[u8],
    caller: &Address,
    state_db: &StateDb,
    current_block: u64,
) -> Result<Vec<u8>, CoreError> {
    let sel = abi::selector(input)?;

    if sel == selector_for("delegate(address,uint128)") {
        let validator = abi::decode_address(&abi::word(input, 0)?);
        let amount = abi::decode_u128(&abi::word(input, 1)?);

        let action = QueuedAction {
            trader: *caller,
            kind: QueuedActionKind::Delegate {
                validator,
                amount: FixedPoint::from_raw(amount as i128),
            },
            block_queued: current_block,
        };
        CoreWriterQueue::enqueue(state_db, &action)?;
        Ok(abi::encode_bool(true).to_vec())
    } else if sel == selector_for("undelegate(address,uint128)") {
        let validator = abi::decode_address(&abi::word(input, 0)?);
        let amount = abi::decode_u128(&abi::word(input, 1)?);

        let action = QueuedAction {
            trader: *caller,
            kind: QueuedActionKind::Undelegate {
                validator,
                amount: FixedPoint::from_raw(amount as i128),
            },
            block_queued: current_block,
        };
        CoreWriterQueue::enqueue(state_db, &action)?;
        Ok(abi::encode_bool(true).to_vec())
    } else if sel == selector_for("claimRewards()") {
        let action = QueuedAction {
            trader: *caller,
            kind: QueuedActionKind::ClaimRewards,
            block_queued: current_block,
        };
        CoreWriterQueue::enqueue(state_db, &action)?;
        // Actual amount determined at execution time
        Ok(abi::encode_u128(0).to_vec())
    } else if sel == selector_for("lockPermanent(uint128)") {
        let amount = abi::decode_u128(&abi::word(input, 0)?);

        let action = QueuedAction {
            trader: *caller,
            kind: QueuedActionKind::LockPermanent {
                amount: FixedPoint::from_raw(amount as i128),
            },
            block_queued: current_block,
        };
        CoreWriterQueue::enqueue(state_db, &action)?;
        Ok(abi::encode_bool(true).to_vec())
    } else {
        Err(CoreError::UnknownSelector(sel))
    }
}

// ============================================================================
// Lockbox Precompile (0x0820) — task 2.4.5
// ============================================================================

fn lockbox_precompile(
    input: &[u8],
    caller: &Address,
    state_db: &StateDb,
) -> Result<Vec<u8>, CoreError> {
    let sel = abi::selector(input)?;

    if sel == selector_for("depositToNative(uint128)") {
        let amount_raw = abi::decode_u128(&abi::word(input, 0)?);
        let amount = FixedPoint::from_raw(amount_raw as i128);
        Lockbox::deposit_to_native(state_db, caller, amount)?;
        Ok(abi::encode_bool(true).to_vec())
    } else if sel == selector_for("withdrawFromNative(uint128)") {
        let amount_raw = abi::decode_u128(&abi::word(input, 0)?);
        let amount = FixedPoint::from_raw(amount_raw as i128);
        Lockbox::withdraw_from_native(state_db, caller, amount)?;
        Ok(abi::encode_bool(true).to_vec())
    } else {
        Err(CoreError::UnknownSelector(sel))
    }
}

// ============================================================================
// CoreWriterQueue — delayed execution queue (task 2.4.6)
// ============================================================================

pub struct CoreWriterQueue;

impl CoreWriterQueue {
    /// Enqueue a CoreWriter action for execution in the next block.
    /// Returns the sequence number assigned to this action.
    pub fn enqueue(state_db: &StateDb, action: &QueuedAction) -> Result<u64, CoreError> {
        let target_block = action.block_queued + 1; // anti-frontrunning: execute next block

        // Determine next sequence number for this target block
        let seq = Self::next_sequence(state_db, target_block)?;

        // Key: target_block(8 BE) + sequence(8 BE)
        let mut key = [0u8; 16];
        key[..8].copy_from_slice(&target_block.to_be_bytes());
        key[8..16].copy_from_slice(&seq.to_be_bytes());

        let data = borsh::to_vec(action).map_err(|e| CoreError::Borsh(e.to_string()))?;
        state_db.put_cf_raw(CF_CORE_WRITER_QUEUE, &key, &data)?;

        Ok(seq)
    }

    /// Drain all actions queued for the given block_number.
    /// Returns the actions and deletes them from state.
    pub fn drain(
        state_db: &StateDb,
        block_number: u64,
    ) -> Result<Vec<QueuedAction>, CoreError> {
        let db = state_db.inner();
        let cf = db
            .cf_handle(CF_CORE_WRITER_QUEUE)
            .ok_or(CoreError::MissingCf(CF_CORE_WRITER_QUEUE))?;

        let prefix = block_number.to_be_bytes();
        let iter = db.prefix_iterator_cf(cf, &prefix);

        let mut actions = Vec::new();
        let mut keys_to_delete = Vec::new();

        for item in iter {
            let (key, value) =
                item.map_err(|e| CoreError::State(torus_state::StateError::RocksDb(e)))?;
            if !key.starts_with(&prefix) {
                break;
            }
            if let Ok(action) = QueuedAction::try_from_slice(&value) {
                actions.push(action);
            }
            keys_to_delete.push(key.to_vec());
        }

        // Delete drained entries
        for key in &keys_to_delete {
            state_db.delete_cf_raw(CF_CORE_WRITER_QUEUE, key)?;
        }

        Ok(actions)
    }

    /// Count pending actions for a given block number.
    pub fn pending_count(state_db: &StateDb, block_number: u64) -> Result<usize, CoreError> {
        let db = state_db.inner();
        let cf = db
            .cf_handle(CF_CORE_WRITER_QUEUE)
            .ok_or(CoreError::MissingCf(CF_CORE_WRITER_QUEUE))?;

        let prefix = block_number.to_be_bytes();
        let iter = db.prefix_iterator_cf(cf, &prefix);

        let mut count = 0usize;
        for item in iter {
            let (key, _) =
                item.map_err(|e| CoreError::State(torus_state::StateError::RocksDb(e)))?;
            if !key.starts_with(&prefix) {
                break;
            }
            count += 1;
        }

        Ok(count)
    }

    /// Find the next sequence number for a target block by scanning existing keys.
    fn next_sequence(state_db: &StateDb, target_block: u64) -> Result<u64, CoreError> {
        let db = state_db.inner();
        let cf = db
            .cf_handle(CF_CORE_WRITER_QUEUE)
            .ok_or(CoreError::MissingCf(CF_CORE_WRITER_QUEUE))?;

        let prefix = target_block.to_be_bytes();
        let iter = db.prefix_iterator_cf(cf, &prefix);

        let mut max_seq: u64 = 0;
        let mut found = false;
        for item in iter {
            let (key, _) =
                item.map_err(|e| CoreError::State(torus_state::StateError::RocksDb(e)))?;
            if !key.starts_with(&prefix) {
                break;
            }
            if key.len() == 16 {
                let seq = u64::from_be_bytes(key[8..16].try_into().unwrap());
                if seq >= max_seq {
                    max_seq = seq;
                    found = true;
                }
            }
        }

        Ok(if found { max_seq + 1 } else { 0 })
    }
}

// ============================================================================
// Queued Action Types
// ============================================================================

/// A CoreWriter action queued for delayed execution.
#[derive(Clone, Debug)]
pub struct QueuedAction {
    pub trader: Address,
    pub kind: QueuedActionKind,
    pub block_queued: u64,
}

/// The specific action being queued.
#[derive(Clone, Debug)]
pub enum QueuedActionKind {
    PlaceOrder {
        market_id: MarketId,
        side: u8,
        order_type: u8,
        price: FixedPoint,
        quantity: FixedPoint,
        time_in_force: u8,
    },
    CancelOrder {
        order_id: OrderId,
    },
    CancelAll {
        market_id: MarketId,
    },
    Delegate {
        validator: Address,
        amount: FixedPoint,
    },
    Undelegate {
        validator: Address,
        amount: FixedPoint,
    },
    ClaimRewards,
    LockPermanent {
        amount: FixedPoint,
    },
}

impl BorshSerialize for QueuedAction {
    fn serialize<W: IoWrite>(&self, w: &mut W) -> io::Result<()> {
        borsh_write_address(&self.trader, w)?;
        w.write_all(&self.block_queued.to_be_bytes())?;
        BorshSerialize::serialize(&self.kind, w)?;
        Ok(())
    }
}

impl BorshDeserialize for QueuedAction {
    fn deserialize_reader<R: IoRead>(r: &mut R) -> io::Result<Self> {
        let trader = borsh_read_address(r)?;
        let mut bb = [0u8; 8];
        r.read_exact(&mut bb)?;
        let block_queued = u64::from_be_bytes(bb);
        let kind = QueuedActionKind::deserialize_reader(r)?;
        Ok(Self {
            trader,
            kind,
            block_queued,
        })
    }
}

impl BorshSerialize for QueuedActionKind {
    fn serialize<W: IoWrite>(&self, w: &mut W) -> io::Result<()> {
        match self {
            Self::PlaceOrder {
                market_id,
                side,
                order_type,
                price,
                quantity,
                time_in_force,
            } => {
                w.write_all(&[0])?;
                w.write_all(&market_id.to_be_bytes())?;
                w.write_all(&[*side])?;
                w.write_all(&[*order_type])?;
                borsh_write_fp(price, w)?;
                borsh_write_fp(quantity, w)?;
                w.write_all(&[*time_in_force])?;
            }
            Self::CancelOrder { order_id } => {
                w.write_all(&[1])?;
                w.write_all(&order_id.to_be_bytes())?;
            }
            Self::CancelAll { market_id } => {
                w.write_all(&[2])?;
                w.write_all(&market_id.to_be_bytes())?;
            }
            Self::Delegate { validator, amount } => {
                w.write_all(&[3])?;
                borsh_write_address(validator, w)?;
                borsh_write_fp(amount, w)?;
            }
            Self::Undelegate { validator, amount } => {
                w.write_all(&[4])?;
                borsh_write_address(validator, w)?;
                borsh_write_fp(amount, w)?;
            }
            Self::ClaimRewards => {
                w.write_all(&[5])?;
            }
            Self::LockPermanent { amount } => {
                w.write_all(&[6])?;
                borsh_write_fp(amount, w)?;
            }
        }
        Ok(())
    }
}

impl BorshDeserialize for QueuedActionKind {
    fn deserialize_reader<R: IoRead>(r: &mut R) -> io::Result<Self> {
        let mut disc = [0u8; 1];
        r.read_exact(&mut disc)?;
        match disc[0] {
            0 => {
                let mut mb = [0u8; 8];
                r.read_exact(&mut mb)?;
                let market_id = u64::from_be_bytes(mb);
                let mut sb = [0u8; 1];
                r.read_exact(&mut sb)?;
                let side = sb[0];
                let mut ot = [0u8; 1];
                r.read_exact(&mut ot)?;
                let order_type = ot[0];
                let price = borsh_read_fp(r)?;
                let quantity = borsh_read_fp(r)?;
                let mut tf = [0u8; 1];
                r.read_exact(&mut tf)?;
                Ok(Self::PlaceOrder {
                    market_id,
                    side,
                    order_type,
                    price,
                    quantity,
                    time_in_force: tf[0],
                })
            }
            1 => {
                let mut ob = [0u8; 16];
                r.read_exact(&mut ob)?;
                Ok(Self::CancelOrder {
                    order_id: u128::from_be_bytes(ob),
                })
            }
            2 => {
                let mut mb = [0u8; 8];
                r.read_exact(&mut mb)?;
                Ok(Self::CancelAll {
                    market_id: u64::from_be_bytes(mb),
                })
            }
            3 => {
                let validator = borsh_read_address(r)?;
                let amount = borsh_read_fp(r)?;
                Ok(Self::Delegate { validator, amount })
            }
            4 => {
                let validator = borsh_read_address(r)?;
                let amount = borsh_read_fp(r)?;
                Ok(Self::Undelegate { validator, amount })
            }
            5 => Ok(Self::ClaimRewards),
            6 => {
                let amount = borsh_read_fp(r)?;
                Ok(Self::LockPermanent { amount })
            }
            x => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("invalid QueuedActionKind discriminant: {x}"),
            )),
        }
    }
}

// ============================================================================
// Stored Types — CF formats for order book data
// ============================================================================

/// A single price level in the order book (persisted in CF_NATIVE_ORDER_BOOKS).
#[derive(Clone, Debug)]
pub struct PriceLevel {
    pub price: FixedPoint,
    pub quantity: FixedPoint,
}

/// Snapshot of order book price levels (persisted in CF_NATIVE_ORDER_BOOKS).
/// Key: market_id(8 BE). Value: borsh-serialized.
#[derive(Clone, Debug)]
pub struct OrderBookSnapshot {
    pub bids: Vec<PriceLevel>,
    pub asks: Vec<PriceLevel>,
}

impl BorshSerialize for OrderBookSnapshot {
    fn serialize<W: IoWrite>(&self, w: &mut W) -> io::Result<()> {
        w.write_all(&(self.bids.len() as u32).to_be_bytes())?;
        for level in &self.bids {
            borsh_write_fp(&level.price, w)?;
            borsh_write_fp(&level.quantity, w)?;
        }
        w.write_all(&(self.asks.len() as u32).to_be_bytes())?;
        for level in &self.asks {
            borsh_write_fp(&level.price, w)?;
            borsh_write_fp(&level.quantity, w)?;
        }
        Ok(())
    }
}

impl BorshDeserialize for OrderBookSnapshot {
    fn deserialize_reader<R: IoRead>(r: &mut R) -> io::Result<Self> {
        let mut cb = [0u8; 4];
        r.read_exact(&mut cb)?;
        let bid_count = u32::from_be_bytes(cb) as usize;
        let mut bids = Vec::with_capacity(bid_count);
        for _ in 0..bid_count {
            let price = borsh_read_fp(r)?;
            let quantity = borsh_read_fp(r)?;
            bids.push(PriceLevel { price, quantity });
        }

        r.read_exact(&mut cb)?;
        let ask_count = u32::from_be_bytes(cb) as usize;
        let mut asks = Vec::with_capacity(ask_count);
        for _ in 0..ask_count {
            let price = borsh_read_fp(r)?;
            let quantity = borsh_read_fp(r)?;
            asks.push(PriceLevel { price, quantity });
        }

        Ok(Self { bids, asks })
    }
}

/// An open order stored in CF_NATIVE_ORDERS.
/// Key: trader(20) + market_id(8) + order_id(16) = 44 bytes.
/// Value: borsh-serialized StoredOrder.
#[derive(Clone, Debug)]
pub struct StoredOrder {
    pub order_id: OrderId,
    pub price: FixedPoint,
    pub remaining_qty: FixedPoint,
    pub side: u8, // 0 = Buy, 1 = Sell
}

impl BorshSerialize for StoredOrder {
    fn serialize<W: IoWrite>(&self, w: &mut W) -> io::Result<()> {
        w.write_all(&self.order_id.to_be_bytes())?;
        borsh_write_fp(&self.price, w)?;
        borsh_write_fp(&self.remaining_qty, w)?;
        w.write_all(&[self.side])?;
        Ok(())
    }
}

impl BorshDeserialize for StoredOrder {
    fn deserialize_reader<R: IoRead>(r: &mut R) -> io::Result<Self> {
        let mut ob = [0u8; 16];
        r.read_exact(&mut ob)?;
        let order_id = u128::from_be_bytes(ob);
        let price = borsh_read_fp(r)?;
        let remaining_qty = borsh_read_fp(r)?;
        let mut sb = [0u8; 1];
        r.read_exact(&mut sb)?;
        Ok(Self {
            order_id,
            price,
            remaining_qty,
            side: sb[0],
        })
    }
}

/// Write an order book snapshot to CF_NATIVE_ORDER_BOOKS.
/// Used by the bridge layer to populate precompile-readable state.
pub fn write_order_book_snapshot(
    state_db: &StateDb,
    market_id: MarketId,
    snapshot: &OrderBookSnapshot,
) -> Result<(), CoreError> {
    let key = market_id.to_be_bytes();
    let data = borsh::to_vec(snapshot).map_err(|e| CoreError::Borsh(e.to_string()))?;
    state_db.put_cf_raw(CF_NATIVE_ORDER_BOOKS, &key, &data)?;
    Ok(())
}

/// Write a stored order to CF_NATIVE_ORDERS.
/// Key: trader(20) + market_id(8) + order_id(16).
pub fn write_stored_order(
    state_db: &StateDb,
    trader: &Address,
    market_id: MarketId,
    order: &StoredOrder,
) -> Result<(), CoreError> {
    let mut key = [0u8; 44];
    key[..20].copy_from_slice(trader.as_slice());
    key[20..28].copy_from_slice(&market_id.to_be_bytes());
    key[28..44].copy_from_slice(&order.order_id.to_be_bytes());
    let data = borsh::to_vec(order).map_err(|e| CoreError::Borsh(e.to_string()))?;
    state_db.put_cf_raw(CF_NATIVE_ORDERS, &key, &data)?;
    Ok(())
}
