//! Liquidation engine — continuous liquidation checks, force close, ADL, socialized loss.
//!
//! Tasks 2.3.1–2.3.4: check_liquidations, execute_liquidation, auto_deleverage, socialize_loss.

use torus_state::cf::CF_NATIVE_BALANCES;
use torus_state::StateDb;
use torus_types::{Address, FixedPoint, MarketId};

use crate::error::CoreError;
use crate::margin::{effective_max_leverage, MarginEngine, MarketMarginConfig};
use crate::position::{MarginType, Position, PositionManager};

// ============================================================================
// Types
// ============================================================================

/// A position flagged for liquidation.
#[derive(Clone, Debug)]
pub struct Liquidation {
    pub trader: Address,
    pub market_id: MarketId,
    pub position: Position,
    pub shortfall: FixedPoint,
    pub margin_type: MarginType,
}

/// Result of executing a liquidation.
#[derive(Clone, Debug)]
pub struct LiquidationResult {
    pub trader: Address,
    pub market_id: MarketId,
    pub size: FixedPoint,
    pub price: FixedPoint,
    pub pnl: FixedPoint,
    pub method: LiquidationMethod,
    pub remaining_deficit: FixedPoint,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LiquidationMethod {
    ForceClose,
    Adl,
    SocializedLoss,
}

/// Result of auto-deleveraging one counterparty.
#[derive(Clone, Debug)]
pub struct AdlResult {
    pub trader: Address,
    pub size_reduced: FixedPoint,
    pub price: FixedPoint,
    pub realized_pnl: FixedPoint,
}

// ============================================================================
// Insurance fund key
// ============================================================================

const INSURANCE_FUND_KEY: &[u8] = b"insurance_fund";

/// Liquidation penalty rate in basis points (e.g., 250 = 2.5%)
const DEFAULT_LIQUIDATION_PENALTY_BPS: i128 = 250;

// ============================================================================
// LiquidationEngine
// ============================================================================

pub struct LiquidationEngine;

impl LiquidationEngine {
    // 2.3.1: Scan all positions for liquidation candidates.
    pub fn check_liquidations(
        positions: &PositionManager,
        traders: &[Address],
        config: &MarketMarginConfig,
        oracle_prices: &[(MarketId, FixedPoint)],
    ) -> Result<Vec<Liquidation>, CoreError> {
        let mut liquidations = Vec::new();

        for trader in traders {
            let all_pos = positions.positions_for_trader(trader)?;
            if all_pos.is_empty() {
                continue;
            }

            // Check cross-margin positions as a group
            let has_cross = all_pos.iter().any(|p| p.margin_type == MarginType::Cross);
            if has_cross {
                if let Some(liq) = Self::check_cross_liquidation(
                    positions,
                    trader,
                    &all_pos,
                    config,
                    oracle_prices,
                )? {
                    liquidations.extend(liq);
                }
            }

            // Check isolated positions individually
            for pos in &all_pos {
                if pos.margin_type == MarginType::Isolated {
                    if let Some(liq) = Self::check_isolated_liquidation(pos, config, oracle_prices)?
                    {
                        liquidations.push(liq);
                    }
                }
            }
        }

        Ok(liquidations)
    }

    /// Check cross-margin account for liquidation.
    fn check_cross_liquidation(
        positions: &PositionManager,
        trader: &Address,
        all_pos: &[Position],
        config: &MarketMarginConfig,
        oracle_prices: &[(MarketId, FixedPoint)],
    ) -> Result<Option<Vec<Liquidation>>, CoreError> {
        let equity = MarginEngine::cross_margin_equity(positions, trader, oracle_prices)?;
        let maintenance =
            MarginEngine::total_maintenance_margin(positions, trader, config, oracle_prices)?;

        if equity >= maintenance {
            return Ok(None);
        }

        let shortfall = maintenance - equity;
        let mut liquidations = Vec::new();

        // Flag all cross-margin positions for liquidation
        for pos in all_pos {
            if pos.margin_type == MarginType::Cross {
                liquidations.push(Liquidation {
                    trader: *trader,
                    market_id: pos.market_id,
                    position: pos.clone(),
                    shortfall,
                    margin_type: MarginType::Cross,
                });
            }
        }

        Ok(Some(liquidations))
    }

    /// Check a single isolated position for liquidation.
    fn check_isolated_liquidation(
        pos: &Position,
        config: &MarketMarginConfig,
        oracle_prices: &[(MarketId, FixedPoint)],
    ) -> Result<Option<Liquidation>, CoreError> {
        let mark = oracle_prices
            .iter()
            .find(|(mid, _)| *mid == pos.market_id)
            .map(|(_, p)| *p);

        // FIX 5: Reject zero/negative oracle prices (ECON-PF-06)
        let mark = match mark {
            Some(p) if p > FixedPoint::ZERO => p,
            _ => return Ok(None), // No valid oracle price, skip
        };

        if MarginEngine::check_isolated_maintenance(pos, mark, config) {
            return Ok(None); // Healthy
        }

        // Equity for isolated = isolated_margin + unrealized_pnl
        let equity = pos.isolated_margin + pos.unrealized_pnl(mark);
        let notional = pos.notional(mark);
        let max_lev = effective_max_leverage(&config.tiers, notional);
        let lev_fp = FixedPoint::from_raw(max_lev as i128 * FixedPoint::SCALE);
        let initial_margin = notional / lev_fp;
        let maint_num =
            FixedPoint::from_raw(config.maintenance_factor_bps as i128 * FixedPoint::SCALE);
        let bps_denom = FixedPoint::from_raw(10_000 * FixedPoint::SCALE);
        let maintenance = initial_margin * maint_num / bps_denom;

        let shortfall = maintenance - equity;

        Ok(Some(Liquidation {
            trader: pos.trader,
            market_id: pos.market_id,
            position: pos.clone(),
            shortfall,
            margin_type: MarginType::Isolated,
        }))
    }

    // 2.3.2: Force close a position at oracle price.
    pub fn execute_liquidation(
        positions: &PositionManager,
        liquidation: &Liquidation,
        oracle_price: FixedPoint,
    ) -> Result<LiquidationResult, CoreError> {
        // FIX 5: Reject zero/negative oracle prices (ECON-PF-06)
        if oracle_price <= FixedPoint::ZERO {
            return Err(CoreError::InvalidOraclePrice {
                market_id: liquidation.market_id,
            });
        }

        let pos = &liquidation.position;
        let pnl = pos.unrealized_pnl(oracle_price);
        let notional = pos.notional(oracle_price);

        // FIX 11: Compute and credit liquidation penalty to insurance fund (ECON-FIND-04)
        let penalty_bps = FixedPoint::from_raw(DEFAULT_LIQUIDATION_PENALTY_BPS * FixedPoint::SCALE);
        let bps_denom = FixedPoint::from_raw(10_000 * FixedPoint::SCALE);
        let liquidation_penalty = notional * penalty_bps / bps_denom;

        // Credit insurance fund
        let state_db = positions.state_db();
        let mut fund_balance = Self::get_insurance_fund(state_db)?;
        fund_balance = fund_balance + liquidation_penalty;
        Self::set_insurance_fund(state_db, fund_balance)?;

        // Credit/debit PnL to trader balance, minus the penalty
        let mut bal = positions.get_native_balance(&pos.trader)?;
        bal.available = bal.available + pnl - liquidation_penalty;

        // For isolated margin, return isolated_margin to balance before accounting
        if pos.margin_type == MarginType::Isolated {
            bal.available = bal.available + pos.isolated_margin;
        }

        // Remove the position
        positions.delete_position(&pos.trader, pos.market_id)?;

        // Check if remaining equity covers the loss
        let remaining_deficit = if bal.available < FixedPoint::ZERO {
            let deficit = -bal.available;
            bal.available = FixedPoint::ZERO;
            deficit
        } else {
            FixedPoint::ZERO
        };

        positions.put_native_balance(&pos.trader, &bal)?;

        Ok(LiquidationResult {
            trader: pos.trader,
            market_id: pos.market_id,
            size: pos.size,
            price: oracle_price,
            pnl,
            method: LiquidationMethod::ForceClose,
            remaining_deficit,
        })
    }

    // 2.3.3: Auto-deleverage — rank profitable traders, reduce their positions.
    pub fn auto_deleverage(
        positions: &PositionManager,
        market_id: MarketId,
        mut loss_amount: FixedPoint,
        oracle_price: FixedPoint,
        traders: &[Address],
    ) -> Result<Vec<AdlResult>, CoreError> {
        if loss_amount <= FixedPoint::ZERO {
            return Ok(Vec::new());
        }

        // Collect profitable traders in this market with their unrealized PnL
        let mut profitable: Vec<(Address, Position, FixedPoint)> = Vec::new();
        for trader in traders {
            if let Some(pos) = positions.get_position(trader, market_id)? {
                let upnl = pos.unrealized_pnl(oracle_price);
                if upnl > FixedPoint::ZERO {
                    profitable.push((*trader, pos, upnl));
                }
            }
        }

        // Sort by unrealized PnL descending (most profitable first)
        profitable.sort_by(|a, b| b.2.cmp(&a.2));

        let mut results = Vec::new();

        for (trader, pos, upnl) in &profitable {
            if loss_amount <= FixedPoint::ZERO {
                break;
            }

            // Determine how much to reduce: proportional to their share of total profit
            // or the remaining loss, whichever is smaller.
            let reduce_amount = if *upnl >= loss_amount {
                // This trader's profit covers the remaining loss
                // Calculate position size to close: loss / price_diff_per_unit
                let price_diff = if pos.is_long {
                    oracle_price - pos.entry_price
                } else {
                    pos.entry_price - oracle_price
                };
                if price_diff > FixedPoint::ZERO {
                    let size_to_close = loss_amount / price_diff;
                    if size_to_close > pos.size {
                        pos.size
                    } else {
                        size_to_close
                    }
                } else {
                    pos.size
                }
            } else {
                // Close entire position
                pos.size
            };

            let realized_pnl = pos.unrealized_pnl(oracle_price) * reduce_amount / pos.size;

            // Update the position
            if reduce_amount >= pos.size {
                // Full close
                positions.delete_position(trader, market_id)?;
            } else {
                // Partial close: reduce size, keep entry price
                let mut updated = pos.clone();
                updated.size = pos.size - reduce_amount;
                updated.realized_pnl = pos.realized_pnl + realized_pnl;
                positions.put_position(&updated)?;
            }

            // Credit realized PnL to the deleveraged trader
            let mut bal = positions.get_native_balance(trader)?;
            bal.available = bal.available + realized_pnl;
            positions.put_native_balance(trader, &bal)?;

            loss_amount = loss_amount - realized_pnl;

            results.push(AdlResult {
                trader: *trader,
                size_reduced: reduce_amount,
                price: oracle_price,
                realized_pnl,
            });
        }

        Ok(results)
    }

    // 2.3.4: Socialized loss — insurance fund first, then spread across all traders.
    pub fn socialize_loss(
        positions: &PositionManager,
        market_id: MarketId,
        mut remaining_loss: FixedPoint,
        traders: &[Address],
    ) -> Result<(), CoreError> {
        if remaining_loss <= FixedPoint::ZERO {
            return Ok(());
        }

        let state_db = positions.state_db();

        // First: deduct from insurance fund
        let fund_balance = Self::get_insurance_fund(state_db)?;
        if fund_balance > FixedPoint::ZERO {
            if fund_balance >= remaining_loss {
                Self::set_insurance_fund(state_db, fund_balance - remaining_loss)?;
                return Ok(());
            }
            remaining_loss = remaining_loss - fund_balance;
            Self::set_insurance_fund(state_db, FixedPoint::ZERO)?;
        }

        if remaining_loss <= FixedPoint::ZERO {
            return Ok(());
        }

        // Spread remaining loss across all traders in market proportional to position size
        let mut traders_with_size: Vec<(Address, FixedPoint)> = Vec::new();
        let mut total_size = FixedPoint::ZERO;

        for trader in traders {
            if let Some(pos) = positions.get_position(trader, market_id)? {
                traders_with_size.push((*trader, pos.size));
                total_size = total_size + pos.size;
            }
        }

        if total_size <= FixedPoint::ZERO || traders_with_size.is_empty() {
            return Ok(()); // No traders to spread across
        }

        // FIX 12: Filter out traders with insufficient balance (ECON-FIND-13)
        // Only include traders whose available balance covers at least 2x their
        // share of the loss, preventing cascading into already-underwater accounts.
        let two = FixedPoint::from_raw(2 * FixedPoint::SCALE);
        let eligible_traders: Vec<(Address, FixedPoint)> = traders_with_size
            .into_iter()
            .filter(|(trader, size)| {
                let share = *size / total_size;
                let deduction = remaining_loss * share;
                if let Ok(bal) = positions.get_native_balance(trader) {
                    bal.available >= deduction * two
                } else {
                    false
                }
            })
            .collect();

        // Recalculate total_size for eligible traders only
        let mut eligible_total_size = FixedPoint::ZERO;
        for (_, size) in &eligible_traders {
            eligible_total_size = eligible_total_size + *size;
        }

        if eligible_total_size <= FixedPoint::ZERO || eligible_traders.is_empty() {
            // No eligible traders — loss becomes protocol deficit
            // (absorbed by insurance fund recovery over time)
            return Ok(());
        }

        for (trader, size) in &eligible_traders {
            let share = *size / eligible_total_size;
            let deduction = remaining_loss * share;
            let mut bal = positions.get_native_balance(trader)?;
            bal.available = bal.available - deduction;
            positions.put_native_balance(trader, &bal)?;
        }

        Ok(())
    }

    /// Get insurance fund balance from state.
    fn get_insurance_fund(state_db: &StateDb) -> Result<FixedPoint, CoreError> {
        match state_db.get_cf_raw(CF_NATIVE_BALANCES, INSURANCE_FUND_KEY)? {
            Some(data) if data.len() == 16 => Ok(FixedPoint::from_raw(i128::from_be_bytes(
                data.try_into().unwrap(),
            ))),
            _ => Ok(FixedPoint::ZERO),
        }
    }

    /// Set insurance fund balance in state.
    fn set_insurance_fund(state_db: &StateDb, amount: FixedPoint) -> Result<(), CoreError> {
        state_db.put_cf_raw(
            CF_NATIVE_BALANCES,
            INSURANCE_FUND_KEY,
            &amount.raw().to_be_bytes(),
        )?;
        Ok(())
    }
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
    fn reject_zero_oracle_price_liquidation() {
        let (_dir, pm) = setup();
        let liq = Liquidation {
            trader: addr(1),
            market_id: 1,
            position: Position {
                trader: addr(1),
                market_id: 1,
                is_long: true,
                size: fp(1),
                entry_price: fp(50000),
                realized_pnl: FixedPoint::ZERO,
                isolated_margin: FixedPoint::ZERO,
                margin_type: MarginType::Cross,
            },
            shortfall: fp(100),
            margin_type: MarginType::Cross,
        };
        let result = LiquidationEngine::execute_liquidation(&pm, &liq, FixedPoint::ZERO);
        assert!(result.is_err());
    }

    #[test]
    fn reject_negative_oracle_price_liquidation() {
        let (_dir, pm) = setup();
        let liq = Liquidation {
            trader: addr(1),
            market_id: 1,
            position: Position {
                trader: addr(1),
                market_id: 1,
                is_long: true,
                size: fp(1),
                entry_price: fp(50000),
                realized_pnl: FixedPoint::ZERO,
                isolated_margin: FixedPoint::ZERO,
                margin_type: MarginType::Cross,
            },
            shortfall: fp(100),
            margin_type: MarginType::Cross,
        };
        let neg_price = FixedPoint::from_raw(-1 * FixedPoint::SCALE);
        let result = LiquidationEngine::execute_liquidation(&pm, &liq, neg_price);
        assert!(result.is_err());
    }

    #[test]
    fn insurance_fund_credited_on_liquidation() {
        let (_dir, pm) = setup();
        let trader = addr(1);
        // Setup: trader has a losing long position
        pm.put_native_balance(
            &trader,
            &NativeBalance {
                available: fp(10000),
                order_margin: FixedPoint::ZERO,
            },
        )
        .unwrap();
        pm.put_position(&Position {
            trader,
            market_id: 1,
            is_long: true,
            size: fp(1),
            entry_price: fp(50000),
            realized_pnl: FixedPoint::ZERO,
            isolated_margin: FixedPoint::ZERO,
            margin_type: MarginType::Cross,
        })
        .unwrap();

        let liq = Liquidation {
            trader,
            market_id: 1,
            position: Position {
                trader,
                market_id: 1,
                is_long: true,
                size: fp(1),
                entry_price: fp(50000),
                realized_pnl: FixedPoint::ZERO,
                isolated_margin: FixedPoint::ZERO,
                margin_type: MarginType::Cross,
            },
            shortfall: fp(100),
            margin_type: MarginType::Cross,
        };

        // Liquidate at 49000 (loss of 1000)
        let _result = LiquidationEngine::execute_liquidation(&pm, &liq, fp(49000)).unwrap();

        // Insurance fund should have been credited with penalty
        let fund = LiquidationEngine::get_insurance_fund(pm.state_db()).unwrap();
        assert!(fund > FixedPoint::ZERO, "insurance fund should be credited");

        // Penalty = notional * 250 / 10000 = 49000 * 0.025 = 1225
        let expected_penalty = fp(1225);
        assert_eq!(fund, expected_penalty);
    }

    #[test]
    fn socialized_loss_excludes_low_balance_traders() {
        let (_dir, pm) = setup();
        let t1 = addr(1);
        let t2 = addr(2);
        let t3 = addr(3);

        // t1: healthy balance
        pm.put_native_balance(
            &t1,
            &NativeBalance {
                available: fp(10000),
                order_margin: FixedPoint::ZERO,
            },
        )
        .unwrap();
        pm.put_position(&Position {
            trader: t1,
            market_id: 1,
            is_long: true,
            size: fp(1),
            entry_price: fp(50000),
            realized_pnl: FixedPoint::ZERO,
            isolated_margin: FixedPoint::ZERO,
            margin_type: MarginType::Cross,
        })
        .unwrap();

        // t2: very low balance (should be excluded)
        pm.put_native_balance(
            &t2,
            &NativeBalance {
                available: fp(10),
                order_margin: FixedPoint::ZERO,
            },
        )
        .unwrap();
        pm.put_position(&Position {
            trader: t2,
            market_id: 1,
            is_long: true,
            size: fp(1),
            entry_price: fp(50000),
            realized_pnl: FixedPoint::ZERO,
            isolated_margin: FixedPoint::ZERO,
            margin_type: MarginType::Cross,
        })
        .unwrap();

        // t3: healthy balance
        pm.put_native_balance(
            &t3,
            &NativeBalance {
                available: fp(10000),
                order_margin: FixedPoint::ZERO,
            },
        )
        .unwrap();
        pm.put_position(&Position {
            trader: t3,
            market_id: 1,
            is_long: true,
            size: fp(1),
            entry_price: fp(50000),
            realized_pnl: FixedPoint::ZERO,
            isolated_margin: FixedPoint::ZERO,
            margin_type: MarginType::Cross,
        })
        .unwrap();

        // Socialize a loss of 1000
        LiquidationEngine::socialize_loss(&pm, 1, fp(1000), &[t1, t2, t3]).unwrap();

        // t2 should NOT have been debited (low balance excluded)
        let bal_t2 = pm.get_native_balance(&t2).unwrap();
        assert_eq!(
            bal_t2.available,
            fp(10),
            "low-balance trader should be excluded from socialized loss"
        );

        // t1 and t3 should share the loss
        let bal_t1 = pm.get_native_balance(&t1).unwrap();
        let bal_t3 = pm.get_native_balance(&t3).unwrap();
        assert!(bal_t1.available < fp(10000));
        assert!(bal_t3.available < fp(10000));
    }
}
