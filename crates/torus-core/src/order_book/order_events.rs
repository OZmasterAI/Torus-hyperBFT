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
}
