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
    // Pre-testnet P0: orders need a listed market (placeholder rows,
    // tick / lot 1, as the old implicit book).
    for m in 1..=512u64 {
        db.put_cf_raw(torus_state::cf::CF_NATIVE_MARKETS, &m.to_be_bytes(), b"listed").unwrap();
    }
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
    // Pre-testnet P0: orders need a listed market (placeholder rows,
    // tick / lot 1, as the old implicit book).
    for m in 1..=512u64 {
        db.put_cf_raw(torus_state::cf::CF_NATIVE_MARKETS, &m.to_be_bytes(), b"listed").unwrap();
    }
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
    // Pre-testnet P0: orders need a listed market (placeholder rows,
    // tick / lot 1, as the old implicit book).
    for m in 1..=512u64 {
        db.put_cf_raw(torus_state::cf::CF_NATIVE_MARKETS, &m.to_be_bytes(), b"listed").unwrap();
    }
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
    // Pre-testnet P0: orders need a listed market (placeholder rows,
    // tick / lot 1, as the old implicit book).
    for m in 1..=512u64 {
        db2.put_cf_raw(torus_state::cf::CF_NATIVE_MARKETS, &m.to_be_bytes(), b"listed").unwrap();
    }
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
    // Pre-testnet P0: orders need a listed market (placeholder rows,
    // tick / lot 1, as the old implicit book).
    for m in 1..=512u64 {
        db.put_cf_raw(torus_state::cf::CF_NATIVE_MARKETS, &m.to_be_bytes(), b"listed").unwrap();
    }
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

// ---- Codex review P2 (s120): a whole book swapped in through the
// `&mut OrderBook` that `order_book_mut` / `for_each_order_book_mut` hand
// out. A loaded or copied book carries no log of its traders and cloids,
// so feeding logs alone leaves them unindexed: the context must notice the
// book is not the one it lent and rebuild both indexes.

#[derive(Clone, Copy, Debug)]
enum Via {
    Guard,
    ForEach,
}

#[derive(Clone, Copy, Debug)]
enum Op {
    Replace,
    Swap,
}

/// `book` as a load (or a copy: `OrderBook` is not `Clone`) gives it: the
/// same orders, empty logs.
fn as_loaded(book: &OrderBook) -> OrderBook {
    let bytes = borsh::to_vec(book).unwrap();
    let mut copy = <OrderBook as borsh::BorshDeserialize>::try_from_slice(&bytes).unwrap();
    copy.drain_new_traders();
    copy.clear_cloid_log();
    copy
}

/// Put `book` at `market` through `via` with `op`; `book` then holds the
/// book it replaced.
fn put_book(ctx: &mut NativeExecContext, via: Via, op: Op, market: MarketId, book: &mut OrderBook) {
    fn put(slot: &mut OrderBook, book: &mut OrderBook, op: Op) {
        match op {
            Op::Swap => std::mem::swap(slot, book),
            Op::Replace => {
                let placeholder = OrderBook::new(0, FixedPoint::ONE, FixedPoint::ONE);
                let incoming = std::mem::replace(book, placeholder);
                *book = std::mem::replace(slot, incoming);
            }
        }
    }
    match via {
        Via::Guard => put(&mut ctx.order_book_mut(market).unwrap(), book, op),
        Via::ForEach => {
            let mut hits = 0;
            ctx.for_each_order_book_mut(|m, slot| {
                if m == market {
                    put(slot, book, op);
                    hits += 1;
                }
            });
            assert_eq!(hits, 1);
        }
    }
}

/// Market 3's book replaced by a loaded one holding `addr(3)`'s `C + 70`
/// and `C + 71`: both are found, `addr(1)`'s `C + 3` left with the old
/// book, a cancel-all of `addr(3)` reaches market 3.
fn loaded_book_is_indexed(via: Via, op: Op) {
    let what = format!("{via:?} {op:?} loaded");
    let dir = tempfile::tempdir().unwrap();
    let db = StateDb::open(dir.path()).unwrap();
    // Pre-testnet P0: orders need a listed market (placeholder rows,
    // tick / lot 1, as the old implicit book).
    for m in 1..=512u64 {
        db.put_cf_raw(torus_state::cf::CF_NATIVE_MARKETS, &m.to_be_bytes(), b"listed").unwrap();
    }
    let mut holder = ResidentBooks::default();
    let mut ctx = ctx_at(&db, 1, &mut holder);
    seed(&mut ctx);

    let mut built = OrderBook::new(3, FixedPoint::ONE, FixedPoint::ONE);
    built.place_order(params(3, true, 90, Some(C + 70)), addr(3), 1);
    built.place_order(params(3, false, 600, Some(C + 71)), addr(3), 1);
    let mut book = as_loaded(&built);
    put_book(&mut ctx, via, op, 3, &mut book);
    assert_eq!(book.open_order_count(&addr(1)), 1, "{what}: the old book came out");
    assert_eq!(ctx.order_book(3).unwrap().open_order_count(&addr(3)), 2, "{what}: in");

    dup(&exec(&mut ctx, 3, sell(8, 500, Some(C + 70))), &format!("{what}: C + 70 rests in 3"));
    dup(&exec(&mut ctx, 3, buy(8, 90, Some(C + 71))), &format!("{what}: C + 71 rests in 3"));
    ok(&exec(&mut ctx, 1, sell(8, 500, Some(C + 3))), &format!("{what}: C + 3 left"));
    assert_consistent(&ctx, &what);
    ok(&exec(&mut ctx, 3, cancel_all()), &what);
    assert_eq!(
        ctx.order_book(3).unwrap().open_order_count(&addr(3)),
        0,
        "{what}: cancel-all reached 3"
    );
    stash_and_check(ctx, &mut holder, &what);
}

/// Market 3's book replaced by a copy of itself taken before `addr(1)`'s
/// `C + 3` was cancelled and `addr(2)`'s `C + 50` rested: `C + 3` is live
/// again, `C + 50` is free, a cancel-all of `addr(1)` reaches market 3.
fn stale_copy_is_indexed(via: Via, op: Op) {
    let what = format!("{via:?} {op:?} copy");
    let dir = tempfile::tempdir().unwrap();
    let db = StateDb::open(dir.path()).unwrap();
    // Pre-testnet P0: orders need a listed market (placeholder rows,
    // tick / lot 1, as the old implicit book).
    for m in 1..=512u64 {
        db.put_cf_raw(torus_state::cf::CF_NATIVE_MARKETS, &m.to_be_bytes(), b"listed").unwrap();
    }
    let mut holder = ResidentBooks::default();
    let mut ctx = ctx_at(&db, 1, &mut holder);
    seed(&mut ctx);

    let mut copy = as_loaded(ctx.order_book(3).unwrap());
    ok(
        &exec(&mut ctx, 1, NativeAction::CancelAllOrders { market_id: Some(3) }),
        &format!("{what}: cancel C + 3"),
    );
    ok(&exec(&mut ctx, 2, buy(3, 95, Some(C + 50))), &format!("{what}: C + 50"));
    dup(&exec(&mut ctx, 2, sell(8, 500, Some(C + 50))), &format!("{what}: C + 50 rests"));
    put_book(&mut ctx, via, op, 3, &mut copy);

    dup(&exec(&mut ctx, 1, sell(8, 500, Some(C + 3))), &format!("{what}: C + 3 is back"));
    ok(&exec(&mut ctx, 2, sell(8, 500, Some(C + 50))), &format!("{what}: C + 50 left"));
    assert_consistent(&ctx, &what);
    ok(&exec(&mut ctx, 1, cancel_all()), &what);
    assert_eq!(
        ctx.order_book(3).unwrap().open_order_count(&addr(1)),
        0,
        "{what}: cancel-all reached 3"
    );
    stash_and_check(ctx, &mut holder, &what);
}

#[test]
fn guard_replace_with_loaded_book_is_indexed() {
    loaded_book_is_indexed(Via::Guard, Op::Replace);
}

#[test]
fn guard_swap_with_loaded_book_is_indexed() {
    loaded_book_is_indexed(Via::Guard, Op::Swap);
}

#[test]
fn for_each_replace_with_loaded_book_is_indexed() {
    loaded_book_is_indexed(Via::ForEach, Op::Replace);
}

#[test]
fn for_each_swap_with_loaded_book_is_indexed() {
    loaded_book_is_indexed(Via::ForEach, Op::Swap);
}

#[test]
fn guard_replace_with_stale_copy_is_indexed() {
    stale_copy_is_indexed(Via::Guard, Op::Replace);
}

#[test]
fn for_each_swap_with_stale_copy_is_indexed() {
    stale_copy_is_indexed(Via::ForEach, Op::Swap);
}

// ---- Codex review r2 P2 (s120): the lent book's own id comes back, but
// its logs were drained elsewhere while it was out: moved into another
// context (`insert_order_book` feeds them to that context's indexes) and
// back, or drained straight through the `&mut`. The same id no longer
// means the logs hold every change since this context last drained them.

/// The ways the lent book's logs leave it while it is lent.
#[derive(Clone, Copy, Debug)]
enum Leak {
    /// Into another context's `insert_order_book` and back out.
    OtherContext,
    /// `drain_new_traders` / `clear_cloid_log` straight on the lent book.
    DrainedInPlace,
}

/// Lend market 3's book through `via`; on it, `addr(3)` rests `C + 80`;
/// then its logs leave through `leak` and the same book goes back in its
/// slot. `C + 80` is found, a cancel-all of `addr(3)` reaches market 3.
fn drained_logs_roundtrip_is_indexed(via: Via, leak: Leak) {
    let what = format!("{via:?} {leak:?} roundtrip");
    let dir = tempfile::tempdir().unwrap();
    let db = StateDb::open(dir.path()).unwrap();
    // Pre-testnet P0: orders need a listed market (placeholder rows,
    // tick / lot 1, as the old implicit book).
    for m in 1..=512u64 {
        db.put_cf_raw(torus_state::cf::CF_NATIVE_MARKETS, &m.to_be_bytes(), b"listed").unwrap();
    }
    let mut holder = ResidentBooks::default();
    let mut ctx = ctx_at(&db, 1, &mut holder);
    seed(&mut ctx);

    let dir_b = tempfile::tempdir().unwrap();
    let db_b = StateDb::open(dir_b.path()).unwrap();
    // Pre-testnet P0: orders need a listed market (placeholder rows,
    // tick / lot 1, as the old implicit book).
    for m in 1..=512u64 {
        db_b.put_cf_raw(torus_state::cf::CF_NATIVE_MARKETS, &m.to_be_bytes(), b"listed").unwrap();
    }
    let mut holder_b = ResidentBooks::default();
    let mut other = ctx_at(&db_b, 1, &mut holder_b);
    seed(&mut other);

    let lent_id = ctx.order_book(3).unwrap().instance_id();
    let mut on_lent = |slot: &mut OrderBook| {
        slot.place_order(params(3, false, 500, Some(C + 80)), addr(3), 1);
        match leak {
            Leak::OtherContext => {
                let placeholder = OrderBook::new(3, FixedPoint::ONE, FixedPoint::ONE);
                let lent = std::mem::replace(slot, placeholder);
                assert!(other.insert_order_book(30, lent).is_none());
                let empty = OrderBook::new(30, FixedPoint::ONE, FixedPoint::ONE);
                let back = other.insert_order_book(30, empty).expect("the lent book");
                drop(std::mem::replace(slot, back));
            }
            Leak::DrainedInPlace => {
                slot.drain_new_traders();
                slot.clear_cloid_log();
            }
        }
    };
    match via {
        Via::Guard => on_lent(&mut ctx.order_book_mut(3).unwrap()),
        Via::ForEach => ctx.for_each_order_book_mut(|m, slot| {
            if m == 3 {
                on_lent(slot);
            }
        }),
    }
    assert_eq!(ctx.order_book(3).unwrap().instance_id(), lent_id, "{what}: the same book is back");
    assert_eq!(ctx.order_book(3).unwrap().open_order_count(&addr(3)), 1, "{what}: C + 80 rests");

    assert_consistent(&ctx, &what);
    dup(&exec(&mut ctx, 3, sell(8, 500, Some(C + 80))), &format!("{what}: C + 80 rests in 3"));
    ok(&exec(&mut ctx, 3, cancel_all()), &what);
    assert_eq!(
        ctx.order_book(3).unwrap().open_order_count(&addr(3)),
        0,
        "{what}: cancel-all reached 3"
    );
    ok(&exec(&mut ctx, 3, sell(8, 500, Some(C + 80))), &format!("{what}: C + 80 freed"));
    stash_and_check(ctx, &mut holder, &what);
}

#[test]
fn guard_roundtrip_through_other_context_is_indexed() {
    drained_logs_roundtrip_is_indexed(Via::Guard, Leak::OtherContext);
}

#[test]
fn for_each_roundtrip_through_other_context_is_indexed() {
    drained_logs_roundtrip_is_indexed(Via::ForEach, Leak::OtherContext);
}

#[test]
fn guard_logs_drained_in_place_are_indexed() {
    drained_logs_roundtrip_is_indexed(Via::Guard, Leak::DrainedInPlace);
}

#[test]
fn for_each_logs_drained_in_place_are_indexed() {
    drained_logs_roundtrip_is_indexed(Via::ForEach, Leak::DrainedInPlace);
}
