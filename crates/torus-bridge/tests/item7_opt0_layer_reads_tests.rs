//! Item 7 option 0, step 1 (G1b): the readers that used to assume "EVM blocks
//! are serial barriers, so the DB is current" read through a backend, so a
//! row held only in an in-flight layer (the parent's frozen set, L, C) is
//! seen, and a row a layer tombstoned is not.
//!
//! - task 1.3: `NativeExecutor::core_writer_due` over the block overlay
//!   (design test 3c);
//! - task 1.4: the lane read of `validate_block_for_catchup` is pinned in
//!   `evm_lanes_tests` (`lane_row_in_parent_layer_sets_big_fee`, next to its
//!   block helpers), the enqueue reads in torus-core `precompile_tests`
//!   (`enqueue_reads_cursor_rows_from_layers`).

use std::sync::Arc;

use torus_bridge::native_executor::NativeExecutor;
use torus_state::cf::CF_CORE_WRITER_QUEUE;
use torus_state::checkpoint_layer::CheckpointLayer;
use torus_state::{NativeStateOverlay, StateBackend, StateDb};

fn open() -> (tempfile::TempDir, StateDb) {
    let dir = tempfile::tempdir().unwrap();
    let db = StateDb::open(dir.path()).unwrap();
    (dir, db)
}

fn queue_key(target: u64, seq: u64) -> [u8; 16] {
    let mut k = [0u8; 16];
    k[..8].copy_from_slice(&target.to_be_bytes());
    k[8..].copy_from_slice(&seq.to_be_bytes());
    k
}

/// A frozen set at `height` holding `f`'s writes (the parent layer / an L block).
fn frozen(
    db: &StateDb,
    height: u64,
    f: impl FnOnce(&NativeStateOverlay),
) -> Arc<torus_state::FrozenPending> {
    let ov = NativeStateOverlay::new(db.clone());
    f(&ov);
    ov.freeze(height)
}

/// Task 1.3 (design test 3c): the due-check reads the block overlay, not the
/// raw DB. A row only in the parent layer is due; a row the since-checkpoint
/// layer L tombstoned (drained) while the DB still holds it is not; a row
/// only in L is due; rows of other targets never count.
#[test]
fn core_writer_due_reads_layers() {
    const H: u64 = 7;

    // Parent only: the previous block queued a row for H, not durable yet.
    let (_d1, db) = open();
    let parent = frozen(&db, H - 1, |ov| {
        ov.put_cf_raw(CF_CORE_WRITER_QUEUE, &queue_key(H, 0), b"row")
            .unwrap();
    });
    assert!(
        !NativeExecutor::core_writer_due(&db, H).unwrap(),
        "the raw DB has no row"
    );
    let ov = NativeStateOverlay::with_parent(db.clone(), Some(parent));
    assert!(
        NativeExecutor::core_writer_due(&ov, H).unwrap(),
        "parent-layer row -> due"
    );
    assert!(
        !NativeExecutor::core_writer_due(&ov, H + 1).unwrap(),
        "other target"
    );

    // Drained in L, still in the DB: not due.
    let (_d2, db) = open();
    db.put_cf_raw(CF_CORE_WRITER_QUEUE, &queue_key(H, 0), b"row")
        .unwrap();
    db.put_cf_raw(CF_CORE_WRITER_QUEUE, &queue_key(H + 1, 0), b"next")
        .unwrap();
    let drained = frozen(&db, H - 2, |ov| {
        ov.delete_cf_raw(CF_CORE_WRITER_QUEUE, &queue_key(H, 0))
            .unwrap();
    });
    let mut l = CheckpointLayer::new();
    l.merge(&drained);
    assert!(
        NativeExecutor::core_writer_due(&db, H).unwrap(),
        "the raw DB still has the row"
    );
    let ov = NativeStateOverlay::with_layers(db.clone(), None, Some(Arc::new(l)), None);
    assert!(
        !NativeExecutor::core_writer_due(&ov, H).unwrap(),
        "L tombstone -> not due"
    );
    assert!(
        NativeExecutor::core_writer_due(&ov, H + 1).unwrap(),
        "the next target's DB row"
    );

    // Only in L (merged, not durable): due.
    let (_d3, db) = open();
    let queued = frozen(&db, H - 3, |ov| {
        ov.put_cf_raw(CF_CORE_WRITER_QUEUE, &queue_key(H, 3), b"row")
            .unwrap();
    });
    let mut l = CheckpointLayer::new();
    l.merge(&queued);
    let ov = NativeStateOverlay::with_layers(db.clone(), None, Some(Arc::new(l)), None);
    assert!(
        NativeExecutor::core_writer_due(&ov, H).unwrap(),
        "L row -> due"
    );
    assert!(!NativeExecutor::core_writer_due(&db, H).unwrap());
}
