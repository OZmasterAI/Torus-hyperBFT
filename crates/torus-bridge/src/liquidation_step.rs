//! Item 3: the end-of-block liquidation step — `docs/plans/liquidation.md`.
//!
//! Child module of `native_executor` (sees its private `place_order_inner`,
//! `run_triggered_stops`, `cancel_orders_and_stops`, `AccountReader`).
//! Deterministic: markets from `listed_market_ids()` (ascending) into a
//! `BTreeMap`, candidates from the sorted `CF_NATIVE_POSITIONS` walk, order
//! books visited by sorted market id, exact integer math, a stale / absent
//! mark in any of an account's markets skips the account.

use super::*;
use std::cmp::Reverse;
use std::collections::BTreeMap;
use torus_core::liquidation::{self as liq, Health, LIQUIDATOR_VAULT};
use torus_core::margin::maintenance_margin;
use torus_core::position::Position;
use torus_state::cf::CF_NATIVE_LIQUIDATION;

type Marks = BTreeMap<MarketId, FixedPoint>;

impl NativeExecutor {
    /// Item 3 block step (after `drain_core_writer`, before governance): stage 1
    /// (book) / backstop (vault) / ADL on the block-start mark. A storage error
    /// is a node fault -> `fatal_error`; everything else is a result.
    pub fn run_liquidations<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
    ) -> Vec<NativeActionResult> {
        Self::run_liquidations_with(ctx, liq::LIQ_SCAN_PER_BLOCK, liq::LIQ_ACT_PER_BLOCK)
    }

    /// [`Self::run_liquidations`] with explicit budgets (tests).
    pub fn run_liquidations_with<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
        scan: usize,
        act: usize,
    ) -> Vec<NativeActionResult> {
        match Self::liquidation_pass(ctx, scan, act) {
            Ok(r) => r,
            Err(e) => {
                ctx.fatal_error = Some(format!("liquidation step: {e}"));
                Vec::new()
            }
        }
    }

    /// Item 3: whether the liquidation step has pending work without any new
    /// action — a cooldown row (a chunk waits for its 30 s) or the cursor row
    /// (a cut pass). Reads the block's overlay (DB + parent layer).
    pub fn liquidation_due<T: StateBackend>(state: &T) -> Result<bool, CoreError> {
        Ok(state.prefix_exists(CF_NATIVE_LIQUIDATION, &[liq::COOLDOWN_TAG])?
            || state.get_cf_raw(CF_NATIVE_LIQUIDATION, &liq::CURSOR_KEY)?.is_some())
    }

    fn liquidation_pass<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
        scan: usize,
        act: usize,
    ) -> Result<Vec<NativeActionResult>, CoreError> {
        let marks: Marks = {
            let reader = AccountReader::of(ctx);
            ctx.governance
                .listed_market_ids()
                .map_err(|e| CoreError::InvalidInput(format!("listed markets: {e}")))?
                .into_iter()
                .filter_map(|m| reader.mark(m).map(|p| (m, p)))
                .collect()
        };
        let prev = liq::prev_marks(&ctx.state, marks.keys().copied())?;
        // C1 (decided, s517): no separate index — walk CF_NATIVE_POSITIONS
        // (sorted by trader) from the round-robin cursor. SCAN + 2: the vault
        // (skipped) is at most one of them, so a pass that consumes every
        // fetched trader without hitting a budget has reached the end.
        let cursor = liq::cursor(&ctx.state)?;
        let accounts = liq::traders_after(&ctx.state, cursor, scan.saturating_add(2))?;
        let mut results = Vec::new();
        let (mut scanned, mut acted, mut last, mut cut) = (0usize, 0usize, None, false);
        for &trader in accounts.iter().filter(|a| **a != LIQUIDATOR_VAULT) {
            if scanned == scan || acted == act {
                cut = true;
                break;
            }
            scanned += 1;
            last = Some(trader);
            let h = match Self::liq_view(ctx, &marks, &trader)?.map(|v| liq::classify(&v)) {
                Some(Some(h)) => h,
                _ => {
                    results.push(NativeActionResult::err(
                        "liquidation",
                        format!("{trader}: skipped (no mark / overflow / isolated)"),
                    ));
                    continue;
                }
            };
            if h == Health::Healthy {
                liq::clear_cooldown(&ctx.state, &trader)?;
                continue;
            }
            acted += 1;
            if let Some(ref m) = ctx.metrics {
                m.liquidations_triggered.inc();
            }
            // D4: every resting order and pending stop goes first (sorted
            // markets), reservations released. AV is unchanged.
            let release = Self::cancel_orders_and_stops(ctx, &trader, None);
            Self::release_order_margin(ctx, &trader, release);
            match h {
                Health::Adl => Self::adl_account(ctx, &marks, &prev, &trader)?,
                Health::Backstop => liq::backstop(&ctx.positions, &trader, &LIQUIDATOR_VAULT, |m| {
                    marks.get(&m).copied()
                })?,
                Health::Stage1 => Self::stage1(ctx, &marks, &trader, &mut results)?,
                Health::Healthy => {}
            }
            liq::settle_flat_deficit(&ctx.positions, &trader, &LIQUIDATOR_VAULT)?;
            if ctx.positions.positions_for_trader(&trader)?.is_empty() {
                liq::clear_cooldown(&ctx.state, &trader)?;
            }
            results.push(NativeActionResult::ok("liquidation", 3000));
        }
        // D8: the vault is exempt from stage 1 / backstop; ADL when its AV < 0.
        // (`classify` is Adl exactly when AV < 0, overflow-checked.)
        if let Some(v) = Self::liq_view(ctx, &marks, &LIQUIDATOR_VAULT)? {
            if liq::classify(&v) == Some(Health::Adl) {
                Self::adl_account(ctx, &marks, &prev, &LIQUIDATOR_VAULT)?;
            }
        }
        liq::put_prev_marks(&ctx.state, &marks, &prev)?;
        liq::put_cursor(&ctx.state, if cut { last } else { None })?;
        Ok(results)
    }

    /// Decision 9: `None` unless the account has positions, all Cross, every
    /// one with a usable mark; overflow -> `None` (skip).
    fn liq_view<T: StateBackend>(
        ctx: &NativeExecContext<T>,
        marks: &Marks,
        trader: &Address,
    ) -> Result<Option<AccountView>, CoreError> {
        let ps = ctx.positions.positions_for_trader(trader)?;
        if ps.is_empty()
            || ps
                .iter()
                .any(|p| p.margin_type != MarginType::Cross || !marks.contains_key(&p.market_id))
        {
            return Ok(None);
        }
        let bal = ctx.positions.get_native_balance(trader)?;
        let reader = AccountReader::of(ctx);
        Ok(AccountView::build(&bal, &ps, |m| marks.get(&m).copied(), |m| reader.tiers(m)).ok())
    }

    /// Stage 1: reduce-only IOC market orders into the book, positions by
    /// (MM desc, market asc); a chunk (D2) sets the cooldown and ends the
    /// account's stage 1 for this block; stops as soon as `AV >= MM`.
    fn stage1<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
        marks: &Marks,
        trader: &Address,
        results: &mut Vec<NativeActionResult>,
    ) -> Result<(), CoreError> {
        if liq::in_cooldown(&ctx.state, trader, ctx.timestamp)? {
            return Ok(());
        }
        let mut order: Vec<(Reverse<FixedPoint>, MarketId)> = Vec::new();
        for p in ctx.positions.positions_for_trader(trader)? {
            let tiers = ctx.margin_configs.get(&p.market_id).map(|c| c.tiers.as_slice());
            let n = Self::liq_notional(&p, marks)?;
            order.push((Reverse(maintenance_margin(tiers, n)), p.market_id));
        }
        order.sort();
        for (_, m) in order {
            let Some(p) = ctx.positions.get_position(trader, m)? else { continue };
            let mark = marks[&m];
            let lot = ctx.order_books.get(&m).map_or(FixedPoint::ONE, |b| b.lot_size);
            let (qty, chunked) = liq::stage1_qty(p.size, mark, lot);
            let tiers = ctx.margin_configs.get(&m).map(|c| c.tiers.as_slice());
            let cap = liq::slippage_cap(tiers, mark, Self::liq_notional(&p, marks)?, !p.is_long);
            let params = PlaceOrderParams {
                market_id: m,
                is_buy: !p.is_long,
                price: cap,
                quantity: qty,
                order_type: OrderType::Market,
                time_in_force: TimeInForce::IOC,
                reduce_only: true,
                client_order_id: None,
            };
            // Exactly `exec_place_order` (D7: stops fired by its fills run).
            let mut queue = VecDeque::new();
            let r = Self::place_order_inner(ctx, trader, &params, None, &mut queue);
            Self::run_triggered_stops(ctx, queue);
            if !r.success {
                results.push(r);
            }
            if chunked {
                liq::set_cooldown(&ctx.state, trader, ctx.timestamp)?;
                break;
            }
            match Self::liq_view(ctx, marks, trader)? {
                Some(v) if liq::classify(&v) == Some(Health::Healthy) => break,
                None => break, // flat (or no longer valuable): nothing left to do
                _ => {}
            }
        }
        Ok(())
    }

    /// |size| × mark of a position whose market has a mark (checked).
    fn liq_notional(p: &Position, marks: &Marks) -> Result<FixedPoint, CoreError> {
        let mark = marks.get(&p.market_id).ok_or(CoreError::NoOraclePrice(p.market_id))?;
        p.size
            .checked_mul(*mark)
            .map_err(|_| CoreError::Overflow("liquidation notional overflows i128".into()))
    }

    /// Decision 5 + D10: close every position of `u` (ascending market)
    /// against ranked opposite-side counterparties at the previous mark (the
    /// current mark the first time).
    fn adl_account<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
        marks: &Marks,
        prev: &Marks,
        u: &Address,
    ) -> Result<(), CoreError> {
        for p in ctx.positions.positions_for_trader(u)? {
            let m = p.market_id;
            let Some(&mark) = marks.get(&m) else { continue };
            let px = prev.get(&m).copied().unwrap_or(mark);
            let reader = AccountReader::of(ctx);
            // C7: ranking AV with entry fallback for unmarked markets. An
            // overflowing valuation ranks last (AV 0) — ranking only; a
            // storage error stays an error (fail-stop).
            let cands = liq::adl_candidates(&ctx.positions, m, u, !p.is_long, |t| {
                let bal = ctx.positions.get_native_balance(t)?;
                let v = match reader.view(t, &bal) {
                    Ok(v) => v,
                    Err(CoreError::Overflow(_)) => return Ok(FixedPoint::ZERO),
                    Err(e) => return Err(e),
                };
                Ok(v.available
                    .checked_add(v.order_margin)
                    .and_then(|x| x.checked_add(v.upnl))
                    .unwrap_or(FixedPoint::ZERO))
            })?;
            liq::adl_close(&ctx.positions, u, m, px, &liq::adl_rank(mark, cands))?;
        }
        Ok(())
    }
}
