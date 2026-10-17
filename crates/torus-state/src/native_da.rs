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

use std::sync::{Condvar, Mutex, OnceLock};

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
/// GC state, `frontier (8 BE) ++ next_record (8 BE)` ([`DaGcState`]).
/// Outside the row prefix (`-` sorts before `/`), so no row scan meets it.
const DA_GC_STATE_KEY: &[u8] = b"\xffda-gc-state";
/// Latest reference of a committed hash: `DA_GC_REF_PREFIX ++ hash` -> the
/// highest recorded height whose row lists it (8 BE). Codex P1: a hash that
/// two committed heights reference (a re-inclusion) is moved out of the
/// earlier, still pending row when the later one is recorded, so each hash is
/// listed by exactly one row, its latest, and its body goes when that
/// reference expires. 43-byte keys, outside the row prefix like the state key.
const DA_GC_REF_PREFIX: &[u8; 11] = b"\xffda-gc-ref/";
const DA_GC_REF_KEY_LEN: usize = DA_GC_REF_PREFIX.len() + 32;

fn da_gc_ref_key(hash: &[u8]) -> [u8; DA_GC_REF_KEY_LEN] {
    let mut key = [0u8; DA_GC_REF_KEY_LEN];
    key[..DA_GC_REF_PREFIX.len()].copy_from_slice(DA_GC_REF_PREFIX);
    key[DA_GC_REF_PREFIX.len()..].copy_from_slice(hash);
    key
}

/// The durable GC state ([`DA_GC_STATE_KEY`]), written in the batch of the
/// pass that changes it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct DaGcState {
    /// Lowest height whose row may still exist (rows below it were deleted).
    /// Only a seek start: it keeps each pass from walking old row tombstones.
    frontier: u64,
    /// One past the highest recorded height. A dispatch below it is a replay
    /// (committed blocks are final: its row is already written, or already
    /// GC'd) and records nothing, so a replay never re-attaches a hash to an
    /// earlier row than its latest reference.
    next_record: u64,
}

impl DaGcState {
    /// Absent or malformed = the initial state.
    fn decode(bytes: Option<&[u8]>) -> Self {
        match bytes {
            Some(b) if b.len() == 16 => Self {
                frontier: u64::from_be_bytes(b[..8].try_into().expect("8 bytes")),
                next_record: u64::from_be_bytes(b[8..].try_into().expect("8 bytes")),
            },
            _ => Self::default(),
        }
    }

    fn encode(self) -> [u8; 16] {
        let mut out = [0u8; 16];
        out[..8].copy_from_slice(&self.frontier.to_be_bytes());
        out[8..].copy_from_slice(&self.next_record.to_be_bytes());
        out
    }
}

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
    /// the consensus app passes the block tree's durable prune pointer minus a
    /// margin (Codex P1: a window derived from the configured block-tree
    /// retention runs ahead of the pruner, which removes at most 64 heights
    /// per commit). `u64::MAX` = no bound, `0` = delete nothing.
    ///
    /// A hash can be committed again at a later height (a re-inclusion). Each
    /// hash is listed only by the row of its LATEST recorded height: a durable
    /// per-hash reference ([`DA_GC_REF_PREFIX`]) finds the earlier row, which
    /// loses the hash in the same batch, so the body and its shards go when
    /// the latest reference expires (Codex P1). A `height` below one already
    /// recorded is a replay and records nothing.
    ///
    /// One atomic `WriteBatch` per pass (the row, the references, any earlier
    /// row a re-inclusion rewrites, the deletes and the GC state), bounded by
    /// `limits`; the first due row is always taken whole, so the GC always
    /// progresses. The rows live next to the bodies (non-32-byte keys), so a
    /// crash never splits a row from its bodies and a restart leaves none
    /// behind. Empty `hashes` records nothing; a `retention` of 0 counts as 1.
    ///
    /// Cost per pass: one point read (the GC state), one `MultiGet` of the
    /// recorded hashes' reference keys (normally all bloom-filter misses), a
    /// bounded iterator over the due rows, one batch.
    pub fn gc_committed(
        &self,
        height: u64,
        hashes: &[B256],
        shard_count: u16,
        retention: u64,
        keep_from: u64,
        limits: DaGcLimits,
    ) -> Result<DaGcOutcome, StateError> {
        let cf = self.db.cf_handle(CF_NATIVE_PENDING)?;
        let state = DaGcState::decode(
            self.db
                .get_cf_raw(CF_NATIVE_PENDING, DA_GC_STATE_KEY)?
                .as_deref(),
        );
        let mut next = state;
        let mut batch = rocksdb::WriteBatch::default();
        let mut out = DaGcOutcome::default();
        // Hashes this record moved out of an earlier row: if that row ages out
        // in this same pass, its read-back copy still lists them.
        let mut moved: Vec<[u8; 32]> = Vec::new();
        if !hashes.is_empty() && height >= state.next_record.max(state.frontier) {
            self.record_row(
                &mut batch,
                height,
                hashes,
                shard_count,
                state.frontier,
                &mut moved,
            )?;
            out.references_moved = moved.len() as u64;
            next.next_record = height.saturating_add(1);
        }
        // Rows at or below `last_due` are due: `h + retention <= height` and
        // `h < keep_from`.
        let last_due = height
            .checked_sub(retention.max(1))
            .zip(keep_from.checked_sub(1))
            .map(|(window, served)| window.min(served));
        if let Some(last_due) = last_due.filter(|&due| state.frontier <= due) {
            if let Some(frontier) = self.gc_due_rows(
                &mut batch,
                state.frontier,
                last_due,
                limits,
                &moved,
                &mut out,
            )? {
                next.frontier = frontier;
            }
        }
        if next != state {
            batch.put_cf(cf, DA_GC_STATE_KEY, next.encode());
        }
        if !batch.is_empty() {
            self.db.write(batch)?;
        }
        Ok(out)
    }

    /// Add the row of `height` and the latest-reference keys of `hashes` to
    /// `batch`. A hash whose reference names an earlier row still pending (at
    /// or above `frontier`) is a re-inclusion: that row is rewritten without
    /// it (deleted when it empties) and the hash goes to `moved`. The rewrite
    /// is a read-modify-write of one row per earlier height, only on a
    /// re-inclusion. An earlier row's larger shard count is not carried over:
    /// shards past the later row's count stay (kept, never early).
    fn record_row(
        &self,
        batch: &mut rocksdb::WriteBatch,
        height: u64,
        hashes: &[B256],
        shard_count: u16,
        frontier: u64,
        moved: &mut Vec<[u8; 32]>,
    ) -> Result<(), StateError> {
        let cf = self.db.cf_handle(CF_NATIVE_PENDING)?;
        let ref_keys: Vec<[u8; DA_GC_REF_KEY_LEN]> =
            hashes.iter().map(|h| da_gc_ref_key(h.as_slice())).collect();
        let mut earlier: std::collections::BTreeMap<u64, Vec<[u8; 32]>> = Default::default();
        let refs =
            self.db
                .inner()
                .batched_multi_get_cf(cf, ref_keys.iter().map(|k| k.as_slice()), false);
        for (hash, slot) in hashes.iter().zip(refs) {
            let Some(value) = slot? else { continue };
            let Ok(prev) = <[u8; 8]>::try_from(&value[..]).map(u64::from_be_bytes) else {
                continue;
            };
            if (frontier..height).contains(&prev) {
                earlier.entry(prev).or_default().push(hash.0);
            }
        }
        for (prev, detach) in earlier {
            let key = da_gc_row_key(prev);
            if let Some(row) = self.db.get_cf_raw(CF_NATIVE_PENDING, &key)? {
                if let Some((row_shards, listed)) = decode_da_gc_row(&row) {
                    let kept: Vec<B256> = listed
                        .chunks_exact(32)
                        .filter(|h| !detach.iter().any(|d| d[..] == h[..]))
                        .map(B256::from_slice)
                        .collect();
                    if kept.is_empty() {
                        batch.delete_cf(cf, key);
                    } else {
                        batch.put_cf(cf, key, encode_da_gc_row(row_shards, &kept));
                    }
                }
            }
            moved.extend(detach);
        }
        batch.put_cf(
            cf,
            da_gc_row_key(height),
            encode_da_gc_row(shard_count, hashes),
        );
        let at = height.to_be_bytes();
        for key in &ref_keys {
            batch.put_cf(cf, key, at);
        }
        Ok(())
    }

    /// Add the deletes of the rows in `[frontier, last_due]` to `batch`, within
    /// `limits`: each listed body, its reference and its shards (bar the
    /// hashes in `moved`), and the row. Returns the new frontier when any row
    /// went.
    fn gc_due_rows(
        &self,
        batch: &mut rocksdb::WriteBatch,
        frontier: u64,
        last_due: u64,
        limits: DaGcLimits,
        moved: &[[u8; 32]],
        out: &mut DaGcOutcome,
    ) -> Result<Option<u64>, StateError> {
        let cf = self.db.cf_handle(CF_NATIVE_PENDING)?;
        let mut shards_cf = None;
        let start = da_gc_row_key(frontier);
        let mut opts = rocksdb::ReadOptions::default();
        opts.set_iterate_lower_bound(start.to_vec());
        // `last_due < height <= u64::MAX`, so `last_due + 1` cannot overflow.
        opts.set_iterate_upper_bound(da_gc_row_key(last_due + 1).to_vec());
        let mut it = self.db.inner().raw_iterator_cf_opt(cf, opts);
        it.seek(start);
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
            let bodies = row_hashes.len() / 32;
            let row_deletes = bodies * (1 + usize::from(row_shards));
            if out.rows_deleted as usize >= limits.max_rows.max(1)
                || (out.rows_deleted > 0 && deletes + row_deletes > limits.max_deletes)
            {
                next_frontier = h;
                out.backlog = true;
                break;
            }
            if row_shards > 0 && shards_cf.is_none() {
                shards_cf = Some(self.db.cf_handle(CF_NATIVE_SHARDS)?);
            }
            for hash in row_hashes.chunks_exact(32) {
                // Re-included at this pass's height: the new row owns it.
                if moved.iter().any(|m| m[..] == *hash) {
                    continue;
                }
                batch.delete_cf(cf, hash);
                batch.delete_cf(cf, da_gc_ref_key(hash));
                if let Some(shards) = shards_cf {
                    let hash: &[u8; 32] = hash.try_into().expect("32-byte chunk");
                    for i in 0..row_shards {
                        batch.delete_cf(shards, shard_key(hash, i));
                    }
                }
                out.bodies_deleted += 1;
                out.shard_keys_deleted += u64::from(row_shards);
            }
            batch.delete_cf(cf, key);
            deletes += row_deletes;
            out.rows_deleted += 1;
            it.next();
        }
        it.status()?;
        Ok((out.rows_deleted > 0).then_some(next_frontier))
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
}
