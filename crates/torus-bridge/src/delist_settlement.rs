//! Delist option C (owner, s131): the settlement of a governance-delisted
//! market. Child module of `native_executor` (sees the private cancel helpers,
//! `AccountReader` and the liquidation step's trader walk).
//!
//! - When a delist executes at height H (`apply_executed_market_proposal`),
//!   the market's settlement row is written in the same block:
//!   `close_at = H + N`, N = governance key [`torus_types::DELIST_GRACE_PARAM`]
//!   read then (default 43,200 blocks). Until `close_at` the market is
//!   reduce-only (the P1 wind-down).
//! - From `close_at`, the block step [`NativeExecutor::run_delist_settlements`]
//!   (after `process_governance`) settles it:
//!   1. the settlement price is chosen ONCE, at the first settlement block,
//!      and stored in the row: the governance close price (owner s134, 1c:
//!      `SetDelistClosePrice`, executed before or at `close_at`; consumed
//!      here), else the usable oracle mark (block-start table), else the
//!      last aggregated oracle price (stale but > 0, no age cap), else none:
//!      the row waits (an error result, retried every block);
//!   2. every resting order and pending stop in the market is cancelled and
//!      refunded (each settlement block: reduce-only orders placed between
//!      settlement blocks never survive one);
//!   3. the walk visits `CF_NATIVE_POSITIONS` traders in ascending address
//!      order after the row's cursor (the liquidation step's C1 walk, from
//!      R's sorted set when attached: identical result) and moves each
//!      trader's whole position in the market to the liquidator vault at the
//!      settlement price ([`liq::transfer`], the backstop primitive: PnL
//!      realised to the trader's balance, Σ long == Σ short after every
//!      block). A flat account left with negative collateral moves it to the
//!      vault ([`liq::settle_flat_deficit`], D9). The vault and the ADL
//!      escrows are not walked: the vault is the counterparty and ends
//!      holding exactly minus the escrows' positions (flat without ADL rows),
//!      which the ADL drain closes against it at the obligations' stored
//!      prices (a governance-delisted market needs no fresh mark there:
//!      settled at a stale price, its oracle may never return).
//!   4. Bounded: at most `scan` traders visited and `close` positions closed
//!      per block, across all settling markets (ascending market id). A cut
//!      walk stores the last visited trader as the cursor; the next block
//!      continues there. The row is deleted when the walk reaches the end.
//!   5. Owner s134, 3b: when the row is gone and the vault and both ADL
//!      escrows are flat in the market (here, or after the ADL drain), the
//!      oracle-closed marker is written: its oracle entries are ignored and
//!      it is no longer aggregated.
//!
//! Every write goes through the block's state (one atomic batch per block).
//! A storage error, or any error of the pass, fail-stops the node like the
//! liquidation step (`fatal_error`).

use super::*;
use torus_core::liquidation::{self as liq, DelistSettlement, LIQUIDATOR_VAULT};

impl NativeExecutor {
    /// Delist option C: the per-block settlement step (after
    /// `process_governance`, so a delist with N = 0 settles in its own
    /// block). One result per settling market that waited for a price or
    /// worked this block.
    pub fn run_delist_settlements<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
    ) -> Vec<NativeActionResult> {
        Self::run_delist_settlements_with(
            ctx,
            liq::DELIST_SETTLE_SCAN_PER_BLOCK,
            liq::DELIST_SETTLE_CLOSE_PER_BLOCK,
        )
    }

    /// [`Self::run_delist_settlements`] with explicit budgets (tests): at most
    /// `scan` traders visited and `close` positions closed this block.
    pub fn run_delist_settlements_with<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
        scan: usize,
        close: usize,
    ) -> Vec<NativeActionResult> {
        match Self::delist_settlement_pass(ctx, scan, close) {
            Ok(r) => r,
            Err(e) => {
                ctx.fatal_error.get_or_insert_with(|| format!("delist settlement step: {e}"));
                Vec::new()
            }
        }
    }

    /// Whether a settlement row is due at `height`: the block must run the
    /// native phase even without other work (app.rs due-check).
    pub fn delist_settlement_due<T: StateBackend>(state: &T, height: u64) -> Result<bool, CoreError> {
        liq::delist_settlement_due(state, height)
    }

    /// Writes `market_id`'s settlement row when its delist executes (in the
    /// governance step, the proposal's block): `close_at = height + N`. A
    /// failed read of N or a failed write is a local fault (fail-stop).
    pub(super) fn schedule_delist_settlement<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
        market_id: MarketId,
    ) {
        let stored = ctx
            .state
            .get_cf_raw(torus_state::cf::CF_FEE_CONFIG, torus_types::DELIST_GRACE_PARAM.as_bytes());
        let res = stored.map_err(CoreError::from).and_then(|stored| {
            let n = torus_types::delist_grace_blocks(stored.as_deref());
            liq::put_delist_settlement(
                &ctx.state,
                &DelistSettlement {
                    market: market_id,
                    close_at: ctx.block_height.saturating_add(n),
                    price: None,
                    cursor: None,
                },
            )
        });
        if let Err(e) = res {
            ctx.fatal_error.get_or_insert_with(|| format!("delist settlement schedule: {e}"));
        }
    }

    /// The settlement price of `m` for a row that has none yet: the
    /// governance close price (owner s134, 1c), else the usable mark, else
    /// the last aggregated oracle price when > 0 (stale, no age cap), else
    /// `None` (wait). The flag is `true` for the governance close price.
    fn delist_settlement_price<T: StateBackend>(
        ctx: &NativeExecContext<T>,
        m: MarketId,
    ) -> Result<Option<(FixedPoint, bool)>, CoreError> {
        if let Some(p) = liq::delist_close_price(&ctx.state, m)? {
            return Ok(Some((p, true)));
        }
        if let Some(mark) = AccountReader::of(ctx).mark(m) {
            return Ok(Some((mark, false)));
        }
        Ok(ctx
            .oracle
            .get_price_opt(m, ctx.timestamp)?
            .map(|p| p.price)
            .filter(|p| *p > FixedPoint::ZERO)
            .map(|p| (p, false)))
    }

    /// Owner s134, 3b: when delisted `m`'s settlement is complete (no
    /// settlement row, and the liquidator vault and both ADL escrows hold no
    /// position in it, so no ADL obligation is left), write its oracle-closed
    /// marker ([`torus_types::market_oracle_closed_key`], value = height) and
    /// add it to the block's set: from the next block its oracle entries are
    /// ignored and it is not aggregated. Called where completion can happen:
    /// the walk's end (here) and the ADL drain of a delisted market. Already
    /// closed, not delisted or not complete: nothing.
    pub(super) fn close_oracle_if_settled<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
        m: MarketId,
    ) -> Result<(), CoreError> {
        if ctx.oracle_closed_markets.contains(&m) || !ctx.delisted_markets.contains(&m) {
            return Ok(());
        }
        if liq::delist_settlement(&ctx.state, m)?.is_some() {
            return Ok(());
        }
        for a in [LIQUIDATOR_VAULT, liq::ADL_ESCROW_LONG, liq::ADL_ESCROW_SHORT] {
            if ctx.positions.get_position(&a, m)?.is_some() {
                return Ok(());
            }
        }
        ctx.state.put_cf_raw(
            torus_state::cf::CF_NATIVE_MARKETS,
            &torus_types::market_oracle_closed_key(m),
            &ctx.block_height.to_be_bytes(),
        )?;
        ctx.oracle_closed_markets.insert(m);
        tracing::info!(market = m, height = ctx.block_height, "delist settlement complete: oracle closed");
        Ok(())
    }

    fn delist_settlement_pass<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
        scan: usize,
        close: usize,
    ) -> Result<Vec<NativeActionResult>, CoreError> {
        let due: Vec<DelistSettlement> = liq::delist_settlements(&ctx.state)?
            .into_iter()
            .filter(|s| s.close_at <= ctx.block_height)
            .collect();
        let mut results = Vec::new();
        let (mut scan_left, mut close_left) = (scan, close);
        for mut row in due {
            if scan_left == 0 || close_left == 0 {
                break; // the rest waits for the next block
            }
            let m = row.market;
            let price = match row.price {
                Some(p) => p,
                None => match Self::delist_settlement_price(ctx, m)? {
                    Some((p, governance)) => {
                        // Owner s134, 1c: a governance close price is
                        // consumed here; from now on the price never changes
                        // (later close-price proposals fail as too late).
                        if governance {
                            liq::delete_delist_close_price(&ctx.state, m)?;
                        }
                        tracing::info!(market = m, price = %p, governance, height = ctx.block_height, "delist settlement: price fixed");
                        p
                    }
                    None => {
                        results.push(NativeActionResult::err(
                            "delist_settlement",
                            format!("delist settlement: market {m} has no oracle price: waiting"),
                        ));
                        continue;
                    }
                },
            };
            // No order survives a settlement block (reduce-only orders placed
            // since the last one included).
            Self::cancel_market_orders_and_stops(ctx, m);
            if let Some(reason) = ctx.reader_fault.get() {
                return Err(CoreError::InvalidInput(format!("order cancel: {reason}")));
            }
            // SCAN + 3: the vault and the two escrows are skipped (not
            // charged to the budget), so a full fetch with the budget left
            // means the walk has not reached the end.
            let limit = scan_left.saturating_add(3);
            let traders = Self::liq_traders_after(ctx, row.cursor, limit)?;
            let (mut last, mut cut, mut closed) = (row.cursor, false, 0u64);
            for t in &traders {
                if *t == LIQUIDATOR_VAULT || liq::is_adl_escrow(t) {
                    last = Some(*t);
                    continue;
                }
                if scan_left == 0 || close_left == 0 {
                    cut = true;
                    break;
                }
                scan_left -= 1;
                last = Some(*t);
                let Some(p) = ctx.positions.get_position(t, m)? else { continue };
                close_left -= 1;
                closed += 1;
                liq::transfer(&ctx.positions, t, &LIQUIDATOR_VAULT, m, p.size, price)?;
                liq::settle_flat_deficit(&ctx.positions, t, &LIQUIDATOR_VAULT)?;
                if ctx.positions.positions_for_trader(t)?.is_empty() {
                    // Flat: the liquidation walk never visits it again.
                    liq::clear_cooldown(&ctx.state, t)?;
                    liq::set_pending(&ctx.state, t, false)?;
                }
                tracing::debug!(market = m, trader = %t, size = %p.size, %price, "delist settlement: position closed");
            }
            let done = !cut && traders.len() < limit;
            if done {
                liq::delete_delist_settlement(&ctx.state, m)?;
                let vault = ctx.positions.get_position(&LIQUIDATOR_VAULT, m)?;
                tracing::info!(market = m, height = ctx.block_height, vault_residual = ?vault.map(|p| (p.is_long, p.size)), "delist settlement: market settled");
                Self::close_oracle_if_settled(ctx, m)?;
            } else {
                row.price = Some(price);
                row.cursor = last;
                liq::put_delist_settlement(&ctx.state, &row)?;
            }
            results.push(NativeActionResult::ok("delist_settlement", 1000 + 3000 * closed));
        }
        Ok(results)
    }
}
