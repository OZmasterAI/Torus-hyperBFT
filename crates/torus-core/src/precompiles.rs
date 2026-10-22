//! Cross-VM precompiles — ABI-encoded bridges between EVM and native state (task 2.4).
//!
//! Precompile address map:
//!   0x0800 — OrderBookReader (read)
//!   0x0801 — BalanceReader (read)
//!   0x0802 — OracleReader (read)
//!   0x0803 — StakingReader (read)
//!   0x080e — BboReader (read: best bid / ask, item 7 step 2, HL's bbo address)
//!   0x0810 — CoreWriter (write: orders, delayed)
//!   0x0811 — CoreWriterStaking (write: staking, delayed)
//!   0x0820 — Lockbox (write: EVM <-> native transfers, native leg delayed)

use std::io::{self, Read as IoRead, Write as IoWrite};

use alloy_primitives::keccak256;
use borsh::{BorshDeserialize, BorshSerialize};
use torus_state::cf::*;
use torus_state::{StateBackend, StateDb};
use torus_types::{Address, Cloid, FixedPoint, MarketId, OrderId, U256};

use crate::error::CoreError;
use crate::lockbox::{wei_to_fp_floor, WEI_PER_NATIVE_UNIT};
use crate::position::{
    borsh_read_address, borsh_read_fp, borsh_write_address, borsh_write_fp, NativeBalance,
};

// ============================================================================
// Precompile Address Constants
// ============================================================================

pub const ADDR_ORDER_BOOK_READER: u16 = 0x0800;
pub const ADDR_BALANCE_READER: u16 = 0x0801;
pub const ADDR_ORACLE_READER: u16 = 0x0802;
pub const ADDR_STAKING_READER: u16 = 0x0803;
/// Item 7 step 2 (owner decision 3): best bid / ask per market, at HL's bbo address.
pub const ADDR_BBO_READER: u16 = 0x080e;
pub const ADDR_CORE_WRITER: u16 = 0x0810;
pub const ADDR_CORE_WRITER_STAKING: u16 = 0x0811;
pub const ADDR_LOCKBOX: u16 = 0x0820;

/// FIX EVM-PF-10: Gas costs for Torus precompiles.
///
/// Base gas of every reader call (0x0800-0x0803, 0x080e). s99 owner decision (item6
/// plan 9.16, revisit later): single reads at Hyperliquid level, ~16,500 gas
/// per position read. getPosition = this base + its 5 answer words = 16,500;
/// a 30M-gas block of cold getPosition calls (ozarchy, block cache cold) runs
/// in ~32-37 ms (`docs/perf/read-precompile-gas.md`, section s99). The base
/// pays for a reader's point reads (getPosition, getBalances, getPrice, the
/// staking point rows) and a scan's fixed cost.
pub const GAS_PRECOMPILE_READ: u64 = 16_400;
/// Gas per hashed row a reader's prefix scan returned (s99: scans 500 gas per
/// scanned row + 20 per returned word).
pub const GAS_PRECOMPILE_READ_PER_ROW: u64 = 500;
/// Gas per 32-byte word a reader returned.
pub const GAS_PRECOMPILE_READ_PER_WORD: u64 = 20;
/// State-mutating writes (SSTORE equivalent range).
pub const GAS_PRECOMPILE_WRITE: u64 = 20_000;
/// Complex operations (governance, liquidation).
pub const GAS_PRECOMPILE_COMPLEX: u64 = 50_000;

/// Gas of a reader call (0x0800-0x0803, 0x080e) whose meter used `used` gas above
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
/// ([`GAS_PRECOMPILE_READ_PER_ROW`]) and 20 per word returned
/// ([`GAS_PRECOMPILE_READ_PER_WORD`]), capped at what the caller's gas pays
/// for. A reader charges BEFORE it does the work it charges for where it can
/// (scans stop at the budget), so the node's work per call is bounded by the
/// call's gas, not only its price. RocksDB deletion markers a scan steps over
/// are never charged: their count is node-local (flush / compaction history),
/// and gas is a block result.
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

    /// Words returned.
    fn charge_words(&mut self, words: u64) -> Result<(), CoreError> {
        self.charge(words.saturating_mul(GAS_PRECOMPILE_READ_PER_WORD))
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
/// prefix that the iterator steps over uncharged (node-local). Unlimited and
/// uncapped: one `iterate_cf` (the pre-existing read).
///
/// F1 (Codex review P1): every storage error, a missing CF included, is
/// `CoreError::State` — a local fault the EVM provider fail-stops on. A
/// missing CF is never an empty answer: that would be a success receipt
/// with a gas charge only this node computes. An existing but empty CF is
/// the empty result.
fn scan_prefix_metered(
    state: &impl StateBackend,
    cf: &'static str,
    prefix: &[u8],
    max_rows: Option<u64>,
    meter: &mut ReadMeter,
) -> Result<Vec<(Vec<u8>, Vec<u8>)>, CoreError> {
    const PAGE: u64 = 64;
    let Some(hashed_id) = torus_state::running_hash::hashed_cf_id(cf) else {
        return Err(CoreError::InvalidPrecompileInput(format!(
            "reader scan of {cf}: not a hashed consensus CF (its charge would be node-dependent)"
        )));
    };
    let consensus = |k: &[u8]| torus_state::running_hash::key_is_hashed(hashed_id, k);
    if meter.is_unlimited() && max_rows.is_none() {
        let p = (!prefix.is_empty()).then_some(prefix);
        let mut rows = state.iterate_cf(cf, p)?;
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
        let rows = state.iterate_cf_prefix_from(cf, prefix, &start, page)?;
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
        0x0800..=0x0803 | ADDR_BBO_READER => GAS_PRECOMPILE_READ,
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
pub const ALL_PRECOMPILE_ADDRESSES: [Address; 8] = [
    precompile_address(ADDR_ORDER_BOOK_READER),
    precompile_address(ADDR_BALANCE_READER),
    precompile_address(ADDR_ORACLE_READER),
    precompile_address(ADDR_STAKING_READER),
    precompile_address(ADDR_BBO_READER),
    precompile_address(ADDR_CORE_WRITER),
    precompile_address(ADDR_CORE_WRITER_STAKING),
    precompile_address(ADDR_LOCKBOX),
];

/// Check whether an address is a Torus precompile.
pub fn is_precompile(address: &Address) -> bool {
    let bytes = address.as_slice();
    bytes[..18].iter().all(|&b| b == 0) && {
        let id = u16::from_be_bytes([bytes[18], bytes[19]]);
        matches!(id, 0x0800..=0x0803 | ADDR_BBO_READER | 0x0810..=0x0811 | 0x0820)
    }
}

/// Whether a precompile id is a read-only reader (0x0800–0x0803, 0x080e), safe to run in an
/// eth_call / eth_estimateGas simulation. The writer precompiles (CoreWriter 0x0810,
/// CoreWriterStaking 0x0811, Lockbox 0x0820) mutate shared state and must be denied there.
pub const fn is_reader_precompile(id: u16) -> bool {
    matches!(
        id,
        ADDR_ORDER_BOOK_READER..=ADDR_STAKING_READER | ADDR_BBO_READER
    )
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
        0,
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
        0,
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
        0,
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
        0,
        read_only,
        meter,
    )
}

/// [`execute_precompile_metered`] for a block whose header carries the P0.0
/// feature bits `features`: the EVM provider's entry. Item 7 T: with
/// [`torus_types::Feature::SameBlockCoreWriter`] in `features`, the writers
/// queue non-order kinds for `current_block` itself
/// ([`CoreWriterQueue::enqueue_with`]). Every other entry runs with no bits
/// (the rules before T).
#[allow(clippy::too_many_arguments)]
pub fn execute_precompile_metered_at(
    address: &Address,
    input: &[u8],
    caller: &Address,
    call_value: U256,
    state_db: &impl StateBackend,
    current_block: u64,
    current_timestamp: u64,
    features: u128,
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
        features,
        read_only,
        meter,
    )
}

/// Item 7 T: whether a block with the P0.0 header bits `features` queues
/// non-order CoreWriter kinds for itself
/// ([`torus_types::Feature::SameBlockCoreWriter`]).
pub const fn same_block_core_writer(features: u128) -> bool {
    features & torus_types::Feature::SameBlockCoreWriter.bit() != 0
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
    features: u128,
    read_only: bool,
    meter: &mut ReadMeter,
) -> Result<Vec<u8>, CoreError> {
    let same_block = same_block_core_writer(features);
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
        ADDR_ORDER_BOOK_READER => {
            read(order_book_reader(input, state_db, current_timestamp), meter)
        }
        ADDR_BALANCE_READER => read(balance_reader(input, state_db, meter), meter),
        ADDR_ORACLE_READER => read(oracle_reader(input, state_db, current_timestamp, meter), meter),
        ADDR_STAKING_READER => read(staking_reader(input, state_db, meter), meter),
        ADDR_BBO_READER => read(bbo_reader(input, state_db), meter),
        ADDR_CORE_WRITER => core_writer(input, caller, state_db, current_block, same_block),
        ADDR_CORE_WRITER_STAKING => {
            core_writer_staking(input, caller, state_db, current_block, same_block)
        }
        ADDR_LOCKBOX => {
            lockbox_precompile(input, caller, call_value, state_db, current_block, same_block)
        }
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
) -> Result<Vec<u8>, CoreError> {
    let sel = abi::selector(input)?;
    // Item 7 step 1 (owner s122): getOrderBook is removed (like getOpenOrders
    // in s99); its selector reverts as `UnknownSelector`.
    if sel == selector_for("getPosition(address,bytes32)") {
        let trader = abi::decode_address(&abi::word(input, 0)?);
        let market_id = abi::decode_market_id(&abi::word(input, 1)?);
        read_position(state_db, &trader, market_id, now)
    } else {
        Err(CoreError::UnknownSelector(sel))
    }
}

/// getPosition → (int128 size, uint128 entry_price, int128 unrealized_pnl, int128 realized_pnl, uint128 margin)
///
/// Item 2: UPnL uses the oracle price only while usable at `now` (block
/// timestamp, s) — the rule of `OraclePrice::usable`; otherwise 0. R02
/// branch 5: a failed oracle read is `CoreError::PrecompileLocalFault`
/// (the node fail-stops), never 0.
fn read_position(
    state_db: &impl StateBackend,
    trader: &Address,
    market_id: MarketId,
    now: u64,
) -> Result<Vec<u8>, CoreError> {
    let key = crate::position::position_key(trader, market_id);
    let pos = match state_db.get_cf_raw(CF_NATIVE_POSITIONS, &key)? {
        // Pre-testnet: a row naming another key is refused, as every load.
        Some(data) => crate::position::decode_position_row(&key, &data)?,
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
        // No usable price (absent, stale, non-positive, short row: the same
        // bytes on every validator): UPnL 0.
        Err(e) if !e.is_local_fault() => FixedPoint::ZERO,
        // R02 branch 5 (owner option A): the aggregate read failed on THIS
        // node. Not UPnL 0 (an answer only this node gives) and not a revert
        // (equally consensus-visible): `PrecompileLocalFault`, which the EVM
        // provider turns into an aborted execution and the node fail-stops.
        Err(e) => return Err(CoreError::PrecompileLocalFault(format!("getPosition oracle read: {e}"))),
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
// BboReader (0x080e) — item 7 step 2 (HL's bbo address)
// ============================================================================

/// `bbo(bytes32 marketId) -> (uint128 bid, uint128 ask)`: the market's best
/// bid and ask (FixedPoint raw) as of the last block that changed its book
/// (`cf_native_bbo`), 0 for an empty side or an unknown market. Owner
/// decision 3, our metering, priced like the other point readers (the point
/// read is in the base, owner s125): 16,400 + 2 x 20 = 16,440 gas.
///
/// The row is node-written and unhashed: a row that is not 32 bytes, or a
/// failed read, is this node's fault (R02 branch 5): `PrecompileLocalFault`
/// (the node fail-stops), never zeros and never a revert.
fn bbo_reader(input: &[u8], state_db: &impl StateBackend) -> Result<Vec<u8>, CoreError> {
    let sel = abi::selector(input)?;
    if sel != selector_for("bbo(bytes32)") {
        return Err(CoreError::UnknownSelector(sel));
    }
    let market_id = abi::decode_market_id(&abi::word(input, 0)?);
    let row = state_db
        .get_cf_raw(CF_NATIVE_BBO, &market_id.to_be_bytes())
        .map_err(|e| {
            CoreError::PrecompileLocalFault(format!("bbo read (market {market_id}): {e}"))
        })?;
    let (bid, ask) = match row {
        None => (0i128, 0i128),
        Some(r) => match <[u8; 32]>::try_from(r.as_slice()) {
            Ok(r) => (
                i128::from_be_bytes(r[..16].try_into().expect("16 bytes")),
                i128::from_be_bytes(r[16..].try_into().expect("16 bytes")),
            ),
            Err(_) => {
                return Err(CoreError::PrecompileLocalFault(format!(
                    "bbo row of market {market_id}: {} bytes, not 32",
                    r.len()
                )))
            }
        },
    };
    let mut out = Vec::with_capacity(64);
    out.extend_from_slice(&abi::encode_u128(bid as u128));
    out.extend_from_slice(&abi::encode_u128(ask as u128));
    Ok(out)
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
    same_block: bool,
) -> Result<Vec<u8>, CoreError> {
    let sel = abi::selector(input)?;

    // s114: the 7-word form appends a client order id (cloid), 0 = none.
    let with_cloid =
        sel == selector_for("placeOrder(bytes32,uint8,uint8,uint128,uint128,uint8,uint128)");
    if with_cloid || sel == selector_for("placeOrder(bytes32,uint8,uint8,uint128,uint128,uint8)") {
        let market_id = abi::decode_market_id(&abi::word(input, 0)?);
        let side = abi::decode_u8(&abi::word(input, 1)?);
        let order_type = abi::decode_u8(&abi::word(input, 2)?);
        let price = abi::decode_u128(&abi::word(input, 3)?);
        let quantity = abi::decode_u128(&abi::word(input, 4)?);
        let time_in_force = abi::decode_u8(&abi::word(input, 5)?);
        let cloid = if with_cloid {
            Some(abi::decode_u128(&abi::word(input, 6)?)).filter(|&c| c != 0)
        } else {
            None
        };

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
                cloid,
            },
            block_queued: current_block,
        };

        CoreWriterQueue::enqueue_with(state_db, &action, same_block)?;
        // HL-parity: no order id. The executor assigns the real one from the
        // global counter when the queue drains (K blocks later), so it cannot be known
        // here (the old `(block + 1) << 64 | seq` never matched it).
        Ok([0u8; 32].to_vec())
    } else if sel == selector_for("cancelOrder(bytes32)") {
        let order_id = abi::decode_order_id(&abi::word(input, 0)?);

        let action = QueuedAction {
            trader: *caller,
            kind: QueuedActionKind::CancelOrder { order_id },
            block_queued: current_block,
        };
        CoreWriterQueue::enqueue_with(state_db, &action, same_block)?;
        Ok(abi::encode_bool(true).to_vec())
    } else if sel == selector_for("cancelByCloid(bytes32,uint128)") {
        // s114: cancel the caller's open order (resting or a pending stop)
        // with this cloid in this market.
        let market_id = abi::decode_market_id(&abi::word(input, 0)?);
        let cloid = abi::decode_u128(&abi::word(input, 1)?);
        let action = QueuedAction {
            trader: *caller,
            kind: QueuedActionKind::CancelByCloid { market_id, cloid },
            block_queued: current_block,
        };
        CoreWriterQueue::enqueue_with(state_db, &action, same_block)?;
        Ok(abi::encode_bool(true).to_vec())
    } else if sel == selector_for("cancelAll(bytes32)") {
        let market_id = abi::decode_market_id(&abi::word(input, 0)?);

        let action = QueuedAction {
            trader: *caller,
            kind: QueuedActionKind::CancelAll { market_id },
            block_queued: current_block,
        };
        CoreWriterQueue::enqueue_with(state_db, &action, same_block)?;
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
    same_block: bool,
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
        CoreWriterQueue::enqueue_with(state_db, &action, same_block)?;
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
        CoreWriterQueue::enqueue_with(state_db, &action, same_block)?;
        Ok(abi::encode_bool(true).to_vec())
    } else if sel == selector_for("claimRewards()") {
        let action = QueuedAction {
            trader: *caller,
            kind: QueuedActionKind::ClaimRewards,
            block_queued: current_block,
        };
        CoreWriterQueue::enqueue_with(state_db, &action, same_block)?;
        // Actual amount determined at execution time
        Ok(abi::encode_u128(0).to_vec())
    } else if sel == selector_for("claimUnbonded()") {
        // Release every matured unbonding entry of the caller (all validators).
        let action = QueuedAction {
            trader: *caller,
            kind: QueuedActionKind::ClaimUnbonded,
            block_queued: current_block,
        };
        CoreWriterQueue::enqueue_with(state_db, &action, same_block)?;
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
        CoreWriterQueue::enqueue_with(state_db, &action, same_block)?;
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
///
/// Item 7 T (`Feature::SameBlockCoreWriter` in the block's header): "NEXT block" above
/// becomes "this block", after its native batches ([`CoreWriterQueue::enqueue_with`]); the
/// value is then burned and credited in the same block.
fn lockbox_precompile(
    input: &[u8],
    caller: &Address,
    call_value: U256,
    state_db: &impl StateBackend,
    current_block: u64,
    same_block: bool,
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

    CoreWriterQueue::enqueue_with(
        state_db,
        &QueuedAction {
            trader: *caller,
            kind,
            block_queued: current_block,
        },
        same_block,
    )?;
    Ok(abi::encode_bool(true).to_vec())
}

// ============================================================================
// CoreWriterQueue — delayed execution queue (task 2.4.6)
// ============================================================================

pub struct CoreWriterQueue;

/// Item 7 step 2 (owner answer 3): `CF_FEE_CONFIG` row holding the last order-kind target
/// block (u64 BE). Not a governance key (not in the allowlist, no proposal sets it), and not
/// in `CF_CORE_WRITER_QUEUE`, whose drain takes every row under the target prefix. Hashed
/// state, written only by EVM blocks (like the queue rows), the same on every node.
pub const CORE_WRITER_LAST_ORDER_TARGET_KEY: &[u8] = b"core_writer_last_order_target";

/// Codex P2 on C7: `CF_FEE_CONFIG` row (u64 BE) holding the first-open-target cursor `C`:
/// for an enqueue in block `B`, every target in `B + 1 .. C` is full (a target loses rows
/// only when its own block drains them, after which no enqueue targets it). An enqueue
/// skips those targets without reading them; one whose walk covered every target from
/// `B + 1` moves `C` past the full targets it found or filled. Hashed state like
/// [`CORE_WRITER_LAST_ORDER_TARGET_KEY`], written only when it moves.
pub const CORE_WRITER_FIRST_OPEN_TARGET_KEY: &[u8] = b"core_writer_first_open_target";

/// Codex P2 on C7: `CF_FEE_CONFIG` row `start (8 BE) ‖ end (8 BE)`: every target in
/// `start .. end` is full. Kept by the order kinds that land beyond `C` (K blocks ahead,
/// where `C` cannot speak for the targets in between), so a later walk jumps the order
/// kinds' run of full targets instead of reading each one. Hashed state, written only
/// when it grows or restarts.
pub const CORE_WRITER_FULL_ORDER_RUN_KEY: &[u8] = b"core_writer_full_order_run";

/// Item 7 T (`Feature::SameBlockCoreWriter`): the order range's first-open-target cursor
/// (u64 BE), [`CORE_WRITER_FIRST_OPEN_TARGET_KEY`]'s meaning for the order range
/// (seqs [`torus_types::CORE_WRITER_ORDER_SEQ_BASE`]`..`, the only range the order walk
/// fills under T). A key of its own: the step 2 cursor speaks for the shared range of the
/// blocks before the activation, and reading it here would make the order walk skip
/// targets whose order range is still open. Hashed state, written only when it moves.
pub const CORE_WRITER_T_FIRST_OPEN_ORDER_TARGET_KEY: &[u8] =
    b"core_writer_t_first_open_order_target";

/// Item 7 T: [`CORE_WRITER_FULL_ORDER_RUN_KEY`]'s meaning for the order range, under its
/// own key (see [`CORE_WRITER_T_FIRST_OPEN_ORDER_TARGET_KEY`]).
pub const CORE_WRITER_T_FULL_ORDER_RUN_KEY: &[u8] = b"core_writer_t_full_order_run";

/// Item 7 T, Codex torus-adversarial (s138): the same-block range's first-open-target
/// cursor `C` (u64 BE): for a non-order enqueue in block `B`, every target in `B .. C` has
/// its same-block range (seqs `0 .. CAP`) full. Sound for every later block: a target's
/// range loses rows only when its own block drains them, after which no enqueue targets
/// it. Every non-order walk starts at the queuing block, so each one may move `C` past the
/// full targets it found or filled: with intake above the drain cap the backlog of full
/// targets grows, but a walk reads O(1) rows instead of one per full target. Hashed state
/// in `CF_FEE_CONFIG`, written only by EVM blocks of the feature, only when it moves.
pub const CORE_WRITER_T_FIRST_OPEN_SAME_BLOCK_TARGET_KEY: &[u8] =
    b"core_writer_t_first_open_same_block_target";

/// The seq range one enqueue walk fills and the cursor rows that speak for it.
struct WalkRange {
    /// The range's first seq; it holds [`torus_types::CORE_WRITER_DRAIN_CAP`] seqs.
    base: u64,
    cursor_key: &'static [u8],
    run_key: &'static [u8],
}

/// Step 2 (and T off): one range per target, from seq 0, every kind.
const SHARED_RANGE: WalkRange = WalkRange {
    base: 0,
    cursor_key: CORE_WRITER_FIRST_OPEN_TARGET_KEY,
    run_key: CORE_WRITER_FULL_ORDER_RUN_KEY,
};

/// Item 7 T: the order kinds' range.
const T_ORDER_RANGE: WalkRange = WalkRange {
    base: torus_types::CORE_WRITER_ORDER_SEQ_BASE,
    cursor_key: CORE_WRITER_T_FIRST_OPEN_ORDER_TARGET_KEY,
    run_key: CORE_WRITER_T_FULL_ORDER_RUN_KEY,
};

/// A `CF_FEE_CONFIG` u64 BE row; absent or not 8 bytes = `None`.
fn read_u64_row(state_db: &impl StateBackend, key: &[u8]) -> Result<Option<u64>, CoreError> {
    Ok(state_db
        .get_cf_raw(CF_FEE_CONFIG, key)?
        .and_then(|v| <[u8; 8]>::try_from(v.as_slice()).ok())
        .map(u64::from_be_bytes))
}

/// [`CORE_WRITER_FULL_ORDER_RUN_KEY`]: `Some((start, end))`; absent or not 16 bytes = `None`.
fn read_order_run(
    state_db: &impl StateBackend,
    run_key: &[u8],
) -> Result<Option<(u64, u64)>, CoreError> {
    Ok(state_db
        .get_cf_raw(CF_FEE_CONFIG, run_key)?
        .and_then(|v| <[u8; 16]>::try_from(v.as_slice()).ok())
        .map(|v| {
            let (start, end) = v.split_at(8);
            (
                u64::from_be_bytes(start.try_into().expect("8 bytes")),
                u64::from_be_bytes(end.try_into().expect("8 bytes")),
            )
        }))
}

/// Queue key: target block (8 BE) ‖ seq (8 BE).
fn queue_key(target_block: u64, seq: u64) -> [u8; 16] {
    let mut key = [0u8; 16];
    key[..8].copy_from_slice(&target_block.to_be_bytes());
    key[8..].copy_from_slice(&seq.to_be_bytes());
    key
}

impl CoreWriterQueue {
    /// Enqueue a CoreWriter action. Returns the row's sequence in its target block.
    ///
    /// Item 7 step 2 (owner decision 4): an order action ([`QueuedActionKind::is_order`])
    /// targets `block_queued + K` (governance `core_writer_order_delay`, absent or invalid =
    /// [`torus_types::DEFAULT_CORE_WRITER_ORDER_DELAY`]), never a block before the last order
    /// action's target ([`CORE_WRITER_LAST_ORDER_TARGET_KEY`], owner answer 3: lowering K
    /// cannot let a cancel overtake its order); any other kind targets `block_queued + 1`. A
    /// target that already holds [`torus_types::CORE_WRITER_DRAIN_CAP`] rows sends the row on
    /// to the next block with room, so no block drains more than the cap and rows keep queue
    /// order. Rows are queued only by EVM blocks; item 7 option 0 (tasks 1.3, 1.4): the
    /// target's due-check (`NativeExecutor::core_writer_due`) reads the block overlay, so a
    /// row counts whether or not it is durable yet, and every read here (the delay, the
    /// cursor rows, the full-target probes, `next_sequence`) goes through `state_db`, the
    /// block's journal over the block overlay's layers (parent, L, C, then the DB).
    ///
    /// Codex P2: the walk skips the targets [`CORE_WRITER_FIRST_OPEN_TARGET_KEY`] and
    /// [`CORE_WRITER_FULL_ORDER_RUN_KEY`] know are full, so a backlog of full targets costs
    /// no reads (the target placed is the same as probing every one). A full target neither
    /// row covers was filled by an order kind before its run restarted, which happens only
    /// when `block + K` passed the run's end: such targets lie below `block + K` (K <= 100),
    /// and each is read once by the walk that then moves the cursor past it.
    ///
    /// T4.4: takes any [`StateBackend`] so EVM execution can enqueue into a per-tx
    /// journaled overlay (durable only if the calling tx succeeds).
    ///
    /// Item 7 T: these are the rules of a block without `Feature::SameBlockCoreWriter`;
    /// the EVM provider calls [`Self::enqueue_with`] with the block's bit.
    pub fn enqueue(state_db: &impl StateBackend, action: &QueuedAction) -> Result<u64, CoreError> {
        Self::enqueue_walk(state_db, action, &SHARED_RANGE)
    }

    /// [`Self::enqueue`]'s walk over the seq range `range` of each target: the target is
    /// full iff the range's last seq is taken, the cursor rows are the range's own. Step 2
    /// (and T off): [`SHARED_RANGE`], every kind. Item 7 T: [`T_ORDER_RANGE`], order kinds
    /// only (so `candidate >= next_block` holds with K >= 1, as the cursor needs).
    fn enqueue_walk(
        state_db: &impl StateBackend,
        action: &QueuedAction,
        range: &WalkRange,
    ) -> Result<u64, CoreError> {
        const CAP: u64 = torus_types::CORE_WRITER_DRAIN_CAP;
        let last_seq = range.base + CAP - 1;
        let order = action.kind.is_order();
        let next_block = action.block_queued.saturating_add(1);
        let candidate = if order {
            let stored = state_db.get_cf_raw(
                CF_FEE_CONFIG,
                torus_types::CORE_WRITER_ORDER_DELAY_PARAM.as_bytes(),
            )?;
            let delay = torus_types::gov_u64_in(
                stored.as_deref(),
                &torus_types::CORE_WRITER_ORDER_DELAY_RANGE,
            )
            .unwrap_or(torus_types::DEFAULT_CORE_WRITER_ORDER_DELAY);
            let last = read_u64_row(state_db, CORE_WRITER_LAST_ORDER_TARGET_KEY)?.unwrap_or(0);
            action.block_queued.saturating_add(delay).max(last)
        } else {
            next_block // anti-frontrunning: execute next block
        };
        // Codex P2: the walk to the first target with room skips the targets the two
        // cursor rows know are full instead of reading each one. K >= 1, so
        // `candidate >= next_block` and every target in `next_block .. cursor` is full.
        let cursor = read_u64_row(state_db, range.cursor_key)?.unwrap_or(0);
        let mut run: Option<Option<(u64, u64)>> = None; // read at the first full probe
        let mut target_block = candidate.max(cursor);
        loop {
            if let Some(Some((start, end))) = run {
                if (start..end).contains(&target_block) {
                    target_block = end.max(cursor);
                    continue;
                }
            }
            // A target's seqs are dense from 0 (rows leave only when the whole target
            // drains), so it is full iff its last slot is taken.
            if state_db
                .get_cf_len(CF_CORE_WRITER_QUEUE, &queue_key(target_block, last_seq))?
                .is_none()
            {
                break;
            }
            if run.is_none() {
                run = Some(read_order_run(state_db, range.run_key)?);
            }
            target_block = target_block
                .checked_add(1)
                .ok_or_else(|| CoreError::Overflow("CoreWriter target block".into()))?;
        }

        let seq = if range.base == 0 {
            Self::next_sequence(state_db, target_block)?
        } else {
            Self::next_sequence_in(state_db, target_block, range.base)?
        };
        let data = borsh::to_vec(action).map_err(|e| CoreError::Borsh(e.to_string()))?;
        state_db.put_cf_raw(CF_CORE_WRITER_QUEUE, &queue_key(target_block, seq), &data)?;

        // Every target in `candidate .. full_end` is full now (the walk's, and this row's
        // target if the row took its last slot).
        let full_end = if seq == last_seq {
            target_block.saturating_add(1)
        } else {
            target_block
        };
        let known_from = cursor.max(next_block);
        if candidate <= known_from {
            // `next_block .. candidate` was known full too: the cursor may move.
            if full_end > known_from {
                state_db.put_cf_raw(CF_FEE_CONFIG, range.cursor_key, &full_end.to_be_bytes())?;
            }
        } else if order && full_end > candidate {
            // An order kind beyond the cursor: the targets between are unknown, so keep
            // what it learnt in the run row. Order targets never move earlier, so the
            // run either continues (`candidate` inside it) or restarts at `candidate`.
            let old = match run {
                Some(r) => r,
                None => read_order_run(state_db, range.run_key)?,
            };
            let new = match old {
                Some((start, end)) if (start..=end).contains(&candidate) => {
                    (start, end.max(full_end))
                }
                _ => (candidate, full_end),
            };
            if old != Some(new) {
                let mut row = [0u8; 16];
                row[..8].copy_from_slice(&new.0.to_be_bytes());
                row[8..].copy_from_slice(&new.1.to_be_bytes());
                state_db.put_cf_raw(CF_FEE_CONFIG, range.run_key, &row)?;
            }
        }
        if order {
            // The target after the cap: a later order action never lands before this row.
            state_db.put_cf_raw(
                CF_FEE_CONFIG,
                CORE_WRITER_LAST_ORDER_TARGET_KEY,
                &target_block.to_be_bytes(),
            )?;
        }
        Ok(seq)
    }

    /// [`Self::enqueue`] under the rules of a block with or without item 7 T
    /// (`same_block` = [`torus_types::Feature::SameBlockCoreWriter`] in the
    /// queuing block's header, [`same_block_core_writer`]). `false` is
    /// exactly [`Self::enqueue`].
    pub fn enqueue_with(
        state_db: &impl StateBackend,
        action: &QueuedAction,
        same_block: bool,
    ) -> Result<u64, CoreError> {
        match (same_block, action.kind.is_order()) {
            (false, _) => Self::enqueue(state_db, action),
            // Order kinds: step 2's walk (K, the last order target, the cursor rows), in
            // the order range.
            (true, true) => Self::enqueue_walk(state_db, action, &T_ORDER_RANGE),
            (true, false) => Self::enqueue_same_block(state_db, action),
        }
    }

    /// Item 7 T: a non-order kind queued in block h targets h itself (drained after h's
    /// native batches), in the target's same-block range (seqs `0 .. CAP`); a full range
    /// sends it on to h + 1, h + 2, ..., in that range only, so order rows (the other
    /// range) never push it out. The range is dense from seq 0: rows leave a target only
    /// when its block drains it, after which no enqueue targets it (the queuing block is
    /// the earliest target); at the activation height the rows queued by H - 1 under the
    /// step 2 rules (seqs from 0) come first.
    ///
    /// Codex torus-adversarial (s138): gas does not bound the walk (10M gas every two
    /// blocks queues more than the 200 a block drains, so the backlog of full targets
    /// grows without end). The walk skips the targets known full instead of reading each
    /// one: [`CORE_WRITER_T_FIRST_OPEN_SAME_BLOCK_TARGET_KEY`] (kept here), and step 2's
    /// [`CORE_WRITER_FIRST_OPEN_TARGET_KEY`] and [`CORE_WRITER_FULL_ORDER_RUN_KEY`], whose
    /// shared range of the blocks before the activation is this range (seqs `0 .. CAP`),
    /// so the step 2 backlog past H is skipped too; after the activation nothing writes
    /// them. The target placed is the same as probing every one. A full target no row
    /// covers was filled before H by a step 2 order kind below `H - 1 + K` (K <= 100): it
    /// costs one read, and the first kept walk past it moves the cursor beyond it. A
    /// reverted call drops its cursor write together with its row (one journal frame), so
    /// the kept cursor covers every kept same-block row's full target.
    fn enqueue_same_block(
        state_db: &impl StateBackend,
        action: &QueuedAction,
    ) -> Result<u64, CoreError> {
        const CAP: u64 = torus_types::CORE_WRITER_DRAIN_CAP;
        let cursor =
            read_u64_row(state_db, CORE_WRITER_T_FIRST_OPEN_SAME_BLOCK_TARGET_KEY)?.unwrap_or(0);
        let step2_cursor = read_u64_row(state_db, CORE_WRITER_FIRST_OPEN_TARGET_KEY)?.unwrap_or(0);
        // Every target in `block_queued .. known_from` is full.
        let known_from = action.block_queued.max(cursor).max(step2_cursor);
        let mut step2_run: Option<Option<(u64, u64)>> = None; // read at the first full probe
        let mut target_block = known_from;
        loop {
            if let Some(Some((start, end))) = step2_run {
                if (start..end).contains(&target_block) {
                    target_block = end;
                    continue;
                }
            }
            if state_db
                .get_cf_len(CF_CORE_WRITER_QUEUE, &queue_key(target_block, CAP - 1))?
                .is_none()
            {
                break;
            }
            if step2_run.is_none() {
                step2_run = Some(read_order_run(state_db, CORE_WRITER_FULL_ORDER_RUN_KEY)?);
            }
            target_block = target_block
                .checked_add(1)
                .ok_or_else(|| CoreError::Overflow("CoreWriter target block".into()))?;
        }
        let seq = Self::next_sequence_in(state_db, target_block, 0)?;
        let data = borsh::to_vec(action).map_err(|e| CoreError::Borsh(e.to_string()))?;
        state_db.put_cf_raw(CF_CORE_WRITER_QUEUE, &queue_key(target_block, seq), &data)?;
        // Every target in `block_queued .. full_end` is full now (the walk's, and this
        // row's target if the row took its last slot).
        let full_end = if seq == CAP - 1 {
            target_block.saturating_add(1)
        } else {
            target_block
        };
        if full_end > known_from {
            state_db.put_cf_raw(
                CF_FEE_CONFIG,
                CORE_WRITER_T_FIRST_OPEN_SAME_BLOCK_TARGET_KEY,
                &full_end.to_be_bytes(),
            )?;
        }
        Ok(seq)
    }

    /// Drain all rows queued for the given block_number, in key (sequence)
    /// order, and delete them from state.
    ///
    /// A-exception (owner s111): one entry per row. A present row that does
    /// not decode is an `Err(UndecodableQueuedAction)` entry at its position
    /// (never silently dropped); the caller turns it into a failed result.
    /// It is still deleted, in the same write as the drain. A storage fault
    /// (the scan or a delete) is an `Err` of the whole call (fail-stop).
    pub fn drain(
        state: &impl StateBackend,
        block_number: u64,
    ) -> Result<Vec<DrainedAction>, CoreError> {
        Ok(Self::drain_keyed(state, block_number)?
            .into_iter()
            .map(|(_, action)| action)
            .collect())
    }

    /// [`Self::drain`] with each entry's queue sequence (the row key's low
    /// 8 bytes; `u64::MAX` for a key that is not `block ‖ seq`, which
    /// `enqueue` never writes). s112: the v4 action status record keys a
    /// CoreWriter result by (drain block, seq).
    pub fn drain_keyed(
        state: &impl StateBackend,
        block_number: u64,
    ) -> Result<Vec<(u64, DrainedAction)>, CoreError> {
        let prefix = block_number.to_be_bytes();
        let entries = state.iterate_cf(CF_CORE_WRITER_QUEUE, Some(&prefix))?;

        let mut actions = Vec::new();
        let mut keys_to_delete = Vec::new();

        for (key, value) in &entries {
            actions.push((
                queue_seq(key),
                QueuedAction::try_from_slice(value).map_err(|e| UndecodableQueuedAction {
                    key: key.clone(),
                    error: e.to_string(),
                }),
            ));
            keys_to_delete.push(key.clone());
        }

        // Delete drained entries
        for key in &keys_to_delete {
            state.delete_cf_raw(CF_CORE_WRITER_QUEUE, key)?;
        }

        Ok(actions)
    }

    /// Count pending actions for a given block number, through any backend
    /// (the DB, or a block overlay with its layers). `> 0` is exactly
    /// [`StateBackend::prefix_exists`] on the target's prefix (the due-check).
    pub fn pending_count(state: &impl StateBackend, block_number: u64) -> Result<usize, CoreError> {
        let prefix = block_number.to_be_bytes();
        Ok(state.iterate_cf(CF_CORE_WRITER_QUEUE, Some(&prefix))?.len())
    }

    /// Find the next sequence number for a target block.
    ///
    /// T4.4: goes through [`StateBackend::iterate_cf`] (instead of the FIX EVM-PF-16 raw
    /// reverse seek) so pending journal writes from earlier calls in the SAME tx/block are
    /// visible (read-your-writes — two placeOrder calls in one tx must get seq 0 then 1).
    /// The scan is bounded to a single target-block prefix, which holds at most
    /// [`torus_types::CORE_WRITER_DRAIN_CAP`] rows (item 7 step 2).
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

impl CoreWriterQueue {
    /// Item 7 T: the next sequence of the seq range `base .. base + CAP` of
    /// `target_block` (one past its highest taken seq, else `base`), reading at
    /// most that range's rows. Read-your-writes through any backend, as
    /// [`Self::next_sequence`].
    fn next_sequence_in(
        state_db: &impl StateBackend,
        target_block: u64,
        base: u64,
    ) -> Result<u64, CoreError> {
        const CAP: u64 = torus_types::CORE_WRITER_DRAIN_CAP;
        let end = base + CAP;
        let rows = state_db.iterate_cf_prefix_from(
            CF_CORE_WRITER_QUEUE,
            &target_block.to_be_bytes(),
            &queue_key(target_block, base),
            CAP as usize,
        )?;
        Ok(rows
            .iter()
            .map(|(key, _)| queue_seq(key))
            .filter(|seq| (base..end).contains(seq))
            .max()
            .map_or(base, |seq| seq + 1))
    }
}

/// The sequence of a queue row key `block (8 BE) ‖ seq (8 BE)`; `u64::MAX`
/// for any other length.
fn queue_seq(key: &[u8]) -> u64 {
    <[u8; 16]>::try_from(key)
        .map(|k| u64::from_be_bytes(k[8..].try_into().expect("8 bytes")))
        .unwrap_or(u64::MAX)
}

// ============================================================================
// Queued Action Types
// ============================================================================

/// One drained queue row: the action, or why its row did not decode.
pub type DrainedAction = Result<QueuedAction, UndecodableQueuedAction>;

/// A-exception (owner s111): a `CF_CORE_WRITER_QUEUE` row that was present
/// but did not decode as a [`QueuedAction`]. Deterministic (a pure function
/// of the row's bytes), so every validator reports the same failure.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UndecodableQueuedAction {
    /// The row's key: target block (8 BE) ‖ sequence (8 BE).
    pub key: Vec<u8>,
    /// The decode error.
    pub error: String,
}

impl std::fmt::Display for UndecodableQueuedAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match <[u8; 16]>::try_from(self.key.as_slice()) {
            Ok(k) => write!(
                f,
                "undecodable queued action (block {}, seq {}): {}",
                u64::from_be_bytes(k[..8].try_into().expect("8 bytes")),
                u64::from_be_bytes(k[8..].try_into().expect("8 bytes")),
                self.error
            ),
            Err(_) => write!(
                f,
                "undecodable queued action (key 0x{}): {}",
                alloy_primitives::hex::encode(&self.key),
                self.error
            ),
        }
    }
}

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
        /// s114: the client order id (row: `0`, or `1 ‖ 16 BE` after
        /// `time_in_force`).
        cloid: Option<Cloid>,
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
    /// s114: cancel by client order id (borsh tag 8 — appended):
    /// `market 8 BE ‖ cloid 16 BE`.
    CancelByCloid {
        market_id: MarketId,
        cloid: Cloid,
    },
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

impl QueuedActionKind {
    /// Item 7 step 2 (owner decision 4): order actions (the placement and every cancel, so a
    /// contract's cancel never overtakes its order) wait K blocks; staking and lockbox kinds
    /// one.
    pub fn is_order(&self) -> bool {
        matches!(
            self,
            Self::PlaceOrder { .. }
                | Self::CancelOrder { .. }
                | Self::CancelAll { .. }
                | Self::CancelByCloid { .. }
        )
    }
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
                cloid,
            } => {
                w.write_all(&[0])?;
                w.write_all(&market_id.to_be_bytes())?;
                w.write_all(&[*side])?;
                w.write_all(&[*order_type])?;
                borsh_write_fp(price, w)?;
                borsh_write_fp(quantity, w)?;
                w.write_all(&[*time_in_force])?;
                match cloid {
                    None => w.write_all(&[0])?,
                    Some(c) => {
                        w.write_all(&[1])?;
                        w.write_all(&c.to_be_bytes())?;
                    }
                }
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
            Self::CancelByCloid { market_id, cloid } => {
                w.write_all(&[8])?;
                w.write_all(&market_id.to_be_bytes())?;
                w.write_all(&cloid.to_be_bytes())?;
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
                let mut ct = [0u8; 1];
                r.read_exact(&mut ct)?;
                let cloid = match ct[0] {
                    0 => None,
                    1 => {
                        let mut cb = [0u8; 16];
                        r.read_exact(&mut cb)?;
                        Some(Cloid::from_be_bytes(cb))
                    }
                    x => {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            format!("invalid PlaceOrder cloid tag: {x}"),
                        ))
                    }
                };
                Ok(Self::PlaceOrder {
                    market_id,
                    side,
                    order_type,
                    price,
                    quantity,
                    time_in_force: tf[0],
                    cloid,
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
            8 => {
                let mut mb = [0u8; 8];
                r.read_exact(&mut mb)?;
                let mut cb = [0u8; 16];
                r.read_exact(&mut cb)?;
                Ok(Self::CancelByCloid {
                    market_id: u64::from_be_bytes(mb),
                    cloid: Cloid::from_be_bytes(cb),
                })
            }
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
    use crate::position::Position;
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

    /// The DB with the oracle CF's reads failing (everything else works).
    #[derive(Clone)]
    struct FailingOracleRead(StateDb);

    impl StateBackend for FailingOracleRead {
        fn get_cf_raw(&self, cf: &str, key: &[u8]) -> Result<Option<Vec<u8>>, StateError> {
            if cf == CF_NATIVE_ORACLE {
                return Err(StateError::Io(std::io::Error::other("injected read failure")));
            }
            self.0.get_cf_raw(cf, key)
        }
        fn put_cf_raw(&self, cf: &str, key: &[u8], value: &[u8]) -> Result<(), StateError> {
            self.0.put_cf_raw(cf, key, value)
        }
        fn delete_cf_raw(&self, cf: &str, key: &[u8]) -> Result<(), StateError> {
            self.0.delete_cf_raw(cf, key)
        }
        fn iterate_cf(&self, cf: &str, prefix: Option<&[u8]>) -> Result<Vec<(Vec<u8>, Vec<u8>)>, StateError> {
            self.0.iterate_cf(cf, prefix)
        }
        fn atomic_write(&self, ops: &[AtomicWriteOp<'_>]) -> Result<(), StateError> {
            self.0.atomic_write(ops)
        }
    }

    /// R02 branch 5 (owner option A): getPosition's UPnL oracle read fails on
    /// this node -> `PrecompileLocalFault` (local), never UPnL 0. RED before:
    /// the output carried UPnL 0. A failed POSITION read keeps today's error
    /// (a plain `State` revert; not part of this decision).
    #[test]
    fn r02_get_position_oracle_read_fault_is_a_precompile_local_fault() {
        let dir = tempfile::tempdir().unwrap();
        let db = StateDb::open(dir.path()).unwrap();
        let trader = Address::new([7; 20]);
        crate::position::PositionManager::new(db.clone())
            .put_position(&Position {
                trader,
                market_id: 1,
                is_long: true,
                size: FixedPoint::ONE,
                entry_price: FixedPoint::ONE,
                cost_basis: FixedPoint::ONE,
                realized_pnl: FixedPoint::ZERO,
                isolated_margin: FixedPoint::ZERO,
                margin_type: crate::position::MarginType::Cross,
            })
            .unwrap();
        match read_position(&FailingOracleRead(db.clone()), &trader, 1, 1_000) {
            Err(e @ CoreError::PrecompileLocalFault(_)) => {
                assert!(e.is_local_fault());
                assert!(e.to_string().contains("getPosition oracle read: state error"), "{e}");
            }
            other => panic!("expected PrecompileLocalFault, got {other:?}"),
        }
        let healthy = read_position(&db, &trader, 1, 1_000).expect("no oracle row: UPnL 0");
        assert_eq!(&healthy[64..96], &[0u8; 32]);
        let e = read_position(&FailingRead, &trader, 1, 1_000).expect_err("position read failed");
        assert!(matches!(e, CoreError::State(_)), "{e}");
    }

    /// Pre-testnet "aliased opaque position rows": getPosition refuses a
    /// stored row whose fields name another key (as every position load
    /// does), a local fault, instead of answering with another market's
    /// position.
    #[test]
    fn get_position_refuses_a_row_naming_another_key() {
        let dir = tempfile::tempdir().unwrap();
        let db = StateDb::open(dir.path()).unwrap();
        let trader = Address::new([7; 20]);
        let row = Position {
            trader,
            market_id: 2,
            is_long: true,
            size: FixedPoint::ONE,
            entry_price: FixedPoint::ONE,
            cost_basis: FixedPoint::ONE,
            realized_pnl: FixedPoint::ZERO,
            isolated_margin: FixedPoint::ZERO,
            margin_type: crate::position::MarginType::Cross,
        };
        db.put_cf_raw(
            CF_NATIVE_POSITIONS,
            &crate::position::position_key(&trader, 1),
            &borsh::to_vec(&row).unwrap(),
        )
        .unwrap();
        let e = read_position(&db, &trader, 1, 1_000).expect_err("aliased row");
        assert!(matches!(e, CoreError::Borsh(_)) && e.is_local_fault(), "{e}");
        assert!(e.to_string().starts_with("borsh error: position row"), "{e}");
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

    // F1 Codex review P1: a scan's storage fault is a LOCAL fault (the EVM
    // provider fail-stops on `CoreError::is_local_fault`), never an empty
    // answer. A missing CF used to read as empty: a success with a gas charge
    // only this node computes.

    use torus_state::{AtomicWriteOp, StateError};

    /// Every (metered, unlimited) x (prefix, no prefix) x max_rows mode.
    fn modes() -> Vec<(ReadMeter, &'static [u8], Option<u64>)> {
        let mut out = Vec::new();
        for prefix in [b"" as &[u8], b"agg"] {
            for max_rows in [None, Some(3)] {
                out.push((ReadMeter::with_max(100_000), prefix, max_rows));
                out.push((ReadMeter::with_max(0), prefix, max_rows));
                out.push((ReadMeter::unlimited(), prefix, max_rows));
            }
        }
        out
    }

    /// A DB opened without `missing` (every read of it fails here only).
    fn db_without(missing: &str) -> (tempfile::TempDir, StateDb) {
        let dir = tempfile::tempdir().unwrap();
        let mut opts = rocksdb::Options::default();
        opts.create_if_missing(true);
        opts.create_missing_column_families(true);
        let cfs = torus_state::cf::ALL_CF_NAMES
            .iter()
            .copied()
            .filter(|cf| *cf != missing);
        let db = StateDb::from_existing_db(rocksdb::DB::open_cf(&opts, dir.path(), cfs).unwrap());
        (dir, db)
    }

    #[test]
    fn scan_of_a_missing_cf_is_a_local_fault() {
        let (_d, db) = db_without(CF_NATIVE_ORACLE);
        for (mut meter, prefix, max_rows) in modes() {
            let e = scan_prefix_metered(&db, CF_NATIVE_ORACLE, prefix, max_rows, &mut meter)
                .expect_err("missing CF scanned as empty");
            assert!(e.is_local_fault(), "{e}");
            assert!(e.to_string().contains(CF_NATIVE_ORACLE), "{e}");
        }
    }

    /// `StateDb` with the iterator of one CF failing (an I/O error, e.g. a
    /// corrupt SST block); point reads and other CFs work.
    #[derive(Clone)]
    struct FailingScan(StateDb, &'static str);

    impl StateBackend for FailingScan {
        fn get_cf_raw(&self, cf: &str, key: &[u8]) -> Result<Option<Vec<u8>>, StateError> {
            self.0.get_cf_raw(cf, key)
        }
        fn put_cf_raw(&self, cf: &str, key: &[u8], value: &[u8]) -> Result<(), StateError> {
            self.0.put_cf_raw(cf, key, value)
        }
        fn delete_cf_raw(&self, cf: &str, key: &[u8]) -> Result<(), StateError> {
            self.0.delete_cf_raw(cf, key)
        }
        fn iterate_cf(
            &self,
            cf: &str,
            prefix: Option<&[u8]>,
        ) -> Result<Vec<(Vec<u8>, Vec<u8>)>, StateError> {
            if cf == self.1 {
                return Err(StateError::Io(std::io::Error::other(
                    "injected iterator failure",
                )));
            }
            self.0.iterate_cf(cf, prefix)
        }
        fn atomic_write(&self, ops: &[AtomicWriteOp<'_>]) -> Result<(), StateError> {
            self.0.atomic_write(ops)
        }
    }

    #[test]
    fn scan_iterator_error_is_a_local_fault() {
        let dir = tempfile::tempdir().unwrap();
        let db = FailingScan(StateDb::open(dir.path()).unwrap(), CF_NATIVE_ORACLE);
        for (mut meter, prefix, max_rows) in modes() {
            let e = scan_prefix_metered(&db, CF_NATIVE_ORACLE, prefix, max_rows, &mut meter)
                .expect_err("iterator failure scanned as a result");
            assert!(e.is_local_fault(), "{e}");
        }
    }

    /// An existing but empty CF is the normal empty result, nothing charged.
    #[test]
    fn scan_of_an_empty_cf_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let db = StateDb::open(dir.path()).unwrap();
        for (mut meter, prefix, max_rows) in modes() {
            let rows = scan_prefix_metered(&db, CF_NATIVE_ORACLE, prefix, max_rows, &mut meter)
                .expect("empty CF");
            assert!(rows.is_empty());
            assert_eq!(meter.used(), 0);
        }
    }

    /// (case, scanned CF, precompile, calldata): every reader that scans.
    fn scan_readers() -> Vec<(&'static str, &'static str, u16, Vec<u8>)> {
        let sel = |sig: &str| selector_for(sig).to_be_bytes().to_vec();
        let staker = [sel("getStakingInfo(address)"), vec![0u8; 12], vec![7u8; 20]].concat();
        vec![
            (
                "getMarkets",
                CF_NATIVE_MARKETS,
                ADDR_BALANCE_READER,
                sel("getMarkets()"),
            ),
            (
                "getAllPrices",
                CF_NATIVE_ORACLE,
                ADDR_ORACLE_READER,
                sel("getAllPrices()"),
            ),
            (
                "getStakingInfo",
                CF_STAKING_DELEGATIONS,
                ADDR_STAKING_READER,
                staker,
            ),
            (
                "getValidators",
                CF_STAKING_VALIDATORS,
                ADDR_STAKING_READER,
                sel("getValidators()"),
            ),
        ]
    }

    fn run_reader(
        state: &impl StateBackend,
        id: u16,
        data: &[u8],
        meter: &mut ReadMeter,
    ) -> Result<Vec<u8>, CoreError> {
        execute_precompile_metered(
            &precompile_address(id),
            data,
            &Address::ZERO,
            U256::ZERO,
            state,
            1,
            1_000,
            false,
            meter,
        )
    }

    /// Through each scan reader (metered as in the EVM, and unlimited): a
    /// missing CF and an iterator error are local faults; an empty CF
    /// answers.
    #[test]
    fn scan_readers_storage_faults_are_local_faults() {
        let dir = tempfile::tempdir().unwrap();
        let healthy = StateDb::open(dir.path()).unwrap();
        for (what, cf, id, data) in scan_readers() {
            for meter in [ReadMeter::with_max(100_000), ReadMeter::unlimited()] {
                let (_d, db) = db_without(cf);
                let e = run_reader(&db, id, &data, &mut { meter }).expect_err(what);
                assert!(e.is_local_fault(), "{what} missing CF: {e}");
                let failing = FailingScan(healthy.clone(), cf);
                let e = run_reader(&failing, id, &data, &mut { meter }).expect_err(what);
                assert!(e.is_local_fault(), "{what} iterator error: {e}");
                run_reader(&healthy, id, &data, &mut { meter })
                    .unwrap_or_else(|e| panic!("{what} empty CF: {e}"));
            }
        }
    }
}
