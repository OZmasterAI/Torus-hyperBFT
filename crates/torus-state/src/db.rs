//! RocksDB wrapper with column family layout and revm `DatabaseRef` implementation.

use std::path::Path;
use std::sync::Arc;

use alloy_primitives::{Address, Bytes, B256, U256};
use revm::bytecode::Bytecode;
use revm::state::AccountInfo;
use rocksdb::{
    BlockBasedOptions, Cache, ColumnFamily, ColumnFamilyDescriptor, DBCompressionType, Options,
    WriteBatch, DB,
};

use crate::cf::*;
use crate::error::StateError;

/// Keccak256 of empty bytes — the code hash for accounts with no code.
pub const KECCAK_EMPTY: B256 = B256::new([
    0xc5, 0xd2, 0x46, 0x01, 0x86, 0xf7, 0x23, 0x3c, 0x92, 0x7e, 0x7d, 0xb2, 0xdc, 0xc7, 0x03, 0xc0,
    0xe5, 0x00, 0xb6, 0x53, 0xca, 0x82, 0x27, 0x3b, 0x7b, 0xfa, 0xd8, 0x04, 0x5d, 0x85, 0xa4, 0x70,
]);

/// Central RocksDB database handle for the Torus node.
///
/// Opens all column families defined in section 6.1. Provides typed accessors
/// for EVM state (accounts, storage, code) and implements `revm::DatabaseRef`.
#[derive(Clone)]
pub struct StateDb {
    db: Arc<DB>,
}

impl StateDb {
    /// Open (or create) the database at the given path with all column families.
    pub fn open(path: &Path) -> Result<Self, StateError> {
        let mut opts = Options::default();
        opts.create_if_missing(true);
        opts.create_missing_column_families(true);
        // DB-wide: parallelize flush/compaction and smooth fsync spikes during
        // heavy block writes. (Stock defaults run only 2 background jobs.)
        opts.set_max_background_jobs(4);
        opts.set_bytes_per_sync(1 << 20); // 1 MiB

        // Per-CF tuning, shared across every column family. RocksDB ships an
        // ~8 MiB block cache and NO bloom filters by default, which is poor for
        // this node's point-lookup-heavy access (account / code / native-action
        // -by-hash). A shared cache + bloom filters is the main read win here.
        let cache = Cache::new_lru_cache(256 * 1024 * 1024); // 256 MiB shared block cache
        let mut bbt = BlockBasedOptions::default();
        bbt.set_block_cache(&cache);
        bbt.set_bloom_filter(10.0, false); // ~1% false positives on point lookups
        bbt.set_block_size(16 * 1024); // 16 KiB blocks
        bbt.set_cache_index_and_filter_blocks(true);
        bbt.set_pin_l0_filter_and_index_blocks_in_cache(true);

        let mut cf_opts = Options::default();
        cf_opts.set_block_based_table_factory(&bbt);
        cf_opts.set_write_buffer_size(128 * 1024 * 1024); // 128 MiB memtable
        cf_opts.set_max_write_buffer_number(4);
        cf_opts.set_compression_type(DBCompressionType::Lz4);
        cf_opts.set_bottommost_compression_type(DBCompressionType::Zstd);
        cf_opts.set_level_compaction_dynamic_level_bytes(true);

        let cf_descriptors: Vec<ColumnFamilyDescriptor> = ALL_CF_NAMES
            .iter()
            .map(|name| ColumnFamilyDescriptor::new(*name, cf_opts.clone()))
            .collect();

        let db = DB::open_cf_descriptors(&opts, path, cf_descriptors)?;
        Ok(Self { db: Arc::new(db) })
    }

    /// Wrap an already-opened RocksDB instance (e.g., a read-only snapshot DB).
    pub fn from_existing_db(db: DB) -> Self {
        Self { db: Arc::new(db) }
    }

    /// Destroy the database at the given path (for testing).
    pub fn destroy(path: &Path) -> Result<(), StateError> {
        DB::destroy(&Options::default(), path)?;
        Ok(())
    }

    /// Get a reference to the underlying RocksDB instance.
    pub fn inner(&self) -> &DB {
        &self.db
    }

    /// Get a shared handle to the underlying RocksDB instance.
    ///
    /// Used by [`torus_consensus::RocksKVStore::new`] which needs `Arc<DB>`.
    pub fn db_arc(&self) -> Arc<DB> {
        self.db.clone()
    }

    fn cf(&self, name: &str) -> Result<&ColumnFamily, StateError> {
        self.db
            .cf_handle(name)
            .ok_or_else(|| StateError::MissingColumnFamily(name.to_string()))
    }

    /// Get a column family handle by name (public, for WriteBatch usage).
    pub fn cf_handle(&self, name: &str) -> Result<&ColumnFamily, StateError> {
        self.cf(name)
    }

    /// Atomically apply a WriteBatch to the database.
    pub fn write(&self, batch: WriteBatch) -> Result<(), StateError> {
        self.db.write(batch)?;
        Ok(())
    }

    // ---- Account operations (cf_accounts) ----

    /// Get account info by address. Returns `None` for non-existent accounts.
    pub fn get_account(&self, address: &Address) -> Result<Option<AccountInfo>, StateError> {
        let cf = self.cf(CF_ACCOUNTS)?;
        match self.db.get_cf(cf, address.as_slice())? {
            Some(data) => Ok(Some(decode_account_info(&data)?)),
            None => Ok(None),
        }
    }

    /// Store account info. The `code` field is stored separately in cf_code.
    pub fn put_account(&self, address: &Address, info: &AccountInfo) -> Result<(), StateError> {
        let cf = self.cf(CF_ACCOUNTS)?;
        self.db
            .put_cf(cf, address.as_slice(), encode_account_info(info))?;
        Ok(())
    }

    /// Delete an account.
    pub fn delete_account(&self, address: &Address) -> Result<(), StateError> {
        let cf = self.cf(CF_ACCOUNTS)?;
        self.db.delete_cf(cf, address.as_slice())?;
        Ok(())
    }

    // ---- Storage operations (cf_storage) ----

    /// Get a storage slot value. Returns `U256::ZERO` for unset slots.
    pub fn get_storage(&self, address: &Address, index: &U256) -> Result<U256, StateError> {
        let cf = self.cf(CF_STORAGE)?;
        let key = storage_key(address, index);
        match self.db.get_cf(cf, key)? {
            Some(data) => {
                if data.len() != 32 {
                    return Err(StateError::InvalidData(format!(
                        "storage value len {} != 32",
                        data.len()
                    )));
                }
                Ok(U256::from_be_slice(&data))
            }
            None => Ok(U256::ZERO),
        }
    }

    /// Set a storage slot. Zero values are deleted to save space.
    pub fn put_storage(
        &self,
        address: &Address,
        index: &U256,
        value: &U256,
    ) -> Result<(), StateError> {
        let cf = self.cf(CF_STORAGE)?;
        let key = storage_key(address, index);
        if value.is_zero() {
            self.db.delete_cf(cf, key)?;
        } else {
            self.db.put_cf(cf, key, value.to_be_bytes::<32>())?;
        }
        Ok(())
    }

    // ---- Code operations (cf_code) ----

    /// Get contract bytecode by its keccak256 hash.
    pub fn get_code(&self, code_hash: &B256) -> Result<Option<Vec<u8>>, StateError> {
        let cf = self.cf(CF_CODE)?;
        Ok(self.db.get_cf(cf, code_hash.as_slice())?)
    }

    /// Store contract bytecode keyed by its keccak256 hash.
    pub fn put_code(&self, code_hash: &B256, code: &[u8]) -> Result<(), StateError> {
        let cf = self.cf(CF_CODE)?;
        self.db.put_cf(cf, code_hash.as_slice(), code)?;
        Ok(())
    }

    // ---- Block hash operations ----

    /// Get block hash by block number.
    pub fn get_block_hash(&self, number: u64) -> Result<Option<B256>, StateError> {
        let cf = self.cf(CF_BLOCK_HEADERS)?;
        let key = number.to_be_bytes();
        match self.db.get_cf(cf, key)? {
            Some(data) if data.len() >= 32 => Ok(Some(B256::from_slice(&data[..32]))),
            _ => Ok(None),
        }
    }

    /// Store a block hash mapping (number -> hash and hash -> number).
    pub fn put_block_hash(&self, number: u64, hash: &B256) -> Result<(), StateError> {
        let cf = self.cf(CF_BLOCK_HEADERS)?;
        self.db.put_cf(cf, number.to_be_bytes(), hash.as_slice())?;

        let cf_reverse = self.cf(CF_BLOCK_HASH_TO_NUMBER)?;
        self.db
            .put_cf(cf_reverse, hash.as_slice(), number.to_be_bytes())?;
        Ok(())
    }

    // ---- Raw CF access (for other crates) ----

    /// Get a raw value from any column family.
    pub fn get_cf_raw(&self, cf_name: &str, key: &[u8]) -> Result<Option<Vec<u8>>, StateError> {
        let cf = self.cf(cf_name)?;
        Ok(self.db.get_cf(cf, key)?)
    }

    /// Presence check for a key in any column family without copying the value
    /// (pinned read). For hot paths that only need to know a (multi-KB) value is
    /// already local — e.g. the native-DA pre-warm filter.
    pub fn exists_cf_raw(&self, cf_name: &str, key: &[u8]) -> Result<bool, StateError> {
        let cf = self.cf(cf_name)?;
        Ok(self.db.get_pinned_cf(cf, key)?.is_some())
    }

    /// Put a raw value into any column family.
    pub fn put_cf_raw(&self, cf_name: &str, key: &[u8], value: &[u8]) -> Result<(), StateError> {
        let cf = self.cf(cf_name)?;
        self.db.put_cf(cf, key, value)?;
        Ok(())
    }

    /// Delete a key from any column family.
    pub fn delete_cf_raw(&self, cf_name: &str, key: &[u8]) -> Result<(), StateError> {
        let cf = self.cf(cf_name)?;
        self.db.delete_cf(cf, key)?;
        Ok(())
    }

    // ---- Session key operations (cf_sessions) ----

    /// Get a session by its ed25519 public key.
    pub fn get_session(&self, pubkey: &[u8; 32]) -> Result<Option<torus_types::SessionData>, StateError> {
        match self.get_cf_raw(crate::cf::CF_SESSIONS, pubkey)? {
            Some(bytes) => {
                let data: torus_types::SessionData = serde_json::from_slice(&bytes)
                    .map_err(|e| StateError::InvalidData(e.to_string()))?;
                Ok(Some(data))
            }
            None => Ok(None),
        }
    }

    /// Store a session.
    pub fn put_session(&self, pubkey: &[u8; 32], data: &torus_types::SessionData) -> Result<(), StateError> {
        let bytes = serde_json::to_vec(data)
            .map_err(|e| StateError::InvalidData(e.to_string()))?;
        self.put_cf_raw(crate::cf::CF_SESSIONS, pubkey, &bytes)
    }

    /// Delete a session.
    pub fn delete_session(&self, pubkey: &[u8; 32]) -> Result<(), StateError> {
        self.delete_cf_raw(crate::cf::CF_SESSIONS, pubkey)
    }

    /// Count active sessions for an owner address.
    /// Scans all sessions (acceptable since max 5 per owner, total count is bounded).
    pub fn count_sessions_for_owner(&self, owner: &alloy_primitives::Address) -> Result<usize, StateError> {
        let cf = self.cf(crate::cf::CF_SESSIONS)?;
        let iter = self.db.iterator_cf(cf, rocksdb::IteratorMode::Start);
        let mut count = 0;
        for item in iter {
            if let Ok((_key, value)) = item {
                if let Ok(data) = serde_json::from_slice::<torus_types::SessionData>(&value) {
                    if data.owner == *owner {
                        count += 1;
                    }
                }
            }
        }
        Ok(count)
    }

    /// List all column family names that were opened.
    pub fn column_families(&self) -> &'static [&'static str] {
        ALL_CF_NAMES
    }

    // ---- Iteration (for trie computation) ----

    /// Collect all accounts from the database.
    pub fn all_accounts(&self) -> Result<Vec<(Address, AccountInfo)>, StateError> {
        let cf = self.cf(CF_ACCOUNTS)?;
        let iter = self.db.iterator_cf(cf, rocksdb::IteratorMode::Start);
        let mut accounts = Vec::new();
        for item in iter {
            let (key, value) = item?;
            if key.len() != 20 {
                return Err(StateError::InvalidData(format!(
                    "account key len {} != 20",
                    key.len()
                )));
            }
            let address = Address::from_slice(&key);
            let info = decode_account_info(&value)?;
            accounts.push((address, info));
        }
        Ok(accounts)
    }

    /// Collect all storage slots for a given address.
    pub fn account_storage(&self, address: &Address) -> Result<Vec<(U256, U256)>, StateError> {
        let cf = self.cf(CF_STORAGE)?;
        let prefix = address.as_slice();
        let iter = self.db.prefix_iterator_cf(cf, prefix);
        let mut slots = Vec::new();
        for item in iter {
            let (key, value) = item?;
            if !key.starts_with(prefix) {
                break;
            }
            if key.len() != 52 {
                return Err(StateError::InvalidData(format!(
                    "storage key len {} != 52",
                    key.len()
                )));
            }
            let slot = U256::from_be_slice(&key[20..]);
            if value.len() != 32 {
                return Err(StateError::InvalidData(format!(
                    "storage value len {} != 32",
                    value.len()
                )));
            }
            let val = U256::from_be_slice(&value);
            slots.push((slot, val));
        }
        Ok(slots)
    }
}

// ---- revm DatabaseRef implementation ----
//
// We implement `DatabaseRef` (&self) rather than `Database` (&mut self) because
// RocksDB reads are thread-safe. Use `revm::database::WrapDatabaseRef<StateDb>`
// to get a `Database` impl, or pass to `StateBuilder::with_database_ref()`.

impl revm::DatabaseRef for StateDb {
    type Error = StateError;

    fn basic_ref(&self, address: Address) -> Result<Option<AccountInfo>, Self::Error> {
        self.get_account(&address)
    }

    fn code_by_hash_ref(&self, code_hash: B256) -> Result<Bytecode, Self::Error> {
        if code_hash == KECCAK_EMPTY || code_hash == B256::ZERO {
            return Ok(Bytecode::default());
        }
        match self.get_code(&code_hash)? {
            Some(bytes) => Ok(Bytecode::new_raw(Bytes::from(bytes))),
            None => Ok(Bytecode::default()),
        }
    }

    fn storage_ref(
        &self,
        address: Address,
        index: revm::primitives::StorageKey,
    ) -> Result<revm::primitives::StorageValue, Self::Error> {
        self.get_storage(&address, &index)
    }

    fn block_hash_ref(&self, number: u64) -> Result<B256, Self::Error> {
        Ok(self.get_block_hash(number)?.unwrap_or(B256::ZERO))
    }
}

// ---- Encoding helpers ----

/// Encode AccountInfo as 72 bytes: balance(32 BE) + nonce(8 BE) + code_hash(32).
pub fn encode_account_info(info: &AccountInfo) -> [u8; 72] {
    let mut buf = [0u8; 72];
    buf[..32].copy_from_slice(&info.balance.to_be_bytes::<32>());
    buf[32..40].copy_from_slice(&info.nonce.to_be_bytes());
    buf[40..72].copy_from_slice(info.code_hash.as_slice());
    buf
}

/// Decode AccountInfo from 72 bytes.
pub fn decode_account_info(data: &[u8]) -> Result<AccountInfo, StateError> {
    if data.len() != 72 {
        return Err(StateError::InvalidData(format!(
            "account data len {} != 72",
            data.len()
        )));
    }
    Ok(AccountInfo {
        balance: U256::from_be_slice(&data[..32]),
        nonce: u64::from_be_bytes(data[32..40].try_into().unwrap()),
        code_hash: B256::from_slice(&data[40..72]),
        account_id: None,
        code: None,
    })
}

/// Build a 52-byte storage key: address(20) ++ slot(32 BE).
pub fn storage_key(address: &Address, index: &U256) -> [u8; 52] {
    let mut key = [0u8; 52];
    key[..20].copy_from_slice(address.as_slice());
    key[20..52].copy_from_slice(&index.to_be_bytes::<32>());
    key
}
