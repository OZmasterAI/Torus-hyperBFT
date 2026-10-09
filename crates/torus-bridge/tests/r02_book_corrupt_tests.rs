//! R02 branch 3 (owner s106): the book readers' errors split in two.
//! - `CoreError::BookLayout` stays deterministic-only (NOT a local fault): a
//!   layout / config mismatch every validator with the same chain and build
//!   sees alike (unknown mode marker, mixed layouts, a key shape this build
//!   cannot read, getOrderBook under the order-row layout, a caller asking a
//!   row reader for the classic layout).
//! - `CoreError::BookCorrupt` (a local fault, `is_local_fault`): a row that
//!   fails to decode or contradicts another on THIS node (corrupt meta /
//!   order / level / stop / classic row, missing meta row, ids or seqs that
//!   disagree, a stale or lost node-local order store).
//!
//! Both keep the same message text ("order-book layout error: ..."), so RPC
//! errors and the EVM revert data are byte-identical to before.

use alloy_primitives::Address;

use torus_bridge::native_executor::{BookMode, NativeExecContext, NativeExecutor};
use torus_core::book_reader::{self, BookLayout};
use torus_core::book_rows::{book_meta_key, level_row_key_tagged, SIDE_TAG_BID};
use torus_core::error::CoreError;
use torus_core::position::NativeBalance;
use torus_core::precompiles::{execute_precompile, precompile_address, ADDR_ORDER_BOOK_READER};
use torus_state::cf::{CF_BOOK_ORDER_ROWS, CF_NATIVE_MARKETS, CF_NATIVE_ORDER_BOOKS};
use torus_state::{StateBackend, StateDb};
use torus_types::{FixedPoint, MarketId, NativeAction, OrderType, PlaceOrderParams, TimeInForce};

fn open_test_db() -> (tempfile::TempDir, StateDb) {
    let dir = tempfile::tempdir().expect("create temp dir");
    let db = StateDb::open(dir.path()).expect("open db");
    (dir, db)
}

fn addr(n: u8) -> Address {
    Address::new([n; 20])
}

fn fp(v: i64) -> FixedPoint {
    FixedPoint::from_raw(v as i128 * FixedPoint::SCALE)
}

fn gtc(market_id: MarketId, is_buy: bool, price: i64, qty: i64) -> (Address, NativeAction) {
    (
        addr(1),
        NativeAction::PlaceOrder(PlaceOrderParams {
            market_id,
            is_buy,
            price: fp(price),
            quantity: fp(qty),
            order_type: OrderType::Limit,
            time_in_force: TimeInForce::GTC,
            reduce_only: false,
            client_order_id: None,
        }),
    )
}

/// Market 1: two bids and an ask from trader 1, saved in `mode`.
fn seed(db: &StateDb, mode: BookMode) {
    let mut ctx = NativeExecContext::new_with_mode(
        db.clone(),
        1,
        1_001,
        0,
        100,
        10,
        addr(99),
        addr(100),
        addr(101),
        mode,
        None,
    );
    assert!(ctx.fatal_error.is_none());
    ctx.positions
        .put_native_balance(&addr(1), &NativeBalance { available: fp(10_000_000), order_margin: FixedPoint::ZERO })
        .unwrap();
    let r = NativeExecutor::execute_batch(&mut ctx, &[gtc(1, true, 100, 5), gtc(1, true, 99, 7), gtc(1, false, 105, 4)]);
    assert!(r.results.iter().all(|x| x.success));
    ctx.save_order_books();
}

fn rows(db: &StateDb, cf: &str) -> Vec<(Vec<u8>, Vec<u8>)> {
    StateBackend::iterate_cf(db, cf, None).unwrap()
}

#[track_caller]
fn assert_local<T>(r: Result<T, CoreError>, what: &str) {
    let e = r.err().unwrap_or_else(|| panic!("{what}: expected an error"));
    assert!(matches!(e, CoreError::BookCorrupt(_)), "{what}: {e:?}");
    assert!(e.is_local_fault(), "{what}: {e}");
    assert!(e.to_string().starts_with("order-book layout error: "), "{what}: {e}");
}

#[track_caller]
fn assert_deterministic<T>(r: Result<T, CoreError>, what: &str) {
    let e = r.err().unwrap_or_else(|| panic!("{what}: expected an error"));
    assert!(matches!(e, CoreError::BookLayout(_)), "{what}: {e:?}");
    assert!(!e.is_local_fault(), "{what}: {e}");
}

#[test]
fn r02_book_corrupt_has_the_book_layout_message() {
    assert_eq!(CoreError::BookCorrupt("x".into()).to_string(), CoreError::BookLayout("x".into()).to_string());
}

// ---- Local: corrupt on this node -------------------------------------------

#[test]
fn r02_missing_meta_row_is_local() {
    let (_d, db) = open_test_db();
    seed(&db, BookMode::OrderRows);
    db.delete_cf_raw(CF_NATIVE_ORDER_BOOKS, &book_meta_key(1)).unwrap();
    assert_local(book_reader::read_book_depth(&db, 1, BookLayout::OrderRows), "orders without meta");
    assert_local(book_reader::rebuild_book(&db, 1, BookLayout::OrderRows), "rebuild without meta");
}

#[test]
fn r02_undecodable_meta_row_is_local() {
    let (_d, db) = open_test_db();
    seed(&db, BookMode::OrderRows);
    db.put_cf_raw(CF_NATIVE_ORDER_BOOKS, &book_meta_key(1), b"x").unwrap();
    assert_local(book_reader::read_book_depth(&db, 1, BookLayout::OrderRows), "meta decode (depth)");
    assert_local(book_reader::read_last_trade_price(&db, 1, BookLayout::OrderRows), "meta decode (ltp)");
}

#[test]
fn r02_undecodable_order_row_is_local() {
    let (_d, db) = open_test_db();
    seed(&db, BookMode::OrderRows);
    let (k, _) = rows(&db, CF_NATIVE_ORDER_BOOKS)
        .into_iter()
        .find(|(k, _)| k.len() == 25)
        .expect("an order row");
    db.put_cf_raw(CF_NATIVE_ORDER_BOOKS, &k, b"x").unwrap();
    assert_local(book_reader::read_book_depth(&db, 1, BookLayout::OrderRows), "order row decode");
    assert_local(
        book_reader::read_open_orders(&db, &addr(1), Some(1), BookLayout::OrderRows, 10),
        "order row decode (open orders)",
    );
}

#[test]
fn r02_order_row_id_mismatch_is_local() {
    let (_d, db) = open_test_db();
    seed(&db, BookMode::OrderRows);
    let order_rows: Vec<_> = rows(&db, CF_NATIVE_ORDER_BOOKS).into_iter().filter(|(k, _)| k.len() == 25).collect();
    // Row 0's payload under row 1's key: the ids disagree.
    db.put_cf_raw(CF_NATIVE_ORDER_BOOKS, &order_rows[1].0, &order_rows[0].1).unwrap();
    assert_local(
        book_reader::read_open_orders(&db, &addr(1), Some(1), BookLayout::OrderRows, 10),
        "key id != payload id",
    );
}

#[test]
fn r02_bad_level_row_is_local() {
    let (_d, db) = open_test_db();
    seed(&db, BookMode::LevelAuthority);
    let key = level_row_key_tagged(1, SIDE_TAG_BID, fp(100).raw());
    db.put_cf_raw(CF_NATIVE_ORDER_BOOKS, &key, b"short").unwrap();
    assert_local(book_reader::read_book_depth(&db, 1, BookLayout::LevelAuthority), "level row len");
}

#[test]
fn r02_stale_node_local_order_store_is_local() {
    let (_d, db) = open_test_db();
    seed(&db, BookMode::LevelAuthority);
    for (k, _) in rows(&db, CF_BOOK_ORDER_ROWS) {
        db.delete_cf_raw(CF_BOOK_ORDER_ROWS, &k).unwrap();
    }
    assert_local(
        book_reader::read_open_orders(&db, &addr(1), Some(1), BookLayout::LevelAuthority, 10),
        "store lost (one market)",
    );
    assert_local(
        book_reader::read_open_orders(&db, &addr(1), None, BookLayout::LevelAuthority, 10),
        "store lost (all markets)",
    );
    assert_local(book_reader::rebuild_book(&db, 1, BookLayout::LevelAuthority), "store lost (rebuild)");
}

#[test]
fn r02_undecodable_node_local_order_store_key_is_local() {
    let (_d, db) = open_test_db();
    seed(&db, BookMode::LevelAuthority);
    db.put_cf_raw(CF_BOOK_ORDER_ROWS, &[0u8; 3], b"x").unwrap();
    assert_local(
        book_reader::read_open_orders(&db, &addr(1), None, BookLayout::LevelAuthority, 10),
        "bad store key",
    );
}

#[test]
fn r02_split_brain_at_detect_layout_is_local() {
    let (_d, db) = open_test_db();
    seed(&db, BookMode::LevelAuthority);
    for (k, _) in rows(&db, CF_NATIVE_ORDER_BOOKS) {
        db.delete_cf_raw(CF_NATIVE_ORDER_BOOKS, &k).unwrap();
    }
    db.delete_cf_raw(CF_NATIVE_MARKETS, book_reader::BOOK_MODE_MARKER_KEY).unwrap();
    assert_local(book_reader::detect_layout(&db), "store rows, no book rows");
}

#[test]
fn r02_wrong_length_mode_marker_is_local() {
    let (_d, db) = open_test_db();
    seed(&db, BookMode::OrderRows);
    db.put_cf_raw(CF_NATIVE_MARKETS, book_reader::BOOK_MODE_MARKER_KEY, &[1, 1]).unwrap();
    assert_local(book_reader::detect_layout(&db), "2-byte marker");
}

#[test]
fn r02_undecodable_classic_blob_is_local() {
    let (_d, db) = open_test_db();
    seed(&db, BookMode::Classic);
    db.put_cf_raw(CF_NATIVE_ORDER_BOOKS, &1u64.to_be_bytes(), b"x").unwrap();
    assert_local(book_reader::read_book_depth(&db, 1, BookLayout::Classic), "classic blob decode");
}

// ---- Deterministic: layout / config mismatch -------------------------------

#[test]
fn r02_unknown_mode_marker_is_deterministic() {
    let (_d, db) = open_test_db();
    seed(&db, BookMode::OrderRows);
    db.put_cf_raw(CF_NATIVE_MARKETS, book_reader::BOOK_MODE_MARKER_KEY, &[9]).unwrap();
    assert_deterministic(book_reader::detect_layout(&db), "unknown marker byte");
}

#[test]
fn r02_mixed_layouts_are_deterministic() {
    let (_d, db) = open_test_db();
    seed(&db, BookMode::OrderRows);
    db.put_cf_raw(CF_NATIVE_ORDER_BOOKS, &7u64.to_be_bytes(), b"blob").unwrap();
    assert_deterministic(book_reader::read_book_depth(&db, 7, BookLayout::OrderRows), "classic blob in a row CF");
    // Without the marker the sniff sees both layouts.
    db.delete_cf_raw(CF_NATIVE_MARKETS, book_reader::BOOK_MODE_MARKER_KEY).unwrap();
    assert_deterministic(book_reader::detect_layout(&db), "sniffed mixed layouts");
}

#[test]
fn r02_unrecognized_book_key_shape_is_deterministic() {
    let (_d, db) = open_test_db();
    seed(&db, BookMode::OrderRows);
    let mut key = 1u64.to_be_bytes().to_vec();
    key.extend_from_slice(&[0x07, 0, 0]);
    db.put_cf_raw(CF_NATIVE_ORDER_BOOKS, &key, b"x").unwrap();
    assert_deterministic(book_reader::read_book_depth(&db, 1, BookLayout::OrderRows), "unknown key shape");
}

#[test]
fn r02_classic_layout_asked_of_a_row_reader_is_deterministic() {
    assert_deterministic(book_reader::depth_from_rows(1, BookLayout::Classic, &[]), "depth_from_rows classic");
    let (_d, db) = open_test_db();
    assert_deterministic(
        book_reader::depth_from_market_rows(&db, 1, BookLayout::Classic, &[]),
        "depth_from_market_rows classic",
    );
}

/// getOrderBook under the order-row layout: every validator reverts alike.
/// A wrong-length level key under the level prefix is a corrupt row (local);
/// the revert text is unchanged either way.
#[test]
fn r02_get_order_book_errors_split() {
    let call = |db: &StateDb| {
        let mut input = alloy_primitives::keccak256(b"getOrderBook(bytes32)")[..4].to_vec();
        let mut word = [0u8; 32];
        word[24..32].copy_from_slice(&1u64.to_be_bytes());
        input.extend_from_slice(&word);
        execute_precompile(&precompile_address(ADDR_ORDER_BOOK_READER), &input, &addr(0), db, 100, 0)
    };
    let (_d, db) = open_test_db();
    seed(&db, BookMode::OrderRows);
    assert_deterministic(call(&db), "getOrderBook under order rows");

    let (_d, db) = open_test_db();
    seed(&db, BookMode::LevelAuthority);
    let mut key = level_row_key_tagged(1, SIDE_TAG_BID, 1).to_vec();
    key.push(0);
    db.put_cf_raw(CF_NATIVE_ORDER_BOOKS, &key, b"junk").unwrap();
    assert_local(call(&db), "wrong-length level key");
}
