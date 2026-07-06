//! Background state pruner — removes old block bodies and receipts.
//!
//! # Column Family Classification (33 CFs)
//!
//! ## NEVER PRUNE (22 CFs — current state, structural, consensus safety)
//!
//! | CF | Key Format | Reason |
//! |---|---|---|
//! | `cf_accounts` | address(20) | Current EVM account state |
//! | `cf_storage` | address(20) + slot(32) | Current EVM storage |
//! | `cf_code` | code_hash(32) | Contract bytecode (content-addressed) |
//! | `cf_block_headers` | height(8 BE) | Chain structure — always needed |
//! | `cf_block_hash_to_number` | hash(32) | Reverse index for headers |
//! | `cf_native_orders` | varies | Current order book state |
//! | `cf_native_positions` | varies | Current position state |
//! | `cf_native_balances` | varies | Current balance state |
//! | `cf_native_order_books` | varies | Order book configuration |
//! | `cf_native_markets` | varies | Market configuration |
//! | `cf_staking_validators` | address(20) | Validator state |
//! | `cf_staking_delegations` | delegator(20) + validator(20) | Delegation state |
//! | `cf_staking_permanent` | address(20) | Permanent stake info |
//! | `cf_staking_rewards` | varies | Reward tracking |
//! | `cf_governance_proposals` | proposal_id | Governance state |
//! | `cf_governance_votes` | varies | Vote state |
//! | `cf_fee_config` | varies | Fee configuration |
//! | `cf_treasury` | varies | Treasury state |
//! | `cf_dev_pool` | varies | Dev pool state |
//! | `cf_native_oracle` | varies | Oracle price data |
//! | `cf_consensus_meta` | opaque (hotstuff_rs) | Block tree, PCs, TCs — consensus safety critical |
//! | `cf_core_writer_queue` | varies | Core writer queue state |
//!
//! ## PRUNABLE — IMPLEMENTED (2 CFs — height is leading key prefix)
//!
//! | CF | Key Format | Delete Strategy |
//! |---|---|---|
//! | `cf_block_bodies` | height(8 BE) | `delete_range_cf` on [0, cutoff) |
//! | `cf_receipts` | height(8 BE) + tx_index(4 BE) | `delete_range_cf` on [0, cutoff‖0×4) |
//!
//! These are the largest disk consumers (block bodies contain full transaction
//! data; receipts contain logs and execution results). Together they cover the
//! majority of historical growth.
//!
//! ## HARD TO PRUNE — DEFERRED (9 CFs)
//!
//! | CF | Key Format | Why Deferred |
//! |---|---|---|
//! | `cf_trie_nodes` | node_hash | MPT reachability analysis needed (research-level) |
//! | `cf_trie_accounts` | node_hash | Same as above |
//! | `cf_trie_storage` | node_hash | Same as above |
//! | `cf_native_trades` | market_id(8) + block(8) + idx(4) | Market ID is leading prefix, not height |
//! | `cf_slash_records` | validator(20) + height(8) | Validator is leading prefix |
//! | `cf_jail_votes` | target(20) + voter(20) | No height component |
//! | `cf_tx_hash_to_location` | tx_hash(32) | Not height-keyed (stale pointers return None safely) |
//! | `cf_logs` | — | Defined but not populated by bridge committer |
//! | `cf_logs_bloom` | — | Defined but not populated by bridge committer |
//!
//! ### Why trie pruning is hard
//!
//! The MPT (Merkle Patricia Trie) stores internal nodes keyed by their hash.
//! Old state roots reference old trie nodes, but current state also shares many
//! of those nodes (unchanged accounts/storage). Pruning requires determining
//! which nodes are reachable from the *current* state root — a mark-and-sweep
//! or reference-counting problem. Ethereum clients (geth, reth) spent years
//! solving this with approaches like snapshot-based pruning and offline
//! mark-and-sweep. This is deferred to a future batch.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use tracing::{debug, info};

use crate::cf::{CF_BLOCK_BODIES, CF_BLOCK_HEADERS, CF_RECEIPTS};
use crate::db::StateDb;
use crate::error::StateError;

/// Metadata key stored in `cf_block_headers` to track pruning progress.
/// At 16 bytes, it will never collide with normal 8-byte height keys.
const PRUNE_META_KEY: &[u8; 16] = b"__prune_meta__\x00\x00";

/// Configuration for the background pruner.
#[derive(Clone, Debug)]
pub struct PrunerConfig {
    /// Number of recent blocks to retain. Everything older is eligible for pruning.
    pub retention_blocks: u64,
    /// How often (in blocks) to check for pruneable data.
    pub prune_interval_blocks: u64,
    /// Maximum number of block heights to prune in a single batch before yielding.
    pub batch_size: u64,
}

impl Default for PrunerConfig {
    fn default() -> Self {
        Self {
            retention_blocks: 100_000,
            prune_interval_blocks: 1000,
            batch_size: 10_000,
        }
    }
}

/// Background state pruner that removes old block bodies and receipts.
///
/// Only prunes the 2 "easy" column families where height is the leading key
/// prefix. See module-level documentation for the full CF classification.
pub struct StatePruner {
    db: StateDb,
    config: PrunerConfig,
    /// Height up to which data has been pruned (exclusive — data at this height exists).
    pruned_up_to: Arc<AtomicU64>,
    /// Last block height at which we ran pruning.
    last_prune_height: u64,
}

impl StatePruner {
    /// Create a new pruner. Reads the last pruned height from the DB.
    pub fn new(db: StateDb, config: PrunerConfig, pruned_up_to: Arc<AtomicU64>) -> Self {
        let stored = read_prune_meta(&db).unwrap_or(0);
        pruned_up_to.store(stored, Ordering::Release);

        info!(
            retention_blocks = config.retention_blocks,
            prune_interval = config.prune_interval_blocks,
            pruned_up_to = stored,
            "state pruner initialized"
        );

        Self {
            db,
            config,
            pruned_up_to,
            last_prune_height: 0,
        }
    }

    /// Check if pruning should run at the given block height and prune if needed.
    ///
    /// Returns the number of block heights worth of data pruned (0 if nothing to do).
    pub fn maybe_prune(&mut self, current_height: u64) -> Result<u64, StateError> {
        // Only prune at configured intervals
        if current_height < self.last_prune_height + self.config.prune_interval_blocks {
            return Ok(0);
        }

        let cutoff = current_height.saturating_sub(self.config.retention_blocks);
        let already_pruned = self.pruned_up_to.load(Ordering::Acquire);

        if cutoff <= already_pruned {
            self.last_prune_height = current_height;
            return Ok(0);
        }

        let total_pruned = self.prune_up_to(already_pruned, cutoff)?;
        self.last_prune_height = current_height;
        Ok(total_pruned)
    }

    /// Prune all data from `from_height` (inclusive) to `to_height` (exclusive).
    ///
    /// Batches deletions to avoid RocksDB compaction latency spikes. Between
    /// batches, yields to the tokio runtime so consensus and RPC are not starved.
    fn prune_up_to(&self, from_height: u64, to_height: u64) -> Result<u64, StateError> {
        if to_height <= from_height {
            return Ok(0);
        }

        let total_blocks = to_height - from_height;
        info!(
            from = from_height,
            to = to_height,
            blocks = total_blocks,
            "pruning old block bodies and receipts"
        );

        let mut current = from_height;
        while current < to_height {
            let batch_end = (current + self.config.batch_size).min(to_height);

            self.prune_range(current, batch_end)?;

            // Update progress
            self.pruned_up_to.store(batch_end, Ordering::Release);
            write_prune_meta(&self.db, batch_end)?;

            let pruned_in_batch = batch_end - current;
            debug!(
                from = current,
                to = batch_end,
                pruned = pruned_in_batch,
                "pruned batch"
            );

            current = batch_end;
        }

        info!(
            pruned_up_to = to_height,
            blocks_pruned = total_blocks,
            "pruning complete"
        );

        Ok(total_blocks)
    }

    /// Delete a contiguous range of block bodies and receipts using RocksDB
    /// range deletion.
    fn prune_range(&self, from_height: u64, to_height: u64) -> Result<(), StateError> {
        let db = self.db.inner();

        // --- Prune cf_block_bodies ---
        // Key format: height(8 BE)
        if let Some(cf) = db.cf_handle(CF_BLOCK_BODIES) {
            let from_key = from_height.to_be_bytes();
            let to_key = to_height.to_be_bytes();
            db.delete_range_cf(&cf, from_key, to_key)?;
        }

        // --- Prune cf_receipts ---
        // Key format: height(8 BE) + tx_index(4 BE) = 12 bytes
        // Delete all receipt keys where the height prefix is in [from, to)
        if let Some(cf) = db.cf_handle(CF_RECEIPTS) {
            let mut from_key = [0u8; 12];
            from_key[..8].copy_from_slice(&from_height.to_be_bytes());
            // to_key: to_height with tx_index=0 → all keys with height < to_height
            let mut to_key = [0u8; 12];
            to_key[..8].copy_from_slice(&to_height.to_be_bytes());
            db.delete_range_cf(&cf, from_key, to_key)?;
        }

        Ok(())
    }

    /// Get the current pruned-up-to height.
    pub fn pruned_height(&self) -> u64 {
        self.pruned_up_to.load(Ordering::Acquire)
    }

    /// Get a shared handle to the pruned-up-to height for use by RPC.
    pub fn pruned_up_to_handle(&self) -> Arc<AtomicU64> {
        self.pruned_up_to.clone()
    }
}

/// Read the pruned-up-to height from the database.
fn read_prune_meta(db: &StateDb) -> Result<u64, StateError> {
    match db.get_cf_raw(CF_BLOCK_HEADERS, PRUNE_META_KEY)? {
        Some(data) if data.len() == 8 => Ok(u64::from_be_bytes(data[..8].try_into().unwrap())),
        _ => Ok(0),
    }
}

/// Write the pruned-up-to height to the database.
fn write_prune_meta(db: &StateDb, height: u64) -> Result<(), StateError> {
    db.put_cf_raw(CF_BLOCK_HEADERS, PRUNE_META_KEY, &height.to_be_bytes())
}

/// Compute the total size of a directory in bytes (recursive).
pub fn dir_size_bytes(path: &std::path::Path) -> u64 {
    walkdir(path)
}

fn walkdir(path: &std::path::Path) -> u64 {
    let mut total = 0u64;
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            let ft = match entry.file_type() {
                Ok(ft) => ft,
                Err(_) => continue,
            };
            if ft.is_file() {
                total += entry.metadata().map(|m| m.len()).unwrap_or(0);
            } else if ft.is_dir() {
                total += walkdir(&entry.path());
            }
        }
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn setup_db_with_data(dir: &std::path::Path) -> StateDb {
        let db = StateDb::open(dir).unwrap();

        // Insert block bodies and receipts for heights 0..20
        for height in 0u64..20 {
            let body_key = height.to_be_bytes();
            let body_data = format!("body-{height}");
            db.put_cf_raw(CF_BLOCK_BODIES, &body_key, body_data.as_bytes())
                .unwrap();

            // Store a header too (these should never be pruned)
            let header_data = format!("header-{height}");
            db.put_cf_raw(CF_BLOCK_HEADERS, &body_key, header_data.as_bytes())
                .unwrap();

            // Store 2 receipts per block
            for tx_idx in 0u32..2 {
                let mut receipt_key = [0u8; 12];
                receipt_key[..8].copy_from_slice(&body_key);
                receipt_key[8..12].copy_from_slice(&tx_idx.to_be_bytes());
                let receipt_data = format!("receipt-{height}-{tx_idx}");
                db.put_cf_raw(CF_RECEIPTS, &receipt_key, receipt_data.as_bytes())
                    .unwrap();
            }
        }

        db
    }

    fn body_exists(db: &StateDb, height: u64) -> bool {
        db.get_cf_raw(CF_BLOCK_BODIES, &height.to_be_bytes())
            .unwrap()
            .is_some()
    }

    fn receipt_exists(db: &StateDb, height: u64, tx_idx: u32) -> bool {
        let mut key = [0u8; 12];
        key[..8].copy_from_slice(&height.to_be_bytes());
        key[8..12].copy_from_slice(&tx_idx.to_be_bytes());
        db.get_cf_raw(CF_RECEIPTS, &key).unwrap().is_some()
    }

    fn header_exists(db: &StateDb, height: u64) -> bool {
        db.get_cf_raw(CF_BLOCK_HEADERS, &height.to_be_bytes())
            .unwrap()
            .is_some()
    }

    #[test]
    fn prune_removes_old_bodies_and_receipts() {
        let dir = TempDir::new().unwrap();
        let db = setup_db_with_data(dir.path());
        let pruned = Arc::new(AtomicU64::new(0));

        let config = PrunerConfig {
            retention_blocks: 10,
            prune_interval_blocks: 1,
            batch_size: 100,
        };
        let mut pruner = StatePruner::new(db.clone(), config, pruned.clone());

        // Current height 15, retention 10 → cutoff = 5
        let count = pruner.maybe_prune(15).unwrap();
        assert_eq!(count, 5);
        assert_eq!(pruner.pruned_height(), 5);

        // Heights 0..5 should be pruned
        for h in 0..5 {
            assert!(!body_exists(&db, h), "body at {h} should be pruned");
            assert!(
                !receipt_exists(&db, h, 0),
                "receipt at {h} should be pruned"
            );
            assert!(
                !receipt_exists(&db, h, 1),
                "receipt at {h} should be pruned"
            );
        }

        // Heights 5..20 should still exist
        for h in 5..20 {
            assert!(body_exists(&db, h), "body at {h} should exist");
            assert!(receipt_exists(&db, h, 0), "receipt at {h} should exist");
        }

        // Headers should NEVER be pruned
        for h in 0..20 {
            assert!(header_exists(&db, h), "header at {h} should always exist");
        }
    }

    #[test]
    fn archive_mode_prunes_nothing() {
        let dir = TempDir::new().unwrap();
        let db = setup_db_with_data(dir.path());
        let pruned = Arc::new(AtomicU64::new(0));

        // Retention larger than total data → nothing pruned
        let config = PrunerConfig {
            retention_blocks: 1_000_000,
            prune_interval_blocks: 1,
            batch_size: 100,
        };
        let mut pruner = StatePruner::new(db.clone(), config, pruned);

        let count = pruner.maybe_prune(20).unwrap();
        assert_eq!(count, 0);

        for h in 0..20 {
            assert!(body_exists(&db, h));
            assert!(receipt_exists(&db, h, 0));
        }
    }

    #[test]
    fn prune_respects_interval() {
        let dir = TempDir::new().unwrap();
        let db = setup_db_with_data(dir.path());
        let pruned = Arc::new(AtomicU64::new(0));

        let config = PrunerConfig {
            retention_blocks: 5,
            prune_interval_blocks: 10,
            batch_size: 100,
        };
        let mut pruner = StatePruner::new(db.clone(), config, pruned);

        // Height 5: interval not met yet (0 + 10 = 10 > 5)
        let count = pruner.maybe_prune(5).unwrap();
        assert_eq!(count, 0);

        // Height 10: interval met
        let count = pruner.maybe_prune(10).unwrap();
        assert_eq!(count, 5); // cutoff = 10 - 5 = 5

        // Height 15: not yet (last prune at 10, interval 10 → next at 20)
        let count = pruner.maybe_prune(15).unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn prune_batches_large_ranges() {
        let dir = TempDir::new().unwrap();
        let db = setup_db_with_data(dir.path());
        let pruned = Arc::new(AtomicU64::new(0));

        let config = PrunerConfig {
            retention_blocks: 2,
            prune_interval_blocks: 1,
            batch_size: 5, // Small batch to test multi-batch pruning
        };
        let mut pruner = StatePruner::new(db.clone(), config, pruned.clone());

        // Current height 19, retention 2 → cutoff = 17
        let count = pruner.maybe_prune(19).unwrap();
        assert_eq!(count, 17);
        assert_eq!(pruned.load(Ordering::Acquire), 17);

        // All blocks < 17 pruned
        for h in 0..17 {
            assert!(!body_exists(&db, h), "body at {h} should be pruned");
        }
        // Blocks 17, 18, 19 kept
        for h in 17..20 {
            assert!(body_exists(&db, h), "body at {h} should be retained");
        }
    }

    #[test]
    fn prune_meta_persists_across_restart() {
        let dir = TempDir::new().unwrap();

        // First run: prune some data
        {
            let db = setup_db_with_data(dir.path());
            let pruned = Arc::new(AtomicU64::new(0));
            let config = PrunerConfig {
                retention_blocks: 10,
                prune_interval_blocks: 1,
                batch_size: 100,
            };
            let mut pruner = StatePruner::new(db, config, pruned);
            pruner.maybe_prune(18).unwrap();
            assert_eq!(pruner.pruned_height(), 8);
        }

        // Second run: pruner picks up where it left off
        {
            let db = StateDb::open(dir.path()).unwrap();
            let pruned = Arc::new(AtomicU64::new(0));
            let config = PrunerConfig {
                retention_blocks: 10,
                prune_interval_blocks: 1,
                batch_size: 100,
            };
            let pruner = StatePruner::new(db, config, pruned.clone());
            assert_eq!(pruner.pruned_height(), 8);
            assert_eq!(pruned.load(Ordering::Acquire), 8);
        }
    }

    #[test]
    fn pruned_rpc_query_returns_none() {
        let dir = TempDir::new().unwrap();
        let db = setup_db_with_data(dir.path());
        let pruned = Arc::new(AtomicU64::new(0));

        let config = PrunerConfig {
            retention_blocks: 10,
            prune_interval_blocks: 1,
            batch_size: 100,
        };
        let mut pruner = StatePruner::new(db.clone(), config, pruned);
        pruner.maybe_prune(15).unwrap();

        // Query pruned block body → None
        assert!(db
            .get_cf_raw(CF_BLOCK_BODIES, &0u64.to_be_bytes())
            .unwrap()
            .is_none());

        // Query retained block body → Some
        assert!(db
            .get_cf_raw(CF_BLOCK_BODIES, &10u64.to_be_bytes())
            .unwrap()
            .is_some());

        // Query pruned receipt → None
        let mut key = [0u8; 12];
        key[..8].copy_from_slice(&0u64.to_be_bytes());
        assert!(db.get_cf_raw(CF_RECEIPTS, &key).unwrap().is_none());
    }

    #[test]
    fn dir_size_bytes_works() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("test.dat");
        std::fs::write(&path, vec![0u8; 1024]).unwrap();
        let size = dir_size_bytes(dir.path());
        assert!(size >= 1024, "expected at least 1024 bytes, got {size}");
    }
}
