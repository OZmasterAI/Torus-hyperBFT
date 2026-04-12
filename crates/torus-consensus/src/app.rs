//! Stub `App` trait implementation for hotstuff_rs.
//!
//! Serializes [`TorusBlock`] into hotstuff_rs `Data` (single `Datum`, serde_json).
//! Validation checks data hash integrity and deserializability.

use sha2::{Digest, Sha256};

use hotstuff_rs::app::{
    App, ProduceBlockRequest, ProduceBlockResponse, ValidateBlockRequest, ValidateBlockResponse,
};
use hotstuff_rs::types::data_types::{CryptoHash, Data, Datum};

use torus_types::{Address, Bloom, TorusBlock, TorusBlockHeader, B256};

use crate::kv_store::RocksKVStore;

/// Stub consensus application.
///
/// Produces empty blocks with minimal headers. Validates that proposed blocks
/// contain a single datum that deserializes to a valid [`TorusBlock`] with a
/// correct data hash.
pub struct TorusApp;

impl TorusApp {
    fn hash_datum(bytes: &[u8]) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        hasher.finalize().into()
    }

    fn do_validate(request: ValidateBlockRequest<RocksKVStore>) -> ValidateBlockResponse {
        let block = request.proposed_block();
        let datums = block.data.vec();

        if datums.len() != 1 {
            return ValidateBlockResponse::Invalid;
        }

        let datum_bytes = datums[0].bytes();

        // Verify data hash
        let computed = Self::hash_datum(datum_bytes);
        if block.data_hash != CryptoHash::new(computed) {
            return ValidateBlockResponse::Invalid;
        }

        // Verify the datum is a valid TorusBlock
        match serde_json::from_slice::<TorusBlock>(datum_bytes) {
            Ok(_) => ValidateBlockResponse::Valid {
                app_state_updates: None,
                validator_set_updates: None,
            },
            Err(_) => ValidateBlockResponse::Invalid,
        }
    }
}

impl App<RocksKVStore> for TorusApp {
    fn produce_block(
        &mut self,
        _request: ProduceBlockRequest<RocksKVStore>,
    ) -> ProduceBlockResponse {
        let block = TorusBlock {
            header: TorusBlockHeader {
                height: 0,
                timestamp: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs(),
                proposer: Address::ZERO,
                state_root: B256::ZERO,
                receipts_root: B256::ZERO,
                logs_bloom: Bloom::ZERO,
                evm_gas_used: 0,
                evm_gas_limit: 30_000_000,
                native_action_count: 0,
                evm_tx_count: 0,
                base_fee_per_gas: 0,
                epoch: 0,
                validator_set_hash: B256::ZERO,
            },
            native_actions: vec![],
            evm_transactions: vec![],
            core_writer_actions: vec![],
        };

        let encoded = serde_json::to_vec(&block).expect("serialize TorusBlock");
        let hash = Self::hash_datum(&encoded);

        ProduceBlockResponse {
            data_hash: CryptoHash::new(hash),
            data: Data::new(vec![Datum::new(encoded)]),
            app_state_updates: None,
            validator_set_updates: None,
        }
    }

    fn validate_block(
        &mut self,
        request: ValidateBlockRequest<RocksKVStore>,
    ) -> ValidateBlockResponse {
        Self::do_validate(request)
    }

    fn validate_block_for_sync(
        &mut self,
        request: ValidateBlockRequest<RocksKVStore>,
    ) -> ValidateBlockResponse {
        Self::do_validate(request)
    }
}
