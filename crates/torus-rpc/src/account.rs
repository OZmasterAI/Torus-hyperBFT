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
//!   positions marked only in LISTED markets.
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
//! market held (market row, oracle aggregate) are charged to the request's
//! scan budget ([`account_figures_bounded`]).

use std::collections::BTreeMap;

use alloy_primitives::Address;
use torus_core::error::CoreError;
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

/// [`AccountFigures`] of `trader` at block time `now` (`None`: no usable
/// mark, every position valued at entry), without a row limit.
pub fn account_figures(
    state: &StateDb,
    trader: &Address,
    now: Option<u64>,
) -> Result<AccountFigures, RpcError> {
    account_figures_bounded(state, trader, now, &mut ScanBudget::unlimited())
}

/// [`account_figures`] charged to `b` (R24): each position row before it is
/// decoded, and per market held its market row and (with a block time) its
/// oracle aggregate. A read or decode failure is the handler's error
/// (`Internal` with the `CoreError` text).
pub(crate) fn account_figures_bounded(
    state: &StateDb,
    trader: &Address,
    now: Option<u64>,
    b: &mut ScanBudget,
) -> Result<AccountFigures, RpcError> {
    let internal = |e: CoreError| RpcError::Internal(e.to_string());
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
    figures(&bal, &positions, &markets).map_err(internal)
}

/// Per market the trader holds: its tiers (`load_margin_configs`: a row
/// under an 8-byte key, `None` = default 20x), whether it is listed (the
/// liquidation step marks only listed markets) and its usable mark.
struct Market {
    tiers: Option<Vec<MarginTier>>,
    listed: bool,
    mark: Option<FixedPoint>,
}

/// The figures from the rows read.
fn figures(
    bal: &NativeBalance,
    positions: &[Position],
    markets: &BTreeMap<MarketId, Market>,
) -> Result<AccountFigures, CoreError> {
    let of = |_| CoreError::Overflow("account margin overflows i128".into());
    let tiers = |m: MarketId| markets.get(&m).and_then(|x| x.tiers.as_deref());
    let view = AccountView::build(
        bal,
        positions,
        |m| markets.get(&m).and_then(|x| x.mark),
        tiers,
    )?;
    let liq_view = AccountView::build(
        bal,
        positions,
        |m| markets.get(&m).filter(|x| x.listed).and_then(|x| x.mark),
        tiers,
    )?;

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

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::U256;
    use torus_bridge::native_executor::{NativeExecContext, NativeExecutor};
    use torus_core::margin::placement_need;
    use torus_core::position::NativeBalance;
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
}
