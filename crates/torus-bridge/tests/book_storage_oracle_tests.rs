//! Deep-book storage round — CHARACTERIZATION ORACLES (exec level).
//!
//! Pins the executor-visible semantics of order-book persistence that must
//! survive the storage-layout change (monolithic borsh blob -> per-order
//! rows):
//!   1. a save + reload across contexts ("restart") is behaviorally invisible
//!      — a node that reloads mid-stream reaches the same book state as a
//!      node that never persisted;
//!   2. the global order-id counter survives;
//!   3. identical action scripts on two fresh DBs produce identical native
//!      state roots, with the incrementally-maintained root equal to the
//!      full-scan oracle (root VALUES may change across the storage refactor;
//!      these equalities may not).
//!
//! Nothing in this file references the storage format directly — everything
//! goes through NativeExecContext / save_order_books / the overlay flush.

use alloy_primitives::Address;
use torus_bridge::native_executor::{NativeExecContext, NativeExecutor};
use torus_state::{NativeStateOverlay, StateDb};
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

fn make_ctx<T: torus_state::StateBackend>(state: T, height: u64) -> NativeExecContext<T> {
    NativeExecContext::new(
        state,
        height,
        1000 + height, // timestamp
        0,             // epoch
        100,           // epoch_length
        10,            // max_validators
        addr(99),      // proposer
        addr(100),
        addr(101),
    )
}

fn fund<T: torus_state::StateBackend>(ctx: &NativeExecContext<T>, trader: &Address) {
    use torus_core::position::NativeBalance;
    let bal = NativeBalance {
        available: fp(100_000_000),
        order_margin: FixedPoint::ZERO,
    };
    ctx.positions.put_native_balance(trader, &bal).unwrap();
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

/// Observable content of every book in a context (sorted by market id).
fn observe_books<T: torus_state::StateBackend>(ctx: &NativeExecContext<T>) -> String {
    let mut markets: Vec<u64> = ctx.order_books.keys().copied().collect();
    markets.sort_unstable();
    let mut out = String::new();
    for m in markets {
        let book = &ctx.order_books[&m];
        out.push_str(&format!(
            "m={m} snap={:?} count={} next_id={} ltp={:?} stops={}\n",
            book.to_snapshot(),
            book.order_count(),
            book.next_order_id(),
            book.last_trade_price(),
            book.pending_stop_count()
        ));
    }
    out.push_str(&format!("global_next={}", ctx.next_global_order_id));
    out
}

/// Block-1 script: resting depth on two markets, a partial cross, a cancel,
/// and a priority-losing modify (qty increase => cancel+reinsert, same id).
fn block1_script<T: torus_state::StateBackend>(ctx: &mut NativeExecContext<T>) {
    for t in 1u8..=8 {
        fund(ctx, &addr(t));
    }
    // Market 1: two bids at 100 (FIFO), one at 99; asks at 105/106.
    assert!(NativeExecutor::execute(ctx, &addr(1), &place(1, true, 100, 5)).success);
    assert!(NativeExecutor::execute(ctx, &addr(2), &place(1, true, 100, 3)).success);
    assert!(NativeExecutor::execute(ctx, &addr(3), &place(1, true, 99, 2)).success);
    assert!(NativeExecutor::execute(ctx, &addr(4), &place(1, false, 105, 4)).success);
    assert!(NativeExecutor::execute(ctx, &addr(5), &place(1, false, 105, 6)).success);
    // Market 2: one resting bid.
    assert!(NativeExecutor::execute(ctx, &addr(6), &place(2, true, 50, 7)).success);
    // Partial cross on market 1: buy 6 @ 105 (fills addr4's 4 + 2 of addr5's 6).
    assert!(NativeExecutor::execute(ctx, &addr(7), &place(1, true, 105, 6)).success);
    // Cancel addr(3)'s bid (order id 3: global ids are 1-based and sequential).
    assert!(
        NativeExecutor::execute(ctx, &addr(3), &NativeAction::CancelOrder { order_id: 3 }).success
    );
    // Modify addr(1)'s bid qty 5 -> 8: cancel+reinsert (same id, loses priority).
    assert!(
        NativeExecutor::execute(
            ctx,
            &addr(1),
            &NativeAction::ModifyOrder {
                order_id: 1,
                new_price: None,
                new_qty: Some(fp(8)),
            }
        )
        .success
    );
}

/// Block-2 script: sweep market 1's 100 level; the fill order (addr2 before
/// the requeued addr1) is the priority-preservation probe.
fn block2_script<T: torus_state::StateBackend>(ctx: &mut NativeExecContext<T>) {
    fund(ctx, &addr(8));
    assert!(NativeExecutor::execute(ctx, &addr(8), &place(1, false, 100, 11)).success);
    assert!(NativeExecutor::execute(ctx, &addr(8), &place(2, false, 50, 2)).success);
}

/// Oracle A: a save + context reload ("restart") between blocks is
/// behaviorally invisible — final books match a run that never reloaded.
#[test]
fn oracle_exec_reload_matches_continuous_run() {
    // Run 1 (restarting): block1 -> save -> FRESH ctx (reload from disk) -> block2.
    let (_d1, db1) = open_test_db();
    let mut ctx = make_ctx(db1.clone(), 1);
    block1_script(&mut ctx);
    ctx.save_order_books();
    let mut ctx = make_ctx(db1.clone(), 2);
    block2_script(&mut ctx);
    ctx.save_order_books();
    let restarted = observe_books(&ctx);

    // Run 2 (continuous): same scripts through ONE in-memory context.
    let (_d2, db2) = open_test_db();
    let mut ctx = make_ctx(db2.clone(), 1);
    block1_script(&mut ctx);
    block2_script(&mut ctx);
    let continuous = observe_books(&ctx);

    assert_eq!(
        restarted, continuous,
        "reload-across-contexts must be behaviorally invisible"
    );

    // And a reload AFTER block2 still observes identically (save is faithful).
    let ctx = make_ctx(db1, 3);
    assert_eq!(observe_books(&ctx), restarted, "post-block2 reload diverged");
}

/// Oracle B: the requeued (modified) order must NOT regain time priority
/// after the reload — addr(2) is filled fully at 100 before addr(1)'s
/// modified order. Detected via remaining book state after a partial sweep.
#[test]
fn oracle_exec_requeue_priority_after_reload() {
    let (_dir, db) = open_test_db();
    let mut ctx = make_ctx(db.clone(), 1);
    block1_script(&mut ctx);
    ctx.save_order_books();

    // Fresh context = reload. Sell 4 @ 100: must consume addr(2)'s 3 first
    // (front of queue), then 1 of addr(1)'s 8 — leaving addr(1) resting 7.
    let mut ctx = make_ctx(db, 2);
    fund(&ctx, &addr(8));
    assert!(NativeExecutor::execute(&mut ctx, &addr(8), &place(1, false, 100, 4)).success);
    let book = ctx.order_books.get(&1).unwrap();
    let order1 = book.get_order(1).expect("modified order still resting");
    assert_eq!(order1.remaining_qty, fp(7), "addr(2) must have filled first");
    assert!(book.get_order(2).is_none(), "addr(2)'s order fully consumed");
}

/// Oracle C: next_global_order_id survives a reload with resting orders.
#[test]
fn oracle_exec_global_order_id_survives_reload() {
    let (_dir, db) = open_test_db();
    let mut ctx = make_ctx(db.clone(), 1);
    block1_script(&mut ctx);
    let next_id = ctx.next_global_order_id;
    ctx.save_order_books();

    let ctx = make_ctx(db, 2);
    assert_eq!(
        ctx.next_global_order_id, next_id,
        "global order-id counter must survive reload"
    );
}

/// Oracle D: identical scripts on two fresh DBs produce identical native
/// state roots, and the incrementally-maintained bucketed root equals the
/// full-scan oracle after every flush. (Root values change with the storage
/// layout; these EQUALITIES must hold on either side of the refactor.)
#[test]
fn oracle_exec_state_root_determinism_and_incremental_equality() {
    let run = |db: &StateDb| -> Vec<alloy_primitives::B256> {
        torus_state::native_trie::build_native_trie_to_cf(db).unwrap();
        let mut roots = Vec::new();
        for block in 1u64..=2 {
            let overlay = NativeStateOverlay::new(db.clone());
            let mut ctx = make_ctx(overlay.clone(), block);
            if block == 1 {
                block1_script(&mut ctx);
            } else {
                block2_script(&mut ctx);
            }
            ctx.save_order_books();
            overlay.flush_with_native_trie(db).unwrap();
            let full = torus_state::native_trie::native_root_full(db).unwrap();
            let incr = torus_state::native_trie::persisted_native_root(db).unwrap();
            assert_eq!(incr, full, "incremental root != full scan at block {block}");
            roots.push(full);
        }
        roots
    };

    let (_d1, db1) = open_test_db();
    let (_d2, db2) = open_test_db();
    let r1 = run(&db1);
    let r2 = run(&db2);
    assert_eq!(r1, r2, "identical scripts must yield identical roots");
    assert_ne!(r1[0], r1[1], "roots must evolve across blocks");
}
