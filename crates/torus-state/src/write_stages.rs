//! Q6 step 0 (`docs/plans/q6-checkpoint-slicing.md` section 5): RocksDB's
//! stage split of ONE write, read from the calling thread's perf context.
//!
//! Used only around the flush worker's checkpoint write
//! ([`crate::CheckpointLayer::flush_checkpoint`]) and the consensus thread's
//! two state-DB writes per commit (torus-consensus `app.rs`: the
//! commit-manifest put and the commit-persist WriteBatch write). The perf
//! level is thread-local: it is raised for that one call and set back to
//! RocksDB's default (`kEnableCount`) right after, also on unwind, so every
//! other write and read (other threads, and the caller's own other work)
//! keeps today's count-only cost.
//!
//! What the timers cover with pipelined write (RocksDB 10.4.2,
//! `DBImpl::PipelinedWriteImpl`, db_impl_write.cc:970-1160):
//! - `write_pre_and_post_process_time` runs from entry through
//!   `JoinBatchGroup`, so it holds the WAL-stage queue wait (waiting to lead
//!   a WAL group), apart from `PreprocessWrite`, which it excludes;
//! - `write_wal_time`: writing the group's WAL record;
//! - `write_memtable_time`: the memtable insert as memtable-writer leader;
//! - `write_delay_time`: write-controller delay / stop (`DelayWrite`);
//! - NOT exposed by the C API: `write_thread_wait_nanos` and
//!   `write_scheduling_flushes_compactions_time` (`PreprocessWrite`: memtable
//!   switch, `WaitForPendingWrites`, flush scheduling). Those, the wait for
//!   memtable-writer leadership (`AwaitState` in `ExitAsBatchGroupLeader`),
//!   untimed glue and, for a consuming `f` such as `StateDb::write`,
//!   rust-rocksdb's drop of the `WriteBatch` (after RocksDB released the
//!   queue; ~50 ms for a 200 MB batch) are the residual = wall - (WAL +
//!   memtable + delay + pre/post).
//! - `db_mutex_lock_nanos` / `db_condition_wait_nanos` (DB mutex and its
//!   condition variable) overlap the stages above; read them as the switch
//!   signal, not as a stage.
//!
//! RocksDB warns the stage split "may be inaccurate" with pipelined write
//! (perf_context.h:190-192): report it as a split, not as exact stage times.

use rocksdb::perf::{set_perf_stats, PerfContext, PerfMetric, PerfStatsLevel};

/// RocksDB's per-stage split of one write call (seconds) and its wall-clock
/// span (unix ms). See the module docs for what each stage covers.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct WriteStageSplit {
    /// Wall time of the `write` call.
    pub wall_seconds: f64,
    /// `write_wal_time`.
    pub wal_seconds: f64,
    /// `write_memtable_time`.
    pub memtable_seconds: f64,
    /// `write_delay_time`.
    pub delay_seconds: f64,
    /// `write_pre_and_post_process_time` (includes the WAL-group join wait).
    pub pre_post_seconds: f64,
    /// `db_mutex_lock_nanos` (overlaps the stages).
    pub mutex_seconds: f64,
    /// `db_condition_wait_nanos` (overlaps the stages).
    pub cond_wait_seconds: f64,
    /// Unix ms just before the call.
    pub start_unix_ms: u64,
    /// Unix ms just after the call.
    pub end_unix_ms: u64,
}

impl WriteStageSplit {
    /// Wall minus the four write-path stage timers, floored at 0: the
    /// memtable-leadership wait, `PreprocessWrite` (memtable switch,
    /// `WaitForPendingWrites`), untimed glue and the consumed batch's drop.
    pub fn residual_seconds(&self) -> f64 {
        (self.wall_seconds
            - (self.wal_seconds
                + self.memtable_seconds
                + self.delay_seconds
                + self.pre_post_seconds))
            .max(0.0)
    }
}

/// RocksDB's `kEnableTime` (6: count, wait, time, CPU time AND mutex time).
/// rust-rocksdb 0.24's `PerfStatsLevel` predates RocksDB's `kEnableWait` (3),
/// so its variants are off by one from `EnableCount` up: its `EnableTime` (5)
/// is C++ `kEnableTimeAndCPUTimeExceptForMutex` (no mutex timers) and its
/// `OutOfBound` (6) is C++ `kEnableTime` (`rocksdb_set_perf_level` passes the
/// integer through; include/rocksdb/perf_level.h). Pinned by
/// `measure_times_the_db_mutex`.
const ROCKSDB_ENABLE_TIME: PerfStatsLevel = PerfStatsLevel::OutOfBound;

/// Sets RocksDB's default per-thread level back (`kEnableCount` = 2, same
/// integer in both enums; perf_level.cc), also on unwind.
struct RestoreDefaultLevel;

impl Drop for RestoreDefaultLevel {
    fn drop(&mut self) {
        set_perf_stats(PerfStatsLevel::EnableCount);
    }
}

fn unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Run `f` (one RocksDB write) with full perf timing on this thread and
/// return RocksDB's stage split of it. Cost: the perf-context reset / reads
/// (~70 counters) plus one clock read per timer start / stop inside the
/// write (a few per stage, a few hundred for a ~200 MB WAL record's buffered
/// appends) -- microseconds per call. No per-key timers on the insert path.
/// `f` must be one write on THIS thread (the perf context is thread-local);
/// the split is meaningless for anything else.
pub fn measure<R>(f: impl FnOnce() -> R) -> (R, WriteStageSplit) {
    let mut ctx = PerfContext::default();
    set_perf_stats(ROCKSDB_ENABLE_TIME);
    let restore = RestoreDefaultLevel;
    ctx.reset();
    let start_unix_ms = unix_ms();
    let timer = std::time::Instant::now();
    let out = f();
    let wall_seconds = timer.elapsed().as_secs_f64();
    let end_unix_ms = unix_ms();
    drop(restore);
    let secs = |m: PerfMetric| ctx.metric(m) as f64 * 1e-9;
    let split = WriteStageSplit {
        wall_seconds,
        wal_seconds: secs(PerfMetric::WriteWalTime),
        memtable_seconds: secs(PerfMetric::WriteMemtableTime),
        delay_seconds: secs(PerfMetric::WriteDelayTime),
        pre_post_seconds: secs(PerfMetric::WritePreAndPostProcessTime),
        mutex_seconds: secs(PerfMetric::DbMutexLockNanos),
        cond_wait_seconds: secs(PerfMetric::DbConditionWaitNanos),
        start_unix_ms,
        end_unix_ms,
    };
    (out, split)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cf::CF_NATIVE_ORDERS;
    use crate::StateDb;
    use rocksdb::perf::{PerfContext, PerfMetric};
    use rocksdb::WriteBatch;

    fn temp_db() -> (StateDb, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let db = StateDb::open(dir.path()).unwrap();
        (db, dir)
    }

    /// `n` puts of 1 KiB values into one batch.
    fn batch(db: &StateDb, n: u32) -> WriteBatch {
        let cf = db.inner().cf_handle(CF_NATIVE_ORDERS).unwrap();
        let mut b = WriteBatch::default();
        for i in 0..n {
            b.put_cf(cf, i.to_be_bytes(), [7u8; 1024]);
        }
        b
    }

    fn now_ms() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64
    }

    /// One real ~20 MB state-DB write: WAL and memtable insert are timed and
    /// carry most of the wall, the stage timers sit inside the wall, the
    /// residual is the rest, and the span lies inside the caller's clock.
    #[test]
    fn measure_splits_a_real_state_db_write() {
        let (db, _dir) = temp_db();
        let b = batch(&db, 20_000);
        let before = now_ms();
        let (r, s) = measure(|| db.write(b));
        r.unwrap();
        let after = now_ms();
        assert!(s.wall_seconds > 0.0, "{s:?}");
        assert!(s.wal_seconds > 0.0, "WAL time: {s:?}");
        assert!(s.memtable_seconds > 0.0, "memtable time: {s:?}");
        let stages = s.wal_seconds + s.memtable_seconds + s.delay_seconds + s.pre_post_seconds;
        assert!(
            stages <= s.wall_seconds + 1e-4,
            "stage timers run inside the call: {s:?}"
        );
        assert!(
            s.wal_seconds + s.memtable_seconds > 0.5 * s.wall_seconds,
            "an uncontended 20 MB write is mostly WAL + insert: {s:?}"
        );
        assert!(
            (s.residual_seconds() - (s.wall_seconds - stages)).abs() < 1e-9,
            "residual = wall - stages: {s:?}"
        );
        assert!(
            before <= s.start_unix_ms && s.start_unix_ms <= s.end_unix_ms && s.end_unix_ms <= after,
            "span [{}, {}] inside [{before}, {after}]",
            s.start_unix_ms,
            s.end_unix_ms
        );
    }

    /// Mutex timing is on inside `measure`: RocksDB's `kEnableTime` (6), not
    /// rust-rocksdb's `PerfStatsLevel::EnableTime` (5 = C++
    /// `kEnableTimeAndCPUTimeExceptForMutex` since RocksDB added `kEnableWait`).
    #[test]
    fn measure_times_the_db_mutex() {
        let (db, _dir) = temp_db();
        db.write(batch(&db, 10)).unwrap();
        // A memtable flush takes the DB mutex on this thread.
        let (r, s) = measure(|| {
            db.inner()
                .flush_cf(db.inner().cf_handle(CF_NATIVE_ORDERS).unwrap())
        });
        r.unwrap();
        assert!(s.mutex_seconds > 0.0, "DB mutex time: {s:?}");
    }

    /// Outside `measure` (also after a panic inside it) the thread is back at
    /// RocksDB's default count-only level: a later write is not timed.
    #[test]
    fn perf_timing_is_off_after_measure() {
        let (db, _dir) = temp_db();
        let (r, _) = measure(|| db.write(batch(&db, 100)));
        r.unwrap();
        let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            measure(|| -> () { panic!("inside measure") })
        }));
        assert!(panicked.is_err());
        let mut ctx = PerfContext::default();
        ctx.reset();
        db.write(batch(&db, 1000)).unwrap();
        assert_eq!(ctx.metric(PerfMetric::WriteWalTime), 0, "WAL not timed");
        assert_eq!(
            ctx.metric(PerfMetric::WriteMemtableTime),
            0,
            "memtable not timed"
        );
        assert_eq!(ctx.metric(PerfMetric::WritePreAndPostProcessTime), 0);
    }
}
