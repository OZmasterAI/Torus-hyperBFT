//! Torus custom precompile provider for revm v36.
//!
//! Wraps [`EthPrecompiles`] to add Torus cross-VM precompiles (0x0800–0x0820)
//! alongside standard Ethereum ones. Delegates native-state reads/writes to
//! [`torus_core::precompiles::execute_precompile`].

use revm::context::{Cfg, ContextTr, JournalTr, LocalContextTr};
use revm::context_interface::journaled_state::account::JournaledAccountTr;
use revm::handler::{EthPrecompiles, PrecompileProvider};
use revm::interpreter::{CallInput, CallInputs, Gas, InstructionResult, InterpreterResult};
use revm::primitives::hardfork::SpecId;
use revm::primitives::{Address, Bytes, U256};

use torus_core::error::CoreError;
use torus_core::precompiles::{
    execute_precompile_metered, is_precompile, is_reader_precompile, precompile_gas, reader_budget,
    reader_gas, ReadMeter, ADDR_LOCKBOX, ALL_PRECOMPILE_ADDRESSES,
};
use torus_state::NativeStateOverlay;

use std::sync::{Arc, OnceLock};

/// Precompile provider combining standard Ethereum precompiles with
/// Torus cross-VM precompiles (order book, balance, oracle, staking readers
/// and core writer / lockbox).
pub struct TorusPrecompiles {
    eth: EthPrecompiles,
    /// T4.4: per-transaction journal for writer-precompile side effects. Writes buffer
    /// in this overlay during EVM execution (reads fall through to the base `StateDb`,
    /// preserving read-your-writes); the executor persists them via
    /// [`NativeStateOverlay::commit_tx`] only when the calling tx SUCCEEDS and drops
    /// them via [`NativeStateOverlay::discard_tx`] on revert/halt — so an EVM revert
    /// also reverts native side effects.
    journal: NativeStateOverlay,
    current_block: u64,
    /// The block's header timestamp (seconds) — the clock of the oracle
    /// staleness rule in 0x0800 / 0x0802 (item 2).
    current_timestamp: u64,
    /// eth_call / eth_estimateGas simulation: deny state-mutating (writer) precompiles so
    /// a simulation can't durably mutate the shared `StateDb` outside consensus.
    read_only: bool,
    /// R02 branch 5 (owner option A): the first LOCAL storage fault a precompile
    /// hit (`CoreError::PrecompileLocalFault`). Set before [`Self::run`] aborts
    /// the execution with a fatal error; the executor reads it
    /// ([`Self::local_fault`]) to return `EvmError::LocalFault`. Per provider,
    /// i.e. per executed block / tx.
    local_fault: Arc<OnceLock<String>>,
}

impl TorusPrecompiles {
    /// Create a provider for real transaction/block execution (writer precompiles enabled,
    /// buffered in `journal` until the executor commits or discards them per tx).
    pub fn new(
        spec: SpecId,
        journal: NativeStateOverlay,
        current_block: u64,
        current_timestamp: u64,
    ) -> Self {
        Self::with_mode(spec, journal, current_block, current_timestamp, false)
    }

    /// Create a provider for eth_call / eth_estimateGas simulation, where `read_only`
    /// denies writer precompiles (they would bypass the EVM sandbox and mutate the DB).
    pub fn with_mode(
        spec: SpecId,
        journal: NativeStateOverlay,
        current_block: u64,
        current_timestamp: u64,
        read_only: bool,
    ) -> Self {
        Self {
            eth: EthPrecompiles::new(spec),
            journal,
            current_block,
            current_timestamp,
            read_only,
            local_fault: Arc::default(),
        }
    }

    /// R02 branch 5: a handle on this provider's local-fault cell (see the
    /// field); take it before the provider moves into the EVM.
    pub fn local_fault(&self) -> Arc<OnceLock<String>> {
        self.local_fault.clone()
    }
}

impl<CTX: ContextTr> PrecompileProvider<CTX> for TorusPrecompiles {
    type Output = InterpreterResult;

    fn set_spec(&mut self, spec: <CTX::Cfg as Cfg>::Spec) -> bool {
        // Torus precompile addresses don't change with spec — delegate only.
        <EthPrecompiles as PrecompileProvider<CTX>>::set_spec(&mut self.eth, spec)
    }

    fn run(
        &mut self,
        context: &mut CTX,
        inputs: &CallInputs,
    ) -> Result<Option<InterpreterResult>, String> {
        let address = inputs.bytecode_address;

        // Fast path: not a Torus precompile → delegate to standard set.
        if !is_precompile(&address) {
            return self.eth.run(context, inputs);
        }

        // Determine gas cost.
        let id_bytes = address.as_slice();
        let id = u16::from_be_bytes([id_bytes[18], id_bytes[19]]);
        let gas_required = precompile_gas(id);

        if gas_required > inputs.gas_limit {
            return Ok(Some(InterpreterResult::new_oog(inputs.gas_limit)));
        }

        // Review (blocking): a reader's WORK is bounded by this call's gas, not
        // only its charge — its meter holds the gas its limit has above the base.
        let mut meter = if is_reader_precompile(id) {
            ReadMeter::with_max(reader_budget(inputs.gas_limit))
        } else {
            ReadMeter::unlimited()
        };

        // Wei revm already moved into this precompile for this frame: only a CALL's
        // transferred value counts (DELEGATECALL value is apparent, CALLCODE sends to the
        // caller itself).
        let call_value = if inputs.target_address == address {
            inputs.transfer_value().unwrap_or_default()
        } else {
            U256::ZERO
        };

        // Resolve calldata (same SharedBuffer / Bytes pattern as EthPrecompiles).
        let result = {
            let r;
            let input_bytes: &[u8] = match &inputs.input {
                CallInput::SharedBuffer(range) => {
                    if let Some(slice) = context.local().shared_memory_buffer_slice(range.clone()) {
                        r = slice;
                        r.as_ref()
                    } else {
                        &[]
                    }
                }
                CallInput::Bytes(bytes) => &bytes.0,
            };

            if self.read_only {
                execute_precompile_metered(
                    &address,
                    input_bytes,
                    &inputs.caller,
                    U256::ZERO,
                    &self.journal,
                    self.current_block,
                    self.current_timestamp,
                    true,
                    &mut meter,
                )
            } else if !is_reader_precompile(id) && (!inputs.scheme.is_call() || inputs.is_static)
            {
                // Every writer (CoreWriter 0x0810, CoreWriterStaking 0x0811, Lockbox 0x0820)
                // acts for msg.sender: a DELEGATECALL / CALLCODE hands it the ORIGINAL caller,
                // so any contract a user calls could trade / (un)delegate / move funds as
                // that user; a static context forbids state change. Readers stay callable.
                Err(CoreError::InvalidPrecompileInput(
                    "writer precompile requires a plain non-static CALL".into(),
                ))
            } else {
                execute_precompile_metered(
                    &address,
                    input_bytes,
                    &inputs.caller,
                    call_value,
                    &self.journal,
                    self.current_block,
                    self.current_timestamp,
                    false,
                    &mut meter,
                )
            }
        };

        // EVM-PF-05 (Hyperliquid model): only an accepted lockbox deposit succeeds with
        // value (every other precompile/selector rejects it). Its wei is BURNED here, in
        // the same frame and through revm's journal, so it leaves EVM supply atomically
        // with the queued native credit, 0x0820 never accumulates a balance, and any
        // enclosing revert restores it (the queued credit is dropped with that frame).
        let result = match result {
            Ok(output) if id == ADDR_LOCKBOX && !call_value.is_zero() => {
                let burned = context
                    .journal_mut()
                    .load_account_mut(address)
                    .map(|mut acct| acct.data.decr_balance(call_value))
                    .unwrap_or(false);
                if burned {
                    Ok(output)
                } else {
                    Err(CoreError::InvalidPrecompileInput(
                        "lockbox: failed to burn deposit value".into(),
                    ))
                }
            }
            other => other,
        };

        // R02 branch 5 (owner option A): a LOCAL storage fault is never a
        // revert (its bytes and gas would be an answer only this node gives):
        // record it and abort the execution with a fatal error (revm maps it
        // to `EVMError::Custom`, no receipt); the executor returns
        // `EvmError::LocalFault` and the node fail-stops. Only this variant:
        // every other precompile error stays a revert, exactly as before.
        if let Err(CoreError::PrecompileLocalFault(msg)) = &result {
            let _ = self.local_fault.set(msg.clone());
            return Err(format!("precompile local fault: {msg}"));
        }

        // A reader over its budget runs out of gas (its stipend is spent).
        if matches!(result, Err(CoreError::PrecompileOutOfGas)) {
            return Ok(Some(InterpreterResult::new_oog(inputs.gas_limit)));
        }
        // HL-parity: a reader pays the base + the work it did (500 per row read,
        // 20 per word returned / blob chunk read), whether it answered or reverted.
        let gas_required = if is_reader_precompile(id) {
            reader_gas(meter.used())
        } else {
            gas_required
        };

        // Build InterpreterResult.
        // FIX MED-NEW-05: Check gas.record_cost return value instead of discarding it.
        let mut gas = Gas::new(inputs.gas_limit);
        if !gas.record_cost(gas_required) {
            return Ok(Some(InterpreterResult::new_oog(inputs.gas_limit)));
        }

        match result {
            Ok(output) => Ok(Some(InterpreterResult {
                result: InstructionResult::Return,
                output: Bytes::from(output),
                gas,
            })),
            Err(e) => Ok(Some(InterpreterResult {
                result: InstructionResult::Revert,
                output: Bytes::from(format!("{e}").into_bytes()),
                gas,
            })),
        }
    }

    fn warm_addresses(&self) -> Box<impl Iterator<Item = Address>> {
        let torus_addrs = ALL_PRECOMPILE_ADDRESSES.iter().copied();
        Box::new(self.eth.warm_addresses().chain(torus_addrs))
    }

    fn contains(&self, address: &Address) -> bool {
        is_precompile(address) || self.eth.contains(address)
    }
}
