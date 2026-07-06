//! Lockbox — bidirectional asset transfer between EVM and native balance (task 2.4.5).
//!
//! Transfers are IMMEDIATE (not queued like CoreWriter) because they only
//! move balances without affecting order book or staking state.
//!
//! FIX 5 (ECON-PF-04): Both deposit and withdraw now use a `WriteBatch`
//! so the debit and credit are applied atomically. A crash between two
//! individual writes could previously cause permanent fund loss or duplication.

use borsh::BorshDeserialize;
use torus_state::cf::{CF_ACCOUNTS, CF_NATIVE_BALANCES};
use torus_state::{AtomicWriteOp, StateBackend};
use torus_types::{Address, FixedPoint, U256};

use crate::error::CoreError;
use crate::position::NativeBalance;

/// KECCAK_EMPTY — code hash for EOA accounts with no code.
const KECCAK_EMPTY: [u8; 32] = [
    0xc5, 0xd2, 0x46, 0x01, 0x86, 0xf7, 0x23, 0x3c, 0x92, 0x7e, 0x7d, 0xb2, 0xdc, 0xc7, 0x03, 0xc0,
    0xe5, 0x00, 0xb6, 0x53, 0xca, 0x82, 0x27, 0x3b, 0x7b, 0xfa, 0xd8, 0x04, 0x5d, 0x85, 0xa4, 0x70,
];

pub struct Lockbox;

impl Lockbox {
    /// Transfer from EVM balance to native balance (immediate, atomic).
    ///
    /// Debits msg.sender's EVM balance, credits their native balance.
    /// Both sides use the same 8-decimal FixedPoint denomination.
    pub fn deposit_to_native(
        state: &impl StateBackend,
        trader: &Address,
        amount: FixedPoint,
    ) -> Result<(), CoreError> {
        if amount <= FixedPoint::ZERO {
            return Ok(());
        }

        let evm_amount = fp_to_u256(amount);

        // Read and verify EVM balance
        let evm_balance = get_evm_balance(state, trader)?;
        if evm_balance < evm_amount {
            return Err(CoreError::InsufficientEvmBalance {
                have: evm_balance,
                need: evm_amount,
            });
        }

        // Credit native balance
        let mut native_bal = get_native_balance(state, trader)?;
        native_bal.available = native_bal.available + amount;

        // Atomic write: debit EVM + credit native.
        let evm_data = build_evm_balance_update(state, trader, evm_balance - evm_amount)?;
        let native_data =
            borsh::to_vec(&native_bal).map_err(|e| CoreError::Borsh(e.to_string()))?;

        state.atomic_write(&[
            AtomicWriteOp::Put {
                cf: CF_ACCOUNTS,
                key: trader.as_slice(),
                value: &evm_data,
            },
            AtomicWriteOp::Put {
                cf: CF_NATIVE_BALANCES,
                key: trader.as_slice(),
                value: &native_data,
            },
        ])?;
        Ok(())
    }

    /// Transfer from native balance to EVM balance (immediate, atomic).
    ///
    /// Debits native balance, credits EVM balance.
    pub fn withdraw_from_native(
        state: &impl StateBackend,
        trader: &Address,
        amount: FixedPoint,
    ) -> Result<(), CoreError> {
        if amount <= FixedPoint::ZERO {
            return Ok(());
        }

        // Read and verify native balance
        let native_bal = get_native_balance(state, trader)?;
        if native_bal.available < amount {
            return Err(CoreError::InsufficientNativeBalance {
                have: native_bal.available,
                need: amount,
            });
        }

        // Debit native balance
        let mut updated_bal = native_bal;
        updated_bal.available = updated_bal.available - amount;

        // Credit EVM balance
        let evm_amount = fp_to_u256(amount);
        let evm_balance = get_evm_balance(state, trader)?;

        // Atomic write: debit native + credit EVM.
        let native_data =
            borsh::to_vec(&updated_bal).map_err(|e| CoreError::Borsh(e.to_string()))?;
        let evm_data = build_evm_balance_update(state, trader, evm_balance + evm_amount)?;

        state.atomic_write(&[
            AtomicWriteOp::Put {
                cf: CF_NATIVE_BALANCES,
                key: trader.as_slice(),
                value: &native_data,
            },
            AtomicWriteOp::Put {
                cf: CF_ACCOUNTS,
                key: trader.as_slice(),
                value: &evm_data,
            },
        ])?;
        Ok(())
    }

    /// FIX ECON-PF-17: Withdraw from sender's native balance to a specified EVM address.
    ///
    /// Debits `sender`'s native balance, credits `to`'s EVM balance.
    /// When `to == sender`, this is equivalent to `withdraw_from_native`.
    pub fn withdraw_from_native_to(
        state: &impl StateBackend,
        sender: &Address,
        to: &Address,
        amount: FixedPoint,
    ) -> Result<(), CoreError> {
        if amount <= FixedPoint::ZERO {
            return Ok(());
        }

        // Read and verify sender's native balance
        let native_bal = get_native_balance(state, sender)?;
        if native_bal.available < amount {
            return Err(CoreError::InsufficientNativeBalance {
                have: native_bal.available,
                need: amount,
            });
        }

        // Debit sender's native balance
        let mut updated_bal = native_bal;
        updated_bal.available = updated_bal.available - amount;

        // Credit recipient's EVM balance
        let evm_amount = fp_to_u256(amount);
        let evm_balance = get_evm_balance(state, to)?;

        // Atomic write: debit sender native + credit recipient EVM.
        let native_data =
            borsh::to_vec(&updated_bal).map_err(|e| CoreError::Borsh(e.to_string()))?;
        let evm_data = build_evm_balance_update(state, to, evm_balance + evm_amount)?;

        state.atomic_write(&[
            AtomicWriteOp::Put {
                cf: CF_NATIVE_BALANCES,
                key: sender.as_slice(),
                value: &native_data,
            },
            AtomicWriteOp::Put {
                cf: CF_ACCOUNTS,
                key: to.as_slice(),
                value: &evm_data,
            },
        ])?;
        Ok(())
    }
}

// ============================================================================
// EVM balance helpers (raw CF access — avoids revm dependency)
// ============================================================================

/// Read EVM balance from CF_ACCOUNTS (first 32 bytes of the 72-byte account record).
fn get_evm_balance(state: &impl StateBackend, address: &Address) -> Result<U256, CoreError> {
    match state.get_cf_raw(CF_ACCOUNTS, address.as_slice())? {
        Some(data) if data.len() >= 32 => Ok(U256::from_be_slice(&data[..32])),
        _ => Ok(U256::ZERO),
    }
}

/// Build a 72-byte EVM account record with the given balance, preserving nonce and code_hash.
/// Creates the account record if it doesn't exist.
fn build_evm_balance_update(
    state: &impl StateBackend,
    address: &Address,
    new_balance: U256,
) -> Result<Vec<u8>, CoreError> {
    let mut data = match state.get_cf_raw(CF_ACCOUNTS, address.as_slice())? {
        Some(d) if d.len() == 72 => d,
        _ => {
            // New account: balance(32) + nonce(8, zero) + code_hash(32, KECCAK_EMPTY)
            let mut d = vec![0u8; 72];
            d[40..72].copy_from_slice(&KECCAK_EMPTY);
            d
        }
    };
    data[..32].copy_from_slice(&new_balance.to_be_bytes::<32>());
    Ok(data)
}

// ============================================================================
// Native balance helpers
// ============================================================================

fn get_native_balance(
    state: &impl StateBackend,
    trader: &Address,
) -> Result<NativeBalance, CoreError> {
    match state.get_cf_raw(CF_NATIVE_BALANCES, trader.as_slice())? {
        Some(data) => Ok(
            NativeBalance::try_from_slice(&data).map_err(|e| CoreError::Borsh(e.to_string()))?
        ),
        None => Ok(NativeBalance::default()),
    }
}

// ============================================================================
// Conversion helpers
// ============================================================================

/// Convert FixedPoint (i128 raw) to U256. Both use the same 8-decimal scale.
pub fn fp_to_u256(fp: FixedPoint) -> U256 {
    if fp.raw() < 0 {
        U256::ZERO
    } else {
        U256::from(fp.raw() as u128)
    }
}

/// Convert U256 to FixedPoint. Returns None if value exceeds i128::MAX.
pub fn u256_to_fp(val: U256) -> Option<FixedPoint> {
    let max = U256::from(i128::MAX as u128);
    if val > max {
        None
    } else {
        let bytes = val.to_be_bytes::<32>();
        let raw = u128::from_be_bytes(bytes[16..32].try_into().unwrap());
        Some(FixedPoint::from_raw(raw as i128))
    }
}
