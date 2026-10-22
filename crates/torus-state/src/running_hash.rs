//! Running state hash (docs/plans/running-state-hash-impl.md).
//!
//! Every block's logical change set to the CONSENSUS column families
//! (consensus key -> new value or deletion) is folded into a chained hash.
//! This module owns the frozen classification of which `(cf, key)` pairs are
//! consensus state and the numbering they are hashed under.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use sha2::{Digest, Sha256};

use crate::cf::*;
use crate::db::StateDb;

/// One consensus write: `(hash cf_id, key, Some(value) | None = deletion)`.
pub type WriteEntry = (u8, Vec<u8>, Option<Vec<u8>>);

/// FROZEN `cf_id` numbering of the hashed column families. Part of the hash
/// preimage: never renumber or reuse an id — append only (a change is a
/// coordinated upgrade). Ids `< EVM_DOMAIN_FIRST_ID` form the native domain
/// `D_native`, ids `>= EVM_DOMAIN_FIRST_ID` the EVM domain `D_evm`.
///
/// Excluded (derived or node-local, never hashed): block headers / bodies /
/// hash index, commit manifest, receipts, logs, bloom, tx index,
/// `cf_book_order_rows`, trade CFs, DA pending / shards, all trie / hashed
/// CFs, and every `cf_consensus_meta` key outside [`META_CONSENSUS_PREFIXES`].
///
/// `cf_staking_validators` (id 8): excluded while the consensus thread rotated
/// the validator set at proposal / validation time (consensus bug (b)); since
/// every validator-row write happens at execution, inside the block's flush
/// (epoch rotation included: `torus_economics::epoch_plan`), it is hashed. Id
/// 8 is reused for it rather than a new id: no chain ever hashed id 8 (no
/// live chain; every chain starts from a fresh genesis).
pub const HASHED_CFS: &[(u8, &str)] = &[
    (1, CF_NATIVE_POSITIONS),
    (2, CF_NATIVE_BALANCES),
    (3, CF_NATIVE_ORDER_BOOKS),
    (4, CF_NATIVE_MARKETS),
    (5, CF_NATIVE_ORACLE),
    (6, CF_NATIVE_NONCES),
    (7, CF_NATIVE_ORDERS),
    (8, CF_STAKING_VALIDATORS),
    (9, CF_STAKING_DELEGATIONS),
    (10, CF_STAKING_PERMANENT),
    (11, CF_STAKING_REWARDS),
    (12, CF_GOVERNANCE_PROPOSALS),
    (13, CF_GOVERNANCE_VOTES),
    (14, CF_FEE_CONFIG),
    (15, CF_TREASURY),
    (16, CF_DEV_POOL),
    (17, CF_SLASH_RECORDS),
    (18, CF_JAIL_VOTES),
    (19, CF_SESSIONS),
    (20, CF_CORE_WRITER_QUEUE),
    (21, CF_CONSENSUS_META),
    (22, CF_STATE_HASH_VOTES),
    // Item 3 (s517, rebase s87): liquidation consensus state (native-root tag 6).
    (23, CF_NATIVE_LIQUIDATION),
    (0x80, CF_ACCOUNTS),
    (0x81, CF_STORAGE),
    (0x82, CF_CODE),
];

/// First `cf_id` of the EVM domain.
pub const EVM_DOMAIN_FIRST_ID: u8 = 0x80;

const ID_NATIVE_MARKETS: u8 = 4;
const ID_CONSENSUS_META: u8 = 21;

/// `cf_native_markets` key that is NODE-LOCAL (the wrong-flag restart marker,
/// `native_executor.rs` `BOOK_MODE_MARKER_KEY`): written once per DB, on the
/// first save after the DB gained it — not a function of consensus history.
pub const NODE_LOCAL_MARKET_KEYS: &[&[u8]] = &[b"__book_mode__"];

/// The only consensus keys inside `cf_consensus_meta` (the rest is the
/// hotstuff block tree and node-local markers): pending key rotations
/// (`staking.rs` `pending_rotation_key`) and the validator whitelist
/// (`governance.rs` `whitelist_key`), and the execution-computed
/// validator-set plans `epoch_vset:` (consensus bug (b),
/// `torus_economics::EPOCH_VSET_PREFIX`), and item 7 step 2's epoch EVM gas row
/// (`torus_economics::EVM_EPOCH_GAS_KEY`, F3). All are written only by execution.
pub const META_CONSENSUS_PREFIXES: &[&[u8]] = &[
    b"pending_rotation:",
    b"validator_whitelist:",
    b"epoch_vset:",
    b"evm_epoch_gas",
];

/// Hash `cf_id` of a column family, `None` if the CF is never hashed.
pub fn hashed_cf_id(cf: &str) -> Option<u8> {
    HASHED_CFS.iter().find(|(_, name)| *name == cf).map(|(id, _)| *id)
}

/// Key-level filter inside a hashed CF (see [`NODE_LOCAL_MARKET_KEYS`],
/// [`META_CONSENSUS_PREFIXES`]).
#[inline]
pub fn key_is_hashed(cf_id: u8, key: &[u8]) -> bool {
    match cf_id {
        ID_NATIVE_MARKETS => !NODE_LOCAL_MARKET_KEYS.contains(&key),
        ID_CONSENSUS_META => META_CONSENSUS_PREFIXES.iter().any(|p| key.starts_with(p)),
        _ => true,
    }
}

// ---------------------------------------------------------------------------
// Hash function
// ---------------------------------------------------------------------------

/// Streaming per-block domain digests `D_native(n)` / `D_evm(n)`: SHA-256 over
/// the block's writes to the domain in `(cf_id, key)` order, each framed as
/// `cf_id ‖ len(key) u32 BE ‖ key ‖ (0x00 | 0x01 ‖ len(value) u32 BE ‖ value)`.
/// Callers feed entries in canonical order; the key filter is applied here.
pub struct BlockDigest {
    native: Sha256,
    evm: Sha256,
    entries: usize,
}

impl Default for BlockDigest {
    fn default() -> Self {
        Self::new()
    }
}

impl BlockDigest {
    pub fn new() -> Self {
        Self {
            native: Sha256::new(),
            evm: Sha256::new(),
            entries: 0,
        }
    }

    /// Fold one write (in canonical order). Excluded keys are skipped.
    #[inline]
    pub fn push(&mut self, cf_id: u8, key: &[u8], value: Option<&[u8]>) {
        if !key_is_hashed(cf_id, key) {
            return;
        }
        let h = if cf_id >= EVM_DOMAIN_FIRST_ID {
            &mut self.evm
        } else {
            &mut self.native
        };
        h.update([cf_id]);
        h.update((key.len() as u32).to_be_bytes());
        h.update(key);
        match value {
            None => h.update([0u8]),
            Some(v) => {
                h.update([1u8]);
                h.update((v.len() as u32).to_be_bytes());
                h.update(v);
            }
        }
        self.entries += 1;
    }

    /// Number of hashed entries folded so far.
    pub fn entries(&self) -> usize {
        self.entries
    }

    /// `h_n = SHA-256(prev ‖ height u64 BE ‖ D_native ‖ D_evm)`.
    pub fn chain(self, prev: &[u8; 32], height: u64) -> [u8; 32] {
        let mut h = Sha256::new();
        h.update(prev);
        h.update(height.to_be_bytes());
        h.update(self.native.finalize());
        h.update(self.evm.finalize());
        h.finalize().into()
    }
}

/// `h_n` from `h_{n-1}` and block `n`'s consensus writes, which must be in
/// canonical `(cf_id, key)` order (as produced by the flush).
pub fn next_running_hash(prev: &[u8; 32], height: u64, writes: &[WriteEntry]) -> [u8; 32] {
    let mut d = BlockDigest::new();
    for (id, k, v) in writes {
        d.push(*id, k, v.as_deref());
    }
    d.chain(prev, height)
}

// ---------------------------------------------------------------------------
// Persistence (CF_CONSENSUS_META, node-local)
// ---------------------------------------------------------------------------

/// Checkpoint every N heights (`height % N == 0`); the on-chain attestation
/// heights.
pub const STATE_HASH_CHECKPOINT_INTERVAL: u64 = 100;
/// Checkpoints kept (older ones are deleted in the batch of a new one).
pub const STATE_HASH_CHECKPOINT_RETAIN: u64 = 64;

/// `(height, h_height)` of the last hashed block, if this DB has hashed any.
pub fn read_running_hash(db: &StateDb) -> Option<(u64, [u8; 32])> {
    let v = db
        .get_cf_raw(CF_CONSENSUS_META, META_RUNNING_STATE_HASH)
        .ok()
        .flatten()?;
    (v.len() == 40).then(|| {
        let height = u64::from_be_bytes(v[..8].try_into().unwrap());
        (height, v[8..].try_into().unwrap())
    })
}

fn read_u64(db: &StateDb, key: &[u8]) -> Option<u64> {
    let v = db.get_cf_raw(CF_CONSENSUS_META, key).ok().flatten()?;
    Some(u64::from_be_bytes(v.as_slice().try_into().ok()?))
}

/// The durable native applied height (`META_NATIVE_APPLIED_HEIGHT`, written
/// in the same batch as the running hash).
pub fn read_applied_height(db: &StateDb) -> Option<u64> {
    read_u64(db, META_NATIVE_APPLIED_HEIGHT)
}

/// The activation height the stored chain started at (see
/// `META_RUNNING_STATE_HASH_ACTIVATION`).
pub fn read_activation_height(db: &StateDb) -> Option<u64> {
    read_u64(db, META_RUNNING_STATE_HASH_ACTIVATION)
}

/// The CHAIN-WIDE activation height every flush's chain step reads; `None` =
/// not hashing. P0.0 phase 3: set by execution from the committed headers
/// (the first height whose header carries the `RunningStateHash` bit, see
/// [`header_activation`]), never from this node's schedule.
pub fn read_configured_activation(db: &StateDb) -> Option<u64> {
    read_u64(db, META_STATE_HASH_CONFIGURED_ACTIVATION)
}

/// First height this node could not extend a valid chain at (sticky; see
/// `META_RUNNING_STATE_HASH_UNVERIFIED`). `Some` = hash-unverified: no new
/// hashes or checkpoints, no attestations, no fail-stop decisions.
pub fn read_unverified_since(db: &StateDb) -> Option<u64> {
    read_u64(db, META_RUNNING_STATE_HASH_UNVERIFIED)
}

/// Fail-stop record (see `META_STATE_HASH_DIVERGED`): the checkpoint whose
/// on-chain quorum hash differed from the local one.
pub fn read_divergence(db: &StateDb) -> Option<u64> {
    read_u64(db, META_STATE_HASH_DIVERGED)
}

/// Persist the fail-stop record (node-local META, never hashed). Keeps the
/// first record.
pub fn record_divergence(db: &StateDb, checkpoint: u64) -> Result<(), crate::StateError> {
    if read_divergence(db).is_some() {
        return Ok(());
    }
    db.put_cf_raw(CF_CONSENSUS_META, META_STATE_HASH_DIVERGED, &checkpoint.to_be_bytes())
}

/// P0.0 phase 3: the running-hash activation the committed headers define at
/// `height`, whose header carries the `RunningStateHash` bit iff `bit_on`:
/// `None` when it does not (nothing is hashed at this height), else the first
/// height whose header carries it (`h_{that-1}` = zeros). `bit_at(h)` reads
/// the bit of the stored committed header at `h < height` (`Err`: the row is
/// missing or does not decode). A schedule only ever switches a feature on,
/// so the bit is monotone: the parent's bit settles the usual case (the
/// activation height itself, or `height` 1), a binary search the rest.
pub fn header_activation<E>(
    height: u64,
    bit_on: bool,
    mut bit_at: impl FnMut(u64) -> Result<bool, E>,
) -> Result<Option<u64>, E> {
    if !bit_on {
        return Ok(None);
    }
    if height <= 1 || !bit_at(height - 1)? {
        return Ok(Some(height.max(1)));
    }
    // The bit is on at `hi`; the first height with it is in `lo..=hi`.
    let (mut lo, mut hi) = (1, height - 1);
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        if bit_at(mid)? {
            hi = mid;
        } else {
            lo = mid + 1;
        }
    }
    Ok(Some(lo))
}

/// Store the CHAIN-WIDE activation height the flushes chain from (`None` =
/// disabled; `Some(0)` is treated as 1, the first executable height). P0.0
/// phase 3: execution calls it with [`header_activation`] before a block whose
/// headers change it (once per chain, at the first header with the bit), not
/// the boot from the chain config. When the configuration differs from the
/// one the stored chain was built under, the stored chain (hash, activation
/// record, unverified marker, checkpoints) is discarded: it belongs to
/// another chain definition. Returns whether a stored chain was discarded.
pub fn configure_activation(db: &StateDb, activation: Option<u64>) -> Result<bool, crate::StateError> {
    let activation = activation.map(|a| a.max(1));
    let previous = read_configured_activation(db);
    let start = read_activation_height(db);
    let checkpoints = checkpoint_heights(db);
    let has_chain = has_stored_chain(db, &checkpoints);
    let reset = has_chain && (previous != activation || start.is_some_and(|s| Some(s) != activation));
    let cf = db.cf_handle(CF_CONSENSUS_META)?;
    let mut batch = rocksdb::WriteBatch::default();
    if reset {
        discard_stored_chain(&mut batch, cf, &checkpoints);
        tracing::warn!(
            ?previous,
            ?activation,
            stored_chain_start = ?start,
            "running state hash: activation config changed, stored chain discarded"
        );
    }
    match activation {
        Some(a) => batch.put_cf(cf, META_STATE_HASH_CONFIGURED_ACTIVATION, a.to_be_bytes()),
        None => batch.delete_cf(cf, META_STATE_HASH_CONFIGURED_ACTIVATION),
    }
    db.write(batch)?;
    tracing::info!(?activation, "running state hash activation (committed headers)");
    Ok(reset)
}

/// Whether `db` holds any part of a stored chain (hash, activation record,
/// unverified marker, checkpoints `checkpoints`).
fn has_stored_chain(db: &StateDb, checkpoints: &[u64]) -> bool {
    read_activation_height(db).is_some()
        || read_running_hash(db).is_some()
        || read_unverified_since(db).is_some()
        || !checkpoints.is_empty()
}

/// Delete the stored chain (hash, activation record, unverified marker and
/// `checkpoints`) in `batch`: it belongs to another chain definition.
fn discard_stored_chain(
    batch: &mut rocksdb::WriteBatch,
    cf: &rocksdb::ColumnFamily,
    checkpoints: &[u64],
) {
    for key in [
        META_RUNNING_STATE_HASH,
        META_RUNNING_STATE_HASH_ACTIVATION,
        META_RUNNING_STATE_HASH_UNVERIFIED,
    ] {
        batch.delete_cf(cf, key);
    }
    for h in checkpoints {
        batch.delete_cf(cf, checkpoint_key(*h));
    }
}

// ---------------------------------------------------------------------------
// L6.1: window-committed Classic order books
// (docs/plans/l61-window-committed-books.md). Consensus-changing: P0.0 feature
// `book_window_commit` (genesis `consensus.activations.book_window_commit` or the
// bool alias `consensus.book_window_commit`), default off = today's preimage.
// ---------------------------------------------------------------------------

/// Hash `cf_id` of `cf_native_order_books` (frozen, [`HASHED_CFS`]).
pub const ID_NATIVE_ORDER_BOOKS: u8 = 3;

/// L6.1: the node-local copy of the L6.1 activation height the committed
/// headers define (the first header carrying the `BookWindowCommit` bit;
/// `META_STATE_HASH_BOOK_WINDOW`, written by execution through
/// [`configure_book_window`] before the block that changes it), `None` = off.
/// Read by the digest only; the execution rule reads the executed header's
/// bit itself.
pub fn read_book_window(db: &StateDb) -> Option<u64> {
    db.get_cf_raw(CF_CONSENSUS_META, META_STATE_HASH_BOOK_WINDOW)
        .ok()
        .flatten()
        .and_then(|v| <[u8; 8]>::try_from(v.as_slice()).ok())
        .map(u64::from_be_bytes)
}

/// L6.1: set the L6.1 activation the digest reads (P0.0: execution calls it
/// with the headers' activation, after a pipeline barrier, before the first
/// block it changes is flushed). It is part of the chain's definition (it
/// changes the hash preimage from its height on), so a stored chain that
/// reaches the first height whose rule changes (the lower of the old and the
/// new activation) is discarded, as an activation change does. A chain whose
/// head is below that height was hashed under both rules alike and is kept
/// (the bit turning on at H, with the chain at H - 1). Returns whether a
/// stored chain was discarded.
pub fn configure_book_window(db: &StateDb, on: Option<u64>) -> Result<bool, crate::StateError> {
    let stored = db.get_cf_raw(CF_CONSENSUS_META, META_STATE_HASH_BOOK_WINDOW)?;
    let previous = stored
        .as_deref()
        .and_then(|v| <[u8; 8]>::try_from(v).ok())
        .map(u64::from_be_bytes);
    // An undecodable row (not 8 bytes) counts as a change: rewritten.
    if previous == on && (on.is_some() || stored.is_none()) {
        // Unchanged: no write (a read-only DB opens with the same config).
        return Ok(false);
    }
    let checkpoints = checkpoint_heights(db);
    // The first height whose digest rule differs between `previous` and `on`.
    let changed_from = previous
        .unwrap_or(u64::MAX)
        .min(on.unwrap_or(u64::MAX));
    let reset = stored_chain_head(db, &checkpoints).is_some_and(|head| head >= changed_from);
    let cf = db.cf_handle(CF_CONSENSUS_META)?;
    let mut batch = rocksdb::WriteBatch::default();
    if reset {
        discard_stored_chain(&mut batch, cf, &checkpoints);
        tracing::warn!(
            ?previous,
            ?on,
            "running state hash: book_window_commit activation changed below the stored chain's \
             head, stored chain discarded"
        );
    }
    if let Some(a) = on {
        batch.put_cf(cf, META_STATE_HASH_BOOK_WINDOW, a.to_be_bytes());
    } else {
        batch.delete_cf(cf, META_STATE_HASH_BOOK_WINDOW);
    }
    db.write(batch)?;
    tracing::info!(
        ?on,
        "running state hash: L6.1 window-committed books (from the committed headers)"
    );
    Ok(reset)
}

/// The highest height any part of the stored chain covers (hash, activation
/// record, unverified marker, checkpoints), `None` = no stored chain.
fn stored_chain_head(db: &StateDb, checkpoints: &[u64]) -> Option<u64> {
    [
        read_running_hash(db).map(|(h, _)| h),
        read_activation_height(db),
        read_unverified_since(db),
        checkpoints.iter().copied().max(),
    ]
    .into_iter()
    .flatten()
    .max()
}

/// L6.1: whether window-committed books apply at `height`: `book_window`
/// (the L6.1 activation height, `ChainConfig::book_window_activation`) is
/// set and `height` >= it. Both rules below require it explicitly, so a
/// later L6.1 activation above the hash activation (P0.0) keeps today's
/// preimage and execution below it.
#[inline]
pub fn book_window_active(book_window: Option<u64>, height: u64) -> bool {
    book_window.is_some_and(|a| height >= a)
}

/// L6.1 rule, digest side: whether block `height`'s digest hashes its
/// `cf_native_order_books` (hash id 3) entries. Inactive at `height`
/// ([`book_window_active`]): always, as before L6.1. Active: only at attest
/// heights (`height % STATE_HASH_CHECKPOINT_INTERVAL == 0`), where the
/// block's Classic save wrote every book ([`book_window_full_save_at`]).
#[inline]
pub fn books_hashed_at(book_window: Option<u64>, height: u64) -> bool {
    !book_window_active(book_window, height)
        || height.is_multiple_of(STATE_HASH_CHECKPOINT_INTERVAL)
}

/// L6.1 rule, execution side (CONSENSUS: it changes the state transition,
/// not only the hash): whether block `height` runs the native phase even
/// with nothing else due, and its Classic save writes the blob of every
/// book: L6.1 active at `height` ([`book_window_active`]) and `height` an
/// attest height. The forced native phase runs every block-level step
/// (governance finalization and execution, the liquidation pass, ...), so
/// the caller passes its CHAIN CONFIG's activation
/// (`ChainConfig::book_window_activation`), never a node-local META row.
#[inline]
pub fn book_window_full_save_at(book_window: Option<u64>, height: u64) -> bool {
    book_window_active(book_window, height) && height.is_multiple_of(STATE_HASH_CHECKPOINT_INTERVAL)
}

/// [`books_hashed_at`] for a digest flushing into `db`, with the test-only
/// mutation seam ([`set_book_window_mutant`]) applied.
#[inline]
pub(crate) fn books_hashed_for(db: &StateDb, book_window: Option<u64>, height: u64) -> bool {
    let rule = books_hashed_at(book_window, height);
    #[cfg(feature = "test-probes")]
    {
        if let Some(m) = book_window_mutant(db) {
            return match m {
                BookWindowMutant::HashBooksAt(h) if h == height => true,
                BookWindowMutant::NeverFilter => true,
                BookWindowMutant::SkipAttest(h) if h == height => false,
                _ => rule,
            };
        }
    }
    #[cfg(not(feature = "test-probes"))]
    let _ = db;
    rule
}

/// L6.1 mutation seam (`test-probes`): a deliberately wrong digest rule for
/// flushes into one DB, so the goldens can prove they catch it.
#[cfg(feature = "test-probes")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BookWindowMutant {
    /// Hash the book entries at this (non-attest) height too.
    HashBooksAt(u64),
    /// Hash the book entries at every height (no filter).
    NeverFilter,
    /// Do not hash the book entries at this attest height.
    SkipAttest(u64),
}

#[cfg(feature = "test-probes")]
static BOOK_WINDOW_MUTANTS: Mutex<Vec<(usize, BookWindowMutant)>> = Mutex::new(Vec::new());

/// L6.1 (`test-probes`): arm (`Some`) or clear (`None`) the digest mutant
/// for flushes into `db`. Callers clear it before `db` is dropped.
#[cfg(feature = "test-probes")]
#[doc(hidden)]
pub fn set_book_window_mutant(db: &StateDb, mutant: Option<BookWindowMutant>) {
    let mut m = BOOK_WINDOW_MUTANTS.lock().unwrap();
    let id = db_id(db);
    m.retain(|(d, _)| *d != id);
    if let Some(mutant) = mutant {
        m.push((id, mutant));
    }
}

#[cfg(feature = "test-probes")]
fn book_window_mutant(db: &StateDb) -> Option<BookWindowMutant> {
    let m = BOOK_WINDOW_MUTANTS.lock().unwrap();
    if m.is_empty() {
        return None;
    }
    let id = db_id(db);
    m.iter().find(|(d, _)| *d == id).map(|(_, x)| *x)
}

fn checkpoint_key(height: u64) -> Vec<u8> {
    let mut k = META_STATE_HASH_CHECKPOINT_PREFIX.to_vec();
    k.extend_from_slice(&height.to_be_bytes());
    k
}

/// The local checkpoint hash at `height`, if retained.
pub fn read_checkpoint(db: &StateDb, height: u64) -> Option<[u8; 32]> {
    let v = db.get_cf_raw(CF_CONSENSUS_META, &checkpoint_key(height)).ok().flatten()?;
    v.as_slice().try_into().ok()
}

/// Heights of the retained checkpoints, ascending.
pub fn checkpoint_heights(db: &StateDb) -> Vec<u64> {
    use crate::backend::StateBackend;
    StateBackend::iterate_cf(db, CF_CONSENSUS_META, Some(META_STATE_HASH_CHECKPOINT_PREFIX))
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(k, _)| {
            let h = k.get(META_STATE_HASH_CHECKPOINT_PREFIX.len()..)?;
            Some(u64::from_be_bytes(h.try_into().ok()?))
        })
        .collect()
}

/// What the flush of a height does with the running hash.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChainStep {
    /// Disabled, or below the activation height: nothing hashed.
    Off,
    /// The activation height: chain from `h_{activation-1}` = zeros.
    Start,
    /// Extend the stored `h_{height-1}`.
    Continue([u8; 32]),
    /// No valid `h_{height-1}` (DB above the activation height without a
    /// chain, or a skipped height): hash-unverified, nothing hashed.
    /// `first`: the marker is not recorded yet (this batch records it).
    ///
    /// A failed flush no longer produces a skipped height in consensus
    /// (R01): every failed write of a block's state or applied marker, on
    /// the serial path and on the flush worker, latches the node's fail-stop
    /// before any later height runs, and the restart replays from the
    /// durable marker. The skipped-height case stays as a guard.
    Unverified { first: bool },
}

/// The chain step for flushing `height`. Never continues from anything but
/// the stored hash of exactly `height - 1` of the configured chain.
pub(crate) fn chain_step(db: &StateDb, height: u64) -> ChainStep {
    let Some(activation) = read_configured_activation(db) else {
        return ChainStep::Off;
    };
    if height < activation {
        return ChainStep::Off;
    }
    if height == activation {
        return ChainStep::Start;
    }
    if read_unverified_since(db).is_some() {
        return ChainStep::Unverified { first: false };
    }
    let start = read_activation_height(db);
    match read_running_hash(db) {
        Some((stored, hash)) if start == Some(activation) && stored + 1 == height => {
            ChainStep::Continue(hash)
        }
        stored => {
            tracing::error!(
                height,
                activation,
                stored_height = ?stored.map(|(h, _)| h),
                stored_chain_start = ?start,
                "running state hash: no valid h(height-1) — node is HASH-UNVERIFIED from here on \
                 (no hashes, checkpoints, attestations or fail-stop decisions)"
            );
            ChainStep::Unverified { first: true }
        }
    }
}

/// Item 6 Phase 3: the running-hash chain state [`chain_step`] reads from
/// META, held in memory by the flush worker W so its digests chain from the
/// previous block without a META read (and, with a checkpoint interval,
/// across blocks whose rows are not durable yet). Loaded from META at W's
/// spawn and after every serial barrier; advanced after each durable batch
/// exactly as [`append_to_batch`] changes the rows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ChainMemo {
    configured: Option<u64>,
    start: Option<u64>,
    unverified: Option<u64>,
    prev: Option<(u64, [u8; 32])>,
    /// L6.1: [`read_book_window`] (chain config, fixed after boot).
    book_window: Option<u64>,
}

impl ChainMemo {
    /// The chain state as stored in `db` (the reads [`chain_step`] does).
    pub fn load(db: &StateDb) -> Self {
        Self {
            configured: read_configured_activation(db),
            start: read_activation_height(db),
            unverified: read_unverified_since(db),
            prev: read_running_hash(db),
            book_window: read_book_window(db),
        }
    }

    /// L6.1: the chain config's L6.1 activation height, as loaded.
    pub fn book_window(&self) -> Option<u64> {
        self.book_window
    }

    /// `(height, h_height)` of the last hashed block (= [`read_running_hash`]
    /// once every advanced batch is durable).
    pub fn prev(&self) -> Option<(u64, [u8; 32])> {
        self.prev
    }

    /// [`chain_step`] over this memo instead of META. Same decisions.
    pub(crate) fn step(&self, height: u64) -> ChainStep {
        let Some(activation) = self.configured else {
            return ChainStep::Off;
        };
        if height < activation {
            return ChainStep::Off;
        }
        if height == activation {
            return ChainStep::Start;
        }
        if self.unverified.is_some() {
            return ChainStep::Unverified { first: false };
        }
        match self.prev {
            Some((stored, hash)) if self.start == Some(activation) && stored + 1 == height => {
                ChainStep::Continue(hash)
            }
            stored => {
                tracing::error!(
                    height,
                    activation,
                    stored_height = ?stored.map(|(h, _)| h),
                    stored_chain_start = ?self.start,
                    "running state hash: no valid h(height-1) — node is HASH-UNVERIFIED from here on \
                     (no hashes, checkpoints, attestations or fail-stop decisions)"
                );
                ChainStep::Unverified { first: true }
            }
        }
    }

    /// Apply what [`append_to_batch`] wrote for `height` (call once the batch
    /// is durable).
    pub(crate) fn advance(&mut self, height: u64, step: ChainStep, hash: Option<&[u8; 32]>) {
        match (step, hash) {
            (ChainStep::Unverified { first: true }, _) => self.unverified = Some(height),
            (ChainStep::Start | ChainStep::Continue(_), Some(hash)) => {
                if step == ChainStep::Start {
                    self.unverified = None;
                    self.start = Some(height);
                }
                self.prev = Some((height, *hash));
            }
            _ => {}
        }
    }
}

/// Append the flush's running-hash rows to its atomic batch: `h_height` (+
/// activation record and stale-row cleanup on [`ChainStep::Start`],
/// checkpoint and checkpoint pruning), or the unverified marker.
pub(crate) fn append_to_batch(
    db: &StateDb,
    batch: &mut rocksdb::WriteBatch,
    height: u64,
    step: ChainStep,
    hash: Option<&[u8; 32]>,
) -> Result<(), crate::StateError> {
    let cf = db.cf_handle(CF_CONSENSUS_META)?;
    chain_rows(
        || checkpoint_heights(db),
        height,
        step,
        hash,
        |key, value| match value {
            Some(v) => batch.put_cf(cf, key, v),
            None => batch.delete_cf(cf, key),
        },
    );
    Ok(())
}

/// The running-hash META rows of one flushed height, in batch order:
/// `emit(key, Some(value))` = put, `emit(key, None)` = delete. `stored`
/// lists the retained checkpoint heights (read only on [`ChainStep::Start`]).
/// The one definition behind [`append_to_batch`] and [`ChainRows::record`].
fn chain_rows(
    stored: impl FnOnce() -> Vec<u64>,
    height: u64,
    step: ChainStep,
    hash: Option<&[u8; 32]>,
    mut emit: impl FnMut(&[u8], Option<&[u8]>),
) {
    match (step, hash) {
        (ChainStep::Unverified { first: true }, _) => {
            emit(
                META_RUNNING_STATE_HASH_UNVERIFIED,
                Some(&height.to_be_bytes()),
            );
        }
        (ChainStep::Start | ChainStep::Continue(_), Some(hash)) => {
            if step == ChainStep::Start {
                // A fresh chain: nothing stored belongs to it.
                for h in stored() {
                    emit(&checkpoint_key(h), None);
                }
                emit(META_RUNNING_STATE_HASH_UNVERIFIED, None);
                emit(
                    META_RUNNING_STATE_HASH_ACTIVATION,
                    Some(&height.to_be_bytes()),
                );
            }
            let mut v = Vec::with_capacity(40);
            v.extend_from_slice(&height.to_be_bytes());
            v.extend_from_slice(hash);
            emit(META_RUNNING_STATE_HASH, Some(&v));
            if height % STATE_HASH_CHECKPOINT_INTERVAL == 0 {
                emit(&checkpoint_key(height), Some(hash));
                if let Some(old) =
                    height.checked_sub(STATE_HASH_CHECKPOINT_INTERVAL * STATE_HASH_CHECKPOINT_RETAIN)
                {
                    emit(&checkpoint_key(old), None);
                }
            }
        }
        _ => {}
    }
}

/// Item 6 Phase 3: the running-hash META rows of the digested blocks since
/// the last checkpoint, last value per key (`None` = delete), kept by the
/// flush worker and written in the checkpoint's batch. Recording heights
/// S+1..=T one by one and writing the result over the DB at S leaves the
/// same rows as flushing each height's [`append_to_batch`] in order.
#[derive(Default, Debug, Clone, PartialEq, Eq)]
pub struct ChainRows {
    rows: std::collections::BTreeMap<Vec<u8>, Option<Vec<u8>>>,
}

impl ChainRows {
    /// Fold height `height`'s rows on top. A [`ChainStep::Start`] deletes
    /// every checkpoint row: the DB's and the ones kept here.
    pub(crate) fn record(
        &mut self,
        db: &StateDb,
        height: u64,
        step: ChainStep,
        hash: Option<&[u8; 32]>,
    ) {
        let kept: Vec<u64> = self
            .rows
            .keys()
            .filter_map(|k| {
                let h = k.strip_prefix(META_STATE_HASH_CHECKPOINT_PREFIX)?;
                Some(u64::from_be_bytes(h.try_into().ok()?))
            })
            .collect();
        let rows = &mut self.rows;
        chain_rows(
            || {
                let mut all = checkpoint_heights(db);
                all.extend(kept);
                all.sort_unstable();
                all.dedup();
                all
            },
            height,
            step,
            hash,
            |key, value| {
                rows.insert(key.to_vec(), value.map(<[u8]>::to_vec));
            },
        );
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Append the kept rows to `batch` (`CF_CONSENSUS_META`, key order).
    pub(crate) fn append_to_batch(
        &self,
        db: &StateDb,
        batch: &mut rocksdb::WriteBatch,
    ) -> Result<(), crate::StateError> {
        let cf = db.cf_handle(CF_CONSENSUS_META)?;
        for (key, value) in &self.rows {
            match value {
                Some(v) => batch.put_cf(cf, key, v),
                None => batch.delete_cf(cf, key),
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Test capture hook (cross-crate determinism tests). Off unless a test arms it:
// one relaxed atomic load per flush in production.
// ---------------------------------------------------------------------------

type Captured = Vec<(u64, Vec<WriteEntry>)>;

static CAPTURE_ACTIVE: AtomicBool = AtomicBool::new(false);
static CAPTURE: Mutex<Vec<(usize, Captured)>> = Mutex::new(Vec::new());

fn db_id(db: &StateDb) -> usize {
    db.inner() as *const rocksdb::DB as usize
}

/// Start recording, per flushed height, the consensus write set every hashed
/// flush into `db` sees. Test hook.
#[doc(hidden)]
pub fn capture_begin(db: &StateDb) {
    let mut c = CAPTURE.lock().unwrap();
    let id = db_id(db);
    c.retain(|(d, _)| *d != id);
    c.push((id, Vec::new()));
    CAPTURE_ACTIVE.store(true, Ordering::SeqCst);
}

/// Stop recording for `db` and return what was captured. Test hook.
#[doc(hidden)]
pub fn capture_take(db: &StateDb) -> Vec<(u64, Vec<WriteEntry>)> {
    let mut c = CAPTURE.lock().unwrap();
    let id = db_id(db);
    let pos = c.iter().position(|(d, _)| *d == id);
    let out = pos.map(|p| c.swap_remove(p).1).unwrap_or_default();
    if c.is_empty() {
        CAPTURE_ACTIVE.store(false, Ordering::SeqCst);
    }
    out
}

/// Item 7 option 0 step 0 (`test-probes`): take what was captured for `db`
/// so far and KEEP recording (no gap between a take and the next begin, as
/// `capture_take` + `capture_begin` would leave while the flush worker hashes
/// on its own thread). Test hook.
#[cfg(feature = "test-probes")]
#[doc(hidden)]
pub fn capture_drain(db: &StateDb) -> Vec<(u64, Vec<WriteEntry>)> {
    let mut c = CAPTURE.lock().unwrap();
    let id = db_id(db);
    c.iter_mut()
        .find(|(d, _)| *d == id)
        .map(|(_, v)| std::mem::take(v))
        .unwrap_or_default()
}

pub(crate) fn capture_active() -> bool {
    CAPTURE_ACTIVE.load(Ordering::Relaxed)
}

pub(crate) fn capture_record(db: &StateDb, height: u64, writes: Vec<WriteEntry>) {
    let id = db_id(db);
    let mut c = CAPTURE.lock().unwrap();
    if let Some((_, v)) = c.iter_mut().find(|(d, _)| *d == id) {
        v.push((height, writes));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn running_hash_cf_table_is_frozen_unique_and_registered() {
        let mut ids = std::collections::BTreeSet::new();
        for (id, cf) in HASHED_CFS {
            assert!(ids.insert(*id), "duplicate cf_id {id}");
            assert!(ALL_CF_NAMES.contains(cf), "{cf} is not a registered CF");
        }
        for derived in [
            CF_BLOCK_HEADERS,
            CF_BLOCK_BODIES,
            CF_COMMIT_MANIFEST,
            CF_RECEIPTS,
            CF_BOOK_ORDER_ROWS,
            CF_NATIVE_TRADES,
            CF_NATIVE_USER_TRADES,
            CF_NATIVE_PENDING,
            CF_NATIVE_TRIE,
            CF_NATIVE_HASHED,
            CF_HASHED_ACCOUNTS,
            CF_TRIE_ACCOUNTS,
            CF_NATIVE_BBO,
        ] {
            assert_eq!(hashed_cf_id(derived), None, "{derived} must not be hashed");
        }
        // Consensus bug (b) follow-up: validator rows are written only by
        // execution, inside the flush batch: hashed under id 8.
        assert_eq!(hashed_cf_id(CF_STAKING_VALIDATORS), Some(8));
        // Every native-root CF is consensus state: hashed (rebase s87).
        for (cf, _) in crate::native_trie::NATIVE_ROOT_CFS {
            assert!(hashed_cf_id(cf).is_some(), "native-root CF {cf} must be hashed");
        }
        assert_eq!(hashed_cf_id(CF_NATIVE_LIQUIDATION), Some(23));
    }

    fn golden_writes() -> Vec<WriteEntry> {
        vec![
            (1, b"pos-a".to_vec(), Some(b"v1".to_vec())),
            (6, b"nonce-key".to_vec(), None), // tombstone
            (21, b"pending_rotation:x".to_vec(), Some(b"r".to_vec())),
            // excluded by the key filter: must not change the hash
            (21, META_NATIVE_APPLIED_HEIGHT.to_vec(), Some(100u64.to_be_bytes().to_vec())),
            (4, b"__book_mode__".to_vec(), Some(vec![3])),
            // EVM domain
            (0x80, vec![0xaa; 20], Some(b"acct".to_vec())),
            (0x81, b"slot".to_vec(), Some(vec![1; 32])),
        ]
    }

    fn hex(b: &[u8; 32]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    /// Golden vector, computed independently (Python hashlib) from the spec:
    /// h_n = SHA-256(h_{n-1} ‖ n u64 BE ‖ D_native ‖ D_evm), D_x = SHA-256 over
    /// cf_id ‖ len(key) u32 BE ‖ key ‖ (0x00 | 0x01 ‖ len(value) u32 BE ‖ value).
    /// P0.0 phase 3: the first height whose header carries the bit, from
    /// any later height, reading few headers (parent first, then a binary
    /// search); `None` while the bit is off; a read error propagates.
    #[test]
    fn header_activation_is_the_first_height_with_the_bit() {
        assert_eq!(header_activation::<()>(7, false, |_| panic!("no read")), Ok(None));
        assert_eq!(header_activation::<()>(1, true, |_| panic!("no read")), Ok(Some(1)));
        for first in 1..=40u64 {
            for height in first..=40 {
                let mut reads = 0;
                let got = header_activation::<()>(height, true, |h| {
                    assert!(h < height && h >= 1, "reads below {height} only: {h}");
                    reads += 1;
                    Ok(h >= first)
                });
                assert_eq!(got, Ok(Some(first)), "first {first}, at {height}");
                assert!(reads <= 1 + 6, "first {first}, at {height}: {reads} reads");
                if height == first {
                    assert!(reads <= 1, "the activation height reads only its parent");
                }
            }
        }
        assert_eq!(header_activation(9, true, |h| if h == 8 { Ok(true) } else { Err(h) }), Err(4));
    }

    #[test]
    fn running_hash_golden_vector() {
        let mut w = golden_writes();
        w.sort_by(|a, b| (a.0, &a.1).cmp(&(b.0, &b.1)));
        let h = next_running_hash(&[0x11; 32], 100, &w);
        assert_eq!(hex(&h), "186ec6603996d1d00398948fde70c2d9b7e295cf09d1a3661d52a4af7af311c6");
        // Empty block from the zero hash: both domains are SHA-256("").
        let e = next_running_hash(&[0; 32], 1, &[]);
        assert_eq!(hex(&e), "09886290c37994a0d854cf970cb95cb3e6cd9440fa19bed923c3ea7d1d340c43");
    }

    /// Any byte change in any input changes the hash; excluded keys do not.
    #[test]
    fn running_hash_every_byte_matters() {
        let mut base = golden_writes();
        base.sort_by(|a, b| (a.0, &a.1).cmp(&(b.0, &b.1)));
        let h0 = next_running_hash(&[0x11; 32], 100, &base);
        assert_ne!(next_running_hash(&[0x12; 32], 100, &base), h0, "prev");
        assert_ne!(next_running_hash(&[0x11; 32], 101, &base), h0, "height");
        let hashed: Vec<usize> = (0..base.len())
            .filter(|&i| key_is_hashed(base[i].0, &base[i].1))
            .collect();
        for &i in &hashed {
            let mut w = base.clone();
            w[i].1.push(0);
            assert_ne!(next_running_hash(&[0x11; 32], 100, &w), h0, "key of entry {i}");
            let mut w = base.clone();
            w[i].2 = match &w[i].2 {
                Some(v) if !v.is_empty() => {
                    let mut v = v.clone();
                    v[0] ^= 1;
                    Some(v)
                }
                Some(_) => None,
                None => Some(Vec::new()), // tombstone vs empty value differ
            };
            assert_ne!(next_running_hash(&[0x11; 32], 100, &w), h0, "value of entry {i}");
            let mut w = base.clone();
            w.remove(i);
            assert_ne!(next_running_hash(&[0x11; 32], 100, &w), h0, "dropping entry {i}");
        }
        // Moving a write between domains (same key/value, other cf_id) differs.
        let mut w = base.clone();
        let evm = w.iter().position(|e| e.0 == 0x80).unwrap();
        w[evm].0 = 0x7f;
        w.sort_by(|a, b| (a.0, &a.1).cmp(&(b.0, &b.1)));
        assert_ne!(next_running_hash(&[0x11; 32], 100, &w), h0, "domain split");
        // Excluded keys: changing or dropping them leaves the hash alone.
        for i in (0..base.len()).filter(|i| !hashed.contains(i)) {
            let mut w = base.clone();
            w[i].2 = Some(b"other".to_vec());
            assert_eq!(next_running_hash(&[0x11; 32], 100, &w), h0, "excluded entry {i}");
            let mut w = base.clone();
            w.remove(i);
            assert_eq!(next_running_hash(&[0x11; 32], 100, &w), h0, "excluded entry {i} dropped");
        }
    }

    #[test]
    fn running_hash_key_filters() {
        let markets = hashed_cf_id(CF_NATIVE_MARKETS).unwrap();
        assert!(!key_is_hashed(markets, b"__book_mode__"));
        assert!(key_is_hashed(markets, b"__next_global_order_id__"));
        assert_eq!(hashed_cf_id(CF_STATE_HASH_VOTES), Some(22), "attestation votes are consensus state");
        assert!(key_is_hashed(markets, &7u64.to_be_bytes()));
        let meta = hashed_cf_id(CF_CONSENSUS_META).unwrap();
        assert!(key_is_hashed(meta, b"pending_rotation:\x01"));
        assert!(key_is_hashed(meta, b"validator_whitelist:\x01"));
        assert!(!key_is_hashed(meta, META_NATIVE_APPLIED_HEIGHT));
        assert!(!key_is_hashed(meta, META_NATIVE_TRIE_STALE));
        assert!(!key_is_hashed(meta, b"highest_view_phase_voted"));
    }
}
