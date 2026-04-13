//! Block proposal — construct a `TorusBlock` from pending EVM transactions.

use alloy_primitives::{Address, Bloom, B256};

use torus_evm::{
    calc_next_block_base_fee, BlockEnvCfg, BlockExecResult, EvmExecutor, DEFAULT_BLOCK_GAS_LIMIT,
};
use torus_state::StateDb;
use torus_types::{TorusBlock, TorusBlockHeader};

use crate::decode::{decode_all_txs, DecodedTx};
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
    #[allow(dead_code)] // used in Phase 2 for native execution
    chain_id: u64,
}

impl BlockProposer {
    pub fn new(chain_id: u64) -> Self {
        Self { chain_id }
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
        let receipts_root = compute_receipts_root(&exec_result.receipts);

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
                evm_gas_limit: gas_limit,
                native_action_count: 0,
                evm_tx_count: evm_transactions.len() as u32,
                base_fee_per_gas: next_base_fee,
                epoch: parent.epoch,
                validator_set_hash: parent.validator_set_hash,
            },
            native_actions: vec![],
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
fn compute_receipts_root(receipts: &[torus_types::Receipt]) -> B256 {
    if receipts.is_empty() {
        return B256::ZERO;
    }
    let data = serde_json::to_vec(receipts).expect("serialize receipts");
    alloy_primitives::keccak256(&data)
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
        evm_gas_limit: DEFAULT_BLOCK_GAS_LIMIT,
        native_action_count: 0,
        evm_tx_count: 0,
        base_fee_per_gas: 1_000_000_000, // 1 gwei initial base fee
        epoch: 0,
        validator_set_hash: B256::ZERO,
    }
}
