//! `App` trait implementation for hotstuff_rs, wired to the bridge.
//!
//! ## Consensus-then-Execute Architecture
//!
//! `produce_block` and `validate_block` do NOT execute EVM transactions.
//! They only build/validate the raw transaction list for consensus ordering.
//!
//! All execution happens post-commit. This eliminates the entire class of
//! state divergence bugs by construction.
//!
//! ## Execution Pipelining (CTE8)
//!
//! Execution is offloaded to a dedicated background thread via a bounded
//! `SyncSender` channel. `on_committed_block` sends the finalized block to
//! the execution thread and returns immediately, allowing consensus to
//! proceed at full speed (~43ms/block) regardless of execution load.

use sha2::{Digest, Sha256};

use ed25519_dalek::VerifyingKey;
use hotstuff_rs::app::{
    App, ProduceBlockRequest, ProduceBlockResponse, ValidateBlockRequest, ValidateBlockResponse,
};
use hotstuff_rs::hotstuff::types::EquivocationEvidence;
use hotstuff_rs::types::block::Block;
use hotstuff_rs::types::data_types::{CryptoHash, Data, Datum, Power};
use hotstuff_rs::types::update_sets::ValidatorSetUpdates;

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::SyncSender;
use std::sync::{Arc, RwLock};
use std::thread::JoinHandle;
use torus_bridge::{
    decode_all_txs, sort_native_actions, BlockCommitter, BlockProposer, BlockValidator,
    BundleState, NativeExecContext, NativeExecutor,
};
use torus_economics::{EpochManager, SlashReason, StakingManager};
use torus_evm::{EvmExecutor, TORUS_CHAIN_ID};
use torus_mempool::Mempool;
use torus_state::cf::{
    CF_BLOCK_BODIES, CF_BLOCK_HEADERS, CF_COMMIT_MANIFEST, CF_CONSENSUS_META,
    META_NATIVE_APPLIED_HEIGHT,
};
use torus_state::{NativeStateOverlay, StateBackend, StateDb};
use torus_types::{
    Address, ChainConfig, CompactBlock, TorusBlock, TorusBlockBody, TorusBlockHeader, ValidatorSet,
};

#[derive(Clone, Debug)]
struct PendingSlash {
    validator: Address,
    fraction_bps: u16,
    reason: SlashReason,
    tombstone: bool,
}

struct CommittedBlockMsg {
    torus_block: TorusBlock,
    pending_slashes: Vec<PendingSlash>,
}

/// Regime-B strict-order execution: how a committed block's bodies reach the
/// execution pipeline. Either the bodies are already in hand (a FULL consensus
/// datum, or a cached proposal whose action hashes match the committed compact),
/// or only the compact reference is known and the bodies must be reconstructed
/// from the durable DA store (which may still be missing them → a heal-able
/// hole). Kept as a source (not a pre-reconstructed block) so a deferred hole is
/// RE-reconstructed from the (healing) DA store on each drain attempt.
enum ExecSource {
    /// Bodies present now — send as soon as it is this height's turn in line.
    Ready(TorusBlock),
    /// Reconstruct from the durable DA store on each attempt (may miss → hole).
    Compact(CompactBlock),
    /// FIX 1b (boot park+heal): a committed height whose header (and possibly
    /// body) is only in the durable CFs — used to seed the strict-order queue at
    /// boot when replay could not reconstruct a gap height locally. Re-loads
    /// header (`CF_BLOCK_HEADERS`) + body (`CF_BLOCK_BODIES`) on each drain
    /// attempt, so the hole heals the moment the body becomes durable (FIX 1a
    /// write, or a block-sync re-delivery that writes it) instead of a boot
    /// fail-stop. A still-missing body degrades to the same heal-able hole.
    Durable(u64),
}

/// A committed block buffered awaiting its strictly-ordered turn to execute.
/// Only ever populated while an earlier height is an unhealed hole — in the
/// healthy in-order case the queue stays empty and the hot path never touches
/// this map (see [`TorusApp::enqueue_for_execution`]).
struct DeferredExecBlock {
    source: ExecSource,
    slashes: Vec<PendingSlash>,
}

/// Regime-B: how long an execution hole (a committed block whose native bodies
/// cannot be reconstructed) may keep retrying before the node fail-stops. A node
/// that cannot obtain a committed block's bodies is broken, and a silent
/// divergence is far worse than a loud halt. Overridable via
/// `TORUS_EXEC_HOLE_BUDGET_SECS` for ops; default ~5 min of retries.
const EXEC_HOLE_FAILSTOP_BUDGET: std::time::Duration = std::time::Duration::from_secs(5 * 60);

/// Regime-B: throttle for the loud "execution stalled at height H" warning so an
/// unrecoverable hole keeps screaming at a readable cadence (never silent, never
/// per-tick spam) while it retries.
const EXEC_HOLE_LOG_INTERVAL: std::time::Duration = std::time::Duration::from_secs(10);

/// Regime-B. The heal budget for an execution hole, [`EXEC_HOLE_FAILSTOP_BUDGET`]
/// by default, overridable via `TORUS_EXEC_HOLE_BUDGET_SECS` (0 = fail-stop on
/// the first stalled attempt — useful for tests / paranoid operators).
fn exec_hole_failstop_budget() -> std::time::Duration {
    match std::env::var("TORUS_EXEC_HOLE_BUDGET_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
    {
        Some(secs) => std::time::Duration::from_secs(secs),
        None => EXEC_HOLE_FAILSTOP_BUDGET,
    }
}

/// Shared consensus state for leader discovery by non-consensus components (RPC).
pub struct LeaderState {
    view: AtomicU64,
    validators: RwLock<hotstuff_rs::types::validator_set::ValidatorSet>,
}

impl LeaderState {
    fn new() -> Self {
        Self {
            view: AtomicU64::new(1),
            validators: RwLock::new(hotstuff_rs::types::validator_set::ValidatorSet::new()),
        }
    }

    fn set_view(&self, view: u64) {
        self.view.store(view, Ordering::Relaxed);
    }

    fn sync_validators(&self, vs: &torus_types::ValidatorSet) {
        let mut hs_vs = hotstuff_rs::types::validator_set::ValidatorSet::new();
        for v in &vs.validators {
            if let Ok(vk) = VerifyingKey::from_bytes(&v.pubkey.0) {
                hs_vs.put(&vk, Power::new(v.power));
            }
        }
        *self.validators.write().unwrap() = hs_vs;
    }

    pub fn current_view(&self) -> u64 {
        self.view.load(Ordering::Relaxed)
    }

    pub fn current_leader(&self) -> Option<VerifyingKey> {
        let vs = self.validators.read().unwrap();
        if vs.is_empty() {
            return None;
        }
        let view =
            hotstuff_rs::types::data_types::ViewNumber::new(self.view.load(Ordering::Relaxed));
        Some(hotstuff_rs::pacemaker::select_leader(view, &vs))
    }
}

struct ExecutionContext {
    state_db: StateDb,
    validator: BlockValidator,
    evm_executor: EvmExecutor,
    staking: StakingManager,
    epoch_length: u64,
    max_validators: u32,
    treasury_address: Address,
    dev_pool_address: Address,
    metrics: Option<Arc<torus_telemetry::Metrics>>,
    /// Shared mempool handle for the exec trust-cache read path: a HIT lets the
    /// execution thread reuse a locally-verified sender and skip the secp256k1
    /// recover. `None` when the node runs without a mempool (rpc-only / tests).
    mempool: Option<Arc<Mempool>>,
    /// Node-local gate for the trust-cache read (`--exec-trust-cache`, default
    /// off). When false the cache is never consulted at exec -> full recover every
    /// time (today's behavior). A HIT is deterministic (== fresh recover), so this
    /// only changes performance, never the resolved sender or state.
    exec_trust_cache: bool,
    /// O3: background writer for trade-history CFs (node-local, non-root).
    /// `Some` on the live node — fills buffer their KVs during exec and this
    /// writer applies them off the execution thread. `None` in tests -> trades
    /// write inline through the overlay exactly as before O3. Dropped with the
    /// ExecutionContext at execution-thread exit, which drains the queue before
    /// `TorusApp::Drop`'s join returns (shutdown flush ordering).
    trade_writer: Option<torus_state::BackgroundCfWriter>,
    /// T1.5 fail-stop latch, shared with `TorusApp` on the consensus thread.
    /// Set (never cleared) when block execution hits a fatal error (e.g. a
    /// market worker panicked and its book is lost). The execution loop exits
    /// on it and the node stops producing, voting, and finalizing.
    exec_failed: Arc<AtomicBool>,
}

// ---- Standalone helpers (used by both execution thread and crash recovery) ----

fn read_native_applied_height(state_db: &StateDb) -> Option<u64> {
    state_db
        .get_cf_raw(CF_CONSENSUS_META, META_NATIVE_APPLIED_HEIGHT)
        .ok()
        .flatten()
        .and_then(|data| {
            if data.len() == 8 {
                Some(u64::from_be_bytes(data[..8].try_into().ok()?))
            } else {
                None
            }
        })
}

fn write_native_applied_height(state_db: &StateDb, height: u64) {
    let _ = state_db.put_cf_raw(
        CF_CONSENSUS_META,
        META_NATIVE_APPLIED_HEIGHT,
        &height.to_be_bytes(),
    );
}

fn find_last_committed_height(state_db: &StateDb) -> Option<u64> {
    let db = state_db.inner();
    let cf = db.cf_handle(CF_BLOCK_HEADERS)?;
    // T1.6: scan from the end, SKIPPING non-height keys. The pruner stores its
    // 16-byte PRUNE_META_KEY (first byte 0x5F) in this CF, which sorts AFTER
    // every 8-byte BE height key — reading only the last key returned None
    // after the first prune, silently skipping crash-replay.
    scan_last_committed_height(db.iterator_cf(cf, rocksdb::IteratorMode::End))
}

/// Core end-first scan for [`find_last_committed_height`], factored out so the
/// skip-prune-meta and continue-past-error paths are unit-testable without a
/// live RocksDB (which cannot be coerced into yielding an iterator `Err`).
///
/// P1(b): a transient RocksDB read error on ONE iterator entry must not abort
/// the whole scan — aborting would mask a real committed height sitting BELOW
/// the failing entry (end-first order) and silently skip crash-replay. So we
/// log at warn and `continue`; only a scan that truly finds no 8-byte height
/// key falls through to `None`.
fn scan_last_committed_height<K, V, E>(
    entries: impl IntoIterator<Item = Result<(K, V), E>>,
) -> Option<u64>
where
    K: AsRef<[u8]>,
    E: std::fmt::Display,
{
    for entry in entries {
        let key = match entry {
            Ok((key, _)) => key,
            Err(e) => {
                tracing::warn!(
                    %e,
                    "find_last_committed_height: transient iterator error, skipping entry"
                );
                continue;
            }
        };
        let key = key.as_ref();
        if key.len() == 8 {
            return Some(u64::from_be_bytes(key[..8].try_into().ok()?));
        }
    }
    None
}

/// T1.2 completeness gate for the `on_committed_block` cache fast-path: a cached
/// proposal may only supply the bodies for a committed compact when its native
/// actions hash-for-hash (order + value) to the committed reference. Any drift
/// (a stale same-height re-proposal, a partial/other action set) rejects the
/// cache so the caller falls back to the all-or-nothing DA reconstruction.
///
/// Cost: O(n) `compute_action_hash` over the cached actions — the same order as
/// the DA reconstruction's per-hash lookup it replaces, never heavier.
fn cached_matches_compact(cached: &TorusBlock, compact: &CompactBlock) -> bool {
    cached.native_actions.len() == compact.native_action_hashes.len()
        && cached
            .native_actions
            .iter()
            .map(torus_types::compute_action_hash)
            .eq(compact.native_action_hashes.iter().copied())
}

fn persist_block_header(state_db: &StateDb, block: &TorusBlock) {
    let block_hash = alloy_primitives::keccak256(block.header.canonical_header_bytes());
    let header_json = match serde_json::to_vec(&block.header) {
        Ok(j) => j,
        Err(e) => {
            tracing::error!(%e, "failed to serialize block header");
            return;
        }
    };
    let mut data = Vec::with_capacity(32 + header_json.len());
    data.extend_from_slice(block_hash.as_slice());
    data.extend_from_slice(&header_json);
    if let Err(e) = state_db.put_cf_raw(CF_BLOCK_HEADERS, &block.header.height.to_be_bytes(), &data)
    {
        tracing::error!(%e, height = block.header.height, "failed to persist block header");
    }
}

/// FIX 1a (crash-safety, commit->execute kill-window durability). Persist a
/// committed block's HEADER and BODY to the durable CFs AT COMMIT TIME — i.e. in
/// `dispatch_to_exec`, BEFORE the block is handed to the execution pipeline.
///
/// ROOT CAUSE this closes: the body was previously written to `CF_BLOCK_BODIES`
/// only at EXECUTION (`execute_committed_block` line ~910 / `commit_block_metadata`),
/// and a native-only block's header only at execution too (`persist_block_header`
/// in the `has_evm == false` arm). A `SIGKILL` in the commit->execute window
/// therefore lost the body (and possibly the committed-height header), so boot
/// crash-recovery (`replay_gap` -> `load_replay_body`, which reads ONLY
/// `CF_BLOCK_BODIES`) found a committed NON-EMPTY height with no body and
/// fail-stopped (devnet t12-final-a/c: `height=132`/`h224`, then `exit(70)`).
/// The durable DA store (`CF_NATIVE_PENDING`) DID retain the native-action
/// bodies, but the persisted header carries only `native_action_count`, NOT the
/// `native_action_hashes` (those live only on the `CompactBlock`), so recovery
/// had no way to reconstruct from the DA store by hash.
///
/// Writing header+body here makes every committed-and-dispatched block durably
/// reconstructable on restart, regardless of where in the execute path a crash
/// lands. The execution-time writes are kept (idempotent, same bytes; they also
/// cover the boot-replay execute path and the EVM receipts/indices that are only
/// known post-execution). Best-effort like the execution-time writes: a failure
/// is loud but does not abort the commit (a still-missing body degrades to the
/// same heal-able hole, never a silent state loss).
fn persist_committed_block_durably(state_db: &StateDb, block: &TorusBlock) {
    // Header first (find_last_committed_height scans CF_BLOCK_HEADERS): a crash
    // between this and execution then still exposes the committed height so the
    // gap is visible and replayable, rather than silently vanishing.
    persist_block_header(state_db, block);
    match serde_json::to_vec(&block.body()) {
        Ok(body_bytes) => {
            if let Err(e) = state_db.put_cf_raw(
                CF_BLOCK_BODIES,
                &block.header.height.to_be_bytes(),
                &body_bytes,
            ) {
                tracing::error!(
                    %e,
                    height = block.header.height,
                    "FIX1a: failed to persist committed block body at commit time (crash-recovery may hole here)"
                );
            }
        }
        Err(e) => {
            tracing::error!(
                %e,
                height = block.header.height,
                "FIX1a: failed to serialize committed block body at commit time"
            );
        }
    }
}

/// FIX (heal-channel crash-safety). Persist the committed consensus DATUM
/// verbatim, keyed by height, to [`CF_COMMIT_MANIFEST`]. Called as the FIRST
/// durable action on every commit callback (`on_committed_block`), BEFORE the
/// body is reconstructed or the block enters the execution pipeline.
///
/// ROOT CAUSE this closes (part B): a `SIGKILL` in the commit->execute window can
/// leave a committed NON-EMPTY height with its body absent from `CF_BLOCK_BODIES`
/// (the body write in `dispatch_to_exec` had not landed). Boot crash-recovery
/// PARKS that hole (FIX 1b), but the parked source (`ExecSource::Durable`) had no
/// way to HEAL it from peers: the persisted header carries only
/// `native_action_count`, not the `native_action_hashes`, so the node could not
/// content-address the missing bodies for the `/torus/native-da/1.0` pull. It
/// therefore stalled forever (`missing=0`, t12-r3-full h1165). The datum here is a
/// `CompactBlock` under compact proposals — it carries exactly those hashes — so
/// recovery can rebuild the compact reference and drive the peer pull (see
/// [`recovery_exec_source`]). Best-effort: a write failure is loud but never
/// aborts the commit (it degrades to the same un-healable-but-loud hole, never a
/// silent state loss).
fn persist_commit_manifest(state_db: &StateDb, height: u64, datum_bytes: &[u8]) {
    if let Err(e) = state_db.put_cf_raw(CF_COMMIT_MANIFEST, &height.to_be_bytes(), datum_bytes) {
        tracing::error!(
            %e,
            height,
            "failed to persist commit manifest at commit time (a boot-parked hole here could not heal from peers)"
        );
    }
}

/// Best-effort prune of a committed height's manifest once its body is durable in
/// `CF_BLOCK_BODIES` (i.e. at `dispatch_to_exec`, after
/// [`persist_committed_block_durably`]). Keeps [`CF_COMMIT_MANIFEST`] bounded to
/// the committed-but-not-yet-dispatched window: a still-parked hole never
/// dispatches, so its manifest is retained for the heal; a healthy in-order block
/// drops its (now-redundant) manifest immediately.
fn prune_commit_manifest(state_db: &StateDb, height: u64) {
    if let Err(e) = state_db.delete_cf_raw(CF_COMMIT_MANIFEST, &height.to_be_bytes()) {
        tracing::warn!(%e, height, "failed to prune commit manifest (harmless; grows the CF)");
    }
}

/// Load a committed height's durable datum from [`CF_COMMIT_MANIFEST`], if present.
fn load_commit_manifest(state_db: &StateDb, height: u64) -> Option<Vec<u8>> {
    match state_db.get_cf_raw(CF_COMMIT_MANIFEST, &height.to_be_bytes()) {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(%e, height, "crash recovery: failed to read commit manifest");
            None
        }
    }
}

/// FIX (heal channel). Decide how a boot-parked committed `height` re-enters the
/// strict-order execution queue.
///
/// Prefer the durable COMMIT MANIFEST (the committed datum): decoded to a
/// [`CompactBlock`] it yields `ExecSource::Compact`, whose reconstruction returns
/// the REAL missing `native_action_hashes` on a body miss — so the heal loop
/// (`note_exec_hole`) fans a `/torus/native-da/1.0` pull for those exact hashes
/// out to ALL peers, which serve them from their durable `CF_NATIVE_PENDING`.
/// A FULL-block datum decodes to `ExecSource::Ready` (bodies inline — no pull
/// needed). Only when NO manifest exists (legacy pre-fix DBs) does this fall back
/// to `ExecSource::Durable`, which can heal solely from a local durable body.
fn recovery_exec_source(state_db: &StateDb, height: u64) -> ExecSource {
    if let Some(datum) = load_commit_manifest(state_db, height) {
        if let Ok(full) = bincode::deserialize::<TorusBlock>(&datum) {
            return ExecSource::Ready(full);
        }
        if let Ok(compact) = bincode::deserialize::<CompactBlock>(&datum) {
            return ExecSource::Compact(compact);
        }
        tracing::error!(
            height,
            "crash recovery: commit manifest present but undecodable — falling back to durable-body heal"
        );
    }
    ExecSource::Durable(height)
}

/// PART 2 (P0 SAFETY, "commit means commit"): when a block re-arrives at a
/// height we have ALREADY applied, decide whether it is a benign duplicate
/// (same block hash → silent skip) or a CONFLICTING commit (a DIFFERENT block
/// hash at a height we already finalized — a local agreement violation).
///
/// The persisted header at `CF_BLOCK_HEADERS[height]` stores the committed
/// block's hash in its first 32 bytes (see [`persist_block_header`] /
/// `BlockCommitter::commit_block_metadata`). We compare that against
/// `incoming_hash` = `keccak256(header.canonical_header_bytes())`.
///
/// On a genuine conflict this SCREAMS with both hashes and latches `exec_failed`
/// (the same fail-stop latch used by the T1.5 native-exec watcher), returning
/// `true` so the caller aborts instead of silently dropping the divergent block.
/// A same-hash duplicate, or no persisted header, returns `false` (unchanged
/// silent-skip behavior).
fn detect_conflicting_commit(
    state_db: &StateDb,
    height: u64,
    incoming_hash: &alloy_primitives::B256,
    exec_failed: &std::sync::atomic::AtomicBool,
) -> bool {
    let data = match state_db.get_cf_raw(CF_BLOCK_HEADERS, &height.to_be_bytes()) {
        Ok(Some(d)) if d.len() >= 32 => d,
        // No persisted header (or a truncated one) to compare against — cannot
        // assert a conflict, so preserve the benign silent-skip behavior.
        _ => return false,
    };
    if data[..32] == incoming_hash.as_slice()[..] {
        // Same block re-arriving at an already-applied height: benign duplicate.
        return false;
    }

    let persisted = alloy_primitives::B256::from_slice(&data[..32]);
    tracing::error!(
        height,
        committed_hash = %persisted,
        incoming_hash = %incoming_hash,
        "CRITICAL SAFETY VIOLATION: a DIFFERENT block arrived at an already-applied height — \
         two conflicting blocks were finalized at the same height. Latching fail-stop \
         (commit means commit); halting the execution pipeline."
    );
    exec_failed.store(true, Ordering::SeqCst);
    true
}

// ---- Crash-recovery gap replay (T156-F2) ----

/// Load and deserialize the durable header for `height` from `CF_BLOCK_HEADERS`.
/// Returns `None` when the header is missing OR corrupt — both are "cannot load" for replay.
fn load_replay_header(state_db: &StateDb, height: u64) -> Option<TorusBlockHeader> {
    match state_db.get_cf_raw(CF_BLOCK_HEADERS, &height.to_be_bytes()) {
        Ok(Some(data)) if data.len() > 32 => match serde_json::from_slice(&data[32..]) {
            Ok(h) => Some(h),
            Err(e) => {
                tracing::error!(%e, height, "crash recovery: failed to deserialize block header");
                None
            }
        },
        Ok(Some(_)) => {
            tracing::error!(height, "crash recovery: block header record too short");
            None
        }
        Ok(None) => None,
        Err(e) => {
            tracing::error!(%e, height, "crash recovery: error reading block header");
            None
        }
    }
}

/// Load and deserialize the durable body for `height` from `CF_BLOCK_BODIES`.
/// Returns `None` when the body is missing (never persisted / pruned) OR corrupt.
fn load_replay_body(state_db: &StateDb, height: u64) -> Option<TorusBlockBody> {
    match state_db.get_cf_raw(CF_BLOCK_BODIES, &height.to_be_bytes()) {
        Ok(Some(data)) => match serde_json::from_slice(&data) {
            Ok(b) => Some(b),
            Err(e) => {
                tracing::error!(%e, height, "crash recovery: failed to deserialize block body");
                None
            }
        },
        Ok(None) => None,
        Err(e) => {
            tracing::error!(%e, height, "crash recovery: error reading block body");
            None
        }
    }
}

/// Outcome of replaying the `[applied+1, committed]` execution gap.
#[derive(Debug)]
enum ReplayGapOutcome {
    /// Every height in the gap was loaded and executed in order; carries the last (== committed)
    /// header so the caller can set `last_header` / the leader view.
    Complete(TorusBlockHeader),
    /// A height in the gap could NOT be reconstructed (missing/corrupt header, or a missing/corrupt
    /// body while the header says the block is non-empty). The node must fail-stop rather than mark
    /// the height applied without executing it (silent state loss). Carries the offending height and
    /// the last header that WAS successfully executed before the hole.
    Hole {
        height: u64,
        last_good: TorusBlockHeader,
    },
}

/// Replay every committed-but-unexecuted block in `(applied, committed]`, in ascending height order
/// (T156-F2). Factored out as a pure seam so the loop, the empty-vs-pruned decision, and the
/// fail-stop-on-hole behavior are unit-testable without a live `TorusApp`/RocksDB.
///
/// For each height:
///   - `load_header(h)` must return the header, else the block is unreconstructable -> `Hole`.
///   - A header whose native/EVM counts are both zero is a genuine empty block: it is executed with
///     empty bodies (no body load needed; `execute` just advances the applied marker).
///   - A non-empty header requires `load_body(h)`; a missing/corrupt body (e.g. pruned) is a `Hole`.
///     This replaces the old "no body -> silently mark applied" arm, which lost state for pruned
///     bodies that are indistinguishable from empty blocks by presence alone.
///
/// On a `Hole` the loop STOPS immediately: it does NOT execute or mark the offending height (or any
/// height past it), so `applied` stays below the hole and the caller can latch the fail-stop.
fn replay_gap<H, B, X>(
    applied: u64,
    committed: u64,
    mut load_header: H,
    mut load_body: B,
    mut execute: X,
) -> ReplayGapOutcome
where
    H: FnMut(u64) -> Option<TorusBlockHeader>,
    B: FnMut(u64) -> Option<TorusBlockBody>,
    X: FnMut(&TorusBlock),
{
    let mut last_good = torus_bridge::genesis_parent_header();
    for height in (applied + 1)..=committed {
        let header = match load_header(height) {
            Some(h) => h,
            None => {
                tracing::error!(
                    height,
                    committed_height = committed,
                    "crash recovery: FAIL-STOP — cannot load header for committed gap height; refusing to skip it (would silently lose state)"
                );
                return ReplayGapOutcome::Hole { height, last_good };
            }
        };

        let is_empty = header.native_action_count == 0 && header.evm_tx_count == 0;
        let block = if is_empty {
            TorusBlock {
                header: header.clone(),
                native_actions: vec![],
                evm_transactions: vec![],
                core_writer_actions: vec![],
            }
        } else {
            match load_body(height) {
                Some(body) => TorusBlock {
                    header: header.clone(),
                    native_actions: body.native_actions,
                    evm_transactions: body.evm_transactions,
                    core_writer_actions: body.core_writer_actions,
                },
                None => {
                    tracing::error!(
                        height,
                        committed_height = committed,
                        native_action_count = header.native_action_count,
                        evm_tx_count = header.evm_tx_count,
                        "crash recovery: FAIL-STOP — body missing/pruned for NON-EMPTY committed gap height; refusing to mark applied without executing (would silently lose state)"
                    );
                    return ReplayGapOutcome::Hole { height, last_good };
                }
            }
        };

        execute(&block);
        last_good = header;
    }
    ReplayGapOutcome::Complete(last_good)
}

/// Authoritative ancestry check at commit/execution time.
///
/// Execution is sequential, so the parent (height-1) is guaranteed applied and
/// persisted by the time we execute `height`. Regime-B strict-order execution
/// STRENGTHENS this from "usually true" to STRICTLY SOUND: the pipeline never
/// receives H+1 before H (a missing-body H holds every later height in the
/// `deferred_exec` queue until it heals), so the parent is ALWAYS persisted
/// before its child reaches this check — a mismatch here is therefore a genuine
/// ancestry/fork violation, never an artefact of out-of-order execution.
/// We compare the block's
/// `parent_hash` against the keccak canonical hash we durably recorded for the
/// parent (first 32 bytes of `CF_BLOCK_HEADERS[height-1]`, written by
/// [`persist_block_header`] / `commit_block_metadata`). Unlike the best-effort
/// voting-time check in `validate_block`, this cannot be fooled by a pending
/// parent — the parent is already final here.
///
/// Genesis edge: the height-1 block's parent is the synthetic
/// `genesis_parent_header` (never persisted), so its expected hash is computed
/// directly. If the parent header cannot be resolved (missing/truncated) we
/// cannot assert a conflict, so we allow — the same conservative stance as
/// [`detect_conflicting_commit`].
///
/// On a genuine mismatch this SCREAMS with both hashes and latches the fail-stop
/// (`exec_failed`), returning `true` so the caller aborts.
fn detect_parent_link_violation(
    state_db: &StateDb,
    height: u64,
    parent_hash: &alloy_primitives::B256,
    exec_failed: &std::sync::atomic::AtomicBool,
) -> bool {
    if height == 0 {
        return false;
    }
    let expected = if height == 1 {
        alloy_primitives::keccak256(torus_bridge::genesis_parent_header().canonical_header_bytes())
    } else {
        match state_db.get_cf_raw(CF_BLOCK_HEADERS, &(height - 1).to_be_bytes()) {
            Ok(Some(d)) if d.len() >= 32 => alloy_primitives::B256::from_slice(&d[..32]),
            // Parent header not resolvable — cannot assert a conflict.
            _ => return false,
        }
    };
    if *parent_hash == expected {
        return false;
    }
    tracing::error!(
        height,
        claimed_parent = %parent_hash,
        expected_parent = %expected,
        "CRITICAL SAFETY VIOLATION: committed block's parent_hash does not match the \
         locally-finalized parent at height-1 — an ancestry/fork violation. Latching \
         fail-stop (commit means commit); halting the execution pipeline."
    );
    exec_failed.store(true, Ordering::SeqCst);
    true
}

// ---- Execution pipeline ----

impl ExecutionContext {
    fn execute_committed_block(
        &self,
        torus_block: &TorusBlock,
        pending_slashes: Vec<PendingSlash>,
    ) {
        let height = torus_block.header.height;

        // T1.2 body-determinism FAIL-STOP: the committed header is the consensus
        // datum; its `native_action_count` is a hashed field of the eth header.
        // If the body handed to execution carries a DIFFERENT number of native
        // actions than the committed header commits to, executing it would (a)
        // apply a divergent native-state subset and (b) persist an eth header
        // whose count no longer matches the body executed — the exact minority
        // divergence proven on devnet (t12-diag1: v2 executed 40 of a committed
        // 59). The completeness fix in `on_committed_block` makes this
        // unreachable; keep it as a hard latch so a partial body is NEVER
        // executed and a divergent header is NEVER persisted.
        if torus_block.native_actions.len() != torus_block.header.native_action_count as usize {
            tracing::error!(
                height,
                header_count = torus_block.header.native_action_count,
                body_count = torus_block.native_actions.len(),
                "FATAL: committed body count != committed header native_action_count — refusing \
                 to execute a partial body / persist a divergent header (fail-stop)"
            );
            self.exec_failed.store(true, Ordering::SeqCst);
            return;
        }

        if let Some(applied) = read_native_applied_height(&self.state_db) {
            if applied >= height {
                // PART 2 (P0 SAFETY): before silently skipping an already-applied
                // height, make sure this is the SAME block we finalized — not a
                // conflicting sibling. A different hash at a committed height is a
                // durable agreement violation; fail-stop instead of dropping it.
                let incoming_hash =
                    alloy_primitives::keccak256(torus_block.header.canonical_header_bytes());
                if detect_conflicting_commit(
                    &self.state_db,
                    height,
                    &incoming_hash,
                    &self.exec_failed,
                ) {
                    return;
                }
                tracing::debug!(
                    height,
                    applied,
                    "execution pipeline: already applied, skipping"
                );
                return;
            }
        }

        // Authoritative ancestry check: the parent (height-1) is applied by now,
        // so a mismatched parent_hash means two conflicting blocks share our
        // ancestry — fail-stop instead of executing on a forked history.
        if detect_parent_link_violation(
            &self.state_db,
            height,
            &torus_block.header.parent_hash,
            &self.exec_failed,
        ) {
            return;
        }

        // Exec-ceiling Option A: total timer starts AFTER the skip-check so
        // restart-replayed (already-applied) blocks never pollute the distribution.
        let block_timer = std::time::Instant::now();

        for slash in pending_slashes {
            match self
                .staking
                .slash(slash.validator, slash.fraction_bps, slash.reason, 0)
            {
                Ok(amount) => {
                    tracing::info!(
                        %slash.validator,
                        %amount,
                        reason = ?slash.reason,
                        "flushed buffered slash to DB"
                    );
                }
                Err(e) => {
                    tracing::error!(
                        %slash.validator,
                        %e,
                        "CRITICAL: failed to flush buffered slash"
                    );
                }
            }
            if slash.tombstone {
                if let Err(e) = self.staking.tombstone_validator(&slash.validator) {
                    tracing::error!(
                        %slash.validator,
                        %e,
                        "CRITICAL: failed to tombstone equivocating leader"
                    );
                }
            }
        }

        let has_evm = !torus_block.evm_transactions.is_empty();
        let has_native = !torus_block.native_actions.is_empty();

        tracing::info!(
            height,
            has_evm,
            has_native,
            evm_tx_count = torus_block.evm_transactions.len(),
            native_count = torus_block.native_actions.len(),
            "execution pipeline: executing finalized block"
        );

        // ---- EVM execution ----
        let mut bundle = BundleState::default();
        let mut computed_fee_revenue: u128 = 0;
        if has_evm {
            match self.validator.validate_block_for_catchup(
                torus_block,
                &self.state_db,
                &self.evm_executor,
            ) {
                Ok(validated) => {
                    computed_fee_revenue =
                        torus_bridge::proposer::compute_fee_revenue(&validated.receipts);
                    // T4.1: reuse the validation-time (root, TrieUpdates) pair — computed by the
                    // ONE StateRoot run inside validate_block_for_catchup over this same committed
                    // base — so the commit never recomputes the EVM root for this block.
                    let precomputed_root = validated.evm_root_updates;
                    // Phase A: commit EVM plain state + the hashed mirror + the incremental trie
                    // nodes in ONE atomic batch, so CF_HASHED_*/CF_TRIE_* stay in lockstep with
                    // CF_ACCOUNTS (keeps the incremental root's base correct across restarts/replay).
                    // Falls back to the plain commit if the incremental path errors, so a trie bug
                    // can never halt the chain (the full-scan root stays primary unless the flag is on).
                    match torus_state::incremental::commit_evm_bundle_incremental(
                        &self.state_db,
                        &validated.bundle,
                        precomputed_root,
                    ) {
                        Ok(_root) => {}
                        Err(e) => {
                            tracing::error!(%e, height, "incremental commit failed; falling back to plain EVM commit");
                            if let Err(e2) = BlockCommitter::commit_pending_bundle(
                                &self.state_db,
                                &validated.bundle,
                            ) {
                                tracing::error!(%e2, height, "failed to commit EVM bundle (fallback)");
                            }
                        }
                    }
                    if let Err(e) = BlockCommitter::commit_block_metadata(
                        &self.state_db,
                        torus_block,
                        &validated.receipts,
                    ) {
                        tracing::error!(%e, height, "failed to commit block metadata");
                    }
                    bundle = validated.bundle;
                }
                Err(e) => {
                    tracing::error!(%e, height, "EVM execution failed for committed block");
                }
            }
        } else {
            persist_block_header(&self.state_db, torus_block);
        }

        // ---- Native execution ----
        if has_native || computed_fee_revenue > 0 {
            // One verification pass resolves every sender too (EIP-712 ecrecover or
            // session owner); `None` marks an invalid signature. Reused below so we
            // never recover the same action twice.
            let verify_timer = std::time::Instant::now();
            let resolved_senders = if has_native {
                torus_types::eip712::batch_verify_native_actions_cached(
                    &torus_block.native_actions,
                    torus_block.header.timestamp,
                    |pubkey| self.state_db.get_session(pubkey).ok().flatten(),
                    // Exec trust-cache read (gated by --exec-trust-cache, default
                    // off): when enabled, a HIT reuses a locally-verified sender
                    // (keyed by the signature-committing key) and skips the secp256k1
                    // recover; a MISS — or the flag being off — falls through to the
                    // full recover + slash below.
                    |key| {
                        if self.exec_trust_cache {
                            self.mempool.as_ref().and_then(|m| m.verified_sender(key))
                        } else {
                            None
                        }
                    },
                )
            } else {
                vec![]
            };
            if let Some(ref m) = self.metrics {
                m.exec_verify_seconds
                    .observe(verify_timer.elapsed().as_secs_f64());
            }

            let invalid_count = resolved_senders.iter().filter(|s| s.is_none()).count();
            if invalid_count > 0 {
                tracing::error!(
                    count = invalid_count,
                    proposer = %torus_block.header.proposer,
                    "SLASHING PROPOSER: attested block contained invalid signatures"
                );
                if let Err(e) = self.staking.slash(
                    torus_block.header.proposer,
                    10000,
                    SlashReason::InvalidAttestation,
                    0,
                ) {
                    tracing::error!(%e, "CRITICAL: failed to slash proposer for invalid attestation");
                }
                if let Err(e) = self
                    .staking
                    .tombstone_validator(&torus_block.header.proposer)
                {
                    tracing::error!(%e, "CRITICAL: failed to tombstone proposer for invalid attestation");
                }
            }

            let replay_guard_timer = std::time::Instant::now();
            let mut sender_actions = Vec::with_capacity(torus_block.native_actions.len());
            let mut consumed_nonces = Vec::new();
            // Defense-in-depth replay guard. The non-destructive mempool selection ×
            // HotStuff 3-chain pipeline re-includes the same action in consecutive
            // blocks before the first commits and calls `remove_committed_native`
            // (memory b206f59c / 282f9818). Without this guard each inclusion
            // re-executed — proven on devnet, one TransferToPerp credited 3x. Skip any
            // (sender, nonce) already consumed by a prior committed block — mirrors the
            // sync/catchup check in torus-bridge validator.rs — so each action executes
            // at most once even when included 2-3x. `seen_in_block` also dedups a
            // (sender, nonce) repeated within a single block.
            let mut seen_in_block = std::collections::HashSet::new();
            for (i, signed) in torus_block.native_actions.iter().enumerate() {
                let Some(sender) = resolved_senders.get(i).copied().flatten() else {
                    tracing::error!(index = i, "INVALID SIG in attested block — skipping action");
                    continue;
                };
                let nonce_key = torus_state::cf::native_nonce_key(&sender, signed.nonce);
                let already_committed = self
                    .state_db
                    .get_cf_raw(torus_state::cf::CF_NATIVE_NONCES, &nonce_key)
                    .unwrap_or(None)
                    .is_some();
                if already_committed || !seen_in_block.insert((sender, signed.nonce)) {
                    tracing::warn!(
                        %sender,
                        nonce = signed.nonce,
                        height,
                        "skipping duplicate/replayed native action on live commit path"
                    );
                    continue;
                }
                consumed_nonces.push((sender, signed.nonce));
                sender_actions.push((sender, signed.action.clone()));
            }
            if let Some(ref m) = self.metrics {
                m.exec_replay_guard_seconds
                    .observe(replay_guard_timer.elapsed().as_secs_f64());
                m.native_actions_processed
                    .inc_by(sender_actions.len() as u64);
            }

            let overlay = NativeStateOverlay::new(self.state_db.clone());
            overlay.seed_from_bundle(&bundle);

            let (pre_evm, post_evm) = sort_native_actions(&sender_actions);
            let mut ctx = NativeExecContext::new(
                overlay.clone(),
                torus_block.header.height,
                torus_block.header.timestamp,
                torus_block.header.epoch,
                self.epoch_length,
                self.max_validators,
                torus_block.header.proposer,
                self.treasury_address,
                self.dev_pool_address,
            );
            ctx.metrics = self.metrics.clone();
            // O3: with a background writer present, fills buffer their
            // trade-history KVs (node-local, non-root CFs) instead of paying
            // per-fill overlay PUTs; they are handed over after the flush below.
            ctx.defer_trades = self.trade_writer.is_some();

            let engine_timer = std::time::Instant::now();
            NativeExecutor::execute_batch(&mut ctx, &pre_evm);
            NativeExecutor::execute_batch(&mut ctx, &post_evm);
            // T1.5 FAIL-STOP: a market worker panicked mid-match — its book
            // was consumed and this block's post-state is unreconstructable.
            // Do NOT run the remaining phases, do NOT flush the overlay or
            // mark the block applied; latch the failure so the execution loop
            // halts and consensus stops instead of zombie-advancing.
            if let Some(reason) = ctx.fatal_error.take() {
                tracing::error!(
                    height,
                    %reason,
                    "FATAL: native execution failed — halting execution pipeline (fail-stop)"
                );
                self.exec_failed.store(true, Ordering::SeqCst);
                return;
            }
            let _ = NativeExecutor::drain_core_writer(&mut ctx);
            NativeExecutor::process_governance(&mut ctx);
            NativeExecutor::distribute_fees(&mut ctx, computed_fee_revenue);
            NativeExecutor::process_epoch_boundary(&mut ctx);
            if let Some(ref m) = self.metrics {
                m.exec_engine_seconds
                    .observe(engine_timer.elapsed().as_secs_f64());
            }

            let save_books_timer = std::time::Instant::now();
            ctx.save_order_books();
            if let Some(ref m) = self.metrics {
                m.exec_save_books_seconds
                    .observe(save_books_timer.elapsed().as_secs_f64());
            }

            let flush_timer = std::time::Instant::now();
            for (sender, nonce) in &consumed_nonces {
                let nonce_key = torus_state::cf::native_nonce_key(sender, *nonce);
                let _ = overlay.put_cf_raw(
                    torus_state::cf::CF_NATIVE_NONCES,
                    &nonce_key,
                    &torus_block.header.height.to_be_bytes(),
                );
            }

            // Flush native state, maintain the incremental native bucketed-Merkle trie, AND write the
            // native applied-height marker — all in ONE atomic batch (Phase A A2.2 + T156-F1). The
            // marker fold is the crash-safety fix: native state and "this height is applied" now
            // commit together, so a hard crash can never re-execute this block on restart and
            // double-apply its fee distribution / epoch rewards. A trie-maintenance failure still
            // never drops committed native state (same batch); it only leaves the off-by-default
            // incremental native root stale for this block. If the atomic write itself fails, NEITHER
            // native state nor the marker is written, so replay correctly re-executes on restart.
            if let Err(e) = overlay.flush_with_native_trie_and_marker(&self.state_db, height) {
                tracing::error!(%e, height, "native overlay flush + applied-height marker failed (block NOT marked applied — restart will replay)");
            }

            // Phase A: native post-commit credited EVM account balances (fees / validator rewards)
            // straight to CF_ACCOUNTS via the overlay flush, bypassing incremental trie maintenance.
            // Re-sync those accounts into CF_HASHED_*/CF_TRIE_* so the incremental root keeps tracking
            // the full scan (devnet-smoke finding). Best-effort: a failure only degrades the
            // flag-gated (off-by-default) incremental path, never the committed plain state.
            let native_evm_addrs = overlay.dirty_evm_accounts();
            if let Err(e) =
                torus_state::incremental::resync_evm_accounts(&self.state_db, &native_evm_addrs)
            {
                tracing::error!(%e, height, "failed to resync incremental trie after native post-commit");
            }
            if let Some(ref m) = self.metrics {
                m.exec_flush_seconds
                    .observe(flush_timer.elapsed().as_secs_f64());
            }

            // O3: hand this block's buffered trade-history KVs to the background
            // writer — off the execution thread, after the atomic state flush.
            // Keys are deterministic per block, so a crash-replay rewrite is
            // idempotent; a hard crash can lose the last few queued batches,
            // which is a cosmetic RPC trade-history gap, never consensus state.
            let trades = ctx.take_pending_trades();
            if !trades.is_empty() {
                let fallback = match &self.trade_writer {
                    Some(writer) => writer.send(trades).err(),
                    None => Some(trades),
                };
                // Writer gone (or absent): write synchronously so no rows are lost.
                if let Some(kvs) = fallback {
                    for (cf, key, value) in &kvs {
                        let _ = self.state_db.put_cf_raw(cf, key, value);
                    }
                }
                if let (Some(m), Some(w)) = (&self.metrics, &self.trade_writer) {
                    m.trade_writer_queued_batches.set(w.queued_batches() as i64);
                }
            }
        }

        // ---- Persist block body for RPC queries ----
        if let Ok(body_bytes) = serde_json::to_vec(&torus_block.body()) {
            let _ = self
                .state_db
                .put_cf_raw(CF_BLOCK_BODIES, &height.to_be_bytes(), &body_bytes);
        }

        // ---- Update tracking ----
        // T156-F1: when the native execution path ran (native actions and/or fee revenue), the
        // applied-height marker was already folded into that path's atomic flush batch above
        // (flush_with_native_trie_and_marker), so native state and the marker committed together.
        // Only blocks that skipped the native path entirely (no native actions AND no fee revenue —
        // empty or pure-EVM blocks, whose re-execution is idempotent) still need a standalone marker
        // write here.
        if !(has_native || computed_fee_revenue > 0) {
            write_native_applied_height(&self.state_db, height);
        }

        if let Some(ref m) = self.metrics {
            m.block_height.set(height as i64);
            m.blocks_committed.inc();
            let tx_count = torus_block.header.evm_tx_count as u64
                + torus_block.header.native_action_count as u64;
            m.block_transactions_count.observe(tx_count as f64);
            m.exec_block_seconds
                .observe(block_timer.elapsed().as_secs_f64());
        }

        tracing::info!(height, "execution pipeline: block done");
    }
}

fn execution_loop(rx: std::sync::mpsc::Receiver<CommittedBlockMsg>, ctx: ExecutionContext) {
    tracing::info!("execution pipeline thread started");
    while let Ok(msg) = rx.recv() {
        let CommittedBlockMsg {
            torus_block,
            pending_slashes,
        } = msg;
        let height = torus_block.header.height;
        // T1.5: contain execution panics — a panic here must become a
        // controlled fail-stop (latch + loop exit), not silent thread death
        // with consensus zombie-advancing while state is frozen.
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            ctx.execute_committed_block(&torus_block, pending_slashes)
        }));
        if result.is_err() {
            ctx.exec_failed.store(true, Ordering::SeqCst);
            tracing::error!(height, "execution pipeline PANICKED executing committed block");
        }
        // Paired with the inc() at the `exec_tx.send` site: dec AFTER execution
        // so the gauge counts queued + in-flight blocks (pinned near the channel
        // bound 64 = execution is the bottleneck).
        if let Some(ref m) = ctx.metrics {
            m.exec_queue_depth.dec();
        }
        if ctx.exec_failed.load(Ordering::SeqCst) {
            // Dropping `rx` closes the channel: the consensus thread's next
            // send fails and latches the same fail-stop on the TorusApp side.
            tracing::error!(
                height,
                "execution pipeline halting (FAIL-STOP) — node must stop, not zombie-advance"
            );
            return;
        }
    }
    tracing::info!("execution pipeline thread shutting down");
}

use crate::kv_store::RocksKVStore;

/// Consensus application wired to the execution bridge.
///
/// ## Consensus-then-Execute with Pipelining
///
/// `produce_block`: drains mempool, builds raw tx list, NO execution.
/// `validate_block`: structural + signature checks only, NO execution.
/// `on_committed_block`: sends finalized blocks to the execution pipeline thread.
/// In-flight native-action hashes by height — every proposal seen (ours and
/// other leaders'), INCLUDING compact proposals whose bodies could not be
/// reconstructed (`MissingData`), which `pending_proposals` cannot track
/// because it stores full bodies. Same-height re-proposals union instead of
/// overwrite. Heights are evicted by the same h+10 sliding window as
/// `pending_proposals` and cleared on commit. Closes the duplicate-inclusion
/// tail (factor 1.08–1.27 measured s355, mem c0f4f938) left by
/// body-dependent tracking.
#[derive(Default)]
struct InFlightHashLedger {
    by_height: std::collections::HashMap<u64, std::collections::HashSet<torus_types::B256>>,
}

impl InFlightHashLedger {
    fn note<I: IntoIterator<Item = torus_types::B256>>(&mut self, height: u64, hashes: I) {
        self.by_height.entry(height).or_default().extend(hashes);
        self.by_height.retain(|&h, _| h + 10 > height);
    }

    fn clear(&mut self, height: u64) {
        self.by_height.remove(&height);
    }

    fn extend_into(&self, out: &mut std::collections::HashSet<torus_types::B256>) {
        for hashes in self.by_height.values() {
            out.extend(hashes.iter().copied());
        }
    }
}

pub struct TorusApp {
    #[allow(dead_code)]
    state_db: StateDb,
    #[allow(dead_code)]
    proposer: BlockProposer,
    #[allow(dead_code)]
    validator: BlockValidator,
    #[allow(dead_code)]
    evm_executor: EvmExecutor,
    proposer_address: Address,
    last_header: TorusBlockHeader,
    staking: StakingManager,
    epoch_length: u64,
    max_validators: u32,
    last_validator_set: ValidatorSet,
    cached_vs_updates: Option<(u64, Option<ValidatorSetUpdates>)>,
    pending_slashes: Vec<PendingSlash>,
    pending_proposals: std::collections::HashMap<u64, TorusBlock>,
    in_flight_hashes: InFlightHashLedger,
    #[allow(dead_code)]
    treasury_address: Address,
    #[allow(dead_code)]
    dev_pool_address: Address,
    #[allow(dead_code)]
    metrics: Option<Arc<torus_telemetry::Metrics>>,
    mempool: Option<Arc<Mempool>>,
    #[allow(dead_code)]
    signing_key: Option<ed25519_dalek::SigningKey>,
    exec_tx: Option<SyncSender<CommittedBlockMsg>>,
    exec_handle: Option<JoinHandle<()>>,
    /// T1.5 fail-stop latch, shared with the execution pipeline thread. Once
    /// set (never cleared) the node stops producing, voting, and finalizing —
    /// exec-thread death must be a loud halt, not zombie-advance.
    exec_failed: Arc<AtomicBool>,
    leader_state: Arc<LeaderState>,
    pre_proposal_tx: Option<std::sync::mpsc::SyncSender<PreProposalBundle>>,
    /// RARE pull-fallback transport (Task 6): fetch missing native-action bodies
    /// by-hash on a reconstruction miss. Injected at startup over `/torus/native-da/1.0`;
    /// `None` in consensus-only tests (no fetch fires).
    da_fetcher: Option<Arc<dyn NativeDaFetcher>>,
    /// BS-4a: off-thread recovery of push-missed native-action bodies. The hot
    /// validate path hands true push misses here (non-blocking) and fails the
    /// view with MissingData; the worker pulls with the roomier ~1 s WORKER
    /// budget so the re-proposed view finds the bodies durably local. `None`
    /// when the mempool or fetcher is absent (consensus-only tests) — a miss
    /// then fails the view without a handoff, same as before BS-4a.
    da_recovery: Option<DaRecoveryWorker>,
    /// Regime-B strict-order execution. The next committed height the execution
    /// pipeline must receive. Blocks are handed to `exec_tx` STRICTLY in
    /// ascending height with NO gaps: native matching is order-dependent, so
    /// executing H+1 before H produces a different state than peers and an
    /// interior hole would be PERMANENT divergence. `None` until the first
    /// committed block seeds it (from the applied marker, else that height).
    exec_next_height: Option<u64>,
    /// Regime-B. Committed blocks that arrived at/above the frontier while an
    /// earlier height was still an unhealed hole — buffered here, keyed by
    /// height, and drained in ascending order once the hole fills. Empty in the
    /// healthy in-order case (the hot path bypasses it entirely).
    deferred_exec: std::collections::BTreeMap<u64, DeferredExecBlock>,
    /// Regime-B. When the current head-of-line execution hole was first detected
    /// (heal budget + throttled-log clock). `None` when there is no hole.
    exec_hole_since: Option<std::time::Instant>,
    /// Regime-B. Last time the loud "execution stalled" warning fired (throttle).
    exec_hole_last_log: Option<std::time::Instant>,
    /// Regime-B. Retry attempts against the current hole (for the operator log).
    exec_hole_retries: u64,
}

/// Actions the proposer pushes to validators via unicast before broadcasting CompactBlock.
pub struct PreProposalBundle {
    pub actions: Vec<(Address, torus_types::SignedNativeAction)>,
}

/// RARE pull-fallback transport for native-action DA bodies (Phase C Task 6).
///
/// Injected from torus-node over the libp2p `/torus/native-da/1.0` protocol so
/// torus-consensus needn't depend on torus-network. Used only on a reconstruction
/// MISS — push covers the common case, so this fires rarely (mem a6cf33a9: pull is
/// fallback-only, never per-block).
///
/// `fetch` is **non-blocking** (it only enqueues a request to the network thread).
/// `drain` returns the bodies that have arrived since the last call (each =
/// `bincode(SignedNativeAction)`). Target selection (which peers to ask) is the
/// implementation's concern, keeping the consensus layer free of peer identity.
pub trait NativeDaFetcher: Send + Sync {
    /// Request the given action-hashes from peers. Non-blocking.
    fn fetch(&self, hashes: Vec<[u8; 32]>);
    /// Drain native-action bodies received so far. Non-blocking.
    fn drain(&self) -> Vec<Vec<u8>>;
}

/// CONSENSUS-CRITICAL version flag: emit hash-only `CompactBlock` proposals
/// (`true`) instead of full self-contained `TorusBlock`s (`false`).
///
/// The consensus block identity is `data_hash` over the datum bytes, so compact
/// and full encodings hash DIFFERENTLY — every validator MUST agree on this value
/// or they split. It is therefore tied to the BINARY version (not env/per-node):
/// flip it only in a coordinated relaunch where every validator runs the fixed
/// binary (Phase C Task 9). Re-enabling compact is what unlocks 400k orders/sec — a
/// block referencing ~20–40k orders ≈ 1–2 MB of bodies far exceeds the 256 KB
/// `max_consensus_message_size`, so bodies must travel out-of-band (durable DA
/// store + push T7 + rare pull T6). Default `false` keeps the proven full-block
/// path until that coordinated flip. Does NOT affect the EVM/RPC header hash
/// (`keccak256(canonical_header_bytes)`), which is identical in both encodings.
///
/// ENABLED (Task 9): all validators MUST run this binary in a coordinated relaunch.
/// Deploying it to a SUBSET while others run a full-block binary splits consensus
/// (different `data_hash` for the same block). Do NOT deploy piecemeal.
const COMPACT_PROPOSALS: bool = true;

/// Encode the datum carried in a consensus proposal.
///
/// With `compact = false` (default): the FULL, self-contained `TorusBlock` (native
/// actions inline) — needs no out-of-band delivery and is self-contained for
/// block-sync. The proven full-block path (06620a1) that ran healthy at cap-100.
///
/// With `compact = true` (version-gated, Task 8): a hash-only `CompactBlock`. The
/// bodies are NOT inline — they are mirrored to the durable DA store (T2), pushed
/// to validators (T7), and fetched on a miss via the rare pull-fallback (T6). This
/// shrinks the proposal so it fits `max_consensus_message_size` at 400k orders/sec.
/// All validators must emit the SAME encoding (see [`COMPACT_PROPOSALS`]).
fn encode_proposal_datum(block: &TorusBlock, compact: bool) -> Vec<u8> {
    if compact {
        bincode::serialize(&CompactBlock::from_block(block)).expect("serialize CompactBlock")
    } else {
        bincode::serialize(block).expect("serialize TorusBlock")
    }
}

/// Pull-fallback poll budget for [`TorusApp::pull_missing_bodies`]: drain-and-absorb
/// is retried `PULL_RETRIES` times, sleeping `PULL_DELAY` between attempts, so the
/// effective wait = `PULL_RETRIES * PULL_DELAY` = ~1 s. This runs ONLY on the
/// block-sync path (never the consensus voting hot path), so blocking up to ~1 s is
/// safe — and necessary: the original 80 ms (4 × 20 ms) gave up before a >4 MB
/// chunked body-set could land, contributing to the wedge (native-DA fix Task 3).
const PULL_RETRIES: usize = 20;
const PULL_DELAY: std::time::Duration = std::time::Duration::from_millis(50);

/// Ceiling for the size-aware sync pull budget: 160 × 50 ms = 8 s. Large
/// batched blocks carry multi-MB body-sets that cannot land within the flat
/// ~1 s budget on WAN links (s334 bs1000); the sync path may safely block
/// longer because it is off the consensus voting path.
const MAX_SYNC_PULL_RETRIES: usize = 160;

/// Sync-path pull budget scaled by how many bodies are missing (~2 bodies per
/// 50 ms tick), clamped to [`PULL_RETRIES`, `MAX_SYNC_PULL_RETRIES`] (~1–8 s).
fn sync_pull_retries(missing: usize) -> usize {
    (missing / 2).clamp(PULL_RETRIES, MAX_SYNC_PULL_RETRIES)
}

/// Shared bounded fetch-wait-absorb loop: used by the SYNC path (in-line, ~1-8s)
/// and the BS-4a recovery worker (off-thread, ~1s). Wake-on-arrival (S391),
/// deadline-bounded (S395), with ONE event-driven mid-budget re-fetch (BS-4b).
fn recover_bodies_bounded(
    mempool: &Mempool,
    fetcher: &dyn NativeDaFetcher,
    missing: &[torus_types::B256],
    retries: usize,
    delay: std::time::Duration,
    metrics: Option<&torus_telemetry::Metrics>,
    cancel: Option<&std::sync::atomic::AtomicBool>,
) -> bool {
    if missing.is_empty() {
        return true;
    }

    // Pre-warm fast path (review F2): a body pushed-as-hash may already be sitting in the
    // fetcher inbound (the receiver pre-warm-pulled it) but not yet absorbed — e.g. it
    // landed just after the hot local-retry's last drain. Absorb + re-check BEFORE issuing
    // a network fetch, so a pre-warmed (or late) body never triggers a redundant fetch.
    TorusApp::absorb_fetched_bodies(mempool, fetcher);
    if missing.iter().all(|h| mempool.get_native_da(h).is_some()) {
        return true;
    }

    let hashes: Vec<[u8; 32]> = missing.iter().map(|h| h.0).collect();
    // S391 wake-on-arrival: snapshot before firing the fetch so a push that
    // races the pull wakes the first wait. Pull responses themselves land in
    // the fetcher inbound and still need this thread's absorb, so the slices
    // keep the old `delay` cadence as the worst case.
    let mut seen = torus_state::NativeDaStore::arrival_generation();
    fetcher.fetch(hashes);

    if let Some(m) = metrics {
        m.native_da_pull_requests.inc();
    }

    // Deadline-bounded loop (S395): a loaded scheduler overshoots each individual
    // wait slice, and with per-ITERATION bounds those overshoots stack (13 x 20ms
    // nominal was observed at 500ms+ wall under load). Bounding by wall-clock
    // deadline keeps the total budget honest regardless of load.
    let budget = delay * retries as u32;
    let deadline = std::time::Instant::now() + budget;
    // BS-4b: one event-driven mid-budget re-fetch (fires once at `refetch_at`).
    let refetch_at = deadline - budget / 2;
    let mut refetched = false;
    loop {
        // BS-4a worker shutdown: abandon the in-flight budget the moment the caller
        // cancels — a dying node has no use for the bodies (a dropped pull just
        // re-fires on the re-proposed view). Bounds Drop-join to ~one slice + O(1).
        // The SYNC caller passes None, so its behavior is bit-identical.
        if cancel.is_some_and(|c| c.load(std::sync::atomic::Ordering::Relaxed)) {
            return false;
        }
        let now = std::time::Instant::now();
        if now >= deadline {
            break;
        }
        torus_state::NativeDaStore::wait_for_arrival(seen, std::cmp::min(delay, deadline - now));
        TorusApp::absorb_fetched_bodies(mempool, fetcher);
        if missing.iter().all(|h| mempool.get_native_da(h).is_some()) {
            if let Some(m) = metrics {
                m.native_da_pull_recovered.inc();
            }
            tracing::info!(count = missing.len(), "native-da pull: bodies recovered");
            return true;
        }
        // BS-4b: one event-driven mid-budget re-fetch — covers a lost pull request or
        // response (the fan-out already rotates validators, bridge.rs:390). Fires at
        // most once, and only for bodies STILL absent at the midpoint, so an already-
        // absorbed body is never re-requested (pull stays rare, mem a6cf33a9). Placed
        // after the absorb+recheck so the still-missing filter sees fresh state, and
        // unreachable when the body arrived earlier (the return above already fired).
        if !refetched && std::time::Instant::now() >= refetch_at {
            let still: Vec<[u8; 32]> = missing
                .iter()
                .filter(|h| mempool.get_native_da(h).is_none())
                .map(|h| h.0)
                .collect();
            if !still.is_empty() {
                fetcher.fetch(still);
            }
            refetched = true;
        }
        // Fold in our own absorb puts so they don't self-wake the next wait.
        seen = torus_state::NativeDaStore::arrival_generation();
    }
    tracing::warn!(
        count = missing.len(),
        "native-da pull: bodies NOT recovered within budget"
    );
    false
}

/// BS-4a recovery-worker pull budget: 50 × 20 ms = ~1 s, sync-parity (matches the
/// flat `PULL_RETRIES * PULL_DELAY` sync budget). OPEN QUESTION: 1 s (sync parity)
/// vs 2 s (cover a slow re-propose cycle so the re-proposed view is guaranteed to
/// find the body locally) — start at sync parity, tune on devnet. The worker runs
/// OFF the consensus thread, so widening this later costs no hot-path latency.
const WORKER_PULL_RETRIES: usize = 50;
const WORKER_PULL_DELAY: std::time::Duration = std::time::Duration::from_millis(20);

/// BS-4a: owns the off-thread recovery of push-missed native-action bodies.
/// The consensus thread hands missing hashes here and votes MissingData
/// immediately; this thread runs the (event-driven, deadline-bounded) pull
/// loop with the roomier WORKER budget, so the re-proposed view finds the
/// bodies locally. Sender-drop => recv Err => thread exits (O3 pattern).
///
/// `tx` is an `Option` (sanctioned deviation from the bare-`Sender` sketch):
/// `Drop` must close the channel BEFORE joining, and the only way to drop a
/// field early is `Option::take` — same shape as the O3 `BackgroundCfWriter`
/// (torus-state/src/bg_writer.rs).
struct DaRecoveryWorker {
    tx: Option<std::sync::mpsc::SyncSender<Vec<torus_types::B256>>>,
    handle: Option<std::thread::JoinHandle<()>>,
    shutdown: Arc<std::sync::atomic::AtomicBool>,
}

/// Bounded in-flight batch queue. Under a sustained-miss / partition regime,
/// submits arrive at view cadence (~2/s) while the worker drains at ~1/s (each
/// unrecoverable batch burns the full ~1 s budget), so an UNBOUNDED channel would
/// grow without limit AND make shutdown drain block queue_depth × ~1 s. Capping at
/// 8 bounds both: memory (≤ 8 queued batches) and shutdown latency (see `Drop`).
/// Since Fix A the worker COALESCES all pending batches into one cycle (drains up
/// to cap+1 per ~1 s), so a full queue is pathological-only and dropping a Full
/// try_send is truly harmless — the re-proposed view resubmits those hashes, which
/// get coalesced into the next cycle rather than starving behind the backlog.
const WORKER_QUEUE_CAP: usize = 8;

impl DaRecoveryWorker {
    /// Spawn the recovery thread. It owns clones of the mempool/fetcher Arcs, so
    /// it needs nothing from `TorusApp` and stays alive until the channel closes.
    fn spawn(
        mempool: Arc<Mempool>,
        fetcher: Arc<dyn NativeDaFetcher>,
        metrics: Option<Arc<torus_telemetry::Metrics>>,
    ) -> Self {
        let (tx, rx) = std::sync::mpsc::sync_channel::<Vec<torus_types::B256>>(WORKER_QUEUE_CAP);
        let shutdown = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let worker_shutdown = shutdown.clone();
        let handle = std::thread::Builder::new()
            .name("torus-da-recovery".into())
            .spawn(move || {
                // recv() yields queued batches even after the sender is dropped,
                // then errors — the shutdown flag below turns that drain into an
                // early exit so a dying node never spends budget on stale work.
                while let Ok(mut batch) = rx.recv() {
                    // Shutdown wins immediately: queued recovery work is worthless
                    // on a node that is going down. Unlike O3's DB writes there is
                    // NO data-loss concern here (a dropped pull just re-fires on the
                    // re-proposed view), so drain-on-shutdown is intentionally NOT
                    // wanted — bail before spending a ~1 s budget on a dying node.
                    if worker_shutdown.load(std::sync::atomic::Ordering::Relaxed) {
                        break;
                    }
                    // BS-4a Fix A (coalesce): drain everything else pending into THIS
                    // cycle before the dedup, so a fresh recoverable batch never
                    // starves behind a queue of dead ones (each of which would burn
                    // the full ~1 s budget FIFO). The queue drains in gulps of up to
                    // cap+1 batches per ~1 s cycle, so a Full try_send is now
                    // pathological-only and the drop-is-harmless claim holds: a
                    // dropped resubmit is coalesced into the next cycle.
                    while let Ok(more) = rx.try_recv() {
                        batch.extend(more);
                    }
                    // Dedup the merged set: the same hash resubmitted across views (or
                    // shared between coalesced batches) would otherwise appear multiple
                    // times — recover_bodies_bounded iterates `missing` per tick, so
                    // duplicates are wasteful, not harmful. Then drop hashes already
                    // durably local (late push, pre-warm, an earlier merged batch) so
                    // we never fire a redundant network fetch.
                    batch.sort_unstable();
                    batch.dedup();
                    let still_missing: Vec<torus_types::B256> = batch
                        .into_iter()
                        .filter(|h| mempool.get_native_da(h).is_none())
                        .collect();
                    if still_missing.is_empty() {
                        continue;
                    }
                    // A recoverable hash merged with never-arriving (dead) hashes is
                    // still recovered mid-loop: absorb_fetched_bodies stores each body
                    // the moment it arrives, even though the batch's overall return is
                    // false when the dead hashes never land. `Some(&worker_shutdown)`
                    // lets Drop abandon the in-flight budget at the next slice (Fix C).
                    let recovered = recover_bodies_bounded(
                        &mempool,
                        fetcher.as_ref(),
                        &still_missing,
                        WORKER_PULL_RETRIES,
                        WORKER_PULL_DELAY,
                        metrics.as_deref(),
                        Some(&worker_shutdown),
                    );
                    // A cancelled in-flight batch (shutdown) is not a budget timeout —
                    // only count as a timeout when the budget genuinely expired without
                    // recovering every body and we are NOT shutting down.
                    if !recovered && !worker_shutdown.load(std::sync::atomic::Ordering::Relaxed) {
                        if let Some(ref m) = metrics {
                            m.native_da_recovery_timeouts.inc();
                        }
                    }
                }
            })
            .expect("spawn torus-da-recovery thread");
        Self {
            tx: Some(tx),
            handle: Some(handle),
            shutdown,
        }
    }

    /// Hand a batch of missing hashes to the worker. Non-blocking (`try_send`):
    /// the consensus thread calls this, so it must NEVER block or panic. On a full
    /// queue (recovery already saturated) or a dead worker the batch is dropped with
    /// a warning — harmless, the re-proposed view resubmits the same hashes.
    ///
    /// Returns `true` iff the batch was actually queued, so the caller can gate the
    /// `native_da_recovery_handoffs` counter on real handoffs — a dead worker must
    /// not paint the A/B dashboard green (final-review finding 1).
    fn submit(&self, hashes: Vec<torus_types::B256>) -> bool {
        use std::sync::mpsc::TrySendError;
        let Some(tx) = &self.tx else {
            tracing::warn!("da-recovery worker: sender already closed; dropping batch");
            return false;
        };
        match tx.try_send(hashes) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) => {
                tracing::warn!("da-recovery worker queue full; dropping missing-hash batch");
                false
            }
            Err(TrySendError::Disconnected(_)) => {
                tracing::warn!("da-recovery worker thread gone; dropping missing-hash batch");
                false
            }
        }
    }
}

impl Drop for DaRecoveryWorker {
    fn drop(&mut self) {
        // Signal shutdown BEFORE closing the channel: the worker checks the flag
        // right after each recv() Ok and bails, so batches still queued (≤
        // WORKER_QUEUE_CAP) are DISCARDED in O(1), not drained (no data-loss concern
        // here — dropped pulls re-fire on the re-proposed view). Since Fix C the
        // in-flight batch inside recover_bodies_bounded also observes this flag
        // (passed as `cancel`) at the top of its wait loop, so it abandons its
        // budget at the NEXT ~20 ms slice boundary instead of running the full
        // ~1 s. Join is therefore bounded by ~one slice + O(1). Closing tx then
        // unblocks a worker parked in recv().
        self.shutdown
            .store(true, std::sync::atomic::Ordering::Relaxed);
        drop(self.tx.take());
        if let Some(h) = self.handle.take() {
            // A worker panic must not propagate out of Drop — log, never unwrap.
            if h.join().is_err() {
                tracing::warn!("da-recovery worker thread panicked");
            }
        }
    }
}

/// Bounded LOCAL retry on the hot validate path (BS-4a): ONE 20 ms wake-on-arrival
/// slice, just enough to catch the racing pre-proposal PUSH that lands right after
/// the CompactBlock (the common case; keeps recovery rare). OPEN QUESTION: the
/// racing-push window distribution is unmeasured — measure on devnet before
/// finalizing the consts (1 vs 2 slices). The old 5 × 20 ms in-line budget moved
/// off-thread to the recovery worker (`WORKER_PULL_RETRIES`/`WORKER_PULL_DELAY`).
const RECONSTRUCT_RETRIES: usize = 1;
const RECONSTRUCT_RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(20);

impl TorusApp {
    pub fn new(
        state_db: StateDb,
        config: &ChainConfig,
        metrics: Option<Arc<torus_telemetry::Metrics>>,
        mempool: Option<Arc<Mempool>>,
        signing_key: Option<ed25519_dalek::SigningKey>,
    ) -> Self {
        let staking = StakingManager::new(state_db.clone());
        let mut proposer = BlockProposer::new(
            config.chain_id,
            config.epoch_length,
            config.max_validators,
            config.treasury_address,
            config.dev_pool_address,
        );
        proposer.metrics = metrics.clone();
        let mut validator = BlockValidator::new(
            config.chain_id,
            config.epoch_length,
            config.max_validators,
            config.treasury_address,
            config.dev_pool_address,
        );
        validator.metrics = metrics.clone();
        let genesis_validator_set =
            EpochManager::compute_new_validator_set(&staking, config.max_validators, 0)
                .unwrap_or_else(|_| ValidatorSet {
                    validators: vec![],
                    epoch: 0,
                });

        // T1.5 fail-stop latch, shared between the execution pipeline thread
        // and the consensus-side TorusApp (see `exec_failed` field docs).
        let exec_failed = Arc::new(AtomicBool::new(false));

        // Execution pipeline context — owns its own copies for thread safety.
        let mut exec_validator = BlockValidator::new(
            config.chain_id,
            config.epoch_length,
            config.max_validators,
            config.treasury_address,
            config.dev_pool_address,
        );
        exec_validator.metrics = metrics.clone();
        let exec_ctx = ExecutionContext {
            state_db: state_db.clone(),
            validator: exec_validator,
            evm_executor: EvmExecutor::new(config.chain_id),
            staking: StakingManager::new(state_db.clone()),
            epoch_length: config.epoch_length,
            max_validators: config.max_validators,
            treasury_address: config.treasury_address,
            dev_pool_address: config.dev_pool_address,
            metrics: metrics.clone(),
            mempool: mempool.clone(),
            exec_trust_cache: config.exec_trust_cache,
            // O3: 256 queued blocks of trade KVs max — a full queue blocks the
            // execution thread (backpressure) instead of ballooning memory.
            trade_writer: Some(torus_state::BackgroundCfWriter::spawn(
                state_db.clone(),
                "torus-trade-writer",
                256,
            )),
            exec_failed: exec_failed.clone(),
        };

        // Phase A: ensure the persistent incremental trie exists before any commit (including
        // replay below). No-op after the first boot; keeps the incremental root's base ready while
        // the full-scan root stays primary until TORUS_INCREMENTAL_STATE_ROOT is enabled.
        if let Err(e) = torus_state::incremental::ensure_trie_built(&state_db) {
            tracing::warn!(%e, "failed to build initial state trie (incremental root unavailable until rebuilt)");
        }
        // Phase A A2.2: same for the native bucketed-Merkle trie. No-op after first boot; keeps the
        // incremental native root's base ready while the full-scan native root stays primary until
        // TORUS_INCREMENTAL_STATE_ROOT is enabled.
        if let Err(e) = torus_state::native_trie::ensure_native_trie_built(&state_db) {
            tracing::warn!(%e, "failed to build initial native trie (incremental native root unavailable until rebuilt)");
        }

        // Crash recovery runs synchronously before spawning the pipeline. FIX 1b:
        // an unresolvable-at-boot gap yields `parked_hole = Some(h)` instead of a
        // fail-stop latch — execution parks at `h` and the live heal loop + backfill
        // get a bounded window to close it (see the `exec_next_height` /
        // `exec_hole_since` wiring in the struct literal below).
        let (last_header, parked_hole) = Self::replay_committed(&state_db, &exec_ctx);

        // Spawn execution pipeline: bounded channel (64 blocks) for backpressure.
        let (exec_tx, exec_rx) = std::sync::mpsc::sync_channel(64);
        let exec_handle = std::thread::Builder::new()
            .name("torus-execution".into())
            .spawn(move || execution_loop(exec_rx, exec_ctx))
            .expect("spawn execution pipeline thread");

        let leader_state = Arc::new(LeaderState::new());
        leader_state.sync_validators(&genesis_validator_set);
        leader_state.set_view(last_header.height.saturating_add(1));

        let mut app = Self {
            state_db,
            proposer,
            validator,
            evm_executor: EvmExecutor::new(config.chain_id),
            proposer_address: signing_key.as_ref().and_then(|sk| {
                let pubkey = sk.verifying_key();
                staking.find_validator_by_pubkey(pubkey.as_bytes())
                    .ok()
                    .flatten()
                    .map(|v| {
                        tracing::info!(address = %v.address, "resolved proposer address from signing key");
                        v.address
                    })
            }).unwrap_or(Address::ZERO),
            last_header,
            staking,
            epoch_length: config.epoch_length,
            max_validators: config.max_validators,
            last_validator_set: genesis_validator_set,
            cached_vs_updates: None,
            pending_slashes: Vec::new(),
            pending_proposals: std::collections::HashMap::new(),
            in_flight_hashes: InFlightHashLedger::default(),
            treasury_address: config.treasury_address,
            dev_pool_address: config.dev_pool_address,
            signing_key,
            metrics,
            mempool,
            exec_tx: Some(exec_tx),
            exec_handle: Some(exec_handle),
            exec_failed,
            leader_state,
            pre_proposal_tx: None,
            da_fetcher: None,
            da_recovery: None,
            exec_next_height: None,
            deferred_exec: std::collections::BTreeMap::new(),
            exec_hole_since: None,
            exec_hole_last_log: None,
            exec_hole_retries: 0,
        };

        // FIX 1b: a boot replay hole PARKS instead of latching a pre-network
        // fail-stop. Seed the strict-order execution queue with the committed
        // heights [hole..=committed] as durable-reload sources, park
        // `exec_next_height` at the hole, and start the heal-budget clock now so
        // the live heal loop + block-sync backfill get the SAME bounded window a
        // live hole gets. The node completes boot and rejoins consensus; the hole
        // heals when its body becomes durable (FIX 1a) or a peer re-delivers the
        // block, and fail-stops ONLY if the budget expires (see `note_exec_hole`).
        if let Some(hole) = parked_hole {
            let committed = find_last_committed_height(&app.state_db).unwrap_or(hole);
            for h in hole..=committed {
                // FIX (heal channel): seed each parked height from its durable
                // COMMIT MANIFEST when present — a `CompactBlock` source whose
                // reconstruction yields the REAL missing `native_action_hashes`, so
                // `note_exec_hole` can pull those bodies from peers. Only a legacy
                // record with no manifest falls back to `Durable(h)` (local-body
                // heal only). This is the difference between a hole that heals and
                // one that stalls forever at `missing=0` (t12-r3-full h1165).
                app.deferred_exec.insert(
                    h,
                    DeferredExecBlock {
                        source: recovery_exec_source(&app.state_db, h),
                        slashes: Vec::new(),
                    },
                );
            }
            app.exec_next_height = Some(hole);
            app.exec_hole_since = Some(std::time::Instant::now());
            tracing::warn!(
                hole_height = hole,
                committed_height = committed,
                parked = committed.saturating_sub(hole) + 1,
                "FIX 1b: boot replay hole PARKED (not fail-stopped) — execution queued at the hole; \
                 live heal loop + block-sync backfill will close it within the heal budget"
            );
        }

        app
    }

    pub fn leader_state(&self) -> Arc<LeaderState> {
        self.leader_state.clone()
    }

    /// T1.5: true once the execution pipeline is dead (panic, fatal error, or
    /// closed channel) — the node is in fail-stop and must not produce, vote,
    /// or finalize.
    fn is_exec_failed(&self) -> bool {
        self.exec_failed.load(Ordering::SeqCst)
    }

    /// T1.5: shared fail-stop latch for the node binary to watch (terminate or
    /// alert once the execution pipeline has died).
    pub fn exec_failed_handle(&self) -> Arc<AtomicBool> {
        self.exec_failed.clone()
    }

    pub fn set_pre_proposal_tx(&mut self, tx: std::sync::mpsc::SyncSender<PreProposalBundle>) {
        self.pre_proposal_tx = Some(tx);
    }

    /// Attach the RARE pull-fallback transport (Task 6). Called once at startup with
    /// a handle over the `/torus/native-da/1.0` protocol.
    ///
    /// BS-4a: when a mempool (durable DA store) is wired too, this also spawns the
    /// off-thread `DaRecoveryWorker` over the same fetcher. Calling this again
    /// replaces the worker — the assignment drops the previous one, whose `Drop`
    /// joins the thread (bounded ≤ ~1 s), so no thread leaks. With no mempool the
    /// fetcher is stored but the worker stays `None`: a hot-path miss then fails
    /// the view without a handoff, same as the pre-BS-4a no-mempool behavior.
    pub fn set_native_da_fetcher(&mut self, fetcher: Arc<dyn NativeDaFetcher>) {
        if let Some(ref mempool) = self.mempool {
            self.da_recovery = Some(DaRecoveryWorker::spawn(
                mempool.clone(),
                fetcher.clone(),
                self.metrics.clone(),
            ));
        }
        self.da_fetcher = Some(fetcher);
    }

    /// Create a stub `TorusApp` without a database (for consensus-only tests).
    pub fn stub() -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("torus-stub-{}-{}", std::process::id(), id));
        let _ = std::fs::create_dir_all(&dir);
        let state_db = StateDb::open(&dir).expect("open stub state db");
        let config = ChainConfig {
            chain_id: TORUS_CHAIN_ID,
            chain_name: "torus-test".to_string(),
            evm_gas_limit: 30_000_000,
            base_fee_per_gas: 1_000_000_000,
            epoch_length: 100,
            max_validators: 4,
            min_stake: torus_economics::MIN_SELF_DELEGATION,
            fee_burn_bps: 1000,
            fee_validator_bps: 0,
            fee_treasury_bps: 4500,
            fee_dev_pool_bps: 4500,
            treasury_address: Address::ZERO,
            dev_pool_address: Address::ZERO,
            timeout_base_ms: 500,
            reputation_leader_selection: false,
            exec_trust_cache: false,
        };
        let mut seed = [0u8; 32];
        seed[..8].copy_from_slice(&id.to_le_bytes());
        let signing_key = ed25519_dalek::SigningKey::from_bytes(&seed);
        Self::new(state_db, &config, None, None, Some(signing_key))
    }

    /// Crash recovery: replay committed blocks whose execution was interrupted.
    ///
    /// Returns `(last_good_header, parked_hole)`. `parked_hole` is `Some(h)` when a
    /// committed height in the gap could NOT be reconstructed locally (FIX 1b): the
    /// caller parks execution at `h` and lets the live heal loop + block-sync
    /// backfill get a bounded window to re-deliver and heal it, fail-stopping only
    /// if the budget expires — instead of latching the fail-stop pre-network.
    fn replay_committed(
        state_db: &StateDb,
        exec_ctx: &ExecutionContext,
    ) -> (TorusBlockHeader, Option<u64>) {
        let genesis = torus_bridge::genesis_parent_header();

        let committed = match find_last_committed_height(state_db) {
            Some(h) if h > 0 => h,
            _ => return (genesis, None),
        };

        let applied = read_native_applied_height(state_db).unwrap_or(0);
        if applied >= committed {
            return (genesis, None);
        }

        tracing::warn!(
            committed_height = committed,
            applied_height = applied,
            gap = committed - applied,
            "crash recovery: execution gap detected, replaying"
        );

        // T156-F2: replay the ENTIRE gap [applied+1, committed] in order — not just the single top
        // committed block. If any height in the gap cannot be reconstructed, `replay_gap` stops at it
        // WITHOUT marking it applied, and we latch the existing fail-stop (`exec_failed`) so the node
        // aborts boot instead of running on with a silently-diverged state (was: silent mark-applied).
        let outcome = replay_gap(
            applied,
            committed,
            |height| load_replay_header(state_db, height),
            |height| load_replay_body(state_db, height),
            |block| exec_ctx.execute_committed_block(block, vec![]),
        );

        match outcome {
            ReplayGapOutcome::Complete(last_header) => (last_header, None),
            ReplayGapOutcome::Hole { height, last_good } => {
                // FIX 1b: do NOT latch exec_failed / die pre-network. Park at the
                // hole and let boot complete; the live strict-order heal loop +
                // block-sync backfill get a bounded window (the same
                // `exec_hole_failstop_budget`, started at boot in `new`) to
                // re-deliver the block and heal it. Fail-stop ONLY if the budget
                // expires with the hole still open. Preserves the invariants of the
                // old latch: never execute out of order (applied stays below the
                // hole), never mark an unexecuted height applied, loud logging.
                tracing::error!(
                    hole_height = height,
                    committed_height = committed,
                    applied_height = applied,
                    "crash recovery: execution gap could not be fully replayed LOCALLY — parking \
                     execution at the hole (FIX 1b) and deferring to the live heal + backfill \
                     window; the node will fail-stop if the hole is not healed within the budget"
                );
                (last_good, Some(height))
            }
        }
    }

    /// Reconstruct a `CompactBlock`'s full body set from the durable DA store.
    ///
    /// Returns `Ok(TorusBlock)` when every referenced native-action body is present
    /// in the DA store, or `Err(missing_hashes)` listing the bodies that must be
    /// fetched (the rare pull-fallback, Task 6). Reads the DURABLE store -- NOT the
    /// ephemeral mempool -- so a block-referenced body survives the 60s nonce
    /// window, pool eviction, and a restart (livelock root cause, mem 28e1a821).
    fn reconstruct_compact_from_da(
        &self,
        compact: &CompactBlock,
    ) -> Result<TorusBlock, Vec<torus_types::B256>> {
        let mut native_actions = Vec::with_capacity(compact.native_action_hashes.len());
        let mut missing = Vec::new();
        if !compact.native_action_hashes.is_empty() {
            match self.mempool {
                Some(ref mempool) => {
                    for hash in &compact.native_action_hashes {
                        match mempool.get_native_da(hash) {
                            Some(action) => native_actions.push(action),
                            None => missing.push(*hash),
                        }
                    }
                }
                None => missing = compact.native_action_hashes.clone(),
            }
        }
        if !missing.is_empty() {
            return Err(missing);
        }
        Ok(TorusBlock {
            header: compact.header.clone(),
            native_actions,
            evm_transactions: compact.evm_transactions.clone(),
            core_writer_actions: compact.core_writer_actions.clone(),
        })
    }

    /// RARE pull-fallback entry point (Task 6): if any of `compact`'s native-action
    /// bodies are absent from the durable DA store, fetch them by-hash and absorb the
    /// response into the store. Returns `true` iff a fetch was issued.
    ///
    /// Does NOTHING when every body is already local — the common case, so pull fires
    /// rarely (mem a6cf33a9: pull is fallback-only, never per-block). Intended for the
    /// SYNC path (`validate_block_for_sync`); the hot `validate_block` path keeps only
    /// its bounded local retry and never triggers a network fetch (mem 8ee99db3).
    pub fn pull_compact_bodies_if_missing(&self, compact: &CompactBlock) -> bool {
        let missing = match self.reconstruct_compact_from_da(compact) {
            Ok(_) => return false, // all bodies local — no fetch (rarity)
            Err(missing) => missing,
        };
        // A miss: attempt the bounded pull (recovery is re-checked by the caller's
        // subsequent reconstruct/validate). Report that a fetch was issued.
        self.pull_missing_bodies(&missing);
        true
    }

    /// Sync-path wrapper over [`Self::pull_missing_bodies_bounded`] with the ~1 s budget
    /// (`PULL_RETRIES` × `PULL_DELAY`). Safe to block here — the block-sync path is off
    /// the consensus voting hot path (mem 8ee99db3). The earlier 80 ms budget gave up
    /// before a >4 MB body-set (now pulled in chunks) could arrive; ~1 s recovers a late
    /// body.
    fn pull_missing_bodies(&self, missing: &[torus_types::B256]) -> bool {
        self.pull_missing_bodies_bounded(missing, sync_pull_retries(missing.len()), PULL_DELAY)
    }

    /// Fetch the `missing` native-action bodies by-hash and absorb any that arrive into
    /// the durable DA store, polling `retries` times with `delay` between drains. Returns
    /// `true` once all are present (or were already), `false` on timeout.
    ///
    /// The budget is the caller's — and since BS-4a the only caller is the SYNC path,
    /// with the ~1 s `PULL_RETRIES`/`PULL_DELAY` (safe to block off the voting path).
    /// The HOT validate path no longer pulls in-line; its off-thread recovery worker
    /// calls `recover_bodies_bounded` directly with the WORKER budget.
    fn pull_missing_bodies_bounded(
        &self,
        missing: &[torus_types::B256],
        retries: usize,
        delay: std::time::Duration,
    ) -> bool {
        if missing.is_empty() {
            return true; // nothing to recover, even with no transport wired
        }
        let (Some(fetcher), Some(mempool)) = (self.da_fetcher.as_ref(), self.mempool.as_ref())
        else {
            return false; // no transport/store wired (consensus-only tests)
        };
        recover_bodies_bounded(
            mempool,
            fetcher.as_ref(),
            missing,
            retries,
            delay,
            self.metrics.as_deref(),
            None, // SYNC path is uncancellable — bit-identical to pre-BS-4a
        )
    }

    /// Reconstruct a CompactBlock's native-action bodies for the HOT validate path
    /// (BS-4a). Bodies travel out-of-band (proposer PUSH → durable DA store), so a
    /// CompactBlock can reference a body the local store does not have yet:
    ///
    /// 1. Fast local lookup, then ONE bounded wake-on-arrival slice — a racing
    ///    pre-proposal PUSH usually lands here (the common case; keeps recovery rare).
    /// 2. On a remaining miss (a true push miss), a NON-BLOCKING handoff to the
    ///    `DaRecoveryWorker` plus fail-fast `Err` — not an in-line pull. The worker
    ///    owns recovery (off-thread, ~1 s budget), so the re-proposed view finds
    ///    the bodies durably local.
    ///
    /// Consensus-thread budget: one ~20 ms slice plus bookkeeping. Returns the
    /// reconstructed actions (in `hashes` order) when every body is present, or
    /// `Err(missing_count)` so the caller votes MissingData — NOT Invalid, no
    /// blacklisting (mem 28e1a821 lineage): the block is simply re-proposed next
    /// view. Only call with a non-empty `hashes`.
    fn reconstruct_native_actions_hot(
        &self,
        hashes: &[torus_types::B256],
    ) -> Result<Vec<torus_types::SignedNativeAction>, usize> {
        let Some(ref mempool) = self.mempool else {
            return Err(hashes.len()); // no DA store wired (consensus-only observer)
        };

        let mut actions: Vec<Option<torus_types::SignedNativeAction>> = vec![None; hashes.len()];
        let mut missing: Vec<usize> = Vec::new();
        for (i, hash) in hashes.iter().enumerate() {
            // Read the DURABLE DA store, not the ephemeral nonce-gated mempool: a
            // block-referenced body survives the 60s nonce window, pool eviction, and a
            // restart (livelock root cause, mem 28e1a821).
            match mempool.get_native_da(hash) {
                Some(action) => actions[i] = Some(action),
                None => missing.push(i),
            }
        }

        // (1) Bounded LOCAL retry: actions are normally delivered by the proposer's
        // pre-proposal unicast push (PreProposalBundle -> BroadcastNativeActions) before
        // this proposal arrives, but that push can race the CompactBlock under load.
        // Poll briefly so a late push lands before we resort to the network.
        if !missing.is_empty() {
            // S391 wake-on-arrival: block on the DA-store arrival notifier with the
            // same 20ms slice instead of a fixed sleep — a racing push wakes this
            // thread at delivery time, while the worst case stays the old tick
            // cadence (one ~20ms slice since BS-4a shrank RECONSTRUCT_RETRIES).
            let mut seen = torus_state::NativeDaStore::arrival_generation();
            // Deadline-bounded like the pull loop below (S395): iteration-count
            // bounds stack scheduler overshoot past the hot budget under load.
            let deadline =
                std::time::Instant::now() + RECONSTRUCT_RETRY_DELAY * RECONSTRUCT_RETRIES as u32;
            loop {
                let now = std::time::Instant::now();
                if now >= deadline {
                    break;
                }
                torus_state::NativeDaStore::wait_for_arrival(
                    seen,
                    std::cmp::min(RECONSTRUCT_RETRY_DELAY, deadline - now),
                );
                // Phase 2.3 pre-warm (#5): a hash-only push made THIS node PULL the bodies
                // out-of-band (the receiver fired the by-hash fetch on the manifest), so they
                // arrive in the fetcher inbound. Absorb them into the DA store here, in the
                // fast local retry, so a pre-warmed body is picked up WITHOUT the redundant
                // hot network fetch below — the common case once the proposer pushes hashes.
                if let Some(ref fetcher) = self.da_fetcher {
                    Self::absorb_fetched_bodies(mempool, fetcher.as_ref());
                }
                missing.retain(|&i| match mempool.get_native_da(&hashes[i]) {
                    Some(action) => {
                        actions[i] = Some(action);
                        false
                    }
                    None => true,
                });
                if missing.is_empty() {
                    break;
                }
                // Fold in our own absorb puts so they don't self-wake the next wait.
                seen = torus_state::NativeDaStore::arrival_generation();
            }
        }

        // (2) BS-4a: a true push miss no longer burns the consensus thread on an
        // in-line pull. Hand the misses to the recovery worker (off-thread, roomier
        // budget) and fail THIS view — MissingData, re-proposed next view, by which
        // point the worker has the bodies durably local. mem 7efe7062.
        if !missing.is_empty() {
            if let Some(ref worker) = self.da_recovery {
                // Count only batches the worker actually queued: a dead worker or
                // full queue must not paint the A/B handoff signal green while
                // recovery is silently dropped (final-review finding 1).
                if worker.submit(missing.iter().map(|&i| hashes[i]).collect()) {
                    if let Some(ref m) = self.metrics {
                        m.native_da_recovery_handoffs.inc();
                    }
                }
            }
            return Err(missing.len());
        }
        Ok(actions
            .into_iter()
            .map(|a| a.expect("all bodies present"))
            .collect())
    }

    /// Drain fetched bodies and mirror them into the durable DA store. Each body is
    /// stored under its own RECOMPUTED action-hash (`mirror_native_to_da`), so a peer
    /// cannot place a body under a hash it does not own.
    fn absorb_fetched_bodies(mempool: &Mempool, fetcher: &dyn NativeDaFetcher) {
        let bodies = fetcher.drain();
        if bodies.is_empty() {
            return;
        }
        let actions: Vec<torus_types::SignedNativeAction> = bodies
            .iter()
            .filter_map(|b| bincode::deserialize::<torus_types::SignedNativeAction>(b).ok())
            .collect();
        if !actions.is_empty() {
            mempool.mirror_native_to_da(&actions);
        }
    }

    fn hash_datum(bytes: &[u8]) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        hasher.finalize().into()
    }

    /// Compute validator set updates at epoch boundary.
    fn epoch_validator_set_updates(&mut self, height: u64) -> Option<ValidatorSetUpdates> {
        if let Some((cached_height, ref cached_result)) = self.cached_vs_updates {
            if cached_height == height {
                return cached_result.clone();
            }
        }

        if !EpochManager::is_epoch_boundary(height, self.epoch_length) {
            self.cached_vs_updates = Some((height, None));
            return None;
        }

        let epoch = EpochManager::epoch_for_block(height, self.epoch_length);

        if let Err(e) = self.staking.apply_pending_rotations(epoch) {
            tracing::error!(%e, "failed to apply pending key rotations");
        }

        let new_set = match EpochManager::compute_new_validator_set(
            &self.staking,
            self.max_validators,
            epoch,
        ) {
            Ok(set) => set,
            Err(e) => {
                tracing::error!(%e, "failed to compute validator set at epoch boundary");
                return None;
            }
        };

        if let Err(e) = EpochManager::check_minimum_set(&new_set) {
            tracing::error!(%e, "epoch rotation aborted: set too small");
            return None;
        }

        let cap = EpochManager::safe_rotation_cap(self.last_validator_set.validators.len());
        let capped_set = if cap > 0 {
            EpochManager::apply_rotation_cap(&self.last_validator_set, new_set.clone(), cap)
        } else {
            new_set.clone()
        };

        let diff = EpochManager::compute_validator_set_diff(&self.last_validator_set, &capped_set);
        if diff.is_empty() {
            self.last_validator_set = capped_set;
            self.cached_vs_updates = Some((height, None));
            return None;
        }

        EpochManager::log_rotation(&self.last_validator_set, &capped_set, &diff, epoch);

        if let Err(e) = EpochManager::update_validator_statuses(&self.staking, &capped_set) {
            tracing::error!(%e, "failed to update validator statuses");
        }

        let mut updates = ValidatorSetUpdates::new();

        for v in &diff.inserts {
            if let Ok(vk) = VerifyingKey::from_bytes(&v.pubkey.0) {
                updates.insert(vk, Power::new(v.power));
            }
        }

        for addr in &diff.deletes {
            if let Ok(Some(val)) = self.staking.get_validator(addr) {
                if let Ok(vk) = VerifyingKey::from_bytes(&val.pubkey) {
                    updates.delete(vk);
                }
            }
        }

        for old_pk in &diff.rotated_out_pubkeys {
            if let Ok(vk) = VerifyingKey::from_bytes(old_pk) {
                updates.delete(vk);
            }
        }

        for v in &diff.inserts {
            let is_new = !self
                .last_validator_set
                .validators
                .iter()
                .any(|old| old.address == v.address);
            if is_new {
                tracing::warn!(
                    epoch,
                    validator = %v.address,
                    "new active validator -- ensure peer connectivity for consensus messages"
                );
            }
        }

        tracing::info!(
            epoch,
            inserts = diff.inserts.len(),
            deletes = diff.deletes.len(),
            key_rotations = diff.rotated_out_pubkeys.len(),
            active_set_size = capped_set.validators.len(),
            "epoch boundary: validator set updated"
        );
        self.last_validator_set = capped_set;
        self.leader_state.sync_validators(&self.last_validator_set);
        self.cached_vs_updates = Some((height, Some(updates.clone())));
        Some(updates)
    }
}

impl Drop for TorusApp {
    fn drop(&mut self) {
        self.exec_tx.take();
        if let Some(handle) = self.exec_handle.take() {
            let _ = handle.join();
        }
    }
}

impl App<RocksKVStore> for TorusApp {
    /// Produce a block: drain mempool, build raw tx list, NO execution.
    ///
    /// The state_root is set to the parent's state_root (unchanged until
    /// execution happens on the pipeline thread).
    fn produce_block(
        &mut self,
        request: ProduceBlockRequest<RocksKVStore>,
    ) -> ProduceBlockResponse {
        // T1.5 FAIL-STOP: with the execution pipeline dead, state is frozen —
        // do not drain the mempool or build a block on top of unexecuted
        // history. The `App` trait has no "refuse" variant, so return an
        // empty, datum-less response that every honest validate_block rejects
        // (datums.len() != 1): this node's leader views time out instead of
        // zombie-advancing the chain.
        if self.is_exec_failed() {
            tracing::error!(
                "produce_block: execution pipeline dead — FAIL-STOP, refusing to build a block"
            );
            return ProduceBlockResponse {
                data_hash: CryptoHash::new(Self::hash_datum(&[])),
                data: Data::new(vec![]),
                app_state_updates: None,
                validator_set_updates: None,
            };
        }
        let parent_header = if let Some(parent_hash) = request.parent_block() {
            if let Ok(Some(parent_block)) = request.block_tree().block(&parent_hash) {
                let datums = parent_block.data.vec();
                datums
                    .first()
                    .and_then(|d| {
                        bincode::deserialize::<TorusBlock>(d.bytes())
                            .map(|b| b.header)
                            .or_else(|_| {
                                bincode::deserialize::<CompactBlock>(d.bytes()).map(|cb| cb.header)
                            })
                            .ok()
                    })
                    .unwrap_or_else(|| self.last_header.clone())
            } else {
                self.last_header.clone()
            }
        } else {
            self.last_header.clone()
        };
        tracing::info!(
            parent_height = parent_header.height,
            local_height = self.last_header.height,
            "produce_block called (CTE)"
        );

        // Exec-ceiling Option A: wire the (previously dead) block_build_seconds —
        // covers mempool selection, DA mirror, attestation, construction, encode.
        let build_timer = std::time::Instant::now();

        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let (native_with_senders, evm_txs) = if let Some(ref mempool) = self.mempool {
            let gas_limit = if parent_header.evm_gas_limit == 0 {
                torus_evm::DEFAULT_BLOCK_GAS_LIMIT
            } else {
                parent_header.evm_gas_limit
            };
            // Pipeline-aware selection: exclude actions already carried by our
            // in-flight (proposed-but-uncommitted) blocks so the non-destructive pool
            // does not re-select them for N+1/N+2 before N commits and calls
            // `remove_committed_native` — the root cause of duplicate native inclusion
            // (memory 282f9818). `pending_proposals` is exactly that in-flight window.
            let mut in_flight: std::collections::HashSet<torus_types::B256> = self
                .pending_proposals
                .values()
                .flat_map(|b| {
                    b.native_actions
                        .iter()
                        .map(torus_types::compute_action_hash)
                })
                .collect();
            // Union the hash ledger: covers compact proposals whose bodies never
            // reconstructed (MissingData) — absent from `pending_proposals` but
            // still in flight on the wire (s355 duplicate-inclusion tail).
            self.in_flight_hashes.extend_into(&mut in_flight);
            let native = mempool.select_native_for_block_with_senders_excluding(
                torus_mempool::rate_limit::native_total_block_cap(),
                &in_flight,
                torus_mempool::rate_limit::native_block_bytes_cap(),
                torus_mempool::rate_limit::native_orders_per_block_cap(),
            );
            let evm = mempool.drain_evm(gas_limit, parent_header.state_root);
            if !evm.is_empty() || !native.is_empty() {
                tracing::info!(
                    evm_txs = evm.len(),
                    native_actions = native.len(),
                    "selected actions for block"
                );
            }
            (native, evm)
        } else {
            (vec![], vec![])
        };

        let native_actions: Vec<torus_types::SignedNativeAction> =
            native_with_senders.iter().map(|(_, a)| a.clone()).collect();

        // Proposer guarantee: mirror every referenced body to the durable DA store
        // so any validator can reconstruct the block out-of-band, even when it is
        // compact (push-primary, pull-rare). Livelock fix (mem 28e1a821).
        if let Some(ref mempool) = self.mempool {
            mempool.mirror_native_to_da(&native_actions);
        }

        let sig_attestation = match self.signing_key {
            Some(ref key) => torus_bridge::proposer::generate_sig_attestation(&native_actions, key),
            None => [0u8; 64],
        };

        use torus_types::{Bloom, B256};
        // Ancestry commitment: bind this proposal to the exact parent header it
        // was built on (`parent_header`). This is the keccak canonical hash — the
        // same value RPC `parentHash` and the fork checker compare — and it is
        // kept consistent with the hotstuff `justify.block` the QC points at (an
        // honest leader builds on the block its high-QC justifies).
        let parent_hash =
            alloy_primitives::keccak256(parent_header.canonical_header_bytes());
        let block = TorusBlock {
            header: TorusBlockHeader {
                height: parent_header.height + 1,
                parent_hash,
                timestamp,
                proposer: self.proposer_address,
                state_root: parent_header.state_root,
                receipts_root: B256::ZERO,
                logs_bloom: Bloom::ZERO,
                evm_gas_used: 0,
                evm_fee_revenue: 0,
                evm_gas_limit: parent_header.evm_gas_limit,
                native_action_count: native_actions.len() as u32,
                evm_tx_count: evm_txs.len() as u32,
                base_fee_per_gas: parent_header.base_fee_per_gas,
                epoch: parent_header.epoch,
                validator_set_hash: parent_header.validator_set_hash,
                sig_attestation,
            },
            native_actions,
            evm_transactions: evm_txs,
            core_writer_actions: vec![],
        };

        let height = block.header.height;

        // Pre-proposal push: only needed for COMPACT proposals, whose bodies travel
        // out-of-band. A FULL block already carries its bodies inline, so pushing them
        // too would be a redundant double-send (Task 8). The proposer always mirrors to
        // its own DA store above, regardless of mode.
        if COMPACT_PROPOSALS && !native_with_senders.is_empty() {
            if let Some(ref tx) = self.pre_proposal_tx {
                let count = native_with_senders.len();
                match tx.try_send(PreProposalBundle {
                    actions: native_with_senders,
                }) {
                    Ok(()) => tracing::info!(count, height, "pre-proposal push sent"),
                    Err(e) => tracing::warn!(count, height, %e, "pre-proposal push failed"),
                }
            }
        }

        // Note our own block's hashes too: a same-height re-proposal would
        // overwrite the `pending_proposals` entry and silently untrack these.
        let own_hashes: Vec<torus_types::B256> = block
            .native_actions
            .iter()
            .map(torus_types::compute_action_hash)
            .collect();
        let encoded = encode_proposal_datum(&block, COMPACT_PROPOSALS);
        self.pending_proposals.insert(height, block);
        self.pending_proposals.retain(|&h, _| h + 10 > height);
        self.in_flight_hashes.note(height, own_hashes);
        let hash = Self::hash_datum(&encoded);

        let validator_set_updates = self.epoch_validator_set_updates(height);

        if let Some(ref m) = self.metrics {
            m.block_build_seconds
                .observe(build_timer.elapsed().as_secs_f64());
        }

        ProduceBlockResponse {
            data_hash: CryptoHash::new(hash),
            data: Data::new(vec![Datum::new(encoded)]),
            app_state_updates: None,
            validator_set_updates,
        }
    }

    /// Validate a proposed block: structural + signature checks only, NO execution.
    fn validate_block(
        &mut self,
        request: ValidateBlockRequest<RocksKVStore>,
    ) -> ValidateBlockResponse {
        tracing::info!("validate_block called (CTE)");

        // T1.5 FAIL-STOP: a dead execution pipeline means committed blocks are
        // no longer applied here — stop voting so this node cannot help
        // finalize blocks it will never execute.
        if self.is_exec_failed() {
            tracing::error!(
                "validate_block: execution pipeline dead — FAIL-STOP, refusing to vote"
            );
            return ValidateBlockResponse::Invalid;
        }

        let block = request.proposed_block();
        let datums = block.data.vec();

        if datums.len() != 1 {
            tracing::warn!(
                datums_len = datums.len(),
                "validate_block: REJECTED -- datums.len() != 1"
            );
            return ValidateBlockResponse::Invalid;
        }

        let datum_bytes = datums[0].bytes();

        let computed = Self::hash_datum(datum_bytes);
        if block.data_hash != CryptoHash::new(computed) {
            tracing::warn!(
                datum_len = datum_bytes.len(),
                "validate_block: REJECTED -- data_hash mismatch"
            );
            return ValidateBlockResponse::Invalid;
        }

        // Try TorusBlock first (new format), CompactBlock fallback (backward compat)
        let torus_block = if let Ok(block) = bincode::deserialize::<TorusBlock>(datum_bytes) {
            tracing::info!(
                height = block.header.height,
                evm_tx_count = block.evm_transactions.len(),
                native_count = block.native_actions.len(),
                "validate_block: TorusBlock deserialized"
            );
            block
        } else if let Ok(compact) = bincode::deserialize::<CompactBlock>(datum_bytes) {
            tracing::info!(
                height = compact.header.height,
                native_hashes = compact.native_action_hashes.len(),
                "validate_block: CompactBlock fallback"
            );

            // Track these hashes BEFORE attempting reconstruction: if bodies are
            // missing (MissingData below) this proposal never reaches
            // `pending_proposals`, yet its actions are in flight on the wire — the
            // next leader must still exclude them from selection or they are
            // re-included 2–4x (the s355 duplicate-inclusion tail, mem c0f4f938).
            self.in_flight_hashes.note(
                compact.header.height,
                compact.native_action_hashes.iter().copied(),
            );

            let native_actions = if compact.native_action_hashes.is_empty() {
                vec![]
            } else {
                // Reconstruct out-of-band bodies for the HOT path: a fast local retry for
                // a racing pre-proposal PUSH, then a SHORT bounded native-DA PULL so a
                // genuine push miss is recovered live instead of wedging (#4 Task 1 — the
                // un-wedge). Budget stays ≪ the 500 ms view timeout.
                match self.reconstruct_native_actions_hot(&compact.native_action_hashes) {
                    Ok(actions) => actions,
                    Err(missing_count) => {
                        if let Some(ref m) = self.metrics {
                            m.missing_action_rejections.inc();
                        }
                        tracing::warn!(
                            missing_count,
                            height = compact.header.height,
                            "validate_block: MISSING native action bodies after hot retry + pull -- fetch via DA (not invalid)"
                        );
                        // MissingData (NOT Invalid): the block is structurally fine, we
                        // just lack the out-of-band bodies. The sync path must not
                        // blacklist the serving peer for this (livelock root cause, mem
                        // 28e1a821) — it re-pulls via the rare fallback instead.
                        return ValidateBlockResponse::MissingData;
                    }
                }
            };

            TorusBlock {
                header: compact.header,
                native_actions,
                evm_transactions: compact.evm_transactions,
                core_writer_actions: compact.core_writer_actions,
            }
        } else {
            tracing::warn!("validate_block: REJECTED -- deserialization failed");
            return ValidateBlockResponse::Invalid;
        };

        if !torus_block.evm_transactions.is_empty()
            && decode_all_txs(&torus_block.evm_transactions).is_err()
        {
            tracing::warn!("validate_block: REJECTED -- invalid EVM transactions");
            return ValidateBlockResponse::Invalid;
        }

        if !torus_block.native_actions.is_empty() {
            if torus_block.header.sig_attestation == [0u8; 64] {
                // Non-attested proposer: we must verify every action ourselves. Use the
                // SAME session-aware resolver as the execution path (see ~L300) so BOTH
                // EIP-712 and ed25519 *session* signatures are checked against committed
                // session state. The old code called `recover_sender`, which returns Err
                // for every `ActionSignature::Session` (eip712.rs:765) and thus blanket-
                // rejected all session actions on non-attested blocks — the deterministic
                // root cause of the reject-loop (mem 41e06912). `None` == bad sig /
                // missing / expired / out-of-scope session.
                //
                // Reads `self.state_db` (committed state, identical across validators at
                // this height) so the verdict is deterministic and cannot fork WITHIN a
                // block. NOTE: this is a CONSENSUS VALIDITY CHANGE — deploy to ALL
                // validators together; a mixed old/new set disagrees on these blocks and
                // forks. Same-block CreateSession+order still rejects here (defect A,
                // tracked separately) — strictly better than rejecting all session blocks.
                let resolved = torus_types::eip712::batch_verify_native_actions(
                    &torus_block.native_actions,
                    torus_block.header.timestamp,
                    |pubkey| self.state_db.get_session(pubkey).ok().flatten(),
                );
                if let Some(i) = resolved.iter().position(|s| s.is_none()) {
                    tracing::warn!(
                        index = i,
                        "validate_block: REJECTED -- invalid native action signature"
                    );
                    return ValidateBlockResponse::Invalid;
                }
            } else {
                let proposer_addr = torus_block.header.proposer;
                let proposer_pubkey = match self.staking.get_validator(&proposer_addr) {
                    Ok(Some(val)) => match ed25519_dalek::VerifyingKey::from_bytes(&val.pubkey) {
                        Ok(vk) => vk,
                        Err(_) => {
                            tracing::warn!(%proposer_addr, "validate_block: REJECTED -- invalid proposer pubkey");
                            return ValidateBlockResponse::Invalid;
                        }
                    },
                    _ => {
                        tracing::warn!(%proposer_addr, "validate_block: REJECTED -- proposer not in validator set");
                        return ValidateBlockResponse::Invalid;
                    }
                };
                if !torus_bridge::proposer::verify_sig_attestation(
                    &torus_block.native_actions,
                    &torus_block.header.sig_attestation,
                    &proposer_pubkey,
                ) {
                    tracing::warn!(%proposer_addr, "validate_block: REJECTED -- invalid sig attestation");
                    return ValidateBlockResponse::Invalid;
                }
            }
        }

        // Ancestry check (best-effort, voting time): if our latest committed
        // header IS the claimed parent (its height is H-1), the proposal's
        // `parent_hash` MUST equal the keccak canonical hash of that header. A
        // mismatch means the proposer built on a DIFFERENT height-(H-1) block than
        // we finalized — the exact fork/equivocation the hash-level fork checker
        // exists to catch — so refuse to vote.
        //
        // When our `last_header` is NOT the parent (we are behind, or the parent
        // is still pending in the header-first fast path) we cannot cheaply
        // resolve the parent here, so we DEFER rather than reject (preserving
        // liveness): the hotstuff justify QC still constrains ancestry at the
        // consensus level, and the commit-time check in `execute_committed_block`
        // re-verifies the link authoritatively once the parent is applied.
        let height = torus_block.header.height;
        if height > 0 && self.last_header.height + 1 == height {
            let expected_parent =
                alloy_primitives::keccak256(self.last_header.canonical_header_bytes());
            if torus_block.header.parent_hash != expected_parent {
                tracing::warn!(
                    height,
                    claimed_parent = %torus_block.header.parent_hash,
                    expected_parent = %expected_parent,
                    "validate_block: REJECTED -- parent_hash does not match locally-known parent (ancestry violation)"
                );
                return ValidateBlockResponse::Invalid;
            }
        }

        self.pending_proposals.insert(height, torus_block);
        self.pending_proposals.retain(|&h, _| h + 10 > height);

        let validator_set_updates = self.epoch_validator_set_updates(height);
        ValidateBlockResponse::Valid {
            app_state_updates: None,
            validator_set_updates,
        }
    }

    /// Validate a block during sync. Delegates to `validate_block`, which now
    /// reconstructs CompactBlock bodies from the durable DA store (not the ephemeral
    /// mempool) -- so a synced block whose bodies are durably present validates
    /// instead of being rejected and blacklisting the serving peer (livelock root
    /// cause, mem 28e1a821). A still-missing body is fetched via the rare
    /// pull-fallback (Task 6).
    fn validate_block_for_sync(
        &mut self,
        request: ValidateBlockRequest<RocksKVStore>,
    ) -> ValidateBlockResponse {
        // RARE pull-fallback (Task 6): a synced CompactBlock whose bodies are absent
        // from the durable DA store fetches them by-hash from peers BEFORE validating,
        // so the block recovers in-call instead of bouncing (no blacklist, mem 28e1a821).
        // This runs on the block-sync path; the hot validate_block path is untouched and
        // never triggers a network fetch (mem 8ee99db3). Peek + decode the datum, then
        // drop the borrow before validating (which consumes `request`).
        let compact: Option<CompactBlock> = {
            let datums = request.proposed_block().data.vec();
            datums.first().and_then(|d| {
                let bytes = d.bytes();
                // A full TorusBlock is self-contained (no out-of-band bodies); only a
                // CompactBlock can miss. Try full first to avoid a misparse.
                if bincode::deserialize::<TorusBlock>(bytes).is_ok() {
                    None
                } else {
                    bincode::deserialize::<CompactBlock>(bytes).ok()
                }
            })
        };
        if let Some(compact) = compact {
            self.pull_compact_bodies_if_missing(&compact);
        }
        self.validate_block(request)
    }

    /// Send committed block to the execution pipeline thread.
    fn on_committed_block(&mut self, block: &Block, _committed_hash: CryptoHash) {
        // T1.5 FAIL-STOP: once the execution pipeline is dead, do NOT advance
        // height/view or accept further finalization — consensus finalizing
        // while state is frozen is exactly the zombie-advance this latch stops.
        if self.is_exec_failed() {
            tracing::error!(
                "on_committed_block: execution pipeline dead — FAIL-STOP, ignoring committed block"
            );
            return;
        }

        let datums = block.data.vec();
        let Some(datum) = datums.first() else {
            tracing::debug!("on_committed_block: no datums in block");
            return;
        };

        let datum_bytes = datum.bytes();
        // The HEADER is always in the datum; only CompactBlock BODIES need the DA
        // store. Extract the header up front and advance last_header from it
        // UNCONDITIONALLY (below) so a missing body can never freeze consensus
        // height/view -- the livelock root cause (mem 28e1a821) was an early return
        // here, before last_header advanced, which froze RPC height and churned views.
        let (header, source): (TorusBlockHeader, ExecSource) = if let Ok(full) =
            bincode::deserialize::<TorusBlock>(datum_bytes)
        {
            // FULL datum carries its bodies inline and its committed header — the
            // consensus datum is self-consistent and authoritative. Drop any
            // stale same-height cache entry (cleanup) but NEVER let it override
            // the committed block (T1.2 body determinism).
            let height = full.header.height;
            self.pending_proposals.remove(&height);
            (full.header.clone(), ExecSource::Ready(full))
        } else if let Ok(compact) = bincode::deserialize::<CompactBlock>(datum_bytes) {
            // The COMMITTED header is the consensus datum — taken VERBATIM,
            // never rebuilt from a local cache or from execution results. The
            // body is the set the committed compact references by hash: a cached
            // proposal is a valid fast path ONLY when its action hashes match
            // that reference exactly (skips a DA read); otherwise reconstruct
            // all-or-nothing from the durable DA store. A stale same-height
            // re-proposal (fewer / other actions, having overwritten this
            // height's `pending_proposals` entry) must NEVER be executed in
            // place of the committed block — that was the t12-diag1 divergence
            // (proposer executed its cached 40 of the committed 59 and persisted
            // a divergent header).
            let height = compact.header.height;
            match self.pending_proposals.remove(&height) {
                Some(cached) if cached_matches_compact(&cached, &compact) => {
                    let ready = TorusBlock {
                        header: compact.header.clone(),
                        native_actions: cached.native_actions,
                        evm_transactions: compact.evm_transactions.clone(),
                        core_writer_actions: compact.core_writer_actions.clone(),
                    };
                    (compact.header.clone(), ExecSource::Ready(ready))
                }
                // Reconstruct lazily (and RE-try on each drain) from the durable DA
                // store — a body still missing here becomes a heal-able hole, never
                // an out-of-order execution.
                _ => (compact.header.clone(), ExecSource::Compact(compact)),
            }
        } else {
            tracing::warn!("on_committed_block: failed to deserialize block datum");
            return;
        };

        let height = header.height;

        // FIX (heal channel): make the committed DATUM durable NOW, as the FIRST
        // durable action of the commit callback — BEFORE the block is buffered,
        // reconstructed, or dispatched (any of which can be deferred behind an
        // earlier hole, widening the loss window). Under compact proposals the
        // datum carries the `native_action_hashes`, which a boot-parked hole needs
        // to content-address the missing bodies from peers. Cheap: one RocksDB
        // write per COMMITTED block (not per voted block), a `CompactBlock` being
        // hashes + small inline EVM/core-writer refs; pruned again once the body is
        // durable (`dispatch_to_exec`). Without this, a SIGKILL in the
        // commit->persist window strands the hole with no way to pull bodies
        // (`missing=0`, t12-r3-full h1165).
        persist_commit_manifest(&self.state_db, height, datum_bytes);

        // Committed: these hashes leave the in-flight window. The mempool prunes
        // them via `remove_committed_native` when the block actually executes.
        self.in_flight_hashes.clear(height);

        // Track committed consensus height/view from the header regardless of body
        // availability (BFT finality is independent of local execution readiness).
        // THIS UNCONDITIONAL ADVANCE IS LOAD-BEARING (livelock fix, mem 28e1a821):
        // consensus voting/finalization runs ahead; execution lags and heals below.
        if height > self.last_header.height {
            self.last_header = header.clone();
            self.leader_state.set_view(height.saturating_add(1));
        }

        // Slashes detected during THIS commit ride with THIS block into the
        // strict-order queue (applied when the block executes, in order).
        let slashes: Vec<PendingSlash> = self.pending_slashes.drain(..).collect();

        // Regime-B strict-order execution: hand blocks to the pipeline strictly in
        // ascending height with no gaps. A missing-body block becomes a head-of-line
        // hole; every later committed block waits behind it until it heals.
        self.enqueue_for_execution(height, source, slashes);
    }

    /// MonadBFT B3: Handle speculative rollback due to leader equivocation.
    fn on_speculative_rollback(
        &mut self,
        block: hotstuff_rs::types::data_types::CryptoHash,
        evidence: &EquivocationEvidence,
    ) {
        tracing::warn!(
            view = %evidence.view.int(),
            leader = ?evidence.leader.to_bytes(),
            block_a = ?evidence.block_a,
            block_b = ?evidence.block_b,
            "SPECULATIVE ROLLBACK: leader equivocation detected"
        );

        let leader_pubkey = evidence.leader.to_bytes();
        let leader_addr = match self.staking.find_validator_by_pubkey(&leader_pubkey) {
            Ok(Some(val)) => val.address,
            Ok(None) => {
                tracing::error!(
                    leader_pubkey = ?leader_pubkey,
                    "equivocation detected but validator not found -- cannot slash"
                );
                return;
            }
            Err(e) => {
                tracing::error!(%e, "failed to look up validator for slashing");
                return;
            }
        };

        self.pending_slashes.push(PendingSlash {
            validator: leader_addr,
            fraction_bps: 500,
            reason: SlashReason::DoubleSign,
            tombstone: true,
        });
        tracing::info!(
            %leader_addr,
            "equivocation slash buffered (5% + tombstone) -- will apply on next committed block"
        );

        tracing::info!(
            rolled_back_block = ?block,
            "speculative rollback complete (no state to revert in CTE model)"
        );
    }

    /// FIX C2: consensus (block sync) detected a FATAL safety violation — a peer's
    /// committed chain conflicts with a block we already committed at the same
    /// height. Latch the shared `exec_failed` fail-stop (the SAME latch the T1.5
    /// native-exec watcher and the conflicting-commit execution guard use). The
    /// node binary watches this latch (`exec_failed_handle()`) and turns it into
    /// process termination (`exit(70)`), so the node stops voting/finalizing over
    /// an already-forked chain instead of merely ending the sync session.
    fn on_fatal_safety_violation(&mut self) {
        tracing::error!(
            "CRITICAL SAFETY VIOLATION: block sync detected a peer's committed chain conflicting \
             with our own committed chain (two honest quorums finalized conflicting blocks). \
             Latching fail-stop to halt the node."
        );
        self.exec_failed.store(true, Ordering::SeqCst);
    }
}

impl TorusApp {
    /// Regime-B strict-order execution gate. Route a committed block into the
    /// execution pipeline while GUARANTEEING it is executed strictly in ascending
    /// height with no gaps — the only correct behaviour, because native matching
    /// is order-dependent (executing H+1 before H diverges from peers and an
    /// interior hole is PERMANENT divergence).
    ///
    /// HOT PATH: in the healthy in-order case (`deferred_exec` empty and this is
    /// the expected height) the block is reconstructed + dispatched directly with
    /// ZERO buffering — bit-identical cost to the pre-Regime-B send. The reorder
    /// map and hole accounting are touched ONLY once a hole exists.
    fn enqueue_for_execution(
        &mut self,
        height: u64,
        source: ExecSource,
        slashes: Vec<PendingSlash>,
    ) {
        let next = *self.exec_next_height.get_or_insert_with(|| {
            // Seed from the durable applied marker (mid-chain / post-replay); with
            // nothing applied yet (fresh genesis) the first committed height we see
            // defines the baseline.
            read_native_applied_height(&self.state_db)
                .map(|a| a + 1)
                .unwrap_or(height)
        });

        // Below the frontier we already sent to exec: a benign re-delivery. A
        // genuine conflicting re-commit at an applied height is caught on the exec
        // thread (`detect_conflicting_commit`); here we only avoid double-sending.
        if height < next {
            return;
        }

        // HOT PATH: no outstanding hole and this is exactly the next height.
        if self.deferred_exec.is_empty() && height == next {
            match self.materialize_owned(source) {
                Ok(block) => {
                    self.dispatch_to_exec(block, slashes);
                    self.exec_next_height = Some(next + 1);
                    return;
                }
                Err((source, missing)) => {
                    // First block of a new hole: buffer it and begin heal accounting.
                    self.deferred_exec
                        .insert(height, DeferredExecBlock { source, slashes });
                    self.note_exec_hole(height, &missing);
                    return;
                }
            }
        }

        // A hole already exists, or this height is ahead of the frontier: buffer in
        // ascending order and drain whatever is now contiguous.
        self.deferred_exec
            .insert(height, DeferredExecBlock { source, slashes });
        self.drain_exec_queue();
    }

    /// Regime-B. Dispatch every contiguously-available buffered block starting at
    /// `exec_next_height`, in ascending order, stopping at the first height whose
    /// bodies are still missing (the head-of-line hole) or not yet committed.
    fn drain_exec_queue(&mut self) {
        loop {
            let next = match self.exec_next_height {
                Some(n) => n,
                None => return,
            };
            // Head-of-line not yet committed/buffered: NOT a hole (consensus commits
            // every height in order, so it will arrive) — just wait.
            let Some(entry) = self.deferred_exec.remove(&next) else {
                return;
            };
            let DeferredExecBlock { source, slashes } = entry;
            match self.materialize_owned(source) {
                Ok(block) => {
                    self.dispatch_to_exec(block, slashes);
                    self.exec_next_height = Some(next + 1);
                    self.clear_exec_hole_state();
                    // continue draining the next contiguous height
                }
                Err((source, missing)) => {
                    // Head-of-line bodies still missing: a real hole. Put it back,
                    // trigger the DA fetch + throttled log + budget check, and STOP —
                    // never execute past it.
                    self.deferred_exec
                        .insert(next, DeferredExecBlock { source, slashes });
                    self.note_exec_hole(next, &missing);
                    return;
                }
            }
        }
    }

    /// Regime-B. Obtain the bodies for a source, consuming it. On a compact miss,
    /// hands the source BACK (so the caller can re-buffer it and retry from the
    /// healing DA store) alongside the missing hashes.
    fn materialize_owned(
        &self,
        source: ExecSource,
    ) -> Result<TorusBlock, (ExecSource, Vec<torus_types::B256>)> {
        match source {
            ExecSource::Ready(block) => Ok(block),
            ExecSource::Compact(compact) => match self.reconstruct_compact_from_da(&compact) {
                Ok(block) => Ok(block),
                Err(missing) => Err((ExecSource::Compact(compact), missing)),
            },
            // FIX 1b: a boot-parked hole. Re-load from the durable CFs on each
            // attempt. The header carries only counts (not the native-action
            // hashes), so a missing body cannot be DA-hash-fetched here — it heals
            // when the body becomes durable locally (FIX 1a) or a block-sync
            // re-delivery of this height arrives as `ExecSource::Ready` and
            // overrides this placeholder. Return an EMPTY missing-hash set so the
            // heal loop still accounts the hole + budget without a futile fetch.
            ExecSource::Durable(height) => match load_replay_header(&self.state_db, height) {
                Some(header) => {
                    let is_empty =
                        header.native_action_count == 0 && header.evm_tx_count == 0;
                    if is_empty {
                        Ok(TorusBlock {
                            header,
                            native_actions: vec![],
                            evm_transactions: vec![],
                            core_writer_actions: vec![],
                        })
                    } else {
                        match load_replay_body(&self.state_db, height) {
                            Some(body) => Ok(TorusBlock {
                                header,
                                native_actions: body.native_actions,
                                evm_transactions: body.evm_transactions,
                                core_writer_actions: body.core_writer_actions,
                            }),
                            None => Err((ExecSource::Durable(height), vec![])),
                        }
                    }
                }
                None => Err((ExecSource::Durable(height), vec![])),
            },
        }
    }

    /// Regime-B. Send a fully-reconstructed committed block to the execution
    /// thread, in order. Carries the mempool bookkeeping that must happen once a
    /// block is actually executed (prune committed native hashes, pin the base
    /// fee) plus the T1.5 closed-channel fail-stop.
    fn dispatch_to_exec(&mut self, torus_block: TorusBlock, pending_slashes: Vec<PendingSlash>) {
        let height = torus_block.header.height;

        // FIX 1a (crash-safety): make the committed block's header+body DURABLE
        // NOW, at commit time, BEFORE it enters the execution pipeline. A crash in
        // the commit->execute window previously lost the body (written only at
        // execution), and boot crash-recovery fail-stopped on the committed-but-
        // bodiless height (devnet t12-final-a/c). The block is fully materialized
        // here (bodies reconstructed from the durable DA store), so this write is
        // authoritative; the execution-time writes remain as idempotent repeats.
        persist_committed_block_durably(&self.state_db, &torus_block);

        // The body is now durable in `CF_BLOCK_BODIES`; the commit manifest that
        // guarded the heal channel for this height is redundant. Drop it (best
        // effort) so `CF_COMMIT_MANIFEST` stays bounded to the committed-but-not-
        // yet-dispatched window. A still-parked hole never reaches here, so its
        // manifest is retained for the pull.
        prune_commit_manifest(&self.state_db, height);

        tracing::info!(
            height,
            evm_txs = torus_block.evm_transactions.len(),
            native = torus_block.native_actions.len(),
            "on_committed_block: sending to execution pipeline"
        );

        if !torus_block.native_actions.is_empty() {
            if let Some(ref mempool) = self.mempool {
                let hashes: Vec<torus_types::B256> = torus_block
                    .native_actions
                    .iter()
                    .map(torus_types::compute_action_hash)
                    .collect();
                mempool.remove_committed_native(&hashes);
            }
        }

        // D4 (S392): pin the mempool's admission fee floor to the base fee
        // committed blocks actually charge (frozen at 1 gwei today; follows
        // the header once the fee market unfreezes).
        if let Some(ref mempool) = self.mempool {
            mempool.set_base_fee(torus_block.header.base_fee_per_gas);
        }

        if let Some(ref tx) = self.exec_tx {
            let msg = CommittedBlockMsg {
                torus_block,
                pending_slashes,
            };
            // inc BEFORE the (possibly blocking) send so a consensus thread stalled
            // on a full channel is visible as depth ≥ the bound, not hidden.
            if let Some(ref m) = self.metrics {
                m.exec_queue_depth.inc();
            }
            if tx.send(msg).is_err() {
                if let Some(ref m) = self.metrics {
                    m.exec_queue_depth.dec();
                }
                // T1.5 FAIL-STOP: the execution thread is gone (panic or
                // fatal) — this block is committed by consensus but will
                // NEVER execute here. Latch the failure: the node stops
                // producing, voting, and finalizing, and crash-replay closes
                // the gap on restart. Never zombie-advance past a dead pipeline.
                self.exec_failed.store(true, Ordering::SeqCst);
                tracing::error!(
                    height,
                    "execution pipeline channel closed — FAIL-STOP: halting block production, voting, and finalization"
                );
            }
        }
    }

    /// Regime-B. Account for a head-of-line execution hole: nudge the bodies via
    /// the EXISTING DA fetch path (non-blocking — this is the consensus thread,
    /// mem 8ee99db3; height has already advanced, so no wedge), log loudly but
    /// throttled, and fail-stop once the heal budget is exhausted. Retries ride on
    /// subsequent `on_committed_block` calls: consensus is provably live here (it
    /// committed PAST the hole), so the drain re-attempts on every new commit.
    fn note_exec_hole(&mut self, height: u64, missing: &[torus_types::B256]) {
        let now = std::time::Instant::now();
        let since = *self.exec_hole_since.get_or_insert(now);
        self.exec_hole_retries = self.exec_hole_retries.saturating_add(1);

        // Reuse the pull-fallback transport (Task 6) — do NOT invent new transport.
        // Peers that executed this committed block have the bodies; a retrying pull
        // must eventually land them in the durable DA store.
        if let Some(fetcher) = self.da_fetcher.as_ref() {
            fetcher.fetch(missing.iter().map(|h| h.0).collect());
            if let Some(ref m) = self.metrics {
                m.native_da_pull_requests.inc();
            }
        }

        let elapsed = now.duration_since(since);

        // Fail-stop budget: a committed block whose bodies cannot be obtained after
        // a generous retry window means this node is broken. Silent divergence is
        // worse than a halt — latch the SAME fail-stop the exec thread / crash
        // replay use; the node binary turns it into process termination.
        let budget = exec_hole_failstop_budget();
        if elapsed >= budget {
            tracing::error!(
                height,
                retries = self.exec_hole_retries,
                elapsed_secs = elapsed.as_secs(),
                missing = missing.len(),
                "execution HOLE unrecoverable: committed block's native bodies could not be \
                 obtained within the heal budget — latching fail-stop (halt, never diverge silently)"
            );
            self.exec_failed.store(true, Ordering::SeqCst);
            return;
        }

        let due = match self.exec_hole_last_log {
            None => true,
            Some(t) => now.duration_since(t) >= EXEC_HOLE_LOG_INTERVAL,
        };
        if due {
            self.exec_hole_last_log = Some(now);
            tracing::warn!(
                height,
                retries = self.exec_hole_retries,
                elapsed_secs = elapsed.as_secs(),
                missing = missing.len(),
                "execution stalled at height {} awaiting native bodies, retry {} — later committed \
                 blocks are queued (NOT executed out of order); still pulling, will NOT go silent",
                height,
                self.exec_hole_retries,
            );
        }
    }

    /// Regime-B. Clear the hole clock/log/retry counters once the head-of-line
    /// hole heals (a buffered block successfully dispatched).
    fn clear_exec_hole_state(&mut self) {
        if self.exec_hole_since.is_some() {
            self.exec_hole_since = None;
            self.exec_hole_last_log = None;
            self.exec_hole_retries = 0;
        }
    }
}

#[cfg(test)]
mod in_flight_ledger_tests {
    use super::*;
    use torus_types::B256;

    fn h(n: u8) -> B256 {
        B256::from([n; 32])
    }

    #[test]
    fn in_flight_ledger_notes_and_unions_per_height() {
        let mut ledger = InFlightHashLedger::default();
        ledger.note(5, [h(1), h(2)]);
        ledger.note(5, [h(2), h(3)]); // same-height re-proposal unions, not overwrites
        let mut out = std::collections::HashSet::new();
        ledger.extend_into(&mut out);
        assert_eq!(out, [h(1), h(2), h(3)].into_iter().collect());
    }

    #[test]
    fn in_flight_ledger_window_evicts_old_heights() {
        let mut ledger = InFlightHashLedger::default();
        ledger.note(1, [h(1)]);
        ledger.note(11, [h(2)]); // 1 + 10 > 11 is false -> height 1 evicted
        let mut out = std::collections::HashSet::new();
        ledger.extend_into(&mut out);
        assert_eq!(out, [h(2)].into_iter().collect());
    }

    #[test]
    fn in_flight_ledger_commit_clear() {
        let mut ledger = InFlightHashLedger::default();
        ledger.note(5, [h(1)]);
        ledger.note(6, [h(2)]);
        ledger.clear(5);
        let mut out = std::collections::HashSet::new();
        ledger.extend_into(&mut out);
        assert_eq!(out, [h(2)].into_iter().collect());
    }

    #[test]
    fn in_flight_ledger_extends_selection_exclusion_set() {
        // produce_block builds the exclusion set from pending_proposals bodies,
        // then unions the ledger (compact proposals whose bodies never
        // reconstructed). Both sources must land in the final set handed to
        // select_native_for_block_with_senders_excluding.
        let mut in_flight: std::collections::HashSet<B256> = [h(1)].into_iter().collect();
        let mut ledger = InFlightHashLedger::default();
        ledger.note(7, [h(2)]);
        ledger.extend_into(&mut in_flight);
        assert!(in_flight.contains(&h(1)) && in_flight.contains(&h(2)));
    }
}

#[cfg(test)]
mod crash_recovery_tests {
    use super::*;
    use torus_types::{Bloom, FixedPoint, NativeAction, SignedNativeAction, B256, U256};

    fn make_test_config_and_db() -> (ChainConfig, StateDb) {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(1000);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("torus-crash-test-{}-{}", std::process::id(), id));
        let _ = std::fs::create_dir_all(&dir);
        let state_db = StateDb::open(&dir).expect("open test db");
        let config = ChainConfig {
            chain_id: torus_evm::TORUS_CHAIN_ID,
            chain_name: "crash-test".to_string(),
            evm_gas_limit: 30_000_000,
            base_fee_per_gas: 1_000_000_000,
            epoch_length: 100,
            max_validators: 4,
            min_stake: torus_economics::MIN_SELF_DELEGATION,
            fee_burn_bps: 1000,
            fee_validator_bps: 0,
            fee_treasury_bps: 4500,
            fee_dev_pool_bps: 4500,
            treasury_address: Address::ZERO,
            dev_pool_address: Address::ZERO,
            timeout_base_ms: 500,
            reputation_leader_selection: false,
            exec_trust_cache: false,
        };
        (config, state_db)
    }

    fn make_block(height: u64, native_actions: Vec<SignedNativeAction>) -> TorusBlock {
        // Header-identity hardening: the commit-time ancestry check
        // (`detect_parent_link_violation`) compares a block's `parent_hash`
        // against the keccak canonical hash durably recorded for height-1. So a
        // fixture chain must be genuinely linked or execution/replay fail-stops.
        // We link to an EMPTY-ancestor chain (every intermediate block treated as
        // `make_block(h-1, vec![])`), which matches every all-empty and
        // single-height-1 fixture in this module. Height 1 links to the synthetic
        // genesis parent header; height 0 (never a child) keeps `B256::ZERO`.
        // Tests whose persisted parent is NON-empty override `parent_hash`
        // explicitly (see `deferred_trades_*`, `duplicate_*`).
        let parent_hash = match height {
            0 => B256::ZERO,
            1 => alloy_primitives::keccak256(
                torus_bridge::genesis_parent_header().canonical_header_bytes(),
            ),
            _ => alloy_primitives::keccak256(
                make_block(height - 1, vec![]).header.canonical_header_bytes(),
            ),
        };
        TorusBlock {
            header: TorusBlockHeader {
                height,
                parent_hash,
                timestamp: 1000 + height,
                proposer: Address::ZERO,
                state_root: B256::ZERO,
                receipts_root: B256::ZERO,
                logs_bloom: Bloom::ZERO,
                evm_gas_used: 0,
                evm_fee_revenue: 0,
                evm_gas_limit: 30_000_000,
                native_action_count: native_actions.len() as u32,
                evm_tx_count: 0,
                base_fee_per_gas: 1_000_000_000,
                epoch: 0,
                validator_set_hash: B256::ZERO,
                sig_attestation: [0u8; 64],
            },
            native_actions,
            evm_transactions: vec![],
            core_writer_actions: vec![],
        }
    }

    fn sign_claim_rewards(nonce: u64) -> SignedNativeAction {
        let mut seed = [1u8; 32];
        seed[0] = ((nonce % 254) + 1) as u8;
        let key = k256::ecdsa::SigningKey::from_slice(&seed).unwrap();
        torus_types::eip712::sign_native_action(NativeAction::ClaimRewards, nonce, &key)
    }

    fn persist_block_for_test(state_db: &StateDb, block: &TorusBlock) {
        let block_hash = alloy_primitives::keccak256(&block.header.canonical_header_bytes());
        let header_json = serde_json::to_vec(&block.header).unwrap();
        let mut data = Vec::with_capacity(32 + header_json.len());
        data.extend_from_slice(block_hash.as_slice());
        data.extend_from_slice(&header_json);
        state_db
            .put_cf_raw(CF_BLOCK_HEADERS, &block.header.height.to_be_bytes(), &data)
            .unwrap();

        let body = block.body();
        let body_bytes = serde_json::to_vec(&body).unwrap();
        state_db
            .put_cf_raw(
                CF_BLOCK_BODIES,
                &block.header.height.to_be_bytes(),
                &body_bytes,
            )
            .unwrap();
    }

    /// The pruner's meta key, byte-for-byte (torus-state pruner.rs
    /// `PRUNE_META_KEY`): 16 bytes, first byte 0x5F ('_'), stored in
    /// `cf_block_headers` where it sorts AFTER every 8-byte BE height key.
    const PRUNE_META_KEY: &[u8; 16] = b"__prune_meta__\x00\x00";

    /// T1.6 RED-first: with the pruner's meta key present in
    /// `cf_block_headers`, `find_last_committed_height` must skip it and
    /// return the real last height.
    ///
    /// RED on pre-fix code: the function read only the LAST key of the CF;
    /// after the first prune that key is the 16-byte meta key, so it returned
    /// `None` and crash-replay was silently skipped. GREEN after: non-8-byte
    /// keys are skipped, so the true height (7) is found.
    #[test]
    fn find_last_committed_height_skips_prune_meta_key() {
        let (_config, state_db) = make_test_config_and_db();
        for h in 1..=7u64 {
            persist_block_for_test(&state_db, &make_block(h, vec![]));
        }
        state_db
            .put_cf_raw(CF_BLOCK_HEADERS, PRUNE_META_KEY, &3u64.to_be_bytes())
            .unwrap();

        assert_eq!(
            find_last_committed_height(&state_db),
            Some(7),
            "prune meta key in cf_block_headers must not mask the last committed height"
        );
    }

    /// P1(b) RED-first: the end-first scan must SKIP the pruner's non-height
    /// meta key AND CONTINUE past a transient iterator read error to still
    /// surface the real committed height sitting below both.
    ///
    /// This drives `scan_last_committed_height` directly with a synthetic
    /// end-first stream — a live RocksDB cannot be coerced into yielding an
    /// iterator `Err`, so this is the only way to exercise the fault path.
    ///
    /// RED on pre-fix code: `find_last_committed_height` did
    /// `let Ok((key, _)) = entry else { return None };` — the injected `Err`
    /// aborted the whole scan and returned `None`, masking height 42 and
    /// silently skipping crash-replay. GREEN after: the error is logged and
    /// skipped, so the true height (42) is found.
    #[test]
    fn scan_last_committed_height_skips_meta_and_continues_past_error() {
        // End-first order, as RocksDB's `IteratorMode::End` would yield:
        //   1. the 16-byte prune meta key (sorts after every height) -> skip,
        //   2. a transient read error                                -> warn+continue,
        //   3. the real 8-byte committed height (42)                 -> return.
        let empty: Box<[u8]> = Vec::new().into_boxed_slice();
        let entries: Vec<Result<(Box<[u8]>, Box<[u8]>), String>> = vec![
            Ok((PRUNE_META_KEY.to_vec().into_boxed_slice(), empty.clone())),
            Err("injected transient rocksdb read error".to_string()),
            Ok((42u64.to_be_bytes().to_vec().into_boxed_slice(), empty)),
        ];

        assert_eq!(
            scan_last_committed_height(entries),
            Some(42),
            "scan must skip the meta key and continue past a read error to find the real height"
        );
    }

    /// P1(b): a scan that finds no 8-byte height key (only a meta key and an
    /// error) must return `None` — the continue-past-error path must not
    /// invent a height where none exists.
    #[test]
    fn scan_last_committed_height_none_when_no_height_present() {
        let empty: Box<[u8]> = Vec::new().into_boxed_slice();
        let entries: Vec<Result<(Box<[u8]>, Box<[u8]>), String>> = vec![
            Ok((PRUNE_META_KEY.to_vec().into_boxed_slice(), empty)),
            Err("read error".to_string()),
        ];

        assert_eq!(
            scan_last_committed_height(entries),
            None,
            "no genuine committed height exists, so the scan must return None"
        );
    }

    /// Header-identity hardening: the authoritative commit-time ancestry check
    /// accepts a block whose `parent_hash` equals the keccak canonical hash of
    /// the locally-finalized parent, and REJECTS (fail-stop latch) one that does
    /// not. This is the sound half of the fork-detection fix — at execution time
    /// the parent is guaranteed applied, so a mismatch is a real ancestry
    /// violation, not a benign race.
    #[test]
    fn parent_link_violation_rejects_mismatched_parent() {
        use std::sync::atomic::AtomicBool;
        let (_config, state_db) = make_test_config_and_db();

        // Finalize a parent at height 5; its durable hash is
        // keccak(canonical_header_bytes) in the first 32 bytes of the record.
        let parent = make_block(5, vec![]);
        persist_block_for_test(&state_db, &parent);
        let correct_parent_hash =
            alloy_primitives::keccak256(parent.header.canonical_header_bytes());

        // Correct link: child at height 6 pointing at the real parent -> accepted,
        // latch stays clear.
        let ef_ok = AtomicBool::new(false);
        assert!(
            !detect_parent_link_violation(&state_db, 6, &correct_parent_hash, &ef_ok),
            "a child whose parent_hash matches the finalized parent must be accepted"
        );
        assert!(!ef_ok.load(Ordering::SeqCst), "no fail-stop on a valid link");

        // Wrong link: child at height 6 with a bogus parent_hash -> rejected AND
        // the fail-stop latch is set (commit means commit).
        let ef_bad = AtomicBool::new(false);
        let bogus = torus_types::B256::repeat_byte(0xEE);
        assert!(
            detect_parent_link_violation(&state_db, 6, &bogus, &ef_bad),
            "a child whose parent_hash mismatches the finalized parent must be rejected"
        );
        assert!(
            ef_bad.load(Ordering::SeqCst),
            "an ancestry violation must latch the execution fail-stop"
        );
    }

    /// Genesis edge of the ancestry check: the height-1 block's parent is the
    /// synthetic `genesis_parent_header` (never persisted), so its expected
    /// parent hash is computed directly. The correct genesis link is accepted;
    /// a wrong one is rejected.
    #[test]
    fn parent_link_violation_genesis_edge() {
        use std::sync::atomic::AtomicBool;
        let (_config, state_db) = make_test_config_and_db();

        let genesis_hash = alloy_primitives::keccak256(
            torus_bridge::genesis_parent_header().canonical_header_bytes(),
        );
        let ef_ok = AtomicBool::new(false);
        assert!(
            !detect_parent_link_violation(&state_db, 1, &genesis_hash, &ef_ok),
            "height-1 block committing to the genesis header hash must be accepted"
        );

        let ef_bad = AtomicBool::new(false);
        assert!(
            detect_parent_link_violation(&state_db, 1, &torus_types::B256::ZERO, &ef_bad),
            "height-1 block whose parent_hash is not the genesis header hash must be rejected"
        );
        assert!(ef_bad.load(Ordering::SeqCst));
    }

    /// T1.6 RED-first: crash-replay must still RUN after the pruner has
    /// written its meta key. Committed tip = 5, applied = 2 (execution died
    /// mid-pipeline), prune meta present.
    ///
    /// RED on pre-fix code: `find_last_committed_height` returned `None`, so
    /// `replay_committed` bailed at its first match arm — the returned header
    /// stayed at genesis (height 0) and the applied height stayed 2. GREEN
    /// after: the committed tip (5) is found, the gap is detected, and the
    /// (empty) block is marked applied.
    #[test]
    fn crash_replay_runs_after_pruning() {
        let (config, state_db) = make_test_config_and_db();
        let exec_ctx = make_exec_ctx(&config, &state_db);

        for h in 1..=5u64 {
            persist_block_for_test(&state_db, &make_block(h, vec![]));
        }
        write_native_applied_height(&state_db, 2);
        state_db
            .put_cf_raw(CF_BLOCK_HEADERS, PRUNE_META_KEY, &1u64.to_be_bytes())
            .unwrap();

        let (last, parked) = TorusApp::replay_committed(&state_db, &exec_ctx);

        assert_eq!(
            parked, None,
            "a fully-replayable gap must NOT park a hole"
        );
        assert_eq!(
            last.height, 5,
            "replay must find the committed tip despite the prune meta key"
        );
        assert_eq!(
            read_native_applied_height(&state_db),
            Some(5),
            "replay must close the execution gap left by the crash"
        );
    }

    /// Persist ONLY the header for a block (no body) — simulates a committed block whose body was
    /// never persisted or was pruned. Mirrors the header half of `persist_block_for_test`.
    fn persist_header_only_for_test(state_db: &StateDb, block: &TorusBlock) {
        let block_hash = alloy_primitives::keccak256(&block.header.canonical_header_bytes());
        let header_json = serde_json::to_vec(&block.header).unwrap();
        let mut data = Vec::with_capacity(32 + header_json.len());
        data.extend_from_slice(block_hash.as_slice());
        data.extend_from_slice(&header_json);
        state_db
            .put_cf_raw(CF_BLOCK_HEADERS, &block.header.height.to_be_bytes(), &data)
            .unwrap();
    }

    /// Persist ONLY the body for a block (no header write) — simulates a late block-sync
    /// delivery that lands the missing native bodies into `CF_BLOCK_BODIES` for a height
    /// whose header is already durable. Mirrors the body half of `persist_block_for_test`.
    fn persist_block_body_for_test(state_db: &StateDb, block: &TorusBlock) {
        let body_bytes = serde_json::to_vec(&block.body()).unwrap();
        state_db
            .put_cf_raw(CF_BLOCK_BODIES, &block.header.height.to_be_bytes(), &body_bytes)
            .unwrap();
    }

    /// T156-F2 (T2) RED-first: `replay_gap` must replay EVERY height in the `(applied, committed]`
    /// gap, in ascending order — not just the single top committed block.
    ///
    /// RED on pre-fix code: `replay_gap` did not exist (the whole seam is new), and the old
    /// `replay_committed` only ever loaded+executed the single `committed` height, so an intermediate
    /// height (H-1) was silently skipped and never executed. GREEN after: both H-1 and H execute, in
    /// order, and the outcome carries the committed (H) header.
    #[test]
    fn replay_gap_executes_whole_gap_in_order() {
        // committed = 3, applied = 1 => gap must execute heights 2 then 3.
        let b2 = make_block(2, vec![sign_claim_rewards(2)]);
        let b3 = make_block(3, vec![sign_claim_rewards(3)]);
        assert_eq!(b2.header.native_action_count, 1, "sanity: non-empty header");

        let headers: std::collections::HashMap<u64, TorusBlockHeader> =
            [(2u64, b2.header.clone()), (3u64, b3.header.clone())]
                .into_iter()
                .collect();
        let bodies: std::collections::HashMap<u64, TorusBlockBody> =
            [(2u64, b2.body()), (3u64, b3.body())].into_iter().collect();

        let mut executed_order: Vec<u64> = Vec::new();
        let outcome = replay_gap(
            1,
            3,
            |h| headers.get(&h).cloned(),
            |h| bodies.get(&h).cloned(),
            |block| executed_order.push(block.header.height),
        );

        assert_eq!(
            executed_order,
            vec![2, 3],
            "replay must execute the whole gap in ascending order"
        );
        match outcome {
            ReplayGapOutcome::Complete(h) => assert_eq!(h.height, 3),
            other => panic!("expected Complete(3), got {other:?}"),
        }
    }

    /// T156-F2 (T3) RED-first: when a body inside the gap is missing while its header says the block
    /// is non-empty (e.g. a pruned body), `replay_gap` must FAIL LOUD (`Hole`) and must NOT execute
    /// or mark that height (or any height past it) applied.
    ///
    /// RED on pre-fix code: no `replay_gap` seam existed; the old code's "no body -> mark applied"
    /// arm would have silently advanced the applied marker for the pruned block (state loss). GREEN
    /// after: the outcome is `Hole { height: H-1 }`, nothing in the gap executed, and H is never
    /// reached.
    #[test]
    fn replay_gap_holes_on_missing_body_without_marking_applied() {
        // committed = 3, applied = 1 => gap = {2, 3}. Body for height 2 (non-empty header) is missing.
        let b2 = make_block(2, vec![sign_claim_rewards(2)]);
        let b3 = make_block(3, vec![sign_claim_rewards(3)]);

        let headers: std::collections::HashMap<u64, TorusBlockHeader> =
            [(2u64, b2.header.clone()), (3u64, b3.header.clone())]
                .into_iter()
                .collect();
        // Deliberately NO body for height 2.
        let bodies: std::collections::HashMap<u64, TorusBlockBody> =
            [(3u64, b3.body())].into_iter().collect();

        let mut executed_order: Vec<u64> = Vec::new();
        let outcome = replay_gap(
            1,
            3,
            |h| headers.get(&h).cloned(),
            |h| bodies.get(&h).cloned(),
            |block| executed_order.push(block.header.height),
        );

        assert!(
            executed_order.is_empty(),
            "a hole at height 2 must stop replay before executing anything past it"
        );
        match outcome {
            ReplayGapOutcome::Hole { height, .. } => assert_eq!(
                height, 2,
                "the hole must identify the unreconstructable height"
            ),
            other => panic!("expected Hole {{ height: 2 }}, got {other:?}"),
        }
    }

    /// FIX 1b RED-first, end-to-end through `replay_committed`: a committed block with a
    /// missing body inside the gap must NOT latch a pre-network fail-stop. It PARKS the hole
    /// (returns `parked = Some(hole)`) after replaying every height below it, so boot completes
    /// and the live heal loop + block-sync backfill get a bounded window to close the hole.
    /// The applied marker still stops BELOW the hole — a height is NEVER marked applied without
    /// being executed.
    ///
    /// RED on pre-fix code: `replay_committed` returned a bare header and LATCHED `exec_failed`
    /// on a gap hole (pre-network fail-stop / boot into fail-stop). GREEN after FIX 1b: heights
    /// below the hole are replayed (applied advances to 3), `exec_failed` is NOT latched, and the
    /// returned tuple carries `parked = Some(4)` with `last_good.height == 3`.
    #[test]
    fn replay_committed_parks_hole_on_missing_body_in_gap() {
        let (config, state_db) = make_test_config_and_db();
        let exec_ctx = make_exec_ctx(&config, &state_db);

        // Heights 1..=3 are empty (bodies present); height 4 has a NON-EMPTY header but NO body
        // (lost in the commit->execute kill window); height 5 is a later committed tip.
        for h in 1..=3u64 {
            persist_block_for_test(&state_db, &make_block(h, vec![]));
        }
        persist_header_only_for_test(&state_db, &make_block(4, vec![sign_claim_rewards(4)]));
        persist_block_for_test(&state_db, &make_block(5, vec![]));

        write_native_applied_height(&state_db, 2);

        let (last, parked) = TorusApp::replay_committed(&state_db, &exec_ctx);

        assert!(
            !exec_ctx.exec_failed.load(std::sync::atomic::Ordering::SeqCst),
            "FIX 1b: a boot gap hole must PARK, not latch a pre-network fail-stop"
        );
        assert_eq!(
            parked,
            Some(4),
            "the parked hole must identify the unreconstructable committed height"
        );
        assert_eq!(
            read_native_applied_height(&state_db),
            Some(3),
            "replay must advance applied only up to the block before the hole (height 3), never past it"
        );
        assert_eq!(
            last.height, 3,
            "the returned header must be the last successfully executed block, not the committed tip"
        );
    }

    /// FIX 1a RED-first (crash-safety, commit->execute kill window). A NON-EMPTY block
    /// committed and made durable AT COMMIT TIME (`persist_committed_block_durably`), then
    /// killed BEFORE execution (applied never advanced), must be fully recoverable on boot:
    /// crash-replay reconstructs it from the durable body and executes it — NO hole, NO
    /// fail-stop.
    ///
    /// RED on pre-fix code: the body was written only at EXECUTION (`execute_committed_block`),
    /// so a SIGKILL in the commit->execute window left `CF_BLOCK_BODIES[3]` empty; boot
    /// `replay_gap` found a committed non-empty height with no body and fail-stopped (devnet
    /// t12-final-a/c: `height=132`/`h224`, `exit(70)`). GREEN after FIX 1a: the commit-time
    /// write makes the body durable, replay completes, and applied advances to the committed tip.
    #[test]
    fn crash_after_commit_before_execute_recovers_via_durable_body() {
        let (config, state_db) = make_test_config_and_db();
        let exec_ctx = make_exec_ctx(&config, &state_db);

        // Heights 1..=2 fully applied (empty, bodies present).
        for h in 1..=2u64 {
            persist_block_for_test(&state_db, &make_block(h, vec![]));
        }
        write_native_applied_height(&state_db, 2);

        // Height 3: a NON-EMPTY committed block. FIX 1a persists header+body at COMMIT time,
        // BEFORE execution. Simulate a SIGKILL in the commit->execute window: committed and
        // durable, but execution never ran (applied stays 2, no execution-time body write).
        let b3 = make_block(3, vec![sign_claim_rewards(3)]);
        assert_eq!(b3.header.native_action_count, 1, "sanity: non-empty block");
        persist_committed_block_durably(&state_db, &b3);

        // Boot crash-recovery over the gap {3}: the durable-at-commit body lets replay
        // reconstruct and execute the block instead of holing.
        let (last, parked) = TorusApp::replay_committed(&state_db, &exec_ctx);

        assert_eq!(
            parked, None,
            "FIX 1a: a body made durable at commit must let replay COMPLETE, never hole"
        );
        assert!(
            !exec_ctx.exec_failed.load(std::sync::atomic::Ordering::SeqCst),
            "FIX 1a: recovery must not fail-stop when the committed body is durable"
        );
        assert_eq!(
            read_native_applied_height(&state_db),
            Some(3),
            "the recovered committed block must execute (applied advances to the committed tip)"
        );
        assert_eq!(last.height, 3);
    }

    /// FIX 1a: `persist_committed_block_durably` writes BOTH the header (with the committed
    /// block hash in the first 32 bytes, for `detect_conflicting_commit`) and the full body,
    /// so the two crash-replay loaders (`load_replay_header` / `load_replay_body`) both resolve
    /// immediately after commit — not only after execution.
    #[test]
    fn persist_committed_block_durably_writes_header_and_body() {
        let (_config, state_db) = make_test_config_and_db();
        let block = make_block(9, vec![sign_claim_rewards(9), sign_claim_rewards(9)]);
        assert_eq!(block.header.native_action_count, 2);

        persist_committed_block_durably(&state_db, &block);

        let h = load_replay_header(&state_db, 9).expect("header durable at commit");
        assert_eq!(h.height, 9);
        assert_eq!(h.native_action_count, 2);
        let b = load_replay_body(&state_db, 9)
            .expect("FIX 1a: body durable at commit, not only at execution");
        assert_eq!(b.native_actions.len(), 2);

        // The header record must carry the committed block hash (conflict detection).
        let raw = state_db
            .get_cf_raw(CF_BLOCK_HEADERS, &9u64.to_be_bytes())
            .unwrap()
            .unwrap();
        let expected = alloy_primitives::keccak256(&block.header.canonical_header_bytes());
        assert_eq!(&raw[..32], expected.as_slice(), "header record must prefix the block hash");
    }

    /// FIX 1b RED-first (park + heal via the live strict-order queue). A boot-parked hole,
    /// seeded into `deferred_exec` as an `ExecSource::Durable`, must NOT execute while the body
    /// is missing, and must heal IN ORDER the moment the body becomes durable (a late block-sync
    /// write) — draining the whole contiguous run without ever executing out of order.
    ///
    /// RED on pre-fix code: `ExecSource::Durable` did not exist and a boot hole latched
    /// `exec_failed` (die), so there was no "park then heal on late body" path at all. GREEN
    /// after FIX 1b: the durable source re-loads on each drain, so an injected body dispatches
    /// the block and advances the frontier.
    #[test]
    fn parked_durable_hole_heals_on_late_body_injection() {
        let mut app = TorusApp::stub();

        // Two committed heights, 5 (the hole) and 6, seeded exactly as boot parking does.
        let b5 = make_block(5, vec![sign_claim_rewards(5)]);
        let b6 = make_block(6, vec![sign_claim_rewards(6)]);
        // Header durable for both (as at commit); body durable for NEITHER yet (the kill window
        // lost them and no peer has re-delivered).
        persist_header_only_for_test(&app.state_db, &b5);
        persist_header_only_for_test(&app.state_db, &b6);

        app.exec_next_height = Some(5);
        app.exec_hole_since = Some(std::time::Instant::now());
        app.deferred_exec.insert(
            5,
            DeferredExecBlock { source: ExecSource::Durable(5), slashes: Vec::new() },
        );
        app.deferred_exec.insert(
            6,
            DeferredExecBlock { source: ExecSource::Durable(6), slashes: Vec::new() },
        );

        // Drain with both bodies missing: nothing executes, frontier stays at the hole.
        app.drain_exec_queue();
        assert_eq!(app.exec_next_height, Some(5), "a missing-body hole must NOT advance");
        assert!(app.deferred_exec.contains_key(&5), "the hole stays buffered");
        assert!(!app.is_exec_failed(), "still within budget — no latch");

        // Late body injection for the hole (5) ONLY: block-sync wrote 5's body.
        persist_block_body_for_test(&app.state_db, &b5);
        app.drain_exec_queue();
        assert_eq!(
            app.exec_next_height,
            Some(6),
            "the injected body must heal height 5 in order and advance to 6"
        );
        assert!(app.deferred_exec.contains_key(&6), "6 waits — its body is still missing");

        // Inject 6's body too: the run drains fully.
        persist_block_body_for_test(&app.state_db, &b6);
        app.drain_exec_queue();
        assert_eq!(app.exec_next_height, Some(7), "the whole contiguous run heals in order");
        assert!(app.deferred_exec.is_empty(), "queue fully drained");
    }

    /// FIX 1b: the heal budget still applies to a parked boot hole. With the budget set to 0
    /// (fail-stop on the first stalled attempt), a drain that cannot reconstruct the head-of-line
    /// durable source must latch `exec_failed` — the park is time-bounded, never an infinite hole.
    #[test]
    fn parked_durable_hole_fail_stops_on_budget_exhaustion() {
        std::env::set_var("TORUS_EXEC_HOLE_BUDGET_SECS", "0");
        let mut app = TorusApp::stub();

        // Header present, body missing, seeded as a parked hole with the clock started.
        let b5 = make_block(5, vec![sign_claim_rewards(5)]);
        persist_header_only_for_test(&app.state_db, &b5);
        app.exec_next_height = Some(5);
        app.exec_hole_since = Some(std::time::Instant::now());
        app.deferred_exec.insert(
            5,
            DeferredExecBlock { source: ExecSource::Durable(5), slashes: Vec::new() },
        );

        app.drain_exec_queue();

        assert!(
            app.is_exec_failed(),
            "a parked hole that outlives its heal budget must latch the same fail-stop as a live hole"
        );
        std::env::remove_var("TORUS_EXEC_HOLE_BUDGET_SECS");
    }

    // ---- HEAL-CHANNEL fix (part A window + part B peer pull) ----

    /// Part A (kill-window durability of the RECONSTRUCTION KEY). The commit
    /// callback persists the committed DATUM to `CF_COMMIT_MANIFEST` as its FIRST
    /// durable action. A SIGKILL immediately after (before the body reaches
    /// `CF_BLOCK_BODIES`) must still leave the `native_action_hashes` durably
    /// recoverable — that key is what lets a boot-parked hole heal from peers.
    ///
    /// RED at 98b69e0: `persist_commit_manifest` / `load_commit_manifest` /
    /// `CF_COMMIT_MANIFEST` do not exist (compile failure); there was NO durable
    /// artifact carrying the committed height's action hashes.
    #[test]
    fn commit_manifest_persists_native_action_hashes_at_commit() {
        let (_config, state_db) = make_test_config_and_db();
        let actions: Vec<SignedNativeAction> = (0..20).map(sign_claim_rewards).collect();
        // Height 5 (small: `make_block` links parent hashes recursively — the real
        // h1165 recurses too deep for a test stack; the height is cosmetic here).
        let block = make_block(5, actions.clone());

        // Exactly what the commit callback ships: the compact consensus datum.
        let datum = encode_proposal_datum(&block, true);
        persist_commit_manifest(&state_db, 5, &datum);

        // Survives the kill window: reload + decode yields the reconstruction key.
        let loaded = load_commit_manifest(&state_db, 5).expect("manifest durable at commit");
        let compact: CompactBlock =
            bincode::deserialize(&loaded).expect("manifest decodes to the committed CompactBlock");
        let want: Vec<_> = actions.iter().map(torus_types::compute_action_hash).collect();
        assert_eq!(
            compact.native_action_hashes, want,
            "the manifest must carry every committed action hash (the peer-pull key)"
        );
    }

    /// Part B (the essential fix). A boot-parked hole seeded from its durable
    /// manifest must ARM the peer pull: reconstruction against an empty DA store
    /// returns the REAL missing hashes (which `note_exec_hole` fans out to peers),
    /// NOT the empty set the pre-fix `Durable` seed returned.
    ///
    /// RED at 98b69e0: `recovery_exec_source` does not exist (compile failure), and
    /// the boot seed was `ExecSource::Durable(h)` whose `materialize_owned` returns
    /// `Err((_, []))` — an EMPTY missing set — so the parked node pulled NOTHING
    /// (`missing=0`, t12-r3-full h1165) and stalled forever.
    #[test]
    fn boot_parked_hole_arms_peer_pull_with_real_hashes() {
        let (config, state_db) = make_test_config_and_db();
        let actions: Vec<SignedNativeAction> = (0..20).map(sign_claim_rewards).collect();
        let want: Vec<_> = actions.iter().map(torus_types::compute_action_hash).collect();
        // Height 5 (small: `make_block` recurses over parent hashes; h1165 would
        // overflow the test stack). The height is cosmetic to the heal logic.
        let block = make_block(5, actions);

        // Kill-window state: header + manifest durable at commit; body NEVER landed.
        persist_header_only_for_test(&state_db, &block);
        persist_commit_manifest(&state_db, 5, &encode_proposal_datum(&block, true));
        // Mark applied so `TorusApp::new`'s replay is a no-op (this test drives the
        // recovery/reconstruct seams directly, not the boot park path).
        write_native_applied_height(&state_db, 5);

        // The heal-channel seed decodes the manifest to a Compact source.
        let source = recovery_exec_source(&state_db, 5);
        let compact = match source {
            ExecSource::Compact(c) => c,
            _ => panic!("a manifest-backed boot hole must seed ExecSource::Compact"),
        };
        assert_eq!(compact.native_action_hashes, want);

        // Reconstruct against an EMPTY DA store: the miss reports the REAL hashes,
        // so the pull is armed for exactly those bodies.
        let mempool = Arc::new(torus_mempool::Mempool::new(
            state_db.clone(),
            torus_mempool::MempoolConfig::default(),
        ));
        let app = TorusApp::new(state_db.clone(), &config, None, Some(mempool), None);
        let missing = app
            .reconstruct_compact_from_da(&compact)
            .expect_err("empty DA store must miss");
        assert_eq!(
            missing, want,
            "the parked hole must pull the committed height's REAL action hashes, not an empty set"
        );

        // Contrast: the pre-fix Durable seed cannot name any hashes to pull.
        let durable_missing = match app.materialize_owned(ExecSource::Durable(5)) {
            Err((_, missing)) => missing,
            Ok(_) => panic!("no durable body was written — Durable must miss"),
        };
        assert!(
            durable_missing.is_empty(),
            "regression guard: the old Durable seed pulls nothing (an empty miss) — the stall"
        );
    }

    /// Part B end-to-end (strict-order drain). A manifest-seeded parked hole must
    /// NOT advance while its bodies are absent, then HEAL in order the moment the
    /// bodies arrive in the durable DA store (a peer served the `/torus/native-da`
    /// pull) — draining the whole contiguous run without executing out of order.
    ///
    /// RED at 98b69e0: seeded as `ExecSource::Durable`, this hole could heal ONLY
    /// from a local durable body (which never comes — it was lost in the crash);
    /// peer-delivered bodies landing in the DA store did nothing.
    #[test]
    fn boot_parked_hole_heals_from_peer_da_bodies() {
        let mut app = TorusApp::stub();
        let mempool = Arc::new(torus_mempool::Mempool::new(
            app.state_db.clone(),
            torus_mempool::MempoolConfig::default(),
        ));
        app.mempool = Some(mempool.clone());

        let a5: Vec<SignedNativeAction> = vec![sign_claim_rewards(5)];
        let a6: Vec<SignedNativeAction> = vec![sign_claim_rewards(6)];
        let b5 = make_block(5, a5.clone());
        let b6 = make_block(6, a6.clone());

        // Seed exactly as manifest-backed boot parking does: Compact sources.
        app.exec_next_height = Some(5);
        app.exec_hole_since = Some(std::time::Instant::now());
        app.deferred_exec.insert(
            5,
            DeferredExecBlock {
                source: ExecSource::Compact(CompactBlock::from_block(&b5)),
                slashes: Vec::new(),
            },
        );
        app.deferred_exec.insert(
            6,
            DeferredExecBlock {
                source: ExecSource::Compact(CompactBlock::from_block(&b6)),
                slashes: Vec::new(),
            },
        );

        // Bodies absent from the DA store: the hole holds, no out-of-order exec.
        app.drain_exec_queue();
        assert_eq!(app.exec_next_height, Some(5), "a missing-body hole must NOT advance");
        assert!(!app.is_exec_failed(), "still within budget — no latch");

        // A peer serves height 5's bodies into the durable DA store.
        mempool.mirror_native_to_da(&a5);
        app.drain_exec_queue();
        assert_eq!(
            app.exec_next_height,
            Some(6),
            "peer-delivered bodies must heal height 5 in order and advance to 6"
        );

        // Height 6's bodies arrive too: the run drains fully.
        mempool.mirror_native_to_da(&a6);
        app.drain_exec_queue();
        assert_eq!(app.exec_next_height, Some(7), "the whole contiguous run heals in order");
        assert!(app.deferred_exec.is_empty(), "queue fully drained");
    }

    /// Part B retry policy (task B2). The parked-hole heal loop must keep pulling
    /// the missing bodies on EVERY commit — far past 9 attempts — for the full
    /// heal budget, never giving up. (Peer ROTATION is the fetcher's job:
    /// `LibP2PNetwork::fetch_native_actions_from_validators` fans each request to
    /// ALL other validators, bridge.rs.) The "9 retries then fall back to sync"
    /// quote in the evidence is the hotstuff-layer tip fetch
    /// (`tick_pending_body_retries`), a DIFFERENT mechanism — the app-layer hole
    /// heal has no such cap.
    #[test]
    fn parked_hole_heal_retries_far_past_nine_within_budget() {
        use std::sync::Mutex;

        struct RecordingFetcher {
            calls: Mutex<Vec<Vec<[u8; 32]>>>,
        }
        impl NativeDaFetcher for RecordingFetcher {
            fn fetch(&self, hashes: Vec<[u8; 32]>) {
                self.calls.lock().unwrap().push(hashes);
            }
            fn drain(&self) -> Vec<Vec<u8>> {
                Vec::new()
            }
        }

        let mut app = TorusApp::stub();
        let fetcher = Arc::new(RecordingFetcher {
            calls: Mutex::new(Vec::new()),
        });
        app.da_fetcher = Some(fetcher.clone());
        app.exec_hole_since = Some(std::time::Instant::now());

        let missing: Vec<torus_types::B256> =
            (0..20u8).map(|i| torus_types::B256::from([i; 32])).collect();

        // 40 commit-driven retries (>> the hotstuff 9-cap): every one must issue a
        // fresh pull for the REAL missing hashes, and none may fail-stop (default
        // 5-min budget, freshly started).
        for _ in 0..40 {
            app.note_exec_hole(1165, &missing);
        }

        let calls = fetcher.calls.lock().unwrap();
        assert_eq!(calls.len(), 40, "the heal loop must pull on every retry, never give up at 9");
        assert!(
            calls.iter().all(|c| c.len() == missing.len()),
            "every retry pulls the REAL missing bodies (non-empty) — not the stalling empty set"
        );
        assert!(
            !app.is_exec_failed(),
            "40 quick retries must stay within the heal budget — no premature fail-stop"
        );
    }

    /// T1.5 RED-first: a closed execution channel (= the execution thread
    /// died) must FAIL-STOP the node, not let consensus keep finalizing over
    /// frozen state.
    ///
    /// RED on pre-fix code: the failed `send` only logged
    /// "block will not be executed!" and returned — no latch existed
    /// (`exec_failed`/`is_exec_failed` did not compile) and a later committed
    /// block still advanced `last_header` to height 2 (zombie-advance). GREEN
    /// after: the first failed send latches the fail-stop and the second
    /// commit is ignored, freezing the node at height 1.
    #[test]
    fn closed_exec_channel_fail_stops_instead_of_zombie_advancing() {
        use hotstuff_rs::hotstuff::types::PhaseCertificate;
        use hotstuff_rs::types::data_types::BlockHeight;

        let mut app = TorusApp::stub();

        // Simulate execution-thread death: swap in a sender whose receiver is
        // already gone (the old sender drops; the stub's real exec thread
        // exits cleanly on its closed channel).
        let (dead_tx, dead_rx) = std::sync::mpsc::sync_channel::<CommittedBlockMsg>(1);
        drop(dead_rx);
        app.exec_tx = Some(dead_tx);

        let committed = |height: u64| {
            let datum = bincode::serialize(&make_block(height, vec![])).unwrap();
            let hash = TorusApp::hash_datum(&datum);
            Block::new(
                BlockHeight::new(height),
                PhaseCertificate::genesis_pc(),
                CryptoHash::new(hash),
                Data::new(vec![Datum::new(datum)]),
            )
        };

        let b1 = committed(1);
        app.on_committed_block(&b1, b1.hash);
        assert!(
            app.is_exec_failed(),
            "a closed exec channel must latch the fail-stop"
        );
        assert_eq!(app.last_header.height, 1);

        let b2 = committed(2);
        app.on_committed_block(&b2, b2.hash);
        assert_eq!(
            app.last_header.height, 1,
            "node kept finalizing after execution death (zombie-advance)"
        );
    }

    /// FIX C2: `App::on_fatal_safety_violation` (invoked by block sync when a
    /// peer's committed chain conflicts with ours) must latch the shared
    /// `exec_failed` fail-stop — the SAME latch the node binary watches and turns
    /// into `exit(70)`. This proves the wiring reaches process halt (the exit
    /// itself is exercised by the node-side watcher, not this unit test).
    ///
    /// RED before fix: `App::on_fatal_safety_violation` did not exist (compile
    /// failure); a block-sync committed conflict only ended the sync session, so
    /// the node kept voting/finalizing over an already-forked chain.
    #[test]
    fn fatal_safety_violation_latches_fail_stop() {
        use hotstuff_rs::app::App;

        let mut app = TorusApp::stub();
        assert!(
            !app.is_exec_failed(),
            "precondition: a fresh app is not in fail-stop"
        );

        app.on_fatal_safety_violation();

        assert!(
            app.is_exec_failed(),
            "on_fatal_safety_violation must latch the fail-stop so the node halts"
        );
    }

    /// PART 2 (P0 SAFETY, "commit means commit"): a SECOND, DIFFERENT block
    /// arriving at an already-applied height is a conflicting commit — it must
    /// latch the fail-stop, not be silently dropped by the `applied >= height`
    /// skip. A same-hash duplicate stays a benign silent skip.
    ///
    /// RED before fix: `detect_conflicting_commit` did not exist (compile
    /// failure); the skip branch dropped every re-arriving block unconditionally,
    /// so a divergent block at an already-finalized height was silently ignored.
    #[test]
    fn conflicting_commit_at_applied_height_fail_stops() {
        use std::sync::atomic::{AtomicBool, Ordering};

        let (_config, state_db) = make_test_config_and_db();

        // Persist block X at height 5 (models a first, already-applied commit).
        let block_x = make_block(5, vec![sign_claim_rewards(1)]);
        persist_block_for_test(&state_db, &block_x);
        let x_hash = alloy_primitives::keccak256(block_x.header.canonical_header_bytes());

        // Same-hash duplicate → benign, must NOT latch.
        let benign = AtomicBool::new(false);
        assert!(
            !detect_conflicting_commit(&state_db, 5, &x_hash, &benign),
            "the same block re-arriving must be a benign duplicate"
        );
        assert!(
            !benign.load(Ordering::SeqCst),
            "a same-hash duplicate must NOT latch the fail-stop"
        );

        // A DIFFERENT block Y at the same height → conflicting commit, must latch.
        let block_y = make_block(5, vec![sign_claim_rewards(2), sign_claim_rewards(3)]);
        let y_hash = alloy_primitives::keccak256(block_y.header.canonical_header_bytes());
        assert_ne!(x_hash, y_hash, "precondition: the two blocks must hash differently");

        let latch = AtomicBool::new(false);
        assert!(
            detect_conflicting_commit(&state_db, 5, &y_hash, &latch),
            "a different block at an already-persisted height must be flagged as a conflict"
        );
        assert!(
            latch.load(Ordering::SeqCst),
            "a conflicting commit must latch the fail-stop (commit means commit)"
        );

        // No header persisted at an unrelated height → nothing to conflict with.
        let none_latch = AtomicBool::new(false);
        assert!(
            !detect_conflicting_commit(&state_db, 6, &y_hash, &none_latch),
            "a height with no persisted header must not be treated as a conflict"
        );
        assert!(!none_latch.load(Ordering::SeqCst));
    }

    /// Test double: a `NativeDaFetcher` whose body only appears on the
    /// `deliver_on_drain`-th `drain()` — models a body that lands some retries into the
    /// pull. Shared by the sync-budget test and the hot-path pull tests (#4 Task 1).
    struct LateFetcher {
        body: Vec<u8>,
        deliver_on_drain: usize,
        drains: std::sync::atomic::AtomicUsize,
    }
    impl NativeDaFetcher for LateFetcher {
        fn fetch(&self, _hashes: Vec<[u8; 32]>) {}
        fn drain(&self) -> Vec<Vec<u8>> {
            use std::sync::atomic::Ordering;
            let n = self.drains.fetch_add(1, Ordering::Relaxed) + 1;
            if n >= self.deliver_on_drain {
                vec![self.body.clone()]
            } else {
                Vec::new()
            }
        }
    }

    /// Test double: a fetcher that NEVER delivers — models a true push miss whose body
    /// cannot be pulled in-budget, so the hot path must fail the view, not hang (#4 Task 1).
    struct NeverFetcher;
    impl NativeDaFetcher for NeverFetcher {
        fn fetch(&self, _hashes: Vec<[u8; 32]>) {}
        fn drain(&self) -> Vec<Vec<u8>> {
            Vec::new()
        }
    }

    /// Test double (Phase 2.3 #5): models a hash-only PRE-WARM — the body already sits in
    /// the inbound queue (delivered once on the first `drain()`, then gone, as a real pull
    /// delivers each body once). `fetch()` is COUNTED so a test can prove the hot local
    /// retry absorbed the pre-warmed body without issuing a redundant network fetch.
    struct PrewarmFetcher {
        body: Vec<u8>,
        fetches: std::sync::atomic::AtomicUsize,
        drained: std::sync::atomic::AtomicBool,
    }
    impl NativeDaFetcher for PrewarmFetcher {
        fn fetch(&self, _hashes: Vec<[u8; 32]>) {
            self.fetches
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        fn drain(&self) -> Vec<Vec<u8>> {
            if self
                .drained
                .swap(true, std::sync::atomic::Ordering::Relaxed)
            {
                Vec::new()
            } else {
                vec![self.body.clone()]
            }
        }
    }

    /// Test double (BS-4b): counts `fetch()` calls and delivers `body` on the
    /// `deliver_on_drain`-th `drain()` (0 = never). Like a real pull, the body only
    /// lands in the inbound AFTER a request was issued, so `drain()` returns empty
    /// until the first `fetch()` — this makes the F2 pre-warm absorb (which drains
    /// before any fetch) a no-op, so the counted fetches are exactly those
    /// `recover_bodies_bounded` issued. Lets a test assert the mid-budget re-fetch
    /// fires exactly once for a stuck body and never for one that arrives early.
    struct CountingFetcher {
        body: Vec<u8>,
        deliver_on_drain: usize,
        fetch_calls: std::sync::atomic::AtomicUsize,
        drains: std::sync::atomic::AtomicUsize,
    }
    impl NativeDaFetcher for CountingFetcher {
        fn fetch(&self, _hashes: Vec<[u8; 32]>) {
            self.fetch_calls
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        fn drain(&self) -> Vec<Vec<u8>> {
            use std::sync::atomic::Ordering;
            // No response can land before a request is issued (pre-warm absorb drains empty).
            if self.fetch_calls.load(Ordering::Relaxed) == 0 {
                return Vec::new();
            }
            let n = self.drains.fetch_add(1, Ordering::Relaxed) + 1;
            if self.deliver_on_drain != 0 && n >= self.deliver_on_drain {
                vec![self.body.clone()]
            } else {
                Vec::new()
            }
        }
    }

    #[test]
    fn crash_recovery_replays_committed_block() {
        let (config, state_db) = make_test_config_and_db();
        let block = make_block(1, vec![sign_claim_rewards(100)]);

        persist_block_for_test(&state_db, &block);

        assert!(state_db
            .get_cf_raw(CF_CONSENSUS_META, META_NATIVE_APPLIED_HEIGHT)
            .unwrap()
            .is_none());

        let app = TorusApp::new(state_db.clone(), &config, None, None, None);

        let applied = read_native_applied_height(&state_db);
        assert_eq!(applied, Some(1), "replay should set applied height to 1");
        assert_eq!(app.last_header.height, 1, "last_header should be updated");
    }

    #[test]
    fn produce_block_datum_is_compact() {
        // Task 8: with compact proposals enabled (version-gated), the proposal datum
        // carries only action HASHES (CompactBlock), not inline bodies — bodies travel
        // out-of-band via the durable DA store (push T7 / pull T6). Required for 400k
        // orders/sec, where a full block of ~MB exceeds max_consensus_message_size.
        let actions: Vec<SignedNativeAction> = (0..100).map(sign_claim_rewards).collect();
        let block = make_block(7, actions);

        // Compact-mode datum: a CompactBlock referencing all 100 actions by hash.
        let compact_datum = encode_proposal_datum(&block, true);
        let decoded: CompactBlock = bincode::deserialize(&compact_datum)
            .expect("compact-mode datum must be a CompactBlock");
        assert_eq!(
            decoded.native_action_hashes.len(),
            100,
            "compact datum references every action by hash"
        );

        // The FULL encoding remains valid behind the flag (staged rollout / rollback),
        // and is strictly larger — proving the compact datum carries no inline bodies.
        let full_datum = encode_proposal_datum(&block, false);
        let full: TorusBlock = bincode::deserialize(&full_datum)
            .expect("full-mode datum must be a self-contained TorusBlock");
        assert_eq!(full.native_actions.len(), 100);
        assert!(
            compact_datum.len() < full_datum.len(),
            "compact datum must be smaller (bodies are out-of-band, not inline)"
        );
    }

    #[test]
    fn compact_proposal_disseminates() {
        // Task 8: a compact proposal carries only hashes, and a peer reconstructs the
        // full block from bodies delivered out-of-band into its durable DA store
        // (push T7 / pull T6) — so re-enabling compact does not strand any validator.
        let (config, state_db) = make_test_config_and_db();
        let actions: Vec<SignedNativeAction> = (0..10).map(sign_claim_rewards).collect();
        let block = make_block(5, actions.clone());

        // The proposer emits a CompactBlock datum (hashes only).
        let datum = encode_proposal_datum(&block, true);
        let compact: CompactBlock =
            bincode::deserialize(&datum).expect("compact-mode datum is a CompactBlock");
        assert_eq!(compact.native_action_hashes.len(), actions.len());

        // A peer holding the bodies in its DA store (delivered out-of-band) reconstructs
        // the full block — the bodies were NOT in the proposal itself.
        let mempool = Arc::new(torus_mempool::Mempool::new(
            state_db.clone(),
            torus_mempool::MempoolConfig::default(),
        ));
        mempool.mirror_native_to_da(&actions); // models push/pull delivery into the DA store
        let app = TorusApp::new(state_db.clone(), &config, None, Some(mempool), None);
        let reconstructed = app
            .reconstruct_compact_from_da(&compact)
            .expect("peer reconstructs the compact block from its durable DA store");
        assert_eq!(reconstructed.native_actions.len(), actions.len());
        for (got, want) in reconstructed.native_actions.iter().zip(actions.iter()) {
            assert_eq!(
                torus_types::compute_action_hash(got),
                torus_types::compute_action_hash(want),
                "reconstructed bodies match the referenced hashes"
            );
        }
    }

    /// One PlaceOrderBatch action carrying `n_orders` orders (dummy signature — the
    /// proposal-size test below only measures encoded bytes; `compute_action_hash`
    /// omits the signature). Mirrors the bench's bs=N order shape.
    fn big_order_batch_action(nonce: u64, n_orders: usize) -> SignedNativeAction {
        use torus_types::{ActionSignature, OrderType, PlaceOrderParams, Signature, TimeInForce};
        let order = PlaceOrderParams {
            market_id: 1,
            is_buy: true,
            price: FixedPoint::from_raw(6_000_000_000_000),
            quantity: FixedPoint::from_raw(FixedPoint::SCALE),
            order_type: OrderType::Limit,
            time_in_force: TimeInForce::GTC,
            reduce_only: false,
            client_order_id: None,
        };
        SignedNativeAction {
            action: NativeAction::PlaceOrderBatch(vec![order; n_orders]),
            nonce,
            signature: ActionSignature::Eip712(Signature {
                v: 27,
                r: [0u8; 32],
                s: [0u8; 32],
            }),
        }
    }

    /// Task 9 (scale milestone, deterministic proof): the bs=500 flood collapsed
    /// because a block of ~50k orders serializes to ~2.5 MB as a FULL TorusBlock —
    /// far over the then-256 KB `max_consensus_message_size` — so the proposal could
    /// not disseminate and the chain stalled (mem 41ca452). The COMPACT encoding
    /// carries only action hashes, so the SAME block fits comfortably: this is the
    /// encoding change that lets the bs=500 flood hold and unblocks the path to 400k
    /// orders/sec. The collapse reproduces against today's raised gate too: the full
    /// block is ~2.5 MB, over the 1 MB accept limit.
    #[test]
    fn compact_proposal_holds_where_full_block_collapsed_at_bs500() {
        // Mirrors torus-network `NetworkConfig::default().max_consensus_message_size`
        // (torus-consensus has no torus-network dep). O5 raised it 256 KB -> 1 MB;
        // keep in sync with config.rs.
        const MAX_CONSENSUS_MESSAGE_SIZE: usize = 1024 * 1024;

        // The bs=500 block shape: 100 actions x 500-order batches = 50k orders.
        let actions: Vec<SignedNativeAction> = (0..100)
            .map(|i| big_order_batch_action(i as u64, 500))
            .collect();
        let block = make_block(9, actions);

        let full = encode_proposal_datum(&block, false);
        let compact = encode_proposal_datum(&block, true);

        assert!(
            full.len() > MAX_CONSENSUS_MESSAGE_SIZE,
            "full block ({} bytes) exceeds the 256KB consensus limit — reproduces the bs=500 collapse",
            full.len()
        );
        assert!(
            compact.len() < MAX_CONSENSUS_MESSAGE_SIZE,
            "compact block ({} bytes) fits the 256KB consensus limit — the fix",
            compact.len()
        );
        // Bodies are entirely out-of-band: compact is orders-of-magnitude smaller.
        assert!(
            compact.len() * 50 < full.len(),
            "compact ({} bytes) must be >50x smaller than full ({} bytes)",
            compact.len(),
            full.len()
        );
    }

    #[test]
    fn compactblock_reconstructs_from_da_store() {
        // Reproduction of the livelock fix: a CompactBlock referencing a native body
        // that lives ONLY in the durable DA store -- never admitted to the ephemeral
        // nonce-gated mempool -- must reconstruct. Previously reconstruction read the
        // mempool, missed, and the block was rejected (+ peer blacklisted on the sync
        // path), wedging the chain (root cause, mem 28e1a821).
        let (config, state_db) = make_test_config_and_db();

        let body = sign_claim_rewards(123);
        let body_hash = torus_types::compute_action_hash(&body);

        let mempool = Arc::new(torus_mempool::Mempool::new(
            state_db.clone(),
            torus_mempool::MempoolConfig::default(),
        ));
        // Mirror to the DA store WITHOUT mempool admission (mirror_native_to_da does
        // not touch the in-memory pool) -- exactly the "in DA, not in mempool" state.
        mempool.mirror_native_to_da(std::slice::from_ref(&body));
        assert!(
            mempool.get_native_by_hash(&body_hash).is_none(),
            "precondition: body is NOT in the ephemeral mempool pool"
        );
        assert!(
            mempool.get_native_da(&body_hash).is_some(),
            "precondition: body IS in the durable DA store"
        );

        let app = TorusApp::new(state_db.clone(), &config, None, Some(mempool), None);

        // A CompactBlock referencing the body by hash reconstructs from the DA store.
        let full = make_block(1, vec![body.clone()]);
        let compact = CompactBlock::from_block(&full);
        let reconstructed = app
            .reconstruct_compact_from_da(&compact)
            .expect("CompactBlock must reconstruct from the DA store");
        assert_eq!(reconstructed.native_actions.len(), 1);
        assert_eq!(
            torus_types::compute_action_hash(&reconstructed.native_actions[0]),
            body_hash,
            "reconstructed body must match the referenced hash"
        );

        // A body present in NEITHER store -> typed miss (Err of the missing hashes),
        // the signal the rare pull-fallback consumes (Task 6) -- not a hard reject
        // that blacklists a peer.
        let unknown = make_block(2, vec![sign_claim_rewards(999)]);
        let unknown_compact = CompactBlock::from_block(&unknown);
        let missing = app
            .reconstruct_compact_from_da(&unknown_compact)
            .expect_err("an absent body must report a typed miss");
        assert_eq!(missing.len(), 1);
    }

    #[test]
    fn no_replay_when_already_applied() {
        let (config, state_db) = make_test_config_and_db();
        let block = make_block(5, vec![sign_claim_rewards(200)]);

        persist_block_for_test(&state_db, &block);

        state_db
            .put_cf_raw(
                CF_CONSENSUS_META,
                META_NATIVE_APPLIED_HEIGHT,
                &5u64.to_be_bytes(),
            )
            .unwrap();

        let app = TorusApp::new(state_db.clone(), &config, None, None, None);
        assert_eq!(read_native_applied_height(&state_db), Some(5));
        assert_eq!(app.last_header.height, 0);
    }

    #[test]
    fn replay_empty_block_marks_applied() {
        let (config, state_db) = make_test_config_and_db();
        let block = make_block(3, vec![]);

        let block_hash = alloy_primitives::keccak256(&block.header.canonical_header_bytes());
        let header_json = serde_json::to_vec(&block.header).unwrap();
        let mut data = Vec::with_capacity(32 + header_json.len());
        data.extend_from_slice(block_hash.as_slice());
        data.extend_from_slice(&header_json);
        state_db
            .put_cf_raw(CF_BLOCK_HEADERS, &block.header.height.to_be_bytes(), &data)
            .unwrap();

        // ADAPTED for T156-F2 whole-gap replay: replay now walks the ENTIRE
        // [applied+1, committed] gap, not just the single top committed height.
        // This test only persists height 3's header, so seed applied=2 to make
        // the gap exactly {3}; otherwise heights 1..2 would be missing headers
        // and replay would (correctly) fail-stop on the hole at height 1. The
        // intent is unchanged: an empty committed block in the gap is executed
        // (empty body) and marked applied.
        state_db
            .put_cf_raw(CF_CONSENSUS_META, META_NATIVE_APPLIED_HEIGHT, &2u64.to_be_bytes())
            .unwrap();

        let _app = TorusApp::new(state_db.clone(), &config, None, None, None);
        assert_eq!(
            read_native_applied_height(&state_db),
            Some(3),
            "empty block should be marked as applied"
        );
    }

    // ---- live-path replay dedup (duplicate native inclusion) ----

    /// Build an `ExecutionContext` directly (mirrors `TorusApp::new`) so a test can
    /// drive `execute_committed_block` for several heights synchronously.
    fn make_exec_ctx(config: &ChainConfig, state_db: &StateDb) -> ExecutionContext {
        // Default: no mempool, cache off -> all misses = today's full-recover
        // behavior, exactly what the existing exec tests assert.
        make_exec_ctx_with_mempool(config, state_db, None, false)
    }

    fn make_exec_ctx_with_mempool(
        config: &ChainConfig,
        state_db: &StateDb,
        mempool: Option<Arc<Mempool>>,
        exec_trust_cache: bool,
    ) -> ExecutionContext {
        ExecutionContext {
            state_db: state_db.clone(),
            validator: BlockValidator::new(
                config.chain_id,
                config.epoch_length,
                config.max_validators,
                config.treasury_address,
                config.dev_pool_address,
            ),
            evm_executor: EvmExecutor::new(config.chain_id),
            staking: StakingManager::new(state_db.clone()),
            epoch_length: config.epoch_length,
            max_validators: config.max_validators,
            treasury_address: config.treasury_address,
            dev_pool_address: config.dev_pool_address,
            metrics: None,
            mempool,
            exec_trust_cache,
            // None -> trades write inline through the overlay (pre-O3 behavior),
            // keeping these tests' reads deterministic right after execution.
            trade_writer: None,
            exec_failed: Arc::new(AtomicBool::new(false)),
        }
    }

    /// GATE (double-verify-trust-cache T5): the exec trust-cache must be a pure
    /// performance optimization. With a real mempool-populated cache, the resolved
    /// senders from the cached verify MUST equal the uncached (full-recover) verify
    /// action-for-action — including an invalid signature (-> None, the slashing
    /// input that fires app.rs:303-320) and a gossip-TRUSTED action
    /// (verified_locally=false -> not cached -> re-verified, never short-circuited).
    #[test]
    fn trust_cache_resolved_senders_identical_cached_vs_uncached() {
        use torus_types::ActionSignature;
        let (_config, state_db) = make_test_config_and_db();
        let mempool = Mempool::new(state_db.clone(), torus_mempool::MempoolConfig::default());
        let base = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;

        let k1 = k256::ecdsa::SigningKey::from_slice(&[11u8; 32]).unwrap();
        let k2 = k256::ecdsa::SigningKey::from_slice(&[12u8; 32]).unwrap();

        // (0) EIP-712 admitted via the verified ingress path -> seeds the cache.
        let a_cached =
            torus_types::eip712::sign_native_action(NativeAction::ClaimRewards, base + 1, &k1);
        let s_cached = a_cached.recover_sender().unwrap();
        mempool
            .add_native_action_presigned(s_cached, a_cached.clone())
            .unwrap();

        // (1) EIP-712 valid but never admitted -> cache MISS -> full recover.
        let a_uncached =
            torus_types::eip712::sign_native_action(NativeAction::ClaimRewards, base + 2, &k2);
        let s_uncached = a_uncached.recover_sender().unwrap();

        // (2) EIP-712 with a corrupted signature -> invalid -> None (slash input).
        let mut a_invalid =
            torus_types::eip712::sign_native_action(NativeAction::ClaimRewards, base + 3, &k1);
        if let ActionSignature::Eip712(ref mut sig) = a_invalid.signature {
            sig.r = [0xff; 32];
            sig.s = [0xff; 32];
        }

        // (3) gossip-TRUSTED (sender claimed by a peer, verified_locally=false) ->
        //     NOT cached -> exec must re-verify it (no short-circuit).
        let a_trusted =
            torus_types::eip712::sign_native_action(NativeAction::ClaimRewards, base + 4, &k2);
        let s_trusted = a_trusted.recover_sender().unwrap();
        mempool
            .add_native_action_from_gossip_trusted(s_trusted, a_trusted.clone())
            .unwrap();

        let actions = vec![a_cached, a_uncached, a_invalid, a_trusted.clone()];

        let uncached = torus_types::eip712::batch_verify_native_actions(&actions, base, |_| None);
        let cached = torus_types::eip712::batch_verify_native_actions_cached(
            &actions,
            base,
            |_| None,
            |key| mempool.verified_sender(key),
        );

        assert_eq!(
            cached, uncached,
            "DETERMINISM GATE: cache-on resolved senders must equal cache-off"
        );
        // Structural sanity across the four provenance classes.
        assert_eq!(cached[0], Some(s_cached), "cached HIT == fresh recover");
        assert_eq!(cached[1], Some(s_uncached), "MISS -> full recover");
        assert_eq!(cached[2], None, "invalid sig -> None (slash path input)");
        assert_eq!(
            cached[3],
            Some(s_trusted),
            "trusted re-verified, not short-circuited"
        );

        // Provenance: the trusted action was never seeded into the cache.
        let trusted_key = torus_types::verified_cache_key(&a_trusted).unwrap();
        assert_eq!(
            mempool.verified_sender(&trusted_key),
            None,
            "gossip-trusted (verified_locally=false) must not be cached"
        );
    }

    /// GATE (T5, state level): executing the SAME block with the trust-cache warm
    /// (HIT) vs cold (no mempool) must reach identical state. A correct cache reuses
    /// exactly the sender a fresh recover yields, so execution is unchanged.
    #[test]
    fn trust_cache_execution_state_identical_cached_vs_uncached() {
        let amount = U256::from(FixedPoint::ONE.raw() as u128);
        let key = k256::ecdsa::SigningKey::from_slice(&[21u8; 32]).unwrap();
        let signed = torus_types::eip712::sign_native_action(
            NativeAction::TransferToPerp { amount },
            424_242,
            &key,
        );
        let sender = signed.recover_sender().unwrap();
        let start = amount * U256::from(10u8);

        // --- Cold cache (no mempool): full recover, today's path. ---
        let (config_off, db_off) = make_test_config_and_db();
        let ctx_off = make_exec_ctx_with_mempool(&config_off, &db_off, None, false);
        fund_evm_balance(&db_off, sender, start);
        ctx_off.execute_committed_block(&make_block(1, vec![signed.clone()]), vec![]);
        let off_balance = read_evm_balance(&db_off, sender);

        // --- Warm cache (HIT): seed the signature-committing key -> sender. ---
        let (config_on, db_on) = make_test_config_and_db();
        let mempool = Arc::new(Mempool::new(
            db_on.clone(),
            torus_mempool::MempoolConfig::default(),
        ));
        let cache_key = torus_types::verified_cache_key(&signed).unwrap();
        mempool.cache_verified_sender(cache_key, sender);
        assert_eq!(
            mempool.verified_sender(&cache_key),
            Some(sender),
            "precondition: cache is warm so exec takes the HIT path"
        );
        // Flag ON so the warm cache is actually consulted (the HIT path).
        let ctx_on = make_exec_ctx_with_mempool(&config_on, &db_on, Some(mempool), true);
        fund_evm_balance(&db_on, sender, start);
        ctx_on.execute_committed_block(&make_block(1, vec![signed.clone()]), vec![]);
        let on_balance = read_evm_balance(&db_on, sender);

        assert_eq!(
            on_balance, off_balance,
            "execution with a warm trust-cache must reach identical state as cold"
        );
        assert_eq!(
            start - on_balance,
            amount,
            "executed exactly once, correct debit"
        );
    }

    /// T6: with `--exec-trust-cache` OFF (the default), the cache is never consulted
    /// at exec — even a POISONED entry is ignored and the action is fully recovered
    /// (today's behavior). This is what makes the flag a safe default-off rollback
    /// switch.
    #[test]
    fn trust_cache_flag_off_bypasses_cache() {
        let amount = U256::from(FixedPoint::ONE.raw() as u128);
        let key = k256::ecdsa::SigningKey::from_slice(&[31u8; 32]).unwrap();
        let signed = torus_types::eip712::sign_native_action(
            NativeAction::TransferToPerp { amount },
            424_242,
            &key,
        );
        let real_sender = signed.recover_sender().unwrap();
        let start = amount * U256::from(10u8);

        let (config, db) = make_test_config_and_db();
        let mempool = Arc::new(Mempool::new(
            db.clone(),
            torus_mempool::MempoolConfig::default(),
        ));
        // POISON the cache: map this action's key to a bogus sender. If exec ever
        // consulted the cache, the deposit would be attributed to `bogus` and the
        // real sender would NOT be debited.
        let bogus = Address::from([0xCD; 20]);
        assert_ne!(bogus, real_sender);
        let cache_key = torus_types::verified_cache_key(&signed).unwrap();
        mempool.cache_verified_sender(cache_key, bogus);

        // Flag OFF -> cache ignored -> real sender recovered + debited.
        let ctx = make_exec_ctx_with_mempool(&config, &db, Some(mempool), false);
        fund_evm_balance(&db, real_sender, start);
        ctx.execute_committed_block(&make_block(1, vec![signed.clone()]), vec![]);

        assert_eq!(
            start - read_evm_balance(&db, real_sender),
            amount,
            "flag OFF must full-recover the REAL sender, ignoring the poisoned cache"
        );
        assert_eq!(
            read_evm_balance(&db, bogus),
            U256::ZERO,
            "poisoned cache entry must never be used when the flag is off"
        );
    }

    /// Seed an EVM balance into CF_ACCOUNTS (72-byte record: balance ++ nonce ++ code_hash).
    /// `Lockbox::get_evm_balance` reads the first 32 bytes big-endian.
    fn fund_evm_balance(state_db: &StateDb, addr: Address, balance: U256) {
        let mut rec = vec![0u8; 72];
        rec[..32].copy_from_slice(&balance.to_be_bytes::<32>());
        state_db
            .put_cf_raw(torus_state::cf::CF_ACCOUNTS, addr.as_slice(), &rec)
            .unwrap();
    }

    fn read_evm_balance(state_db: &StateDb, addr: Address) -> U256 {
        match state_db
            .get_cf_raw(torus_state::cf::CF_ACCOUNTS, addr.as_slice())
            .unwrap()
        {
            Some(d) if d.len() >= 32 => U256::from_be_slice(&d[..32]),
            _ => U256::ZERO,
        }
    }

    /// REGRESSION (correctness/fund-safety): an identical native action committed in
    /// several consecutive blocks must be EXECUTED exactly once.
    ///
    /// The non-destructive mempool selection × HotStuff 3-chain pipeline re-includes
    /// the same action in blocks N, N+1, N+2 before N commits (memory b206f59c). The
    /// live commit path had NO per-(sender,nonce) replay guard, so each inclusion
    /// re-executed — proven on devnet: one TransferToPerp credited 3x
    /// (devnet/scripts/native-transfer-probe.py). Here we commit the SAME signed
    /// TransferToPerp in three blocks and assert the sender's EVM balance is debited
    /// for exactly ONE deposit (not three).
    #[test]
    fn duplicate_committed_native_action_executes_once() {
        let (config, state_db) = make_test_config_and_db();
        let exec_ctx = make_exec_ctx(&config, &state_db);

        // amount is a raw 8-decimal FixedPoint (u256_to_fp), NOT 18-decimal wei.
        let amount_raw: u128 = FixedPoint::ONE.raw() as u128; // 1.0 == 100_000_000
        let amount = U256::from(amount_raw);
        let key = k256::ecdsa::SigningKey::from_slice(&[7u8; 32]).unwrap();
        let signed = torus_types::eip712::sign_native_action(
            NativeAction::TransferToPerp { amount },
            424_242,
            &key,
        );
        let sender = signed.recover_sender().expect("recover sender");

        // Fund enough EVM balance for THREE deposits, so the only thing that can stop
        // a 2nd/3rd execution is correct dedup — never insufficient funds.
        let start_balance = amount * U256::from(10u8);
        fund_evm_balance(&state_db, sender, start_balance);

        // Same action committed in three consecutive blocks (the proven bug scenario).
        // Link each child to its ACTUAL (non-empty) parent so the commit-time
        // ancestry check passes and the per-(sender,nonce) replay guard — not a
        // parent-link fail-stop — is what dedups the 2nd/3rd inclusion.
        let mut parent: Option<TorusBlock> = None;
        for height in 1..=3u64 {
            let mut block = make_block(height, vec![signed.clone()]);
            if let Some(p) = &parent {
                block.header.parent_hash =
                    alloy_primitives::keccak256(p.header.canonical_header_bytes());
            }
            exec_ctx.execute_committed_block(&block, vec![]);
            parent = Some(block);
        }

        let evm_after = read_evm_balance(&state_db, sender);
        let debited = start_balance - evm_after;
        assert_eq!(
            debited, amount,
            "TransferToPerp committed in 3 blocks must debit EVM balance ONCE (got {debited}, \
             expected {amount}); duplicate execution = fund-safety bug",
        );

        // The consumed nonce must be recorded in CF_NATIVE_NONCES.
        let nonce_key = torus_state::cf::native_nonce_key(&sender, signed.nonce);
        assert!(
            state_db
                .get_cf_raw(torus_state::cf::CF_NATIVE_NONCES, &nonce_key)
                .unwrap()
                .is_some(),
            "consumed nonce should be recorded",
        );
    }

    /// O3: with a background trade writer attached, fills buffer their
    /// trade-history KVs during exec (`ctx.defer_trades`) and the writer
    /// persists them off the execution thread. Dropping the ExecutionContext
    /// closes the writer's channel, drains the queue, and joins — the shutdown
    /// flush ordering — so every row must be durable afterwards.
    #[test]
    fn deferred_trades_reach_db_via_background_writer() {
        let (config, state_db) = make_test_config_and_db();
        let mut exec_ctx = make_exec_ctx(&config, &state_db);
        exec_ctx.trade_writer = Some(torus_state::BackgroundCfWriter::spawn(
            state_db.clone(),
            "test-trade-writer",
            8,
        ));

        let k_sell = k256::ecdsa::SigningKey::from_slice(&[31u8; 32]).unwrap();
        let k_buy = k256::ecdsa::SigningKey::from_slice(&[32u8; 32]).unwrap();

        // Block 1: fund native balances through the public path (EVM balance ->
        // TransferToPerp), so block 2's margin reserve succeeds.
        let deposit_raw: u128 = 1_000 * FixedPoint::ONE.raw() as u128; // 1000.0
        let deposit = U256::from(deposit_raw);
        let mut transfers = Vec::new();
        for (i, key) in [&k_sell, &k_buy].into_iter().enumerate() {
            let signed = torus_types::eip712::sign_native_action(
                NativeAction::TransferToPerp { amount: deposit },
                9_000 + i as u64,
                key,
            );
            fund_evm_balance(&state_db, signed.recover_sender().unwrap(), deposit);
            transfers.push(signed);
        }
        let block1 = make_block(1, transfers);
        exec_ctx.execute_committed_block(&block1, vec![]);

        // Block 2: a resting sell crossed by a buy -> exactly one fill.
        let order = |is_buy: bool| torus_types::PlaceOrderParams {
            market_id: 1,
            is_buy,
            price: FixedPoint::from_raw(100 * FixedPoint::SCALE),
            quantity: FixedPoint::from_raw(FixedPoint::SCALE),
            order_type: torus_types::OrderType::Limit,
            time_in_force: torus_types::TimeInForce::GTC,
            reduce_only: false,
            client_order_id: None,
        };
        let sell = torus_types::eip712::sign_native_action(
            NativeAction::PlaceOrder(order(false)),
            9_002,
            &k_sell,
        );
        let buy = torus_types::eip712::sign_native_action(
            NativeAction::PlaceOrder(order(true)),
            9_003,
            &k_buy,
        );
        // Link block 2 to the ACTUAL (non-empty) block 1 so the commit-time
        // ancestry check passes (block 1 persisted its own canonical hash).
        let mut block2 = make_block(2, vec![sell, buy]);
        block2.header.parent_hash =
            alloy_primitives::keccak256(block1.header.canonical_header_bytes());
        exec_ctx.execute_committed_block(&block2, vec![]);

        // Shutdown flush ordering: drop drains + joins the writer before reads.
        drop(exec_ctx);

        let trades =
            StateBackend::iterate_cf(&state_db, torus_state::cf::CF_NATIVE_TRADES, None).unwrap();
        let user_trades =
            StateBackend::iterate_cf(&state_db, torus_state::cf::CF_NATIVE_USER_TRADES, None)
                .unwrap();
        assert_eq!(
            trades.len(),
            1,
            "one fill -> one trade row via the background writer"
        );
        assert_eq!(user_trades.len(), 2, "maker + taker user-trade rows");
    }

    #[test]
    fn duplicate_committed_native_batch_consumes_nonce_once() {
        // Phase B / Task B5: a PlaceOrderBatch carries many orders but ONE (sender, nonce),
        // so committing the same batch twice must execute it once (replay guard at the
        // per-action (sender, nonce) granularity — the 2e52851 fix, unchanged by batching).
        let (config, state_db) = make_test_config_and_db();
        let exec_ctx = make_exec_ctx(&config, &state_db);

        let mk = |market_id: u64| torus_types::PlaceOrderParams {
            market_id,
            is_buy: true,
            price: FixedPoint::from_raw(100 * FixedPoint::SCALE),
            quantity: FixedPoint::from_raw(FixedPoint::SCALE),
            order_type: torus_types::OrderType::Limit,
            time_in_force: torus_types::TimeInForce::GTC,
            reduce_only: false,
            client_order_id: None,
        };
        let key = k256::ecdsa::SigningKey::from_slice(&[9u8; 32]).unwrap();
        let signed = torus_types::eip712::sign_native_action(
            NativeAction::PlaceOrderBatch(vec![mk(1), mk(2), mk(3)]),
            555_555,
            &key,
        );
        let sender = signed.recover_sender().expect("recover sender");

        // Same batch committed in two consecutive blocks (the proven dup scenario).
        // Link block 2 to the ACTUAL (non-empty) block 1 so the commit-time
        // ancestry check passes and the (sender, nonce) replay guard — not the
        // parent-link fail-stop — is what skips the duplicate.
        let block1 = make_block(1, vec![signed.clone()]);
        exec_ctx.execute_committed_block(&block1, vec![]);
        let mut block2 = make_block(2, vec![signed.clone()]);
        block2.header.parent_hash =
            alloy_primitives::keccak256(block1.header.canonical_header_bytes());
        exec_ctx.execute_committed_block(&block2, vec![]);

        // Recorded exactly once; the stored value is the FIRST commit height (1),
        // proving the second commit was skipped (not re-executed / re-written).
        let nonce_key = torus_state::cf::native_nonce_key(&sender, signed.nonce);
        let recorded = state_db
            .get_cf_raw(torus_state::cf::CF_NATIVE_NONCES, &nonce_key)
            .unwrap()
            .expect("batch must consume its (sender, nonce) once");
        assert_eq!(
            recorded.as_slice(),
            &1u64.to_be_bytes(),
            "replay guard must skip the second commit (nonce height stays 1)",
        );
    }

    /// Exec-ceiling Option A: executing ONE native block must observe each
    /// phase histogram exactly once and count the executed action, so the
    /// live probe's per-phase sums decompose `exec_block_seconds` cleanly
    /// (per-phase count == native-block count by design).
    #[test]
    fn exec_phase_histograms_observe_per_block() {
        let (config, state_db) = make_test_config_and_db();
        let mut exec_ctx = make_exec_ctx(&config, &state_db);
        let metrics = Arc::new(torus_telemetry::Metrics::new());
        exec_ctx.metrics = Some(metrics.clone());

        exec_ctx.execute_committed_block(&make_block(1, vec![sign_claim_rewards(7)]), vec![]);

        let text = metrics.encode();
        for name in [
            "torus_exec_verify_seconds",
            "torus_exec_replay_guard_seconds",
            "torus_exec_engine_seconds",
            "torus_exec_save_books_seconds",
            "torus_exec_flush_seconds",
            "torus_exec_block_seconds",
        ] {
            assert!(
                text.contains(&format!("{name}_count 1")),
                "{name} must observe exactly once per native block:\n{text}",
            );
        }
        assert!(
            text.contains("torus_native_actions_processed_total 1"),
            "executed action must be counted:\n{text}",
        );
        // Direct synchronous call never touches the exec channel; the gauge's
        // inc/dec sites live there and are verified by the live probe (Task 3).
        assert!(
            text.contains("torus_exec_queue_depth 0"),
            "queue gauge must stay 0 on the direct-call path:\n{text}",
        );
    }

    /// Task 3 (RED first): the pull-fallback poll budget must be ≥ 1 s so a body that
    /// arrives a few hundred ms after the request (slow peer / under load) is still
    /// recovered on the sync path. The original 4 × 20 ms = 80 ms gave up far too
    /// early — a body landing later was lost, contributing to the >4 MB wedge. The
    /// widened 20 × 50 ms = 1 s is safe here (sync path only, never consensus voting).
    #[test]
    fn pull_budget_is_at_least_one_second() {
        let budget = PULL_DELAY * PULL_RETRIES as u32;
        assert!(
            budget >= std::time::Duration::from_secs(1),
            "PULL_RETRIES({PULL_RETRIES}) * PULL_DELAY({PULL_DELAY:?}) = {budget:?} must be ≥ 1 s",
        );
    }

    /// Sprint 1 T4: the sync-path pull budget scales with how many bodies are
    /// missing — a flat ~1 s budget recovers a late single body but gives up on
    /// the multi-MB body-sets of batched blocks (s334 bs1000: fetch exhausted,
    /// fell back to sync, which also timed out). Floor stays ~1 s, cap ~8 s.
    #[test]
    fn sync_pull_retries_scales_with_missing_count() {
        assert_eq!(
            sync_pull_retries(0),
            PULL_RETRIES,
            "floor at no/low missing"
        );
        assert_eq!(sync_pull_retries(40), PULL_RETRIES, "40/2 == floor");
        assert_eq!(sync_pull_retries(100), 50, "linear midband: ~2 bodies/tick");
        assert_eq!(
            sync_pull_retries(10_000),
            MAX_SYNC_PULL_RETRIES,
            "cap at ~8 s for huge sets"
        );
    }

    /// Task 3 (behavioral): a body that only arrives AFTER the old 80 ms budget would
    /// have expired (modelled here as delivered on the 8th drain ≈ 400 ms in) is still
    /// recovered within the widened budget. The old 4-retry loop gave up at drain 4 and
    /// lost it; 20 retries reach drain 8 and recover it. This is the consensus-layer
    /// half of the >4 MB un-wedge (the codec/chunk half lives in torus-network).
    #[test]
    fn pull_recovers_body_delivered_after_old_budget() {
        // `LateFetcher` is the shared test double hoisted to module scope.
        let (config, state_db) = make_test_config_and_db();
        let mempool = Arc::new(torus_mempool::Mempool::new(
            state_db.clone(),
            torus_mempool::MempoolConfig::default(),
        ));
        let mut app = TorusApp::new(state_db.clone(), &config, None, Some(mempool.clone()), None);

        let action = sign_claim_rewards(42);
        let hash = torus_types::compute_action_hash(&action);
        let body = bincode::serialize(&action).expect("serialize body");

        // Delivered on the 8th drain — past the old 4-retry budget, within the new 20.
        let fetcher = Arc::new(LateFetcher {
            body,
            deliver_on_drain: 8,
            drains: std::sync::atomic::AtomicUsize::new(0),
        });
        app.set_native_da_fetcher(fetcher);

        assert!(
            mempool.get_native_da(&hash).is_none(),
            "body absent before the pull"
        );
        let recovered = app.pull_missing_bodies(&[hash]);
        assert!(
            recovered,
            "a body arriving after the old 80 ms budget is recovered within ~1 s"
        );
        assert!(
            mempool.get_native_da(&hash).is_some(),
            "the recovered body landed in the durable DA store",
        );
    }

    /// BS-4a (RED first): a true push miss must NOT block the consensus thread on the
    /// in-line pull budget. reconstruct returns Err (MissingData) within the shrunken
    /// local slice, and the RECOVERY WORKER pulls + absorbs the body in the background
    /// so the re-proposed view finds it locally. MUST fail before BS-4a lands (today
    /// the hot path blocks ~260ms and returns Ok via the in-line pull).
    #[test]
    fn hot_path_hands_off_and_recovers_in_background() {
        let (config, state_db) = make_test_config_and_db();
        let mempool = Arc::new(torus_mempool::Mempool::new(
            state_db.clone(),
            torus_mempool::MempoolConfig::default(),
        ));
        let mut app = TorusApp::new(state_db.clone(), &config, None, Some(mempool.clone()), None);

        let action = sign_claim_rewards(13);
        let hash = torus_types::compute_action_hash(&action);
        let body = bincode::serialize(&action).expect("serialize body");
        let fetcher = Arc::new(LateFetcher {
            body,
            deliver_on_drain: 3,
            drains: std::sync::atomic::AtomicUsize::new(0),
        });
        app.set_native_da_fetcher(fetcher);

        let start = std::time::Instant::now();
        let result = app.reconstruct_native_actions_hot(&[hash]);
        let elapsed = start.elapsed();

        assert_eq!(
            result.err(),
            Some(1),
            "a push miss fails THIS view immediately"
        );
        assert!(
            elapsed < std::time::Duration::from_millis(80),
            "consensus thread must not run the pull budget in-line: took {elapsed:?}",
        );
        // The worker recovers the body off-thread well before the next view.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while mempool.get_native_da(&hash).is_none() {
            assert!(
                std::time::Instant::now() < deadline,
                "worker never recovered the body"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    /// BS-4a: the recovery worker, fed a missing hash, pulls + absorbs the body into
    /// the durable DA store off-thread within its budget.
    #[test]
    fn recovery_worker_recovers_late_body_off_thread() {
        let (_config, state_db) = make_test_config_and_db();
        let mempool = Arc::new(torus_mempool::Mempool::new(
            state_db.clone(),
            torus_mempool::MempoolConfig::default(),
        ));

        let action = sign_claim_rewards(23);
        let hash = torus_types::compute_action_hash(&action);
        let body = bincode::serialize(&action).expect("serialize body");
        // Delivered on the 3rd drain — inside the worker budget, as in the driver test.
        let fetcher = Arc::new(LateFetcher {
            body,
            deliver_on_drain: 3,
            drains: std::sync::atomic::AtomicUsize::new(0),
        });

        // No TorusApp: the worker is constructed directly from the Arcs it owns.
        let worker = DaRecoveryWorker::spawn(mempool.clone(), fetcher, None);
        assert!(
            mempool.get_native_da(&hash).is_none(),
            "body absent before the worker pull"
        );
        worker.submit(vec![hash]);

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while mempool.get_native_da(&hash).is_none() {
            assert!(
                std::time::Instant::now() < deadline,
                "worker never recovered the body"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        // Exercise Drop-join on a live worker thread — a hang here IS the defect.
        drop(worker);
    }

    /// BS-4a: dropping the worker with an in-flight batch AND a queued backlog must
    /// join fast. Queued batches are DISCARDED via the shutdown flag (old defect:
    /// drop drained queue_depth x ~1s), and since Fix C the IN-FLIGHT batch inside
    /// recover_bodies_bounded also observes the flag (`cancel`) at the top of its
    /// wait loop, so it abandons its ~1s budget at the NEXT ~20ms slice boundary
    /// after the flag is set instead of running to completion. Uses a
    /// never-delivering fetcher so any processed batch would otherwise burn the full
    /// ~1s budget; the 3s bound is the regression separator — it cleanly separates
    /// fixed (≤~100ms: one slice) from regressed (≥~1s in-flight, up to ~9s without
    /// coalesce) while staying load-tolerant (elapsed bounds flake under
    /// parallel-suite load, mem 96dfce88).
    #[test]
    fn recovery_worker_drop_discards_backlog_and_joins_bounded() {
        let (_config, state_db) = make_test_config_and_db();
        let mempool = Arc::new(torus_mempool::Mempool::new(
            state_db.clone(),
            torus_mempool::MempoolConfig::default(),
        ));

        // NeverFetcher delivers nothing, so the in-flight batch would burn the full
        // ~1s budget if it were not cancellable — the regression this test guards.
        let worker = DaRecoveryWorker::spawn(mempool.clone(), Arc::new(NeverFetcher), None);

        // 9 distinct single-hash batches: never-arriving hashes, so the worker's
        // store-check dedup can't drop them. First is taken in-flight; the rest are
        // coalesced into that same cycle (Fix A drains the queue via try_recv), so a
        // 9th queued would warn+drop — fine either way.
        for nonce in 100u64..109 {
            let hash = torus_types::compute_action_hash(&sign_claim_rewards(nonce));
            worker.submit(vec![hash]);
        }

        // Let the worker pick up a batch in-flight (now inside recover_bodies_bounded,
        // parked in its wait loop where the cancel check lives).
        std::thread::sleep(std::time::Duration::from_millis(50));

        let t = std::time::Instant::now();
        drop(worker);
        let elapsed = t.elapsed();
        // Visible under `--nocapture`: fixed ≈ one slice (≤~100ms), regressed ≥~1s.
        println!("drop-join elapsed: {elapsed:?}");
        assert!(
            elapsed < std::time::Duration::from_secs(3),
            "drop must not drain the backlog: took {elapsed:?}",
        );
    }

    /// Phase 2.3 (#5, RED first): when a hash-only push PRE-WARMED the bodies (they already
    /// sit in the fetcher inbound), the HOT local retry must ABSORB them into the DA store
    /// and reconstruct WITHOUT issuing a redundant network fetch. Asserts the body is
    /// recovered AND `fetch()` was never called. MUST fail before Task 4 (today only the
    /// hot-PULL phase absorbs, and it calls `fetch` first → fetch_count ≥ 1).
    #[test]
    fn prewarmed_bodies_absorbed_without_redundant_hot_fetch() {
        let (config, state_db) = make_test_config_and_db();
        let mempool = Arc::new(torus_mempool::Mempool::new(
            state_db.clone(),
            torus_mempool::MempoolConfig::default(),
        ));
        let mut app = TorusApp::new(state_db.clone(), &config, None, Some(mempool.clone()), None);

        let action = sign_claim_rewards(11);
        let hash = torus_types::compute_action_hash(&action);
        let body = bincode::serialize(&action).expect("serialize body");
        let fetcher = Arc::new(PrewarmFetcher {
            body,
            fetches: std::sync::atomic::AtomicUsize::new(0),
            drained: std::sync::atomic::AtomicBool::new(false),
        });
        app.set_native_da_fetcher(fetcher.clone());

        assert!(
            mempool.get_native_da(&hash).is_none(),
            "body absent before reconstruct"
        );
        let actions = app
            .reconstruct_native_actions_hot(&[hash])
            .expect("the pre-warmed body must be absorbed and reconstructed");
        assert_eq!(
            actions.len(),
            1,
            "the absorbed action is returned (in hash order)"
        );
        assert!(
            mempool.get_native_da(&hash).is_some(),
            "the pre-warmed body landed in the durable DA store",
        );
        assert_eq!(
            fetcher.fetches.load(std::sync::atomic::Ordering::Relaxed),
            0,
            "a pre-warmed body must be absorbed by the local retry -- no redundant hot fetch",
        );
    }

    /// BS-4a: when the body NEVER arrives, the hot path must FAIL THE VIEW (Err with
    /// the missing count) FAST — the contract is now fail-fast (one ~20 ms local slice
    /// + a non-blocking worker handoff), not merely under the 500 ms view timeout. The
    /// block is simply re-proposed next view. The 80 ms bound is 4x the 20 ms slice:
    /// under parallel-suite load elapsed bounds can flake (mem 96dfce88), so keep
    /// headroom above the nominal slice.
    #[test]
    fn hot_path_fails_view_fast_when_body_never_arrives() {
        let (config, state_db) = make_test_config_and_db();
        let mempool = Arc::new(torus_mempool::Mempool::new(
            state_db.clone(),
            torus_mempool::MempoolConfig::default(),
        ));
        let mut app = TorusApp::new(state_db.clone(), &config, None, Some(mempool.clone()), None);

        let action = sign_claim_rewards(9);
        let hash = torus_types::compute_action_hash(&action);
        app.set_native_da_fetcher(Arc::new(NeverFetcher));

        let start = std::time::Instant::now();
        let result = app.reconstruct_native_actions_hot(&[hash]);
        let elapsed = start.elapsed();

        assert_eq!(
            result.err(),
            Some(1),
            "a never-arriving body fails the view, reporting the missing count",
        );
        assert!(
            elapsed < std::time::Duration::from_millis(80),
            "hot path must fail fast (one local slice + non-blocking handoff): took {elapsed:?}",
        );
    }

    /// BS-4a: both recovery budgets stay within bounds. (a) The consensus-thread share
    /// of hot reconstruction is ONE local wake-on-arrival slice — well under the 500 ms
    /// view timeout (hotstuff: 4*EWNL + produce + validate < max_view_time), so
    /// validate_block fails a missed view instead of burning it. (b) The worker's
    /// off-thread pull budget stays under ~2 s, so an unrecoverable batch never
    /// outlives more than a couple of re-propose cycles (which resubmit the same
    /// hashes anyway). Companion of `pull_budget_is_at_least_one_second` (sync LOWER
    /// bound). Renamed from `hot_pull_budget_under_view_timeout` — there is no in-line
    /// hot pull anymore.
    #[test]
    fn recovery_budgets_within_bounds() {
        let local = RECONSTRUCT_RETRY_DELAY * RECONSTRUCT_RETRIES as u32;
        assert!(
            local < std::time::Duration::from_millis(50),
            "consensus-thread budget {local:?} (one slice) must be well under the 500 ms view timeout",
        );
        let worker = WORKER_PULL_DELAY * WORKER_PULL_RETRIES as u32;
        assert!(
            worker < std::time::Duration::from_secs(2),
            "worker pull budget {worker:?} must not outlive a couple of re-propose cycles",
        );
    }

    /// BS-4b (RED first): a body that never arrives must trigger EXACTLY ONE
    /// mid-budget re-fetch — covering a lost pull request/response — on top of the
    /// initial fetch, and NOT one re-fetch per wait tick. Drives `recover_bodies_bounded`
    /// directly with a small budget (10 × 10 ms = 100 ms, midpoint ~50 ms). Before the
    /// re-fetch lands this fails on `fetch_calls == 2` (today only the initial fetch fires).
    #[test]
    fn recover_bodies_refetches_once_mid_budget() {
        let (_config, state_db) = make_test_config_and_db();
        let mempool = Arc::new(torus_mempool::Mempool::new(
            state_db.clone(),
            torus_mempool::MempoolConfig::default(),
        ));

        let action = sign_claim_rewards(77);
        let hash = torus_types::compute_action_hash(&action);
        let body = bincode::serialize(&action).expect("serialize body");
        // Never delivered: the body stays missing for the whole budget.
        let fetcher = Arc::new(CountingFetcher {
            body,
            deliver_on_drain: 0,
            fetch_calls: std::sync::atomic::AtomicUsize::new(0),
            drains: std::sync::atomic::AtomicUsize::new(0),
        });

        let recovered = recover_bodies_bounded(
            &mempool,
            fetcher.as_ref(),
            &[hash],
            10,
            std::time::Duration::from_millis(10),
            None,
            None,
        );
        assert!(
            !recovered,
            "a never-arriving body is not recovered in-budget"
        );
        assert_eq!(
            fetcher
                .fetch_calls
                .load(std::sync::atomic::Ordering::Relaxed),
            2,
            "initial fetch + exactly one mid-budget re-fetch (not one per tick)",
        );
    }

    /// BS-4b: a body that arrives well before the budget midpoint must NOT trigger a
    /// re-fetch — the initial fetch is the only one. `deliver_on_drain: 1` lands the
    /// body on the first wait-loop drain (~10 ms into the ~100 ms budget, midpoint ~50 ms).
    #[test]
    fn recover_bodies_no_refetch_when_body_arrives_early() {
        let (_config, state_db) = make_test_config_and_db();
        let mempool = Arc::new(torus_mempool::Mempool::new(
            state_db.clone(),
            torus_mempool::MempoolConfig::default(),
        ));

        let action = sign_claim_rewards(88);
        let hash = torus_types::compute_action_hash(&action);
        let body = bincode::serialize(&action).expect("serialize body");
        // Delivered on the first drain after the initial fetch — well before the midpoint.
        let fetcher = Arc::new(CountingFetcher {
            body,
            deliver_on_drain: 1,
            fetch_calls: std::sync::atomic::AtomicUsize::new(0),
            drains: std::sync::atomic::AtomicUsize::new(0),
        });

        let recovered = recover_bodies_bounded(
            &mempool,
            fetcher.as_ref(),
            &[hash],
            10,
            std::time::Duration::from_millis(10),
            None,
            None,
        );
        assert!(recovered, "an early-arriving body is recovered");
        assert_eq!(
            fetcher
                .fetch_calls
                .load(std::sync::atomic::Ordering::Relaxed),
            1,
            "body arrived before the midpoint — only the initial fetch, no re-fetch",
        );
    }

    // ========================================================================
    // T1.2 body-determinism regression suite
    //
    // Instrumented devnet (t12-diag1) proved HotStuff consensus is SAFE — all
    // nodes vote + commit an identical block — yet a minority node executed a
    // DIFFERENT native-action body (native_count=40 of the committed 59) and
    // persisted a divergent eth header. Root cause: `on_committed_block`
    // preferred a stale `pending_proposals[height]` cache (a same-height
    // re-proposal overwrote the entry) over the committed consensus datum, for
    // BOTH the header and the body. These tests pin the two halves of the fix.
    // ========================================================================

    /// Build a committed HotStuff `Block` carrying a COMPACT datum for `block`.
    fn committed_compact_block(block: &TorusBlock) -> hotstuff_rs::types::block::Block {
        use hotstuff_rs::hotstuff::types::PhaseCertificate;
        use hotstuff_rs::types::data_types::BlockHeight;
        let datum = encode_proposal_datum(block, true);
        let hash = TorusApp::hash_datum(&datum);
        hotstuff_rs::types::block::Block::new(
            BlockHeight::new(block.header.height),
            PhaseCertificate::genesis_pc(),
            CryptoHash::new(hash),
            Data::new(vec![Datum::new(datum)]),
        )
    }

    /// T1.2 (a) RED-first: a STALE same-height `pending_proposals` entry (fewer
    /// actions than the committed block) must NEVER be executed in place of the
    /// committed datum. The body handed to execution must carry the COMMITTED
    /// action set (reconstructed from the durable DA store), never the cache's
    /// partial subset.
    ///
    /// RED on pre-fix code: the compact path did
    /// `if let Some(cached) = pending_proposals.remove(&height) { (cached.header, Ok(cached)) }`,
    /// so it executed the cached 40-action block and its 40-count header.
    /// GREEN after: header is taken verbatim from the committed compact and the
    /// bodies are reconstructed from DA (all-or-nothing), so 59 actions execute.
    #[test]
    fn committed_block_ignores_stale_smaller_pending_proposal() {
        let (config, state_db) = make_test_config_and_db();
        let mempool = Arc::new(torus_mempool::Mempool::new(
            state_db.clone(),
            torus_mempool::MempoolConfig::default(),
        ));

        // The COMMITTED block: 59 native actions (nonces 1..=59).
        let committed_actions: Vec<SignedNativeAction> =
            (1..=59u64).map(sign_claim_rewards).collect();
        let committed_block = make_block(130, committed_actions.clone());
        // Every committed body lives in the durable DA store (proposer mirror).
        mempool.mirror_native_to_da(&committed_actions);

        let mut app = TorusApp::new(state_db.clone(), &config, None, Some(mempool), None);

        // Capture what on_committed_block hands to the execution pipeline.
        let (tx, rx) = std::sync::mpsc::sync_channel::<CommittedBlockMsg>(4);
        app.exec_tx = Some(tx);

        // A STALE same-height re-proposal cached only 40 of those actions — the
        // devnet "proposer executed 40 of its own 59" state.
        let stale_actions: Vec<SignedNativeAction> = (1..=40u64).map(sign_claim_rewards).collect();
        let stale_block = make_block(130, stale_actions);
        app.pending_proposals.insert(130, stale_block);

        let committed = committed_compact_block(&committed_block);
        app.on_committed_block(&committed, committed.hash);

        let msg = rx
            .try_recv()
            .expect("committed block must be sent to execution");
        assert_eq!(
            msg.torus_block.native_actions.len(),
            59,
            "execution must run the COMMITTED body (59), never the stale cache's 40"
        );
        assert_eq!(
            msg.torus_block.header.native_action_count, 59,
            "the persisted header count must be the committed 59, not the cache's 40"
        );
        assert_eq!(
            msg.torus_block.header.canonical_header_bytes(),
            committed_block.header.canonical_header_bytes(),
            "header handed to execution must be byte-identical to the committed header"
        );
    }

    /// T1.2 (b) RED-first: feeding the execute path a body with FEWER native
    /// actions than the committed header's `native_action_count` must FAIL-STOP
    /// (latch `exec_failed`) and persist NOTHING — never silently execute a
    /// partial body and persist a header rebuilt from the wrong count.
    ///
    /// RED on pre-fix code: `execute_committed_block` had no body/header count
    /// guard, so it executed the 2 actions and `persist_block_header` wrote the
    /// (divergent) header verbatim — with `exec_failed` never latched.
    /// GREEN after: the guard latches fail-stop and returns before any persist.
    #[test]
    fn execute_partial_body_vs_header_count_fail_stops() {
        let (config, state_db) = make_test_config_and_db();
        let exec_ctx = make_exec_ctx(&config, &state_db);

        // Header claims 5 native actions; the body only carries 2 — the exact
        // partial-body shape the completeness fix forbids.
        let mut block = make_block(7, vec![sign_claim_rewards(1), sign_claim_rewards(2)]);
        block.header.native_action_count = 5;

        exec_ctx.execute_committed_block(&block, vec![]);

        assert!(
            exec_ctx.exec_failed.load(Ordering::SeqCst),
            "a body/header native_action_count mismatch must latch the fail-stop"
        );
        assert!(
            state_db
                .get_cf_raw(CF_BLOCK_HEADERS, &7u64.to_be_bytes())
                .unwrap()
                .is_none(),
            "a divergent header must NOT be persisted"
        );
        assert!(
            read_native_applied_height(&state_db).is_none(),
            "a fail-stopped block must not be marked applied"
        );
    }

    /// T1.2 (c) guard: for a normal, well-formed block the persisted eth header
    /// must be byte-identical to the committed header — the header-verbatim
    /// invariant. Passes before and after the fix; a regression that rebuilds
    /// the header from execution results would break it.
    #[test]
    fn persisted_header_equals_committed_header_byte_for_byte() {
        let (config, state_db) = make_test_config_and_db();
        let exec_ctx = make_exec_ctx(&config, &state_db);

        let block = make_block(3, vec![sign_claim_rewards(11), sign_claim_rewards(12)]);
        exec_ctx.execute_committed_block(&block, vec![]);

        let stored = state_db
            .get_cf_raw(CF_BLOCK_HEADERS, &3u64.to_be_bytes())
            .unwrap()
            .expect("normal block must persist a header");
        // Layout (persist_block_header): [0..32] = committed block hash,
        // [32..] = serde(header). Both derive from the committed header verbatim.
        let want_hash = alloy_primitives::keccak256(block.header.canonical_header_bytes());
        assert_eq!(
            &stored[..32],
            want_hash.as_slice(),
            "persisted block hash must equal keccak(committed canonical header)"
        );
        let persisted_header: TorusBlockHeader =
            serde_json::from_slice(&stored[32..]).expect("persisted header decodes");
        assert_eq!(
            persisted_header.canonical_header_bytes(),
            block.header.canonical_header_bytes(),
            "persisted header must be byte-identical to the committed header"
        );
    }

    // ========================================================================
    // Regime-B STRICT-ORDER EXECUTION (never execute H+1 before H).
    //
    // EVIDENCE: a full devnet run ended with permanently-missing executed
    // heights (v2: 987-989, v3: 911-913) while LATER heights executed —
    // consensus committed past a block whose native bodies were not
    // reconstructable at that instant, the deferred-body path dropped it, and
    // later blocks executed anyway. Native matching is order-dependent, so an
    // interior hole is PERMANENT divergence. These tests pin the invariant:
    // on_committed_block hands blocks to `exec_tx` strictly in ascending height
    // with NO gaps; a missing-body height holds every later height until it heals.
    // ========================================================================

    /// Wire a TorusApp with a live mempool DA store and a RECORDING exec channel,
    /// so a test can observe exactly what (and in what order) on_committed_block
    /// hands to the execution pipeline. Returns the app, the receiver, and the
    /// mempool (to mirror bodies into the DA store, i.e. make them reconstructable).
    fn app_with_recording_exec(
        config: &ChainConfig,
        state_db: &StateDb,
    ) -> (
        TorusApp,
        std::sync::mpsc::Receiver<CommittedBlockMsg>,
        Arc<Mempool>,
    ) {
        let mempool = Arc::new(torus_mempool::Mempool::new(
            state_db.clone(),
            torus_mempool::MempoolConfig::default(),
        ));
        let mut app = TorusApp::new(state_db.clone(), config, None, Some(mempool.clone()), None);
        let (tx, rx) = std::sync::mpsc::sync_channel::<CommittedBlockMsg>(64);
        app.exec_tx = Some(tx);
        (app, rx, mempool)
    }

    /// RED-first (out-of-order delivery): the execution pipeline must NEVER
    /// receive H+1 before H, even when H+1's bodies are fully available and H has
    /// not yet arrived. RED on pre-fix code: `on_committed_block` sent every
    /// reconstructable block immediately, so a committed H+1 executed while H was
    /// still deferred — the permanent interior-hole divergence. GREEN: H+1 is
    /// buffered until H arrives, then both drain in strict order.
    #[test]
    fn exec_pipeline_never_runs_higher_height_before_lower() {
        let (config, state_db) = make_test_config_and_db();
        let (mut app, rx, mempool) = app_with_recording_exec(&config, &state_db);

        // Both bodies are present in the DA store (nothing missing here — this is
        // pure ordering, not a hole).
        let a5 = vec![sign_claim_rewards(5)];
        let a6 = vec![sign_claim_rewards(6)];
        mempool.mirror_native_to_da(&a5);
        mempool.mirror_native_to_da(&a6);
        let b5 = make_block(5, a5);
        let b6 = make_block(6, a6);

        // The pipeline expects height 5 next.
        app.exec_next_height = Some(5);

        // H+1 (6) is committed with its body READY, but H (5) has not arrived.
        let c6 = committed_compact_block(&b6);
        app.on_committed_block(&c6, c6.hash);
        assert!(
            rx.try_recv().is_err(),
            "H+1 must NOT reach execution before H — strict height order (RED on pre-fix code)"
        );

        // H (5) arrives → 5 then 6 drain in ascending order.
        let c5 = committed_compact_block(&b5);
        app.on_committed_block(&c5, c5.hash);
        assert_eq!(
            rx.try_recv().unwrap().torus_block.header.height,
            5,
            "H executes first"
        );
        assert_eq!(
            rx.try_recv().unwrap().torus_block.header.height,
            6,
            "then H+1"
        );
        assert!(rx.try_recv().is_err(), "nothing else queued");
        assert!(!app.is_exec_failed(), "in-order delivery must not fail-stop");
    }

    /// RED-first (deferred hole heals): a committed block whose native bodies are
    /// initially missing is DEFERRED (not executed); a later committed block waits
    /// behind it; once the missing bodies land in the DA store, the next commit
    /// drives BOTH (and the follower) through execution in strict order.
    #[test]
    fn deferred_hole_executes_in_order_once_bodies_arrive() {
        let (config, state_db) = make_test_config_and_db();
        let (mut app, rx, mempool) = app_with_recording_exec(&config, &state_db);

        let a5 = vec![sign_claim_rewards(5)];
        let a6 = vec![sign_claim_rewards(6)];
        let a7 = vec![sign_claim_rewards(7)];
        // Initially only 6 and 7 are reconstructable; 5's body is MISSING.
        mempool.mirror_native_to_da(&a6);
        mempool.mirror_native_to_da(&a7);
        let b5 = make_block(5, a5.clone());
        let b6 = make_block(6, a6);
        let b7 = make_block(7, a7);

        app.exec_next_height = Some(5);

        // H=5 committed but body missing → deferred, nothing executes.
        let c5 = committed_compact_block(&b5);
        app.on_committed_block(&c5, c5.hash);
        assert!(
            rx.try_recv().is_err(),
            "a missing-body committed block must defer, not execute"
        );
        assert!(
            app.exec_hole_since.is_some(),
            "the hole clock must start when the head-of-line body is missing"
        );

        // H+1=6 committed WITH its body → must wait behind the hole at 5.
        let c6 = committed_compact_block(&b6);
        app.on_committed_block(&c6, c6.hash);
        assert!(
            rx.try_recv().is_err(),
            "H+1 must wait behind the deferred hole at H (no out-of-order execution)"
        );

        // Heal: 5's body arrives in the DA store; the next commit (7) drives the drain.
        mempool.mirror_native_to_da(&a5);
        let c7 = committed_compact_block(&b7);
        app.on_committed_block(&c7, c7.hash);

        assert_eq!(rx.try_recv().unwrap().torus_block.header.height, 5, "5 first");
        assert_eq!(rx.try_recv().unwrap().torus_block.header.height, 6, "then 6");
        assert_eq!(rx.try_recv().unwrap().torus_block.header.height, 7, "then 7");
        assert!(rx.try_recv().is_err(), "exactly three blocks, in order");
        assert!(
            !app.is_exec_failed(),
            "a healed hole must NOT latch fail-stop"
        );
        assert!(
            app.exec_hole_since.is_none(),
            "healing the hole must clear the hole clock"
        );
    }

    /// RED-first (heal-budget exhaustion): an execution hole that cannot heal
    /// within the budget must latch `exec_failed` — a node that cannot obtain a
    /// committed block's bodies is broken, and a silent divergence is worse than a
    /// halt. First detection stays WITHIN budget (retry, no latch); once the
    /// budget elapses, the next re-attempt latches the fail-stop.
    #[test]
    fn exec_hole_budget_exhaustion_latches_fail_stop() {
        let (config, state_db) = make_test_config_and_db();
        let (mut app, _rx, _mempool) = app_with_recording_exec(&config, &state_db);

        // 5's body is PERMANENTLY absent from the DA store (never mirrored).
        let b5 = make_block(5, vec![sign_claim_rewards(5)]);
        let b6 = make_block(6, vec![]); // empty follower, always reconstructable

        app.exec_next_height = Some(5);

        // First hit: hole detected, still within budget → retrying, NOT latched.
        let c5 = committed_compact_block(&b5);
        app.on_committed_block(&c5, c5.hash);
        assert!(
            !app.is_exec_failed(),
            "a fresh hole within budget must keep retrying, not fail-stop"
        );
        assert!(
            app.exec_hole_since.is_some(),
            "the hole clock must start on first detection"
        );

        // Simulate the heal budget elapsing (default 5 min).
        app.exec_hole_since =
            Some(std::time::Instant::now() - std::time::Duration::from_secs(6 * 60));

        // The next commit re-attempts the still-broken hole → budget exhausted →
        // fail-stop latched (halt, never diverge silently).
        let c6 = committed_compact_block(&b6);
        app.on_committed_block(&c6, c6.hash);
        assert!(
            app.is_exec_failed(),
            "a hole that cannot heal within the budget must latch exec_failed"
        );
    }
}
