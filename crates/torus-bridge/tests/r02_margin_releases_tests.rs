//! R02 branch 4 (audit main-chain-deep-review-2026-10-08, inventory section
//! (4) item 4): the order-margin releases. A LOCAL fault on the balance read
//! of a release (the read failed on this node, or the balance row does not
//! decode), or a failed release write, must fail-stop the block (returned by
//! `take_fatal_error`, the committer's check), never release nothing:
//! - `release_order_margin` (cancel / cancel-all / the single path's taker
//!   and maker releases / triggered stops / modify): read and write;
//! - the batch settle's maker (A5 / STP) release, sequential AND parallel
//!   settle (pinned per call, never `TORUS_PARALLEL_SETTLE`).
//!
//! The batch settle's taker release and `sell_top_ups` read a sender whose
//! balance Phase 2 always caches, so no backend fault reaches them through
//! the public API; their fault tests are unit tests in the crate
//! (`src/r02_margin_release_unit_tests.rs`).
//!
//! Healthy nodes keep today's results and state byte for byte (the
//! no-fault controls below pass before and after the fix).

use std::sync::{Arc, Mutex};

use alloy_primitives::Address;

use torus_bridge::native_executor::{NativeExecContext, NativeExecutor};
use torus_core::position::NativeBalance;
use torus_state::cf::CF_NATIVE_BALANCES;
use torus_state::{AtomicWriteOp, StateBackend, StateDb, StateError};
use torus_types::{FixedPoint, MarketId, NativeAction, OrderType, PlaceOrderParams, TimeInForce};

/// Armed `(cf, key)` rows.
type Armed = Arc<Mutex<Vec<(&'static str, Vec<u8>)>>>;

/// A backend whose point reads (`reads`) or writes (`writes`) of the armed
/// `(cf, key)` rows fail; `hits` counts the injected failures. Everything
/// else delegates.
#[derive(Clone)]
struct Failing {
    inner: StateDb,
    reads: Armed,
    writes: Armed,
    hits: Arc<Mutex<usize>>,
}

impl Failing {
    fn arm_read(&self, cf: &'static str, key: &[u8]) {
        self.reads.lock().unwrap().push((cf, key.to_vec()));
    }
    fn arm_write(&self, cf: &'static str, key: &[u8]) {
        self.writes.lock().unwrap().push((cf, key.to_vec()));
    }
    fn hits(&self) -> usize {
        *self.hits.lock().unwrap()
    }
    fn armed(list: &Mutex<Vec<(&'static str, Vec<u8>)>>, cf: &str, key: &[u8]) -> bool {
        list.lock()
            .unwrap()
            .iter()
            .any(|(c, k)| *c == cf && k.as_slice() == key)
    }
}

impl StateBackend for Failing {
    fn get_cf_raw(&self, cf: &str, key: &[u8]) -> Result<Option<Vec<u8>>, StateError> {
        if Self::armed(&self.reads, cf, key) {
            *self.hits.lock().unwrap() += 1;
            return Err(StateError::Io(std::io::Error::other(
                "injected release read failure",
            )));
        }
        self.inner.get_cf_raw(cf, key)
    }
    fn put_cf_raw(&self, cf: &str, key: &[u8], value: &[u8]) -> Result<(), StateError> {
        if Self::armed(&self.writes, cf, key) {
            *self.hits.lock().unwrap() += 1;
            return Err(StateError::Io(std::io::Error::other(
                "injected release write failure",
            )));
        }
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
        self.inner.iterate_cf(cf, prefix)
    }
    fn atomic_write(&self, ops: &[AtomicWriteOp<'_>]) -> Result<(), StateError> {
        self.inner.atomic_write(ops)
    }
}

const NOW: u64 = 1_001;

fn addr(n: u8) -> Address {
    Address::new([n; 20])
}

fn fp(v: i64) -> FixedPoint {
    FixedPoint::from_raw(v as i128 * FixedPoint::SCALE)
}

type Ctx = NativeExecContext<Failing>;

/// Traders 1..=4 rich. Block 2 at `NOW`, nothing armed.
fn setup() -> (tempfile::TempDir, StateDb, Failing, Ctx) {
    let dir = tempfile::tempdir().unwrap();
    let db = StateDb::open(dir.path()).unwrap();
    let backend = Failing {
        inner: db.clone(),
        reads: Arc::default(),
        writes: Arc::default(),
        hits: Arc::default(),
    };
    let ctx = NativeExecContext::new(
        backend.clone(),
        2,
        NOW,
        0,
        1_000,
        10,
        addr(99),
        addr(100),
        addr(101),
    );
    for t in 1..=4 {
        ctx.positions
            .put_native_balance(
                &addr(t),
                &NativeBalance {
                    available: fp(1_000_000),
                    order_margin: FixedPoint::ZERO,
                },
            )
            .unwrap();
    }
    (dir, db, backend, ctx)
}

fn order(
    trader: u8,
    market: MarketId,
    is_buy: bool,
    price: i64,
    qty: i64,
) -> (Address, NativeAction) {
    (
        addr(trader),
        NativeAction::PlaceOrder(PlaceOrderParams {
            market_id: market,
            is_buy,
            price: fp(price),
            quantity: fp(qty),
            order_type: OrderType::Limit,
            time_in_force: TimeInForce::GTC,
            reduce_only: false,
            client_order_id: None,
        }),
    )
}

fn cancel_all(trader: u8) -> (Address, NativeAction) {
    (
        addr(trader),
        NativeAction::CancelAllOrders { market_id: None },
    )
}

/// How a block runs: the single-action path, or a batch with the Phase-1
/// cancel batching pinned, or a batch with the settle loop pinned.
#[derive(Clone, Copy, Debug)]
enum Mode {
    Single,
    CancelBatch(bool),
    Settle { parallel: bool },
}

/// Each action's `(success, error)`.
type Outcome = Vec<(bool, Option<String>)>;

fn exec(ctx: &mut Ctx, mode: Mode, block: &[(Address, NativeAction)]) -> Outcome {
    let r: Vec<_> = match mode {
        Mode::Single => block
            .iter()
            .map(|(s, a)| NativeExecutor::execute(ctx, s, a))
            .collect(),
        Mode::CancelBatch(cb) => NativeExecutor::execute_batch_cancel_mode(ctx, block, cb).results,
        Mode::Settle { parallel } => {
            NativeExecutor::execute_batch_settle_workers(ctx, block, parallel, Some(2)).results
        }
    };
    r.into_iter()
        .map(|a| (a.success, a.error.map(String::from)))
        .collect()
}

/// The block fail-stops on the injected fault, recorded by `step` (the site).
fn assert_injected(reason: Option<String>, step: &str, injected: &str, what: &str) {
    let reason = reason.unwrap_or_else(|| panic!("{what}: must fail-stop"));
    assert!(
        reason.starts_with(step),
        "{what}: recorded by {step}: {reason}"
    );
    assert!(reason.contains(injected), "{what}: {reason}");
}

fn bal(db: &StateDb, trader: u8) -> NativeBalance {
    let ctx = NativeExecContext::new(
        db.clone(),
        2,
        NOW,
        0,
        1_000,
        10,
        addr(99),
        addr(100),
        addr(101),
    );
    ctx.positions.get_native_balance(&addr(trader)).unwrap()
}

const CANCEL_MODES: [Mode; 3] = [
    Mode::Single,
    Mode::CancelBatch(false),
    Mode::CancelBatch(true),
];

/// Trader 1 rests a GTC bid on market 1 (order margin held).
fn rest_bid(ctx: &mut Ctx) {
    assert!(
        exec(ctx, Mode::Single, &[order(1, 1, true, 100, 1)])[0].0,
        "bid rests"
    );
    assert_eq!(ctx.take_fatal_error(), None);
}

// ---------------------------------------------------------------------------
// release_order_margin (through cancel-all).
// ---------------------------------------------------------------------------

/// The release's balance read fails. RED before R02: nothing released, the
/// block went on.
#[test]
fn r02_release_order_margin_read_fault_fail_stops() {
    for mode in CANCEL_MODES {
        let (_d, _db, backend, mut ctx) = setup();
        rest_bid(&mut ctx);
        backend.arm_read(CF_NATIVE_BALANCES, addr(1).as_slice());
        exec(&mut ctx, mode, &[cancel_all(1)]);
        assert!(backend.hits() > 0, "{mode:?}: fault injected");
        assert_injected(
            ctx.take_fatal_error(),
            "release order margin balance read",
            "injected release read failure",
            &format!("{mode:?}"),
        );
        assert_eq!(ctx.take_fatal_error(), None, "{mode:?}: taken once");
    }
}

/// The release's balance write fails. RED before R02: the `Result` was
/// discarded (`let _ =`).
#[test]
fn r02_release_order_margin_write_fault_fail_stops() {
    for mode in CANCEL_MODES {
        let (_d, _db, backend, mut ctx) = setup();
        rest_bid(&mut ctx);
        backend.arm_write(CF_NATIVE_BALANCES, addr(1).as_slice());
        exec(&mut ctx, mode, &[cancel_all(1)]);
        assert!(backend.hits() > 0, "{mode:?}: fault injected");
        assert_injected(
            ctx.take_fatal_error(),
            "release order margin balance write",
            "injected release write failure",
            &format!("{mode:?}"),
        );
    }
}

/// An undecodable balance row on the release (a local fault: the row is
/// only ever written by `put_native_balance`). RED before R02: skipped.
#[test]
fn r02_release_order_margin_undecodable_balance_fail_stops() {
    for mode in CANCEL_MODES {
        let (_d, db, _backend, mut ctx) = setup();
        rest_bid(&mut ctx);
        db.put_cf_raw(CF_NATIVE_BALANCES, addr(1).as_slice(), b"x")
            .unwrap();
        exec(&mut ctx, mode, &[cancel_all(1)]);
        let reason = ctx
            .take_fatal_error()
            .unwrap_or_else(|| panic!("{mode:?}: must fail-stop"));
        assert!(
            reason.starts_with("release order margin balance read: borsh"),
            "{mode:?}: {reason}"
        );
    }
}

/// Control (no fault): cancel-all releases the bid's whole reservation, as
/// today; an absent balance row (trader 5, never funded, nothing held)
/// releases nothing and does not fail-stop.
#[test]
fn r02_release_order_margin_without_fault_unchanged() {
    for mode in CANCEL_MODES {
        let (_d, db, backend, mut ctx) = setup();
        rest_bid(&mut ctx);
        let held = bal(&db, 1);
        assert!(held.order_margin > FixedPoint::ZERO, "{mode:?}: reserved");
        let r = exec(&mut ctx, mode, &[cancel_all(1), cancel_all(5)]);
        assert!(r.iter().all(|x| x.0), "{mode:?}: {r:?}");
        assert_eq!(ctx.take_fatal_error(), None, "{mode:?}");
        assert_eq!(backend.hits(), 0);
        let after = bal(&db, 1);
        assert_eq!(after.order_margin, FixedPoint::ZERO, "{mode:?}");
        assert_eq!(after.available, fp(1_000_000), "{mode:?}");
        assert_eq!(
            db.get_cf_raw(CF_NATIVE_BALANCES, addr(5).as_slice())
                .unwrap(),
            None,
            "{mode:?}: no row written"
        );
    }
}

// ---------------------------------------------------------------------------
// The maker release (single path: release_order_margin; batch: settle).
// ---------------------------------------------------------------------------

const FILL_MODES: [Mode; 3] = [
    Mode::Single,
    Mode::Settle { parallel: false },
    Mode::Settle { parallel: true },
];

/// Trader 2 rests an ask at 100 on market 1; trader 3 rests a bid on
/// market 2 (so a batch has work in two markets: parallel settle engages).
fn rest_maker(ctx: &mut Ctx) {
    let r = exec(
        ctx,
        Mode::Single,
        &[order(2, 1, false, 100, 1), order(3, 2, true, 50, 1)],
    );
    assert!(r.iter().all(|x| x.0), "{r:?}");
    assert_eq!(ctx.take_fatal_error(), None);
}

/// Trader 1 lifts the ask; trader 4 bids on market 2 (rests).
fn fill_block() -> Vec<(Address, NativeAction)> {
    vec![order(1, 1, true, 100, 1), order(4, 2, true, 40, 1)]
}

/// Which settle loop ran (the parallel one times its pass A; the
/// sequential one leaves it 0), and no fallback to sequential.
fn assert_settle_path(ctx: &Ctx, mode: Mode) {
    if let Mode::Settle { parallel } = mode {
        assert_eq!(
            ctx.phase_accum.settle_pass_a_ns > 0,
            parallel,
            "{mode:?}: settle path pinned"
        );
        assert_eq!(ctx.phase_accum.settle_fallbacks, 0, "{mode:?}: no fallback");
    }
}

/// The maker's balance read on the release of its consumed reservation
/// fails. RED before R02 (every mode): nothing released.
#[test]
fn r02_maker_release_read_fault_fail_stops() {
    for mode in FILL_MODES {
        let (_d, _db, backend, mut ctx) = setup();
        rest_maker(&mut ctx);
        backend.arm_read(CF_NATIVE_BALANCES, addr(2).as_slice());
        exec(&mut ctx, mode, &fill_block());
        assert!(backend.hits() > 0, "{mode:?}: fault injected");
        assert_settle_path(&ctx, mode);
        let step = match mode {
            Mode::Single => "release order margin balance read",
            _ => "settle maker release balance read",
        };
        assert_injected(
            ctx.take_fatal_error(),
            step,
            "injected release read failure",
            &format!("{mode:?}"),
        );
    }
}

/// Single path: the maker release's write fails. RED before R02.
#[test]
fn r02_maker_release_write_fault_fail_stops_single() {
    let (_d, _db, backend, mut ctx) = setup();
    rest_maker(&mut ctx);
    backend.arm_write(CF_NATIVE_BALANCES, addr(2).as_slice());
    exec(&mut ctx, Mode::Single, &fill_block());
    assert!(backend.hits() > 0, "fault injected");
    assert_injected(
        ctx.take_fatal_error(),
        "release order margin balance write",
        "injected release write failure",
        "single",
    );
}

/// An undecodable maker balance row (a local fault). RED before R02.
#[test]
fn r02_maker_release_undecodable_balance_fail_stops() {
    for mode in FILL_MODES {
        let (_d, db, _backend, mut ctx) = setup();
        rest_maker(&mut ctx);
        db.put_cf_raw(CF_NATIVE_BALANCES, addr(2).as_slice(), b"x")
            .unwrap();
        exec(&mut ctx, mode, &fill_block());
        assert_settle_path(&ctx, mode);
        let reason = ctx
            .take_fatal_error()
            .unwrap_or_else(|| panic!("{mode:?}: must fail-stop"));
        let step = if matches!(mode, Mode::Single) {
            "release order margin"
        } else {
            "settle maker release"
        };
        assert!(
            reason.starts_with(&format!("{step} balance read: borsh")),
            "{mode:?}: {reason}"
        );
    }
}

/// Control (no fault): the fill and every balance are the same in all three
/// modes (the maker's consumed reservation released, the taker's too);
/// nothing fail-stops.
#[test]
fn r02_maker_release_without_fault_unchanged() {
    let mut seen: Option<(Outcome, Vec<(FixedPoint, FixedPoint)>)> = None;
    for mode in FILL_MODES {
        let (_d, db, backend, mut ctx) = setup();
        rest_maker(&mut ctx);
        let r = exec(&mut ctx, mode, &fill_block());
        assert!(r.iter().all(|x| x.0), "{mode:?}: {r:?}");
        assert_settle_path(&ctx, mode);
        assert_eq!(ctx.take_fatal_error(), None, "{mode:?}");
        assert_eq!(backend.hits(), 0);
        let bals: Vec<_> = (1..=4)
            .map(|t| bal(&db, t))
            .map(|b| (b.available, b.order_margin))
            .collect();
        assert_eq!(
            bals[0].1,
            FixedPoint::ZERO,
            "{mode:?}: taker filled, nothing held"
        );
        assert_eq!(
            bals[1].1,
            FixedPoint::ZERO,
            "{mode:?}: maker consumed, released"
        );
        match &seen {
            None => seen = Some((r, bals)),
            Some((r0, b0)) => {
                assert_eq!(&r, r0, "{mode:?}: results");
                assert_eq!(&bals, b0, "{mode:?}: balances");
            }
        }
    }
}
