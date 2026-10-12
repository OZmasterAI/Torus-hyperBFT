//! s112 (owner option b): every CoreWriter drain result reaches the v4 block
//! action status record. `NativeExecutor::drain_core_writer_entries` returns
//! one entry per drained queue row, in queue order, with the row's identity
//! (queue sequence, kind, trader, queued block) next to its result: a
//! success, an undecodable row, the one-block delay guard, a conversion
//! failure (stop order type), a failed lockbox withdraw and a cancel of a
//! missing order. The record and the RPC are proven in torus-consensus
//! (`app.rs`) and torus-rpc.

use alloy_primitives::Address;

use torus_bridge::native_executor::{NativeExecContext, NativeExecutor};
use torus_core::precompiles::{CoreWriterQueue, QueuedAction, QueuedActionKind};
use torus_state::action_status::CoreWriterKind;
use torus_state::cf::CF_CORE_WRITER_QUEUE;
use torus_state::{StateBackend, StateDb};
use torus_types::FixedPoint;

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

fn enqueue(db: &StateDb, trader: u8, kind: QueuedActionKind, block_queued: u64) -> u64 {
    CoreWriterQueue::enqueue(
        db,
        &QueuedAction {
            trader: addr(trader),
            kind,
            block_queued,
        },
    )
    .unwrap()
}

fn key(block: u64, seq: u64) -> Vec<u8> {
    let mut k = block.to_be_bytes().to_vec();
    k.extend_from_slice(&seq.to_be_bytes());
    k
}

/// Block 2's queue, six rows with mixed outcomes:
/// 0 lockbox deposit (executes), 1 undecodable, 2 a CancelAll whose row says
/// it was queued in block 2 itself (delay guard), 3 a stop PlaceOrder (does
/// not convert), 4 a lockbox withdraw without native balance, 5 a cancel of
/// an order that does not exist.
fn seed(db: &StateDb) {
    assert_eq!(enqueue(db, 7, QueuedActionKind::LockboxDeposit { amount: fp(5) }, 1), 0);
    db.put_cf_raw(CF_CORE_WRITER_QUEUE, &key(2, 1), &[0xFF, 0x00, 0x01])
        .unwrap();
    // Queued "in block 2": enqueue files it under block 3; move it to (2, 2).
    assert_eq!(enqueue(db, 12, QueuedActionKind::CancelAll { market_id: 1 }, 2), 0);
    let bytes = db.get_cf_raw(CF_CORE_WRITER_QUEUE, &key(3, 0)).unwrap().unwrap();
    db.delete_cf_raw(CF_CORE_WRITER_QUEUE, &key(3, 0)).unwrap();
    db.put_cf_raw(CF_CORE_WRITER_QUEUE, &key(2, 2), &bytes).unwrap();
    let stop = QueuedActionKind::PlaceOrder {
        market_id: 1,
        side: 0,
        order_type: 2,
        price: fp(100),
        quantity: fp(1),
        time_in_force: 0,
    };
    assert_eq!(enqueue(db, 11, stop, 1), 3);
    assert_eq!(enqueue(db, 9, QueuedActionKind::LockboxWithdraw { amount: fp(100) }, 1), 4);
    assert_eq!(enqueue(db, 10, QueuedActionKind::CancelOrder { order_id: 12_345 }, 1), 5);
}

fn ctx(db: &StateDb) -> NativeExecContext<StateDb> {
    NativeExecContext::new(
        db.clone(),
        2,
        1_002,
        0,
        100,
        10,
        addr(99),
        addr(100),
        addr(101),
    )
}

/// RED before: the entries carried no identity (seq 0, kind unknown, zero
/// trader) for every row.
#[test]
fn drain_entries_carry_each_rows_identity_and_result_in_queue_order() {
    let (_dir, db) = open_test_db();
    seed(&db);
    let mut ctx = ctx(&db);
    let entries = NativeExecutor::drain_core_writer_entries(&mut ctx).expect("queue reads");
    assert_eq!(ctx.take_fatal_error(), None);
    let got: Vec<_> = entries
        .iter()
        .map(|e| (e.seq, e.kind, e.trader, e.queued_block, e.result.success))
        .collect();
    use CoreWriterKind as K;
    assert_eq!(
        got,
        vec![
            (0, K::LockboxDeposit, addr(7), 1, true),
            (1, K::Undecodable, Address::ZERO, 0, false),
            (2, K::CancelAll, addr(12), 2, false),
            (3, K::PlaceOrder, addr(11), 1, false),
            (4, K::LockboxWithdraw, addr(9), 1, false),
            (5, K::CancelOrder, addr(10), 1, false),
        ]
    );
    let msg = |i: usize| entries[i].result.error.as_deref().unwrap_or_default().to_string();
    assert!(msg(1).starts_with("undecodable queued action (block 2, seq 1): "), "{}", msg(1));
    assert_eq!(msg(2), "action queued in block 2 cannot execute in block 2");
    assert!(msg(3).contains("unsupported CoreWriter order_type 2"), "{}", msg(3));
    assert!(!msg(4).is_empty() && !msg(5).is_empty());
    assert!(
        StateBackend::iterate_cf(&db, CF_CORE_WRITER_QUEUE, None)
            .unwrap()
            .is_empty(),
        "every row of block 2 deleted"
    );
}

/// `drain_core_writer` is the entries' results, unchanged.
#[test]
fn drain_core_writer_is_the_entries_results() {
    let run = |entries: bool| {
        let (dir, db) = open_test_db();
        seed(&db);
        let mut ctx = ctx(&db);
        let results: Vec<_> = if entries {
            NativeExecutor::drain_core_writer_entries(&mut ctx)
                .unwrap()
                .into_iter()
                .map(|e| e.result)
                .collect()
        } else {
            NativeExecutor::drain_core_writer(&mut ctx).unwrap()
        };
        drop(ctx);
        let out: Vec<_> = results
            .into_iter()
            .map(|r| (r.action_type, r.success, r.reason, r.error.map(String::from)))
            .collect();
        (dir, out)
    };
    assert_eq!(run(true).1, run(false).1);
}
