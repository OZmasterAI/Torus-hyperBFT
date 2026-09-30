//! Hash maps for the execution path (s82).
//!
//! std's SipHash was ~9% of the execution thread (s77 profile). ahash is
//! ~5-10x cheaper on our keys and is still seeded per process from the OS
//! RNG (`runtime-rng`), so keys a trader chooses (addresses, prices) cannot be
//! aimed at collisions. Never swap in an unseeded hasher (e.g. plain FxHash).
//!
//! Iteration order is random per process, as with std: nothing that feeds
//! consensus may depend on it.

pub type FastMap<K, V> = std::collections::HashMap<K, V, ahash::RandomState>;
pub type FastSet<T> = std::collections::HashSet<T, ahash::RandomState>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hasher_is_randomly_seeded_per_map() {
        let a = FastMap::<u128, ()>::default();
        let b = FastMap::<u128, ()>::default();
        assert_ne!(a.hasher().hash_one(1u128), b.hasher().hash_one(1u128));
    }
}
