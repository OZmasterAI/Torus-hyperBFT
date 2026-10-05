//! Fix A (s92): an off-tick `Limit` order and a dust order (quantity below
//! the lot) are rejected BEFORE the book, with the book's exact rules
//! (`OrderBook::place_order_with_accounts`) and the book's tick / lot (a
//! market without a book: 1 / 1, the book Phase 3 would create). Such an
//! order fails (`success == false`), counts as `orders_rejected_other` (not
//! `orders_rejected_book`), and has NO effect: no open-order slot, no
//! margin reservation, no order id, no D6 projection, no D2 pool claim, no
//! book created. Every engine mode (serial / sharded Phase 2) and the
//! single-action path agree.

use std::sync::Arc;

use alloy_primitives::{Address, B256};

use torus_bridge::native_executor::{NativeExecContext, NativeExecutor};
use torus_bridge::state_root::compute_native_state_root;
use torus_core::position::NativeBalance;
use torus_state::cf::{
    CF_BOOK_ORDER_ROWS, CF_NATIVE_BALANCES, CF_NATIVE_MARKETS, CF_NATIVE_ORDER_BOOKS,
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

fn half() -> FixedPoint {
    FixedPoint::from_raw(FixedPoint::SCALE / 2)
}

fn fund_native(ctx: &NativeExecContext, trader: &Address, amount: FixedPoint) {
    let bal = NativeBalance { available: amount, order_margin: FixedPoint::ZERO };
    ctx.positions.put_native_balance(trader, &bal).unwrap();
}

fn order(market_id: MarketId, is_buy: bool, price: FixedPoint, qty: FixedPoint, tif: TimeInForce) -> PlaceOrderParams {
    PlaceOrderParams {
        market_id,
        is_buy,
        price,
        quantity: qty,
        order_type: OrderType::Limit,
        time_in_force: tif,
        reduce_only: false,
        client_order_id: None,
    }
}

fn gtc(market_id: MarketId, is_buy: bool, price: i64, qty: i64) -> PlaceOrderParams {
    order(market_id, is_buy, fp(price), fp(qty), TimeInForce::GTC)
}

fn market(market_id: MarketId, is_buy: bool, cap: FixedPoint, qty: FixedPoint) -> PlaceOrderParams {
    PlaceOrderParams { order_type: OrderType::Market, ..order(market_id, is_buy, cap, qty, TimeInForce::IOC) }
}

fn place(sender: Address, p: PlaceOrderParams) -> (Address, NativeAction) {
    (sender, NativeAction::PlaceOrder(p))
}

fn state_dump(ctx: &NativeExecContext) -> Vec<(String, Vec<u8>, Vec<u8>)> {
    let cfs = [
        CF_NATIVE_BALANCES,
        CF_NATIVE_POSITIONS,
        CF_NATIVE_ORDER_BOOKS,
        CF_NATIVE_MARKETS,
        CF_NATIVE_TRADES,
        CF_NATIVE_USER_TRADES,
        CF_BOOK_ORDER_ROWS,
    ];
    let mut out = Vec::new();
    for cf in cfs {
        for (k, v) in ctx.state.iterate_cf(cf, None).expect("iterate cf") {
            out.push((cf.to_string(), k, v));
        }
    }
    out
}

fn is_tick_reject(r: &(bool, Option<String>)) -> bool {
    !r.0 && r.1.as_deref().is_some_and(|e| e.contains("is not a multiple of the tick"))
}

fn is_lot_reject(r: &(bool, Option<String>)) -> bool {
    !r.0 && r.1.as_deref().is_some_and(|e| e.contains("below the lot size"))
}

/// How a block is executed: `None` = the single-action path (`execute` per
/// action), `Some(t)` = `execute_batch_engine_mode(_, _, t)`.
type Mode = Option<usize>;

/// Everything observable about a run (minus per-action results).
#[derive(PartialEq, Eq, Debug)]
struct World {
    dump: Vec<(String, Vec<u8>, Vec<u8>)>,
    next_global_order_id: u128,
    trade_index: u32,
    root: B256,
}

struct Run {
    results: Vec<(bool, Option<String>)>,
    world: World,
    rejected_other: u64,
    rejected_book: u64,
}

fn exec(ctx: &mut NativeExecContext, mode: Mode, block: &[(Address, NativeAction)]) -> Vec<(bool, Option<String>)> {
    let r: Vec<_> = match mode {
        None => block.iter().map(|(s, a)| NativeExecutor::execute(ctx, s, a)).collect(),
        Some(t) => NativeExecutor::execute_batch_engine_mode(ctx, block, t).results,
    };
    assert!(ctx.fatal_error.is_none(), "{:?}", ctx.fatal_error);
    r.into_iter().map(|a| (a.success, a.error)).collect()
}

/// Book m1 (tick 2, lot 2) with maker addr(2)'s ask 5 @60; then `block`
/// from a (addr 1) and c (addr 3).
fn run(mode: Mode, block: &[(Address, NativeAction)]) -> Run {
    let dir = tempfile::tempdir().unwrap();
    let db = StateDb::open(dir.path()).unwrap();
    let mut ctx = NativeExecContext::new(db, 1, 1000, 0, 100, 10, addr(99), addr(100), addr(101));
    for n in 1..=3u8 {
        fund_native(&ctx, &addr(n), fp(1_000_000));
    }
    exec(&mut ctx, mode, &[place(addr(2), gtc(1, false, 60, 5))]);
    let book = ctx.order_books.get_mut(&1).expect("m1 book");
    book.tick_size = fp(2);
    book.lot_size = fp(2);
    let metrics = Arc::new(Metrics::new());
    ctx.metrics = Some(metrics.clone());
    let results = exec(&mut ctx, mode, block);
    ctx.save_order_books();
    Run {
        results,
        world: World {
            dump: state_dump(&ctx),
            next_global_order_id: ctx.next_global_order_id,
            trade_index: ctx.trade_index,
            root: compute_native_state_root(&ctx.state).expect("root"),
        },
        rejected_other: metrics.orders_rejected_other.get(),
        rejected_book: metrics.orders_rejected_book.get(),
    }
}

/// Off-tick limits and dust orders of every type fail before the book with
/// the book's tick / lot (and 1 / 1 where no book exists), while a market
/// order's off-tick cap is NOT tick-checked (the book checks only `Limit`).
/// The rejected orders leave the world byte-identical to the same block
/// without them, on every path; batch modes agree byte for byte.
#[test]
fn off_tick_and_dust_rejected_before_the_book_on_every_path() {
    let (a, c) = (addr(1), addr(3));
    let stop_dust = PlaceOrderParams {
        order_type: OrderType::StopMarket { trigger: fp(70) },
        ..gtc(1, true, 80, 1)
    };
    let block = vec![
        place(a, gtc(1, true, 51, 2)),                                    // 0: off the tick 2
        place(a, gtc(1, true, 50, 1)),                                    // 1: below the lot 2
        place(a, market(1, true, fp(60), fp(1))),                         // 2: dust market order
        place(a, market(1, true, fp(61), fp(2))),                         // 3: off-tick CAP: fills 2 @60
        place(a, order(7, true, fp(40) + half(), fp(1), TimeInForce::GTC)), // 4: no book: tick 1
        place(a, order(8, true, fp(40), half(), TimeInForce::GTC)),        // 5: no book: lot 1
        place(a, stop_dust),                                              // 6: dust stop
        place(a, order(1, true, fp(50), fp(2), TimeInForce::PostOnly)),    // 7: rests
        place(c, gtc(1, true, 48, 2)),                                    // 8: rests
    ];
    let valid: Vec<_> = [3usize, 7, 8].iter().map(|&i| block[i].clone()).collect();
    let mut batch_worlds = Vec::new();
    for mode in [None, Some(0), Some(2), Some(4)] {
        let got = run(mode, &block);
        let r = &got.results;
        for i in [0usize, 4] {
            assert!(is_tick_reject(&r[i]), "{mode:?} #{i}: {:?}", r[i]);
        }
        for i in [1usize, 2, 5, 6] {
            assert!(is_lot_reject(&r[i]), "{mode:?} #{i}: {:?}", r[i]);
        }
        for i in [3usize, 7, 8] {
            assert!(r[i].0, "{mode:?} #{i}: {:?}", r[i]);
        }
        assert_eq!(got.rejected_other, 6, "{mode:?}: pre-book rejects");
        assert_eq!(got.rejected_book, 0, "{mode:?}: nothing reached the book to be rejected");

        let reference = run(mode, &valid);
        assert!(reference.results.iter().all(|r| r.0), "{mode:?}: {:?}", reference.results);
        assert_eq!(got.world, reference.world, "{mode:?}: the rejected orders changed state");
        if mode.is_some() {
            batch_worlds.push((got.results, got.world));
        }
    }
    assert!(batch_worlds.windows(2).all(|w| w[0] == w[1]), "engine modes differ");
}

/// A dust order that is also off-tick reports the book's FIRST reject
/// (dust), and a reject before the book allocates no order id: the next
/// accepted order gets the id the rejected one would have taken.
#[test]
fn rejects_take_no_order_id_and_dust_is_checked_first() {
    let a = addr(1);
    let both = order(1, true, fp(51), fp(1), TimeInForce::GTC);
    for mode in [None, Some(0), Some(2), Some(4)] {
        let block = vec![place(a, both.clone()), place(a, gtc(1, true, 50, 2)), place(addr(3), gtc(1, true, 48, 2))];
        let got = run(mode, &block);
        assert!(is_lot_reject(&got.results[0]), "{mode:?}: {:?}", got.results[0]);
        let reference = run(mode, &block[1..]);
        assert_eq!(got.world.next_global_order_id, reference.world.next_global_order_id, "{mode:?}");
        assert_eq!(got.world, reference.world, "{mode:?}");
    }
}

/// D2 pool (s517 F1): a sender's first ACCEPTED checked taker of the batch
/// picks the market of its free-margin pool. T (100) sends [off-tick GTC
/// sell 1 @200.5 in m1, market sell 20 (cap 1, no mark) in m2]; B's bid 20
/// @100 rests earlier in m2 (no start-of-batch bid or ask there, so no
/// Option B floor and no same-batch top-up). Before A the off-tick sell
/// (rejected later by the book) claimed the pool for m1 and the m2 market
/// sell had only its reservation (cap 1: 1) — it filled nothing. Now the
/// m2 sell gets the pool: 1 + 99 = IM(2,000) → fills 20 @100.
#[test]
fn off_tick_sell_does_not_claim_the_d2_pool() {
    let (t, b) = (addr(1), addr(3));
    let off_tick_sell = order(1, false, fp(200) + half(), fp(1), TimeInForce::GTC);
    let block = vec![
        place(b, gtc(2, true, 100, 20)),
        place(t, off_tick_sell),
        place(t, market(2, false, fp(1), fp(20))),
    ];
    let mut worlds = Vec::new();
    for mode in [Some(0), Some(2), Some(4)] {
        let dir = tempfile::tempdir().unwrap();
        let db = StateDb::open(dir.path()).unwrap();
        let mut ctx = NativeExecContext::new(db, 1, 1000, 0, 100, 10, addr(99), addr(100), addr(101));
        fund_native(&ctx, &t, fp(100));
        fund_native(&ctx, &b, fp(1_000));
        let r = exec(&mut ctx, mode, &block);
        assert!(is_tick_reject(&r[1]), "{mode:?}: {:?}", r[1]);
        assert!(r[2].0, "{mode:?}: {:?}", r[2]);
        let pos = ctx.positions.get_position(&t, 2).unwrap().expect("t short in m2");
        assert!(!pos.is_long && pos.size == fp(20), "{mode:?}: the m2 sell used the pool: {pos:?}");
        let bal = ctx.positions.get_native_balance(&t).unwrap();
        assert_eq!((bal.available, bal.order_margin), (fp(100), FixedPoint::ZERO), "{mode:?}");
        assert!(!ctx.order_books.contains_key(&1), "{mode:?}: no book created for the reject");
        ctx.save_order_books();
        worlds.push((r, state_dump(&ctx), ctx.next_global_order_id));
    }
    assert!(worlds.windows(2).all(|w| w[0] == w[1]), "engine modes differ");
}
