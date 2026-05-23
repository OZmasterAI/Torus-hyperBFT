//! Block proposal — construct a `TorusBlock` from pending EVM transactions.

use alloy_primitives::{Address, Bloom, B256};

use torus_evm::{
    calc_next_block_base_fee, BlockEnvCfg, BlockExecResult, EvmExecutor, DEFAULT_BLOCK_GAS_LIMIT,
};
use torus_state::StateDb;
use torus_types::{Receipt, SignedNativeAction, TorusBlock, TorusBlockHeader};

use crate::decode::{decode_all_txs, DecodedTx};
use crate::error::BridgeError;
use crate::state_root::{compute_full_composite_root, compute_native_state_root, compute_post_bundle_state_root};

/// Result of a block proposal.
pub struct ProposedBlock {
    /// The complete block with header fields filled in (state root, gas, bloom, etc.).
    pub block: TorusBlock,
    /// EVM execution result: receipts (with tx hashes set), bundle state, gas used, bloom.
    pub exec_result: BlockExecResult,
}

/// Constructs valid `TorusBlock`s ready for consensus.
pub struct BlockProposer {
    #[allow(dead_code)]
    chain_id: u64,
    epoch_length: u64,
    #[allow(dead_code)]
    max_validators: u32,
    #[allow(dead_code)]
    treasury_address: Address,
    #[allow(dead_code)]
    dev_pool_address: Address,
    pub metrics: Option<std::sync::Arc<torus_telemetry::Metrics>>,
}

impl BlockProposer {
    pub fn new(
        chain_id: u64,
        epoch_length: u64,
        max_validators: u32,
        treasury_address: Address,
        dev_pool_address: Address,
    ) -> Self {
        Self { chain_id, epoch_length, max_validators, treasury_address, dev_pool_address, metrics: None }
    }

    /// Build a block from the given EVM transactions.
    ///
    /// Decodes each RLP-encoded transaction, executes the block against the
    /// current state, computes the post-execution state root, and fills in
    /// all header fields. Does NOT modify the database.
    pub fn build_block(
        &self,
        state_db: &StateDb,
        evm_executor: &EvmExecutor,
        parent: &TorusBlockHeader,
        evm_transactions: Vec<Vec<u8>>,
        timestamp: u64,
        proposer: Address,
    ) -> Result<ProposedBlock, BridgeError> {
        // 1. Calculate next base fee from parent.
        let next_base_fee = calc_next_block_base_fee(
            parent.evm_gas_used,
            parent.evm_gas_limit,
            parent.base_fee_per_gas,
        );

        let block_height = parent.height + 1;
        let gas_limit = if parent.evm_gas_limit == 0 {
            DEFAULT_BLOCK_GAS_LIMIT
        } else {
            parent.evm_gas_limit
        };

        // 2. Decode RLP transactions.
        let decoded_txs = decode_all_txs(&evm_transactions)?;
        let tx_envs: Vec<_> = decoded_txs.iter().map(|d| d.tx_env.clone()).collect();

        // 3. Build block environment.
        let block_cfg = BlockEnvCfg {
            number: block_height,
            timestamp,
            beneficiary: proposer,
            gas_limit,
            base_fee: next_base_fee,
        };

        // 4. Execute EVM transactions (skip invalid — don't abort block for one bad tx).
        let mut exec_result = evm_executor.execute_block(state_db, &block_cfg, tx_envs, true)?;

        // 5. Filter to only include successfully executed txs.
        let included = &exec_result.included_indices;
        let evm_transactions: Vec<Vec<u8>> = included.iter().map(|&i| evm_transactions[i].clone()).collect();
        let included_decoded: Vec<_> = included.iter().map(|&i| &decoded_txs[i]).collect();
        if included.len() < decoded_txs.len() {
            tracing::warn!(
                total = decoded_txs.len(),
                included = included.len(),
                skipped = decoded_txs.len() - included.len(),
                "skipped invalid txs during block proposal"
            );
        }

        // 6. Set tx hashes and block info on receipts.
        for (i, receipt) in exec_result.receipts.iter_mut().enumerate() {
            if let Some(dtx) = included_decoded.get(i) {
                receipt.tx_hash = dtx.tx_hash;
            }
            receipt.block_number = block_height;
        }

        // 7. Compute state root without modifying DB.
        let state_root = compute_post_bundle_state_root(state_db, &exec_result.bundle)?;

        // 8. Compute receipts root (deterministic hash of serialised receipts).
        let receipts_root = compute_receipts_root(&exec_result.receipts)
            .map_err(|e| BridgeError::Serialization(format!("receipts: {e}")))?;

        // 9. Assemble block with only the included txs.
        let block = TorusBlock {
            header: TorusBlockHeader {
                height: block_height,
                timestamp,
                proposer,
                state_root,
                receipts_root,
                logs_bloom: exec_result.logs_bloom,
                evm_gas_used: exec_result.gas_used,
                evm_fee_revenue: compute_fee_revenue(&exec_result.receipts),
                evm_gas_limit: gas_limit,
                native_action_count: 0,
                evm_tx_count: evm_transactions.len() as u32,
                base_fee_per_gas: next_base_fee,
                epoch: torus_economics::EpochManager::epoch_for_block(block_height, self.epoch_length),
                validator_set_hash: parent.validator_set_hash,
                sig_attestation: [0u8; 64],
            },
            native_actions: vec![],
            evm_transactions,
            core_writer_actions: vec![],
        };

        Ok(ProposedBlock { block, exec_result })
    }

    /// Build a block containing both native actions and EVM transactions.
    ///
    /// Native actions are sorted into deterministic execution order per tech-req:
    ///   1. Cancellations (pre-EVM)
    ///   2. Non-GTC orders (pre-EVM)
    ///   3. EVM transactions
    ///   4. GTC limit orders (post-EVM)
    ///   5-8: CoreWriter drain, governance, fee distribution, epoch boundary
    ///
    /// Senders are recovered from EIP-712 signatures on SignedNativeActions.
    pub fn build_block_with_native(
        &self,
        state_db: &StateDb,
        evm_executor: &EvmExecutor,
        parent: &TorusBlockHeader,
        signed_native_actions: Vec<SignedNativeAction>,
        evm_transactions: Vec<Vec<u8>>,
        timestamp: u64,
        proposer: Address,
    ) -> Result<ProposedBlock, BridgeError> {
        let next_base_fee = calc_next_block_base_fee(
            parent.evm_gas_used,
            parent.evm_gas_limit,
            parent.base_fee_per_gas,
        );

        let block_height = parent.height + 1;
        let gas_limit = if parent.evm_gas_limit == 0 {
            DEFAULT_BLOCK_GAS_LIMIT
        } else {
            parent.evm_gas_limit
        };

        // FIX CONS-PF-02: Recover senders from EIP-712 signatures.
        // SignedNativeActions are kept intact in the block so validators can
        // independently verify signatures during consensus validation.
        // FIX ECON-FIND-03: Check persistent nonces to prevent replay.
        for signed in &signed_native_actions {
            let sender = signed
                .resolve_sender(timestamp, |pubkey| {
                    state_db.get_session(pubkey).ok().flatten()
                })
                .map_err(|e| BridgeError::SignatureRecovery(format!("{e}")))?;
            let mut nonce_key = [0u8; 28];
            nonce_key[..20].copy_from_slice(sender.as_slice());
            nonce_key[20..28].copy_from_slice(&signed.nonce.to_be_bytes());
            if state_db
                .get_cf_raw(torus_state::cf::CF_NATIVE_NONCES, &nonce_key)
                .unwrap_or(None)
                .is_some()
            {
                tracing::warn!(%sender, nonce = signed.nonce, "skipping replayed native action");
            }
        }

        // Execute EVM transactions.
        let decoded_txs = decode_all_txs(&evm_transactions)?;
        let tx_envs: Vec<_> = decoded_txs.iter().map(|d| d.tx_env.clone()).collect();

        let block_cfg = BlockEnvCfg {
            number: block_height,
            timestamp,
            beneficiary: proposer,
            gas_limit,
            base_fee: next_base_fee,
        };

        let mut exec_result = evm_executor.execute_block(state_db, &block_cfg, tx_envs, true)?;

        let included = &exec_result.included_indices;
        let evm_transactions: Vec<Vec<u8>> = included.iter().map(|&i| evm_transactions[i].clone()).collect();
        let included_decoded: Vec<_> = included.iter().map(|&i| &decoded_txs[i]).collect();
        if included.len() < decoded_txs.len() {
            tracing::warn!(
                total = decoded_txs.len(),
                included = included.len(),
                skipped = decoded_txs.len() - included.len(),
                "skipped invalid txs during block proposal"
            );
        }
        for (i, receipt) in exec_result.receipts.iter_mut().enumerate() {
            if let Some(dtx) = included_decoded.get(i) {
                receipt.tx_hash = dtx.tx_hash;
            }
            receipt.block_number = block_height;
        }

        // Compute composite state root (lagged native root — reads unmodified DB).
        let native_root = compute_native_state_root(state_db)?;
        let state_root = compute_full_composite_root(state_db, &exec_result.bundle, native_root)?;
        let receipts_root = compute_receipts_root(&exec_result.receipts)
            .map_err(|e| BridgeError::Serialization(format!("receipts: {e}")))?;

        let block = TorusBlock {
            header: TorusBlockHeader {
                height: block_height,
                timestamp,
                proposer,
                state_root,
                receipts_root,
                logs_bloom: exec_result.logs_bloom,
                evm_gas_used: exec_result.gas_used,
                evm_fee_revenue: compute_fee_revenue(&exec_result.receipts),
                evm_gas_limit: gas_limit,
                native_action_count: signed_native_actions.len() as u32,
                evm_tx_count: evm_transactions.len() as u32,
                base_fee_per_gas: next_base_fee,
                epoch: torus_economics::EpochManager::epoch_for_block(block_height, self.epoch_length),
                validator_set_hash: parent.validator_set_hash,
                sig_attestation: [0u8; 64],
            },
            native_actions: signed_native_actions,
            evm_transactions,
            core_writer_actions: vec![],
        };

        Ok(ProposedBlock { block, exec_result })
    }
}

/// Set tx_hash and block_number on each receipt.
fn set_receipt_metadata(
    exec_result: &mut BlockExecResult,
    decoded_txs: &[DecodedTx],
    block_height: u64,
) {
    for (i, receipt) in exec_result.receipts.iter_mut().enumerate() {
        if let Some(dtx) = decoded_txs.get(i) {
            receipt.tx_hash = dtx.tx_hash;
        }
        receipt.block_number = block_height;
    }
}

/// Deterministic receipts root: keccak256 of serde-serialised receipts.
///
/// FIX EVM-FIND-16: Returns `Result` instead of panicking on serialization failure.
pub(crate) fn compute_receipts_root(
    receipts: &[torus_types::Receipt],
) -> Result<B256, serde_json::Error> {
    if receipts.is_empty() {
        return Ok(B256::ZERO);
    }
    let data = serde_json::to_vec(receipts)?;
    Ok(alloy_primitives::keccak256(&data))
}

/// Genesis-compatible default parent header (height 0).
pub fn genesis_parent_header() -> TorusBlockHeader {
    TorusBlockHeader {
        height: 0,
        timestamp: 0,
        proposer: Address::ZERO,
        state_root: B256::ZERO,
        receipts_root: B256::ZERO,
        logs_bloom: Bloom::ZERO,
        evm_gas_used: 0,
        evm_fee_revenue: 0,
        evm_gas_limit: DEFAULT_BLOCK_GAS_LIMIT,
        native_action_count: 0,
        evm_tx_count: 0,
        base_fee_per_gas: 1_000_000_000, // 1 gwei initial base fee
        epoch: 0,
        validator_set_hash: B256::ZERO,
        sig_attestation: [0u8; 64],
    }
}

// ============================================================================
// Signature Attestation
// ============================================================================

fn attestation_digest(actions: &[SignedNativeAction]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    for action in actions {
        hasher.update(&bincode::serialize(action).unwrap_or_default());
    }
    hasher.finalize().into()
}

pub fn generate_sig_attestation(
    native_actions: &[SignedNativeAction],
    proposer_key: &ed25519_dalek::SigningKey,
) -> [u8; 64] {
    if native_actions.is_empty() {
        return [0u8; 64];
    }
    use ed25519_dalek::Signer;
    let digest = attestation_digest(native_actions);
    proposer_key.sign(&digest).to_bytes()
}

pub fn verify_sig_attestation(
    native_actions: &[SignedNativeAction],
    attestation: &[u8; 64],
    proposer_pubkey: &ed25519_dalek::VerifyingKey,
) -> bool {
    if native_actions.is_empty() {
        return *attestation == [0u8; 64];
    }
    let digest = attestation_digest(native_actions);
    let sig = ed25519_dalek::Signature::from_bytes(attestation);
    use ed25519_dalek::Verifier;
    proposer_pubkey.verify(&digest, &sig).is_ok()
}

pub fn compute_fee_revenue(receipts: &[Receipt]) -> u128 {
    receipts
        .iter()
        .map(|r| r.gas_used as u128 * r.effective_gas_price as u128)
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use torus_types::{ActionSignature, NativeAction, Signature};

    fn make_test_signed_action(nonce: u64) -> SignedNativeAction {
        SignedNativeAction {
            action: NativeAction::ClaimRewards,
            nonce,
            signature: ActionSignature::Eip712(Signature {
                v: 27,
                r: [0u8; 32],
                s: [0u8; 32],
            }),
        }
    }

    #[test]
    fn attestation_roundtrip() {
        let key = ed25519_dalek::SigningKey::from_bytes(&[7u8; 32]);
        let actions = vec![make_test_signed_action(1), make_test_signed_action(2)];
        let att = generate_sig_attestation(&actions, &key);
        assert_ne!(att, [0u8; 64]);
        assert!(verify_sig_attestation(&actions, &att, &key.verifying_key()));
    }

    #[test]
    fn attestation_tampered_actions_fails() {
        let key = ed25519_dalek::SigningKey::from_bytes(&[7u8; 32]);
        let actions = vec![make_test_signed_action(1), make_test_signed_action(2)];
        let att = generate_sig_attestation(&actions, &key);

        let mut bad = actions.clone();
        bad.push(make_test_signed_action(3));
        assert!(!verify_sig_attestation(&bad, &att, &key.verifying_key()));
    }

    #[test]
    fn attestation_wrong_key_fails() {
        let key = ed25519_dalek::SigningKey::from_bytes(&[7u8; 32]);
        let other = ed25519_dalek::SigningKey::from_bytes(&[8u8; 32]);
        let actions = vec![make_test_signed_action(1)];
        let att = generate_sig_attestation(&actions, &key);
        assert!(!verify_sig_attestation(&actions, &att, &other.verifying_key()));
    }

    #[test]
    fn empty_actions_zero_attestation() {
        let key = ed25519_dalek::SigningKey::from_bytes(&[7u8; 32]);
        let att = generate_sig_attestation(&[], &key);
        assert_eq!(att, [0u8; 64]);
        assert!(verify_sig_attestation(&[], &[0u8; 64], &key.verifying_key()));
    }

    #[test]
    fn empty_actions_nonzero_attestation_fails() {
        let key = ed25519_dalek::SigningKey::from_bytes(&[7u8; 32]);
        assert!(!verify_sig_attestation(&[], &[1u8; 64], &key.verifying_key()));
    }
}
