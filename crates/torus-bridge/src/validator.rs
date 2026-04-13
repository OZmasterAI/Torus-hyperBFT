//! Block validation — execute a proposed block and verify its state root.

use alloy_primitives::{Address, B256};

use revm::database::BundleState;

use torus_evm::{BlockEnvCfg, EvmExecutor};
use torus_state::StateDb;
use torus_types::{Receipt, TorusBlock};

use crate::decode::decode_all_txs;
use crate::error::BridgeError;
use crate::native_executor::{
    sort_native_actions, NativeExecContext, NativeExecutor,
};
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
}

/// Validates proposed blocks by re-executing EVM transactions and verifying the state root.
pub struct BlockValidator {
    #[allow(dead_code)] // used in Phase 2 for native execution
    chain_id: u64,
}

impl BlockValidator {
    pub fn new(chain_id: u64) -> Self {
        Self { chain_id }
    }

    /// Validate a proposed block against the current state.
    pub fn validate_block(
        &self,
        block: &TorusBlock,
        state_db: &StateDb,
        evm_executor: &EvmExecutor,
    ) -> Result<ValidatedBlock, BridgeError> {
        let decoded_txs = decode_all_txs(&block.evm_transactions)?;
        let tx_envs: Vec<_> = decoded_txs.iter().map(|d| d.tx_env.clone()).collect();

        let block_cfg = BlockEnvCfg {
            number: block.header.height,
            timestamp: block.header.timestamp,
            beneficiary: block.header.proposer,
            gas_limit: block.header.evm_gas_limit,
            base_fee: block.header.base_fee_per_gas,
        };

        let mut exec_result = evm_executor.execute_block(state_db, &block_cfg, tx_envs)?;

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
    ///
    /// `native_senders` provides the recovered sender address for each entry
    /// in `block.native_actions` (matched 1:1 by index).
    pub fn validate_block_with_native(
        &self,
        block: &TorusBlock,
        state_db: &StateDb,
        evm_executor: &EvmExecutor,
        native_senders: &[Address],
    ) -> Result<ValidatedBlock, BridgeError> {
        if native_senders.len() != block.native_actions.len() {
            return Err(BridgeError::InvalidBlock(format!(
                "native_senders length {} != native_actions length {}",
                native_senders.len(),
                block.native_actions.len()
            )));
        }

        // Build (sender, action) pairs.
        let sender_actions: Vec<(Address, _)> = native_senders
            .iter()
            .zip(block.native_actions.iter())
            .map(|(s, a)| (*s, a.clone()))
            .collect();

        // Sort into pre-EVM and post-EVM groups.
        let (pre_evm, post_evm) = sort_native_actions(&sender_actions);

        // Create native execution context.
        let mut ctx = NativeExecContext::new(
            state_db.clone(),
            block.header.height,
            block.header.timestamp,
            block.header.epoch,
            0, // epoch_length — set by node in production
            0, // max_validators
            block.header.proposer,
            Address::ZERO, // treasury
            Address::ZERO, // dev_pool
        );

        // Phase 1: Execute pre-EVM native actions (cancellations, non-GTC orders).
        NativeExecutor::execute_batch(&mut ctx, &pre_evm);

        // Phase 2: Execute EVM transactions.
        let decoded_txs = decode_all_txs(&block.evm_transactions)?;
        let tx_envs: Vec<_> = decoded_txs.iter().map(|d| d.tx_env.clone()).collect();

        let block_cfg = BlockEnvCfg {
            number: block.header.height,
            timestamp: block.header.timestamp,
            beneficiary: block.header.proposer,
            gas_limit: block.header.evm_gas_limit,
            base_fee: block.header.base_fee_per_gas,
        };

        let mut exec_result = evm_executor.execute_block(state_db, &block_cfg, tx_envs)?;

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

        // Phase 3: Execute post-EVM native actions (GTC orders, lockbox, oracle, governance).
        NativeExecutor::execute_batch(&mut ctx, &post_evm);

        // Phase 4: Drain and execute CoreWriter queue from previous block.
        NativeExecutor::drain_core_writer(&mut ctx);

        // Phase 5-7: Oracle aggregation, governance processing, liquidation checks
        // are driven by the block-level helpers. In production, market lists and
        // validator stakes come from the chain config / staking state.
        NativeExecutor::process_governance(&mut ctx);

        // Phase 8: Fee distribution.
        NativeExecutor::distribute_fees(&mut ctx, exec_result.gas_used);

        // Phase 9: Epoch boundary check.
        NativeExecutor::process_epoch_boundary(&mut ctx);

        // Compute composite state root (EVM bundle + native state).
        let native_root = compute_native_state_root(state_db);
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
        })
    }
}
