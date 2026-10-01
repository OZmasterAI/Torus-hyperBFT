//! s83 security fix: `NativeAction::ModifyOrder` must only modify the SENDER's
//! own resting order (mirrors `CancelOrder`'s ownership check).
//!
//! Every production path that applies a `ModifyOrder` funnels into
//! `NativeExecutor::execute_action` (the CoreWriter precompile has no modify
//! selector), reached via:
//!   - `execute_batch` (live block commit, pre/post-EVM batches),
//!   - `execute_batch` with `TORUS_CANCEL_BATCH` on (Phase-1 run splitter),
//!   - `execute` (single-action path).
//! Each is exercised below.

use alloy_primitives::Address;

use torus_bridge::native_executor::{NativeActionResult, NativeExecContext, NativeExecutor};

use torus_state::StateDb;
use torus_types::{FixedPoint, NativeAction, OrderType, PlaceOrderParams, Side, TimeInForce};

const MARKET: u32 = 1;

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

fn make_ctx(state_db: StateDb) -> NativeExecContext {
    NativeExecContext::new(
        state_db,
        1,
        1000,
        0,
        100,
        10,
        addr(99),
        addr(100),
        addr(101),
    )
}

fn fund(ctx: &NativeExecContext, trader: &Address, amount: FixedPoint) {
    let bal = torus_core::position::NativeBalance {
        available: amount,
        order_margin: FixedPoint::ZERO,
    };
    ctx.positions.put_native_balance(trader, &bal).unwrap();
}

fn sell(price: i64, qty: i64) -> NativeAction {
    NativeAction::PlaceOrder(PlaceOrderParams {
        market_id: MARKET as _,
        is_buy: false,
        price: fp(price),
        quantity: fp(qty),
        order_type: OrderType::Limit,
        time_in_force: TimeInForce::GTC,
        reduce_only: false,
        client_order_id: None,
    })
}

fn modify(order_id: u128, new_price: Option<i64>, new_qty: Option<i64>) -> NativeAction {
    NativeAction::ModifyOrder {
        order_id,
        new_price: new_price.map(fp),
        new_qty: new_qty.map(fp),
    }
}

#[derive(Clone, Copy, Debug)]
enum Path {
    Batch,
    BatchCancelMode,
    Single,
}

const PATHS: [Path; 3] = [Path::Batch, Path::BatchCancelMode, Path::Single];

fn run(
    ctx: &mut NativeExecContext,
    path: Path,
    sender: Address,
    action: NativeAction,
) -> NativeActionResult {
    match path {
        Path::Batch => NativeExecutor::execute_batch(ctx, &[(sender, action)])
            .results
            .remove(0),
        Path::BatchCancelMode => {
            NativeExecutor::execute_batch_cancel_mode(ctx, &[(sender, action)], true)
                .results
                .remove(0)
        }
        Path::Single => NativeExecutor::execute(ctx, &sender, &action),
    }
}

/// Everything an attacker could disturb: the order row bytes (seq + borsh
/// order: price, qty, trader, ...), the level queue order (time priority) at
/// the order's price, and both balances.
#[derive(Debug, PartialEq, Eq)]
struct Snapshot {
    row: Option<Vec<u8>>,
    seq: Option<u64>,
    level_ids: Vec<u128>,
    balances: Vec<(FixedPoint, FixedPoint)>,
}

fn snapshot(ctx: &NativeExecContext, order_id: u128, price: i64, who: &[Address]) -> Snapshot {
    let book = &ctx.order_books[&(MARKET as _)];
    Snapshot {
        row: book.encode_order_row(order_id),
        seq: book.order_seq_of(order_id),
        level_ids: book
            .level_queue(Side::Sell, fp(price))
            .map(|q| q.iter().map(|o| o.id).collect())
            .unwrap_or_default(),
        balances: who
            .iter()
            .map(|a| {
                let b = ctx.positions.get_native_balance(a).unwrap();
                (b.available, b.order_margin)
            })
            .collect(),
    }
}

/// A rests sell@110 qty4, then C rests sell@110 qty4 behind it. Returns A's id.
fn setup(path: Path) -> (tempfile::TempDir, NativeExecContext, u128) {
    let (dir, db) = open_test_db();
    let mut ctx = make_ctx(db);
    for n in 1..=3 {
        fund(&ctx, &addr(n), fp(1_000));
    }
    let r = run(&mut ctx, path, addr(1), sell(110, 4));
    assert!(r.success, "{:?}", r.error);
    let r = run(&mut ctx, path, addr(3), sell(110, 4));
    assert!(r.success, "{:?}", r.error);
    let book = &ctx.order_books[&(MARKET as _)];
    let ids = book.orders_for_trader(&addr(1));
    assert_eq!(ids.len(), 1);
    let id = ids[0].id;
    (dir, ctx, id)
}

#[test]
fn non_owner_modify_is_rejected_and_changes_nothing() {
    let (a, b, c) = (addr(1), addr(2), addr(3));
    // Price change, qty increase (would charge A extra margin), qty decrease
    // (in-place path), and both at once.
    let attacks = [
        (Some(200), None),
        (None, Some(40)),
        (None, Some(1)),
        (Some(120), Some(2)),
    ];
    // Collect every failing (path, attack) so a regression report names them all.
    let mut failures = Vec::new();
    for path in PATHS {
        for (new_price, new_qty) in attacks {
            let (_dir, mut ctx, id) = setup(path);
            let before = snapshot(&ctx, id, 110, &[a, b, c]);
            assert!(before.row.is_some());

            let r = run(&mut ctx, path, b, modify(id, new_price, new_qty));
            let what = format!("{path:?} {new_price:?}/{new_qty:?}");
            let err = r.error.clone().unwrap_or_default();
            if r.success || !err.contains(&format!("order {id} belongs to {a}, not sender {b}")) {
                failures.push(format!("{what}: expected ownership error, got {r:?}"));
            }
            if snapshot(&ctx, id, 110, &[a, b, c]) != before {
                failures.push(format!("{what}: victim order or balances changed"));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} failures:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn owner_modify_price_change_still_works() {
    let a = addr(1);
    for path in PATHS {
        let (_dir, mut ctx, id) = setup(path);
        let r = run(&mut ctx, path, a, modify(id, Some(120), None));
        assert!(r.success, "{path:?}: {:?}", r.error);
        let book = &ctx.order_books[&(MARKET as _)];
        let o = book.get_order(id).expect("still resting");
        assert_eq!(
            (o.price, o.remaining_qty, o.trader),
            (fp(120), fp(4), a),
            "{path:?}"
        );
        // Old level now only holds C's order.
        let old_level: Vec<u128> = book
            .level_queue(Side::Sell, fp(110))
            .map(|q| q.iter().map(|o| o.id).collect())
            .unwrap_or_default();
        assert!(!old_level.contains(&id), "{path:?}");
        // Margin: 110*4/20 = 22 -> 120*4/20 = 24, charged to the owner.
        let bal = ctx.positions.get_native_balance(&a).unwrap();
        assert_eq!(
            (bal.available, bal.order_margin),
            (fp(1_000 - 24), fp(24)),
            "{path:?}"
        );
    }
}

#[test]
fn owner_modify_qty_decrease_keeps_priority() {
    let a = addr(1);
    for path in PATHS {
        let (_dir, mut ctx, id) = setup(path);
        let before = snapshot(&ctx, id, 110, &[]);
        let r = run(&mut ctx, path, a, modify(id, None, Some(2)));
        assert!(r.success, "{path:?}: {:?}", r.error);
        let after = snapshot(&ctx, id, 110, &[]);
        assert_eq!(after.seq, before.seq, "{path:?}: priority kept");
        assert_eq!(
            after.level_ids, before.level_ids,
            "{path:?}: queue order kept"
        );
        assert_eq!(after.level_ids.first(), Some(&id), "{path:?}");
        let o = ctx.order_books[&(MARKET as _)]
            .get_order(id)
            .unwrap()
            .clone();
        assert_eq!((o.price, o.remaining_qty), (fp(110), fp(2)), "{path:?}");
        // Margin: 22 -> 110*2/20 = 11, released to the owner.
        let bal = ctx.positions.get_native_balance(&a).unwrap();
        assert_eq!(
            (bal.available, bal.order_margin),
            (fp(1_000 - 11), fp(11)),
            "{path:?}"
        );
    }
}

#[test]
fn modify_unknown_order_still_not_found() {
    for path in PATHS {
        let (_dir, mut ctx, _id) = setup(path);
        let r = run(&mut ctx, path, addr(2), modify(987_654, Some(120), None));
        assert!(!r.success);
        assert_eq!(
            r.error.as_deref(),
            Some("order 987654 not found"),
            "{path:?}"
        );
    }
}
