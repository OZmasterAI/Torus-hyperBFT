//! Running state hash (docs/plans/running-state-hash-impl.md).
//!
//! Every block's logical change set to the CONSENSUS column families
//! (consensus key -> new value or deletion) is folded into a chained hash.
//! This module owns the frozen classification of which `(cf, key)` pairs are
//! consensus state and the numbering they are hashed under.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

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
