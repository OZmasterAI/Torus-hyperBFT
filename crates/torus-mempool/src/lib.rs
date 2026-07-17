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
mod verified_cache;

use std::sync::RwLock;

use alloy_primitives::{Address, B256};
use torus_state::{NativeDaStore, StateDb};
use torus_types::SignedNativeAction;

use crate::verified_cache::FifoCache;

/// Wall-clock milliseconds — the clock native-action nonces are minted from.
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub use crate::error::MempoolError;
pub use crate::evm_pool::EvmPoolEntry;
pub use crate::native_pool::is_cancel;

/// Pure decision for the sojourn admission gate (P3 Round-2 scope 3), factored
/// out for deterministic unit testing. Returns true iff a new non-cancel action
/// should be reject-retryable:
/// - NEVER above the reject when `pool_size < floor` (blocks must never starve).
/// - Boot grace: OFF until the first drain rate is measured (`seeded`).
/// - Divide-by-zero guarded: a deep, non-draining pool (ema≈0) gates.
/// - Otherwise gate when estimated drain time `pool_size / ema_per_ms > cap_ms`.
fn sojourn_should_gate(
    pool_size: usize,
    floor: usize,
    seeded: bool,
    ema_per_ms: f64,
    cap_ms: u64,
) -> bool {
    if pool_size < floor {
        return false;
    }
    if !seeded {
        return false;
    }
    if ema_per_ms <= f64::EPSILON {
        return true;
    }
    (pool_size as f64 / ema_per_ms) > cap_ms as f64
}

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
    /// Initial admission fee floor in wei (D4, S392): txs with `max_fee_per_gas`
    /// below the current base fee are rejected. Updated from committed headers
    /// via [`Mempool::set_base_fee`]; 1 gwei matches the frozen chain base fee.
    pub initial_base_fee: u64,
    // ---- Block-space budgets (D1, S392) ----
    /// Per-block EVM gas budget for drain selection. 5M debut default;
    /// `TORUS_EVM_BLOCK_GAS_BUDGET` env override. The acceptance bench at 15M
    /// earns the raise (docs/plans/evm-blocker-set-decisions.md D1).
    pub evm_block_gas_budget: u64,
    /// Per-sender share of the EVM gas budget, in percent. 0 disables
    /// (mainnet = pure gas budget); `TORUS_EVM_SENDER_SHARE_PCT` env override.
    pub evm_sender_share_pct: u32,
    /// Max native actions per sender per block.
    pub native_per_block_cap: usize,
    /// Max total native pool size.
    pub native_pool_max_size: usize,
    /// Max pending native actions per sender in the pool.
    pub native_per_sender_cap: usize,
    /// Capacity of the exec trust-cache (locally-verified action hash -> sender).
    pub verified_sender_cache_cap: usize,
    /// Capacity of the exec-path session-owner cache (session_pubkey -> SessionData).
    pub session_owner_cache_cap: usize,
    /// Capacity of the exec-path session signature-validity cache
    /// (`session_validity_cache_key` -> presence).
    pub session_sig_cache_cap: usize,
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
            initial_base_fee: 1_000_000_000,
            evm_block_gas_budget: rate_limit::evm_block_gas_budget(),
            evm_sender_share_pct: rate_limit::evm_sender_share_pct(),
            native_per_block_cap: rate_limit::native_per_block_cap(),
            native_pool_max_size: rate_limit::native_pool_max_size(),
            native_per_sender_cap: rate_limit::native_per_sender_cap(),
            verified_sender_cache_cap: rate_limit::VERIFIED_SENDER_CACHE_CAP,
            session_owner_cache_cap: rate_limit::SESSION_OWNER_CACHE_CAP,
            session_sig_cache_cap: rate_limit::SESSION_SIG_CACHE_CAP,
            max_memory_bytes: 64 * 1024 * 1024, // 64 MB default
        }
    }
}

/// Thread-safe transaction mempool combining EVM and native action pools.
pub struct Mempool {
    evm: RwLock<evm_pool::EvmPool>,
    native: RwLock<native_pool::NativePool>,
    state: StateDb,
    /// Durable, nonce-gate-decoupled native-action body store (out-of-band DA).
    da_store: NativeDaStore,
    config: MempoolConfig,
    /// Current base fee in wei — the D4 admission/drain fee floor. Written from
    /// committed block headers; read on every add_evm_tx and drain.
    current_base_fee: std::sync::atomic::AtomicU64,
    /// Approximate total memory used by pooled transactions (Phase 3: 3.1.7).
    memory_used: std::sync::atomic::AtomicUsize,
    /// Outbound native action gossip channel (set once at startup).
    native_gossip_tx: std::sync::OnceLock<tokio::sync::mpsc::Sender<Vec<u8>>>,
    /// When false, native action gossip is suppressed (direct-to-leader mode).
    native_gossip_enabled: std::sync::atomic::AtomicBool,
    /// Node metrics handle (set once at startup; absent in most unit tests).
    metrics: std::sync::OnceLock<std::sync::Arc<torus_telemetry::Metrics>>,
    /// Exec trust-cache: locally-verified action hash -> recovered sender, letting
    /// the execution thread skip a redundant secp256k1 recovery on a HIT. Keyed by
    /// the signature-committing `torus_types::verified_cache_key` (NOT
    /// `compute_action_hash`, which omits the signature). MISS => full recover +
    /// slash. See `docs/plans/double-verify-trust-cache-impl.md`.
    verified_senders: RwLock<FifoCache<B256, Address>>,
    /// Exec-path session-owner cache: `session_pubkey -> SessionData`, letting the
    /// execution thread skip the per-action `get_session` RocksDB read on a HIT.
    /// This memoizes ONLY the DB read — the verify path still re-validates the
    /// cached `SessionData` in full (expiry vs block timestamp, scope, and the
    /// ed25519 signature over the action) on every action, so a HIT can never
    /// accept an expired / out-of-scope / forged action. Correctness across
    /// REVOCATION (and revoke->recreate owner rebinding) is preserved by
    /// `invalidate_session`, called by the exec thread for every CreateSession /
    /// RevokeSession action once the block that mutated the session is flushed
    /// (`put_session`/`delete_session` in native_executor are the ONLY mutators).
    /// Populated only after a real successful `get_session` resolution. A MISS (or
    /// a stale-then-invalidated entry) falls through to the authoritative DB read,
    /// so a HIT is indistinguishable from a MISS => deterministic across validators.
    session_owners: RwLock<FifoCache<[u8; 32], torus_types::SessionData>>,
    /// Exec-path session **signature-validity** cache: presence keyed by
    /// `torus_types::session_validity_cache_key` (which commits to the FULL ed25519
    /// signature + session pubkey). A HIT means "this exact signature was locally
    /// verified here", letting the exec verify path skip the EIP-712 struct/signing
    /// hash + the ed25519 verify for the 100%-session-signed workload. It caches
    /// ONLY the stateless crypto fact — NOT the resolved sender (which is exec-time
    /// state; see `session_validity_cache_key` and the s375 fork note). The exec
    /// verify path STILL re-runs `session_lookup` + expiry + scope on every action,
    /// HIT or MISS, so a HIT can never accept an expired/revoked/out-of-scope/forged
    /// action and is observationally identical to a MISS (deterministic across
    /// validators). Populated ONLY after a successful LOCAL verify (RPC ingress +
    /// gossip-recover); gossip-TRUSTED / block-copy admits are never populated.
    session_sig_verified: RwLock<FifoCache<B256, ()>>,
    /// P3 Round-2 scope 3: sojourn-gate drain estimator. `remove_committed_native`
    /// (consensus thread, sole writer) updates an EMA of the pool drain rate;
    /// the RPC admit path (readers) divides `pool_size` by it to estimate how
    /// long a newly-admitted action would sit before inclusion.
    sojourn: std::sync::Mutex<SojournState>,
}

/// Drain-rate estimator backing the sojourn admission gate (scope 3).
#[derive(Default)]
struct SojournState {
    /// EMA of the pool drain rate in actions-per-millisecond.
    drain_ema_per_ms: f64,
    /// Wall-clock ms of the last `remove_committed_native`; 0 = none yet.
    last_remove_ms: u64,
    /// False until the first drain interval is measured (boot grace).
    seeded: bool,
}

impl Mempool {
    /// Create a new mempool backed by the given state database.
    pub fn new(state: StateDb, config: MempoolConfig) -> Self {
        let native_pool = native_pool::NativePool::new(
            config.native_pool_max_size,
            config.native_per_sender_cap,
            config.native_per_block_cap,
        );
        // DA store shares the same RocksDB handle; every native-action body the
        // mempool sees is mirrored here durably (decoupled from the nonce gate).
        let da_store = NativeDaStore::new(state.clone());
        let verified_cap = config.verified_sender_cache_cap;
        let session_owner_cap = config.session_owner_cache_cap;
        let session_sig_cap = config.session_sig_cache_cap;
        let initial_base_fee = config.initial_base_fee;
        Self {
            evm: RwLock::new(evm_pool::EvmPool::new()),
            native: RwLock::new(native_pool),
            state,
            da_store,
            config,
            current_base_fee: std::sync::atomic::AtomicU64::new(initial_base_fee),
            memory_used: std::sync::atomic::AtomicUsize::new(0),
            native_gossip_tx: std::sync::OnceLock::new(),
            native_gossip_enabled: std::sync::atomic::AtomicBool::new(false),
            metrics: std::sync::OnceLock::new(),
            verified_senders: RwLock::new(FifoCache::new(verified_cap)),
            session_owners: RwLock::new(FifoCache::new(session_owner_cap)),
            session_sig_verified: RwLock::new(FifoCache::new(session_sig_cap)),
            sojourn: std::sync::Mutex::new(SojournState::default()),
        }
    }

    /// Record a locally-verified `key -> sender` mapping in the exec trust-cache.
    ///
    /// `key` MUST be the signature-committing `torus_types::verified_cache_key`
    /// (NOT `compute_action_hash`, which omits the signature) so that a later HIT
    /// can only ever return the sender a fresh recover would. Only the verified
    /// ingress / gossip-recover paths call this; gossip-TRUSTED admits must not.
    pub fn cache_verified_sender(&self, key: B256, sender: Address) {
        let evicted = match self.verified_senders.write() {
            Ok(mut cache) => cache.insert(key, sender),
            Err(_) => return,
        };
        if evicted > 0 {
            if let Some(m) = self.metrics.get() {
                m.verified_sender_cache_evictions.inc_by(evicted);
            }
        }
    }

    /// Look up a locally-verified sender by its signature-committing key. Read-only
    /// (no recency bump). `None` => cache MISS => caller must full recover + slash.
    pub fn verified_sender(&self, key: &B256) -> Option<Address> {
        let result = self
            .verified_senders
            .read()
            .ok()
            .and_then(|cache| cache.get(key).copied());
        if let Some(m) = self.metrics.get() {
            if result.is_some() {
                m.verified_sender_cache_hits.inc();
            } else {
                m.verified_sender_cache_misses.inc();
            }
        }
        result
    }

    /// Record that the ed25519 signature committed by `key`
    /// (`torus_types::session_validity_cache_key`) was locally verified here, so a
    /// later exec-path HIT can skip re-verifying it. `key` MUST commit to the FULL
    /// signature + pubkey; only the LOCAL-verify paths (RPC ingress, gossip-recover)
    /// call this — gossip-TRUSTED / block-copy admits must not, since they did not
    /// verify the signature.
    pub fn cache_session_sig_verified(&self, key: B256) {
        let evicted = match self.session_sig_verified.write() {
            Ok(mut cache) => cache.insert(key, ()),
            Err(_) => return,
        };
        if evicted > 0 {
            if let Some(m) = self.metrics.get() {
                m.session_sig_cache_evictions.inc_by(evicted);
            }
        }
    }

    /// Session sig-validity cache HIT lookup: `true` iff the ed25519 signature
    /// committed by `key` was locally verified. Read-only (no recency bump).
    /// `false` => MISS => the caller MUST run the full ed25519 verify. A HIT lets
    /// the caller skip ONLY the signature verify + its EIP-712 hash — never the
    /// stateful session resolve / expiry / scope checks.
    pub fn session_sig_verified(&self, key: &B256) -> bool {
        let hit = self
            .session_sig_verified
            .read()
            .ok()
            .map(|cache| cache.get(key).is_some())
            .unwrap_or(false);
        if let Some(m) = self.metrics.get() {
            if hit {
                m.session_sig_cache_hits.inc();
            } else {
                m.session_sig_cache_misses.inc();
            }
        }
        hit
    }

    /// Exec-path session-owner cache HIT lookup: return the cached `SessionData`
    /// for `session_pubkey`, avoiding the authoritative `get_session` RocksDB read.
    ///
    /// Read-only (no recency bump). `None` => MISS => the caller MUST resolve via
    /// `get_session` (and then `cache_session_owner`). The returned `SessionData`
    /// MUST still be re-validated by the caller exactly as an uncached resolution
    /// would be (expiry vs block timestamp, scope, and the ed25519 signature over
    /// the action); this method only skips the DB read, never the validation.
    pub fn session_owner(&self, session_pubkey: &[u8; 32]) -> Option<torus_types::SessionData> {
        let result = self
            .session_owners
            .read()
            .ok()
            .and_then(|cache| cache.get(session_pubkey).cloned());
        if let Some(m) = self.metrics.get() {
            if result.is_some() {
                m.session_owner_cache_hits.inc();
            } else {
                m.session_owner_cache_misses.inc();
            }
        }
        result
    }

    /// Populate the session-owner cache after a real successful `get_session`
    /// resolution. MUST be called only with a `SessionData` that the authoritative
    /// state DB just returned for `session_pubkey`, so a later HIT returns exactly
    /// what a fresh `get_session` would (deterministic across validators).
    pub fn cache_session_owner(&self, session_pubkey: [u8; 32], data: torus_types::SessionData) {
        let evicted = match self.session_owners.write() {
            Ok(mut cache) => cache.insert(session_pubkey, data),
            Err(_) => return,
        };
        if evicted > 0 {
            if let Some(m) = self.metrics.get() {
                m.session_owner_cache_evictions.inc_by(evicted);
            }
        }
    }

    /// Invalidate the cached owner for `session_pubkey`. MUST be called by the exec
    /// thread for every CreateSession / RevokeSession action in a committed block
    /// (after the block's state mutations are flushed), because those are the ONLY
    /// operations that change what `get_session` returns. Over-invalidation (e.g. on
    /// a create/revoke that later fails) is safe: the next lookup simply MISSes and
    /// re-resolves from the authoritative DB. Under-invalidation would be a
    /// stale-accept bug, so this is deliberately unconditional on the action kind.
    pub fn invalidate_session(&self, session_pubkey: &[u8; 32]) {
        if let Ok(mut cache) = self.session_owners.write() {
            cache.remove(session_pubkey);
        }
    }

    /// Enable or disable native action gossip at runtime.
    pub fn set_native_gossip_enabled(&self, enabled: bool) {
        self.native_gossip_enabled
            .store(enabled, std::sync::atomic::Ordering::Relaxed);
    }

    /// Set the outbound gossip channel for native actions.
    /// Called once at startup after network initialization.
    pub fn set_native_gossip_tx(&self, tx: tokio::sync::mpsc::Sender<Vec<u8>>) {
        match self.native_gossip_tx.set(tx) {
            Ok(()) => tracing::info!("native action gossip enabled"),
            Err(_) => tracing::warn!("native_gossip_tx already set"),
        }
    }

    /// Set the node metrics handle. Called once at startup.
    pub fn set_metrics(&self, metrics: std::sync::Arc<torus_telemetry::Metrics>) {
        if self.metrics.set(metrics).is_err() {
            tracing::warn!("mempool metrics already set");
        }
    }

    /// Submit a raw RLP-encoded EVM transaction.
    /// Decodes, validates, checks rate limit, and inserts. Returns the tx hash.
    pub fn add_evm_tx(&self, raw_rlp: Vec<u8>) -> Result<B256, MempoolError> {
        // D1 (S392): bound tx gas by the EVM budget, not just the block gas
        // limit — a tx above the budget would pass a bare block-limit check
        // yet sit unselectable in the pool forever (stuck nonce).
        let max_tx_gas = self
            .config
            .block_gas_limit
            .min(self.config.evm_block_gas_budget);
        let entry = validate::validate_evm_tx(
            &raw_rlp,
            &self.state,
            self.config.chain_id,
            max_tx_gas,
            self.current_base_fee
                .load(std::sync::atomic::Ordering::Relaxed),
        )?;

        let hash = entry.hash;
        let tx_size = raw_rlp.len();

        // Memory budget check (Phase 3: 3.1.7).
        if self.config.max_memory_bytes > 0 {
            let current = self.memory_used.load(std::sync::atomic::Ordering::Relaxed);
            if current + tx_size > self.config.max_memory_bytes {
                return Err(MempoolError::PoolFull);
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
    ///
    /// D1 (S392): selection is bounded by the block's EVM gas budget —
    /// min(block gas limit, configured budget; 5M debut default,
    /// `TORUS_EVM_BLOCK_GAS_BUDGET` override) — with an optional per-sender
    /// share cap. Count caps deleted. Respects per-sender nonce ordering and
    /// removes selected txs from the pool.
    ///
    /// `parent_hash` seeds deterministic same-price shuffling (anti-MEV, Task 3.1.5).
    pub fn drain_evm(&self, gas_limit: u64, parent_hash: B256) -> Vec<Vec<u8>> {
        let budget = gas_limit.min(self.config.evm_block_gas_budget);
        // Per-sender share cap (testnet spam guard; 0 = off for mainnet).
        let sender_gas_cap = match self.config.evm_sender_share_pct {
            0 => None,
            pct => Some(budget.saturating_mul(pct as u64) / 100),
        };
        // D4 re-check: the floor may have moved since admission.
        let min_base_fee = self
            .current_base_fee
            .load(std::sync::atomic::Ordering::Relaxed) as u128;
        let mut pool = self.evm.write().unwrap();
        let drained = pool.drain(budget, sender_gas_cap, &parent_hash, min_base_fee);
        // FIX EVM-FIND-02: Decrement memory for drained transactions.
        let drained_bytes: usize = drained.iter().map(|tx| tx.len()).sum();
        if drained_bytes > 0 {
            self.memory_used
                .fetch_sub(drained_bytes, std::sync::atomic::Ordering::Relaxed);
        }
        drained
    }

    /// Update the D4 fee floor from a committed block header's base fee.
    pub fn set_base_fee(&self, base_fee: u64) {
        self.current_base_fee
            .store(base_fee, std::sync::atomic::Ordering::Relaxed);
    }

    /// Current D4 fee floor in wei.
    pub fn base_fee(&self) -> u64 {
        self.current_base_fee
            .load(std::sync::atomic::Ordering::Relaxed)
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
    ///
    /// Verified gossip ingest, authenticate the CLAIMED sender before pool
    /// admission, since a malicious peer could otherwise gossip forged
    /// `(sender, action)` pairs into every validator's pool. Eip712: recovered
    /// signer must equal the claim. Session: signature must verify and the
    /// registered session owner must equal the claim.
    ///
    /// P3 Round-2 (scope 1): DA-mirroring is NO LONGER done here. The inbound
    /// mirror was hoisted AHEAD of this verify FIFO into the off-loop mirror
    /// worker (`mirror_native_to_da` at network receipt, torus-node), so a
    /// block-referenced body becomes DA-resident immediately even while this
    /// (slow) verify queue is backed up — the availability-starvation stall the
    /// round removes (mem 28e1a821). Callers on the gossip ingest path MUST have
    /// mirrored the body before calling this (the mirror worker does). The
    /// availability ≠ validity split is unchanged: the mirror never trusts the
    /// body for execution/pool admission; the crypto verify below is the gate.
    pub fn add_native_action_from_gossip(
        &self,
        claimed_sender: alloy_primitives::Address,
        action: SignedNativeAction,
    ) -> Result<(), MempoolError> {
        // P3 Round-2 scope 2: PRESCREEN before the expensive crypto verify. The
        // body is already DA-mirrored (receipt worker), so both skip paths keep
        // it reconstructable (mem 28e1a821); they only avoid paying verify.
        // (1) Compute the canonical action hash ONCE — reused by the dedup check
        //     AND the pool insert (invariant 2: identical to today's hash).
        let action_hash = torus_types::compute_action_hash(&action);
        // (2) DEDUP: an identical action already pooled or recently committed is a
        //     no-op — skip verify, return Ok, pool unchanged. NEVER seeds the
        //     trust-cache (invariant 3): we return before any insert. Also kills
        //     duplicate block inclusion (the recently-committed ring).
        if self
            .native
            .read()
            .unwrap()
            .contains_or_recently_committed(&action_hash)
        {
            if let Some(m) = self.metrics.get() {
                m.native_ingest_dedup_skips.inc();
            }
            return Ok(());
        }
        // (3) STALENESS: past the 60s nonce window — skip verify (the body stays
        //     DA-resident from the receipt mirror). NONCE_WINDOW_MS untouched.
        use torus_types::eip712::NONCE_WINDOW_MS;
        if action.nonce.saturating_add(NONCE_WINDOW_MS) < now_ms() {
            if let Some(m) = self.metrics.get() {
                m.native_ingest_stale_skips.inc();
            }
            return Err(MempoolError::NativeValidationFailed(
                "nonce too old (>60s, prescreened)".into(),
            ));
        }

        // (4) Crypto verify the CLAIMED sender (forged gossip pairs must not
        //     pollute the pool).
        let verified_sender = match &action.signature {
            torus_types::ActionSignature::Eip712(_) => action.recover_sender().map_err(|e| {
                MempoolError::NativeValidationFailed(format!("gossip sig recovery: {e}"))
            })?,
            torus_types::ActionSignature::Session { .. } => {
                let pubkey = action.verify_session_signature().map_err(|e| {
                    MempoolError::NativeValidationFailed(format!("gossip session sig: {e}"))
                })?;
                match self.state.get_session(&pubkey) {
                    Ok(Some(session)) => session.owner,
                    _ => {
                        return Err(MempoolError::NativeValidationFailed(
                            "gossip session unknown".into(),
                        ))
                    }
                }
            }
        };
        if verified_sender != claimed_sender {
            return Err(MempoolError::NativeValidationFailed(
                "gossip sender mismatch".into(),
            ));
        }
        // This node re-derived the sender from the signature above -> locally
        // verified, so the entry may seed the exec trust-cache. Reuse the
        // precomputed hash for the insert (stops the third recompute).
        self.admit_gossip_with_hash(claimed_sender, action, true, Some(action_hash))
    }

    pub fn add_native_action_from_gossip_trusted(
        &self,
        sender: alloy_primitives::Address,
        action: SignedNativeAction,
    ) -> Result<(), MempoolError> {
        // Sender is CLAIMED by the peer and NOT re-derived here -> not locally
        // verified; must never be short-circuited at exec.
        self.admit_gossip(sender, action, false)
    }

    /// Shared gossip admission: nonce-window gate + pool insert with the given
    /// provenance. `verified_locally` distinguishes the recover path (true, seeds
    /// the trust-cache) from the raw trusted path (false).
    ///
    /// P3 Round-2 (scope 1): the durable DA mirror that used to run HERE (once
    /// per gossip action, decoupled from the nonce gate) was hoisted ahead of the
    /// verify FIFO into the off-loop mirror worker at network receipt. That kept
    /// the exact same nonce-gate-DECOUPLED semantics — a stale body is still
    /// mirrored before this admission rejects it — but moved it off the slow
    /// verify path and deleted the old per-action DOUBLE put (mem 28e1a821). The
    /// gossip ingest callers MUST mirror before calling this.
    fn admit_gossip(
        &self,
        sender: alloy_primitives::Address,
        action: SignedNativeAction,
        verified_locally: bool,
    ) -> Result<(), MempoolError> {
        self.admit_gossip_with_hash(sender, action, verified_locally, None)
    }

    /// `admit_gossip` variant that threads the gossip prescreen's precomputed
    /// action hash (scope 2) into the pool insert so it is not recomputed.
    fn admit_gossip_with_hash(
        &self,
        sender: alloy_primitives::Address,
        action: SignedNativeAction,
        verified_locally: bool,
        precomputed_hash: Option<B256>,
    ) -> Result<(), MempoolError> {
        // G1 defensive layer (O2): an oversize/empty batch must never enter
        // the POOL (an honest node must never SELECT it into a proposal). It is
        // still DA-mirrored at network receipt (by the off-loop mirror worker,
        // BEFORE this admission runs): a malicious proposer may reference it, and
        // the block must stay reconstructable so the deterministic exec-side skip
        // can run (availability != validity, mem 28e1a821). Covers every network
        // ingest: gossip topic, pre-proposal full-body push, direct forward, and
        // the gossip-trusted path.
        crate::rate_limit::validate_batch_size(&action.action)
            .map_err(MempoolError::NativeValidationFailed)?;

        let current_time_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock before epoch")
            .as_millis() as u64;
        use torus_types::eip712::NONCE_WINDOW_MS;
        if action.nonce.saturating_add(NONCE_WINDOW_MS) < current_time_ms {
            return Err(MempoolError::NativeValidationFailed(
                "nonce too old (>60s)".into(),
            ));
        }
        if action.nonce > current_time_ms.saturating_add(NONCE_WINDOW_MS) {
            return Err(MempoolError::NativeValidationFailed(
                "nonce too far in future".into(),
            ));
        }
        self.submit_native_action_inner(sender, action, verified_locally, precomputed_hash)
    }

    /// Gossip includes sender address so receivers can skip ECDSA recovery.
    /// Disabled when direct-to-leader forwarding is active (avoids flooding
    /// the GossipSub mesh and starving consensus messages under load).
    fn gossip_native_action(&self, sender: alloy_primitives::Address, action: &SignedNativeAction) {
        if !self
            .native_gossip_enabled
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            return;
        }
        if let Some(tx) = self.native_gossip_tx.get() {
            if let Ok(bytes) = bincode::serialize(&(sender, action)) {
                match tx.try_send(bytes) {
                    Ok(()) => {}
                    Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                        if let Some(m) = self.metrics.get() {
                            m.native_gossip_dropped_full.inc();
                        }
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
    ///
    /// The public form treats the sender as locally verified (its documented
    /// contract), so the entry is eligible to seed the exec trust-cache.
    pub fn submit_native_action(
        &self,
        sender: alloy_primitives::Address,
        action: SignedNativeAction,
    ) -> Result<(), MempoolError> {
        self.submit_native_action_inner(sender, action, true, None)
    }

    /// Native insert with explicit provenance. `verified_locally` records whether
    /// THIS node verified the signature and resolved `sender` from it (RPC ingress
    /// or gossip-RECOVER). Only locally-verified EIP-712 actions seed the exec
    /// trust-cache (keyed by the signature-committing `verified_cache_key`);
    /// gossip-TRUSTED admits pass `false` and are never short-circuited at exec.
    ///
    /// `precomputed_hash` (P3 Round-2 scope 2) is the gossip prescreen's
    /// already-computed `compute_action_hash`; when `Some`, insertion reuses it
    /// instead of recomputing (must equal the canonical hash — invariant 2). RPC
    /// ingress passes `None`.
    fn submit_native_action_inner(
        &self,
        sender: alloy_primitives::Address,
        action: SignedNativeAction,
        verified_locally: bool,
        precomputed_hash: Option<B256>,
    ) -> Result<(), MempoolError> {
        // Derive the trust-cache key BEFORE `action` is moved into the pool. `None`
        // for non-EIP-712 (session) actions, which are never short-circuited.
        let cache_key = if verified_locally {
            torus_types::verified_cache_key(&action)
        } else {
            None
        };
        // Feature #13: derive the session sig-validity key (Some only for session
        // actions). `verified_locally` means THIS node re-derived the sender from
        // the signature (RPC ingress or gossip-recover), so the ed25519 session
        // signature was locally verified and its validity may be cached. A
        // gossip-TRUSTED / block-copy admit passes `false` and is never cached —
        // the same populate-on-verify invariant as the EIP-712 trust-cache.
        let sig_key = if verified_locally {
            torus_types::session_validity_cache_key(&action)
        } else {
            None
        };

        let size_after = {
            let mut pool = self.native.write().unwrap();
            match precomputed_hash {
                Some(h) => {
                    pool.insert_verified_with_hash(sender, action, verified_locally, h)?;
                }
                None => {
                    pool.insert_verified(sender, action, verified_locally)?;
                }
            }
            pool.size()
        };

        // P3 Round-1 item 1: a working pool-occupancy gauge (the P2 report found
        // `mempool_native_size` mis-wired — `.set()` called NOWHERE — reading 0 at
        // every 1s sample while thousands of actions were pooled) plus an
        // insert-success counter, so the intake identity closes per cell.
        if let Some(m) = self.metrics.get() {
            m.native_pool_inserted.inc();
            m.mempool_native_size.set(size_after as i64);
        }

        // Seed only after a successful insert, so a rejected (dup/full) action
        // never pollutes the cache.
        if let Some(key) = cache_key {
            self.cache_verified_sender(key, sender);
        }
        if let Some(key) = sig_key {
            self.cache_session_sig_verified(key);
        }
        tracing::debug!(%sender, "native action added to mempool");
        Ok(())
    }

    /// P3 Round-1 item 1: refresh `torus_mempool_native_size` from the current
    /// pool occupancy. Called after every pool mutation that is NOT already
    /// gated behind a size read (insert refreshes inline). Cheap: one read-lock
    /// + one atomic store when metrics are wired (absent in unit tests).
    fn refresh_native_size_gauge(&self) {
        if let Some(m) = self.metrics.get() {
            let size = self.native.read().unwrap().size();
            m.mempool_native_size.set(size as i64);
        }
    }

    /// Drain native actions for a block proposal.
    /// Returns up to `limit` actions in priority order (cancels first).
    /// Per-sender-per-block caps are enforced internally.
    pub fn drain_native(&self, limit: usize) -> Vec<SignedNativeAction> {
        let (drained, size_after) = {
            let mut pool = self.native.write().unwrap();
            let (evicted, expired_orders) = pool.evict_expired(now_ms());
            self.count_expired(evicted, expired_orders);
            let drained = pool.drain(limit);
            (drained, pool.size())
        };
        if let Some(m) = self.metrics.get() {
            m.mempool_native_size.set(size_after as i64);
        }
        drained
    }

    /// P3 Round-1 item 2: continuous-expiry tick. A 1s cadence caller (the
    /// torus-node runtime) invokes this so nonce-window eviction fires steadily
    /// instead of as a single lump the next time a proposer happens to select —
    /// which is what made `pool_expired_*` appear only at cooldown in P2 and left
    /// the intake identity 22–55% unattributed mid-run. Purely additive: the
    /// lazy evictions in `drain_native` / selection remain as a safety net, so
    /// reverting to lazy-only expiry is just deleting the tick task.
    pub fn tick_expiry(&self) {
        let (evicted, expired_orders, size_after) = {
            let mut pool = self.native.write().unwrap();
            let (evicted, expired_orders) = pool.evict_expired(now_ms());
            (evicted, expired_orders, pool.size())
        };
        self.count_expired(evicted, expired_orders);
        if let Some(m) = self.metrics.get() {
            m.mempool_native_size.set(size_after as i64);
        }
    }

    /// Count a nonce-expiry purge on the P2 funnel counters (the 60s window is
    /// otherwise a silent loss sink — the pool "drains to 0" without any trace).
    fn count_expired(&self, evicted: usize, expired_orders: u64) {
        if evicted == 0 {
            return;
        }
        if let Some(m) = self.metrics.get() {
            m.native_pool_expired_actions.inc_by(evicted as u64);
            m.native_pool_expired_orders.inc_by(expired_orders);
        }
        tracing::info!(
            evicted,
            expired_orders,
            "evicted nonce-expired native actions from pool"
        );
    }

    /// Re-insert previously drained native actions (e.g., after reorg).
    pub fn reinsert_native(&self, actions: Vec<SignedNativeAction>) {
        // Keep the DA-store invariant: every pooled body stays reconstructable.
        self.mirror_native_to_da(&actions);
        let size_after = {
            let mut pool = self.native.write().unwrap();
            pool.reinsert(actions);
            pool.size()
        };
        if let Some(m) = self.metrics.get() {
            m.mempool_native_size.set(size_after as i64);
        }
    }

    /// Current native pool size.
    pub fn native_pool_size(&self) -> usize {
        self.native.read().unwrap().size()
    }

    /// True when the native pool is at capacity (non-cancel admits will fail).
    pub fn native_pool_is_full(&self) -> bool {
        self.native.read().unwrap().is_full()
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

    /// RH4 body GC: delete durable DA bodies whose commit height has aged past
    /// the retention window. Returns the number of delete tombstones issued.
    /// Best-effort: a write failure is logged, never propagated (GC must never
    /// disturb the commit path). See [`NativeDaStore::remove_batch`].
    pub fn gc_native_da_bodies(&self, hashes: &[B256]) -> usize {
        match self.da_store.remove_batch(hashes) {
            Ok(n) => n,
            Err(e) => {
                tracing::error!("native DA body GC failed: {e}");
                0
            }
        }
    }

    /// Mirror native-action bodies into the durable DA store (proposer guarantee:
    /// every body referenced by a block we propose stays reconstructable).
    /// One atomic WriteBatch + one arrival-notifier wake for the whole block —
    /// this runs on the leader's produce_block critical path (S395).
    pub fn mirror_native_to_da(&self, actions: &[SignedNativeAction]) {
        if let Err(e) = self.da_store.put_batch(actions) {
            tracing::error!("native DA store batch write failed: {e}");
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

    pub fn select_native_for_block_with_senders(
        &self,
        limit: usize,
    ) -> Vec<(alloy_primitives::Address, SignedNativeAction)> {
        self.native
            .write()
            .unwrap()
            .select_for_block_with_senders(limit)
    }

    /// Pipeline-aware variant: skips actions whose hash is in `exclude` (the
    /// proposer's in-flight, proposed-but-uncommitted action hashes). Prevents the
    /// same action being re-selected for blocks N+1/N+2 before N commits — the root
    /// cause of duplicate native inclusion.
    /// `bytes_cap` bounds the summed encoded size of the selected bodies (WAN
    /// dissemination budget — see `rate_limit::NATIVE_BLOCK_BYTES_CAP`).
    /// `orders_cap` bounds total orders via `order_count` (`NATIVE_ORDERS_PER_BLOCK_CAP`).
    pub fn select_native_for_block_with_senders_excluding(
        &self,
        limit: usize,
        exclude: &std::collections::HashSet<B256>,
        bytes_cap: usize,
        orders_cap: usize,
    ) -> Vec<(alloy_primitives::Address, SignedNativeAction)> {
        let (selected, size_after) = {
            let mut pool = self.native.write().unwrap();
            let (evicted, expired_orders) = pool.evict_expired(now_ms());
            self.count_expired(evicted, expired_orders);
            let selected =
                pool.select_for_block_with_senders_excluding(limit, exclude, bytes_cap, orders_cap);
            (selected, pool.size())
        };
        if let Some(m) = self.metrics.get() {
            m.mempool_native_size.set(size_after as i64);
        }
        selected
    }

    /// Remove native actions that were included in a committed block.
    ///
    /// Before pruning, refresh-stash each locally-verified sender into the exec
    /// trust-cache so it survives long enough for the exec thread (which lags up to
    /// the exec-queue depth behind commit) to read it on a HIT. These committed
    /// actions are pruned here on the consensus thread BEFORE the block crosses the
    /// exec channel, so without this restash a live pool lookup at exec would miss
    /// (the prune-before-exec trap, mem a644ca0a). Read-then-write on the single
    /// consensus thread; concurrent peers only insert, never remove, so the
    /// captured entries are still present at prune time.
    pub fn remove_committed_native(&self, hashes: &[B256]) {
        let restash = {
            let pool = self.native.read().unwrap();
            pool.verified_restash_keys(hashes)
        };
        for (key, sender) in restash {
            self.cache_verified_sender(key, sender);
        }
        self.native.write().unwrap().remove_committed(hashes);
        // P3 Round-2 scope 3: feed the sojourn-gate drain estimator. Commit
        // removal is the pool's drain event, so its rate (actions per ms) is the
        // rate the gate compares pool depth against.
        self.record_drain(hashes.len());
        // P3 Round-1 item 1: commit removal is a pool mutation — refresh the gauge.
        self.refresh_native_size_gauge();
    }

    /// Update the sojourn-gate drain EMA on a commit-drain of `drained` actions
    /// (scope 3). Single-writer (consensus thread). The first interval seeds the
    /// EMA directly; subsequent intervals blend with α=0.2. A zero-length or
    /// zero-elapsed interval is ignored (divide-by-zero guarded).
    fn record_drain(&self, drained: usize) {
        let now = now_ms();
        let mut st = self.sojourn.lock().unwrap();
        if st.last_remove_ms > 0 && drained > 0 {
            let dt = now.saturating_sub(st.last_remove_ms).max(1) as f64;
            let rate = drained as f64 / dt; // actions per ms this interval
            const ALPHA: f64 = 0.2;
            st.drain_ema_per_ms = if st.seeded {
                ALPHA * rate + (1.0 - ALPHA) * st.drain_ema_per_ms
            } else {
                rate
            };
            st.seeded = true;
        }
        st.last_remove_ms = now;
    }

    /// True when the RPC admit path should reject-RETRYABLE a new non-cancel
    /// action because the pool's estimated drain time exceeds the sojourn cap
    /// (scope 3). Guarantees blocks never starve: the pool must first be at least
    /// `2 × per-block cap` deep (the FLOOR) before the gate can fire. Below the
    /// floor, or before any drain is measured (boot grace), the gate is OFF, so
    /// default behaviour is unchanged until the pool is genuinely deep and
    /// draining slower than it fills. Divide-by-zero guarded.
    pub fn sojourn_gate_active(&self) -> bool {
        let pool_size = self.native_pool_size();
        let (seeded, ema) = {
            let st = self.sojourn.lock().unwrap();
            (st.seeded, st.drain_ema_per_ms)
        };
        sojourn_should_gate(
            pool_size,
            rate_limit::native_total_block_cap().saturating_mul(2),
            seeded,
            ema,
            rate_limit::pool_sojourn_cap_ms(),
        )
    }

    /// Approximate total memory used by pooled transactions (Phase 3: 3.1.7).
    pub fn memory_used(&self) -> usize {
        self.memory_used.load(std::sync::atomic::Ordering::Relaxed)
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

    /// Prune stale transactions from senders included in a committed block —
    /// txs whose nonce is now below the sender's confirmed state nonce.
    /// (D1, S392: the dormant sliding-window rate tracker this also fed was
    /// deleted; its feed was test-only.)
    pub fn notify_block_committed(&self, evm_senders: &[alloy_primitives::Address]) {
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
            tracing::info!(
                pruned = total_pruned,
                senders = senders.len(),
                "pruned stale txs on block commit"
            );
        }
    }
}

#[cfg(test)]
mod sojourn_tests {
    use super::sojourn_should_gate;

    // P3 Round-2 scope 3: the sojourn gate rejects when the estimated drain time
    // exceeds the cap, NEVER below the 2×-block-cap floor, and is off during boot
    // grace / guarded against divide-by-zero.
    const FLOOR: usize = 200; // 2 × the total block cap (100)
    const CAP_MS: u64 = 25_000;

    #[test]
    fn never_gates_below_floor_even_with_stalled_drain() {
        // Below the floor, no matter how slow the drain (even 0), blocks must
        // always keep candidates -> gate OFF.
        assert!(!sojourn_should_gate(FLOOR - 1, FLOOR, true, 0.0, CAP_MS));
        assert!(!sojourn_should_gate(0, FLOOR, true, 0.0001, CAP_MS));
    }

    #[test]
    fn boot_grace_gate_off_until_seeded() {
        // Deep pool but no drain measured yet -> OFF (don't gate on no data).
        assert!(!sojourn_should_gate(10_000, FLOOR, false, 0.0, CAP_MS));
    }

    #[test]
    fn gates_above_floor_when_sojourn_exceeds_cap() {
        // 10_000 actions draining at 0.1/ms => 100_000 ms sojourn > 25_000 cap.
        assert!(sojourn_should_gate(10_000, FLOOR, true, 0.1, CAP_MS));
    }

    #[test]
    fn does_not_gate_when_draining_fast_enough() {
        // 10_000 actions draining at 1/ms => 10_000 ms sojourn < 25_000 cap.
        assert!(!sojourn_should_gate(10_000, FLOOR, true, 1.0, CAP_MS));
    }

    #[test]
    fn divide_by_zero_deep_stalled_pool_gates() {
        // Above floor, seeded, but drain EMA ~0 (pool not draining) -> gate,
        // no panic.
        assert!(sojourn_should_gate(FLOOR + 1, FLOOR, true, 0.0, CAP_MS));
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

    #[test]
    fn gossip_ingest_verifies_claimed_sender() {
        let (_dir, state) = setup();
        let pool = Mempool::new(state, MempoolConfig::default());
        // Hardhat account 0 — a real key so signature recovery yields a real sender.
        let key = k256::ecdsa::SigningKey::from_slice(
            &alloy_primitives::hex::decode(
                "ac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80",
            )
            .unwrap(),
        )
        .unwrap();
        let now = now_ms();
        let signed = torus_types::eip712::sign_native_action(
            torus_types::NativeAction::ClaimRewards,
            now,
            &key,
        );
        let real_sender = signed.recover_sender().unwrap();
        let hash = torus_types::compute_action_hash(&signed);

        // A peer gossiping a forged (sender, action) pair must not pollute the
        // pool — but the body stays DA-mirrored (availability ≠ validity).
        // P3 Round-2 scope 1: the mirror is now the off-loop worker's job
        // (mirror-at-receipt), modelled here by an explicit mirror stage BEFORE
        // the admit/verify stage.
        pool.mirror_native_to_da(std::slice::from_ref(&signed));
        let forged = alloy_primitives::Address::repeat_byte(0xEE);
        assert!(pool
            .add_native_action_from_gossip(forged, signed.clone())
            .is_err());
        assert_eq!(
            pool.native_pool_size(),
            0,
            "forged sender must not enter pool"
        );
        assert!(
            pool.get_native_da(&hash).is_some(),
            "body still DA-mirrored"
        );

        // The genuine sender is admitted.
        pool.add_native_action_from_gossip(real_sender, signed)
            .unwrap();
        assert_eq!(pool.native_pool_size(), 1);
    }

    #[test]
    fn gossip_admit_rejects_oversize_and_empty_batch_but_keeps_da_mirror() {
        use crate::rate_limit::NATIVE_ORDERS_PER_BATCH_CAP;
        let (_dir, state) = setup();
        let pool = Mempool::new(state, MempoolConfig::default());
        let key = k256::ecdsa::SigningKey::from_slice(
            &alloy_primitives::hex::decode(
                "ac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80",
            )
            .unwrap(),
        )
        .unwrap();
        let now = now_ms();
        let params = torus_types::PlaceOrderParams {
            market_id: 1,
            is_buy: true,
            price: torus_types::FixedPoint::from_raw(6_500_000_000_000),
            quantity: torus_types::FixedPoint::from_raw(10_000_000),
            order_type: torus_types::OrderType::Limit,
            time_in_force: torus_types::TimeInForce::GTC,
            reduce_only: false,
            client_order_id: None,
        };

        // Oversize batch: rejected by BOTH gossip paths, pool stays empty,
        // but the body IS DA-mirrored (availability != validity — a block
        // referencing it must stay reconstructable for the exec-side skip).
        let over = torus_types::eip712::sign_native_action(
            torus_types::NativeAction::PlaceOrderBatch(vec![
                params.clone();
                NATIVE_ORDERS_PER_BATCH_CAP + 1
            ]),
            now,
            &key,
        );
        let sender = over.recover_sender().unwrap();
        let over_hash = torus_types::compute_action_hash(&over);
        // P3 Round-2 scope 1: the receipt mirror worker mirrors EVERY decoded
        // body (no validate_batch_size gate on the mirror), so an oversize batch
        // is still reconstructable even though pool admission rejects it. Model
        // that mirror stage explicitly.
        pool.mirror_native_to_da(std::slice::from_ref(&over));
        assert!(pool
            .add_native_action_from_gossip(sender, over.clone())
            .is_err());
        assert!(pool
            .add_native_action_from_gossip_trusted(sender, over)
            .is_err());
        assert_eq!(
            pool.native_pool_size(),
            0,
            "oversize batch must never enter the pool"
        );
        assert!(
            pool.get_native_da(&over_hash).is_some(),
            "body still DA-mirrored"
        );

        // Empty batch: same rejection.
        let empty = torus_types::eip712::sign_native_action(
            torus_types::NativeAction::PlaceOrderBatch(vec![]),
            now + 1,
            &key,
        );
        assert!(pool.add_native_action_from_gossip(sender, empty).is_err());
        assert_eq!(pool.native_pool_size(), 0);

        // At-cap batch: admitted (boundary).
        let at_cap = torus_types::eip712::sign_native_action(
            torus_types::NativeAction::PlaceOrderBatch(vec![params; NATIVE_ORDERS_PER_BATCH_CAP]),
            now + 2,
            &key,
        );
        pool.add_native_action_from_gossip(sender, at_cap).unwrap();
        assert_eq!(pool.native_pool_size(), 1);
    }

    #[test]
    fn trust_cache_populated_on_verified_paths_not_on_trusted() {
        let (_dir, state) = setup();
        let pool = Mempool::new(state, MempoolConfig::default());
        // Real key so EIP-712 recovery yields a real sender (EIP-712 is the only
        // cacheable signature kind — its sender is a stateless function of the sig).
        let k = k256::ecdsa::SigningKey::from_slice(
            &alloy_primitives::hex::decode(
                "ac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80",
            )
            .unwrap(),
        )
        .unwrap();
        let base = now_ms();

        // (a) RPC presigned ingress -> locally verified -> cached.
        let a1 = torus_types::eip712::sign_native_action(
            torus_types::NativeAction::ClaimRewards,
            base + 1,
            &k,
        );
        let s1 = a1.recover_sender().unwrap();
        let key1 = torus_types::verified_cache_key(&a1).expect("eip712 is cacheable");
        pool.add_native_action_presigned(s1, a1).unwrap();
        assert_eq!(
            pool.verified_sender(&key1),
            Some(s1),
            "presigned ingress must seed the trust-cache"
        );

        // (b) gossip RECOVER (sig re-derived here) -> locally verified -> cached.
        let a2 = torus_types::eip712::sign_native_action(
            torus_types::NativeAction::ClaimRewards,
            base + 2,
            &k,
        );
        let s2 = a2.recover_sender().unwrap();
        let key2 = torus_types::verified_cache_key(&a2).expect("eip712 is cacheable");
        pool.add_native_action_from_gossip(s2, a2).unwrap();
        assert_eq!(
            pool.verified_sender(&key2),
            Some(s2),
            "gossip-recover must seed the trust-cache"
        );

        // (c) gossip TRUSTED (sender claimed by peer, NOT re-derived) -> NOT cached.
        let a3 = torus_types::eip712::sign_native_action(
            torus_types::NativeAction::ClaimRewards,
            base + 3,
            &k,
        );
        let s3 = a3.recover_sender().unwrap();
        let key3 = torus_types::verified_cache_key(&a3).expect("eip712 is cacheable");
        pool.add_native_action_from_gossip_trusted(s3, a3).unwrap();
        assert_eq!(
            pool.verified_sender(&key3),
            None,
            "gossip-trusted must NOT seed the trust-cache (verified_locally=false)"
        );
    }

    #[test]
    fn session_owner_cache_hit_miss_and_invalidate() {
        let (_dir, state) = setup();
        let pool = Mempool::new(state, MempoolConfig::default());

        let pk = [0x42u8; 32];
        let data = torus_types::SessionData {
            owner: alloy_primitives::Address::from([0xAA; 20]),
            expiry: 9_999,
            scope: torus_types::SessionScope::Trading,
            created_at: 1,
        };

        // MISS before populate.
        assert_eq!(pool.session_owner(&pk), None, "cold lookup MISSes");

        // Populate -> HIT returns the exact SessionData a get_session would.
        pool.cache_session_owner(pk, data.clone());
        assert_eq!(pool.session_owner(&pk), Some(data.clone()), "warm lookup HITs");

        // Invalidate (models a revoke / create in a committed block) -> MISS again,
        // forcing a re-resolve from the authoritative DB on the next verify.
        pool.invalidate_session(&pk);
        assert_eq!(
            pool.session_owner(&pk),
            None,
            "invalidated session MISSes (no stale HIT after revoke)"
        );

        // Re-populate with a DIFFERENT owner (revoke->recreate rebinding) -> HIT now
        // returns the NEW owner, never the stale one.
        let rebound = torus_types::SessionData {
            owner: alloy_primitives::Address::from([0xBB; 20]),
            ..data
        };
        pool.cache_session_owner(pk, rebound.clone());
        assert_eq!(
            pool.session_owner(&pk),
            Some(rebound),
            "rebound session resolves to the new owner"
        );
    }

    #[test]
    fn session_owner_cache_bounded_by_cap() {
        let (_dir, state) = setup();
        // Cap 2 => inserting a 3rd distinct session evicts the oldest.
        let config = MempoolConfig {
            session_owner_cache_cap: 2,
            ..MempoolConfig::default()
        };
        let pool = Mempool::new(state, config);

        let mk = |n: u8| torus_types::SessionData {
            owner: alloy_primitives::Address::from([n; 20]),
            expiry: u64::MAX,
            scope: torus_types::SessionScope::Full,
            created_at: 0,
        };
        pool.cache_session_owner([1u8; 32], mk(1));
        pool.cache_session_owner([2u8; 32], mk(2));
        pool.cache_session_owner([3u8; 32], mk(3)); // evicts oldest ([1])

        assert_eq!(pool.session_owner(&[1u8; 32]), None, "oldest evicted at cap");
        assert!(pool.session_owner(&[2u8; 32]).is_some());
        assert!(pool.session_owner(&[3u8; 32]).is_some());
    }

    /// Feature #13 — the load-bearing populate-on-verify invariant: a session
    /// action admitted via a LOCAL-verify path (RPC presigned / gossip-recover)
    /// caches its signature validity, but a gossip-TRUSTED admit (sender claimed,
    /// signature NOT re-verified here) must NEVER populate it. The exec HIT path
    /// skips ed25519 verification, so a forged/unverified sig must never reach the
    /// cache — same invariant as the EIP-712 trust-cache.
    #[test]
    fn session_sig_cache_populate_on_local_verify_only() {
        let (_dir, state) = setup();
        let pool = Mempool::new(state, MempoolConfig::default());
        let base = now_ms();
        let owner = alloy_primitives::Address::from([0xEE; 20]);

        // Build a session-signed action directly — the admit paths under test do
        // NOT verify the ed25519 signature (that is the caller's job); this test
        // isolates the populate DISCIPLINE (verified_locally gate), and the sig
        // key commits to the raw bytes regardless of validity.
        let mk = |order_id: u128, nonce: u64| torus_types::SignedNativeAction {
            action: torus_types::NativeAction::CancelOrder { order_id },
            nonce,
            signature: torus_types::ActionSignature::Session {
                session_pubkey: [7u8; 32],
                sig: torus_types::Ed25519Sig([9u8; 64]),
            },
        };

        // Local-verify path: RPC presigned admit populates the sig cache.
        let a = mk(9, base + 1);
        let key_a = torus_types::session_validity_cache_key(&a).expect("session is keyed");
        assert!(!pool.session_sig_verified(&key_a), "cold: not yet cached");
        pool.add_native_action_presigned(owner, a).unwrap();
        assert!(
            pool.session_sig_verified(&key_a),
            "local verify (presigned) must populate the sig cache"
        );

        // Gossip-TRUSTED path: sender claimed, sig not re-verified => NOT cached.
        let b = mk(10, base + 2);
        let key_b = torus_types::session_validity_cache_key(&b).unwrap();
        pool.add_native_action_from_gossip_trusted(owner, b).unwrap();
        assert!(
            !pool.session_sig_verified(&key_b),
            "gossip-trusted admit must NOT populate (not locally verified)"
        );
    }

    /// Feature #13 — the sig cache is bounded: past the cap the oldest entry is
    /// evicted (a MISS => full ed25519 verify, safe).
    #[test]
    fn session_sig_cache_bounded_by_cap() {
        let (_dir, state) = setup();
        let config = MempoolConfig {
            session_sig_cache_cap: 2,
            ..MempoolConfig::default()
        };
        let pool = Mempool::new(state, config);
        let k1 = B256::from([1u8; 32]);
        let k2 = B256::from([2u8; 32]);
        let k3 = B256::from([3u8; 32]);
        pool.cache_session_sig_verified(k1);
        pool.cache_session_sig_verified(k2);
        pool.cache_session_sig_verified(k3); // evicts oldest (k1)
        assert!(!pool.session_sig_verified(&k1), "oldest evicted at cap");
        assert!(pool.session_sig_verified(&k2));
        assert!(pool.session_sig_verified(&k3));
    }

    #[test]
    fn remove_committed_stash_refreshes_verified_sender() {
        let (_dir, state) = setup();
        // Tiny cache cap so a few inserts cheaply churn the original out.
        let config = MempoolConfig {
            verified_sender_cache_cap: 2,
            ..MempoolConfig::default()
        };
        let pool = Mempool::new(state, config);

        let k = k256::ecdsa::SigningKey::from_slice(
            &alloy_primitives::hex::decode(
                "ac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80",
            )
            .unwrap(),
        )
        .unwrap();
        let base = now_ms();

        let a = torus_types::eip712::sign_native_action(
            torus_types::NativeAction::ClaimRewards,
            base + 1,
            &k,
        );
        let sender = a.recover_sender().unwrap();
        let hash = torus_types::compute_action_hash(&a); // pool key
        let key = torus_types::verified_cache_key(&a).expect("eip712"); // trust-cache key

        // Admit (verified) -> cached + pooled (presigned does not remove).
        pool.add_native_action_presigned(sender, a).unwrap();
        assert_eq!(pool.verified_sender(&key), Some(sender), "seeded on admit");

        // Churn the cache past its cap (same signer, distinct nonces => distinct
        // keys) to evict the original entry from the cache.
        for i in 0..5u64 {
            let other = torus_types::eip712::sign_native_action(
                torus_types::NativeAction::ClaimRewards,
                base + 100 + i,
                &k,
            );
            let os = other.recover_sender().unwrap();
            pool.add_native_action_presigned(os, other).unwrap();
        }
        assert_eq!(
            pool.verified_sender(&key),
            None,
            "original evicted from the cache by churn"
        );
        assert!(
            pool.get_native_by_hash(&hash).is_some(),
            "but still resident in the pool"
        );

        // Commit it: remove_committed_native MUST refresh-stash the verified sender
        // to the fresh cache end BEFORE pruning the pool entry, so the exec thread
        // (which lags behind commit) still gets a HIT.
        pool.remove_committed_native(&[hash]);
        assert_eq!(
            pool.verified_sender(&key),
            Some(sender),
            "re-stashed to the cache before prune"
        );
        assert!(
            pool.get_native_by_hash(&hash).is_none(),
            "pool entry removed after commit"
        );
    }

    /// P2 funnel item 1: the 60s nonce-window purge was info-log only — the
    /// biggest silent loss sink. Both counters (actions AND orders, the latter
    /// batch-aware) must increment when the lazy eviction fires in drain_native.
    #[test]
    fn nonce_expiry_purge_increments_counters() {
        use torus_types::eip712::NONCE_WINDOW_MS;
        let (_dir, state) = setup();
        let pool = Mempool::new(state, MempoolConfig::default());
        let metrics = std::sync::Arc::new(torus_telemetry::Metrics::new());
        pool.set_metrics(metrics.clone());

        let key = k256::ecdsa::SigningKey::from_slice(
            &alloy_primitives::hex::decode(
                "ac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80",
            )
            .unwrap(),
        )
        .unwrap();
        let order = torus_types::PlaceOrderParams {
            market_id: 1,
            is_buy: true,
            price: torus_types::FixedPoint::from_raw(100 * torus_types::FixedPoint::SCALE),
            quantity: torus_types::FixedPoint::from_raw(torus_types::FixedPoint::SCALE),
            order_type: torus_types::OrderType::Limit,
            time_in_force: torus_types::TimeInForce::GTC,
            reduce_only: false,
            client_order_id: None,
        };
        // Already outside the 60s window. Admission would reject it, so plant it
        // via reinsert_native (the reorg path, which skips the nonce gate) —
        // exactly the class of entry the lazy eviction must purge and count.
        let stale = torus_types::eip712::sign_native_action(
            torus_types::NativeAction::PlaceOrderBatch(vec![order; 3]),
            now_ms().saturating_sub(2 * NONCE_WINDOW_MS),
            &key,
        );
        pool.reinsert_native(vec![stale]);
        assert_eq!(pool.native_pool_size(), 1, "stale entry planted");

        let drained = pool.drain_native(10);
        assert!(drained.is_empty(), "expired entry must not be drained");
        assert_eq!(
            metrics.native_pool_expired_actions.get(),
            1,
            "one expired action counted"
        );
        assert_eq!(
            metrics.native_pool_expired_orders.get(),
            3,
            "orders = actions × batch size"
        );
    }

    /// P3 Round-1 item 1: the pool-occupancy gauge (mis-wired in P2 — read 0 at
    /// every sample) must go NONZERO on a successful insert, the insert-success
    /// counter must advance, and the gauge must DECREMENT when the action is
    /// removed on commit.
    #[test]
    fn native_size_gauge_tracks_insert_and_remove() {
        let (_dir, state) = setup();
        let pool = Mempool::new(state, MempoolConfig::default());
        let metrics = std::sync::Arc::new(torus_telemetry::Metrics::new());
        pool.set_metrics(metrics.clone());
        assert_eq!(metrics.mempool_native_size.get(), 0, "gauge starts at 0");

        let k = k256::ecdsa::SigningKey::from_slice(
            &alloy_primitives::hex::decode(
                "ac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80",
            )
            .unwrap(),
        )
        .unwrap();
        let base = now_ms();
        let a = torus_types::eip712::sign_native_action(
            torus_types::NativeAction::ClaimRewards,
            base + 1,
            &k,
        );
        let sender = a.recover_sender().unwrap();
        let hash = torus_types::compute_action_hash(&a);

        pool.add_native_action_presigned(sender, a).unwrap();
        assert_eq!(
            metrics.mempool_native_size.get(),
            1,
            "gauge nonzero after insert"
        );
        assert_eq!(
            metrics.native_pool_inserted.get(),
            1,
            "insert-success counter advanced"
        );

        pool.remove_committed_native(&[hash]);
        assert_eq!(
            metrics.mempool_native_size.get(),
            0,
            "gauge decremented on commit removal"
        );
    }

    /// P3 Round-1 item 2: the continuous-expiry tick must purge nonce-aged
    /// entries and count them WITHOUT any selection/drain call, and must drive
    /// the occupancy gauge back to 0 — the fix for P2's lump-at-cooldown expiry.
    #[test]
    fn tick_expiry_evicts_without_selection() {
        use torus_types::eip712::NONCE_WINDOW_MS;
        let (_dir, state) = setup();
        let pool = Mempool::new(state, MempoolConfig::default());
        let metrics = std::sync::Arc::new(torus_telemetry::Metrics::new());
        pool.set_metrics(metrics.clone());

        let k = k256::ecdsa::SigningKey::from_slice(
            &alloy_primitives::hex::decode(
                "ac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80",
            )
            .unwrap(),
        )
        .unwrap();
        let order = torus_types::PlaceOrderParams {
            market_id: 1,
            is_buy: true,
            price: torus_types::FixedPoint::from_raw(100 * torus_types::FixedPoint::SCALE),
            quantity: torus_types::FixedPoint::from_raw(torus_types::FixedPoint::SCALE),
            order_type: torus_types::OrderType::Limit,
            time_in_force: torus_types::TimeInForce::GTC,
            reduce_only: false,
            client_order_id: None,
        };
        // Plant an already-expired batch via the reorg path (skips the nonce gate).
        let stale = torus_types::eip712::sign_native_action(
            torus_types::NativeAction::PlaceOrderBatch(vec![order; 5]),
            now_ms().saturating_sub(2 * NONCE_WINDOW_MS),
            &k,
        );
        pool.reinsert_native(vec![stale]);
        assert_eq!(pool.native_pool_size(), 1);
        assert!(metrics.mempool_native_size.get() >= 1, "gauge saw the reinsert");

        // The tick alone (no drain/select) must evict and count it.
        pool.tick_expiry();
        assert_eq!(pool.native_pool_size(), 0, "tick evicted the stale entry");
        assert_eq!(metrics.native_pool_expired_actions.get(), 1);
        assert_eq!(
            metrics.native_pool_expired_orders.get(),
            5,
            "orders = actions × batch size"
        );
        assert_eq!(metrics.mempool_native_size.get(), 0, "gauge back to 0");
    }

    #[test]
    fn gossip_drop_increments_counter() {
        let (_dir, state) = setup();
        let pool = Mempool::new(state, MempoolConfig::default());
        let metrics = std::sync::Arc::new(torus_telemetry::Metrics::new());
        pool.set_metrics(metrics.clone());
        // Capacity-1 channel with a live but never-drained receiver: the second
        // post-admission publish hits TrySendError::Full and must be counted.
        let (tx, _rx) = tokio::sync::mpsc::channel(1);
        pool.set_native_gossip_tx(tx);
        pool.set_native_gossip_enabled(true);

        let key1 = k256::ecdsa::SigningKey::from_slice(
            &alloy_primitives::hex::decode(
                "ac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80",
            )
            .unwrap(),
        )
        .unwrap();
        let key2 = k256::ecdsa::SigningKey::from_slice(
            &alloy_primitives::hex::decode(
                "59c6995e998f97a5a0044966f0945389dc9e86dae88c7a8412f4603b6b78690d",
            )
            .unwrap(),
        )
        .unwrap();
        let now = now_ms();
        let a1 = torus_types::eip712::sign_native_action(
            torus_types::NativeAction::ClaimRewards,
            now,
            &key1,
        );
        let a2 = torus_types::eip712::sign_native_action(
            torus_types::NativeAction::ClaimRewards,
            now,
            &key2,
        );
        let s1 = a1.recover_sender().unwrap();
        let s2 = a2.recover_sender().unwrap();

        pool.add_native_action_presigned(s1, a1).unwrap();
        assert_eq!(metrics.native_gossip_dropped_full.get(), 0);
        pool.add_native_action_presigned(s2, a2).unwrap();
        assert_eq!(
            metrics.native_gossip_dropped_full.get(),
            1,
            "second publish must be counted as a channel-full drop"
        );
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
        // At the D4 floor (1 gwei) but not outbidding the cheapest pooled tx
        // (same max fee, lower priority fee) — still PoolFull, not FeeTooLow.
        let err = pool
            .add_evm_tx(create_eip1559_tx(
                &k5,
                0,
                1_000_000_000,
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
        // Share cap off: this test exercises the budget dimension alone.
        let config = MempoolConfig {
            evm_sender_share_pct: 0,
            ..MempoolConfig::default()
        };
        let pool = Mempool::new(state.clone(), config);

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
    fn evm_sender_gas_share_enforced() {
        // D1 (S392): one sender may only fill their share of the EVM budget.
        // Budget 210k at 25% => 52.5k/sender => two 21k transfers fit, not three.
        let (_dir, state) = setup();
        let config = MempoolConfig {
            evm_block_gas_budget: 210_000,
            evm_sender_share_pct: 25,
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

        let drained = pool.drain_evm(30_000_000, B256::ZERO);
        assert_eq!(drained.len(), 2, "sender capped at 25% of the EVM budget");
        assert_eq!(pool.evm_pool_size(), 3, "excess stays pooled");
    }

    #[test]
    fn evm_sender_share_zero_disables_cap() {
        // D1: share pct 0 = mainnet mode — a single sender may fill the budget.
        let (_dir, state) = setup();
        let config = MempoolConfig {
            evm_block_gas_budget: 105_000,
            evm_sender_share_pct: 0,
            ..MempoolConfig::default()
        };
        let pool = Mempool::new(state.clone(), config);

        let k = key(71);
        fund(&state, &address_from_key(&k), U256::from(10u64.pow(18)), 0);
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

        let drained = pool.drain_evm(30_000_000, B256::ZERO);
        assert_eq!(drained.len(), 5, "5 x 21k = 105k fills the whole budget");
    }

    #[test]
    fn drain_uses_configured_evm_budget() {
        // D1: the configured budget bounds selection even when the caller
        // passes the full 30M block gas limit.
        let (_dir, state) = setup();
        let config = MempoolConfig {
            evm_block_gas_budget: 63_000,
            evm_sender_share_pct: 0,
            ..MempoolConfig::default()
        };
        let pool = Mempool::new(state.clone(), config);

        let k = key(72);
        fund(&state, &address_from_key(&k), U256::from(10u64.pow(18)), 0);
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

        let drained = pool.drain_evm(30_000_000, B256::ZERO);
        assert_eq!(drained.len(), 3, "63k budget admits exactly three 21k txs");
        assert_eq!(pool.evm_pool_size(), 2);
    }

    #[test]
    fn anti_mev_same_gas_different_parent_hash() {
        let (_dir, state) = setup();
        let config = MempoolConfig::default();

        // Create two senders with the same gas price.
        let ka = key(80);
        let kb = key(81);
        fund(&state, &address_from_key(&ka), U256::from(10u64.pow(18)), 0);
        fund(&state, &address_from_key(&kb), U256::from(10u64.pow(18)), 0);

        // Submit with identical gas prices.
        let pool1 = Mempool::new(state.clone(), config.clone());
        pool1
            .add_evm_tx(create_eip1559_tx(
                &ka,
                0,
                1_000_000_000,
                100_000_000,
                21_000,
                U256::ZERO,
            ))
            .unwrap();
        pool1
            .add_evm_tx(create_eip1559_tx(
                &kb,
                0,
                1_000_000_000,
                100_000_000,
                21_000,
                U256::ZERO,
            ))
            .unwrap();

        let pool2 = Mempool::new(state.clone(), config);
        pool2
            .add_evm_tx(create_eip1559_tx(
                &ka,
                0,
                1_000_000_000,
                100_000_000,
                21_000,
                U256::ZERO,
            ))
            .unwrap();
        pool2
            .add_evm_tx(create_eip1559_tx(
                &kb,
                0,
                1_000_000_000,
                100_000_000,
                21_000,
                U256::ZERO,
            ))
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
                &k,
                i,
                1_000_000_000,
                100_000_000,
                21_000,
                U256::ZERO,
            ))
            .unwrap();
        }

        let mem_after_add = pool.memory_used();
        assert!(mem_after_add > 0, "memory should be tracked after adds");

        // Drain all
        let drained = pool.drain_evm(30_000_000, B256::ZERO);
        assert_eq!(drained.len(), 3);

        let mem_after_drain = pool.memory_used();
        assert_eq!(
            mem_after_drain, 0,
            "memory should be zero after draining all txs"
        );

        // Add more — must succeed, not PoolFull
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

        pool.add_evm_tx(create_eip1559_tx(
            &k,
            0,
            1_000_000_000,
            100_000_000,
            21_000,
            U256::ZERO,
        ))
        .unwrap();
        assert_eq!(pool.pending_nonce(&addr), 1);

        pool.add_evm_tx(create_eip1559_tx(
            &k,
            1,
            1_000_000_000,
            100_000_000,
            21_000,
            U256::ZERO,
        ))
        .unwrap();
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
            2,
            1_000_000_000,
            100_000_000,
            21_000,
            U256::ZERO,
        ))
        .unwrap();
        assert_eq!(pool.pending_nonce(&addr), 1);

        // Fill the gap — now pending nonce should jump to 3
        pool.add_evm_tx(create_eip1559_tx(
            &k,
            1,
            1_000_000_000,
            100_000_000,
            21_000,
            U256::ZERO,
        ))
        .unwrap();
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
        assert_eq!(pool.evm_pool_size(), 4);

        // Simulate block commit: advance state nonce to 2
        fund(&state, &addr, U256::from(10u64.pow(18)), 2);
        pool.notify_block_committed(&[addr]);

        // Nonces 0 and 1 should be pruned, 2 and 3 remain
        assert_eq!(pool.evm_pool_size(), 2);
        assert_eq!(pool.pending_nonce(&addr), 4);
    }

    // ---- D3/D4 (S392): tx-type gate and fee floor ----

    fn sign_envelope<T>(key: &SigningKey, tx: T) -> Vec<u8>
    where
        T: SignableTransaction<AlloySig>,
        TxEnvelope: From<alloy_consensus::Signed<T>>,
    {
        let sig_hash = tx.signature_hash();
        let (sig, recid) = key.sign_prehash_recoverable(sig_hash.as_slice()).unwrap();
        let signature = AlloySig::new(
            U256::from_be_slice(sig.r().to_bytes().as_slice()),
            U256::from_be_slice(sig.s().to_bytes().as_slice()),
            recid.is_y_odd(),
        );
        let envelope = TxEnvelope::from(tx.into_signed(signature));
        let mut buf = Vec::new();
        envelope.encode(&mut buf);
        buf
    }

    fn create_eip4844_tx(key: &SigningKey, nonce: u64) -> Vec<u8> {
        sign_envelope(
            key,
            alloy_consensus::TxEip4844 {
                chain_id: torus_types::eip712::TORUS_CHAIN_ID,
                nonce,
                gas_limit: 21_000,
                max_fee_per_gas: 2_000_000_000,
                max_priority_fee_per_gas: 1,
                to: Address::ZERO,
                value: U256::ZERO,
                input: Bytes::new(),
                access_list: Default::default(),
                blob_versioned_hashes: vec![B256::repeat_byte(1)],
                max_fee_per_blob_gas: 1,
            },
        )
    }

    fn create_eip7702_tx(key: &SigningKey, nonce: u64) -> Vec<u8> {
        sign_envelope(
            key,
            alloy_consensus::TxEip7702 {
                chain_id: torus_types::eip712::TORUS_CHAIN_ID,
                nonce,
                gas_limit: 21_000,
                max_fee_per_gas: 2_000_000_000,
                max_priority_fee_per_gas: 1,
                to: Address::ZERO,
                value: U256::ZERO,
                input: Bytes::new(),
                access_list: Default::default(),
                authorization_list: vec![],
            },
        )
    }

    #[test]
    fn rejects_blob_tx_type_at_admission() {
        let (_dir, state) = setup();
        let pool = Mempool::new(state.clone(), MempoolConfig::default());
        let k = key(95);
        fund(&state, &address_from_key(&k), U256::from(10u64.pow(18)), 0);

        let err = pool.add_evm_tx(create_eip4844_tx(&k, 0)).unwrap_err();
        assert!(
            matches!(err, MempoolError::UnsupportedTxType { tx_type: 3 }),
            "expected UnsupportedTxType(3), got: {err}"
        );
        assert_eq!(pool.evm_pool_size(), 0);
    }

    #[test]
    fn rejects_setcode_tx_type_at_admission() {
        let (_dir, state) = setup();
        let pool = Mempool::new(state.clone(), MempoolConfig::default());
        let k = key(96);
        fund(&state, &address_from_key(&k), U256::from(10u64.pow(18)), 0);

        let err = pool.add_evm_tx(create_eip7702_tx(&k, 0)).unwrap_err();
        assert!(
            matches!(err, MempoolError::UnsupportedTxType { tx_type: 4 }),
            "expected UnsupportedTxType(4), got: {err}"
        );
        assert_eq!(pool.evm_pool_size(), 0);
    }

    #[test]
    fn admission_rejects_max_fee_below_base_fee() {
        let (_dir, state) = setup();
        let pool = Mempool::new(state.clone(), MempoolConfig::default());
        let k = key(97);
        let addr = address_from_key(&k);
        fund(&state, &addr, U256::from(10u64.pow(18)), 0);

        // 0.999999999 gwei < the 1-gwei floor.
        let err = pool
            .add_evm_tx(create_eip1559_tx(&k, 0, 999_999_999, 0, 21_000, U256::ZERO))
            .unwrap_err();
        assert!(
            matches!(err, MempoolError::FeeTooLow { .. }),
            "expected FeeTooLow, got: {err}"
        );

        // Nonce chain unaffected: the SAME nonce at the floor is admitted cleanly.
        pool.add_evm_tx(create_eip1559_tx(
            &k,
            0,
            1_000_000_000,
            100,
            21_000,
            U256::ZERO,
        ))
        .unwrap();
        assert_eq!(pool.pending_nonce(&addr), 1);
    }

    #[test]
    fn drain_recheck_drops_below_floor_txs() {
        let (_dir, state) = setup();
        let pool = Mempool::new(state.clone(), MempoolConfig::default());
        let k = key(98);
        fund(&state, &address_from_key(&k), U256::from(10u64.pow(18)), 0);

        pool.add_evm_tx(create_eip1559_tx(
            &k,
            0,
            1_000_000_000,
            100,
            21_000,
            U256::ZERO,
        ))
        .unwrap();

        // Floor rises after admission: the tx must NOT be selected, but stays pooled.
        pool.set_base_fee(2_000_000_000);
        assert!(
            pool.drain_evm(30_000_000, B256::ZERO).is_empty(),
            "below-floor tx must not be drained into a block"
        );
        assert_eq!(
            pool.evm_pool_size(),
            1,
            "tx stays pooled for when the fee drops"
        );

        // Floor falls back: the tx becomes selectable again.
        pool.set_base_fee(1_000_000_000);
        assert_eq!(pool.drain_evm(30_000_000, B256::ZERO).len(), 1);
    }

    #[test]
    fn accepts_pre_eip155_legacy_tx() {
        // D6 (S392): Nick's-method keyless deploys (canonical Multicall3) are
        // pre-EIP-155 legacy txs with NO chain id — admission must accept them.
        let (_dir, state) = setup();
        let pool = Mempool::new(state.clone(), MempoolConfig::default());
        let k = key(99);
        fund(&state, &address_from_key(&k), U256::from(10u64.pow(18)), 0);

        let tx = alloy_consensus::TxLegacy {
            chain_id: None,
            nonce: 0,
            gas_price: 2_000_000_000,
            gas_limit: 21_000,
            to: TxKind::Call(Address::ZERO),
            value: U256::ZERO,
            input: Bytes::new(),
        };
        let raw = sign_envelope(&k, tx);
        pool.add_evm_tx(raw)
            .expect("pre-155 legacy tx must be admitted");
        assert_eq!(pool.evm_pool_size(), 1);
    }
}
