//! s87 perf fixes: a `StateBackend` wrapper counting the reads the work-count
//! tests pin — prefix scans of `CF_NATIVE_POSITIONS` (one per
//! `positions_for_trader`) per prefix, and point reads of `CF_NATIVE_ORACLE`.
//! Delegates every method; counts only while armed.

#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use torus_state::cf::{CF_NATIVE_ORACLE, CF_NATIVE_POSITIONS};
use torus_state::error::StateError;
use torus_state::{AtomicWriteOp, StateBackend};

#[derive(Default)]
pub struct Counts {
    armed: AtomicBool,
    position_scans: Mutex<HashMap<Vec<u8>, usize>>,
    oracle_reads: AtomicUsize,
}

#[derive(Clone)]
pub struct CountingBackend<T: StateBackend> {
    pub inner: T,
    pub counts: Arc<Counts>,
}

impl<T: StateBackend> CountingBackend<T> {
    pub fn new(inner: T) -> Self {
        Self { inner, counts: Arc::new(Counts::default()) }
    }

    /// Zero the counters and start counting.
    pub fn arm(&self) {
        self.counts.position_scans.lock().unwrap().clear();
        self.counts.oracle_reads.store(0, Ordering::SeqCst);
        self.counts.armed.store(true, Ordering::SeqCst);
    }

    pub fn disarm(&self) {
        self.counts.armed.store(false, Ordering::SeqCst);
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

    fn armed(&self) -> bool {
        self.counts.armed.load(Ordering::SeqCst)
    }
}

impl<T: StateBackend> StateBackend for CountingBackend<T> {
    fn get_cf_raw(&self, cf: &str, key: &[u8]) -> Result<Option<Vec<u8>>, StateError> {
        if cf == CF_NATIVE_ORACLE && self.armed() {
            self.counts.oracle_reads.fetch_add(1, Ordering::SeqCst);
        }
        self.inner.get_cf_raw(cf, key)
    }

    fn put_cf_raw(&self, cf: &str, key: &[u8], value: &[u8]) -> Result<(), StateError> {
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
        self.inner.iterate_cf(cf, prefix)
    }

    fn iterate_cf_from(&self, cf: &str, start: &[u8], limit: usize) -> Result<Vec<(Vec<u8>, Vec<u8>)>, StateError> {
        self.inner.iterate_cf_from(cf, start, limit)
    }

    fn prefix_exists(&self, cf: &str, prefix: &[u8]) -> Result<bool, StateError> {
        self.inner.prefix_exists(cf, prefix)
    }

    fn atomic_write(&self, ops: &[AtomicWriteOp<'_>]) -> Result<(), StateError> {
        self.inner.atomic_write(ops)
    }
}
