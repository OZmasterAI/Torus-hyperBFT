//! Level-row oracle (0x0800 top-N gas round).
//!
//! `CF_NATIVE_ORDER_BOOKS` gains price-level aggregate rows (tag 0x02):
//! `market_id(8 BE) ‖ 0x02 ‖ side(1) ‖ price_enc(16)` → borsh `FixedPoint`
//! total remaining quantity, where `price_enc` is the order-preserving
//! sign-flipped BE encoding, bitwise-NOT'd for bids so that forward
//! lexicographic iteration is best-first on BOTH sides.
//!
//! Oracle invariant pinned here: after ANY save (delta or full), the set of
//! level rows for a market byte-equals the levels of `to_snapshot()` — same
//! prices, summed remaining quantities, no rows for empty levels — and the
//! lexicographic key order is bids best→worst then asks best→worst.

use std::collections::BTreeMap;

use torus_core::order_book::OrderBook;
use torus_core::order_book_store::{
    level_row_key, load_book, save_book_delta, save_book_full, ROW_TAG_LEVEL,
};
use torus_state::cf::CF_NATIVE_ORDER_BOOKS;
use torus_state::{StateBackend, StateDb};
use torus_types::{Address, FixedPoint, OrderType, PlaceOrderParams, Side, TimeInForce};

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

fn open_db() -> (tempfile::TempDir, StateDb) {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = StateDb::open(dir.path()).expect("open db");
    (dir, db)
}

/// Actual level rows in the CF for a market: `key → qty`, in lexicographic
/// key order (BTreeMap), decoded strictly.
fn actual_level_rows(db: &StateDb, market_id: u64) -> BTreeMap<Vec<u8>, FixedPoint> {
    let prefix = market_id.to_be_bytes();
    let mut rows = BTreeMap::new();
    for (key, value) in db
        .iterate_cf(CF_NATIVE_ORDER_BOOKS, Some(&prefix))
        .expect("iterate")
    {
        if key.len() == 26 && key[8] == ROW_TAG_LEVEL {
            let raw: [u8; 16] = value.as_slice().try_into().expect("level qty is 16 BE bytes");
            rows.insert(key, FixedPoint::from_raw(i128::from_be_bytes(raw)));
        }
    }
    rows
}

/// Expected level rows derived from the book's own snapshot aggregation
/// (the consensus-read semantics the precompile serves).
fn expected_level_rows(book: &OrderBook) -> BTreeMap<Vec<u8>, FixedPoint> {
    let snap = book.to_snapshot();
    let mut rows = BTreeMap::new();
    for l in &snap.bids {
        rows.insert(
            level_row_key(book.market_id, Side::Buy, l.price).to_vec(),
            l.quantity,
        );
    }
    for l in &snap.asks {
        rows.insert(
            level_row_key(book.market_id, Side::Sell, l.price).to_vec(),
            l.quantity,
        );
    }
    rows
}

fn assert_levels_match(db: &StateDb, book: &OrderBook, ctx: &str) {
    let actual = actual_level_rows(db, book.market_id);
    let expected = expected_level_rows(book);
    assert_eq!(actual, expected, "level rows != snapshot levels [{ctx}]");
    for qty in actual.values() {
        assert!(*qty > FixedPoint::ZERO, "empty/zero level row persisted [{ctx}]");
    }
}

/// Key-encoding property: forward lexicographic order is best-first per side,
/// and every bid key sorts before every ask key for the same market.
#[test]
fn level_key_order_is_best_first_bids_then_asks() {
    let bid_hi = level_row_key(7, Side::Buy, fp(105)).to_vec();
    let bid_lo = level_row_key(7, Side::Buy, fp(99)).to_vec();
    let ask_lo = level_row_key(7, Side::Sell, fp(106)).to_vec();
    let ask_hi = level_row_key(7, Side::Sell, fp(120)).to_vec();

    // Bids: best (highest) price FIRST.
    assert!(bid_hi < bid_lo, "bid keys must sort best(high)-first");
    // Asks: best (lowest) price FIRST.
    assert!(ask_lo < ask_hi, "ask keys must sort best(low)-first");
    // Side grouping: all bids before all asks.
    assert!(bid_lo < ask_lo, "bid keys must precede ask keys");
    // Market grouping intact (prefix preserved).
    assert_eq!(&bid_hi[..8], &7u64.to_be_bytes());
    assert_eq!(bid_hi[8], ROW_TAG_LEVEL);
}

#[test]
fn delta_saves_keep_level_rows_equal_to_snapshot() {
    let (_dir, db) = open_db();
    let mut book = OrderBook::new(1, fp(1), fp(1));

    // Multi-level, multi-order book: two orders share the 100 bid level.
    book.place_order(limit(1, true, 100, 5), addr(1), 10);
    book.place_order(limit(1, true, 100, 3), addr(2), 11);
    book.place_order(limit(1, true, 99, 2), addr(3), 12);
    book.place_order(limit(1, false, 105, 4), addr(4), 13);
    book.place_order(limit(1, false, 106, 1), addr(5), 14);
    save_book_delta(&db, &mut book).expect("save 1");
    assert_levels_match(&db, &book, "initial resting");

    // Partial fill at 105 (crossing buy 2@105): ask level shrinks 4 -> 2.
    book.place_order(limit(1, true, 105, 2), addr(6), 15);
    // Full fill of the 106 ask (crossing buy 1@106... crosses 105 first).
    save_book_delta(&db, &mut book).expect("save 2");
    assert_levels_match(&db, &book, "after partial fill");

    // Cancel one of the two 100-bids: level must shrink, not vanish.
    let cancelled = book.cancel_all(addr(2), None);
    assert_eq!(cancelled.len(), 1);
    save_book_delta(&db, &mut book).expect("save 3");
    assert_levels_match(&db, &book, "after cancel shared level");

    // Cancel the remaining 100-bid: level row must be DELETED.
    book.cancel_all(addr(1), None);
    save_book_delta(&db, &mut book).expect("save 4");
    assert_levels_match(&db, &book, "after level emptied");

    // Empty the whole book: zero level rows remain.
    book.cancel_all(addr(3), None);
    book.cancel_all(addr(4), None);
    book.cancel_all(addr(5), None);
    save_book_delta(&db, &mut book).expect("save 5");
    assert!(
        actual_level_rows(&db, 1).is_empty(),
        "emptied book must leave no level rows"
    );
}

#[test]
fn full_save_writes_and_reconciles_level_rows() {
    let (_dir, db) = open_db();
    let mut book = OrderBook::new(3, fp(1), fp(1));
    book.place_order(limit(3, true, 50, 5), addr(1), 1);
    book.place_order(limit(3, false, 60, 5), addr(2), 2);
    save_book_full(&db, &mut book).expect("full save");
    assert_levels_match(&db, &book, "full save");

    // Plant a stale level row (price with no orders); full save must remove it.
    let stale_key = level_row_key(3, Side::Buy, fp(42));
    db.put_cf_raw(
        CF_NATIVE_ORDER_BOOKS,
        &stale_key,
        &fp(9).raw().to_be_bytes(),
    )
    .expect("plant stale");
    save_book_full(&db, &mut book).expect("full save 2");
    assert_levels_match(&db, &book, "full save reconciles stale row");
}

#[test]
fn load_book_tolerates_level_rows_and_reload_delta_stays_consistent() {
    let (_dir, db) = open_db();
    let mut book = OrderBook::new(5, fp(1), fp(1));
    book.place_order(limit(5, true, 100, 5), addr(1), 1);
    book.place_order(limit(5, true, 100, 3), addr(2), 2);
    book.place_order(limit(5, false, 110, 4), addr(3), 3);
    save_book_full(&db, &mut book).expect("full save");

    // Load must not choke on 26-byte level rows in the market prefix.
    let mut reloaded = load_book(&db, 5)
        .expect("load with level rows present")
        .expect("book exists");

    // Mutate the RELOADED book, delta-save, and require consistency:
    // shrink the shared 100 level, add a brand-new level.
    reloaded.cancel_all(addr(2), None);
    reloaded.place_order(limit(5, false, 111, 7), addr(4), 4);
    save_book_delta(&db, &mut reloaded).expect("delta after reload");
    assert_levels_match(&db, &reloaded, "delta save on reloaded book");
}
