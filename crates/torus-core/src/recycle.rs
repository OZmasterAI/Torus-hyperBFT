//! s80 exec-block-arena (`TORUS_S80_EXEC_ARENA`, node-local, default off):
//! free lists for the engine's per-call scratch maps, so a map built for one
//! market of one `execute_batch` call is CLEARED and handed to the next one
//! instead of dropped (capacity retained: no regrowth, rehash, realloc or
//! first-touch page faults on the next call).
//!
//! The executor decides (the flag lives in `torus-bridge`); this crate only
//! offers the recycled constructors and the `recycle` sinks:
//! - [`crate::position::PositionCache::recycled`] / `recycle` (pass A's
//!   per-market plan caches);
//! - [`crate::order_book::AccountMargins::new_recycled`] /
//!   [`crate::order_book::OrderBook::recycle_account_margins`];
//! - [`crate::order_book::ReduceOnlyPositions::recycled`] /
//!   [`crate::order_book::OrderBook::recycle_reduce_only_positions`].
//!
//! Every recycled map is EMPTY when handed out (cleared on the way in), so a
//! recycled value differs from a fresh one only in its capacity and hasher
//! seed: none of these maps is iterated in an order that reaches an output
//! (point reads / writes only, or sorted before use). Builders run on the
//! match / plan pool workers and the sinks on the exec thread, so the lists
//! are process-wide (`Mutex`, a push or a pop per hold).

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::Mutex;

/// A map kept by a free list holds at most this many entries' capacity; a
/// bigger one (an outlier block) is dropped instead of kept.
pub const MAX_KEPT_CAPACITY: usize = 1 << 16;

/// A bounded process-wide free list of cleared values.
pub struct Recycler<T> {
    free: Mutex<Vec<T>>,
    cap: usize,
    hits: AtomicU64,
    misses: AtomicU64,
}

impl<T> Recycler<T> {
    /// A free list keeping at most `cap` values.
    pub const fn new(cap: usize) -> Self {
        Self {
            free: Mutex::new(Vec::new()),
            cap,
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
        }
    }

    /// A kept value (cleared when it was given), if any.
    pub fn take(&self) -> Option<T> {
        let got = self.free.lock().unwrap_or_else(|e| e.into_inner()).pop();
        match got {
            Some(_) => self.hits.fetch_add(1, Relaxed),
            None => self.misses.fetch_add(1, Relaxed),
        };
        got
    }

    /// Keep `t` (the caller cleared it) unless the list is full.
    pub fn give(&self, t: T) {
        let mut free = self.free.lock().unwrap_or_else(|e| e.into_inner());
        if free.len() < self.cap {
            free.push(t);
        }
    }

    /// Values kept now.
    pub fn len(&self) -> usize {
        self.free.lock().unwrap_or_else(|e| e.into_inner()).len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// (takes served from the list, takes that found it empty) so far.
    pub fn stats(&self) -> (u64, u64) {
        (self.hits.load(Relaxed), self.misses.load(Relaxed))
    }
}

/// Every free list's (hits, misses), summed (tests: proves the flag-on arm
/// recycled).
pub fn stats() -> (u64, u64) {
    let all = [
        crate::position::position_cache_recycler_stats(),
        crate::order_book::account_margins_recycler_stats(),
        crate::order_book::reduce_only_recycler_stats(),
    ];
    all.iter().fold((0, 0), |(h, m), (a, b)| (h + a, m + b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn take_is_lifo_and_give_respects_the_cap() {
        let r: Recycler<Vec<u8>> = Recycler::new(2);
        assert_eq!(r.take(), None);
        r.give(vec![1]);
        r.give(vec![2]);
        r.give(vec![3]); // full: dropped
        assert_eq!(r.len(), 2);
        assert_eq!(r.take(), Some(vec![2]));
        assert_eq!(r.take(), Some(vec![1]));
        assert_eq!(r.take(), None);
        assert_eq!(r.stats(), (2, 2));
    }
}
