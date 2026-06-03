use std::collections::{HashMap, VecDeque};
use ed25519_dalek::VerifyingKey;

/// Bounded per-peer buffer of items awaiting peer (re)connection.
///
/// Generic over payload so it is unit-testable without constructing a live
/// consensus `Message`. Mirrors the drop-on-full policy of `enqueue_inbound`
/// (swarm.rs), but drops the OLDEST entry when a per-key queue is full — a
/// stale vote/bundle is the least useful thing to keep.
pub struct PendingSendQueue<T> {
    map: HashMap<[u8; 32], VecDeque<T>>,
    max_per_key: usize,
}

impl<T> PendingSendQueue<T> {
    pub fn new(max_per_key: usize) -> Self {
        Self { map: HashMap::new(), max_per_key }
    }

    /// Buffer `item` for `vk`, evicting the oldest if the per-key cap is hit.
    pub fn enqueue(&mut self, vk: &VerifyingKey, item: T) {
        let q = self.map.entry(vk.to_bytes()).or_default();
        if q.len() >= self.max_per_key {
            q.pop_front();
        }
        q.push_back(item);
    }

    /// Remove and return all queued items for `vk`, in enqueue order.
    pub fn flush(&mut self, vk: &VerifyingKey) -> Vec<T> {
        self.map.remove(&vk.to_bytes()).map(Vec::from).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{SigningKey, VerifyingKey};

    fn vk(seed: u8) -> VerifyingKey {
        let mut b = [0u8; 32];
        b[0] = seed;
        VerifyingKey::from(&SigningKey::from_bytes(&b))
    }

    #[test]
    fn flush_returns_in_order_then_clears() {
        let mut q = PendingSendQueue::<u32>::new(8);
        let k = vk(1);
        q.enqueue(&k, 1);
        q.enqueue(&k, 2);
        assert_eq!(q.flush(&k), vec![1, 2]);
        assert!(q.flush(&k).is_empty());
    }

    #[test]
    fn per_key_cap_drops_oldest() {
        let mut q = PendingSendQueue::<u32>::new(2);
        let k = vk(1);
        q.enqueue(&k, 1);
        q.enqueue(&k, 2);
        q.enqueue(&k, 3);
        assert_eq!(q.flush(&k), vec![2, 3]); // oldest evicted, bounded
    }

    #[test]
    fn keys_are_independent() {
        let mut q = PendingSendQueue::<u32>::new(8);
        q.enqueue(&vk(1), 10);
        q.enqueue(&vk(2), 20);
        assert_eq!(q.flush(&vk(1)), vec![10]);
        assert_eq!(q.flush(&vk(2)), vec![20]);
    }
}
