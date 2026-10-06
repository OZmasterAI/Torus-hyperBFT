//! Funded-account check at native ingress (anti-spam item A).
//!
//! Hyperliquid rejects actions from an address that has never deposited
//! ("User or API Wallet does not exist. Must deposit before performing
//! actions"). Without an equivalent, any freshly generated key could sign
//! cancel-alls (which skip the admission limit) for free. This module is the
//! Torus rule: an action is admitted to the pool only if its sender (for a
//! session-signed action: the session OWNER, which is what every ingress path
//! resolves the sender to) holds at least `min_trs` whole TRS.
//!
//! NODE-LOCAL policy: it decides only what RPC ingress and gossip admission put
//! in THIS node's pool. Block validity, execution and the state hash never
//! read it, so mixed values (or stale reads) across validators cannot fork.
//! Every caller DA-mirrors a body before admission, so a block that references
//! an action this node refused still reconstructs.
//!
//! "Funded" (any one suffices, cheapest read first):
//! 1. perp collateral: `NativeBalance.available + order_margin` >= min — ONE
//!    point read, the path every active trader takes;
//! 2. spot: the EVM account balance >= min (in wei) — what `TransferToPerp`
//!    (the deposit) and staking draw from, so a new user can deposit and stake;
//! 3. any open position — margin locked in positions (cross positions carry no
//!    `isolated_margin`) must never stop an owner from cancelling or closing.
//!
//! A brand-new spammer key pays two point reads and one empty prefix seek.
//!
//! The DB the check reads can lag execution by a block or more: a deposit
//! becomes visible a little late (the client sees "not funded" briefly), and
//! an account that just went to zero may still pass for a block. Both are fine
//! for an anti-spam rule.
//!
//! Exempt action kinds — none of them can use perp collateral, and all are
//! non-cancels, so they never get cancel priority and stay subject to the
//! admission limit:
//! - validator duties: `SubmitOraclePrices`, `AttestStateHash`, `JailVote`,
//!   `UnjailSelf`, `RotateValidatorKey`, `UpdateCommission`, but ONLY from a
//!   registered validator (one point read of the validator table). Registration
//!   moves the validator's whole spot balance into self-stake, so a working
//!   validator can hold 0 spot and 0 perp; refusing its oracle prices or state
//!   attestations at gossip admission would hurt the chain. From any other key
//!   the executor rejects these kinds, so they are not a free path past this
//!   check. The same rule exempts them from the per-address limit (item B).
//! - stake exit, claims and governance votes: `Undelegate`, `ClaimRewards`,
//!   `ClaimUnbonded`, `Vote`. A staker's funds sit in staking, which this
//!   check does not read; they must always be able to get them back or vote.

use alloy_primitives::Address;
use torus_core::lockbox::fp_to_wei;
use torus_core::position::PositionManager;
use torus_state::cf::CF_STAKING_VALIDATORS;
use torus_state::StateDb;
use torus_types::{FixedPoint, NativeAction};

/// Default `TORUS_INGRESS_MIN_COLLATERAL` in whole TRS (HL charges 1 USDC to
/// activate an account).
pub const DEFAULT_INGRESS_MIN_COLLATERAL_TRS: u64 = 1;

/// Parse `TORUS_INGRESS_MIN_COLLATERAL` (whole TRS). `0` = check OFF; unset or
/// unparsable => [`DEFAULT_INGRESS_MIN_COLLATERAL_TRS`].
pub fn parse_ingress_min_collateral(raw: Option<String>) -> u64 {
    raw.and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_INGRESS_MIN_COLLATERAL_TRS)
}

/// The node's `TORUS_INGRESS_MIN_COLLATERAL` (read by `torus-node` at startup;
/// `MempoolConfig::default()` keeps the check off for library users/tests).
pub fn ingress_min_collateral() -> u64 {
    parse_ingress_min_collateral(std::env::var("TORUS_INGRESS_MIN_COLLATERAL").ok())
}

/// Stake-exit, claim and vote kinds, admitted without the funded check (see
/// the module docs).
pub fn is_exempt(action: &NativeAction) -> bool {
    matches!(
        action,
        NativeAction::Undelegate { .. }
            | NativeAction::ClaimRewards
            | NativeAction::ClaimUnbonded
            | NativeAction::Vote { .. }
    )
}

/// Validator-duty kinds. Exempt from the funded check and from the
/// per-address limit (item B) only when the sender is a registered validator
/// ([`is_registered_validator`]), or, for `SubmitOraclePrices` only, an
/// Active validator's registered oracle signer (`Mempool::is_duty_exempt`);
/// from anyone else the executor rejects them, so they get no exemption.
pub fn is_validator_duty(action: &NativeAction) -> bool {
    matches!(
        action,
        NativeAction::SubmitOraclePrices(_)
            | NativeAction::AttestStateHash { .. }
            | NativeAction::JailVote { .. }
            | NativeAction::UnjailSelf
            | NativeAction::RotateValidatorKey { .. }
            | NativeAction::UpdateCommission { .. }
    )
}

/// True when `sender` has a row in the validator table, in any status (a
/// jailed validator must still be able to unjail). One point read. A read
/// error counts as registered, as in [`is_funded`].
pub fn is_registered_validator(state: &StateDb, sender: &Address) -> bool {
    state
        .get_cf_raw(CF_STAKING_VALIDATORS, sender.as_slice())
        .map(|row| row.is_some())
        .unwrap_or(true)
}

/// A validator duty sent by a registered validator: skips items A and B.
pub fn is_validator_duty_of_validator(
    state: &StateDb,
    sender: &Address,
    action: &NativeAction,
) -> bool {
    is_validator_duty(action) && is_registered_validator(state, sender)
}

/// True when `sender` holds at least `min_trs` whole TRS (see the module
/// docs). A state read error counts as funded: a storage hiccup must not turn
/// into rejecting every honest user.
pub fn is_funded(state: &StateDb, sender: &Address, min_trs: u64) -> bool {
    let min = FixedPoint::from_raw((min_trs as i128).saturating_mul(FixedPoint::SCALE));
    let pm = PositionManager::new(state.clone());
    // 1. perp collateral (one point read).
    match pm.get_native_balance(sender) {
        Ok(bal) => {
            let total = bal.available.raw().saturating_add(bal.order_margin.raw());
            if total >= min.raw() {
                return true;
            }
        }
        Err(_) => return true,
    }
    // 2. spot balance (one point read).
    match state.get_account(sender) {
        Ok(Some(acct)) if acct.balance >= fp_to_wei(min) => return true,
        Ok(_) => {}
        Err(_) => return true,
    }
    // 3. any open position (prefix seek; empty for a fresh key).
    pm.positions_for_trader(sender)
        .map(|p| !p.is_empty())
        .unwrap_or(true)
}
