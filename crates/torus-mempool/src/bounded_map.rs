//! A size-bounded map for per-client limiter state (anti-spam items B, D).
//!
//! Two generations: lookups hit `current`, then `previous` (moving the entry
//! up); when `current` holds half the bound, `previous` is dropped and
//! `current` becomes `previous`. Total entries never exceed the bound, every
//! operation is O(1) amortized, and a key idle for a whole generation is
//! forgotten (it comes back with fresh state).

use std::collections::HashMap;
use std::hash::Hash;

pub struct TwoGen<K, V> {
    current: HashMap<K, V>,
    previous: HashMap<K, V>,
    half: usize,
}

impl<K: Hash + Eq + Copy, V> TwoGen<K, V> {
    /// `max` entries at most (at least 2).
    pub fn new(max: usize) -> Self {
        Self {
            current: HashMap::new(),
            previous: HashMap::new(),
            half: (max / 2).max(1),
        }
    }

    pub fn get(&self, key: &K) -> Option<&V> {
        self.current.get(key).or_else(|| self.previous.get(key))
    }

    /// The entry for `key` in `current` (moved up from `previous`, or
    /// `init()`), rotating generations first when `current` is at its half.
    pub fn entry_with(&mut self, key: K, init: impl FnOnce() -> V) -> &mut V {
        if !self.current.contains_key(&key) {
            let v = self.previous.remove(&key).unwrap_or_else(init);
            if self.current.len() >= self.half {
                self.previous = std::mem::take(&mut self.current);
            }
            self.current.insert(key, v);
        }
        self.current.get_mut(&key).expect("just inserted")
    }

    pub fn len(&self) -> usize {
        self.current.len() + self.previous.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_and_keeps_active_keys() {
        let mut m: TwoGen<u32, u32> = TwoGen::new(8);
        *m.entry_with(0, || 0) = 42;
        for k in 1..1_000u32 {
            *m.entry_with(k, || 0) += 1;
            // key 0 stays active
            assert_eq!(*m.entry_with(0, || 0), 42);
            assert!(m.len() <= 8, "{}", m.len());
        }
        assert!(m.get(&1).is_none(), "idle key forgotten");
    }
}
