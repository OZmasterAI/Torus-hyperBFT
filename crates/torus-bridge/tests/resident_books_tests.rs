//! rank8 (perf/exec-scaleup): resident order books (`TORUS_RESIDENT_BOOKS`).
//!
//! Books (+ row shadows + the order-id high-water mark) survive across blocks
//! in a `ResidentBooks` holder instead of being rebuilt from
//! `CF_NATIVE_ORDER_BOOKS` every block; under rows mode, saves are driven by
//! the books' mutation journal instead of a whole-book walk.
//!
//! THE contract: resident mode is NOT consensus-visible. For any block
//! sequence — including process restarts mid-sequence and stale-holder
//! fallbacks — the CF writes and native state root must be byte-identical to
//! the per-block reload path in the same persistence mode. The journal-driven
//! differ must assign queue seqs exactly like the full walk (write-count
//! equality is asserted as a structural witness).
//!
//! Script: the same multi-block adversarial sequence as book_rows_tests.rs
//! (dusty quantities, partial fills, in-place and priority-losing modifies,
//! STP cancels, multi-book cancel-all, pending stops).

use alloy_primitives::Address;

use torus_bridge::native_executor::{NativeExecContext, NativeExecutor, ResidentBooks};
use torus_bridge::state_root::compute_native_state_root;
use torus_core::position::NativeBalance;
use torus_state::cf::{
    CF_CONSENSUS_META, CF_NATIVE_BALANCES, CF_NATIVE_ORDER_BOOKS, CF_NATIVE_POSITIONS,
    CF_NATIVE_TRADES, CF_NATIVE_USER_TRADES, META_NATIVE_APPLIED_HEIGHT,
};
use torus_state::{StateBackend, StateDb};
use torus_types::{
    FixedPoint, MarketId, NativeAction, OrderType, PlaceOrderParams, TimeInForce,
};

// ---- Helpers ----

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

fn make_ctx(
    db: StateDb,
    height: u64,
    rows: bool,
    resident: Option<&mut ResidentBooks>,
) -> NativeExecContext {
    NativeExecContext::new_with_modes(
        db,
        height,
        1000 + height,
        0,
        100,
        10,
        addr(99),
        addr(100),
        addr(101),
        rows,
        resident,
    )
}

fn fund_native(ctx: &NativeExecContext, trader: &Address, amount: FixedPoint) {
    let bal = NativeBalance {
        available: amount,
        order_margin: FixedPoint::ZERO,
    };
    ctx.positions.put_native_balance(trader, &bal).unwrap();
}

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

fn book_bytes(ctx: &NativeExecContext) -> Vec<(MarketId, Vec<u8>)> {
    let mut out: Vec<(MarketId, Vec<u8>)> = ctx
        .order_books
        .iter()
        .map(|(mid, book)| (*mid, borsh::to_vec(book).expect("book borsh")))
        .collect();
    out.sort_by_key(|(mid, _)| *mid);
    out
}

fn dump_cf(db: &StateDb, cf: &'static str) -> Vec<(Vec<u8>, Vec<u8>)> {
    StateBackend::iterate_cf(db, cf, None).expect("iterate cf")
}

fn non_book_dump(db: &StateDb) -> Vec<(&'static str, Vec<u8>, Vec<u8>)> {
    let mut out = Vec::new();
    for cf in [
        CF_NATIVE_BALANCES,
        CF_NATIVE_POSITIONS,
        CF_NATIVE_TRADES,
        CF_NATIVE_USER_TRADES,
    ] {
        for (k, v) in dump_cf(db, cf) {
            out.push((cf, k, v));
        }
    }
    out
}

/// The adversarial per-block batches (see book_rows_tests.rs for the play-by-
/// play). Returns 3 batches; `id0` is the first global order id.
fn script_batches(id0: u128) -> Vec<Vec<(Address, NativeAction)>> {
    let m1 = addr(1);
    let m2 = addr(2);
    let t1 = addr(3);
    let s1 = addr(4);
    let dusty = FixedPoint::from_raw(2 * FixedPoint::SCALE + 7);

    let b1 = vec![
        place(m1, gtc(1, true, 100, 5)),
        place(m2, gtc(1, true, 100, 3)),
        place(m1, gtc(1, false, 105, 4)),
        place(m2, order(2, true, fp(50), dusty, OrderType::Limit, TimeInForce::GTC)),
        place(m1, gtc(2, false, 55, 6)),
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
        ),
        place(m2, gtc(3, false, 11, 1)),
    ];
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
    let b3 = vec![
        (m2, NativeAction::CancelOrder { order_id: id0 + 3 }),
        place(t1, gtc(1, true, 105, 4)),
        place(t1, gtc(3, true, 11, 1)),
        (m1, NativeAction::CancelAllOrders { market_id: None }),
    ];
    vec![b1, b2, b3]
}

struct RunResult {
    /// Non-book CF dumps after each block.
    fingerprints: Vec<Vec<(&'static str, Vec<u8>, Vec<u8>)>>,
    /// Classic Borsh bytes of every in-memory book after each block.
    books_after: Vec<Vec<(MarketId, Vec<u8>)>>,
    /// save_order_books() write counts per block.
    writes: Vec<usize>,
    /// Books after a final fresh (non-resident) reload.
    reloaded_books: Vec<(MarketId, Vec<u8>)>,
    book_cf: Vec<(Vec<u8>, Vec<u8>)>,
    root: alloy_primitives::B256,
}

/// Run the script. `restart_before`: block heights before which the resident
/// holder is dropped (simulated process restart). Non-resident runs ignore it.
fn run_script(db: StateDb, rows: bool, resident: bool, restart_before: &[u64]) -> RunResult {
    let mut holder = ResidentBooks::default();
    let mut fingerprints = Vec::new();
    let mut books_after = Vec::new();
    let mut writes = Vec::new();

    // id0 must be probed from a throwaway context (fresh DB ⇒ 1).
    let batches = script_batches(1);

    for (i, batch) in batches.iter().enumerate() {
        let height = (i + 1) as u64;
        if resident && restart_before.contains(&height) {
            holder = ResidentBooks::default(); // process restart: memory gone
        }
        let expected_reuse = resident && height > 1 && !restart_before.contains(&height);
        let mut ctx = make_ctx(
            db.clone(),
            height,
            rows,
            if resident { Some(&mut holder) } else { None },
        );
        assert!(ctx.fatal_error.is_none(), "load {height}: {:?}", ctx.fatal_error);
        assert_eq!(
            ctx.resident_reused(),
            expected_reuse,
            "block {height}: resident reuse mismatch"
        );
        if height == 1 {
            for t in [addr(1), addr(2), addr(3), addr(4)] {
                fund_native(&ctx, &t, fp(1_000_000));
            }
        }
        let r = NativeExecutor::execute_batch(&mut ctx, batch);
        assert!(
            r.results.iter().all(|x| x.success),
            "block {height}: {:?}",
            r.results.iter().filter(|x| !x.success).collect::<Vec<_>>()
        );
        writes.push(ctx.save_order_books());
        books_after.push(book_bytes(&ctx));
        ctx.stash_resident(&mut holder);
        fingerprints.push(non_book_dump(&db));
        if resident {
            assert!(holder.is_populated(), "block {height}: stash left holder empty");
        }
    }

    // Final reload through a fresh, NON-resident context: what a restarted /
    // non-resident node would see.
    let ctx = make_ctx(db.clone(), 4, rows, None);
    assert!(ctx.fatal_error.is_none(), "final reload: {:?}", ctx.fatal_error);
    RunResult {
        fingerprints,
        books_after,
        writes,
        reloaded_books: book_bytes(&ctx),
        book_cf: dump_cf(&db, CF_NATIVE_ORDER_BOOKS),
        root: compute_native_state_root(&db).expect("root"),
    }
}

// ============================================================================
// 1. Four-combo matrix: resident is invisible in persisted state
// ============================================================================

#[test]
fn resident_matrix_state_identical() {
    let mut results = Vec::new();
    for (rows, resident) in [(false, false), (false, true), (true, false), (true, true)] {
        let (_dir, db) = open_test_db();
        results.push(((rows, resident), run_script(db, rows, resident, &[])));
    }

    let get = |rows: bool, resident: bool| {
        &results
            .iter()
            .find(|((r, rs), _)| *r == rows && *rs == resident)
            .unwrap()
            .1
    };

    for rows in [false, true] {
        let off = get(rows, false);
        let on = get(rows, true);
        // Everything persisted must be byte-identical: resident mode is a
        // node-local optimization, NOT a consensus change.
        assert_eq!(off.book_cf, on.book_cf, "rows={rows}: book CF diverged");
        assert_eq!(off.root, on.root, "rows={rows}: state root diverged");
        assert_eq!(
            off.fingerprints, on.fingerprints,
            "rows={rows}: non-book state diverged"
        );
        assert_eq!(
            off.books_after, on.books_after,
            "rows={rows}: in-memory books diverged"
        );
        assert_eq!(
            off.reloaded_books, on.reloaded_books,
            "rows={rows}: reloaded books diverged"
        );
        // Write-count equality: under rows mode this is the structural
        // witness that the journal-driven differ writes EXACTLY the rows the
        // full walk writes (same puts + deletes, block by block).
        assert_eq!(off.writes, on.writes, "rows={rows}: write counts diverged");
    }

    // And the two persistence modes still agree on everything non-book.
    assert_eq!(
        get(false, true).fingerprints,
        get(true, true).fingerprints,
        "persistence mode leaked into non-book state under resident mode"
    );
}

// ============================================================================
// 2. Restart mid-sequence: rebuild-from-persisted == never-restarted
// ============================================================================

#[test]
fn resident_restart_mid_sequence_identical() {
    for rows in [false, true] {
        let (_d1, db_base) = open_test_db();
        let base = run_script(db_base, rows, true, &[]);

        for restart in [&[2u64][..], &[3u64][..], &[2, 3][..]] {
            let (_d2, db) = open_test_db();
            let restarted = run_script(db, rows, true, restart);
            assert_eq!(
                base.root, restarted.root,
                "rows={rows} restart_before={restart:?}: root diverged"
            );
            assert_eq!(
                base.book_cf, restarted.book_cf,
                "rows={rows} restart_before={restart:?}: book CF diverged"
            );
            assert_eq!(
                base.books_after, restarted.books_after,
                "rows={rows} restart_before={restart:?}: in-memory books diverged"
            );
            assert_eq!(
                base.writes, restarted.writes,
                "rows={rows} restart_before={restart:?}: write counts diverged"
            );
        }
    }
}

// ============================================================================
// 3. Staleness guard: height gaps and marker mismatches force a rebuild
// ============================================================================

#[test]
fn resident_stale_holder_rebuilds_from_db() {
    let (_dir, db) = open_test_db();
    let mut holder = ResidentBooks::default();

    // Block 1 (resident): rest one order, save, stash.
    let mut ctx = make_ctx(db.clone(), 1, true, Some(&mut holder));
    fund_native(&ctx, &addr(1), fp(1_000_000));
    fund_native(&ctx, &addr(2), fp(1_000_000));
    let r = NativeExecutor::execute_batch(&mut ctx, &[place(addr(1), gtc(1, true, 100, 5))]);
    assert!(r.results[0].success);
    ctx.save_order_books();
    ctx.stash_resident(&mut holder);
    assert!(holder.is_populated());

    // Block 2 executes WITHOUT the holder (out-of-band DB progress).
    let mut ctx = make_ctx(db.clone(), 2, true, None);
    let r = NativeExecutor::execute_batch(&mut ctx, &[place(addr(2), gtc(1, true, 101, 2))]);
    assert!(r.results[0].success);
    ctx.save_order_books();

    // Block 3 (resident): holder says height 1, block is 3 → guard trips,
    // books rebuild from the DB and must contain block 2's order.
    let ctx = make_ctx(db.clone(), 3, true, Some(&mut holder));
    assert!(!ctx.resident_reused(), "stale holder must not be reused");
    assert!(!holder.is_populated(), "stale holder must be drained");
    let book = ctx.order_books.get(&1).expect("book rebuilt from DB");
    assert_eq!(book.order_count(), 2, "rebuild must include block 2's order");
}

#[test]
fn resident_marker_mismatch_rebuilds() {
    let (_dir, db) = open_test_db();
    let mut holder = ResidentBooks::default();

    let mut ctx = make_ctx(db.clone(), 1, true, Some(&mut holder));
    fund_native(&ctx, &addr(1), fp(1_000_000));
    let r = NativeExecutor::execute_batch(&mut ctx, &[place(addr(1), gtc(1, true, 100, 5))]);
    assert!(r.results[0].success);
    ctx.save_order_books();
    ctx.stash_resident(&mut holder);

    // Marker agrees with the holder → reuse.
    db.put_cf_raw(CF_CONSENSUS_META, META_NATIVE_APPLIED_HEIGHT, &1u64.to_be_bytes())
        .unwrap();
    let mut ctx = make_ctx(db.clone(), 2, true, Some(&mut holder));
    assert!(ctx.resident_reused(), "marker == holder height must reuse");
    ctx.save_order_books();
    ctx.stash_resident(&mut holder);

    // Marker says a DIFFERENT height than the holder (simulated failed flush
    // or out-of-band replay) → guard trips even though heights are sequential.
    db.put_cf_raw(CF_CONSENSUS_META, META_NATIVE_APPLIED_HEIGHT, &7u64.to_be_bytes())
        .unwrap();
    let ctx = make_ctx(db.clone(), 3, true, Some(&mut holder));
    assert!(
        !ctx.resident_reused(),
        "marker mismatch must force a rebuild from the DB"
    );
}

#[test]
fn fatal_context_invalidates_holder_on_stash() {
    let (_dir, db) = open_test_db();
    let mut holder = ResidentBooks::default();

    let mut ctx = make_ctx(db.clone(), 1, true, Some(&mut holder));
    fund_native(&ctx, &addr(1), fp(1_000_000));
    let r = NativeExecutor::execute_batch(&mut ctx, &[place(addr(1), gtc(1, true, 100, 5))]);
    assert!(r.results[0].success);
    ctx.save_order_books();
    ctx.fatal_error = Some("simulated worker panic".to_string());
    ctx.stash_resident(&mut holder);
    assert!(
        !holder.is_populated(),
        "a fatal context must invalidate, never stash"
    );
}

// ============================================================================
// 4. Journal-driven saves stay O(changed) on a deep resident book
// ============================================================================

#[test]
fn resident_rows_save_is_incremental_via_journal() {
    let (_dir, db) = open_test_db();
    let mut holder = ResidentBooks::default();

    // Block 1: 200 resting orders (qty 2) across 40 levels.
    let mut ctx = make_ctx(db.clone(), 1, true, Some(&mut holder));
    fund_native(&ctx, &addr(1), fp(100_000_000));
    let batch: Vec<(Address, NativeAction)> = (0..200)
        .map(|i| place(addr(1), gtc(1, true, 50 + (i % 40) as i64, 2)))
        .collect();
    let r = NativeExecutor::execute_batch(&mut ctx, &batch);
    assert!(r.results.iter().all(|x| x.success));
    assert_eq!(ctx.save_order_books(), 201, "200 order rows + meta");
    ctx.stash_resident(&mut holder);

    // Block 2: ONE new order — resident install, journal-driven diff.
    let mut ctx = make_ctx(db.clone(), 2, true, Some(&mut holder));
    assert!(ctx.resident_reused());
    fund_native(&ctx, &addr(2), fp(1_000_000));
    let r = NativeExecutor::execute_batch(&mut ctx, &[place(addr(2), gtc(1, true, 49, 1))]);
    assert!(r.results.iter().all(|x| x.success));
    assert_eq!(
        ctx.save_order_books(),
        2,
        "journal-driven save must write only the delta (1 row + meta)"
    );
    ctx.stash_resident(&mut holder);

    // Block 3: one partial fill at the best level front order.
    let mut ctx = make_ctx(db.clone(), 3, true, Some(&mut holder));
    assert!(ctx.resident_reused());
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
    assert!(w3 <= 3, "partial fill must touch O(1) rows, wrote {w3}");
    ctx.stash_resident(&mut holder);

    // The persisted result must equal a from-scratch reload's view.
    let ctx = make_ctx(db.clone(), 4, true, None);
    assert!(ctx.fatal_error.is_none());
    assert_eq!(
        ctx.order_books.get(&1).unwrap().order_count(),
        201,
        "200 seeded + 1 new (front of 89 partially filled, still resting)"
    );
}
