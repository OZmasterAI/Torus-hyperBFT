//! s94 option 2 (HL "Order price too far from oracle", `oracleRejected`):
//! the price band at placement. A Limit price (any TIF), a StopLimit's limit
//! and trigger and a StopMarket's trigger must lie within ± `price_band_bps`
//! (governance key in `CF_FEE_CONFIG`, default 5,000 = ±50%) of the
//! market's reference: its mark, or — mark stale / missing — the median of
//! the book's best bid, best ask and last trade (mid of what exists; the
//! last fresh mark when the book has none) clamped to ±10% of the last
//! fresh mark; no reference at all (no aggregate ever): no band. A Market
//! order's cap is out of scope (P2 slippage cap). The executor rejects
//! before the book like fix A: no open-order slot, no margin, no order id,
//! typed reason `price_band`, every path.

use std::sync::Arc;

use alloy_primitives::{Address, B256};

use torus_bridge::native_executor::{NativeExecContext, NativeExecutor};
use torus_bridge::state_root::compute_native_state_root;
use torus_core::position::NativeBalance;
use torus_state::action_status::FailureReason;
use torus_state::cf::{
    CF_BOOK_ORDER_ROWS, CF_FEE_CONFIG, CF_NATIVE_BALANCES, CF_NATIVE_MARKETS, CF_NATIVE_ORDER_BOOKS,
    CF_NATIVE_POSITIONS, CF_NATIVE_TRADES, CF_NATIVE_USER_TRADES,
};
use torus_state::{StateBackend, StateDb};
use torus_telemetry::Metrics;
use torus_types::{FixedPoint, MarketId, NativeAction, OrderType, PlaceOrderParams, TimeInForce};

fn addr(n: u8) -> Address {
    Address::new([n; 20])
}

fn fp(v: i64) -> FixedPoint {
    FixedPoint::from_raw(v as i128 * FixedPoint::SCALE)
}

fn raw(v: i128) -> FixedPoint {
    FixedPoint::from_raw(v)
}

const M: MarketId = 1;
const S: i128 = FixedPoint::SCALE;

fn fund(ctx: &NativeExecContext, t: &Address, amount: FixedPoint) {
    ctx.positions
        .put_native_balance(t, &NativeBalance { available: amount, order_margin: FixedPoint::ZERO })
        .unwrap();
}

fn order(is_buy: bool, price: FixedPoint, order_type: OrderType, tif: TimeInForce) -> PlaceOrderParams {
    PlaceOrderParams {
        market_id: M,
        is_buy,
        price,
        quantity: fp(1),
        order_type,
        time_in_force: tif,
        reduce_only: false,
        client_order_id: None,
    }
}

fn gtc(is_buy: bool, price: FixedPoint) -> PlaceOrderParams {
    order(is_buy, price, OrderType::Limit, TimeInForce::GTC)
}

fn place(t: Address, p: PlaceOrderParams) -> (Address, NativeAction) {
    (t, NativeAction::PlaceOrder(p))
}

/// The aggregated mark of `m` at the context's block (3 equal reporters).
fn set_mark(ctx: &NativeExecContext, m: MarketId, price: FixedPoint) {
    let reporters = [addr(150), addr(151), addr(152)];
    for v in &reporters {
        ctx.oracle.submit_price(v, m, price, ctx.block_height, ctx.timestamp).unwrap();
    }
    let stakes: Vec<(Address, FixedPoint)> = reporters.iter().map(|v| (*v, fp(1))).collect();
    assert_eq!(ctx.oracle.aggregate_price(m, ctx.block_height, ctx.timestamp, &stakes).unwrap(), price);
}

/// Market 1 listed (test row: default 20x, tick / lot 1), traders 1..=4
/// rich; `mark` aggregated at block 1, time 1,001.
fn setup(mark: Option<i64>) -> (tempfile::TempDir, NativeExecContext) {
    let dir = tempfile::tempdir().unwrap();
    let db = StateDb::open(dir.path()).unwrap();
    db.put_cf_raw(CF_NATIVE_MARKETS, &M.to_be_bytes(), b"listed").unwrap();
    let ctx = NativeExecContext::new(db, 1, 1_001, 0, 1_000, 10, addr(99), addr(100), addr(101));
    for n in 1..=4u8 {
        fund(&ctx, &addr(n), fp(1_000_000));
    }
    if let Some(m) = mark {
        set_mark(&ctx, M, fp(m));
    }
    (dir, ctx)
}

/// `None` = the single-action path, `Some(t)` = `execute_batch_engine_mode(_, _, t)`.
type Mode = Option<usize>;
const MODES: [Mode; 4] = [None, Some(0), Some(2), Some(4)];

type Outcome = (bool, Option<String>, FailureReason);

fn exec(ctx: &mut NativeExecContext, mode: Mode, block: &[(Address, NativeAction)]) -> Vec<Outcome> {
    let r: Vec<_> = match mode {
        None => block.iter().map(|(s, a)| NativeExecutor::execute(ctx, s, a)).collect(),
        Some(t) => NativeExecutor::execute_batch_engine_mode(ctx, block, t).results,
    };
    assert!(ctx.fatal_error.is_none(), "{:?}", ctx.fatal_error);
    r.into_iter().map(|a| (a.success, a.error, a.reason)).collect()
}

fn is_band_reject(r: &Outcome, price: &str, pct: &str, reference: &str) -> bool {
    let want = format!("order rejected: price {price} is more than {pct}% from the reference price {reference}");
    !r.0 && r.2 == FailureReason::PriceBand && r.1.as_deref() == Some(want.as_str())
}

#[derive(PartialEq, Eq, Debug)]
struct World {
    dump: Vec<(String, Vec<u8>, Vec<u8>)>,
    next_global_order_id: u128,
    trade_index: u32,
    root: B256,
}

fn world(ctx: &mut NativeExecContext) -> World {
    ctx.save_order_books();
    let cfs = [
        CF_NATIVE_BALANCES,
        CF_NATIVE_POSITIONS,
        CF_NATIVE_ORDER_BOOKS,
        CF_NATIVE_MARKETS,
        CF_NATIVE_TRADES,
        CF_NATIVE_USER_TRADES,
        CF_BOOK_ORDER_ROWS,
    ];
    let mut dump = Vec::new();
    for cf in cfs {
        for (k, v) in ctx.state.iterate_cf(cf, None).unwrap() {
            dump.push((cf.to_string(), k, v));
        }
    }
    World {
        dump,
        next_global_order_id: ctx.next_global_order_id,
        trade_index: ctx.trade_index,
        root: compute_native_state_root(&ctx.state).unwrap(),
    }
}

/// Mark 100, band ±50%: [50, 150] (tick 1; the raw-unit boundary is pinned
/// in torus-core's price_band_tests). Out of the band (rejected, typed
/// `price_band`): GTC buy @151, GTC sell @49, IOC buy
/// @151, FOK sell @49, PostOnly buy @151, a StopLimit whose LIMIT is out
/// (151, trigger 120), one whose TRIGGER is out (limit 120, trigger 151), a
/// StopMarket with its trigger out (49). Accepted: the exact boundaries
/// @150 / @50 and a Market order whose cap (1,000) is out (out of scope;
/// row 50: its only ask is its sender's own, so the book rejects it,
/// `marketOrderNoLiquidityRejected` — not a band reject).
/// The rejects take no slot / margin / order id: the world equals the block
/// without them, on every path; batch modes agree byte for byte.
#[test]
fn orders_outside_the_band_are_rejected_before_the_book_on_every_path() {
    let (a, b) = (addr(1), addr(2));
    let stop_limit = |trigger: i64, limit: i64| PlaceOrderParams {
        order_type: OrderType::StopLimit { trigger: fp(trigger), limit: fp(limit) },
        ..gtc(true, fp(limit))
    };
    let stop_market = PlaceOrderParams {
        order_type: OrderType::StopMarket { trigger: fp(49) },
        ..gtc(false, fp(40))
    };
    let block = vec![
        place(a, gtc(true, fp(151))),                                             // 0 out
        place(a, gtc(false, fp(49))),                                             // 1 out
        place(a, order(true, fp(151), OrderType::Limit, TimeInForce::IOC)),       // 2 out
        place(a, order(false, fp(49), OrderType::Limit, TimeInForce::FOK)),       // 3 out
        place(a, order(true, fp(151), OrderType::Limit, TimeInForce::PostOnly)),  // 4 out
        place(a, stop_limit(120, 151)),                                           // 5 out (limit)
        place(a, stop_limit(151, 120)),                                           // 6 out (trigger)
        place(a, stop_market),                                                    // 7 out (trigger)
        place(a, gtc(true, fp(50))),                                              // 8 boundary: rests
        place(b, gtc(false, fp(150))),                                            // 9 boundary: rests
        place(b, order(true, fp(1_000), OrderType::Market, TimeInForce::IOC)),    // 10 cap out: no fill (own ask)
    ];
    let want = [
        ("151.00000000", "50.00"),
        ("49.00000000", "50.00"),
        ("151.00000000", "50.00"),
        ("49.00000000", "50.00"),
        ("151.00000000", "50.00"),
        ("151.00000000", "50.00"),
        ("151.00000000", "50.00"),
        ("49.00000000", "50.00"),
    ];
    let valid: Vec<_> = block[8..].to_vec();
    let mut batch_worlds = Vec::new();
    for mode in MODES {
        let (_d, mut ctx) = setup(Some(100));
        let metrics = Arc::new(Metrics::new());
        ctx.metrics = Some(metrics.clone());
        let r = exec(&mut ctx, mode, &block);
        for (i, (price, pct)) in want.iter().enumerate() {
            assert!(is_band_reject(&r[i], price, pct, "100.00000000"), "{mode:?} #{i}: {:?}", r[i]);
        }
        for i in 8..10 {
            assert!(r[i].0, "{mode:?} #{i}: {:?}", r[i]);
        }
        assert!(!r[10].0 && r[10].2 == FailureReason::MarketNoLiquidity, "{mode:?}: {:?}", r[10]);
        assert_eq!(metrics.orders_rejected_other.get(), 8, "{mode:?}: pre-book rejects");
        let got = world(&mut ctx);
        let (_d2, mut reference) = setup(Some(100));
        assert!(exec(&mut reference, mode, &valid).iter().take(2).all(|r| r.0), "{mode:?}");
        assert_eq!(got, world(&mut reference), "{mode:?}: the rejected orders changed state");
        if mode.is_some() {
            batch_worlds.push((r, got));
        }
    }
    assert!(batch_worlds.windows(2).all(|w| w[0] == w[1]), "engine modes differ");
}

/// The probe's attack (buy 10 @2x / sell 10 @0.5x of the mark 100, A and B
/// one owner) never reaches the book: rejected at placement.
#[test]
fn the_probe_attack_at_2x_and_half_the_mark_is_rejected_at_placement() {
    for mode in MODES {
        let (_d, mut ctx) = setup(Some(100));
        let r = exec(&mut ctx, mode, &[place(addr(1), gtc(true, fp(200))), place(addr(2), gtc(false, fp(200)))]);
        for x in &r {
            assert!(is_band_reject(x, "200.00000000", "50.00", "100.00000000"), "{mode:?}: {x:?}");
        }
        let r = exec(&mut ctx, mode, &[place(addr(1), gtc(false, fp(49)))]);
        assert!(is_band_reject(&r[0], "49.00000000", "50.00", "100.00000000"), "{mode:?}: {:?}", r[0]);
        assert!(ctx.order_books.get(&M).is_none_or(|b| b.best_bid().is_none() && b.best_ask().is_none()));
    }
}

/// Governance narrows the band: `price_band_bps` = "1000" (±10%) rejects
/// @111 and accepts @110; an invalid stored value ("0", "abc", 9,001) falls
/// back to the default ±50%.
#[test]
fn the_band_is_the_governance_parameter() {
    for (stored, rejects_111) in [("1000", true), ("0", false), ("abc", false), ("9001", false), ("9000", false)] {
        for mode in MODES {
            let (_d, mut ctx) = setup(Some(100));
            ctx.state.put_cf_raw(CF_FEE_CONFIG, b"price_band_bps", stored.as_bytes()).unwrap();
            let r = exec(&mut ctx, mode, &[place(addr(1), gtc(true, fp(111))), place(addr(1), gtc(true, fp(110)))]);
            if rejects_111 {
                assert!(is_band_reject(&r[0], "111.00000000", "10.00", "100.00000000"), "{stored} {mode:?}: {:?}", r[0]);
            } else {
                assert!(r[0].0, "{stored} {mode:?}: {:?}", r[0]);
            }
            assert!(r[1].0, "{stored} {mode:?}: {:?}", r[1]);
        }
    }
}

/// No reference at all (no oracle aggregate ever, a new market): the band
/// is skipped — a buy @10x rests.
#[test]
fn no_reference_skips_the_band() {
    for mode in MODES {
        let (_d, mut ctx) = setup(None);
        let r = exec(&mut ctx, mode, &[place(addr(1), gtc(true, fp(1_000)))]);
        assert!(r[0].0, "{mode:?}: {:?}", r[0]);
    }
}

/// Stale mark (120 s after the last aggregate of 100): the reference is the
/// median of the book's best bid, best ask and last trade, clamped to ±10%
/// of 100. Book: bid 95, ask 107, last trade 104 (a cross at 104 first) →
/// 104 → band [52, 156]: @157 rejected, @156 accepted. Same book with only
/// bid 95 / ask 107 and no trade → mid 101. A book above the last mark
/// (bid 115, ask 125, trade 120) → 120 clamped to 110 → [55, 165].
/// An empty book → the last mark 100.
#[test]
fn a_stale_mark_falls_back_to_the_book_clamped_to_the_last_mark() {
    // (book: bid, ask, trade price or None; reference; first price out)
    let cases: [((i64, i64, Option<i64>), &str, i128); 4] = [
        ((95, 107, Some(104)), "104.00000000", 157 * S),
        ((95, 107, None), "101.00000000", 152 * S),
        ((115, 125, Some(120)), "110.00000000", 166 * S),
        ((0, 0, None), "100.00000000", 151 * S),
    ];
    for ((bid, ask, trade), reference, out) in cases {
        for mode in MODES {
            let what = format!("bid {bid} ask {ask} trade {trade:?} {mode:?}");
            let (_d, mut ctx) = setup(Some(100));
            if bid > 0 {
                // Built while the mark is fresh (inside its band).
                let mut setup_orders = vec![place(addr(3), gtc(true, fp(bid))), place(addr(4), gtc(false, fp(ask)))];
                if let Some(t) = trade {
                    setup_orders.insert(0, place(addr(3), gtc(true, fp(t))));
                    setup_orders.insert(1, place(addr(4), gtc(false, fp(t))));
                }
                let r = exec(&mut ctx, mode, &setup_orders);
                assert!(r.iter().all(|x| x.0), "{what}: {r:?}");
            }
            ctx.timestamp += 120;
            let below = raw(out - S);
            let r = exec(&mut ctx, mode, &[place(addr(1), gtc(true, raw(out))), place(addr(1), gtc(true, below))]);
            let price = format!("{}", raw(out));
            assert!(is_band_reject(&r[0], &price, "50.00", reference), "{what}: {:?}", r[0]);
            assert!(r[1].0, "{what}: {:?}", r[1]);
        }
    }
}

/// A modify moving a resting order out of the band is rejected (typed
/// `price_band`); inside the band it moves.
#[test]
fn a_modify_out_of_the_band_is_rejected() {
    let (_d, mut ctx) = setup(Some(100));
    let r = exec(&mut ctx, None, &[place(addr(1), gtc(true, fp(90)))]);
    assert!(r[0].0);
    let id = ctx.order_books[&M].orders_for_trader(&addr(1))[0].id;
    let modify = |p: i64| NativeAction::ModifyOrder { order_id: id, new_price: Some(fp(p)), new_qty: None };
    let r = NativeExecutor::execute(&mut ctx, &addr(1), &modify(40));
    assert!(!r.success && r.reason == FailureReason::PriceBand, "{r:?}");
    assert_eq!(
        r.error.as_deref(),
        Some("modify rejected: price 40.00000000 is more than 50.00% from the reference price 100.00000000")
    );
    assert!(NativeExecutor::execute(&mut ctx, &addr(1), &modify(60)).success);
}
