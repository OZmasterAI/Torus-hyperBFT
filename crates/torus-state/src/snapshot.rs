//! State snapshot creation, verification, and recovery using RocksDB Checkpoint API.
//!
//! Provides atomic point-in-time snapshots of the full database (all column families).
//! Snapshots are hardlink-based, making creation near-instant.

use std::path::{Path, PathBuf};

use alloy_primitives::B256;
use rocksdb::{checkpoint::Checkpoint, ColumnFamilyDescriptor, Options, DB};
use serde::{Deserialize, Serialize};

use crate::cf::ALL_CF_NAMES;
use crate::db::StateDb;
use crate::error::StateError;
use crate::native_trie::native_root_full;
use crate::trie::{compute_composite_root, compute_state_root_from_db};

/// Metadata recorded alongside a snapshot.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SnapshotMetadata {
    /// Block height at which the snapshot was taken.
    pub block_height: u64,
    /// Block hash at the snapshot height.
    pub block_hash: B256,
    /// State root computed at the snapshot height.
    pub state_root: B256,
    /// Unix timestamp when the snapshot was created.
    pub timestamp: u64,
    /// Hash of the validator set at the snapshot epoch.
    pub validator_set_hash: B256,
    /// Number of column families in the snapshot.
    pub column_family_count: usize,
}

/// Result of verifying a snapshot.
#[derive(Clone, Debug)]
pub struct SnapshotVerifyResult {
    /// Whether the snapshot passed all checks.
    pub verified: bool,
    /// Recorded state root from metadata.
    pub recorded_state_root: B256,
    /// Recomputed state root from snapshot data.
    pub computed_state_root: B256,
    /// Block height of the snapshot.
    pub block_height: u64,
    /// Human-readable error if verification failed.
    pub error: Option<String>,
}

/// Configuration for automatic periodic snapshots.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SnapshotConfig {
    /// Create a snapshot every N blocks (0 = disabled).
    pub snapshot_interval: u64,
    /// Maximum number of snapshots to keep (prune older ones).
    pub max_snapshots: usize,
    /// Base directory for snapshot storage.
    pub snapshot_dir: PathBuf,
}

impl Default for SnapshotConfig {
    fn default() -> Self {
        Self {
            snapshot_interval: 0,
            max_snapshots: 3,
            snapshot_dir: PathBuf::from("./snapshots"),
        }
    }
}

const METADATA_FILENAME: &str = "snapshot_metadata.json";

impl StateDb {
    /// Create an atomic point-in-time snapshot of the full database using RocksDB Checkpoint.
    ///
    /// The snapshot includes all column families at a consistent point in time.
    /// Metadata is written as a JSON sidecar file alongside the checkpoint.
    pub fn create_snapshot(
        &self,
        output_path: &Path,
        metadata: &SnapshotMetadata,
    ) -> Result<(), StateError> {
        // Ensure parent directory exists
        if let Some(parent) = output_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // Create checkpoint (atomic, hardlink-based)
        let checkpoint = Checkpoint::new(self.inner()).map_err(|e| StateError::RocksDb(e))?;
        checkpoint
            .create_checkpoint(output_path)
            .map_err(|e| StateError::RocksDb(e))?;

        // Write metadata sidecar
        let metadata_path = output_path.join(METADATA_FILENAME);
        let metadata_json = serde_json::to_string_pretty(metadata)
            .map_err(|e| StateError::InvalidData(format!("serialize metadata: {e}")))?;
        std::fs::write(&metadata_path, metadata_json)?;

        tracing::info!(
            path = %output_path.display(),
            block_height = metadata.block_height,
            "snapshot created"
        );
        Ok(())
    }

    /// Verify a snapshot by opening it read-only and recomputing the state root.
    ///
    /// Returns a detailed report of the verification outcome.
    pub fn verify_snapshot(snapshot_path: &Path) -> Result<SnapshotVerifyResult, StateError> {
        // Read metadata
        let metadata_path = snapshot_path.join(METADATA_FILENAME);
        let metadata_json = std::fs::read_to_string(&metadata_path)?;
        let metadata: SnapshotMetadata = serde_json::from_str(&metadata_json)
            .map_err(|e| StateError::InvalidData(format!("parse metadata: {e}")))?;

        // Open snapshot read-only
        let mut opts = Options::default();
        opts.create_if_missing(false);
        opts.create_missing_column_families(false);

        let cf_descriptors: Vec<ColumnFamilyDescriptor> = ALL_CF_NAMES
            .iter()
            .map(|name| ColumnFamilyDescriptor::new(*name, Options::default()))
            .collect();

        let db = DB::open_cf_descriptors_read_only(&opts, snapshot_path, cf_descriptors, false)
            .map_err(|e| StateError::RocksDb(e))?;

        let snapshot_db = StateDb::from_existing_db(db);

        // FIX 6 (EVM-PF-11): Recompute the *composite* root (EVM + native) to
        // compare like-for-like against metadata.state_root which is the composite root.
        let evm_root = compute_state_root_from_db(&snapshot_db)?;
        // Native half = the bucketed-Merkle root (Phase A A2.3), matching the consensus native root
        // (`torus_bridge::state_root::flagged_native_root`) so a snapshot verifies like-for-like
        // against the block's composite `state_root`. (Replaces the divergent 5-CF flat keccak.)
        let native_root = native_root_full(&snapshot_db)?;
        let computed_root = compute_composite_root(evm_root, native_root);

        let verified = computed_root == metadata.state_root;
        let error = if verified {
            None
        } else {
            Some(format!(
                "state root mismatch: recorded={}, computed={}",
                metadata.state_root, computed_root
            ))
        };

        Ok(SnapshotVerifyResult {
            verified,
            recorded_state_root: metadata.state_root,
            computed_state_root: computed_root,
            block_height: metadata.block_height,
            error,
        })
    }

    /// Restore the database from a snapshot by copying it to the data directory.
    ///
    /// Verifies the snapshot state root before accepting.
    /// Returns the snapshot metadata on success.
    pub fn restore_from_snapshot(
        snapshot_path: &Path,
        data_dir: &Path,
    ) -> Result<SnapshotMetadata, StateError> {
        // Verify first
        let result = Self::verify_snapshot(snapshot_path)?;
        if !result.verified {
            return Err(StateError::SnapshotVerificationFailed(
                result.error.unwrap_or_else(|| "unknown error".to_string()),
            ));
        }

        // Read metadata
        let metadata_path = snapshot_path.join(METADATA_FILENAME);
        let metadata_json = std::fs::read_to_string(&metadata_path)?;
        let metadata: SnapshotMetadata = serde_json::from_str(&metadata_json)
            .map_err(|e| StateError::InvalidData(format!("parse metadata: {e}")))?;

        // FIX 4 (EVM-FIND-06): Atomic directory swap.
        // A crash between remove_dir_all and copy_dir_recursive would leave no
        // data directory at all. Instead: copy to temp, rename old → .old,
        // rename temp → data_dir. At every point at least one valid dir exists.
        let temp_dir = data_dir.with_extension("restoring");
        let old_dir = data_dir.with_extension("old");

        // Clean up leftover dirs from any previous failed restore.
        if temp_dir.exists() {
            std::fs::remove_dir_all(&temp_dir)?;
        }
        if old_dir.exists() {
            std::fs::remove_dir_all(&old_dir)?;
        }

        // Step 1: Copy snapshot to temp location (data_dir still intact).
        copy_dir_recursive(snapshot_path, &temp_dir)?;

        // Step 2: Atomic swap — rename is atomic on the same filesystem.
        if data_dir.exists() {
            std::fs::rename(data_dir, &old_dir)?;
        }
        std::fs::rename(&temp_dir, data_dir)?;

        // Step 3: Cleanup old data (non-critical — failure is safe).
        if old_dir.exists() {
            let _ = std::fs::remove_dir_all(&old_dir);
        }

        tracing::info!(
            snapshot = %snapshot_path.display(),
            data_dir = %data_dir.display(),
            block_height = metadata.block_height,
            "database restored from snapshot"
        );

        Ok(metadata)
    }
}

/// Manage periodic auto-snapshots: create at intervals, prune old ones.
pub struct SnapshotManager {
    config: SnapshotConfig,
}

impl SnapshotManager {
    pub fn new(config: SnapshotConfig) -> Self {
        Self { config }
    }

    /// Check if a snapshot should be created at this block height.
    pub fn should_snapshot(&self, block_height: u64) -> bool {
        self.config.snapshot_interval > 0
            && block_height > 0
            && block_height % self.config.snapshot_interval == 0
    }

    /// Create a snapshot and prune old ones if needed.
    pub fn create_and_prune(
        &self,
        db: &StateDb,
        metadata: &SnapshotMetadata,
    ) -> Result<PathBuf, StateError> {
        let snapshot_name = format!("snapshot_{}", metadata.block_height);
        let snapshot_path = self.config.snapshot_dir.join(&snapshot_name);

        db.create_snapshot(&snapshot_path, metadata)?;
        self.prune_old_snapshots()?;

        Ok(snapshot_path)
    }

    /// Remove old snapshots, keeping only the most recent `max_snapshots`.
    fn prune_old_snapshots(&self) -> Result<(), StateError> {
        let dir = &self.config.snapshot_dir;
        if !dir.exists() {
            return Ok(());
        }

        let mut snapshots: Vec<(u64, PathBuf)> = Vec::new();
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                let metadata_file = path.join(METADATA_FILENAME);
                if metadata_file.exists() {
                    if let Ok(json) = std::fs::read_to_string(&metadata_file) {
                        if let Ok(meta) = serde_json::from_str::<SnapshotMetadata>(&json) {
                            snapshots.push((meta.block_height, path));
                        }
                    }
                }
            }
        }

        // Sort by block height descending
        snapshots.sort_by(|a, b| b.0.cmp(&a.0));

        // Remove snapshots beyond max_snapshots
        for (_height, path) in snapshots.iter().skip(self.config.max_snapshots) {
            tracing::info!(path = %path.display(), "pruning old snapshot");
            if let Err(e) = std::fs::remove_dir_all(path) {
                tracing::warn!(path = %path.display(), %e, "failed to prune snapshot");
            }
        }

        Ok(())
    }
}

/// Recursively copy a directory.
fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<(), StateError> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if src_path.is_dir() {
            copy_dir_recursive(&src_path, &dst_path)?;
        } else {
            std::fs::copy(&src_path, &dst_path)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{Address, U256};
    use revm::state::AccountInfo;
    use tempfile::TempDir;

    fn make_test_db(dir: &Path) -> StateDb {
        let db = StateDb::open(dir).unwrap();
        // Insert some test accounts
        let addr1 = Address::repeat_byte(0x01);
        let addr2 = Address::repeat_byte(0x02);
        db.put_account(
            &addr1,
            &AccountInfo {
                balance: U256::from(1000u64),
                nonce: 1,
                code_hash: B256::ZERO,
                code: None,
                account_id: None,
            },
        )
        .unwrap();
        db.put_account(
            &addr2,
            &AccountInfo {
                balance: U256::from(2000u64),
                nonce: 5,
                code_hash: B256::ZERO,
                code: None,
                account_id: None,
            },
        )
        .unwrap();
        db
    }

    fn test_metadata(state_root: B256) -> SnapshotMetadata {
        SnapshotMetadata {
            block_height: 100,
            block_hash: B256::ZERO,
            state_root,
            timestamp: 1234567890,
            validator_set_hash: B256::ZERO,
            column_family_count: ALL_CF_NAMES.len(),
        }
    }

    /// Compute the composite state root (EVM + native) matching what verify_snapshot expects.
    fn composite_root_from_db(db: &StateDb) -> B256 {
        let evm_root = compute_state_root_from_db(db).unwrap();
        let native_root = native_root_full(db).unwrap();
        compute_composite_root(evm_root, native_root)
    }

    #[test]
    fn create_snapshot_and_verify_passes() {
        let dir = TempDir::new().unwrap();
        let db = make_test_db(dir.path());
        let state_root = composite_root_from_db(&db);

        let snap_dir = TempDir::new().unwrap();
        let snap_path = snap_dir.path().join("snap1");

        db.create_snapshot(&snap_path, &test_metadata(state_root))
            .unwrap();

        let result = StateDb::verify_snapshot(&snap_path).unwrap();
        assert!(
            result.verified,
            "snapshot should verify: {:?}",
            result.error
        );
        assert_eq!(result.block_height, 100);
        assert_eq!(result.recorded_state_root, result.computed_state_root);
    }

    #[test]
    fn verify_detects_corrupted_snapshot() {
        let dir = TempDir::new().unwrap();
        let db = make_test_db(dir.path());
        let state_root = composite_root_from_db(&db);

        let snap_dir = TempDir::new().unwrap();
        let snap_path = snap_dir.path().join("snap_corrupt");
        db.create_snapshot(&snap_path, &test_metadata(state_root))
            .unwrap();

        // Corrupt: write a wrong state root in metadata
        let bad_root = B256::repeat_byte(0xFF);
        let bad_meta = test_metadata(bad_root);
        let metadata_path = snap_path.join(METADATA_FILENAME);
        std::fs::write(
            &metadata_path,
            serde_json::to_string_pretty(&bad_meta).unwrap(),
        )
        .unwrap();

        let result = StateDb::verify_snapshot(&snap_path).unwrap();
        assert!(!result.verified);
        assert!(result.error.is_some());
        assert_ne!(result.recorded_state_root, result.computed_state_root);
    }

    #[test]
    fn restore_from_valid_snapshot() {
        let dir = TempDir::new().unwrap();
        let db = make_test_db(dir.path());
        let state_root = composite_root_from_db(&db);

        let snap_dir = TempDir::new().unwrap();
        let snap_path = snap_dir.path().join("snap_restore");
        db.create_snapshot(&snap_path, &test_metadata(state_root))
            .unwrap();
        drop(db);

        // Restore to a new directory
        let restore_dir = TempDir::new().unwrap();
        let restore_path = restore_dir.path().join("restored_data");
        let meta = StateDb::restore_from_snapshot(&snap_path, &restore_path).unwrap();
        assert_eq!(meta.block_height, 100);

        // Open restored DB and verify data
        let restored = StateDb::open(&restore_path).unwrap();
        let acct = restored
            .get_account(&Address::repeat_byte(0x01))
            .unwrap()
            .unwrap();
        assert_eq!(acct.balance, U256::from(1000u64));
        assert_eq!(acct.nonce, 1);
    }

    #[test]
    fn restore_rejects_corrupted_snapshot() {
        let dir = TempDir::new().unwrap();
        let db = make_test_db(dir.path());
        let state_root = composite_root_from_db(&db);

        let snap_dir = TempDir::new().unwrap();
        let snap_path = snap_dir.path().join("snap_bad");
        db.create_snapshot(&snap_path, &test_metadata(state_root))
            .unwrap();

        // Corrupt metadata
        let bad_meta = test_metadata(B256::repeat_byte(0xAA));
        std::fs::write(
            snap_path.join(METADATA_FILENAME),
            serde_json::to_string_pretty(&bad_meta).unwrap(),
        )
        .unwrap();

        let restore_dir = TempDir::new().unwrap();
        let result = StateDb::restore_from_snapshot(&snap_path, &restore_dir.path().join("data"));
        assert!(result.is_err());
    }

    #[test]
    fn auto_snapshot_prunes_old_ones() {
        let dir = TempDir::new().unwrap();
        let db = make_test_db(dir.path());
        let state_root = composite_root_from_db(&db);

        let snap_base = TempDir::new().unwrap();
        let config = SnapshotConfig {
            snapshot_interval: 10,
            max_snapshots: 2,
            snapshot_dir: snap_base.path().to_path_buf(),
        };
        let mgr = SnapshotManager::new(config);

        assert!(mgr.should_snapshot(10));
        assert!(mgr.should_snapshot(20));
        assert!(!mgr.should_snapshot(15));
        assert!(!mgr.should_snapshot(0));

        // Create 3 snapshots
        for height in [10, 20, 30] {
            let mut meta = test_metadata(state_root);
            meta.block_height = height;
            mgr.create_and_prune(&db, &meta).unwrap();
        }

        // Should have pruned the oldest, keeping only 2
        let entries: Vec<_> = std::fs::read_dir(snap_base.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_dir())
            .collect();
        assert_eq!(entries.len(), 2, "should keep max_snapshots=2");
    }
}
