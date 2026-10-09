//! R02 (audit main-chain-deep-review-2026-10-08, option A, branch 1): a
//! LOCAL storage / decode fault reported as a `CoreError` latches
//! `ctx.fatal_error` through `NativeExecutor::latch_core_fault`, so the node
//! fail-stops before the block's flush instead of executing on a default.
//! Errors every validator hits alike never latch. The block-level proof (a
//! failed CoreWriter queue read stops the block) is in torus-consensus
//! `app.rs` (`r02_core_writer_queue_read_fault_fail_stops_block`).

use alloy_primitives::Address;

use torus_bridge::native_executor::{NativeExecContext, NativeExecutor};
use torus_core::error::CoreError;
use torus_core::precompiles::{CoreWriterQueue, QueuedAction, QueuedActionKind};
use torus_state::cf::CF_CORE_WRITER_QUEUE;
use torus_state::{AtomicWriteOp, StateBackend, StateDb, StateError};
use torus_types::FixedPoint;

/// A backend whose scans of one column family fail (an injected read fault).
#[derive(Clone)]
struct FailingScan<B: StateBackend> {
    inner: B,
    cf: &'static str,
}

impl<B: StateBackend> StateBackend for FailingScan<B> {
    fn get_cf_raw(&self, cf: &str, key: &[u8]) -> Result<Option<Vec<u8>>, StateError> {
        self.inner.get_cf_raw(cf, key)
    }
    fn put_cf_raw(&self, cf: &str, key: &[u8], value: &[u8]) -> Result<(), StateError> {
        self.inner.put_cf_raw(cf, key, value)
    }
    fn delete_cf_raw(&self, cf: &str, key: &[u8]) -> Result<(), StateError> {
        self.inner.delete_cf_raw(cf, key)
    }
    fn iterate_cf(
        &self,
        cf: &str,
        prefix: Option<&[u8]>,
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>, StateError> {
        if cf == self.cf {
            return Err(StateError::Io(std::io::Error::other(
                "injected read failure",
            )));
        }
        self.inner.iterate_cf(cf, prefix)
    }
    fn atomic_write(&self, ops: &[AtomicWriteOp<'_>]) -> Result<(), StateError> {
        self.inner.atomic_write(ops)
    }
}

fn open_test_db() -> (tempfile::TempDir, StateDb) {
    let dir = tempfile::tempdir().expect("create temp dir");
    let db = StateDb::open(dir.path()).expect("open db");
    (dir, db)
}

fn make_ctx<B: StateBackend>(state: B, height: u64) -> NativeExecContext<B> {
    let a = |n: u8| Address::new([n; 20]);
    NativeExecContext::new(state, height, 1000, 0, 100, 10, a(99), a(100), a(101))
}

fn local_fault(msg: &str) -> CoreError {
    CoreError::State(StateError::InvalidData(msg.into()))
}

#[test]
fn latch_core_fault_latches_a_local_fault() {
    let (_dir, db) = open_test_db();
    let mut ctx = make_ctx(db, 1);
    NativeExecutor::latch_core_fault(&mut ctx, "step", &local_fault("disk"));
    assert_eq!(
        ctx.fatal_error.as_deref(),
        Some("step: state error: invalid data: disk")
    );
}

#[test]
fn latch_core_fault_ignores_a_protocol_error() {
    let (_dir, db) = open_test_db();
    let mut ctx = make_ctx(db, 1);
    for e in [
        CoreError::InsufficientMargin {
            required: FixedPoint::ONE,
            available: FixedPoint::ZERO,
        },
        CoreError::NoOraclePrice(1),
        CoreError::MarketNotFound(1),
        CoreError::Overflow("x".into()),
    ] {
        NativeExecutor::latch_core_fault(&mut ctx, "step", &e);
        assert_eq!(ctx.fatal_error, None, "{e}");
    }
}

#[test]
fn latch_core_fault_keeps_the_first_fault() {
    let (_dir, db) = open_test_db();
    let mut ctx = make_ctx(db, 1);
    NativeExecutor::latch_core_fault(&mut ctx, "first", &local_fault("a"));
    NativeExecutor::latch_core_fault(&mut ctx, "second", &CoreError::MissingCf("cf"));
    assert_eq!(
        ctx.fatal_error.as_deref(),
        Some("first: state error: invalid data: a")
    );
}

/// The error the app's CoreWriter step receives: a failed queue read is a
/// `CoreError::State`, i.e. a local fault (`drain_core_writer` itself does not
/// latch; its caller does).
#[test]
fn drain_core_writer_surfaces_a_queue_read_fault_as_a_local_fault() {
    let (_dir, db) = open_test_db();
    CoreWriterQueue::enqueue(
        &db,
        &QueuedAction {
            trader: Address::new([7; 20]),
            kind: QueuedActionKind::LockboxDeposit {
                amount: FixedPoint::ONE,
            },
            block_queued: 1,
        },
    )
    .unwrap();
    let state = FailingScan {
        inner: db.clone(),
        cf: CF_CORE_WRITER_QUEUE,
    };
    let mut ctx = make_ctx(state, 2);
    let err = NativeExecutor::drain_core_writer(&mut ctx).expect_err("the queue read failed");
    assert!(matches!(err, CoreError::State(StateError::Io(_))), "{err}");
    assert!(err.is_local_fault());
    assert_eq!(
        StateBackend::iterate_cf(&db, CF_CORE_WRITER_QUEUE, None)
            .unwrap()
            .len(),
        1,
        "nothing drained"
    );
}
