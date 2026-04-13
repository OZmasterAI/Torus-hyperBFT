//! Block commitment — persist state changes, block data, and receipts to RocksDB.

use alloy_primitives::B256;

use revm::database::BundleState;

use torus_state::cf::{
    CF_BLOCK_BODIES, CF_BLOCK_HASH_TO_NUMBER, CF_BLOCK_HEADERS, CF_RECEIPTS, CF_TX_HASH_TO_LOCATION,
};
use torus_state::db::StateDb;
use torus_types::{Receipt, TorusBlock};

use crate::error::BridgeError;

/// Persists validated blocks to the state database.
pub struct BlockCommitter;

impl BlockCommitter {
    /// Commit a validated block: apply state changes and persist block/receipt data.
    ///
    /// Returns the block hash.
    pub fn commit_block(
        state_db: &StateDb,
        block: &TorusBlock,
        bundle: &BundleState,
        receipts: &[Receipt],
    ) -> Result<B256, BridgeError> {
        // 1. Apply EVM state changes.
        apply_bundle_to_db(state_db, bundle)?;

        // 2. Compute block hash (keccak256 of serialised header).
        let header_bytes = serde_json::to_vec(&block.header)
            .map_err(|e| BridgeError::Serialization(e.to_string()))?;
        let block_hash = alloy_primitives::keccak256(&header_bytes);

        // 3. Store block header.
        let height_key = block.header.height.to_be_bytes();
        state_db.put_cf_raw(CF_BLOCK_HEADERS, &height_key, &header_bytes)?;

        // 4. Store block body.
        let body = block.body();
        let body_bytes =
            serde_json::to_vec(&body).map_err(|e| BridgeError::Serialization(e.to_string()))?;
        state_db.put_cf_raw(CF_BLOCK_BODIES, &height_key, &body_bytes)?;

        // 5. Store receipts (key = height(8) || tx_index(4)).
        for receipt in receipts {
            let mut key = [0u8; 12];
            key[..8].copy_from_slice(&height_key);
            key[8..12].copy_from_slice(&receipt.tx_index.to_be_bytes());
            let receipt_bytes = serde_json::to_vec(receipt)
                .map_err(|e| BridgeError::Serialization(e.to_string()))?;
            state_db.put_cf_raw(CF_RECEIPTS, &key, &receipt_bytes)?;
        }

        // 6. Block hash → number index.
        state_db.put_cf_raw(CF_BLOCK_HASH_TO_NUMBER, block_hash.as_slice(), &height_key)?;

        // 7. Tx hash → location index (height(8) || tx_index(4)).
        for (i, _tx_bytes) in block.evm_transactions.iter().enumerate() {
            if let Some(receipt) = receipts.get(i) {
                let mut location = [0u8; 12];
                location[..8].copy_from_slice(&height_key);
                location[8..12].copy_from_slice(&(i as u32).to_be_bytes());
                state_db.put_cf_raw(
                    CF_TX_HASH_TO_LOCATION,
                    receipt.tx_hash.as_slice(),
                    &location,
                )?;
            }
        }

        Ok(block_hash)
    }
}

/// Apply a `BundleState` to the database: accounts, storage, and contract code.
fn apply_bundle_to_db(state_db: &StateDb, bundle: &BundleState) -> Result<(), BridgeError> {
    for (address, bundle_acct) in &bundle.state {
        match &bundle_acct.info {
            Some(info) => {
                state_db.put_account(address, info)?;
            }
            None => {
                if bundle_acct.original_info.is_some() {
                    state_db.delete_account(address)?;
                }
            }
        }

        for (slot, slot_val) in &bundle_acct.storage {
            state_db.put_storage(address, slot, &slot_val.present_value)?;
        }
    }

    for (code_hash, bytecode) in &bundle.contracts {
        let raw = bytecode.bytes();
        state_db.put_code(code_hash, raw.as_ref())?;
    }

    Ok(())
}
