//! Block validation — execute a proposed block and verify its state root.

use alloy_primitives::{Address, B256};

use revm::database::BundleState;

use torus_evm::{BlockEnvCfg, EvmExecutor};
use torus_state::StateDb;
use torus_types::{NativeAction, Receipt, TorusBlock};

use crate::decode::decode_all_txs;
use crate::error::BridgeError;
use crate::state_root::{
    compute_full_composite_root, compute_native_state_root, compute_post_bundle_state_root,
};

/// Successfully validated block — ready for commitment.
#[derive(Debug)]
pub struct ValidatedBlock {
    /// EVM receipts with tx hashes set.
    pub receipts: Vec<Receipt>,
    /// Accumulated state changes from EVM execution.
    pub bundle: BundleState,
    /// Computed state root (matches the block header).
    pub state_root: B256,
    /// Recovered (sender, action) pairs for post-commit native execution.
    pub native_sender_actions: Vec<(Address, NativeAction)>,
    /// Consumed nonces for post-commit persistence.
    pub native_consumed_nonces: Vec<(Address, u64)>,
}

/// Validates proposed blocks by re-executing EVM transactions and verifying the state root.
pub struct BlockValidator {
    #[allow(dead_code)]
    chain_id: u64,
    #[allow(dead_code)]
    epoch_length: u64,
    #[allow(dead_code)]
    max_validators: u32,
    #[allow(dead_code)]
    treasury_address: Address,
    #[allow(dead_code)]
    dev_pool_address: Address,
    pub metrics: Option<std::sync::Arc<torus_telemetry::Metrics>>,
}

impl BlockValidator {
    pub fn new(
        chain_id: u64,
        epoch_length: u64,
        max_validators: u32,
        treasury_address: Address,
        dev_pool_address: Address,
    ) -> Self {
        Self { chain_id, epoch_length, max_validators, treasury_address, dev_pool_address, metrics: None }
    }

    /// Validate a proposed block against the current state.
    ///
    /// FIX CONS-PF-14: If `parent_header` is provided, verify base_fee using EIP-1559
    /// recalculation from parent block data.
    pub fn validate_block(
        &self,
        block: &TorusBlock,
        state_db: &StateDb,
        evm_executor: &EvmExecutor,
    ) -> Result<ValidatedBlock, BridgeError> {
        self.validate_block_inner(block, state_db, evm_executor, None)
    }

    /// Validate with parent header for base_fee verification.
    pub fn validate_block_with_parent(
        &self,
        block: &TorusBlock,
        state_db: &StateDb,
        evm_executor: &EvmExecutor,
        parent_header: &torus_types::TorusBlockHeader,
    ) -> Result<ValidatedBlock, BridgeError> {
        self.validate_block_inner(block, state_db, evm_executor, Some(parent_header))
    }

    fn validate_block_inner(
        &self,
        block: &TorusBlock,
        state_db: &StateDb,
        evm_executor: &EvmExecutor,
        parent_header: Option<&torus_types::TorusBlockHeader>,
    ) -> Result<ValidatedBlock, BridgeError> {
        // FIX CONS-PF-14: Verify base_fee using EIP-1559 if parent is available.
        if let Some(parent) = parent_header {
            let expected_base_fee = torus_evm::calc_next_block_base_fee(
                parent.evm_gas_used,
                parent.evm_gas_limit,
                parent.base_fee_per_gas,
            );
            if block.header.base_fee_per_gas != expected_base_fee {
                return Err(BridgeError::InvalidBlock(format!(
                    "base_fee mismatch: header={}, expected={} (EIP-1559 from parent)",
                    block.header.base_fee_per_gas, expected_base_fee
                )));
            }
        }

        let decoded_txs = decode_all_txs(&block.evm_transactions)?;
        let tx_envs: Vec<_> = decoded_txs.iter().map(|d| d.tx_env.clone()).collect();

        let block_cfg = BlockEnvCfg {
            number: block.header.height,
            timestamp: block.header.timestamp,
            beneficiary: block.header.proposer,
            gas_limit: block.header.evm_gas_limit,
            base_fee: block.header.base_fee_per_gas,
        };

        let mut exec_result = evm_executor.execute_block(state_db, &block_cfg, tx_envs, false)?;

        for (i, receipt) in exec_result.receipts.iter_mut().enumerate() {
            if let Some(dtx) = decoded_txs.get(i) {
                receipt.tx_hash = dtx.tx_hash;
            }
            receipt.block_number = block.header.height;
        }

        if exec_result.gas_used != block.header.evm_gas_used {
            return Err(BridgeError::InvalidBlock(format!(
                "gas used mismatch: header={}, executed={}",
                block.header.evm_gas_used, exec_result.gas_used
            )));
        }

        let computed_fee_revenue = crate::proposer::compute_fee_revenue(&exec_result.receipts);
        if computed_fee_revenue != block.header.evm_fee_revenue {
            return Err(BridgeError::InvalidBlock(format!(
                "fee revenue mismatch: header={}, computed={}",
                block.header.evm_fee_revenue, computed_fee_revenue
            )));
        }

        // FIX CONS-PF-13: Verify receipts_root by recomputing from execution results.
        let computed_receipts_root = crate::proposer::compute_receipts_root(&exec_result.receipts)
            .map_err(|e| BridgeError::Serialization(format!("receipts: {e}")))?;
        if computed_receipts_root != block.header.receipts_root {
            return Err(BridgeError::InvalidBlock(format!(
                "receipts_root mismatch: header={}, computed={}",
                block.header.receipts_root, computed_receipts_root
            )));
        }

        // FIX CONS-PF-13: Verify logs_bloom matches execution results.
        if exec_result.logs_bloom != block.header.logs_bloom {
            return Err(BridgeError::InvalidBlock(format!(
                "logs_bloom mismatch: header={}, computed={}",
                block.header.logs_bloom, exec_result.logs_bloom
            )));
        }

        let computed_root = compute_post_bundle_state_root(state_db, &exec_result.bundle)?;

        if computed_root != block.header.state_root {
            return Err(BridgeError::StateRootMismatch {
                expected: block.header.state_root,
                computed: computed_root,
            });
        }

        Ok(ValidatedBlock {
            receipts: exec_result.receipts,
            bundle: exec_result.bundle,
            state_root: computed_root,
            native_sender_actions: vec![],
            native_consumed_nonces: vec![],
        })
    }

    /// Validate a block received during sync (same as validate_block in Phase 1).
    pub fn validate_block_for_sync(
        &self,
        block: &TorusBlock,
        state_db: &StateDb,
        evm_executor: &EvmExecutor,
    ) -> Result<ValidatedBlock, BridgeError> {
        self.validate_block(block, state_db, evm_executor)
    }

    /// Validate a block with the full native + EVM execution pipeline.
    ///
    /// FIX CONS-PF-02: Recovers senders from the EIP-712 signatures embedded
    /// in each `SignedNativeAction` in the block. This ensures validators
    /// independently verify authorization rather than trusting the proposer.
    ///
    /// Execution order per tech-req:
    ///   1. Native cancellations
    ///   2. Native non-GTC orders (IOC, FOK, market)
    ///   3. EVM transactions
    ///   4. Native GTC limit orders
    ///   5. CoreWriter queue drain (actions queued by EVM in previous block)
    ///   6. Lockbox operations
    ///   7. Oracle submissions + aggregation
    ///   8. Governance actions + process_pending_proposals
    ///   9. Liquidation checks
    ///   10. Fee distribution + epoch check
    pub fn validate_block_with_native(
        &self,
        block: &TorusBlock,
        state_db: &StateDb,
        evm_executor: &EvmExecutor,
    ) -> Result<ValidatedBlock, BridgeError> {
        // FIX CONS-PF-02: Recover senders from EIP-712 signatures in each
        // SignedNativeAction. If any signature is invalid, reject the block.
        // FIX ECON-FIND-03: Check persistent nonces to prevent replay.
        let mut sender_actions = Vec::with_capacity(block.native_actions.len());
        let mut consumed_nonces: Vec<(Address, u64)> = Vec::new();
        for (i, signed) in block.native_actions.iter().enumerate() {
            let sender = signed.recover_sender().map_err(|e| {
                BridgeError::InvalidBlock(format!(
                    "native action {i}: invalid EIP-712 signature: {e}"
                ))
            })?;
            // Replay check: reject blocks containing replayed nonces.
            let mut nonce_key = [0u8; 28];
            nonce_key[..20].copy_from_slice(sender.as_slice());
            nonce_key[20..28].copy_from_slice(&signed.nonce.to_be_bytes());
            if state_db
                .get_cf_raw(torus_state::cf::CF_NATIVE_NONCES, &nonce_key)
                .unwrap_or(None)
                .is_some()
            {
                return Err(BridgeError::InvalidBlock(format!(
                    "native action {i}: replayed nonce {} for sender {sender}",
                    signed.nonce,
                )));
            }
            consumed_nonces.push((sender, signed.nonce));
            sender_actions.push((sender, signed.action.clone()));
        }

        // Execute EVM transactions.
        let decoded_txs = decode_all_txs(&block.evm_transactions)?;
        let tx_envs: Vec<_> = decoded_txs.iter().map(|d| d.tx_env.clone()).collect();

        let block_cfg = BlockEnvCfg {
            number: block.header.height,
            timestamp: block.header.timestamp,
            beneficiary: block.header.proposer,
            gas_limit: block.header.evm_gas_limit,
            base_fee: block.header.base_fee_per_gas,
        };

        let mut exec_result = evm_executor.execute_block(state_db, &block_cfg, tx_envs, false)?;

        for (i, receipt) in exec_result.receipts.iter_mut().enumerate() {
            if let Some(dtx) = decoded_txs.get(i) {
                receipt.tx_hash = dtx.tx_hash;
            }
            receipt.block_number = block.header.height;
        }

        if exec_result.gas_used != block.header.evm_gas_used {
            return Err(BridgeError::InvalidBlock(format!(
                "gas used mismatch: header={}, executed={}",
                block.header.evm_gas_used, exec_result.gas_used
            )));
        }

        let computed_fee_revenue = crate::proposer::compute_fee_revenue(&exec_result.receipts);
        if computed_fee_revenue != block.header.evm_fee_revenue {
            return Err(BridgeError::InvalidBlock(format!(
                "fee revenue mismatch: header={}, computed={}",
                block.header.evm_fee_revenue, computed_fee_revenue
            )));
        }

        // Verify receipts_root by recomputing from execution results.
        let computed_receipts_root = crate::proposer::compute_receipts_root(&exec_result.receipts)
            .map_err(|e| BridgeError::Serialization(format!("receipts: {e}")))?;
        if computed_receipts_root != block.header.receipts_root {
            return Err(BridgeError::InvalidBlock(format!(
                "receipts_root mismatch: header={}, computed={}",
                block.header.receipts_root, computed_receipts_root
            )));
        }

        // Verify logs_bloom matches execution results.
        if exec_result.logs_bloom != block.header.logs_bloom {
            return Err(BridgeError::InvalidBlock(format!(
                "logs_bloom mismatch: header={}, computed={}",
                block.header.logs_bloom, exec_result.logs_bloom
            )));
        }

        // Compute composite state root (lagged native root — reads unmodified DB).
        let native_root = compute_native_state_root(state_db)?;
        let computed_root =
            compute_full_composite_root(state_db, &exec_result.bundle, native_root)?;

        if computed_root != block.header.state_root {
            return Err(BridgeError::StateRootMismatch {
                expected: block.header.state_root,
                computed: computed_root,
            });
        }

        Ok(ValidatedBlock {
            receipts: exec_result.receipts,
            bundle: exec_result.bundle,
            state_root: computed_root,
            native_sender_actions: sender_actions,
            native_consumed_nonces: consumed_nonces,
        })
    }
}
