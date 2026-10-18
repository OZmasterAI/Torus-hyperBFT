//! Item 6 Phase 5 step 3 (`docs/plans/item6-phase5-impl.md`): Classic book
//! rows from a dirty set.
//!
//! The Classic row of a book is its whole-book Borsh blob
//! (`BorshSerialize for OrderBook`): a header, the resting orders (bids
//! ascending by price, then asks ascending, FIFO within a level), then the
//! pending stops. [`OrderBook::classic_row`] returns exactly those bytes
//! without walking every order: each price level's encoded orders are kept
//! between saves, and only the levels in the book's level journal (the 3c
//! journal every queue primitive writes: insert, cancel, cancel-all and its
//! batched forms, modify, the match loop's fills and its self-trade /
//! reduce-only / margin cuts) are encoded again. The header and the stops
//! are encoded every time.
//!
//! The dirty set is the level journal. Its other drains (a mode-1/2/3 save,
//! a full write, a bare discard) and a loaded order (inserted without a
//! journal entry) drop the kept levels; the next row is then built from
//! every level ([`ClassicRowStats::full_build`]).
//!
//! In-RAM only: never serialized, hashed or read by matching.

use super::*;

/// What one [`OrderBook::classic_row`] call did (test / metrics hook).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ClassicRowStats {
    /// The level cache was (re)built from every level of the book.
    pub full_build: bool,
    /// Levels whose kept bytes were copied as they were.
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
    /// `Order::serialize` of each order of the level, FIFO order.
    bytes: Vec<u8>,
}

/// Upper bound of one encoded order (`BorshSerialize for Order`: id 16,
/// trader 20, side 1, three amounts 48, type 1 + two amounts 32, tif 1,
/// timestamp 8, reduce-only 1, cloid 1 + 16).
const ORDER_MAX_BYTES: usize = 16 + 20 + 1 + 48 + 33 + 1 + 8 + 1 + 17;
/// Upper bound of one encoded stop (`BorshSerialize for StopOrder`: id 16,
/// trader 20, market 8, side 1, trigger 16, tag 1 + 16, quantity 16, tif 1,
/// timestamp 8, reduce-only 1, cloid 1 + 16).
const STOP_MAX_BYTES: usize = 16 + 20 + 8 + 1 + 16 + 17 + 16 + 1 + 8 + 1 + 17;
/// Upper bound of the header (market 8, tick 16, lot 16, next id 16, last
/// trade price 1 + 16).
const HEADER_MAX_BYTES: usize = 8 + 16 + 16 + 16 + 17;

/// Encode `queue` into `level` (its buffer reused).
fn encode_level(queue: &VecDeque<Order>, level: &mut CachedLevel) -> io::Result<()> {
    level.bytes.clear();
    level.bytes.reserve(queue.len() * ORDER_MAX_BYTES);
    for order in queue {
        order.serialize(&mut level.bytes)?;
    }
    level.count = queue.len();
    Ok(())
}

impl OrderBook {
    /// The book's Classic row, byte for byte `borsh::to_vec(self)`: the
    /// levels in the level journal are encoded again, every other level's
    /// kept bytes are copied. Takes the level journal (the Classic save
    /// discards it right after anyway). On an error the kept levels are
    /// dropped (the journal is gone), so the next call builds them all.
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
        let cache: &mut ClassicRowCache = match classic_rows {
            Some(cache) => {
                for &(tag, raw) in &journal {
                    let side = if tag == SIDE_TAG_BID { &*bids } else { &*asks };
                    match side.get(&FixedPoint::from_raw(raw)) {
                        Some(queue) => {
                            encode_level(queue, cache.levels.entry((tag, raw)).or_default())?;
                            stats.levels_encoded += 1;
                        }
                        None => {
                            stats.levels_removed +=
                                u64::from(cache.levels.remove(&(tag, raw)).is_some());
                        }
                    }
                }
                stats.levels_reused = cache.levels.len() as u64 - stats.levels_encoded;
                cache
            }
            None => {
                let mut cache = ClassicRowCache::default();
                for (tag, side) in [(SIDE_TAG_BID, &*bids), (SIDE_TAG_ASK, &*asks)] {
                    for (price, queue) in side {
                        let mut level = CachedLevel::default();
                        encode_level(queue, &mut level)?;
                        cache.levels.insert((tag, price.raw()), level);
                    }
                }
                stats.full_build = true;
                stats.levels_encoded = cache.levels.len() as u64;
                classic_rows.insert(Box::new(cache))
            }
        };
        // Assemble, in the order of `BorshSerialize for OrderBook`.
        let level_bytes: usize = cache.levels.values().map(|l| l.bytes.len()).sum();
        let order_count: usize = cache.levels.values().map(|l| l.count).sum();
        let mut w = Vec::with_capacity(
            HEADER_MAX_BYTES + 4 + level_bytes + 4 + pending_stops.len() * STOP_MAX_BYTES,
        );
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
        for level in cache.levels.values() {
            w.extend_from_slice(&level.bytes);
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
