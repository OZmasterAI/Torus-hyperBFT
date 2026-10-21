//! s132 (8r follow-up): prune the height-keyed history CFs without range
//! tombstones. Perf doc section 103.6: the state pruner's `delete_range_cf`
//! range deletes on `cf_block_bodies` doubled that CF's compaction on the
//! state DB (compaction CPU 0.077 -> 0.151 s/s per node, L0 3.3 vs 1.2).
//!
//! # Modes (`TORUS_PRUNE_MODE`, read once at state-DB open)
//!
//! - `filter` (default): the pruner persists the new cutoff (synced WAL),
//!   then raises the shared [`PruneCutoff`], then calls `DeleteFilesInRange`
//!   on `[0, cutoff)` of each CF below. Whole SST files below the cutoff are
//!   dropped (a manifest edit, no tombstone, no rewrite); the rows of files
//!   that straddle the cutoff are dropped by a compaction filter
//!   ([`PruneFilterFactory`]) during compactions RocksDB runs anyway.
//! - `range`: today's path, `delete_range_cf` per batch (kept for the A/B).
//!   No filter is installed.
//!
//! Anything else (unset included) is `filter`; an unknown value logs a warning.
//!
//! # CFs (height is the leading key part; [`HEIGHT_KEYED_CFS`])
//!
//! | CF | key | pruned rows |
//! |---|---|---|
//! | `cf_block_bodies` | height(8) | `key < cutoff` |
//! | `cf_block_action_status` | height(8) | `key < cutoff` |
//! | `cf_receipts` | height(8) ‖ tx_index(4) | `key < cutoff ‖ 0x00*4` |
//! | `cf_core_writer_tx` | target block(8) ‖ seq(8) | `key < cutoff ‖ 0x00*8` |
//!
//! "Pruned" ([`below_cutoff`]) is exactly the union of the range deletes the
//! `range` mode issues, `[bound(0), bound(cutoff))` with
//! `bound(h) = h ‖ 0x00*(len-8)`. The D13 trade CFs keep their own path (the
//! block is not the leading key part, so `DeleteFilesInRange` cannot help).
//!
//! # Lazy deletion and the readers
//!
//! In `filter` mode a row below the cutoff can still be in an SST until a
//! compaction (or `DeleteFilesInRange`) removes it. Every read of these CFs
//! that would see it goes through [`crate::StateDb`], which hides it
//! ([`PruneCutoff::is_hidden`]) in BOTH modes, so a reader gets exactly what
//! the eager range delete gives: absent below the cutoff, untouched at and
//! above it. The hidden accessors: `get_cf_raw`, `multi_get_cf_raw`,
//! `exists_cf_raw` and the `StateBackend` impl (`get_cf_raw`, `get_cf_len`,
//! `iterate_cf`, `iterate_cf_from`, `iterate_cf_prefix_from`,
//! `prefix_exists`). The readers of these CFs (s132 audit) and how each
//! stays identical:
//!
//! 1. RPC eth (`torus-rpc/src/eth.rs`): `get_body`, the per-block receipts
//!    loop, `eth_getTransactionReceipt` (also bounded by `check_pruned` on the
//!    RPC's `pruned_up_to`): `StateDb::get_cf_raw` -> hidden.
//! 2. RPC `torus_getBlockBody` (`torus-rpc/src/torus.rs`): body, action
//!    status and core-writer links, `StateDb::get_cf_raw` -> hidden.
//! 3. Crash replay (`torus-consensus` `load_replay_body`): `get_cf_raw` ->
//!    hidden; it only reads `(applied, committed]`, and the cutoff never
//!    passes the applied marker.
//! 4. RH4 DA body GC restore (`da_gc_worker.rs` `DaGcRunner::restore`):
//!    through `load_replay_body` -> hidden (a pruned height is "durable body
//!    unavailable", as with the range delete).
//! 5. Exec overlays / `ReadView` / checkpoint layer: these CFs are not
//!    ingress CFs; reads that miss the in-memory layers (which only hold
//!    heights above the applied marker) fall through to `StateDb` -> hidden.
//! 6. Snapshot export (`snapshot.rs`, RocksDB checkpoint = hard links): the
//!    copy carries the lingering rows and the persisted cutoff
//!    (`__prune_meta__` in `cf_block_headers`); any `StateDb` opened on it
//!    loads the cutoff at open (before the first read) -> hidden.
//!    `verify_snapshot` reads only the state-root CFs.
//! 7. Catch-up / block serving: HotStuff block sync serves the consensus
//!    DB's block tree and the native-DA heal serves `cf_native_pending`
//!    (consensus DB); neither reads these CFs. The explorer and
//!    `bench-throughput` read them only through the RPC (1, 2).
//! 8. No production code reads these CFs through `StateDb::inner()` /
//!    `db_arc()` raw handles or a RocksDB snapshot (the writers in the
//!    bridge committer, the exec pipeline and the commit path use raw
//!    handles for `WriteBatch` puts only). A new raw reader must check
//!    [`PruneCutoff::is_hidden`] itself.
//!
//! Every writer of these CFs writes heights above the applied marker
//! (commit, exec, heal, replay), and the cutoff never passes the applied
//! marker, so no row is written below the cutoff; if one were, it would be
//! hidden in both modes (the range mode only deletes `[pruned, cutoff)` on
//! each pass, so it would keep it on disk).
//!
//! # Boot and crash
//!
//! - Read side: [`crate::StateDb`] loads the persisted cutoff into
//!   [`PruneCutoff::hidden_below`] inside `open`, before it returns, so no
//!   read ever runs without it, whatever the retention setting.
//! - Filter side: [`PruneCutoff::filter_below`] starts at 0 on every open
//!   (the filter drops nothing). Only a pruner with `--retention-blocks` set
//!   arms it, from the same persisted cutoff, when it is built. Compactions
//!   that run between the open and the arming keep every row, so the filter
//!   never drops a row at or above the persisted cutoff, and never drops
//!   anything on a node with retention off.
//! - A pass persists the new cutoff with a synced WAL write BEFORE raising
//!   either atomic: a crash (or power loss) after a compaction dropped rows
//!   below the new cutoff always restarts with that cutoff, so no reader
//!   sees a half-pruned range.
//! - Compaction filters run on keys a RocksDB snapshot still sees (RocksDB
//!   >= 6). No reader of these CFs uses a snapshot (8 above).

use std::ffi::CStr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use rocksdb::compaction_filter::{CompactionFilter, Decision};
use rocksdb::compaction_filter_factory::{CompactionFilterContext, CompactionFilterFactory};

use crate::cf::{CF_BLOCK_ACTION_STATUS, CF_BLOCK_BODIES, CF_CORE_WRITER_TX, CF_RECEIPTS};

/// Env var selecting the prune mode (read once at state-DB open).
pub const PRUNE_MODE_ENV: &str = "TORUS_PRUNE_MODE";

/// How the state pruner removes the [`HEIGHT_KEYED_CFS`] rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PruneMode {
    /// Today's eager `delete_range_cf` (range tombstones).
    Range,
    /// Compaction filter + `DeleteFilesInRange` (no tombstones). Default.
    #[default]
    Filter,
}

/// Pure parse of `TORUS_PRUNE_MODE`: `range` = [`PruneMode::Range`];
/// anything else (unset included) = [`PruneMode::Filter`].
pub fn parse_prune_mode(raw: Option<&str>) -> PruneMode {
    match raw.map(str::trim) {
        Some("range") => PruneMode::Range,
        _ => PruneMode::Filter,
    }
}

/// [`parse_prune_mode`] of the environment; warns on an unknown value.
pub fn prune_mode_from_env() -> PruneMode {
    let raw = std::env::var(PRUNE_MODE_ENV).ok();
    if let Some(value) = raw.as_deref() {
        if !matches!(value.trim(), "range" | "filter" | "") {
            tracing::warn!(
                value,
                "unknown {PRUNE_MODE_ENV} (range | filter); using filter"
            );
        }
    }
    parse_prune_mode(raw.as_deref())
}

/// The CFs the state pruner removes by height, with their key length.
pub const HEIGHT_KEYED_CFS: [(&str, usize); 4] = [
    (CF_BLOCK_BODIES, 8),
    (CF_BLOCK_ACTION_STATUS, 8),
    (CF_RECEIPTS, 12),
    (CF_CORE_WRITER_TX, 16),
];

/// Key length of a [`HEIGHT_KEYED_CFS`] CF, `None` for any other CF.
#[inline]
pub fn height_keyed_key_len(cf: &str) -> Option<usize> {
    match cf {
        CF_BLOCK_BODIES | CF_BLOCK_ACTION_STATUS => Some(8),
        CF_RECEIPTS => Some(12),
        CF_CORE_WRITER_TX => Some(16),
        _ => None,
    }
}

/// `height ‖ 0x00*(key_len-8)`: the range-delete bound of `height`
/// (`key_len` in 8..=16).
#[inline]
pub fn height_bound(height: u64, key_len: usize) -> ([u8; 16], usize) {
    let mut b = [0u8; 16];
    b[..8].copy_from_slice(&height.to_be_bytes());
    (b, key_len.clamp(8, 16))
}

/// Is `key` (of a CF with `key_len`-byte keys) pruned at `cutoff`? Exactly
/// the union of the range mode's deletes: `bound(0) <= key < bound(cutoff)`.
#[inline]
pub fn below_cutoff(key: &[u8], key_len: usize, cutoff: u64) -> bool {
    if cutoff == 0 {
        return false;
    }
    let (lo, n) = height_bound(0, key_len);
    let (hi, _) = height_bound(cutoff, key_len);
    key >= &lo[..n] && key < &hi[..n]
}

/// Largest key `DeleteFilesInRange` may treat as below `cutoff` (its end is
/// inclusive): `(cutoff-1) ‖ 0xFF*(key_len-8)`. Every key `<=` it is below
/// `bound(cutoff)`; `None` for cutoff 0.
pub fn files_range_end(cutoff: u64, key_len: usize) -> Option<Vec<u8>> {
    let last = cutoff.checked_sub(1)?;
    let mut k = last.to_be_bytes().to_vec();
    k.resize(key_len.clamp(8, 16), 0xFF);
    Some(k)
}

/// The prune cutoff shared by every clone of a state DB handle, the
/// compaction filters and the pruner.
#[derive(Debug)]
pub struct PruneCutoff {
    mode: PruneMode,
    /// Reads hide rows below this (the persisted cutoff; both modes).
    hidden_below: AtomicU64,
    /// The compaction filter drops rows below this (filter mode, retention
    /// on; 0 = drop nothing). Never above `hidden_below`.
    filter_below: AtomicU64,
    /// Rows the compaction filter dropped since open.
    filter_rows_dropped: AtomicU64,
}

impl PruneCutoff {
    pub fn new(mode: PruneMode) -> Self {
        Self {
            mode,
            hidden_below: AtomicU64::new(0),
            filter_below: AtomicU64::new(0),
            filter_rows_dropped: AtomicU64::new(0),
        }
    }

    /// The mode the DB was opened with (decides whether a filter is installed).
    pub fn mode(&self) -> PruneMode {
        self.mode
    }

    /// Rows below this height are hidden from every `StateDb` read.
    pub fn hidden_below(&self) -> u64 {
        self.hidden_below.load(Ordering::Acquire)
    }

    /// Rows below this height are dropped by compactions (0 = none).
    pub fn filter_below(&self) -> u64 {
        self.filter_below.load(Ordering::Acquire)
    }

    /// Rows the compaction filter dropped since open.
    pub fn filter_rows_dropped(&self) -> u64 {
        self.filter_rows_dropped.load(Ordering::Relaxed)
    }

    /// Raise the read-side cutoff (never lowered).
    pub(crate) fn hide_below(&self, height: u64) {
        self.hidden_below.fetch_max(height, Ordering::AcqRel);
    }

    /// Raise the filter's cutoff (never lowered, never above the read side).
    pub(crate) fn arm_filter(&self, height: u64) {
        self.hide_below(height);
        self.filter_below.fetch_max(height, Ordering::AcqRel);
    }

    /// Is `key` of `cf` a pruned row that reads must not return?
    #[inline]
    pub fn is_hidden(&self, cf: &str, key: &[u8]) -> bool {
        let cutoff = self.hidden_below.load(Ordering::Acquire);
        if cutoff == 0 {
            return false;
        }
        match height_keyed_key_len(cf) {
            Some(len) => below_cutoff(key, len, cutoff),
            None => false,
        }
    }

    /// The cutoff reads of `cf` hide below, with its key length (`None`:
    /// nothing hidden in `cf`).
    #[inline]
    pub(crate) fn hidden_for(&self, cf: &str) -> Option<(u64, usize)> {
        let cutoff = self.hidden_below.load(Ordering::Acquire);
        if cutoff == 0 {
            return None;
        }
        height_keyed_key_len(cf).map(|len| (cutoff, len))
    }
}

/// Compaction filter factory of one [`HEIGHT_KEYED_CFS`] CF. RocksDB asks it
/// for a fresh filter per (sub)compaction, so each filter is used by one
/// thread; the filter reads the cutoff once, when it is created (a cutoff
/// raised mid-compaction applies from the next compaction: conservative).
pub struct PruneFilterFactory {
    cutoff: Arc<PruneCutoff>,
    key_len: usize,
}

impl PruneFilterFactory {
    pub fn new(cutoff: Arc<PruneCutoff>, key_len: usize) -> Self {
        Self { cutoff, key_len }
    }
}

const FILTER_NAME: &CStr = c"torus-prune-below-cutoff";

impl CompactionFilterFactory for PruneFilterFactory {
    type Filter = PruneFilter;

    fn create(&mut self, _context: CompactionFilterContext) -> PruneFilter {
        PruneFilter {
            cutoff: self.cutoff.filter_below(),
            key_len: self.key_len,
            dropped: 0,
            stats: self.cutoff.clone(),
        }
    }

    fn name(&self) -> &CStr {
        FILTER_NAME
    }
}

/// One compaction's filter: drops the rows below the cutoff it was created
/// with.
pub struct PruneFilter {
    cutoff: u64,
    key_len: usize,
    dropped: u64,
    stats: Arc<PruneCutoff>,
}

impl CompactionFilter for PruneFilter {
    fn filter(&mut self, _level: u32, key: &[u8], _value: &[u8]) -> Decision {
        if below_cutoff(key, self.key_len, self.cutoff) {
            self.dropped += 1;
            Decision::Remove
        } else {
            Decision::Keep
        }
    }

    fn name(&self) -> &CStr {
        FILTER_NAME
    }
}

impl Drop for PruneFilter {
    fn drop(&mut self) {
        if self.dropped > 0 {
            self.stats
                .filter_rows_dropped
                .fetch_add(self.dropped, Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prune_mode_parse() {
        assert_eq!(parse_prune_mode(None), PruneMode::Filter, "default filter");
        assert_eq!(parse_prune_mode(Some("filter")), PruneMode::Filter);
        assert_eq!(parse_prune_mode(Some(" range ")), PruneMode::Range);
        assert_eq!(parse_prune_mode(Some("RANGE")), PruneMode::Filter);
        assert_eq!(parse_prune_mode(Some("")), PruneMode::Filter);
    }

    /// The predicate is exactly the range mode's `[bound(0), bound(cutoff))`
    /// per key length, edges included.
    #[test]
    fn below_cutoff_matches_the_range_delete_bounds() {
        let k = |h: u64, tail: &[u8]| [&h.to_be_bytes()[..], tail].concat();
        for (len, tail_max) in [(8usize, vec![]), (12, vec![0xFF; 4]), (16, vec![0xFF; 8])] {
            let zeros = vec![0u8; len - 8];
            assert!(
                !below_cutoff(&k(0, &zeros), len, 0),
                "cutoff 0 prunes nothing"
            );
            assert!(below_cutoff(&k(0, &zeros), len, 1));
            assert!(below_cutoff(&k(16, &tail_max), len, 17));
            assert!(!below_cutoff(&k(17, &zeros), len, 17));
            assert!(!below_cutoff(&k(u64::MAX, &tail_max), len, u64::MAX));
            assert!(below_cutoff(&k(u64::MAX - 1, &tail_max), len, u64::MAX));
            // Keys below bound(0) (shorter than the format) were never in a
            // range delete either.
            assert!(!below_cutoff(&[0u8; 3], len, 17));
        }
        // The DeleteFilesInRange end is below bound(cutoff), and the last
        // row below the cutoff is <= it.
        for len in [8usize, 12, 16] {
            let end = files_range_end(17, len).unwrap();
            assert!(below_cutoff(&end, len, 17));
            let last = k(16, &vec![0xFF; len - 8]);
            assert!(last <= end);
            assert!(files_range_end(0, len).is_none());
        }
    }

    #[test]
    fn cutoff_hides_only_height_keyed_cfs_and_is_never_lowered() {
        let c = PruneCutoff::new(PruneMode::Filter);
        assert!(!c.is_hidden(CF_BLOCK_BODIES, &3u64.to_be_bytes()));
        c.hide_below(10);
        assert!(c.is_hidden(CF_BLOCK_BODIES, &3u64.to_be_bytes()));
        assert!(!c.is_hidden(CF_BLOCK_BODIES, &10u64.to_be_bytes()));
        assert!(!c.is_hidden(crate::cf::CF_BLOCK_HEADERS, &3u64.to_be_bytes()));
        assert_eq!(c.filter_below(), 0, "hiding does not arm the filter");
        c.hide_below(5);
        assert_eq!(c.hidden_below(), 10, "never lowered");
        c.arm_filter(12);
        assert_eq!((c.hidden_below(), c.filter_below()), (12, 12));
    }
}
