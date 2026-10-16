//! Item 7 step 2 (B7): `NativeAction::EvmUserModify` sets or clears the sender's EVM lane row
//! `"lan" ‖ address` -> `[1]` in `CF_NATIVE_ORACLE`. An unchanged lane writes nothing.
//! `docs/plans/item7-step2-impl.md` B7.

use alloy_primitives::Address;

use torus_bridge::native_executor::{NativeActionResult, NativeExecContext, NativeExecutor};
use torus_state::backend::NativeStateOverlay;
use torus_state::cf::{evm_lane_key, CF_NATIVE_ORACLE};
use torus_state::{StateBackend, StateDb};
use torus_types::NativeAction;

fn open_test_db() -> (tempfile::TempDir, StateDb) {
    let dir = tempfile::tempdir().expect("create temp dir");
    let db = StateDb::open(dir.path()).expect("open db");
    (dir, db)
}

fn addr(n: u8) -> Address {
    Address::new([n; 20])
}

fn ctx_at<T: StateBackend>(state: T, height: u64) -> NativeExecContext<T> {
    NativeExecContext::new(
        state,
        height,
        1_000 + height,
        0,
        1_000,
        10,
        addr(99),
        addr(100),
        addr(101),
    )
}

/// Run one `EvmUserModify` from `sender` in a fresh overlay over `db`; returns the result and
/// the overlay's pending write count, then flushes the overlay into `db`.
fn modify(db: &StateDb, height: u64, sender: Address, big: bool) -> (NativeActionResult, usize) {
    let overlay = NativeStateOverlay::new(db.clone());
    let action = NativeAction::EvmUserModify {
        using_big_blocks: big,
    };
    let r = NativeExecutor::execute(&mut ctx_at(overlay.clone(), height), &sender, &action);
    let writes = overlay.pending_write_count();
    overlay.flush(db).unwrap();
    (r, writes)
}

fn lane_row(db: &StateDb, a: Address) -> Option<Vec<u8>> {
    db.get_cf_raw(CF_NATIVE_ORACLE, &evm_lane_key(&a)).unwrap()
}

#[test]
fn evm_user_modify_sets_and_clears_the_lane_row() {
    let (_dir, db) = open_test_db();
    let a = addr(7);
    assert_eq!(lane_row(&db, a), None);

    let (r, writes) = modify(&db, 1, a, true);
    assert!(r.success, "{:?}", r.error);
    assert_eq!(writes, 1);
    assert_eq!(lane_row(&db, a), Some(vec![1]));

    // Unchanged lane: success, no write.
    let (r, writes) = modify(&db, 2, a, true);
    assert!(r.success, "{:?}", r.error);
    assert_eq!(writes, 0, "true again must write nothing");
    assert_eq!(lane_row(&db, a), Some(vec![1]));

    let (r, writes) = modify(&db, 3, a, false);
    assert!(r.success, "{:?}", r.error);
    assert_eq!(writes, 1);
    assert_eq!(lane_row(&db, a), None);

    let (r, writes) = modify(&db, 4, a, false);
    assert!(r.success, "{:?}", r.error);
    assert_eq!(writes, 0, "false again must write nothing");
    assert_eq!(lane_row(&db, a), None);

    // Another sender's row is its own.
    assert!(modify(&db, 5, addr(8), true).0.success);
    assert_eq!(lane_row(&db, a), None);
    assert_eq!(lane_row(&db, addr(8)), Some(vec![1]));
}
