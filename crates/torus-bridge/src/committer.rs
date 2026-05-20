//! Block commitment — persist state changes, block data, and receipts to RocksDB.

use alloy_primitives::B256;
use rocksdb::WriteBatch;

use revm::database::BundleState;

// AUDIT: EVM-FIND-21 -- CF_LOGS / CF_LOGS_BLOOM not populated in commit path.
// Log indexing deferred; logs served from receipts in RPC layer.
use torus_state::cf::{
    CF_ACCOUNTS, CF_BLOCK_BODIES, CF_BLOCK_HASH_TO_NUMBER, CF_BLOCK_HEADERS, CF_CODE, CF_RECEIPTS,
    CF_STORAGE, CF_TX_HASH_TO_LOCATION,
};
use torus_state::db::{encode_account_info, storage_key, StateDb};
use torus_types::{Receipt, TorusBlock};

use crate::error::BridgeError;

/// Persists validated blocks to the state database.
pub struct BlockCommitter;

impl BlockCommitter {
    /// Commit a validated block: apply state changes and persist block/receipt data.
    ///
    /// All writes go through a single `WriteBatch` for atomicity — either
    /// all changes are applied or none are, preventing corrupt partial state
    /// on crash (FIX 3: EVM-FIND-03).
    ///
    /// Returns the block hash.
    pub fn commit_block(
        state_db: &StateDb,
        block: &TorusBlock,
        bundle: &BundleState,
        receipts: &[Receipt],
    ) -> Result<B256, BridgeError> {
        let mut batch = WriteBatch::default();

        // Resolve CF handles up front.
        let cf_accounts = state_db.cf_handle(CF_ACCOUNTS)?;
        let cf_storage = state_db.cf_handle(CF_STORAGE)?;
        let cf_code = state_db.cf_handle(CF_CODE)?;
        let cf_headers = state_db.cf_handle(CF_BLOCK_HEADERS)?;
        let cf_bodies = state_db.cf_handle(CF_BLOCK_BODIES)?;
        let cf_receipts = state_db.cf_handle(CF_RECEIPTS)?;
        let cf_hash_to_num = state_db.cf_handle(CF_BLOCK_HASH_TO_NUMBER)?;
        let cf_tx_loc = state_db.cf_handle(CF_TX_HASH_TO_LOCATION)?;

        // 1. Apply EVM state changes to batch.
        for (address, bundle_acct) in &bundle.state {
            match &bundle_acct.info {
                Some(info) => {
                    batch.put_cf(cf_accounts, address.as_slice(), encode_account_info(info));
                    // Write storage only for live accounts.
                    for (slot, slot_val) in &bundle_acct.storage {
                        let key = storage_key(address, slot);
                        if slot_val.present_value.is_zero() {
                            batch.delete_cf(cf_storage, &key);
                        } else {
                            batch.put_cf(
                                cf_storage,
                                &key,
                                slot_val.present_value.to_be_bytes::<32>(),
                            );
                        }
                    }
                }
                None => {
                    // Account destroyed.
                    if bundle_acct.original_info.is_some() {
                        batch.delete_cf(cf_accounts, address.as_slice());
                    }
                    // FIX 10 (EVM-FIND-20): Clean up storage for destroyed accounts.
                    // Handles same-block create+destroy (orphaned storage) and
                    // normal destruction. Delete all storage entries tracked by the
                    // bundle rather than writing them.
                    for (slot, _) in &bundle_acct.storage {
                        batch.delete_cf(cf_storage, &storage_key(address, slot));
                    }
                }
            }
        }

        for (code_hash, bytecode) in &bundle.contracts {
            let raw = bytecode.bytes();
            batch.put_cf(cf_code, code_hash.as_slice(), raw.as_ref());
        }

        // 2. Compute block hash using canonical encoding (deterministic across serde versions).
        let canonical_bytes = block.header.canonical_header_bytes();
        let block_hash = alloy_primitives::keccak256(&canonical_bytes);

        // 3. Store block header with hash prefix (FIX 1: EVM-PF-08).
        // Format: block_hash(32) || header_json(variable).
        // `get_block_hash` reads the first 32 bytes to return the correct hash
        // for the BLOCKHASH opcode, while RPC reads bytes[32..] for header JSON.
        let height_key = block.header.height.to_be_bytes();
        let header_json = serde_json::to_vec(&block.header)
            .map_err(|e| BridgeError::Serialization(e.to_string()))?;
        let mut header_data = Vec::with_capacity(32 + header_json.len());
        header_data.extend_from_slice(block_hash.as_slice());
        header_data.extend_from_slice(&header_json);
        batch.put_cf(cf_headers, &height_key, &header_data);

        // 4. Store block body.
        let body = block.body();
        let body_bytes =
            serde_json::to_vec(&body).map_err(|e| BridgeError::Serialization(e.to_string()))?;
        batch.put_cf(cf_bodies, &height_key, &body_bytes);

        // 5. Store receipts (key = height(8) || tx_index(4)).
        for receipt in receipts {
            let mut key = [0u8; 12];
            key[..8].copy_from_slice(&height_key);
            key[8..12].copy_from_slice(&receipt.tx_index.to_be_bytes());
            let receipt_bytes = serde_json::to_vec(receipt)
                .map_err(|e| BridgeError::Serialization(e.to_string()))?;
            batch.put_cf(cf_receipts, &key, &receipt_bytes);
        }

        // 6. Block hash → number index.
        batch.put_cf(cf_hash_to_num, block_hash.as_slice(), &height_key);

        // 7. Tx hash → location index (height(8) || tx_index(4)).
        for (i, _tx_bytes) in block.evm_transactions.iter().enumerate() {
            if let Some(receipt) = receipts.get(i) {
                let mut location = [0u8; 12];
                location[..8].copy_from_slice(&height_key);
                location[8..12].copy_from_slice(&(i as u32).to_be_bytes());
                batch.put_cf(cf_tx_loc, receipt.tx_hash.as_slice(), &location);
            }
        }

        // Atomic write — all or nothing.
        state_db.write(batch)?;

        Ok(block_hash)
    }

    /// Write block metadata (header, body, receipts, indices) WITHOUT EVM state.
    ///
    /// Used in deferred-commit mode where EVM state is flushed later when
    /// consensus confirms the block.
    pub fn commit_block_metadata(
        state_db: &StateDb,
        block: &TorusBlock,
        receipts: &[Receipt],
    ) -> Result<B256, BridgeError> {
        let mut batch = rocksdb::WriteBatch::default();

        let cf_headers = state_db.cf_handle(CF_BLOCK_HEADERS)?;
        let cf_bodies = state_db.cf_handle(CF_BLOCK_BODIES)?;
        let cf_receipts_cf = state_db.cf_handle(CF_RECEIPTS)?;
        let cf_hash_to_num = state_db.cf_handle(CF_BLOCK_HASH_TO_NUMBER)?;
        let cf_tx_loc = state_db.cf_handle(CF_TX_HASH_TO_LOCATION)?;

        let canonical_bytes = block.header.canonical_header_bytes();
        let block_hash = alloy_primitives::keccak256(&canonical_bytes);
        let height_key = block.header.height.to_be_bytes();

        let header_json = serde_json::to_vec(&block.header)
            .map_err(|e| BridgeError::Serialization(e.to_string()))?;
        let mut header_data = Vec::with_capacity(32 + header_json.len());
        header_data.extend_from_slice(block_hash.as_slice());
        header_data.extend_from_slice(&header_json);
        batch.put_cf(cf_headers, &height_key, &header_data);

        let body = block.body();
        let body_bytes =
            serde_json::to_vec(&body).map_err(|e| BridgeError::Serialization(e.to_string()))?;
        batch.put_cf(cf_bodies, &height_key, &body_bytes);

        for receipt in receipts {
            let mut key = [0u8; 12];
            key[..8].copy_from_slice(&height_key);
            key[8..12].copy_from_slice(&receipt.tx_index.to_be_bytes());
            let receipt_bytes = serde_json::to_vec(receipt)
                .map_err(|e| BridgeError::Serialization(e.to_string()))?;
            batch.put_cf(cf_receipts_cf, &key, &receipt_bytes);
        }

        batch.put_cf(cf_hash_to_num, block_hash.as_slice(), &height_key);

        for (i, _tx_bytes) in block.evm_transactions.iter().enumerate() {
            if let Some(receipt) = receipts.get(i) {
                let mut location = [0u8; 12];
                location[..8].copy_from_slice(&height_key);
                location[8..12].copy_from_slice(&(i as u32).to_be_bytes());
                batch.put_cf(cf_tx_loc, receipt.tx_hash.as_slice(), &location);
            }
        }

        state_db.write(batch)?;
        Ok(block_hash)
    }

    /// Flush only the EVM state portion of a pending bundle to state_db.
    ///
    /// Unlike [`commit_block`](Self::commit_block), this does NOT write block
    /// headers, bodies, receipts, or indices — those are written eagerly
    /// during validation. This only applies account/storage/code changes.
    pub fn commit_pending_bundle(
        state_db: &StateDb,
        bundle: &BundleState,
    ) -> Result<(), BridgeError> {
        let mut batch = rocksdb::WriteBatch::default();

        let cf_accounts = state_db.cf_handle(CF_ACCOUNTS)?;
        let cf_storage = state_db.cf_handle(CF_STORAGE)?;
        let cf_code = state_db.cf_handle(CF_CODE)?;

        for (address, bundle_acct) in &bundle.state {
            match &bundle_acct.info {
                Some(info) => {
                    batch.put_cf(cf_accounts, address.as_slice(), encode_account_info(info));
                    for (slot, slot_val) in &bundle_acct.storage {
                        let key = storage_key(address, slot);
                        if slot_val.present_value.is_zero() {
                            batch.delete_cf(cf_storage, &key);
                        } else {
                            batch.put_cf(
                                cf_storage,
                                &key,
                                slot_val.present_value.to_be_bytes::<32>(),
                            );
                        }
                    }
                }
                None => {
                    if bundle_acct.original_info.is_some() {
                        batch.delete_cf(cf_accounts, address.as_slice());
                    }
                    for (slot, _) in &bundle_acct.storage {
                        batch.delete_cf(cf_storage, &storage_key(address, slot));
                    }
                }
            }
        }

        for (code_hash, bytecode) in &bundle.contracts {
            let raw = bytecode.bytes();
            batch.put_cf(cf_code, code_hash.as_slice(), raw.as_ref());
        }

        state_db.write(batch)?;
        Ok(())
    }
}
