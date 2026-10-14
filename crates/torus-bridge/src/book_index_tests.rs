//! s120: `order_books` is private; whole books are inserted, replaced,
//! taken or moved only through [`NativeExecContext`] methods that keep the
//! books' dependent indexes consistent: the cancel-all index
//! ([`TraderMarkets`], a superset of every book's traders) and the
//! cross-market cloid index ([`CloidMarkets`], every live cloid at its
//! market and id; exact once the stash drains the logs).
//!
//! Each test builds both indexes first (a check with a cloid, a
//! cancel-all), then goes through one path and checks:
//! - the verdicts (duplicate cloid found / freed, cancel-all reaches the
//!   order), which is what the indexes are for;
//! - the carried indexes against the ones a load would build now, on the
//!   context and after the stash (exact there).

use super::*;
use torus_core::position::NativeBalance;
use torus_types::{OrderType, PlaceOrderParams, TimeInForce};

const C: u128 = 0xb00c_0000_0000_0000_0000_0000_0000_0001;

fn addr(n: u8) -> Address {
    Address::new([n; 20])
}

fn fp(v: i64) -> FixedPoint {
    FixedPoint::from_raw(v as i128 * FixedPoint::SCALE)
}

fn params(market_id: MarketId, is_buy: bool, price: i64, cloid: Option<u128>) -> PlaceOrderParams {
    PlaceOrderParams {
        market_id,
        is_buy,
        price: fp(price),
        quantity: fp(1),
        order_type: OrderType::Limit,
        time_in_force: TimeInForce::GTC,
        reduce_only: false,
        client_order_id: cloid,
    }
}

fn buy(market_id: MarketId, price: i64, cloid: Option<u128>) -> NativeAction {
    NativeAction::PlaceOrder(params(market_id, true, price, cloid))
}

fn sell(market_id: MarketId, price: i64, cloid: Option<u128>) -> NativeAction {
    NativeAction::PlaceOrder(params(market_id, false, price, cloid))
}

fn cancel_all() -> NativeAction {
    NativeAction::CancelAllOrders { market_id: None }
}

fn ctx_at(db: &StateDb, height: u64, holder: &mut ResidentBooks) -> NativeExecContext {
    let ctx = NativeExecContext::new_with_mode(
        db.clone(),
        height,
        1000 + height,
        0,
        100,
        10,
        addr(99),
        addr(100),
        addr(101),
        BookMode::Classic,
        Some(holder),
    );
    assert!(ctx.fatal_error.is_none(), "{:?}", ctx.fatal_error);
    for n in 1..=4 {
        let bal = NativeBalance {
            available: fp(100_000_000),
            order_margin: FixedPoint::ZERO,
        };
        ctx.positions.put_native_balance(&addr(n), &bal).unwrap();
    }
    ctx
}

fn exec(ctx: &mut NativeExecContext, sender: u8, action: NativeAction) -> NativeActionResult {
    NativeExecutor::execute(ctx, &addr(sender), &action)
}

fn ok(r: &NativeActionResult, what: &str) {
    assert!(r.success, "{what}: {r:?}");
}

fn dup(r: &NativeActionResult, what: &str) {
    assert!(!r.success, "{what}: expected a duplicate-cloid reject, got {r:?}");
    assert_eq!(r.reason, FailureReason::DuplicateCloid, "{what}: {r:?}");
}

/// `addr(1)` rests cloid `C + m` in markets 1..=4; both indexes built.
fn seed(ctx: &mut NativeExecContext) {
    for m in 1..=4u64 {
        ok(&exec(ctx, 1, buy(m, 100, Some(C + m as u128))), "seed");
    }
    // Nothing of addr(4) rests: the cancel-all only builds the index.
    ok(&exec(ctx, 4, cancel_all()), "seed cancel-all");
    assert!(ctx.cloid_index().0.is_some(), "seed: cloid index built");
    assert!(ctx.trader_index().0.is_some(), "seed: cancel-all index built");
    assert_consistent(ctx, "seed");
}

/// What a check relies on: every live cloid is indexed at its market and
/// id; every trader present in a book is listed for that market.
fn assert_consistent(ctx: &NativeExecContext, what: &str) {
    let (carried, rebuilt) = ctx.cloid_index();
    if let Some(carried) = carried {
        for (key, slot) in &rebuilt {
            assert_eq!(carried.get(key), Some(slot), "{what}: cloid {key:?}");
        }
    }
    let (carried, rebuilt) = ctx.trader_index();
    if let Some(carried) = carried {
        for (trader, markets) in &rebuilt {
            for m in markets {
                assert!(
                    carried.get(trader).is_some_and(|c| c.contains(m)),
                    "{what}: {trader} not listed for market {m}"
                );
            }
        }
    }
}

/// The stash drains every log: the carried cloid index is exact, the
/// cancel-all index covers the books.
fn stash_and_check(mut ctx: NativeExecContext, holder: &mut ResidentBooks, what: &str) {
    assert_consistent(&ctx, what);
    ctx.stash_resident(holder);
    let (carried, rebuilt) = holder.cloid_index().expect("stashed");
    if let Some(carried) = carried {
        assert_eq!(carried, rebuilt, "{what}: stashed cloid index");
    }
    let (carried, rebuilt) = holder.trader_index().expect("stashed");
    if let Some(carried) = carried {
        for (trader, markets) in &rebuilt {
            for m in markets {
                assert!(
                    carried.get(trader).is_some_and(|c| c.contains(m)),
                    "{what}: stashed: {trader} not listed for market {m}"
                );
            }
        }
    }
}

/// The batch matching moves each batch book out of the map and back (a
/// market without a book gets a new one): the indexes stay exact.
#[test]
fn batch_move_keeps_indexes_consistent() {
    let dir = tempfile::tempdir().unwrap();
    let db = StateDb::open(dir.path()).unwrap();
    let mut holder = ResidentBooks::default();
    let mut ctx = ctx_at(&db, 1, &mut holder);
    seed(&mut ctx);
    let block = [
        // Fills addr(1)'s C + 1 (removed from the book).
        (addr(2), sell(1, 100, None)),
        (addr(2), buy(2, 90, Some(C + 100))),
        // Market 9 has no book yet.
        (addr(3), buy(9, 90, Some(C + 9))),
    ];
    for r in NativeExecutor::execute_batch(&mut ctx, &block).results {
        ok(&r, "batch");
    }
    assert_consistent(&ctx, "after batch");
    dup(&exec(&mut ctx, 3, sell(5, 500, Some(C + 9))), "C + 9 rests in 9");
    ok(&exec(&mut ctx, 1, sell(5, 500, Some(C + 1))), "C + 1 was filled");
    ok(&exec(&mut ctx, 3, cancel_all()), "cancel-all");
    assert_eq!(ctx.order_book(9).unwrap().open_order_count(&addr(3)), 0);
    stash_and_check(ctx, &mut holder, "batch");
}

/// A book put in from outside (new market) and one replacing a market's
/// book: its cloids are found, the replaced book's are free, a cancel-all
/// reaches its orders.
#[test]
fn insert_and_replace_book_keep_indexes_consistent() {
    let dir = tempfile::tempdir().unwrap();
    let db = StateDb::open(dir.path()).unwrap();
    let mut holder = ResidentBooks::default();
    let mut ctx = ctx_at(&db, 1, &mut holder);
    seed(&mut ctx);

    let mut book = OrderBook::new(7, FixedPoint::ONE, FixedPoint::ONE);
    book.place_order(params(7, true, 90, Some(C + 7)), addr(3), 1);
    assert!(ctx.insert_order_book(7, book).is_none());
    // Market 1's book (addr(1)'s C + 1) replaced by an empty one.
    let old = ctx.insert_order_book(1, OrderBook::new(1, FixedPoint::ONE, FixedPoint::ONE));
    assert_eq!(old.map(|b| b.order_count()), Some(1));
    assert_consistent(&ctx, "after insert");

    dup(&exec(&mut ctx, 3, sell(8, 500, Some(C + 7))), "C + 7 rests in 7");
    ok(&exec(&mut ctx, 1, sell(8, 500, Some(C + 1))), "C + 1 left with the book");
    dup(&exec(&mut ctx, 1, sell(8, 500, Some(C + 2))), "C + 2 still in 2");
    ok(&exec(&mut ctx, 3, cancel_all()), "cancel-all");
    assert_eq!(ctx.order_book(7).unwrap().open_order_count(&addr(3)), 0, "cancel-all reached 7");
    stash_and_check(ctx, &mut holder, "insert");
}

/// Books taken out of one context and put into another (what tests that
/// carry books across blocks do): neither context keeps a stale index.
#[test]
fn take_and_set_books_keep_indexes_consistent() {
    let dir = tempfile::tempdir().unwrap();
    let db = StateDb::open(dir.path()).unwrap();
    let mut holder = ResidentBooks::default();
    let mut a = ctx_at(&db, 1, &mut holder);
    seed(&mut a);
    let books = a.take_order_books();
    assert!(a.order_books().is_empty());
    assert_consistent(&a, "after take");
    ok(&exec(&mut a, 1, sell(5, 500, Some(C + 1))), "a: C + 1 left with the books");
    let mut holder_a = ResidentBooks::default();
    stash_and_check(a, &mut holder_a, "take");

    let dir2 = tempfile::tempdir().unwrap();
    let db2 = StateDb::open(dir2.path()).unwrap();
    let mut holder = ResidentBooks::default();
    let mut b = ctx_at(&db2, 1, &mut holder);
    // b's own book and both indexes, then every book replaced.
    ok(&exec(&mut b, 3, buy(300, 90, Some(C + 300))), "b: own cloid");
    ok(&exec(&mut b, 4, cancel_all()), "b: cancel-all");
    b.set_order_books(books);
    assert_consistent(&b, "after set");
    dup(&exec(&mut b, 1, sell(6, 500, Some(C + 2))), "b: C + 2 came with the books");
    ok(&exec(&mut b, 3, sell(6, 500, Some(C + 300))), "b: C + 300 left with b's book");
    ok(&exec(&mut b, 1, cancel_all()), "b: cancel-all");
    for m in 1..=4 {
        assert_eq!(b.order_book(m).unwrap().open_order_count(&addr(1)), 0, "b: cancel-all reached {m}");
    }
    stash_and_check(b, &mut holder, "set");
}

/// An order placed straight into a book through `order_book_mut` (tests
/// do) is in both indexes.
#[test]
fn order_book_mut_feeds_indexes() {
    let dir = tempfile::tempdir().unwrap();
    let db = StateDb::open(dir.path()).unwrap();
    let mut holder = ResidentBooks::default();
    let mut ctx = ctx_at(&db, 1, &mut holder);
    seed(&mut ctx);
    ctx.order_book_mut(3)
        .unwrap()
        .place_order(params(3, false, 500, Some(C + 33)), addr(3), 1);
    assert_consistent(&ctx, "after order_book_mut");
    dup(&exec(&mut ctx, 3, sell(8, 500, Some(C + 33))), "C + 33 rests in 3");
    ok(&exec(&mut ctx, 3, cancel_all()), "cancel-all");
    assert_eq!(ctx.order_book(3).unwrap().open_order_count(&addr(3)), 0, "cancel-all reached 3");
    stash_and_check(ctx, &mut holder, "order_book_mut");
}
