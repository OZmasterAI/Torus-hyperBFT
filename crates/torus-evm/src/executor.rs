use alloy_primitives::{Address, Bloom, B256, U256};
use revm::context::result::{EVMError, ExecutionResult, HaltReason, Output};
use revm::context::{BlockEnv as RevmBlockEnv, TxEnv};
use revm::database::states::bundle_state::BundleRetention;
use revm::database::{BundleState, State};
use revm::interpreter::{CallInputs, CallOutcome, CreateInputs, CreateOutcome};
use revm::primitives::hardfork::SpecId;
use revm::{Context, InspectCommitEvm, Inspector, MainBuilder, MainContext};

use crate::precompile_provider::TorusPrecompiles;

use torus_state::cf::CF_CORE_WRITER_QUEUE;
use torus_state::{NativeStateOverlay, StateDb, StateOverlay};
use torus_types::{Log as TorusLog, Receipt};

use crate::bloom::logs_bloom;
use crate::error::EvmError;

/// Torus mainnet chain ID.
pub const TORUS_CHAIN_ID: u64 = 7778;

/// T4.4 revert-safety: mirrors revm's call-frame checkpoint/commit/revert lifecycle onto
/// the writer-precompile journal.
///
/// Writer precompiles (CoreWriter / Lockbox) buffer their native writes in a shared
/// [`NativeStateOverlay`] journal. revm rolls back EVM state on a frame revert but knows
/// nothing about that journal, so a native write made inside a CAUGHT inner-frame revert
/// (a Solidity try/catch around a sub-call) would otherwise survive into the top-level tx
/// commit and corrupt native state (prior-review finding C).
///
/// By opening a journal checkpoint on every CALL/CREATE frame entry and committing or
/// reverting it on frame exit according to the frame's outcome, native side effects
/// participate in exactly the same revert boundaries as EVM state. The journal is shared
/// (Arc-backed clone) with the [`TorusPrecompiles`] provider and the executor, so all three
/// observe the same buffer.
struct NativeJournalInspector {
    journal: NativeStateOverlay,
}

impl<CTX> Inspector<CTX> for NativeJournalInspector {
    fn call(&mut self, _context: &mut CTX, _inputs: &mut CallInputs) -> Option<CallOutcome> {
        self.journal.checkpoint();
        None
    }

    fn call_end(&mut self, _context: &mut CTX, _inputs: &CallInputs, outcome: &mut CallOutcome) {
        if outcome.result.result.is_ok() {
            self.journal.commit_checkpoint();
        } else {
            self.journal.revert_to_checkpoint();
        }
    }

    fn create(&mut self, _context: &mut CTX, _inputs: &mut CreateInputs) -> Option<CreateOutcome> {
        self.journal.checkpoint();
        None
    }

    fn create_end(
        &mut self,
        _context: &mut CTX,
        _inputs: &CreateInputs,
        outcome: &mut CreateOutcome,
    ) {
        if outcome.result.result.is_ok() {
            self.journal.commit_checkpoint();
        } else {
            self.journal.revert_to_checkpoint();
        }
    }
}

/// Default block gas limit (30M).
pub const DEFAULT_BLOCK_GAS_LIMIT: u64 = 30_000_000;

/// Block environment parameters for EVM execution.
#[derive(Clone, Debug)]
pub struct BlockEnvCfg {
    pub number: u64,
    pub timestamp: u64,
    pub beneficiary: Address,
    pub gas_limit: u64,
    pub base_fee: u64,
}

impl Default for BlockEnvCfg {
    fn default() -> Self {
        Self {
            number: 0,
            timestamp: 1,
            beneficiary: Address::ZERO,
            gas_limit: DEFAULT_BLOCK_GAS_LIMIT,
            base_fee: 0,
        }
    }
}

/// Item 7 step 2: a block's big lane: its base fee and the senders that pay it (their
/// `EvmUserModify` rows, read by the caller while big blocks are on). Default = one lane.
#[derive(Clone, Debug, Default)]
pub struct BigLane {
    pub base_fee: u64,
    pub senders: std::collections::HashSet<Address>,
}

/// Result of executing a single EVM transaction.
#[derive(Clone, Debug)]
pub struct TxExecResult {
    /// Whether the transaction succeeded.
    pub success: bool,
    /// Gas consumed by this transaction.
    pub gas_used: u64,
    /// Logs emitted (empty for reverts/halts).
    pub logs: Vec<TorusLog>,
    /// Return data (call output) or revert reason.
    pub output: Vec<u8>,
    /// Deployed contract address (creation transactions only).
    pub contract_address: Option<Address>,
}

/// Result of executing a full block of transactions.
#[derive(Debug)]
pub struct BlockExecResult {
    /// Receipts for each executed transaction.
    pub receipts: Vec<Receipt>,
    /// Accumulated state changes for the entire block.
    pub bundle: BundleState,
    /// Total gas consumed by all transactions in the block.
    pub gas_used: u64,
    /// Item 7 step 2: gas used by big-lane txs (the rest is the small lane's).
    pub big_gas_used: u64,
    /// Item 7 step 2: the base-fee part of the fees, each tx at its lane's base fee:
    /// `sum(gas_used * min(effective_gas_price, lane base fee))`.
    pub fee_revenue: u128,
    /// Aggregate logs bloom for the block.
    pub logs_bloom: Bloom,
    /// Indices of input txs that were included (only differs from 0..N when
    /// `skip_invalid` is true and some txs were dropped).
    pub included_indices: Vec<usize>,
    /// F1 (s515): writer-precompile side effects of the block's SUCCESSFUL txs
    /// (CoreWriter / lockbox queue rows for the next block), still pending —
    /// nothing was written to the `StateDb`. The committer persists them in the
    /// SAME atomic batch as `bundle` (`evm_block_batch_incremental`), so a queued
    /// native credit is durable iff the EVM burn that paid for it is. Dropping
    /// the result (execution error, crash before commit) drops them too.
    pub native_writes: NativeStateOverlay,
    /// s114 (CoreWriter follow-up 1): which tx queued each writer-precompile
    /// queue row in `native_writes` — one entry per row a SUCCESSFUL tx
    /// queued, in queue (seq) order. Node-local (the `CF_CORE_WRITER_TX`
    /// index); the queue rows themselves do not carry it.
    pub core_writer_origins: Vec<CoreWriterOrigin>,
}

/// s114: the tx that queued one CoreWriter / lockbox queue row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CoreWriterOrigin {
    /// The row's target (drain) block, the key's high 8 bytes. Item 7 step 2: K blocks
    /// after this one for an order kind, one for any other, later if a block was full.
    pub target_block: u64,
    /// The row's sequence in the target block's queue.
    pub seq: u64,
    /// Index into [`BlockExecResult::receipts`] of the tx that queued it.
    pub receipt: usize,
    /// The row's position among the rows that tx queued (call order).
    pub call_index: u32,
}

/// EVM executor configured for the Torus chain.
///
/// Wraps revm with Cancun spec and the specified chain ID.
/// Executes transactions against a [`StateDb`] backend without
/// modifying the underlying database — state changes are returned
/// as a [`BundleState`] for the caller to commit.
pub struct EvmExecutor {
    chain_id: u64,
}

impl EvmExecutor {
    /// Create an executor for the given chain ID.
    pub fn new(chain_id: u64) -> Self {
        Self { chain_id }
    }

    /// Execute a single transaction against the given state.
    ///
    /// Returns the execution result and the accumulated state changes.
    pub fn execute_tx(
        &self,
        state_db: &StateDb,
        block_cfg: &BlockEnvCfg,
        tx: TxEnv,
    ) -> Result<(TxExecResult, BundleState), EvmError> {
        self.execute_tx_inner(state_db, block_cfg, tx, false)
    }

    /// Execute a transaction in CALL-SIMULATION mode (D2, S392): standard
    /// geth/reth eth_call / eth_estimateGas semantics. Base-fee and nonce
    /// checks are disabled, so a bare call (omitted fee fields => gas price 0)
    /// from an empty account succeeds at height > 0. Provided fee fields still
    /// charge normally, and value transfers still require balance — exactly
    /// geth. Never use this on a consensus path.
    pub fn execute_call(
        &self,
        state_db: &StateDb,
        block_cfg: &BlockEnvCfg,
        tx: TxEnv,
    ) -> Result<(TxExecResult, BundleState), EvmError> {
        self.execute_tx_inner(state_db, block_cfg, tx, true)
    }

    fn execute_tx_inner(
        &self,
        state_db: &StateDb,
        block_cfg: &BlockEnvCfg,
        tx: TxEnv,
        call_mode: bool,
    ) -> Result<(TxExecResult, BundleState), EvmError> {
        let state = State::builder()
            .with_database_ref(state_db)
            .with_bundle_update()
            .build();

        let chain_id = self.chain_id;
        let ctx = Context::mainnet()
            .modify_cfg_chained(|cfg| {
                cfg.set_spec_and_mainnet_gas_params(SpecId::CANCUN);
                cfg.chain_id = chain_id;
                if call_mode {
                    cfg.disable_base_fee = true;
                    cfg.disable_nonce_check = true;
                }
            })
            .modify_block_chained(|b| apply_block_env(b, block_cfg))
            .with_db(state);

        // In call-simulation mode (eth_call / eth_estimateGas) run the precompiles
        // read-only so the Torus writer precompiles cannot durably mutate the shared
        // StateDb outside consensus (they bypass revm's revert sandbox).
        //
        // T4.4: writer-precompile side effects are journaled per transaction — they
        // buffer in `journal` during execution and only become durable if the tx
        // succeeds, so an EVM revert also reverts native side effects.
        let journal = NativeStateOverlay::new(state_db.clone());
        let precompiles = TorusPrecompiles::with_mode(
            SpecId::CANCUN,
            journal.clone(),
            block_cfg.number,
            block_cfg.timestamp,
            call_mode,
        );
        let local_fault = precompiles.local_fault();
        let mut evm = ctx
            .build_mainnet_with_inspector(NativeJournalInspector {
                journal: journal.clone(),
            })
            .with_precompiles(precompiles);
        // T4.4: run via the inspector path so `NativeJournalInspector` sees every call frame
        // and can checkpoint/revert the writer-precompile journal at frame boundaries.
        let result = evm
            .inspect_tx_commit(tx)
            .map_err(|e| local_fault_or(&local_fault, e))?;

        // T4.4: persist journaled precompile writes only on tx success (never in call
        // mode, where writer precompiles are denied and the journal stays empty).
        if !call_mode && result.is_success() {
            journal
                .commit_tx(state_db)
                .map_err(|e| EvmError::Internal(format!("precompile journal commit: {e}")))?;
        } else {
            journal.discard_tx();
        }

        let tx_result = build_tx_result(&result);

        // Merge transitions and extract bundle.
        let mut bundle = BundleState::default();
        evm.ctx.modify_db(|s| {
            s.merge_transitions(BundleRetention::Reverts);
            bundle = s.take_bundle();
        });

        Ok((tx_result, bundle))
    }

    /// Execute a block of transactions, enforcing the block gas limit.
    ///
    /// Returns receipts, the accumulated [`BundleState`], total gas used,
    /// and the aggregate logs bloom.
    pub fn execute_block(
        &self,
        state_db: &StateDb,
        block_cfg: &BlockEnvCfg,
        transactions: Vec<TxEnv>,
        skip_invalid: bool,
    ) -> Result<BlockExecResult, EvmError> {
        self.execute_block_lanes(
            state_db,
            block_cfg,
            transactions,
            skip_invalid,
            &BigLane::default(),
        )
    }

    /// [`Self::execute_block`] with a big lane (item 7 step 2): a tx from `big.senders` runs at
    /// `big.base_fee`, every other tx at `block_cfg.base_fee`.
    pub fn execute_block_lanes(
        &self,
        state_db: &StateDb,
        block_cfg: &BlockEnvCfg,
        transactions: Vec<TxEnv>,
        skip_invalid: bool,
        big: &BigLane,
    ) -> Result<BlockExecResult, EvmError> {
        let state = State::builder()
            .with_database_ref(state_db)
            .with_bundle_update()
            .build();

        let chain_id = self.chain_id;
        let ctx = Context::mainnet()
            .modify_cfg_chained(|cfg| {
                cfg.set_spec_and_mainnet_gas_params(SpecId::CANCUN);
                cfg.chain_id = chain_id;
            })
            .modify_block_chained(|b| apply_block_env(b, block_cfg))
            .with_db(state);

        // T4.4: journal for writer-precompile side effects — a tx's writes are kept
        // only when it succeeds, dropped on revert/halt.
        // F1 (s515): the journal is BLOCK-scoped: kept writes stay pending (later txs
        // read them — next_sequence) and are returned in `native_writes` for the
        // committer to persist atomically with the bundle. Nothing touches the DB
        // here, so a block that errors part-way (or a crash before the bundle
        // commit) leaves no queued action behind.
        let journal = NativeStateOverlay::new(state_db.clone());
        let precompiles =
            TorusPrecompiles::new(SpecId::CANCUN, journal.clone(), block_cfg.number, block_cfg.timestamp);
        // R02 branch 5: the provider's local-fault cell (see `local_fault_or`).
        let local_fault = precompiles.local_fault();
        let mut evm = ctx
            .build_mainnet_with_inspector(NativeJournalInspector {
                journal: journal.clone(),
            })
            .with_precompiles(precompiles);

        let mut receipts = Vec::with_capacity(transactions.len());
        let mut cumulative_gas: u64 = 0;
        let mut block_bloom = Bloom::ZERO;
        let mut included_indices = Vec::with_capacity(transactions.len());
        let mut receipt_idx: usize = 0;
        let mut core_writer_origins = Vec::new();
        let mut big_gas_used: u64 = 0;
        let mut fee_revenue: u128 = 0;

        for (idx, tx) in transactions.into_iter().enumerate() {
            let max_fee = tx.gas_price;
            let priority_fee = tx.gas_priority_fee;
            // Item 7 step 2: the tx runs at its lane's base fee (revm's per-tx block env).
            let in_big_lane = big.senders.contains(&tx.caller);
            let lane_base_fee = if in_big_lane {
                big.base_fee
            } else {
                block_cfg.base_fee
            };
            if !big.senders.is_empty() {
                evm.ctx.modify_block(|b| b.basefee = lane_base_fee);
            }

            // T4.4: inspector path so writer-precompile side effects are checkpointed
            // per call frame (caught inner-frame reverts roll back their native writes).
            journal.begin_tx();
            let result = match evm.inspect_tx_commit(tx) {
                Ok(r) => r,
                Err(EVMError::Transaction(tx_err)) if skip_invalid => {
                    tracing::warn!(idx, ?tx_err, "skipping invalid tx during block proposal");
                    journal.revert_tx();
                    continue;
                }
                Err(e) => return Err(local_fault_or(&local_fault, e)),
            };
            included_indices.push(idx);
            let gas_used = result.gas().used();
            if in_big_lane {
                big_gas_used += gas_used;
            }

            cumulative_gas =
                cumulative_gas
                    .checked_add(gas_used)
                    .ok_or(EvmError::BlockGasLimitExceeded {
                        cumulative: u64::MAX,
                        limit: block_cfg.gas_limit,
                    })?;

            if cumulative_gas > block_cfg.gas_limit {
                return Err(EvmError::BlockGasLimitExceeded {
                    cumulative: cumulative_gas,
                    limit: block_cfg.gas_limit,
                });
            }

            let success = result.is_success();

            // T4.4: revert must revert writer-precompile side effects too.
            // s114: a kept tx's queue rows are linked to its receipt.
            if success {
                let queued = journal.keep_tx_written_keys(CF_CORE_WRITER_QUEUE);
                push_core_writer_origins(&mut core_writer_origins, queued, receipts.len());
            } else {
                journal.revert_tx();
            }

            // Only include logs from successful transactions.
            let (torus_logs, tx_bloom) = if success {
                let evm_logs = result.logs();
                let bloom = logs_bloom(evm_logs.iter());
                let logs = evm_logs.iter().map(convert_log).collect();
                (logs, bloom)
            } else {
                (Vec::new(), Bloom::ZERO)
            };

            block_bloom |= tx_bloom;

            let contract_address = match &result {
                ExecutionResult::Success {
                    output: Output::Create(_, addr),
                    ..
                } => *addr,
                _ => None,
            };

            let effective_gas_price =
                calc_effective_gas_price(lane_base_fee, max_fee, priority_fee);
            fee_revenue +=
                u128::from(gas_used) * u128::from(effective_gas_price.min(lane_base_fee));

            receipts.push(Receipt {
                tx_hash: B256::ZERO,
                block_number: block_cfg.number,
                block_hash: B256::ZERO,
                tx_index: receipt_idx as u32,
                cumulative_gas_used: cumulative_gas,
                gas_used,
                contract_address,
                logs: torus_logs,
                logs_bloom: tx_bloom,
                status: success,
                effective_gas_price,
            });
            receipt_idx += 1;
        }

        // Merge all transaction transitions into the bundle.
        let mut bundle = BundleState::default();
        evm.ctx.modify_db(|s| {
            s.merge_transitions(BundleRetention::Reverts);
            bundle = s.take_bundle();
        });

        Ok(BlockExecResult {
            receipts,
            bundle,
            gas_used: cumulative_gas,
            big_gas_used,
            fee_revenue,
            logs_bloom: block_bloom,
            included_indices,
            native_writes: journal,
            core_writer_origins,
        })
    }

    /// Same as [`execute_block`](Self::execute_block) but reads EVM state from
    /// a [`StateOverlay`] (pending parent bundles layered on state_db).
    /// Precompiles still read native state from the underlying `state_db`.
    pub fn execute_block_with_overlay(
        &self,
        overlay: &StateOverlay,
        block_cfg: &BlockEnvCfg,
        transactions: Vec<TxEnv>,
        skip_invalid: bool,
    ) -> Result<BlockExecResult, EvmError> {
        let state = State::builder()
            .with_database_ref(overlay)
            .with_bundle_update()
            .build();

        let chain_id = self.chain_id;
        let ctx = Context::mainnet()
            .modify_cfg_chained(|cfg| {
                cfg.set_spec_and_mainnet_gas_params(SpecId::CANCUN);
                cfg.chain_id = chain_id;
            })
            .modify_block_chained(|b| apply_block_env(b, block_cfg))
            .with_db(state);

        // T4.4: journal for writer-precompile side effects. Precompiles read native
        // state from the underlying `state_db` (the overlay base).
        // F1 (s515): block-scoped, returned in `native_writes` (see `execute_block`).
        let journal = NativeStateOverlay::new(overlay.base().clone());
        let precompiles =
            TorusPrecompiles::new(SpecId::CANCUN, journal.clone(), block_cfg.number, block_cfg.timestamp);
        // R02 branch 5: the provider's local-fault cell (see `local_fault_or`).
        let local_fault = precompiles.local_fault();
        let mut evm = ctx
            .build_mainnet_with_inspector(NativeJournalInspector {
                journal: journal.clone(),
            })
            .with_precompiles(precompiles);

        let mut receipts = Vec::with_capacity(transactions.len());
        let mut cumulative_gas: u64 = 0;
        let mut block_bloom = Bloom::ZERO;
        let mut included_indices = Vec::with_capacity(transactions.len());
        let mut receipt_idx: usize = 0;
        let mut core_writer_origins = Vec::new();
        let mut fee_revenue: u128 = 0;

        for (idx, tx) in transactions.into_iter().enumerate() {
            let max_fee = tx.gas_price;
            let priority_fee = tx.gas_priority_fee;

            // T4.4: inspector path so writer-precompile side effects are checkpointed
            // per call frame (caught inner-frame reverts roll back their native writes).
            journal.begin_tx();
            let result = match evm.inspect_tx_commit(tx) {
                Ok(r) => r,
                Err(EVMError::Transaction(tx_err)) if skip_invalid => {
                    tracing::warn!(idx, ?tx_err, "skipping invalid tx during block proposal");
                    journal.revert_tx();
                    continue;
                }
                Err(e) => return Err(local_fault_or(&local_fault, e)),
            };
            included_indices.push(idx);
            let gas_used = result.gas().used();

            cumulative_gas =
                cumulative_gas
                    .checked_add(gas_used)
                    .ok_or(EvmError::BlockGasLimitExceeded {
                        cumulative: u64::MAX,
                        limit: block_cfg.gas_limit,
                    })?;

            if cumulative_gas > block_cfg.gas_limit {
                return Err(EvmError::BlockGasLimitExceeded {
                    cumulative: cumulative_gas,
                    limit: block_cfg.gas_limit,
                });
            }

            let success = result.is_success();

            // T4.4: revert must revert writer-precompile side effects too.
            // s114: a kept tx's queue rows are linked to its receipt.
            if success {
                let queued = journal.keep_tx_written_keys(CF_CORE_WRITER_QUEUE);
                push_core_writer_origins(&mut core_writer_origins, queued, receipts.len());
            } else {
                journal.revert_tx();
            }

            let (torus_logs, tx_bloom) = if success {
                let evm_logs = result.logs();
                let bloom = logs_bloom(evm_logs.iter());
                let logs = evm_logs.iter().map(convert_log).collect();
                (logs, bloom)
            } else {
                (Vec::new(), Bloom::ZERO)
            };

            block_bloom |= tx_bloom;

            let contract_address = match &result {
                ExecutionResult::Success {
                    output: Output::Create(_, addr),
                    ..
                } => *addr,
                _ => None,
            };

            let effective_gas_price =
                calc_effective_gas_price(block_cfg.base_fee, max_fee, priority_fee);
            fee_revenue +=
                u128::from(gas_used) * u128::from(effective_gas_price.min(block_cfg.base_fee));

            receipts.push(Receipt {
                tx_hash: B256::ZERO,
                block_number: block_cfg.number,
                block_hash: B256::ZERO,
                tx_index: receipt_idx as u32,
                cumulative_gas_used: cumulative_gas,
                gas_used,
                contract_address,
                logs: torus_logs,
                logs_bloom: tx_bloom,
                status: success,
                effective_gas_price,
            });
            receipt_idx += 1;
        }

        let mut bundle = BundleState::default();
        evm.ctx.modify_db(|s| {
            s.merge_transitions(BundleRetention::Reverts);
            bundle = s.take_bundle();
        });

        Ok(BlockExecResult {
            receipts,
            bundle,
            gas_used: cumulative_gas,
            big_gas_used: 0,
            fee_revenue,
            logs_bloom: block_bloom,
            included_indices,
            native_writes: journal,
            core_writer_origins,
        })
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// s114: one [`CoreWriterOrigin`] per queue row key a kept tx wrote (`keys`,
/// its call order), for the receipt at index `receipt`. Every key the queue
/// writes is `target (8 BE) ‖ seq (8 BE)`; any other shape is not a queue row
/// `enqueue` wrote and is not linked.
fn push_core_writer_origins(out: &mut Vec<CoreWriterOrigin>, keys: Vec<Vec<u8>>, receipt: usize) {
    let mut call_index = 0u32;
    for key in keys {
        let Ok(k) = <[u8; 16]>::try_from(key.as_slice()) else {
            continue;
        };
        out.push(CoreWriterOrigin {
            target_block: u64::from_be_bytes(k[..8].try_into().expect("8 bytes")),
            seq: u64::from_be_bytes(k[8..].try_into().expect("8 bytes")),
            receipt,
            call_index,
        });
        call_index += 1;
    }
}

/// Apply [`BlockEnvCfg`] to revm's [`RevmBlockEnv`].
fn apply_block_env(b: &mut RevmBlockEnv, cfg: &BlockEnvCfg) {
    b.number = U256::from(cfg.number);
    b.timestamp = U256::from(cfg.timestamp);
    b.beneficiary = cfg.beneficiary;
    b.gas_limit = cfg.gas_limit;
    b.basefee = cfg.base_fee;
}

/// Convert a revm `ExecutionResult` into our [`TxExecResult`].
fn build_tx_result(result: &ExecutionResult<HaltReason>) -> TxExecResult {
    let gas_used = result.gas().used();
    let success = result.is_success();

    let logs = if success {
        result.logs().iter().map(convert_log).collect()
    } else {
        Vec::new()
    };

    let (output, contract_address) = match result {
        ExecutionResult::Success { output, .. } => match output {
            Output::Call(data) => (data.to_vec(), None),
            Output::Create(data, addr) => (data.to_vec(), *addr),
        },
        ExecutionResult::Revert { output, .. } => (output.to_vec(), None),
        ExecutionResult::Halt { .. } => (Vec::new(), None),
    };

    TxExecResult {
        success,
        gas_used,
        logs,
        output,
        contract_address,
    }
}

/// Convert a revm log into a [`torus_types::Log`].
fn convert_log(log: &alloy_primitives::Log) -> TorusLog {
    TorusLog {
        address: log.address,
        topics: log.data.topics().to_vec(),
        data: log.data.data.to_vec(),
    }
}

/// Effective gas price per EIP-1559.
///
/// Legacy (no priority fee): `max_fee` is the gas price.
/// EIP-1559: `base_fee + min(priority_fee, max_fee - base_fee)`.
///
/// FIX EVM-PF-02: Caps at `u64::MAX` instead of silent truncation via `as u64`.
fn calc_effective_gas_price(base_fee: u64, max_fee: u128, priority_fee: Option<u128>) -> u64 {
    let result = match priority_fee {
        Some(pf) => {
            let max_priority = pf.min(max_fee.saturating_sub(base_fee as u128));
            base_fee as u128 + max_priority
        }
        None => max_fee,
    };
    u64::try_from(result).unwrap_or(u64::MAX)
}

/// R02 branch 5 (owner option A): an execution error is `EvmError::LocalFault`
/// when a precompile recorded a local storage fault in `cell` (it aborted the
/// execution with a fatal error for exactly that reason), else the usual
/// mapping ([`map_evm_err`]). The consensus app fail-stops on `LocalFault`
/// only.
fn local_fault_or<DB: core::fmt::Debug, TX: core::fmt::Debug>(
    cell: &std::sync::OnceLock<String>,
    err: EVMError<DB, TX>,
) -> EvmError {
    match cell.get() {
        Some(msg) => EvmError::LocalFault(msg.clone()),
        None => map_evm_err(err),
    }
}

/// Map a revm [`EVMError`] into our [`EvmError`].
fn map_evm_err<DB: core::fmt::Debug, TX: core::fmt::Debug>(err: EVMError<DB, TX>) -> EvmError {
    match err {
        EVMError::Transaction(tx_err) => EvmError::InvalidTransaction(format!("{tx_err:?}")),
        EVMError::Database(db_err) => EvmError::Internal(format!("database: {db_err:?}")),
        EVMError::Header(h_err) => EvmError::Internal(format!("header: {h_err:?}")),
        EVMError::Custom(msg) => EvmError::Internal(msg),
    }
}

// AUDIT: EVM-FIND-22 -- Unit tests for calc_effective_gas_price.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calc_effective_gas_price_legacy() {
        // Legacy tx (no priority fee): gas price is max_fee directly.
        assert_eq!(calc_effective_gas_price(100, 500, None), 500);
    }

    #[test]
    fn calc_effective_gas_price_eip1559_normal() {
        // EIP-1559: base_fee + min(priority_fee, max_fee - base_fee)
        // = 100 + min(50, 400) = 150
        assert_eq!(calc_effective_gas_price(100, 500, Some(50)), 150);
    }

    #[test]
    fn calc_effective_gas_price_eip1559_capped() {
        // Priority fee exceeds headroom: capped to max_fee - base_fee.
        // = 100 + min(1000, 400) = 500
        assert_eq!(calc_effective_gas_price(100, 500, Some(1000)), 500);
    }

    #[test]
    fn calc_effective_gas_price_overflow_saturates() {
        // Values exceeding u64::MAX saturate instead of truncating.
        let huge: u128 = u64::MAX as u128 + 1000;
        assert_eq!(calc_effective_gas_price(0, huge, None), u64::MAX);
        assert_eq!(calc_effective_gas_price(100, huge, Some(huge)), u64::MAX);
    }

    #[test]
    fn calc_effective_gas_price_zero_base() {
        // Zero base fee: effective price equals min(priority_fee, max_fee).
        assert_eq!(calc_effective_gas_price(0, 100, Some(50)), 50);
        assert_eq!(calc_effective_gas_price(0, 0, Some(0)), 0);
    }
}
