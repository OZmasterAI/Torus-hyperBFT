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

use std::collections::{BTreeMap, HashMap, VecDeque};

use torus_types::{
    Address, FixedPoint, MarketId, OrderId, OrderType, PlaceOrderParams, Side, TimeInForce,
};

use crate::error::CoreError;

// ============================================================================
// Types
// ============================================================================

/// An order resting on the book.
#[derive(Clone, Debug)]
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
    pub self_trade_cancels: Vec<OrderId>,
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
// OrderBook
// ============================================================================

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

        // Stop orders → store in pending_stops
        match params.order_type {
            OrderType::StopMarket { trigger } => {
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

        Ok(order)
    }

    /// Cancel all orders for a trader. Returns cancelled orders.
    pub fn cancel_all(&mut self, trader: Address, _market_id: Option<MarketId>) -> Vec<Order> {
        let order_ids = match self.trader_orders.remove(&trader) {
            Some(ids) => ids,
            None => return vec![],
        };

        let mut cancelled = Vec::with_capacity(order_ids.len());
        for order_id in order_ids {
            if let Some(loc) = self.order_index.remove(&order_id) {
                let book = match loc.side {
                    Side::Buy => &mut self.bids,
                    Side::Sell => &mut self.asks,
                };
                if let Some(queue) = book.get_mut(&loc.price) {
                    if let Some(pos) = queue.iter().position(|o| o.id == order_id) {
                        cancelled.push(queue.remove(pos).unwrap());
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

                let book = match loc.side {
                    Side::Buy => &mut self.bids,
                    Side::Sell => &mut self.asks,
                };
                if let Some(queue) = book.get_mut(&loc.price) {
                    if let Some(order) = queue.iter_mut().find(|o| o.id == order_id) {
                        if new_q > FixedPoint::ZERO && new_q < order.remaining_qty {
                            order.remaining_qty = new_q;
                            return Ok(order.clone());
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

    /// Number of pending stop orders.
    pub fn pending_stop_count(&self) -> usize {
        self.pending_stops.len()
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
    fn execute_match(&mut self, taker: &mut Order, is_market: bool) -> (Vec<Fill>, Vec<OrderId>) {
        let mut fills = Vec::new();
        let mut self_trade_cancels = Vec::new();

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
                        &mut fills,
                        &mut self_trade_cancels,
                        &mut self.order_index,
                        &mut self.trader_orders,
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
                        &mut fills,
                        &mut self_trade_cancels,
                        &mut self.order_index,
                        &mut self.trader_orders,
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
    fn match_at_level(
        taker: &mut Order,
        queue: &mut VecDeque<Order>,
        price: FixedPoint,
        fills: &mut Vec<Fill>,
        self_trade_cancels: &mut Vec<OrderId>,
        order_index: &mut HashMap<OrderId, OrderLocation>,
        trader_orders: &mut HashMap<Address, Vec<OrderId>>,
    ) {
        while taker.remaining_qty > FixedPoint::ZERO && !queue.is_empty() {
            let maker = queue.front().unwrap();

            // Self-trade prevention: cancel the resting (maker) order
            if maker.trader == taker.trader {
                let cancelled = queue.pop_front().unwrap();
                order_index.remove(&cancelled.id);
                if let Some(ids) = trader_orders.get_mut(&cancelled.trader) {
                    ids.retain(|&id| id != cancelled.id);
                }
                self_trade_cancels.push(cancelled.id);
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

            if maker.remaining_qty == FixedPoint::ZERO {
                let filled = queue.pop_front().unwrap();
                order_index.remove(&filled.id);
                if let Some(ids) = trader_orders.get_mut(&filled.trader) {
                    ids.retain(|&id| id != filled.id);
                }
            }
        }
    }

    /// Insert an order into the book (at the back of its price level queue).
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
                let _ = self.place_order(params, stop.trader, stop.timestamp);
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
        assert_eq!(r.self_trade_cancels, vec![1]);
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
}
