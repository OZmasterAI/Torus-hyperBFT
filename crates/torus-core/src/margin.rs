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
#[derive(Clone, Debug)]
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

impl AccountView {
    /// Cross positions only (every production position is Cross).
    pub fn build<'t>(
        bal: &NativeBalance,
        positions: &[Position],
        mark: impl Fn(MarketId) -> Option<FixedPoint>,
        tiers: impl Fn(MarketId) -> Option<&'t [MarginTier]>,
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
            let px = position_price(pos, mark(pos.market_id));
            let n = pos.size.checked_mul(px).map_err(of)?;
            let diff = if pos.is_long { px - pos.entry_price } else { pos.entry_price - px };
            v.upnl = v.upnl.checked_add(diff.checked_mul(pos.size).map_err(of)?).map_err(of)?;
            v.notional = v.notional.checked_add(n).map_err(of)?;
            let t = tiers(pos.market_id);
            v.position_im = v.position_im.checked_add(order_initial_margin(t, n)).map_err(of)?;
            v.maintenance = v.maintenance.checked_add(maintenance_margin(t, n)).map_err(of)?;
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
#[derive(Clone, Debug)]
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
}
