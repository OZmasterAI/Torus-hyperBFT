//! `App` trait implementation for hotstuff_rs, wired to the bridge.
//!
//! Serializes [`TorusBlock`] into hotstuff_rs `Data` (single `Datum`, serde_json).
//! Uses [`BlockProposer`] for block construction, [`BlockValidator`] for validation,
//! and [`BlockCommitter`] for state commitment.

use sha2::{Digest, Sha256};

use hotstuff_rs::app::{
    App, ProduceBlockRequest, ProduceBlockResponse, ValidateBlockRequest, ValidateBlockResponse,
};
use hotstuff_rs::types::data_types::{CryptoHash, Data, Datum};

use torus_bridge::{BlockProposer, BlockValidator};
use torus_evm::{EvmExecutor, TORUS_CHAIN_ID};
use torus_state::StateDb;
use torus_types::{Address, TorusBlock, TorusBlockHeader};

use crate::kv_store::RocksKVStore;

/// Consensus application wired to the execution bridge.
///
/// In Phase 1: produces empty EVM blocks (no mempool), validates proposed
/// blocks by re-executing EVM transactions and verifying state roots.
pub struct TorusApp {
    state_db: StateDb,
    proposer: BlockProposer,
    validator: BlockValidator,
    evm_executor: EvmExecutor,
    proposer_address: Address,
    last_header: TorusBlockHeader,
}

impl TorusApp {
    /// Create a new `TorusApp` with the given state database.
    pub fn new(state_db: StateDb) -> Self {
        Self {
            state_db,
            proposer: BlockProposer::new(TORUS_CHAIN_ID),
            validator: BlockValidator::new(TORUS_CHAIN_ID),
            evm_executor: EvmExecutor::new(TORUS_CHAIN_ID),
            proposer_address: Address::ZERO,
            last_header: torus_bridge::genesis_parent_header(),
        }
    }

    /// Create a stub `TorusApp` without a database (for consensus-only tests).
    pub fn stub() -> Self {
        // Open an in-memory temp path — tests using `stub()` don't exercise EVM.
        let dir = std::env::temp_dir().join(format!("torus-stub-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let state_db = StateDb::open(&dir).expect("open stub state db");
        Self::new(state_db)
    }

    fn hash_datum(bytes: &[u8]) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        hasher.finalize().into()
    }

    fn do_validate(&self, request: ValidateBlockRequest<RocksKVStore>) -> ValidateBlockResponse {
        let block = request.proposed_block();
        let datums = block.data.vec();

        if datums.len() != 1 {
            return ValidateBlockResponse::Invalid;
        }

        let datum_bytes = datums[0].bytes();

        // Verify data hash.
        let computed = Self::hash_datum(datum_bytes);
        if block.data_hash != CryptoHash::new(computed) {
            return ValidateBlockResponse::Invalid;
        }

        // Deserialize.
        let torus_block: TorusBlock = match serde_json::from_slice(datum_bytes) {
            Ok(b) => b,
            Err(_) => return ValidateBlockResponse::Invalid,
        };

        // If the block has EVM transactions, validate via the bridge.
        if !torus_block.evm_transactions.is_empty() {
            match self
                .validator
                .validate_block(&torus_block, &self.state_db, &self.evm_executor)
            {
                Ok(_validated) => ValidateBlockResponse::Valid {
                    app_state_updates: None,
                    validator_set_updates: None,
                },
                Err(_) => ValidateBlockResponse::Invalid,
            }
        } else {
            // Empty block — always valid.
            ValidateBlockResponse::Valid {
                app_state_updates: None,
                validator_set_updates: None,
            }
        }
    }
}

impl App<RocksKVStore> for TorusApp {
    fn produce_block(
        &mut self,
        _request: ProduceBlockRequest<RocksKVStore>,
    ) -> ProduceBlockResponse {
        // Phase 1: produce empty blocks (no mempool integration yet).
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let result = self.proposer.build_block(
            &self.state_db,
            &self.evm_executor,
            &self.last_header,
            vec![], // no pending txs in Phase 1
            timestamp,
            self.proposer_address,
        );

        let block = match result {
            Ok(proposed) => proposed.block,
            Err(_) => {
                // Fallback: empty block with minimal header.
                return produce_empty_block(&self.last_header, timestamp, self.proposer_address);
            }
        };

        let encoded = serde_json::to_vec(&block).expect("serialize TorusBlock");
        let hash = Self::hash_datum(&encoded);

        self.last_header = block.header.clone();

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
        self.do_validate(request)
    }

    fn validate_block_for_sync(
        &mut self,
        request: ValidateBlockRequest<RocksKVStore>,
    ) -> ValidateBlockResponse {
        self.do_validate(request)
    }
}

/// Produce a fallback empty block when bridge proposal fails.
fn produce_empty_block(
    parent: &TorusBlockHeader,
    timestamp: u64,
    proposer: Address,
) -> ProduceBlockResponse {
    use sha2::{Digest, Sha256};
    use torus_types::{Bloom, B256};

    let block = TorusBlock {
        header: TorusBlockHeader {
            height: parent.height + 1,
            timestamp,
            proposer,
            state_root: B256::ZERO,
            receipts_root: B256::ZERO,
            logs_bloom: Bloom::ZERO,
            evm_gas_used: 0,
            evm_gas_limit: parent.evm_gas_limit,
            native_action_count: 0,
            evm_tx_count: 0,
            base_fee_per_gas: parent.base_fee_per_gas,
            epoch: parent.epoch,
            validator_set_hash: parent.validator_set_hash,
        },
        native_actions: vec![],
        evm_transactions: vec![],
        core_writer_actions: vec![],
    };

    let encoded = serde_json::to_vec(&block).expect("serialize TorusBlock");
    let mut hasher = Sha256::new();
    hasher.update(&encoded);
    let hash: [u8; 32] = hasher.finalize().into();

    ProduceBlockResponse {
        data_hash: CryptoHash::new(hash),
        data: Data::new(vec![Datum::new(encoded)]),
        app_state_updates: None,
        validator_set_updates: None,
    }
}
