//! Running state hash, node side (docs/plans/running-state-hash-impl.md):
//! the validator's automatic `AttestStateHash` submitter (Task 6) and the
//! always-on mismatch detection with the gated fail-stop (Task 7).
//!
//! Runs on the execution thread after every executed block and only READS
//! durable state (`CF_CONSENSUS_META` checkpoints, `cf_state_hash_votes`,
//! validators): it never writes consensus state.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

use torus_economics::types::ValidatorStatus;
use torus_economics::StakingManager;
use torus_mempool::Mempool;
use torus_state::running_hash::{
    checkpoint_heights, read_checkpoint, read_running_hash, STATE_HASH_CHECKPOINT_INTERVAL,
    STATE_HASH_CHECKPOINT_RETAIN,
};
use torus_state::StateDb;
use torus_types::{Address, NativeAction, TorusBlock, B256};

/// Pure parse of `TORUS_STATE_HASH_FAILSTOP`: ON only for exactly `1`.
pub fn parse_failstop(v: Option<String>) -> bool {
    matches!(v.as_deref().map(str::trim), Some("1"))
}

/// `TORUS_STATE_HASH_FAILSTOP=1`, read once per process. Default OFF: on a
/// 3-validator net one diverged node that stops voting halts the chain
/// (> 2/3 needs all 3), trading a silent fork for a halt.
pub fn failstop_enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| {
        let on = parse_failstop(std::env::var("TORUS_STATE_HASH_FAILSTOP").ok());
        tracing::info!(on, "running state hash fail-stop (TORUS_STATE_HASH_FAILSTOP)");
        on
    })
}

/// Ethereum address of a secp256k1 account key.
pub fn account_address(key: &k256::ecdsa::SigningKey) -> Address {
    let pubkey = key.verifying_key().to_encoded_point(false);
    Address::from_slice(&alloy_primitives::keccak256(&pubkey.as_bytes()[1..])[12..])
}

#[derive(Default)]
struct Inner {
    /// Checkpoints this process already submitted an attestation for.
    submitted: BTreeSet<u64>,
    /// Latest checkpoint the submitter has scanned for (`None` until the first
    /// block after start). Scans run once per new checkpoint, over every
    /// retained checkpoint, so a failed admission is retried at the next one.
    considered_upto: Option<u64>,
    last_nonce: u64,
    /// Detection queue: block height that carried attestations -> the
    /// checkpoint heights they attested. Checked once that block is durable.
    to_check: BTreeMap<u64, BTreeSet<u64>>,
    /// Mismatches already reported: `(checkpoint, Some(validator) | None = quorum)`.
    reported: BTreeSet<(u64, Option<Address>)>,
    /// Checkpoints already reported as all-votes-in without quorum.
    no_quorum: BTreeSet<u64>,
}

/// Per-node running-state-hash monitor, shared by the app (configuration) and
/// the execution thread ([`Self::after_block`]).
#[derive(Default)]
pub struct StateHashMonitor {
    /// The validator's ACCOUNT key (secp256k1; `--state-hash-attest-key`), the
    /// signer of its `AttestStateHash` actions. `None`: never submits.
    attest_key: OnceLock<k256::ecdsa::SigningKey>,
    /// Latch `exec_failed` when the on-chain quorum hash differs from the
    /// local checkpoint (`TORUS_STATE_HASH_FAILSTOP=1`).
    failstop: bool,
    inner: Mutex<Inner>,
}

impl StateHashMonitor {
    /// Fail-stop from `TORUS_STATE_HASH_FAILSTOP` (default off).
    pub fn new() -> Self {
        Self::with_failstop(failstop_enabled())
    }

    pub fn with_failstop(failstop: bool) -> Self {
        Self {
            failstop,
            ..Self::default()
        }
    }

    /// Configure the attestation signer; returns its address. First call wins.
    pub fn set_attest_key(&self, key: k256::ecdsa::SigningKey) -> Address {
        let addr = account_address(&key);
        let _ = self.attest_key.set(key);
        addr
    }

    /// After block `block` executed: compare the attestations that became
    /// durable with the local checkpoints, then submit this validator's own
    /// attestations for durable checkpoints it has not voted on yet. Reads
    /// durable state only — identical behaviour serial or pipelined (a
    /// pipelined block's votes are checked one block later, once durable).
    pub fn after_block(
        &self,
        block: &TorusBlock,
        state_db: &StateDb,
        mempool: Option<&Mempool>,
        metrics: Option<&torus_telemetry::Metrics>,
        exec_failed: &AtomicBool,
    ) {
        let attested: BTreeSet<u64> = block
            .native_actions
            .iter()
            .filter_map(|a| match a.action {
                NativeAction::AttestStateHash { height, .. } => Some(height),
                _ => None,
            })
            .collect();
        let Some((hashed, _)) = read_running_hash(state_db) else {
            return;
        };
        if let Some(m) = metrics {
            m.state_hash_height.set(hashed as i64);
        }
        {
            let mut inner = self.inner.lock().unwrap();
            if !attested.is_empty() {
                inner.to_check.entry(block.header.height).or_default().extend(attested);
            }
            let durable: Vec<u64> = inner.to_check.range(..=hashed).map(|(h, _)| *h).collect();
            for carried in durable {
                for checkpoint in inner.to_check.remove(&carried).unwrap_or_default() {
                    self.check(&mut inner, checkpoint, state_db, metrics, exec_failed);
                }
            }
            let floor = hashed.saturating_sub(STATE_HASH_CHECKPOINT_INTERVAL * STATE_HASH_CHECKPOINT_RETAIN);
            inner.reported.retain(|(h, _)| *h > floor);
            inner.no_quorum.retain(|h| *h > floor);
        }
        if let (Some(key), Some(mempool)) = (self.attest_key.get(), mempool) {
            self.submit_attestations(key, hashed, state_db, mempool, metrics);
        }
    }

    /// Compare every recorded vote and the quorum hash for `checkpoint` with
    /// the local checkpoint. Side effects outside consensus state only:
    /// metrics, logs, and (fail-stop on) the `exec_failed` latch.
    fn check(
        &self,
        inner: &mut Inner,
        checkpoint: u64,
        state_db: &StateDb,
        metrics: Option<&torus_telemetry::Metrics>,
        exec_failed: &AtomicBool,
    ) {
        let Some(local) = read_checkpoint(state_db, checkpoint) else {
            tracing::debug!(checkpoint, "state hash attestation for a checkpoint this node does not hold");
            return;
        };
        let local_b = B256::from(local);
        let staking = StakingManager::new(state_db.clone());
        let votes = staking.state_hash_votes(checkpoint).unwrap_or_default();
        for (validator, hash) in &votes {
            if *hash != local && inner.reported.insert((checkpoint, Some(*validator))) {
                if let Some(m) = metrics {
                    m.state_hash_mismatch
                        .get_or_create(&vec![("validator".to_string(), validator.to_string())])
                        .inc();
                }
                tracing::error!(
                    checkpoint,
                    %validator,
                    vote = %B256::from(*hash),
                    local = %local_b,
                    "STATE HASH MISMATCH: validator attestation differs from the local checkpoint"
                );
            }
        }
        match staking.state_hash_quorum(checkpoint) {
            Ok(Some(quorum)) if quorum != local => {
                if inner.reported.insert((checkpoint, None)) {
                    if let Some(m) = metrics {
                        m.state_hash_mismatch
                            .get_or_create(&vec![("validator".to_string(), "quorum".to_string())])
                            .inc();
                    }
                    tracing::error!(
                        checkpoint,
                        quorum = %B256::from(quorum),
                        local = %local_b,
                        failstop = self.failstop,
                        "STATE HASH MISMATCH: on-chain quorum hash differs from the local checkpoint — this node diverged"
                    );
                }
                if self.failstop {
                    tracing::error!(
                        checkpoint,
                        "FAIL-STOP (TORUS_STATE_HASH_FAILSTOP=1): latching exec_failed, node stops voting"
                    );
                    exec_failed.store(true, Ordering::SeqCst);
                }
            }
            Ok(None) => {
                let active = staking
                    .all_validators()
                    .map(|vs| vs.iter().filter(|v| v.status == ValidatorStatus::Active).count())
                    .unwrap_or(usize::MAX);
                if votes.len() >= active && inner.no_quorum.insert(checkpoint) {
                    if let Some(m) = metrics {
                        m.state_hash_no_quorum.inc();
                    }
                    tracing::warn!(
                        checkpoint,
                        votes = votes.len(),
                        "state hash checkpoint: every active validator voted, no > 2/3 quorum"
                    );
                }
            }
            _ => {}
        }
    }

    /// One attestation per durable checkpoint, unless this validator's vote
    /// is already on-chain (re-submission after a restart) or the checkpoint
    /// is outside the on-chain acceptance window. Duplicates that still slip
    /// through (a pre-restart submission landing later) are rejected on-chain
    /// deterministically (first vote wins).
    fn submit_attestations(
        &self,
        key: &k256::ecdsa::SigningKey,
        hashed: u64,
        state_db: &StateDb,
        mempool: &Mempool,
        metrics: Option<&torus_telemetry::Metrics>,
    ) {
        let latest = hashed - hashed % STATE_HASH_CHECKPOINT_INTERVAL;
        let mut inner = self.inner.lock().unwrap();
        if latest == 0 || inner.considered_upto.is_some_and(|c| c >= latest) {
            return;
        }
        inner.considered_upto = Some(latest);
        let me = account_address(key);
        let staking = StakingManager::new(state_db.clone());
        match staking.get_validator(&me) {
            Ok(Some(v)) if v.status == ValidatorStatus::Active => {}
            _ => return, // not an active validator: on-chain would reject it
        }
        let window = STATE_HASH_CHECKPOINT_INTERVAL * STATE_HASH_CHECKPOINT_RETAIN;
        for height in checkpoint_heights(state_db) {
            if height > latest
                || height + window <= hashed + 1
                || inner.submitted.contains(&height)
            {
                continue;
            }
            let on_chain = staking
                .state_hash_votes(height)
                .map(|votes| votes.iter().any(|(v, _)| *v == me))
                .unwrap_or(false);
            let Some(hash) = read_checkpoint(state_db, height) else {
                continue;
            };
            if on_chain {
                inner.submitted.insert(height);
                continue;
            }
            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            let nonce = now_ms.max(inner.last_nonce + 1);
            inner.last_nonce = nonce;
            let action = NativeAction::AttestStateHash {
                height,
                hash: B256::from(hash),
            };
            let signed = torus_types::eip712::sign_native_action(action, nonce, key);
            match mempool.add_native_action(signed) {
                Ok(()) => {
                    inner.submitted.insert(height);
                    if let Some(m) = metrics {
                        m.state_hash_attestations_submitted.inc();
                    }
                    tracing::info!(height, validator = %me, "state hash attestation submitted");
                }
                Err(e) => {
                    tracing::warn!(height, %e, "state hash attestation not admitted (retried at the next checkpoint)")
                }
            }
        }
        let floor = latest.saturating_sub(window);
        inner.submitted.retain(|h| *h > floor);
    }
}
