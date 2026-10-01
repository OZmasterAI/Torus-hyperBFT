//! Running state hash, node side (docs/plans/running-state-hash-impl.md):
//! the validator's automatic `AttestStateHash` submitter (Task 6).
//!
//! Runs on the execution thread after every executed block and only READS
//! durable state (`CF_CONSENSUS_META` checkpoints, `cf_state_hash_votes`,
//! validators): it never writes consensus state.

use std::collections::BTreeSet;
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
}

/// Per-node running-state-hash monitor, shared by the app (configuration) and
/// the execution thread ([`Self::after_block`]).
#[derive(Default)]
pub struct StateHashMonitor {
    /// The validator's ACCOUNT key (secp256k1; `--state-hash-attest-key`), the
    /// signer of its `AttestStateHash` actions. `None`: never submits.
    attest_key: OnceLock<k256::ecdsa::SigningKey>,
    inner: Mutex<Inner>,
}

impl StateHashMonitor {
    pub fn new() -> Self {
        Self::default()
    }

    /// Configure the attestation signer; returns its address. First call wins.
    pub fn set_attest_key(&self, key: k256::ecdsa::SigningKey) -> Address {
        let addr = account_address(&key);
        let _ = self.attest_key.set(key);
        addr
    }

    /// After block `block` executed: submit attestations for durable
    /// checkpoints this validator has not voted on yet.
    pub fn after_block(
        &self,
        _block: &TorusBlock,
        state_db: &StateDb,
        mempool: Option<&Mempool>,
        metrics: Option<&torus_telemetry::Metrics>,
    ) {
        let Some((hashed, _)) = read_running_hash(state_db) else {
            return;
        };
        if let Some(m) = metrics {
            m.state_hash_height.set(hashed as i64);
        }
        if let (Some(key), Some(mempool)) = (self.attest_key.get(), mempool) {
            self.submit_attestations(key, hashed, state_db, mempool, metrics);
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
