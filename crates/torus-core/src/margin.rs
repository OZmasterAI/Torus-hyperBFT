//! Margin engine — cross and isolated margin models (tasks 2.2.1–2.2.4).
//!
//! Provides initial and maintenance margin checks at both order submission
//! and match time (double-check). Uses margin tiers for leverage limits.

use torus_types::{Address, FixedPoint, MarketId};

use crate::error::CoreError;
use crate::position::{MarginType, Position, PositionManager};

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

// ============================================================================
// MarginEngine
// ============================================================================

pub struct MarginEngine;

impl MarginEngine {
    /// Check margin at order submission time.
    /// Returns Ok(()) if the trader has sufficient margin for the new order.
    pub fn check_initial_margin(
        positions: &PositionManager,
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
                    config,
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
    pub fn check_margin_at_match(
        positions: &PositionManager,
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

    /// Compute cross-margin equity: balance + sum(unrealized PnL across all positions).
    pub fn cross_margin_equity(
        positions: &PositionManager,
        trader: &Address,
        oracle_prices: &[(MarketId, FixedPoint)],
    ) -> Result<FixedPoint, CoreError> {
        let bal = positions.get_native_balance(trader)?;
        let all_pos = positions.positions_for_trader(trader)?;

        // FIX 3 (ECON-FIND-08): Subtract order_margin to avoid double-counting
        // funds committed to open orders as free equity.
        let mut equity = bal.available - bal.order_margin;
        for pos in &all_pos {
            if pos.margin_type != MarginType::Cross {
                continue;
            }
            if let Some(mark) = oracle_price_for(oracle_prices, pos.market_id) {
                equity = equity + pos.unrealized_pnl(mark);
            }
        }
        Ok(equity)
    }

    /// Total maintenance margin required across all cross-margin positions.
    pub fn total_maintenance_margin(
        positions: &PositionManager,
        trader: &Address,
        config: &MarketMarginConfig,
        oracle_prices: &[(MarketId, FixedPoint)],
    ) -> Result<FixedPoint, CoreError> {
        let all_pos = positions.positions_for_trader(trader)?;
        let mut total = FixedPoint::ZERO;

        for pos in &all_pos {
            if pos.margin_type != MarginType::Cross {
                continue;
            }
            if let Some(mark) = oracle_price_for(oracle_prices, pos.market_id) {
                let notional = pos.notional(mark);
                let max_lev = effective_max_leverage(&config.tiers, notional);
                let lev_fp = FixedPoint::from_raw(max_lev as i128 * FixedPoint::SCALE);
                let initial = notional / lev_fp;
                // Maintenance = initial * maintenance_factor_bps / 10000
                // FIX 4 (ECON-PF-10): Scale maint_num correctly as a FixedPoint value
                let maint_num = FixedPoint::from_raw(config.maintenance_factor_bps as i128 * FixedPoint::SCALE);
                let bps_denom = FixedPoint::from_raw(10_000 * FixedPoint::SCALE);
                total = total + initial * maint_num / bps_denom;
            }
        }
        Ok(total)
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
        let maint_num = FixedPoint::from_raw(config.maintenance_factor_bps as i128 * FixedPoint::SCALE);
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
        assert!(matches!(
            result,
            Err(CoreError::MaxLeverageExceeded { .. })
        ));
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

    // FIX 3: Cross-margin equity subtracts order_margin
    #[test]
    fn cross_margin_equity_deducts_order_margin() {
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
        let equity =
            MarginEngine::cross_margin_equity(&pm, &trader, &oracle_prices).unwrap();
        // equity = available - order_margin = 10000 - 3000 = 7000
        assert_eq!(equity, fp(7_000));
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

        let maint = MarginEngine::total_maintenance_margin(
            &pm,
            &trader,
            &config,
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
        assert!(MarginEngine::check_isolated_maintenance(&pos, fp(50_000), &config));

        // With less margin: 400 < 500 → should fail
        let pos2 = Position {
            isolated_margin: fp(400),
            ..pos.clone()
        };
        assert!(!MarginEngine::check_isolated_maintenance(&pos2, fp(50_000), &config));
    }
}
