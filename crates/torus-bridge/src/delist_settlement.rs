//! Delist option C (owner, s131): the settlement of a governance-delisted
//! market. Child module of `native_executor` (sees the private cancel helpers
//! and `AccountReader`).

use super::*;
use torus_core::liquidation as liq;

impl NativeExecutor {
    /// Delist option C: the per-block settlement step (after
    /// `process_governance`).
    pub fn run_delist_settlements<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
    ) -> Vec<NativeActionResult> {
        Self::run_delist_settlements_with(
            ctx,
            liq::DELIST_SETTLE_SCAN_PER_BLOCK,
            liq::DELIST_SETTLE_CLOSE_PER_BLOCK,
        )
    }

    /// [`Self::run_delist_settlements`] with explicit budgets (tests).
    pub fn run_delist_settlements_with<T: StateBackend>(
        _ctx: &mut NativeExecContext<T>,
        _scan: usize,
        _close: usize,
    ) -> Vec<NativeActionResult> {
        Vec::new() // RED stub
    }

    /// Whether a settlement row is due at `height`.
    pub fn delist_settlement_due<T: StateBackend>(state: &T, height: u64) -> Result<bool, CoreError> {
        liq::delist_settlement_due(state, height)
    }

    /// Writes the market's settlement row when its delist executes.
    pub(super) fn schedule_delist_settlement<T: StateBackend>(
        _ctx: &mut NativeExecContext<T>,
        _market_id: MarketId,
    ) {
        // RED stub
    }
}
