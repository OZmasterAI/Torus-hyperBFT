//! `App` trait implementation for hotstuff_rs, wired to the bridge.
//!
//! Serializes [`TorusBlock`] into hotstuff_rs `Data` (single `Datum`, serde_json).
//! Uses [`BlockProposer`] for block construction, [`BlockValidator`] for validation,
//! and [`BlockCommitter`] for state commitment.

use sha2::{Digest, Sha256};

use ed25519_dalek::VerifyingKey;
use hotstuff_rs::app::{
    App, ProduceBlockRequest, ProduceBlockResponse, ValidateBlockRequest, ValidateBlockResponse,
};
use hotstuff_rs::types::data_types::{CryptoHash, Data, Datum, Power};
use hotstuff_rs::types::update_sets::ValidatorSetUpdates;

use torus_bridge::{BlockProposer, BlockValidator};
use torus_economics::{EpochManager, StakingManager};
use torus_evm::{EvmExecutor, TORUS_CHAIN_ID};
use torus_state::StateDb;
use torus_types::{Address, ChainConfig, TorusBlock, TorusBlockHeader, ValidatorSet};

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
    staking: StakingManager,
    epoch_length: u64,
    max_validators: u32,
    last_validator_set: ValidatorSet,
}

impl TorusApp {
    /// Create a new `TorusApp` with the given state database and chain config.
    pub fn new(state_db: StateDb, config: &ChainConfig) -> Self {
        let staking = StakingManager::new(state_db.clone());
        Self {
            state_db,
            proposer: BlockProposer::new(TORUS_CHAIN_ID),
            validator: BlockValidator::new(TORUS_CHAIN_ID),
            evm_executor: EvmExecutor::new(TORUS_CHAIN_ID),
            proposer_address: Address::ZERO,
            last_header: torus_bridge::genesis_parent_header(),
            staking,
            epoch_length: config.epoch_length,
            max_validators: config.max_validators,
            last_validator_set: ValidatorSet {
                validators: vec![],
                epoch: 0,
            },
        }
    }

    /// Create a stub `TorusApp` without a database (for consensus-only tests).
    pub fn stub() -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("torus-stub-{}-{}", std::process::id(), id));
        let _ = std::fs::create_dir_all(&dir);
        let state_db = StateDb::open(&dir).expect("open stub state db");
        let config = ChainConfig {
            chain_id: TORUS_CHAIN_ID,
            chain_name: "torus-test".to_string(),
            evm_gas_limit: 30_000_000,
            base_fee_per_gas: 1_000_000_000,
            epoch_length: 100,
            max_validators: 4,
            min_stake: torus_economics::MIN_SELF_DELEGATION,
            fee_burn_bps: 1000,
            fee_validator_bps: 0,
            fee_treasury_bps: 4500,
            fee_dev_pool_bps: 4500,
        };
        Self::new(state_db, &config)
    }

    /// Compute validator set updates at epoch boundary.
    ///
    /// Phase 3 (3.2): applies pending key rotations, enforces rotation cap
    /// derived from BFT safety requirements, and checks minimum set size.
    fn epoch_validator_set_updates(&mut self, height: u64) -> Option<ValidatorSetUpdates> {
        if !EpochManager::is_epoch_boundary(height, self.epoch_length) {
            return None;
        }

        let epoch = EpochManager::epoch_for_block(height, self.epoch_length);

        // Phase 3 (3.2.4): Apply pending key rotations BEFORE computing new set
        if let Err(e) = self.staking.apply_pending_rotations(epoch) {
            tracing::error!(%e, "failed to apply pending key rotations");
        }

        let new_set = match EpochManager::compute_new_validator_set(
            &self.staking,
            self.max_validators,
            epoch,
        ) {
            Ok(set) => set,
            Err(e) => {
                tracing::error!(%e, "failed to compute validator set at epoch boundary");
                return None;
            }
        };

        // Phase 3 (3.2.4): Check minimum set size
        if let Err(e) = EpochManager::check_minimum_set(&new_set) {
            tracing::error!(%e, "epoch rotation aborted: set too small");
            return None;
        }

        // Phase 3 (3.2.4): Apply rotation cap derived from BFT safety
        // hotstuff_rs quorum = (2/3 * total_power) + 1, so max floor(n/3)
        // validators can change per epoch to maintain quorum overlap.
        let cap = EpochManager::safe_rotation_cap(self.last_validator_set.validators.len());
        let capped_set = if cap > 0 {
            EpochManager::apply_rotation_cap(&self.last_validator_set, new_set.clone(), cap)
        } else {
            // First epoch or empty set — no cap needed
            new_set.clone()
        };

        let diff =
            EpochManager::compute_validator_set_diff(&self.last_validator_set, &capped_set);
        if diff.is_empty() {
            self.last_validator_set = capped_set;
            return None;
        }

        // Phase 3 (3.2.2): Log detailed rotation events
        EpochManager::log_rotation(&self.last_validator_set, &capped_set, &diff, epoch);

        // Update validator statuses in state.
        if let Err(e) = EpochManager::update_validator_statuses(&self.staking, &capped_set) {
            tracing::error!(%e, "failed to update validator statuses");
        }

        let mut updates = ValidatorSetUpdates::new();

        for v in &diff.inserts {
            if let Ok(vk) = VerifyingKey::from_bytes(&v.pubkey.0) {
                updates.insert(vk, Power::new(v.power));
            }
        }

        for addr in &diff.deletes {
            // Look up the pubkey for the deleted validator.
            if let Ok(Some(val)) = self.staking.get_validator(addr) {
                if let Ok(vk) = VerifyingKey::from_bytes(&val.pubkey) {
                    updates.delete(vk);
                }
            }
        }

        // BUG FIX (3.2): Handle key rotations — delete old pubkeys
        for old_pk in &diff.rotated_out_pubkeys {
            if let Ok(vk) = VerifyingKey::from_bytes(old_pk) {
                updates.delete(vk);
            }
        }

        // Phase 3 (3.2.4): Warn if new validators are not known peers
        for v in &diff.inserts {
            let is_new = !self
                .last_validator_set
                .validators
                .iter()
                .any(|old| old.address == v.address);
            if is_new {
                tracing::warn!(
                    epoch,
                    validator = %v.address,
                    "new active validator — ensure peer connectivity for consensus messages"
                );
            }
        }

        tracing::info!(
            epoch,
            inserts = diff.inserts.len(),
            deletes = diff.deletes.len(),
            key_rotations = diff.rotated_out_pubkeys.len(),
            active_set_size = capped_set.validators.len(),
            "epoch boundary: validator set updated"
        );
        self.last_validator_set = capped_set;
        Some(updates)
    }

    fn hash_datum(bytes: &[u8]) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        hasher.finalize().into()
    }

    fn do_validate(
        &mut self,
        request: ValidateBlockRequest<RocksKVStore>,
    ) -> ValidateBlockResponse {
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
                Ok(_validated) => {
                    let validator_set_updates =
                        self.epoch_validator_set_updates(torus_block.header.height);
                    ValidateBlockResponse::Valid {
                        app_state_updates: None,
                        validator_set_updates,
                    }
                }
                Err(_) => ValidateBlockResponse::Invalid,
            }
        } else {
            // Empty block — check epoch boundary.
            let validator_set_updates = self.epoch_validator_set_updates(torus_block.header.height);
            ValidateBlockResponse::Valid {
                app_state_updates: None,
                validator_set_updates,
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

        // Check for epoch boundary and compute validator set updates.
        let validator_set_updates = self.epoch_validator_set_updates(block.header.height);

        ProduceBlockResponse {
            data_hash: CryptoHash::new(hash),
            data: Data::new(vec![Datum::new(encoded)]),
            app_state_updates: None,
            validator_set_updates,
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
