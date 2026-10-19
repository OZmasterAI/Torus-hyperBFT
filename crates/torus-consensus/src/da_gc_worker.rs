//! RH4: the DA body GC off the consensus thread (bench 8o: 1.4-1.7 ms per
//! native block and 51-189 ms stalls on the hotstuff algorithm thread, 95%
//! of it the GC's DB write).
//!
//! The consensus thread hands each dispatched height to one worker thread
//! and returns: `DaGcWorker::submit` moves the block's action hashes (already
//! computed for the mempool prune) into a `DaGcJob` and `try_send`s it on a
//! bounded queue ([`DA_GC_QUEUE_CAP`]). No DB read or write, no body lookup,
//! no wait. The worker runs the passes (`DaGcRunner::run`), in height order:
//! the body restore, the record, the delete scan and the batch write, with
//! every rule of [`crate::da_gc`] unchanged. `TORUS_DA_GC_INLINE=1` runs the
//! same pass on the consensus thread instead (the pre-worker behaviour, for
//! bisects and A/B benches).
//!
//! # The handoff
//!
//! A full queue never blocks consensus: the job is dropped and only its
//! height kept ("coalesced", `torus_da_gc_coalesced_heights`). Its record
//! then comes from the block tree, by the cursor rule that already covers a
//! crash between dispatch and the GC record: the next pass finds the cursor
//! behind and back-fills the missing heights from the tree before it deletes
//! anything. If nothing follows the dropped heights, the worker runs a pass at
//! the highest one itself, reading its block from the tree. A crash with jobs
//! queued (or one mid-pass) is the same case: the records that did not land
//! are back-filled on restart.
//!
//! The worker drains the queue in batches: every job but the last of a batch
//! only records (`keep_from` 0, nothing is due); the last one runs the full
//! pass. A lagging worker therefore does one delete scan per batch, and its
//! passes do not hold as `dispatch_behind` merely because the queue holds
//! dispatched heights above them.
//!
//! # What changes for the rules
//!
//! A pass runs later than the dispatch it belongs to, so the tree may have
//! moved on (newer commits, newer tail blocks) and other threads may check
//! bodies while it runs. Each rule still holds:
//!
//! - Due rows, the tree bound and the back-fill are decided from the durable
//!   GC state and the tree as the pass reads them, as before: a late pass
//!   only deletes later, never earlier than the inline pass of the same
//!   height would.
//! - Dispatch behind the committed tip: a committed height above the pass's
//!   holds it, as before (a queued job is one of those).
//! - The uncommitted tail is read from the committed tip the pass read, not
//!   the current one ([`crate::da_gc::uncommitted_native_tail_from`]): a block
//!   committed between the tip read and the tail read is still pinned.
//! - Blocks that enter the tree after the tail read: their bodies were checked
//!   (proposal check, validation, the proposer's mirror) by a thread that
//!   pinned the hashes first ([`torus_state::DaGcFence`]), and the pass drops
//!   pinned bodies from its deletes right before the write. A check that
//!   races the write sees the bodies as being deleted and treats them as
//!   missing. Pins are released once the worker finished a pass at or above
//!   their height.
//! - The body restore of a dispatched block runs on the worker, from the
//!   durable body row dispatch wrote (from the block's actions when that row
//!   did not land: then the job carries them).

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError};
use std::sync::Arc;
use std::time::{Duration, Instant};

use torus_mempool::Mempool;
use torus_state::{DaGcOutcome, DaGcPassStats, StateDb};
use torus_types::{SignedNativeAction, B256};

use crate::da_gc::{BlockTreeHistory, DaBodyGc, DA_GC_LIMITS};

/// Most dispatched heights the handoff queue holds (about 5 KB each at 150
/// actions per block). A worker this far behind gets the next heights by
/// height only, and back-fills their records from the block tree.
pub const DA_GC_QUEUE_CAP: usize = 512;

/// Whether `TORUS_DA_GC_INLINE` (`env`) runs the GC pass on the consensus
/// thread (`1`), as before the worker. Anything else: the worker.
pub fn resolve_da_gc_inline(env: Option<&str>) -> bool {
    env.map(str::trim) == Some("1")
}

/// One dispatched height for the GC worker.
pub(crate) struct DaGcJob {
    pub height: u64,
    /// The block's native-action hashes.
    pub hashes: Vec<B256>,
    /// `false`: the block's record is unknown (its body row did not land and
    /// it has actions): the cursor stays at it.
    pub record: bool,
    /// Erasure shards custodied per body.
    pub shard_count: u16,
    /// The block's actions, only when its body row did not land (the restore
    /// cannot reload them then).
    pub actions: Option<Vec<SignedNativeAction>>,
}

/// Everything one GC pass needs, shared by the inline path and the worker.
pub(crate) struct DaGcRunner {
    pub gc: DaBodyGc,
    pub mempool: Arc<Mempool>,
    /// The app's state DB (`CF_BLOCK_BODIES`: the restore's source).
    pub state_db: StateDb,
    pub metrics: Option<Arc<torus_telemetry::Metrics>>,
    /// The slow-pass threshold in ns (`TORUS_DA_GC_SLOW_MS`).
    pub slow_pass_nanos: Arc<AtomicU64>,
    /// Fence the deletes against concurrent body checks (worker passes).
    pub fenced: bool,
    #[cfg(test)]
    pub hooks: Arc<DaGcTestHooks>,
}

impl DaGcRunner {
    fn slow_pass(&self) -> Duration {
        Duration::from_nanos(self.slow_pass_nanos.load(Ordering::Relaxed))
    }

    /// Put back the bodies `hashes` (a committed block's at `height`) that
    /// the DA store lacks, from `actions` (the block's) or else its durable
    /// body row. `backfill`: a height the pass back-fills (log wording).
    fn restore(
        &self,
        height: u64,
        hashes: &[B256],
        actions: Option<&[SignedNativeAction]>,
        backfill: bool,
    ) {
        if hashes.is_empty() {
            return;
        }
        let loaded;
        let actions = match actions {
            Some(actions) => actions,
            None => {
                if self.mempool.native_da_missing(hashes).is_empty() {
                    return;
                }
                match crate::app::load_replay_body(&self.state_db, height) {
                    Some(body) => {
                        loaded = body.native_actions;
                        &loaded[..]
                    }
                    None => {
                        tracing::warn!(
                            height,
                            backfill,
                            "DA body GC: a referenced body is missing and the durable block \
                             body is unavailable to restore it"
                        );
                        return;
                    }
                }
            }
        };
        if actions.len() != hashes.len() {
            tracing::warn!(
                height,
                backfill,
                "DA body GC: block body does not match the block's hashes, not restoring"
            );
            return;
        }
        match self.mempool.mirror_missing_native_to_da(actions, hashes) {
            Ok(0) => {}
            Ok(restored) if backfill => tracing::info!(
                height,
                restored,
                "DA body GC back-fill: restored bodies a committed block references"
            ),
            Ok(restored) => tracing::info!(
                height,
                restored,
                "DA body GC: restored bodies a dispatched block references (re-included after \
                 an earlier row aged out)"
            ),
            Err(e) => tracing::warn!(
                height,
                backfill,
                "DA body GC: restoring a committed block's missing bodies failed: {e}"
            ),
        }
    }

    /// One GC pass for the dispatched `height` (see
    /// `TorusApp::gc_da_bodies_after_dispatch` for the rules): restore the
    /// block's missing bodies (from `actions`, else its durable body row),
    /// record `hashes` (`record = false`: unknown, the cursor stays), then,
    /// with `delete`, delete what is due. Without `delete` it only records
    /// (`keep_from` 0: nothing is due, no tip or tail read). Metrics and the
    /// slow-pass line as before.
    pub fn run(
        &self,
        height: u64,
        actions: Option<&[SignedNativeAction]>,
        hashes: &[B256],
        record: bool,
        shard_count: u16,
        delete: bool,
    ) -> DaGcOutcome {
        #[cfg(test)]
        self.hooks.note_pass();
        let gc = &self.gc;
        let started = Instant::now();
        self.restore(height, hashes, actions, false);
        // Slow-pass observability: plain `Instant`s between the phases.
        let restored_at = Instant::now();
        let keep_from = if delete {
            gc.keep_from().unwrap_or_else(|e| {
                tracing::warn!(
                    height,
                    "DA body GC: block-tree prune pointer unreadable, deleting nothing this \
                     pass: {e}"
                );
                0
            })
        } else {
            0
        };
        let pointer_read = restored_at.elapsed();
        // Heights this pass finds unrecorded (a crash after an earlier
        // dispatch's body write, a failed record, a coalesced handoff) are
        // re-read from the block tree; their missing bodies are restored
        // like this height's.
        let restore = |at: u64, hashes: &[B256], actions: Option<&[SignedNativeAction]>| {
            self.restore(at, hashes, actions, true)
        };
        let fence = self.fenced.then(|| self.mempool.da_gc_fence().as_ref());
        let history = BlockTreeHistory::new(&gc.block_tree_db, &restore).with_fence(fence);
        #[cfg(test)]
        let history = history.with_test_hooks(&self.hooks);
        let mut stats = DaGcPassStats::default();
        let out = self.mempool.gc_committed_native_da_timed(
            height,
            record.then_some(hashes),
            shard_count,
            gc.retention,
            keep_from,
            DA_GC_LIMITS,
            &history,
            &mut stats,
        );
        let elapsed = started.elapsed();
        let slow = elapsed >= self.slow_pass();
        if slow {
            self.log_slow_pass(
                height,
                hashes.len(),
                elapsed,
                restored_at.duration_since(started),
                pointer_read,
                &stats,
                &out,
                history.tail_blocks(),
            );
        }
        if let Some(ref m) = self.metrics {
            m.da_gc_seconds.observe(elapsed.as_secs_f64());
            m.da_gc_last_pass_max.observe(elapsed.as_secs_f64());
            if slow {
                m.da_gc_slow_passes.inc();
            }
            m.da_bodies_gc_deleted.inc_by(out.bodies_deleted);
            m.da_gc_fence_kept.inc_by(out.fence_pinned);
            // P3 (claude-review r4): every held pass, by its reason.
            if let Some(reason) = out.hold_reason {
                m.da_gc_held_passes
                    .get_or_create(&vec![("reason".into(), reason.as_str().into())])
                    .inc();
            }
        }
        if out.tail_pinned > 0 {
            tracing::warn!(
                height,
                kept = out.tail_pinned,
                "DA body GC: kept due bodies a block above the committed tip re-includes"
            );
        }
        if out.fence_pinned > 0 {
            tracing::info!(
                height,
                kept = out.fence_pinned,
                "DA body GC: kept due bodies a block being checked right now references"
            );
        }
        if out.heights_backfilled > 0 {
            tracing::info!(
                height,
                backfilled = out.heights_backfilled,
                "DA body GC: back-filled unrecorded heights from the block tree"
            );
        }
        if out.backlog {
            tracing::debug!(
                height,
                rows = out.rows_deleted,
                "DA body GC: backlog left for the next pass"
            );
        }
        out
    }

    /// A pass at `height` with its record read from the block tree (a height
    /// handed off while the queue was full; nothing followed it).
    fn run_from_tree(&self, height: u64, shard_count: u16) -> DaGcOutcome {
        match crate::da_gc::committed_native_actions_in_tree(&self.gc.block_tree_db, height) {
            Ok(Some((hashes, actions))) => self.run(
                height,
                actions.as_deref(),
                &hashes,
                true,
                shard_count,
                true,
            ),
            other => {
                tracing::warn!(
                    height,
                    read = ?other.map(|o| o.is_some()),
                    "DA body GC: a coalesced height is not in the block tree, leaving it \
                     unrecorded (the next pass retries)"
                );
                self.run(height, None, &[], false, shard_count, true)
            }
        }
    }

    /// The one warn line of a DA body GC pass at or over the slow-pass
    /// threshold (8h: rare 51-205 ms passes, nothing recorded to explain
    /// them). Times in ms (3 decimals): `restore_ms` the body presence
    /// `MultiGet` (and any restore write), `bookkeeping_read_ms` the tree's
    /// prune pointer plus the GC state, `record_ms` the latest-reference
    /// `MultiGet` and the record's puts, `backfill_ms` back-filling from the
    /// tree, `tip_ms` / `tail_ms` the tree's committed tip and uncommitted
    /// tail (`tail_blocks` blocks decoded, `tail_hashes` hashes),
    /// `delete_scan_ms` the due-row walk (tail excluded) and `write_ms` the
    /// batch write (record, deletes and GC state together). `dispatch_lag` is
    /// the tree's committed tip minus this height (the tip read again here
    /// when the pass did not). `fence_pinned`: due bodies kept for a block
    /// being checked. A failed pass logs too (`held=store_error`, `error` its
    /// message, the failed phase's time included; `none` when it did not
    /// fail). Off the hot path: only slow passes get here.
    #[allow(clippy::too_many_arguments)]
    fn log_slow_pass(
        &self,
        height: u64,
        recorded: usize,
        total: Duration,
        restore: Duration,
        pointer_read: Duration,
        stats: &DaGcPassStats,
        out: &DaGcOutcome,
        tail_blocks: usize,
    ) {
        let ms = |d: Duration| (d.as_secs_f64() * 1e6).round() / 1e3;
        let tip = stats.committed_tip.or_else(|| {
            crate::da_gc::read_tree_committed_tip(&self.gc.block_tree_db)
                .ok()
                .flatten()
        });
        let opt = |v: Option<u64>| v.map_or_else(|| "none".to_string(), |v| v.to_string());
        tracing::warn!(
            height,
            total_ms = ms(total),
            threshold_ms = self.slow_pass().as_millis() as u64,
            bodies_recorded = recorded,
            rows_deleted = out.rows_deleted,
            bodies_scanned = stats.bodies_scanned,
            bodies_deleted = out.bodies_deleted,
            shard_keys_deleted = out.shard_keys_deleted,
            tail_pinned = out.tail_pinned,
            fence_pinned = out.fence_pinned,
            backlog = out.backlog,
            restore_ms = ms(restore),
            bookkeeping_read_ms = ms(pointer_read + stats.state_read),
            record_ms = ms(stats.record),
            backfill_ms = ms(stats.backfill),
            heights_backfilled = out.heights_backfilled,
            tip_ms = ms(stats.tip_read),
            tail_ms = ms(stats.tail_read),
            tail_blocks,
            tail_hashes = stats.tail_hashes,
            delete_scan_ms = ms(stats.delete_scan),
            write_ms = ms(stats.write),
            committed_tip = %opt(tip),
            dispatch_lag = %opt(tip.map(|tip| tip.saturating_sub(height))),
            held = %out.hold_reason.map_or("none", |r| r.as_str()),
            error = %stats.error.as_deref().unwrap_or("none"),
            "DA body GC: slow pass"
        );
    }
}

/// What `DaGcWorker::submit` did with a height.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Handoff {
    /// In the queue.
    Queued,
    /// The queue was full: only the height was kept (its record is
    /// back-filled from the block tree).
    Coalesced,
    /// The worker is gone (it panicked): nothing more is GC'd.
    Dead,
}

/// State shared by the consensus thread and the worker (all atomics).
struct WorkerShared {
    /// Jobs in the queue.
    queued: AtomicUsize,
    /// The worker is processing a batch.
    busy: AtomicBool,
    /// Highest height handed off.
    dispatched: AtomicU64,
    /// Highest height whose pass finished.
    processed: AtomicU64,
    /// Highest height handed off while the queue was full (0 = none), and
    /// its shard count.
    dropped_through: AtomicU64,
    dropped_shards: AtomicU64,
    /// The worker thread exited (a panic).
    dead: AtomicBool,
    /// Shutdown: stop after the current pass.
    stop: AtomicBool,
    metrics: Option<Arc<torus_telemetry::Metrics>>,
    #[cfg(test)]
    hooks: Arc<DaGcTestHooks>,
}

impl WorkerShared {
    #[cfg(test)]
    fn wake_gate(&self) {
        self.hooks.kill();
    }

    fn update_gauges(&self) {
        if let Some(ref m) = self.metrics {
            m.da_gc_queue_depth
                .set(self.queued.load(Ordering::SeqCst) as i64);
            let lag = self
                .dispatched
                .load(Ordering::SeqCst)
                .saturating_sub(self.processed.load(Ordering::SeqCst));
            m.da_gc_lag_heights.set(lag.min(i64::MAX as u64) as i64);
        }
    }
}

/// The GC worker thread and its handoff queue. Dropping it (node shutdown)
/// stops the worker after its current pass and joins it: the jobs still
/// queued are dropped like at a crash (their records are back-filled from
/// the block tree after the restart), and the thread never outlives the
/// stores it writes.
pub(crate) struct DaGcWorker {
    tx: Option<SyncSender<DaGcJob>>,
    shared: Arc<WorkerShared>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for DaGcWorker {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        self.tx = None;
        // A test-paused worker would wait at its gate for good.
        #[cfg(test)]
        self.shared.wake_gate();
        if let Some(thread) = self.thread.take() {
            if thread.join().is_err() {
                tracing::error!("DA body GC worker panicked");
            }
        }
    }
}

impl DaGcWorker {
    /// Spawn the worker over `runner` with a queue of `cap` heights. The
    /// thread logs through the caller's tracing dispatcher.
    pub fn spawn(runner: Arc<DaGcRunner>, cap: usize) -> std::io::Result<Self> {
        let (tx, rx) = std::sync::mpsc::sync_channel(cap.max(1));
        let shared = Arc::new(WorkerShared {
            queued: AtomicUsize::new(0),
            busy: AtomicBool::new(false),
            dispatched: AtomicU64::new(0),
            processed: AtomicU64::new(0),
            dropped_through: AtomicU64::new(0),
            dropped_shards: AtomicU64::new(0),
            dead: AtomicBool::new(false),
            stop: AtomicBool::new(false),
            metrics: runner.metrics.clone(),
            #[cfg(test)]
            hooks: runner.hooks.clone(),
        });
        let dispatch = tracing::dispatcher::get_default(|d| d.clone());
        let thread_shared = shared.clone();
        let thread = std::thread::Builder::new()
            .name("torus-da-gc".into())
            .spawn(move || {
                let _log = tracing::dispatcher::set_default(&dispatch);
                let _exit = ExitGuard {
                    shared: thread_shared.clone(),
                    runner: runner.clone(),
                };
                work(&rx, &runner, &thread_shared);
            })?;
        Ok(Self {
            tx: Some(tx),
            shared,
            thread: Some(thread),
        })
    }

    /// Hand `job` to the worker: never blocks, never touches the DB.
    pub fn submit(&self, job: DaGcJob) -> Handoff {
        let height = job.height;
        let shards = job.shard_count;
        self.shared.dispatched.fetch_max(height, Ordering::SeqCst);
        let Some(tx) = self.tx.as_ref() else {
            return Handoff::Dead;
        };
        if self.shared.dead.load(Ordering::SeqCst) {
            return Handoff::Dead;
        }
        // Counted before the send, so the worker's decrement never runs first.
        self.shared.queued.fetch_add(1, Ordering::SeqCst);
        let handoff = match tx.try_send(job) {
            Ok(()) => Handoff::Queued,
            Err(TrySendError::Full(_)) => {
                self.shared.queued.fetch_sub(1, Ordering::SeqCst);
                self.shared
                    .dropped_shards
                    .store(u64::from(shards), Ordering::SeqCst);
                self.shared
                    .dropped_through
                    .fetch_max(height, Ordering::SeqCst);
                if let Some(ref m) = self.shared.metrics {
                    m.da_gc_coalesced_heights.inc();
                }
                Handoff::Coalesced
            }
            Err(TrySendError::Disconnected(_)) => {
                self.shared.queued.fetch_sub(1, Ordering::SeqCst);
                self.shared.dead.store(true, Ordering::SeqCst);
                Handoff::Dead
            }
        };
        self.shared.update_gauges();
        handoff
    }

    /// Whether the worker has nothing left to do: the queue is empty, no
    /// batch runs and every coalesced height was passed.
    pub fn is_idle(&self) -> bool {
        // The queue first: the worker marks itself busy before it takes a
        // job off the count.
        self.shared.queued.load(Ordering::SeqCst) == 0
            && !self.shared.busy.load(Ordering::SeqCst)
            && self.shared.dropped_through.load(Ordering::SeqCst)
                <= self.shared.processed.load(Ordering::SeqCst)
    }

    /// Whether the worker thread is gone.
    pub fn is_dead(&self) -> bool {
        self.shared.dead.load(Ordering::SeqCst)
    }

    /// Wait until `is_idle` (or the worker died), at most
    /// `timeout`. Tests and tools: the node never waits for the GC.
    pub fn wait_idle(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while !self.is_idle() && !self.is_dead() {
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_micros(200));
        }
        true
    }
}

/// Marks the worker dead when its thread exits; on a panic also turns the
/// fence off (no more deletes, so no more pins are needed).
struct ExitGuard {
    shared: Arc<WorkerShared>,
    runner: Arc<DaGcRunner>,
}

impl Drop for ExitGuard {
    fn drop(&mut self) {
        self.shared.dead.store(true, Ordering::SeqCst);
        self.shared.busy.store(false, Ordering::SeqCst);
        if std::thread::panicking() {
            tracing::error!(
                "DA body GC worker panicked: no more DA bodies are deleted on this node \
                 (restart to resume; bodies are kept, nothing is lost)"
            );
            if self.runner.fenced {
                self.runner.mempool.da_gc_fence().set_enabled(false);
            }
        }
    }
}

/// The worker loop: drain the queue in batches, record every job, run the
/// full pass for the last of each batch, then the coalesced heights.
fn work(rx: &Receiver<DaGcJob>, runner: &DaGcRunner, shared: &WorkerShared) {
    let finish = |height: u64| {
        shared.processed.fetch_max(height, Ordering::SeqCst);
        if runner.fenced {
            let fence = runner.mempool.da_gc_fence();
            fence.release_through(height);
            if let Some(ref m) = runner.metrics {
                m.da_gc_fence_pins.set(fence.pinned() as i64);
            }
        }
        shared.update_gauges();
    };
    while let Ok(first) = rx.recv() {
        shared.busy.store(true, Ordering::SeqCst);
        shared.queued.fetch_sub(1, Ordering::SeqCst);
        let mut jobs = vec![first];
        while let Ok(job) = rx.try_recv() {
            shared.queued.fetch_sub(1, Ordering::SeqCst);
            jobs.push(job);
        }
        shared.update_gauges();
        let last = jobs.len() - 1;
        for (i, job) in jobs.into_iter().enumerate() {
            #[cfg(test)]
            if !runner.hooks.wait_gate() {
                return;
            }
            if shared.stop.load(Ordering::SeqCst) {
                return;
            }
            runner.run(
                job.height,
                job.actions.as_deref(),
                &job.hashes,
                job.record,
                job.shard_count,
                i == last,
            );
            finish(job.height);
        }
        // Heights handed off while the queue was full, once every queued
        // job (all below them, or above them and back-filling them) ran and
        // none passed them yet. A pass back-fills a bounded number of
        // heights: repeat while it says so and nothing new arrived.
        let dropped = shared.dropped_through.load(Ordering::SeqCst);
        if dropped > shared.processed.load(Ordering::SeqCst)
            && shared.queued.load(Ordering::SeqCst) == 0
        {
            let shards = shared.dropped_shards.load(Ordering::SeqCst);
            let shards = u16::try_from(shards).unwrap_or(u16::MAX);
            loop {
                #[cfg(test)]
                if !runner.hooks.wait_gate() {
                    return;
                }
                if shared.stop.load(Ordering::SeqCst) {
                    return;
                }
                let out = runner.run_from_tree(dropped, shards);
                if out.hold_reason != Some(torus_state::DaGcHoldReason::Backfilling)
                    || shared.queued.load(Ordering::SeqCst) > 0
                {
                    break;
                }
            }
            finish(dropped);
        }
        shared.busy.store(false, Ordering::SeqCst);
    }
}

/// Test hooks shared by an app, its runner and its worker.
#[cfg(test)]
#[derive(Default)]
pub(crate) struct DaGcTestHooks {
    /// The tail read sleeps this long (ms) first: a slow GC phase.
    pub tail_delay_ms: AtomicU64,
    /// Runs right before a pass's delete write, after the fence marked the
    /// bodies being deleted.
    #[allow(clippy::type_complexity)]
    pub before_delete: std::sync::Mutex<Option<Box<dyn Fn() + Send>>>,
    /// Passes run, and the threads they ran on.
    pub passes: AtomicU64,
    pub pass_threads: std::sync::Mutex<std::collections::HashSet<std::thread::ThreadId>>,
    gate: std::sync::Mutex<TestGate>,
    gate_cv: std::sync::Condvar,
}

#[cfg(test)]
#[derive(Default)]
struct TestGate {
    paused: bool,
    killed: bool,
    /// Jobs the worker still runs before it pauses itself.
    pause_after: Option<u64>,
}

#[cfg(test)]
impl DaGcTestHooks {
    pub fn tail_read(&self) {
        let ms = self.tail_delay_ms.load(Ordering::SeqCst);
        if ms > 0 {
            std::thread::sleep(Duration::from_millis(ms));
        }
    }

    pub fn before_delete_write(&self) {
        if let Some(f) = self.before_delete.lock().unwrap().as_ref() {
            f();
        }
    }

    fn note_pass(&self) {
        self.passes.fetch_add(1, Ordering::SeqCst);
        self.pass_threads
            .lock()
            .unwrap()
            .insert(std::thread::current().id());
    }

    /// Hold the worker before its next job.
    pub fn pause(&self) {
        self.gate.lock().unwrap().paused = true;
    }

    /// Let the worker run `jobs` more jobs, then pause it.
    pub fn pause_after(&self, jobs: u64) {
        let mut gate = self.gate.lock().unwrap();
        gate.pause_after = Some(jobs);
        gate.paused = false;
        self.gate_cv.notify_all();
    }

    pub fn resume(&self) {
        let mut gate = self.gate.lock().unwrap();
        gate.paused = false;
        gate.pause_after = None;
        self.gate_cv.notify_all();
    }

    /// A crash: the worker exits at its next job, running nothing more.
    pub fn kill(&self) {
        self.gate.lock().unwrap().killed = true;
        self.gate_cv.notify_all();
    }

    /// The worker before each job: wait while paused; `false` = killed.
    fn wait_gate(&self) -> bool {
        let mut gate = self.gate.lock().unwrap();
        loop {
            if gate.killed {
                return false;
            }
            if let Some(0) = gate.pause_after {
                gate.pause_after = None;
                gate.paused = true;
            }
            if !gate.paused {
                if let Some(n) = gate.pause_after.as_mut() {
                    *n -= 1;
                }
                return true;
            }
            gate = self.gate_cv.wait(gate).unwrap();
        }
    }
}
