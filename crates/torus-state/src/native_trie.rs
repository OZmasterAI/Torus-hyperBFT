//! Native incremental state root via a bucketed Merkle tree (Phase A, Stage A2).
//!
//! The native root commits the 6 consensus-authoritative native CFs. The flat keccak it replaces
//! (`torus-bridge::state_root::compute_native_state_root`) is O(total) and fundamentally
//! non-incremental (keccak streams the whole state), so this introduces a keyed Merkle structure.
//!
//! Structure (see `docs/plans/incremental-state-root-native-impl.md`): each `(cf, key) -> value`
//! native entry is placed in one of `NUM_BUCKETS` buckets by `keccak(cf_tag ‖ key)[0..2]`. A bucket's
//! hash is the keccak of its entries' length-framed bytes in `(cf_tag, key)` order — reusing the
//! proven framing of the flat keccak it replaces. A complete depth-`TREE_DEPTH` binary tree over the
//! buckets (with default-node compression) yields the root. Only changed buckets + their tree paths
//! are touched per block, so the root is O(changed).
//!
//! Determinism is #1: [`native_root_full`] (full O(total) recompute) is the oracle; the persisted
//! incremental root must equal it byte-for-byte, every block (the A2 gate). Both go through the same
//! [`build_tree`] so they cannot diverge.

use std::collections::{BTreeMap, BTreeSet};

use alloy_primitives::{keccak256, B256};
use rocksdb::WriteBatch;

use crate::cf::{
    CF_NATIVE_BALANCES, CF_NATIVE_HASHED, CF_NATIVE_ORACLE, CF_NATIVE_ORDER_BOOKS,
    CF_NATIVE_POSITIONS, CF_NATIVE_TRIE, CF_STAKING_DELEGATIONS, CF_STAKING_VALIDATORS,
};
use crate::db::StateDb;
use crate::error::StateError;
use crate::trie::EMPTY_ROOT_HASH;

/// Tree depth in bits = number of buckets is `1 << TREE_DEPTH`. 16 bits → 65536 buckets.
pub const TREE_DEPTH: usize = 16;

/// The 6 authoritative native-root CFs and their **FROZEN** 1-byte tags. NEVER reorder or renumber:
/// the `cf_tag` is part of the consensus root preimage (bucket assignment + leaf framing). The set
/// must stay equal to the CFs hashed by `compute_native_state_root` (the consensus-authoritative
/// full scan).
pub const NATIVE_ROOT_CFS: [(&str, u8); 6] = [
    (CF_NATIVE_BALANCES, 0),
    (CF_NATIVE_ORDER_BOOKS, 1),
    (CF_NATIVE_POSITIONS, 2),
    (CF_NATIVE_ORACLE, 3),
    (CF_STAKING_DELEGATIONS, 4),
    (CF_STAKING_VALIDATORS, 5),
];

/// `cf_tag` for a CF name, or `None` if it is not one of the 6 native-root CFs.
pub fn cf_tag(cf_name: &str) -> Option<u8> {
    NATIVE_ROOT_CFS.iter().find(|(n, _)| *n == cf_name).map(|(_, t)| *t)
}

// CF_NATIVE_TRIE key prefixes.
const NODE_PREFIX_LEAF: u8 = 0x00; // 0x00 ‖ bucket(2 BE)
const NODE_PREFIX_INTERNAL: u8 = 0x01; // 0x01 ‖ level(1) ‖ index(2 BE)
const ROOT_KEY: [u8; 1] = [0x02]; // persisted native root marker

/// Sentinel hash of an empty bucket / empty leaf (domain-separated so it can't be a real bucket hash).
fn empty_leaf() -> B256 {
    keccak256(b"torus.native.bucket.empty")
}

/// `keccak256(left ‖ right)` — the internal-node combiner.
fn hash_pair(left: &B256, right: &B256) -> B256 {
    let mut buf = [0u8; 64];
    buf[..32].copy_from_slice(left.as_slice());
    buf[32..].copy_from_slice(right.as_slice());
    keccak256(buf)
}

/// `default[L]` = hash of an all-empty subtree rooted at level `L` (index by level, `0..=TREE_DEPTH`).
/// `default[TREE_DEPTH]` is the empty leaf; each level up hashes two copies of the level below.
fn default_nodes() -> [B256; TREE_DEPTH + 1] {
    let mut d = [B256::ZERO; TREE_DEPTH + 1];
    d[TREE_DEPTH] = empty_leaf();
    let mut level = TREE_DEPTH;
    while level > 0 {
        d[level - 1] = hash_pair(&d[level], &d[level]);
        level -= 1;
    }
    d
}

/// Bucket for `(cf_tag, key)` — the top `TREE_DEPTH` bits of `keccak(cf_tag ‖ key)`, big-endian.
/// Hashing the key gives a uniform bucket distribution regardless of native key layout (no hot
/// buckets from clustered addresses / sequential ids).
fn bucket_id(tag: u8, key: &[u8]) -> u16 {
    let mut input = Vec::with_capacity(1 + key.len());
    input.push(tag);
    input.extend_from_slice(key);
    let h = keccak256(&input);
    u16::from_be_bytes([h[0], h[1]])
}

/// Append one entry's canonical length-framed bytes to a bucket accumulator:
/// `cf_tag(1) ‖ key.len()(u32 LE) ‖ key ‖ value.len()(u32 LE) ‖ value`.
fn frame_entry(out: &mut Vec<u8>, tag: u8, key: &[u8], value: &[u8]) {
    out.push(tag);
    out.extend_from_slice(&(key.len() as u32).to_le_bytes());
    out.extend_from_slice(key);
    out.extend_from_slice(&(value.len() as u32).to_le_bytes());
    out.extend_from_slice(value);
}

/// Mirror key in `CF_NATIVE_HASHED`: `bucket(2 BE) ‖ cf_tag(1) ‖ native_key`. A prefix scan on the
/// 2-byte bucket yields the bucket's members in `(cf_tag, key)` order — the canonical hash order.
fn mirror_key(bucket: u16, tag: u8, key: &[u8]) -> Vec<u8> {
    let mut k = Vec::with_capacity(3 + key.len());
    k.extend_from_slice(&bucket.to_be_bytes());
    k.push(tag);
    k.extend_from_slice(key);
    k
}

fn node_key_leaf(bucket: u16) -> [u8; 3] {
    let mut k = [0u8; 3];
    k[0] = NODE_PREFIX_LEAF;
    k[1..].copy_from_slice(&bucket.to_be_bytes());
    k
}

fn node_key_internal(level: usize, index: usize) -> [u8; 4] {
    let mut k = [0u8; 4];
    k[0] = NODE_PREFIX_INTERNAL;
    k[1] = level as u8;
    k[2..].copy_from_slice(&(index as u16).to_be_bytes());
    k
}

/// Map the raw top node (`node[0][0]`) to the canonical native root: an entirely-empty tree
/// (`node00 == default[0]`) is reported as `EMPTY_ROOT_HASH`, matching the EVM half's empty
/// convention and the flat keccak this replaces.
fn finalize_root(node00: B256, defaults: &[B256; TREE_DEPTH + 1]) -> B256 {
    if node00 == defaults[0] {
        EMPTY_ROOT_HASH
    } else {
        node00
    }
}

/// Build the complete bucketed tree from the set of non-empty leaf hashes. Returns the raw top node
/// `node[0][0]` and **all non-default nodes** (leaves + internals) keyed by their `CF_NATIVE_TRIE`
/// key. Single source of truth for both the full scan and the persisted build — they cannot diverge.
fn build_tree(
    leaves: &BTreeMap<u16, B256>,
    defaults: &[B256; TREE_DEPTH + 1],
) -> (B256, Vec<([u8; 4], B256)>) {
    // We collect node (key, hash) pairs. Leaf keys are 3 bytes; pad to a 4-byte buffer with a
    // length marker so leaves and internals share one Vec type without allocation.
    let mut nodes: Vec<([u8; 4], B256)> = Vec::new();
    for (b, h) in leaves {
        let lk = node_key_leaf(*b);
        nodes.push(([lk[0], lk[1], lk[2], 0xFF], *h)); // 0xFF tail marks "3-byte leaf key"
    }

    let mut level_nodes: BTreeMap<usize, B256> =
        leaves.iter().map(|(b, h)| (*b as usize, *h)).collect();
    for level in (0..TREE_DEPTH).rev() {
        let child_default = defaults[level + 1];
        let parents: BTreeSet<usize> = level_nodes.keys().map(|i| i / 2).collect();
        let mut next = BTreeMap::new();
        for p in parents {
            let left = level_nodes.get(&(2 * p)).copied().unwrap_or(child_default);
            let right = level_nodes.get(&(2 * p + 1)).copied().unwrap_or(child_default);
            let h = hash_pair(&left, &right);
            nodes.push((node_key_internal(level, p), h));
            next.insert(p, h);
        }
        level_nodes = next;
    }
    let node00 = level_nodes.get(&0).copied().unwrap_or(defaults[0]);
    (node00, nodes)
}

/// Write a `build_tree` node into `batch`, decoding the leaf vs internal key form.
fn put_node(batch: &mut WriteBatch, trie_cf: &rocksdb::ColumnFamily, key: &[u8; 4], hash: &B256) {
    if key[3] == 0xFF && key[0] == NODE_PREFIX_LEAF {
        batch.put_cf(trie_cf, &key[..3], hash.as_slice());
    } else {
        batch.put_cf(trie_cf, &key[..], hash.as_slice());
    }
}

/// Collect the non-empty bucket leaf hashes from the live 6 native CFs (pure; no persisted trie).
/// Entries accumulate in `(cf_tag, key)` order because the CFs are visited in tag order and each CF
/// iterates in key order — the canonical bucket-hash input.
fn leaves_from_db(db: &StateDb) -> Result<BTreeMap<u16, B256>, StateError> {
    let mut acc: BTreeMap<u16, Vec<u8>> = BTreeMap::new();
    for (cf_name, tag) in NATIVE_ROOT_CFS {
        let Some(cf) = db.inner().cf_handle(cf_name) else { continue };
        let iter = db.inner().iterator_cf(cf, rocksdb::IteratorMode::Start);
        for item in iter {
            let (key, value) = item?;
            let b = bucket_id(tag, &key);
            frame_entry(acc.entry(b).or_default(), tag, &key, &value);
        }
    }
    Ok(acc.into_iter().map(|(b, data)| (b, keccak256(&data))).collect())
}

/// Full-scan native root — the determinism ORACLE and the flag-off path. O(total native state):
/// scans the 6 CFs, buckets every entry, builds the tree. Does NOT touch the persisted trie/mirror.
pub fn native_root_full(db: &StateDb) -> Result<B256, StateError> {
    let leaves = leaves_from_db(db)?;
    let defaults = default_nodes();
    let (node00, _nodes) = build_tree(&leaves, &defaults);
    Ok(finalize_root(node00, &defaults))
}

/// Delete every entry of `cf_name` into `batch` (a from-scratch rebuild wipes prior nodes first so
/// the persisted node set is a pure function of current state — idempotent).
fn clear_cf(batch: &mut WriteBatch, db: &StateDb, cf_name: &str) -> Result<(), StateError> {
    let cf = db.cf_handle(cf_name)?;
    let iter = db.inner().iterator_cf(cf, rocksdb::IteratorMode::Start);
    for item in iter {
        let (k, _) = item?;
        batch.delete_cf(cf, k);
    }
    Ok(())
}

/// One-time migration: build the bucket-ordered mirror (`CF_NATIVE_HASHED`) and the persisted tree
/// (`CF_NATIVE_TRIE`) from the current native state, returning the root (== [`native_root_full`]).
/// Idempotent: clears both CFs first, so a rebuild reproduces the same root AND node set.
pub fn build_native_trie_to_cf(db: &StateDb) -> Result<B256, StateError> {
    let mut batch = WriteBatch::default();
    // Wipe any prior mirror + trie first (deletes precede puts in the batch, so puts win on overlap).
    clear_cf(&mut batch, db, CF_NATIVE_HASHED)?;
    clear_cf(&mut batch, db, CF_NATIVE_TRIE)?;
    let mirror_cf = db.cf_handle(CF_NATIVE_HASHED)?;
    let trie_cf = db.cf_handle(CF_NATIVE_TRIE)?;

    // Mirror every entry and accumulate bucket inputs in one pass.
    let mut acc: BTreeMap<u16, Vec<u8>> = BTreeMap::new();
    for (cf_name, tag) in NATIVE_ROOT_CFS {
        let Some(cf) = db.inner().cf_handle(cf_name) else { continue };
        let iter = db.inner().iterator_cf(cf, rocksdb::IteratorMode::Start);
        for item in iter {
            let (key, value) = item?;
            let b = bucket_id(tag, &key);
            batch.put_cf(mirror_cf, mirror_key(b, tag, &key), &value);
            frame_entry(acc.entry(b).or_default(), tag, &key, &value);
        }
    }
    let leaves: BTreeMap<u16, B256> =
        acc.into_iter().map(|(b, data)| (b, keccak256(&data))).collect();

    let defaults = default_nodes();
    let (node00, nodes) = build_tree(&leaves, &defaults);
    for (k, h) in &nodes {
        put_node(&mut batch, trie_cf, k, h);
    }
    let root = finalize_root(node00, &defaults);
    batch.put_cf(trie_cf, ROOT_KEY, root.as_slice());
    db.write(batch)?;
    Ok(root)
}

/// Ensure the native trie + mirror exist, building them once if absent (idempotent boot helper).
/// Returns `true` iff a build was performed. The trie CF always holds at least the root marker once
/// built (even for empty native state), so its emptiness is the reliable "not yet built" signal.
pub fn ensure_native_trie_built(db: &StateDb) -> Result<bool, StateError> {
    let cf = db.cf_handle(CF_NATIVE_TRIE)?;
    let mut iter = db.inner().raw_iterator_cf(cf);
    iter.seek_to_first();
    let built = iter.valid();
    iter.status()?;
    if built {
        return Ok(false);
    }
    build_native_trie_to_cf(db)?;
    Ok(true)
}

/// Read the persisted native root marker (the incrementally-maintained root). Returns
/// `EMPTY_ROOT_HASH` if the trie has not been built yet.
pub fn persisted_native_root(db: &StateDb) -> Result<B256, StateError> {
    let cf = db.cf_handle(CF_NATIVE_TRIE)?;
    match db.inner().get_cf(cf, ROOT_KEY)? {
        Some(v) if v.len() == 32 => Ok(B256::from_slice(&v)),
        Some(_) => Err(StateError::InvalidData("native root marker has wrong length".into())),
        None => Ok(EMPTY_ROOT_HASH),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::StateDb;

    fn temp_db() -> (StateDb, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        (StateDb::open(dir.path()).expect("open db"), dir)
    }

    /// Seed a varied corpus across all 6 native-root CFs (varied key/value lengths, enough entries
    /// to populate many buckets and force real internal branch nodes).
    fn seed(db: &StateDb) {
        for (tag, (cf_name, _)) in NATIVE_ROOT_CFS.iter().enumerate().map(|(i, c)| (i, c)) {
            for i in 0u32..150 {
                let mut key = vec![tag as u8];
                key.extend_from_slice(&i.to_be_bytes());
                if i % 3 == 0 {
                    key.push(0xAB); // vary key length
                }
                let mut val = vec![0x5au8; 8 + (i as usize % 40)];
                val[0] = tag as u8;
                val[1..5].copy_from_slice(&i.to_le_bytes());
                db.put_cf_raw(cf_name, &key, &val).unwrap();
            }
        }
    }

    /// A2.1 determinism gate: the persisted bucketed root must equal the full-scan oracle over a
    /// corpus, be idempotent, and actually persist nodes.
    #[test]
    fn native_trie_root_matches_full_scan() {
        let (db, _dir) = temp_db();
        seed(&db);

        let full = native_root_full(&db).unwrap();
        assert_ne!(full, EMPTY_ROOT_HASH, "seeded corpus must be non-empty");

        let built = build_native_trie_to_cf(&db).unwrap();
        assert_eq!(built, full, "built native trie root must equal the full scan");
        assert_eq!(persisted_native_root(&db).unwrap(), full, "persisted marker must match");

        // Idempotent: rebuilding reproduces the same root.
        let again = build_native_trie_to_cf(&db).unwrap();
        assert_eq!(again, full, "rebuild must be idempotent");

        // Nodes were actually persisted.
        let cf = db.cf_handle(CF_NATIVE_TRIE).unwrap();
        let mut it = db.inner().raw_iterator_cf(cf);
        it.seek_to_first();
        assert!(it.valid(), "native trie CF should be non-empty after build");
    }

    #[test]
    fn native_trie_empty_state_is_empty_root() {
        let (db, _dir) = temp_db();
        assert_eq!(native_root_full(&db).unwrap(), EMPTY_ROOT_HASH);
        let built = build_native_trie_to_cf(&db).unwrap();
        assert_eq!(built, EMPTY_ROOT_HASH, "empty native state => EMPTY_ROOT_HASH");
        assert_eq!(persisted_native_root(&db).unwrap(), EMPTY_ROOT_HASH);
    }

    #[test]
    fn native_root_changes_with_state() {
        let (db, _dir) = temp_db();
        seed(&db);
        let r1 = native_root_full(&db).unwrap();
        db.put_cf_raw(CF_NATIVE_BALANCES, b"\x00\x00\x00\x00new", b"v").unwrap();
        let r2 = native_root_full(&db).unwrap();
        assert_ne!(r1, r2, "native root must change when state changes");
    }

    #[test]
    fn ensure_native_trie_built_is_one_shot() {
        let (db, _dir) = temp_db();
        seed(&db);
        assert!(ensure_native_trie_built(&db).unwrap(), "first call builds");
        assert!(!ensure_native_trie_built(&db).unwrap(), "second call is a no-op");
        assert_eq!(persisted_native_root(&db).unwrap(), native_root_full(&db).unwrap());
    }
}
