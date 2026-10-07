//! Margin engine — cross and isolated margin models (tasks 2.2.1–2.2.4).
//!
//! Provides initial and maintenance margin checks at both order submission
//! and match time (double-check). Uses margin tiers for leverage limits.

use torus_state::StateBackend;
use torus_types::{Address, FixedPoint, MarketId};

use crate::error::CoreError;
use crate::position::{MarginType, NativeBalance, Position, PositionManager};

// ============================================================================
// Margin Tiers — leverage limits by notional size
// ============================================================================

/// A single margin tier: positions with notional <= max_notional can use up to max_leverage.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MarginTier {
    pub max_notional: FixedPoint,
    pub max_leverage: u32,
}

/// Default margin tiers.
pub fn default_margin_tiers() -> Vec<MarginTier> {
    vec![
        MarginTier {
            max_notional: FixedPoint::from_raw(100_000 * FixedPoint::SCALE),
            max_leverage: 50,
        },
        MarginTier {
            max_notional: FixedPoint::from_raw(1_000_000 * FixedPoint::SCALE),
            max_leverage: 20,
        },
        MarginTier {
            max_notional: FixedPoint::from_raw(10_000_000 * FixedPoint::SCALE),
            max_leverage: 10,
        },
        MarginTier {
            max_notional: FixedPoint::from_raw(i128::MAX), // unlimited
            max_leverage: 5,
        },
    ]
}

/// Look up effective max leverage for a given notional.
pub fn effective_max_leverage(tiers: &[MarginTier], notional: FixedPoint) -> u32 {
    for tier in tiers {
        if notional <= tier.max_notional {
            return tier.max_leverage;
        }
    }
    1 // fallback: 1x
}

/// Max leverage of an order in a market without a [`MarketMarginConfig`].
pub const DEFAULT_ORDER_MAX_LEVERAGE: u32 = 20;

/// THE order initial-margin formula (s515): `notional / max leverage`, the
/// leverage taken from `tiers` for this notional ([`DEFAULT_ORDER_MAX_LEVERAGE`]
/// without a market config), truncating integer division. The executor's
/// placement reservation / releases and the book's match-time margin check
/// both use it, so they agree to the last raw unit. A 0x tier panics exactly
/// like the executor's historical formula (pinned by its integer-margin
/// tests); placement reserves through it first, so the book never sees one.
pub fn order_initial_margin(tiers: Option<&[MarginTier]>, notional: FixedPoint) -> FixedPoint {
    let lev = tiers.map_or(DEFAULT_ORDER_MAX_LEVERAGE, |t| {
        effective_max_leverage(t, notional)
    });
    notional
        .raw()
        .checked_div(i128::from(lev))
        .map(FixedPoint::from_raw)
        .ok_or(torus_types::ArithmeticError::DivisionByZero)
        .expect("FixedPoint division error")
}

/// Item 3 (HL): maintenance margin = half the initial margin at max leverage
/// (position-size tier), truncating — THE formula of liquidation.
pub fn maintenance_margin(tiers: Option<&[MarginTier]>, notional: FixedPoint) -> FixedPoint {
    FixedPoint::from_raw(order_initial_margin(tiers, notional).raw() / 2)
}

// ============================================================================
// Account-level margin (F1, s517) — Hyperliquid cross margin, computed
// ============================================================================

/// F1: the price a position is valued at — the market's mark, else its entry
/// price (s517 decision 2: UPnL 0 and IM at entry notional without a mark).
pub fn position_price(pos: &Position, mark: Option<FixedPoint>) -> FixedPoint {
    mark.unwrap_or(pos.entry_price)
}

/// s94 option 1: one fill's loss against the market's reference price (the
/// mark), as the match-time margin checks charge it. A fill is valued at its
/// own price by the IM checks, while the account is valued at the mark right
/// after it; a fill worse than the mark loses `loss` = `a × q` at once (`a` =
/// `price − mark` for a buy, `mark − price` for a sell; 0 at or better than
/// the mark). Part of that loss is tolerated, so fills near the mark are
/// checked exactly as before:
///
/// - `tol_open` = `IM(price × q_open) − MM(mark × q_open)`: the opening part's
///   own IM (which the checks already charge) above its maintenance at the
///   mark;
/// - the closing part's `IM(mark × q_close) − MM(mark × q_close)`;
/// - `charge` = `max(0, loss − both)`. `charge == 0`: the existing check and
///   commit, unchanged. `charge > 0`: the fill is checked on `loss` (see
///   [`crate::order_book`]) and `charge` is what it commits beyond the IM
///   delta.
///
/// `closing` = the part of `q` that reduces the trader's position. Each
/// tolerance is clamped at 0. `None` on overflow (the caller treats the fill
/// as not fitting).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MarkLoss {
    pub loss: FixedPoint,
    pub tol_open: FixedPoint,
    pub charge: FixedPoint,
}

impl MarkLoss {
    /// No loss (no mark, or a fill at or better than it).
    pub const NONE: Self = Self { loss: FixedPoint::ZERO, tol_open: FixedPoint::ZERO, charge: FixedPoint::ZERO };
}

pub fn mark_loss(
    tiers: Option<&[MarginTier]>,
    mark: FixedPoint,
    is_buy: bool,
    price: FixedPoint,
    q: FixedPoint,
    closing: FixedPoint,
) -> Option<MarkLoss> {
    let a = if is_buy { price.checked_sub(mark).ok()? } else { mark.checked_sub(price).ok()? };
    if a <= FixedPoint::ZERO || q <= FixedPoint::ZERO {
        return Some(MarkLoss::NONE);
    }
    let loss = a.checked_mul(q).ok()?;
    let closing = closing.min(q).max(FixedPoint::ZERO);
    let opening = q - closing;
    let tol = |n_im: FixedPoint, n_mm: FixedPoint| {
        (order_initial_margin(tiers, n_im) - maintenance_margin(tiers, n_mm)).max(FixedPoint::ZERO)
    };
    let tol_open = tol(price.checked_mul(opening).ok()?, mark.checked_mul(opening).ok()?);
    let n_close = mark.checked_mul(closing).ok()?;
    let tol_close = tol(n_close, n_close);
    let charge = (loss.checked_sub(tol_open).ok()?.checked_sub(tol_close).ok()?).max(FixedPoint::ZERO);
    Some(MarkLoss { loss, tol_open, charge })
}

/// F1: IM change when one market's (position [+ resting]) notional goes from
/// `before` to `after`, both at their own POSITION-size tier. < 0 = released.
pub fn im_delta(tiers: Option<&[MarginTier]>, before: FixedPoint, after: FixedPoint) -> FixedPoint {
    order_initial_margin(tiers, after) - order_initial_margin(tiers, before)
}

/// F1: margin need of placing `qty` on side `is_buy` against signed position
/// `signed` (valued at `px`), priced at `price`: the larger of the IM delta of
/// a complete fill (the closing part releases IM) and — for an order that can
/// rest — of resting its opening part. `<= 0`: it only reduces. `None` on
/// overflow. The book's match-time need is the same notional arithmetic.
pub fn placement_need(
    tiers: Option<&[MarginTier]>,
    signed: FixedPoint,
    px: FixedPoint,
    is_buy: bool,
    qty: FixedPoint,
    price: FixedPoint,
    can_rest: bool,
) -> Option<FixedPoint> {
    let size = if signed < FixedPoint::ZERO { -signed } else { signed };
    let closing = qty.min(crate::order_book::reduce_only_allowance(signed, is_buy));
    let opening_notional = price.checked_mul(qty - closing).ok()?;
    let before = size.checked_mul(px).ok()?;
    // Item 6 cut 2: nothing closes (the common case: an order on the
    // position's side, or flat) -> `left == before`, so the complete fill and
    // the rest are the same notional and one IM delta; IM(before) once. The
    // same values and overflow checks as `placement_need_reference`.
    let left = if closing == FixedPoint::ZERO {
        before
    } else {
        (size - closing).checked_mul(px).ok()?
    };
    let after_fill = left.checked_add(opening_notional).ok()?;
    let im_after_fill = order_initial_margin(tiers, after_fill);
    let im_before = order_initial_margin(tiers, before);
    let filled = im_after_fill - im_before;
    if !can_rest || closing == FixedPoint::ZERO {
        return Some(filled);
    }
    let rested = order_initial_margin(tiers, before.checked_add(opening_notional).ok()?) - im_before;
    Some(filled.max(rested))
}

/// [`placement_need`] before item 6 cut 2 (two full IM deltas): the test
/// oracle.
#[cfg(test)]
pub(crate) fn placement_need_reference(
    tiers: Option<&[MarginTier]>,
    signed: FixedPoint,
    px: FixedPoint,
    is_buy: bool,
    qty: FixedPoint,
    price: FixedPoint,
    can_rest: bool,
) -> Option<FixedPoint> {
    let size = if signed < FixedPoint::ZERO { -signed } else { signed };
    let closing = qty.min(crate::order_book::reduce_only_allowance(signed, is_buy));
    let opening_notional = price.checked_mul(qty - closing).ok()?;
    let before = size.checked_mul(px).ok()?;
    let left = (size - closing).checked_mul(px).ok()?;
    let filled = im_delta(tiers, before, left.checked_add(opening_notional).ok()?);
    if !can_rest {
        return Some(filled);
    }
    let rested = im_delta(tiers, before, before.checked_add(opening_notional).ok()?);
    Some(filled.max(rested))
}

/// F1: one trader's cross-margin account (never stored — decision 1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AccountView {
    pub available: FixedPoint,
    pub order_margin: FixedPoint,
    /// Σ unrealized PnL at [`position_price`].
    pub upnl: FixedPoint,
    /// Σ position IM at [`position_price`], each at its market's position-size tier.
    pub position_im: FixedPoint,
    /// Σ |size| × [`position_price`].
    pub notional: FixedPoint,
    /// Item 3: Σ [`maintenance_margin`] at [`position_price`], each market's tiers.
    pub maintenance: FixedPoint,
}

/// Item 6 C6b: one Cross position's contribution to each of
/// [`AccountView`]'s position sums — THE formula of [`AccountView::build`]
/// (also used by the executor's in-block partial re-value, which subtracts
/// and adds these terms). Exact integers: the sums do not depend on the
/// order the terms are added in, as long as no partial sum overflows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PositionTerms {
    pub upnl: FixedPoint,
    pub notional: FixedPoint,
    pub position_im: FixedPoint,
    pub maintenance: FixedPoint,
}

impl PositionTerms {
    /// The four terms, in [`AccountView`] field order (upnl, position_im,
    /// notional, maintenance).
    pub fn parts(&self) -> [FixedPoint; 4] {
        [self.upnl, self.position_im, self.notional, self.maintenance]
    }
}

/// [`PositionTerms`] of `pos` valued at `mark` (else entry, see
/// [`position_price`]) with its market's `tiers`. `Err` = overflow.
/// s100 item 2: UPnL = `n − cost_basis` (long) or `cost_basis − n` (short),
/// `n` = size × mark (one rounding); at mark 0 exactly `∓cost_basis`.
/// Without a mark it stays 0 (s517 decision 2).
pub fn position_terms(
    pos: &Position,
    mark: Option<FixedPoint>,
    tiers: Option<&[MarginTier]>,
) -> Result<PositionTerms, CoreError> {
    let of = |_| CoreError::Overflow("account margin overflows i128".into());
    let px = position_price(pos, mark);
    let n = pos.size.checked_mul(px).map_err(of)?;
    let upnl = match mark {
        None => FixedPoint::ZERO,
        Some(_) if pos.is_long => n.checked_sub(pos.cost_basis).map_err(of)?,
        Some(_) => pos.cost_basis.checked_sub(n).map_err(of)?,
    };
    Ok(PositionTerms {
        upnl,
        notional: n,
        position_im: order_initial_margin(tiers, n),
        maintenance: maintenance_margin(tiers, n),
    })
}

impl AccountView {
    /// Cross positions only (every production position is Cross).
    pub fn build<'t>(
        bal: &NativeBalance,
        positions: &[Position],
        mark: impl Fn(MarketId) -> Option<FixedPoint>,
        tiers: impl Fn(MarketId) -> Option<&'t [MarginTier]>,
    ) -> Result<Self, CoreError> {
        Self::build_with(bal, positions, mark, tiers, |_, _| {})
    }

    /// [`Self::build`], handing each Cross position's [`PositionTerms`] to
    /// `seen` (in order, before they are added). Item 6 C6b: the executor's
    /// sums cache records what its partial re-value needs through it.
    pub fn build_with<'t>(
        bal: &NativeBalance,
        positions: &[Position],
        mark: impl Fn(MarketId) -> Option<FixedPoint>,
        tiers: impl Fn(MarketId) -> Option<&'t [MarginTier]>,
        mut seen: impl FnMut(&Position, &PositionTerms),
    ) -> Result<Self, CoreError> {
        let of = |_| CoreError::Overflow("account margin overflows i128".into());
        let mut v = Self {
            available: bal.available,
            order_margin: bal.order_margin,
            upnl: FixedPoint::ZERO,
            position_im: FixedPoint::ZERO,
            notional: FixedPoint::ZERO,
            maintenance: FixedPoint::ZERO,
        };
        for pos in positions.iter().filter(|p| p.margin_type == MarginType::Cross) {
            let t = position_terms(pos, mark(pos.market_id), tiers(pos.market_id))?;
            seen(pos, &t);
            v.upnl = v.upnl.checked_add(t.upnl).map_err(of)?;
            v.notional = v.notional.checked_add(t.notional).map_err(of)?;
            v.position_im = v.position_im.checked_add(t.position_im).map_err(of)?;
            v.maintenance = v.maintenance.checked_add(t.maintenance).map_err(of)?;
        }
        Ok(v)
    }

    /// Collateral + UPnL.
    pub fn equity(&self) -> FixedPoint {
        self.available + self.order_margin + self.upnl
    }

    /// What the positions add to (or take from) free margin: UPnL − IM.
    pub fn pos_net(&self) -> FixedPoint {
        self.upnl - self.position_im
    }

    /// equity − (position IM + order margin) = available + [`Self::pos_net`].
    pub fn free(&self) -> FixedPoint {
        self.available + self.pos_net()
    }

    /// HL `transfer_margin_required`: max(Σ IM, 10% × Σ notional).
    pub fn transfer_required(&self) -> FixedPoint {
        self.position_im.max(FixedPoint::from_raw(self.notional.raw() / 10))
    }

    /// s517 decision 5, SAFE variant (user, s517): the cash bound
    /// `amount <= available` AND equity WITHOUT the resting orders'
    /// reservations (`available + upnl`) after the withdrawal covers
    /// [`Self::transfer_required`]. A negative `available` withdraws nothing.
    pub fn withdrawal_allowed(&self, amount: FixedPoint) -> bool {
        amount <= self.available
            && self.equity() - self.order_margin - amount >= self.transfer_required()
    }
}

// ============================================================================
// Market configuration for margin
// ============================================================================

/// Per-market margin configuration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MarketMarginConfig {
    pub market_id: MarketId,
    pub max_leverage: u32,
    /// Maintenance margin = initial margin * maintenance_factor_bps / 10000.
    /// Default: 5000 (50% of initial).
    pub maintenance_factor_bps: u32,
    pub tiers: Vec<MarginTier>,
}

impl MarketMarginConfig {
    pub fn new(market_id: MarketId, max_leverage: u32) -> Self {
        Self {
            market_id,
            max_leverage,
            maintenance_factor_bps: 5000, // 50%
            tiers: default_margin_tiers(),
        }
    }
}

/// Item 3 (F2, F8, D11, s517): the margin config of a `CF_NATIVE_MARKETS` row
/// (borsh `(base, quote, lot raw, tick raw, initial_margin raw percent)`, the
/// whole row): ONE flat tier at `max_leverage = max(1, floor(100 /
/// initial_margin %))` (u32, saturating), `maintenance_factor_bps` 5000.
/// An undecodable row (test fixtures) or `initial_margin <= 0` ⇒ `None` (the
/// default 20x applies).
pub fn market_margin_config(market_id: MarketId, row: &[u8]) -> Option<MarketMarginConfig> {
    let (_, _, _, _, im) =
        <(String, String, i128, i128, i128) as borsh::BorshDeserialize>::try_from_slice(row)
            .ok()?;
    if im <= 0 {
        return None;
    }
    let lev = ((100 * FixedPoint::SCALE) / im).clamp(1, i128::from(u32::MAX)) as u32;
    Some(MarketMarginConfig {
        market_id,
        max_leverage: lev,
        maintenance_factor_bps: 5000,
        tiers: vec![MarginTier { max_notional: FixedPoint::MAX, max_leverage: lev }],
    })
}

// ============================================================================
// MarginEngine
// ============================================================================

pub struct MarginEngine;

impl MarginEngine {
    /// Check margin at order submission time.
    /// Returns Ok(()) if the trader has sufficient margin for the new order.
    #[allow(clippy::too_many_arguments)]
    pub fn check_initial_margin(
        positions: &PositionManager<impl StateBackend>,
        trader: &Address,
        _market_id: MarketId,
        _is_buy: bool,
        price: FixedPoint,
        quantity: FixedPoint,
        leverage: u32,
        margin_type: MarginType,
        config: &MarketMarginConfig,
        oracle_prices: &[(MarketId, FixedPoint)],
    ) -> Result<(), CoreError> {
        let notional = price * quantity;

        // Tier check: leverage must not exceed tier limit for this notional
        let max_lev = effective_max_leverage(&config.tiers, notional);
        if leverage > max_lev {
            return Err(CoreError::MaxLeverageExceeded {
                leverage,
                max_leverage: max_lev,
                notional,
            });
        }

        // Initial margin = notional / leverage
        let lev_fp = FixedPoint::from_raw(leverage as i128 * FixedPoint::SCALE);
        let required_initial = notional / lev_fp;

        match margin_type {
            MarginType::Isolated => {
                // Isolated: check available balance >= initial margin
                let bal = positions.get_native_balance(trader)?;
                if bal.available < required_initial {
                    return Err(CoreError::InsufficientMargin {
                        required: required_initial,
                        available: bal.available,
                    });
                }
            }
            MarginType::Cross => {
                // Cross: available + sum(unrealized PnL) - sum(maintenance) >= initial
                let equity = Self::cross_margin_equity(positions, trader, oracle_prices)?;
                let maint = Self::total_maintenance_margin(
                    positions,
                    trader,
                    |_| Some(config.tiers.as_slice()),
                    oracle_prices,
                )?;
                let free_margin = equity - maint;
                if free_margin < required_initial {
                    return Err(CoreError::InsufficientMargin {
                        required: required_initial,
                        available: free_margin,
                    });
                }
            }
        }

        Ok(())
    }

    /// Double-check margin at match time. Same logic as initial but called during fill.
    /// If margin fails, the order should be cancelled (not executed).
    #[allow(clippy::too_many_arguments)]
    pub fn check_margin_at_match(
        positions: &PositionManager<impl StateBackend>,
        trader: &Address,
        market_id: MarketId,
        is_buy: bool,
        fill_price: FixedPoint,
        fill_qty: FixedPoint,
        leverage: u32,
        margin_type: MarginType,
        config: &MarketMarginConfig,
        oracle_prices: &[(MarketId, FixedPoint)],
    ) -> Result<(), CoreError> {
        // Same check as submission time, using fill price
        Self::check_initial_margin(
            positions,
            trader,
            market_id,
            is_buy,
            fill_price,
            fill_qty,
            leverage,
            margin_type,
            config,
            oracle_prices,
        )
    }

    /// F1: [`AccountView::equity`] — collateral (available + order margin) + UPnL
    /// at the mark, entry price without one.
    pub fn cross_margin_equity(
        positions: &PositionManager<impl StateBackend>,
        trader: &Address,
        oracle_prices: &[(MarketId, FixedPoint)],
    ) -> Result<FixedPoint, CoreError> {
        let bal = positions.get_native_balance(trader)?;
        let all_pos = positions.positions_for_trader(trader)?;
        let view = AccountView::build(
            &bal,
            &all_pos,
            |m| oracle_price_for(oracle_prices, m),
            |_| None,
        )?;
        Ok(view.equity())
    }

    /// Item 3 (F6): [`AccountView::maintenance`] — Σ [`maintenance_margin`] of
    /// cross positions at [`position_price`], each at ITS market's `tiers`
    /// (was: one config's tiers × `maintenance_factor_bps` for every position).
    pub fn total_maintenance_margin<'t>(
        positions: &PositionManager<impl StateBackend>,
        trader: &Address,
        tiers: impl Fn(MarketId) -> Option<&'t [MarginTier]>,
        oracle_prices: &[(MarketId, FixedPoint)],
    ) -> Result<FixedPoint, CoreError> {
        let bal = positions.get_native_balance(trader)?;
        let all_pos = positions.positions_for_trader(trader)?;
        let view = AccountView::build(
            &bal,
            &all_pos,
            |m| oracle_price_for(oracle_prices, m),
            tiers,
        )?;
        Ok(view.maintenance)
    }

    /// Check maintenance margin for a specific isolated position.
    pub fn check_isolated_maintenance(
        pos: &Position,
        mark_price: FixedPoint,
        config: &MarketMarginConfig,
    ) -> bool {
        let notional = pos.notional(mark_price);
        let max_lev = effective_max_leverage(&config.tiers, notional);
        let lev_fp = FixedPoint::from_raw(max_lev as i128 * FixedPoint::SCALE);
        let initial_margin = notional / lev_fp;
        // FIX 4 (ECON-PF-10): Scale maint_num correctly as a FixedPoint value
        let maint_num =
            FixedPoint::from_raw(config.maintenance_factor_bps as i128 * FixedPoint::SCALE);
        let bps_denom = FixedPoint::from_raw(10_000 * FixedPoint::SCALE);
        let maintenance = initial_margin * maint_num / bps_denom;

        let equity = pos.isolated_margin + pos.unrealized_pnl(mark_price);
        equity >= maintenance
    }
}

/// Look up oracle price for a market.
fn oracle_price_for(prices: &[(MarketId, FixedPoint)], market_id: MarketId) -> Option<FixedPoint> {
    prices
        .iter()
        .find(|(mid, _)| *mid == market_id)
        .map(|(_, p)| *p)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::position::NativeBalance;
    use torus_state::StateDb;

    fn setup() -> (tempfile::TempDir, PositionManager) {
        let dir = tempfile::tempdir().unwrap();
        let db = StateDb::open(dir.path()).unwrap();
        (dir, PositionManager::new(db))
    }

    fn addr(n: u8) -> Address {
        Address::new([n; 20])
    }

    fn fp(v: i64) -> FixedPoint {
        FixedPoint::from_raw(v as i128 * FixedPoint::SCALE)
    }

    fn cents(v: i64) -> FixedPoint {
        FixedPoint::from_raw(v as i128 * (FixedPoint::SCALE / 100))
    }

    /// s94 option 1: mark 100, q 10, default 20x (IM 5%, MM 2.5%). Opening
    /// buy at p: loss 10 (p − 100), tolerance IM(10p) − MM(1,000) = p/2 −
    /// 25: the charge starts above 102.63 (the probe's "healthy" bound: an
    /// account funded at the IM stays >= MM). Sell at p: loss 10 (100 − p),
    /// tolerance p/2 − 25: from below 97.62. At or better than the mark: 0.
    /// Closing part: tolerance IM − MM of its notional at the mark (25).
    #[test]
    fn mark_loss_charges_only_beyond_the_fills_own_margin() {
        let m = fp(100);
        let q = fp(10);
        // (is_buy, price in cents, closing, loss, tol_open, charge) in cents
        let cases = [
            (true, 10_000, 0, 0, 0, 0),
            (true, 9_000, 0, 0, 0, 0),
            (false, 11_000, 0, 0, 0, 0),
            (true, 10_200, 0, 2_000, 2_600, 0),
            (true, 10_262, 0, 2_620, 2_631, 0),
            (true, 10_264, 0, 2_640, 2_632, 8),
            (true, 10_300, 0, 3_000, 2_650, 350),
            (true, 12_000, 0, 20_000, 3_500, 16_500),
            (true, 20_000, 0, 100_000, 7_500, 92_500),
            (false, 9_800, 0, 2_000, 2_400, 0),
            (false, 9_700, 0, 3_000, 2_350, 650),
            (false, 5_000, 0, 50_000, 0, 50_000),
            // Closing 10 of a short (buy) at 103: tolerance 50 − 25 = 25.
            (true, 10_300, 10, 3_000, 0, 500),
            // Closing 4, opening 6 at 103: 30.9 − 15 + 20 − 10.
            (true, 10_300, 4, 3_000, 1_590, 410),
        ];
        for (is_buy, px, closing, loss, tol_open, charge) in cases {
            let got = mark_loss(None, m, is_buy, cents(px), q, fp(closing)).unwrap();
            let want = MarkLoss { loss: cents(loss), tol_open: cents(tol_open), charge: cents(charge) };
            assert_eq!(got, want, "is_buy={is_buy} price={px} closing={closing}");
        }
        assert_eq!(mark_loss(None, m, true, FixedPoint::MAX, q, FixedPoint::ZERO), None, "overflow");
    }

    #[test]
    fn cross_margin_check_passes() {
        let (_dir, pm) = setup();
        let trader = addr(1);

        // Give trader 10,000 balance
        pm.put_native_balance(
            &trader,
            &NativeBalance {
                available: fp(10_000),
                order_margin: FixedPoint::ZERO,
            },
        )
        .unwrap();

        let config = MarketMarginConfig::new(1, 20);
        let oracle_prices = vec![(1u64, fp(50_000))];

        // Buy 1 BTC at 50,000 with 10x leverage -> initial margin = 5,000
        MarginEngine::check_initial_margin(
            &pm,
            &trader,
            1,
            true,
            fp(50_000),
            fp(1),
            10,
            MarginType::Cross,
            &config,
            &oracle_prices,
        )
        .unwrap();
    }

    #[test]
    fn cross_margin_check_fails_insufficient() {
        let (_dir, pm) = setup();
        let trader = addr(1);

        pm.put_native_balance(
            &trader,
            &NativeBalance {
                available: fp(1_000),
                order_margin: FixedPoint::ZERO,
            },
        )
        .unwrap();

        let config = MarketMarginConfig::new(1, 20);
        let oracle_prices = vec![(1u64, fp(50_000))];

        // Buy 1 BTC at 50,000 with 10x -> need 5,000, have 1,000
        let result = MarginEngine::check_initial_margin(
            &pm,
            &trader,
            1,
            true,
            fp(50_000),
            fp(1),
            10,
            MarginType::Cross,
            &config,
            &oracle_prices,
        );
        assert!(matches!(result, Err(CoreError::InsufficientMargin { .. })));
    }

    #[test]
    fn tier_system_limits_leverage() {
        let (_dir, pm) = setup();
        let trader = addr(1);

        pm.put_native_balance(
            &trader,
            &NativeBalance {
                available: fp(1_000_000),
                order_margin: FixedPoint::ZERO,
            },
        )
        .unwrap();

        let config = MarketMarginConfig::new(1, 50);
        let oracle_prices = vec![(1u64, fp(50_000))];

        // Notional = 2M (40 BTC * 50k) -> tier allows max 20x, request 50x -> fail
        let result = MarginEngine::check_initial_margin(
            &pm,
            &trader,
            1,
            true,
            fp(50_000),
            fp(40),
            50,
            MarginType::Cross,
            &config,
            &oracle_prices,
        );
        assert!(matches!(result, Err(CoreError::MaxLeverageExceeded { .. })));
    }

    #[test]
    fn isolated_margin_check() {
        let (_dir, pm) = setup();
        let trader = addr(1);

        pm.put_native_balance(
            &trader,
            &NativeBalance {
                available: fp(5_000),
                order_margin: FixedPoint::ZERO,
            },
        )
        .unwrap();

        let config = MarketMarginConfig::new(1, 50);
        let oracle_prices = vec![(1u64, fp(50_000))];

        // Isolated: just check available >= initial margin
        MarginEngine::check_initial_margin(
            &pm,
            &trader,
            1,
            true,
            fp(50_000),
            fp(1),
            20,
            MarginType::Isolated,
            &config,
            &oracle_prices,
        )
        .unwrap();
    }

    #[test]
    fn effective_leverage_tiers() {
        let tiers = default_margin_tiers();
        assert_eq!(effective_max_leverage(&tiers, fp(50_000)), 50);
        assert_eq!(effective_max_leverage(&tiers, fp(500_000)), 20);
        assert_eq!(effective_max_leverage(&tiers, fp(5_000_000)), 10);
        assert_eq!(effective_max_leverage(&tiers, fp(50_000_000)), 5);
    }

    // F1 (s517): equity counts collateral once — `available` is already net of
    // `order_margin` (every reservation moves it out of `available`), so the
    // old FIX 3 subtraction counted reserved collateral as a loss.
    #[test]
    fn cross_margin_equity_counts_order_margin_once() {
        let (_dir, pm) = setup();
        let trader = addr(1);

        pm.put_native_balance(
            &trader,
            &NativeBalance {
                available: fp(10_000),
                order_margin: fp(3_000),
            },
        )
        .unwrap();

        let oracle_prices: Vec<(u64, FixedPoint)> = vec![];
        let equity = MarginEngine::cross_margin_equity(&pm, &trader, &oracle_prices).unwrap();
        // equity = available + order_margin = 10000 + 3000 = 13000
        assert_eq!(equity, fp(13_000));
    }

    // FIX 4: Maintenance margin BPS scaling is correct
    #[test]
    fn maintenance_margin_correct_scaling() {
        let (_dir, pm) = setup();
        let trader = addr(1);

        pm.put_native_balance(
            &trader,
            &NativeBalance {
                available: fp(100_000),
                order_margin: FixedPoint::ZERO,
            },
        )
        .unwrap();

        // Position: 1 BTC at $50,000
        pm.put_position(&Position {
            trader,
            market_id: 1,
            is_long: true,
            size: fp(1),
            entry_price: fp(50_000),
            cost_basis: fp(50_000) * fp(1),
            realized_pnl: FixedPoint::ZERO,
            isolated_margin: FixedPoint::ZERO,
            margin_type: MarginType::Cross,
        })
        .unwrap();

        let config = MarketMarginConfig::new(1, 50);
        // maintenance_factor_bps = 5000 (50%)
        let oracle_prices = vec![(1u64, fp(50_000))];

        // Item 3: per-market tiers closure (signature only; value unchanged).
        let maint = MarginEngine::total_maintenance_margin(
            &pm,
            &trader,
            |_| Some(config.tiers.as_slice()),
            &oracle_prices,
        )
        .unwrap();

        // Notional = 1 * 50000 = 50000
        // Tier: 50000 <= 100000 → max_leverage = 50
        // Initial = 50000 / 50 = 1000
        // Maintenance = 1000 * 5000 / 10000 = 500
        assert_eq!(maint, fp(500));
    }

    // FIX 4: Isolated maintenance check with correct BPS
    #[test]
    fn isolated_maintenance_correct_bps() {
        let pos = Position {
            trader: addr(1),
            market_id: 1,
            is_long: true,
            size: fp(1),
            entry_price: fp(50_000),
            cost_basis: fp(50_000) * fp(1),
            realized_pnl: FixedPoint::ZERO,
            isolated_margin: fp(600), // above 500 maintenance
            margin_type: MarginType::Isolated,
        };
        let config = MarketMarginConfig::new(1, 50);

        // At mark price 50000: maintenance = 500, isolated_margin + upnl = 600 + 0 = 600
        assert!(MarginEngine::check_isolated_maintenance(
            &pos,
            fp(50_000),
            &config
        ));

        // With less margin: 400 < 500 → should fail
        let pos2 = Position {
            isolated_margin: fp(400),
            ..pos.clone()
        };
        assert!(!MarginEngine::check_isolated_maintenance(
            &pos2,
            fp(50_000),
            &config
        ));
    }

    /// Item 6 cut 2: `placement_need` (one IM delta when nothing closes,
    /// IM(before) once) equals the pre-cut formula on random inputs: every
    /// side vs position sign, flat / long / short, closing none / part /
    /// all / more, rest or not, prices and sizes up to overflow, no tiers /
    /// default tiers / a one-tier list.
    #[test]
    fn placement_need_matches_the_reference_formula() {
        struct Rng(u64);
        impl Rng {
            fn next(&mut self, n: u64) -> u64 {
                self.0 ^= self.0 << 13;
                self.0 ^= self.0 >> 7;
                self.0 ^= self.0 << 17;
                self.0 % n
            }
            fn val(&mut self, zero_ok: bool) -> FixedPoint {
                const SCALES: [i128; 5] = [
                    1,
                    FixedPoint::SCALE,
                    1_000 * FixedPoint::SCALE,
                    1_000_000_000 * FixedPoint::SCALE,
                    i128::MAX / 3,
                ];
                if zero_ok && self.next(5) == 0 {
                    return FixedPoint::ZERO;
                }
                let s = SCALES[self.next(5) as usize];
                FixedPoint::from_raw((self.next(1_000_000) as i128).saturating_mul((s / 1_000).max(1)).max(1))
            }
        }
        let mut rng = Rng(0x0dd1_5eed_c0ff_ee11);
        let defaults = default_margin_tiers();
        let one = [MarginTier { max_notional: FixedPoint::from_raw(i128::MAX), max_leverage: 3 }];
        let tier_sets: [Option<&[MarginTier]>; 3] = [None, Some(&defaults), Some(&one)];
        let mut overflows = 0;
        let mut closing_cases = 0;
        for i in 0..200_000 {
            let tiers = tier_sets[i % 3];
            let size = rng.val(true);
            let signed = if rng.next(2) == 0 { size } else { -size };
            let is_buy = rng.next(2) == 0;
            // Often exactly the position (closing all) or a part of it.
            let qty = match rng.next(4) {
                0 => size.max(FixedPoint::from_raw(1)),
                1 => FixedPoint::from_raw((size.raw() / 2).max(1)),
                _ => rng.val(false),
            };
            let (px, price, can_rest) = (rng.val(true), rng.val(false), rng.next(2) == 0);
            let got = placement_need(tiers, signed, px, is_buy, qty, price, can_rest);
            let want = placement_need_reference(tiers, signed, px, is_buy, qty, price, can_rest);
            assert_eq!(got, want, "{signed:?} {px:?} {is_buy} {qty:?} {price:?} {can_rest}");
            overflows += usize::from(want.is_none());
            closing_cases += usize::from(
                qty.min(crate::order_book::reduce_only_allowance(signed, is_buy)) > FixedPoint::ZERO,
            );
        }
        assert!(overflows > 1_000 && closing_cases > 10_000, "{overflows} {closing_cases}");
    }
}
