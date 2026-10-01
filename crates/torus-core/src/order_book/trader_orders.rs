//! One trader's resting order ids in one book, in `trader_orders` order
//! (arrival order; decode/load order after a reload). `cancel_all` returns
//! orders in this order, so removal must never reorder it.

use std::collections::HashMap;
use std::fmt;

use torus_types::OrderId;

/// Marks a removed slot (no real order id reaches it).
const HOLE: OrderId = OrderId::MAX;

/// Ordered ids with O(1) amortized removal: a removed id leaves a hole, and
/// holes are compacted away (order kept) once they outnumber the live ids.
/// Replaces a `Vec<OrderId>` whose `retain` cost O(n) per cancel, maker fill
/// and STP cancel, with n up to the 5000 open-order limit.
#[derive(Clone, Default)]
pub(super) struct TraderOrders {
    slots: Vec<OrderId>,
    /// Slot of each id (the last one if a corrupt list repeats an id).
    pos: HashMap<OrderId, usize>,
    live: usize,
}

impl TraderOrders {
    pub(super) fn push(&mut self, id: OrderId) {
        debug_assert_ne!(id, HOLE);
        self.pos.insert(id, self.slots.len());
        self.slots.push(id);
        self.live += 1;
    }

    pub(super) fn remove(&mut self, id: OrderId) {
        let Some(slot) = self.pos.remove(&id) else {
            return;
        };
        self.slots[slot] = HOLE;
        self.live -= 1;
        if self.slots.len() >= 32 && self.slots.len() > 2 * self.live {
            self.slots.retain(|&id| id != HOLE);
            for (slot, id) in self.slots.iter().enumerate() {
                self.pos.insert(*id, slot);
            }
        }
    }

    pub(super) fn len(&self) -> usize {
        self.live
    }

    pub(super) fn is_empty(&self) -> bool {
        self.live == 0
    }

    pub(super) fn iter(&self) -> impl Iterator<Item = &OrderId> {
        self.slots.iter().filter(|&&id| id != HOLE)
    }

    pub(super) fn to_vec(&self) -> Vec<OrderId> {
        self.iter().copied().collect()
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
    }
}
