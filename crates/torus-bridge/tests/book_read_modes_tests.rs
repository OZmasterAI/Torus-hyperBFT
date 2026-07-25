//! READ-PATH parity across the three book persistence modes (`TORUS_BOOK_ROWS`
//! 0 / 1 / 2).
//!
//! The bug this pins: every book READER assumed the Classic 8-byte-key
//! whole-book blob and returned an EMPTY book (no error) under modes 1 and 2.
//! Contracts:
//!   1. `book_reader::detect_layout` reports the layout actually on disk
//!      (`__book_mode__` marker, else content sniffing) — never an env var.
//!   2. Depth / last-trade-price / per-trader open orders read back IDENTICAL
//!      to the in-memory executor books under ALL three modes.
//!   3. Mode 2 depth comes from the ROOT-CF level rows (consensus authority);
//!      per-trader orders come from the node-local `CF_BOOK_ORDER_ROWS`.
//!   4. Never silently empty: a layout the reader cannot decode (mixed
//!      layouts, missing meta, split-brain node-local store) is an ERROR.
//!   5. Precompile 0x0800 `getOrderBook` serves modes 1/2 instead of handing
//!      an empty book to EVM contracts. The Classic arm is asserted UNCHANGED
//!      (see `classic_precompile_behaviour_is_unchanged`).

use alloy_primitives::Address;

use torus_bridge::native_executor::{BookMode, NativeExecContext, NativeExecutor};
use torus_core::book_reader::{self, BookLayout};
use torus_core::position::NativeBalance;
use torus_core::precompiles::{execute_precompile, precompile_address, ADDR_ORDER_BOOK_READER};
use torus_state::cf::{CF_BOOK_ORDER_ROWS, CF_NATIVE_ORDER_BOOKS};
use torus_state::{StateBackend, StateDb};
use torus_types::{FixedPoint, MarketId, NativeAction, OrderType, PlaceOrderParams, TimeInForce};

// ---- Helpers ---------------------------------------------------------------

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

fn make_ctx(state_db: StateDb, height: u64, mode: BookMode) -> NativeExecContext {
    NativeExecContext::new_with_mode(
        state_db,
        height,
        1000 + height,
        0,
        100,
        10,
        addr(99),
        addr(100),
        addr(101),
        mode,
        None,
    )
}

fn fund_native(ctx: &NativeExecContext, trader: &Address, amount: FixedPoint) {
    let bal = NativeBalance {
        available: amount,
        order_margin: FixedPoint::ZERO,
    };
    ctx.positions.put_native_balance(trader, &bal).unwrap();
}

fn gtc(market_id: MarketId, is_buy: bool, price: i64, qty: i64) -> PlaceOrderParams {
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

fn place(sender: Address, p: PlaceOrderParams) -> (Address, NativeAction) {
    (sender, NativeAction::PlaceOrder(p))
}

/// What the in-memory books say — the oracle every reader must reproduce.
struct Expected {
    m1_bids: Vec<(FixedPoint, FixedPoint, usize)>,
    m1_asks: Vec<(FixedPoint, FixedPoint, usize)>,
    m1_ltp: Option<FixedPoint>,
    m2_bids: Vec<(FixedPoint, FixedPoint, usize)>,
    /// (market, order id, price, remaining qty) for MAKER, ascending order id.
    maker_orders: Vec<(MarketId, u128, FixedPoint, FixedPoint)>,
}

const MAKER: u8 = 1;
const TAKER: u8 = 3;

/// One block of real order flow through the real executor, persisted with the
/// real `save_order_books` in `mode`. Two markets, a partial fill (sets the
/// last trade price), resting depth on both sides.
fn seed(db: &StateDb, mode: BookMode) -> Expected {
    let maker = addr(MAKER);
    let taker = addr(TAKER);

    let mut ctx = make_ctx(db.clone(), 1, mode);
    assert!(ctx.fatal_error.is_none(), "seed load: {:?}", ctx.fatal_error);
    for t in [maker, taker] {
        fund_native(&ctx, &t, fp(10_000_000));
    }

    let block = vec![
        // Market 1: two bid levels + one ask level from MAKER.
        place(maker, gtc(1, true, 100, 5)),
        place(maker, gtc(1, true, 99, 7)),
        place(maker, gtc(1, false, 105, 4)),
        // Market 2: one bid level from MAKER.
        place(maker, gtc(2, true, 50, 3)),
        // TAKER sells into the best bid: partial fill => last_trade_price = 100.
        place(taker, gtc(1, false, 100, 2)),
    ];
    let r = NativeExecutor::execute_batch(&mut ctx, &block);
    assert!(r.results.iter().all(|x| x.success), "seed block failed");
    ctx.save_order_books();

    let b1 = ctx.order_books.get(&1).expect("market 1 book");
    let b2 = ctx.order_books.get(&2).expect("market 2 book");
    let mut maker_orders: Vec<(MarketId, u128, FixedPoint, FixedPoint)> = Vec::new();
    for (mid, book) in [(1u64, b1), (2u64, b2)] {
        for o in book.orders_for_trader(&maker) {
            maker_orders.push((mid, o.id, o.price, o.remaining_qty));
        }
    }
    maker_orders.sort_by_key(|(mid, id, _, _)| (*mid, *id));

    let exp = Expected {
        m1_bids: b1.bid_depth(),
        m1_asks: b1.ask_depth(),
        m1_ltp: b1.last_trade_price(),
        m2_bids: b2.bid_depth(),
        maker_orders,
    };
    assert_eq!(exp.m1_ltp, Some(fp(100)), "the fill must set the LTP");
    assert_eq!(exp.m1_bids.len(), 2, "two resting bid levels on market 1");
    assert_eq!(exp.maker_orders.len(), 4, "maker rests 4 orders");
    exp
}

fn depth_tuples(levels: &[book_reader::DepthLevel]) -> Vec<(FixedPoint, FixedPoint, usize)> {
    levels
        .iter()
        .map(|l| (l.price, l.quantity, l.order_count as usize))
        .collect()
}

/// Decode `n` ABI `uint128[]` arrays out of a precompile response.
fn decode_u128_arrays(out: &[u8], n: usize) -> Vec<Vec<u128>> {
    let word =
        |at: usize| -> u128 { u128::from_be_bytes(out[at + 16..at + 32].try_into().expect("word")) };
    let mut arrays = Vec::with_capacity(n);
    for i in 0..n {
        let off = word(i * 32) as usize;
        let len = word(off) as usize;
        arrays.push((0..len).map(|j| word(off + 32 + j * 32)).collect());
    }
    arrays
}

fn call_get_order_book(db: &StateDb, market_id: MarketId) -> Result<Vec<Vec<u128>>, String> {
    let address = precompile_address(ADDR_ORDER_BOOK_READER);
    let mut input = Vec::with_capacity(36);
    let sel = alloy_primitives::keccak256(b"getOrderBook(bytes32)");
    input.extend_from_slice(&sel[..4]);
    let mut word = [0u8; 32];
    word[24..32].copy_from_slice(&market_id.to_be_bytes());
    input.extend_from_slice(&word);
    execute_precompile(&address, &input, &addr(0), db, 100)
        .map(|out| decode_u128_arrays(&out, 4))
        .map_err(|e| e.to_string())
}

// ---- 1. Layout detection ---------------------------------------------------

#[test]
fn detect_layout_reports_what_is_on_disk() {
    for (mode, want) in [
        (BookMode::Classic, BookLayout::Classic),
        (BookMode::OrderRows, BookLayout::OrderRows),
        (BookMode::LevelAuthority, BookLayout::LevelAuthority),
    ] {
        let (_dir, db) = open_test_db();
        seed(&db, mode);
        assert_eq!(
            book_reader::detect_layout(&db).expect("detect"),
            want,
            "layout for {mode:?}"
        );
    }
}

#[test]
fn detect_layout_sniffs_content_when_the_marker_row_is_absent() {
    for (mode, want) in [
        (BookMode::OrderRows, BookLayout::OrderRows),
        (BookMode::LevelAuthority, BookLayout::LevelAuthority),
    ] {
        let (_dir, db) = open_test_db();
        seed(&db, mode);
        // Pre-marker DB: drop `__book_mode__`, leave the rows.
        db.delete_cf_raw(
            torus_state::cf::CF_NATIVE_MARKETS,
            book_reader::BOOK_MODE_MARKER_KEY,
        )
        .unwrap();
        assert_eq!(
            book_reader::detect_layout(&db).expect("sniff"),
            want,
            "sniffed layout for {mode:?}"
        );
    }
}

// ---- 2. Depth --------------------------------------------------------------

#[test]
fn depth_reads_back_identically_under_every_mode() {
    for mode in [
        BookMode::Classic,
        BookMode::OrderRows,
        BookMode::LevelAuthority,
    ] {
        let (_dir, db) = open_test_db();
        let exp = seed(&db, mode);
        let layout = book_reader::detect_layout(&db).expect("detect");

        let d1 = book_reader::read_book_depth(&db, 1, layout).expect("market 1 depth");
        assert_eq!(depth_tuples(&d1.bids), exp.m1_bids, "{mode:?} m1 bids");
        assert_eq!(depth_tuples(&d1.asks), exp.m1_asks, "{mode:?} m1 asks");
        assert!(
            !d1.bids.is_empty(),
            "{mode:?}: depth must never be silently empty"
        );

        let d2 = book_reader::read_book_depth(&db, 2, layout).expect("market 2 depth");
        assert_eq!(depth_tuples(&d2.bids), exp.m2_bids, "{mode:?} m2 bids");
    }
}

#[test]
fn depth_of_an_unknown_market_is_empty_not_an_error() {
    for mode in [
        BookMode::Classic,
        BookMode::OrderRows,
        BookMode::LevelAuthority,
    ] {
        let (_dir, db) = open_test_db();
        seed(&db, mode);
        let layout = book_reader::detect_layout(&db).expect("detect");
        let d = book_reader::read_book_depth(&db, 9_999, layout).expect("absent market");
        assert!(d.bids.is_empty() && d.asks.is_empty());
    }
}

// ---- 3. Last trade price ---------------------------------------------------

#[test]
fn last_trade_price_reads_back_under_every_mode() {
    for mode in [
        BookMode::Classic,
        BookMode::OrderRows,
        BookMode::LevelAuthority,
    ] {
        let (_dir, db) = open_test_db();
        let exp = seed(&db, mode);
        let layout = book_reader::detect_layout(&db).expect("detect");
        assert_eq!(
            book_reader::read_last_trade_price(&db, 1, layout).expect("ltp"),
            exp.m1_ltp,
            "{mode:?} last trade price"
        );
        // Market 2 never traded.
        assert_eq!(
            book_reader::read_last_trade_price(&db, 2, layout).expect("ltp"),
            None,
            "{mode:?} untraded market"
        );
    }
}

// ---- 4. Open orders --------------------------------------------------------

#[test]
fn open_orders_read_back_under_every_mode() {
    let maker = addr(MAKER);
    for mode in [
        BookMode::Classic,
        BookMode::OrderRows,
        BookMode::LevelAuthority,
    ] {
        let (_dir, db) = open_test_db();
        let exp = seed(&db, mode);
        let layout = book_reader::detect_layout(&db).expect("detect");

        // All markets.
        let mut all: Vec<(MarketId, u128, FixedPoint, FixedPoint)> =
            book_reader::read_open_orders(&db, &maker, None, layout, 500)
                .expect("all-market open orders")
                .into_iter()
                .map(|(mid, o)| (mid, o.id, o.price, o.remaining_qty))
                .collect();
        all.sort_by_key(|(mid, id, _, _)| (*mid, *id));
        assert_eq!(all, exp.maker_orders, "{mode:?} open orders (all markets)");
        assert!(
            !all.is_empty(),
            "{mode:?}: open orders must never be silently empty"
        );

        // Single market.
        let one: Vec<u128> = book_reader::read_open_orders(&db, &maker, Some(1), layout, 500)
            .expect("single-market open orders")
            .into_iter()
            .map(|(_, o)| o.id)
            .collect();
        let want: Vec<u128> = exp
            .maker_orders
            .iter()
            .filter(|(mid, ..)| *mid == 1)
            .map(|(_, id, ..)| *id)
            .collect();
        assert_eq!(one, want, "{mode:?} open orders (market 1)");

        // A trader with no orders is legitimately empty.
        assert!(
            book_reader::read_open_orders(&db, &addr(200), None, layout, 500)
                .expect("no orders")
                .is_empty()
        );
    }
}

// ---- 5. Never silently empty ----------------------------------------------

#[test]
fn mode2_split_brain_order_store_is_an_error_not_an_empty_book() {
    let (_dir, db) = open_test_db();
    let exp = seed(&db, BookMode::LevelAuthority);
    assert!(!exp.maker_orders.is_empty());

    // Node-local order store wiped (stale snapshot / lost CF) while the root CF
    // still commits level rows: reading open orders MUST fail loudly.
    for (k, _) in StateBackend::iterate_cf(&db, CF_BOOK_ORDER_ROWS, None).unwrap() {
        db.delete_cf_raw(CF_BOOK_ORDER_ROWS, &k).unwrap();
    }
    let layout = book_reader::detect_layout(&db).expect("detect");
    let err = book_reader::read_open_orders(&db, &addr(MAKER), Some(1), layout, 500)
        .expect_err("split-brain store must be an error, not []");
    assert!(
        err.to_string().contains("cf_book_order_rows"),
        "error must name the node-local store: {err}"
    );

    // Depth still works: it is served from the ROOT-CF level rows.
    let d = book_reader::read_book_depth(&db, 1, layout).expect("level rows are authority");
    assert_eq!(depth_tuples(&d.bids), exp.m1_bids);
}

#[test]
fn mixed_layouts_are_an_error() {
    let (_dir, db) = open_test_db();
    seed(&db, BookMode::OrderRows);
    // A classic whole-book blob smuggled into a row-layout CF.
    db.put_cf_raw(CF_NATIVE_ORDER_BOOKS, &7u64.to_be_bytes(), b"blob")
        .unwrap();
    let layout = book_reader::detect_layout(&db).expect("marker still says order rows");
    let err = book_reader::read_book_depth(&db, 7, layout)
        .expect_err("classic blob under a row layout must be an error");
    assert!(!format!("{err}").is_empty());
}

#[test]
fn order_rows_without_a_meta_row_are_an_error() {
    let (_dir, db) = open_test_db();
    seed(&db, BookMode::OrderRows);
    // Drop market 1's meta row: the book is no longer reconstructible.
    db.delete_cf_raw(
        CF_NATIVE_ORDER_BOOKS,
        &torus_core::book_rows::book_meta_key(1),
    )
    .unwrap();
    let layout = book_reader::detect_layout(&db).expect("detect");
    let err = book_reader::read_book_depth(&db, 1, layout)
        .expect_err("orders without meta must be an error");
    assert!(format!("{err}").contains("meta"), "{err}");
}

#[test]
fn a_corrupt_book_mode_marker_is_an_error() {
    let (_dir, db) = open_test_db();
    seed(&db, BookMode::OrderRows);
    db.put_cf_raw(
        torus_state::cf::CF_NATIVE_MARKETS,
        book_reader::BOOK_MODE_MARKER_KEY,
        &[9u8],
    )
    .unwrap();
    assert!(book_reader::detect_layout(&db).is_err());
}

// ---- 6. Precompile 0x0800 --------------------------------------------------

#[test]
fn precompile_get_order_book_serves_row_modes() {
    for mode in [BookMode::OrderRows, BookMode::LevelAuthority] {
        let (_dir, db) = open_test_db();
        let exp = seed(&db, mode);
        let arrays = call_get_order_book(&db, 1).expect("precompile must decode row modes");
        let (bid_px, bid_qty, ask_px, ask_qty) = (&arrays[0], &arrays[1], &arrays[2], &arrays[3]);

        let want_bid_px: Vec<u128> = exp.m1_bids.iter().map(|(p, ..)| p.raw() as u128).collect();
        let want_bid_qty: Vec<u128> = exp.m1_bids.iter().map(|(_, q, _)| q.raw() as u128).collect();
        let want_ask_px: Vec<u128> = exp.m1_asks.iter().map(|(p, ..)| p.raw() as u128).collect();
        let want_ask_qty: Vec<u128> = exp.m1_asks.iter().map(|(_, q, _)| q.raw() as u128).collect();

        assert_eq!(bid_px, &want_bid_px, "{mode:?} bid prices");
        assert_eq!(bid_qty, &want_bid_qty, "{mode:?} bid quantities");
        assert_eq!(ask_px, &want_ask_px, "{mode:?} ask prices");
        assert_eq!(ask_qty, &want_ask_qty, "{mode:?} ask quantities");
        assert!(!bid_px.is_empty(), "{mode:?}: EVM must not get an empty book");
    }
}

#[test]
fn classic_precompile_behaviour_is_unchanged() {
    // PRE-EXISTING, CONSENSUS-VISIBLE: under Classic the 0x0800 reader decodes
    // ONLY the legacy `OrderBookSnapshot`, so a production whole-book blob
    // makes it REVERT (it has never returned an empty book here). Changing
    // that changes EVM-visible output for an already-deployed layout, so this
    // test PINS today's behaviour until that decision is made explicitly.
    let (_dir, db) = open_test_db();
    seed(&db, BookMode::Classic);
    let err = call_get_order_book(&db, 1)
        .expect_err("classic arm still rejects the production blob (unchanged)");
    assert!(err.contains("borsh"), "{err}");
}
