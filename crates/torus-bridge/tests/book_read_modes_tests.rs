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

use alloy_primitives::Address;

use torus_bridge::native_executor::{BookMode, NativeExecContext, NativeExecutor};
use torus_core::book_reader::{self, BookLayout};
use torus_core::position::NativeBalance;
use torus_state::cf::{CF_BOOK_ORDER_ROWS, CF_NATIVE_ORDER_BOOKS};
use torus_state::{StateBackend, StateDb};
use torus_types::{FixedPoint, MarketId, NativeAction, OrderType, PlaceOrderParams, TimeInForce};

// ---- Helpers ---------------------------------------------------------------

fn open_test_db() -> (tempfile::TempDir, StateDb) {
    let dir = tempfile::tempdir().expect("create temp dir");
    let db = StateDb::open(dir.path()).expect("open db");
    // Pre-testnet P0: orders need a listed market (no implicit books).
    // Placeholder rows: tick / lot 1 and no margin config, as the old
    // implicit book had.
    for m in 1..=64u64 {
        db.put_cf_raw(torus_state::cf::CF_NATIVE_MARKETS, &m.to_be_bytes(), b"listed")
            .expect("list market");
    }
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

    let b1 = ctx.order_books().get(&1).expect("market 1 book");
    let b2 = ctx.order_books().get(&2).expect("market 2 book");
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

/// Owner s100: the writers guarantee the book layout (the RPC readers reject
/// any other, see `mixed_layouts_are_an_error`). Every row-mode save, serial
/// or deferred (modes 2 / 3), across a reload, a market emptied
/// by a fill and a market first saved by the deferred path, leaves each market in cf_native_order_books with (1)
/// order rows or level rows, never both, and only the kind of its mode, and
/// (2) a meta row whenever it has any order, level or stop row. Any other
/// key fails the test.
#[test]
fn row_mode_writers_keep_one_row_kind_and_a_meta_row_per_market() {
    use torus_core::book_rows::{ROW_TAG_LEVEL, ROW_TAG_META, ROW_TAG_ORDER, ROW_TAG_STOP};
    for mode in [
        BookMode::OrderRows,
        BookMode::LevelAuthority,
        BookMode::LevelAuthorityChunked,
    ] {
        let (_dir, db) = open_test_db();
        seed(&db, mode);
        // Block 2 on a reloaded context: TAKER fills all of market 2 (its
        // order and level rows are deleted), MAKER adds a bid on market 1 and
        // opens market 3 (its first save is the deferred one in modes 2 / 3).
        let mut ctx = make_ctx(db.clone(), 2, mode);
        assert!(
            ctx.fatal_error.is_none(),
            "{mode:?} reload: {:?}",
            ctx.fatal_error
        );
        let block = vec![
            place(addr(TAKER), gtc(2, false, 50, 3)),
            place(addr(MAKER), gtc(1, true, 98, 1)),
            place(addr(MAKER), gtc(3, false, 70, 2)),
        ];
        let r = NativeExecutor::execute_batch(&mut ctx, &block);
        assert!(
            r.results.iter().all(|x| x.success),
            "{mode:?}: block 2 failed"
        );
        match ctx.save_order_books_deferred() {
            Some(save) => {
                torus_bridge::native_executor::apply_deferred_book_save(&db, None, save)
                    .expect("deferred book save");
            }
            None => {
                ctx.save_order_books();
            }
        }
        assert!(ctx
            .order_books()
            .get(&2)
            .expect("market 2")
            .bid_depth()
            .is_empty());

        let mut markets: std::collections::BTreeMap<Vec<u8>, (bool, bool, bool, bool)> =
            Default::default();
        for (key, _) in db.iterate_cf(CF_NATIVE_ORDER_BOOKS, None).unwrap() {
            let m = markets.entry(key[..8].to_vec()).or_default();
            match key.get(8).copied() {
                Some(ROW_TAG_META) => m.0 = true,
                Some(ROW_TAG_ORDER) => m.1 = true,
                Some(ROW_TAG_LEVEL) => m.2 = true,
                Some(ROW_TAG_STOP) => m.3 = true,
                _ => panic!("{mode:?}: unknown book key {key:02x?}"),
            }
        }
        assert_eq!(markets.len(), 3, "{mode:?}: {markets:?}");
        for (market, (meta, orders, levels, stops)) in markets {
            let rows = orders || levels || stops;
            assert!(
                meta || !rows,
                "{mode:?} market {market:?}: rows without a meta row"
            );
            assert!(
                !(orders && levels),
                "{mode:?} market {market:?}: order and level rows"
            );
            let order_mode = mode == BookMode::OrderRows;
            assert!(!levels || !order_mode, "{mode:?}: level rows in mode 1");
            assert!(!orders || order_mode, "{mode:?}: order rows in the root CF");
        }
    }
}
