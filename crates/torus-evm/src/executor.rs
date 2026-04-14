use alloy_primitives::{Address, Bloom, B256, U256};
use revm::context::result::{EVMError, ExecutionResult, HaltReason, Output};
use revm::context::{BlockEnv as RevmBlockEnv, TxEnv};
use revm::database::states::bundle_state::BundleRetention;
use revm::database::{BundleState, State};
use revm::primitives::hardfork::SpecId;
use revm::{Context, ExecuteCommitEvm, MainBuilder, MainContext};

use crate::precompile_provider::TorusPrecompiles;

use torus_state::StateDb;
use torus_types::{Log as TorusLog, Receipt};

use crate::bloom::logs_bloom;
use crate::error::EvmError;

/// Torus mainnet chain ID.
pub const TORUS_CHAIN_ID: u64 = 7777;

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
    /// Aggregate logs bloom for the block.
    pub logs_bloom: Bloom,
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

        let mut evm = ctx
            .build_mainnet()
            .with_precompiles(TorusPrecompiles::new(SpecId::CANCUN, state_db, block_cfg.number));
        let result = evm.transact_commit(tx).map_err(map_evm_err)?;

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

        let mut evm = ctx
            .build_mainnet()
            .with_precompiles(TorusPrecompiles::new(SpecId::CANCUN, state_db, block_cfg.number));

        let mut receipts = Vec::with_capacity(transactions.len());
        let mut cumulative_gas: u64 = 0;
        let mut block_bloom = Bloom::ZERO;

        for (idx, tx) in transactions.into_iter().enumerate() {
            let max_fee = tx.gas_price;
            let priority_fee = tx.gas_priority_fee;

            let result = evm.transact_commit(tx).map_err(map_evm_err)?;
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
                calc_effective_gas_price(block_cfg.base_fee, max_fee, priority_fee);

            receipts.push(Receipt {
                tx_hash: B256::ZERO,
                block_number: block_cfg.number,
                block_hash: B256::ZERO,
                tx_index: idx as u32,
                cumulative_gas_used: cumulative_gas,
                gas_used,
                contract_address,
                logs: torus_logs,
                logs_bloom: tx_bloom,
                status: success,
                effective_gas_price,
            });
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
            logs_bloom: block_bloom,
        })
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

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
fn calc_effective_gas_price(base_fee: u64, max_fee: u128, priority_fee: Option<u128>) -> u64 {
    match priority_fee {
        Some(pf) => {
            let max_priority = pf.min(max_fee.saturating_sub(base_fee as u128));
            (base_fee as u128 + max_priority) as u64
        }
        None => max_fee as u64,
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
