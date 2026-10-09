//! Native action execution — dispatches each NativeAction to the correct handler.
//!
//! NativeExecutor is the core dispatch layer for processing native (non-EVM) actions
//! within a block. It handles order book operations, staking, oracle, governance,
//! lockbox, and liquidation processing in a deterministic pipeline.
//!
//! Task 2.5.1: NativeExecutor dispatch table + batch execution.

use std::borrow::Cow;
use std::collections::{BTreeSet, VecDeque};
use std::sync::Arc;

use alloy_primitives::{Address, B256};
// Item 6 Phase 2 P2-5: std's HashMap with foldhash, seeded per process
// (alloy's default `map-foldhash`), instead of SipHash.
use alloy_primitives::map::{HashMap, HashSet};
use torus_core::error::CoreError;
use torus_core::lockbox::{fp_to_u256, u256_to_fp, Lockbox};
use torus_core::margin::{
    effective_max_leverage, market_margin_config, order_initial_margin, placement_need,
    position_price, position_terms, AccountView, MarginTier, MarketMarginConfig,
};
use torus_core::oracle::{OracleConfig, OracleManager};
use torus_core::order_book::{
    band_reference, market_row_shape, order_price_violation, price_band_violation, reduce_only_allowance,
    shape_violation, AccountMargins, Fill, MakerAccount, MakerAccountSource, OrderBook, OrderStatus, PlaceResult,
    PriceBand, ReduceOnlyPositions, ShapeViolation, TakerMarginLimit, TriggeredStop,
};
use torus_core::position::{
    open_order_limit, FillEffect, MarginType, NativeBalance, PositionCache, PositionManager,
    OPEN_ORDER_BASE_LIMIT,
};
use torus_core::precompiles::{CoreWriterQueue, QueuedAction, QueuedActionKind};
use torus_economics::{
    EpochManager, GovernanceManager, RewardDistributor, StakingManager,
};
use torus_state::action_status::FailureReason;
use torus_state::trade_rows::{encode_block, FillExtras, TradeFill};
use torus_state::{NativeStateOverlay, PackedCfBatch, StateBackend, StateDb};
use torus_types::{
    FixedPoint, MarketId, NativeAction, OrderId, OrderType, PlaceOrderParams,
    SessionScope, Side, TimeInForce, ValidatorSet, VoteOption, U256,
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
    /// Why it failed. Borrowed for a book rejection (its reason's
    /// [`FailureReason::book_reject_message`]: no allocation per rejected
    /// order), owned for a message built at the check.
    pub error: Option<Cow<'static, str>>,
    pub gas_used: u64,
    /// Why it failed, set where the check failed (v2 action status stores
    /// it; never derived from `error`). `Other` for a success and for any
    /// failure without a dedicated code.
    pub reason: FailureReason,
}

impl NativeActionResult {
    fn ok(action_type: &'static str, gas_used: u64) -> Self {
        Self {
            action_type,
            success: true,
            error: None,
            gas_used,
            reason: FailureReason::Other,
        }
    }

    fn err(action_type: &'static str, error: String) -> Self {
        Self::rejected(action_type, (FailureReason::Other, error))
    }

    /// A failure with its typed reason.
    fn rejected(action_type: &'static str, (reason, error): Rejection) -> Self {
        Self {
            action_type,
            success: false,
            error: Some(Cow::Owned(error)),
            gas_used: 0,
            reason,
        }
    }
}

/// A refused action: its typed reason and its message.
type Rejection = (FailureReason, String);

/// Gas of a placed order, whatever the book did with it.
const PLACE_ORDER_GAS: u64 = 1000;

impl NativeActionResult {
    /// Row 50: the result of an order the book answered, THE helper of the
    /// sequential settle, the parallel settle's pass B and the single path
    /// (`place_order_inner`). Rejected with the book's HL reason when the
    /// book refused it or cancelled it without a fill
    /// ([`PlaceResult::reject`]: IOC / market / FOK without a fill, crossing
    /// PostOnly, reduce-only that cannot reduce, margin cut before the first
    /// fill, bad stop trigger); executed otherwise — resting, filled, partly
    /// filled with the rest cancelled (HL: filled), pending trigger. Either
    /// way it costs the executed order's gas: the label changes no total.
    fn placed(result: &PlaceResult) -> Self {
        match result.reject {
            None => Self::ok("place_order", PLACE_ORDER_GAS),
            // The message is the reason's canonical one (torus-state, one
            // table for the executor and the action status record).
            Some(reason) => Self {
                action_type: "place_order",
                success: false,
                error: Some(Cow::Borrowed(reason.book_reject_message())),
                gas_used: PLACE_ORDER_GAS,
                reason,
            },
        }
    }
}

/// Result of executing a batch of native actions.
#[derive(Clone, Debug)]
pub struct NativeBatchResult {
    pub results: Vec<NativeActionResult>,
    pub total_gas: u64,
}

/// Result of epoch boundary processing: the validator set installed at this
/// boundary (`None` = unchanged), from the plan the previous boundary stored.
pub struct EpochBoundaryResult {
    pub action: NativeActionResult,
    pub new_set: Option<ValidatorSet>,
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
#[derive(Default)]
struct BalanceCache {
    map: HashMap<Address, CachedBalance>,
    dirty: Vec<Address>,
}

/// Per-call `cum_volume` increments: maker and taker each add `price * qty`
/// for every fill side whose position effect applied. Flushed with the
/// balance cache, in sorted-address order.
type VolumeCache = HashMap<Address, FixedPoint>;

/// Adds the first `sides` fill sides of `fills` (in order: fill 0 taker, fill
/// 0 maker, fill 1 taker, ...) to `volumes`, one `price * qty` per fill.
fn add_fill_volumes(volumes: &mut VolumeCache, fills: &[Fill], sides: usize) {
    for (k, fill) in fills.iter().enumerate().take(sides.div_ceil(2)) {
        let notional = fill.price * fill.quantity;
        *volumes.entry(fill.taker).or_insert(FixedPoint::ZERO) += notional;
        if 2 * k + 1 < sides {
            *volumes.entry(fill.maker).or_insert(FixedPoint::ZERO) += notional;
        }
    }
}

#[cfg(test)]
mod volume_cache_probe {
    use super::*;
    use std::time::Instant;

    fn trader(n: u64) -> Address {
        let mut b = [0u8; 20];
        b[..8].copy_from_slice(&n.to_be_bytes());
        Address::from(b)
    }

    /// The s84 cell per native block: ~58k fills over 300 markets (~195 per
    /// market); takers among the call's ~250 senders, makers among ~900 of
    /// the 5000 traders resting in each book.
    fn bench_fills() -> Vec<Vec<Fill>> {
        let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut next = |n: u64| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x % n
        };
        (0..300u64)
            .map(|m| {
                (0..195)
                    .map(|_| Fill {
                        maker_order_id: 1,
                        taker_order_id: 2,
                        price: FixedPoint::from_raw(30_000 * FixedPoint::SCALE),
                        quantity: FixedPoint::from_raw(FixedPoint::SCALE / 20),
                        maker: trader((m * 17 + next(900)) % 5000),
                        taker: trader(next(250) * 20),
                        maker_side: Side::Buy,
                        timestamp: 0,
                    })
                    .collect()
            })
            .collect()
    }

    fn median_ms(mut f: impl FnMut()) -> (f64, f64) {
        let mut ms: Vec<f64> = (0..41)
            .map(|_| {
                let t0 = Instant::now();
                f();
                t0.elapsed().as_secs_f64() * 1e3
            })
            .collect();
        ms.sort_by(f64::total_cmp);
        (ms[0], ms[20])
    }

    /// Release probe: per-market plan maps (`compute_market_settle_plan`,
    /// on the settle workers; summed over the 300 markets here) and the
    /// call's merge map (pass B), unsized vs pre-sized.
    ///   cargo test --release -p torus-bridge --lib volume_cache_probe -- --ignored --nocapture
    #[test]
    #[ignore = "release cost probe"]
    fn volume_cache_sizing_cost() {
        let fills = bench_fills();
        let plans = |sized: bool| -> Vec<VolumeCache> {
            fills
                .iter()
                .map(|f| {
                    let mut v = if sized {
                        VolumeCache::with_capacity_and_hasher(2 * f.len(), Default::default())
                    } else {
                        VolumeCache::default()
                    };
                    add_fill_volumes(&mut v, f, 2 * f.len());
                    v
                })
                .collect()
        };
        for sized in [false, true] {
            let (min, med) = median_ms(|| drop(std::hint::black_box(plans(sized))));
            println!("300 plan maps, with_capacity={sized}: min {min:.3} ms, median {med:.3} ms");
        }
        let built = plans(true);
        let distinct = {
            let mut all = VolumeCache::default();
            for p in &built {
                all.extend(p.iter().map(|(t, v)| (*t, *v)));
            }
            all.len()
        };
        let max_plan = built.iter().map(VolumeCache::len).max().unwrap_or(0);
        for (name, cap) in [("new", 0), ("max plan len", max_plan), ("distinct", distinct)] {
            let (min, med) = median_ms(|| {
                let mut merged = VolumeCache::with_capacity_and_hasher(cap, Default::default());
                for p in &built {
                    for (t, add) in p {
                        *merged.entry(*t).or_insert(FixedPoint::ZERO) += *add;
                    }
                }
                std::hint::black_box(merged);
            });
            println!(
                "merge into one map ({distinct} distinct traders), capacity {name} ({cap}): \
                 min {min:.3} ms, median {med:.3} ms"
            );
        }
    }
}

struct CachedBalance {
    balance: NativeBalance,
    dirty: bool,
}

impl BalanceCache {
    fn new() -> Self {
        Self {
            map: HashMap::default(),
            dirty: Vec::new(),
        }
    }

    /// Return the sender's balance, reading through to `positions` on a miss.
    fn load<T: StateBackend>(
        &mut self,
        positions: &PositionManager<T>,
        addr: &Address,
    ) -> Result<NativeBalance, CoreError> {
        if let Some(entry) = self.map.get(addr) {
            return Ok(entry.balance.clone());
        }
        let bal = positions.get_native_balance(addr)?;
        self.map.insert(
            *addr,
            CachedBalance {
                balance: bal.clone(),
                dirty: false,
            },
        );
        Ok(bal)
    }

    /// Update the cached balance and mark it dirty (write-back — no overlay PUT yet).
    fn set(&mut self, addr: &Address, bal: NativeBalance) {
        use std::collections::hash_map::Entry;
        match self.map.entry(*addr) {
            Entry::Occupied(mut slot) => {
                let entry = slot.get_mut();
                entry.balance = bal;
                if !entry.dirty {
                    entry.dirty = true;
                    self.dirty.push(*addr);
                }
            }
            Entry::Vacant(slot) => {
                slot.insert(CachedBalance {
                    balance: bal,
                    dirty: true,
                });
                self.dirty.push(*addr);
            }
        }
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
        self.dirty.sort_unstable();
        for addr in &self.dirty {
            if let Some(entry) = self.map.get(addr) {
                positions.put_native_balance(addr, &entry.balance)?;
            }
        }
        // A partial flush must leave EVERY entry dirty, including writes that
        // succeeded before the error. Only a wholly successful flush resets
        // the flags/list, preserving the original all-dirty retry behavior.
        for addr in self.dirty.drain(..) {
            if let Some(entry) = self.map.get_mut(&addr) {
                entry.dirty = false;
            }
        }
        Ok(())
    }
}

// Item 3 (s517): the end-of-block liquidation step (child module: sees the
// private placement / stop / cancel helpers and `AccountReader`).
#[path = "liquidation_step.rs"]
mod liquidation_step;

#[path = "trader_positions.rs"]
mod trader_positions;
use trader_positions::TraderPositions;

#[cfg(test)]
#[path = "balance_cache_tests.rs"]
mod balance_cache_tests;

#[cfg(test)]
#[path = "load_books_parallel_tests.rs"]
mod load_books_parallel_tests;

// ============================================================================
// C3 — deterministic parallel Phase-4 settlement: plumbing types
// ============================================================================

/// B-blind (s92, owner): a non-pool sell is topped up to cover bids up to
/// its market's start-of-Phase-2 best bid plus this many basis points
/// ([`NativeExecutor::sell_top_ups`]): `reserve(B0 x (1 + 10 bps), qty)`.
const SELL_TOP_UP_BPS: i128 = 10;

/// C2/C3: one Phase-2-prepared PlaceOrder flowing through matching (Phase 3)
/// and settlement (Phase 4). `params` borrows the caller's committed action
/// slice — no per-order deep clone anywhere in the pipeline.
struct PreparedOrder<'a> {
    index: usize,
    sender: Address,
    params: &'a PlaceOrderParams,
    order_id: u128,
    margin_reserved: FixedPoint,
    /// F1 (s517): `Some(UPnL − position IM)` of a checked taker's sender
    /// (pre-batch); `None` = unchecked. Its match-time budget is its own
    /// reservation + the sender's exclusive pool (D2, see Phase 3).
    checked_pos_net: Option<FixedPoint>,
    /// Item 6 M1: the sender's pre-batch (signed position, valuation price)
    /// in this market as Phase 2 read it (`position_px`; every order with
    /// an account check, so every checked taker); `None` = not read (a
    /// reduce-only order). Phase 3 polices and values from it instead of
    /// reading the position again (the backend is frozen in between).
    pre_pos: Option<(FixedPoint, FixedPoint)>,
    /// Option B (s87): a [`NativeExecutor::takes_bid_floor`] sell whose
    /// sender's D2 pool market is another market — exactly `prepare_one`'s
    /// bid-floor condition (the pool is fixed by the sender's first accepted
    /// checked order, so it is known when the order is prepared). B-blind
    /// (s92) tops these up ([`NativeExecutor::sell_top_ups`]).
    top_up_candidate: bool,
    /// s92 (observability only): the price `margin_reserved` was taken at
    /// (Phase 2's reservation price, raised to a granted top-up's price); the
    /// sell-cut counters bucket a cut by its hit price minus this.
    res_price: FixedPoint,
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
    /// error text, trader, pnl, fill side index `2 * fill + {0 taker, 1
    /// maker}`). Emitted precisely when `apply_fill_cached` returns `Some` —
    /// including `Some(ZERO)` (still materializes the row).
    pnl_events: Vec<(&'static str, Address, FixedPoint, usize)>,
    /// s80: `[taker, maker]` position effect of each applied fill, in fill
    /// order (output-only, for the fills' `FillExtras`). Only filled while a
    /// stream wants fills (`record_fills`); otherwise empty, never allocated.
    fill_effects: Vec<[FillEffect; 2]>,
    /// First position-side fill-application failure, pre-formatted like the
    /// sequential path ("taker fill failed: …" / "maker fill failed: …").
    fill_error: Option<String>,
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
    /// `cum_volume` of every fill side of this market (summed per trader on
    /// the worker). Pass B adds it whole unless an order stops early there.
    volumes: VolumeCache,
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
    /// Pinned by the caller (tests / A-B benches): `parallel = true` runs the
    /// parallel path whenever >=2 markets have work, `false` always runs the
    /// sequential loop. `workers` pins the pass-A chunk-worker cap (`None` =
    /// the production cap); it is a scheduling knob only — every value must
    /// produce byte-identical state.
    Force {
        parallel: bool,
        workers: Option<usize>,
    },
}

/// R6: pass-A settle worker cap. `TORUS_SETTLE_WORKERS` overrides, else
/// `TORUS_MATCH_WORKERS`, else host parallelism (min 1). Read per block —
/// the resolution is a couple of env lookups against a per-block cost of
/// hundreds of ms of plan compute.
fn settle_worker_cap() -> usize {
    crate::market_workers::MarketWorkerPool::resolve_worker_cap_named(Some(
        "TORUS_SETTLE_WORKERS",
    ))
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
    /// The stitch assigns the global order id and builds the `PreparedOrder`.
    Pass(PrepPass),
    /// Rejected pre-book. `funnel` selects the funnel counter; `reason` is
    /// the result's typed reason, `msg` the exact serial-path error string.
    Reject { funnel: RejectReason, reason: FailureReason, msg: String },
}

/// L3-ENG: a passed order's Phase-2 results ([`PreparedOrder`]'s fields).
struct PrepPass {
    /// Margin reserved (possibly ZERO for market orders).
    required: FixedPoint,
    /// F1 (s517): `Some(UPnL − position IM)` of a checked taker's sender
    /// (`None` = unchecked).
    checked_pos_net: Option<FixedPoint>,
    /// Item 6 M1: [`PreparedOrder::pre_pos`].
    pre_pos: Option<(FixedPoint, FixedPoint)>,
    /// [`PreparedOrder::top_up_candidate`].
    top_up_candidate: bool,
    /// s92: [`PreparedOrder::res_price`].
    res_price: FixedPoint,
    /// Review fix 1: the order's own unchecked excess (what
    /// [`SenderState::excess_unchecked`] sums; tests' oracle).
    #[cfg(test)]
    excess_im: FixedPoint,
}

/// Open-order count work (trader probes + stops) per worker thread: below
/// 2x this the count stays on the exec thread. Every count a 300-market block
/// can carry (at most 400 senders x 300 books = 120k probes) stays serial:
/// spawning workers per count (s84 profile: 25k per worker, 3-4 workers per
/// block) cost ~28% of the count's CPU in thread clone/exit, and on the
/// loaded bench host the exec thread waited longer for them than the whole
/// count's CPU (margin phase +43 ms per block vs ~31 ms of count CPU). In
/// the release probe (`open_order_count_bench_shape`, bench-sized counts)
/// the split takes 0.33-0.47x the serial time on an idle host but 0.5-1.04x
/// (median ~0.87x) under emulated bursty load, which still leaves out the
/// bench's spawn waits. Larger counts (e.g. 1000 books x 400 senders: 38 ms
/// serial, ~12 ms on 4 workers on an idle host) still split.
const OPEN_COUNT_WORK_PER_THREAD: usize = 100_000;

#[cfg(test)]
mod open_order_count_tests {
    use super::*;

    fn trader(n: u64) -> Address {
        let mut b = [0u8; 20];
        b[..8].copy_from_slice(&n.to_be_bytes());
        Address::from(b)
    }

    fn fp(v: i64) -> FixedPoint {
        FixedPoint::from_raw(v as i128 * FixedPoint::SCALE)
    }

    fn params(market_id: MarketId, price: i64, order_type: OrderType) -> PlaceOrderParams {
        PlaceOrderParams {
            market_id,
            is_buy: true,
            price: fp(price),
            quantity: fp(1),
            order_type,
            time_in_force: TimeInForce::GTC,
            reduce_only: false,
            client_order_id: None,
        }
    }

    struct XorShift(u64);
    impl XorShift {
        fn below(&mut self, n: u64) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0 % n
        }
    }

    /// `NativeExecutor::open_order_counts` gives exactly the counts of the
    /// pre-change split (25k probes per worker, up to 18 workers, the s84
    /// host) and of a per-sender sum, on randomized books: empty books,
    /// resting bids and pending stops, senders repeated, senders absent from
    /// every book, and a call big enough (40 books x 1600 traders x 1600
    /// senders = 64k probes) that the old split ran on workers.
    #[test]
    fn counts_match_pre_change_split_on_random_books() {
        for seed in 1..=8u64 {
            let mut rng = XorShift(0x9E37_79B9_7F4A_7C15 ^ seed);
            let big = seed == 8;
            let (n_books, universe) = if big { (40, 1600) } else { (1 + rng.below(30), 400) };
            let mut books: HashMap<MarketId, OrderBook> = HashMap::default();
            for m in 1..=n_books {
                let mut book = OrderBook::new(m, fp(1), fp(1));
                let traders = match (big, rng.below(4)) {
                    (true, _) => universe,
                    (false, 0) => 0,
                    _ => rng.below(universe),
                };
                for t in 0..traders {
                    let who = if big { t } else { rng.below(universe) };
                    for _ in 0..=rng.below(3) {
                        let price = 1 + rng.below(400) as i64;
                        book.place_order(params(m, price, OrderType::Limit), trader(who), 0);
                    }
                    if rng.below(16) == 0 {
                        let stop = OrderType::StopMarket { trigger: fp(1000) };
                        book.place_order(params(m, 0, stop), trader(who), 0);
                    }
                }
                books.insert(m, book);
            }
            // Senders: some of the universe (some twice) plus some never seen.
            let senders: Vec<Address> = if big {
                (0..universe).map(trader).collect()
            } else {
                (0..1 + rng.below(600)).map(|_| trader(rng.below(universe + 50))).collect()
            };
            let got = NativeExecutor::open_order_counts(&books, senders.iter());

            let mut idx: HashMap<Address, usize> = HashMap::default();
            for s in &senders {
                let next = idx.len();
                idx.entry(*s).or_insert(next);
            }
            let refs: Vec<&OrderBook> = books.values().collect();
            let old = torus_core::order_book::open_order_counts(&refs, &idx, 18, 25_000);
            let distinct: HashSet<Address> = senders.iter().copied().collect();
            assert_eq!(got.len(), distinct.len(), "seed {seed}");
            for (sender, &i) in &idx {
                let naive: usize = books.values().map(|b| b.open_order_count(sender)).sum();
                assert_eq!(got[sender] as usize, old[i], "seed {seed}");
                assert_eq!(old[i], naive, "seed {seed}");
            }
            if big {
                // Lower bound of the old walk: senders present in each book.
                let work: usize = refs
                    .iter()
                    .map(|b| idx.keys().filter(|s| b.open_order_count(s) > 0).count())
                    .sum();
                assert!(work >= 2 * 25_000, "the old split must have used workers");
                assert!(old.iter().sum::<usize>() > 100_000);
            }
        }
    }
}

/// Why an order died pre-book (selects its funnel counter).
#[derive(Clone, Copy)]
enum RejectReason {
    Margin,
    OpenLimit,
    Other,
}

impl RejectReason {
    /// The result's reason of a reject counted under this funnel counter
    /// (the open-order slot check: `OpenLimit` or a state read error).
    fn failure(self) -> FailureReason {
        match self {
            RejectReason::Margin => FailureReason::Margin,
            RejectReason::OpenLimit => FailureReason::OpenLimit,
            RejectReason::Other => FailureReason::Other,
        }
    }

    fn count(self, m: &torus_telemetry::Metrics) {
        match self {
            RejectReason::Margin => m.orders_rejected_margin.inc(),
            RejectReason::OpenLimit => m.orders_rejected_open_limit.inc(),
            RejectReason::Other => m.orders_rejected_other.inc(),
        };
    }
}

/// One sender's open-order slots while its orders are prepared: orders open
/// now (books at load + this block's accepted orders that can rest) and the
/// sender's limit.
#[derive(Clone, Copy)]
struct OpenSlots {
    open: u32,
    limit: u32,
}

/// Whether `p` holds an open-order slot: everything but Market, IOC and FOK
/// orders (they never rest); stops always do while pending.
fn takes_open_slot(p: &PlaceOrderParams) -> bool {
    let never_rests = matches!(p.order_type, OrderType::Market)
        || matches!(p.time_in_force, TimeInForce::IOC | TimeInForce::FOK);
    !never_rests
        || matches!(
            p.order_type,
            OrderType::StopMarket { .. } | OrderType::StopLimit { .. }
        )
}

/// F1 (s517): read-only inputs of the account-level margin formulas —
/// shared by placement (single + Phase 2), modify and withdrawals, so every
/// path computes the same numbers. Only READS state; in Phase 3 the backend
/// is frozen (write-back caches, flushed after settlement).
struct AccountReader<'a, T: StateBackend> {
    positions: &'a PositionManager<T>,
    oracle: &'a OracleManager<T>,
    /// The block's header timestamp (s): the clock of the oracle mark rule.
    now: u64,
    margin_configs: &'a HashMap<MarketId, MarketMarginConfig>,
    /// Item 6 C2: the block's mark table (`None`: every `mark` reads the oracle).
    marks: Option<&'a BlockMarks>,
    /// Item 6 C3: the block's margin sums cache (`None`: every valuation
    /// builds over the trader's rows, the reference path).
    sums: Option<&'a BlockSums>,
    /// Item 6 C6c: the `execute_batch` call's valuation state (Phase 2 / 3
    /// reader only; `None` elsewhere).
    batch: Option<&'a BatchSums>,
    /// Item 6 M1: `margin_configs`' tiers by market id (batch reader only).
    dense_tiers: Option<&'a DenseTiers<'a>>,
    /// adl-dirty-check (node-local): the ADL drain's dirty traders
    /// (`DrainCache::dirty_traders`, ranking reader only), in place of a
    /// `layer_touches` per read. `None` elsewhere.
    drain_dirty: Option<&'a HashSet<Address>>,
    /// R02: the context's reader fault channel
    /// ([`NativeExecContext::reader_fault`]); the first local fault a read
    /// of this reader hits is kept there (any thread).
    fault: &'a std::sync::OnceLock<String>,
}

/// Item 6 Phase 1 (C3, plan 2.4, S1): the position-dependent part of an
/// [`AccountView`] — exactly `AccountView::build`'s sums over a trader's
/// positions (same function, same rows: bit-exact by construction) — plus
/// what liquidation's valuation guard reads (L1, C4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PosSums {
    upnl: FixedPoint,
    position_im: FixedPoint,
    notional: FixedPoint,
    maintenance: FixedPoint,
    /// Positions that are not Cross (`build` skips them; liquidation does
    /// not value such an account). A count (C6b) so a partial re-value can
    /// add and remove. Read by L1 (C4).
    isolated: u32,
    /// Cross positions valued at a mark (not at entry). Read by L1 (C4).
    marked: u32,
    /// C6b magnitude guard: Σ |term| of each sum (upnl, position_im,
    /// notional, maintenance; saturating). At most `i128::MAX` = no partial
    /// sum of `build` can overflow, in any order of its positions.
    abs: [u128; 4],
}

/// C6b: the guard's bound (see [`PosSums::abs`]).
const ABS_GUARD: u128 = i128::MAX as u128;

/// C6b (A-lite) / item 6 M1: `base` (a trader's sums over some rows) minus
/// the terms of each changed row's position as it was (`.0`) plus its terms
/// now (`.1`), valued with `marks` and `tiers`. Exact: the sums are integer
/// sums of the same [`position_terms`] as `build`, the order does not matter
/// while nothing overflows, every step is checked, and the guard (Σ |term|
/// <= `i128::MAX`, see [`PosSums::abs`]) proves `build` over the rows now
/// cannot overflow either. `None` (the caller builds or drops): the guard
/// on `base`, a Cross position in a market outside `marks`, an overflow, or
/// the guard on the result.
fn sums_with_changes<'t>(
    base: PosSums,
    changes: &[trader_positions::Change],
    marks: &BlockMarks,
    tiers: impl Fn(MarketId) -> Option<&'t [MarginTier]>,
) -> Option<SumsResult> {
    if base.abs.iter().any(|a| *a > ABS_GUARD) {
        return None;
    }
    let mut sums = [base.upnl.raw(), base.position_im.raw(), base.notional.raw(), base.maintenance.raw()];
    let (mut abs, mut isolated, mut marked) = (base.abs, base.isolated, base.marked);
    // Removals first (exact: `abs` holds no saturated value), then additions.
    for add in [false, true] {
        for (resident, current) in changes {
            let Some(pos) = (if add { current } else { resident }) else {
                continue;
            };
            let step = |n: u32| if add { n.checked_add(1) } else { n.checked_sub(1) };
            if pos.margin_type != MarginType::Cross {
                isolated = step(isolated)?;
                continue;
            }
            let mark = marks.get(pos.market_id)?;
            if mark.is_some() {
                marked = step(marked)?;
            }
            let t = position_terms(pos, mark, tiers(pos.market_id)).ok()?;
            for ((sum, a), x) in sums.iter_mut().zip(abs.iter_mut()).zip(t.parts()) {
                let x = x.raw();
                if add {
                    *sum = sum.checked_add(x)?;
                    *a = a.saturating_add(x.unsigned_abs());
                } else {
                    *sum = sum.checked_sub(x)?;
                    *a = a.checked_sub(x.unsigned_abs())?;
                }
            }
        }
    }
    if abs.iter().any(|a| *a > ABS_GUARD) {
        return None;
    }
    let [upnl, position_im, notional, maintenance] = sums.map(FixedPoint::from_raw);
    Some(Ok(PosSums { upnl, position_im, notional, maintenance, isolated, marked, abs }))
}

/// C3: `AccountView::build`'s sums over `ps` valued with `mark` / `tiers`
/// (`Err(())`: `build` overflowed), with the counts and magnitudes of
/// [`PosSums`].
fn build_sums<'t>(
    ps: &[torus_core::position::Position],
    mark: impl Fn(MarketId) -> Option<FixedPoint>,
    tiers: impl Fn(MarketId) -> Option<&'t [MarginTier]>,
) -> SumsResult {
    let marked = std::cell::Cell::new(0u32);
    let mark = |m: MarketId| {
        let mark = mark(m);
        marked.set(marked.get().saturating_add(u32::from(mark.is_some())));
        mark
    };
    let mut abs = [0u128; 4];
    let seen = |_: &torus_core::position::Position, t: &torus_core::margin::PositionTerms| {
        for (a, x) in abs.iter_mut().zip(t.parts()) {
            *a = a.saturating_add(x.raw().unsigned_abs());
        }
    };
    AccountView::build_with(&NativeBalance::default(), ps, mark, tiers, seen)
        .map(|v| PosSums {
            upnl: v.upnl,
            position_im: v.position_im,
            notional: v.notional,
            maintenance: v.maintenance,
            isolated: u32::try_from(ps.iter().filter(|p| p.margin_type != MarginType::Cross).count())
                .unwrap_or(u32::MAX),
            marked: marked.get(),
            abs,
        })
        .map_err(|_| ())
}

impl PosSums {
    /// Some position is not Cross.
    fn any_isolated(&self) -> bool {
        self.isolated > 0
    }

    /// `build`'s view with balance `bal`: `build` copies the two balance
    /// fields and adds the position sums, which never read the balance.
    fn view(&self, bal: &NativeBalance) -> AccountView {
        AccountView {
            available: bal.available,
            order_margin: bal.order_margin,
            upnl: self.upnl,
            position_im: self.position_im,
            notional: self.notional,
            maintenance: self.maintenance,
        }
    }
}

/// A trader's sums, `Err(())` = `build` overflowed (reproduced as
/// `CoreError::Overflow("account margin overflows i128")`).
type SumsResult = Result<PosSums, ()>;

/// Address length: a positions key's trader prefix.
const TRADER_PREFIX: usize = 20;

/// Item 6 C3: the persistent sums, kept in the resident rows slot and
/// read-only during a block: each trader's sums over its rows in R, valued
/// with the mark table / configs of `version`. An entry follows the
/// trader's rows: a block that writes them moves it on to the rows after the
/// block or drops it (item 6 M1, [`SumsCarry::trader`]); it is read only
/// while the version holds (versions are never reused, so an entry at
/// another version is never read; the map is cleared when the version moves).
#[derive(Debug, Default)]
struct SumsCache {
    version: u64,
    map: HashMap<Address, SumsResult>,
    /// Item 6 M1 (tests): entries `end_resident` moved on to a trader's rows
    /// after the block (instead of dropping them).
    #[cfg(test)]
    carried: usize,
}

/// Item 6 C3: one block's sums state on the context — the slot's cache and
/// the block's memo (fix 1's pattern: the map lock covers the lookup only,
/// callers needing the same trader wait on its cell for ONE computation;
/// safe under parallel Phase 2 / 3). A cell holds `None` when the result may
/// not be cached (a positions read error, or a mark outside the table):
/// callers then compute it directly.
#[derive(Debug, Default)]
pub(crate) struct BlockSums {
    cache: SumsCache,
    /// The mark version the memo's entries are valued at (set by
    /// `fill_block_marks`; `None`: no table, the cache is not used).
    memo_version: Option<u64>,
    memo: std::sync::Mutex<HashMap<Address, Arc<std::sync::OnceLock<Option<SumsResult>>>>>,
    /// Item 6 C7: R's positions decoded per trader (from the slot; `None`:
    /// every read goes through the overlay). Read only for a trader with
    /// nothing pending ([`AccountReader::resident_positions`]).
    records: Option<TraderPositions>,
    /// P1 (tests): every cached answer is also computed by the reference
    /// path; differences are recorded (a panic in a worker would be caught).
    #[cfg(test)]
    shadow: bool,
    #[cfg(test)]
    shadow_mismatches: std::sync::Mutex<Vec<String>>,
    #[cfg(test)]
    counters: SumsCounters,
}

/// Item 6 M1 / step 1: the slot's sums after a block, while R's decoded
/// positions follow the block's delta ([`TraderPositions::apply`] reports
/// each written trader to [`Self::trader`]).
struct SumsCarry<'a> {
    cache: SumsCache,
    /// The block's mark table / configs (and the configs' tiers by market)
    /// when the entries may be carried; `None`: written traders' entries are
    /// dropped.
    table: Option<(&'a BlockMarksState, DenseTiers<'a>)>,
}

impl SumsCarry<'_> {
    /// A trader the block wrote under: its entry (sums over R's rows before
    /// the block, at the table's version) moves on to its rows after the
    /// block, or is dropped when that is not possible (no entry, no carry,
    /// `changes` unknown: `None`, a Cross position in a market outside the
    /// table, an overflow or the guard of [`sums_with_changes`]). Exact: the
    /// entry is then `build` over R's rows after the block at the version,
    /// as an entry the next block would compute (used only while the version
    /// holds, i.e. the same marks and configs). `changes` / `now`: the
    /// trader's changed positions (before, after) and its positions after
    /// the block (see [`TraderPositions::apply`]); when re-valuing all of
    /// `now` costs no more terms than the changes (2 per change), `build`
    /// over `now` instead (the same sums, exactly).
    fn trader(&mut self, trader: &Address, report: Option<(&[trader_positions::Change], &[torus_core::position::Position])>) {
        let moved = (|| {
            let (t, dense) = self.table.as_ref()?;
            let (changes, now) = report?;
            let base = self.cache.map.get(trader)?.ok()?;
            let tiers = |m: MarketId| {
                if dense.tiers.is_empty() {
                    t.configs.get(&m).map(|c| c.tiers.as_slice())
                } else {
                    dense.tiers.get(m as usize).copied().flatten()
                }
            };
            if now.len() <= 2 * changes.len() {
                let in_table = std::cell::Cell::new(true);
                let mark = |m: MarketId| {
                    t.marks.get(m).unwrap_or_else(|| {
                        in_table.set(false);
                        None
                    })
                };
                let r = build_sums(now, mark, tiers);
                return in_table.get().then_some(r);
            }
            sums_with_changes(base, changes, &t.marks, tiers)
        })();
        match moved {
            Some(r) => {
                self.cache.map.insert(*trader, r);
                #[cfg(test)]
                {
                    self.cache.carried += 1;
                }
            }
            None => {
                self.cache.map.remove(trader);
            }
        }
    }

    /// Every trader the block wrote under loses its entry (no decoded
    /// positions to follow).
    fn drop_written(&mut self, delta: &torus_state::ResidentDelta) {
        let mut last: Option<&[u8]> = None;
        for (key, _) in delta.entries(torus_state::cf::CF_NATIVE_POSITIONS) {
            let Some(prefix) = key.get(..TRADER_PREFIX) else { continue };
            if last != Some(prefix) {
                self.cache.map.remove(&Address::from_slice(prefix));
                last = Some(prefix);
            }
        }
    }
}

/// Which path answered each `pos_sums` call (tests).
#[cfg(test)]
#[derive(Debug, Default)]
struct SumsCounters {
    shadow: std::sync::atomic::AtomicUsize,
    persistent: std::sync::atomic::AtomicUsize,
    memo: std::sync::atomic::AtomicUsize,
    computed: std::sync::atomic::AtomicUsize,
    dirty: std::sync::atomic::AtomicUsize,
    /// C6b: dirty valuations answered by the partial re-value / by a build.
    delta: std::sync::atomic::AtomicUsize,
    delta_fallback: std::sync::atomic::AtomicUsize,
    /// Full valuations (`build` over a trader's rows: the memo's or a
    /// direct one; not the shadow check's) per trader.
    builds: std::sync::Mutex<HashMap<Address, usize>>,
    /// C6c: valuations per trader — full builds plus partial re-values
    /// (cache / memo hits are not valuations).
    valuations: std::sync::Mutex<HashMap<Address, usize>>,
    /// C4: liquidation valuations through L1 / through the walk (L1 off).
    l1: std::sync::atomic::AtomicUsize,
    l1_off: std::sync::atomic::AtomicUsize,
    /// C7: position reads / memo builds answered by the decoded records.
    records: std::sync::atomic::AtomicUsize,
    /// E2: liquidation candidate lists from the slot's trader set.
    traders_slice: std::sync::atomic::AtomicUsize,
    /// adl-budget Q1: ADL counterparty rankings (one trader list each).
    adl_rankings: std::sync::atomic::AtomicUsize,
    /// adl-budget A8 perf (P1): trader sets the drain took (one per drain
    /// with a ranking; the rankings share it).
    adl_trader_sets: std::sync::atomic::AtomicUsize,
    /// adl-budget C2: rankings over a market's holder list (records
    /// attached), each shadow-checked against C1 when `shadow` is on.
    adl_holder_lists: std::sync::atomic::AtomicUsize,
}

#[cfg(test)]
fn bump(c: &std::sync::atomic::AtomicUsize) {
    c.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

#[cfg(test)]
impl SumsCounters {
    fn built(&self, trader: &Address) {
        *self.builds.lock().unwrap().entry(*trader).or_insert(0) += 1;
        self.valued(trader);
    }

    fn valued(&self, trader: &Address) {
        *self.valuations.lock().unwrap().entry(*trader).or_insert(0) += 1;
    }

    /// Valuations of `trader` so far.
    pub(crate) fn valuations_of(&self, trader: &Address) -> usize {
        self.valuations.lock().unwrap().get(trader).copied().unwrap_or(0)
    }

    /// Full valuations of `trader` so far.
    pub(crate) fn builds_of(&self, trader: &Address) -> usize {
        self.builds.lock().unwrap().get(trader).copied().unwrap_or(0)
    }
}

impl BlockSums {
    fn new(cache: SumsCache) -> Self {
        Self { cache, ..Self::default() }
    }

    /// The block's mark table was (re)filled: the memo restarts at `version`.
    fn start(&mut self, version: Option<u64>) {
        self.memo.get_mut().unwrap_or_else(std::sync::PoisonError::into_inner).clear();
        self.memo_version = version;
    }

    /// C6b: `trader`'s sums over R's rows, if already known: the slot's
    /// entry at `version`, else the block's memo (not computed here). `None`
    /// also for a build that overflowed (no terms to adjust).
    fn base(&self, version: u64, trader: &Address) -> Option<PosSums> {
        if self.cache.version == version {
            if let Some(r) = self.cache.map.get(trader) {
                return r.ok();
            }
        }
        let cell = self.memo.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(trader).cloned()?;
        let r = (*cell.get()?)?;
        r.ok()
    }

    /// End of the block: the memo joins the cache (at the memo's version),
    /// returned with what [`SumsCarry::trader`] needs to move on the entry of
    /// each trader the block wrote under (item 6 M1): `table`, the block's
    /// mark table and configs, when the entries may be carried (see below).
    fn into_carry(self, table: Option<&BlockMarksState>) -> SumsCarry<'_> {
        // Node-local policy: carry only while the mark version held across
        // the previous block (the slot's cache is at this block's version).
        // A moving version (marks move most blocks) clears the next block's
        // cache, so carried entries would be thrown away (measured: ~1 ms
        // per 1k fills of block-end work for nothing at walk 10).
        let stable = self.memo_version.is_some_and(|v| v == self.cache.version);
        let mut cache = self.cache;
        #[cfg(test)]
        {
            cache.carried = 0;
        }
        if let Some(version) = self.memo_version {
            if cache.version != version {
                cache.map.clear();
                cache.version = version;
            }
            let memo = self.memo.into_inner().unwrap_or_else(std::sync::PoisonError::into_inner);
            for (trader, cell) in memo {
                if let Some(Some(r)) = cell.get() {
                    cache.map.insert(trader, *r);
                }
            }
        }
        // Carry only at the version the entries are valued at.
        let table = table.filter(|t| {
            stable && self.memo_version == Some(t.marks.version) && cache.version == t.marks.version
        });
        SumsCarry { cache, table: table.map(|t| (t, DenseTiers::of(&t.configs))) }
    }

    /// P1 shadow check: `cached` == the reference path's answer.
    #[cfg(test)]
    fn shadow_check(&self, trader: &Address, cached: &SumsResult, reference: impl FnOnce() -> Result<SumsResult, CoreError>) {
        if !self.shadow {
            return;
        }
        bump(&self.counters.shadow);
        let want = reference();
        if want.as_ref().ok() != Some(cached) {
            self.shadow_mismatches
                .lock()
                .unwrap()
                .push(format!("{trader}: cached {cached:?}, reference {want:?}"));
        }
    }
}

/// Item 6 C6c (D): one `execute_batch` call's valuation state, on its
/// Phase 2 / 3 reader. Sound because the backend is frozen while that
/// reader lives: Phase 2 and Phase 4 write through the balance / position
/// caches, flushed after settlement (the reader is gone by then), and the
/// Phase 3 workers only read. Built only with a sums cache attached.
/// * `dirty`: the traders with own pending position writes, read once
///   (`layer_keys`) in place of a `layer_touches` lock per (maker, book);
///   `None` = not available (ask `layer_touches`).
/// * `memo`: a dirty trader's sums, computed once per call (partial
///   re-value or build); a cell holding `None` = a read error (the caller
///   computes it directly and gets the error).
/// * `id`: keys each Phase 3 worker's maker cache ([`MAKER_FREE`]).
pub(crate) struct BatchSums {
    id: u64,
    dirty: Option<HashSet<Address>>,
    memo: std::sync::Mutex<HashMap<Address, Arc<std::sync::OnceLock<Option<SumsResult>>>>>,
}

/// C6c: source of [`BatchSums::id`] (process-wide, never reused; 0 unused).
static BATCH_IDS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

impl BatchSums {
    fn new<T: StateBackend>(positions: &PositionManager<T>) -> Self {
        let dirty = positions.state().layer_keys(torus_state::cf::CF_NATIVE_POSITIONS).map(|keys| {
            keys.iter()
                .filter(|k| k.len() >= TRADER_PREFIX)
                .map(|k| Address::from_slice(&k[..TRADER_PREFIX]))
                .collect()
        });
        Self {
            id: BATCH_IDS.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1,
            dirty,
            memo: std::sync::Mutex::default(),
        }
    }
}

#[cfg(test)]
thread_local! {
    /// adl-dirty-check (tests): [`AccountReader::dirty`] answers from the ADL
    /// drain's set, from `layer_touches`.
    static DIRTY_CHECKS: std::cell::Cell<(usize, usize)> = const { std::cell::Cell::new((0, 0)) };
}

thread_local! {
    /// C6c (D): each Phase 3 worker's maker cache, in front of the shared
    /// memo locks: [`AccountReader::maker_free`] per maker for the batch
    /// `.0` ([`BatchSums::id`]; another batch clears it). A maker's free
    /// margin is the same in every market of a batch (frozen backend).
    static MAKER_FREE: std::cell::RefCell<(u64, HashMap<Address, FixedPoint>)> =
        std::cell::RefCell::new((0, HashMap::default()));
}

/// Item 6 Phase 1 (C2, plan 2.3): every market's mark for the whole block,
/// read ONCE at the end of [`NativeExecutor::begin_block_oracle`] with the
/// per-read rule ([`AccountReader::mark`]: `get_price(m, now).usable()`) for
/// every listed market, every market with a margin config, every market with
/// an aggregate row (so a delisted market whose last aggregate is still fresh
/// is in the table too) and every market with a loaded book (as fix 1's memo:
/// an unlisted market's `None` is not re-read per position). Sound because only `begin_block_oracle` writes
/// the aggregate rows, before any action of the block, and `now` is the
/// block's: every read of the block returns the same value. A market outside
/// the table (no aggregate row at the block start) reads the oracle directly.
/// Replaces fix 1's per-`execute_batch` memo (`BatchMarks`).
#[derive(Debug)]
pub(crate) struct BlockMarks {
    marks: HashMap<MarketId, Option<FixedPoint>>,
    /// Item 6 M1: `marks` indexed by market id (`dense[m]` = `marks.get(m)`)
    /// when every id is below [`DENSE_MARKETS`]; empty otherwise.
    dense: Vec<Option<Option<FixedPoint>>>,
    /// Changes exactly when the table or `margin_configs` differ from the
    /// previous block's ([`BlockMarksState`], kept in the resident rows slot);
    /// a changed block takes a new process-wide value, never one used before
    /// (slot absent or rebuilt = changed). Keys the sums cache (C3).
    version: u64,
}

/// Item 6 C2: source of new mark versions (process-wide, so a rebuilt or
/// second holder never meets an old value).
static MARK_VERSIONS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Item 6 M1: market ids below this get dense (array-indexed) per-market
/// tables ([`BlockMarks::get`], [`DenseTiers`]); any larger id keeps the map.
const DENSE_MARKETS: u64 = 4096;

/// Item 6 M1: `map` as an array indexed by market id, when every key is below
/// [`DENSE_MARKETS`] (else empty: callers use the map).
fn dense_index<'m, V, W: Clone>(map: &'m HashMap<MarketId, V>, f: impl Fn(&'m V) -> W) -> Vec<Option<W>> {
    match map.keys().max() {
        Some(&max) if max < DENSE_MARKETS => {
            let mut v = vec![None; max as usize + 1];
            for (m, x) in map {
                v[*m as usize] = Some(f(x));
            }
            v
        }
        _ => Vec::new(),
    }
}

/// Item 6 M1: a margin config map's tiers indexed by market id
/// ([`dense_index`]), built per `execute_batch` call from the map the batch
/// reader borrows, so it holds exactly the map's tiers.
struct DenseTiers<'a> {
    tiers: Vec<Option<&'a [MarginTier]>>,
}

impl<'a> DenseTiers<'a> {
    fn of(configs: &'a HashMap<MarketId, MarketMarginConfig>) -> Self {
        Self { tiers: dense_index(configs, |c| c.tiers.as_slice()) }
    }
}

impl BlockMarks {
    fn new(marks: HashMap<MarketId, Option<FixedPoint>>, version: u64) -> Self {
        let dense = dense_index(&marks, |m| *m);
        Self { marks, dense, version }
    }

    /// `marks.get(m)`, through the dense index when there is one.
    fn get(&self, m: MarketId) -> Option<Option<FixedPoint>> {
        if self.dense.is_empty() {
            return self.marks.get(&m).copied();
        }
        self.dense.get(m as usize).copied().flatten()
    }

    /// The per-read mark rule ([`usable_mark`]) for each of `markets`; the
    /// first local fault is the `Err` (R02).
    fn read<T: StateBackend>(
        oracle: &OracleManager<T>,
        now: u64,
        markets: impl IntoIterator<Item = MarketId>,
    ) -> Result<HashMap<MarketId, Option<FixedPoint>>, CoreError> {
        markets
            .into_iter()
            .map(|m| Ok((m, usable_mark(oracle, m, now)?)))
            .collect()
    }

    /// The table's version (see the field).
    pub(crate) fn version(&self) -> u64 {
        self.version
    }

    /// Markets with a mark that are not in `listed`, ascending (normally
    /// none). Liquidation values only listed markets at their mark
    /// (`liquidation_step` `Marks` = the table filtered to `listed`), every
    /// other reader any market with a mark: the two agree iff this is empty.
    /// The liquidation step (L1, C4) decides with it.
    pub(crate) fn delisted_marked(&self, listed: &[MarketId]) -> Vec<MarketId> {
        let mut out: Vec<MarketId> = self
            .marks
            .iter()
            .filter(|(m, mark)| mark.is_some() && !listed.contains(m))
            .map(|(m, _)| *m)
            .collect();
        out.sort_unstable();
        out
    }
}

/// R02: the mark rule of every reader (`get_price(m, now).usable()`) with
/// absence split from failure: no aggregate row, a stale or non-positive
/// one is `Ok(None)` (as before); a local fault (the read failed, the row
/// does not decode) is the `Err`, never "no mark".
fn usable_mark<T: StateBackend>(
    oracle: &OracleManager<T>,
    market_id: MarketId,
    now: u64,
) -> Result<Option<FixedPoint>, CoreError> {
    Ok(oracle.get_price_opt(market_id, now)?.and_then(|p| p.usable()))
}

/// Item 6 C2: a block's mark table and the margin configs it was valued
/// with, carried to the next block in the resident rows slot (via
/// [`ResidentBlock`]) to decide whether the version changes.
#[derive(Debug)]
struct BlockMarksState {
    marks: BlockMarks,
    configs: HashMap<MarketId, MarketMarginConfig>,
}

impl<'a, T: StateBackend> AccountReader<'a, T> {
    fn of(ctx: &'a NativeExecContext<T>) -> Self {
        Self {
            positions: &ctx.positions,
            oracle: &ctx.oracle,
            now: ctx.timestamp,
            margin_configs: &ctx.margin_configs,
            marks: ctx.block_marks.as_ref(),
            sums: ctx.sums.as_ref(),
            batch: None,
            dense_tiers: None,
            drain_dirty: None,
            fault: &ctx.reader_fault,
        }
    }

    /// R02: record `e` on the reader fault channel when it is a local fault
    /// (the first is kept); the caller then returns its "absent" value, which
    /// the block never uses (it fail-stops).
    fn note_fault(&self, step: &str, e: &CoreError) {
        if e.is_local_fault() {
            let _ = self.fault.set(format!("{step}: {e}"));
        }
    }

    /// s515 review 4 mark: the aggregated oracle price while usable
    /// ([`OraclePrice::usable`](torus_core::oracle::OraclePrice::usable):
    /// time-based, stale 60 s of block time after the last fresh aggregate),
    /// `None` when absent, stale or non-positive. R02: a read that fails on
    /// this node is recorded on the reader fault channel (the block
    /// fail-stops) and reads `None` meanwhile. Only oracle
    /// aggregation writes that row, before any action of the block, so every
    /// placement of a block (single, batch serial, batch sharded) reads the
    /// same value on every validator.
    fn mark(&self, market_id: MarketId) -> Option<FixedPoint> {
        match self.marks.and_then(|t| t.get(market_id)) {
            Some(mark) => mark,
            None => usable_mark(self.oracle, market_id, self.now).unwrap_or_else(|e| {
                self.note_fault("oracle mark read", &e);
                None
            }),
        }
    }

    fn tiers(&self, market_id: MarketId) -> Option<&'a [MarginTier]> {
        match self.dense_tiers.filter(|d| !d.tiers.is_empty()) {
            Some(d) => d.tiers.get(market_id as usize).copied().flatten(),
            None => self.margin_configs.get(&market_id).map(|c| c.tiers.as_slice()),
        }
    }

    /// F1: `trader`'s cross-margin account with balance `bal` — positions at
    /// the mark, else at entry (s517 decision 2). Item 6 C3: the position
    /// part from [`Self::pos_sums`].
    fn view(&self, trader: &Address, bal: &NativeBalance) -> Result<AccountView, CoreError> {
        Ok(self.pos_sums(trader)?.view(bal))
    }

    /// F1: UPnL − position IM of `trader` (balance-independent part of `free`).
    fn pos_net(&self, trader: &Address) -> Result<FixedPoint, CoreError> {
        Ok(self.view(trader, &NativeBalance::default())?.pos_net())
    }

    /// Item 6 C3 (plan 2.4): `trader`'s position sums.
    /// 1. no cache or no mark table: `build` over `positions_for_trader`;
    /// 2. the block wrote under the trader's positions prefix: the same, over
    ///    the overlay (own pending + R);
    /// 3. the slot's entry at the block's mark version;
    /// 4. else the block's memo, built once over R's rows (the overlay with
    ///    nothing of the trader pending reads exactly them).
    ///
    /// C6b (A-lite): path 2 first tries [`Self::delta_sums`] (the trader's
    /// sums over R's rows adjusted by this block's changes of its rows).
    fn pos_sums(&self, trader: &Address) -> Result<PosSums, CoreError> {
        let r = match (self.sums, self.marks) {
            (Some(s), Some(table)) if s.memo_version == Some(table.version) => {
                if self.dirty(trader) {
                    #[cfg(test)]
                    bump(&s.counters.dirty);
                    // C6c: once per `execute_batch` call.
                    let r = match self.batch {
                        Some(b) => {
                            let cell = b
                                .memo
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner)
                                .entry(*trader)
                                .or_default()
                                .clone();
                            match *cell.get_or_init(|| self.dirty_sums(s, table, trader).ok()) {
                                Some(r) => r,
                                None => self.dirty_sums(s, table, trader)?,
                            }
                        }
                        None => self.dirty_sums(s, table, trader)?,
                    };
                    #[cfg(test)]
                    s.shadow_check(trader, &r, || self.reference_sums(trader));
                    r
                } else {
                    let r = self.cached_sums(s, table.version, trader)?;
                    #[cfg(test)]
                    s.shadow_check(trader, &r, || self.reference_sums(trader));
                    r
                }
            }
            _ => self.direct_sums(trader)?,
        };
        r.map_err(|()| CoreError::Overflow("account margin overflows i128".into()))
    }

    /// Whether the block wrote under `trader`'s positions prefix (own pending
    /// writes or tombstones): the ADL drain's set on its ranking reader,
    /// C6c's frozen set on a batch reader, else `layer_touches` ("dirty" when
    /// R is not attached).
    fn dirty(&self, trader: &Address) -> bool {
        let touches = || self.positions.state().layer_touches(torus_state::cf::CF_NATIVE_POSITIONS, trader.as_slice());
        if let Some(set) = self.drain_dirty {
            let d = set.contains(trader);
            debug_assert_eq!(d, touches(), "drain dirty set == layer_touches ({trader})");
            #[cfg(test)]
            DIRTY_CHECKS.with(|c| c.set((c.get().0 + 1, c.get().1)));
            return d;
        }
        match self.batch.and_then(|b| b.dirty.as_ref()) {
            Some(set) => set.contains(trader),
            None => {
                #[cfg(test)]
                DIRTY_CHECKS.with(|c| c.set((c.get().0, c.get().1 + 1)));
                touches()
            }
        }
    }

    /// Item 6 C7: `trader`'s positions decoded from R's slot — what the
    /// overlay reads for a trader with nothing pending. `None` (read the
    /// overlay): no records, the trader is dirty, or it holds an irregular
    /// row.
    fn resident_positions(&self, trader: &Address) -> Option<&'a [torus_core::position::Position]> {
        let records = self.sums?.records.as_ref()?;
        if self.dirty(trader) {
            return None;
        }
        records.get(trader)
    }

    /// `trader`'s position in `market_id`: C7's record when clean, else the
    /// overlay (`PositionManager::get_position`).
    fn get_position(
        &self,
        trader: &Address,
        market_id: MarketId,
    ) -> Result<Option<torus_core::position::Position>, CoreError> {
        let Some(ps) = self.resident_positions(trader) else {
            return self.positions.get_position(trader, market_id);
        };
        let p = trader_positions::find(ps, market_id).cloned();
        #[cfg(test)]
        if let Some(s) = self.sums {
            bump(&s.counters.records);
            if s.shadow {
                let want = format!("{:?}", self.positions.get_position(trader, market_id));
                let got = format!("{:?}", Ok::<_, CoreError>(p.clone()));
                if want != got {
                    s.shadow_mismatches.lock().unwrap().push(format!("get_position {trader} {market_id}: record {got}, overlay {want}"));
                }
            }
        }
        Ok(p)
    }

    /// Path 2: the partial re-value (C6b), else `build` over the overlay.
    fn dirty_sums(&self, s: &BlockSums, table: &BlockMarks, trader: &Address) -> Result<SumsResult, CoreError> {
        match self.delta_sums(s, table, trader) {
            Some(r) => {
                #[cfg(test)]
                {
                    bump(&s.counters.delta);
                    s.counters.valued(trader);
                }
                Ok(r)
            }
            None => {
                #[cfg(test)]
                bump(&s.counters.delta_fallback);
                self.direct_sums(trader)
            }
        }
    }

    /// Paths 1 and 2: `build` over the trader's rows as the backend shows them.
    fn direct_sums(&self, trader: &Address) -> Result<SumsResult, CoreError> {
        #[cfg(test)]
        if let Some(s) = self.sums {
            s.counters.built(trader);
        }
        self.reference_sums(trader)
    }

    /// `build` over the trader's rows (the shadow check's reference; not
    /// counted as a valuation).
    fn reference_sums(&self, trader: &Address) -> Result<SumsResult, CoreError> {
        Ok(self.sums_of(&self.positions.positions_for_trader(trader)?).0)
    }

    /// C6b (A-lite): `trader`'s sums now = its sums over R's rows (the slot's
    /// entry or the block's memo, [`BlockSums::base`]) minus the terms of
    /// each row the block changed under its prefix as R holds it, plus the
    /// terms of the row now ([`torus_state::StateBackend::resident_changes`]).
    /// Exact: the sums are integer sums of the same [`position_terms`] as
    /// `build`, the order does not matter while nothing overflows, every
    /// step is checked, and the guard (Σ |term| <= `i128::MAX`, see
    /// [`PosSums::abs`]) proves `build` over the rows now cannot overflow
    /// either. `None` (the caller builds): no base, no change list, a row
    /// that does not decode, a Cross position in a market outside the mark
    /// table, an overflow, or the guard.
    fn delta_sums(&self, s: &BlockSums, table: &BlockMarks, trader: &Address) -> Option<SumsResult> {
        let base = s.base(table.version, trader)?;
        if base.abs.iter().any(|a| *a > ABS_GUARD) {
            return None;
        }
        let changes =
            self.positions.state().resident_changes(torus_state::cf::CF_NATIVE_POSITIONS, trader.as_slice())?;
        // A row that does not decode: `None` (the caller builds).
        let decode = |row: &Option<Vec<u8>>| -> Option<Option<torus_core::position::Position>> {
            use borsh::BorshDeserialize;
            row.as_deref().map_or(Some(None), |r| torus_core::position::Position::try_from_slice(r).ok().map(Some))
        };
        let changes = changes
            .iter()
            .map(|c| Some((decode(&c.resident)?, decode(&c.current)?)))
            .collect::<Option<Vec<_>>>()?;
        sums_with_changes(base, &changes, table, |m| self.tiers(m))
    }

    /// Paths 3 and 4 (the trader has nothing pending this block).
    fn cached_sums(&self, s: &BlockSums, version: u64, trader: &Address) -> Result<SumsResult, CoreError> {
        if s.cache.version == version {
            if let Some(r) = s.cache.map.get(trader) {
                #[cfg(test)]
                bump(&s.counters.persistent);
                return Ok(*r);
            }
        }
        let cell = s
            .memo
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entry(*trader)
            .or_default()
            .clone();
        #[cfg(test)]
        let computed = std::cell::Cell::new(false);
        let memo = *cell.get_or_init(|| {
            #[cfg(test)]
            {
                computed.set(true);
                s.counters.built(trader);
            }
            // C7: the caller found the trader clean, so its record (if
            // any) is exactly the overlay's rows.
            let (r, in_table) = match s.records.as_ref().and_then(|rec| rec.get(trader)) {
                Some(ps) => {
                    #[cfg(test)]
                    bump(&s.counters.records);
                    self.sums_of(ps)
                }
                None => self.sums_of(&self.positions.positions_for_trader(trader).ok()?),
            };
            in_table.then_some(r)
        });
        #[cfg(test)]
        bump(if computed.get() { &s.counters.computed } else { &s.counters.memo });
        match memo {
            Some(r) => Ok(r),
            None => self.direct_sums(trader),
        }
    }

    /// `AccountView::build`'s sums over `ps` with this reader's marks and
    /// tiers, and whether every mark it used came from the block's table
    /// (only then is the result a function of R's rows and the mark version,
    /// i.e. cacheable; a market outside the table reads the oracle).
    fn sums_of(&self, ps: &[torus_core::position::Position]) -> (SumsResult, bool) {
        let in_table = std::cell::Cell::new(true);
        let mark = |m: MarketId| match self.marks.and_then(|t| t.get(m)) {
            Some(mark) => mark,
            None => {
                in_table.set(false);
                self.mark(m)
            }
        };
        (build_sums(ps, mark, |m| self.tiers(m)), in_table.get())
    }

    /// F1: signed position in `market_id` and the price it is valued at
    /// (mark, else entry; ZERO when flat without a mark).
    fn position_px(
        &self,
        trader: &Address,
        market_id: MarketId,
    ) -> Result<(FixedPoint, FixedPoint), CoreError> {
        let mark = self.mark(market_id);
        Ok(Self::signed_px(self.get_position(trader, market_id)?.as_ref(), mark))
    }

    /// [`Self::position_px`] of a position already read, with the market's
    /// `mark` (item 6 P4: Phase 3 reuses `reduce_only_positions_for`'s read).
    fn signed_px(p: Option<&torus_core::position::Position>, mark: Option<FixedPoint>) -> (FixedPoint, FixedPoint) {
        match p {
            Some(p) => (
                if p.is_long { p.size } else { -p.size },
                position_price(p, mark),
            ),
            None => (FixedPoint::ZERO, mark.unwrap_or(FixedPoint::ZERO)),
        }
    }
}

impl<T: StateBackend> AccountReader<'_, T> {
    /// F1 (s517 #4): a maker's snapshot free margin — balance and positions
    /// from the backend. A read error snapshots as 0 (deterministic).
    /// C6c: on a batch reader, once per maker per Phase 3 worker
    /// ([`MAKER_FREE`]).
    fn maker_free(&self, maker: &Address) -> FixedPoint {
        let compute = || {
            self.positions
                .get_native_balance(maker)
                .and_then(|b| self.view(maker, &b))
                .map_or(FixedPoint::ZERO, |v| v.free())
        };
        let Some(b) = self.batch else {
            return compute();
        };
        let hit = MAKER_FREE.with(|c| {
            let c = c.borrow();
            if c.0 == b.id { c.1.get(maker).copied() } else { None }
        });
        if let Some(free) = hit {
            #[cfg(test)]
            if let Some(s) = self.sums.filter(|s| s.shadow) {
                let want = compute();
                if want != free {
                    s.shadow_mismatches.lock().unwrap().push(format!("maker_free {maker}: worker cache {free:?}, now {want:?}"));
                }
            }
            return free;
        }
        let free = compute();
        MAKER_FREE.with(|c| {
            let mut c = c.borrow_mut();
            if c.0 != b.id {
                *c = (b.id, HashMap::default());
            }
            c.1.insert(*maker, free);
        });
        free
    }

    /// F1: `maker`'s signed position in `market_id` and its valuation price;
    /// a read error snapshots as flat (deterministic).
    fn maker_position_px(&self, maker: &Address, market_id: MarketId) -> (FixedPoint, FixedPoint) {
        self.position_px(maker, market_id)
            .unwrap_or((FixedPoint::ZERO, FixedPoint::ZERO))
    }
}

impl<T: StateBackend> MakerAccountSource for AccountReader<'_, T> {
    /// F1 (s517 #4): a maker's account as the book first sees it — balance
    /// and positions from the backend (Phase 3: the frozen post-Phase-1
    /// state; single path: current state). A read error snapshots as free 0
    /// / flat (deterministic).
    fn maker_account(&self, maker: &Address, market_id: MarketId) -> MakerAccount {
        let (signed_pos, px) = self.maker_position_px(maker, market_id);
        MakerAccount { free: self.maker_free(maker), signed_pos, px }
    }
}

/// L3-ENG: one Phase-2 fold — the state of the senders it prepares. Keys
/// are per sender, so a shard's fold equals the serial fold restricted to
/// its senders. Item 6 M1: one map entry per sender (was a balance cache and
/// five maps); std `HashMap`s, never iterated in a way that affects output
/// ([`Self::finish`] only builds maps). Item 6 cut 2: the projections live
/// in their sender's state, and the latest sender's state is held out of the
/// map ([`Self::state`]).
#[derive(Default)]
struct SenderFold {
    senders: HashMap<Address, SenderState>,
    /// Item 6 cut 2: the previous order's sender and its state, out of
    /// `senders`. A PlaceOrderBatch's orders, and a sender's actions in a
    /// sorted list, are adjacent, so most orders need no map lookup.
    cur: Option<(Address, SenderState)>,
}

/// Item 6 M1: one sender's Phase-2 state in a [`SenderFold`].
struct SenderState {
    /// O1: the sender's balance as Phase 2 left it (read through on first
    /// use; `None` until read, or after a failed read — retried), and
    /// whether Phase 2 changed it ([`BalanceCache`] semantics).
    balance: Option<NativeBalance>,
    balance_dirty: bool,
    /// F1: UPnL − position IM (pre-batch, read once).
    pos_net: Option<FixedPoint>,
    /// F1 (Decision s517): Σ position IM the sender's earlier accepted
    /// orders of this batch are projected to RELEASE (their closing parts).
    /// Credited to `free` only when checking a match-checked order — its
    /// fills are re-checked against the real position in Phase 3. Never
    /// part of the Phase-3 pool (the book credits the real release).
    released: FixedPoint,
    /// Review fix 1 (s517): Σ position-tier need beyond the order-tier
    /// reservation of the sender's accepted orders of this batch — committed
    /// but not debited; off `free` in its later Phase-2 checks.
    committed: FixedPoint,
    /// Review fix 1 (s517): Σ of the sender's accepted UNCHECKED orders'
    /// position-tier need beyond their order-tier reservation (never
    /// re-checked at match, D7) — off its D2 pool after Phase 2 (was each
    /// prepared order's `excess_im`, summed over the batches). Checked
    /// takers add nothing: the book charges their need at match.
    excess_unchecked: FixedPoint,
    /// Per-user open-order limit: the sender's slots in this
    /// `execute_batch` call (see [`NativeExecutor::take_open_slot`]).
    open_slots: Option<OpenSlots>,
    /// F1: the sender's markets → the in-batch position projection (item 6
    /// cut 2: was a map keyed by (sender, market) in the fold).
    proj: Projections,
    /// Option B (s87): F1 D2 — the market of the sender's first ACCEPTED
    /// match-checked order of this batch (flat order), i.e. the market
    /// Phase 3 gives its pool (was `d2_pool_takers`, a pass over the
    /// prepared orders: the same first order).
    pool: Option<MarketId>,
}

/// F1: (projected signed position, valuation price): the pre-batch
/// position advanced by the sender's earlier ACCEPTED non-reduce-only
/// orders of this batch in the market (as if filled — conservative), so a
/// later order is charged at the projected position's tier; plus the
/// pre-batch read itself (item 6 M1: [`PreparedOrder::pre_pos`]).
#[derive(Clone, Copy)]
struct Projection {
    signed: FixedPoint,
    px: FixedPoint,
    pre: (FixedPoint, FixedPoint),
}

/// Item 6 cut 2: one sender's [`Projection`]s by market — a short list
/// (linear search) up to [`Projections::FEW`] markets, then a map. A pure
/// map: only point lookups and inserts, never iterated.
enum Projections {
    Few(Vec<(MarketId, Projection)>),
    Many(HashMap<MarketId, Projection>),
}

impl Default for Projections {
    fn default() -> Self {
        Self::Few(Vec::new())
    }
}

impl Projections {
    /// Markets kept in the list before it becomes a map.
    const FEW: usize = 16;

    /// `market`'s projection; when absent, `init()`'s, inserted (an error
    /// inserts nothing).
    fn get_or_try_insert<E>(
        &mut self,
        market: MarketId,
        init: impl FnOnce() -> Result<Projection, E>,
    ) -> Result<&mut Projection, E> {
        let found = match self {
            Self::Few(list) => list.iter().position(|(m, _)| *m == market),
            Self::Many(map) => {
                return Ok(match map.entry(market) {
                    std::collections::hash_map::Entry::Occupied(o) => o.into_mut(),
                    std::collections::hash_map::Entry::Vacant(v) => v.insert(init()?),
                })
            }
        };
        if let Some(i) = found {
            let Self::Few(list) = self else { unreachable!("matched Few above") };
            return Ok(&mut list[i].1);
        }
        let p = init()?;
        if matches!(self, Self::Few(list) if list.len() >= Self::FEW) {
            let Self::Few(list) = std::mem::take(self) else { unreachable!("matched Few above") };
            *self = Self::Many(list.into_iter().collect());
        }
        Ok(match self {
            Self::Few(list) => {
                list.push((market, p));
                &mut list.last_mut().expect("just pushed").1
            }
            // Absent: it was not in the list.
            Self::Many(map) => map.entry(market).or_insert(p),
        })
    }
}

/// Item 6 M1: what a Phase-2 fold hands on: the balance cache, each pooled
/// sender's D2 pool `(market, pos_net)` and each sender's unchecked
/// `excess_im` sum (> 0 only). Sender-keyed std `HashMap`s, only looked up
/// or iterated into other maps / the cache's read-through (no output order).
#[derive(Default)]
struct FoldOut {
    cache: BalanceCache,
    pools: HashMap<Address, (MarketId, FixedPoint)>,
    excess: HashMap<Address, FixedPoint>,
}

/// Item 6 M1: one batch market's read-only Phase-2 inputs, looked up once
/// per order (was a shape map, a bid-floor map, the margin configs three
/// times and the mark per (sender, market)). Built once per call by
/// [`NativeExecutor::phase2_markets`], shared by the serial and sharded
/// prepare paths (Phase 2 touches no book, config or mark).
#[derive(Clone, Copy)]
struct Phase2Market<'a> {
    /// Fix A (s92) / row 42: [`NativeExecutor::book_shape`].
    shape: (FixedPoint, FixedPoint),
    /// Option B (s87): the market's best bid after Phase 1 (`None`: no bid).
    bid_floor: Option<FixedPoint>,
    cfg: Option<&'a MarketMarginConfig>,
    /// [`AccountReader::mark`].
    mark: Option<FixedPoint>,
    /// s94 option 2: the market's price band ([`NativeExecutor::price_band`],
    /// from the books after Phase 1); its reference is also option 1's mark
    /// for the fills' charge. `None`: no reference (never marked).
    band: Option<PriceBand>,
    /// Item 6 cut 2: the call's PlaceOrders in the market (the capacity of
    /// its prepared batch: no regrowth while stitching).
    orders: usize,
}

impl Phase2Market<'_> {
    fn tiers(&self) -> Option<&[MarginTier]> {
        self.cfg.map(|c| c.tiers.as_slice())
    }
}

impl FoldOut {
    /// L3-ENG: absorb a sharded worker's output (sender-disjoint).
    fn merge_disjoint(&mut self, other: FoldOut) {
        self.cache.merge_disjoint(other.cache);
        self.pools.extend(other.pools);
        self.excess.extend(other.excess);
    }
}

/// Item 6 cut 2: [`Projections`] and [`SenderFold::state`] behave as the
/// maps they replace (`HashMap<(Address, MarketId), Projection>` and
/// `HashMap<Address, SenderState>` with `entry().or_default()`), on random
/// interleavings across the list -> map switch, failed inits included.
#[cfg(test)]
mod projections_tests {
    use super::*;

    fn proj(v: i128) -> Projection {
        let f = FixedPoint::from_raw(v);
        Projection { signed: f, px: f, pre: (f, f) }
    }

    #[test]
    fn projections_and_sender_states_equal_the_maps() {
        let mut seed = 0x6a09_e667_f3bc_c908u64;
        let mut next = move |n: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % n
        };
        for round in 0..200 {
            let markets = [3u64, 12, 40][round % 3];
            let mut fold = SenderFold::with_capacity(0);
            let mut want: HashMap<(Address, MarketId), Projection> = HashMap::default();
            let mut want_released: HashMap<Address, FixedPoint> = HashMap::default();
            for step in 0..600 {
                // Runs of one sender, as in a batch, and switches.
                let sender = Address::repeat_byte(1 + next(4) as u8);
                let market = 1 + next(markets);
                let fail = next(7) == 0;
                let st = fold.state(&sender);
                st.released += FixedPoint::from_raw(1);
                *want_released.entry(sender).or_insert(FixedPoint::ZERO) += FixedPoint::from_raw(1);
                let got = st
                    .proj
                    .get_or_try_insert(market, || if fail { Err(()) } else { Ok(proj(step)) })
                    .map(|p| {
                        p.signed += FixedPoint::from_raw(1);
                        *p
                    });
                let expect = match want.entry((sender, market)) {
                    std::collections::hash_map::Entry::Occupied(o) => Ok(o.into_mut()),
                    std::collections::hash_map::Entry::Vacant(v) => {
                        if fail { Err(()) } else { Ok(v.insert(proj(step))) }
                    }
                }
                .map(|p| {
                    p.signed += FixedPoint::from_raw(1);
                    *p
                });
                assert_eq!(
                    got.map(|p| (p.signed, p.px, p.pre)),
                    expect.map(|p| (p.signed, p.px, p.pre)),
                    "round {round} step {step}"
                );
            }
            if let Some((a, st)) = fold.cur.take() {
                fold.senders.insert(a, st);
            }
            assert_eq!(fold.senders.len(), want_released.len());
            for (sender, released) in want_released {
                assert_eq!(fold.senders[&sender].released, released);
            }
        }
    }
}

impl SenderFold {
    /// Pre-sized: `senders` expected.
    fn with_capacity(senders: usize) -> Self {
        Self {
            senders: HashMap::with_capacity_and_hasher(senders, Default::default()),
            cur: None,
        }
    }

    /// `sender`'s state (default when new), held out of the map until the
    /// next order of another sender puts it back.
    fn state(&mut self, sender: &Address) -> &mut SenderState {
        if !matches!(&self.cur, Some((a, _)) if a == sender) {
            if let Some((a, st)) = self.cur.take() {
                self.senders.insert(a, st);
            }
            let st = self.senders.remove(sender).unwrap_or_default();
            self.cur = Some((*sender, st));
        }
        &mut self.cur.as_mut().expect("just set").1
    }

    /// The fold's balances as a [`BalanceCache`] (read entries clean, changed
    /// ones dirty: the cache `load` / `set` would have left), its pools and
    /// its excess sums.
    fn finish(mut self) -> FoldOut {
        if let Some((a, st)) = self.cur.take() {
            self.senders.insert(a, st);
        }
        let mut out = FoldOut {
            cache: BalanceCache {
                map: HashMap::with_capacity_and_hasher(self.senders.len(), Default::default()),
                dirty: Vec::new(),
            },
            ..FoldOut::default()
        };
        for (sender, st) in self.senders {
            if let Some(pool) = st.pool {
                debug_assert!(st.pos_net.is_some(), "a pooled sender passed an account check");
                out.pools.insert(sender, (pool, st.pos_net.unwrap_or(FixedPoint::ZERO)));
            }
            if st.excess_unchecked > FixedPoint::ZERO {
                out.excess.insert(sender, st.excess_unchecked);
            }
            if let Some(balance) = st.balance {
                if st.balance_dirty {
                    out.cache.dirty.push(sender);
                }
                out.cache.map.insert(sender, CachedBalance { balance, dirty: st.balance_dirty });
            }
        }
        out
    }
}

impl Default for SenderState {
    fn default() -> Self {
        Self {
            balance: None,
            balance_dirty: false,
            pos_net: None,
            released: FixedPoint::ZERO,
            committed: FixedPoint::ZERO,
            excess_unchecked: FixedPoint::ZERO,
            open_slots: None,
            proj: Projections::default(),
            pool: None,
        }
    }
}

impl SenderState {
    /// [`BalanceCache::load`] on the sender's own entry (`slot` =
    /// [`Self::balance`]; a field borrow, so the other fields stay usable).
    fn load_balance<'s, T: StateBackend>(
        slot: &'s mut Option<NativeBalance>,
        positions: &PositionManager<T>,
        sender: &Address,
    ) -> Result<&'s mut NativeBalance, CoreError> {
        if slot.is_none() {
            *slot = Some(positions.get_native_balance(sender)?);
        }
        Ok(slot.get_or_insert_with(NativeBalance::default))
    }
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
// LevelAuthorityChunked (`TORUS_BOOK_ROWS=3`): IDENTICAL key/value layout and
// load/save/verify machinery to mode 2, but the `level_hash` preimage is the
// CHUNKED digest (`torus_core::book_rows::LevelRowData` docs): frames are
// bucketed by `seq / 64` so a dirty level costs O(touched chunks) instead of
// O(resting depth) at save time. Different digest ⇒ different state root ⇒
// its own marker byte (3): a mode-2 node restarted as mode 3 (or vice versa)
// fail-stops at the marker AND at boot verify 3 (recomputed level rows differ).
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
    /// Mode 2's layout with the CHUNKED, depth-independent `level_hash`
    /// preimage (`TORUS_BOOK_ROWS=3`). Consensus-visible; fresh genesis.
    LevelAuthorityChunked,
}

impl BookMode {
    /// One-byte discriminant persisted in the `__book_mode__` marker row.
    fn marker_byte(self) -> u8 {
        match self {
            BookMode::Classic => 0,
            BookMode::OrderRows => 1,
            BookMode::LevelAuthority => 2,
            BookMode::LevelAuthorityChunked => 3,
        }
    }

    /// Both level-authority variants share the row layout, loader, saver and
    /// boot verify; they differ only in the `level_hash` preimage.
    fn is_level_authority(self) -> bool {
        matches!(
            self,
            BookMode::LevelAuthority | BookMode::LevelAuthorityChunked
        )
    }

    /// True for the chunked-digest variant (mode 3).
    fn level_hash_chunked(self) -> bool {
        matches!(self, BookMode::LevelAuthorityChunked)
    }

    fn describe(self) -> &'static str {
        match self {
            BookMode::Classic => "classic (whole-book blobs)",
            BookMode::OrderRows => "order rows (TORUS_BOOK_ROWS=1)",
            BookMode::LevelAuthority => "level authority (TORUS_BOOK_ROWS=2)",
            BookMode::LevelAuthorityChunked => {
                "level authority, chunked level hash (TORUS_BOOK_ROWS=3)"
            }
        }
    }
}

/// Runtime mode: `TORUS_BOOK_ROWS=1` → OrderRows, `=2` → LevelAuthority,
/// `=3` → LevelAuthorityChunked; anything else (INCLUDING UNSET) → Classic —
/// byte-identical state root to today. Read once per process. Fleet-uniform,
/// fresh genesis to change.
fn book_mode() -> BookMode {
    static MODE: std::sync::OnceLock<BookMode> = std::sync::OnceLock::new();
    *MODE.get_or_init(|| parse_book_rows_mode(std::env::var("TORUS_BOOK_ROWS").ok()))
}

/// Pure parse of the `TORUS_BOOK_ROWS` value (same trim/strictness as C4).
fn parse_book_rows_mode(v: Option<String>) -> BookMode {
    match v.as_deref().map(str::trim) {
        Some("1") => BookMode::OrderRows,
        Some("2") => BookMode::LevelAuthority,
        Some("3") => BookMode::LevelAuthorityChunked,
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

// ============================================================================
// Mode-2 save: parallel journal drain across dirty books
// (`TORUS_SAVE_BOOKS_WORKERS`, `TORUS_SAVE_BOOKS_MIN_OPS`) — node-local
// ============================================================================
//
// The LevelAuthority save is, per dirty book, a DRAIN (`take_row_ops` row
// encode + `take_level_ops` level keccak — pure functions of that ONE book,
// including its private level-hash sponge cache) followed by CF WRITES. The
// drain dominates the phase (docs/l3-savebooks-attribution.md) and books are
// independent, so dirty books are drained on scoped worker threads
// (LPT-chunked by journal length, the `market_workers` idiom — no rayon) and
// the drained ops are then written SERIALLY in market-ascending order — the
// exact order and bytes of the serial loop. Byte-identical by construction for
// any worker count (differential test: `tests/save_books_parallel_tests.rs`);
// the overlay is keyed anyway, so even the write order is belt-and-braces.
// A worker panic propagates (resume_unwind) exactly like a serial-loop panic —
// no fallback, no silent skip. Modes 0/1 never reach this path.

/// Hard cap on save-books drain worker threads.
const MAX_SAVE_BOOKS_WORKERS: usize = 64;

/// Default work gate for the parallel drain: total journaled rows + levels
/// across dirty books below this run the serial loop (a thread scope costs
/// ~tens of µs; a tiny block is not worth it). Byte-identical either way.
const DEFAULT_SAVE_BOOKS_MIN_OPS: usize = 32;

/// `TORUS_SAVE_BOOKS_WORKERS`: worker cap for the mode-2 save drain. Unset /
/// garbage → host parallelism (capped at [`MAX_SAVE_BOOKS_WORKERS`]); `0` /
/// `1` → serial (the exact-today loop); N>=2 → N (capped). Read once per
/// process; tests override the ctx field (`save_books_workers`) directly.
fn save_books_workers_env() -> usize {
    static N: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *N.get_or_init(|| {
        let host = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1);
        parse_save_books_workers(std::env::var("TORUS_SAVE_BOOKS_WORKERS").ok(), host)
    })
}

/// `TORUS_LOAD_BOOKS_WORKERS`: worker threads for the boot-time per-market
/// book rebuild + verify (s74). Same policy as `TORUS_SAVE_BOOKS_WORKERS`:
/// unset / garbage = host parallelism, `1` = the serial load.
fn load_books_workers_env() -> usize {
    static N: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *N.get_or_init(|| {
        let host = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1);
        parse_save_books_workers(std::env::var("TORUS_LOAD_BOOKS_WORKERS").ok(), host)
    })
}

/// Pure parse for [`save_books_workers_env`] and [`load_books_workers_env`]. `host` is the fallback for
/// unset / garbage (same policy as the sibling knobs: never a third behaviour).
fn parse_save_books_workers(v: Option<String>, host: usize) -> usize {
    match v.as_deref().map(str::trim).and_then(|s| s.parse::<usize>().ok()) {
        Some(n) if n >= 2 => n.min(MAX_SAVE_BOOKS_WORKERS),
        Some(_) => 1,
        None => host.clamp(1, MAX_SAVE_BOOKS_WORKERS),
    }
}

/// `TORUS_SAVE_BOOKS_MIN_OPS`: work gate for the parallel drain (journaled
/// rows + levels across dirty books). Unset / garbage →
/// [`DEFAULT_SAVE_BOOKS_MIN_OPS`]; `0` = always parallel when >= 2 dirty books
/// and >= 2 workers. Read once per process; tests override the ctx field.
fn save_books_min_ops_env() -> usize {
    static N: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *N.get_or_init(|| parse_save_books_min_ops(std::env::var("TORUS_SAVE_BOOKS_MIN_OPS").ok()))
}

/// Pure parse for [`save_books_min_ops_env`].
fn parse_save_books_min_ops(v: Option<String>) -> usize {
    v.as_deref()
        .map(str::trim)
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(DEFAULT_SAVE_BOOKS_MIN_OPS)
}

#[cfg(test)]
mod save_books_workers_toggle_tests {
    use super::{
        parse_save_books_min_ops, parse_save_books_workers, DEFAULT_SAVE_BOOKS_MIN_OPS,
        MAX_SAVE_BOOKS_WORKERS,
    };

    /// Unset ⇒ ON at host parallelism (the change is default-on).
    #[test]
    fn unset_is_host_parallelism() {
        assert_eq!(parse_save_books_workers(None, 18), 18);
        assert_eq!(parse_save_books_workers(None, 1), 1);
        assert_eq!(parse_save_books_workers(None, 0), 1, "host 0 clamps to 1");
    }

    /// `0` / `1` ⇒ serial — the explicit opt-out back to the exact-today loop.
    #[test]
    fn zero_and_one_are_serial() {
        for v in ["0", "1", " 1 ", "0\n"] {
            assert_eq!(parse_save_books_workers(Some(v.to_string()), 18), 1, "{v:?}");
        }
    }

    /// N>=2 is honoured and capped.
    #[test]
    fn explicit_n_is_honoured_and_capped() {
        assert_eq!(parse_save_books_workers(Some("2".to_string()), 18), 2);
        assert_eq!(parse_save_books_workers(Some(" 4 ".to_string()), 18), 4);
        assert_eq!(parse_save_books_workers(Some("40".to_string()), 18), 40);
        assert_eq!(
            parse_save_books_workers(Some("4096".to_string()), 18),
            MAX_SAVE_BOOKS_WORKERS
        );
        assert_eq!(parse_save_books_workers(None, 4096), MAX_SAVE_BOOKS_WORKERS);
    }

    /// Garbage falls back to the host default, never to a third behaviour.
    #[test]
    fn garbage_falls_back_to_host() {
        for v in ["", "on", "true", "-1", "2.5", "yes"] {
            assert_eq!(parse_save_books_workers(Some(v.to_string()), 8), 8, "{v:?}");
        }
    }

    #[test]
    fn min_ops_default_and_override() {
        assert_eq!(parse_save_books_min_ops(None), DEFAULT_SAVE_BOOKS_MIN_OPS);
        assert_eq!(
            parse_save_books_min_ops(Some("garbage".to_string())),
            DEFAULT_SAVE_BOOKS_MIN_OPS
        );
        assert_eq!(parse_save_books_min_ops(Some("0".to_string())), 0);
        assert_eq!(parse_save_books_min_ops(Some(" 500 ".to_string())), 500);
    }
}

/// Pass-1 output of the mode-2 save for ONE dirty book: its drained journals
/// (row upserts/deletes for the node-local store, level upserts/deletes for
/// the root CF), ready for the serial market-ascending write pass. `rows_ns`
/// / `levels_ns` are the drain times (0 unless `timed`).
struct DrainedBook {
    market_id: MarketId,
    row_ops: Vec<(OrderId, Option<Vec<u8>>)>,
    level_ops: Vec<((u8, i128), Option<LevelRowData>)>,
    rows_ns: u128,
    levels_ns: u128,
}

/// Deferred book save (s63 port of item 6a, origin 4298728): one book's
/// pass-2 payload — its drained journals plus snapshots of the two live-book
/// inputs pass 2 needs (`stop_rows()`, meta bytes), taken on the exec thread
/// at save time so the flush worker never touches the live book (which the
/// next block's engine is already mutating).
struct DeferredBook {
    drained: DrainedBook,
    stops: Vec<(OrderId, Vec<u8>)>,
    meta: Vec<u8>,
}

/// A whole block's deferred mode-2/3 save pass 2, produced by
/// [`NativeExecContext::save_order_books_deferred`] on the exec thread and
/// applied by [`apply_deferred_book_save`] on the exec pipeline's flush worker
/// (market-ascending, the exact serial write sequence). Opaque outside this
/// module: consensus only carries it inside `Job::Flush`.
pub struct DeferredBookSave {
    books: Vec<DeferredBook>,
}

impl DeferredBookSave {
    /// Number of dirty books carried (tests / logging).
    pub fn book_count(&self) -> usize {
        self.books.len()
    }
}

/// The flush-worker half of the deferred book save: apply pass 2 against
/// `state` — node-local order rows, root-CF level rows, stop diff,
/// meta-if-moved, market-ascending — exactly the write sequence
/// `save_order_books` runs on the exec thread (same owned puts: the drained
/// row bytes MOVE into the pending set). Returns the root-CF write count (same
/// meaning as `save_order_books`'s return). `state`'s reads (stop-row diff,
/// meta compare) must see the durable post-state of every height below this
/// block — the flush worker's view, since it flushes strictly in order.
pub fn apply_deferred_book_save<T: StateBackend>(
    state: &T,
    metrics: Option<&torus_telemetry::Metrics>,
    save: DeferredBookSave,
) -> usize {
    let mut acc = SaveTimings::default();
    let mut written = 0usize;
    for b in save.books {
        written += NativeExecContext::<T>::write_drained_book(
            state, metrics, b.drained, b.stops, &b.meta, false, &mut acc,
        );
    }
    written
}

/// Drain one book's journals (mode 2). Enables/refreshes (or drops, budget 0)
/// its private level-hash sponge cache first — byte-identical output in both
/// states. Pure over the book: no state access, safe on any thread.
fn drain_book(
    market_id: MarketId,
    book: &mut OrderBook,
    level_cache_per_book: usize,
    chunked: bool,
    timed: bool,
) -> DrainedBook {
    if chunked {
        // Mode 3: chunked digest (idempotent select; the load path already
        // set it for rebuilt books — this covers books created since, e.g. a
        // market's first order). Never uses the sponge cache: the chunked
        // digest is depth-independent by construction.
        book.set_level_hash_chunked(true);
        book.disable_level_hash_cache();
    } else if level_cache_per_book > 0 {
        // L3: enable/refresh (or actively drop, when budget = 0) the
        // level-hash sponge cache before draining level ops. Byte-identical
        // output in both states.
        book.ensure_level_hash_cache(level_cache_per_book);
    } else {
        book.disable_level_hash_cache();
    }
    let t = timed.then(std::time::Instant::now);
    let row_ops = book.take_row_ops();
    let rows_ns = t.map(|t| t.elapsed().as_nanos()).unwrap_or(0);
    let t = timed.then(std::time::Instant::now);
    let level_ops = book.take_level_ops();
    let levels_ns = t.map(|t| t.elapsed().as_nanos()).unwrap_or(0);
    DrainedBook {
        market_id,
        row_ops,
        level_ops,
        rows_ns,
        levels_ns,
    }
}

/// Drain `books` on up to `workers` scoped threads (LPT by journal length —
/// heaviest book first onto the least-loaded worker; deterministic layout).
/// Returns the drained books sorted by market id plus the per-worker MAX of
/// the row / level drain times (the phase's wall-clock contribution when
/// `timed`). A worker panic is re-raised on the caller (after every worker
/// has been joined) exactly as the serial loop would have panicked.
fn drain_books_parallel(
    books: Vec<(MarketId, &mut OrderBook)>,
    workers: usize,
    level_cache_per_book: usize,
    chunked: bool,
    timed: bool,
) -> (Vec<DrainedBook>, u128, u128) {
    let n = books.len();
    let workers = workers.clamp(1, n.max(1));
    let weights: Vec<(MarketId, usize)> = books
        .iter()
        .map(|(id, b)| (*id, b.journaled_rows() + b.journaled_levels()))
        .collect();
    let assignment = MarketWorkerPool::assign_chunks(&weights, workers);
    let mut chunks: Vec<Vec<(MarketId, &mut OrderBook)>> =
        (0..workers).map(|_| Vec::new()).collect();
    for ((id, book), w) in books.into_iter().zip(assignment) {
        chunks[w].push((id, book));
    }
    let mut out: Vec<DrainedBook> = Vec::with_capacity(n);
    let (mut max_rows_ns, mut max_levels_ns) = (0u128, 0u128);
    std::thread::scope(|s| {
        let handles: Vec<_> = chunks
            .into_iter()
            .filter(|c| !c.is_empty())
            .map(|chunk| {
                s.spawn(move || {
                    let mut drained = Vec::with_capacity(chunk.len());
                    let (mut rows_ns, mut levels_ns) = (0u128, 0u128);
                    for (id, book) in chunk {
                        let d = drain_book(id, book, level_cache_per_book, chunked, timed);
                        rows_ns += d.rows_ns;
                        levels_ns += d.levels_ns;
                        drained.push(d);
                    }
                    (drained, rows_ns, levels_ns)
                })
            })
            .collect();
        torus_state::spawn_count::add(
            torus_state::spawn_count::SpawnSite::SaveBooks,
            handles.len(),
        );
        let mut panic_payload: Option<Box<dyn std::any::Any + Send>> = None;
        for h in handles {
            match h.join() {
                Ok((drained, rows_ns, levels_ns)) => {
                    out.extend(drained);
                    max_rows_ns = max_rows_ns.max(rows_ns);
                    max_levels_ns = max_levels_ns.max(levels_ns);
                }
                Err(payload) => {
                    if panic_payload.is_none() {
                        panic_payload = Some(payload);
                    }
                }
            }
        }
        if let Some(payload) = panic_payload {
            std::panic::resume_unwind(payload);
        }
    });
    out.sort_unstable_by_key(|d| d.market_id);
    (out, max_rows_ns, max_levels_ns)
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
    fn three_is_level_authority_chunked_with_marker_three() {
        assert_eq!(
            parse_book_rows_mode(Some("3".to_string())),
            BookMode::LevelAuthorityChunked
        );
        assert_eq!(
            parse_book_rows_mode(Some(" 3\n".to_string())),
            BookMode::LevelAuthorityChunked
        );
        assert_eq!(BookMode::LevelAuthorityChunked.marker_byte(), 3);
        assert!(BookMode::LevelAuthorityChunked.is_level_authority());
        assert!(BookMode::LevelAuthorityChunked.level_hash_chunked());
        assert!(BookMode::LevelAuthority.is_level_authority());
        assert!(!BookMode::LevelAuthority.level_hash_chunked());
        // Distinct marker bytes across all modes (a wrong-flag restart must be
        // caught by the marker even before content sniffing).
        let bytes: std::collections::BTreeSet<u8> = [
            BookMode::Classic,
            BookMode::OrderRows,
            BookMode::LevelAuthority,
            BookMode::LevelAuthorityChunked,
        ]
        .into_iter()
        .map(BookMode::marker_byte)
        .collect();
        assert_eq!(bytes.len(), 4);
    }

    #[test]
    fn anything_else_stays_classic() {
        for v in ["0", "true", "on", "", "yes", "4", "12", "level"] {
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
//
// UNTOUCHED BLOCKS (r2 resident-books-stale-rebuild): a committed block with
// no native actions and no fee revenue never builds a context, so the books
// are untouched and the holder is ALSO that block's post-state. The pipeline
// advances the holder's height stamp with the standalone applied-height
// marker (`ResidentBooks::advance_untouched`) — otherwise every empty block
// between two native blocks tripped the guard (devnet: resident_height=376,
// block_height=379 → 377/378 were empty) and forced a multi-second full
// reload. Only a direct successor advances; anything else drains the holder.

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

/// bl1 resident-books-untouched-advance kill-switch:
/// `TORUS_RESIDENT_ADVANCE_UNTOUCHED=0` disables advancing the rank8 holder
/// across untouched (empty / non-native) blocks — restoring the pre-candidate
/// behaviour (holder falls behind → full rebuild at the next native block).
/// Anything else (INCLUDING UNSET) keeps the advance ON. Node-local only:
/// the holder is a cache of what the DB already holds; the toggle never
/// changes state, roots or matching.
fn advance_untouched_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| {
        parse_advance_untouched_toggle(std::env::var("TORUS_RESIDENT_ADVANCE_UNTOUCHED").ok())
    })
}

/// Pure parse of the `TORUS_RESIDENT_ADVANCE_UNTOUCHED` value: only `"0"` disables.
fn parse_advance_untouched_toggle(v: Option<String>) -> bool {
    !matches!(v.as_deref().map(str::trim), Some("0"))
}

/// rank8: cross-block resident book state. Owned by the execution pipeline
/// (one per node, behind a Mutex on the exec thread); handed to each block's
/// context via [`NativeExecContext::new_with_modes`] and refilled via
/// [`NativeExecContext::stash_resident`]. Empty/invalidated ⇒ the next
/// context rebuilds from persisted state.
#[derive(Default)]
pub struct ResidentBooks {
    inner: Option<ResidentInner>,
    /// Item 6 Phase 1: the resident rows R and the height whose post-state
    /// they hold. Same guard as the books (take; reuse iff successor and the
    /// applied marker agrees; `invalidate` drops; `advance_untouched`
    /// advances), but NOT gated by `TORUS_RESIDENT_BOOKS`: every node keeps R.
    /// See [`begin_resident`] / [`end_resident`].
    rows: Option<RowsSlot>,
    /// Item 6 step 2: the slot being made by [`end_resident_on_worker`] on
    /// its worker thread (the worker owns R, the delta and the sums; it never
    /// touches this holder). Joined into `rows` by [`Self::settle_rows`],
    /// which every reader / writer of `rows` calls first, so nothing can see
    /// the slot of the block before the worker finished it.
    rows_pending: Option<PendingRows>,
    /// Item 6 Phase 1: R builds (cold start or guard trip) and
    /// `end_resident` calls that found another clone of R alive (test /
    /// ops introspection).
    rows_builds: u64,
    rows_shared_fallbacks: u64,
}

/// Item 6 step 2: one block's `end_resident` running on a worker thread.
struct PendingRows {
    handle: std::thread::JoinHandle<RowsSlot>,
    height: u64,
    /// For the wait timer at the join.
    metrics: Option<Arc<torus_telemetry::Metrics>>,
}

struct RowsSlot {
    rows: Arc<torus_state::ResidentRows>,
    /// C2: the mark table / configs of block `height` (None: that block had
    /// no table — the next one takes a new version).
    marks: Option<BlockMarksState>,
    /// C3: the traders' margin sums over `rows` (plan 2.4). Built empty with
    /// R, dropped with it.
    sums: SumsCache,
    /// C7: `rows`' positions decoded per trader. Built with R, updated by
    /// each block's delta, dropped with it.
    positions: TraderPositions,
    height: u64,
}

struct ResidentInner {
    books: HashMap<MarketId, OrderBook>,
    /// Item 6 Phase 2 P2-1: the books' cancel-all index (`None`: not built
    /// since the last load; the next cancel-all builds it from the books).
    trader_markets: Option<TraderMarkets>,
    /// Post-block global order-id high-water mark (replaces the load scan).
    next_global_order_id: u128,
    /// The block height whose POST-state this reflects (staleness guard).
    height: u64,
}

/// Item 6 Phase 2 P2-1 (tests): trader -> markets, ascending.
#[cfg(any(test, feature = "test-reference-paths"))]
pub type TraderIndexSnapshot = std::collections::BTreeMap<Address, Vec<MarketId>>;

/// Item 6 Phase 2 P2-1: the node-local cancel-all index, trader -> the
/// markets (ascending) where the trader may have resting orders, pending
/// stops or reduce-only entries. A SUPERSET of [`OrderBook::traders_present`]
/// over all books: fed from each book's `new_traders` log at every site where
/// an order can rest or a stop be stored ([`Self::absorb`]), never removed
/// eagerly; a cancel-all of the trader takes the markets it visits out (the
/// trader has nothing left there). Built from the books at the first
/// cancel-all after a load ([`NativeExecContext::cancel_index`]), carried
/// with the resident books. Never hashed, never persisted: a missing entry
/// would leave orders on a book, an extra one costs one probe.
#[derive(Default)]
struct TraderMarkets {
    map: HashMap<Address, Vec<MarketId>>,
    /// (trader, market) entries over all of `map` (s104 size gauge).
    entries: usize,
}

impl TraderMarkets {
    /// The index of `books` as they are (their logs are emptied: the scan
    /// already sees everything logged).
    fn build(books: &mut HashMap<MarketId, OrderBook>) -> Self {
        for book in books.values_mut() {
            book.drain_new_traders();
        }
        let mut index = Self::default();
        for (&market, book) in books.iter() {
            for trader in book.traders_present() {
                index.insert(*trader, market);
            }
        }
        index
    }

    fn insert(&mut self, trader: Address, market: MarketId) {
        let markets = self.map.entry(trader).or_default();
        if let Err(at) = markets.binary_search(&market) {
            markets.insert(at, market);
            self.entries += 1;
        }
    }

    /// `trader`'s markets, ascending, taken out of the index.
    fn take(&mut self, trader: &Address) -> Vec<MarketId> {
        let markets = self.map.remove(trader).unwrap_or_default();
        self.entries -= markets.len();
        markets
    }

    /// Whether the index lists `market` for `trader`; takes it out.
    fn remove(&mut self, trader: &Address, market: MarketId) -> bool {
        let Some(markets) = self.map.get_mut(trader) else {
            return false;
        };
        let Ok(at) = markets.binary_search(&market) else {
            return false;
        };
        markets.remove(at);
        self.entries -= 1;
        if markets.is_empty() {
            self.map.remove(trader);
        }
        true
    }

    /// (s104 size gauges) `((trader, market) entries, traders)`.
    fn size(&self) -> (u64, u64) {
        (self.entries as u64, self.map.len() as u64)
    }

    /// Feed `book`'s log (market `market`) into `index`. Called wherever an
    /// order can rest or a stop be stored: after the single-action
    /// placement, for every book the batch matching hands back, after a
    /// modify. Without an index the log is dropped: the build scans the books.
    fn absorb(index: &mut Option<Self>, book: &mut OrderBook, market: MarketId) {
        match index {
            Some(index) => {
                for trader in book.drain_new_traders() {
                    index.insert(trader, market);
                }
            }
            None => {
                book.drain_new_traders();
            }
        }
    }

    #[cfg(any(test, feature = "test-reference-paths"))]
    fn snapshot(&self) -> TraderIndexSnapshot {
        self.map.iter().map(|(t, ms)| (*t, ms.clone())).collect()
    }
}

/// P2-1 (tests): `(carried, rebuilt)` — `index` (`None`: not built)
/// and the index a load would build from `books` now (exact).
#[cfg(any(test, feature = "test-reference-paths"))]
fn trader_index_snapshot(
    index: Option<&TraderMarkets>,
    books: &HashMap<MarketId, OrderBook>,
) -> (Option<TraderIndexSnapshot>, TraderIndexSnapshot) {
    let mut rebuilt = TraderIndexSnapshot::new();
    for (&market, book) in books {
        for trader in book.traders_present() {
            let markets = rebuilt.entry(*trader).or_default();
            if let Err(at) = markets.binary_search(&market) {
                markets.insert(at, market);
            }
        }
    }
    (index.map(TraderMarkets::snapshot), rebuilt)
}

impl ResidentBooks {
    /// Item 6 Phase 2 P2-1 (tests): `(carried, rebuilt)` — the cancel-all
    /// index carried with the books (`None`: not built since the last load)
    /// and the one a load would build from them now. The carried index
    /// covers the rebuilt one (it may also list markets where a trader has
    /// nothing left). `None`: no resident books.
    #[cfg(any(test, feature = "test-reference-paths"))]
    pub fn trader_index(&self) -> Option<(Option<TraderIndexSnapshot>, TraderIndexSnapshot)> {
        self.inner
            .as_ref()
            .map(|inner| trader_index_snapshot(inner.trader_markets.as_ref(), &inner.books))
    }

    /// Drop any resident state (books and rows) — the next block rebuilds
    /// from the DB. Waits for an `end_resident` worker first (step 2).
    pub fn invalidate(&mut self) {
        self.settle_rows();
        self.inner = None;
        self.rows = None;
    }

    /// Item 6 step 2: wait for the `end_resident` worker (if one runs) and
    /// put the slot it made into the holder; observes
    /// `exec_end_resident_wait_seconds` (the time this thread waited). A
    /// worker that panicked leaves the slot empty: the next native block
    /// rebuilds R, as after a failed block. Every access to the rows slot
    /// calls this first.
    pub fn settle_rows(&mut self) {
        let Some(PendingRows { handle, height, metrics }) = self.rows_pending.take() else {
            return;
        };
        let timer = std::time::Instant::now();
        let joined = handle.join();
        if let Some(m) = metrics.as_deref() {
            m.exec_end_resident_wait_seconds.observe(timer.elapsed().as_secs_f64());
        }
        match joined {
            Ok(slot) => self.rows = Some(slot),
            Err(_) => {
                tracing::error!(
                    height,
                    "item 6: end_resident worker panicked — dropping R (next native block rebuilds it)"
                );
                self.rows = None;
            }
        }
    }

    /// Item 6 step 2: an `end_resident` worker has not been joined yet
    /// (test / ops introspection; does not wait).
    pub fn rows_in_flight(&self) -> bool {
        self.rows_pending.is_some()
    }

    /// Item 6 Phase 1: the resident rows R between blocks (None = drained or
    /// taken by a block in progress). Waits for an `end_resident` worker.
    pub fn rows(&mut self) -> Option<&torus_state::ResidentRows> {
        self.settle_rows();
        self.rows.as_ref().map(|s| &*s.rows)
    }

    /// Item 6 Phase 1: the block height whose post-state R holds. Waits for
    /// an `end_resident` worker.
    pub fn rows_height(&mut self) -> Option<u64> {
        self.settle_rows();
        self.rows.as_ref().map(|s| s.height)
    }

    /// Item 6 Phase 1: R builds so far (1 per process in a normal sequence).
    pub fn rows_builds(&self) -> u64 {
        self.rows_builds
    }

    /// Item 6 Phase 1: `end_resident` calls that could not take R back
    /// (`Arc::get_mut` failed: a clone was alive). 0 in the normal sequence.
    pub fn rows_shared_fallbacks(&self) -> u64 {
        self.rows_shared_fallbacks
    }

    /// Item 6 C7: whether the slot's decoded positions equal a cold decode
    /// of its R (`None`: no slot). Test / ops introspection.
    pub fn trader_positions_match_rows(&mut self) -> Option<bool> {
        self.settle_rows();
        self.rows.as_ref().map(|s| s.positions.same_as(&TraderPositions::build(&s.rows)))
    }

    /// Item 6 C5 (tests): empty the slot's sums cache (R, its decoded
    /// positions and the mark state stay), so the next block values every
    /// trader afresh. Returns the entries dropped. Waits for an
    /// `end_resident` worker. Not called on the node path.
    pub fn drop_sums_cache(&mut self) -> usize {
        self.settle_rows();
        self.rows.as_mut().map_or(0, |s| std::mem::take(&mut s.sums.map).len())
    }

    /// Whether the holder currently carries state (test/ops introspection).
    pub fn is_populated(&self) -> bool {
        self.inner.is_some()
    }

    /// The block height whose post-state the holder reflects (None = drained).
    pub fn height(&self) -> Option<u64> {
        self.inner.as_ref().map(|i| i.height)
    }

    /// r2 resident-books-stale-rebuild: a committed block that skipped the
    /// native path entirely (no native actions, no fee revenue — the pipeline
    /// never builds a context for it) leaves every book untouched, so the
    /// holder's state is ALSO that block's post-state. Advance the height
    /// stamp iff `block_height` is the holder's direct successor and return
    /// true. Call it right after the standalone applied-height marker write
    /// for that block, so memory and the marker move together.
    ///
    /// Anything else — same height replayed, a skipped height, an empty
    /// holder — is not a normal sequence: the holder is DRAINED (returns
    /// false) and the next context rebuilds from the DB, which is always
    /// authoritative. Strict by design: a wrong "not stale" here would be a
    /// correctness bug; a needless rebuild is only a stall.
    pub fn advance_untouched(&mut self, block_height: u64) -> bool {
        self.advance_untouched_with(advance_untouched_enabled(), block_height)
    }

    /// [`Self::advance_untouched`] with the kill-switch value passed
    /// explicitly (tests; the env-reading wrapper above is what the pipeline
    /// calls). `enabled == false` is a pure no-op: the holder is neither
    /// advanced nor drained, exactly the pre-candidate sequence.
    pub fn advance_untouched_with(&mut self, enabled: bool, block_height: u64) -> bool {
        // Item 6 Phase 1: the rows slot advances (or drains) by the same rule,
        // independent of the books' kill switch (R has no runtime switch).
        // Step 2: the previous block's slot first (its worker may still run).
        self.settle_rows();
        match self.rows.as_mut() {
            Some(slot) if slot.height + 1 == block_height => slot.height = block_height,
            Some(slot) => {
                tracing::warn!(
                    rows_height = slot.height,
                    block_height,
                    "item 6: untouched block is not the resident rows' successor — draining R \
                     (next native block rebuilds it)"
                );
                self.rows = None;
            }
            None => {}
        }
        if !enabled {
            return false;
        }
        match self.inner.as_mut() {
            Some(inner) if inner.height + 1 == block_height => {
                inner.height = block_height;
                true
            }
            Some(inner) => {
                tracing::warn!(
                    resident_height = inner.height,
                    block_height,
                    "rank8: untouched block is not the resident holder's successor — draining \
                     the holder (next block rebuilds from persisted state)"
                );
                self.inner = None;
                false
            }
            None => false,
        }
    }
}

/// The DB's native applied-height marker as `state` sees it (through an
/// overlay: own pending -> parent layer -> DB). Staleness-guard input for the
/// resident books and the resident rows.
fn applied_marker<B: StateBackend>(state: &B) -> Option<u64> {
    use torus_state::cf::{CF_CONSENSUS_META, META_NATIVE_APPLIED_HEIGHT};
    let bytes = state
        .get_cf_raw(CF_CONSENSUS_META, META_NATIVE_APPLIED_HEIGHT)
        .ok()
        .flatten()?;
    Some(u64::from_be_bytes(bytes.try_into().ok()?))
}

/// Item 6 Phase 1: one block's handle on the resident rows R, from
/// [`begin_resident`] to [`end_resident`].
#[derive(Debug)]
pub struct ResidentBlock {
    height: u64,
    attached: bool,
    rebuilt: bool,
    /// C2: from `begin_resident`, the previous block's mark state when the
    /// slot was reused (moved into the context by
    /// [`NativeExecContext::attach_resident_block`]); from
    /// [`NativeExecContext::detach_resident_block`], this block's (stashed
    /// by `end_resident`).
    marks: Option<BlockMarksState>,
    /// C3: the slot's sums cache (empty when R was built) and, after
    /// [`NativeExecContext::detach_resident_block`], this block's memo;
    /// `end_resident` merges them. `None`: R not attached (no cache).
    sums: Option<BlockSums>,
    /// Item 6 cut 5: [`Self::drop_later`].
    retired: Retired,
    /// Step 2 tests: runs first inside the `end_resident` worker (hold it,
    /// or panic in it).
    #[cfg(test)]
    worker_hook: Option<WorkerHook>,
}

/// Item 6 cut 5: values a block no longer needs, never read again, dropped
/// with its `end_resident` work ([`ResidentBlock::drop_later`]).
#[derive(Default)]
struct Retired(Vec<Box<dyn Send>>);

impl std::fmt::Debug for Retired {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Retired({})", self.0.len())
    }
}

/// Step 2 tests: see [`ResidentBlock::worker_hook`].
#[cfg(test)]
pub(crate) struct WorkerHook(pub(crate) Box<dyn FnOnce() + Send>);

#[cfg(test)]
impl std::fmt::Debug for WorkerHook {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("WorkerHook")
    }
}

impl ResidentBlock {
    /// R is attached to the block's overlay (false: no holder, or the build
    /// failed — reads go to the DB, today's path).
    pub fn attached(&self) -> bool {
        self.attached
    }

    /// R was built for this block (cold start or staleness guard trip).
    pub fn rebuilt(&self) -> bool {
        self.rebuilt
    }

    /// Item 6 cut 5: drop `value` with this block's `end_resident` work — on
    /// its worker under [`end_resident_on_worker`] (off the execution
    /// thread), else where the block is dropped. For the block's large
    /// leftovers (the batch results); dropping memory has no other effect.
    pub fn drop_later(&mut self, value: impl Send + 'static) {
        self.retired.0.push(Box::new(value));
    }
}

/// Item 6 Phase 1: start of a native block. TAKE the holder's rows slot and
/// reuse it iff it holds the post-state of `height - 1` (slot height + 1 ==
/// `height`, and the applied marker — read through `overlay`, i.e. pending ->
/// parent -> DB — equals the slot height when present, and C6a: the overlay's
/// parent layer, if any, is the slot height's frozen set); otherwise build R from
/// `overlay` (DB + parent layer: the previous block's post-state). Attach R to
/// `overlay`, which must not have been cloned yet. `holder: None` = today's
/// path (nothing attached). Called by app.rs and the harnesses
/// (`perf_equivalence_golden`, `ubench_econ`).
///
/// A block that never reaches [`end_resident`] (fatal, failed hand-off)
/// leaves the slot empty: the next block rebuilds.
pub fn begin_resident(
    holder: Option<&mut ResidentBooks>,
    overlay: &mut NativeStateOverlay,
    height: u64,
    metrics: Option<&torus_telemetry::Metrics>,
) -> ResidentBlock {
    let mut block = ResidentBlock {
        height,
        attached: false,
        rebuilt: false,
        marks: None,
        sums: None,
        retired: Retired::default(),
        #[cfg(test)]
        worker_hook: None,
    };
    let Some(holder) = holder else {
        return block;
    };
    // Step 2: the previous block's slot may still be in its `end_resident`
    // worker — wait for it here, before the slot is taken (never later).
    holder.settle_rows();
    debug_assert!(!overlay.has_resident(), "begin_resident on an overlay that already has R");
    let reused = holder.rows.take().and_then(|slot| {
        let marker = applied_marker(&*overlay);
        let height_ok = slot.height + 1 == height;
        let marker_ok = marker.is_none_or(|m| m == slot.height);
        // C6a (B0): the overlay reads R's CFs without its parent layer, so R
        // must already hold it: the parent (if any) is the block R reflects.
        let parent_ok = overlay.parent_height().is_none_or(|p| p == slot.height);
        if height_ok && marker_ok && parent_ok {
            block.marks = slot.marks;
            block.sums = Some(BlockSums { records: Some(slot.positions), ..BlockSums::new(slot.sums) });
            return Some(slot.rows);
        }
        tracing::warn!(
            rows_height = slot.height,
            height,
            applied_marker = marker,
            parent_height = overlay.parent_height(),
            "item 6: resident rows stale (height/marker/parent mismatch) — rebuilding R"
        );
        None
    });
    let rows = match reused {
        Some(rows) => rows,
        None => {
            let timer = std::time::Instant::now();
            match torus_state::ResidentRows::build(&*overlay) {
                Ok(rows) => {
                    holder.rows_builds += 1;
                    block.rebuilt = true;
                    // C3: an empty sums cache; C7: R's positions decoded.
                    block.sums =
                        Some(BlockSums { records: Some(TraderPositions::build(&rows)), ..BlockSums::default() });
                    if let Some(m) = metrics {
                        m.exec_resident_rows_rebuilds.inc();
                        m.exec_resident_rows_build_seconds
                            .observe(timer.elapsed().as_secs_f64());
                    }
                    tracing::info!(
                        height,
                        rows = rows.len(),
                        bytes = rows.bytes(),
                        build_ms = timer.elapsed().as_secs_f64() * 1e3,
                        "item 6: resident rows built"
                    );
                    Arc::new(rows)
                }
                Err(e) => {
                    // Reads fall through to the DB (today's path) for this block.
                    tracing::error!(%e, height, "item 6: resident rows build failed — block reads the DB");
                    return block;
                }
            }
        }
    };
    if let Some(m) = metrics {
        m.exec_resident_rows.set(rows.len() as i64);
        m.exec_resident_rows_bytes.set(rows.bytes() as i64);
    }
    overlay.attach_resident(rows);
    block.attached = true;
    block
}

/// Item 6 Phase 1: end of a native block, after the hand-off (pipelined) or
/// the flush (serial; `ok` = it succeeded) and after every clone of `overlay`
/// (the context's managers) was dropped. `delta` = `overlay.own_pending_delta()`
/// taken before `freeze` / the flush. Detaches R, takes it back with
/// `Arc::get_mut`, applies `delta` and stashes it at `block`'s height. If a
/// clone of R is still alive (never in the normal sequence) or `!ok`, the
/// slot stays empty and the next block rebuilds.
///
/// Runs on the calling thread; [`end_resident_on_worker`] is the same work on
/// a worker thread (app.rs).
pub fn end_resident(
    holder: &mut ResidentBooks,
    block: ResidentBlock,
    overlay: &mut NativeStateOverlay,
    delta: impl Into<BlockDelta>,
    ok: bool,
    metrics: Option<&torus_telemetry::Metrics>,
) {
    let delta = delta.into();
    holder.settle_rows();
    let timer = std::time::Instant::now();
    if let Some(job) = end_resident_take(holder, block, overlay, delta, ok) {
        holder.rows = Some(job.run(metrics));
    }
    // Item 6 step 1: the whole upkeep (the delta and the memo dropped included).
    if let Some(m) = metrics {
        m.exec_end_resident_seconds.observe(timer.elapsed().as_secs_f64());
    }
}

/// Item 6 step 2: [`end_resident`] with its work (R applying `delta`, the
/// decoded positions and the sums carry) on a new worker thread, so it
/// overlaps the next block's work up to its [`begin_resident`]. The checks
/// (`ok`, R attached, no other clone of R alive) run here, on the calling
/// thread, exactly as in `end_resident`; the worker then owns R, the delta and
/// the block's sums, and never touches `holder`. The slot it makes enters
/// `holder` at the next access of the rows slot ([`ResidentBooks::settle_rows`]:
/// `begin_resident`, `advance_untouched`, `invalidate`, the introspection
/// methods), so every later block sees exactly the slot `end_resident` would
/// have stashed. A failed spawn or a panic in the worker leaves the slot
/// empty (the next native block rebuilds R). `exec_end_resident_seconds` is
/// observed by the worker (its time plus the checks here).
pub fn end_resident_on_worker(
    holder: &mut ResidentBooks,
    block: ResidentBlock,
    overlay: &mut NativeStateOverlay,
    delta: impl Into<BlockDelta>,
    ok: bool,
    metrics: Option<Arc<torus_telemetry::Metrics>>,
) {
    let delta = delta.into();
    holder.settle_rows();
    let timer = std::time::Instant::now();
    let Some(job) = end_resident_take(holder, block, overlay, delta, ok) else {
        if let Some(m) = metrics.as_deref() {
            m.exec_end_resident_seconds.observe(timer.elapsed().as_secs_f64());
        }
        return;
    };
    let height = job.height;
    let checks = timer.elapsed();
    let worker_metrics = metrics.clone();
    let spawned = std::thread::Builder::new().name("torus-end-resident".into()).spawn(move || {
        let timer = std::time::Instant::now();
        let slot = job.run(worker_metrics.as_deref());
        if let Some(m) = worker_metrics.as_deref() {
            m.exec_end_resident_seconds.observe((checks + timer.elapsed()).as_secs_f64());
        }
        slot
    });
    match spawned {
        Ok(handle) => {
            torus_state::spawn_count::add(torus_state::spawn_count::SpawnSite::EndResident, 1);
            holder.rows_pending = Some(PendingRows {
                handle,
                height,
                metrics,
            })
        }
        Err(e) => {
            // The job (R included) was dropped with the closure.
            tracing::error!(%e, height, "item 6: end_resident worker spawn failed — dropping R (next native block rebuilds it)");
        }
    }
}

/// Item 6 cut 5: the block's own writes and tombstones of R's CFs as
/// `end_resident` gets them.
pub enum BlockDelta {
    /// Taken by the caller (`own_pending_delta`, before `freeze` / the flush).
    Taken(torus_state::ResidentDelta),
    /// The block's frozen pending set (the pipelined hand-off's): the delta is
    /// taken from it where `end_resident` runs — the worker, off the
    /// execution thread ([`torus_state::FrozenPending::resident_delta`],
    /// equal to `own_pending_delta` just before the freeze).
    Frozen(Arc<torus_state::FrozenPending>),
}

impl From<torus_state::ResidentDelta> for BlockDelta {
    fn from(delta: torus_state::ResidentDelta) -> Self {
        Self::Taken(delta)
    }
}

impl BlockDelta {
    fn into_delta(self) -> torus_state::ResidentDelta {
        match self {
            Self::Taken(delta) => delta,
            Self::Frozen(frozen) => frozen.resident_delta(),
        }
    }
}

/// Item 6 step 2: `end_resident`'s inputs once its checks passed — owned by
/// whichever thread runs [`Self::run`].
struct EndResidentJob {
    /// R, with no other clone alive (checked by [`end_resident_take`]).
    rows: Arc<torus_state::ResidentRows>,
    height: u64,
    marks: Option<BlockMarksState>,
    sums: Option<BlockSums>,
    delta: BlockDelta,
    /// Dropped by whichever thread runs the job.
    retired: Retired,
    #[cfg(test)]
    worker_hook: Option<WorkerHook>,
}

/// `end_resident`'s checks: detach R from `overlay`; `None` (slot stays
/// empty) if R was not attached, `!ok`, or another clone of R is alive.
fn end_resident_take(
    holder: &mut ResidentBooks,
    block: ResidentBlock,
    overlay: &mut NativeStateOverlay,
    delta: BlockDelta,
    ok: bool,
) -> Option<EndResidentJob> {
    let mut rows = overlay.detach_resident()?;
    if !block.attached || !ok {
        return None;
    }
    if Arc::get_mut(&mut rows).is_none() {
        holder.rows_shared_fallbacks += 1;
        tracing::warn!(
            height = block.height,
            "item 6: resident rows still shared at end of block — dropping R (next block rebuilds)"
        );
        return None;
    }
    let ResidentBlock { height, marks, sums, retired, .. } = block;
    Some(EndResidentJob {
        rows,
        height,
        marks,
        sums,
        delta,
        retired,
        #[cfg(test)]
        worker_hook: block.worker_hook,
    })
}

impl EndResidentJob {
    /// R takes the block's delta; the decoded positions and the sums follow.
    fn run(self, metrics: Option<&torus_telemetry::Metrics>) -> RowsSlot {
        let EndResidentJob {
            mut rows,
            height,
            marks,
            sums,
            delta,
            retired,
            #[cfg(test)]
            worker_hook,
        } = self;
        drop(retired);
        #[cfg(test)]
        if let Some(hook) = worker_hook {
            (hook.0)();
        }
        let r = Arc::get_mut(&mut rows).expect("end_resident_take checked R is not shared");
        // C3: the block's memo joins the slot's sums; item 6 M1: every trader
        // whose positions the block wrote or deleted has its sums moved on to
        // its rows after the block, else loses them (step 1: while its decoded
        // positions follow the delta, below).
        let (mut carry, records) = match sums {
            Some(mut s) => {
                let records = s.records.take();
                (Some(s.into_carry(marks.as_ref())), records)
            }
            None => (None, None),
        };
        // Item 6 cut 5: from the frozen set (pipelined), here on the worker.
        let delta = delta.into_delta();
        let timer = std::time::Instant::now();
        r.apply(&delta);
        if let Some(m) = metrics {
            m.exec_end_resident_rows_seconds.observe(timer.elapsed().as_secs_f64());
            m.exec_resident_rows.set(r.len() as i64);
            m.exec_resident_rows_bytes.set(r.bytes() as i64);
        }
        // C7: the decoded positions follow the delta (decoded cold if the
        // context kept the block's state).
        let timer = std::time::Instant::now();
        let positions = match records {
            Some(mut p) => {
                match carry.as_mut() {
                    Some(c) => p.apply(&delta, r, Some(&mut |t: &Address, report| c.trader(t, report))),
                    None => p.apply(&delta, r, None),
                }
                p
            }
            None => {
                if let Some(c) = carry.as_mut() {
                    c.drop_written(&delta);
                }
                TraderPositions::build(r)
            }
        };
        if let Some(m) = metrics {
            m.exec_end_resident_positions_seconds.observe(timer.elapsed().as_secs_f64());
        }
        let sums = carry.map(|c| c.cache).unwrap_or_default();
        drop(delta);
        RowsSlot { rows, marks, sums, positions, height }
    }
}

/// s63 runtime toggle, default ON since s64: each maximal run of consecutive
/// `CancelAllOrders` in Phase 1 (deferred places do not break a run) executes
/// with one book compaction per touched level instead of one per action.
/// `TORUS_CANCEL_BATCH=0` is the kill switch back to the per-action loop.
/// State-equivalent either way; read once per process.
fn cancel_batch_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| parse_cancel_batch_toggle(std::env::var("TORUS_CANCEL_BATCH").ok()))
}

/// Pure parse of the `TORUS_CANCEL_BATCH` value: only `"0"` disables.
fn parse_cancel_batch_toggle(v: Option<String>) -> bool {
    !matches!(v.as_deref().map(str::trim), Some("0"))
}

#[cfg(test)]
mod cancel_batch_toggle_tests {
    use super::parse_cancel_batch_toggle;

    #[test]
    fn default_is_on() {
        assert!(parse_cancel_batch_toggle(None));
    }

    #[test]
    fn zero_disables() {
        assert!(!parse_cancel_batch_toggle(Some("0".to_string())));
        assert!(!parse_cancel_batch_toggle(Some(" 0 ".to_string())));
    }

    #[test]
    fn anything_else_stays_on() {
        for v in ["1", " 1 ", "true", "on", "", "yes", "2"] {
            assert!(parse_cancel_batch_toggle(Some(v.to_string())), "{v}");
        }
    }
}

#[cfg(test)]
#[path = "cancel_batch_exec_tests.rs"]
mod cancel_batch_exec_tests;

#[cfg(any(test, feature = "test-reference-paths"))]
#[path = "reference_paths.rs"]
mod reference_paths;

#[cfg(test)]
#[path = "block_marks_tests.rs"]
mod block_marks_tests;

#[cfg(test)]
#[path = "sums_cache_tests.rs"]
mod sums_cache_tests;

#[cfg(test)]
#[path = "end_resident_worker_tests.rs"]
mod end_resident_worker_tests;

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

#[cfg(test)]
mod advance_untouched_toggle_tests {
    use super::{parse_advance_untouched_toggle, ResidentBooks};

    #[test]
    fn default_is_on() {
        assert!(parse_advance_untouched_toggle(None));
    }

    #[test]
    fn zero_disables() {
        assert!(!parse_advance_untouched_toggle(Some("0".to_string())));
        assert!(!parse_advance_untouched_toggle(Some(" 0 ".to_string())));
    }

    #[test]
    fn anything_else_stays_on() {
        for v in ["1", "true", "on", "", "yes", "2"] {
            assert!(parse_advance_untouched_toggle(Some(v.to_string())), "{v}");
        }
    }

    /// Kill-switch semantics: with the advance disabled the holder is left
    /// EXACTLY as before the candidate — not advanced, not drained — so the
    /// next native block trips the height guard and rebuilds from the DB.
    #[test]
    fn disabled_advance_leaves_holder_untouched() {
        let mut holder = ResidentBooks::default();
        assert!(!holder.advance_untouched_with(false, 1));
        assert_eq!(holder.height(), None);
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
    pub dirty_books: HashSet<MarketId>,
    /// Per-market margin configuration.
    pub margin_configs: HashMap<MarketId, MarketMarginConfig>,
    /// Item 6 C2: the block's mark table, filled by `begin_block_oracle`
    /// (`None` before it, or when it failed: marks read the oracle).
    block_marks: Option<BlockMarks>,
    /// Item 6 C2: the previous block's mark state from the resident rows slot
    /// ([`Self::attach_resident_block`]), consumed by `begin_block_oracle`.
    prev_marks: Option<BlockMarksState>,
    /// Item 6 C3: the block's margin sums cache ([`Self::attach_resident_block`]
    /// to [`Self::detach_resident_block`]; `None`: every valuation builds).
    sums: Option<BlockSums>,
    /// Item 6 Phase 2 P2-1: the cancel-all index of `order_books`
    /// ([`TraderMarkets`]; `None` until the first cancel-all after a load
    /// builds it, carried in the resident holder).
    trader_markets: Option<TraderMarkets>,
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
    /// Per-block fill counter (`trade_id` of each fill); counts every fill,
    /// with trade history on or off.
    pub trade_index: u32,

    /// O3: when set, the block's fills (`pending_fills`) are left for the
    /// caller to take after all exec phases and to write as packed rows
    /// (`trade_rows::encode_block`, `CF_NATIVE_TRADES` / `CF_NATIVE_USER_TRADES`
    /// — node-local CFs outside the native consensus root, never read during
    /// execution), e.g. on a background writer. Off by default: each
    /// `execute_batch` then writes the rows of all the block's fills so far
    /// inline.
    pub defer_trades: bool,
    /// s77: when false, no trade-history rows are written and no fills are
    /// recorded unless `record_fills` is set (`TORUS_TRADE_HISTORY=0`,
    /// node-local; for validators that do not serve trade-history RPC).
    /// Default true.
    pub trade_history: bool,
    /// s80: a stream wants this block's fills: record them even with trade
    /// history off, AND record each fill's stream-only `FillExtras` (order
    /// ids, position effects). When false, settling does exactly the s78 work
    /// (no effect bookkeeping, no extras). Output-only either way: state,
    /// results and the native root do not depend on it. Default false.
    pub record_fills: bool,
    /// This block's fills in `trade_index` order (s77 packed trade rows).
    pending_fills: Vec<TradeFill>,
    /// s80: the `FillExtras` of `pending_fills` — exactly as long and
    /// index-aligned while `record_fills` is set, else empty. Cleared and
    /// taken together with `pending_fills`.
    pending_extras: Vec<FillExtras>,
    /// Block height of `pending_fills` (inline mode clears them on a new height).
    fills_block: u64,
    /// Inline mode: how many of `pending_fills` already have rows written.
    inline_fills_written: usize,
    /// s80: `[taker, maker]` effect of each fill of the order being settled
    /// (sequential and single-action paths), only while `record_fills`;
    /// reused so settling allocates nothing per order.
    fill_effects_scratch: Vec<[FillEffect; 2]>,

    /// Optional metrics handle for Prometheus instrumentation.
    pub metrics: Option<std::sync::Arc<torus_telemetry::Metrics>>,
    /// adl-budget A7, node-local and proof-only (the node sets it from
    /// `TORUS_LIQ_VALUE_SUM=1`): with metrics attached, every liquidation step
    /// also walks all balances and positions for the value-sum gauge and log
    /// line. Never read by execution. Default false.
    pub liq_value_sum: bool,

    /// T1.5: set (never cleared) when a market worker panicked mid-matching.
    /// The panicking worker consumed its market's `OrderBook`, so this block's
    /// post-state is unreconstructable — the committer MUST treat this as
    /// fatal (fail-stop the node), never flush state or mark the block applied.
    pub fatal_error: Option<String>,
    /// R02: the fault channel of the readers that hold no `&mut` context
    /// ([`AccountReader`]: the oracle mark and price-band reads, also on the
    /// Phase 2 / 3 worker threads). The first LOCAL fault
    /// (`CoreError::is_local_fault`) a reader hits is kept here; the reader
    /// then returns its "absent" value, which is never used because the
    /// block fail-stops: [`Self::take_fatal_error`] (the committer's check)
    /// returns it like `fatal_error`.
    pub reader_fault: std::sync::OnceLock<String>,

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
    /// Mode-2 save drain worker cap (`TORUS_SAVE_BOOKS_WORKERS`; default =
    /// host parallelism). 1 = serial exact-today loop. Node-local, output
    /// byte-identical for any value; tests may override per ctx.
    pub save_books_workers: usize,
    /// Mode-2 save parallel-drain work gate (`TORUS_SAVE_BOOKS_MIN_OPS`):
    /// journaled rows + levels across dirty books must reach this to spawn
    /// workers. Perf-only; tests may override per ctx.
    pub save_books_min_ops: usize,
    /// Worker threads the LAST `save_order_books` drained on (1 = serial
    /// path, including modes 0/1 and gated-out blocks). Test/metrics hook.
    last_save_workers: usize,
    /// L3 save-books attribution (µbench-only, feature `save-timings`): when
    /// set, the mode-2 `save_order_books` arm accumulates per-sub-step
    /// timings into `last_save_timings`. Compiled out of production builds.
    #[cfg(feature = "save-timings")]
    pub collect_save_timings: bool,
    /// L3 save-books attribution (µbench-only): last block's sub-step timings.
    #[cfg(feature = "save-timings")]
    pub last_save_timings: SaveTimings,
    /// s74 boot profile: phase timings of this ctx's mode-2/3 book load
    /// (all zero when the books came from the resident holder).
    pub load_timings: LoadTimings,

    /// r6 engine-untimed-attribution: nanosecond accumulators for the exec
    /// sub-phases, summed across every `execute_batch` call this context
    /// serves (a block makes two: pre-EVM and post-EVM). Node-local
    /// instrumentation only — never read by execution, never part of the
    /// state root. The caller (`app.rs`) observes them once per block and
    /// derives the residual against the engine wall clock.
    pub phase_accum: ExecPhaseAccum,

    /// bl1 exec-chain-sub-100-attribution: PRODUCTION (always compiled)
    /// nanosecond split of `save_order_books` into its two passes. Unlike the
    /// `save-timings` accumulators above — a µbench-only feature compiled out
    /// of the node — these ship in release builds (two `Instant::now()` pairs
    /// per block) because deciding whether pass 2 is worth moving off the exec
    /// thread needs the PRODUCTION share, not a µbench's. Node-local
    /// instrumentation: never read by execution, never part of the state root.
    pub save_split: SaveSplitAccum,

    /// Item 6 Phase 2 step 0.4: test-only reference switch (no runtime flag,
    /// D16). `true` runs every cancel-all (user runs and single actions, the
    /// liquidation step's cancels) one action at a time through the frozen
    /// full scan of every book (`reference_paths.rs`
    /// `cancel_orders_and_stops_full_scan`), the reference P2-1's book index
    /// is compared against. Other crates' tests reach it through the
    /// test-only `test-reference-paths` feature (plan 9.8). Default off.
    #[cfg(any(test, feature = "test-reference-paths"))]
    pub test_cancel_all_full_scan: bool,
    /// Item 6 Phase 2 (plan 9.8): test-only reference switch. `true` flushes
    /// the batch position cache through the frozen per-row reference
    /// (`PositionCache::flush_all_per_row`), the reference P2-2's batch
    /// overlay writes are compared against. Default off.
    #[cfg(any(test, feature = "test-reference-paths"))]
    pub test_flush_per_row: bool,
}

/// bl1 exec-chain-sub-100-attribution: per-block nanosecond split of
/// `save_order_books` (two-pass / level-authority path only).
///
/// The two spans are DISJOINT and together cover the whole two-pass save, so
/// `drain_ns + write_ns` is bounded by `exec_save_books_seconds`. Only pass 2
/// could ever move off the exec thread: pass 1 reads the LIVE book levels the
/// next block's engine mutates.
#[derive(Default, Clone, Copy, Debug, PartialEq, Eq)]
pub struct SaveSplitAccum {
    /// Pass 1: journal drain (`take_row_ops` / `take_level_ops`) + level
    /// digests, inline or on the scoped drain workers (wall clock of the
    /// whole pass, so the parallel path records elapsed time, not CPU time).
    pub drain_ns: u128,
    /// Pass 2: the serial market-ascending overlay writes (order rows, level
    /// rows, stop diff, meta).
    pub write_ns: u128,
}

/// r6 engine-untimed-attribution: per-block nanosecond accumulators for the
/// exec sub-phases that `exec_phase_margin/match/settle` never covered.
///
/// The four *top-level* spans (`phase1_actions`, `margin`, `match`, `settle`)
/// are DISJOINT and together cover everything `execute_batch` does except its
/// own flat-action build and bookkeeping; [`Self::timed_total_ns`] sums them.
/// `settle_pass_a`, `settle_pass_b` and `cache_flush` are NESTED inside
/// `settle_ns` and must not be added to the total again.
#[derive(Default, Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExecPhaseAccum {
    /// Phase 1: the non-PlaceOrder action loop (cancels, modifies, transfers,
    /// governance…) executed inline before the order pipeline.
    pub phase1_actions_ns: u128,
    /// Phase 2: margin reservation + market partition (mirrors
    /// `exec_phase_margin_seconds`).
    pub margin_ns: u128,
    /// Phase 3: parallel per-market matching (mirrors
    /// `exec_phase_match_seconds`).
    pub match_ns: u128,
    /// Phase 4: settlement AND the end-of-call cache flush (mirrors
    /// `exec_phase_settle_seconds`).
    pub settle_ns: u128,
    /// Nested in `settle_ns`: parallel settle pass A (scoped-thread per-market
    /// plan compute). Stays 0 on the canonical sequential path.
    pub settle_pass_a_ns: u128,
    /// Nested in `settle_ns`: settle pass B — the single-threaded
    /// deterministic apply, or the whole sequential loop.
    pub settle_pass_b_ns: u128,
    /// Nested in `settle_ns`: `pos_cache.flush_all` + `bal_cache.flush_all`.
    pub cache_flush_ns: u128,
    /// Parallel settles that fell back to the sequential loop (worker panic
    /// or a position-side fill failure).
    pub settle_fallbacks: u64,
    /// B-blind (s92): non-pool sells topped up (in full or partly) after
    /// Phase 2 ([`NativeExecutor::sell_top_ups`]). A count, not a span.
    pub sell_top_ups: u64,
    /// Item 6 Phase 2 step 0.2 (P2-1): cancel-alls executed (user
    /// `CancelAllOrders` and the liquidation step's cancels), the books they
    /// visited, and the books where the sender had orders or stops.
    pub cancel_alls: u64,
    pub cancel_all_books_visited: u64,
    pub cancel_all_books_hit: u64,
    /// Step 0.2 (P2-1b): `CancelOrder` / `ModifyOrder` executed, and the
    /// books they probed to find the order.
    pub by_id_actions: u64,
    pub by_id_books_probed: u64,
}

impl ExecPhaseAccum {
    /// Sum of the DISJOINT top-level spans — what the caller subtracts from
    /// the engine wall clock (together with the post-engine tail) to get the
    /// untimed residual.
    pub fn timed_total_ns(&self) -> u128 {
        self.phase1_actions_ns + self.margin_ns + self.match_ns + self.settle_ns
    }

    /// Seconds view of a nanosecond accumulator field, for `Histogram::observe`.
    pub fn secs(ns: u128) -> f64 {
        ns as f64 / 1e9
    }
}

/// s74 boot profile: phase timings of the mode-2/3 book load
/// (`load_order_books_levels`), the one-time cost a restarted node pays on
/// its first replayed block (~4.5 s at ~1M resting orders). Node-local
/// instrumentation; logged once per load and kept on the ctx.
#[derive(Default, Clone, Copy, Debug)]
pub struct LoadTimings {
    /// Root-CF scan (meta/stop/level rows) incl. parse.
    pub root_scan_ns: u128,
    /// Node-local order-row store scan incl. parse.
    pub store_scan_ns: u128,
    /// `rebuild_book` summed across markets (sort + insert; CPU time).
    pub rebuild_ns: u128,
    /// Boot verify summed across markets: meta, stop and level rows (incl.
    /// level_hash) recomputed (CPU time).
    pub verify_ns: u128,
    /// Wall time of the per-market rebuild + verify phase (parallel).
    pub books_wall_ns: u128,
    pub markets: u32,
    pub orders: u64,
    pub levels: u64,
    /// Item 6 E4: `load_margin_configs` (every context, resident or not).
    pub margin_configs_ns: u128,
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
                let applied_marker = Self::read_applied_marker(&state);
                let marker_ok = match applied_marker {
                    Some(applied) => applied == inner.height,
                    // No marker row (test envs / pre-marker DBs): the height
                    // sequence check below is the only guard available.
                    None => true,
                };
                let height_ok = inner.height + 1 == block_height;
                if height_ok && marker_ok {
                    reused_inner = Some(inner);
                } else {
                    // Attribute the trip: "height" = the holder is not the
                    // block's predecessor (a block advanced without the holder
                    // — see `ResidentBooks::advance_untouched`), "marker" =
                    // the DB's applied height disagrees with memory (failed
                    // flush / out-of-band progress), "height+marker" = both.
                    let reason = match (height_ok, marker_ok) {
                        (false, false) => "height+marker",
                        (false, true) => "height",
                        _ => "marker",
                    };
                    tracing::warn!(
                        resident_height = inner.height,
                        block_height,
                        applied_marker,
                        reason,
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
        let mut load_timings = LoadTimings::default();
        // P2-1: the resident books bring their index; a load starts without
        // one (the first cancel-all builds it from the books).
        let mut trader_markets = None;
        let (mut order_books, scanned_next_id, mut load_error) = match reused_inner {
            Some(inner) => {
                trader_markets = inner.trader_markets;
                (inner.books, inner.next_global_order_id, None)
            }
            None => match book_mode {
                BookMode::Classic => Self::load_order_books(&state),
                BookMode::OrderRows => Self::load_order_books_rows(&state),
                BookMode::LevelAuthority | BookMode::LevelAuthorityChunked => {
                    Self::load_order_books_levels(
                        &state,
                        book_mode.level_hash_chunked(),
                        &mut load_timings,
                        load_books_workers_env(),
                    )
                }
            },
        };
        if trader_markets.is_none() {
            // P2-1: what the load inserted is in the books; the index build
            // scans them.
            for book in order_books.values_mut() {
                book.drain_new_traders();
            }
        }

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

        // Item 3 (F2, F8, D11): margin configs from the market listings. A read
        // error is a node fault (fatal, like the book load).
        let margin_configs_timer = std::time::Instant::now();
        let margin_configs = match Self::load_margin_configs(&state) {
            Ok(m) => m,
            Err(e) => {
                if load_error.is_none() {
                    load_error = Some(format!("margin configs: {e}"));
                }
                HashMap::default()
            }
        };
        load_timings.margin_configs_ns = margin_configs_timer.elapsed().as_nanos();

        Self {
            positions,
            oracle,
            staking,
            governance,
            state,
            order_books,
            dirty_books: HashSet::default(),
            margin_configs,
            block_marks: None,
            prev_marks: None,
            sums: None,
            trader_markets,
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
            trade_history: true,
            record_fills: false,
            pending_fills: Vec::new(),
            pending_extras: Vec::new(),
            fills_block: 0,
            inline_fills_written: 0,
            fill_effects_scratch: Vec::new(),
            metrics: None,
            liq_value_sum: false,
            fatal_error: load_error,
            reader_fault: std::sync::OnceLock::new(),
            book_mode,
            book_mode_marker_present,
            resident: resident_mode,
            resident_reused,
            level_hash_cache_bytes: level_hash_cache_env_bytes(),
            save_books_workers: save_books_workers_env(),
            save_books_min_ops: save_books_min_ops_env(),
            last_save_workers: 1,
            #[cfg(feature = "save-timings")]
            collect_save_timings: false,
            #[cfg(feature = "save-timings")]
            last_save_timings: SaveTimings::default(),
            load_timings,
            phase_accum: ExecPhaseAccum::default(),
            save_split: SaveSplitAccum::default(),
            #[cfg(any(test, feature = "test-reference-paths"))]
            test_cancel_all_full_scan: false,
            #[cfg(any(test, feature = "test-reference-paths"))]
            test_flush_per_row: false,
        }
    }

    /// Item 6 C2: take the resident rows slot's mark state (the previous
    /// block's table and configs) from `block` — call before
    /// [`NativeExecutor::begin_block_oracle`]. Without it (reference path,
    /// tests) every block's table takes a new version.
    pub fn attach_resident_block(&mut self, block: &mut ResidentBlock) {
        self.prev_marks = block.marks.take();
        // C3: the slot's sums cache (used once the table is filled).
        self.sums = block.sums.take();
    }

    /// Item 6 C2: hand this block's mark table and configs to `block` for
    /// `end_resident` to stash in the slot — call after the block's last
    /// action, before the context is dropped. Later mark reads of this
    /// context go to the oracle.
    pub fn detach_resident_block(&mut self, block: &mut ResidentBlock) {
        block.marks = self
            .block_marks
            .take()
            .map(|marks| BlockMarksState { marks, configs: self.margin_configs.clone() });
        // C3: the sums cache and this block's memo, merged by `end_resident`.
        if let Some(sums) = self.sums.take() {
            block.sums = Some(sums);
        }
    }

    /// Item 6 C2: the version of the block's mark table (`None`: no table).
    pub fn mark_version(&self) -> Option<u64> {
        self.block_marks.as_ref().map(BlockMarks::version)
    }

    /// Worker threads the last `save_order_books` drained dirty books on
    /// (1 = serial path). Test/ops introspection.
    pub fn last_save_workers(&self) -> usize {
        self.last_save_workers
    }

    /// rank8: whether this context reused the holder's resident state (vs
    /// rebuilding from persisted). Test/ops introspection.
    pub fn resident_reused(&self) -> bool {
        self.resident_reused
    }

    /// rank8: whether this context runs in resident-book mode at all (was
    /// constructed with a holder). The deferred book save is gated on this:
    /// without resident books every block reloads its books THROUGH the
    /// overlay's parent layer, which would not hold the worker-applied bytes.
    pub fn resident_mode(&self) -> bool {
        self.resident
    }

    /// rank8: whether this context ran in resident mode but had to REBUILD
    /// from persisted state (holder empty at startup, drained after a fatal
    /// block, or the staleness guard tripped). Metrics hook: every hit is a
    /// full O(resting depth) reload on the exec thread.
    pub fn resident_rebuilt(&self) -> bool {
        self.resident && !self.resident_reused
    }

    /// The block's fail-stop reason, taken: `fatal_error`, else the first
    /// fault a reader recorded (R02, [`Self::reader_fault`]). The committer
    /// checks this (never `fatal_error` alone) before it flushes anything.
    pub fn take_fatal_error(&mut self) -> Option<String> {
        self.fatal_error.take().or_else(|| self.reader_fault.take())
    }

    /// rank8: hand the books (their journals ride inside, journal-in-book)
    /// and the order-id high-water mark back to the cross-block holder. Call
    /// AFTER `save_order_books`. No-op for non-resident contexts. A context
    /// that latched `fatal_error` (or a reader fault) INVALIDATES the holder instead — its
    /// in-memory state may not match what was (not) persisted, so the next
    /// block must rebuild from the DB.
    pub fn stash_resident(&mut self, resident: &mut ResidentBooks) {
        if !self.resident {
            return;
        }
        if self.fatal_error.is_some() || self.reader_fault.get().is_some() {
            resident.invalidate();
            return;
        }
        self.debug_assert_index_fed();
        resident.inner = Some(ResidentInner {
            books: std::mem::take(&mut self.order_books),
            trader_markets: self.trader_markets.take(),
            next_global_order_id: self.next_global_order_id,
            height: self.block_height,
        });
    }

    /// rank8 staleness guard input: the DB's native applied-height marker.
    fn read_applied_marker(state: &T) -> Option<u64> {
        applied_marker(state)
    }

    /// Item 6 Phase 2 P2-1: the cancel-all index, built from the books on
    /// first use after a load.
    fn cancel_index(&mut self) -> &mut TraderMarkets {
        let books = &mut self.order_books;
        self.trader_markets
            .get_or_insert_with(|| TraderMarkets::build(books))
    }

    /// P2-1 (debug builds): with an index, every book's log was fed into it
    /// at its site ([`TraderMarkets::absorb`]); a log left over means a site
    /// that rests orders or stores stops does not feed the index.
    fn debug_assert_index_fed(&self) {
        debug_assert!(
            self.trader_markets.is_none()
                || self
                    .order_books
                    .values()
                    .all(|b| b.new_traders_logged() == 0),
            "P2-1: a book's new-trader log was not fed into the cancel-all index"
        );
    }

    /// Item 6 Phase 2 P2-1 (s104, node-local gauges): the cancel-all index's
    /// `((trader, market) entries, traders)`; `(0, 0)` while not built (until
    /// the first cancel-all after a load, and always without resident books).
    /// Counts every entry, stale ones too. O(1): the index keeps its entry
    /// count.
    pub fn cancel_index_size(&self) -> (u64, u64) {
        self.trader_markets
            .as_ref()
            .map_or((0, 0), TraderMarkets::size)
    }

    /// P2-1 (tests): `(carried, rebuilt)` as [`ResidentBooks::trader_index`].
    #[cfg(test)]
    pub(crate) fn trader_index(&self) -> (Option<TraderIndexSnapshot>, TraderIndexSnapshot) {
        trader_index_snapshot(self.trader_markets.as_ref(), &self.order_books)
    }

    /// Drain the block's fills (`trade_index` order). Under `defer_trades` the
    /// caller owns writing their rows from here (`trade_rows::encode_block`
    /// on a background writer, or a synchronous fallback).
    /// Drops their extras (see `take_pending_fills_and_extras`).
    pub fn take_pending_trade_fills(&mut self) -> Vec<TradeFill> {
        self.take_pending_fills_and_extras().0
    }

    /// Drain the block's fills and their stream-only extras (s80). The extras
    /// are empty unless `record_fills` was set, else index-aligned with the
    /// fills.
    pub fn take_pending_fills_and_extras(&mut self) -> (Vec<TradeFill>, Vec<FillExtras>) {
        self.inline_fills_written = 0;
        (
            std::mem::take(&mut self.pending_fills),
            std::mem::take(&mut self.pending_extras),
        )
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

        let mut books = HashMap::default();
        let mut max_order_id: u128 = 0;

        if let Ok(entries) = state.iterate_cf(CF_NATIVE_ORDER_BOOKS, None) {
            for (key, value) in entries {
                if Self::is_book_row_key(&key) {
                    return (
                        HashMap::default(),
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

        let fail = |msg: String| (HashMap::default(), 1, Some(msg));

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
        let mut accs: HashMap<MarketId, Acc> = HashMap::default();

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

        let mut books = HashMap::default();
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
        chunked: bool,
        timings: &mut LoadTimings,
        workers: usize,
    ) -> (HashMap<MarketId, OrderBook>, u128, Option<String>) {
        use torus_state::cf::{CF_BOOK_ORDER_ROWS, CF_NATIVE_ORDER_BOOKS};

        let fail = |msg: String| (HashMap::default(), 1, Some(msg));

        let load_start = std::time::Instant::now();
        let t0 = load_start;
        // ---- Root CF scan: meta + stops + level rows (consensus) ----
        #[derive(Default)]
        struct RootAcc {
            meta: Option<(BookMeta, Vec<u8>)>,
            stops: Vec<(u128, Vec<u8>)>,
            /// level key (26 B) -> stored 52 B value
            levels: std::collections::BTreeMap<Vec<u8>, Vec<u8>>,
        }
        let mut roots: HashMap<MarketId, RootAcc> = HashMap::default();

        let entries = match state.iterate_cf(CF_NATIVE_ORDER_BOOKS, None) {
            Ok(e) => e,
            Err(e) => return fail(format!("3c: book row scan failed: {e}")),
        };
        for (key, value) in entries {
            if key.len() == 8 {
                return fail(
                    "3c: TORUS_BOOK_ROWS=2/3 but cf_native_order_books holds classic \
                     whole-book blobs — the flag must match the DB's history \
                     (fleet-uniform; enabling it needs a fresh genesis)"
                        .to_string(),
                );
            }
            if key.len() == 25 && key[8] == ROW_TAG_ORDER {
                return fail(
                    "3c: TORUS_BOOK_ROWS=2/3 but cf_native_order_books holds per-order \
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

        timings.root_scan_ns = t0.elapsed().as_nanos();
        let t0 = std::time::Instant::now();
        // ---- Node-local order-row store scan ----
        let mut store_orders: HashMap<MarketId, Vec<(u64, torus_core::order_book::Order)>> =
            HashMap::default();
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

        timings.store_scan_ns = t0.elapsed().as_nanos();
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

        // ---- Rebuild + boot verification (per market, on worker threads) ----
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
        type Orders = Vec<(u64, torus_core::order_book::Order)>;
        let work: Vec<(MarketId, RootAcc, Orders)> = market_ids
            .into_iter()
            .map(|id| {
                let acc = roots.remove(&id).unwrap();
                (id, acc, store_orders.remove(&id).unwrap_or_default())
            })
            .collect();
        for (_, acc, orders) in &work {
            timings.markets += 1;
            timings.orders += orders.len() as u64;
            timings.levels += acc.levels.len() as u64;
        }

        // One market: rebuild + boot verify, a pure function of that market's
        // rows. s74: markets run on scoped worker threads (the ~1 s rebuild and
        // ~1 s verify at ~1M resting orders were serial); results are consumed
        // in market-ascending order below, so the first error reported — and
        // every book — is exactly the serial loop's.
        type Loaded = Result<(OrderBook, u128, u128), String>;
        let load_market = move |market_id: MarketId, acc: RootAcc, orders: Orders| -> Loaded {
            let Some((meta, stored_meta_bytes)) = acc.meta else {
                return Err(format!(
                    "3c: market {market_id} has stop/level rows but no meta row \
                     (corrupt row store)"
                ));
            };
            let stop_bytes = acc.stops.clone();
            let t0 = std::time::Instant::now();
            let mut book = Self::rebuild_book(market_id, meta, orders, acc.stops)
                .map_err(|e| format!("3c: {e}"))?;
            let rebuild_ns = t0.elapsed().as_nanos();
            let t0 = std::time::Instant::now();
            // Mode 3: select the chunked digest BEFORE any drain so every
            // later mutation marks its chunk (boot verify below recomputes
            // from scratch either way).
            book.set_level_hash_chunked(chunked);

            // -- Boot verify 1: meta row bytes --
            let recomputed_meta = book_meta_value(&book);
            if recomputed_meta != stored_meta_bytes {
                return Err(format!(
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
                return Err(format!(
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
                return Err(format!(
                    "3c: market {market_id}: recomputed level rows != root-committed \
                     level rows (node-local order store corrupt/stale, or this node's \
                     TORUS_BOOK_ROWS={} does not match the mode the DB was written \
                     under — the level_hash preimage differs between modes 2 and 3; \
                     refusing to serve or sign)",
                    if chunked { 3 } else { 2 }
                ));
            }
            Ok((book, rebuild_ns, t0.elapsed().as_nanos()))
        };

        let workers = workers.clamp(1, work.len().max(1));
        let t_books = std::time::Instant::now();
        let results: Vec<(MarketId, Loaded)> = if workers == 1 {
            work.into_iter()
                .map(|(id, acc, orders)| (id, load_market(id, acc, orders)))
                .collect()
        } else {
            // LPT by rows to rebuild/verify — the save-drain idiom.
            let weights: Vec<(MarketId, usize)> = work
                .iter()
                .map(|(id, acc, orders)| (*id, orders.len() + acc.levels.len()))
                .collect();
            let assignment = MarketWorkerPool::assign_chunks(&weights, workers);
            let mut chunks: Vec<Vec<(MarketId, RootAcc, Orders)>> =
                (0..workers).map(|_| Vec::new()).collect();
            for (item, w) in work.into_iter().zip(assignment) {
                chunks[w].push(item);
            }
            let load_market = &load_market;
            let mut out: Vec<(MarketId, Loaded)> = Vec::new();
            std::thread::scope(|s| {
                let handles: Vec<_> = chunks
                    .into_iter()
                    .filter(|c| !c.is_empty())
                    .map(|chunk| {
                        s.spawn(move || {
                            chunk
                                .into_iter()
                                .map(|(id, acc, orders)| (id, load_market(id, acc, orders)))
                                .collect::<Vec<_>>()
                        })
                    })
                    .collect();
                torus_state::spawn_count::add(
                    torus_state::spawn_count::SpawnSite::LoadBooks,
                    handles.len(),
                );
                // Join every worker; re-raise a panic like the serial loop.
                let mut panic_payload: Option<Box<dyn std::any::Any + Send>> = None;
                for h in handles {
                    match h.join() {
                        Ok(loaded) => out.extend(loaded),
                        Err(payload) => {
                            if panic_payload.is_none() {
                                panic_payload = Some(payload);
                            }
                        }
                    }
                }
                if let Some(payload) = panic_payload {
                    std::panic::resume_unwind(payload);
                }
            });
            out.sort_by_key(|(id, _)| *id);
            out
        };
        timings.books_wall_ns = t_books.elapsed().as_nanos();

        let mut books = HashMap::default();
        let mut max_order_id: u128 = 0;
        for (market_id, loaded) in results {
            let (book, rebuild_ns, verify_ns) = match loaded {
                Ok(loaded) => loaded,
                Err(e) => return fail(e),
            };
            timings.rebuild_ns += rebuild_ns;
            timings.verify_ns += verify_ns;
            let book_next_id = book.next_order_id();
            if book_next_id > max_order_id {
                max_order_id = book_next_id;
            }
            books.insert(market_id, book);
        }

        let ms = |ns: u128| ns / 1_000_000;
        tracing::info!(
            markets = timings.markets,
            orders = timings.orders,
            levels = timings.levels,
            root_scan_ms = %ms(timings.root_scan_ns),
            store_scan_ms = %ms(timings.store_scan_ns),
            workers,
            books_wall_ms = %ms(timings.books_wall_ns),
            rebuild_cpu_ms = %ms(timings.rebuild_ns),
            verify_cpu_ms = %ms(timings.verify_ns),
            total_ms = %load_start.elapsed().as_millis(),
            "load_books: order books loaded from DB (level authority)"
        );
        let next_id = if max_order_id > 0 { max_order_id } else { 1 };
        (books, next_id, None)
    }

    /// Read-only recompute of EVERY non-empty level's row data for a book
    /// (boot verify / staleness-guard verify — does NOT touch the journals).
    /// Uses the digest the book is configured for (flat mode 2 / chunked
    /// mode 3), computed FROM SCRATCH — independent of any incremental state.
    fn live_level_rows(book: &OrderBook) -> Vec<((u8, i128), LevelRowData)> {
        use torus_core::book_rows::{SIDE_TAG_ASK, SIDE_TAG_BID};
        let chunked = book.level_hash_chunked();
        let mut out = Vec::new();
        for (tag, prices) in [
            (SIDE_TAG_BID, book.bid_queues().map(|(p, _)| p.raw()).collect::<Vec<_>>()),
            (SIDE_TAG_ASK, book.ask_queues().map(|(p, _)| p.raw()).collect::<Vec<_>>()),
        ] {
            for raw in prices {
                let data = if chunked {
                    book.level_row_data_chunked(tag, raw)
                } else {
                    book.level_row_data(tag, raw)
                };
                if let Some(data) = data {
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

    /// Item 3 (F2, F8, D11): one [`market_margin_config`] per listed market
    /// (8-byte keys of `CF_NATIVE_MARKETS`; metadata rows skipped). Undecodable
    /// / non-positive rows get none (default 20x). One scan of <= M rows.
    fn load_margin_configs(
        state: &T,
    ) -> Result<HashMap<MarketId, MarketMarginConfig>, torus_state::error::StateError> {
        use torus_state::cf::CF_NATIVE_MARKETS;
        Ok(state
            .iterate_cf(CF_NATIVE_MARKETS, None)?
            .into_iter()
            .filter(|(k, _)| k.len() == 8)
            .filter_map(|(k, v)| {
                let m = u64::from_be_bytes(k[..8].try_into().ok()?);
                market_margin_config(m, &v).map(|c| (m, c))
            })
            .collect())
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
            CF_NATIVE_MARKETS, CF_NATIVE_ORDER_BOOKS,
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
            // (Mode 3 never uses the sponge cache: its digest is
            // depth-independent by construction, so the arm below disables it.)
            (self.level_hash_cache_bytes / self.order_books.len().max(1)).max(1)
        } else {
            0
        };
        // Mode 2: two-pass save (parallel journal drain across dirty books,
        // then serial market-ascending writes) — see `save_level_authority`.
        // Modes 0/1 keep the per-market loop below.
        let dirty = if self.book_mode.is_level_authority() {
            written += self.save_level_authority(&dirty, level_cache_per_book, timed, &mut acc);
            Vec::new()
        } else {
            self.last_save_workers = 1;
            dirty
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
                                self.state.put_cf_raw_owned(CF_NATIVE_ORDER_BOOKS, &key, data)
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
                        let res = match op {
                            Some(bytes) => {
                                self.state.put_cf_raw_owned(CF_NATIVE_ORDER_BOOKS, &key, bytes)
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
                    written += Self::diff_stop_rows(&self.state, market_id, book.stop_rows());
                    written +=
                        Self::write_meta_if_moved(&self.state, market_id, &book_meta_value(book));
                }
                // Modes 2/3 are saved by the two-pass path above (`dirty` is
                // empty here under either level-authority variant).
                BookMode::LevelAuthority | BookMode::LevelAuthorityChunked => {}
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

    /// Mode 2 (LevelAuthority) save, two passes:
    ///
    ///   pass 1 — DRAIN every dirty book's journals (`take_row_ops` +
    ///            `take_level_ops`, incl. its private level-hash cache) —
    ///            on `save_books_workers` scoped threads when >= 2 dirty
    ///            books, >= 2 workers and the journaled work reaches
    ///            `save_books_min_ops`; otherwise inline, market-ascending;
    ///   pass 2 — WRITE the drained ops + stop diff + meta SERIALLY in
    ///            market-ascending order (`write_drained_book`).
    ///
    /// The drain is a pure function of each book, so pass 1's thread layout
    /// cannot change any byte; pass 2 is the exact serial write sequence.
    /// Returns the number of root-CF writes (test hook, as before). Records
    /// the worker count actually used in `last_save_workers`.
    fn save_level_authority(
        &mut self,
        dirty: &[MarketId],
        level_cache_per_book: usize,
        timed: bool,
        acc: &mut SaveTimings,
    ) -> usize {
        let drained = self.drain_level_books(dirty, level_cache_per_book, timed, acc);

        // Pass 2: serial writes, market ascending (`drained` is sorted).
        // bl1 exec-chain ruler: pass 2 (WRITE) wall clock — the only half of
        // save_books a flush worker could take (and the half
        // `save_order_books_deferred` hands to it under the exec pipeline).
        let write_timer = std::time::Instant::now();
        let mut written = 0usize;
        for d in drained {
            let Some(book) = self.order_books.get(&d.market_id) else {
                // Cannot happen: every drained book was borrowed from the
                // map in pass 1 and nothing removed it since.
                continue;
            };
            let (stops, meta) = (book.stop_rows(), book_meta_value(book));
            written += Self::write_drained_book(
                &self.state,
                self.metrics.as_deref(),
                d,
                stops,
                &meta,
                timed,
                acc,
            );
        }
        self.save_split.write_ns += write_timer.elapsed().as_nanos();
        written
    }

    /// Mode-2/3 pass 1 for every dirty book: gather `&mut` handles (disjoint
    /// map entries — one `&mut` each) sorted by market id, then DRAIN the
    /// journals inline or on the scoped drain workers. Shared verbatim by the
    /// serial save (`save_level_authority`) and the pipelined
    /// `save_order_books_deferred` — the drain reads the LIVE books, so it can
    /// never leave the exec thread in either mode.
    fn drain_level_books(
        &mut self,
        dirty: &[MarketId],
        level_cache_per_book: usize,
        timed: bool,
        acc: &mut SaveTimings,
    ) -> Vec<DrainedBook> {
        let dirty_set = &self.dirty_books;
        let mut books: Vec<(MarketId, &mut OrderBook)> = self
            .order_books
            .iter_mut()
            .filter(|(id, _)| dirty_set.contains(id))
            .map(|(id, b)| (*id, b))
            .collect();
        books.sort_unstable_by_key(|(id, _)| *id);
        debug_assert!(books.len() <= dirty.len());
        let total_ops: usize = books
            .iter()
            .map(|(_, b)| b.journaled_rows() + b.journaled_levels())
            .sum();
        let workers = self.save_books_workers.min(books.len()).max(1);
        let parallel = workers >= 2 && total_ops >= self.save_books_min_ops;
        let chunked = self.book_mode.level_hash_chunked();
        // bl1 exec-chain ruler: pass 1 (DRAIN) wall clock. Production timer —
        // one `Instant::now()` pair per block, not per book.
        let drain_timer = std::time::Instant::now();
        let drained: Vec<DrainedBook> = if parallel {
            let (drained, rows_ns, levels_ns) =
                drain_books_parallel(books, workers, level_cache_per_book, chunked, timed);
            // Wall-clock attribution: the slowest worker's drain time.
            acc.rows_ns += rows_ns;
            acc.levels_ns += levels_ns;
            drained
        } else {
            let mut out = Vec::with_capacity(books.len());
            for (id, book) in books {
                let d = drain_book(id, book, level_cache_per_book, chunked, timed);
                acc.rows_ns += d.rows_ns;
                acc.levels_ns += d.levels_ns;
                out.push(d);
            }
            out
        };
        self.save_split.drain_ns += drain_timer.elapsed().as_nanos();
        self.last_save_workers = if parallel { workers } else { 1 };
        drained
    }

    /// Deferred book save (s63 port of item 6a, origin 4298728; exec-pipeline
    /// fast path only): run pass 1 of the mode-2/3 save on THIS thread (journal
    /// drain — reads the live books, can never leave the exec thread) and
    /// RETURN pass 2 (the state writes) for the flush worker instead of
    /// applying it here. Also writes the order-id counter and the
    /// `__book_mode__` marker into `self.state` exactly as `save_order_books`
    /// does (cheap point writes into this block's overlay, so the next block
    /// reads them through the parent layer as today). Returns `None` — with NO
    /// side effects — for Classic / OrderRows, which have no two-pass save; the
    /// caller then runs `save_order_books`.
    ///
    /// Why the snapshots: pass 2's only live-book reads are `stop_rows()` and
    /// the meta bytes; they are captured HERE (block N's post-state) because by
    /// the time the flush worker runs, block N+1's engine is already mutating
    /// the books. The stop-row diff and the meta compare themselves move to the
    /// worker, whose read view (heights < N durable) matches what this thread
    /// would have read through the overlay (E writes no book-CF key before the
    /// save, so the overlay contributes nothing to those reads).
    pub fn save_order_books_deferred(&mut self) -> Option<DeferredBookSave> {
        use torus_state::cf::CF_NATIVE_MARKETS;
        if !self.book_mode.is_level_authority() {
            return None;
        }
        // Same per-book sponge-cache budget as `save_order_books` (mode 3
        // never uses the sponge cache — depth-independent digest).
        let level_cache_per_book = if matches!(self.book_mode, BookMode::LevelAuthority)
            && self.level_hash_cache_bytes > 0
        {
            (self.level_hash_cache_bytes / self.order_books.len().max(1)).max(1)
        } else {
            0
        };
        let mut dirty: Vec<MarketId> = self.dirty_books.iter().copied().collect();
        dirty.sort_unstable();
        // µbench save-timings are not collected on the deferred path (pass 2
        // runs on the worker); the production drain timer still updates
        // `save_split.drain_ns` inside `drain_level_books`.
        let mut acc = SaveTimings::default();
        let drained = self.drain_level_books(&dirty, level_cache_per_book, false, &mut acc);
        let books: Vec<DeferredBook> = drained
            .into_iter()
            .filter_map(|d| {
                // Mirrors the pass-2 lookup; a drained book always exists.
                let book = self.order_books.get(&d.market_id)?;
                Some(DeferredBook {
                    stops: book.stop_rows(),
                    meta: book_meta_value(book),
                    drained: d,
                })
            })
            .collect();

        // Counter + mode marker: same condition, same bytes, same placement
        // (this overlay) as the `save_order_books` tail.
        if self.loaded_next_global_order_id != Some(self.next_global_order_id) {
            if let Err(e) = self.state.put_cf_raw(
                CF_NATIVE_MARKETS,
                Self::NEXT_GLOBAL_ORDER_ID_KEY,
                &self.next_global_order_id.to_be_bytes(),
            ) {
                tracing::error!(%e, "failed to persist next_global_order_id");
            }
        }
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
        Some(DeferredBookSave { books })
    }

    /// Mode-2 pass 2 for one book: node-local order rows, root-CF level rows,
    /// stop diff, meta-if-moved, funnel metrics — the exact per-market write
    /// sequence of the former single-pass loop. Returns root-CF writes.
    /// `stops` / `meta` are the book's `stop_rows()` / `book_meta_value`
    /// captured at save time — passed in (not read from the book) so the
    /// deferred save's flush worker can run this against its snapshots.
    fn write_drained_book(
        state: &T,
        metrics: Option<&torus_telemetry::Metrics>,
        drained: DrainedBook,
        stops: Vec<(OrderId, Vec<u8>)>,
        meta: &[u8],
        timed: bool,
        acc: &mut SaveTimings,
    ) -> usize {
        use torus_state::cf::{CF_BOOK_ORDER_ROWS, CF_NATIVE_ORDER_BOOKS};
        let market_id = drained.market_id;
        let mut written = 0usize;
        let mut rows_written = 0usize;
        let mut rows_deleted = 0usize;
        let t_rows = timed.then(std::time::Instant::now);
        for (order_id, op) in drained.row_ops {
            let key = book_order_key(market_id, order_id);
            let res = match op {
                Some(bytes) => {
                    rows_written += 1;
                    state.put_cf_raw_owned(CF_BOOK_ORDER_ROWS, &key, bytes)
                }
                None => {
                    rows_deleted += 1;
                    state.delete_cf_raw(CF_BOOK_ORDER_ROWS, &key)
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
        for ((tag, raw_price), op) in drained.level_ops {
            let key = level_row_key_tagged(market_id, tag, raw_price);
            let res = match &op {
                Some(data) => {
                    levels_written += 1;
                    state.put_cf_raw(CF_NATIVE_ORDER_BOOKS, &key, &data.encode())
                }
                None => {
                    levels_deleted += 1;
                    state.delete_cf_raw(CF_NATIVE_ORDER_BOOKS, &key)
                }
            };
            match res {
                Ok(()) => written += 1,
                Err(e) => tracing::error!(market_id, %e, "3c: level row write failed"),
            }
        }
        if let Some(t) = t_levels {
            acc.levels_ns += t.elapsed().as_nanos();
        }
        let t_stops = timed.then(std::time::Instant::now);
        written += Self::diff_stop_rows(state, market_id, stops);
        if let Some(t) = t_stops {
            acc.stops_ns += t.elapsed().as_nanos();
        }
        let t_meta = timed.then(std::time::Instant::now);
        written += Self::write_meta_if_moved(state, market_id, meta);
        if let Some(t) = t_meta {
            acc.meta_ns += t.elapsed().as_nanos();
        }
        if let Some(m) = metrics {
            m.exec_book_rows_written.inc_by(rows_written as u64);
            m.exec_book_rows_deleted.inc_by(rows_deleted as u64);
            m.exec_book_levels_written.inc_by(levels_written as u64);
            m.exec_book_levels_deleted.inc_by(levels_deleted as u64);
        }
        written
    }

    /// Shared modes 1/2: pending stops are immutable per id → put new ids,
    /// delete gone ids. Stateless diff against the persisted stop rows (one
    /// bounded prefix scan over `market ‖ 0x02` — the stop set is tiny).
    /// `stops` = the book's `stop_rows()` (captured at save time on the
    /// deferred path).
    fn diff_stop_rows(state: &T, market_id: MarketId, stops: Vec<(OrderId, Vec<u8>)>) -> usize {
        use torus_state::cf::CF_NATIVE_ORDER_BOOKS;
        let mut prefix = [0u8; 9];
        prefix[..8].copy_from_slice(&market_id.to_be_bytes());
        prefix[8] = ROW_TAG_STOP;
        let persisted: HashSet<u128> = state
            .iterate_cf(CF_NATIVE_ORDER_BOOKS, Some(&prefix))
            .map(|rows| {
                rows.iter()
                    .filter(|(k, _)| k.len() == 25 && k[8] == ROW_TAG_STOP)
                    .map(|(k, _)| u128::from_be_bytes(k[9..25].try_into().unwrap()))
                    .collect()
            })
            .unwrap_or_default();

        let mut written = 0usize;
        let mut live = HashSet::with_capacity_and_hasher(stops.len(), Default::default());
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
    /// `meta` = `book_meta_value(book)` (captured at save time on the
    /// deferred path).
    fn write_meta_if_moved(state: &T, market_id: MarketId, meta: &[u8]) -> usize {
        use torus_state::cf::CF_NATIVE_ORDER_BOOKS;
        let key = book_meta_key(market_id);
        match state.get_cf_raw(CF_NATIVE_ORDER_BOOKS, &key) {
            Ok(Some(existing)) if existing == meta => return 0,
            Ok(_) => {}
            Err(e) => {
                tracing::error!(market_id, %e, "meta row read failed");
            }
        }
        if let Err(e) = state.put_cf_raw(CF_NATIVE_ORDER_BOOKS, &key, meta) {
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

        // Mode 3: full writes emit the chunked digest (and re-seed the
        // book's incremental chunk state).
        book.set_level_hash_chunked(mode.level_hash_chunked());
        let row_ops = book.full_row_ops();
        let level_ops = book.full_level_ops();
        let meta = book_meta_value(book);
        let stops = book.stop_rows();

        // Fresh key set per CF.
        let mut keep_root: HashSet<Vec<u8>> =
            HashSet::default();
        let mut keep_store: HashSet<Vec<u8>> =
            HashSet::default();
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
            BookMode::LevelAuthority | BookMode::LevelAuthorityChunked => {
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
        if mode.is_level_authority() {
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
            BookMode::LevelAuthority | BookMode::LevelAuthorityChunked => {
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

/// Error for the direct admin market actions (`ListMarket`, `DelistMarket`,
/// `UpdateMarketParams`): markets change only through governance.
const GOVERNANCE_ONLY_MSG: &str = "governance-only: submit a proposal";

impl NativeExecutor {
    /// Execute a single native action for the given sender.
    pub fn execute<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
        sender: &Address,
        action: &NativeAction,
    ) -> NativeActionResult {
        let result = Self::execute_action(ctx, sender, action);
        Self::write_trades_inline(ctx);
        result
    }

    /// Without `defer_trades`: write the rows of ALL the block's fills so far,
    /// if any fill is new. A later call rewrites the same keys with a superset,
    /// so calls within a block never drop each other's fills.
    /// Nothing with trade history off (fills recorded only for `record_fills`).
    fn write_trades_inline<T: StateBackend>(ctx: &mut NativeExecContext<T>) {
        if !ctx.trade_history
            || ctx.defer_trades
            || ctx.pending_fills.len() == ctx.inline_fills_written
        {
            return;
        }
        let mut rows = PackedCfBatch::default();
        encode_block(ctx.fills_block, ctx.timestamp, &ctx.pending_fills, &mut rows);
        for (cf, key, value) in rows.iter() {
            let _ = ctx.state.put_cf_raw(cf, key, value);
        }
        ctx.inline_fills_written = ctx.pending_fills.len();
    }

    fn execute_action<T: StateBackend>(
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
                    return NativeActionResult::rejected(
                        "place_order_batch",
                        (
                            FailureReason::BatchCap,
                            format!(
                                "batch size {} outside [1, {}] — skipped (deterministic cap)",
                                orders.len(),
                                torus_types::NATIVE_ORDERS_PER_BATCH_CAP
                            ),
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
                    error: (ok != total).then(|| format!("{ok}/{total} orders placed").into()),
                    gas_used: batch.total_gas,
                    // The first failing order's reason.
                    reason: batch
                        .results
                        .iter()
                        .find(|r| !r.success)
                        .map_or(FailureReason::Other, |r| r.reason),
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
            } => Self::exec_modify_order(ctx, sender, *order_id, *new_price, *new_qty),

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
            NativeAction::ClaimUnbonded => Self::exec_claim_unbonded(ctx, sender),
            // FIX ECON-FIND-15: TopUpSelfStake via NativeAction.
            NativeAction::TopUpSelfStake { amount } => {
                match ctx.staking.top_up_self_stake(*sender, *amount) {
                    Ok(()) => NativeActionResult::ok("top_up_self_stake", 2000),
                    Err(e) => Self::econ_err(ctx, "top_up_self_stake", e),
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
            NativeAction::AttestStateHash { height, hash } => {
                Self::exec_attest_state_hash(ctx, sender, *height, hash)
            }
            NativeAction::UnjailSelf => Self::exec_unjail_self(ctx, sender),
            NativeAction::RotateValidatorKey { new_pubkey } => {
                Self::exec_rotate_key(ctx, sender, new_pubkey)
            }
            NativeAction::SetOracleSigner { signer, proof } => {
                Self::exec_set_oracle_signer(ctx, sender, *signer, proof.as_ref())
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

            // ---- Admin (governance-only) ----
            // AUDIT: ECON-PF-07 -- There is no authority check on the direct path, so
            // these are REJECTED (previously a silent ok no-op, which told clients a
            // listing had succeeded). Market changes go through
            // SubmitProposal(ProposalAction::*) and execute via governance.
            NativeAction::UpdateMarketParams { .. } => {
                NativeActionResult::err("update_market_params", GOVERNANCE_ONLY_MSG.into())
            }
            NativeAction::ListMarket(_) => {
                NativeActionResult::err("list_market", GOVERNANCE_ONLY_MSG.into())
            }
            NativeAction::DelistMarket { .. } => {
                NativeActionResult::err("delist_market", GOVERNANCE_ONLY_MSG.into())
            }
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
        Self::execute_batch_inner(
            ctx,
            actions,
            SettleMode::Auto,
            EngineMode::Auto,
            cancel_batch_enabled(),
        )
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
        Self::execute_batch_settle_workers(ctx, actions, parallel, None)
    }

    /// R6: `execute_batch_settle_mode` with the pass-A settle worker cap
    /// pinned too (`None` = production cap). Pass A packs markets into at most
    /// `workers` chunks; the layout is pure scheduling, so the differential
    /// tests pin every cap from 1 (serial pass A) upwards and demand
    /// byte-identical state. Env-free so it cannot race other test threads.
    pub fn execute_batch_settle_workers<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
        actions: &[(Address, NativeAction)],
        parallel: bool,
        workers: Option<usize>,
    ) -> NativeBatchResult {
        Self::execute_batch_inner(
            ctx,
            actions,
            SettleMode::Force { parallel, workers },
            EngineMode::Force(0),
            cancel_batch_enabled(),
        )
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
                SettleMode::Force {
                    parallel: true,
                    workers: None,
                },
                EngineMode::Force(threads),
                cancel_batch_enabled(),
            )
        } else {
            Self::execute_batch_inner(
                ctx,
                actions,
                SettleMode::Force {
                    parallel: false,
                    workers: None,
                },
                EngineMode::Force(0),
                cancel_batch_enabled(),
            )
        }
    }

    /// s63: `execute_batch` with the Phase-1 cancel-all batching pinned
    /// (`TORUS_CANCEL_BATCH` ignored) and the canonical serial engine/settle
    /// path. For the flag-on/flag-off differential tests (per-process env
    /// vars race across test threads; this doesn't).
    pub fn execute_batch_cancel_mode<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
        actions: &[(Address, NativeAction)],
        cancel_batch: bool,
    ) -> NativeBatchResult {
        Self::execute_batch_inner(
            ctx,
            actions,
            SettleMode::Force {
                parallel: false,
                workers: None,
            },
            EngineMode::Force(0),
            cancel_batch,
        )
    }

    fn execute_batch_inner<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
        actions: &[(Address, NativeAction)],
        settle_mode: SettleMode,
        engine_mode: EngineMode,
        cancel_batch: bool,
    ) -> NativeBatchResult {
        let out = Self::execute_batch_phases(ctx, actions, settle_mode, engine_mode, cancel_batch);
        Self::write_trades_inline(ctx);
        out
    }

    fn execute_batch_phases<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
        actions: &[(Address, NativeAction)],
        settle_mode: SettleMode,
        engine_mode: EngineMode,
        cancel_batch: bool,
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
        // Count entries without walking batch contents, so expanded batches do
        // not repeatedly grow and copy either vector. Invalid batches contribute
        // nothing, matching the execution loop below. On count overflow retain
        // the original capacities and let the original push path handle growth.
        let (flat_capacity, place_capacity) = actions
            .iter()
            .try_fold((0usize, 0usize), |(flat, places), (_, action)| {
                let (entries, place_entries) = match action {
                    NativeAction::PlaceOrder(_) => (1, 1),
                    NativeAction::PlaceOrderBatch(orders) => {
                        if torus_types::batch_len_within_cap(orders.len()) {
                            (orders.len(), orders.len())
                        } else {
                            (0, 0)
                        }
                    }
                    _ => (1, 0),
                };
                Some((flat.checked_add(entries)?, places.checked_add(place_entries)?))
            })
            .unwrap_or((actions.len(), 0));
        let mut flat: Vec<(Address, FlatAction<'_>)> = Vec::with_capacity(flat_capacity);
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
        // r6: this loop was inside the untimed engine share — cancels walk the
        // book and write tombstones, so a cancel-heavy block pays here and
        // nowhere else in the phase table.
        let phase1_timer = std::time::Instant::now();
        let mut place_order_indices: Vec<usize> = Vec::with_capacity(place_capacity);

        if !cancel_batch {
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
        } else {
            // s63 (`TORUS_CANCEL_BATCH=1`): a maximal run of CancelAllOrders
            // executes as one book pass per market. Places only record their
            // index here (they run in Phase 2 either way), so they do not end
            // a run; every other action does, because it may read or mutate a
            // book or a balance a later cancel-all depends on.
            let mut run: Vec<(usize, Address, Option<MarketId>)> = Vec::new();
            let mut i = 0;
            while i < n {
                let (sender, entry) = &flat[i];
                match entry {
                    FlatAction::Place(_) => {
                        place_order_indices.push(i);
                        i += 1;
                    }
                    FlatAction::Other(NativeAction::CancelAllOrders { .. }) => {
                        run.clear();
                        while i < n {
                            match &flat[i].1 {
                                FlatAction::Place(_) => place_order_indices.push(i),
                                FlatAction::Other(NativeAction::CancelAllOrders { market_id }) => {
                                    run.push((i, flat[i].0, *market_id))
                                }
                                FlatAction::Other(_) => break,
                            }
                            i += 1;
                        }
                        for (k, result) in
                            Self::exec_cancel_all_run(ctx, &run).into_iter().enumerate()
                        {
                            total_gas += result.gas_used;
                            results[run[k].0] = result;
                        }
                    }
                    FlatAction::Other(action) => {
                        let result = Self::execute(ctx, sender, action);
                        total_gas += result.gas_used;
                        results[i] = result;
                        i += 1;
                    }
                }
            }
        }
        ctx.phase_accum.phase1_actions_ns += phase1_timer.elapsed().as_nanos();

        if place_order_indices.is_empty() {
            ctx.debug_assert_index_fed();
            return NativeBatchResult { results, total_gas };
        }

        // ---- Phase 2: Pre-reserve margin, assign IDs, partition by market ----
        let margin_timer = std::time::Instant::now();
        // C2: `PreparedOrder.params` borrows from the caller's `actions` slice
        // — the prepared order carries an 8-byte reference through Phases 2-4
        // instead of a per-order deep clone of `PlaceOrderParams`.
        let mut market_batches: HashMap<MarketId, Vec<PreparedOrder<'_>>> = HashMap::default();

        // Open orders after Phase 1 of every sender with an order that takes
        // an open-order slot (read by the serial loop and the sharded workers).
        // Taken per call: a block's post-EVM call sees its pre-EVM call's
        // orders and, through cum_volume, its fills.
        let open_at_start = Self::open_order_counts(
            &ctx.order_books,
            place_order_indices.iter().filter_map(|&i| match &flat[i] {
                (sender, FlatAction::Place(p)) if takes_open_slot(p) => Some(sender),
                _ => None,
            }),
        );

        // O1: write-through balance cache, scoped to this execute_batch call. Serves
        // repeated Phase 2 reserve / Phase 4 release reads for the same sender without
        // re-hitting the overlay's lock + alloc + Borsh path.
        let mut bal_cache = BalanceCache::new();
        // C1: same pattern for position rows — Phase 4 does two position
        // read-modify-writes per fill; they hit this map and flush once
        // (sorted keys) at the end of the call.
        let mut pos_cache = PositionCache::new();
        let mut vol_cache = VolumeCache::default();

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
        // F4 / review 4: reservation basis overrides (reduce-only size, the
        // market orders' mark price) from pre-batch state, computed once and
        // shared by the serial and sharded prepare paths (identical inputs).
        let place_orders: Vec<(usize, Address, &PlaceOrderParams)> = place_order_indices
            .iter()
            .map(|&i| match &flat[i] {
                (sender, FlatAction::Place(p)) => (i, *sender, *p),
                (_, FlatAction::Other(_)) => unreachable!(),
            })
            .collect();
        let basis = Self::phase2_reservation_basis(ctx, &place_orders);
        // F1 / L3-ENG: read-only state for Phase 2, built from FIELDS so it
        // coexists with the stitch's `&mut ctx.next_global_order_id`. Item 6
        // C2: marks from the block's table (fix 1's per-call memo is gone).
        // C6c: the call's valuation state (the backend is frozen from here
        // to the cache flush after settlement).
        let batch_sums = ctx.sums.is_some().then(|| BatchSums::new(&ctx.positions));
        let dense_tiers = DenseTiers::of(&ctx.margin_configs);
        let reader = AccountReader {
            positions: &ctx.positions,
            oracle: &ctx.oracle,
            now: ctx.timestamp,
            margin_configs: &ctx.margin_configs,
            marks: ctx.block_marks.as_ref(),
            sums: ctx.sums.as_ref(),
            batch: batch_sums.as_ref(),
            dense_tiers: Some(&dense_tiers),
            drain_dirty: None,
            fault: &ctx.reader_fault,
        };
        // Option B (s87) best bids, fix A (s92) / row 42 tick and lot, the
        // configs and marks: each batch market's, once (item 6 M1).
        let markets = Self::phase2_markets(&ctx.order_books, &ctx.state, &reader, &place_orders);

        let mut fold_out: Option<FoldOut> = None;
        let mut prep_outcomes: Option<Vec<Option<PrepOutcome>>> = None;
        if engine_threads >= 2
            && (matches!(engine_mode, EngineMode::Force(_))
                || place_order_indices.len() >= parallel_engine_min_orders())
        {
            // Group place indices by sender, first-appearance order (order of
            // groups is irrelevant — shards are disjoint — but keep it
            // deterministic anyway).
            let mut groups: Vec<(Address, Vec<(usize, &PlaceOrderParams)>)> = Vec::new();
            let mut group_of: HashMap<Address, usize> = HashMap::default();
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
                match Self::phase2_parallel_prepare(&reader, &open_at_start, &basis, &markets, &groups, engine_threads, n) {
                    Some((outcomes, out)) => {
                        // Sender shards are disjoint, so the merged output
                        // is exactly the serial loop's.
                        fold_out = Some(out);
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
                let (sender, params) = match &flat[i] {
                    (s, FlatAction::Place(p)) => (s, *p),
                    (_, FlatAction::Other(_)) => unreachable!(),
                };
                let Some(outcome) = outcomes[i].take() else {
                    unreachable!("every place index has a worker outcome");
                };
                Self::stitch_outcome(
                    &mut ctx.next_global_order_id,
                    &ctx.metrics,
                    &markets,
                    &mut market_batches,
                    &mut results,
                    i,
                    sender,
                    params,
                    outcome,
                );
            }
        } else {
            // L3-ENG: the serial loop runs literally the sharded workers'
            // step ([`Self::prepare_one`]) over one fold, in flat order.
            // Item 6 M1: pre-sized (senders with slot-taking orders).
            let mut fold = SenderFold::with_capacity(open_at_start.len());
            for &i in &place_order_indices {
                let (sender, params) = match &flat[i] {
                    (s, FlatAction::Place(p)) => (s, *p),
                    (_, FlatAction::Other(_)) => unreachable!(),
                };
                let outcome = Self::prepare_one(&reader, &open_at_start, &basis, &markets, &mut fold, i, sender, params);
                Self::stitch_outcome(
                    &mut ctx.next_global_order_id,
                    &ctx.metrics,
                    &markets,
                    &mut market_batches,
                    &mut results,
                    i,
                    sender,
                    params,
                    outcome,
                );
            }
            fold_out = Some(fold.finish());
        }
        let FoldOut { cache, pools: pool_takers, excess: excess_by_sender } = fold_out.unwrap_or_default();
        bal_cache.merge_disjoint(cache);

        // F1 (s517, D2): a sender's free margin after ALL its Phase-2
        // reservations is an EXCLUSIVE budget of the market of its FIRST
        // checked taker (flat order); in that book its takers share it as a
        // running budget; its other markets start at 0 — no two market
        // workers spend the same free margin. Item 6 M1: `pool_takers` =
        // the fold's first checked market and pos_net per sender (sender
        // keyed; each pool below is a function of its sender only).
        // Review fix 1 (s517): unchecked orders' committed-but-undebited
        // need comes off their sender's pool (exact integer sum, the fold's
        // `excess_unchecked`).
        // B-blind (s92): top-ups come off what is left for the pools, so
        // they run before the pools are read.
        let [full, partial, none] = Self::sell_top_ups(
            &ctx.positions,
            &markets,
            &basis,
            &pool_takers,
            &excess_by_sender,
            &mut market_batches,
            &mut bal_cache,
        );
        ctx.phase_accum.sell_top_ups += full + partial;
        if let Some(ref m) = ctx.metrics {
            m.sell_top_ups_full.inc_by(full);
            m.sell_top_ups_partial.inc_by(partial);
            m.sell_top_ups_none.inc_by(none);
        }
        let mut pools: HashMap<(Address, MarketId), FixedPoint> =
            HashMap::with_capacity_and_hasher(pool_takers.len(), Default::default());
        for (&sender, &(market_id, pos_net)) in &pool_takers {
            let available = bal_cache
                .load(&ctx.positions, &sender)
                .map_or(FixedPoint::ZERO, |b| b.available);
            let excess = excess_by_sender.get(&sender).copied().unwrap_or(FixedPoint::ZERO);
            pools.insert((sender, market_id), available + pos_net - excess);
        }

        let margin_elapsed = margin_timer.elapsed();
        ctx.phase_accum.margin_ns += margin_elapsed.as_nanos();
        if let Some(ref m) = ctx.metrics {
            m.exec_phase_margin_seconds
                .observe(margin_elapsed.as_secs_f64());
        }

        // ---- Phase 3: Parallel matching ----
        let match_timer = std::time::Instant::now();
        let mut worker_batches: HashMap<MarketId, (OrderBook, Vec<MatchRequest<'_>>)> =
            HashMap::default();

        for (&market_id, prepared) in &market_batches {
            // Item 6 M1 (row 42): a missing book gets the tick / lot Phase 2
            // checked against (`shapes` holds every batch market).
            let mut book = ctx.order_books.remove(&market_id).unwrap_or_else(|| {
                let (tick, lot) = markets
                    .get(&market_id)
                    .map(|m| m.shape)
                    .unwrap_or_else(|| Self::market_shape(&ctx.state, market_id));
                OrderBook::new(market_id, tick, lot)
            });

            // s515 (BUG 2): police reduce-only orders. Positions are loaded
            // as of this point (all Phase-1 effects, none of this batch's
            // fills); the book then advances them through every fill of the
            // batch in this market (positions are keyed per market, and only
            // this worker fills this market), so every order is checked
            // against the position left by the block's earlier orders.
            // Review 5: checked takers are tracked the same way — their
            // closing fills are free at match time — so every path frees
            // exactly the position the single path would read. F1 (s517):
            // EVERY sender of the batch in this market is tracked (checked
            // takers value their position; maker checks need in-batch
            // positions).
            // Item 6 M1: a sender whose position Phase 2 read (`pre_pos`,
            // same reader, nothing written in between) is not read again.
            if !prepared.is_empty() || book.has_reduce_only_orders() {
                let mut known = ReduceOnlyPositions::new();
                for p in prepared {
                    if let Some((signed, _)) = p.pre_pos {
                        known.insert(p.sender, signed);
                    }
                }
                let ro = Self::reduce_only_positions_for(
                    &reader,
                    &book,
                    market_id,
                    prepared.iter().filter(|p| p.pre_pos.is_none()).map(|p| p.sender),
                    known,
                );
                book.set_reduce_only_positions(ro);
            }

            // s515 review 4: one shared copy of the market's tiers for the
            // checked takers' match-time margin limits.
            let tiers = Self::margin_tiers(ctx.margin_configs.get(&market_id));
            // F1 (s517, D2): each checked sender's exclusive pool (0 outside
            // the market of its first checked taker) and valuation price.
            let mut am = AccountMargins::new(tiers.clone());
            // s94 option 1: every fill's loss against the mark is charged.
            // An unchecked taker (GTC buy, reduce-only) pays a charge from
            // its pool here, else its snapshot — the pre-batch state, which
            // still holds its own reservation (the book takes it off).
            am.set_mark(markets.get(&market_id).and_then(|m| m.band).map(|b| b.reference));
            am.set_pre_batch_snapshots(true);
            for p in prepared.iter().filter(|p| p.checked_pos_net.is_some()) {
                if am.get(&p.sender).is_none() {
                    // Item 6 M1: Phase 2's `position_px` (every checked
                    // taker passed an account check, so it has `pre_pos`).
                    let px = match p.pre_pos {
                        Some((_, px)) => px,
                        None => reader.position_px(&p.sender, market_id).map_or(FixedPoint::ZERO, |(_, px)| px),
                    };
                    #[cfg(test)]
                    if let Some(s) = reader.sums.filter(|s| s.shadow) {
                        let want = reader.position_px(&p.sender, market_id).map_or(FixedPoint::ZERO, |(_, px)| px);
                        if want != px || p.pre_pos.is_none() {
                            s.shadow_mismatches.lock().unwrap().push(format!(
                                "M1 px {} {market_id}: reused {px:?}, position_px {want:?}, pre_pos {:?}",
                                p.sender, p.pre_pos
                            ));
                        }
                    }
                    // Review fix 4 (s517): only the pool market's entry is
                    // the sender's account (shared with its makers there).
                    match pools.get(&(p.sender, market_id)) {
                        Some(&pool) => am.insert(p.sender, pool, px),
                        None => am.insert_taker_only(p.sender, px),
                    }
                }
            }
            book.set_account_margins(am);
            let requests: Vec<MatchRequest<'_>> = prepared
                .iter()
                .map(|p| MatchRequest {
                    sender: p.sender,
                    params: p.params,
                    order_id: p.order_id,
                    margin: p
                        .checked_pos_net
                        .map(|_| Self::taker_margin_limit(&tiers, p.params, p.margin_reserved)),
                })
                .collect();

            worker_batches.insert(market_id, (book, requests));
        }

        // F1 (s517 #4): workers only READ the backend through `reader`.
        // Item 6 C3: a maker's `free` = its balance (frozen backend during
        // matching) + its position sums, memoised for the block (was fix 1's
        // per-call `BatchMakerAccounts`): the same value in every market.
        let mut market_results = match MarketWorkerPool::match_parallel_with(
            worker_batches,
            ctx.timestamp,
            Some(&reader),
        ) {
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
        let match_elapsed = match_timer.elapsed();
        ctx.phase_accum.match_ns += match_elapsed.as_nanos();
        if let Some(ref m) = ctx.metrics {
            m.exec_phase_match_seconds
                .observe(match_elapsed.as_secs_f64());
        }

        // ---- Phase 4: settlement ----
        // A5: settle markets in market-id order. `match_parallel` returns
        // HashMap iteration order (random per instance) — balance mutations
        // are commutative so consensus state never depended on it, but the
        // per-block trade_index assignment (node-local trade keys) and the
        // defensive `.min(order_margin)` clamps on the new cross-trader maker
        // releases do observe settlement order. Sorting pins both.
        market_results.sort_by_key(|m| m.market_id);
        // s515: the policing positions are batch-scoped (never let them go
        // stale on a resident book); stops fired during matching are placed
        // after settlement, in market-id / order / trigger order.
        let mut triggered: VecDeque<TriggeredStop> = VecDeque::new();
        for mbr in market_results.iter_mut() {
            // P2-1: what rested or was stored in this book feeds the index.
            TraderMarkets::absorb(&mut ctx.trader_markets, &mut mbr.book, mbr.market_id);
            mbr.book.clear_reduce_only_positions();
            // Plan 9.11 (telemetry): this book's charged maker fills.
            if let Some(m) = ctx.metrics.as_deref() {
                m.maker_offmark_charged_fills.inc_by(mbr.book.account_margins().charged_maker_fills());
            }
            mbr.book.clear_account_margins();
            for r in &mbr.results {
                triggered.extend(r.result.triggered_stops.iter().cloned());
            }
            // s92 counters (aligned with the market's prepared orders).
            if let (Some(m), Some(prepared)) = (ctx.metrics.as_deref(), market_batches.get(&mbr.market_id)) {
                for (r, p) in mbr.results.iter().zip(prepared) {
                    Self::record_margin_cuts(m, p.params.is_buy, &r.result, p.res_price, mbr.book.tick_size, p.top_up_candidate);
                }
            }
        }
        let settle_timer = std::time::Instant::now();
        // C3: parallel settle pays a thread scope + plan handoff, so it needs
        // >=2 markets with work (always) and, in Auto mode, enough fills to
        // amortize the overhead. The sequential loop remains the canonical
        // semantics that the parallel path must reproduce byte-for-byte.
        let use_parallel = market_results.len() >= 2
            && match settle_mode {
                SettleMode::Force { parallel, .. } => parallel,
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
            // R6: pass-A worker cap. Pinned caps come from `SettleMode::Force`
            // (tests / A-B benches); production resolves it from the env.
            let settle_workers = match settle_mode {
                SettleMode::Force {
                    workers: Some(w), ..
                } => w.max(1),
                _ => settle_worker_cap(),
            };
            Self::settle_market_results_parallel(
                ctx,
                market_results,
                &market_batches,
                &mut results,
                &mut total_gas,
                &mut bal_cache,
                &mut pos_cache,
                &mut vol_cache,
                settle_workers,
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
                &mut vol_cache,
            );
        }

        // C1/O1: materialize all deferred position + balance mutations into the
        // overlay before the call returns (each in deterministic sorted-key
        // order), so the next execute_batch call and every post-batch consumer
        // sees authoritative state. A flush failure means committed fills are
        // not in the overlay — that block must never be applied, so latch the
        // fatal (the committer halts the execution pipeline on it).
        // r6: the two sorted flush_all walks are the tail of Phase 4 and were
        // only ever visible lumped into `exec_phase_settle_seconds`.
        let cache_flush_timer = std::time::Instant::now();
        // Plan 9.8: the test-only per-row reference (P2-2's comparison point).
        #[cfg(any(test, feature = "test-reference-paths"))]
        let pos_flushed = if ctx.test_flush_per_row {
            pos_cache.flush_all_per_row(&ctx.positions)
        } else {
            pos_cache.flush_all(&ctx.positions)
        };
        #[cfg(not(any(test, feature = "test-reference-paths")))]
        let pos_flushed = pos_cache.flush_all(&ctx.positions);
        if let Err(e) = pos_flushed {
            ctx.fatal_error = Some(format!("position cache flush failed: {e}"));
        }
        if let Err(e) = bal_cache.flush_all(&ctx.positions) {
            ctx.fatal_error = Some(format!("balance cache flush failed: {e}"));
        }
        let mut volumes: Vec<_> = vol_cache.into_iter().collect();
        volumes.sort_unstable_by_key(|(trader, _)| *trader);
        for (trader, add) in volumes {
            if let Err(e) = Self::add_cum_volume(&ctx.positions, &trader, add) {
                ctx.fatal_error = Some(format!("cum_volume flush failed: {e}"));
            }
        }
        ctx.phase_accum.cache_flush_ns += cache_flush_timer.elapsed().as_nanos();

        // s515: stops fired during matching go through the single-action
        // placement path against the fully settled state (positions and
        // balances flushed above) — reduce-only re-checked at trigger time.
        if ctx.fatal_error.is_none() {
            Self::run_triggered_stops(ctx, triggered);
        }

        let settle_elapsed = settle_timer.elapsed();
        ctx.phase_accum.settle_ns += settle_elapsed.as_nanos();
        if let Some(ref m) = ctx.metrics {
            m.exec_phase_settle_seconds
                .observe(settle_elapsed.as_secs_f64());
        }

        ctx.debug_assert_index_fed();
        NativeBatchResult { results, total_gas }
    }

    /// L3-ENG: Phase 2 of ONE PlaceOrder — shared by the serial loop and
    /// the sharded workers so both run literally the same code. s515:
    /// validation + the reservation formula of `exec_place_order`; F4 /
    /// review 4: `basis` overrides; F2: an overflowing notional rejects.
    /// Review 4: a checked taker's match-time budget is its reservation +
    /// the available balance left after it, in this per-sender fold.
    /// Reduce-only is NOT pre-checked here: Phase 2 only sees pre-batch
    /// positions, so the book polices it at match time (Phase 3).
    /// Option B (s87) bid floor and fix A (s92) shape: `markets` =
    /// [`phase2_markets`]. Item 6 M1: one `senders` entry and at most one
    /// `proj` entry per order (same reads, same arithmetic, same order).
    #[allow(clippy::too_many_arguments)]
    fn prepare_one<T: StateBackend>(
        reader: &AccountReader<'_, T>,
        open_at_start: &HashMap<Address, u32>,
        basis: &HashMap<usize, (FixedPoint, FixedPoint)>,
        markets: &HashMap<MarketId, Phase2Market<'_>>,
        fold: &mut SenderFold,
        i: usize,
        sender: &Address,
        params: &PlaceOrderParams,
    ) -> PrepOutcome {
        let st = fold.state(sender);
        // Open-order limit first: a rejected order reserves nothing.
        let taken = match Self::take_open_slot(&mut st.open_slots, reader.positions, open_at_start, sender, params) {
            Ok(taken) => taken,
            Err((funnel, msg)) => return PrepOutcome::Reject { funnel, reason: funnel.failure(), msg },
        };
        if let Err((reason, msg)) = Self::validate_order_price(params) {
            return PrepOutcome::Reject {
                funnel: RejectReason::Other,
                reason,
                msg,
            };
        }
        // Every market of the batch has an entry (the fallback is never
        // taken; it reads what the entry would hold).
        let market = match markets.get(&params.market_id) {
            Some(m) => *m,
            None => Phase2Market {
                shape: (FixedPoint::ONE, FixedPoint::ONE),
                bid_floor: None,
                cfg: reader.margin_configs.get(&params.market_id),
                mark: reader.mark(params.market_id),
                band: None,
                orders: 0,
            },
        };
        // Fix A (s92): the book's dust / off-tick rejects, before anything
        // is reserved, projected or pooled.
        if let Some((reason, msg)) = Self::book_shape_violation(params, market.shape) {
            return PrepOutcome::Reject {
                funnel: RejectReason::Other,
                reason,
                msg,
            };
        }
        // s94 option 2: the price band, same place and effect as fix A.
        if let Some(v) = market.band.and_then(|b| price_band_violation(params, b)) {
            return PrepOutcome::Reject {
                funnel: RejectReason::Other,
                reason: FailureReason::PriceBand,
                msg: v.placement_message(),
            };
        }
        let (base_price, res_qty) = (!basis.is_empty())
            .then(|| basis.get(&i).copied())
            .flatten()
            .unwrap_or((Self::reserve_price(params), params.quantity));
        let cfg = market.cfg;
        // Option B (s87): outside its D2 pool market a sell's match-time
        // budget is only its own reservation (`insert_taker_only`), so it
        // reserves — and is placement-checked — for the best bid it can hit
        // at the start of Phase 2 (`max(base, best bid)`, quantity unchanged).
        // Its hold, resting row and every release stay at the limit (A5).
        // A best bid whose notional would overflow keeps `base`.
        let floor_candidate = Self::takes_bid_floor(params) && st.pool.is_some_and(|m| m != params.market_id);
        let mut res_price = base_price;
        let mut floored = None;
        if floor_candidate {
            if let Some(bid) = market.bid_floor {
                if bid > res_price {
                    if let Ok(r) = Self::try_reserve_for_qty_cfg(cfg, bid, res_qty) {
                        res_price = bid;
                        floored = Some(r);
                    }
                }
            }
        }
        // M1: the floor's reservation is the one at `res_price` (computed once).
        let required = match floored.map_or_else(|| Self::try_reserve_for_qty_cfg(cfg, res_price, res_qty), Ok) {
            Ok(r) => r,
            Err(msg) => {
                return PrepOutcome::Reject {
                    funnel: RejectReason::Other,
                    reason: FailureReason::Price,
                    msg,
                }
            }
        };
        let checked = Self::match_margin_checked(params);
        let needs_account = !params.reduce_only;
        let mut pos_net = FixedPoint::ZERO;
        let mut excess = FixedPoint::ZERO;
        let mut projection: Option<&mut Projection> = None;
        if required > FixedPoint::ZERO || checked || needs_account {
            let bal = match SenderState::load_balance(&mut st.balance, reader.positions, sender) {
                Ok(bal) => bal,
                Err(e) => {
                    return PrepOutcome::Reject {
                        funnel: RejectReason::Other,
                        reason: FailureReason::Other,
                        msg: e.to_string(),
                    }
                }
            };
            // F1 (s517, D1 strict HL): the account check is the ONLY
            // placement gate — no `available >= reservation`; the debit
            // below may take `available` negative.
            if needs_account {
                let pn = match st.pos_net {
                    Some(v) => v,
                    None => match reader.pos_net(sender) {
                        Ok(v) => *st.pos_net.insert(v),
                        Err(e) => {
                            return PrepOutcome::Reject {
                                funnel: RejectReason::Other,
                                reason: FailureReason::Other,
                                msg: e.to_string(),
                            }
                        }
                    },
                };
                // `position_px` with the market's mark read once per call.
                let e = match st.proj.get_or_try_insert(params.market_id, || {
                    reader.get_position(sender, params.market_id).map(|pos| {
                        let (signed, px) = AccountReader::<T>::signed_px(pos.as_ref(), market.mark);
                        Projection { signed, px, pre: (signed, px) }
                    })
                }) {
                    Ok(e) => e,
                    Err(e) => {
                        return PrepOutcome::Reject {
                            funnel: RejectReason::Other,
                            reason: FailureReason::Other,
                            msg: e.to_string(),
                        }
                    }
                };
                // Decision s517: only a match-checked order sees the
                // projected releases of the sender's earlier orders;
                // unchecked ones (GTC buys, stops) stay strict.
                let credit = if checked { st.released } else { FixedPoint::ZERO };
                // Review fix 1: minus what earlier orders committed.
                let need = match Self::account_check(
                    market.tiers(),
                    e.signed,
                    e.px,
                    params,
                    res_price,
                    bal.available + pn + credit - st.committed,
                ) {
                    Ok(need) => need,
                    Err((reason, msg)) => {
                        return PrepOutcome::Reject {
                            funnel: RejectReason::Margin,
                            reason,
                            msg,
                        }
                    }
                };
                // Review fix 1 (s517): the need beyond the reservation
                // stays committed (the projection advances the position as
                // if filled).
                excess = (need - required).max(FixedPoint::ZERO);
                if excess > FixedPoint::ZERO {
                    st.committed += excess;
                }
                pos_net = pn;
                projection = Some(e);
            }
            if required > FixedPoint::ZERO {
                bal.available -= required;
                bal.order_margin += required;
                st.balance_dirty = true;
            }
        }
        let pre_pos = projection.as_ref().map(|e| e.pre);
        // F1 (D6): project the accepted order as if filled, valuing an
        // in-batch position at its first order's price.
        if !Self::is_stop(params) {
            if let Some(e) = projection {
                // Decision s517: the IM its closing part releases (at the
                // projection's valuation; overflow = no credit).
                let tiers = market.tiers();
                let size = if e.signed < FixedPoint::ZERO { -e.signed } else { e.signed };
                let closing = params.quantity.min(reduce_only_allowance(e.signed, params.is_buy));
                // Item 6 cut 2: nothing closes -> `b == a`, the IM delta is
                // 0 and nothing is credited, so it is not computed. Only with
                // `px > 0`: then `b` is the account check's `before` (same
                // size and price), whose IM was just computed, so skipping
                // it cannot skip a 0x-tier panic either.
                if closing > FixedPoint::ZERO || e.px <= FixedPoint::ZERO {
                    let release = size
                        .checked_mul(e.px)
                        .ok()
                        .zip((size - closing).checked_mul(e.px).ok())
                        .map(|(b, a)| -torus_core::margin::im_delta(tiers, b, a));
                    if let Some(r) = release.filter(|r| *r > FixedPoint::ZERO) {
                        st.released = st.released.checked_add(r).unwrap_or(st.released);
                    }
                }
                e.signed = if params.is_buy {
                    e.signed + params.quantity
                } else {
                    e.signed - params.quantity
                };
                if e.px == FixedPoint::ZERO {
                    e.px = base_price;
                }
            }
        }
        // The order passed: it now holds its open-order slot.
        if taken.is_some() {
            st.open_slots = taken;
        }
        if checked {
            st.pool.get_or_insert(params.market_id);
        } else {
            // Review fix 1: an unchecked order's excess comes off the pool.
            st.excess_unchecked += excess;
        }
        PrepOutcome::Pass(PrepPass {
            required,
            checked_pos_net: checked.then_some(pos_net),
            pre_pos,
            top_up_candidate: floor_candidate,
            res_price,
            #[cfg(test)]
            excess_im: if checked { FixedPoint::ZERO } else { excess },
        })
    }

    /// L3-ENG: apply one Phase-2 outcome in flat order — a pass gets the
    /// next global order id and joins its market's batch; a reject records
    /// its funnel counter (`orders_rejected_margin` vs `_other`) and error.
    /// Field-level borrows, so it coexists with an [`AccountReader`].
    #[allow(clippy::too_many_arguments)]
    fn stitch_outcome<'a>(
        next_id: &mut u128,
        metrics: &Option<Arc<torus_telemetry::Metrics>>,
        markets: &HashMap<MarketId, Phase2Market<'_>>,
        market_batches: &mut HashMap<MarketId, Vec<PreparedOrder<'a>>>,
        results: &mut [NativeActionResult],
        i: usize,
        sender: &Address,
        params: &'a PlaceOrderParams,
        outcome: PrepOutcome,
    ) {
        match outcome {
            PrepOutcome::Pass(pass) => {
                let order_id = *next_id;
                *next_id += 1;
                market_batches
                    .entry(params.market_id)
                    .or_insert_with(|| Vec::with_capacity(markets.get(&params.market_id).map_or(0, |m| m.orders)))
                    .push(PreparedOrder {
                        index: i,
                        sender: *sender,
                        params,
                        order_id,
                        margin_reserved: pass.required,
                        checked_pos_net: pass.checked_pos_net,
                        pre_pos: pass.pre_pos,
                        top_up_candidate: pass.top_up_candidate,
                        res_price: pass.res_price,
                    });
            }
            PrepOutcome::Reject { funnel, reason, msg } => {
                // Funnel (perf A1): died pre-book.
                if let Some(ref m) = metrics {
                    funnel.count(m);
                }
                results[i] = NativeActionResult::rejected("place_order", (reason, msg));
            }
        }
    }

    /// L3-ENG: sharded Phase-2 prepare. `groups` is the per-sender partition
    /// of the batch's PlaceOrders (each sender's orders in flat order);
    /// workers process disjoint contiguous shards of the sender list, each
    /// replaying its senders' balance folds against a worker-local
    /// fold (read-through to the shared overlay, which at this
    /// point holds all Phase-1 effects — exactly what the serial loop reads).
    ///
    /// Determinism (docs/design-parallel-engine.md §3): an order's outcome is
    /// a pure function of (margin config, params, its sender's balance
    /// trajectory), and the trajectory is a fold over that sender's own
    /// orders only — no other Phase-2 step touches it — so outcomes are
    /// independent of shard assignment and thread count. Returns the
    /// per-flat-index outcomes plus the merged (sender-disjoint) fold
    /// output; `None` if any worker panicked (caller falls back to the
    /// serial loop — nothing shared has been mutated).
    #[allow(clippy::too_many_arguments)]
    fn phase2_parallel_prepare<T: StateBackend>(
        reader: &AccountReader<'_, T>,
        open_at_start: &HashMap<Address, u32>,
        basis: &HashMap<usize, (FixedPoint, FixedPoint)>,
        markets: &HashMap<MarketId, Phase2Market<'_>>,
        groups: &[(Address, Vec<(usize, &PlaceOrderParams)>)],
        threads: usize,
        n: usize,
    ) -> Option<(Vec<Option<PrepOutcome>>, FoldOut)> {
        let workers = threads.min(groups.len()).max(1);
        let shard = groups.len().div_ceil(workers);

        type WorkerOut = (Vec<(usize, PrepOutcome)>, FoldOut);
        torus_state::spawn_count::add(
            torus_state::spawn_count::SpawnSite::MarginPrepare,
            groups.len().div_ceil(shard),
        );
        let worker_results: Vec<Result<WorkerOut, ()>> = std::thread::scope(|s| {
            let handles: Vec<_> = groups
                .chunks(shard)
                .map(|shard_groups| {
                    s.spawn(move || {
                        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            let orders: usize = shard_groups.iter().map(|(_, o)| o.len()).sum();
                            let mut fold = SenderFold::with_capacity(shard_groups.len());
                            let mut out: Vec<(usize, PrepOutcome)> = Vec::with_capacity(orders);
                            for (sender, orders) in shard_groups {
                                for &(i, params) in orders {
                                    // L3-ENG: literally the serial loop's step.
                                    out.push((
                                        i,
                                        Self::prepare_one(
                                            reader,
                                            open_at_start,
                                            basis,
                                            markets,
                                            &mut fold,
                                            i,
                                            sender,
                                            params,
                                        ),
                                    ));
                                }
                            }
                            (out, fold.finish())
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
        let mut merged = FoldOut::default();
        for r in worker_results {
            match r {
                Ok((out, fold)) => {
                    for (i, o) in out {
                        outcomes[i] = Some(o);
                    }
                    merged.merge_disjoint(fold);
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
        vol_cache: &mut VolumeCache,
    ) {
        // r6: the canonical loop IS the apply pass — attribute it to pass B so
        // the sequential and parallel paths land in the same histogram (the
        // parallel path's pass A stays 0 here, which is how the phase table
        // shows which settle path a cell ran).
        let pass_b_timer = std::time::Instant::now();
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

                // Release margin for filled/cancelled portion (A5 telescoping:
                // reserved minus the reserve still owed for what rests).
                {
                    let margin_to_release = Self::taker_margin_release_cfg(
                        ctx.margin_configs.get(&market_id),
                        prep.params,
                        prep.margin_reserved,
                        result,
                    );

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
                // s80: effects are kept (for the fills' extras) only while a
                // stream wants fills; otherwise this is the s78 loop.
                let mut fill_failed = false;
                let record_fills = ctx.record_fills;
                if record_fills {
                    ctx.fill_effects_scratch.clear();
                }
                for fill in &result.fills {
                    let taker_is_buy = fill.maker_side != Side::Buy;
                    let notional = fill.price * fill.quantity;
                    let taker = match Self::apply_fill_via_caches(
                        &ctx.positions,
                        pos_cache,
                        bal_cache,
                        &fill.taker,
                        market_id,
                        taker_is_buy,
                        fill.quantity,
                        fill.price,
                    ) {
                        Ok(effect) => effect,
                        Err(e) => {
                            results[prep.index] = NativeActionResult::rejected(
                                "place_order",
                                (FailureReason::Fill, format!("taker fill failed: {e}")),
                            );
                            fill_failed = true;
                            break;
                        }
                    };
                    *vol_cache.entry(fill.taker).or_insert(FixedPoint::ZERO) += notional;
                    let maker = match Self::apply_fill_via_caches(
                        &ctx.positions,
                        pos_cache,
                        bal_cache,
                        &fill.maker,
                        market_id,
                        fill.maker_side == Side::Buy,
                        fill.quantity,
                        fill.price,
                    ) {
                        Ok(effect) => effect,
                        Err(e) => {
                            results[prep.index] = NativeActionResult::rejected(
                                "place_order",
                                (FailureReason::Fill, format!("maker fill failed: {e}")),
                            );
                            fill_failed = true;
                            break;
                        }
                    };
                    *vol_cache.entry(fill.maker).or_insert(FixedPoint::ZERO) += notional;
                    if record_fills {
                        ctx.fill_effects_scratch.push([taker, maker]);
                    }
                }

                if fill_failed {
                    // Funnel (perf A1): died on fill application, not on the book.
                    if let Some(ref m) = ctx.metrics {
                        m.orders_rejected_other.inc();
                    }
                    continue;
                }

                // Persist trades (with each fill's effects from above while a
                // stream wants fills).
                if record_fills {
                    for (i, fill) in result.fills.iter().enumerate() {
                        let [taker, maker] = ctx.fill_effects_scratch[i];
                        Self::persist_trade_with_extras(ctx, market_id, fill, taker, maker);
                    }
                } else {
                    for fill in &result.fills {
                        Self::persist_trade(ctx, market_id, fill);
                    }
                }

                if let Some(ref m) = ctx.metrics {
                    m.orders_matched.inc_by(result.fills.len() as u64);
                    Self::record_order_status_funnel(m, &result.status, result.fills.len());
                }

                *total_gas += PLACE_ORDER_GAS;
                results[prep.index] = NativeActionResult::placed(result);
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
        ctx.phase_accum.settle_pass_b_ns += pass_b_timer.elapsed().as_nanos();
    }

    /// C3: deterministic parallel Phase-4 settlement.
    ///
    /// Pass A (parallel, markets LPT-packed across at most `max_workers`
    /// scoped threads — same pool shape as Phase-3 matching): a PURE compute
    /// of each market's settle plan. Chunking is scheduling only; plans are
    /// scattered back by input index, so pass B is unaffected. Workers
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
        vol_cache: &mut VolumeCache,
        max_workers: usize,
    ) {
        // ---- Pass A: pure per-market plans on CAPPED chunk workers ----
        //
        // R6: this used to spawn one scoped thread PER MARKET. At 300 markets
        // that is 300 threads on ~5 free cores plus 300x thread prologue for
        // work that is often only a few fills wide. Markets are now packed by
        // the same deterministic LPT chunker the matching pool and the
        // save-books drain use (`MarketWorkerPool::chunk_indices`), and each
        // worker computes its chunk's plans sequentially.
        //
        // Chunk layout is SCHEDULING ONLY: results are scattered back by input
        // index, so `plans[i]` always belongs to `market_results[i]` no matter
        // how markets were packed. `compute_market_settle_plan` is pure in
        // (`&PositionManager` reads, market config, this market's
        // `MarketBatchResult` + `PreparedOrder`s, block height, timestamp) and
        // writes only into its own fresh `PositionCache` — no pass-A plan can
        // observe another market's pass-A output, so packing several markets
        // onto one worker cannot change any plan. Pass B is untouched.
        // r7 restack: pass A is the parallel half (now LPT-chunked workers,
        // r6 measured it at 463 ms/blk of the 699 ms settle in the 300-market
        // cell); pass B below is the serial apply. Splitting them tells whether
        // the settle cost is worker scheduling or the serial diet.
        let pass_a_timer = std::time::Instant::now();
        let plans: Vec<Result<MarketSettlePlan, String>> = {
            let positions = &ctx.positions;
            let margin_configs = &ctx.margin_configs;
            let record_fills = ctx.record_fills;
            let mrs: &[crate::market_workers::MarketBatchResult] = &market_results;

            // Per-market weight for the LPT packer: plan cost is one pass over
            // the market's orders plus two position round-trips per fill.
            let weights: Vec<(MarketId, usize)> = mrs
                .iter()
                .map(|mbr| {
                    let fills: usize = mbr.results.iter().map(|r| r.result.fills.len()).sum();
                    (mbr.market_id, mbr.results.len() + 2 * fills)
                })
                .collect();

            // One market's contained plan compute. `&` so it can be shared by
            // every worker (copy-captures only the two u64s and the refs).
            let plan_for = |i: usize| -> Result<MarketSettlePlan, String> {
                let mbr = &mrs[i];
                let prepared: &[PreparedOrder<'_>] = market_batches
                    .get(&mbr.market_id)
                    .map(|v| v.as_slice())
                    .unwrap_or(&[]);
                let cfg = margin_configs.get(&mbr.market_id);
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    Self::compute_market_settle_plan(
                        positions,
                        cfg,
                        mbr,
                        prepared,
                        record_fills,
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
            };

            let chunks =
                crate::market_workers::MarketWorkerPool::chunk_indices(&weights, max_workers);

            if chunks.len() <= 1 {
                // One worker (cap 1, or a single market): no scope, no spawn.
                (0..mrs.len()).map(plan_for).collect()
            } else {
                let mut slots: Vec<Option<Result<MarketSettlePlan, String>>> =
                    (0..mrs.len()).map(|_| None).collect();
                let plan_for = &plan_for;
                torus_state::spawn_count::add(
                    torus_state::spawn_count::SpawnSite::Settle,
                    chunks.len(),
                );
                let chunk_out: Vec<Vec<(usize, Result<MarketSettlePlan, String>)>> =
                    std::thread::scope(|s| {
                        let handles: Vec<_> = chunks
                            .into_iter()
                            .map(|chunk| {
                                s.spawn(move || {
                                    chunk
                                        .into_iter()
                                        .map(|i| (i, plan_for(i)))
                                        .collect::<Vec<_>>()
                                })
                            })
                            .collect();
                        handles
                            .into_iter()
                            .map(|h| h.join().unwrap_or_default())
                            .collect()
                    });
                // A worker thread that died OUTSIDE the per-market containment
                // yields an empty vec; its markets stay `None` and become the
                // fallback trigger below (never a silently skipped market).
                for out in chunk_out {
                    for (i, plan) in out {
                        slots[i] = Some(plan);
                    }
                }
                slots
                    .into_iter()
                    .map(|p| p.unwrap_or_else(|| Err("settle worker thread died".to_string())))
                    .collect()
            }
        };

        ctx.phase_accum.settle_pass_a_ns += pass_a_timer.elapsed().as_nanos();

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
            ctx.phase_accum.settle_fallbacks += 1;
            return Self::settle_market_results_sequential(
                ctx,
                market_results,
                market_batches,
                results,
                total_gas,
                bal_cache,
                pos_cache,
                vol_cache,
            );
        }
        let plans: Vec<MarketSettlePlan> = plans.into_iter().map(|p| p.unwrap()).collect();

        // ---- Pass B: deterministic apply, markets ascending by id ----
        let pass_b_timer = std::time::Instant::now();
        // One allocation for every market's position entries: merging 300
        // caches into an unreserved map rehashes it at each doubling
        // (capacity only; ubench_position_cache: ~1.2 -> ~0.7 us/entry).
        pos_cache.reserve(plans.iter().map(|p| p.pos_cache.len()).sum());
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

            // Orders of this market that stopped before their last fill side:
            // (order index, sides applied).
            let mut stops: Vec<(usize, usize)> = Vec::new();
            for (k, ((match_result, prep), oplan)) in mbr
                .results
                .iter()
                .zip(prepared.iter())
                .zip(plan.orders)
                .enumerate()
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
                let mut sides_applied = 2 * result.fills.len();
                for (side, trader, pnl, at) in &oplan.pnl_events {
                    match bal_cache.load(&ctx.positions, trader) {
                        Ok(mut bal) => {
                            bal.available += *pnl;
                            bal_cache.set(trader, bal);
                        }
                        Err(e) => {
                            fill_failed = Some(format!("{side} fill failed: {e}"));
                            sides_applied = *at;
                            break;
                        }
                    }
                }
                // Any worker position failure fell back to sequential above,
                // so the only failure here is a balance read: `sides_applied`
                // is the whole stop point.
                debug_assert!(oplan.fill_error.is_none());
                if fill_failed.is_none() {
                    fill_failed = oplan.fill_error;
                }
                // cum_volume counts every fill side the sequential loop
                // completes before its stop (a failed side adds nothing).
                if sides_applied < 2 * result.fills.len() {
                    stops.push((k, sides_applied));
                }

                if let Some(err) = fill_failed {
                    results[prep.index] =
                        NativeActionResult::rejected("place_order", (FailureReason::Fill, err));
                    // Funnel (perf A1): died on fill application, not on the book.
                    if let Some(ref m) = ctx.metrics {
                        m.orders_rejected_other.inc();
                    }
                    continue;
                }

                // Record fills in canonical order, exactly like persist_trade.
                if ctx.record_fills {
                    for (fill, &[taker, maker]) in result.fills.iter().zip(&oplan.fill_effects) {
                        Self::persist_trade_with_extras(ctx, market_id, fill, taker, maker);
                    }
                } else {
                    for fill in &result.fills {
                        Self::persist_trade(ctx, market_id, fill);
                    }
                }

                if let Some(ref m) = ctx.metrics {
                    m.orders_matched.inc_by(result.fills.len() as u64);
                    Self::record_order_status_funnel(m, &result.status, result.fills.len());
                }

                *total_gas += PLACE_ORDER_GAS;
                results[prep.index] = NativeActionResult::placed(result);
            }

            // cum_volume: the worker's per-trader sums when every order ran
            // to its last fill side; otherwise per side, up to each stop.
            if stops.is_empty() {
                for (trader, add) in plan.volumes {
                    *vol_cache.entry(trader).or_insert(FixedPoint::ZERO) += add;
                }
            } else {
                for (k, match_result) in mbr.results.iter().take(prepared.len()).enumerate() {
                    let fills = &match_result.result.fills;
                    let sides = stops
                        .iter()
                        .find(|stop| stop.0 == k)
                        .map_or(2 * fills.len(), |stop| stop.1);
                    add_fill_volumes(vol_cache, fills, sides);
                }
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
        ctx.phase_accum.settle_pass_b_ns += pass_b_timer.elapsed().as_nanos();
    }

    /// C3 pass-A worker: compute one market's settlement plan. PURE with
    /// respect to shared state — reads positions through a fresh per-market
    /// cache, mutates only plan-local data. Mirrors the sequential loop's
    /// per-order semantics exactly (see `settle_market_results_sequential`).
    /// `record_fills` (s80): also keep each fill's effects for its extras.
    fn compute_market_settle_plan<T: StateBackend>(
        positions: &PositionManager<T>,
        cfg: Option<&MarketMarginConfig>,
        mbr: &crate::market_workers::MarketBatchResult,
        prepared: &[PreparedOrder<'_>],
        record_fills: bool,
    ) -> MarketSettlePlan {
        let market_id = mbr.market_id;
        let mut pos_cache = PositionCache::new();
        let mut orders = Vec::with_capacity(prepared.len());
        // At most two traders per fill: sized up front, the map never
        // regrows (s84: regrowth was a third of the cum_volume time).
        let fill_sides: usize = mbr.results.iter().map(|m| 2 * m.result.fills.len()).sum();
        let mut volumes = VolumeCache::with_capacity_and_hasher(fill_sides, Default::default());

        for (match_result, prep) in mbr.results.iter().zip(prepared.iter()) {
            let result = &match_result.result;

            // Taker-side release amount — same formula as sequential.
            let margin_release =
                Self::taker_margin_release_cfg(cfg, prep.params, prep.margin_reserved, result);

            // Fill application: position transitions into the market-local
            // cache; PnL events recorded in exact order; first POSITION-side
            // failure stops the order like sequential (its message matches).
            let mut pnl_events: Vec<(&'static str, Address, FixedPoint, usize)> = Vec::new();
            let mut fill_effects = Vec::new();
            let mut fill_error: Option<String> = None;
            'fills: for (k, fill) in result.fills.iter().enumerate() {
                let taker_is_buy = fill.maker_side != Side::Buy;
                let mut pair = [FillEffect::default(); 2];
                for (s, (slot, (side, trader, is_buy))) in pair
                    .iter_mut()
                    .zip([
                        ("taker", &fill.taker, taker_is_buy),
                        ("maker", &fill.maker, fill.maker_side == Side::Buy),
                    ])
                    .enumerate()
                {
                    match positions.apply_fill_cached_effect(
                        &mut pos_cache,
                        trader,
                        market_id,
                        is_buy,
                        fill.quantity,
                        fill.price,
                        MarginType::Cross,
                    ) {
                        Ok(effect) => {
                            if let Some(pnl) = effect.closed_pnl {
                                pnl_events.push((side, *trader, pnl, 2 * k + s));
                            }
                            *slot = effect;
                        }
                        Err(e) => {
                            fill_error = Some(format!("{side} fill failed: {e}"));
                            break 'fills;
                        }
                    }
                }
                if record_fills {
                    fill_effects.push(pair);
                }
            }
            // A position failure sends the whole call to the sequential loop,
            // so these sums are only used when every side applied.
            if fill_error.is_none() {
                add_fill_volumes(&mut volumes, &result.fills, 2 * result.fills.len());
            }

            orders.push(OrderSettlePlan {
                margin_release,
                pnl_events,
                fill_effects,
                fill_error,
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
            volumes,
        }
    }

    /// C1: apply one side of a fill entirely through the per-batch write-back
    /// caches. The position read-modify-write hits `pos_cache`; if the fill
    /// had a close component, `apply_fill_cached_effect` returns the realized
    /// PnL and it is credited through `bal_cache` (exactly when the classic
    /// `apply_fill` would have written the balance row — including a zero PnL,
    /// which still materializes the row). No overlay access on the hot path.
    /// Returns the fill's effect (s80: recorded with the fill).
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
    ) -> Result<FillEffect, CoreError> {
        let effect = positions.apply_fill_cached_effect(
            pos_cache,
            trader,
            market_id,
            is_buy,
            qty,
            price,
            MarginType::Cross,
        )?;
        if let Some(pnl) = effect.closed_pnl {
            let mut bal = bal_cache.load(positions, trader)?;
            bal.available += pnl;
            bal_cache.set(trader, bal);
        }
        Ok(effect)
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

    /// s92 (B-blind observability; nothing reads it back): one book
    /// outcome's maker margin cancels and reduce-only cuts and, for a SELL
    /// taker whose match-time margin ran out, its cut — `[non_pool]
    /// [partial][bucket]`, the bucket of its hit price minus `res_price` (the
    /// price its reservation was taken at) in ticks of `tick`, rounded up.
    /// `non_pool`: its budget was the D2 taker-only reservation.
    fn record_margin_cuts(
        m: &torus_telemetry::Metrics,
        is_buy: bool,
        result: &PlaceResult,
        res_price: FixedPoint,
        tick: FixedPoint,
        non_pool: bool,
    ) {
        m.maker_margin_cancels.inc_by(result.margin_cancels.len() as u64);
        m.reduce_only_cuts.inc_by(result.reduce_only_cuts.len() as u64);
        if let (false, Some(hit)) = (is_buy, result.margin_cut_price) {
            let d = hit.raw().saturating_sub(res_price.raw());
            let ticks = if d <= 0 { 0 } else { (d - 1) / tick.raw().max(1) + 1 };
            let bucket = torus_telemetry::margin_cut_tick_bucket(ticks);
            m.sell_margin_cuts[usize::from(non_pool)][usize::from(!result.fills.is_empty())][bucket].inc();
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
    ///
    /// C3: ctx-free — pure in (market margin config, price, qty), so
    /// settle-plan workers can compute release amounts off-thread with
    /// byte-identical arithmetic. Placement goes through
    /// [`try_reserve_for_qty_cfg`] (F2: overflow rejects instead of panicking).
    fn reserve_for_qty_cfg(
        cfg: Option<&MarketMarginConfig>,
        price: FixedPoint,
        qty: FixedPoint,
    ) -> FixedPoint {
        if price <= FixedPoint::ZERO || qty <= FixedPoint::ZERO {
            return FixedPoint::ZERO;
        }
        // s515 review 4: the formula lives in torus-core so the book's
        // match-time margin check computes it identically.
        order_initial_margin(cfg.map(|c| c.tiers.as_slice()), price * qty)
    }

    /// F2 (s515 review): the PLACEMENT-time reservation — [`reserve_for_qty_cfg`]
    /// (byte-identical result), except that a price × qty past `i128::MAX` is an
    /// order rejection instead of a panic (every validator would panic on the
    /// same action — a chain halt). Every later release is a difference of
    /// `reserve_for_qty_cfg` at quantities <= the reserved one, so it cannot
    /// overflow once this passed.
    fn try_reserve_for_qty_cfg(
        cfg: Option<&MarketMarginConfig>,
        price: FixedPoint,
        qty: FixedPoint,
    ) -> Result<FixedPoint, String> {
        if price <= FixedPoint::ZERO || qty <= FixedPoint::ZERO {
            return Ok(FixedPoint::ZERO);
        }
        // Item 6 M1: the product once (`reserve_for_qty_cfg`'s `price * qty`
        // is this `checked_mul`, unwrapped).
        match price.checked_mul(qty) {
            Ok(notional) => Ok(order_initial_margin(cfg.map(|c| c.tiers.as_slice()), notional)),
            Err(_) => Err(format!("order notional overflows: price {price} x quantity {qty}")),
        }
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
            // s515: quantity cut from resting reduce-only orders telescopes
            // exactly like consumption by a fill.
            // F1 (s517 #4): so does a maker cancelled for margin.
            for c in r.reduce_only_cuts.iter().chain(&r.margin_cancels) {
                let e = consumed
                    .entry(c.order_id)
                    .or_insert((c.trader, c.price, FixedPoint::ZERO));
                e.2 += c.qty;
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

    /// Per-user open-order limit, one rule for the serial Phase-2 loop, the
    /// sharded workers and `exec_place_order`. Market, IOC and FOK orders
    /// never rest: they pass and take no slot. Any other order (GTC,
    /// PostOnly, a stop while pending) needs a free slot; as on Hyperliquid,
    /// reduce-only and stop orders also need fewer than
    /// `OPEN_ORDER_BASE_LIMIT` open orders. `slots` loads on the
    /// sender's first such order in this `execute_batch` call: its open orders
    /// after Phase 1 (`open_at_start`) and the limit from the stored
    /// `cum_volume`. A slot is taken before matching, so an order the book
    /// then rejects (e.g. a PostOnly cross) keeps it for the call; dust and
    /// off-tick orders are rejected before the book (fix A, s92). Returns
    /// the slots with this order counted; the caller stores them only once the
    /// order also passed its price / shape checks and margin reserve, so a
    /// rejected order takes no slot.
    fn take_open_slot<T: StateBackend>(
        slots: &mut Option<OpenSlots>,
        positions: &PositionManager<T>,
        open_at_start: &HashMap<Address, u32>,
        sender: &Address,
        params: &PlaceOrderParams,
    ) -> Result<Option<OpenSlots>, (RejectReason, String)> {
        if !takes_open_slot(params) {
            return Ok(None);
        }
        let s = match *slots {
            Some(s) => s,
            None => {
                let volume = positions
                    .get_cum_volume(sender)
                    .map_err(|e| (RejectReason::Other, e.to_string()))?;
                *slots.insert(OpenSlots {
                    open: open_at_start.get(sender).copied().unwrap_or(0),
                    limit: open_order_limit(volume),
                })
            }
        };
        let is_stop = matches!(
            params.order_type,
            OrderType::StopMarket { .. } | OrderType::StopLimit { .. }
        );
        if (params.reduce_only || is_stop) && s.open >= OPEN_ORDER_BASE_LIMIT {
            return Err((
                RejectReason::OpenLimit,
                format!(
                    "open order limit: reduce-only and stop orders need fewer than \
                     {OPEN_ORDER_BASE_LIMIT} open orders, have {}",
                    s.open
                ),
            ));
        }
        if s.open >= s.limit {
            return Err((
                RejectReason::OpenLimit,
                format!(
                    "open order limit reached: {} open orders, limit {}",
                    s.open, s.limit
                ),
            ));
        }
        Ok(Some(OpenSlots {
            open: s.open + 1,
            ..s
        }))
    }

    /// Open orders (resting + pending stops) of each of `senders`, summed
    /// over all books (`torus_core::order_book::open_order_counts`: book-outer,
    /// smaller side per book, stops walked once). Split by book over up to the
    /// match worker cap once the walk is big enough to pay for the threads.
    fn open_order_counts<'a>(
        books: &HashMap<MarketId, OrderBook>,
        senders: impl Iterator<Item = &'a Address>,
    ) -> HashMap<Address, u32> {
        let mut idx: HashMap<Address, usize> = HashMap::default();
        for sender in senders {
            let next = idx.len();
            idx.entry(*sender).or_insert(next);
        }
        let refs: Vec<&OrderBook> = books.values().collect();
        let counts = torus_core::order_book::open_order_counts(
            &refs,
            &idx,
            MarketWorkerPool::resolve_worker_cap_named(None),
            OPEN_COUNT_WORK_PER_THREAD,
        );
        idx.into_iter()
            .map(|(sender, i)| (sender, u32::try_from(counts[i]).unwrap_or(u32::MAX)))
            .collect()
    }

    /// Add `amount` to `trader`'s stored lifetime volume (`cum_volume`).
    fn add_cum_volume<T: StateBackend>(
        positions: &PositionManager<T>,
        trader: &Address,
        amount: FixedPoint,
    ) -> Result<(), CoreError> {
        let volume = positions.get_cum_volume(trader)?;
        positions.put_cum_volume(trader, volume + amount)
    }

    /// s515 (BUG 1): the price a RESTING / pending order's margin is
    /// reserved — and every release computed — at: the limit price, a
    /// pending stop-market's slippage cap (`params.price`; its row stores no
    /// other price, so the trigger-time release is exact), or a stop-limit's
    /// limit. One price per order keeps the A5 telescoping identity
    /// (Σ released == reserved) for every order type. A `Market` order
    /// reserves at [`reservation_price`] instead (it never rests).
    fn reserve_price(params: &PlaceOrderParams) -> FixedPoint {
        match params.order_type {
            OrderType::StopLimit { limit, .. } => limit,
            _ => params.price,
        }
    }

    /// s515 review 4 (Hyperliquid: "the margin required to open a position
    /// is position_size * mark_price / leverage"): the price the PLACEMENT
    /// reservation is taken at. A `Market` order (buy or sell, incl. a
    /// triggered stop-market, which is placed as one at trigger time)
    /// reserves at the market's MARK price, falling back to its cap when the
    /// market has no usable oracle price; everything else reserves at
    /// [`reserve_price`]. Safe for the A5 identity: a market order never
    /// rests, so its whole reservation is released after matching. Fills
    /// worse than the mark are bounded by the match-time check instead
    /// ([`match_margin_checked`]).
    fn reservation_price(params: &PlaceOrderParams, mark: Option<FixedPoint>) -> FixedPoint {
        match params.order_type {
            OrderType::Market => mark.unwrap_or(params.price),
            _ => Self::reserve_price(params),
        }
    }

    /// s515 review 4: the mark price of `market_id` — the stake-weighted
    /// median aggregated by the oracle and committed in state — or `None`
    /// (F1: one formula, [`AccountReader::mark`]).
    fn mark_price<T: StateBackend>(ctx: &NativeExecContext<T>, market_id: MarketId) -> Option<FixedPoint> {
        AccountReader::of(ctx).mark(market_id)
    }

    /// s515 review 4 (Hyperliquid: margin is checked "when orders are placed
    /// and again when they match"): whether a taker's fills can cost more
    /// initial margin than it reserved, so the book re-checks every fill
    /// against a [`TakerMarginLimit`]: a market order (reserved at the mark,
    /// fills anywhere up to its cap) and a limit SELL (reserved at its limit,
    /// fills at bids >= it). A GTC / PostOnly limit buy fills at asks <= its
    /// limit, i.e. within its full reservation. Review 5: an IOC / FOK limit
    /// buy reserves only for its opening part ([`never_rests`]), so it is
    /// checked too. Reduce-only orders are exempt (Hyperliquid: reducing
    /// needs no margin); stops are checked once triggered. F1 (s517): the
    /// budget is the reservation + the sender's running free margin.
    fn match_margin_checked(params: &PlaceOrderParams) -> bool {
        !params.reduce_only
            && match params.order_type {
                OrderType::Market => true,
                OrderType::Limit => !params.is_buy || Self::never_rests(params),
                _ => false,
            }
    }

    /// Option B (s87): a match-checked sell that can take (not PostOnly) —
    /// the orders whose batch reservation, outside the sender's D2 pool
    /// market, is raised to the start-of-batch best bid ([`prepare_one`]).
    /// Reduce-only orders and stops are not match-checked.
    fn takes_bid_floor(params: &PlaceOrderParams) -> bool {
        !params.is_buy && Self::match_margin_checked(params) && params.time_in_force != TimeInForce::PostOnly
    }

    /// s515 review 5 (F2): an order that can never rest — market, or an IOC /
    /// FOK limit. Its whole placement reservation is released after matching
    /// (`rested_qty` = 0), so it reserves only for the quantity beyond what
    /// closes the sender's opposite-side position ([`reduce_only_allowance`]
    /// — closing needs no margin, Hyperliquid) without touching the A5
    /// identity. An order that can rest keeps reserving its full quantity:
    /// the reservation of a resting row is `reserve(price, remaining)` for
    /// every later release, so it cannot hold less.
    fn never_rests(params: &PlaceOrderParams) -> bool {
        match params.order_type {
            OrderType::Market => true,
            OrderType::Limit => {
                matches!(params.time_in_force, TimeInForce::IOC | TimeInForce::FOK)
            }
            _ => false,
        }
    }

    /// s515 review 4: the market's leverage tiers, shared by its checked
    /// takers' [`TakerMarginLimit`]s.
    fn margin_tiers(cfg: Option<&MarketMarginConfig>) -> Option<Arc<[MarginTier]>> {
        cfg.map(|c| Arc::from(c.tiers.as_slice()))
    }

    /// s515 review 4: the book-side match-time margin limit of a checked
    /// taker. F1 (s517): `reserved` is the order's own reservation; the book
    /// adds the sender's running free margin (`AccountMargins`). A GTC
    /// limit's remainder can rest and keeps `reserve(limit, left)`, so its
    /// opening part counts against the budget too.
    fn taker_margin_limit(
        tiers: &Option<Arc<[MarginTier]>>,
        params: &PlaceOrderParams,
        reserved: FixedPoint,
    ) -> TakerMarginLimit {
        let can_rest = matches!(params.order_type, OrderType::Limit)
            && params.time_in_force == TimeInForce::GTC;
        TakerMarginLimit {
            budget: reserved,
            tiers: tiers.clone(),
            hold_price: can_rest.then(|| Self::reserve_price(params)),
        }
    }

    /// F1 (s517): THE placement gate (strict HL, D1 — there is no
    /// `available >= reservation` gate any more): the order's need at the
    /// POSITION-size tier (closing part free) must be `<= 0` (only reduces)
    /// or `<= free` (available + UPnL − position IM, before this
    /// reservation; `available` may be negative). Reduce-only orders are
    /// clamped to the position, so they only reduce and are skipped. A
    /// pending stop is checked as a resting order at its reservation price
    /// (it is re-checked when it triggers); without this, stops would have
    /// no gate at all.
    fn account_check(
        tiers: Option<&[MarginTier]>,
        signed: FixedPoint,
        px: FixedPoint,
        params: &PlaceOrderParams,
        res_price: FixedPoint,
        free: FixedPoint,
    ) -> Result<FixedPoint, Rejection> {
        if params.reduce_only {
            return Ok(FixedPoint::ZERO);
        }
        // A flat (or unmarked) position is valued at the order's own price.
        let px = if px > FixedPoint::ZERO { px } else { res_price };
        let can_rest = !Self::never_rests(params); // stops: true
        match placement_need(tiers, signed, px, params.is_buy, params.quantity, res_price, can_rest) {
            None => Err((
                FailureReason::Price,
                format!("order notional overflows: price {res_price} x quantity {}", params.quantity),
            )),
            Some(need) if need > FixedPoint::ZERO && need > free => Err((
                FailureReason::Margin,
                format!("insufficient margin: need {need}, have {free} (account)"),
            )),
            Some(need) => Ok(need),
        }
    }

    /// F4 (s515 review): whether `params` is a stop (pending until triggered).
    /// A reduce-only stop reserves its full quantity — it is re-checked and
    /// clamped only at trigger time.
    fn is_stop(params: &PlaceOrderParams) -> bool {
        matches!(
            params.order_type,
            OrderType::StopMarket { .. } | OrderType::StopLimit { .. }
        )
    }

    /// F4 (s515 review) / review 4: per-order overrides of the batch Phase-2
    /// reservation basis `(price, qty)` — default `(reserve_price, quantity)`
    /// — from PRE-BATCH state only, computed ONCE so the serial and sharded
    /// prepare paths see identical inputs.
    /// - Market order: priced by [`reservation_price`] at the market's mark
    ///   price, read once per market (the oracle row does not change within
    ///   the block — see [`mark_price`]), so identical to the single path.
    /// - Reduce-only non-stop order: sized to min(qty, bound), where bound is
    ///   an upper bound on the book's match-time clamp — the pre-batch
    ///   allowance plus everything that can still grow the position before the
    ///   order matches (the sender's resting non-reduce-only orders in that
    ///   market and its earlier non-reduce-only orders in this batch). A doomed
    ///   or oversize order no longer reserves (and starves the sender's later
    ///   orders of) margin it can never use, and since the clamp — hence the
    ///   resting remainder — never exceeds the bound, A5 telescoping stays exact.
    /// - Review 5: an order that never rests ([`never_rests`], reduce-only or
    ///   not) reserves only for the quantity beyond the closing allowance
    ///   still left: the pre-batch allowance for that sender / market / side,
    ///   used up (in flat order) by EVERY earlier order of the sender on that
    ///   side, whatever its type — so one allowance never frees two orders.
    ///   The book re-derives the closing part at match time from
    ///   the position advanced through the batch's earlier fills; if those
    ///   shrank it, the extra opening part is charged against the order's
    ///   budget there (the reservation itself is released whole anyway).
    fn phase2_reservation_basis<T: StateBackend>(
        ctx: &NativeExecContext<T>,
        orders: &[(usize, Address, &PlaceOrderParams)],
    ) -> HashMap<usize, (FixedPoint, FixedPoint)> {
        let sat_add =
            |a: FixedPoint, b: FixedPoint| FixedPoint::from_raw(a.raw().saturating_add(b.raw()));
        let track_growth = orders.iter().any(|(_, _, p)| p.reduce_only && !Self::is_stop(p));
        // Item 6 M1: the closing allowances are only read by orders that
        // never rest; without one in the batch they are not tracked (the
        // map is otherwise only written). Pre-sized: one key per order at most.
        let track_closing = orders.iter().any(|(_, _, p)| Self::never_rests(p));
        let mut growth: HashMap<(Address, MarketId), FixedPoint> = HashMap::default();
        let mut marks: HashMap<MarketId, Option<FixedPoint>> = HashMap::default();
        let mut closing: HashMap<(Address, MarketId, bool), (Option<FixedPoint>, FixedPoint)> =
            HashMap::with_capacity_and_hasher(
                if track_closing { orders.len() } else { 0 },
                Default::default(),
            );
        let mut out = HashMap::default();
        for &(i, sender, params) in orders {
            let mark = if matches!(params.order_type, OrderType::Market) {
                *marks
                    .entry(params.market_id)
                    .or_insert_with(|| Self::mark_price(ctx, params.market_id))
            } else {
                None
            };
            let price = Self::reservation_price(params, mark);
            let mut qty = params.quantity;
            if params.reduce_only && !Self::is_stop(params) {
                // A position read error polices as flat in the book; keep the
                // full reservation then (conservative).
                if let Ok(pos) = Self::signed_position(&ctx.positions, &sender, params.market_id) {
                    let resting = ctx.order_books.get(&params.market_id).map_or(FixedPoint::ZERO, |b| {
                        b.orders_for_trader(&sender)
                            .iter()
                            .filter(|o| !o.reduce_only)
                            .fold(FixedPoint::ZERO, |acc, o| sat_add(acc, o.remaining_qty))
                    });
                    let grown = growth
                        .get(&(sender, params.market_id))
                        .copied()
                        .unwrap_or(FixedPoint::ZERO);
                    let bound = sat_add(
                        sat_add(reduce_only_allowance(pos, params.is_buy), resting),
                        grown,
                    );
                    qty = qty.min(bound);
                }
            } else if track_growth && !params.reduce_only {
                let g = growth
                    .entry((sender, params.market_id))
                    .or_insert(FixedPoint::ZERO);
                *g = sat_add(*g, params.quantity);
            }
            // `(pre-batch allowance, read lazily; quantity of the sender's
            // earlier orders on this side)`. Every order on the side, of any
            // type, uses the allowance up (conservative).
            if track_closing {
                let key = (sender, params.market_id, params.is_buy);
                let (allowance, used) = closing.entry(key).or_insert((None, FixedPoint::ZERO));
                if Self::never_rests(params) {
                    // A read error frees nothing — charges whole (conservative).
                    let allowance = *allowance.get_or_insert_with(|| {
                        Self::signed_position(&ctx.positions, &sender, params.market_id)
                            .map_or(FixedPoint::ZERO, |pos| reduce_only_allowance(pos, params.is_buy))
                    });
                    let left = (allowance - *used).max(FixedPoint::ZERO);
                    qty -= qty.min(left);
                }
                *used = sat_add(*used, params.quantity);
            }
            if price != Self::reserve_price(params) || qty != params.quantity {
                out.insert(i, (price, qty));
            }
        }
        out
    }

    /// Item 6 M1: every batch market's read-only Phase-2 inputs
    /// ([`Phase2Market`]), read once after Phase 1 and shared by the serial
    /// and sharded prepare paths (the sharded workers have no books; Phase 2
    /// touches no book, so this is exactly what Phase 3 matches against):
    /// fix A's [`book_shape`] (row 42: a missing book's market row), Option
    /// B's best bid (books are consensus state, so every node reads the same
    /// prices), the margin config and, for a market with an order that is
    /// not reduce-only, [`AccountReader::mark`]. Field borrows (`books`,
    /// `state`, the reader's configs), so it coexists with the stitch's
    /// `&mut ctx.next_global_order_id`.
    fn phase2_markets<'a, T: StateBackend>(
        books: &HashMap<MarketId, OrderBook>,
        state: &T,
        reader: &AccountReader<'a, T>,
        orders: &[(usize, Address, &PlaceOrderParams)],
    ) -> HashMap<MarketId, Phase2Market<'a>> {
        let mut out: HashMap<MarketId, Phase2Market<'a>> = HashMap::default();
        for &(_, _, p) in orders {
            out.entry(p.market_id)
                .or_insert_with(|| Phase2Market {
                    shape: Self::book_shape(books, state, p.market_id),
                    bid_floor: books.get(&p.market_id).and_then(OrderBook::best_bid),
                    cfg: reader.margin_configs.get(&p.market_id),
                    mark: None,
                    band: None,
                    orders: 0,
                })
                .orders += 1;
        }
        // s94 option 2: the band of every batch market (its mark from the
        // block table; the stale fallback reads the books after Phase 1).
        let bps = Self::price_band_bps(state);
        // Independent reads per market: iteration order is irrelevant.
        out.into_iter()
            .map(|(m, mut market)| {
                market.mark = reader.mark(m);
                market.band = Self::price_band(reader, books.get(&m), m, market.mark, bps);
                (m, market)
            })
            .collect()
    }

    /// s94 option 2: the band width in force — governance key
    /// [`torus_types::PRICE_BAND_PARAM`] in `CF_FEE_CONFIG` (default ±50%).
    fn price_band_bps<T: StateBackend>(state: &T) -> u64 {
        let stored = state
            .get_cf_raw(torus_state::cf::CF_FEE_CONFIG, torus_types::PRICE_BAND_PARAM.as_bytes())
            .ok()
            .flatten();
        torus_types::price_band_bps(stored.as_deref())
    }

    /// s94 option 2: `market_id`'s band — its usable `mark`, else the book
    /// fallback ([`band_reference`]) clamped to the last aggregated oracle
    /// price (read only when the mark is not usable); `None` when the market
    /// was never marked (no band, no option-1 charge).
    fn price_band<T: StateBackend>(
        reader: &AccountReader<'_, T>,
        book: Option<&OrderBook>,
        market_id: MarketId,
        mark: Option<FixedPoint>,
        bps: u64,
    ) -> Option<PriceBand> {
        let last = match mark {
            Some(_) => None,
            // R02: absence is "never marked"; a local fault fail-stops.
            None => match reader.oracle.get_price_opt(market_id, reader.now) {
                Ok(p) => p.map(|p| p.price),
                Err(e) => {
                    reader.note_fault("price band oracle read", &e);
                    None
                }
            },
        };
        let reference = match book {
            Some(b) => b.band_reference(mark, last),
            None => band_reference(mark, last, None, None, None),
        }?;
        Some(PriceBand { reference, bps })
    }

    /// s94 option 2: [`Self::price_band`] on the single-action path (the
    /// context's current book and mark).
    fn market_band<T: StateBackend>(ctx: &NativeExecContext<T>, market_id: MarketId) -> Option<PriceBand> {
        let reader = AccountReader::of(ctx);
        let mark = reader.mark(market_id);
        Self::price_band(&reader, ctx.order_books.get(&market_id), market_id, mark, Self::price_band_bps(&ctx.state))
    }

    /// B-blind (s92, owner decisions; replaces the s87 / s89 same-batch bid
    /// bound). After the Phase-2 fold each non-pool sell that takes a bid
    /// floor ([`PreparedOrder::top_up_candidate`]: [`takes_bid_floor`], its
    /// sender's D2 pool is another market) is topped up towards
    /// `reserve(B0 x (1 + δ), qty)`: `B0` its market's best bid at the start
    /// of Phase 2 (none: no top-up), δ = [`SELL_TOP_UP_BPS`]
    /// ([`sell_top_up_price`]), `qty` its Phase-2 reservation quantity. It
    /// reads no other trader's order of the batch, so no one can raise a
    /// reservation, or drain a sender's pool through one: griefing-free.
    ///
    /// Soft and partial: the extra comes from the sender's free margin LEFT
    /// after its whole Phase-2 fold (`available + pos_net − excess`, what
    /// Phase 3 would give its D2 pool), `min(extra, free left)`, candidates in
    /// flat batch order (a sender's earlier candidates first). Never a
    /// placement gate: every Phase-2 outcome is unchanged. It joins
    /// `margin_reserved` (the taker-only budget); the hold, resting row and
    /// every release stay at the limit (release = reserved − hold, A5
    /// exact). Deterministic: a pure function of the Phase-2 outcomes
    /// (serial == sharded) and the start-of-batch books.
    ///
    /// s89 review finding 2 (by design): a top-up is taken AHEAD of the
    /// sender's own pool-market orders, even those earlier in flat order — it
    /// comes off the free margin after the whole fold, before Phase 3 hands
    /// the rest to the pool.
    ///
    /// A full top-up moves the order's `res_price` (counters only) to the
    /// top-up price; a partial one keeps Phase 2's. Returns how many
    /// candidates that needed a top-up got it `[in full, partly, not at all]`.
    #[allow(clippy::too_many_arguments)]
    fn sell_top_ups<T: StateBackend>(
        positions: &PositionManager<T>,
        markets: &HashMap<MarketId, Phase2Market<'_>>,
        basis: &HashMap<usize, (FixedPoint, FixedPoint)>,
        pools: &HashMap<Address, (MarketId, FixedPoint)>,
        excess_by_sender: &HashMap<Address, FixedPoint>,
        market_batches: &mut HashMap<MarketId, Vec<PreparedOrder<'_>>>,
        bal_cache: &mut BalanceCache,
    ) -> [u64; 3] {
        let mut counts = [0u64; 3];
        // (flat index, market, position in its batch), in flat order.
        let mut wanted: Vec<(usize, MarketId, usize)> = market_batches
            .iter()
            .flat_map(|(&m, batch)| {
                batch.iter().enumerate().filter(|(_, p)| p.top_up_candidate).map(move |(k, p)| (p.index, m, k))
            })
            .collect();
        wanted.sort_unstable_by_key(|w| w.0);
        for (i, market_id, k) in wanted {
            let Some(market) = markets.get(&market_id) else { continue };
            let Some(price) = market.bid_floor.and_then(Self::sell_top_up_price) else { continue };
            let Some(p) = market_batches.get_mut(&market_id).and_then(|b| b.get_mut(k)) else { continue };
            let res_qty = basis.get(&i).map_or(p.params.quantity, |b| b.1);
            let Ok(target) = Self::try_reserve_for_qty_cfg(market.cfg, price, res_qty) else { continue };
            let extra = target - p.margin_reserved;
            if extra <= FixedPoint::ZERO {
                continue;
            }
            let Ok(mut bal) = bal_cache.load(positions, &p.sender) else { continue };
            let pos_net = pools.get(&p.sender).map_or(FixedPoint::ZERO, |v| v.1);
            let excess = excess_by_sender.get(&p.sender).copied().unwrap_or(FixedPoint::ZERO);
            let grant = extra.min(bal.available + pos_net - excess);
            if grant <= FixedPoint::ZERO {
                counts[2] += 1;
                continue;
            }
            bal.available -= grant;
            bal.order_margin += grant;
            bal_cache.set(&p.sender, bal);
            p.margin_reserved += grant;
            if grant == extra {
                p.res_price = price;
                counts[0] += 1;
            } else {
                counts[1] += 1;
            }
        }
        counts
    }

    /// B-blind (s92): the top-up price `B0 x (1 + SELL_TOP_UP_BPS / 10,000)`
    /// in exact integer math on the raw value, rounded down; `None` on
    /// overflow (no top-up).
    fn sell_top_up_price(b0: FixedPoint) -> Option<FixedPoint> {
        b0.raw().checked_mul(10_000 + SELL_TOP_UP_BPS).map(|r| FixedPoint::from_raw(r / 10_000))
    }

    /// F1 (s517, D2): each sender's pool taker — its FIRST checked taker of
    /// the batch in flat order — as `(sender, market, pos_net)`. Item 6 M1:
    /// Phase 2's fold now keeps it ([`SenderState::pool`]); this pass over
    /// the prepared orders stays as the tests' oracle.
    #[cfg(test)]
    fn d2_pool_takers(
        market_batches: &HashMap<MarketId, Vec<PreparedOrder<'_>>>,
    ) -> Vec<(Address, MarketId, FixedPoint)> {
        let mut checked_takers: Vec<(usize, Address, MarketId, FixedPoint)> = market_batches
            .values()
            .flatten()
            .filter_map(|p| p.checked_pos_net.map(|n| (p.index, p.sender, p.params.market_id, n)))
            .collect();
        checked_takers.sort_unstable_by_key(|c| c.0);
        let mut pooled: BTreeSet<Address> = BTreeSet::new();
        checked_takers
            .into_iter()
            .filter(|c| pooled.insert(c.1))
            .map(|(_, sender, market_id, pos_net)| (sender, market_id, pos_net))
            .collect()
    }

    /// s515 (BUG 1): Hyperliquid parity — a market order is an aggressive
    /// IOC limit, so its `price` is a REQUIRED worst-acceptable-price cap
    /// (the book never matches past it). Pre-s515 a market order reserved
    /// zero margin and matched at any price.
    /// Item 6 M1 (row 41): a `Limit` price must be positive too (the book
    /// rejects it; checked here so it is rejected before the book).
    /// Row 46 follow-up: the rule and its text are
    /// [`torus_core::order_book::order_price_violation`] (shared with the
    /// RPC intake check).
    fn validate_order_price(params: &PlaceOrderParams) -> Result<(), Rejection> {
        order_price_violation(params).map_or(Ok(()), |msg| Err((FailureReason::Price, msg)))
    }

    /// Fix A (s92): the book's dust and off-tick rejects
    /// (`OrderBook::place_order_with_accounts`, same rules, same order: dust
    /// for every order type, then the tick for a `Limit` price and, since
    /// row 45, a `StopLimit`'s limit), applied BEFORE
    /// the book so such an order takes no open-order slot, reserves nothing,
    /// gets no order id and no in-batch projection / D2 pool. `shape` = the
    /// market book's `(tick_size, lot_size)`; a market without a book uses
    /// `(ONE, ONE)`, the book placement would create. The book keeps its own
    /// checks. Item 6 M1: the rule and its text are
    /// [`torus_core::order_book::shape_violation`] (shared with the RPC
    /// intake check), which also checks a `StopLimit`'s limit (row 40).
    fn book_shape_violation(params: &PlaceOrderParams, (tick, lot): (FixedPoint, FixedPoint)) -> Option<Rejection> {
        shape_violation(params, tick, lot).map(|v| (Self::shape_reason(&v), v.placement_message()))
    }

    /// The result's reason of a tick / lot violation (placement and modify).
    fn shape_reason(v: &ShapeViolation) -> FailureReason {
        match v {
            ShapeViolation::OffTick { .. } => FailureReason::Tick,
            ShapeViolation::BelowLot { .. } => FailureReason::Lot,
        }
    }

    /// Fix A (s92): a market's `(tick_size, lot_size)` for
    /// [`book_shape_violation`] — its book's; item 6 M1 (row 42): without a
    /// book, [`market_shape`] (the tick / lot the book will be created with).
    fn book_shape<T: StateBackend>(
        books: &HashMap<MarketId, OrderBook>,
        state: &T,
        market_id: MarketId,
    ) -> (FixedPoint, FixedPoint) {
        books
            .get(&market_id)
            .map_or_else(|| Self::market_shape(state, market_id), |b| (b.tick_size, b.lot_size))
    }

    /// Item 6 M1 (row 42): the `(tick, lot)` a NEW book of `market_id` is
    /// created with — its `CF_NATIVE_MARKETS` row's
    /// ([`market_row_shape`], the row the RPC intake check reads), `(ONE,
    /// ONE)` when there is no row, it does not decode or the read fails.
    /// Every node reads the same row (governance / genesis writes only), so
    /// every path creates the same book. A book that exists keeps its own.
    fn market_shape<T: StateBackend>(state: &T, market_id: MarketId) -> (FixedPoint, FixedPoint) {
        state
            .get_cf_raw(torus_state::cf::CF_NATIVE_MARKETS, &market_id.to_be_bytes())
            .ok()
            .flatten()
            .and_then(|row| market_row_shape(&row))
            .unwrap_or((FixedPoint::ONE, FixedPoint::ONE))
    }

    /// s515 (BUG 2): signed position size (+long / -short / 0 flat).
    fn signed_position<T: StateBackend>(
        positions: &PositionManager<T>,
        trader: &Address,
        market_id: MarketId,
    ) -> Result<FixedPoint, CoreError> {
        Ok(match positions.get_position(trader, market_id)? {
            Some(p) if p.is_long => p.size,
            Some(p) => -p.size,
            None => FixedPoint::ZERO,
        })
    }

    /// s515 (BUG 2): placement check of a reduce-only order against the
    /// trader's position — the same rule the book applies at match time
    /// ([`reduce_only_allowance`]); an oversize order passes here and is
    /// clamped to the position by the book (Hyperliquid resizes).
    fn reduce_only_violation(signed_pos: FixedPoint, params: &PlaceOrderParams) -> Option<String> {
        if !params.reduce_only {
            None
        } else if signed_pos == FixedPoint::ZERO {
            Some("reduce-only order rejected: no open position".to_string())
        } else if reduce_only_allowance(signed_pos, params.is_buy) == FixedPoint::ZERO {
            Some("reduce-only order rejected: would increase position".to_string())
        } else {
            None
        }
    }

    /// s515 (BUG 2): the positions `book` must police — every trader with a
    /// resting reduce-only order there plus the given reduce-only senders.
    /// A position row that fails to read polices as flat (every node reads
    /// the same bytes, so this stays deterministic). Item 6 C7: read through
    /// `reader` (decoded records for clean traders).
    ///
    /// Item 6 M1: `known` = positions already read (the batch's Phase 2
    /// reads, [`PreparedOrder::pre_pos`]); only the other traders are read.
    fn reduce_only_positions_for<T: StateBackend>(
        reader: &AccountReader<'_, T>,
        book: &OrderBook,
        market_id: MarketId,
        ro_senders: impl Iterator<Item = Address>,
        known: ReduceOnlyPositions,
    ) -> ReduceOnlyPositions {
        let mut traders: BTreeSet<Address> = book.reduce_only_traders().into_iter().collect();
        traders.extend(ro_senders);
        let mut out = known;
        for t in traders {
            if out.get(&t).is_some() {
                continue;
            }
            let pos = match reader.get_position(&t, market_id) {
                Ok(Some(p)) if p.is_long => p.size,
                Ok(Some(p)) => -p.size,
                Ok(None) | Err(_) => FixedPoint::ZERO,
            };
            out.insert(t, pos);
        }
        out
    }

    /// Taker-side release after an order's own placement: everything
    /// reserved except the reservation still owed by what it left resting
    /// (`rested_qty`: the book remainder, a pending stop's full quantity, or
    /// zero). s515: `rested_qty` (not `quantity - filled`) keeps this exact
    /// when a reduce-only clamp shrank the order. Shared by the single-action
    /// path and both batch settle paths.
    fn taker_margin_release_cfg(
        cfg: Option<&MarketMarginConfig>,
        params: &PlaceOrderParams,
        reserved: FixedPoint,
        result: &PlaceResult,
    ) -> FixedPoint {
        if reserved <= FixedPoint::ZERO {
            return FixedPoint::ZERO;
        }
        let still_owed =
            Self::reserve_for_qty_cfg(cfg, Self::reserve_price(params), result.rested_qty);
        (reserved - still_owed).max(FixedPoint::ZERO)
    }

    /// Move up to `amount` of `trader`'s order margin back to available
    /// (clamped so legacy state can't underflow).
    fn release_order_margin<T: StateBackend>(
        ctx: &NativeExecContext<T>,
        trader: &Address,
        amount: FixedPoint,
    ) {
        if amount <= FixedPoint::ZERO {
            return;
        }
        if let Ok(mut bal) = ctx.positions.get_native_balance(trader) {
            let release = amount.min(bal.order_margin);
            bal.order_margin -= release;
            bal.available += release;
            let _ = ctx.positions.put_native_balance(trader, &bal);
        }
    }

    fn exec_place_order<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
        sender: &Address,
        params: &PlaceOrderParams,
    ) -> NativeActionResult {
        let open_at_start = Self::open_order_counts(&ctx.order_books, std::iter::once(sender));
        if let Err((reason, msg)) = Self::take_open_slot(
            &mut None,
            &ctx.positions,
            &open_at_start,
            sender,
            params,
        ) {
            if let Some(ref m) = ctx.metrics {
                reason.count(m);
            }
            return NativeActionResult::rejected("place_order", (reason.failure(), msg));
        }
        let mut triggered = VecDeque::new();
        let result = Self::place_order_inner(ctx, sender, params, None, &mut triggered, false);
        Self::run_triggered_stops(ctx, triggered);
        result
    }

    /// s515: place stops fired by earlier fills (the book hands them back
    /// instead of executing them inline), FIFO. Each one first releases its
    /// pending reservation — `reserve(price, qty)` at the triggered order's
    /// price, which IS the stop's reservation price (cap / limit, see
    /// `reserve_price`) — then goes through the normal placement path: price
    /// validation, the reduce-only check against the position AT TRIGGER
    /// TIME, a fresh margin reservation, matching, settlement. Stops it
    /// fires in turn join the queue; each stop fires at most once, so the
    /// cascade is bounded by the pending set.
    fn run_triggered_stops<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
        mut queue: VecDeque<TriggeredStop>,
    ) {
        while let Some(stop) = queue.pop_front() {
            // F2: a (legacy) row whose price × qty overflows cannot have
            // reserved anything — release nothing instead of panicking.
            let reserved = Self::try_reserve_for_qty_cfg(
                ctx.margin_configs.get(&stop.params.market_id),
                stop.params.price,
                stop.params.quantity,
            )
            .unwrap_or(FixedPoint::ZERO);
            Self::release_order_margin(ctx, &stop.trader, reserved);
            let r = Self::place_order_inner(ctx, &stop.trader, &stop.params, Some(stop.id), &mut queue, false);
            if !r.success {
                tracing::debug!(
                    stop_id = stop.id,
                    trader = %stop.trader,
                    market_id = stop.params.market_id,
                    error = ?r.error,
                    "triggered stop not placed"
                );
            }
        }
    }

    /// One PlaceOrder through the single-action path. `forced_id` places a
    /// triggered stop under its own id; stops fired by this order's fills are
    /// appended to `triggered`. `liquidation`: a stage-1 liquidation order
    /// (s94 option 1: its fills carry no mark charge — the liquidation step
    /// bounds them by its slippage cap and must not be cut).
    fn place_order_inner<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
        sender: &Address,
        params: &PlaceOrderParams,
        forced_id: Option<OrderId>,
        triggered: &mut VecDeque<TriggeredStop>,
        liquidation: bool,
    ) -> NativeActionResult {
        let market_id = params.market_id;

        // s515 (BUG 1): market / stop orders need a positive price cap.
        // Fix A (s92): the book's dust / off-tick rejects, before any
        // reservation, order id or (missing) book.
        // s515 (BUG 2): reduce-only needs a position it can reduce.
        // Item 6 M1 (row 42): a missing book is created below with `shape`.
        let mut ro_pos = None;
        let shape = Self::book_shape(&ctx.order_books, &ctx.state, market_id);
        // s94 option 2: the band (its reference is also option 1's mark).
        let band = Self::market_band(ctx, market_id);
        let pre_check = Self::validate_order_price(params)
            .err()
            .or_else(|| Self::book_shape_violation(params, shape))
            .or_else(|| {
                let v = price_band_violation(params, band?)?;
                Some((FailureReason::PriceBand, v.placement_message()))
            })
            .or_else(|| {
                if !params.reduce_only {
                    return None;
                }
                match Self::signed_position(&ctx.positions, sender, market_id) {
                    Ok(pos) => {
                        ro_pos = Some(pos);
                        Self::reduce_only_violation(pos, params).map(|m| (FailureReason::ReduceOnly, m))
                    }
                    Err(e) => Some((FailureReason::Other, e.to_string())),
                }
            });
        if let Some(rejection) = pre_check {
            // Funnel (perf A1): died pre-book on validation.
            if let Some(ref m) = ctx.metrics {
                m.orders_rejected_other.inc();
            }
            return NativeActionResult::rejected("place_order", rejection);
        }

        // FIX 2 (ECON-FIND-05): Reserve order margin before placing the order.
        // A5: reserve and every later release share reserve_for_qty_cfg.
        // s515 (BUG 1): EVERY order type reserves — a market order (it used
        // to reserve ZERO) at the mark price, the cap without one (review
        // 4); the unused part comes back below once the order no longer rests.
        // F4: a reduce-only non-stop order only for the quantity the book
        // will let it keep (the clamp to the position it reads below). F2: an
        // overflowing notional rejects.
        let mut reserve_qty = match ro_pos {
            Some(pos) if !Self::is_stop(params) => {
                params.quantity.min(reduce_only_allowance(pos, params.is_buy))
            }
            _ => params.quantity,
        };
        // Review 5 (F2): an order that never rests reserves only for what it
        // would open beyond closing the current position (a read error
        // charges it whole — conservative; the book then treats it as flat).
        if Self::never_rests(params) && ro_pos.is_none() {
            ro_pos = Self::signed_position(&ctx.positions, sender, market_id).ok();
        }
        if Self::never_rests(params) {
            if let Some(pos) = ro_pos {
                reserve_qty -= reserve_qty.min(reduce_only_allowance(pos, params.is_buy));
            }
        }
        let mark = if matches!(params.order_type, OrderType::Market) {
            Self::mark_price(ctx, market_id)
        } else {
            None
        };
        let res_price = Self::reservation_price(params, mark);
        let order_margin_required = match Self::try_reserve_for_qty_cfg(
            ctx.margin_configs.get(&market_id),
            res_price,
            reserve_qty,
        ) {
            Ok(m) => m,
            Err(msg) => {
                if let Some(ref m) = ctx.metrics {
                    m.orders_rejected_other.inc();
                }
                return NativeActionResult::rejected("place_order", (FailureReason::Price, msg));
            }
        };

        // s515 review 4: a checked taker's match-time budget — its
        // reservation (F1: + the sender's free margin after it, installed
        // in the book as `AccountMargins` below).
        let checked = Self::match_margin_checked(params);
        let mut margin_budget = None;
        // F1 (s517): read from FIELDS — the book below borrows
        // `ctx.order_books` mutably.
        let reader = AccountReader {
            positions: &ctx.positions,
            oracle: &ctx.oracle,
            now: ctx.timestamp,
            margin_configs: &ctx.margin_configs,
            marks: ctx.block_marks.as_ref(),
            sums: ctx.sums.as_ref(),
            batch: None,
            dense_tiers: None,
            drain_dirty: None,
            fault: &ctx.reader_fault,
        };
        let needs_account = !params.reduce_only;
        let mut account = None;
        if order_margin_required > FixedPoint::ZERO || checked || needs_account {
            match ctx.positions.get_native_balance(sender) {
                Ok(mut bal) => {
                    // F1 (s517, D1 strict HL): the account-level check is
                    // the ONLY placement gate — the old `available >=
                    // reservation` rejection is gone, so the debit below may
                    // take `available` negative (UPnL funds it).
                    if needs_account {
                        let acct = reader
                            .position_px(sender, market_id)
                            .and_then(|(s, px)| Ok((s, px, reader.pos_net(sender)?)));
                        let (signed, px, pos_net) = match acct {
                            Ok(a) => a,
                            Err(e) => {
                                // Funnel (perf A1): died pre-book on a state read error.
                                if let Some(ref m) = ctx.metrics {
                                    m.orders_rejected_other.inc();
                                }
                                return NativeActionResult::err("place_order", e.to_string());
                            }
                        };
                        let free = bal.available + pos_net;
                        if let Err(rejection) = Self::account_check(
                            reader.tiers(market_id),
                            signed,
                            px,
                            params,
                            res_price,
                            free,
                        ) {
                            // Funnel (perf A1): died pre-book on the margin check.
                            if let Some(ref m) = ctx.metrics {
                                m.orders_rejected_margin.inc();
                            }
                            return NativeActionResult::rejected("place_order", rejection);
                        }
                        account = Some((free, px));
                    }
                    if checked {
                        margin_budget = Some(order_margin_required);
                    }
                    if order_margin_required > FixedPoint::ZERO {
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
            .or_insert_with(|| OrderBook::new(market_id, shape.0, shape.1));

        // s515 (BUG 2): police reduce-only orders against CURRENT positions.
        // Review 5: a checked taker's position too — its closing fills are
        // free at match time (same source and freshness as the policing).
        let track_sender = params.reduce_only || checked;
        if track_sender || book.has_reduce_only_orders() {
            // The sender's position read above is reused (only the balance
            // was written since); a failed / skipped read reads it here.
            let reread = track_sender && ro_pos.is_none();
            let mut ro = Self::reduce_only_positions_for(
                &reader,
                book,
                market_id,
                reread.then_some(*sender).into_iter(),
                ReduceOnlyPositions::new(),
            );
            if let (true, Some(pos)) = (track_sender, ro_pos) {
                ro.insert(*sender, pos);
            }
            book.set_reduce_only_positions(ro);
        }

        // FIX 6 (ECON-FIND-09): Sync global order ID counter to prevent cross-market collisions.
        // s515: a triggered stop keeps its own (already allocated) id.
        book.set_next_order_id(forced_id.unwrap_or(ctx.next_global_order_id));
        let margin_limit = margin_budget.map(|budget| {
            Self::taker_margin_limit(
                &Self::margin_tiers(ctx.margin_configs.get(&market_id)),
                params,
                budget,
            )
        });
        // F1 (s517): the sender's free margin after this reservation, for
        // the match-time check.
        let mut am = AccountMargins::new(Self::margin_tiers(ctx.margin_configs.get(&market_id)));
        if let (true, Some((free, px))) = (checked, account) {
            am.insert(*sender, free - order_margin_required, px);
        }
        // s94 option 1: every fill's loss against the mark is charged.
        if !liquidation {
            am.set_mark(band.map(|b| b.reference));
        }
        book.set_account_margins(am);
        // F1 (s517 #4): makers are checked on every fill (HL marginCanceled).
        let result = book.place_order_with_accounts(
            params.clone(),
            *sender,
            ctx.timestamp,
            margin_limit.as_ref(),
            Some(&reader),
        );
        // P2-1: the rest of the order / its stop feeds the cancel-all index.
        TraderMarkets::absorb(&mut ctx.trader_markets, book, market_id);
        book.clear_reduce_only_positions();
        // Plan 9.11 (telemetry): this placement's charged maker fills.
        if let Some(m) = ctx.metrics.as_deref() {
            m.maker_offmark_charged_fills.inc_by(book.account_margins().charged_maker_fills());
        }
        book.clear_account_margins();
        if forced_id.is_some() {
            book.set_next_order_id(ctx.next_global_order_id);
        } else {
            ctx.next_global_order_id = book.next_order_id();
        }
        triggered.extend(result.triggered_stops.iter().cloned());

        // Funnel (perf A1): STP maker cancels already happened in the book,
        // regardless of how settlement below turns out.
        if let Some(ref m) = ctx.metrics {
            m.orders_self_trade_cancels
                .inc_by(result.self_trade_cancels.len() as u64);
            // s92: the single path's budget is the account (never non-pool).
            Self::record_margin_cuts(m, params.is_buy, &result, res_price, book.tick_size, false);
        }

        // FIX 2: Release margin for the filled / cancelled portion; keep the
        // reservation of what rests (A5 telescoping — see
        // taker_margin_release_cfg).
        let margin_to_release = Self::taker_margin_release_cfg(
            ctx.margin_configs.get(&market_id),
            params,
            order_margin_required,
            &result,
        );
        Self::release_order_margin(ctx, sender, margin_to_release);

        // A5 (maker-fill margin leak): release maker-side order margin consumed
        // by this order's fills, and the full remaining reservation of makers
        // STP-cancelled during matching — mirrors execute_batch Phase 4.
        // s515: plus the reduce-only cuts.
        for (trader, amount) in
            Self::maker_margin_releases(ctx, market_id, std::iter::once(&result))
        {
            Self::release_order_margin(ctx, &trader, amount);
        }

        // Apply fills to position manager.
        // FIX 22 (ECON-FIND-23): Propagate fill errors instead of discarding them.
        // s80: effects are kept only while a stream wants fills.
        let record_fills = ctx.record_fills;
        if record_fills {
            ctx.fill_effects_scratch.clear();
        }
        for fill in &result.fills {
            let taker_is_buy = fill.maker_side != Side::Buy;
            let notional = fill.price * fill.quantity;
            let taker = match ctx
                .positions
                .apply_fill(
                    &fill.taker,
                    market_id,
                    taker_is_buy,
                    fill.quantity,
                    fill.price,
                    MarginType::Cross,
                )
                .and_then(|effect| {
                    Self::add_cum_volume(&ctx.positions, &fill.taker, notional)?;
                    Ok(effect)
                }) {
                Ok(effect) => effect,
                Err(e) => {
                    // Funnel (perf A1): died on fill application, not on the book.
                    if let Some(ref m) = ctx.metrics {
                        m.orders_rejected_other.inc();
                    }
                    return NativeActionResult::rejected(
                        "place_order",
                        (FailureReason::Fill, format!("taker fill failed: {e}")),
                    );
                }
            };
            let maker = match ctx
                .positions
                .apply_fill(
                    &fill.maker,
                    market_id,
                    fill.maker_side == Side::Buy,
                    fill.quantity,
                    fill.price,
                    MarginType::Cross,
                )
                .and_then(|effect| {
                    Self::add_cum_volume(&ctx.positions, &fill.maker, notional)?;
                    Ok(effect)
                }) {
                Ok(effect) => effect,
                Err(e) => {
                    // Funnel (perf A1): died on fill application, not on the book.
                    if let Some(ref m) = ctx.metrics {
                        m.orders_rejected_other.inc();
                    }
                    return NativeActionResult::rejected(
                        "place_order",
                        (FailureReason::Fill, format!("maker fill failed: {e}")),
                    );
                }
            };
            if record_fills {
                ctx.fill_effects_scratch.push([taker, maker]);
            }
        }

        // Persist trades to CF_NATIVE_TRADES and CF_NATIVE_USER_TRADES.
        // These CFs are NOT in the state root, so writes cannot affect consensus.
        if record_fills {
            for (i, fill) in result.fills.iter().enumerate() {
                let [taker, maker] = ctx.fill_effects_scratch[i];
                Self::persist_trade_with_extras(ctx, market_id, fill, taker, maker);
            }
        } else {
            for fill in &result.fills {
                Self::persist_trade(ctx, market_id, fill);
            }
        }

        if let Some(ref m) = ctx.metrics {
            m.orders_matched.inc_by(result.fills.len() as u64);
            Self::record_order_status_funnel(m, &result.status, result.fills.len());
        }

        NativeActionResult::placed(&result)
    }

    /// Record one fill for this block's trade-history rows (written after the
    /// batch, see `defer_trades`) and advance the per-block fill counter.
    /// While `record_fills` is set, callers use `persist_trade_with_extras`.
    fn persist_trade<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
        market_id: MarketId,
        fill: &torus_core::order_book::Fill,
    ) {
        if ctx.trade_history || ctx.record_fills {
            if !ctx.defer_trades && ctx.fills_block != ctx.block_height {
                // Inline mode, context reused for a new block: the earlier
                // block's rows are already written.
                ctx.pending_fills.clear();
                ctx.pending_extras.clear();
                ctx.inline_fills_written = 0;
            }
            ctx.fills_block = ctx.block_height;
            ctx.pending_fills.push(TradeFill {
                trade_index: ctx.trade_index,
                market: market_id,
                maker: fill.maker,
                taker: fill.taker,
                price_raw: fill.price.raw(),
                qty_raw: fill.quantity.raw(),
                taker_side: if fill.maker_side == Side::Buy { 1 } else { 0 },
            });
        }
        ctx.trade_index += 1;
    }

    /// s80, only while `record_fills`: `persist_trade` plus the fill's
    /// stream-only extras (`taker` / `maker` are its position effects), so
    /// `pending_extras` stays index-aligned with `pending_fills`.
    fn persist_trade_with_extras<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
        market_id: MarketId,
        fill: &torus_core::order_book::Fill,
        taker: FillEffect,
        maker: FillEffect,
    ) {
        debug_assert!(ctx.record_fills, "extras are recorded only for a stream");
        Self::persist_trade(ctx, market_id, fill);
        ctx.pending_extras.push(FillExtras {
            maker_order_id: fill.maker_order_id,
            taker_order_id: fill.taker_order_id,
            maker_start_raw: maker.start_size.raw(),
            taker_start_raw: taker.start_size.raw(),
            maker_pnl_raw: maker.closed_pnl.map_or(0, |p| p.raw()),
            taker_pnl_raw: taker.closed_pnl.map_or(0, |p| p.raw()),
        });
        debug_assert_eq!(ctx.pending_extras.len(), ctx.pending_fills.len());
    }

    /// FIX CONS-FIND-30: Ownership check added -- only the order's trader can cancel.
    fn exec_cancel_order<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
        sender: &Address,
        order_id: u128,
    ) -> NativeActionResult {
        ctx.phase_accum.by_id_actions += 1;
        // Check ownership before cancelling (cheaper than cancel + re-insert).
        for book in ctx.order_books.values() {
            ctx.phase_accum.by_id_books_probed += 1;
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
            ctx.phase_accum.by_id_books_probed += 1;
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
                let margin_to_release = Self::margin_at_integer_leverage(notional, max_lev);

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
        // FIX 2 (ECON-FIND-05) + C4 (s517): release what the cancelled orders
        // AND pending stops reserved.
        let total_margin_release = Self::cancel_orders_and_stops(ctx, sender, market_id);
        Self::release_order_margin(ctx, sender, total_margin_release);
        NativeActionResult::ok("cancel_all", 500)
    }

    /// C4 (s517): cancel `trader`'s resting orders AND pending stops in
    /// `market` (`None` = every market, ascending id) and return the margin
    /// they reserved — orders at `price × remaining` (FIX 2), stops at
    /// [`Self::stop_reservation`]. A market that lost anything is dirty. The
    /// caller releases the sum (`min(order_margin)`). Shared by the user
    /// `CancelAll` and the liquidation step. Item 6 Phase 2 P2-1: with
    /// resident books, only the markets the cancel-all index lists for
    /// `trader` are visited (in ascending id, as the full scan); the others
    /// hold nothing of the trader's, so the scan changed nothing there. The
    /// visited markets leave the index. Without resident books every book is
    /// visited (row 23: an index would be rebuilt in every block).
    fn cancel_orders_and_stops<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
        trader: &Address,
        market: Option<MarketId>,
    ) -> FixedPoint {
        #[cfg(any(test, feature = "test-reference-paths"))]
        if ctx.test_cancel_all_full_scan {
            return reference_paths::cancel_orders_and_stops_full_scan(ctx, trader, market);
        }
        let market_ids: Vec<MarketId> = if ctx.resident {
            let index = ctx.cancel_index();
            match market {
                Some(m) if index.remove(trader, m) => vec![m],
                Some(_) => Vec::new(),
                None => index.take(trader),
            }
        } else {
            match market {
                Some(m) => vec![m],
                None => {
                    let mut v: Vec<MarketId> = ctx.order_books.keys().copied().collect();
                    v.sort_unstable();
                    v
                }
            }
        };
        let mut total = FixedPoint::ZERO;
        ctx.phase_accum.cancel_alls += 1;
        for mid in market_ids {
            let Some(book) = ctx.order_books.get_mut(&mid) else { continue };
            ctx.phase_accum.cancel_all_books_visited += 1;
            let stops = book.take_pending_stops(trader);
            let cancelled = book.cancel_all(*trader, market);
            if stops.is_empty() && cancelled.is_empty() {
                continue;
            }
            ctx.phase_accum.cancel_all_books_hit += 1;
            ctx.dirty_books.insert(mid);
            let cfg = ctx.margin_configs.get(&mid);
            total += Self::cancelled_orders_margin(cfg, &cancelled);
            for &(price, qty) in &stops {
                total += Self::stop_reservation(cfg, price, qty);
            }
        }
        total
    }

    /// FIX 2 (ECON-FIND-05): the margin `cancelled` resting orders reserved.
    fn cancelled_orders_margin(
        cfg: Option<&MarketMarginConfig>,
        cancelled: &[torus_core::order_book::Order],
    ) -> FixedPoint {
        let mut total = FixedPoint::ZERO;
        for order in cancelled {
            let notional = order.price * order.remaining_qty;
            let max_lev = cfg
                .map(|c| effective_max_leverage(&c.tiers, notional))
                .unwrap_or(20);
            total += Self::margin_at_integer_leverage(notional, max_lev);
        }
        total
    }

    /// C4 (s517): a pending stop's reservation at its `(price, qty)` from
    /// `take_pending_stops` — exactly what [`Self::run_triggered_stops`]
    /// releases when it fires; an overflowing legacy row reserved nothing.
    fn stop_reservation(
        cfg: Option<&MarketMarginConfig>,
        price: FixedPoint,
        qty: FixedPoint,
    ) -> FixedPoint {
        Self::try_reserve_for_qty_cfg(cfg, price, qty).unwrap_or(FixedPoint::ZERO)
    }

    /// s63: a run of consecutive `CancelAllOrders` (`run` = flat index,
    /// sender, market), state-equivalent to `exec_cancel_all` per action in
    /// run order. Book work goes first: one `OrderBook::cancel_all_many` per
    /// market over the run's senders targeting it, in run order (a repeated
    /// sender gets the empty result the sequential second call gets; P2-1:
    /// only the markets the cancel-all index lists for a sender). Then,
    /// per action in run order, exactly `exec_cancel_all`'s bookkeeping:
    /// dirty marks, the margin sum over the same market and order sequence,
    /// and one balance release. Book removal never reads balances and a
    /// release never touches a book, so hoisting the book work changes no
    /// value and no write sequence.
    fn exec_cancel_all_run<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
        run: &[(usize, Address, Option<MarketId>)],
    ) -> Vec<NativeActionResult> {
        if let [(_, sender, market_id)] = run {
            return vec![Self::exec_cancel_all(ctx, sender, *market_id)];
        }
        // Step 0.4 reference: one action at a time, each a full scan.
        #[cfg(any(test, feature = "test-reference-paths"))]
        if ctx.test_cancel_all_full_scan {
            return run
                .iter()
                .map(|(_, sender, m)| Self::exec_cancel_all(ctx, sender, *m))
                .collect();
        }
        if !ctx.resident {
            return Self::exec_cancel_all_run_every_book(ctx, run);
        }
        // P2-1: the markets each action visits — its sender's markets in the
        // cancel-all index (`None`), or its target if the index lists it —
        // taken out of the index (after the run the sender has nothing left
        // there). A repeated sender finds its markets taken: it visits none,
        // as its sequential second call finds nothing. Other markets hold
        // nothing of the sender's, so skipping them changes no book, result
        // or sum. Sorted: markets ascending, actions in run order within one.
        let index = ctx.cancel_index();
        let mut visits: Vec<(MarketId, usize)> = Vec::new();
        for (k, &(_, sender, target)) in run.iter().enumerate() {
            match target {
                Some(m) => {
                    if index.remove(&sender, m) {
                        visits.push((m, k));
                    }
                }
                None => visits.extend(index.take(&sender).into_iter().map(|m| (m, k))),
            }
        }
        visits.sort_unstable();
        // (action, market, its cancelled orders, its stops' reservations) for
        // every visit that removed something, markets ascending.
        let mut hits: Vec<(
            usize,
            MarketId,
            Vec<torus_core::order_book::Order>,
            FixedPoint,
        )> = Vec::new();
        let mut senders: Vec<Address> = Vec::with_capacity(run.len());
        let mut stops_k: Vec<(bool, FixedPoint)> = Vec::with_capacity(run.len());
        for group in visits.chunk_by(|a, b| a.0 == b.0) {
            let mid = group[0].0;
            let Some(book) = ctx.order_books.get_mut(&mid) else {
                continue;
            };
            ctx.phase_accum.cancel_all_books_visited += group.len() as u64;
            let cfg = ctx.margin_configs.get(&mid);
            senders.clear();
            stops_k.clear();
            // C4: each member's stops first, in run order.
            for &(_, k) in group {
                let sender = run[k].1;
                let mut took = (false, FixedPoint::ZERO);
                for (price, qty) in book.take_pending_stops(&sender) {
                    took.0 = true;
                    took.1 += Self::stop_reservation(cfg, price, qty);
                }
                stops_k.push(took);
                senders.push(sender);
            }
            let cancelled = book.cancel_all_many(&senders);
            for ((&(_, k), &(took_stops, stops)), orders) in
                group.iter().zip(&stops_k).zip(cancelled)
            {
                if took_stops || !orders.is_empty() {
                    hits.push((k, mid, orders, stops));
                }
            }
        }
        // Per action in run order, its markets ascending (stable sort).
        hits.sort_by_key(|h| h.0);

        let mut results = Vec::with_capacity(run.len());
        ctx.phase_accum.cancel_alls += run.len() as u64;
        let mut hits = hits.into_iter().peekable();
        for (k, (_, sender, _)) in run.iter().enumerate() {
            // FIX 2 (ECON-FIND-05): same release as `exec_cancel_all`, summed
            // in ascending market order like it (P2-1; was the book map's
            // order: the terms are non-negative, so the sum and whether the
            // `checked_add` overflows do not depend on the order).
            let mut total_margin_release = FixedPoint::ZERO;
            while let Some((_, mid, orders, stops)) = hits.next_if(|h| h.0 == k) {
                ctx.phase_accum.cancel_all_books_hit += 1;
                ctx.dirty_books.insert(mid);
                let cfg = ctx.margin_configs.get(&mid);
                total_margin_release += Self::cancelled_orders_margin(cfg, &orders);
                total_margin_release += stops;
            }
            Self::release_order_margin(ctx, sender, total_margin_release);
            results.push(NativeActionResult::ok("cancel_all", 500));
        }
        results
    }

    /// Row 23 (18c s104): [`Self::exec_cancel_all_run`] without resident
    /// books, as before P2-1: every book, one `cancel_all_many` per book over
    /// the run's senders targeting it. Without resident books every block
    /// loads its books, so P2-1's index would be rebuilt in every block with
    /// a cancel-all.
    fn exec_cancel_all_run_every_book<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
        run: &[(usize, Address, Option<MarketId>)],
    ) -> Vec<NativeActionResult> {
        // The order `exec_cancel_all` iterates for `None`. Cancels never add
        // or remove books, so every action of the run would see this order.
        let market_ids: Vec<MarketId> = ctx.order_books.keys().copied().collect();
        // cancelled[m][k]: the orders action k removed from market_ids[m].
        let mut cancelled: Vec<Vec<Vec<torus_core::order_book::Order>>> =
            Vec::with_capacity(market_ids.len());
        // C4 (s517): (took a stop?, their reservations) of action k in market_ids[m].
        let mut stop_release: Vec<Vec<(bool, FixedPoint)>> = Vec::with_capacity(market_ids.len());
        let mut members: Vec<usize> = Vec::with_capacity(run.len());
        let mut senders: Vec<Address> = Vec::with_capacity(run.len());
        for mid in &market_ids {
            members.clear();
            senders.clear();
            for (k, &(_, sender, target)) in run.iter().enumerate() {
                if target.is_none_or(|t| t == *mid) {
                    members.push(k);
                    senders.push(sender);
                }
            }
            let mut per_action = vec![Vec::new(); run.len()];
            let mut stops_k = vec![(false, FixedPoint::ZERO); run.len()];
            ctx.phase_accum.cancel_all_books_visited += members.len() as u64;
            if !senders.is_empty() {
                let cfg = ctx.margin_configs.get(mid);
                let book = ctx.order_books.get_mut(mid).expect("key just listed");
                // C4: each member's stops first, in run order (a repeated
                // sender finds none, as its sequential second call would).
                for &k in &members {
                    for (price, qty) in book.take_pending_stops(&run[k].1) {
                        stops_k[k].0 = true;
                        stops_k[k].1 += Self::stop_reservation(cfg, price, qty);
                    }
                }
                for (&k, orders) in members.iter().zip(book.cancel_all_many(&senders)) {
                    per_action[k] = orders;
                }
            }
            cancelled.push(per_action);
            stop_release.push(stops_k);
        }

        let mut results = Vec::with_capacity(run.len());
        ctx.phase_accum.cancel_alls += run.len() as u64;
        for (k, (_, sender, _)) in run.iter().enumerate() {
            // FIX 2 (ECON-FIND-05): same release as `exec_cancel_all`.
            let mut total_margin_release = FixedPoint::ZERO;
            for (m, mid) in market_ids.iter().enumerate() {
                let orders = &cancelled[m][k];
                let (took_stops, stops) = stop_release[m][k];
                if orders.is_empty() && !took_stops {
                    continue;
                }
                ctx.phase_accum.cancel_all_books_hit += 1;
                ctx.dirty_books.insert(*mid);
                let cfg = ctx.margin_configs.get(mid);
                total_margin_release += Self::cancelled_orders_margin(cfg, orders);
                total_margin_release += stops;
            }
            Self::release_order_margin(ctx, sender, total_margin_release);
            results.push(NativeActionResult::ok("cancel_all", 500));
        }
        results
    }

    /// Dividing raw notional by positive integer leverage
    /// exactly cancels SCALE in the general fixed-point division. Signed
    /// truncation is unchanged for every i128; a u32 divisor cannot be -1,
    /// so checked_div can fail only at zero. Preserve the original panic text
    /// and keep this call after each order's multiplication and tier lookup.
    fn margin_at_integer_leverage(notional: FixedPoint, leverage: u32) -> FixedPoint {
        notional
            .raw()
            .checked_div(i128::from(leverage))
            .map(FixedPoint::from_raw)
            .ok_or(torus_types::ArithmeticError::DivisionByZero)
            .expect("FixedPoint division error")
    }

    /// F3 (s515 review): cancel-and-replace of the SENDER's resting order
    /// (Hyperliquid modify = cancel + new order), validated like placement
    /// BEFORE anything is touched:
    /// - ownership (like `exec_cancel_order`);
    /// - price > 0, a NEW price on the book's tick; quantity > 0 and (when
    ///   changed) >= lot — before the reduce-only clamp, as in placement;
    /// - no crossing: the replacement is re-inserted, never matched, so a
    ///   price at / through the opposite best is rejected (the client cancels
    ///   and places instead) rather than resting a crossed book;
    /// - reduce-only: clamped to the position (placement's allowance), and
    ///   rejected when there is nothing left to reduce;
    /// - margin with placement's formula (`try_reserve_for_qty_cfg`: tiered
    ///   leverage, overflow rejects): the order's outstanding reservation is
    ///   `reserve(price, remaining)` (A5 telescoping), so the delta to
    ///   `reserve(new price, new qty)` is reserved (insufficient ⇒ rejected,
    ///   book and balances untouched) or released exactly — no drift.
    fn exec_modify_order<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
        sender: &Address,
        order_id: u128,
        new_price: Option<FixedPoint>,
        new_qty: Option<FixedPoint>,
    ) -> NativeActionResult {
        let err = |msg: String| NativeActionResult::err("modify_order", msg);
        let rejected = |reason, msg: String| NativeActionResult::rejected("modify_order", (reason, msg));
        if new_price.is_none() && new_qty.is_none() {
            return err("nothing to modify: no new price or quantity".to_string());
        }
        ctx.phase_accum.by_id_actions += 1;
        // Order ids are global, so at most one book holds it.
        let probed = &mut ctx.phase_accum.by_id_books_probed;
        let Some((market_id, old)) = ctx.order_books.iter().find_map(|(mid, b)| {
            *probed += 1;
            b.get_order(order_id).map(|o| (*mid, o.clone()))
        }) else {
            return err(format!("order {order_id} not found"));
        };
        if old.trader != *sender {
            return err(format!(
                "order {order_id} belongs to {}, not sender {sender}",
                old.trader
            ));
        }
        let book = &ctx.order_books[&market_id];
        let is_buy = old.side == Side::Buy;

        let price = new_price.unwrap_or(old.price);
        if price <= FixedPoint::ZERO {
            return rejected(FailureReason::Price, format!("modify rejected: price must be positive, got {price}"));
        }
        // s515 review 3: only a NEW price is tick-checked — an order resting
        // off the current tick (tick changed, legacy row) can still be resized.
        if price != old.price
            && book.tick_size > FixedPoint::ZERO
            && price.raw() % book.tick_size.raw() != 0
        {
            let v = ShapeViolation::OffTick { price, tick: book.tick_size };
            return rejected(Self::shape_reason(&v), format!("modify rejected: {v}"));
        }
        // s94 option 2: a NEW price must lie in the band too.
        if price != old.price {
            let probe = PlaceOrderParams {
                market_id,
                is_buy,
                price,
                quantity: old.remaining_qty,
                order_type: OrderType::Limit,
                time_in_force: old.time_in_force,
                reduce_only: old.reduce_only,
                client_order_id: None,
            };
            if let Some(v) = Self::market_band(ctx, market_id).and_then(|b| price_band_violation(&probe, b)) {
                return rejected(FailureReason::PriceBand, format!("modify rejected: {v}"));
            }
        }
        let book = &ctx.order_books[&market_id];
        let mut qty = new_qty.unwrap_or(old.remaining_qty);
        if qty <= FixedPoint::ZERO {
            return rejected(FailureReason::Lot, format!("modify rejected: quantity must be positive, got {qty}"));
        }
        // Placement's convention (s515 review 3): the lot applies to the
        // REQUESTED quantity; the reduce-only clamp below may land under it
        // (it closes the position exactly — placement rests such an order too).
        if new_qty.is_some() && qty < book.lot_size {
            let v = ShapeViolation::BelowLot { quantity: qty, lot: book.lot_size };
            return rejected(Self::shape_reason(&v), format!("modify rejected: {v}"));
        }
        let crosses = if is_buy {
            book.best_ask().is_some_and(|ask| price >= ask)
        } else {
            book.best_bid().is_some_and(|bid| price <= bid)
        };
        if crosses {
            return err(format!(
                "modify rejected: price {price} would cross the book (a modify never matches; \
                 cancel and place a new order instead)"
            ));
        }
        if old.reduce_only {
            let pos = match Self::signed_position(&ctx.positions, sender, market_id) {
                Ok(pos) => pos,
                Err(e) => return err(e.to_string()),
            };
            let allowance = reduce_only_allowance(pos, is_buy);
            if allowance == FixedPoint::ZERO {
                return rejected(
                    FailureReason::ReduceOnly,
                    "reduce-only order rejected: no position to reduce".to_string(),
                );
            }
            qty = qty.min(allowance);
        }
        if price == old.price && qty == old.remaining_qty {
            return NativeActionResult::ok("modify_order", 800);
        }

        let cfg = ctx.margin_configs.get(&market_id);
        // Placed through try_reserve_for_qty_cfg, so it cannot overflow; a
        // legacy row that would cannot have reserved anything.
        let old_reserved = Self::try_reserve_for_qty_cfg(cfg, old.price, old.remaining_qty)
            .unwrap_or(FixedPoint::ZERO);
        let new_reserved = match Self::try_reserve_for_qty_cfg(cfg, price, qty) {
            Ok(m) => m,
            Err(msg) => return rejected(FailureReason::Price, msg),
        };
        let extra = new_reserved - old_reserved;
        // F1 (s517, D1 strict HL) + review fixes 3/5: THE modify gate — the
        // new order's POSITION-tier need (`placement_need`, as at placement;
        // closing is free) minus what the old order gives back (the larger
        // of its need and its reservation) must fit the account's free
        // margin (UPnL counts). No `available >= extra` gate: the debit below may
        // take `available` negative. Reduce-only orders only reduce.
        if !old.reduce_only {
            let reader = AccountReader::of(ctx);
            let acct = reader
                .position_px(sender, market_id)
                .and_then(|(s, px)| Ok((s, px, reader.pos_net(sender)?)))
                .and_then(|a| Ok((a, ctx.positions.get_native_balance(sender)?)));
            let ((signed, px, pos_net), bal) = match acct {
                Ok(a) => a,
                Err(e) => return err(e.to_string()),
            };
            let px = if px > FixedPoint::ZERO { px } else { price };
            let tiers = cfg.map(|c| c.tiers.as_slice());
            let need = |q, p| placement_need(tiers, signed, px, is_buy, q, p, true);
            let Some(need_new) = need(qty, price) else {
                return rejected(
                    FailureReason::Price,
                    format!("order notional overflows: price {price} x quantity {qty}"),
                );
            };
            // Review fix 5 (s517): cancel-and-replace equivalence — the old
            // order gives back its whole reservation (>= its need when it
            // has a closing part, D4); an overflowing old need falls back to
            // it too (a shrink is never rejected).
            let old_cost = need(old.remaining_qty, old.price)
                .map_or(old_reserved, |n| n.max(old_reserved));
            let delta = need_new - old_cost;
            let free = bal.available + pos_net;
            if need_new > FixedPoint::ZERO && delta > FixedPoint::ZERO && delta > free {
                return rejected(
                    FailureReason::Margin,
                    format!("insufficient margin for modify: need {delta}, have {free} (account)"),
                );
            }
        }
        if extra > FixedPoint::ZERO {
            let mut bal = match ctx.positions.get_native_balance(sender) {
                Ok(bal) => bal,
                Err(e) => return err(e.to_string()),
            };
            bal.available -= extra;
            bal.order_margin += extra;
            if let Err(e) = ctx.positions.put_native_balance(sender, &bal) {
                return err(e.to_string());
            }
        }

        let book = ctx
            .order_books
            .get_mut(&market_id)
            .expect("market found above");
        // Unchanged fields stay `None`: a qty-only decrease keeps time priority.
        let modified = book.modify_order(
            order_id,
            (price != old.price).then_some(price),
            (qty != old.remaining_qty).then_some(qty),
        );
        // P2-1: a re-inserted order feeds the index (its market is listed
        // already: the old order rested there).
        TraderMarkets::absorb(&mut ctx.trader_markets, book, market_id);
        if let Err(e) = modified {
            // Unreachable (the order was found above) — undo the reserve.
            Self::release_order_margin(ctx, sender, extra);
            return err(e.to_string());
        }
        ctx.dirty_books.insert(market_id);
        Self::release_order_margin(ctx, sender, -extra);
        NativeActionResult::ok("modify_order", 800)
    }

    /// Row 74: latch the fail-stop (`fatal_error`) when `e` is a LOCAL fault
    /// (`EconomicsError::is_local_fault`: storage failed or stored bytes do
    /// not decode). Nothing rolls back inside a block, so the writes made
    /// before the error sit in the block's overlay; the committer then skips
    /// the flush and halts. Errors every validator hits alike never halt.
    /// The first latched fault is kept.
    fn latch_local_fault<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
        step: &str,
        e: &torus_economics::EconomicsError,
    ) {
        if e.is_local_fault() && ctx.fatal_error.is_none() {
            ctx.fatal_error = Some(format!("{step}: {e}"));
        }
    }

    /// An economics error as the action's error result (unchanged), after
    /// [`Self::latch_local_fault`].
    fn econ_err<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
        action_type: &'static str,
        e: torus_economics::EconomicsError,
    ) -> NativeActionResult {
        Self::latch_local_fault(ctx, action_type, &e);
        NativeActionResult::err(action_type, e.to_string())
    }

    /// R02: `latch_local_fault` for a [`CoreError`]: latch the fail-stop
    /// (`fatal_error`) when `err` is a local fault
    /// (`CoreError::is_local_fault`). The first latched fault is kept.
    pub fn latch_core_fault<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
        step: &str,
        err: &CoreError,
    ) {
        if err.is_local_fault() && ctx.fatal_error.is_none() {
            ctx.fatal_error = Some(format!("{step}: {err}"));
        }
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
            Err(e) => Self::econ_err(ctx, "delegate", e),
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
            Err(e) => Self::econ_err(ctx, "undelegate", e),
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
            Err(e) => Self::econ_err(ctx, "permanent_stake", e),
        }
    }

    fn exec_claim_rewards<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
        sender: &Address,
    ) -> NativeActionResult {
        match ctx.staking.claim_rewards(*sender) {
            Ok(_) => NativeActionResult::ok("claim_rewards", 1500),
            Err(e) => Self::econ_err(ctx, "claim_rewards", e),
        }
    }

    /// Release every matured unbonding entry across all of the sender's
    /// delegations. Errors (no state change) if nothing has matured.
    fn exec_claim_unbonded<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
        sender: &Address,
    ) -> NativeActionResult {
        match ctx.staking.claim_unbonded(*sender, ctx.block_height) {
            Ok(_) => NativeActionResult::ok("claim_unbonded", 2000),
            Err(e) => Self::econ_err(ctx, "claim_unbonded", e),
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
            Err(e) => Self::econ_err(ctx, "jail_vote", e),
        }
    }

    /// Running state hash: validator-only attestation (see
    /// `StakingManager::record_state_hash_attestation`).
    fn exec_attest_state_hash<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
        sender: &Address,
        height: u64,
        hash: &torus_types::B256,
    ) -> NativeActionResult {
        match ctx
            .staking
            .record_state_hash_attestation(*sender, height, hash.0, ctx.block_height)
        {
            Ok(_) => NativeActionResult::ok("attest_state_hash", 2000),
            Err(e) => Self::econ_err(ctx, "attest_state_hash", e),
        }
    }

    fn exec_unjail_self<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
        sender: &Address,
    ) -> NativeActionResult {
        match ctx.staking.unjail(sender, ctx.block_height) {
            Ok(()) => NativeActionResult::ok("unjail_self", 2000),
            Err(e) => Self::econ_err(ctx, "unjail_self", e),
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
            Err(e) => return Self::econ_err(ctx, "register_validator", e),
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
            Err(e) => {
                let msg = e.to_string();
                Self::latch_local_fault(ctx, "register_validator", &e.into());
                return NativeActionResult::err("register_validator", msg);
            }
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
                    Self::latch_local_fault(ctx, "register_validator", &e);
                    return NativeActionResult::err(
                        "register_validator",
                        format!("registered but whitelist error: {e}"),
                    );
                }
                NativeActionResult::ok("register_validator", 5000)
            }
            Err(e) => Self::econ_err(ctx, "register_validator", e),
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
            Err(e) => Self::econ_err(ctx, "update_commission", e),
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
            Err(e) => Self::econ_err(ctx, "rotate_validator_key", e),
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
        sample_ms: u64,
    ) -> NativeActionResult {
        // FIX 18 (ECON-FIND-19): the REPORTER must be an active, non-jailed
        // validator. s517: the reporter is the sender itself (direct submission)
        // or the validator whose registered hot oracle signer the sender is.
        let (reporter, status) = match Self::resolve_oracle_reporter(ctx, sender) {
            Ok(r) => r,
            Err(m) => return NativeActionResult::err("submit_oracle_prices", m),
        };
        if status != torus_economics::types::ValidatorStatus::Active {
            return NativeActionResult::err(
                "submit_oracle_prices",
                format!("validator {reporter} is not active (status: {status:?})"),
            );
        }

        use torus_core::oracle::{
            valid_oracle_price, MAX_ORACLE_PRICES_PER_SUBMISSION as CAP, MAX_ORACLE_SAMPLE_SKEW_MS as SKEW,
        };
        let err = |m: String| NativeActionResult::err("submit_oracle_prices", m);
        // Review M1(b): the signed sample time must be within SKEW of the
        // block's header time (seconds), so a delayed submission never lands
        // as a fresh price.
        let block_ms = ctx.timestamp.saturating_mul(1_000);
        if sample_ms.saturating_add(SKEW) < block_ms || sample_ms > block_ms.saturating_add(SKEW) {
            return err(format!(
                "submission sampled at {sample_ms} ms, more than {SKEW} ms from the block time {block_ms} ms"
            ));
        }
        // Item 2: validate EVERY entry before writing any (no per-action rollback)
        // — the action is all-or-nothing.
        if prices.is_empty() || prices.len() > CAP {
            return err(format!("a submission carries 1..={CAP} prices, got {}", prices.len()));
        }
        let mut seen = BTreeSet::new();
        for &(market_id, price) in prices {
            if !seen.insert(market_id) {
                return err(format!("duplicate market {market_id} in submission"));
            }
            match ctx.governance.market_exists(market_id) {
                Ok(true) => {}
                Ok(false) => return err(format!("market {market_id} is not listed")),
                Err(e) => return err(e.to_string()),
            }
            if !valid_oracle_price(price) {
                return err(format!("invalid oracle price {price} for market {market_id}"));
            }
        }

        // Review M1(b): newest sample wins per (market, validator), whatever
        // the in-block order: a sample not newer than the stored one is
        // skipped (an equal one keeps the first in canonical order).
        let mut newer = Vec::with_capacity(prices.len());
        for &(market_id, price) in prices {
            match ctx.oracle.stored_sample_ms(market_id, &reporter) {
                Ok(Some(stored)) if stored >= sample_ms => {}
                Ok(_) => newer.push((market_id, price)),
                Err(e) => return err(e.to_string()),
            }
        }
        for (market_id, price) in newer {
            if let Err(e) = ctx.oracle.submit_sampled(
                &reporter,
                market_id,
                price,
                sample_ms,
                ctx.block_height,
                ctx.timestamp,
            ) {
                return NativeActionResult::err("submit_oracle_prices", e.to_string());
            }
        }
        NativeActionResult::ok("submit_oracle_prices", 1000)
    }

    /// The validator an oracle submission from `sender` reports for, with its
    /// status: the sender's own validator record first (direct submission),
    /// else the hot-signer index cross-checked against that validator's
    /// record, so a stale index entry never resolves.
    fn resolve_oracle_reporter<T: StateBackend>(
        ctx: &NativeExecContext<T>,
        sender: &Address,
    ) -> Result<(Address, torus_economics::types::ValidatorStatus), String> {
        if let Some(v) = ctx.staking.get_validator(sender).map_err(|e| e.to_string())? {
            return Ok((*sender, v.status));
        }
        let key = torus_state::cf::oracle_signer_key(sender);
        let raw = ctx.state.get_cf_raw(torus_state::cf::CF_NATIVE_ORACLE, &key).map_err(|e| e.to_string())?;
        if let Some(v) = raw.filter(|b| b.len() == 20).map(|b| Address::from_slice(&b)) {
            if let Some(rec) = ctx.staking.get_validator(&v).map_err(|e| e.to_string())? {
                if rec.oracle_signer == Some(*sender) {
                    return Ok((v, rec.status));
                }
            }
        }
        Err(format!("{sender} is not a registered validator or oracle signer"))
    }

    /// `SetOracleSigner` (s517): set, rotate or (with `Address::ZERO`) clear
    /// the sender validator's hot oracle signer. Every check runs before the
    /// first write (no per-action rollback). Writes: the old index entry is
    /// deleted, the new one written, the record updated — rotation invalidates
    /// the old signer at once.
    fn exec_set_oracle_signer<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
        sender: &Address,
        signer: Address,
        proof: Option<&torus_types::OracleSignerProof>,
    ) -> NativeActionResult {
        use torus_economics::types::ValidatorStatus;
        use torus_state::cf::{oracle_signer_key, CF_NATIVE_ORACLE};
        let err = |m: String| NativeActionResult::err("set_oracle_signer", m);
        let new = (signer != Address::ZERO).then_some(signer);
        // Review L2: a tombstoned validator may still CLEAR its signer (so the
        // signer address is freed), but never set or rotate one.
        let mut v = match ctx.staking.get_validator(sender) {
            Ok(Some(v)) if v.status != ValidatorStatus::Tombstoned || new.is_none() => v,
            Ok(Some(_)) => return err(format!("validator {sender} is tombstoned")),
            Ok(None) => return err(format!("{sender} is not a registered validator")),
            Err(e) => return err(e.to_string()),
        };
        if v.oracle_signer == new {
            return NativeActionResult::ok("set_oracle_signer", 1000);
        }
        if let Some(s) = new {
            match ctx.staking.get_validator(&s) {
                Ok(None) => {}
                Ok(Some(_)) => return err(format!("signer {s} is a validator")),
                Err(e) => return err(e.to_string()),
            }
            // Review M3: proof of possession — the signer key signed
            // (this validator, chain id, nonce), so nobody can squat a signer
            // address they do not hold, nor replay another validator's proof.
            let proven = proof
                .and_then(|p| torus_types::eip712::recover_oracle_signer_proof(sender, p).ok())
                == Some(s);
            if !proven {
                return err(format!(
                    "signer {s}: missing or invalid proof of possession for validator {sender}"
                ));
            }
            match ctx.state.get_cf_raw(CF_NATIVE_ORACLE, &oracle_signer_key(&s)) {
                Ok(None) => {}
                Ok(Some(owner)) => {
                    return err(format!(
                        "signer {s} already serves validator 0x{}",
                        alloy_primitives::hex::encode(owner)
                    ))
                }
                Err(e) => return err(e.to_string()),
            }
        }
        // Writes (validation complete).
        if let Some(old) = v.oracle_signer {
            if let Err(e) = ctx.state.delete_cf_raw(CF_NATIVE_ORACLE, &oracle_signer_key(&old)) {
                return err(e.to_string());
            }
        }
        if let Some(s) = new {
            if let Err(e) = ctx.state.put_cf_raw(CF_NATIVE_ORACLE, &oracle_signer_key(&s), sender.as_slice()) {
                return err(e.to_string());
            }
        }
        v.oracle_signer = new;
        match ctx.staking.put_validator(sender, &v) {
            Ok(()) => NativeActionResult::ok("set_oracle_signer", 2000),
            Err(e) => err(e.to_string()),
        }
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
            ProposalAction::ListMarket(listing) => {
                if listing.max_leverage == 0 {
                    return NativeActionResult::err(
                        "submit_proposal",
                        "market listing max_leverage must be > 0".into(),
                    );
                }
                Some(ExecutionPayload::MarketListing {
                    market_id: 0, // assigned at execution time (max existing id + 1)
                    base_asset: listing.base_asset.clone(),
                    quote_asset: listing.quote_asset.clone(),
                    lot_size: listing.lot_size,
                    tick_size: listing.tick_size,
                    // Initial margin = 1 / max_leverage, in the genesis convention
                    // (percent as FixedPoint: 20x -> "5.0"). maintenance_margin_bps
                    // is a different quantity and has no slot in the market row.
                    initial_margin: torus_types::FixedPoint::from_raw(
                        100 * torus_types::FixedPoint::SCALE / listing.max_leverage as i128,
                    ),
                })
            }
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
            Err(e) => Self::econ_err(ctx, "submit_proposal", e),
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
                    Err(e) => Self::econ_err(ctx, "vote", e),
                }
            }
            _ => {
                let support = matches!(option, VoteOption::Yes);
                match ctx
                    .governance
                    .cast_vote(*sender, proposal_id, support, ctx.block_height)
                {
                    Ok(()) => NativeActionResult::ok("vote", 2000),
                    Err(e) => Self::econ_err(ctx, "vote", e),
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
        if let Err(rejection) = Self::check_withdrawal_margin(ctx, sender, fp_amount) {
            return NativeActionResult::rejected("withdraw_from_native", rejection);
        }
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
        if let Err(rejection) = Self::check_withdrawal_margin(ctx, sender, fp_amount) {
            return NativeActionResult::rejected("withdraw_to", rejection);
        }
        match Lockbox::withdraw_from_native_to(&ctx.state, sender, to, fp_amount) {
            Ok(()) => NativeActionResult::ok("withdraw_to", 1500),
            Err(e) => NativeActionResult::err("withdraw_to", e.to_string()),
        }
    }

    /// F1 (s517 decision 5, Hyperliquid `transfer_margin_required`): a native
    /// withdrawal — TransferToSpot, Withdraw, and CoreWriter LockboxWithdraw
    /// (drains as TransferToSpot) — must leave equity minus order margin >=
    /// max(Σ position IM, 10% × Σ position notional) (D3, SAFE variant: the
    /// resting orders' reservations are not collateral for the positions).
    /// `amount > available` (incl. any amount while `available < 0`) is left
    /// to the Lockbox's own cash check (error text unchanged).
    fn check_withdrawal_margin<T: StateBackend>(
        ctx: &NativeExecContext<T>,
        sender: &Address,
        amount: FixedPoint,
    ) -> Result<(), Rejection> {
        let bal = ctx
            .positions
            .get_native_balance(sender)
            .map_err(|e| (FailureReason::Other, e.to_string()))?;
        if amount <= FixedPoint::ZERO || amount > bal.available {
            return Ok(());
        }
        let view = AccountReader::of(ctx)
            .view(sender, &bal)
            .map_err(|e| (FailureReason::Other, e.to_string()))?;
        if view.withdrawal_allowed(amount) {
            return Ok(());
        }
        Err((
            FailureReason::Margin,
            format!(
                "withdrawal of {amount} would leave the account under-margined: equity after (excl. order margin) {}, required {}",
                view.equity() - view.order_margin - amount,
                view.transfer_required()
            ),
        ))
    }

    // ========================================================================
    // Block-level processing helpers (called by validator pipeline)
    // ========================================================================

    /// Whether CoreWriter / lockbox actions are queued for `height` — the block
    /// pipeline must then run the native path (and so [`Self::drain_core_writer`]) even
    /// for a block with no native actions and no fees. Queue entries for `height` are
    /// buffered by block `height - 1`'s EVM execution and committed in the SAME atomic
    /// batch as that block's EVM bundle (s515 F1), which lands before this block runs
    /// — so a DB read is authoritative here. A read error errs on the side of running
    /// the drain.
    pub fn core_writer_due(state_db: &StateDb, height: u64) -> bool {
        CoreWriterQueue::pending_count(state_db, height).map_or(true, |n| n > 0)
    }

    /// Item 2: whether this block must run the native phase for the oracle —
    /// any submission row in `state` (the block's overlay: DB + pipelined parent
    /// layer). Without rows the block-start step writes nothing, so "aggregate
    /// every block" == "run it whenever a row exists". Errors propagate (the
    /// caller fail-stops; never "assume due / not due").
    pub fn oracle_due<T: StateBackend>(state: &T) -> Result<bool, CoreError> {
        OracleManager::new(state.clone(), OracleConfig::default()).has_submissions()
    }

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

            // HL-parity: a stop order type (a row queued before the precompile
            // rejected them) fails instead of running as a plain Limit order.
            let result = match core_writer_to_native(qa) {
                Ok(Some(action)) => Self::execute(ctx, &qa.trader, &action),
                Ok(None) => Self::exec_settle_lockbox_deposit(ctx, qa),
                Err(e) => NativeActionResult::err("core_writer", e),
            };
            results.push(result);
        }
        Ok(results)
    }

    /// Native leg of a lockbox 0x0820 `depositToNative` queued last block (EVM-PF-05):
    /// credit native ONLY — the EVM value was burned inside the depositing tx. Kept off
    /// the `NativeAction` path on purpose: no user-signed action may credit native
    /// without an EVM debit.
    fn exec_settle_lockbox_deposit<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
        qa: &QueuedAction,
    ) -> NativeActionResult {
        let QueuedActionKind::LockboxDeposit { amount } = qa.kind else {
            return NativeActionResult::err("lockbox_deposit", "not a lockbox deposit".into());
        };
        match Lockbox::credit_native(&ctx.state, &qa.trader, amount) {
            Ok(()) => NativeActionResult::ok("lockbox_deposit", 1500),
            Err(e) => NativeActionResult::err("lockbox_deposit", e.to_string()),
        }
    }

    /// Item 2 (option A) block-start step — runs FIRST in every executed native
    /// block, before any action: deletes submission rows older than the window
    /// (all markets), then aggregates every listed market from the rows of
    /// EARLIER blocks, weighted by the whole-token stake of Active validators,
    /// at the block timestamp. Nothing else writes the aggregate row, so the
    /// whole block reads one mark. Per-market errors are results; a storage
    /// error in the global reads (prune, market list, validator set) is a node
    /// fault → `fatal_error` (fail-stop, never a silently skipped aggregation).
    pub fn begin_block_oracle<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
    ) -> Vec<NativeActionResult> {
        match Self::oracle_inputs(ctx) {
            Ok((markets, stakes)) => {
                let results = Self::aggregate_oracle_prices(ctx, &markets, &stakes);
                Self::fill_block_marks(ctx, &markets);
                results
            }
            Err(e) => {
                ctx.fatal_error = Some(format!("oracle block-start step: {e}"));
                Vec::new()
            }
        }
    }

    /// The step's global reads: prune, listed markets (ascending), Active stakes.
    #[allow(clippy::type_complexity)]
    fn oracle_inputs<T: StateBackend>(
        ctx: &NativeExecContext<T>,
    ) -> Result<(Vec<MarketId>, Vec<(Address, FixedPoint)>), String> {
        ctx.oracle.prune_submissions(ctx.timestamp).map_err(|e| e.to_string())?;
        let markets = ctx.governance.listed_market_ids().map_err(|e| e.to_string())?;
        let stakes = ctx
            .staking
            .all_validators()
            .map_err(|e| e.to_string())?
            .into_iter()
            .filter(|v| v.status == torus_economics::types::ValidatorStatus::Active)
            .map(|v| {
                let power = i128::from(whole_token_power(&v));
                (v.address, FixedPoint::from_raw(power * FixedPoint::SCALE))
            })
            .collect();
        Ok((markets, stakes))
    }

    /// Item 6 C2: the end of [`Self::begin_block_oracle`] (after the
    /// aggregation, before any action): fill the block's mark table for
    /// `listed`, the margin-config markets, every market with an aggregate
    /// row and every book market, and set its version against the previous block's state
    /// ([`NativeExecContext::attach_resident_block`]). A storage error in the
    /// aggregate-row scan leaves no table (every mark reads the oracle, as
    /// before C2). Public for the harness that splits the oracle step.
    pub fn fill_block_marks<T: StateBackend>(ctx: &mut NativeExecContext<T>, listed: &[MarketId]) {
        let prev = ctx.prev_marks.take();
        ctx.block_marks = None;
        // C3: memo entries are valued at one table version.
        if let Some(sums) = ctx.sums.as_mut() {
            sums.start(None);
        }
        let aggregated = match ctx.oracle.aggregated_market_ids() {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!(%e, height = ctx.block_height, "item 6: mark table not built (aggregate scan failed) — marks read the oracle");
                return;
            }
        };
        let markets: BTreeSet<MarketId> = listed
            .iter()
            .copied()
            .chain(ctx.margin_configs.keys().copied())
            .chain(aggregated)
            .chain(ctx.order_books.keys().copied())
            .collect();
        // R02: a local fault in a mark read fail-stops (no table).
        let marks = match BlockMarks::read(&ctx.oracle, ctx.timestamp, markets) {
            Ok(marks) => marks,
            Err(e) => {
                Self::latch_core_fault(ctx, "block mark table", &e);
                return;
            }
        };
        let version = match prev {
            Some(p) if p.marks.marks == marks && p.configs == ctx.margin_configs => p.marks.version,
            _ => MARK_VERSIONS.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1,
        };
        ctx.block_marks = Some(BlockMarks::new(marks, version));
        if let Some(sums) = ctx.sums.as_mut() {
            sums.start(Some(version));
        }
    }

    /// Aggregate oracle prices for `markets` at the block timestamp — called by
    /// [`Self::begin_block_oracle`]. One result per market; R02: a local
    /// fault also latches `fatal_error`.
    pub fn aggregate_oracle_prices<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
        markets: &[MarketId],
        validator_stakes: &[(Address, FixedPoint)],
    ) -> Vec<NativeActionResult> {
        let mut results = Vec::new();
        for &market_id in markets {
            match ctx
                .oracle
                .aggregate_price(market_id, ctx.block_height, ctx.timestamp, validator_stakes)
            {
                Ok(_) => results.push(NativeActionResult::ok("oracle_aggregate", 500)),
                Err(e) => {
                    // R02: per-market errors are results, except a local
                    // fault (failed read, undecodable row): fail-stop.
                    Self::latch_core_fault(ctx, "oracle aggregate", &e);
                    results.push(NativeActionResult::err("oracle_aggregate", e.to_string()))
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
            Err(e) => Self::econ_err(ctx, "fee_distribution", e),
        }
    }

    /// Check and process epoch boundary (reward distribution + validator rotation).
    ///
    /// Phase A — Rewards: distributes permanent staking rewards and validator
    /// inflation to the CURRENT active set. Errors log-and-continue (reward
    /// bugs should not block rotation); a local fault (row 74) also latches
    /// `fatal_error`, as does one in Phase B.
    ///
    /// Phase B — Rotation (consensus bug (b), torus_economics::epoch_plan):
    /// applies the plan the previous boundary stored (the set consensus installs
    /// at this height: key rotations + statuses) and stores the plan for the
    /// next boundary. The only writer of epoch statuses / rotated keys.
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
            Self::latch_local_fault(ctx, "permanent staking rewards", &e);
        }

        if let Err(e) =
            RewardDistributor::distribute_validator_inflation(&ctx.staking, ctx.epoch_length)
        {
            tracing::error!(%e, "validator inflation distribution failed");
            Self::latch_local_fault(ctx, "validator inflation", &e);
        }

        // --- Phase B: Validator set rotation ---

        Some(
            match EpochManager::execute_planned_rotation(
                &ctx.staking,
                ctx.max_validators,
                ctx.block_height,
                ctx.epoch_length,
            ) {
                Ok(new_set) => {
                    if let Some(ref m) = ctx.metrics {
                        m.epoch_number.set(
                            EpochManager::epoch_for_block(ctx.block_height, ctx.epoch_length) as i64,
                        );
                        if let Some(ref set) = new_set {
                            m.validator_set_size.set(set.validators.len() as i64);
                        }
                    }
                    EpochBoundaryResult {
                        action: NativeActionResult::ok("epoch_boundary", 5000),
                        new_set,
                    }
                }
                Err(e) => {
                    tracing::error!(%e, height = ctx.block_height, "planned epoch rotation failed");
                    Self::latch_local_fault(ctx, "epoch_rotation", &e);
                    EpochBoundaryResult {
                        action: NativeActionResult::err("epoch_rotation", e.to_string()),
                        new_set: None,
                    }
                }
            },
        )
    }

    /// Process pending governance proposals: one result per proposal outcome.
    /// s94: a proposal that failed at execution is an error result carrying
    /// its id and reason; the others still run. A storage error aborts the
    /// step as one error result (as before) and, row 74, latches
    /// `fatal_error` when it is a local fault: the node fail-stops.
    pub fn process_governance<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
    ) -> Vec<NativeActionResult> {
        use torus_economics::governance::ProposalOutcome;
        match ctx.governance.process_pending_proposals(ctx.block_height) {
            Ok(outcomes) => outcomes
                .iter()
                .map(|outcome| match outcome {
                    ProposalOutcome::Failed(id, reason) => NativeActionResult::err(
                        "governance_process",
                        format!("proposal {id} execution failed: {reason}"),
                    ),
                    ProposalOutcome::Passed(_)
                    | ProposalOutcome::Rejected(_)
                    | ProposalOutcome::Executed(_) => {
                        NativeActionResult::ok("governance_process", 1000)
                    }
                })
                .collect(),
            Err(e) => vec![Self::econ_err(ctx, "governance_process", e)],
        }
    }
}

/// Whole-token voting / oracle power: floor(wei / 10^18), saturating at
/// u64::MAX (U256 wei would overflow FixedPoint).
fn whole_token_power(v: &torus_economics::ValidatorState) -> u64 {
    let wei = U256::from(10u64).pow(U256::from(18u64));
    (v.total_stake() / wei).try_into().unwrap_or(u64::MAX)
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
        | NativeAction::ClaimRewards
        | NativeAction::ClaimUnbonded => ActionCategory::Staking,
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
    let ((pre_evm, _), (post_evm, _)) = sort_native_actions_indexed(actions.to_vec());
    (pre_evm, post_evm)
}

/// One sorted list of [`sort_native_actions_indexed`]: the actions, and for
/// each one its position in the input.
pub type IndexedActions = (Vec<(Address, NativeAction)>, Vec<u32>);

/// [`sort_native_actions`] by value (no clone of the actions), returning with
/// each list the input position of every entry, so a per-entry result maps
/// back to its action by index (item 6 cut 1: the v2 action status).
///
/// Same lists as [`sort_native_actions`]: the key is (category, sender,
/// keccak of the canonical bytes) and the sort is stable, so entries with
/// equal keys keep their input order.
pub fn sort_native_actions_indexed(
    actions: Vec<(Address, NativeAction)>,
) -> (IndexedActions, IndexedActions) {
    let mut pre_evm = Vec::new();
    let mut post_evm = Vec::new();
    for (i, (sender, action)) in actions.into_iter().enumerate() {
        let key = action_sort_key(&sender, &action);
        let entry = (key, i as u32, (sender, action));
        match key.0 {
            ActionCategory::Cancellation | ActionCategory::NonGtcOrder => pre_evm.push(entry),
            _ => post_evm.push(entry),
        }
    }
    (sort_deterministic(pre_evm), sort_deterministic(post_evm))
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

/// An action with its pre-computed sort key and its input position.
type KeyedAction = (
    (ActionCategory, Address, B256),
    u32,
    (Address, NativeAction),
);

/// Sort keyed actions deterministically by (category, sender,
/// action_content_hash), stable (the keys are pre-computed to avoid repeated
/// hashing during the sort); returns the actions and their input positions.
fn sort_deterministic(mut keyed: Vec<KeyedAction>) -> IndexedActions {
    keyed.sort_by(|a, b| a.0.cmp(&b.0));
    keyed.into_iter().map(|(_, i, pair)| (pair, i)).unzip()
}

// ============================================================================
// CoreWriter → NativeAction conversion
// ============================================================================

/// Convert a CoreWriter queued action to a NativeAction for execution.
///
/// `Ok(None)` for `LockboxDeposit`: its native-only credit has no `NativeAction` form
/// and is settled directly by `drain_core_writer`. `Err` for a `PlaceOrder` whose
/// order type is not Limit / Market (stop types carry no trigger price here).
fn core_writer_to_native(qa: &QueuedAction) -> Result<Option<NativeAction>, String> {
    Ok(Some(match &qa.kind {
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
            order_type: decode_order_type(*order_type)
                .ok_or_else(|| format!("unsupported CoreWriter order_type {order_type}"))?,
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
        QueuedActionKind::ClaimUnbonded => NativeAction::ClaimUnbonded,
        // Lockbox 0x0820 `withdrawFromNative`: the same debit-native / credit-EVM
        // (×10^10) path as a signed TransferToSpot; fails cleanly if native is short.
        QueuedActionKind::LockboxWithdraw { amount } => NativeAction::TransferToSpot {
            amount: fp_to_u256(*amount),
        },
        QueuedActionKind::LockboxDeposit { .. } => return Ok(None),
    }))
}

#[cfg(test)]
mod core_writer_conversion_tests {
    use super::*;

    fn place(order_type: u8) -> QueuedAction {
        QueuedAction {
            trader: Address::new([1; 20]),
            kind: QueuedActionKind::PlaceOrder {
                market_id: 1,
                side: 0,
                order_type,
                price: FixedPoint::from_raw(100),
                quantity: FixedPoint::from_raw(1),
                time_in_force: 0,
            },
            block_queued: 1,
        }
    }

    /// Review nit: the conversion itself refuses stop / unknown order types
    /// (no silent Limit fallback left anywhere); Limit and Market convert.
    #[test]
    fn stop_and_unknown_order_types_do_not_convert() {
        for t in [2u8, 3, 4, 255] {
            assert!(core_writer_to_native(&place(t)).is_err(), "order_type {t}");
        }
        for (t, want) in [(0u8, OrderType::Limit), (1, OrderType::Market)] {
            match core_writer_to_native(&place(t)) {
                Ok(Some(NativeAction::PlaceOrder(p))) => assert_eq!(p.order_type, want),
                other => panic!("order_type {t}: {other:?}"),
            }
        }
    }
}

/// CoreWriter order type: 0 = Limit, 1 = Market; `None` for anything else
/// (stop types carry no trigger price over the CoreWriter ABI).
fn decode_order_type(code: u8) -> Option<OrderType> {
    match code {
        0 => Some(OrderType::Limit),
        1 => Some(OrderType::Market),
        _ => None,
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

#[cfg(test)]
mod integer_margin_tests {
    use super::*;
    use torus_core::margin::MarginTier;

    fn legacy(notional: FixedPoint, leverage: u32) -> FixedPoint {
        notional / FixedPoint::from_raw(i128::from(leverage) * FixedPoint::SCALE)
    }

    // Frozen reservation formula, including early exits and tier lookup order.
    fn legacy_reserve(
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

    #[test]
    fn reserve_integer_margin_matches_legacy_tiers_and_rounding() {
        let default = MarketMarginConfig::new(1, 999);
        let mut tiered = MarketMarginConfig::new(1, 999);
        tiered.tiers = vec![
            MarginTier {
                max_notional: FixedPoint::from_raw(100 * FixedPoint::SCALE),
                max_leverage: 3,
            },
            MarginTier {
                max_notional: FixedPoint::from_raw(200 * FixedPoint::SCALE),
                max_leverage: 7,
            },
        ]; // Above the final tier falls back to 1x, not the scalar 999.
        let mut empty = MarketMarginConfig::new(1, 999);
        empty.tiers.clear();
        let mut quantities = vec![1, 19, 20, 21, FixedPoint::SCALE - 1, i128::MAX];
        for boundary in [100, 200, 100_000, 1_000_000, 10_000_000] {
            let raw = boundary * FixedPoint::SCALE;
            quantities.extend([raw - 1, raw, raw + 1]);
        }
        for cfg in [None, Some(&default), Some(&tiered), Some(&empty)] {
            for &raw in &quantities {
                // ONE hits exact tier boundaries; subunit prices also exercise
                // multiplication truncation before leverage division.
                for price in [
                    FixedPoint::ONE,
                    FixedPoint::from_raw(1),
                    FixedPoint::from_raw(FixedPoint::SCALE / 3 + 1),
                ] {
                    let qty = FixedPoint::from_raw(raw);
                    assert_eq!(
                        NativeExecutor::reserve_for_qty_cfg(cfg, price, qty),
                        legacy_reserve(cfg, price, qty),
                        "price={price:?} qty={qty:?} config={cfg:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn reserve_integer_margin_preserves_early_exits_and_panic_order() {
        let mut zero = MarketMarginConfig::new(1, 999);
        zero.tiers = vec![MarginTier {
            max_notional: FixedPoint::MAX,
            max_leverage: 0,
        }];
        let outcome = |f: &dyn Fn() -> FixedPoint| {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).map_err(|payload| {
                payload
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| payload.downcast_ref::<&str>().map(|s| (*s).to_owned()))
                    .expect("string panic payload")
            })
        };
        for cfg in [None, Some(&zero)] {
            for p in [i128::MIN, -1, 0, 1, FixedPoint::SCALE, i128::MAX] {
                for q in [i128::MIN, -1, 0, 1, FixedPoint::SCALE, i128::MAX] {
                    let price = FixedPoint::from_raw(p);
                    let qty = FixedPoint::from_raw(q);
                    let expected = outcome(&|| legacy_reserve(cfg, price, qty));
                    let actual = outcome(&|| NativeExecutor::reserve_for_qty_cfg(cfg, price, qty));
                    if p <= 0 || q <= 0 {
                        assert_eq!(actual, Ok(FixedPoint::ZERO));
                    }
                    assert_eq!(actual, expected, "price={p} qty={q} config={cfg:?}");
                }
            }
        }
    }

    #[test]
    fn cancel_all_integer_margin_matches_general_division_extremes() {
        let values = [
            i128::MIN,
            i128::MIN + 1,
            -FixedPoint::SCALE - 1,
            -FixedPoint::SCALE,
            -31,
            -1,
            0,
            1,
            19,
            20,
            21,
            FixedPoint::SCALE - 1,
            FixedPoint::SCALE,
            FixedPoint::SCALE + 1,
            i128::MAX - 1,
            i128::MAX,
        ];
        let leverages = [1, 2, 3, 5, 7, 20, 31, 50, u32::MAX];
        for raw in values {
            for leverage in leverages {
                let notional = FixedPoint::from_raw(raw);
                assert_eq!(
                    NativeExecutor::margin_at_integer_leverage(notional, leverage),
                    legacy(notional, leverage),
                    "raw={raw} leverage={leverage}"
                );
            }
        }
        // Deterministic broad raw-value coverage, including negative values.
        let mut bits = 0x7d21_975a_ffff_0011_ee42_189f_5555_aaabu128;
        for _ in 0..512 {
            bits ^= bits << 13;
            bits ^= bits >> 7;
            bits ^= bits << 17;
            let notional = FixedPoint::from_raw(bits as i128);
            let leverage = (bits as u32).max(1);
            assert_eq!(
                NativeExecutor::margin_at_integer_leverage(notional, leverage),
                legacy(notional, leverage)
            );
        }
    }

    #[test]
    fn cancel_all_integer_margin_zero_preserves_panic() {
        fn text(payload: Box<dyn std::any::Any + Send>) -> String {
            if let Some(s) = payload.downcast_ref::<String>() {
                s.clone()
            } else if let Some(s) = payload.downcast_ref::<&str>() {
                (*s).to_owned()
            } else {
                panic!("unexpected panic payload")
            }
        }
        for raw in [i128::MIN, -1, 0, 1, i128::MAX] {
            let notional = FixedPoint::from_raw(raw);
            let old = std::panic::catch_unwind(|| legacy(notional, 0)).unwrap_err();
            let new = std::panic::catch_unwind(|| {
                NativeExecutor::margin_at_integer_leverage(notional, 0)
            })
            .unwrap_err();
            let old = text(old);
            assert!(old.starts_with("FixedPoint division error"));
            assert_eq!(text(new), old);
        }
    }
}

/// Fix 1 (s87) / item 6 C2 / C3: the block mark table and the sums cache
/// (memo and persistent entries) return exactly what the per-call reads do.
#[cfg(test)]
mod maker_accounts_tests {
    use super::*;
    use torus_core::position::Position;

    struct Lcg(u64);
    impl Lcg {
        fn below(&mut self, n: u64) -> u64 {
            self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (self.0 >> 33) % n
        }
    }

    fn fp(v: i64) -> FixedPoint {
        FixedPoint::from_raw(v as i128 * FixedPoint::SCALE)
    }

    /// 200 seeded rounds: 1-12 traders with positions in 1-25 markets (long /
    /// short, random entry, sometimes isolated, one overflow-sized), balances
    /// with negative `available`, marks fresh / stale / absent per market.
    /// Every (trader, market) pair, queried in a shuffled order from 4 threads
    /// through one reader with the block's table and sums cache (item 6 C2 /
    /// C3: over an overlay with R, first through the block memo, then through
    /// the persistent entries it leaves), equals `AccountReader::maker_account`
    /// without cache or table; that reader gives the same `mark` / `view` /
    /// `pos_net` / `position_px`.
    #[test]
    fn cached_reader_maker_accounts_equal_reader_on_random_states() {
        let dir = tempfile::tempdir().unwrap();
        let db = StateDb::open(dir.path()).unwrap();
        let now = 10_000u64;
        let ctx = NativeExecContext::new(db.clone(), 9, now, 0, 1_000, 10, Address::ZERO, Address::ZERO, Address::ZERO);
        let reporters: Vec<(Address, FixedPoint)> = (0..3u8).map(|i| (Address::new([200 + i; 20]), fp(1))).collect();
        let mut rng = Lcg(0x5EED_0001);
        let (mut pairs_checked, mut marked, mut overflowed, mut persistent) = (0usize, 0usize, 0usize, 0usize);
        for round in 0..200u64 {
            let n_traders = 1 + rng.below(12);
            let n_markets = 1 + rng.below(25);
            let markets: Vec<MarketId> = (0..n_markets).map(|i| round * 100 + 1 + i).collect();
            for &m in &markets {
                let ts = match rng.below(3) {
                    0 => continue,  // absent
                    1 => now - 120, // stale
                    _ => now - 5,   // fresh
                };
                let px = fp(1 + rng.below(50_000) as i64);
                for (v, _) in &reporters {
                    ctx.oracle.submit_price(v, m, px, 8, ts).unwrap();
                }
                ctx.oracle.aggregate_price(m, 8, ts, &reporters).unwrap();
            }
            let traders: Vec<Address> = (0..n_traders)
                .map(|t| {
                    let mut b = [0x31u8; 20];
                    b[12..].copy_from_slice(&(round * 1_000 + t).to_be_bytes());
                    Address::new(b)
                })
                .collect();
            for (ti, t) in traders.iter().enumerate() {
                let available = fp(rng.below(2_000_000) as i64 - 500_000);
                ctx.positions
                    .put_native_balance(t, &NativeBalance { available, order_margin: fp(rng.below(1_000) as i64) })
                    .unwrap();
                for &m in &markets {
                    if rng.below(4) == 0 {
                        continue; // flat here
                    }
                    let huge = round % 17 == 0 && ti == 0 && m == markets[0];
                    let size = if huge {
                        FixedPoint::from_raw(i128::MAX / 3)
                    } else {
                        FixedPoint::from_raw(1 + rng.below(500 * FixedPoint::SCALE as u64) as i128)
                    };
                    let is_long = rng.below(2) == 0;
                    let entry_price = fp(1 + rng.below(50_000) as i64);
                    ctx.positions
                        .put_position(&Position {
                            trader: *t,
                            market_id: m,
                            is_long,
                            size,
                            entry_price,
                            // The huge size overflows the notional anyway.
                            cost_basis: entry_price.checked_mul(size).unwrap_or(FixedPoint::MAX),
                            realized_pnl: FixedPoint::ZERO,
                            isolated_margin: FixedPoint::ZERO,
                            margin_type: if rng.below(40) == 0 { MarginType::Isolated } else { MarginType::Cross },
                        })
                        .unwrap();
                }
            }
            let plain = AccountReader::of(&ctx);
            let table = BlockMarks::new(BlockMarks::read(&ctx.oracle, now, markets.iter().copied()).unwrap(), 7);
            // The block's view: an overlay with R (nothing pending), its context.
            let mut overlay = NativeStateOverlay::new(db.clone());
            overlay.attach_resident(Arc::new(torus_state::ResidentRows::build(&overlay).unwrap()));
            let rctx = NativeExecContext::new(overlay, 9, now, 0, 1_000, 10, Address::ZERO, Address::ZERO, Address::ZERO);
            let mut memo_sums = BlockSums { shadow: true, ..BlockSums::default() };
            memo_sums.start(Some(7));
            let reader = AccountReader { marks: Some(&table), sums: Some(&memo_sums), ..AccountReader::of(&rctx) };
            for &m in &markets {
                assert_eq!(reader.mark(m), plain.mark(m), "round {round}: mark {m}");
                marked += usize::from(plain.mark(m).is_some());
            }
            assert_eq!(reader.mark(u64::MAX), plain.mark(u64::MAX), "outside the table");
            let pairs: Vec<(Address, MarketId)> =
                traders.iter().flat_map(|t| markets.iter().map(move |m| (*t, *m))).collect();
            let want: HashMap<(Address, MarketId), MakerAccount> =
                pairs.iter().map(|&(t, m)| ((t, m), plain.maker_account(&t, m))).collect();
            for t in &traders {
                let bal = ctx.positions.get_native_balance(t).unwrap();
                let (a, b) = (reader.view(t, &bal), plain.view(t, &bal));
                overflowed += usize::from(b.is_err());
                assert_eq!(format!("{a:?}"), format!("{b:?}"), "round {round}: view");
                assert_eq!(format!("{:?}", reader.pos_net(t)), format!("{:?}", plain.pos_net(t)), "round {round}: pos_net");
                for &m in &markets {
                    assert_eq!(
                        format!("{:?}", reader.position_px(t, m)),
                        format!("{:?}", plain.position_px(t, m)),
                        "round {round}: position_px"
                    );
                }
            }
            std::thread::scope(|s| {
                for w in 0..4u64 {
                    let mut order = pairs.clone();
                    let mut shuffle = Lcg(round * 4 + w + 1);
                    for i in (1..order.len()).rev() {
                        order.swap(i, shuffle.below(i as u64 + 1) as usize);
                    }
                    let (reader, want) = (&reader, &want);
                    s.spawn(move || {
                        for (t, m) in order {
                            assert_eq!(reader.maker_account(&t, m), want[&(t, m)], "round {round}: {t} market {m}");
                        }
                    });
                }
            });
            pairs_checked += pairs.len();
            // The memo, carried as persistent entries into the next block's sums.
            let c = &memo_sums.counters;
            let computed = c.computed.load(std::sync::atomic::Ordering::Relaxed);
            assert!(computed <= traders.len(), "round {round}: each trader built once ({computed})");
            assert!(memo_sums.shadow_mismatches.lock().unwrap().is_empty(), "round {round}: shadow");
            let mut next = BlockSums { shadow: true, ..BlockSums::new(memo_sums.into_carry(None).cache) };
            next.start(Some(7));
            let carried = AccountReader { marks: Some(&table), sums: Some(&next), ..AccountReader::of(&rctx) };
            for &(t, m) in &pairs {
                assert_eq!(carried.maker_account(&t, m), want[&(t, m)], "round {round}: persistent {t} market {m}");
            }
            persistent += next.counters.persistent.load(std::sync::atomic::Ordering::Relaxed);
            assert_eq!(next.counters.computed.load(std::sync::atomic::Ordering::Relaxed), 0, "round {round}: no rebuild");
            assert!(next.shadow_mismatches.lock().unwrap().is_empty(), "round {round}: shadow");
        }
        assert!(
            pairs_checked > 1_000 && marked > 100 && overflowed > 0 && persistent > 1_000,
            "non-vacuous: {pairs_checked} {marked} {overflowed} {persistent}"
        );
    }
}

/// Option B (s87): Phase 2's `SenderFold::pool` must name exactly the market
/// Phase 3 gives each sender's D2 pool — else B would raise a sell in the
/// pool market or miss a taker-only one.
#[cfg(test)]
mod option_b_fold_pool_tests {
    use super::*;

    struct Lcg(u64);
    impl Lcg {
        fn below(&mut self, n: u64) -> u64 {
            self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (self.0 >> 33) % n
        }
    }

    fn fp(v: i64) -> FixedPoint {
        FixedPoint::from_raw(v as i128 * FixedPoint::SCALE)
    }

    /// 100 seeded batches: 2-8 senders (unfunded / thin / rich), 1-5 markets
    /// with or without resting bids, orders of every kind — limit / market /
    /// IOC / FOK / PostOnly / reduce-only / stops, buys and sells, some
    /// invalid (market price 0) — so senders' first checked orders are often
    /// rejected (margin, validation). The serial fold's `pool` equals the
    /// keys of Phase 3's pools ([`NativeExecutor::d2_pool_takers`]), and the
    /// sharded prepare's outcomes give the same pools.
    #[test]
    fn fold_pool_equals_phase3_pool() {
        let mut rng = Lcg(0x5EED_0B0B);
        let (mut pooled, mut first_rejected, mut raised) = (0usize, 0usize, 0usize);
        let (mut candidates, mut held_positions) = (0usize, 0usize);
        for round in 0..100u64 {
            let dir = tempfile::tempdir().unwrap();
            let db = StateDb::open(dir.path()).unwrap();
            let mut ctx = NativeExecContext::new(db, 9, 10_000, 0, 1_000, 10, Address::ZERO, Address::ZERO, Address::ZERO);
            let n_senders = 2 + rng.below(7);
            let n_markets = 1 + rng.below(5);
            let senders: Vec<Address> = (0..n_senders).map(|i| Address::new([10 + i as u8; 20])).collect();
            for s in &senders {
                let available = fp([0, 30, 60, 1_000_000][rng.below(4) as usize]);
                ctx.positions
                    .put_native_balance(s, &NativeBalance { available, order_margin: FixedPoint::ZERO })
                    .unwrap();
            }
            let maker = Address::new([200; 20]);
            for m in 1..=n_markets {
                if rng.below(3) > 0 {
                    let mut b = OrderBook::new(m, FixedPoint::ONE, FixedPoint::ONE);
                    let bid = PlaceOrderParams {
                        market_id: m,
                        is_buy: true,
                        price: fp(95 + rng.below(15) as i64),
                        quantity: fp(5),
                        order_type: OrderType::Limit,
                        time_in_force: TimeInForce::GTC,
                        reduce_only: false,
                        client_order_id: None,
                    };
                    b.place_order(bid, maker, 1);
                    ctx.order_books.insert(m, b);
                }
            }
            let orders: Vec<(Address, PlaceOrderParams)> = (0..2 + rng.below(30))
                .map(|_| {
                    let s = senders[rng.below(n_senders) as usize];
                    let price = fp(90 + rng.below(20) as i64);
                    let mut p = PlaceOrderParams {
                        market_id: 1 + rng.below(n_markets),
                        is_buy: rng.below(2) == 0,
                        price,
                        quantity: fp(1 + rng.below(10) as i64),
                        order_type: OrderType::Limit,
                        time_in_force: TimeInForce::GTC,
                        reduce_only: false,
                        client_order_id: None,
                    };
                    match rng.below(12) {
                        0 => p.order_type = OrderType::Market,
                        1 => {
                            p.order_type = OrderType::Market;
                            p.price = FixedPoint::ZERO; // validation reject
                        }
                        2 => p.time_in_force = TimeInForce::IOC,
                        3 => p.time_in_force = TimeInForce::FOK,
                        4 => p.time_in_force = TimeInForce::PostOnly,
                        5 => p.reduce_only = true,
                        6 => p.order_type = OrderType::StopLimit { trigger: price, limit: price },
                        _ => {}
                    }
                    (s, p)
                })
                .collect();
            let place_orders: Vec<(usize, Address, &PlaceOrderParams)> =
                orders.iter().enumerate().map(|(i, (s, p))| (i, *s, p)).collect();
            let basis = NativeExecutor::phase2_reservation_basis(&ctx, &place_orders);
            let open_at_start = HashMap::default();
            let reader = AccountReader::of(&ctx);
            let markets = NativeExecutor::phase2_markets(&ctx.order_books, &ctx.state, &reader, &place_orders);

            let mut fold = SenderFold::default();
            let mut batches: HashMap<MarketId, Vec<PreparedOrder<'_>>> = HashMap::default();
            let mut results: Vec<NativeActionResult> =
                (0..orders.len()).map(|_| NativeActionResult::ok("pending", 0)).collect();
            let mut next_id = 1u128;
            let mut first_seen: HashMap<Address, bool> = HashMap::default();
            let mut want_excess: HashMap<Address, FixedPoint> = HashMap::default();
            for (i, (s, p)) in orders.iter().enumerate() {
                let outcome = NativeExecutor::prepare_one(&reader, &open_at_start, &basis, &markets, &mut fold, i, s, p);
                if let PrepOutcome::Pass(pass) = &outcome {
                    let base = basis.get(&i).map_or(NativeExecutor::reserve_price(p), |b| b.0);
                    let qty = basis.get(&i).map_or(p.quantity, |b| b.1);
                    if pass.required > NativeExecutor::reserve_for_qty_cfg(None, base, qty) {
                        raised += 1;
                    }
                    if pass.excess_im > FixedPoint::ZERO {
                        *want_excess.entry(*s).or_insert(FixedPoint::ZERO) += pass.excess_im;
                    }
                }
                if NativeExecutor::match_margin_checked(p) && !first_seen.contains_key(s) {
                    first_seen.insert(*s, true);
                    if matches!(outcome, PrepOutcome::Reject { .. }) {
                        first_rejected += 1;
                    }
                }
                NativeExecutor::stitch_outcome(&mut next_id, &None, &markets, &mut batches, &mut results, i, s, p, outcome);
            }
            // M1 cut 5: the fold's pools = the old pass over the batches
            // (first checked taker per sender, its market and pos_net).
            let want: HashMap<Address, (MarketId, FixedPoint)> =
                NativeExecutor::d2_pool_takers(&batches).into_iter().map(|(s, m, n)| (s, (m, n))).collect();
            let serial = fold.finish();
            assert_eq!(serial.pools, want, "round {round}: serial fold");
            assert_eq!(serial.excess, want_excess, "round {round}: serial excess");
            pooled += want.len();
            for p in batches.values().flatten() {
                // M1 cut 2: the candidate flag is the old top-up condition.
                let old = NativeExecutor::takes_bid_floor(p.params)
                    && want.get(&p.sender).is_some_and(|(m, _)| *m != p.params.market_id);
                assert_eq!(p.top_up_candidate, old, "round {round}: order {}", p.index);
                candidates += usize::from(old);
                // M1 cut 8: pre_pos = `position_px` (every order with an
                // account check; checked takers always).
                assert_eq!(p.pre_pos.is_some(), !p.params.reduce_only, "round {round}");
                if let Some(pre) = p.pre_pos {
                    assert_eq!(Ok(pre), reader.position_px(&p.sender, p.params.market_id).map_err(|e| e.to_string()));
                    held_positions += usize::from(pre.0 != FixedPoint::ZERO);
                }
            }

            // Sharded prepare (2 workers): its outcomes stitched in flat order.
            let mut groups: Vec<(Address, Vec<(usize, &PlaceOrderParams)>)> = Vec::new();
            for (i, (s, p)) in orders.iter().enumerate() {
                match groups.iter_mut().find(|g| g.0 == *s) {
                    Some(g) => g.1.push((i, p)),
                    None => groups.push((*s, vec![(i, p)])),
                }
            }
            let (mut outcomes, sharded_out) =
                NativeExecutor::phase2_parallel_prepare(&reader, &open_at_start, &basis, &markets, &groups, 2, orders.len())
                    .expect("no worker panic");
            let mut sharded: HashMap<MarketId, Vec<PreparedOrder<'_>>> = HashMap::default();
            let mut next_id = 1u128;
            for (i, (s, p)) in orders.iter().enumerate() {
                let o = outcomes[i].take().unwrap();
                NativeExecutor::stitch_outcome(&mut next_id, &None, &markets, &mut sharded, &mut results, i, s, p, o);
            }
            let got: HashMap<Address, (MarketId, FixedPoint)> =
                NativeExecutor::d2_pool_takers(&sharded).into_iter().map(|(s, m, n)| (s, (m, n))).collect();
            assert_eq!(got, want, "round {round}: sharded");
            assert_eq!(sharded_out.pools, want, "round {round}: sharded fold");
            assert_eq!(sharded_out.excess, serial.excess, "round {round}: sharded excess");
            let cache = |c: &BalanceCache| {
                let mut v: Vec<_> =
                    c.map.iter().map(|(a, b)| (*a, b.balance.available, b.balance.order_margin, b.dirty)).collect();
                v.sort_unstable_by_key(|x| x.0);
                let mut d = c.dirty.clone();
                d.sort_unstable();
                (v, d)
            };
            assert_eq!(cache(&sharded_out.cache), cache(&serial.cache), "round {round}: caches");
            let key = |b: &HashMap<MarketId, Vec<PreparedOrder<'_>>>| {
                let mut v: Vec<_> = b
                    .values()
                    .flatten()
                    .map(|p| (p.index, p.order_id, p.margin_reserved, p.checked_pos_net, p.pre_pos, p.top_up_candidate, p.res_price))
                    .collect();
                v.sort_unstable_by_key(|x| x.0);
                v
            };
            assert_eq!(key(&sharded), key(&batches), "round {round}: prepared orders");
        }
        assert!(candidates > 20 && held_positions == 0, "non-vacuous: {candidates} candidates");
        assert!(pooled > 100 && first_rejected > 10 && raised > 10, "non-vacuous");
    }
}
