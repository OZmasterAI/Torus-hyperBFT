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
use hotstuff_rs::hotstuff::types::EquivocationEvidence;
use hotstuff_rs::types::data_types::{CryptoHash, Data, Datum, Power};
use hotstuff_rs::types::update_sets::ValidatorSetUpdates;

use std::sync::Arc;
use torus_bridge::{
    sort_native_actions, BlockCommitter, BlockProposer, BlockValidator, BridgeError,
    NativeExecContext, NativeExecutor,
};
use torus_mempool::Mempool;
use torus_economics::{EpochManager, SlashReason, StakingManager};
use torus_evm::{EvmExecutor, TORUS_CHAIN_ID};
use torus_state::cf::CF_BLOCK_HEADERS;
use torus_state::StateDb;
use torus_types::{Address, ChainConfig, NativeAction, TorusBlock, TorusBlockHeader, ValidatorSet};

/// FIX CONS-PF-08: Buffered slash intent recorded during speculative rollback.
/// Applied to DB only when the next block is produced/validated (i.e., the chain
/// has moved forward past the equivocation). Discarded if the app is reconstructed
/// from DB state (meaning the entire speculative chain was abandoned).
#[derive(Clone, Debug)]
struct PendingSlash {
    validator: Address,
    fraction_bps: u16,
    reason: SlashReason,
    tombstone: bool,
}

use crate::kv_store::RocksKVStore;

/// Consensus application wired to the execution bridge.
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
    cached_vs_updates: Option<(u64, Option<ValidatorSetUpdates>)>,
    pending_slashes: Vec<PendingSlash>,
    treasury_address: Address,
    dev_pool_address: Address,
    metrics: Option<Arc<torus_telemetry::Metrics>>,
    mempool: Option<Arc<Mempool>>,
}

impl TorusApp {
    pub fn new(
        state_db: StateDb,
        config: &ChainConfig,
        metrics: Option<Arc<torus_telemetry::Metrics>>,
        mempool: Option<Arc<Mempool>>,
    ) -> Self {
        let staking = StakingManager::new(state_db.clone());
        let mut proposer = BlockProposer::new(
            config.chain_id,
            config.epoch_length,
            config.max_validators,
            config.treasury_address,
            config.dev_pool_address,
        );
        proposer.metrics = metrics.clone();
        let mut validator = BlockValidator::new(
            config.chain_id,
            config.epoch_length,
            config.max_validators,
            config.treasury_address,
            config.dev_pool_address,
        );
        validator.metrics = metrics.clone();
        let genesis_validator_set = EpochManager::compute_new_validator_set(
            &staking, config.max_validators, 0,
        ).unwrap_or_else(|_| ValidatorSet { validators: vec![], epoch: 0 });
        Self {
            state_db,
            proposer,
            validator,
            evm_executor: EvmExecutor::new(config.chain_id),
            proposer_address: Address::ZERO,
            last_header: torus_bridge::genesis_parent_header(),
            staking,
            epoch_length: config.epoch_length,
            max_validators: config.max_validators,
            last_validator_set: genesis_validator_set,
            cached_vs_updates: None,
            pending_slashes: Vec::new(),
            treasury_address: config.treasury_address,
            dev_pool_address: config.dev_pool_address,
            metrics,
            mempool,
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
            treasury_address: Address::ZERO,
            dev_pool_address: Address::ZERO,
        };
        Self::new(state_db, &config, None, None)
    }

    /// FIX CONS-PF-08: Flush any buffered slash intents to DB.
    /// Called at the start of produce_block and do_validate — by the time the
    /// chain asks us to build or validate the next block, the equivocation
    /// evidence is no longer speculative.
    fn flush_pending_slashes(&mut self) {
        for slash in self.pending_slashes.drain(..) {
            match self.staking.slash(
                slash.validator,
                slash.fraction_bps,
                slash.reason.clone(),
                0,
            ) {
                Ok(amount) => {
                    tracing::info!(
                        %slash.validator,
                        %amount,
                        reason = ?slash.reason,
                        "flushed buffered slash to DB"
                    );
                }
                Err(e) => {
                    tracing::error!(
                        %slash.validator,
                        %e,
                        "CRITICAL: failed to flush buffered slash"
                    );
                }
            }
            if slash.tombstone {
                if let Err(e) = self.staking.tombstone_validator(&slash.validator) {
                    tracing::error!(
                        %slash.validator,
                        %e,
                        "CRITICAL: failed to tombstone equivocating leader"
                    );
                }
            }
        }
    }

    /// Compute validator set updates at epoch boundary.
    ///
    /// Phase 3 (3.2): applies pending key rotations, enforces rotation cap
    /// derived from BFT safety requirements, and checks minimum set size.
    fn epoch_validator_set_updates(&mut self, height: u64) -> Option<ValidatorSetUpdates> {
        // FIX CONS-FIND-02: Return cached result if already computed for this height.
        if let Some((cached_height, ref cached_result)) = self.cached_vs_updates {
            if cached_height == height {
                return cached_result.clone();
            }
        }

        if !EpochManager::is_epoch_boundary(height, self.epoch_length) {
            self.cached_vs_updates = Some((height, None));
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
            self.cached_vs_updates = Some((height, None));
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
        self.cached_vs_updates = Some((height, Some(updates.clone())));
        Some(updates)
    }

    fn hash_datum(bytes: &[u8]) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        hasher.finalize().into()
    }

    fn persist_block_header(&self, block: &TorusBlock) {
        let block_hash = alloy_primitives::keccak256(&block.header.canonical_header_bytes());
        let header_json = match serde_json::to_vec(&block.header) {
            Ok(j) => j,
            Err(e) => {
                tracing::error!(%e, "failed to serialize block header");
                return;
            }
        };
        let mut data = Vec::with_capacity(32 + header_json.len());
        data.extend_from_slice(block_hash.as_slice());
        data.extend_from_slice(&header_json);
        if let Err(e) = self.state_db.put_cf_raw(
            CF_BLOCK_HEADERS,
            &block.header.height.to_be_bytes(),
            &data,
        ) {
            tracing::error!(%e, height = block.header.height, "failed to persist block header");
        }
    }

    fn execute_native_post_commit(
        &self,
        block: &TorusBlock,
        sender_actions: Vec<(Address, NativeAction)>,
        consumed_nonces: Vec<(Address, u64)>,
    ) -> Result<(), BridgeError> {
        let (pre_evm, post_evm) = sort_native_actions(&sender_actions);
        let mut ctx = NativeExecContext::new(
            self.state_db.clone(),
            block.header.height,
            block.header.timestamp,
            block.header.epoch,
            self.epoch_length,
            self.max_validators,
            block.header.proposer,
            self.treasury_address,
            self.dev_pool_address,
        );
        ctx.metrics = self.metrics.clone();
        NativeExecutor::execute_batch(&mut ctx, &pre_evm);
        NativeExecutor::execute_batch(&mut ctx, &post_evm);
        NativeExecutor::drain_core_writer(&mut ctx)?;
        NativeExecutor::process_governance(&mut ctx);
        NativeExecutor::distribute_fees(&mut ctx, block.header.evm_gas_used);
        NativeExecutor::process_epoch_boundary(&mut ctx);
        ctx.save_order_books();
        for (sender, nonce) in &consumed_nonces {
            let mut nonce_key = [0u8; 28];
            nonce_key[..20].copy_from_slice(sender.as_slice());
            nonce_key[20..28].copy_from_slice(&nonce.to_be_bytes());
            let _ = self.state_db.put_cf_raw(
                torus_state::cf::CF_NATIVE_NONCES,
                &nonce_key,
                &block.header.height.to_be_bytes(),
            );
        }
        Ok(())
    }

    fn do_validate(
        &mut self,
        request: ValidateBlockRequest<RocksKVStore>,
    ) -> ValidateBlockResponse {
        tracing::info!("do_validate called");
        self.flush_pending_slashes();

        let block = request.proposed_block();
        let datums = block.data.vec();

        tracing::info!(datums_len = datums.len(), block_height = block.height.int(), "do_validate: datum count");

        if datums.len() != 1 {
            tracing::warn!(datums_len = datums.len(), "do_validate: REJECTED — datums.len() != 1");
            return ValidateBlockResponse::Invalid;
        }

        let datum_bytes = datums[0].bytes();

        // Verify data hash.
        let computed = Self::hash_datum(datum_bytes);
        if block.data_hash != CryptoHash::new(computed) {
            tracing::warn!(datum_len = datum_bytes.len(), "do_validate: REJECTED — data_hash mismatch");
            return ValidateBlockResponse::Invalid;
        }

        // Deserialize.
        let torus_block: TorusBlock = match serde_json::from_slice(datum_bytes) {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!(%e, "do_validate: REJECTED — deserialization failed");
                return ValidateBlockResponse::Invalid;
            }
        };

        let has_native = !torus_block.native_actions.is_empty();
        let has_evm = !torus_block.evm_transactions.is_empty();
        tracing::info!(
            height = torus_block.header.height,
            has_native,
            has_evm,
            evm_tx_count = torus_block.evm_transactions.len(),
            native_count = torus_block.native_actions.len(),
            "do_validate: block contents"
        );

        let validation_result = if has_native {
            self.validator
                .validate_block_with_native(&torus_block, &self.state_db, &self.evm_executor)
        } else if has_evm {
            self.validator
                .validate_block(&torus_block, &self.state_db, &self.evm_executor)
        } else {
            // Empty block — no execution to validate.
            self.persist_block_header(&torus_block);
            if torus_block.header.height > self.last_header.height {
                self.last_header = torus_block.header.clone();
            }
            let validator_set_updates = self.epoch_validator_set_updates(torus_block.header.height);
            return ValidateBlockResponse::Valid {
                app_state_updates: None,
                validator_set_updates,
            };
        };

        match validation_result {
            Ok(validated) => {
                let commit_result = BlockCommitter::commit_block(
                    &self.state_db,
                    &torus_block,
                    &validated.bundle,
                    &validated.receipts,
                );
                match commit_result {
                    Ok(block_hash) => {
                        tracing::info!(
                            height = torus_block.header.height,
                            %block_hash,
                            evm_txs = torus_block.evm_transactions.len(),
                            "committed block state to DB"
                        );
                        if has_native {
                            if let Err(e) = self.execute_native_post_commit(
                                &torus_block,
                                validated.native_sender_actions,
                                validated.native_consumed_nonces,
                            ) {
                                tracing::error!(%e, "native post-commit execution failed");
                            }
                        }
                    }
                    Err(e) => {
                        tracing::error!(%e, "failed to commit block state");
                        return ValidateBlockResponse::Invalid;
                    }
                }

                if torus_block.header.height > self.last_header.height {
                    self.last_header = torus_block.header.clone();
                }
                if let Some(ref m) = self.metrics {
                    m.block_height.set(torus_block.header.height as i64);
                    m.blocks_committed.inc();
                    let tx_count = torus_block.header.evm_tx_count as u64
                        + torus_block.header.native_action_count as u64;
                    m.block_transactions_count.observe(tx_count as f64);
                }
                let validator_set_updates =
                    self.epoch_validator_set_updates(torus_block.header.height);
                ValidateBlockResponse::Valid {
                    app_state_updates: None,
                    validator_set_updates,
                }
            }
            Err(e) => {
                tracing::warn!(%e, "block validation failed");
                ValidateBlockResponse::Invalid
            }
        }
    }
}

impl App<RocksKVStore> for TorusApp {
    fn produce_block(
        &mut self,
        request: ProduceBlockRequest<RocksKVStore>,
    ) -> ProduceBlockResponse {
        // Derive the correct parent header from the block tree to avoid desync.
        let parent_header = if let Some(parent_hash) = request.parent_block() {
            if let Ok(Some(parent_block)) = request.block_tree().block(&parent_hash) {
                let datums = parent_block.data.vec();
                datums.first()
                    .and_then(|d| serde_json::from_slice::<TorusBlock>(d.bytes()).ok())
                    .map(|b| b.header)
                    .unwrap_or_else(|| self.last_header.clone())
            } else {
                self.last_header.clone()
            }
        } else {
            self.last_header.clone()
        };
        tracing::info!(parent_height = parent_header.height, local_height = self.last_header.height, "produce_block called");
        self.flush_pending_slashes();

        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let (native_actions, evm_txs) = if let Some(ref mempool) = self.mempool {
            let gas_limit = if parent_header.evm_gas_limit == 0 {
                torus_evm::DEFAULT_BLOCK_GAS_LIMIT
            } else {
                parent_header.evm_gas_limit
            };
            let (native, evm) = mempool.drain_for_block(256, gas_limit, parent_header.state_root);
            if !evm.is_empty() || !native.is_empty() {
                tracing::info!(evm_txs = evm.len(), native_actions = native.len(), "drained mempool for block");
            }
            (native, evm)
        } else {
            (vec![], vec![])
        };

        let evm_txs_backup = evm_txs.clone();
        let native_actions_backup = native_actions.clone();

        let result = if native_actions.is_empty() {
            self.proposer.build_block(
                &self.state_db,
                &self.evm_executor,
                &parent_header,
                evm_txs,
                timestamp,
                self.proposer_address,
            )
        } else {
            self.proposer.build_block_with_native(
                &self.state_db,
                &self.evm_executor,
                &parent_header,
                native_actions,
                evm_txs,
                timestamp,
                self.proposer_address,
            )
        };

        let block = match result {
            Ok(proposed) => proposed.block,
            Err(e) => {
                tracing::error!(%e, "block proposal FAILED — falling back to empty block");
                if let Some(ref mempool) = self.mempool {
                    if !evm_txs_backup.is_empty() {
                        tracing::warn!(count = evm_txs_backup.len(), "re-inserting drained EVM txs after proposal failure");
                        mempool.reinsert_evm(evm_txs_backup);
                    }
                    if !native_actions_backup.is_empty() {
                        tracing::warn!(count = native_actions_backup.len(), "re-inserting drained native actions after proposal failure");
                        mempool.reinsert_native(native_actions_backup);
                    }
                }
                return produce_empty_block(&parent_header, timestamp, self.proposer_address, &self.state_db);
            }
        };

        let encoded = serde_json::to_vec(&block).expect("serialize TorusBlock");
        let hash = Self::hash_datum(&encoded);

        // FIX CONS-PF-05: Don't update last_header during produce_block (before consensus).
        // It will be updated in do_validate when the block passes consensus validation.

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

    /// MonadBFT B3: Handle speculative rollback due to leader equivocation.
    ///
    /// Reverts any state changes from the rolled-back block and triggers
    /// slashing of the equivocating leader (5% + tombstone).
    fn on_speculative_rollback(
        &mut self,
        block: hotstuff_rs::types::data_types::CryptoHash,
        evidence: &EquivocationEvidence,
    ) {
        tracing::warn!(
            view = %evidence.view.int(),
            leader = ?evidence.leader.to_bytes(),
            block_a = ?evidence.block_a,
            block_b = ?evidence.block_b,
            "SPECULATIVE ROLLBACK: leader equivocation detected, reverting block"
        );

        // Phase 1: No EVM state changes to revert (empty blocks).
        // When EVM execution is added, the StateOverlay approach will handle this:
        // the overlay is simply discarded instead of committed to RocksDB.

        // FIX CONS-FIND-01: Look up the validator's Ethereum address using their
        // consensus public key. The previous code derived SHA-256(pubkey)[12..32]
        // which never matched any registered Ethereum address, so all slashes failed.
        let leader_pubkey = evidence.leader.to_bytes();
        let leader_addr = match self.staking.find_validator_by_pubkey(&leader_pubkey) {
            Ok(Some(val)) => val.address,
            Ok(None) => {
                tracing::error!(
                    leader_pubkey = ?leader_pubkey,
                    "equivocation detected but validator not found by consensus pubkey — cannot slash"
                );
                return;
            }
            Err(e) => {
                tracing::error!(%e, "failed to look up validator for slashing");
                return;
            }
        };

        // FIX CONS-PF-08: Buffer slash intent instead of writing to DB during
        // speculative execution. The buffer is flushed at the start of the next
        // produce_block or do_validate call, ensuring the slash only persists
        // once the chain has moved forward past the equivocation event.
        self.pending_slashes.push(PendingSlash {
            validator: leader_addr,
            fraction_bps: 500, // 5%
            reason: SlashReason::DoubleSign,
            tombstone: true,
        });
        tracing::info!(
            %leader_addr,
            "equivocation slash buffered (5% + tombstone) — will apply on next block"
        );

        tracing::info!(
            rolled_back_block = ?block,
            "speculative rollback complete — chain continues from pre-rollback state"
        );
    }
}

/// Produce a fallback empty block when bridge proposal fails.
/// FIX CONS-PF-01: Compute real state root even for empty blocks.
fn produce_empty_block(
    parent: &TorusBlockHeader,
    timestamp: u64,
    proposer: Address,
    state_db: &StateDb,
) -> ProduceBlockResponse {
    use sha2::{Digest, Sha256};
    use torus_types::{Bloom, B256};

    // FIX CONS-PF-01: Use parent's state_root for empty blocks (no state change).
    // An empty block doesn't modify state, so the state root carries forward.
    let state_root = parent.state_root;

    let block = TorusBlock {
        header: TorusBlockHeader {
            height: parent.height + 1,
            timestamp,
            proposer,
            state_root,
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
