//! Item 6 Phase 5 step 3 (`docs/plans/item6-phase5-impl.md`): Classic book
//! rows from a dirty set.
//!
//! The Classic row of a book is its whole-book Borsh blob
//! (`BorshSerialize for OrderBook`): a header, the resting orders (bids
//! ascending by price, then asks ascending, FIFO within a level), then the
//! pending stops. [`OrderBook::classic_row`] returns exactly those bytes
//! without encoding every order again: each price level's encoded orders are
//! kept between saves. The dirty set is the book's level journal (the 3c
//! journal every queue primitive writes: insert, cancel, cancel-all and its
//! batched forms, modify, the match loop's fills and its self-trade /
//! reduce-only / margin cuts; complete because mode 2's level rows, consensus
//! in that mode, are written from it). One pass in row order: a level in the
//! journal is encoded straight into the row and its kept bytes are replaced
//! from there (a copy of bytes just written); any other level's kept bytes
//! are copied. The header and the stops are encoded every time.
//!
//! Why levels and not orders: a per-order rebuild of a dirty level reads
//! every order of it anyway (to find the unchanged ones), which costs as much
//! as encoding it (µbench, plan step 3 Built note).
//!
//! Any other drain of the level journal (a mode-1/2/3 save, a full write, a
//! bare discard) and a loaded order (inserted without a journal entry) drop
//! the kept levels; the next row is then built from every level
//! ([`ClassicRowStats::full_build`]).
//!
//! In-RAM only: never serialized, hashed or read by matching.

use super::*;

/// What one [`OrderBook::classic_row`] call did (test / metrics hook).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ClassicRowStats {
    /// The level cache was (re)built from every level of the book.
    pub full_build: bool,
    /// Levels whose kept bytes were copied.
    pub levels_reused: u64,
    /// Levels encoded in this call (all of them on a full build).
    pub levels_encoded: u64,
    /// Journaled levels that were gone from the book (dropped from the cache).
    pub levels_removed: u64,
    /// Length of the row.
    pub bytes: u64,
}

/// The kept encoding of every level of a book, keyed like the level journal
/// (`(side_tag, raw_price)`), so bids (tag 0) come before asks (tag 1), each
/// ascending by price: the Classic row's order.
#[derive(Default)]
pub(super) struct ClassicRowCache {
    levels: BTreeMap<(u8, i128), CachedLevel>,
}

#[derive(Default)]
struct CachedLevel {
    /// Orders in the level (the row's order count sums them).
    count: usize,
    /// `Order::serialize` of each order of the level, FIFO order; stale while
    /// `dirty`.
    bytes: Vec<u8>,
    /// In the level journal: encode it from the book in this pass.
    dirty: bool,
}

/// Upper bound of one encoded stop (`BorshSerialize for StopOrder`: id 16,
/// trader 20, market 8, side 1, trigger 16, tag 1 + 16, quantity 16, tif 1,
/// timestamp 8, reduce-only 1, cloid 1 + 16).
const STOP_MAX_BYTES: usize = 16 + 20 + 8 + 1 + 16 + 17 + 16 + 1 + 8 + 1 + 17;
/// Upper bound of the header (market 8, tick 16, lot 16, next id 16, last
/// trade price 1 + 16).
const HEADER_MAX_BYTES: usize = 8 + 16 + 16 + 16 + 17;
/// A limit order without a cloid: the presize of a dirty level per order.
const ORDER_TYPICAL_BYTES: usize = 97;

impl OrderBook {
    /// The book's Classic row, byte for byte `borsh::to_vec(self)`: the
    /// levels in the level journal are encoded, every other level's kept
    /// bytes are copied. Takes the level journal (the Classic save discards
    /// it right after anyway). On an error the kept levels are dropped (the
    /// journal is gone), so the next call builds them all.
    pub fn classic_row(&mut self) -> io::Result<(Vec<u8>, ClassicRowStats)> {
        let out = self.classic_row_inner();
        if out.is_err() {
            self.classic_rows = None;
        }
        out
    }

    fn classic_row_inner(&mut self) -> io::Result<(Vec<u8>, ClassicRowStats)> {
        use crate::book_rows::{SIDE_TAG_ASK, SIDE_TAG_BID};
        let mut stats = ClassicRowStats::default();
        let journal = std::mem::take(&mut self.level_journal);
        // A tag no queue primitive writes: trust nothing kept.
        if journal.iter().any(|(tag, _)| *tag != SIDE_TAG_BID && *tag != SIDE_TAG_ASK) {
            self.classic_rows = None;
        }
        let Self {
            market_id,
            bids,
            asks,
            pending_stops,
            tick_size,
            lot_size,
            next_id,
            last_trade_price,
            classic_rows,
            ..
        } = self;
        let side = |tag: u8| if tag == SIDE_TAG_BID { &*bids } else { &*asks };
        // 1. The cache's levels = the book's; the journaled ones (all on a
        //    full build) marked dirty with their new order count.
        let cache: &mut ClassicRowCache = match classic_rows {
            Some(cache) => {
                for &(tag, raw) in &journal {
                    match side(tag).get(&FixedPoint::from_raw(raw)) {
                        Some(queue) => {
                            let level = cache.levels.entry((tag, raw)).or_default();
                            level.count = queue.len();
                            level.dirty = true;
                        }
                        None => {
                            stats.levels_removed +=
                                u64::from(cache.levels.remove(&(tag, raw)).is_some());
                        }
                    }
                }
                cache
            }
            None => {
                let mut cache = ClassicRowCache::default();
                for tag in [SIDE_TAG_BID, SIDE_TAG_ASK] {
                    for (price, queue) in side(tag) {
                        let level = CachedLevel {
                            count: queue.len(),
                            bytes: Vec::new(),
                            dirty: true,
                        };
                        cache.levels.insert((tag, price.raw()), level);
                    }
                }
                stats.full_build = true;
                classic_rows.insert(Box::new(cache))
            }
        };
        // 2. One pass in row order (`BorshSerialize for OrderBook`).
        let (mut order_count, mut size) = (0usize, 0usize);
        for level in cache.levels.values() {
            order_count += level.count;
            size += if level.dirty {
                level.bytes.len().max(level.count * ORDER_TYPICAL_BYTES)
            } else {
                level.bytes.len()
            };
        }
        let mut w =
            Vec::with_capacity(HEADER_MAX_BYTES + 4 + size + 4 + pending_stops.len() * STOP_MAX_BYTES);
        w.write_all(&market_id.to_be_bytes())?;
        borsh_write_fp(tick_size, &mut w)?;
        borsh_write_fp(lot_size, &mut w)?;
        w.write_all(&next_id.to_be_bytes())?;
        match last_trade_price {
            None => w.write_all(&[0u8])?,
            Some(ltp) => {
                w.write_all(&[1u8])?;
                borsh_write_fp(ltp, &mut w)?;
            }
        }
        w.write_all(&(order_count as u32).to_be_bytes())?;
        for (&(tag, raw), level) in cache.levels.iter_mut() {
            if level.dirty {
                let queue = side(tag).get(&FixedPoint::from_raw(raw)).ok_or_else(|| {
                    io::Error::other("classic row: a dirty level is not in the book")
                })?;
                let start = w.len();
                for order in queue {
                    order.serialize(&mut w)?;
                }
                level.bytes.clear();
                level.bytes.extend_from_slice(&w[start..]);
                level.dirty = false;
                stats.levels_encoded += 1;
            } else {
                w.extend_from_slice(&level.bytes);
                stats.levels_reused += 1;
            }
        }
        w.write_all(&(pending_stops.len() as u32).to_be_bytes())?;
        for stop in pending_stops.iter() {
            stop.serialize(&mut w)?;
        }
        stats.bytes = w.len() as u64;
        Ok((w, stats))
    }

    /// Drop the kept level encodings when the level journal is about to be
    /// drained by anything but [`Self::classic_row`] (its entries would be
    /// lost to the cache), or a level changed without a journal entry.
    #[inline]
    pub(super) fn forget_classic_rows(&mut self) {
        self.classic_rows = None;
    }
}
