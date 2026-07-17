//! Unit tests for the per-order-row order-book store (deep-book round).
//!
//! Covers what the characterization oracles cannot: O(touched) write bounds,
//! delta-vs-full byte convergence (catches any missed journal site), loud
//! legacy/corruption failures, and row lifecycle edge cases.

use torus_core::order_book::OrderBook;
use torus_core::order_book_store::{
    header_key, load_all_books, load_book, load_last_trade_price, order_row_key, save_book_delta,
    save_book_full,
};
use torus_state::cf::CF_NATIVE_ORDER_BOOKS;
use torus_state::{StateBackend, StateDb};
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

fn temp_db() -> (tempfile::TempDir, StateDb) {
    let dir = tempfile::tempdir().unwrap();
    let db = StateDb::open(dir.path()).unwrap();
    (dir, db)
}

fn cf_dump(db: &StateDb) -> Vec<(Vec<u8>, Vec<u8>)> {
    StateBackend::iterate_cf(db, CF_NATIVE_ORDER_BOOKS, None).unwrap()
}

/// Deterministic multi-step mutation script, applied in "blocks".
/// Returns per-block closures' effects by applying `ops[block]` to `book`.
fn apply_block(book: &mut OrderBook, block: usize) {
    match block {
        0 => {
            // Depth build: 20 resting bids + 20 resting asks, 4 traders.
            for i in 0..20i64 {
                book.place_order(limit(1, true, 80 - i, 1 + (i % 3)), addr((i % 4) as u8 + 1), 10);
                book.place_order(limit(1, false, 90 + i, 1 + (i % 3)), addr((i % 4) as u8 + 5), 11);
            }
        }
        1 => {
            // Touch a few: partial fill at best ask, one cancel, one requeue.
            book.place_order(limit(1, true, 90, 1), addr(9), 20); // partial vs best ask
            book.cancel_order(3).expect("cancel resting id 3");
            book.modify_order(5, None, Some(fp(9))).expect("qty increase requeue");
        }
        2 => {
            // Sweep two ask levels; place one fresh bid.
            book.place_order(limit(1, true, 91, 5), addr(10), 30);
            book.place_order(limit(1, true, 60, 2), addr(10), 31);
        }
        _ => unreachable!(),
    }
    book.verify_invariants();
}

/// Delta saves after every block must converge to EXACTLY the same CF bytes
/// as one full save of an identically-mutated book — any missed journal site
/// diverges here.
#[test]
fn delta_saves_converge_to_full_save_bytes() {
    let (_d1, db_delta) = temp_db();
    let (_d2, db_full) = temp_db();

    let mut book_delta = OrderBook::new(1, fp(1), fp(1));
    for block in 0..3 {
        apply_block(&mut book_delta, block);
        save_book_delta(&db_delta, &mut book_delta).unwrap();
    }

    let mut book_full = OrderBook::new(1, fp(1), fp(1));
    for block in 0..3 {
        apply_block(&mut book_full, block);
    }
    save_book_full(&db_full, &mut book_full).unwrap();

    assert_eq!(
        cf_dump(&db_delta),
        cf_dump(&db_full),
        "delta-save trajectory must converge to the full-save byte image"
    );

    // And both reload to the same observable book.
    let a = load_book(&db_delta, 1).unwrap().unwrap();
    let b = load_book(&db_full, 1).unwrap().unwrap();
    a.verify_invariants();
    b.verify_invariants();
    assert_eq!(format!("{:?}", a.to_snapshot()), format!("{:?}", b.to_snapshot()));
    assert_eq!(a.order_count(), b.order_count());
    assert_eq!(a.next_order_id(), b.next_order_id());
}

/// THE O(touched) proof at unit level: with a DEEP book (2000 resting), a
/// block that touches a handful of orders writes a handful of rows — not
/// the depth.
#[test]
fn save_delta_writes_o_touched_not_o_depth() {
    let (_dir, db) = temp_db();
    let mut book = OrderBook::new(1, fp(1), fp(1));

    // Depth build: 2000 resting orders far apart (never cross).
    for i in 0..1000i64 {
        book.place_order(limit(1, true, 1_000 + i, 1), addr((i % 20) as u8 + 1), 10);
        book.place_order(limit(1, false, 900_000 + i, 1), addr((i % 20) as u8 + 30), 11);
    }
    let stats0 = save_book_delta(&db, &mut book).unwrap();
    assert_eq!(stats0.rows_written, 2000, "initial build writes all rows");

    // Touched block: 1 new resting bid, 1 cancel, 1 partial fill of best ask.
    book.place_order(limit(1, true, 999, 1), addr(50), 20);
    book.cancel_order(7).unwrap();
    let r = book.place_order(limit(1, true, 900_000, 1), addr(51), 21);
    assert_eq!(r.fills.len(), 1, "scripted partial fill");
    book.verify_invariants();

    let stats1 = save_book_delta(&db, &mut book).unwrap();
    let touched = stats1.rows_written + stats1.rows_deleted;
    assert!(
        touched <= 4,
        "block touching 3 orders must write O(touched) rows, got {touched} \
         (written={}, deleted={})",
        stats1.rows_written,
        stats1.rows_deleted
    );
    assert!(
        stats1.bytes_written < 1024,
        "touched-block bytes must be small, got {}",
        stats1.bytes_written
    );

    // Reload sanity at depth.
    let reloaded = load_book(&db, 1).unwrap().unwrap();
    reloaded.verify_invariants();
    assert_eq!(reloaded.order_count(), book.order_count());
}

/// An order placed AND fully consumed between saves never had a row — the
/// save must not emit a phantom delete for it.
#[test]
fn same_block_place_and_fill_emits_no_phantom_ops() {
    let (_dir, db) = temp_db();
    let mut book = OrderBook::new(1, fp(1), fp(1));
    book.place_order(limit(1, true, 100, 5), addr(1), 10); // rests…
    let r = book.place_order(limit(1, false, 100, 5), addr(2), 11); // …fully consumed
    assert_eq!(r.fills.len(), 1);
    assert_eq!(book.order_count(), 0);

    let stats = save_book_delta(&db, &mut book).unwrap();
    assert_eq!(stats.rows_written, 0, "no resting rows to write");
    assert_eq!(stats.rows_deleted, 0, "no persisted row existed — no phantom delete");

    // Only the header row lands in the CF.
    let dump = cf_dump(&db);
    assert_eq!(dump.len(), 1);
    assert_eq!(dump[0].0, header_key(1).to_vec());

    // Counters still persisted.
    let reloaded = load_book(&db, 1).unwrap().unwrap();
    assert_eq!(reloaded.order_count(), 0);
    assert_eq!(reloaded.next_order_id(), book.next_order_id());
    assert_eq!(reloaded.last_trade_price(), Some(fp(100)));
}

/// Cancelling a PERSISTED order point-deletes its row.
#[test]
fn cancel_after_save_deletes_row() {
    let (_dir, db) = temp_db();
    let mut book = OrderBook::new(1, fp(1), fp(1));
    let ra = book.place_order(limit(1, true, 100, 5), addr(1), 10);
    book.place_order(limit(1, true, 99, 5), addr(2), 11);
    save_book_delta(&db, &mut book).unwrap();
    assert_eq!(cf_dump(&db).len(), 3, "header + 2 rows");

    book.cancel_order(ra.order_id).unwrap();
    let stats = save_book_delta(&db, &mut book).unwrap();
    assert_eq!(stats.rows_deleted, 1);
    let dump = cf_dump(&db);
    assert_eq!(dump.len(), 2, "header + 1 row after delete");
    assert!(!dump
        .iter()
        .any(|(k, _)| k == &order_row_key(1, ra.order_id).to_vec()));

    let reloaded = load_book(&db, 1).unwrap().unwrap();
    assert_eq!(reloaded.order_count(), 1);
    assert!(reloaded.get_order(ra.order_id).is_none());
}

/// A LEGACY monolithic value (8-byte key) fails LOUDLY on every load path.
#[test]
fn legacy_monolithic_value_fails_loudly() {
    let (_dir, db) = temp_db();
    let mut legacy = OrderBook::new(1, fp(1), fp(1));
    legacy.place_order(limit(1, true, 100, 5), addr(1), 10);
    db.put_cf_raw(
        CF_NATIVE_ORDER_BOOKS,
        &1u64.to_be_bytes(),
        &borsh::to_vec(&legacy).unwrap(),
    )
    .unwrap();

    fn expect_legacy<T>(r: Result<T, torus_core::error::CoreError>) {
        match r {
            Err(torus_core::error::CoreError::LegacyOrderBookValue { .. }) => {}
            Err(e) => panic!("expected LegacyOrderBookValue, got {e}"),
            Ok(_) => panic!("expected LegacyOrderBookValue, got Ok"),
        }
    }
    expect_legacy(load_book(&db, 1));
    expect_legacy(load_all_books(&db));
    expect_legacy(load_last_trade_price(&db, 1));
}

/// Orphan order rows (no header) and unknown header versions are corruption
/// errors, never silent skips.
#[test]
fn corruption_fails_loudly() {
    // Orphan row.
    let (_dir, db) = temp_db();
    let mut book = OrderBook::new(1, fp(1), fp(1));
    book.place_order(limit(1, true, 100, 5), addr(1), 10);
    save_book_delta(&db, &mut book).unwrap();
    db.delete_cf_raw(CF_NATIVE_ORDER_BOOKS, &header_key(1)).unwrap();
    assert!(load_book(&db, 1).is_err(), "orphan rows must error");
    assert!(load_all_books(&db).is_err());

    // Unknown header version.
    let (_dir2, db2) = temp_db();
    let mut book2 = OrderBook::new(2, fp(1), fp(1));
    save_book_delta(&db2, &mut book2).unwrap();
    let mut hdr = db2
        .get_cf_raw(CF_NATIVE_ORDER_BOOKS, &header_key(2))
        .unwrap()
        .unwrap();
    hdr[0] = 0xEE;
    db2.put_cf_raw(CF_NATIVE_ORDER_BOOKS, &header_key(2), &hdr)
        .unwrap();
    assert!(load_book(&db2, 2).is_err(), "unknown header version must error");

    // Unrecognized key shape.
    let (_dir3, db3) = temp_db();
    db3.put_cf_raw(CF_NATIVE_ORDER_BOOKS, b"garbage-key!", b"x")
        .unwrap();
    assert!(load_all_books(&db3).is_err(), "unknown key shape must error");
}

/// load_all_books reconstructs multiple markets, and absent markets are None.
#[test]
fn multi_market_load() {
    let (_dir, db) = temp_db();
    let mut b1 = OrderBook::new(1, fp(1), fp(1));
    b1.place_order(limit(1, true, 100, 5), addr(1), 10);
    let mut b2 = OrderBook::new(2, fp(1), fp(1));
    b2.set_next_order_id(50);
    b2.place_order(limit(2, false, 200, 3), addr(2), 11);
    save_book_delta(&db, &mut b1).unwrap();
    save_book_delta(&db, &mut b2).unwrap();

    let books = load_all_books(&db).unwrap();
    assert_eq!(books.len(), 2);
    assert_eq!(books[&1].order_count(), 1);
    assert_eq!(books[&2].order_count(), 1);
    assert_eq!(books[&2].next_order_id(), 51);
    assert!(load_book(&db, 3).unwrap().is_none());
}

/// last_trade_price header read is consistent with the full book load.
#[test]
fn last_trade_price_header_read() {
    let (_dir, db) = temp_db();
    let mut book = OrderBook::new(1, fp(1), fp(1));
    assert!(load_last_trade_price(&db, 1).unwrap().is_none());
    book.place_order(limit(1, false, 105, 2), addr(1), 10);
    book.place_order(limit(1, true, 105, 1), addr(2), 11);
    save_book_delta(&db, &mut book).unwrap();
    assert_eq!(load_last_trade_price(&db, 1).unwrap(), Some(fp(105)));
    assert_eq!(
        load_book(&db, 1).unwrap().unwrap().last_trade_price(),
        Some(fp(105))
    );
}
