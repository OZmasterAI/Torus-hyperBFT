//! A-exception (owner s111, R02 item 8): the CoreWriter drain never silently
//! drops an undecodable queued row. `NativeExecutor::drain_core_writer`
//! returns a failed `core_writer` result at the row's queue position (the
//! same result mechanism as the drain's other refusals: the delay guard, an
//! unconvertible action), the row is deleted with the rest of the block's
//! queue, its valid neighbours execute in order, and nothing fail-stops (a
//! decode error is deterministic, owner decision A). A storage fault in the
//! drain still fail-stops: `r02_fail_stop_tests.rs`
//! (`drain_core_writer_read_fault_with_an_undecodable_row_still_fails_stop`)
//! and the block-level proofs in torus-consensus `app.rs`.

use alloy_primitives::Address;

use torus_bridge::native_executor::{NativeExecContext, NativeExecutor};
use torus_core::position::PositionManager;
use torus_core::precompiles::{CoreWriterQueue, QueuedAction, QueuedActionKind};
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

fn deposit(db: &StateDb, trader: u8, amount: i64) -> u64 {
    CoreWriterQueue::enqueue(
        db,
        &QueuedAction {
            trader: addr(trader),
            kind: QueuedActionKind::LockboxDeposit { amount: fp(amount) },
            block_queued: 1,
        },
    )
    .unwrap()
}

/// Block 2's queue: deposit (seq 0), an undecodable row (seq 1), deposit (seq 2).
fn seed(db: &StateDb) {
    assert_eq!(deposit(db, 7, 5), 0);
    let mut key = 2u64.to_be_bytes().to_vec();
    key.extend_from_slice(&1u64.to_be_bytes());
    db.put_cf_raw(CF_CORE_WRITER_QUEUE, &key, &[0xFF, 0x00, 0x01])
        .unwrap();
    assert_eq!(deposit(db, 8, 3), 2);
}

fn available(db: &StateDb, trader: u8) -> FixedPoint {
    PositionManager::new(db.clone())
        .get_native_balance(&addr(trader))
        .unwrap()
        .available
}

/// RED before: two results (the bad row vanished without a trace).
#[test]
fn undecodable_queue_row_is_a_failed_result_in_place_and_neighbours_execute() {
    let (_dir, db) = open_test_db();
    seed(&db);
    let mut ctx = NativeExecContext::new(
        db.clone(),
        2,
        1_002,
        0,
        100,
        10,
        addr(99),
        addr(100),
        addr(101),
    );
    let results = NativeExecutor::drain_core_writer(&mut ctx).expect("queue reads");
    assert_eq!(results.len(), 3, "one result per queued row: {results:?}");

    assert!(results[0].success, "{:?}", results[0]);
    assert_eq!(results[0].action_type, "lockbox_deposit");

    let bad = &results[1];
    assert!(!bad.success, "{bad:?}");
    assert_eq!(bad.action_type, "core_writer");
    let msg = bad.error.as_deref().unwrap_or_default();
    assert!(
        msg.starts_with("undecodable queued action (block 2, seq 1): "),
        "{msg}"
    );

    assert!(results[2].success, "{:?}", results[2]);
    assert_eq!(results[2].action_type, "lockbox_deposit");

    assert_eq!(
        ctx.take_fatal_error(),
        None,
        "a decode error never fail-stops"
    );
    assert_eq!(available(&db, 7), fp(5));
    assert_eq!(available(&db, 8), fp(3));
    assert!(
        StateBackend::iterate_cf(&db, CF_CORE_WRITER_QUEUE, None)
            .unwrap()
            .is_empty(),
        "every row of block 2 deleted, the undecodable one included"
    );
}

/// Same queue on two nodes: the same results and the same state.
#[test]
fn undecodable_queue_row_drains_identically_on_two_nodes() {
    let run = || {
        let (dir, db) = open_test_db();
        seed(&db);
        let mut ctx = NativeExecContext::new(
            db.clone(),
            2,
            1_002,
            0,
            100,
            10,
            addr(99),
            addr(100),
            addr(101),
        );
        let results: Vec<_> = NativeExecutor::drain_core_writer(&mut ctx)
            .unwrap()
            .into_iter()
            .map(|r| (r.action_type, r.success, r.error.map(String::from)))
            .collect();
        drop(ctx);
        let balances = (available(&db, 7), available(&db, 8));
        (dir, results, balances)
    };
    let (_a, ra, ba) = run();
    let (_b, rb, bb) = run();
    assert_eq!(ra, rb);
    assert_eq!(ba, bb);
}
