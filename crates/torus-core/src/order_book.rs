//! Order book matching engine — price-time priority CLOB.
//!
//! Central Limit Order Book with:
//! - Price-time priority matching (best price first, FIFO within level)
//! - Order types: Limit, Market, StopMarket, StopLimit, PostOnly
//! - Time-in-force: GTC, IOC, FOK
//! - O(1) cancel via order index, per-trader index for cancel-all
//! - Self-trade prevention (cancel-resting)
//! - All arithmetic via FixedPoint (no f64)
//! - Deterministic: same input sequence → same state

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::io::{self, Read as IoRead, Write as IoWrite};

use borsh::{BorshDeserialize, BorshSerialize};
use torus_types::{
    Address, FixedPoint, MarketId, OrderId, OrderType, PlaceOrderParams, Side, TimeInForce,
};

use crate::error::CoreError;
use crate::position::{borsh_read_address, borsh_read_fp, borsh_write_address, borsh_write_fp};

const MAX_ORDERS_PER_TRADER_PER_MARKET: usize = 200;

// ============================================================================
// Types
// ============================================================================

/// An order resting on the book.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Order {
    pub id: OrderId,
    pub trader: Address,
    pub side: Side,
    pub price: FixedPoint,
    pub remaining_qty: FixedPoint,
    pub original_qty: FixedPoint,
    pub order_type: OrderType,
    pub time_in_force: TimeInForce,
    pub timestamp: u64,
    pub reduce_only: bool,
    pub client_order_id: Option<u64>,
}

/// A fill (execution) between a maker and taker order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fill {
    pub maker_order_id: OrderId,
    pub taker_order_id: OrderId,
    pub price: FixedPoint,
    pub quantity: FixedPoint,
    pub maker: Address,
    pub taker: Address,
    pub maker_side: Side,
    pub timestamp: u64,
}

/// Result of placing an order.
#[derive(Clone, Debug)]
pub struct PlaceResult {
    pub order_id: OrderId,
    pub status: OrderStatus,
    pub fills: Vec<Fill>,
    /// A5: resting maker orders auto-cancelled by self-trade prevention,
    /// captured WHOLE (not just the id) so the executor can release the
    /// cancelled maker's remaining order-margin reservation
    /// (`price × remaining_qty` at cancel time).
    pub self_trade_cancels: Vec<Order>,
}

/// Order placement outcome.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OrderStatus {
    /// Completely filled.
    Filled,
    /// Partially filled, remainder resting on book.
    PartiallyFilled,
    /// No match, resting on book.
    Resting,
    /// Remainder cancelled (IOC/Market with partial or no fill).
    Cancelled,
    /// Rejected (PostOnly cross, FOK not fillable, empty book for Market).
    Rejected,
    /// Stop order pending trigger.
    PendingTrigger,
}

/// A pending stop order awaiting trigger.
#[derive(Clone, Debug)]
#[allow(dead_code)]
struct StopOrder {
    id: OrderId,
    trader: Address,
    market_id: MarketId,
    side: Side,
    trigger_price: FixedPoint,
    limit_price: Option<FixedPoint>,
    quantity: FixedPoint,
    time_in_force: TimeInForce,
    timestamp: u64,
    reduce_only: bool,
    client_order_id: Option<u64>,
}

/// Internal: where an order lives in the book.
#[derive(Clone, Debug)]
struct OrderLocation {
    side: Side,
    price: FixedPoint,
}

// ============================================================================
// L3 level-hash sponge cache (node-local, in-RAM only)
// ============================================================================
//
// See docs/design-levelhash-cache.md. Keccak absorbs sequentially and pads
// only at finalization, so for a level whose queue only APPENDED at the tail
// since the last save we can keep the un-finalized hasher state (post-absorb
// of the first `frame_count` frames), absorb just the new tail frames, and
// finalize a clone — a byte-identical digest in O(new frames) instead of
// O(depth). Validity of the cached prefix is proven by the per-level
// monotonic `level_epoch`: EVERY non-append queue mutation (front pop,
// mid-queue removal, in-place qty change — see the bump sites) increments it;
// an entry is usable only if its recorded epoch still matches. Any mismatch,
// or any doubt, falls back to the full rehash (today's exact behavior).
//
// The cache is never serialized, never part of any hash or row, and every
// book-rebuild path constructs a fresh OrderBook (empty cache + empty epoch
// map) — a dropped cache is only a cold start, never a correctness event.

/// Estimated in-RAM cost of one cache entry (keccak state ~200 B + block
/// buffer ~136 B + metadata + map overhead), used to convert a byte budget
/// into an entry cap.
const LEVEL_CACHE_ENTRY_COST: usize = 512;

struct LevelHashCacheEntry {
    /// `level_epoch` value observed when this state was absorbed.
    epoch: u64,
    /// Number of queue frames absorbed into `hasher`.
    frame_count: u32,
    /// Total bytes absorbed (bookkeeping / debug identity check).
    absorbed_len: u64,
    /// Sum of `remaining_qty` over the absorbed prefix, accumulated in the
    /// same front→back `+=` order as the one-shot path.
    prefix_qty: FixedPoint,
    /// Un-finalized keccak-256 sponge state after absorbing the prefix.
    hasher: sha3::Keccak256,
    /// LRU tick for eviction.
    last_used: u64,
}

/// Per-book incremental level-hash cache (L3, `TORUS_LEVEL_HASH_CACHE`).
struct LevelHashCache {
    entries: HashMap<(u8, i128), LevelHashCacheEntry>,
    max_entries: usize,
    tick: u64,
    hits: u64,
    misses: u64,
}

impl LevelHashCache {
    fn insert_bounded(&mut self, key: (u8, i128), entry: LevelHashCacheEntry) {
        if self.entries.len() >= self.max_entries && !self.entries.contains_key(&key) {
            // Evict the least-recently-used quarter in one batch (rare;
            // amortized cheap). Eviction is always safe — cold start only.
            let mut by_age: Vec<((u8, i128), u64)> = self
                .entries
                .iter()
                .map(|(k, e)| (*k, e.last_used))
                .collect();
            by_age.sort_unstable_by_key(|(_, t)| *t);
            let evict = (self.max_entries / 4).max(1);
            for (k, _) in by_age.into_iter().take(evict) {
                self.entries.remove(&k);
            }
        }
        self.entries.insert(key, entry);
    }
}

// ============================================================================
// OrderBook
// ============================================================================
//
// 3c journal-in-book (ported from the deep-book/level-rows lineage, replacing
// the rank8 `BookJournal`): every primitive that changes RESTING-order state
// journals the order id (`row_journal`) AND its `(side_tag, raw_price)` level
// (`level_journal`). Capture is at the OrderBook PRIMITIVES (insert_order /
// cancel_order / cancel_all / modify_order / match_at_level) — stop triggers,
// STP cancels and every executor path funnel through these, so no caller can
// bypass the journals. Pending stops are NOT journaled (the save path diffs
// the tiny stop set directly). The journals are in-memory bookkeeping only:
// never serialized, never part of any hash; the SAVE PATH decides what (if
// anything) to do with them per persistence mode.

/// Price-time priority Central Limit Order Book.
pub struct OrderBook {
    pub market_id: MarketId,
    /// Buy side: price → time-ordered queue. Best bid = last key (highest).
    bids: BTreeMap<FixedPoint, VecDeque<Order>>,
    /// Sell side: price → time-ordered queue. Best ask = first key (lowest).
    asks: BTreeMap<FixedPoint, VecDeque<Order>>,
    /// O(1) order lookup by ID → location.
    order_index: HashMap<OrderId, OrderLocation>,
    /// Per-trader order tracking for cancel-all.
    trader_orders: HashMap<Address, Vec<OrderId>>,
    /// Pending stop orders.
    pending_stops: Vec<StopOrder>,
    pub tick_size: FixedPoint,
    pub lot_size: FixedPoint,
    next_id: OrderId,
    last_trade_price: Option<FixedPoint>,
    /// Guard against recursive stop triggering.
    triggering_stops: bool,

    // ---- Per-order-row persistence state (journal-in-book, 3c) ----
    //
    // CONSENSUS-CRITICAL: intra-price-level queue order is NOT ascending
    // order-id (`modify_order` cancel+reinserts with the SAME id at the BACK
    // of the queue), so every resting order carries an explicit insertion
    // sequence, ASSIGNED AT INSERT TIME by the book. `next_seq` is persisted
    // in the meta row — recomputing it as max(seq)+1 at reload would assign
    // different seqs on a restarted node whenever cancels left a gap at the
    // top, forking the state root.
    /// Insertion sequence per resting order (assigned by `insert_order`,
    /// preserved by in-place modifies, reassigned on cancel+reinsert).
    order_seq: HashMap<OrderId, u64>,
    /// Monotonic seq allocator. Part of the persisted meta row (consensus).
    next_seq: u64,
    /// Row journal: order ids whose persisted row must be upserted (still
    /// resting) or deleted (gone) at the next save. Drained by `take_row_ops`.
    row_journal: BTreeSet<OrderId>,
    /// Order ids that currently have a persisted row — lets the save skip
    /// deletes for orders placed AND removed between two saves.
    row_exists: HashSet<OrderId>,
    /// Level journal: `(side_tag, raw_price)` of every price level touched
    /// since the last save. Drained by `take_level_ops`.
    level_journal: BTreeSet<(u8, i128)>,
    /// Levels that currently have a persisted level row (skip-useless-
    /// tombstones role, mirrors `row_exists`).
    level_exists: HashSet<(u8, i128)>,

    // ---- L3 level-hash sponge cache (node-local, in-RAM only) ----
    /// Per-level prefix-invalidation epoch: bumped by EVERY non-append queue
    /// mutation while the cache is enabled. Entries are NEVER removed for the
    /// lifetime of this book instance (a re-created level must not see a
    /// reset epoch). Never serialized.
    level_epoch: HashMap<(u8, i128), u64>,
    /// The cache itself. `None` = disabled (default; `take_level_ops` runs
    /// today's exact one-shot path). Never serialized.
    level_hash_cache: Option<Box<LevelHashCache>>,
}

impl OrderBook {
    pub fn new(market_id: MarketId, tick_size: FixedPoint, lot_size: FixedPoint) -> Self {
        Self {
            market_id,
            bids: BTreeMap::new(),
            asks: BTreeMap::new(),
            order_index: HashMap::new(),
            trader_orders: HashMap::new(),
            pending_stops: Vec::new(),
            tick_size,
            lot_size,
            next_id: 1,
            last_trade_price: None,
            triggering_stops: false,
            order_seq: HashMap::new(),
            next_seq: 1,
            row_journal: BTreeSet::new(),
            row_exists: HashSet::new(),
            level_journal: BTreeSet::new(),
            level_exists: HashSet::new(),
            level_epoch: HashMap::new(),
            level_hash_cache: None,
        }
    }

    fn alloc_id(&mut self) -> OrderId {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    // ========================================================================
    // Public API
    // ========================================================================

    /// Place an order. Main entry point dispatching by order type and TIF.
    pub fn place_order(
        &mut self,
        params: PlaceOrderParams,
        trader: Address,
        timestamp: u64,
    ) -> PlaceResult {
        let order_id = self.alloc_id();
        let side = if params.is_buy { Side::Buy } else { Side::Sell };

        // Dust order rejection (2.1b.2): qty must be >= lot_size
        if params.quantity < self.lot_size {
            return PlaceResult {
                order_id,
                status: OrderStatus::Rejected,
                fills: vec![],
                self_trade_cancels: vec![],
            };
        }

        // FIX 7 (ECON-FIND-10): Limit orders must have positive price
        if matches!(params.order_type, OrderType::Limit) && params.price <= FixedPoint::ZERO {
            return PlaceResult {
                order_id,
                status: OrderStatus::Rejected,
                fills: vec![],
                self_trade_cancels: vec![],
            };
        }

        // FIX 8 (ECON-FIND-11): Enforce tick size for limit orders
        if matches!(params.order_type, OrderType::Limit)
            && self.tick_size > FixedPoint::ZERO
            && params.price.raw() % self.tick_size.raw() != 0
        {
            return PlaceResult {
                order_id,
                status: OrderStatus::Rejected,
                fills: vec![],
                self_trade_cancels: vec![],
            };
        }

        // FIX 10 (ECON-FIND-17): Limit orders per trader per market
        let trader_order_count = self.trader_orders.get(&trader).map_or(0, |ids| ids.len());
        if trader_order_count >= MAX_ORDERS_PER_TRADER_PER_MARKET {
            return PlaceResult {
                order_id,
                status: OrderStatus::Rejected,
                fills: vec![],
                self_trade_cancels: vec![],
            };
        }

        // Stop orders → store in pending_stops
        match params.order_type {
            OrderType::StopMarket { trigger } => {
                // FIX 9 (ECON-FIND-12): Validate trigger direction
                if let Some(current_price) = self.last_trade_price {
                    let invalid_trigger = match side {
                        Side::Buy => trigger <= current_price,
                        Side::Sell => trigger >= current_price,
                    };
                    if invalid_trigger {
                        return PlaceResult {
                            order_id,
                            status: OrderStatus::Rejected,
                            fills: vec![],
                            self_trade_cancels: vec![],
                        };
                    }
                }
                self.pending_stops.push(StopOrder {
                    id: order_id,
                    trader,
                    market_id: params.market_id,
                    side,
                    trigger_price: trigger,
                    limit_price: None,
                    quantity: params.quantity,
                    time_in_force: params.time_in_force,
                    timestamp,
                    reduce_only: params.reduce_only,
                    client_order_id: params.client_order_id,
                });
                return PlaceResult {
                    order_id,
                    status: OrderStatus::PendingTrigger,
                    fills: vec![],
                    self_trade_cancels: vec![],
                };
            }
            OrderType::StopLimit { trigger, limit } => {
                // FIX 9 (ECON-FIND-12): Validate trigger direction
                if let Some(current_price) = self.last_trade_price {
                    let invalid_trigger = match side {
                        Side::Buy => trigger <= current_price,
                        Side::Sell => trigger >= current_price,
                    };
                    if invalid_trigger {
                        return PlaceResult {
                            order_id,
                            status: OrderStatus::Rejected,
                            fills: vec![],
                            self_trade_cancels: vec![],
                        };
                    }
                }
                self.pending_stops.push(StopOrder {
                    id: order_id,
                    trader,
                    market_id: params.market_id,
                    side,
                    trigger_price: trigger,
                    limit_price: Some(limit),
                    quantity: params.quantity,
                    time_in_force: params.time_in_force,
                    timestamp,
                    reduce_only: params.reduce_only,
                    client_order_id: params.client_order_id,
                });
                return PlaceResult {
                    order_id,
                    status: OrderStatus::PendingTrigger,
                    fills: vec![],
                    self_trade_cancels: vec![],
                };
            }
            _ => {}
        }

        // PostOnly: reject if would cross the spread
        if params.time_in_force == TimeInForce::PostOnly && self.would_cross(side, params.price) {
            return PlaceResult {
                order_id,
                status: OrderStatus::Rejected,
                fills: vec![],
                self_trade_cancels: vec![],
            };
        }

        let is_market = matches!(params.order_type, OrderType::Market);

        // Market order: reject if no liquidity
        if is_market {
            let has_liquidity = match side {
                Side::Buy => !self.asks.is_empty(),
                Side::Sell => !self.bids.is_empty(),
            };
            if !has_liquidity {
                return PlaceResult {
                    order_id,
                    status: OrderStatus::Rejected,
                    fills: vec![],
                    self_trade_cancels: vec![],
                };
            }
        }

        let mut order = Order {
            id: order_id,
            trader,
            side,
            price: params.price,
            remaining_qty: params.quantity,
            original_qty: params.quantity,
            order_type: params.order_type,
            time_in_force: params.time_in_force,
            timestamp,
            reduce_only: params.reduce_only,
            client_order_id: params.client_order_id,
        };

        // FOK: pre-check full fill availability
        if params.time_in_force == TimeInForce::FOK
            && !self.can_fill_completely(side, params.price, params.quantity, trader, is_market)
        {
            return PlaceResult {
                order_id,
                status: OrderStatus::Rejected,
                fills: vec![],
                self_trade_cancels: vec![],
            };
        }

        // Execute matching
        let (fills, self_trade_cancels) = self.execute_match(&mut order, is_market);

        if let Some(last_fill) = fills.last() {
            self.last_trade_price = Some(last_fill.price);
        }

        // Determine outcome
        let status = if order.remaining_qty == FixedPoint::ZERO {
            OrderStatus::Filled
        } else if is_market
            || params.time_in_force == TimeInForce::IOC
            || params.time_in_force == TimeInForce::FOK
        {
            OrderStatus::Cancelled
        } else {
            // GTC / PostOnly: rest remainder on book
            self.insert_order(order);
            if fills.is_empty() {
                OrderStatus::Resting
            } else {
                OrderStatus::PartiallyFilled
            }
        };

        // Trigger stops (non-recursive)
        if !fills.is_empty() && !self.triggering_stops {
            self.trigger_stops();
        }

        PlaceResult {
            order_id,
            status,
            fills,
            self_trade_cancels,
        }
    }

    /// Cancel an order by ID. Returns the cancelled order.
    pub fn cancel_order(&mut self, order_id: OrderId) -> Result<Order, CoreError> {
        let loc = self
            .order_index
            .remove(&order_id)
            .ok_or(CoreError::OrderNotFound(order_id))?;

        let book = match loc.side {
            Side::Buy => &mut self.bids,
            Side::Sell => &mut self.asks,
        };

        let queue = book
            .get_mut(&loc.price)
            .ok_or(CoreError::OrderNotFound(order_id))?;

        let pos = queue
            .iter()
            .position(|o| o.id == order_id)
            .ok_or(CoreError::OrderNotFound(order_id))?;

        let order = queue.remove(pos).unwrap();

        if queue.is_empty() {
            book.remove(&loc.price);
        }

        if let Some(ids) = self.trader_orders.get_mut(&order.trader) {
            ids.retain(|&id| id != order_id);
            if ids.is_empty() {
                self.trader_orders.remove(&order.trader);
            }
        }

        self.order_seq.remove(&order_id);
        self.row_journal.insert(order_id);
        self.level_journal
            .insert((crate::book_rows::side_tag(order.side), order.price.raw()));
        // L3: mid-queue removal invalidates any cached level-hash prefix.
        Self::bump_level_epoch(
            self.level_hash_cache.is_some(),
            &mut self.level_epoch,
            crate::book_rows::side_tag(order.side),
            order.price.raw(),
        );
        Ok(order)
    }

    /// Cancel all orders for a trader. Returns cancelled orders.
    pub fn cancel_all(&mut self, trader: Address, _market_id: Option<MarketId>) -> Vec<Order> {
        let order_ids = match self.trader_orders.remove(&trader) {
            Some(ids) => ids,
            None => return vec![],
        };

        let mut cancelled = Vec::with_capacity(order_ids.len());
        let cache_on = self.level_hash_cache.is_some();
        for order_id in order_ids {
            if let Some(loc) = self.order_index.remove(&order_id) {
                let book = match loc.side {
                    Side::Buy => &mut self.bids,
                    Side::Sell => &mut self.asks,
                };
                if let Some(queue) = book.get_mut(&loc.price) {
                    if let Some(pos) = queue.iter().position(|o| o.id == order_id) {
                        cancelled.push(queue.remove(pos).unwrap());
                        self.order_seq.remove(&order_id);
                        self.row_journal.insert(order_id);
                        self.level_journal
                            .insert((crate::book_rows::side_tag(loc.side), loc.price.raw()));
                        // L3: removal invalidates the cached level-hash prefix.
                        Self::bump_level_epoch(
                            cache_on,
                            &mut self.level_epoch,
                            crate::book_rows::side_tag(loc.side),
                            loc.price.raw(),
                        );
                    }
                    if queue.is_empty() {
                        book.remove(&loc.price);
                    }
                }
            }
        }

        // Also remove pending stops for this trader
        self.pending_stops.retain(|s| s.trader != trader);

        cancelled
    }

    /// Modify an order (cancel-and-replace).
    /// Qty decrease only: keeps time priority. Price change or qty increase: loses priority.
    pub fn modify_order(
        &mut self,
        order_id: OrderId,
        new_price: Option<FixedPoint>,
        new_qty: Option<FixedPoint>,
    ) -> Result<Order, CoreError> {
        // Qty-only decrease: modify in place (keeps time priority)
        if new_price.is_none() {
            if let Some(new_q) = new_qty {
                let loc = self
                    .order_index
                    .get(&order_id)
                    .ok_or(CoreError::OrderNotFound(order_id))?
                    .clone();

                let cache_on = self.level_hash_cache.is_some();
                let book = match loc.side {
                    Side::Buy => &mut self.bids,
                    Side::Sell => &mut self.asks,
                };
                if let Some(queue) = book.get_mut(&loc.price) {
                    if let Some(order) = queue.iter_mut().find(|o| o.id == order_id) {
                        if new_q > FixedPoint::ZERO && new_q < order.remaining_qty {
                            order.remaining_qty = new_q;
                            let out = order.clone();
                            // In-place change: same seq (priority kept), row rewritten.
                            self.row_journal.insert(order_id);
                            self.level_journal
                                .insert((crate::book_rows::side_tag(loc.side), loc.price.raw()));
                            // L3: in-place qty mutation changes an existing
                            // frame's bytes — invalidate the cached prefix.
                            Self::bump_level_epoch(
                                cache_on,
                                &mut self.level_epoch,
                                crate::book_rows::side_tag(loc.side),
                                loc.price.raw(),
                            );
                            return Ok(out);
                        }
                    }
                }
            }
        }

        // Cancel old, re-insert with updated fields (loses time priority)
        let old = self.cancel_order(order_id)?;

        let mut replacement = old;
        if let Some(p) = new_price {
            replacement.price = p;
        }
        if let Some(q) = new_qty {
            replacement.remaining_qty = q;
            // FIX 21 (ECON-FIND-28): Update original_qty when quantity increases
            if q > replacement.original_qty {
                replacement.original_qty = q;
            }
        }
        replacement.id = order_id;

        self.insert_order(replacement.clone());

        Ok(replacement)
    }

    /// Manually trigger pending stop orders.
    pub fn check_stops(&mut self) {
        self.trigger_stops();
    }

    // ========================================================================
    // Query Methods
    // ========================================================================

    /// Get the next order ID counter value.
    pub fn next_order_id(&self) -> OrderId {
        self.next_id
    }

    /// Set the next order ID counter (for global ID coordination across markets).
    pub fn set_next_order_id(&mut self, id: OrderId) {
        self.next_id = id;
    }

    /// Best (highest) bid price.
    pub fn best_bid(&self) -> Option<FixedPoint> {
        self.bids.keys().next_back().copied()
    }

    /// Best (lowest) ask price.
    pub fn best_ask(&self) -> Option<FixedPoint> {
        self.asks.keys().next().copied()
    }

    /// Spread = best_ask - best_bid.
    pub fn spread(&self) -> Option<FixedPoint> {
        match (self.best_ask(), self.best_bid()) {
            (Some(ask), Some(bid)) => Some(ask - bid),
            _ => None,
        }
    }

    /// Look up an order by ID.
    pub fn get_order(&self, order_id: OrderId) -> Option<&Order> {
        let loc = self.order_index.get(&order_id)?;
        let book = match loc.side {
            Side::Buy => &self.bids,
            Side::Sell => &self.asks,
        };
        book.get(&loc.price)?.iter().find(|o| o.id == order_id)
    }

    /// Total resting orders on the book.
    pub fn order_count(&self) -> usize {
        self.order_index.len()
    }

    /// Number of bid price levels.
    pub fn bid_levels(&self) -> usize {
        self.bids.len()
    }

    /// Number of ask price levels.
    pub fn ask_levels(&self) -> usize {
        self.asks.len()
    }

    /// Last trade price.
    pub fn last_trade_price(&self) -> Option<FixedPoint> {
        self.last_trade_price
    }

    /// All resting orders belonging to a specific trader.
    pub fn orders_for_trader(&self, trader: &Address) -> Vec<&Order> {
        match self.trader_orders.get(trader) {
            Some(ids) => ids.iter().filter_map(|id| self.get_order(*id)).collect(),
            None => vec![],
        }
    }

    /// Number of pending stop orders.
    pub fn pending_stop_count(&self) -> usize {
        self.pending_stops.len()
    }

    /// Aggregated bid depth, best (highest price) first:
    /// `(price, total remaining quantity, resting order count)` per level.
    /// Read-only view for RPC (S444: `torus_getOrderBook` reads the PROD
    /// `OrderBook` blob, not the test-only `OrderBookSnapshot`).
    pub fn bid_depth(&self) -> Vec<(FixedPoint, FixedPoint, usize)> {
        self.bids
            .iter()
            .rev()
            .map(|(price, q)| {
                let total = q
                    .iter()
                    .fold(FixedPoint::ZERO, |acc, o| acc + o.remaining_qty);
                (*price, total, q.len())
            })
            .collect()
    }

    /// Aggregated ask depth, best (lowest price) first — see [`Self::bid_depth`].
    pub fn ask_depth(&self) -> Vec<(FixedPoint, FixedPoint, usize)> {
        self.asks
            .iter()
            .map(|(price, q)| {
                let total = q
                    .iter()
                    .fold(FixedPoint::ZERO, |acc, o| acc + o.remaining_qty);
                (*price, total, q.len())
            })
            .collect()
    }

    /// Verify internal book invariants. Panics if any invariant is violated.
    /// Used by fuzz tests and determinism tests.
    pub fn verify_invariants(&self) {
        // Invariant 1: no crossed orders
        if let (Some(bid), Some(ask)) = (self.best_bid(), self.best_ask()) {
            assert!(bid < ask, "Crossed book: best_bid={bid} >= best_ask={ask}");
        }
        // Invariant 2: order_count matches actual orders in bids + asks
        let bid_orders: usize = self.bids.values().map(|q| q.len()).sum();
        let ask_orders: usize = self.asks.values().map(|q| q.len()).sum();
        assert_eq!(
            self.order_index.len(),
            bid_orders + ask_orders,
            "order_index({}) != bids({bid_orders}) + asks({ask_orders})",
            self.order_index.len()
        );
        // Invariant 3: all resting quantities > 0
        for queue in self.bids.values().chain(self.asks.values()) {
            for order in queue {
                assert!(
                    order.remaining_qty > FixedPoint::ZERO,
                    "Order {} has non-positive remaining qty",
                    order.id
                );
            }
        }
        // Invariant 4: no empty price levels
        for queue in self.bids.values().chain(self.asks.values()) {
            assert!(!queue.is_empty(), "Empty price level in book");
        }
    }

    // ========================================================================
    // Internal: Matching
    // ========================================================================

    /// Core matching: taker vs opposite side of the book.
    fn execute_match(&mut self, taker: &mut Order, is_market: bool) -> (Vec<Fill>, Vec<Order>) {
        let mut fills = Vec::new();
        let mut self_trade_cancels = Vec::new();
        let cache_on = self.level_hash_cache.is_some();

        match taker.side {
            Side::Buy => {
                while taker.remaining_qty > FixedPoint::ZERO {
                    let best_ask = match self.asks.keys().next().copied() {
                        Some(p) => p,
                        None => break,
                    };
                    if !is_market && best_ask > taker.price {
                        break;
                    }
                    let queue = self.asks.get_mut(&best_ask).unwrap();
                    Self::match_at_level(
                        taker,
                        queue,
                        best_ask,
                        Side::Sell,
                        &mut fills,
                        &mut self_trade_cancels,
                        &mut self.order_index,
                        &mut self.trader_orders,
                        &mut self.order_seq,
                        &mut self.row_journal,
                        &mut self.level_journal,
                        &mut self.level_epoch,
                        cache_on,
                    );
                    if self.asks.get(&best_ask).is_none_or(|q| q.is_empty()) {
                        self.asks.remove(&best_ask);
                    }
                }
            }
            Side::Sell => {
                while taker.remaining_qty > FixedPoint::ZERO {
                    let best_bid = match self.bids.keys().next_back().copied() {
                        Some(p) => p,
                        None => break,
                    };
                    if !is_market && best_bid < taker.price {
                        break;
                    }
                    let queue = self.bids.get_mut(&best_bid).unwrap();
                    Self::match_at_level(
                        taker,
                        queue,
                        best_bid,
                        Side::Buy,
                        &mut fills,
                        &mut self_trade_cancels,
                        &mut self.order_index,
                        &mut self.trader_orders,
                        &mut self.order_seq,
                        &mut self.row_journal,
                        &mut self.level_journal,
                        &mut self.level_epoch,
                        cache_on,
                    );
                    if self.bids.get(&best_bid).is_none_or(|q| q.is_empty()) {
                        self.bids.remove(&best_bid);
                    }
                }
            }
        }

        (fills, self_trade_cancels)
    }

    /// Match taker against orders at a single price level.
    /// Static method to satisfy the borrow checker (operates on disjoint fields).
    #[allow(clippy::too_many_arguments)]
    fn match_at_level(
        taker: &mut Order,
        queue: &mut VecDeque<Order>,
        price: FixedPoint,
        maker_side: Side,
        fills: &mut Vec<Fill>,
        self_trade_cancels: &mut Vec<Order>,
        order_index: &mut HashMap<OrderId, OrderLocation>,
        trader_orders: &mut HashMap<Address, Vec<OrderId>>,
        order_seq: &mut HashMap<OrderId, u64>,
        row_journal: &mut BTreeSet<OrderId>,
        level_journal: &mut BTreeSet<(u8, i128)>,
        level_epoch: &mut HashMap<(u8, i128), u64>,
        cache_on: bool,
    ) {
        // Every path below mutates this maker level (self-trade pop, partial
        // fill, full fill) — journal it once up front. Every such mutation
        // touches the FRONT of the queue, so the cached level-hash prefix is
        // invalidated at the same guard (L3).
        if taker.remaining_qty > FixedPoint::ZERO && !queue.is_empty() {
            level_journal.insert((crate::book_rows::side_tag(maker_side), price.raw()));
            Self::bump_level_epoch(
                cache_on,
                level_epoch,
                crate::book_rows::side_tag(maker_side),
                price.raw(),
            );
        }
        while taker.remaining_qty > FixedPoint::ZERO && !queue.is_empty() {
            let maker = queue.front().unwrap();

            // Self-trade prevention: cancel the resting (maker) order
            if maker.trader == taker.trader {
                let cancelled = queue.pop_front().unwrap();
                order_index.remove(&cancelled.id);
                if let Some(ids) = trader_orders.get_mut(&cancelled.trader) {
                    ids.retain(|&id| id != cancelled.id);
                }
                order_seq.remove(&cancelled.id);
                row_journal.insert(cancelled.id);
                // A5: hand the whole cancelled order back so the executor can
                // release its remaining order-margin reservation.
                self_trade_cancels.push(cancelled);
                continue;
            }

            // Capture maker info before mutable borrow
            let maker_id = maker.id;
            let maker_addr = maker.trader;
            let maker_side = maker.side;
            let maker_remaining = maker.remaining_qty;

            let fill_qty = taker.remaining_qty.min(maker_remaining);

            fills.push(Fill {
                maker_order_id: maker_id,
                taker_order_id: taker.id,
                price,
                quantity: fill_qty,
                maker: maker_addr,
                taker: taker.trader,
                maker_side,
                timestamp: taker.timestamp,
            });

            taker.remaining_qty -= fill_qty;

            let maker = queue.front_mut().unwrap();
            maker.remaining_qty -= fill_qty;

            // Maker row changed either way: partial fill rewrites it,
            // full fill deletes it.
            row_journal.insert(maker_id);
            if maker.remaining_qty == FixedPoint::ZERO {
                let filled = queue.pop_front().unwrap();
                order_index.remove(&filled.id);
                if let Some(ids) = trader_orders.get_mut(&filled.trader) {
                    ids.retain(|&id| id != filled.id);
                }
                order_seq.remove(&filled.id);
            }
        }
    }

    /// L3: bump a level's prefix-invalidation epoch. Called from EVERY
    /// non-append queue mutation site (front pop, mid-queue removal, in-place
    /// qty change). Gated on `cache_on` so the disabled state pays zero cost;
    /// entries are never removed for the book's lifetime (a re-created level
    /// must not see a reset epoch). Appends (`insert_order`) do NOT bump —
    /// they are the cacheable case.
    #[inline]
    fn bump_level_epoch(
        cache_on: bool,
        level_epoch: &mut HashMap<(u8, i128), u64>,
        tag: u8,
        raw_price: i128,
    ) {
        if cache_on {
            *level_epoch.entry((tag, raw_price)).or_insert(0) += 1;
        }
    }

    /// Insert an order into the book (at the back of its price level queue).
    /// Assigns the order's insertion sequence (queue-priority persistence)
    /// and journals its row + level.
    fn insert_order(&mut self, order: Order) {
        let side = order.side;
        let price = order.price;
        let id = order.id;
        let trader = order.trader;

        let book = match side {
            Side::Buy => &mut self.bids,
            Side::Sell => &mut self.asks,
        };
        book.entry(price).or_default().push_back(order);

        self.order_index.insert(id, OrderLocation { side, price });
        self.trader_orders.entry(trader).or_default().push(id);

        let seq = self.next_seq;
        self.next_seq += 1;
        self.order_seq.insert(id, seq);
        self.row_journal.insert(id);
        self.level_journal
            .insert((crate::book_rows::side_tag(side), price.raw()));
    }

    /// Would placing an order at `price` cross the spread?
    fn would_cross(&self, side: Side, price: FixedPoint) -> bool {
        match side {
            Side::Buy => self.best_ask().is_some_and(|ask| price >= ask),
            Side::Sell => self.best_bid().is_some_and(|bid| price <= bid),
        }
    }

    /// Read-only pre-check: can a FOK order be completely filled?
    fn can_fill_completely(
        &self,
        side: Side,
        price: FixedPoint,
        qty: FixedPoint,
        trader: Address,
        is_market: bool,
    ) -> bool {
        let mut remaining = qty;
        match side {
            Side::Buy => {
                for (&ask_price, queue) in &self.asks {
                    if !is_market && ask_price > price {
                        break;
                    }
                    for order in queue {
                        if order.trader == trader {
                            continue;
                        }
                        remaining -= remaining.min(order.remaining_qty);
                        if remaining <= FixedPoint::ZERO {
                            return true;
                        }
                    }
                }
            }
            Side::Sell => {
                for (&bid_price, queue) in self.bids.iter().rev() {
                    if !is_market && bid_price < price {
                        break;
                    }
                    for order in queue {
                        if order.trader == trader {
                            continue;
                        }
                        remaining -= remaining.min(order.remaining_qty);
                        if remaining <= FixedPoint::ZERO {
                            return true;
                        }
                    }
                }
            }
        }
        false
    }

    /// Trigger pending stop orders based on last trade price.
    fn trigger_stops(&mut self) {
        let price = match self.last_trade_price {
            Some(p) => p,
            None => return,
        };

        self.triggering_stops = true;

        for _ in 0..10 {
            let current_price = self.last_trade_price.unwrap_or(price);

            let mut triggered = Vec::new();
            self.pending_stops.retain(|stop| {
                let fire = match stop.side {
                    Side::Buy => current_price >= stop.trigger_price,
                    Side::Sell => current_price <= stop.trigger_price,
                };
                if fire {
                    triggered.push(stop.clone());
                    false
                } else {
                    true
                }
            });

            if triggered.is_empty() {
                break;
            }

            let prev_price = current_price;
            for stop in triggered {
                let (order_type, order_price) = match stop.limit_price {
                    None => (OrderType::Market, FixedPoint::ZERO),
                    Some(limit) => (OrderType::Limit, limit),
                };
                let params = PlaceOrderParams {
                    market_id: stop.market_id,
                    is_buy: stop.side == Side::Buy,
                    price: order_price,
                    quantity: stop.quantity,
                    order_type,
                    time_in_force: stop.time_in_force,
                    reduce_only: stop.reduce_only,
                    client_order_id: stop.client_order_id,
                };
                let result = self.place_order(params, stop.trader, stop.timestamp);
                if result.status == OrderStatus::Rejected {
                    tracing::warn!(
                        stop_id = stop.id,
                        trader = %stop.trader,
                        market_id = stop.market_id,
                        trigger_price = ?stop.trigger_price,
                        "stop order triggered but resulting order rejected"
                    );
                }
            }

            // No price change → no new triggers
            if self.last_trade_price == Some(prev_price) {
                break;
            }
        }

        self.triggering_stops = false;
    }
}

// ============================================================================
// C4 (perf/exec-scaleup): canonical read access + rebuild hooks for
// per-order-row book persistence (torus-bridge, `TORUS_BOOK_ROWS`).
//
// The row store persists each resting order / pending stop as its own KV row
// instead of one whole-book Borsh blob, so it needs (a) level-structured
// read access in the CANONICAL order (the exact order the whole-book Borsh
// serializer walks: bids ascending price then asks ascending price, FIFO
// within a level) and (b) rebuild hooks equivalent to what
// `BorshDeserialize for OrderBook` does internally. Nothing here can express
// a book state the classic (de)serializer couldn't.
// ============================================================================

impl OrderBook {
    /// Bid price levels ascending (the whole-book serializer's walk order),
    /// each with its FIFO queue (front = highest time priority).
    pub fn bid_queues(&self) -> impl Iterator<Item = (&FixedPoint, &VecDeque<Order>)> {
        self.bids.iter()
    }

    /// Ask price levels ascending, each with its FIFO queue.
    pub fn ask_queues(&self) -> impl Iterator<Item = (&FixedPoint, &VecDeque<Order>)> {
        self.asks.iter()
    }

    /// Set the last trade price during a persistence rebuild (the classic
    /// blob carries it in its header).
    pub fn set_last_trade_price(&mut self, ltp: Option<FixedPoint>) {
        self.last_trade_price = ltp;
    }

    /// Pending stop orders as `(id, borsh bytes)` in trigger (Vec) order.
    /// Stop ids are allocated monotonically and stops are only ever appended /
    /// removed (`Vec::retain` preserves relative order), so Vec order ==
    /// ascending id — the row store relies on this to rebuild trigger order
    /// from id-sorted rows. Debug-asserted here.
    pub fn stop_rows(&self) -> Vec<(OrderId, Vec<u8>)> {
        debug_assert!(
            self.pending_stops.windows(2).all(|w| w[0].id < w[1].id),
            "pending_stops must stay id-ascending (rebuild order invariant)"
        );
        self.pending_stops
            .iter()
            .map(|s| {
                (
                    s.id,
                    borsh::to_vec(s).expect("StopOrder borsh serialize cannot fail"),
                )
            })
            .collect()
    }

    /// rank8: the FIFO queue at one price level, if it exists. Read access for
    /// the journal-driven row differ (walks only affected levels).
    pub fn level_queue(&self, side: Side, price: FixedPoint) -> Option<&VecDeque<Order>> {
        match side {
            Side::Buy => self.bids.get(&price),
            Side::Sell => self.asks.get(&price),
        }
    }

    /// Rebuild one pending stop from its row bytes (see [`Self::stop_rows`]).
    /// Callers MUST append in ascending id order. Returns the stop's id.
    pub fn restore_stop_row(&mut self, bytes: &[u8]) -> io::Result<OrderId> {
        let stop = StopOrder::try_from_slice(bytes)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        if let Some(last) = self.pending_stops.last() {
            if stop.id <= last.id {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "stop rows must be restored in ascending id order",
                ));
            }
        }
        let id = stop.id;
        self.pending_stops.push(stop);
        Ok(id)
    }
}

// ============================================================================
// Journal-in-book persistence codec (3c)
//
// The row/level KEY layouts live in `crate::book_rows`; this block owns the
// VALUE encodings and journal draining, which need the book's private fields.
//
// STATE-ROOT PREIMAGE: order-row bytes (mode 1) and level-row bytes incl.
// `level_hash` (mode 2) are committed by the native state root. FROZEN once
// deployed.
// ============================================================================

impl OrderBook {
    /// The book-owned monotonic queue-seq allocator (persisted in the meta row).
    pub fn next_seq(&self) -> u64 {
        self.next_seq
    }

    /// Restore the seq allocator during a persistence rebuild.
    pub fn set_next_seq(&mut self, next_seq: u64) {
        self.next_seq = next_seq;
    }

    /// The insertion seq of a resting order (test/verify hook).
    pub fn order_seq_of(&self, order_id: OrderId) -> Option<u64> {
        self.order_seq.get(&order_id).copied()
    }

    /// Encode one resting order's row value: `seq(8 BE) ‖ borsh(Order)` (the
    /// frozen `Order` codec; the id is repeated inside for integrity).
    /// `None` if the order is not resting.
    pub fn encode_order_row(&self, order_id: OrderId) -> Option<Vec<u8>> {
        let seq = *self.order_seq.get(&order_id)?;
        let order = self.get_order(order_id)?;
        Some(Self::encode_order_row_parts(seq, order))
    }

    /// `seq(8 BE) ‖ borsh(Order)` from parts (shared with the level hasher).
    fn encode_order_row_parts(seq: u64, order: &Order) -> Vec<u8> {
        let mut w = Vec::with_capacity(8 + 112);
        w.extend_from_slice(&seq.to_be_bytes());
        order.serialize(&mut w).expect("vec write");
        w
    }

    /// Decode an order-row value into `(seq, order)`.
    pub fn decode_order_row(bytes: &[u8]) -> io::Result<(u64, Order)> {
        let mut r = bytes;
        let mut s = [0u8; 8];
        r.read_exact(&mut s)?;
        let seq = u64::from_be_bytes(s);
        let order = Order::deserialize_reader(&mut r)?;
        Ok((seq, order))
    }

    /// Insert an order loaded FROM a persisted row: restores its stored seq,
    /// does NOT journal (a load is not a mutation), and marks its row + level
    /// as persisted. Callers must insert in canonical order (bids ascending
    /// (price, seq), then asks) — the loader owns that ordering.
    pub fn insert_loaded_order(&mut self, order: Order, seq: u64) {
        let side = order.side;
        let price = order.price;
        let id = order.id;
        let trader = order.trader;

        let book = match side {
            Side::Buy => &mut self.bids,
            Side::Sell => &mut self.asks,
        };
        book.entry(price).or_default().push_back(order);
        self.order_index.insert(id, OrderLocation { side, price });
        self.trader_orders.entry(trader).or_default().push(id);
        self.order_seq.insert(id, seq);
        self.row_exists.insert(id);
        // A loaded order implies its level's persisted row exists (save-path
        // invariant) — mark it so a later emptying save deletes it.
        self.level_exists
            .insert((crate::book_rows::side_tag(side), price.raw()));
        debug_assert!(seq < self.next_seq, "loaded seq {seq} >= meta next_seq");
    }

    /// Drain the row journal into row ops, O(touched since last save):
    /// `(order_id, Some(row_bytes))` = upsert, `(order_id, None)` = delete.
    /// Ids placed AND removed since the last save (no row ever persisted)
    /// are skipped. Deterministic ascending-id order.
    pub fn take_row_ops(&mut self) -> Vec<(OrderId, Option<Vec<u8>>)> {
        let ids = std::mem::take(&mut self.row_journal);
        let mut ops = Vec::with_capacity(ids.len());
        for id in ids {
            if self.order_index.contains_key(&id) {
                let bytes = self
                    .encode_order_row(id)
                    .expect("resting order must encode");
                self.row_exists.insert(id);
                ops.push((id, Some(bytes)));
            } else if self.row_exists.remove(&id) {
                ops.push((id, None));
            }
        }
        ops
    }

    /// Row upserts for EVERY resting order (full write — genesis, tests,
    /// offline rebuild). Resets the journal and marks all rows persisted.
    /// Deterministic ascending-id order.
    pub fn full_row_ops(&mut self) -> Vec<(OrderId, Vec<u8>)> {
        self.row_journal.clear();
        self.row_exists.clear();
        let mut ids: Vec<OrderId> = self.order_index.keys().copied().collect();
        ids.sort_unstable();
        let mut ops = Vec::with_capacity(ids.len());
        for id in ids {
            let bytes = self
                .encode_order_row(id)
                .expect("resting order must encode");
            self.row_exists.insert(id);
            ops.push((id, bytes));
        }
        ops
    }

    /// Number of journaled (touched-since-last-save) rows — test/metrics hook.
    pub fn journaled_rows(&self) -> usize {
        self.row_journal.len()
    }

    /// The consensus aggregate of one price level (level-row VALUE parts), or
    /// `None` if the level has no resting orders. O(orders in the level):
    /// sums `remaining_qty` and keccaks the framed order rows front→back.
    pub fn level_row_data(&self, tag: u8, raw_price: i128) -> Option<crate::book_rows::LevelRowData> {
        let price = FixedPoint::from_raw(raw_price);
        let side_book = if tag == crate::book_rows::SIDE_TAG_BID {
            &self.bids
        } else {
            &self.asks
        };
        let queue = side_book.get(&price).filter(|q| !q.is_empty())?;
        let mut total = FixedPoint::ZERO;
        let mut preimage: Vec<u8> = Vec::with_capacity(queue.len() * 160);
        for order in queue {
            total += order.remaining_qty;
            let seq = *self
                .order_seq
                .get(&order.id)
                .expect("resting order must have a seq");
            let row = Self::encode_order_row_parts(seq, order);
            preimage.extend_from_slice(&(row.len() as u32).to_le_bytes());
            preimage.extend_from_slice(&row);
        }
        Some(crate::book_rows::LevelRowData {
            total_qty_raw: total.raw(),
            order_count: queue.len() as u32,
            level_hash: alloy_primitives::keccak256(&preimage).0,
        })
    }

    /// Drain the level journal into level-row ops, O(touched levels) journal
    /// work + O(orders in touched levels) hashing (or O(appended tail) per
    /// level when the L3 sponge cache holds a valid prefix):
    /// `((side_tag, raw_price), Some(data))` = upsert, `(_, None)` = delete.
    /// Levels touched-and-emptied with no persisted row are skipped (mirrors
    /// `take_row_ops`). Deterministic: BTreeSet drain order. Cache-on and
    /// cache-off produce BYTE-IDENTICAL data (the cache only changes how the
    /// same keccak digest is computed — docs/design-levelhash-cache.md).
    pub fn take_level_ops(
        &mut self,
    ) -> Vec<((u8, i128), Option<crate::book_rows::LevelRowData>)> {
        let keys = std::mem::take(&mut self.level_journal);
        // Move the cache out to sidestep the &self/&mut cache split borrow.
        let mut cache = self.level_hash_cache.take();
        let mut ops = Vec::with_capacity(keys.len());
        for key in keys {
            let (tag, raw_price) = key;
            let data = match cache.as_deref_mut() {
                Some(c) => self.level_row_data_cached(tag, raw_price, c),
                None => self.level_row_data(tag, raw_price),
            };
            match data {
                Some(data) => {
                    self.level_exists.insert(key);
                    ops.push((key, Some(data)));
                }
                None => {
                    // Emptied level: drop its cache entry (its epoch entry
                    // stays — see bump_level_epoch).
                    if let Some(c) = cache.as_deref_mut() {
                        c.entries.remove(&key);
                    }
                    if self.level_exists.remove(&key) {
                        ops.push((key, None));
                    }
                }
            }
        }
        self.level_hash_cache = cache;
        ops
    }

    /// L3 cached variant of [`Self::level_row_data`] — byte-identical output.
    /// HIT (epoch match): clone-extend the stored sponge state with only the
    /// appended tail frames. MISS/any doubt: full front→back absorb (the same
    /// byte stream as the one-shot path), re-seeding the entry.
    fn level_row_data_cached(
        &self,
        tag: u8,
        raw_price: i128,
        cache: &mut LevelHashCache,
    ) -> Option<crate::book_rows::LevelRowData> {
        use sha3::Digest;
        let price = FixedPoint::from_raw(raw_price);
        let side_book = if tag == crate::book_rows::SIDE_TAG_BID {
            &self.bids
        } else {
            &self.asks
        };
        let queue = side_book.get(&price).filter(|q| !q.is_empty())?;
        let key = (tag, raw_price);
        let epoch = self.level_epoch.get(&key).copied().unwrap_or(0);
        let n = queue.len();
        cache.tick += 1;
        let tick = cache.tick;

        if let Some(e) = cache.entries.get_mut(&key) {
            let start = e.frame_count as usize;
            if e.epoch == epoch && start <= n {
                // HIT: no invalidating op since the prefix was absorbed ⇒ the
                // first `start` frames are byte-identical to what the sponge
                // holds (design doc §1). Absorb only the appended tail.
                for order in queue.iter().skip(start) {
                    // Same FixedPoint += order as the one-shot sum.
                    e.prefix_qty += order.remaining_qty;
                    let seq = *self
                        .order_seq
                        .get(&order.id)
                        .expect("resting order must have a seq");
                    let row = Self::encode_order_row_parts(seq, order);
                    e.hasher.update((row.len() as u32).to_le_bytes());
                    e.hasher.update(&row);
                    e.absorbed_len += 4 + row.len() as u64;
                }
                e.frame_count = n as u32;
                e.last_used = tick;
                cache.hits += 1;
                let digest: [u8; 32] = e.hasher.clone().finalize().into();
                return Some(crate::book_rows::LevelRowData {
                    total_qty_raw: e.prefix_qty.raw(),
                    order_count: n as u32,
                    level_hash: digest,
                });
            }
            // Stale (epoch mismatch or impossible frame count): fall through
            // to the full rehash below; the entry is replaced.
        }

        // MISS: full absorb front→back — the identical byte stream the
        // one-shot `level_row_data` keccaks — keeping the un-finalized state.
        cache.misses += 1;
        let mut hasher = sha3::Keccak256::new();
        let mut total = FixedPoint::ZERO;
        let mut absorbed: u64 = 0;
        for order in queue {
            total += order.remaining_qty;
            let seq = *self
                .order_seq
                .get(&order.id)
                .expect("resting order must have a seq");
            let row = Self::encode_order_row_parts(seq, order);
            hasher.update((row.len() as u32).to_le_bytes());
            hasher.update(&row);
            absorbed += 4 + row.len() as u64;
        }
        let digest: [u8; 32] = hasher.clone().finalize().into();
        cache.insert_bounded(
            key,
            LevelHashCacheEntry {
                epoch,
                frame_count: n as u32,
                absorbed_len: absorbed,
                prefix_qty: total,
                hasher,
                last_used: tick,
            },
        );
        Some(crate::book_rows::LevelRowData {
            total_qty_raw: total.raw(),
            order_count: n as u32,
            level_hash: digest,
        })
    }

    /// L3: enable (or re-size) the level-hash sponge cache with a byte
    /// budget. `0` disables (drops) it — the default state; disabled =
    /// exact-today one-shot hashing. Node-local: output bytes are identical
    /// either way.
    pub fn ensure_level_hash_cache(&mut self, budget_bytes: usize) {
        if budget_bytes == 0 {
            self.level_hash_cache = None;
            return;
        }
        let max_entries = (budget_bytes / LEVEL_CACHE_ENTRY_COST).max(1);
        match &mut self.level_hash_cache {
            Some(c) => c.max_entries = max_entries,
            None => {
                self.level_hash_cache = Some(Box::new(LevelHashCache {
                    entries: HashMap::new(),
                    max_entries,
                    tick: 0,
                    hits: 0,
                    misses: 0,
                }))
            }
        }
    }

    /// L3: drop the level-hash cache (returns to exact-today hashing).
    pub fn disable_level_hash_cache(&mut self) {
        self.level_hash_cache = None;
    }

    /// L3 test/metrics hook: `(hits, misses, live entries)` if enabled.
    pub fn level_hash_cache_stats(&self) -> Option<(u64, u64, usize)> {
        self.level_hash_cache
            .as_ref()
            .map(|c| (c.hits, c.misses, c.entries.len()))
    }

    /// Modes 0/1 never persist level rows: drop the journaled level keys
    /// without computing any aggregates (no keccak spent).
    pub fn discard_level_ops(&mut self) {
        self.level_journal.clear();
    }

    /// Classic mode never persists per-order rows: drop journaled ids without
    /// encoding anything.
    pub fn discard_row_ops(&mut self) {
        self.row_journal.clear();
    }

    /// Level rows for EVERY non-empty level (full write — genesis, tests,
    /// offline rebuild). Resets the level journal and marks all level rows
    /// persisted. Deterministic: bids then asks, price ascending.
    pub fn full_level_ops(&mut self) -> Vec<((u8, i128), crate::book_rows::LevelRowData)> {
        self.level_journal.clear();
        self.level_exists.clear();
        // L3: full writes (genesis / offline rebuild) use the plain path;
        // drop any cached sponge states defensively.
        if let Some(c) = self.level_hash_cache.as_deref_mut() {
            c.entries.clear();
        }
        let keys: Vec<(u8, i128)> = self
            .bids
            .keys()
            .map(|p| (crate::book_rows::SIDE_TAG_BID, p.raw()))
            .chain(
                self.asks
                    .keys()
                    .map(|p| (crate::book_rows::SIDE_TAG_ASK, p.raw())),
            )
            .collect();
        let mut ops = Vec::with_capacity(keys.len());
        for key in keys {
            let data = self
                .level_row_data(key.0, key.1)
                .expect("non-empty level must aggregate");
            self.level_exists.insert(key);
            ops.push((key, data));
        }
        ops
    }
}

// ============================================================================
// FIX 1 (ECON-FIND-02): Borsh Serialization for OrderBook
// ============================================================================

impl BorshSerialize for Order {
    fn serialize<W: IoWrite>(&self, w: &mut W) -> io::Result<()> {
        w.write_all(&self.id.to_be_bytes())?;
        borsh_write_address(&self.trader, w)?;
        w.write_all(&[match self.side {
            Side::Buy => 0u8,
            Side::Sell => 1u8,
        }])?;
        borsh_write_fp(&self.price, w)?;
        borsh_write_fp(&self.remaining_qty, w)?;
        borsh_write_fp(&self.original_qty, w)?;
        // OrderType discriminant + variant data
        match &self.order_type {
            OrderType::Limit => w.write_all(&[0u8])?,
            OrderType::Market => w.write_all(&[1u8])?,
            OrderType::StopMarket { trigger } => {
                w.write_all(&[2u8])?;
                borsh_write_fp(trigger, w)?;
            }
            OrderType::StopLimit { trigger, limit } => {
                w.write_all(&[3u8])?;
                borsh_write_fp(trigger, w)?;
                borsh_write_fp(limit, w)?;
            }
        }
        // TimeInForce
        w.write_all(&[match self.time_in_force {
            TimeInForce::GTC => 0u8,
            TimeInForce::IOC => 1u8,
            TimeInForce::FOK => 2u8,
            TimeInForce::PostOnly => 3u8,
        }])?;
        w.write_all(&self.timestamp.to_be_bytes())?;
        w.write_all(&[u8::from(self.reduce_only)])?;
        // Option<u64>
        match self.client_order_id {
            None => w.write_all(&[0u8])?,
            Some(cid) => {
                w.write_all(&[1u8])?;
                w.write_all(&cid.to_be_bytes())?;
            }
        }
        Ok(())
    }
}

impl BorshDeserialize for Order {
    fn deserialize_reader<R: IoRead>(r: &mut R) -> io::Result<Self> {
        let mut id_buf = [0u8; 16];
        r.read_exact(&mut id_buf)?;
        let id = u128::from_be_bytes(id_buf);

        let trader = borsh_read_address(r)?;

        let mut side_buf = [0u8; 1];
        r.read_exact(&mut side_buf)?;
        let side = if side_buf[0] == 0 {
            Side::Buy
        } else {
            Side::Sell
        };

        let price = borsh_read_fp(r)?;
        let remaining_qty = borsh_read_fp(r)?;
        let original_qty = borsh_read_fp(r)?;

        let mut ot_buf = [0u8; 1];
        r.read_exact(&mut ot_buf)?;
        let order_type = match ot_buf[0] {
            0 => OrderType::Limit,
            1 => OrderType::Market,
            2 => {
                let trigger = borsh_read_fp(r)?;
                OrderType::StopMarket { trigger }
            }
            3 => {
                let trigger = borsh_read_fp(r)?;
                let limit = borsh_read_fp(r)?;
                OrderType::StopLimit { trigger, limit }
            }
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "invalid OrderType discriminant",
                ))
            }
        };

        let mut tif_buf = [0u8; 1];
        r.read_exact(&mut tif_buf)?;
        let time_in_force = match tif_buf[0] {
            0 => TimeInForce::GTC,
            1 => TimeInForce::IOC,
            2 => TimeInForce::FOK,
            3 => TimeInForce::PostOnly,
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "invalid TimeInForce discriminant",
                ))
            }
        };

        let mut ts_buf = [0u8; 8];
        r.read_exact(&mut ts_buf)?;
        let timestamp = u64::from_be_bytes(ts_buf);

        let mut ro_buf = [0u8; 1];
        r.read_exact(&mut ro_buf)?;
        let reduce_only = ro_buf[0] != 0;

        let mut cid_tag = [0u8; 1];
        r.read_exact(&mut cid_tag)?;
        let client_order_id = if cid_tag[0] == 0 {
            None
        } else {
            let mut cid_buf = [0u8; 8];
            r.read_exact(&mut cid_buf)?;
            Some(u64::from_be_bytes(cid_buf))
        };

        Ok(Order {
            id,
            trader,
            side,
            price,
            remaining_qty,
            original_qty,
            order_type,
            time_in_force,
            timestamp,
            reduce_only,
            client_order_id,
        })
    }
}

impl BorshSerialize for StopOrder {
    fn serialize<W: IoWrite>(&self, w: &mut W) -> io::Result<()> {
        w.write_all(&self.id.to_be_bytes())?;
        borsh_write_address(&self.trader, w)?;
        w.write_all(&self.market_id.to_be_bytes())?;
        w.write_all(&[match self.side {
            Side::Buy => 0u8,
            Side::Sell => 1u8,
        }])?;
        borsh_write_fp(&self.trigger_price, w)?;
        match &self.limit_price {
            None => w.write_all(&[0u8])?,
            Some(lp) => {
                w.write_all(&[1u8])?;
                borsh_write_fp(lp, w)?;
            }
        }
        borsh_write_fp(&self.quantity, w)?;
        w.write_all(&[match self.time_in_force {
            TimeInForce::GTC => 0u8,
            TimeInForce::IOC => 1u8,
            TimeInForce::FOK => 2u8,
            TimeInForce::PostOnly => 3u8,
        }])?;
        w.write_all(&self.timestamp.to_be_bytes())?;
        w.write_all(&[u8::from(self.reduce_only)])?;
        match self.client_order_id {
            None => w.write_all(&[0u8])?,
            Some(cid) => {
                w.write_all(&[1u8])?;
                w.write_all(&cid.to_be_bytes())?;
            }
        }
        Ok(())
    }
}

impl BorshDeserialize for StopOrder {
    fn deserialize_reader<R: IoRead>(r: &mut R) -> io::Result<Self> {
        let mut id_buf = [0u8; 16];
        r.read_exact(&mut id_buf)?;
        let id = u128::from_be_bytes(id_buf);

        let trader = borsh_read_address(r)?;

        let mut mid_buf = [0u8; 8];
        r.read_exact(&mut mid_buf)?;
        let market_id = u64::from_be_bytes(mid_buf);

        let mut side_buf = [0u8; 1];
        r.read_exact(&mut side_buf)?;
        let side = if side_buf[0] == 0 {
            Side::Buy
        } else {
            Side::Sell
        };

        let trigger_price = borsh_read_fp(r)?;

        let mut lp_tag = [0u8; 1];
        r.read_exact(&mut lp_tag)?;
        let limit_price = if lp_tag[0] == 0 {
            None
        } else {
            Some(borsh_read_fp(r)?)
        };

        let quantity = borsh_read_fp(r)?;

        let mut tif_buf = [0u8; 1];
        r.read_exact(&mut tif_buf)?;
        let time_in_force = match tif_buf[0] {
            0 => TimeInForce::GTC,
            1 => TimeInForce::IOC,
            2 => TimeInForce::FOK,
            3 => TimeInForce::PostOnly,
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "invalid TimeInForce discriminant",
                ))
            }
        };

        let mut ts_buf = [0u8; 8];
        r.read_exact(&mut ts_buf)?;
        let timestamp = u64::from_be_bytes(ts_buf);

        let mut ro_buf = [0u8; 1];
        r.read_exact(&mut ro_buf)?;
        let reduce_only = ro_buf[0] != 0;

        let mut cid_tag = [0u8; 1];
        r.read_exact(&mut cid_tag)?;
        let client_order_id = if cid_tag[0] == 0 {
            None
        } else {
            let mut cid_buf = [0u8; 8];
            r.read_exact(&mut cid_buf)?;
            Some(u64::from_be_bytes(cid_buf))
        };

        Ok(StopOrder {
            id,
            trader,
            market_id,
            side,
            trigger_price,
            limit_price,
            quantity,
            time_in_force,
            timestamp,
            reduce_only,
            client_order_id,
        })
    }
}

impl BorshSerialize for OrderBook {
    fn serialize<W: IoWrite>(&self, w: &mut W) -> io::Result<()> {
        w.write_all(&self.market_id.to_be_bytes())?;
        borsh_write_fp(&self.tick_size, w)?;
        borsh_write_fp(&self.lot_size, w)?;
        w.write_all(&self.next_id.to_be_bytes())?;

        // last_trade_price: Option<FixedPoint>
        match &self.last_trade_price {
            None => w.write_all(&[0u8])?,
            Some(ltp) => {
                w.write_all(&[1u8])?;
                borsh_write_fp(ltp, w)?;
            }
        }

        // Collect all resting orders from bids and asks
        let order_count: u32 = (self.bids.values().map(|q| q.len()).sum::<usize>()
            + self.asks.values().map(|q| q.len()).sum::<usize>())
            as u32;
        w.write_all(&order_count.to_be_bytes())?;

        // Write bids (price ascending from BTreeMap iteration, but order within level is FIFO)
        for queue in self.bids.values() {
            for order in queue {
                order.serialize(w)?;
            }
        }
        // Write asks
        for queue in self.asks.values() {
            for order in queue {
                order.serialize(w)?;
            }
        }

        // Write pending stops
        let stop_count = self.pending_stops.len() as u32;
        w.write_all(&stop_count.to_be_bytes())?;
        for stop in &self.pending_stops {
            stop.serialize(w)?;
        }

        Ok(())
    }
}

impl BorshDeserialize for OrderBook {
    fn deserialize_reader<R: IoRead>(r: &mut R) -> io::Result<Self> {
        let mut mid_buf = [0u8; 8];
        r.read_exact(&mut mid_buf)?;
        let market_id = u64::from_be_bytes(mid_buf);

        let tick_size = borsh_read_fp(r)?;
        let lot_size = borsh_read_fp(r)?;

        let mut nid_buf = [0u8; 16];
        r.read_exact(&mut nid_buf)?;
        let next_id = u128::from_be_bytes(nid_buf);

        let mut ltp_tag = [0u8; 1];
        r.read_exact(&mut ltp_tag)?;
        let last_trade_price = if ltp_tag[0] == 0 {
            None
        } else {
            Some(borsh_read_fp(r)?)
        };

        // Read orders and rebuild the book
        let mut oc_buf = [0u8; 4];
        r.read_exact(&mut oc_buf)?;
        let order_count = u32::from_be_bytes(oc_buf) as usize;

        let mut book = OrderBook {
            market_id,
            bids: BTreeMap::new(),
            asks: BTreeMap::new(),
            order_index: HashMap::new(),
            trader_orders: HashMap::new(),
            pending_stops: Vec::new(),
            tick_size,
            lot_size,
            next_id,
            last_trade_price,
            triggering_stops: false,
            order_seq: HashMap::new(),
            next_seq: 1,
            row_journal: BTreeSet::new(),
            row_exists: HashSet::new(),
            level_journal: BTreeSet::new(),
            level_exists: HashSet::new(),
            level_epoch: HashMap::new(),
            level_hash_cache: None,
        };

        for _ in 0..order_count {
            let order = Order::deserialize_reader(r)?;
            book.insert_order(order);
        }

        // Read pending stops
        let mut sc_buf = [0u8; 4];
        r.read_exact(&mut sc_buf)?;
        let stop_count = u32::from_be_bytes(sc_buf) as usize;

        for _ in 0..stop_count {
            let stop = StopOrder::deserialize_reader(r)?;
            book.pending_stops.push(stop);
        }

        // Deserializing is a LOAD, not a mutation: the insert_order calls
        // above journaled every order/level — clear that (classic mode never
        // drains the journals; leaving them populated only leaks memory).
        book.row_journal.clear();
        book.level_journal.clear();

        Ok(book)
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // Helpers
    fn fp(n: i64) -> FixedPoint {
        FixedPoint::from_raw(n as i128 * FixedPoint::SCALE)
    }

    fn fp_frac(whole: i64, frac_8: i64) -> FixedPoint {
        FixedPoint::from_raw(whole as i128 * FixedPoint::SCALE + frac_8 as i128)
    }

    fn addr(n: u8) -> Address {
        Address::from([n; 20])
    }

    fn book() -> OrderBook {
        OrderBook::new(1, fp_frac(0, 1), fp_frac(0, 1))
    }

    fn limit_buy(price: FixedPoint, qty: FixedPoint) -> PlaceOrderParams {
        PlaceOrderParams {
            market_id: 1,
            is_buy: true,
            price,
            quantity: qty,
            order_type: OrderType::Limit,
            time_in_force: TimeInForce::GTC,
            reduce_only: false,
            client_order_id: None,
        }
    }

    fn limit_sell(price: FixedPoint, qty: FixedPoint) -> PlaceOrderParams {
        PlaceOrderParams {
            market_id: 1,
            is_buy: false,
            price,
            quantity: qty,
            order_type: OrderType::Limit,
            time_in_force: TimeInForce::GTC,
            reduce_only: false,
            client_order_id: None,
        }
    }

    fn market_buy(qty: FixedPoint) -> PlaceOrderParams {
        PlaceOrderParams {
            market_id: 1,
            is_buy: true,
            price: FixedPoint::ZERO,
            quantity: qty,
            order_type: OrderType::Market,
            time_in_force: TimeInForce::GTC,
            reduce_only: false,
            client_order_id: None,
        }
    }

    fn market_sell(qty: FixedPoint) -> PlaceOrderParams {
        PlaceOrderParams {
            market_id: 1,
            is_buy: false,
            price: FixedPoint::ZERO,
            quantity: qty,
            order_type: OrderType::Market,
            time_in_force: TimeInForce::GTC,
            reduce_only: false,
            client_order_id: None,
        }
    }

    // ====================================================================
    // L3 level-hash cache — crypto-equivalence witnesses
    // ====================================================================

    /// The sponge-cache correctness axioms, checked as executable facts:
    /// (1) streaming sha3::Keccak256 over arbitrary split points equals the
    ///     one-shot alloy_primitives::keccak256 (same function, cross-impl);
    /// (2) a CLONED mid-absorb state, extended and finalized, equals the
    ///     one-shot digest of the full stream (clone+extend ≡ absorb-all),
    ///     and finalizing the clone does not perturb the original state.
    #[test]
    fn keccak_stream_clone_equivalence() {
        use sha3::Digest;
        let mut s = 0x1234_5678_9ABC_DEF0u64;
        let mut xs = || {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            s
        };
        for len in [0usize, 1, 7, 135, 136, 137, 200, 1000, 5000] {
            let data: Vec<u8> = (0..len).map(|_| (xs() & 0xFF) as u8).collect();
            let oneshot = alloy_primitives::keccak256(&data).0;
            // Random split points, including 0 and len.
            for _ in 0..8 {
                let cut = (xs() as usize) % (len + 1);
                let mut h = sha3::Keccak256::new();
                h.update(&data[..cut]);
                // Snapshot mid-absorb (the cached state), then extend both
                // the original and the snapshot with the tail.
                let mut snap = h.clone();
                h.update(&data[cut..]);
                snap.update(&data[cut..]);
                // Finalizing a CLONE of the extended state must not perturb
                // further use of the state itself.
                let d1: [u8; 32] = h.clone().finalize().into();
                let d2: [u8; 32] = snap.finalize().into();
                assert_eq!(d1, oneshot, "streamed != one-shot (len={len} cut={cut})");
                assert_eq!(d2, oneshot, "cloned-state != one-shot (len={len} cut={cut})");
                // The original is still usable and still agrees.
                let d3: [u8; 32] = h.finalize().into();
                assert_eq!(d3, oneshot, "post-clone original diverged");
            }
        }
    }

    /// Cache-on take_level_ops equals a plain recompute after append-heavy
    /// then invalidating ops (unit-level smoke; the exhaustive differential
    /// lives in tests/level_rows_core_tests.rs and torus-bridge).
    #[test]
    fn level_hash_cache_smoke_append_then_invalidate() {
        let mut ob = book();
        ob.ensure_level_hash_cache(1 << 20);
        // Seed and save (miss path seeds sponge states).
        for i in 0..5 {
            ob.place_order(limit_buy(fp(100), fp(1 + i)), addr(1), i as u64);
        }
        let ops1 = ob.take_level_ops();
        assert_eq!(ops1.len(), 1);
        // Append-only block → hit path.
        ob.place_order(limit_buy(fp(100), fp(9)), addr(2), 10);
        let ops2 = ob.take_level_ops();
        let (hits, _m, _l) = ob.level_hash_cache_stats().unwrap();
        assert_eq!(hits, 1, "tail append must hit the sponge cache");
        // Oracle: plain recompute (the frozen one-shot path).
        let plain = ob.level_row_data(crate::book_rows::SIDE_TAG_BID, fp(100).raw());
        assert_eq!(ops2[0].1, plain, "cached digest != one-shot digest");
        // Invalidate (cancel mid-queue) → full rehash, still identical.
        let victim = ob
            .level_queue(Side::Buy, fp(100))
            .unwrap()
            .get(2)
            .unwrap()
            .id;
        ob.cancel_order(victim).unwrap();
        let ops3 = ob.take_level_ops();
        let plain = ob.level_row_data(crate::book_rows::SIDE_TAG_BID, fp(100).raw());
        assert_eq!(ops3[0].1, plain, "post-invalidation digest != one-shot");
        let (hits, misses, _l) = ob.level_hash_cache_stats().unwrap();
        assert_eq!((hits, misses), (1, 2), "cancel must force the miss path");
    }

    // ====================================================================
    // 2.1.1 — Basic price-time priority matching
    // ====================================================================

    #[test]
    fn basic_buy_sell_match() {
        let mut ob = book();
        // Sell 10 @ 100
        let r1 = ob.place_order(limit_sell(fp(100), fp(10)), addr(1), 1);
        assert_eq!(r1.status, OrderStatus::Resting);
        assert_eq!(ob.order_count(), 1);

        // Buy 10 @ 100 → full match
        let r2 = ob.place_order(limit_buy(fp(100), fp(10)), addr(2), 2);
        assert_eq!(r2.status, OrderStatus::Filled);
        assert_eq!(r2.fills.len(), 1);
        assert_eq!(r2.fills[0].price, fp(100));
        assert_eq!(r2.fills[0].quantity, fp(10));
        assert_eq!(r2.fills[0].maker, addr(1));
        assert_eq!(r2.fills[0].taker, addr(2));
        assert_eq!(ob.order_count(), 0);
    }

    #[test]
    fn no_match_resting_order() {
        let mut ob = book();
        // Buy 10 @ 95, Sell 10 @ 100 → no match, both rest
        ob.place_order(limit_buy(fp(95), fp(10)), addr(1), 1);
        ob.place_order(limit_sell(fp(100), fp(10)), addr(2), 2);
        assert_eq!(ob.order_count(), 2);
        assert_eq!(ob.best_bid(), Some(fp(95)));
        assert_eq!(ob.best_ask(), Some(fp(100)));
        assert_eq!(ob.spread(), Some(fp(5)));
    }

    #[test]
    fn partial_fill() {
        let mut ob = book();
        ob.place_order(limit_sell(fp(100), fp(10)), addr(1), 1);

        // Buy 6 @ 100 → partial fill, maker has 4 remaining
        let r = ob.place_order(limit_buy(fp(100), fp(6)), addr(2), 2);
        assert_eq!(r.status, OrderStatus::Filled);
        assert_eq!(r.fills[0].quantity, fp(6));
        assert_eq!(ob.order_count(), 1);

        let maker = ob.get_order(1).unwrap();
        assert_eq!(maker.remaining_qty, fp(4));
    }

    #[test]
    fn multiple_fills_at_different_levels() {
        let mut ob = book();
        ob.place_order(limit_sell(fp(100), fp(5)), addr(1), 1);
        ob.place_order(limit_sell(fp(101), fp(5)), addr(2), 2);

        // Buy 10 @ 101 → fills both levels
        let r = ob.place_order(limit_buy(fp(101), fp(10)), addr(3), 3);
        assert_eq!(r.status, OrderStatus::Filled);
        assert_eq!(r.fills.len(), 2);
        assert_eq!(r.fills[0].price, fp(100));
        assert_eq!(r.fills[0].quantity, fp(5));
        assert_eq!(r.fills[1].price, fp(101));
        assert_eq!(r.fills[1].quantity, fp(5));
        assert_eq!(ob.order_count(), 0);
    }

    #[test]
    fn buy_partial_rest_on_book() {
        let mut ob = book();
        ob.place_order(limit_sell(fp(100), fp(5)), addr(1), 1);

        // Buy 10 @ 100 → fill 5, rest 5 on book
        let r = ob.place_order(limit_buy(fp(100), fp(10)), addr(2), 2);
        assert_eq!(r.status, OrderStatus::PartiallyFilled);
        assert_eq!(r.fills.len(), 1);
        assert_eq!(r.fills[0].quantity, fp(5));
        assert_eq!(ob.order_count(), 1);
        assert_eq!(ob.best_bid(), Some(fp(100)));
    }

    // ====================================================================
    // Price priority
    // ====================================================================

    #[test]
    fn price_priority_asks() {
        let mut ob = book();
        // Place asks at 102, 100, 101 (out of order)
        ob.place_order(limit_sell(fp(102), fp(5)), addr(1), 1);
        ob.place_order(limit_sell(fp(100), fp(5)), addr(2), 2);
        ob.place_order(limit_sell(fp(101), fp(5)), addr(3), 3);

        // Buy 5 → should fill at 100 (lowest ask first)
        let r = ob.place_order(limit_buy(fp(102), fp(5)), addr(4), 4);
        assert_eq!(r.fills[0].price, fp(100));
        assert_eq!(r.fills[0].maker, addr(2));
    }

    #[test]
    fn price_priority_bids() {
        let mut ob = book();
        ob.place_order(limit_buy(fp(98), fp(5)), addr(1), 1);
        ob.place_order(limit_buy(fp(100), fp(5)), addr(2), 2);
        ob.place_order(limit_buy(fp(99), fp(5)), addr(3), 3);

        // Sell 5 → should fill at 100 (highest bid first)
        let r = ob.place_order(limit_sell(fp(98), fp(5)), addr(4), 4);
        assert_eq!(r.fills[0].price, fp(100));
        assert_eq!(r.fills[0].maker, addr(2));
    }

    // ====================================================================
    // Time priority
    // ====================================================================

    #[test]
    fn time_priority_same_price() {
        let mut ob = book();
        // Two sells at same price, different times
        ob.place_order(limit_sell(fp(100), fp(5)), addr(1), 1);
        ob.place_order(limit_sell(fp(100), fp(5)), addr(2), 2);

        // Buy 5 → should fill against addr(1) (earlier timestamp)
        let r = ob.place_order(limit_buy(fp(100), fp(5)), addr(3), 3);
        assert_eq!(r.fills[0].maker, addr(1));
        // addr(2) still resting
        assert_eq!(ob.order_count(), 1);
        assert_eq!(ob.get_order(2).unwrap().trader, addr(2));
    }

    #[test]
    fn time_priority_fifo_across_fills() {
        let mut ob = book();
        ob.place_order(limit_sell(fp(100), fp(3)), addr(1), 1);
        ob.place_order(limit_sell(fp(100), fp(3)), addr(2), 2);
        ob.place_order(limit_sell(fp(100), fp(3)), addr(3), 3);

        // Buy 7 → fills addr(1) fully (3), addr(2) fully (3), addr(3) partial (1)
        let r = ob.place_order(limit_buy(fp(100), fp(7)), addr(4), 4);
        assert_eq!(r.fills.len(), 3);
        assert_eq!(r.fills[0].maker, addr(1));
        assert_eq!(r.fills[0].quantity, fp(3));
        assert_eq!(r.fills[1].maker, addr(2));
        assert_eq!(r.fills[1].quantity, fp(3));
        assert_eq!(r.fills[2].maker, addr(3));
        assert_eq!(r.fills[2].quantity, fp(1));
        assert_eq!(ob.get_order(3).unwrap().remaining_qty, fp(2));
    }

    // ====================================================================
    // 2.1.2 — Order types
    // ====================================================================

    #[test]
    fn market_buy_sweeps_book() {
        let mut ob = book();
        ob.place_order(limit_sell(fp(100), fp(3)), addr(1), 1);
        ob.place_order(limit_sell(fp(101), fp(3)), addr(2), 2);
        ob.place_order(limit_sell(fp(102), fp(3)), addr(3), 3);

        // Market buy 7 → sweeps first two levels fully, partial third
        let r = ob.place_order(market_buy(fp(7)), addr(4), 4);
        assert_eq!(r.status, OrderStatus::Filled);
        assert_eq!(r.fills.len(), 3);
        assert_eq!(r.fills[0].price, fp(100));
        assert_eq!(r.fills[1].price, fp(101));
        assert_eq!(r.fills[2].price, fp(102));
        assert_eq!(r.fills[2].quantity, fp(1));
    }

    #[test]
    fn market_sell_sweeps_bids() {
        let mut ob = book();
        ob.place_order(limit_buy(fp(100), fp(5)), addr(1), 1);
        ob.place_order(limit_buy(fp(99), fp(5)), addr(2), 2);

        let r = ob.place_order(market_sell(fp(8)), addr(3), 3);
        assert_eq!(r.fills.len(), 2);
        assert_eq!(r.fills[0].price, fp(100)); // highest bid first
        assert_eq!(r.fills[0].quantity, fp(5));
        assert_eq!(r.fills[1].price, fp(99));
        assert_eq!(r.fills[1].quantity, fp(3));
    }

    #[test]
    fn market_order_remainder_cancelled() {
        let mut ob = book();
        ob.place_order(limit_sell(fp(100), fp(3)), addr(1), 1);

        // Market buy 10, only 3 available → fill 3, cancel 7
        let r = ob.place_order(market_buy(fp(10)), addr(2), 2);
        assert_eq!(r.status, OrderStatus::Cancelled);
        assert_eq!(r.fills[0].quantity, fp(3));
        assert_eq!(ob.order_count(), 0);
    }

    #[test]
    fn market_order_empty_book_rejected() {
        let mut ob = book();
        let r = ob.place_order(market_buy(fp(10)), addr(1), 1);
        assert_eq!(r.status, OrderStatus::Rejected);
        assert!(r.fills.is_empty());
    }

    #[test]
    fn post_only_rests_when_no_cross() {
        let mut ob = book();
        ob.place_order(limit_sell(fp(100), fp(5)), addr(1), 1);

        // PostOnly buy @ 99 → no cross, rests
        let params = PlaceOrderParams {
            time_in_force: TimeInForce::PostOnly,
            ..limit_buy(fp(99), fp(5))
        };
        let r = ob.place_order(params, addr(2), 2);
        assert_eq!(r.status, OrderStatus::Resting);
        assert_eq!(ob.order_count(), 2);
    }

    #[test]
    fn post_only_rejected_when_crosses() {
        let mut ob = book();
        ob.place_order(limit_sell(fp(100), fp(5)), addr(1), 1);

        // PostOnly buy @ 100 → would cross, rejected
        let params = PlaceOrderParams {
            time_in_force: TimeInForce::PostOnly,
            ..limit_buy(fp(100), fp(5))
        };
        let r = ob.place_order(params, addr(2), 2);
        assert_eq!(r.status, OrderStatus::Rejected);
        assert!(r.fills.is_empty());
        assert_eq!(ob.order_count(), 1); // only the original sell
    }

    #[test]
    fn post_only_sell_rejected_when_crosses() {
        let mut ob = book();
        ob.place_order(limit_buy(fp(100), fp(5)), addr(1), 1);

        let params = PlaceOrderParams {
            time_in_force: TimeInForce::PostOnly,
            ..limit_sell(fp(100), fp(5))
        };
        let r = ob.place_order(params, addr(2), 2);
        assert_eq!(r.status, OrderStatus::Rejected);
    }

    #[test]
    fn stop_market_pending_then_triggered() {
        let mut ob = book();
        // Place a sell at 105 as liquidity
        ob.place_order(limit_sell(fp(105), fp(10)), addr(1), 1);
        // Place a buy at 95 as liquidity
        ob.place_order(limit_buy(fp(95), fp(10)), addr(2), 2);

        // Stop market buy: trigger at 100
        let params = PlaceOrderParams {
            market_id: 1,
            is_buy: true,
            price: FixedPoint::ZERO,
            quantity: fp(5),
            order_type: OrderType::StopMarket { trigger: fp(100) },
            time_in_force: TimeInForce::GTC,
            reduce_only: false,
            client_order_id: None,
        };
        let r = ob.place_order(params, addr(3), 3);
        assert_eq!(r.status, OrderStatus::PendingTrigger);
        assert_eq!(ob.pending_stop_count(), 1);

        // Trade at 100 to trigger the stop: sell at 95 matches the buy at 95
        // Actually we need a trade that sets last_trade_price >= 100
        // Let's do a buy that matches the sell at 105
        ob.place_order(limit_buy(fp(105), fp(1)), addr(4), 4);
        // This trade at 105 triggers the stop buy (trigger=100, 105>=100)
        // The stop converts to market buy, fills against remaining sell at 105
        assert_eq!(ob.pending_stop_count(), 0);
    }

    #[test]
    fn stop_limit_pending_then_triggered() {
        let mut ob = book();
        ob.place_order(limit_sell(fp(105), fp(10)), addr(1), 1);

        // Stop limit sell: trigger at 95, limit at 90
        let params = PlaceOrderParams {
            market_id: 1,
            is_buy: false,
            price: FixedPoint::ZERO,
            quantity: fp(5),
            order_type: OrderType::StopLimit {
                trigger: fp(95),
                limit: fp(90),
            },
            time_in_force: TimeInForce::GTC,
            reduce_only: false,
            client_order_id: None,
        };
        let r = ob.place_order(params, addr(2), 2);
        assert_eq!(r.status, OrderStatus::PendingTrigger);
        assert_eq!(ob.pending_stop_count(), 1);
    }

    // ====================================================================
    // 2.1.3 — Time-in-force
    // ====================================================================

    #[test]
    fn gtc_rests_on_book() {
        let mut ob = book();
        let r = ob.place_order(limit_buy(fp(100), fp(10)), addr(1), 1);
        assert_eq!(r.status, OrderStatus::Resting);
        assert_eq!(ob.order_count(), 1);
    }

    #[test]
    fn ioc_partial_fill_remainder_cancelled() {
        let mut ob = book();
        ob.place_order(limit_sell(fp(100), fp(3)), addr(1), 1);

        let params = PlaceOrderParams {
            time_in_force: TimeInForce::IOC,
            ..limit_buy(fp(100), fp(10))
        };
        let r = ob.place_order(params, addr(2), 2);
        assert_eq!(r.status, OrderStatus::Cancelled);
        assert_eq!(r.fills.len(), 1);
        assert_eq!(r.fills[0].quantity, fp(3));
        // Remainder (7) cancelled, not on book
        assert_eq!(ob.order_count(), 0);
    }

    #[test]
    fn ioc_no_match_cancelled() {
        let mut ob = book();
        let params = PlaceOrderParams {
            time_in_force: TimeInForce::IOC,
            ..limit_buy(fp(95), fp(10))
        };
        let r = ob.place_order(params, addr(1), 1);
        assert_eq!(r.status, OrderStatus::Cancelled);
        assert!(r.fills.is_empty());
        assert_eq!(ob.order_count(), 0);
    }

    #[test]
    fn fok_full_fill() {
        let mut ob = book();
        ob.place_order(limit_sell(fp(100), fp(10)), addr(1), 1);

        let params = PlaceOrderParams {
            time_in_force: TimeInForce::FOK,
            ..limit_buy(fp(100), fp(10))
        };
        let r = ob.place_order(params, addr(2), 2);
        assert_eq!(r.status, OrderStatus::Filled);
        assert_eq!(r.fills.len(), 1);
        assert_eq!(r.fills[0].quantity, fp(10));
    }

    #[test]
    fn fok_rejected_insufficient_qty() {
        let mut ob = book();
        ob.place_order(limit_sell(fp(100), fp(5)), addr(1), 1);

        let params = PlaceOrderParams {
            time_in_force: TimeInForce::FOK,
            ..limit_buy(fp(100), fp(10))
        };
        let r = ob.place_order(params, addr(2), 2);
        assert_eq!(r.status, OrderStatus::Rejected);
        assert!(r.fills.is_empty());
        // Maker order untouched
        assert_eq!(ob.order_count(), 1);
        assert_eq!(ob.get_order(1).unwrap().remaining_qty, fp(5));
    }

    #[test]
    fn fok_rejected_no_orders() {
        let mut ob = book();
        let params = PlaceOrderParams {
            time_in_force: TimeInForce::FOK,
            ..limit_buy(fp(100), fp(10))
        };
        let r = ob.place_order(params, addr(1), 1);
        assert_eq!(r.status, OrderStatus::Rejected);
    }

    #[test]
    fn fok_multi_level_fill() {
        let mut ob = book();
        ob.place_order(limit_sell(fp(100), fp(5)), addr(1), 1);
        ob.place_order(limit_sell(fp(101), fp(5)), addr(2), 2);

        let params = PlaceOrderParams {
            time_in_force: TimeInForce::FOK,
            ..limit_buy(fp(101), fp(10))
        };
        let r = ob.place_order(params, addr(3), 3);
        assert_eq!(r.status, OrderStatus::Filled);
        assert_eq!(r.fills.len(), 2);
    }

    // ====================================================================
    // 2.1.4 — Cancel and modify
    // ====================================================================

    #[test]
    fn cancel_existing_order() {
        let mut ob = book();
        let r = ob.place_order(limit_buy(fp(100), fp(10)), addr(1), 1);
        let order_id = r.order_id;

        let cancelled = ob.cancel_order(order_id).unwrap();
        assert_eq!(cancelled.id, order_id);
        assert_eq!(cancelled.remaining_qty, fp(10));
        assert_eq!(ob.order_count(), 0);
    }

    #[test]
    fn cancel_nonexistent_order() {
        let mut ob = book();
        let result = ob.cancel_order(999);
        assert!(result.is_err());
    }

    #[test]
    fn cancel_all_for_trader() {
        let mut ob = book();
        ob.place_order(limit_buy(fp(99), fp(5)), addr(1), 1);
        ob.place_order(limit_buy(fp(100), fp(5)), addr(1), 2);
        ob.place_order(limit_sell(fp(105), fp(5)), addr(1), 3);
        ob.place_order(limit_sell(fp(110), fp(5)), addr(2), 4);

        let cancelled = ob.cancel_all(addr(1), None);
        assert_eq!(cancelled.len(), 3);
        assert_eq!(ob.order_count(), 1); // only addr(2)'s order remains
    }

    #[test]
    fn cancel_all_empty() {
        let mut ob = book();
        let cancelled = ob.cancel_all(addr(1), None);
        assert!(cancelled.is_empty());
    }

    #[test]
    fn modify_qty_decrease_keeps_priority() {
        let mut ob = book();
        ob.place_order(limit_sell(fp(100), fp(10)), addr(1), 1);
        ob.place_order(limit_sell(fp(100), fp(10)), addr(2), 2);

        // Decrease addr(1)'s qty → should keep time priority (still first)
        ob.modify_order(1, None, Some(fp(5))).unwrap();

        // Buy matches addr(1) first (still has priority)
        let r = ob.place_order(limit_buy(fp(100), fp(5)), addr(3), 3);
        assert_eq!(r.fills[0].maker, addr(1));
        assert_eq!(r.fills[0].quantity, fp(5));
    }

    #[test]
    fn modify_price_loses_priority() {
        let mut ob = book();
        ob.place_order(limit_sell(fp(100), fp(10)), addr(1), 1);
        ob.place_order(limit_sell(fp(100), fp(10)), addr(2), 2);

        // Change addr(1)'s price → loses priority, goes to back
        ob.modify_order(1, Some(fp(100)), None).unwrap();

        // Buy matches addr(2) first (addr(1) lost priority)
        let r = ob.place_order(limit_buy(fp(100), fp(5)), addr(3), 3);
        assert_eq!(r.fills[0].maker, addr(2));
    }

    #[test]
    fn modify_nonexistent_order() {
        let mut ob = book();
        let result = ob.modify_order(999, Some(fp(100)), None);
        assert!(result.is_err());
    }

    #[test]
    fn modify_price_and_qty() {
        let mut ob = book();
        ob.place_order(limit_sell(fp(100), fp(10)), addr(1), 1);

        let modified = ob.modify_order(1, Some(fp(99)), Some(fp(8))).unwrap();
        assert_eq!(modified.price, fp(99));
        assert_eq!(modified.remaining_qty, fp(8));
        assert_eq!(ob.best_ask(), Some(fp(99)));
    }

    // ====================================================================
    // 2.1.5 — Comprehensive tests
    // ====================================================================

    // Self-trade prevention
    #[test]
    fn self_trade_prevention_cancel_resting() {
        let mut ob = book();
        // Trader 1 places a sell
        ob.place_order(limit_sell(fp(100), fp(10)), addr(1), 1);

        // Same trader places a buy → self-trade, maker cancelled, buyer rests
        let r = ob.place_order(limit_buy(fp(100), fp(5)), addr(1), 2);
        assert_eq!(r.self_trade_cancels.len(), 1);
        assert!(r.fills.is_empty());
        assert_eq!(r.status, OrderStatus::Resting);
        assert_eq!(ob.order_count(), 1); // maker cancelled, buy rests (GTC)
        assert_eq!(ob.best_bid(), Some(fp(100)));
    }

    #[test]
    fn self_trade_skips_to_next_maker() {
        let mut ob = book();
        ob.place_order(limit_sell(fp(100), fp(5)), addr(1), 1); // same as buyer
        ob.place_order(limit_sell(fp(100), fp(5)), addr(2), 2); // different

        // Trader 1 buys → skips own sell, fills against addr(2)
        let r = ob.place_order(limit_buy(fp(100), fp(5)), addr(1), 3);
        let cancelled_ids: Vec<_> = r.self_trade_cancels.iter().map(|o| o.id).collect();
        assert_eq!(cancelled_ids, vec![1]);
        assert_eq!(r.fills.len(), 1);
        assert_eq!(r.fills[0].maker, addr(2));
        assert_eq!(r.status, OrderStatus::Filled);
    }

    // Multi-level sweep
    #[test]
    fn market_buy_sweep_multiple_levels() {
        let mut ob = book();
        for i in 0..5 {
            ob.place_order(
                limit_sell(fp(100 + i as i64), fp(2)),
                addr(i + 1),
                i as u64 + 1,
            );
        }

        let r = ob.place_order(market_buy(fp(10)), addr(10), 10);
        assert_eq!(r.status, OrderStatus::Filled);
        assert_eq!(r.fills.len(), 5);
        for (i, fill) in r.fills.iter().enumerate() {
            assert_eq!(fill.price, fp(100 + i as i64));
            assert_eq!(fill.quantity, fp(2));
        }
        assert_eq!(ob.order_count(), 0);
    }

    #[test]
    fn market_sell_sweep_multiple_levels() {
        let mut ob = book();
        for i in 0..5 {
            ob.place_order(
                limit_buy(fp(100 - i as i64), fp(2)),
                addr(i + 1),
                i as u64 + 1,
            );
        }

        let r = ob.place_order(market_sell(fp(10)), addr(10), 10);
        assert_eq!(r.status, OrderStatus::Filled);
        assert_eq!(r.fills.len(), 5);
        // Highest bid filled first
        assert_eq!(r.fills[0].price, fp(100));
        assert_eq!(r.fills[4].price, fp(96));
    }

    // Edge cases
    #[test]
    fn minimum_quantity_fill() {
        let mut ob = book();
        let min = fp_frac(0, 1); // 0.00000001
        ob.place_order(limit_sell(fp(100), min), addr(1), 1);

        let r = ob.place_order(limit_buy(fp(100), min), addr(2), 2);
        assert_eq!(r.status, OrderStatus::Filled);
        assert_eq!(r.fills[0].quantity, min);
    }

    #[test]
    fn large_price_values() {
        let mut ob = book();
        let big = FixedPoint::from_raw(i64::MAX as i128 * FixedPoint::SCALE / 100);
        ob.place_order(limit_sell(big, fp(1)), addr(1), 1);

        let r = ob.place_order(limit_buy(big, fp(1)), addr(2), 2);
        assert_eq!(r.status, OrderStatus::Filled);
    }

    #[test]
    fn sell_matches_best_bid_only() {
        let mut ob = book();
        ob.place_order(limit_buy(fp(100), fp(5)), addr(1), 1);
        ob.place_order(limit_buy(fp(98), fp(5)), addr(2), 2);

        // Sell at 99 → matches 100 bid but not 98 bid
        let r = ob.place_order(limit_sell(fp(99), fp(5)), addr(3), 3);
        assert_eq!(r.fills.len(), 1);
        assert_eq!(r.fills[0].price, fp(100));
        assert_eq!(ob.order_count(), 1);
        assert_eq!(ob.best_bid(), Some(fp(98)));
    }

    // Book state consistency
    #[test]
    fn book_consistent_after_operations() {
        let mut ob = book();

        // Build up the book
        for i in 1..=5 {
            ob.place_order(limit_buy(fp(95 + i as i64), fp(10)), addr(i), i as u64);
        }
        for i in 6..=10 {
            ob.place_order(limit_sell(fp(100 + i as i64), fp(10)), addr(i), i as u64);
        }
        assert_eq!(ob.order_count(), 10);
        assert_eq!(ob.bid_levels(), 5);
        assert_eq!(ob.ask_levels(), 5);

        // Cancel some
        ob.cancel_order(1).unwrap();
        ob.cancel_order(6).unwrap();
        assert_eq!(ob.order_count(), 8);

        // Match some
        ob.place_order(market_sell(fp(25)), addr(11), 11);
        // Filled 25 from bids (best bids first)

        // Verify remaining state is consistent
        let bid_count: usize = ob.bids.values().map(|q| q.len()).sum();
        let ask_count: usize = ob.asks.values().map(|q| q.len()).sum();
        assert_eq!(ob.order_count(), bid_count + ask_count);

        // Every order in index has a valid location
        for (&id, loc) in &ob.order_index {
            let book = match loc.side {
                Side::Buy => &ob.bids,
                Side::Sell => &ob.asks,
            };
            assert!(book.get(&loc.price).unwrap().iter().any(|o| o.id == id));
        }
    }

    // Fill at maker's price
    #[test]
    fn fill_at_maker_price_not_taker() {
        let mut ob = book();
        ob.place_order(limit_sell(fp(100), fp(10)), addr(1), 1);

        // Buy at 105 → fills at 100 (maker's price)
        let r = ob.place_order(limit_buy(fp(105), fp(10)), addr(2), 2);
        assert_eq!(r.fills[0].price, fp(100));
    }

    #[test]
    fn sell_fill_at_maker_price() {
        let mut ob = book();
        ob.place_order(limit_buy(fp(100), fp(10)), addr(1), 1);

        // Sell at 95 → fills at 100 (maker's price)
        let r = ob.place_order(limit_sell(fp(95), fp(10)), addr(2), 2);
        assert_eq!(r.fills[0].price, fp(100));
    }

    // Multiple traders cancel-all
    #[test]
    fn cancel_all_leaves_other_traders() {
        let mut ob = book();
        ob.place_order(limit_buy(fp(99), fp(5)), addr(1), 1);
        ob.place_order(limit_buy(fp(100), fp(5)), addr(2), 2);
        ob.place_order(limit_sell(fp(105), fp(5)), addr(1), 3);

        ob.cancel_all(addr(1), None);
        assert_eq!(ob.order_count(), 1);
        // addr(2)'s order still there
        assert!(ob.get_order(2).is_some());
    }

    // IOC full fill
    #[test]
    fn ioc_full_fill() {
        let mut ob = book();
        ob.place_order(limit_sell(fp(100), fp(10)), addr(1), 1);

        let params = PlaceOrderParams {
            time_in_force: TimeInForce::IOC,
            ..limit_buy(fp(100), fp(10))
        };
        let r = ob.place_order(params, addr(2), 2);
        assert_eq!(r.status, OrderStatus::Filled);
        assert_eq!(r.fills.len(), 1);
    }

    // Order ID allocation
    #[test]
    fn order_ids_sequential() {
        let mut ob = book();
        let r1 = ob.place_order(limit_buy(fp(100), fp(10)), addr(1), 1);
        let r2 = ob.place_order(limit_buy(fp(99), fp(10)), addr(2), 2);
        let r3 = ob.place_order(limit_buy(fp(98), fp(10)), addr(3), 3);
        assert_eq!(r1.order_id, 1);
        assert_eq!(r2.order_id, 2);
        assert_eq!(r3.order_id, 3);
    }

    // Last trade price tracking
    #[test]
    fn last_trade_price_updates() {
        let mut ob = book();
        assert_eq!(ob.last_trade_price(), None);

        ob.place_order(limit_sell(fp(100), fp(5)), addr(1), 1);
        ob.place_order(limit_buy(fp(100), fp(5)), addr(2), 2);
        assert_eq!(ob.last_trade_price(), Some(fp(100)));

        ob.place_order(limit_sell(fp(101), fp(5)), addr(3), 3);
        ob.place_order(limit_buy(fp(101), fp(5)), addr(4), 4);
        assert_eq!(ob.last_trade_price(), Some(fp(101)));
    }

    // Cancel after partial fill
    #[test]
    fn cancel_partially_filled_order() {
        let mut ob = book();
        ob.place_order(limit_sell(fp(100), fp(10)), addr(1), 1);
        ob.place_order(limit_buy(fp(100), fp(3)), addr(2), 2);

        // Order 1 partially filled (7 remaining)
        let order = ob.get_order(1).unwrap();
        assert_eq!(order.remaining_qty, fp(7));

        // Cancel the remainder
        let cancelled = ob.cancel_order(1).unwrap();
        assert_eq!(cancelled.remaining_qty, fp(7));
        assert_eq!(ob.order_count(), 0);
    }

    // Determinism test
    #[test]
    fn deterministic_matching() {
        fn run_sequence() -> (
            Vec<PlaceResult>,
            usize,
            Option<FixedPoint>,
            Option<FixedPoint>,
        ) {
            let mut ob = book();
            let mut results = Vec::new();

            results.push(ob.place_order(limit_sell(fp(100), fp(10)), addr(1), 1));
            results.push(ob.place_order(limit_sell(fp(99), fp(5)), addr(2), 2));
            results.push(ob.place_order(limit_buy(fp(100), fp(8)), addr(3), 3));
            results.push(ob.place_order(limit_buy(fp(98), fp(3)), addr(4), 4));
            results.push(ob.place_order(market_sell(fp(2)), addr(5), 5));

            (results, ob.order_count(), ob.best_bid(), ob.best_ask())
        }

        let (r1, c1, bb1, ba1) = run_sequence();
        let (r2, c2, bb2, ba2) = run_sequence();

        assert_eq!(c1, c2);
        assert_eq!(bb1, bb2);
        assert_eq!(ba1, ba2);
        for (a, b) in r1.iter().zip(r2.iter()) {
            assert_eq!(a.order_id, b.order_id);
            assert_eq!(a.status, b.status);
            assert_eq!(a.fills, b.fills);
        }
    }

    // Mixed buy/sell at same price
    #[test]
    fn crossing_orders_immediate_match() {
        let mut ob = book();
        ob.place_order(limit_sell(fp(100), fp(5)), addr(1), 1);

        // Buy at 101 crosses the ask at 100
        let r = ob.place_order(limit_buy(fp(101), fp(5)), addr(2), 2);
        assert_eq!(r.status, OrderStatus::Filled);
        assert_eq!(r.fills[0].price, fp(100)); // fills at maker's price
    }

    // Query methods
    #[test]
    fn query_best_bid_ask_empty() {
        let ob = book();
        assert_eq!(ob.best_bid(), None);
        assert_eq!(ob.best_ask(), None);
        assert_eq!(ob.spread(), None);
        assert_eq!(ob.order_count(), 0);
    }

    #[test]
    fn query_spread() {
        let mut ob = book();
        ob.place_order(limit_buy(fp(99), fp(5)), addr(1), 1);
        ob.place_order(limit_sell(fp(101), fp(5)), addr(2), 2);
        assert_eq!(ob.spread(), Some(fp(2)));
    }

    // Fractional quantities
    #[test]
    fn fractional_quantity_matching() {
        let mut ob = book();
        let qty = fp_frac(1, 50_000_000); // 1.5
        ob.place_order(limit_sell(fp(100), qty), addr(1), 1);

        let r = ob.place_order(limit_buy(fp(100), qty), addr(2), 2);
        assert_eq!(r.status, OrderStatus::Filled);
        assert_eq!(r.fills[0].quantity, qty);
    }

    // Reduce-only flag preserved
    #[test]
    fn reduce_only_preserved() {
        let mut ob = book();
        let params = PlaceOrderParams {
            reduce_only: true,
            ..limit_buy(fp(100), fp(10))
        };
        let r = ob.place_order(params, addr(1), 1);
        let order = ob.get_order(r.order_id).unwrap();
        assert!(order.reduce_only);
    }

    // Client order ID preserved
    #[test]
    fn client_order_id_preserved() {
        let mut ob = book();
        let params = PlaceOrderParams {
            client_order_id: Some(42),
            ..limit_buy(fp(100), fp(10))
        };
        let r = ob.place_order(params, addr(1), 1);
        let order = ob.get_order(r.order_id).unwrap();
        assert_eq!(order.client_order_id, Some(42));
    }

    // Fill maker_side tracking
    #[test]
    fn fill_tracks_maker_side() {
        let mut ob = book();
        ob.place_order(limit_sell(fp(100), fp(10)), addr(1), 1);

        let r = ob.place_order(limit_buy(fp(100), fp(10)), addr(2), 2);
        assert_eq!(r.fills[0].maker_side, Side::Sell);

        let mut ob = book();
        ob.place_order(limit_buy(fp(100), fp(10)), addr(1), 1);
        let r = ob.place_order(limit_sell(fp(100), fp(10)), addr(2), 2);
        assert_eq!(r.fills[0].maker_side, Side::Buy);
    }

    // Empty book after all fills
    #[test]
    fn book_empty_after_full_sweep() {
        let mut ob = book();
        for i in 1..=10 {
            ob.place_order(limit_sell(fp(100 + i as i64), fp(1)), addr(i), i as u64);
        }
        assert_eq!(ob.order_count(), 10);

        ob.place_order(market_buy(fp(10)), addr(20), 20);
        assert_eq!(ob.order_count(), 0);
        assert!(ob.asks.is_empty());
        assert_eq!(ob.ask_levels(), 0);
    }

    // FOK with self-trade: pre-check should skip own orders
    #[test]
    fn fok_skips_self_trade_in_precheck() {
        let mut ob = book();
        // Trader 1 places sell of 5
        ob.place_order(limit_sell(fp(100), fp(5)), addr(1), 1);
        // Trader 2 places sell of 5
        ob.place_order(limit_sell(fp(100), fp(5)), addr(2), 2);

        // Trader 1 FOK buy of 5 → should skip own sell, fill against trader 2
        let params = PlaceOrderParams {
            time_in_force: TimeInForce::FOK,
            ..limit_buy(fp(100), fp(5))
        };
        let r = ob.place_order(params, addr(1), 3);
        assert_eq!(r.status, OrderStatus::Filled);
        assert_eq!(r.fills[0].maker, addr(2));
    }

    // FOK rejected because only self-trade available
    #[test]
    fn fok_rejected_only_self_trade() {
        let mut ob = book();
        ob.place_order(limit_sell(fp(100), fp(10)), addr(1), 1);

        let params = PlaceOrderParams {
            time_in_force: TimeInForce::FOK,
            ..limit_buy(fp(100), fp(10))
        };
        // Same trader → can't fill (self-trade prevention), so FOK rejects
        let r = ob.place_order(params, addr(1), 2);
        assert_eq!(r.status, OrderStatus::Rejected);
    }

    // Many orders at same price level
    #[test]
    fn many_orders_same_level() {
        let mut ob = book();
        for i in 1..=100u8 {
            ob.place_order(limit_sell(fp(100), fp(1)), addr(i), i as u64);
        }
        assert_eq!(ob.order_count(), 100);
        assert_eq!(ob.ask_levels(), 1);

        // Sweep 50
        let r = ob.place_order(market_buy(fp(50)), addr(200), 200);
        assert_eq!(r.fills.len(), 50);
        assert_eq!(ob.order_count(), 50);

        // Fills are in FIFO order
        for (i, fill) in r.fills.iter().enumerate() {
            assert_eq!(fill.maker, addr(i as u8 + 1));
        }
    }

    // Modify to different price level
    #[test]
    fn modify_to_different_price_level() {
        let mut ob = book();
        ob.place_order(limit_sell(fp(100), fp(10)), addr(1), 1);
        assert_eq!(ob.best_ask(), Some(fp(100)));

        ob.modify_order(1, Some(fp(99)), None).unwrap();
        assert_eq!(ob.best_ask(), Some(fp(99)));
        assert_eq!(ob.order_count(), 1);
    }

    // Cancel then re-place
    #[test]
    fn cancel_and_resubmit() {
        let mut ob = book();
        let r = ob.place_order(limit_buy(fp(100), fp(10)), addr(1), 1);
        ob.cancel_order(r.order_id).unwrap();
        assert_eq!(ob.order_count(), 0);

        // Re-place at different price
        let r2 = ob.place_order(limit_buy(fp(101), fp(10)), addr(1), 2);
        assert_eq!(ob.order_count(), 1);
        assert_eq!(ob.best_bid(), Some(fp(101)));
        assert_ne!(r.order_id, r2.order_id); // new ID
    }

    // ====================================================================
    // 2.1b.2 — Dust order rejection
    // ====================================================================

    #[test]
    fn dust_order_rejected() {
        // lot_size = 1.0, so 0.5 is dust
        let mut ob = OrderBook::new(1, fp_frac(0, 1), fp(1));
        let r = ob.place_order(limit_buy(fp(100), fp_frac(0, 50_000_000)), addr(1), 1);
        assert_eq!(r.status, OrderStatus::Rejected);
        assert!(r.fills.is_empty());
        assert_eq!(ob.order_count(), 0);
    }

    #[test]
    fn dust_order_exact_lot_size_accepted() {
        let mut ob = OrderBook::new(1, fp_frac(0, 1), fp(1));
        let r = ob.place_order(limit_buy(fp(100), fp(1)), addr(1), 1);
        assert_eq!(r.status, OrderStatus::Resting);
        assert_eq!(ob.order_count(), 1);
    }

    #[test]
    fn dust_stop_order_rejected() {
        let mut ob = OrderBook::new(1, fp_frac(0, 1), fp(1));
        let params = PlaceOrderParams {
            market_id: 1,
            is_buy: true,
            price: FixedPoint::ZERO,
            quantity: fp_frac(0, 50_000_000),
            order_type: OrderType::StopMarket { trigger: fp(100) },
            time_in_force: TimeInForce::GTC,
            reduce_only: false,
            client_order_id: None,
        };
        let r = ob.place_order(params, addr(1), 1);
        assert_eq!(r.status, OrderStatus::Rejected);
        assert_eq!(ob.pending_stop_count(), 0);
    }

    #[test]
    fn dust_market_order_rejected() {
        let mut ob = OrderBook::new(1, fp_frac(0, 1), fp(1));
        ob.place_order(limit_sell(fp(100), fp(10)), addr(1), 1);
        let r = ob.place_order(market_buy(fp_frac(0, 50_000_000)), addr(2), 2);
        assert_eq!(r.status, OrderStatus::Rejected);
    }

    // ====================================================================
    // FIX 7 — Price validation for limit orders
    // ====================================================================

    #[test]
    fn reject_zero_price_limit() {
        let mut ob = book();
        let r = ob.place_order(limit_buy(FixedPoint::ZERO, fp(10)), addr(1), 1);
        assert_eq!(r.status, OrderStatus::Rejected);
        assert_eq!(ob.order_count(), 0);
    }

    #[test]
    fn reject_negative_price_limit() {
        let mut ob = book();
        let r = ob.place_order(
            limit_buy(FixedPoint::from_raw(-100 * FixedPoint::SCALE), fp(10)),
            addr(1),
            1,
        );
        assert_eq!(r.status, OrderStatus::Rejected);
        assert_eq!(ob.order_count(), 0);
    }

    // ====================================================================
    // FIX 8 — Tick size enforcement
    // ====================================================================

    #[test]
    fn reject_tick_size_violation() {
        // Book with tick_size = 1.0 (fp(1))
        let mut ob = OrderBook::new(1, fp(1), fp_frac(0, 1));
        // Price 100.5 doesn't align to tick_size 1.0
        // raw = 100 * SCALE + SCALE/2 = 10050000000
        // tick_size raw = 1 * SCALE = 100000000
        // 10050000000 % 100000000 = 50000000 != 0 => rejected
        let r = ob.place_order(
            limit_buy(fp_frac(100, FixedPoint::SCALE as i64 / 2), fp(10)),
            addr(1),
            1,
        );
        assert_eq!(r.status, OrderStatus::Rejected);
    }

    #[test]
    fn accept_valid_tick_size() {
        let mut ob = OrderBook::new(1, fp(1), fp_frac(0, 1));
        let r = ob.place_order(limit_sell(fp(100), fp(10)), addr(1), 1);
        assert_eq!(r.status, OrderStatus::Resting);
    }

    // ====================================================================
    // FIX 9 — Stop order trigger direction validation
    // ====================================================================

    #[test]
    fn reject_buy_stop_below_market() {
        let mut ob = book();
        // Create a last trade price by executing a fill
        ob.place_order(limit_sell(fp(100), fp(5)), addr(1), 1);
        ob.place_order(market_buy(fp(5)), addr(2), 2);
        // last_trade_price is now 100

        // Buy stop with trigger at 90 (below current) should be rejected
        let r = ob.place_order(
            PlaceOrderParams {
                market_id: 1,
                is_buy: true,
                price: FixedPoint::ZERO,
                quantity: fp(5),
                order_type: OrderType::StopMarket { trigger: fp(90) },
                time_in_force: TimeInForce::GTC,
                reduce_only: false,
                client_order_id: None,
            },
            addr(3),
            3,
        );
        assert_eq!(r.status, OrderStatus::Rejected);
    }

    #[test]
    fn accept_buy_stop_above_market() {
        let mut ob = book();
        ob.place_order(limit_sell(fp(100), fp(5)), addr(1), 1);
        ob.place_order(market_buy(fp(5)), addr(2), 2);

        // Buy stop with trigger at 110 (above current) should be accepted
        let r = ob.place_order(
            PlaceOrderParams {
                market_id: 1,
                is_buy: true,
                price: FixedPoint::ZERO,
                quantity: fp(5),
                order_type: OrderType::StopMarket { trigger: fp(110) },
                time_in_force: TimeInForce::GTC,
                reduce_only: false,
                client_order_id: None,
            },
            addr(3),
            3,
        );
        assert_eq!(r.status, OrderStatus::PendingTrigger);
    }

    // ====================================================================
    // FIX 10 — Max orders per trader per market
    // ====================================================================

    #[test]
    fn reject_excess_orders_per_trader() {
        let mut ob = book();
        // Place MAX_ORDERS_PER_TRADER_PER_MARKET orders
        for i in 0..MAX_ORDERS_PER_TRADER_PER_MARKET {
            let price = fp(100) + FixedPoint::from_raw(i as i128 * FixedPoint::SCALE);
            let r = ob.place_order(limit_sell(price, fp(1)), addr(1), i as u64);
            assert_eq!(r.status, OrderStatus::Resting, "order {i} should rest");
        }
        // Next order should be rejected
        let r = ob.place_order(limit_sell(fp(500), fp(1)), addr(1), 999);
        assert_eq!(r.status, OrderStatus::Rejected);
        // Different trader can still place
        let r = ob.place_order(limit_sell(fp(500), fp(1)), addr(2), 999);
        assert_eq!(r.status, OrderStatus::Resting);
    }

    // ====================================================================
    // FIX 21 — original_qty updated on modify increase
    // ====================================================================

    #[test]
    fn modify_increase_updates_original_qty() {
        let mut ob = book();
        ob.place_order(limit_sell(fp(100), fp(10)), addr(1), 1);

        let modified = ob.modify_order(1, None, Some(fp(20))).unwrap();
        assert_eq!(modified.remaining_qty, fp(20));
        assert_eq!(modified.original_qty, fp(20)); // FIX 21: should be updated

        // Verify on book too
        let on_book = ob.get_order(1).unwrap();
        assert_eq!(on_book.original_qty, fp(20));
    }

    // ====================================================================
    // FIX 1 — Borsh serialization roundtrip
    // ====================================================================

    #[test]
    fn order_book_serialize_deserialize_roundtrip() {
        let mut ob = book();

        // Place some orders
        ob.place_order(limit_sell(fp(100), fp(10)), addr(1), 1);
        ob.place_order(limit_sell(fp(101), fp(5)), addr(2), 2);
        ob.place_order(limit_buy(fp(99), fp(8)), addr(3), 3);
        ob.place_order(limit_buy(fp(98), fp(3)), addr(4), 4);

        let original_count = ob.order_count();
        let original_bid = ob.best_bid();
        let original_ask = ob.best_ask();

        // Serialize
        let data = borsh::to_vec(&ob).unwrap();

        // Deserialize
        let restored: OrderBook = OrderBook::try_from_slice(&data).unwrap();

        assert_eq!(restored.order_count(), original_count);
        assert_eq!(restored.best_bid(), original_bid);
        assert_eq!(restored.best_ask(), original_ask);
        assert_eq!(restored.market_id, ob.market_id);

        // Verify specific orders
        assert!(restored.get_order(1).is_some());
        assert_eq!(restored.get_order(1).unwrap().remaining_qty, fp(10));
        assert!(restored.get_order(3).is_some());
        assert_eq!(restored.get_order(3).unwrap().remaining_qty, fp(8));
    }
}
