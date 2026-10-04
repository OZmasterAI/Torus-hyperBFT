//! s87 perf fixes: a `StateBackend` wrapper counting the reads the work-count
//! tests pin — prefix scans of `CF_NATIVE_POSITIONS` (one per
//! `positions_for_trader`) per prefix, and point reads of `CF_NATIVE_ORACLE`.
//! Delegates every method; counts only while armed.
//!
//! Item 6 Phase 1 (step 0.2): with [`CountingBackend::arm_storage_probe`] it
//! also records every read of `CF_NATIVE_POSITIONS` / `CF_NATIVE_BALANCES`
//! that reached RocksDB, i.e. that no overlay layer (own pending, parent, and
//! later the resident rows) answered. Wrap the block's `NativeStateOverlay`:
//! the overlay's DB fallback runs synchronously on the calling thread, so the
//! thread's RocksDB perf context moves exactly when the call touched RocksDB.
//! Each such read keeps its stack, for attribution to an engine path.

#![allow(dead_code)]

use std::backtrace::Backtrace;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use rocksdb::perf::{set_perf_stats, PerfContext, PerfMetric, PerfStatsLevel};
use torus_state::cf::{CF_NATIVE_BALANCES, CF_NATIVE_LIQUIDATION, CF_NATIVE_ORACLE, CF_NATIVE_POSITIONS};
use torus_state::error::StateError;
use torus_state::{AtomicWriteOp, StateBackend};

/// One read of `CF_NATIVE_POSITIONS` / `CF_NATIVE_BALANCES` that reached RocksDB.
pub struct StorageRead {
    pub cf: &'static str,
    /// `get_cf_raw` | `iterate_cf` | `iterate_cf_from` | `prefix_exists`.
    pub op: &'static str,
    pub stack: Backtrace,
}

#[derive(Default)]
pub struct Counts {
    armed: AtomicBool,
    position_scans: Mutex<HashMap<Vec<u8>, usize>>,
    oracle_reads: AtomicUsize,
    /// While armed: puts of a liquidation cooldown row (`0x02 ‖ trader`).
    cooldown_puts: AtomicUsize,
    probe: AtomicBool,
    storage_reads: Mutex<Vec<StorageRead>>,
    /// Calls of the wrapped backend for the two probed CFs, `(cf, op)` ->
    /// count, answered by a layer or not.
    layer_calls: Mutex<HashMap<(&'static str, &'static str), usize>>,
    /// Item 6 C3: while armed, every `iterate_cf(CF_NATIVE_POSITIONS,
    /// Some(prefix))` (answered by any layer) with its prefix and stack.
    scan_probe: AtomicBool,
    scans: Mutex<Vec<PositionScan>>,
}

/// Item 6 C3: one prefix scan of `CF_NATIVE_POSITIONS` (one
/// `positions_for_trader`), wherever it was answered.
pub struct PositionScan {
    pub prefix: Vec<u8>,
    pub stack: Backtrace,
}

#[derive(Clone)]
pub struct CountingBackend<T: StateBackend> {
    pub inner: T,
    pub counts: Arc<Counts>,
}

/// Counters of the calling thread's RocksDB perf context that move on any
/// memtable / SST access of a point read or an iterator (seek, next, block).
/// `MemTable::Get` skips its counter on an EMPTY memtable, so the probe test
/// keeps both CFs non-empty in the memtable (see `storage_probe_sees_*`).
fn rocks_touches() -> u64 {
    let pc = PerfContext::default();
    [
        PerfMetric::GetFromMemtableCount,
        PerfMetric::SeekOnMemtableCount,
        PerfMetric::NextOnMemtableCount,
        PerfMetric::PrevOnMemtableCount,
        PerfMetric::BlockCacheHitCount,
        PerfMetric::BlockReadCount,
        PerfMetric::GetReadBytes,
        PerfMetric::IterReadBytes,
        PerfMetric::UserKeyComparisonCount,
        PerfMetric::InternalKeySkippedCount,
        PerfMetric::InternalDeleteSkippedCount,
        PerfMetric::BloomSstHitCount,
        PerfMetric::BloomSstMissCount,
        PerfMetric::BloomMemtableHitCount,
        PerfMetric::BloomMemtableMissCount,
    ]
    .iter()
    .map(|&m| pc.metric(m))
    .fold(0u64, u64::wrapping_add)
}

fn probed_cf(cf: &str) -> Option<&'static str> {
    if cf == CF_NATIVE_POSITIONS {
        Some(CF_NATIVE_POSITIONS)
    } else if cf == CF_NATIVE_BALANCES {
        Some(CF_NATIVE_BALANCES)
    } else {
        None
    }
}

impl<T: StateBackend> CountingBackend<T> {
    pub fn new(inner: T) -> Self {
        Self { inner, counts: Arc::new(Counts::default()) }
    }

    /// The same counters over another backend (e.g. the next block's overlay).
    pub fn with_counts(inner: T, counts: Arc<Counts>) -> Self {
        Self { inner, counts }
    }

    /// Zero the counters and start counting.
    pub fn arm(&self) {
        self.counts.position_scans.lock().unwrap().clear();
        self.counts.oracle_reads.store(0, Ordering::SeqCst);
        self.counts.cooldown_puts.store(0, Ordering::SeqCst);
        self.counts.armed.store(true, Ordering::SeqCst);
    }

    pub fn disarm(&self) {
        self.counts.armed.store(false, Ordering::SeqCst);
    }

    /// Start (or resume) recording storage reads of the two probed CFs; keeps
    /// what was recorded so far ([`Self::take_storage_reads`] drains it).
    pub fn arm_storage_probe(&self) {
        self.counts.probe.store(true, Ordering::SeqCst);
    }

    pub fn disarm_storage_probe(&self) {
        self.counts.probe.store(false, Ordering::SeqCst);
    }

    pub fn take_storage_reads(&self) -> Vec<StorageRead> {
        std::mem::take(&mut *self.counts.storage_reads.lock().unwrap())
    }

    /// Start recording every positions prefix scan with its stack.
    pub fn arm_scan_probe(&self) {
        self.counts.scan_probe.store(true, Ordering::SeqCst);
    }

    pub fn disarm_scan_probe(&self) {
        self.counts.scan_probe.store(false, Ordering::SeqCst);
    }

    pub fn take_scans(&self) -> Vec<PositionScan> {
        std::mem::take(&mut *self.counts.scans.lock().unwrap())
    }

    pub fn take_layer_calls(&self) -> HashMap<(&'static str, &'static str), usize> {
        std::mem::take(&mut *self.counts.layer_calls.lock().unwrap())
    }

    /// `iterate_cf(CF_NATIVE_POSITIONS, Some(prefix))` calls for exactly `prefix`.
    pub fn position_scans(&self, prefix: &[u8]) -> usize {
        self.counts.position_scans.lock().unwrap().get(prefix).copied().unwrap_or(0)
    }

    /// All `iterate_cf(CF_NATIVE_POSITIONS, Some(_))` calls.
    pub fn all_position_scans(&self) -> usize {
        self.counts.position_scans.lock().unwrap().values().sum()
    }

    pub fn oracle_reads(&self) -> usize {
        self.counts.oracle_reads.load(Ordering::SeqCst)
    }

    pub fn cooldown_puts(&self) -> usize {
        self.counts.cooldown_puts.load(Ordering::SeqCst)
    }

    fn armed(&self) -> bool {
        self.counts.armed.load(Ordering::SeqCst)
    }

    /// Run one read of `cf`; while the storage probe is armed and `cf` is
    /// probed, record it if it touched RocksDB on this thread.
    fn probe<R>(&self, cf: &str, op: &'static str, read: impl FnOnce() -> R) -> R {
        let Some(cf) = probed_cf(cf).filter(|_| self.counts.probe.load(Ordering::SeqCst)) else {
            return read();
        };
        *self.counts.layer_calls.lock().unwrap().entry((cf, op)).or_insert(0) += 1;
        set_perf_stats(PerfStatsLevel::EnableCount);
        let before = rocks_touches();
        let out = read();
        if rocks_touches() != before {
            let stack = Backtrace::force_capture();
            self.counts.storage_reads.lock().unwrap().push(StorageRead { cf, op, stack });
        }
        out
    }
}

impl<T: StateBackend> StateBackend for CountingBackend<T> {
    fn get_cf_raw(&self, cf: &str, key: &[u8]) -> Result<Option<Vec<u8>>, StateError> {
        if cf == CF_NATIVE_ORACLE && self.armed() {
            self.counts.oracle_reads.fetch_add(1, Ordering::SeqCst);
        }
        self.probe(cf, "get_cf_raw", || self.inner.get_cf_raw(cf, key))
    }

    fn put_cf_raw(&self, cf: &str, key: &[u8], value: &[u8]) -> Result<(), StateError> {
        if cf == CF_NATIVE_LIQUIDATION && key.first() == Some(&0x02) && self.armed() {
            self.counts.cooldown_puts.fetch_add(1, Ordering::SeqCst);
        }
        self.inner.put_cf_raw(cf, key, value)
    }

    fn put_cf_raw_owned(&self, cf: &str, key: &[u8], value: Vec<u8>) -> Result<(), StateError> {
        self.inner.put_cf_raw_owned(cf, key, value)
    }

    fn delete_cf_raw(&self, cf: &str, key: &[u8]) -> Result<(), StateError> {
        self.inner.delete_cf_raw(cf, key)
    }

    fn iterate_cf(&self, cf: &str, prefix: Option<&[u8]>) -> Result<Vec<(Vec<u8>, Vec<u8>)>, StateError> {
        if let (true, Some(p)) = (cf == CF_NATIVE_POSITIONS && self.armed(), prefix) {
            *self.counts.position_scans.lock().unwrap().entry(p.to_vec()).or_insert(0) += 1;
        }
        if let (true, Some(p)) = (cf == CF_NATIVE_POSITIONS && self.counts.scan_probe.load(Ordering::SeqCst), prefix) {
            let scan = PositionScan { prefix: p.to_vec(), stack: Backtrace::force_capture() };
            self.counts.scans.lock().unwrap().push(scan);
        }
        self.probe(cf, "iterate_cf", || self.inner.iterate_cf(cf, prefix))
    }

    fn iterate_cf_from(&self, cf: &str, start: &[u8], limit: usize) -> Result<Vec<(Vec<u8>, Vec<u8>)>, StateError> {
        self.probe(cf, "iterate_cf_from", || self.inner.iterate_cf_from(cf, start, limit))
    }

    fn prefix_exists(&self, cf: &str, prefix: &[u8]) -> Result<bool, StateError> {
        self.probe(cf, "prefix_exists", || self.inner.prefix_exists(cf, prefix))
    }

    fn layer_touches(&self, cf: &str, prefix: &[u8]) -> bool {
        self.inner.layer_touches(cf, prefix)
    }

    fn atomic_write(&self, ops: &[AtomicWriteOp<'_>]) -> Result<(), StateError> {
        self.inner.atomic_write(ops)
    }
}
