//! Block validation — execute a proposed block and verify its state root.

use alloy_primitives::{Address, B256};

use revm::database::BundleState;

use torus_evm::{BlockEnvCfg, EvmExecutor};
use torus_state::incremental::TrieUpdates;
use torus_state::{StateDb, StateOverlay};
use torus_types::{NativeAction, Receipt, TorusBlock};

use crate::decode::{decode_all_txs, decode_txs_lossy, DecodedTx};
use crate::error::BridgeError;
use crate::state_root::{
    compute_post_bundle_state_root, compute_post_bundle_state_root_with_updates,
};

/// Successfully validated block — ready for commitment.
#[derive(Debug)]
pub struct ValidatedBlock {
    /// EVM receipts with tx hashes set.
    pub receipts: Vec<Receipt>,
    /// Accumulated state changes from EVM execution.
    pub bundle: BundleState,
    /// F1 (s515): the block's pending writer-precompile queue rows — commit them
    /// in the SAME batch as `bundle` and the block's native flush
    /// (`torus_state::incremental::evm_block_batch_incremental`, consensus bug (c)).
    pub native_writes: torus_state::NativeStateOverlay,
    /// s114 (CoreWriter follow-up 1): for each queue row in `native_writes`,
    /// its key `(target block, seq)` and the EVM tx that queued it — the
    /// node-local `CF_CORE_WRITER_TX` rows, written in the same batch.
    pub core_writer_links: Vec<(u64, u64, torus_state::corewriter_tx::CoreWriterTxLink)>,
    /// Computed state root (matches the block header).
    pub state_root: B256,
    /// EVM `(root, TrieUpdates)` pair from this validation's single incremental StateRoot run
    /// (T4.1) — `Some` only when the incremental engine produced it (flag on + trie built).
    /// Reused by `commit_evm_bundle_incremental` so the committed-block path never recomputes
    /// the root. Node-local; never serialized or consensus-visible.
    pub evm_root_updates: Option<(B256, TrieUpdates)>,
    /// Recovered (sender, action) pairs for post-commit native execution.
    pub native_sender_actions: Vec<(Address, NativeAction)>,
    /// Item 7 step 2: gas the block's EVM txs used, all lanes.
    pub gas_used: u64,
    /// Item 7 step 2: of `gas_used`, the big lane's (F3 counts each lane's gas per epoch).
    pub big_gas_used: u64,
    /// Item 7 step 2: the base-fee part of the fees, each tx at its lane's base fee.
    pub fee_revenue: u128,
}

/// Item 7 option 0 (task 1.5): where a block's EVM part reads.
pub enum EvmReads<'a> {
    /// The DB: revm reads it, the writer-precompile journal is an overlay over
    /// it (the serial path, the vote-side validation, RPC).
    Db(&'a StateDb),
    /// The block overlay's layers: revm through `torus_state::LayeredEvmDb`,
    /// the journal over the same layers (`torus_state::EvmBlockState`, moved
    /// into the executor and returned as `native_writes`).
    Layered(torus_state::EvmBlockState<'a>),
}

impl<'a> EvmReads<'a> {
    /// Item 7 option 0 (task 1.4): a point read before the execution (the
    /// lane read): the DB, or the block overlay with its layers.
    pub fn get_cf_raw(
        &self,
        cf: &str,
        key: &[u8],
    ) -> Result<Option<Vec<u8>>, torus_state::StateError> {
        match self {
            EvmReads::Db(db) => db.get_cf_raw(cf, key),
            EvmReads::Layered(state) => state.lane_read(cf, key),
        }
    }

    /// The DB under the reads (the EVM root compute reads it).
    pub fn state_db(&self) -> &'a StateDb {
        match self {
            EvmReads::Db(db) => db,
            EvmReads::Layered(state) => state.state_db(),
        }
    }
}

/// Item 7 option 0 (task 1.5): whether the catch-up validation computes the
/// post-bundle EVM root.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RootMode {
    /// On the executing thread, over the DB (today's serial path); its
    /// `(root, TrieUpdates)` pair is reused by the block's trie batch.
    OnE,
    /// No root and no full-scan oracle (the fast path, step 3: the EVM trie
    /// moves to the flush worker): `state_root` is zero and
    /// `evm_root_updates` `None`.
    Skip,
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
        self.validate_block_inner(block, EvmReads::Db(state_db), evm_executor, None, false, RootMode::OnE)
    }

    /// Validate with parent header for base_fee verification.
    pub fn validate_block_with_parent(
        &self,
        block: &TorusBlock,
        state_db: &StateDb,
        evm_executor: &EvmExecutor,
        parent_header: &torus_types::TorusBlockHeader,
    ) -> Result<ValidatedBlock, BridgeError> {
        self.validate_block_inner(
            block,
            EvmReads::Db(state_db),
            evm_executor,
            Some(parent_header),
            false,
            RootMode::OnE,
        )
    }

    /// Validate during catch-up replay — skips state root check since blocks
    /// are already consensus-committed. Item 7 option 0 (task 1.5): the EVM
    /// part reads `reads` (the DB, or the block overlay's layers), and `root`
    /// says whether the post-bundle EVM root is computed.
    pub fn validate_block_for_catchup(
        &self,
        block: &TorusBlock,
        reads: EvmReads<'_>,
        evm_executor: &EvmExecutor,
        root: RootMode,
    ) -> Result<ValidatedBlock, BridgeError> {
        self.validate_block_inner(block, reads, evm_executor, None, true, root)
    }

    fn validate_block_inner(
        &self,
        block: &TorusBlock,
        reads: EvmReads<'_>,
        evm_executor: &EvmExecutor,
        parent_header: Option<&torus_types::TorusBlockHeader>,
        skip_state_root_check: bool,
        root: RootMode,
    ) -> Result<ValidatedBlock, BridgeError> {
        debug_assert!(
            skip_state_root_check || root == RootMode::OnE,
            "the strict validation always checks the root"
        );
        let state_db = reads.state_db();
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

        // Item 7 step 2: while big blocks are on (M > 0) a sender whose `EvmUserModify` row says big
        // pays the big base fee (owner answer 7: the row when the block executes). Item 7 option 0
        // (task 1.4): the row is read through `reads`, i.e. through the block overlay when the
        // block runs over its layers, so a row an earlier block wrote and W has not made durable
        // yet counts. A read fault is a local fault (fail-stop), never "small".
        let mut big_lane = torus_evm::BigLane {
            base_fee: block.header.big_base_fee_per_gas,
            ..Default::default()
        };
        if block.header.evm_big_block_interval > 0 {
            let callers: std::collections::HashSet<Address> =
                decoded_txs.iter().map(|(_, d)| d.tx_env.caller).collect();
            for caller in callers {
                let row = reads
                    .get_cf_raw(
                        torus_state::cf::CF_NATIVE_ORACLE,
                        &torus_state::cf::evm_lane_key(&caller),
                    )
                    .map_err(|e| {
                        BridgeError::Evm(torus_evm::EvmError::LocalFault(format!(
                            "EVM lane read: {e}"
                        )))
                    })?;
                if row.is_some() {
                    big_lane.senders.insert(caller);
                }
            }
        }
        let block_cfg = BlockEnvCfg {
            number: block.header.height,
            timestamp: block.header.timestamp,
            beneficiary: block.header.proposer,
            gas_limit: block.header.evm_block_gas_cap(),
            base_fee: block.header.base_fee_per_gas,
        };

        let mut exec_result = match reads {
            EvmReads::Db(db) => evm_executor.execute_block_lanes(
                db,
                torus_state::NativeStateOverlay::new(db.clone()),
                &block_cfg,
                tx_envs,
                skip_state_root_check,
                &big_lane,
            )?,
            EvmReads::Layered(torus_state::EvmBlockState { db, journal }) => evm_executor
                .execute_block_lanes(
                    db,
                    journal,
                    &block_cfg,
                    tx_envs,
                    skip_state_root_check,
                    &big_lane,
                )?,
        };

        align_receipts(&mut exec_result, &decoded_txs, block.header.height);

        if !skip_state_root_check {
            if exec_result.gas_used != block.header.evm_gas_used {
                return Err(BridgeError::InvalidBlock(format!(
                    "gas used mismatch: header={}, executed={}",
                    block.header.evm_gas_used, exec_result.gas_used
                )));
            }

            let computed_fee_revenue = exec_result.fee_revenue;
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

        let (computed_root, evm_root_updates) = match root {
            RootMode::OnE => {
                let root_timer = std::time::Instant::now();
                let out =
                    compute_post_bundle_state_root_with_updates(state_db, &exec_result.bundle)?;
                if let Some(ref m) = self.metrics {
                    m.state_root_compute_seconds
                        .observe(root_timer.elapsed().as_secs_f64());
                }
                out
            }
            RootMode::Skip => (B256::ZERO, None),
        };

        if !skip_state_root_check && computed_root != block.header.state_root {
            return Err(BridgeError::StateRootMismatch {
                expected: block.header.state_root,
                computed: computed_root,
            });
        }

        let core_writer_links = core_writer_links(&exec_result);
        Ok(ValidatedBlock {
            receipts: exec_result.receipts,
            bundle: exec_result.bundle,
            core_writer_links,
            native_writes: exec_result.native_writes,
            state_root: computed_root,
            evm_root_updates,
            native_sender_actions: vec![],
            gas_used: exec_result.gas_used,
            big_gas_used: exec_result.big_gas_used,
            fee_revenue: exec_result.fee_revenue,
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

        let computed_fee_revenue = crate::proposer::compute_fee_revenue(
                &exec_result.receipts,
                block.header.base_fee_per_gas,
            );
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

        let core_writer_links = core_writer_links(&exec_result);
        Ok(ValidatedBlock {
            receipts: exec_result.receipts,
            bundle: exec_result.bundle,
            core_writer_links,
            native_writes: exec_result.native_writes,
            state_root: computed_root,
            // Root computed over the MERGED multi-block bundle — its TrieUpdates are not
            // reusable for a single-block commit.
            evm_root_updates: None,
            native_sender_actions: vec![],
            gas_used: exec_result.gas_used,
            big_gas_used: exec_result.big_gas_used,
            fee_revenue: exec_result.fee_revenue,
        })
    }
}

/// s114 (CoreWriter follow-up 1): the block's queue rows with the EVM tx
/// that queued each (`receipts` must already carry the tx hashes,
/// [`align_receipts`]).
fn core_writer_links(
    exec: &torus_evm::executor::BlockExecResult,
) -> Vec<(u64, u64, torus_state::corewriter_tx::CoreWriterTxLink)> {
    exec.core_writer_origins
        .iter()
        .filter_map(|o| {
            let receipt = exec.receipts.get(o.receipt)?;
            Some((
                o.target_block,
                o.seq,
                torus_state::corewriter_tx::CoreWriterTxLink {
                    tx_hash: receipt.tx_hash,
                    call_index: o.call_index,
                },
            ))
        })
        .collect()
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
