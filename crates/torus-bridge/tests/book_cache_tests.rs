//! Cross-block book-cache equivalence (deep-book round, load-side fix).
//!
//! Per-order rows made SAVE O(touched), but the executor still rebuilt every
//! book from rows at every block (NativeExecContext::new). The cache carries
//! the books (+ global order-id counter) from block N to block N+1 in memory;
//! the CF remains the durable authority (restart reloads from rows).
//!
//! The invariant proven here: a run that RESUMES from the cache every block
//! is byte-identical — in final CF content AND in behavior — to a run that
//! reloads from disk every block.

use alloy_primitives::Address;
use torus_bridge::native_executor::{NativeExecContext, NativeExecutor};
use torus_state::cf::CF_NATIVE_ORDER_BOOKS;
use torus_state::{StateBackend, StateDb};
use torus_types::{FixedPoint, NativeAction, OrderType, PlaceOrderParams, TimeInForce};

fn open_test_db() -> (tempfile::TempDir, StateDb) {
    let dir = tempfile::tempdir().expect("create temp dir");
    let db = StateDb::open(dir.path()).expect("open db");
    (dir, db)
}

fn addr(n: u8) -> Address {
    Address::new([n; 20])
}

fn fp(n: i64) -> FixedPoint {
    FixedPoint::from_raw(n as i128 * FixedPoint::SCALE)
}

fn make_ctx(state: StateDb, height: u64) -> NativeExecContext {
    NativeExecContext::new(
        state,
        height,
        1000 + height,
        0,
        100,
        10,
        addr(99),
        addr(100),
        addr(101),
    )
}

fn fund(ctx: &NativeExecContext, trader: &Address) {
    use torus_core::position::NativeBalance;
    ctx.positions
        .put_native_balance(
            trader,
            &NativeBalance {
                available: fp(100_000_000),
                order_margin: FixedPoint::ZERO,
            },
        )
        .unwrap();
}

fn place(market_id: u64, is_buy: bool, price: i64, qty: i64) -> NativeAction {
    NativeAction::PlaceOrder(PlaceOrderParams {
        market_id,
        is_buy,
        price: fp(price),
        quantity: fp(qty),
        order_type: OrderType::Limit,
        time_in_force: TimeInForce::GTC,
        reduce_only: false,
        client_order_id: None,
    })
}

/// Per-"block" action scripts: depth build, then touches (fill/cancel/modify),
/// then a sweep — the journal must stay correct across cache handoffs.
fn block_actions(block: u64) -> Vec<(Address, NativeAction)> {
    match block {
        1 => {
            let mut v = Vec::new();
            for i in 0..30i64 {
                v.push((addr((i % 5) as u8 + 1), place(1, true, 500 - i, 1 + i % 3)));
                v.push((addr((i % 5) as u8 + 6), place(1, false, 600 + i, 1 + i % 3)));
            }
            v.push((addr(11), place(2, true, 50, 7)));
            v
        }
        2 => vec![
            (addr(12), place(1, true, 600, 2)), // partial fill at best ask
            // id 3 = the second bid placed (i=1) => owner addr(2).
            (addr(2), NativeAction::CancelOrder { order_id: 3 }),
            (
                addr(2),
                NativeAction::ModifyOrder {
                    order_id: 5,
                    new_price: None,
                    new_qty: Some(fp(9)), // qty increase => requeue, same id
                },
            ),
        ],
        3 => vec![
            (addr(13), place(1, false, 500, 4)), // sweep into best bid
            (addr(14), place(2, false, 50, 3)),  // cross market 2
        ],
        _ => unreachable!(),
    }
}

fn run_block(ctx: &mut NativeExecContext, block: u64) {
    for t in 1u8..=14 {
        fund(ctx, &addr(t));
    }
    for (sender, action) in block_actions(block) {
        NativeExecutor::execute(ctx, &sender, &action);
    }
    ctx.save_order_books();
}

fn observe(ctx: &NativeExecContext) -> String {
    let mut mids: Vec<u64> = ctx.order_books.keys().copied().collect();
    mids.sort_unstable();
    let mut out = String::new();
    for m in mids {
        let b = &ctx.order_books[&m];
        out.push_str(&format!(
            "m={m} snap={:?} count={} next={} ltp={:?}\n",
            b.to_snapshot(),
            b.order_count(),
            b.next_order_id(),
            b.last_trade_price()
        ));
    }
    out.push_str(&format!("g={}", ctx.next_global_order_id));
    out
}

fn cf_dump(db: &StateDb) -> Vec<(Vec<u8>, Vec<u8>)> {
    StateBackend::iterate_cf(db, CF_NATIVE_ORDER_BOOKS, None).unwrap()
}

/// THE equivalence proof: cached handoff across blocks == reload-per-block,
/// in CF bytes and in observable book state.
#[test]
fn cached_run_matches_reload_per_block_run() {
    // Run A: reload from disk every block (pre-cache behavior).
    let (_d1, db_a) = open_test_db();
    let mut last_obs_a = String::new();
    for block in 1..=3u64 {
        let mut ctx = make_ctx(db_a.clone(), block);
        run_block(&mut ctx, block);
        last_obs_a = observe(&ctx);
    }

    // Run B: resume from the cache between blocks.
    let (_d2, db_b) = open_test_db();
    let mut cache = None;
    let mut last_obs_b = String::new();
    for block in 1..=3u64 {
        let mut ctx = match cache.take() {
            Some(c) => NativeExecContext::resume(db_b.clone(), c, block, 1000 + block, 0, 100, 10, addr(99), addr(100), addr(101)),
            None => make_ctx(db_b.clone(), block),
        };
        run_block(&mut ctx, block);
        last_obs_b = observe(&ctx);
        cache = Some(ctx.take_book_cache(block + 1));
    }

    assert_eq!(last_obs_a, last_obs_b, "in-memory book state diverged");
    assert_eq!(cf_dump(&db_a), cf_dump(&db_b), "CF bytes diverged");
}

/// Journal continuity: a save in a RESUMED context still writes O(touched)
/// rows, and never re-writes untouched depth.
#[test]
fn resumed_save_is_o_touched() {
    let (_dir, db) = open_test_db();
    let mut ctx = make_ctx(db.clone(), 1);
    run_block(&mut ctx, 1); // builds 61 resting orders, saves all
    let rows_after_b1 = cf_dump(&db).len();
    assert!(rows_after_b1 > 60, "depth built: {rows_after_b1}");

    let cache = ctx.take_book_cache(2);
    let mut ctx = NativeExecContext::resume(
        db.clone(),
        cache,
        2,
        1002,
        0,
        100,
        10,
        addr(99),
        addr(100),
        addr(101),
    );
    assert_eq!(ctx.books_loaded, 0, "resume must not reload from disk");
    fund(&ctx, &addr(12));
    // One cancel touches exactly one row (id 3 is owned by addr(2)).
    let r = NativeExecutor::execute(&mut ctx, &addr(2), &NativeAction::CancelOrder { order_id: 3 });
    assert!(r.success, "cancel must succeed: {:?}", r.error);
    ctx.save_order_books();
    let after = cf_dump(&db);
    assert_eq!(
        after.len(),
        rows_after_b1 - 1,
        "exactly one row deleted, nothing rewritten spuriously"
    );

    // Restart-equivalence: a FRESH context (disk reload) sees the same books.
    let fresh = make_ctx(db, 3);
    assert_eq!(observe(&fresh), observe(&ctx), "disk reload != cached state");
}

/// The height guard: resuming with the wrong expected height must be refused
/// by the caller-side check (cache carries the height it is valid for).
#[test]
fn cache_carries_expected_height() {
    let (_dir, db) = open_test_db();
    let mut ctx = make_ctx(db.clone(), 1);
    run_block(&mut ctx, 1);
    let cache = ctx.take_book_cache(2);
    assert_eq!(cache.next_height, 2);
}

/// Counter-row continuity: the persisted global order-id counter stays
/// correct across cached handoffs (no spurious rewrites, no stale values).
#[test]
fn counter_row_correct_across_cache_handoff() {
    let (_dir, db) = open_test_db();
    let mut ctx = make_ctx(db.clone(), 1);
    run_block(&mut ctx, 1);
    let next_after_b1 = ctx.next_global_order_id;

    let cache = ctx.take_book_cache(2);
    let mut ctx = NativeExecContext::resume(
        db.clone(),
        cache,
        2,
        1002,
        0,
        100,
        10,
        addr(99),
        addr(100),
        addr(101),
    );
    assert_eq!(ctx.next_global_order_id, next_after_b1);
    fund(&ctx, &addr(12));
    NativeExecutor::execute(&mut ctx, &addr(12), &place(1, true, 400, 1));
    ctx.save_order_books();
    let next_after_b2 = ctx.next_global_order_id;
    assert!(next_after_b2 > next_after_b1);

    // A cold restart must see the advanced counter.
    let fresh = make_ctx(db, 3);
    assert_eq!(
        fresh.next_global_order_id, next_after_b2,
        "persisted counter must reflect the cached-run allocations"
    );
}
