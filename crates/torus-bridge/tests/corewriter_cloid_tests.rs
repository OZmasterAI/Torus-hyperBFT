//! s114 cloid over CoreWriter: `placeOrder(bytes32,uint8,uint8,uint128,
//! uint128,uint8,uint128)` queues a placement with a cloid (0 = none) and
//! `cancelByCloid(bytes32,uint128)` a cancel by cloid (queue kind tag 8).
//! The drain runs them as native `PlaceOrder` / `CancelByCloid` under the
//! cross-market uniqueness rule and reports each row's cloid.

use alloy_primitives::Address;

use torus_bridge::native_executor::{BookMode, NativeExecContext, NativeExecutor};
use torus_core::position::NativeBalance;
use torus_state::action_status::FailureReason;
use torus_state::StateDb;
use torus_types::{FixedPoint, MarketId};

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

fn make_ctx(db: StateDb, height: u64, mode: BookMode) -> NativeExecContext {
    NativeExecContext::new_with_mode(db, height, 1000 + height, 0, 100, 10, addr(99), addr(100), addr(101), mode, None)
}

fn fund(ctx: &NativeExecContext, trader: &Address) {
    let bal = NativeBalance {
        available: fp(1_000_000),
        order_margin: FixedPoint::ZERO,
    };
    ctx.positions.put_native_balance(trader, &bal).unwrap();
}

fn live(ctx: &NativeExecContext, market_id: MarketId, trader: &Address, cloid: u128) -> Option<u128> {
    ctx.order_books.get(&market_id).and_then(|b| b.order_id_by_cloid(trader, cloid))
}

// ---------------------------------------------------------------------------
// CoreWriter: placeOrder with a cloid, cancelByCloid
// ---------------------------------------------------------------------------

fn cw_enqueue(db: &StateDb, trader: Address, kind: torus_core::precompiles::QueuedActionKind) -> u64 {
    torus_core::precompiles::CoreWriterQueue::enqueue(
        db,
        &torus_core::precompiles::QueuedAction { trader, kind, block_queued: 1 },
    )
    .unwrap()
}

fn cw_place(market_id: MarketId, price: i64, cloid: Option<u128>) -> torus_core::precompiles::QueuedActionKind {
    torus_core::precompiles::QueuedActionKind::PlaceOrder {
        market_id,
        side: 0,
        order_type: 0,
        price: fp(price),
        quantity: fp(1),
        time_in_force: 0,
        cloid,
    }
}

/// Block 2 drains: a placement with C (rests, reports oid + cloid), a second
/// placement with C in another market (duplicate), a cancel by cloid (cancels
/// the first; reports its cloid), and a placement without a cloid.
#[test]
fn core_writer_places_and_cancels_by_cloid() {
    use torus_core::precompiles::QueuedActionKind;
    use torus_state::action_status::CoreWriterKind;
    let (_d, db) = open_test_db();
    let t = addr(5);
    let pre = make_ctx(db.clone(), 2, BookMode::Classic);
    fund(&pre, &t);
    drop(pre);
    // Queued by block 1's EVM: the queue files them under block 2.
    for kind in [
        cw_place(1, 100, Some(C)),
        cw_place(2, 100, Some(C)),
        QueuedActionKind::CancelByCloid { market_id: 1, cloid: C },
        cw_place(1, 90, None),
    ] {
        cw_enqueue(&db, t, kind);
    }
    let mut ctx = make_ctx(db, 2, BookMode::Classic);
    let e = NativeExecutor::drain_core_writer_entries(&mut ctx).expect("queue reads");
    assert_eq!(ctx.take_fatal_error(), None);
    let got: Vec<_> = e
        .iter()
        .map(|e| (e.kind, e.result.success, e.result.reason, e.cloid, e.oid.is_some()))
        .collect();
    assert_eq!(
        got,
        vec![
            (CoreWriterKind::PlaceOrder, true, FailureReason::Other, Some(C), true),
            (CoreWriterKind::PlaceOrder, false, FailureReason::DuplicateCloid, Some(C), false),
            (CoreWriterKind::CancelByCloid, true, FailureReason::Other, Some(C), false),
            (CoreWriterKind::PlaceOrder, true, FailureReason::Other, None, true),
        ]
    );
    assert!(live(&ctx, 1, &t, C).is_none());
    assert_eq!(ctx.order_books[&1].order_count(), 1);
    assert_eq!(ctx.order_books.get(&2).map_or(0, |b| b.order_count()), 0);
}
