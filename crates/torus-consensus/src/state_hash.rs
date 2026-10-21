//! Running state hash, node side (docs/plans/running-state-hash-impl.md):
//! the validator's automatic `AttestStateHash` submitter (Task 6) and the
//! always-on mismatch detection with the gated fail-stop (Task 7).
//!
//! Runs on the execution thread after every executed block and only READS
//! durable state (`CF_CONSENSUS_META` checkpoints, `cf_state_hash_votes`,
//! validators): it never writes consensus state. Its only write is the
//! node-local fail-stop record (`META_STATE_HASH_DIVERGED`).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use torus_economics::types::ValidatorStatus;
use torus_economics::StakingManager;
use torus_mempool::Mempool;
use torus_state::running_hash::{
    checkpoint_heights, read_activation_height, read_applied_height, read_checkpoint,
    read_configured_activation, read_divergence, read_running_hash, read_unverified_since,
    record_divergence, STATE_HASH_CHECKPOINT_INTERVAL, STATE_HASH_CHECKPOINT_RETAIN,
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
        tracing::info!(
            on,
            "running state hash fail-stop (TORUS_STATE_HASH_FAILSTOP)"
        );
        on
    })
}

/// Ethereum address of a secp256k1 account key.
pub fn account_address(key: &k256::ecdsa::SigningKey) -> Address {
    let pubkey = key.verifying_key().to_encoded_point(false);
    Address::from_slice(&alloy_primitives::keccak256(&pubkey.as_bytes()[1..])[12..])
}

/// A signed attestation is re-signed (fresh nonce) and resubmitted only once
/// it can no longer land: its nonce is outside the ±`NONCE_WINDOW_MS`
/// acceptance window everywhere (mempool admission rejects and evicts it),
/// plus a margin for clock skew between nodes.
pub const DEFAULT_RESUBMIT_AFTER_MS: u64 = torus_types::eip712::NONCE_WINDOW_MS + 5_000;

/// While hash-unverified, the loud ERROR repeats at most this often (owner
/// s134, option b: boot and periodically).
pub const UNVERIFIED_LOG_INTERVAL: Duration = Duration::from_secs(60);

/// The periodic hash-unverified ERROR is due: never logged yet, or the last
/// one is at least [`UNVERIFIED_LOG_INTERVAL`] old.
pub fn unverified_log_due(last: Option<Instant>, now: Instant) -> bool {
    last.is_none_or(|t| now.saturating_duration_since(t) >= UNVERIFIED_LOG_INTERVAL)
}

/// The node's running-hash chain as the attest path sees it at boot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChainHealth {
    /// Not hashing at the applied height (no applied height, or the header
    /// there does not carry the `RunningStateHash` bit).
    Off,
    /// A stored chain ends at the applied height.
    Verified,
    /// Marked hash-unverified since this height (sticky until a resync or a
    /// chain reset).
    Unverified { since: u64 },
    /// Hashing at the applied height but the stored chain rows do not end
    /// there (lost: a restore without them, a damaged DB). The next flush
    /// marks the node unverified; until then it already never attests.
    RowsMissing { applied: u64, hashed: Option<u64> },
}

impl ChainHealth {
    /// No valid local chain: never attest, never fail-stop on a mismatch.
    pub fn is_unverified(self) -> bool {
        matches!(
            self,
            ChainHealth::Unverified { .. } | ChainHealth::RowsMissing { .. }
        )
    }
}

/// [`ChainHealth`] of `state_db`. Reads META and at most one header; call it
/// when nothing writes the DB (boot, after the replay).
pub fn chain_health(state_db: &StateDb) -> ChainHealth {
    if let Some(since) = read_unverified_since(state_db) {
        return ChainHealth::Unverified { since };
    }
    let Some(applied) = read_applied_height(state_db).filter(|h| *h > 0) else {
        return ChainHealth::Off;
    };
    let bit = torus_types::Feature::RunningStateHash.bit();
    // The header the applied height executed under says whether it was
    // hashed; without the row, the configured activation.
    let hashing = match crate::app::header_at(state_db, applied) {
        Ok(h) => h.features & bit != 0,
        Err(_) => read_configured_activation(state_db).is_some_and(|a| a <= applied),
    };
    if !hashing {
        return ChainHealth::Off;
    }
    match read_running_hash(state_db) {
        Some((hashed, _)) if hashed == applied && read_activation_height(state_db).is_some() => {
            ChainHealth::Verified
        }
        stored => ChainHealth::RowsMissing {
            applied,
            hashed: stored.map(|(h, _)| h),
        },
    }
}

/// A stored chain ends exactly at `durable` (the applied height read before).
/// Read while the flush worker may write: a batch moves the hash and the
/// applied marker together, so a torn read only ever says "not valid".
fn chain_valid_at(state_db: &StateDb, durable: u64) -> bool {
    read_activation_height(state_db).is_some()
        && read_running_hash(state_db).is_some_and(|(h, _)| h == durable)
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[derive(Default)]
struct Inner {
    /// Checkpoints whose vote by this validator is not on-chain yet -> nonce
    /// of the last signed submission (`None`: not signed yet). Kept until the
    /// vote is read back on-chain or the checkpoint leaves the window.
    pending: BTreeMap<u64, Option<u64>>,
    /// Checkpoints whose vote by this validator is on-chain.
    on_chain: BTreeSet<u64>,
    /// Latest checkpoint the submitter has scanned for (`None` until the first
    /// block after start: a restart rescans every retained checkpoint).
    considered_upto: Option<u64>,
    last_nonce: u64,
    /// Detection queue: block height that carried attestations -> the
    /// checkpoint heights they attested. Checked once that block is durable.
    to_check: BTreeMap<u64, BTreeSet<u64>>,
    /// Mismatches already reported: `(checkpoint, Some(validator) | None = quorum)`.
    reported: BTreeSet<(u64, Option<Address>)>,
    /// Checkpoints already reported as all-votes-in without quorum.
    no_quorum: BTreeSet<u64>,
    /// When the hash-unverified ERROR was last logged (see
    /// [`unverified_log_due`]).
    unverified_logged: Option<Instant>,
}

/// Per-node running-state-hash monitor, shared by the app (configuration) and
/// the execution thread ([`Self::after_block`]).
pub struct StateHashMonitor {
    /// The validator's ACCOUNT key (secp256k1; `--state-hash-attest-key`), the
    /// signer of its `AttestStateHash` actions. `None`: never submits.
    attest_key: OnceLock<k256::ecdsa::SigningKey>,
    /// Latch `exec_failed` when the on-chain quorum hash differs from the
    /// local checkpoint (`TORUS_STATE_HASH_FAILSTOP=1`).
    failstop: bool,
    /// See [`DEFAULT_RESUBMIT_AFTER_MS`].
    resubmit_after_ms: u64,
    /// [`Self::report_at_boot`] found [`ChainHealth::RowsMissing`]: no
    /// attestation until a stored chain ends at the durable height again
    /// (normally the next flush marks the node unverified first).
    rows_missing_at_boot: AtomicBool,
    inner: Mutex<Inner>,
}

impl Default for StateHashMonitor {
    fn default() -> Self {
        Self {
            attest_key: OnceLock::new(),
            failstop: false,
            resubmit_after_ms: DEFAULT_RESUBMIT_AFTER_MS,
            rows_missing_at_boot: AtomicBool::new(false),
            inner: Mutex::default(),
        }
    }
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

    /// Override [`DEFAULT_RESUBMIT_AFTER_MS`] (tests).
    pub fn with_resubmit_after_ms(mut self, ms: u64) -> Self {
        self.resubmit_after_ms = ms;
        self
    }

    /// Configure the attestation signer; returns its address. First call wins.
    pub fn set_attest_key(&self, key: k256::ecdsa::SigningKey) -> Address {
        let addr = account_address(&key);
        let _ = self.attest_key.set(key);
        addr
    }

    /// Boot: a persisted fail-stop record (this node diverged from the
    /// on-chain quorum hash before the restart) re-latches `exec_failed` when
    /// fail-stop is on, so a supervisor restart after exit(70) cannot resume
    /// voting on the diverged state. The record is node-local META and lives
    /// as long as the DB (a resync clears it). Returns whether it latched.
    pub fn relatch_at_boot(&self, state_db: &StateDb, exec_failed: &AtomicBool) -> bool {
        let Some(checkpoint) = read_divergence(state_db) else {
            return false;
        };
        if self.failstop {
            tracing::error!(
                checkpoint,
                "FAIL-STOP (TORUS_STATE_HASH_FAILSTOP=1): this node diverged from the on-chain \
                 quorum state hash at this checkpoint before the restart — re-latching exec_failed \
                 (resync the node to recover)"
            );
            exec_failed.store(true, Ordering::SeqCst);
            true
        } else {
            tracing::warn!(
                checkpoint,
                "running state hash: this node diverged from the on-chain quorum hash at this \
                 checkpoint (recorded); fail-stop is off, so it keeps running"
            );
            false
        }
    }

    /// Boot (after the replay): report a node without a valid running-hash
    /// chain (owner s134, option b): a loud ERROR, the
    /// `torus_state_hash_unverified` gauge, and no attestation until the
    /// chain is valid again. It keeps running and voting. Returns the health.
    pub fn report_at_boot(
        &self,
        state_db: &StateDb,
        metrics: Option<&torus_telemetry::Metrics>,
    ) -> ChainHealth {
        let health = chain_health(state_db);
        self.rows_missing_at_boot.store(
            matches!(health, ChainHealth::RowsMissing { .. }),
            Ordering::SeqCst,
        );
        if let Some(m) = metrics {
            m.state_hash_unverified.set(health.is_unverified() as i64);
        }
        if health.is_unverified() {
            self.inner.lock().unwrap().unverified_logged = Some(Instant::now());
            tracing::error!(
                ?health,
                "RUNNING STATE HASH UNVERIFIED: this node has no valid running-hash chain (the stored \
                 chain rows are lost or a height was never hashed). It keeps running and voting but \
                 submits NO state-hash attestations and makes no fail-stop decision until the chain is \
                 valid again: resync the node or restore its data dir (torus_state_hash_unverified = 1)"
            );
        }
        health
    }

    /// After block `block` executed: compare the attestations that became
    /// durable with the local checkpoints, then submit this validator's own
    /// attestations for durable checkpoints whose vote is not on-chain yet.
    /// Reads durable state only — identical behaviour serial or pipelined (a
    /// pipelined block's votes are checked once it is durable). A
    /// hash-unverified node (marked, or its rows found lost at boot) neither
    /// attests nor fail-stops; it logs a loud ERROR once per
    /// [`UNVERIFIED_LOG_INTERVAL`].
    pub fn after_block(
        &self,
        block: &TorusBlock,
        state_db: &StateDb,
        mempool: Option<&Mempool>,
        metrics: Option<&torus_telemetry::Metrics>,
        exec_failed: &AtomicBool,
    ) {
        if read_configured_activation(state_db).is_none() {
            return; // running hash disabled
        }
        let Some(durable) = read_applied_height(state_db) else {
            return;
        };
        let since = read_unverified_since(state_db);
        let mut unverified = since.is_some();
        if !unverified && self.rows_missing_at_boot.load(Ordering::SeqCst) {
            if chain_valid_at(state_db, durable) {
                self.rows_missing_at_boot.store(false, Ordering::SeqCst);
            } else {
                unverified = true;
            }
        }
        if let Some(m) = metrics {
            m.state_hash_unverified.set(unverified as i64);
            if let Some((hashed, _)) = read_running_hash(state_db) {
                m.state_hash_height.set(hashed as i64);
            }
        }
        let attested: BTreeSet<u64> = block
            .native_actions
            .iter()
            .filter_map(|a| match a.action {
                NativeAction::AttestStateHash { height, .. } => Some(height),
                _ => None,
            })
            .collect();
        {
            let mut inner = self.inner.lock().unwrap();
            if !attested.is_empty() {
                inner
                    .to_check
                    .entry(block.header.height)
                    .or_default()
                    .extend(attested);
            }
            let ready: Vec<u64> = inner.to_check.range(..=durable).map(|(h, _)| *h).collect();
            for carried in ready {
                for checkpoint in inner.to_check.remove(&carried).unwrap_or_default() {
                    self.check(
                        &mut inner,
                        checkpoint,
                        state_db,
                        metrics,
                        exec_failed,
                        !unverified,
                    );
                }
            }
            let floor = durable
                .saturating_sub(STATE_HASH_CHECKPOINT_INTERVAL * STATE_HASH_CHECKPOINT_RETAIN);
            inner.reported.retain(|(h, _)| *h > floor);
            inner.no_quorum.retain(|h| *h > floor);
            if unverified && unverified_log_due(inner.unverified_logged, Instant::now()) {
                inner.unverified_logged = Some(Instant::now());
                tracing::error!(
                    unverified_since = ?since,
                    durable,
                    "RUNNING STATE HASH UNVERIFIED: no valid running-hash chain; still running and \
                     voting, NO state-hash attestations until the node is resynced or its data dir \
                     restored (torus_state_hash_unverified = 1)"
                );
            }
        }
        if let (Some(key), Some(mempool)) = (self.attest_key.get(), mempool) {
            self.submit_attestations(key, durable, state_db, mempool, metrics, unverified);
        }
    }

    /// Compare every recorded vote and the quorum hash for `checkpoint` with
    /// the local checkpoint. Side effects outside consensus state only:
    /// metrics, logs, and (fail-stop on and `allow_failstop`) the fail-stop
    /// record + the `exec_failed` latch.
    fn check(
        &self,
        inner: &mut Inner,
        checkpoint: u64,
        state_db: &StateDb,
        metrics: Option<&torus_telemetry::Metrics>,
        exec_failed: &AtomicBool,
        allow_failstop: bool,
    ) {
        let Some(local) = read_checkpoint(state_db, checkpoint) else {
            tracing::debug!(
                checkpoint,
                "state hash attestation for a checkpoint this node does not hold"
            );
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
                if self.failstop && allow_failstop {
                    // Persist first: the latch ends in exit(70); the record
                    // re-latches it after the restart (`relatch_at_boot`).
                    if let Err(e) = record_divergence(state_db, checkpoint) {
                        tracing::error!(%e, checkpoint, "failed to persist the state hash fail-stop record");
                    }
                    tracing::error!(
                        checkpoint,
                        "FAIL-STOP (TORUS_STATE_HASH_FAILSTOP=1): latching exec_failed, node stops voting"
                    );
                    exec_failed.store(true, Ordering::SeqCst);
                } else if self.failstop {
                    tracing::warn!(
                        checkpoint,
                        "node is hash-unverified: no fail-stop decision on the quorum mismatch"
                    );
                }
            }
            Ok(None) => {
                let active = staking
                    .all_validators()
                    .map(|vs| {
                        vs.iter()
                            .filter(|v| v.status == ValidatorStatus::Active)
                            .count()
                    })
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

    /// Get this validator's vote ON-CHAIN for every durable checkpoint inside
    /// the on-chain acceptance window. A checkpoint stays pending until its
    /// vote is read back from `cf_state_hash_votes`; a submission that has not
    /// landed is re-signed with a fresh nonce once the previous signature can
    /// no longer land (`resubmit_after_ms`), so it is never dropped silently.
    /// Should two signatures land anyway, the on-chain rule (first vote per
    /// validator wins) rejects the second deterministically.
    fn submit_attestations(
        &self,
        key: &k256::ecdsa::SigningKey,
        durable: u64,
        state_db: &StateDb,
        mempool: &Mempool,
        metrics: Option<&torus_telemetry::Metrics>,
        unverified: bool,
    ) {
        // Owner s134 (option b): a node without a valid local chain never
        // attests, not even a checkpoint it still holds from before the loss.
        if unverified {
            return;
        }
        let window = STATE_HASH_CHECKPOINT_INTERVAL * STATE_HASH_CHECKPOINT_RETAIN;
        let in_window = |h: u64| h + window > durable + 1;
        let latest = durable - durable % STATE_HASH_CHECKPOINT_INTERVAL;
        let mut inner = self.inner.lock().unwrap();
        if latest > 0 && inner.considered_upto.is_none_or(|c| c < latest) {
            inner.considered_upto = Some(latest);
            for h in checkpoint_heights(state_db) {
                if h <= latest && in_window(h) && !inner.on_chain.contains(&h) {
                    inner.pending.entry(h).or_insert(None);
                }
            }
        }
        inner.pending.retain(|h, _| in_window(*h));
        inner.on_chain.retain(|h| in_window(*h));
        if inner.pending.is_empty() {
            return;
        }
        let me = account_address(key);
        let staking = StakingManager::new(state_db.clone());
        match staking.get_validator(&me) {
            Ok(Some(v)) if v.status == ValidatorStatus::Active => {}
            _ => return, // not an active validator: on-chain would reject it
        }
        let now = now_ms();
        let heights: Vec<u64> = inner.pending.keys().copied().collect();
        for height in heights {
            let on_chain = staking
                .state_hash_votes(height)
                .map(|votes| votes.iter().any(|(v, _)| *v == me))
                .unwrap_or(false);
            if on_chain {
                inner.pending.remove(&height);
                inner.on_chain.insert(height);
                continue;
            }
            let previous = inner.pending.get(&height).copied().flatten();
            if previous.is_some_and(|n| now < n.saturating_add(self.resubmit_after_ms)) {
                continue; // the previous signature may still land
            }
            let Some(hash) = read_checkpoint(state_db, height) else {
                inner.pending.remove(&height);
                continue;
            };
            let nonce = now.max(inner.last_nonce + 1);
            inner.last_nonce = nonce;
            let action = NativeAction::AttestStateHash {
                height,
                hash: B256::from(hash),
            };
            let signed = torus_types::eip712::sign_native_action(action, nonce, key);
            // Admitted or not, the next attempt waits for the window: a failed
            // admission is retried then (rate-limited), never forgotten.
            inner.pending.insert(height, Some(nonce));
            match mempool.add_native_action(signed) {
                Ok(()) => {
                    if let Some(m) = metrics {
                        m.state_hash_attestations_submitted.inc();
                    }
                    tracing::info!(
                        height,
                        validator = %me,
                        resubmission = previous.is_some(),
                        "state hash attestation submitted"
                    );
                }
                Err(e) => {
                    tracing::warn!(height, %e, "state hash attestation not admitted (retried after the resubmit window)")
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unverified_error_repeats_once_per_interval() {
        let t = Instant::now();
        assert!(unverified_log_due(None, t), "the first one is due");
        assert!(!unverified_log_due(Some(t), t));
        assert!(!unverified_log_due(
            Some(t),
            t + UNVERIFIED_LOG_INTERVAL - Duration::from_millis(1)
        ));
        assert!(unverified_log_due(Some(t), t + UNVERIFIED_LOG_INTERVAL));
    }

    #[test]
    fn rows_missing_and_marked_are_unverified() {
        assert!(ChainHealth::Unverified { since: 5 }.is_unverified());
        assert!(ChainHealth::RowsMissing {
            applied: 5,
            hashed: None
        }
        .is_unverified());
        assert!(!ChainHealth::Verified.is_unverified());
        assert!(!ChainHealth::Off.is_unverified());
    }
}
