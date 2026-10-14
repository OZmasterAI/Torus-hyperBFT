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

#[cfg(any(test, debug_assertions, feature = "test-reference-paths"))]
use std::collections::BTreeMap;

use alloy_primitives::map::HashMap;
use alloy_primitives::Address;
use torus_core::order_book::OrderBook;
use torus_types::MarketId;

#[derive(Default, Debug)]
pub(crate) struct OpenOrders {
    /// Users with at least one open order; absent = 0.
    map: HashMap<Address, u32>,
}

impl OpenOrders {
    /// Apply every book's open-order log (and empty it). Returns the log
    /// entries applied (one per (book, user) changed since the last absorb).
    pub(crate) fn absorb(&mut self, books: &mut HashMap<MarketId, OrderBook>) -> u64 {
        let mut applied = 0u64;
        let map = &mut self.map;
        for book in books.values_mut() {
            if book.open_log_len() == 0 {
                continue;
            }
            book.drain_open_log(|trader, delta| {
                applied += 1;
                Self::apply(map, trader, delta);
            });
        }
        applied
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

    /// Count from the books (their logs cleared): the first count after a
    /// load, or after a market worker panic consumed a book.
    pub(crate) fn build(books: &mut HashMap<MarketId, OrderBook>) -> Self {
        let mut map: HashMap<Address, u32> = HashMap::default();
        for book in books.values_mut() {
            book.clear_open_log();
            book.for_each_open_count(|trader, n| {
                let e = map.entry(trader).or_default();
                *e = e.saturating_add(u32::try_from(n).unwrap_or(u32::MAX));
            });
        }
        Self { map }
    }

    /// Every user's count: this counter plus the books' logs not absorbed
    /// yet (tests and debug checks; does not absorb).
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
        let got = self.with_logs(books);
        let want = Self::recount(books);
        assert!(got == want, "step 5: open-order counter + logs != books\n{got:?}\n{want:?}");
    }
}
