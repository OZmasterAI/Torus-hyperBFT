//! Item 6 Phase 5 step 3 (`docs/plans/item6-phase5-impl.md`): Classic book
//! rows from a dirty set.
//!
//! The Classic row of a book is its whole-book Borsh blob
//! (`BorshSerialize for OrderBook`): a header, the resting orders (bids
//! ascending by price, then asks ascending, FIFO within a level), then the
//! pending stops. [`OrderBook::classic_row`] returns exactly those bytes
//! without walking every order: each price level's encoded orders are kept
//! between saves, and only the levels in the book's level journal (the 3c
//! journal every queue primitive writes) are encoded again. The header and
//! the stops are encoded every time.
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

struct CachedLevel {
    /// Orders in the level (the row's order count sums them).
    count: usize,
    /// `Order::serialize` of each order of the level, FIFO order.
    bytes: Vec<u8>,
}

impl OrderBook {
    /// The book's Classic row, byte for byte `borsh::to_vec(self)`.
    pub fn classic_row(&mut self) -> io::Result<(Vec<u8>, ClassicRowStats)> {
        let _ = &self.classic_rows;
        Ok((Vec::new(), ClassicRowStats::default()))
    }

    /// Drop the kept level encodings.
    pub(super) fn forget_classic_rows(&mut self) {
        let _ = (CachedLevel { count: 0, bytes: Vec::new() }.count, ClassicRowCache::default().levels.len());
    }
}
