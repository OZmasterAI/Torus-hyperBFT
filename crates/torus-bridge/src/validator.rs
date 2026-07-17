//! Block validation — execute a proposed block and verify its state root.

use alloy_primitives::{Address, B256};

use revm::database::BundleState;

use torus_evm::{BlockEnvCfg, EvmExecutor};
use torus_state::{StateDb, StateOverlay};
use torus_types::{NativeAction, Receipt, TorusBlock};

use crate::decode::{decode_all_txs, decode_txs_lossy, DecodedTx};
use crate::error::BridgeError;
use crate::state_root::{
    compute_full_composite_root, compute_post_bundle_state_root, flagged_native_root,
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
        Self {
            chain_id,
            epoch_length,
            max_validators,
            treasury_address,
            dev_pool_address,
            metrics: None,
        }
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
        self.validate_block_inner(block, state_db, evm_executor, None, false)
    }

    /// Validate with parent header for base_fee verification.
    pub fn validate_block_with_parent(
        &self,
        block: &TorusBlock,
        state_db: &StateDb,
        evm_executor: &EvmExecutor,
        parent_header: &torus_types::TorusBlockHeader,
    ) -> Result<ValidatedBlock, BridgeError> {
        self.validate_block_inner(block, state_db, evm_executor, Some(parent_header), false)
    }

    /// Validate during catch-up replay — skips state root check since blocks
    /// are already consensus-committed.
    pub fn validate_block_for_catchup(
        &self,
        block: &TorusBlock,
        state_db: &StateDb,
        evm_executor: &EvmExecutor,
    ) -> Result<ValidatedBlock, BridgeError> {
        self.validate_block_inner(block, state_db, evm_executor, None, true)
    }

    fn validate_block_inner(
        &self,
        block: &TorusBlock,
        state_db: &StateDb,
        evm_executor: &EvmExecutor,
        parent_header: Option<&torus_types::TorusBlockHeader>,
        skip_state_root_check: bool,
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

        // D3 (S392): on the committed/catchup path an undecodable tx skips only
        // itself — never the whole block's EVM execution. Strict (vote-side)
        // validation still rejects such blocks outright.
        let decoded_txs: Vec<(usize, DecodedTx)> = if skip_state_root_check {
            decode_txs_lossy(&block.evm_transactions)
        } else {
            decode_all_txs(&block.evm_transactions)?
                .into_iter()
                .enumerate()
                .collect()
        };
        let tx_envs: Vec<_> = decoded_txs.iter().map(|(_, d)| d.tx_env.clone()).collect();

        let block_cfg = BlockEnvCfg {
            number: block.header.height,
            timestamp: block.header.timestamp,
            beneficiary: block.header.proposer,
            gas_limit: block.header.evm_gas_limit,
            base_fee: block.header.base_fee_per_gas,
        };

        let mut exec_result =
            evm_executor.execute_block(state_db, &block_cfg, tx_envs, skip_state_root_check)?;

        align_receipts(&mut exec_result, &decoded_txs, block.header.height);

        if !skip_state_root_check {
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

            let computed_receipts_root =
                crate::proposer::compute_receipts_root(&exec_result.receipts)
                    .map_err(|e| BridgeError::Serialization(format!("receipts: {e}")))?;
            if computed_receipts_root != block.header.receipts_root {
                return Err(BridgeError::InvalidBlock(format!(
                    "receipts_root mismatch: header={}, computed={}",
                    block.header.receipts_root, computed_receipts_root
                )));
            }

            if exec_result.logs_bloom != block.header.logs_bloom {
                return Err(BridgeError::InvalidBlock(format!(
                    "logs_bloom mismatch: header={}, computed={}",
                    block.header.logs_bloom, exec_result.logs_bloom
                )));
            }
        }

        let root_timer = std::time::Instant::now();
        let computed_root = compute_post_bundle_state_root(state_db, &exec_result.bundle)?;
        if let Some(ref m) = self.metrics {
            m.state_root_compute_seconds
                .observe(root_timer.elapsed().as_secs_f64());
        }

        if !skip_state_root_check && computed_root != block.header.state_root {
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

    /// Validate a block against a [`StateOverlay`] (deferred-commit mode).
    ///
    /// EVM execution reads from the overlay (pending parent bundles layered
    /// on state_db). State root is verified using `state_db + merged_bundle`
    /// where `merged_bundle` combines all pending ancestor changes with this
    /// block's execution result.
    pub fn validate_block_with_overlay(
        &self,
        block: &TorusBlock,
        overlay: &StateOverlay,
        state_db: &StateDb,
        merged_parent_bundle: &BundleState,
        evm_executor: &EvmExecutor,
    ) -> Result<ValidatedBlock, BridgeError> {
        let decoded_txs: Vec<(usize, DecodedTx)> = decode_all_txs(&block.evm_transactions)?
            .into_iter()
            .enumerate()
            .collect();
        let tx_envs: Vec<_> = decoded_txs.iter().map(|(_, d)| d.tx_env.clone()).collect();

        let block_cfg = BlockEnvCfg {
            number: block.header.height,
            timestamp: block.header.timestamp,
            beneficiary: block.header.proposer,
            gas_limit: block.header.evm_gas_limit,
            base_fee: block.header.base_fee_per_gas,
        };

        let mut exec_result =
            evm_executor.execute_block_with_overlay(overlay, &block_cfg, tx_envs, false)?;

        align_receipts(&mut exec_result, &decoded_txs, block.header.height);

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

        let computed_receipts_root = crate::proposer::compute_receipts_root(&exec_result.receipts)
            .map_err(|e| BridgeError::Serialization(format!("receipts: {e}")))?;
        if computed_receipts_root != block.header.receipts_root {
            return Err(BridgeError::InvalidBlock(format!(
                "receipts_root mismatch: header={}, computed={}",
                block.header.receipts_root, computed_receipts_root
            )));
        }

        if exec_result.logs_bloom != block.header.logs_bloom {
            return Err(BridgeError::InvalidBlock(format!(
                "logs_bloom mismatch: header={}, computed={}",
                block.header.logs_bloom, exec_result.logs_bloom
            )));
        }

        // State root: merge pending parent changes with this block's bundle
        // and verify against state_db (the last committed base).
        let mut verification_bundle = merged_parent_bundle.clone();
        merge_bundle_into(&mut verification_bundle, &exec_result.bundle);
        let root_timer = std::time::Instant::now();
        let computed_root = compute_post_bundle_state_root(state_db, &verification_bundle)?;
        if let Some(ref m) = self.metrics {
            m.state_root_compute_seconds
                .observe(root_timer.elapsed().as_secs_f64());
        }

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
        self.validate_block_with_native_inner(block, state_db, evm_executor, false)
    }

    /// Validate with native during catch-up replay — skips state root check
    /// since blocks are already consensus-committed.
    pub fn validate_block_with_native_for_catchup(
        &self,
        block: &TorusBlock,
        state_db: &StateDb,
        evm_executor: &EvmExecutor,
    ) -> Result<ValidatedBlock, BridgeError> {
        self.validate_block_with_native_inner(block, state_db, evm_executor, true)
    }

    fn validate_block_with_native_inner(
        &self,
        block: &TorusBlock,
        state_db: &StateDb,
        evm_executor: &EvmExecutor,
        skip_state_root_check: bool,
    ) -> Result<ValidatedBlock, BridgeError> {
        // FIX CONS-PF-02: Recover senders from EIP-712 signatures in each
        // SignedNativeAction. If any signature is invalid, reject the block.
        // FIX ECON-FIND-03: Check persistent nonces to prevent replay.
        let mut sender_actions = Vec::with_capacity(block.native_actions.len());
        let mut consumed_nonces: Vec<(Address, u64)> = Vec::new();
        for (i, signed) in block.native_actions.iter().enumerate() {
            let sender = signed
                // Seconds→ms: `header.timestamp` is SECONDS, session `expiry` is
                // MILLISECONDS. Convert so this legacy native validate/catch-up path
                // agrees on session expiry with the live exec/validate paths
                // (app.rs). This helper is STRICT (no exec grace window); the
                // authoritative live checks with the grace window live in app.rs.
                .resolve_sender(block.header.timestamp.saturating_mul(1000), |pubkey| {
                    state_db.get_session(pubkey).ok().flatten()
                })
                .map_err(|e| {
                    BridgeError::InvalidBlock(format!(
                        "native action {i}: signature verification failed: {e}"
                    ))
                })?;
            // Replay check: reject blocks containing replayed nonces.
            let nonce_key = torus_state::cf::native_nonce_key(&sender, signed.nonce);
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

        // Execute EVM transactions. D3 (S392): lossy decode on the catchup
        // path so one bad tx can't void the whole block's EVM effects.
        let decoded_txs: Vec<(usize, DecodedTx)> = if skip_state_root_check {
            decode_txs_lossy(&block.evm_transactions)
        } else {
            decode_all_txs(&block.evm_transactions)?
                .into_iter()
                .enumerate()
                .collect()
        };
        let tx_envs: Vec<_> = decoded_txs.iter().map(|(_, d)| d.tx_env.clone()).collect();

        let block_cfg = BlockEnvCfg {
            number: block.header.height,
            timestamp: block.header.timestamp,
            beneficiary: block.header.proposer,
            gas_limit: block.header.evm_gas_limit,
            base_fee: block.header.base_fee_per_gas,
        };

        let mut exec_result =
            evm_executor.execute_block(state_db, &block_cfg, tx_envs, skip_state_root_check)?;

        align_receipts(&mut exec_result, &decoded_txs, block.header.height);

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
        let root_timer = std::time::Instant::now();
        let native_root = flagged_native_root(state_db)?;
        let computed_root =
            compute_full_composite_root(state_db, &exec_result.bundle, native_root)?;
        if let Some(ref m) = self.metrics {
            m.state_root_compute_seconds
                .observe(root_timer.elapsed().as_secs_f64());
        }

        if !skip_state_root_check && computed_root != block.header.state_root {
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

/// D5 (S392): map executor receipts back to the txs that produced them.
///
/// `receipts[j]` came from input env `included_indices[j]`, and
/// `decoded[included_indices[j]].0` is that tx's ORIGINAL index in
/// `block.evm_transactions`. Receipts carry the original body index in
/// `tx_index` so CF_RECEIPTS, CF_TX_HASH_TO_LOCATION and body lookups stay
/// aligned when a tx is skipped mid-block; skipped txs get no receipt.
fn align_receipts(
    exec: &mut torus_evm::executor::BlockExecResult,
    decoded: &[(usize, DecodedTx)],
    height: u64,
) {
    let torus_evm::executor::BlockExecResult {
        receipts,
        included_indices,
        ..
    } = exec;
    for (j, receipt) in receipts.iter_mut().enumerate() {
        if let Some((orig_idx, dtx)) = included_indices.get(j).and_then(|&e| decoded.get(e)) {
            receipt.tx_hash = dtx.tx_hash;
            receipt.tx_index = *orig_idx as u32;
        }
        receipt.block_number = height;
    }
}

/// Merge `src` bundle changes into `dst`. For overlapping accounts/slots,
/// `src` (the later block) wins.
pub fn merge_bundle_into(dst: &mut BundleState, src: &BundleState) {
    for (addr, src_acct) in &src.state {
        let entry = dst.state.entry(*addr).or_insert_with(|| src_acct.clone());
        if std::ptr::eq(entry, src_acct) {
            continue;
        }
        entry.info = src_acct.info.clone();
        for (slot, slot_val) in &src_acct.storage {
            entry.storage.insert(*slot, *slot_val);
        }
    }
    for (hash, bytecode) in &src.contracts {
        dst.contracts.insert(*hash, bytecode.clone());
    }
}
