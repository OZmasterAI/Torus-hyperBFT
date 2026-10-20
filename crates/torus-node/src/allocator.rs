//! Process allocator: jemalloc when built with the default `jemalloc` feature
//! on Linux (the `#[global_allocator]` is in `main.rs`), else the system
//! allocator. jemalloc is built unprefixed, so it also serves C/C++ `malloc`
//! (RocksDB), the same coverage as `LD_PRELOAD=libjemalloc.so`.

/// True when this binary runs on jemalloc.
pub const JEMALLOC: bool = cfg!(all(feature = "jemalloc", target_os = "linux"));

/// Allocator name for the startup log line.
pub const NAME: &str = if JEMALLOC { "jemalloc" } else { "system" };

/// One snapshot of jemalloc's process-wide byte counters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stats {
    pub allocated: u64,
    pub active: u64,
    pub resident: u64,
    pub retained: u64,
    pub mapped: u64,
}

#[cfg(all(feature = "jemalloc", target_os = "linux"))]
mod imp {
    use super::Stats;
    use tikv_jemalloc_ctl::{epoch, stats};

    pub fn version() -> Option<&'static str> {
        tikv_jemalloc_ctl::version::read().ok()
    }

    pub fn stats() -> Option<Stats> {
        // The stats are cached per epoch; advancing it refreshes them.
        epoch::advance().ok()?;
        Some(Stats {
            allocated: stats::allocated::read().ok()? as u64,
            active: stats::active::read().ok()? as u64,
            resident: stats::resident::read().ok()? as u64,
            retained: stats::retained::read().ok()? as u64,
            mapped: stats::mapped::read().ok()? as u64,
        })
    }
}

#[cfg(not(all(feature = "jemalloc", target_os = "linux")))]
mod imp {
    use super::Stats;

    pub fn version() -> Option<&'static str> {
        None
    }

    pub fn stats() -> Option<Stats> {
        None
    }
}

/// jemalloc's version string; `None` on the system allocator.
pub fn version() -> Option<&'static str> {
    imp::version()
}

/// Current jemalloc stats; `None` on the system allocator.
pub fn stats() -> Option<Stats> {
    imp::stats()
}

/// Copy a snapshot into the `torus_jemalloc_*_bytes` gauges.
pub fn publish(m: &torus_telemetry::JemallocMetrics, s: &Stats) {
    m.allocated.set(s.allocated as i64);
    m.active.set(s.active as i64);
    m.resident.set(s.resident as i64);
    m.retained.set(s.retained as i64);
    m.mapped.set(s.mapped as i64);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// With the feature on, the global allocator IS jemalloc: an allocation
    /// made through `Vec` (Rust's global allocator) shows up in jemalloc's
    /// per-thread allocated-bytes counter.
    #[cfg(all(feature = "jemalloc", target_os = "linux"))]
    #[test]
    fn global_allocator_is_jemalloc() {
        use tikv_jemalloc_ctl::thread;
        assert_eq!(NAME, "jemalloc");
        let counter = thread::allocatedp::mib().unwrap().read().unwrap();
        const N: usize = 8 << 20;
        let before = counter.get();
        let v: Vec<u8> = std::hint::black_box(Vec::with_capacity(N));
        let after = counter.get();
        drop(v);
        assert!(
            after - before >= N as u64,
            "an {N}-byte Vec did not go through jemalloc: thread.allocated {before} -> {after}"
        );
        let v = version();
        assert!(v.is_some_and(|v| v.starts_with('5')), "{v:?}");
    }

    /// C `malloc` is jemalloc too (unprefixed build), so RocksDB's heap is on it.
    #[cfg(all(feature = "jemalloc", target_os = "linux"))]
    #[test]
    fn c_malloc_is_jemalloc() {
        use tikv_jemalloc_ctl::thread;
        let counter = thread::allocatedp::mib().unwrap().read().unwrap();
        const N: usize = 8 << 20;
        let before = counter.get();
        let p = unsafe { libc::malloc(N) };
        let after = counter.get();
        assert!(!p.is_null());
        unsafe { libc::free(p) };
        assert!(
            after - before >= N as u64,
            "libc::malloc({N}) did not go through jemalloc: thread.allocated {before} -> {after}"
        );
    }

    /// The orderings jemalloc documents (jemalloc(3), 5.3): `stats.active` is
    /// ">= stats.allocated", `stats.resident` is "larger than stats.active",
    /// `stats.mapped` is "larger than stats.active"; `active` and `resident`
    /// are page multiples. There is "no strict ordering" between `mapped` and
    /// `resident`, and `retained` is excluded from `mapped` and unrelated to
    /// the others, so neither pair is compared.
    ///
    /// Those orderings hold for a QUIESCENT process only. `epoch::advance`
    /// (ctl_refresh) merges each arena's counters with relaxed atomic loads,
    /// in sequence and without stopping other threads: `active` is the
    /// arena's `nactive` read in `arena_basic_stats_merge`; `allocated`
    /// (bins, large stats) is read after it; `resident` is a SECOND read of
    /// `nactive` + `ndirty` in `pa_shard_stats_merge`. Another thread
    /// allocating in between gives `allocated > active`; one freeing pages
    /// that are purged in between (freed pages move to dirty, a purge drops
    /// them) gives `resident < active`. Seen once under heavy host load
    /// (active 50982912, resident 49717248). Measured (s131, a throwaway
    /// probe: 4 threads churning 0.25-4 MiB Vecs, 20k snapshots): 1089 with
    /// `active < allocated`; with `MALLOC_CONF=dirty_decay_ms:0` also 1 with
    /// `resident < active`. So the orderings are asserted on SOME snapshot
    /// out of a bounded number: a torn snapshot is retried, while a wrong
    /// field mapping in `stats()` (or stats that read 0) fails every attempt.
    #[cfg(all(feature = "jemalloc", target_os = "linux"))]
    #[test]
    fn stats_are_readable_and_published() {
        const PAGE: u64 = 4096; // jemalloc's page is a power of two >= 4 KiB
        const ATTEMPTS: usize = 100;
        let consistent = |s: &Stats| {
            s.allocated > 0
                && s.active >= s.allocated
                && s.resident >= s.active
                && s.mapped >= s.active
                && s.active % PAGE == 0
                && s.resident % PAGE == 0
        };
        let mut torn = Vec::new();
        let s = loop {
            let s = stats().expect("jemalloc stats");
            if consistent(&s) {
                break s;
            }
            torn.push(s);
            assert!(
                torn.len() < ATTEMPTS,
                "no consistent jemalloc snapshot in {ATTEMPTS} tries; first and last: {:?} {:?}",
                torn[0],
                torn[torn.len() - 1]
            );
            std::thread::sleep(std::time::Duration::from_millis(1));
        };
        let m = torus_telemetry::Metrics::new();
        publish(&m.jemalloc, &s);
        let text = m.encode();
        let line = format!("torus_jemalloc_allocated_bytes {}", s.allocated);
        assert!(text.lines().any(|l| l == line), "{line} missing:\n{text}");
    }

    #[cfg(not(all(feature = "jemalloc", target_os = "linux")))]
    #[test]
    fn system_allocator_when_feature_off() {
        assert_eq!(NAME, "system");
        assert_eq!(version(), None);
        assert_eq!(stats(), None);
    }
}
