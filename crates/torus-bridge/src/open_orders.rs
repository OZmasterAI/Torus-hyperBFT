//! Item 6 Phase 4 step 5 (plan decision 2 b): each user's open orders
//! (resting orders plus pending stops, summed over every book), the count
//! the per-user open-order cap (`take_open_slot`) reads.
//!
//! Kept from the books' open-order logs ([`OrderBook::drain_open_log`]):
//! every path that changes a trader's count in a book logs it there (place,
//! fill, cancel, cancel-all, modify, stop store / trigger / cancel, the
//! liquidation step's cancels and closes, the loads), on whichever thread
//! runs that book. The invariant: this counter plus every book's
//! not-yet-absorbed log equals the books' count of every user. The first
//! count after a load builds it from the books ([`Self::build`], clearing
//! their logs), like the cancel-all and cloid indexes; it is then carried
//! with the resident books (whose logs ride inside the books).
//! [`Self::absorb`] runs before every read; it applies each book's log
//! (exact integer sums: the order of books cannot change a count).
//! Node-local: never hashed or persisted; a restart rebuilds it from the
//! loaded books.
//!
//! Codex r1: the book map is public and can change under a built counter
//! (books moved in from another context, a book replaced by its reload,
//! removed). So the counter checks, before every absorb, that the map holds
//! exactly the books it counted ([`Self::covers`]): every book it drained
//! or built carries its id ([`OrderBook::open_log_owner`]), and it knows how
//! many there were. Fresh books (created or decoded: their log is their
//! whole count, e.g. a new market's book from a match worker) are absorbed
//! as they are. Anything else (a book owned by another counter or by
//! nobody, a counted book gone) and it counts again from the books
//! ([`Self::build`]), in every build profile: the cap can never read a
//! count the books do not hold.

#[cfg(any(test, debug_assertions, feature = "test-reference-paths"))]
use std::collections::BTreeMap;

use std::sync::atomic::{AtomicU64, Ordering};

use alloy_primitives::map::HashMap;
use alloy_primitives::Address;
use torus_core::order_book::OrderBook;
use torus_types::MarketId;

/// Counter ids, unique per process: never [`OrderBook::OPEN_LOG_FRESH`] (0)
/// or [`OrderBook::OPEN_LOG_UNKNOWN`] (`u64::MAX`, unreachable).
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Debug)]
pub(crate) struct OpenOrders {
    /// Users with at least one open order; absent = 0.
    map: HashMap<Address, u32>,
    /// This counter's id: every book it counted carries it
    /// ([`OrderBook::open_log_owner`]).
    id: u64,
    /// Books carrying `id` (the map's size at the last build or absorb).
    books: usize,
}

impl OpenOrders {
    /// Apply every book's open-order log (and empty it). Returns the log
    /// entries applied (one per (book, user) changed since the last absorb).
    /// A map that is not the one this counter counted ([`Self::covers`]):
    /// counts again from the books instead (returns 0).
    pub(crate) fn absorb(&mut self, books: &mut HashMap<MarketId, OrderBook>) -> u64 {
        if !self.covers(books) {
            *self = Self::build(books);
            return 0;
        }
        let mut applied = 0u64;
        let (id, map) = (self.id, &mut self.map);
        for book in books.values_mut() {
            if book.open_log_len() == 0 && book.open_log_owner() == id {
                continue;
            }
            // A fresh book (whole count in its log) becomes ours here.
            book.drain_open_log(id, |trader, delta| {
                applied += 1;
                Self::apply(map, trader, delta);
            });
        }
        self.books = books.len();
        applied
    }

    /// Codex r1: `books` is the map this counter counted, give or take
    /// fresh books: every other book carries this counter's id, and there
    /// are as many as it counted. A book this counter drained cannot be
    /// duplicated (`OrderBook` is not `Clone`) and no other counter or path
    /// sets this id, so the owned books are exactly the counted ones. O(books).
    pub(crate) fn covers(&self, books: &HashMap<MarketId, OrderBook>) -> bool {
        let mut owned = 0usize;
        for book in books.values() {
            let owner = book.open_log_owner();
            if owner == self.id {
                owned += 1;
            } else if owner != OrderBook::OPEN_LOG_FRESH {
                return false;
            }
        }
        owned == self.books
    }

    /// Add `delta` to `trader`'s count. Never below 0 (the invariant; a
    /// book's delta applied to a counter that holds that book's old count).
    fn apply(map: &mut HashMap<Address, u32>, trader: Address, delta: i32) {
        use std::collections::hash_map::Entry;
        match map.entry(trader) {
            Entry::Occupied(mut e) => {
                let n = i64::from(*e.get()) + i64::from(delta);
                debug_assert!(n >= 0, "step 5: open-order count of {trader} below 0");
                if n <= 0 {
                    e.remove();
                } else {
                    *e.get_mut() = u32::try_from(n).unwrap_or(u32::MAX);
                }
            }
            Entry::Vacant(e) => {
                debug_assert!(delta > 0, "step 5: open-order count of {trader} below 0");
                if delta > 0 {
                    e.insert(delta as u32);
                }
            }
        }
    }

    /// `trader`'s open orders as of the last [`Self::absorb`].
    #[inline]
    pub(crate) fn get(&self, trader: &Address) -> u32 {
        self.map.get(trader).copied().unwrap_or(0)
    }

    /// Count from the books (their logs cleared, each now carrying the new
    /// counter's id): the first count after a load, after a market worker
    /// panic consumed a book, or when the map changed under the counter
    /// ([`Self::absorb`]).
    pub(crate) fn build(books: &mut HashMap<MarketId, OrderBook>) -> Self {
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let mut map: HashMap<Address, u32> = HashMap::default();
        for book in books.values_mut() {
            book.clear_open_log(id);
            book.for_each_open_count(|trader, n| {
                let e = map.entry(trader).or_default();
                *e = e.saturating_add(u32::try_from(n).unwrap_or(u32::MAX));
            });
        }
        Self { map, id, books: books.len() }
    }

    /// Every user's count: this counter plus the books' logs not absorbed
    /// yet (tests and debug checks; does not absorb). Meaningful while
    /// [`Self::covers`] holds.
    #[cfg(any(test, debug_assertions, feature = "test-reference-paths"))]
    pub(crate) fn with_logs(&self, books: &HashMap<MarketId, OrderBook>) -> BTreeMap<Address, u32> {
        let mut out: BTreeMap<Address, i64> =
            self.map.iter().map(|(t, n)| (*t, i64::from(*n))).collect();
        for book in books.values() {
            for (trader, delta) in book.open_log() {
                *out.entry(trader).or_default() += i64::from(delta);
            }
        }
        out.into_iter()
            .filter(|&(_, n)| n != 0)
            .map(|(t, n)| (t, u32::try_from(n).unwrap_or(u32::MAX)))
            .collect()
    }

    /// Every user's count over the books (the reference).
    #[cfg(any(test, debug_assertions, feature = "test-reference-paths"))]
    pub(crate) fn recount(books: &HashMap<MarketId, OrderBook>) -> BTreeMap<Address, u32> {
        let mut out: BTreeMap<Address, u32> = BTreeMap::new();
        for book in books.values() {
            book.for_each_open_count(|trader, n| *out.entry(trader).or_default() += n as u32);
        }
        out
    }

    /// Debug builds: the invariant (counter + logs == books), checked over
    /// every user. O(books' traders + stops).
    #[cfg(debug_assertions)]
    pub(crate) fn debug_assert_exact(&self, books: &HashMap<MarketId, OrderBook>) {
        assert!(self.covers(books), "step 5: the counter does not cover these books");
        let got = self.with_logs(books);
        let want = Self::recount(books);
        assert!(got == want, "step 5: open-order counter + logs != books\n{got:?}\n{want:?}");
    }
}
