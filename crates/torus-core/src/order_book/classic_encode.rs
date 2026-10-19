//! 8k L3.3 (`TORUS_BOOK_FAST_ENCODE`): an exact-size encoder for the Classic
//! whole-book blob.
//!
//! [`OrderBook::encode_classic`] returns the same bytes as
//! `borsh::to_vec(book)` (the custom `BorshSerialize` impls of `Order`,
//! `StopOrder` and `OrderBook` in `order_book.rs`), but:
//! - the `Vec` is allocated once at its exact final size (a size pass over
//!   the levels first), where `borsh::to_vec` starts at 1 KiB and doubles,
//!   copying the blob on every growth (s96.3: memmove 15% of `save_books`
//!   at 1M resting orders);
//! - each resting order is assembled in one stack record from
//!   `to_be_bytes` and appended with a single copy, where the borsh impl
//!   makes ~14 `write_all` calls per order, each with its own capacity check.
//!
//! The layout is the Classic blob's and must never diverge from it: the
//! `classic_encode_tests` proptest pins the two byte for byte.
//!
//! Scope (s131): only the Classic book mode (`TORUS_BOOK_ROWS` unset or `0`)
//! calls this. Modes 1, 2 and 3 never do. If the testnet genesis picks mode 3
//! (pending bench 8p and the owner's book-mode decision), this encoder and its
//! flag are dead code: remove them in a cleanup. Before any default-on,
//! profile the unexplained +2.8% exec-engine timer on the deep book (perf doc
//! section 97.8).

use super::*;

/// Fixed part of an encoded resting order: id 16, trader 20, side 1, price /
/// remaining / original 3 x 16, order-type tag 1, TIF 1, timestamp 8,
/// reduce-only 1, cloid tag 1.
const ORDER_BASE: usize = 16 + 20 + 1 + 3 * 16 + 1 + 1 + 8 + 1 + 1;
/// Largest encoded order: a StopLimit (+32) with a cloid (+16).
const ORDER_MAX: usize = ORDER_BASE + 32 + 16;

const _: () = assert!(ORDER_BASE == 97);

#[inline]
fn order_encoded_len(o: &Order) -> usize {
    let ot = match o.order_type {
        OrderType::Limit | OrderType::Market => 0,
        OrderType::StopMarket { .. } => 16,
        OrderType::StopLimit { .. } => 32,
    };
    let cloid = if o.client_order_id.is_some() { 16 } else { 0 };
    ORDER_BASE + ot + cloid
}

#[inline]
fn stop_encoded_len(s: &StopOrder) -> usize {
    // id 16, trader 20, market 8, side 1, trigger 16, price tag 1 (+16 for a
    // limit or a positive cap), quantity 16, TIF 1, timestamp 8, reduce-only
    // 1, cloid tag 1 (+16).
    let price = match s.limit_price {
        None if s.price_cap > FixedPoint::ZERO => 17,
        None => 1,
        Some(_) => 17,
    };
    let cloid = if s.client_order_id.is_some() { 17 } else { 1 };
    16 + 20 + 8 + 1 + 16 + price + 16 + 1 + 8 + 1 + cloid
}

#[inline]
fn tif_byte(t: TimeInForce) -> u8 {
    match t {
        TimeInForce::GTC => 0,
        TimeInForce::IOC => 1,
        TimeInForce::FOK => 2,
        TimeInForce::PostOnly => 3,
    }
}

/// Writes `o`'s Classic encoding into `rec`; returns its length.
#[inline(always)]
fn encode_order(o: &Order, rec: &mut [u8; ORDER_MAX]) -> usize {
    rec[0..16].copy_from_slice(&o.id.to_be_bytes());
    rec[16..36].copy_from_slice(o.trader.as_slice());
    rec[36] = match o.side {
        Side::Buy => 0,
        Side::Sell => 1,
    };
    rec[37..53].copy_from_slice(&o.price.raw().to_be_bytes());
    rec[53..69].copy_from_slice(&o.remaining_qty.raw().to_be_bytes());
    rec[69..85].copy_from_slice(&o.original_qty.raw().to_be_bytes());
    let mut p = 85;
    match o.order_type {
        OrderType::Limit => {
            rec[p] = 0;
            p += 1;
        }
        OrderType::Market => {
            rec[p] = 1;
            p += 1;
        }
        OrderType::StopMarket { trigger } => {
            rec[p] = 2;
            rec[p + 1..p + 17].copy_from_slice(&trigger.raw().to_be_bytes());
            p += 17;
        }
        OrderType::StopLimit { trigger, limit } => {
            rec[p] = 3;
            rec[p + 1..p + 17].copy_from_slice(&trigger.raw().to_be_bytes());
            rec[p + 17..p + 33].copy_from_slice(&limit.raw().to_be_bytes());
            p += 33;
        }
    }
    rec[p] = tif_byte(o.time_in_force);
    rec[p + 1..p + 9].copy_from_slice(&o.timestamp.to_be_bytes());
    rec[p + 9] = u8::from(o.reduce_only);
    p += 10;
    match o.client_order_id {
        None => {
            rec[p] = 0;
            p += 1;
        }
        Some(cid) => {
            rec[p] = 1;
            rec[p + 1..p + 17].copy_from_slice(&cid.to_be_bytes());
            p += 17;
        }
    }
    p
}

impl OrderBook {
    /// Classic blob header: market 8, tick 16, lot 16, next id 16, last
    /// trade price tag 1 (+16), order count 4.
    #[inline]
    fn classic_header_len(&self) -> usize {
        8 + 16
            + 16
            + 16
            + if self.last_trade_price.is_some() {
                17
            } else {
                1
            }
            + 4
    }

    /// Exact length of [`Self::encode_classic`] (== `borsh::to_vec(self)`).
    pub fn classic_encoded_len(&self) -> usize {
        let orders: usize = self
            .bids
            .values()
            .chain(self.asks.values())
            .flat_map(|q| q.iter())
            .map(order_encoded_len)
            .sum();
        let stops: usize = self.pending_stops.iter().map(stop_encoded_len).sum();
        self.classic_header_len() + orders + 4 + stops
    }

    /// Capacity for [`Self::encode_classic`] without a pass over the orders:
    /// header + 97 B per resting order + 16 B per resting cloid (the cloid
    /// index holds exactly the resting orders' and pending stops' cloids),
    /// plus the stops. Exact unless a resting order carries a stop order type
    /// (+16 / +32 B; the book never rests one: stops wait in
    /// `pending_stops`) or the cloid index is off; then the `Vec` grows. The
    /// bytes never depend on it.
    pub fn classic_capacity_hint(&self) -> usize {
        let order_count = self.bids.values().map(|q| q.len()).sum::<usize>()
            + self.asks.values().map(|q| q.len()).sum::<usize>();
        let mut stops = 0usize;
        let mut stop_cloids = 0usize;
        for s in &self.pending_stops {
            stops += stop_encoded_len(s);
            stop_cloids += usize::from(s.client_order_id.is_some());
        }
        let resting_cloids = self.cloids.map.len().saturating_sub(stop_cloids);
        self.classic_header_len() + order_count * ORDER_BASE + resting_cloids * 16 + 4 + stops
    }

    /// 8k L3.3: the Classic whole-book blob, byte-identical to
    /// `borsh::to_vec(self)`, built in one allocation sized by
    /// [`Self::classic_capacity_hint`] with one copy per resting order
    /// (module docs).
    pub fn encode_classic(&self) -> Vec<u8> {
        self.encode_classic_with_capacity(self.classic_capacity_hint())
    }

    /// The ranker's spec variant: an exact size pass over every order
    /// ([`Self::classic_encoded_len`]) before the encode. Kept for the
    /// microbench (it reads every order twice).
    #[doc(hidden)]
    pub fn encode_classic_size_pass(&self) -> Vec<u8> {
        let v = self.encode_classic_with_capacity(self.classic_encoded_len());
        debug_assert_eq!(
            v.len(),
            v.capacity(),
            "classic_encoded_len disagrees with encode_classic"
        );
        v
    }

    fn encode_classic_with_capacity(&self, capacity: usize) -> Vec<u8> {
        let mut buf: Vec<u8> = Vec::with_capacity(capacity);

        // Header (same fields and order as `BorshSerialize for OrderBook`).
        buf.extend_from_slice(&self.market_id.to_be_bytes());
        buf.extend_from_slice(&self.tick_size.raw().to_be_bytes());
        buf.extend_from_slice(&self.lot_size.raw().to_be_bytes());
        buf.extend_from_slice(&self.next_id.to_be_bytes());
        match &self.last_trade_price {
            None => buf.push(0),
            Some(ltp) => {
                buf.push(1);
                buf.extend_from_slice(&ltp.raw().to_be_bytes());
            }
        }
        let order_count: u32 = (self.bids.values().map(|q| q.len()).sum::<usize>()
            + self.asks.values().map(|q| q.len()).sum::<usize>())
            as u32;
        buf.extend_from_slice(&order_count.to_be_bytes());

        // Bids by level (BTreeMap ascending), then asks; FIFO within a level.
        let mut rec = [0u8; ORDER_MAX];
        for queue in self.bids.values().chain(self.asks.values()) {
            for order in queue {
                let n = encode_order(order, &mut rec);
                if n == ORDER_BASE {
                    // The common shape (Limit / Market, no cloid): a
                    // constant-size copy.
                    buf.extend_from_slice(&rec[..ORDER_BASE]);
                } else {
                    buf.extend_from_slice(&rec[..n]);
                }
            }
        }

        // Pending stops: few; the borsh impl itself (writing into a Vec
        // cannot fail).
        buf.extend_from_slice(&(self.pending_stops.len() as u32).to_be_bytes());
        for stop in &self.pending_stops {
            stop.serialize(&mut buf)
                .expect("serializing a stop into a Vec cannot fail");
        }

        buf
    }
}
