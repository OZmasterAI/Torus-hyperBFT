//! Review of fix/read-precompile-gas (blocking): a reader precompile's WORK is
//! bounded by the gas its caller pays, not only its charge. The EVM provider
//! hands each reader a [`ReadMeter`] of `(gas_limit - 2,600) / 50` units (a
//! row or 32 blob bytes read, a 32-byte word returned); a reader stops and
//! returns `PrecompileOutOfGas` as soon as it would exceed it. So a contract
//! looping `staticcall{gas: 2,600 + small}` cannot make the node scan a whole
//! market list / book per call.
//!
//! Re-review: the charge (hence gas_used / out-of-gas, i.e. block results)
//! depends only on hashed consensus rows — never on the node-local
//! `__book_mode__` marker — a scan reads at most `remaining + 1` rows (+ the
//! node-local rows it skips), and a classic blob is sized before it is read.

#[path = "common/counting_backend.rs"]
mod counting_backend;

use std::sync::atomic::Ordering;

use alloy_primitives::{Address, U256};
use counting_backend::CountingBackend;
use torus_bridge::native_executor::{BookMode, NativeExecContext, NativeExecutor};
use torus_core::book_reader::BOOK_MODE_MARKER_KEY;
use torus_core::error::CoreError;
use torus_core::position::NativeBalance;
use torus_core::precompiles::{
    execute_precompile_metered, precompile_address, write_stored_order, ReadMeter, StoredOrder,
    ADDR_BALANCE_READER, ADDR_ORDER_BOOK_READER,
};
use torus_state::cf::{CF_NATIVE_MARKETS, CF_NATIVE_ORDER_BOOKS};
use torus_state::{StateBackend, StateDb};
use torus_types::{FixedPoint, MarketId, NativeAction, OrderType, PlaceOrderParams, TimeInForce};

fn open_test_db() -> (tempfile::TempDir, StateDb) {
    let dir = tempfile::tempdir().expect("create temp dir");
    let db = StateDb::open(dir.path()).expect("open db");
    (dir, db)
}

fn fp(v: i64) -> FixedPoint {
    FixedPoint::from_raw(v as i128 * FixedPoint::SCALE)
}

fn call(
    state: &impl StateBackend,
    id: u16,
    sig: &str,
    words: &[[u8; 32]],
    meter: &mut ReadMeter,
) -> Result<Vec<u8>, CoreError> {
    let mut input = alloy_primitives::keccak256(sig.as_bytes())[..4].to_vec();
    for w in words {
        input.extend_from_slice(w);
    }
    execute_precompile_metered(
        &precompile_address(id),
        &input,
        &Address::ZERO,
        U256::ZERO,
        state,
        100,
        0,
        false,
        meter,
    )
}

fn market_word(m: MarketId) -> [u8; 32] {
    let mut w = [0u8; 32];
    w[24..].copy_from_slice(&m.to_be_bytes());
    w
}

fn addr_word(a: &Address) -> [u8; 32] {
    let mut w = [0u8; 32];
    w[12..].copy_from_slice(a.as_slice());
    w
}

fn oog(r: &Result<Vec<u8>, CoreError>) -> bool {
    matches!(r, Err(CoreError::PrecompileOutOfGas))
}

fn rows_read(cb: &CountingBackend<StateDb>) -> usize {
    cb.counts.rows_read.load(Ordering::SeqCst)
}

fn order_book(state: &impl StateBackend, meter: &mut ReadMeter) -> Result<Vec<u8>, CoreError> {
    call(state, ADDR_ORDER_BOOK_READER, "getOrderBook(bytes32)", &[market_word(1)], meter)
}

/// getMarkets over 5,000 markets with a 20-unit budget: out of gas after
/// reading at most 21 rows (it used to read all 5,000 for 2,600 gas). With a
/// large budget the answer equals the unmetered one and the meter holds rows
/// read + words returned.
#[test]
fn get_markets_work_is_bounded_by_the_budget() {
    let (_dir, db) = open_test_db();
    for m in 1..=5_000u64 {
        db.put_cf_raw(CF_NATIVE_MARKETS, &m.to_be_bytes(), &[1u8]).unwrap();
    }
    let cb = CountingBackend::new(db.clone());

    let r = call(&cb, ADDR_BALANCE_READER, "getMarkets()", &[], &mut ReadMeter::with_max(20));
    assert!(oog(&r), "{r:?}");
    assert!(rows_read(&cb) <= 21, "read {} rows on a 20-unit budget", rows_read(&cb));

    let want = call(&db, ADDR_BALANCE_READER, "getMarkets()", &[], &mut ReadMeter::unlimited()).unwrap();
    let mut big = ReadMeter::with_max(1_000_000);
    assert_eq!(call(&db, ADDR_BALANCE_READER, "getMarkets()", &[], &mut big).unwrap(), want);
    assert_eq!(big.used(), 5_000 + (4 + 2 * 5_000), "rows read + words returned");
}

/// One maker rests one order per entry of `prices` (bids) on market 1 under
/// `mode`, saved with the real `save_order_books` (which writes the
/// node-local `__book_mode__` marker).
fn seed_book(db: &StateDb, mode: BookMode, prices: &[i64]) {
    let maker = Address::new([1; 20]);
    let mut ctx = NativeExecContext::new_with_mode(
        db.clone(),
        1,
        1_001,
        0,
        100,
        10,
        Address::new([99; 20]),
        Address::new([100; 20]),
        Address::new([101; 20]),
        mode,
        None,
    );
    ctx.positions
        .put_native_balance(&maker, &NativeBalance { available: fp(100_000_000), order_margin: FixedPoint::ZERO })
        .unwrap();
    let block: Vec<(Address, NativeAction)> = prices
        .iter()
        .map(|&p| {
            (
                maker,
                NativeAction::PlaceOrder(PlaceOrderParams {
                    market_id: 1,
                    is_buy: true,
                    price: fp(p),
                    quantity: fp(1),
                    order_type: OrderType::Limit,
                    time_in_force: TimeInForce::GTC,
                    reduce_only: false,
                    client_order_id: None,
                }),
            )
        })
        .collect();
    let r = NativeExecutor::execute_batch(&mut ctx, &block);
    assert!(r.results.iter().all(|x| x.success), "seed block failed");
    ctx.save_order_books();
}

fn levels(n: i64) -> Vec<i64> {
    (1..=n).collect()
}

/// getOrderBook on a 150-level (mode 2) book: a 40-unit budget runs out of
/// gas after at most 41 rows; a large budget returns exactly the unmetered
/// answer.
#[test]
fn get_order_book_work_is_bounded_by_the_budget() {
    let (_dir, db) = open_test_db();
    seed_book(&db, BookMode::LevelAuthority, &levels(150));
    let cb = CountingBackend::new(db.clone());

    let r = order_book(&cb, &mut ReadMeter::with_max(40));
    assert!(oog(&r), "{r:?}");
    assert!(rows_read(&cb) <= 41, "read {} rows on a 40-unit budget", rows_read(&cb));

    let want = order_book(&db, &mut ReadMeter::unlimited()).unwrap();
    assert_eq!(want.len(), (8 + 150 * 2) * 32, "150 bid levels, no asks");
    let mut big = ReadMeter::with_max(1_000_000);
    assert_eq!(order_book(&db, &mut big).unwrap(), want);
    assert!(big.used() >= 150 + 8 + 300, "rows + words charged, got {}", big.used());
}

/// A stipend below the answer's fixed head (8 words) reads nothing at all.
#[test]
fn get_order_book_with_no_budget_reads_nothing() {
    let (_dir, db) = open_test_db();
    seed_book(&db, BookMode::LevelAuthority, &levels(10));
    let cb = CountingBackend::new(db.clone());
    let r = order_book(&cb, &mut ReadMeter::with_max(0));
    assert!(oog(&r), "{r:?}");
    assert_eq!(rows_read(&cb), 0);
}

/// B1 (determinism): the node-local `__book_mode__` marker (not hashed,
/// absent after a restore) must not change a reader's answer or charge —
/// gas_used / out-of-gas are block results. Same getOrderBook and getMarkets
/// with and without it, and without it the work stays bounded.
#[test]
fn the_node_local_marker_changes_neither_gas_nor_answer() {
    for mode in [BookMode::LevelAuthority, BookMode::OrderRows, BookMode::Classic] {
        let (_dir, db) = open_test_db();
        seed_book(&db, mode, &levels(30));
        for m in 1..=3u64 {
            db.put_cf_raw(CF_NATIVE_MARKETS, &m.to_be_bytes(), &[1u8]).unwrap();
        }
        assert!(db.get_cf_raw(CF_NATIVE_MARKETS, BOOK_MODE_MARKER_KEY).unwrap().is_some(), "{mode:?}");

        let run = |db: &StateDb| {
            let mut mb = ReadMeter::with_max(1_000_000);
            let book = order_book(db, &mut mb);
            let mut mm = ReadMeter::with_max(1_000_000);
            let markets = call(db, ADDR_BALANCE_READER, "getMarkets()", &[], &mut mm).unwrap();
            (book.map_err(|e| e.to_string()), mb.used(), markets, mm.used())
        };
        let with = run(&db);
        db.delete_cf_raw(CF_NATIVE_MARKETS, BOOK_MODE_MARKER_KEY).unwrap();
        let without = run(&db);
        assert_eq!(with, without, "{mode:?}: marker changed the answer or the charge");
        // Charged rows = the hashed rows of cf_native_markets (the 3 markets +
        // the executor's consensus rows), never the marker.
        let hashed = db.iterate_cf(CF_NATIVE_MARKETS, None).unwrap().len() as u64;
        assert_eq!(with.3, hashed + 4 + 2 * 3, "{mode:?}: getMarkets charges hashed rows only");

        let cb = CountingBackend::new(db.clone());
        assert!(oog(&order_book(&cb, &mut ReadMeter::with_max(12))), "{mode:?}");
        assert!(rows_read(&cb) <= 13, "{mode:?}: read {} rows without the marker", rows_read(&cb));
    }
}

/// Classic layout (the default, TORUS_BOOK_ROWS unset): the whole-book blob is
/// sized with a length probe and charged (32 bytes a unit) BEFORE it is read.
/// An under-budget call reads no value bytes at all; with enough budget the
/// production blob still fails to decode, and the revert pays for the blob.
#[test]
fn classic_blob_is_sized_and_charged_before_it_is_read() {
    let (_dir, db) = open_test_db();
    seed_book(&db, BookMode::Classic, &levels(50));
    let blob_units = db
        .get_cf_raw(CF_NATIVE_ORDER_BOOKS, &1u64.to_be_bytes())
        .unwrap()
        .expect("classic blob")
        .len()
        .div_ceil(32) as u64;
    assert!(blob_units > 50, "a 50-order blob is several hundred bytes");

    let cb = CountingBackend::new(db.clone());
    let r = order_book(&cb, &mut ReadMeter::with_max(blob_units - 1));
    assert!(oog(&r), "blob larger than the budget: out of gas, got {r:?}");
    assert_eq!(cb.counts.bytes_read.load(Ordering::SeqCst), 0, "no blob byte read under budget");

    let mut big = ReadMeter::with_max(1_000_000);
    let r = order_book(&db, &mut big);
    assert!(matches!(r, Err(CoreError::Borsh(_))), "production blob still reverts: {r:?}");
    assert!(big.used() >= blob_units, "the revert pays for the blob it read");
}

/// Mode 1 stores one row per ORDER: 30 orders on 3 price levels are charged
/// 30+ rows for a 3-level answer, and a tight budget stops the scan.
#[test]
fn mode1_charges_one_unit_per_order_row() {
    let (_dir, db) = open_test_db();
    let prices: Vec<i64> = (0..30).map(|i| 10 + i % 3).collect();
    seed_book(&db, BookMode::OrderRows, &prices);
    let rows = db.iterate_cf(CF_NATIVE_ORDER_BOOKS, Some(&1u64.to_be_bytes())).unwrap().len() as u64;
    assert!(rows >= 30, "one row per order (+ meta)");

    let mut big = ReadMeter::with_max(1_000_000);
    let out = order_book(&db, &mut big).unwrap();
    assert_eq!(out.len(), (8 + 3 * 2) * 32, "3 bid levels");
    assert_eq!(big.used(), rows + 8 + 6, "every row of the market + the words returned");

    let cb = CountingBackend::new(db.clone());
    assert!(oog(&order_book(&cb, &mut ReadMeter::with_max(10))));
    assert!(rows_read(&cb) <= 11, "read {} rows on a 10-unit budget", rows_read(&cb));
}

/// A scan ending mid-table: trader A's 3 orders are followed in key order by
/// trader B's 1,000. getOpenOrders(A) reads A's rows (the scan stops at the
/// prefix end) and charges 3 rows + 20 words; a 2-unit budget stops early.
#[test]
fn open_orders_scan_stops_at_the_prefix_end() {
    let (_dir, db) = open_test_db();
    let (a, b) = (Address::new([0x0A; 20]), Address::new([0x0B; 20]));
    let order = |id: u128| StoredOrder { order_id: id, price: fp(100), remaining_qty: fp(1), side: 0 };
    for id in 1..=3u128 {
        write_stored_order(&db, &a, 1, &order(id)).unwrap();
    }
    for id in 10..1_010u128 {
        write_stored_order(&db, &b, 1, &order(id)).unwrap();
    }
    let sig = "getOpenOrders(address,bytes32)";
    let cb = CountingBackend::new(db.clone());

    let mut big = ReadMeter::with_max(1_000_000);
    let out = call(&cb, ADDR_ORDER_BOOK_READER, sig, &[addr_word(&a), market_word(1)], &mut big).unwrap();
    assert_eq!(out.len(), (8 + 4 * 3) * 32, "A's 3 orders");
    assert_eq!(big.used(), 3 + 20);
    assert!(rows_read(&cb) <= 3, "read {} rows: past A's prefix", rows_read(&cb));

    let cb = CountingBackend::new(db.clone());
    let r = call(&cb, ADDR_ORDER_BOOK_READER, sig, &[addr_word(&a), market_word(1)], &mut ReadMeter::with_max(2));
    assert!(oog(&r), "{r:?}");
    assert!(rows_read(&cb) <= 3);
}
