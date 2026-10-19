//! Durable native-action data-availability (DA) body store.
//!
//! Stores native-action bodies keyed by their 32-byte action-hash in
//! [`CF_NATIVE_PENDING`], **decoupled** from the 60s nonce-staleness gate that
//! governs mempool admission. A body referenced by a proposed or committed
//! `CompactBlock` must always be reconstructable — even after the nonce window
//! expires or the process restarts — so consensus never wedges on a missing
//! body (livelock root cause, mem 28e1a821).
//!
//! Push-primary / fetch-rare (s297 Option D): bodies are mirrored here on every
//! ingest / produce / receive path, and the rare pull-fallback
//! (`/torus/native-da/1.0`) serves them by-hash on a miss. Bodies use the same
//! `bincode` encoding as the native-action network wire format.
//!
//! RH4: committed bodies (and their shards) are deleted a retention window
//! after the LATEST dispatched block that references them
//! ([`NativeDaStore::gc_committed`]; the window policy is
//! `torus_consensus::da_gc`). Bodies of uncommitted actions stay.

use std::collections::HashSet;
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use alloy_primitives::B256;
use torus_types::{compute_action_hash, compute_action_hash_with_scratch, SignedNativeAction};

use crate::cf::{CF_NATIVE_PENDING, CF_NATIVE_SHARDS};
use crate::db::StateDb;
use crate::erasure::{encode, ErasureParams};
use crate::error::StateError;
use crate::shard_store::{decode_stored_shard, encode_stored_shard, shard_key, StoredShard};

/// Process-wide body-arrival notifier (S391). `NativeDaStore` instances are
/// constructed independently over the one shared DB, so the notifier is a
/// process global: any `put` (proposer push ingest, gossip mirror, pull
/// absorb) wakes every waiter. The hot reconstruct path waits on this instead
/// of sleeping fixed 20ms ticks, so the common push-race resolves at actual
/// arrival time.
struct ArrivalNotifier {
    generation: Mutex<u64>,
    arrived: Condvar,
}

fn arrivals() -> &'static ArrivalNotifier {
    static ARRIVALS: OnceLock<ArrivalNotifier> = OnceLock::new();
    ARRIVALS.get_or_init(|| ArrivalNotifier {
        generation: Mutex::new(0),
        arrived: Condvar::new(),
    })
}

/// Durable, nonce-gate-decoupled store of native-action bodies keyed by action-hash.
///
/// CF-backed (survives restart). Cheap to clone — it shares the underlying
/// [`StateDb`] handle (an `Arc<DB>`), so every consensus / mempool / network
/// site can hold its own `NativeDaStore` over the one database. In the node
/// that is the consensus DB ([`StateDb::open_consensus`], section 71.8
/// option A), not the state DB.
/// Wall time of one [`NativeDaStore::get_batch_timed`] read, split into the
/// RocksDB `MultiGet` and the body decode.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DaReadTiming {
    pub multi_get: std::time::Duration,
    pub decode: std::time::Duration,
}

#[derive(Clone)]
pub struct NativeDaStore {
    db: StateDb,
}

/// RH4 body GC: the retention row of one committed height lives in
/// [`CF_NATIVE_PENDING`] under `DA_GC_ROW_PREFIX ++ height (8 BE)`, next to
/// the bodies it lists, so ONE `WriteBatch` deletes the bodies, their shards
/// and the row. Body keys are always 32 bytes (an action hash); these keys
/// are 15 bytes, so they can never be read or served as a body.
const DA_GC_ROW_PREFIX: &[u8; 7] = b"\xffda-gc/";
const DA_GC_ROW_KEY_LEN: usize = DA_GC_ROW_PREFIX.len() + 8;
/// GC state, `frontier ++ cursor ++ hold` (8 BE each; [`DaGcState`]).
/// Outside the row prefix (`-` sorts before `/`), so no row scan meets it.
const DA_GC_STATE_KEY: &[u8] = b"\xffda-gc-state";
/// Latest reference of a committed hash: `DA_GC_REF_PREFIX ++ hash` ->
/// `height (8 BE) ++ shard_count (u16 BE)` of the highest recorded row that
/// lists it. Codex P1: a hash that two committed heights reference (a
/// re-inclusion) is moved out of the earlier, still pending row when the
/// later one is recorded (a [`DA_GC_MOVED_PREFIX`] marker), so only its
/// latest row deletes it, when that reference expires. 43-byte keys, outside
/// the row prefix like the state key.
const DA_GC_REF_PREFIX: &[u8; 11] = b"\xffda-gc-ref/";
const DA_GC_REF_KEY_LEN: usize = DA_GC_REF_PREFIX.len() + 32;
/// A hash moved out of an earlier row: `DA_GC_MOVED_PREFIX ++ height (8 BE)
/// ++ hash` -> empty, `height` being the earlier row's. The earlier row is
/// never rewritten (Codex P2: a block re-including one hash from each of
/// many pending rows would rewrite all of them on the consensus thread): its
/// GC skips the hashes its markers name and deletes the markers with it.
/// 51-byte keys, outside the row prefix (`-` sorts before `/`).
const DA_GC_MOVED_PREFIX: &[u8; 11] = b"\xffda-gc-mov/";
const DA_GC_MOVED_KEY_LEN: usize = DA_GC_MOVED_PREFIX.len() + 8 + 32;

fn da_gc_ref_key(hash: &[u8]) -> [u8; DA_GC_REF_KEY_LEN] {
    let mut key = [0u8; DA_GC_REF_KEY_LEN];
    key[..DA_GC_REF_PREFIX.len()].copy_from_slice(DA_GC_REF_PREFIX);
    key[DA_GC_REF_PREFIX.len()..].copy_from_slice(hash);
    key
}

fn encode_da_gc_ref(height: u64, shard_count: u16) -> [u8; 10] {
    let mut value = [0u8; 10];
    value[..8].copy_from_slice(&height.to_be_bytes());
    value[8..].copy_from_slice(&shard_count.to_be_bytes());
    value
}

/// `(height, shard_count)`, `None` when malformed.
fn decode_da_gc_ref(value: &[u8]) -> Option<(u64, u16)> {
    let value: &[u8; 10] = value.try_into().ok()?;
    Some((
        u64::from_be_bytes(value[..8].try_into().expect("8 bytes")),
        u16::from_be_bytes([value[8], value[9]]),
    ))
}

/// The marker of `hash` moved out of the row of `height`; with an empty
/// `hash`, the lower bound of the markers of the rows at or above `height`.
fn da_gc_moved_key(height: u64, hash: &[u8]) -> Vec<u8> {
    let mut key = Vec::with_capacity(DA_GC_MOVED_KEY_LEN);
    key.extend_from_slice(DA_GC_MOVED_PREFIX);
    key.extend_from_slice(&height.to_be_bytes());
    key.extend_from_slice(hash);
    key
}

/// The durable GC state ([`DA_GC_STATE_KEY`]), written in the batch of the
/// pass that changes it: `frontier ++ cursor ++ hold` (24 bytes, 8 BE each),
/// or the pre-cursor `frontier ++ next_record` (16 bytes, no cursor yet).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct DaGcState {
    /// Lowest height whose row may still exist (rows below it were deleted).
    /// Only a seek start: it keeps each pass from walking old row tombstones.
    frontier: u64,
    /// Pre-cursor state only (16-byte encoding): one past the highest height
    /// that recorded a row. A dispatch below it is a replay.
    legacy_next_record: u64,
    /// "Recorded through": one past the highest height whose record (a row,
    /// or nothing for a native-empty height) is durable; every height from
    /// the first recorded one up to it is recorded. Written in the batch of
    /// the records it covers. A dispatch below it is a replay (committed
    /// blocks are final) and records nothing, so a replay never re-attaches a
    /// hash to an earlier row than its latest reference. A dispatch above it
    /// finds the bookkeeping behind (a crash between the body write and the
    /// record, a failed record write): the missing heights are back-filled
    /// from the [`DaGcHistory`] before anything is deleted. `None` = not
    /// established (a fresh store, or a pre-cursor one): the first recording
    /// pass establishes it at its own height.
    cursor: Option<u64>,
    /// One past the highest height a back-fill skipped because the history
    /// pruned it (`0` = none). Its re-inclusions are unknown, so nothing is
    /// deleted until it is itself due (window and tree bound).
    hold: u64,
    /// Decoded from a pre-cursor (16-byte) state: rows exist that no cursor
    /// vouches for. Not encoded.
    pre_cursor: bool,
}

impl DaGcState {
    /// Absent or malformed = the initial state.
    fn decode(bytes: Option<&[u8]>) -> Self {
        let word = |b: &[u8], i: usize| {
            u64::from_be_bytes(b[8 * i..8 * i + 8].try_into().expect("8 bytes"))
        };
        match bytes {
            Some(b) if b.len() == 24 => Self {
                frontier: word(b, 0),
                legacy_next_record: 0,
                cursor: Some(word(b, 1)),
                hold: word(b, 2),
                pre_cursor: false,
            },
            Some(b) if b.len() == 16 => Self {
                frontier: word(b, 0),
                legacy_next_record: word(b, 1),
                cursor: None,
                hold: 0,
                pre_cursor: true,
            },
            _ => Self::default(),
        }
    }

    fn encode(self) -> Vec<u8> {
        let words = match self.cursor {
            Some(cursor) => vec![self.frontier, cursor, self.hold],
            None => vec![self.frontier, self.legacy_next_record],
        };
        words.iter().flat_map(|w| w.to_be_bytes()).collect()
    }
}

/// What [`NativeDaStore::gc_due_rows`] did.
enum DueRows {
    /// The deletes are in the batch; `Some` = the new frontier (a row went).
    Deleted(Option<u64>),
    /// The uncommitted tail is unreadable: nothing added, the pass holds.
    TailUnreadable,
}

/// Most heights one pass back-fills from the [`DaGcHistory`] (heights the
/// history pruned are skipped in O(1) and do not count). A longer gap takes
/// several passes, deleting nothing until it is closed.
pub const DA_GC_MAX_BACKFILL_HEIGHTS: u64 = 256;

/// Latest references recorded earlier in the same batch (a back-fill writes
/// several heights in one batch, and a `MultiGet` does not see the batch).
type PendingRefs = std::collections::HashMap<[u8; 32], (u64, u16)>;

fn da_gc_row_key(height: u64) -> [u8; DA_GC_ROW_KEY_LEN] {
    let mut key = [0u8; DA_GC_ROW_KEY_LEN];
    key[..DA_GC_ROW_PREFIX.len()].copy_from_slice(DA_GC_ROW_PREFIX);
    key[DA_GC_ROW_PREFIX.len()..].copy_from_slice(&height.to_be_bytes());
    key
}

/// Row value: `shard_count (u16 BE) ++ hash_0 ++ .. ++ hash_n` (32 bytes each).
fn encode_da_gc_row(shard_count: u16, hashes: &[B256]) -> Vec<u8> {
    let mut row = Vec::with_capacity(2 + 32 * hashes.len());
    row.extend_from_slice(&shard_count.to_be_bytes());
    for hash in hashes {
        row.extend_from_slice(hash.as_slice());
    }
    row
}

/// `(shard_count, concatenated hashes)`, `None` when malformed.
fn decode_da_gc_row(row: &[u8]) -> Option<(u16, &[u8])> {
    if row.len() < 2 || !(row.len() - 2).is_multiple_of(32) {
        return None;
    }
    Some((u16::from_be_bytes([row[0], row[1]]), &row[2..]))
}

/// Run `f`, adding its wall time to `total`.
fn timed<T>(total: &mut Duration, f: impl FnOnce() -> T) -> T {
    let t = Instant::now();
    let r = f();
    *total += t.elapsed();
    r
}

/// Walk `markers` (ascending, bounded to the marker keys) through the
/// markers at or below `through`, deleting each in `batch`; `marked` ends up
/// holding the hashes of those AT `through`. One below it has no row left
/// (none in a consistent store) and just goes.
fn take_da_gc_markers(
    markers: &mut rocksdb::DBRawIterator<'_>,
    batch: &mut rocksdb::WriteBatch,
    cf: &rocksdb::ColumnFamily,
    through: u64,
    marked: &mut HashSet<[u8; 32]>,
) -> Result<(), StateError> {
    marked.clear();
    let at = DA_GC_MOVED_PREFIX.len();
    while let Some(key) = markers.key() {
        // A 32-byte body key can share the prefix: never treat it as a marker.
        if key.len() == DA_GC_MOVED_KEY_LEN {
            let h = u64::from_be_bytes(key[at..at + 8].try_into().expect("8 bytes"));
            if h > through {
                break;
            }
            if h == through {
                marked.insert(key[at + 8..].try_into().expect("32 bytes"));
            }
            batch.delete_cf(cf, key);
        }
        markers.next();
    }
    markers.status()?;
    Ok(())
}

/// Per-pass bounds of [`NativeDaStore::gc_committed`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DaGcLimits {
    /// Most retention rows (committed heights) one pass deletes.
    pub max_rows: usize,
    /// Most delete operations (bodies + shard keys) one pass issues. The first
    /// row is always taken whole, so a pass always progresses.
    pub max_deletes: usize,
}

/// What one [`NativeDaStore::gc_committed`] pass did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DaGcOutcome {
    /// Body delete operations issued (one per hash of each deleted row).
    pub bodies_deleted: u64,
    /// Shard-key delete operations issued.
    pub shard_keys_deleted: u64,
    /// Retention rows (committed heights) deleted.
    pub rows_deleted: u64,
    /// A bound stopped the pass while aged rows remained; the next pass
    /// continues from there.
    pub backlog: bool,
    /// Hashes the recorded block re-includes that an earlier, still pending
    /// row listed: moved to this height, their bodies now go with it.
    pub references_moved: u64,
    /// Heights below `height` whose records this pass wrote from the
    /// [`DaGcHistory`] (the GC bookkeeping was behind: a crash between the
    /// body write and the GC record, or a failed record write).
    pub heights_backfilled: u64,
    /// The pass deleted nothing because the bookkeeping is still behind (a
    /// height the history could not supply) or a skipped height is still
    /// inside the window or the tree margin. The bodies stay; the next pass
    /// retries.
    pub deletion_held: bool,
    /// The history could not supply a missing height (a missing block or a
    /// read error): the pass deleted nothing (`deletion_held`) and the next
    /// one retries.
    pub backfill_stalled: bool,
    /// The history holds committed heights above `height` (dispatch is
    /// behind the committed tip: boot reconciliation, a multi-block commit):
    /// the pass deleted nothing (`deletion_held`), since one of them may
    /// re-include a due row's body before its own pass records the pin.
    pub undispatched_commits: bool,
    /// Why the pass deleted nothing (`deletion_held`), `None` when it did
    /// not hold. One reason per held pass, the first rule that held it.
    pub hold_reason: Option<DaGcHoldReason>,
    /// Bodies of due rows kept because a block above the history's
    /// committed tip (the uncommitted tail) references them: the row went,
    /// the body (its reference and shards) stayed.
    pub tail_pinned: u64,
    /// Bodies of due rows kept because the history's fence dropped them
    /// right before the write ([`DaGcHistory::begin_delete`]: a concurrent
    /// check pinned them for a block it is validating). The row went, the
    /// body (its reference and shards) stayed, as for [`tail_pinned`](Self::tail_pinned).
    pub fence_pinned: u64,
}

/// Where one GC pass spent its time and what it walked
/// ([`NativeDaStore::gc_committed_with_history_timed`]; RH4 slow-pass
/// observability, 8h). Plain `Instant`s around each phase and two counters,
/// nothing else: the pass decides and writes exactly what the untimed pass
/// does. Kept out of [`DaGcOutcome`], whose equality the tests rely on.
/// A failed phase still adds its time before the error propagates.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DaGcPassStats {
    /// Reading the GC state (one point read).
    pub state_read: Duration,
    /// Back-filling unrecorded heights from the history (its block reads,
    /// body restores and the records), normally zero.
    pub backfill: Duration,
    /// Recording this height: the latest-reference `MultiGet` over its
    /// hashes and the row, reference and marker puts into the batch.
    pub record: Duration,
    /// Reading the history's committed tip (only when a row is due).
    pub tip_read: Duration,
    /// Reading the history's uncommitted tail (once, at the first due row).
    pub tail_read: Duration,
    /// Walking the due rows and their markers and adding the deletes to the
    /// batch, the tail read excluded.
    pub delete_scan: Duration,
    /// The batch write (records, deletes, GC state; a back-filling pass
    /// writes two batches, both counted).
    pub write: Duration,
    /// Bodies the deleted rows list (deleted, moved, marked or tail-pinned).
    pub bodies_scanned: u64,
    /// Hashes the uncommitted tail references (when it was read).
    pub tail_hashes: u64,
    /// The history's committed tip, when this pass read it.
    pub committed_tip: Option<u64>,
    /// The error of a failed pass, set by the caller that absorbs it (as
    /// [`DaGcHoldReason::StoreError`]) so its slow-pass line can name it.
    pub error: Option<String>,
}

/// Why a GC pass deleted nothing ([`DaGcOutcome::hold_reason`]); its
/// [`as_str`](Self::as_str) is the `reason` label of
/// `torus_da_gc_held_passes`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DaGcHoldReason {
    /// The bookkeeping is behind and the history cannot supply a missing
    /// height (a missing block, a read error), or a pre-cursor store's
    /// prune pointer is unreadable.
    Gap,
    /// The bookkeeping is behind by more than one pass back-fills
    /// ([`DA_GC_MAX_BACKFILL_HEIGHTS`]); the next passes continue.
    Backfilling,
    /// This height's record is unknown (dispatch could not make its body
    /// durable): the cursor stays at it.
    Unrecorded,
    /// A pre-cursor store whose cursor is not established yet (a replay
    /// below its replay floor).
    PreCursor,
    /// A height a back-fill skipped (the history pruned it) is not due yet.
    PrunedGap,
    /// The history holds committed heights above the dispatched one.
    DispatchBehind,
    /// The history's committed tip is unreadable.
    TipUnreadable,
    /// A block above the history's committed tip cannot be read or decoded.
    TailUnreadable,
    /// The pass failed (a store read or write error; set by the caller that
    /// absorbs the error): its batch did not land.
    StoreError,
    /// The history's fence against concurrent body checks is over its cap
    /// ([`crate::DaGcFence`]): it cannot tell which bodies a check is about
    /// to read, so nothing goes until the GC passed every height it was
    /// asked to pin meanwhile.
    FenceFull,
}

impl DaGcHoldReason {
    /// The metric label.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Gap => "gap",
            Self::Backfilling => "backfilling",
            Self::Unrecorded => "unrecorded",
            Self::PreCursor => "pre_cursor",
            Self::PrunedGap => "pruned_gap",
            Self::DispatchBehind => "dispatch_behind",
            Self::TipUnreadable => "tip_unreadable",
            Self::TailUnreadable => "tail_unreadable",
            Self::StoreError => "store_error",
            Self::FenceFull => "fence_full",
        }
    }
}

/// Where the GC re-reads committed heights it has no record for
/// ([`NativeDaStore::gc_committed_with_history`]): the consensus app's block
/// tree.
pub trait DaGcHistory {
    /// Heights below this are pruned: never served again, so not needed.
    /// `0` = nothing pruned. All heights here are the GC's (the caller
    /// converts its history's own numbering).
    fn pruned_below(&self) -> Result<u64, StateError>;
    /// The native-action hashes committed at `height` (at or above
    /// [`pruned_below`](Self::pruned_below)). `Ok(None)` = not available (a
    /// missing block): the GC then deletes nothing until it is.
    fn committed_hashes(&self, height: u64) -> Result<Option<Vec<B256>>, StateError>;
    /// The highest committed height the history holds (`Ok(None)` = none),
    /// in the same height system as the GC's. A committed height above the
    /// dispatched one is not recorded yet and may re-include a due row's
    /// body, so nothing is deleted until dispatch catches up with it.
    fn committed_through(&self) -> Result<Option<u64>, StateError>;
    /// The native-action hashes the history's blocks ABOVE its committed tip
    /// reference (the uncommitted tail, every branch; normally 2-3 blocks).
    /// Such a block may still commit, and peers block-sync it meanwhile, so
    /// a due row's body it references stays. An error (a tail block that
    /// cannot be read or decoded) holds the whole pass.
    fn uncommitted_hashes(&self) -> Result<Vec<B256>, StateError>;
    /// Right before the pass writes its batch: `bodies` (`(hash, shard
    /// count)`) are the due bodies it is about to delete. Drop the ones that
    /// must stay after all and return how many were dropped
    /// ([`DaGcOutcome::fence_pinned`]: the row goes, the body, its reference
    /// and shards stay). Runs only when the pass deletes a body, and is then
    /// followed by exactly one [`end_delete`](Self::end_delete) once the write
    /// returned (landed or failed). A pass off the consensus thread fences
    /// its deletes against concurrent body checks here
    /// ([`crate::DaGcFence`]). Default: keep none.
    fn begin_delete(&self, _bodies: &mut Vec<([u8; 32], u16)>) -> u64 {
        0
    }
    /// The batch [`begin_delete`](Self::begin_delete) preceded returned.
    fn end_delete(&self) {}
    /// Whether the history's fence is over its cap: the pass then deletes
    /// nothing ([`DaGcHoldReason::FenceFull`]). Default: never.
    fn fence_full(&self) -> bool {
        false
    }
}

/// A history that knows no height: a gap in the GC bookkeeping holds every
/// deletion (the safe default of [`NativeDaStore::gc_committed`]).
#[derive(Clone, Copy, Debug, Default)]
pub struct NoDaGcHistory;

impl DaGcHistory for NoDaGcHistory {
    fn pruned_below(&self) -> Result<u64, StateError> {
        Ok(0)
    }

    fn committed_hashes(&self, _height: u64) -> Result<Option<Vec<B256>>, StateError> {
        Ok(None)
    }

    fn committed_through(&self) -> Result<Option<u64>, StateError> {
        Ok(None)
    }

    fn uncommitted_hashes(&self) -> Result<Vec<B256>, StateError> {
        Ok(Vec::new())
    }
}

impl NativeDaStore {
    /// Wrap a shared state-db handle.
    pub fn new(db: StateDb) -> Self {
        Self { db }
    }

    /// Store a body, keyed by its [`compute_action_hash`]. Idempotent: re-putting
    /// the same body overwrites with identical bytes. Wakes every
    /// [`wait_for_arrival`](Self::wait_for_arrival) waiter.
    pub fn put(&self, action: &SignedNativeAction) -> Result<(), StateError> {
        let hash = compute_action_hash(action);
        let bytes =
            bincode::serialize(action).map_err(|e| StateError::InvalidData(e.to_string()))?;
        self.db
            .put_cf_raw(CF_NATIVE_PENDING, hash.as_slice(), &bytes)?;
        let notifier = arrivals();
        let mut generation = notifier
            .generation
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *generation = generation.wrapping_add(1);
        notifier.arrived.notify_all();
        Ok(())
    }

    /// Store many bodies in ONE atomic `WriteBatch`, waking arrival waiters once.
    ///
    /// The leader's `produce_block` mirrors a full block's actions (up to the
    /// per-block cap) before the proposal is encoded; doing that with per-body
    /// [`put`](Self::put) calls issued N separate RocksDB writes + N notifier
    /// wakes on the consensus hot path (S395 floor shave #1). Empty input is a
    /// no-op. Same idempotent overwrite semantics as `put`.
    pub fn put_batch(&self, actions: &[SignedNativeAction]) -> Result<(), StateError> {
        if actions.is_empty() {
            return Ok(());
        }
        let mut batch = rocksdb::WriteBatch::default();
        let cf = self.db.cf_handle(CF_NATIVE_PENDING)?;
        let mut hash_scratch = Vec::with_capacity(64);
        let mut bytes = Vec::new();
        for action in actions {
            let hash = compute_action_hash_with_scratch(action, &mut hash_scratch);
            bytes.clear();
            bincode::serialize_into(&mut bytes, action)
                .map_err(|e| StateError::InvalidData(e.to_string()))?;
            // WriteBatch copies the value into its owned representation before
            // returning, so the next action can safely reuse this buffer.
            batch.put_cf(cf, hash.as_slice(), &bytes);
        }
        self.db.write(batch)?;
        let notifier = arrivals();
        let mut generation = notifier
            .generation
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *generation = generation.wrapping_add(1);
        notifier.arrived.notify_all();
        Ok(())
    }

    /// Snapshot the arrival generation. Take it BEFORE checking the store for
    /// missing bodies and pass it to [`wait_for_arrival`](Self::wait_for_arrival)
    /// so an insert racing the check can never be missed.
    pub fn arrival_generation() -> u64 {
        *arrivals()
            .generation
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Block until any body arrives (a `put` after `seen_generation` was
    /// snapshotted) or `timeout` elapses. Returns the latest generation, which
    /// the caller feeds back in on the next iteration of its wait loop.
    pub fn wait_for_arrival(seen_generation: u64, timeout: std::time::Duration) -> u64 {
        let notifier = arrivals();
        let generation = notifier
            .generation
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if *generation > seen_generation {
            return *generation;
        }
        let (generation, _timed_out) = notifier
            .arrived
            .wait_timeout_while(generation, timeout, |g| *g <= seen_generation)
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *generation
    }

    /// Fetch a body by action-hash. Returns `None` if absent.
    pub fn get(&self, hash: &B256) -> Result<Option<SignedNativeAction>, StateError> {
        match self.db.get_cf_raw(CF_NATIVE_PENDING, hash.as_slice())? {
            Some(bytes) => {
                let action = bincode::deserialize(&bytes)
                    .map_err(|e| StateError::InvalidData(e.to_string()))?;
                Ok(Some(action))
            }
            None => Ok(None),
        }
    }

    /// Batched [`get`](Self::get): one RocksDB `MultiGet` for all `hashes`,
    /// results in input order, `None` per absent hash. A present-but-undecodable
    /// body is `Err(InvalidData)` exactly as in `get`. The hot compact-block
    /// reconstruct path uses this instead of a per-hash `get` loop.
    pub fn get_batch(&self, hashes: &[B256]) -> Result<Vec<Option<SignedNativeAction>>, StateError> {
        self.get_batch_timed(hashes).map(|(actions, _)| actions)
    }

    /// [`get_batch`](Self::get_batch) plus how long the `MultiGet` and the
    /// decode took (s76 validate-path timers).
    pub fn get_batch_timed(
        &self,
        hashes: &[B256],
    ) -> Result<(Vec<Option<SignedNativeAction>>, DaReadTiming), StateError> {
        if hashes.is_empty() {
            return Ok((Vec::new(), DaReadTiming::default()));
        }
        let cf = self.db.cf_handle(CF_NATIVE_PENDING)?;
        let started = std::time::Instant::now();
        // Decode directly from pinned values: the generic raw MultiGet copies
        // each full body into a temporary Vec that this path would discard.
        // Input is deliberately not sorted; duplicates and caller order matter.
        // Collect storage errors before decoding, as the old raw helper did.
        let pinned = self
            .db
            .inner()
            .batched_multi_get_cf(cf, hashes.iter().map(|hash| hash.as_slice()), false)
            .into_iter()
            .collect::<Result<Vec<_>, _>>()?;
        let multi_get = started.elapsed();
        let started = std::time::Instant::now();
        let actions = pinned
            .into_iter()
            .map(|slot| match slot {
                Some(bytes) => bincode::deserialize(&bytes)
                    .map(Some)
                    .map_err(|e| StateError::InvalidData(e.to_string())),
                None => Ok(None),
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok((actions, DaReadTiming { multi_get, decode: started.elapsed() }))
    }

    /// Positions in `hashes` whose body is absent, in input order, from one
    /// pinned `MultiGet` that never decodes or copies a present body. A present
    /// key is exactly as durable as a fresh `put` would make it (same DB, same
    /// write options), so the proposer writes only these positions (s68).
    pub fn missing(&self, hashes: &[B256]) -> Result<Vec<usize>, StateError> {
        if hashes.is_empty() {
            return Ok(Vec::new());
        }
        let cf = self.db.cf_handle(CF_NATIVE_PENDING)?;
        let mut missing = Vec::new();
        for (i, slot) in self
            .db
            .inner()
            .batched_multi_get_cf(cf, hashes.iter().map(|hash| hash.as_slice()), false)
            .into_iter()
            .enumerate()
        {
            if slot?.is_none() {
                missing.push(i);
            }
        }
        Ok(missing)
    }

    /// Fetch the raw stored bytes (`bincode(SignedNativeAction)`) by action-hash,
    /// without deserializing. The `/torus/native-da/1.0` serve path ships these
    /// bytes verbatim to a requesting peer. Takes the 32-byte hash directly so the
    /// network layer needn't depend on `alloy-primitives`. Returns `None` if absent.
    pub fn get_raw(&self, hash: &[u8; 32]) -> Result<Option<Vec<u8>>, StateError> {
        self.db.get_cf_raw(CF_NATIVE_PENDING, hash.as_slice())
    }

    /// Raw batched reads for DA serving. Preserve request order and duplicates;
    /// values are returned verbatim, including undecodable stored bytes.
    /// A storage error fails the batch, allowing callers to retry per key.
    pub fn get_raw_batch(&self, hashes: &[[u8; 32]]) -> Result<Vec<Option<Vec<u8>>>, StateError> {
        let keys: Vec<&[u8]> = hashes.iter().map(|hash| hash.as_slice()).collect();
        self.db.multi_get_cf_raw(CF_NATIVE_PENDING, &keys)
    }

    /// Remove bodies by hash (e.g. after commit + an eviction window). Best-effort:
    /// absent keys are silently skipped.
    pub fn remove(&self, hashes: &[B256]) -> Result<(), StateError> {
        for h in hashes {
            self.db.delete_cf_raw(CF_NATIVE_PENDING, h.as_slice())?;
        }
        Ok(())
    }

    /// Presence check by action-hash WITHOUT copying the body. The pre-warm pull
    /// filter calls this once per manifest hash, so it must not pay `get_raw`'s
    /// multi-KB value copy just to test existence.
    pub fn contains(&self, hash: &[u8; 32]) -> Result<bool, StateError> {
        self.db.exists_cf_raw(CF_NATIVE_PENDING, hash.as_slice())
    }

    /// RH4 durable body GC: one pass per dispatched committed height.
    ///
    /// Records `hashes` (the committed block's action hashes; `shard_count`
    /// erasure shards were custodied per body, 0 = none) as the retention row
    /// of `height`, then deletes the bodies, the shards and the row of every
    /// recorded height `h` with `h + retention <= height` AND `h < keep_from`,
    /// so a body survives `retention` heights past its own and is never
    /// deleted at or above `keep_from`. The additive form keeps everything
    /// while `height < retention` (`height - retention` floored at 0 deleted
    /// height 0: the July RH4 bug). Bodies no row lists (never committed, or
    /// expired: pool expiry is local-clock and a skewed peer may still include
    /// them) are never touched.
    ///
    /// `keep_from` is the caller's bound from what the node can still serve:
    /// the consensus app passes the first height the block tree still serves
    /// (its durable prune pointer, converted to this height system) minus a
    /// margin (Codex P1: a window derived from the configured block-tree
    /// retention runs ahead of the pruner, which removes at most 64 heights
    /// per commit). `u64::MAX` = no bound, `0` = delete nothing.
    ///
    /// A hash can be committed again at a later height (a re-inclusion). Only
    /// the row of its LATEST recorded height deletes it: a durable per-hash
    /// reference ([`DA_GC_REF_PREFIX`]) finds the earlier row, and a marker
    /// written in the same batch makes that row's GC skip the hash, so the
    /// body and every shard written for it go when the latest reference
    /// expires (Codex P1, P2). A `height` below one already recorded is a
    /// replay and records nothing.
    ///
    /// Never delete while the bookkeeping is behind (Codex P1, pre-merge
    /// review): the GC state's cursor says which heights are recorded. A
    /// `height` above it means a dispatched height has no record (a crash
    /// between the body write and this pass, a failed record write), so a
    /// re-inclusion's pin may be missing. This variant has no history to fill
    /// such a gap: the pass records and deletes nothing, ever, until a pass
    /// with a history ([`gc_committed_with_history`](Self::gc_committed_with_history))
    /// fills it.
    ///
    /// One atomic `WriteBatch` per pass (the row, the references, the markers
    /// of a re-inclusion, the deletes and the GC state), bounded by `limits`;
    /// the first due row is always taken whole, so the GC always progresses.
    /// The rows live next to the bodies (non-32-byte keys), so a crash never
    /// splits a row from its bodies and a restart leaves none behind. Empty
    /// `hashes` records nothing; a `retention` of 0 counts as 1.
    ///
    /// Cost per pass: one point read (the GC state), one `MultiGet` of the
    /// recorded hashes' reference keys (normally all bloom-filter misses), a
    /// bounded iterator over the due rows and one over their markers (normally
    /// none), one batch. The record is linear in `hashes` whatever earlier
    /// rows they come from (Codex P2).
    pub fn gc_committed(
        &self,
        height: u64,
        hashes: &[B256],
        shard_count: u16,
        retention: u64,
        keep_from: u64,
        limits: DaGcLimits,
    ) -> Result<DaGcOutcome, StateError> {
        self.gc_committed_with_history(
            height,
            Some(hashes),
            shard_count,
            retention,
            keep_from,
            limits,
            &NoDaGcHistory,
        )
    }

    /// [`gc_committed`](Self::gc_committed) with a [`DaGcHistory`] to
    /// back-fill missed heights from (the consensus app's block tree).
    ///
    /// `hashes`: `height`'s record, `None` = unknown (dispatch could not
    /// make the block durable). An unknown record is never written as "no
    /// actions" (that would drop its re-inclusion pins): the cursor stays at
    /// `height`, the pass deletes nothing, and the next pass back-fills it
    /// from `history`.
    ///
    /// Nothing is deleted either while `history` holds committed heights
    /// above `height` ([`DaGcHistory::committed_through`], read only when a
    /// row is due; an error holds too, [`DaGcOutcome::undispatched_commits`]):
    /// they are not recorded yet and one may re-include a due row's body.
    /// And a due body a block ABOVE the committed tip references
    /// ([`DaGcHistory::uncommitted_hashes`], read at the first due row) stays
    /// while its row goes ([`DaGcOutcome::tail_pinned`]); an unreadable tail
    /// holds the pass. A held pass names why ([`DaGcOutcome::hold_reason`]).
    ///
    /// Before any deletion, every height from the cursor up to `height` gets
    /// its record: the missing ones from `history` (at most
    /// [`DA_GC_MAX_BACKFILL_HEIGHTS`] per pass; their bodies and shards then
    /// follow the same rules, re-inclusion pins included), then `height`'s own
    /// from `hashes`. Records are idempotent and the cursor lands in the
    /// batch of the records it covers, so a crash or a failed write only
    /// leaves the cursor behind, and the next pass re-does exactly the
    /// missing heights. A height the history cannot supply (`Ok(None)` or an
    /// error) stops the back-fill: the pass writes the heights before it,
    /// deletes nothing ([`DaGcOutcome::backfill_stalled`]) and the next pass
    /// retries. Heights below the history's pruned height are skipped (their
    /// blocks are never served again), but nothing is deleted until the last
    /// of them is itself due (window and `keep_from`): one of them may have
    /// re-included a body an older row lists.
    ///
    /// A back-filling pass writes its records in one batch, then deletes in a
    /// second (the deletion pass reads the rows from the DB, so it must see
    /// the back-filled ones); a crash between the two only defers the
    /// deletion. A pass without a gap is one batch, as before.
    #[allow(clippy::too_many_arguments)]
    pub fn gc_committed_with_history(
        &self,
        height: u64,
        hashes: Option<&[B256]>,
        shard_count: u16,
        retention: u64,
        keep_from: u64,
        limits: DaGcLimits,
        history: &dyn DaGcHistory,
    ) -> Result<DaGcOutcome, StateError> {
        self.gc_committed_with_history_timed(
            height,
            hashes,
            shard_count,
            retention,
            keep_from,
            limits,
            history,
            &mut DaGcPassStats::default(),
        )
    }

    /// [`gc_committed_with_history`](Self::gc_committed_with_history), and
    /// where the pass spent its time ([`DaGcPassStats`], added to `stats`):
    /// the same decisions and the same writes.
    #[allow(clippy::too_many_arguments)]
    pub fn gc_committed_with_history_timed(
        &self,
        height: u64,
        hashes: Option<&[B256]>,
        shard_count: u16,
        retention: u64,
        keep_from: u64,
        limits: DaGcLimits,
        history: &dyn DaGcHistory,
        stats: &mut DaGcPassStats,
    ) -> Result<DaGcOutcome, StateError> {
        let cf = self.db.cf_handle(CF_NATIVE_PENDING)?;
        // Every fallible phase is timed with its `Result` and only then
        // propagated: a failed (and maybe slow) phase still reports its time
        // (Codex P2, 8h review).
        let state = timed(&mut stats.state_read, || {
            self.db
                .get_cf_raw(CF_NATIVE_PENDING, DA_GC_STATE_KEY)
                .map(|raw| DaGcState::decode(raw.as_deref()))
        })?;
        let mut next = state;
        let mut batch = rocksdb::WriteBatch::default();
        let mut out = DaGcOutcome::default();
        // Hashes this record moved out of an earlier row: if that row ages out
        // in this same pass, its read-back copy still lists them. A set: the
        // membership tests below run once per listed hash (Codex P2).
        let mut moved: HashSet<[u8; 32]> = HashSet::new();
        let mut backfilled = false;
        // The cursor this pass starts from. Not established yet:
        // - a fresh store (no GC state, so no row an unrecorded pin could
        //   matter for) starts it at this height;
        // - a pre-cursor store (owner, s128) holds rows no cursor vouches
        //   for, and its binary may have missed a re-inclusion's record (the
        //   P1 crash): start it where the block tree's retained range starts
        //   (its prune pointer; never below the frontier, whose rows are
        //   gone), so the back-fill re-reads every height the tree still
        //   serves before anything is deleted. Heights the tree already
        //   pruned are held like a back-fill skip.
        // A replay (below the pre-cursor replay floor) records nothing.
        let floor = state.legacy_next_record.max(state.frontier);
        let start = match state.cursor {
            Some(cursor) => Some(cursor),
            None if height < floor => None,
            None if !state.pre_cursor => Some(height),
            None => match history.pruned_below() {
                Ok(pruned) => {
                    let start = state.frontier.max(pruned.min(height));
                    if start > state.frontier {
                        next.hold = next.hold.max(start);
                    }
                    Some(start)
                }
                Err(e) => {
                    tracing::warn!(
                        height,
                        "DA body GC: upgrading a pre-cursor store, the block tree's prune \
                         pointer is unreadable, deleting nothing this pass: {e}"
                    );
                    out.backfill_stalled = true;
                    None
                }
            },
        };
        match start {
            None => {}
            // A replay: already recorded (or already GC'd).
            Some(cursor) if height < cursor => {}
            Some(cursor) => {
                let mut recorded_to = cursor;
                let mut refs = PendingRefs::new();
                if cursor < height {
                    backfilled = true;
                    recorded_to = timed(&mut stats.backfill, || {
                        self.backfill_rows(
                            &mut batch,
                            cursor,
                            height,
                            shard_count,
                            state.frontier,
                            history,
                            &mut next.hold,
                            &mut refs,
                            &mut out,
                        )
                    })?;
                }
                // An unknown record (`None`) is never written as "no
                // actions": the cursor stays at `height`, so this pass
                // deletes nothing and the next one back-fills it.
                if let Some(hashes) = hashes.filter(|_| recorded_to == height) {
                    if !hashes.is_empty() {
                        timed(&mut stats.record, || {
                            self.record_row(
                                &mut batch,
                                height,
                                hashes,
                                shard_count,
                                state.frontier,
                                &mut moved,
                                backfilled.then_some(&mut refs),
                            )
                        })?;
                    }
                    recorded_to = height.saturating_add(1);
                }
                next.cursor = Some(recorded_to);
            }
        }
        out.references_moved = moved.len() as u64;
        // Still behind (stalled, or the back-fill bound), or a pre-cursor
        // store not yet vouched for: delete nothing.
        let behind = out.backfill_stalled
            || next.cursor.is_some_and(|c| c <= height)
            || (next.cursor.is_none() && state.pre_cursor);
        // Rows at or below `last_due` are due: `h + retention <= height` and
        // `h < keep_from`.
        let last_due = height
            .checked_sub(retention.max(1))
            .zip(keep_from.checked_sub(1))
            .map(|(window, served)| window.min(served));
        // Every height a back-fill skipped must be due itself.
        let held = next.hold > 0 && last_due.is_none_or(|due| due < next.hold - 1);
        let mut written = state;
        if backfilled && next != state {
            // The deletion pass reads rows from the DB: land the back-filled
            // ones (and the cursor covering them) first.
            batch.put_cf(cf, DA_GC_STATE_KEY, next.encode());
            timed(&mut stats.write, || {
                self.db.write(std::mem::take(&mut batch))
            })?;
            written = next;
            moved.clear();
        }
        // Why nothing may go, the first rule that holds (P3: every held
        // pass is counted by its reason).
        let mut hold = if out.backfill_stalled {
            Some(DaGcHoldReason::Gap)
        } else if let Some(cursor) = next.cursor.filter(|&c| c <= height) {
            // Short of `height`: the back-fill bound; AT it: `height`'s own
            // record is unknown (`hashes` = `None`).
            Some(if cursor < height {
                DaGcHoldReason::Backfilling
            } else {
                DaGcHoldReason::Unrecorded
            })
        } else if behind {
            Some(DaGcHoldReason::PreCursor)
        } else if held {
            Some(DaGcHoldReason::PrunedGap)
        } else {
            None
        };
        // Something is due: committed heights above `height` are not
        // recorded yet (dispatch is behind the committed tip) and may
        // re-include a due row's body, so nothing goes until dispatch caught
        // up. Read only when a deletion is otherwise allowed.
        let due = last_due.filter(|&due| next.frontier <= due);
        if hold.is_none() && due.is_some() {
            let t = Instant::now();
            let tip = history.committed_through();
            stats.tip_read += t.elapsed();
            match tip {
                Ok(tip) => {
                    stats.committed_tip = tip;
                    out.undispatched_commits = tip.is_some_and(|tip| tip > height);
                    if out.undispatched_commits {
                        hold = Some(DaGcHoldReason::DispatchBehind);
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        height,
                        "DA body GC: the history's committed tip is unreadable, deleting \
                         nothing this pass: {e}"
                    );
                    hold = Some(DaGcHoldReason::TipUnreadable);
                }
            }
        }
        // A full fence (GC worker) cannot say which bodies a concurrent check
        // is about to read: nothing goes, the rows stay for a later pass. (A
        // fence that fills between here and `begin_delete` keeps every body
        // there instead: those rows go, their bodies stay.)
        if hold.is_none() && due.is_some() && history.fence_full() {
            hold = Some(DaGcHoldReason::FenceFull);
        }
        // The due bodies to delete, `(hash, shard count)`: added to the batch
        // only after the history's fence saw them, right before the write.
        let mut bodies: Vec<([u8; 32], u16)> = Vec::new();
        if hold.is_none() {
            if let Some(last_due) = due {
                let t = Instant::now();
                let tail_before = stats.tail_read;
                let rows = self.gc_due_rows(
                    &mut batch,
                    next.frontier,
                    last_due,
                    limits,
                    &moved,
                    history,
                    &mut out,
                    stats,
                    &mut bodies,
                );
                // `stats` goes into the walk (its tail read), so not `timed`.
                stats.delete_scan += t.elapsed().saturating_sub(stats.tail_read - tail_before);
                match rows? {
                    DueRows::Deleted(Some(frontier)) => next.frontier = frontier,
                    DueRows::Deleted(None) => {}
                    DueRows::TailUnreadable => {
                        bodies.clear();
                        hold = Some(DaGcHoldReason::TailUnreadable);
                    }
                }
            }
        }
        if let Some(reason) = hold {
            out.deletion_held = true;
            out.hold_reason = Some(reason);
        }
        if next != written {
            batch.put_cf(cf, DA_GC_STATE_KEY, next.encode());
        }
        // Nothing fallible between the fence's begin and end but the write.
        let shards_cf = if bodies.iter().any(|&(_, shards)| shards > 0) {
            Some(self.db.cf_handle(CF_NATIVE_SHARDS)?)
        } else {
            None
        };
        let fenced = !bodies.is_empty();
        if fenced {
            out.fence_pinned = history.begin_delete(&mut bodies);
            for (hash, shards) in &bodies {
                batch.delete_cf(cf, hash);
                batch.delete_cf(cf, da_gc_ref_key(hash));
                if let Some(shards_cf) = shards_cf {
                    for i in 0..*shards {
                        batch.delete_cf(shards_cf, shard_key(hash, i));
                    }
                }
                out.bodies_deleted += 1;
                out.shard_keys_deleted += u64::from(*shards);
            }
        }
        let wrote = if batch.is_empty() {
            Ok(())
        } else {
            timed(&mut stats.write, || self.db.write(batch))
        };
        if fenced {
            history.end_delete();
        }
        wrote?;
        Ok(out)
    }

    /// Add the records of the heights `[cursor, height)` to `batch` from
    /// `history`: skip those below its pruned height (raising `hold` past
    /// them), then record up to [`DA_GC_MAX_BACKFILL_HEIGHTS`] in order.
    /// Returns the first height not recorded (`height` when the gap is
    /// closed). A height the history cannot supply stops it there
    /// (`out.backfill_stalled`, logged).
    #[allow(clippy::too_many_arguments)]
    fn backfill_rows(
        &self,
        batch: &mut rocksdb::WriteBatch,
        cursor: u64,
        height: u64,
        shard_count: u16,
        frontier: u64,
        history: &dyn DaGcHistory,
        hold: &mut u64,
        refs: &mut PendingRefs,
        out: &mut DaGcOutcome,
    ) -> Result<u64, StateError> {
        let mut at = cursor;
        let pruned = match history.pruned_below() {
            Ok(pruned) => pruned,
            Err(e) => {
                tracing::warn!(
                    height,
                    behind_from = cursor,
                    "DA body GC: bookkeeping behind and the history's pruned height is \
                     unreadable, deleting nothing this pass: {e}"
                );
                out.backfill_stalled = true;
                return Ok(at);
            }
        };
        if pruned > at {
            // Never served again: skip, but hold deletion until they are due.
            at = pruned.min(height);
            *hold = (*hold).max(at);
            tracing::info!(
                skipped_from = cursor,
                skipped_to = at,
                "DA body GC: skipped heights the block tree already pruned; deletion waits \
                 until they are due"
            );
        }
        let end = height.min(at.saturating_add(DA_GC_MAX_BACKFILL_HEIGHTS));
        while at < end {
            match history.committed_hashes(at) {
                Ok(Some(hashes)) => {
                    if !hashes.is_empty() {
                        // Per height: a marker per (earlier row, hash).
                        let mut moved = HashSet::new();
                        self.record_row(
                            batch,
                            at,
                            &hashes,
                            shard_count,
                            frontier,
                            &mut moved,
                            Some(&mut *refs),
                        )?;
                    }
                    out.heights_backfilled += 1;
                    at += 1;
                }
                Ok(None) => {
                    tracing::warn!(
                        height,
                        missing = at,
                        "DA body GC: bookkeeping behind and the block tree lacks a height to \
                         back-fill, deleting nothing this pass"
                    );
                    out.backfill_stalled = true;
                    break;
                }
                Err(e) => {
                    tracing::warn!(
                        height,
                        missing = at,
                        "DA body GC: bookkeeping behind and back-filling a height failed, \
                         deleting nothing this pass: {e}"
                    );
                    out.backfill_stalled = true;
                    break;
                }
            }
        }
        Ok(at)
    }

    /// Add the row of `height` and the latest-reference keys of `hashes` to
    /// `batch`. A hash whose reference names an earlier row still pending (at
    /// or above `frontier`) is a re-inclusion: a marker moves it out of that
    /// row and it goes to `moved`. Linear in `hashes`: no earlier row is read
    /// or rewritten (Codex P2).
    ///
    /// The new row takes the largest shard count of the rows it took hashes
    /// from (each reference carries its row's, Codex P2): a moved hash's
    /// shards were custodied under that count (a larger validator set, say),
    /// and its latest expiry must delete every index written. The new row's
    /// other hashes then also get deletes for indices they never had:
    /// tombstones, only on a re-inclusion after a set shrink.
    ///
    /// `pending`: the references recorded earlier in the same batch (a
    /// back-fill records several heights in one), which win over the DB's;
    /// this row's references are added to it.
    #[allow(clippy::too_many_arguments)]
    fn record_row(
        &self,
        batch: &mut rocksdb::WriteBatch,
        height: u64,
        hashes: &[B256],
        shard_count: u16,
        frontier: u64,
        moved: &mut HashSet<[u8; 32]>,
        pending: Option<&mut PendingRefs>,
    ) -> Result<(), StateError> {
        let cf = self.db.cf_handle(CF_NATIVE_PENDING)?;
        let ref_keys: Vec<[u8; DA_GC_REF_KEY_LEN]> =
            hashes.iter().map(|h| da_gc_ref_key(h.as_slice())).collect();
        let mut row_shard_count = shard_count;
        let refs =
            self.db
                .inner()
                .batched_multi_get_cf(cf, ref_keys.iter().map(|k| k.as_slice()), false);
        for (hash, slot) in hashes.iter().zip(refs) {
            let slot = slot?;
            let earlier = pending.as_deref().and_then(|p| p.get(&hash.0).copied());
            let Some((prev, prev_shards)) =
                earlier.or_else(|| slot.as_deref().and_then(decode_da_gc_ref))
            else {
                continue;
            };
            if (frontier..height).contains(&prev) {
                row_shard_count = row_shard_count.max(prev_shards);
                if moved.insert(hash.0) {
                    batch.put_cf(cf, da_gc_moved_key(prev, hash.as_slice()), []);
                }
            }
        }
        batch.put_cf(
            cf,
            da_gc_row_key(height),
            encode_da_gc_row(row_shard_count, hashes),
        );
        let reference = encode_da_gc_ref(height, row_shard_count);
        for key in &ref_keys {
            batch.put_cf(cf, key, reference);
        }
        if let Some(pending) = pending {
            for hash in hashes {
                pending.insert(hash.0, (height, row_shard_count));
            }
        }
        Ok(())
    }

    /// Add the deletes of the rows in `[frontier, last_due]` to `batch`, within
    /// `limits`: the row and its markers; each listed body (bar the hashes in
    /// `moved`, those the row's markers name and those the history's
    /// uncommitted tail references) goes to `bodies` with the row's shard
    /// count, its body, reference and shard deletes added by the caller once
    /// the history's fence saw them. Returns the new frontier when any row
    /// went.
    ///
    /// The tail ([`DaGcHistory::uncommitted_hashes`]) is read once, at the
    /// first due row (a pass with no due row reads nothing): a body a block
    /// above the committed tip references stays, its row goes (claude-review
    /// r4 / Codex r3 P2). An unreadable tail adds nothing to `batch` and
    /// returns [`DueRows::TailUnreadable`]: the pass holds.
    #[allow(clippy::too_many_arguments)]
    fn gc_due_rows(
        &self,
        batch: &mut rocksdb::WriteBatch,
        frontier: u64,
        last_due: u64,
        limits: DaGcLimits,
        moved: &HashSet<[u8; 32]>,
        history: &dyn DaGcHistory,
        out: &mut DaGcOutcome,
        stats: &mut DaGcPassStats,
        bodies: &mut Vec<([u8; 32], u16)>,
    ) -> Result<DueRows, StateError> {
        let cf = self.db.cf_handle(CF_NATIVE_PENDING)?;
        let start = da_gc_row_key(frontier);
        let mut opts = rocksdb::ReadOptions::default();
        opts.set_iterate_lower_bound(start.to_vec());
        // `last_due < height <= u64::MAX`, so `last_due + 1` cannot overflow.
        opts.set_iterate_upper_bound(da_gc_row_key(last_due + 1).to_vec());
        let mut it = self.db.inner().raw_iterator_cf_opt(cf, opts);
        it.seek(start);
        // The markers of the same heights, walked in step with the rows.
        let markers_start = da_gc_moved_key(frontier, &[]);
        let mut opts = rocksdb::ReadOptions::default();
        opts.set_iterate_lower_bound(markers_start.clone());
        opts.set_iterate_upper_bound(da_gc_moved_key(last_due + 1, &[]));
        let mut markers = self.db.inner().raw_iterator_cf_opt(cf, opts);
        markers.seek(markers_start);
        let mut marked: HashSet<[u8; 32]> = HashSet::new();
        let mut tail: Option<HashSet<[u8; 32]>> = None;
        let mut next_frontier = last_due + 1;
        let mut deletes = 0usize;
        while let (Some(key), Some(value)) = (it.key(), it.value()) {
            // A 32-byte body key can share the 7-byte prefix (p = 2^-56):
            // never treat it as a row.
            if key.len() != DA_GC_ROW_KEY_LEN || !key.starts_with(DA_GC_ROW_PREFIX) {
                it.next();
                continue;
            }
            let h = u64::from_be_bytes(key[DA_GC_ROW_PREFIX.len()..].try_into().expect("8 bytes"));
            let (row_shards, row_hashes) = decode_da_gc_row(value).unwrap_or_else(|| {
                tracing::warn!(height = h, len = value.len(), "malformed DA GC row dropped");
                (0, &[][..])
            });
            let pins = match &mut tail {
                Some(pins) => &*pins,
                // The first due row: nothing is in `batch` from this walk yet.
                None => match timed(&mut stats.tail_read, || history.uncommitted_hashes()) {
                    Ok(hashes) => {
                        stats.tail_hashes = hashes.len() as u64;
                        &*tail.insert(hashes.iter().map(|h| h.0).collect())
                    }
                    Err(e) => {
                        tracing::warn!(
                            due_from = h,
                            "DA body GC: a block above the committed tip is unreadable, \
                             deleting nothing this pass: {e}"
                        );
                        return Ok(DueRows::TailUnreadable);
                    }
                },
            };
            let listed = row_hashes.len() / 32;
            let row_deletes = listed * (1 + usize::from(row_shards));
            if out.rows_deleted as usize >= limits.max_rows.max(1)
                || (out.rows_deleted > 0 && deletes + row_deletes > limits.max_deletes)
            {
                next_frontier = h;
                out.backlog = true;
                break;
            }
            stats.bodies_scanned += listed as u64;
            take_da_gc_markers(&mut markers, batch, cf, h, &mut marked)?;
            for hash in row_hashes.chunks_exact(32) {
                // Re-included at this pass's height: the new row owns it (and
                // the marker this pass wrote for this row goes with the row).
                if moved.contains(hash) {
                    batch.delete_cf(cf, da_gc_moved_key(h, hash));
                    continue;
                }
                // Re-included at a later, still pending height.
                if marked.contains(hash) {
                    continue;
                }
                // Referenced by a block above the committed tip: it may still
                // commit (and is block-synced meanwhile). The body, its
                // reference and shards stay; if that block commits, its
                // record owns them; if it never does, they stay for good
                // (a faulty leader's re-inclusion only).
                if pins.contains(hash) {
                    out.tail_pinned += 1;
                    continue;
                }
                bodies.push((hash.try_into().expect("32-byte chunk"), row_shards));
            }
            batch.delete_cf(cf, key);
            deletes += row_deletes;
            out.rows_deleted += 1;
            it.next();
        }
        it.status()?;
        if !out.backlog {
            // Every row up to `last_due` went: a marker left there has no row.
            take_da_gc_markers(&mut markers, batch, cf, last_due, &mut marked)?;
        }
        Ok(DueRows::Deleted((out.rows_deleted > 0).then_some(next_frontier)))
    }

    /// Erasure-encode each body under `params` and persist all `n` shards into
    /// [`CF_NATIVE_SHARDS`] in ONE atomic `WriteBatch` (Sprint 5 T5, recovery-path).
    ///
    /// ADDITIVE: the whole-body mirror ([`put_batch`](Self::put_batch)) remains the
    /// durability guarantee; this only ADDS shard custody so a lagging peer can
    /// reconstruct from `k` shards gathered across `k` DIFFERENT sources instead of
    /// pulling the whole body from one (kills the s338 single-source hotspot). The
    /// bytes sharded are the SAME `bincode(SignedNativeAction)` keyed by
    /// [`compute_action_hash`], so the body-hash backstop at reconstruct time
    /// re-hashes to exactly this. Empty input is a no-op.
    pub fn put_shards_batch(
        &self,
        actions: &[SignedNativeAction],
        params: ErasureParams,
    ) -> Result<(), StateError> {
        if actions.is_empty() {
            return Ok(());
        }
        params
            .validate()
            .map_err(|e| StateError::InvalidData(e.to_string()))?;
        let mut batch = rocksdb::WriteBatch::default();
        let cf = self.db.cf_handle(CF_NATIVE_SHARDS)?;
        let mut hash_scratch = Vec::with_capacity(64);
        let mut body = Vec::new();
        for action in actions {
            let hash = compute_action_hash_with_scratch(action, &mut hash_scratch);
            body.clear();
            bincode::serialize_into(&mut body, action)
                .map_err(|e| StateError::InvalidData(e.to_string()))?;
            // EncodedBody owns its shards; no body-buffer borrow survives this
            // iteration. All shards still enter the same atomic WriteBatch.
            let enc = encode(&body, params).map_err(|e| StateError::InvalidData(e.to_string()))?;
            for i in 0..enc.params.n {
                let stored = StoredShard::from_encoded_body(&enc, i);
                let bytes = encode_stored_shard(&stored)?;
                batch.put_cf(cf, shard_key(&hash.0, i as u16), &bytes);
            }
        }
        self.db.write(batch)?;
        Ok(())
    }

    /// Fetch a single custodied shard for `(body_hash, index)`. `None` when this
    /// node does not custody it (the serve path answers `present=false`; a fetcher
    /// then tries another peer/index).
    pub fn get_shard(
        &self,
        body_hash: &[u8; 32],
        index: u16,
    ) -> Result<Option<StoredShard>, StateError> {
        match self
            .db
            .get_cf_raw(CF_NATIVE_SHARDS, &shard_key(body_hash, index))?
        {
            Some(bytes) => Ok(Some(decode_stored_shard(&bytes)?)),
            None => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store() -> (tempfile::TempDir, NativeDaStore) {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = StateDb::open(dir.path()).expect("open temp StateDb");
        (dir, NativeDaStore::new(db))
    }

    /// `contains` must reflect raw presence in CF_NATIVE_PENDING without needing
    /// a decodable body (the serve path also ships raw bytes verbatim).
    #[test]
    fn contains_reflects_put_and_absent() {
        let (_dir, store) = temp_store();
        let present = [7u8; 32];
        let absent = [9u8; 32];
        store
            .db
            .put_cf_raw(CF_NATIVE_PENDING, &present, b"body-bytes")
            .expect("raw put");
        assert!(store.contains(&present).expect("contains present"));
        assert!(!store.contains(&absent).expect("contains absent"));
    }

    /// `get_batch` must return one slot per input hash, in input order, with
    /// `None` for absent keys — the single-read replacement for the per-hash
    /// `get` loop on the hot reconstruct path.
    #[test]
    fn get_batch_preserves_order_and_reports_missing() {
        let (_dir, store) = temp_store();
        let a1 = dummy_action(1);
        let a2 = dummy_action(2);
        let a3 = dummy_action(3);
        let h1 = compute_action_hash(&a1);
        let h3 = compute_action_hash(&a3);
        let missing = B256::repeat_byte(0xAB);
        store.put_batch(&[a1.clone(), a2, a3.clone()]).expect("put_batch");

        let got = store.get_batch(&[h1, missing, h3]).expect("get_batch");
        assert_eq!(got.len(), 3);
        assert_eq!(got[0].as_ref().map(compute_action_hash), Some(h1));
        assert!(got[1].is_none(), "absent hash must read as None");
        assert_eq!(got[2].as_ref().map(compute_action_hash), Some(h3));

        assert!(store.get_batch(&[]).expect("empty get_batch").is_empty());
    }

    /// s76: the timed variant returns exactly what `get_batch` returns, plus
    /// the MultiGet and decode split the hot reconstruct timers need.
    #[test]
    fn get_batch_timed_matches_get_batch() {
        let (_dir, store) = temp_store();
        let a1 = dummy_action(1);
        let h1 = compute_action_hash(&a1);
        let missing = B256::repeat_byte(0xAB);
        store.put_batch(&[a1]).expect("put_batch");

        let (got, timing) = store.get_batch_timed(&[h1, missing]).expect("get_batch_timed");
        let plain = store.get_batch(&[h1, missing]).expect("get_batch");
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].as_ref().map(compute_action_hash), Some(h1));
        assert!(got[1].is_none());
        assert_eq!(
            got.iter().map(|a| a.as_ref().map(compute_action_hash)).collect::<Vec<_>>(),
            plain.iter().map(|a| a.as_ref().map(compute_action_hash)).collect::<Vec<_>>()
        );
        assert!(timing.multi_get > std::time::Duration::ZERO);

        let (empty, timing) = store.get_batch_timed(&[]).expect("empty");
        assert!(empty.is_empty());
        assert_eq!(timing, DaReadTiming::default());
    }

    /// `missing` reports the input positions whose body is absent, in input
    /// order (duplicates included), without decoding any present body. The
    /// proposer uses it to write only the bodies ingest has not already stored.
    #[test]
    fn missing_reports_absent_positions_in_input_order() {
        let (_dir, store) = temp_store();
        let a1 = dummy_action(1);
        let a3 = dummy_action(3);
        let h1 = compute_action_hash(&a1);
        let h2 = compute_action_hash(&dummy_action(2));
        let h3 = compute_action_hash(&a3);
        store.put_batch(&[a1, a3]).expect("put_batch");

        assert_eq!(store.missing(&[h1, h2, h3, h2]).expect("missing"), vec![1, 3]);
        assert!(store.missing(&[h1, h3]).expect("all present").is_empty());
        assert!(store.missing(&[]).expect("empty input").is_empty());
    }

    /// A present-but-undecodable body is an `Err`, exactly like `get`.
    #[test]
    fn get_batch_rejects_invalid_data() {
        let (_dir, store) = temp_store();
        let bad = [5u8; 32];
        store
            .db
            .put_cf_raw(CF_NATIVE_PENDING, &bad, b"not-bincode")
            .expect("raw put");
        assert!(matches!(
            store.get_batch(&[B256::from(bad)]),
            Err(StateError::InvalidData(_))
        ));
    }

    #[test]
    fn pinned_batch_matches_single_reads_for_unsorted_duplicates_and_sst_values() {
        use torus_types::{FixedPoint, NativeAction, OrderType, PlaceOrderParams, TimeInForce};
        let (dir, store) = temp_store();
        let mut large = dummy_action(2);
        large.action = NativeAction::PlaceOrderBatch(vec![PlaceOrderParams {
            market_id: 7,
            is_buy: true,
            price: FixedPoint::from_raw(123_000_000),
            quantity: FixedPoint::from_raw(100_000_000),
            order_type: OrderType::Limit,
            time_in_force: TimeInForce::GTC,
            reduce_only: false,
            client_order_id: None,
        }; 400]);
        let actions = [dummy_action(1), large, dummy_action(3)];
        store.put_batch(&actions).unwrap();
        let mut hashes: Vec<_> = actions.iter().map(compute_action_hash).collect();
        hashes.sort_unstable();
        hashes.reverse();
        hashes.insert(1, B256::repeat_byte(0xAB));
        hashes.extend_from_within(0..3);
        let check = |store: &NativeDaStore| {
            let expected: Vec<_> = hashes.iter().map(|hash| store.get(hash).unwrap()).collect();
            let got = store.get_batch(&hashes).unwrap();
            assert_eq!(bincode::serialize(&got).unwrap(), bincode::serialize(&expected).unwrap());
            assert_eq!(got.len(), hashes.len());
            assert!(store.get_batch(&[]).unwrap().is_empty());
        };
        check(&store); // memtable values
        let cf = store.db.cf_handle(CF_NATIVE_PENDING).unwrap();
        store.db.inner().flush_cf(cf).unwrap();
        store.db.inner().compact_range_cf(cf, None::<&[u8]>, None::<&[u8]>);
        check(&store); // SST values and block-cache pins
        drop(store);
        let reopened = NativeDaStore::new(StateDb::open(dir.path()).unwrap());
        check(&reopened);
        // Overwrite and delete after prior reads; pins must not escape decoding.
        reopened.db.put_cf_raw(CF_NATIVE_PENDING, hashes[0].as_slice(), b"bad").unwrap();
        assert!(matches!(reopened.get_batch(&hashes), Err(StateError::InvalidData(_))));
        reopened.remove(&[hashes[0]]).unwrap();
        check(&reopened);
    }

    fn dummy_action(nonce: u64) -> SignedNativeAction {
        use torus_types::{ActionSignature, NativeAction, Signature};
        SignedNativeAction {
            action: NativeAction::ClaimRewards,
            nonce,
            signature: ActionSignature::Eip712(Signature {
                v: 27,
                r: [0u8; 32],
                s: [0u8; 32],
            }),
        }
    }

    // Frozen 6a51413 shard batch oracle: keep allocating bincode::serialize
    // independent of the reusable production serializer.
    fn legacy_put_shards_batch(
        store: &NativeDaStore,
        actions: &[SignedNativeAction],
        params: ErasureParams,
    ) -> Result<(), StateError> {
        if actions.is_empty() {
            return Ok(());
        }
        params
            .validate()
            .map_err(|e| StateError::InvalidData(e.to_string()))?;
        let mut batch = rocksdb::WriteBatch::default();
        let cf = store.db.cf_handle(CF_NATIVE_SHARDS)?;
        for action in actions {
            let hash = compute_action_hash(action);
            let body =
                bincode::serialize(action).map_err(|e| StateError::InvalidData(e.to_string()))?;
            let enc = encode(&body, params).map_err(|e| StateError::InvalidData(e.to_string()))?;
            for i in 0..enc.params.n {
                let stored = StoredShard::from_encoded_body(&enc, i);
                let bytes = encode_stored_shard(&stored)?;
                batch.put_cf(cf, shard_key(&hash.0, i as u16), &bytes);
            }
        }
        store.db.write(batch)?;
        Ok(())
    }

    #[test]
    fn batch_hash_scratch_preserves_body_and_shard_bytes_across_reuse_and_reopen() {
        use crate::StateBackend;
        use torus_types::{ActionSignature, Ed25519Sig, FixedPoint, NativeAction,
            OrderType, PlaceOrderParams, TimeInForce};
        let (dir, actual) = temp_store();
        let (_reference_dir, reference) = temp_store();
        let large = SignedNativeAction {
            action: NativeAction::PlaceOrderBatch(vec![PlaceOrderParams {
                market_id: 7, is_buy: true, price: FixedPoint::from_raw(123_000_000),
                quantity: FixedPoint::from_raw(100_000_011),
                order_type: OrderType::StopLimit {
                    trigger: FixedPoint::from_raw(200_000_000),
                    limit: FixedPoint::from_raw(123_000_000),
                },
                time_in_force: TimeInForce::GTC, reduce_only: false, client_order_id: Some(42),
            }; 400]),
            nonce: u64::MAX,
            signature: ActionSignature::Session {
                session_pubkey: [7; 32], sig: Ed25519Sig([9; 64]),
            },
        };
        let mut small_session = dummy_action(3);
        small_session.signature = large.signature.clone();
        // Repeated first body also pins the existing idempotent overwrite.
        let actions = vec![dummy_action(1), large, small_session, dummy_action(1)];
        let params = ErasureParams::new(2, 3);
        actual.put_batch(&actions).unwrap();
        actual.put_shards_batch(&actions, params).unwrap();
        for action in &actions {
            reference.put(action).unwrap();
        }
        legacy_put_shards_batch(&reference, &actions, params).unwrap();
        for cf in [CF_NATIVE_PENDING, CF_NATIVE_SHARDS] {
            assert_eq!(actual.db.iterate_cf(cf, None).unwrap(), reference.db.iterate_cf(cf, None).unwrap());
        }
        drop(actual);
        let reopened = NativeDaStore::new(StateDb::open(dir.path()).unwrap());
        for action in &actions {
            let hash = compute_action_hash(action);
            assert_eq!(reopened.get_raw(&hash.0).unwrap().unwrap(), bincode::serialize(action).unwrap());
            for i in 0..params.n as u16 {
                let shard = reopened.get_shard(&hash.0, i).unwrap().unwrap();
                assert!(shard.verify());
                assert_eq!(encode_stored_shard(&shard).unwrap(),
                    encode_stored_shard(&reference.get_shard(&hash.0, i).unwrap().unwrap()).unwrap());
            }
        }
    }

    /// S391: `put` must wake a blocked `wait_for_arrival` immediately — the hot
    /// reconstruct path waits on this instead of sleeping fixed 20ms ticks, so
    /// a racing push resolves at arrival time rather than the next tick.
    #[test]
    fn wait_for_arrival_wakes_on_put() {
        let (_dir, store) = temp_store();
        let seen = NativeDaStore::arrival_generation();
        let writer = store.clone();
        let t = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(30));
            writer.put(&dummy_action(1)).expect("put");
        });
        let start = std::time::Instant::now();
        let new_gen =
            NativeDaStore::wait_for_arrival(seen, std::time::Duration::from_millis(2_000));
        t.join().unwrap();
        assert!(new_gen > seen, "a put must advance the arrival generation");
        assert!(
            start.elapsed() < std::time::Duration::from_millis(1_000),
            "waiter must wake on the arrival, not ride out the timeout (elapsed {:?})",
            start.elapsed()
        );
    }

    /// With no arrival the wait returns at the timeout. Tolerant to unrelated
    /// concurrent puts from parallel tests (asserts return, not stasis).
    #[test]
    fn wait_for_arrival_returns_on_timeout() {
        let seen = NativeDaStore::arrival_generation();
        let start = std::time::Instant::now();
        let new_gen = NativeDaStore::wait_for_arrival(seen, std::time::Duration::from_millis(30));
        assert!(new_gen >= seen, "generation never regresses");
        assert!(
            start.elapsed() < std::time::Duration::from_millis(1_500),
            "wait must return promptly after its timeout"
        );
    }

    /// T5: mirroring encodes each body into exactly `n` shards keyed `0..n`, and
    /// every stored shard verifies against its own committed root.
    #[test]
    fn mirror_encodes_and_persists_n_shards() {
        let (_dir, store) = temp_store();
        let action = dummy_action(1);
        let params = ErasureParams::new(2, 3);
        store
            .put_shards_batch(std::slice::from_ref(&action), params)
            .expect("put shards");
        let hash = compute_action_hash(&action);
        for i in 0..params.n as u16 {
            let s = store.get_shard(&hash.0, i).expect("get").expect("shard present");
            assert_eq!(s.shard_index, i);
            assert!(s.verify(), "stored shard {i} must verify against its root");
        }
        // No (n+1)th shard.
        assert!(store
            .get_shard(&hash.0, params.n as u16)
            .expect("get")
            .is_none());
    }

    /// T5: the shards read back out reconstruct the ORIGINAL body bytes, which
    /// re-hash to the action-hash used as the key (body-hash backstop identity).
    #[test]
    fn stored_shards_reconstruct_original_body() {
        use crate::erasure::reconstruct;
        let (_dir, store) = temp_store();
        let action = dummy_action(42);
        let params = ErasureParams::new(2, 3);
        store
            .put_shards_batch(std::slice::from_ref(&action), params)
            .expect("put shards");
        let hash = compute_action_hash(&action);
        let mut slots: Vec<Option<Vec<u8>>> = Vec::new();
        let mut body_len = 0usize;
        for i in 0..params.n as u16 {
            let s = store.get_shard(&hash.0, i).expect("get").expect("present");
            body_len = s.body_len as usize;
            slots.push(Some(s.shard_bytes));
        }
        let body = reconstruct(slots, params, body_len).expect("reconstruct");
        let original = bincode::serialize(&action).expect("serialize");
        assert_eq!(body, original, "reconstructed body must equal bincode(action)");
        let decoded: SignedNativeAction = bincode::deserialize(&body).expect("decode");
        assert_eq!(compute_action_hash(&decoded), hash, "backstop: re-hash matches key");
    }

    /// T5: shard encode is ADDITIVE — the whole-body store still serves the body
    /// after both mirrors run (no disturbance to the durability path).
    #[test]
    fn mirror_still_populates_whole_body_store() {
        let (_dir, store) = temp_store();
        let action = dummy_action(7);
        store
            .put_batch(std::slice::from_ref(&action))
            .expect("put_batch body");
        store
            .put_shards_batch(std::slice::from_ref(&action), ErasureParams::new(2, 3))
            .expect("put shards");
        let hash = compute_action_hash(&action);
        assert!(
            store.get(&hash).expect("get").is_some(),
            "whole body still present after shard encode"
        );
    }

    // ---- RH4 durable body GC ----

    const LIMITS: DaGcLimits = DaGcLimits {
        max_rows: 64,
        max_deletes: 1 << 20,
    };

    /// No `keep_from` bound: the window alone decides.
    const UNCAPPED: u64 = u64::MAX;

    fn present(store: &NativeDaStore, action: &SignedNativeAction) -> bool {
        store.contains(&compute_action_hash(action).0).unwrap()
    }

    fn bookkeeping_keys(store: &NativeDaStore) -> usize {
        use crate::StateBackend;
        store
            .db
            .iterate_cf(CF_NATIVE_PENDING, None)
            .unwrap()
            .iter()
            .filter(|(k, _)| k.len() != 32)
            .count()
    }

    /// A committed body survives `retention` dispatched heights past its commit
    /// height and goes on the pass where `commit height + retention == height`.
    #[test]
    fn gc_deletes_committed_bodies_after_the_window_not_before() {
        let (_dir, store) = temp_store();
        let (a, b) = (dummy_action(1), dummy_action(2));
        store.put_batch(&[a.clone(), b.clone()]).unwrap();
        let (ha, hb) = (compute_action_hash(&a), compute_action_hash(&b));
        // Retention 4: committed at 10 -> deletable from height 14.
        store
            .gc_committed(10, &[ha], 0, 4, UNCAPPED, LIMITS)
            .unwrap();
        store
            .gc_committed(11, &[hb], 0, 4, UNCAPPED, LIMITS)
            .unwrap();
        for height in 12..=13 {
            assert_eq!(
                store
                    .gc_committed(height, &[], 0, 4, UNCAPPED, LIMITS)
                    .unwrap(),
                DaGcOutcome::default()
            );
        }
        assert!(
            present(&store, &a) && present(&store, &b),
            "both inside the window at 13"
        );

        let out = store.gc_committed(14, &[], 0, 4, UNCAPPED, LIMITS).unwrap();
        assert_eq!(
            (out.rows_deleted, out.bodies_deleted, out.backlog),
            (1, 1, false)
        );
        assert!(!present(&store, &a), "height 10 aged out at 14");
        assert!(
            present(&store, &b),
            "height 11 is still inside the window at 14"
        );

        let out = store.gc_committed(15, &[], 0, 4, UNCAPPED, LIMITS).unwrap();
        assert_eq!((out.rows_deleted, out.bodies_deleted), (1, 1));
        assert!(!present(&store, &b), "height 11 aged out at 15");
        assert_eq!(
            store.gc_committed(16, &[], 0, 4, UNCAPPED, LIMITS).unwrap(),
            DaGcOutcome::default()
        );
        // Every row went with its bodies; only the frontier key remains.
        assert_eq!(bookkeeping_keys(&store), 1);
    }

    /// The July RH4 windowing bug (memory a70717bb): `cutoff = tip -
    /// retention` (saturating) with `h <= cutoff` deleted the height-0 body
    /// while `tip < retention`. Nothing may go before `h + retention <= tip`.
    #[test]
    fn gc_never_deletes_inside_the_window_near_genesis() {
        let (_dir, store) = temp_store();
        let genesis = dummy_action(7);
        store.put(&genesis).unwrap();
        store
            .gc_committed(0, &[compute_action_hash(&genesis)], 0, 4, UNCAPPED, LIMITS)
            .unwrap();
        for tip in 1..4 {
            let out = store
                .gc_committed(tip, &[], 0, 4, UNCAPPED, LIMITS)
                .unwrap();
            assert_eq!(out.bodies_deleted, 0, "tip {tip} < retention 4");
            assert!(
                present(&store, &genesis),
                "height-0 body must survive tip {tip}"
            );
        }
        assert_eq!(
            store
                .gc_committed(4, &[], 0, 4, UNCAPPED, LIMITS)
                .unwrap()
                .bodies_deleted,
            1
        );
        assert!(!present(&store, &genesis));
    }

    /// Only bodies a committed row lists are deleted. A body no committed
    /// block references (expired, or a losing proposal's) stays: pool expiry
    /// is local-clock, a skewed peer may still include it.
    #[test]
    fn gc_keeps_uncommitted_bodies() {
        let (_dir, store) = temp_store();
        let (committed, uncommitted) = (dummy_action(1), dummy_action(2));
        store
            .put_batch(&[committed.clone(), uncommitted.clone()])
            .unwrap();
        store
            .gc_committed(
                1,
                &[compute_action_hash(&committed)],
                0,
                2,
                UNCAPPED,
                LIMITS,
            )
            .unwrap();
        for height in 2..200 {
            store
                .gc_committed(height, &[], 0, 2, UNCAPPED, LIMITS)
                .unwrap();
        }
        assert!(!present(&store, &committed));
        assert!(
            present(&store, &uncommitted),
            "an uncommitted body is never GC'd"
        );
    }

    /// The shards of a GC'd body go with it (every index below the row's
    /// shard count); the shards of an uncommitted body stay.
    #[test]
    fn gc_deletes_the_shards_of_gcd_bodies() {
        let (_dir, store) = temp_store();
        let params = ErasureParams::new(2, 3);
        let (c, u) = (dummy_action(1), dummy_action(2));
        store.put_batch(&[c.clone(), u.clone()]).unwrap();
        store
            .put_shards_batch(&[c.clone(), u.clone()], params)
            .unwrap();
        let (hc, hu) = (compute_action_hash(&c), compute_action_hash(&u));
        store
            .gc_committed(5, &[hc], params.n as u16, 1, UNCAPPED, LIMITS)
            .unwrap();
        let out = store.gc_committed(6, &[], 0, 1, UNCAPPED, LIMITS).unwrap();
        assert_eq!((out.bodies_deleted, out.shard_keys_deleted), (1, 3));
        for i in 0..params.n as u16 {
            assert!(
                store.get_shard(&hc.0, i).unwrap().is_none(),
                "shard {i} of the GC'd body"
            );
            assert!(
                store.get_shard(&hu.0, i).unwrap().is_some(),
                "shard {i} of the uncommitted body"
            );
        }
    }

    /// One pass deletes at most `max_rows` rows and `max_deletes` operations
    /// (the first row whole), reports the backlog, and later passes drain it.
    #[test]
    fn gc_pass_is_bounded_and_drains_the_backlog() {
        let (_dir, store) = temp_store();
        let actions: Vec<_> = (1..=30).map(dummy_action).collect();
        store.put_batch(&actions).unwrap();
        // Heights 100..=109, three bodies each, recorded under a wide window...
        for (i, chunk) in actions.chunks(3).enumerate() {
            let hashes: Vec<_> = chunk.iter().map(compute_action_hash).collect();
            store
                .gc_committed(100 + i as u64, &hashes, 0, 1000, UNCAPPED, LIMITS)
                .unwrap();
        }
        assert!(actions.iter().all(|a| present(&store, a)));
        // ...then a restart with retention 1: all ten rows are due at once.
        let ops = DaGcLimits {
            max_rows: 8,
            max_deletes: 7,
        };
        let out = store.gc_committed(110, &[], 0, 1, UNCAPPED, ops).unwrap();
        assert_eq!(
            (out.rows_deleted, out.bodies_deleted, out.backlog),
            (2, 6, true)
        );
        let rows = DaGcLimits {
            max_rows: 3,
            max_deletes: 1000,
        };
        let out = store.gc_committed(111, &[], 0, 1, UNCAPPED, rows).unwrap();
        assert_eq!((out.rows_deleted, out.backlog), (3, true));
        let tiny = DaGcLimits {
            max_rows: 8,
            max_deletes: 1,
        };
        let out = store.gc_committed(112, &[], 0, 1, UNCAPPED, tiny).unwrap();
        assert_eq!(
            (out.rows_deleted, out.bodies_deleted, out.backlog),
            (1, 3, true),
            "a row above max_deletes is still taken whole"
        );
        let mut deleted = 2 + 3 + 1;
        for tip in 113.. {
            let out = store.gc_committed(tip, &[], 0, 1, UNCAPPED, rows).unwrap();
            deleted += out.rows_deleted;
            if !out.backlog {
                break;
            }
        }
        assert_eq!(deleted, 10);
        assert!(actions.iter().all(|a| !present(&store, a)));
    }

    /// Rows are durable: a restart between the record and the GC leaves no
    /// residue, and re-dispatching a height after it is harmless.
    #[test]
    fn gc_rows_survive_a_restart() {
        let (dir, store) = temp_store();
        let a = dummy_action(1);
        store.put(&a).unwrap();
        let ha = compute_action_hash(&a);
        store
            .gc_committed(20, &[ha], 0, 8, UNCAPPED, LIMITS)
            .unwrap();
        drop(store);
        let reopened = NativeDaStore::new(StateDb::open(dir.path()).unwrap());
        assert!(present(&reopened, &a));
        reopened
            .gc_committed(20, &[ha], 0, 8, UNCAPPED, LIMITS)
            .unwrap();
        // Every height dispatches (a skipped one is a gap in the bookkeeping).
        for height in 21..28 {
            reopened
                .gc_committed(height, &[], 0, 8, UNCAPPED, LIMITS)
                .unwrap();
        }
        assert_eq!(
            reopened
                .gc_committed(28, &[], 0, 8, UNCAPPED, LIMITS)
                .unwrap()
                .bodies_deleted,
            1
        );
        assert!(!present(&reopened, &a));
    }

    /// The GC rows share `CF_NATIVE_PENDING` with the bodies but can never be
    /// mistaken for one: body keys are exactly 32 bytes, row keys are not.
    #[test]
    fn gc_bookkeeping_keys_are_never_body_keys() {
        assert_ne!(DA_GC_ROW_KEY_LEN, 32);
        assert_ne!(DA_GC_STATE_KEY.len(), 32);
        assert_ne!(DA_GC_REF_KEY_LEN, 32);
        assert_ne!(DA_GC_MOVED_KEY_LEN, 32);
        let (_dir, store) = temp_store();
        let a = dummy_action(1);
        store.put(&a).unwrap();
        store
            .gc_committed(3, &[compute_action_hash(&a)], 0, 9, UNCAPPED, LIMITS)
            .unwrap();
        assert_eq!(
            bookkeeping_keys(&store),
            3,
            "the row of height 3, its hash's reference, the GC state"
        );
        use crate::StateBackend;
        for (key, value) in store.db.iterate_cf(CF_NATIVE_PENDING, None).unwrap() {
            if key.len() == 32 {
                assert!(bincode::deserialize::<SignedNativeAction>(&value).is_ok());
            }
        }
        // A native-empty height records nothing.
        store.gc_committed(4, &[], 0, 9, UNCAPPED, LIMITS).unwrap();
        assert_eq!(bookkeeping_keys(&store), 3);
    }

    /// Codex P1 (finding 1): `keep_from` bounds the window. A row at or above
    /// it stays however old, goes on the first pass after the bound passes
    /// it, and the frontier never skips a row the bound held back.
    #[test]
    fn gc_never_deletes_at_or_above_keep_from() {
        let (_dir, store) = temp_store();
        let actions: Vec<_> = (1..=3).map(dummy_action).collect();
        store.put_batch(&actions).unwrap();
        for (i, a) in actions.iter().enumerate() {
            let h = 10 + i as u64;
            store
                .gc_committed(h, &[compute_action_hash(a)], 0, 1, 0, LIMITS)
                .unwrap();
        }
        assert!(actions.iter().all(|a| present(&store, a)), "keep_from 0");
        for h in 13..100 {
            store.gc_committed(h, &[], 0, 1, 0, LIMITS).unwrap();
        }
        // Every row aged out of window 1 by 100; keep_from 11 frees height 10.
        let out = store.gc_committed(100, &[], 0, 1, 11, LIMITS).unwrap();
        assert_eq!((out.rows_deleted, out.backlog), (1, false));
        assert!(!present(&store, &actions[0]));
        assert!(present(&store, &actions[1]) && present(&store, &actions[2]));
        assert_eq!(
            store.gc_committed(101, &[], 0, 1, 0, LIMITS).unwrap(),
            DaGcOutcome::default(),
            "keep_from 0 deletes nothing"
        );
        let out = store.gc_committed(102, &[], 0, 1, 13, LIMITS).unwrap();
        assert_eq!(out.rows_deleted, 2, "the bound passed 11 and 12");
        assert!(actions.iter().all(|a| !present(&store, a)));
        assert_eq!(bookkeeping_keys(&store), 1);
    }

    /// Codex P1 (finding 2): a hash two committed heights reference (a
    /// re-inclusion) keeps its body and shards until the LATER reference
    /// expires; the earlier row expiring must not take them.
    #[test]
    fn gc_keeps_a_body_until_its_latest_reference_expires() {
        const W: u64 = 1128;
        let (_dir, store) = temp_store();
        let params = ErasureParams::new(2, 3);
        let n = params.n as u16;
        let a = dummy_action(1);
        let ha = compute_action_hash(&a);
        store.put(&a).unwrap();
        store
            .put_shards_batch(std::slice::from_ref(&a), params)
            .unwrap();
        for tip in 100..=300 + W {
            let hashes: &[B256] = if tip == 100 || tip == 300 { &[ha] } else { &[] };
            store
                .gc_committed(tip, hashes, n, W, UNCAPPED, LIMITS)
                .unwrap();
            let live = tip < 300 + W;
            assert_eq!(present(&store, &a), live, "body at tip {tip}");
            for i in 0..n {
                assert_eq!(
                    store.get_shard(&ha.0, i).unwrap().is_some(),
                    live,
                    "shard {i} at tip {tip}"
                );
            }
        }
        // No residue: the rows (and any per-hash records) went with the body.
        assert_eq!(bookkeeping_keys(&store), 1, "only the GC state key remains");
    }

    /// Finding 2 across a restart: the later reference is durable, and a
    /// replayed (at-least-once) dispatch of the earlier height after the
    /// restart does not re-attach the body to it.
    #[test]
    fn gc_latest_reference_survives_a_restart_and_a_replayed_dispatch() {
        const W: u64 = 16;
        let (dir, store) = temp_store();
        let a = dummy_action(1);
        let ha = compute_action_hash(&a);
        store.put(&a).unwrap();
        for tip in 10..=20 {
            let hashes: &[B256] = if tip == 10 || tip == 20 { &[ha] } else { &[] };
            store
                .gc_committed(tip, hashes, 0, W, UNCAPPED, LIMITS)
                .unwrap();
        }
        drop(store);
        let store = NativeDaStore::new(StateDb::open(dir.path()).unwrap());
        // Boot replay re-dispatches 10..=20, then dispatch goes on.
        for tip in (10..=20).chain(21..20 + W) {
            let hashes: &[B256] = if tip == 10 || tip == 20 { &[ha] } else { &[] };
            store
                .gc_committed(tip, hashes, 0, W, UNCAPPED, LIMITS)
                .unwrap();
            assert!(
                present(&store, &a),
                "tip {tip}: height 20 still references it"
            );
        }
        store
            .gc_committed(20 + W, &[], 0, W, UNCAPPED, LIMITS)
            .unwrap();
        assert!(!present(&store, &a), "the latest reference (20) expired");
        assert_eq!(bookkeeping_keys(&store), 1);
    }

    /// A hash re-included twice (once twice in one block), the last time in
    /// the very pass that ages its previous row out: it stays until its last
    /// reference expires, while the other bodies of the earlier rows go on
    /// time, and no reference or row is left behind.
    #[test]
    fn gc_moves_a_reference_through_several_reinclusions() {
        const W: u64 = 4;
        let (_dir, store) = temp_store();
        let x = dummy_action(1);
        let others: Vec<_> = (2..=4).map(dummy_action).collect();
        store.put(&x).unwrap();
        store.put_batch(&others).unwrap();
        let hx = compute_action_hash(&x);
        let ho: Vec<_> = others.iter().map(compute_action_hash).collect();
        // 16 = 12 + W: row 12 ages out in the pass that records 16.
        let rows = [
            (10, vec![ho[0], hx]),
            (12, vec![hx, ho[1], hx]),
            (16, vec![ho[2], hx]),
        ];
        for tip in 10..=20u64 {
            let hashes = rows
                .iter()
                .find(|(h, _)| *h == tip)
                .map(|(_, v)| v.clone())
                .unwrap_or_default();
            let out = store
                .gc_committed(tip, &hashes, 0, W, UNCAPPED, LIMITS)
                .unwrap();
            assert_eq!(
                out.references_moved > 0,
                tip == 12 || tip == 16,
                "tip {tip}"
            );
            assert_eq!(present(&store, &others[0]), tip < 14, "tip {tip}");
            assert_eq!(present(&store, &others[1]), tip < 16, "tip {tip}");
            assert_eq!(present(&store, &others[2]), tip < 20, "tip {tip}");
            assert_eq!(present(&store, &x), tip < 20, "tip {tip}: x (last at 16)");
        }
        assert_eq!(bookkeeping_keys(&store), 1, "only the GC state key remains");
    }

    /// Codex P2 (finding 1): re-including a whole earlier block (16,000
    /// hashes, a manifest the default message cap admits) runs on the
    /// consensus thread, so moving the hashes out of the earlier row, and
    /// skipping them when that row ages out in the same pass, must stay
    /// linear. Self-calibrated against recording the same hashes fresh (the
    /// same `MultiGet`, row and references): the linear pass costs 2-3x
    /// that (a marker per moved hash), the quadratic membership scans
    /// (16,000^2 hash compares, twice) over 20x (debug build; 8x splits
    /// them). Three tries, so one scheduling stall cannot fail it.
    #[test]
    fn gc_reinclusion_of_a_large_block_stays_linear() {
        const N: u64 = 16_000;
        const W: u64 = 4;
        let hashes: Vec<B256> = (0..N)
            .map(|i| alloy_primitives::keccak256(i.to_be_bytes()))
            .collect();
        let mut ratios = Vec::new();
        for _ in 0..3 {
            let (_dir, store) = temp_store();
            let started = std::time::Instant::now();
            store
                .gc_committed(10, &hashes, 0, W, UNCAPPED, LIMITS)
                .unwrap();
            let fresh = started.elapsed();
            for h in 11..10 + W {
                store.gc_committed(h, &[], 0, W, UNCAPPED, LIMITS).unwrap();
            }
            // 10 + W: this pass re-includes every hash of row 10 AND ages it out.
            let started = std::time::Instant::now();
            let out = store
                .gc_committed(10 + W, &hashes, 0, W, UNCAPPED, LIMITS)
                .unwrap();
            let reinclude = started.elapsed();
            assert_eq!(
                (out.references_moved, out.rows_deleted, out.bodies_deleted),
                (N, 1, 0)
            );
            let ratio = reinclude.as_secs_f64() / fresh.as_secs_f64().max(1e-6);
            if ratio < 8.0 {
                return;
            }
            ratios.push(ratio);
        }
        panic!("re-including {N} hashes cost {ratios:.1?} x a fresh record: not linear");
    }

    /// The shard indices of `hash` present among the first `upto`.
    fn shard_indices(store: &NativeDaStore, hash: &B256, upto: u16) -> Vec<u16> {
        (0..upto)
            .filter(|&i| store.get_shard(&hash.0, i).unwrap().is_some())
            .collect()
    }

    /// Codex P2 (finding 2): a hash re-included after a validator-set shrink
    /// moves from a 7-shard row to a 4-shard one. Its shards 4..7 (custodied
    /// under the larger set) must still go at its latest expiry, across
    /// restarts: the later row keeps the larger count. Both moves: `a` while
    /// the earlier row is pending, `c` in the very pass that ages it out;
    /// the earlier row's other hash `b` goes on time with all 7 shards.
    #[test]
    fn gc_reinclusion_after_a_set_shrink_deletes_every_shard() {
        const W: u64 = 4;
        let (wide, narrow) = (ErasureParams::new(3, 7), ErasureParams::new(2, 4));
        let (dir, mut store) = temp_store();
        let actions: Vec<_> = (1..=3).map(dummy_action).collect();
        let [ha, hb, hc] = [0, 1, 2].map(|i| compute_action_hash(&actions[i]));
        store.put_batch(&actions).unwrap();
        store.put_shards_batch(&actions, wide).unwrap();
        // (action index, latest reference height)
        let latest = [(0, 12), (1, 10), (2, 14)];
        for tip in 10..=20u64 {
            // The re-including block's custody runs under the shrunk set.
            let (hashes, n) = match tip {
                10 => (vec![ha, hb, hc], wide.n as u16),
                12 => (vec![ha], narrow.n as u16),
                14 => (vec![hc], narrow.n as u16),
                _ => (vec![], narrow.n as u16),
            };
            if tip == 12 || tip == 14 {
                let i = if tip == 12 { 0 } else { 2 };
                store
                    .put_shards_batch(std::slice::from_ref(&actions[i]), narrow)
                    .unwrap();
            }
            store
                .gc_committed(tip, &hashes, n, W, UNCAPPED, LIMITS)
                .unwrap();
            if tip == 13 {
                drop(store);
                store = NativeDaStore::new(StateDb::open(dir.path()).unwrap());
            }
            for (i, last) in latest {
                let live = tip < last + W;
                let hash = compute_action_hash(&actions[i]);
                assert_eq!(present(&store, &actions[i]), live, "body {i} at tip {tip}");
                let expected: Vec<u16> = if live { (0..7).collect() } else { vec![] };
                assert_eq!(
                    shard_indices(&store, &hash, 7),
                    expected,
                    "shards of body {i} at tip {tip}"
                );
            }
        }
        drop(store);
        let store = NativeDaStore::new(StateDb::open(dir.path()).unwrap());
        for hash in [ha, hb, hc] {
            assert_eq!(shard_indices(&store, &hash, 7), Vec::<u16>::new());
        }
        assert_eq!(bookkeeping_keys(&store), 1, "only the GC state key remains");
    }

    /// Codex P2 sibling (finding 1's class): a block that re-includes one
    /// hash from each of many pending rows (a Byzantine proposer can build
    /// one) must cost what recording its own hashes costs, not what reading
    /// and rewriting every row they came from costs. 64 rows of 2,000 hashes,
    /// one re-included from each, self-calibrated against the mean cost of
    /// recording one hash in the setup: the re-inclusion costs under 2x
    /// that per re-included hash, rewriting the 64 rows (128,000 hashes) over
    /// 100 times (debug build; 15x splits them). Then the re-included bodies
    /// stay until their latest reference expires and nothing is left behind.
    #[test]
    fn gc_reinclusion_across_many_rows_stays_linear() {
        const ROWS: u64 = 64;
        const PER_ROW: u64 = 2_000;
        const W: u64 = 4;
        let actions: Vec<_> = (0..ROWS).map(dummy_action).collect();
        let reincluded: Vec<B256> = actions.iter().map(compute_action_hash).collect();
        let filler = |row: u64, i: u64| {
            let mut seed = [0u8; 16];
            seed[..8].copy_from_slice(&row.to_be_bytes());
            seed[8..].copy_from_slice(&i.to_be_bytes());
            alloy_primitives::keccak256(seed)
        };
        let mut ratios = Vec::new();
        for _ in 0..3 {
            let (_dir, store) = temp_store();
            store.put_batch(&actions).unwrap();
            // Heights 10..10 + ROWS; keep_from 0, so nothing is due yet.
            let started = std::time::Instant::now();
            for row in 0..ROWS {
                let mut hashes: Vec<B256> = (1..PER_ROW).map(|i| filler(row, i)).collect();
                hashes.push(reincluded[row as usize]);
                store
                    .gc_committed(10 + row, &hashes, 0, W, 0, LIMITS)
                    .unwrap();
            }
            let per_hash = started.elapsed().as_secs_f64() / (ROWS * PER_ROW) as f64;
            let at = 10 + ROWS;
            let started = std::time::Instant::now();
            let out = store
                .gc_committed(at, &reincluded, 0, W, 0, LIMITS)
                .unwrap();
            let ratio = started.elapsed().as_secs_f64() / (per_hash * ROWS as f64).max(1e-9);
            assert_eq!(out.references_moved, ROWS);
            if ratio >= 15.0 {
                ratios.push(ratio);
                continue;
            }
            // Every earlier row ages out on the first pass; the re-included
            // bodies go with row `at`.
            for tip in at + 1..=at + W {
                store
                    .gc_committed(tip, &[], 0, W, UNCAPPED, LIMITS)
                    .unwrap();
                for (i, a) in actions.iter().enumerate() {
                    assert_eq!(present(&store, a), tip < at + W, "body {i} at tip {tip}");
                }
            }
            assert_eq!(bookkeeping_keys(&store), 1, "only the GC state key remains");
            return;
        }
        panic!("re-including {ROWS} hashes from {ROWS} rows cost {ratios:.1?} x recording them");
    }

    /// A test [`DaGcHistory`]: `known` heights, every height below `pruned`
    /// pruned, every other height `empty` (known, no actions) or unknown;
    /// `fail` = every read errors. `tip`: its committed tip (`None` = none
    /// known), `tip_fail` = reading the tip errors. `tail`: the hashes its
    /// uncommitted blocks reference, `tail_fail` = reading them errors.
    #[derive(Default)]
    struct TestHistory {
        pruned: u64,
        known: std::collections::BTreeMap<u64, Vec<B256>>,
        empty: bool,
        fail: bool,
        tip: Option<u64>,
        tip_fail: bool,
        tail: Vec<B256>,
        tail_fail: bool,
        /// How long reading the tail takes (slow-pass timing tests).
        tail_delay: std::time::Duration,
        /// Bodies its fence drops from a pass's deletes.
        fenced: Vec<B256>,
        /// `begin_delete` / `end_delete` calls.
        begun: std::cell::Cell<u32>,
        ended: std::cell::Cell<u32>,
        /// Its fence is over its cap.
        full: bool,
    }

    impl DaGcHistory for TestHistory {
        fn pruned_below(&self) -> Result<u64, StateError> {
            if self.fail {
                return Err(StateError::InvalidData("test history read".into()));
            }
            Ok(self.pruned)
        }

        fn committed_hashes(&self, height: u64) -> Result<Option<Vec<B256>>, StateError> {
            if self.fail {
                return Err(StateError::InvalidData("test history read".into()));
            }
            assert!(height >= self.pruned, "pruned height {height} read");
            Ok(match self.known.get(&height) {
                Some(hashes) => Some(hashes.clone()),
                None if self.empty => Some(vec![]),
                None => None,
            })
        }

        fn committed_through(&self) -> Result<Option<u64>, StateError> {
            if self.fail || self.tip_fail {
                return Err(StateError::InvalidData("test history tip read".into()));
            }
            Ok(self.tip)
        }

        fn uncommitted_hashes(&self) -> Result<Vec<B256>, StateError> {
            std::thread::sleep(self.tail_delay);
            if self.fail || self.tail_fail {
                return Err(StateError::InvalidData("test history tail read".into()));
            }
            Ok(self.tail.clone())
        }

        fn begin_delete(&self, bodies: &mut Vec<([u8; 32], u16)>) -> u64 {
            assert_eq!(self.begun.get(), self.ended.get(), "begin without end");
            self.begun.set(self.begun.get() + 1);
            let before = bodies.len();
            bodies.retain(|(h, _)| !self.fenced.iter().any(|f| f.0 == *h));
            (before - bodies.len()) as u64
        }

        fn end_delete(&self) {
            self.ended.set(self.ended.get() + 1);
            assert_eq!(self.begun.get(), self.ended.get(), "end without begin");
        }

        fn fence_full(&self) -> bool {
            self.full
        }
    }

    /// RH4 GC worker (Codex adversarial, s132): a full fence cannot say which
    /// bodies a concurrent check is about to read, so the pass HOLDS
    /// (`fence_full`): no row, no body goes, the frontier stays, the fence
    /// is not even asked. The next pass after the fence drained deletes them.
    #[test]
    fn gc_holds_while_the_fence_is_full() {
        const W: u64 = 4;
        let (_dir, store) = temp_store();
        let (x, y) = (dummy_action(1), dummy_action(2));
        let (hx, hy) = (compute_action_hash(&x), compute_action_hash(&y));
        store.put_batch(&[x.clone(), y.clone()]).unwrap();
        let mut hist = history(&[(10, vec![hx, hy])]);
        for h in 10..=13 {
            let hashes = hist.known.get(&h).cloned().unwrap_or_default();
            gc_h(&store, h, &hashes, W, &hist);
        }
        hist.full = true;
        let out = gc_h(&store, 14, &[], W, &hist);
        assert_eq!(
            (out.rows_deleted, out.bodies_deleted, out.hold_reason, out.deletion_held),
            (0, 0, Some(DaGcHoldReason::FenceFull), true)
        );
        assert_eq!(hist.begun.get(), 0, "held before the fence");
        assert!(present(&store, &x) && present(&store, &y));
        hist.full = false;
        let out = gc_h(&store, 15, &[], W, &hist);
        assert_eq!((out.rows_deleted, out.bodies_deleted, out.hold_reason), (1, 2, None));
        assert!(!present(&store, &x) && !present(&store, &y));
    }

    /// A history whose body checks go through a real [`DaGcFence`].
    struct FenceHistory<'a>(&'a crate::DaGcFence);

    impl DaGcHistory for FenceHistory<'_> {
        fn pruned_below(&self) -> Result<u64, StateError> {
            Ok(0)
        }
        fn committed_hashes(&self, _: u64) -> Result<Option<Vec<B256>>, StateError> {
            Ok(Some(vec![]))
        }
        fn committed_through(&self) -> Result<Option<u64>, StateError> {
            Ok(None)
        }
        fn uncommitted_hashes(&self) -> Result<Vec<B256>, StateError> {
            Ok(vec![])
        }
        fn begin_delete(&self, bodies: &mut Vec<([u8; 32], u16)>) -> u64 {
            self.0.begin_delete(bodies)
        }
        fn end_delete(&self) {
            self.0.end_delete()
        }
        fn fence_full(&self) -> bool {
            self.0.is_full()
        }
    }

    /// Codex adversarial round 2 (s132, `/tmp/rh4-round2-store-proof`, real
    /// GC and RocksDB): once the fence saturated, honest pipelined checks at
    /// H+1 kept every pass held ("pins=0 full=true held_passes=1000
    /// body_present=true"). The fence resumes tracking pins as soon as it
    /// has room, so the hold ends once the worker passed the refused height
    /// and x (due at 14, never pinned) goes. Cap 2: this schedule needs two
    /// pins at a time (the block at H until pass H, the proposal at H+1).
    #[test]
    fn gc_resumes_after_the_fence_saturated_under_honest_pipelining() {
        const W: u64 = 4;
        let (_dir, store) = temp_store();
        let x = dummy_action(1);
        let hx = compute_action_hash(&x);
        store.put(&x).unwrap();
        let fence = crate::DaGcFence::with_max_pins(2);
        fence.set_enabled(true);
        fence.note_committed(13);
        let hist = FenceHistory(&fence);
        let gc = |height: u64, hashes: &[B256]| {
            store
                .gc_committed_with_history(height, Some(hashes), 0, W, UNCAPPED, LIMITS, &hist)
                .unwrap()
        };
        gc(10, &[hx]);
        for h in 11..=13 {
            gc(h, &[]);
        }
        let n = |i: u64| B256::from(alloy_primitives::U256::from(i));
        assert!(fence.pin(15, &[n(1), n(2), n(3)]).is_empty());
        let mut held = Vec::new();
        for h in 14..=1013u64 {
            fence.note_committed(h);
            assert!(fence.pin(h + 1, &[n(h + 10)]).is_empty());
            let out = gc(h, &[]);
            if out.hold_reason == Some(DaGcHoldReason::FenceFull) {
                held.push(h);
            }
            assert_eq!(store.contains(&hx.0).unwrap(), h < 17, "x at {h}");
            fence.release_through(h);
        }
        assert_eq!(held, vec![14, 15, 16], "held through refusals only");
        assert_eq!((fence.pinned(), fence.is_full()), (1, false));
    }

    /// RH4 GC worker: right before its write a pass hands its due bodies to
    /// the history's fence, which drops the ones a concurrent check pinned:
    /// the row goes, those bodies (and their references) stay, counted as
    /// `fence_pinned`; the others go. `end_delete` follows once the write
    /// returned, also when it failed; a pass that deletes no body calls
    /// neither. A kept body follows its later re-inclusion's window.
    #[test]
    fn gc_keeps_the_bodies_the_fence_drops() {
        const W: u64 = 4;
        let (dir, store) = temp_store();
        let (x, y) = (dummy_action(1), dummy_action(2));
        let (hx, hy) = (compute_action_hash(&x), compute_action_hash(&y));
        store.put_batch(&[x.clone(), y.clone()]).unwrap();
        let mut hist = history(&[(10, vec![hx, hy])]);
        for h in 10..=13 {
            let hashes = hist.known.get(&h).cloned().unwrap_or_default();
            gc_h(&store, h, &hashes, W, &hist);
        }
        assert_eq!(hist.begun.get(), 0, "nothing due, no fence call");
        // The failed write of the due pass still ends the fence.
        drop(store);
        {
            let ro = NativeDaStore::new(StateDb::open_read_only(dir.path()).unwrap());
            assert!(ro
                .gc_committed_with_history(14, Some(&[]), 0, W, UNCAPPED, LIMITS, &hist)
                .is_err());
            assert_eq!((hist.begun.get(), hist.ended.get()), (1, 1));
        }
        let store = NativeDaStore::new(StateDb::open(dir.path()).unwrap());
        hist.fenced = vec![hx];
        let out = gc_h(&store, 14, &[], W, &hist);
        assert_eq!(
            (out.rows_deleted, out.bodies_deleted, out.fence_pinned, out.hold_reason),
            (1, 1, 1, None)
        );
        assert_eq!((hist.begun.get(), hist.ended.get()), (2, 2));
        assert!(present(&store, &x), "fenced: stays");
        assert!(!present(&store, &y), "not fenced: goes");
        // The pinning block (16) commits: its record owns x for its window.
        hist.fenced.clear();
        for tip in 15..=16 + W {
            let hashes = if tip == 16 { vec![hx] } else { vec![] };
            gc_h(&store, tip, &hashes, W, &hist);
            assert_eq!(present(&store, &x), tip < 16 + W, "x (pinned by 16) at {tip}");
        }
        assert_eq!(bookkeeping_keys(&store), 1, "only the GC state key remains");
    }

    fn history(rows: &[(u64, Vec<B256>)]) -> TestHistory {
        TestHistory {
            known: rows.iter().cloned().collect(),
            empty: true,
            ..TestHistory::default()
        }
    }

    fn gc_h(
        store: &NativeDaStore,
        height: u64,
        hashes: &[B256],
        retention: u64,
        history: &TestHistory,
    ) -> DaGcOutcome {
        store
            .gc_committed_with_history(
                height,
                Some(hashes),
                0,
                retention,
                UNCAPPED,
                LIMITS,
                history,
            )
            .unwrap()
    }

    /// Every key and value of `CF_NATIVE_PENDING` (bodies and bookkeeping).
    fn dump(store: &NativeDaStore) -> Vec<(Vec<u8>, Vec<u8>)> {
        use crate::StateBackend;
        store.db.iterate_cf(CF_NATIVE_PENDING, None).unwrap()
    }

    /// Codex P1 (pre-merge review, failed record): the write that records a
    /// re-inclusion fails. It is not dropped: the next pass back-fills it
    /// from the history before deleting anything, so the body stays until
    /// the re-including height's window ends while the earlier row's other
    /// body still goes on time.
    #[test]
    fn gc_failed_record_write_is_backfilled_before_any_deletion() {
        const W: u64 = 8;
        let (dir, store) = temp_store();
        let (x, y) = (dummy_action(1), dummy_action(2));
        let (hx, hy) = (compute_action_hash(&x), compute_action_hash(&y));
        store.put_batch(&[x.clone(), y.clone()]).unwrap();
        let hist = history(&[(10, vec![hx, hy]), (13, vec![hx])]);
        for h in 10..=12 {
            let hashes = hist.known.get(&h).cloned().unwrap_or_default();
            gc_h(&store, h, &hashes, W, &hist);
        }
        drop(store);
        // The record of height 13 fails: the DB rejects the write.
        {
            let ro = NativeDaStore::new(StateDb::open_read_only(dir.path()).unwrap());
            assert!(ro
                .gc_committed_with_history(13, Some(&[hx]), 0, W, UNCAPPED, LIMITS, &hist)
                .is_err());
        }
        let store = NativeDaStore::new(StateDb::open(dir.path()).unwrap());
        for tip in 14..=13 + W {
            let out = gc_h(&store, tip, &[], W, &hist);
            if tip == 14 {
                assert_eq!(
                    out.heights_backfilled, 1,
                    "height 13's record lands on retry"
                );
            }
            assert_eq!(present(&store, &y), tip < 10 + W, "y (row 10) at tip {tip}");
            assert_eq!(
                present(&store, &x),
                tip < 13 + W,
                "x (re-included at 13) at tip {tip}"
            );
        }
        assert_eq!(bookkeeping_keys(&store), 1, "only the GC state key remains");
    }

    /// A gap the history cannot fill (a missing block, a read error): the
    /// pass deletes nothing, however due the rows are, and says so; once the
    /// history supplies the heights the deletions resume.
    #[test]
    fn gc_unfillable_gap_holds_every_deletion_until_filled() {
        const W: u64 = 4;
        let (_dir, store) = temp_store();
        let x = dummy_action(1);
        let hx = compute_action_hash(&x);
        store.put(&x).unwrap();
        let full = history(&[(10, vec![hx])]);
        for h in 10..=12 {
            let hashes = full.known.get(&h).cloned().unwrap_or_default();
            gc_h(&store, h, &hashes, W, &full);
        }
        // 13..=19 never recorded; the history does not have them.
        let missing = TestHistory {
            known: full.known.clone(),
            ..TestHistory::default()
        };
        let out = gc_h(&store, 20, &[], W, &missing);
        assert!(
            out.deletion_held,
            "row 10 is due at 20 but 13..=19 are unknown"
        );
        assert_eq!(out.rows_deleted, 0);
        assert!(present(&store, &x));
        let broken = TestHistory {
            fail: true,
            ..TestHistory::default()
        };
        let out = gc_h(&store, 21, &[], W, &broken);
        assert!(out.deletion_held, "a history read error holds too");
        assert!(present(&store, &x));
        // Filled: 13..=21 back-filled (20 and 21 included: their own passes
        // recorded nothing), then row 10 goes.
        let out = gc_h(&store, 22, &[], W, &full);
        assert_eq!((out.heights_backfilled, out.deletion_held), (9, false));
        assert_eq!(out.rows_deleted, 1);
        assert!(!present(&store, &x));
    }

    /// Codex P1 (round 2, app.rs state-body write): a height whose record
    /// is unknown (dispatch could not make its body durable) is never
    /// recorded as "no actions". The pass leaves the cursor AT it and deletes
    /// nothing, although row 10 is due; the next pass back-fills it from the
    /// history, re-inclusion pin included, so x outlives row 10 until the
    /// re-including height's window ends.
    #[test]
    fn gc_unknown_record_leaves_the_cursor_behind_and_holds_deletion() {
        const W: u64 = 4;
        let (_dir, store) = temp_store();
        let x = dummy_action(1);
        let hx = compute_action_hash(&x);
        store.put(&x).unwrap();
        let hist = history(&[(10, vec![hx]), (14, vec![hx])]);
        for h in 10..=13 {
            let hashes = hist.known.get(&h).cloned().unwrap_or_default();
            gc_h(&store, h, &hashes, W, &hist);
        }
        let out = store
            .gc_committed_with_history(14, None, 0, W, UNCAPPED, LIMITS, &hist)
            .unwrap();
        assert!(out.deletion_held, "row 10 is due at 14, but 14 is unknown");
        assert_eq!(out.bodies_deleted, 0);
        assert!(present(&store, &x));
        let out = gc_h(&store, 15, &[], W, &hist);
        assert_eq!(out.heights_backfilled, 1, "14 back-filled from the history");
        for tip in 16..=14 + W {
            gc_h(&store, tip, &[], W, &hist);
            assert_eq!(
                present(&store, &x),
                tip < 14 + W,
                "x (pinned by 14) at {tip}"
            );
        }
    }

    /// Codex P1 (round 2, native_da.rs completeness): the history holds a
    /// committed height above the dispatched one (boot reconciliation, a
    /// multi-block commit) that re-includes a due row's body. Nothing goes
    /// until dispatch caught up with the committed tip; an unreadable tip
    /// holds too. Then the re-including height's pin keeps the body for its
    /// own window.
    #[test]
    fn gc_holds_deletion_while_committed_heights_are_undispatched() {
        const W: u64 = 4;
        let (_dir, store) = temp_store();
        let x = dummy_action(1);
        let hx = compute_action_hash(&x);
        store.put(&x).unwrap();
        let mut hist = history(&[(10, vec![hx]), (15, vec![hx])]);
        for h in 10..=13 {
            let hashes = hist.known.get(&h).cloned().unwrap_or_default();
            gc_h(&store, h, &hashes, W, &hist);
        }
        // 14 and 15 are committed together; row 10 is due at 14.
        hist.tip = Some(15);
        let out = gc_h(&store, 14, &[], W, &hist);
        assert!(out.undispatched_commits && out.deletion_held);
        assert_eq!(out.bodies_deleted, 0);
        assert!(present(&store, &x), "committed 15 still needs x");
        hist.tip_fail = true;
        let out = gc_h(&store, 15, &[hx], W, &hist);
        assert!(out.deletion_held, "an unreadable tip holds");
        assert!(present(&store, &x));
        hist.tip_fail = false;
        for tip in 16..=15 + W {
            hist.tip = Some(tip);
            let out = gc_h(&store, tip, &[], W, &hist);
            assert!(!out.undispatched_commits, "caught up at {tip}");
            assert_eq!(
                present(&store, &x),
                tip < 15 + W,
                "x (pinned by 15) at {tip}"
            );
        }
    }

    /// A gap below the history's pruned height is skipped (those blocks are
    /// never served again), but nothing goes until every skipped height is
    /// itself due: one of them may have re-included a body an older row
    /// lists.
    #[test]
    fn gc_pruned_gap_holds_deletion_until_the_skipped_heights_are_due() {
        const W: u64 = 4;
        let (_dir, store) = temp_store();
        let x = dummy_action(1);
        let hx = compute_action_hash(&x);
        store.put(&x).unwrap();
        let hist = TestHistory {
            pruned: 30,
            empty: true,
            ..TestHistory::default()
        };
        gc_h(&store, 10, &[hx], W, &hist);
        // 11..=29 pruned (skipped), 30 and 31 back-filled; skipped 29 is due
        // only at 33.
        let out = gc_h(&store, 32, &[], W, &hist);
        assert_eq!(out.heights_backfilled, 2);
        assert!(out.deletion_held);
        assert!(present(&store, &x), "row 10 waits for skipped 29 to be due");
        let out = gc_h(&store, 33, &[], W, &hist);
        assert_eq!((out.rows_deleted, out.deletion_held), (1, false));
        assert!(!present(&store, &x));
    }

    /// Codex r3 / claude-review r4 P2 (torus issue 282de5bb): the history
    /// holds an UNCOMMITTED block above its committed tip (the tail) that
    /// re-includes x as x's row comes due. Peers block-sync that block and
    /// it may still commit, possibly on a node with no cached proposal,
    /// which then rebuilds it from the DA store. So the row goes but x
    /// (body and reference) stays; y, which the tail does not reference,
    /// goes on time. Once the tail commits, its record pins x for its own
    /// window.
    #[test]
    fn gc_keeps_the_bodies_the_uncommitted_tail_references() {
        const W: u64 = 4;
        let (_dir, store) = temp_store();
        let (x, y) = (dummy_action(1), dummy_action(2));
        let (hx, hy) = (compute_action_hash(&x), compute_action_hash(&y));
        store.put_batch(&[x.clone(), y.clone()]).unwrap();
        let mut hist = history(&[(10, vec![hx, hy])]);
        for h in 10..=13 {
            let hashes = hist.known.get(&h).cloned().unwrap_or_default();
            gc_h(&store, h, &hashes, W, &hist);
        }
        // Row 10 is due at 14; the uncommitted 15 re-includes x.
        hist.tip = Some(14);
        hist.tail = vec![hx];
        let out = gc_h(&store, 14, &[], W, &hist);
        assert_eq!((out.rows_deleted, out.deletion_held), (1, false));
        assert_eq!(out.hold_reason, None);
        assert_eq!((out.bodies_deleted, out.tail_pinned), (1, 1));
        assert!(present(&store, &x), "the uncommitted tail still needs x");
        assert!(!present(&store, &y), "y is not in the tail: it goes");
        // 15 commits and dispatches: its record pins x for its own window.
        hist.tail.clear();
        for tip in 15..=15 + W {
            hist.tip = Some(tip);
            let hashes = if tip == 15 { vec![hx] } else { vec![] };
            gc_h(&store, tip, &hashes, W, &hist);
            assert_eq!(present(&store, &x), tip < 15 + W, "x (pinned by 15) at {tip}");
        }
        assert_eq!(bookkeeping_keys(&store), 1, "only the GC state key remains");
    }

    /// A tail block that cannot be read or decoded holds the whole pass
    /// (the safe direction): nothing goes until the tail reads again.
    #[test]
    fn gc_unreadable_tail_holds_the_pass() {
        const W: u64 = 4;
        let (_dir, store) = temp_store();
        let x = dummy_action(1);
        let hx = compute_action_hash(&x);
        store.put(&x).unwrap();
        let mut hist = history(&[(10, vec![hx])]);
        for h in 10..=13 {
            let hashes = hist.known.get(&h).cloned().unwrap_or_default();
            gc_h(&store, h, &hashes, W, &hist);
        }
        hist.tip = Some(14);
        hist.tail_fail = true;
        let out = gc_h(&store, 14, &[], W, &hist);
        assert!(out.deletion_held);
        assert_eq!(out.hold_reason, Some(DaGcHoldReason::TailUnreadable));
        assert_eq!(out.rows_deleted, 0);
        assert!(present(&store, &x));
        hist.tail_fail = false;
        hist.tip = Some(15);
        let out = gc_h(&store, 15, &[], W, &hist);
        assert_eq!((out.rows_deleted, out.hold_reason), (1, None));
        assert!(!present(&store, &x));
    }

    /// Slow-pass observability (8h): the timed pass reports its phases (the
    /// tail read, here made slow by the history; the due-row scan, which
    /// excludes it; the GC state read; the batch write), the bodies the
    /// due rows list, the tail's hashes and the committed tip it read. And it
    /// changes nothing: the same passes untimed give the same outcomes and
    /// byte-identical stores.
    #[test]
    fn gc_timed_pass_reports_its_phases_and_changes_nothing() {
        use std::time::Duration;
        const W: u64 = 4;
        let (_d1, timed) = temp_store();
        let (_d2, plain) = temp_store();
        let (x, y, z) = (dummy_action(1), dummy_action(2), dummy_action(3));
        let (hx, hy, hz) = (
            compute_action_hash(&x),
            compute_action_hash(&y),
            compute_action_hash(&z),
        );
        for store in [&timed, &plain] {
            store.put_batch(&[x.clone(), y.clone(), z.clone()]).unwrap();
        }
        let mut hist = history(&[(10, vec![hx, hy]), (11, vec![hz])]);
        let mut last = DaGcPassStats::default();
        for h in 10..=14 {
            if h == 14 {
                // Row 10 is due; the uncommitted tail re-includes x.
                hist.tip = Some(14);
                hist.tail = vec![hx];
                hist.tail_delay = Duration::from_millis(30);
            }
            let hashes = hist.known.get(&h).cloned().unwrap_or_default();
            let mut stats = DaGcPassStats::default();
            let a = timed
                .gc_committed_with_history_timed(
                    h,
                    Some(&hashes),
                    0,
                    W,
                    UNCAPPED,
                    LIMITS,
                    &hist,
                    &mut stats,
                )
                .unwrap();
            let b = gc_h(&plain, h, &hashes, W, &hist);
            assert_eq!(a, b, "height {h}: the same outcome");
            assert_eq!(dump(&timed), dump(&plain), "height {h}: the same store");
            assert!(stats.state_read > Duration::ZERO, "height {h}: {stats:?}");
            if h < 14 {
                assert_eq!(stats.bodies_scanned, 0, "height {h}: no row due");
                assert_eq!(stats.tail_read, Duration::ZERO, "height {h}");
            }
            last = stats;
        }
        assert_eq!(last.bodies_scanned, 2, "row 10 lists x and y");
        assert_eq!(last.tail_hashes, 1);
        assert_eq!(last.committed_tip, Some(14));
        assert!(last.tail_read >= Duration::from_millis(30), "{last:?}");
        assert!(
            last.delete_scan < Duration::from_millis(30),
            "the scan excludes the tail read: {last:?}"
        );
        assert!(last.write > Duration::ZERO, "{last:?}");
    }

    /// Codex P2 (8h review): a pass whose phase fails still reports that
    /// phase's elapsed time (a slow failing write must not read
    /// `write_ms=0`) and still returns the error. Both write sites: the
    /// single batch of a pass without a gap, and the first batch of a
    /// back-filling pass (whose back-fill is timed too).
    #[test]
    fn gc_failed_write_reports_its_elapsed_and_still_errs() {
        use std::time::Duration;
        const W: u64 = 8;
        let (dir, store) = temp_store();
        let (x, y) = (dummy_action(1), dummy_action(2));
        let (hx, hy) = (compute_action_hash(&x), compute_action_hash(&y));
        store.put_batch(&[x.clone(), y.clone()]).unwrap();
        let hist = history(&[(10, vec![hx, hy]), (13, vec![hx])]);
        for h in 10..=12 {
            let hashes = hist.known.get(&h).cloned().unwrap_or_default();
            gc_h(&store, h, &hashes, W, &hist);
        }
        drop(store);
        let ro = NativeDaStore::new(StateDb::open_read_only(dir.path()).unwrap());
        // No gap: record height 13, one batch, the write fails.
        let mut stats = DaGcPassStats::default();
        let err = ro.gc_committed_with_history_timed(
            13,
            Some(&[hx]),
            0,
            W,
            UNCAPPED,
            LIMITS,
            &hist,
            &mut stats,
        );
        assert!(err.is_err(), "the failed write is still an error");
        assert!(stats.state_read > Duration::ZERO, "{stats:?}");
        assert!(stats.record > Duration::ZERO, "{stats:?}");
        assert!(
            stats.write > Duration::ZERO,
            "the failed write's time: {stats:?}"
        );
        // A gap (13 and 14 unrecorded): the back-fill batch write fails.
        let mut stats = DaGcPassStats::default();
        let err = ro.gc_committed_with_history_timed(
            15,
            Some(&[hy]),
            0,
            W,
            UNCAPPED,
            LIMITS,
            &hist,
            &mut stats,
        );
        assert!(err.is_err(), "the failed back-fill write is still an error");
        assert!(stats.backfill > Duration::ZERO, "{stats:?}");
        assert!(
            stats.write > Duration::ZERO,
            "the failed write's time: {stats:?}"
        );
    }

    /// P3 (claude-review r4): every pass that deletes nothing names why
    /// (the `reason` label of `torus_da_gc_held_passes`), each rule its own
    /// reason; a pass that is not held names none.
    #[test]
    fn gc_every_held_pass_names_its_reason() {
        use DaGcHoldReason as R;
        const W: u64 = 4;
        // A store with x recorded at 10 and every height through 13.
        let fresh = || {
            let (dir, store) = temp_store();
            let x = dummy_action(1);
            store.put(&x).unwrap();
            let hx = compute_action_hash(&x);
            for h in 10..=13 {
                let hashes = if h == 10 { vec![hx] } else { vec![] };
                gc_h(&store, h, &hashes, W, &history(&[]));
            }
            (dir, store)
        };
        let reason = |store: &NativeDaStore, height: u64, record: Option<&[B256]>, hist: &TestHistory| {
            let out = store
                .gc_committed_with_history(height, record, 0, W, UNCAPPED, LIMITS, hist)
                .unwrap();
            assert_eq!(out.deletion_held, out.hold_reason.is_some(), "{out:?}");
            out.hold_reason
        };
        let (_d, store) = fresh();
        assert_eq!(reason(&store, 14, Some(&[]), &history(&[])), None, "not held");
        let (_d, store) = fresh();
        let missing = TestHistory::default();
        assert_eq!(reason(&store, 20, Some(&[]), &missing), Some(R::Gap));
        let (_d, store) = fresh();
        let broken = TestHistory {
            fail: true,
            ..TestHistory::default()
        };
        assert_eq!(reason(&store, 20, Some(&[]), &broken), Some(R::Gap));
        let (_d, store) = fresh();
        let far = 14 + DA_GC_MAX_BACKFILL_HEIGHTS + 1;
        assert_eq!(reason(&store, far, Some(&[]), &history(&[])), Some(R::Backfilling));
        let (_d, store) = fresh();
        assert_eq!(reason(&store, 14, None, &history(&[])), Some(R::Unrecorded));
        let (_d, store) = fresh();
        let pruned = TestHistory {
            pruned: 30,
            empty: true,
            ..TestHistory::default()
        };
        assert_eq!(reason(&store, 32, Some(&[]), &pruned), Some(R::PrunedGap));
        let (_d, store) = fresh();
        let ahead = TestHistory {
            empty: true,
            tip: Some(15),
            ..TestHistory::default()
        };
        assert_eq!(reason(&store, 14, Some(&[]), &ahead), Some(R::DispatchBehind));
        let (_d, store) = fresh();
        let tip_broken = TestHistory {
            empty: true,
            tip_fail: true,
            ..TestHistory::default()
        };
        assert_eq!(reason(&store, 14, Some(&[]), &tip_broken), Some(R::TipUnreadable));
        let (_d, store) = fresh();
        let tail_broken = TestHistory {
            empty: true,
            tail_fail: true,
            ..TestHistory::default()
        };
        assert_eq!(reason(&store, 14, Some(&[]), &tail_broken), Some(R::TailUnreadable));
        // A pre-cursor store (16-byte state) replaying below its floor.
        let (_d, store) = temp_store();
        store
            .db
            .put_cf_raw(
                CF_NATIVE_PENDING,
                DA_GC_STATE_KEY,
                &[0u64.to_be_bytes(), 50u64.to_be_bytes()].concat(),
            )
            .unwrap();
        assert_eq!(reason(&store, 20, Some(&[]), &history(&[])), Some(R::PreCursor));
        let labels: std::collections::HashSet<_> = [
            R::Gap,
            R::Backfilling,
            R::Unrecorded,
            R::PreCursor,
            R::PrunedGap,
            R::DispatchBehind,
            R::TipUnreadable,
            R::TailUnreadable,
            R::StoreError,
            R::FenceFull,
        ]
        .iter()
        .map(|r| r.as_str())
        .collect();
        assert_eq!(labels.len(), 10, "one label per reason");
    }

    /// Idempotence: back-filling heights writes exactly what recording them
    /// live wrote (re-inclusions across the back-filled heights included),
    /// and replaying already recorded heights, live or after a back-fill,
    /// changes nothing. Both stores then GC identically.
    #[test]
    fn gc_backfill_matches_live_recording_and_replays_change_nothing() {
        const W: u64 = 6;
        let actions: Vec<_> = (1..=4).map(dummy_action).collect();
        let h: Vec<B256> = actions.iter().map(compute_action_hash).collect();
        let sched = history(&[
            (10, vec![h[0], h[1]]),
            (12, vec![h[0]]),
            (13, vec![h[2], h[0], h[0]]),
            (15, vec![h[3], h[2]]),
        ]);
        let at = |height: u64| sched.known.get(&height).cloned().unwrap_or_default();
        let (_d1, live) = temp_store();
        let (_d2, filled) = temp_store();
        for store in [&live, &filled] {
            store.put_batch(&actions).unwrap();
        }
        for tip in 10..=16 {
            gc_h(&live, tip, &at(tip), W, &sched);
        }
        gc_h(&filled, 10, &at(10), W, &sched);
        let out = gc_h(&filled, 16, &at(16), W, &sched);
        assert_eq!(out.heights_backfilled, 5, "11..=15");
        assert_eq!(dump(&filled), dump(&live), "back-fill = live recording");
        let before = dump(&live);
        for tip in [10, 13, 15, 16] {
            gc_h(&live, tip, &at(tip), W, &sched);
            gc_h(&filled, tip, &at(tip), W, &sched);
        }
        assert_eq!(dump(&live), before, "a replay changes nothing");
        assert_eq!(dump(&filled), before);
        for tip in 17..=15 + W {
            gc_h(&live, tip, &[], W, &sched);
            gc_h(&filled, tip, &[], W, &sched);
            assert_eq!(dump(&filled), dump(&live), "tip {tip}");
            assert_eq!(present(&live, &actions[0]), tip < 13 + W, "x at tip {tip}");
        }
        assert_eq!(bookkeeping_keys(&live), 1, "only the GC state key remains");
    }

    fn gc_state(store: &NativeDaStore) -> DaGcState {
        DaGcState::decode(
            store
                .db
                .get_cf_raw(CF_NATIVE_PENDING, DA_GC_STATE_KEY)
                .unwrap()
                .as_deref(),
        )
    }

    fn put_gc_state(store: &NativeDaStore, state: DaGcState) {
        store
            .db
            .put_cf_raw(CF_NATIVE_PENDING, DA_GC_STATE_KEY, &state.encode())
            .unwrap();
    }

    /// Records are idempotent: with the cursor rewound over heights whose
    /// records already landed (rows, references, re-inclusion markers), the
    /// back-fill re-writes them and the store ends exactly as without the
    /// rewind.
    #[test]
    fn gc_rewriting_landed_records_changes_nothing() {
        const W: u64 = 6;
        let actions: Vec<_> = (1..=4).map(dummy_action).collect();
        let h: Vec<B256> = actions.iter().map(compute_action_hash).collect();
        let sched = history(&[
            (10, vec![h[0], h[1]]),
            (12, vec![h[0]]),
            (13, vec![h[2], h[0], h[0]]),
            (15, vec![h[3], h[2]]),
        ]);
        let at = |height: u64| sched.known.get(&height).cloned().unwrap_or_default();
        let (_d1, control) = temp_store();
        let (_d2, rewound) = temp_store();
        for store in [&control, &rewound] {
            store.put_batch(&actions).unwrap();
            for tip in 10..=15 {
                gc_h(store, tip, &at(tip), W, &sched);
            }
        }
        let mut state = gc_state(&rewound);
        assert_eq!(state.cursor, Some(16));
        state.cursor = Some(11);
        put_gc_state(&rewound, state);
        gc_h(&control, 16, &[], W, &sched);
        let out = gc_h(&rewound, 16, &[], W, &sched);
        assert_eq!(out.heights_backfilled, 5, "11..=15 re-written");
        assert_eq!(dump(&rewound), dump(&control));
    }

    /// Turn `store` into what a pre-cursor binary left: its rows and
    /// references, a 16-byte state (no cursor) whose replay floor is
    /// `next_record`.
    fn make_pre_cursor(store: &NativeDaStore, next_record: u64) {
        let state = gc_state(store);
        put_gc_state(
            store,
            DaGcState {
                frontier: state.frontier,
                legacy_next_record: next_record,
                cursor: None,
                hold: 0,
                pre_cursor: true,
            },
        );
        assert_eq!(gc_state(store).encode().len(), 16);
    }

    /// A store a pre-cursor binary wrote (16-byte state): a replay below its
    /// last recorded height still records nothing and establishes nothing.
    #[test]
    fn gc_replay_on_a_pre_cursor_store_records_nothing() {
        const W: u64 = 4;
        let (_dir, store) = temp_store();
        let (x, y) = (dummy_action(1), dummy_action(2));
        store.put_batch(&[x.clone(), y.clone()]).unwrap();
        gc_h(&store, 20, &[compute_action_hash(&x)], W, &history(&[]));
        make_pre_cursor(&store, 21);
        let before = dump(&store);
        gc_h(&store, 19, &[compute_action_hash(&y)], W, &history(&[]));
        assert_eq!(dump(&store), before, "a replay changes nothing");
        assert_eq!(gc_state(&store).cursor, None);
    }

    /// Upgrade from a pre-cursor store (owner, s128): the old binary recorded
    /// rows 10 [x] and 11 [y], then crashed before recording 598, which
    /// re-included x, and never recorded it. On the first pass of the new
    /// code (600) the cursor starts where the block tree's retained range
    /// starts (its prune pointer, 5), not at 600: the back-fill walks
    /// 5..=601 at 256 heights per pass, deleting nothing until it caught up
    /// (602), and it re-finds 598's pin, so x outlives row 10 until 598's
    /// window ends while y goes as soon as deletion resumes.
    #[test]
    fn gc_upgrade_from_a_pre_cursor_store_backfills_the_retained_tree_range() {
        const W: u64 = 8;
        const B: u64 = DA_GC_MAX_BACKFILL_HEIGHTS;
        let (_dir, store) = temp_store();
        let (x, y) = (dummy_action(1), dummy_action(2));
        let (hx, hy) = (compute_action_hash(&x), compute_action_hash(&y));
        store.put_batch(&[x.clone(), y.clone()]).unwrap();
        // What the old binary wrote: rows 10 and 11 (nothing due yet).
        for (h, hashes) in [(10, vec![hx]), (11, vec![hy])] {
            store
                .gc_committed_with_history(h, Some(&hashes), 0, W, 0, LIMITS, &history(&[]))
                .unwrap();
        }
        make_pre_cursor(&store, 12);
        let tree = TestHistory {
            pruned: 5,
            known: [(10, vec![hx]), (11, vec![hy]), (598, vec![hx])]
                .into_iter()
                .collect(),
            empty: true,
            ..TestHistory::default()
        };
        let out = gc_h(&store, 600, &[], W, &tree);
        assert_eq!((out.heights_backfilled, out.deletion_held), (B, true));
        assert_eq!(out.bodies_deleted, 0);
        let out = gc_h(&store, 601, &[], W, &tree);
        assert_eq!((out.heights_backfilled, out.deletion_held), (B, true));
        assert!(
            present(&store, &x) && present(&store, &y),
            "rows 10, 11 due, held"
        );
        // 517..=601 closes the gap; 602 records; rows 10 and 11 go.
        let out = gc_h(&store, 602, &[], W, &tree);
        assert_eq!(
            (out.heights_backfilled, out.deletion_held),
            (601 - 517 + 1, false)
        );
        assert!(!present(&store, &y), "deletion resumed once caught up");
        for tip in 603..=598 + W {
            assert!(present(&store, &x), "tip {tip}: 598 still pins x");
            gc_h(&store, tip, &[], W, &tree);
        }
        assert!(!present(&store, &x), "598's window ended");
        assert_eq!(bookkeeping_keys(&store), 1, "only the GC state key remains");
    }

    /// A long gap is back-filled at most `DA_GC_MAX_BACKFILL_HEIGHTS` heights
    /// per pass, deleting nothing until it is closed.
    #[test]
    fn gc_backfill_is_bounded_per_pass() {
        const W: u64 = 4;
        const B: u64 = DA_GC_MAX_BACKFILL_HEIGHTS;
        let (_dir, store) = temp_store();
        let x = dummy_action(1);
        store.put(&x).unwrap();
        let hist = history(&[]);
        gc_h(&store, 10, &[compute_action_hash(&x)], W, &hist);
        let gap_end = 11 + 2 * B + 50;
        let out = gc_h(&store, gap_end, &[], W, &hist);
        assert_eq!((out.heights_backfilled, out.deletion_held), (B, true));
        assert!(!out.backfill_stalled, "bounded, not stalled");
        let out = gc_h(&store, gap_end + 1, &[], W, &hist);
        assert_eq!((out.heights_backfilled, out.deletion_held), (B, true));
        assert!(present(&store, &x));
        let out = gc_h(&store, gap_end + 2, &[], W, &hist);
        assert_eq!((out.heights_backfilled, out.deletion_held), (52, false));
        assert_eq!(out.bodies_deleted, 1);
        assert_eq!(gc_state(&store).cursor, Some(gap_end + 3));
    }

    /// Markers across a bounded pass: a pass stopped by its row limit leaves
    /// the next row's markers for the next pass (its moved hash stays), and a
    /// marker without a row (none in a consistent store) goes once the GC
    /// passes its height.
    #[test]
    fn gc_markers_survive_a_backlog_and_orphans_go() {
        const W: u64 = 4;
        let one_row = DaGcLimits {
            max_rows: 1,
            max_deletes: 1 << 20,
        };
        let (_dir, store) = temp_store();
        let actions: Vec<_> = (1..=4).map(dummy_action).collect();
        let h: Vec<B256> = actions.iter().map(compute_action_hash).collect();
        store.put_batch(&actions).unwrap();
        // Rows 10 [x, y] and 11 [x', y']; 12 re-includes x and x'.
        store
            .gc_committed(10, &[h[0], h[1]], 0, W, 0, LIMITS)
            .unwrap();
        store
            .gc_committed(11, &[h[2], h[3]], 0, W, 0, LIMITS)
            .unwrap();
        let out = store
            .gc_committed(12, &[h[0], h[2]], 0, W, 0, LIMITS)
            .unwrap();
        assert_eq!(out.references_moved, 2);
        // Orphan markers: below every row, and above every row (height 13).
        for (height, hash) in [(3, h[1]), (13, h[3])] {
            store
                .db
                .put_cf_raw(
                    CF_NATIVE_PENDING,
                    &da_gc_moved_key(height, hash.as_slice()),
                    &[],
                )
                .unwrap();
        }
        for height in 13..15 {
            store.gc_committed(height, &[], 0, W, 0, LIMITS).unwrap();
        }
        let live = |store: &NativeDaStore| -> Vec<bool> {
            actions.iter().map(|a| present(store, a)).collect()
        };
        // 15: rows 10 and 11 are due, the limit takes 10 only.
        let out = store
            .gc_committed(15, &[], 0, W, UNCAPPED, one_row)
            .unwrap();
        assert_eq!((out.rows_deleted, out.backlog), (1, true));
        assert_eq!(live(&store), [true, false, true, true]);
        // 16: row 11 (its marker kept x'), then the limit stops before 12.
        let out = store
            .gc_committed(16, &[], 0, W, UNCAPPED, one_row)
            .unwrap();
        assert_eq!((out.rows_deleted, out.backlog), (1, true));
        assert_eq!(live(&store), [true, false, true, false]);
        // 17: row 12 takes x and x', and the pass drains the marker at 13.
        let out = store.gc_committed(17, &[], 0, W, UNCAPPED, LIMITS).unwrap();
        assert_eq!((out.rows_deleted, out.backlog), (1, false));
        assert_eq!(live(&store), [false; 4]);
        assert_eq!(bookkeeping_keys(&store), 1, "only the GC state key remains");
    }
}
