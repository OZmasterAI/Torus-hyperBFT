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

        // 4. Execute EVM transactions.
        let mut exec_result = evm_executor.execute_block(state_db, &block_cfg, tx_envs)?;

        // 5. Set tx hashes and block info on receipts.
        set_receipt_metadata(&mut exec_result, &decoded_txs, block_height);

        // 6. Compute state root without modifying DB.
        let state_root = compute_post_bundle_state_root(state_db, &exec_result.bundle)?;

        // 7. Compute receipts root (deterministic hash of serialised receipts).
        let receipts_root = compute_receipts_root(&exec_result.receipts)
            .map_err(|e| BridgeError::Serialization(format!("receipts: {e}")))?;

        // 8. Assemble block.
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
                .recover_sender()
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

        let mut exec_result = evm_executor.execute_block(state_db, &block_cfg, tx_envs)?;
        set_receipt_metadata(&mut exec_result, &decoded_txs, block_height);

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
    }
}

pub fn compute_fee_revenue(receipts: &[Receipt]) -> u128 {
    receipts
        .iter()
        .map(|r| r.gas_used as u128 * r.effective_gas_price as u128)
        .sum()
}
