//! Review of fix/read-precompile-gas (blocking): a reader precompile's WORK is
//! bounded by the gas its caller pays, not only its charge. The EVM provider
//! hands each reader a [`ReadMeter`] of `(gas_limit - 2,600) / 50` units (a
//! row or 32 blob bytes read, a 32-byte word returned); a reader stops and
//! returns `PrecompileOutOfGas` as soon as it would exceed it. So a contract
//! looping `staticcall{gas: 2,600 + small}` cannot make the node scan a whole
//! market list / book per call.

#[path = "common/counting_backend.rs"]
mod counting_backend;

use std::sync::atomic::Ordering;

use alloy_primitives::{Address, U256};
use counting_backend::CountingBackend;
use torus_bridge::native_executor::{BookMode, NativeExecContext, NativeExecutor};
use torus_core::error::CoreError;
use torus_core::position::NativeBalance;
use torus_core::precompiles::{
    execute_precompile_metered, precompile_address, ReadMeter, ADDR_BALANCE_READER,
    ADDR_ORDER_BOOK_READER,
};
use torus_state::cf::CF_NATIVE_MARKETS;
use torus_state::{StateBackend, StateDb};
use torus_types::{FixedPoint, MarketId, NativeAction, OrderType, PlaceOrderParams, TimeInForce};

/// Rows one bounded scan may read past what it is charged for: the rest of
/// its last page (64) plus the row that proves the budget is exceeded.
const SCAN_SLACK: usize = 65;

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

fn oog(r: &Result<Vec<u8>, CoreError>) -> bool {
    matches!(r, Err(CoreError::PrecompileOutOfGas))
}

/// getMarkets over 5,000 markets with a 20-unit budget: out of gas after
/// reading at most 20 + slack rows (it used to read all 5,000 for 2,600 gas).
/// With a large budget the answer equals the unmetered one and the meter
/// holds rows read + words returned.
#[test]
fn get_markets_work_is_bounded_by_the_budget() {
    let (_dir, db) = open_test_db();
    for m in 1..=5_000u64 {
        db.put_cf_raw(CF_NATIVE_MARKETS, &m.to_be_bytes(), &[1u8]).unwrap();
    }
    let cb = CountingBackend::new(db.clone());

    let mut tight = ReadMeter::with_max(20);
    let r = call(&cb, ADDR_BALANCE_READER, "getMarkets()", &[], &mut tight);
    assert!(oog(&r), "{r:?}");
    let read = cb.counts.rows_read.load(Ordering::SeqCst);
    assert!(read <= 20 + SCAN_SLACK, "read {read} rows on a 20-unit budget");

    let want = call(&db, ADDR_BALANCE_READER, "getMarkets()", &[], &mut ReadMeter::unlimited()).unwrap();
    let mut big = ReadMeter::with_max(1_000_000);
    assert_eq!(call(&db, ADDR_BALANCE_READER, "getMarkets()", &[], &mut big).unwrap(), want);
    assert_eq!(big.used(), 5_000 + (4 + 2 * 5_000), "rows read + words returned");
}

/// One maker rests `levels` bids at distinct prices (one level each) in a
/// level-row (mode 2) book on market 1.
fn seed_level_book(db: &StateDb, levels: i64) {
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
        BookMode::LevelAuthority,
        None,
    );
    ctx.positions
        .put_native_balance(&maker, &NativeBalance { available: fp(100_000_000), order_margin: FixedPoint::ZERO })
        .unwrap();
    let block: Vec<(Address, NativeAction)> = (1..=levels)
        .map(|p| {
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

/// getOrderBook on a 150-level book: a 40-unit budget runs out of gas after
/// at most 40 + slack rows (+ the layout marker read); a large budget returns
/// exactly the unmetered answer.
#[test]
fn get_order_book_work_is_bounded_by_the_budget() {
    let (_dir, db) = open_test_db();
    seed_level_book(&db, 150);
    let cb = CountingBackend::new(db.clone());
    let sig = "getOrderBook(bytes32)";

    let mut tight = ReadMeter::with_max(40);
    let r = call(&cb, ADDR_ORDER_BOOK_READER, sig, &[market_word(1)], &mut tight);
    assert!(oog(&r), "{r:?}");
    let read = cb.counts.rows_read.load(Ordering::SeqCst);
    assert!(read <= 40 + SCAN_SLACK + 2, "read {read} rows on a 40-unit budget");

    let want = call(&db, ADDR_ORDER_BOOK_READER, sig, &[market_word(1)], &mut ReadMeter::unlimited()).unwrap();
    assert_eq!(want.len(), (8 + 150 * 2) * 32, "150 bid levels, no asks");
    let mut big = ReadMeter::with_max(1_000_000);
    assert_eq!(call(&db, ADDR_ORDER_BOOK_READER, sig, &[market_word(1)], &mut big).unwrap(), want);
    assert!(big.used() >= 150 + 8 + 300, "rows + words charged, got {}", big.used());
}

/// A stipend below the answer's fixed head (8 words) reads nothing at all.
#[test]
fn get_order_book_with_no_budget_reads_nothing() {
    let (_dir, db) = open_test_db();
    seed_level_book(&db, 10);
    let cb = CountingBackend::new(db.clone());
    let r = call(&cb, ADDR_ORDER_BOOK_READER, "getOrderBook(bytes32)", &[market_word(1)], &mut ReadMeter::with_max(0));
    assert!(oog(&r), "{r:?}");
    assert_eq!(cb.counts.rows_read.load(Ordering::SeqCst), 0);
}

/// Classic layout: the whole-book blob is charged by its size (32 bytes per
/// unit) BEFORE it is decoded. A budget below that is out of gas (the decode
/// never runs: the production blob would otherwise fail with a borsh error);
/// with enough budget the decode error is charged the work done, not the base.
#[test]
fn classic_blob_is_charged_by_size_before_decode() {
    let (_dir, db) = open_test_db();
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
        BookMode::Classic,
        None,
    );
    ctx.positions
        .put_native_balance(&maker, &NativeBalance { available: fp(100_000_000), order_margin: FixedPoint::ZERO })
        .unwrap();
    let block: Vec<(Address, NativeAction)> = (1..=50)
        .map(|p| {
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
    assert!(NativeExecutor::execute_batch(&mut ctx, &block).results.iter().all(|x| x.success));
    ctx.save_order_books();
    let blob_units = db
        .get_cf_raw(torus_state::cf::CF_NATIVE_ORDER_BOOKS, &1u64.to_be_bytes())
        .unwrap()
        .expect("classic blob")
        .len()
        .div_ceil(32) as u64;
    assert!(blob_units > 50, "a 50-order blob is several hundred bytes");

    let sig = "getOrderBook(bytes32)";
    let r = call(&db, ADDR_ORDER_BOOK_READER, sig, &[market_word(1)], &mut ReadMeter::with_max(blob_units - 1));
    assert!(oog(&r), "blob larger than the budget: out of gas before decode, got {r:?}");

    let mut big = ReadMeter::with_max(1_000_000);
    let r = call(&db, ADDR_ORDER_BOOK_READER, sig, &[market_word(1)], &mut big);
    assert!(matches!(r, Err(CoreError::Borsh(_))), "production blob still reverts: {r:?}");
    assert!(big.used() >= blob_units, "the revert pays for the blob it read");
}
