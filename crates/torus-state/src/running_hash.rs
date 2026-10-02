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
/// Also excluded (consensus state, but not deterministic per height — tied to
/// the pre-existing bug "epoch rotation on the consensus thread"):
/// `cf_staking_validators` (id 8, RESERVED — never reuse). The consensus
/// thread read-modify-writes whole validator rows (status, rotated pubkey)
/// at proposal / validation time of an epoch boundary block, while the
/// execution thread may lag by any number of blocks; every execution-time
/// write of a validator row (commission, stake, jail, slash) therefore
/// carries consensus-thread fields whose visibility depends on timing. Re-add
/// it (new id) once epoch rotation runs at execution of the boundary block.
pub const HASHED_CFS: &[(u8, &str)] = &[
    (1, CF_NATIVE_POSITIONS),
    (2, CF_NATIVE_BALANCES),
    (3, CF_NATIVE_ORDER_BOOKS),
    (4, CF_NATIVE_MARKETS),
    (5, CF_NATIVE_ORACLE),
    (6, CF_NATIVE_NONCES),
    (7, CF_NATIVE_ORDERS),
    // 8: cf_staking_validators — excluded, id reserved (see above).
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
/// (`governance.rs` `whitelist_key`). Only EXECUTION writes of them are
/// hashed: the consensus thread's epoch-boundary deletion of applied
/// rotations is out of batch and timing-dependent (see [`HASHED_CFS`]), so
/// it is not. `epoch_vset:` (consensus bug (b): execution-computed validator
/// set plans, `torus_economics::EPOCH_VSET_PREFIX`) exists only from the
/// chain-wide epoch-rotation activation on, so earlier hashes are unchanged.
pub const META_CONSENSUS_PREFIXES: &[&[u8]] =
    &[b"pending_rotation:", b"validator_whitelist:", b"epoch_vset:"];

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

/// The CHAIN-WIDE activation height this node is configured with (genesis
/// `consensus.state_hash_activation_height`); `None` = running hash disabled.
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

/// Apply the chain config at boot (before any block is flushed): store the
/// CHAIN-WIDE activation height (`None` = disabled; `Some(0)` is treated as
/// 1, the first executable height). When the configuration differs from the
/// one the stored chain was built under, the stored chain (hash, activation
/// record, unverified marker, checkpoints) is discarded: it belongs to
/// another chain definition. Returns whether a stored chain was discarded.
pub fn configure_activation(db: &StateDb, activation: Option<u64>) -> Result<bool, crate::StateError> {
    let activation = activation.map(|a| a.max(1));
    let previous = read_configured_activation(db);
    let start = read_activation_height(db);
    let checkpoints = checkpoint_heights(db);
    let has_chain = start.is_some()
        || read_running_hash(db).is_some()
        || read_unverified_since(db).is_some()
        || !checkpoints.is_empty();
    let reset = has_chain && (previous != activation || start.is_some_and(|s| Some(s) != activation));
    let cf = db.cf_handle(CF_CONSENSUS_META)?;
    let mut batch = rocksdb::WriteBatch::default();
    if reset {
        for key in [
            META_RUNNING_STATE_HASH,
            META_RUNNING_STATE_HASH_ACTIVATION,
            META_RUNNING_STATE_HASH_UNVERIFIED,
        ] {
            batch.delete_cf(cf, key);
        }
        for h in &checkpoints {
            batch.delete_cf(cf, checkpoint_key(*h));
        }
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
    tracing::info!(?activation, "running state hash activation (chain config)");
    Ok(reset)
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
    match (step, hash) {
        (ChainStep::Unverified { first: true }, _) => {
            batch.put_cf(cf, META_RUNNING_STATE_HASH_UNVERIFIED, height.to_be_bytes());
        }
        (ChainStep::Start | ChainStep::Continue(_), Some(hash)) => {
            if step == ChainStep::Start {
                // A fresh chain: nothing stored belongs to it.
                for h in checkpoint_heights(db) {
                    batch.delete_cf(cf, checkpoint_key(h));
                }
                batch.delete_cf(cf, META_RUNNING_STATE_HASH_UNVERIFIED);
                batch.put_cf(cf, META_RUNNING_STATE_HASH_ACTIVATION, height.to_be_bytes());
            }
            let mut v = Vec::with_capacity(40);
            v.extend_from_slice(&height.to_be_bytes());
            v.extend_from_slice(hash);
            batch.put_cf(cf, META_RUNNING_STATE_HASH, v);
            if height % STATE_HASH_CHECKPOINT_INTERVAL == 0 {
                batch.put_cf(cf, checkpoint_key(height), hash);
                if let Some(old) =
                    height.checked_sub(STATE_HASH_CHECKPOINT_INTERVAL * STATE_HASH_CHECKPOINT_RETAIN)
                {
                    batch.delete_cf(cf, checkpoint_key(old));
                }
            }
        }
        _ => {}
    }
    Ok(())
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
        ] {
            assert_eq!(hashed_cf_id(derived), None, "{derived} must not be hashed");
        }
        // Review finding 2: validator rows are read-modify-written by the
        // consensus thread at epoch boundaries (timing-dependent): excluded,
        // and their id 8 is never reused.
        assert_eq!(hashed_cf_id(CF_STAKING_VALIDATORS), None);
        assert!(!ids.contains(&8), "cf_id 8 is reserved");
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
