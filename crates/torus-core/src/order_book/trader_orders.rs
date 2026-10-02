//! One trader's resting order ids in one book, in `trader_orders` order
//! (arrival order; decode/load order after a reload). `cancel_all` returns
//! orders in this order, so removal must never reorder it.

use std::collections::HashMap;
use std::fmt;

use torus_types::OrderId;

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
                let mut pos = HashMap::with_capacity(2 * SMALL);
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
}
