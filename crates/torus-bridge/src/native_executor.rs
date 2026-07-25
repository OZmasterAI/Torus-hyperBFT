//! Native action execution — dispatches each NativeAction to the correct handler.
//!
//! NativeExecutor is the core dispatch layer for processing native (non-EVM) actions
//! within a block. It handles order book operations, staking, oracle, governance,
//! lockbox, and liquidation processing in a deterministic pipeline.
//!
//! Task 2.5.1: NativeExecutor dispatch table + batch execution.

use std::collections::HashMap;

use alloy_primitives::{Address, B256};
use torus_core::error::CoreError;
use torus_core::liquidation::LiquidationEngine;
use torus_core::lockbox::{fp_to_u256, u256_to_fp, Lockbox};
use torus_core::margin::{effective_max_leverage, MarketMarginConfig};
use torus_core::oracle::{OracleConfig, OracleManager};
use torus_core::order_book::{OrderBook, OrderStatus, PlaceResult};
use torus_core::position::{MarginType, NativeBalance, PositionCache, PositionManager};
use torus_core::precompiles::{CoreWriterQueue, QueuedAction, QueuedActionKind};
use torus_economics::epoch::ValidatorSetDiff;
use torus_economics::{
    EpochManager, GovernanceManager, RewardDistributor, StakingManager, ValidatorStatus,
};
use torus_state::cf::{CF_NATIVE_TRADES, CF_NATIVE_USER_TRADES};
use torus_state::{RawCfKv, StateBackend, StateDb};
use torus_types::{
    FixedPoint, MarketId, NativeAction, OrderId, OrderType, PlaceOrderParams, PublicKey,
    SessionScope, Side, TimeInForce, ValidatorInfo, ValidatorSet, VoteOption, U256,
};

use crate::market_workers::{MarketWorkerPool, MatchRequest};

// ============================================================================
// Result types
// ============================================================================

/// Result of executing a single native action.
#[derive(Clone, Debug)]
pub struct NativeActionResult {
    pub action_type: &'static str,
    pub success: bool,
    pub error: Option<String>,
    pub gas_used: u64,
}

impl NativeActionResult {
    fn ok(action_type: &'static str, gas_used: u64) -> Self {
        Self {
            action_type,
            success: true,
            error: None,
            gas_used,
        }
    }

    fn err(action_type: &'static str, error: String) -> Self {
        Self {
            action_type,
            success: false,
            error: Some(error),
            gas_used: 0,
        }
    }
}

/// Result of executing a batch of native actions.
#[derive(Clone, Debug)]
pub struct NativeBatchResult {
    pub results: Vec<NativeActionResult>,
    pub total_gas: u64,
}

/// Result of epoch boundary processing. Carries the validator set diff
/// for future hotstuff_rs ValidatorSetUpdates integration.
pub struct EpochBoundaryResult {
    pub action: NativeActionResult,
    pub new_set: Option<ValidatorSet>,
    pub diff: Option<ValidatorSetDiff>,
}

// ============================================================================
// Execution context — holds all state managers and per-block metadata
// ============================================================================

/// All state managers and block metadata needed for native execution.
/// Per-`execute_batch`-call write-back cache for native balances (O1).
///
/// Phase 2 (margin reserve) and Phase 4 (margin release) touch each sender's
/// `NativeBalance` repeatedly. The dominant cost is the per-order `NativeStateOverlay`
/// PUT: each `put_cf_raw` allocates a `String` CF-name + key/value `Vec`s, Borsh-encodes,
/// takes an `RwLock`, and inserts into a `BTreeMap`. At bs400 with N flood senders, a
/// naive path pays one such PUT per order (~4×N per block); this cache defers those,
/// mutating an in-memory typed map and flushing each *unique* sender's final balance to
/// the overlay once (≈N PUTs), while first-read misses fall through to the overlay.
///
/// Coherence (the overlay must be authoritative at every point some *other* reader could
/// observe it): NO in-call reader bypasses this cache (C1). Historically Phase 4
/// `apply_fill` credited realized PnL straight to the overlay, which forced a
/// flush-and-evict of both parties' balances before every fill; settlement now uses
/// `apply_fill_cached`, which never touches balances — it RETURNS the PnL event and the
/// executor credits it through this cache, keeping it the single balance authority for
/// the whole call (and killing 2 overlay round-trips per fill). Position rows get the
/// same treatment via `PositionCache`. `flush_all` runs at the end of the call, so the
/// *next* `execute_batch` call and all post-batch consumers (`drain_core_writer`,
/// `save_order_books`, block-end flush) see a fully materialized overlay. Flush order is
/// over distinct per-sender keys, so it is state-independent of iteration order; the map
/// is otherwise never iterated.
struct BalanceCache {
    map: HashMap<Address, NativeBalance>,
    dirty: std::collections::HashSet<Address>,
}

impl BalanceCache {
    fn new() -> Self {
        Self {
            map: HashMap::new(),
            dirty: std::collections::HashSet::new(),
        }
    }

    /// Return the sender's balance, reading through to `positions` on a miss.
    fn load<T: StateBackend>(
        &mut self,
        positions: &PositionManager<T>,
        addr: &Address,
    ) -> Result<NativeBalance, CoreError> {
        if let Some(bal) = self.map.get(addr) {
            return Ok(bal.clone());
        }
        let bal = positions.get_native_balance(addr)?;
        self.map.insert(*addr, bal.clone());
        Ok(bal)
    }

    /// Update the cached balance and mark it dirty (write-back — no overlay PUT yet).
    fn set(&mut self, addr: &Address, bal: NativeBalance) {
        self.map.insert(*addr, bal);
        self.dirty.insert(*addr);
    }

    /// L3-ENG: absorb a Phase-2 worker's cache. Caller guarantees the key
    /// sets are DISJOINT (workers are sharded by sender), so merge order
    /// cannot affect any entry and the result equals the serial cache.
    fn merge_disjoint(&mut self, other: BalanceCache) {
        self.map.extend(other.map);
        self.dirty.extend(other.dirty);
    }

    /// Flush every pending dirty balance to the overlay (end of the `execute_batch` call).
    /// Keys are distinct per sender, so final overlay state is independent of flush order;
    /// sorted anyway to keep the write sequence deterministic.
    fn flush_all<T: StateBackend>(
        &mut self,
        positions: &PositionManager<T>,
    ) -> Result<(), CoreError> {
        let mut addrs: Vec<Address> = self.dirty.iter().copied().collect();
        addrs.sort();
        for addr in &addrs {
            if let Some(bal) = self.map.get(addr) {
                positions.put_native_balance(addr, bal)?;
            }
        }
        self.dirty.clear();
        Ok(())
    }
}

// ============================================================================
// C3 — deterministic parallel Phase-4 settlement: plumbing types
// ============================================================================

/// C2/C3: one Phase-2-prepared PlaceOrder flowing through matching (Phase 3)
/// and settlement (Phase 4). `params` borrows the caller's committed action
/// slice — no per-order deep clone anywhere in the pipeline.
struct PreparedOrder<'a> {
    index: usize,
    sender: Address,
    params: &'a PlaceOrderParams,
    order_id: u128,
    margin_reserved: FixedPoint,
}

/// One fill's trade-history rows — exactly the bytes `persist_trade` has
/// always written to `CF_NATIVE_TRADES` (primary) and `CF_NATIVE_USER_TRADES`
/// (maker + taker secondary index). C3: settle workers byte-build these
/// off-thread with a PROVISIONAL trade index; the single-threaded apply pass
/// stamps the definitive per-block index (`stamp_trade_index`) in canonical
/// settlement order before routing, so the persisted key sequence is
/// byte-identical to the sequential path's inline `persist_trade` calls.
struct TradeKvs {
    trade_key: [u8; 20],
    trade_data: Vec<u8>,
    maker_key: [u8; 32],
    maker_data: Vec<u8>,
    taker_key: [u8; 32],
    taker_data: Vec<u8>,
}

impl TradeKvs {
    /// Byte-identical to the classic inline `persist_trade` construction.
    fn build(
        market_id: MarketId,
        block_height: u64,
        timestamp: u64,
        trade_index: u32,
        fill: &torus_core::order_book::Fill,
    ) -> Self {
        let taker_side: u8 = if fill.maker_side == Side::Buy { 1 } else { 0 };

        // Primary key: market_id(8) + block_number(8) + trade_index(4)
        let mut trade_key = [0u8; 20];
        trade_key[..8].copy_from_slice(&market_id.to_be_bytes());
        trade_key[8..16].copy_from_slice(&block_height.to_be_bytes());
        trade_key[16..20].copy_from_slice(&trade_index.to_be_bytes());

        let trade_id = trade_index as u128;
        let price_raw = fill.price.raw();
        let quantity_raw = fill.quantity.raw();

        // Borsh-serialize trade data (matches StoredTrade layout).
        // 16+16+16+1+8+8 = 65 bytes exactly (the classic with_capacity(64)
        // paid one realloc per fill).
        let mut trade_data = Vec::with_capacity(65);
        trade_data.extend_from_slice(&trade_id.to_le_bytes());
        trade_data.extend_from_slice(&price_raw.to_le_bytes());
        trade_data.extend_from_slice(&quantity_raw.to_le_bytes());
        trade_data.push(taker_side);
        trade_data.extend_from_slice(&block_height.to_le_bytes());
        trade_data.extend_from_slice(&timestamp.to_le_bytes());

        // Secondary index: per-user trades (descending block order).
        // 16+8+16+16+1+1+8+8 = 74 bytes exactly.
        let desc_block = u64::MAX - block_height;
        let mut maker_data = Vec::with_capacity(74);
        maker_data.extend_from_slice(&trade_id.to_le_bytes());
        maker_data.extend_from_slice(&market_id.to_le_bytes());
        maker_data.extend_from_slice(&price_raw.to_le_bytes());
        maker_data.extend_from_slice(&quantity_raw.to_le_bytes());
        maker_data.push(taker_side);
        maker_data.push(0u8); // role: maker
        maker_data.extend_from_slice(&block_height.to_le_bytes());
        maker_data.extend_from_slice(&timestamp.to_le_bytes());

        let mut maker_key = [0u8; 32];
        maker_key[..20].copy_from_slice(fill.maker.as_slice());
        maker_key[20..28].copy_from_slice(&desc_block.to_be_bytes());
        maker_key[28..32].copy_from_slice(&trade_index.to_be_bytes());

        // Taker entry (flip role byte at offset 57: 16+8+16+16+1)
        let mut taker_data = maker_data.clone();
        taker_data[57] = 1u8; // role: taker
        let mut taker_key = [0u8; 32];
        taker_key[..20].copy_from_slice(fill.taker.as_slice());
        taker_key[20..28].copy_from_slice(&desc_block.to_be_bytes());
        taker_key[28..32].copy_from_slice(&trade_index.to_be_bytes());

        Self {
            trade_key,
            trade_data,
            maker_key,
            maker_data,
            taker_key,
            taker_data,
        }
    }

    /// Stamp the definitive per-block trade index over the provisional one:
    /// 4-byte BE index in each of the 3 keys, 16-byte LE trade_id at the head
    /// of each of the 3 values. Offsets are fixed by the layouts in `build`.
    fn stamp_trade_index(&mut self, idx: u32) {
        let be = idx.to_be_bytes();
        let id_le = (idx as u128).to_le_bytes();
        self.trade_key[16..20].copy_from_slice(&be);
        self.trade_data[0..16].copy_from_slice(&id_le);
        self.maker_key[28..32].copy_from_slice(&be);
        self.maker_data[0..16].copy_from_slice(&id_le);
        self.taker_key[28..32].copy_from_slice(&be);
        self.taker_data[0..16].copy_from_slice(&id_le);
    }
}

/// C3: everything a settle worker precomputes for ONE prepared order —
/// aligned index-for-index with the market's `PreparedOrder`s/match results.
/// All of it is either market-local (position transitions live in the plan's
/// `PositionCache`) or balance-INDEPENDENT amounts; every cross-market
/// mutation (balance clamps, trade_index, results) happens later on the apply
/// thread in canonical order.
struct OrderSettlePlan {
    /// Taker-side order-margin release amount (pre-clamp; ZERO = none).
    margin_release: FixedPoint,
    /// Realized-PnL events in exact fill-application order: (side label for
    /// error text, trader, pnl). Emitted precisely when `apply_fill_cached`
    /// returns `Some` — including `Some(ZERO)` (still materializes the row).
    pnl_events: Vec<(&'static str, Address, FixedPoint)>,
    /// First position-side fill-application failure, pre-formatted like the
    /// sequential path ("taker fill failed: …" / "maker fill failed: …").
    fill_error: Option<String>,
    /// Prebuilt trade rows (provisional index), empty when `fill_error`.
    trades: Vec<TradeKvs>,
}

/// C3: one market's full settlement plan, computed off-thread by a pure pass
/// over `(MarketBatchResult, [PreparedOrder])`.
struct MarketSettlePlan {
    orders: Vec<OrderSettlePlan>,
    /// This market's position mutations. Keys are `(trader, market_id)` — the
    /// per-market key sets are disjoint, so merging into the batch cache in
    /// sorted market order reproduces the sequential cache exactly.
    pos_cache: PositionCache,
    /// A5 maker/STP release amounts, in the deterministic order
    /// `maker_margin_releases` has always produced.
    maker_releases: Vec<(Address, FixedPoint)>,
}

/// C3 runtime toggle: `TORUS_PARALLEL_SETTLE=1` enables the parallel settle
/// path; anything else (INCLUDING UNSET) keeps today's sequential loop.
/// Default OFF — unset env is byte-identical to the pre-C3 serial semantics.
/// Read once per process.
fn parallel_settle_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| {
        parse_parallel_settle_toggle(std::env::var("TORUS_PARALLEL_SETTLE").ok())
    })
}

/// Pure parse of the `TORUS_PARALLEL_SETTLE` value: only `"1"` enables the
/// parallel path (exact-today default — unset/`"0"`/garbage all mean the
/// classic sequential settle loop).
fn parse_parallel_settle_toggle(v: Option<String>) -> bool {
    matches!(v.as_deref().map(str::trim), Some("1"))
}

/// C3 auto-mode work gate: parallel settle pays a thread scope + plan handoff,
/// so the env-driven path engages it only when the batch produced at least
/// this many fills (across >=2 markets). Explicitly forced modes
/// (`execute_batch_settle_mode`) bypass the gate — determinism holds at any
/// size, this is purely a break-even heuristic. Tunable for bench sweeps via
/// `TORUS_PARALLEL_SETTLE_MIN_FILLS`.
fn parallel_settle_min_fills() -> usize {
    static MIN: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *MIN.get_or_init(|| {
        std::env::var("TORUS_PARALLEL_SETTLE_MIN_FILLS")
            .ok()
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(1024)
    })
}

/// How the Phase-4 settle path is chosen.
#[derive(Clone, Copy)]
enum SettleMode {
    /// Env toggle + work gate (the live `execute_batch` path).
    Auto,
    /// Pinned by the caller (tests / A-B benches): true = parallel whenever
    /// >=2 markets have work, false = always the sequential loop.
    Force(bool),
}

#[cfg(test)]
mod parallel_settle_toggle_tests {
    use super::parse_parallel_settle_toggle;

    #[test]
    fn default_is_off() {
        assert!(!parse_parallel_settle_toggle(None));
    }

    #[test]
    fn one_enables() {
        assert!(parse_parallel_settle_toggle(Some("1".to_string())));
        assert!(parse_parallel_settle_toggle(Some(" 1 ".to_string())));
    }

    #[test]
    fn anything_else_stays_off() {
        for v in ["0", "true", "on", "", "yes", "2"] {
            assert!(!parse_parallel_settle_toggle(Some(v.to_string())), "{v}");
        }
    }
}

// ============================================================================
// L3-ENG — TORUS_PARALLEL_ENGINE: sender-sharded parallel Phase-2 prepare +
// parallel-settle engagement at cap-400 block shapes.
// See docs/design-parallel-engine.md for the determinism argument (the
// phase-sequencing law: all of Phase 2 precedes all matching precedes all
// settlement, so cross-market coupling through shared trader balances exists
// ONLY inside Phase 2 — serialized per sender by sharding on sender — and
// inside Phase 4 — serialized by the C3 pass-B canonical apply).
// ============================================================================

/// L3-ENG runtime toggle: `TORUS_PARALLEL_ENGINE=N` with N>=2 enables the
/// parallel engine path with N Phase-2 workers; anything else (INCLUDING
/// UNSET, `0`, `1`) keeps today's serial prepare — exact-today default.
/// Read once per process. Node-local: outputs are byte-identical either way.
fn parallel_engine_threads() -> usize {
    static THREADS: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *THREADS.get_or_init(|| {
        parse_parallel_engine_threads(std::env::var("TORUS_PARALLEL_ENGINE").ok())
    })
}

/// Pure parse of the `TORUS_PARALLEL_ENGINE` value: only an integer N>=2
/// enables (capped at 32 workers); unset/`0`/`1`/garbage all mean OFF.
fn parse_parallel_engine_threads(v: Option<String>) -> usize {
    match v.as_deref().map(str::trim).and_then(|s| s.parse::<usize>().ok()) {
        Some(n) if n >= 2 => n.min(32),
        _ => 0,
    }
}

/// L3-ENG work gate: sharded Phase-2 prepare pays a thread scope + outcome
/// stitch, so the env-driven path engages only when the batch carries at
/// least this many PlaceOrders (across >=2 senders). Forced modes bypass it —
/// determinism holds at any size; this is purely break-even. Tunable via
/// `TORUS_PARALLEL_ENGINE_MIN_ORDERS`.
fn parallel_engine_min_orders() -> usize {
    static MIN: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *MIN.get_or_init(|| {
        std::env::var("TORUS_PARALLEL_ENGINE_MIN_ORDERS")
            .ok()
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(64)
    })
}

/// L3-ENG settle-engagement gate: with the engine on, the C3 parallel settle
/// engages from this many fills (vs `TORUS_PARALLEL_SETTLE`'s 1024 default,
/// which never fires at cap-400). Perf-only. Tunable via
/// `TORUS_PARALLEL_ENGINE_MIN_FILLS`.
fn parallel_engine_min_fills() -> usize {
    static MIN: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *MIN.get_or_init(|| {
        std::env::var("TORUS_PARALLEL_ENGINE_MIN_FILLS")
            .ok()
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(64)
    })
}

/// How the engine (Phase-2 prepare) path is chosen.
#[derive(Clone, Copy)]
enum EngineMode {
    /// Env toggle + work gates (the live `execute_batch` path).
    Auto,
    /// Pinned by the caller (tests / benches): 0 or 1 = serial prepare,
    /// N>=2 = sharded prepare with N workers, gates bypassed.
    Force(usize),
}

/// L3-ENG: one Phase-2 outcome for a single flattened PlaceOrder, computed by
/// a sharded worker and applied by the serial stitch in flat order.
enum PrepOutcome {
    /// Margin reserved (possibly ZERO for market orders) — the stitch assigns
    /// the global order id and builds the `PreparedOrder`.
    Pass(FixedPoint),
    /// Rejected pre-book. `margin` selects the funnel counter
    /// (`orders_rejected_margin` vs `orders_rejected_other`); `msg` is the
    /// exact serial-path error string.
    Reject { margin: bool, msg: String },
}

#[cfg(test)]
mod parallel_engine_toggle_tests {
    use super::parse_parallel_engine_threads;

    #[test]
    fn default_is_off() {
        assert_eq!(parse_parallel_engine_threads(None), 0);
    }

    #[test]
    fn n_at_least_two_enables() {
        assert_eq!(parse_parallel_engine_threads(Some("2".to_string())), 2);
        assert_eq!(parse_parallel_engine_threads(Some(" 8 ".to_string())), 8);
        assert_eq!(parse_parallel_engine_threads(Some("18".to_string())), 18);
    }

    #[test]
    fn capped_at_32() {
        assert_eq!(parse_parallel_engine_threads(Some("4096".to_string())), 32);
    }

    #[test]
    fn zero_one_and_garbage_stay_off() {
        for v in ["0", "1", "true", "on", "", "yes", "-2", "2.5"] {
            assert_eq!(parse_parallel_engine_threads(Some(v.to_string())), 0, "{v}");
        }
    }
}

// ============================================================================
// Book persistence modes — C4 rows (`TORUS_BOOK_ROWS=1`) + 3c level authority
// (`TORUS_BOOK_ROWS=2`)
// ============================================================================
//
// Classic path (default): each market's ENTIRE `OrderBook` is Borsh-serialized
// into one `CF_NATIVE_ORDER_BOOKS` row per touched block — O(book depth) bytes
// serialized AND state-root-hashed per block, the 2GB-RSS / swap driver once
// books hold >1M resting orders.
//
// OrderRows (`TORUS_BOOK_ROWS=1`, C4): one root-CF KV row per resting order /
// pending stop plus one small meta row per market. Saves drain the book's own
// mutation journal (journal-in-book, 3c port) — O(touched orders).
//
// LevelAuthority (`TORUS_BOOK_ROWS=2`, 3c): per-price-level aggregate rows
// (tag 0x03, value = total_qty ‖ order_count ‖ level_hash) become the root
// authority; full order rows move to the NODE-LOCAL `CF_BOOK_ORDER_ROWS`
// (never bucketed/mirrored/hashed). Root dirty entries scale with touched
// LEVELS (10s–100s), not touched orders (~16k). Order identity stays
// consensus-committed via the per-level keccak commitment inside the level
// row; the node-local store is verified against it at boot.
//
// ############################ CONSENSUS WARNING ############################
// `CF_NATIVE_ORDER_BOOKS` is one of the 6 native-state-root CFs. Each mode
// stores DIFFERENT keys/values, so THE MODE CHANGES THE STATE-ROOT FORMAT:
//   - every validator in a fleet MUST run the same TORUS_BOOK_ROWS value —
//     a mixed fleet forks at the first block that touches any book;
//   - flipping the mode on an existing chain REQUIRES a fresh genesis —
//     there is no migration, and load refuses to start (fail-stop via
//     ctx.fatal_error) when the CF's on-disk content OR the node-local
//     `__book_mode__` marker does not match the configured mode.
// Default (unset/other) = Classic = byte-identical persistence to today.
// ###########################################################################
//
// Row schema: see `torus_core::book_rows` (frozen key/value layouts; level
// tag 0x03 — 0x02 stays frozen as stops).
//
// `seq` is the queue-priority sequence: within one (side, price) level, FIFO
// order == ascending seq. 3c: seqs are assigned AT INSERT TIME by the book
// itself (`OrderBook::insert_order`) and persisted via the meta row's
// `next_seq` — a pure function of consensus history, identical on every
// validator. (This differs from the pre-3c save-time canonical-walk
// assignment: same determinism, different bytes — part of the batched 3c
// preimage round, never to be back-ported alone.)

/// Consensus-visible book persistence mode (see the warning above).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BookMode {
    /// Whole-book borsh blob per market (8-byte key) — exact-today default.
    Classic,
    /// C4 per-order rows in the root CF (`TORUS_BOOK_ROWS=1`).
    OrderRows,
    /// 3c level rows in the root CF + node-local order rows (`TORUS_BOOK_ROWS=2`).
    LevelAuthority,
}

impl BookMode {
    /// One-byte discriminant persisted in the `__book_mode__` marker row.
    fn marker_byte(self) -> u8 {
        match self {
            BookMode::Classic => 0,
            BookMode::OrderRows => 1,
            BookMode::LevelAuthority => 2,
        }
    }

    fn describe(self) -> &'static str {
        match self {
            BookMode::Classic => "classic (whole-book blobs)",
            BookMode::OrderRows => "order rows (TORUS_BOOK_ROWS=1)",
            BookMode::LevelAuthority => "level authority (TORUS_BOOK_ROWS=2)",
        }
    }
}

/// Runtime mode: `TORUS_BOOK_ROWS=1` → OrderRows, `=2` → LevelAuthority;
/// anything else (INCLUDING UNSET) → Classic — byte-identical state root to
/// today. Read once per process. Fleet-uniform, fresh genesis to change.
fn book_mode() -> BookMode {
    static MODE: std::sync::OnceLock<BookMode> = std::sync::OnceLock::new();
    *MODE.get_or_init(|| parse_book_rows_mode(std::env::var("TORUS_BOOK_ROWS").ok()))
}

/// Pure parse of the `TORUS_BOOK_ROWS` value (same trim/strictness as C4).
fn parse_book_rows_mode(v: Option<String>) -> BookMode {
    match v.as_deref().map(str::trim) {
        Some("1") => BookMode::OrderRows,
        Some("2") => BookMode::LevelAuthority,
        _ => BookMode::Classic,
    }
}

/// Default `TORUS_LEVEL_HASH_CACHE` budget in whole MB, applied when the env
/// var is unset or unparseable. `TORUS_LEVEL_HASH_CACHE=0` is the explicit
/// opt-out (restores the exact-today one-shot rehash).
///
/// WHY 256: it is the value the only in-vivo A/B actually ran
/// (`devnet/wsl/results/cacheab-18c-13bd833.md`, cap-400 bench-standard, 300 s
/// @ rate 750, one binary, env unset vs `256`), so shipping it on by default
/// ships the configuration that was measured rather than an extrapolation.
///
/// Memory: this is a GLOBAL ceiling, not a per-book one — `save_order_books`
/// derives the per-book budget as `level_hash_cache_bytes / order_books.len()`,
/// so a node with many markets gets a smaller slice per book, never more total
/// RAM. Each book converts its slice to an entry cap at
/// `torus_core::order_book::LEVEL_CACHE_ENTRY_COST` (512 B modelled per level:
/// keccak state ~200 B + block buffer ~136 B + metadata), and only DIRTY books
/// in the mode-2 save arm ever allocate one.
///
/// Blast radius: the cache engages ONLY under `BookMode::LevelAuthority`
/// (`TORUS_BOOK_ROWS=2`). Classic and OrderRows nodes `discard_level_ops` and
/// never reach the arm, so for them this default costs exactly zero bytes.
const DEFAULT_LEVEL_HASH_CACHE_MB: usize = 256;

/// L3 level-hash sponge cache budget (`TORUS_LEVEL_HASH_CACHE`, whole MB).
/// Unset / invalid = [`DEFAULT_LEVEL_HASH_CACHE_MB`] (ON); `0` = explicit
/// opt-out, restoring the exact-today one-shot rehash. NODE-LOCAL and
/// byte-identical either way (docs/design-levelhash-cache.md): the cache only
/// changes how the frozen mode-2 `level_hash` digest is computed in the
/// tail-append case — proven by `savebooks_levelcache_ab` and
/// `levelhash_cache_differential_matrix_byte_identical`, which compare
/// per-block book-CF bytes and state roots with the cache on vs off. Engages
/// exclusively in the mode-2 save arm. Read once per process; tests override
/// the ctx field directly.
///
/// SCOPE OF THE WIN (do not oversell): the win is on append-heavy / low-churn
/// levels. It CANNOT help match-touched levels — `OrderBook::match_at_level`
/// bumps `level_epoch` unconditionally on every taker touch, which forces the
/// Miss arm (a plain one-shot rehash plus a cheap probe insert).
fn level_hash_cache_env_bytes() -> usize {
    static BYTES: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *BYTES.get_or_init(|| {
        parse_level_hash_cache_mb(std::env::var("TORUS_LEVEL_HASH_CACHE").ok())
            .saturating_mul(1024 * 1024)
    })
}

/// Pure parse of the `TORUS_LEVEL_HASH_CACHE` value (MB; same trim rules as
/// the sibling toggles). An explicit `0` disables; anything unparseable falls
/// back to [`DEFAULT_LEVEL_HASH_CACHE_MB`], matching how `TORUS_BOOK_ROWS`
/// treats garbage (fall back to the default, never to a third behaviour).
fn parse_level_hash_cache_mb(v: Option<String>) -> usize {
    v.as_deref()
        .map(str::trim)
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(DEFAULT_LEVEL_HASH_CACHE_MB)
}

#[cfg(test)]
mod level_hash_cache_toggle_tests {
    use super::{parse_level_hash_cache_mb, DEFAULT_LEVEL_HASH_CACHE_MB};

    /// The shipped default is ON at the budget the in-vivo A/B measured
    /// (`devnet/wsl/results/cacheab-18c-13bd833.md` ran `=256`). If this
    /// constant is ever retuned, retune it against a fresh A/B, not by feel.
    #[test]
    fn default_budget_is_the_proven_256_mb() {
        assert_eq!(DEFAULT_LEVEL_HASH_CACHE_MB, 256);
    }

    /// Unset env ⇒ the cache is ON at the default budget (previously 0/OFF).
    #[test]
    fn unset_is_default_on() {
        let mb = parse_level_hash_cache_mb(None);
        assert_eq!(mb, DEFAULT_LEVEL_HASH_CACHE_MB);
        assert!(mb > 0, "default must be non-zero");
    }

    /// `TORUS_LEVEL_HASH_CACHE=0` remains the explicit opt-out — the ONLY way
    /// back to the exact-today one-shot rehash.
    #[test]
    fn explicit_zero_still_disables() {
        for v in ["0", " 0 ", "0\n", "\t0"] {
            assert_eq!(parse_level_hash_cache_mb(Some(v.to_string())), 0, "{v:?}");
        }
    }

    /// An explicit budget wins over the default, trimmed like the siblings.
    #[test]
    fn explicit_budget_is_honoured() {
        assert_eq!(parse_level_hash_cache_mb(Some("1".to_string())), 1);
        assert_eq!(parse_level_hash_cache_mb(Some(" 64 ".to_string())), 64);
        assert_eq!(parse_level_hash_cache_mb(Some("512".to_string())), 512);
    }

    /// Garbage falls back to the default (same policy as `TORUS_BOOK_ROWS`),
    /// never to a silent third behaviour.
    #[test]
    fn garbage_falls_back_to_default() {
        for v in ["", "on", "true", "-1", "256MB", "1.5", "yes"] {
            assert_eq!(
                parse_level_hash_cache_mb(Some(v.to_string())),
                DEFAULT_LEVEL_HASH_CACHE_MB,
                "{v:?}"
            );
        }
    }
}

// Frozen key layouts — single source of truth in torus-core.
use torus_core::book_rows::{
    book_meta_key, book_order_key, book_stop_key, level_row_key_tagged, LevelRowData,
    ROW_TAG_LEVEL, ROW_TAG_META, ROW_TAG_ORDER, ROW_TAG_STOP,
};

/// Parsed meta row — the codec itself lives in `torus_core::book_rows` so the
/// save path here and every READ path (RPC, precompiles) share one source of
/// truth for these bytes.
type BookMeta = torus_core::book_rows::BookMetaRow;

/// Meta row value — layout unchanged from C4 (`next_seq` now sourced from the
/// book's own allocator, journal-in-book).
fn book_meta_value(book: &OrderBook) -> Vec<u8> {
    BookMeta {
        next_seq: book.next_seq(),
        tick_size: book.tick_size,
        lot_size: book.lot_size,
        next_id: book.next_order_id(),
        last_trade_price: book.last_trade_price(),
    }
    .encode()
}

fn parse_book_meta(v: &[u8]) -> Result<BookMeta, String> {
    BookMeta::decode(v)
}

fn parse_book_order_row(v: &[u8]) -> Result<(u64, torus_core::order_book::Order), String> {
    use borsh::BorshDeserialize;
    if v.len() < 8 {
        return Err("book order row truncated".to_string());
    }
    let seq = u64::from_be_bytes(v[0..8].try_into().unwrap());
    let order = torus_core::order_book::Order::try_from_slice(&v[8..])
        .map_err(|e| format!("book order row borsh: {e}"))?;
    Ok((seq, order))
}

#[cfg(test)]
mod book_rows_toggle_tests {
    use super::{parse_book_rows_mode, BookMode};

    #[test]
    fn default_is_classic() {
        assert_eq!(parse_book_rows_mode(None), BookMode::Classic);
    }

    #[test]
    fn one_is_order_rows_two_is_level_authority() {
        assert_eq!(parse_book_rows_mode(Some("1".to_string())), BookMode::OrderRows);
        assert_eq!(parse_book_rows_mode(Some(" 1 ".to_string())), BookMode::OrderRows);
        assert_eq!(
            parse_book_rows_mode(Some("2".to_string())),
            BookMode::LevelAuthority
        );
        assert_eq!(
            parse_book_rows_mode(Some(" 2 ".to_string())),
            BookMode::LevelAuthority
        );
    }

    #[test]
    fn anything_else_stays_classic() {
        for v in ["0", "true", "on", "", "yes", "3", "12", "level"] {
            assert_eq!(parse_book_rows_mode(Some(v.to_string())), BookMode::Classic, "{v}");
        }
    }
}

// ============================================================================
// rank8 — resident order books (`TORUS_RESIDENT_BOOKS`)
// ============================================================================
//
// Classic lifecycle: a fresh NativeExecContext per block RELOADS every order
// book from `CF_NATIVE_ORDER_BOOKS` (full deserialization / row rebuild of
// every resting order, every block) and the row-store save re-walks every
// dirty book. At ~700k resting orders the reload alone dominates block time
// (early-proof cell B: 6.7s → 9.5s avg block under rows mode).
//
// `TORUS_RESIDENT_BOOKS=1`: books (plus row shadows and the order-id
// high-water mark) survive across blocks in a `ResidentBooks` holder owned by
// the execution pipeline. Each block's context TAKES the state at
// construction and STASHES it back after a successful save; startup/restart
// rebuilds once from persisted state (whichever persistence mode). Combined
// with `TORUS_BOOK_ROWS=1`, saves are driven by the books' mutation journal —
// O(changed orders) with NO whole-book walk.
//
// NOT consensus-visible BY DESIGN: resident mode must produce byte-identical
// CF writes and state roots to the reload path (the journal-driven differ
// assigns queue seqs in exactly the canonical order the full walk does — see
// `save_book_rows_journaled`). The flag is therefore per-node and safe to mix
// across a fleet; the differential tests pin root identity in all four
// resident×rows combinations.
//
// STALENESS GUARD (restart / replay / fork defense): the holder remembers the
// height its state reflects. A block-H context reuses it ONLY if
// `holder.height + 1 == H` AND (when the node has the applied-height marker
// infra) the DB's `META_NATIVE_APPLIED_HEIGHT` equals `holder.height` — i.e.
// memory is exactly the DB's post-state. Any mismatch (crashed flush, skipped
// or replayed height, out-of-band DB progress, anything reorg-shaped) falls
// back to a full rebuild from persisted state, which is always authoritative.
// A fatal block (`ctx.fatal_error`) never stashes — the holder stays drained,
// forcing a rebuild.

/// rank8 runtime toggle: `TORUS_RESIDENT_BOOKS=1` enables resident books;
/// anything else (INCLUDING UNSET) keeps the per-block reload — exact-today.
fn resident_books_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| parse_resident_books_toggle(std::env::var("TORUS_RESIDENT_BOOKS").ok()))
}

/// Pure parse of the `TORUS_RESIDENT_BOOKS` value: only `"1"` enables.
fn parse_resident_books_toggle(v: Option<String>) -> bool {
    matches!(v.as_deref().map(str::trim), Some("1"))
}

/// rank8: cross-block resident book state. Owned by the execution pipeline
/// (one per node, behind a Mutex on the exec thread); handed to each block's
/// context via [`NativeExecContext::new_with_modes`] and refilled via
/// [`NativeExecContext::stash_resident`]. Empty/invalidated ⇒ the next
/// context rebuilds from persisted state.
#[derive(Default)]
pub struct ResidentBooks {
    inner: Option<ResidentInner>,
}

struct ResidentInner {
    books: HashMap<MarketId, OrderBook>,
    /// Post-block global order-id high-water mark (replaces the load scan).
    next_global_order_id: u128,
    /// The block height whose POST-state this reflects (staleness guard).
    height: u64,
}

impl ResidentBooks {
    /// Drop any resident state — the next context rebuilds from the DB.
    pub fn invalidate(&mut self) {
        self.inner = None;
    }

    /// Whether the holder currently carries state (test/ops introspection).
    pub fn is_populated(&self) -> bool {
        self.inner.is_some()
    }
}

#[cfg(test)]
mod resident_books_toggle_tests {
    use super::parse_resident_books_toggle;

    #[test]
    fn default_is_off() {
        assert!(!parse_resident_books_toggle(None));
    }

    #[test]
    fn one_enables() {
        assert!(parse_resident_books_toggle(Some("1".to_string())));
        assert!(parse_resident_books_toggle(Some(" 1 ".to_string())));
    }

    #[test]
    fn anything_else_stays_off() {
        for v in ["0", "true", "on", "", "yes", "2"] {
            assert!(!parse_resident_books_toggle(Some(v.to_string())), "{v}");
        }
    }
}

pub struct NativeExecContext<T: StateBackend = StateDb> {
    pub positions: PositionManager<T>,
    pub oracle: OracleManager<T>,
    pub staking: StakingManager<T>,
    pub governance: GovernanceManager<T>,
    pub state: T,

    /// Order books (per market, in-memory).
    pub order_books: HashMap<MarketId, OrderBook>,
    /// Markets whose book was mutated during THIS block (placed / matched /
    /// cancelled / modified). `save_order_books` writes only these — untouched
    /// books already hold identical bytes in the CF, so skipping them is
    /// byte-identical in final state (state-root-safe even in mixed
    /// deployments) and turns the old O(all resting orders) rewrite into
    /// O(touched) (S395).
    pub dirty_books: std::collections::HashSet<MarketId>,
    /// Per-market margin configuration.
    pub margin_configs: HashMap<MarketId, MarketMarginConfig>,
    /// FIX 6 (ECON-FIND-09): Global order ID counter shared across all markets.
    pub next_global_order_id: u128,
    /// Counter value at load time — the counter row is persisted only when it
    /// advanced this block (or was never stored), so no-order blocks write nothing.
    loaded_next_global_order_id: Option<u128>,

    // Block metadata
    pub block_height: u64,
    pub timestamp: u64,
    pub epoch: u64,
    pub epoch_length: u64,
    pub max_validators: u32,
    pub proposer: Address,
    pub treasury_address: Address,
    pub dev_pool_address: Address,

    /// Accumulated native fees during this block.
    pub total_native_fees: u64,
    /// Per-block trade counter for unique trade keys.
    pub trade_index: u32,

    /// O3: when set, trade-history writes (`CF_NATIVE_TRADES` /
    /// `CF_NATIVE_USER_TRADES` — node-local CFs outside the native consensus
    /// root, never read during execution) are buffered in `pending_trades`
    /// instead of PUT into the state backend, for the caller to hand to a
    /// background writer after all exec phases. Off by default: every existing
    /// caller keeps inline writes.
    pub defer_trades: bool,
    /// Raw trade-history KVs buffered while `defer_trades` is set.
    pending_trades: Vec<RawCfKv>,

    /// Optional metrics handle for Prometheus instrumentation.
    pub metrics: Option<std::sync::Arc<torus_telemetry::Metrics>>,

    /// T1.5: set (never cleared) when a market worker panicked mid-matching.
    /// The panicking worker consumed its market's `OrderBook`, so this block's
    /// post-state is unreconstructable — the committer MUST treat this as
    /// fatal (fail-stop the node), never flush state or mark the block applied.
    pub fatal_error: Option<String>,

    /// Book persistence mode (`TORUS_BOOK_ROWS`). CONSENSUS-VISIBLE — see the
    /// module-level schema comment: fleet-uniform, fresh genesis required,
    /// mixed on-disk content fail-stops at load.
    book_mode: BookMode,
    /// Whether the node-local `__book_mode__` marker row already exists (and
    /// matched); when false, the first save writes it.
    book_mode_marker_present: bool,
    /// rank8: this context participates in resident-book handoff (was
    /// constructed with a holder). NOT consensus-visible — resident mode
    /// must be byte-identical to the reload path.
    resident: bool,
    /// rank8: whether resident state was actually reused this block (vs a
    /// rebuild from persisted state — first block, restart, or stale guard).
    resident_reused: bool,
    /// L3: level-hash sponge cache budget in BYTES for mode-2 saves
    /// (`TORUS_LEVEL_HASH_CACHE` env, MB; default
    /// [`DEFAULT_LEVEL_HASH_CACHE_MB`] = ON). 0 = disabled (exact-today),
    /// reachable only via an explicit `TORUS_LEVEL_HASH_CACHE=0`.
    /// Node-local, byte-identical output; tests may override per ctx.
    pub level_hash_cache_bytes: usize,
    /// L3 save-books attribution (µbench-only, feature `save-timings`): when
    /// set, the mode-2 `save_order_books` arm accumulates per-sub-step
    /// timings into `last_save_timings`. Compiled out of production builds.
    #[cfg(feature = "save-timings")]
    pub collect_save_timings: bool,
    /// L3 save-books attribution (µbench-only): last block's sub-step timings.
    #[cfg(feature = "save-timings")]
    pub last_save_timings: SaveTimings,
}

/// L3 save-books attribution (µbench-only): breakdown of the mode-2
/// `save_order_books` span into its four per-market sub-steps. The struct is
/// always compiled (the accumulator code is statically dead when the
/// `save-timings` feature is off); the ctx fields and collection flag exist
/// only under the feature.
#[derive(Default, Clone, Copy, Debug)]
pub struct SaveTimings {
    /// take_row_ops drain + encode_order_row + CF_BOOK_ORDER_ROWS put/delete.
    pub rows_ns: u128,
    /// take_level_ops drain + level_row_data keccak + CF level put/delete.
    pub levels_ns: u128,
    /// diff_stop_rows — the per-market RocksDB prefix seek over stop rows.
    pub stops_ns: u128,
    /// write_meta_if_moved — the per-market RocksDB meta point read + compare.
    pub meta_ns: u128,
    /// counter row + book-mode marker (once) + loop overhead.
    pub other_ns: u128,
    /// number of dirty markets saved this block.
    pub markets: u32,
}

impl<T: StateBackend> NativeExecContext<T> {
    /// Create a new execution context from a state backend and block metadata.
    /// Book persistence mode comes from the `TORUS_BOOK_ROWS` env toggle
    /// (default OFF = classic whole-book blobs).
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        state: T,
        block_height: u64,
        timestamp: u64,
        epoch: u64,
        epoch_length: u64,
        max_validators: u32,
        proposer: Address,
        treasury_address: Address,
        dev_pool_address: Address,
    ) -> Self {
        Self::new_with_mode(
            state,
            block_height,
            timestamp,
            epoch,
            epoch_length,
            max_validators,
            proposer,
            treasury_address,
            dev_pool_address,
            book_mode(),
            None,
        )
    }

    /// [`Self::new`] with the book-persistence mode pinned as a bool (legacy
    /// C4 test/tooling shim): `false` = Classic, `true` = OrderRows. New code
    /// (and anything exercising mode 2) uses [`Self::new_with_mode`].
    #[allow(clippy::too_many_arguments)]
    pub fn new_with_book_rows(
        state: T,
        block_height: u64,
        timestamp: u64,
        epoch: u64,
        epoch_length: u64,
        max_validators: u32,
        proposer: Address,
        treasury_address: Address,
        dev_pool_address: Address,
        book_rows: bool,
    ) -> Self {
        Self::new_with_mode(
            state,
            block_height,
            timestamp,
            epoch,
            epoch_length,
            max_validators,
            proposer,
            treasury_address,
            dev_pool_address,
            if book_rows { BookMode::OrderRows } else { BookMode::Classic },
            None,
        )
    }

    /// rank8 live-node entry: every mode from env (`TORUS_BOOK_ROWS`,
    /// `TORUS_RESIDENT_BOOKS`). The caller owns the cross-block holder; with
    /// `TORUS_RESIDENT_BOOKS` unset this is exactly [`Self::new`] and the
    /// holder is never touched.
    #[allow(clippy::too_many_arguments)]
    pub fn new_env(
        state: T,
        block_height: u64,
        timestamp: u64,
        epoch: u64,
        epoch_length: u64,
        max_validators: u32,
        proposer: Address,
        treasury_address: Address,
        dev_pool_address: Address,
        resident: &mut ResidentBooks,
    ) -> Self {
        let use_resident = resident_books_enabled();
        Self::new_with_mode(
            state,
            block_height,
            timestamp,
            epoch,
            epoch_length,
            max_validators,
            proposer,
            treasury_address,
            dev_pool_address,
            book_mode(),
            if use_resident { Some(resident) } else { None },
        )
    }

    /// Legacy bool shim for [`Self::new_with_mode`] (`false` = Classic,
    /// `true` = OrderRows) — existing C4/rank8 tests.
    #[allow(clippy::too_many_arguments)]
    pub fn new_with_modes(
        state: T,
        block_height: u64,
        timestamp: u64,
        epoch: u64,
        epoch_length: u64,
        max_validators: u32,
        proposer: Address,
        treasury_address: Address,
        dev_pool_address: Address,
        book_rows: bool,
        resident: Option<&mut ResidentBooks>,
    ) -> Self {
        Self::new_with_mode(
            state,
            block_height,
            timestamp,
            epoch,
            epoch_length,
            max_validators,
            proposer,
            treasury_address,
            dev_pool_address,
            if book_rows { BookMode::OrderRows } else { BookMode::Classic },
            resident,
        )
    }

    /// Full-control constructor: book persistence mode + optional rank8
    /// resident-book holder, both pinned explicitly.
    ///
    /// With `resident: Some(holder)`, the holder's state is TAKEN and reused
    /// iff the staleness guard passes (`holder.height + 1 == block_height`,
    /// and the DB's applied-height marker — when present — equals
    /// `holder.height`); otherwise it is dropped and books rebuild from
    /// persisted state exactly like the non-resident path. Give the state
    /// back with [`Self::stash_resident`] after `save_order_books`.
    #[allow(clippy::too_many_arguments)]
    pub fn new_with_mode(
        state: T,
        block_height: u64,
        timestamp: u64,
        epoch: u64,
        epoch_length: u64,
        max_validators: u32,
        proposer: Address,
        treasury_address: Address,
        dev_pool_address: Address,
        book_mode: BookMode,
        resident: Option<&mut ResidentBooks>,
    ) -> Self {
        let positions = PositionManager::new(state.clone());
        let oracle = OracleManager::new(state.clone(), OracleConfig::default());
        let staking = StakingManager::new(state.clone());
        let governance = GovernanceManager::new(state.clone());

        // rank8: try to reuse resident state. TAKE unconditionally — on any
        // guard failure the memory state is dropped and the DB (authoritative)
        // is re-read, so a stale holder can never leak into execution.
        let resident_mode = resident.is_some();
        let mut reused_inner: Option<ResidentInner> = None;
        if let Some(holder) = resident {
            if let Some(inner) = holder.inner.take() {
                let marker_ok = match Self::read_applied_marker(&state) {
                    Some(applied) => applied == inner.height,
                    // No marker row (test envs / pre-marker DBs): the height
                    // sequence check below is the only guard available.
                    None => true,
                };
                if inner.height + 1 == block_height && marker_ok {
                    reused_inner = Some(inner);
                } else {
                    tracing::warn!(
                        resident_height = inner.height,
                        block_height,
                        "rank8: resident books stale (height/marker mismatch) — rebuilding from persisted state"
                    );
                }
            }
        }
        let resident_reused = reused_inner.is_some();

        // FIX 1 (ECON-FIND-02): Load persisted order books from DB on startup.
        // The on-disk layout must match the configured mode — a mismatch
        // means this node's flag disagrees with the DB's history. Loading
        // "what we can" would silently diverge from the fleet, so latch a
        // fatal instead: the committer fail-stops before flushing anything.
        let (order_books, scanned_next_id, mut load_error) = match reused_inner {
            Some(inner) => (inner.books, inner.next_global_order_id, None),
            None => match book_mode {
                BookMode::Classic => Self::load_order_books(&state),
                BookMode::OrderRows => Self::load_order_books_rows(&state),
                BookMode::LevelAuthority => Self::load_order_books_levels(&state),
            },
        };

        // 3c robustness marker: `__book_mode__` (node-local, non-root) catches
        // wrong-flag restarts even on chains whose books are still empty
        // (content sniffing has nothing to sniff there). Checked regardless of
        // resident reuse (one point read).
        let book_mode_marker_present = match Self::load_book_mode_marker(&state) {
            Some(byte) if byte == book_mode.marker_byte() => true,
            Some(byte) => {
                if load_error.is_none() {
                    load_error = Some(format!(
                        "book-mode marker mismatch: DB was written under mode {byte} but \
                         this node runs {} — TORUS_BOOK_ROWS must match the DB's history \
                         (fleet-uniform; changing it needs a fresh genesis)",
                        book_mode.describe()
                    ));
                }
                true
            }
            None => false,
        };

        // S395: the durable counter row is authoritative when present — the
        // book-maxima scan resets to 1 once all books drain, silently reusing
        // order ids across a restart. max() keeps back-compat with DBs written
        // before the counter row existed. (Resident reuse feeds the carried
        // high-water mark through the same max.)
        let persisted_next_id = Self::load_next_global_order_id(&state);
        let next_global_order_id = scanned_next_id.max(persisted_next_id.unwrap_or(1));

        Self {
            positions,
            oracle,
            staking,
            governance,
            state,
            order_books,
            dirty_books: std::collections::HashSet::new(),
            margin_configs: HashMap::new(),
            next_global_order_id,
            loaded_next_global_order_id: persisted_next_id,
            block_height,
            timestamp,
            epoch,
            epoch_length,
            max_validators,
            proposer,
            treasury_address,
            dev_pool_address,
            total_native_fees: 0,
            trade_index: 0,
            defer_trades: false,
            pending_trades: Vec::new(),
            metrics: None,
            fatal_error: load_error,
            book_mode,
            book_mode_marker_present,
            resident: resident_mode,
            resident_reused,
            level_hash_cache_bytes: level_hash_cache_env_bytes(),
            #[cfg(feature = "save-timings")]
            collect_save_timings: false,
            #[cfg(feature = "save-timings")]
            last_save_timings: SaveTimings::default(),
        }
    }

    /// rank8: whether this context reused the holder's resident state (vs
    /// rebuilding from persisted). Test/ops introspection.
    pub fn resident_reused(&self) -> bool {
        self.resident_reused
    }

    /// rank8: hand the books (their journals ride inside, journal-in-book)
    /// and the order-id high-water mark back to the cross-block holder. Call
    /// AFTER `save_order_books`. No-op for non-resident contexts. A context
    /// that latched `fatal_error` INVALIDATES the holder instead — its
    /// in-memory state may not match what was (not) persisted, so the next
    /// block must rebuild from the DB.
    pub fn stash_resident(&mut self, resident: &mut ResidentBooks) {
        if !self.resident {
            return;
        }
        if self.fatal_error.is_some() {
            resident.invalidate();
            return;
        }
        resident.inner = Some(ResidentInner {
            books: std::mem::take(&mut self.order_books),
            next_global_order_id: self.next_global_order_id,
            height: self.block_height,
        });
    }

    /// rank8 staleness guard input: the DB's native applied-height marker.
    fn read_applied_marker(state: &T) -> Option<u64> {
        use torus_state::cf::{CF_CONSENSUS_META, META_NATIVE_APPLIED_HEIGHT};
        let bytes = state
            .get_cf_raw(CF_CONSENSUS_META, META_NATIVE_APPLIED_HEIGHT)
            .ok()
            .flatten()?;
        Some(u64::from_be_bytes(bytes.try_into().ok()?))
    }

    /// Drain the trade-history KVs buffered under `defer_trades`. The caller
    /// owns durability from here (background writer, or synchronous fallback).
    pub fn take_pending_trades(&mut self) -> Vec<RawCfKv> {
        std::mem::take(&mut self.pending_trades)
    }

    /// PROFILER (s470): total resting orders across every loaded book. Sampled
    /// once per block (after the constructor's load/rebuild) as the book-depth
    /// axis for the depth-vs-cost correlation.
    pub fn resting_order_count(&self) -> usize {
        self.order_books.values().map(|b| b.order_count()).sum()
    }

    /// L3 metrics hook: fold the per-book level-hash sponge cache stats across
    /// every loaded book into `(hits, misses, seeds, live entries)`. `hits` /
    /// `misses` / `seeds` are cumulative sums (monotonic while books persist as
    /// resident); `entries` is the instantaneous resident-sponge count. Returns
    /// `None` when the cache is disabled (no book reports stats), so the export
    /// site skips the metric update entirely — zero cost with
    /// `TORUS_LEVEL_HASH_CACHE` off. Mirrors `OrderBook::level_hash_cache_stats`.
    pub fn level_hash_cache_stats(&self) -> Option<(u64, u64, u64, u64)> {
        let mut any = false;
        let mut agg = (0u64, 0u64, 0u64, 0u64);
        for book in self.order_books.values() {
            if let Some((hits, misses, seeds, entries)) = book.level_hash_cache_stats() {
                any = true;
                agg.0 += hits;
                agg.1 += misses;
                agg.2 += seeds;
                agg.3 += entries as u64;
            }
        }
        any.then_some(agg)
    }

    /// True if `key` belongs to the tagged row schema (meta / order / stop /
    /// level row — modes 1/2).
    fn is_book_row_key(key: &[u8]) -> bool {
        (key.len() == 9 && key[8] == ROW_TAG_META)
            || (key.len() == 25 && (key[8] == ROW_TAG_ORDER || key[8] == ROW_TAG_STOP))
            || (key.len() == 26 && key[8] == ROW_TAG_LEVEL)
    }

    /// FIX 1 (ECON-FIND-02): Load order books from DB (classic whole-book
    /// blobs). Returns (books, next_global_order_id, fatal load error).
    /// Finding tagged row-schema keys here (9/25/26 bytes) means the DB was
    /// written under `TORUS_BOOK_ROWS=1/2` but this node runs without it —
    /// fatal (the classic loader would silently see empty books and diverge
    /// from the fleet).
    fn load_order_books(state: &T) -> (HashMap<MarketId, OrderBook>, u128, Option<String>) {
        use borsh::BorshDeserialize;
        use torus_state::cf::CF_NATIVE_ORDER_BOOKS;

        let mut books = HashMap::new();
        let mut max_order_id: u128 = 0;

        if let Ok(entries) = state.iterate_cf(CF_NATIVE_ORDER_BOOKS, None) {
            for (key, value) in entries {
                if Self::is_book_row_key(&key) {
                    return (
                        HashMap::new(),
                        1,
                        Some(
                            "C4: cf_native_order_books holds per-order/level rows but \
                             TORUS_BOOK_ROWS is not set — the flag must match the \
                             DB's history (fleet-uniform; changing it needs a fresh \
                             genesis)"
                                .to_string(),
                        ),
                    );
                }
                if key.len() == 8 {
                    let market_id = u64::from_be_bytes(key[..8].try_into().unwrap());
                    if let Ok(book) = OrderBook::try_from_slice(&value) {
                        let book_next_id = book.next_order_id();
                        if book_next_id > max_order_id {
                            max_order_id = book_next_id;
                        }
                        books.insert(market_id, book);
                    }
                }
            }
        }

        // Global ID starts at max found + 1 (or 1 if no books loaded)
        let next_id = if max_order_id > 0 { max_order_id } else { 1 };
        (books, next_id, None)
    }

    /// Rebuild one market's book from parsed meta + `(seq, order)` rows +
    /// stop rows, in the CANONICAL insertion order the classic deserializer
    /// uses — bids ascending (price, seq), then asks ascending (price, seq);
    /// stops ascending id. Shared by the mode-1 and mode-2 loaders.
    fn rebuild_book(
        market_id: MarketId,
        meta: BookMeta,
        mut orders: Vec<(u64, torus_core::order_book::Order)>,
        mut stops: Vec<(u128, Vec<u8>)>,
    ) -> Result<OrderBook, String> {
        let mut book = OrderBook::new(market_id, meta.tick_size, meta.lot_size);
        book.set_next_order_id(meta.next_id);
        book.set_last_trade_price(meta.last_trade_price);
        book.set_next_seq(meta.next_seq);

        for (seq, _) in &orders {
            if *seq >= meta.next_seq {
                return Err(format!(
                    "market {market_id}: order row seq {seq} >= meta next_seq {} \
                     (corrupt row store)",
                    meta.next_seq
                ));
            }
        }
        orders.sort_by(|a, b| {
            let rank = |o: &torus_core::order_book::Order| u8::from(o.side == Side::Sell);
            (rank(&a.1), a.1.price, a.0).cmp(&(rank(&b.1), b.1.price, b.0))
        });
        for (seq, order) in orders {
            book.insert_loaded_order(order, seq);
        }

        stops.sort_by_key(|(id, _)| *id);
        for (stop_id, bytes) in stops {
            match book.restore_stop_row(&bytes) {
                Ok(id) if id == stop_id => {}
                Ok(id) => {
                    return Err(format!(
                        "market {market_id}: stop row key id {stop_id} != payload id \
                         {id} (corrupt row store)"
                    ))
                }
                Err(e) => return Err(format!("market {market_id}: {e}")),
            }
        }
        Ok(book)
    }

    /// C4 / mode 1: load order books from per-order ROOT-CF rows
    /// (`TORUS_BOOK_ROWS=1`). Journal-in-book: seqs restore straight into the
    /// book (no shadows). Returns (books, next_global_order_id, fatal error).
    fn load_order_books_rows(
        state: &T,
    ) -> (HashMap<MarketId, OrderBook>, u128, Option<String>) {
        use torus_state::cf::CF_NATIVE_ORDER_BOOKS;

        let fail = |msg: String| (HashMap::new(), 1, Some(msg));

        // Per-market accumulators.
        #[derive(Default)]
        struct Acc {
            meta: Option<BookMeta>,
            orders: Vec<(u64, torus_core::order_book::Order)>,
            stops: Vec<(u128, Vec<u8>)>,
        }
        fn acc(m: &mut HashMap<MarketId, Acc>, id: MarketId) -> &mut Acc {
            m.entry(id).or_default()
        }
        let mut accs: HashMap<MarketId, Acc> = HashMap::new();

        let entries = match state.iterate_cf(CF_NATIVE_ORDER_BOOKS, None) {
            Ok(e) => e,
            Err(e) => return fail(format!("C4: book row scan failed: {e}")),
        };
        for (key, value) in entries {
            if key.len() == 8 {
                return fail(
                    "C4: TORUS_BOOK_ROWS=1 but cf_native_order_books holds classic \
                     whole-book blobs — the flag must match the DB's history \
                     (fleet-uniform; enabling it needs a fresh genesis)"
                        .to_string(),
                );
            }
            if key.len() == 26 && key[8] == ROW_TAG_LEVEL {
                return fail(
                    "C4: TORUS_BOOK_ROWS=1 but cf_native_order_books holds level rows \
                     — the DB was written under TORUS_BOOK_ROWS=2 (fleet-uniform; \
                     changing the mode needs a fresh genesis)"
                        .to_string(),
                );
            }
            if !Self::is_book_row_key(&key) {
                return fail(format!(
                    "C4: unrecognized cf_native_order_books key (len {}) under \
                     TORUS_BOOK_ROWS=1",
                    key.len()
                ));
            }
            let market_id = u64::from_be_bytes(key[..8].try_into().unwrap());
            match key[8] {
                ROW_TAG_META => match parse_book_meta(&value) {
                    Ok(m) => acc(&mut accs, market_id).meta = Some(m),
                    Err(e) => return fail(format!("C4: market {market_id}: {e}")),
                },
                ROW_TAG_ORDER => match parse_book_order_row(&value) {
                    Ok(row) => acc(&mut accs, market_id).orders.push(row),
                    Err(e) => return fail(format!("C4: market {market_id}: {e}")),
                },
                ROW_TAG_STOP => {
                    let stop_id = u128::from_be_bytes(key[9..25].try_into().unwrap());
                    acc(&mut accs, market_id).stops.push((stop_id, value));
                }
                _ => unreachable!("is_book_row_key checked the tag"),
            }
        }

        let mut books = HashMap::new();
        let mut max_order_id: u128 = 0;

        // Deterministic rebuild order (market id ascending).
        let mut market_ids: Vec<MarketId> = accs.keys().copied().collect();
        market_ids.sort_unstable();

        for market_id in market_ids {
            let a = accs.remove(&market_id).unwrap();
            let Some(meta) = a.meta else {
                return fail(format!(
                    "C4: market {market_id} has order/stop rows but no meta row \
                     (corrupt row store)"
                ));
            };
            let book = match Self::rebuild_book(market_id, meta, a.orders, a.stops) {
                Ok(b) => b,
                Err(e) => return fail(format!("C4: {e}")),
            };
            let book_next_id = book.next_order_id();
            if book_next_id > max_order_id {
                max_order_id = book_next_id;
            }
            books.insert(market_id, book);
        }

        let next_id = if max_order_id > 0 { max_order_id } else { 1 };
        (books, next_id, None)
    }

    /// 3c / mode 2: load order books under LEVEL AUTHORITY
    /// (`TORUS_BOOK_ROWS=2`). Root CF holds meta + stop + level rows ONLY;
    /// full order rows live in the node-local `CF_BOOK_ORDER_ROWS`. Rebuilds
    /// every book from the node-local store, then BYTE-VERIFIES the
    /// root-committed meta/stop/level rows (incl. every `level_hash`) against
    /// the rebuilt books — any mismatch means the node-local store is
    /// corrupt/stale and the node must not serve or sign (fatal).
    /// Returns (books, next_global_order_id, fatal error).
    fn load_order_books_levels(
        state: &T,
    ) -> (HashMap<MarketId, OrderBook>, u128, Option<String>) {
        use torus_state::cf::{CF_BOOK_ORDER_ROWS, CF_NATIVE_ORDER_BOOKS};

        let fail = |msg: String| (HashMap::new(), 1, Some(msg));

        // ---- Root CF scan: meta + stops + level rows (consensus) ----
        #[derive(Default)]
        struct RootAcc {
            meta: Option<(BookMeta, Vec<u8>)>,
            stops: Vec<(u128, Vec<u8>)>,
            /// level key (26 B) -> stored 52 B value
            levels: std::collections::BTreeMap<Vec<u8>, Vec<u8>>,
        }
        let mut roots: HashMap<MarketId, RootAcc> = HashMap::new();

        let entries = match state.iterate_cf(CF_NATIVE_ORDER_BOOKS, None) {
            Ok(e) => e,
            Err(e) => return fail(format!("3c: book row scan failed: {e}")),
        };
        for (key, value) in entries {
            if key.len() == 8 {
                return fail(
                    "3c: TORUS_BOOK_ROWS=2 but cf_native_order_books holds classic \
                     whole-book blobs — the flag must match the DB's history \
                     (fleet-uniform; enabling it needs a fresh genesis)"
                        .to_string(),
                );
            }
            if key.len() == 25 && key[8] == ROW_TAG_ORDER {
                return fail(
                    "3c: TORUS_BOOK_ROWS=2 but cf_native_order_books holds per-order \
                     rows — the DB was written under TORUS_BOOK_ROWS=1 (fleet-uniform; \
                     changing the mode needs a fresh genesis)"
                        .to_string(),
                );
            }
            if !Self::is_book_row_key(&key) {
                return fail(format!(
                    "3c: unrecognized cf_native_order_books key (len {}) under \
                     TORUS_BOOK_ROWS=2",
                    key.len()
                ));
            }
            let market_id = u64::from_be_bytes(key[..8].try_into().unwrap());
            let acc = roots.entry(market_id).or_default();
            match key[8] {
                ROW_TAG_META => match parse_book_meta(&value) {
                    Ok(m) => acc.meta = Some((m, value)),
                    Err(e) => return fail(format!("3c: market {market_id}: {e}")),
                },
                ROW_TAG_STOP => {
                    let stop_id = u128::from_be_bytes(key[9..25].try_into().unwrap());
                    acc.stops.push((stop_id, value));
                }
                ROW_TAG_LEVEL => {
                    if value.len() != torus_core::book_rows::LEVEL_ROW_VALUE_LEN {
                        return fail(format!(
                            "3c: market {market_id}: level row value len {} != {} \
                             (corrupt level row)",
                            value.len(),
                            torus_core::book_rows::LEVEL_ROW_VALUE_LEN
                        ));
                    }
                    acc.levels.insert(key, value);
                }
                _ => unreachable!("is_book_row_key checked the tag"),
            }
        }

        // ---- Node-local order-row store scan ----
        let mut store_orders: HashMap<MarketId, Vec<(u64, torus_core::order_book::Order)>> =
            HashMap::new();
        let store_entries = match state.iterate_cf(CF_BOOK_ORDER_ROWS, None) {
            Ok(e) => e,
            Err(e) => return fail(format!("3c: order-row store scan failed: {e}")),
        };
        let store_nonempty = !store_entries.is_empty();
        for (key, value) in store_entries {
            if key.len() != 25 || key[8] != ROW_TAG_ORDER {
                return fail(format!(
                    "3c: unrecognized cf_book_order_rows key (len {}) — corrupt \
                     node-local order store",
                    key.len()
                ));
            }
            let market_id = u64::from_be_bytes(key[..8].try_into().unwrap());
            let order_id = u128::from_be_bytes(key[9..25].try_into().unwrap());
            match parse_book_order_row(&value) {
                Ok((seq, order)) => {
                    if order.id != order_id {
                        return fail(format!(
                            "3c: market {market_id}: order row key id {order_id} != \
                             payload id {} (corrupt node-local order store)",
                            order.id
                        ));
                    }
                    store_orders.entry(market_id).or_default().push((seq, order));
                }
                Err(e) => return fail(format!("3c: market {market_id}: {e}")),
            }
        }

        // Split-brain: an order store with content while the root CF commits
        // no books at all.
        if store_nonempty && roots.is_empty() {
            return fail(
                "3c: cf_book_order_rows is non-empty but cf_native_order_books has \
                 no meta/level rows — split-brain between the node-local order \
                 store and the consensus root (corrupt DB)"
                    .to_string(),
            );
        }

        // ---- Rebuild + boot verification ----
        let mut books = HashMap::new();
        let mut max_order_id: u128 = 0;

        // Orders for a market that has no root presence at all: fatal.
        for mid in store_orders.keys() {
            if !roots.contains_key(mid) {
                return fail(format!(
                    "3c: market {mid} has node-local order rows but no root-CF rows \
                     (split-brain store)"
                ));
            }
        }

        let mut market_ids: Vec<MarketId> = roots.keys().copied().collect();
        market_ids.sort_unstable();

        for market_id in market_ids {
            let acc = roots.remove(&market_id).unwrap();
            let Some((meta, stored_meta_bytes)) = acc.meta else {
                return fail(format!(
                    "3c: market {market_id} has stop/level rows but no meta row \
                     (corrupt row store)"
                ));
            };
            let orders = store_orders.remove(&market_id).unwrap_or_default();
            let stop_bytes = acc.stops.clone();
            let book = match Self::rebuild_book(market_id, meta, orders, acc.stops) {
                Ok(b) => b,
                Err(e) => return fail(format!("3c: {e}")),
            };

            // -- Boot verify 1: meta row bytes --
            let recomputed_meta = book_meta_value(&book);
            if recomputed_meta != stored_meta_bytes {
                return fail(format!(
                    "3c: market {market_id}: recomputed meta row != root-committed \
                     meta row (node-local order store corrupt/stale — refusing to \
                     serve or sign)"
                ));
            }

            // -- Boot verify 2: stop rows (content-checked by rebuild; the set
            //    is exactly what the root CF holds by construction, so only the
            //    id/content integrity check in rebuild_book applies). Recompute
            //    the serialized set for byte parity anyway (cheap).
            let live_stops: Vec<(u128, Vec<u8>)> = book.stop_rows();
            let mut stored_stops = stop_bytes;
            stored_stops.sort_by_key(|(id, _)| *id);
            if live_stops != stored_stops {
                return fail(format!(
                    "3c: market {market_id}: rebuilt stop set != root-committed stop \
                     rows (corrupt row store)"
                ));
            }

            // -- Boot verify 3: full level-row set incl. level_hash --
            let mut recomputed: std::collections::BTreeMap<Vec<u8>, Vec<u8>> =
                std::collections::BTreeMap::new();
            for ((tag, raw_price), data) in Self::live_level_rows(&book) {
                recomputed.insert(
                    level_row_key_tagged(market_id, tag, raw_price).to_vec(),
                    data.encode().to_vec(),
                );
            }
            if recomputed != acc.levels {
                return fail(format!(
                    "3c: market {market_id}: recomputed level rows != root-committed \
                     level rows (node-local order store corrupt/stale — refusing to \
                     serve or sign)"
                ));
            }

            let book_next_id = book.next_order_id();
            if book_next_id > max_order_id {
                max_order_id = book_next_id;
            }
            books.insert(market_id, book);
        }

        let next_id = if max_order_id > 0 { max_order_id } else { 1 };
        (books, next_id, None)
    }

    /// Read-only recompute of EVERY non-empty level's row data for a book
    /// (boot verify / staleness-guard verify — does NOT touch the journals).
    fn live_level_rows(book: &OrderBook) -> Vec<((u8, i128), LevelRowData)> {
        use torus_core::book_rows::{SIDE_TAG_ASK, SIDE_TAG_BID};
        let mut out = Vec::new();
        for (tag, prices) in [
            (SIDE_TAG_BID, book.bid_queues().map(|(p, _)| p.raw()).collect::<Vec<_>>()),
            (SIDE_TAG_ASK, book.ask_queues().map(|(p, _)| p.raw()).collect::<Vec<_>>()),
        ] {
            for raw in prices {
                if let Some(data) = book.level_row_data(tag, raw) {
                    out.push(((tag, raw), data));
                }
            }
        }
        out
    }

    /// Node-local `__book_mode__` marker row (CF_NATIVE_MARKETS — same
    /// classification as `NEXT_GLOBAL_ORDER_ID_KEY`: non-root, key length
    /// != 8 so every market reader skips it). Written on first save, checked
    /// at boot — catches wrong-flag restarts on chains whose books are still
    /// empty.
    const BOOK_MODE_MARKER_KEY: &'static [u8] = b"__book_mode__";

    /// Read the persisted book-mode marker byte, if the row exists.
    fn load_book_mode_marker(state: &T) -> Option<u8> {
        use torus_state::cf::CF_NATIVE_MARKETS;
        let bytes = state
            .get_cf_raw(CF_NATIVE_MARKETS, Self::BOOK_MODE_MARKER_KEY)
            .ok()
            .flatten()?;
        (bytes.len() == 1).then(|| bytes[0])
    }

    /// Durable global-order-id counter row. Lives in `CF_NATIVE_MARKETS` — a
    /// non-consensus-root CF (NOT one of `NATIVE_ROOT_CFS`, and every reader of
    /// that CF skips keys whose length != 8) — so it is node-local metadata
    /// that can never perturb the state root and is invisible to old binaries.
    const NEXT_GLOBAL_ORDER_ID_KEY: &'static [u8] = b"__next_global_order_id__";

    /// Read the persisted counter, if the row exists (DBs written before S395
    /// have none — the caller falls back to the book-maxima scan).
    fn load_next_global_order_id(state: &T) -> Option<u128> {
        use torus_state::cf::CF_NATIVE_MARKETS;
        let bytes = state
            .get_cf_raw(CF_NATIVE_MARKETS, Self::NEXT_GLOBAL_ORDER_ID_KEY)
            .ok()
            .flatten()?;
        Some(u128::from_be_bytes(bytes.try_into().ok()?))
    }

    /// FIX 1 (ECON-FIND-02) + S395 dirty-only rewrite: persist the order books
    /// TOUCHED this block (placed / matched / cancelled / modified) and the
    /// global order-id counter when it advanced. Untouched books already hold
    /// identical bytes in the CF. Returns the number of CF writes performed
    /// (whole-book rows classically; row/level puts + deletes under modes 1/2
    /// — test hook either way; node-local order-row writes under mode 2 are
    /// NOT counted, they are outside the root).
    ///
    /// Journal-in-book (3c): modes 1/2 drain the book's own journals — no
    /// shadow, no whole-book walk, no save-time seq derivation:
    ///   mode 1: `take_row_ops` → root-CF order rows; level ops DISCARDED;
    ///           stops + meta shared.
    ///   mode 2: `take_row_ops` → NODE-LOCAL `CF_BOOK_ORDER_ROWS`;
    ///           `take_level_ops` → root-CF level rows (qty ‖ count ‖ hash);
    ///           stops + meta shared.
    pub fn save_order_books(&mut self) -> usize {
        use torus_state::cf::{
            CF_BOOK_ORDER_ROWS, CF_NATIVE_MARKETS, CF_NATIVE_ORDER_BOOKS,
        };

        let mut written = 0;
        // Deterministic save order (market id ascending) — the overlay is
        // keyed so final bytes never depend on order, but stable iteration
        // keeps write counts and logs reproducible.
        let mut dirty: Vec<MarketId> = self.dirty_books.iter().copied().collect();
        dirty.sort_unstable();
        // L3 attribution (µbench-only): per-sub-step accumulators. With the
        // `save-timings` feature off, `timed` is statically false and every
        // accumulator below is dead code.
        #[cfg(feature = "save-timings")]
        let timed = self.collect_save_timings;
        #[cfg(not(feature = "save-timings"))]
        let timed = false;
        let mut acc = SaveTimings {
            markets: dirty.len() as u32,
            ..SaveTimings::default()
        };
        // L3: mode-2 level-hash sponge cache — GLOBAL budget split across
        // live books (docs/design-levelhash-cache.md §2), so more markets ⇒ a
        // smaller slice each, never more total RAM. On by default
        // (`DEFAULT_LEVEL_HASH_CACHE_MB`); 0 = explicit opt-out. Modes 0/1
        // never reach this arm, so they pay nothing.
        let level_cache_per_book = if matches!(self.book_mode, BookMode::LevelAuthority)
            && self.level_hash_cache_bytes > 0
        {
            (self.level_hash_cache_bytes / self.order_books.len().max(1)).max(1)
        } else {
            0
        };
        for market_id in dirty {
            let Some(book) = self.order_books.get_mut(&market_id) else {
                continue;
            };
            match self.book_mode {
                BookMode::Classic => {
                    let key = market_id.to_be_bytes();
                    match borsh::to_vec(&*book) {
                        Ok(data) => {
                            if let Err(e) =
                                self.state.put_cf_raw(CF_NATIVE_ORDER_BOOKS, &key, &data)
                            {
                                tracing::error!(market_id, %e, "failed to persist order book");
                            } else {
                                written += 1;
                            }
                        }
                        Err(e) => {
                            tracing::error!(market_id, %e, "failed to serialize order book");
                        }
                    }
                    // Classic never drains the journals — cap their memory.
                    book.discard_row_ops();
                    book.discard_level_ops();
                }
                BookMode::OrderRows => {
                    for (order_id, op) in book.take_row_ops() {
                        let key = book_order_key(market_id, order_id);
                        let res = match &op {
                            Some(bytes) => {
                                self.state.put_cf_raw(CF_NATIVE_ORDER_BOOKS, &key, bytes)
                            }
                            None => self.state.delete_cf_raw(CF_NATIVE_ORDER_BOOKS, &key),
                        };
                        match res {
                            Ok(()) => written += 1,
                            Err(e) => tracing::error!(
                                market_id, order_id = %order_id, %e,
                                "C4: order row write failed"
                            ),
                        }
                    }
                    // Mode 1 has no level rows: drop journaled levels unhashed.
                    book.discard_level_ops();
                    written += Self::diff_stop_rows(&self.state, market_id, book);
                    written += Self::write_meta_if_moved(&self.state, market_id, book);
                }
                BookMode::LevelAuthority => {
                    // L3: enable/refresh (or actively drop, when budget = 0)
                    // the level-hash sponge cache before draining level ops.
                    // Byte-identical output in both states.
                    if level_cache_per_book > 0 {
                        book.ensure_level_hash_cache(level_cache_per_book);
                    } else {
                        book.disable_level_hash_cache();
                    }
                    let mut rows_written = 0usize;
                    let mut rows_deleted = 0usize;
                    let t_rows = timed.then(std::time::Instant::now);
                    for (order_id, op) in book.take_row_ops() {
                        let key = book_order_key(market_id, order_id);
                        let res = match &op {
                            Some(bytes) => {
                                rows_written += 1;
                                self.state.put_cf_raw(CF_BOOK_ORDER_ROWS, &key, bytes)
                            }
                            None => {
                                rows_deleted += 1;
                                self.state.delete_cf_raw(CF_BOOK_ORDER_ROWS, &key)
                            }
                        };
                        if let Err(e) = res {
                            tracing::error!(
                                market_id, order_id = %order_id, %e,
                                "3c: node-local order row write failed"
                            );
                        }
                    }
                    if let Some(t) = t_rows {
                        acc.rows_ns += t.elapsed().as_nanos();
                    }
                    let mut levels_written = 0usize;
                    let mut levels_deleted = 0usize;
                    let t_levels = timed.then(std::time::Instant::now);
                    for ((tag, raw_price), op) in book.take_level_ops() {
                        let key = level_row_key_tagged(market_id, tag, raw_price);
                        let res = match &op {
                            Some(data) => {
                                levels_written += 1;
                                self.state.put_cf_raw(
                                    CF_NATIVE_ORDER_BOOKS,
                                    &key,
                                    &data.encode(),
                                )
                            }
                            None => {
                                levels_deleted += 1;
                                self.state.delete_cf_raw(CF_NATIVE_ORDER_BOOKS, &key)
                            }
                        };
                        match res {
                            Ok(()) => written += 1,
                            Err(e) => tracing::error!(
                                market_id, %e, "3c: level row write failed"
                            ),
                        }
                    }
                    if let Some(t) = t_levels {
                        acc.levels_ns += t.elapsed().as_nanos();
                    }
                    let t_stops = timed.then(std::time::Instant::now);
                    written += Self::diff_stop_rows(&self.state, market_id, book);
                    if let Some(t) = t_stops {
                        acc.stops_ns += t.elapsed().as_nanos();
                    }
                    let t_meta = timed.then(std::time::Instant::now);
                    written += Self::write_meta_if_moved(&self.state, market_id, book);
                    if let Some(t) = t_meta {
                        acc.meta_ns += t.elapsed().as_nanos();
                    }
                    if let Some(ref m) = self.metrics {
                        m.exec_book_rows_written.inc_by(rows_written as u64);
                        m.exec_book_rows_deleted.inc_by(rows_deleted as u64);
                        m.exec_book_levels_written.inc_by(levels_written as u64);
                        m.exec_book_levels_deleted.inc_by(levels_deleted as u64);
                    }
                }
            }
        }

        // Persist the counter only when it moved (or was never stored), so
        // blocks without order flow write nothing at all.
        if self.loaded_next_global_order_id != Some(self.next_global_order_id) {
            if let Err(e) = self.state.put_cf_raw(
                CF_NATIVE_MARKETS,
                Self::NEXT_GLOBAL_ORDER_ID_KEY,
                &self.next_global_order_id.to_be_bytes(),
            ) {
                tracing::error!(%e, "failed to persist next_global_order_id");
            }
        }

        // 3c: write the node-local mode marker once (first save on this DB).
        if !self.book_mode_marker_present {
            match self.state.put_cf_raw(
                CF_NATIVE_MARKETS,
                Self::BOOK_MODE_MARKER_KEY,
                &[self.book_mode.marker_byte()],
            ) {
                Ok(()) => self.book_mode_marker_present = true,
                Err(e) => tracing::error!(%e, "failed to persist __book_mode__ marker"),
            }
        }

        #[cfg(feature = "save-timings")]
        if timed {
            self.last_save_timings = acc;
        }
        #[cfg(not(feature = "save-timings"))]
        let _ = acc;

        written
    }

    /// Shared modes 1/2: pending stops are immutable per id → put new ids,
    /// delete gone ids. Stateless diff against the persisted stop rows (one
    /// bounded prefix scan over `market ‖ 0x02` — the stop set is tiny).
    fn diff_stop_rows(state: &T, market_id: MarketId, book: &OrderBook) -> usize {
        use torus_state::cf::CF_NATIVE_ORDER_BOOKS;
        let mut prefix = [0u8; 9];
        prefix[..8].copy_from_slice(&market_id.to_be_bytes());
        prefix[8] = ROW_TAG_STOP;
        let persisted: std::collections::HashSet<u128> = state
            .iterate_cf(CF_NATIVE_ORDER_BOOKS, Some(&prefix))
            .map(|rows| {
                rows.iter()
                    .filter(|(k, _)| k.len() == 25 && k[8] == ROW_TAG_STOP)
                    .map(|(k, _)| u128::from_be_bytes(k[9..25].try_into().unwrap()))
                    .collect()
            })
            .unwrap_or_default();

        let mut written = 0usize;
        let stops = book.stop_rows();
        let mut live = std::collections::HashSet::with_capacity(stops.len());
        for (id, bytes) in stops {
            live.insert(id);
            if !persisted.contains(&id) {
                let key = book_stop_key(market_id, id);
                if let Err(e) = state.put_cf_raw(CF_NATIVE_ORDER_BOOKS, &key, &bytes) {
                    tracing::error!(market_id, stop_id = %id, %e, "stop row put failed");
                } else {
                    written += 1;
                }
            }
        }
        let mut gone: Vec<u128> = persisted.difference(&live).copied().collect();
        gone.sort_unstable();
        for id in gone {
            let key = book_stop_key(market_id, id);
            if let Err(e) = state.delete_cf_raw(CF_NATIVE_ORDER_BOOKS, &key) {
                tracing::error!(market_id, stop_id = %id, %e, "stop row delete failed");
            } else {
                written += 1;
            }
        }
        written
    }

    /// Shared modes 1/2: rewrite the meta row only when its bytes moved
    /// (stateless compare against the persisted row — one point read).
    fn write_meta_if_moved(state: &T, market_id: MarketId, book: &OrderBook) -> usize {
        use torus_state::cf::CF_NATIVE_ORDER_BOOKS;
        let meta = book_meta_value(book);
        let key = book_meta_key(market_id);
        match state.get_cf_raw(CF_NATIVE_ORDER_BOOKS, &key) {
            Ok(Some(existing)) if existing == meta => return 0,
            Ok(_) => {}
            Err(e) => {
                tracing::error!(market_id, %e, "meta row read failed");
            }
        }
        if let Err(e) = state.put_cf_raw(CF_NATIVE_ORDER_BOOKS, &key, &meta) {
            tracing::error!(market_id, %e, "meta row put failed");
            return 0;
        }
        1
    }

    /// Persist ONE book IN FULL under `mode` — genesis seeding, tests,
    /// offline rebuild (the block path is [`Self::save_order_books`]).
    /// Reconciles: deletes every persisted key for this market that the
    /// fresh write does not overwrite, in BOTH the root CF and (mode 2) the
    /// node-local order-row store.
    pub fn save_book_full(state: &T, book: &mut OrderBook, mode: BookMode) -> usize {
        use torus_state::cf::{CF_BOOK_ORDER_ROWS, CF_NATIVE_ORDER_BOOKS};
        let market_id = book.market_id;
        let mut written = 0usize;

        if mode == BookMode::Classic {
            let key = market_id.to_be_bytes();
            if let Ok(data) = borsh::to_vec(&*book) {
                if state.put_cf_raw(CF_NATIVE_ORDER_BOOKS, &key, &data).is_ok() {
                    written += 1;
                }
            }
            book.discard_row_ops();
            book.discard_level_ops();
            return written;
        }

        let row_ops = book.full_row_ops();
        let level_ops = book.full_level_ops();
        let meta = book_meta_value(book);
        let stops = book.stop_rows();

        // Fresh key set per CF.
        let mut keep_root: std::collections::HashSet<Vec<u8>> =
            std::collections::HashSet::new();
        let mut keep_store: std::collections::HashSet<Vec<u8>> =
            std::collections::HashSet::new();
        keep_root.insert(book_meta_key(market_id).to_vec());
        for (id, _) in &stops {
            keep_root.insert(book_stop_key(market_id, *id).to_vec());
        }
        match mode {
            BookMode::OrderRows => {
                for (id, _) in &row_ops {
                    keep_root.insert(book_order_key(market_id, *id).to_vec());
                }
            }
            BookMode::LevelAuthority => {
                for (id, _) in &row_ops {
                    keep_store.insert(book_order_key(market_id, *id).to_vec());
                }
                for ((tag, raw), _) in &level_ops {
                    keep_root.insert(level_row_key_tagged(market_id, *tag, *raw).to_vec());
                }
            }
            BookMode::Classic => unreachable!(),
        }

        // Reconcile-delete stale keys.
        let prefix = market_id.to_be_bytes();
        if let Ok(existing) = state.iterate_cf(CF_NATIVE_ORDER_BOOKS, Some(&prefix)) {
            for (key, _) in existing {
                if !keep_root.contains(&key)
                    && state.delete_cf_raw(CF_NATIVE_ORDER_BOOKS, &key).is_ok()
                {
                    written += 1;
                }
            }
        }
        if mode == BookMode::LevelAuthority {
            if let Ok(existing) = state.iterate_cf(CF_BOOK_ORDER_ROWS, Some(&prefix)) {
                for (key, _) in existing {
                    if !keep_store.contains(&key) {
                        let _ = state.delete_cf_raw(CF_BOOK_ORDER_ROWS, &key);
                    }
                }
            }
        }

        // Fresh writes.
        let _ = state.put_cf_raw(CF_NATIVE_ORDER_BOOKS, &book_meta_key(market_id), &meta);
        written += 1;
        for (id, bytes) in &stops {
            let _ = state.put_cf_raw(
                CF_NATIVE_ORDER_BOOKS,
                &book_stop_key(market_id, *id),
                bytes,
            );
            written += 1;
        }
        match mode {
            BookMode::OrderRows => {
                for (id, bytes) in &row_ops {
                    let _ = state.put_cf_raw(
                        CF_NATIVE_ORDER_BOOKS,
                        &book_order_key(market_id, *id),
                        bytes,
                    );
                    written += 1;
                }
            }
            BookMode::LevelAuthority => {
                for (id, bytes) in &row_ops {
                    let _ = state.put_cf_raw(
                        CF_BOOK_ORDER_ROWS,
                        &book_order_key(market_id, *id),
                        bytes,
                    );
                }
                for ((tag, raw), data) in &level_ops {
                    let _ = state.put_cf_raw(
                        CF_NATIVE_ORDER_BOOKS,
                        &level_row_key_tagged(market_id, *tag, *raw),
                        &data.encode(),
                    );
                    written += 1;
                }
            }
            BookMode::Classic => unreachable!(),
        }
        written
    }
}

// ============================================================================
// NativeExecutor — dispatch + execution
// ============================================================================

/// Dispatches each NativeAction variant to the correct torus-core / torus-economics handler.
///
/// Individual action failures are captured in `NativeActionResult` and do NOT
/// prevent other actions from executing (deterministic batch semantics).
pub struct NativeExecutor;

impl NativeExecutor {
    /// Execute a single native action for the given sender.
    pub fn execute<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
        sender: &Address,
        action: &NativeAction,
    ) -> NativeActionResult {
        match action {
            // ---- Order book ----
            NativeAction::PlaceOrder(params) => Self::exec_place_order(ctx, sender, params),
            // A batch reaching the single-action path (e.g. crash-replay) is routed through
            // the same flatten + per-market parallel pipeline as the live path, then collapsed
            // to one summary result. `execute_batch` flattens batches into `PlaceOrder`s, so it
            // never re-enters this arm — no recursion.
            NativeAction::PlaceOrderBatch(orders) => {
                if !torus_types::batch_len_within_cap(orders.len()) {
                    return NativeActionResult::err(
                        "place_order_batch",
                        format!(
                            "batch size {} outside [1, {}] — skipped (deterministic cap)",
                            orders.len(),
                            torus_types::NATIVE_ORDERS_PER_BATCH_CAP
                        ),
                    );
                }
                let pair = (*sender, action.clone());
                let batch = Self::execute_batch(ctx, std::slice::from_ref(&pair));
                let total = batch.results.len();
                let ok = batch.results.iter().filter(|r| r.success).count();
                NativeActionResult {
                    action_type: "place_order_batch",
                    success: ok == total,
                    error: (ok != total).then(|| format!("{ok}/{total} orders placed")),
                    gas_used: batch.total_gas,
                }
            }
            NativeAction::CancelOrder { order_id } => {
                Self::exec_cancel_order(ctx, sender, *order_id)
            }
            NativeAction::CancelAllOrders { market_id } => {
                Self::exec_cancel_all(ctx, sender, *market_id)
            }
            NativeAction::ModifyOrder {
                order_id,
                new_price,
                new_qty,
            } => Self::exec_modify_order(ctx, *order_id, *new_price, *new_qty),

            // ---- Staking ----
            NativeAction::Delegate { validator, amount } => {
                Self::exec_delegate(ctx, sender, validator, *amount)
            }
            NativeAction::Undelegate { validator, amount } => {
                Self::exec_undelegate(ctx, sender, validator, *amount)
            }
            NativeAction::PermanentStake { amount } => {
                Self::exec_permanent_stake(ctx, sender, *amount)
            }
            NativeAction::ClaimRewards => Self::exec_claim_rewards(ctx, sender),
            // FIX ECON-FIND-15: TopUpSelfStake via NativeAction.
            NativeAction::TopUpSelfStake { amount } => {
                match ctx.staking.top_up_self_stake(*sender, *amount) {
                    Ok(()) => NativeActionResult::ok("top_up_self_stake", 2000),
                    Err(e) => NativeActionResult::err("top_up_self_stake", e.to_string()),
                }
            }

            // ---- Oracle ----
            NativeAction::SubmitOraclePrices(submission) => Self::exec_submit_oracle_prices(
                ctx,
                sender,
                &submission.prices,
                submission.timestamp,
            ),

            // ---- Governance ----
            NativeAction::SubmitProposal(proposal) => {
                Self::exec_submit_proposal(ctx, sender, proposal)
            }
            NativeAction::Vote {
                proposal_id,
                option,
            } => Self::exec_vote(ctx, sender, *proposal_id, *option),

            // ---- Lockbox / Transfers ----
            NativeAction::TransferToPerp { amount } => {
                Self::exec_deposit_to_native(ctx, sender, *amount)
            }
            NativeAction::TransferToSpot { amount } => {
                Self::exec_withdraw_from_native(ctx, sender, *amount)
            }
            // FIX ECON-PF-17: Wire `to` address — debit sender, credit recipient.
            NativeAction::Withdraw { amount, to } => {
                Self::exec_withdraw_to(ctx, sender, to, *amount)
            }

            // ---- Validator management ----
            NativeAction::RegisterValidator { pubkey, commission } => {
                Self::exec_register_validator(ctx, sender, pubkey, *commission)
            }
            NativeAction::UpdateCommission { new_rate } => {
                Self::exec_update_commission(ctx, sender, *new_rate)
            }
            NativeAction::JailVote { target } => Self::exec_jail_vote(ctx, sender, target),
            NativeAction::UnjailSelf => Self::exec_unjail_self(ctx, sender),
            NativeAction::RotateValidatorKey { new_pubkey } => {
                Self::exec_rotate_key(ctx, sender, new_pubkey)
            }

            // ---- Session Keys ----
            NativeAction::CreateSession {
                session_pubkey,
                expiry,
                scope,
            } => Self::exec_create_session(ctx, sender, session_pubkey, *expiry, *scope),
            NativeAction::RevokeSession { session_pubkey } => {
                Self::exec_revoke_session(ctx, sender, session_pubkey)
            }

            // ---- Admin (governance-gated, stubs) ----
            // AUDIT: ECON-PF-07 -- Intentional stubs. Market management (listing,
            // delisting, param updates) will be implemented in the market registry
            // feature. Variants are defined now so governance pipeline and EIP-712
            // encoding are stable before the registry is built.
            NativeAction::UpdateMarketParams { .. } => {
                NativeActionResult::ok("update_market_params", 0)
            }
            NativeAction::ListMarket(_) => NativeActionResult::ok("list_market", 0),
            NativeAction::DelistMarket { .. } => NativeActionResult::ok("delist_market", 0),
        }
    }

    /// Execute a batch of (sender, action) pairs with per-market parallel matching.
    ///
    /// Pipeline:
    ///   Phase 1 — Execute all non-PlaceOrder actions sequentially
    ///   Phase 2 — Pre-reserve margin, assign global order IDs, partition by market
    ///   Phase 3 — Parallel matching: one thread per market's OrderBook
    ///   Phase 4 — Settlement: release margin, apply fills, persist trades.
    ///             C3: `TORUS_PARALLEL_SETTLE=1` opts in to parallel per-market
    ///             compute + deterministic apply; default (unset) is the
    ///             classic sequential loop, byte-identical to pre-C3.
    ///
    /// Individual action failures do NOT stop the batch (deterministic semantics).
    pub fn execute_batch<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
        actions: &[(Address, NativeAction)],
    ) -> NativeBatchResult {
        Self::execute_batch_inner(ctx, actions, SettleMode::Auto, EngineMode::Auto)
    }

    /// `execute_batch` with the Phase-4 settle mode pinned explicitly —
    /// `parallel = false` runs the classic sequential settle loop; `true`
    /// runs the parallel path whenever >=2 markets have work (no size gate).
    /// The L3-ENG engine path is pinned OFF (pre-engine behavior, exactly).
    /// For A/B benches and the differential determinism tests (per-process
    /// env vars race across test threads; this doesn't).
    pub fn execute_batch_settle_mode<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
        actions: &[(Address, NativeAction)],
        parallel: bool,
    ) -> NativeBatchResult {
        Self::execute_batch_inner(ctx, actions, SettleMode::Force(parallel), EngineMode::Force(0))
    }

    /// L3-ENG: `execute_batch` with the engine thread count pinned explicitly
    /// (tests / benches — immune to per-process env races). `threads <= 1`
    /// pins the CANONICAL serial path (serial Phase-2 prepare + sequential
    /// settle); `threads >= 2` pins sharded Phase-2 prepare with that worker
    /// count + forced parallel settle whenever >=2 markets have work (all
    /// work gates bypassed). The differential contract: any `threads` value
    /// must produce byte-identical state, events, and results.
    pub fn execute_batch_engine_mode<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
        actions: &[(Address, NativeAction)],
        threads: usize,
    ) -> NativeBatchResult {
        if threads >= 2 {
            Self::execute_batch_inner(
                ctx,
                actions,
                SettleMode::Force(true),
                EngineMode::Force(threads),
            )
        } else {
            Self::execute_batch_inner(ctx, actions, SettleMode::Force(false), EngineMode::Force(0))
        }
    }

    fn execute_batch_inner<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
        actions: &[(Address, NativeAction)],
        settle_mode: SettleMode,
        engine_mode: EngineMode,
    ) -> NativeBatchResult {
        // One flattened executable entry: either a PlaceOrder's params (single or
        // batch-expanded) or any other action. C2: everything is BORROWED from the
        // caller's `actions` slice — the old Cow flatten deep-cloned every order
        // AND every non-place action whenever a PlaceOrderBatch was present.
        enum FlatAction<'a> {
            Other(&'a NativeAction),
            Place(&'a PlaceOrderParams),
        }

        // Flatten any PlaceOrderBatch into individual (sender, PlaceOrder) entries so the
        // per-market parallel matching pipeline treats batched and singly-submitted orders
        // identically. Deterministic: actions in slice order, orders in batch order.
        let mut flat: Vec<(Address, FlatAction<'_>)> = Vec::with_capacity(actions.len());
        let mut skipped_batches = 0usize;
        for (sender, action) in actions {
            match action {
                NativeAction::PlaceOrder(p) => flat.push((*sender, FlatAction::Place(p))),
                NativeAction::PlaceOrderBatch(orders) => {
                    // G1 (O2): DETERMINISTIC exec-side cap. The RPC/admit
                    // checks are node-local policy; this is the consensus-
                    // critical bound. An oversize (or empty) batch is
                    // skipped WHOLESALE — same doctrine as the replay-guard
                    // skip (app.rs warn + continue) — the block is never
                    // aborted, and every correct node skips identically.
                    //
                    // LOCKSTEP-DEPLOY: this skip is a consensus-semantics
                    // change (a pre-upgrade node flattens+executes an
                    // oversize batch; an upgraded node skips it → divergent
                    // state root on a block that carries one). The whole
                    // fleet must run this before any block can legally carry
                    // a batch above the cap — same deploy class as the
                    // SessionScope::Trading widening (torus-types lib.rs).
                    if !torus_types::batch_len_within_cap(orders.len()) {
                        // Aggregate the count; do NOT log per batch. A
                        // malicious proposer can pack a block with thousands
                        // of tiny empty/oversize batches under the byte cap;
                        // per-batch WARN with %sender formatting would stall
                        // the single exec thread every block (log-DoS).
                        skipped_batches += 1;
                        continue;
                    }
                    for p in orders {
                        flat.push((*sender, FlatAction::Place(p)));
                    }
                }
                other => flat.push((*sender, FlatAction::Other(other))),
            }
        }
        if skipped_batches > 0 {
            tracing::warn!(
                skipped_batches,
                cap = torus_types::NATIVE_ORDERS_PER_BATCH_CAP,
                "skipped oversize/empty PlaceOrderBatch action(s) at exec (deterministic cap)"
            );
        }

        let n = flat.len();
        let mut results: Vec<NativeActionResult> = (0..n)
            .map(|_| NativeActionResult::ok("pending", 0))
            .collect();
        let mut total_gas = 0u64;

        // ---- Phase 1: Partition and execute non-PlaceOrder actions ----
        let mut place_order_indices: Vec<usize> = Vec::new();

        for (i, (sender, entry)) in flat.iter().enumerate() {
            match entry {
                FlatAction::Place(_) => place_order_indices.push(i),
                FlatAction::Other(action) => {
                    let result = Self::execute(ctx, sender, action);
                    total_gas += result.gas_used;
                    results[i] = result;
                }
            }
        }

        if place_order_indices.is_empty() {
            return NativeBatchResult { results, total_gas };
        }

        // ---- Phase 2: Pre-reserve margin, assign IDs, partition by market ----
        let margin_timer = std::time::Instant::now();
        // C2: `PreparedOrder.params` borrows from the caller's `actions` slice
        // — the prepared order carries an 8-byte reference through Phases 2-4
        // instead of a per-order deep clone of `PlaceOrderParams`.
        let mut market_batches: HashMap<MarketId, Vec<PreparedOrder<'_>>> = HashMap::new();

        // O1: write-through balance cache, scoped to this execute_batch call. Serves
        // repeated Phase 2 reserve / Phase 4 release reads for the same sender without
        // re-hitting the overlay's lock + alloc + Borsh path.
        let mut bal_cache = BalanceCache::new();
        // C1: same pattern for position rows — Phase 4 does two position
        // read-modify-writes per fill; they hit this map and flush once
        // (sorted keys) at the end of the call.
        let mut pos_cache = PositionCache::new();

        // L3-ENG: resolve the engine worker count (0 = serial prepare).
        let engine_threads = match engine_mode {
            EngineMode::Force(t) => {
                if t >= 2 {
                    t
                } else {
                    0
                }
            }
            EngineMode::Auto => parallel_engine_threads(),
        };

        // L3-ENG: sharded Phase-2 prepare. Workers compute each sender's
        // pass/fail fold on disjoint sender shards; the serial stitch below
        // replays flat order to assign order ids — byte-identical to the
        // serial loop (docs/design-parallel-engine.md §3). Any worker panic
        // falls back to the serial loop with nothing shared mutated.
        let mut prep_outcomes: Option<Vec<Option<PrepOutcome>>> = None;
        if engine_threads >= 2
            && (matches!(engine_mode, EngineMode::Force(_))
                || place_order_indices.len() >= parallel_engine_min_orders())
        {
            // Group place indices by sender, first-appearance order (order of
            // groups is irrelevant — shards are disjoint — but keep it
            // deterministic anyway).
            let mut groups: Vec<(Address, Vec<(usize, &PlaceOrderParams)>)> = Vec::new();
            let mut group_of: HashMap<Address, usize> = HashMap::new();
            for &i in &place_order_indices {
                let (sender, entry) = &flat[i];
                let params: &PlaceOrderParams = match entry {
                    FlatAction::Place(p) => p,
                    FlatAction::Other(_) => unreachable!(),
                };
                let gi = *group_of.entry(*sender).or_insert_with(|| {
                    groups.push((*sender, Vec::new()));
                    groups.len() - 1
                });
                groups[gi].1.push((i, params));
            }
            if groups.len() >= 2 {
                match Self::phase2_parallel_prepare(
                    &ctx.positions,
                    &ctx.margin_configs,
                    &groups,
                    engine_threads,
                    n,
                ) {
                    Some((outcomes, worker_cache)) => {
                        // Sender shards are disjoint, so the merged cache is
                        // exactly the serial loop's cache.
                        bal_cache.merge_disjoint(worker_cache);
                        prep_outcomes = Some(outcomes);
                    }
                    None => {
                        tracing::error!(
                            "L3-ENG: phase-2 prepare worker panicked — falling back to serial prepare"
                        );
                    }
                }
            }
        }

        if let Some(mut outcomes) = prep_outcomes {
            // Serial stitch in flat order: ids go to passing orders exactly
            // as the serial loop assigns them.
            for &i in &place_order_indices {
                let (sender, entry) = &flat[i];
                let params: &PlaceOrderParams = match entry {
                    FlatAction::Place(p) => p,
                    FlatAction::Other(_) => unreachable!(),
                };
                let Some(outcome) = outcomes[i].take() else {
                    unreachable!("every place index has a worker outcome");
                };
                match outcome {
                    PrepOutcome::Pass(margin_reserved) => {
                        let order_id = ctx.next_global_order_id;
                        ctx.next_global_order_id += 1;
                        market_batches
                            .entry(params.market_id)
                            .or_default()
                            .push(PreparedOrder {
                                index: i,
                                sender: *sender,
                                params,
                                order_id,
                                margin_reserved,
                            });
                    }
                    PrepOutcome::Reject { margin, msg } => {
                        // Funnel (perf A1): same counters as the serial loop.
                        if let Some(ref m) = ctx.metrics {
                            if margin {
                                m.orders_rejected_margin.inc();
                            } else {
                                m.orders_rejected_other.inc();
                            }
                        }
                        results[i] = NativeActionResult::err("place_order", msg);
                    }
                }
            }
        } else {
            for &i in &place_order_indices {
                let (sender, entry) = &flat[i];
                let params: &PlaceOrderParams = match entry {
                    FlatAction::Place(p) => p,
                    FlatAction::Other(_) => unreachable!(),
                };

                let market_id = params.market_id;
                let is_market = matches!(params.order_type, OrderType::Market);

                // Reserve margin (same logic as exec_place_order Phase 2).
                // A5: reserve and every later release share reserve_for_qty.
                let order_margin_required = if !is_market {
                    Self::reserve_for_qty(ctx, market_id, params.price, params.quantity)
                } else {
                    FixedPoint::ZERO
                };

                if order_margin_required > FixedPoint::ZERO {
                    match bal_cache.load(&ctx.positions, sender) {
                        Ok(mut bal) => {
                            if bal.available < order_margin_required {
                                // Funnel (perf A1): died pre-book on the margin reserve.
                                if let Some(ref m) = ctx.metrics {
                                    m.orders_rejected_margin.inc();
                                }
                                results[i] = NativeActionResult::err(
                                    "place_order",
                                    format!(
                                        "insufficient margin: need {order_margin_required}, have {}",
                                        bal.available
                                    ),
                                );
                                continue;
                            }
                            bal.available -= order_margin_required;
                            bal.order_margin += order_margin_required;
                            bal_cache.set(sender, bal);
                        }
                        Err(e) => {
                            // Funnel (perf A1): died pre-book on a balance read error.
                            if let Some(ref m) = ctx.metrics {
                                m.orders_rejected_other.inc();
                            }
                            results[i] = NativeActionResult::err("place_order", e.to_string());
                            continue;
                        }
                    }
                }

                // Assign global order ID (monotonic, pre-matching)
                let order_id = ctx.next_global_order_id;
                ctx.next_global_order_id += 1;

                market_batches
                    .entry(market_id)
                    .or_default()
                    .push(PreparedOrder {
                        index: i,
                        sender: *sender,
                        params,
                        order_id,
                        margin_reserved: order_margin_required,
                    });
            }
        }

        if let Some(ref m) = ctx.metrics {
            m.exec_phase_margin_seconds
                .observe(margin_timer.elapsed().as_secs_f64());
        }

        // ---- Phase 3: Parallel matching ----
        let match_timer = std::time::Instant::now();
        let mut worker_batches: HashMap<MarketId, (OrderBook, Vec<MatchRequest<'_>>)> =
            HashMap::new();

        for (&market_id, prepared) in &market_batches {
            let book = ctx
                .order_books
                .remove(&market_id)
                .unwrap_or_else(|| OrderBook::new(market_id, FixedPoint::ONE, FixedPoint::ONE));

            let requests: Vec<MatchRequest<'_>> = prepared
                .iter()
                .map(|p| MatchRequest {
                    sender: p.sender,
                    params: p.params,
                    order_id: p.order_id,
                })
                .collect();

            worker_batches.insert(market_id, (book, requests));
        }

        let mut market_results = match MarketWorkerPool::match_parallel(worker_batches, ctx.timestamp)
        {
            Ok(r) => r,
            Err(panic) => {
                // T1.5 FAIL-STOP: the panicking worker consumed its market's
                // book (and this block's other books were consumed with it),
                // so settlement cannot proceed and the block must never be
                // applied. Latch the fatal on the context — the committer
                // halts the execution pipeline on it — and bail out loudly.
                tracing::error!(
                    market_id = panic.market_id,
                    message = %panic.message,
                    "market worker panicked — FATAL, block cannot be executed"
                );
                for &i in &place_order_indices {
                    results[i] = NativeActionResult::err(
                        "place_order",
                        "market worker panicked — block execution aborted".to_string(),
                    );
                }
                ctx.fatal_error = Some(format!(
                    "market worker panicked (market {}): {}",
                    panic.market_id, panic.message
                ));
                return NativeBatchResult { results, total_gas };
            }
        };
        if let Some(ref m) = ctx.metrics {
            m.exec_phase_match_seconds
                .observe(match_timer.elapsed().as_secs_f64());
        }

        // ---- Phase 4: settlement ----
        // A5: settle markets in market-id order. `match_parallel` returns
        // HashMap iteration order (random per instance) — balance mutations
        // are commutative so consensus state never depended on it, but the
        // per-block trade_index assignment (node-local trade keys) and the
        // defensive `.min(order_margin)` clamps on the new cross-trader maker
        // releases do observe settlement order. Sorting pins both.
        market_results.sort_by_key(|m| m.market_id);
        let settle_timer = std::time::Instant::now();
        // C3: parallel settle pays a thread scope + plan handoff, so it needs
        // >=2 markets with work (always) and, in Auto mode, enough fills to
        // amortize the overhead. The sequential loop remains the canonical
        // semantics that the parallel path must reproduce byte-for-byte.
        let use_parallel = market_results.len() >= 2
            && match settle_mode {
                SettleMode::Force(parallel) => parallel,
                SettleMode::Auto => {
                    let total_fills: usize = market_results
                        .iter()
                        .flat_map(|m| m.results.iter())
                        .map(|r| r.result.fills.len())
                        .sum();
                    // L3-ENG: with the engine on, the (byte-identical) C3
                    // parallel settle engages from a much lower fill count —
                    // cap-400 blocks never reach the classic 1024 gate.
                    (parallel_settle_enabled() && total_fills >= parallel_settle_min_fills())
                        || (engine_threads >= 2 && total_fills >= parallel_engine_min_fills())
                }
            };
        if use_parallel {
            Self::settle_market_results_parallel(
                ctx,
                market_results,
                &market_batches,
                &mut results,
                &mut total_gas,
                &mut bal_cache,
                &mut pos_cache,
            );
        } else {
            Self::settle_market_results_sequential(
                ctx,
                market_results,
                &market_batches,
                &mut results,
                &mut total_gas,
                &mut bal_cache,
                &mut pos_cache,
            );
        }

        // C1/O1: materialize all deferred position + balance mutations into the
        // overlay before the call returns (each in deterministic sorted-key
        // order), so the next execute_batch call and every post-batch consumer
        // sees authoritative state. A flush failure means committed fills are
        // not in the overlay — that block must never be applied, so latch the
        // fatal (the committer halts the execution pipeline on it).
        if let Err(e) = pos_cache.flush_all(&ctx.positions) {
            ctx.fatal_error = Some(format!("position cache flush failed: {e}"));
        }
        if let Err(e) = bal_cache.flush_all(&ctx.positions) {
            ctx.fatal_error = Some(format!("balance cache flush failed: {e}"));
        }

        if let Some(ref m) = ctx.metrics {
            m.exec_phase_settle_seconds
                .observe(settle_timer.elapsed().as_secs_f64());
        }

        NativeBatchResult { results, total_gas }
    }

    /// L3-ENG: sharded Phase-2 prepare. `groups` is the per-sender partition
    /// of the batch's PlaceOrders (each sender's orders in flat order);
    /// workers process disjoint contiguous shards of the sender list, each
    /// replaying its senders' balance folds against a worker-local
    /// `BalanceCache` (read-through to the shared overlay, which at this
    /// point holds all Phase-1 effects — exactly what the serial loop reads).
    ///
    /// Determinism (docs/design-parallel-engine.md §3): an order's outcome is
    /// a pure function of (margin config, params, its sender's balance
    /// trajectory), and the trajectory is a fold over that sender's own
    /// orders only — no other Phase-2 step touches it — so outcomes are
    /// independent of shard assignment and thread count. Returns the
    /// per-flat-index outcomes plus the merged (sender-disjoint) cache;
    /// `None` if any worker panicked (caller falls back to the serial loop —
    /// nothing shared has been mutated).
    fn phase2_parallel_prepare<T: StateBackend>(
        positions: &PositionManager<T>,
        margin_configs: &HashMap<MarketId, MarketMarginConfig>,
        groups: &[(Address, Vec<(usize, &PlaceOrderParams)>)],
        threads: usize,
        n: usize,
    ) -> Option<(Vec<Option<PrepOutcome>>, BalanceCache)> {
        let workers = threads.min(groups.len()).max(1);
        let shard = groups.len().div_ceil(workers);

        type WorkerOut = (Vec<(usize, PrepOutcome)>, BalanceCache);
        let worker_results: Vec<Result<WorkerOut, ()>> = std::thread::scope(|s| {
            let handles: Vec<_> = groups
                .chunks(shard)
                .map(|shard_groups| {
                    s.spawn(move || {
                        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            let mut cache = BalanceCache::new();
                            let mut out: Vec<(usize, PrepOutcome)> = Vec::with_capacity(
                                shard_groups.iter().map(|(_, o)| o.len()).sum(),
                            );
                            for (sender, orders) in shard_groups {
                                for &(i, params) in orders {
                                    let is_market =
                                        matches!(params.order_type, OrderType::Market);
                                    // Same formula as the serial loop / exec_place_order.
                                    let required = if !is_market {
                                        Self::reserve_for_qty_cfg(
                                            margin_configs.get(&params.market_id),
                                            params.price,
                                            params.quantity,
                                        )
                                    } else {
                                        FixedPoint::ZERO
                                    };
                                    if required > FixedPoint::ZERO {
                                        match cache.load(positions, sender) {
                                            Ok(mut bal) => {
                                                if bal.available < required {
                                                    out.push((
                                                        i,
                                                        PrepOutcome::Reject {
                                                            margin: true,
                                                            msg: format!(
                                                                "insufficient margin: need {required}, have {}",
                                                                bal.available
                                                            ),
                                                        },
                                                    ));
                                                    continue;
                                                }
                                                bal.available -= required;
                                                bal.order_margin += required;
                                                cache.set(sender, bal);
                                            }
                                            Err(e) => {
                                                out.push((
                                                    i,
                                                    PrepOutcome::Reject {
                                                        margin: false,
                                                        msg: e.to_string(),
                                                    },
                                                ));
                                                continue;
                                            }
                                        }
                                    }
                                    out.push((i, PrepOutcome::Pass(required)));
                                }
                            }
                            (out, cache)
                        }))
                        .map_err(|_| ())
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|h| h.join().unwrap_or(Err(())))
                .collect()
        });

        let mut outcomes: Vec<Option<PrepOutcome>> = (0..n).map(|_| None).collect();
        let mut merged = BalanceCache::new();
        for r in worker_results {
            match r {
                Ok((out, cache)) => {
                    for (i, o) in out {
                        outcomes[i] = Some(o);
                    }
                    merged.merge_disjoint(cache);
                }
                Err(()) => return None,
            }
        }
        Some((outcomes, merged))
    }

    /// Classic Phase-4 settlement: one thread walks the (market-id-sorted)
    /// match results and performs every mutation inline. This is the CANONICAL
    /// settle semantics — `settle_market_results_parallel` must produce
    /// byte-identical state, and `TORUS_PARALLEL_SETTLE=0` falls back here.
    #[allow(clippy::too_many_arguments)]
    fn settle_market_results_sequential<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
        market_results: Vec<crate::market_workers::MarketBatchResult>,
        market_batches: &HashMap<MarketId, Vec<PreparedOrder<'_>>>,
        results: &mut [NativeActionResult],
        total_gas: &mut u64,
        bal_cache: &mut BalanceCache,
        pos_cache: &mut PositionCache,
    ) {
        for mbr in market_results {
            let market_id = mbr.market_id;

            // Reinsert updated book
            ctx.order_books.insert(market_id, mbr.book);
            ctx.dirty_books.insert(market_id);

            // Update global ID high-water mark
            if mbr.next_order_id > ctx.next_global_order_id {
                ctx.next_global_order_id = mbr.next_order_id;
            }

            let prepared = match market_batches.get(&market_id) {
                Some(p) => p,
                None => continue,
            };

            for (match_result, prep) in mbr.results.iter().zip(prepared.iter()) {
                let result = &match_result.result;

                // Funnel (perf A1): STP maker cancels already happened in the
                // book during matching, regardless of how settlement below
                // turns out — count them here, not with the status outcome.
                if let Some(ref m) = ctx.metrics {
                    m.orders_self_trade_cancels
                        .inc_by(result.self_trade_cancels.len() as u64);
                }

                // Release margin for filled/cancelled portion
                if prep.margin_reserved > FixedPoint::ZERO {
                    let order_rests = matches!(
                        result.status,
                        OrderStatus::Resting
                            | OrderStatus::PartiallyFilled
                            | OrderStatus::PendingTrigger
                    );
                    let filled_qty: FixedPoint = result
                        .fills
                        .iter()
                        .map(|f| f.quantity)
                        .fold(FixedPoint::ZERO, |a, b| a + b);

                    let margin_to_release = if !order_rests {
                        prep.margin_reserved
                    } else if filled_qty > FixedPoint::ZERO {
                        // A5: telescoping release — reserved minus the reserve
                        // still owed for the resting remainder, so the later
                        // releases of that remainder (maker fills / cancel)
                        // sum to EXACTLY the original reservation (no dust).
                        prep.margin_reserved
                            - Self::reserve_for_qty(
                                ctx,
                                market_id,
                                prep.params.price,
                                prep.params.quantity - filled_qty,
                            )
                    } else {
                        FixedPoint::ZERO
                    };

                    if margin_to_release > FixedPoint::ZERO {
                        if let Ok(mut bal) = bal_cache.load(&ctx.positions, &prep.sender) {
                            let release = margin_to_release.min(bal.order_margin);
                            bal.order_margin -= release;
                            bal.available += release;
                            bal_cache.set(&prep.sender, bal);
                        }
                    }
                }

                // Apply fills through the write-back caches (C1). Positions
                // read-modify-write in pos_cache; realized-PnL events are
                // credited through bal_cache — never straight to the overlay —
                // so the old per-fill flush_and_evict (2 overlay round-trips
                // per fill) is gone and both caches stay the single in-batch
                // authority for their rows.
                let mut fill_failed = false;
                for fill in &result.fills {
                    let taker_is_buy = fill.maker_side != Side::Buy;
                    if let Err(e) = Self::apply_fill_via_caches(
                        &ctx.positions,
                        pos_cache,
                        bal_cache,
                        &fill.taker,
                        market_id,
                        taker_is_buy,
                        fill.quantity,
                        fill.price,
                    ) {
                        results[prep.index] = NativeActionResult::err(
                            "place_order",
                            format!("taker fill failed: {e}"),
                        );
                        fill_failed = true;
                        break;
                    }
                    if let Err(e) = Self::apply_fill_via_caches(
                        &ctx.positions,
                        pos_cache,
                        bal_cache,
                        &fill.maker,
                        market_id,
                        fill.maker_side == Side::Buy,
                        fill.quantity,
                        fill.price,
                    ) {
                        results[prep.index] = NativeActionResult::err(
                            "place_order",
                            format!("maker fill failed: {e}"),
                        );
                        fill_failed = true;
                        break;
                    }
                }

                if fill_failed {
                    // Funnel (perf A1): died on fill application, not on the book.
                    if let Some(ref m) = ctx.metrics {
                        m.orders_rejected_other.inc();
                    }
                    continue;
                }

                // Persist trades
                for fill in &result.fills {
                    Self::persist_trade(ctx, market_id, fill);
                }

                if let Some(ref m) = ctx.metrics {
                    m.orders_matched.inc_by(result.fills.len() as u64);
                    Self::record_order_status_funnel(m, &result.status, result.fills.len());
                }

                *total_gas += 1000;
                results[prep.index] = NativeActionResult::ok("place_order", 1000);
            }

            // A5 (maker-fill margin leak): release maker-side order margin for
            // resting orders consumed by this batch's fills, and the full
            // remaining reservation of makers STP-cancelled during matching.
            // Aggregated once per market across ALL results (a maker can be
            // consumed by several takers); amounts telescope on remaining
            // quantity so total released == total reserved (see
            // maker_margin_releases). Clamped so legacy state can't underflow.
            for (trader, amount) in
                Self::maker_margin_releases(ctx, market_id, mbr.results.iter().map(|m| &m.result))
            {
                if let Ok(mut bal) = bal_cache.load(&ctx.positions, &trader) {
                    let release = amount.min(bal.order_margin);
                    bal.order_margin -= release;
                    bal.available += release;
                    bal_cache.set(&trader, bal);
                }
            }
        }
    }

    /// C3: deterministic parallel Phase-4 settlement.
    ///
    /// Pass A (parallel, scoped thread per market — same pool shape as
    /// Phase-3 matching): a PURE compute of each market's settle plan. Workers
    /// only BORROW (`&MarketBatchResult`, `&[PreparedOrder]`, `&PositionManager`)
    /// and mutate nothing shared:
    ///   - position transitions land in a per-market `PositionCache` — position
    ///     keys are `(trader, market_id)`, so the per-market key sets are
    ///     provably disjoint and within-market application order is preserved
    ///     by the worker loop ⇒ merged caches are identical to the sequential
    ///     shared cache;
    ///   - realized-PnL events are RECORDED (not applied) in exact fill order —
    ///     `fill_transition` never reads balances, so splitting position math
    ///     from the balance credit cannot change any position outcome;
    ///   - taker/maker margin-release AMOUNTS are precomputed — they are pure
    ///     in (market config, params, match results, post-match book) and
    ///     independent of balances (only the defensive clamp reads a balance);
    ///   - trade-history rows are byte-built with a provisional index.
    ///
    /// Pass B (single-threaded): applies every cross-market mutation in
    /// EXACTLY the sequential order — markets ascending by id, orders in
    /// prepared order, PnL credits in fill order (taker then maker), then the
    /// market's A5 maker releases — so every `.min(order_margin)` clamp sees
    /// byte-identical balance state, and `trade_index` stamps the identical
    /// key sequence. Merged position caches flush later through the one
    /// sorted `flush_all`, identical to sequential.
    ///
    /// Determinism proof obligations (C3):
    ///   - fee accounting order: NO fees are touched in Phase 4
    ///     (`total_native_fees` is only read at the epoch boundary) — nothing
    ///     to order;
    ///   - realized-PnL accumulation order: position-side accumulation is
    ///     market-local (worker order == sequential order); balance-side
    ///     credits happen in pass B in the sequential order;
    ///   - margin release order (A5): all releases apply in pass B in the
    ///     sequential order with precomputed amounts.
    ///
    /// Failure semantics: a worker panic aborts NOTHING durable — since
    /// workers borrow only, no shared state has mutated, so we log and fall
    /// back to the sequential loop (which, being canonical, either succeeds
    /// or fails exactly as a sequential node would). Position-load errors
    /// inside a worker are captured per-order exactly like the sequential
    /// path. The one sequential/parallel divergence window left is a FAILING
    /// BALANCE-ROW READ at PnL-credit time (backend IO/corruption): both
    /// modes fail the order and skip its trades, but the sequential loop also
    /// stops applying that order's later-fill POSITION deltas, while the plan
    /// already computed them. A node whose balance rows fail to read is
    /// already diverging from healthy peers under sequential settle (the
    /// order soft-fails node-locally); the state root catches it either way.
    #[allow(clippy::too_many_arguments)]
    fn settle_market_results_parallel<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
        market_results: Vec<crate::market_workers::MarketBatchResult>,
        market_batches: &HashMap<MarketId, Vec<PreparedOrder<'_>>>,
        results: &mut [NativeActionResult],
        total_gas: &mut u64,
        bal_cache: &mut BalanceCache,
        pos_cache: &mut PositionCache,
    ) {
        // ---- Pass A: pure per-market plans on scoped threads ----
        let plans: Vec<Result<MarketSettlePlan, String>> = {
            let positions = &ctx.positions;
            let margin_configs = &ctx.margin_configs;
            let (block_height, timestamp) = (ctx.block_height, ctx.timestamp);
            std::thread::scope(|s| {
                let handles: Vec<_> = market_results
                    .iter()
                    .map(|mbr| {
                        let prepared: &[PreparedOrder<'_>] = market_batches
                            .get(&mbr.market_id)
                            .map(|v| v.as_slice())
                            .unwrap_or(&[]);
                        let cfg = margin_configs.get(&mbr.market_id);
                        s.spawn(move || {
                            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                Self::compute_market_settle_plan(
                                    positions,
                                    cfg,
                                    mbr,
                                    prepared,
                                    block_height,
                                    timestamp,
                                )
                            }))
                            .map_err(|payload| {
                                if let Some(s) = payload.downcast_ref::<&'static str>() {
                                    (*s).to_string()
                                } else if let Some(s) = payload.downcast_ref::<String>() {
                                    s.clone()
                                } else {
                                    "non-string panic payload".to_string()
                                }
                            })
                        })
                    })
                    .collect();
                handles
                    .into_iter()
                    .map(|h| {
                        h.join()
                            .unwrap_or_else(|_| Err("settle worker thread died".to_string()))
                    })
                    .collect()
            })
        };

        // Fallback triggers: a worker panic, or ANY position-side fill
        // application failure (backend read error — sick-node territory).
        // Workers borrowed only, so no shared state has mutated and the books
        // are still owned by `market_results` — the canonical sequential loop
        // reruns the whole settlement from scratch and is authoritative for
        // error semantics (per-order failure results, skipped trades).
        let fallback_reason = plans.iter().find_map(|p| match p {
            Err(msg) => Some(msg.clone()),
            Ok(plan) => plan
                .orders
                .iter()
                .find_map(|o| o.fill_error.clone()),
        });
        if let Some(msg) = fallback_reason {
            tracing::error!(
                error = %msg,
                "C3: parallel settle aborted (worker panic or fill failure) — falling back to sequential settlement"
            );
            return Self::settle_market_results_sequential(
                ctx,
                market_results,
                market_batches,
                results,
                total_gas,
                bal_cache,
                pos_cache,
            );
        }
        let plans: Vec<MarketSettlePlan> = plans.into_iter().map(|p| p.unwrap()).collect();

        // ---- Pass B: deterministic apply, markets ascending by id ----
        for (mbr, plan) in market_results.into_iter().zip(plans) {
            let market_id = mbr.market_id;

            // Reinsert updated book
            ctx.order_books.insert(market_id, mbr.book);
            ctx.dirty_books.insert(market_id);

            // Update global ID high-water mark
            if mbr.next_order_id > ctx.next_global_order_id {
                ctx.next_global_order_id = mbr.next_order_id;
            }

            // This market's position mutations become part of the batch cache
            // (disjoint keys across markets; single sorted flush at call end).
            pos_cache.merge_disjoint(plan.pos_cache);

            let prepared = match market_batches.get(&market_id) {
                Some(p) => p,
                None => continue,
            };

            for ((match_result, prep), oplan) in
                mbr.results.iter().zip(prepared.iter()).zip(plan.orders)
            {
                let result = &match_result.result;

                // Funnel (perf A1): STP maker cancels already happened in the
                // book during matching — same site as sequential.
                if let Some(ref m) = ctx.metrics {
                    m.orders_self_trade_cancels
                        .inc_by(result.self_trade_cancels.len() as u64);
                }

                // Taker-side margin release (amount precomputed; clamp here,
                // where the balance authority lives).
                if oplan.margin_release > FixedPoint::ZERO {
                    if let Ok(mut bal) = bal_cache.load(&ctx.positions, &prep.sender) {
                        let release = oplan.margin_release.min(bal.order_margin);
                        bal.order_margin -= release;
                        bal.available += release;
                        bal_cache.set(&prep.sender, bal);
                    }
                }

                // Realized-PnL credits, in exact fill order. Sequential
                // interleaves balance credits with position application, so a
                // balance-row READ failure at event j fails the order AT j —
                // before any later (position-side) failure the worker may have
                // recorded. The worker's event stream already stops at its own
                // position failure, so walking it in order and failing on the
                // first balance error reproduces the sequential outcome
                // exactly; if no balance error occurs, the worker's position
                // failure (if any) stands.
                let mut fill_failed: Option<String> = None;
                for (side, trader, pnl) in &oplan.pnl_events {
                    match bal_cache.load(&ctx.positions, trader) {
                        Ok(mut bal) => {
                            bal.available += *pnl;
                            bal_cache.set(trader, bal);
                        }
                        Err(e) => {
                            fill_failed = Some(format!("{side} fill failed: {e}"));
                            break;
                        }
                    }
                }
                if fill_failed.is_none() {
                    fill_failed = oplan.fill_error;
                }

                if let Some(err) = fill_failed {
                    results[prep.index] = NativeActionResult::err("place_order", err);
                    // Funnel (perf A1): died on fill application, not on the book.
                    if let Some(ref m) = ctx.metrics {
                        m.orders_rejected_other.inc();
                    }
                    continue;
                }

                // Persist trades: stamp the definitive per-block index into
                // the worker-built bytes, then route exactly like persist_trade.
                for mut kvs in oplan.trades {
                    kvs.stamp_trade_index(ctx.trade_index);
                    Self::route_trade_kvs(ctx, kvs);
                    ctx.trade_index += 1;
                }

                if let Some(ref m) = ctx.metrics {
                    m.orders_matched.inc_by(result.fills.len() as u64);
                    Self::record_order_status_funnel(m, &result.status, result.fills.len());
                }

                *total_gas += 1000;
                results[prep.index] = NativeActionResult::ok("place_order", 1000);
            }

            // A5 maker/STP releases — amounts precomputed by the worker in the
            // canonical order; clamps applied here against live balances.
            for (trader, amount) in plan.maker_releases {
                if let Ok(mut bal) = bal_cache.load(&ctx.positions, &trader) {
                    let release = amount.min(bal.order_margin);
                    bal.order_margin -= release;
                    bal.available += release;
                    bal_cache.set(&trader, bal);
                }
            }
        }
    }

    /// C3 pass-A worker: compute one market's settlement plan. PURE with
    /// respect to shared state — reads positions through a fresh per-market
    /// cache, mutates only plan-local data. Mirrors the sequential loop's
    /// per-order semantics exactly (see `settle_market_results_sequential`).
    fn compute_market_settle_plan<T: StateBackend>(
        positions: &PositionManager<T>,
        cfg: Option<&MarketMarginConfig>,
        mbr: &crate::market_workers::MarketBatchResult,
        prepared: &[PreparedOrder<'_>],
        block_height: u64,
        timestamp: u64,
    ) -> MarketSettlePlan {
        let market_id = mbr.market_id;
        let mut pos_cache = PositionCache::new();
        let mut orders = Vec::with_capacity(prepared.len());

        for (match_result, prep) in mbr.results.iter().zip(prepared.iter()) {
            let result = &match_result.result;

            // Taker-side release amount — same formula as sequential.
            let mut margin_release = FixedPoint::ZERO;
            if prep.margin_reserved > FixedPoint::ZERO {
                let order_rests = matches!(
                    result.status,
                    OrderStatus::Resting
                        | OrderStatus::PartiallyFilled
                        | OrderStatus::PendingTrigger
                );
                let filled_qty: FixedPoint = result
                    .fills
                    .iter()
                    .map(|f| f.quantity)
                    .fold(FixedPoint::ZERO, |a, b| a + b);

                margin_release = if !order_rests {
                    prep.margin_reserved
                } else if filled_qty > FixedPoint::ZERO {
                    prep.margin_reserved
                        - Self::reserve_for_qty_cfg(
                            cfg,
                            prep.params.price,
                            prep.params.quantity - filled_qty,
                        )
                } else {
                    FixedPoint::ZERO
                };
            }

            // Fill application: position transitions into the market-local
            // cache; PnL events recorded in exact order; first POSITION-side
            // failure stops the order like sequential (its message matches).
            let mut pnl_events: Vec<(&'static str, Address, FixedPoint)> = Vec::new();
            let mut fill_error: Option<String> = None;
            'fills: for fill in &result.fills {
                let taker_is_buy = fill.maker_side != Side::Buy;
                for (side, trader, is_buy) in [
                    ("taker", &fill.taker, taker_is_buy),
                    ("maker", &fill.maker, fill.maker_side == Side::Buy),
                ] {
                    match positions.apply_fill_cached(
                        &mut pos_cache,
                        trader,
                        market_id,
                        is_buy,
                        fill.quantity,
                        fill.price,
                        MarginType::Cross,
                    ) {
                        Ok(Some(pnl)) => pnl_events.push((side, *trader, pnl)),
                        Ok(None) => {}
                        Err(e) => {
                            fill_error = Some(format!("{side} fill failed: {e}"));
                            break 'fills;
                        }
                    }
                }
            }

            // Trade rows (skipped wholesale on a failed order, like sequential;
            // the definitive index is stamped in pass B).
            let trades: Vec<TradeKvs> = if fill_error.is_none() {
                result
                    .fills
                    .iter()
                    .map(|f| TradeKvs::build(market_id, block_height, timestamp, 0, f))
                    .collect()
            } else {
                Vec::new()
            };

            orders.push(OrderSettlePlan {
                margin_release,
                pnl_events,
                fill_error,
                trades,
            });
        }

        // A5 maker/STP release amounts against the POST-MATCH book (the same
        // object the sequential loop reads back out of ctx.order_books).
        let maker_releases = Self::maker_margin_releases_cfg(
            cfg,
            Some(&mbr.book),
            mbr.results.iter().map(|m| &m.result),
        );

        MarketSettlePlan {
            orders,
            pos_cache,
            maker_releases,
        }
    }

    /// C1: apply one side of a fill entirely through the per-batch write-back
    /// caches. The position read-modify-write hits `pos_cache`; if the fill
    /// had a close component, `apply_fill_cached` returns the realized PnL and
    /// it is credited through `bal_cache` (exactly when the classic
    /// `apply_fill` would have written the balance row — including a zero PnL,
    /// which still materializes the row). No overlay access on the hot path.
    #[allow(clippy::too_many_arguments)]
    fn apply_fill_via_caches<T: StateBackend>(
        positions: &PositionManager<T>,
        pos_cache: &mut PositionCache,
        bal_cache: &mut BalanceCache,
        trader: &Address,
        market_id: MarketId,
        is_buy: bool,
        qty: FixedPoint,
        price: FixedPoint,
    ) -> Result<(), CoreError> {
        if let Some(pnl) = positions.apply_fill_cached(
            pos_cache,
            trader,
            market_id,
            is_buy,
            qty,
            price,
            MarginType::Cross,
        )? {
            let mut bal = bal_cache.load(positions, trader)?;
            bal.available += pnl;
            bal_cache.set(trader, bal);
        }
        Ok(())
    }

    // ========================================================================
    // Order book handlers
    // ========================================================================

    /// Funnel-truth (perf A1): classify one matching-engine outcome into the
    /// order-funnel counters. Called exactly once per PlaceOrder that reached
    /// the book AND settled (margin rejects, balance errors and fill-application
    /// failures are counted at their own sites as `orders_rejected_margin` /
    /// `orders_rejected_other`). Observability only: nothing in execution reads
    /// these counters back.
    fn record_order_status_funnel(
        m: &torus_telemetry::Metrics,
        status: &OrderStatus,
        fill_count: usize,
    ) {
        match status {
            OrderStatus::Filled => {
                m.orders_placed_accepted.inc();
            }
            OrderStatus::Resting | OrderStatus::PartiallyFilled => {
                m.orders_placed_accepted.inc();
                m.orders_resting.inc();
            }
            OrderStatus::Rejected => {
                m.orders_rejected_book.inc();
            }
            OrderStatus::Cancelled => {
                if fill_count > 0 {
                    // IOC/Market remainder cancelled after real fills — the
                    // filled part DID trade, never count it as a dead order.
                    m.orders_cancelled_partial_fill.inc();
                } else {
                    m.orders_rejected_cancelled.inc();
                }
            }
            // Stop order queued for trigger: neither accepted onto the book
            // nor dead — it re-enters the matching funnel when triggered.
            OrderStatus::PendingTrigger => {}
        }
    }

    /// A5 (maker-fill margin leak): THE reserve/release formula.
    ///
    /// Margin reserved for `qty` of a limit order at `price` in `market_id`:
    /// `notional / effective_max_leverage(notional)` (default 20x), FixedPoint
    /// truncating arithmetic — byte-identical to the Phase-2 reserve.
    ///
    /// Exactness invariant: an order resting with remaining quantity `r` has
    /// outstanding reservation EXACTLY `reserve(price, r)`. Every release is
    /// computed as a difference of this function at two quantities
    /// (telescoping), so over an order's whole lifetime
    /// Σ releases == the original reservation — no truncation dust stranded
    /// in `order_margin`, no over-release into other orders' reservations.
    fn reserve_for_qty<T: StateBackend>(
        ctx: &NativeExecContext<T>,
        market_id: MarketId,
        price: FixedPoint,
        qty: FixedPoint,
    ) -> FixedPoint {
        Self::reserve_for_qty_cfg(ctx.margin_configs.get(&market_id), price, qty)
    }

    /// C3: ctx-free core of [`reserve_for_qty`] — pure in (market margin
    /// config, price, qty), so settle-plan workers can compute release
    /// amounts off-thread with byte-identical arithmetic.
    fn reserve_for_qty_cfg(
        cfg: Option<&MarketMarginConfig>,
        price: FixedPoint,
        qty: FixedPoint,
    ) -> FixedPoint {
        if price <= FixedPoint::ZERO || qty <= FixedPoint::ZERO {
            return FixedPoint::ZERO;
        }
        let notional = price * qty;
        let max_lev = cfg
            .map(|c| effective_max_leverage(&c.tiers, notional))
            .unwrap_or(20);
        notional / FixedPoint::from_raw(max_lev as i128 * FixedPoint::SCALE)
    }

    /// A5: margin releases owed to RESTING (maker) orders that were consumed
    /// by fills or STP-cancelled during matching of `results` in `market_id`.
    /// Returns deterministic `(trader, amount)` pairs (BTreeMap order-id order,
    /// then STP order-id order).
    ///
    /// Per maker order the release telescopes over remaining quantity:
    /// `reserve(r_before_fills) - reserve(r_after_fills)`, where the
    /// post-fill remainder comes from the book (still resting), the captured
    /// STP-cancelled order (remaining at cancel time), or zero (fully filled).
    /// STP-cancelled makers additionally release `reserve(remaining_at_cancel)`
    /// — their whole leftover reservation. Combined with the taker-side and
    /// cancel-path releases this makes total released == total reserved.
    ///
    /// MUST be called ONCE per market over ALL of the batch's results: fills
    /// from several takers consuming one maker order have to be aggregated
    /// before comparing against the book's final remaining quantity.
    fn maker_margin_releases<'a, T: StateBackend>(
        ctx: &NativeExecContext<T>,
        market_id: MarketId,
        results: impl Iterator<Item = &'a PlaceResult>,
    ) -> Vec<(Address, FixedPoint)> {
        Self::maker_margin_releases_cfg(
            ctx.margin_configs.get(&market_id),
            ctx.order_books.get(&market_id),
            results,
        )
    }

    /// C3: ctx-free core of [`maker_margin_releases`] — pure in (market
    /// margin config, post-match book, match results). Settle-plan workers
    /// call it with the worker-owned post-match book (the same object the
    /// sequential loop reads back out of `ctx.order_books` after reinsertion).
    fn maker_margin_releases_cfg<'a>(
        cfg: Option<&MarketMarginConfig>,
        book: Option<&OrderBook>,
        results: impl Iterator<Item = &'a PlaceResult>,
    ) -> Vec<(Address, FixedPoint)> {
        use std::collections::BTreeMap;

        // maker order id -> (maker, resting price, total qty consumed by fills)
        let mut consumed: BTreeMap<OrderId, (Address, FixedPoint, FixedPoint)> = BTreeMap::new();
        // STP-cancelled maker id -> (trader, price, remaining_qty at cancel)
        let mut stp: BTreeMap<OrderId, (Address, FixedPoint, FixedPoint)> = BTreeMap::new();

        for r in results {
            for f in &r.fills {
                let e = consumed
                    .entry(f.maker_order_id)
                    .or_insert((f.maker, f.price, FixedPoint::ZERO));
                e.2 += f.quantity;
            }
            for o in &r.self_trade_cancels {
                stp.insert(o.id, (o.trader, o.price, o.remaining_qty));
            }
        }
        if consumed.is_empty() && stp.is_empty() {
            return Vec::new();
        }

        let mut out = Vec::with_capacity(consumed.len() + stp.len());

        for (&order_id, &(maker, price, qty_consumed)) in &consumed {
            // Remaining AFTER all of this batch's fills on the order: still on
            // the book, or captured at STP-cancel time, or 0 (fully filled).
            let remaining_after = book
                .and_then(|b| b.get_order(order_id))
                .map(|o| o.remaining_qty)
                .or_else(|| stp.get(&order_id).map(|&(_, _, rem)| rem))
                .unwrap_or(FixedPoint::ZERO);
            let release = Self::reserve_for_qty_cfg(cfg, price, remaining_after + qty_consumed)
                - Self::reserve_for_qty_cfg(cfg, price, remaining_after);
            if release > FixedPoint::ZERO {
                out.push((maker, release));
            }
        }
        for (&_order_id, &(trader, price, remaining)) in &stp {
            // Full leftover reservation of the STP-cancelled maker (its fills
            // earlier in the batch, if any, are covered by the pass above).
            let release = Self::reserve_for_qty_cfg(cfg, price, remaining);
            if release > FixedPoint::ZERO {
                out.push((trader, release));
            }
        }
        out
    }

    fn exec_place_order<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
        sender: &Address,
        params: &PlaceOrderParams,
    ) -> NativeActionResult {
        let market_id = params.market_id;
        let is_market = matches!(params.order_type, OrderType::Market);

        // FIX 2 (ECON-FIND-05): Reserve order margin before placing the order.
        // For market orders, margin is settled at fill time (no resting order).
        // A5: reserve and every later release share reserve_for_qty.
        let order_margin_required = if !is_market {
            Self::reserve_for_qty(ctx, market_id, params.price, params.quantity)
        } else {
            FixedPoint::ZERO
        };

        if order_margin_required > FixedPoint::ZERO {
            match ctx.positions.get_native_balance(sender) {
                Ok(mut bal) => {
                    if bal.available < order_margin_required {
                        // Funnel (perf A1): died pre-book on the margin reserve.
                        if let Some(ref m) = ctx.metrics {
                            m.orders_rejected_margin.inc();
                        }
                        return NativeActionResult::err(
                            "place_order",
                            format!(
                                "insufficient margin: need {order_margin_required}, have {}",
                                bal.available
                            ),
                        );
                    }
                    bal.available -= order_margin_required;
                    bal.order_margin += order_margin_required;
                    if let Err(e) = ctx.positions.put_native_balance(sender, &bal) {
                        // Funnel (perf A1): died pre-book on a balance write error.
                        if let Some(ref m) = ctx.metrics {
                            m.orders_rejected_other.inc();
                        }
                        return NativeActionResult::err("place_order", e.to_string());
                    }
                }
                Err(e) => {
                    // Funnel (perf A1): died pre-book on a balance read error.
                    if let Some(ref m) = ctx.metrics {
                        m.orders_rejected_other.inc();
                    }
                    return NativeActionResult::err("place_order", e.to_string());
                }
            }
        }

        // Get or create order book for this market.
        ctx.dirty_books.insert(market_id);
        let book = ctx
            .order_books
            .entry(market_id)
            .or_insert_with(|| OrderBook::new(market_id, FixedPoint::ONE, FixedPoint::ONE));

        // FIX 6 (ECON-FIND-09): Sync global order ID counter to prevent cross-market collisions.
        book.set_next_order_id(ctx.next_global_order_id);
        let result = book.place_order(params.clone(), *sender, ctx.timestamp);
        ctx.next_global_order_id = book.next_order_id();

        // Funnel (perf A1): STP maker cancels already happened in the book,
        // regardless of how settlement below turns out.
        if let Some(ref m) = ctx.metrics {
            m.orders_self_trade_cancels
                .inc_by(result.self_trade_cancels.len() as u64);
        }

        // FIX 2: Release margin for the filled portion; keep reserved for resting.
        if order_margin_required > FixedPoint::ZERO {
            let order_rests = matches!(
                result.status,
                OrderStatus::Resting | OrderStatus::PartiallyFilled | OrderStatus::PendingTrigger
            );
            let filled_qty: FixedPoint = result
                .fills
                .iter()
                .map(|f| f.quantity)
                .fold(FixedPoint::ZERO, |a, b| a + b);

            let margin_to_release = if !order_rests {
                // Fully filled, cancelled, or rejected — release all reserved margin
                order_margin_required
            } else if filled_qty > FixedPoint::ZERO {
                // A5: telescoping release — reserved minus the reserve still
                // owed for the resting remainder, so the later releases of that
                // remainder (maker fills / cancel) sum to EXACTLY the original
                // reservation (no truncation dust).
                order_margin_required
                    - Self::reserve_for_qty(
                        ctx,
                        market_id,
                        params.price,
                        params.quantity - filled_qty,
                    )
            } else {
                FixedPoint::ZERO
            };

            if margin_to_release > FixedPoint::ZERO {
                if let Ok(mut bal) = ctx.positions.get_native_balance(sender) {
                    let release = margin_to_release.min(bal.order_margin);
                    bal.order_margin -= release;
                    bal.available += release;
                    let _ = ctx.positions.put_native_balance(sender, &bal);
                }
            }
        }

        // A5 (maker-fill margin leak): release maker-side order margin consumed
        // by this order's fills, and the full remaining reservation of makers
        // STP-cancelled during matching — mirrors execute_batch Phase 4.
        for (trader, amount) in
            Self::maker_margin_releases(ctx, market_id, std::iter::once(&result))
        {
            if let Ok(mut bal) = ctx.positions.get_native_balance(&trader) {
                let release = amount.min(bal.order_margin);
                bal.order_margin -= release;
                bal.available += release;
                let _ = ctx.positions.put_native_balance(&trader, &bal);
            }
        }

        // Apply fills to position manager.
        // FIX 22 (ECON-FIND-23): Propagate fill errors instead of discarding them.
        for fill in &result.fills {
            let taker_is_buy = fill.maker_side != Side::Buy;
            if let Err(e) = ctx.positions.apply_fill(
                &fill.taker,
                market_id,
                taker_is_buy,
                fill.quantity,
                fill.price,
                MarginType::Cross,
            ) {
                // Funnel (perf A1): died on fill application, not on the book.
                if let Some(ref m) = ctx.metrics {
                    m.orders_rejected_other.inc();
                }
                return NativeActionResult::err("place_order", format!("taker fill failed: {e}"));
            }
            if let Err(e) = ctx.positions.apply_fill(
                &fill.maker,
                market_id,
                fill.maker_side == Side::Buy,
                fill.quantity,
                fill.price,
                MarginType::Cross,
            ) {
                // Funnel (perf A1): died on fill application, not on the book.
                if let Some(ref m) = ctx.metrics {
                    m.orders_rejected_other.inc();
                }
                return NativeActionResult::err("place_order", format!("maker fill failed: {e}"));
            }
        }

        // Persist trades to CF_NATIVE_TRADES and CF_NATIVE_USER_TRADES.
        // These CFs are NOT in the state root, so writes cannot affect consensus.
        for fill in &result.fills {
            Self::persist_trade(ctx, market_id, fill);
        }

        if let Some(ref m) = ctx.metrics {
            m.orders_matched.inc_by(result.fills.len() as u64);
            Self::record_order_status_funnel(m, &result.status, result.fills.len());
        }

        NativeActionResult::ok("place_order", 1000)
    }

    /// Persist a single fill to CF_NATIVE_TRADES and CF_NATIVE_USER_TRADES —
    /// inline, or buffered for a background writer when `ctx.defer_trades`
    /// (O3; both CFs are node-local, outside the native consensus root).
    fn persist_trade<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
        market_id: MarketId,
        fill: &torus_core::order_book::Fill,
    ) {
        let kvs = TradeKvs::build(
            market_id,
            ctx.block_height,
            ctx.timestamp,
            ctx.trade_index,
            fill,
        );
        Self::route_trade_kvs(ctx, kvs);
        ctx.trade_index += 1;
    }

    /// Route one fill's trade rows: buffered under `defer_trades` (O3), else
    /// inline overlay PUTs — exactly the classic persist_trade tail.
    fn route_trade_kvs<T: StateBackend>(ctx: &mut NativeExecContext<T>, kvs: TradeKvs) {
        if ctx.defer_trades {
            ctx.pending_trades
                .push((CF_NATIVE_TRADES, kvs.trade_key.to_vec(), kvs.trade_data));
            ctx.pending_trades
                .push((CF_NATIVE_USER_TRADES, kvs.maker_key.to_vec(), kvs.maker_data));
            ctx.pending_trades
                .push((CF_NATIVE_USER_TRADES, kvs.taker_key.to_vec(), kvs.taker_data));
        } else {
            let _ = ctx
                .state
                .put_cf_raw(CF_NATIVE_TRADES, &kvs.trade_key, &kvs.trade_data);
            let _ = ctx
                .state
                .put_cf_raw(CF_NATIVE_USER_TRADES, &kvs.maker_key, &kvs.maker_data);
            let _ = ctx
                .state
                .put_cf_raw(CF_NATIVE_USER_TRADES, &kvs.taker_key, &kvs.taker_data);
        }
    }

    /// FIX CONS-FIND-30: Ownership check added -- only the order's trader can cancel.
    fn exec_cancel_order<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
        sender: &Address,
        order_id: u128,
    ) -> NativeActionResult {
        // Check ownership before cancelling (cheaper than cancel + re-insert).
        for book in ctx.order_books.values() {
            if let Some(order) = book.get_order(order_id) {
                if order.trader != *sender {
                    return NativeActionResult::err(
                        "cancel_order",
                        format!(
                            "order {order_id} belongs to {}, not sender {sender}",
                            order.trader
                        ),
                    );
                }
                break;
            }
        }
        for book in ctx.order_books.values_mut() {
            if let Ok(cancelled) = book.cancel_order(order_id) {
                // FIX 2 (ECON-FIND-05): Release order margin on cancel.
                let notional = cancelled.price * cancelled.remaining_qty;
                let market_id = book.market_id;
                ctx.dirty_books.insert(market_id);
                let max_lev = ctx
                    .margin_configs
                    .get(&market_id)
                    .map(|c| effective_max_leverage(&c.tiers, notional))
                    .unwrap_or(20);
                let lev_fp = FixedPoint::from_raw(max_lev as i128 * FixedPoint::SCALE);
                let margin_to_release = notional / lev_fp;

                if margin_to_release > FixedPoint::ZERO {
                    if let Ok(mut bal) = ctx.positions.get_native_balance(&cancelled.trader) {
                        let release = margin_to_release.min(bal.order_margin);
                        bal.order_margin -= release;
                        bal.available += release;
                        let _ = ctx.positions.put_native_balance(&cancelled.trader, &bal);
                    }
                }
                return NativeActionResult::ok("cancel_order", 500);
            }
        }
        NativeActionResult::err("cancel_order", format!("order {order_id} not found"))
    }

    fn exec_cancel_all<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
        sender: &Address,
        market_id: Option<MarketId>,
    ) -> NativeActionResult {
        // FIX 2 (ECON-FIND-05): Compute total margin to release from cancelled orders.
        let mut total_margin_release = FixedPoint::ZERO;

        match market_id {
            Some(mid) => {
                if let Some(book) = ctx.order_books.get_mut(&mid) {
                    let cancelled = book.cancel_all(*sender, Some(mid));
                    if !cancelled.is_empty() {
                        ctx.dirty_books.insert(mid);
                    }
                    for order in &cancelled {
                        let notional = order.price * order.remaining_qty;
                        let max_lev = ctx
                            .margin_configs
                            .get(&mid)
                            .map(|c| effective_max_leverage(&c.tiers, notional))
                            .unwrap_or(20);
                        let lev_fp = FixedPoint::from_raw(max_lev as i128 * FixedPoint::SCALE);
                        total_margin_release += notional / lev_fp;
                    }
                }
            }
            None => {
                let market_ids: Vec<MarketId> = ctx.order_books.keys().copied().collect();
                for mid in market_ids {
                    if let Some(book) = ctx.order_books.get_mut(&mid) {
                        let cancelled = book.cancel_all(*sender, None);
                        if !cancelled.is_empty() {
                            ctx.dirty_books.insert(mid);
                        }
                        for order in &cancelled {
                            let notional = order.price * order.remaining_qty;
                            let max_lev = ctx
                                .margin_configs
                                .get(&mid)
                                .map(|c| effective_max_leverage(&c.tiers, notional))
                                .unwrap_or(20);
                            let lev_fp = FixedPoint::from_raw(max_lev as i128 * FixedPoint::SCALE);
                            total_margin_release += notional / lev_fp;
                        }
                    }
                }
            }
        }

        if total_margin_release > FixedPoint::ZERO {
            if let Ok(mut bal) = ctx.positions.get_native_balance(sender) {
                let release = total_margin_release.min(bal.order_margin);
                bal.order_margin -= release;
                bal.available += release;
                let _ = ctx.positions.put_native_balance(sender, &bal);
            }
        }

        NativeActionResult::ok("cancel_all", 500)
    }

    fn exec_modify_order<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
        order_id: u128,
        new_price: Option<FixedPoint>,
        new_qty: Option<FixedPoint>,
    ) -> NativeActionResult {
        for book in ctx.order_books.values_mut() {
            // Capture old order state for margin delta calculation.
            let old_order = book.get_order(order_id).cloned();
            if let Ok(modified) = book.modify_order(order_id, new_price, new_qty) {
                ctx.dirty_books.insert(book.market_id);
                // FIX 2 (ECON-FIND-05): Adjust order margin for the modified order.
                if let Some(old) = old_order {
                    let market_id = book.market_id;
                    let old_notional = old.price * old.remaining_qty;
                    let new_notional = modified.price * modified.remaining_qty;

                    let max_lev_old = ctx
                        .margin_configs
                        .get(&market_id)
                        .map(|c| effective_max_leverage(&c.tiers, old_notional))
                        .unwrap_or(20);
                    let max_lev_new = ctx
                        .margin_configs
                        .get(&market_id)
                        .map(|c| effective_max_leverage(&c.tiers, new_notional))
                        .unwrap_or(20);

                    let old_margin = old_notional
                        / FixedPoint::from_raw(max_lev_old as i128 * FixedPoint::SCALE);
                    let new_margin = new_notional
                        / FixedPoint::from_raw(max_lev_new as i128 * FixedPoint::SCALE);

                    if new_margin > old_margin {
                        let delta = new_margin - old_margin;
                        if let Ok(mut bal) = ctx.positions.get_native_balance(&modified.trader) {
                            if bal.available < delta {
                                return NativeActionResult::err(
                                    "modify_order",
                                    format!(
                                        "insufficient margin for modify: need {delta}, have {}",
                                        bal.available
                                    ),
                                );
                            }
                            bal.available -= delta;
                            bal.order_margin += delta;
                            let _ = ctx.positions.put_native_balance(&modified.trader, &bal);
                        }
                    } else if old_margin > new_margin {
                        let delta = old_margin - new_margin;
                        if let Ok(mut bal) = ctx.positions.get_native_balance(&modified.trader) {
                            let release = delta.min(bal.order_margin);
                            bal.order_margin -= release;
                            bal.available += release;
                            let _ = ctx.positions.put_native_balance(&modified.trader, &bal);
                        }
                    }
                }
                return NativeActionResult::ok("modify_order", 800);
            }
        }
        NativeActionResult::err("modify_order", format!("order {order_id} not found"))
    }

    // ========================================================================
    // Staking handlers
    // ========================================================================

    fn exec_delegate<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
        sender: &Address,
        validator: &Address,
        amount: U256,
    ) -> NativeActionResult {
        match ctx.staking.delegate(*sender, *validator, amount) {
            Ok(()) => NativeActionResult::ok("delegate", 2000),
            Err(e) => NativeActionResult::err("delegate", e.to_string()),
        }
    }

    fn exec_undelegate<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
        sender: &Address,
        validator: &Address,
        amount: U256,
    ) -> NativeActionResult {
        match ctx
            .staking
            .undelegate(*sender, *validator, amount, ctx.block_height)
        {
            Ok(()) => NativeActionResult::ok("undelegate", 2000),
            Err(e) => NativeActionResult::err("undelegate", e.to_string()),
        }
    }

    fn exec_permanent_stake<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
        sender: &Address,
        amount: U256,
    ) -> NativeActionResult {
        match ctx
            .staking
            .permanent_stake(*sender, amount, ctx.block_height)
        {
            Ok(()) => NativeActionResult::ok("permanent_stake", 2000),
            Err(e) => NativeActionResult::err("permanent_stake", e.to_string()),
        }
    }

    fn exec_claim_rewards<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
        sender: &Address,
    ) -> NativeActionResult {
        match ctx.staking.claim_rewards(*sender) {
            Ok(_) => NativeActionResult::ok("claim_rewards", 1500),
            Err(e) => NativeActionResult::err("claim_rewards", e.to_string()),
        }
    }

    fn exec_jail_vote<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
        sender: &Address,
        target: &Address,
    ) -> NativeActionResult {
        match ctx
            .staking
            .record_jail_vote(*sender, *target, ctx.block_height)
        {
            Ok(jailed) => {
                let gas = if jailed { 5000 } else { 2000 };
                NativeActionResult::ok("jail_vote", gas)
            }
            Err(e) => NativeActionResult::err("jail_vote", e.to_string()),
        }
    }

    fn exec_unjail_self<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
        sender: &Address,
    ) -> NativeActionResult {
        match ctx.staking.unjail(sender, ctx.block_height) {
            Ok(()) => NativeActionResult::ok("unjail_self", 2000),
            Err(e) => NativeActionResult::err("unjail_self", e.to_string()),
        }
    }

    fn exec_register_validator<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
        sender: &Address,
        pubkey: &torus_types::PublicKey,
        commission: u16,
    ) -> NativeActionResult {
        // Phase 3 (3.2.1): Check governance whitelist before registration
        match ctx.governance.is_whitelisted(sender, ctx.block_height) {
            Ok(true) => {}
            Ok(false) => {
                return NativeActionResult::err(
                    "register_validator",
                    format!("validator registration not approved by governance for {sender}"),
                );
            }
            Err(e) => return NativeActionResult::err("register_validator", e.to_string()),
        }

        // Determine self-stake: sender's full balance is used as self-stake
        let self_stake = match ctx.state.get_account(sender) {
            Ok(Some(acct)) => acct.balance,
            Ok(None) => {
                return NativeActionResult::err(
                    "register_validator",
                    "sender account not found".to_string(),
                );
            }
            Err(e) => return NativeActionResult::err("register_validator", e.to_string()),
        };

        match ctx
            .staking
            .register_validator(*sender, pubkey.0, commission, self_stake)
        {
            Ok(()) => {
                // FIX ECON-FIND-21: Whitelist consume failure must fail the registration
                // to prevent a validator from registering without consuming their whitelist slot.
                if let Err(e) = ctx.governance.consume_whitelist(sender) {
                    tracing::error!(%sender, %e, "whitelist consumption failed after registration");
                    return NativeActionResult::err(
                        "register_validator",
                        format!("registered but whitelist error: {e}"),
                    );
                }
                NativeActionResult::ok("register_validator", 5000)
            }
            Err(e) => NativeActionResult::err("register_validator", e.to_string()),
        }
    }

    fn exec_update_commission<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
        sender: &Address,
        new_rate: u16,
    ) -> NativeActionResult {
        match ctx
            .staking
            .update_commission(*sender, new_rate, ctx.block_height)
        {
            Ok(()) => NativeActionResult::ok("update_commission", 2000),
            Err(e) => NativeActionResult::err("update_commission", e.to_string()),
        }
    }

    fn exec_rotate_key<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
        sender: &Address,
        new_pubkey: &torus_types::PublicKey,
    ) -> NativeActionResult {
        let current_epoch = ctx.block_height / ctx.epoch_length.max(1);
        match ctx.staking.submit_key_rotation(
            *sender,
            new_pubkey.0,
            current_epoch,
            ctx.block_height,
        ) {
            Ok(()) => NativeActionResult::ok("rotate_validator_key", 5000),
            Err(e) => NativeActionResult::err("rotate_validator_key", e.to_string()),
        }
    }

    // ========================================================================
    // Session key handlers
    // ========================================================================

    fn exec_create_session<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
        sender: &Address,
        session_pubkey: &[u8; 32],
        expiry: u64,
        scope: SessionScope,
    ) -> NativeActionResult {
        use torus_types::eip712::{MAX_SESSIONS_PER_ADDRESS, MAX_SESSION_EXPIRY_MS};

        // The block timestamp is SECONDS (header.timestamp = .as_secs()), but
        // `expiry`, MAX_SESSION_EXPIRY_MS, and order-time validation (resolve_sender
        // against current_time_ms) are all MILLISECONDS. Convert to ms here so
        // creation and usage agree — otherwise no expiry satisfies both gates and
        // every session is unusable (rejected "expiry exceeds 24h maximum" on create,
        // so orders later fail "session key not found").
        let now_ms = ctx.timestamp.saturating_mul(1000);
        if expiry > now_ms + MAX_SESSION_EXPIRY_MS {
            return NativeActionResult::err("create_session", "expiry exceeds 24h maximum".into());
        }
        if expiry <= now_ms {
            return NativeActionResult::err("create_session", "session already expired".into());
        }

        // Check max sessions per owner
        match ctx.state.count_sessions_for_owner(sender) {
            Ok(count) if count >= MAX_SESSIONS_PER_ADDRESS => {
                return NativeActionResult::err(
                    "create_session",
                    "max 5 active sessions exceeded".into(),
                );
            }
            Err(e) => {
                return NativeActionResult::err("create_session", format!("state error: {e}"));
            }
            _ => {}
        }

        // Check if session key already exists
        match ctx.state.get_session(session_pubkey) {
            Ok(Some(_)) => {
                return NativeActionResult::err(
                    "create_session",
                    "session key already exists".into(),
                );
            }
            Err(e) => {
                return NativeActionResult::err("create_session", format!("state error: {e}"));
            }
            _ => {}
        }

        // Store the session
        let data = torus_types::SessionData {
            owner: *sender,
            expiry,
            scope,
            created_at: now_ms,
        };
        match ctx.state.put_session(session_pubkey, &data) {
            Ok(()) => NativeActionResult::ok("create_session", 5000),
            Err(e) => NativeActionResult::err("create_session", format!("store failed: {e}")),
        }
    }

    fn exec_revoke_session<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
        sender: &Address,
        session_pubkey: &[u8; 32],
    ) -> NativeActionResult {
        // Verify session exists and sender is the owner
        match ctx.state.get_session(session_pubkey) {
            Ok(Some(data)) => {
                if data.owner != *sender {
                    return NativeActionResult::err("revoke_session", "not session owner".into());
                }
            }
            Ok(None) => {
                return NativeActionResult::err("revoke_session", "session not found".into());
            }
            Err(e) => {
                return NativeActionResult::err("revoke_session", format!("state error: {e}"));
            }
        }

        match ctx.state.delete_session(session_pubkey) {
            Ok(()) => NativeActionResult::ok("revoke_session", 2000),
            Err(e) => NativeActionResult::err("revoke_session", format!("delete failed: {e}")),
        }
    }

    // ========================================================================
    // Oracle handlers
    // ========================================================================

    fn exec_submit_oracle_prices<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
        sender: &Address,
        prices: &[(MarketId, FixedPoint)],
        _submission_timestamp: u64,
    ) -> NativeActionResult {
        // FIX 18 (ECON-FIND-19): Verify sender is an active, non-jailed validator.
        match ctx.staking.get_validator(sender) {
            Ok(Some(v)) => {
                use torus_economics::types::ValidatorStatus;
                if v.status != ValidatorStatus::Active {
                    return NativeActionResult::err(
                        "submit_oracle_prices",
                        format!("validator {sender} is not active (status: {:?})", v.status),
                    );
                }
            }
            Ok(None) => {
                return NativeActionResult::err(
                    "submit_oracle_prices",
                    format!("{sender} is not a registered validator"),
                );
            }
            Err(e) => {
                return NativeActionResult::err("submit_oracle_prices", e.to_string());
            }
        }

        for &(market_id, price) in prices {
            if let Err(e) =
                ctx.oracle
                    .submit_price(sender, market_id, price, ctx.block_height, ctx.timestamp)
            {
                return NativeActionResult::err("submit_oracle_prices", e.to_string());
            }
        }
        NativeActionResult::ok("submit_oracle_prices", 1000)
    }

    // ========================================================================
    // Governance handlers
    // ========================================================================

    fn exec_submit_proposal<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
        sender: &Address,
        proposal: &torus_types::Proposal,
    ) -> NativeActionResult {
        // Convert torus_types::ProposalAction to torus_economics::governance::ExecutionPayload
        use torus_economics::governance::ExecutionPayload;
        use torus_types::ProposalAction;

        let execution_payload = match &proposal.action {
            ProposalAction::ParameterChange { key, value } => {
                Some(ExecutionPayload::ParameterChange {
                    param_key: key.clone(),
                    new_value: value.clone(),
                })
            }
            ProposalAction::ListMarket(listing) => Some(ExecutionPayload::MarketListing {
                market_id: 0, // auto-assigned
                base_asset: listing.base_asset.clone(),
                quote_asset: listing.quote_asset.clone(),
                lot_size: listing.lot_size,
                tick_size: listing.tick_size,
                initial_margin: torus_types::FixedPoint::from_raw(
                    listing.maintenance_margin_bps as i128 * torus_types::FixedPoint::SCALE / 10000,
                ),
            }),
            ProposalAction::UpdateMarketParams { .. } | ProposalAction::DelistMarket { .. } => {
                None // text-only for now
            }
            ProposalAction::ValidatorRegistration { candidate } => {
                Some(ExecutionPayload::ValidatorRegistration {
                    candidate: *candidate,
                })
            }
        };

        match ctx.governance.submit_proposal(
            *sender,
            proposal.title.clone(),
            proposal.description.clone(),
            execution_payload,
            ctx.block_height,
        ) {
            Ok(_id) => NativeActionResult::ok("submit_proposal", 5000),
            Err(e) => NativeActionResult::err("submit_proposal", e.to_string()),
        }
    }

    fn exec_vote<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
        sender: &Address,
        proposal_id: u64,
        option: VoteOption,
    ) -> NativeActionResult {
        // FIX 20 (ECON-PF-03): Abstain votes are handled separately so they
        // contribute to quorum without affecting the yes/no tally.
        match option {
            VoteOption::Abstain => {
                match ctx
                    .governance
                    .cast_vote_abstain(*sender, proposal_id, ctx.block_height)
                {
                    Ok(()) => NativeActionResult::ok("vote", 2000),
                    Err(e) => NativeActionResult::err("vote", e.to_string()),
                }
            }
            _ => {
                let support = matches!(option, VoteOption::Yes);
                match ctx
                    .governance
                    .cast_vote(*sender, proposal_id, support, ctx.block_height)
                {
                    Ok(()) => NativeActionResult::ok("vote", 2000),
                    Err(e) => NativeActionResult::err("vote", e.to_string()),
                }
            }
        }
    }

    // ========================================================================
    // Lockbox handlers
    // ========================================================================

    fn exec_deposit_to_native<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
        sender: &Address,
        amount: U256,
    ) -> NativeActionResult {
        let fp_amount = match u256_to_fp(amount) {
            Some(fp) => fp,
            None => return NativeActionResult::err("deposit_to_native", "amount overflow".into()),
        };
        match Lockbox::deposit_to_native(&ctx.state, sender, fp_amount) {
            Ok(()) => NativeActionResult::ok("deposit_to_native", 1500),
            Err(e) => NativeActionResult::err("deposit_to_native", e.to_string()),
        }
    }

    fn exec_withdraw_from_native<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
        sender: &Address,
        amount: U256,
    ) -> NativeActionResult {
        let fp_amount = match u256_to_fp(amount) {
            Some(fp) => fp,
            None => {
                return NativeActionResult::err("withdraw_from_native", "amount overflow".into())
            }
        };
        match Lockbox::withdraw_from_native(&ctx.state, sender, fp_amount) {
            Ok(()) => NativeActionResult::ok("withdraw_from_native", 1500),
            Err(e) => NativeActionResult::err("withdraw_from_native", e.to_string()),
        }
    }

    /// FIX ECON-PF-17: Withdraw from sender's native balance to a specified EVM address.
    fn exec_withdraw_to<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
        sender: &Address,
        to: &Address,
        amount: U256,
    ) -> NativeActionResult {
        let fp_amount = match u256_to_fp(amount) {
            Some(fp) => fp,
            None => return NativeActionResult::err("withdraw_to", "amount overflow".into()),
        };
        match Lockbox::withdraw_from_native_to(&ctx.state, sender, to, fp_amount) {
            Ok(()) => NativeActionResult::ok("withdraw_to", 1500),
            Err(e) => NativeActionResult::err("withdraw_to", e.to_string()),
        }
    }

    // ========================================================================
    // Block-level processing helpers (called by validator pipeline)
    // ========================================================================

    /// Drain and execute CoreWriter actions queued from the previous block.
    ///
    /// Task 3.1.5 (anti-MEV): Asserts the one-block delay is enforced — all
    /// drained actions must have been queued in a strictly earlier block.
    ///
    /// FIX EVM-FIND-12: Propagates drain errors instead of silently returning empty vec.
    pub fn drain_core_writer<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
    ) -> Result<Vec<NativeActionResult>, torus_core::error::CoreError> {
        let queued = CoreWriterQueue::drain(&ctx.state, ctx.block_height)?;

        let mut results = Vec::with_capacity(queued.len());
        for qa in &queued {
            // BUG FIX (3.1): CoreWriter delay guard — skip any action that was
            // queued in the current or a future block (should never happen, but
            // guards against bypass bugs).
            if qa.block_queued >= ctx.block_height {
                tracing::error!(
                    queued_block = qa.block_queued,
                    current_block = ctx.block_height,
                    "BUG (3.1): CoreWriter action from current/future block — skipping"
                );
                results.push(NativeActionResult::err(
                    "core_writer",
                    format!(
                        "action queued in block {} cannot execute in block {}",
                        qa.block_queued, ctx.block_height
                    ),
                ));
                continue;
            }

            let action = core_writer_to_native(qa);
            let result = Self::execute(ctx, &qa.trader, &action);
            results.push(result);
        }
        Ok(results)
    }

    /// Aggregate oracle prices for listed markets after oracle submissions.
    pub fn aggregate_oracle_prices<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
        markets: &[MarketId],
        validator_stakes: &[(Address, FixedPoint)],
    ) -> Vec<NativeActionResult> {
        let mut results = Vec::new();
        for &market_id in markets {
            match ctx
                .oracle
                .aggregate_price(market_id, ctx.block_height, validator_stakes)
            {
                Ok(_) => results.push(NativeActionResult::ok("oracle_aggregate", 500)),
                Err(e) => results.push(NativeActionResult::err("oracle_aggregate", e.to_string())),
            }
        }
        results
    }

    /// Run liquidation checks across all configured markets.
    pub fn run_liquidation_checks<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
        traders: &[Address],
        oracle_prices: &[(MarketId, FixedPoint)],
    ) -> Vec<NativeActionResult> {
        let mut results = Vec::new();

        // Iterate over a snapshot of config keys to avoid borrow conflict.
        let market_configs: Vec<(MarketId, MarketMarginConfig)> = ctx
            .margin_configs
            .iter()
            .map(|(&k, v)| (k, v.clone()))
            .collect();

        for (market_id, config) in &market_configs {
            let liquidations = match LiquidationEngine::check_liquidations(
                &ctx.positions,
                traders,
                config,
                oracle_prices,
            ) {
                Ok(liqs) => liqs,
                Err(e) => {
                    results.push(NativeActionResult::err("liquidation_check", e.to_string()));
                    continue;
                }
            };

            // FIX 5 (ECON-PF-06): Skip liquidation if no valid oracle price.
            let oracle_price = match oracle_prices
                .iter()
                .find(|(mid, _)| *mid == *market_id)
                .map(|(_, p)| *p)
            {
                Some(p) if p > FixedPoint::ZERO => p,
                _ => {
                    tracing::warn!(market_id, "skipping liquidations: no valid oracle price");
                    continue;
                }
            };

            for liq in &liquidations {
                match LiquidationEngine::execute_liquidation(&ctx.positions, liq, oracle_price) {
                    Ok(lr) => {
                        if let Some(ref m) = ctx.metrics {
                            m.liquidations_triggered.inc();
                        }
                        results.push(NativeActionResult::ok("liquidation", 3000));
                        if lr.remaining_deficit > FixedPoint::ZERO {
                            let _ = LiquidationEngine::auto_deleverage(
                                &ctx.positions,
                                *market_id,
                                lr.remaining_deficit,
                                oracle_price,
                                traders,
                            );
                        }
                    }
                    Err(e) => {
                        results.push(NativeActionResult::err("liquidation", e.to_string()));
                    }
                }
            }
        }
        results
    }

    /// Distribute fees at end of block.
    pub fn distribute_fees<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
        total_evm_fees: u128,
    ) -> NativeActionResult {
        let total_fees = U256::from(ctx.total_native_fees as u128 + total_evm_fees);
        if total_fees.is_zero() {
            return NativeActionResult::ok("fee_distribution", 0);
        }

        // FIX ECON-FIND-22: Removed dead FeeSplitter::split_fees call.
        // RewardDistributor::distribute_block_fees computes fee splits internally.

        match RewardDistributor::distribute_block_fees(
            &ctx.staking,
            ctx.proposer,
            total_fees,
            ctx.epoch,
            ctx.treasury_address,
            ctx.dev_pool_address,
        ) {
            Ok(()) => NativeActionResult::ok("fee_distribution", 2000),
            Err(e) => NativeActionResult::err("fee_distribution", e.to_string()),
        }
    }

    /// Check and process epoch boundary (reward distribution + validator rotation).
    ///
    /// Phase A — Rewards: distributes permanent staking rewards and validator
    /// inflation to the CURRENT active set. Errors log-and-continue (reward
    /// bugs should not block rotation).
    ///
    /// Phase B — Rotation: computes new validator set, applies rotation cap,
    /// updates statuses, logs changes. Errors on compute_new_validator_set
    /// early-return (broken validator set is a consensus-safety issue).
    pub fn process_epoch_boundary<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
    ) -> Option<EpochBoundaryResult> {
        if !EpochManager::is_epoch_boundary(ctx.block_height, ctx.epoch_length) {
            return None;
        }

        // --- Phase A: Reward distribution (uses CURRENT active set) ---

        if let Err(e) =
            RewardDistributor::distribute_permanent_staking_rewards(&ctx.staking, ctx.epoch_length)
        {
            tracing::error!(%e, "permanent staking rewards failed");
        }

        if let Err(e) =
            RewardDistributor::distribute_validator_inflation(&ctx.staking, ctx.epoch_length)
        {
            tracing::error!(%e, "validator inflation distribution failed");
        }

        // --- Phase B: Validator set rotation ---

        // B1: Build old set from current Active validators
        let old_set = build_current_validator_set(&ctx.staking, ctx.epoch);

        // B2: Compute new set
        let new_set = match EpochManager::compute_new_validator_set(
            &ctx.staking,
            ctx.max_validators,
            ctx.epoch + 1,
        ) {
            Ok(set) => set,
            Err(e) => {
                return Some(EpochBoundaryResult {
                    action: NativeActionResult::err("epoch_rotation", e.to_string()),
                    new_set: None,
                    diff: None,
                });
            }
        };

        // B3: Apply rotation cap
        let cap = EpochManager::safe_rotation_cap(old_set.validators.len());
        let new_set = EpochManager::apply_rotation_cap(&old_set, new_set, cap);

        // B3.5 (FIX 4, S443): enforce the BFT-minimum floor. If the capped rotation
        // would drop the active set below MIN_ACTIVE_VALIDATORS while the old set met
        // it, re-seat the highest-priority departed validator(s) so the cluster keeps
        // a viable quorum (t15: a wrongful deposition dropped 4 → 3 and stalled).
        let new_set = EpochManager::enforce_minimum_floor(&old_set, new_set);

        // B4: Check minimum set (post-condition; floor guard above should keep this
        // green whenever the old set met the minimum).
        if let Err(e) = EpochManager::check_minimum_set(&new_set) {
            tracing::error!(%e, "validator set below minimum");
        }

        // B5: Compute diff
        let diff = EpochManager::compute_validator_set_diff(&old_set, &new_set);

        // B6: Update statuses
        if let Err(e) = EpochManager::update_validator_statuses(&ctx.staking, &new_set) {
            tracing::error!(%e, "validator status update failed");
        }

        // B7: Log rotation
        EpochManager::log_rotation(&old_set, &new_set, &diff, ctx.epoch + 1);

        if let Some(ref m) = ctx.metrics {
            m.epoch_number.set((ctx.epoch + 1) as i64);
            m.validator_set_size.set(new_set.validators.len() as i64);
        }

        Some(EpochBoundaryResult {
            action: NativeActionResult::ok("epoch_boundary", 5000),
            new_set: Some(new_set),
            diff: Some(diff),
        })
    }

    /// Process pending governance proposals.
    pub fn process_governance<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
    ) -> Vec<NativeActionResult> {
        match ctx.governance.process_pending_proposals(ctx.block_height) {
            Ok(outcomes) => outcomes
                .iter()
                .map(|_| NativeActionResult::ok("governance_process", 1000))
                .collect(),
            Err(e) => vec![NativeActionResult::err("governance_process", e.to_string())],
        }
    }
}

/// Build a ValidatorSet from current Active validators in staking state.
/// Uses the same power conversion as `EpochManager::compute_new_validator_set`.
fn build_current_validator_set(
    staking: &StakingManager<impl StateBackend>,
    epoch: u64,
) -> ValidatorSet {
    let wei = U256::from(10u64).pow(U256::from(18u64));
    let validators: Vec<ValidatorInfo> = match staking.all_validators() {
        Ok(all) => all
            .into_iter()
            .filter(|v| v.status == ValidatorStatus::Active)
            .map(|v| ValidatorInfo {
                address: v.address,
                pubkey: PublicKey(v.pubkey),
                power: (v.total_stake() / wei).try_into().unwrap_or(u64::MAX),
                commission_bps: v.commission_bps,
            })
            .collect(),
        Err(e) => {
            tracing::error!(%e, "failed to read validators for old set");
            Vec::new()
        }
    };
    ValidatorSet { validators, epoch }
}

// ============================================================================
// Action classification and ordering (tech-req section 2.2)
// ============================================================================

/// Categories for deterministic block execution ordering.
///
/// Ordering per tech-req:
///   1. Native cancellations
///   2. Native non-GTC orders (IOC, FOK, market)
///   3. EVM transactions (not a native category — interleaved by caller)
///   4. Native GTC limit orders
///   5. CoreWriter drain (implicit, processed by validator)
///   6. Lockbox operations
///   7. Oracle submissions
///   8. Governance actions
///   9. Liquidation checks (implicit)
///   10. Fee distribution (implicit)
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ActionCategory {
    Cancellation = 0,
    NonGtcOrder = 1,
    GtcOrder = 2,
    Lockbox = 3,
    Oracle = 4,
    Governance = 5,
    Staking = 6,
    Other = 7,
}

/// Classify a NativeAction for block ordering.
pub fn classify_action(action: &NativeAction) -> ActionCategory {
    match action {
        NativeAction::CancelOrder { .. } | NativeAction::CancelAllOrders { .. } => {
            ActionCategory::Cancellation
        }
        NativeAction::PlaceOrder(params) => match params.order_type {
            OrderType::Market | OrderType::StopMarket { .. } => ActionCategory::NonGtcOrder,
            _ => match params.time_in_force {
                TimeInForce::GTC | TimeInForce::PostOnly => ActionCategory::GtcOrder,
                TimeInForce::IOC | TimeInForce::FOK => ActionCategory::NonGtcOrder,
            },
        },
        // A batch is a unit of GTC-class limit orders (the market-maker use case); it
        // sorts alongside other GTC orders into the post-EVM phase. Internal order is
        // preserved when `execute_batch` flattens it.
        NativeAction::PlaceOrderBatch(_) => ActionCategory::GtcOrder,
        NativeAction::ModifyOrder { .. } => ActionCategory::NonGtcOrder,
        NativeAction::TransferToPerp { .. }
        | NativeAction::TransferToSpot { .. }
        | NativeAction::Withdraw { .. } => ActionCategory::Lockbox,
        NativeAction::SubmitOraclePrices(_) => ActionCategory::Oracle,
        NativeAction::SubmitProposal(_) | NativeAction::Vote { .. } => ActionCategory::Governance,
        NativeAction::Delegate { .. }
        | NativeAction::Undelegate { .. }
        | NativeAction::PermanentStake { .. }
        | NativeAction::ClaimRewards => ActionCategory::Staking,
        _ => ActionCategory::Other,
    }
}

/// Sort native actions into pre-EVM and post-EVM groups per tech-req ordering.
///
/// Pre-EVM:  cancellations, non-GTC orders
/// Post-EVM: GTC orders, lockbox, oracle, governance, staking, other
///
/// Task 3.1.5 (anti-MEV): Within each category, actions are sorted by
/// (sender_address, action_content_hash) for determinism. This prevents a
/// validator-proposer from manipulating within-category ordering by choosing
/// submission order.
#[allow(clippy::type_complexity)]
pub fn sort_native_actions(
    actions: &[(Address, NativeAction)],
) -> (Vec<(Address, NativeAction)>, Vec<(Address, NativeAction)>) {
    let mut pre_evm = Vec::new();
    let mut post_evm = Vec::new();

    for (sender, action) in actions {
        let pair = (*sender, action.clone());
        match classify_action(action) {
            ActionCategory::Cancellation | ActionCategory::NonGtcOrder => pre_evm.push(pair),
            _ => post_evm.push(pair),
        }
    }

    // Deterministic sort: (category, sender, action_content_hash).
    sort_deterministic(&mut pre_evm);
    sort_deterministic(&mut post_evm);

    (pre_evm, post_evm)
}

/// Deterministic sort key for a native action.
///
/// Uses `NativeAction::canonical_bytes()` instead of `Debug` formatting to ensure
/// the sort key is identical across compiler versions and crate updates.
fn action_sort_key(sender: &Address, action: &NativeAction) -> (ActionCategory, Address, B256) {
    let category = classify_action(action);
    let hash = alloy_primitives::keccak256(action.canonical_bytes());
    (category, *sender, hash)
}

/// Sort actions deterministically by (category, sender, action_content_hash).
fn sort_deterministic(actions: &mut Vec<(Address, NativeAction)>) {
    // Pre-compute sort keys to avoid repeated hashing during sort.
    let mut keyed: Vec<_> = actions
        .drain(..)
        .map(|(s, a)| {
            let key = action_sort_key(&s, &a);
            (key, (s, a))
        })
        .collect();
    keyed.sort_by(|a, b| a.0.cmp(&b.0));
    actions.extend(keyed.into_iter().map(|(_, pair)| pair));
}

// ============================================================================
// CoreWriter → NativeAction conversion
// ============================================================================

/// Convert a CoreWriter queued action to a NativeAction for execution.
fn core_writer_to_native(qa: &QueuedAction) -> NativeAction {
    match &qa.kind {
        QueuedActionKind::PlaceOrder {
            market_id,
            side,
            order_type,
            price,
            quantity,
            time_in_force,
        } => NativeAction::PlaceOrder(PlaceOrderParams {
            market_id: *market_id,
            is_buy: *side == 0,
            price: *price,
            quantity: *quantity,
            order_type: decode_order_type(*order_type),
            time_in_force: decode_time_in_force(*time_in_force),
            reduce_only: false,
            client_order_id: None,
        }),
        QueuedActionKind::CancelOrder { order_id } => NativeAction::CancelOrder {
            order_id: *order_id,
        },
        QueuedActionKind::CancelAll { market_id } => NativeAction::CancelAllOrders {
            market_id: Some(*market_id),
        },
        QueuedActionKind::Delegate { validator, amount } => NativeAction::Delegate {
            validator: *validator,
            amount: fp_to_u256(*amount),
        },
        QueuedActionKind::Undelegate { validator, amount } => NativeAction::Undelegate {
            validator: *validator,
            amount: fp_to_u256(*amount),
        },
        QueuedActionKind::ClaimRewards => NativeAction::ClaimRewards,
        QueuedActionKind::LockPermanent { amount } => NativeAction::PermanentStake {
            amount: fp_to_u256(*amount),
        },
    }
}

fn decode_order_type(code: u8) -> OrderType {
    match code {
        0 => OrderType::Limit,
        1 => OrderType::Market,
        _ => OrderType::Limit,
    }
}

fn decode_time_in_force(code: u8) -> TimeInForce {
    match code {
        0 => TimeInForce::GTC,
        1 => TimeInForce::IOC,
        2 => TimeInForce::FOK,
        3 => TimeInForce::PostOnly,
        _ => TimeInForce::GTC,
    }
}
