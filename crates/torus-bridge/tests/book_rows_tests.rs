//! C4 (perf/exec-scaleup): per-order-row book persistence (`TORUS_BOOK_ROWS`).
//!
//! Classic path: every touched market re-serializes its ENTIRE `OrderBook`
//! into one `CF_NATIVE_ORDER_BOOKS` blob per block — O(book depth) bytes
//! serialized and state-root-hashed per block. Row path: one KV row per
//! resting order / pending stop + a small per-market meta row; saves write
//! only changed rows.
//!
//! CONSENSUS-VISIBLE: the row layout changes the state-root format. The flag
//! must be fleet-uniform and enabling it requires a fresh genesis; load
//! fail-stops (ctx.fatal_error) on mode/content mismatch — tested below.
//!
//! Contracts under test:
//!   1. Differential: any op sequence (places incl. dust, partial fills,
//!      cancels, modifies incl. priority loss, STP, stops, multi-block with
//!      per-block reload) yields BYTE-IDENTICAL books after reload in either
//!      mode, and byte-identical non-book state throughout.
//!   2. State-root stability per path: same run twice per mode → identical
//!      CF contents and native root; roots differ ACROSS modes (that is the
//!      documented consensus visibility).
//!   3. FIFO queue priority survives the row roundtrip, including orders that
//!      lost time priority via modify (same id, re-stamped queue seq).
//!   4. Incremental writes: an untouched deep book costs zero row writes; a
//!      single new order costs a handful.
//!   5. Mixed on-disk content fail-stops in both directions.

use alloy_primitives::Address;

use torus_bridge::native_executor::{NativeExecContext, NativeExecutor};
use torus_bridge::state_root::compute_native_state_root;
use torus_core::position::NativeBalance;
use torus_state::cf::{
    CF_NATIVE_BALANCES, CF_NATIVE_ORDER_BOOKS, CF_NATIVE_POSITIONS, CF_NATIVE_TRADES,
    CF_NATIVE_USER_TRADES,
};
use torus_state::{StateBackend, StateDb};
use torus_types::{
    FixedPoint, MarketId, NativeAction, OrderType, PlaceOrderParams, TimeInForce,
};

// ---- Helpers (same idiom as parallel_settle_tests.rs) ----

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

/// Fresh per-block context, book persistence mode pinned explicitly.
fn make_ctx(state_db: StateDb, height: u64, rows: bool) -> NativeExecContext {
    NativeExecContext::new_with_book_rows(
        state_db,
        height,
        1000 + height, // timestamp
        0,             // epoch
        100,           // epoch_length
        10,            // max_validators
        addr(99),
        addr(100),
        addr(101),
        rows,
    )
}

fn fund_native(ctx: &NativeExecContext, trader: &Address, amount: FixedPoint) {
    let bal = NativeBalance {
        available: amount,
        order_margin: FixedPoint::ZERO,
    };
    ctx.positions.put_native_balance(trader, &bal).unwrap();
}

#[allow(clippy::too_many_arguments)]
fn order(
    market_id: MarketId,
    is_buy: bool,
    price: FixedPoint,
    qty: FixedPoint,
    order_type: OrderType,
    tif: TimeInForce,
) -> PlaceOrderParams {
    PlaceOrderParams {
        market_id,
        is_buy,
        price,
        quantity: qty,
        order_type,
        time_in_force: tif,
        reduce_only: false,
        client_order_id: None,
    }
}

fn gtc(market_id: MarketId, is_buy: bool, price: i64, qty: i64) -> PlaceOrderParams {
    order(
        market_id,
        is_buy,
        fp(price),
        fp(qty),
        OrderType::Limit,
        TimeInForce::GTC,
    )
}

fn place(sender: Address, p: PlaceOrderParams) -> (Address, NativeAction) {
    (sender, NativeAction::PlaceOrder(p))
}

/// Canonical comparator for in-memory book equality: the CLASSIC whole-book
/// Borsh bytes (the consensus format the row store must reproduce exactly).
fn book_bytes(ctx: &NativeExecContext) -> Vec<(MarketId, Vec<u8>)> {
    let mut out: Vec<(MarketId, Vec<u8>)> = ctx
        .order_books
        .iter()
        .map(|(mid, book)| (*mid, borsh::to_vec(book).expect("book borsh")))
        .collect();
    out.sort_by_key(|(mid, _)| *mid);
    out
}

fn dump_cf(ctx: &NativeExecContext, cf: &'static str) -> Vec<(Vec<u8>, Vec<u8>)> {
    ctx.state.iterate_cf(cf, None).expect("iterate cf")
}

/// Non-book state (must be byte-identical between modes at every block).
fn non_book_dump(ctx: &NativeExecContext) -> Vec<(&'static str, Vec<u8>, Vec<u8>)> {
    let mut out = Vec::new();
    for cf in [
        CF_NATIVE_BALANCES,
        CF_NATIVE_POSITIONS,
        CF_NATIVE_TRADES,
        CF_NATIVE_USER_TRADES,
    ] {
        for (k, v) in dump_cf(ctx, cf) {
            out.push((cf, k, v));
        }
    }
    out
}

/// The mixed multi-block adversarial script: places (incl. dusty quantities),
/// crossing/partial fills, cancels, modifies (both priority-keeping qty
/// decrease and priority-LOSING price change), STP cancels, and a stop order,
/// across 3 markets. Returns per-block action batches; `order_ids[i]` are
/// resolved dynamically by the runner via next_global_order_id snapshots.
fn run_script(db: StateDb, rows: bool) -> ScriptResult {
    let m1 = addr(1);
    let m2 = addr(2);
    let t1 = addr(3);
    let s1 = addr(4);

    let dusty = FixedPoint::from_raw(2 * FixedPoint::SCALE + 7); // 2.00000007

    let mut fingerprints = Vec::new();
    let mut books_after = Vec::new();
    let mut writes = Vec::new();

    // ---- Block 1: seed resting liquidity ----
    let mut ctx = make_ctx(db.clone(), 1, rows);
    assert!(ctx.fatal_error.is_none(), "load: {:?}", ctx.fatal_error);
    for t in [m1, m2, t1, s1] {
        fund_native(&ctx, &t, fp(1_000_000));
    }
    let id0 = ctx.next_global_order_id;
    let b1 = vec![
        place(m1, gtc(1, true, 100, 5)),                                     // id0
        place(m2, gtc(1, true, 100, 3)),                                     // id0+1 (same level, behind)
        place(m1, gtc(1, false, 105, 4)),                                    // id0+2
        place(m2, order(2, true, fp(50), dusty, OrderType::Limit, TimeInForce::GTC)), // id0+3 dusty
        place(m1, gtc(2, false, 55, 6)),                                     // id0+4
        place(
            s1,
            order(
                3,
                true,
                fp(10),
                fp(2),
                OrderType::StopMarket { trigger: fp(12) },
                TimeInForce::GTC,
            ),
        ), // pending stop on mkt 3
        place(m2, gtc(3, false, 11, 1)),                                     // resting ask mkt3
    ];
    let r = NativeExecutor::execute_batch(&mut ctx, &b1);
    assert!(
        r.results.iter().all(|x| x.success),
        "block1: {:?}",
        r.results
            .iter()
            .filter(|x| !x.success)
            .collect::<Vec<_>>()
    );
    writes.push(ctx.save_order_books());
    fingerprints.push(non_book_dump(&ctx));
    books_after.push(book_bytes(&ctx));

    // ---- Block 2: partial fills + modify (both kinds) + STP ----
    let mut ctx = make_ctx(db.clone(), 2, rows);
    assert!(ctx.fatal_error.is_none(), "load2: {:?}", ctx.fatal_error);
    // NOTE execution order: Phase 1 runs the non-PlaceOrder actions (the two
    // modifies) BEFORE any placement/matching, so:
    //   - m2's dusty mkt2 bid shrinks to qty 1 (in-place decrease, keeps
    //     priority and its persisted queue seq);
    //   - m1's mkt1 bid moves 100→99 (cancel+reinsert, SAME id — its
    //     persisted row must be rewritten even though remaining_qty did not
    //     change, and queue priority is re-derived);
    // then t1's sell 2 @ 100 fills against what still rests at 100 (m2's 3),
    // and s1 self-crosses at 54 on mkt2 → STP cancels s1's resting bid.
    let b2 = vec![
        place(t1, gtc(1, false, 100, 2)),
        (
            m2,
            NativeAction::ModifyOrder {
                order_id: id0 + 3,
                new_price: None,
                new_qty: Some(fp(1)),
            },
        ),
        (
            m1,
            NativeAction::ModifyOrder {
                order_id: id0,
                new_price: Some(fp(99)),
                new_qty: None,
            },
        ),
        place(s1, gtc(2, true, 54, 1)),
        place(s1, gtc(2, false, 54, 1)),
    ];
    let r = NativeExecutor::execute_batch(&mut ctx, &b2);
    assert!(
        r.results.iter().all(|x| x.success),
        "block2: {:?}",
        r.results
            .iter()
            .filter(|x| !x.success)
            .collect::<Vec<_>>()
    );
    writes.push(ctx.save_order_books());
    fingerprints.push(non_book_dump(&ctx));
    books_after.push(book_bytes(&ctx));

    // ---- Block 3: cancels + deeper fills + trigger the stop ----
    let mut ctx = make_ctx(db.clone(), 3, rows);
    assert!(ctx.fatal_error.is_none(), "load3: {:?}", ctx.fatal_error);
    // NOTE Phase 1 runs both cancels first: m2's modified dusty order goes,
    // and m1's cancel-all clears its mkt1 bid + mkt1 ask + mkt2 ask (row
    // deletes across two books). The places then run against the emptied
    // books: t1's buy 4 @ 105 finds no asks and RESTS; t1's buy 1 @ 11 on
    // mkt3 fills m2's ask and sets last_trade_price = 11 — below the stop
    // trigger (12), so the stop stays pending (stop-row persistence).
    let b3 = vec![
        (m2, NativeAction::CancelOrder { order_id: id0 + 3 }),
        place(t1, gtc(1, true, 105, 4)),
        place(t1, gtc(3, true, 11, 1)),
        (m1, NativeAction::CancelAllOrders { market_id: None }),
    ];
    let r = NativeExecutor::execute_batch(&mut ctx, &b3);
    assert!(
        r.results.iter().all(|x| x.success),
        "block3: {:?}",
        r.results
            .iter()
            .filter(|x| !x.success)
            .collect::<Vec<_>>()
    );
    writes.push(ctx.save_order_books());
    fingerprints.push(non_book_dump(&ctx));
    books_after.push(book_bytes(&ctx));

    // ---- Reload (block 4, no ops): the persisted form round-trips ----
    let mut ctx = make_ctx(db.clone(), 4, rows);
    assert!(ctx.fatal_error.is_none(), "reload: {:?}", ctx.fatal_error);
    let reloaded_books = book_bytes(&ctx);
    let idle_writes = ctx.save_order_books();

    ScriptResult {
        fingerprints,
        books_after,
        writes,
        reloaded_books,
        idle_writes,
        book_cf: dump_cf(&ctx, CF_NATIVE_ORDER_BOOKS),
        root: compute_native_state_root(&ctx.state).expect("root"),
    }
}

struct ScriptResult {
    /// Non-book CF dumps after each block.
    fingerprints: Vec<Vec<(&'static str, Vec<u8>, Vec<u8>)>>,
    /// Classic Borsh bytes of every in-memory book after each block.
    books_after: Vec<Vec<(MarketId, Vec<u8>)>>,
    /// save_order_books() write counts per block.
    writes: Vec<usize>,
    /// Books after a fresh reload from persistence.
    reloaded_books: Vec<(MarketId, Vec<u8>)>,
    /// Writes issued by a save with NO ops after reload (must be 0).
    idle_writes: usize,
    /// Raw CF_NATIVE_ORDER_BOOKS contents at the end.
    book_cf: Vec<(Vec<u8>, Vec<u8>)>,
    /// Native state root at the end.
    root: alloy_primitives::B256,
}

// ============================================================================
// 1 + 2 + 3. Differential + roundtrip + stability
// ============================================================================

#[test]
fn book_rows_differential_identical_books_and_state() {
    let (_d1, db_classic) = open_test_db();
    let (_d2, db_rows) = open_test_db();

    let classic = run_script(db_classic, false);
    let rows = run_script(db_rows, true);

    // Non-book state: byte-identical after every block. Book persistence must
    // not leak into balances / positions / trade history in any way.
    for (i, (c, r)) in classic
        .fingerprints
        .iter()
        .zip(rows.fingerprints.iter())
        .enumerate()
    {
        assert_eq!(c, r, "non-book state diverged after block {}", i + 1);
    }

    // In-memory books: byte-identical (classic Borsh comparator) after every
    // block — the row store reproduces book behavior exactly.
    for (i, (c, r)) in classic
        .books_after
        .iter()
        .zip(rows.books_after.iter())
        .enumerate()
    {
        assert_eq!(c, r, "in-memory books diverged after block {}", i + 1);
    }

    // Roundtrip: reloading from rows reproduces the same books the classic
    // path reloads — including FIFO priority (block 2 made order id0 lose
    // time priority via a price modify; its row was re-stamped with a fresh
    // queue seq) and the pending stop on market 3.
    assert_eq!(
        classic.reloaded_books, rows.reloaded_books,
        "reloaded books diverged between persistence modes"
    );
    assert_eq!(
        classic.books_after.last().unwrap(),
        &classic.reloaded_books,
        "classic roundtrip lost state"
    );
    assert_eq!(
        rows.books_after.last().unwrap(),
        &rows.reloaded_books,
        "row roundtrip lost state"
    );

    // A no-op block writes nothing in either mode.
    assert_eq!(classic.idle_writes, 0, "classic idle save wrote rows");
    assert_eq!(rows.idle_writes, 0, "row idle save wrote rows");

    // Consensus visibility (the documented fork line): the two modes store
    // DIFFERENT bytes in CF_NATIVE_ORDER_BOOKS, so their state roots differ.
    assert_ne!(
        classic.book_cf, rows.book_cf,
        "modes unexpectedly stored identical book CF contents"
    );
    assert_ne!(
        classic.root, rows.root,
        "state roots must differ across modes (consensus-visible flag)"
    );
}

/// Same script, same mode, twice → identical CF bytes and root (per-path
/// determinism, incl. the row store's queue-seq assignment).
#[test]
fn book_rows_state_root_stable_per_path() {
    for rows in [false, true] {
        let (_d1, db_a) = open_test_db();
        let (_d2, db_b) = open_test_db();
        let a = run_script(db_a, rows);
        let b = run_script(db_b, rows);
        assert_eq!(
            a.book_cf, b.book_cf,
            "rows={rows}: book CF contents not reproducible"
        );
        assert_eq!(a.root, b.root, "rows={rows}: state root not reproducible");
        assert_eq!(a.writes, b.writes, "rows={rows}: write counts not reproducible");
    }
}

// ============================================================================
// 4. Incremental writes: row saves are O(changed), not O(book)
// ============================================================================

#[test]
fn book_rows_save_is_incremental() {
    let (_dir, db) = open_test_db();

    // Block 1: build a deep book — 200 resting orders (qty 2 each, so a later
    // taker can PARTIALLY fill one) across 40 bid levels on market 1.
    let mut ctx = make_ctx(db.clone(), 1, true);
    fund_native(&ctx, &addr(1), fp(100_000_000));
    let batch: Vec<(Address, NativeAction)> = (0..200)
        .map(|i| place(addr(1), gtc(1, true, 50 + (i % 40) as i64, 2)))
        .collect();
    let r = NativeExecutor::execute_batch(&mut ctx, &batch);
    assert!(r.results.iter().all(|x| x.success));
    let w1 = ctx.save_order_books();
    // 200 order rows + 1 meta row.
    assert_eq!(w1, 201, "initial save writes every new row once");

    // Block 2: ONE new resting order on the same book.
    let mut ctx = make_ctx(db.clone(), 2, true);
    fund_native(&ctx, &addr(2), fp(1_000_000));
    let r = NativeExecutor::execute_batch(&mut ctx, &[place(addr(2), gtc(1, true, 49, 1))]);
    assert!(r.results.iter().all(|x| x.success));
    let w2 = ctx.save_order_books();
    // 1 new order row + 1 meta row (next_seq/next_id moved). The other 200
    // resting orders cost NOTHING — the classic path would have re-serialized
    // and re-hashed all of them.
    assert_eq!(w2, 2, "row save must write only the delta");

    // Block 3: one taker partially fills the front order of the best level
    // (sell 1 into the 89 bid whose front order has qty 2 → 1 remains).
    let mut ctx = make_ctx(db.clone(), 3, true);
    fund_native(&ctx, &addr(3), fp(1_000_000));
    let r = NativeExecutor::execute_batch(
        &mut ctx,
        &[place(
            addr(3),
            order(1, false, fp(89), fp(1), OrderType::Limit, TimeInForce::IOC),
        )],
    );
    assert!(r.results.iter().all(|x| x.success));
    let w3 = ctx.save_order_books();
    // 1 refilled order row (remaining_qty moved) + 1 meta row (next_id / ltp
    // moved). The other 200 resting rows cost nothing.
    assert!(w3 <= 3, "partial fill must touch O(1) rows, wrote {w3}");
}

// ============================================================================
// 5. Mode/content mismatch fail-stops
// ============================================================================

#[test]
fn book_rows_mixed_content_is_fatal_both_directions() {
    // Classic content, rows-mode load → fatal.
    {
        let (_dir, db) = open_test_db();
        let mut ctx = make_ctx(db.clone(), 1, false);
        fund_native(&ctx, &addr(1), fp(1_000_000));
        let r = NativeExecutor::execute_batch(&mut ctx, &[place(addr(1), gtc(1, true, 100, 1))]);
        assert!(r.results[0].success);
        ctx.save_order_books();

        let ctx = make_ctx(db, 2, true);
        let err = ctx.fatal_error.as_deref().unwrap_or("");
        assert!(
            err.contains("TORUS_BOOK_ROWS") && err.contains("fresh genesis"),
            "rows-mode load of classic content must fail-stop, got: {err:?}"
        );
    }

    // Row content, classic-mode load → fatal (NOT silent empty books).
    {
        let (_dir, db) = open_test_db();
        let mut ctx = make_ctx(db.clone(), 1, true);
        fund_native(&ctx, &addr(1), fp(1_000_000));
        let r = NativeExecutor::execute_batch(&mut ctx, &[place(addr(1), gtc(1, true, 100, 1))]);
        assert!(r.results[0].success);
        ctx.save_order_books();

        let ctx = make_ctx(db, 2, false);
        let err = ctx.fatal_error.as_deref().unwrap_or("");
        assert!(
            err.contains("TORUS_BOOK_ROWS"),
            "classic-mode load of row content must fail-stop, got: {err:?}"
        );
    }
}
