//! Background state pruner — removes old block bodies and receipts, and
//! (opt-in, D13 `--trade-retention-blocks`) old trade-history rows.
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
//! ## PRUNABLE — IMPLEMENTED (3 CFs — height is leading key prefix)
//!
//! | CF | Key Format | Delete Strategy |
//! |---|---|---|
//! | `cf_block_bodies` | height(8 BE) | `delete_range_cf` on [0, cutoff) |
//! | `cf_block_action_status` | height(8 BE) | `delete_range_cf` on [0, cutoff) |
//! | `cf_receipts` | height(8 BE) + tx_index(4 BE) | `delete_range_cf` on [0, cutoff‖0×4) |
//!
//! These are the largest disk consumers (block bodies contain full transaction
//! data; receipts contain logs and execution results). Together they cover the
//! majority of historical growth.
//!
//! ## PRUNABLE — OPT-IN, D13 (2 CFs — block is not the leading key part)
//!
//! | CF | Key Format | Delete Strategy |
//! |---|---|---|
//! | `cf_native_trades` | market(8) + block(8) + chunk(2) | skip-scan markets, `delete_range_cf` on [market‖0‖0, market‖cutoff‖0) |
//! | `cf_native_user_trades` | address(20) + !block(8) | skip-scan addresses, point deletes of rows below the cutoff |
//!
//! Only with `PrunerConfig::trade_retention_blocks` set; see
//! [`prune_trades_below`].
//!
//! ## HARD TO PRUNE — DEFERRED (8 CFs)
//!
//! | CF | Key Format | Why Deferred |
//! |---|---|---|
//! | `cf_trie_nodes` | node_hash | MPT reachability analysis needed (research-level) |
//! | `cf_trie_accounts` | node_hash | Same as above |
//! | `cf_trie_storage` | node_hash | Same as above |
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

use tracing::{debug, info, warn};

use crate::cf::{
    CF_BLOCK_ACTION_STATUS, CF_BLOCK_BODIES, CF_BLOCK_HEADERS, CF_CONSENSUS_META, CF_RECEIPTS,
    META_TRADES_PRUNED_HEIGHT,
};
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
    /// D13: number of recent blocks of trade history (`cf_native_trades`,
    /// `cf_native_user_trades`) to keep. `None` = keep every trade row (the
    /// default, exactly today). Independent of `retention_blocks`; both use the
    /// same interval, batch size and applied-marker clamp.
    pub trade_retention_blocks: Option<u64>,
}

impl Default for PrunerConfig {
    fn default() -> Self {
        Self {
            retention_blocks: 100_000,
            prune_interval_blocks: 1000,
            batch_size: 10_000,
            trade_retention_blocks: None,
        }
    }
}

/// Background state pruner that removes old block bodies and receipts, and
/// (opt-in, D13) old trade-history rows.
///
/// See module-level documentation for the full CF classification.
pub struct StatePruner {
    db: StateDb,
    config: PrunerConfig,
    /// Height up to which data has been pruned (exclusive — data at this height exists).
    pruned_up_to: Arc<AtomicU64>,
    /// D13: trade rows of blocks below this height are deleted or being
    /// deleted (0 = none). Raised (and persisted, `META_TRADES_PRUNED_HEIGHT`)
    /// before a pass deletes, so RPC readers sharing it never serve a
    /// half-pruned range; loaded from disk at start and never lowered.
    trades_pruned_up_to: Arc<AtomicU64>,
    /// Cutoff of the last trade pass that completed (0 at start, so the first
    /// pass after a restart re-deletes below the stored cutoff).
    trades_done: u64,
    /// Cutoff last written to `META_TRADES_PRUNED_HEIGHT` (`None` = the
    /// startup read failed: re-read before a pass writes, never lowered).
    trades_persisted: Option<u64>,
    /// Last block height at which we ran pruning.
    last_prune_height: u64,
}

impl StatePruner {
    /// Create a new pruner. Reads the last pruned height from the DB.
    pub fn new(db: StateDb, config: PrunerConfig, pruned_up_to: Arc<AtomicU64>) -> Self {
        let stored = read_prune_meta(&db).unwrap_or(0);
        pruned_up_to.store(stored, Ordering::Release);
        let trades_read = read_trades_pruned_height(&db)
            .map_err(|e| warn!(%e, "trade-history cutoff unreadable; re-read on the first pass"))
            .ok();
        let trades_stored = trades_read.unwrap_or(0);

        info!(
            retention_blocks = config.retention_blocks,
            prune_interval = config.prune_interval_blocks,
            pruned_up_to = stored,
            trade_retention_blocks = ?config.trade_retention_blocks,
            trades_pruned_up_to = trades_stored,
            "state pruner initialized"
        );

        Self {
            db,
            config,
            pruned_up_to,
            trades_pruned_up_to: Arc::new(AtomicU64::new(trades_stored)),
            trades_done: 0,
            trades_persisted: trades_read,
            last_prune_height: 0,
        }
    }

    /// Share the trade-history cutoff with the RPC readers
    /// (`RpcServer::trades_pruned_up_to`).
    pub fn with_trades_pruned_handle(mut self, handle: Arc<AtomicU64>) -> Self {
        handle.fetch_max(
            self.trades_pruned_up_to.load(Ordering::Acquire),
            Ordering::AcqRel,
        );
        self.trades_pruned_up_to = handle;
        self
    }

    /// Trade rows below this height are pruned (0 = none).
    pub fn trades_pruned_height(&self) -> u64 {
        self.trades_pruned_up_to.load(Ordering::Acquire)
    }

    /// Check if pruning should run at the given block height and prune if needed.
    ///
    /// Returns the number of block heights worth of data pruned (0 if nothing to do).
    pub fn maybe_prune(&mut self, current_height: u64) -> Result<u64, StateError> {
        // Only prune at configured intervals
        if current_height < self.last_prune_height + self.config.prune_interval_blocks {
            return Ok(0);
        }

        // Crash replay re-executes `(applied, committed]` from the stored
        // bodies, so never prune at or above the durable applied marker
        // (absent = nothing applied = prune nothing).
        let applied = crate::running_hash::read_applied_height(&self.db).unwrap_or(0);
        let cutoff = current_height
            .saturating_sub(self.config.retention_blocks)
            .min(applied);
        let already_pruned = self.pruned_up_to.load(Ordering::Acquire);

        let total_pruned = if cutoff > already_pruned {
            self.prune_up_to(already_pruned, cutoff)?
        } else {
            0
        };

        // D13 trade history, same clamp: replay rewrites the trade rows of
        // `(applied, committed]`, so pruning them first would leak the rewrite.
        if let Some(keep) = self.config.trade_retention_blocks {
            let trade_cutoff = current_height.saturating_sub(keep).min(applied);
            if trade_cutoff > self.trades_done {
                let persisted = match self.trades_persisted {
                    Some(h) => h,
                    None => {
                        let h = read_trades_pruned_height(&self.db)?;
                        self.trades_pruned_up_to.fetch_max(h, Ordering::AcqRel);
                        self.trades_persisted = Some(h);
                        h
                    }
                };
                // Never below the stored cutoff (N raised across a restart):
                // those rows are gone, RPC keeps them pruned.
                let cutoff = self
                    .trades_pruned_up_to
                    .fetch_max(trade_cutoff, Ordering::AcqRel)
                    .max(trade_cutoff);
                // Persist before deleting: a crash in between leaves rows
                // hidden, never deleted rows served as "no trades".
                if cutoff > persisted {
                    write_trades_pruned_height(&self.db, cutoff)?;
                    self.trades_persisted = Some(cutoff);
                }
                let stats = prune_trades_below(&self.db, cutoff, self.config.batch_size)?;
                self.trades_done = cutoff;
                info!(
                    cutoff,
                    markets = stats.market_ranges,
                    user_rows = stats.user_rows,
                    "pruned trade history"
                );
            }
        }

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

        // --- Prune cf_block_action_status (s84 executed/skipped record) ---
        // Key format: height(8 BE), same range as the bodies it describes.
        if let Some(cf) = db.cf_handle(CF_BLOCK_ACTION_STATUS) {
            db.delete_range_cf(&cf, from_height.to_be_bytes(), to_height.to_be_bytes())?;
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

/// What one trade-history pass deleted.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct TradePruneStats {
    /// Markets that had rows below the cutoff (one range delete each).
    pub market_ranges: u64,
    /// `cf_native_user_trades` rows deleted (point deletes).
    pub user_rows: u64,
}

/// Delete every trade-history row of a block below `cutoff` (D13), in both
/// CFs. Node-local history, outside the consensus root and the state hash.
///
/// The block is not the leading key part, so both CFs are walked by
/// skip-scan, one prefix at a time:
/// - `cf_native_trades` (`market u64 | block u64 | chunk u16`): per market
///   whose first row is below the cutoff, one range delete
///   `[market|0|0, market|cutoff|0)`. Markets are few (hundreds).
/// - `cf_native_user_trades` (`address[20] | !block u64`, newest first): per
///   address, seek to its first row below the cutoff (`!(cutoff - 1)`) and
///   point-delete through the end of the address. Point deletes, not one range
///   tombstone per address: addresses can be many, each with few old rows.
///
/// Every pass deletes everything below `cutoff`, not only what is new since
/// the last pass, so rows the background trade writer lands late and rows
/// left by a pass that failed or ran before a restart are caught by the next.
/// Deletes go out in batches of at most `batch_size` operations.
pub fn prune_trades_below(
    db: &StateDb,
    cutoff: u64,
    batch_size: u64,
) -> Result<TradePruneStats, StateError> {
    use crate::cf::{CF_NATIVE_TRADES, CF_NATIVE_USER_TRADES};
    use crate::trade_rows::{trade_key, TRADE_KEY_LEN, USER_TRADE_KEY_LEN};

    let mut stats = TradePruneStats::default();
    if cutoff == 0 {
        return Ok(stats);
    }
    let batch_size = batch_size.max(1) as usize;
    let opts = rocksdb::WriteOptions::default();
    let inner = db.inner();
    let mut batch = rocksdb::WriteBatch::default();
    let flush = |batch: &mut rocksdb::WriteBatch, force: bool| -> Result<(), StateError> {
        if batch.len() >= batch_size || (force && !batch.is_empty()) {
            db.write_with(std::mem::take(batch), &opts)?;
        }
        Ok(())
    };

    // --- cf_native_trades: one range per market ---
    let cf = db.cf_handle(CF_NATIVE_TRADES)?;
    let mut it = inner.raw_iterator_cf(cf);
    it.seek_to_first();
    while let Some(key) = it.key() {
        if key.len() < 8 {
            // Not a packed row (the format marker wipes old rows at startup):
            // step over it.
            let next = [key, &[0u8]].concat();
            it.seek(&next);
            continue;
        }
        let market = u64::from_be_bytes(key[..8].try_into().unwrap());
        // The first key of the market: its oldest block.
        let oldest = (key.len() == TRADE_KEY_LEN)
            .then(|| u64::from_be_bytes(key[8..16].try_into().unwrap()));
        if oldest.is_none_or(|b| b < cutoff) {
            batch.delete_range_cf(cf, trade_key(market, 0, 0), trade_key(market, cutoff, 0));
            stats.market_ranges += 1;
            flush(&mut batch, false)?;
        }
        match market.checked_add(1) {
            Some(next) => it.seek(next.to_be_bytes()),
            None => break,
        }
    }
    it.status()?;
    drop(it);

    // --- cf_native_user_trades: point deletes per address ---
    let cf = db.cf_handle(CF_NATIVE_USER_TRADES)?;
    let first_old = (!(cutoff - 1)).to_be_bytes();
    let mut it = inner.raw_iterator_cf(cf);
    it.seek_to_first();
    while let Some(key) = it.key() {
        if key.len() < 20 {
            let next = [key, &[0u8]].concat();
            it.seek(&next);
            continue;
        }
        let addr: [u8; 20] = key[..20].try_into().unwrap();
        let mut start = [0u8; USER_TRADE_KEY_LEN];
        start[..20].copy_from_slice(&addr);
        start[20..].copy_from_slice(&first_old);
        if key < &start[..] {
            it.seek(start);
        }
        // Every row of this address from here on is below the cutoff.
        while let Some(key) = it.key() {
            if key[..20.min(key.len())] != addr[..] {
                break;
            }
            if key.len() == USER_TRADE_KEY_LEN {
                batch.delete_cf(cf, key);
                stats.user_rows += 1;
                flush(&mut batch, false)?;
            }
            it.next();
        }
        // The iterator now sits on the next address (or is exhausted).
    }
    it.status()?;
    drop(it);

    flush(&mut batch, true)?;
    Ok(stats)
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

/// D13: the persisted trade-history cutoff (`META_TRADES_PRUNED_HEIGHT`):
/// trade rows of blocks below it are pruned. 0 if absent (nothing pruned) or
/// not 8 bytes.
pub fn read_trades_pruned_height(db: &StateDb) -> Result<u64, StateError> {
    match db.get_cf_raw(CF_CONSENSUS_META, META_TRADES_PRUNED_HEIGHT)? {
        Some(data) if data.len() == 8 => Ok(u64::from_be_bytes(data[..8].try_into().unwrap())),
        _ => Ok(0),
    }
}

fn write_trades_pruned_height(db: &StateDb, height: u64) -> Result<(), StateError> {
    db.put_cf_raw(
        CF_CONSENSUS_META,
        META_TRADES_PRUNED_HEIGHT,
        &height.to_be_bytes(),
    )
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
        // Every stored block is applied (the tests below prune by retention only).
        set_applied(&db, 19);

        db
    }

    fn set_applied(db: &StateDb, height: u64) {
        db.put_cf_raw(
            crate::cf::CF_CONSENSUS_META,
            crate::cf::META_NATIVE_APPLIED_HEIGHT,
            &height.to_be_bytes(),
        )
        .unwrap();
    }

    /// Crash replay re-executes `(applied, committed]` from the stored bodies:
    /// the pruner must never delete a body execution has not applied, however
    /// far the committed height (what the pruner is driven by) runs ahead.
    #[test]
    fn pruner_never_deletes_bodies_above_applied_marker() {
        let dir = TempDir::new().unwrap();
        let db = setup_db_with_data(dir.path());
        set_applied(&db, 5);
        let config = PrunerConfig {
            retention_blocks: 2,
            prune_interval_blocks: 1,
            batch_size: 100,
            trade_retention_blocks: None,
        };
        let mut pruner = StatePruner::new(db.clone(), config, Arc::new(AtomicU64::new(0)));

        pruner.maybe_prune(19).unwrap();

        assert!(
            pruner.pruned_height() <= 5,
            "cutoff {} above the applied marker",
            pruner.pruned_height()
        );
        for h in 5..20 {
            assert!(
                body_exists(&db, h),
                "body {h} (not yet replay-safe) was pruned"
            );
        }

        // No marker at all = nothing applied: nothing is pruned.
        let dir2 = TempDir::new().unwrap();
        let db2 = StateDb::open(dir2.path()).unwrap();
        db2.put_cf_raw(CF_BLOCK_BODIES, &3u64.to_be_bytes(), b"b")
            .unwrap();
        let mut pruner2 = StatePruner::new(
            db2.clone(),
            PrunerConfig {
                retention_blocks: 0,
                prune_interval_blocks: 1,
                batch_size: 100,
                trade_retention_blocks: None,
            },
            Arc::new(AtomicU64::new(0)),
        );
        pruner2.maybe_prune(10).unwrap();
        assert!(body_exists(&db2, 3));
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
            trade_retention_blocks: None,
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
            trade_retention_blocks: None,
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
            trade_retention_blocks: None,
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
            trade_retention_blocks: None,
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
                trade_retention_blocks: None,
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
                trade_retention_blocks: None,
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
            trade_retention_blocks: None,
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

    // ---- D13 trade-history retention ----

    use crate::cf::{CF_NATIVE_TRADES, CF_NATIVE_USER_TRADES};
    use crate::trade_rows::{
        encode_block, parse_trade_key, parse_user_trade_key, TradeFill, TRADE_CHUNK_FILLS,
    };
    use alloy_primitives::Address;

    fn trade_cfg(retention: u64, trade_retention: Option<u64>, batch: u64) -> PrunerConfig {
        PrunerConfig {
            retention_blocks: retention,
            prune_interval_blocks: 1,
            batch_size: batch,
            trade_retention_blocks: trade_retention,
        }
    }

    /// Write one block's trade rows: one fill per market in `markets`, maker
    /// and taker from `traders` (round robin), `extra` fills more in the first
    /// market (to make several chunks).
    fn write_trades(db: &StateDb, block: u64, markets: &[u64], traders: &[Address], extra: usize) {
        let mut fills = Vec::new();
        let mut i = 0u32;
        for (n, &m) in markets.iter().enumerate() {
            let count = if n == 0 { 1 + extra } else { 1 };
            for _ in 0..count {
                fills.push(TradeFill {
                    trade_index: i,
                    market: m,
                    maker: traders[i as usize % traders.len()],
                    taker: traders[(i as usize + 1) % traders.len()],
                    price_raw: 100 + i as i128,
                    qty_raw: 1,
                    taker_side: (i % 2) as u8,
                });
                i += 1;
            }
        }
        let mut rows = crate::PackedCfBatch::default();
        encode_block(block, 1_000 + block, &fills, &mut rows);
        for (cf, k, v) in rows.iter() {
            db.put_cf_raw(cf, k, v).unwrap();
        }
    }

    /// `(market, block)` of every market row, `(trader, block)` of every user row.
    type TradeBlocks = (Vec<(u64, u64)>, Vec<(Address, u64)>);

    /// Blocks of every `(market, block)` row and every `(trader, block)` row.
    fn trade_blocks(db: &StateDb) -> TradeBlocks {
        use crate::StateBackend;
        let mut m: Vec<_> = db
            .iterate_cf(CF_NATIVE_TRADES, None)
            .unwrap()
            .iter()
            .map(|(k, _)| {
                let (m, b, _) = parse_trade_key(k).unwrap();
                (m, b)
            })
            .collect();
        m.dedup();
        let u = db
            .iterate_cf(CF_NATIVE_USER_TRADES, None)
            .unwrap()
            .iter()
            .map(|(k, _)| parse_user_trade_key(k).unwrap())
            .collect();
        (m, u)
    }

    fn traders() -> Vec<Address> {
        // Includes the all-0x00 and all-0xFF addresses (key-range edges).
        vec![
            Address::repeat_byte(0x00),
            Address::repeat_byte(0x42),
            Address::repeat_byte(0xFF),
        ]
    }

    /// Markets including 0 and u64::MAX (key-range edges).
    const MARKETS: [u64; 4] = [0, 7, 300, u64::MAX];

    fn trade_db(dir: &std::path::Path, blocks: std::ops::Range<u64>) -> StateDb {
        let db = StateDb::open(dir).unwrap();
        for b in blocks {
            write_trades(&db, b, &MARKETS, &traders(), 0);
        }
        db
    }

    #[test]
    fn pruner_prunes_trades_below_retention() {
        let dir = TempDir::new().unwrap();
        let db = trade_db(dir.path(), 0..20);
        // Block 3 also has a multi-chunk market row.
        write_trades(&db, 3, &MARKETS, &traders(), 2 * TRADE_CHUNK_FILLS);
        set_applied(&db, 19);
        let cut = Arc::new(AtomicU64::new(0));
        let mut pruner = StatePruner::new(
            db.clone(),
            trade_cfg(u64::MAX, Some(10), 3),
            Arc::new(AtomicU64::new(0)),
        )
        .with_trades_pruned_handle(cut.clone());

        pruner.maybe_prune(19).unwrap();

        // cutoff = 19 - 10 = 9: blocks 0..9 gone, 9..20 kept, in both CFs.
        assert_eq!(pruner.trades_pruned_height(), 9);
        assert_eq!(cut.load(Ordering::Acquire), 9);
        let (m, u) = trade_blocks(&db);
        let want_m: Vec<_> = MARKETS
            .iter()
            .flat_map(|&mk| (9..20).map(move |b| (mk, b)))
            .collect();
        assert_eq!(m, want_m, "market rows");
        for t in traders() {
            let blocks: Vec<u64> = u.iter().filter(|(a, _)| *a == t).map(|(_, b)| *b).collect();
            assert_eq!(blocks, (9..20).rev().collect::<Vec<_>>(), "trader {t} rows");
        }
        // Bodies are untouched without --retention-blocks.
        assert_eq!(pruner.pruned_height(), 0);

        // A later pass moves the cutoff on; nothing at or above it goes.
        pruner.maybe_prune(25).unwrap();
        assert_eq!(pruner.trades_pruned_height(), 15);
        let (m, u) = trade_blocks(&db);
        assert_eq!(m.iter().map(|x| x.1).min(), Some(15));
        assert_eq!(u.iter().map(|x| x.1).min(), Some(15));
        // Far ahead: clamped to the applied marker 19, so block 19 stays.
        pruner.maybe_prune(40).unwrap();
        assert_eq!(
            pruner.trades_pruned_height(),
            19,
            "clamped to the applied marker 19"
        );
        let (m, u) = trade_blocks(&db);
        assert!(m.iter().all(|&(_, b)| b == 19), "{m:?}");
        assert!(u.iter().all(|&(_, b)| b == 19), "{u:?}");
        assert_eq!(m.len(), MARKETS.len());
        assert_eq!(u.len(), traders().len());
    }

    /// Unset = keep every trade row (today), whatever the body retention.
    #[test]
    fn pruner_trades_unset_keeps_everything() {
        let dir = TempDir::new().unwrap();
        let db = trade_db(dir.path(), 0..20);
        set_applied(&db, 19);
        let before = trade_blocks(&db);
        let mut pruner = StatePruner::new(
            db.clone(),
            trade_cfg(2, None, 100),
            Arc::new(AtomicU64::new(0)),
        );
        pruner.maybe_prune(19).unwrap();
        assert_eq!(pruner.pruned_height(), 17, "bodies still pruned");
        assert_eq!(pruner.trades_pruned_height(), 0);
        assert_eq!(trade_blocks(&db), before);
    }

    /// Trade rows of blocks at or above the applied marker stay (crash replay
    /// rewrites them; deleting them first would leak the rewrite).
    #[test]
    fn pruner_trades_cutoff_clamped_to_applied_marker() {
        let dir = TempDir::new().unwrap();
        let db = trade_db(dir.path(), 0..20);
        set_applied(&db, 4);
        let mut pruner = StatePruner::new(
            db.clone(),
            trade_cfg(u64::MAX, Some(0), 100),
            Arc::new(AtomicU64::new(0)),
        );
        pruner.maybe_prune(19).unwrap();
        assert_eq!(pruner.trades_pruned_height(), 4);
        let (m, u) = trade_blocks(&db);
        assert_eq!(m.iter().map(|x| x.1).min(), Some(4));
        assert_eq!(u.iter().map(|x| x.1).min(), Some(4));

        // No marker = nothing applied = nothing pruned.
        let dir2 = TempDir::new().unwrap();
        let db2 = trade_db(dir2.path(), 0..5);
        let before = trade_blocks(&db2);
        let mut p2 = StatePruner::new(
            db2.clone(),
            trade_cfg(u64::MAX, Some(0), 100),
            Arc::new(AtomicU64::new(0)),
        );
        p2.maybe_prune(10).unwrap();
        assert_eq!(trade_blocks(&db2), before);
    }

    /// The background trade writer can land a block's rows after the pass
    /// that covered it; every pass deletes everything below its cutoff, so
    /// the next pass removes them. A restart also prunes from scratch.
    #[test]
    fn pruner_trades_late_rows_and_restart_are_pruned() {
        let dir = TempDir::new().unwrap();
        let db = trade_db(dir.path(), 0..20);
        set_applied(&db, 19);
        let mut pruner = StatePruner::new(
            db.clone(),
            trade_cfg(u64::MAX, Some(10), 100),
            Arc::new(AtomicU64::new(0)),
        );
        pruner.maybe_prune(19).unwrap();
        // A late row for block 2, a new trader, a new market.
        write_trades(
            &db,
            2,
            &[55],
            &[Address::repeat_byte(0x99), Address::repeat_byte(0x98)],
            0,
        );
        set_applied(&db, 30);
        pruner.maybe_prune(20).unwrap();
        let (m, u) = trade_blocks(&db);
        assert!(m.iter().all(|&(_, b)| b >= 10), "{m:?}");
        assert!(u.iter().all(|&(_, b)| b >= 10), "{u:?}");

        // Restart: a fresh pruner with the same settings removes old rows too.
        write_trades(&db, 1, &[56], &traders(), 0);
        drop(pruner);
        let mut again = StatePruner::new(
            db.clone(),
            trade_cfg(u64::MAX, Some(10), 100),
            Arc::new(AtomicU64::new(0)),
        );
        again.maybe_prune(20).unwrap();
        let (m, u) = trade_blocks(&db);
        assert!(m.iter().all(|&(_, b)| b >= 10), "{m:?}");
        assert!(u.iter().all(|&(_, b)| b >= 10), "{u:?}");
    }

    /// The cutoff is persisted (`META_TRADES_PRUNED_HEIGHT`) and never lowered:
    /// a restart with a larger N, or without the flag, keeps the old cutoff,
    /// so blocks whose rows are gone stay pruned for RPC.
    #[test]
    fn pruner_trades_cutoff_persists_across_restart_with_larger_n() {
        let dir = TempDir::new().unwrap();
        let db = trade_db(dir.path(), 0..20);
        set_applied(&db, 19);
        assert_eq!(read_trades_pruned_height(&db).unwrap(), 0, "absent = 0");
        let mut pruner = StatePruner::new(
            db.clone(),
            trade_cfg(u64::MAX, Some(10), 100),
            Arc::new(AtomicU64::new(0)),
        );
        pruner.maybe_prune(19).unwrap();
        assert_eq!(read_trades_pruned_height(&db).unwrap(), 9);
        drop(pruner);

        // Restart with N = 15: cutoff 5 < 9, the stored 9 stands.
        let cut = Arc::new(AtomicU64::new(0));
        let mut larger = StatePruner::new(
            db.clone(),
            trade_cfg(u64::MAX, Some(15), 100),
            Arc::new(AtomicU64::new(0)),
        )
        .with_trades_pruned_handle(cut.clone());
        assert_eq!(
            cut.load(Ordering::Acquire),
            9,
            "known before the first pass"
        );
        // A row the trade writer landed late below the stored cutoff.
        write_trades(&db, 7, &[57], &traders(), 0);
        larger.maybe_prune(20).unwrap();
        assert_eq!(larger.trades_pruned_height(), 9);
        assert_eq!(read_trades_pruned_height(&db).unwrap(), 9, "never lowered");
        let (m, u) = trade_blocks(&db);
        assert!(m.iter().all(|&(_, b)| b >= 9), "{m:?}");
        assert!(u.iter().all(|&(_, b)| b >= 9), "{u:?}");
        // Later passes raise it again once N = 15 passes the old cutoff.
        set_applied(&db, 30);
        larger.maybe_prune(30).unwrap();
        assert_eq!(read_trades_pruned_height(&db).unwrap(), 15);
        drop(larger);

        // Restart without the flag (bodies only): the cutoff stays 15.
        let p = StatePruner::new(
            db.clone(),
            trade_cfg(5, None, 100),
            Arc::new(AtomicU64::new(0)),
        );
        assert_eq!(p.trades_pruned_height(), 15);
    }

    /// Trade pruning follows the pruner's interval like bodies do.
    #[test]
    fn pruner_trades_respect_interval() {
        let dir = TempDir::new().unwrap();
        let db = trade_db(dir.path(), 0..20);
        set_applied(&db, 19);
        let mut cfg = trade_cfg(u64::MAX, Some(5), 100);
        cfg.prune_interval_blocks = 10;
        let mut pruner = StatePruner::new(db.clone(), cfg, Arc::new(AtomicU64::new(0)));
        pruner.maybe_prune(5).unwrap();
        assert_eq!(pruner.trades_pruned_height(), 0, "interval not reached");
        pruner.maybe_prune(10).unwrap();
        assert_eq!(pruner.trades_pruned_height(), 5);
        pruner.maybe_prune(15).unwrap();
        assert_eq!(pruner.trades_pruned_height(), 5, "next pass at 20");
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
