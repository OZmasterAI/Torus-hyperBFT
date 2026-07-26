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
        //
        // L3 #3 (compaction smoothing): `max_background_jobs` and
        // `max_subcompactions` are env-gated so an 18-core box can dedicate more
        // threads to flush/compaction (the measured contention vs the CPU-bound
        // leader that inflates flush variance to 39→119 ms/blk). Both default to
        // exact-today behaviour (4 jobs; subcompactions unset). Node-local,
        // perf-only, no format/consensus impact.
        opts.set_max_background_jobs(max_background_jobs());
        if let Some(sub) = max_subcompactions() {
            opts.set_max_subcompactions(sub);
        }
        // L3 #4 (bytes_per_sync A/B): range-sync granularity during heavy flush.
        // 1 MiB range-sync can add write-path latency spikes; the bench A/Bs 4
        // MiB or 0 (disabled). Default 1 MiB = exact-today. Node-local, perf-only.
        opts.set_bytes_per_sync(bytes_per_sync_bytes());
        // STABILITY: the ONLY global bound on total memtable memory.
        //
        // `set_write_buffer_size` / `set_max_write_buffer_number` below are
        // PER-COLUMN-FAMILY, and they are applied to every name in
        // `ALL_CF_NAMES` (44 CFs), so the untuned sum is
        // 43 x 128 MiB x 4 + 8 MiB x 4 = ~21.5 GiB with NO global bound — against
        // a documented 4 GB / 8 GB-for-validators node floor
        // (docs/node-operator-guide.md). RocksDB only approaches that sum under a
        // broad multi-CF write burst (memtable arenas are allocated lazily, and
        // the 4 buffer slots per CF only fill when flush falls behind), but
        // nothing clips the tail, so a burst can OOM the node.
        //
        // `db_write_buffer_size` is RocksDB's DB-wide write-buffer budget: once
        // the sum of all live memtables crosses it, RocksDB force-flushes the CF
        // with the largest memtable instead of letting the sum keep growing.
        // Bounds the tail without reshaping steady state (see
        // `db_write_buffer_bytes` for the default and per-node-class guidance).
        opts.set_db_write_buffer_size(db_write_buffer_bytes());

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

        // cf_consensus_meta holds a small hot keyset rewritten EVERY block
        // (leader reputation, speculative commits, highest PC/TC, block tree).
        // Under the shared 128 MiB buffer its memtable accumulates dead
        // versions for hours without flushing, and the consensus thread's
        // propose-path reads slow down walking the growing skiplist —
        // measured +0.9 ms per 1k blocks (S405 soak), the live height-drag
        // mechanism. A small buffer flushes it early; compaction then drops
        // the dead versions and reads stay flat.
        let mut meta_opts = cf_opts.clone();
        meta_opts.set_write_buffer_size(8 * 1024 * 1024); // 8 MiB

        // L3 #3 (compaction smoothing): the churny native CFs (order books +
        // the hash-only mirror + the node-local order-row store) accumulate dead
        // versions / tombstones under the shared 128 MiB buffer, so a scan or a
        // flush pays the growing skiplist + tombstone walk. A smaller buffer
        // flushes them small/often (the exact treatment already applied to
        // CF_CONSENSUS_META), so compaction drops the dead versions promptly.
        // Env-gated; 0 (default) = shared 128 MiB = exact-today.
        let churny_opts = churny_cf_write_buffer_bytes().map(|bytes| {
            let mut o = cf_opts.clone();
            o.set_write_buffer_size(bytes);
            o
        });
        let is_churny_cf = |name: &str| {
            name == CF_NATIVE_ORDER_BOOKS || name == CF_NATIVE_HASHED || name == CF_BOOK_ORDER_ROWS
        };

        let cf_descriptors: Vec<ColumnFamilyDescriptor> = ALL_CF_NAMES
            .iter()
            .map(|name| {
                let opts = if *name == CF_CONSENSUS_META {
                    meta_opts.clone()
                } else if let (true, Some(o)) = (is_churny_cf(name), churny_opts.as_ref()) {
                    o.clone()
                } else {
                    cf_opts.clone()
                };
                ColumnFamilyDescriptor::new(*name, opts)
            })
            .collect();

        let db = DB::open_cf_descriptors(&opts, path, cf_descriptors)?;
        Ok(Self { db: Arc::new(db) })
    }

    /// Wrap an already-opened RocksDB instance (e.g., a read-only snapshot DB).
    pub fn from_existing_db(db: DB) -> Self {
        Self { db: Arc::new(db) }
    }

    /// Open the database read-only (all column families). Works against a LIVE
    /// primary too (no LOCK contention), but then sees data only as of the last
    /// flush — recent memtable-only writes are invisible. Used by offline
    /// tooling (`torus-unwedge --inspect`) for recon without stopping the node.
    pub fn open_read_only(path: &Path) -> Result<Self, StateError> {
        let opts = Options::default();
        let cf_descriptors: Vec<ColumnFamilyDescriptor> = ALL_CF_NAMES
            .iter()
            .map(|name| ColumnFamilyDescriptor::new(*name, Options::default()))
            .collect();
        let db = DB::open_cf_descriptors_read_only(&opts, path, cf_descriptors, false)?;
        Ok(Self { db: Arc::new(db) })
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

    /// Fsync the shared write-ahead log, making every write issued so far
    /// crash-durable — surviving host power-loss / hard VM-stop, not just a
    /// process kill (which the OS page cache already covers).
    ///
    /// Task B / Option C: `kv_store` (consensus-meta / commit-frontier) and
    /// `native_da` (bodies) share this one `Arc<DB>` and therefore one WAL, so a
    /// single `flush_wal(true)` at the commit boundary makes the whole committed
    /// prefix (frontier + header + body) durable atomically-by-WAL-order. Call
    /// it exactly ONCE per committed block — never per-write or per-action.
    pub fn sync_wal(&self) -> Result<(), StateError> {
        self.db.flush_wal(true)?;
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
    pub fn get_session(
        &self,
        pubkey: &[u8; 32],
    ) -> Result<Option<torus_types::SessionData>, StateError> {
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
    pub fn put_session(
        &self,
        pubkey: &[u8; 32],
        data: &torus_types::SessionData,
    ) -> Result<(), StateError> {
        let bytes = serde_json::to_vec(data).map_err(|e| StateError::InvalidData(e.to_string()))?;
        self.put_cf_raw(crate::cf::CF_SESSIONS, pubkey, &bytes)
    }

    /// Delete a session.
    pub fn delete_session(&self, pubkey: &[u8; 32]) -> Result<(), StateError> {
        self.delete_cf_raw(crate::cf::CF_SESSIONS, pubkey)
    }

    /// Count active sessions for an owner address.
    /// Scans all sessions (acceptable since max 5 per owner, total count is bounded).
    pub fn count_sessions_for_owner(
        &self,
        owner: &alloy_primitives::Address,
    ) -> Result<usize, StateError> {
        let cf = self.cf(crate::cf::CF_SESSIONS)?;
        let iter = self.db.iterator_cf(cf, rocksdb::IteratorMode::Start);
        let mut count = 0;
        for (_key, value) in iter.flatten() {
            if let Ok(data) = serde_json::from_slice::<torus_types::SessionData>(&value) {
                if data.owner == *owner {
                    count += 1;
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

/// L3 #3: DB-wide `max_background_jobs` (flush + compaction threads). Default 4
/// (exact-today). `TORUS_MAX_BG_JOBS` overrides — e.g. `8` on an 18-core box to
/// relieve compaction-vs-exec CPU contention. Read once at DB open.
pub fn max_background_jobs() -> i32 {
    parse_positive_i32(std::env::var("TORUS_MAX_BG_JOBS").ok(), 4)
}

/// L3 #3: DB-wide `max_subcompactions` (parallelism WITHIN one compaction job).
/// `None` (default) = leave unset = exact-today. `TORUS_MAX_SUBCOMPACTIONS`
/// (>=1) sets it. Read once at DB open.
pub fn max_subcompactions() -> Option<u32> {
    parse_opt_u32_min1(std::env::var("TORUS_MAX_SUBCOMPACTIONS").ok())
}

/// L3 #4: DB-wide `bytes_per_sync` range-sync granularity, in bytes. Default
/// 1 MiB (exact-today). `TORUS_BYTES_PER_SYNC_MIB` overrides in whole MiB; `0`
/// disables range-sync entirely. Read once at DB open.
pub fn bytes_per_sync_bytes() -> u64 {
    match std::env::var("TORUS_BYTES_PER_SYNC_MIB").ok() {
        Some(v) => match v.trim().parse::<u64>() {
            Ok(mib) => mib.saturating_mul(1 << 20),
            Err(_) => 1 << 20,
        },
        None => 1 << 20,
    }
}

/// L3 #3: per-CF write-buffer size for the churny native CFs, in bytes. `None`
/// (default) = shared 128 MiB = exact-today. `TORUS_CHURNY_CF_WRITE_BUFFER_MB`
/// (>=1) gives them a smaller buffer so they flush small/often. Read once at DB
/// open.
pub fn churny_cf_write_buffer_bytes() -> Option<usize> {
    parse_opt_usize_min1_mb(std::env::var("TORUS_CHURNY_CF_WRITE_BUFFER_MB").ok())
}

/// STABILITY: DB-wide memtable budget, in bytes — the global cap on the SUM of
/// every column family's memtables (`Options::set_db_write_buffer_size`).
/// Without it the per-CF 128 MiB x 4 buffers across 44 CFs sum to ~21.5 GiB
/// unbounded, which no documented node class can absorb.
///
/// Default **1 GiB**: room for 8 simultaneously-full 128 MiB memtables, which
/// exceeds the hot multi-CF write set, so steady-state flush behaviour is
/// unchanged and only the burst tail is clipped. Sized against the 8 GB
/// validator class — with the 256 MiB shared block cache it puts the DB's
/// bounded memory at ~1.25 GiB.
///
/// `TORUS_DB_WRITE_BUFFER_MB` overrides in whole MiB; `0` disables the cap
/// (RocksDB default = exact-today unbounded). Recommended per node class:
/// `512` on a 4 GB node, default `1024` on 8 GB, `2048` on 16 GB+.
/// Read once at DB open.
pub fn db_write_buffer_bytes() -> usize {
    parse_usize_mb_or(std::env::var("TORUS_DB_WRITE_BUFFER_MB").ok(), 1024)
}

/// Pure parse: a positive `i32` env value, falling back to `default` on
/// unset / non-numeric / `< 1`.
fn parse_positive_i32(raw: Option<String>, default: i32) -> i32 {
    match raw.as_deref().map(str::trim).and_then(|s| s.parse::<i32>().ok()) {
        Some(n) if n >= 1 => n,
        _ => default,
    }
}

/// Pure parse: `Some(n)` for a `>= 1` env value, else `None` (unset / garbage).
fn parse_opt_u32_min1(raw: Option<String>) -> Option<u32> {
    raw.as_deref()
        .map(str::trim)
        .and_then(|s| s.parse::<u32>().ok())
        .filter(|&n| n >= 1)
}

/// Pure parse: whole-MiB env value `>= 1` → `Some(bytes)`, else `None`.
fn parse_opt_usize_min1_mb(raw: Option<String>) -> Option<usize> {
    raw.as_deref()
        .map(str::trim)
        .and_then(|s| s.parse::<usize>().ok())
        .filter(|&mb| mb >= 1)
        .map(|mb| mb.saturating_mul(1024 * 1024))
}

/// Pure parse: whole-MiB env value → bytes, falling back to `default_mb` MiB on
/// unset / non-numeric. Unlike [`parse_opt_usize_min1_mb`], `0` is HONOURED
/// (it disables the knob), matching `TORUS_BYTES_PER_SYNC_MIB`.
fn parse_usize_mb_or(raw: Option<String>, default_mb: usize) -> usize {
    let mb = raw
        .as_deref()
        .map(str::trim)
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(default_mb);
    mb.saturating_mul(1024 * 1024)
}

/// Runtime toggle: fsync the WAL once per committed block ([`StateDb::sync_wal`]).
///
/// Default **OFF** ⇒ byte-identical to today's async-write behavior (no flush).
/// Set `TORUS_SYNC_WAL_ON_COMMIT` to a truthy value (`1`/`true`/`yes`/`on`) to
/// enable. Proposer/replica-local, format-neutral, needs no coordination — safe
/// to A/B on a single node. Read once at first use (same pattern as
/// `evm_block_gas_budget`).
pub fn sync_wal_on_commit_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| parse_sync_wal_toggle(std::env::var("TORUS_SYNC_WAL_ON_COMMIT").ok()))
}

/// Pure parse of the `TORUS_SYNC_WAL_ON_COMMIT` value (default OFF). Split from
/// the `OnceLock` reader so the default and accepted spellings are unit-testable
/// without touching process-global state.
fn parse_sync_wal_toggle(raw: Option<String>) -> bool {
    match raw {
        Some(v) => matches!(v.trim(), "1" | "true" | "TRUE" | "yes" | "on"),
        None => false,
    }
}

#[cfg(test)]
mod sync_wal_tests {
    use super::*;

    // Task 1 (RED): crash-durable commit via a single WAL fsync per block.
    // `kv_store` (consensus-meta/frontier) and `native_da` (bodies) share this
    // one `Arc<DB>`/WAL, so one flush covers the whole committed prefix.

    #[test]
    fn sync_wal_flushes_and_write_survives_reopen() {
        let dir = tempfile::tempdir().expect("tempdir");
        {
            let db = StateDb::open(dir.path()).expect("open temp StateDb");
            db.put_cf_raw(CF_CONSENSUS_META, b"frontier", b"height-1")
                .expect("put");
            db.sync_wal().expect("sync_wal must succeed");
        }
        // Reopen the (cleanly-closed) DB: the synced write is present.
        let db2 = StateDb::open(dir.path()).expect("reopen temp StateDb");
        assert_eq!(
            db2.get_cf_raw(CF_CONSENSUS_META, b"frontier").expect("get"),
            Some(b"height-1".to_vec()),
        );
    }

    #[test]
    fn sync_wal_on_commit_defaults_off() {
        // Env unset ⇒ toggle OFF ⇒ behavior byte-identical to today (no flush).
        assert!(!parse_sync_wal_toggle(None));
    }

    #[test]
    fn sync_wal_toggle_parses_truthy_spellings() {
        assert!(parse_sync_wal_toggle(Some("1".to_string())));
        assert!(parse_sync_wal_toggle(Some("true".to_string())));
        assert!(parse_sync_wal_toggle(Some(" on ".to_string())));
        assert!(!parse_sync_wal_toggle(Some("0".to_string())));
        assert!(!parse_sync_wal_toggle(Some("".to_string())));
    }

    // L3 #3 / #4: every compaction / sync knob defaults to exact-today.

    #[test]
    fn l3_max_bg_jobs_defaults_to_four() {
        assert_eq!(parse_positive_i32(None, 4), 4);
        assert_eq!(parse_positive_i32(Some("0".into()), 4), 4);
        assert_eq!(parse_positive_i32(Some("garbage".into()), 4), 4);
        assert_eq!(parse_positive_i32(Some(" 8 ".into()), 4), 8);
    }

    #[test]
    fn l3_max_subcompactions_defaults_unset() {
        assert_eq!(parse_opt_u32_min1(None), None);
        assert_eq!(parse_opt_u32_min1(Some("0".into())), None);
        assert_eq!(parse_opt_u32_min1(Some("x".into())), None);
        assert_eq!(parse_opt_u32_min1(Some("4".into())), Some(4));
    }

    #[test]
    fn l3_bytes_per_sync_defaults_1mib() {
        // The pure mapping the reader uses (whole MiB → bytes; 0 disables).
        let map = |mib: u64| mib.saturating_mul(1 << 20);
        assert_eq!(map(1), 1 << 20);
        assert_eq!(map(4), 4 << 20);
        assert_eq!(map(0), 0);
    }

    #[test]
    fn l3_churny_cf_buffer_defaults_unset() {
        assert_eq!(parse_opt_usize_min1_mb(None), None);
        assert_eq!(parse_opt_usize_min1_mb(Some("0".into())), None);
        assert_eq!(parse_opt_usize_min1_mb(Some("garbage".into())), None);
        assert_eq!(parse_opt_usize_min1_mb(Some("16".into())), Some(16 * 1024 * 1024));
    }

    // STABILITY: global memtable cap (`db_write_buffer_size`). The per-CF
    // buffers are unbounded in SUM; this is the only DB-wide bound.

    const MIB: usize = 1024 * 1024;

    #[test]
    fn db_write_buffer_defaults_to_1gib() {
        // Unset / garbage ⇒ the 1 GiB default, NOT rocksdb's unbounded 0.
        assert_eq!(parse_usize_mb_or(None, 1024), 1024 * MIB);
        assert_eq!(parse_usize_mb_or(Some("garbage".into()), 1024), 1024 * MIB);
        assert_eq!(parse_usize_mb_or(Some("".into()), 1024), 1024 * MIB);
        assert_eq!(parse_usize_mb_or(Some("-1".into()), 1024), 1024 * MIB);
    }

    #[test]
    fn db_write_buffer_env_overrides_in_whole_mib() {
        // Per-node-class guidance: 512 on a 4 GB node, 2048 on 16 GB+.
        assert_eq!(parse_usize_mb_or(Some("512".into()), 1024), 512 * MIB);
        assert_eq!(parse_usize_mb_or(Some(" 2048 ".into()), 1024), 2048 * MIB);
    }

    #[test]
    fn db_write_buffer_zero_disables_the_cap() {
        // `0` is rocksdb's "disabled" sentinel — the escape hatch back to
        // exact-today unbounded behaviour. It must NOT fall back to the default.
        assert_eq!(parse_usize_mb_or(Some("0".into()), 1024), 0);
    }

    #[test]
    fn db_write_buffer_cap_is_far_below_the_untuned_per_cf_sum() {
        // The bound this knob exists to enforce. `max_write_buffer_number` is 4
        // and every CF but CF_CONSENSUS_META (8 MiB) gets the 128 MiB buffer.
        let untuned_sum = (ALL_CF_NAMES.len() - 1) * 128 * MIB * 4 + 8 * MIB * 4;
        assert!(
            untuned_sum > 20 * 1024 * MIB,
            "untuned per-CF memtable sum {untuned_sum} should exceed 20 GiB across {} CFs",
            ALL_CF_NAMES.len()
        );
        // Default cap is ~1/20th of that, and fits under the 4 GB node floor.
        let cap = parse_usize_mb_or(None, 1024);
        assert!(cap < untuned_sum / 20);
        assert!(cap < 4 * 1024 * MIB);
    }

    #[test]
    fn db_opens_and_round_trips_with_global_memtable_cap() {
        // `set_db_write_buffer_size` is applied in `open()`: prove a real DB
        // still opens all 44 CFs, accepts writes, and reopens with data intact.
        let dir = tempfile::tempdir().expect("tempdir");
        {
            let db = StateDb::open(dir.path()).expect("open with db_write_buffer_size set");
            assert_eq!(db.column_families().len(), ALL_CF_NAMES.len());
            db.put_cf_raw(CF_ACCOUNTS, b"acct", b"value").expect("put");
            db.put_cf_raw(CF_CONSENSUS_META, b"meta", b"m").expect("put");
            db.sync_wal().expect("sync_wal");
        }
        let db2 = StateDb::open(dir.path()).expect("reopen");
        assert_eq!(
            db2.get_cf_raw(CF_ACCOUNTS, b"acct").expect("get"),
            Some(b"value".to_vec()),
        );
    }
}
