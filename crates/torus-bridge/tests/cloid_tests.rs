//! s114 cloid (owner decisions 2-3) at the executor:
//! - a cloid is unique among ALL of a trader's open orders across ALL
//!   markets (resting orders and pending stops): a placement whose cloid is
//!   live for its sender is rejected (`FailureReason::DuplicateCloid`), on
//!   the single path and the batch path (Phase 2 runs after Phase 1, so the
//!   check sees this call's cancels; two placements of one cloid in one call:
//!   the first claims it);
//! - `NativeAction::CancelByCloid { market_id, cloid }` cancels the sender's
//!   resting order OR pending stop with that cloid in that market and
//!   releases its margin;
//! - `CancelOrder { order_id }` also cancels a pending stop (stops used to be
//!   cancellable only through cancel-all);
//! - the index is rebuilt from the books on restart, and two replicas (or a
//!   restart in the middle) end in the same state root.

use alloy_primitives::Address;

use torus_bridge::native_executor::{
    BookMode, NativeActionResult, NativeExecContext, NativeExecutor,
};
use torus_bridge::state_root::compute_native_state_root;
use torus_core::position::NativeBalance;
use torus_state::action_status::FailureReason;
use torus_state::StateDb;
use torus_types::{FixedPoint, MarketId, NativeAction, OrderType, PlaceOrderParams, TimeInForce};

/// Above u64::MAX.
const C: u128 = 0xc10d_0000_0000_0000_0000_0000_0000_0001;

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

const MODES: [BookMode; 3] = [
    BookMode::Classic,
    BookMode::OrderRows,
    BookMode::LevelAuthority,
];

fn make_ctx(db: StateDb, height: u64, mode: BookMode) -> NativeExecContext {
    NativeExecContext::new_with_mode(
        db,
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

fn fund(ctx: &NativeExecContext, trader: &Address) {
    let bal = NativeBalance {
        available: fp(1_000_000),
        order_margin: FixedPoint::ZERO,
    };
    ctx.positions.put_native_balance(trader, &bal).unwrap();
}

fn margin(ctx: &NativeExecContext, trader: &Address) -> FixedPoint {
    ctx.positions
        .get_native_balance(trader)
        .unwrap()
        .order_margin
}

fn order(
    market_id: MarketId,
    is_buy: bool,
    price: i64,
    ot: OrderType,
    cloid: Option<u128>,
) -> PlaceOrderParams {
    PlaceOrderParams {
        market_id,
        is_buy,
        price: fp(price),
        quantity: fp(1),
        order_type: ot,
        time_in_force: TimeInForce::GTC,
        reduce_only: false,
        client_order_id: cloid,
    }
}

fn buy(market_id: MarketId, price: i64, cloid: Option<u128>) -> NativeAction {
    NativeAction::PlaceOrder(order(market_id, true, price, OrderType::Limit, cloid))
}

fn sell(market_id: MarketId, price: i64, cloid: Option<u128>) -> NativeAction {
    NativeAction::PlaceOrder(order(market_id, false, price, OrderType::Limit, cloid))
}

/// A buy stop far above the market (never fires unless a trade reaches it).
fn stop(market_id: MarketId, trigger: i64, cloid: Option<u128>) -> NativeAction {
    NativeAction::PlaceOrder(order(
        market_id,
        true,
        trigger + 5,
        OrderType::StopMarket {
            trigger: fp(trigger),
        },
        cloid,
    ))
}

fn by_cloid(market_id: MarketId, cloid: u128) -> NativeAction {
    NativeAction::CancelByCloid { market_id, cloid }
}

#[derive(Clone, Copy, Debug)]
enum Path {
    Single,
    Batch,
    BatchCancelMode,
}

const PATHS: [Path; 3] = [Path::Single, Path::Batch, Path::BatchCancelMode];

fn run(
    ctx: &mut NativeExecContext,
    path: Path,
    sender: Address,
    action: NativeAction,
) -> NativeActionResult {
    match path {
        Path::Single => NativeExecutor::execute(ctx, &sender, &action),
        Path::Batch => NativeExecutor::execute_batch(ctx, &[(sender, action)])
            .results
            .remove(0),
        Path::BatchCancelMode => {
            NativeExecutor::execute_batch_cancel_mode(ctx, &[(sender, action)], true)
                .results
                .remove(0)
        }
    }
}

fn ok(r: &NativeActionResult, what: &str) {
    assert!(r.success, "{what}: {r:?}");
}

fn dup(r: &NativeActionResult, what: &str) {
    assert!(
        !r.success,
        "{what}: expected a duplicate-cloid reject, got {r:?}"
    );
    assert_eq!(r.reason, FailureReason::DuplicateCloid, "{what}: {r:?}");
}

fn live(
    ctx: &NativeExecContext,
    market_id: MarketId,
    trader: &Address,
    cloid: u128,
) -> Option<u128> {
    ctx.order_books
        .get(&market_id)
        .and_then(|b| b.order_id_by_cloid(trader, cloid))
}

#[test]
fn a_live_cloid_rejects_a_placement_in_any_market() {
    for path in PATHS {
        let what = format!("{path:?}");
        let (_d, db) = open_test_db();
        let mut ctx = make_ctx(db, 1, BookMode::Classic);
        for n in 1..=2 {
            fund(&ctx, &addr(n));
        }
        ok(&run(&mut ctx, path, addr(1), buy(1, 100, Some(C))), &what);
        dup(&run(&mut ctx, path, addr(1), buy(1, 90, Some(C))), &what);
        dup(&run(&mut ctx, path, addr(1), sell(2, 500, Some(C))), &what);
        dup(&run(&mut ctx, path, addr(1), stop(3, 900, Some(C))), &what);
        // Another trader, another cloid, or none: fine.
        ok(&run(&mut ctx, path, addr(2), buy(2, 100, Some(C))), &what);
        ok(
            &run(&mut ctx, path, addr(1), buy(2, 100, Some(C + 1))),
            &what,
        );
        ok(&run(&mut ctx, path, addr(1), buy(2, 100, None)), &what);
        // A live stop holds its cloid across markets too.
        ok(
            &run(&mut ctx, path, addr(1), stop(3, 900, Some(C + 2))),
            &what,
        );
        dup(
            &run(&mut ctx, path, addr(1), buy(1, 80, Some(C + 2))),
            &what,
        );
        // Nothing the rejects did is left behind: one order per live cloid.
        assert_eq!(ctx.order_books[&1].open_order_count(&addr(1)), 1, "{what}");
        // Cancelled -> free again, in any market.
        let id = live(&ctx, 1, &addr(1), C).unwrap();
        ok(
            &run(
                &mut ctx,
                path,
                addr(1),
                NativeAction::CancelOrder { order_id: id },
            ),
            &what,
        );
        ok(&run(&mut ctx, path, addr(1), sell(2, 500, Some(C))), &what);
    }
}

/// One batch call: Phase 1 (cancels) runs before the placements, and the
/// first placement of a cloid claims it for the rest of the call.
#[test]
fn a_batch_checks_after_its_cancels_and_lets_the_first_placement_win() {
    let (_d, db) = open_test_db();
    let mut ctx = make_ctx(db, 1, BookMode::Classic);
    fund(&ctx, &addr(1));
    ok(
        &run(&mut ctx, Path::Single, addr(1), buy(1, 100, Some(C))),
        "setup",
    );
    let r = NativeExecutor::execute_batch(
        &mut ctx,
        &[
            (addr(1), buy(2, 100, Some(C))), // after the cancel below: free
            (addr(1), by_cloid(1, C)),
            (addr(1), sell(3, 500, Some(C))), // claimed by the first placement
            (
                addr(1),
                NativeAction::PlaceOrderBatch(vec![
                    order(1, true, 50, OrderType::Limit, Some(C + 1)),
                    order(2, true, 50, OrderType::Limit, Some(C + 1)),
                ]),
            ),
        ],
    );
    let res = &r.results;
    assert_eq!(res.len(), 5);
    ok(&res[0], "first placement");
    ok(&res[1], "cancel by cloid");
    dup(&res[2], "second placement in the call");
    ok(&res[3], "batch order 1");
    dup(&res[4], "batch order 2");
    assert!(live(&ctx, 1, &addr(1), C).is_none());
    assert!(live(&ctx, 2, &addr(1), C).is_some());
    assert!(live(&ctx, 1, &addr(1), C + 1).is_some());
}

#[test]
fn cancel_by_cloid_cancels_a_resting_order_and_releases_its_margin() {
    for path in PATHS {
        let what = format!("{path:?}");
        let (_d, db) = open_test_db();
        let mut ctx = make_ctx(db, 1, BookMode::Classic);
        fund(&ctx, &addr(1));
        fund(&ctx, &addr(2));
        ok(&run(&mut ctx, path, addr(1), buy(1, 100, Some(C))), &what);
        assert!(margin(&ctx, &addr(1)) > FixedPoint::ZERO, "{what}");
        // Wrong market, unknown cloid, another trader: nothing to cancel.
        assert!(
            !run(&mut ctx, path, addr(1), by_cloid(2, C)).success,
            "{what}"
        );
        assert!(
            !run(&mut ctx, path, addr(1), by_cloid(1, C + 9)).success,
            "{what}"
        );
        assert!(
            !run(&mut ctx, path, addr(2), by_cloid(1, C)).success,
            "{what}"
        );
        assert!(live(&ctx, 1, &addr(1), C).is_some(), "{what}");

        ok(&run(&mut ctx, path, addr(1), by_cloid(1, C)), &what);
        assert!(live(&ctx, 1, &addr(1), C).is_none(), "{what}");
        assert_eq!(ctx.order_books[&1].order_count(), 0, "{what}");
        assert_eq!(margin(&ctx, &addr(1)), FixedPoint::ZERO, "{what}");
        assert!(
            !run(&mut ctx, path, addr(1), by_cloid(1, C)).success,
            "{what}: twice"
        );
    }
}

#[test]
fn cancel_by_cloid_cancels_a_pending_stop_and_releases_its_margin() {
    for path in PATHS {
        let what = format!("{path:?}");
        let (_d, db) = open_test_db();
        let mut ctx = make_ctx(db, 1, BookMode::Classic);
        fund(&ctx, &addr(1));
        ok(&run(&mut ctx, path, addr(1), stop(1, 900, None)), &what);
        let other = margin(&ctx, &addr(1));
        ok(&run(&mut ctx, path, addr(1), stop(1, 800, Some(C))), &what);
        assert!(margin(&ctx, &addr(1)) > other, "{what}: the stop reserved");
        assert_eq!(ctx.order_books[&1].pending_stop_count(), 2, "{what}");

        ok(&run(&mut ctx, path, addr(1), by_cloid(1, C)), &what);
        assert_eq!(ctx.order_books[&1].pending_stop_count(), 1, "{what}");
        assert_eq!(
            margin(&ctx, &addr(1)),
            other,
            "{what}: its reservation came back"
        );
        assert!(live(&ctx, 1, &addr(1), C).is_none(), "{what}");
        ok(&run(&mut ctx, path, addr(1), stop(2, 800, Some(C))), &what);
    }
}

#[test]
fn cancel_order_cancels_a_pending_stop_by_id_for_its_owner_only() {
    for path in PATHS {
        let what = format!("{path:?}");
        let (_d, db) = open_test_db();
        let mut ctx = make_ctx(db, 1, BookMode::Classic);
        fund(&ctx, &addr(1));
        fund(&ctx, &addr(2));
        ok(&run(&mut ctx, path, addr(1), stop(1, 800, Some(C))), &what);
        let id = live(&ctx, 1, &addr(1), C).expect("stop indexed");
        let r = run(
            &mut ctx,
            path,
            addr(2),
            NativeAction::CancelOrder { order_id: id },
        );
        assert!(!r.success, "{what}: a stranger cancelled the stop");
        assert!(
            r.error.as_deref().unwrap_or("").contains("belongs to"),
            "{what}: {r:?}"
        );
        assert_eq!(ctx.order_books[&1].pending_stop_count(), 1, "{what}");

        ok(
            &run(
                &mut ctx,
                path,
                addr(1),
                NativeAction::CancelOrder { order_id: id },
            ),
            &what,
        );
        assert_eq!(ctx.order_books[&1].pending_stop_count(), 0, "{what}");
        assert_eq!(margin(&ctx, &addr(1)), FixedPoint::ZERO, "{what}");
        assert!(live(&ctx, 1, &addr(1), C).is_none(), "{what}");
    }
}

/// A stop-limit carrying a cloid fires; the order it places rests under the
/// same cloid (the stop's own id), which then blocks a new placement.
#[test]
fn a_triggered_stop_keeps_its_cloid() {
    let (_d, db) = open_test_db();
    let mut ctx = make_ctx(db, 1, BookMode::Classic);
    for n in 1..=3 {
        fund(&ctx, &addr(n));
    }
    let sl = NativeAction::PlaceOrder(order(
        1,
        true,
        90,
        OrderType::StopLimit {
            trigger: fp(105),
            limit: fp(90),
        },
        Some(C),
    ));
    ok(&NativeExecutor::execute(&mut ctx, &addr(1), &sl), "stop");
    let stop_id = live(&ctx, 1, &addr(1), C).unwrap();
    ok(
        &NativeExecutor::execute(&mut ctx, &addr(2), &sell(1, 106, None)),
        "ask",
    );
    ok(
        &NativeExecutor::execute(&mut ctx, &addr(3), &buy(1, 106, None)),
        "trade at 106",
    );
    assert_eq!(ctx.order_books[&1].pending_stop_count(), 0);
    assert_eq!(
        live(&ctx, 1, &addr(1), C),
        Some(stop_id),
        "rests under its stop id"
    );
    dup(
        &NativeExecutor::execute(&mut ctx, &addr(1), &buy(2, 50, Some(C))),
        "after trigger",
    );
}

/// Block 1 places cloids (a resting order and a stop) and saves; a fresh
/// context (restart) rebuilds the index from the stored books.
#[test]
fn the_index_is_rebuilt_on_restart() {
    for mode in MODES {
        let what = format!("{mode:?}");
        let (_d, db) = open_test_db();
        let mut ctx = make_ctx(db.clone(), 1, mode);
        fund(&ctx, &addr(1));
        ok(
            &NativeExecutor::execute(&mut ctx, &addr(1), &buy(1, 100, Some(C))),
            &what,
        );
        ok(
            &NativeExecutor::execute(&mut ctx, &addr(1), &stop(2, 900, Some(C + 1))),
            &what,
        );
        ctx.save_order_books();
        drop(ctx);

        let mut ctx = make_ctx(db, 2, mode);
        assert!(ctx.take_fatal_error().is_none(), "{what}");
        assert!(live(&ctx, 1, &addr(1), C).is_some(), "{what}");
        assert!(live(&ctx, 2, &addr(1), C + 1).is_some(), "{what}");
        dup(
            &NativeExecutor::execute(&mut ctx, &addr(1), &buy(3, 100, Some(C))),
            &what,
        );
        dup(
            &NativeExecutor::execute(&mut ctx, &addr(1), &buy(3, 100, Some(C + 1))),
            &what,
        );
        ok(
            &NativeExecutor::execute(&mut ctx, &addr(1), &by_cloid(2, C + 1)),
            &what,
        );
        ok(
            &NativeExecutor::execute(&mut ctx, &addr(1), &by_cloid(1, C)),
            &what,
        );
    }
}

/// Blocks of cloid traffic: placements (dup rejects included), a fill that
/// frees a maker cloid, cancels by cloid of an order and a stop, re-use.
fn script() -> Vec<Vec<(Address, NativeAction)>> {
    let (a, b) = (addr(1), addr(2));
    vec![
        vec![
            (a, sell(1, 100, Some(C))),
            (a, buy(2, 50, Some(C))),
            (a, stop(2, 900, Some(C + 1))),
            (b, buy(1, 90, Some(C))),
        ],
        vec![
            (b, buy(1, 100, None)), // fills a's C
            (a, by_cloid(2, C + 1)),
            (b, by_cloid(1, C)),
        ],
        vec![
            (a, buy(2, 60, Some(C))), // free again after the fill
            (a, stop(1, 950, Some(C + 1))),
            (a, buy(1, 70, Some(C + 1))), // dup of the stop
        ],
    ]
}

fn run_script(
    db: StateDb,
    mode: BookMode,
    restart_after: Option<usize>,
) -> (Vec<Vec<(bool, FailureReason)>>, alloy_primitives::B256) {
    let mut out = Vec::new();
    let mut ctx = make_ctx(db.clone(), 1, mode);
    fund(&ctx, &addr(1));
    fund(&ctx, &addr(2));
    for (i, block) in script().into_iter().enumerate() {
        if i > 0 {
            ctx.save_order_books();
            if restart_after == Some(i) {
                drop(ctx);
                ctx = make_ctx(db.clone(), 1, mode);
            }
        }
        let r = NativeExecutor::execute_batch(&mut ctx, &block);
        out.push(r.results.iter().map(|r| (r.success, r.reason)).collect());
    }
    ctx.save_order_books();
    (out, compute_native_state_root(&ctx.state).unwrap())
}

#[test]
fn replicas_and_a_restart_agree_on_results_and_state_root() {
    for mode in MODES {
        let what = format!("{mode:?}");
        let (_d1, db1) = open_test_db();
        let (_d2, db2) = open_test_db();
        let (_d3, db3) = open_test_db();
        let one = run_script(db1, mode, None);
        let two = run_script(db2, mode, None);
        let restarted = run_script(db3, mode, Some(2));
        assert_eq!(one, two, "{what}: two replicas");
        assert_eq!(one, restarted, "{what}: restart before block 3");
        let r = &one.0;
        assert_eq!(r[0][3], (true, FailureReason::Other), "{what}: b's own C");
        assert_eq!(
            r[2][0],
            (true, FailureReason::Other),
            "{what}: C free after the fill"
        );
        assert_eq!(
            r[2][2],
            (false, FailureReason::DuplicateCloid),
            "{what}: dup of a stop"
        );
    }
}
