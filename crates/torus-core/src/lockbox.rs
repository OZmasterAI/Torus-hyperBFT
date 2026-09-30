//! Lockbox — bidirectional asset transfer between EVM and native balance (task 2.4.5).
//!
//! # Units (Hyperliquid model)
//!
//! The native ledger (`CF_NATIVE_BALANCES`) keeps 8-decimal [`FixedPoint`]
//! amounts (HyperCore `weiDecimals` = 8). The EVM side (`CF_ACCOUNTS` balances,
//! tx `value`, gas) is 18-decimal wei, like HyperEVM's native gas token.
//! Crossing the boundary scales by `10^EVM_EXTRA_WEI_DECIMALS`:
//!
//! * native → EVM: `wei = raw × 10^10` (exact, never lossy);
//! * EVM → native: `raw = floor(wei / 10^10)`; the remainder (`< 10^10` wei,
//!   i.e. less than one native unit) is BURNED — removed from supply, not
//!   refunded — exactly like HyperCore's "non-round amount is burned".
//!
//! # Two entry paths
//!
//! * **Native actions** (`TransferToPerp` / `TransferToSpot` / `Withdraw`) call
//!   [`Lockbox::deposit_to_native`] / [`Lockbox::withdraw_from_native`] /
//!   [`Lockbox::withdraw_from_native_to`] from the NATIVE executor, which runs
//!   after the block's EVM bundle is committed and seeded into the native overlay,
//!   so the raw `CF_ACCOUNTS` read-modify-write here sees fresh EVM state. Their
//!   amounts are native 8-decimal units, so this path never produces dust.
//! * **EVM precompile 0x0820** NEVER calls these (EVM-PF-05: a mid-EVM raw
//!   `CF_ACCOUNTS` write is clobbered by revm's cached account at bundle commit).
//!   The precompile only enqueues a CoreWriter action; the EVM value leg is moved
//!   by revm itself, and the native leg is applied next block by
//!   `NativeExecutor::drain_core_writer` via [`Lockbox::credit_native`] (deposit)
//!   or [`Lockbox::withdraw_from_native`] (withdraw).
//!
//! FIX 5 (ECON-PF-04): the two-sided functions use one atomic write so the debit
//! and credit land together (a crash between two writes could lose or duplicate
//! funds).

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

/// Extra decimals the EVM side carries over the native ledger (18 − 8).
pub const EVM_EXTRA_WEI_DECIMALS: u32 = 10;

/// Wei per native `FixedPoint` raw unit (`10^EVM_EXTRA_WEI_DECIMALS`).
pub const WEI_PER_NATIVE_UNIT: u128 = 10_000_000_000;

pub struct Lockbox;

impl Lockbox {
    /// Native action `TransferToPerp`: EVM → native (immediate, atomic).
    ///
    /// Debits `amount × 10^10` wei from the trader's EVM balance and credits
    /// `amount` to their native balance. `amount` is in native units, so there is
    /// no dust on this path.
    pub fn deposit_to_native(
        state: &impl StateBackend,
        trader: &Address,
        amount: FixedPoint,
    ) -> Result<(), CoreError> {
        if amount <= FixedPoint::ZERO {
            return Ok(());
        }

        let evm_amount = fp_to_wei(amount);

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
        native_bal.available = checked_credit(native_bal.available, amount)?;

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

    /// Native leg of an EVM-side deposit (precompile 0x0820 `depositToNative`),
    /// applied next block by `drain_core_writer`.
    ///
    /// Credits `amount` to the trader's native balance ONLY: the EVM value was
    /// already removed from supply by revm inside the depositing transaction, so
    /// this must never touch `CF_ACCOUNTS`. Not reachable from any user-signed
    /// native action — only from a queued action the lockbox precompile wrote.
    pub fn credit_native(
        state: &impl StateBackend,
        trader: &Address,
        amount: FixedPoint,
    ) -> Result<(), CoreError> {
        if amount <= FixedPoint::ZERO {
            return Ok(());
        }
        let mut native_bal = get_native_balance(state, trader)?;
        native_bal.available = checked_credit(native_bal.available, amount)?;
        let native_data =
            borsh::to_vec(&native_bal).map_err(|e| CoreError::Borsh(e.to_string()))?;
        state.put_cf_raw(CF_NATIVE_BALANCES, trader.as_slice(), &native_data)?;
        Ok(())
    }

    /// Native → EVM (immediate, atomic).
    ///
    /// Debits `amount` from the native balance, credits `amount × 10^10` wei to
    /// the EVM balance. Used by the native action `TransferToSpot` and by the
    /// next-block drain of a queued precompile `withdrawFromNative`.
    pub fn withdraw_from_native(
        state: &impl StateBackend,
        trader: &Address,
        amount: FixedPoint,
    ) -> Result<(), CoreError> {
        Self::withdraw_from_native_to(state, trader, trader, amount)
    }

    /// FIX ECON-PF-17: Withdraw from sender's native balance to a specified EVM address.
    ///
    /// Debits `sender`'s native balance by `amount`, credits `to`'s EVM balance by
    /// `amount × 10^10` wei. When `to == sender`, this is `withdraw_from_native`.
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
        updated_bal.available -= amount;

        // Credit recipient's EVM balance
        let evm_amount = fp_to_wei(amount);
        let evm_balance = get_evm_balance(state, to)?;
        let new_evm_balance = evm_balance
            .checked_add(evm_amount)
            .ok_or_else(|| CoreError::Overflow("lockbox EVM credit overflows U256".into()))?;

        // Atomic write: debit sender native + credit recipient EVM.
        let native_data =
            borsh::to_vec(&updated_bal).map_err(|e| CoreError::Borsh(e.to_string()))?;
        let evm_data = build_evm_balance_update(state, to, new_evm_balance)?;

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

fn checked_credit(balance: FixedPoint, amount: FixedPoint) -> Result<FixedPoint, CoreError> {
    balance
        .checked_add(amount)
        .map_err(|_| CoreError::Overflow("lockbox native credit overflows i128".into()))
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

/// Native 8-decimal amount → EVM wei (`raw × 10^10`). Exact: `i128::MAX × 10^10`
/// is far below `U256::MAX`. Negative amounts map to zero.
pub fn fp_to_wei(fp: FixedPoint) -> U256 {
    fp_to_u256(fp) * U256::from(WEI_PER_NATIVE_UNIT)
}

/// EVM wei → native 8-decimal amount, rounding DOWN. Returns
/// `(native_amount, dust_wei)` where `dust_wei < 10^10` is the non-round remainder
/// the caller burns. `None` if the native amount would exceed `i128::MAX`.
pub fn wei_to_fp_floor(wei: U256) -> Option<(FixedPoint, U256)> {
    let unit = U256::from(WEI_PER_NATIVE_UNIT);
    let (whole, dust) = (wei / unit, wei % unit);
    u256_to_fp(whole).map(|fp| (fp, dust))
}

/// Carry a native 8-decimal `FixedPoint` in a `U256` field (raw, UNSCALED).
///
/// NOT an EVM unit conversion — use [`fp_to_wei`] for that. This is the encoding
/// of the `U256 amount` fields of native actions (`TransferToPerp`,
/// `TransferToSpot`, `Withdraw`, `Delegate`, ...), which are native 8-decimal
/// units. Negative amounts map to zero.
pub fn fp_to_u256(fp: FixedPoint) -> U256 {
    if fp.raw() < 0 {
        U256::ZERO
    } else {
        U256::from(fp.raw() as u128)
    }
}

/// Inverse of [`fp_to_u256`] (raw, UNSCALED — NOT a wei conversion; see
/// [`wei_to_fp_floor`]). Returns `None` if the value exceeds `i128::MAX`.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wei_round_trip_is_exact_for_native_amounts() {
        let five = FixedPoint::from_raw(5 * FixedPoint::SCALE);
        let wei = fp_to_wei(five);
        assert_eq!(wei, U256::from(5_000_000_000_000_000_000u128)); // 5e18
        assert_eq!(wei_to_fp_floor(wei), Some((five, U256::ZERO)));
    }

    #[test]
    fn wei_to_fp_floors_and_reports_dust() {
        let wei = U256::from(3 * WEI_PER_NATIVE_UNIT + 9_999_999_999);
        assert_eq!(
            wei_to_fp_floor(wei),
            Some((FixedPoint::from_raw(3), U256::from(9_999_999_999u64)))
        );
        assert_eq!(
            wei_to_fp_floor(U256::from(WEI_PER_NATIVE_UNIT - 1)),
            Some((FixedPoint::ZERO, U256::from(WEI_PER_NATIVE_UNIT - 1)))
        );
    }

    #[test]
    fn wei_to_fp_rejects_native_overflow() {
        let max_ok = U256::from(i128::MAX as u128) * U256::from(WEI_PER_NATIVE_UNIT);
        assert!(wei_to_fp_floor(max_ok).is_some());
        assert!(wei_to_fp_floor(max_ok + U256::from(WEI_PER_NATIVE_UNIT)).is_none());
        assert_eq!(fp_to_wei(FixedPoint::from_raw(i128::MAX)), max_ok);
    }

    #[test]
    fn negative_native_amount_maps_to_zero_wei() {
        assert_eq!(fp_to_wei(FixedPoint::from_raw(-1)), U256::ZERO);
    }
}
