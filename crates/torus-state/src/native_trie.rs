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
//! HASH-ONLY MIRROR (3c batched preimage round, design inherited from
//! `docs/plans/hash-only-native-mirror.md`): the mirror (`CF_NATIVE_HASHED`) stores
//! `keccak256(value)` (32 B) instead of the full value, and bucket leaves are defined over the
//! per-entry VALUE HASHES: `leaf = keccak(frame(cf_tag, key, value_hash) ...)`. This is a
//! CONSENSUS-VISIBLE preimage change (all 6 CFs, every mode): fleet-uniform, fresh genesis /
//! offline mirror rebuild required. `bucket_id`, tree shape, node keys and defaults are unchanged.
//!
//! Determinism is #1: [`native_root_full`] (full O(total) recompute) is the oracle; the persisted
//! incremental root must equal it byte-for-byte, every block (the A2 gate). Both go through the same
//! [`build_tree`] so they cannot diverge.

use std::borrow::Borrow;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

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
    NATIVE_ROOT_CFS
        .iter()
        .find(|(n, _)| *n == cf_name)
        .map(|(_, t)| *t)
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
/// The mirror VALUE is `keccak256(native value)` (32 B) — hash-only mirror, see module docs.
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
            let right = level_nodes
                .get(&(2 * p + 1))
                .copied()
                .unwrap_or(child_default);
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
        let Some(cf) = db.inner().cf_handle(cf_name) else {
            continue;
        };
        let iter = db.inner().iterator_cf(cf, rocksdb::IteratorMode::Start);
        for item in iter {
            let (key, value) = item?;
            let b = bucket_id(tag, &key);
            // Hash-only mirror: leaves are defined over per-entry value hashes.
            frame_entry(acc.entry(b).or_default(), tag, &key, keccak256(&value).as_slice());
        }
    }
    Ok(acc
        .into_iter()
        .map(|(b, data)| (b, keccak256(&data)))
        .collect())
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
        let Some(cf) = db.inner().cf_handle(cf_name) else {
            continue;
        };
        let iter = db.inner().iterator_cf(cf, rocksdb::IteratorMode::Start);
        for item in iter {
            let (key, value) = item?;
            let b = bucket_id(tag, &key);
            // Hash-only mirror: store keccak(value), frame the hash into the leaf preimage.
            let vh = keccak256(&value);
            batch.put_cf(mirror_cf, mirror_key(b, tag, &key), vh.as_slice());
            frame_entry(acc.entry(b).or_default(), tag, &key, vh.as_slice());
        }
    }
    let leaves: BTreeMap<u16, B256> = acc
        .into_iter()
        .map(|(b, data)| (b, keccak256(&data)))
        .collect();

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
    if is_native_trie_built(db)? {
        return Ok(false);
    }
    build_native_trie_to_cf(db)?;
    Ok(true)
}

/// `true` if the native bucketed trie has been built (cheap probe: `CF_NATIVE_TRIE` is non-empty —
/// the root marker is always written by [`build_native_trie_to_cf`], even for empty native state).
/// The flag-routed native root falls back to the full scan when this is `false` (an unmigrated DB).
pub fn is_native_trie_built(db: &StateDb) -> Result<bool, StateError> {
    let cf = db.cf_handle(CF_NATIVE_TRIE)?;
    let mut iter = db.inner().raw_iterator_cf(cf);
    iter.seek_to_first();
    let built = iter.valid();
    iter.status()?;
    Ok(built)
}

/// Read the persisted native root marker (the incrementally-maintained root). Returns
/// `EMPTY_ROOT_HASH` if the trie has not been built yet.
pub fn persisted_native_root(db: &StateDb) -> Result<B256, StateError> {
    let cf = db.cf_handle(CF_NATIVE_TRIE)?;
    match db.inner().get_cf(cf, ROOT_KEY)? {
        Some(v) if v.len() == 32 => Ok(B256::from_slice(&v)),
        Some(_) => Err(StateError::InvalidData(
            "native root marker has wrong length".into(),
        )),
        None => Ok(EMPTY_ROOT_HASH),
    }
}

// ============================================================================
// rank-root — in-RAM node cache + clean-write elision (`TORUS_NATIVE_ROOT_CACHE`)
// ============================================================================
//
// The incremental update below is already O(dirty buckets), but each dirty
// bucket pays ~TREE_DEPTH persisted-node point reads for sibling hashes
// during path propagation, plus a full bucket rehash even when the "dirty"
// entries are CLEAN rewrites (a put of byte-identical value — e.g. a cache
// flush that re-writes an unchanged row). `TORUS_NATIVE_ROOT_CACHE=1` keeps
// the entire node set (leaf + internal hashes, ≈12MB fully populated) in RAM
// across blocks and elides buckets whose member set did not actually change.
//
// VALUE-NEUTRAL and NODE-LOCAL: the cache changes where sibling hashes are
// READ from and skips only idempotent rewrites — the persisted trie node
// set, the mirror contents and the root are byte-identical to the uncached
// path for any input (asserted by differential tests). Mixed fleets are safe.
//
// STALENESS GUARD: the trie is self-authenticating — the cache carries the
// root it believes the persisted trie has, and is usable iff it equals the
// persisted ROOT_KEY row (32-byte point read per flush). Any out-of-band trie
// change (boot rebuild, crash replay, another writer, anything reorg-shaped)
// shows as a root mismatch ⇒ the cache reloads from `CF_NATIVE_TRIE` (one
// scan of ≤ ~131k small rows), the DB staying authoritative. This is strictly
// stronger than a height-marker check: it cannot false-negative, and blocks
// that never touch native state (which advance the applied-height marker but
// not the trie) do not invalidate it. The cache mutates only after the
// atomic batch containing the ops commits; any flush error invalidates.

/// Runtime toggle: `TORUS_NATIVE_ROOT_CACHE=1` enables the in-RAM node cache
/// + clean-write elision; anything else (INCLUDING UNSET) runs the persisted-
/// node path exactly as today. Read once per process.
pub fn native_root_cache_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED
        .get_or_init(|| parse_native_root_cache_toggle(std::env::var("TORUS_NATIVE_ROOT_CACHE").ok()))
}

/// Pure parse: only `"1"` enables.
fn parse_native_root_cache_toggle(v: Option<String>) -> bool {
    matches!(v.as_deref().map(str::trim), Some("1"))
}

/// rank-root: cross-block in-RAM image of the persisted native trie.
/// Owned by the execution pipeline (same holder discipline as rank8's
/// `ResidentBooks`); handed into the flush by the caller.
#[derive(Default)]
pub struct NativeTrieCache {
    inner: Option<TrieCacheInner>,
}

struct TrieCacheInner {
    /// Non-default leaf hashes by bucket.
    leaves: HashMap<u16, B256>,
    /// Non-default internal node hashes by (level, index), level 0..TREE_DEPTH.
    nodes: HashMap<(u8, u16), B256>,
    /// The finalized root this image corresponds to (== persisted ROOT_KEY).
    root: B256,
}

impl NativeTrieCache {
    /// Drop the cached image — the next flush reloads from the DB.
    pub fn invalidate(&mut self) {
        self.inner = None;
    }

    /// Whether an image is currently held (test/ops introspection).
    pub fn is_populated(&self) -> bool {
        self.inner.is_some()
    }

    /// Fold a committed block's `changed` node map into the in-RAM image and
    /// advance its root (no-op when no image is held). Called by the flush ONLY
    /// after the atomic batch carrying the same nodes is durable.
    pub(crate) fn commit_changed(
        &mut self,
        changed: &BTreeMap<(usize, usize), B256>,
        root: B256,
    ) {
        if let Some(inner) = self.inner.as_mut() {
            apply_changed_to_cache(inner, changed, root);
        }
    }
}

/// Load the full persisted node set into RAM (one `CF_NATIVE_TRIE` scan).
fn load_trie_cache(db: &StateDb) -> Result<TrieCacheInner, StateError> {
    let cf = db.cf_handle(CF_NATIVE_TRIE)?;
    let mut leaves = HashMap::new();
    let mut nodes = HashMap::new();
    let mut root = EMPTY_ROOT_HASH;
    let iter = db.inner().iterator_cf(cf, rocksdb::IteratorMode::Start);
    for item in iter {
        let (k, v) = item?;
        match (k.first(), k.len()) {
            (Some(&NODE_PREFIX_LEAF), 3) => {
                let bucket = u16::from_be_bytes([k[1], k[2]]);
                leaves.insert(bucket, B256::from_slice(&v));
            }
            (Some(&NODE_PREFIX_INTERNAL), 4) => {
                let level = k[1];
                let index = u16::from_be_bytes([k[2], k[3]]);
                nodes.insert((level, index), B256::from_slice(&v));
            }
            (Some(&0x02), 1) => {
                if v.len() == 32 {
                    root = B256::from_slice(&v);
                }
            }
            _ => {
                return Err(StateError::InvalidData(format!(
                    "unrecognized CF_NATIVE_TRIE key (len {})",
                    k.len()
                )))
            }
        }
    }
    Ok(TrieCacheInner { leaves, nodes, root })
}

/// Ensure the cache holds a usable image: present AND matching the persisted
/// root (the self-authentication guard). Reloads from the DB otherwise.
fn ensure_cache_usable<'c>(
    db: &StateDb,
    cache: &'c mut NativeTrieCache,
) -> Result<&'c mut TrieCacheInner, StateError> {
    let persisted = persisted_native_root(db)?;
    let usable = cache
        .inner
        .as_ref()
        .is_some_and(|inner| inner.root == persisted);
    if !usable {
        if cache.inner.is_some() {
            tracing::warn!(
                "rank-root: native trie cache stale (root mismatch) — reloading from CF_NATIVE_TRIE"
            );
        }
        cache.inner = Some(load_trie_cache(db)?);
        // A DB whose ROOT_KEY row is absent (never-built trie) loads as
        // EMPTY_ROOT_HASH with no nodes — consistent with the persisted view.
    }
    Ok(cache.inner.as_mut().expect("just ensured"))
}

// ============================================================================
// Incremental update (A2.2) — O(changed)/block from the overlay dirty set
// ============================================================================

/// Which native-trie CF a pending op targets.
enum CfTarget {
    Mirror,
    Trie,
}

/// A pending write/delete to a native-trie CF, produced by the incremental update so it can be
/// folded into any `WriteBatch` (atomically with the native-CF flush). `value == None` is a delete.
///
/// EVERY value written to either native-trie CF is exactly 32 bytes — a mirror row is
/// `keccak(value)`, a trie row is a node hash, and the root marker is the root — so the payload is
/// an inline [`B256`] rather than a heap `Vec<u8>` (s450: one fewer allocation per emitted op; the
/// stored bytes are the same 32).
struct NodeOp {
    target: CfTarget,
    key: Vec<u8>,
    value: Option<B256>,
}

/// Read a persisted tree node (leaf at `level == TREE_DEPTH`, else internal), or its level default.
fn read_node(
    db: &StateDb,
    level: usize,
    index: usize,
    defaults: &[B256; TREE_DEPTH + 1],
) -> Result<B256, StateError> {
    let cf = db.cf_handle(CF_NATIVE_TRIE)?;
    let key: Vec<u8> = if level == TREE_DEPTH {
        node_key_leaf(index as u16).to_vec()
    } else {
        node_key_internal(level, index).to_vec()
    };
    match db.inner().get_cf(cf, &key)? {
        Some(v) if v.len() == 32 => Ok(B256::from_slice(&v)),
        _ => Ok(defaults[level]),
    }
}

/// Read a bucket's current members from the mirror (prefix scan), decoded as
/// `(cf_tag, key) -> value_hash` (32 B keccak of the live CF value — hash-only mirror) in
/// canonical order.
///
/// L3 #2 (member-miss cost): the scan is bounded to `[bucket, bucket+1)` via a
/// `ReadOptions` upper bound so RocksDB (a) prunes SST files whose key range does
/// not overlap this 2-byte bucket, and (b) stops the merging iterator AT the
/// bound instead of skipping every tombstone between the bucket and the next
/// live key. On the churny hash-only mirror (802k writes / 593k deletes at
/// cap-400) a cold miss on a new/near-empty bucket previously walked those
/// tombstones to find the first live key of the NEXT bucket before the
/// `starts_with` break rejected it; the upper bound makes that walk impossible.
/// Byte-identical output to the unbounded scan for any input — the `starts_with`
/// guard still defines the members, the bound only prunes work outside the
/// bucket. Node-local, value-neutral.
fn read_bucket_members(db: &StateDb, bucket: u16) -> Result<Members, StateError> {
    let cf = db.cf_handle(CF_NATIVE_HASHED)?;
    let prefix = bucket.to_be_bytes();
    let mut out = Members::new();
    let mut ro = rocksdb::ReadOptions::default();
    // First key strictly after this bucket = (bucket+1) as 2 BE bytes. For the
    // last bucket (0xFFFF) there is no successor, so leave the scan unbounded
    // (the `starts_with` break still terminates it) — that bucket simply forgoes
    // SST pruning, a negligible edge.
    if let Some(next) = bucket.checked_add(1) {
        ro.set_iterate_upper_bound(next.to_be_bytes().to_vec());
    }
    let mut iter = db.inner().raw_iterator_cf_opt(cf, ro);
    iter.seek(prefix);
    while iter.valid() {
        let (k, v) = match (iter.key(), iter.value()) {
            (Some(k), Some(v)) if k.starts_with(&prefix) && k.len() >= 3 => (k, v),
            _ => break,
        };
        // Hash-only mirror: every row is exactly 32 B by construction (both
        // writers — `build_native_trie_to_cf` and `apply_native_dirty` — emit
        // `keccak(value)`). A row of any other length is a corrupt / pre-hash-only
        // mirror whose leaves cannot reproduce the consensus preimage anyway, so
        // fail loudly instead of hashing garbage: trie maintenance aborts for the
        // block (native CF writes still flush) rather than persisting a bad root.
        if v.len() != 32 {
            return Err(StateError::InvalidData(format!(
                "CF_NATIVE_HASHED row for bucket {bucket} is {} B, expected a 32 B value hash",
                v.len()
            )));
        }
        out.insert((k[2], k[3..].to_vec()), B256::from_slice(v));
        iter.next();
    }
    iter.status()?;
    Ok(out)
}

// ============================================================================
// rank-root round-3: parallel dirty-bucket rehash (TORUS_PARALLEL_BUCKET_HASH)
// + bounded bucket-member cache (TORUS_BUCKET_MEMBER_CACHE_MB)
// ============================================================================
//
// Both cut the PER-BUCKET term of native-root maintenance (mirror scan + keccak
// + path), are node-local + VALUE-NEUTRAL (persisted bytes AND the root are
// byte-identical to the serial/uncached path for any input), and default OFF.
//
// PARALLELISM & DETERMINISM: dirty buckets are independent, so each bucket's
// member-merge + leaf hash is computed on a worker thread (scoped threads — the
// workspace idiom; no rayon). The tree fold is then done SERIALLY over the
// merged leaves in bucket order, so the root is byte-identical to serial for
// any input and any thread count (proved by the 20x-run determinism test).
// Workers only READ (RocksDB `&DB` is Send+Sync; each worker opens its own
// iterator) and every op is computed in RAM BEFORE anything is appended to the
// batch, so a worker error/panic fails the whole apply cleanly (nothing
// appended → native-CF writes still flush; the incremental root is merely stale
// for the block, exactly as a serial trie-maintenance error already behaves).
//
// MEMBER CACHE: an LRU of bucket_id -> decoded member set eliminates the
// CF_NATIVE_HASHED prefix-scan on a hit. Write-through (post-commit) keeps each
// resident entry byte-equal to the mirror, so a hit returns EXACTLY what a scan
// would. SELF-AUTHENTICATING on the persisted native root — the same guard as
// NativeTrieCache — so any out-of-band trie/mirror change (boot rebuild, crash
// replay, another writer) shows as a root mismatch and flushes the cache. That
// is the SHARED staleness cause with the trie cache (when both are on they
// flush together); it also stands alone when the trie cache is disabled.
// Memory-bounded: LRU eviction to the byte budget; an evicted bucket re-scans.
// Composes with the trie cache and parallelism in every on/off combination.

/// Decoded members of one bucket, in canonical `(cf_tag, key)` order. Values are
/// the 32 B per-entry value hashes (hash-only mirror), held INLINE as [`B256`]
/// (s450) rather than in a per-member heap `Vec<u8>`: same 32 bytes, one fewer
/// allocation per member on every scan / merge / write-through.
pub(crate) type Members = BTreeMap<(u8, Vec<u8>), B256>;

/// Hard cap on `TORUS_PARALLEL_BUCKET_HASH` worker threads.
const MAX_BUCKET_HASH_THREADS: usize = 64;

/// `TORUS_PARALLEL_BUCKET_HASH` = worker-thread count for dirty-bucket leaf
/// hashing. `>= 2` enables the parallel path (capped at [`MAX_BUCKET_HASH_THREADS`]);
/// unset / `"1"` / garbage → `1` (serial = exact-today). Read once per process.
pub fn parallel_bucket_hash_threads() -> usize {
    static N: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *N.get_or_init(|| parse_parallel_threads(std::env::var("TORUS_PARALLEL_BUCKET_HASH").ok()))
}

/// Pure parse for [`parallel_bucket_hash_threads`].
fn parse_parallel_threads(v: Option<String>) -> usize {
    match v.as_deref().map(str::trim).and_then(|s| s.parse::<usize>().ok()) {
        Some(n) if n >= 2 => n.min(MAX_BUCKET_HASH_THREADS),
        _ => 1,
    }
}

/// L3 flush-pipe (a): adaptive engagement threshold for the parallel bucket-hash
/// state-root. The parallel path engages ONLY when the dirty-bucket count exceeds
/// this value; at or below it the serial path runs even when
/// `TORUS_PARALLEL_BUCKET_HASH >= 2`. Rationale (l3-diagnosis §1): at the cap-400
/// shape (~226 dirty buckets) scoped-thread spawn cost dominates (p1 4.22 ms vs
/// p4 6.47 ms); parallel only wins on large dirty sets (uncapped). Unset / garbage
/// → `1`, which reproduces the pre-existing `work.len() <= 1` gate byte-for-byte
/// (parallel engages at `>= 2` buckets exactly as today) for every value of
/// `TORUS_PARALLEL_BUCKET_HASH`. Read once per process.
pub fn bucket_hash_min_buckets() -> usize {
    static N: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *N.get_or_init(|| parse_bucket_hash_min_buckets(std::env::var("TORUS_BUCKET_HASH_MIN_BUCKETS").ok()))
}

/// Pure parse for [`bucket_hash_min_buckets`]. Unset / garbage / `< 1` → `1`
/// (exact-today). A valid `>= 1` value is the crossover bucket count.
fn parse_bucket_hash_min_buckets(v: Option<String>) -> usize {
    match v.as_deref().map(str::trim).and_then(|s| s.parse::<usize>().ok()) {
        Some(n) if n >= 1 => n,
        _ => 1,
    }
}

/// `TORUS_BUCKET_MEMBER_CACHE_MB` = RAM budget (MB) for the bucket-member cache.
/// `>= 1` enables; unset / `"0"` / garbage → `0` (disabled = exact-today). Read
/// once per process.
pub fn member_cache_budget_bytes() -> usize {
    static B: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *B.get_or_init(|| parse_member_cache_mb(std::env::var("TORUS_BUCKET_MEMBER_CACHE_MB").ok()))
}

/// Pure parse for [`member_cache_budget_bytes`].
fn parse_member_cache_mb(v: Option<String>) -> usize {
    match v.as_deref().map(str::trim).and_then(|s| s.parse::<usize>().ok()) {
        Some(mb) if mb >= 1 => mb.saturating_mul(1024 * 1024),
        _ => 0,
    }
}

/// Per-member RAM proxy added to key+value bytes (BTreeMap node + two Vec
/// headers). Approximate — it only needs to be a stable memory proxy for the
/// LRU budget, not exact.
const MEMBER_ENTRY_OVERHEAD: usize = 48;

/// Byte size of a member set for the LRU budget. Values are always 32 B value
/// hashes, so the formula is unchanged from the `Vec<u8>`-valued `Members`
/// (`v.len()` was always 32) — the LRU's eviction behaviour is identical.
fn members_bytes(m: &Members) -> usize {
    m.keys()
        .map(|(_, k)| k.len() + 32 + MEMBER_ENTRY_OVERHEAD)
        .sum()
}

/// rank-root round-3: bounded LRU of `bucket_id -> Arc<Members>`. Owned by the
/// execution pipeline (same holder discipline as [`NativeTrieCache`]) and handed
/// into the flush. Disabled (`inner == None`) is the exact-today path.
#[derive(Default)]
pub struct NativeMemberCache {
    inner: Option<MemberCacheInner>,
}

struct MemberCacheInner {
    budget_bytes: usize,
    total_bytes: usize,
    clock: u64,
    map: HashMap<u16, MemberSlot>,
    /// LRU order: `seq -> bucket` (lowest seq == least-recently-used).
    recency: BTreeMap<u64, u16>,
    /// Persisted native root this image is consistent with (self-authentication).
    root: B256,
}

struct MemberSlot {
    members: Arc<Members>,
    bytes: usize,
    seq: u64,
}

impl NativeMemberCache {
    /// Enabled cache with a `budget_bytes` RAM budget; `0` → disabled (exact-today).
    pub fn with_budget(budget_bytes: usize) -> Self {
        if budget_bytes == 0 {
            Self { inner: None }
        } else {
            Self {
                inner: Some(MemberCacheInner {
                    budget_bytes,
                    total_bytes: 0,
                    clock: 0,
                    map: HashMap::new(),
                    recency: BTreeMap::new(),
                    root: EMPTY_ROOT_HASH,
                }),
            }
        }
    }

    /// Whether the cache is enabled (a budget was configured).
    pub fn is_enabled(&self) -> bool {
        self.inner.is_some()
    }

    /// Number of resident buckets (test/ops introspection).
    pub fn len(&self) -> usize {
        self.inner.as_ref().map_or(0, |i| i.map.len())
    }

    /// Whether the cache currently holds no resident buckets.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Drop all resident entries (keeps the budget) — next accesses re-scan.
    pub fn invalidate(&mut self) {
        if let Some(i) = self.inner.as_mut() {
            i.map.clear();
            i.recency.clear();
            i.total_bytes = 0;
            i.clock = 0;
        }
    }

    /// Self-authenticating guard: flush the image if its root disagrees with the
    /// persisted native root (an out-of-band trie/mirror change — the SAME
    /// staleness cause as [`NativeTrieCache`]). No-op when disabled.
    fn ensure_usable(&mut self, db: &StateDb) -> Result<(), StateError> {
        let Some(i) = self.inner.as_mut() else {
            return Ok(());
        };
        let persisted = persisted_native_root(db)?;
        if i.root != persisted {
            if !i.map.is_empty() {
                tracing::warn!(
                    "rank-root: member cache stale (root mismatch) — flushing (will re-scan)"
                );
            }
            i.map.clear();
            i.recency.clear();
            i.total_bytes = 0;
            i.clock = 0;
            i.root = persisted;
        }
        Ok(())
    }

    /// Resident-member HANDOVER: remove the bucket's slot and hand its `Arc` to
    /// the caller. `None` on a miss (or when disabled).
    ///
    /// s450: this used to LEND a second strong reference (`Arc::clone`), which
    /// forced `process_bucket` to deep-clone the whole `BTreeMap` (~K/G members,
    /// one allocation per member key) on every cache hit — the dominant per-dirty-
    /// bucket cost as state grows. Handing the entry out instead leaves the `Arc`
    /// uniquely owned, so the merge mutates the resident map IN PLACE with zero
    /// copying. The set is handed back by [`Self::commit_finals`] once the batch is
    /// durable — `apply_native_dirty` emits a `member_final` for EVERY bucket it
    /// took, including buckets whose membership did not change.
    ///
    /// Safety of the removal: a missing entry can only cause a re-scan, never a
    /// wrong answer, so every error path (which invalidates the whole cache
    /// anyway) and any caller that forgets to commit degrade to correct-but-cold.
    /// The `ensure_usable` root guard still authenticates whatever stays resident.
    fn take(&mut self, bucket: u16) -> Option<(Arc<Members>, usize)> {
        let i = self.inner.as_mut()?;
        let slot = i.map.remove(&bucket)?;
        i.recency.remove(&slot.seq);
        i.total_bytes = i.total_bytes.saturating_sub(slot.bytes);
        Some((slot.members, slot.bytes))
    }

    /// Write-through a committed block's per-bucket member sets and advance the
    /// image root, then evict LRU to the byte budget. MUST be called only after
    /// the batch carrying the same mirror ops is durable. Returns evictions done.
    ///
    /// Each final carries its own LRU byte size, computed by `process_bucket`
    /// during a pass it was already making over the members (s450) — this used to
    /// be a fourth full `Theta(members)` walk per dirty bucket via
    /// [`members_bytes`]. The value is identical (debug-asserted against
    /// `members_bytes` at the point of production).
    pub(crate) fn commit_finals(
        &mut self,
        finals: Vec<(u16, Members, usize)>,
        root: B256,
    ) -> usize {
        let Some(i) = self.inner.as_mut() else {
            return 0;
        };
        for (bucket, members, bytes) in finals {
            debug_assert_eq!(
                bytes,
                members_bytes(&members),
                "member-set byte size carried from process_bucket must equal members_bytes"
            );
            if let Some(old) = i.map.remove(&bucket) {
                i.total_bytes = i.total_bytes.saturating_sub(old.bytes);
                i.recency.remove(&old.seq);
            }
            i.clock += 1;
            let seq = i.clock;
            i.total_bytes += bytes;
            i.recency.insert(seq, bucket);
            i.map.insert(
                bucket,
                MemberSlot {
                    members: Arc::new(members),
                    bytes,
                    seq,
                },
            );
        }
        i.root = root;
        let mut evictions = 0usize;
        while i.total_bytes > i.budget_bytes {
            let Some((&seq, &bucket)) = i.recency.iter().next() else {
                break;
            };
            i.recency.remove(&seq);
            if let Some(slot) = i.map.remove(&bucket) {
                i.total_bytes = i.total_bytes.saturating_sub(slot.bytes);
            }
            evictions += 1;
        }
        evictions
    }
}

/// Per-bucket result merged into the deterministic tree fold.
struct BucketOutcome {
    changed: bool,
    /// New leaf hash — meaningful only when `changed`.
    leaf: B256,
    mirror_ops: Vec<NodeOp>,
    /// Whether this bucket paid a CF_NATIVE_HASHED prefix-scan (miss / no cache).
    scanned: bool,
    /// Post-block member set to write through into the member cache, with its LRU
    /// byte size (present iff the member cache is enabled).
    member_final: Option<(Members, usize)>,
}

/// One dirty entry, BORROWED from the caller's dirty map: `(cf_tag, native key,
/// Some(new value) | None = delete)`. s450: the grouping used to clone the key and
/// value out of a map that had itself already cloned them out of the overlay.
type DirtyRef<'d> = (u8, &'d [u8], Option<&'d [u8]>);

/// One bucket's unit of work: its id, its (borrowed) dirty entries in canonical
/// order, and the member set handed over by the cache on a hit (with the LRU byte
/// size the cache had already computed for it).
type BucketWork<'d> = (u16, Vec<DirtyRef<'d>>, Option<(Arc<Members>, usize)>);

/// Pure per-bucket work (runs serially or on a worker thread): apply this
/// block's edits to the bucket's members, emit mirror ops, and — when the
/// membership actually changes (ALWAYS on the uncached `elide == false` path) —
/// recompute the leaf hash. Output is identical whether the base members come
/// from a member-cache hit (`base`) or a fresh mirror scan.
fn process_bucket(
    db: &StateDb,
    bucket: u16,
    entries: &[DirtyRef<'_>],
    base: Option<(Arc<Members>, usize)>,
    member_enabled: bool,
    elide: bool,
    defaults: &[B256; TREE_DEPTH + 1],
) -> Result<BucketOutcome, StateError> {
    let scanned = base.is_none();
    // LRU byte size of the base set, when the cache handed one over — reusable
    // verbatim if this block turns out to change nothing (s450).
    let base_bytes = base.as_ref().map(|(_, b)| *b);
    let mut members: Members = match base {
        // The cache HANDS the slot over (`NativeMemberCache::take` removes it), so
        // the strong count is 1 and `try_unwrap` moves the map out without copying
        // a single member. The `unwrap_or_else` arm is the old deep clone, kept as
        // a correctness fallback in case the `Arc` is ever shared again.
        Some((arc, _)) => Arc::try_unwrap(arc).unwrap_or_else(|shared| (*shared).clone()),
        None => read_bucket_members(db, bucket)?,
    };

    let mut mirror_ops: Vec<NodeOp> = Vec::new();
    let mut bucket_changed = false;
    // ONE reusable probe key: `BTreeMap<(u8, Vec<u8>), _>` lookups need an owned
    // tuple, so refill this buffer per entry instead of allocating a fresh
    // `key.clone()` for every get / remove (s450).
    let mut probe: (u8, Vec<u8>) = (0, Vec::new());
    for (tag, key, val) in entries {
        probe.0 = *tag;
        probe.1.clear();
        probe.1.extend_from_slice(key);
        match val {
            Some(v) => {
                // Hash-only mirror: members / mirror rows hold keccak(value); the
                // clean-write elision compares stored hash == hash of the new value
                // (identical semantics — a keccak collision is a broken world anyway).
                let vh = keccak256(v);
                match members.get_mut(&probe) {
                    Some(slot) => {
                        if elide && *slot == vh {
                            continue; // clean rewrite — idempotent (cached path elides)
                        }
                        *slot = vh; // in-place update: no key allocation
                    }
                    None => {
                        members.insert((*tag, key.to_vec()), vh);
                    }
                }
                mirror_ops.push(NodeOp {
                    target: CfTarget::Mirror,
                    key: mirror_key(bucket, *tag, key),
                    value: Some(vh),
                });
                bucket_changed = true;
            }
            None => {
                let was_present = members.remove(&probe).is_some();
                if elide && !was_present {
                    continue; // delete of absent — idempotent (cached path elides)
                }
                mirror_ops.push(NodeOp {
                    target: CfTarget::Mirror,
                    key: mirror_key(bucket, *tag, key),
                    value: None,
                });
                bucket_changed = true;
            }
        }
    }

    // The leaf rebuild already walks every member, so the write-through's LRU byte
    // size is accumulated here rather than in a separate `members_bytes` pass
    // (s450) — same formula, same value (debug-asserted in `commit_finals`).
    let mut leaf_bytes = 0usize;
    let leaf = if bucket_changed {
        if members.is_empty() {
            defaults[TREE_DEPTH]
        } else {
            let mut data = Vec::new();
            for ((tag, key), v) in &members {
                frame_entry(&mut data, *tag, key, v.as_slice());
                leaf_bytes += key.len() + 32 + MEMBER_ENTRY_OVERHEAD;
            }
            keccak256(&data)
        }
    } else {
        defaults[TREE_DEPTH] // unused when !bucket_changed
    };

    // Hand the member set back whenever the cache is enabled: either we scanned
    // (the set is now authoritative — populate) or the cache HANDED US ITS SLOT
    // and must get it back, including when nothing changed (s450: `take` removes,
    // so an unchanged hit returning `None` here would silently drop a live
    // bucket). Byte size: from the leaf walk if the bucket changed, else the size
    // the cache already had (the set is untouched); a fresh scan whose entries all
    // elided is the only case that still has to measure.
    let member_final = member_enabled.then(|| {
        let bytes = if bucket_changed {
            leaf_bytes
        } else {
            base_bytes.unwrap_or_else(|| members_bytes(&members))
        };
        (members, bytes)
    });

    Ok(BucketOutcome {
        changed: bucket_changed,
        leaf,
        mirror_ops,
        scanned,
        member_final,
    })
}

/// Run every dirty bucket's [`process_bucket`] — serially (`parallel <= 1`) or
/// across `parallel` scoped worker threads — and collect into a bucket-ordered
/// map (the deterministic merge key). Identical output regardless of thread
/// count: leaf hashes are pure per-bucket functions and the fold is serial.
fn run_buckets(
    db: &StateDb,
    mut work: Vec<BucketWork<'_>>,
    member_enabled: bool,
    elide: bool,
    defaults: &[B256; TREE_DEPTH + 1],
    parallel: usize,
    min_buckets: usize,
) -> Result<BTreeMap<u16, BucketOutcome>, StateError> {
    // L3 flush-pipe (a): serial unless parallelism is enabled AND the dirty-bucket
    // count exceeds the adaptive threshold. `min_buckets == 1` (default) is the
    // pre-existing `work.len() <= 1` gate. Output is byte-identical on either path.
    //
    // The cached member set is MOVED into `process_bucket` (`hit.take()`) rather
    // than lent, so the merge can consume the `Arc` instead of deep-cloning it.
    if parallel <= 1 || work.len() <= min_buckets.max(1) {
        let mut out = BTreeMap::new();
        for (b, entries, hit) in &mut work {
            let o = process_bucket(db, *b, entries, hit.take(), member_enabled, elide, defaults)?;
            out.insert(*b, o);
        }
        return Ok(out);
    }

    let nthreads = parallel.min(work.len());
    let chunk_size = work.len().div_ceil(nthreads);
    // `chunks_mut` (was `chunks`) so each worker can MOVE its buckets' member
    // sets out of its own disjoint slice; the chunks stay non-overlapping, so the
    // partition and hence the per-bucket work is unchanged.
    let chunks: Vec<&mut [BucketWork<'_>]> = work.chunks_mut(chunk_size).collect();

    // Contain worker panics so a bug in one worker fails the apply cleanly
    // (nothing is appended to the batch) instead of unwinding the exec thread.
    let results: Vec<Result<Vec<(u16, BucketOutcome)>, StateError>> = std::thread::scope(|s| {
        let handles: Vec<_> = chunks
            .into_iter()
            .map(|chunk| {
                s.spawn(move || -> Result<Vec<(u16, BucketOutcome)>, StateError> {
                    let mut v = Vec::with_capacity(chunk.len());
                    for (b, entries, hit) in chunk.iter_mut() {
                        let o = process_bucket(
                            db,
                            *b,
                            entries,
                            hit.take(),
                            member_enabled,
                            elide,
                            defaults,
                        )?;
                        v.push((*b, o));
                    }
                    Ok(v)
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|h| {
                h.join().unwrap_or_else(|_| {
                    Err(StateError::InvalidData(
                        "native bucket-hash worker panicked".into(),
                    ))
                })
            })
            .collect()
    });

    let mut out = BTreeMap::new();
    for r in results {
        for (b, o) in r? {
            out.insert(b, o);
        }
    }
    Ok(out)
}

/// Unchanged-sibling hash for the tree fold: from the in-RAM trie image (cached
/// path) or the persisted node (uncached path).
fn sibling_node(
    db: &StateDb,
    trie_inner: Option<&TrieCacheInner>,
    level: usize,
    index: usize,
    defaults: &[B256; TREE_DEPTH + 1],
) -> Result<B256, StateError> {
    match trie_inner {
        Some(inner) => Ok(if level == TREE_DEPTH {
            inner
                .leaves
                .get(&(index as u16))
                .copied()
                .unwrap_or(defaults[TREE_DEPTH])
        } else {
            inner
                .nodes
                .get(&(level as u8, index as u16))
                .copied()
                .unwrap_or(defaults[level])
        }),
        None => read_node(db, level, index, defaults),
    }
}

/// What the flush needs after a [`apply_native_dirty`] call to (a) report and
/// (b) fold into the caches once the batch is durable.
pub struct NativeTrieApply {
    pub root: B256,
    /// Buckets actually rehashed (post-elision) — the O(dirty) witness.
    pub rehashed_buckets: usize,
    /// CF_NATIVE_HASHED prefix-scans performed — drops with member-cache hits.
    pub bucket_scans: usize,
    pub member_hits: usize,
    pub member_misses: usize,
    /// Committed trie node changes — folded into the trie cache post-commit
    /// (`Some` iff the trie cache was used).
    pub(crate) trie_changed: Option<BTreeMap<(usize, usize), B256>>,
    /// Per-bucket member sets (with their LRU byte size) to write through into the
    /// member cache post-commit.
    pub(crate) member_finals: Vec<(u16, Members, usize)>,
}

/// rank-root round-3: THE unified native-trie append. Computes all mirror + tree
/// ops fallibly (nothing appended on `Err`), optionally using the trie node
/// cache (sibling reads from RAM + clean-write elision), the bucket-member cache
/// (skip the mirror scan on a hit), and `parallel` worker threads for per-bucket
/// leaf hashing — in ANY combination, always byte-identical to the serial
/// uncached path. The caller applies `trie_changed` / `member_finals` to the
/// caches ONLY after the batch commits, and invalidates on any failure.
///
/// `dirty` is generic over how it owns its bytes so the consensus flush can pass a
/// map BORROWED straight out of the overlay (`K = V = &[u8]`), while tests and the
/// standalone helpers keep passing owned `Vec<u8>` maps — same code, no copies on
/// the hot path.
#[allow(clippy::too_many_arguments)]
pub fn apply_native_dirty<K, V>(
    db: &StateDb,
    batch: &mut WriteBatch,
    dirty: &BTreeMap<(u8, K), Option<V>>,
    trie_cache: Option<&mut NativeTrieCache>,
    member_cache: Option<&mut NativeMemberCache>,
    parallel: usize,
    min_buckets: usize,
) -> Result<NativeTrieApply, StateError>
where
    K: Borrow<[u8]>,
    V: Borrow<[u8]>,
{
    let defaults = default_nodes();

    // 1. Group dirty entries by bucket (canonical, sorted). The grouped entries
    //    BORROW `dirty`'s keys and values (s450) — they were cloned here before,
    //    a second copy of a set the caller had already copied.
    let mut by_bucket: BTreeMap<u16, Vec<DirtyRef<'_>>> = BTreeMap::new();
    for ((tag, key), val) in dirty {
        let key: &[u8] = key.borrow();
        let b = bucket_id(*tag, key);
        by_bucket
            .entry(b)
            .or_default()
            .push((*tag, key, val.as_ref().map(Borrow::borrow)));
    }

    // 2. The cached path reads siblings from the in-RAM trie image AND elides
    //    idempotent writes; the uncached path reads from the DB and never elides
    //    (byte-identical to pre-round-3).
    let elide = trie_cache.is_some();
    let mut trie_cache = trie_cache;
    let trie_inner: Option<&TrieCacheInner> = if elide {
        let inner = ensure_cache_usable(
            db,
            trie_cache.as_deref_mut().expect("elide => trie cache present"),
        )?;
        Some(&*inner)
    } else {
        None
    };

    // 3. Member cache: validate (self-authenticating on the persisted root) and
    //    HAND OVER resident member sets on the MAIN thread (s450: `take`, not a
    //    lent `Arc::clone`), so each worker owns its bucket's set outright and
    //    merges into it without copying. Every taken set is handed back through
    //    `member_finals` after the batch commits.
    let mut member_cache = member_cache;
    let member_enabled = member_cache.as_ref().is_some_and(|c| c.is_enabled());
    if member_enabled {
        member_cache
            .as_deref_mut()
            .expect("member cache present")
            .ensure_usable(db)?;
    }
    let (mut member_hits, mut member_misses) = (0usize, 0usize);
    let mut work: Vec<BucketWork<'_>> = Vec::with_capacity(by_bucket.len());
    for (b, entries) in by_bucket {
        let hit = if member_enabled {
            match member_cache
                .as_deref_mut()
                .expect("member cache present")
                .take(b)
            {
                Some(arc) => {
                    member_hits += 1;
                    Some(arc)
                }
                None => {
                    member_misses += 1;
                    None
                }
            }
        } else {
            None
        };
        work.push((b, entries, hit));
    }

    // 4. Per-bucket rehash (serial or scoped-thread parallel; identical output).
    let outcomes = run_buckets(db, work, member_enabled, elide, &defaults, parallel, min_buckets)?;

    // 5. Merge deterministically in bucket order.
    let mut ops: Vec<NodeOp> = Vec::new();
    let mut changed: BTreeMap<(usize, usize), B256> = BTreeMap::new();
    let mut rehashed = 0usize;
    let mut bucket_scans = 0usize;
    let mut member_finals: Vec<(u16, Members, usize)> = Vec::new();
    for (bucket, o) in outcomes {
        if o.scanned {
            bucket_scans += 1;
        }
        ops.extend(o.mirror_ops);
        if o.changed {
            rehashed += 1;
            changed.insert((TREE_DEPTH, bucket as usize), o.leaf);
        }
        if let Some((m, bytes)) = o.member_final {
            member_finals.push((bucket, m, bytes));
        }
    }

    let mirror_cf = db.cf_handle(CF_NATIVE_HASHED)?;
    let trie_cf = db.cf_handle(CF_NATIVE_TRIE)?;

    // 6. Nothing actually changed (only reachable on the elide path): the trie is
    //    untouched — append any residual mirror ops (there are none) and return
    //    the unchanged root. Falling through would default node00 to the empty
    //    subtree and wrongly report EMPTY_ROOT_HASH.
    if changed.is_empty() {
        for op in &ops {
            match &op.value {
                Some(v) => batch.put_cf(mirror_cf, &op.key, v.as_slice()),
                None => batch.delete_cf(mirror_cf, &op.key),
            }
        }
        let root = match trie_inner {
            Some(inner) => inner.root,
            None => persisted_native_root(db)?,
        };
        return Ok(NativeTrieApply {
            root,
            rehashed_buckets: 0,
            bucket_scans,
            member_hits,
            member_misses,
            trie_changed: if elide { Some(changed) } else { None },
            member_finals,
        });
    }

    // 7. Propagate up, level by level (serial → deterministic). A parent
    //    recomputes from its changed children and its unchanged siblings.
    for level in (1..=TREE_DEPTH).rev() {
        let level_indices: Vec<usize> = changed
            .range((level, 0)..(level + 1, 0))
            .map(|(&(_, i), _)| i)
            .collect();
        let parents: BTreeSet<usize> = level_indices.iter().map(|i| i / 2).collect();
        for p in parents {
            let left = match changed.get(&(level, 2 * p)) {
                Some(v) => *v,
                None => sibling_node(db, trie_inner, level, 2 * p, &defaults)?,
            };
            let right = match changed.get(&(level, 2 * p + 1)) {
                Some(v) => *v,
                None => sibling_node(db, trie_inner, level, 2 * p + 1, &defaults)?,
            };
            changed.insert((level - 1, p), hash_pair(&left, &right));
        }
    }

    // 8. Emit tree-node ops (delete-to-default discipline) + root marker.
    for ((level, index), value) in &changed {
        let key: Vec<u8> = if *level == TREE_DEPTH {
            node_key_leaf(*index as u16).to_vec()
        } else {
            node_key_internal(*level, *index).to_vec()
        };
        if *value == defaults[*level] {
            ops.push(NodeOp {
                target: CfTarget::Trie,
                key,
                value: None,
            });
        } else {
            ops.push(NodeOp {
                target: CfTarget::Trie,
                key,
                value: Some(*value),
            });
        }
    }
    let node00 = changed.get(&(0, 0)).copied().unwrap_or(defaults[0]);
    let root = finalize_root(node00, &defaults);
    ops.push(NodeOp {
        target: CfTarget::Trie,
        key: ROOT_KEY.to_vec(),
        value: Some(root),
    });

    // 9. Append (infallible now — everything above committed to memory first).
    for op in &ops {
        let cf = match op.target {
            CfTarget::Mirror => mirror_cf,
            CfTarget::Trie => trie_cf,
        };
        match &op.value {
            Some(v) => batch.put_cf(cf, &op.key, v.as_slice()),
            None => batch.delete_cf(cf, &op.key),
        }
    }

    Ok(NativeTrieApply {
        root,
        rehashed_buckets: rehashed,
        bucket_scans,
        member_hits,
        member_misses,
        trie_changed: if elide { Some(changed) } else { None },
        member_finals,
    })
}

/// Standalone incremental commit (serial, uncached): apply `dirty` to the
/// persisted native trie in its own atomic batch, returning the new root. The
/// consensus path folds the same ops into the native-CF flush batch via
/// [`apply_native_dirty`]; this is for tests / non-flush callers.
pub fn commit_native_trie_incremental(
    db: &StateDb,
    dirty: &BTreeMap<(u8, Vec<u8>), Option<Vec<u8>>>,
) -> Result<B256, StateError> {
    let mut batch = WriteBatch::default();
    let apply = apply_native_dirty(db, &mut batch, dirty, None, None, 1, 1)?;
    db.write(batch)?;
    Ok(apply.root)
}

/// The number of DISTINCT buckets a dirty set touches — the uncached path's
/// rehash count (the cached path may elide clean rewrites below this).
pub fn dirty_bucket_count(dirty: &BTreeMap<(u8, Vec<u8>), Option<Vec<u8>>>) -> usize {
    let mut buckets = std::collections::HashSet::new();
    for (tag, key) in dirty.keys() {
        buckets.insert(bucket_id(*tag, key));
    }
    buckets.len()
}

/// Fold the `changed` node map of a successfully-committed cached update into
/// the in-RAM image (delete-to-default mirrored as removal).
fn apply_changed_to_cache(inner: &mut TrieCacheInner, changed: &BTreeMap<(usize, usize), B256>, root: B256) {
    let defaults = default_nodes();
    for ((level, index), value) in changed {
        if *level == TREE_DEPTH {
            if *value == defaults[TREE_DEPTH] {
                inner.leaves.remove(&(*index as u16));
            } else {
                inner.leaves.insert(*index as u16, *value);
            }
        } else if *value == defaults[*level] {
            inner.nodes.remove(&(*level as u8, *index as u16));
        } else {
            inner.nodes.insert((*level as u8, *index as u16), *value);
        }
    }
    inner.root = root;
}

/// Test/tooling helper: cached standalone commit (twin of
/// [`commit_native_trie_incremental`]). Returns `(root, rehashed_buckets)`.
pub fn commit_native_trie_incremental_cached(
    db: &StateDb,
    dirty: &BTreeMap<(u8, Vec<u8>), Option<Vec<u8>>>,
    cache: &mut NativeTrieCache,
) -> Result<(B256, usize), StateError> {
    let (root, rehashed, _scans) =
        commit_native_trie_incremental_full(db, dirty, Some(cache), None, 1)?;
    Ok((root, rehashed))
}

/// rank-root round-3: full standalone commit covering every trie-cache /
/// member-cache / parallelism combination — the differential-test entry point
/// (env is never consulted; `parallel` and the caches are explicit). Applies
/// `dirty` in its own atomic batch and, on success, folds both caches; on
/// failure both are invalidated. Returns `(root, rehashed_buckets, bucket_scans)`.
pub fn commit_native_trie_incremental_full(
    db: &StateDb,
    dirty: &BTreeMap<(u8, Vec<u8>), Option<Vec<u8>>>,
    mut trie_cache: Option<&mut NativeTrieCache>,
    mut member_cache: Option<&mut NativeMemberCache>,
    parallel: usize,
) -> Result<(B256, usize, usize), StateError> {
    let mut batch = WriteBatch::default();
    // Differential-test entry: threshold fixed at 1 (exact-today engagement) so the
    // explicit `parallel` alone selects serial vs parallel, as before.
    let apply = match apply_native_dirty(
        db,
        &mut batch,
        dirty,
        trie_cache.as_deref_mut(),
        member_cache.as_deref_mut(),
        parallel,
        1,
    ) {
        Ok(a) => a,
        Err(e) => {
            if let Some(c) = trie_cache.as_deref_mut() {
                c.invalidate();
            }
            if let Some(c) = member_cache.as_deref_mut() {
                c.invalidate();
            }
            return Err(e);
        }
    };
    if let Err(e) = db.write(batch) {
        if let Some(c) = trie_cache.as_deref_mut() {
            c.invalidate();
        }
        if let Some(c) = member_cache.as_deref_mut() {
            c.invalidate();
        }
        return Err(e.into());
    }
    if let (Some(c), Some(changed)) = (trie_cache.as_deref_mut(), &apply.trie_changed) {
        c.commit_changed(changed, apply.root);
    }
    if let Some(c) = member_cache.as_deref_mut() {
        c.commit_finals(apply.member_finals, apply.root);
    }
    Ok((apply.root, apply.rehashed_buckets, apply.bucket_scans))
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
        assert_eq!(
            built, full,
            "built native trie root must equal the full scan"
        );
        assert_eq!(
            persisted_native_root(&db).unwrap(),
            full,
            "persisted marker must match"
        );

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
        assert_eq!(
            built, EMPTY_ROOT_HASH,
            "empty native state => EMPTY_ROOT_HASH"
        );
        assert_eq!(persisted_native_root(&db).unwrap(), EMPTY_ROOT_HASH);
    }

    #[test]
    fn native_root_changes_with_state() {
        let (db, _dir) = temp_db();
        seed(&db);
        let r1 = native_root_full(&db).unwrap();
        db.put_cf_raw(CF_NATIVE_BALANCES, b"\x00\x00\x00\x00new", b"v")
            .unwrap();
        let r2 = native_root_full(&db).unwrap();
        assert_ne!(r1, r2, "native root must change when state changes");
    }

    #[test]
    fn ensure_native_trie_built_is_one_shot() {
        let (db, _dir) = temp_db();
        seed(&db);
        assert!(ensure_native_trie_built(&db).unwrap(), "first call builds");
        assert!(
            !ensure_native_trie_built(&db).unwrap(),
            "second call is a no-op"
        );
        assert_eq!(
            persisted_native_root(&db).unwrap(),
            native_root_full(&db).unwrap()
        );
    }

    fn key_for(tag: usize, i: u32) -> Vec<u8> {
        let mut key = vec![tag as u8];
        key.extend_from_slice(&i.to_be_bytes());
        if i % 3 == 0 {
            key.push(0xAB);
        }
        key
    }

    /// Apply native-CF changes `(cf_index, key, Some(value)|None=delete)` to the live CFs (simulating
    /// the overlay flush) and return the matching dirty map for the incremental commit.
    fn apply_ops(
        db: &StateDb,
        ops: &[(usize, Vec<u8>, Option<Vec<u8>>)],
    ) -> BTreeMap<(u8, Vec<u8>), Option<Vec<u8>>> {
        let mut dirty = BTreeMap::new();
        for (cfi, key, val) in ops {
            let (cf_name, tag) = NATIVE_ROOT_CFS[*cfi];
            match val {
                Some(v) => {
                    db.put_cf_raw(cf_name, key, v).unwrap();
                    dirty.insert((tag, key.clone()), Some(v.clone()));
                }
                None => {
                    db.delete_cf_raw(cf_name, key).unwrap();
                    dirty.insert((tag, key.clone()), None);
                }
            }
        }
        dirty
    }

    /// A2.2 single-step determinism gate: after each kind of change (update / insert / delete /
    /// bucket-emptying / multi-CF), the incremental root must equal the full-scan oracle.
    #[test]
    fn incremental_native_root_equals_full_scan() {
        let (db, _dir) = temp_db();
        seed(&db);
        build_native_trie_to_cf(&db).unwrap();

        let cases: Vec<Vec<(usize, Vec<u8>, Option<Vec<u8>>)>> = vec![
            vec![(0, key_for(0, 5), Some(vec![1, 2, 3, 4]))], // update existing balance
            vec![(2, b"brand-new-position-key".to_vec(), Some(vec![9; 20]))], // insert (fresh bucket)
            vec![(3, key_for(3, 10), None)], // delete existing oracle entry
            vec![
                (0, key_for(0, 1), Some(vec![7; 16])),
                (1, key_for(1, 2), Some(vec![8; 30])),
                (4, key_for(4, 3), None),
                (5, b"new-validator".to_vec(), Some(vec![0xCD; 33])),
            ], // multi-CF in one block
            vec![(5, b"new-validator".to_vec(), None)], // delete what we just inserted (may empty a bucket)
        ];

        for (n, ops) in cases.iter().enumerate() {
            let dirty = apply_ops(&db, ops);
            let incr = commit_native_trie_incremental(&db, &dirty).unwrap();
            let full = native_root_full(&db).unwrap();
            assert_eq!(incr, full, "case {n}: incremental != full-scan");
            assert_eq!(
                persisted_native_root(&db).unwrap(),
                full,
                "case {n}: persisted marker != full"
            );
        }
    }

    /// A2.2 multi-block gate (the shape that caught the EVM bug the single-step test missed): many
    /// SEQUENTIAL incremental commits over an evolving base must keep the persisted root byte-
    /// identical to the full scan after EVERY block. Deterministic xorshift seed for reproducibility.
    #[test]
    fn sequential_native_commits_match_full_scan() {
        fn xorshift(s: &mut u64) -> u64 {
            *s ^= *s << 13;
            *s ^= *s >> 7;
            *s ^= *s << 17;
            *s
        }
        let (db, _dir) = temp_db();
        seed(&db);
        build_native_trie_to_cf(&db).unwrap();

        let mut rng = 0x2545_f491_4f6c_dd1du64;
        for round in 0..30u32 {
            let mut ops: Vec<(usize, Vec<u8>, Option<Vec<u8>>)> = Vec::new();
            let n = (xorshift(&mut rng) % 6) + 1;
            for _ in 0..n {
                let cfi = (xorshift(&mut rng) % 6) as usize;
                let i = (xorshift(&mut rng) % 160) as u32;
                let key = key_for(cfi, i);
                match xorshift(&mut rng) % 4 {
                    0 => ops.push((cfi, key, None)), // delete
                    _ => {
                        let len = 4 + (xorshift(&mut rng) % 40) as usize;
                        ops.push((cfi, key, Some(vec![(round as u8).wrapping_add(7); len])));
                    }
                }
            }
            let dirty = apply_ops(&db, &ops);
            let incr = commit_native_trie_incremental(&db, &dirty).unwrap();
            let full = native_root_full(&db).unwrap();
            assert_eq!(
                incr, full,
                "round {round}: incremental != full-scan over evolved base"
            );
        }
    }

    /// Assert every `CF_NATIVE_HASHED` row is exactly 32 B and equals the keccak of the
    /// live CF value it mirrors, and that no live entry is missing a mirror row
    /// (the hash-only-mirror witness).
    fn assert_mirror_is_hash_only(db: &StateDb) {
        let mirror_cf = db.cf_handle(CF_NATIVE_HASHED).unwrap();
        let mut mirror_rows = 0usize;
        for item in db.inner().iterator_cf(mirror_cf, rocksdb::IteratorMode::Start) {
            let (k, v) = item.unwrap();
            assert!(k.len() >= 3, "mirror key too short");
            assert_eq!(v.len(), 32, "mirror value must be a 32 B hash");
            let tag = k[2];
            let native_key = &k[3..];
            let (cf_name, _) = NATIVE_ROOT_CFS
                .iter()
                .find(|(_, t)| *t == tag)
                .expect("mirror row tags a native-root CF");
            let live = db
                .get_cf_raw(cf_name, native_key)
                .unwrap()
                .expect("mirror row must have a live CF entry");
            assert_eq!(
                v.as_ref(),
                keccak256(&live).as_slice(),
                "mirror value != keccak(live value)"
            );
            // Key placement: the bucket prefix must match bucket_id(tag, key).
            let b = u16::from_be_bytes([k[0], k[1]]);
            assert_eq!(b, bucket_id(tag, native_key), "mirror bucket prefix wrong");
            mirror_rows += 1;
        }
        // Completeness: every live native-root entry has a mirror row.
        let mut live_rows = 0usize;
        for (cf_name, _) in NATIVE_ROOT_CFS {
            if let Some(cf) = db.inner().cf_handle(cf_name) {
                live_rows += db
                    .inner()
                    .iterator_cf(cf, rocksdb::IteratorMode::Start)
                    .count();
            }
        }
        assert_eq!(mirror_rows, live_rows, "mirror row count != live entry count");
    }

    /// Hash-only mirror witness: after a full build AND after incremental commits
    /// (inserts / updates / deletes), every mirror row is exactly `keccak(value)`.
    #[test]
    fn mirror_stores_value_hashes_only() {
        let (db, _dir) = temp_db();
        seed(&db);
        build_native_trie_to_cf(&db).unwrap();
        assert_mirror_is_hash_only(&db);

        let cases: Vec<Vec<(usize, Vec<u8>, Option<Vec<u8>>)>> = vec![
            vec![(0, key_for(0, 5), Some(vec![1, 2, 3, 4]))],
            vec![(2, b"hash-only-new-key".to_vec(), Some(vec![9; 20]))],
            vec![(3, key_for(3, 10), None)],
            vec![(2, b"hash-only-new-key".to_vec(), None)],
        ];
        for ops in &cases {
            let dirty = apply_ops(&db, ops);
            let incr = commit_native_trie_incremental(&db, &dirty).unwrap();
            assert_eq!(incr, native_root_full(&db).unwrap());
            assert_mirror_is_hash_only(&db);
        }
    }

    // ---- rank-root: cached-path gates ----

    #[test]
    fn root_cache_toggle_default_off_only_one_enables() {
        assert!(!parse_native_root_cache_toggle(None));
        assert!(parse_native_root_cache_toggle(Some("1".to_string())));
        assert!(parse_native_root_cache_toggle(Some(" 1 ".to_string())));
        for v in ["0", "true", "on", "", "yes", "2"] {
            assert!(!parse_native_root_cache_toggle(Some(v.to_string())), "{v}");
        }
    }

    /// Dump a CF's full contents (byte-equality witness).
    fn dump(db: &StateDb, cf_name: &str) -> Vec<(Vec<u8>, Vec<u8>)> {
        let cf = db.cf_handle(cf_name).unwrap();
        db.inner()
            .iterator_cf(cf, rocksdb::IteratorMode::Start)
            .map(|i| {
                let (k, v) = i.unwrap();
                (k.to_vec(), v.to_vec())
            })
            .collect()
    }

    /// THE rank-root differential gate: a long random sequence applied to twin
    /// DBs — uncached on A, cached on B — must keep roots, the full-scan
    /// oracle AND the persisted trie/mirror CF bytes identical after every
    /// block, including across a mid-sequence cache invalidation (restart)
    /// and with clean rewrites mixed in.
    #[test]
    fn cached_commits_byte_identical_to_uncached() {
        fn xorshift(s: &mut u64) -> u64 {
            *s ^= *s << 13;
            *s ^= *s >> 7;
            *s ^= *s << 17;
            *s
        }
        let (db_a, _da) = temp_db();
        let (db_b, _db_) = temp_db();
        seed(&db_a);
        seed(&db_b);
        build_native_trie_to_cf(&db_a).unwrap();
        build_native_trie_to_cf(&db_b).unwrap();
        let mut cache = NativeTrieCache::default();

        let mut rng = 0x0DDB_A11_5EEDu64;
        for round in 0..30u32 {
            if round == 13 {
                cache.invalidate(); // simulated restart mid-sequence
            }
            let mut ops: Vec<(usize, Vec<u8>, Option<Vec<u8>>)> = Vec::new();
            let n = (xorshift(&mut rng) % 6) + 1;
            for _ in 0..n {
                let cfi = (xorshift(&mut rng) % 6) as usize;
                let i = (xorshift(&mut rng) % 160) as u32;
                let key = key_for(cfi, i);
                match xorshift(&mut rng) % 4 {
                    0 => ops.push((cfi, key, None)),
                    _ => {
                        let len = 4 + (xorshift(&mut rng) % 40) as usize;
                        ops.push((cfi, key, Some(vec![(round as u8).wrapping_add(7); len])));
                    }
                }
            }
            // Every third round, mix in a CLEAN rewrite of an existing entry
            // (put of the identical value) — the elision case.
            if round % 3 == 0 {
                let (cf_name, _) = NATIVE_ROOT_CFS[1];
                let key = key_for(1, 4);
                if let Some(v) = db_a.get_cf_raw(cf_name, &key).unwrap() {
                    ops.push((1, key, Some(v)));
                }
            }

            let dirty_a = apply_ops(&db_a, &ops);
            let dirty_b = apply_ops(&db_b, &ops);
            assert_eq!(dirty_a, dirty_b);

            let root_a = commit_native_trie_incremental(&db_a, &dirty_a).unwrap();
            let (root_b, rehashed) =
                commit_native_trie_incremental_cached(&db_b, &dirty_b, &mut cache).unwrap();

            assert_eq!(root_a, root_b, "round {round}: cached root != uncached");
            let full = native_root_full(&db_b).unwrap();
            assert_eq!(root_b, full, "round {round}: cached root != full-scan oracle");
            assert!(
                rehashed <= dirty_bucket_count(&dirty_b),
                "round {round}: elision can only shrink the rehash set"
            );
            // Persisted trie + mirror must be BYTE-identical (elision skips
            // only idempotent rewrites).
            assert_eq!(
                dump(&db_a, CF_NATIVE_TRIE),
                dump(&db_b, CF_NATIVE_TRIE),
                "round {round}: trie CF diverged"
            );
            assert_eq!(
                dump(&db_a, CF_NATIVE_HASHED),
                dump(&db_b, CF_NATIVE_HASHED),
                "round {round}: mirror CF diverged"
            );
        }
    }

    /// Elision witness: a block of ONLY clean rewrites must rehash ZERO
    /// buckets on the cached path (the uncached path rehashes every touched
    /// bucket) — and leave the root and CFs untouched.
    #[test]
    fn cached_path_elides_clean_rewrites() {
        let (db, _dir) = temp_db();
        seed(&db);
        build_native_trie_to_cf(&db).unwrap();
        let before_root = persisted_native_root(&db).unwrap();
        let before_trie = dump(&db, CF_NATIVE_TRIE);
        let mut cache = NativeTrieCache::default();

        // Rewrite 10 existing entries with their CURRENT values + delete two
        // absent keys.
        let mut ops: Vec<(usize, Vec<u8>, Option<Vec<u8>>)> = Vec::new();
        for i in 0..10u32 {
            let (cf_name, _) = NATIVE_ROOT_CFS[(i % 6) as usize];
            let key = key_for((i % 6) as usize, i + 20);
            let v = db.get_cf_raw(cf_name, &key).unwrap().expect("seeded");
            ops.push(((i % 6) as usize, key, Some(v)));
        }
        ops.push((0, b"never-existed-a".to_vec(), None));
        ops.push((4, b"never-existed-b".to_vec(), None));

        let dirty = apply_ops(&db, &ops);
        assert!(dirty_bucket_count(&dirty) > 0, "uncached would rehash");
        let (root, rehashed) =
            commit_native_trie_incremental_cached(&db, &dirty, &mut cache).unwrap();
        assert_eq!(rehashed, 0, "clean rewrites must be fully elided");
        assert_eq!(root, before_root, "root unchanged");
        assert_eq!(dump(&db, CF_NATIVE_TRIE), before_trie, "trie CF unchanged");
        assert_eq!(root, native_root_full(&db).unwrap(), "oracle agrees");
    }

    /// Staleness guard: an out-of-band trie change (another writer / boot
    /// rebuild) makes the cached image's root disagree with the persisted
    /// marker — the next cached commit must RELOAD, not trust stale nodes.
    #[test]
    fn cached_path_rebuilds_on_out_of_band_trie_change() {
        let (db, _dir) = temp_db();
        seed(&db);
        build_native_trie_to_cf(&db).unwrap();
        let mut cache = NativeTrieCache::default();

        // Populate the cache with one cached commit.
        let dirty = apply_ops(&db, &[(0, key_for(0, 3), Some(vec![9; 9]))]);
        commit_native_trie_incremental_cached(&db, &dirty, &mut cache).unwrap();
        assert!(cache.is_populated());

        // Out-of-band: an UNCACHED commit advances the persisted trie behind
        // the cache's back.
        let dirty = apply_ops(&db, &[(2, b"oob-key".to_vec(), Some(vec![1; 7]))]);
        commit_native_trie_incremental(&db, &dirty).unwrap();

        // Cached commit on the now-stale cache: the root-match guard must
        // force a reload, and the result must still equal the oracle.
        let dirty = apply_ops(&db, &[(1, key_for(1, 8), Some(vec![2; 11]))]);
        let (root, _) = commit_native_trie_incremental_cached(&db, &dirty, &mut cache).unwrap();
        assert_eq!(root, native_root_full(&db).unwrap(), "post-reload root == oracle");
        assert_eq!(root, persisted_native_root(&db).unwrap());
    }

    /// A2.2 crash-consistency: an incremental commit's trie batch survives drop + reopen — the root
    /// recomputes identically from the persisted nodes.
    #[test]
    fn native_trie_survives_crash_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();
        let root = {
            let db = StateDb::open(&path).unwrap();
            seed(&db);
            build_native_trie_to_cf(&db).unwrap();
            let dirty = apply_ops(
                &db,
                &[
                    (0, key_for(0, 7), Some(vec![3; 12])),
                    (2, b"crash-key".to_vec(), Some(vec![4; 18])),
                    (3, key_for(3, 9), None),
                ],
            );
            let root = commit_native_trie_incremental(&db, &dirty).unwrap();
            assert_eq!(root, native_root_full(&db).unwrap());
            root
            // `db` dropped here — simulates process shutdown / crash.
        };
        let db = StateDb::open(&path).unwrap();
        assert_eq!(
            persisted_native_root(&db).unwrap(),
            root,
            "persisted native root changed after reopen"
        );
        assert_eq!(
            native_root_full(&db).unwrap(),
            root,
            "full-scan root changed after reopen"
        );
        drop(dir);
    }

    // ---- rank-root round-3: parallel rehash + bucket-member cache gates ----

    #[test]
    fn round3_parse_parallel_threads_default_off_and_capped() {
        assert_eq!(parse_parallel_threads(None), 1);
        assert_eq!(parse_parallel_threads(Some("1".into())), 1);
        assert_eq!(parse_parallel_threads(Some(" 6 ".into())), 6);
        assert_eq!(parse_parallel_threads(Some("2".into())), 2);
        // garbage / non-positive → serial
        for v in ["0", "-3", "on", "", "x", "true"] {
            assert_eq!(parse_parallel_threads(Some(v.into())), 1, "{v}");
        }
        // capped
        assert_eq!(
            parse_parallel_threads(Some("9999".into())),
            MAX_BUCKET_HASH_THREADS
        );
    }

    #[test]
    fn round3_parse_member_cache_mb_default_off() {
        assert_eq!(parse_member_cache_mb(None), 0);
        assert_eq!(parse_member_cache_mb(Some("0".into())), 0);
        assert_eq!(parse_member_cache_mb(Some("1".into())), 1024 * 1024);
        assert_eq!(parse_member_cache_mb(Some(" 8 ".into())), 8 * 1024 * 1024);
        for v in ["", "x", "-1", "on"] {
            assert_eq!(parse_member_cache_mb(Some(v.into())), 0, "{v}");
        }
    }

    // ---- L3 flush-pipe (a): adaptive parallel state-root threshold ----

    #[test]
    fn flushpipe_parse_bucket_hash_min_buckets_default_exact_today() {
        // Unset / garbage / < 1 → 1, which reproduces the pre-existing
        // `work.len() <= 1` gate byte-for-byte (parallel engages at >= 2 buckets).
        assert_eq!(parse_bucket_hash_min_buckets(None), 1);
        assert_eq!(parse_bucket_hash_min_buckets(Some("0".into())), 1);
        for v in ["", "x", "-1", "on", "true"] {
            assert_eq!(parse_bucket_hash_min_buckets(Some(v.into())), 1, "{v}");
        }
        // Valid crossover values pass through (e.g. the l3-diagnosis ~3000).
        assert_eq!(parse_bucket_hash_min_buckets(Some("1".into())), 1);
        assert_eq!(parse_bucket_hash_min_buckets(Some(" 3000 ".into())), 3000);
    }

    /// The adaptive threshold only SELECTS serial vs parallel; root + persisted
    /// trie/mirror bytes are byte-identical whichever side of the boundary the
    /// dirty-bucket count falls. Build a dirty set of exactly K buckets, apply it
    /// with parallel=4 on twin seeded DBs — one with `min_buckets=K` (K <= K ⇒
    /// SERIAL) and one with `min_buckets=K-1` (K > K-1 ⇒ PARALLEL) — assert equal,
    /// and equal to the serial-uncached full-scan oracle.
    #[test]
    fn flushpipe_bucket_hash_threshold_boundary_determinism() {
        let (db_serial, _d1) = temp_db();
        seed(&db_serial);
        build_native_trie_to_cf(&db_serial).unwrap();
        let (db_parallel, _d2) = temp_db();
        seed(&db_parallel);
        build_native_trie_to_cf(&db_parallel).unwrap();

        // Deterministic dirty set touching several distinct buckets.
        let mut ops = Vec::new();
        for i in 0..24u32 {
            ops.push((1usize, key_for(1, i), Some(vec![0xB0u8 ^ (i as u8); 40])));
        }
        let dirty_s = apply_ops(&db_serial, &ops);
        let dirty_p = apply_ops(&db_parallel, &ops);
        assert_eq!(dirty_s, dirty_p, "twin DBs must derive identical dirty maps");
        let k = dirty_bucket_count(&dirty_s);
        assert!(k >= 2, "need >= 2 buckets to distinguish the paths (got {k})");

        // SERIAL: min_buckets = k ⇒ work.len() (== k) <= k ⇒ serial even at parallel=4.
        let mut batch_s = WriteBatch::default();
        let apply_s =
            apply_native_dirty(&db_serial, &mut batch_s, &dirty_s, None, None, 4, k).unwrap();
        db_serial.write(batch_s).unwrap();

        // PARALLEL: min_buckets = k-1 ⇒ work.len() (== k) > k-1 ⇒ parallel path.
        let mut batch_p = WriteBatch::default();
        let apply_p =
            apply_native_dirty(&db_parallel, &mut batch_p, &dirty_p, None, None, 4, k - 1).unwrap();
        db_parallel.write(batch_p).unwrap();

        assert_eq!(
            apply_s.root, apply_p.root,
            "threshold boundary: root diverged serial vs parallel"
        );
        assert_eq!(
            dump(&db_serial, CF_NATIVE_TRIE),
            dump(&db_parallel, CF_NATIVE_TRIE),
            "threshold boundary: trie CF diverged"
        );
        assert_eq!(
            dump(&db_serial, CF_NATIVE_HASHED),
            dump(&db_parallel, CF_NATIVE_HASHED),
            "threshold boundary: mirror CF diverged"
        );
        assert_eq!(
            apply_s.root,
            native_root_full(&db_serial).unwrap(),
            "threshold boundary: root != serial-uncached full-scan oracle"
        );
    }

    fn xorshift(s: &mut u64) -> u64 {
        *s ^= *s << 13;
        *s ^= *s >> 7;
        *s ^= *s << 17;
        *s
    }

    /// One random adversarial block (mixed puts/deletes, varied lengths).
    fn gen_block(rng: &mut u64, round: u32) -> Vec<(usize, Vec<u8>, Option<Vec<u8>>)> {
        let mut ops = Vec::new();
        let n = (xorshift(rng) % 6) + 1;
        for _ in 0..n {
            let cfi = (xorshift(rng) % 6) as usize;
            let i = (xorshift(rng) % 160) as u32;
            let key = key_for(cfi, i);
            match xorshift(rng) % 4 {
                0 => ops.push((cfi, key, None)), // delete
                _ => {
                    let len = 4 + (xorshift(rng) % 40) as usize;
                    ops.push((cfi, key, Some(vec![(round as u8).wrapping_add(7); len])));
                }
            }
        }
        ops
    }

    /// THE round-3 differential gate: a long adversarial multi-block sequence
    /// (mixed puts/deletes/tombstones, empty-bucket transitions, clean rewrites,
    /// mid-sequence restart) applied to a SERIAL-UNCACHED reference DB and to a
    /// matrix of experimental DBs — every combination of {parallel 1/4/6/8} ×
    /// {trie cache on/off} × {member cache off / tiny(evicting) / roomy}. Root,
    /// full-scan oracle, and the persisted trie + mirror CF BYTES must stay
    /// identical after every block.
    #[test]
    fn round3_all_combos_byte_identical_to_serial_uncached() {
        struct Exp {
            db: StateDb,
            _dir: tempfile::TempDir,
            trie: Option<NativeTrieCache>,
            member: Option<NativeMemberCache>,
            parallel: usize,
        }
        let mk = |parallel: usize, trie: bool, mb_bytes: usize| -> Exp {
            let dir = tempfile::tempdir().unwrap();
            let db = StateDb::open(dir.path()).unwrap();
            seed(&db);
            build_native_trie_to_cf(&db).unwrap();
            Exp {
                db,
                _dir: dir,
                trie: trie.then(NativeTrieCache::default),
                member: (mb_bytes > 0).then(|| NativeMemberCache::with_budget(mb_bytes)),
                parallel,
            }
        };

        let (ref_db, _rd) = temp_db();
        seed(&ref_db);
        build_native_trie_to_cf(&ref_db).unwrap();

        let tiny = 4 * 1024; // forces eviction churn
        let roomy = 1 << 20;
        let mut exps = vec![
            mk(1, false, 0),      // == reference (self-check)
            mk(4, false, 0),      // parallel only
            mk(8, true, 0),       // parallel + trie cache
            mk(1, true, tiny),    // trie + member (evicting)
            mk(4, false, tiny),   // parallel + member (evicting, no trie)
            mk(8, true, roomy),   // all three, roomy budget
            mk(6, true, tiny),    // all three, eviction pressure
        ];

        let mut rng = 0xF00D_BABE_1234_5678u64;
        for round in 0..40u32 {
            if round == 17 {
                // simulated restart mid-sequence: drop every cache image.
                for e in exps.iter_mut() {
                    if let Some(c) = e.trie.as_mut() {
                        c.invalidate();
                    }
                    if let Some(c) = e.member.as_mut() {
                        c.invalidate();
                    }
                }
            }
            let mut ops = gen_block(&mut rng, round);
            // Every third round, mix in a CLEAN rewrite of a live entry (the
            // elision case) — read the value from the reference BEFORE applying,
            // so it is byte-identical across all DBs (they hold identical state).
            if round % 3 == 0 {
                let (cf_name, _) = NATIVE_ROOT_CFS[1];
                let key = key_for(1, 4);
                if let Some(v) = ref_db.get_cf_raw(cf_name, &key).unwrap() {
                    ops.push((1, key, Some(v)));
                }
            }

            let dirty = apply_ops(&ref_db, &ops);
            let ref_root = commit_native_trie_incremental(&ref_db, &dirty).unwrap();
            let ref_full = native_root_full(&ref_db).unwrap();
            assert_eq!(ref_root, ref_full, "round {round}: reference != full-scan");
            let ref_trie = dump(&ref_db, CF_NATIVE_TRIE);
            let ref_mirror = dump(&ref_db, CF_NATIVE_HASHED);

            for (idx, e) in exps.iter_mut().enumerate() {
                let d = apply_ops(&e.db, &ops);
                assert_eq!(d, dirty, "cfg {idx} round {round}: dirty map diverged");
                let (root, rehashed, scans) = commit_native_trie_incremental_full(
                    &e.db,
                    &d,
                    e.trie.as_mut(),
                    e.member.as_mut(),
                    e.parallel,
                )
                .unwrap();
                assert_eq!(root, ref_root, "cfg {idx} round {round}: root diverged");
                assert!(
                    rehashed <= dirty_bucket_count(&d),
                    "cfg {idx} round {round}: rehash count exceeds dirty buckets"
                );
                assert!(
                    scans <= dirty_bucket_count(&d),
                    "cfg {idx} round {round}: scans exceed dirty buckets"
                );
                assert_eq!(
                    dump(&e.db, CF_NATIVE_TRIE),
                    ref_trie,
                    "cfg {idx} round {round}: trie CF diverged"
                );
                assert_eq!(
                    dump(&e.db, CF_NATIVE_HASHED),
                    ref_mirror,
                    "cfg {idx} round {round}: mirror CF diverged"
                );
            }
        }
    }

    /// Determinism: the SAME input applied 20x through the parallel path (8
    /// workers) + both caches must yield a byte-identical root AND trie CF each
    /// time (parallel leaf hashing + serial fold ⇒ order-independent root).
    #[test]
    fn round3_parallel_determinism_20x() {
        let seq: Vec<Vec<(usize, Vec<u8>, Option<Vec<u8>>)>> = {
            let mut rng = 0x1357_9BDF_2468_ACE0u64;
            (0..25u32).map(|r| gen_block(&mut rng, r)).collect()
        };
        let run = || -> (B256, Vec<(Vec<u8>, Vec<u8>)>) {
            let (db, _d) = temp_db();
            seed(&db);
            build_native_trie_to_cf(&db).unwrap();
            let mut trie = NativeTrieCache::default();
            let mut member = NativeMemberCache::with_budget(64 * 1024);
            for ops in &seq {
                let dirty = apply_ops(&db, ops);
                commit_native_trie_incremental_full(
                    &db,
                    &dirty,
                    Some(&mut trie),
                    Some(&mut member),
                    8,
                )
                .unwrap();
            }
            (persisted_native_root(&db).unwrap(), dump(&db, CF_NATIVE_TRIE))
        };
        let (root0, trie0) = run();
        assert_eq!(root0, native_root_full_of_seq(&seq), "seq root mismatch");
        for k in 0..19 {
            let (r, t) = run();
            assert_eq!(r, root0, "parallel run {k}: root nondeterministic");
            assert_eq!(t, trie0, "parallel run {k}: trie CF nondeterministic");
        }
    }

    /// Oracle for the determinism test: replay the sequence on a fresh serial
    /// uncached DB and return the final full-scan root.
    fn native_root_full_of_seq(seq: &[Vec<(usize, Vec<u8>, Option<Vec<u8>>)>]) -> B256 {
        let (db, _d) = temp_db();
        seed(&db);
        build_native_trie_to_cf(&db).unwrap();
        for ops in seq {
            let dirty = apply_ops(&db, ops);
            commit_native_trie_incremental(&db, &dirty).unwrap();
        }
        native_root_full(&db).unwrap()
    }

    /// Structural scan-count witness: after warming buckets into the member
    /// cache, a block that re-touches ONLY resident buckets must perform ZERO
    /// mirror prefix-scans (the per-bucket-cost win) while staying correct.
    #[test]
    fn round3_member_cache_elides_scans_on_hits() {
        let (db, _d) = temp_db();
        seed(&db);
        build_native_trie_to_cf(&db).unwrap();
        let mut member = NativeMemberCache::with_budget(1 << 20); // roomy

        let warm: Vec<_> = (0..8u32)
            .map(|i| (0usize, key_for(0, i + 50), Some(vec![i as u8; 10])))
            .collect();
        let d = apply_ops(&db, &warm);
        let (_r, _rh, scans_cold) =
            commit_native_trie_incremental_full(&db, &d, None, Some(&mut member), 1).unwrap();
        assert!(scans_cold > 0, "cold warm-up must scan the mirror");

        // Re-touch the SAME keys (same buckets), now resident → zero scans.
        let re: Vec<_> = (0..8u32)
            .map(|i| (0usize, key_for(0, i + 50), Some(vec![(i + 1) as u8; 12])))
            .collect();
        let d2 = apply_ops(&db, &re);
        let (_r2, _rh2, scans_warm) =
            commit_native_trie_incremental_full(&db, &d2, None, Some(&mut member), 1).unwrap();
        assert_eq!(scans_warm, 0, "resident buckets must not re-scan the mirror");
        assert_eq!(
            persisted_native_root(&db).unwrap(),
            native_root_full(&db).unwrap(),
            "oracle must still agree after cached hits"
        );
    }

    /// L3 #2 witness: on a tombstone-laden mirror a cold miss on an EMPTY bucket
    /// must not walk the deleted range to reach the next live key. The bounded
    /// scan (production `read_bucket_members`) returns byte-identically to the
    /// unbounded scan for EVERY bucket, and is dramatically cheaper on the
    /// all-tombstone runs the cap-400 churn produces. Run with
    /// `cargo test -p torus-state read_bucket_members_bounded -- --nocapture`
    /// to see the before/after timings.
    #[test]
    fn read_bucket_members_bounded_skips_tombstones() {
        // Old, UNBOUNDED scan — the code before L3 #2, kept here as the A-side
        // oracle so the witness proves identical membership and measures cost.
        fn read_unbounded(db: &StateDb, bucket: u16) -> Members {
            let cf = db.cf_handle(CF_NATIVE_HASHED).unwrap();
            let prefix = bucket.to_be_bytes();
            let mut out = Members::new();
            let mut iter = db.inner().raw_iterator_cf(cf);
            iter.seek(prefix);
            while iter.valid() {
                let (k, v) = match (iter.key(), iter.value()) {
                    (Some(k), Some(v)) if k.starts_with(&prefix) && k.len() >= 3 => {
                        (k.to_vec(), B256::from_slice(v))
                    }
                    _ => break,
                };
                out.insert((k[2], k[3..].to_vec()), v);
                iter.next();
            }
            out
        }

        let (db, _d) = temp_db();
        let cf = db.cf_handle(CF_NATIVE_HASHED).unwrap();

        // Build a churny mirror: fill buckets 0..N with a couple of entries each,
        // flush to SST, then DELETE every bucket except the last (tombstones in a
        // fresh SST), and keep a handful of live buckets scattered near the top.
        const N: u16 = 6000;
        for b in 0..N {
            for t in 0u8..2 {
                let key = mirror_key(b, t, &b.to_le_bytes());
                db.put_cf_raw(CF_NATIVE_HASHED, &key, &[t; 32]).unwrap();
            }
        }
        db.inner().flush_cf(cf).unwrap();
        // Delete everything below N-1 (all tombstones), leave bucket N-1 live.
        for b in 0..(N - 1) {
            for t in 0u8..2 {
                let key = mirror_key(b, t, &b.to_le_bytes());
                db.delete_cf_raw(CF_NATIVE_HASHED, &key).unwrap();
            }
        }
        db.inner().flush_cf(cf).unwrap();

        // Correctness: bounded == unbounded for a spread of buckets (empty ones
        // amid tombstones, and the one live bucket).
        for &b in &[0u16, 1, 100, 3000, N - 2, N - 1] {
            assert_eq!(
                read_bucket_members(&db, b).unwrap(),
                read_unbounded(&db, b),
                "bounded scan must equal unbounded scan for bucket {b}"
            );
        }

        // Cost witness: scan the ~226 lowest (all-empty, tombstone-fronted)
        // buckets — the cap-400 cold-miss shape — both ways and print timings.
        const COLD: u16 = 226;
        let t0 = std::time::Instant::now();
        let mut sink = 0usize;
        for b in 0..COLD {
            sink += read_unbounded(&db, b).len();
        }
        let unbounded = t0.elapsed();
        let t1 = std::time::Instant::now();
        for b in 0..COLD {
            sink += read_bucket_members(&db, b).unwrap().len();
        }
        let bounded = t1.elapsed();
        assert_eq!(sink, 0, "these buckets are all-tombstone (empty)");
        println!(
            "L3#2 read_bucket_members over {COLD} tombstone-fronted empty buckets: \
             unbounded={unbounded:?} ({:.1}us/bkt)  bounded={bounded:?} ({:.1}us/bkt)",
            unbounded.as_secs_f64() * 1e6 / COLD as f64,
            bounded.as_secs_f64() * 1e6 / COLD as f64,
        );
    }

    /// Memory bound: under a very tight budget the member cache must evict, so
    /// residency stays bounded while correctness is preserved across churn.
    #[test]
    fn round3_member_cache_evicts_under_budget() {
        let (db, _d) = temp_db();
        seed(&db);
        build_native_trie_to_cf(&db).unwrap();
        let mut member = NativeMemberCache::with_budget(2048); // ~20-odd small entries

        for blk in 0..12u32 {
            let ops: Vec<_> = (0..12u32)
                .map(|i| {
                    let k = format!("evict-{blk}-{i}").into_bytes();
                    (2usize, k, Some(vec![0xEE; 28]))
                })
                .collect();
            let d = apply_ops(&db, &ops);
            commit_native_trie_incremental_full(&db, &d, None, Some(&mut member), 4).unwrap();
        }
        assert!(
            member.len() < 40,
            "tight budget must bound residency, got {}",
            member.len()
        );
        assert_eq!(
            persisted_native_root(&db).unwrap(),
            native_root_full(&db).unwrap(),
            "oracle must agree despite eviction churn"
        );
    }

    /// Shared-staleness guard: an out-of-band trie/mirror change (another
    /// writer / boot rebuild) makes the member cache's root disagree with the
    /// persisted marker — the next cached commit must FLUSH (re-scan), not trust
    /// stale members, and still equal the oracle.
    #[test]
    fn round3_member_cache_flushes_on_out_of_band_change() {
        let (db, _d) = temp_db();
        seed(&db);
        build_native_trie_to_cf(&db).unwrap();
        let mut member = NativeMemberCache::with_budget(1 << 20);

        // Populate the member cache.
        let d = apply_ops(&db, &[(0, key_for(0, 3), Some(vec![9; 9]))]);
        commit_native_trie_incremental_full(&db, &d, None, Some(&mut member), 1).unwrap();
        assert!(member.len() > 0, "cache should be populated");

        // Out-of-band: a plain uncached commit advances the trie behind the
        // member cache's back (root now disagrees).
        let d = apply_ops(&db, &[(0, key_for(0, 3), Some(vec![7; 21]))]);
        commit_native_trie_incremental(&db, &d).unwrap();

        // Next cached commit on the SAME bucket: the guard must flush + re-scan,
        // otherwise it would merge onto stale members and diverge.
        let d = apply_ops(&db, &[(0, key_for(0, 3), Some(vec![5; 13]))]);
        let (root, _rh, _sc) =
            commit_native_trie_incremental_full(&db, &d, None, Some(&mut member), 1).unwrap();
        assert_eq!(
            root,
            native_root_full(&db).unwrap(),
            "post-flush cached root must equal the oracle"
        );
    }

    // ---- s450 char-pins: dense-bucket characterization gates ----
    //
    // The natural key distribution at test scale puts <1 member in almost every
    // bucket, so the member-clone / member-merge paths are barely exercised by
    // the round-3 suite. These pins DELIBERATELY construct buckets with several
    // members each (by searching the keccak bucket assignment for collisions),
    // replay a multi-block sequence at several dirty-set sizes across ALL 6
    // native-root CFs (inserts, updates, clean rewrites, deletes, bucket-
    // emptying), and pin the exact root per block plus the exact persisted
    // trie + mirror bytes. Any change that perturbs a preimage byte, the member
    // merge order, or bucket assignment breaks them.

    /// Deterministic dense-bucket corpus for `tag`: scan `i in 0..scan`, group the
    /// generated keys by [`bucket_id`], and return the first `want` buckets (in
    /// bucket order) holding at least `per` keys, truncated to `per` keys each.
    fn dense_buckets(tag: u8, scan: u32, want: usize, per: usize) -> Vec<(u16, Vec<Vec<u8>>)> {
        let mut by_bucket: BTreeMap<u16, Vec<Vec<u8>>> = BTreeMap::new();
        for i in 0..scan {
            let mut key = i.to_le_bytes().to_vec();
            key.push(0xC0 ^ tag);
            if i % 3 == 0 {
                key.extend_from_slice(b"xx"); // vary key length (framing coverage)
            }
            by_bucket.entry(bucket_id(tag, &key)).or_default().push(key);
        }
        by_bucket
            .into_iter()
            .filter(|(_, ks)| ks.len() >= per)
            .map(|(b, mut ks)| {
                ks.truncate(per);
                (b, ks)
            })
            .take(want)
            .collect()
    }

    /// Members per dense bucket in the char-pin corpus.
    const CHARPIN_PER_BUCKET: usize = 6;
    /// Dense buckets per CF in the char-pin corpus.
    const CHARPIN_BUCKETS_PER_CF: usize = 120;

    /// Flat `(cf_index, key)` corpus over all 6 native-root CFs, every key living
    /// in a bucket shared with `CHARPIN_PER_BUCKET - 1` others of the same CF.
    fn charpin_corpus() -> Vec<(usize, Vec<u8>)> {
        let mut out = Vec::new();
        for (cfi, (_, tag)) in NATIVE_ROOT_CFS.iter().enumerate() {
            for (_, keys) in dense_buckets(*tag, 100_000, CHARPIN_BUCKETS_PER_CF, CHARPIN_PER_BUCKET)
            {
                for k in keys {
                    out.push((cfi, k));
                }
            }
        }
        out
    }

    /// One block of `(cf_index, key, Some(value) | None = delete)` ops.
    type Block = Vec<(usize, Vec<u8>, Option<Vec<u8>>)>;

    /// The pinned block sequence: an insert-everything block, then blocks of
    /// 1 / 10 / 500 / 2000 dirty entries mixing updates and deletes, then a block
    /// that empties whole buckets.
    fn charpin_sequence(corpus: &[(usize, Vec<u8>)]) -> Vec<Block> {
        let mut seq: Vec<Block> = Vec::new();
        let val = |round: u32, i: usize, len: usize| -> Vec<u8> {
            let mut v = vec![0u8; len];
            v[0] = round as u8;
            v[1] = (i & 0xff) as u8;
            v[2] = ((i >> 8) & 0xff) as u8;
            v
        };
        // Block 0: insert the whole corpus (every bucket becomes multi-member).
        seq.push(
            corpus
                .iter()
                .enumerate()
                .map(|(i, (cfi, k))| (*cfi, k.clone(), Some(val(0, i, 8 + i % 37))))
                .collect(),
        );
        // Blocks 1..4: 1 / 10 / 500 / 2000 dirty entries, updates + deletes.
        let mut rng = 0x5150_4A50_C0FF_EE01u64;
        for (idx, size) in [1usize, 10, 500, 2000].into_iter().enumerate() {
            let round = idx as u32 + 1;
            let mut ops = Vec::new();
            let mut used = std::collections::HashSet::new();
            while ops.len() < size {
                let i = (xorshift(&mut rng) % corpus.len() as u64) as usize;
                if !used.insert(i) {
                    continue;
                }
                let (cfi, k) = &corpus[i];
                // 1-in-5 deletes; the rest updates with round-varying lengths.
                if xorshift(&mut rng).is_multiple_of(5) {
                    ops.push((*cfi, k.clone(), None));
                } else {
                    ops.push((*cfi, k.clone(), Some(val(round, i, 4 + (i % 53)))));
                }
            }
            ops.sort();
            seq.push(ops);
        }
        // Final block: delete every key of the first 20 dense buckets — drives
        // whole buckets to empty (leaf -> default, node deletions).
        seq.push(
            corpus
                .iter()
                .take(20 * CHARPIN_PER_BUCKET)
                .map(|(cfi, k)| (*cfi, k.clone(), None))
                .collect(),
        );
        seq
    }

    /// Byte digest of a CF's full contents (length-framed, so it pins bytes AND
    /// boundaries).
    fn dump_digest(db: &StateDb, cf_name: &str) -> B256 {
        let mut buf = Vec::new();
        for (k, v) in dump(db, cf_name) {
            buf.extend_from_slice(&(k.len() as u32).to_le_bytes());
            buf.extend_from_slice(&k);
            buf.extend_from_slice(&(v.len() as u32).to_le_bytes());
            buf.extend_from_slice(&v);
        }
        keccak256(&buf)
    }

    /// Replay `seq` on a fresh DB with the given config, returning the per-block
    /// roots and the final (trie, mirror) CF digests.
    fn charpin_replay(
        seq: &[Block],
        trie_on: bool,
        member_bytes: usize,
        parallel: usize,
    ) -> (Vec<B256>, B256, B256) {
        let (db, _d) = temp_db();
        build_native_trie_to_cf(&db).unwrap();
        let mut trie = trie_on.then(NativeTrieCache::default);
        let mut member = (member_bytes > 0).then(|| NativeMemberCache::with_budget(member_bytes));
        let mut roots = Vec::with_capacity(seq.len());
        for ops in seq {
            let dirty = apply_ops(&db, ops);
            let (root, _rh, _sc) = commit_native_trie_incremental_full(
                &db,
                &dirty,
                trie.as_mut(),
                member.as_mut(),
                parallel,
            )
            .unwrap();
            assert_eq!(
                root,
                native_root_full(&db).unwrap(),
                "incremental root != full-scan oracle"
            );
            roots.push(root);
        }
        (
            roots,
            dump_digest(&db, CF_NATIVE_TRIE),
            dump_digest(&db, CF_NATIVE_HASHED),
        )
    }

    // Captured from the pre-refactor (31a5739) serial-uncached path. CONSENSUS
    // PINS: only a deliberate, fleet-uniform preimage change may move these.
    const CHARPIN_FINAL_ROOT: &str =
        "a221f2cd2bdcd308d77d521bee3e8c99efb9e64cbb652441b916943e2227695a";
    const CHARPIN_SEQ_DIGEST: &str =
        "b9292d39a785674b1ad0396339202b181435ec783aa6c0e74369f56bf982700a";
    const CHARPIN_TRIE_DIGEST: &str =
        "81dea124a20c5e6d23d358c9cb7f17a214092466d7ed21c6fe6e7258d67102d6";
    const CHARPIN_MIRROR_DIGEST: &str =
        "6af9f5181d1e592c88fdcfbad2a3bb2f8ad1fa90b7df96e2d2ea7d5d74f3b09a";

    /// CHAR-PIN: exact per-block roots and exact persisted trie/mirror bytes for a
    /// dense-bucket multi-block sequence over all 6 native-root CFs — under every
    /// cache/parallelism combination (member cache OFF / tiny-evicting = MISS-heavy /
    /// roomy = HIT-heavy). The constants are consensus-visible: a diff here means a
    /// preimage moved.
    #[test]
    fn charpin_dense_bucket_roots_and_bytes() {
        let corpus = charpin_corpus();
        assert_eq!(
            corpus.len(),
            6 * CHARPIN_BUCKETS_PER_CF * CHARPIN_PER_BUCKET,
            "dense corpus shape changed — the pins below no longer apply"
        );
        let seq = charpin_sequence(&corpus);
        assert_eq!(seq.len(), 6, "sequence shape changed");

        // Reference: serial, uncached.
        let (roots, trie_digest, mirror_digest) = charpin_replay(&seq, false, 0, 1);
        let seq_digest = {
            let mut buf = Vec::new();
            for r in &roots {
                buf.extend_from_slice(r.as_slice());
            }
            keccak256(&buf)
        };

        assert_eq!(
            format!("{:x}", roots[roots.len() - 1]),
            CHARPIN_FINAL_ROOT,
            "FINAL NATIVE ROOT CHANGED — a consensus preimage moved"
        );
        assert_eq!(
            format!("{seq_digest:x}"),
            CHARPIN_SEQ_DIGEST,
            "per-block root sequence changed — a consensus preimage moved"
        );
        assert_eq!(
            format!("{trie_digest:x}"),
            CHARPIN_TRIE_DIGEST,
            "persisted CF_NATIVE_TRIE bytes changed"
        );
        assert_eq!(
            format!("{mirror_digest:x}"),
            CHARPIN_MIRROR_DIGEST,
            "persisted CF_NATIVE_HASHED bytes changed"
        );

        // Every cache / parallelism combination must reproduce the SAME pins.
        // budget 0 = member cache disabled, 4 KiB = evicting (miss-heavy),
        // 8 MiB = roomy (hit-heavy: the multi-member base-set path).
        for (trie_on, mb, par) in [
            (false, 0usize, 4usize),
            (true, 0, 1),
            (true, 0, 8),
            (false, 4 * 1024, 1),
            (true, 4 * 1024, 4),
            (false, 8 << 20, 1),
            (true, 8 << 20, 1),
            (true, 8 << 20, 8),
        ] {
            let (r, t, m) = charpin_replay(&seq, trie_on, mb, par);
            assert_eq!(
                r, roots,
                "cfg (trie={trie_on}, mb={mb}, par={par}): roots diverged"
            );
            assert_eq!(
                t, trie_digest,
                "cfg (trie={trie_on}, mb={mb}, par={par}): trie bytes diverged"
            );
            assert_eq!(
                m, mirror_digest,
                "cfg (trie={trie_on}, mb={mb}, par={par}): mirror bytes diverged"
            );
        }
    }

    /// CHAR-PIN: member-cache residency semantics on MULTI-MEMBER buckets — a hit
    /// must serve the bucket without a mirror scan, an all-clean-rewrite block
    /// (fully elided, nothing changed) must LEAVE THE BUCKET RESIDENT, and a
    /// delete that empties a bucket must keep the oracle in agreement.
    #[test]
    fn charpin_member_cache_residency_and_tombstones() {
        let (db, _d) = temp_db();
        build_native_trie_to_cf(&db).unwrap();
        let dense = dense_buckets(0, 100_000, 12, CHARPIN_PER_BUCKET);
        assert_eq!(dense.len(), 12, "need 12 dense buckets");
        let keys: Vec<Vec<u8>> = dense.iter().flat_map(|(_, ks)| ks.clone()).collect();

        let mut trie = NativeTrieCache::default();
        let mut member = NativeMemberCache::with_budget(8 << 20);

        // Block 1 — insert everything (cold: every bucket scans).
        let ops: Vec<_> = keys
            .iter()
            .enumerate()
            .map(|(i, k)| (0usize, k.clone(), Some(vec![i as u8; 16])))
            .collect();
        let d = apply_ops(&db, &ops);
        let (_r, _rh, scans_cold) =
            commit_native_trie_incremental_full(&db, &d, Some(&mut trie), Some(&mut member), 1)
                .unwrap();
        assert_eq!(scans_cold, 12, "cold block must scan every dirty bucket");
        assert_eq!(member.len(), 12, "all 12 buckets must be resident after commit");

        // Block 2 — CLEAN rewrite of the same values: fully elided, nothing
        // changes, and the buckets must STAY resident (a hit must put back what
        // it took).
        let d2 = apply_ops(&db, &ops);
        let (_r2, rehashed2, scans2) =
            commit_native_trie_incremental_full(&db, &d2, Some(&mut trie), Some(&mut member), 1)
                .unwrap();
        assert_eq!(scans2, 0, "resident buckets must not re-scan");
        assert_eq!(rehashed2, 0, "clean rewrites must be fully elided");
        assert_eq!(member.len(), 12, "an elided hit must leave the bucket resident");

        // Block 3 — real updates on resident buckets: still zero scans.
        let ops3: Vec<_> = keys
            .iter()
            .enumerate()
            .map(|(i, k)| (0usize, k.clone(), Some(vec![(i as u8) ^ 0x5A; 24])))
            .collect();
        let d3 = apply_ops(&db, &ops3);
        let (r3, _rh3, scans3) =
            commit_native_trie_incremental_full(&db, &d3, Some(&mut trie), Some(&mut member), 1)
                .unwrap();
        assert_eq!(
            scans3, 0,
            "resident buckets must not re-scan after a real change"
        );
        assert_eq!(member.len(), 12, "buckets stay resident after a real change");
        assert_eq!(r3, native_root_full(&db).unwrap(), "oracle agrees");

        // Block 4 — delete EVERY member of the first 4 buckets (buckets go empty)
        // plus a delete of an absent key, all on resident buckets.
        let mut ops4: Vec<(usize, Vec<u8>, Option<Vec<u8>>)> = dense
            .iter()
            .take(4)
            .flat_map(|(_, ks)| ks.iter().map(|k| (0usize, k.clone(), None)))
            .collect();
        ops4.push((0, b"charpin-absent-key".to_vec(), None));
        let d4 = apply_ops(&db, &ops4);
        let (r4, _rh4, _sc4) =
            commit_native_trie_incremental_full(&db, &d4, Some(&mut trie), Some(&mut member), 1)
                .unwrap();
        assert_eq!(
            r4,
            native_root_full(&db).unwrap(),
            "oracle agrees after emptying"
        );

        // Block 5 — re-insert into the emptied buckets; the resident (now empty)
        // member sets must still serve without a scan and stay correct.
        let ops5: Vec<_> = dense
            .iter()
            .take(4)
            .flat_map(|(_, ks)| ks.iter().map(|k| (0usize, k.clone(), Some(vec![0x77; 9]))))
            .collect();
        let d5 = apply_ops(&db, &ops5);
        let (r5, _rh5, scans5) =
            commit_native_trie_incremental_full(&db, &d5, Some(&mut trie), Some(&mut member), 1)
                .unwrap();
        assert_eq!(
            r5,
            native_root_full(&db).unwrap(),
            "oracle agrees after re-insert"
        );
        assert_eq!(scans5, 0, "emptied-but-resident buckets must not re-scan");
    }

    // ---- s450 allocation witness ----
    //
    // The refactor's whole claim is "fewer allocations, identical bytes". The
    // byte half is pinned above; this counts the allocation half. A per-THREAD
    // counting allocator (installed only in this crate's `cfg(test)` lib binary)
    // makes the numbers exact and load-independent — unlike wall-clock, which on a
    // busy box disagreed with itself by 5x this session.

    /// Thread-scoped allocation counters. Per-thread (not global) so the count is
    /// unaffected by whatever else the test harness runs in parallel.
    mod alloc_count {
        use std::alloc::{GlobalAlloc, Layout, System};
        use std::cell::Cell;

        thread_local! {
            static ALLOCS: Cell<u64> = const { Cell::new(0) };
            static BYTES: Cell<u64> = const { Cell::new(0) };
        }

        pub struct Counting;

        #[inline]
        fn bump(size: usize) {
            let _ = ALLOCS.try_with(|c| c.set(c.get().wrapping_add(1)));
            let _ = BYTES.try_with(|c| c.set(c.get().wrapping_add(size as u64)));
        }

        // SAFETY: every method forwards to `System` unchanged; the counters are
        // thread-local `Cell`s with const initialisers, so they never allocate
        // and never re-enter the allocator.
        unsafe impl GlobalAlloc for Counting {
            unsafe fn alloc(&self, l: Layout) -> *mut u8 {
                bump(l.size());
                unsafe { System.alloc(l) }
            }
            unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
                unsafe { System.dealloc(p, l) }
            }
            unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
                bump(l.size());
                unsafe { System.alloc_zeroed(l) }
            }
            unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
                bump(new);
                unsafe { System.realloc(p, l, new) }
            }
        }

        /// `(allocations, bytes)` performed by `f` ON THIS THREAD.
        pub fn measure<R>(f: impl FnOnce() -> R) -> (u64, u64, R) {
            let a0 = ALLOCS.with(Cell::get);
            let b0 = BYTES.with(Cell::get);
            let r = f();
            let a1 = ALLOCS.with(Cell::get);
            let b1 = BYTES.with(Cell::get);
            (a1 - a0, b1 - b0, r)
        }
    }

    #[global_allocator]
    static COUNTING_ALLOCATOR: alloc_count::Counting = alloc_count::Counting;

    /// s450 ALLOCATION WITNESS — the copying this refactor removed, counted
    /// exactly, on a dense-bucket DB (`CHARPIN_PER_BUCKET` members per bucket)
    /// with warm trie + member caches, serial so every allocation lands on this
    /// thread. Three measurements, each over the VERBATIM code shape involved:
    ///
    ///  A. dirty-set handoff, pre-s450: `PendingState::native_dirty`'s clone-out
    ///     loop PLUS `apply_native_dirty`'s old `by_bucket` clone-again loop.
    ///  B. dirty-set handoff, s450: `native_dirty_ref`'s borrow-out loop plus the
    ///     current borrowing `by_bucket` loop.
    ///  C. the per-bucket `Members` deep clone a member-cache HIT used to pay at
    ///     `process_bucket` — counted on the real resident sets, which is exactly
    ///     what the cache holds (`read_bucket_members` is how they got there).
    ///
    /// Plus the absolute cost of one whole cached block through the real
    /// `apply_native_dirty`, for scale.
    ///
    /// NOTE: measuring `apply_native_dirty(owned)` vs `apply_native_dirty(borrowed)`
    /// would show ZERO delta — the function is generic over `Borrow<[u8]>` and
    /// borrows internally either way. The removed clones are in the CONSTRUCTION
    /// of the dirty set and in the old grouping loop, which is what A/B measure.
    ///
    /// Run: `cargo test -p torus-state --lib alloc_witness -- --nocapture`
    #[test]
    fn alloc_witness_native_trie_block() {
        let (db, _d) = temp_db();
        build_native_trie_to_cf(&db).unwrap();
        let dense = dense_buckets(0, 100_000, 300, CHARPIN_PER_BUCKET);
        let keys: Vec<Vec<u8>> = dense.iter().flat_map(|(_, ks)| ks.clone()).collect();

        // Seed: every bucket multi-member (CHARPIN_PER_BUCKET members each).
        let seed_ops: Vec<_> = keys
            .iter()
            .enumerate()
            .map(|(i, k)| (0usize, k.clone(), Some(vec![i as u8; 48])))
            .collect();
        let seed_dirty = apply_ops(&db, &seed_ops);
        let mut trie = NativeTrieCache::default();
        let mut member = NativeMemberCache::with_budget(64 << 20);
        commit_native_trie_incremental_full(
            &db,
            &seed_dirty,
            Some(&mut trie),
            Some(&mut member),
            1,
        )
        .unwrap();
        assert_eq!(member.len(), dense.len(), "all buckets resident (warm)");

        // The measured block: touch ONE key in every dense bucket (all cache
        // hits). `pending` has the exact shape of `CfPending::writes` — the map
        // the flush actually reads its dirty set out of.
        let pending: BTreeMap<Vec<u8>, Vec<u8>> = dense
            .iter()
            .map(|(_, ks)| (ks[0].clone(), vec![0xD1; 44]))
            .collect();
        for (k, v) in &pending {
            db.put_cf_raw(NATIVE_ROOT_CFS[0].0, k, v).unwrap();
        }
        let nb = dense.len() as u64;
        let ne = pending.len() as u64;

        // --- A: pre-s450 handoff (clone out of the overlay, then clone again) ---
        #[allow(clippy::type_complexity)]
        let (a_old, b_old, _) = alloc_count::measure(|| {
            // verbatim `PendingState::native_dirty` (pre-s450)
            let mut dirty: BTreeMap<(u8, Vec<u8>), Option<Vec<u8>>> = BTreeMap::new();
            for (key, value) in &pending {
                dirty.insert((0u8, key.clone()), Some(value.clone()));
            }
            // verbatim `apply_native_dirty` step 1 (pre-s450)
            let mut by_bucket: BTreeMap<u16, Vec<((u8, Vec<u8>), Option<Vec<u8>>)>> =
                BTreeMap::new();
            for ((tag, key), val) in &dirty {
                let b = bucket_id(*tag, key);
                by_bucket
                    .entry(b)
                    .or_default()
                    .push(((*tag, key.clone()), val.clone()));
            }
            by_bucket.len()
        });

        // --- B: s450 handoff (borrow out, group by reference) ---
        let (a_new, b_new, _) = alloc_count::measure(|| {
            // verbatim `PendingState::native_dirty_ref`
            let mut dirty: BTreeMap<(u8, &[u8]), Option<&[u8]>> = BTreeMap::new();
            for (key, value) in &pending {
                dirty.insert((0u8, key.as_slice()), Some(value.as_slice()));
            }
            // verbatim `apply_native_dirty` step 1 (s450)
            let mut by_bucket: BTreeMap<u16, Vec<DirtyRef<'_>>> = BTreeMap::new();
            for ((tag, key), val) in &dirty {
                let key: &[u8] = key.borrow();
                let b = bucket_id(*tag, key);
                by_bucket
                    .entry(b)
                    .or_default()
                    .push((*tag, key, val.as_ref().map(Borrow::borrow)));
            }
            by_bucket.len()
        });

        // --- C: the per-bucket Members deep clone a HIT used to pay ---
        let resident: Vec<Members> = dense
            .iter()
            .map(|(b, _)| read_bucket_members(&db, *b).unwrap())
            .collect();
        let (a_clone, b_clone, _) = alloc_count::measure(|| {
            let mut sink = 0usize;
            for m in &resident {
                sink += m.clone().len(); // == the old `(**arc).clone()` at process_bucket
            }
            sink
        });

        // --- scale: one whole cached block through the real path, s450 shape ---
        let borrowed: BTreeMap<(u8, &[u8]), Option<&[u8]>> = pending
            .iter()
            .map(|(k, v)| ((0u8, k.as_slice()), Some(v.as_slice())))
            .collect();
        let (a_block, b_block, _) = alloc_count::measure(|| {
            let mut batch = WriteBatch::default();
            apply_native_dirty(
                &db,
                &mut batch,
                &borrowed,
                Some(&mut trie),
                Some(&mut member),
                1,
                1,
            )
            .unwrap()
        });

        println!(
            "S450-ALLOC shape: dirty_entries={ne} dirty_buckets={nb} members_per_bucket={CHARPIN_PER_BUCKET}\n\
             S450-ALLOC A dirty handoff PRE-s450 (clone + clone again)  allocs={a_old:6} bytes={b_old:8}  ({:.2}/entry)\n\
             S450-ALLOC B dirty handoff s450     (borrow + borrow)      allocs={a_new:6} bytes={b_new:8}  ({:.2}/entry)\n\
             S450-ALLOC   => removed                                    allocs={:6} bytes={:8}  ({:.2}/entry)\n\
             S450-ALLOC C Members deep clone on member-cache HITS       allocs={a_clone:6} bytes={b_clone:8}  ({:.2}/bucket)\n\
             S450-ALLOC   => removed entirely (Arc handover + try_unwrap)\n\
             S450-ALLOC   whole cached block via apply_native_dirty     allocs={a_block:6} bytes={b_block:8}  ({:.2}/bucket)",
            a_old as f64 / ne as f64,
            a_new as f64 / ne as f64,
            a_old - a_new,
            b_old - b_new,
            (a_old - a_new) as f64 / ne as f64,
            a_clone as f64 / nb as f64,
            a_block as f64 / nb as f64,
        );

        // Regression guards — loose enough to survive unrelated churn, tight
        // enough to fail if the copying comes back.
        assert!(
            a_old >= a_new + 4 * ne,
            "pre-s450 handoff must cost >= 4 allocations/entry more ({a_old} vs {a_new}, {ne} entries)"
        );
        assert!(
            a_clone >= nb,
            "the removed deep clone allocated at least once per bucket ({a_clone} over {nb})"
        );
        // Per-bucket ceiling for a whole cached block. At the time of writing this
        // is ~23/bucket, almost all of it the depth-16 tree fold's per-changed-node
        // key `Vec`s (a term in dirty-BUCKETS, not in total state size). Restoring
        // the removed copying would add (4/entry + 7/bucket) ≈ 34/bucket and trip
        // this.
        assert!(
            a_block < 30 * nb,
            "cached block allocations regressed: {a_block} over {nb} buckets \
             ({:.1}/bucket, ceiling 30)",
            a_block as f64 / nb as f64
        );
    }

    /// Per-bucket cost harness (round-3 proof-bench input). Measures the marginal
    /// per-dirty-bucket cost of native-trie maintenance at ~10k dirty buckets for
    /// serial-scan / parallel-scan / member-cached (no-scan) paths. Ignored by
    /// default; run with:
    ///   cargo test -p torus-state --lib -- --ignored --nocapture round3_perf_per_bucket_10k
    #[test]
    #[ignore]
    fn round3_perf_per_bucket_10k() {
        // THIN ~2 members/bucket (mirror scan cheap) and FAT ~14 members/bucket
        // (mirror scan is the member cache's target cost).
        for (n_seed, tag) in [(120_000u32, "THIN~2mem"), (900_000u32, "FAT~14mem")] {
            println!("PERF ===== scenario {tag} (n_seed={n_seed}) =====");
            round3_perf_scenario(n_seed);
        }
    }

    fn round3_perf_scenario(n_seed: u32) {
        use std::time::Instant;
        let (db, _d) = temp_db();
        let mkkey = |i: u32| {
            let mut k = i.to_le_bytes().to_vec();
            k.push(0x11);
            k
        };
        for i in 0..n_seed {
            let (cf, _) = NATIVE_ROOT_CFS[(i % 6) as usize];
            db.put_cf_raw(cf, &mkkey(i), &vec![0xAB; 48]).unwrap();
        }
        build_native_trie_to_cf(&db).unwrap();

        // 10k DISTINCT buckets drawn from seeded (populated) keys, so each dirty
        // bucket pays a realistic member scan on the uncached path.
        let target = 10_000usize;
        let mut seen = std::collections::HashSet::new();
        let mut chosen: Vec<(u8, Vec<u8>)> = Vec::new();
        let mut i = 0u32;
        while chosen.len() < target && i < n_seed {
            let tag = (i % 6) as u8;
            let key = mkkey(i);
            if seen.insert(bucket_id(tag, &key)) {
                chosen.push((tag, key));
            }
            i += 1;
        }
        assert_eq!(chosen.len(), target, "need {target} distinct buckets");

        // Warm a trie node cache + member cache for those buckets (and commit, so
        // the persisted trie/mirror and both images agree). Big budgets => no
        // eviction. The trie cache turns the depth-16 sibling fold into RAM reads;
        // the member cache turns the mirror scan into a RAM hit — the two per-bucket
        // terms round-3 attacks. Measured WITHOUT them, the uncached DB fold
        // (~depth-16 point reads / changed leaf) dominates and masks both.
        let mut warm_trie = NativeTrieCache::default();
        let mut warm_member = NativeMemberCache::with_budget(512 * 1024 * 1024);
        let d0: BTreeMap<(u8, Vec<u8>), Option<Vec<u8>>> = chosen
            .iter()
            .map(|(t, k)| ((*t, k.clone()), Some(vec![0xC0; 40])))
            .collect();
        for ((t, k), v) in &d0 {
            let (cf, _) = NATIVE_ROOT_CFS[*t as usize];
            db.put_cf_raw(cf, k, v.as_ref().unwrap()).unwrap();
        }
        commit_native_trie_incremental_full(
            &db,
            &d0,
            Some(&mut warm_trie),
            Some(&mut warm_member),
            1,
        )
        .unwrap();

        // Measured dirty set: same buckets, new values (real rehash). NOT committed
        // — timing only reads + builds a throwaway batch, so it is repeatable and
        // the warm images stay warm across reps.
        let d1: BTreeMap<(u8, Vec<u8>), Option<Vec<u8>>> = chosen
            .iter()
            .map(|(t, k)| ((*t, k.clone()), Some(vec![0xD1; 44])))
            .collect();
        let nb = dirty_bucket_count(&d1);

        let bench = |label: &str,
                     mut trie: Option<&mut NativeTrieCache>,
                     mut member: Option<&mut NativeMemberCache>,
                     parallel: usize|
         -> f64 {
            let mut best = f64::INFINITY;
            for _ in 0..6 {
                // s450: a member-cache HIT hands the resident set out of the
                // cache, so an apply that is never committed leaves it drained.
                // Re-warm with an idempotent (uncached, hence non-eliding) commit
                // of the warm-up set before each timed rep — untimed, and it
                // rewrites identical bytes so the persisted trie/root do not move.
                if member.is_some() {
                    commit_native_trie_incremental_full(
                        &db,
                        &d0,
                        None,
                        member.as_deref_mut(),
                        1,
                    )
                    .unwrap();
                }
                let mut batch = WriteBatch::default();
                let t = Instant::now();
                apply_native_dirty(
                    &db,
                    &mut batch,
                    &d1,
                    trie.as_deref_mut(),
                    member.as_deref_mut(),
                    parallel,
                    1,
                )
                .unwrap();
                best = best.min(t.elapsed().as_secs_f64());
            }
            let per_us = best / nb as f64 * 1e6;
            println!("PERF {label:30} total={best:.4}s  per_bucket={per_us:.3}us");
            per_us
        };

        println!("PERF dirty_buckets={nb}");
        // Uncached DB fold (round-1 baseline): the tree fold's DB point reads swamp
        // the per-bucket scan+hash term.
        let a = bench("serial_uncachedfold_scan", None, None, 1);
        let b = bench("parallel8_uncachedfold_scan", None, None, 8);
        // Trie cache on (round-2 baseline = production recommended): fold from RAM,
        // so the residual per-bucket cost is scan + keccak — exactly round-3's target.
        let c = bench("serial_triecache_scan", Some(&mut warm_trie), None, 1);
        let d = bench("parallel8_triecache_scan", Some(&mut warm_trie), None, 8);
        let e = bench(
            "serial_triecache_membercache",
            Some(&mut warm_trie),
            Some(&mut warm_member),
            1,
        );
        let f = bench(
            "parallel8_triecache_membercache",
            Some(&mut warm_trie),
            Some(&mut warm_member),
            8,
        );
        println!("PERF summary (round-1 baseline = serial_uncachedfold_scan):");
        println!("PERF   parallel_on_uncachedfold   {a:.1}->{b:.1}us  = {:.2}x", a / b);
        println!("PERF   triecache_fold_elim         {a:.1}->{c:.1}us  = {:.2}x", a / c);
        println!("PERF (round-2 baseline = serial_triecache_scan; isolates round-3's per-bucket term):");
        println!("PERF   parallel_on_triecache       {c:.1}->{d:.1}us  = {:.2}x", c / d);
        println!("PERF   membercache_scan_elim        {c:.1}->{e:.1}us  = {:.2}x", c / e);
        println!("PERF   full_stack (par8+member)     {c:.1}->{f:.1}us  = {:.2}x", c / f);
    }
}
