//! Transaction mempool for the Torus-hyperBFT blockchain.
//!
//! Manages pending EVM transactions and native actions, providing:
//! - EVM tx pool with nonce tracking and gas price ordering
//! - Transaction validation: signature recovery, nonce/balance checks
//! - Size-limited pool with eviction and replacement-by-fee
//! - Drain interface for block proposers

pub mod error;
pub mod evm_pool;
pub mod native_pool;
pub mod rate_limit;
pub mod validate;

use std::sync::RwLock;

use alloy_primitives::B256;
use torus_state::{NativeDaStore, StateDb};
use torus_types::SignedNativeAction;

/// Wall-clock milliseconds — the clock native-action nonces are minted from.
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub use crate::error::MempoolError;
pub use crate::evm_pool::EvmPoolEntry;

/// Mempool configuration.
#[derive(Clone, Debug)]
pub struct MempoolConfig {
    /// Maximum total EVM transactions in the pool.
    pub max_pool_size: usize,
    /// Maximum pending EVM transactions per sender.
    pub max_per_sender: usize,
    /// Minimum gas price bump percentage for replacement (e.g., 10 = 10%).
    pub replacement_bump_pct: u64,
    /// Chain ID for transaction validation.
    pub chain_id: u64,
    /// Block gas limit for validation.
    pub block_gas_limit: u64,
    // ---- Rate limiting (Task 3.1.4) ----
    /// Sliding window size in blocks for rate tracking.
    pub rate_window_blocks: u64,
    /// Max EVM txs per sender within the rate window.
    pub evm_rate_limit_per_window: u32,
    /// Max native actions per sender within the rate window.
    pub native_rate_limit_per_window: u32,
    /// Max EVM txs per sender per block.
    pub evm_per_block_cap: usize,
    /// Max total EVM txs per block (all senders combined).
    pub evm_total_block_cap: usize,
    /// Max native actions per sender per block.
    pub native_per_block_cap: usize,
    /// Max total native pool size.
    pub native_pool_max_size: usize,
    /// Max pending native actions per sender in the pool.
    pub native_per_sender_cap: usize,
    // ---- Memory budget (Phase 3: 3.1.7) ----
    /// Maximum combined memory for EVM + native pools in bytes (0 = unlimited).
    pub max_memory_bytes: usize,
}

impl Default for MempoolConfig {
    fn default() -> Self {
        Self {
            max_pool_size: 4096,
            max_per_sender: 16,
            replacement_bump_pct: 10,
            chain_id: torus_types::eip712::TORUS_CHAIN_ID,
            block_gas_limit: 30_000_000,
            rate_window_blocks: rate_limit::RATE_WINDOW_BLOCKS,
            evm_rate_limit_per_window: rate_limit::EVM_RATE_LIMIT_PER_WINDOW,
            native_rate_limit_per_window: rate_limit::NATIVE_RATE_LIMIT_PER_WINDOW,
            evm_per_block_cap: rate_limit::EVM_PER_BLOCK_CAP,
            evm_total_block_cap: rate_limit::EVM_TOTAL_BLOCK_CAP,
            native_per_block_cap: rate_limit::NATIVE_PER_BLOCK_CAP,
            native_pool_max_size: rate_limit::NATIVE_POOL_MAX_SIZE,
            native_per_sender_cap: rate_limit::NATIVE_PER_SENDER_CAP,
            max_memory_bytes: 64 * 1024 * 1024, // 64 MB default
        }
    }
}

/// Thread-safe transaction mempool combining EVM and native action pools.
pub struct Mempool {
    evm: RwLock<evm_pool::EvmPool>,
    native: RwLock<native_pool::NativePool>,
    rate_tracker: RwLock<rate_limit::RateTracker>,
    state: StateDb,
    /// Durable, nonce-gate-decoupled native-action body store (out-of-band DA).
    da_store: NativeDaStore,
    config: MempoolConfig,
    /// Approximate total memory used by pooled transactions (Phase 3: 3.1.7).
    memory_used: std::sync::atomic::AtomicUsize,
    /// Outbound native action gossip channel (set once at startup).
    native_gossip_tx: std::sync::OnceLock<tokio::sync::mpsc::Sender<Vec<u8>>>,
    /// When false, native action gossip is suppressed (direct-to-leader mode).
    native_gossip_enabled: std::sync::atomic::AtomicBool,
}

impl Mempool {
    /// Create a new mempool backed by the given state database.
    pub fn new(state: StateDb, config: MempoolConfig) -> Self {
        let native_pool = native_pool::NativePool::new(
            config.native_pool_max_size,
            config.native_per_sender_cap,
            config.native_per_block_cap,
        );
        let tracker = rate_limit::RateTracker::new(
            config.rate_window_blocks,
            config.evm_rate_limit_per_window,
            config.native_rate_limit_per_window,
        );
        // DA store shares the same RocksDB handle; every native-action body the
        // mempool sees is mirrored here durably (decoupled from the nonce gate).
        let da_store = NativeDaStore::new(state.clone());
        Self {
            evm: RwLock::new(evm_pool::EvmPool::new()),
            native: RwLock::new(native_pool),
            rate_tracker: RwLock::new(tracker),
            state,
            da_store,
            config,
            memory_used: std::sync::atomic::AtomicUsize::new(0),
            native_gossip_tx: std::sync::OnceLock::new(),
            native_gossip_enabled: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// Enable or disable native action gossip at runtime.
    pub fn set_native_gossip_enabled(&self, enabled: bool) {
        self.native_gossip_enabled.store(enabled, std::sync::atomic::Ordering::Relaxed);
    }

    /// Set the outbound gossip channel for native actions.
    /// Called once at startup after network initialization.
    pub fn set_native_gossip_tx(&self, tx: tokio::sync::mpsc::Sender<Vec<u8>>) {
        match self.native_gossip_tx.set(tx) {
            Ok(()) => tracing::info!("native action gossip enabled"),
            Err(_) => tracing::warn!("native_gossip_tx already set"),
        }
    }

    /// Submit a raw RLP-encoded EVM transaction.
    /// Decodes, validates, checks rate limit, and inserts. Returns the tx hash.
    pub fn add_evm_tx(&self, raw_rlp: Vec<u8>) -> Result<B256, MempoolError> {
        let entry = validate::validate_evm_tx(
            &raw_rlp,
            &self.state,
            self.config.chain_id,
            self.config.block_gas_limit,
        )?;

        let hash = entry.hash;
        let sender = entry.sender;
        let tx_size = raw_rlp.len();

        // Memory budget check (Phase 3: 3.1.7).
        if self.config.max_memory_bytes > 0 {
            let current = self
                .memory_used
                .load(std::sync::atomic::Ordering::Relaxed);
            if current + tx_size > self.config.max_memory_bytes {
                return Err(MempoolError::PoolFull);
            }
        }

        // Rate limit check (Task 3.1.4).
        {
            let tracker = self.rate_tracker.read().unwrap();
            if tracker.is_evm_rate_limited(&sender) {
                return Err(MempoolError::RateLimited {
                    sender,
                    window: self.config.rate_window_blocks,
                });
            }
        }

        let mut pool = self.evm.write().unwrap();

        if pool.contains(&hash) {
            return Err(MempoolError::DuplicateTx(hash));
        }

        let (_replaced, freed_bytes) = pool.insert(
            entry,
            self.config.max_pool_size,
            self.config.max_per_sender,
            self.config.replacement_bump_pct,
        )?;

        // Track memory usage (Phase 3: 3.1.7)
        self.memory_used
            .fetch_add(tx_size, std::sync::atomic::Ordering::Relaxed);
        // FIX EVM-FIND-02: Decrement for evicted/replaced transactions.
        if freed_bytes > 0 {
            self.memory_used
                .fetch_sub(freed_bytes, std::sync::atomic::Ordering::Relaxed);
        }

        tracing::debug!(tx_hash = %hash, "evm tx added to mempool");
        Ok(hash)
    }

    /// Re-insert previously drained EVM transactions (e.g., after block reorg).
    pub fn reinsert_evm(&self, raw_txs: Vec<Vec<u8>>) {
        for raw in raw_txs {
            if let Err(e) = self.add_evm_tx(raw) {
                tracing::trace!("reinsert skipped: {e}");
            }
        }
    }

    /// Drain EVM transactions for a block proposal.
    /// Selects highest-gas-price transactions that fit within `gas_limit`,
    /// respecting per-sender nonce ordering. Removes selected txs from the pool.
    ///
    /// `parent_hash` seeds deterministic same-price shuffling (anti-MEV, Task 3.1.5).
    pub fn drain_evm(&self, gas_limit: u64, parent_hash: B256) -> Vec<Vec<u8>> {
        let mut pool = self.evm.write().unwrap();
        let drained = pool.drain(gas_limit, self.config.evm_per_block_cap, self.config.evm_total_block_cap, &parent_hash);
        // FIX EVM-FIND-02: Decrement memory for drained transactions.
        let drained_bytes: usize = drained.iter().map(|tx| tx.len()).sum();
        if drained_bytes > 0 {
            self.memory_used
                .fetch_sub(drained_bytes, std::sync::atomic::Ordering::Relaxed);
        }
        drained
    }

    /// Current EVM pool size.
    pub fn evm_pool_size(&self) -> usize {
        self.evm.read().unwrap().size()
    }

    /// Submit a signed native action. Full EIP-712 validation: signature, chain ID, nonce freshness.
    /// Gossips to the validator mesh on success (RPC submission path).
    /// FIX EVM-FIND-05: Calls validate() instead of just recover_sender().
    pub fn add_native_action(&self, action: SignedNativeAction) -> Result<(), MempoolError> {
        let current_time_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock before epoch")
            .as_millis() as u64;
        let sender = action
            .validate(current_time_ms, self.config.chain_id)
            .map_err(|e| MempoolError::NativeValidationFailed(e.to_string()))?;
        self.submit_native_action(sender, action.clone())?;
        // Mirror the admitted body to the durable DA store (out-of-band delivery).
        self.mirror_to_da(&action);
        self.gossip_native_action(sender, &action);
        Ok(())
    }

    /// Submit a session-signed native action with pre-resolved sender.
    /// Called by RPC after verifying the session key signature against state.
    /// Gossips to the validator mesh on success.
    pub fn add_native_action_presigned(
        &self,
        sender: alloy_primitives::Address,
        action: SignedNativeAction,
    ) -> Result<(), MempoolError> {
        let current_time_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock before epoch")
            .as_millis() as u64;

        use torus_types::eip712::NONCE_WINDOW_MS;
        if action.nonce.saturating_add(NONCE_WINDOW_MS) < current_time_ms {
            return Err(MempoolError::NativeValidationFailed("nonce too old".into()));
        }
        if action.nonce > current_time_ms.saturating_add(NONCE_WINDOW_MS) {
            return Err(MempoolError::NativeValidationFailed(
                "nonce too far in future".into(),
            ));
        }

        self.submit_native_action(sender, action.clone())?;
        // Mirror the admitted body to the durable DA store (out-of-band delivery).
        self.mirror_to_da(&action);
        self.gossip_native_action(sender, &action);
        Ok(())
    }

    /// Insert a native action received from gossip with a pre-verified sender.
    /// Skips ECDSA recovery (the expensive part) — trusts that the originating
    /// node already verified the signature. Only checks nonce freshness.
    pub fn add_native_action_from_gossip_trusted(
        &self,
        sender: alloy_primitives::Address,
        action: SignedNativeAction,
    ) -> Result<(), MempoolError> {
        // DA store is DECOUPLED from the 60s nonce gate: a pushed/gossiped body is
        // (or will be) block-referenced, so mirror it durably even when it is too
        // stale for mempool admission -- otherwise the referencing block can never
        // be reconstructed and consensus wedges (livelock root cause, mem 28e1a821).
        self.mirror_to_da(&action);

        let current_time_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock before epoch")
            .as_millis() as u64;
        use torus_types::eip712::NONCE_WINDOW_MS;
        if action.nonce.saturating_add(NONCE_WINDOW_MS) < current_time_ms {
            return Err(MempoolError::NativeValidationFailed("nonce too old (>60s)".into()));
        }
        if action.nonce > current_time_ms.saturating_add(NONCE_WINDOW_MS) {
            return Err(MempoolError::NativeValidationFailed("nonce too far in future".into()));
        }
        self.submit_native_action(sender, action)
    }

    /// Gossip includes sender address so receivers can skip ECDSA recovery.
    /// Disabled when direct-to-leader forwarding is active (avoids flooding
    /// the GossipSub mesh and starving consensus messages under load).
    fn gossip_native_action(&self, sender: alloy_primitives::Address, action: &SignedNativeAction) {
        if !self.native_gossip_enabled.load(std::sync::atomic::Ordering::Relaxed) {
            return;
        }
        if let Some(tx) = self.native_gossip_tx.get() {
            if let Ok(bytes) = bincode::serialize(&(sender, action)) {
                match tx.try_send(bytes) {
                    Ok(()) => {}
                    Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                        tracing::debug!("native gossip channel full, dropping outbound action");
                    }
                    Err(_) => {
                        tracing::warn!("native gossip channel closed");
                    }
                }
            }
        }
    }

    /// FIX EVM-FIND-15: Renamed from submit_native_action and restricted to pub(crate).
    /// Internal method for inserting a native action with a pre-verified sender.
    pub fn submit_native_action(
        &self,
        sender: alloy_primitives::Address,
        action: SignedNativeAction,
    ) -> Result<(), MempoolError> {
        // Rate limit check — exempt oracle/governance actions (Task 3.1.4).
        if !rate_limit::is_exempt_action(&action.action) {
            let tracker = self.rate_tracker.read().unwrap();
            if tracker.is_native_rate_limited(&sender) {
                return Err(MempoolError::RateLimited {
                    sender,
                    window: self.config.rate_window_blocks,
                });
            }
        }

        let mut pool = self.native.write().unwrap();
        pool.insert(sender, action)?;
        tracing::debug!(%sender, "native action added to mempool");
        Ok(())
    }

    /// Drain native actions for a block proposal.
    /// Returns up to `limit` actions in priority order (cancels first).
    /// Per-sender-per-block caps are enforced internally.
    pub fn drain_native(&self, limit: usize) -> Vec<SignedNativeAction> {
        let mut pool = self.native.write().unwrap();
        let evicted = pool.evict_expired(now_ms());
        if evicted > 0 {
            tracing::info!(evicted, "evicted nonce-expired native actions from pool");
        }
        pool.drain(limit)
    }

    /// Re-insert previously drained native actions (e.g., after reorg).
    pub fn reinsert_native(&self, actions: Vec<SignedNativeAction>) {
        // Keep the DA-store invariant: every pooled body stays reconstructable.
        self.mirror_native_to_da(&actions);
        let mut pool = self.native.write().unwrap();
        pool.reinsert(actions);
    }

    /// Current native pool size.
    pub fn native_pool_size(&self) -> usize {
        self.native.read().unwrap().size()
    }

    /// Look up a native action by its hash (for compact block reconstruction).
    pub fn get_native_by_hash(&self, hash: &B256) -> Option<SignedNativeAction> {
        self.native.read().unwrap().get_by_hash(hash)
    }

    /// Look up a native-action body in the durable DA store by hash.
    ///
    /// The DA store is the durable, nonce-gate-decoupled superset of the in-memory
    /// pool, so a block-referenced body survives the 60s nonce window, pool
    /// eviction, and a process restart. Compact-block reconstruction MUST use this
    /// (not the ephemeral pool) to avoid the missing-body livelock (mem 28e1a821).
    pub fn get_native_da(&self, hash: &B256) -> Option<SignedNativeAction> {
        match self.da_store.get(hash) {
            Ok(v) => v,
            Err(e) => {
                tracing::error!("native DA store read failed: {e}");
                None
            }
        }
    }

    /// Mirror native-action bodies into the durable DA store (proposer guarantee:
    /// every body referenced by a block we propose stays reconstructable).
    pub fn mirror_native_to_da(&self, actions: &[SignedNativeAction]) {
        for action in actions {
            self.mirror_to_da(action);
        }
    }

    /// Best-effort durable mirror of one native-action body. A DA write failure is
    /// logged, never propagated: it must not fail an ingest path (the rare
    /// pull-fallback is the safety net), but it is loud because a lost body can
    /// later force a fetch or, worst case, a stall.
    fn mirror_to_da(&self, action: &SignedNativeAction) {
        if let Err(e) = self.da_store.put(action) {
            tracing::error!("native DA store write failed: {e}");
        }
    }

    /// Select native actions for a block proposal WITHOUT removing them.
    /// Actions stay in the pool for other validators' `get_by_hash` lookups.
    /// Call `remove_committed_native` after the block is committed.
    pub fn select_native_for_block(&self, limit: usize) -> Vec<SignedNativeAction> {
        self.native.write().unwrap().select_for_block(limit)
    }

    pub fn select_native_for_block_with_senders(&self, limit: usize) -> Vec<(alloy_primitives::Address, SignedNativeAction)> {
        self.native.write().unwrap().select_for_block_with_senders(limit)
    }

    /// Pipeline-aware variant: skips actions whose hash is in `exclude` (the
    /// proposer's in-flight, proposed-but-uncommitted action hashes). Prevents the
    /// same action being re-selected for blocks N+1/N+2 before N commits — the root
    /// cause of duplicate native inclusion.
    /// `bytes_cap` bounds the summed encoded size of the selected bodies (WAN
    /// dissemination budget — see `rate_limit::NATIVE_BLOCK_BYTES_CAP`).
    pub fn select_native_for_block_with_senders_excluding(
        &self,
        limit: usize,
        exclude: &std::collections::HashSet<B256>,
        bytes_cap: usize,
    ) -> Vec<(alloy_primitives::Address, SignedNativeAction)> {
        let mut pool = self.native.write().unwrap();
        let evicted = pool.evict_expired(now_ms());
        if evicted > 0 {
            tracing::info!(evicted, "evicted nonce-expired native actions from pool");
        }
        pool.select_for_block_with_senders_excluding(limit, exclude, bytes_cap)
    }

    /// Remove native actions that were included in a committed block.
    pub fn remove_committed_native(&self, hashes: &[B256]) {
        self.native.write().unwrap().remove_committed(hashes);
    }

    /// Approximate total memory used by pooled transactions (Phase 3: 3.1.7).
    pub fn memory_used(&self) -> usize {
        self.memory_used
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Drain both pools for a block proposal.
    ///
    /// `parent_hash` seeds EVM tx anti-MEV shuffling (Task 3.1.5).
    pub fn drain_for_block(
        &self,
        native_limit: usize,
        evm_gas_limit: u64,
        parent_hash: B256,
    ) -> (Vec<SignedNativeAction>, Vec<Vec<u8>>) {
        let native = self.drain_native(native_limit);
        let evm = self.drain_evm(evm_gas_limit, parent_hash);
        (native, evm)
    }

    /// Return the next nonce a sender should use, accounting for pending pool TXs.
    ///
    /// This is the "pending" nonce: state_nonce + count of consecutive in-pool
    /// nonces starting from state_nonce. Clients use this to avoid nonce collisions
    /// when submitting transactions faster than blocks commit.
    pub fn pending_nonce(&self, sender: &alloy_primitives::Address) -> u64 {
        let account = self.state.get_account(sender).ok().flatten();
        let state_nonce = account.map(|a| a.nonce).unwrap_or(0);
        let pool = self.evm.read().unwrap();
        pool.pending_nonce(sender, state_nonce)
    }

    /// Record which senders had txs/actions included in a committed block.
    /// Updates the sliding-window rate tracker and prunes stale transactions
    /// whose nonce is now below the sender's confirmed state nonce.
    pub fn notify_block_committed(
        &self,
        block_height: u64,
        evm_senders: &[alloy_primitives::Address],
        native_senders: &[alloy_primitives::Address],
    ) {
        let mut tracker = self.rate_tracker.write().unwrap();
        tracker.record_block(block_height, evm_senders, native_senders);
        drop(tracker);

        if !evm_senders.is_empty() {
            let mut pool = self.evm.write().unwrap();
            let mut total_pruned = 0usize;
            let mut total_freed = 0usize;
            for sender in evm_senders {
                let state_nonce = self
                    .state
                    .get_account(sender)
                    .ok()
                    .flatten()
                    .map(|a| a.nonce)
                    .unwrap_or(0);
                let (pruned, freed) = pool.prune_confirmed(sender, state_nonce);
                total_pruned += pruned;
                total_freed += freed;
            }
            if total_freed > 0 {
                self.memory_used
                    .fetch_sub(total_freed, std::sync::atomic::Ordering::Relaxed);
            }
            if total_pruned > 0 {
                tracing::debug!(pruned = total_pruned, "pruned stale txs after block commit");
            }
        }
    }

    /// Prune all senders in the EVM pool against current state nonces.
    /// Call this on every block commit so non-proposing validators clean up
    /// txs that were included by other validators' blocks.
    pub fn prune_committed_txs(&self) {
        let senders: Vec<alloy_primitives::Address> = {
            let pool = self.evm.read().unwrap();
            pool.all_senders()
        };
        if senders.is_empty() {
            return;
        }
        let mut pool = self.evm.write().unwrap();
        let mut total_pruned = 0usize;
        let mut total_freed = 0usize;
        for sender in &senders {
            let state_nonce = self
                .state
                .get_account(sender)
                .ok()
                .flatten()
                .map(|a| a.nonce)
                .unwrap_or(0);
            let (pruned, freed) = pool.prune_confirmed(sender, state_nonce);
            total_pruned += pruned;
            total_freed += freed;
        }
        if total_freed > 0 {
            self.memory_used
                .fetch_sub(total_freed, std::sync::atomic::Ordering::Relaxed);
        }
        if total_pruned > 0 {
            tracing::info!(pruned = total_pruned, senders = senders.len(), "pruned stale txs on block commit");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_consensus::{SignableTransaction, TxEip1559, TxEnvelope};
    use alloy_primitives::{Address, Bytes, Signature as AlloySig, TxKind, B256, U256};
    use alloy_rlp::Encodable;
    use k256::ecdsa::SigningKey;
    use revm::state::AccountInfo;
    use sha3::{Digest, Keccak256};
    use tempfile::TempDir;

    fn address_from_key(key: &SigningKey) -> Address {
        let vk = key.verifying_key();
        let pubkey = vk.to_encoded_point(false);
        let hash = Keccak256::digest(&pubkey.as_bytes()[1..]);
        Address::from_slice(&hash[12..])
    }

    fn create_eip1559_tx(
        key: &SigningKey,
        nonce: u64,
        max_fee: u128,
        priority_fee: u128,
        gas_limit: u64,
        value: U256,
    ) -> Vec<u8> {
        let tx = TxEip1559 {
            chain_id: torus_types::eip712::TORUS_CHAIN_ID,
            nonce,
            max_fee_per_gas: max_fee,
            max_priority_fee_per_gas: priority_fee,
            gas_limit,
            to: TxKind::Call(Address::ZERO),
            value,
            input: Bytes::new(),
            access_list: Default::default(),
        };

        let sig_hash = tx.signature_hash();
        let (sig, recid) = key.sign_prehash_recoverable(sig_hash.as_slice()).unwrap();
        let r_bytes = sig.r().to_bytes();
        let s_bytes = sig.s().to_bytes();
        let r = U256::from_be_slice(r_bytes.as_slice());
        let s = U256::from_be_slice(s_bytes.as_slice());
        let v = recid.is_y_odd();
        let signature = AlloySig::new(r, s, v);

        let signed = tx.into_signed(signature);
        let envelope = TxEnvelope::Eip1559(signed);
        let mut buf = Vec::new();
        envelope.encode(&mut buf);
        buf
    }

    fn setup() -> (TempDir, StateDb) {
        let dir = TempDir::new().unwrap();
        let state = StateDb::open(dir.path()).unwrap();
        (dir, state)
    }

    fn fund(state: &StateDb, addr: &Address, balance: U256, nonce: u64) {
        state
            .put_account(
                addr,
                &AccountInfo {
                    balance,
                    nonce,
                    code_hash: B256::ZERO,
                    code: None,
                    account_id: None,
                },
            )
            .unwrap();
    }

    fn key(seed: u8) -> SigningKey {
        let mut secret = [0u8; 32];
        secret[31] = seed;
        secret[0] = 1;
        SigningKey::from_slice(&secret).unwrap()
    }

    #[test]
    fn insert_and_retrieve() {
        let (_dir, state) = setup();
        let pool = Mempool::new(state.clone(), MempoolConfig::default());

        let k = key(1);
        let addr = address_from_key(&k);
        fund(&state, &addr, U256::from(10u64.pow(18)), 0);

        let raw = create_eip1559_tx(&k, 0, 1_000_000_000, 100_000_000, 21_000, U256::ZERO);
        let hash = pool.add_evm_tx(raw).unwrap();
        assert_eq!(pool.evm_pool_size(), 1);
        assert!(!hash.is_zero());
    }

    #[test]
    fn nonce_ordering_per_sender() {
        let (_dir, state) = setup();
        let pool = Mempool::new(state.clone(), MempoolConfig::default());

        let k = key(2);
        let addr = address_from_key(&k);
        fund(&state, &addr, U256::from(10u64.pow(18)), 0);

        pool.add_evm_tx(create_eip1559_tx(
            &k,
            2,
            1_000_000_000,
            100_000_000,
            21_000,
            U256::ZERO,
        ))
        .unwrap();
        pool.add_evm_tx(create_eip1559_tx(
            &k,
            0,
            3_000_000_000,
            300_000_000,
            21_000,
            U256::ZERO,
        ))
        .unwrap();
        pool.add_evm_tx(create_eip1559_tx(
            &k,
            1,
            2_000_000_000,
            200_000_000,
            21_000,
            U256::ZERO,
        ))
        .unwrap();
        assert_eq!(pool.evm_pool_size(), 3);

        let drained = pool.drain_evm(30_000_000, B256::ZERO);
        assert_eq!(drained.len(), 3);
        assert_eq!(pool.evm_pool_size(), 0);
    }

    #[test]
    fn reject_wrong_chain_id() {
        let (_dir, state) = setup();
        let k = key(3);
        let addr = address_from_key(&k);
        fund(&state, &addr, U256::from(10u64.pow(18)), 0);

        let tx = TxEip1559 {
            chain_id: 9999,
            nonce: 0,
            max_fee_per_gas: 1_000_000_000,
            max_priority_fee_per_gas: 100_000_000,
            gas_limit: 21_000,
            to: TxKind::Call(Address::ZERO),
            value: U256::ZERO,
            input: Bytes::new(),
            access_list: Default::default(),
        };
        let sig_hash = tx.signature_hash();
        let (sig, recid) = k.sign_prehash_recoverable(sig_hash.as_slice()).unwrap();
        let signature = AlloySig::new(
            U256::from_be_slice(sig.r().to_bytes().as_slice()),
            U256::from_be_slice(sig.s().to_bytes().as_slice()),
            recid.is_y_odd(),
        );
        let envelope = TxEnvelope::Eip1559(tx.into_signed(signature));
        let mut raw = Vec::new();
        envelope.encode(&mut raw);

        let pool = Mempool::new(state, MempoolConfig::default());
        let err = pool.add_evm_tx(raw).unwrap_err();
        assert!(matches!(err, MempoolError::InvalidChainId { .. }));
    }

    #[test]
    fn reject_nonce_too_low() {
        let (_dir, state) = setup();
        let pool = Mempool::new(state.clone(), MempoolConfig::default());

        let k = key(4);
        let addr = address_from_key(&k);
        fund(&state, &addr, U256::from(10u64.pow(18)), 5);

        let raw = create_eip1559_tx(&k, 3, 1_000_000_000, 100_000_000, 21_000, U256::ZERO);
        let err = pool.add_evm_tx(raw).unwrap_err();
        assert!(matches!(err, MempoolError::NonceTooLow { .. }));
    }

    #[test]
    fn reject_insufficient_balance() {
        let (_dir, state) = setup();
        let pool = Mempool::new(state.clone(), MempoolConfig::default());

        let k = key(5);
        let addr = address_from_key(&k);
        fund(&state, &addr, U256::from(1000u64), 0);

        let raw = create_eip1559_tx(&k, 0, 1_000_000_000, 100_000_000, 21_000, U256::ZERO);
        let err = pool.add_evm_tx(raw).unwrap_err();
        assert!(matches!(err, MempoolError::InsufficientBalance { .. }));
    }

    #[test]
    fn reject_gas_limit_exceeded() {
        let (_dir, state) = setup();
        let pool = Mempool::new(state.clone(), MempoolConfig::default());

        let k = key(6);
        let addr = address_from_key(&k);
        fund(&state, &addr, U256::from(10u64.pow(18)), 0);

        let raw = create_eip1559_tx(&k, 0, 1_000_000_000, 100_000_000, 31_000_000, U256::ZERO);
        let err = pool.add_evm_tx(raw).unwrap_err();
        assert!(matches!(err, MempoolError::GasLimitExceeded { .. }));
    }

    #[test]
    fn replacement_by_fee() {
        let (_dir, state) = setup();
        let pool = Mempool::new(state.clone(), MempoolConfig::default());

        let k = key(7);
        let addr = address_from_key(&k);
        fund(&state, &addr, U256::from(10u64.pow(18)), 0);

        pool.add_evm_tx(create_eip1559_tx(
            &k,
            0,
            1_000_000_000,
            100_000_000,
            21_000,
            U256::ZERO,
        ))
        .unwrap();
        assert_eq!(pool.evm_pool_size(), 1);

        let err = pool
            .add_evm_tx(create_eip1559_tx(
                &k,
                0,
                1_050_000_000,
                105_000_000,
                21_000,
                U256::ZERO,
            ))
            .unwrap_err();
        assert!(matches!(err, MempoolError::ReplacementUnderpriced { .. }));

        pool.add_evm_tx(create_eip1559_tx(
            &k,
            0,
            1_200_000_000,
            120_000_000,
            21_000,
            U256::ZERO,
        ))
        .unwrap();
        assert_eq!(pool.evm_pool_size(), 1);
    }

    #[test]
    fn pool_size_limit_eviction() {
        let (_dir, state) = setup();
        let config = MempoolConfig {
            max_pool_size: 3,
            max_per_sender: 16,
            ..MempoolConfig::default()
        };
        let pool = Mempool::new(state.clone(), config);

        for i in 1..=3u8 {
            let k = key(10 + i);
            let addr = address_from_key(&k);
            fund(&state, &addr, U256::from(10u64.pow(18)), 0);
            pool.add_evm_tx(create_eip1559_tx(
                &k,
                0,
                (i as u128) * 1_000_000_000,
                (i as u128) * 100_000_000,
                21_000,
                U256::ZERO,
            ))
            .unwrap();
        }
        assert_eq!(pool.evm_pool_size(), 3);

        let k4 = key(14);
        fund(&state, &address_from_key(&k4), U256::from(10u64.pow(18)), 0);
        pool.add_evm_tx(create_eip1559_tx(
            &k4,
            0,
            5_000_000_000,
            500_000_000,
            21_000,
            U256::ZERO,
        ))
        .unwrap();
        assert_eq!(pool.evm_pool_size(), 3);

        let k5 = key(15);
        fund(&state, &address_from_key(&k5), U256::from(10u64.pow(18)), 0);
        let err = pool
            .add_evm_tx(create_eip1559_tx(
                &k5,
                0,
                500_000_000,
                50_000_000,
                21_000,
                U256::ZERO,
            ))
            .unwrap_err();
        assert!(matches!(err, MempoolError::PoolFull));
    }

    #[test]
    fn per_sender_limit() {
        let (_dir, state) = setup();
        let config = MempoolConfig {
            max_per_sender: 2,
            ..MempoolConfig::default()
        };
        let pool = Mempool::new(state.clone(), config);

        let k = key(20);
        let addr = address_from_key(&k);
        fund(&state, &addr, U256::from(10u64.pow(18)), 0);

        pool.add_evm_tx(create_eip1559_tx(
            &k,
            0,
            1_000_000_000,
            100_000_000,
            21_000,
            U256::ZERO,
        ))
        .unwrap();
        pool.add_evm_tx(create_eip1559_tx(
            &k,
            1,
            1_000_000_000,
            100_000_000,
            21_000,
            U256::ZERO,
        ))
        .unwrap();
        let err = pool
            .add_evm_tx(create_eip1559_tx(
                &k,
                2,
                1_000_000_000,
                100_000_000,
                21_000,
                U256::ZERO,
            ))
            .unwrap_err();
        assert!(matches!(err, MempoolError::PoolFull));
    }

    #[test]
    fn drain_respects_gas_budget() {
        let (_dir, state) = setup();
        let pool = Mempool::new(state.clone(), MempoolConfig::default());

        let k = key(30);
        let addr = address_from_key(&k);
        fund(&state, &addr, U256::from(10u64.pow(18)), 0);

        for i in 0..3u64 {
            pool.add_evm_tx(create_eip1559_tx(
                &k,
                i,
                1_000_000_000,
                100_000_000,
                21_000,
                U256::ZERO,
            ))
            .unwrap();
        }

        let drained = pool.drain_evm(42_000, B256::ZERO);
        assert_eq!(drained.len(), 2);
        assert_eq!(pool.evm_pool_size(), 1);
    }

    #[test]
    fn drain_gas_price_priority_across_senders() {
        let (_dir, state) = setup();
        let pool = Mempool::new(state.clone(), MempoolConfig::default());

        let ka = key(40);
        fund(&state, &address_from_key(&ka), U256::from(10u64.pow(18)), 0);
        pool.add_evm_tx(create_eip1559_tx(
            &ka,
            0,
            1_000_000_000,
            100_000_000,
            21_000,
            U256::ZERO,
        ))
        .unwrap();

        let kb = key(41);
        fund(&state, &address_from_key(&kb), U256::from(10u64.pow(18)), 0);
        pool.add_evm_tx(create_eip1559_tx(
            &kb,
            0,
            5_000_000_000,
            500_000_000,
            21_000,
            U256::ZERO,
        ))
        .unwrap();

        let drained = pool.drain_evm(21_000, B256::ZERO);
        assert_eq!(drained.len(), 1);
        assert_eq!(pool.evm_pool_size(), 1);

        let rest = pool.drain_evm(30_000_000, B256::ZERO);
        assert_eq!(rest.len(), 1);
    }

    #[test]
    fn drain_native_priority() {
        let (_dir, state) = setup();
        let pool = Mempool::new(state, MempoolConfig::default());

        use torus_types::{ActionSignature, NativeAction, Signature};
        let sig = ActionSignature::Eip712(Signature {
            v: 27,
            r: [0u8; 32],
            s: [0u8; 32],
        });
        let sender = Address::repeat_byte(0xAA);

        // Nonces are ms timestamps by protocol convention (admission enforces the
        // window); pool TTL eviction would discard 1970-era synthetic nonces.
        let base = now_ms();
        pool.submit_native_action(
            sender,
            SignedNativeAction {
                action: NativeAction::ClaimRewards,
                nonce: base + 1,
                signature: sig.clone(),
            },
        )
        .unwrap();
        pool.submit_native_action(
            sender,
            SignedNativeAction {
                action: NativeAction::CancelOrder { order_id: 42 },
                nonce: base + 2,
                signature: sig.clone(),
            },
        )
        .unwrap();
        pool.submit_native_action(
            sender,
            SignedNativeAction {
                action: NativeAction::ClaimRewards,
                nonce: base + 3,
                signature: sig,
            },
        )
        .unwrap();

        let drained = pool.drain_native(2);
        assert_eq!(drained.len(), 2);
        assert!(matches!(
            drained[0].action,
            NativeAction::CancelOrder { .. }
        ));
        assert_eq!(pool.native_pool_size(), 1);
    }

    #[test]
    fn reinsert_evm_after_drain() {
        let (_dir, state) = setup();
        let pool = Mempool::new(state.clone(), MempoolConfig::default());

        let k = key(50);
        fund(&state, &address_from_key(&k), U256::from(10u64.pow(18)), 0);
        pool.add_evm_tx(create_eip1559_tx(
            &k,
            0,
            1_000_000_000,
            100_000_000,
            21_000,
            U256::ZERO,
        ))
        .unwrap();

        let drained = pool.drain_evm(30_000_000, B256::ZERO);
        assert_eq!(pool.evm_pool_size(), 0);

        pool.reinsert_evm(drained);
        assert_eq!(pool.evm_pool_size(), 1);
    }

    #[test]
    fn drain_for_block_combined() {
        let (_dir, state) = setup();
        let pool = Mempool::new(state.clone(), MempoolConfig::default());

        let k = key(60);
        fund(&state, &address_from_key(&k), U256::from(10u64.pow(18)), 0);
        pool.add_evm_tx(create_eip1559_tx(
            &k,
            0,
            1_000_000_000,
            100_000_000,
            21_000,
            U256::ZERO,
        ))
        .unwrap();

        let sender = Address::repeat_byte(0xBB);
        pool.submit_native_action(
            sender,
            SignedNativeAction {
                action: torus_types::NativeAction::ClaimRewards,
                // ms-timestamp nonce: survives the pool's TTL eviction.
                nonce: now_ms(),
                signature: torus_types::ActionSignature::Eip712(torus_types::Signature {
                    v: 27,
                    r: [0; 32],
                    s: [0; 32],
                }),
            },
        )
        .unwrap();

        let (native, evm) = pool.drain_for_block(10, 30_000_000, B256::ZERO);
        assert_eq!(evm.len(), 1);
        assert_eq!(native.len(), 1);
        assert_eq!(pool.evm_pool_size(), 0);
        assert_eq!(pool.native_pool_size(), 0);
    }

    #[test]
    fn evm_per_block_cap_enforced() {
        let (_dir, state) = setup();
        let config = MempoolConfig {
            evm_per_block_cap: 2,
            ..MempoolConfig::default()
        };
        let pool = Mempool::new(state.clone(), config);

        let k = key(70);
        let addr = address_from_key(&k);
        fund(&state, &addr, U256::from(10u64.pow(18)), 0);

        for i in 0..5u64 {
            pool.add_evm_tx(create_eip1559_tx(
                &k,
                i,
                1_000_000_000,
                100_000_000,
                21_000,
                U256::ZERO,
            ))
            .unwrap();
        }
        assert_eq!(pool.evm_pool_size(), 5);

        // Per-block cap = 2: only 2 drained, 3 remain.
        let drained = pool.drain_evm(30_000_000, B256::ZERO);
        assert_eq!(drained.len(), 2);
        assert_eq!(pool.evm_pool_size(), 3);
    }

    #[test]
    fn anti_mev_same_gas_different_parent_hash() {
        let (_dir, state) = setup();
        let config = MempoolConfig {
            evm_per_block_cap: 10,
            ..MempoolConfig::default()
        };

        // Create two senders with the same gas price.
        let ka = key(80);
        let kb = key(81);
        fund(&state, &address_from_key(&ka), U256::from(10u64.pow(18)), 0);
        fund(&state, &address_from_key(&kb), U256::from(10u64.pow(18)), 0);

        // Submit with identical gas prices.
        let pool1 = Mempool::new(state.clone(), config.clone());
        pool1
            .add_evm_tx(create_eip1559_tx(&ka, 0, 1_000_000_000, 100_000_000, 21_000, U256::ZERO))
            .unwrap();
        pool1
            .add_evm_tx(create_eip1559_tx(&kb, 0, 1_000_000_000, 100_000_000, 21_000, U256::ZERO))
            .unwrap();

        let pool2 = Mempool::new(state.clone(), config);
        pool2
            .add_evm_tx(create_eip1559_tx(&ka, 0, 1_000_000_000, 100_000_000, 21_000, U256::ZERO))
            .unwrap();
        pool2
            .add_evm_tx(create_eip1559_tx(&kb, 0, 1_000_000_000, 100_000_000, 21_000, U256::ZERO))
            .unwrap();

        // Same parent hash → same ordering (deterministic).
        let hash_a = B256::repeat_byte(0x11);
        let d1 = pool1.drain_evm(30_000_000, hash_a);
        let d2 = pool2.drain_evm(30_000_000, hash_a);
        assert_eq!(d1.len(), 2);
        assert_eq!(d1, d2, "same parent hash must give same order");
    }

    // ---- FIX EVM-FIND-02: memory_used decremented on drain ----

    #[test]
    fn memory_used_decremented_on_drain() {
        let (_dir, state) = setup();
        let config = MempoolConfig {
            max_memory_bytes: 1024 * 1024, // 1 MB
            ..MempoolConfig::default()
        };
        let pool = Mempool::new(state.clone(), config);

        let k = key(90);
        let addr = address_from_key(&k);
        fund(&state, &addr, U256::from(10u64.pow(18)), 0);

        // Add transactions and track memory
        for i in 0..3u64 {
            pool.add_evm_tx(create_eip1559_tx(
                &k, i, 1_000_000_000, 100_000_000, 21_000, U256::ZERO,
            )).unwrap();
        }

        let mem_after_add = pool.memory_used();
        assert!(mem_after_add > 0, "memory should be tracked after adds");

        // Drain all
        let drained = pool.drain_evm(30_000_000, B256::ZERO);
        assert_eq!(drained.len(), 3);

        let mem_after_drain = pool.memory_used();
        assert_eq!(mem_after_drain, 0, "memory should be zero after draining all txs");

        // Add more — must succeed, not PoolFull
        pool.add_evm_tx(create_eip1559_tx(
            &k, 0, 1_000_000_000, 100_000_000, 21_000, U256::ZERO,
        )).unwrap();
        assert_eq!(pool.evm_pool_size(), 1);
    }

    // ---- FIX EVM-FIND-08: future nonce rejection ----

    #[test]
    fn reject_nonce_too_far_in_future() {
        let (_dir, state) = setup();
        let pool = Mempool::new(state.clone(), MempoolConfig::default());

        let k = key(91);
        let addr = address_from_key(&k);
        fund(&state, &addr, U256::from(10u64.pow(18)), 0);

        // Nonce 65 is beyond MAX_NONCE_GAP (64) from state nonce 0
        let raw = create_eip1559_tx(&k, 65, 1_000_000_000, 100_000_000, 21_000, U256::ZERO);
        let err = pool.add_evm_tx(raw).unwrap_err();
        assert!(matches!(err, MempoolError::NonceTooFar { .. }));

        // Nonce 64 should still be accepted
        let raw = create_eip1559_tx(&k, 64, 1_000_000_000, 100_000_000, 21_000, U256::ZERO);
        pool.add_evm_tx(raw).unwrap();
    }

    #[test]
    fn pending_nonce_consecutive() {
        let (_dir, state) = setup();
        let pool = Mempool::new(state.clone(), MempoolConfig::default());
        let k = key(92);
        let addr = address_from_key(&k);
        fund(&state, &addr, U256::from(10u64.pow(18)), 0);

        assert_eq!(pool.pending_nonce(&addr), 0);

        pool.add_evm_tx(create_eip1559_tx(&k, 0, 1_000_000_000, 100_000_000, 21_000, U256::ZERO)).unwrap();
        assert_eq!(pool.pending_nonce(&addr), 1);

        pool.add_evm_tx(create_eip1559_tx(&k, 1, 1_000_000_000, 100_000_000, 21_000, U256::ZERO)).unwrap();
        assert_eq!(pool.pending_nonce(&addr), 2);
    }

    #[test]
    fn pending_nonce_with_gap() {
        let (_dir, state) = setup();
        let pool = Mempool::new(state.clone(), MempoolConfig::default());
        let k = key(93);
        let addr = address_from_key(&k);
        fund(&state, &addr, U256::from(10u64.pow(18)), 0);

        // Insert nonces 0 and 2 (skip 1) — pending nonce should be 1
        pool.add_evm_tx(create_eip1559_tx(&k, 0, 1_000_000_000, 100_000_000, 21_000, U256::ZERO)).unwrap();
        pool.add_evm_tx(create_eip1559_tx(&k, 2, 1_000_000_000, 100_000_000, 21_000, U256::ZERO)).unwrap();
        assert_eq!(pool.pending_nonce(&addr), 1);

        // Fill the gap — now pending nonce should jump to 3
        pool.add_evm_tx(create_eip1559_tx(&k, 1, 1_000_000_000, 100_000_000, 21_000, U256::ZERO)).unwrap();
        assert_eq!(pool.pending_nonce(&addr), 3);
    }

    #[test]
    fn prune_after_block_commit() {
        let (_dir, state) = setup();
        let pool = Mempool::new(state.clone(), MempoolConfig::default());
        let k = key(94);
        let addr = address_from_key(&k);
        fund(&state, &addr, U256::from(10u64.pow(18)), 0);

        for i in 0..4u64 {
            pool.add_evm_tx(create_eip1559_tx(&k, i, 1_000_000_000, 100_000_000, 21_000, U256::ZERO)).unwrap();
        }
        assert_eq!(pool.evm_pool_size(), 4);

        // Simulate block commit: advance state nonce to 2
        fund(&state, &addr, U256::from(10u64.pow(18)), 2);
        pool.notify_block_committed(1, &[addr], &[]);

        // Nonces 0 and 1 should be pruned, 2 and 3 remain
        assert_eq!(pool.evm_pool_size(), 2);
        assert_eq!(pool.pending_nonce(&addr), 4);
    }
}
