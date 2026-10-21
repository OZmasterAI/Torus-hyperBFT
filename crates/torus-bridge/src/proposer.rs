//! Block proposal — construct a `TorusBlock` from pending EVM transactions.

use alloy_primitives::{Address, Bloom, B256};

use torus_evm::{calc_next_block_base_fee, BlockEnvCfg, BlockExecResult, EvmExecutor};
use torus_state::StateDb;
use torus_types::{Receipt, SignedNativeAction, TorusBlock, TorusBlockHeader};

use crate::decode::decode_all_txs;
use crate::error::BridgeError;
use crate::state_root::compute_post_bundle_state_root;

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
    /// P0.0: the schedule the header `features` bits come from.
    activations: torus_types::ActivationSchedule,
}

impl BlockProposer {
    pub fn new(
        chain_id: u64,
        epoch_length: u64,
        max_validators: u32,
        treasury_address: Address,
        dev_pool_address: Address,
    ) -> Self {
        Self {
            chain_id,
            epoch_length,
            max_validators,
            treasury_address,
            dev_pool_address,
            metrics: None,
            activations: torus_types::ActivationSchedule::default(),
        }
    }

    /// P0.0: build headers with the feature bits of `activations` (default: none).
    pub fn with_activations(mut self, activations: torus_types::ActivationSchedule) -> Self {
        self.activations = activations;
        self
    }

    /// Build a block from the given EVM transactions.
    ///
    /// Decodes each RLP-encoded transaction, executes the block against the
    /// current state, computes the post-execution state root, and fills in
    /// all header fields. Does NOT commit the EVM bundle.
    ///
    /// F1 (s515): writer-precompile side effects (CoreWriter / lockbox queue rows)
    /// stay pending in `exec_result.native_writes` — nothing is written to `state_db`,
    /// so building a block that never commits leaves no queued action behind. Only
    /// tests call this today (production executes blocks once, post-commit, in
    /// `torus-consensus` app.rs).
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
        // Item 7 step 1: the parent's limit (the genesis parent carries the genesis value).
        let gas_limit = parent.evm_gas_limit;

        // 2. Decode RLP transactions.
        let decoded_txs = decode_all_txs(&evm_transactions)?;
        let tx_envs: Vec<_> = decoded_txs.iter().map(|d| d.tx_env.clone()).collect();

        // 3. Build block environment. P0.0 phase 2: at the bits this header carries.
        let features = self.activations.active_at(block_height);
        let block_cfg = BlockEnvCfg {
            number: block_height,
            timestamp,
            beneficiary: proposer,
            gas_limit,
            base_fee: next_base_fee,
            features,
        };

        // 4. Execute EVM transactions (skip invalid — don't abort block for one bad tx).
        let mut exec_result = evm_executor.execute_block(state_db, &block_cfg, tx_envs, true)?;

        // 5. Filter to only include successfully executed txs.
        let included = &exec_result.included_indices;
        let evm_transactions: Vec<Vec<u8>> = included
            .iter()
            .map(|&i| evm_transactions[i].clone())
            .collect();
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
        let root_timer = std::time::Instant::now();
        let state_root = compute_post_bundle_state_root(state_db, &exec_result.bundle)?;
        if let Some(ref m) = self.metrics {
            m.state_root_compute_seconds
                .observe(root_timer.elapsed().as_secs_f64());
        }

        // 8. Compute receipts root (deterministic hash of serialised receipts).
        let receipts_root = compute_receipts_root(&exec_result.receipts)
            .map_err(|e| BridgeError::Serialization(format!("receipts: {e}")))?;

        // 9. Assemble block with only the included txs.
        // Ancestry commitment: bind this block to the parent it was built on.
        let parent_hash = alloy_primitives::keccak256(parent.canonical_header_bytes());
        let block = TorusBlock {
            header: TorusBlockHeader {
                height: block_height,
                parent_hash,
                timestamp,
                proposer,
                state_root,
                receipts_root,
                logs_bloom: exec_result.logs_bloom,
                evm_gas_used: exec_result.gas_used,
                evm_fee_revenue: compute_fee_revenue(&exec_result.receipts, next_base_fee),
                evm_gas_limit: gas_limit,
                evm_block_interval: parent.evm_block_interval,
                // Item 7 step 2: the big lane's values are the parent's here (the
                // boundary rule and F3 set them in torus-consensus).
                evm_big_block_interval: parent.evm_big_block_interval,
                evm_big_gas_limit: parent.evm_big_gas_limit,
                native_action_count: 0,
                evm_tx_count: evm_transactions.len() as u32,
                base_fee_per_gas: next_base_fee,
                big_base_fee_per_gas: parent.big_base_fee_per_gas,
                epoch: torus_economics::EpochManager::epoch_for_block(
                    block_height,
                    self.epoch_length,
                ),
                validator_set_hash: parent.validator_set_hash,
                // P0.0: the bits validators require at this height.
                features,
                sig_attestation: [0u8; 64],
            },
            native_actions: vec![],
            evm_transactions,
            core_writer_actions: vec![],
        };

        Ok(ProposedBlock { block, exec_result })
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

/// Genesis-compatible parent header (height 0). Item 7 step 1: the EVM values
/// come from the genesis (`evm.gas_limit`, `evm.block_interval`); `TorusApp::try_new`
/// builds it once from the chain config, and every caller uses that header.
pub fn genesis_parent_header(evm_gas_limit: u64, evm_block_interval: u64) -> TorusBlockHeader {
    TorusBlockHeader {
        height: 0,
        // Genesis is the ancestry root: no parent, so the parent commitment is
        // zero. The first real block (height 1) commits to
        // keccak256(genesis_parent_header(..).canonical_header_bytes()).
        parent_hash: B256::ZERO,
        timestamp: 0,
        proposer: Address::ZERO,
        state_root: B256::ZERO,
        receipts_root: B256::ZERO,
        logs_bloom: Bloom::ZERO,
        evm_gas_used: 0,
        evm_fee_revenue: 0,
        evm_gas_limit,
        evm_block_interval,
        // Item 7 step 2: big blocks off and the default big limit; a chain config's
        // values come in through `genesis_parent_header_for`.
        evm_big_block_interval: torus_types::DEFAULT_EVM_BIG_BLOCK_INTERVAL,
        evm_big_gas_limit: torus_types::DEFAULT_EVM_BIG_GAS_LIMIT,
        native_action_count: 0,
        evm_tx_count: 0,
        base_fee_per_gas: 1_000_000_000, // 1 gwei initial base fee
        big_base_fee_per_gas: torus_types::EVM_MIN_BASE_FEE, // 1 gwei, like the small lane
        epoch: 0,
        validator_set_hash: B256::ZERO,
        features: 0,
        sig_attestation: [0u8; 64],
    }
}

/// Item 7 step 2: the genesis parent header of a chain config (small limit, N, M and the big limit
/// from genesis; both base fees 1 gwei). One value for every user, or block 1 fails to link.
pub fn genesis_parent_header_for(config: &torus_types::ChainConfig) -> TorusBlockHeader {
    let mut header = genesis_parent_header(config.evm_gas_limit, config.evm_block_interval);
    header.evm_big_block_interval = config.evm_big_block_interval;
    header.evm_big_gas_limit = config.evm_big_gas_limit;
    // P0.0: block 1's bits (features at 0 or 1), so another genesis set fails
    // block 1's parent link.
    header.features = config.activations.active_at(0);
    header
}

// ============================================================================
// Signature Attestation
// ============================================================================

fn attestation_digest(actions: &[SignedNativeAction]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    let mut encoded = Vec::new();
    for action in actions {
        encoded.clear();
        // Keep one call-local buffer instead of sizing and allocating a fresh
        // body for each action. A serialization error contributes no bytes,
        // exactly like the previous serialize(...).unwrap_or_default().
        if bincode::serialize_into(&mut encoded, action).is_ok() {
            hasher.update(&encoded);
        }
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

/// EVM gas revenue handed to the native fee distributor (`distribute_fees`):
/// the BASE-FEE part only, `sum(gas_used * min(effective_gas_price, base_fee))`.
/// revm already paid the tip (`effective_gas_price - base_fee`) to the block
/// beneficiary (the proposer) and removed the base-fee part from supply, so the
/// distributor redistributes exactly what revm did not credit to anyone; gas is
/// credited once (HL-parity report: "EVM gas fees credited twice").
pub fn compute_fee_revenue(receipts: &[Receipt], base_fee: u64) -> u128 {
    receipts
        .iter()
        .map(|r| r.gas_used as u128 * r.effective_gas_price.min(base_fee) as u128)
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use torus_types::{ActionSignature, NativeAction, Signature};

    fn receipt(gas_used: u64, effective_gas_price: u64) -> Receipt {
        Receipt {
            tx_hash: Default::default(),
            block_number: 1,
            block_hash: Default::default(),
            tx_index: 0,
            cumulative_gas_used: gas_used,
            gas_used,
            contract_address: None,
            logs: vec![],
            logs_bloom: Default::default(),
            status: true,
            effective_gas_price,
        }
    }

    /// Review nit (EVM gas credited once): the distributor gets the base-fee
    /// part only, for every tx kind. A legacy tx's effective price is its whole
    /// gas_price (revm pays gas_price - base_fee to the proposer), an EIP-1559
    /// tx's is base + tip; either way only gas_used * base_fee is redistributed.
    /// With base_fee 0 everything was a tip paid by revm: revenue 0.
    #[test]
    fn fee_revenue_is_the_base_fee_part_for_legacy_and_zero_base_fee() {
        let base = 1_000_000_000u64;
        // legacy at 3 gwei, EIP-1559 at base + 2 gwei, one exactly at base
        let receipts = [receipt(21_000, 3 * base), receipt(50_000, base + 2 * base), receipt(30_000, base)];
        assert_eq!(compute_fee_revenue(&receipts, base), 101_000u128 * base as u128);
        assert_eq!(compute_fee_revenue(&receipts, 0), 0, "base fee 0: all of it was tip");
        assert_eq!(compute_fee_revenue(&[], base), 0);
    }

    /// Item 7 step 1: the genesis parent header carries the genesis EVM
    /// values (`evm.gas_limit`, `evm.block_interval`), not a hardcoded 30M.
    #[test]
    fn genesis_parent_header_carries_the_genesis_evm_values() {
        let genesis = genesis_parent_header(3_000_000, 5);
        assert_eq!(genesis.height, 0);
        assert_eq!(genesis.parent_hash, B256::ZERO);
        assert_eq!(genesis.evm_gas_limit, 3_000_000);
        assert_eq!(genesis.evm_block_interval, 5);
        assert_eq!(genesis.base_fee_per_gas, 1_000_000_000);
    }

    /// Item 7 step 2: the genesis parent header of a chain config carries the
    /// config's small limit, N, M and big limit, and 1 gwei for both base fees.
    #[test]
    fn genesis_parent_header_for_takes_the_big_lane_from_the_config() {
        let json = r#"{
            "chain_id": 1,
            "chain_name": "t",
            "evm_gas_limit": 3000000,
            "evm_block_interval": 7,
            "evm_big_block_interval": 190,
            "evm_big_gas_limit": 15000000,
            "base_fee_per_gas": 1,
            "epoch_length": 1,
            "max_validators": 1,
            "min_stake": "0x0",
            "fee_burn_bps": 0,
            "fee_validator_bps": 0,
            "fee_treasury_bps": 0,
            "fee_dev_pool_bps": 0
        }"#;
        let config: torus_types::ChainConfig = serde_json::from_str(json).unwrap();
        let h = genesis_parent_header_for(&config);
        assert_eq!((h.height, h.parent_hash), (0, B256::ZERO));
        assert_eq!((h.evm_gas_limit, h.evm_block_interval), (3_000_000, 7));
        assert_eq!(
            (h.evm_big_block_interval, h.evm_big_gas_limit),
            (190, 15_000_000)
        );
        assert_eq!(
            (h.base_fee_per_gas, h.big_base_fee_per_gas),
            (1_000_000_000, 1_000_000_000)
        );
        // The two-argument form: big blocks off, the default big limit.
        let plain = genesis_parent_header(3_000_000, 7);
        assert_eq!(
            (plain.evm_big_block_interval, plain.evm_big_gas_limit),
            (0, 10_000_000)
        );
        assert_eq!(plain.big_base_fee_per_gas, 1_000_000_000);
    }

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
    fn attestation_reused_serialization_matches_legacy_digest_and_signature() {
        use ed25519_dalek::Signer;
        use sha2::{Digest, Sha256};
        let key = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
        let small = make_test_signed_action(1);
        let mut large = make_test_signed_action(2);
        large.action = NativeAction::PlaceOrderBatch(vec![
            torus_types::PlaceOrderParams {
                market_id: 3,
                is_buy: false,
                price: torus_types::FixedPoint::from_raw(123_000_000),
                quantity: torus_types::FixedPoint::from_raw(100_000_000),
                order_type: torus_types::OrderType::StopLimit {
                    trigger: torus_types::FixedPoint::from_raw(110_000_000),
                    limit: torus_types::FixedPoint::from_raw(109_000_000),
                },
                time_in_force: torus_types::TimeInForce::GTC,
                reduce_only: true,
                client_order_id: Some(42),
            };
            400
        ]);
        let mut session = large.clone();
        session.signature = ActionSignature::Session {
            session_pubkey: [8; 32],
            sig: torus_types::Ed25519Sig([9; 64]),
        };
        for actions in [
            vec![],
            vec![small.clone()],
            vec![
                small.clone(),
                large.clone(),
                small.clone(),
                session.clone(),
                small.clone(),
            ],
            vec![session, large, small],
        ] {
            let mut legacy = Sha256::new();
            for action in &actions {
                legacy.update(bincode::serialize(action).unwrap_or_default());
            }
            let legacy: [u8; 32] = legacy.finalize().into();
            assert_eq!(attestation_digest(&actions), legacy);
            let expected = if actions.is_empty() {
                [0; 64]
            } else {
                key.sign(&legacy).to_bytes()
            };
            assert_eq!(generate_sig_attestation(&actions, &key), expected);
            assert!(verify_sig_attestation(
                &actions,
                &expected,
                &key.verifying_key()
            ));
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
        assert!(!verify_sig_attestation(
            &actions,
            &att,
            &other.verifying_key()
        ));
    }

    #[test]
    fn empty_actions_zero_attestation() {
        let key = ed25519_dalek::SigningKey::from_bytes(&[7u8; 32]);
        let att = generate_sig_attestation(&[], &key);
        assert_eq!(att, [0u8; 64]);
        assert!(verify_sig_attestation(
            &[],
            &[0u8; 64],
            &key.verifying_key()
        ));
    }

    #[test]
    fn empty_actions_nonzero_attestation_fails() {
        let key = ed25519_dalek::SigningKey::from_bytes(&[7u8; 32]);
        assert!(!verify_sig_attestation(
            &[],
            &[1u8; 64],
            &key.verifying_key()
        ));
    }
}
