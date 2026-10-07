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
use torus_state::cf::{CF_NATIVE_BALANCES, CF_NATIVE_LIQUIDATION, CF_NATIVE_POSITIONS};

type Marks = BTreeMap<MarketId, FixedPoint>;

#[cfg(test)]
thread_local! {
    /// Tests: the ADL drain's caches (P1 trader set, P3 ranking AV) off =
    /// the reference path (a fresh trader set and AV per ranking).
    static ADL_CACHES_OFF: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// Tests: (AV cache hits, traders dropped from the set, sets taken).
    static ADL_CACHE_STATS: std::cell::Cell<(usize, usize, usize)> = const { std::cell::Cell::new((0, 0, 0)) };
    /// Tests: C2 off = the drain ranks every trader of the set (C1, the
    /// reference) even with the records attached.
    static ADL_C2_OFF: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// Tests: (rankings over a holder list, Σ traders of the set the holder
    /// lists left out).
    static ADL_C2_STATS: std::cell::Cell<(usize, usize)> = const { std::cell::Cell::new((0, 0)) };
    /// Tests (s99): rankings whose charged holder count was checked against
    /// a state walk.
    static ADL_UNIT_CHECKS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// Tests (adl-dirty-check): the rankings' dirty checks answered by the
    /// drain's set, by `layer_touches`.
    static ADL_DIRTY_STATS: std::cell::Cell<(usize, usize)> = const { std::cell::Cell::new((0, 0)) };
}

/// What one step did — telemetry only (metrics and logs), never read by
/// execution.
#[derive(Default)]
struct LiqStats {
    /// Accounts classified (the vault excluded).
    scanned: u64,
    /// Accounts acted on within the act budget (the vault excluded).
    acted: u64,
    stage1: u64,
    backstop: u64,
    /// ADL'd accounts, the vault's own ADL included.
    adl: u64,
    vault_adl: bool,
    /// Whether the step wrote or deleted any pending row.
    pending_changed: bool,
    /// The scan window's candidates (ascending, vault excluded) the act
    /// budget left unclassified; empty when the budget held.
    deferred: Vec<Address>,
    /// adl-budget P2: obligation rows the drain worked on (waiting rows not
    /// counted), the block's ADL work units (s99: B's own transfers + the
    /// drain's), the escrow dust swept to the vault and the vault's
    /// escrow-pairing amounts (signed raw units, this step).
    adl_obligations: u64,
    adl_work: u64,
    adl_dust: i128,
    adl_pairing: i128,
    /// Fix list b (18c review): obligation rows this step wrote (B) and
    /// deleted (drained or paired to 0): the queue gauge's running count.
    adl_rows_added: u64,
    adl_rows_removed: u64,
    /// s99 review LOW 1: the obligation rows left, exact, when the drain
    /// ended at the queue's end (it saw every remaining row; nothing writes
    /// a row after it): replaces the running count, so an overcount from a
    /// write or delete that skipped the counters corrects itself.
    adl_rows_left: Option<u64>,
    /// The step's marks (A7: the escrow gauges and the value sum value at
    /// them).
    marks: Marks,
}

/// adl-budget A8 perf (P1 + P3, bit-identical): the ADL drain's caches for
/// one block, built from state at the drain (never carried over a block).
///
/// * `traders`: `liq_traders_after(None, MAX)` taken once, at the drain's
///   first ranking that needs it (C1: no records; with C2 only the test
///   shadow), after block B's transfers, then kept equal to a fresh
///   read: during the drain the only writes are closes that move size from
///   an escrow to an existing opposite holder (`adl_close`: q <= the
///   holder's size, never a flip), escrow pairings (`cross_close`: the two
///   escrows' positions, the vault's BALANCE) and obligation rows — so no
///   trader gains a first position key, and one that loses its last is
///   dropped ([`Self::touched`]). (A trader gone flat would be skipped by
///   `get_position` anyway; the drop keeps the set equal to a fresh one,
///   asserted in tests.) The dust sweep runs after the last ranking.
/// * `seen` (s99 units, consensus): every trader the block's drain valued
///   for a ranking. A valuation costs 1 unit the first time a trader is
///   valued in the block (never removed: a re-valuation after a close is
///   covered by the close's read unit). Kept with the caches on AND off, so
///   the units do not depend on them.
/// * `av`: the ranking AV per trader — a function of its balance, its
///   positions and the block's fixed marks / configs — used only as a
///   lookup (never iterated); dropped for both parties of every close and
///   for the vault on a pairing, i.e. for every account the drain writes.
/// * `dirty` (adl-budget C2): the block's dirty traders per market
///   ([`trader_positions::dirty_by_market`]), taken at the drain's first
///   ranking with R attached. A ranking of `m` reads R's holders of `m`
///   merged with them. That list covers every holder of `m` for the whole
///   drain for the reason `traders` stays exact: the drain gives no trader a
///   new key. (A listed trader gone flat reads no position and is skipped.)
/// * `dirty_traders` (adl-dirty-check, node-local): the traders the block
///   wrote under ([`trader_positions::dirty_traders`]), taken with `dirty`
///   and grown by every drain write ([`Self::touched`]), so it equals
///   `layer_touches` at every ranking (asserted in tests). The ranking's
///   reader asks it instead of a `layer_touches` per read.
#[derive(Default)]
struct DrainCache {
    traders: Option<Vec<Address>>,
    av: HashMap<Address, FixedPoint>,
    dirty: Option<HashMap<MarketId, Vec<Address>>>,
    dirty_traders: Option<std::collections::HashSet<Address>>,
    seen: std::collections::HashSet<Address>,
}

impl DrainCache {
    /// The drain wrote `t`'s position in `m` (and balance): drop its AV, and
    /// drop it from the set once it holds no position row (one seek, only
    /// when its row in `m` is gone).
    fn touched<T: StateBackend>(&mut self, ctx: &NativeExecContext<T>, t: &Address, m: MarketId) -> Result<(), CoreError> {
        self.av.remove(t);
        if let Some(set) = self.dirty_traders.as_mut() {
            set.insert(*t);
        }
        let Some(list) = self.traders.as_mut() else { return Ok(()) };
        let Ok(i) = list.binary_search(t) else { return Ok(()) };
        if ctx.positions.get_position(t, m)?.is_none() && !trader_positions::has_key(&ctx.state, t)? {
            list.remove(i);
            #[cfg(test)]
            ADL_CACHE_STATS.with(|s| s.set((s.get().0, s.get().1 + 1, s.get().2)));
        }
        Ok(())
    }
}

impl NativeExecutor {
    /// Item 3 block step (after `drain_core_writer`, before governance): stage 1
    /// (book) / backstop (vault) / ADL on the block-start mark. A storage error
    /// is a node fault -> `fatal_error`; everything else is a result.
    pub fn run_liquidations<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
    ) -> Vec<NativeActionResult> {
        Self::run_liquidations_with(ctx, liq::LIQ_SCAN_PER_BLOCK, liq::LIQ_ACT_PER_BLOCK, liq::ADL_WORK_PER_BLOCK)
    }

    /// [`Self::run_liquidations`] with explicit budgets (tests): `work` is the
    /// ADL drain's per-block work units (adl-budget Q3; 0 = no drain).
    pub fn run_liquidations_with<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
        scan: usize,
        act: usize,
        work: u64,
    ) -> Vec<NativeActionResult> {
        let started = std::time::Instant::now();
        let mut stats = LiqStats::default();
        let out = match Self::liquidation_pass(ctx, scan, act, work, &mut stats) {
            Ok(r) => r,
            Err(e) => {
                ctx.fatal_error = Some(format!("liquidation step: {e}"));
                Vec::new()
            }
        };
        Self::liquidation_telemetry(ctx, &stats, started.elapsed());
        if ctx.liq_value_sum && ctx.metrics.is_some() && ctx.fatal_error.is_none() {
            Self::liquidation_value_sum(ctx);
        }
        out
    }

    /// adl-budget A7, proof-only (`ctx.liq_value_sum`, metrics attached;
    /// after the step's timer): Σ over ALL accounts (vault and escrows
    /// included) of available + order margin + UPnL at ONE common price per
    /// market — a paged walk of every balance and position row. Fix list c
    /// (18c review; s750vs: -1,740.69 in the step the marks went stale): the
    /// common price is 0 for every market (UPnL = -signed size x entry), not
    /// the mark with unmarked positions at entry, which jumped whenever a
    /// market gained or lost its mark. With OI symmetric, Σ UPnL of a market
    /// is the same at any common price (at the mark too), so the sum stays
    /// constant across a drain without transfers (within the escrow dust).
    /// Exact (`FixedPoint`, i128); the gauge is its f64. Sets the gauge and
    /// logs `liquidation: value sum`; a read error skips it.
    fn liquidation_value_sum<T: StateBackend>(ctx: &NativeExecContext<T>) {
        const PAGE: usize = 1_024;
        let of = |_| CoreError::Overflow("liquidation value sum overflows i128".into());
        let borsh = |e: std::io::Error| CoreError::Borsh(e.to_string());
        let walk = || -> Result<FixedPoint, CoreError> {
            let mut sum = FixedPoint::ZERO;
            for cf in [CF_NATIVE_BALANCES, CF_NATIVE_POSITIONS] {
                let mut start = Vec::new();
                loop {
                    let page = ctx.state.iterate_cf_from(cf, &start, PAGE)?;
                    for (k, v) in &page {
                        let x = if cf == CF_NATIVE_BALANCES {
                            if k.len() != 20 {
                                continue;
                            }
                            let b = <NativeBalance as borsh::BorshDeserialize>::try_from_slice(v).map_err(borsh)?;
                            b.available.checked_add(b.order_margin).map_err(of)?
                        } else {
                            if k.len() != 28 {
                                continue;
                            }
                            let p = <Position as borsh::BorshDeserialize>::try_from_slice(v).map_err(borsh)?;
                            torus_core::margin::position_terms(&p, Some(FixedPoint::ZERO), None)?.upnl
                        };
                        sum = sum.checked_add(x).map_err(of)?;
                    }
                    match page.last() {
                        Some((k, _)) if page.len() == PAGE => start = [k.as_slice(), &[0u8]].concat(),
                        _ => break,
                    }
                }
            }
            Ok(sum)
        };
        match walk() {
            Ok(sum) => {
                if let Some(ref m) = ctx.metrics {
                    m.liquidation_value_sum.set(sum.raw() as f64 / FixedPoint::SCALE as f64);
                }
                tracing::info!(height = ctx.block_height, value_sum = %sum, "liquidation: value sum");
            }
            Err(e) => tracing::debug!(%e, "liquidation telemetry: value sum unreadable"),
        }
    }

    /// adl-budget A7 (metrics only, read-only): the obligation rows, and Σ
    /// over both escrows of |size| x mark (notional) and of available + UPnL
    /// at the step's marks (the queue's deficit; unmarked: entry).
    fn adl_queue_telemetry<T: StateBackend>(
        ctx: &NativeExecContext<T>,
        s: &LiqStats,
        m: &torus_telemetry::Metrics,
    ) -> Result<(u64, FixedPoint, FixedPoint), CoreError> {
        let of = |_| CoreError::Overflow("adl escrow telemetry overflows i128".into());
        let marks = &s.marks;
        let rows = Self::adl_queue_rows(ctx, s, m)?;
        let (mut notional, mut deficit) = (FixedPoint::ZERO, FixedPoint::ZERO);
        for e in [liq::ADL_ESCROW_LONG, liq::ADL_ESCROW_SHORT] {
            let b = ctx.positions.get_native_balance(&e)?;
            deficit = deficit.checked_add(b.available).and_then(|d| d.checked_add(b.order_margin)).map_err(of)?;
            for p in ctx.positions.positions_for_trader(&e)? {
                let t = torus_core::margin::position_terms(&p, marks.get(&p.market_id).copied(), None)?;
                notional = notional.checked_add(t.notional).map_err(of)?;
                deficit = deficit.checked_add(t.upnl).map_err(of)?;
            }
        }
        Ok((rows, notional, deficit))
    }

    /// Fix list b (18c review: `tag_count` read the whole queue every block
    /// with metrics, O(queue) per block during an S=750 drain): the
    /// obligation rows as a running count kept on the Metrics instance —
    /// the last count + the rows this step wrote - the rows it deleted. A
    /// drain that reached the queue's end gives the exact rows left (no read;
    /// this corrects an overcount, s99 review LOW 1). An empty queue is one
    /// seek (and resets the count to 0); the rows are counted only without a
    /// count (a start, or after a read error / fatal step: -1) or when the
    /// running count says 0 while rows exist.
    fn adl_queue_rows<T: StateBackend>(
        ctx: &NativeExecContext<T>,
        s: &LiqStats,
        m: &torus_telemetry::Metrics,
    ) -> Result<u64, CoreError> {
        use std::sync::atomic::Ordering::Relaxed;
        let rows = if let Some(left) = s.adl_rows_left {
            left
        } else if !ctx.state.prefix_exists(CF_NATIVE_LIQUIDATION, &[liq::ADL_OBLIGATION_TAG])? {
            0
        } else {
            let running = u64::try_from(m.liquidation_adl_queue_rows_cache.load(Relaxed))
                .ok()
                .and_then(|c| c.checked_add(s.adl_rows_added)?.checked_sub(s.adl_rows_removed));
            match running {
                Some(n) if n > 0 => n,
                _ => liq::tag_count(&ctx.state, liq::ADL_OBLIGATION_TAG)?,
            }
        };
        m.liquidation_adl_queue_rows_cache.store(rows as i64, Relaxed);
        Ok(rows)
    }

    /// Node-local telemetry of one step (after its timer stopped): the
    /// counters, the pending gauge and the log line (info when the step acted,
    /// else debug; `pending` only with metrics attached). Read-only: a read
    /// error skips the pending value, never the block.
    fn liquidation_telemetry<T: StateBackend>(
        ctx: &NativeExecContext<T>,
        s: &LiqStats,
        took: std::time::Duration,
    ) {
        let happened = s.acted > 0 || s.vault_adl || s.adl_obligations > 0;
        // Pending = pending rows ∪ deferred window candidates (metrics only).
        // The row count is re-read only when this step changed a row or the
        // Metrics instance has none yet (a start; a read error resets it); a
        // deferred candidate was not scanned, so its row is as before.
        let pending = match ctx.metrics.as_ref() {
            Some(m) if ctx.fatal_error.is_none() => {
                use std::sync::atomic::Ordering::Relaxed;
                let cached = m.liquidation_pending_rows_cache.load(Relaxed);
                let rows = if s.pending_changed || cached < 0 {
                    liq::tag_count(&ctx.state, liq::PENDING_TAG)
                } else {
                    Ok(cached as u64)
                };
                match rows.and_then(|r| Ok((r, liq::pending_among(&ctx.state, &s.deferred)?))) {
                    Ok((rows, both)) => {
                        m.liquidation_pending_rows_cache.store(rows as i64, Relaxed);
                        Some(rows + s.deferred.len() as u64 - both)
                    }
                    Err(e) => {
                        m.liquidation_pending_rows_cache.store(-1, Relaxed);
                        tracing::debug!(%e, "liquidation telemetry: pending rows unreadable");
                        None
                    }
                }
            }
            _ => None,
        };
        if let Some(ref m) = ctx.metrics {
            m.liquidation_step_seconds.observe(took.as_secs_f64());
            m.liquidations_stage1.inc_by(s.stage1);
            m.liquidations_backstop.inc_by(s.backstop);
            m.liquidations_adl.inc_by(s.adl);
            m.liquidation_scanned.inc_by(s.scanned);
            m.liquidation_acted.inc_by(s.acted);
            m.liquidation_deferred.set(s.deferred.len() as i64);
            if let Some(p) = pending {
                m.liquidation_pending.set(p as i64);
            }
            let tokens = |raw: i128| raw as f64 / FixedPoint::SCALE as f64;
            m.liquidation_adl_work_total.inc_by(s.adl_work);
            m.liquidation_adl_dust.inc_by(tokens(s.adl_dust));
            m.liquidation_adl_pairing.inc_by(tokens(s.adl_pairing));
            if ctx.fatal_error.is_none() {
                match Self::adl_queue_telemetry(ctx, s, m) {
                    Ok((rows, notional, deficit)) => {
                        m.liquidation_adl_queue.set(rows as i64);
                        m.liquidation_adl_escrow_notional.set(tokens(notional.raw()));
                        m.liquidation_adl_queue_deficit.set(tokens(deficit.raw()));
                    }
                    Err(e) => {
                        m.liquidation_adl_queue_rows_cache.store(-1, std::sync::atomic::Ordering::Relaxed);
                        tracing::debug!(%e, "liquidation telemetry: ADL queue unreadable");
                    }
                }
            } else {
                // A failed step's writes are not the block's: count again.
                m.liquidation_adl_queue_rows_cache.store(-1, std::sync::atomic::Ordering::Relaxed);
            }
        }
        let ms = took.as_secs_f64() * 1e3;
        if happened {
            tracing::info!(
                height = ctx.block_height,
                scanned = s.scanned,
                acted = s.acted,
                stage1 = s.stage1,
                backstop = s.backstop,
                adl = s.adl,
                vault_adl = s.vault_adl,
                deferred = s.deferred.len(),
                pending = ?pending,
                adl_obligations = s.adl_obligations,
                adl_work = s.adl_work,
                adl_dust = %FixedPoint::from_raw(s.adl_dust),
                adl_pairing = %FixedPoint::from_raw(s.adl_pairing),
                ms,
                "liquidation step"
            );
        } else {
            tracing::debug!(height = ctx.block_height, scanned = s.scanned, ms, "liquidation step: nothing acted on");
        }
    }

    /// Item 3: whether the liquidation step has pending work without any new
    /// action — a cooldown row (a chunked account: whole-position orders for
    /// 30 s, then the next chunk), the cursor row
    /// (a cut pass), review M2, a pending row (an account left under MM by
    /// its last action, e.g. a thin book) or, adl-budget P2, an ADL
    /// obligation row (escrow still to drain). Reads the block's overlay (DB
    /// + parent layer).
    pub fn liquidation_due<T: StateBackend>(state: &T) -> Result<bool, CoreError> {
        Ok(state.prefix_exists(CF_NATIVE_LIQUIDATION, &[liq::COOLDOWN_TAG])?
            || state.prefix_exists(CF_NATIVE_LIQUIDATION, &[liq::PENDING_TAG])?
            || state.get_cf_raw(CF_NATIVE_LIQUIDATION, &liq::CURSOR_KEY)?.is_some()
            || state.prefix_exists(CF_NATIVE_LIQUIDATION, &[liq::ADL_OBLIGATION_TAG])?)
    }

    fn liquidation_pass<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
        scan: usize,
        act: usize,
        work: u64,
        stats: &mut LiqStats,
    ) -> Result<Vec<NativeActionResult>, CoreError> {
        let listed = ctx
            .governance
            .listed_market_ids()
            .map_err(|e| CoreError::InvalidInput(format!("listed markets: {e}")))?;
        let marks: Marks = {
            let reader = AccountReader::of(ctx);
            listed.iter().filter_map(|&m| reader.mark(m).map(|p| (m, p))).collect()
        };
        let (prev, mark_rows) = liq::adl_bases(&ctx.state, &listed, &marks)?;
        let l1 = Self::l1_on(ctx, &listed);
        // C1 (decided, s517): no separate index — walk CF_NATIVE_POSITIONS
        // (sorted by trader) from the round-robin cursor. SCAN + 4: the vault
        // and the two ADL escrows (skipped: never classified, P2) are at most
        // three of them, so a pass that consumes every fetched trader without
        // hitting a budget has reached the end.
        let cursor = liq::cursor(&ctx.state)?;
        let accounts = Self::liq_traders_after(ctx, cursor, scan.saturating_add(4))?;
        let mut results = Vec::new();
        let (mut scanned, mut acted, mut last, mut cut) = (0usize, 0usize, None, false);
        // adl-budget s99 (owner decision 3): block B's own work, charged into
        // W before the drain (`ADL_TRANSFER_UNITS` per position moved to an
        // escrow). B itself is never cut by it.
        let mut b_work = 0u64;
        let not_protocol = |a: &&Address| **a != LIQUIDATOR_VAULT && !liq::is_adl_escrow(a);
        for (i, &trader) in accounts.iter().enumerate().filter(|(_, a)| not_protocol(a)) {
            if scanned == scan || acted == act {
                cut = true;
                if acted == act {
                    // Only the window: the walk fetched scan + 4 candidates.
                    stats.deferred =
                        accounts[i..].iter().filter(not_protocol).take(scan - scanned).copied().collect();
                }
                break;
            }
            scanned += 1;
            stats.scanned += 1;
            last = Some(trader);
            let h = match Self::liq_view(ctx, &marks, &trader, l1)?.map(|v| liq::classify(&v)) {
                Some(Some(h)) => h,
                _ => {
                    // M2: nothing the step can do until a mark returns (the
                    // oracle step makes those blocks due).
                    stats.pending_changed |= liq::set_pending(&ctx.state, &trader, false)?;
                    results.push(NativeActionResult::err(
                        "liquidation",
                        format!("{trader}: skipped (no marked position / overflow / isolated)"),
                    ));
                    continue;
                }
            };
            if h == Health::Healthy {
                liq::clear_cooldown(&ctx.state, &trader)?;
                stats.pending_changed |= liq::set_pending(&ctx.state, &trader, false)?;
                continue;
            }
            acted += 1;
            stats.acted += 1;
            match h {
                Health::Stage1 => stats.stage1 += 1,
                Health::Backstop => stats.backstop += 1,
                Health::Adl => stats.adl += 1,
                Health::Healthy => {}
            }
            if let Some(ref m) = ctx.metrics {
                m.liquidations_triggered.inc();
            }
            // D4: every resting order and pending stop goes first (sorted
            // markets), reservations released. AV is unchanged.
            let release = Self::cancel_orders_and_stops(ctx, &trader, None);
            Self::release_order_margin(ctx, &trader, release);
            match h {
                Health::Adl => {
                    let rows = Self::adl_to_escrow(ctx, &marks, &prev, &trader)?;
                    stats.adl_rows_added += rows;
                    b_work = b_work.saturating_add(rows.saturating_mul(liq::ADL_TRANSFER_UNITS));
                }
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
            stats.pending_changed |= Self::mark_pending(ctx, &marks, l1, &trader)?;
            results.push(NativeActionResult::ok("liquidation", 3000));
        }
        // D8: the vault is exempt from stage 1 / backstop; ADL when its AV < 0.
        // (`classify` is Adl exactly when AV < 0, overflow-checked.)
        if let Some(v) = Self::liq_view(ctx, &marks, &LIQUIDATOR_VAULT, l1)? {
            if liq::classify(&v) == Some(Health::Adl) {
                stats.adl += 1;
                stats.vault_adl = true;
                let rows = Self::adl_to_escrow(ctx, &marks, &prev, &LIQUIDATOR_VAULT)?;
                stats.adl_rows_added += rows;
                b_work = b_work.saturating_add(rows.saturating_mul(liq::ADL_TRANSFER_UNITS));
            }
        }
        // adl-budget P2: the escrows close their obligations under what B
        // left of `work` (s99: none when B's own work reached W; the rows
        // wait for the next block). An empty queue costs one seek.
        stats.adl_work = b_work;
        if b_work < work && liq::next_obligation(&ctx.state, &[liq::ADL_OBLIGATION_TAG])?.is_some() {
            Self::adl_drain(ctx, &listed, &marks, work, b_work, stats)?;
        }
        // M2 for the vault: pending while it stays ADL-able — after the drain
        // (the vault is an ordinary ADL candidate: a close can sink it; the
        // row keeps the step due and the next block's D8 check acts).
        let vault_adl = Self::liq_view(ctx, &marks, &LIQUIDATOR_VAULT, l1)?
            .is_some_and(|v| liq::classify(&v) == Some(Health::Adl));
        stats.pending_changed |= liq::set_pending(&ctx.state, &LIQUIDATOR_VAULT, vault_adl)?;
        liq::put_mark_rows(&ctx.state, &listed, &marks, &mark_rows)?;
        liq::put_cursor(&ctx.state, if cut { last } else { None })?;
        // Metric: the vault's deficit (negative cash it absorbed, D9). A pure
        // point read, only with metrics attached; a read error skips the update
        // so observability never changes what the step does.
        if let Some(ref m) = ctx.metrics {
            if let Ok(b) = ctx.positions.get_native_balance(&LIQUIDATOR_VAULT) {
                let deficit = match b.available.raw() {
                    raw if raw < 0 => raw.unsigned_abs() as f64 / FixedPoint::SCALE as f64,
                    _ => 0.0,
                };
                m.liquidator_vault_deficit.set(deficit);
            }
        }
        stats.marks = marks;
        Ok(results)
    }

    /// Item 6 E2: the walk's candidates (`liq::traders_after(&ctx.state,
    /// after, limit)`) from the slot's sorted trader set when the context
    /// holds the decoded records with R attached (traders the block wrote are
    /// looked up through the overlay); else the walk itself (one seek per
    /// trader).
    fn liq_traders_after<T: StateBackend>(
        ctx: &NativeExecContext<T>,
        after: Option<Address>,
        limit: usize,
    ) -> Result<Vec<Address>, CoreError> {
        let Some(records) = ctx.sums.as_ref().and_then(|s| s.records.as_ref()) else {
            return liq::traders_after(&ctx.state, after, limit);
        };
        let Some(out) = records.traders_after(&ctx.state, after, limit)? else {
            return liq::traders_after(&ctx.state, after, limit);
        };
        #[cfg(test)]
        if let Some(s) = ctx.sums.as_ref() {
            bump(&s.counters.traders_slice);
            if s.shadow {
                let want = liq::traders_after(&ctx.state, after, limit);
                if want.as_ref().ok() != Some(&out) {
                    s.shadow_mismatches.lock().unwrap().push(format!("traders_after {after:?} {limit}: slot {out:?}, walk {want:?}"));
                }
            }
        }
        Ok(out)
    }

    /// adl-budget Q1 (C1): `m`'s counterparties on side `want_long` — a point
    /// read of every trader of the positions CF (the slot's sorted set merged
    /// with the block's dirty traders, else the walk), through the records for a
    /// clean trader and the overlay for a dirty one; escrows skipped. Returns the
    /// candidates and the ranking's work units (Q3, s99 below).
    /// A8 perf: the set and the ranking AV come from the drain's `cache`
    /// ([`DrainCache`]: the same values as a fresh read, see there).
    ///
    /// C2 (owner 18c s96 / s99): with the records attached the point reads go
    /// only to the traders that may hold `m` — R's holders of `m` merged with
    /// the block's dirty traders of `m` ([`TraderPositions::holders_with`]),
    /// ascending — instead of every trader: the same candidates in the same
    /// order (every holder is in the list, a listed non-holder is skipped as
    /// C1 skips it), shadow-checked against C1 at every ranking in tests. The
    /// trader set is then not taken at all (only the test shadow reads it).
    /// Without records: C1.
    ///
    /// Units (owner 18c s99, decisions 2 and 3; consensus): the HOLDERS of
    /// `m` — the traders `get_position` finds a position row for in `m` at
    /// ranking time, the two escrows never counted (`adl_candidates` skips
    /// them; the vault counts, it is a candidate) — plus 1 per candidate
    /// valued for the first time in the block's drain (`cache.seen`). Both
    /// lists hold every holder once and read the same overlay, so C1 and C2
    /// count the same exact live number: C2's extra entries (dirty keys the
    /// block deleted) read no row and are not counted. Asserted in tests at
    /// every ranking against a walk of `CF_NATIVE_POSITIONS`, and C2's count
    /// against the shadow's C1 count.
    fn adl_candidates_of<T: StateBackend>(
        ctx: &NativeExecContext<T>,
        m: MarketId,
        want_long: bool,
        cache: &mut DrainCache,
    ) -> Result<(Vec<liq::AdlCandidate>, u64), CoreError> {
        #[cfg(test)]
        let on = !ADL_CACHES_OFF.with(|c| c.get());
        #[cfg(not(test))]
        let on = true;
        #[cfg(test)]
        let c2 = !ADL_C2_OFF.with(|c| c.get());
        #[cfg(not(test))]
        let c2 = true;
        #[cfg(test)]
        let shadow = ctx.sums.as_ref().is_some_and(|s| s.shadow);
        #[cfg(not(test))]
        let shadow = false;
        let fresh_set;
        let fresh_dirty;
        let DrainCache { traders, av, dirty: dirty_cache, dirty_traders, seen } = cache;
        // C2: the holder list of `m` when the records are attached (with R:
        // the dirty map exists), else every trader (C1).
        let records = ctx.sums.as_ref().and_then(|s| s.records.as_ref()).filter(|_| c2);
        let dirty = match records {
            None => None,
            Some(_) if on => {
                if dirty_cache.is_none() {
                    *dirty_cache = trader_positions::dirty_by_market(&ctx.state);
                    *dirty_traders = trader_positions::dirty_traders(&ctx.state);
                }
                dirty_cache.as_ref()
            }
            Some(_) => {
                fresh_dirty = trader_positions::dirty_by_market(&ctx.state);
                fresh_dirty.as_ref()
            }
        };
        let holders: Option<Vec<Address>> =
            records.zip(dirty).map(|(r, d)| r.holders_with(m, d.get(&m).map_or(&[][..], Vec::as_slice)));
        // The trader set: C1's list; with a holder list only the shadow's.
        let set: Option<&[Address]> = if holders.is_some() && !shadow {
            None
        } else if on {
            if traders.is_none() {
                *traders = Some(Self::liq_traders_after(ctx, None, usize::MAX)?);
                #[cfg(test)]
                ADL_CACHE_STATS.with(|s| s.set((s.get().0, s.get().1, s.get().2 + 1)));
                #[cfg(test)]
                if let Some(s) = ctx.sums.as_ref() {
                    bump(&s.counters.adl_trader_sets);
                }
            }
            let kept = traders.as_deref().expect("taken above");
            #[cfg(test)]
            assert_eq!(kept, liq::traders_after(&ctx.state, None, usize::MAX)?.as_slice(), "kept trader set == the walk");
            Some(kept)
        } else {
            fresh_set = Self::liq_traders_after(ctx, None, usize::MAX)?;
            Some(&fresh_set)
        };
        let list: &[Address] = match (holders.as_deref(), set) {
            (Some(h), _) => h,
            (None, Some(s)) => s,
            (None, None) => unreachable!("without a holder list the set is taken"),
        };
        let reader = AccountReader { drain_dirty: dirty_traders.as_ref(), ..AccountReader::of(ctx) };
        #[cfg(test)]
        let checks_before = DIRTY_CHECKS.with(|c| c.get());
        // C7: ranking AV with entry fallback; overflow ranks last (AV 0); a
        // storage error stays an error (fail-stop).
        let value = |t: &Address| -> Result<FixedPoint, CoreError> {
            let bal = ctx.positions.get_native_balance(t)?;
            let v = match reader.view(t, &bal) {
                Ok(v) => v,
                Err(CoreError::Overflow(_)) => return Ok(FixedPoint::ZERO),
                Err(e) => return Err(e),
            };
            Ok(v.available.checked_add(v.order_margin).and_then(|x| x.checked_add(v.upnl)).unwrap_or(FixedPoint::ZERO))
        };
        // s99 units: the holders read, the first-sight valuations.
        let (mut held, mut first_sight) = (0u64, 0u64);
        let get = |t: &Address| {
            let p = reader.get_position(t, m)?;
            held += u64::from(p.is_some());
            Ok(p)
        };
        let cands = liq::adl_candidates(list, want_long, get, |t| {
            first_sight += u64::from(seen.insert(*t));
            if !on {
                return value(t);
            }
            if let Some(&v) = av.get(t) {
                #[cfg(test)]
                {
                    assert_eq!(v, value(t)?, "ranking AV cache hit == a fresh valuation ({t})");
                    ADL_CACHE_STATS.with(|s| s.set((s.get().0 + 1, s.get().1, s.get().2)));
                }
                return Ok(v);
            }
            let v = value(t)?;
            av.insert(*t, v);
            Ok(v)
        })?;
        #[cfg(test)]
        {
            let (set, layer) = DIRTY_CHECKS.with(|c| c.get());
            ADL_DIRTY_STATS.with(|x| x.set((x.get().0 + set - checks_before.0, x.get().1 + layer - checks_before.1)));
        }
        #[cfg(test)]
        if let Some(s) = ctx.sums.as_ref() {
            bump(&s.counters.adl_rankings);
            if let Some(h) = holders.as_ref() {
                bump(&s.counters.adl_holder_lists);
                let skipped = set.map_or(0, |t| t.len().saturating_sub(h.len()));
                ADL_C2_STATS.with(|x| x.set((x.get().0 + 1, x.get().1 + skipped)));
                if let Some(all) = set.filter(|_| s.shadow) {
                    // C2 == C1: the whole set, the same reads and valuation,
                    // the same holder count (s99 units).
                    let mut c1_held = 0u64;
                    let c1_get = |t: &Address| {
                        let p = reader.get_position(t, m)?;
                        c1_held += u64::from(p.is_some());
                        Ok(p)
                    };
                    let want = liq::adl_candidates(all, want_long, c1_get, |t| value(t));
                    if want.as_ref().ok() != Some(&cands) || c1_held != held {
                        s.shadow_mismatches.lock().unwrap().push(format!(
                            "adl_candidates {m} long={want_long}: holders {cands:?} ({held} held), C1 {want:?} ({c1_held} held)"
                        ));
                    }
                }
            }
        }
        #[cfg(test)]
        {
            // s99 (owner decision 2b): the charged count == the holders of
            // `m` from a full walk of the block's overlay (escrows out).
            let walk = ctx
                .state
                .iterate_cf(CF_NATIVE_POSITIONS, None)?
                .iter()
                .filter(|(k, _)| {
                    k.len() == 28 && k[20..] == m.to_be_bytes() && !liq::is_adl_escrow(&Address::from_slice(&k[..20]))
                })
                .count() as u64;
            assert_eq!(held, walk, "the ranking of {m} charges its holders (a state walk)");
            ADL_UNIT_CHECKS.with(|c| c.set(c.get() + 1));
        }
        Ok((cands, held + first_sight))
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
            Ok(s) if s.any_isolated() || s.marked == 0 => None,
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
    /// still valuable and not healthy. Returns whether the row changed.
    fn mark_pending<T: StateBackend>(
        ctx: &NativeExecContext<T>,
        marks: &Marks,
        l1: bool,
        trader: &Address,
    ) -> Result<bool, CoreError> {
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
    /// * Outside the cooldown every position goes by its own rule in the
    ///   same block (rule B, owner decision s91): above 100,000 notional a
    ///   20% chunk (D2), otherwise the entire position. A chunk does not end
    ///   the account's stage 1 for the block; the next position is ordered
    ///   unless `AV >= MM`. If the block placed a chunk, the cooldown row is
    ///   written once, at the block time, after the loop. Evidence (s91, HL
    ///   public API, 44 liquidated accounts, 270 orders, orderStatus
    ///   origSz): one block (same hash) of account 0xb0fb held seven 20%
    ///   orders (HYPE 1.25M, NEAR 285k, MNT 211k, ETH 206k, XPL 188k, LINK
    ///   155k, MON 109k); in its next episode MON at 87.6k went whole with
    ///   six others at 20%. Was rule A (2d03111): a chunk ended the
    ///   account's stage 1 for the block.
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
        let mut any_chunk = false;
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
            let r = Self::place_order_inner(ctx, trader, &params, None, &mut queue, true);
            Self::run_triggered_stops(ctx, queue);
            if !r.success {
                results.push(r);
            }
            any_chunk |= chunked;
            match Self::liq_view(ctx, marks, trader, l1)? {
                Some(v) if liq::classify(&v) == Some(Health::Healthy) => break,
                None => break, // flat (or no longer valuable): nothing left to do
                _ => {}
            }
        }
        if any_chunk {
            liq::set_cooldown(&ctx.state, trader, ctx.timestamp)?;
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

    /// adl-budget P2 (owner s96): terms fixed at B. Every MARKED position of
    /// `u` (ascending market) moves to the escrow of its side at its ADL price
    /// — the rule-H base (the mark without one) clamped one-sided to `u`'s
    /// bankruptcy price (review H1, unchanged) — and its obligation is queued.
    /// `rest` (cash + the UPnL of u's OTHER positions: marked at the mark,
    /// unmarked at entry = 0) is kept running: rows read once, one balance
    /// point read per transfer — O(P) (was `adl_rest` per market: O(P^2)).
    /// Then D9: a non-vault account without marked positions hands its
    /// remaining collateral to the vault. Never positive under the clamp (an
    /// error line if it is; the move still conserves value): flat, exactly 0.
    /// Returns the obligation rows written (telemetry: the queue count).
    fn adl_to_escrow<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
        marks: &Marks,
        prev: &Marks,
        u: &Address,
    ) -> Result<u64, CoreError> {
        let ps = ctx.positions.positions_for_trader(u)?;
        // `build`'s terms per position (adl_rest's `build`); None = overflow
        // -> no bankruptcy price (adl_rest's None).
        // Review L2 (accepted, no behavior change): one position's UPnL
        // overflow makes `total` None, so `rest` is None for EVERY later
        // position too, while `adl_rest` excludes only the overflowing one.
        // Production then clamps those to the mark instead of a bankruptcy
        // price (`adl_price`'s fallback: still never a positive D9
        // remainder); only the test-only shadow assert below would differ.
        // It needs ~1e30-token notional: unreachable.
        let upnl = |p: &Position| {
            torus_core::margin::position_terms(p, marks.get(&p.market_id).copied(), None).ok().map(|t| t.upnl)
        };
        let mut total = ps.iter().try_fold(FixedPoint::ZERO, |a, p| a.checked_add(upnl(p)?).ok());
        let mut rows = 0u64;
        for p in &ps {
            let m = p.market_id;
            let Some(&mark) = marks.get(&m) else { continue };
            let own = upnl(p);
            let bal = ctx.positions.get_native_balance(u)?;
            let rest = (|| {
                let others = total?.checked_sub(own?).ok()?;
                bal.available.checked_add(bal.order_margin).ok()?.checked_add(others).ok()
            })();
            #[cfg(test)]
            assert_eq!(rest, Self::adl_rest(ctx, marks, u, m)?, "running rest == adl_rest ({u}, {m})");
            let base = prev.get(&m).copied().unwrap_or(mark);
            let bankruptcy = rest.and_then(|r| liq::bankruptcy_price(r, p.is_long, p.size, p.entry_price));
            let px = liq::adl_price(base, bankruptcy, mark, p.is_long);
            liq::transfer(&ctx.positions, u, &liq::adl_escrow(p.is_long), m, p.size, px)?;
            total = (|| total?.checked_sub(own?).ok())();
            let o = liq::Obligation { height: ctx.block_height, market: m, is_long: p.is_long, trader: *u, size: p.size, price: px };
            liq::put_obligation(&ctx.state, &o)?; // size > 0: a new row (insert-only)
            rows += 1;
            tracing::info!(
                height = ctx.block_height,
                account = %u,
                market = m,
                size = %p.size,
                base = %base,
                bankruptcy = ?bankruptcy,
                price = %px,
                "liquidation: ADL to escrow"
            );
        }
        if *u != LIQUIDATOR_VAULT
            && !ctx.positions.positions_for_trader(u)?.iter().any(|p| marks.contains_key(&p.market_id))
        {
            let moved = liq::move_collateral(&ctx.positions, u, &LIQUIDATOR_VAULT)?;
            if moved > FixedPoint::ZERO {
                tracing::error!(account = %u, %moved, "liquidation: positive D9 remainder after ADL (clamp invariant broken)");
            }
        }
        Ok(rows)
    }

    /// Review H1: `u`'s collateral + the UPnL of its positions OTHER than in
    /// `m` (marked at the mark, unmarked at entry = 0). `None` on overflow.
    /// adl-budget P2: the shadow reference of `adl_to_escrow`'s running
    /// `rest` (tests only).
    #[cfg(test)]
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

    /// adl-budget P2 / Q2 / Q3: drain the obligation rows in key order under
    /// `work` units, `spent` of them already used by block B's own work (s99).
    /// One step = one row (atomic; starts only while used <
    /// work, so a block overshoots by at most one step). Every visited row
    /// costs 1, plus its (market, side) ranking (once per block: `ranked`
    /// holds the list and the position of the first candidate not yet used
    /// up, so later rows continue where the last one stopped; s99: the
    /// market's holders + first-sight valuations, [`Self::adl_candidates_of`]),
    /// plus the candidates `adl_close` read, plus edge rows. The escrow of the row's
    /// side closes at the row's stored price; real holders exhausted ->
    /// [`Self::adl_cross`] (one pairing position per market and block); a
    /// row still open after both breaks escrow size = Σ rows and is fatal
    /// (review M1). A listed market without a usable mark waits; a delisted
    /// one ranks at the stored price (owner s96). Then a flat escrow's
    /// balance (dust) goes to the vault.
    fn adl_drain<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
        listed: &[MarketId],
        marks: &Marks,
        work: u64,
        spent: u64,
        stats: &mut LiqStats,
    ) -> Result<(), CoreError> {
        let mut ranked: BTreeMap<(MarketId, bool), (Vec<liq::AdlCandidate>, usize)> = BTreeMap::new();
        let mut paired: BTreeMap<MarketId, Vec<u8>> = BTreeMap::new();
        let mut cache = DrainCache::default();
        let (mut used, mut start) = (spent, vec![liq::ADL_OBLIGATION_TAG]);
        // Telemetry (s99 review LOW 1): the rows this drain left in place. A
        // visited row either waits or closes to 0 (else the step is fatal);
        // a row the pairing deleted ahead is never visited, one it reduced is
        // visited later (its key is after the current row's).
        let (mut waiting, mut at_end) = (0u64, false);
        while used < work {
            let Some(mut o) = liq::next_obligation(&ctx.state, &start)? else {
                at_end = true;
                break;
            };
            start = [o.key().as_slice(), &[0]].concat();
            used += 1; // the visit: a waiting row is never free
            // A delisted market ranks at a stored price (owner s96). The
            // ranking is once per (block, market, side), so it is the stored
            // price of the FIRST row of that (market, side) the block visits;
            // the later rows of the key reuse that ranking, whatever their own
            // stored price (each still closes at its own price). 18c review
            // nit g: documented, not changed.
            let rank_px = match marks.get(&o.market) {
                Some(&mark) => mark,
                None if listed.binary_search(&o.market).is_err() => o.price, // delisted
                None => {
                    waiting += 1; // listed, stale: wait
                    continue;
                }
            };
            let key = (o.market, o.is_long);
            if !ranked.contains_key(&key) {
                let (c, units) = Self::adl_candidates_of(ctx, o.market, !o.is_long, &mut cache)?;
                used += units;
                ranked.insert(key, (liq::adl_rank(rank_px, c), 0));
            }
            let escrow = liq::adl_escrow(o.is_long);
            let (list, at) = ranked.get_mut(&key).expect("ranked above");
            let (closes, next, read) = liq::adl_close(&ctx.positions, &escrow, o.market, o.price, o.size, &list[*at..])?;
            *at += next;
            used += read as u64;
            let owed = o.size;
            for (c, q) in &closes {
                tracing::debug!(market = o.market, account = %o.trader, counterparty = %c, size = %q, price = %o.price, "liquidation: ADL close");
                o.size -= *q;
                cache.touched(ctx, c, o.market)?;
            }
            if !closes.is_empty() {
                cache.touched(ctx, &escrow, o.market)?;
            }
            if o.size > FixedPoint::ZERO {
                let before = o.size;
                let from = paired.entry(o.market).or_default();
                used += Self::adl_cross(ctx, &mut o, from, stats)?;
                if o.size != before {
                    // cross_close wrote both escrows and the vault's balance.
                    cache.touched(ctx, &liq::ADL_ESCROW_LONG, o.market)?;
                    cache.touched(ctx, &liq::ADL_ESCROW_SHORT, o.market)?;
                    cache.av.remove(&LIQUIDATOR_VAULT);
                }
            }
            if o.size > FixedPoint::ZERO {
                // Review M1: the escrow holds Σ rows and OI is symmetric, so
                // the holders plus the pairing always cover a row.
                return Err(CoreError::InvalidInput(format!(
                    "ADL obligation left open after the holders and the escrow pairing (OI asymmetry): {o:?}"
                )));
            }
            if o.size != owed {
                liq::update_obligation(&ctx.state, &o)?; // deleted at 0
                stats.adl_rows_removed += u64::from(o.size == FixedPoint::ZERO);
            }
            stats.adl_obligations += 1;
        }
        for e in [liq::ADL_ESCROW_LONG, liq::ADL_ESCROW_SHORT] {
            if ctx.positions.positions_for_trader(&e)?.is_empty() {
                let dust = liq::move_collateral(&ctx.positions, &e, &LIQUIDATOR_VAULT)?;
                if dust != FixedPoint::ZERO {
                    stats.adl_dust += dust.raw();
                    tracing::info!(escrow = %e, %dust, "liquidation: ADL escrow dust to the vault");
                    // Coarse node-local alarm (the exact bound is a test assertion).
                    if dust.raw().unsigned_abs() >= FixedPoint::SCALE as u128 {
                        tracing::error!(escrow = %e, %dust, "liquidation: ADL escrow dust >= 1 token (invariant alarm)");
                    }
                }
            }
        }
        stats.adl_work = used;
        stats.adl_rows_left = at_end.then_some(waiting);
        Ok(())
    }

    /// P2 edge: the real opposite holders of `o.market` are exhausted (both
    /// escrows hold it). Pair `o` with the opposite side's rows of the same
    /// market in key order; each escrow closes at its own row's price, the
    /// vault pays the difference (`liq::cross_close`, which refuses to flip an
    /// escrow). Returns the rows scanned (work units).
    ///
    /// Review M2: `from` is the market's pairing position in this block
    /// (empty: none yet). The scan starts at the later of `from` and just
    /// after `o`: an opposite row before `o` was visited first and closed
    /// (it paired forward, `o` included, or the step went fatal), and rows
    /// an earlier scan of the market passed are not read again. It ends at
    /// the last partner (re-read next time when only partly used) or after
    /// it, so the units are linear in the queue per market and block.
    fn adl_cross<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
        o: &mut liq::Obligation,
        from: &mut Vec<u8>,
        stats: &mut LiqStats,
    ) -> Result<u64, CoreError> {
        let mut units = 0u64;
        let mut start = std::mem::take(from).max([o.key().as_slice(), &[0]].concat());
        while o.size > FixedPoint::ZERO {
            let Some(mut x) = liq::next_obligation(&ctx.state, &start)? else { break };
            units += 1;
            let after = [x.key().as_slice(), &[0]].concat();
            if x.market != o.market || x.is_long == o.is_long {
                start = after;
                continue;
            }
            let q = o.size.min(x.size);
            let (pl, ps) = if o.is_long { (o.price, x.price) } else { (x.price, o.price) };
            let paid = liq::cross_close(&ctx.positions, o.market, q, pl, ps, &LIQUIDATOR_VAULT)?;
            stats.adl_pairing += paid.raw();
            tracing::info!(market = o.market, size = %q, p_long = %pl, p_short = %ps, vault = %paid, "liquidation: ADL escrow pairing");
            o.size -= q;
            x.size -= q;
            liq::update_obligation(&ctx.state, &x)?;
            stats.adl_rows_removed += u64::from(x.size == FixedPoint::ZERO);
            start = if x.size > FixedPoint::ZERO { x.key().to_vec() } else { after };
        }
        *from = start;
        Ok(units)
    }
}

#[cfg(test)]
#[path = "liquidation_l1_tests.rs"]
mod liquidation_l1_tests;
