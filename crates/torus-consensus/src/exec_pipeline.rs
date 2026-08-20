//! bl2 exec pipeline (`TORUS_EXEC_PIPELINE`): the flush worker **W**.
//!
//! Design: `docs/perf/design-exec-pipeline-2026-08-20.md` §2 (candidate 3,
//! `state-write-async-overlay-carry`, which absorbs `flush-root-overlap-1deep`:
//! the root's trie/mirror ops share ONE atomic batch with the native state and
//! the applied-height marker, so the whole `flush_with_native_trie_stats` moves
//! together — root overlap comes for free, the fence is untouched).
//!
//! Threads:
//! * **E** (the exec thread, `execution_loop`) runs verify + engine + save_books
//!   for block N, freezes the overlay into a [`FrozenPending`] and hands it to W.
//!   It then starts N+1 on an overlay whose *parent* is pending(N)
//!   (`NativeStateOverlay::with_parent`) — read-your-writes without waiting for
//!   the RocksDB write.
//! * **W** (this module) flushes jobs strictly in height order: build + root +
//!   marker + ONE atomic write, then the EVM mirror resync for any dirtied
//!   accounts — the same code and the same batch content as the serial path.
//!
//! Depth is exactly one: the channel is a **rendezvous** (`sync_channel(0)`), so
//! `submit(N)` returns only once W has *received* N, and W receives only after
//! finishing N−1. Invariant when E starts engine(N+1): every height < N is
//! durable and the only non-durable set is pending(N) — which is exactly the
//! parent layer of overlay(N+1). The time E spends blocked in `submit` is
//! `torus_exec_handoff_wait_seconds`; W's per-job wall is
//! `torus_flush_worker_seconds`; `torus_flush_worker_depth` counts jobs handed
//! off and not yet durable (0 ⇒ safe to digest).
//!
//! Failure: a write error (or a panic) on W latches the shared `exec_failed`
//! fail-stop AND W's own latch; E checks the latch before every hand-off and in
//! `wait_idle`, so nothing of N+1 is ever written on top of a non-durable N.
//! Restart replays from the durable marker (at most depth + queue blocks).
//!
//! Crash windows and the hazard table live in the design doc (§3); the tests
//! in `app.rs` (`exec_pipeline_*`) pin them.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;

use torus_state::native_trie::{NativeMemberCache, NativeTrieCache};
use torus_state::{FrozenPending, StateDb};
use torus_types::Address;

/// Kill switch: `TORUS_EXEC_PIPELINE=1` enables the flush worker; anything else
/// (INCLUDING UNSET) keeps the serial exec chain — exact-today. Default OFF
/// until two agreeing 3-validator cells + the crash gate exist (design §2.1).
pub fn exec_pipeline_enabled() -> bool {
    parse_exec_pipeline_toggle(std::env::var("TORUS_EXEC_PIPELINE").ok())
}

/// Pure parse of the `TORUS_EXEC_PIPELINE` value: only `"1"` enables.
pub fn parse_exec_pipeline_toggle(v: Option<String>) -> bool {
    matches!(v.as_deref().map(str::trim), Some("1"))
}

/// One unit of work for W, in commit order.
pub enum Job {
    /// A native block: flush its frozen pending set (state + trie + marker in one
    /// atomic batch), then resync the EVM mirror for `evm_addrs` (usually empty).
    Flush {
        height: u64,
        pending: Arc<FrozenPending>,
        evm_addrs: Vec<Address>,
    },
    /// An empty / non-native block: advance the applied-height marker, in order
    /// behind the previous block's batch. `pending` is the 1-key marker layer the
    /// exec thread also layers under the next overlay; W writes exactly that key.
    Marker {
        height: u64,
        pending: Arc<FrozenPending>,
    },
}

impl Job {
    pub fn height(&self) -> u64 {
        match self {
            Job::Flush { height, .. } | Job::Marker { height, .. } => *height,
        }
    }
}

/// Test hook: a gate W passes through before EVERY job. `hold()` parks W (so a
/// test can pin "batch(N) in flight"), `release()` lets it run; `fail_next()`
/// makes W treat the next job's write as a RocksDB error WITHOUT writing
/// (latch path). Production passes `None` — no gate, no cost.
#[derive(Default)]
pub struct WorkerGate {
    held: Mutex<bool>,
    cv: Condvar,
    fail_next: AtomicBool,
    /// Heights W has received (passed the gate) — observability for tests.
    received: Mutex<Vec<u64>>,
}

impl WorkerGate {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn hold(&self) {
        *self.held.lock().unwrap() = true;
    }

    pub fn release(&self) {
        *self.held.lock().unwrap() = false;
        self.cv.notify_all();
    }

    pub fn fail_next(&self) {
        self.fail_next.store(true, Ordering::SeqCst);
    }

    pub fn received(&self) -> Vec<u64> {
        self.received.lock().unwrap().clone()
    }

    fn wait_open(&self, height: u64) -> bool {
        self.received.lock().unwrap().push(height);
        let mut held = self.held.lock().unwrap();
        while *held {
            held = self.cv.wait(held).unwrap();
        }
        self.fail_next.swap(false, Ordering::SeqCst)
    }
}

struct Shared {
    /// Jobs handed off by E (counted BEFORE the rendezvous send) and not yet
    /// finished by W. 0 ⇒ everything E ever submitted is durable.
    outstanding: Mutex<usize>,
    idle: Condvar,
    /// W's failure latch (write error / panic). Never cleared.
    failed: AtomicBool,
    /// Highest height W has made durable (seeded from the marker at spawn).
    durable_height: AtomicU64,
}

/// `submit` error: W has failed (latched) or its thread is gone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkerUnavailable;

/// Handle to the flush worker thread. Dropping it closes the channel, drains
/// every queued job and joins the thread — so a graceful shutdown never loses a
/// handed-off batch (the same ordering `BackgroundCfWriter` uses).
pub struct FlushWorker {
    tx: Option<SyncSender<Job>>,
    handle: Option<JoinHandle<()>>,
    shared: Arc<Shared>,
}

/// Everything W needs to run a job; owned by the worker thread.
pub struct WorkerEnv {
    pub state_db: StateDb,
    /// Shared with E's serial/barrier path (which only touches them after
    /// `wait_idle`, so the mutexes are never contended).
    pub trie_cache: Arc<Mutex<NativeTrieCache>>,
    pub member_cache: Arc<Mutex<NativeMemberCache>>,
    pub metrics: Option<Arc<torus_telemetry::Metrics>>,
    /// The node-wide fail-stop latch (T1.5), shared with E and consensus.
    pub exec_failed: Arc<AtomicBool>,
    pub gate: Option<Arc<WorkerGate>>,
}

impl FlushWorker {
    /// Spawn W. `durable_height_seed` is the durable applied-height marker at
    /// construction (after boot replay — design §2.1 W.6).
    pub fn spawn(env: WorkerEnv, durable_height_seed: u64) -> Self {
        let (tx, rx) = sync_channel::<Job>(0);
        let shared = Arc::new(Shared {
            outstanding: Mutex::new(0),
            idle: Condvar::new(),
            failed: AtomicBool::new(false),
            durable_height: AtomicU64::new(durable_height_seed),
        });
        let shared_w = shared.clone();
        let handle = std::thread::Builder::new()
            .name("torus-flush-worker".into())
            .spawn(move || worker_loop(rx, env, shared_w))
            .expect("spawn flush worker thread");
        Self {
            tx: Some(tx),
            handle: Some(handle),
            shared,
        }
    }

    /// Hand a job to W. BLOCKS until W has received it (rendezvous), i.e. until
    /// W finished the previous job. Returns `Err` if W has failed or is gone —
    /// the caller must latch the fail-stop and must not proceed.
    pub fn submit(&self, job: Job) -> Result<(), WorkerUnavailable> {
        if self.failed() {
            return Err(WorkerUnavailable);
        }
        let Some(tx) = &self.tx else {
            return Err(WorkerUnavailable);
        };
        {
            let mut n = self.shared.outstanding.lock().unwrap();
            *n += 1;
        }
        match tx.send(job) {
            Ok(()) => Ok(()),
            Err(_) => {
                let mut n = self.shared.outstanding.lock().unwrap();
                *n -= 1;
                self.shared.idle.notify_all();
                Err(WorkerUnavailable)
            }
        }
    }

    /// Block until every submitted job is durable (or W has failed). Returns
    /// `true` when W is idle and healthy, `false` if it has failed.
    pub fn wait_idle(&self) -> bool {
        let mut n = self.shared.outstanding.lock().unwrap();
        while *n > 0 && !self.failed() {
            n = self.shared.idle.wait(n).unwrap();
        }
        !self.failed()
    }

    pub fn failed(&self) -> bool {
        self.shared.failed.load(Ordering::SeqCst)
    }

    pub fn durable_height(&self) -> u64 {
        self.shared.durable_height.load(Ordering::SeqCst)
    }

    /// Jobs handed off and not yet durable (what `torus_flush_worker_depth`
    /// reports).
    pub fn outstanding(&self) -> usize {
        *self.shared.outstanding.lock().unwrap()
    }
}

impl Drop for FlushWorker {
    fn drop(&mut self) {
        self.tx.take();
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

fn finish_job(shared: &Shared, metrics: &Option<Arc<torus_telemetry::Metrics>>) {
    let mut n = shared.outstanding.lock().unwrap();
    *n = n.saturating_sub(1);
    if let Some(m) = metrics {
        m.flush_worker_depth.set(*n as i64);
    }
    drop(n);
    shared.idle.notify_all();
}

fn worker_loop(rx: Receiver<Job>, env: WorkerEnv, shared: Arc<Shared>) {
    tracing::info!("flush worker thread started (TORUS_EXEC_PIPELINE)");
    while let Ok(job) = rx.recv() {
        let height = job.height();
        if let Some(m) = &env.metrics {
            m.flush_worker_depth.set(*shared.outstanding.lock().unwrap() as i64);
        }
        let inject_fail = env.gate.as_ref().is_some_and(|g| g.wait_open(height));
        let timer = std::time::Instant::now();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            if inject_fail {
                return Err(JobError::Write("injected write failure (test gate)".into()));
            }
            run_job(&env, &job)
        }));
        let wall = timer.elapsed().as_secs_f64();
        if let Some(m) = &env.metrics {
            m.flush_worker_seconds.observe(wall);
            m.exec_flush_seconds.observe(wall);
        }
        match result {
            Ok(Ok(())) => {
                shared.durable_height.store(height, Ordering::SeqCst);
                finish_job(&shared, &env.metrics);
            }
            Ok(Err(JobError::TrieStale(e))) => {
                // Today's semantics: the batch (state + marker) is durable, only
                // the off-by-default incremental native root is stale for this
                // block; caches were invalidated inside the flush. Keep going.
                tracing::error!(%e, height, "flush worker: native trie maintenance failed (state + marker durable, root stale)");
                shared.durable_height.store(height, Ordering::SeqCst);
                finish_job(&shared, &env.metrics);
            }
            Ok(Err(JobError::Write(e))) => {
                tracing::error!(
                    %e,
                    height,
                    "FATAL: flush worker write failed — block NOT durable while the exec thread may \
                     already have executed its successor on top of it; latching fail-stop (restart \
                     replays from the durable marker)"
                );
                shared.failed.store(true, Ordering::SeqCst);
                env.exec_failed.store(true, Ordering::SeqCst);
                finish_job(&shared, &env.metrics);
                return;
            }
            Err(_) => {
                tracing::error!(height, "FATAL: flush worker PANICKED — latching fail-stop");
                shared.failed.store(true, Ordering::SeqCst);
                env.exec_failed.store(true, Ordering::SeqCst);
                finish_job(&shared, &env.metrics);
                return;
            }
        }
    }
    tracing::info!("flush worker thread shutting down");
}

enum JobError {
    /// The atomic batch did NOT land (RocksDB write error / missing CF).
    Write(String),
    /// The batch landed (marker durable) but trie maintenance failed.
    TrieStale(String),
}

fn run_job(env: &WorkerEnv, job: &Job) -> Result<(), JobError> {
    match job {
        Job::Marker { height, pending } => {
            // Exactly today's `write_native_applied_height` bytes, sequenced
            // behind the previous block's batch by this thread's order. Written
            // through the frozen 1-key set so the bytes are the SAME object the
            // exec thread layered under the next overlay.
            pending
                .flush_with_native_trie_stats(&env.state_db, Some(*height), None, None)
                .map(|_| ())
                .map_err(|e| JobError::Write(e.to_string()))
        }
        Job::Flush {
            height,
            pending,
            evm_addrs,
        } => {
            let flush_result = {
                let mut trie_cache = env
                    .trie_cache
                    .lock()
                    .expect("trie-cache mutex poisoned");
                let cache_opt = if torus_state::native_trie::native_root_cache_enabled() {
                    Some(&mut *trie_cache)
                } else {
                    None
                };
                let mut member_cache = env
                    .member_cache
                    .lock()
                    .expect("member-cache mutex poisoned");
                let member_opt = if member_cache.is_enabled() {
                    Some(&mut *member_cache)
                } else {
                    None
                };
                pending.flush_with_native_trie_stats(
                    &env.state_db,
                    Some(*height),
                    cache_opt,
                    member_opt,
                )
            };
            match flush_result {
                Ok(stats) => {
                    if let Some(m) = &env.metrics {
                        m.exec_root_seconds.observe(stats.root_seconds);
                        m.exec_state_write_seconds.observe(stats.write_seconds);
                        m.exec_state_write_build_seconds
                            .observe(stats.write_build_seconds);
                        m.exec_state_write_db_seconds
                            .observe(stats.write_db_seconds);
                        m.exec_state_write_batch_bytes
                            .observe(stats.batch_bytes as f64);
                        m.exec_root_dirty_buckets.observe(stats.dirty_buckets as f64);
                        m.exec_root_bucket_scans.inc_by(stats.bucket_scans as u64);
                        m.member_cache_hits.inc_by(stats.member_hits as u64);
                        m.member_cache_misses.inc_by(stats.member_misses as u64);
                        m.member_cache_evictions.inc_by(stats.member_evictions as u64);
                        m.member_cache_resident_buckets
                            .set(stats.member_resident_buckets as i64);
                        for (i, n) in stats.dirty_entries_by_cf.iter().enumerate() {
                            m.exec_dirty_entries_by_cf[i].inc_by(*n as u64);
                        }
                    }
                }
                Err(e) => {
                    // `flush_with_native_trie_stats` returns Err both when the
                    // write failed (nothing landed) and when the write landed but
                    // trie maintenance failed. The durable marker tells them apart.
                    let landed = env
                        .state_db
                        .get_cf_raw(
                            torus_state::cf::CF_CONSENSUS_META,
                            torus_state::cf::META_NATIVE_APPLIED_HEIGHT,
                        )
                        .ok()
                        .flatten()
                        .and_then(|b| <[u8; 8]>::try_from(b.as_slice()).ok())
                        .map(u64::from_be_bytes)
                        == Some(*height);
                    return Err(if landed {
                        JobError::TrieStale(e.to_string())
                    } else {
                        JobError::Write(e.to_string())
                    });
                }
            }
            // Phase A: native post-commit credited EVM balances straight to
            // CF_ACCOUNTS; resync the incremental mirror AFTER our own batch is
            // durable (DB->DB, sequenced here — design F2). Best-effort, as today.
            let resync_timer = std::time::Instant::now();
            if !evm_addrs.is_empty() {
                if let Err(e) =
                    torus_state::incremental::resync_evm_accounts(&env.state_db, evm_addrs)
                {
                    tracing::error!(%e, height, "flush worker: failed to resync incremental trie after native post-commit");
                }
            }
            if let Some(m) = &env.metrics {
                m.exec_evm_resync_seconds
                    .observe(resync_timer.elapsed().as_secs_f64());
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toggle_default_off_only_one_enables() {
        assert!(!parse_exec_pipeline_toggle(None));
        for v in ["0", "", "true", "on", "yes", "2"] {
            assert!(!parse_exec_pipeline_toggle(Some(v.to_string())), "{v}");
        }
        assert!(parse_exec_pipeline_toggle(Some("1".to_string())));
        assert!(parse_exec_pipeline_toggle(Some(" 1 ".to_string())));
    }

    fn temp_env(gate: Option<Arc<WorkerGate>>) -> (WorkerEnv, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let state_db = StateDb::open(dir.path()).unwrap();
        (
            WorkerEnv {
                state_db,
                trie_cache: Arc::new(Mutex::new(Default::default())),
                member_cache: Arc::new(Mutex::new(NativeMemberCache::with_budget(0))),
                metrics: None,
                exec_failed: Arc::new(AtomicBool::new(false)),
                gate,
            },
            dir,
        )
    }

    /// Depth bound: with W parked inside job N−1, the FIRST hand-off (N) blocks
    /// (rendezvous, not a 1-slot buffer), `outstanding` never exceeds 2 (one in
    /// flight + one at the rendezvous), and `wait_idle` returns only once both
    /// are durable, in order.
    #[test]
    fn rendezvous_blocks_first_handoff_and_preserves_order() {
        let gate = WorkerGate::new();
        let (env, _dir) = temp_env(Some(gate.clone()));
        let db = env.state_db.clone();
        let worker = Arc::new(FlushWorker::spawn(env, 0));

        gate.hold();
        worker
            .submit(Job::Marker {
                height: 1,
                pending: Arc::new(FrozenPending::marker_only(1)),
            })
            .unwrap();
        // W received 1 and is parked on the gate.
        assert_eq!(gate.received(), vec![1]);
        assert_eq!(worker.outstanding(), 1);

        let w2 = worker.clone();
        let sender = std::thread::spawn(move || {
            w2.submit(Job::Marker {
                height: 2,
                pending: Arc::new(FrozenPending::marker_only(2)),
            })
            .unwrap();
        });
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert!(!sender.is_finished(), "submit(2) must block while W holds job 1");
        assert_eq!(gate.received(), vec![1], "W must not have received 2 yet");
        assert_eq!(worker.outstanding(), 2);
        assert_eq!(worker.durable_height(), 0);

        gate.release();
        sender.join().unwrap();
        assert!(worker.wait_idle());
        assert_eq!(gate.received(), vec![1, 2]);
        assert_eq!(worker.durable_height(), 2);
        assert_eq!(worker.outstanding(), 0);
        let marker = db
            .get_cf_raw(
                torus_state::cf::CF_CONSENSUS_META,
                torus_state::cf::META_NATIVE_APPLIED_HEIGHT,
            )
            .unwrap();
        assert_eq!(marker, Some(2u64.to_be_bytes().to_vec()));
    }

    /// A write error on W latches both W's latch and the shared fail-stop; the
    /// next submit is refused and `wait_idle` reports the failure.
    #[test]
    fn write_error_latches_failstop_and_refuses_further_jobs() {
        let gate = WorkerGate::new();
        let (env, _dir) = temp_env(Some(gate.clone()));
        let exec_failed = env.exec_failed.clone();
        let worker = FlushWorker::spawn(env, 0);
        gate.fail_next();
        worker
            .submit(Job::Marker {
                height: 1,
                pending: Arc::new(FrozenPending::marker_only(1)),
            })
            .unwrap();
        assert!(!worker.wait_idle(), "wait_idle must report the failure");
        assert!(worker.failed());
        assert!(exec_failed.load(Ordering::SeqCst), "shared fail-stop latched");
        assert!(
            worker
                .submit(Job::Marker {
                    height: 2,
                    pending: Arc::new(FrozenPending::marker_only(2)),
                })
                .is_err(),
            "no job may be accepted after the latch"
        );
    }

    /// Drop drains: a job handed off right before drop is durable after drop.
    #[test]
    fn drop_drains_and_joins() {
        let (env, _dir) = temp_env(None);
        let db = env.state_db.clone();
        let worker = FlushWorker::spawn(env, 0);
        worker
            .submit(Job::Marker {
                height: 5,
                pending: Arc::new(FrozenPending::marker_only(5)),
            })
            .unwrap();
        drop(worker);
        let marker = db
            .get_cf_raw(
                torus_state::cf::CF_CONSENSUS_META,
                torus_state::cf::META_NATIVE_APPLIED_HEIGHT,
            )
            .unwrap();
        assert_eq!(marker, Some(5u64.to_be_bytes().to_vec()));
    }
}
