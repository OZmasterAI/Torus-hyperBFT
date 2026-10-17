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

/// Default ON (s64): the flush worker is attached unless `TORUS_EXEC_PIPELINE=0`,
/// the kill switch that restores the serial exec chain. Flipped after the s63
/// agreeing 3-validator cells and the s64 crash gate (design §2.1).
pub fn exec_pipeline_enabled() -> bool {
    parse_exec_pipeline_toggle(std::env::var("TORUS_EXEC_PIPELINE").ok())
}

/// Pure parse of the `TORUS_EXEC_PIPELINE` value: only `"0"` disables.
pub fn parse_exec_pipeline_toggle(v: Option<String>) -> bool {
    !matches!(v.as_deref().map(str::trim), Some("0"))
}

/// s124 epoch-boundary fast path (`TORUS_EPOCH_FAST_PATH`, node-local, default
/// off, read once at startup by `TorusApp::try_new`): an
/// epoch-boundary block with no EVM tx and no buffered slash takes the
/// pipelined path instead of the barrier (forced checkpoint drain + W drain)
/// and the serial flush; with a checkpoint interval it checkpoints at its own
/// height. Same state, roots and running hash; only timing changes. No
/// effect without W (`TORUS_EXEC_PIPELINE=0`).
pub fn epoch_fast_path_enabled() -> bool {
    parse_epoch_fast_path(std::env::var("TORUS_EPOCH_FAST_PATH").ok())
}

/// Pure parse of `TORUS_EPOCH_FAST_PATH`: `1` or `true` = on; unset, empty,
/// `0` or `false` = off; anything else = off with a warning.
pub fn parse_epoch_fast_path(raw: Option<String>) -> bool {
    let Some(raw) = raw else {
        return false;
    };
    match raw.trim().to_ascii_lowercase().as_str() {
        "" | "0" | "false" => false,
        "1" | "true" => true,
        other => {
            tracing::warn!(
                value = other,
                "TORUS_EPOCH_FAST_PATH: not 0/1/true/false — epoch fast path OFF"
            );
            false
        }
    }
}

/// Item 6 Phase 3 step 6: the compiled default interval on validators (owner
/// s109, the gate's 15 s). `--rpc-only` nodes default to 0 (per-block flush).
pub const VALIDATOR_CHECKPOINT_INTERVAL: std::time::Duration = std::time::Duration::from_secs(15);

/// Role default for an unset `TORUS_CHECKPOINT_INTERVAL_MS`, set once by the
/// node binary before [`crate::app::TorusApp::try_new`]. Never set (tests,
/// tools) = `None`: the per-block flush, as before step 6.
static CHECKPOINT_INTERVAL_DEFAULT: std::sync::OnceLock<Option<std::time::Duration>> =
    std::sync::OnceLock::new();

/// The role's default interval: validators [`VALIDATOR_CHECKPOINT_INTERVAL`],
/// `--rpc-only` nodes `None` (their RPC reads must not lag).
pub fn role_checkpoint_interval(rpc_only: bool) -> Option<std::time::Duration> {
    (!rpc_only).then_some(VALIDATOR_CHECKPOINT_INTERVAL)
}

/// Set the role default for an unset `TORUS_CHECKPOINT_INTERVAL_MS` (the node
/// binary: `--rpc-only`). First call wins; returns the interval the
/// environment then selects (before the book-mode check in
/// [`resolve_checkpoint_interval`]).
pub fn set_checkpoint_interval_default(rpc_only: bool) -> Option<std::time::Duration> {
    let _ = CHECKPOINT_INTERVAL_DEFAULT.set(role_checkpoint_interval(rpc_only));
    checkpoint_interval()
}

fn checkpoint_interval_default() -> Option<std::time::Duration> {
    CHECKPOINT_INTERVAL_DEFAULT.get().copied().flatten()
}

/// Item 6 Phase 3: coalesced state checkpoints. `TORUS_CHECKPOINT_INTERVAL_MS`
/// is the node-local interval of execution time between checkpoints; `0`
/// keeps today's per-block flush byte for byte; unset or empty is the role
/// default (step 6: 15 s on validators, 0 on `--rpc-only`; 0 when no role
/// default was set).
pub fn checkpoint_interval() -> Option<std::time::Duration> {
    parse_checkpoint_interval(
        std::env::var("TORUS_CHECKPOINT_INTERVAL_MS").ok(),
        checkpoint_interval_default(),
    )
}

/// True when `TORUS_CHECKPOINT_INTERVAL_MS` is set to a non-empty value (the
/// operator chose the interval; the role default did not).
fn checkpoint_interval_env_set(v: Option<&str>) -> bool {
    v.is_some_and(|s| !s.trim().is_empty())
}

/// The interval the node runs, from the env value, the role default and the
/// book mode. An explicit interval > 0 with a deferred book mode (2/3) refuses
/// to start ([`check_checkpoint_book_mode`], task 1.2). The role default
/// yields to a deferred book mode instead (per-block flush, `Ok(None)`): the
/// operator chose the book mode, not the interval (e.g. the bench
/// `RECORD_ENV`'s `TORUS_BOOK_ROWS=3`).
pub fn resolve_checkpoint_interval(
    raw: Option<String>,
    default: Option<std::time::Duration>,
    mode: torus_bridge::native_executor::BookMode,
) -> Result<Option<std::time::Duration>, String> {
    let explicit = checkpoint_interval_env_set(raw.as_deref());
    let interval = parse_checkpoint_interval(raw, default);
    if explicit {
        check_checkpoint_book_mode(interval, mode)?;
        return Ok(interval);
    }
    match check_checkpoint_book_mode(interval, mode) {
        Ok(()) => Ok(interval),
        Err(_) => {
            tracing::warn!(
                ?mode,
                "TORUS_BOOK_ROWS selects a deferred book save: the compiled default checkpoint \
                 interval is off (per-block flush); set TORUS_BOOK_ROWS unset/0/1 for checkpoints"
            );
            Ok(None)
        }
    }
}

/// [`resolve_checkpoint_interval`] from the environment and the role default.
pub fn effective_checkpoint_interval(
    mode: torus_bridge::native_executor::BookMode,
) -> Result<Option<std::time::Duration>, String> {
    resolve_checkpoint_interval(
        std::env::var("TORUS_CHECKPOINT_INTERVAL_MS").ok(),
        checkpoint_interval_default(),
        mode,
    )
}

/// True when the operator set `TORUS_CHECKPOINT_INTERVAL_MS` (not the role
/// default).
pub fn checkpoint_interval_explicit() -> bool {
    checkpoint_interval_env_set(std::env::var("TORUS_CHECKPOINT_INTERVAL_MS").ok().as_deref())
}

/// `TORUS_CHECKPOINT_MAX_BYTES`: cap on the since-checkpoint layer's bytes.
pub fn checkpoint_max_bytes() -> Option<u64> {
    parse_checkpoint_max_bytes(std::env::var("TORUS_CHECKPOINT_MAX_BYTES").ok())
}

/// Pure parse of `TORUS_CHECKPOINT_INTERVAL_MS` (milliseconds, trimmed). Unset
/// or empty -> `default` (the role default, step 6); `0` -> `None` (per-block
/// flush, exactly as before Phase 3); anything that is not a plain unsigned
/// integer -> `None` with a warning (fail safe: the per-block path).
pub fn parse_checkpoint_interval(
    v: Option<String>,
    default: Option<std::time::Duration>,
) -> Option<std::time::Duration> {
    let Some(raw) = v.filter(|s| !s.trim().is_empty()) else {
        return default;
    };
    match raw.trim().parse::<u64>() {
        Ok(0) => None,
        Ok(ms) => Some(std::time::Duration::from_millis(ms)),
        Err(_) => {
            tracing::warn!(
                value = %raw,
                "TORUS_CHECKPOINT_INTERVAL_MS is not a whole number of milliseconds; \
                 ignored (checkpoints off, per-block flush)"
            );
            None
        }
    }
}

/// Pure parse of `TORUS_CHECKPOINT_MAX_BYTES` (bytes, trimmed). Unset, `0` or
/// garbage (warned) -> `None` (no byte cap from the setting).
pub fn parse_checkpoint_max_bytes(v: Option<String>) -> Option<u64> {
    let raw = v?;
    match raw.trim().parse::<u64>() {
        Ok(0) => None,
        Ok(b) => Some(b),
        Err(_) => {
            tracing::warn!(
                value = %raw,
                "TORUS_CHECKPOINT_MAX_BYTES is not a whole number of bytes; ignored"
            );
            None
        }
    }
}

/// Item 6 Phase 3: when E closes the since-checkpoint layer into a
/// checkpoint (checked at the end of every pipelined block). Attest heights
/// (`h % 100 == 0`), barriers and shutdown always checkpoint, and so does a
/// pipelined epoch boundary under the epoch fast path
/// ([`epoch_fast_path_enabled`]); these are the node-local extra triggers.
/// `CheckpointPolicy::default()` = only those ("never by time").
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CheckpointPolicy {
    /// Wall time on E since the last checkpoint (`TORUS_CHECKPOINT_INTERVAL_MS`).
    pub interval: Option<std::time::Duration>,
    /// Since-checkpoint layer bytes (`TORUS_CHECKPOINT_MAX_BYTES`).
    pub max_bytes: Option<u64>,
    /// Every n blocks since the last checkpoint (tests).
    pub every_blocks: Option<u64>,
}

impl CheckpointPolicy {
    /// The node's policy for `interval` (the resolved
    /// [`effective_checkpoint_interval`]), with the byte cap from the
    /// environment: `None` (interval off) = today's per-block flush.
    pub fn for_interval(interval: Option<std::time::Duration>) -> Option<Self> {
        interval.map(|interval| Self {
            interval: Some(interval),
            max_bytes: checkpoint_max_bytes(),
            every_blocks: None,
        })
    }
}

/// Interval > 0 requires a non-deferred book mode (owner s109, plan 6.1): in
/// modes 2/3 W applies the deferred book save reading the DB as "every height
/// < N durable", which is false with a since-checkpoint layer. The node
/// refuses to start; the error names both settings.
pub fn check_checkpoint_book_mode(
    interval: Option<std::time::Duration>,
    mode: torus_bridge::native_executor::BookMode,
) -> Result<(), String> {
    use torus_bridge::native_executor::BookMode;
    match (interval, mode) {
        (Some(iv), BookMode::LevelAuthority | BookMode::LevelAuthorityChunked) => Err(format!(
            "TORUS_CHECKPOINT_INTERVAL_MS={} needs a non-deferred book mode, but \
             TORUS_BOOK_ROWS selects {:?} (deferred book save); unset \
             TORUS_CHECKPOINT_INTERVAL_MS or use TORUS_BOOK_ROWS unset/0 (Classic) or 1 (OrderRows)",
            iv.as_millis(),
            mode
        )),
        _ => Ok(()),
    }
}

/// One unit of work for W, in commit order.
pub enum Job {
    /// A native block: flush its frozen pending set (state + trie + marker in one
    /// atomic batch), then resync the EVM mirror for `evm_addrs` (usually empty).
    /// `books` optionally carries the block's DEFERRED mode-2/3 book-save pass 2
    /// (`save_order_books_deferred` on E; s63 port of item 6a): W applies it
    /// into a sidecar pending set whose entries join the SAME atomic batch and
    /// the SAME native-root inputs as `pending` — byte-identical durable state
    /// to the serial on-E save, with the write half of save_books off E's
    /// critical path. `None` = exactly the pre-port job.
    Flush {
        height: u64,
        pending: Arc<FrozenPending>,
        evm_addrs: Vec<Address>,
        books: Option<torus_bridge::native_executor::DeferredBookSave>,
        /// s77 order latency: the block's action nonces, observed at `durable`
        /// once the job's batch is written.
        order_nonces: Vec<u64>,
        /// v2 action status: the block's executed/skipped/failed record, if
        /// it has one. W encodes it into the sidecar (`CF_BLOCK_ACTION_STATUS`,
        /// not hashed, not a root CF) so it joins the block's atomic batch.
        action_status: Option<torus_state::action_status::BlockActionStatus>,
    },
    /// An empty / non-native block: advance the applied-height marker, in order
    /// behind the previous block's batch. `pending` is the 1-key marker layer the
    /// exec thread also layers under the next overlay; W writes exactly that key.
    Marker {
        height: u64,
        pending: Arc<FrozenPending>,
    },
    /// Item 6 Phase 3 (checkpoint interval set): a block whose state rows
    /// are NOT written now. W computes `h_height` from its in-memory
    /// `h(height-1)` over exactly the layers a `Flush` would digest, and
    /// keeps the block's running-hash rows, action-status row, EVM accounts
    /// and order nonces for the next [`Job::Checkpoint`]. Nothing is written
    /// and `durable_height` does not move.
    Digest {
        height: u64,
        pending: Arc<FrozenPending>,
        evm_addrs: Vec<Address>,
        order_nonces: Vec<u64>,
        action_status: Option<torus_state::action_status::BlockActionStatus>,
    },
    /// Item 6 Phase 3: write the since-checkpoint layer `layer` (blocks
    /// S'+1..=height, last value per key) in ONE atomic batch with the
    /// marker = `height`, the running-hash rows kept since the last
    /// checkpoint and the kept action-status rows; then resync the EVM
    /// mirror for the kept accounts. At most one checkpoint is in flight
    /// ([`FlushWorker::submit`] waits for the previous one).
    Checkpoint {
        height: u64,
        layer: Arc<torus_state::CheckpointLayer>,
    },
}

impl Job {
    pub fn height(&self) -> u64 {
        match self {
            Job::Flush { height, .. }
            | Job::Marker { height, .. }
            | Job::Digest { height, .. }
            | Job::Checkpoint { height, .. } => *height,
        }
    }
}

/// Item 6 Phase 3: W's queue depth with a checkpoint interval set
/// (**estimate**, plan task 1.5): digest jobs are small and E must not wait
/// for a checkpoint write, so the rendezvous becomes a bounded buffer.
pub const DIGEST_QUEUE: usize = 128;

/// Test hook: a gate W passes through before EVERY job. `hold()` parks W (so a
/// test can pin "batch(N) in flight"), `release()` lets it run; `fail_next()`
/// makes W treat the next job's write as a RocksDB error WITHOUT writing
/// (latch path). Production passes `None` — no gate, no cost.
#[derive(Default)]
pub struct WorkerGate {
    held: Mutex<bool>,
    /// Item 6 Phase 3: park W on checkpoint jobs only (digests pass).
    hold_ckpt: AtomicBool,
    /// The checkpoint height W is parked on (`hold_checkpoints`).
    parked_ckpt: Mutex<Option<u64>>,
    cv: Condvar,
    fail_next: AtomicBool,
    /// Heights W has received (passed the gate) — observability for tests.
    received: Mutex<Vec<u64>>,
    /// s64 seam: park W inside its failure latch, between the two stores
    /// (see [`latch_failstop`]), until `release`.
    #[cfg(test)]
    hold_latch: AtomicBool,
    /// W is parked at the latch seam.
    #[cfg(test)]
    latch_parked: AtomicBool,
}

impl WorkerGate {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn hold(&self) {
        *self.held.lock().unwrap() = true;
    }

    pub fn release(&self) {
        let mut held = self.held.lock().unwrap();
        *held = false;
        self.hold_ckpt.store(false, Ordering::SeqCst);
        #[cfg(test)]
        self.hold_latch.store(false, Ordering::SeqCst);
        drop(held);
        self.cv.notify_all();
    }

    /// Item 6 Phase 3: park W on the next checkpoint job (its batch "in
    /// flight": received, not written) until `release`; digest jobs pass.
    pub fn hold_checkpoints(&self) {
        let _held = self.held.lock().unwrap();
        *self.parked_ckpt.lock().unwrap() = None;
        self.hold_ckpt.store(true, Ordering::SeqCst);
    }

    /// The checkpoint height W is parked on, if any.
    pub fn parked_checkpoint(&self) -> Option<u64> {
        *self.parked_ckpt.lock().unwrap()
    }

    pub fn fail_next(&self) {
        self.fail_next.store(true, Ordering::SeqCst);
    }

    /// s64 seam: park W between the two stores of its failure latch until
    /// `release`.
    #[cfg(test)]
    pub fn hold_latch(&self) {
        let _held = self.held.lock().unwrap();
        self.hold_latch.store(true, Ordering::SeqCst);
    }

    /// W is parked at the latch seam (`hold_latch`).
    #[cfg(test)]
    pub fn latch_parked(&self) -> bool {
        self.latch_parked.load(Ordering::SeqCst)
    }

    #[cfg(test)]
    fn latch_seam(&self) {
        let mut held = self.held.lock().unwrap();
        while self.hold_latch.load(Ordering::SeqCst) {
            self.latch_parked.store(true, Ordering::SeqCst);
            held = self.cv.wait(held).unwrap();
        }
    }

    pub fn received(&self) -> Vec<u64> {
        self.received.lock().unwrap().clone()
    }

    /// Block (≤ 10 s) until W has recorded receipt of `height`. The rendezvous
    /// `submit` returns as soon as the value is handed over, a hair BEFORE W's
    /// thread records it here — tests must wait for the record, not assume it.
    pub fn wait_received(&self, height: u64) -> bool {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while std::time::Instant::now() < deadline {
            if self.received.lock().unwrap().contains(&height) {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        false
    }

    fn wait_open(&self, height: u64, checkpoint: bool) -> bool {
        self.received.lock().unwrap().push(height);
        let mut held = self.held.lock().unwrap();
        loop {
            let ckpt_held = checkpoint && self.hold_ckpt.load(Ordering::SeqCst);
            if !*held && !ckpt_held {
                break;
            }
            if ckpt_held {
                *self.parked_ckpt.lock().unwrap() = Some(height);
            }
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
    /// Item 6 Phase 3 (task 1.3): the running-hash chain state W digests
    /// from — `h(n-1)` in memory instead of a META read per block. Seeded
    /// from META at spawn; reloaded from META after a serial barrier
    /// (`chain_stale`), when E flushed blocks itself.
    chain: Mutex<torus_state::running_hash::ChainMemo>,
    /// Set by E at a barrier (W idle): the serial path is about to write
    /// META directly, so W reloads `chain` before its next job.
    chain_stale: AtomicBool,
    /// Item 6 Phase 3: a checkpoint is handed off and not finished (at most
    /// one in flight); `ckpt_done` wakes a hand-off waiting on it.
    ckpt_inflight: Mutex<bool>,
    ckpt_done: Condvar,
    /// Checkpoint hand-offs that had to wait for the previous one.
    ckpt_waits: AtomicU64,
}

impl Shared {
    /// W's chain state for the next job (reloaded from META if a serial
    /// barrier ran since the last job). Only called with W idle or on W.
    fn chain_for_next(
        &self,
        db: &StateDb,
    ) -> std::sync::MutexGuard<'_, torus_state::running_hash::ChainMemo> {
        let mut memo = self.chain.lock().unwrap();
        if self.chain_stale.swap(false, Ordering::SeqCst) {
            *memo = torus_state::running_hash::ChainMemo::load(db);
        }
        memo
    }
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
    /// Test gate (None in production); released on drop so a failing test
    /// that leaves W parked fails instead of hanging the join.
    gate: Option<Arc<WorkerGate>>,
    /// The worker's DB (chain-state reload in [`Self::running_hash_prev`]).
    db: StateDb,
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
        Self::spawn_with_queue(env, durable_height_seed, 0)
    }

    /// Item 6 Phase 3: W for a checkpoint interval (queue [`DIGEST_QUEUE`]).
    pub fn spawn_checkpointing(env: WorkerEnv, durable_height_seed: u64) -> Self {
        Self::spawn_with_queue(env, durable_height_seed, DIGEST_QUEUE)
    }

    /// `queue` 0 = the bl2 rendezvous (exactly today).
    fn spawn_with_queue(env: WorkerEnv, durable_height_seed: u64, queue: usize) -> Self {
        let (tx, rx) = sync_channel::<Job>(queue);
        let db = env.state_db.clone();
        let shared = Arc::new(Shared {
            outstanding: Mutex::new(0),
            idle: Condvar::new(),
            failed: AtomicBool::new(false),
            durable_height: AtomicU64::new(durable_height_seed),
            chain: Mutex::new(torus_state::running_hash::ChainMemo::load(&db)),
            chain_stale: AtomicBool::new(false),
            ckpt_inflight: Mutex::new(false),
            ckpt_done: Condvar::new(),
            ckpt_waits: AtomicU64::new(0),
        });
        let shared_w = shared.clone();
        let gate = env.gate.clone();
        let handle = std::thread::Builder::new()
            .name("torus-flush-worker".into())
            .spawn(move || worker_loop(rx, env, shared_w))
            .expect("spawn flush worker thread");
        Self {
            tx: Some(tx),
            handle: Some(handle),
            shared,
            gate,
            db,
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
        // Item 6 Phase 3: at most one checkpoint in flight. A second one
        // waits (counted) until W finished the first, or W failed.
        let ckpt = matches!(job, Job::Checkpoint { .. });
        if ckpt {
            let mut busy = self.shared.ckpt_inflight.lock().unwrap();
            if *busy {
                self.shared.ckpt_waits.fetch_add(1, Ordering::SeqCst);
            }
            while *busy && !self.failed() {
                busy = self.shared.ckpt_done.wait(busy).unwrap();
            }
            if self.failed() {
                return Err(WorkerUnavailable);
            }
            *busy = true;
        }
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
                if ckpt {
                    release_checkpoint(&self.shared);
                }
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

    /// Item 6 Phase 3: W's in-memory `(height, h_height)` of the last hashed
    /// block (what the next digest chains from).
    pub fn running_hash_prev(&self) -> Option<(u64, [u8; 32])> {
        self.shared.chain_for_next(&self.db).prev()
    }

    /// Called by E at a barrier, after `wait_idle`: the serial path writes
    /// META itself from here, so W reloads its chain state before its next
    /// job.
    pub fn mark_chain_stale(&self) {
        self.shared.chain_stale.store(true, Ordering::SeqCst);
    }

    /// Jobs handed off and not yet durable (what `torus_flush_worker_depth`
    /// reports).
    pub fn outstanding(&self) -> usize {
        *self.shared.outstanding.lock().unwrap()
    }

    /// Item 6 Phase 3: hand-offs of a checkpoint that waited for the
    /// previous one to become durable.
    pub fn checkpoint_waits(&self) -> u64 {
        self.shared.ckpt_waits.load(Ordering::SeqCst)
    }
}

impl Drop for FlushWorker {
    fn drop(&mut self) {
        self.tx.take();
        if let Some(g) = &self.gate {
            g.release();
        }
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

/// Item 6 Phase 3: the checkpoint in flight is finished (or never reached
/// W); wake a hand-off waiting for it.
fn release_checkpoint(shared: &Shared) {
    *shared.ckpt_inflight.lock().unwrap() = false;
    shared.ckpt_done.notify_all();
}

/// Item 6 Phase 3: what W keeps from the digested blocks since the last
/// checkpoint, for the checkpoint's batch and its after-write work.
#[derive(Default)]
struct Window {
    /// Running-hash META rows of the digested heights (last value per key).
    rows: torus_state::running_hash::ChainRows,
    /// Rows W itself produced for the window (action status).
    side: torus_state::CheckpointLayer,
    /// EVM accounts the window's native post-commit credited (resync after
    /// the checkpoint is durable).
    evm_addrs: std::collections::BTreeSet<Address>,
    /// Order nonces observed `Durable` once the checkpoint lands.
    order_nonces: Vec<u64>,
    /// Last digested height.
    last: Option<u64>,
}

fn worker_loop(rx: Receiver<Job>, env: WorkerEnv, shared: Arc<Shared>) {
    tracing::info!("flush worker thread started (TORUS_EXEC_PIPELINE)");
    let mut window = Window::default();
    while let Ok(mut job) = rx.recv() {
        let height = job.height();
        let ckpt = matches!(job, Job::Checkpoint { .. });
        let digest = matches!(job, Job::Digest { .. });
        if let Some(m) = &env.metrics {
            m.flush_worker_depth.set(*shared.outstanding.lock().unwrap() as i64);
        }
        let inject_fail = env.gate.as_ref().is_some_and(|g| g.wait_open(height, ckpt));
        let timer = std::time::Instant::now();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            if inject_fail {
                return Err(JobError::Write("injected write failure (test gate)".into()));
            }
            // R01b: the shared fail-stop may be set outside W (DA latch, exec
            // thread). Once it is, nothing more is written — not even a block
            // handed off before it; restart replays from the durable marker.
            if env.exec_failed.load(Ordering::SeqCst) {
                return Err(JobError::Write("fail-stop latched — refusing to write".into()));
            }
            run_job(&env, &shared, &mut window, &mut job)
        }));
        let wall = timer.elapsed().as_secs_f64();
        if let Some(m) = &env.metrics {
            m.flush_worker_seconds.observe(wall);
            // `exec_flush_seconds` keeps its serial meaning (one observation per
            // NATIVE block) so the per-phase table's `flush` column stays
            // comparable; Marker jobs are not native blocks.
            if matches!(job, Job::Flush { .. }) {
                m.exec_flush_seconds.observe(wall);
            }
            if ckpt {
                m.flush_worker_checkpoint_seconds.observe(wall);
            }
        }
        if let (Ok(Ok(()) | Err(JobError::TrieStale(_))), Some(m), Job::Flush { order_nonces, .. }) =
            (&result, &env.metrics, &job)
        {
            m.observe_order_ages(torus_telemetry::OrderStage::Durable, order_nonces.iter().copied());
        }
        match result {
            Ok(Ok(())) => {
                // A digest makes nothing durable.
                if !digest {
                    shared.durable_height.store(height, Ordering::SeqCst);
                }
                if ckpt {
                    release_checkpoint(&shared);
                }
                finish_job(&shared, &env.metrics);
            }
            Ok(Err(JobError::TrieStale(e))) => {
                // Today's semantics: the batch (state + marker) is durable, only
                // the off-by-default incremental native root is stale for this
                // block; caches were invalidated inside the flush. Keep going.
                tracing::error!(%e, height, "flush worker: native trie maintenance failed (state + marker durable, root stale)");
                shared.durable_height.store(height, Ordering::SeqCst);
                if ckpt {
                    release_checkpoint(&shared);
                }
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
                latch_failstop(&env, &shared);
                release_checkpoint(&shared);
                finish_job(&shared, &env.metrics);
                return;
            }
            Err(_) => {
                tracing::error!(height, "FATAL: flush worker PANICKED — latching fail-stop");
                latch_failstop(&env, &shared);
                release_checkpoint(&shared);
                finish_job(&shared, &env.metrics);
                return;
            }
        }
    }
    tracing::info!("flush worker thread shutting down");
}

/// W's failure latch (write error or panic). Order matters: the shared
/// `exec_failed` fail-stop FIRST, then W's own latch, so anyone who sees
/// `failed()` (`wait_idle`, `submit`) also sees `exec_failed` (s64: the
/// reverse order let `wait_idle` return with `exec_failed` still false when
/// W was preempted between the stores). The flags stay separate: E's paths
/// latch `exec_failed` themselves; `failed` is W's own state.
fn latch_failstop(env: &WorkerEnv, shared: &Shared) {
    env.exec_failed.store(true, Ordering::SeqCst);
    #[cfg(test)]
    if let Some(g) = &env.gate {
        g.latch_seam();
    }
    shared.failed.store(true, Ordering::SeqCst);
}

enum JobError {
    /// The atomic batch did NOT land (RocksDB write error / missing CF).
    Write(String),
    /// The batch landed (marker durable) but trie maintenance failed.
    TrieStale(String),
}

fn run_job(
    env: &WorkerEnv,
    shared: &Shared,
    window: &mut Window,
    job: &mut Job,
) -> Result<(), JobError> {
    let mut chain = shared.chain_for_next(&env.state_db);
    match job {
        Job::Marker { height, pending } => {
            // Exactly today's `write_native_applied_height` bytes, sequenced
            // behind the previous block's batch by this thread's order. Written
            // through the frozen 1-key set so the bytes are the SAME object the
            // exec thread layered under the next overlay.
            pending
                .flush_on_worker(
                    None,
                    &env.state_db,
                    Some(*height),
                    None,
                    None,
                    Some(&mut chain),
                )
                .map(|_| ())
                .map_err(|e| JobError::Write(e.to_string()))
        }
        Job::Digest {
            height,
            pending,
            evm_addrs,
            order_nonces,
            action_status,
        } => {
            // The window is contiguous: E checkpoints before any serial
            // block, so a gap here is a pipeline bug — never chain over it.
            if let Some(last) = window.last {
                if last + 1 != *height {
                    return Err(JobError::Write(format!(
                        "digest {height} does not follow the digested window (last {last})"
                    )));
                }
            }
            // v2 action status: encoded here as on a Flush job, kept in
            // the window's side rows (and digested as the Flush sidecar).
            let sidecar = action_status.take().map(|status| {
                let overlay = torus_state::NativeStateOverlay::new(env.state_db.clone());
                let bytes = crate::action_results::encode_status(&status, env.metrics.as_deref());
                if let Err(e) = torus_state::StateBackend::put_cf_raw(
                    &overlay,
                    torus_state::cf::CF_BLOCK_ACTION_STATUS,
                    &height.to_be_bytes(),
                    &bytes,
                ) {
                    tracing::error!(%e, height, "flush worker: failed to stage the action status record");
                }
                overlay.freeze(*height)
            });
            let (entries, secs) = pending.digest_on_worker(
                sidecar.as_deref(),
                &env.state_db,
                &mut chain,
                &mut window.rows,
            );
            if let Some(m) = &env.metrics {
                m.state_hash_seconds.observe(secs);
                m.state_hash_entries.inc_by(entries as u64);
            }
            if let Some(side) = &sidecar {
                window.side.merge(side);
            }
            window.evm_addrs.extend(evm_addrs.drain(..));
            window.order_nonces.append(order_nonces);
            window.last = Some(*height);
            Ok(())
        }
        Job::Checkpoint { height, layer } => {
            if window.last != Some(*height) || layer.top_height() != Some(*height) {
                return Err(JobError::Write(format!(
                    "checkpoint {height} does not close the digested window (last digest {:?}, layer top {:?})",
                    window.last,
                    layer.top_height()
                )));
            }
            let flush_result = {
                let mut trie_cache = env.trie_cache.lock().expect("trie-cache mutex poisoned");
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
                layer.flush_checkpoint(
                    (!window.side.is_empty()).then_some(&window.side),
                    &window.rows,
                    &env.state_db,
                    *height,
                    cache_opt,
                    member_opt,
                )
            };
            let landed = match flush_result {
                Ok(stats) => {
                    observe_flush_stats(env, &stats);
                    observe_checkpoint_write(env, *height, &stats);
                    Ok(())
                }
                Err(e) if marker_is(env, *height) => Err(JobError::TrieStale(e.to_string())),
                Err(e) => return Err(JobError::Write(e.to_string())),
            };
            // The window is durable: start the next one.
            let done = std::mem::take(window);
            if let Some(m) = &env.metrics {
                m.observe_order_ages(
                    torus_telemetry::OrderStage::Durable,
                    done.order_nonces.iter().copied(),
                );
            }
            resync_evm(env, *height, done.evm_addrs.into_iter().collect());
            landed
        }
        Job::Flush {
            height,
            pending,
            evm_addrs,
            books,
            action_status,
            ..
        } => {
            // Deferred book save: apply pass 2 (if any) into a sidecar overlay
            // FIRST — its reads (stop-row diff, meta compare) see heights <
            // `height` durable (W flushes strictly in order) plus this job's own
            // earlier markets, exactly the view E's serial pass 2 had — then fold
            // the sidecar into the flush below so the book bytes share the ONE
            // atomic batch (and the root computation) with state + marker. The
            // frozen `pending` is never mutated. The payload is TAKEN so the
            // drained row bytes move into the sidecar (owned puts, as on E).
            // `exec_save_books_write_seconds` keeps one observation per native
            // block: E observes it on the serial save, W observes it here.
            // v2 action status: encoded here, off the exec thread, and put in
            // the same sidecar (a non-hashed, non-root CF: no root input).
            let sidecar = if books.is_some() || action_status.is_some() {
                let overlay = torus_state::NativeStateOverlay::new(env.state_db.clone());
                if let Some(b) = books.take() {
                    let t = std::time::Instant::now();
                    // R02 branch 7: a failed book write fails the job (W
                    // latches the fail-stop; nothing of the block lands).
                    torus_bridge::native_executor::apply_deferred_book_save(
                        &overlay,
                        env.metrics.as_deref(),
                        b,
                    )
                    .map_err(|e| JobError::Write(format!("deferred book save: {e}")))?;
                    if let Some(m) = &env.metrics {
                        m.exec_save_books_write_seconds
                            .observe(t.elapsed().as_secs_f64());
                    }
                }
                if let Some(status) = action_status.take() {
                    let bytes =
                        crate::action_results::encode_status(&status, env.metrics.as_deref());
                    if let Err(e) = torus_state::StateBackend::put_cf_raw(
                        &overlay,
                        torus_state::cf::CF_BLOCK_ACTION_STATUS,
                        &height.to_be_bytes(),
                        &bytes,
                    ) {
                        tracing::error!(%e, height, "flush worker: failed to stage the action status record");
                    }
                }
                Some(overlay.freeze(*height))
            } else {
                None
            };
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
                pending.flush_on_worker(
                    sidecar.as_deref(),
                    &env.state_db,
                    Some(*height),
                    cache_opt,
                    member_opt,
                    Some(&mut chain),
                )
            };
            match flush_result {
                Ok(stats) => observe_flush_stats(env, &stats),
                Err(e) => {
                    // `flush_with_native_trie_stats` returns Err both when the
                    // write failed (nothing landed) and when the write landed but
                    // trie maintenance failed. The durable marker tells them apart.
                    return Err(if marker_is(env, *height) {
                        JobError::TrieStale(e.to_string())
                    } else {
                        JobError::Write(e.to_string())
                    });
                }
            }
            resync_evm(env, *height, std::mem::take(evm_addrs));
            Ok(())
        }
    }
}

/// The durable applied marker equals `height` (a flush that returned Err
/// landed: only trie maintenance failed).
fn marker_is(env: &WorkerEnv, height: u64) -> bool {
    env.state_db
        .get_cf_raw(
            torus_state::cf::CF_CONSENSUS_META,
            torus_state::cf::META_NATIVE_APPLIED_HEIGHT,
        )
        .ok()
        .flatten()
        .and_then(|b| <[u8; 8]>::try_from(b.as_slice()).ok())
        .map(u64::from_be_bytes)
        == Some(height)
}

/// Phase A: native post-commit credited EVM balances straight to
/// CF_ACCOUNTS; resync the incremental mirror AFTER the batch is durable
/// (DB->DB, sequenced here — design F2). Best-effort, as today.
fn resync_evm(env: &WorkerEnv, height: u64, evm_addrs: Vec<Address>) {
    let resync_timer = std::time::Instant::now();
    if !evm_addrs.is_empty() {
        if let Err(e) = torus_state::incremental::resync_evm_accounts(&env.state_db, &evm_addrs) {
            tracing::error!(%e, height, "flush worker: failed to resync incremental trie after native post-commit");
        }
    }
    if let Some(m) = &env.metrics {
        m.exec_evm_resync_seconds
            .observe(resync_timer.elapsed().as_secs_f64());
    }
}

/// Q6 step 0: the checkpoint DB write's RocksDB stage split (histograms),
/// its start / end wall clock (gauges) and one info line per checkpoint, so
/// windows can be cut to the DB write itself even when two checkpoints fall
/// between two scrapes.
fn observe_checkpoint_write(env: &WorkerEnv, height: u64, stats: &torus_state::NativeFlushStats) {
    let Some(s) = stats.checkpoint_write else {
        return;
    };
    if let Some(m) = &env.metrics {
        m.exec_checkpoint_db_write_seconds.observe(s.wall_seconds);
        m.exec_checkpoint_db_wal_seconds.observe(s.wal_seconds);
        m.exec_checkpoint_db_memtable_seconds
            .observe(s.memtable_seconds);
        m.exec_checkpoint_db_delay_seconds.observe(s.delay_seconds);
        m.exec_checkpoint_db_pre_post_seconds
            .observe(s.pre_post_seconds);
        m.exec_checkpoint_db_mutex_seconds.observe(s.mutex_seconds);
        m.exec_checkpoint_db_cond_wait_seconds
            .observe(s.cond_wait_seconds);
        m.exec_checkpoint_db_residual_seconds
            .observe(s.residual_seconds());
        m.exec_checkpoint_db_write_start_ms
            .set(i64::try_from(s.start_unix_ms).unwrap_or(i64::MAX));
        m.exec_checkpoint_db_write_end_ms
            .set(i64::try_from(s.end_unix_ms).unwrap_or(i64::MAX));
    }
    let ms = |x: f64| (x * 1e4).round() / 10.0;
    tracing::info!(
        height,
        start_ms = s.start_unix_ms,
        end_ms = s.end_unix_ms,
        batch_bytes = stats.batch_bytes,
        write_ms = ms(s.wall_seconds),
        wal_ms = ms(s.wal_seconds),
        memtable_ms = ms(s.memtable_seconds),
        delay_ms = ms(s.delay_seconds),
        pre_post_ms = ms(s.pre_post_seconds),
        residual_ms = ms(s.residual_seconds()),
        mutex_ms = ms(s.mutex_seconds),
        cond_wait_ms = ms(s.cond_wait_seconds),
        "checkpoint db write"
    );
}

fn observe_flush_stats(env: &WorkerEnv, stats: &torus_state::NativeFlushStats) {
    if let Some(m) = &env.metrics {
        m.exec_root_seconds.observe(stats.root_seconds);
        m.state_hash_seconds.observe(stats.state_hash_seconds);
        m.state_hash_entries.inc_by(stats.state_hash_entries as u64);
        m.exec_state_write_seconds.observe(stats.write_seconds);
        m.exec_state_write_build_seconds
            .observe(stats.write_build_seconds);
        m.exec_state_write_db_seconds
            .observe(stats.write_db_seconds);
        m.exec_state_write_batch_bytes
            .observe(stats.batch_bytes as f64);
        m.exec_root_dirty_buckets
            .observe(stats.dirty_buckets as f64);
        m.exec_root_bucket_scans.inc_by(stats.bucket_scans as u64);
        m.member_cache_hits.inc_by(stats.member_hits as u64);
        m.member_cache_misses.inc_by(stats.member_misses as u64);
        m.member_cache_evictions
            .inc_by(stats.member_evictions as u64);
        m.member_cache_resident_buckets
            .set(stats.member_resident_buckets as i64);
        for (i, n) in stats.dirty_entries_by_cf.iter().enumerate() {
            m.exec_dirty_entries_by_cf[i].inc_by(*n as u64);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toggle_default_on_only_zero_disables() {
        assert!(parse_exec_pipeline_toggle(None));
        for v in ["1", " 1 ", "", "true", "on", "yes", "2"] {
            assert!(parse_exec_pipeline_toggle(Some(v.to_string())), "{v}");
        }
        assert!(!parse_exec_pipeline_toggle(Some("0".to_string())));
        assert!(!parse_exec_pipeline_toggle(Some(" 0 ".to_string())));
    }

    /// s124: `TORUS_EPOCH_FAST_PATH` is opt-in. `1` / `true` (any case,
    /// trimmed) turn it on; unset, empty, `0`, `false` and anything else
    /// (warned) leave it off.
    #[test]
    fn epoch_fast_path_parse_default_off() {
        assert!(!parse_epoch_fast_path(None));
        for v in [
            "", " ", "0", " 0 ", "false", "FALSE", "off", "on", "yes", "2",
        ] {
            assert!(!parse_epoch_fast_path(Some(v.to_string())), "{v:?}");
        }
        for v in ["1", " 1 ", "true", "TRUE", "True"] {
            assert!(parse_epoch_fast_path(Some(v.to_string())), "{v:?}");
        }
    }

    #[test]
    fn checkpoint_interval_parse() {
        use std::time::Duration;
        // No role default (tests, tools): unset / empty = per-block flush.
        assert_eq!(parse_checkpoint_interval(None, None), None);
        assert_eq!(parse_checkpoint_interval(Some("".into()), None), None);
        assert_eq!(parse_checkpoint_interval(Some("0".into()), None), None);
        assert_eq!(parse_checkpoint_interval(Some(" 0 ".into()), None), None);
        assert_eq!(
            parse_checkpoint_interval(Some("15000".into()), None),
            Some(Duration::from_secs(15))
        );
        assert_eq!(
            parse_checkpoint_interval(Some(" 1 ".into()), None),
            Some(Duration::from_millis(1))
        );
        for bad in ["abc", "-5", "15s", "1.5"] {
            assert_eq!(parse_checkpoint_interval(Some(bad.into()), None), None, "{bad:?}");
        }
        assert_eq!(parse_checkpoint_max_bytes(None), None);
        assert_eq!(parse_checkpoint_max_bytes(Some("0".into())), None);
        assert_eq!(
            parse_checkpoint_max_bytes(Some("268435456".into())),
            Some(268_435_456)
        );
        assert_eq!(parse_checkpoint_max_bytes(Some("lots".into())), None);
    }

    /// Step 6: the compiled default. Unset or empty = the role default
    /// (validators 15 s, `--rpc-only` off); explicit values win, `0` = the
    /// per-block flush; garbage stays the fail-safe per-block flush.
    #[test]
    fn checkpoint_interval_role_default_is_15s_on_validators_off_on_rpc_only() {
        use std::time::Duration;
        assert_eq!(VALIDATOR_CHECKPOINT_INTERVAL, Duration::from_secs(15));
        assert_eq!(role_checkpoint_interval(false), Some(Duration::from_secs(15)));
        assert_eq!(role_checkpoint_interval(true), None);
        for rpc_only in [false, true] {
            let d = role_checkpoint_interval(rpc_only);
            assert_eq!(parse_checkpoint_interval(None, d), d, "{rpc_only}");
            assert_eq!(parse_checkpoint_interval(Some(" ".into()), d), d, "{rpc_only}");
            assert_eq!(parse_checkpoint_interval(Some("0".into()), d), None, "{rpc_only}");
            assert_eq!(
                parse_checkpoint_interval(Some("5000".into()), d),
                Some(Duration::from_secs(5)),
                "{rpc_only}"
            );
            assert_eq!(parse_checkpoint_interval(Some("abc".into()), d), None, "{rpc_only}");
        }
    }

    /// Step 6: the role default yields to a deferred book mode (per-block
    /// flush) instead of refusing to start; an explicit interval still
    /// refuses (task 1.2); Classic / OrderRows run the default.
    #[test]
    fn checkpoint_interval_role_default_yields_to_deferred_book_modes() {
        use std::time::Duration;
        use torus_bridge::native_executor::BookMode;
        let d = role_checkpoint_interval(false);
        for mode in [BookMode::LevelAuthority, BookMode::LevelAuthorityChunked] {
            assert_eq!(resolve_checkpoint_interval(None, d, mode), Ok(None), "{mode:?}");
            assert_eq!(
                resolve_checkpoint_interval(Some("".into()), d, mode),
                Ok(None),
                "{mode:?}"
            );
            let err = resolve_checkpoint_interval(Some("15000".into()), d, mode)
                .expect_err("an explicit interval with a deferred book mode refuses to start");
            assert!(err.contains("TORUS_BOOK_ROWS"), "{err}");
            assert_eq!(
                resolve_checkpoint_interval(Some("0".into()), d, mode),
                Ok(None),
                "{mode:?}"
            );
        }
        for mode in [BookMode::Classic, BookMode::OrderRows] {
            assert_eq!(
                resolve_checkpoint_interval(None, d, mode),
                Ok(Some(Duration::from_secs(15))),
                "{mode:?}"
            );
            assert_eq!(resolve_checkpoint_interval(None, None, mode), Ok(None), "{mode:?}");
            assert_eq!(
                resolve_checkpoint_interval(Some("0".into()), d, mode),
                Ok(None),
                "{mode:?}"
            );
        }
    }

    #[test]
    fn checkpoint_interval_refuses_deferred_book_modes() {
        use std::time::Duration;
        use torus_bridge::native_executor::BookMode;
        let on = Some(Duration::from_secs(15));
        for mode in [BookMode::LevelAuthority, BookMode::LevelAuthorityChunked] {
            let err = check_checkpoint_book_mode(on, mode)
                .expect_err("interval > 0 with a deferred book mode must refuse to start");
            assert!(err.contains("TORUS_CHECKPOINT_INTERVAL_MS"), "{err}");
            assert!(err.contains("TORUS_BOOK_ROWS"), "{err}");
        }
        for mode in [BookMode::Classic, BookMode::OrderRows] {
            assert_eq!(check_checkpoint_book_mode(on, mode), Ok(()), "{mode:?}");
        }
        for mode in [
            BookMode::Classic,
            BookMode::OrderRows,
            BookMode::LevelAuthority,
            BookMode::LevelAuthorityChunked,
        ] {
            assert_eq!(check_checkpoint_book_mode(None, mode), Ok(()), "{mode:?}");
        }
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
        assert!(gate.wait_received(1));
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
        // Wait (bounded) until the sender is inside submit(2): `outstanding` is
        // bumped right before the rendezvous send. A fixed sleep here flaked
        // under load (the thread had not run yet, outstanding still 1).
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while worker.outstanding() < 2 {
            assert!(
                std::time::Instant::now() < deadline,
                "sender thread never reached submit(2) within 10 s (outstanding = {})",
                worker.outstanding()
            );
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        // The sender is now at (or about to enter) the rendezvous send; give it
        // time to complete if it wrongly could. A slow scheduler can only make
        // this check pass, never fail it spuriously.
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

    /// s64: whoever sees W's latch (`failed()`, hence `wait_idle`/`submit`)
    /// must already see the shared `exec_failed` fail-stop. W used to store
    /// its own latch first, so a caller could return from `wait_idle` and read
    /// `exec_failed == false` (the "crash at N latched" flake under load).
    /// The seam parks W between its two stores, which makes the window
    /// deterministic.
    #[test]
    fn failed_latch_implies_exec_failed_visible() {
        let gate = WorkerGate::new();
        let (env, _dir) = temp_env(Some(gate.clone()));
        let exec_failed = env.exec_failed.clone();
        let worker = Arc::new(FlushWorker::spawn(env, 0));
        gate.hold_latch();
        gate.fail_next();
        worker
            .submit(Job::Marker {
                height: 1,
                pending: Arc::new(FrozenPending::marker_only(1)),
            })
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !gate.latch_parked() {
            assert!(std::time::Instant::now() < deadline, "W never reached the latch seam");
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        // W is between its two stores.
        assert!(
            !worker.failed() || exec_failed.load(Ordering::SeqCst),
            "failed() visible before the shared exec_failed fail-stop"
        );
        // The flake's shape: `wait_idle` must not report the failure before
        // `exec_failed` is set. Give it a bounded chance to return early.
        let w2 = worker.clone();
        let ef = exec_failed.clone();
        let waiter = std::thread::spawn(move || {
            let healthy = w2.wait_idle();
            (healthy, ef.load(Ordering::SeqCst))
        });
        let until = std::time::Instant::now() + std::time::Duration::from_millis(200);
        while !waiter.is_finished() && std::time::Instant::now() < until {
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        gate.release();
        let (healthy, latched) = waiter.join().unwrap();
        assert!(!healthy, "wait_idle must report the failure");
        assert!(latched, "wait_idle returned before exec_failed was latched");
        assert!(worker.failed());
        assert!(exec_failed.load(Ordering::SeqCst));
    }

    /// R01b follow-up: once the shared fail-stop is set from outside W (e.g.
    /// the DA latch), W writes no queued job and reports the failure.
    #[test]
    fn external_failstop_refuses_queued_job() {
        let (env, _dir) = temp_env(None);
        let db = env.state_db.clone();
        env.exec_failed.store(true, Ordering::SeqCst);
        let worker = FlushWorker::spawn(env, 0);
        let _ = worker.submit(Job::Marker {
            height: 1,
            pending: Arc::new(FrozenPending::marker_only(1)),
        });
        assert!(!worker.wait_idle(), "wait_idle must report the failure");
        assert!(worker.failed());
        assert_eq!(worker.durable_height(), 0);
        let marker = db
            .get_cf_raw(
                torus_state::cf::CF_CONSENSUS_META,
                torus_state::cf::META_NATIVE_APPLIED_HEIGHT,
            )
            .unwrap();
        assert_eq!(marker, None, "no marker written after the external latch");
    }

    // ---- Item 6 Phase 3 task 1.5: Digest and Checkpoint jobs ----

    type CfDump = Vec<(&'static str, Vec<(Vec<u8>, Vec<u8>)>)>;

    fn dump_all(db: &StateDb) -> CfDump {
        torus_state::cf::ALL_CF_NAMES
            .iter()
            .map(|cf| {
                (
                    *cf,
                    torus_state::StateBackend::iterate_cf(db, cf, None).unwrap(),
                )
            })
            .collect()
    }

    fn assert_dumps_eq(a: &CfDump, b: &CfDump, what: &str) {
        for ((cf, x), (_, y)) in a.iter().zip(b.iter()) {
            assert_eq!(x, y, "{what}: CF {cf} diverged");
        }
    }

    /// One block's frozen set: rewrites / tombstones over a small key space
    /// in hashed CFs (so blocks overwrite each other's rows), the applied
    /// marker put as E's fast path does, and every third block an action
    /// status record.
    fn ckpt_block(
        db: &StateDb,
        height: u64,
    ) -> (
        Arc<FrozenPending>,
        Option<torus_state::action_status::BlockActionStatus>,
    ) {
        use torus_state::StateBackend;
        let o = torus_state::NativeStateOverlay::new(db.clone());
        let cfs = [
            torus_state::cf::CF_NATIVE_ORDERS,
            torus_state::cf::CF_NATIVE_BALANCES,
            torus_state::cf::CF_SESSIONS,
        ];
        for i in 0..6u64 {
            let x = height.wrapping_mul(2_654_435_761).wrapping_add(i * 97);
            let cf = cfs[(x % 3) as usize];
            let key = [b'k', (x % 11) as u8];
            if x.is_multiple_of(5) {
                o.delete_cf_raw(cf, &key).unwrap();
            } else {
                o.put_cf_raw(cf, &key, &[height as u8, i as u8, (x >> 8) as u8])
                    .unwrap();
            }
        }
        o.put_cf_raw(
            torus_state::cf::CF_CONSENSUS_META,
            torus_state::cf::META_NATIVE_APPLIED_HEIGHT,
            &height.to_be_bytes(),
        )
        .unwrap();
        let status = torus_state::action_status::BlockActionStatus {
            evm_skipped: vec![false, true],
            native_skipped: vec![height.is_multiple_of(2)],
            native_failed: Vec::new(),
            // v4 (main s112): a CoreWriter result rides the checkpoint too.
            core_writer: vec![torus_state::action_status::CoreWriterResult {
                seq: height,
                kind: torus_state::action_status::CoreWriterKind::LockboxDeposit,
                trader: alloy_primitives::Address::repeat_byte(height as u8),
                queued_block: height.saturating_sub(1),
                outcome: torus_state::action_status::Outcome::Failed,
                reason: torus_state::action_status::FailureReason::Other,
                message: format!("row {height}"),
                // v5 (main s114): the order id / cloid fields ride it too.
                oid: Some(u128::from(height)),
                cloid: height
                    .is_multiple_of(2)
                    .then_some(u128::MAX - u128::from(height)),
            }],
        };
        let status = height.is_multiple_of(3).then_some(status);
        (o.freeze(height), status)
    }

    /// Per-block reference: the same frozen sets through `Job::Flush`.
    fn reference_db(
        heights: std::ops::RangeInclusive<u64>,
        activation: u64,
    ) -> (StateDb, tempfile::TempDir) {
        let (env, dir) = temp_env(None);
        let db = env.state_db.clone();
        torus_state::running_hash::configure_activation(&db, Some(activation)).unwrap();
        let worker = FlushWorker::spawn(env, 0);
        for h in heights {
            let (pending, action_status) = ckpt_block(&db, h);
            worker
                .submit(Job::Flush {
                    height: h,
                    pending,
                    evm_addrs: Vec::new(),
                    books: None,
                    order_nonces: Vec::new(),
                    action_status,
                })
                .unwrap();
        }
        assert!(worker.wait_idle());
        drop(worker);
        (db, dir)
    }

    /// Hand blocks `heights` to `worker` as digest jobs and return the
    /// since-checkpoint layer E would hold for them.
    fn digest_window(
        worker: &FlushWorker,
        db: &StateDb,
        heights: std::ops::RangeInclusive<u64>,
    ) -> torus_state::CheckpointLayer {
        let mut layer = torus_state::CheckpointLayer::new();
        for h in heights {
            let (pending, action_status) = ckpt_block(db, h);
            layer.merge(&pending);
            worker
                .submit(Job::Digest {
                    height: h,
                    pending,
                    evm_addrs: Vec::new(),
                    order_nonces: Vec::new(),
                    action_status,
                })
                .unwrap();
        }
        layer
    }

    /// 20 blocks as digest jobs leave the DB untouched; the checkpoint at
    /// 100 then writes ONE batch after which the DB equals the per-block
    /// flush DB at 100 (state, marker, running hash, activation record,
    /// attest checkpoint row, action status). A second window (101..=120)
    /// chains from W's in-memory h(100) and matches again.
    #[test]
    fn digest_jobs_then_checkpoint_writes_one_batch() {
        let (env, _dir) = temp_env(None);
        let db = env.state_db.clone();
        torus_state::running_hash::configure_activation(&db, Some(81)).unwrap();
        let before = dump_all(&db);
        let worker = FlushWorker::spawn_checkpointing(env, 0);

        let layer = digest_window(&worker, &db, 81..=100);
        assert!(worker.wait_idle());
        assert_dumps_eq(&dump_all(&db), &before, "digests wrote nothing");
        assert_eq!(worker.durable_height(), 0, "digests are not durable");
        let (ref100, _r1) = reference_db(81..=100, 81);
        let h100 =
            torus_state::running_hash::read_running_hash(&ref100).expect("reference hashed 100");
        assert_eq!(
            worker.running_hash_prev(),
            Some(h100),
            "W chained h(100) in memory"
        );

        worker
            .submit(Job::Checkpoint {
                height: 100,
                layer: Arc::new(layer),
            })
            .unwrap();
        assert!(worker.wait_idle());
        assert_eq!(worker.durable_height(), 100);
        assert!(
            torus_state::running_hash::read_checkpoint(&db, 100).is_some(),
            "attest row"
        );
        assert_dumps_eq(&dump_all(&db), &dump_all(&ref100), "checkpoint at 100");

        let before2 = dump_all(&db);
        let layer = digest_window(&worker, &db, 101..=120);
        assert!(worker.wait_idle());
        assert_dumps_eq(
            &dump_all(&db),
            &before2,
            "second window: digests wrote nothing",
        );
        worker
            .submit(Job::Checkpoint {
                height: 120,
                layer: Arc::new(layer),
            })
            .unwrap();
        assert!(worker.wait_idle());
        let (ref120, _r2) = reference_db(81..=120, 81);
        assert_dumps_eq(&dump_all(&db), &dump_all(&ref120), "checkpoint at 120");
    }

    /// Q6 step 0: each checkpoint observes the DB write and its RocksDB stage
    /// split once (digests observe nothing), and sets the write's start / end
    /// wall clock (unix ms) gauges; the second checkpoint moves them forward.
    #[test]
    fn checkpoint_observes_the_db_write_stage_split_once() {
        let (mut env, _dir) = temp_env(None);
        let metrics = Arc::new(torus_telemetry::Metrics::new());
        env.metrics = Some(metrics.clone());
        let db = env.state_db.clone();
        torus_state::running_hash::configure_activation(&db, Some(1)).unwrap();
        let worker = FlushWorker::spawn_checkpointing(env, 0);
        let hists = [
            "torus_exec_checkpoint_db_write_seconds",
            "torus_exec_checkpoint_db_wal_seconds",
            "torus_exec_checkpoint_db_memtable_seconds",
            "torus_exec_checkpoint_db_delay_seconds",
            "torus_exec_checkpoint_db_pre_post_seconds",
            "torus_exec_checkpoint_db_mutex_seconds",
            "torus_exec_checkpoint_db_cond_wait_seconds",
            "torus_exec_checkpoint_db_residual_seconds",
        ];
        let value = |text: &str, name: &str| -> f64 {
            text.lines()
                .find_map(|l| l.strip_prefix(&format!("{name} ")))
                .unwrap_or_else(|| panic!("{name} missing:\n{text}"))
                .trim()
                .parse()
                .unwrap()
        };
        let before_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as f64;

        let layer = digest_window(&worker, &db, 1..=10);
        assert!(worker.wait_idle());
        let text = metrics.encode();
        for h in hists {
            assert_eq!(value(&text, &format!("{h}_count")), 0.0, "digests: {h}");
        }
        worker
            .submit(Job::Checkpoint {
                height: 10,
                layer: Arc::new(layer),
            })
            .unwrap();
        assert!(worker.wait_idle());
        let text = metrics.encode();
        for h in hists {
            assert_eq!(
                value(&text, &format!("{h}_count")),
                1.0,
                "one checkpoint: {h}"
            );
        }
        let write = value(&text, "torus_exec_checkpoint_db_write_seconds_sum");
        assert!(write > 0.0, "write wall timed");
        let stages: f64 = hists[1..5]
            .iter()
            .map(|h| value(&text, &format!("{h}_sum")))
            .sum::<f64>()
            + value(&text, "torus_exec_checkpoint_db_residual_seconds_sum");
        assert!(
            (stages - write).abs() < 1e-6,
            "WAL + memtable + delay + pre/post + residual = wall: {stages} vs {write}"
        );
        let start = value(&text, "torus_exec_checkpoint_db_write_start_ms");
        let end = value(&text, "torus_exec_checkpoint_db_write_end_ms");
        assert!(
            before_ms <= start && start <= end,
            "span [{start}, {end}] after {before_ms}"
        );

        let layer = digest_window(&worker, &db, 11..=20);
        assert!(worker.wait_idle());
        std::thread::sleep(std::time::Duration::from_millis(5));
        worker
            .submit(Job::Checkpoint {
                height: 20,
                layer: Arc::new(layer),
            })
            .unwrap();
        assert!(worker.wait_idle());
        let text = metrics.encode();
        for h in hists {
            assert_eq!(
                value(&text, &format!("{h}_count")),
                2.0,
                "two checkpoints: {h}"
            );
        }
        assert!(value(&text, "torus_exec_checkpoint_db_write_start_ms") > end);
    }

    /// A failed checkpoint write latches W and the shared fail-stop; nothing
    /// of the window lands and no later job is accepted.
    #[test]
    fn checkpoint_write_failure_latches() {
        let gate = WorkerGate::new();
        let (env, _dir) = temp_env(Some(gate.clone()));
        let db = env.state_db.clone();
        let exec_failed = env.exec_failed.clone();
        torus_state::running_hash::configure_activation(&db, Some(1)).unwrap();
        let before = dump_all(&db);
        let worker = FlushWorker::spawn_checkpointing(env, 0);
        let layer = digest_window(&worker, &db, 1..=10);
        assert!(worker.wait_idle());
        assert_eq!(worker.durable_height(), 0, "digests are not durable");
        gate.fail_next();
        worker
            .submit(Job::Checkpoint {
                height: 10,
                layer: Arc::new(layer),
            })
            .unwrap();
        assert!(!worker.wait_idle(), "wait_idle must report the failure");
        assert!(worker.failed());
        assert!(
            exec_failed.load(Ordering::SeqCst),
            "shared fail-stop latched"
        );
        assert_eq!(worker.durable_height(), 0);
        assert_dumps_eq(&dump_all(&db), &before, "nothing of the window landed");
        let (pending, _) = ckpt_block(&db, 11);
        assert!(
            worker
                .submit(Job::Digest {
                    height: 11,
                    pending,
                    evm_addrs: Vec::new(),
                    order_nonces: Vec::new(),
                    action_status: None,
                })
                .is_err(),
            "no job may be accepted after the latch"
        );
    }

    /// At most one checkpoint in flight: while W holds checkpoint 5, digest
    /// jobs still queue without blocking E, but handing off checkpoint 10
    /// blocks until 5 is durable. The end state equals the per-block DB.
    #[test]
    fn second_checkpoint_waits_for_first() {
        let gate = WorkerGate::new();
        let (env, _dir) = temp_env(Some(gate.clone()));
        let db = env.state_db.clone();
        torus_state::running_hash::configure_activation(&db, Some(1)).unwrap();
        let worker = Arc::new(FlushWorker::spawn_checkpointing(env, 0));

        let layer = digest_window(&worker, &db, 1..=5);
        assert!(worker.wait_idle());
        gate.hold();
        worker
            .submit(Job::Checkpoint {
                height: 5,
                layer: Arc::new(layer),
            })
            .unwrap();
        assert!(gate.wait_received(5), "W holds checkpoint 5");
        // Digest jobs do not wait for the checkpoint (buffered queue).
        let layer2 = digest_window(&worker, &db, 6..=10);
        let w2 = worker.clone();
        let sender = std::thread::spawn(move || {
            w2.submit(Job::Checkpoint {
                height: 10,
                layer: Arc::new(layer2),
            })
            .unwrap();
        });
        std::thread::sleep(std::time::Duration::from_millis(150));
        assert!(
            !sender.is_finished(),
            "checkpoint 10 must wait for checkpoint 5"
        );
        assert_eq!(worker.durable_height(), 0);
        gate.release();
        sender.join().unwrap();
        assert!(worker.wait_idle());
        assert_eq!(worker.durable_height(), 10);
        assert!(worker.checkpoint_waits() >= 1, "the wait is counted");
        let (reference, _r) = reference_db(1..=10, 1);
        assert_dumps_eq(&dump_all(&db), &dump_all(&reference), "two checkpoints");
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
