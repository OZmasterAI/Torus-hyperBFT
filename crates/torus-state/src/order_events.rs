//! s139: order lifecycle events for the `orderUpdates` WebSocket stream.
//!
//! Recorded by execution only while a stream subscriber exists (the order
//! book's event log, `OrderBook::set_order_events`, plus the executor's
//! pre-book rejections), then handed to the stream sink once per executed
//! block ([`crate::trade_rows::FillSink::send_order_events`]). Node-local
//! output: never written to state, never hashed, never read by execution.

use alloy_primitives::Address;
use torus_types::{Cloid, FixedPoint, MarketId, OrderId, Side};

use crate::action_status::FailureReason;

/// What happened to an order. One event per transition; an order can have
/// several in one block (e.g. `open` then `canceled`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OrderEventStatus {
    /// Placed and resting on the book, nothing filled at placement.
    Open,
    /// Some quantity filled (`filled`), `remaining` still resting, or (a
    /// taker that never rests) followed by a `Canceled` of the remainder.
    PartiallyFilled,
    /// The whole remaining quantity filled; `remaining` is zero.
    Filled,
    /// Removed from the book with `remaining` unfilled; see the reason.
    Canceled,
    /// A stop order stored, waiting for its trigger.
    TriggerPending,
    /// A stop order's trigger fired; its order is placed next (same id).
    Triggered,
    /// A resting order changed size or price (user modify, or a reduce-only
    /// shrink); `remaining` is the new size.
    Modified,
    /// Refused: never rested. `order_id` is `None` when it was refused
    /// before the book assigned one.
    Rejected,
}

/// Why an order was canceled, modified or rejected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OrderEventReason {
    /// No reason applies (open, fills, triggers).
    None,
    /// The owner's cancel, cancel-all or modify.
    User,
    /// Self-trade prevention: the owner's own taker crossed it.
    SelfTrade,
    /// A reduce-only order that could no longer reduce the position.
    ReduceOnly,
    /// The maker's account could not afford the fill (HL `marginCanceled`).
    Margin,
    /// The owner was liquidated.
    Liquidation,
    /// The market was delisted.
    Delisted,
    /// The unfilled remainder of an order that never rests (IOC, market, FOK,
    /// a taker out of margin, a reduce-only clamp).
    Unfilled,
    /// The placement was refused for this reason.
    Reject(FailureReason),
}

/// One order event. Quantities and prices are the book's FixedPoint values.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OrderEvent {
    pub market: MarketId,
    /// `None` only for a [`OrderEventStatus::Rejected`] placement refused
    /// before the book assigned an id.
    pub order_id: Option<OrderId>,
    pub trader: Address,
    pub cloid: Option<Cloid>,
    pub side: Side,
    /// The order's limit price (a market order's or stop-market's price cap).
    pub price: FixedPoint,
    /// A stop order's trigger price (`TriggerPending`, `Triggered`, and the
    /// cancel of a pending stop).
    pub trigger_price: Option<FixedPoint>,
    /// The order's original size.
    pub orig_qty: FixedPoint,
    /// Size still live after this event (resting, or pending for a stop).
    pub remaining: FixedPoint,
    /// Size this event filled (zero unless a fill status).
    pub filled: FixedPoint,
    pub reduce_only: bool,
    pub status: OrderEventStatus,
    pub reason: OrderEventReason,
}

/// One executed block's order events, in execution order within a market;
/// markets in ascending id, then the pre-book rejections.
#[derive(Debug, Default)]
pub struct BlockOrderEvents {
    pub height: u64,
    pub timestamp: u64,
    pub events: Vec<OrderEvent>,
}
