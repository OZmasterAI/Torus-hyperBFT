//! Deep-book storage round — CHARACTERIZATION ORACLES (book level).
//!
//! These tests pin the SEMANTICS of order-book persistence that must survive
//! the storage-layout change (monolithic borsh blob -> per-order rows):
//! a persisted-then-reloaded book must behave byte-identically (matching
//! order, price-time priority, counters, stops) to the live book it was
//! persisted from.
//!
//! ONLY `persist_reload` below knows the storage format. The assertions in
//! this file must pass UNCHANGED across the refactor — that is the whole
//! point of the oracle.

use torus_core::order_book::{OrderBook, PlaceResult};
use torus_types::{Address, FixedPoint, OrderType, PlaceOrderParams, TimeInForce};

fn fp(n: i64) -> FixedPoint {
    FixedPoint::from_raw(n as i128 * FixedPoint::SCALE)
}

fn addr(n: u8) -> Address {
    Address::from([n; 20])
}

fn limit(market_id: u64, is_buy: bool, price: i64, qty: i64) -> PlaceOrderParams {
    PlaceOrderParams {
        market_id,
        is_buy,
        price: fp(price),
        quantity: fp(qty),
        order_type: OrderType::Limit,
        time_in_force: TimeInForce::GTC,
        reduce_only: false,
        client_order_id: None,
    }
}

fn market(market_id: u64, is_buy: bool, qty: i64) -> PlaceOrderParams {
    PlaceOrderParams {
        market_id,
        is_buy,
        price: FixedPoint::ZERO,
        quantity: fp(qty),
        order_type: OrderType::Market,
        time_in_force: TimeInForce::GTC,
        reduce_only: false,
        client_order_id: None,
    }
}

/// Persist a book through the PRODUCTION storage codec and reload it.
/// This helper is the ONLY format-coupled code in this file: it follows
/// whatever `save_order_books` / the exec load path actually do — today a
/// monolithic borsh-`OrderBook` value in CF_NATIVE_ORDER_BOOKS.
fn persist_reload(book: &mut OrderBook) -> OrderBook {
    use borsh::BorshDeserialize;
    let dir = tempfile::tempdir().expect("tempdir");
    let db = torus_state::StateDb::open(dir.path()).expect("open db");
    let key = book.market_id.to_be_bytes();
    let blob = borsh::to_vec(book).expect("serialize book");
    db.put_cf_raw(torus_state::cf::CF_NATIVE_ORDER_BOOKS, &key, &blob)
        .expect("persist book");
    let bytes = db
        .get_cf_raw(torus_state::cf::CF_NATIVE_ORDER_BOOKS, &key)
        .expect("read book")
        .expect("book present");
    OrderBook::try_from_slice(&bytes).expect("reload book")
}

/// Debug-render the behavioral content of a PlaceResult (status, fills,
/// self-trade cancels, reject reason) for exact comparison.
fn render(r: &PlaceResult) -> String {
    format!(
        "status={:?} fills={:?} stc={:?} reject={:?}",
        r.status, r.fills, r.self_trade_cancels, r.reject_reason
    )
}

/// Snapshot of everything externally observable about a book.
fn observe(book: &OrderBook) -> String {
    let snap = book.to_snapshot();
    format!(
        "snap={snap:?} best_bid={:?} best_ask={:?} count={} bid_levels={} ask_levels={} \
         next_id={} ltp={:?} stops={}",
        book.best_bid(),
        book.best_ask(),
        book.order_count(),
        book.bid_levels(),
        book.ask_levels(),
        book.next_order_id(),
        book.last_trade_price(),
        book.pending_stop_count()
    )
}

/// Build a non-trivial book: multiple traders, same-price FIFO queues, a
/// partial fill, a cancel, and an in-place qty decrease.
fn build_scripted_book() -> OrderBook {
    let mut book = OrderBook::new(1, fp(1), fp(1));
    // Resting bids: two at 100 (FIFO), one at 99.
    book.place_order(limit(1, true, 100, 5), addr(1), 10);
    book.place_order(limit(1, true, 100, 3), addr(2), 11);
    book.place_order(limit(1, true, 99, 2), addr(3), 12);
    // Resting asks: two at 105 (FIFO), one at 106.
    book.place_order(limit(1, false, 105, 4), addr(4), 13);
    let ask2 = book.place_order(limit(1, false, 105, 6), addr(5), 14);
    book.place_order(limit(1, false, 106, 1), addr(6), 15);
    // Partial fill: buy 6 @ 105 consumes addr(4)'s 4 fully and 2 of addr(5)'s 6.
    let r = book.place_order(limit(1, true, 105, 6), addr(7), 16);
    assert_eq!(r.fills.len(), 2, "scripted partial fill");
    // Cancel the 99 bid.
    let cancelled = book
        .cancel_order(3) // third allocated id = addr(3)'s bid
        .expect("cancel scripted bid");
    assert_eq!(cancelled.trader, addr(3));
    // In-place qty decrease keeps time priority.
    book.modify_order(ask2.order_id, None, Some(fp(2)))
        .expect("qty decrease in place");
    book.verify_invariants();
    book
}

/// Oracle 1: a reloaded book is observationally identical to the live book,
/// and identical follow-up operations produce identical results on both.
#[test]
fn oracle_roundtrip_preserves_matching_behavior() {
    let mut live = build_scripted_book();
    let mut reloaded = persist_reload(&mut live);

    assert_eq!(observe(&live), observe(&reloaded), "post-reload observation");

    // Identical follow-up script on both books must match exactly:
    // a sweep of the remaining 105 ask level + a new resting bid + a cancel.
    let follow_ups = vec![
        (limit(1, true, 105, 10), addr(8), 20u64),
        (limit(1, true, 98, 4), addr(9), 21),
        (market(1, false, 3), addr(10), 22),
    ];
    for (params, trader, ts) in follow_ups {
        let a = live.place_order(params.clone(), trader, ts);
        let b = reloaded.place_order(params, trader, ts);
        assert_eq!(render(&a), render(&b), "follow-up op diverged");
    }
    live.verify_invariants();
    reloaded.verify_invariants();
    assert_eq!(observe(&live), observe(&reloaded), "post-follow-up observation");
}

/// Oracle 2 (the killer case for naive per-order keying): modify with qty
/// INCREASE is cancel+reinsert — the order keeps its ID but moves to the BACK
/// of its price level. Reload must preserve that queue order, not order-id
/// order: the next fill must hit the OTHER order first.
#[test]
fn oracle_modify_requeue_priority_survives_reload() {
    let mut live = OrderBook::new(1, fp(1), fp(1));
    let ra = live.place_order(limit(1, true, 100, 5), addr(1), 10);
    let rb = live.place_order(limit(1, true, 100, 5), addr(2), 11);
    // Qty increase => addr(1)'s order loses priority (requeued behind addr(2)).
    live.modify_order(ra.order_id, None, Some(fp(8)))
        .expect("qty increase requeues");

    let mut reloaded = persist_reload(&mut live);
    assert_eq!(observe(&live), observe(&reloaded));

    // A sell for 5 must fill addr(2)'s order (rb) FIRST on both books.
    let a = live.place_order(limit(1, false, 100, 5), addr(3), 20);
    let b = reloaded.place_order(limit(1, false, 100, 5), addr(3), 20);
    assert_eq!(render(&a), render(&b), "fill sequence diverged after reload");
    assert_eq!(
        a.fills[0].maker_order_id, rb.order_id,
        "requeued order must NOT regain priority"
    );
    assert_eq!(observe(&live), observe(&reloaded));
}

/// Oracle 3: same as oracle 2 but with a PRICE change (also cancel+reinsert,
/// same ID, lands at the back of the destination level).
#[test]
fn oracle_modify_price_change_priority_survives_reload() {
    let mut live = OrderBook::new(1, fp(1), fp(1));
    let ra = live.place_order(limit(1, true, 99, 5), addr(1), 10);
    let rb = live.place_order(limit(1, true, 100, 5), addr(2), 11);
    // Move addr(1)'s order UP to 100 — behind addr(2) at that level despite
    // its smaller order id.
    live.modify_order(ra.order_id, Some(fp(100)), None)
        .expect("price change requeues");

    let mut reloaded = persist_reload(&mut live);
    assert_eq!(observe(&live), observe(&reloaded));

    let a = live.place_order(limit(1, false, 100, 5), addr(3), 20);
    let b = reloaded.place_order(limit(1, false, 100, 5), addr(3), 20);
    assert_eq!(render(&a), render(&b));
    assert_eq!(
        a.fills[0].maker_order_id, rb.order_id,
        "price-modified order must rest BEHIND the incumbent at its new level"
    );
}

/// Oracle 4: pending stop orders survive persist+reload and trigger
/// identically on both books.
#[test]
fn oracle_stops_survive_reload() {
    let mut live = OrderBook::new(1, fp(1), fp(1));
    // Establish a last trade price of 100.
    live.place_order(limit(1, false, 100, 1), addr(1), 10);
    live.place_order(limit(1, true, 100, 1), addr(2), 11);
    assert_eq!(live.last_trade_price(), Some(fp(100)));
    // Liquidity for the triggered stop to hit.
    live.place_order(limit(1, false, 103, 5), addr(3), 12);
    // Buy stop-market, triggers when ltp >= 102.
    let rs = live.place_order(
        PlaceOrderParams {
            market_id: 1,
            is_buy: true,
            price: FixedPoint::ZERO,
            quantity: fp(2),
            order_type: OrderType::StopMarket { trigger: fp(102) },
            time_in_force: TimeInForce::GTC,
            reduce_only: false,
            client_order_id: None,
        },
        addr(4),
        13,
    );
    assert_eq!(live.pending_stop_count(), 1, "stop pending: {:?}", rs.status);

    let mut reloaded = persist_reload(&mut live);
    assert_eq!(observe(&live), observe(&reloaded));
    assert_eq!(reloaded.pending_stop_count(), 1, "stop survived reload");

    // Trade at 103 (crossing buy) fires the stop on both books identically.
    let a = live.place_order(limit(1, true, 103, 1), addr(5), 20);
    let b = reloaded.place_order(limit(1, true, 103, 1), addr(5), 20);
    assert_eq!(render(&a), render(&b));
    assert_eq!(live.pending_stop_count(), 0, "stop fired");
    assert_eq!(observe(&live), observe(&reloaded));
}

/// Oracle 5: persistence is idempotent/stable — reload of a reload observes
/// identically, and `orders_for_trader` returns the same order set as the
/// live book (compared as sorted ids; the live Vec order reflects placement
/// order which persistence has never guaranteed).
#[test]
fn oracle_reload_is_stable_and_preserves_trader_orders() {
    let mut live = build_scripted_book();
    let mut r1 = persist_reload(&mut live);
    let r2 = persist_reload(&mut r1);
    assert_eq!(observe(&r1), observe(&r2), "second reload diverged");

    for t in 1u8..=10 {
        let mut live_ids: Vec<u128> = live.orders_for_trader(&addr(t)).iter().map(|o| o.id).collect();
        let mut r_ids: Vec<u128> = r2.orders_for_trader(&addr(t)).iter().map(|o| o.id).collect();
        live_ids.sort_unstable();
        r_ids.sort_unstable();
        assert_eq!(live_ids, r_ids, "trader {t} order set diverged");
        // Exact per-order field equality.
        for id in &live_ids {
            let a = live.get_order(*id).unwrap();
            let b = r2.get_order(*id).unwrap();
            assert_eq!(format!("{a:?}"), format!("{b:?}"), "order {id} fields diverged");
        }
    }
}

/// Oracle 6: an empty (fully drained) book still persists its counters —
/// next_order_id and last_trade_price survive even with zero resting orders.
#[test]
fn oracle_drained_book_keeps_counters() {
    let mut live = OrderBook::new(7, fp(1), fp(1));
    live.place_order(limit(7, false, 100, 1), addr(1), 10);
    live.place_order(limit(7, true, 100, 1), addr(2), 11); // trade, drains book
    assert_eq!(live.order_count(), 0);
    assert_eq!(live.last_trade_price(), Some(fp(100)));
    let next_id = live.next_order_id();
    assert!(next_id > 1);

    let reloaded = persist_reload(&mut live);
    assert_eq!(reloaded.order_count(), 0);
    assert_eq!(reloaded.next_order_id(), next_id, "counter survived drain");
    assert_eq!(reloaded.last_trade_price(), Some(fp(100)));
    assert_eq!(reloaded.market_id, 7);
    assert_eq!(reloaded.tick_size, fp(1));
    assert_eq!(reloaded.lot_size, fp(1));
}
