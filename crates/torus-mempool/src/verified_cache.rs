//! Bounded, insertion-ordered FIFO cache backing the exec trust-cache.
//!
//! Maps a locally-verified native-action hash -> recovered sender so the
//! execution thread can skip a redundant secp256k1 recovery on a cache HIT
//! (see `docs/plans/double-verify-trust-cache-impl.md`). A HIT only ever returns
//! a sender that a *local* verify already produced; a MISS falls through to the
//! full recover + slash path, so this can never change the resolved sender and
//! therefore cannot fork the chain. Bounded so memory stays O(cap).

use std::collections::{HashMap, VecDeque};
use std::hash::Hash;

/// Insertion-ordered map with a hard capacity. On overflow the oldest *live* key
/// (by most-recent insert) is evicted. Reads never change eviction order (no LRU
/// recency bump) — keeping a HIT cheap (read-lock only) and behavior fully
/// deterministic.
///
/// Re-inserting a present key refreshes it to the fresh end; the prune-time
/// refresh-stash relies on this to bridge the <=64-block exec lag. Eviction is
/// lazy via per-key occurrence counts so every operation is O(1) amortized —
/// this matters because the cache rides the consensus prune path (~100
/// restashes/block) and the exec read path.
pub(crate) struct FifoCache<K, V> {
    /// key -> (value, number of live occurrences currently sitting in `order`).
    map: HashMap<K, (V, u32)>,
    /// Insertion order. May hold stale duplicate slots for keys that were
    /// re-inserted; reconciled against the occurrence count during eviction.
    order: VecDeque<K>,
    cap: usize,
}

impl<K: Hash + Eq + Clone, V: Clone> FifoCache<K, V> {
    pub fn new(cap: usize) -> Self {
        Self {
            map: HashMap::new(),
            order: VecDeque::new(),
            cap: cap.max(1),
        }
    }

    /// Insert or refresh `key -> value`. Returns the number of entries evicted
    /// (0 or 1). Re-inserting a present key refreshes its position to the fresh
    /// end without increasing the live size.
    pub fn insert(&mut self, key: K, value: V) -> u64 {
        match self.map.get_mut(&key) {
            Some(slot) => {
                slot.0 = value;
                slot.1 += 1; // a now-stale earlier copy still sits in `order`
            }
            None => {
                self.map.insert(key.clone(), (value, 1));
            }
        }
        self.order.push_back(key);

        let mut evicted = 0u64;
        while self.map.len() > self.cap {
            // Pop the oldest LIVE key, skipping stale duplicate slots.
            loop {
                let cand = self
                    .order
                    .pop_front()
                    .expect("order is non-empty while map exceeds cap");
                let remaining = {
                    let slot = self.map.get_mut(&cand).expect("ordered key is live");
                    slot.1 -= 1;
                    slot.1
                };
                if remaining == 0 {
                    self.map.remove(&cand);
                    evicted += 1;
                    break;
                }
                // else: stale slot for a since-refreshed key; keep popping.
            }
        }
        evicted
    }

    /// Look up `key`. Read-only: never affects eviction order (no recency bump).
    pub fn get(&self, key: &K) -> Option<&V> {
        self.map.get(key).map(|(v, _)| v)
    }

    /// Number of live entries.
    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.map.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verified_sender_cache_fifo_evicts() {
        // Insert past cap -> oldest evicted, newest present.
        let mut c: FifoCache<u32, u32> = FifoCache::new(3);
        c.insert(1, 10);
        c.insert(2, 20);
        c.insert(3, 30);
        assert_eq!(c.len(), 3);

        let evicted = c.insert(4, 40); // over cap -> evict oldest (key 1)
        assert_eq!(evicted, 1, "exactly one eviction");
        assert_eq!(c.len(), 3, "stays at cap");
        assert_eq!(c.get(&1), None, "oldest evicted");
        assert_eq!(c.get(&4), Some(&40), "newest present");
        assert_eq!(c.get(&2), Some(&20));
        assert_eq!(c.get(&3), Some(&30));
    }

    #[test]
    fn verified_sender_cache_read_does_not_bump_recency() {
        // A read must NOT save the oldest entry from eviction (no LRU recency).
        let mut c: FifoCache<u32, u32> = FifoCache::new(2);
        c.insert(1, 10);
        c.insert(2, 20);
        assert_eq!(c.get(&1), Some(&10)); // read the oldest
        c.insert(3, 30); // evicts oldest by INSERT order = key 1, despite the read
        assert_eq!(c.get(&1), None, "read must not refresh recency");
        assert_eq!(c.get(&2), Some(&20));
        assert_eq!(c.get(&3), Some(&30));
    }

    #[test]
    fn verified_sender_cache_reinsert_refreshes_to_back() {
        // Re-inserting a present key refreshes it to the fresh FIFO end — the
        // mechanism the prune-time restash uses to bridge the <=64-block exec lag.
        let mut c: FifoCache<u32, u32> = FifoCache::new(2);
        c.insert(1, 10);
        c.insert(2, 20);
        c.insert(1, 11); // refresh key 1 -> now key 2 is the oldest
        c.insert(3, 30); // evicts oldest live = key 2
        assert_eq!(c.get(&2), None, "un-refreshed key evicted first");
        assert_eq!(
            c.get(&1),
            Some(&11),
            "refreshed key survives with new value"
        );
        assert_eq!(c.get(&3), Some(&30));
        assert_eq!(c.len(), 2);
    }
}
