//! Cross-VM precompiles — ABI-encoded bridges between EVM and native state (task 2.4).
//!
//! Precompile address map:
//!   0x0800 — OrderBookReader (read)
//!   0x0801 — BalanceReader (read)
//!   0x0802 — OracleReader (read)
//!   0x0803 — StakingReader (read)
//!   0x0810 — CoreWriter (write: orders, delayed)
//!   0x0811 — CoreWriterStaking (write: staking, delayed)
//!   0x0820 — Lockbox (write: EVM <-> native transfers, native leg delayed)

use std::io::{self, Read as IoRead, Write as IoWrite};

use alloy_primitives::keccak256;
use borsh::{BorshDeserialize, BorshSerialize};
use torus_state::cf::*;
use torus_state::{StateBackend, StateDb};
use torus_types::{Address, FixedPoint, MarketId, OrderId, U256};

use crate::error::CoreError;
use crate::lockbox::{wei_to_fp_floor, WEI_PER_NATIVE_UNIT};
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
///
/// Base gas of every reader call (0x0800-0x0803). s99 owner decision (item6
/// plan 9.16, revisit later): single reads at Hyperliquid level, ~16,500 gas
/// per position read. getPosition = this base + its 5 answer words = 16,500;
/// a 30M-gas block of cold getPosition calls (ozarchy, block cache cold) runs
/// in ~32-37 ms (`docs/perf/read-precompile-gas.md`, section s99). The base
/// pays for a reader's point reads (getPosition, getBalances, getPrice, the
/// staking point rows, the classic blob probe) and a scan's fixed cost.
pub const GAS_PRECOMPILE_READ: u64 = 16_400;
/// Gas per hashed row a reader's prefix scan returned (s99: scans 500 gas per
/// scanned row + 20 per returned word or blob chunk).
pub const GAS_PRECOMPILE_READ_PER_ROW: u64 = 500;
/// Gas per 32-byte word a reader returned, and per 32 bytes of a classic
/// whole-book blob it read (sized and charged before it is read).
pub const GAS_PRECOMPILE_READ_PER_WORD: u64 = 20;
/// Most price levels per side one getOrderBook call answers (s99 owner
/// decision, final): the 64 best bids and the 64 best asks of a mode 2 / 3
/// market (level rows), no revert past the cap (see [`book_levels`]).
pub const READER_MAX_LEVELS_PER_SIDE: u64 = 64;
/// State-mutating writes (SSTORE equivalent range).
pub const GAS_PRECOMPILE_WRITE: u64 = 20_000;
/// Complex operations (governance, liquidation).
pub const GAS_PRECOMPILE_COMPLEX: u64 = 50_000;

/// Gas of a reader call (0x0800-0x0803) whose meter used `used` gas above
/// the base ([`ReadMeter::used`]): [`GAS_PRECOMPILE_READ`] + `used`.
/// Charged by the EVM provider after the call, on success and on revert.
pub const fn reader_gas(used: u64) -> u64 {
    GAS_PRECOMPILE_READ.saturating_add(used)
}

/// The gas a reader call with `gas_limit` may use above the base (0 when it
/// does not even cover the base): its [`ReadMeter`] cap.
pub const fn reader_budget(gas_limit: u64) -> u64 {
    gas_limit.saturating_sub(GAS_PRECOMPILE_READ)
}

/// Work meter of one reader call, in gas above the base: 500 per row read
/// ([`GAS_PRECOMPILE_READ_PER_ROW`]) and 20 per word returned or 32 blob bytes
/// read ([`GAS_PRECOMPILE_READ_PER_WORD`]), capped at what the caller's gas
/// pays for. A reader charges BEFORE it does the work it charges for where it
/// can (scans stop at the budget, a classic blob is charged before it is
/// decoded), so the node's work per call is bounded by the call's gas, not
/// only its price. RocksDB deletion markers a scan steps over are never
/// charged: their count is node-local (flush / compaction history), and gas
/// is a block result.
#[derive(Clone, Copy, Debug)]
pub struct ReadMeter {
    max: u64,
    used: u64,
}

impl ReadMeter {
    /// No cap (non-EVM callers and tests): readers take their unbounded paths.
    pub const fn unlimited() -> Self {
        Self { max: u64::MAX, used: 0 }
    }

    /// At most `max` units.
    pub const fn with_max(max: u64) -> Self {
        Self { max, used: 0 }
    }

    /// Gas used so far above the base.
    pub const fn used(&self) -> u64 {
        self.used
    }

    fn is_unlimited(&self) -> bool {
        self.max == u64::MAX
    }

    fn remaining(&self) -> u64 {
        self.max.saturating_sub(self.used)
    }

    /// Rows the remaining gas pays for.
    fn rows_left(&self) -> u64 {
        self.remaining() / GAS_PRECOMPILE_READ_PER_ROW
    }

    /// Use `gas`; out of gas once the total exceeds the cap.
    fn charge(&mut self, gas: u64) -> Result<(), CoreError> {
        self.used = self.used.saturating_add(gas);
        if self.used > self.max {
            return Err(CoreError::PrecompileOutOfGas);
        }
        Ok(())
    }

    fn charge_rows(&mut self, rows: u64) -> Result<(), CoreError> {
        self.charge(rows.saturating_mul(GAS_PRECOMPILE_READ_PER_ROW))
    }

    /// Words returned, or 32-byte blob chunks read.
    fn charge_words(&mut self, words: u64) -> Result<(), CoreError> {
        self.charge(words.saturating_mul(GAS_PRECOMPILE_READ_PER_WORD))
    }

    /// Out of gas unless `words` more would still fit (a check, no charge).
    fn require_words(&self, words: u64) -> Result<(), CoreError> {
        if words.saturating_mul(GAS_PRECOMPILE_READ_PER_WORD) > self.remaining() {
            return Err(CoreError::PrecompileOutOfGas);
        }
        Ok(())
    }
}

/// Consensus rows of `cf` whose key starts with `prefix`, in key order, at
/// most `max_rows` (the first ones; `None` = all), each charged a row. Only
/// keys the running state hash covers (`running_hash::key_is_hashed`) are
/// charged and returned: a node-local row
/// (the `__book_mode__` marker in `cf_native_markets`) is skipped, so the
/// charge — a block result — is the same on every node.
///
/// Only hashed CFs may be scanned: a CF the running state hash does not cover
/// at all (node-local, e.g. `cf_book_order_rows`) is an error, never "charge
/// everything" — its rows differ between nodes.
///
/// Metered (or capped): pages of at most 64 rows through
/// `iterate_cf_prefix_from` (RocksDB bounded at the prefix end), each page no
/// larger than the rows still allowed + 1 and the rows `max_rows` still
/// allows; out of gas as soon as the charged rows exceed the budget. A scan
/// therefore reads at most `min(rows paid + 1, max_rows)` rows, plus the
/// node-local rows it skips, plus the RocksDB deletion markers inside the
/// prefix that the iterator steps over uncharged (node-local; bounded by the
/// background compaction a delete flush schedules for the order ranges,
/// `StateDb::compact_range_in_background`). Unlimited and uncapped: one
/// `iterate_cf` (the pre-existing read). A missing CF reads as empty.
fn scan_prefix_metered(
    state: &impl StateBackend,
    cf: &'static str,
    prefix: &[u8],
    max_rows: Option<u64>,
    meter: &mut ReadMeter,
) -> Result<Vec<(Vec<u8>, Vec<u8>)>, CoreError> {
    const PAGE: u64 = 64;
    let missing_is_empty = |e: torus_state::StateError| match e {
        torus_state::StateError::MissingColumnFamily(_) => Ok(Vec::new()),
        e => Err(CoreError::State(e)),
    };
    let Some(hashed_id) = torus_state::running_hash::hashed_cf_id(cf) else {
        return Err(CoreError::InvalidPrecompileInput(format!(
            "reader scan of {cf}: not a hashed consensus CF (its charge would be node-dependent)"
        )));
    };
    let consensus = |k: &[u8]| torus_state::running_hash::key_is_hashed(hashed_id, k);
    if meter.is_unlimited() && max_rows.is_none() {
        let p = (!prefix.is_empty()).then_some(prefix);
        let mut rows = state.iterate_cf(cf, p).or_else(missing_is_empty)?;
        rows.retain(|(k, _)| consensus(k));
        meter.charge_rows(rows.len() as u64)?;
        return Ok(rows);
    }
    let max_rows = max_rows.unwrap_or(u64::MAX);
    let mut out: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
    let mut start = prefix.to_vec();
    while (out.len() as u64) < max_rows {
        // Rows still paid for, plus one that proves the budget is exceeded,
        // and never past the cap.
        let left = meter
            .rows_left()
            .saturating_sub(out.len() as u64)
            .saturating_add(1);
        let page = left.min(PAGE).min(max_rows - out.len() as u64) as usize;
        let rows = state
            .iterate_cf_prefix_from(cf, prefix, &start, page)
            .or_else(missing_is_empty)?;
        let ended = rows.len() < page;
        let last = rows.last().map(|(k, _)| k.clone());
        out.extend(rows.into_iter().filter(|(k, _)| consensus(k)));
        if out.len() as u64 > meter.rows_left() {
            return Err(CoreError::PrecompileOutOfGas);
        }
        match last {
            Some(mut k) if !ended => {
                // The smallest key after the last one read.
                k.push(0);
                start = k;
            }
            _ => break,
        }
    }
    meter.charge_rows(out.len() as u64)?;
    Ok(out)
}

/// Base gas cost for a precompile by address ID (a reader's full cost is
/// [`reader_gas`] of the gas its [`ReadMeter`] used).
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
    Address::new([
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, b[0], b[1],
    ])
}

/// All Torus precompile addresses (for EVM warm-address injection).
pub const ALL_PRECOMPILE_ADDRESSES: [Address; 7] = [
    precompile_address(ADDR_ORDER_BOOK_READER),
    precompile_address(ADDR_BALANCE_READER),
    precompile_address(ADDR_ORACLE_READER),
    precompile_address(ADDR_STAKING_READER),
    precompile_address(ADDR_CORE_WRITER),
    precompile_address(ADDR_CORE_WRITER_STAKING),
    precompile_address(ADDR_LOCKBOX),
];

/// Check whether an address is a Torus precompile.
pub fn is_precompile(address: &Address) -> bool {
    let bytes = address.as_slice();
    bytes[..18].iter().all(|&b| b == 0) && {
        let id = u16::from_be_bytes([bytes[18], bytes[19]]);
        matches!(id, 0x0800..=0x0803 | 0x0810..=0x0811 | 0x0820)
    }
}

/// Whether a precompile id is a read-only reader (0x0800–0x0803), safe to run in an
/// eth_call / eth_estimateGas simulation. The writer precompiles (CoreWriter 0x0810,
/// CoreWriterStaking 0x0811, Lockbox 0x0820) mutate shared state and must be denied there.
pub const fn is_reader_precompile(id: u16) -> bool {
    matches!(id, ADDR_ORDER_BOOK_READER..=ADDR_STAKING_READER)
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

    /// F1/D1 (s517): a balance that may be negative, as a `uint128`:
    /// negative → 0 (the ABI stays `uint128`; a raw cast would wrap it to
    /// ~2^128).
    pub fn encode_balance_as_u128(fp: FixedPoint) -> [u8; 32] {
        encode_u128(fp.raw().max(0) as u128)
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
/// * `state_db` — state access. T4.4: any [`StateBackend`] — real block execution passes
///   a per-tx journaled overlay so writer side effects only become durable on tx success
///   (reads fall through to the base DB, preserving read-your-writes within the tx)
/// * `current_block` — current block number
/// * `current_timestamp` — current block's header timestamp (seconds): the clock
///   of the oracle staleness rule (0x0802 stale flags, 0x0800 UPnL)
pub fn execute_precompile(
    address: &Address,
    input: &[u8],
    caller: &Address,
    state_db: &impl StateBackend,
    current_block: u64,
    current_timestamp: u64,
) -> Result<Vec<u8>, CoreError> {
    execute_precompile_inner(
        address,
        input,
        caller,
        U256::ZERO,
        state_db,
        current_block,
        current_timestamp,
        false,
        &mut ReadMeter::unlimited(),
    )
}

/// [`execute_precompile`] for a call that carries EVM value.
///
/// `call_value` is the wei revm has ALREADY moved from `caller` into the precompile
/// address for this frame (a CALL's transferred value; zero for any other scheme). Only
/// the lockbox `depositToNative` accepts value — every other precompile / selector
/// rejects it, which reverts the frame and returns the wei to the caller. The caller of
/// this function (the EVM precompile provider) owns burning an accepted deposit's value
/// from the precompile address inside the same frame.
pub fn execute_precompile_with_value(
    address: &Address,
    input: &[u8],
    caller: &Address,
    call_value: U256,
    state_db: &impl StateBackend,
    current_block: u64,
    current_timestamp: u64,
) -> Result<Vec<u8>, CoreError> {
    execute_precompile_inner(
        address,
        input,
        caller,
        call_value,
        state_db,
        current_block,
        current_timestamp,
        false,
        &mut ReadMeter::unlimited(),
    )
}

/// Read-only variant for eth_call / eth_estimateGas simulation.
///
/// SECURITY: the writer precompiles (CoreWriter 0x0810, CoreWriterStaking 0x0811, Lockbox
/// 0x0820) mutate the shared `StateDb` directly, bypassing the EVM's revert sandbox. Run in
/// an eth_call/estimateGas simulation they would durably mutate consensus state out of
/// consensus — diverging this node's state root from the network (state root mismatch →
/// fork/halt) and, for CoreWriter, enqueuing an action that the next block drains and
/// executes on this node only. This variant denies them (revert) while readers still work.
pub fn execute_precompile_read_only(
    address: &Address,
    input: &[u8],
    caller: &Address,
    state_db: &impl StateBackend,
    current_block: u64,
    current_timestamp: u64,
) -> Result<Vec<u8>, CoreError> {
    execute_precompile_inner(
        address,
        input,
        caller,
        U256::ZERO,
        state_db,
        current_block,
        current_timestamp,
        true,
        &mut ReadMeter::unlimited(),
    )
}

/// The EVM provider's entry: [`execute_precompile_with_value`] (or, with
/// `read_only`, [`execute_precompile_read_only`]) where a reader's work is
/// metered by `meter` — rows read and words returned, `PrecompileOutOfGas`
/// once over its cap. `meter.used()` is the work done, also after an error.
/// Writers ignore the meter.
#[allow(clippy::too_many_arguments)]
pub fn execute_precompile_metered(
    address: &Address,
    input: &[u8],
    caller: &Address,
    call_value: U256,
    state_db: &impl StateBackend,
    current_block: u64,
    current_timestamp: u64,
    read_only: bool,
    meter: &mut ReadMeter,
) -> Result<Vec<u8>, CoreError> {
    execute_precompile_inner(
        address,
        input,
        caller,
        call_value,
        state_db,
        current_block,
        current_timestamp,
        read_only,
        meter,
    )
}

#[allow(clippy::too_many_arguments)]
fn execute_precompile_inner(
    address: &Address,
    input: &[u8],
    caller: &Address,
    call_value: U256,
    state_db: &impl StateBackend,
    current_block: u64,
    current_timestamp: u64,
    read_only: bool,
    meter: &mut ReadMeter,
) -> Result<Vec<u8>, CoreError> {
    let bytes = address.as_slice();
    if !bytes[..18].iter().all(|&b| b == 0) {
        return Err(CoreError::InvalidPrecompileInput(
            "invalid precompile address prefix".into(),
        ));
    }
    let id = u16::from_be_bytes([bytes[18], bytes[19]]);

    // Default-deny: in a read-only (eth_call) context, only reader precompiles may run.
    // Any state-mutating (or unknown) precompile reverts instead of touching the DB.
    if read_only && !is_reader_precompile(id) {
        return Err(CoreError::InvalidPrecompileInput(
            "state-mutating precompile invoked in read-only (eth_call) context".into(),
        ));
    }

    // Non-payable: value sent to any Torus precompile other than the lockbox would be
    // stranded at the precompile address forever. Reverting returns it.
    if !call_value.is_zero() && id != ADDR_LOCKBOX {
        return Err(CoreError::InvalidPrecompileInput(format!(
            "precompile 0x{id:04x} is not payable"
        )));
    }

    // Readers: the answer's words are work too, charged once it is built.
    let read = |out: Result<Vec<u8>, CoreError>, meter: &mut ReadMeter| {
        let out = out?;
        meter.charge_words((out.len() as u64).div_ceil(32))?;
        Ok(out)
    };
    match id {
        ADDR_ORDER_BOOK_READER => read(order_book_reader(input, state_db, current_timestamp, meter), meter),
        ADDR_BALANCE_READER => read(balance_reader(input, state_db, meter), meter),
        ADDR_ORACLE_READER => read(oracle_reader(input, state_db, current_timestamp, meter), meter),
        ADDR_STAKING_READER => read(staking_reader(input, state_db, meter), meter),
        ADDR_CORE_WRITER => core_writer(input, caller, state_db, current_block),
        ADDR_CORE_WRITER_STAKING => core_writer_staking(input, caller, state_db, current_block),
        ADDR_LOCKBOX => lockbox_precompile(input, caller, call_value, state_db, current_block),
        _ => Err(CoreError::InvalidPrecompileInput(format!(
            "unknown precompile 0x{id:04x}"
        ))),
    }
}

// ============================================================================
// OrderBookReader (0x0800) — tasks 2.4.1
// ============================================================================

fn order_book_reader(
    input: &[u8],
    state_db: &impl StateBackend,
    now: u64,
    meter: &mut ReadMeter,
) -> Result<Vec<u8>, CoreError> {
    let sel = abi::selector(input)?;

    if sel == selector_for("getOrderBook(bytes32)") {
        let market_id = abi::decode_market_id(&abi::word(input, 0)?);
        read_order_book(state_db, market_id, meter)
    } else if sel == selector_for("getPosition(address,bytes32)") {
        let trader = abi::decode_address(&abi::word(input, 0)?);
        let market_id = abi::decode_market_id(&abi::word(input, 1)?);
        read_position(state_db, &trader, market_id, now)
    } else {
        Err(CoreError::UnknownSelector(sel))
    }
}

/// `(price, quantity)` per aggregated price level, best-first.
type PriceQtyLevels = Vec<(FixedPoint, FixedPoint)>;

/// getOrderBook → (uint128[] bid_prices, uint128[] bid_qtys, uint128[] ask_prices, uint128[] ask_qtys)
///
/// EVM-VISIBLE / CONSENSUS-RELEVANT. Layout handling is deliberately
/// asymmetric:
///
/// * **Row layouts (`TORUS_BOOK_ROWS` 1 / 2)** — this reader used to look only
///   for the classic 8-byte key, find nothing, and hand contracts an EMPTY
///   book. It now serves the real depth (mode 2 straight from the ROOT-CF
///   level rows, i.e. the consensus aggregate). Those layouts require a fresh
///   genesis, so no already-deployed chain observes a change here.
/// * **Classic** — UNCHANGED on purpose. It still decodes only the legacy
///   `OrderBookSnapshot`, so a production whole-book blob still makes this
///   precompile revert. Changing that alters EVM-visible output for an
///   already-deployed layout: a consensus decision, not a refactor. Pinned by
///   `torus-bridge/tests/book_read_modes_tests.rs::
///   classic_precompile_behaviour_is_unchanged`.
///
/// GAS / WORK: metered by `meter` ([`ReadMeter`]): 500 per book row read
/// (20 per 32 bytes of a classic blob, sized and charged before it is read)
/// and 20 per word returned; the market's rows are scanned only up to the
/// budget and the 64-levels-per-side cap ([`READER_MAX_LEVELS_PER_SIDE`]),
/// and only this market's hashed rows are read (see [`book_levels`]).
fn read_order_book(
    state_db: &impl StateBackend,
    market_id: MarketId,
    meter: &mut ReadMeter,
) -> Result<Vec<u8>, CoreError> {
    // The answer has at least its 8-word head: a budget below it reads nothing.
    meter.require_words(8)?;
    // (price, quantity) per level, best-first.
    let (bids, asks) = book_levels(state_db, market_id, meter)?;

    let bid_prices: Vec<[u8; 32]> = bids.iter().map(|l| abi::encode_fp_as_u128(l.0)).collect();
    let bid_qtys: Vec<[u8; 32]> = bids.iter().map(|l| abi::encode_fp_as_u128(l.1)).collect();
    let ask_prices: Vec<[u8; 32]> = asks.iter().map(|l| abi::encode_fp_as_u128(l.0)).collect();
    let ask_qtys: Vec<[u8; 32]> = asks.iter().map(|l| abi::encode_fp_as_u128(l.1)).collect();

    Ok(abi::encode_arrays_response(&[
        &bid_prices,
        &bid_qtys,
        &ask_prices,
        &ask_qtys,
    ]))
}

/// Classic arm: decode only the legacy `OrderBookSnapshot` (a production
/// whole-book blob reverts — pre-existing, pinned by
/// `classic_precompile_behaviour_is_unchanged`).
fn classic_levels(data: &[u8]) -> Result<(PriceQtyLevels, PriceQtyLevels), CoreError> {
    let snapshot =
        OrderBookSnapshot::try_from_slice(data).map_err(|e| CoreError::Borsh(e.to_string()))?;
    let pairs = |levels: &[PriceLevel]| levels.iter().map(|l| (l.price, l.quantity)).collect::<Vec<_>>();
    Ok((pairs(&snapshot.bids), pairs(&snapshot.asks)))
}

/// The levels getOrderBook answers. Everything read and charged is a hashed
/// consensus row of THIS market, so the charge is the same on every node —
/// never the node-local `__book_mode__` marker or a DB-wide sniff. Every
/// caller (metered or not) gets the same answer.
///
/// * Classic iff the market's 8-byte whole-book key exists: sized with a
///   length probe and charged (20 gas per 32 bytes) BEFORE it is read, then
///   decoded whole (a blob cannot be read in part; no cap).
/// * Modes 2 / 3 (level rows, `market ‖ 0x03 ‖ side ‖ price`, 26-byte keys):
///   the [`READER_MAX_LEVELS_PER_SIDE`] (64) best levels per side, best-first
///   (two bounded scans, bids then asks: at most 128 rows). A key of another
///   length under those prefixes is a corrupt row store: revert.
/// * Mode 1 (order rows `market ‖ 0x01 ‖ order_id`, no price index): not
///   served (s99 owner decision, final). When the market has no level row, one
///   order row is probed (charged, 500 gas); if there is one, the call reverts
///   as an unsupported layout (16,400 + 500 = 16,900 gas through the EVM).
///
/// Meta and stop rows are not read. A market with neither level nor order
/// rows is an empty book. Not checked here (owner s100: it would cost an
/// extra read per call): a market holding both level and order rows, or rows
/// without a meta row. The writers never produce either (one mode per
/// process, the meta row saved with every book; a DB of another mode
/// fail-stops at load; test `row_mode_writers_keep_one_row_kind_and_a_meta_row_per_market`),
/// and the RPC readers (`book_reader::read_book_depth`) still reject both.
fn book_levels(
    state_db: &impl StateBackend,
    market_id: MarketId,
    meter: &mut ReadMeter,
) -> Result<(PriceQtyLevels, PriceQtyLevels), CoreError> {
    use crate::book_reader::{depth_from_rows, BookLayout};
    use crate::book_rows::{ROW_TAG_LEVEL, ROW_TAG_ORDER, SIDE_TAG_ASK, SIDE_TAG_BID};

    let key = market_id.to_be_bytes();
    if let Some(len) = state_db.get_cf_len(CF_NATIVE_ORDER_BOOKS, &key)? {
        meter.charge_words((len as u64).div_ceil(32))?;
        let data = state_db.get_cf_raw(CF_NATIVE_ORDER_BOOKS, &key)?.unwrap_or_default();
        return classic_levels(&data);
    }
    let sub = |tags: &[u8]| [&key[..], tags].concat();
    let side = |tag: u8, meter: &mut ReadMeter| {
        let prefix = sub(&[ROW_TAG_LEVEL, tag]);
        let max = Some(READER_MAX_LEVELS_PER_SIDE);
        scan_prefix_metered(state_db, CF_NATIVE_ORDER_BOOKS, &prefix, max, meter)
    };
    let mut rows = side(SIDE_TAG_BID, meter)?;
    rows.extend(side(SIDE_TAG_ASK, meter)?);
    if let Some((k, _)) = rows.iter().find(|(k, _)| k.len() != 26) {
        return Err(CoreError::BookLayout(format!(
            "market {market_id}: level row key of {} bytes (corrupt row store)",
            k.len()
        )));
    }
    if rows.is_empty() {
        let orders = sub(&[ROW_TAG_ORDER]);
        if !scan_prefix_metered(state_db, CF_NATIVE_ORDER_BOOKS, &orders, Some(1), meter)?
            .is_empty()
        {
            return Err(CoreError::BookLayout(format!(
                "market {market_id}: getOrderBook does not serve the order-row layout \
                 (TORUS_BOOK_ROWS=1)"
            )));
        }
    }
    let depth = depth_from_rows(market_id, BookLayout::LevelAuthority, &rows)?;
    let pairs = |levels: Vec<crate::book_reader::DepthLevel>| {
        levels
            .into_iter()
            .map(|l| (l.price, l.quantity))
            .collect::<Vec<_>>()
    };
    Ok((pairs(depth.bids), pairs(depth.asks)))
}

/// getPosition → (int128 size, uint128 entry_price, int128 unrealized_pnl, int128 realized_pnl, uint128 margin)
///
/// Item 2: UPnL uses the oracle price only while usable at `now` (block
/// timestamp, s) — the rule of `OraclePrice::usable`; otherwise 0.
fn read_position(
    state_db: &impl StateBackend,
    trader: &Address,
    market_id: MarketId,
    now: u64,
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
    let unrealized_pnl = match get_oracle_price_fp(state_db, market_id, now) {
        Ok(mark) => pos.unrealized_pnl(mark),
        // No usable price (absent, stale, non-positive): UPnL 0.
        Err(e) if !e.is_local_fault() => FixedPoint::ZERO,
        // R02 OPEN (branch 3, owner decision pending): the aggregate read
        // failed on THIS node. Still UPnL 0 as before: the EVM path has no
        // fail-stop channel yet (a precompile `Err` is a consensus-visible
        // revert, and a failed EVM section is logged and skipped), so the
        // fault cannot stop the block from here without a new rule.
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

// ============================================================================
// BalanceReader (0x0801) — tasks 2.4.1
// ============================================================================

fn balance_reader(
    input: &[u8],
    state_db: &impl StateBackend,
    meter: &mut ReadMeter,
) -> Result<Vec<u8>, CoreError> {
    let sel = abi::selector(input)?;

    if sel == selector_for("getBalances(address)") {
        let trader = abi::decode_address(&abi::word(input, 0)?);
        read_balances(state_db, &trader)
    } else if sel == selector_for("getMarkets()") {
        read_markets(state_db, meter)
    } else {
        Err(CoreError::UnknownSelector(sel))
    }
}

/// getBalances → (uint128 native_balance, uint128 evm_balance, uint128 total_margin_used, uint128 available)
fn read_balances(state_db: &impl StateBackend, trader: &Address) -> Result<Vec<u8>, CoreError> {
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
    // F1/D1 (s517): `available` may be negative — reported as 0.
    out.extend_from_slice(&abi::encode_balance_as_u128(native_bal.available));
    out.extend_from_slice(&abi::encode_u128(evm_balance));
    out.extend_from_slice(&abi::encode_fp_as_u128(native_bal.order_margin));
    // available = native_available (native balance not locked in orders)
    out.extend_from_slice(&abi::encode_balance_as_u128(native_bal.available));
    Ok(out)
}

/// getMarkets → (bytes32[] market_ids, bool[] active)
fn read_markets(state_db: &impl StateBackend, meter: &mut ReadMeter) -> Result<Vec<u8>, CoreError> {
    let entries = scan_prefix_metered(state_db, CF_NATIVE_MARKETS, &[], None, meter)?;
    let mut market_ids = Vec::new();
    let mut active_flags = Vec::new();

    for (key, value) in &entries {
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

// FIX MED-NEW-13: Use shared constant from oracle module.
use crate::oracle::DEFAULT_MAX_ORACLE_AGE_SECS;

/// Aggregate row: price(16) ‖ block(8) ‖ reporters(4) ‖ ts(8). Stale iff
/// now − ts > DEFAULT_MAX_ORACLE_AGE_SECS (saturating: age clamps at 0) — the
/// rule of `OraclePrice::usable`. `None` for a short (undecodable) row.
fn decode_agg(data: &[u8], now: u64) -> Option<(FixedPoint, u64, bool)> {
    if data.len() < 36 {
        return None;
    }
    let price = FixedPoint::from_raw(i128::from_be_bytes(data[..16].try_into().unwrap()));
    let block = u64::from_be_bytes(data[16..24].try_into().unwrap());
    let ts = u64::from_be_bytes(data[28..36].try_into().unwrap());
    Some((price, block, now.saturating_sub(ts) > DEFAULT_MAX_ORACLE_AGE_SECS))
}

/// `now` = the block's header timestamp (s); outputs keep their ABI.
fn oracle_reader(
    input: &[u8],
    state_db: &impl StateBackend,
    now: u64,
    meter: &mut ReadMeter,
) -> Result<Vec<u8>, CoreError> {
    let sel = abi::selector(input)?;

    if sel == selector_for("getPrice(bytes32)") {
        let market_id = abi::decode_market_id(&abi::word(input, 0)?);
        read_oracle_price(state_db, market_id, now)
    } else if sel == selector_for("getAllPrices()") {
        read_all_oracle_prices(state_db, now, meter)
    } else {
        Err(CoreError::UnknownSelector(sel))
    }
}

/// getPrice → (uint128 price, uint64 block_number, bool stale)
fn read_oracle_price(
    state_db: &impl StateBackend,
    market_id: MarketId,
    now: u64,
) -> Result<Vec<u8>, CoreError> {
    let key = oracle_agg_key(market_id);
    match state_db
        .get_cf_raw(CF_NATIVE_ORACLE, &key)?
        .and_then(|data| decode_agg(&data, now))
    {
        Some((price, block_number, stale)) => {
            let mut out = Vec::with_capacity(96);
            out.extend_from_slice(&abi::encode_fp_as_u128(price));
            out.extend_from_slice(&abi::encode_u64(block_number));
            out.extend_from_slice(&abi::encode_bool(stale));
            Ok(out)
        }
        None => Err(CoreError::NoOraclePrice(market_id)),
    }
}

/// getAllPrices → (bytes32[] market_ids, uint128[] prices, bool[] stale_flags)
fn read_all_oracle_prices(
    state_db: &impl StateBackend,
    now: u64,
    meter: &mut ReadMeter,
) -> Result<Vec<u8>, CoreError> {
    let prefix = b"agg";
    let entries = scan_prefix_metered(state_db, CF_NATIVE_ORACLE, prefix, None, meter)?;

    let mut market_ids = Vec::new();
    let mut prices = Vec::new();
    let mut stale_flags = Vec::new();

    for (key, value) in &entries {
        if key.len() != 11 {
            continue;
        }
        if let Some((price, _, stale)) = decode_agg(value, now) {
            let mid = u64::from_be_bytes(key[3..11].try_into().unwrap());

            market_ids.push(abi::encode_market_id(mid));
            prices.push(abi::encode_fp_as_u128(price));
            stale_flags.push(abi::encode_bool(stale));
        }
    }

    Ok(abi::encode_arrays_response(&[
        &market_ids,
        &prices,
        &stale_flags,
    ]))
}

/// Build the oracle aggregated price key: "agg" + market_id(8 BE).
fn oracle_agg_key(market_id: MarketId) -> Vec<u8> {
    let mut key = Vec::with_capacity(11);
    key.extend_from_slice(b"agg");
    key.extend_from_slice(&market_id.to_be_bytes());
    key
}

/// The usable oracle price at `now` (helper for other precompiles): `Ok` only
/// if the aggregate is not stale and > 0 — the rule of `OraclePrice::usable`.
fn get_oracle_price_fp(
    state_db: &impl StateBackend,
    market_id: MarketId,
    now: u64,
) -> Result<FixedPoint, CoreError> {
    let key = oracle_agg_key(market_id);
    match state_db
        .get_cf_raw(CF_NATIVE_ORACLE, &key)?
        .and_then(|data| decode_agg(&data, now))
    {
        Some((_, _, true)) => Err(CoreError::StaleOraclePrice(market_id)),
        Some((price, _, false)) if price > FixedPoint::ZERO => Ok(price),
        _ => Err(CoreError::NoOraclePrice(market_id)),
    }
}

// ============================================================================
// StakingReader (0x0803) — tasks 2.4.2
// ============================================================================

fn staking_reader(
    input: &[u8],
    state_db: &impl StateBackend,
    meter: &mut ReadMeter,
) -> Result<Vec<u8>, CoreError> {
    let sel = abi::selector(input)?;

    if sel == selector_for("getStakingInfo(address)") {
        let staker = abi::decode_address(&abi::word(input, 0)?);
        read_staking_info(state_db, &staker, meter)
    } else if sel == selector_for("getValidators()") {
        read_validators(state_db, meter)
    } else {
        Err(CoreError::UnknownSelector(sel))
    }
}

/// getStakingInfo → (uint128 delegated, uint128 permanent, uint128 rewards_pending, address validator)
fn read_staking_info(
    state_db: &impl StateBackend,
    staker: &Address,
    meter: &mut ReadMeter,
) -> Result<Vec<u8>, CoreError> {
    // Read delegation info: scan CF_STAKING_DELEGATIONS with prefix = staker(20)
    let mut total_delegated = U256::ZERO;
    let mut first_validator = Address::ZERO;

    let entries = scan_prefix_metered(
        state_db,
        CF_STAKING_DELEGATIONS,
        staker.as_slice(),
        None,
        meter,
    )?;
    for (key, value) in &entries {
        if key.len() != 40 {
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
fn read_validators(state_db: &impl StateBackend, meter: &mut ReadMeter) -> Result<Vec<u8>, CoreError> {
    let entries = scan_prefix_metered(state_db, CF_STAKING_VALIDATORS, &[], None, meter)?;
    let mut validators = Vec::new();
    let mut stakes = Vec::new();
    let mut commissions = Vec::new();

    for (key, value) in &entries {
        if key.len() != 20 {
            continue;
        }
        // FIX MED-NEW-14: Use Borsh deserialization instead of fragile byte-offset parsing.
        // Manual offsets silently break if ValidatorState fields are added/reordered.
        if let Ok(vs) = torus_economics::types::ValidatorState::try_from_slice(value) {
            let total = vs.self_stake + vs.total_delegated;
            validators.push(abi::encode_address(&vs.address));
            stakes.push(abi::encode_u128(u256_to_u128_saturating(total)));
            commissions.push(abi::encode_u128(vs.commission_bps as u128));
        }
    }

    Ok(abi::encode_arrays_response(&[
        &validators,
        &stakes,
        &commissions,
    ]))
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
    state_db: &impl StateBackend,
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

        // FIX ECON-FIND-27: Validate enum discriminant ranges before use
        // Side: 0=Buy, 1=Sell
        if side > 1 {
            return Err(CoreError::InvalidInput(format!("invalid side: {side}")));
        }
        // OrderType: 0=Limit, 1=Market. HL-parity: StopMarket (2) / StopLimit (3)
        // are rejected — this ABI carries no trigger price, and the drain used to
        // run them as plain Limit orders.
        if order_type > 1 {
            return Err(CoreError::InvalidInput(format!(
                "invalid order_type: {order_type} (0=Limit, 1=Market; stop orders are not supported)"
            )));
        }
        // TimeInForce: 0=GTC, 1=IOC, 2=FOK, 3=PostOnly
        if time_in_force > 3 {
            return Err(CoreError::InvalidInput(format!(
                "invalid time_in_force: {time_in_force}"
            )));
        }
        // FIX ECON-FIND-26: Validate u128 fits in i128 before cast (price & quantity)
        if price > i128::MAX as u128 {
            return Err(CoreError::Overflow("order price exceeds i128::MAX".into()));
        }
        if quantity > i128::MAX as u128 {
            return Err(CoreError::Overflow(
                "order quantity exceeds i128::MAX".into(),
            ));
        }

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

        CoreWriterQueue::enqueue(state_db, &action)?;
        // HL-parity: no order id. The executor assigns the real one from the
        // global counter when the queue drains next block, so it cannot be known
        // here (the old `(block + 1) << 64 | seq` never matched it).
        Ok([0u8; 32].to_vec())
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
    state_db: &impl StateBackend,
    current_block: u64,
) -> Result<Vec<u8>, CoreError> {
    let sel = abi::selector(input)?;

    if sel == selector_for("delegate(address,uint128)") {
        let validator = abi::decode_address(&abi::word(input, 0)?);
        let amount = abi::decode_u128(&abi::word(input, 1)?);
        // FIX ECON-FIND-26: Validate u128 fits in i128 before cast
        if amount > i128::MAX as u128 {
            return Err(CoreError::Overflow(
                "delegate amount exceeds i128::MAX".into(),
            ));
        }

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
        // FIX ECON-FIND-26: Validate u128 fits in i128 before cast
        if amount > i128::MAX as u128 {
            return Err(CoreError::Overflow(
                "undelegate amount exceeds i128::MAX".into(),
            ));
        }

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
    } else if sel == selector_for("claimUnbonded()") {
        // Release every matured unbonding entry of the caller (all validators).
        let action = QueuedAction {
            trader: *caller,
            kind: QueuedActionKind::ClaimUnbonded,
            block_queued: current_block,
        };
        CoreWriterQueue::enqueue(state_db, &action)?;
        // Actual amount determined at execution time
        Ok(abi::encode_u128(0).to_vec())
    } else if sel == selector_for("lockPermanent(uint128)") {
        let amount = abi::decode_u128(&abi::word(input, 0)?);
        // FIX ECON-FIND-26: Validate u128 fits in i128 before cast
        if amount > i128::MAX as u128 {
            return Err(CoreError::Overflow(
                "lockPermanent amount exceeds i128::MAX".into(),
            ));
        }

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

/// Lockbox precompile — Hyperliquid model (EVM-PF-05 fix).
///
/// It NEVER writes `CF_ACCOUNTS` or native balances: a raw mid-EVM account write is
/// invisible to revm's block-level `State` cache, which later overwrites it at bundle
/// commit (deposit minted native value, withdraw destroyed EVM value). Instead:
///
/// * `depositToNative(uint128 amountWei)` — PAYABLE. `amountWei` must equal
///   `msg.value`. revm has already moved the value out of the caller through its own
///   journal (so any enclosing revert undoes it); the EVM provider burns it from 0x0820
///   in the same frame. This enqueues a `LockboxDeposit` crediting
///   `floor(msg.value / 10^10)` native units NEXT block; the remainder (< 10^10 wei)
///   is burned (HyperCore semantics). Zero value: no-op. Nonzero value that is all dust
///   (< 1 native unit) reverts — it would destroy the whole amount for no credit.
/// * `withdrawFromNative(uint128 amountWei)` — non-payable. `amountWei` must be a
///   whole number of native units (multiple of 10^10). Enqueues a `LockboxWithdraw`
///   that debits `amountWei / 10^10` native and credits `amountWei` EVM wei NEXT block;
///   if the native balance is insufficient at drain time it fails there with no state
///   change. Zero: no-op.
///
/// Both return `true` = "queued", not "executed". The queue write goes through the
/// per-tx / per-frame native journal, so a reverted frame drops it.
fn lockbox_precompile(
    input: &[u8],
    caller: &Address,
    call_value: U256,
    state_db: &impl StateBackend,
    current_block: u64,
) -> Result<Vec<u8>, CoreError> {
    let sel = abi::selector(input)?;

    let kind = if sel == selector_for("depositToNative(uint128)") {
        let amount_wei = U256::from(abi::decode_u128(&abi::word(input, 0)?));
        if amount_wei != call_value {
            return Err(CoreError::InvalidInput(format!(
                "depositToNative: amount {amount_wei} must equal msg.value {call_value} (wei)"
            )));
        }
        if call_value.is_zero() {
            return Ok(abi::encode_bool(true).to_vec());
        }
        let (amount, _dust_burned) = wei_to_fp_floor(call_value)
            .ok_or_else(|| CoreError::Overflow("lockbox deposit exceeds i128::MAX".into()))?;
        if amount <= FixedPoint::ZERO {
            return Err(CoreError::InvalidInput(format!(
                "depositToNative: {call_value} wei is below one native unit \
                 ({WEI_PER_NATIVE_UNIT} wei) and would be burned entirely"
            )));
        }
        QueuedActionKind::LockboxDeposit { amount }
    } else if sel == selector_for("withdrawFromNative(uint128)") {
        if !call_value.is_zero() {
            return Err(CoreError::InvalidInput(
                "withdrawFromNative is not payable".into(),
            ));
        }
        let amount_wei = abi::decode_u128(&abi::word(input, 0)?);
        if amount_wei == 0 {
            return Ok(abi::encode_bool(true).to_vec());
        }
        if amount_wei % WEI_PER_NATIVE_UNIT != 0 {
            return Err(CoreError::InvalidInput(format!(
                "withdrawFromNative: {amount_wei} wei is not a multiple of one native unit \
                 ({WEI_PER_NATIVE_UNIT} wei)"
            )));
        }
        // u128::MAX / 10^10 < i128::MAX, so the cast cannot overflow.
        let amount = FixedPoint::from_raw((amount_wei / WEI_PER_NATIVE_UNIT) as i128);
        QueuedActionKind::LockboxWithdraw { amount }
    } else {
        return Err(CoreError::UnknownSelector(sel));
    };

    CoreWriterQueue::enqueue(
        state_db,
        &QueuedAction {
            trader: *caller,
            kind,
            block_queued: current_block,
        },
    )?;
    Ok(abi::encode_bool(true).to_vec())
}

// ============================================================================
// CoreWriterQueue — delayed execution queue (task 2.4.6)
// ============================================================================

pub struct CoreWriterQueue;

impl CoreWriterQueue {
    /// Enqueue a CoreWriter action for execution in the next block.
    /// Returns the sequence number assigned to this action.
    ///
    /// T4.4: takes any [`StateBackend`] so EVM execution can enqueue into a per-tx
    /// journaled overlay (durable only if the calling tx succeeds).
    pub fn enqueue(state_db: &impl StateBackend, action: &QueuedAction) -> Result<u64, CoreError> {
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
        state: &impl StateBackend,
        block_number: u64,
    ) -> Result<Vec<QueuedAction>, CoreError> {
        let prefix = block_number.to_be_bytes();
        let entries = state.iterate_cf(CF_CORE_WRITER_QUEUE, Some(&prefix))?;

        let mut actions = Vec::new();
        let mut keys_to_delete = Vec::new();

        for (key, value) in &entries {
            if let Ok(action) = QueuedAction::try_from_slice(value) {
                actions.push(action);
            }
            keys_to_delete.push(key.clone());
        }

        // Delete drained entries
        for key in &keys_to_delete {
            state.delete_cf_raw(CF_CORE_WRITER_QUEUE, key)?;
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
        let iter = torus_state::db::prefix_iter(db, &cf, &prefix);

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

    /// Find the next sequence number for a target block.
    ///
    /// T4.4: goes through [`StateBackend::iterate_cf`] (instead of the FIX EVM-PF-16 raw
    /// reverse seek) so pending journal writes from earlier calls in the SAME tx/block are
    /// visible (read-your-writes — two placeOrder calls in one tx must get seq 0 then 1).
    /// The scan is bounded to a single target-block prefix (only actions queued for the
    /// next block), so it stays small.
    fn next_sequence(state_db: &impl StateBackend, target_block: u64) -> Result<u64, CoreError> {
        let prefix = target_block.to_be_bytes();
        let entries = state_db.iterate_cf(CF_CORE_WRITER_QUEUE, Some(&prefix))?;

        // Entries are in sorted key order; the last well-formed key holds the max seq.
        if let Some((key, _)) = entries.last() {
            if key.len() == 16 {
                let seq = u64::from_be_bytes(key[8..16].try_into().unwrap());
                return Ok(seq + 1);
            }
        }
        Ok(0)
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
    /// Claim all matured unbonding entries (borsh tag 7 — appended).
    ClaimUnbonded,
    /// Lockbox 0x0820 `depositToNative`: credit `amount` native. The EVM value was
    /// already burned in the depositing tx — the drain must NOT debit EVM again.
    LockboxDeposit {
        amount: FixedPoint,
    },
    /// Lockbox 0x0820 `withdrawFromNative`: debit `amount` native, credit
    /// `amount × 10^10` wei EVM (via `Lockbox::withdraw_from_native`).
    LockboxWithdraw {
        amount: FixedPoint,
    },
}

/// Borsh tags of the lockbox kinds. Kept clear of the dense 0.. range used by the
/// CoreWriter kinds so the CoreWriter set can keep growing without a tag clash.
const TAG_LOCKBOX_DEPOSIT: u8 = 0x20;
const TAG_LOCKBOX_WITHDRAW: u8 = 0x21;

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
            Self::ClaimUnbonded => {
                w.write_all(&[7])?;
            }
            Self::LockboxDeposit { amount } => {
                w.write_all(&[TAG_LOCKBOX_DEPOSIT])?;
                borsh_write_fp(amount, w)?;
            }
            Self::LockboxWithdraw { amount } => {
                w.write_all(&[TAG_LOCKBOX_WITHDRAW])?;
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
            7 => Ok(Self::ClaimUnbonded),
            TAG_LOCKBOX_DEPOSIT => Ok(Self::LockboxDeposit {
                amount: borsh_read_fp(r)?,
            }),
            TAG_LOCKBOX_WITHDRAW => Ok(Self::LockboxWithdraw {
                amount: borsh_read_fp(r)?,
            }),
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

/// Write a legacy order-book snapshot to CF_NATIVE_ORDER_BOOKS under the
/// CLASSIC 8-byte market key.
///
/// # DANGER — test scaffolding only
///
/// This has NO production caller (only `torus-core/tests/precompile_tests.rs`
/// and the `torus-rpc` in-crate tests). `cf_native_order_books` is one of the
/// `NATIVE_ROOT_CFS`, and under `TORUS_BOOK_ROWS=1/2` an 8-byte classic key in
/// that CF is exactly the artifact the executor's loaders treat as a FATAL
/// wrong-layout signal — a single call against a mode-1/2 DB poisons the CF
/// and the node fail-stops at every subsequent boot, permanently.
///
/// It is therefore GUARDED: the write is refused unless the DB's observed
/// layout is Classic. (A hard `#[cfg(test)]` gate was rejected because both
/// existing callers are integration tests in OTHER crates, which do not see
/// `cfg(test)` of this crate; the runtime guard protects production DBs
/// without a feature-flag dance.)
pub fn write_order_book_snapshot(
    state_db: &StateDb,
    market_id: MarketId,
    snapshot: &OrderBookSnapshot,
) -> Result<(), CoreError> {
    match crate::book_reader::detect_layout(state_db)? {
        crate::book_reader::BookLayout::Classic => {}
        layout => {
            return Err(CoreError::BookLayout(format!(
                "write_order_book_snapshot is classic-layout-only test scaffolding, but \
                 this DB is written in {} — refusing to poison cf_native_order_books \
                 (an 8-byte key there is a permanent boot fail-stop under row layouts)",
                layout.describe()
            )))
        }
    }
    let key = market_id.to_be_bytes();
    let data = borsh::to_vec(snapshot).map_err(|e| CoreError::Borsh(e.to_string()))?;
    state_db.put_cf_raw(CF_NATIVE_ORDER_BOOKS, &key, &data)?;
    Ok(())
}

/// R02 branch 3: `get_oracle_price_fp` (0x0800 getPosition's UPnL price)
/// keeps absence and failure apart up to the EVM boundary: a failed read is
/// a local-fault `Err`; no row, a stale, non-positive or short row are not.
#[cfg(test)]
mod r02_oracle_price_tests {
    use super::*;
    use torus_state::{AtomicWriteOp, StateError};

    #[derive(Clone)]
    struct FailingRead;

    impl StateBackend for FailingRead {
        fn get_cf_raw(&self, _cf: &str, _key: &[u8]) -> Result<Option<Vec<u8>>, StateError> {
            Err(StateError::Io(std::io::Error::other("injected read failure")))
        }
        fn put_cf_raw(&self, _cf: &str, _key: &[u8], _value: &[u8]) -> Result<(), StateError> {
            unreachable!()
        }
        fn delete_cf_raw(&self, _cf: &str, _key: &[u8]) -> Result<(), StateError> {
            unreachable!()
        }
        fn iterate_cf(&self, _cf: &str, _prefix: Option<&[u8]>) -> Result<Vec<(Vec<u8>, Vec<u8>)>, StateError> {
            unreachable!()
        }
        fn atomic_write(&self, _ops: &[AtomicWriteOp<'_>]) -> Result<(), StateError> {
            unreachable!()
        }
    }

    fn row(price_raw: i128, ts: u64) -> Vec<u8> {
        [price_raw.to_be_bytes().as_slice(), &1u64.to_be_bytes(), &3u32.to_be_bytes(), &ts.to_be_bytes()].concat()
    }

    #[test]
    fn r02_oracle_price_fp_failed_read_is_a_local_fault() {
        let e = get_oracle_price_fp(&FailingRead, 1, 1_000).expect_err("read failed");
        assert!(e.is_local_fault(), "{e}");
    }

    #[test]
    fn r02_oracle_price_fp_absence_is_not_a_local_fault() {
        let dir = tempfile::tempdir().unwrap();
        let db = StateDb::open(dir.path()).unwrap();
        let e = get_oracle_price_fp(&db, 1, 1_000).expect_err("no row");
        assert!(!e.is_local_fault(), "{e}");
        let fresh = FixedPoint::from_raw(5 * FixedPoint::SCALE);
        for (m, data, ok) in [
            (2, row(fresh.raw(), 1_000), true),
            (3, row(fresh.raw(), 1_000 - DEFAULT_MAX_ORACLE_AGE_SECS - 1), false),
            (4, row(0, 1_000), false),
            (5, vec![1, 2, 3], false),
        ] {
            db.put_cf_raw(CF_NATIVE_ORACLE, &oracle_agg_key(m), &data).unwrap();
            match get_oracle_price_fp(&db, m, 1_000) {
                Ok(p) => assert!(ok && p == fresh, "market {m}"),
                Err(e) => assert!(!ok && !e.is_local_fault(), "market {m}: {e}"),
            }
        }
    }
}

#[cfg(test)]
mod scan_prefix_metered_tests {
    use super::*;

    /// Final review S2: a metered scan of a CF the running state hash does not
    /// cover (node-local, e.g. `cf_book_order_rows`) would make the charge — a
    /// block result — node-dependent. It is refused, metered or not, and
    /// charges nothing.
    #[test]
    fn scanning_an_unhashed_cf_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let db = StateDb::open(dir.path()).unwrap();
        db.put_cf_raw(CF_BOOK_ORDER_ROWS, b"row", b"v").unwrap();
        assert!(torus_state::running_hash::hashed_cf_id(CF_BOOK_ORDER_ROWS).is_none());
        for mut meter in [ReadMeter::with_max(1_000), ReadMeter::unlimited()] {
            let r = scan_prefix_metered(&db, CF_BOOK_ORDER_ROWS, b"", None, &mut meter);
            assert!(r.is_err(), "unhashed CF scanned: {r:?}");
            assert_eq!(meter.used(), 0);
        }
        // A hashed CF still scans.
        let mut meter = ReadMeter::with_max(1_000);
        assert!(scan_prefix_metered(&db, CF_NATIVE_MARKETS, b"", None, &mut meter).is_ok());
    }
}
