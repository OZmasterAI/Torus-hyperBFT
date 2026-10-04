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
//! Exempt action kinds (no check) — none of them can use perp collateral, and
//! all are non-cancels, so they never get cancel priority and stay subject to
//! the admission limit:
//! - validator duties: `SubmitOraclePrices`, `AttestStateHash`, `JailVote`,
//!   `UnjailSelf`, `RotateValidatorKey`, `UpdateCommission`. Registration
//!   moves the validator's whole spot balance into self-stake, so a working
//!   validator can hold 0 spot and 0 perp; refusing its oracle prices or state
//!   attestations at gossip admission would hurt the chain.
//! - stake exit, claims and governance votes: `Undelegate`, `ClaimRewards`,
//!   `ClaimUnbonded`, `Vote`. A staker's funds sit in staking, which this
//!   check does not read; they must always be able to get them back or vote.

use alloy_primitives::Address;
use torus_core::lockbox::fp_to_wei;
use torus_core::position::PositionManager;
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

/// Action kinds admitted without the funded check (see the module docs).
pub fn is_exempt(action: &NativeAction) -> bool {
    matches!(
        action,
        NativeAction::SubmitOraclePrices(_)
            | NativeAction::AttestStateHash { .. }
            | NativeAction::JailVote { .. }
            | NativeAction::UnjailSelf
            | NativeAction::RotateValidatorKey { .. }
            | NativeAction::UpdateCommission { .. }
            | NativeAction::Undelegate { .. }
            | NativeAction::ClaimRewards
            | NativeAction::ClaimUnbonded
            | NativeAction::Vote { .. }
    )
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
