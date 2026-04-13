//! Block validation — execute a proposed block and verify its state root.

use alloy_primitives::B256;

use revm::database::BundleState;

use torus_evm::{BlockEnvCfg, EvmExecutor};
use torus_state::StateDb;
use torus_types::{Receipt, TorusBlock};

use crate::decode::decode_all_txs;
use crate::error::BridgeError;
use crate::state_root::compute_post_bundle_state_root;

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
}
