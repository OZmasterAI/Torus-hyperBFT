//! s139: the book's order event log for the `orderUpdates` WebSocket stream
//! (`torus_state::order_events`). Off by default. While on, the public
//! mutations log what they did to every order they touched, in execution
//! order: placements (taker outcome or rejection, stops stored), maker fills,
//! self-trade / reduce-only / margin cancels, cancels, cancel-alls, stop
//! cancels and triggers, modifies. In-RAM only: the log never changes what
//! the book matches, saves or hashes (`order_events_never_change_the_book`).

use super::*;

impl OrderBook {
    /// Turn the event log on (an empty log) or off (dropped). Turning it on
    /// again keeps what is logged.
    pub fn set_order_events(&mut self, on: bool) {
        match (on, self.order_events.is_some()) {
            (true, false) => self.order_events = Some(Vec::new()),
            (false, true) => self.order_events = None,
            _ => {}
        }
    }

    /// Whether the event log is on.
    #[inline]
    pub fn order_events_on(&self) -> bool {
        self.order_events.is_some()
    }

    /// Drain the logged events (the log stays on).
    pub fn take_order_events(&mut self) -> Vec<OrderEvent> {
        self.order_events.as_mut().map(std::mem::take).unwrap_or_default()
    }

    /// Set the reason the public cancel paths log (liquidation, delist);
    /// returns the previous one for the caller to restore.
    pub fn set_cancel_cause(&mut self, cause: OrderEventReason) -> OrderEventReason {
        std::mem::replace(&mut self.cancel_cause, cause)
    }

    /// Log `e` if the log is on.
    #[inline]
    pub(super) fn log_event(&mut self, e: OrderEvent) {
        if let Some(log) = self.order_events.as_mut() {
            log.push(e);
        }
    }

    /// Log a removed resting order, `remaining` unfilled.
    pub(super) fn log_removed(&mut self, o: &Order, reason: OrderEventReason) {
        if self.order_events.is_some() {
            let e = order_event(self.market_id, o, OrderEventStatus::Canceled, reason, FixedPoint::ZERO);
            self.log_event(e);
        }
    }

    /// Log a modified resting order (its new size and price).
    pub(super) fn log_modified(&mut self, o: &Order, reason: OrderEventReason) {
        if self.order_events.is_some() {
            let e = order_event(self.market_id, o, OrderEventStatus::Modified, reason, FixedPoint::ZERO);
            self.log_event(e);
        }
    }

    /// Log a removed pending stop.
    pub(super) fn log_stop_removed(&mut self, s: &StopOrder, reason: OrderEventReason) {
        if self.order_events.is_some() {
            self.log_event(stop_event(s, OrderEventStatus::Canceled, reason));
        }
    }

    /// The pending stops of the traders `owned` selects, cloned, while the
    /// log is on (empty otherwise): a cancel-all removes them, and the log
    /// needs them afterwards.
    pub(super) fn stops_if_logging(&self, owned: impl Fn(&Address) -> bool) -> Vec<StopOrder> {
        if self.order_events.is_none() {
            return Vec::new();
        }
        self.pending_stops
            .iter()
            .filter(|s| owned(&s.trader))
            .cloned()
            .collect()
    }

    /// Log the stops of `before` that are no longer pending (a cancel-all
    /// removed them) with the current cancel cause.
    pub(super) fn log_stops_gone(&mut self, before: Vec<StopOrder>) {
        let cause = self.cancel_cause;
        for s in before {
            if !self.pending_stops.iter().any(|p| p.id == s.id) {
                self.log_stop_removed(&s, cause);
            }
        }
    }

    /// Log one placement's own outcome (the taker side; its makers and the
    /// stops it fired were logged while it ran). `p` is the placement as
    /// requested. The fired stops' `Triggered` events, logged by
    /// `trigger_stops` at the end of the placement, stay after it: the
    /// taker's events go in before the last `triggered` entries.
    pub(super) fn log_placement(&mut self, p: &PlacedParams, result: &PlaceResult) {
        let Some(log) = self.order_events.as_mut() else {
            return;
        };
        use OrderEventStatus as S;
        let filled = result
            .fills
            .iter()
            .fold(FixedPoint::ZERO, |acc, f| acc + f.quantity);
        let base = OrderEvent {
            market: self.market_id,
            order_id: Some(result.order_id),
            trader: p.trader,
            cloid: p.cloid,
            side: p.side,
            price: p.price,
            trigger_price: p.trigger,
            orig_qty: p.qty,
            remaining: FixedPoint::ZERO,
            filled: FixedPoint::ZERO,
            reduce_only: p.reduce_only,
            status: S::Open,
            reason: OrderEventReason::None,
        };
        let ev = |status, reason, filled, remaining| OrderEvent {
            status,
            reason,
            filled,
            remaining,
            ..base.clone()
        };
        let unfilled = p.qty - filled.min(p.qty);
        // Pushed at the end, then rotated in front of the fired stops' events
        // (no allocation per placement).
        let start = log.len();
        let out = &mut *log;
        match (result.reject, &result.status) {
            (Some(reason), _) => out.push(ev(
                S::Rejected,
                OrderEventReason::Reject(reason),
                FixedPoint::ZERO,
                FixedPoint::ZERO,
            )),
            (None, OrderStatus::PendingTrigger) => {
                out.push(ev(S::TriggerPending, OrderEventReason::None, FixedPoint::ZERO, p.qty))
            }
            (None, OrderStatus::Filled) => {
                out.push(ev(S::Filled, OrderEventReason::None, filled, FixedPoint::ZERO))
            }
            (None, OrderStatus::PartiallyFilled) => out.push(ev(
                S::PartiallyFilled,
                OrderEventReason::None,
                filled,
                result.rested_qty,
            )),
            (None, OrderStatus::Resting) => {
                out.push(ev(S::Open, OrderEventReason::None, FixedPoint::ZERO, result.rested_qty))
            }
            (None, OrderStatus::Cancelled) => {
                if filled > FixedPoint::ZERO {
                    out.push(ev(S::PartiallyFilled, OrderEventReason::None, filled, unfilled));
                }
                out.push(ev(S::Canceled, OrderEventReason::Unfilled, FixedPoint::ZERO, unfilled));
            }
            (None, OrderStatus::Rejected) => out.push(ev(
                S::Rejected,
                OrderEventReason::Reject(FailureReason::Other),
                FixedPoint::ZERO,
                FixedPoint::ZERO,
            )),
        }
        let added = log.len() - start;
        let fired = result.triggered_stops.len().min(start);
        if fired > 0 {
            log[start - fired..].rotate_right(added);
        }
    }
}

/// What [`OrderBook::log_placement`] needs of a placement, captured before
/// the book consumes its params (only while the log is on).
pub(super) struct PlacedParams {
    pub trader: Address,
    pub cloid: Option<Cloid>,
    pub side: Side,
    /// The limit (a StopLimit's limit), or a market / stop-market cap.
    pub price: FixedPoint,
    pub trigger: Option<FixedPoint>,
    pub qty: FixedPoint,
    pub reduce_only: bool,
}

impl PlacedParams {
    pub(super) fn new(params: &PlaceOrderParams, trader: Address) -> Self {
        let (price, trigger) = match params.order_type {
            OrderType::Limit | OrderType::Market => (params.price, None),
            OrderType::StopMarket { trigger } => (params.price, Some(trigger)),
            OrderType::StopLimit { trigger, limit } => (limit, Some(trigger)),
        };
        Self {
            trader,
            cloid: params.client_order_id,
            side: if params.is_buy { Side::Buy } else { Side::Sell },
            price,
            trigger,
            qty: params.quantity,
            reduce_only: params.reduce_only,
        }
    }
}

/// The event of resting order `o` in market `market` (its fields as they are
/// now: `remaining` is `o.remaining_qty`).
pub(super) fn order_event(
    market: MarketId,
    o: &Order,
    status: OrderEventStatus,
    reason: OrderEventReason,
    filled: FixedPoint,
) -> OrderEvent {
    OrderEvent {
        market,
        order_id: Some(o.id),
        trader: o.trader,
        cloid: o.client_order_id,
        side: o.side,
        price: o.price,
        trigger_price: None,
        orig_qty: o.original_qty,
        remaining: o.remaining_qty,
        filled,
        reduce_only: o.reduce_only,
        status,
        reason,
    }
}

/// The event of stop `s` (`remaining` is its whole quantity).
pub(super) fn stop_event(s: &StopOrder, status: OrderEventStatus, reason: OrderEventReason) -> OrderEvent {
    OrderEvent {
        market: s.market_id,
        order_id: Some(s.id),
        trader: s.trader,
        cloid: s.client_order_id,
        side: s.side,
        price: s.reservation_price(),
        trigger_price: Some(s.trigger_price),
        orig_qty: s.quantity,
        remaining: s.quantity,
        filled: FixedPoint::ZERO,
        reduce_only: s.reduce_only,
        status,
        reason,
    }
}
