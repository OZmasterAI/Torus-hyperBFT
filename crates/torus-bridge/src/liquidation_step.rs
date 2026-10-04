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
    /// action — a cooldown row (a chunked account: whole-position orders for
    /// 30 s, then the next chunk), the cursor row
    /// (a cut pass) or, review M2, a pending row (an account left under MM by
    /// its last action, e.g. a thin book). Reads the block's overlay (DB +
    /// parent layer).
    pub fn liquidation_due<T: StateBackend>(state: &T) -> Result<bool, CoreError> {
        Ok(state.prefix_exists(CF_NATIVE_LIQUIDATION, &[liq::COOLDOWN_TAG])?
            || state.prefix_exists(CF_NATIVE_LIQUIDATION, &[liq::PENDING_TAG])?
            || state.get_cf_raw(CF_NATIVE_LIQUIDATION, &liq::CURSOR_KEY)?.is_some())
    }

    fn liquidation_pass<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
        scan: usize,
        act: usize,
    ) -> Result<Vec<NativeActionResult>, CoreError> {
        let listed = ctx
            .governance
            .listed_market_ids()
            .map_err(|e| CoreError::InvalidInput(format!("listed markets: {e}")))?;
        let marks: Marks = {
            let reader = AccountReader::of(ctx);
            listed.iter().filter_map(|&m| reader.mark(m).map(|p| (m, p))).collect()
        };
        let prev = liq::prev_marks(&ctx.state, marks.keys().copied())?;
        let l1 = Self::l1_on(ctx, &listed);
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
            let h = match Self::liq_view(ctx, &marks, &trader, l1)?.map(|v| liq::classify(&v)) {
                Some(Some(h)) => h,
                _ => {
                    // M2: nothing the step can do until a mark returns (the
                    // oracle step makes those blocks due).
                    liq::set_pending(&ctx.state, &trader, false)?;
                    results.push(NativeActionResult::err(
                        "liquidation",
                        format!("{trader}: skipped (no marked position / overflow / isolated)"),
                    ));
                    continue;
                }
            };
            if h == Health::Healthy {
                liq::clear_cooldown(&ctx.state, &trader)?;
                liq::set_pending(&ctx.state, &trader, false)?;
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
                Health::Stage1 => Self::stage1(ctx, &marks, l1, &trader, &mut results)?,
                Health::Healthy => {}
            }
            liq::settle_flat_deficit(&ctx.positions, &trader, &LIQUIDATOR_VAULT)?;
            if ctx.positions.positions_for_trader(&trader)?.is_empty() {
                liq::clear_cooldown(&ctx.state, &trader)?;
            }
            // M2: still under MM (thin book, cooldown, bounded ADL) -> keep the
            // step due until the account is healthy, flat or unvaluable.
            Self::mark_pending(ctx, &marks, l1, &trader)?;
            results.push(NativeActionResult::ok("liquidation", 3000));
        }
        // D8: the vault is exempt from stage 1 / backstop; ADL when its AV < 0.
        // (`classify` is Adl exactly when AV < 0, overflow-checked.)
        if let Some(v) = Self::liq_view(ctx, &marks, &LIQUIDATOR_VAULT, l1)? {
            if liq::classify(&v) == Some(Health::Adl) {
                Self::adl_account(ctx, &marks, &prev, &LIQUIDATOR_VAULT)?;
            }
        }
        // M2 for the vault: pending while it stays ADL-able.
        let vault_adl = Self::liq_view(ctx, &marks, &LIQUIDATOR_VAULT, l1)?
            .is_some_and(|v| liq::classify(&v) == Some(Health::Adl));
        liq::set_pending(&ctx.state, &LIQUIDATOR_VAULT, vault_adl)?;
        liq::put_prev_marks(&ctx.state, &listed, &marks, &prev)?;
        liq::put_cursor(&ctx.state, if cut { last } else { None })?;
        Ok(results)
    }

    /// Item 6 C4 (plan 2.5, L1): whether [`Self::liq_view`] values from the
    /// sums cache. Its sums value every market with a mark (the block's
    /// table, else the oracle); the step's `Marks` only the `listed` ones.
    /// They agree iff no market outside `listed` has a mark: a market outside
    /// the table has no aggregate row (the table takes every aggregated
    /// market, and only `begin_block_oracle` writes those rows), so
    /// `delisted_marked` decides it. Normally empty; a delisted market with
    /// a fresh aggregate (up to 60 s) turns L1 off for the block.
    fn l1_on<T: StateBackend>(ctx: &NativeExecContext<T>, listed: &[MarketId]) -> bool {
        ctx.sums.is_some() && ctx.block_marks.as_ref().is_some_and(|t| t.delisted_marked(listed).is_empty())
    }

    /// Review H2 (user decision s517): the account is valued with its marked
    /// positions at the mark and its UNMARKED ones at their entry price (UPnL 0,
    /// IM / MM still count — `AccountView::build`'s fallback). `None` (skip)
    /// when it has no position in a marked market, holds an Isolated position,
    /// or the valuation overflows. Only marked positions are ever acted on.
    /// Item 6 C4 (L1, `l1` from [`Self::l1_on`]): the position part from the
    /// sums cache ([`AccountReader::pos_sums`]: the same `build` over the
    /// same rows with the same marks), the balance a point read.
    fn liq_view<T: StateBackend>(
        ctx: &NativeExecContext<T>,
        marks: &Marks,
        trader: &Address,
        l1: bool,
    ) -> Result<Option<AccountView>, CoreError> {
        // Fix 2a (s87): with no usable mark in any listed market no position
        // is marked, so every account is `None` below — skip the reads. The
        // step's cursor / pending / prev-mark writes do not depend on them.
        if marks.is_empty() {
            return Ok(None);
        }
        if !l1 {
            #[cfg(test)]
            if let Some(s) = ctx.sums.as_ref() {
                bump(&s.counters.l1_off);
            }
            return Self::liq_view_walk(ctx, marks, trader);
        }
        let v = match AccountReader::of(ctx).pos_sums(trader) {
            // `build` skips a non-Cross position; the step does not value
            // such an account. `marked == 0`: no position at a mark.
            Ok(s) if s.any_isolated || s.marked == 0 => None,
            Ok(s) => Some(s.view(&ctx.positions.get_native_balance(trader)?)),
            Err(CoreError::Overflow(_)) => None,
            Err(e) => return Err(e),
        };
        #[cfg(test)]
        if let Some(s) = ctx.sums.as_ref() {
            bump(&s.counters.l1);
            if s.shadow {
                let want = Self::liq_view_walk(ctx, marks, trader);
                if want.as_ref().ok() != Some(&v) {
                    s.shadow_mismatches.lock().unwrap().push(format!("liq_view {trader}: L1 {v:?}, walk {want:?}"));
                }
            }
        }
        Ok(v)
    }

    /// [`Self::liq_view`] without the cache: `build` over the trader's rows
    /// at the step's `marks` (the reference path).
    fn liq_view_walk<T: StateBackend>(
        ctx: &NativeExecContext<T>,
        marks: &Marks,
        trader: &Address,
    ) -> Result<Option<AccountView>, CoreError> {
        let ps = ctx.positions.positions_for_trader(trader)?;
        if ps.iter().any(|p| p.margin_type != MarginType::Cross)
            || !ps.iter().any(|p| marks.contains_key(&p.market_id))
        {
            return Ok(None);
        }
        let bal = ctx.positions.get_native_balance(trader)?;
        let reader = AccountReader::of(ctx);
        Ok(AccountView::build(&bal, &ps, |m| marks.get(&m).copied(), |m| reader.tiers(m)).ok())
    }

    /// Review M2: the pending row of `trader` after its action — set iff it is
    /// still valuable and not healthy.
    fn mark_pending<T: StateBackend>(
        ctx: &NativeExecContext<T>,
        marks: &Marks,
        l1: bool,
        trader: &Address,
    ) -> Result<(), CoreError> {
        let under = Self::liq_view(ctx, marks, trader, l1)?
            .and_then(|v| liq::classify(&v))
            .is_some_and(|h| h != Health::Healthy);
        liq::set_pending(&ctx.state, trader, under)
    }

    /// Stage 1: reduce-only IOC market orders into the book, positions by
    /// (MM desc, market asc); stops as soon as `AV >= MM`.
    ///
    /// HL parity (s88; was: stage 1 skipped in the cooldown, a misreading of
    /// HL): "After a block where any position of a user is partially
    /// liquidated, there is a cooldown period of 30 seconds. During this
    /// cooldown period, all market liquidation orders for that user will be
    /// for the entire position."
    /// * Outside the cooldown a position above 100,000 notional goes as a 20%
    ///   chunk (D2): the chunk sets the cooldown (block time) and ends the
    ///   account's stage 1 for this block — its other positions wait for the
    ///   next block, where they are inside the cooldown.
    /// * Inside the cooldown every order is for the ENTIRE position, in the
    ///   same order, until `AV >= MM`. Such an order never writes the
    ///   cooldown row: only a chunk starts a cooldown, so it is neither
    ///   restarted nor extended (expiry stays chunk time + 30 s), also when
    ///   the book fills the order only in part. What the book cannot take
    ///   stays; the account remains pending and backstop / ADL act when it
    ///   falls below 2/3 MM / 0. The order keeps the slippage cap.
    fn stage1<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
        marks: &Marks,
        l1: bool,
        trader: &Address,
        results: &mut Vec<NativeActionResult>,
    ) -> Result<(), CoreError> {
        let whole = liq::in_cooldown(&ctx.state, trader, ctx.timestamp)?;
        let mut order: Vec<(Reverse<FixedPoint>, MarketId)> = Vec::new();
        // Review H2: only marked positions are sold; unmarked ones stay.
        for p in ctx.positions.positions_for_trader(trader)? {
            if !marks.contains_key(&p.market_id) {
                continue;
            }
            let tiers = ctx.margin_configs.get(&p.market_id).map(|c| c.tiers.as_slice());
            let n = Self::liq_notional(&p, marks)?;
            order.push((Reverse(maintenance_margin(tiers, n)), p.market_id));
        }
        order.sort();
        for (_, m) in order {
            let Some(p) = ctx.positions.get_position(trader, m)? else { continue };
            let mark = marks[&m];
            let lot = ctx.order_books.get(&m).map_or(FixedPoint::ONE, |b| b.lot_size);
            let (qty, chunked) = if whole { (p.size, false) } else { liq::stage1_qty(p.size, mark, lot) };
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
            match Self::liq_view(ctx, marks, trader, l1)? {
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

    /// Decision 5 + D10 + review H1: close every MARKED position of `u`
    /// (ascending market) against ranked opposite-side counterparties at the
    /// previous mark (the current mark without one) clamped to `u`'s
    /// bankruptcy price ([`liq::adl_price`]). Afterwards a non-vault account
    /// without marked positions hands its remaining collateral (rounding dust,
    /// or the deficit when the previous mark was worse than bankruptcy) to
    /// the vault: it ends at exactly 0.
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
            let bankruptcy = Self::adl_rest(ctx, marks, u, m)?
                .and_then(|rest| liq::bankruptcy_price(rest, p.is_long, p.size, p.entry_price));
            let px = liq::adl_price(px, bankruptcy, mark, p.is_long);
            let reader = AccountReader::of(ctx);
            // C7: ranking AV with entry fallback for unmarked markets. An
            // overflowing valuation ranks last (AV 0) — ranking only; a
            // storage error stays an error (fail-stop).
            let cands = liq::adl_candidates(&ctx.positions, m, u, !p.is_long, liq::ADL_MAX_SCAN_ROWS, |t| {
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
        if *u != LIQUIDATOR_VAULT
            && !ctx
                .positions
                .positions_for_trader(u)?
                .iter()
                .any(|p| marks.contains_key(&p.market_id))
        {
            liq::move_collateral(&ctx.positions, u, &LIQUIDATOR_VAULT)?;
        }
        Ok(())
    }

    /// Review H1: `u`'s collateral + the UPnL of its positions OTHER than in
    /// `m` (marked at the mark, unmarked at entry = 0). `None` on overflow.
    fn adl_rest<T: StateBackend>(
        ctx: &NativeExecContext<T>,
        marks: &Marks,
        u: &Address,
        m: MarketId,
    ) -> Result<Option<FixedPoint>, CoreError> {
        let bal = ctx.positions.get_native_balance(u)?;
        let others: Vec<Position> = ctx
            .positions
            .positions_for_trader(u)?
            .into_iter()
            .filter(|q| q.market_id != m)
            .collect();
        let v = match AccountView::build(&bal, &others, |x| marks.get(&x).copied(), |_| None) {
            Ok(v) => v,
            Err(CoreError::Overflow(_)) => return Ok(None),
            Err(e) => return Err(e),
        };
        Ok(v.available
            .checked_add(v.order_margin)
            .and_then(|x| x.checked_add(v.upnl))
            .ok())
    }
}

#[cfg(test)]
#[path = "liquidation_l1_tests.rs"]
mod liquidation_l1_tests;
