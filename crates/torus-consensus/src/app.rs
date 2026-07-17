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

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::SyncSender;
use std::sync::{Arc, RwLock};
use std::thread::JoinHandle;
use torus_bridge::{
    decode_all_txs, sort_native_actions, BlockCommitter, BlockProposer, BlockValidator,
    BundleState, NativeExecContext, NativeExecutor,
};
use torus_economics::{EpochManager, SlashReason, StakingManager};
use torus_evm::{EvmExecutor, TORUS_CHAIN_ID};
use torus_mempool::Mempool;
use torus_state::cf::{
    CF_BLOCK_BODIES, CF_BLOCK_HEADERS, CF_CONSENSUS_META, META_NATIVE_APPLIED_HEIGHT,
};
use torus_state::{NativeStateOverlay, StateBackend, StateDb};
use torus_types::{
    Address, ChainConfig, CompactBlock, TorusBlock, TorusBlockBody, TorusBlockHeader, ValidatorSet,
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

/// Shared consensus state for leader discovery by non-consensus components (RPC).
pub struct LeaderState {
    view: AtomicU64,
    validators: RwLock<hotstuff_rs::types::validator_set::ValidatorSet>,
}

impl LeaderState {
    fn new() -> Self {
        Self {
            view: AtomicU64::new(1),
            validators: RwLock::new(hotstuff_rs::types::validator_set::ValidatorSet::new()),
        }
    }

    fn set_view(&self, view: u64) {
        self.view.store(view, Ordering::Relaxed);
    }

    fn sync_validators(&self, vs: &torus_types::ValidatorSet) {
        let mut hs_vs = hotstuff_rs::types::validator_set::ValidatorSet::new();
        for v in &vs.validators {
            if let Ok(vk) = VerifyingKey::from_bytes(&v.pubkey.0) {
                hs_vs.put(&vk, Power::new(v.power));
            }
        }
        *self.validators.write().unwrap() = hs_vs;
    }

    pub fn current_view(&self) -> u64 {
        self.view.load(Ordering::Relaxed)
    }

    pub fn current_leader(&self) -> Option<VerifyingKey> {
        let vs = self.validators.read().unwrap();
        if vs.is_empty() {
            return None;
        }
        let view =
            hotstuff_rs::types::data_types::ViewNumber::new(self.view.load(Ordering::Relaxed));
        Some(hotstuff_rs::pacemaker::select_leader(view, &vs))
    }
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
    /// Shared mempool handle for the exec trust-cache read path: a HIT lets the
    /// execution thread reuse a locally-verified sender and skip the secp256k1
    /// recover. `None` when the node runs without a mempool (rpc-only / tests).
    mempool: Option<Arc<Mempool>>,
    /// Node-local gate for the trust-cache read (`--exec-trust-cache`, default
    /// off). When false the cache is never consulted at exec -> full recover every
    /// time (today's behavior). A HIT is deterministic (== fresh recover), so this
    /// only changes performance, never the resolved sender or state.
    exec_trust_cache: bool,
    /// O3: background writer for trade-history CFs (node-local, non-root).
    /// `Some` on the live node — fills buffer their KVs during exec and this
    /// writer applies them off the execution thread. `None` in tests -> trades
    /// write inline through the overlay exactly as before O3. Dropped with the
    /// ExecutionContext at execution-thread exit, which drains the queue before
    /// `TorusApp::Drop`'s join returns (shutdown flush ordering).
    trade_writer: Option<torus_state::BackgroundCfWriter>,
    /// P3 Task-1: background writer for the per-block CF_BLOCK_BODIES JSON write,
    /// gated by TORUS_ASYNC_BODY_PERSIST. `Some` moves the RocksDB body put off
    /// the execution thread (the block-body put was 12.7% of exec_block on the
    /// bs400 cross-flow leg); the exec thread still serializes the body (bytes are
    /// MOVED into the channel — no extra clone) and only the write is deferred.
    /// `None` (default / tests) = synchronous put, byte-identical to pre-Task-1.
    /// The stored JSON is unchanged, so every reader (RPC getBlock, pruner, crash
    /// recovery) parses it exactly as before. Crash safety: replay only needs the
    /// body of the single last-committed height, which crashed mid-execution
    /// BEFORE reaching the body write, so it is absent under sync AND async; a
    /// crash losing an async-queued body for an already-applied height leaves
    /// state durable (flush precedes the body write) and only leaves a cosmetic
    /// RPC/trade-history hole, never consensus divergence. Dropped with the
    /// ExecutionContext at exec-thread exit, draining the queue before join.
    body_writer: Option<torus_state::BackgroundCfWriter>,
}

// ---- Standalone helpers (used by both execution thread and crash recovery) ----

/// Truthy check for a `TORUS_*` boolean env var (1/true/on/yes, case-insensitive,
/// trimmed). Anything else — unset, empty, or unparseable — is false, so a typo
/// leaves the safe default (synchronous behavior) in place.
fn env_flag(name: &str) -> bool {
    std::env::var(name)
        .ok()
        .map(|v| {
            matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "on" | "yes"
            )
        })
        .unwrap_or(false)
}

/// RH4: DA-body GC retention window in blocks, from `TORUS_DA_BODY_RETENTION`.
///
/// A mirrored body is deleted once its commit height is this many blocks behind
/// the current committed tip — the point past which no local reconstruct and no
/// lagging peer's push-race pull can still need it. Default 128 (≈25 s at the
/// idle ~5 blk/s cadence, far beyond the few-view live push-race window served
/// by `/torus/native-da/1.0`). `0` disables GC (archive: mirror stays
/// write-only). Unparseable → default, so a typo never silently disables GC.
fn da_body_retention() -> u64 {
    match std::env::var("TORUS_DA_BODY_RETENTION") {
        Ok(v) => v.trim().parse::<u64>().unwrap_or(128),
        Err(_) => 128,
    }
}

/// RH4 GC windowing (pure, testable). Pushes `committed_hashes` for `height`
/// onto the retention FIFO, then pops and returns the body-hashes of every
/// buffered height that has aged strictly outside the window
/// (`commit_height <= height - retention`). The FIFO is left holding only the
/// in-window heights, so a body survives exactly `retention` blocks past its
/// commit before becoming eligible for deletion.
///
/// Deferred (not commit-instant) because a lagging peer still executing that
/// height reconstructs its `CompactBlock` bodies via the `/torus/native-da/1.0`
/// pull path we serve; deleting at the local commit instant would recreate the
/// missing-body stall (`missing_action_rejections` 0→nonzero, the R1 signature).
/// The retention window (default 128 blocks) comfortably exceeds that live
/// push-race lag. Committed-only: expired-but-never-committed bodies are NOT
/// GC'd here — pool expiry is local-clock (`evict_expired` uses `now_ms`), so a
/// peer could still include an action the instant before our local expiry.
fn drain_da_gc_window(
    pending: &mut std::collections::VecDeque<(u64, Vec<torus_types::B256>)>,
    height: u64,
    retention: u64,
    committed_hashes: Vec<torus_types::B256>,
) -> Vec<torus_types::B256> {
    if !committed_hashes.is_empty() {
        pending.push_back((height, committed_hashes));
    }
    // A body committed at height `h` is deletable once `h + retention <= tip`.
    // The additive form (vs `h <= tip - retention`) is correct at genesis: with
    // `tip < retention` nothing ages out, whereas `saturating_sub` would floor
    // the cutoff to 0 and wrongly delete height 0.
    let mut to_delete: Vec<torus_types::B256> = Vec::new();
    while let Some((h, _)) = pending.front() {
        if h.saturating_add(retention) <= height {
            let (_, hashes) = pending.pop_front().expect("front just peeked");
            to_delete.extend(hashes);
        } else {
            break;
        }
    }
    to_delete
}

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
    iter.next().and_then(|r| r.ok()).and_then(|(key, _)| {
        if key.len() == 8 {
            Some(u64::from_be_bytes(key[..8].try_into().ok()?))
        } else {
            None
        }
    })
}

fn persist_block_header(state_db: &StateDb, block: &TorusBlock) {
    let block_hash = alloy_primitives::keccak256(block.header.canonical_header_bytes());
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
    if let Err(e) = state_db.put_cf_raw(CF_BLOCK_HEADERS, &block.header.height.to_be_bytes(), &data)
    {
        tracing::error!(%e, height = block.header.height, "failed to persist block header");
    }
}

// ---- Execution pipeline ----

impl ExecutionContext {
    fn execute_committed_block(
        &self,
        torus_block: &TorusBlock,
        pending_slashes: Vec<PendingSlash>,
    ) {
        let height = torus_block.header.height;

        if let Some(applied) = read_native_applied_height(&self.state_db) {
            if applied >= height {
                tracing::debug!(
                    height,
                    applied,
                    "execution pipeline: already applied, skipping"
                );
                return;
            }
        }

        // Exec-ceiling Option A: total timer starts AFTER the skip-check so
        // restart-replayed (already-applied) blocks never pollute the distribution.
        let block_timer = std::time::Instant::now();

        for slash in pending_slashes {
            match self
                .staking
                .slash(slash.validator, slash.fraction_bps, slash.reason, 0)
            {
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
                    computed_fee_revenue =
                        torus_bridge::proposer::compute_fee_revenue(&validated.receipts);
                    // Phase A: commit EVM plain state + the hashed mirror + the incremental trie
                    // nodes in ONE atomic batch, so CF_HASHED_*/CF_TRIE_* stay in lockstep with
                    // CF_ACCOUNTS (keeps the incremental root's base correct across restarts/replay).
                    // Falls back to the plain commit if the incremental path errors, so a trie bug
                    // can never halt the chain (the full-scan root stays primary unless the flag is on).
                    match torus_state::incremental::commit_evm_bundle_incremental(
                        &self.state_db,
                        &validated.bundle,
                    ) {
                        Ok(_root) => {}
                        Err(e) => {
                            tracing::error!(%e, height, "incremental commit failed; falling back to plain EVM commit");
                            if let Err(e2) = BlockCommitter::commit_pending_bundle(
                                &self.state_db,
                                &validated.bundle,
                            ) {
                                tracing::error!(%e2, height, "failed to commit EVM bundle (fallback)");
                            }
                        }
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
            // One verification pass resolves every sender too (EIP-712 ecrecover or
            // session owner); `None` marks an invalid signature. Reused below so we
            // never recover the same action twice.
            let verify_timer = std::time::Instant::now();
            let resolved_senders = if has_native {
                torus_types::eip712::batch_verify_native_actions_cached(
                    &torus_block.native_actions,
                    // `header.timestamp` is SECONDS; session `expiry` is MILLISECONDS
                    // (mem 0226e678: exec_create_session validates in ms). Convert so
                    // the exec-side expiry comparison is unit-correct — before this
                    // fix seconds-vs-ms meant sessions never expired on-chain
                    // (~56000-year window). Grace window is applied inside
                    // `batch_verify` (SESSION_EXPIRY_EXEC_GRACE_MS).
                    torus_block.header.timestamp.saturating_mul(1000),
                    // Session-owner cache (gated by --exec-trust-cache): on a HIT
                    // reuse the cached SessionData and skip the per-action
                    // `get_session` RocksDB read (the P3 exec_verify lever for the
                    // session-signed workload). The verify path below STILL
                    // re-validates whatever this returns in full — expiry vs block
                    // timestamp, scope, and the ed25519 signature over the action —
                    // exactly as an uncached resolution, so a HIT can never accept
                    // an expired / out-of-scope / forged action; it only removes the
                    // DB read. A MISS falls through to the authoritative
                    // `get_session` and populates the cache. Correctness across a
                    // session REVOKE (and revoke->recreate owner rebinding) is held
                    // by `invalidate_session` on every CreateSession/RevokeSession
                    // once this block is flushed (below), so a stale entry is never
                    // served. A HIT is thus indistinguishable from a MISS =>
                    // deterministic across validators (cache is a CPU optimization,
                    // never a consensus input).
                    |pubkey| {
                        if self.exec_trust_cache {
                            if let Some(m) = self.mempool.as_ref() {
                                if let Some(data) = m.session_owner(pubkey) {
                                    return Some(data); // HIT: skip get_session
                                }
                                // MISS: authoritative read, then populate.
                                let data = self.state_db.get_session(pubkey).ok().flatten()?;
                                m.cache_session_owner(*pubkey, data.clone());
                                return Some(data);
                            }
                        }
                        self.state_db.get_session(pubkey).ok().flatten()
                    },
                    // Exec trust-cache read (gated by --exec-trust-cache, default
                    // off): when enabled, a HIT reuses a locally-verified sender
                    // (keyed by the signature-committing key) and skips the secp256k1
                    // recover; a MISS — or the flag being off — falls through to the
                    // full recover + slash below.
                    |key| {
                        if self.exec_trust_cache {
                            let hit =
                                self.mempool.as_ref().and_then(|m| m.verified_sender(key));
                            // Proof-leg counter: a HIT here is exactly one secp256k1
                            // recover skipped at execution. Incremented ONLY on Some,
                            // so a MISS (None) never counts — the cache can only ever
                            // reduce verify on the hit path, never skip-by-default on a
                            // miss (security invariant 2). Pairs with
                            // `exec_verify_seconds` so the proof leg shows skips rising
                            // as verify time falls.
                            if hit.is_some() {
                                if let Some(ref m) = self.metrics {
                                    m.exec_verify_skipped.inc();
                                }
                            }
                            hit
                        } else {
                            None
                        }
                    },
                    // Feature #13: session signature-validity cache read (gated by
                    // --exec-trust-cache). A HIT means this exact ed25519 signature
                    // was already locally verified (RPC ingress / gossip-recover),
                    // so the batch verify can skip the EIP-712 struct/signing hash +
                    // ed25519 verify for this action — the dominant exec_verify cost
                    // on the 100%-session-signed workload. The stateful checks
                    // (session_lookup, expiry vs block timestamp, scope) STILL run
                    // above on every action, so a HIT can never accept an
                    // expired/revoked/out-of-scope/forged action; it is
                    // observationally identical to a MISS. A MISS — or the flag being
                    // off — full-verifies, exactly as today.
                    |key| {
                        if self.exec_trust_cache {
                            let hit = self
                                .mempool
                                .as_ref()
                                .map(|m| m.session_sig_verified(key))
                                .unwrap_or(false);
                            // Proof-leg counter: a HIT is exactly one ed25519 session
                            // verify skipped at execution. Incremented ONLY on a HIT,
                            // so a MISS never counts (never skip-by-default).
                            if hit {
                                if let Some(ref m) = self.metrics {
                                    m.exec_session_sig_skipped.inc();
                                }
                            }
                            hit
                        } else {
                            false
                        }
                    },
                )
            } else {
                vec![]
            };
            if let Some(ref m) = self.metrics {
                m.exec_verify_seconds
                    .observe(verify_timer.elapsed().as_secs_f64());
            }

            let invalid_count = resolved_senders.iter().filter(|s| s.is_none()).count();
            if invalid_count > 0 {
                tracing::error!(
                    count = invalid_count,
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
                if let Err(e) = self
                    .staking
                    .tombstone_validator(&torus_block.header.proposer)
                {
                    tracing::error!(%e, "CRITICAL: failed to tombstone proposer for invalid attestation");
                }
            }

            let replay_guard_timer = std::time::Instant::now();
            let mut sender_actions = Vec::with_capacity(torus_block.native_actions.len());
            let mut consumed_nonces = Vec::new();
            // Defense-in-depth replay guard. The non-destructive mempool selection ×
            // HotStuff 3-chain pipeline re-includes the same action in consecutive
            // blocks before the first commits and calls `remove_committed_native`
            // (memory b206f59c / 282f9818). Without this guard each inclusion
            // re-executed — proven on devnet, one TransferToPerp credited 3x. Skip any
            // (sender, nonce) already consumed by a prior committed block — mirrors the
            // sync/catchup check in torus-bridge validator.rs — so each action executes
            // at most once even when included 2-3x. `seen_in_block` also dedups a
            // (sender, nonce) repeated within a single block.
            let mut seen_in_block = std::collections::HashSet::new();
            for (i, signed) in torus_block.native_actions.iter().enumerate() {
                let Some(sender) = resolved_senders.get(i).copied().flatten() else {
                    tracing::error!(index = i, "INVALID SIG in attested block — skipping action");
                    continue;
                };
                let nonce_key = torus_state::cf::native_nonce_key(&sender, signed.nonce);
                let already_committed = self
                    .state_db
                    .get_cf_raw(torus_state::cf::CF_NATIVE_NONCES, &nonce_key)
                    .unwrap_or(None)
                    .is_some();
                if already_committed || !seen_in_block.insert((sender, signed.nonce)) {
                    tracing::warn!(
                        %sender,
                        nonce = signed.nonce,
                        height,
                        "skipping duplicate/replayed native action on live commit path"
                    );
                    continue;
                }
                consumed_nonces.push((sender, signed.nonce));
                sender_actions.push((sender, signed.action.clone()));
            }
            if let Some(ref m) = self.metrics {
                m.exec_replay_guard_seconds
                    .observe(replay_guard_timer.elapsed().as_secs_f64());
                m.native_actions_processed
                    .inc_by(sender_actions.len() as u64);
            }

            let seed_bundle_timer = std::time::Instant::now();
            let overlay = NativeStateOverlay::new(self.state_db.clone());
            overlay.seed_from_bundle(&bundle);
            if let Some(ref m) = self.metrics {
                m.exec_seed_bundle_seconds
                    .observe(seed_bundle_timer.elapsed().as_secs_f64());
            }

            let ctx_setup_timer = std::time::Instant::now();
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
            // P3 Round-1 item 3: the book load ran inside NativeExecContext::new
            // (before metrics were wired) — publish its timing + decode counters now.
            ctx.record_book_load_metrics();
            // O3: with a background writer present, fills buffer their
            // trade-history KVs (node-local, non-root CFs) instead of paying
            // per-fill overlay PUTs; they are handed over after the flush below.
            ctx.defer_trades = self.trade_writer.is_some();
            if let Some(ref m) = self.metrics {
                m.exec_ctx_setup_seconds
                    .observe(ctx_setup_timer.elapsed().as_secs_f64());
            }

            let engine_timer = std::time::Instant::now();
            NativeExecutor::execute_batch(&mut ctx, &pre_evm);
            NativeExecutor::execute_batch(&mut ctx, &post_evm);
            let _ = NativeExecutor::drain_core_writer(&mut ctx);
            NativeExecutor::process_governance(&mut ctx);
            NativeExecutor::distribute_fees(&mut ctx, computed_fee_revenue);
            NativeExecutor::process_epoch_boundary(&mut ctx);
            if let Some(ref m) = self.metrics {
                m.exec_engine_seconds
                    .observe(engine_timer.elapsed().as_secs_f64());
            }

            let save_books_timer = std::time::Instant::now();
            ctx.save_order_books();
            if let Some(ref m) = self.metrics {
                m.exec_save_books_seconds
                    .observe(save_books_timer.elapsed().as_secs_f64());
            }
            // P3 Round-1 item 3(b): first true resting-depth ground truth — sample
            // the in-memory books right after persistence (getOrderBook is
            // borsh-broken at this base). exec_save_books_bytes is recorded inside
            // save_order_books above.
            ctx.record_resting_depth();

            let flush_timer = std::time::Instant::now();
            for (sender, nonce) in &consumed_nonces {
                let nonce_key = torus_state::cf::native_nonce_key(sender, *nonce);
                let _ = overlay.put_cf_raw(
                    torus_state::cf::CF_NATIVE_NONCES,
                    &nonce_key,
                    &torus_block.header.height.to_be_bytes(),
                );
            }

            // Flush native state AND maintain the incremental native bucketed-Merkle trie in ONE
            // atomic batch (Phase A A2.2). Unconditional like the EVM resync below — the trie is
            // kept current regardless of TORUS_INCREMENTAL_STATE_ROOT so it is ready when the flag
            // flips. A trie-maintenance failure never drops committed native state (the native-CF
            // writes are in the same batch and are written even if the trie ops are skipped); it
            // only leaves the off-by-default incremental native root stale for this block.
            if let Err(e) = overlay.flush_with_native_trie(&self.state_db) {
                tracing::error!(%e, height, "native overlay flush / incremental native trie maintenance failed");
            }

            // Phase A: native post-commit credited EVM account balances (fees / validator rewards)
            // straight to CF_ACCOUNTS via the overlay flush, bypassing incremental trie maintenance.
            // Re-sync those accounts into CF_HASHED_*/CF_TRIE_* so the incremental root keeps tracking
            // the full scan (devnet-smoke finding). Best-effort: a failure only degrades the
            // flag-gated (off-by-default) incremental path, never the committed plain state.
            let native_evm_addrs = overlay.dirty_evm_accounts();
            if let Err(e) =
                torus_state::incremental::resync_evm_accounts(&self.state_db, &native_evm_addrs)
            {
                tracing::error!(%e, height, "failed to resync incremental trie after native post-commit");
            }
            if let Some(ref m) = self.metrics {
                m.exec_flush_seconds
                    .observe(flush_timer.elapsed().as_secs_f64());
            }

            // Session-owner cache invalidation. `put_session` / `delete_session`
            // (native_executor `exec_create_session` / `exec_revoke_session`) are the
            // ONLY operations that change what `get_session` returns, and both were
            // just committed by the flush above. Drop the cached owner for every
            // session_pubkey touched by a CreateSession / RevokeSession in THIS block
            // so the next block's verify MISSes and re-resolves from the authoritative
            // (post-mutation) state — this is what keeps a revoked (or revoke->recreate
            // rebound) session from ever being served stale on a later HIT. Runs on
            // the single exec thread, so it is ordered before the next block's verify.
            // Over-invalidation (a create/revoke that failed) is safe: it only forces
            // a re-resolve. Unconditional on outcome by design.
            let post_native_timer = std::time::Instant::now();
            if self.exec_trust_cache {
                if let Some(m) = self.mempool.as_ref() {
                    for signed in &torus_block.native_actions {
                        match &signed.action {
                            torus_types::NativeAction::CreateSession { session_pubkey, .. }
                            | torus_types::NativeAction::RevokeSession { session_pubkey } => {
                                m.invalidate_session(session_pubkey);
                            }
                            _ => {}
                        }
                    }
                }
            }

            // O3: hand this block's buffered trade-history KVs to the background
            // writer — off the execution thread, after the atomic state flush.
            // Keys are deterministic per block, so a crash-replay rewrite is
            // idempotent; a hard crash can lose the last few queued batches,
            // which is a cosmetic RPC trade-history gap, never consensus state.
            let trades = ctx.take_pending_trades();
            if !trades.is_empty() {
                let fallback = match &self.trade_writer {
                    Some(writer) => writer.send(trades).err(),
                    None => Some(trades),
                };
                // Writer gone (or absent): write synchronously so no rows are lost.
                if let Some(kvs) = fallback {
                    for (cf, key, value) in &kvs {
                        let _ = self.state_db.put_cf_raw(cf, key, value);
                    }
                }
                if let (Some(m), Some(w)) = (&self.metrics, &self.trade_writer) {
                    m.trade_writer_queued_batches.set(w.queued_batches() as i64);
                }
            }
            if let Some(ref m) = self.metrics {
                m.exec_post_native_seconds
                    .observe(post_native_timer.elapsed().as_secs_f64());
            }
        }

        // ---- Persist block body for RPC queries ----
        // P3 Task-1: split serialize vs put so the exec-thread cost of the
        // per-block CF_BLOCK_BODIES JSON write is visible (prime unaccounted
        // suspect — multi-MB serialize under load, every block). The put was
        // 12.7% of exec_block on the bs400 cross-flow leg; with a body_writer
        // (TORUS_ASYNC_BODY_PERSIST) it moves off the exec thread. The exec
        // thread still serializes — the owned bytes are MOVED into the writer
        // channel with no extra copy — and the stored JSON is byte-identical, so
        // every reader (RPC getBlock, pruner, crash-recovery replay) is unchanged.
        let body_serialize_timer = std::time::Instant::now();
        if let Ok(body_bytes) = serde_json::to_vec(&torus_block.body()) {
            if let Some(ref m) = self.metrics {
                m.exec_body_serialize_seconds
                    .observe(body_serialize_timer.elapsed().as_secs_f64());
                m.exec_body_bytes.inc_by(body_bytes.len() as u64);
            }
            let body_put_timer = std::time::Instant::now();
            // Hand the write to the background body writer when present; on a
            // gone/closed writer the batch comes back and we write it inline so a
            // body is never silently lost. `None` = synchronous put (default).
            let fallback = match &self.body_writer {
                Some(writer) => writer
                    .send(vec![(
                        CF_BLOCK_BODIES,
                        height.to_be_bytes().to_vec(),
                        body_bytes,
                    )])
                    .err()
                    .and_then(|kvs| kvs.into_iter().next().map(|(_, _, v)| v)),
                None => Some(body_bytes),
            };
            if let Some(bytes) = fallback {
                let _ = self
                    .state_db
                    .put_cf_raw(CF_BLOCK_BODIES, &height.to_be_bytes(), &bytes);
            }
            if let Some(ref m) = self.metrics {
                // On the async path this only times the send handoff (put is
                // off-thread); on the sync/fallback path it times the real put.
                m.exec_body_put_seconds
                    .observe(body_put_timer.elapsed().as_secs_f64());
                if let Some(w) = &self.body_writer {
                    m.body_writer_queued_batches.set(w.queued_batches() as i64);
                }
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
            m.exec_block_seconds
                .observe(block_timer.elapsed().as_secs_f64());
        }

        tracing::info!(height, "execution pipeline: block done");
    }
}

fn execution_loop(rx: std::sync::mpsc::Receiver<CommittedBlockMsg>, ctx: ExecutionContext) {
    tracing::info!("execution pipeline thread started");
    while let Ok(msg) = rx.recv() {
        ctx.execute_committed_block(&msg.torus_block, msg.pending_slashes);
        // Paired with the inc() at the `exec_tx.send` site: dec AFTER execution
        // so the gauge counts queued + in-flight blocks (pinned near the channel
        // bound 64 = execution is the bottleneck). exec_queue_out is the counter
        // twin (P2 funnel item 4): in − out = occupancy, race-proof across
        // scrapes, and the out RATE is exec-thread block throughput.
        if let Some(ref m) = ctx.metrics {
            m.exec_queue_depth.dec();
            m.exec_queue_out.inc();
        }
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
/// In-flight native-action hashes by height — every proposal seen (ours and
/// other leaders'), INCLUDING compact proposals whose bodies could not be
/// reconstructed (`MissingData`), which `pending_proposals` cannot track
/// because it stores full bodies. Same-height re-proposals union instead of
/// overwrite. Heights are evicted by the same h+10 sliding window as
/// `pending_proposals` and cleared on commit. Closes the duplicate-inclusion
/// tail (factor 1.08–1.27 measured s355, mem c0f4f938) left by
/// body-dependent tracking.
#[derive(Default)]
struct InFlightHashLedger {
    by_height: std::collections::HashMap<u64, std::collections::HashSet<torus_types::B256>>,
}

impl InFlightHashLedger {
    fn note<I: IntoIterator<Item = torus_types::B256>>(&mut self, height: u64, hashes: I) {
        self.by_height.entry(height).or_default().extend(hashes);
        self.by_height.retain(|&h, _| h + 10 > height);
    }

    fn clear(&mut self, height: u64) {
        self.by_height.remove(&height);
    }

    fn extend_into(&self, out: &mut std::collections::HashSet<torus_types::B256>) {
        for hashes in self.by_height.values() {
            out.extend(hashes.iter().copied());
        }
    }
}

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
    pending_proposals: std::collections::HashMap<u64, TorusBlock>,
    in_flight_hashes: InFlightHashLedger,
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
    leader_state: Arc<LeaderState>,
    pre_proposal_tx: Option<std::sync::mpsc::SyncSender<PreProposalBundle>>,
    /// RARE pull-fallback transport (Task 6): fetch missing native-action bodies
    /// by-hash on a reconstruction miss. Injected at startup over `/torus/native-da/1.0`;
    /// `None` in consensus-only tests (no fetch fires).
    da_fetcher: Option<Arc<dyn NativeDaFetcher>>,
    /// BS-4a: off-thread recovery of push-missed native-action bodies. The hot
    /// validate path hands true push misses here (non-blocking) and fails the
    /// view with MissingData; the worker pulls with the roomier ~1 s WORKER
    /// budget so the re-proposed view finds the bodies durably local. `None`
    /// when the mempool or fetcher is absent (consensus-only tests) — a miss
    /// then fails the view without a handoff, same as before BS-4a.
    da_recovery: Option<DaRecoveryWorker>,
    /// RH4 durable DA-body GC. `> 0` enables deferred deletion of mirrored
    /// native-action bodies (`CF_NATIVE_PENDING`) `da_gc_retention` blocks after
    /// their commit height — the point past which neither a local reconstruct nor
    /// a lagging peer's push-race recovery can still need them. `0` = disabled
    /// (archive; the mirror stays write-only, pre-RH4 behavior). From
    /// `TORUS_DA_BODY_RETENTION` (default 128).
    da_gc_retention: u64,
    /// FIFO of `(commit_height, committed_action_hashes)` awaiting GC. Drained
    /// front-to-back each commit as heights fall outside the retention window;
    /// bounded by `da_gc_retention` heights of committed actions.
    da_gc_pending: std::collections::VecDeque<(u64, Vec<torus_types::B256>)>,
}

/// Actions the proposer pushes to validators via unicast before broadcasting CompactBlock.
pub struct PreProposalBundle {
    pub actions: Vec<(Address, torus_types::SignedNativeAction)>,
}

/// RARE pull-fallback transport for native-action DA bodies (Phase C Task 6).
///
/// Injected from torus-node over the libp2p `/torus/native-da/1.0` protocol so
/// torus-consensus needn't depend on torus-network. Used only on a reconstruction
/// MISS — push covers the common case, so this fires rarely (mem a6cf33a9: pull is
/// fallback-only, never per-block).
///
/// `fetch` is **non-blocking** (it only enqueues a request to the network thread).
/// `drain` returns the bodies that have arrived since the last call (each =
/// `bincode(SignedNativeAction)`). Target selection (which peers to ask) is the
/// implementation's concern, keeping the consensus layer free of peer identity.
pub trait NativeDaFetcher: Send + Sync {
    /// Request the given action-hashes from peers. Non-blocking.
    fn fetch(&self, hashes: Vec<[u8; 32]>);
    /// Drain native-action bodies received so far. Non-blocking.
    fn drain(&self) -> Vec<Vec<u8>>;
}

/// CONSENSUS-CRITICAL version flag: emit hash-only `CompactBlock` proposals
/// (`true`) instead of full self-contained `TorusBlock`s (`false`).
///
/// The consensus block identity is `data_hash` over the datum bytes, so compact
/// and full encodings hash DIFFERENTLY — every validator MUST agree on this value
/// or they split. It is therefore tied to the BINARY version (not env/per-node):
/// flip it only in a coordinated relaunch where every validator runs the fixed
/// binary (Phase C Task 9). Re-enabling compact is what unlocks 400k orders/sec — a
/// block referencing ~20–40k orders ≈ 1–2 MB of bodies far exceeds the 256 KB
/// `max_consensus_message_size`, so bodies must travel out-of-band (durable DA
/// store + push T7 + rare pull T6). Default `false` keeps the proven full-block
/// path until that coordinated flip. Does NOT affect the EVM/RPC header hash
/// (`keccak256(canonical_header_bytes)`), which is identical in both encodings.
///
/// ENABLED (Task 9): all validators MUST run this binary in a coordinated relaunch.
/// Deploying it to a SUBSET while others run a full-block binary splits consensus
/// (different `data_hash` for the same block). Do NOT deploy piecemeal.
const COMPACT_PROPOSALS: bool = true;

/// Proposer-side near-expiry margin (milliseconds) for the Option-A session
/// filter. At block production the proposer skips a session-signed action whose
/// session expiry is within this margin of the proposal time (i.e. it includes
/// the action only if `expiry >= now_ms + PROPOSAL_EXPIRY_MARGIN_MS`), so it never
/// proposes an action execution would later reject as expired.
///
/// This is a proposer-LOCAL policy, NOT a consensus rule: different proposers may
/// filter differently and the chain still agrees (execution — with the
/// `SESSION_EXPIRY_EXEC_GRACE_MS` grace window — is the single source of truth).
/// It is therefore safe to keep local and does not affect determinism. Combined
/// with the 120 s exec grace, an action that passes this 60 s filter has ≥60 s to
/// expiry at proposal and execution rejects only past expiry+120 s, so an honest
/// proposer would be slashed only on a > 180 s proposal→commit gap (pathological).
const PROPOSAL_EXPIRY_MARGIN_MS: u64 = 60_000;

/// Proposer-side near-expiry filter (Option A). Returns `(kept, filtered_count)`:
/// `kept` is `selected` minus any session-signed action whose session expiry is
/// within `margin_ms` of `now_ms` (i.e. keeps it only if
/// `expiry >= now_ms + margin_ms`). EIP-712 actions are always kept.
/// `session_expiry(pubkey)` returns the session's expiry in ms, or `None` if the
/// session can't be resolved — in which case the action is KEPT (execution is the
/// authority; the filter is best-effort). Expiry is resolved at most once per
/// unique pubkey.
///
/// This is a pure transform over the SELECTED list; it never touches the mempool,
/// so a filtered action is NOT consumed and stays eligible for later blocks.
fn filter_near_expiry_sessions(
    selected: Vec<(Address, torus_types::SignedNativeAction)>,
    now_ms: u64,
    margin_ms: u64,
    session_expiry: impl Fn(&[u8; 32]) -> Option<u64>,
) -> (Vec<(Address, torus_types::SignedNativeAction)>, u64) {
    let cutoff_ms = now_ms.saturating_add(margin_ms);
    let mut cache: std::collections::HashMap<[u8; 32], Option<u64>> =
        std::collections::HashMap::new();
    let mut filtered: u64 = 0;
    let kept = selected
        .into_iter()
        .filter(|(_, action)| {
            let torus_types::ActionSignature::Session { session_pubkey, .. } = &action.signature
            else {
                return true; // EIP-712 action: not session-expiry-gated
            };
            let expiry = cache
                .entry(*session_pubkey)
                .or_insert_with(|| session_expiry(session_pubkey));
            match expiry {
                Some(exp) if *exp >= cutoff_ms => true, // healthy: keep
                Some(_) => {
                    filtered += 1;
                    false // within the margin of expiry: skip for this block
                }
                None => true, // unresolved: keep, execution decides
            }
        })
        .collect();
    (kept, filtered)
}

/// Encode the datum carried in a consensus proposal.
///
/// With `compact = false` (default): the FULL, self-contained `TorusBlock` (native
/// actions inline) — needs no out-of-band delivery and is self-contained for
/// block-sync. The proven full-block path (06620a1) that ran healthy at cap-100.
///
/// With `compact = true` (version-gated, Task 8): a hash-only `CompactBlock`. The
/// bodies are NOT inline — they are mirrored to the durable DA store (T2), pushed
/// to validators (T7), and fetched on a miss via the rare pull-fallback (T6). This
/// shrinks the proposal so it fits `max_consensus_message_size` at 400k orders/sec.
/// All validators must emit the SAME encoding (see [`COMPACT_PROPOSALS`]).
fn encode_proposal_datum(block: &TorusBlock, compact: bool) -> Vec<u8> {
    if compact {
        bincode::serialize(&CompactBlock::from_block(block)).expect("serialize CompactBlock")
    } else {
        bincode::serialize(block).expect("serialize TorusBlock")
    }
}

/// Pull-fallback poll budget for [`TorusApp::pull_missing_bodies`]: drain-and-absorb
/// is retried `PULL_RETRIES` times, sleeping `PULL_DELAY` between attempts, so the
/// effective wait = `PULL_RETRIES * PULL_DELAY` = ~1 s. This runs ONLY on the
/// block-sync path (never the consensus voting hot path), so blocking up to ~1 s is
/// safe — and necessary: the original 80 ms (4 × 20 ms) gave up before a >4 MB
/// chunked body-set could land, contributing to the wedge (native-DA fix Task 3).
const PULL_RETRIES: usize = 20;
const PULL_DELAY: std::time::Duration = std::time::Duration::from_millis(50);

/// Ceiling for the size-aware sync pull budget: 160 × 50 ms = 8 s. Large
/// batched blocks carry multi-MB body-sets that cannot land within the flat
/// ~1 s budget on WAN links (s334 bs1000); the sync path may safely block
/// longer because it is off the consensus voting path.
const MAX_SYNC_PULL_RETRIES: usize = 160;

/// Sync-path pull budget scaled by how many bodies are missing (~2 bodies per
/// 50 ms tick), clamped to [`PULL_RETRIES`, `MAX_SYNC_PULL_RETRIES`] (~1–8 s).
fn sync_pull_retries(missing: usize) -> usize {
    (missing / 2).clamp(PULL_RETRIES, MAX_SYNC_PULL_RETRIES)
}

/// Shared bounded fetch-wait-absorb loop: used by the SYNC path (in-line, ~1-8s)
/// and the BS-4a recovery worker (off-thread, ~1s). Wake-on-arrival (S391),
/// deadline-bounded (S395), with ONE event-driven mid-budget re-fetch (BS-4b).
fn recover_bodies_bounded(
    mempool: &Mempool,
    fetcher: &dyn NativeDaFetcher,
    missing: &[torus_types::B256],
    retries: usize,
    delay: std::time::Duration,
    metrics: Option<&torus_telemetry::Metrics>,
    cancel: Option<&std::sync::atomic::AtomicBool>,
) -> bool {
    if missing.is_empty() {
        return true;
    }

    // Pre-warm fast path (review F2): a body pushed-as-hash may already be sitting in the
    // fetcher inbound (the receiver pre-warm-pulled it) but not yet absorbed — e.g. it
    // landed just after the hot local-retry's last drain. Absorb + re-check BEFORE issuing
    // a network fetch, so a pre-warmed (or late) body never triggers a redundant fetch.
    TorusApp::absorb_fetched_bodies(mempool, fetcher);
    if missing.iter().all(|h| mempool.get_native_da(h).is_some()) {
        return true;
    }

    let hashes: Vec<[u8; 32]> = missing.iter().map(|h| h.0).collect();
    // S391 wake-on-arrival: snapshot before firing the fetch so a push that
    // races the pull wakes the first wait. Pull responses themselves land in
    // the fetcher inbound and still need this thread's absorb, so the slices
    // keep the old `delay` cadence as the worst case.
    let mut seen = torus_state::NativeDaStore::arrival_generation();
    fetcher.fetch(hashes);

    if let Some(m) = metrics {
        m.native_da_pull_requests.inc();
    }

    // Deadline-bounded loop (S395): a loaded scheduler overshoots each individual
    // wait slice, and with per-ITERATION bounds those overshoots stack (13 x 20ms
    // nominal was observed at 500ms+ wall under load). Bounding by wall-clock
    // deadline keeps the total budget honest regardless of load.
    let budget = delay * retries as u32;
    let deadline = std::time::Instant::now() + budget;
    // BS-4b: one event-driven mid-budget re-fetch (fires once at `refetch_at`).
    let refetch_at = deadline - budget / 2;
    let mut refetched = false;
    loop {
        // BS-4a worker shutdown: abandon the in-flight budget the moment the caller
        // cancels — a dying node has no use for the bodies (a dropped pull just
        // re-fires on the re-proposed view). Bounds Drop-join to ~one slice + O(1).
        // The SYNC caller passes None, so its behavior is bit-identical.
        if cancel.is_some_and(|c| c.load(std::sync::atomic::Ordering::Relaxed)) {
            return false;
        }
        let now = std::time::Instant::now();
        if now >= deadline {
            break;
        }
        torus_state::NativeDaStore::wait_for_arrival(seen, std::cmp::min(delay, deadline - now));
        TorusApp::absorb_fetched_bodies(mempool, fetcher);
        if missing.iter().all(|h| mempool.get_native_da(h).is_some()) {
            if let Some(m) = metrics {
                m.native_da_pull_recovered.inc();
            }
            tracing::info!(count = missing.len(), "native-da pull: bodies recovered");
            return true;
        }
        // BS-4b: one event-driven mid-budget re-fetch — covers a lost pull request or
        // response (the fan-out already rotates validators, bridge.rs:390). Fires at
        // most once, and only for bodies STILL absent at the midpoint, so an already-
        // absorbed body is never re-requested (pull stays rare, mem a6cf33a9). Placed
        // after the absorb+recheck so the still-missing filter sees fresh state, and
        // unreachable when the body arrived earlier (the return above already fired).
        if !refetched && std::time::Instant::now() >= refetch_at {
            let still: Vec<[u8; 32]> = missing
                .iter()
                .filter(|h| mempool.get_native_da(h).is_none())
                .map(|h| h.0)
                .collect();
            if !still.is_empty() {
                fetcher.fetch(still);
            }
            refetched = true;
        }
        // Fold in our own absorb puts so they don't self-wake the next wait.
        seen = torus_state::NativeDaStore::arrival_generation();
    }
    tracing::warn!(
        count = missing.len(),
        "native-da pull: bodies NOT recovered within budget"
    );
    false
}

/// BS-4a recovery-worker pull budget: 50 × 20 ms = ~1 s, sync-parity (matches the
/// flat `PULL_RETRIES * PULL_DELAY` sync budget). OPEN QUESTION: 1 s (sync parity)
/// vs 2 s (cover a slow re-propose cycle so the re-proposed view is guaranteed to
/// find the body locally) — start at sync parity, tune on devnet. The worker runs
/// OFF the consensus thread, so widening this later costs no hot-path latency.
const WORKER_PULL_RETRIES: usize = 50;
const WORKER_PULL_DELAY: std::time::Duration = std::time::Duration::from_millis(20);

/// BS-4a: owns the off-thread recovery of push-missed native-action bodies.
/// The consensus thread hands missing hashes here and votes MissingData
/// immediately; this thread runs the (event-driven, deadline-bounded) pull
/// loop with the roomier WORKER budget, so the re-proposed view finds the
/// bodies locally. Sender-drop => recv Err => thread exits (O3 pattern).
///
/// `tx` is an `Option` (sanctioned deviation from the bare-`Sender` sketch):
/// `Drop` must close the channel BEFORE joining, and the only way to drop a
/// field early is `Option::take` — same shape as the O3 `BackgroundCfWriter`
/// (torus-state/src/bg_writer.rs).
struct DaRecoveryWorker {
    tx: Option<std::sync::mpsc::SyncSender<Vec<torus_types::B256>>>,
    handle: Option<std::thread::JoinHandle<()>>,
    shutdown: Arc<std::sync::atomic::AtomicBool>,
}

/// Bounded in-flight batch queue. Under a sustained-miss / partition regime,
/// submits arrive at view cadence (~2/s) while the worker drains at ~1/s (each
/// unrecoverable batch burns the full ~1 s budget), so an UNBOUNDED channel would
/// grow without limit AND make shutdown drain block queue_depth × ~1 s. Capping at
/// 8 bounds both: memory (≤ 8 queued batches) and shutdown latency (see `Drop`).
/// Since Fix A the worker COALESCES all pending batches into one cycle (drains up
/// to cap+1 per ~1 s), so a full queue is pathological-only and dropping a Full
/// try_send is truly harmless — the re-proposed view resubmits those hashes, which
/// get coalesced into the next cycle rather than starving behind the backlog.
const WORKER_QUEUE_CAP: usize = 8;

impl DaRecoveryWorker {
    /// Spawn the recovery thread. It owns clones of the mempool/fetcher Arcs, so
    /// it needs nothing from `TorusApp` and stays alive until the channel closes.
    fn spawn(
        mempool: Arc<Mempool>,
        fetcher: Arc<dyn NativeDaFetcher>,
        metrics: Option<Arc<torus_telemetry::Metrics>>,
    ) -> Self {
        let (tx, rx) = std::sync::mpsc::sync_channel::<Vec<torus_types::B256>>(WORKER_QUEUE_CAP);
        let shutdown = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let worker_shutdown = shutdown.clone();
        let handle = std::thread::Builder::new()
            .name("torus-da-recovery".into())
            .spawn(move || {
                // recv() yields queued batches even after the sender is dropped,
                // then errors — the shutdown flag below turns that drain into an
                // early exit so a dying node never spends budget on stale work.
                while let Ok(mut batch) = rx.recv() {
                    // Shutdown wins immediately: queued recovery work is worthless
                    // on a node that is going down. Unlike O3's DB writes there is
                    // NO data-loss concern here (a dropped pull just re-fires on the
                    // re-proposed view), so drain-on-shutdown is intentionally NOT
                    // wanted — bail before spending a ~1 s budget on a dying node.
                    if worker_shutdown.load(std::sync::atomic::Ordering::Relaxed) {
                        break;
                    }
                    // BS-4a Fix A (coalesce): drain everything else pending into THIS
                    // cycle before the dedup, so a fresh recoverable batch never
                    // starves behind a queue of dead ones (each of which would burn
                    // the full ~1 s budget FIFO). The queue drains in gulps of up to
                    // cap+1 batches per ~1 s cycle, so a Full try_send is now
                    // pathological-only and the drop-is-harmless claim holds: a
                    // dropped resubmit is coalesced into the next cycle.
                    while let Ok(more) = rx.try_recv() {
                        batch.extend(more);
                    }
                    // Dedup the merged set: the same hash resubmitted across views (or
                    // shared between coalesced batches) would otherwise appear multiple
                    // times — recover_bodies_bounded iterates `missing` per tick, so
                    // duplicates are wasteful, not harmful. Then drop hashes already
                    // durably local (late push, pre-warm, an earlier merged batch) so
                    // we never fire a redundant network fetch.
                    batch.sort_unstable();
                    batch.dedup();
                    let still_missing: Vec<torus_types::B256> = batch
                        .into_iter()
                        .filter(|h| mempool.get_native_da(h).is_none())
                        .collect();
                    if still_missing.is_empty() {
                        continue;
                    }
                    // A recoverable hash merged with never-arriving (dead) hashes is
                    // still recovered mid-loop: absorb_fetched_bodies stores each body
                    // the moment it arrives, even though the batch's overall return is
                    // false when the dead hashes never land. `Some(&worker_shutdown)`
                    // lets Drop abandon the in-flight budget at the next slice (Fix C).
                    let recovered = recover_bodies_bounded(
                        &mempool,
                        fetcher.as_ref(),
                        &still_missing,
                        WORKER_PULL_RETRIES,
                        WORKER_PULL_DELAY,
                        metrics.as_deref(),
                        Some(&worker_shutdown),
                    );
                    // A cancelled in-flight batch (shutdown) is not a budget timeout —
                    // only count as a timeout when the budget genuinely expired without
                    // recovering every body and we are NOT shutting down.
                    if !recovered && !worker_shutdown.load(std::sync::atomic::Ordering::Relaxed) {
                        if let Some(ref m) = metrics {
                            m.native_da_recovery_timeouts.inc();
                        }
                    }
                }
            })
            .expect("spawn torus-da-recovery thread");
        Self {
            tx: Some(tx),
            handle: Some(handle),
            shutdown,
        }
    }

    /// Hand a batch of missing hashes to the worker. Non-blocking (`try_send`):
    /// the consensus thread calls this, so it must NEVER block or panic. On a full
    /// queue (recovery already saturated) or a dead worker the batch is dropped with
    /// a warning — harmless, the re-proposed view resubmits the same hashes.
    ///
    /// Returns `true` iff the batch was actually queued, so the caller can gate the
    /// `native_da_recovery_handoffs` counter on real handoffs — a dead worker must
    /// not paint the A/B dashboard green (final-review finding 1).
    fn submit(&self, hashes: Vec<torus_types::B256>) -> bool {
        use std::sync::mpsc::TrySendError;
        let Some(tx) = &self.tx else {
            tracing::warn!("da-recovery worker: sender already closed; dropping batch");
            return false;
        };
        match tx.try_send(hashes) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) => {
                tracing::warn!("da-recovery worker queue full; dropping missing-hash batch");
                false
            }
            Err(TrySendError::Disconnected(_)) => {
                tracing::warn!("da-recovery worker thread gone; dropping missing-hash batch");
                false
            }
        }
    }
}

impl Drop for DaRecoveryWorker {
    fn drop(&mut self) {
        // Signal shutdown BEFORE closing the channel: the worker checks the flag
        // right after each recv() Ok and bails, so batches still queued (≤
        // WORKER_QUEUE_CAP) are DISCARDED in O(1), not drained (no data-loss concern
        // here — dropped pulls re-fire on the re-proposed view). Since Fix C the
        // in-flight batch inside recover_bodies_bounded also observes this flag
        // (passed as `cancel`) at the top of its wait loop, so it abandons its
        // budget at the NEXT ~20 ms slice boundary instead of running the full
        // ~1 s. Join is therefore bounded by ~one slice + O(1). Closing tx then
        // unblocks a worker parked in recv().
        self.shutdown
            .store(true, std::sync::atomic::Ordering::Relaxed);
        drop(self.tx.take());
        if let Some(h) = self.handle.take() {
            // A worker panic must not propagate out of Drop — log, never unwrap.
            if h.join().is_err() {
                tracing::warn!("da-recovery worker thread panicked");
            }
        }
    }
}

/// Bounded LOCAL retry on the hot validate path (BS-4a): ONE 20 ms wake-on-arrival
/// slice, just enough to catch the racing pre-proposal PUSH that lands right after
/// the CompactBlock (the common case; keeps recovery rare). OPEN QUESTION: the
/// racing-push window distribution is unmeasured — measure on devnet before
/// finalizing the consts (1 vs 2 slices). The old 5 × 20 ms in-line budget moved
/// off-thread to the recovery worker (`WORKER_PULL_RETRIES`/`WORKER_PULL_DELAY`).
const RECONSTRUCT_RETRIES: usize = 1;
const RECONSTRUCT_RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(20);

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
        let genesis_validator_set =
            EpochManager::compute_new_validator_set(&staking, config.max_validators, 0)
                .unwrap_or_else(|_| ValidatorSet {
                    validators: vec![],
                    epoch: 0,
                });

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
            mempool: mempool.clone(),
            exec_trust_cache: config.exec_trust_cache,
            // O3: 256 queued blocks of trade KVs max — a full queue blocks the
            // execution thread (backpressure) instead of ballooning memory.
            trade_writer: Some(torus_state::BackgroundCfWriter::spawn(
                state_db.clone(),
                "torus-trade-writer",
                256,
            )),
            // P3 Task-1: opt-in async block-body persistence. When
            // TORUS_ASYNC_BODY_PERSIST is truthy, the per-block CF_BLOCK_BODIES
            // put moves onto this writer's thread (12.7% of exec_block on the
            // bs400 cross-flow leg); a full 256-block queue backpressures the
            // exec thread instead of ballooning memory. Default off = the
            // synchronous put, byte-identical to prior behavior — the OFF leg of
            // the A/B runs the same binary with the flag unset.
            body_writer: if env_flag("TORUS_ASYNC_BODY_PERSIST") {
                tracing::info!("async block-body persistence ENABLED (CF_BLOCK_BODIES put off exec thread)");
                Some(torus_state::BackgroundCfWriter::spawn(
                    state_db.clone(),
                    "torus-body-writer",
                    256,
                ))
            } else {
                None
            },
        };

        // Phase A: ensure the persistent incremental trie exists before any commit (including
        // replay below). No-op after the first boot; keeps the incremental root's base ready while
        // the full-scan root stays primary until TORUS_INCREMENTAL_STATE_ROOT is enabled.
        if let Err(e) = torus_state::incremental::ensure_trie_built(&state_db) {
            tracing::warn!(%e, "failed to build initial state trie (incremental root unavailable until rebuilt)");
        }
        // Phase A A2.2: same for the native bucketed-Merkle trie. No-op after first boot; keeps the
        // incremental native root's base ready while the full-scan native root stays primary until
        // TORUS_INCREMENTAL_STATE_ROOT is enabled.
        if let Err(e) = torus_state::native_trie::ensure_native_trie_built(&state_db) {
            tracing::warn!(%e, "failed to build initial native trie (incremental native root unavailable until rebuilt)");
        }

        // Crash recovery runs synchronously before spawning the pipeline.
        let last_header = Self::replay_committed(&state_db, &exec_ctx);

        // Spawn execution pipeline: bounded channel (64 blocks) for backpressure.
        let (exec_tx, exec_rx) = std::sync::mpsc::sync_channel(64);
        let exec_handle = std::thread::Builder::new()
            .name("torus-execution".into())
            .spawn(move || execution_loop(exec_rx, exec_ctx))
            .expect("spawn execution pipeline thread");

        let leader_state = Arc::new(LeaderState::new());
        leader_state.sync_validators(&genesis_validator_set);
        leader_state.set_view(last_header.height.saturating_add(1));

        Self {
            state_db,
            proposer,
            validator,
            evm_executor: EvmExecutor::new(config.chain_id),
            proposer_address: signing_key.as_ref().and_then(|sk| {
                let pubkey = sk.verifying_key();
                staking.find_validator_by_pubkey(pubkey.as_bytes())
                    .ok()
                    .flatten()
                    .map(|v| {
                        tracing::info!(address = %v.address, "resolved proposer address from signing key");
                        v.address
                    })
            }).unwrap_or(Address::ZERO),
            last_header,
            staking,
            epoch_length: config.epoch_length,
            max_validators: config.max_validators,
            last_validator_set: genesis_validator_set,
            cached_vs_updates: None,
            pending_slashes: Vec::new(),
            pending_proposals: std::collections::HashMap::new(),
            in_flight_hashes: InFlightHashLedger::default(),
            treasury_address: config.treasury_address,
            dev_pool_address: config.dev_pool_address,
            signing_key,
            metrics,
            mempool,
            exec_tx: Some(exec_tx),
            exec_handle: Some(exec_handle),
            leader_state,
            pre_proposal_tx: None,
            da_fetcher: None,
            da_recovery: None,
            da_gc_retention: da_body_retention(),
            da_gc_pending: std::collections::VecDeque::new(),
        }
    }

    pub fn leader_state(&self) -> Arc<LeaderState> {
        self.leader_state.clone()
    }

    pub fn set_pre_proposal_tx(&mut self, tx: std::sync::mpsc::SyncSender<PreProposalBundle>) {
        self.pre_proposal_tx = Some(tx);
    }

    /// Attach the RARE pull-fallback transport (Task 6). Called once at startup with
    /// a handle over the `/torus/native-da/1.0` protocol.
    ///
    /// BS-4a: when a mempool (durable DA store) is wired too, this also spawns the
    /// off-thread `DaRecoveryWorker` over the same fetcher. Calling this again
    /// replaces the worker — the assignment drops the previous one, whose `Drop`
    /// joins the thread (bounded ≤ ~1 s), so no thread leaks. With no mempool the
    /// fetcher is stored but the worker stays `None`: a hot-path miss then fails
    /// the view without a handoff, same as the pre-BS-4a no-mempool behavior.
    pub fn set_native_da_fetcher(&mut self, fetcher: Arc<dyn NativeDaFetcher>) {
        if let Some(ref mempool) = self.mempool {
            self.da_recovery = Some(DaRecoveryWorker::spawn(
                mempool.clone(),
                fetcher.clone(),
                self.metrics.clone(),
            ));
        }
        self.da_fetcher = Some(fetcher);
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
            reputation_leader_selection: false,
            exec_trust_cache: false,
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

        let body: TorusBlockBody =
            match state_db.get_cf_raw(CF_BLOCK_BODIES, &committed.to_be_bytes()) {
                Ok(Some(data)) => match serde_json::from_slice(&data) {
                    Ok(b) => b,
                    Err(e) => {
                        tracing::error!(%e, "crash recovery: failed to deserialize block body");
                        write_native_applied_height(state_db, committed);
                        return last_header;
                    }
                },
                _ => {
                    tracing::info!(
                        height = committed,
                        "crash recovery: no block body (empty block), marking applied"
                    );
                    write_native_applied_height(state_db, committed);
                    return last_header;
                }
            };

        if body.native_actions.is_empty() && body.evm_transactions.is_empty() {
            tracing::info!(
                height = committed,
                "crash recovery: empty block, marking applied"
            );
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

    /// Reconstruct a `CompactBlock`'s full body set from the durable DA store.
    ///
    /// Returns `Ok(TorusBlock)` when every referenced native-action body is present
    /// in the DA store, or `Err(missing_hashes)` listing the bodies that must be
    /// fetched (the rare pull-fallback, Task 6). Reads the DURABLE store -- NOT the
    /// ephemeral mempool -- so a block-referenced body survives the 60s nonce
    /// window, pool eviction, and a restart (livelock root cause, mem 28e1a821).
    fn reconstruct_compact_from_da(
        &self,
        compact: &CompactBlock,
    ) -> Result<TorusBlock, Vec<torus_types::B256>> {
        let mut native_actions = Vec::with_capacity(compact.native_action_hashes.len());
        let mut missing = Vec::new();
        if !compact.native_action_hashes.is_empty() {
            match self.mempool {
                Some(ref mempool) => {
                    for hash in &compact.native_action_hashes {
                        match mempool.get_native_da(hash) {
                            Some(action) => native_actions.push(action),
                            None => missing.push(*hash),
                        }
                    }
                }
                None => missing = compact.native_action_hashes.clone(),
            }
        }
        if !missing.is_empty() {
            return Err(missing);
        }
        Ok(TorusBlock {
            header: compact.header.clone(),
            native_actions,
            evm_transactions: compact.evm_transactions.clone(),
            core_writer_actions: compact.core_writer_actions.clone(),
        })
    }

    /// RARE pull-fallback entry point (Task 6): if any of `compact`'s native-action
    /// bodies are absent from the durable DA store, fetch them by-hash and absorb the
    /// response into the store. Returns `true` iff a fetch was issued.
    ///
    /// Does NOTHING when every body is already local — the common case, so pull fires
    /// rarely (mem a6cf33a9: pull is fallback-only, never per-block). Intended for the
    /// SYNC path (`validate_block_for_sync`); the hot `validate_block` path keeps only
    /// its bounded local retry and never triggers a network fetch (mem 8ee99db3).
    pub fn pull_compact_bodies_if_missing(&self, compact: &CompactBlock) -> bool {
        let missing = match self.reconstruct_compact_from_da(compact) {
            Ok(_) => return false, // all bodies local — no fetch (rarity)
            Err(missing) => missing,
        };
        // A miss: attempt the bounded pull (recovery is re-checked by the caller's
        // subsequent reconstruct/validate). Report that a fetch was issued.
        self.pull_missing_bodies(&missing);
        true
    }

    /// Sync-path wrapper over [`Self::pull_missing_bodies_bounded`] with the ~1 s budget
    /// (`PULL_RETRIES` × `PULL_DELAY`). Safe to block here — the block-sync path is off
    /// the consensus voting hot path (mem 8ee99db3). The earlier 80 ms budget gave up
    /// before a >4 MB body-set (now pulled in chunks) could arrive; ~1 s recovers a late
    /// body.
    fn pull_missing_bodies(&self, missing: &[torus_types::B256]) -> bool {
        self.pull_missing_bodies_bounded(missing, sync_pull_retries(missing.len()), PULL_DELAY)
    }

    /// Fetch the `missing` native-action bodies by-hash and absorb any that arrive into
    /// the durable DA store, polling `retries` times with `delay` between drains. Returns
    /// `true` once all are present (or were already), `false` on timeout.
    ///
    /// The budget is the caller's — and since BS-4a the only caller is the SYNC path,
    /// with the ~1 s `PULL_RETRIES`/`PULL_DELAY` (safe to block off the voting path).
    /// The HOT validate path no longer pulls in-line; its off-thread recovery worker
    /// calls `recover_bodies_bounded` directly with the WORKER budget.
    fn pull_missing_bodies_bounded(
        &self,
        missing: &[torus_types::B256],
        retries: usize,
        delay: std::time::Duration,
    ) -> bool {
        if missing.is_empty() {
            return true; // nothing to recover, even with no transport wired
        }
        let (Some(fetcher), Some(mempool)) = (self.da_fetcher.as_ref(), self.mempool.as_ref())
        else {
            return false; // no transport/store wired (consensus-only tests)
        };
        recover_bodies_bounded(
            mempool,
            fetcher.as_ref(),
            missing,
            retries,
            delay,
            self.metrics.as_deref(),
            None, // SYNC path is uncancellable — bit-identical to pre-BS-4a
        )
    }

    /// Reconstruct a CompactBlock's native-action bodies for the HOT validate path
    /// (BS-4a). Bodies travel out-of-band (proposer PUSH → durable DA store), so a
    /// CompactBlock can reference a body the local store does not have yet:
    ///
    /// 1. Fast local lookup, then ONE bounded wake-on-arrival slice — a racing
    ///    pre-proposal PUSH usually lands here (the common case; keeps recovery rare).
    /// 2. On a remaining miss (a true push miss), a NON-BLOCKING handoff to the
    ///    `DaRecoveryWorker` plus fail-fast `Err` — not an in-line pull. The worker
    ///    owns recovery (off-thread, ~1 s budget), so the re-proposed view finds
    ///    the bodies durably local.
    ///
    /// Consensus-thread budget: one ~20 ms slice plus bookkeeping. Returns the
    /// reconstructed actions (in `hashes` order) when every body is present, or
    /// `Err(missing_count)` so the caller votes MissingData — NOT Invalid, no
    /// blacklisting (mem 28e1a821 lineage): the block is simply re-proposed next
    /// view. Only call with a non-empty `hashes`.
    fn reconstruct_native_actions_hot(
        &self,
        hashes: &[torus_types::B256],
    ) -> Result<Vec<torus_types::SignedNativeAction>, usize> {
        let Some(ref mempool) = self.mempool else {
            return Err(hashes.len()); // no DA store wired (consensus-only observer)
        };

        let mut actions: Vec<Option<torus_types::SignedNativeAction>> = vec![None; hashes.len()];
        let mut missing: Vec<usize> = Vec::new();
        for (i, hash) in hashes.iter().enumerate() {
            // Read the DURABLE DA store, not the ephemeral nonce-gated mempool: a
            // block-referenced body survives the 60s nonce window, pool eviction, and a
            // restart (livelock root cause, mem 28e1a821).
            match mempool.get_native_da(hash) {
                Some(action) => actions[i] = Some(action),
                None => missing.push(i),
            }
        }

        // (1) Bounded LOCAL retry: actions are normally delivered by the proposer's
        // pre-proposal unicast push (PreProposalBundle -> BroadcastNativeActions) before
        // this proposal arrives, but that push can race the CompactBlock under load.
        // Poll briefly so a late push lands before we resort to the network.
        if !missing.is_empty() {
            // S391 wake-on-arrival: block on the DA-store arrival notifier with the
            // same 20ms slice instead of a fixed sleep — a racing push wakes this
            // thread at delivery time, while the worst case stays the old tick
            // cadence (one ~20ms slice since BS-4a shrank RECONSTRUCT_RETRIES).
            let mut seen = torus_state::NativeDaStore::arrival_generation();
            // Deadline-bounded like the pull loop below (S395): iteration-count
            // bounds stack scheduler overshoot past the hot budget under load.
            let deadline =
                std::time::Instant::now() + RECONSTRUCT_RETRY_DELAY * RECONSTRUCT_RETRIES as u32;
            loop {
                let now = std::time::Instant::now();
                if now >= deadline {
                    break;
                }
                torus_state::NativeDaStore::wait_for_arrival(
                    seen,
                    std::cmp::min(RECONSTRUCT_RETRY_DELAY, deadline - now),
                );
                // Phase 2.3 pre-warm (#5): a hash-only push made THIS node PULL the bodies
                // out-of-band (the receiver fired the by-hash fetch on the manifest), so they
                // arrive in the fetcher inbound. Absorb them into the DA store here, in the
                // fast local retry, so a pre-warmed body is picked up WITHOUT the redundant
                // hot network fetch below — the common case once the proposer pushes hashes.
                if let Some(ref fetcher) = self.da_fetcher {
                    Self::absorb_fetched_bodies(mempool, fetcher.as_ref());
                }
                missing.retain(|&i| match mempool.get_native_da(&hashes[i]) {
                    Some(action) => {
                        actions[i] = Some(action);
                        false
                    }
                    None => true,
                });
                if missing.is_empty() {
                    break;
                }
                // Fold in our own absorb puts so they don't self-wake the next wait.
                seen = torus_state::NativeDaStore::arrival_generation();
            }
        }

        // (2) BS-4a: a true push miss no longer burns the consensus thread on an
        // in-line pull. Hand the misses to the recovery worker (off-thread, roomier
        // budget) and fail THIS view — MissingData, re-proposed next view, by which
        // point the worker has the bodies durably local. mem 7efe7062.
        if !missing.is_empty() {
            if let Some(ref worker) = self.da_recovery {
                // Count only batches the worker actually queued: a dead worker or
                // full queue must not paint the A/B handoff signal green while
                // recovery is silently dropped (final-review finding 1).
                if worker.submit(missing.iter().map(|&i| hashes[i]).collect()) {
                    if let Some(ref m) = self.metrics {
                        m.native_da_recovery_handoffs.inc();
                    }
                }
            }
            return Err(missing.len());
        }
        Ok(actions
            .into_iter()
            .map(|a| a.expect("all bodies present"))
            .collect())
    }

    /// Drain fetched bodies and mirror them into the durable DA store. Each body is
    /// stored under its own RECOMPUTED action-hash (`mirror_native_to_da`), so a peer
    /// cannot place a body under a hash it does not own.
    fn absorb_fetched_bodies(mempool: &Mempool, fetcher: &dyn NativeDaFetcher) {
        let bodies = fetcher.drain();
        if bodies.is_empty() {
            return;
        }
        let actions: Vec<torus_types::SignedNativeAction> = bodies
            .iter()
            .filter_map(|b| bincode::deserialize::<torus_types::SignedNativeAction>(b).ok())
            .collect();
        if !actions.is_empty() {
            mempool.mirror_native_to_da(&actions);
        }
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

        let diff = EpochManager::compute_validator_set_diff(&self.last_validator_set, &capped_set);
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
        self.leader_state.sync_validators(&self.last_validator_set);
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
                datums
                    .first()
                    .and_then(|d| {
                        bincode::deserialize::<TorusBlock>(d.bytes())
                            .map(|b| b.header)
                            .or_else(|_| {
                                bincode::deserialize::<CompactBlock>(d.bytes()).map(|cb| cb.header)
                            })
                            .ok()
                    })
                    .unwrap_or_else(|| self.last_header.clone())
            } else {
                self.last_header.clone()
            }
        } else {
            self.last_header.clone()
        };
        tracing::info!(
            parent_height = parent_header.height,
            local_height = self.last_header.height,
            "produce_block called (CTE)"
        );

        // Exec-ceiling Option A: wire the (previously dead) block_build_seconds —
        // covers mempool selection, DA mirror, attestation, construction, encode.
        let build_timer = std::time::Instant::now();

        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let (native_with_senders, evm_txs) = if let Some(ref mempool) = self.mempool {
            let gas_limit = if parent_header.evm_gas_limit == 0 {
                torus_evm::DEFAULT_BLOCK_GAS_LIMIT
            } else {
                parent_header.evm_gas_limit
            };
            // Pipeline-aware selection: exclude actions already carried by our
            // in-flight (proposed-but-uncommitted) blocks so the non-destructive pool
            // does not re-select them for N+1/N+2 before N commits and calls
            // `remove_committed_native` — the root cause of duplicate native inclusion
            // (memory 282f9818). `pending_proposals` is exactly that in-flight window.
            let mut in_flight: std::collections::HashSet<torus_types::B256> = self
                .pending_proposals
                .values()
                .flat_map(|b| {
                    b.native_actions
                        .iter()
                        .map(torus_types::compute_action_hash)
                })
                .collect();
            // Union the hash ledger: covers compact proposals whose bodies never
            // reconstructed (MissingData) — absent from `pending_proposals` but
            // still in flight on the wire (s355 duplicate-inclusion tail).
            self.in_flight_hashes.extend_into(&mut in_flight);
            let selected = mempool.select_native_for_block_with_senders_excluding(
                torus_mempool::rate_limit::native_total_block_cap(),
                &in_flight,
                torus_mempool::rate_limit::native_block_bytes_cap(),
                torus_mempool::rate_limit::native_orders_per_block_cap(),
            );

            // Proposer-side near-expiry filter (Option A) — proposer-LOCAL policy,
            // NOT a consensus rule, so determinism is unaffected. NON-CONSUMPTION:
            // the select above is non-destructive and did NOT mark these consumed —
            // dropping them from THIS block leaves them in the pool, eligible for
            // later blocks (they expire in the pool naturally). `now_ms` mirrors the
            // exec unit (block timestamp is seconds; expiry is milliseconds).
            let now_ms = timestamp.saturating_mul(1000);
            let (native, expiry_filtered) = filter_near_expiry_sessions(
                selected,
                now_ms,
                PROPOSAL_EXPIRY_MARGIN_MS,
                |pk| self.state_db.get_session(pk).ok().flatten().map(|s| s.expiry),
            );
            if expiry_filtered > 0 {
                if let Some(ref m) = self.metrics {
                    m.proposal_expiry_filtered.inc_by(expiry_filtered);
                }
            }

            let evm = mempool.drain_evm(gas_limit, parent_header.state_root);
            if !evm.is_empty() || !native.is_empty() {
                tracing::info!(
                    evm_txs = evm.len(),
                    native_actions = native.len(),
                    "selected actions for block"
                );
            }
            // P3 Task-3 empty-block diagnosis: attribute WHY this proposal is
            // native-empty. `native_pool_size` is the live pool depth AFTER the
            // in-select expiry evict (non-destructive select, so == depth at
            // selection); `in_flight.len()` is the exclude set. This distinguishes
            // ingest lag (pool_drained) from pipeline back-pressure (all_in_flight)
            // from cap/nonce gating on a live pool (pool_had_actions).
            if let Some(ref m) = self.metrics {
                let pool_size = mempool.native_pool_size();
                let excluded = in_flight.len();
                let reason = if !native.is_empty() {
                    "nonempty"
                } else if pool_size == 0 {
                    "empty_pool_drained"
                } else if excluded >= pool_size {
                    "empty_all_in_flight"
                } else {
                    "empty_pool_had_actions"
                };
                m.produce_block_result
                    .get_or_create(&vec![("reason".to_string(), reason.to_string())])
                    .inc();
                m.produce_block_pool_size.observe(pool_size as f64);
                m.produce_block_excluded.observe(excluded as f64);
            }
            (native, evm)
        } else {
            if let Some(ref m) = self.metrics {
                m.produce_block_result
                    .get_or_create(&vec![("reason".to_string(), "empty_no_mempool".to_string())])
                    .inc();
            }
            (vec![], vec![])
        };

        let native_actions: Vec<torus_types::SignedNativeAction> =
            native_with_senders.iter().map(|(_, a)| a.clone()).collect();

        // Proposer guarantee: mirror every referenced body to the durable DA store
        // so any validator can reconstruct the block out-of-band, even when it is
        // compact (push-primary, pull-rare). Livelock fix (mem 28e1a821).
        if let Some(ref mempool) = self.mempool {
            mempool.mirror_native_to_da(&native_actions);
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

        let height = block.header.height;

        // Pre-proposal push: only needed for COMPACT proposals, whose bodies travel
        // out-of-band. A FULL block already carries its bodies inline, so pushing them
        // too would be a redundant double-send (Task 8). The proposer always mirrors to
        // its own DA store above, regardless of mode.
        if COMPACT_PROPOSALS && !native_with_senders.is_empty() {
            if let Some(ref tx) = self.pre_proposal_tx {
                let count = native_with_senders.len();
                match tx.try_send(PreProposalBundle {
                    actions: native_with_senders,
                }) {
                    Ok(()) => tracing::info!(count, height, "pre-proposal push sent"),
                    Err(e) => {
                        // P3 Round-2 scope 4: a dropped bundle was previously a
                        // silent warn — count it so a saturated dissemination
                        // thread (replicas forced onto the slow DA pull) is
                        // observable. The bodies are still mirrored to the
                        // proposer's DA store above, so this is recoverable.
                        if let Some(ref m) = self.metrics {
                            m.native_pre_proposal_push_dropped.inc();
                        }
                        tracing::warn!(count, height, %e, "pre-proposal push failed");
                    }
                }
            }
        }

        // Note our own block's hashes too: a same-height re-proposal would
        // overwrite the `pending_proposals` entry and silently untrack these.
        let own_hashes: Vec<torus_types::B256> = block
            .native_actions
            .iter()
            .map(torus_types::compute_action_hash)
            .collect();
        let encoded = encode_proposal_datum(&block, COMPACT_PROPOSALS);
        self.pending_proposals.insert(height, block);
        self.pending_proposals.retain(|&h, _| h + 10 > height);
        self.in_flight_hashes.note(height, own_hashes);
        let hash = Self::hash_datum(&encoded);

        let validator_set_updates = self.epoch_validator_set_updates(height);

        if let Some(ref m) = self.metrics {
            m.block_build_seconds
                .observe(build_timer.elapsed().as_secs_f64());
        }

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
            tracing::warn!(
                datums_len = datums.len(),
                "validate_block: REJECTED -- datums.len() != 1"
            );
            return ValidateBlockResponse::Invalid;
        }

        let datum_bytes = datums[0].bytes();

        let computed = Self::hash_datum(datum_bytes);
        if block.data_hash != CryptoHash::new(computed) {
            tracing::warn!(
                datum_len = datum_bytes.len(),
                "validate_block: REJECTED -- data_hash mismatch"
            );
            return ValidateBlockResponse::Invalid;
        }

        // Try TorusBlock first (new format), CompactBlock fallback (backward compat)
        let torus_block = if let Ok(block) = bincode::deserialize::<TorusBlock>(datum_bytes) {
            tracing::info!(
                height = block.header.height,
                evm_tx_count = block.evm_transactions.len(),
                native_count = block.native_actions.len(),
                "validate_block: TorusBlock deserialized"
            );
            block
        } else if let Ok(compact) = bincode::deserialize::<CompactBlock>(datum_bytes) {
            tracing::info!(
                height = compact.header.height,
                native_hashes = compact.native_action_hashes.len(),
                "validate_block: CompactBlock fallback"
            );

            // Track these hashes BEFORE attempting reconstruction: if bodies are
            // missing (MissingData below) this proposal never reaches
            // `pending_proposals`, yet its actions are in flight on the wire — the
            // next leader must still exclude them from selection or they are
            // re-included 2–4x (the s355 duplicate-inclusion tail, mem c0f4f938).
            self.in_flight_hashes.note(
                compact.header.height,
                compact.native_action_hashes.iter().copied(),
            );

            let native_actions = if compact.native_action_hashes.is_empty() {
                vec![]
            } else {
                // Reconstruct out-of-band bodies for the HOT path: a fast local retry for
                // a racing pre-proposal PUSH, then a SHORT bounded native-DA PULL so a
                // genuine push miss is recovered live instead of wedging (#4 Task 1 — the
                // un-wedge). Budget stays ≪ the 500 ms view timeout.
                match self.reconstruct_native_actions_hot(&compact.native_action_hashes) {
                    Ok(actions) => actions,
                    Err(missing_count) => {
                        if let Some(ref m) = self.metrics {
                            m.missing_action_rejections.inc();
                        }
                        tracing::warn!(
                            missing_count,
                            height = compact.header.height,
                            "validate_block: MISSING native action bodies after hot retry + pull -- fetch via DA (not invalid)"
                        );
                        // MissingData (NOT Invalid): the block is structurally fine, we
                        // just lack the out-of-band bodies. The sync path must not
                        // blacklist the serving peer for this (livelock root cause, mem
                        // 28e1a821) — it re-pulls via the rare fallback instead.
                        return ValidateBlockResponse::MissingData;
                    }
                }
            };

            TorusBlock {
                header: compact.header,
                native_actions,
                evm_transactions: compact.evm_transactions,
                core_writer_actions: compact.core_writer_actions,
            }
        } else {
            tracing::warn!("validate_block: REJECTED -- deserialization failed");
            return ValidateBlockResponse::Invalid;
        };

        if !torus_block.evm_transactions.is_empty()
            && decode_all_txs(&torus_block.evm_transactions).is_err()
        {
            tracing::warn!("validate_block: REJECTED -- invalid EVM transactions");
            return ValidateBlockResponse::Invalid;
        }

        if !torus_block.native_actions.is_empty() {
            if torus_block.header.sig_attestation == [0u8; 64] {
                // Non-attested proposer: we must verify every action ourselves. Use the
                // SAME session-aware resolver as the execution path (see ~L300) so BOTH
                // EIP-712 and ed25519 *session* signatures are checked against committed
                // session state. The old code called `recover_sender`, which returns Err
                // for every `ActionSignature::Session` (eip712.rs:765) and thus blanket-
                // rejected all session actions on non-attested blocks — the deterministic
                // root cause of the reject-loop (mem 41e06912). `None` == bad sig /
                // missing / expired / out-of-scope session.
                //
                // Reads `self.state_db` (committed state, identical across validators at
                // this height) so the verdict is deterministic and cannot fork WITHIN a
                // block. NOTE: this is a CONSENSUS VALIDITY CHANGE — deploy to ALL
                // validators together; a mixed old/new set disagrees on these blocks and
                // forks. Same-block CreateSession+order still rejects here (defect A,
                // tracked separately) — strictly better than rejecting all session blocks.
                let resolved = torus_types::eip712::batch_verify_native_actions(
                    &torus_block.native_actions,
                    // Seconds→ms: `header.timestamp` is SECONDS, session `expiry` is
                    // MILLISECONDS. Must match the exec path (~L399) exactly or a
                    // validator would disagree with execution on session expiry.
                    torus_block.header.timestamp.saturating_mul(1000),
                    |pubkey| self.state_db.get_session(pubkey).ok().flatten(),
                );
                if let Some(i) = resolved.iter().position(|s| s.is_none()) {
                    tracing::warn!(
                        index = i,
                        "validate_block: REJECTED -- invalid native action signature"
                    );
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

        let height = torus_block.header.height;
        self.pending_proposals.insert(height, torus_block);
        self.pending_proposals.retain(|&h, _| h + 10 > height);

        let validator_set_updates = self.epoch_validator_set_updates(height);
        ValidateBlockResponse::Valid {
            app_state_updates: None,
            validator_set_updates,
        }
    }

    /// Validate a block during sync. Delegates to `validate_block`, which now
    /// reconstructs CompactBlock bodies from the durable DA store (not the ephemeral
    /// mempool) -- so a synced block whose bodies are durably present validates
    /// instead of being rejected and blacklisting the serving peer (livelock root
    /// cause, mem 28e1a821). A still-missing body is fetched via the rare
    /// pull-fallback (Task 6).
    fn validate_block_for_sync(
        &mut self,
        request: ValidateBlockRequest<RocksKVStore>,
    ) -> ValidateBlockResponse {
        // RARE pull-fallback (Task 6): a synced CompactBlock whose bodies are absent
        // from the durable DA store fetches them by-hash from peers BEFORE validating,
        // so the block recovers in-call instead of bouncing (no blacklist, mem 28e1a821).
        // This runs on the block-sync path; the hot validate_block path is untouched and
        // never triggers a network fetch (mem 8ee99db3). Peek + decode the datum, then
        // drop the borrow before validating (which consumes `request`).
        let compact: Option<CompactBlock> = {
            let datums = request.proposed_block().data.vec();
            datums.first().and_then(|d| {
                let bytes = d.bytes();
                // A full TorusBlock is self-contained (no out-of-band bodies); only a
                // CompactBlock can miss. Try full first to avoid a misparse.
                if bincode::deserialize::<TorusBlock>(bytes).is_ok() {
                    None
                } else {
                    bincode::deserialize::<CompactBlock>(bytes).ok()
                }
            })
        };
        if let Some(compact) = compact {
            self.pull_compact_bodies_if_missing(&compact);
        }
        self.validate_block(request)
    }

    /// Send committed block to the execution pipeline thread.
    fn on_committed_block(&mut self, block: &Block, _committed_hash: CryptoHash) {
        let datums = block.data.vec();
        let Some(datum) = datums.first() else {
            tracing::debug!("on_committed_block: no datums in block");
            return;
        };

        let datum_bytes = datum.bytes();
        // The HEADER is always in the datum; only CompactBlock BODIES need the DA
        // store. Extract the header up front and advance last_header from it
        // UNCONDITIONALLY (below) so a missing body can never freeze consensus
        // height/view -- the livelock root cause (mem 28e1a821) was an early return
        // here, before last_header advanced, which froze RPC height and churned views.
        let (header, reconstructed): (
            TorusBlockHeader,
            Result<TorusBlock, Vec<torus_types::B256>>,
        ) = if let Ok(full) = bincode::deserialize::<TorusBlock>(datum_bytes) {
            let height = full.header.height;
            let block = self.pending_proposals.remove(&height).unwrap_or(full);
            (block.header.clone(), Ok(block))
        } else if let Ok(compact) = bincode::deserialize::<CompactBlock>(datum_bytes) {
            let height = compact.header.height;
            if let Some(cached) = self.pending_proposals.remove(&height) {
                (cached.header.clone(), Ok(cached))
            } else {
                (
                    compact.header.clone(),
                    self.reconstruct_compact_from_da(&compact),
                )
            }
        } else {
            tracing::warn!("on_committed_block: failed to deserialize block datum");
            return;
        };

        let height = header.height;
        // Committed: these hashes leave the in-flight window. The mempool prunes
        // them via `remove_committed_native` further down this path.
        self.in_flight_hashes.clear(height);

        // Track committed consensus height/view from the header regardless of body
        // availability (BFT finality is independent of local execution readiness).
        if height > self.last_header.height {
            self.last_header = header.clone();
            self.leader_state.set_view(height.saturating_add(1));
        }

        let torus_block = match reconstructed {
            Ok(b) => b,
            Err(missing) => {
                // An already-committed block whose bodies we cannot reconstruct. With
                // the durable DA store + proposer mirror + push this is rare (a node that
                // committed via QC without validating); the pull-fallback (Task 6) fetches
                // and executes it. Loud, never silent -- and crucially the chain height
                // has already advanced above, so consensus does NOT wedge.
                tracing::error!(
                    height,
                    missing = missing.len(),
                    "on_committed_block: missing native bodies for committed block -- execution deferred (fetch: Task 6)"
                );
                // NON-BLOCKING pull (Task 6): nudge the bodies toward the durable DA
                // store so a re-sync / restart-replay of this committed block can
                // reconstruct it. Do NOT wait here -- this is the consensus thread
                // (mem 8ee99db3); height has already advanced above, so no wedge.
                if let Some(fetcher) = self.da_fetcher.as_ref() {
                    fetcher.fetch(missing.iter().map(|h| h.0).collect());
                    if let Some(ref m) = self.metrics {
                        m.native_da_pull_requests.inc();
                    }
                }
                return;
            }
        };

        tracing::info!(
            height,
            evm_txs = torus_block.evm_transactions.len(),
            native = torus_block.native_actions.len(),
            "on_committed_block: sending to execution pipeline"
        );

        let committed_hashes: Vec<torus_types::B256> = if torus_block.native_actions.is_empty() {
            Vec::new()
        } else {
            torus_block
                .native_actions
                .iter()
                .map(torus_types::compute_action_hash)
                .collect()
        };
        if !committed_hashes.is_empty() {
            if let Some(ref mempool) = self.mempool {
                mempool.remove_committed_native(&committed_hashes);
            }
        }

        // RH4 durable DA-body GC: record this height's committed body-hashes and
        // delete the bodies of heights that have now aged out of the retention
        // window. Runs every commit (even native-empty ones) so the window slides
        // and the FIFO drains. Bounded per-commit work; best-effort.
        if self.da_gc_retention > 0 {
            let to_delete = drain_da_gc_window(
                &mut self.da_gc_pending,
                height,
                self.da_gc_retention,
                committed_hashes,
            );
            if !to_delete.is_empty() {
                if let Some(ref mempool) = self.mempool {
                    let deleted = mempool.gc_native_da_bodies(&to_delete);
                    if let Some(ref m) = self.metrics {
                        m.da_bodies_gc_deleted.inc_by(deleted as u64);
                    }
                }
            }
        }

        // D4 (S392): pin the mempool's admission fee floor to the base fee
        // committed blocks actually charge (frozen at 1 gwei today; follows
        // the header once the fee market unfreezes).
        if let Some(ref mempool) = self.mempool {
            mempool.set_base_fee(torus_block.header.base_fee_per_gas);
        }

        if let Some(ref tx) = self.exec_tx {
            let msg = CommittedBlockMsg {
                torus_block,
                pending_slashes: self.pending_slashes.drain(..).collect(),
            };
            // inc BEFORE the (possibly blocking) send so a consensus thread stalled
            // on a full channel is visible as depth ≥ the bound, not hidden.
            // exec_queue_in mirrors the gauge inc (P2 funnel item 4).
            if let Some(ref m) = self.metrics {
                m.exec_queue_depth.inc();
                m.exec_queue_in.inc();
            }
            if tx.send(msg).is_err() {
                tracing::error!(
                    height,
                    "execution pipeline channel closed — block will not be executed!"
                );
                if let Some(ref m) = self.metrics {
                    m.exec_queue_depth.dec();
                    // Rebalance the counter pair too: the block never entered
                    // the queue (shutdown race), so in − out must return to 0.
                    m.exec_queue_out.inc();
                }
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
mod in_flight_ledger_tests {
    use super::*;
    use torus_types::B256;

    fn h(n: u8) -> B256 {
        B256::from([n; 32])
    }

    #[test]
    fn in_flight_ledger_notes_and_unions_per_height() {
        let mut ledger = InFlightHashLedger::default();
        ledger.note(5, [h(1), h(2)]);
        ledger.note(5, [h(2), h(3)]); // same-height re-proposal unions, not overwrites
        let mut out = std::collections::HashSet::new();
        ledger.extend_into(&mut out);
        assert_eq!(out, [h(1), h(2), h(3)].into_iter().collect());
    }

    #[test]
    fn in_flight_ledger_window_evicts_old_heights() {
        let mut ledger = InFlightHashLedger::default();
        ledger.note(1, [h(1)]);
        ledger.note(11, [h(2)]); // 1 + 10 > 11 is false -> height 1 evicted
        let mut out = std::collections::HashSet::new();
        ledger.extend_into(&mut out);
        assert_eq!(out, [h(2)].into_iter().collect());
    }

    #[test]
    fn in_flight_ledger_commit_clear() {
        let mut ledger = InFlightHashLedger::default();
        ledger.note(5, [h(1)]);
        ledger.note(6, [h(2)]);
        ledger.clear(5);
        let mut out = std::collections::HashSet::new();
        ledger.extend_into(&mut out);
        assert_eq!(out, [h(2)].into_iter().collect());
    }

    #[test]
    fn in_flight_ledger_extends_selection_exclusion_set() {
        // produce_block builds the exclusion set from pending_proposals bodies,
        // then unions the ledger (compact proposals whose bodies never
        // reconstructed). Both sources must land in the final set handed to
        // select_native_for_block_with_senders_excluding.
        let mut in_flight: std::collections::HashSet<B256> = [h(1)].into_iter().collect();
        let mut ledger = InFlightHashLedger::default();
        ledger.note(7, [h(2)]);
        ledger.extend_into(&mut in_flight);
        assert!(in_flight.contains(&h(1)) && in_flight.contains(&h(2)));
    }
}

#[cfg(test)]
mod da_gc_window_tests {
    use super::*;
    use torus_types::B256;

    fn h(n: u8) -> B256 {
        B256::from([n; 32])
    }

    /// A body inside the retention window survives; one that ages strictly
    /// outside it is returned for deletion.
    #[test]
    fn window_keeps_recent_and_deletes_aged() {
        let mut q = std::collections::VecDeque::new();
        // retention 4: a body committed at height H is deletable once tip >= H+4.
        // Commit h(1)@10, h(2)@11.
        assert!(drain_da_gc_window(&mut q, 10, 4, vec![h(1)]).is_empty());
        assert!(drain_da_gc_window(&mut q, 11, 4, vec![h(2)]).is_empty());
        // Tip 13: 10 <= 13-4=9? no. Nothing aged out yet — both in window.
        assert!(drain_da_gc_window(&mut q, 13, 4, vec![]).is_empty());
        // Tip 14: cutoff 10, 10 <= 10 -> height 10's body (h(1)) is deleted;
        // height 11 (h(2)) still in window.
        assert_eq!(drain_da_gc_window(&mut q, 14, 4, vec![]), vec![h(1)]);
        // Tip 15: cutoff 11 -> h(2) now deleted.
        assert_eq!(drain_da_gc_window(&mut q, 15, 4, vec![]), vec![h(2)]);
        assert!(q.is_empty(), "FIFO fully drained");
    }

    /// Multiple hashes per committed height are all deleted together when that
    /// height ages out, and native-empty heights buffer nothing.
    #[test]
    fn window_deletes_all_hashes_of_an_aged_height() {
        let mut q = std::collections::VecDeque::new();
        drain_da_gc_window(&mut q, 100, 2, vec![h(1), h(2), h(3)]);
        // native-empty commits advance the window without buffering.
        drain_da_gc_window(&mut q, 101, 2, vec![]);
        // Tip 102: cutoff 100, 100 <= 100 -> all three bodies of height 100 go.
        let deleted = drain_da_gc_window(&mut q, 102, 2, vec![]);
        assert_eq!(deleted, vec![h(1), h(2), h(3)]);
        assert!(q.is_empty());
    }

    /// A large retention window keeps everything (archive-ish); nothing deleted
    /// until the tip climbs past commit_height + retention.
    #[test]
    fn window_large_retention_keeps_all() {
        let mut q = std::collections::VecDeque::new();
        for height in 0..50u64 {
            let del = drain_da_gc_window(&mut q, height, 1000, vec![h(height as u8)]);
            assert!(del.is_empty(), "retention 1000: nothing aged out by height {height}");
        }
        assert_eq!(q.len(), 50, "all committed heights still buffered");
    }
}

#[cfg(test)]
mod crash_recovery_tests {
    use super::*;
    use torus_types::{Bloom, FixedPoint, NativeAction, SignedNativeAction, B256, U256};

    fn make_test_config_and_db() -> (ChainConfig, StateDb) {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(1000);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("torus-crash-test-{}-{}", std::process::id(), id));
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
            reputation_leader_selection: false,
            exec_trust_cache: false,
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
            .put_cf_raw(
                CF_BLOCK_BODIES,
                &block.header.height.to_be_bytes(),
                &body_bytes,
            )
            .unwrap();
    }

    /// Test double: a `NativeDaFetcher` whose body only appears on the
    /// `deliver_on_drain`-th `drain()` — models a body that lands some retries into the
    /// pull. Shared by the sync-budget test and the hot-path pull tests (#4 Task 1).
    struct LateFetcher {
        body: Vec<u8>,
        deliver_on_drain: usize,
        drains: std::sync::atomic::AtomicUsize,
    }
    impl NativeDaFetcher for LateFetcher {
        fn fetch(&self, _hashes: Vec<[u8; 32]>) {}
        fn drain(&self) -> Vec<Vec<u8>> {
            use std::sync::atomic::Ordering;
            let n = self.drains.fetch_add(1, Ordering::Relaxed) + 1;
            if n >= self.deliver_on_drain {
                vec![self.body.clone()]
            } else {
                Vec::new()
            }
        }
    }

    /// Test double: a fetcher that NEVER delivers — models a true push miss whose body
    /// cannot be pulled in-budget, so the hot path must fail the view, not hang (#4 Task 1).
    struct NeverFetcher;
    impl NativeDaFetcher for NeverFetcher {
        fn fetch(&self, _hashes: Vec<[u8; 32]>) {}
        fn drain(&self) -> Vec<Vec<u8>> {
            Vec::new()
        }
    }

    /// Test double (Phase 2.3 #5): models a hash-only PRE-WARM — the body already sits in
    /// the inbound queue (delivered once on the first `drain()`, then gone, as a real pull
    /// delivers each body once). `fetch()` is COUNTED so a test can prove the hot local
    /// retry absorbed the pre-warmed body without issuing a redundant network fetch.
    struct PrewarmFetcher {
        body: Vec<u8>,
        fetches: std::sync::atomic::AtomicUsize,
        drained: std::sync::atomic::AtomicBool,
    }
    impl NativeDaFetcher for PrewarmFetcher {
        fn fetch(&self, _hashes: Vec<[u8; 32]>) {
            self.fetches
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        fn drain(&self) -> Vec<Vec<u8>> {
            if self
                .drained
                .swap(true, std::sync::atomic::Ordering::Relaxed)
            {
                Vec::new()
            } else {
                vec![self.body.clone()]
            }
        }
    }

    /// Test double (BS-4b): counts `fetch()` calls and delivers `body` on the
    /// `deliver_on_drain`-th `drain()` (0 = never). Like a real pull, the body only
    /// lands in the inbound AFTER a request was issued, so `drain()` returns empty
    /// until the first `fetch()` — this makes the F2 pre-warm absorb (which drains
    /// before any fetch) a no-op, so the counted fetches are exactly those
    /// `recover_bodies_bounded` issued. Lets a test assert the mid-budget re-fetch
    /// fires exactly once for a stuck body and never for one that arrives early.
    struct CountingFetcher {
        body: Vec<u8>,
        deliver_on_drain: usize,
        fetch_calls: std::sync::atomic::AtomicUsize,
        drains: std::sync::atomic::AtomicUsize,
    }
    impl NativeDaFetcher for CountingFetcher {
        fn fetch(&self, _hashes: Vec<[u8; 32]>) {
            self.fetch_calls
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        fn drain(&self) -> Vec<Vec<u8>> {
            use std::sync::atomic::Ordering;
            // No response can land before a request is issued (pre-warm absorb drains empty).
            if self.fetch_calls.load(Ordering::Relaxed) == 0 {
                return Vec::new();
            }
            let n = self.drains.fetch_add(1, Ordering::Relaxed) + 1;
            if self.deliver_on_drain != 0 && n >= self.deliver_on_drain {
                vec![self.body.clone()]
            } else {
                Vec::new()
            }
        }
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
    fn produce_block_datum_is_compact() {
        // Task 8: with compact proposals enabled (version-gated), the proposal datum
        // carries only action HASHES (CompactBlock), not inline bodies — bodies travel
        // out-of-band via the durable DA store (push T7 / pull T6). Required for 400k
        // orders/sec, where a full block of ~MB exceeds max_consensus_message_size.
        let actions: Vec<SignedNativeAction> = (0..100).map(sign_claim_rewards).collect();
        let block = make_block(7, actions);

        // Compact-mode datum: a CompactBlock referencing all 100 actions by hash.
        let compact_datum = encode_proposal_datum(&block, true);
        let decoded: CompactBlock = bincode::deserialize(&compact_datum)
            .expect("compact-mode datum must be a CompactBlock");
        assert_eq!(
            decoded.native_action_hashes.len(),
            100,
            "compact datum references every action by hash"
        );

        // The FULL encoding remains valid behind the flag (staged rollout / rollback),
        // and is strictly larger — proving the compact datum carries no inline bodies.
        let full_datum = encode_proposal_datum(&block, false);
        let full: TorusBlock = bincode::deserialize(&full_datum)
            .expect("full-mode datum must be a self-contained TorusBlock");
        assert_eq!(full.native_actions.len(), 100);
        assert!(
            compact_datum.len() < full_datum.len(),
            "compact datum must be smaller (bodies are out-of-band, not inline)"
        );
    }

    #[test]
    fn compact_proposal_disseminates() {
        // Task 8: a compact proposal carries only hashes, and a peer reconstructs the
        // full block from bodies delivered out-of-band into its durable DA store
        // (push T7 / pull T6) — so re-enabling compact does not strand any validator.
        let (config, state_db) = make_test_config_and_db();
        let actions: Vec<SignedNativeAction> = (0..10).map(sign_claim_rewards).collect();
        let block = make_block(5, actions.clone());

        // The proposer emits a CompactBlock datum (hashes only).
        let datum = encode_proposal_datum(&block, true);
        let compact: CompactBlock =
            bincode::deserialize(&datum).expect("compact-mode datum is a CompactBlock");
        assert_eq!(compact.native_action_hashes.len(), actions.len());

        // A peer holding the bodies in its DA store (delivered out-of-band) reconstructs
        // the full block — the bodies were NOT in the proposal itself.
        let mempool = Arc::new(torus_mempool::Mempool::new(
            state_db.clone(),
            torus_mempool::MempoolConfig::default(),
        ));
        mempool.mirror_native_to_da(&actions); // models push/pull delivery into the DA store
        let app = TorusApp::new(state_db.clone(), &config, None, Some(mempool), None);
        let reconstructed = app
            .reconstruct_compact_from_da(&compact)
            .expect("peer reconstructs the compact block from its durable DA store");
        assert_eq!(reconstructed.native_actions.len(), actions.len());
        for (got, want) in reconstructed.native_actions.iter().zip(actions.iter()) {
            assert_eq!(
                torus_types::compute_action_hash(got),
                torus_types::compute_action_hash(want),
                "reconstructed bodies match the referenced hashes"
            );
        }
    }

    /// One PlaceOrderBatch action carrying `n_orders` orders (dummy signature — the
    /// proposal-size test below only measures encoded bytes; `compute_action_hash`
    /// omits the signature). Mirrors the bench's bs=N order shape.
    fn big_order_batch_action(nonce: u64, n_orders: usize) -> SignedNativeAction {
        use torus_types::{ActionSignature, OrderType, PlaceOrderParams, Signature, TimeInForce};
        let order = PlaceOrderParams {
            market_id: 1,
            is_buy: true,
            price: FixedPoint::from_raw(6_000_000_000_000),
            quantity: FixedPoint::from_raw(FixedPoint::SCALE),
            order_type: OrderType::Limit,
            time_in_force: TimeInForce::GTC,
            reduce_only: false,
            client_order_id: None,
        };
        SignedNativeAction {
            action: NativeAction::PlaceOrderBatch(vec![order; n_orders]),
            nonce,
            signature: ActionSignature::Eip712(Signature {
                v: 27,
                r: [0u8; 32],
                s: [0u8; 32],
            }),
        }
    }

    /// Task 9 (scale milestone, deterministic proof): the bs=500 flood collapsed
    /// because a block of ~50k orders serializes to ~2.5 MB as a FULL TorusBlock —
    /// far over the then-256 KB `max_consensus_message_size` — so the proposal could
    /// not disseminate and the chain stalled (mem 41ca452). The COMPACT encoding
    /// carries only action hashes, so the SAME block fits comfortably: this is the
    /// encoding change that lets the bs=500 flood hold and unblocks the path to 400k
    /// orders/sec. The collapse reproduces against today's raised gate too: the full
    /// block is ~2.5 MB, over the 1 MB accept limit.
    #[test]
    fn compact_proposal_holds_where_full_block_collapsed_at_bs500() {
        // Mirrors torus-network `NetworkConfig::default().max_consensus_message_size`
        // (torus-consensus has no torus-network dep). O5 raised it 256 KB -> 1 MB;
        // keep in sync with config.rs.
        const MAX_CONSENSUS_MESSAGE_SIZE: usize = 1024 * 1024;

        // The bs=500 block shape: 100 actions x 500-order batches = 50k orders.
        let actions: Vec<SignedNativeAction> = (0..100)
            .map(|i| big_order_batch_action(i as u64, 500))
            .collect();
        let block = make_block(9, actions);

        let full = encode_proposal_datum(&block, false);
        let compact = encode_proposal_datum(&block, true);

        assert!(
            full.len() > MAX_CONSENSUS_MESSAGE_SIZE,
            "full block ({} bytes) exceeds the 256KB consensus limit — reproduces the bs=500 collapse",
            full.len()
        );
        assert!(
            compact.len() < MAX_CONSENSUS_MESSAGE_SIZE,
            "compact block ({} bytes) fits the 256KB consensus limit — the fix",
            compact.len()
        );
        // Bodies are entirely out-of-band: compact is orders-of-magnitude smaller.
        assert!(
            compact.len() * 50 < full.len(),
            "compact ({} bytes) must be >50x smaller than full ({} bytes)",
            compact.len(),
            full.len()
        );
    }

    #[test]
    fn compactblock_reconstructs_from_da_store() {
        // Reproduction of the livelock fix: a CompactBlock referencing a native body
        // that lives ONLY in the durable DA store -- never admitted to the ephemeral
        // nonce-gated mempool -- must reconstruct. Previously reconstruction read the
        // mempool, missed, and the block was rejected (+ peer blacklisted on the sync
        // path), wedging the chain (root cause, mem 28e1a821).
        let (config, state_db) = make_test_config_and_db();

        let body = sign_claim_rewards(123);
        let body_hash = torus_types::compute_action_hash(&body);

        let mempool = Arc::new(torus_mempool::Mempool::new(
            state_db.clone(),
            torus_mempool::MempoolConfig::default(),
        ));
        // Mirror to the DA store WITHOUT mempool admission (mirror_native_to_da does
        // not touch the in-memory pool) -- exactly the "in DA, not in mempool" state.
        mempool.mirror_native_to_da(std::slice::from_ref(&body));
        assert!(
            mempool.get_native_by_hash(&body_hash).is_none(),
            "precondition: body is NOT in the ephemeral mempool pool"
        );
        assert!(
            mempool.get_native_da(&body_hash).is_some(),
            "precondition: body IS in the durable DA store"
        );

        let app = TorusApp::new(state_db.clone(), &config, None, Some(mempool), None);

        // A CompactBlock referencing the body by hash reconstructs from the DA store.
        let full = make_block(1, vec![body.clone()]);
        let compact = CompactBlock::from_block(&full);
        let reconstructed = app
            .reconstruct_compact_from_da(&compact)
            .expect("CompactBlock must reconstruct from the DA store");
        assert_eq!(reconstructed.native_actions.len(), 1);
        assert_eq!(
            torus_types::compute_action_hash(&reconstructed.native_actions[0]),
            body_hash,
            "reconstructed body must match the referenced hash"
        );

        // A body present in NEITHER store -> typed miss (Err of the missing hashes),
        // the signal the rare pull-fallback consumes (Task 6) -- not a hard reject
        // that blacklists a peer.
        let unknown = make_block(2, vec![sign_claim_rewards(999)]);
        let unknown_compact = CompactBlock::from_block(&unknown);
        let missing = app
            .reconstruct_compact_from_da(&unknown_compact)
            .expect_err("an absent body must report a typed miss");
        assert_eq!(missing.len(), 1);
    }

    #[test]
    fn no_replay_when_already_applied() {
        let (config, state_db) = make_test_config_and_db();
        let block = make_block(5, vec![sign_claim_rewards(200)]);

        persist_block_for_test(&state_db, &block);

        state_db
            .put_cf_raw(
                CF_CONSENSUS_META,
                META_NATIVE_APPLIED_HEIGHT,
                &5u64.to_be_bytes(),
            )
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

    // ---- live-path replay dedup (duplicate native inclusion) ----

    /// Build an `ExecutionContext` directly (mirrors `TorusApp::new`) so a test can
    /// drive `execute_committed_block` for several heights synchronously.
    fn make_exec_ctx(config: &ChainConfig, state_db: &StateDb) -> ExecutionContext {
        // Default: no mempool, cache off -> all misses = today's full-recover
        // behavior, exactly what the existing exec tests assert.
        make_exec_ctx_with_mempool(config, state_db, None, false)
    }

    fn make_exec_ctx_with_mempool(
        config: &ChainConfig,
        state_db: &StateDb,
        mempool: Option<Arc<Mempool>>,
        exec_trust_cache: bool,
    ) -> ExecutionContext {
        ExecutionContext {
            state_db: state_db.clone(),
            validator: BlockValidator::new(
                config.chain_id,
                config.epoch_length,
                config.max_validators,
                config.treasury_address,
                config.dev_pool_address,
            ),
            evm_executor: EvmExecutor::new(config.chain_id),
            staking: StakingManager::new(state_db.clone()),
            epoch_length: config.epoch_length,
            max_validators: config.max_validators,
            treasury_address: config.treasury_address,
            dev_pool_address: config.dev_pool_address,
            metrics: None,
            mempool,
            exec_trust_cache,
            // None -> trades write inline through the overlay (pre-O3 behavior),
            // keeping these tests' reads deterministic right after execution.
            trade_writer: None,
            // None -> synchronous block-body put (pre-Task-1 behavior); tests that
            // exercise the async path attach a writer explicitly.
            body_writer: None,
        }
    }

    /// GATE (double-verify-trust-cache T5): the exec trust-cache must be a pure
    /// performance optimization. With a real mempool-populated cache, the resolved
    /// senders from the cached verify MUST equal the uncached (full-recover) verify
    /// action-for-action — including an invalid signature (-> None, the slashing
    /// input that fires app.rs:303-320) and a gossip-TRUSTED action
    /// (verified_locally=false -> not cached -> re-verified, never short-circuited).
    #[test]
    fn trust_cache_resolved_senders_identical_cached_vs_uncached() {
        use torus_types::ActionSignature;
        let (_config, state_db) = make_test_config_and_db();
        let mempool = Mempool::new(state_db.clone(), torus_mempool::MempoolConfig::default());
        let base = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;

        let k1 = k256::ecdsa::SigningKey::from_slice(&[11u8; 32]).unwrap();
        let k2 = k256::ecdsa::SigningKey::from_slice(&[12u8; 32]).unwrap();

        // (0) EIP-712 admitted via the verified ingress path -> seeds the cache.
        let a_cached =
            torus_types::eip712::sign_native_action(NativeAction::ClaimRewards, base + 1, &k1);
        let s_cached = a_cached.recover_sender().unwrap();
        mempool
            .add_native_action_presigned(s_cached, a_cached.clone())
            .unwrap();

        // (1) EIP-712 valid but never admitted -> cache MISS -> full recover.
        let a_uncached =
            torus_types::eip712::sign_native_action(NativeAction::ClaimRewards, base + 2, &k2);
        let s_uncached = a_uncached.recover_sender().unwrap();

        // (2) EIP-712 with a corrupted signature -> invalid -> None (slash input).
        let mut a_invalid =
            torus_types::eip712::sign_native_action(NativeAction::ClaimRewards, base + 3, &k1);
        if let ActionSignature::Eip712(ref mut sig) = a_invalid.signature {
            sig.r = [0xff; 32];
            sig.s = [0xff; 32];
        }

        // (3) gossip-TRUSTED (sender claimed by a peer, verified_locally=false) ->
        //     NOT cached -> exec must re-verify it (no short-circuit).
        let a_trusted =
            torus_types::eip712::sign_native_action(NativeAction::ClaimRewards, base + 4, &k2);
        let s_trusted = a_trusted.recover_sender().unwrap();
        mempool
            .add_native_action_from_gossip_trusted(s_trusted, a_trusted.clone())
            .unwrap();

        let actions = vec![a_cached, a_uncached, a_invalid, a_trusted.clone()];

        let uncached = torus_types::eip712::batch_verify_native_actions(&actions, base, |_| None);
        let cached = torus_types::eip712::batch_verify_native_actions_cached(
            &actions,
            base,
            |_| None,
            |key| mempool.verified_sender(key),
            |key| mempool.session_sig_verified(key),
        );

        assert_eq!(
            cached, uncached,
            "DETERMINISM GATE: cache-on resolved senders must equal cache-off"
        );
        // Structural sanity across the four provenance classes.
        assert_eq!(cached[0], Some(s_cached), "cached HIT == fresh recover");
        assert_eq!(cached[1], Some(s_uncached), "MISS -> full recover");
        assert_eq!(cached[2], None, "invalid sig -> None (slash path input)");
        assert_eq!(
            cached[3],
            Some(s_trusted),
            "trusted re-verified, not short-circuited"
        );

        // Provenance: the trusted action was never seeded into the cache.
        let trusted_key = torus_types::verified_cache_key(&a_trusted).unwrap();
        assert_eq!(
            mempool.verified_sender(&trusted_key),
            None,
            "gossip-trusted (verified_locally=false) must not be cached"
        );
    }

    /// GATE (T5, state level): executing the SAME block with the trust-cache warm
    /// (HIT) vs cold (no mempool) must reach identical state. A correct cache reuses
    /// exactly the sender a fresh recover yields, so execution is unchanged.
    #[test]
    fn trust_cache_execution_state_identical_cached_vs_uncached() {
        let amount = U256::from(FixedPoint::ONE.raw() as u128);
        let key = k256::ecdsa::SigningKey::from_slice(&[21u8; 32]).unwrap();
        let signed = torus_types::eip712::sign_native_action(
            NativeAction::TransferToPerp { amount },
            424_242,
            &key,
        );
        let sender = signed.recover_sender().unwrap();
        let start = amount * U256::from(10u8);

        // --- Cold cache (no mempool): full recover, today's path. ---
        let (config_off, db_off) = make_test_config_and_db();
        let ctx_off = make_exec_ctx_with_mempool(&config_off, &db_off, None, false);
        fund_evm_balance(&db_off, sender, start);
        ctx_off.execute_committed_block(&make_block(1, vec![signed.clone()]), vec![]);
        let off_balance = read_evm_balance(&db_off, sender);

        // --- Warm cache (HIT): seed the signature-committing key -> sender. ---
        let (config_on, db_on) = make_test_config_and_db();
        let mempool = Arc::new(Mempool::new(
            db_on.clone(),
            torus_mempool::MempoolConfig::default(),
        ));
        let cache_key = torus_types::verified_cache_key(&signed).unwrap();
        mempool.cache_verified_sender(cache_key, sender);
        assert_eq!(
            mempool.verified_sender(&cache_key),
            Some(sender),
            "precondition: cache is warm so exec takes the HIT path"
        );
        // Flag ON so the warm cache is actually consulted (the HIT path).
        let ctx_on = make_exec_ctx_with_mempool(&config_on, &db_on, Some(mempool), true);
        fund_evm_balance(&db_on, sender, start);
        ctx_on.execute_committed_block(&make_block(1, vec![signed.clone()]), vec![]);
        let on_balance = read_evm_balance(&db_on, sender);

        assert_eq!(
            on_balance, off_balance,
            "execution with a warm trust-cache must reach identical state as cold"
        );
        assert_eq!(
            start - on_balance,
            amount,
            "executed exactly once, correct debit"
        );
    }

    /// T6: with `--exec-trust-cache` OFF (the default), the cache is never consulted
    /// at exec — even a POISONED entry is ignored and the action is fully recovered
    /// (today's behavior). This is what makes the flag a safe default-off rollback
    /// switch.
    #[test]
    fn trust_cache_flag_off_bypasses_cache() {
        let amount = U256::from(FixedPoint::ONE.raw() as u128);
        let key = k256::ecdsa::SigningKey::from_slice(&[31u8; 32]).unwrap();
        let signed = torus_types::eip712::sign_native_action(
            NativeAction::TransferToPerp { amount },
            424_242,
            &key,
        );
        let real_sender = signed.recover_sender().unwrap();
        let start = amount * U256::from(10u8);

        let (config, db) = make_test_config_and_db();
        let mempool = Arc::new(Mempool::new(
            db.clone(),
            torus_mempool::MempoolConfig::default(),
        ));
        // POISON the cache: map this action's key to a bogus sender. If exec ever
        // consulted the cache, the deposit would be attributed to `bogus` and the
        // real sender would NOT be debited.
        let bogus = Address::from([0xCD; 20]);
        assert_ne!(bogus, real_sender);
        let cache_key = torus_types::verified_cache_key(&signed).unwrap();
        mempool.cache_verified_sender(cache_key, bogus);

        // Flag OFF -> cache ignored -> real sender recovered + debited.
        let ctx = make_exec_ctx_with_mempool(&config, &db, Some(mempool), false);
        fund_evm_balance(&db, real_sender, start);
        ctx.execute_committed_block(&make_block(1, vec![signed.clone()]), vec![]);

        assert_eq!(
            start - read_evm_balance(&db, real_sender),
            amount,
            "flag OFF must full-recover the REAL sender, ignoring the poisoned cache"
        );
        assert_eq!(
            read_evm_balance(&db, bogus),
            U256::ZERO,
            "poisoned cache entry must never be used when the flag is off"
        );
    }

    /// `make_exec_ctx_with_mempool` + a wired metrics handle so the exec-side
    /// `exec_verify_skipped` counter (trust-cache HITs) is observable in-test.
    fn make_exec_ctx_with_metrics(
        config: &ChainConfig,
        state_db: &StateDb,
        mempool: Option<Arc<Mempool>>,
        exec_trust_cache: bool,
        metrics: Arc<torus_telemetry::Metrics>,
    ) -> ExecutionContext {
        let mut ctx = make_exec_ctx_with_mempool(config, state_db, mempool, exec_trust_cache);
        ctx.metrics = Some(metrics);
        ctx
    }

    /// PROOF-LEG + INVARIANT 3 (determinism): an EIP-712 action verified at gossip
    /// INGRESS (the real `add_native_action_from_gossip` recover path), then pruned
    /// on commit via `remove_committed_native` (which refresh-stashes the verified
    /// sender BEFORE the exec thread reads it), MUST skip re-verify on commit — and
    /// reach byte-identical state to a cold (no-cache) full-recover run. The new
    /// `exec_verify_skipped` counter proves the skip fired.
    #[test]
    fn trust_cache_ingress_then_commit_skips_reverify_and_matches_cold() {
        let amount = U256::from(FixedPoint::ONE.raw() as u128);
        let key = k256::ecdsa::SigningKey::from_slice(&[41u8; 32]).unwrap();
        // Nonce in the live window so the real ingress admit path accepts it.
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        let signed = torus_types::eip712::sign_native_action(
            NativeAction::TransferToPerp { amount },
            nonce,
            &key,
        );
        let sender = signed.recover_sender().unwrap();
        let start = amount * U256::from(10u8);

        // --- COLD baseline: no mempool, cache off -> full recover every time. ---
        let (config_off, db_off) = make_test_config_and_db();
        let ctx_off = make_exec_ctx_with_mempool(&config_off, &db_off, None, false);
        fund_evm_balance(&db_off, sender, start);
        ctx_off.execute_committed_block(&make_block(1, vec![signed.clone()]), vec![]);
        let cold_balance = read_evm_balance(&db_off, sender);

        // --- WARM: verify at ingress, then commit-prune, then exec. ---
        let (config_on, db_on) = make_test_config_and_db();
        let mempool = Arc::new(Mempool::new(
            db_on.clone(),
            torus_mempool::MempoolConfig::default(),
        ));
        // Real gossip-ingress verify path: recovers the sender locally
        // (verified_locally=true) and seeds the signature-committing key.
        mempool
            .add_native_action_from_gossip(sender, signed.clone())
            .expect("gossip ingress verify + admit");
        let hash = torus_types::compute_action_hash(&signed);
        // Commit: refresh-stash the verified sender, then prune the pool entry —
        // the exec thread (which lags commit) must still HIT.
        mempool.remove_committed_native(&[hash]);

        let metrics = Arc::new(torus_telemetry::Metrics::new());
        let ctx_on = make_exec_ctx_with_metrics(
            &config_on,
            &db_on,
            Some(mempool.clone()),
            true,
            metrics.clone(),
        );
        fund_evm_balance(&db_on, sender, start);
        ctx_on.execute_committed_block(&make_block(1, vec![signed.clone()]), vec![]);
        let warm_balance = read_evm_balance(&db_on, sender);

        // INVARIANT 3: hit-path state == miss-path state.
        assert_eq!(
            warm_balance, cold_balance,
            "determinism: commit with a HIT must reach identical state to a cold recover"
        );
        assert_eq!(start - warm_balance, amount, "executed exactly once, correct debit");
        // Proof leg: exactly one recover was skipped on the exec path.
        assert_eq!(
            metrics.exec_verify_skipped.get(),
            1,
            "the ingress-verified action must skip re-verify on commit (counter proves it)"
        );
    }

    /// INVARIANT 2 (never skip-by-default): an action the validator saw ONLY inside
    /// the block — never at gossip/RPC ingress, so never seeded — MUST be fully
    /// verified on commit. Cache MISS => full recover, `exec_verify_skipped` stays 0.
    #[test]
    fn trust_cache_block_only_action_is_fully_verified_no_skip() {
        let amount = U256::from(FixedPoint::ONE.raw() as u128);
        let key = k256::ecdsa::SigningKey::from_slice(&[42u8; 32]).unwrap();
        let signed = torus_types::eip712::sign_native_action(
            NativeAction::TransferToPerp { amount },
            424_242,
            &key,
        );
        let sender = signed.recover_sender().unwrap();
        let start = amount * U256::from(10u8);

        let (config, db) = make_test_config_and_db();
        // Empty mempool: the action was NEVER admitted/seeded (block-only).
        let mempool = Arc::new(Mempool::new(
            db.clone(),
            torus_mempool::MempoolConfig::default(),
        ));
        let metrics = Arc::new(torus_telemetry::Metrics::new());
        // Flag ON, but the cache is empty -> MISS -> full recover (never skip).
        let ctx = make_exec_ctx_with_metrics(&config, &db, Some(mempool), true, metrics.clone());
        fund_evm_balance(&db, sender, start);
        ctx.execute_committed_block(&make_block(1, vec![signed.clone()]), vec![]);

        assert_eq!(
            start - read_evm_balance(&db, sender),
            amount,
            "block-only action must still be fully verified and executed (invariant 2)"
        );
        assert_eq!(
            metrics.exec_verify_skipped.get(),
            0,
            "a cache MISS must NEVER skip verification (no skip-by-default)"
        );
    }

    /// FEATURE #13 PROOF LEG + determinism: a SESSION-signed action verified at
    /// gossip INGRESS (real `add_native_action_from_gossip` recover path, which
    /// populates the sig-validity cache) then committed MUST skip the ed25519
    /// re-verify at exec, and resolve to the SAME owner a cold full-verify would.
    /// The `exec_session_sig_skipped` counter proves the skip fired. (Byte-identity
    /// of the HIT vs MISS resolved-sender vector across every session shape is
    /// exhaustively proven at the `batch_verify_native_actions_cached` level in
    /// torus-types; here we prove the real ingress->commit->exec flow HITs.)
    #[test]
    fn session_sig_cache_ingress_then_commit_skips_verify() {
        let ed = ed25519_dalek::SigningKey::from_bytes(&[55u8; 32]);
        let pubkey = ed.verifying_key().to_bytes();
        let owner = Address::from([0x5A; 20]);
        let session = torus_types::SessionData {
            owner,
            expiry: u64::MAX,
            scope: torus_types::SessionScope::Trading,
            created_at: 0,
        };
        // Live-window nonce so the real ingress admit path accepts it.
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        let signed = torus_types::eip712::sign_native_action_with_session(
            NativeAction::CancelOrder { order_id: 7 },
            nonce,
            &ed,
        );
        let sig_key = torus_types::session_validity_cache_key(&signed).unwrap();

        let (config_on, db_on) = make_test_config_and_db();
        db_on.put_session(&pubkey, &session).unwrap();
        let mempool = Arc::new(Mempool::new(
            db_on.clone(),
            torus_mempool::MempoolConfig::default(),
        ));
        // Real ingress verify path: validates the ed25519 session sig and populates
        // the sig-validity cache (verified_locally=true).
        mempool
            .add_native_action_from_gossip(owner, signed.clone())
            .expect("gossip ingress session verify + admit");
        assert!(
            mempool.session_sig_verified(&sig_key),
            "ingress verify must populate the session sig cache"
        );

        let metrics = Arc::new(torus_telemetry::Metrics::new());
        let ctx_on = make_exec_ctx_with_metrics(
            &config_on,
            &db_on,
            Some(mempool.clone()),
            true,
            metrics.clone(),
        );
        ctx_on.execute_committed_block(&make_block(1, vec![signed.clone()]), vec![]);

        // Proof leg: exactly one ed25519 session verify was skipped at exec.
        assert_eq!(
            metrics.exec_session_sig_skipped.get(),
            1,
            "the ingress-verified session action must skip re-verify on commit"
        );
    }

    /// FEATURE #13 INVARIANT 2 (never skip-by-default): a session action seen ONLY
    /// inside the block — never verified at ingress, so never cached — MUST be fully
    /// ed25519-verified on commit. Cache MISS => `exec_session_sig_skipped` stays 0.
    #[test]
    fn session_sig_cache_block_only_action_is_fully_verified_no_skip() {
        let ed = ed25519_dalek::SigningKey::from_bytes(&[56u8; 32]);
        let pubkey = ed.verifying_key().to_bytes();
        let owner = Address::from([0x5B; 20]);
        let session = torus_types::SessionData {
            owner,
            expiry: u64::MAX,
            scope: torus_types::SessionScope::Trading,
            created_at: 0,
        };
        let signed = torus_types::eip712::sign_native_action_with_session(
            NativeAction::CancelOrder { order_id: 8 },
            777_777,
            &ed,
        );

        let (config, db) = make_test_config_and_db();
        db.put_session(&pubkey, &session).unwrap();
        // Empty mempool: the action was NEVER admitted/seeded (block-only).
        let mempool = Arc::new(Mempool::new(
            db.clone(),
            torus_mempool::MempoolConfig::default(),
        ));
        let metrics = Arc::new(torus_telemetry::Metrics::new());
        // Flag ON, but the sig cache is empty -> MISS -> full ed25519 verify.
        let ctx = make_exec_ctx_with_metrics(&config, &db, Some(mempool), true, metrics.clone());
        ctx.execute_committed_block(&make_block(1, vec![signed.clone()]), vec![]);

        assert_eq!(
            metrics.exec_session_sig_skipped.get(),
            0,
            "a sig-cache MISS must NEVER skip ed25519 verify (no skip-by-default)"
        );
    }

    /// DRIVING REGRESSION (item #5, seconds-vs-ms session expiry): a session whose
    /// MS expiry is well in the past relative to a realistic SECONDS block timestamp
    /// MUST be rejected at execution — it resolves to `None`, is skipped, and is
    /// NOT counted in `native_actions_processed`.
    ///
    /// Before the fix the exec caller passed `header.timestamp` (SECONDS) into
    /// `batch_verify`, whose expiry comparison is in MS; the tiny seconds value was
    /// always `<=` any ms expiry, so on-chain sessions NEVER expired (~56000-year
    /// window). The fix passes `header.timestamp.saturating_mul(1000)`. This test
    /// FAILS on the old code (the expired action is wrongly processed = 1) and
    /// PASSES post-fix (0).
    #[test]
    fn expired_session_rejected_at_exec_with_seconds_block_timestamp() {
        let ed = ed25519_dalek::SigningKey::from_bytes(&[71u8; 32]);
        let pubkey = ed.verifying_key().to_bytes();
        let owner = Address::from([0x71; 20]);
        let ts_seconds = 1_700_000_000u64;
        // Expired ~200 s ago in ms terms — past the 120 s exec grace window.
        let expiry_ms = ts_seconds * 1000 - 200_000;
        let session = torus_types::SessionData {
            owner,
            expiry: expiry_ms,
            scope: torus_types::SessionScope::Trading,
            created_at: 0,
        };
        // Nonce value is irrelevant at exec (no nonce-window check on that path).
        let signed = torus_types::eip712::sign_native_action_with_session(
            NativeAction::CancelOrder { order_id: 9 },
            expiry_ms,
            &ed,
        );

        let (config, db) = make_test_config_and_db();
        db.put_session(&pubkey, &session).unwrap();
        let metrics = Arc::new(torus_telemetry::Metrics::new());
        let ctx = make_exec_ctx_with_metrics(&config, &db, None, false, metrics.clone());

        // Block carries a realistic SECONDS timestamp, as real block headers do.
        let mut block = make_block(1, vec![signed.clone()]);
        block.header.timestamp = ts_seconds;
        ctx.execute_committed_block(&block, vec![]);

        assert_eq!(
            metrics.native_actions_processed.get(),
            0,
            "expired session (ms expiry past grace) must be rejected at exec, not processed"
        );
    }

    /// Companion to the driving regression: the SAME session, still LIVE in ms
    /// terms (expiry comfortably in the future), is accepted and processed at exec
    /// under the ms-correct caller — proving the fix doesn't over-reject live
    /// sessions.
    #[test]
    fn live_session_accepted_at_exec_with_seconds_block_timestamp() {
        let ed = ed25519_dalek::SigningKey::from_bytes(&[72u8; 32]);
        let pubkey = ed.verifying_key().to_bytes();
        let owner = Address::from([0x72; 20]);
        let ts_seconds = 1_700_000_000u64;
        // Live: expires 1 h in the future (ms).
        let expiry_ms = ts_seconds * 1000 + 3_600_000;
        let session = torus_types::SessionData {
            owner,
            expiry: expiry_ms,
            scope: torus_types::SessionScope::Trading,
            created_at: 0,
        };
        let signed = torus_types::eip712::sign_native_action_with_session(
            NativeAction::CancelOrder { order_id: 10 },
            ts_seconds * 1000,
            &ed,
        );

        let (config, db) = make_test_config_and_db();
        db.put_session(&pubkey, &session).unwrap();
        let metrics = Arc::new(torus_telemetry::Metrics::new());
        let ctx = make_exec_ctx_with_metrics(&config, &db, None, false, metrics.clone());

        let mut block = make_block(1, vec![signed.clone()]);
        block.header.timestamp = ts_seconds;
        ctx.execute_committed_block(&block, vec![]);

        assert_eq!(
            metrics.native_actions_processed.get(),
            1,
            "a live session must still be accepted and processed at exec"
        );
    }

    // ---- proposer-side near-expiry filter (Option A) ----

    /// The proposer filter drops ONLY session-signed actions within
    /// `PROPOSAL_EXPIRY_MARGIN_MS` of expiry, reports the count, keeps healthy
    /// sessions, keeps unresolved sessions (execution is the authority), and keeps
    /// EIP-712 actions. NON-CONSUMPTION is structural: the helper is a pure
    /// transform over the SELECTED list and never touches the mempool pool.
    #[test]
    fn proposer_filter_skips_near_expiry_counts_and_keeps_rest() {
        let now_ms = 1_700_000_000_000u64;
        let margin = super::PROPOSAL_EXPIRY_MARGIN_MS;

        let ed_near = ed25519_dalek::SigningKey::from_bytes(&[81u8; 32]);
        let pk_near = ed_near.verifying_key().to_bytes();
        let ed_far = ed25519_dalek::SigningKey::from_bytes(&[82u8; 32]);
        let pk_far = ed_far.verifying_key().to_bytes();

        let a_near = (
            Address::from([1; 20]),
            torus_types::eip712::sign_native_action_with_session(
                NativeAction::CancelOrder { order_id: 1 },
                now_ms,
                &ed_near,
            ),
        );
        let a_far = (
            Address::from([2; 20]),
            torus_types::eip712::sign_native_action_with_session(
                NativeAction::CancelOrder { order_id: 2 },
                now_ms,
                &ed_far,
            ),
        );
        // Unknown session pubkey -> lookup None -> must be KEPT (exec decides).
        let ed_unknown = ed25519_dalek::SigningKey::from_bytes(&[83u8; 32]);
        let a_unknown = (
            Address::from([3; 20]),
            torus_types::eip712::sign_native_action_with_session(
                NativeAction::CancelOrder { order_id: 3 },
                now_ms,
                &ed_unknown,
            ),
        );
        let a_eip712 = (Address::from([4; 20]), sign_claim_rewards(now_ms));

        let lookup = move |pk: &[u8; 32]| {
            if pk == &pk_near {
                Some(now_ms + margin - 1) // just inside the margin -> filtered
            } else if pk == &pk_far {
                Some(now_ms + margin + 10_000) // well beyond the margin -> kept
            } else {
                None // unknown -> None -> kept
            }
        };

        let selected = vec![
            a_near.clone(),
            a_far.clone(),
            a_unknown.clone(),
            a_eip712.clone(),
        ];
        let (kept, filtered) =
            super::filter_near_expiry_sessions(selected, now_ms, margin, lookup);

        assert_eq!(filtered, 1, "exactly the near-expiry session action is filtered");
        let hashes: Vec<_> = kept
            .iter()
            .map(|(_, a)| torus_types::compute_action_hash(a))
            .collect();
        assert!(
            !hashes.contains(&torus_types::compute_action_hash(&a_near.1)),
            "near-expiry session filtered out of the proposed block"
        );
        assert!(
            hashes.contains(&torus_types::compute_action_hash(&a_far.1)),
            "healthy session kept"
        );
        assert!(
            hashes.contains(&torus_types::compute_action_hash(&a_unknown.1)),
            "unresolved session kept (execution is the authority)"
        );
        assert!(
            hashes.contains(&torus_types::compute_action_hash(&a_eip712.1)),
            "EIP-712 action kept"
        );
        assert_eq!(kept.len(), 3);
    }

    /// Boundary: a session expiring EXACTLY at `now + margin` is kept (`>=` cutoff).
    #[test]
    fn proposer_filter_boundary_at_exactly_margin_is_kept() {
        let now_ms = 1_700_000_000_000u64;
        let margin = super::PROPOSAL_EXPIRY_MARGIN_MS;
        let ed = ed25519_dalek::SigningKey::from_bytes(&[84u8; 32]);
        let pk = ed.verifying_key().to_bytes();
        let a = (
            Address::from([9; 20]),
            torus_types::eip712::sign_native_action_with_session(
                NativeAction::CancelOrder { order_id: 5 },
                now_ms,
                &ed,
            ),
        );
        let lookup = move |p: &[u8; 32]| (p == &pk).then_some(now_ms + margin);
        let (kept, filtered) =
            super::filter_near_expiry_sessions(vec![a], now_ms, margin, lookup);
        assert_eq!(filtered, 0, "expiry exactly at the margin boundary is kept");
        assert_eq!(kept.len(), 1);
    }

    /// INVARIANT 1 (key commits to the signature): `compute_action_hash` OMITS the
    /// signature (torus-types:863), so it is an insufficient key. A cached entry for
    /// action A (payload P, nonce N, sig_A) MUST NOT authorize a DIFFERENT action B
    /// with the SAME payload+nonce (== same `compute_action_hash`) but a different /
    /// forged signature. Because the trust-cache is keyed by the signature-committing
    /// `verified_cache_key`, B's key differs => cache MISS => full recover: a valid
    /// re-sign resolves to ITS OWN signer (never A's), and a forged sig resolves to
    /// None (the slash signal) — the substitution attack is defeated.
    #[test]
    fn trust_cache_substitution_attack_rejected() {
        use torus_types::ActionSignature;
        let (_config, state_db) = make_test_config_and_db();
        let mempool = Mempool::new(state_db.clone(), torus_mempool::MempoolConfig::default());
        let base = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;

        let k_a = k256::ecdsa::SigningKey::from_slice(&[51u8; 32]).unwrap();
        let k_b = k256::ecdsa::SigningKey::from_slice(&[52u8; 32]).unwrap();

        // A: verified at ingress -> seeds cache[key(A)] = sender_a.
        let action_a = torus_types::eip712::sign_native_action(NativeAction::ClaimRewards, base + 1, &k_a);
        let sender_a = action_a.recover_sender().unwrap();
        mempool
            .add_native_action_from_gossip(sender_a, action_a.clone())
            .expect("A admitted + seeded");

        // B: SAME payload + SAME nonce as A (=> identical compute_action_hash) but a
        // DIFFERENT, valid signature (signer k_b).
        let action_b = torus_types::eip712::sign_native_action(NativeAction::ClaimRewards, base + 1, &k_b);
        let sender_b = action_b.recover_sender().unwrap();
        assert_ne!(sender_a, sender_b, "distinct signers");
        assert_eq!(
            torus_types::compute_action_hash(&action_a),
            torus_types::compute_action_hash(&action_b),
            "precondition: identical action-hash (signature omitted) — the substitution surface"
        );
        assert_ne!(
            torus_types::verified_cache_key(&action_a),
            torus_types::verified_cache_key(&action_b),
            "the trust-cache key MUST differ because it commits to the signature"
        );

        // C: FORGED — A's payload+nonce with a corrupted signature (recovers to nobody).
        let mut action_c = torus_types::eip712::sign_native_action(NativeAction::ClaimRewards, base + 1, &k_a);
        if let ActionSignature::Eip712(ref mut sig) = action_c.signature {
            sig.r = [0xff; 32];
            sig.s = [0xff; 32];
        }
        assert_eq!(
            torus_types::compute_action_hash(&action_a),
            torus_types::compute_action_hash(&action_c),
            "precondition: forged C shares A's action-hash"
        );

        // Verify B and C against the cache warmed by A. Neither may borrow A's sender.
        let resolved = torus_types::eip712::batch_verify_native_actions_cached(
            &[action_b.clone(), action_c.clone()],
            base,
            |_| None,
            |key| mempool.verified_sender(key),
            |key| mempool.session_sig_verified(key),
        );
        assert_eq!(
            resolved[0],
            Some(sender_b),
            "B is re-verified to ITS OWN signer, never A's cached sender"
        );
        assert_ne!(
            resolved[0],
            Some(sender_a),
            "the substitution MUST NOT resolve to A's cached sender"
        );
        assert_eq!(
            resolved[1], None,
            "forged C -> cache MISS -> recover fails -> None (slash signal)"
        );
    }

    /// Seed an EVM balance into CF_ACCOUNTS (72-byte record: balance ++ nonce ++ code_hash).
    /// `Lockbox::get_evm_balance` reads the first 32 bytes big-endian.
    fn fund_evm_balance(state_db: &StateDb, addr: Address, balance: U256) {
        let mut rec = vec![0u8; 72];
        rec[..32].copy_from_slice(&balance.to_be_bytes::<32>());
        state_db
            .put_cf_raw(torus_state::cf::CF_ACCOUNTS, addr.as_slice(), &rec)
            .unwrap();
    }

    fn read_evm_balance(state_db: &StateDb, addr: Address) -> U256 {
        match state_db
            .get_cf_raw(torus_state::cf::CF_ACCOUNTS, addr.as_slice())
            .unwrap()
        {
            Some(d) if d.len() >= 32 => U256::from_be_slice(&d[..32]),
            _ => U256::ZERO,
        }
    }

    /// REGRESSION (correctness/fund-safety): an identical native action committed in
    /// several consecutive blocks must be EXECUTED exactly once.
    ///
    /// The non-destructive mempool selection × HotStuff 3-chain pipeline re-includes
    /// the same action in blocks N, N+1, N+2 before N commits (memory b206f59c). The
    /// live commit path had NO per-(sender,nonce) replay guard, so each inclusion
    /// re-executed — proven on devnet: one TransferToPerp credited 3x
    /// (devnet/scripts/native-transfer-probe.py). Here we commit the SAME signed
    /// TransferToPerp in three blocks and assert the sender's EVM balance is debited
    /// for exactly ONE deposit (not three).
    #[test]
    fn duplicate_committed_native_action_executes_once() {
        let (config, state_db) = make_test_config_and_db();
        let exec_ctx = make_exec_ctx(&config, &state_db);

        // amount is a raw 8-decimal FixedPoint (u256_to_fp), NOT 18-decimal wei.
        let amount_raw: u128 = FixedPoint::ONE.raw() as u128; // 1.0 == 100_000_000
        let amount = U256::from(amount_raw);
        let key = k256::ecdsa::SigningKey::from_slice(&[7u8; 32]).unwrap();
        let signed = torus_types::eip712::sign_native_action(
            NativeAction::TransferToPerp { amount },
            424_242,
            &key,
        );
        let sender = signed.recover_sender().expect("recover sender");

        // Fund enough EVM balance for THREE deposits, so the only thing that can stop
        // a 2nd/3rd execution is correct dedup — never insufficient funds.
        let start_balance = amount * U256::from(10u8);
        fund_evm_balance(&state_db, sender, start_balance);

        // Same action committed in three consecutive blocks (the proven bug scenario).
        for height in 1..=3u64 {
            exec_ctx.execute_committed_block(&make_block(height, vec![signed.clone()]), vec![]);
        }

        let evm_after = read_evm_balance(&state_db, sender);
        let debited = start_balance - evm_after;
        assert_eq!(
            debited, amount,
            "TransferToPerp committed in 3 blocks must debit EVM balance ONCE (got {debited}, \
             expected {amount}); duplicate execution = fund-safety bug",
        );

        // The consumed nonce must be recorded in CF_NATIVE_NONCES.
        let nonce_key = torus_state::cf::native_nonce_key(&sender, signed.nonce);
        assert!(
            state_db
                .get_cf_raw(torus_state::cf::CF_NATIVE_NONCES, &nonce_key)
                .unwrap()
                .is_some(),
            "consumed nonce should be recorded",
        );
    }

    /// P3 Task-1: with NO body writer (default), the per-block CF_BLOCK_BODIES
    /// JSON is written synchronously and is immediately readable + byte-identical
    /// to a fresh serialize — the exact contract the crash-recovery replay, RPC
    /// getBlock, and pruner all depend on.
    #[test]
    fn body_persist_sync_readable_without_writer() {
        let (config, state_db) = make_test_config_and_db();
        let mut exec_ctx = make_exec_ctx(&config, &state_db);
        assert!(exec_ctx.body_writer.is_none(), "default path is synchronous");

        let block = make_block(1, vec![sign_claim_rewards(7)]);
        exec_ctx.execute_committed_block(&block, vec![]);

        let stored = state_db
            .get_cf_raw(CF_BLOCK_BODIES, &1u64.to_be_bytes())
            .unwrap()
            .expect("body must be present immediately after synchronous commit");
        let expected = serde_json::to_vec(&block.body()).unwrap();
        assert_eq!(stored, expected, "stored body JSON must be byte-identical");
        // ...and it must parse back through the recovery path's decoder.
        let parsed: TorusBlockBody = serde_json::from_slice(&stored).unwrap();
        assert_eq!(parsed.native_actions.len(), block.native_actions.len());
    }

    /// P3 Task-1: with a background body writer attached (TORUS_ASYNC_BODY_PERSIST
    /// on the live node), the CF_BLOCK_BODIES put moves off the execution thread.
    /// Dropping the writer drains + joins, so the body must then be durable AND
    /// byte-identical to the synchronous path — the deferral changes only WHERE
    /// the write happens, never the stored format the readers parse.
    #[test]
    fn body_persist_async_writer_roundtrip() {
        let (config, state_db) = make_test_config_and_db();
        let mut exec_ctx = make_exec_ctx(&config, &state_db);
        exec_ctx.body_writer = Some(torus_state::BackgroundCfWriter::spawn(
            state_db.clone(),
            "test-body-writer",
            8,
        ));

        let block = make_block(1, vec![sign_claim_rewards(7)]);
        exec_ctx.execute_committed_block(&block, vec![]);

        // Drop the writer -> channel closes, queue drains, thread joins.
        exec_ctx.body_writer = None;

        let stored = state_db
            .get_cf_raw(CF_BLOCK_BODIES, &1u64.to_be_bytes())
            .unwrap()
            .expect("async body must be durable after the writer drains on drop");
        let expected = serde_json::to_vec(&block.body()).unwrap();
        assert_eq!(
            stored, expected,
            "async-written body JSON must be byte-identical to the synchronous path"
        );
    }

    /// O3: with a background trade writer attached, fills buffer their
    /// trade-history KVs during exec (`ctx.defer_trades`) and the writer
    /// persists them off the execution thread. Dropping the ExecutionContext
    /// closes the writer's channel, drains the queue, and joins — the shutdown
    /// flush ordering — so every row must be durable afterwards.
    #[test]
    fn deferred_trades_reach_db_via_background_writer() {
        let (config, state_db) = make_test_config_and_db();
        let mut exec_ctx = make_exec_ctx(&config, &state_db);
        exec_ctx.trade_writer = Some(torus_state::BackgroundCfWriter::spawn(
            state_db.clone(),
            "test-trade-writer",
            8,
        ));

        let k_sell = k256::ecdsa::SigningKey::from_slice(&[31u8; 32]).unwrap();
        let k_buy = k256::ecdsa::SigningKey::from_slice(&[32u8; 32]).unwrap();

        // Block 1: fund native balances through the public path (EVM balance ->
        // TransferToPerp), so block 2's margin reserve succeeds.
        let deposit_raw: u128 = 1_000 * FixedPoint::ONE.raw() as u128; // 1000.0
        let deposit = U256::from(deposit_raw);
        let mut transfers = Vec::new();
        for (i, key) in [&k_sell, &k_buy].into_iter().enumerate() {
            let signed = torus_types::eip712::sign_native_action(
                NativeAction::TransferToPerp { amount: deposit },
                9_000 + i as u64,
                key,
            );
            fund_evm_balance(&state_db, signed.recover_sender().unwrap(), deposit);
            transfers.push(signed);
        }
        exec_ctx.execute_committed_block(&make_block(1, transfers), vec![]);

        // Block 2: a resting sell crossed by a buy -> exactly one fill.
        let order = |is_buy: bool| torus_types::PlaceOrderParams {
            market_id: 1,
            is_buy,
            price: FixedPoint::from_raw(100 * FixedPoint::SCALE),
            quantity: FixedPoint::from_raw(FixedPoint::SCALE),
            order_type: torus_types::OrderType::Limit,
            time_in_force: torus_types::TimeInForce::GTC,
            reduce_only: false,
            client_order_id: None,
        };
        let sell = torus_types::eip712::sign_native_action(
            NativeAction::PlaceOrder(order(false)),
            9_002,
            &k_sell,
        );
        let buy = torus_types::eip712::sign_native_action(
            NativeAction::PlaceOrder(order(true)),
            9_003,
            &k_buy,
        );
        exec_ctx.execute_committed_block(&make_block(2, vec![sell, buy]), vec![]);

        // Shutdown flush ordering: drop drains + joins the writer before reads.
        drop(exec_ctx);

        let trades =
            StateBackend::iterate_cf(&state_db, torus_state::cf::CF_NATIVE_TRADES, None).unwrap();
        let user_trades =
            StateBackend::iterate_cf(&state_db, torus_state::cf::CF_NATIVE_USER_TRADES, None)
                .unwrap();
        assert_eq!(
            trades.len(),
            1,
            "one fill -> one trade row via the background writer"
        );
        assert_eq!(user_trades.len(), 2, "maker + taker user-trade rows");
    }

    #[test]
    fn duplicate_committed_native_batch_consumes_nonce_once() {
        // Phase B / Task B5: a PlaceOrderBatch carries many orders but ONE (sender, nonce),
        // so committing the same batch twice must execute it once (replay guard at the
        // per-action (sender, nonce) granularity — the 2e52851 fix, unchanged by batching).
        let (config, state_db) = make_test_config_and_db();
        let exec_ctx = make_exec_ctx(&config, &state_db);

        let mk = |market_id: u64| torus_types::PlaceOrderParams {
            market_id,
            is_buy: true,
            price: FixedPoint::from_raw(100 * FixedPoint::SCALE),
            quantity: FixedPoint::from_raw(FixedPoint::SCALE),
            order_type: torus_types::OrderType::Limit,
            time_in_force: torus_types::TimeInForce::GTC,
            reduce_only: false,
            client_order_id: None,
        };
        let key = k256::ecdsa::SigningKey::from_slice(&[9u8; 32]).unwrap();
        let signed = torus_types::eip712::sign_native_action(
            NativeAction::PlaceOrderBatch(vec![mk(1), mk(2), mk(3)]),
            555_555,
            &key,
        );
        let sender = signed.recover_sender().expect("recover sender");

        // Same batch committed in two consecutive blocks (the proven dup scenario).
        exec_ctx.execute_committed_block(&make_block(1, vec![signed.clone()]), vec![]);
        exec_ctx.execute_committed_block(&make_block(2, vec![signed.clone()]), vec![]);

        // Recorded exactly once; the stored value is the FIRST commit height (1),
        // proving the second commit was skipped (not re-executed / re-written).
        let nonce_key = torus_state::cf::native_nonce_key(&sender, signed.nonce);
        let recorded = state_db
            .get_cf_raw(torus_state::cf::CF_NATIVE_NONCES, &nonce_key)
            .unwrap()
            .expect("batch must consume its (sender, nonce) once");
        assert_eq!(
            recorded.as_slice(),
            &1u64.to_be_bytes(),
            "replay guard must skip the second commit (nonce height stays 1)",
        );
    }

    /// Exec-ceiling Option A: executing ONE native block must observe each
    /// phase histogram exactly once and count the executed action, so the
    /// live probe's per-phase sums decompose `exec_block_seconds` cleanly
    /// (per-phase count == native-block count by design).
    #[test]
    fn exec_phase_histograms_observe_per_block() {
        let (config, state_db) = make_test_config_and_db();
        let mut exec_ctx = make_exec_ctx(&config, &state_db);
        let metrics = Arc::new(torus_telemetry::Metrics::new());
        exec_ctx.metrics = Some(metrics.clone());

        exec_ctx.execute_committed_block(&make_block(1, vec![sign_claim_rewards(7)]), vec![]);

        let text = metrics.encode();
        for name in [
            "torus_exec_verify_seconds",
            "torus_exec_replay_guard_seconds",
            "torus_exec_engine_seconds",
            "torus_exec_save_books_seconds",
            "torus_exec_flush_seconds",
            "torus_exec_block_seconds",
            // P3 Task-1: unaccounted-gap decomposition families. Each fires once
            // per native block (body_* fire on every block, native or empty).
            "torus_exec_seed_bundle_seconds",
            "torus_exec_ctx_setup_seconds",
            "torus_exec_post_native_seconds",
            "torus_exec_body_serialize_seconds",
            "torus_exec_body_put_seconds",
        ] {
            assert!(
                text.contains(&format!("{name}_count 1")),
                "{name} must observe exactly once per native block:\n{text}",
            );
        }
        assert!(
            text.contains("torus_native_actions_processed_total 1"),
            "executed action must be counted:\n{text}",
        );
        // Direct synchronous call never touches the exec channel; the gauge's
        // inc/dec sites live there and are verified by the live probe (Task 3).
        assert!(
            text.contains("torus_exec_queue_depth 0"),
            "queue gauge must stay 0 on the direct-call path:\n{text}",
        );
    }

    /// P2 funnel item 4: the exec handoff queue gets in/out COUNTERS next to the
    /// existing depth gauge — a scrape race can't fake occupancy (in − out) and
    /// the pair exposes flow rates. The out side is driven here through the real
    /// `execution_loop`; the in side lives at the `exec_tx.send` site (verified
    /// live: in − out == exec_queue_depth).
    #[test]
    fn exec_queue_out_counts_executed_blocks() {
        let (config, state_db) = make_test_config_and_db();
        let mut exec_ctx = make_exec_ctx(&config, &state_db);
        let metrics = Arc::new(torus_telemetry::Metrics::new());
        exec_ctx.metrics = Some(metrics.clone());

        let (tx, rx) = std::sync::mpsc::sync_channel(64);
        for h in 1..=2u64 {
            tx.send(CommittedBlockMsg {
                torus_block: make_block(h, vec![]),
                pending_slashes: vec![],
            })
            .unwrap();
        }
        drop(tx); // loop exits after draining both blocks
        execution_loop(rx, exec_ctx);

        let text = metrics.encode();
        assert!(
            text.contains("torus_exec_queue_out_total 2"),
            "exec_queue_out must count each executed block:\n{text}"
        );
    }

    /// Task 3 (RED first): the pull-fallback poll budget must be ≥ 1 s so a body that
    /// arrives a few hundred ms after the request (slow peer / under load) is still
    /// recovered on the sync path. The original 4 × 20 ms = 80 ms gave up far too
    /// early — a body landing later was lost, contributing to the >4 MB wedge. The
    /// widened 20 × 50 ms = 1 s is safe here (sync path only, never consensus voting).
    #[test]
    fn pull_budget_is_at_least_one_second() {
        let budget = PULL_DELAY * PULL_RETRIES as u32;
        assert!(
            budget >= std::time::Duration::from_secs(1),
            "PULL_RETRIES({PULL_RETRIES}) * PULL_DELAY({PULL_DELAY:?}) = {budget:?} must be ≥ 1 s",
        );
    }

    /// Sprint 1 T4: the sync-path pull budget scales with how many bodies are
    /// missing — a flat ~1 s budget recovers a late single body but gives up on
    /// the multi-MB body-sets of batched blocks (s334 bs1000: fetch exhausted,
    /// fell back to sync, which also timed out). Floor stays ~1 s, cap ~8 s.
    #[test]
    fn sync_pull_retries_scales_with_missing_count() {
        assert_eq!(
            sync_pull_retries(0),
            PULL_RETRIES,
            "floor at no/low missing"
        );
        assert_eq!(sync_pull_retries(40), PULL_RETRIES, "40/2 == floor");
        assert_eq!(sync_pull_retries(100), 50, "linear midband: ~2 bodies/tick");
        assert_eq!(
            sync_pull_retries(10_000),
            MAX_SYNC_PULL_RETRIES,
            "cap at ~8 s for huge sets"
        );
    }

    /// Task 3 (behavioral): a body that only arrives AFTER the old 80 ms budget would
    /// have expired (modelled here as delivered on the 8th drain ≈ 400 ms in) is still
    /// recovered within the widened budget. The old 4-retry loop gave up at drain 4 and
    /// lost it; 20 retries reach drain 8 and recover it. This is the consensus-layer
    /// half of the >4 MB un-wedge (the codec/chunk half lives in torus-network).
    #[test]
    fn pull_recovers_body_delivered_after_old_budget() {
        // `LateFetcher` is the shared test double hoisted to module scope.
        let (config, state_db) = make_test_config_and_db();
        let mempool = Arc::new(torus_mempool::Mempool::new(
            state_db.clone(),
            torus_mempool::MempoolConfig::default(),
        ));
        let mut app = TorusApp::new(state_db.clone(), &config, None, Some(mempool.clone()), None);

        let action = sign_claim_rewards(42);
        let hash = torus_types::compute_action_hash(&action);
        let body = bincode::serialize(&action).expect("serialize body");

        // Delivered on the 8th drain — past the old 4-retry budget, within the new 20.
        let fetcher = Arc::new(LateFetcher {
            body,
            deliver_on_drain: 8,
            drains: std::sync::atomic::AtomicUsize::new(0),
        });
        app.set_native_da_fetcher(fetcher);

        assert!(
            mempool.get_native_da(&hash).is_none(),
            "body absent before the pull"
        );
        let recovered = app.pull_missing_bodies(&[hash]);
        assert!(
            recovered,
            "a body arriving after the old 80 ms budget is recovered within ~1 s"
        );
        assert!(
            mempool.get_native_da(&hash).is_some(),
            "the recovered body landed in the durable DA store",
        );
    }

    /// BS-4a (RED first): a true push miss must NOT block the consensus thread on the
    /// in-line pull budget. reconstruct returns Err (MissingData) within the shrunken
    /// local slice, and the RECOVERY WORKER pulls + absorbs the body in the background
    /// so the re-proposed view finds it locally. MUST fail before BS-4a lands (today
    /// the hot path blocks ~260ms and returns Ok via the in-line pull).
    #[test]
    fn hot_path_hands_off_and_recovers_in_background() {
        let (config, state_db) = make_test_config_and_db();
        let mempool = Arc::new(torus_mempool::Mempool::new(
            state_db.clone(),
            torus_mempool::MempoolConfig::default(),
        ));
        let mut app = TorusApp::new(state_db.clone(), &config, None, Some(mempool.clone()), None);

        let action = sign_claim_rewards(13);
        let hash = torus_types::compute_action_hash(&action);
        let body = bincode::serialize(&action).expect("serialize body");
        let fetcher = Arc::new(LateFetcher {
            body,
            deliver_on_drain: 3,
            drains: std::sync::atomic::AtomicUsize::new(0),
        });
        app.set_native_da_fetcher(fetcher);

        let start = std::time::Instant::now();
        let result = app.reconstruct_native_actions_hot(&[hash]);
        let elapsed = start.elapsed();

        assert_eq!(
            result.err(),
            Some(1),
            "a push miss fails THIS view immediately"
        );
        assert!(
            elapsed < std::time::Duration::from_millis(80),
            "consensus thread must not run the pull budget in-line: took {elapsed:?}",
        );
        // The worker recovers the body off-thread well before the next view.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while mempool.get_native_da(&hash).is_none() {
            assert!(
                std::time::Instant::now() < deadline,
                "worker never recovered the body"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    /// BS-4a: the recovery worker, fed a missing hash, pulls + absorbs the body into
    /// the durable DA store off-thread within its budget.
    #[test]
    fn recovery_worker_recovers_late_body_off_thread() {
        let (_config, state_db) = make_test_config_and_db();
        let mempool = Arc::new(torus_mempool::Mempool::new(
            state_db.clone(),
            torus_mempool::MempoolConfig::default(),
        ));

        let action = sign_claim_rewards(23);
        let hash = torus_types::compute_action_hash(&action);
        let body = bincode::serialize(&action).expect("serialize body");
        // Delivered on the 3rd drain — inside the worker budget, as in the driver test.
        let fetcher = Arc::new(LateFetcher {
            body,
            deliver_on_drain: 3,
            drains: std::sync::atomic::AtomicUsize::new(0),
        });

        // No TorusApp: the worker is constructed directly from the Arcs it owns.
        let worker = DaRecoveryWorker::spawn(mempool.clone(), fetcher, None);
        assert!(
            mempool.get_native_da(&hash).is_none(),
            "body absent before the worker pull"
        );
        worker.submit(vec![hash]);

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while mempool.get_native_da(&hash).is_none() {
            assert!(
                std::time::Instant::now() < deadline,
                "worker never recovered the body"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        // Exercise Drop-join on a live worker thread — a hang here IS the defect.
        drop(worker);
    }

    /// BS-4a: dropping the worker with an in-flight batch AND a queued backlog must
    /// join fast. Queued batches are DISCARDED via the shutdown flag (old defect:
    /// drop drained queue_depth x ~1s), and since Fix C the IN-FLIGHT batch inside
    /// recover_bodies_bounded also observes the flag (`cancel`) at the top of its
    /// wait loop, so it abandons its ~1s budget at the NEXT ~20ms slice boundary
    /// after the flag is set instead of running to completion. Uses a
    /// never-delivering fetcher so any processed batch would otherwise burn the full
    /// ~1s budget; the 3s bound is the regression separator — it cleanly separates
    /// fixed (≤~100ms: one slice) from regressed (≥~1s in-flight, up to ~9s without
    /// coalesce) while staying load-tolerant (elapsed bounds flake under
    /// parallel-suite load, mem 96dfce88).
    #[test]
    fn recovery_worker_drop_discards_backlog_and_joins_bounded() {
        let (_config, state_db) = make_test_config_and_db();
        let mempool = Arc::new(torus_mempool::Mempool::new(
            state_db.clone(),
            torus_mempool::MempoolConfig::default(),
        ));

        // NeverFetcher delivers nothing, so the in-flight batch would burn the full
        // ~1s budget if it were not cancellable — the regression this test guards.
        let worker = DaRecoveryWorker::spawn(mempool.clone(), Arc::new(NeverFetcher), None);

        // 9 distinct single-hash batches: never-arriving hashes, so the worker's
        // store-check dedup can't drop them. First is taken in-flight; the rest are
        // coalesced into that same cycle (Fix A drains the queue via try_recv), so a
        // 9th queued would warn+drop — fine either way.
        for nonce in 100u64..109 {
            let hash = torus_types::compute_action_hash(&sign_claim_rewards(nonce));
            worker.submit(vec![hash]);
        }

        // Let the worker pick up a batch in-flight (now inside recover_bodies_bounded,
        // parked in its wait loop where the cancel check lives).
        std::thread::sleep(std::time::Duration::from_millis(50));

        let t = std::time::Instant::now();
        drop(worker);
        let elapsed = t.elapsed();
        // Visible under `--nocapture`: fixed ≈ one slice (≤~100ms), regressed ≥~1s.
        println!("drop-join elapsed: {elapsed:?}");
        assert!(
            elapsed < std::time::Duration::from_secs(3),
            "drop must not drain the backlog: took {elapsed:?}",
        );
    }

    /// Phase 2.3 (#5, RED first): when a hash-only push PRE-WARMED the bodies (they already
    /// sit in the fetcher inbound), the HOT local retry must ABSORB them into the DA store
    /// and reconstruct WITHOUT issuing a redundant network fetch. Asserts the body is
    /// recovered AND `fetch()` was never called. MUST fail before Task 4 (today only the
    /// hot-PULL phase absorbs, and it calls `fetch` first → fetch_count ≥ 1).
    #[test]
    fn prewarmed_bodies_absorbed_without_redundant_hot_fetch() {
        let (config, state_db) = make_test_config_and_db();
        let mempool = Arc::new(torus_mempool::Mempool::new(
            state_db.clone(),
            torus_mempool::MempoolConfig::default(),
        ));
        let mut app = TorusApp::new(state_db.clone(), &config, None, Some(mempool.clone()), None);

        let action = sign_claim_rewards(11);
        let hash = torus_types::compute_action_hash(&action);
        let body = bincode::serialize(&action).expect("serialize body");
        let fetcher = Arc::new(PrewarmFetcher {
            body,
            fetches: std::sync::atomic::AtomicUsize::new(0),
            drained: std::sync::atomic::AtomicBool::new(false),
        });
        app.set_native_da_fetcher(fetcher.clone());

        assert!(
            mempool.get_native_da(&hash).is_none(),
            "body absent before reconstruct"
        );
        let actions = app
            .reconstruct_native_actions_hot(&[hash])
            .expect("the pre-warmed body must be absorbed and reconstructed");
        assert_eq!(
            actions.len(),
            1,
            "the absorbed action is returned (in hash order)"
        );
        assert!(
            mempool.get_native_da(&hash).is_some(),
            "the pre-warmed body landed in the durable DA store",
        );
        assert_eq!(
            fetcher.fetches.load(std::sync::atomic::Ordering::Relaxed),
            0,
            "a pre-warmed body must be absorbed by the local retry -- no redundant hot fetch",
        );
    }

    /// BS-4a: when the body NEVER arrives, the hot path must FAIL THE VIEW (Err with
    /// the missing count) FAST — the contract is now fail-fast (one ~20 ms local slice
    /// + a non-blocking worker handoff), not merely under the 500 ms view timeout. The
    /// block is simply re-proposed next view. The 80 ms bound is 4x the 20 ms slice:
    /// under parallel-suite load elapsed bounds can flake (mem 96dfce88), so keep
    /// headroom above the nominal slice.
    #[test]
    fn hot_path_fails_view_fast_when_body_never_arrives() {
        let (config, state_db) = make_test_config_and_db();
        let mempool = Arc::new(torus_mempool::Mempool::new(
            state_db.clone(),
            torus_mempool::MempoolConfig::default(),
        ));
        let mut app = TorusApp::new(state_db.clone(), &config, None, Some(mempool.clone()), None);

        let action = sign_claim_rewards(9);
        let hash = torus_types::compute_action_hash(&action);
        app.set_native_da_fetcher(Arc::new(NeverFetcher));

        let start = std::time::Instant::now();
        let result = app.reconstruct_native_actions_hot(&[hash]);
        let elapsed = start.elapsed();

        assert_eq!(
            result.err(),
            Some(1),
            "a never-arriving body fails the view, reporting the missing count",
        );
        assert!(
            elapsed < std::time::Duration::from_millis(80),
            "hot path must fail fast (one local slice + non-blocking handoff): took {elapsed:?}",
        );
    }

    /// BS-4a: both recovery budgets stay within bounds. (a) The consensus-thread share
    /// of hot reconstruction is ONE local wake-on-arrival slice — well under the 500 ms
    /// view timeout (hotstuff: 4*EWNL + produce + validate < max_view_time), so
    /// validate_block fails a missed view instead of burning it. (b) The worker's
    /// off-thread pull budget stays under ~2 s, so an unrecoverable batch never
    /// outlives more than a couple of re-propose cycles (which resubmit the same
    /// hashes anyway). Companion of `pull_budget_is_at_least_one_second` (sync LOWER
    /// bound). Renamed from `hot_pull_budget_under_view_timeout` — there is no in-line
    /// hot pull anymore.
    #[test]
    fn recovery_budgets_within_bounds() {
        let local = RECONSTRUCT_RETRY_DELAY * RECONSTRUCT_RETRIES as u32;
        assert!(
            local < std::time::Duration::from_millis(50),
            "consensus-thread budget {local:?} (one slice) must be well under the 500 ms view timeout",
        );
        let worker = WORKER_PULL_DELAY * WORKER_PULL_RETRIES as u32;
        assert!(
            worker < std::time::Duration::from_secs(2),
            "worker pull budget {worker:?} must not outlive a couple of re-propose cycles",
        );
    }

    /// BS-4b (RED first): a body that never arrives must trigger EXACTLY ONE
    /// mid-budget re-fetch — covering a lost pull request/response — on top of the
    /// initial fetch, and NOT one re-fetch per wait tick. Drives `recover_bodies_bounded`
    /// directly with a small budget (10 × 10 ms = 100 ms, midpoint ~50 ms). Before the
    /// re-fetch lands this fails on `fetch_calls == 2` (today only the initial fetch fires).
    #[test]
    fn recover_bodies_refetches_once_mid_budget() {
        let (_config, state_db) = make_test_config_and_db();
        let mempool = Arc::new(torus_mempool::Mempool::new(
            state_db.clone(),
            torus_mempool::MempoolConfig::default(),
        ));

        let action = sign_claim_rewards(77);
        let hash = torus_types::compute_action_hash(&action);
        let body = bincode::serialize(&action).expect("serialize body");
        // Never delivered: the body stays missing for the whole budget.
        let fetcher = Arc::new(CountingFetcher {
            body,
            deliver_on_drain: 0,
            fetch_calls: std::sync::atomic::AtomicUsize::new(0),
            drains: std::sync::atomic::AtomicUsize::new(0),
        });

        let recovered = recover_bodies_bounded(
            &mempool,
            fetcher.as_ref(),
            &[hash],
            10,
            std::time::Duration::from_millis(10),
            None,
            None,
        );
        assert!(
            !recovered,
            "a never-arriving body is not recovered in-budget"
        );
        assert_eq!(
            fetcher
                .fetch_calls
                .load(std::sync::atomic::Ordering::Relaxed),
            2,
            "initial fetch + exactly one mid-budget re-fetch (not one per tick)",
        );
    }

    /// BS-4b: a body that arrives well before the budget midpoint must NOT trigger a
    /// re-fetch — the initial fetch is the only one. `deliver_on_drain: 1` lands the
    /// body on the first wait-loop drain (~10 ms into the ~100 ms budget, midpoint ~50 ms).
    #[test]
    fn recover_bodies_no_refetch_when_body_arrives_early() {
        let (_config, state_db) = make_test_config_and_db();
        let mempool = Arc::new(torus_mempool::Mempool::new(
            state_db.clone(),
            torus_mempool::MempoolConfig::default(),
        ));

        let action = sign_claim_rewards(88);
        let hash = torus_types::compute_action_hash(&action);
        let body = bincode::serialize(&action).expect("serialize body");
        // Delivered on the first drain after the initial fetch — well before the midpoint.
        let fetcher = Arc::new(CountingFetcher {
            body,
            deliver_on_drain: 1,
            fetch_calls: std::sync::atomic::AtomicUsize::new(0),
            drains: std::sync::atomic::AtomicUsize::new(0),
        });

        let recovered = recover_bodies_bounded(
            &mempool,
            fetcher.as_ref(),
            &[hash],
            10,
            std::time::Duration::from_millis(10),
            None,
            None,
        );
        assert!(recovered, "an early-arriving body is recovered");
        assert_eq!(
            fetcher
                .fetch_calls
                .load(std::sync::atomic::Ordering::Relaxed),
            1,
            "body arrived before the midpoint — only the initial fetch, no re-fetch",
        );
    }
}
