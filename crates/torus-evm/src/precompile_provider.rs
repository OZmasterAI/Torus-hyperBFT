//! Torus custom precompile provider for revm v36.
//!
//! Wraps [`EthPrecompiles`] to add Torus cross-VM precompiles (0x0800–0x0820)
//! alongside standard Ethereum ones. Delegates native-state reads/writes to
//! [`torus_core::precompiles::execute_precompile`].

use revm::context::{Cfg, ContextTr, LocalContextTr};
use revm::handler::{EthPrecompiles, PrecompileProvider};
use revm::interpreter::{CallInput, CallInputs, Gas, InstructionResult, InterpreterResult};
use revm::primitives::hardfork::SpecId;
use revm::primitives::{Address, Bytes};

use torus_core::precompiles::{
    execute_precompile, is_precompile, precompile_gas, ALL_PRECOMPILE_ADDRESSES,
};
use torus_state::StateDb;

/// Precompile provider combining standard Ethereum precompiles with
/// Torus cross-VM precompiles (order book, balance, oracle, staking readers
/// and core writer / lockbox).
pub struct TorusPrecompiles<'a> {
    eth: EthPrecompiles,
    state_db: &'a StateDb,
    current_block: u64,
}

impl<'a> TorusPrecompiles<'a> {
    /// Create a new provider for the given spec, state database, and block number.
    pub fn new(spec: SpecId, state_db: &'a StateDb, current_block: u64) -> Self {
        Self {
            eth: EthPrecompiles::new(spec),
            state_db,
            current_block,
        }
    }
}

impl<CTX: ContextTr> PrecompileProvider<CTX> for TorusPrecompiles<'_> {
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

        // Resolve calldata (same SharedBuffer / Bytes pattern as EthPrecompiles).
        let result = {
            let r;
            let input_bytes: &[u8] = match &inputs.input {
                CallInput::SharedBuffer(range) => {
                    if let Some(slice) =
                        context.local().shared_memory_buffer_slice(range.clone())
                    {
                        r = slice;
                        r.as_ref()
                    } else {
                        &[]
                    }
                }
                CallInput::Bytes(bytes) => &bytes.0,
            };

            execute_precompile(
                &address,
                input_bytes,
                &inputs.caller,
                self.state_db,
                self.current_block,
            )
        };

        // Build InterpreterResult.
        let mut gas = Gas::new(inputs.gas_limit);
        let _ = gas.record_cost(gas_required);

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
