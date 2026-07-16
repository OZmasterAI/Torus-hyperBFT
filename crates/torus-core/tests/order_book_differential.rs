//! Rank-10 (Package D Wave-2): differential property test for the
//! matching-core micro-architecture changes (FxHash indices, single-descent
//! price levels, swap_remove trader back-index).
//!
//! HARD REQUIREMENT: book state, fills, and serialized bytes must stay
//! byte-identical. The `reference` module below is a verbatim frozen copy of
//! the PRE-CHANGE `OrderBook` implementation (trimmed of logging); randomized
//! order flows are driven into both implementations in lockstep and every
//! output is compared:
//!   - place_order: order_id, status, fills (exact sequence), and
//!     self_trade_cancels (exact sequence)
//!   - cancel_order / modify_order: result orders / error-ness
//!   - cancel_all: returned orders compared as an id-sorted multiset
//!     (the trader-index Vec order is NOT part of the contract — its only
//!     consensus consumer folds a commutative checked sum, see
//!     native_executor.rs exec_cancel_all)
//!   - after EVERY op: full Borsh-serialized book bytes must be identical,
//!     and the production book's invariants must hold.

use borsh::to_vec;
use proptest::prelude::*;
use torus_core::order_book::OrderBook;
use torus_types::{Address, FixedPoint, OrderType, PlaceOrderParams, Side, TimeInForce};

// ============================================================================
// Frozen reference implementation (pre-change order_book.rs, verbatim except:
// module-local borsh helpers, tracing call removed, tests removed)
// ============================================================================
mod reference {
    use std::collections::{BTreeMap, HashMap, VecDeque};
    use std::io::{self, Write as IoWrite};

    use torus_types::{
        Address, FixedPoint, MarketId, OrderId, OrderType, PlaceOrderParams, Side, TimeInForce,
    };

    const MAX_ORDERS_PER_TRADER_PER_MARKET: usize = 200;

    fn borsh_write_fp<W: IoWrite>(val: &FixedPoint, w: &mut W) -> io::Result<()> {
        w.write_all(&val.raw().to_be_bytes())
    }

    fn borsh_write_address<W: IoWrite>(addr: &Address, w: &mut W) -> io::Result<()> {
        w.write_all(addr.as_slice())
    }

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

    #[derive(Clone, Debug)]
    pub struct PlaceResult {
        pub order_id: OrderId,
        pub status: OrderStatus,
        pub fills: Vec<Fill>,
        pub self_trade_cancels: Vec<Order>,
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    pub enum OrderStatus {
        Filled,
        PartiallyFilled,
        Resting,
        Cancelled,
        Rejected,
        PendingTrigger,
    }

    #[derive(Clone, Debug)]
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

    #[derive(Clone, Debug)]
    struct OrderLocation {
        side: Side,
        price: FixedPoint,
    }

    pub struct OrderBook {
        pub market_id: MarketId,
        bids: BTreeMap<FixedPoint, VecDeque<Order>>,
        asks: BTreeMap<FixedPoint, VecDeque<Order>>,
        order_index: HashMap<OrderId, OrderLocation>,
        trader_orders: HashMap<Address, Vec<OrderId>>,
        pending_stops: Vec<StopOrder>,
        pub tick_size: FixedPoint,
        pub lot_size: FixedPoint,
        next_id: OrderId,
        last_trade_price: Option<FixedPoint>,
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

        pub fn place_order(
            &mut self,
            params: PlaceOrderParams,
            trader: Address,
            timestamp: u64,
        ) -> PlaceResult {
            let order_id = self.alloc_id();
            let side = if params.is_buy { Side::Buy } else { Side::Sell };

            if params.quantity < self.lot_size {
                return PlaceResult {
                    order_id,
                    status: OrderStatus::Rejected,
                    fills: vec![],
                    self_trade_cancels: vec![],
                };
            }

            if matches!(params.order_type, OrderType::Limit) && params.price <= FixedPoint::ZERO {
                return PlaceResult {
                    order_id,
                    status: OrderStatus::Rejected,
                    fills: vec![],
                    self_trade_cancels: vec![],
                };
            }

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

            let trader_order_count = self.trader_orders.get(&trader).map_or(0, |ids| ids.len());
            if trader_order_count >= MAX_ORDERS_PER_TRADER_PER_MARKET {
                return PlaceResult {
                    order_id,
                    status: OrderStatus::Rejected,
                    fills: vec![],
                    self_trade_cancels: vec![],
                };
            }

            match params.order_type {
                OrderType::StopMarket { trigger } => {
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

            if params.time_in_force == TimeInForce::PostOnly
                && self.would_cross(side, params.price)
            {
                return PlaceResult {
                    order_id,
                    status: OrderStatus::Rejected,
                    fills: vec![],
                    self_trade_cancels: vec![],
                };
            }

            let is_market = matches!(params.order_type, OrderType::Market);

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

            let (fills, self_trade_cancels) = self.execute_match(&mut order, is_market);

            if let Some(last_fill) = fills.last() {
                self.last_trade_price = Some(last_fill.price);
            }

            let status = if order.remaining_qty == FixedPoint::ZERO {
                OrderStatus::Filled
            } else if is_market
                || params.time_in_force == TimeInForce::IOC
                || params.time_in_force == TimeInForce::FOK
            {
                OrderStatus::Cancelled
            } else {
                self.insert_order(order);
                if fills.is_empty() {
                    OrderStatus::Resting
                } else {
                    OrderStatus::PartiallyFilled
                }
            };

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

        pub fn cancel_order(&mut self, order_id: OrderId) -> Result<Order, ()> {
            let loc = self.order_index.remove(&order_id).ok_or(())?;

            let book = match loc.side {
                Side::Buy => &mut self.bids,
                Side::Sell => &mut self.asks,
            };

            let queue = book.get_mut(&loc.price).ok_or(())?;

            let pos = queue.iter().position(|o| o.id == order_id).ok_or(())?;

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

            self.pending_stops.retain(|s| s.trader != trader);

            cancelled
        }

        pub fn modify_order(
            &mut self,
            order_id: OrderId,
            new_price: Option<FixedPoint>,
            new_qty: Option<FixedPoint>,
        ) -> Result<Order, ()> {
            if new_price.is_none() {
                if let Some(new_q) = new_qty {
                    let loc = self.order_index.get(&order_id).ok_or(())?.clone();

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

            let old = self.cancel_order(order_id)?;

            let mut replacement = old;
            if let Some(p) = new_price {
                replacement.price = p;
            }
            if let Some(q) = new_qty {
                replacement.remaining_qty = q;
                if q > replacement.original_qty {
                    replacement.original_qty = q;
                }
            }
            replacement.id = order_id;

            self.insert_order(replacement.clone());

            Ok(replacement)
        }

        pub fn check_stops(&mut self) {
            self.trigger_stops();
        }

        pub fn best_bid(&self) -> Option<FixedPoint> {
            self.bids.keys().next_back().copied()
        }

        pub fn best_ask(&self) -> Option<FixedPoint> {
            self.asks.keys().next().copied()
        }

        fn execute_match(&mut self, taker: &mut Order, is_market: bool) -> (Vec<Fill>, Vec<Order>) {
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

        fn match_at_level(
            taker: &mut Order,
            queue: &mut VecDeque<Order>,
            price: FixedPoint,
            fills: &mut Vec<Fill>,
            self_trade_cancels: &mut Vec<Order>,
            order_index: &mut HashMap<OrderId, OrderLocation>,
            trader_orders: &mut HashMap<Address, Vec<OrderId>>,
        ) {
            while taker.remaining_qty > FixedPoint::ZERO && !queue.is_empty() {
                let maker = queue.front().unwrap();

                if maker.trader == taker.trader {
                    let cancelled = queue.pop_front().unwrap();
                    order_index.remove(&cancelled.id);
                    if let Some(ids) = trader_orders.get_mut(&cancelled.trader) {
                        ids.retain(|&id| id != cancelled.id);
                    }
                    self_trade_cancels.push(cancelled);
                    continue;
                }

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

        fn would_cross(&self, side: Side, price: FixedPoint) -> bool {
            match side {
                Side::Buy => self.best_ask().is_some_and(|ask| price >= ask),
                Side::Sell => self.best_bid().is_some_and(|bid| price <= bid),
            }
        }

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

                if self.last_trade_price == Some(prev_price) {
                    break;
                }
            }

            self.triggering_stops = false;
        }

        // ---- serialization (verbatim byte format of the production impl) ----

        fn serialize_order<W: IoWrite>(order: &Order, w: &mut W) -> io::Result<()> {
            w.write_all(&order.id.to_be_bytes())?;
            borsh_write_address(&order.trader, w)?;
            w.write_all(&[match order.side {
                Side::Buy => 0u8,
                Side::Sell => 1u8,
            }])?;
            borsh_write_fp(&order.price, w)?;
            borsh_write_fp(&order.remaining_qty, w)?;
            borsh_write_fp(&order.original_qty, w)?;
            match &order.order_type {
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
            w.write_all(&[match order.time_in_force {
                TimeInForce::GTC => 0u8,
                TimeInForce::IOC => 1u8,
                TimeInForce::FOK => 2u8,
                TimeInForce::PostOnly => 3u8,
            }])?;
            w.write_all(&order.timestamp.to_be_bytes())?;
            w.write_all(&[u8::from(order.reduce_only)])?;
            match order.client_order_id {
                None => w.write_all(&[0u8])?,
                Some(cid) => {
                    w.write_all(&[1u8])?;
                    w.write_all(&cid.to_be_bytes())?;
                }
            }
            Ok(())
        }

        fn serialize_stop<W: IoWrite>(stop: &StopOrder, w: &mut W) -> io::Result<()> {
            w.write_all(&stop.id.to_be_bytes())?;
            borsh_write_address(&stop.trader, w)?;
            w.write_all(&stop.market_id.to_be_bytes())?;
            w.write_all(&[match stop.side {
                Side::Buy => 0u8,
                Side::Sell => 1u8,
            }])?;
            borsh_write_fp(&stop.trigger_price, w)?;
            match &stop.limit_price {
                None => w.write_all(&[0u8])?,
                Some(lp) => {
                    w.write_all(&[1u8])?;
                    borsh_write_fp(lp, w)?;
                }
            }
            borsh_write_fp(&stop.quantity, w)?;
            w.write_all(&[match stop.time_in_force {
                TimeInForce::GTC => 0u8,
                TimeInForce::IOC => 1u8,
                TimeInForce::FOK => 2u8,
                TimeInForce::PostOnly => 3u8,
            }])?;
            w.write_all(&stop.timestamp.to_be_bytes())?;
            w.write_all(&[u8::from(stop.reduce_only)])?;
            match stop.client_order_id {
                None => w.write_all(&[0u8])?,
                Some(cid) => {
                    w.write_all(&[1u8])?;
                    w.write_all(&cid.to_be_bytes())?;
                }
            }
            Ok(())
        }

        pub fn to_bytes(&self) -> Vec<u8> {
            let mut w: Vec<u8> = Vec::new();
            w.write_all(&self.market_id.to_be_bytes()).unwrap();
            borsh_write_fp(&self.tick_size, &mut w).unwrap();
            borsh_write_fp(&self.lot_size, &mut w).unwrap();
            w.write_all(&self.next_id.to_be_bytes()).unwrap();

            match &self.last_trade_price {
                None => w.write_all(&[0u8]).unwrap(),
                Some(ltp) => {
                    w.write_all(&[1u8]).unwrap();
                    borsh_write_fp(ltp, &mut w).unwrap();
                }
            }

            let order_count: u32 = (self.bids.values().map(|q| q.len()).sum::<usize>()
                + self.asks.values().map(|q| q.len()).sum::<usize>())
                as u32;
            w.write_all(&order_count.to_be_bytes()).unwrap();

            for queue in self.bids.values() {
                for order in queue {
                    Self::serialize_order(order, &mut w).unwrap();
                }
            }
            for queue in self.asks.values() {
                for order in queue {
                    Self::serialize_order(order, &mut w).unwrap();
                }
            }

            let stop_count = self.pending_stops.len() as u32;
            w.write_all(&stop_count.to_be_bytes()).unwrap();
            for stop in &self.pending_stops {
                Self::serialize_stop(stop, &mut w).unwrap();
            }

            w
        }
    }
}

// ============================================================================
// Op model + comparison plumbing
// ============================================================================

type OrderTuple = (
    u128,
    Address,
    u8,
    i128,
    i128,
    i128,
    u8,
    u8,
    u64,
    bool,
    Option<u64>,
);

fn side_u8(s: Side) -> u8 {
    match s {
        Side::Buy => 0,
        Side::Sell => 1,
    }
}

fn ot_u8(ot: &OrderType) -> u8 {
    match ot {
        OrderType::Limit => 0,
        OrderType::Market => 1,
        OrderType::StopMarket { .. } => 2,
        OrderType::StopLimit { .. } => 3,
    }
}

fn tif_u8(t: TimeInForce) -> u8 {
    match t {
        TimeInForce::GTC => 0,
        TimeInForce::IOC => 1,
        TimeInForce::FOK => 2,
        TimeInForce::PostOnly => 3,
    }
}

fn prod_order_tuple(o: &torus_core::order_book::Order) -> OrderTuple {
    (
        o.id,
        o.trader,
        side_u8(o.side),
        o.price.raw(),
        o.remaining_qty.raw(),
        o.original_qty.raw(),
        ot_u8(&o.order_type),
        tif_u8(o.time_in_force),
        o.timestamp,
        o.reduce_only,
        o.client_order_id,
    )
}

fn ref_order_tuple(o: &reference::Order) -> OrderTuple {
    (
        o.id,
        o.trader,
        side_u8(o.side),
        o.price.raw(),
        o.remaining_qty.raw(),
        o.original_qty.raw(),
        ot_u8(&o.order_type),
        tif_u8(o.time_in_force),
        o.timestamp,
        o.reduce_only,
        o.client_order_id,
    )
}

type FillTuple = (u128, u128, i128, i128, Address, Address, u8, u64);

fn prod_fill_tuple(f: &torus_core::order_book::Fill) -> FillTuple {
    (
        f.maker_order_id,
        f.taker_order_id,
        f.price.raw(),
        f.quantity.raw(),
        f.maker,
        f.taker,
        side_u8(f.maker_side),
        f.timestamp,
    )
}

fn ref_fill_tuple(f: &reference::Fill) -> FillTuple {
    (
        f.maker_order_id,
        f.taker_order_id,
        f.price.raw(),
        f.quantity.raw(),
        f.maker,
        f.taker,
        side_u8(f.maker_side),
        f.timestamp,
    )
}

fn status_u8(s: &torus_core::order_book::OrderStatus) -> u8 {
    use torus_core::order_book::OrderStatus as S;
    match s {
        S::Filled => 0,
        S::PartiallyFilled => 1,
        S::Resting => 2,
        S::Cancelled => 3,
        S::Rejected => 4,
        S::PendingTrigger => 5,
    }
}

fn ref_status_u8(s: &reference::OrderStatus) -> u8 {
    use reference::OrderStatus as S;
    match s {
        S::Filled => 0,
        S::PartiallyFilled => 1,
        S::Resting => 2,
        S::Cancelled => 3,
        S::Rejected => 4,
        S::PendingTrigger => 5,
    }
}

#[derive(Clone, Debug)]
enum Op {
    Place {
        is_buy: bool,
        price: i64,
        qty: i64,
        ot: u8,
        tif: u8,
        trader: u8,
        reduce_only: bool,
        cid: Option<u64>,
    },
    Cancel {
        sel: usize,
    },
    CancelAll {
        trader: u8,
    },
    Modify {
        sel: usize,
        new_price: Option<i64>,
        new_qty: Option<i64>,
    },
    CheckStops,
}

fn fp(n: i64) -> FixedPoint {
    FixedPoint::from_raw(n as i128 * FixedPoint::SCALE)
}

fn arb_op() -> impl Strategy<Value = Op> {
    prop_oneof![
        6 => (
            prop::bool::ANY,
            1..=300i64,
            1..=50i64,
            0..=6u8,
            0..=3u8,
            1..=8u8,
            prop::bool::ANY,
            prop::option::of(0..1000u64),
        )
            .prop_map(
                |(is_buy, price, qty, ot, tif, trader, reduce_only, cid)| Op::Place {
                    is_buy,
                    price,
                    qty,
                    ot,
                    tif,
                    trader,
                    reduce_only,
                    cid,
                }
            ),
        2 => (0..1000usize).prop_map(|sel| Op::Cancel { sel }),
        1 => (1..=8u8).prop_map(|trader| Op::CancelAll { trader }),
        1 => (
            0..1000usize,
            prop::option::of(1..=300i64),
            prop::option::of(1..=60i64),
        )
            .prop_map(|(sel, new_price, new_qty)| Op::Modify {
                sel,
                new_price,
                new_qty,
            }),
        1 => Just(Op::CheckStops),
    ]
}

fn build_params(op: &Op) -> Option<(PlaceOrderParams, Address)> {
    if let Op::Place {
        is_buy,
        price,
        qty,
        ot,
        tif,
        trader,
        reduce_only,
        cid,
    } = op
    {
        // ot: 0-3 Limit, 4 Market, 5 StopMarket, 6 StopLimit
        let order_type = match ot {
            0..=3 => OrderType::Limit,
            4 => OrderType::Market,
            5 => OrderType::StopMarket { trigger: fp(*price) },
            _ => OrderType::StopLimit {
                trigger: fp(*price),
                limit: fp((*price + 5).min(300)),
            },
        };
        let time_in_force = match tif {
            0 => TimeInForce::GTC,
            1 => TimeInForce::IOC,
            2 => TimeInForce::FOK,
            _ => TimeInForce::PostOnly,
        };
        Some((
            PlaceOrderParams {
                market_id: 1,
                is_buy: *is_buy,
                price: fp(*price),
                quantity: fp(*qty),
                order_type,
                time_in_force,
                reduce_only: *reduce_only,
                client_order_id: *cid,
            },
            Address::from([*trader; 20]),
        ))
    } else {
        None
    }
}

/// Drive the same op sequence into the production book and the frozen
/// reference; assert identical outputs and byte-identical serialized state
/// after every operation.
fn run_differential(ops: &[Op]) {
    let mut prod = OrderBook::new(1, FixedPoint::from_raw(1), FixedPoint::from_raw(1));
    let mut refr = reference::OrderBook::new(1, FixedPoint::from_raw(1), FixedPoint::from_raw(1));

    let mut all_ids: Vec<u128> = Vec::new();

    for (i, op) in ops.iter().enumerate() {
        let ts = i as u64;
        match op {
            Op::Place { .. } => {
                let (params, trader) = build_params(op).unwrap();
                let rp = prod.place_order(params.clone(), trader, ts);
                let rr = refr.place_order(params, trader, ts);

                assert_eq!(rp.order_id, rr.order_id, "op {i}: order_id diverged");
                assert_eq!(
                    status_u8(&rp.status),
                    ref_status_u8(&rr.status),
                    "op {i}: status diverged"
                );
                let pf: Vec<FillTuple> = rp.fills.iter().map(prod_fill_tuple).collect();
                let rf: Vec<FillTuple> = rr.fills.iter().map(ref_fill_tuple).collect();
                assert_eq!(pf, rf, "op {i}: fills diverged");
                let pc: Vec<OrderTuple> =
                    rp.self_trade_cancels.iter().map(prod_order_tuple).collect();
                let rc: Vec<OrderTuple> =
                    rr.self_trade_cancels.iter().map(ref_order_tuple).collect();
                assert_eq!(pc, rc, "op {i}: self_trade_cancels diverged");

                all_ids.push(rp.order_id);
            }
            Op::Cancel { sel } => {
                if all_ids.is_empty() {
                    continue;
                }
                let id = all_ids[sel % all_ids.len()];
                let rp = prod.cancel_order(id);
                let rr = refr.cancel_order(id);
                assert_eq!(rp.is_ok(), rr.is_ok(), "op {i}: cancel ok-ness diverged");
                if let (Ok(po), Ok(ro)) = (&rp, &rr) {
                    assert_eq!(
                        prod_order_tuple(po),
                        ref_order_tuple(ro),
                        "op {i}: cancelled order diverged"
                    );
                }
            }
            Op::CancelAll { trader } => {
                let t = Address::from([*trader; 20]);
                let mut rp: Vec<OrderTuple> = prod
                    .cancel_all(t, None)
                    .iter()
                    .map(prod_order_tuple)
                    .collect();
                let mut rr: Vec<OrderTuple> = refr
                    .cancel_all(t, None)
                    .iter()
                    .map(ref_order_tuple)
                    .collect();
                // Trader-index Vec order is not part of the contract (the only
                // consensus consumer folds a commutative sum) — compare as an
                // id-sorted multiset.
                rp.sort_unstable();
                rr.sort_unstable();
                assert_eq!(rp, rr, "op {i}: cancel_all set diverged");
            }
            Op::Modify {
                sel,
                new_price,
                new_qty,
            } => {
                if all_ids.is_empty() {
                    continue;
                }
                let id = all_ids[sel % all_ids.len()];
                let np = new_price.map(fp);
                let nq = new_qty.map(fp);
                let rp = prod.modify_order(id, np, nq);
                let rr = refr.modify_order(id, np, nq);
                assert_eq!(rp.is_ok(), rr.is_ok(), "op {i}: modify ok-ness diverged");
                if let (Ok(po), Ok(ro)) = (&rp, &rr) {
                    assert_eq!(
                        prod_order_tuple(po),
                        ref_order_tuple(ro),
                        "op {i}: modified order diverged"
                    );
                }
            }
            Op::CheckStops => {
                prod.check_stops();
                refr.check_stops();
            }
        }

        // Book state must be BYTE-IDENTICAL after every operation.
        let prod_bytes = to_vec(&prod).expect("prod serialize");
        let ref_bytes = refr.to_bytes();
        assert_eq!(
            prod_bytes, ref_bytes,
            "op {i}: serialized book bytes diverged"
        );
        assert_eq!(prod.best_bid(), refr.best_bid(), "op {i}: best_bid");
        assert_eq!(prod.best_ask(), refr.best_ask(), "op {i}: best_ask");

        // modify_order re-inserts at the new price WITHOUT re-matching (a
        // pre-existing production behavior), so a random price modify can
        // legitimately cross the book. verify_invariants asserts an
        // uncrossed book, so only run it when that pre-existing hole was not
        // exercised — the byte-identity assertions above always run.
        let crossed = matches!(
            (prod.best_bid(), prod.best_ask()),
            (Some(b), Some(a)) if b >= a
        );
        if !crossed {
            prod.verify_invariants();
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

proptest! {
    #![proptest_config(ProptestConfig::with_cases(150))]

    /// Randomized order flows: place (all types/TIFs, dense self-trading),
    /// cancel, cancel_all, modify, stop triggering — production vs frozen
    /// pre-change reference, byte-identical state after every op.
    #[test]
    fn differential_random_flows(ops in prop::collection::vec(arb_op(), 1..250)) {
        run_differential(&ops);
    }
}

/// Deterministic dense scenario: few traders, tight price band — maximizes
/// self-trade cancels, multi-level sweeps, and trader-index churn.
#[test]
fn differential_dense_matching() {
    let mut ops = Vec::new();
    let mut s = 0x1234_5678_9ABC_DEFu64;
    let mut next = || {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        s
    };
    for _ in 0..2000 {
        let r = next();
        let op = match r % 10 {
            0..=5 => Op::Place {
                is_buy: r & 1 == 0,
                price: 95 + (next() % 11) as i64, // 95..=105 tight band
                qty: 1 + (next() % 20) as i64,
                ot: (next() % 7) as u8,
                tif: (next() % 4) as u8,
                trader: 1 + (next() % 3) as u8, // only 3 traders → heavy STP
                reduce_only: false,
                cid: None,
            },
            6 | 7 => Op::Cancel {
                sel: next() as usize,
            },
            8 => Op::Modify {
                sel: next() as usize,
                new_price: if next() & 1 == 0 {
                    Some(95 + (next() % 11) as i64)
                } else {
                    None
                },
                new_qty: Some(1 + (next() % 25) as i64),
            },
            _ => Op::CancelAll {
                trader: 1 + (next() % 3) as u8,
            },
        };
        ops.push(op);
    }
    run_differential(&ops);
}

/// Per-trader order-cap boundary: a single trader walks up to and over the
/// 200-resting-orders cap, then frees slots via cancel and refills.
#[test]
fn differential_trader_cap_boundary() {
    let mut ops = Vec::new();
    for k in 0..230 {
        ops.push(Op::Place {
            is_buy: true,
            price: 1 + (k % 250),
            qty: 1,
            ot: 0,
            tif: 0,
            trader: 1,
            reduce_only: false,
            cid: None,
        });
    }
    for k in 0..40usize {
        ops.push(Op::Cancel { sel: k * 5 });
    }
    for k in 0..60 {
        ops.push(Op::Place {
            is_buy: k % 2 == 0,
            price: 1 + (k % 250),
            qty: 2,
            ot: 0,
            tif: 0,
            trader: 1,
            reduce_only: false,
            cid: None,
        });
    }
    run_differential(&ops);
}
