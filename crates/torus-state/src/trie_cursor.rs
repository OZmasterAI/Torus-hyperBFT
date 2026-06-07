//! RocksDB-backed reth trie cursors + trie-node persistence (Phase A: incremental state root).
//!
//! Activates the `CF_TRIE_ACCOUNTS` / `CF_TRIE_STORAGE` column families as the persistent store
//! of intermediate Merkle-Patricia branch nodes consumed by reth's `StateRoot` engine. By reading
//! these nodes through [`TrieCursorFactory`], `StateRoot` skips recomputation of unchanged subtrees,
//! turning the O(total-state) root scan into O(changed)/block (the Phase A block-speed goal).
//!
//! ## Key layout
//! RocksDB orders keys by raw bytes, and `StateRoot`'s cursors require iteration in `Nibbles`
//! lexicographic order. `Nibbles::to_vec()` yields one byte per nibble (`0x0..=0xf`), so raw byte
//! order equals nibble order — the cursors are correct by construction.
//! - `CF_TRIE_ACCOUNTS`: key = account-trie path nibbles (1 byte/nibble), value = branch node.
//! - `CF_TRIE_STORAGE`:  key = `keccak(address)(32) ++` storage-trie path nibbles, value = branch
//!   node. The 32-byte hashed-address prefix isolates each account's storage trie (no cross-account
//!   collision) and lets a per-account cursor prefix-scan its own subtree.
//!
//! ## Node value encoding
//! `BranchNodeCompact` is stored with a small explicit layout (below) rather than serde/RLP: its
//! `hashes: Arc<Vec<B256>>` field would otherwise pull in serde's `rc` feature. This encoding is
//! purely internal and never affects the computed root — only round-trip fidelity matters, which is
//! asserted by the A1.1 round-trip test here and, end-to-end, by the A1.2/A1.3 determinism gates.

use std::sync::Arc;

use alloy_primitives::{B256, U256};
use reth_primitives_traits::Account;
use reth_storage_errors::db::DatabaseError;
use reth_trie::hashed_cursor::{HashedCursor, HashedCursorFactory, HashedStorageCursor};
use reth_trie::trie_cursor::{TrieCursor, TrieCursorFactory, TrieStorageCursor};
use reth_trie_common::updates::TrieUpdates;
use reth_trie_common::{BranchNodeCompact, Nibbles, TrieMask};
use rocksdb::{DBRawIterator, WriteBatch};

use crate::cf::{CF_HASHED_ACCOUNTS, CF_HASHED_STORAGE, CF_TRIE_ACCOUNTS, CF_TRIE_STORAGE};
use crate::db::{decode_account_info, StateDb};
use crate::error::StateError;

/// Length of the hashed-address prefix on storage-trie keys.
const HASHED_ADDR_LEN: usize = 32;

// ---- Node key encoding ----

/// Account-trie node key: the path nibbles, one byte per nibble.
pub fn account_trie_key(path: &Nibbles) -> Vec<u8> {
    path.to_vec()
}

/// Storage-trie node key: `keccak(address)(32) ++ path nibbles`.
pub fn storage_trie_key(hashed_address: &B256, path: &Nibbles) -> Vec<u8> {
    let nibbles = path.to_vec();
    let mut key = Vec::with_capacity(HASHED_ADDR_LEN + nibbles.len());
    key.extend_from_slice(hashed_address.as_slice());
    key.extend_from_slice(&nibbles);
    key
}

// ---- Node value encoding ----
//
//   state_mask(u16 BE) tree_mask(u16 BE) hash_mask(u16 BE) hash_count(u16 BE)
//   hash_count * B256(32)   root_hash_flag(u8)   [root_hash B256(32) if flag == 1]

fn encode_branch_node(node: &BranchNodeCompact) -> Vec<u8> {
    let mut buf = Vec::with_capacity(8 + node.hashes.len() * 32 + 33);
    buf.extend_from_slice(&node.state_mask.get().to_be_bytes());
    buf.extend_from_slice(&node.tree_mask.get().to_be_bytes());
    buf.extend_from_slice(&node.hash_mask.get().to_be_bytes());
    buf.extend_from_slice(&(node.hashes.len() as u16).to_be_bytes());
    for hash in node.hashes.iter() {
        buf.extend_from_slice(hash.as_slice());
    }
    match node.root_hash {
        Some(root_hash) => {
            buf.push(1);
            buf.extend_from_slice(root_hash.as_slice());
        }
        None => buf.push(0),
    }
    buf
}

fn decode_branch_node(bytes: &[u8]) -> Result<BranchNodeCompact, StateError> {
    let invalid =
        |what: &str| StateError::InvalidData(format!("trie branch node: {what} (len {})", bytes.len()));

    if bytes.len() < 8 {
        return Err(invalid("truncated header"));
    }
    let state_mask = TrieMask::new(u16::from_be_bytes([bytes[0], bytes[1]]));
    let tree_mask = TrieMask::new(u16::from_be_bytes([bytes[2], bytes[3]]));
    let hash_mask = TrieMask::new(u16::from_be_bytes([bytes[4], bytes[5]]));
    let count = u16::from_be_bytes([bytes[6], bytes[7]]) as usize;

    let mut offset = 8;
    let hashes_end = offset + count * 32;
    if bytes.len() < hashes_end + 1 {
        return Err(invalid("truncated hashes"));
    }
    let mut hashes = Vec::with_capacity(count);
    for _ in 0..count {
        hashes.push(B256::from_slice(&bytes[offset..offset + 32]));
        offset += 32;
    }
    let root_hash = match bytes[offset] {
        0 => None,
        1 => {
            offset += 1;
            if bytes.len() < offset + 32 {
                return Err(invalid("truncated root_hash"));
            }
            Some(B256::from_slice(&bytes[offset..offset + 32]))
        }
        other => return Err(invalid(&format!("bad root_hash flag {other}"))),
    };

    // Construct by field (not `BranchNodeCompact::new`, which debug-asserts mask/hash invariants):
    // stored nodes are trusted, and any corruption surfaces as a root mismatch via the determinism
    // gate rather than a panic on the read path.
    Ok(BranchNodeCompact { state_mask, tree_mask, hash_mask, hashes: Arc::new(hashes), root_hash })
}

// ---- Persistence: fold TrieUpdates into a WriteBatch ----

/// Persist a [`TrieUpdates`] (changed + removed account/storage branch nodes) into `batch`.
///
/// Callers commit `batch` in the SAME atomic write as the EVM state change (A1.4), keeping the trie
/// and state crash-consistent. Storage tries flagged `is_deleted` are wiped before their new nodes
/// are written.
pub fn write_trie_updates(
    db: &StateDb,
    batch: &mut WriteBatch,
    updates: &TrieUpdates,
) -> Result<(), StateError> {
    // Removes BEFORE upserts: RocksDB applies batch ops in insertion order, and a full recompute
    // can list a path in BOTH `removed_nodes` and `account_nodes` (e.g. prefix-set "all"). Deleting
    // first lets the upsert win, so a re-emitted node ends up present rather than wiped.
    let acc_cf = db.cf_handle(CF_TRIE_ACCOUNTS)?;
    for path in &updates.removed_nodes {
        batch.delete_cf(acc_cf, account_trie_key(path));
    }
    for (path, node) in &updates.account_nodes {
        batch.put_cf(acc_cf, account_trie_key(path), encode_branch_node(node));
    }

    let stor_cf = db.cf_handle(CF_TRIE_STORAGE)?;
    for (hashed_address, storage) in &updates.storage_tries {
        if storage.is_deleted {
            // Wipe every persisted node under this account's 32-byte prefix.
            let mut iter = db.inner().raw_iterator_cf(stor_cf);
            iter.seek(hashed_address.as_slice());
            while iter.valid() {
                let key = match iter.key() {
                    Some(k) if k.starts_with(hashed_address.as_slice()) => k.to_vec(),
                    _ => break,
                };
                batch.delete_cf(stor_cf, &key);
                iter.next();
            }
            iter.status()?;
        }
        for path in &storage.removed_nodes {
            batch.delete_cf(stor_cf, storage_trie_key(hashed_address, path));
        }
        for (path, node) in &storage.storage_nodes {
            batch.put_cf(stor_cf, storage_trie_key(hashed_address, path), encode_branch_node(node));
        }
    }
    Ok(())
}

// ---- reth TrieCursorFactory over RocksDB ----

/// A [`TrieCursorFactory`] backed by `CF_TRIE_ACCOUNTS` / `CF_TRIE_STORAGE`.
///
/// Handed to reth's `StateRoot` so it can lazily read persisted branch nodes by path.
#[derive(Clone, Copy)]
pub struct RocksTrieCursorFactory<'a> {
    db: &'a StateDb,
}

impl<'a> RocksTrieCursorFactory<'a> {
    pub fn new(db: &'a StateDb) -> Self {
        Self { db }
    }
}

impl<'f> TrieCursorFactory for RocksTrieCursorFactory<'f> {
    type AccountTrieCursor<'a>
        = RocksTrieCursor<'a>
    where
        Self: 'a;
    type StorageTrieCursor<'a>
        = RocksTrieCursor<'a>
    where
        Self: 'a;

    fn account_trie_cursor(&self) -> Result<Self::AccountTrieCursor<'_>, DatabaseError> {
        RocksTrieCursor::new(self.db, CF_TRIE_ACCOUNTS, None)
    }

    fn storage_trie_cursor(
        &self,
        hashed_address: B256,
    ) -> Result<Self::StorageTrieCursor<'_>, DatabaseError> {
        RocksTrieCursor::new(self.db, CF_TRIE_STORAGE, Some(hashed_address))
    }
}

/// Cursor over persisted trie branch nodes in a single column family.
///
/// `prefix == None` → account trie (`CF_TRIE_ACCOUNTS`); `prefix == Some(addr)` → that account's
/// storage trie (`CF_TRIE_STORAGE`), scanning only keys under the 32-byte hashed-address prefix.
pub struct RocksTrieCursor<'a> {
    db: &'a StateDb,
    cf_name: &'static str,
    prefix: Option<[u8; HASHED_ADDR_LEN]>,
    iter: DBRawIterator<'a>,
    current: Option<Nibbles>,
}

impl<'a> RocksTrieCursor<'a> {
    fn new(
        db: &'a StateDb,
        cf_name: &'static str,
        hashed_address: Option<B256>,
    ) -> Result<Self, DatabaseError> {
        let iter = Self::make_iter(db, cf_name)?;
        Ok(Self { db, cf_name, prefix: hashed_address.map(|a| a.0), iter, current: None })
    }

    fn make_iter(db: &'a StateDb, cf_name: &'static str) -> Result<DBRawIterator<'a>, DatabaseError> {
        let cf = db.cf_handle(cf_name).map_err(to_db_err)?;
        Ok(db.inner().raw_iterator_cf(cf))
    }

    /// Full RocksDB key for a seek to `path` (prepends the storage prefix when present).
    fn seek_bytes(&self, path: &Nibbles) -> Vec<u8> {
        let nibbles = path.to_vec();
        match &self.prefix {
            Some(prefix) => {
                let mut key = Vec::with_capacity(HASHED_ADDR_LEN + nibbles.len());
                key.extend_from_slice(prefix);
                key.extend_from_slice(&nibbles);
                key
            }
            None => nibbles,
        }
    }

    /// Decode the entry at the current iterator position, stripping the storage prefix. Returns
    /// `None` past-end or once the cursor leaves its account's prefix.
    fn read_current(&mut self) -> Result<Option<(Nibbles, BranchNodeCompact)>, DatabaseError> {
        self.iter.status().map_err(to_db_err)?;
        if !self.iter.valid() {
            self.current = None;
            return Ok(None);
        }
        let raw_key = self.iter.key().expect("valid iterator has a key");
        let nibble_bytes: &[u8] = match &self.prefix {
            Some(prefix) => {
                if raw_key.len() < HASHED_ADDR_LEN || &raw_key[..HASHED_ADDR_LEN] != prefix {
                    self.current = None;
                    return Ok(None);
                }
                &raw_key[HASHED_ADDR_LEN..]
            }
            None => raw_key,
        };
        let path = Nibbles::from_nibbles_unchecked(nibble_bytes);
        let value = self.iter.value().expect("valid iterator has a value");
        let node = decode_branch_node(value).map_err(to_db_err)?;
        self.current = Some(path.clone());
        Ok(Some((path, node)))
    }
}

impl<'a> TrieCursor for RocksTrieCursor<'a> {
    fn seek_exact(
        &mut self,
        key: Nibbles,
    ) -> Result<Option<(Nibbles, BranchNodeCompact)>, DatabaseError> {
        let bytes = self.seek_bytes(&key);
        self.iter.seek(&bytes);
        match self.read_current()? {
            Some((path, node)) if path == key => Ok(Some((path, node))),
            _ => Ok(None),
        }
    }

    fn seek(
        &mut self,
        key: Nibbles,
    ) -> Result<Option<(Nibbles, BranchNodeCompact)>, DatabaseError> {
        let bytes = self.seek_bytes(&key);
        self.iter.seek(&bytes);
        self.read_current()
    }

    fn next(&mut self) -> Result<Option<(Nibbles, BranchNodeCompact)>, DatabaseError> {
        if !self.iter.valid() {
            self.current = None;
            return Ok(None);
        }
        self.iter.next();
        self.read_current()
    }

    fn current(&mut self) -> Result<Option<Nibbles>, DatabaseError> {
        Ok(self.current.clone())
    }

    fn reset(&mut self) {
        // Recreate the underlying iterator; the trait contract requires the next op to be a seek.
        if let Ok(iter) = Self::make_iter(self.db, self.cf_name) {
            self.iter = iter;
        }
        self.current = None;
    }
}

impl<'a> TrieStorageCursor for RocksTrieCursor<'a> {
    fn set_hashed_address(&mut self, hashed_address: B256) {
        self.prefix = Some(hashed_address.0);
        if let Ok(iter) = Self::make_iter(self.db, self.cf_name) {
            self.iter = iter;
        }
        self.current = None;
    }
}

fn to_db_err<E: std::fmt::Display>(err: E) -> DatabaseError {
    DatabaseError::Other(err.to_string())
}

// ---- reth HashedCursorFactory over RocksDB (the keccak-ordered hashed-state mirror) ----

/// A [`HashedCursorFactory`] backed by `CF_HASHED_ACCOUNTS` / `CF_HASHED_STORAGE`.
///
/// Provides reth's `StateRoot` with post-state account/storage values iterated in keccak(key)
/// order (Ethereum state-trie order). Account values are the existing 72-byte `AccountInfo`
/// encoding, converted to reth's `Account` on read.
#[derive(Clone, Copy)]
pub struct RocksHashedCursorFactory<'a> {
    db: &'a StateDb,
}

impl<'a> RocksHashedCursorFactory<'a> {
    pub fn new(db: &'a StateDb) -> Self {
        Self { db }
    }
}

impl<'f> HashedCursorFactory for RocksHashedCursorFactory<'f> {
    type AccountCursor<'a>
        = RocksHashedAccountCursor<'a>
    where
        Self: 'a;
    type StorageCursor<'a>
        = RocksHashedStorageCursor<'a>
    where
        Self: 'a;

    fn hashed_account_cursor(&self) -> Result<Self::AccountCursor<'_>, DatabaseError> {
        RocksHashedAccountCursor::new(self.db)
    }

    fn hashed_storage_cursor(
        &self,
        hashed_address: B256,
    ) -> Result<Self::StorageCursor<'_>, DatabaseError> {
        RocksHashedStorageCursor::new(self.db, hashed_address)
    }
}

/// Cursor over hashed accounts (`CF_HASHED_ACCOUNTS`), keyed by keccak(address).
pub struct RocksHashedAccountCursor<'a> {
    db: &'a StateDb,
    iter: DBRawIterator<'a>,
}

impl<'a> RocksHashedAccountCursor<'a> {
    fn new(db: &'a StateDb) -> Result<Self, DatabaseError> {
        let cf = db.cf_handle(CF_HASHED_ACCOUNTS).map_err(to_db_err)?;
        Ok(Self { db, iter: db.inner().raw_iterator_cf(cf) })
    }

    fn read_current(&mut self) -> Result<Option<(B256, Account)>, DatabaseError> {
        self.iter.status().map_err(to_db_err)?;
        if !self.iter.valid() {
            return Ok(None);
        }
        let key = self.iter.key().expect("valid iterator has a key");
        if key.len() != 32 {
            return Err(to_db_err(format!("hashed account key len {} != 32", key.len())));
        }
        let hashed_address = B256::from_slice(key);
        let info = decode_account_info(self.iter.value().expect("valid value")).map_err(to_db_err)?;
        Ok(Some((hashed_address, Account::from(&info))))
    }
}

impl<'a> HashedCursor for RocksHashedAccountCursor<'a> {
    type Value = Account;

    fn seek(&mut self, key: B256) -> Result<Option<(B256, Account)>, DatabaseError> {
        self.iter.seek(key.as_slice());
        self.read_current()
    }

    fn next(&mut self) -> Result<Option<(B256, Account)>, DatabaseError> {
        if !self.iter.valid() {
            return Ok(None);
        }
        self.iter.next();
        self.read_current()
    }

    fn reset(&mut self) {
        if let Ok(cf) = self.db.cf_handle(CF_HASHED_ACCOUNTS) {
            self.iter = self.db.inner().raw_iterator_cf(cf);
        }
    }
}

/// Cursor over one account's hashed storage (`CF_HASHED_STORAGE`), keyed by
/// keccak(address) ++ keccak(slot), scanning only the 32-byte address prefix.
pub struct RocksHashedStorageCursor<'a> {
    db: &'a StateDb,
    prefix: [u8; HASHED_ADDR_LEN],
    iter: DBRawIterator<'a>,
}

impl<'a> RocksHashedStorageCursor<'a> {
    fn new(db: &'a StateDb, hashed_address: B256) -> Result<Self, DatabaseError> {
        let cf = db.cf_handle(CF_HASHED_STORAGE).map_err(to_db_err)?;
        Ok(Self { db, prefix: hashed_address.0, iter: db.inner().raw_iterator_cf(cf) })
    }

    fn refresh_iter(&mut self) {
        if let Ok(cf) = self.db.cf_handle(CF_HASHED_STORAGE) {
            self.iter = self.db.inner().raw_iterator_cf(cf);
        }
    }

    fn read_current(&mut self) -> Result<Option<(B256, U256)>, DatabaseError> {
        self.iter.status().map_err(to_db_err)?;
        if !self.iter.valid() {
            return Ok(None);
        }
        let key = self.iter.key().expect("valid iterator has a key");
        if key.len() != HASHED_ADDR_LEN + 32 || !key.starts_with(&self.prefix) {
            return Ok(None);
        }
        let hashed_slot = B256::from_slice(&key[HASHED_ADDR_LEN..]);
        let value = U256::from_be_slice(self.iter.value().expect("valid value"));
        Ok(Some((hashed_slot, value)))
    }
}

impl<'a> HashedCursor for RocksHashedStorageCursor<'a> {
    type Value = U256;

    fn seek(&mut self, subkey: B256) -> Result<Option<(B256, U256)>, DatabaseError> {
        let mut key = Vec::with_capacity(HASHED_ADDR_LEN + 32);
        key.extend_from_slice(&self.prefix);
        key.extend_from_slice(subkey.as_slice());
        self.iter.seek(&key);
        self.read_current()
    }

    fn next(&mut self) -> Result<Option<(B256, U256)>, DatabaseError> {
        if !self.iter.valid() {
            return Ok(None);
        }
        self.iter.next();
        self.read_current()
    }

    fn reset(&mut self) {
        self.refresh_iter();
    }
}

impl<'a> HashedStorageCursor for RocksHashedStorageCursor<'a> {
    fn is_storage_empty(&mut self) -> Result<bool, DatabaseError> {
        self.iter.seek(&self.prefix);
        self.iter.status().map_err(to_db_err)?;
        let has_entry =
            self.iter.valid() && self.iter.key().map_or(false, |k| k.starts_with(&self.prefix));
        Ok(!has_entry)
    }

    fn set_hashed_address(&mut self, hashed_address: B256) {
        self.prefix = hashed_address.0;
        self.refresh_iter();
    }
}

#[cfg(test)]
mod tests {
    use super::{write_trie_updates, RocksTrieCursorFactory};
    use crate::cf::{CF_TRIE_ACCOUNTS, CF_TRIE_STORAGE};
    use crate::db::StateDb;
    use alloy_primitives::B256;
    use reth_trie::trie_cursor::{TrieCursor, TrieCursorFactory, TrieStorageCursor};
    use reth_trie_common::updates::{StorageTrieUpdates, TrieUpdates};
    use reth_trie_common::{BranchNodeCompact, Nibbles, TrieMask};
    use rocksdb::WriteBatch;

    fn temp_db() -> (StateDb, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = StateDb::open(dir.path()).expect("open db");
        (db, dir)
    }

    /// Build a valid `BranchNodeCompact` whose `hash_mask` matches `hashes.len()`.
    fn branch(hashes: Vec<B256>) -> BranchNodeCompact {
        let bits = (1u32 << hashes.len()) - 1;
        let mask = TrieMask::new(bits as u16);
        BranchNodeCompact::new(mask, TrieMask::new(0), mask, hashes, None)
    }

    #[test]
    fn trie_cursor_roundtrip_absent_and_no_cross_account_collision() {
        let (db, _dir) = temp_db();

        // Trie CFs exist and are wired up.
        assert!(db.cf_handle(CF_TRIE_ACCOUNTS).is_ok());
        assert!(db.cf_handle(CF_TRIE_STORAGE).is_ok());

        let p12 = Nibbles::from_nibbles_unchecked([0x1u8, 0x2]);
        let p13 = Nibbles::from_nibbles_unchecked([0x1u8, 0x3]);
        let absent = Nibbles::from_nibbles_unchecked([0x9u8, 0x9]);

        let acc_a = branch(vec![B256::repeat_byte(0xa1), B256::repeat_byte(0xa2)]);
        let acc_b = branch(vec![B256::repeat_byte(0xb1), B256::repeat_byte(0xb2)]);

        let addr1 = B256::repeat_byte(0x11);
        let addr2 = B256::repeat_byte(0x22);
        let s1 = branch(vec![B256::repeat_byte(0x51)]);
        let s2 = branch(vec![B256::repeat_byte(0x52)]);

        let mut updates = TrieUpdates::default();
        updates.account_nodes.insert(p12.clone(), acc_a.clone());
        updates.account_nodes.insert(p13.clone(), acc_b.clone());
        let mut st1 = StorageTrieUpdates::default();
        st1.storage_nodes.insert(p12.clone(), s1.clone());
        let mut st2 = StorageTrieUpdates::default();
        st2.storage_nodes.insert(p12.clone(), s2.clone());
        updates.storage_tries.insert(addr1, st1);
        updates.storage_tries.insert(addr2, st2);

        let mut batch = WriteBatch::default();
        write_trie_updates(&db, &mut batch, &updates).expect("write trie updates");
        db.write(batch).expect("commit batch");

        let factory = RocksTrieCursorFactory::new(&db);

        // Account trie: exact round-trip.
        let mut ac = factory.account_trie_cursor().expect("account cursor");
        assert_eq!(ac.seek_exact(p12.clone()).unwrap(), Some((p12.clone(), acc_a.clone())));
        assert_eq!(ac.seek_exact(p13.clone()).unwrap(), Some((p13.clone(), acc_b.clone())));
        // Absent path -> None.
        assert_eq!(ac.seek_exact(absent.clone()).unwrap(), None);
        // seek(>=) lands on the matching key.
        assert_eq!(ac.seek(p12.clone()).unwrap(), Some((p12.clone(), acc_a.clone())));

        // Storage trie: an identical path under two accounts must NOT collide.
        let mut sc1 = factory.storage_trie_cursor(addr1).expect("storage cursor 1");
        assert_eq!(sc1.seek_exact(p12.clone()).unwrap(), Some((p12.clone(), s1.clone())));
        let mut sc2 = factory.storage_trie_cursor(addr2).expect("storage cursor 2");
        assert_eq!(sc2.seek_exact(p12.clone()).unwrap(), Some((p12.clone(), s2.clone())));
        assert_ne!(s1, s2);

        // set_hashed_address reuse: switch cursor 1 to addr2 -> now sees s2, not s1.
        sc1.set_hashed_address(addr2);
        assert_eq!(sc1.seek_exact(p12.clone()).unwrap(), Some((p12.clone(), s2)));
    }
}
