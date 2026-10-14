//! One trader's resting order ids in one book, in `trader_orders` order
//! (arrival order; decode/load order after a reload). `cancel_all` returns
//! orders in this order, so removal must never reorder it.

use alloy_primitives::map::HashMap;
use std::fmt;
use std::ops::Deref;

use torus_types::{Address, OrderId};

/// Marks a removed slot (no real order id reaches it).
const HOLE: OrderId = OrderId::MAX;

/// Lists of up to this many slots keep no position map: a removal scans and
/// shifts at most `SMALL` ids, cheaper than a map per (trader, book) pair.
const SMALL: usize = 32;

/// Ordered ids with O(1) amortized removal. Up to `SMALL` slots it is a plain
/// list (linear scan, order-keeping shift). Past that a position map is
/// built: a removed id leaves a hole, and holes are compacted away (order
/// kept) once they outnumber the live ids; a compaction down to `SMALL` live
/// ids drops the map again. Replaces a `Vec<OrderId>` whose `retain` cost
/// O(n) per cancel, maker fill and STP cancel, with n up to the 5000
/// open-order limit.
#[derive(Clone, Default)]
pub(super) struct TraderOrders {
    /// Ids in order; holes only while `large` is set.
    slots: Vec<OrderId>,
    large: Option<Box<Large>>,
}

#[derive(Clone)]
struct Large {
    /// Slot of each id (the last one if a corrupt list repeats an id).
    pos: HashMap<OrderId, usize>,
    live: usize,
}

impl TraderOrders {
    pub(super) fn push(&mut self, id: OrderId) {
        debug_assert_ne!(id, HOLE);
        match &mut self.large {
            Some(large) => {
                large.pos.insert(id, self.slots.len());
                large.live += 1;
            }
            None if self.slots.len() == SMALL => {
                let mut pos = HashMap::with_capacity_and_hasher(2 * SMALL, Default::default());
                for (slot, id) in self.slots.iter().enumerate() {
                    pos.insert(*id, slot);
                }
                pos.insert(id, SMALL);
                self.large = Some(Box::new(Large {
                    pos,
                    live: SMALL + 1,
                }));
            }
            None => {}
        }
        self.slots.push(id);
    }

    pub(super) fn remove(&mut self, id: OrderId) {
        let Some(large) = &mut self.large else {
            if let Some(slot) = self.slots.iter().rposition(|&x| x == id) {
                self.slots.remove(slot);
            }
            return;
        };
        let Some(slot) = large.pos.remove(&id) else {
            return;
        };
        self.slots[slot] = HOLE;
        large.live -= 1;
        if self.slots.len() >= SMALL && self.slots.len() > 2 * large.live {
            self.slots.retain(|&id| id != HOLE);
            if large.live <= SMALL {
                self.large = None;
            } else {
                for (slot, id) in self.slots.iter().enumerate() {
                    large.pos.insert(*id, slot);
                }
            }
        }
    }

    pub(super) fn len(&self) -> usize {
        self.large.as_ref().map_or(self.slots.len(), |large| large.live)
    }

    pub(super) fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub(super) fn iter(&self) -> impl Iterator<Item = &OrderId> {
        self.slots.iter().filter(|&&id| id != HOLE)
    }

    #[cfg(test)]
    pub(super) fn to_vec(&self) -> Vec<OrderId> {
        self.iter().copied().collect()
    }

    /// The live ids in order, reusing the list's allocation.
    pub(super) fn into_vec(mut self) -> Vec<OrderId> {
        if self.large.is_some() {
            self.slots.retain(|&id| id != HOLE);
        }
        self.slots
    }

    /// Tests: rewrite the list as a plain `Vec` (to build corrupt states).
    #[cfg(test)]
    pub(super) fn edit(&mut self, f: impl FnOnce(&mut Vec<OrderId>)) {
        let mut ids = self.to_vec();
        f(&mut ids);
        *self = Self::default();
        for id in ids {
            self.push(id);
        }
    }
}

impl PartialEq for TraderOrders {
    fn eq(&self, other: &Self) -> bool {
        self.iter().eq(other.iter())
    }
}

impl Eq for TraderOrders {}

impl fmt::Debug for TraderOrders {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.iter()).finish()
    }
}

/// Item 6 Phase 4 step 5: a book's `trader -> TraderOrders` map plus its
/// open-order log. Every change of a trader's resting-order count goes
/// through a method here and is logged, net per trader, until the executor
/// drains it ([`super::OrderBook::drain_open_log`]); pending stops log
/// through [`Self::note`] at their (few) sites in the book. Read access is
/// the plain map (`Deref`); there is no `DerefMut`, so no path can change a
/// count without logging it. The log is in-RAM bookkeeping only: never
/// serialized, hashed, or read by matching or saving.
#[derive(Default)]
pub(super) struct TraderIndex {
    map: HashMap<Address, TraderOrders>,
    /// Net change of each trader's open-order count (resting orders plus
    /// pending stops) in this book since the last drain; 0 entries possible.
    log: HashMap<Address, i32>,
    /// Who holds this book's count minus `log` (Codex r1): 0 (the default,
    /// [`super::OrderBook::OPEN_LOG_FRESH`]) = nobody, the log holds the
    /// whole count; otherwise the id the last drain or clear passed.
    owner: u64,
}

impl Deref for TraderIndex {
    type Target = HashMap<Address, TraderOrders>;

    fn deref(&self) -> &Self::Target {
        &self.map
    }
}

impl TraderIndex {
    /// Log a change of `trader`'s open-order count by `delta`.
    #[inline]
    pub(super) fn note(&mut self, trader: Address, delta: i32) {
        if delta != 0 {
            *self.log.entry(trader).or_default() += delta;
        }
    }

    /// Append `id` to `trader`'s list; true when the trader had no resting
    /// order in this book until now (absent or empty list).
    pub(super) fn push(&mut self, trader: Address, id: OrderId) -> bool {
        let ids = self.map.entry(trader).or_default();
        let was_empty = ids.is_empty();
        // `TraderOrders::push` always adds one to `len`.
        ids.push(id);
        self.note(trader, 1);
        was_empty
    }

    /// Remove `id` from `trader`'s list, keeping the (possibly empty) list
    /// (the matching sites).
    pub(super) fn remove_id(&mut self, trader: &Address, id: OrderId) {
        if let Some(ids) = self.map.get_mut(trader) {
            let before = ids.len();
            ids.remove(id);
            let delta = ids.len() as i64 - before as i64;
            self.note(*trader, delta as i32);
        }
    }

    /// Remove `id` from `trader`'s list; the list goes once empty.
    pub(super) fn remove_id_drop_empty(&mut self, trader: &Address, id: OrderId) {
        if let Some(ids) = self.map.get_mut(trader) {
            let before = ids.len();
            ids.remove(id);
            let (after, empty) = (ids.len(), ids.is_empty());
            if empty {
                self.map.remove(trader);
            }
            self.note(*trader, (after as i64 - before as i64) as i32);
        }
    }

    /// Take `trader`'s whole list.
    pub(super) fn remove_trader(&mut self, trader: &Address) -> Option<TraderOrders> {
        let ids = self.map.remove(trader)?;
        self.note(*trader, -(ids.len() as i64) as i32);
        Some(ids)
    }

    /// Hand every non-zero logged delta to `f`; the log is empty afterwards
    /// and `owner` holds the book's count.
    pub(super) fn drain_log(&mut self, owner: u64, mut f: impl FnMut(Address, i32)) {
        self.owner = owner;
        for (trader, delta) in self.log.drain() {
            if delta != 0 {
                f(trader, delta);
            }
        }
    }

    /// The logged deltas, not drained (0 entries skipped).
    pub(super) fn log(&self) -> impl Iterator<Item = (Address, i32)> + '_ {
        self.log.iter().filter(|(_, d)| **d != 0).map(|(t, d)| (*t, *d))
    }

    /// Entries in the log (0 entries included).
    pub(super) fn log_len(&self) -> usize {
        self.log.len()
    }

    /// Drop the log; `owner` counts the book from its orders.
    pub(super) fn clear_log(&mut self, owner: u64) {
        self.owner = owner;
        self.log.clear();
    }

    pub(super) fn owner(&self) -> u64 {
        self.owner
    }

    /// Tests: the map itself, NOT logged (to build corrupt states).
    #[cfg(test)]
    pub(super) fn map_mut(&mut self) -> &mut HashMap<Address, TraderOrders> {
        &mut self.map
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every removal pattern keeps the survivors in order, against a plain
    /// `Vec::retain` model, across compactions.
    #[test]
    fn removal_keeps_order_like_vec_retain() {
        let mut seed = 7u64;
        let mut next = || {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            seed >> 33
        };
        let mut t = TraderOrders::default();
        let mut model: Vec<OrderId> = Vec::new();
        let mut id = 1u128;
        for _ in 0..5000 {
            if next() % 3 == 0 && !model.is_empty() {
                let victim = model[next() as usize % model.len()];
                t.remove(victim);
                model.retain(|&x| x != victim);
            } else {
                t.push(id);
                model.push(id);
                id += 1;
            }
            assert_eq!(t.len(), model.len());
        }
        assert_eq!(t.to_vec(), model);
        t.remove(999_999_999);
        assert_eq!(t.to_vec(), model);
        for victim in model.clone() {
            t.remove(victim);
        }
        assert!(t.is_empty() && t.iter().next().is_none());
        assert!(t.slots.len() < 32, "compacted, {} slots", t.slots.len());
        assert!(t.large.is_none(), "an emptied list drops its position map");
    }

    /// A list of up to `SMALL` ids keeps no position map (the common case:
    /// a few orders per trader per book); it is built past `SMALL` and
    /// dropped again once compaction brings the list back to `SMALL`.
    #[test]
    fn position_map_only_past_small() {
        let mut t = TraderOrders::default();
        let mut model: Vec<OrderId> = Vec::new();
        for id in 1..=SMALL as OrderId {
            t.push(id);
            model.push(id);
        }
        assert!(t.large.is_none());
        t.remove(5);
        model.retain(|&x| x != 5);
        assert!(t.large.is_none() && t.slots == model, "small removal shifts, no holes");
        for id in 100..110 {
            t.push(id);
            model.push(id);
        }
        assert!(t.large.is_some());
        assert_eq!(t.len(), model.len());
        // Remove from the front until compaction fires at <= SMALL live.
        while t.large.is_some() {
            let victim = model.remove(0);
            t.remove(victim);
            assert_eq!(t.to_vec(), model);
        }
        assert!(model.len() <= SMALL);
        assert_eq!(t.slots, model, "back to a hole-free small list");
        t.push(7);
        model.push(7);
        assert_eq!(t.to_vec(), model);
    }

    /// `into_vec` hands back the live ids in order (small: the list itself,
    /// large: holes dropped) — what `cancel_all` consumes.
    #[test]
    fn into_vec_matches_to_vec_in_both_modes() {
        for n in [0, 3, SMALL, SMALL + 1, 200] {
            let mut t = TraderOrders::default();
            for id in 0..n as OrderId {
                t.push(id);
            }
            for id in (0..n as OrderId).step_by(3) {
                t.remove(id);
            }
            let want = t.to_vec();
            assert_eq!(t.clone().into_vec(), want, "n={n}");
        }
    }

    /// A corrupt list that repeats an id removes the LAST copy first in both
    /// modes (the large map keeps the last slot of a repeated id).
    #[test]
    fn repeated_id_removes_last_copy() {
        for extra in [0, SMALL as OrderId] {
            let mut t = TraderOrders::default();
            for id in [1, 2, 1, 3] {
                t.push(id);
            }
            for id in 100..100 + extra {
                t.push(id);
            }
            t.remove(1);
            let head: Vec<OrderId> = t.iter().copied().take(3).collect();
            assert_eq!(head, [1, 2, 3], "extra={extra}");
        }
    }

    /// The per-(trader, book) entry stays as small as the `Vec<OrderId>` it
    /// replaced plus one pointer.
    #[test]
    fn small_footprint() {
        assert!(std::mem::size_of::<TraderOrders>() <= 32);
    }

    /// Release probe: the s84 cell's lists between two cancel-alls (70k
    /// (trader, book) lists of Poisson(~2.7) ids: 250 senders x 800 orders
    /// over 300 books), filled by `push`, then emptied by `into_vec` as
    /// `cancel_all` does.
    ///   cargo test --release -p torus-core --lib trader_orders::tests::push_growth_cost -- --ignored --nocapture
    #[test]
    #[ignore = "release cost probe"]
    fn push_growth_cost() {
        let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
        const LISTS: u64 = 250 * 300;
        let mut lens = vec![0usize; LISTS as usize];
        for _ in 0..250 * 800 {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            lens[(x % LISTS) as usize] += 1;
        }
        lens.retain(|&n| n > 0);
        let mut ms = Vec::new();
        for _ in 0..41 {
            let t0 = std::time::Instant::now();
            let mut lists: Vec<TraderOrders> = Vec::with_capacity(lens.len());
            let mut id: OrderId = 1;
            // Round-robin over the lists, as interleaved batches fill books.
            let rounds = *lens.iter().max().unwrap();
            lists.resize_with(lens.len(), TraderOrders::default);
            for r in 0..rounds {
                for (list, &n) in lists.iter_mut().zip(&lens) {
                    if r < n {
                        list.push(id);
                        id += 1;
                    }
                }
            }
            let total: usize = lists.into_iter().map(|l| l.into_vec().len()).sum();
            ms.push(t0.elapsed().as_secs_f64() * 1e3);
            assert_eq!(total, 250 * 800);
        }
        ms.sort_by(f64::total_cmp);
        let over4 = lens.iter().filter(|&&n| n > 4).count();
        println!(
            "{} lists ({over4} over 4 ids), 200k pushes + into_vec: min {:.2} ms, median {:.2} ms",
            lens.len(),
            ms[0],
            ms[20]
        );
    }
}
