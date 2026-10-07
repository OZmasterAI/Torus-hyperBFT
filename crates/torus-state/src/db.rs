//! RocksDB wrapper with column family layout and revm `DatabaseRef` implementation.

use std::path::Path;
use std::sync::Arc;

use alloy_primitives::{Address, Bytes, B256, U256};
use revm::bytecode::Bytecode;
use revm::state::AccountInfo;
use rocksdb::{
    BlockBasedOptions, Cache, ColumnFamily, ColumnFamilyDescriptor, DBCompressionType, Options,
    WriteBatch, DB,
};

use crate::cf::*;
use crate::error::StateError;

/// Keccak256 of empty bytes — the code hash for accounts with no code.
pub const KECCAK_EMPTY: B256 = B256::new([
    0xc5, 0xd2, 0x46, 0x01, 0x86, 0xf7, 0x23, 0x3c, 0x92, 0x7e, 0x7d, 0xb2, 0xdc, 0xc7, 0x03, 0xc0,
    0xe5, 0x00, 0xb6, 0x53, 0xca, 0x82, 0x27, 0x3b, 0x7b, 0xfa, 0xd8, 0x04, 0x5d, 0x85, 0xa4, 0x70,
]);

/// The smallest key greater than every key that starts with `prefix`: strip
/// trailing 0xff bytes, then increment the last byte. `None` for an empty or
/// all-0xff prefix (no such key; the scan runs to the end of the CF).
pub fn prefix_successor(prefix: &[u8]) -> Option<Vec<u8>> {
    let mut succ = prefix.to_vec();
    while let Some(last) = succ.pop() {
        if last != 0xff {
            succ.push(last + 1);
            return Some(succ);
        }
    }
    None
}

/// Read options for a forward scan of the keys under `prefix`, bounded above
/// by [`prefix_successor`] (s89 fix A). No prefix extractor is configured, so
/// without the bound RocksDB walks every tombstone after the prefix until it
/// finds the next live key, which the caller then rejects. Callers still stop
/// at the first key not starting with `prefix` (needed when there is no bound),
/// so results are byte-identical; only the work past the prefix goes.
pub fn prefix_read_opts(prefix: &[u8]) -> rocksdb::ReadOptions {
    let mut ro = rocksdb::ReadOptions::default();
    if let Some(upper) = prefix_successor(prefix) {
        ro.set_iterate_upper_bound(upper);
    }
    ro
}

/// Forward iterator over `cf` from `prefix`, bounded by [`prefix_read_opts`].
/// Replaces `DB::prefix_iterator_cf`, which sets no upper bound.
pub fn prefix_iter<'a>(
    db: &'a DB,
    cf: &impl rocksdb::AsColumnFamilyRef,
    prefix: &[u8],
) -> rocksdb::DBIteratorWithThreadMode<'a, DB> {
    db.iterator_cf_opt(
        cf,
        prefix_read_opts(prefix),
        rocksdb::IteratorMode::From(prefix, rocksdb::Direction::Forward),
    )
}

/// One key range a background compaction covers: `[start, end)` of a CF
/// (`end` `None` = to the CF's end).
pub(crate) type KeyRange = (&'static str, Vec<u8>, Option<Vec<u8>>);

/// Requested ranges per `(CF, group)`: `(start, end)`.
type PendingRanges =
    std::collections::BTreeMap<(&'static str, Vec<u8>), (Vec<u8>, Option<Vec<u8>>)>;
/// Uncompacted deletes per `(CF, market prefix)`: `(count, first key, last key)`.
type DeleteCounts = std::collections::HashMap<(&'static str, Vec<u8>), (u64, Vec<u8>, Vec<u8>)>;

/// What the [`RangeCompaction`] job holds under its lock.
#[derive(Default)]
struct JobState {
    /// The worker is compacting.
    running: bool,
    /// Ranges requested and not yet started, one per `(CF, group)` (the oracle
    /// submission prefix, or one market's book rows); a request for a group
    /// widens its range.
    pending: PendingRanges,
    /// s99 (c): deletes not yet compacted per `(CF, market prefix)` of the
    /// rows the reader precompiles scan: `(count, first key, last key)`,
    /// summed across flushes until the count reaches
    /// [`crate::cf::SCANNED_DELETES_COMPACTION_THRESHOLD`]. In memory only
    /// (node-local; a restart forgets it, RocksDB's own compactions remain).
    uncompacted: DeleteCounts,
    /// The worker thread exists (spawned on the first request, lives until
    /// the last owner drops).
    worker_started: bool,
}

/// s89 fix B (generalized for s99 (c)): the background compaction of key
/// ranges whose rows flushes deleted — the pruned oracle submissions
/// (`[ORACLE_SUBMISSION_PREFIX, successor)` of `CF_NATIVE_ORACLE`) and, per
/// market, the book rows the reader precompiles scan (see
/// [`StateDb::compact_range_in_background`], [`StateDb::note_scanned_deletes`]).
/// One long-lived worker thread, parked on `work` between runs.
#[derive(Default)]
struct RangeCompaction {
    state: std::sync::Mutex<JobState>,
    /// Signalled on a new request and on shutdown.
    work: std::sync::Condvar,
    /// Signalled when the worker goes idle (nothing running or pending).
    idle: std::sync::Condvar,
    /// Finished runs: `(done, failed)`.
    runs: std::sync::Mutex<(u64, u64)>,
    /// Set by [`CompactionOwner`]'s drop: no new run, no further range.
    shutdown: std::sync::atomic::AtomicBool,
    /// The worker thread, joined by [`CompactionOwner`]'s drop.
    worker: std::sync::Mutex<Option<std::thread::JoinHandle<()>>>,
    /// The DB the worker compacts (set with the first request); weak, so a
    /// parked worker never keeps a closed DB (and its LOCK) alive.
    db: std::sync::Mutex<std::sync::Weak<DB>>,
    /// Test hook: every run fails (logged, counted) instead of compacting.
    #[cfg(test)]
    fail: std::sync::atomic::AtomicBool,
    /// Test hook: runs that took their strong `Arc<DB>`.
    #[cfg(test)]
    started: std::sync::atomic::AtomicU64,
    /// Test hook: each run keeps its strong `Arc<DB>` this many ms before compacting.
    #[cfg(test)]
    hold_ms: std::sync::atomic::AtomicU64,
    /// Test hook: every range a run compacted, in order.
    #[cfg(test)]
    ran: std::sync::Mutex<Vec<KeyRange>>,
}

impl RangeCompaction {
    fn run(&self, db: &DB, ranges: &[KeyRange]) -> Result<(), String> {
        use std::sync::atomic::Ordering::Relaxed;
        #[cfg(test)]
        if self.fail.load(Relaxed) {
            return Err("injected failure (test)".into());
        }
        #[cfg(test)]
        {
            self.started.fetch_add(1, Relaxed);
            std::thread::sleep(std::time::Duration::from_millis(self.hold_ms.load(Relaxed)));
        }
        let mut opts = rocksdb::CompactOptions::default();
        // Do not hold back the automatic compactions while this one runs.
        opts.set_exclusive_manual_compaction(false);
        // Rewrite the bottommost files of the range too: a file that reached
        // the last level by a trivial move (no overlap, e.g. rows and their
        // deletes flushed together) still carries its tombstones otherwise.
        // ForceOptimized skips only files this same compaction just wrote; a
        // trivially moved file keeps its older file number and is rewritten.
        opts.set_bottommost_level_compaction(rocksdb::BottommostLevelCompaction::ForceOptimized);
        let mut result = Ok(());
        for (name, start, end) in ranges {
            // The last owner is dropping: leave the rest.
            if self.shutdown.load(Relaxed) {
                break;
            }
            let Some(cf) = db.cf_handle(name) else {
                result = Err(format!("missing column family {name}"));
                continue;
            };
            #[cfg(test)]
            self.ran
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push((name, start.clone(), end.clone()));
            let done = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                db.compact_range_cf_opt(cf, Some(start.as_slice()), end.as_deref(), &opts)
            }));
            if done.is_err() {
                result = Err(format!("compact_range_cf of {name} panicked"));
            }
        }
        result
    }

    fn lock_state(&self) -> std::sync::MutexGuard<'_, JobState> {
        self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn lock_runs(&self) -> std::sync::MutexGuard<'_, (u64, u64)> {
        self.runs.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The worker: wait for requests, compact them, until shutdown.
    fn work_loop(&self) {
        loop {
            let ranges: Vec<KeyRange> = {
                let mut state = self.lock_state();
                loop {
                    if self.shutdown.load(std::sync::atomic::Ordering::Relaxed) {
                        state.running = false;
                        state.pending.clear();
                        self.idle.notify_all();
                        return;
                    }
                    if !state.pending.is_empty() {
                        break;
                    }
                    if state.running {
                        state.running = false;
                        self.idle.notify_all();
                    }
                    state = self
                        .work
                        .wait(state)
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                }
                state.running = true;
                std::mem::take(&mut state.pending)
                    .into_iter()
                    .map(|((cf, _), (start, end))| (cf, start, end))
                    .collect()
            };
            let started = std::time::Instant::now();
            // Strong only for this run: the DB closed since the request leaves
            // nothing to compact.
            let db = self
                .db
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .upgrade();
            let result = db.as_deref().map_or(Ok(()), |db| self.run(db, &ranges));
            drop(db);
            match result {
                Ok(()) => {
                    self.lock_runs().0 += 1;
                    tracing::debug!(
                        ms = started.elapsed().as_secs_f64() * 1e3,
                        ranges = ranges.len(),
                        "compacted deleted key ranges"
                    );
                }
                Err(e) => {
                    self.lock_runs().1 += 1;
                    tracing::warn!(error = %e, "deleted key range compaction failed (ignored)");
                }
            }
        }
    }
}

/// The owners' side of the [`RangeCompaction`] job, shared by every clone
/// of one `StateDb` (the worker holds only the job). Its drop runs once, with
/// the last clone and before that clone's `Arc<DB>`: it stops the worker
/// (no further range; a running RocksDB compaction is cancelled with
/// `cancel_all_background_work`, so shutdown does not wait for it) and joins
/// it. The worker upgrades its `Weak<DB>` for a whole run, so without the join
/// it could hold the last `Arc<DB>` and close RocksDB on its own thread during
/// process exit, after RocksDB's static mutexes are destroyed (teardown
/// SIGABRT "pthread lock: Invalid argument").
#[derive(Default)]
struct CompactionOwner(Arc<RangeCompaction>);

impl Drop for CompactionOwner {
    fn drop(&mut self) {
        let job = &self.0;
        job.shutdown
            .store(true, std::sync::atomic::Ordering::Relaxed);
        {
            let state = job.lock_state();
            if state.running {
                // Only at the last owner's drop (node shutdown): it also stops
                // RocksDB's automatic background work for this DB instance.
                let db = job
                    .db
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .upgrade();
                if let Some(db) = db {
                    db.cancel_all_background_work(false);
                }
            }
            job.work.notify_all();
        }
        let worker = job
            .worker
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(worker) = worker {
            let _ = worker.join();
        }
    }
}

/// Central RocksDB database handle for the Torus node.
///
/// Opens all column families defined in section 6.1. Provides typed accessors
/// for EVM state (accounts, storage, code) and implements `revm::DatabaseRef`.
#[derive(Clone)]
pub struct StateDb {
    /// s89 fix B: the background compaction of deleted key ranges (pruned
    /// oracle submissions, scanned order rows), shared by every clone of this
    /// handle (one in flight per DB). Declared before `db`: fields drop in
    /// order, so the last clone joins the worker while it still holds its own
    /// `Arc<DB>`.
    range_compaction: Arc<CompactionOwner>,
    db: Arc<DB>,
    /// The DB-wide `Options` the instance was opened with, kept alive so the
    /// RocksDB `Statistics` object it owns (tickers + histograms) can be read
    /// at runtime (`runtime_stats`). `None` for wrapped/read-only handles.
    opts: Option<Arc<Options>>,
    /// Statistics level the instance was opened with (0 = none, 1 = tickers,
    /// 2 = tickers + histograms). Decides which parts of `runtime_stats` are
    /// meaningful.
    stats_level: u8,
}

/// r3 exec-write-stall-attribution: node-local RocksDB tuning read once at DB
/// open. Every default is EXACT-TODAY except `stats_level` (tickers on so the
/// stall / write-group counters are scrapeable). None of these affect the
/// on-disk format or consensus state — they change only when a write waits.
///
/// Env:
/// - `TORUS_ROCKSDB_STATS` — `0` off, `1` tickers only (default), `2` RocksDB's
///   own default level (`ExceptDetailedTimers`: adds the db.write / write.stall
///   / flush / compaction histograms at the cost of two clock reads per op).
/// - `TORUS_ROCKSDB_L0_SLOWDOWN` / `TORUS_ROCKSDB_L0_STOP` — per-CF
///   `level0_slowdown_writes_trigger` / `level0_stop_writes_trigger` (RocksDB
///   defaults 20 / 36 when unset). A DB-wide slowdown throttles EVERY writer,
///   the exec thread included, so raising them trades read amplification for
///   write latency.
/// - `TORUS_ROCKSDB_MAX_WRITE_BUFFERS` — per-CF `max_write_buffer_number`
///   (default 4 = exact-today; RocksDB slows writes at `n-1` unflushed
///   memtables and stops at `n`).
/// - `TORUS_ROCKSDB_PIPELINED_WRITE` — `enable_pipelined_write` (WAL and
///   memtable stages of consecutive write groups overlap; default off).
/// - `TORUS_ROCKSDB_MAX_TOTAL_WAL_MB` — whole-MiB WAL flush trigger (default
///   512; invalid values fall back to it). `0` restores RocksDB's automatic
///   threshold (4x aggregate CF buffer capacity, ~86 GiB here), under which a
///   cold CF pins every WAL segment since open: s74 crash A/B, restart DB open
///   20.5 s -> 7.9 s. A soft flush trigger, not a hard disk limit; WAL
///   durability is unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DbTuning {
    pub stats_level: u8,
    pub l0_slowdown_trigger: Option<i32>,
    pub l0_stop_trigger: Option<i32>,
    pub max_write_buffer_number: i32,
    pub pipelined_write: bool,
    pub max_total_wal_size: Option<u64>,
}

impl Default for DbTuning {
    fn default() -> Self {
        Self::from_raw(None, None, None, None, None)
    }
}

impl DbTuning {
    /// Read every knob from the environment (once, at DB open).
    pub fn from_env() -> Self {
        let v = |k: &str| std::env::var(k).ok();
        let mut tuning = Self::from_raw(
            v("TORUS_ROCKSDB_STATS"),
            v("TORUS_ROCKSDB_L0_SLOWDOWN"),
            v("TORUS_ROCKSDB_L0_STOP"),
            v("TORUS_ROCKSDB_MAX_WRITE_BUFFERS"),
            v("TORUS_ROCKSDB_PIPELINED_WRITE"),
        );
        let raw = v("TORUS_ROCKSDB_MAX_TOTAL_WAL_MB");
        tuning.max_total_wal_size = parse_max_total_wal_mib(raw.as_deref());
        if let Some(value) = raw.as_deref() {
            if valid_wal_mib(value).is_none() {
                tracing::warn!(value, "invalid TORUS_ROCKSDB_MAX_TOTAL_WAL_MB; using the {DEFAULT_MAX_TOTAL_WAL_MIB} MiB default");
            }
        }
        tuning
    }

    /// Pure parse of the raw env strings (unit-testable without touching
    /// process-global state).
    pub fn from_raw(
        stats: Option<String>,
        l0_slowdown: Option<String>,
        l0_stop: Option<String>,
        max_write_buffers: Option<String>,
        pipelined: Option<String>,
    ) -> Self {
        let pos_i32 = |raw: Option<String>| -> Option<i32> {
            raw.as_deref()
                .map(str::trim)
                .and_then(|s| s.parse::<i32>().ok())
                .filter(|&n| n >= 1)
        };
        Self {
            stats_level: parse_rocksdb_stats_level(stats),
            l0_slowdown_trigger: pos_i32(l0_slowdown),
            l0_stop_trigger: pos_i32(l0_stop),
            // Below 2 the DB could not switch memtables at all; clamp.
            max_write_buffer_number: pos_i32(max_write_buffers).unwrap_or(4).max(2),
            pipelined_write: matches!(
                pipelined.as_deref().map(str::trim),
                Some("1" | "true" | "TRUE" | "yes" | "on")
            ),
            max_total_wal_size: parse_max_total_wal_mib(None),
        }
    }
}

pub const DEFAULT_MAX_TOTAL_WAL_MIB: u64 = 512;

/// `max_total_wal_size` in bytes: unset or invalid => the 512 MiB default,
/// `0` => None (RocksDB's automatic threshold), N => N MiB (checked).
pub fn parse_max_total_wal_mib(raw: Option<&str>) -> Option<u64> {
    let mib = raw.and_then(valid_wal_mib).unwrap_or(DEFAULT_MAX_TOTAL_WAL_MIB);
    Some(mib << 20).filter(|&bytes| bytes > 0)
}

/// ASCII digits whose MiB value fits in u64 bytes; anything else is invalid.
fn valid_wal_mib(raw: &str) -> Option<u64> {
    let raw = raw.trim();
    if raw.is_empty() || !raw.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    raw.parse::<u64>().ok().filter(|&mib| mib.checked_mul(1 << 20).is_some())
}

/// Pure parse of `TORUS_ROCKSDB_STATS`: unset / garbage => 1 (tickers only),
/// `0` off, `2` (or more) => 2 (histograms too; never the mutex-timing levels).
pub fn parse_rocksdb_stats_level(raw: Option<String>) -> u8 {
    match raw.as_deref().map(str::trim).and_then(|s| s.parse::<u8>().ok()) {
        Some(0) => 0,
        Some(1) => 1,
        Some(_) => 2,
        None => 1,
    }
}

/// Cumulative RocksDB statistics tickers (present when the DB was opened with
/// `stats_level >= 1`). All counters are since-open.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RocksdbTickers {
    /// `rocksdb.stall.micros`: total time writers spent blocked/delayed by the
    /// write controller (L0 / memtable / pending-compaction triggers).
    pub stall_micros: u64,
    /// `rocksdb.write.self`: writes that led their write group.
    pub write_self: u64,
    /// `rocksdb.write.other`: writes that rode as followers behind another
    /// thread's write group (the head-of-line-blocking signature).
    pub write_other: u64,
    /// `rocksdb.bytes.written` (through Put/Write).
    pub bytes_written: u64,
    /// `rocksdb.wal.bytes`.
    pub wal_bytes: u64,
    /// `rocksdb.flush.write.bytes`.
    pub flush_write_bytes: u64,
    /// `rocksdb.compact.read.bytes` / `rocksdb.compact.write.bytes`.
    pub compact_read_bytes: u64,
    pub compact_write_bytes: u64,
    /// `rocksdb.compaction.total.time.cpu_micros`.
    pub compaction_cpu_micros: u64,
}

/// One RocksDB histogram, reduced to the fields the sampler exports.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct RocksdbHist {
    pub count: u64,
    pub sum: u64,
    pub p99: f64,
    pub max: f64,
}

/// RocksDB latency histograms (present when `stats_level >= 2`).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct RocksdbHistograms {
    /// `rocksdb.db.write.micros`: end-to-end DB write latency incl. group wait.
    pub db_write: RocksdbHist,
    /// `rocksdb.db.write.stall`: per-write stall time.
    pub write_stall: RocksdbHist,
    /// `rocksdb.db.flush.micros`.
    pub flush: RocksdbHist,
    /// `rocksdb.compaction.times.micros`.
    pub compaction: RocksdbHist,
}

/// DB-wide runtime snapshot: write-controller state + LSM shape aggregated
/// over every column family (the per-CF labelled families stay for the deep
/// dive; these unlabelled totals are what a `$1==name` scraper can read).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RocksdbRuntimeStats {
    /// Sum of `rocksdb.cur-size-all-mem-tables` over all CFs.
    pub memtable_bytes_all: u64,
    /// Sum of `rocksdb.num-immutable-mem-table` over all CFs (flush backlog).
    pub immutable_memtables_all: u64,
    /// Max `rocksdb.num-files-at-level0` over all CFs (the slowdown trigger
    /// fires per CF, so the max is the one that matters).
    pub l0_files_max: u64,
    /// Sum of `rocksdb.estimate-pending-compaction-bytes` over all CFs.
    pub pending_compaction_bytes_all: u64,
    /// `rocksdb.actual-delayed-write-rate` (0 = no write delay in force).
    pub delayed_write_rate: u64,
    /// `rocksdb.is-write-stopped` (1 while writes are fully stopped).
    pub write_stopped: u64,
    /// `rocksdb.num-running-compactions` / `rocksdb.num-running-flushes`.
    pub running_compactions: u64,
    pub running_flushes: u64,
    /// `rocksdb.block-cache-usage`.
    pub block_cache_bytes: u64,
    pub tickers: Option<RocksdbTickers>,
    pub histograms: Option<RocksdbHistograms>,
}

impl StateDb {
    /// Open (or create) the database at the given path with all column families,
    /// with the tuning knobs read from the environment ([`DbTuning::from_env`]).
    pub fn open(path: &Path) -> Result<Self, StateError> {
        Self::open_with_tuning(path, &DbTuning::from_env())
    }

    /// Open (or create) the database with explicit tuning (tests / tooling).
    pub fn open_with_tuning(path: &Path, tuning: &DbTuning) -> Result<Self, StateError> {
        let mut opts = Options::default();
        opts.create_if_missing(true);
        opts.create_missing_column_families(true);
        // r3 exec-write-stall-attribution: RocksDB's own statistics. Level 1
        // (default) = tickers only — stall micros, write self/other (write-group
        // followers), WAL/flush/compaction bytes, compaction CPU. Level 2 adds
        // the db.write / write.stall / flush / compaction histograms.
        if tuning.stats_level >= 1 {
            opts.enable_statistics();
            opts.set_statistics_level(if tuning.stats_level >= 2 {
                rocksdb::statistics::StatsLevel::ExceptDetailedTimers
            } else {
                rocksdb::statistics::StatsLevel::ExceptHistogramOrTimers
            });
        }
        if tuning.pipelined_write {
            opts.set_enable_pipelined_write(true);
        }
        if let Some(bytes) = tuning.max_total_wal_size.filter(|&bytes| bytes > 0) {
            opts.set_max_total_wal_size(bytes);
        }
        // DB-wide: parallelize flush/compaction and smooth fsync spikes during
        // heavy block writes. (Stock defaults run only 2 background jobs.)
        //
        // L3 #3 (compaction smoothing): `max_background_jobs` and
        // `max_subcompactions` are env-gated so an 18-core box can dedicate more
        // threads to flush/compaction (the measured contention vs the CPU-bound
        // leader that inflates flush variance to 39→119 ms/blk). Both default to
        // exact-today behaviour (4 jobs; subcompactions unset). Node-local,
        // perf-only, no format/consensus impact.
        opts.set_max_background_jobs(max_background_jobs());
        if let Some(sub) = max_subcompactions() {
            opts.set_max_subcompactions(sub);
        }
        // L3 #4 (bytes_per_sync A/B): range-sync granularity during heavy flush.
        // 1 MiB range-sync can add write-path latency spikes; the bench A/Bs 4
        // MiB or 0 (disabled). Default 1 MiB = exact-today. Node-local, perf-only.
        opts.set_bytes_per_sync(bytes_per_sync_bytes());
        // STABILITY: the ONLY global bound on total memtable memory.
        //
        // `set_write_buffer_size` / `set_max_write_buffer_number` below are
        // PER-COLUMN-FAMILY, and they are applied to every name in
        // `ALL_CF_NAMES` (44 CFs), so the untuned sum is
        // 43 x 128 MiB x 4 + 8 MiB x 4 = ~21.5 GiB with NO global bound — against
        // a documented 4 GB / 8 GB-for-validators node floor
        // (docs/node-operator-guide.md). RocksDB only approaches that sum under a
        // broad multi-CF write burst (memtable arenas are allocated lazily, and
        // the 4 buffer slots per CF only fill when flush falls behind), but
        // nothing clips the tail, so a burst can OOM the node.
        //
        // `db_write_buffer_size` is RocksDB's DB-wide write-buffer budget: once
        // the sum of all live memtables crosses it, RocksDB force-flushes the CF
        // with the largest memtable instead of letting the sum keep growing.
        // Bounds the tail without reshaping steady state (see
        // `db_write_buffer_bytes` for the default and per-node-class guidance).
        opts.set_db_write_buffer_size(db_write_buffer_bytes());

        // Per-CF tuning, shared across every column family. RocksDB ships an
        // ~8 MiB block cache and NO bloom filters by default, which is poor for
        // this node's point-lookup-heavy access (account / code / native-action
        // -by-hash). A shared cache + bloom filters is the main read win here.
        let cache = Cache::new_lru_cache(256 * 1024 * 1024); // 256 MiB shared block cache
        let mut bbt = BlockBasedOptions::default();
        bbt.set_block_cache(&cache);
        bbt.set_bloom_filter(10.0, false); // ~1% false positives on point lookups
        bbt.set_block_size(16 * 1024); // 16 KiB blocks
        bbt.set_cache_index_and_filter_blocks(true);
        bbt.set_pin_l0_filter_and_index_blocks_in_cache(true);

        let mut cf_opts = Options::default();
        cf_opts.set_block_based_table_factory(&bbt);
        cf_opts.set_write_buffer_size(128 * 1024 * 1024); // 128 MiB memtable
        cf_opts.set_max_write_buffer_number(tuning.max_write_buffer_number);
        // r3: write-controller triggers (unset = RocksDB defaults 20 / 36 =
        // exact-today). A slowdown on ANY CF throttles every writer of the
        // instance, the exec thread's small header/body puts included.
        if let Some(n) = tuning.l0_slowdown_trigger {
            cf_opts.set_level_zero_slowdown_writes_trigger(n);
        }
        if let Some(n) = tuning.l0_stop_trigger {
            cf_opts.set_level_zero_stop_writes_trigger(n);
        }
        cf_opts.set_compression_type(DBCompressionType::Lz4);
        cf_opts.set_bottommost_compression_type(DBCompressionType::Zstd);
        cf_opts.set_level_compaction_dynamic_level_bytes(true);

        // cf_consensus_meta holds a small hot keyset rewritten EVERY block
        // (leader reputation, speculative commits, highest PC/TC, block tree).
        // Under the shared 128 MiB buffer its memtable accumulates dead
        // versions for hours without flushing, and the consensus thread's
        // propose-path reads slow down walking the growing skiplist —
        // measured +0.9 ms per 1k blocks (S405 soak), the live height-drag
        // mechanism. A small buffer flushes it early; compaction then drops
        // the dead versions and reads stay flat.
        let mut meta_opts = cf_opts.clone();
        meta_opts.set_write_buffer_size(8 * 1024 * 1024); // 8 MiB

        // L3 #3 (compaction smoothing): the churny native CFs (order books +
        // the hash-only mirror + the node-local order-row store) accumulate dead
        // versions / tombstones under the shared 128 MiB buffer, so a scan or a
        // flush pays the growing skiplist + tombstone walk. A smaller buffer
        // flushes them small/often (the exact treatment already applied to
        // CF_CONSENSUS_META), so compaction drops the dead versions promptly.
        // Env-gated; 0 (default) = shared 128 MiB = exact-today.
        let churny_opts = churny_cf_write_buffer_bytes().map(|bytes| {
            let mut o = cf_opts.clone();
            o.set_write_buffer_size(bytes);
            o
        });
        let is_churny_cf = |name: &str| {
            name == CF_NATIVE_ORDER_BOOKS || name == CF_NATIVE_HASHED || name == CF_BOOK_ORDER_ROWS
        };

        let cf_descriptors: Vec<ColumnFamilyDescriptor> = ALL_CF_NAMES
            .iter()
            .map(|name| {
                let opts = if *name == CF_CONSENSUS_META {
                    meta_opts.clone()
                } else if let (true, Some(o)) = (is_churny_cf(name), churny_opts.as_ref()) {
                    o.clone()
                } else {
                    cf_opts.clone()
                };
                ColumnFamilyDescriptor::new(*name, opts)
            })
            .collect();

        let db = DB::open_cf_descriptors(&opts, path, cf_descriptors)?;
        Ok(Self {
            db: Arc::new(db),
            opts: (tuning.stats_level >= 1).then(|| Arc::new(opts)),
            stats_level: tuning.stats_level,
            range_compaction: Arc::default(),
        })
    }

    /// Wrap an already-opened RocksDB instance (e.g., a read-only snapshot DB).
    pub fn from_existing_db(db: DB) -> Self {
        Self {
            db: Arc::new(db),
            opts: None,
            stats_level: 0,
            range_compaction: Arc::default(),
        }
    }

    /// r3 exec-write-stall-attribution: one DB-wide runtime snapshot — the
    /// write-controller state and LSM shape aggregated over every CF, plus the
    /// RocksDB statistics tickers (`stats_level >= 1`) and latency histograms
    /// (`stats_level >= 2`). Cheap enough to sample every few seconds (a few
    /// hundred property lookups; each takes the DB mutex briefly).
    pub fn runtime_stats(&self) -> RocksdbRuntimeStats {
        let db = &self.db;
        let mut s = RocksdbRuntimeStats::default();
        for cf_name in ALL_CF_NAMES {
            let Some(cf) = db.cf_handle(cf_name) else {
                continue;
            };
            let prop = |name: &str| -> u64 {
                db.property_int_value_cf(cf, name).ok().flatten().unwrap_or(0)
            };
            s.memtable_bytes_all += prop("rocksdb.cur-size-all-mem-tables");
            s.immutable_memtables_all += prop("rocksdb.num-immutable-mem-table");
            s.l0_files_max = s.l0_files_max.max(prop("rocksdb.num-files-at-level0"));
            s.pending_compaction_bytes_all += prop("rocksdb.estimate-pending-compaction-bytes");
        }
        let dbprop = |name: &str| -> u64 { db.property_int_value(name).ok().flatten().unwrap_or(0) };
        s.delayed_write_rate = dbprop("rocksdb.actual-delayed-write-rate");
        s.write_stopped = dbprop("rocksdb.is-write-stopped");
        s.running_compactions = dbprop("rocksdb.num-running-compactions");
        s.running_flushes = dbprop("rocksdb.num-running-flushes");
        s.block_cache_bytes = dbprop("rocksdb.block-cache-usage");

        if let Some(opts) = &self.opts {
            use rocksdb::statistics::{Histogram, Ticker};
            let t = |ticker: Ticker| opts.get_ticker_count(ticker);
            s.tickers = Some(RocksdbTickers {
                stall_micros: t(Ticker::StallMicros),
                write_self: t(Ticker::WriteDoneBySelf),
                write_other: t(Ticker::WriteDoneByOther),
                bytes_written: t(Ticker::BytesWritten),
                wal_bytes: t(Ticker::WalFileBytes),
                flush_write_bytes: t(Ticker::FlushWriteBytes),
                compact_read_bytes: t(Ticker::CompactReadBytes),
                compact_write_bytes: t(Ticker::CompactWriteBytes),
                compaction_cpu_micros: t(Ticker::CompactionCpuTotalTime),
            });
            if self.stats_level >= 2 {
                let h = |hist: Histogram| {
                    let d = opts.get_histogram_data(hist);
                    RocksdbHist {
                        count: d.count(),
                        sum: d.sum(),
                        p99: d.p99(),
                        max: d.max(),
                    }
                };
                s.histograms = Some(RocksdbHistograms {
                    db_write: h(Histogram::DbWrite),
                    write_stall: h(Histogram::WriteStall),
                    flush: h(Histogram::FlushTime),
                    compaction: h(Histogram::CompactionTime),
                });
            }
        }
        s
    }

    /// Open the database read-only (all column families). Works against a LIVE
    /// primary too (no LOCK contention), but then sees data only as of the last
    /// flush — recent memtable-only writes are invisible. Used by offline
    /// tooling (`torus-unwedge --inspect`) for recon without stopping the node.
    pub fn open_read_only(path: &Path) -> Result<Self, StateError> {
        let opts = Options::default();
        let cf_descriptors: Vec<ColumnFamilyDescriptor> = ALL_CF_NAMES
            .iter()
            .map(|name| ColumnFamilyDescriptor::new(*name, Options::default()))
            .collect();
        let db = DB::open_cf_descriptors_read_only(&opts, path, cf_descriptors, false)?;
        Ok(Self {
            db: Arc::new(db),
            opts: None,
            stats_level: 0,
            range_compaction: Arc::default(),
        })
    }

    /// Destroy the database at the given path (for testing).
    pub fn destroy(path: &Path) -> Result<(), StateError> {
        DB::destroy(&Options::default(), path)?;
        Ok(())
    }

    /// Get a reference to the underlying RocksDB instance.
    pub fn inner(&self) -> &DB {
        &self.db
    }

    /// s89 fix B: compact `[ORACLE_SUBMISSION_PREFIX, successor)` of
    /// `CF_NATIVE_ORACLE` on a background thread. Called after a flush whose
    /// batch deleted submission rows (the prune when the oracle feed pauses):
    /// RocksDB keeps the tombstones until a compaction drops them, and every
    /// `sub` scan walks them meanwhile (`oracle_due` on every block).
    pub fn compact_pruned_submissions_in_background(&self) {
        let end = prefix_successor(ORACLE_SUBMISSION_PREFIX);
        self.compact_range_in_background(
            CF_NATIVE_ORACLE,
            ORACLE_SUBMISSION_PREFIX,
            end.as_deref(),
        );
    }

    /// Compact `[start, end)` of `cf` (`end` `None` = to the CF's end) on a
    /// background thread, bottommost level included, so the tombstones a flush
    /// left there stop costing later scans of the range (s89 fix B).
    ///
    /// Never blocks the caller and never runs on it: one long-lived worker per
    /// DB compacts one batch of requests at a time; a request while it runs
    /// joins the next batch (requests with the same `start` widen one range).
    /// A failure is logged and counted, never returned. Node-local: a
    /// compaction changes no read result, state, root or hash.
    pub fn compact_range_in_background(&self, cf: &'static str, start: &[u8], end: Option<&[u8]>) {
        let mut state = self.range_compaction.0.lock_state();
        self.request_locked(
            &mut state,
            cf,
            start.to_vec(),
            start.to_vec(),
            end.map(<[u8]>::to_vec),
        );
    }

    /// s99 (c): a flush deleted `n` rows in `[first, last]` under `prefix` of
    /// `cf` (one market's book rows, which getOrderBook scans). The count is
    /// summed per `(cf, prefix)` across flushes; once it reaches
    /// [`crate::cf::SCANNED_DELETES_COMPACTION_THRESHOLD`] the market's span of
    /// uncompacted deletes is compacted in the background
    /// ([`Self::compact_range_in_background`]) and the count starts over. One
    /// market per range, so a run rewrites only the files holding the churned
    /// markets, never the whole CF.
    pub fn note_scanned_deletes(
        &self,
        cf: &'static str,
        prefix: &[u8],
        n: u64,
        first: &[u8],
        last: &[u8],
    ) {
        let mut state = self.range_compaction.0.lock_state();
        let key = (cf, prefix.to_vec());
        let entry = state
            .uncompacted
            .entry(key.clone())
            .or_insert_with(|| (0, first.to_vec(), last.to_vec()));
        entry.0 += n;
        if first < entry.1.as_slice() {
            entry.1 = first.to_vec();
        }
        if last > entry.2.as_slice() {
            entry.2 = last.to_vec();
        }
        if entry.0 < crate::cf::SCANNED_DELETES_COMPACTION_THRESHOLD {
            return;
        }
        let (_, lo, mut hi) = state.uncompacted.remove(&key).expect("entry just updated");
        // The smallest key after `hi`: the range covers `hi` itself.
        hi.push(0);
        self.request_locked(&mut state, cf, key.1, lo, Some(hi));
    }

    /// Queue `[start, end)` of `cf` under `group`, start the worker if needed
    /// and wake it.
    fn request_locked(
        &self,
        state: &mut JobState,
        cf: &'static str,
        group: Vec<u8>,
        start: Vec<u8>,
        end: Option<Vec<u8>>,
    ) {
        let job = &self.range_compaction.0;
        match state.pending.entry((cf, group)) {
            std::collections::btree_map::Entry::Occupied(mut e) => {
                let r = e.get_mut();
                if start < r.0 {
                    r.0 = start;
                }
                r.1 = match (r.1.take(), end) {
                    (Some(a), Some(b)) => Some(a.max(b)),
                    _ => None,
                };
            }
            std::collections::btree_map::Entry::Vacant(e) => {
                e.insert((start, end));
            }
        }
        if !state.worker_started {
            *job.db
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Arc::downgrade(&self.db);
            let worker = Arc::clone(job);
            match std::thread::Builder::new()
                .name("torus-range-compact".into())
                .spawn(move || worker.work_loop())
            {
                Ok(handle) => {
                    *job.worker
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(handle);
                    state.worker_started = true;
                }
                Err(e) => {
                    tracing::warn!(error = %e, "could not start the deleted key range compaction (ignored)");
                    job.lock_runs().1 += 1;
                    state.pending.clear();
                    return;
                }
            }
        }
        job.work.notify_all();
    }

    /// Wait until no background range compaction is running or pending;
    /// returns the finished runs `(done, failed)` so far. Tests and benches
    /// only — the node never waits for it.
    pub fn wait_background_compaction(&self) -> (u64, u64) {
        let job = &self.range_compaction.0;
        let mut state = job.lock_state();
        while state.running || !state.pending.is_empty() {
            state = job.idle.wait(state).unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        drop(state);
        *job.lock_runs()
    }

    /// Test hook: make every following background compaction fail.
    #[cfg(test)]
    pub(crate) fn fail_background_compaction(&self, fail: bool) {
        self.range_compaction
            .0
            .fail
            .store(fail, std::sync::atomic::Ordering::Relaxed);
    }

    /// Test hook: the compaction job shared with the worker thread.
    #[cfg(test)]
    fn compaction_job(&self) -> Arc<RangeCompaction> {
        Arc::clone(&self.range_compaction.0)
    }

    /// Test hook: every range the background compactions ran, in order.
    #[cfg(test)]
    pub(crate) fn compacted_ranges(&self) -> Vec<KeyRange> {
        self.range_compaction
            .0
            .ran
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Get a shared handle to the underlying RocksDB instance.
    ///
    /// Used by [`torus_consensus::RocksKVStore::new`] which needs `Arc<DB>`.
    pub fn db_arc(&self) -> Arc<DB> {
        self.db.clone()
    }

    fn cf(&self, name: &str) -> Result<&ColumnFamily, StateError> {
        self.db
            .cf_handle(name)
            .ok_or_else(|| StateError::MissingColumnFamily(name.to_string()))
    }

    /// Get a column family handle by name (public, for WriteBatch usage).
    pub fn cf_handle(&self, name: &str) -> Result<&ColumnFamily, StateError> {
        self.cf(name)
    }

    /// Atomically apply a WriteBatch to the database.
    pub fn write(&self, batch: WriteBatch) -> Result<(), StateError> {
        self.db.write(batch)?;
        Ok(())
    }

    /// Atomically apply a WriteBatch with explicit [`rocksdb::WriteOptions`]
    /// (e.g. `low_pri` for the background trade writer so compaction
    /// back-pressure lands on it instead of the exec / consensus threads).
    pub fn write_with(
        &self,
        batch: WriteBatch,
        opts: &rocksdb::WriteOptions,
    ) -> Result<(), StateError> {
        self.db.write_opt(batch, opts)?;
        Ok(())
    }

    /// Fsync the shared write-ahead log, making every write issued so far
    /// crash-durable — surviving host power-loss / hard VM-stop, not just a
    /// process kill (which the OS page cache already covers).
    ///
    /// Task B / Option C: `kv_store` (consensus-meta / commit-frontier) and
    /// `native_da` (bodies) share this one `Arc<DB>` and therefore one WAL, so a
    /// single `flush_wal(true)` at the commit boundary makes the whole committed
    /// prefix (frontier + header + body) durable atomically-by-WAL-order. Call
    /// it exactly ONCE per committed block — never per-write or per-action.
    pub fn sync_wal(&self) -> Result<(), StateError> {
        self.db.flush_wal(true)?;
        Ok(())
    }

    // ---- Account operations (cf_accounts) ----

    /// Get account info by address. Returns `None` for non-existent accounts.
    pub fn get_account(&self, address: &Address) -> Result<Option<AccountInfo>, StateError> {
        let cf = self.cf(CF_ACCOUNTS)?;
        match self.db.get_cf(cf, address.as_slice())? {
            Some(data) => Ok(Some(decode_account_info(&data)?)),
            None => Ok(None),
        }
    }

    /// Store account info. The `code` field is stored separately in cf_code.
    pub fn put_account(&self, address: &Address, info: &AccountInfo) -> Result<(), StateError> {
        let cf = self.cf(CF_ACCOUNTS)?;
        self.db
            .put_cf(cf, address.as_slice(), encode_account_info(info))?;
        Ok(())
    }

    /// Delete an account.
    pub fn delete_account(&self, address: &Address) -> Result<(), StateError> {
        let cf = self.cf(CF_ACCOUNTS)?;
        self.db.delete_cf(cf, address.as_slice())?;
        Ok(())
    }

    // ---- Storage operations (cf_storage) ----

    /// Get a storage slot value. Returns `U256::ZERO` for unset slots.
    pub fn get_storage(&self, address: &Address, index: &U256) -> Result<U256, StateError> {
        let cf = self.cf(CF_STORAGE)?;
        let key = storage_key(address, index);
        match self.db.get_cf(cf, key)? {
            Some(data) => {
                if data.len() != 32 {
                    return Err(StateError::InvalidData(format!(
                        "storage value len {} != 32",
                        data.len()
                    )));
                }
                Ok(U256::from_be_slice(&data))
            }
            None => Ok(U256::ZERO),
        }
    }

    /// Set a storage slot. Zero values are deleted to save space.
    pub fn put_storage(
        &self,
        address: &Address,
        index: &U256,
        value: &U256,
    ) -> Result<(), StateError> {
        let cf = self.cf(CF_STORAGE)?;
        let key = storage_key(address, index);
        if value.is_zero() {
            self.db.delete_cf(cf, key)?;
        } else {
            self.db.put_cf(cf, key, value.to_be_bytes::<32>())?;
        }
        Ok(())
    }

    // ---- Code operations (cf_code) ----

    /// Get contract bytecode by its keccak256 hash.
    pub fn get_code(&self, code_hash: &B256) -> Result<Option<Vec<u8>>, StateError> {
        let cf = self.cf(CF_CODE)?;
        Ok(self.db.get_cf(cf, code_hash.as_slice())?)
    }

    /// Store contract bytecode keyed by its keccak256 hash.
    pub fn put_code(&self, code_hash: &B256, code: &[u8]) -> Result<(), StateError> {
        let cf = self.cf(CF_CODE)?;
        self.db.put_cf(cf, code_hash.as_slice(), code)?;
        Ok(())
    }

    // ---- Block hash operations ----

    /// Get block hash by block number.
    pub fn get_block_hash(&self, number: u64) -> Result<Option<B256>, StateError> {
        let cf = self.cf(CF_BLOCK_HEADERS)?;
        let key = number.to_be_bytes();
        match self.db.get_cf(cf, key)? {
            Some(data) if data.len() >= 32 => Ok(Some(B256::from_slice(&data[..32]))),
            _ => Ok(None),
        }
    }

    /// Store a block hash mapping (number -> hash and hash -> number).
    pub fn put_block_hash(&self, number: u64, hash: &B256) -> Result<(), StateError> {
        let cf = self.cf(CF_BLOCK_HEADERS)?;
        self.db.put_cf(cf, number.to_be_bytes(), hash.as_slice())?;

        let cf_reverse = self.cf(CF_BLOCK_HASH_TO_NUMBER)?;
        self.db
            .put_cf(cf_reverse, hash.as_slice(), number.to_be_bytes())?;
        Ok(())
    }

    // ---- Raw CF access (for other crates) ----

    /// Get a raw value from any column family.
    pub fn get_cf_raw(&self, cf_name: &str, key: &[u8]) -> Result<Option<Vec<u8>>, StateError> {
        let cf = self.cf(cf_name)?;
        Ok(self.db.get_cf(cf, key)?)
    }

    /// Batched point-get from one column family: ONE RocksDB `MultiGet` for all
    /// `keys` instead of N independent `get_cf_raw` calls. Result order matches
    /// `keys`; any per-key RocksDB error fails the whole read (same mapping as
    /// `get_cf_raw`). Hot reconstruct path reads ~25 native-DA bodies per block
    /// on the consensus thread, so this trades 25 point-gets for one.
    pub fn multi_get_cf_raw(
        &self,
        cf_name: &str,
        keys: &[&[u8]],
    ) -> Result<Vec<Option<Vec<u8>>>, StateError> {
        if keys.is_empty() {
            return Ok(Vec::new());
        }
        let cf = self.cf(cf_name)?;
        self.db
            .multi_get_cf(keys.iter().map(|k| (cf, *k)))
            .into_iter()
            .map(|r| r.map_err(StateError::from))
            .collect()
    }

    /// Presence check for a key in any column family without copying the value
    /// (pinned read). For hot paths that only need to know a (multi-KB) value is
    /// already local — e.g. the native-DA pre-warm filter.
    pub fn exists_cf_raw(&self, cf_name: &str, key: &[u8]) -> Result<bool, StateError> {
        let cf = self.cf(cf_name)?;
        Ok(self.db.get_pinned_cf(cf, key)?.is_some())
    }

    /// Put a raw value into any column family.
    pub fn put_cf_raw(&self, cf_name: &str, key: &[u8], value: &[u8]) -> Result<(), StateError> {
        let cf = self.cf(cf_name)?;
        self.db.put_cf(cf, key, value)?;
        Ok(())
    }

    /// Delete a key from any column family.
    pub fn delete_cf_raw(&self, cf_name: &str, key: &[u8]) -> Result<(), StateError> {
        let cf = self.cf(cf_name)?;
        self.db.delete_cf(cf, key)?;
        Ok(())
    }

    // ---- Session key operations (cf_sessions) ----

    /// Get a session by its ed25519 public key.
    pub fn get_session(
        &self,
        pubkey: &[u8; 32],
    ) -> Result<Option<torus_types::SessionData>, StateError> {
        match self.get_cf_raw(crate::cf::CF_SESSIONS, pubkey)? {
            Some(bytes) => {
                let data: torus_types::SessionData = serde_json::from_slice(&bytes)
                    .map_err(|e| StateError::InvalidData(e.to_string()))?;
                Ok(Some(data))
            }
            None => Ok(None),
        }
    }

    /// Store a session.
    pub fn put_session(
        &self,
        pubkey: &[u8; 32],
        data: &torus_types::SessionData,
    ) -> Result<(), StateError> {
        let bytes = serde_json::to_vec(data).map_err(|e| StateError::InvalidData(e.to_string()))?;
        self.put_cf_raw(crate::cf::CF_SESSIONS, pubkey, &bytes)
    }

    /// Delete a session.
    pub fn delete_session(&self, pubkey: &[u8; 32]) -> Result<(), StateError> {
        self.delete_cf_raw(crate::cf::CF_SESSIONS, pubkey)
    }

    /// Count active sessions for an owner address.
    /// Scans all sessions (acceptable since max 5 per owner, total count is bounded).
    pub fn count_sessions_for_owner(
        &self,
        owner: &alloy_primitives::Address,
    ) -> Result<usize, StateError> {
        let cf = self.cf(crate::cf::CF_SESSIONS)?;
        let iter = self.db.iterator_cf(cf, rocksdb::IteratorMode::Start);
        let mut count = 0;
        for (_key, value) in iter.flatten() {
            if let Ok(data) = serde_json::from_slice::<torus_types::SessionData>(&value) {
                if data.owner == *owner {
                    count += 1;
                }
            }
        }
        Ok(count)
    }

    /// List all column family names that were opened.
    pub fn column_families(&self) -> &'static [&'static str] {
        ALL_CF_NAMES
    }

    // ---- Iteration (for trie computation) ----

    /// Collect all accounts from the database.
    pub fn all_accounts(&self) -> Result<Vec<(Address, AccountInfo)>, StateError> {
        let cf = self.cf(CF_ACCOUNTS)?;
        let iter = self.db.iterator_cf(cf, rocksdb::IteratorMode::Start);
        let mut accounts = Vec::new();
        for item in iter {
            let (key, value) = item?;
            if key.len() != 20 {
                return Err(StateError::InvalidData(format!(
                    "account key len {} != 20",
                    key.len()
                )));
            }
            let address = Address::from_slice(&key);
            let info = decode_account_info(&value)?;
            accounts.push((address, info));
        }
        Ok(accounts)
    }

    /// Collect all storage slots for a given address.
    pub fn account_storage(&self, address: &Address) -> Result<Vec<(U256, U256)>, StateError> {
        let cf = self.cf(CF_STORAGE)?;
        let prefix = address.as_slice();
        let iter = prefix_iter(&self.db, &cf, prefix);
        let mut slots = Vec::new();
        for item in iter {
            let (key, value) = item?;
            if !key.starts_with(prefix) {
                break;
            }
            if key.len() != 52 {
                return Err(StateError::InvalidData(format!(
                    "storage key len {} != 52",
                    key.len()
                )));
            }
            let slot = U256::from_be_slice(&key[20..]);
            if value.len() != 32 {
                return Err(StateError::InvalidData(format!(
                    "storage value len {} != 32",
                    value.len()
                )));
            }
            let val = U256::from_be_slice(&value);
            slots.push((slot, val));
        }
        Ok(slots)
    }
}

// ---- revm DatabaseRef implementation ----
//
// We implement `DatabaseRef` (&self) rather than `Database` (&mut self) because
// RocksDB reads are thread-safe. Use `revm::database::WrapDatabaseRef<StateDb>`
// to get a `Database` impl, or pass to `StateBuilder::with_database_ref()`.

impl revm::DatabaseRef for StateDb {
    type Error = StateError;

    fn basic_ref(&self, address: Address) -> Result<Option<AccountInfo>, Self::Error> {
        self.get_account(&address)
    }

    fn code_by_hash_ref(&self, code_hash: B256) -> Result<Bytecode, Self::Error> {
        if code_hash == KECCAK_EMPTY || code_hash == B256::ZERO {
            return Ok(Bytecode::default());
        }
        match self.get_code(&code_hash)? {
            Some(bytes) => Ok(Bytecode::new_raw(Bytes::from(bytes))),
            None => Ok(Bytecode::default()),
        }
    }

    fn storage_ref(
        &self,
        address: Address,
        index: revm::primitives::StorageKey,
    ) -> Result<revm::primitives::StorageValue, Self::Error> {
        self.get_storage(&address, &index)
    }

    fn block_hash_ref(&self, number: u64) -> Result<B256, Self::Error> {
        Ok(self.get_block_hash(number)?.unwrap_or(B256::ZERO))
    }
}

// ---- Encoding helpers ----

/// Encode AccountInfo as 72 bytes: balance(32 BE) + nonce(8 BE) + code_hash(32).
pub fn encode_account_info(info: &AccountInfo) -> [u8; 72] {
    let mut buf = [0u8; 72];
    buf[..32].copy_from_slice(&info.balance.to_be_bytes::<32>());
    buf[32..40].copy_from_slice(&info.nonce.to_be_bytes());
    buf[40..72].copy_from_slice(info.code_hash.as_slice());
    buf
}

/// Decode AccountInfo from 72 bytes.
pub fn decode_account_info(data: &[u8]) -> Result<AccountInfo, StateError> {
    if data.len() != 72 {
        return Err(StateError::InvalidData(format!(
            "account data len {} != 72",
            data.len()
        )));
    }
    Ok(AccountInfo {
        balance: U256::from_be_slice(&data[..32]),
        nonce: u64::from_be_bytes(data[32..40].try_into().unwrap()),
        code_hash: B256::from_slice(&data[40..72]),
        account_id: None,
        code: None,
    })
}

/// Build a 52-byte storage key: address(20) ++ slot(32 BE).
pub fn storage_key(address: &Address, index: &U256) -> [u8; 52] {
    let mut key = [0u8; 52];
    key[..20].copy_from_slice(address.as_slice());
    key[20..52].copy_from_slice(&index.to_be_bytes::<32>());
    key
}

/// L3 #3: DB-wide `max_background_jobs` (flush + compaction threads). Default 4
/// (exact-today). `TORUS_MAX_BG_JOBS` overrides — e.g. `8` on an 18-core box to
/// relieve compaction-vs-exec CPU contention. Read once at DB open.
pub fn max_background_jobs() -> i32 {
    parse_positive_i32(std::env::var("TORUS_MAX_BG_JOBS").ok(), 4)
}

/// L3 #3: DB-wide `max_subcompactions` (parallelism WITHIN one compaction job).
/// `None` (default) = leave unset = exact-today. `TORUS_MAX_SUBCOMPACTIONS`
/// (>=1) sets it. Read once at DB open.
pub fn max_subcompactions() -> Option<u32> {
    parse_opt_u32_min1(std::env::var("TORUS_MAX_SUBCOMPACTIONS").ok())
}

/// L3 #4: DB-wide `bytes_per_sync` range-sync granularity, in bytes. Default
/// 1 MiB (exact-today). `TORUS_BYTES_PER_SYNC_MIB` overrides in whole MiB; `0`
/// disables range-sync entirely. Read once at DB open.
pub fn bytes_per_sync_bytes() -> u64 {
    match std::env::var("TORUS_BYTES_PER_SYNC_MIB").ok() {
        Some(v) => match v.trim().parse::<u64>() {
            Ok(mib) => mib.saturating_mul(1 << 20),
            Err(_) => 1 << 20,
        },
        None => 1 << 20,
    }
}

/// L3 #3: per-CF write-buffer size for the churny native CFs, in bytes. `None`
/// (default) = shared 128 MiB = exact-today. `TORUS_CHURNY_CF_WRITE_BUFFER_MB`
/// (>=1) gives them a smaller buffer so they flush small/often. Read once at DB
/// open.
pub fn churny_cf_write_buffer_bytes() -> Option<usize> {
    parse_opt_usize_min1_mb(std::env::var("TORUS_CHURNY_CF_WRITE_BUFFER_MB").ok())
}

/// STABILITY: DB-wide memtable budget, in bytes — the global cap on the SUM of
/// every column family's memtables (`Options::set_db_write_buffer_size`).
/// Without it the per-CF 128 MiB x 4 buffers across 44 CFs sum to ~21.5 GiB
/// unbounded, which no documented node class can absorb.
///
/// Default **1 GiB**: room for 8 simultaneously-full 128 MiB memtables, which
/// exceeds the hot multi-CF write set, so steady-state flush behaviour is
/// unchanged and only the burst tail is clipped. Sized against the 8 GB
/// validator class — with the 256 MiB shared block cache it puts the DB's
/// bounded memory at ~1.25 GiB.
///
/// `TORUS_DB_WRITE_BUFFER_MB` overrides in whole MiB; `0` disables the cap
/// (RocksDB default = exact-today unbounded). Recommended per node class:
/// `512` on a 4 GB node, default `1024` on 8 GB, `2048` on 16 GB+.
/// Read once at DB open.
pub fn db_write_buffer_bytes() -> usize {
    parse_usize_mb_or(std::env::var("TORUS_DB_WRITE_BUFFER_MB").ok(), 1024)
}

/// Pure parse: a positive `i32` env value, falling back to `default` on
/// unset / non-numeric / `< 1`.
fn parse_positive_i32(raw: Option<String>, default: i32) -> i32 {
    match raw.as_deref().map(str::trim).and_then(|s| s.parse::<i32>().ok()) {
        Some(n) if n >= 1 => n,
        _ => default,
    }
}

/// Pure parse: `Some(n)` for a `>= 1` env value, else `None` (unset / garbage).
fn parse_opt_u32_min1(raw: Option<String>) -> Option<u32> {
    raw.as_deref()
        .map(str::trim)
        .and_then(|s| s.parse::<u32>().ok())
        .filter(|&n| n >= 1)
}

/// Pure parse: whole-MiB env value `>= 1` → `Some(bytes)`, else `None`.
fn parse_opt_usize_min1_mb(raw: Option<String>) -> Option<usize> {
    raw.as_deref()
        .map(str::trim)
        .and_then(|s| s.parse::<usize>().ok())
        .filter(|&mb| mb >= 1)
        .map(|mb| mb.saturating_mul(1024 * 1024))
}

/// Pure parse: whole-MiB env value → bytes, falling back to `default_mb` MiB on
/// unset / non-numeric. Unlike [`parse_opt_usize_min1_mb`], `0` is HONOURED
/// (it disables the knob), matching `TORUS_BYTES_PER_SYNC_MIB`.
fn parse_usize_mb_or(raw: Option<String>, default_mb: usize) -> usize {
    let mb = raw
        .as_deref()
        .map(str::trim)
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(default_mb);
    mb.saturating_mul(1024 * 1024)
}

/// Runtime toggle: fsync the WAL once per committed block ([`StateDb::sync_wal`]).
///
/// Default **OFF** ⇒ byte-identical to today's async-write behavior (no flush).
/// Set `TORUS_SYNC_WAL_ON_COMMIT` to a truthy value (`1`/`true`/`yes`/`on`) to
/// enable. Proposer/replica-local, format-neutral, needs no coordination — safe
/// to A/B on a single node. Read once at first use (same pattern as
/// `evm_block_gas_budget`).
pub fn sync_wal_on_commit_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| parse_sync_wal_toggle(std::env::var("TORUS_SYNC_WAL_ON_COMMIT").ok()))
}

/// Pure parse of the `TORUS_SYNC_WAL_ON_COMMIT` value (default OFF). Split from
/// the `OnceLock` reader so the default and accepted spellings are unit-testable
/// without touching process-global state.
fn parse_sync_wal_toggle(raw: Option<String>) -> bool {
    match raw {
        Some(v) => matches!(v.trim(), "1" | "true" | "TRUE" | "yes" | "on"),
        None => false,
    }
}

#[cfg(test)]
mod prefix_successor_tests {
    use super::prefix_successor;

    #[test]
    fn successor_strips_trailing_ff_and_increments() {
        assert_eq!(prefix_successor(b"sub"), Some(b"suc".to_vec()));
        assert_eq!(prefix_successor(&[0x01, 0xff]), Some(vec![0x02]));
        assert_eq!(prefix_successor(&[0x01, 0xfe, 0xff, 0xff]), Some(vec![0x01, 0xff]));
        assert_eq!(prefix_successor(&[0x00]), Some(vec![0x01]));
        assert_eq!(prefix_successor(&[]), None);
        assert_eq!(prefix_successor(&[0xff]), None);
        assert_eq!(prefix_successor(&[0xff, 0xff, 0xff]), None);
    }

    /// Brute force over all 1..=2-byte prefixes and 1..=3-byte keys of an edge
    /// alphabet: `key` starts with `prefix` <=> prefix <= key < successor.
    #[test]
    fn successor_bounds_exactly_the_prefixed_keys() {
        const A: [u8; 5] = [0x00, 0x01, 0x7f, 0xfe, 0xff];
        let mut words: Vec<Vec<u8>> = vec![];
        for &a in &A {
            words.push(vec![a]);
            for &b in &A {
                words.push(vec![a, b]);
                for &c in &A {
                    words.push(vec![a, b, c]);
                }
            }
        }
        for p in words.iter().filter(|w| w.len() <= 2) {
            let succ = prefix_successor(p);
            for k in &words {
                let in_range = k >= p && succ.as_ref().is_none_or(|s| k < s);
                assert_eq!(in_range, k.starts_with(p), "prefix {p:02x?} key {k:02x?}");
            }
        }
    }
}

#[cfg(test)]
mod sync_wal_tests {
    use super::*;

    // Task 1 (RED): crash-durable commit via a single WAL fsync per block.
    // `kv_store` (consensus-meta/frontier) and `native_da` (bodies) share this
    // one `Arc<DB>`/WAL, so one flush covers the whole committed prefix.

    #[test]
    fn sync_wal_flushes_and_write_survives_reopen() {
        let dir = tempfile::tempdir().expect("tempdir");
        {
            let db = StateDb::open(dir.path()).expect("open temp StateDb");
            db.put_cf_raw(CF_CONSENSUS_META, b"frontier", b"height-1")
                .expect("put");
            db.sync_wal().expect("sync_wal must succeed");
        }
        // Reopen the (cleanly-closed) DB: the synced write is present.
        let db2 = StateDb::open(dir.path()).expect("reopen temp StateDb");
        assert_eq!(
            db2.get_cf_raw(CF_CONSENSUS_META, b"frontier").expect("get"),
            Some(b"height-1".to_vec()),
        );
    }

    #[test]
    fn sync_wal_on_commit_defaults_off() {
        // Env unset ⇒ toggle OFF ⇒ behavior byte-identical to today (no flush).
        assert!(!parse_sync_wal_toggle(None));
    }

    #[test]
    fn sync_wal_toggle_parses_truthy_spellings() {
        assert!(parse_sync_wal_toggle(Some("1".to_string())));
        assert!(parse_sync_wal_toggle(Some("true".to_string())));
        assert!(parse_sync_wal_toggle(Some(" on ".to_string())));
        assert!(!parse_sync_wal_toggle(Some("0".to_string())));
        assert!(!parse_sync_wal_toggle(Some("".to_string())));
    }

    // L3 #3 / #4: every compaction / sync knob defaults to exact-today.

    #[test]
    fn l3_max_bg_jobs_defaults_to_four() {
        assert_eq!(parse_positive_i32(None, 4), 4);
        assert_eq!(parse_positive_i32(Some("0".into()), 4), 4);
        assert_eq!(parse_positive_i32(Some("garbage".into()), 4), 4);
        assert_eq!(parse_positive_i32(Some(" 8 ".into()), 4), 8);
    }

    #[test]
    fn l3_max_subcompactions_defaults_unset() {
        assert_eq!(parse_opt_u32_min1(None), None);
        assert_eq!(parse_opt_u32_min1(Some("0".into())), None);
        assert_eq!(parse_opt_u32_min1(Some("x".into())), None);
        assert_eq!(parse_opt_u32_min1(Some("4".into())), Some(4));
    }

    #[test]
    fn l3_bytes_per_sync_defaults_1mib() {
        // The pure mapping the reader uses (whole MiB → bytes; 0 disables).
        let map = |mib: u64| mib.saturating_mul(1 << 20);
        assert_eq!(map(1), 1 << 20);
        assert_eq!(map(4), 4 << 20);
        assert_eq!(map(0), 0);
    }

    #[test]
    fn l3_churny_cf_buffer_defaults_unset() {
        assert_eq!(parse_opt_usize_min1_mb(None), None);
        assert_eq!(parse_opt_usize_min1_mb(Some("0".into())), None);
        assert_eq!(parse_opt_usize_min1_mb(Some("garbage".into())), None);
        assert_eq!(parse_opt_usize_min1_mb(Some("16".into())), Some(16 * 1024 * 1024));
    }

    // STABILITY: global memtable cap (`db_write_buffer_size`). The per-CF
    // buffers are unbounded in SUM; this is the only DB-wide bound.

    const MIB: usize = 1024 * 1024;

    #[test]
    fn db_write_buffer_defaults_to_1gib() {
        // Unset / garbage ⇒ the 1 GiB default, NOT rocksdb's unbounded 0.
        assert_eq!(parse_usize_mb_or(None, 1024), 1024 * MIB);
        assert_eq!(parse_usize_mb_or(Some("garbage".into()), 1024), 1024 * MIB);
        assert_eq!(parse_usize_mb_or(Some("".into()), 1024), 1024 * MIB);
        assert_eq!(parse_usize_mb_or(Some("-1".into()), 1024), 1024 * MIB);
    }

    #[test]
    fn db_write_buffer_env_overrides_in_whole_mib() {
        // Per-node-class guidance: 512 on a 4 GB node, 2048 on 16 GB+.
        assert_eq!(parse_usize_mb_or(Some("512".into()), 1024), 512 * MIB);
        assert_eq!(parse_usize_mb_or(Some(" 2048 ".into()), 1024), 2048 * MIB);
    }

    #[test]
    fn db_write_buffer_zero_disables_the_cap() {
        // `0` is rocksdb's "disabled" sentinel — the escape hatch back to
        // exact-today unbounded behaviour. It must NOT fall back to the default.
        assert_eq!(parse_usize_mb_or(Some("0".into()), 1024), 0);
    }

    #[test]
    fn db_write_buffer_cap_is_far_below_the_untuned_per_cf_sum() {
        // The bound this knob exists to enforce. `max_write_buffer_number` is 4
        // and every CF but CF_CONSENSUS_META (8 MiB) gets the 128 MiB buffer.
        let untuned_sum = (ALL_CF_NAMES.len() - 1) * 128 * MIB * 4 + 8 * MIB * 4;
        assert!(
            untuned_sum > 20 * 1024 * MIB,
            "untuned per-CF memtable sum {untuned_sum} should exceed 20 GiB across {} CFs",
            ALL_CF_NAMES.len()
        );
        // Default cap is ~1/20th of that, and fits under the 4 GB node floor.
        let cap = parse_usize_mb_or(None, 1024);
        assert!(cap < untuned_sum / 20);
        assert!(cap < 4 * 1024 * MIB);
    }

    #[test]
    fn db_opens_and_round_trips_with_global_memtable_cap() {
        // `set_db_write_buffer_size` is applied in `open()`: prove a real DB
        // still opens all 44 CFs, accepts writes, and reopens with data intact.
        let dir = tempfile::tempdir().expect("tempdir");
        {
            let db = StateDb::open(dir.path()).expect("open with db_write_buffer_size set");
            assert_eq!(db.column_families().len(), ALL_CF_NAMES.len());
            db.put_cf_raw(CF_ACCOUNTS, b"acct", b"value").expect("put");
            db.put_cf_raw(CF_CONSENSUS_META, b"meta", b"m").expect("put");
            db.sync_wal().expect("sync_wal");
        }
        let db2 = StateDb::open(dir.path()).expect("reopen");
        assert_eq!(
            db2.get_cf_raw(CF_ACCOUNTS, b"acct").expect("get"),
            Some(b"value".to_vec()),
        );
    }

    // ------------------------------------------------------------------
    // r3 exec-write-stall-attribution: RocksDB statistics + write-controller
    // knobs + a DB-wide runtime-stats snapshot (the bench sampler used to read
    // 0 because the only gauges were per-CF labelled families).
    // ------------------------------------------------------------------

    #[test]
    fn rocksdb_stats_level_parses_and_defaults_to_tickers() {
        // Default (unset / garbage) = 1 = tickers only (cheap: no timers on the
        // exec hot path). 0 = off, 2 = RocksDB's own default level with the
        // db.write / write.stall / flush / compaction histograms.
        assert_eq!(parse_rocksdb_stats_level(None), 1);
        assert_eq!(parse_rocksdb_stats_level(Some("garbage".into())), 1);
        assert_eq!(parse_rocksdb_stats_level(Some("".into())), 1);
        assert_eq!(parse_rocksdb_stats_level(Some("0".into())), 0);
        assert_eq!(parse_rocksdb_stats_level(Some("1".into())), 1);
        assert_eq!(parse_rocksdb_stats_level(Some(" 2 ".into())), 2);
        // Anything above 2 clamps to 2 (never the mutex-timing levels).
        assert_eq!(parse_rocksdb_stats_level(Some("9".into())), 2);
    }

    #[test]
    fn db_tuning_defaults_are_exact_today() {
        let t = DbTuning::from_raw(None, None, None, None, None);
        assert_eq!(t.stats_level, 1);
        assert_eq!(t.l0_slowdown_trigger, None);
        assert_eq!(t.l0_stop_trigger, None);
        assert_eq!(t.max_write_buffer_number, 4);
        assert!(!t.pipelined_write);
        assert_eq!(t, DbTuning::default());
    }

    #[test]
    fn db_tuning_parses_env_shapes() {
        let t = DbTuning::from_raw(
            Some("2".into()),
            Some("40".into()),
            Some("64".into()),
            Some("6".into()),
            Some("1".into()),
        );
        assert_eq!(t.stats_level, 2);
        assert_eq!(t.l0_slowdown_trigger, Some(40));
        assert_eq!(t.l0_stop_trigger, Some(64));
        assert_eq!(t.max_write_buffer_number, 6);
        assert!(t.pipelined_write);
        // Garbage / zero fall back to exact-today.
        let g = DbTuning::from_raw(
            Some("x".into()),
            Some("0".into()),
            Some("-3".into()),
            Some("1".into()),
            Some("0".into()),
        );
        assert_eq!(g.stats_level, 1);
        assert_eq!(g.l0_slowdown_trigger, None);
        assert_eq!(g.l0_stop_trigger, None);
        // max_write_buffer_number below 2 is nonsense for a live DB: clamp to 2.
        assert_eq!(g.max_write_buffer_number, 2);
        assert!(!g.pipelined_write);
    }

    #[test]
    fn runtime_stats_are_none_when_statistics_off() {
        let dir = tempfile::tempdir().expect("tempdir");
        let t = DbTuning {
            stats_level: 0,
            ..DbTuning::default()
        };
        let db = StateDb::open_with_tuning(dir.path(), &t).expect("open");
        db.put_cf_raw(CF_ACCOUNTS, b"k", b"v").expect("put");
        let s = db.runtime_stats();
        assert!(s.tickers.is_none(), "no statistics object => no tickers");
        assert!(s.histograms.is_none());
        // The DB-wide property snapshot is available regardless of statistics.
        assert!(s.memtable_bytes_all > 0, "one put must show up in some memtable");
        assert_eq!(s.write_stopped, 0);
    }

    #[test]
    fn runtime_stats_tickers_count_writes_and_bytes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let t = DbTuning {
            stats_level: 1,
            ..DbTuning::default()
        };
        let db = StateDb::open_with_tuning(dir.path(), &t).expect("open");
        for i in 0..8u32 {
            db.put_cf_raw(CF_ACCOUNTS, &i.to_be_bytes(), &[7u8; 256]).expect("put");
        }
        let s = db.runtime_stats();
        let tk = s.tickers.expect("tickers on at level 1");
        assert!(tk.write_self >= 8, "8 solo writes => write.self >= 8, got {}", tk.write_self);
        assert!(tk.bytes_written >= 8 * 256, "bytes.written {}", tk.bytes_written);
        assert!(tk.wal_bytes >= 8 * 256, "wal.bytes {}", tk.wal_bytes);
        assert_eq!(tk.stall_micros, 0, "an idle temp DB never stalls");
        // Level 1 = tickers only: histograms are absent.
        assert!(s.histograms.is_none(), "level 1 must not report histograms");
    }

    #[test]
    fn runtime_stats_histograms_present_at_level_2() {
        let dir = tempfile::tempdir().expect("tempdir");
        let t = DbTuning {
            stats_level: 2,
            ..DbTuning::default()
        };
        let db = StateDb::open_with_tuning(dir.path(), &t).expect("open");
        for i in 0..8u32 {
            db.put_cf_raw(CF_ACCOUNTS, &i.to_be_bytes(), &[7u8; 256]).expect("put");
        }
        let s = db.runtime_stats();
        let h = s.histograms.expect("histograms on at level 2");
        assert!(h.db_write.count >= 8, "db.write.micros count {}", h.db_write.count);
        assert_eq!(h.write_stall.count, 0);
    }

    #[test]
    fn open_applies_write_controller_knobs_and_round_trips() {
        // Non-default triggers + pipelined write must still open all CFs and
        // survive a reopen (node-local options, no format impact).
        let dir = tempfile::tempdir().expect("tempdir");
        let t = DbTuning {
            stats_level: 1,
            l0_slowdown_trigger: Some(40),
            l0_stop_trigger: Some(64),
            max_write_buffer_number: 6,
            pipelined_write: true,
            max_total_wal_size: None,
        };
        {
            let db = StateDb::open_with_tuning(dir.path(), &t).expect("open");
            assert_eq!(db.column_families().len(), ALL_CF_NAMES.len());
            db.put_cf_raw(CF_ACCOUNTS, b"acct", b"value").expect("put");
        }
        let db2 = StateDb::open(dir.path()).expect("reopen with defaults");
        assert_eq!(
            db2.get_cf_raw(CF_ACCOUNTS, b"acct").expect("get"),
            Some(b"value".to_vec()),
        );
    }

    #[test]
    fn write_with_low_pri_lands_rows() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = StateDb::open(dir.path()).expect("open");
        let mut batch = WriteBatch::default();
        batch.put_cf(db.cf_handle(CF_ACCOUNTS).unwrap(), b"lp", b"v");
        let mut wo = rocksdb::WriteOptions::default();
        wo.set_low_pri(true);
        db.write_with(batch, &wo).expect("low-pri write");
        assert_eq!(db.get_cf_raw(CF_ACCOUNTS, b"lp").unwrap(), Some(b"v".to_vec()));
    }
}

#[cfg(test)]
mod compaction_drop_tests {
    //! Teardown SIGABRT (s18): the compaction worker upgrades its `Weak<DB>`
    //! for the whole run, so it could hold the LAST `Arc<DB>` and close
    //! RocksDB on its own thread during process exit. The last `StateDb`
    //! owner must wait for it instead.
    use super::*;
    use std::sync::atomic::Ordering::Relaxed;
    use std::time::{Duration, Instant};

    /// Start a compaction that keeps its strong `Arc<DB>` for `hold_ms`;
    /// returns once the worker holds it.
    fn start_held_compaction(db: &StateDb, hold_ms: u64) -> Arc<RangeCompaction> {
        let job = db.compaction_job();
        job.hold_ms.store(hold_ms, Relaxed);
        let before = job.started.load(Relaxed);
        db.compact_pruned_submissions_in_background();
        let deadline = Instant::now() + Duration::from_secs(10);
        while job.started.load(Relaxed) == before {
            assert!(Instant::now() < deadline, "worker never took the DB");
            std::thread::sleep(Duration::from_millis(1));
        }
        job
    }

    #[test]
    fn last_drop_waits_for_the_compaction_and_closes_the_db_itself() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = StateDb::open(dir.path()).expect("open");
        let weak = Arc::downgrade(&db.db);
        let job = start_held_compaction(&db, 300);
        drop(db);
        assert!(
            weak.upgrade().is_none(),
            "the last StateDb drop must close RocksDB on its own thread, not leave the last Arc<DB> to the worker"
        );
        assert_eq!(
            *job.lock_runs(),
            (1, 0),
            "drop returned before the run finished"
        );
        assert!(
            !job.lock_state().running,
            "job still marked running after drop"
        );
        assert_eq!(
            Arc::strong_count(&job),
            1,
            "the worker thread is still alive"
        );
        StateDb::open(dir.path()).expect("reopen right after drop (LOCK released)");
    }

    #[test]
    fn drop_with_a_pending_rerun_skips_it_and_returns_promptly() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = StateDb::open(dir.path()).expect("open");
        let job = start_held_compaction(&db, 200);
        db.compact_pruned_submissions_in_background();
        assert!(
            !job.lock_state().pending.is_empty(),
            "second request must be queued as a re-run"
        );
        let t = Instant::now();
        drop(db);
        let took = t.elapsed();
        assert_eq!(*job.lock_runs(), (1, 0), "the re-run must not start after the owner dropped");
        assert_eq!(job.started.load(Relaxed), 1);
        assert_eq!(Arc::strong_count(&job), 1, "the worker thread is still alive");
        assert!(took < Duration::from_secs(5), "drop took {took:?}");
    }

    #[test]
    fn dropping_a_clone_does_not_wait() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = StateDb::open(dir.path()).expect("open");
        let weak = Arc::downgrade(&db.db);
        let clone = db.clone();
        let job = start_held_compaction(&db, 300);
        let t = Instant::now();
        drop(clone);
        assert!(
            t.elapsed() < Duration::from_millis(150),
            "a clone drop waited {:?}",
            t.elapsed()
        );
        assert!(
            job.lock_state().running,
            "the compaction should still be running"
        );
        drop(db);
        assert!(weak.upgrade().is_none(), "the last drop must close RocksDB itself");
        assert_eq!(*job.lock_runs(), (1, 0));
    }
}
