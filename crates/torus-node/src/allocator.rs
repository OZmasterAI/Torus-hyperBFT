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

    #[cfg(all(feature = "jemalloc", target_os = "linux"))]
    #[test]
    fn stats_are_readable_and_published() {
        let s = stats().expect("jemalloc stats");
        assert!(s.allocated > 0, "{s:?}");
        assert!(s.active >= s.allocated, "{s:?}");
        assert!(s.resident >= s.active, "{s:?}");
        assert!(s.mapped >= s.active, "{s:?}");
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
