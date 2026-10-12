//! Item 6 Phase 2 step 0.2: the threads the block execution path spawns,
//! counted per site since process start (P2-3 sizes its pool from these).
//! Node-local instrumentation, never read by execution: one relaxed add per
//! spawn batch. app.rs exports the totals as the `torus_exec_thread_spawns_*`
//! gauges (names: `torus_telemetry::EXEC_SPAWN_SITES`, in [`SpawnSite`]
//! order).

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

/// A place on the execution path that spawns threads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpawnSite {
    /// `MarketWorkerPool::match_parallel_capped_with`: matching.
    Match,
    /// `settle_market_results_parallel`: settle pass A.
    Settle,
    /// `drain_books_parallel`: save books, pass 1.
    SaveBooks,
    /// `phase2_parallel_prepare`: the sharded margin phase.
    MarginPrepare,
    /// `torus_core::order_book::open_order_counts`: margin phase.
    OpenOrders,
    /// `end_resident_on_worker`: one named thread per native block.
    EndResident,
    /// `flush_pending_after_batch`: the running-hash digest of a large block.
    FlushDigest,
    /// `native_trie::run_buckets`: bucket hashing (trie maintenance on).
    RootBuckets,
    /// `load_order_books_levels`: book load (start-up, rebuilds).
    LoadBooks,
}

impl SpawnSite {
    pub const COUNT: usize = 9;
}

static SPAWNS: [AtomicU64; SpawnSite::COUNT] = [const { AtomicU64::new(0) }; SpawnSite::COUNT];

thread_local! {
    /// P2-3: the same counts, for the threads the CALLING thread spawned
    /// (tests: exact per test thread, not raced by other tests in a
    /// one-process run).
    static THREAD_SPAWNS: [std::cell::Cell<u64>; SpawnSite::COUNT] =
        const { [const { std::cell::Cell::new(0) }; SpawnSite::COUNT] };
}

/// Count `n` threads spawned at `site`.
pub fn add(site: SpawnSite, n: usize) {
    SPAWNS[site as usize].fetch_add(n as u64, Relaxed);
    THREAD_SPAWNS.with(|c| c[site as usize].set(c[site as usize].get() + n as u64));
}

/// Threads spawned per site since process start, in [`SpawnSite`] order.
pub fn totals() -> [u64; SpawnSite::COUNT] {
    std::array::from_fn(|i| SPAWNS[i].load(Relaxed))
}

/// Threads spawned per site by the calling thread since it started, in
/// [`SpawnSite`] order.
pub fn thread_totals() -> [u64; SpawnSite::COUNT] {
    THREAD_SPAWNS.with(|c| std::array::from_fn(|i| c[i].get()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Process-wide counters: other tests may spawn concurrently, so the
    /// deltas are lower bounds.
    #[test]
    fn add_counts_at_its_site() {
        let before = totals();
        add(SpawnSite::LoadBooks, 3);
        add(SpawnSite::Match, 2);
        let after = totals();
        assert!(after[SpawnSite::LoadBooks as usize] >= before[SpawnSite::LoadBooks as usize] + 3);
        assert!(after[SpawnSite::Match as usize] >= before[SpawnSite::Match as usize] + 2);
        assert_eq!(SpawnSite::LoadBooks as usize, SpawnSite::COUNT - 1);
    }

    /// P2-3: the per-thread totals are exact for the calling thread and see
    /// nothing another thread counted.
    #[test]
    fn thread_totals_count_the_calling_thread_only() {
        let before = thread_totals();
        add(SpawnSite::Settle, 4);
        std::thread::spawn(|| add(SpawnSite::Settle, 100)).join().unwrap();
        let after = thread_totals();
        assert_eq!(after[SpawnSite::Settle as usize], before[SpawnSite::Settle as usize] + 4);
        assert_eq!(after[SpawnSite::Match as usize], before[SpawnSite::Match as usize]);
    }
}
