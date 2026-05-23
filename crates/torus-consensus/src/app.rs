//! `App` trait implementation for hotstuff_rs, wired to the bridge.
//!
//! ## Consensus-then-Execute Architecture
//!
//! `produce_block` and `validate_block` do NOT execute EVM transactions.
//! They only build/validate the raw transaction list for consensus ordering.
//!
//! All execution happens post-commit. This eliminates the entire class of
//! state divergence bugs by construction.
//!
//! ## Execution Pipelining (CTE8)
//!
//! Execution is offloaded to a dedicated background thread via a bounded
//! `SyncSender` channel. `on_committed_block` sends the finalized block to
//! the execution thread and returns immediately, allowing consensus to
//! proceed at full speed (~43ms/block) regardless of execution load.

use sha2::{Digest, Sha256};

use ed25519_dalek::VerifyingKey;
use hotstuff_rs::app::{
    App, ProduceBlockRequest, ProduceBlockResponse, ValidateBlockRequest, ValidateBlockResponse,
};
use hotstuff_rs::hotstuff::types::EquivocationEvidence;
use hotstuff_rs::types::block::Block;
use hotstuff_rs::types::data_types::{CryptoHash, Data, Datum, Power};
use hotstuff_rs::types::update_sets::ValidatorSetUpdates;

use std::sync::Arc;
use std::sync::mpsc::SyncSender;
use std::thread::JoinHandle;
use torus_bridge::{
    sort_native_actions, decode_all_txs, BlockCommitter, BlockProposer, BlockValidator,
    BundleState, NativeExecContext, NativeExecutor,
};
use torus_mempool::Mempool;
use torus_economics::{EpochManager, SlashReason, StakingManager};
use torus_evm::{EvmExecutor, TORUS_CHAIN_ID};
use torus_state::cf::{
    CF_BLOCK_BODIES, CF_BLOCK_HEADERS, CF_CONSENSUS_META, META_NATIVE_APPLIED_HEIGHT,
};
use torus_state::{NativeStateOverlay, StateBackend, StateDb};
use torus_types::{
    Address, ChainConfig, TorusBlock, TorusBlockBody, TorusBlockHeader, ValidatorSet,
};

#[derive(Clone, Debug)]
struct PendingSlash {
    validator: Address,
    fraction_bps: u16,
    reason: SlashReason,
    tombstone: bool,
}

struct CommittedBlockMsg {
    torus_block: TorusBlock,
    pending_slashes: Vec<PendingSlash>,
}

struct ExecutionContext {
    state_db: StateDb,
    validator: BlockValidator,
    evm_executor: EvmExecutor,
    staking: StakingManager,
    epoch_length: u64,
    max_validators: u32,
    treasury_address: Address,
    dev_pool_address: Address,
    metrics: Option<Arc<torus_telemetry::Metrics>>,
}

// ---- Standalone helpers (used by both execution thread and crash recovery) ----

fn read_native_applied_height(state_db: &StateDb) -> Option<u64> {
    state_db
        .get_cf_raw(CF_CONSENSUS_META, META_NATIVE_APPLIED_HEIGHT)
        .ok()
        .flatten()
        .and_then(|data| {
            if data.len() == 8 {
                Some(u64::from_be_bytes(data[..8].try_into().ok()?))
            } else {
                None
            }
        })
}

fn write_native_applied_height(state_db: &StateDb, height: u64) {
    let _ = state_db.put_cf_raw(
        CF_CONSENSUS_META,
        META_NATIVE_APPLIED_HEIGHT,
        &height.to_be_bytes(),
    );
}

fn find_last_committed_height(state_db: &StateDb) -> Option<u64> {
    let db = state_db.inner();
    let cf = db.cf_handle(CF_BLOCK_HEADERS)?;
    let mut iter = db.iterator_cf(cf, rocksdb::IteratorMode::End);
    iter.next()
        .and_then(|r| r.ok())
        .and_then(|(key, _)| {
            if key.len() == 8 {
                Some(u64::from_be_bytes(key[..8].try_into().ok()?))
            } else {
                None
            }
        })
}

fn persist_block_header(state_db: &StateDb, block: &TorusBlock) {
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
    if let Err(e) = state_db.put_cf_raw(
        CF_BLOCK_HEADERS,
        &block.header.height.to_be_bytes(),
        &data,
    ) {
        tracing::error!(%e, height = block.header.height, "failed to persist block header");
    }
}

// ---- Execution pipeline ----

impl ExecutionContext {
    fn execute_committed_block(&self, torus_block: &TorusBlock, pending_slashes: Vec<PendingSlash>) {
        let height = torus_block.header.height;

        if let Some(applied) = read_native_applied_height(&self.state_db) {
            if applied >= height {
                tracing::debug!(height, applied, "execution pipeline: already applied, skipping");
                return;
            }
        }

        for slash in pending_slashes {
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

        let has_evm = !torus_block.evm_transactions.is_empty();
        let has_native = !torus_block.native_actions.is_empty();

        tracing::info!(
            height,
            has_evm,
            has_native,
            evm_tx_count = torus_block.evm_transactions.len(),
            native_count = torus_block.native_actions.len(),
            "execution pipeline: executing finalized block"
        );

        // ---- EVM execution ----
        let mut bundle = BundleState::default();
        let mut computed_fee_revenue: u128 = 0;
        if has_evm {
            match self.validator.validate_block_for_catchup(
                torus_block,
                &self.state_db,
                &self.evm_executor,
            ) {
                Ok(validated) => {
                    computed_fee_revenue = torus_bridge::proposer::compute_fee_revenue(&validated.receipts);
                    if let Err(e) = BlockCommitter::commit_pending_bundle(
                        &self.state_db,
                        &validated.bundle,
                    ) {
                        tracing::error!(%e, height, "failed to commit EVM bundle");
                    }
                    if let Err(e) = BlockCommitter::commit_block_metadata(
                        &self.state_db,
                        torus_block,
                        &validated.receipts,
                    ) {
                        tracing::error!(%e, height, "failed to commit block metadata");
                    }
                    bundle = validated.bundle;
                }
                Err(e) => {
                    tracing::error!(%e, height, "EVM execution failed for committed block");
                }
            }
        } else {
            persist_block_header(&self.state_db, torus_block);
        }

        // ---- Native execution ----
        if has_native || computed_fee_revenue > 0 {
            let invalid_indices = if has_native {
                torus_types::eip712::batch_verify_native_actions(
                    &torus_block.native_actions,
                    torus_block.header.timestamp,
                    |pubkey| self.state_db.get_session(pubkey).ok().flatten(),
                )
            } else {
                vec![]
            };

            if !invalid_indices.is_empty() {
                tracing::error!(
                    count = invalid_indices.len(),
                    proposer = %torus_block.header.proposer,
                    "SLASHING PROPOSER: attested block contained invalid signatures"
                );
                if let Err(e) = self.staking.slash(
                    torus_block.header.proposer,
                    10000,
                    SlashReason::InvalidAttestation,
                    0,
                ) {
                    tracing::error!(%e, "CRITICAL: failed to slash proposer for invalid attestation");
                }
                if let Err(e) = self.staking.tombstone_validator(&torus_block.header.proposer) {
                    tracing::error!(%e, "CRITICAL: failed to tombstone proposer for invalid attestation");
                }
            }

            let mut sender_actions = Vec::with_capacity(torus_block.native_actions.len());
            let mut consumed_nonces = Vec::new();
            for (i, signed) in torus_block.native_actions.iter().enumerate() {
                if invalid_indices.contains(&i) {
                    tracing::error!(index = i, "INVALID SIG in attested block — skipping action");
                    continue;
                }
                match signed.resolve_sender(torus_block.header.timestamp, |pubkey| {
                    self.state_db.get_session(pubkey).ok().flatten()
                }) {
                    Ok(sender) => {
                        consumed_nonces.push((sender, signed.nonce));
                        sender_actions.push((sender, signed.action.clone()));
                    }
                    Err(e) => {
                        tracing::warn!(%e, "failed to recover native action sender, skipping");
                    }
                }
            }

            let overlay = NativeStateOverlay::new(self.state_db.clone());
            overlay.seed_from_bundle(&bundle);

            let (pre_evm, post_evm) = sort_native_actions(&sender_actions);
            let mut ctx = NativeExecContext::new(
                overlay.clone(),
                torus_block.header.height,
                torus_block.header.timestamp,
                torus_block.header.epoch,
                self.epoch_length,
                self.max_validators,
                torus_block.header.proposer,
                self.treasury_address,
                self.dev_pool_address,
            );
            ctx.metrics = self.metrics.clone();

            NativeExecutor::execute_batch(&mut ctx, &pre_evm);
            NativeExecutor::execute_batch(&mut ctx, &post_evm);
            let _ = NativeExecutor::drain_core_writer(&mut ctx);
            NativeExecutor::process_governance(&mut ctx);
            NativeExecutor::distribute_fees(&mut ctx, computed_fee_revenue);
            NativeExecutor::process_epoch_boundary(&mut ctx);
            ctx.save_order_books();

            for (sender, nonce) in &consumed_nonces {
                let mut nonce_key = [0u8; 28];
                nonce_key[..20].copy_from_slice(sender.as_slice());
                nonce_key[20..28].copy_from_slice(&nonce.to_be_bytes());
                let _ = overlay.put_cf_raw(
                    torus_state::cf::CF_NATIVE_NONCES,
                    &nonce_key,
                    &torus_block.header.height.to_be_bytes(),
                );
            }

            if let Err(e) = overlay.flush(&self.state_db) {
                tracing::error!(%e, height, "failed to flush native overlay");
            }
        }

        // ---- Update tracking ----
        write_native_applied_height(&self.state_db, height);

        if let Some(ref m) = self.metrics {
            m.block_height.set(height as i64);
            m.blocks_committed.inc();
            let tx_count = torus_block.header.evm_tx_count as u64
                + torus_block.header.native_action_count as u64;
            m.block_transactions_count.observe(tx_count as f64);
        }

        tracing::info!(height, "execution pipeline: block done");
    }
}

fn execution_loop(rx: std::sync::mpsc::Receiver<CommittedBlockMsg>, ctx: ExecutionContext) {
    tracing::info!("execution pipeline thread started");
    while let Ok(msg) = rx.recv() {
        ctx.execute_committed_block(&msg.torus_block, msg.pending_slashes);
    }
    tracing::info!("execution pipeline thread shutting down");
}

use crate::kv_store::RocksKVStore;

/// Consensus application wired to the execution bridge.
///
/// ## Consensus-then-Execute with Pipelining
///
/// `produce_block`: drains mempool, builds raw tx list, NO execution.
/// `validate_block`: structural + signature checks only, NO execution.
/// `on_committed_block`: sends finalized blocks to the execution pipeline thread.
pub struct TorusApp {
    #[allow(dead_code)]
    state_db: StateDb,
    #[allow(dead_code)]
    proposer: BlockProposer,
    #[allow(dead_code)]
    validator: BlockValidator,
    #[allow(dead_code)]
    evm_executor: EvmExecutor,
    proposer_address: Address,
    last_header: TorusBlockHeader,
    staking: StakingManager,
    epoch_length: u64,
    max_validators: u32,
    last_validator_set: ValidatorSet,
    cached_vs_updates: Option<(u64, Option<ValidatorSetUpdates>)>,
    pending_slashes: Vec<PendingSlash>,
    #[allow(dead_code)]
    treasury_address: Address,
    #[allow(dead_code)]
    dev_pool_address: Address,
    #[allow(dead_code)]
    metrics: Option<Arc<torus_telemetry::Metrics>>,
    mempool: Option<Arc<Mempool>>,
    #[allow(dead_code)]
    signing_key: Option<ed25519_dalek::SigningKey>,
    exec_tx: Option<SyncSender<CommittedBlockMsg>>,
    exec_handle: Option<JoinHandle<()>>,
}

impl TorusApp {
    pub fn new(
        state_db: StateDb,
        config: &ChainConfig,
        metrics: Option<Arc<torus_telemetry::Metrics>>,
        mempool: Option<Arc<Mempool>>,
        signing_key: Option<ed25519_dalek::SigningKey>,
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

        // Execution pipeline context — owns its own copies for thread safety.
        let mut exec_validator = BlockValidator::new(
            config.chain_id,
            config.epoch_length,
            config.max_validators,
            config.treasury_address,
            config.dev_pool_address,
        );
        exec_validator.metrics = metrics.clone();
        let exec_ctx = ExecutionContext {
            state_db: state_db.clone(),
            validator: exec_validator,
            evm_executor: EvmExecutor::new(config.chain_id),
            staking: StakingManager::new(state_db.clone()),
            epoch_length: config.epoch_length,
            max_validators: config.max_validators,
            treasury_address: config.treasury_address,
            dev_pool_address: config.dev_pool_address,
            metrics: metrics.clone(),
        };

        // Crash recovery runs synchronously before spawning the pipeline.
        let last_header = Self::replay_committed(&state_db, &exec_ctx);

        // Spawn execution pipeline: bounded channel (64 blocks) for backpressure.
        let (exec_tx, exec_rx) = std::sync::mpsc::sync_channel(64);
        let exec_handle = std::thread::Builder::new()
            .name("torus-execution".into())
            .spawn(move || execution_loop(exec_rx, exec_ctx))
            .expect("spawn execution pipeline thread");

        Self {
            state_db,
            proposer,
            validator,
            evm_executor: EvmExecutor::new(config.chain_id),
            proposer_address: Address::ZERO,
            last_header,
            staking,
            epoch_length: config.epoch_length,
            max_validators: config.max_validators,
            last_validator_set: genesis_validator_set,
            cached_vs_updates: None,
            pending_slashes: Vec::new(),
            treasury_address: config.treasury_address,
            dev_pool_address: config.dev_pool_address,
            signing_key,
            metrics,
            mempool,
            exec_tx: Some(exec_tx),
            exec_handle: Some(exec_handle),
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
            timeout_base_ms: 500,
        };
        let mut seed = [0u8; 32];
        seed[..8].copy_from_slice(&id.to_le_bytes());
        let signing_key = ed25519_dalek::SigningKey::from_bytes(&seed);
        Self::new(state_db, &config, None, None, Some(signing_key))
    }

    /// Crash recovery: replay committed blocks whose execution was interrupted.
    fn replay_committed(state_db: &StateDb, exec_ctx: &ExecutionContext) -> TorusBlockHeader {
        let mut last_header = torus_bridge::genesis_parent_header();

        let committed = match find_last_committed_height(state_db) {
            Some(h) if h > 0 => h,
            _ => return last_header,
        };

        let applied = read_native_applied_height(state_db).unwrap_or(0);
        if applied >= committed {
            return last_header;
        }

        tracing::warn!(
            committed_height = committed,
            applied_height = applied,
            "crash recovery: execution gap detected, replaying"
        );

        let header: TorusBlockHeader = match state_db
            .get_cf_raw(CF_BLOCK_HEADERS, &committed.to_be_bytes())
        {
            Ok(Some(data)) if data.len() > 32 => match serde_json::from_slice(&data[32..]) {
                Ok(h) => h,
                Err(e) => {
                    tracing::error!(%e, height = committed, "crash recovery: failed to deserialize header");
                    write_native_applied_height(state_db, committed);
                    return last_header;
                }
            },
            _ => {
                tracing::error!(height = committed, "crash recovery: block header not found");
                write_native_applied_height(state_db, committed);
                return last_header;
            }
        };

        last_header = header.clone();

        let body: TorusBlockBody = match state_db
            .get_cf_raw(CF_BLOCK_BODIES, &committed.to_be_bytes())
        {
            Ok(Some(data)) => match serde_json::from_slice(&data) {
                Ok(b) => b,
                Err(e) => {
                    tracing::error!(%e, "crash recovery: failed to deserialize block body");
                    write_native_applied_height(state_db, committed);
                    return last_header;
                }
            },
            _ => {
                tracing::info!(height = committed, "crash recovery: no block body (empty block), marking applied");
                write_native_applied_height(state_db, committed);
                return last_header;
            }
        };

        if body.native_actions.is_empty() && body.evm_transactions.is_empty() {
            tracing::info!(height = committed, "crash recovery: empty block, marking applied");
            write_native_applied_height(state_db, committed);
            return last_header;
        }

        let block = TorusBlock {
            header,
            native_actions: body.native_actions,
            evm_transactions: body.evm_transactions,
            core_writer_actions: body.core_writer_actions,
        };
        exec_ctx.execute_committed_block(&block, vec![]);

        last_header
    }

    fn hash_datum(bytes: &[u8]) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        hasher.finalize().into()
    }

    /// Compute validator set updates at epoch boundary.
    fn epoch_validator_set_updates(&mut self, height: u64) -> Option<ValidatorSetUpdates> {
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

        if let Err(e) = EpochManager::check_minimum_set(&new_set) {
            tracing::error!(%e, "epoch rotation aborted: set too small");
            return None;
        }

        let cap = EpochManager::safe_rotation_cap(self.last_validator_set.validators.len());
        let capped_set = if cap > 0 {
            EpochManager::apply_rotation_cap(&self.last_validator_set, new_set.clone(), cap)
        } else {
            new_set.clone()
        };

        let diff =
            EpochManager::compute_validator_set_diff(&self.last_validator_set, &capped_set);
        if diff.is_empty() {
            self.last_validator_set = capped_set;
            self.cached_vs_updates = Some((height, None));
            return None;
        }

        EpochManager::log_rotation(&self.last_validator_set, &capped_set, &diff, epoch);

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
            if let Ok(Some(val)) = self.staking.get_validator(addr) {
                if let Ok(vk) = VerifyingKey::from_bytes(&val.pubkey) {
                    updates.delete(vk);
                }
            }
        }

        for old_pk in &diff.rotated_out_pubkeys {
            if let Ok(vk) = VerifyingKey::from_bytes(old_pk) {
                updates.delete(vk);
            }
        }

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
                    "new active validator -- ensure peer connectivity for consensus messages"
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
}

impl Drop for TorusApp {
    fn drop(&mut self) {
        self.exec_tx.take();
        if let Some(handle) = self.exec_handle.take() {
            let _ = handle.join();
        }
    }
}

impl App<RocksKVStore> for TorusApp {
    /// Produce a block: drain mempool, build raw tx list, NO execution.
    ///
    /// The state_root is set to the parent's state_root (unchanged until
    /// execution happens on the pipeline thread).
    fn produce_block(
        &mut self,
        request: ProduceBlockRequest<RocksKVStore>,
    ) -> ProduceBlockResponse {
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
        tracing::info!(parent_height = parent_header.height, local_height = self.last_header.height, "produce_block called (CTE)");

        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let (mut native_actions, evm_txs) = if let Some(ref mempool) = self.mempool {
            let gas_limit = if parent_header.evm_gas_limit == 0 {
                torus_evm::DEFAULT_BLOCK_GAS_LIMIT
            } else {
                parent_header.evm_gas_limit
            };
            let (native, evm) = mempool.drain_for_block(4096, gas_limit, parent_header.state_root);
            if !evm.is_empty() || !native.is_empty() {
                tracing::info!(evm_txs = evm.len(), native_actions = native.len(), "drained mempool for block");
            }
            (native, evm)
        } else {
            (vec![], vec![])
        };

        if !native_actions.is_empty() {
            let invalid = torus_types::eip712::batch_verify_native_actions(
                &native_actions,
                timestamp,
                |pubkey| self.state_db.get_session(pubkey).ok().flatten(),
            );
            if !invalid.is_empty() {
                tracing::warn!(count = invalid.len(), "produce_block: dropping actions with invalid signatures");
                for &idx in invalid.iter().rev() {
                    native_actions.remove(idx);
                }
            }
        }

        let sig_attestation = match self.signing_key {
            Some(ref key) => torus_bridge::proposer::generate_sig_attestation(&native_actions, key),
            None => [0u8; 64],
        };

        use torus_types::{Bloom, B256};
        let block = TorusBlock {
            header: TorusBlockHeader {
                height: parent_header.height + 1,
                timestamp,
                proposer: self.proposer_address,
                state_root: parent_header.state_root,
                receipts_root: B256::ZERO,
                logs_bloom: Bloom::ZERO,
                evm_gas_used: 0,
                evm_fee_revenue: 0,
                evm_gas_limit: parent_header.evm_gas_limit,
                native_action_count: native_actions.len() as u32,
                evm_tx_count: evm_txs.len() as u32,
                base_fee_per_gas: parent_header.base_fee_per_gas,
                epoch: parent_header.epoch,
                validator_set_hash: parent_header.validator_set_hash,
                sig_attestation,
            },
            native_actions,
            evm_transactions: evm_txs,
            core_writer_actions: vec![],
        };

        let encoded = serde_json::to_vec(&block).expect("serialize TorusBlock");
        let hash = Self::hash_datum(&encoded);

        let validator_set_updates = self.epoch_validator_set_updates(block.header.height);

        ProduceBlockResponse {
            data_hash: CryptoHash::new(hash),
            data: Data::new(vec![Datum::new(encoded)]),
            app_state_updates: None,
            validator_set_updates,
        }
    }

    /// Validate a proposed block: structural + signature checks only, NO execution.
    fn validate_block(
        &mut self,
        request: ValidateBlockRequest<RocksKVStore>,
    ) -> ValidateBlockResponse {
        tracing::info!("validate_block called (CTE)");

        let block = request.proposed_block();
        let datums = block.data.vec();

        if datums.len() != 1 {
            tracing::warn!(datums_len = datums.len(), "validate_block: REJECTED -- datums.len() != 1");
            return ValidateBlockResponse::Invalid;
        }

        let datum_bytes = datums[0].bytes();

        let computed = Self::hash_datum(datum_bytes);
        if block.data_hash != CryptoHash::new(computed) {
            tracing::warn!(datum_len = datum_bytes.len(), "validate_block: REJECTED -- data_hash mismatch");
            return ValidateBlockResponse::Invalid;
        }

        let torus_block: TorusBlock = match serde_json::from_slice(datum_bytes) {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!(%e, "validate_block: REJECTED -- deserialization failed");
                return ValidateBlockResponse::Invalid;
            }
        };

        tracing::info!(
            height = torus_block.header.height,
            evm_tx_count = torus_block.evm_transactions.len(),
            native_count = torus_block.native_actions.len(),
            "validate_block: structural check passed"
        );

        if !torus_block.evm_transactions.is_empty() {
            if decode_all_txs(&torus_block.evm_transactions).is_err() {
                tracing::warn!("validate_block: REJECTED -- invalid EVM transactions");
                return ValidateBlockResponse::Invalid;
            }
        }

        if !torus_block.native_actions.is_empty() {
            if torus_block.header.sig_attestation == [0u8; 64] {
                use rayon::prelude::*;
                let all_valid = torus_block.native_actions.par_iter().enumerate().all(|(i, sa)| {
                    if sa.recover_sender().is_err() {
                        tracing::warn!(index = i, "validate_block: REJECTED -- invalid native action signature");
                        return false;
                    }
                    true
                });
                if !all_valid {
                    return ValidateBlockResponse::Invalid;
                }
            } else {
                let proposer_addr = torus_block.header.proposer;
                let proposer_pubkey = match self.staking.get_validator(&proposer_addr) {
                    Ok(Some(val)) => match ed25519_dalek::VerifyingKey::from_bytes(&val.pubkey) {
                        Ok(vk) => vk,
                        Err(_) => {
                            tracing::warn!(%proposer_addr, "validate_block: REJECTED -- invalid proposer pubkey");
                            return ValidateBlockResponse::Invalid;
                        }
                    },
                    _ => {
                        tracing::warn!(%proposer_addr, "validate_block: REJECTED -- proposer not in validator set");
                        return ValidateBlockResponse::Invalid;
                    }
                };
                if !torus_bridge::proposer::verify_sig_attestation(
                    &torus_block.native_actions,
                    &torus_block.header.sig_attestation,
                    &proposer_pubkey,
                ) {
                    tracing::warn!(%proposer_addr, "validate_block: REJECTED -- invalid sig attestation");
                    return ValidateBlockResponse::Invalid;
                }
            }
        }

        let validator_set_updates = self.epoch_validator_set_updates(torus_block.header.height);
        ValidateBlockResponse::Valid {
            app_state_updates: None,
            validator_set_updates,
        }
    }

    /// Validate a block during sync: same as validate_block (structural only).
    fn validate_block_for_sync(
        &mut self,
        request: ValidateBlockRequest<RocksKVStore>,
    ) -> ValidateBlockResponse {
        self.validate_block(request)
    }

    /// Send committed block to the execution pipeline thread.
    fn on_committed_block(
        &mut self,
        block: &Block,
        _committed_hash: CryptoHash,
    ) {
        let datums = block.data.vec();
        let Some(datum) = datums.first() else {
            tracing::debug!("on_committed_block: no datums in block");
            return;
        };

        let Ok(torus_block) = serde_json::from_slice::<TorusBlock>(datum.bytes()) else {
            tracing::warn!("on_committed_block: failed to deserialize TorusBlock");
            return;
        };

        let height = torus_block.header.height;
        tracing::info!(
            height,
            evm_txs = torus_block.evm_transactions.len(),
            native = torus_block.native_actions.len(),
            "on_committed_block: sending to execution pipeline"
        );

        if height > self.last_header.height {
            self.last_header = torus_block.header.clone();
        }

        if let Some(ref tx) = self.exec_tx {
            let msg = CommittedBlockMsg {
                torus_block,
                pending_slashes: self.pending_slashes.drain(..).collect(),
            };
            if tx.send(msg).is_err() {
                tracing::error!(height, "execution pipeline channel closed — block will not be executed!");
            }
        }
    }

    /// MonadBFT B3: Handle speculative rollback due to leader equivocation.
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
            "SPECULATIVE ROLLBACK: leader equivocation detected"
        );

        let leader_pubkey = evidence.leader.to_bytes();
        let leader_addr = match self.staking.find_validator_by_pubkey(&leader_pubkey) {
            Ok(Some(val)) => val.address,
            Ok(None) => {
                tracing::error!(
                    leader_pubkey = ?leader_pubkey,
                    "equivocation detected but validator not found -- cannot slash"
                );
                return;
            }
            Err(e) => {
                tracing::error!(%e, "failed to look up validator for slashing");
                return;
            }
        };

        self.pending_slashes.push(PendingSlash {
            validator: leader_addr,
            fraction_bps: 500,
            reason: SlashReason::DoubleSign,
            tombstone: true,
        });
        tracing::info!(
            %leader_addr,
            "equivocation slash buffered (5% + tombstone) -- will apply on next committed block"
        );

        tracing::info!(
            rolled_back_block = ?block,
            "speculative rollback complete (no state to revert in CTE model)"
        );
    }
}

#[cfg(test)]
mod crash_recovery_tests {
    use super::*;
    use torus_types::{Bloom, NativeAction, SignedNativeAction, B256};

    fn make_test_config_and_db() -> (ChainConfig, StateDb) {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(1000);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("torus-crash-test-{}-{}", std::process::id(), id));
        let _ = std::fs::create_dir_all(&dir);
        let state_db = StateDb::open(&dir).expect("open test db");
        let config = ChainConfig {
            chain_id: torus_evm::TORUS_CHAIN_ID,
            chain_name: "crash-test".to_string(),
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
            timeout_base_ms: 500,
        };
        (config, state_db)
    }

    fn make_block(height: u64, native_actions: Vec<SignedNativeAction>) -> TorusBlock {
        TorusBlock {
            header: TorusBlockHeader {
                height,
                timestamp: 1000 + height,
                proposer: Address::ZERO,
                state_root: B256::ZERO,
                receipts_root: B256::ZERO,
                logs_bloom: Bloom::ZERO,
                evm_gas_used: 0,
                evm_fee_revenue: 0,
                evm_gas_limit: 30_000_000,
                native_action_count: native_actions.len() as u32,
                evm_tx_count: 0,
                base_fee_per_gas: 1_000_000_000,
                epoch: 0,
                validator_set_hash: B256::ZERO,
                sig_attestation: [0u8; 64],
            },
            native_actions,
            evm_transactions: vec![],
            core_writer_actions: vec![],
        }
    }

    fn sign_claim_rewards(nonce: u64) -> SignedNativeAction {
        let mut seed = [1u8; 32];
        seed[0] = ((nonce % 254) + 1) as u8;
        let key = k256::ecdsa::SigningKey::from_slice(&seed).unwrap();
        torus_types::eip712::sign_native_action(NativeAction::ClaimRewards, nonce, &key)
    }

    fn persist_block_for_test(state_db: &StateDb, block: &TorusBlock) {
        let block_hash = alloy_primitives::keccak256(&block.header.canonical_header_bytes());
        let header_json = serde_json::to_vec(&block.header).unwrap();
        let mut data = Vec::with_capacity(32 + header_json.len());
        data.extend_from_slice(block_hash.as_slice());
        data.extend_from_slice(&header_json);
        state_db
            .put_cf_raw(CF_BLOCK_HEADERS, &block.header.height.to_be_bytes(), &data)
            .unwrap();

        let body = block.body();
        let body_bytes = serde_json::to_vec(&body).unwrap();
        state_db
            .put_cf_raw(CF_BLOCK_BODIES, &block.header.height.to_be_bytes(), &body_bytes)
            .unwrap();
    }

    #[test]
    fn crash_recovery_replays_committed_block() {
        let (config, state_db) = make_test_config_and_db();
        let block = make_block(1, vec![sign_claim_rewards(100)]);

        persist_block_for_test(&state_db, &block);

        assert!(state_db
            .get_cf_raw(CF_CONSENSUS_META, META_NATIVE_APPLIED_HEIGHT)
            .unwrap()
            .is_none());

        let app = TorusApp::new(state_db.clone(), &config, None, None, None);

        let applied = read_native_applied_height(&state_db);
        assert_eq!(applied, Some(1), "replay should set applied height to 1");
        assert_eq!(app.last_header.height, 1, "last_header should be updated");
    }

    #[test]
    fn no_replay_when_already_applied() {
        let (config, state_db) = make_test_config_and_db();
        let block = make_block(5, vec![sign_claim_rewards(200)]);

        persist_block_for_test(&state_db, &block);

        state_db
            .put_cf_raw(CF_CONSENSUS_META, META_NATIVE_APPLIED_HEIGHT, &5u64.to_be_bytes())
            .unwrap();

        let app = TorusApp::new(state_db.clone(), &config, None, None, None);
        assert_eq!(read_native_applied_height(&state_db), Some(5));
        assert_eq!(app.last_header.height, 0);
    }

    #[test]
    fn replay_empty_block_marks_applied() {
        let (config, state_db) = make_test_config_and_db();
        let block = make_block(3, vec![]);

        let block_hash = alloy_primitives::keccak256(&block.header.canonical_header_bytes());
        let header_json = serde_json::to_vec(&block.header).unwrap();
        let mut data = Vec::with_capacity(32 + header_json.len());
        data.extend_from_slice(block_hash.as_slice());
        data.extend_from_slice(&header_json);
        state_db
            .put_cf_raw(CF_BLOCK_HEADERS, &block.header.height.to_be_bytes(), &data)
            .unwrap();

        let _app = TorusApp::new(state_db.clone(), &config, None, None, None);
        assert_eq!(
            read_native_applied_height(&state_db),
            Some(3),
            "empty block should be marked as applied"
        );
    }
}
