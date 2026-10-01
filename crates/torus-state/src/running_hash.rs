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
/// (`governance.rs` `whitelist_key`).
pub const META_CONSENSUS_PREFIXES: &[&[u8]] = &[b"pending_rotation:", b"validator_whitelist:"];

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
        assert!(key_is_hashed(markets, &7u64.to_be_bytes()));
        let meta = hashed_cf_id(CF_CONSENSUS_META).unwrap();
        assert!(key_is_hashed(meta, b"pending_rotation:\x01"));
        assert!(key_is_hashed(meta, b"validator_whitelist:\x01"));
        assert!(!key_is_hashed(meta, META_NATIVE_APPLIED_HEIGHT));
        assert!(!key_is_hashed(meta, META_NATIVE_TRIE_STALE));
        assert!(!key_is_hashed(meta, b"highest_view_phase_voted"));
    }
}
