//! `torus_getBalances` account figures (review fix #3): the cross-margin
//! numbers the engine checks against, computed from the RPC read path only.
//!
//! Every figure is [`AccountView`] arithmetic over rows read from the DB
//! (balance, the trader's positions, one market row and one oracle aggregate
//! per market it holds), so it is what the executor computes on the same
//! rows at the same block time:
//! - placement gate (`account_check`): an order is accepted iff its need is
//!   `<= 0` or `<= available + UPnL − Σ position IM` = [`AccountView::free`];
//! - withdraw check (`check_withdrawal_margin` + the Lockbox cash check):
//!   [`AccountView::withdrawal_allowed`] and `amount <= available`;
//! - liquidation (`liq_view` + `liquidation::classify`): maintenance with
//!   positions marked only in LISTED markets; `torus_getPosition`'s cross
//!   `liquidationPrice` ([`cross_liquidation_price`], R23) is the mark at
//!   which that check flips.
//!
//! Marks follow the executor's rule (`usable_mark`): the aggregate while
//! usable at `now`, else none (the position is valued at entry). `now` is
//! the executed head's block time, so a mark that turns stale at the next
//! block is still used here.
//!
//! No executor structure is touched (torus-bridge is a dev-dependency of
//! this crate only): no resident book, sums cache, block mark table or exec
//! pipeline lock, only RocksDB point reads and one prefix scan.
//!
//! R24: the prefix scan (one row per position) and the two point reads per
//! market held (market row, oracle aggregate) are charged to a scan budget
//! ([`AccountRows::read_bounded`]); getBalances ([`account_figures_bounded`])
//! and getPosition's cross price ([`cross_liquidation_price_bounded`])
//! charge their request's budget.

use std::collections::BTreeMap;

use alloy_primitives::Address;
use torus_core::error::CoreError;
use torus_core::liquidation::{classify, Health};
use torus_core::margin::{market_margin_config, AccountView, MarginTier};
use torus_core::oracle::{OracleConfig, OracleManager};
use torus_core::position::{MarginType, NativeBalance, Position, PositionManager};
use torus_state::cf::CF_NATIVE_MARKETS;
use torus_state::StateDb;
use torus_types::{FixedPoint, MarketId};

use crate::error::RpcError;
use crate::scan::{self, ScanBudget};

/// The `torus_getBalances` margin figures of one account.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AccountFigures {
    /// Cash + order margin (`nativeBalance`), from the same balance read.
    pub native_balance: FixedPoint,
    /// Equity: cash + order margin + Σ UPnL ([`AccountView::equity`]).
    pub account_value: FixedPoint,
    /// Order margin + Σ cross position IM + Σ isolated margin.
    pub total_margin_used: FixedPoint,
    /// Σ maintenance margin as the liquidation check values it.
    pub maintenance_margin_used: FixedPoint,
    /// Ledger cash (`available`), may be negative.
    pub cash_balance: FixedPoint,
    /// Largest amount the withdraw check accepts (0 if none).
    pub withdrawable: FixedPoint,
    /// The pool a new order's IM need is checked against ([`AccountView::free`]).
    pub free_margin: FixedPoint,
}

/// Per market the trader holds: its tiers (`load_margin_configs`: a row
/// under an 8-byte key, `None` = default 20x), whether it is listed (the
/// liquidation step marks only listed markets) and its usable mark.
struct Market {
    tiers: Option<Vec<MarginTier>>,
    listed: bool,
    mark: Option<FixedPoint>,
}

/// The rows every figure is computed from: one balance read, the trader's
/// positions and one [`Market`] per market it holds.
struct AccountRows {
    bal: NativeBalance,
    positions: Vec<Position>,
    markets: BTreeMap<MarketId, Market>,
}

/// A `CoreError` as the handlers report it.
fn internal(e: CoreError) -> RpcError {
    RpcError::Internal(e.to_string())
}

impl AccountRows {
    /// The rows, charged to `b` (R24): each position row before it is
    /// decoded, and per market held its market row and (with a block time)
    /// its oracle aggregate. A read or decode failure is `Internal` with the
    /// `CoreError` text.
    fn read_bounded(
        state: &StateDb,
        trader: &Address,
        now: Option<u64>,
        b: &mut ScanBudget,
    ) -> Result<Self, RpcError> {
        let pm = PositionManager::new(state.clone());
        let bal = pm.get_native_balance(trader).map_err(internal)?;
        let positions = scan::trader_positions(state, trader, b)?;
        let oracle = OracleManager::new(state.clone(), OracleConfig::default());
        let mut markets: BTreeMap<MarketId, Market> = BTreeMap::new();
        for p in &positions {
            if markets.contains_key(&p.market_id) {
                continue;
            }
            b.step()?;
            let row = state
                .get_cf_raw(CF_NATIVE_MARKETS, &p.market_id.to_be_bytes())
                .map_err(|e| internal(e.into()))?;
            let mark = match now {
                Some(t) => {
                    b.step()?;
                    oracle
                        .get_price_opt(p.market_id, t)
                        .map_err(internal)?
                        .and_then(|op| op.usable())
                }
                None => None,
            };
            markets.insert(
                p.market_id,
                Market {
                    tiers: row
                        .as_deref()
                        .and_then(|r| market_margin_config(p.market_id, r))
                        .map(|c| c.tiers),
                    listed: row.is_some(),
                    mark,
                },
            );
        }
        Ok(Self {
            bal,
            positions,
            markets,
        })
    }

    fn tiers(&self, m: MarketId) -> Option<&[MarginTier]> {
        self.markets.get(&m).and_then(|x| x.tiers.as_deref())
    }

    /// The executor's mark (placement, withdraw): any market's usable mark.
    fn mark(&self, m: MarketId) -> Option<FixedPoint> {
        self.markets.get(&m).and_then(|x| x.mark)
    }

    /// The liquidation step's mark: listed markets only.
    fn liq_mark(&self, m: MarketId) -> Option<FixedPoint> {
        self.markets
            .get(&m)
            .filter(|x| x.listed)
            .and_then(|x| x.mark)
    }
}

/// [`AccountFigures`] of `trader` at block time `now` (`None`: no usable
/// mark, every position valued at entry), without a row limit.
pub fn account_figures(
    state: &StateDb,
    trader: &Address,
    now: Option<u64>,
) -> Result<AccountFigures, RpcError> {
    account_figures_bounded(state, trader, now, &mut ScanBudget::unlimited())
}

/// [`account_figures`] charged to `b` (R24, [`AccountRows::read_bounded`]).
/// A read or decode failure is the handler's error (`Internal` with the
/// `CoreError` text).
pub(crate) fn account_figures_bounded(
    state: &StateDb,
    trader: &Address,
    now: Option<u64>,
    b: &mut ScanBudget,
) -> Result<AccountFigures, RpcError> {
    let a = AccountRows::read_bounded(state, trader, now, b)?;
    figures(&a).map_err(internal)
}

/// The figures from the rows read.
fn figures(a: &AccountRows) -> Result<AccountFigures, CoreError> {
    let of = |_| CoreError::Overflow("account margin overflows i128".into());
    let (bal, positions) = (&a.bal, &a.positions);
    let view = AccountView::build(bal, positions, |m| a.mark(m), |m| a.tiers(m))?;
    let liq_view = AccountView::build(bal, positions, |m| a.liq_mark(m), |m| a.tiers(m))?;

    let mut total_margin_used = bal.order_margin.checked_add(view.position_im).map_err(of)?;
    for p in positions
        .iter()
        .filter(|p| p.margin_type != MarginType::Cross)
    {
        total_margin_used = total_margin_used
            .checked_add(p.isolated_margin)
            .map_err(of)?;
    }
    // `withdrawal_allowed`: amount <= available and
    // available + UPnL − amount >= transfer_required.
    let margin_bound = (view.equity() - view.order_margin)
        .checked_sub(view.transfer_required())
        .map_err(of)?;
    let withdrawable = bal.available.min(margin_bound).max(FixedPoint::ZERO);
    Ok(AccountFigures {
        native_balance: bal.available.checked_add(bal.order_margin).map_err(of)?,
        account_value: view.equity(),
        total_margin_used,
        maintenance_margin_used: liq_view.maintenance,
        cash_balance: bal.available,
        withdrawable,
        free_margin: view.free(),
    })
}

/// R23: the cross liquidation price of `trader`'s position in `market` at
/// block time `now`: the mark of `market` at which the account's equity
/// meets its maintenance margin, every other market's mark held at what the
/// liquidation step uses now (listed and usable, else entry).
///
/// Nothing is re-derived: each candidate mark is valued with the engine's
/// [`AccountView::build`] (liquidation marks) and [`classify`]; the answer
/// is the boundary between `Healthy` and liquidatable, found by bisection on
/// raw units. With the flat per-market tier every listed market has,
/// equity − MM is monotone in the mark (UPnL moves by the notional, MM by at
/// most half of it), so the boundary is unique:
/// - long: the LOWEST mark still healthy (`equity >= MM`); one raw unit
///   below, the step liquidates;
/// - short: the HIGHEST mark still healthy; one raw unit above, it
///   liquidates.
///
/// A mark whose valuation overflows i128 is indeterminate (the step skips
/// the account there): neither side. Every term is monotone in the mark, so
/// the marks the step can value form one interval; the search moves only
/// through valued marks and returns a price only when the two adjacent raw
/// marks around it are both valued, one `Healthy`, one liquidatable.
///
/// `None` when no mark of `market` crosses maintenance: no Cross position
/// there; the market is not listed (the step never marks it); the account
/// holds a non-Cross position (the step does not value such an account); a
/// long healthy even at mark 0; a short liquidatable even at mark 0 (it is
/// liquidatable at every mark); no crossing in `[0, i128::MAX]`; or the
/// crossing cannot be bracketed by valued marks (mark 0 overflows, or every
/// mark the step can value is on one side and the other side overflows).
pub fn cross_liquidation_price(
    state: &StateDb,
    trader: &Address,
    market: MarketId,
    now: Option<u64>,
) -> Result<Option<FixedPoint>, RpcError> {
    cross_liquidation_price_bounded(state, trader, market, now, &mut ScanBudget::unlimited())
}

/// [`cross_liquidation_price`] with the account's rows charged to `b`
/// (R24, [`AccountRows::read_bounded`]); the search itself reads nothing.
pub(crate) fn cross_liquidation_price_bounded(
    state: &StateDb,
    trader: &Address,
    market: MarketId,
    now: Option<u64>,
    b: &mut ScanBudget,
) -> Result<Option<FixedPoint>, RpcError> {
    let a = AccountRows::read_bounded(state, trader, now, b)?;
    let Some(pos) = a.positions.iter().find(|p| p.market_id == market) else {
        return Ok(None);
    };
    if pos.size <= FixedPoint::ZERO
        || a.positions
            .iter()
            .any(|p| p.margin_type != MarginType::Cross)
        || !a.markets.get(&market).is_some_and(|x| x.listed)
    {
        return Ok(None);
    }
    // As the step decides: `Some(liquidatable)` when valued, `None` on an
    // overflow (the step skips the account: indeterminate, neither side).
    let liquidatable = |raw: i128| {
        let px = FixedPoint::from_raw(raw);
        AccountView::build(
            &a.bal,
            &a.positions,
            |m| if m == market { Some(px) } else { a.liq_mark(m) },
            |m| a.tiers(m),
        )
        .ok()
        .and_then(|v| classify(&v))
        .map(|h| h != Health::Healthy)
    };
    // Long: liquidatable below the boundary; short: above it. Mark 0 must be
    // valued, on the liquidatable side for a long and the healthy side for a
    // short.
    let long = pos.is_long;
    let low_side = Some(long);
    if liquidatable(0) != low_side {
        return Ok(None);
    }
    // `lo` is valued and on the low side; `hi` is the first candidate not
    // known to be: doubling from the entry price, the last step clamped to
    // i128::MAX.
    let (mut lo, mut hi) = (0i128, pos.entry_price.raw().max(1));
    while liquidatable(hi) == low_side {
        if hi == i128::MAX {
            return Ok(None);
        }
        lo = hi;
        hi = hi.checked_mul(2).unwrap_or(i128::MAX);
    }
    // `hi` is on the other side or overflows. Valued marks form an interval
    // containing `lo`, so a mark that overflows lies above every valued one:
    // move `hi` down to it as to a mark on the other side.
    while hi - lo > 1 {
        let mid = lo + (hi - lo) / 2;
        if liquidatable(mid) == low_side {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    // Both endpoints valued and on opposite sides, else no bracket.
    if liquidatable(lo) != low_side || liquidatable(hi) != Some(!long) {
        return Ok(None);
    }
    // Long: `hi` is the lowest healthy mark; short: `lo` the highest.
    Ok(Some(FixedPoint::from_raw(if long { hi } else { lo })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::U256;
    use torus_bridge::native_executor::{NativeExecContext, NativeExecutor};
    use torus_core::margin::placement_need;
    use torus_types::{NativeAction, OrderType, PlaceOrderParams, TimeInForce};

    fn addr(n: u8) -> Address {
        Address::repeat_byte(n)
    }

    fn fp(v: i64) -> FixedPoint {
        FixedPoint::from_raw(v as i128 * FixedPoint::SCALE)
    }

    /// Markets listed with the test-fixture row (undecodable: default 20x,
    /// tick / lot 1).
    fn test_db(markets: &[MarketId]) -> (tempfile::TempDir, StateDb) {
        let dir = tempfile::tempdir().unwrap();
        let db = StateDb::open(dir.path()).unwrap();
        for m in markets {
            db.put_cf_raw(CF_NATIVE_MARKETS, &m.to_be_bytes(), b"listed")
                .unwrap();
        }
        (dir, db)
    }

    /// Block 1 at time 1001.
    fn ctx_of(db: &StateDb) -> NativeExecContext {
        NativeExecContext::new(
            db.clone(),
            1,
            1_001,
            0,
            1_000,
            10,
            addr(99),
            addr(100),
            addr(101),
        )
    }

    fn fund(ctx: &NativeExecContext, t: &Address, amount: FixedPoint) {
        ctx.positions
            .put_native_balance(
                t,
                &NativeBalance {
                    available: amount,
                    order_margin: FixedPoint::ZERO,
                },
            )
            .unwrap();
    }

    fn open_pair(
        ctx: &NativeExecContext,
        long: &Address,
        short: &Address,
        m: MarketId,
        qty: i64,
        px: i64,
    ) {
        ctx.positions
            .apply_fill(long, m, true, fp(qty), fp(px), MarginType::Cross)
            .unwrap();
        ctx.positions
            .apply_fill(short, m, false, fp(qty), fp(px), MarginType::Cross)
            .unwrap();
    }

    fn set_mark(ctx: &NativeExecContext, m: MarketId, price: FixedPoint) {
        let reporters = [addr(150), addr(151), addr(152)];
        for v in &reporters {
            ctx.oracle
                .submit_price(v, m, price, ctx.block_height, ctx.timestamp)
                .unwrap();
        }
        let stakes: Vec<(Address, FixedPoint)> = reporters.iter().map(|v| (*v, fp(1))).collect();
        assert_eq!(
            ctx.oracle
                .aggregate_price(m, ctx.block_height, ctx.timestamp, &stakes)
                .unwrap(),
            price
        );
    }

    fn figures(ctx: &NativeExecContext, t: &Address) -> AccountFigures {
        account_figures(&ctx.state, t, Some(ctx.timestamp)).unwrap()
    }

    fn gtc_buy(m: MarketId, price: i64, qty: i64) -> PlaceOrderParams {
        PlaceOrderParams {
            market_id: m,
            is_buy: true,
            price: fp(price),
            quantity: fp(qty),
            order_type: OrderType::Limit,
            time_in_force: TimeInForce::GTC,
            reduce_only: false,
            client_order_id: None,
        }
    }

    fn amount(v: FixedPoint) -> U256 {
        U256::from(v.raw() as u128)
    }

    /// t: cash 1000, long 10 @ 1000 in market 1, marked at 1010 (UPnL 100,
    /// IM 10100 / 20 = 505). s holds the short.
    fn long_at_1010() -> (tempfile::TempDir, NativeExecContext, Address) {
        let (d, db) = test_db(&[1, 2]);
        let ctx = ctx_of(&db);
        let (t, s) = (addr(1), addr(2));
        fund(&ctx, &t, fp(1_000));
        fund(&ctx, &s, fp(1_000_000));
        open_pair(&ctx, &t, &s, 1, 10, 1_000);
        set_mark(&ctx, 1, fp(1_010));
        (d, ctx, t)
    }

    /// freeMargin is the engine's placement pool: on the same state an order
    /// whose need equals it is accepted, the next lot (need just above) is
    /// rejected, and the rejection's "have" is freeMargin.
    #[test]
    fn free_margin_matches_the_placement_gate() {
        let (_d, mut ctx, t) = long_at_1010();
        let f = figures(&ctx, &t);
        assert_eq!(f.free_margin, fp(1_000 + 100 - 505));
        // Market 2: flat, no mark, default 20x: need = 100 q / 20 = 5 q.
        let need = |q: i64| {
            placement_need(None, FixedPoint::ZERO, fp(100), true, fp(q), fp(100), true).unwrap()
        };
        assert_eq!(
            need(119),
            f.free_margin,
            "boundary order needs exactly freeMargin"
        );
        assert!(need(120) > f.free_margin);

        let r = NativeExecutor::execute(
            &mut ctx,
            &t,
            &NativeAction::PlaceOrder(gtc_buy(2, 100, 120)),
        );
        assert!(!r.success);
        let e = r.error.unwrap();
        assert!(
            e.contains(&format!("have {} (account)", f.free_margin)),
            "{e}"
        );
        let r = NativeExecutor::execute(
            &mut ctx,
            &t,
            &NativeAction::PlaceOrder(gtc_buy(2, 100, 119)),
        );
        assert!(r.success, "{:?}", r.error);
        // Nothing left: the next order of one lot is rejected with have 0.
        let f = figures(&ctx, &t);
        assert_eq!(f.free_margin, FixedPoint::ZERO);
        let r =
            NativeExecutor::execute(&mut ctx, &t, &NativeAction::PlaceOrder(gtc_buy(2, 100, 1)));
        assert!(r
            .error
            .unwrap()
            .contains(&format!("have {} (account)", FixedPoint::ZERO)));
    }

    /// withdrawable is the largest amount the engine's withdraw check takes:
    /// the margin bound (available + UPnL − max(IM, 10% notional)) and the
    /// cash bound, whichever binds; 0 when cash is negative.
    #[test]
    fn withdrawable_matches_the_withdraw_check() {
        let one = FixedPoint::from_raw(1);
        // Margin binds: 1000 + 100 − max(505, 1010) = 90 < cash 1000.
        let (_d, mut ctx, t) = long_at_1010();
        let w = figures(&ctx, &t).withdrawable;
        assert_eq!(w, fp(90));
        let r = NativeExecutor::execute(
            &mut ctx,
            &t,
            &NativeAction::TransferToSpot {
                amount: amount(w + one),
            },
        );
        assert!(!r.success, "one unit above withdrawable");
        let r = NativeExecutor::execute(
            &mut ctx,
            &t,
            &NativeAction::TransferToSpot { amount: amount(w) },
        );
        assert!(r.success, "{:?}", r.error);
        assert_eq!(figures(&ctx, &t).withdrawable, FixedPoint::ZERO);

        // Cash binds: mark 1200 (UPnL 2000, required 1200), cash 100.
        let (_d2, db) = test_db(&[1]);
        let mut ctx = ctx_of(&db);
        let (t, s) = (addr(1), addr(2));
        fund(&ctx, &t, fp(100));
        fund(&ctx, &s, fp(1_000_000));
        open_pair(&ctx, &t, &s, 1, 10, 1_000);
        set_mark(&ctx, 1, fp(1_200));
        let w = figures(&ctx, &t).withdrawable;
        assert_eq!(w, fp(100));
        let r = NativeExecutor::execute(
            &mut ctx,
            &t,
            &NativeAction::TransferToSpot {
                amount: amount(w + one),
            },
        );
        assert!(!r.success);
        let r = NativeExecutor::execute(
            &mut ctx,
            &t,
            &NativeAction::TransferToSpot { amount: amount(w) },
        );
        assert!(r.success, "{:?}", r.error);

        // Negative cash: nothing.
        let (_d3, db) = test_db(&[1]);
        let mut ctx = ctx_of(&db);
        fund(&ctx, &t, fp(-5));
        let f = figures(&ctx, &t);
        assert_eq!((f.cash_balance, f.withdrawable), (fp(-5), FixedPoint::ZERO));
        let r = NativeExecutor::execute(
            &mut ctx,
            &t,
            &NativeAction::TransferToSpot {
                amount: amount(one),
            },
        );
        assert!(!r.success);
    }

    /// t: long 10 @ 1000 marked at 990 with `cash`; a maker bid at 985 for
    /// stage 1 to sell into.
    fn liq_case(cash: FixedPoint) -> (tempfile::TempDir, NativeExecContext, Address) {
        let (d, db) = test_db(&[1]);
        let mut ctx = ctx_of(&db);
        let (t, s, m) = (addr(1), addr(2), addr(3));
        fund(&ctx, &t, cash);
        fund(&ctx, &s, fp(1_000_000));
        fund(&ctx, &m, fp(1_000_000));
        open_pair(&ctx, &t, &s, 1, 10, 1_000);
        let r =
            NativeExecutor::execute(&mut ctx, &m, &NativeAction::PlaceOrder(gtc_buy(1, 985, 10)));
        assert!(r.success, "{:?}", r.error);
        set_mark(&ctx, 1, fp(990));
        (d, ctx, t)
    }

    fn long_size(ctx: &NativeExecContext, t: &Address) -> FixedPoint {
        ctx.positions
            .get_position(t, 1)
            .unwrap()
            .map_or(FixedPoint::ZERO, |p| p.size)
    }

    /// maintenanceMarginUsed is the liquidation check's MM: with
    /// accountValue == maintenanceMarginUsed the account is left alone, one
    /// raw unit of cash less and stage 1 closes the position.
    #[test]
    fn maintenance_matches_the_liquidation_check() {
        let (_d, ctx, t) = liq_case(fp(1_000));
        let f = figures(&ctx, &t);
        assert_eq!(
            f.maintenance_margin_used,
            FixedPoint::from_raw(fp(9_900).raw() / 40),
            "2.5% of 9900"
        );
        let at_mm = fp(1_000) - (f.account_value - f.maintenance_margin_used);

        let (_d, mut ctx, t) = liq_case(at_mm);
        let f = figures(&ctx, &t);
        assert_eq!(f.account_value, f.maintenance_margin_used);
        NativeExecutor::run_liquidations(&mut ctx);
        assert!(ctx.fatal_error.is_none());
        assert_eq!(long_size(&ctx, &t), fp(10), "AV == MM: healthy");

        let (_d, mut ctx, t) = liq_case(at_mm - FixedPoint::from_raw(1));
        let f = figures(&ctx, &t);
        assert!(f.account_value < f.maintenance_margin_used);
        NativeExecutor::run_liquidations(&mut ctx);
        assert!(ctx.fatal_error.is_none());
        assert_eq!(long_size(&ctx, &t), FixedPoint::ZERO, "AV < MM: liquidated");
    }

    /// The liquidation check marks only LISTED markets; a position in a
    /// market without a row is valued at entry for maintenance, but its
    /// usable mark still counts for the placement pool (the executor's
    /// `mark` reads any market's aggregate).
    #[test]
    fn maintenance_ignores_marks_of_unlisted_markets() {
        let (_d, db) = test_db(&[1]);
        let ctx = ctx_of(&db);
        let (t, s) = (addr(1), addr(2));
        fund(&ctx, &t, fp(1_000));
        fund(&ctx, &s, fp(1_000_000));
        open_pair(&ctx, &t, &s, 7, 10, 1_000); // market 7: no row
        set_mark(&ctx, 7, fp(1_200));
        let f = figures(&ctx, &t);
        assert_eq!(
            f.maintenance_margin_used,
            FixedPoint::from_raw(fp(10_000).raw() / 40),
            "at entry"
        );
        // Placement: UPnL 2000, IM 12000 / 20 = 600.
        assert_eq!(f.free_margin, fp(1_000 + 2_000 - 600));
        assert_eq!(f.account_value, fp(3_000));
    }

    /// Without a block time (no executed header) nothing is marked: every
    /// position at entry, UPnL 0. Totals: order margin + cross IM + isolated.
    #[test]
    fn no_block_time_values_positions_at_entry() {
        let (_d, db) = test_db(&[1]);
        let ctx = ctx_of(&db);
        let t = addr(1);
        ctx.positions
            .put_native_balance(
                &t,
                &NativeBalance {
                    available: fp(800),
                    order_margin: fp(50),
                },
            )
            .unwrap();
        open_pair(&ctx, &t, &addr(2), 1, 10, 1_000);
        set_mark(&ctx, 1, fp(1_200));
        let f = account_figures(&ctx.state, &t, None).unwrap();
        assert_eq!(f.native_balance, fp(850));
        assert_eq!(f.account_value, fp(850));
        assert_eq!(f.total_margin_used, fp(50 + 500));
        assert_eq!(f.free_margin, fp(800 - 500));
        assert_eq!(f.cash_balance, fp(800));
        // 800 + 0 − max(500, 1000) < 0.
        assert_eq!(f.withdrawable, FixedPoint::ZERO);
    }

    // ---- R23: cross liquidation price ----

    fn gtc_sell(m: MarketId, price: i64, qty: i64) -> PlaceOrderParams {
        PlaceOrderParams {
            is_buy: false,
            ..gtc_buy(m, price, qty)
        }
    }

    /// One cross leg of t: (market, long?, size, entry).
    type Leg = (MarketId, bool, i64, i64);

    /// t (addr 1) with `cash` and cross `legs` (s = addr 2 holds the other
    /// side); maker addr 3 rests `quotes` (market, buy?, price, size) for a
    /// stage-1 liquidation to fill against; then every market in `marks` is
    /// marked. Markets 1 and 2 are listed (default 20x: MM 2.5% notional).
    fn liq_scenario(
        cash: FixedPoint,
        legs: &[Leg],
        quotes: &[(MarketId, bool, i64, i64)],
        marks: &[(MarketId, FixedPoint)],
    ) -> (tempfile::TempDir, NativeExecContext, Address) {
        let (d, db) = test_db(&[1, 2]);
        let mut ctx = ctx_of(&db);
        let (t, s, mk) = (addr(1), addr(2), addr(3));
        fund(&ctx, &t, cash);
        fund(&ctx, &s, fp(1_000_000));
        fund(&ctx, &mk, fp(1_000_000));
        for &(m, long, qty, px) in legs {
            let (l, sh) = if long { (&t, &s) } else { (&s, &t) };
            open_pair(&ctx, l, sh, m, qty, px);
        }
        for &(m, buy, px, qty) in quotes {
            let p = if buy {
                gtc_buy(m, px, qty)
            } else {
                gtc_sell(m, px, qty)
            };
            let r = NativeExecutor::execute(&mut ctx, &mk, &NativeAction::PlaceOrder(p));
            assert!(r.success, "{:?}", r.error);
        }
        for &(m, px) in marks {
            set_mark(&ctx, m, px);
        }
        (d, ctx, t)
    }

    fn liq_price(ctx: &NativeExecContext, t: &Address, m: MarketId) -> Option<FixedPoint> {
        cross_liquidation_price(&ctx.state, t, m, Some(ctx.timestamp)).unwrap()
    }

    /// Whether the engine's liquidation step touches t (any position size
    /// changes) on the [`liq_scenario`] state.
    fn engine_liquidates(
        cash: FixedPoint,
        legs: &[Leg],
        quotes: &[(MarketId, bool, i64, i64)],
        marks: &[(MarketId, FixedPoint)],
    ) -> bool {
        let (_d, mut ctx, t) = liq_scenario(cash, legs, quotes, marks);
        let sizes = |ctx: &NativeExecContext| -> Vec<FixedPoint> {
            legs.iter()
                .map(|&(m, ..)| {
                    ctx.positions
                        .get_position(&t, m)
                        .unwrap()
                        .map_or(FixedPoint::ZERO, |p| p.size)
                })
                .collect()
        };
        let before = sizes(&ctx);
        NativeExecutor::run_liquidations(&mut ctx);
        assert!(ctx.fatal_error.is_none());
        sizes(&ctx) != before
    }

    /// One raw unit (1e-8).
    fn one_raw() -> FixedPoint {
        FixedPoint::from_raw(1)
    }

    /// Long 10 @ 1000, cash 1000: healthy iff 1000 + 10P − 10000 >= 2.5% ×
    /// 10P, so P = 9000 / 9.75 = 923.0769230.., rounded up to the raw unit
    /// at which equity first covers MM. The engine leaves the account alone
    /// at that mark and liquidates one raw unit below it.
    #[test]
    fn cross_long_liquidation_price_is_the_engine_threshold() {
        let legs = [(1, true, 10, 1_000)];
        let (_d, ctx, t) = liq_scenario(fp(1_000), &legs, &[], &[(1, fp(1_000))]);
        let p = liq_price(&ctx, &t, 1).expect("a long has a liquidation price");
        assert_eq!(p, FixedPoint::from_raw(92_307_692_308));
        // The current mark does not move it (only this market's mark is
        // the variable).
        let (_d, ctx, t) = liq_scenario(fp(1_000), &legs, &[], &[(1, fp(1_100))]);
        assert_eq!(liq_price(&ctx, &t, 1), Some(p));

        let bid = [(1, true, 920, 10)];
        assert!(!engine_liquidates(fp(1_000), &legs, &bid, &[(1, p)]));
        assert!(engine_liquidates(
            fp(1_000),
            &legs,
            &bid,
            &[(1, p - one_raw())]
        ));
    }

    /// Short 10 @ 1000, cash 1000: healthy iff 11000 − 10P >= 2.5% × 10P,
    /// P = 11000 / 10.25 = 1073.1707317.., the highest raw price still
    /// healthy. The engine liquidates one raw unit above it.
    #[test]
    fn cross_short_liquidation_price_is_the_engine_threshold() {
        let legs = [(1, false, 10, 1_000)];
        let (_d, ctx, t) = liq_scenario(fp(1_000), &legs, &[], &[(1, fp(1_000))]);
        let p = liq_price(&ctx, &t, 1).expect("a short has a liquidation price");
        assert_eq!(p, FixedPoint::from_raw(107_317_073_170));

        let ask = [(1, false, 1_080, 10)];
        assert!(!engine_liquidates(fp(1_000), &legs, &ask, &[(1, p)]));
        assert!(engine_liquidates(
            fp(1_000),
            &legs,
            &ask,
            &[(1, p + one_raw())]
        ));
    }

    /// Cross account with two positions: long 10 @ 1000 in market 1 and
    /// short 1 @ 2000 in market 2, cash 1000. Each position's price is the
    /// shock of its own market with the other market's mark held fixed:
    /// - market 1 (market 2 at 2100: UPnL −100, MM 52.5): 9.75 P >= 9152.5,
    ///   P = 938.7179487..;
    /// - market 2 (market 1 at 1000: UPnL 0, MM 250): 3000 − Q >= 250 +
    ///   2.5% Q, Q = 2682.9268292.. (highest healthy raw price).
    #[test]
    fn cross_two_positions_each_shocks_its_own_market() {
        let legs = [(1, true, 10, 1_000), (2, false, 1, 2_000)];
        let marks = [(1, fp(1_000)), (2, fp(2_100))];
        let (_d, ctx, t) = liq_scenario(fp(1_000), &legs, &[], &marks);
        let p1 = liq_price(&ctx, &t, 1).unwrap();
        assert_eq!(p1, FixedPoint::from_raw(93_871_794_872));
        let quotes = [(1, true, 930, 10), (2, false, 2_110, 1)];
        assert!(!engine_liquidates(
            fp(1_000),
            &legs,
            &quotes,
            &[(1, p1), (2, fp(2_100))]
        ));
        assert!(engine_liquidates(
            fp(1_000),
            &legs,
            &quotes,
            &[(1, p1 - one_raw()), (2, fp(2_100))]
        ));

        let p2 = liq_price(&ctx, &t, 2).unwrap();
        // Market 1 is held at its current mark 1000, whatever market 2's
        // current mark is.
        assert_eq!(p2, FixedPoint::from_raw(268_292_682_927));
        let quotes = [(1, true, 990, 10), (2, false, 2_690, 1)];
        assert!(!engine_liquidates(
            fp(1_000),
            &legs,
            &quotes,
            &[(1, fp(1_000)), (2, p2)]
        ));
        assert!(engine_liquidates(
            fp(1_000),
            &legs,
            &quotes,
            &[(1, fp(1_000)), (2, p2 + one_raw())]
        ));
    }

    /// No mark of the market crosses maintenance ⇒ `None`:
    /// - a fully funded long is healthy even at mark 0 (cash 10000 covers
    ///   the whole cost basis);
    /// - an account liquidatable at every mark (here a short whose equity
    ///   is negative even at mark 0);
    /// - no position in the market;
    /// - a position in an unlisted market (the liquidation step never marks
    ///   it, so its price cannot trigger liquidation);
    /// - the account also holds an isolated position (the liquidation step
    ///   does not value such an account).
    #[test]
    fn cross_liquidation_price_none_when_no_mark_crosses() {
        let (_d, ctx, t) =
            liq_scenario(fp(10_000), &[(1, true, 10, 1_000)], &[], &[(1, fp(1_000))]);
        assert_eq!(liq_price(&ctx, &t, 1), None, "fully funded long");
        assert!(!engine_liquidates(
            fp(10_000),
            &[(1, true, 10, 1_000)],
            &[(1, true, 1, 1)],
            &[(1, one_raw())]
        ));

        let (_d, ctx, t) = liq_scenario(
            fp(-10_001),
            &[(1, false, 10, 1_000)],
            &[],
            &[(1, fp(1_000))],
        );
        assert_eq!(liq_price(&ctx, &t, 1), None, "liquidatable at any mark");

        let (_d, ctx, t) = liq_scenario(fp(1_000), &[(1, true, 10, 1_000)], &[], &[]);
        assert_eq!(liq_price(&ctx, &t, 2), None, "no position");

        let (_d, ctx, t) = liq_scenario(fp(1_000), &[(7, true, 10, 1_000)], &[], &[(7, fp(1_000))]);
        assert_eq!(liq_price(&ctx, &t, 7), None, "unlisted market");

        let (_d, ctx, t) = liq_scenario(fp(1_000), &[(1, true, 10, 1_000)], &[], &[(1, fp(1_000))]);
        ctx.positions
            .apply_fill(&t, 2, true, fp(1), fp(100), MarginType::Isolated)
            .unwrap();
        assert_eq!(
            liq_price(&ctx, &t, 1),
            None,
            "account with an isolated position"
        );
    }

    /// t (addr 1) with raw `cash` and one cross position in listed market 1
    /// (size `size`, raw entry `entry`); no mark (the shocked market's price
    /// is the variable).
    fn raw_account(
        cash: i128,
        long: bool,
        size: FixedPoint,
        entry: i128,
    ) -> (tempfile::TempDir, NativeExecContext, Address) {
        let (d, db) = test_db(&[1]);
        let ctx = ctx_of(&db);
        let t = addr(1);
        fund(&ctx, &t, FixedPoint::from_raw(cash));
        ctx.positions
            .apply_fill(
                &t,
                1,
                long,
                size,
                FixedPoint::from_raw(entry),
                MarginType::Cross,
            )
            .unwrap();
        (d, ctx, t)
    }

    /// The liquidation step's verdict on t with market 1 marked at `px`
    /// (default tiers): `None` when the valuation overflows (the step skips
    /// the account).
    fn health_at(ctx: &NativeExecContext, t: &Address, px: FixedPoint) -> Option<Health> {
        let bal = ctx.positions.get_native_balance(t).unwrap();
        let positions = ctx.positions.positions_for_trader(t).unwrap();
        AccountView::build(&bal, &positions, |_| Some(px), |_| None)
            .ok()
            .and_then(|v| classify(&v))
    }

    /// Codex R1 P2 (account.rs:217): an overflowing valuation is
    /// indeterminate, not healthy.
    /// - short 3 @ MAX/4, cash MAX/10: healthy at 0, valued up to mark
    ///   MAX/3 (notional 3P), crossing at 0.2764 MAX; the first doubling
    ///   (MAX/2) overflows. The price is the highest healthy raw mark;
    /// - long 2 @ 0.45 MAX, cash −MAX/10: liquidatable at every mark the
    ///   step can value (up to MAX/2), overflow above. No valid valuation
    ///   is healthy, so there is no price (not the overflow edge).
    #[test]
    fn cross_liquidation_price_overflow_is_indeterminate_not_healthy() {
        let max = i128::MAX;
        let (_d, ctx, t) = raw_account(max / 10, false, fp(3), max / 4);
        let p = liq_price(&ctx, &t, 1).expect("crossing below the overflow");
        assert_eq!(
            p,
            FixedPoint::from_raw(47_030_896_241_105_316_088_433_888_832_033_004_834)
        );
        assert_eq!(health_at(&ctx, &t, p), Some(Health::Healthy));
        assert!(health_at(&ctx, &t, p + one_raw()).is_some_and(|h| h != Health::Healthy));

        let (_d, ctx, t) = raw_account(-(max / 10), true, fp(2), max / 20 * 9);
        let last = FixedPoint::from_raw(max / 2);
        assert!(health_at(&ctx, &t, last).is_some_and(|h| h != Health::Healthy));
        assert_eq!(health_at(&ctx, &t, last + one_raw()), None, "overflow");
        assert_eq!(liq_price(&ctx, &t, 1), None, "no healthy valid mark");
    }

    /// Codex R1 P2 (account.rs:234): when doubling overflows i128 the search
    /// still checks the rest of the range up to MAX. Short 1 @ raw 1, cash
    /// 3 MAX/4: healthy at 0, Adl at MAX, crossing at 0.7317 MAX (above the
    /// last power of two, 2^126).
    #[test]
    fn cross_liquidation_price_searches_up_to_the_max_mark() {
        let max = i128::MAX;
        let (_d, ctx, t) = raw_account(max / 4 * 3, false, fp(1), 1);
        assert_eq!(
            health_at(&ctx, &t, FixedPoint::from_raw(max)),
            Some(Health::Adl)
        );
        let p = liq_price(&ctx, &t, 1).expect("crossing between 2^126 and MAX");
        assert_eq!(
            p,
            FixedPoint::from_raw(124_493_548_873_514_071_998_795_588_084_793_248_092)
        );
        assert_eq!(health_at(&ctx, &t, p), Some(Health::Healthy));
        assert!(health_at(&ctx, &t, p + one_raw()).is_some_and(|h| h != Health::Healthy));
    }

    /// Without a block time the other positions are valued at entry (as
    /// `account_figures`); the shocked market's price is the variable, so
    /// a single long has the same price as with a mark.
    #[test]
    fn cross_liquidation_price_without_block_time() {
        let (_d, ctx, t) = liq_scenario(fp(1_000), &[(1, true, 10, 1_000)], &[], &[(1, fp(1_000))]);
        assert_eq!(
            cross_liquidation_price(&ctx.state, &t, 1, None).unwrap(),
            Some(FixedPoint::from_raw(92_307_692_308))
        );
    }
}
