//! Item 6 M1 correctness items (owner approved, consensus change; review
//! rows 40-42), every path: the single-action path and the batch engine
//! with serial (0) and sharded (2, 4) Phase 2.
//!
//! * Row 40: a `StopLimit` whose LIMIT is off the tick is rejected at
//!   placement (before the book), like fix A's off-tick `Limit`: no
//!   open-order slot, no margin, no order id, no pending stop.
//! * Row 41: a `Limit` with price <= 0 is rejected before the book (it used
//!   to reach the book, report ok and hold a slot and margin for the batch).
//! * Row 42: a book is created with the tick and lot of the market's
//!   `CF_NATIVE_MARKETS` row (genesis / governance layout), not 1 / 1; a
//!   market without a decodable row keeps 1 / 1, and a book that already
//!   exists keeps its stored tick / lot.

use std::sync::Arc;

use alloy_primitives::{Address, B256};

use torus_bridge::native_executor::{NativeExecContext, NativeExecutor};
use torus_bridge::state_root::compute_native_state_root;
use torus_core::order_book::OrderBook;
use torus_core::position::NativeBalance;
use torus_state::cf::{
    CF_BOOK_ORDER_ROWS, CF_NATIVE_BALANCES, CF_NATIVE_MARKETS, CF_NATIVE_ORDER_BOOKS,
    CF_NATIVE_POSITIONS, CF_NATIVE_TRADES, CF_NATIVE_USER_TRADES,
};
use torus_state::{StateBackend, StateDb};
use torus_telemetry::Metrics;
use torus_types::{FixedPoint, MarketId, NativeAction, OrderType, PlaceOrderParams, TimeInForce};

const S: i128 = FixedPoint::SCALE;

fn addr(n: u8) -> Address {
    Address::new([n; 20])
}

fn fp(v: i64) -> FixedPoint {
    FixedPoint::from_raw(v as i128 * S)
}

fn fpr(raw: i128) -> FixedPoint {
    FixedPoint::from_raw(raw)
}

fn fund_native(ctx: &NativeExecContext, trader: &Address, amount: FixedPoint) {
    let bal = NativeBalance { available: amount, order_margin: FixedPoint::ZERO };
    ctx.positions.put_native_balance(trader, &bal).unwrap();
}

fn order(market_id: MarketId, is_buy: bool, price: FixedPoint, qty: FixedPoint) -> PlaceOrderParams {
    PlaceOrderParams {
        market_id,
        is_buy,
        price,
        quantity: qty,
        order_type: OrderType::Limit,
        time_in_force: TimeInForce::GTC,
        reduce_only: false,
        client_order_id: None,
    }
}

fn gtc(market_id: MarketId, is_buy: bool, price: i64, qty: i64) -> PlaceOrderParams {
    order(market_id, is_buy, fp(price), fp(qty))
}

fn stop_limit(market_id: MarketId, is_buy: bool, trigger: i64, limit: FixedPoint, qty: i64) -> PlaceOrderParams {
    PlaceOrderParams {
        order_type: OrderType::StopLimit { trigger: fp(trigger), limit },
        ..order(market_id, is_buy, FixedPoint::ZERO, fp(qty))
    }
}

fn place(sender: Address, p: PlaceOrderParams) -> (Address, NativeAction) {
    (sender, NativeAction::PlaceOrder(p))
}

/// The genesis / governance market row: base, quote, lot, tick, initial margin.
fn market_row(tick: i128, lot: i128) -> Vec<u8> {
    borsh::to_vec(&("BTC".to_string(), "USD".to_string(), lot, tick, 5 * S)).unwrap()
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

fn err_has(r: &(bool, Option<String>), text: &str) -> bool {
    !r.0 && r.1.as_deref().is_some_and(|e| e.contains(text))
}

/// `None` = the single-action path, `Some(t)` = `execute_batch_engine_mode(_, _, t)`.
type Mode = Option<usize>;
const MODES: [Mode; 4] = [None, Some(0), Some(2), Some(4)];

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

fn world(ctx: &mut NativeExecContext) -> World {
    ctx.save_order_books();
    World {
        dump: state_dump(ctx),
        next_global_order_id: ctx.next_global_order_id,
        trade_index: ctx.trade_index,
        root: compute_native_state_root(&ctx.state).expect("root"),
    }
}

/// Book m1 (tick 2, lot 2) with maker addr(2)'s ask 5 @60; then `block`.
fn run_m1(mode: Mode, block: &[(Address, NativeAction)]) -> Run {
    let dir = tempfile::tempdir().unwrap();
    let db = StateDb::open(dir.path()).unwrap();
    let mut ctx = NativeExecContext::new(db, 1, 1000, 0, 100, 10, addr(99), addr(100), addr(101));
    for n in 1..=3u8 {
        fund_native(&ctx, &addr(n), fp(1_000_000));
    }
    exec(&mut ctx, mode, &[place(addr(2), gtc(1, false, 60, 6))]);
    let book = ctx.order_books.get_mut(&1).expect("m1 book");
    book.tick_size = fp(2);
    book.lot_size = fp(2);
    let metrics = Arc::new(Metrics::new());
    ctx.metrics = Some(metrics.clone());
    let results = exec(&mut ctx, mode, block);
    Run {
        results,
        world: world(&mut ctx),
        rejected_other: metrics.orders_rejected_other.get(),
        rejected_book: metrics.orders_rejected_book.get(),
    }
}

/// Every mode: `checks` on the results, the world equals the same block
/// without the rejected orders, and the batch modes agree byte for byte.
fn check_rejects_leave_no_trace(
    block: &[(Address, NativeAction)],
    rejected: &[usize],
    rejected_other: u64,
    checks: impl Fn(Mode, &[(bool, Option<String>)]),
) {
    let valid: Vec<_> =
        block.iter().enumerate().filter(|(i, _)| !rejected.contains(i)).map(|(_, a)| a.clone()).collect();
    let mut batch = Vec::new();
    for mode in MODES {
        let got = run_m1(mode, block);
        checks(mode, &got.results);
        for (i, r) in got.results.iter().enumerate() {
            assert_eq!(r.0, !rejected.contains(&i), "{mode:?} #{i}: {r:?}");
        }
        assert_eq!(got.rejected_other, rejected_other, "{mode:?}: pre-book rejects");
        assert_eq!(got.rejected_book, 0, "{mode:?}: nothing reached the book to be rejected");
        let reference = run_m1(mode, &valid);
        assert!(reference.results.iter().all(|r| r.0), "{mode:?}: {:?}", reference.results);
        assert_eq!(got.world, reference.world, "{mode:?}: the rejected orders changed state");
        if mode.is_some() {
            batch.push((got.results, got.world));
        }
    }
    assert!(batch.windows(2).all(|w| w[0] == w[1]), "engine modes differ");
}

/// Row 40: an off-tick stop-limit LIMIT is rejected at placement on every
/// path (it used to be accepted, hold a slot and margin while pending, and
/// be rejected only when triggered); an on-tick one still goes pending.
/// The stop-limit's own `price` field is not its limit and is not checked.
#[test]
fn row40_off_tick_stop_limit_rejected_at_placement() {
    let (a, c) = (addr(1), addr(3));
    let block = vec![
        place(a, stop_limit(1, true, 70, fp(61), 2)), // 0: limit off the tick 2
        place(a, stop_limit(1, true, 70, fp(62), 2)), // 1: pending
        place(c, stop_limit(1, false, 40, fp(41), 4)), // 2: limit off the tick 2
        place(c, PlaceOrderParams { price: fp(41), ..stop_limit(1, false, 40, fp(42), 4) }), // 3: pending
        place(c, gtc(1, true, 48, 2)),               // 4: rests
    ];
    check_rejects_leave_no_trace(&block, &[0, 2], 2, |mode, r| {
        for i in [0usize, 2] {
            assert!(err_has(&r[i], "order rejected: price"), "{mode:?} #{i}: {:?}", r[i]);
            assert!(err_has(&r[i], "is not a multiple of the tick 2.00000000"), "{mode:?} #{i}: {:?}", r[i]);
        }
    });
}

/// Row 41: a `Limit` with price 0 or below is rejected before the book on
/// every path, both sides, any time in force; a stop-limit's limit <= 0
/// already was (control).
#[test]
fn row41_limit_price_not_positive_rejected_before_the_book() {
    let (a, c) = (addr(1), addr(3));
    let block = vec![
        place(a, gtc(1, true, 0, 2)),                                   // 0
        place(a, gtc(1, false, -2, 2)),                                 // 1
        place(a, PlaceOrderParams { time_in_force: TimeInForce::IOC, ..gtc(1, true, 0, 2) }), // 2
        place(a, stop_limit(1, true, 70, FixedPoint::ZERO, 2)),         // 3: control
        place(a, gtc(1, true, 50, 2)),                                  // 4: rests
        place(c, gtc(1, true, 48, 2)),                                  // 5: rests
    ];
    check_rejects_leave_no_trace(&block, &[0, 1, 2, 3], 4, |mode, r| {
        for i in [0usize, 1, 2] {
            assert!(err_has(&r[i], "limit order requires a positive price"), "{mode:?} #{i}: {:?}", r[i]);
        }
        assert!(err_has(&r[3], "stop-limit order requires a positive limit price"), "{mode:?}: {:?}", r[3]);
    });
}

/// Row 41 / slots: before the fix a price-0 limit took an open-order slot
/// for the batch (it passed Phase 2), so with one slot left the sender's
/// next order hit the limit. Now it takes none.
#[test]
fn row41_rejected_limit_takes_no_open_order_slot() {
    use torus_core::position::OPEN_ORDER_BASE_LIMIT;
    let a = addr(1);
    for mode in MODES {
        let dir = tempfile::tempdir().unwrap();
        let db = StateDb::open(dir.path()).unwrap();
        let mut ctx = NativeExecContext::new(db, 1, 1000, 0, 100, 10, addr(99), addr(100), addr(101));
        fund_native(&ctx, &a, fp(1_000_000_000));
        // Fill all but one slot with resting bids (no volume: base limit).
        let fill: Vec<_> =
            (0..OPEN_ORDER_BASE_LIMIT as i64 - 1).map(|k| place(a, gtc(1, true, 1 + k, 1))).collect();
        for chunk in fill.chunks(200) {
            let r = exec(&mut ctx, mode, chunk);
            assert!(r.iter().all(|r| r.0), "{mode:?}");
        }
        let r = exec(&mut ctx, mode, &[place(a, gtc(2, true, 0, 1)), place(a, gtc(2, true, 5, 1))]);
        assert!(err_has(&r[0], "limit order requires a positive price"), "{mode:?}: {:?}", r[0]);
        assert!(r[1].0, "{mode:?}: the slot is still free: {:?}", r[1]);
    }
}

/// Row 42: a market with a row (tick 0.5, lot 0.1) and no book gets a book
/// with the row's tick and lot on every path; pre-book checks use them; the
/// book persists them. A placeholder row (not a market layout) and a market
/// without a row keep 1 / 1.
#[test]
fn row42_books_created_from_the_market_row() {
    let (a, c) = (addr(1), addr(3));
    let block = vec![
        place(a, order(5, true, fpr(100 * S + S / 2), fpr(S / 5))), // 0: rests (tick 0.5, lot 0.1)
        place(a, order(5, true, fpr(100 * S + S / 4), fp(1))),      // 1: off the tick 0.5
        place(a, order(5, true, fp(100), fpr(S / 20))),             // 2: below the lot 0.1
        place(c, order(5, false, fpr(101 * S + S / 2), fpr(3 * S / 10))), // 3: rests
        place(a, order(6, true, fpr(100 * S + S / 2), fp(1))),      // 4: placeholder row: tick 1
        place(a, order(7, true, fp(100), fpr(S / 2))),              // 5: no row: lot 1
        place(c, order(6, false, fp(101), fp(2))),                  // 6: rests in m6 (1 / 1)
    ];
    let mut batch = Vec::new();
    for mode in MODES {
        let dir = tempfile::tempdir().unwrap();
        let db = StateDb::open(dir.path()).unwrap();
        db.put_cf_raw(CF_NATIVE_MARKETS, &5u64.to_be_bytes(), &market_row(S / 2, S / 10)).unwrap();
        db.put_cf_raw(CF_NATIVE_MARKETS, &6u64.to_be_bytes(), b"listed").unwrap();
        let mut ctx = NativeExecContext::new(db.clone(), 1, 1000, 0, 100, 10, addr(99), addr(100), addr(101));
        for t in [a, c] {
            fund_native(&ctx, &t, fp(1_000_000));
        }
        let r = exec(&mut ctx, mode, &block);
        for i in [0usize, 3, 6] {
            assert!(r[i].0, "{mode:?} #{i}: {:?}", r[i]);
        }
        assert!(err_has(&r[1], "order rejected: price 100.25000000 is not a multiple of the tick 0.50000000"), "{mode:?}: {:?}", r[1]);
        assert!(err_has(&r[2], "order rejected: quantity 0.05000000 below the lot size 0.10000000"), "{mode:?}: {:?}", r[2]);
        assert!(err_has(&r[4], "is not a multiple of the tick 1.00000000"), "{mode:?}: {:?}", r[4]);
        assert!(err_has(&r[5], "below the lot size 1.00000000"), "{mode:?}: {:?}", r[5]);
        let b5 = &ctx.order_books[&5];
        assert_eq!((b5.tick_size, b5.lot_size), (fpr(S / 2), fpr(S / 10)), "{mode:?}");
        assert_eq!(b5.best_bid(), Some(fpr(100 * S + S / 2)), "{mode:?}");
        let b6 = &ctx.order_books[&6];
        assert_eq!((b6.tick_size, b6.lot_size), (FixedPoint::ONE, FixedPoint::ONE), "{mode:?}");
        assert!(!ctx.order_books.contains_key(&7), "{mode:?}: a rejected order creates no book");
        let w = world(&mut ctx);
        // The book's stored meta carries the row's tick / lot across a reload.
        drop(ctx);
        let reloaded = NativeExecContext::new(db, 2, 1001, 0, 100, 10, addr(99), addr(100), addr(101));
        let b5 = &reloaded.order_books[&5];
        assert_eq!((b5.tick_size, b5.lot_size), (fpr(S / 2), fpr(S / 10)), "{mode:?}: reloaded");
        if mode.is_some() {
            batch.push((r, w));
        }
    }
    assert!(batch.windows(2).all(|w| w[0] == w[1]), "engine modes differ");
}

/// Row 42: a book that exists keeps its stored tick / lot even when the
/// market row says otherwise (existing persisted books are not rewritten).
#[test]
fn row42_existing_book_keeps_its_meta() {
    let a = addr(1);
    for mode in MODES {
        let dir = tempfile::tempdir().unwrap();
        let db = StateDb::open(dir.path()).unwrap();
        db.put_cf_raw(CF_NATIVE_MARKETS, &5u64.to_be_bytes(), &market_row(S / 2, S / 10)).unwrap();
        let mut ctx = NativeExecContext::new(db, 1, 1000, 0, 100, 10, addr(99), addr(100), addr(101));
        fund_native(&ctx, &a, fp(1_000_000));
        ctx.order_books.insert(5, OrderBook::new(5, FixedPoint::ONE, FixedPoint::ONE));
        let r = exec(&mut ctx, mode, &[place(a, order(5, true, fpr(100 * S + S / 2), fp(1))), place(a, gtc(5, true, 100, 1))]);
        assert!(err_has(&r[0], "is not a multiple of the tick 1.00000000"), "{mode:?}: {:?}", r[0]);
        assert!(r[1].0, "{mode:?}: {:?}", r[1]);
        assert_eq!(ctx.order_books[&5].tick_size, FixedPoint::ONE, "{mode:?}");
    }
}
