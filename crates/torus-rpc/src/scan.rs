//! R24: bounded work for public read RPCs.
//!
//! A response-byte cap (`TORUS_RPC_MAX_RESPONSE_MB`) does not bound the work a
//! read does BEFORE it answers: a small `torus_getOpenInterest` reply used to
//! walk every position row. Every read here either seeks to the rows it needs
//! (a market prefix, the best N book levels) or stops after
//! `TORUS_RPC_MAX_SCAN_ROWS` rows with [`RpcError::ScanLimitExceeded`]
//! (`-32005`), instead of finishing the scan first.
//!
//! A "row" is one key the RocksDB iterator returns (or one seek), whether or
//! not it is kept: filtered rows cost the same to read.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::Arc;

use crate::error::RpcError;
use crate::RpcState;

/// Default `TORUS_RPC_MAX_SCAN_ROWS`: rows one public read may examine.
pub(crate) const DEFAULT_MAX_SCAN_ROWS: usize = 250_000;
/// Env var: rows one public read may examine (R24).
pub(crate) const ENV_MAX_SCAN_ROWS: &str = "TORUS_RPC_MAX_SCAN_ROWS";
/// `torus_getOrderBook` levels per side: the default and the most a `depth`
/// parameter may ask for.
pub(crate) const ORDER_BOOK_MAX_LEVELS: usize = 1000;

/// Resolve `TORUS_RPC_MAX_SCAN_ROWS` from a raw env value. Pure (takes the raw
/// string) so it is testable without env races. Empty / unset / unparsable /
/// zero fall back to [`DEFAULT_MAX_SCAN_ROWS`].
pub(crate) fn resolve_max_scan_rows(raw: Option<&str>) -> usize {
    raw.map(str::trim)
        .and_then(|s| s.parse::<usize>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(DEFAULT_MAX_SCAN_ROWS)
}

/// The rows one read may still examine. Dropping it adds the rows it counted
/// to the server's `scan_rows_examined` total (error paths included).
pub(crate) struct ScanBudget {
    max: usize,
    examined: usize,
    sink: Option<Arc<AtomicU64>>,
}

impl ScanBudget {
    /// A budget of `max` rows, not reported anywhere (unit tests, helpers).
    pub(crate) fn new(max: usize) -> Self {
        Self {
            max,
            examined: 0,
            sink: None,
        }
    }

    /// Count one examined row; `ScanLimitExceeded` once more than `max` rows
    /// were examined (so a read touches at most `max + 1` rows).
    pub(crate) fn step(&mut self) -> Result<(), RpcError> {
        self.examined += 1;
        if self.examined > self.max {
            return Err(RpcError::ScanLimitExceeded { limit: self.max });
        }
        Ok(())
    }

    /// Rows examined so far.
    pub(crate) fn examined(&self) -> usize {
        self.examined
    }
}

impl Drop for ScanBudget {
    fn drop(&mut self) {
        if let Some(sink) = &self.sink {
            sink.fetch_add(self.examined as u64, Relaxed);
        }
    }
}

impl RpcState {
    /// A fresh per-request budget of `max_scan_rows`, reported to
    /// `scan_rows_examined` when dropped.
    pub(crate) fn scan_budget(&self) -> ScanBudget {
        ScanBudget {
            max: self.max_scan_rows,
            examined: 0,
            sink: Some(self.scan_rows_examined.clone()),
        }
    }
}

#[cfg(test)]
#[path = "scan_tests.rs"]
mod scan_tests;
