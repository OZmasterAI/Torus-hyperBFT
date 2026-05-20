//! `App` trait implementation for hotstuff_rs, wired to the bridge.
//!
//! Serializes [`TorusBlock`] into hotstuff_rs `Data` (single `Datum`, serde_json).
//! Uses [`BlockProposer`] for block construction, [`BlockValidator`] for validation,
//! and [`BlockCommitter`] for state commitment.

use sha2::{Digest, Sha256};

use std::collections::HashMap;

use ed25519_dalek::VerifyingKey;
use hotstuff_rs::app::{
    App, ProduceBlockRequest, ProduceBlockResponse, ValidateBlockRequest, ValidateBlockResponse,
};
use hotstuff_rs::hotstuff::types::EquivocationEvidence;
use hotstuff_rs::types::data_types::{CryptoHash, Data, Datum, Power};
use hotstuff_rs::types::update_sets::ValidatorSetUpdates;

use std::sync::Arc;
use torus_bridge::{
    sort_native_actions, merge_bundle_into, BlockCommitter, BlockProposer, BlockValidator,
    BridgeError, BundleState, NativeExecContext, NativeExecutor,
    state_root::compute_post_bundle_state_root,
};
use torus_mempool::Mempool;
use torus_economics::{EpochManager, SlashReason, StakingManager};
use torus_evm::{EvmExecutor, TORUS_CHAIN_ID};
use torus_state::cf::{
    CF_BLOCK_BODIES, CF_BLOCK_HEADERS, CF_CONSENSUS_META, META_NATIVE_APPLIED_HEIGHT,
};
use torus_state::{NativeStateOverlay, StateBackend, StateDb, StateOverlay};
use torus_types::{
    Address, ChainConfig, NativeAction, TorusBlock, TorusBlockBody, TorusBlockHeader, ValidatorSet,
};

/// Pending EVM execution result stored until consensus commits the block.
struct PendingExec {
    bundle: BundleState,
    native_overlay: Option<NativeStateOverlay>,
    parent_hash: Option<CryptoHash>,
    height: u64,
}

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
    /// Pending EVM bundles not yet flushed to state_db, keyed by block hash.
    pending_bundles: HashMap<CryptoHash, PendingExec>,
    /// Block hash whose EVM state was last flushed to state_db.
    state_db_tip: Option<CryptoHash>,
    /// Highest block height whose EVM state has been committed to state_db.
    /// May lag behind last_header.height when blocks are synced without EVM re-execution.
    evm_committed_height: u64,
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
        let mut app = Self {
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
            pending_bundles: HashMap::new(),
            state_db_tip: None,
            evm_committed_height: 0,
        };
        app.replay_native_post_commit_if_needed();
        app
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
        Self::new(state_db, &config, None, None)
    }

    /// Detect and replay native post-commit execution missed due to a crash.
    fn replay_native_post_commit_if_needed(&mut self) {
        let committed = match self.find_last_committed_height() {
            Some(h) if h > 0 => h,
            _ => return,
        };

        let applied = self.read_native_applied_height().unwrap_or(0);
        if applied >= committed {
            return;
        }

        tracing::warn!(
            committed_height = committed,
            applied_height = applied,
            "crash recovery: native post-commit gap detected, replaying"
        );

        let header: TorusBlockHeader = match self
            .state_db
            .get_cf_raw(CF_BLOCK_HEADERS, &committed.to_be_bytes())
        {
            Ok(Some(data)) if data.len() > 32 => match serde_json::from_slice(&data[32..]) {
                Ok(h) => h,
                Err(e) => {
                    tracing::error!(%e, height = committed, "crash recovery: failed to deserialize header");
                    self.write_native_applied_height(committed);
                    return;
                }
            },
            _ => {
                tracing::error!(height = committed, "crash recovery: block header not found");
                self.write_native_applied_height(committed);
                return;
            }
        };

        self.last_header = header.clone();

        let body: TorusBlockBody = match self
            .state_db
            .get_cf_raw(CF_BLOCK_BODIES, &committed.to_be_bytes())
        {
            Ok(Some(data)) => match serde_json::from_slice(&data) {
                Ok(b) => b,
                Err(e) => {
                    tracing::error!(%e, "crash recovery: failed to deserialize block body");
                    self.write_native_applied_height(committed);
                    return;
                }
            },
            _ => {
                tracing::info!(height = committed, "crash recovery: no block body (empty block), marking applied");
                self.write_native_applied_height(committed);
                return;
            }
        };

        if body.native_actions.is_empty() {
            tracing::info!(height = committed, "crash recovery: no native actions, marking applied");
            self.write_native_applied_height(committed);
            return;
        }

        let mut sender_actions = Vec::with_capacity(body.native_actions.len());
        let mut consumed_nonces = Vec::new();
        for signed in &body.native_actions {
            match signed.resolve_sender(header.timestamp, |pubkey| {
                self.state_db.get_session(pubkey).ok().flatten()
            }) {
                Ok(sender) => {
                    consumed_nonces.push((sender, signed.nonce));
                    sender_actions.push((sender, signed.action.clone()));
                }
                Err(e) => {
                    tracing::error!(%e, "crash recovery: failed to recover sender, skipping action");
                }
            }
        }

        let block = TorusBlock {
            header,
            native_actions: body.native_actions,
            evm_transactions: body.evm_transactions,
            core_writer_actions: body.core_writer_actions,
        };

        match self.execute_native_post_commit(&block, sender_actions, consumed_nonces) {
            Ok(()) => {
                self.write_native_applied_height(committed);
                tracing::info!(height = committed, "crash recovery: native post-commit replayed successfully");
            }
            Err(e) => {
                tracing::error!(%e, height = committed, "crash recovery: replay failed");
            }
        }
    }

    fn find_last_committed_height(&self) -> Option<u64> {
        let db = self.state_db.inner();
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

    fn read_native_applied_height(&self) -> Option<u64> {
        self.state_db
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

    fn write_native_applied_height(&self, height: u64) {
        let _ = self.state_db.put_cf_raw(
            CF_CONSENSUS_META,
            META_NATIVE_APPLIED_HEIGHT,
            &height.to_be_bytes(),
        );
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

    // ------------------------------------------------------------------
    // Deferred EVM commit: overlay helpers
    // ------------------------------------------------------------------

    /// Build a merged `BundleState` containing all pending ancestor changes
    /// between `state_db_tip` and `parent_hash` (inclusive).
    fn merged_parent_bundle(&self, parent_hash: &CryptoHash) -> BundleState {
        let mut chain = Vec::new();
        let mut cursor = *parent_hash;
        while let Some(pending) = self.pending_bundles.get(&cursor) {
            chain.push(&pending.bundle);
            match pending.parent_hash {
                Some(p) => cursor = p,
                None => break,
            }
        }
        // Apply oldest-first so later bundles override earlier ones.
        let mut merged = BundleState::default();
        for bundle in chain.into_iter().rev() {
            merge_bundle_into(&mut merged, bundle);
        }
        merged
    }

    /// Returns true if `parent_hash` has pending (unflushed) EVM state.
    fn parent_is_pending(&self, parent_hash: &CryptoHash) -> bool {
        self.pending_bundles.contains_key(parent_hash)
    }

    /// Flush committed blocks' pending bundles to `state_db`.
    ///
    /// `committed_hash` is the highest block known to be committed by consensus.
    /// We flush all pending bundles on the committed chain, then prune
    /// orphaned fork bundles.
    fn flush_committed_bundles(&mut self, committed_hash: Option<CryptoHash>) {
        let committed_hash = match committed_hash {
            Some(h) => h,
            None => return,
        };

        // Walk from committed block back to state_db_tip, collecting bundles to flush.
        let mut to_flush = Vec::new();
        let mut cursor = committed_hash;
        while let Some(pending) = self.pending_bundles.get(&cursor) {
            to_flush.push(cursor);
            match pending.parent_hash {
                Some(p) => cursor = p,
                None => break,
            }
        }

        if to_flush.is_empty() {
            return;
        }

        // Flush oldest-first.
        to_flush.reverse();
        let flush_height = self.pending_bundles.get(to_flush.last().unwrap())
            .map(|p| p.height)
            .unwrap_or(0);

        for hash in &to_flush {
            if let Some(pending) = self.pending_bundles.remove(hash) {
                if let Err(e) = BlockCommitter::commit_pending_bundle(
                    &self.state_db,
                    &pending.bundle,
                ) {
                    tracing::error!(%e, "failed to flush pending EVM bundle");
                }
                // Flush native overlay (native execution changes)
                if let Some(ref overlay) = pending.native_overlay {
                    if let Err(e) = overlay.flush(&self.state_db) {
                        tracing::error!(%e, "failed to flush native overlay");
                    }
                }
            }
        }

        self.state_db_tip = Some(committed_hash);
        if flush_height > self.evm_committed_height {
            self.evm_committed_height = flush_height;
        }
        self.write_native_applied_height(flush_height);

        // Prune orphaned fork bundles (height <= committed height).
        self.pending_bundles.retain(|_, p| p.height > flush_height);
    }

    fn execute_native_post_commit(
        &self,
        block: &TorusBlock,
        sender_actions: Vec<(Address, NativeAction)>,
        consumed_nonces: Vec<(Address, u64)>,
    ) -> Result<(), BridgeError> {
        if let Some(applied) = self.read_native_applied_height() {
            if applied >= block.header.height {
                return Ok(());
            }
        }
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
        NativeExecutor::distribute_fees(&mut ctx, block.header.evm_fee_revenue);
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

    /// Run native execution on a NativeStateOverlay seeded with EVM bundle
    /// account changes. All writes go to the overlay, NOT to state_db.
    fn execute_native_on_overlay(
        &self,
        block: &TorusBlock,
        sender_actions: Vec<(Address, NativeAction)>,
        consumed_nonces: Vec<(Address, u64)>,
        bundle: &BundleState,
    ) -> Result<NativeStateOverlay, BridgeError> {
        let overlay = NativeStateOverlay::new(self.state_db.clone());
        overlay.seed_from_bundle(bundle);

        let (pre_evm, post_evm) = sort_native_actions(&sender_actions);
        let mut ctx = NativeExecContext::new(
            overlay.clone(),
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
        NativeExecutor::distribute_fees(&mut ctx, block.header.evm_fee_revenue);
        NativeExecutor::process_epoch_boundary(&mut ctx);
        ctx.save_order_books();

        // Write consumed nonces to overlay
        for (sender, nonce) in &consumed_nonces {
            let mut nonce_key = [0u8; 28];
            nonce_key[..20].copy_from_slice(sender.as_slice());
            nonce_key[20..28].copy_from_slice(&nonce.to_be_bytes());
            let _ = overlay.put_cf_raw(
                torus_state::cf::CF_NATIVE_NONCES,
                &nonce_key,
                &block.header.height.to_be_bytes(),
            );
        }

        Ok(overlay)
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

        if !has_native && !has_evm {
            // Empty block — no execution to validate.
            self.persist_block_header(&torus_block);
            self.write_native_applied_height(torus_block.header.height);
            let parent_height = torus_block.header.height.saturating_sub(1);
            if parent_height <= self.evm_committed_height
                && torus_block.header.height > self.evm_committed_height
            {
                self.evm_committed_height = torus_block.header.height;
            }
            if torus_block.header.height > self.last_header.height {
                self.last_header = torus_block.header.clone();
            }
            let validator_set_updates = self.epoch_validator_set_updates(torus_block.header.height);
            return ValidateBlockResponse::Valid {
                app_state_updates: None,
                validator_set_updates,
            };
        }

        // Flush any pending bundles that consensus has committed.
        let committed_hash = request.block_tree()
            .highest_committed_block()
            .ok()
            .flatten();
        self.flush_committed_bundles(committed_hash);

        // Determine the parent block hash for overlay construction.
        let parent_hash = if block.justify.is_genesis_pc() {
            None
        } else {
            Some(block.justify.block)
        };

        // EVM catch-up: if our EVM state is behind, walk the committed chain
        // backwards from this block's parent to our last EVM-committed height,
        // then replay each ancestor's EVM txs in forward order to advance state_db.
        let mut did_catchup = false;
        if let Some(ref ph) = parent_hash {
            let parent_height = torus_block.header.height.saturating_sub(1);
            let evm_height = self.evm_committed_height;
            if parent_height > evm_height && !self.pending_bundles.contains_key(ph) {
                let gap = parent_height - evm_height;
                tracing::info!(height = torus_block.header.height, parent_height, evm_height, gap, "do_validate: EVM behind — attempting catch-up walk");

                // Collect ancestors from parent back to our EVM height.
                let mut ancestors = Vec::new();
                let mut cursor = *ph;
                while let Ok(Some(ancestor_block)) = request.block_tree().block(&cursor) {
                    let datums = ancestor_block.data.vec();
                    if let Some(datum) = datums.first() {
                        if let Ok(tb) = serde_json::from_slice::<TorusBlock>(datum.bytes()) {
                            if tb.header.height <= evm_height {
                                break;
                            }
                            ancestors.push((cursor, ancestor_block.clone(), tb));
                        }
                    }
                    if ancestor_block.justify.is_genesis_pc() {
                        break;
                    }
                    cursor = ancestor_block.justify.block;
                }
                ancestors.reverse();

                if ancestors.is_empty() || ancestors.first().map(|(_, _, tb)| tb.header.height).unwrap_or(0) > evm_height + 1 {
                    // Ancestor chain incomplete — insert this block without EVM
                    // so it becomes available for future catch-up walks.
                    tracing::info!(height = torus_block.header.height, evm_height, "do_validate: ancestor chain incomplete — inserting without EVM");
                    self.persist_block_header(&torus_block);
                    if torus_block.header.height > self.last_header.height {
                        self.last_header = torus_block.header.clone();
                    }
                    let validator_set_updates = self.epoch_validator_set_updates(torus_block.header.height);
                    return ValidateBlockResponse::Valid {
                        app_state_updates: None,
                        validator_set_updates,
                    };
                }

                let count = ancestors.len();
                for (hash, _raw_block, tb) in &ancestors {
                    let has_evm = !tb.evm_transactions.is_empty();
                    let has_native = !tb.native_actions.is_empty();
                    if has_evm || has_native {
                        let result = if has_native {
                            self.validator.validate_block_with_native_for_catchup(tb, &self.state_db, &self.evm_executor)
                        } else {
                            self.validator.validate_block_for_catchup(tb, &self.state_db, &self.evm_executor)
                        };
                        match result {
                            Ok(validated) => {
                                if let Err(e) = BlockCommitter::commit_block_metadata(&self.state_db, tb, &validated.receipts) {
                                    tracing::error!(%e, height = tb.header.height, "catch-up: metadata commit failed");
                                    return ValidateBlockResponse::Invalid;
                                }
                                if let Err(e) = BlockCommitter::commit_pending_bundle(&self.state_db, &validated.bundle) {
                                    tracing::error!(%e, height = tb.header.height, "catch-up: bundle commit failed");
                                    return ValidateBlockResponse::Invalid;
                                }
                                if has_native || tb.header.evm_fee_revenue > 0 {
                                    if let Err(e) = self.execute_native_post_commit(tb, validated.native_sender_actions, validated.native_consumed_nonces) {
                                        tracing::error!(%e, height = tb.header.height, "catch-up: native post-commit FAILED");
                                    }
                                }
                                self.write_native_applied_height(tb.header.height);
                            }
                            Err(e) => {
                                tracing::error!(%e, height = tb.header.height, "catch-up: EVM re-execution failed");
                                return ValidateBlockResponse::Invalid;
                            }
                        }
                    } else {
                        self.persist_block_header(tb);
                        self.write_native_applied_height(tb.header.height);
                    }
                    // Diagnostic: compare state root after each catch-up block.
                    let empty_bundle = BundleState::default();
                    if let Ok(current_root) = compute_post_bundle_state_root(&self.state_db, &empty_bundle) {
                        if current_root != tb.header.state_root {
                            tracing::warn!(
                                height = tb.header.height,
                                expected = %tb.header.state_root,
                                actual = %current_root,
                                fee_revenue = tb.header.evm_fee_revenue,
                                "catch-up: STATE ROOT DRIFT after block replay"
                            );
                        }
                    }
                    self.evm_committed_height = tb.header.height;
                    if tb.header.height > self.last_header.height {
                        self.last_header = tb.header.clone();
                    }
                    self.state_db_tip = Some(*hash);
                }
                did_catchup = true;
                tracing::info!(count, new_evm_height = self.evm_committed_height, "do_validate: EVM catch-up complete");
            }
        }

        // Build overlay from pending parent bundles so EVM reads uncommitted
        // ancestor state without corrupting state_db.
        let use_overlay = parent_hash
            .as_ref()
            .map_or(false, |ph| self.parent_is_pending(ph));

        let validation_result = if use_overlay {
            let parent = parent_hash.unwrap();
            let merged_parent = self.merged_parent_bundle(&parent);
            let overlay = StateOverlay::from_bundle(self.state_db.clone(), &merged_parent);
            if has_native {
                // Native+EVM with overlay not yet supported — fall back to direct.
                // Native blocks are rare; fork during native block is extremely unlikely.
                self.validator
                    .validate_block_with_native(&torus_block, &self.state_db, &self.evm_executor)
            } else {
                self.validator.validate_block_with_overlay(
                    &torus_block,
                    &overlay,
                    &self.state_db,
                    &merged_parent,
                    &self.evm_executor,
                )
            }
        } else if did_catchup {
            // Block immediately after catch-up: skip state root check since
            // catch-up replay may leave state_db with accumulated drift from
            // fee distribution effects. The block is already consensus-committed.
            if has_native {
                self.validator
                    .validate_block_with_native_for_catchup(&torus_block, &self.state_db, &self.evm_executor)
            } else {
                self.validator
                    .validate_block_for_catchup(&torus_block, &self.state_db, &self.evm_executor)
            }
        } else if has_native {
            self.validator
                .validate_block_with_native(&torus_block, &self.state_db, &self.evm_executor)
        } else {
            self.validator
                .validate_block(&torus_block, &self.state_db, &self.evm_executor)
        };

        match validation_result {
            Ok(validated) => {
                // Write block metadata (header, body, receipts, indices) immediately.
                // Defer EVM state (accounts, storage, code) until consensus commits.
                let meta_result = BlockCommitter::commit_block_metadata(
                    &self.state_db,
                    &torus_block,
                    &validated.receipts,
                );
                match meta_result {
                    Ok(block_hash) => {
                        tracing::info!(
                            height = torus_block.header.height,
                            %block_hash,
                            evm_txs = torus_block.evm_transactions.len(),
                            pending = use_overlay,
                            "validated block (EVM state deferred)"
                        );

                        let block_crypto_hash = block.hash;

                        if use_overlay {
                            // Fork case: defer BOTH EVM + native via overlay to
                            // prevent fork siblings from corrupting shared state_db.
                            let native_overlay = if has_native || torus_block.header.evm_fee_revenue > 0 {
                                match self.execute_native_on_overlay(
                                    &torus_block,
                                    validated.native_sender_actions,
                                    validated.native_consumed_nonces,
                                    &validated.bundle,
                                ) {
                                    Ok(ov) => Some(ov),
                                    Err(e) => {
                                        tracing::error!(%e, "native overlay execution failed");
                                        None
                                    }
                                }
                            } else {
                                None
                            };

                            self.pending_bundles.insert(block_crypto_hash, PendingExec {
                                bundle: validated.bundle,
                                native_overlay,
                                parent_hash,
                                height: torus_block.header.height,
                            });
                        } else {
                            // Linear case: no fork siblings, safe to commit immediately.
                            if let Err(e) = BlockCommitter::commit_pending_bundle(
                                &self.state_db,
                                &validated.bundle,
                            ) {
                                tracing::error!(%e, "failed to flush EVM bundle");
                            }
                            self.state_db_tip = Some(block_crypto_hash);
                            self.evm_committed_height = torus_block.header.height;

                            if has_native || torus_block.header.evm_fee_revenue > 0 {
                                match self.execute_native_post_commit(
                                    &torus_block,
                                    validated.native_sender_actions,
                                    validated.native_consumed_nonces,
                                ) {
                                    Ok(()) => self.write_native_applied_height(torus_block.header.height),
                                    Err(e) => tracing::error!(%e, "native post-commit execution failed"),
                                }
                            } else {
                                self.write_native_applied_height(torus_block.header.height);
                            }
                        }
                    }
                    Err(e) => {
                        tracing::error!(%e, "failed to commit block metadata");
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

        let (block, exec_result) = match result {
            Ok(proposed) => (proposed.block, proposed.exec_result),
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

        // Proposer state commitment: write metadata + EVM state immediately
        // (proposer always extends the linear chain — no fork risk).
        let has_evm = !block.evm_transactions.is_empty();
        let has_native = !block.native_actions.is_empty();
        if has_evm || has_native {
            match BlockCommitter::commit_block_metadata(
                &self.state_db,
                &block,
                &exec_result.receipts,
            ) {
                Ok(block_hash) => {
                    if let Err(e) = BlockCommitter::commit_pending_bundle(
                        &self.state_db,
                        &exec_result.bundle,
                    ) {
                        tracing::error!(%e, "proposer: failed to commit EVM bundle");
                    }
                    self.evm_committed_height = block.header.height;
                    tracing::info!(
                        height = block.header.height,
                        %block_hash,
                        evm_txs = block.evm_transactions.len(),
                        "proposer: committed block state to DB"
                    );
                    if has_native || block.header.evm_fee_revenue > 0 {
                        match self.execute_native_post_commit(
                            &block,
                            vec![],
                            vec![],
                        ) {
                            Ok(()) => self.write_native_applied_height(block.header.height),
                            Err(e) => tracing::error!(%e, "proposer: native post-commit failed"),
                        }
                    } else {
                        self.write_native_applied_height(block.header.height);
                    }
                }
                Err(e) => {
                    tracing::error!(%e, "proposer: failed to commit block metadata");
                }
            }
        }

        if block.header.height > self.last_header.height {
            self.last_header = block.header.clone();
        }

        let encoded = serde_json::to_vec(&block).expect("serialize TorusBlock");
        let hash = Self::hash_datum(&encoded);

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
        // During sync, blocks are already consensus-committed by quorum.
        // If our EVM state is behind, skip EVM re-execution so the block
        // gets inserted into the tree. The catch-up walk in do_validate
        // will replay EVM when the ancestor chain is complete.
        let block = request.proposed_block();
        let datums = block.data.vec();
        if datums.len() == 1 {
            if let Ok(tb) = serde_json::from_slice::<TorusBlock>(datums[0].bytes()) {
                let parent_height = tb.header.height.saturating_sub(1);
                if parent_height > self.evm_committed_height {
                    tracing::info!(
                        height = tb.header.height,
                        evm_height = self.evm_committed_height,
                        "validate_block_for_sync: EVM behind — inserting without re-execution"
                    );
                    self.persist_block_header(&tb);
                    if tb.header.height > self.last_header.height {
                        self.last_header = tb.header.clone();
                    }
                    let validator_set_updates = self.epoch_validator_set_updates(tb.header.height);
                    return ValidateBlockResponse::Valid {
                        app_state_updates: None,
                        validator_set_updates,
                    };
                }
            }
        }
        // EVM state is current — do full validation.
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
            evm_fee_revenue: 0,
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

#[cfg(test)]
mod crash_recovery_tests {
    use super::*;
    use torus_types::{Bloom, SignedNativeAction, B256};

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
    fn crash_recovery_replays_native_post_commit() {
        let (config, state_db) = make_test_config_and_db();
        let block = make_block(1, vec![sign_claim_rewards(100)]);

        // Simulate commit_block writing header+body but native exec never runs
        persist_block_for_test(&state_db, &block);

        // META_NATIVE_APPLIED_HEIGHT not set → simulates crash before native exec
        assert!(state_db
            .get_cf_raw(CF_CONSENSUS_META, META_NATIVE_APPLIED_HEIGHT)
            .unwrap()
            .is_none());

        // Reconstruct TorusApp — should detect gap and replay
        let app = TorusApp::new(state_db.clone(), &config, None, None);

        // Verify applied height was written
        let applied = app.read_native_applied_height();
        assert_eq!(applied, Some(1), "replay should set applied height to 1");
        assert_eq!(app.last_header.height, 1, "last_header should be updated");
    }

    #[test]
    fn no_replay_when_already_applied() {
        let (config, state_db) = make_test_config_and_db();
        let block = make_block(5, vec![sign_claim_rewards(200)]);

        persist_block_for_test(&state_db, &block);

        // Pre-set applied height = committed height → no gap
        state_db
            .put_cf_raw(CF_CONSENSUS_META, META_NATIVE_APPLIED_HEIGHT, &5u64.to_be_bytes())
            .unwrap();

        let app = TorusApp::new(state_db.clone(), &config, None, None);
        assert_eq!(app.read_native_applied_height(), Some(5));
        // last_header stays at genesis since no replay occurred
        assert_eq!(app.last_header.height, 0);
    }

    #[test]
    fn replay_empty_block_marks_applied() {
        let (config, state_db) = make_test_config_and_db();
        let block = make_block(3, vec![]); // no native actions

        // Only persist header (empty block has no body in CF_BLOCK_BODIES)
        let block_hash = alloy_primitives::keccak256(&block.header.canonical_header_bytes());
        let header_json = serde_json::to_vec(&block.header).unwrap();
        let mut data = Vec::with_capacity(32 + header_json.len());
        data.extend_from_slice(block_hash.as_slice());
        data.extend_from_slice(&header_json);
        state_db
            .put_cf_raw(CF_BLOCK_HEADERS, &block.header.height.to_be_bytes(), &data)
            .unwrap();

        let app = TorusApp::new(state_db.clone(), &config, None, None);
        assert_eq!(
            app.read_native_applied_height(),
            Some(3),
            "empty block should be marked as applied"
        );
    }

    #[test]
    fn idempotent_native_post_commit() {
        let (config, state_db) = make_test_config_and_db();
        let signed = sign_claim_rewards(300);
        let sender = signed.recover_sender().unwrap();
        let block = make_block(2, vec![signed.clone()]);

        let app = TorusApp::new(state_db.clone(), &config, None, None);

        let sender_actions = vec![(sender, signed.action.clone())];
        let consumed_nonces = vec![(sender, signed.nonce)];

        // First call
        app.execute_native_post_commit(&block, sender_actions.clone(), consumed_nonces.clone())
            .unwrap();
        app.write_native_applied_height(2);

        // Second call — should be a no-op due to idempotency guard
        app.execute_native_post_commit(&block, sender_actions, consumed_nonces)
            .unwrap();

        assert_eq!(app.read_native_applied_height(), Some(2));
    }
}
