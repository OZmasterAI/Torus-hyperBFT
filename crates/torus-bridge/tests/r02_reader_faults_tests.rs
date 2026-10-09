//! R02 branch 5 (audit main-chain-deep-review-2026-10-08, owner option A):
//! the native executor's readers without a `Result` path. A LOCAL fault (the
//! read failed on this node, or a stored row of the reader's own type does
//! not decode) must fail-stop the block through the reader fault channel
//! (`ctx.reader_fault`, returned by `take_fatal_error`, the committer's
//! check), never be read as a default:
//! - `AccountReader::maker_position_px` (Phase 3 maker snapshot: flat);
//! - `reduce_only_positions_for` (a policed trader: flat);
//! - `phase2_reservation_basis` (reduce-only clamp: full reservation;
//!   closing allowance: none);
//! - `place_order_inner`'s never-rests position read (full reservation);
//! - `market_shape` / `book_shape` (a new book's tick / lot: `(ONE, ONE)`);
//! - `price_band_bps` (the governance band width: the ±50% default).
//!
//! Legitimate absence (no row) and the deterministic decode policies (an
//! undecodable market row is `(ONE, ONE)`, an invalid band value is the
//! default; every validator holds the same bytes) keep today's results and
//! never fail-stop. The block-level proof (exec_failed, nothing flushed,
//! marker unchanged) is in torus-consensus `app.rs` (`r02_*`).

use std::sync::{Arc, Mutex};

use alloy_primitives::Address;

use torus_bridge::native_executor::{NativeExecContext, NativeExecutor};
use torus_core::position::{position_key, MarginType, NativeBalance, Position};
use torus_state::cf::{CF_FEE_CONFIG, CF_NATIVE_MARKETS, CF_NATIVE_ORACLE, CF_NATIVE_POSITIONS};
use torus_state::{AtomicWriteOp, StateBackend, StateDb, StateError};
use torus_types::{FixedPoint, MarketId, NativeAction, OrderType, PlaceOrderParams, TimeInForce};

/// A backend whose point reads of the armed `(cf, key)` rows fail (an
/// injected read fault). Everything else delegates.
#[derive(Clone)]
struct FailingRead {
    inner: StateDb,
    armed: Arc<Mutex<Vec<(&'static str, Vec<u8>)>>>,
}

impl FailingRead {
    fn arm(&self, cf: &'static str, key: &[u8]) {
        self.armed.lock().unwrap().push((cf, key.to_vec()));
    }
}

impl StateBackend for FailingRead {
    fn get_cf_raw(&self, cf: &str, key: &[u8]) -> Result<Option<Vec<u8>>, StateError> {
        if self.armed.lock().unwrap().iter().any(|(c, k)| *c == cf && k.as_slice() == key) {
            return Err(StateError::Io(std::io::Error::other("injected reader read failure")));
        }
        self.inner.get_cf_raw(cf, key)
    }
    fn put_cf_raw(&self, cf: &str, key: &[u8], value: &[u8]) -> Result<(), StateError> {
        self.inner.put_cf_raw(cf, key, value)
    }
    fn delete_cf_raw(&self, cf: &str, key: &[u8]) -> Result<(), StateError> {
        self.inner.delete_cf_raw(cf, key)
    }
    fn iterate_cf(&self, cf: &str, prefix: Option<&[u8]>) -> Result<Vec<(Vec<u8>, Vec<u8>)>, StateError> {
        self.inner.iterate_cf(cf, prefix)
    }
    fn atomic_write(&self, ops: &[AtomicWriteOp<'_>]) -> Result<(), StateError> {
        self.inner.atomic_write(ops)
    }
}

const M: MarketId = 1;
const NOW: u64 = 1_001;
const BAND_KEY: &[u8] = torus_types::PRICE_BAND_PARAM.as_bytes();

fn addr(n: u8) -> Address {
    Address::new([n; 20])
}

fn fp(v: i64) -> FixedPoint {
    FixedPoint::from_raw(v as i128 * FixedPoint::SCALE)
}

type Ctx = NativeExecContext<FailingRead>;

/// Market 1's row (`market_row`; None: no row), traders 1..=4 rich. Block 2
/// at `NOW`, nothing armed.
fn setup(market_row: Option<&[u8]>) -> (tempfile::TempDir, StateDb, FailingRead, Ctx) {
    let dir = tempfile::tempdir().unwrap();
    let db = StateDb::open(dir.path()).unwrap();
    if let Some(row) = market_row {
        db.put_cf_raw(CF_NATIVE_MARKETS, &M.to_be_bytes(), row).unwrap();
    }
    let backend = FailingRead { inner: db.clone(), armed: Arc::default() };
    let ctx = NativeExecContext::new(backend.clone(), 2, NOW, 0, 1_000, 10, addr(99), addr(100), addr(101));
    for t in 1..=4 {
        ctx.positions
            .put_native_balance(&addr(t), &NativeBalance { available: fp(1_000_000), order_margin: FixedPoint::ZERO })
            .unwrap();
    }
    (dir, db, backend, ctx)
}

/// `trader` holds `size` (signed) of market 1 at 100.
fn put_position(ctx: &Ctx, trader: Address, size: i64) {
    ctx.positions
        .put_position(&Position {
            trader,
            market_id: M,
            is_long: size > 0,
            size: fp(size.abs()),
            entry_price: fp(100),
            cost_basis: fp(100 * size.abs()),
            realized_pnl: FixedPoint::ZERO,
            isolated_margin: FixedPoint::ZERO,
            margin_type: MarginType::Cross,
        })
        .unwrap();
}

fn order(trader: u8, is_buy: bool, price: i64, qty: i64, tif: TimeInForce, reduce_only: bool) -> (Address, NativeAction) {
    (
        addr(trader),
        NativeAction::PlaceOrder(PlaceOrderParams {
            market_id: M,
            is_buy,
            price: fp(price),
            quantity: fp(qty),
            order_type: OrderType::Limit,
            time_in_force: tif,
            reduce_only,
            client_order_id: None,
        }),
    )
}

/// `None` = the single-action path, `Some(t)` = `execute_batch_engine_mode(_, _, t)`.
const MODES: [Option<usize>; 4] = [None, Some(0), Some(2), Some(4)];

/// Each action's `(success, error)`.
type Outcome = Vec<(bool, Option<String>)>;

fn exec(ctx: &mut Ctx, mode: Option<usize>, block: &[(Address, NativeAction)]) -> Outcome {
    let r: Vec<_> = match mode {
        None => block.iter().map(|(s, a)| NativeExecutor::execute(ctx, s, a)).collect(),
        Some(t) => NativeExecutor::execute_batch_engine_mode(ctx, block, t).results,
    };
    r.into_iter().map(|a| (a.success, a.error.map(String::from))).collect()
}

/// The block fail-stops on the injected fault, recorded by `step` (the site).
fn assert_injected(reason: Option<String>, step: &str, what: &str) {
    let reason = reason.unwrap_or_else(|| panic!("{what}: must fail-stop"));
    assert!(reason.starts_with(step), "{what}: recorded by {step}: {reason}");
    assert!(reason.contains("injected reader read failure"), "{what}: {reason}");
}

/// The order margin `trader` holds after the block.
fn order_margin(ctx: &Ctx, trader: u8) -> FixedPoint {
    ctx.positions.get_native_balance(&addr(trader)).unwrap().order_margin
}

// ---------------------------------------------------------------------------
// Position reads.
// ---------------------------------------------------------------------------

/// The Phase 3 / single-path maker snapshot (`maker_position_px` through
/// `MakerAccountSource::maker_account`): the maker's position read fails.
/// RED before R02: the maker snapshotted as flat and the fill went on.
#[test]
fn r02_maker_position_read_fault_fail_stops() {
    for mode in MODES {
        let (_d, _db, backend, mut ctx) = setup(None);
        put_position(&ctx, addr(2), -1);
        assert!(exec(&mut ctx, None, &[order(2, false, 100, 1, TimeInForce::GTC, false)])[0].0, "maker rests");
        assert_eq!(ctx.take_fatal_error(), None);
        backend.arm(CF_NATIVE_POSITIONS, &position_key(&addr(2), M));
        exec(&mut ctx, mode, &[order(1, true, 100, 1, TimeInForce::GTC, false)]);
        assert_injected(ctx.take_fatal_error(), "maker position read", &format!("mode {mode:?}"));
        assert_eq!(ctx.take_fatal_error(), None, "mode {mode:?}: taken once");
    }
}

/// A maker position row that does not decode on this node (a `Position`
/// row is only ever written by `put_position`; Borsh failure = corruption,
/// a local fault). RED before R02: read as flat.
#[test]
fn r02_maker_position_undecodable_row_fail_stops() {
    for mode in MODES {
        let (_d, db, _backend, mut ctx) = setup(None);
        assert!(exec(&mut ctx, None, &[order(2, false, 100, 1, TimeInForce::GTC, false)])[0].0, "maker rests");
        db.put_cf_raw(CF_NATIVE_POSITIONS, &position_key(&addr(2), M), b"garbage").unwrap();
        exec(&mut ctx, mode, &[order(1, true, 100, 1, TimeInForce::GTC, false)]);
        let reason = ctx.take_fatal_error().unwrap_or_else(|| panic!("mode {mode:?}: must fail-stop"));
        assert!(reason.starts_with("maker position read: borsh"), "mode {mode:?}: {reason}");
    }
}

/// `reduce_only_positions_for`: a trader with a resting reduce-only order
/// is policed when another order hits the book; its position read fails.
/// RED before R02: policed as flat.
#[test]
fn r02_reduce_only_policed_position_read_fault_fail_stops() {
    for mode in MODES {
        let (_d, _db, backend, mut ctx) = setup(None);
        put_position(&ctx, addr(3), 1);
        assert!(exec(&mut ctx, None, &[order(3, false, 150, 1, TimeInForce::GTC, true)])[0].0, "RO ask rests");
        assert_eq!(ctx.take_fatal_error(), None);
        backend.arm(CF_NATIVE_POSITIONS, &position_key(&addr(3), M));
        exec(&mut ctx, mode, &[order(1, true, 90, 1, TimeInForce::GTC, false)]);
        assert_injected(ctx.take_fatal_error(), "reduce-only policed position read", &format!("mode {mode:?}"));
    }
}

/// A reduce-only order's own position read (single path: the pre-check,
/// an A-R reject on main, unchanged; batch: `phase2_reservation_basis`'s
/// clamp and Phase 2). Batch modes: the clamp read's fault is recorded.
/// RED before R02 in the batch modes: full reservation, no fail-stop.
#[test]
fn r02_reduce_only_sender_position_read_fault_fail_stops_in_batches() {
    for mode in MODES.into_iter().filter(Option::is_some) {
        let (_d, _db, backend, mut ctx) = setup(None);
        put_position(&ctx, addr(3), 1);
        backend.arm(CF_NATIVE_POSITIONS, &position_key(&addr(3), M));
        exec(&mut ctx, mode, &[order(3, false, 150, 1, TimeInForce::GTC, true)]);
        assert_injected(ctx.take_fatal_error(), "reduce-only reservation position read", &format!("mode {mode:?}"));
    }
}

/// An order that never rests (IOC), not reduce-only: its closing-allowance
/// position read (single: `place_order_inner`; batch:
/// `phase2_reservation_basis`) fails. RED before R02: charged the full
/// reservation, no fail-stop.
#[test]
fn r02_never_rests_position_read_fault_fail_stops() {
    for mode in MODES {
        let (_d, _db, backend, mut ctx) = setup(None);
        put_position(&ctx, addr(1), -1);
        backend.arm(CF_NATIVE_POSITIONS, &position_key(&addr(1), M));
        exec(&mut ctx, mode, &[order(1, true, 100, 1, TimeInForce::IOC, false)]);
        assert_injected(ctx.take_fatal_error(), if mode.is_none() { "never-rests position read" } else { "closing allowance position read" }, &format!("mode {mode:?}"));
    }
}

/// Regression (no fault): positions that exist and positions that do not
/// (flat) keep today's results, reservations and fills; nothing fail-stops.
#[test]
fn r02_position_reads_without_fault_unchanged() {
    for mode in MODES {
        let what = format!("mode {mode:?}");
        // Maker with and without a position row; the taker fills.
        for maker_pos in [None, Some(-1)] {
            let (_d, _db, _b, mut ctx) = setup(None);
            if let Some(s) = maker_pos {
                put_position(&ctx, addr(2), s);
            }
            assert!(exec(&mut ctx, None, &[order(2, false, 100, 1, TimeInForce::GTC, false)])[0].0);
            let r = exec(&mut ctx, mode, &[order(1, true, 100, 1, TimeInForce::GTC, false)]);
            assert!(r[0].0, "{what} maker {maker_pos:?}: {r:?}");
            assert_eq!(ctx.take_fatal_error(), None, "{what}");
            assert_eq!(order_margin(&ctx, 1), FixedPoint::ZERO, "{what}: filled, nothing reserved");
        }
        // A policed reduce-only trader; a flat one (no row).
        let (_d, _db, _b, mut ctx) = setup(None);
        put_position(&ctx, addr(3), 1);
        assert!(exec(&mut ctx, None, &[order(3, false, 150, 1, TimeInForce::GTC, true)])[0].0);
        assert!(exec(&mut ctx, mode, &[order(1, true, 90, 1, TimeInForce::GTC, false)])[0].0, "{what}");
        let r = exec(&mut ctx, mode, &[order(4, false, 150, 1, TimeInForce::GTC, true)]);
        assert!(!r[0].0, "{what}: flat reduce-only still rejected: {r:?}");
        assert_eq!(ctx.take_fatal_error(), None, "{what}");
        // An IOC buy that closes a short (no reservation for the closing part)
        // and one with no position (full reservation, released: nothing rests).
        for (pos, trader) in [(Some(-1), 1u8), (None, 4u8)] {
            let (_d, _db, _b, mut ctx) = setup(None);
            if let Some(s) = pos {
                put_position(&ctx, addr(trader), s);
            }
            let r = exec(&mut ctx, mode, &[order(trader, true, 100, 1, TimeInForce::IOC, false)]);
            assert_eq!(ctx.take_fatal_error(), None, "{what} pos {pos:?}: {r:?}");
            assert_eq!(order_margin(&ctx, trader), FixedPoint::ZERO, "{what} pos {pos:?}");
        }
    }
}

// ---------------------------------------------------------------------------
// The market row (a new book's tick / lot) and the band width.
// ---------------------------------------------------------------------------

/// `market_shape` (no book yet): the market row read fails. RED before
/// R02: the book was created with `(ONE, ONE)`.
#[test]
fn r02_market_shape_read_fault_fail_stops() {
    for mode in MODES {
        let (_d, _db, backend, mut ctx) = setup(Some(b"listed"));
        backend.arm(CF_NATIVE_MARKETS, &M.to_be_bytes());
        exec(&mut ctx, mode, &[order(1, true, 100, 1, TimeInForce::GTC, false)]);
        assert_injected(ctx.take_fatal_error(), "market shape read", &format!("mode {mode:?}"));
    }
}

/// Regression: no market row and an undecodable one (the deterministic
/// decode policy of `market_row_shape`: every validator holds the same
/// row) create the book with `(ONE, ONE)`, as today: a 1-lot bid at 100
/// rests, a 0.5-lot one is rejected (below the lot). No fail-stop.
#[test]
fn r02_market_shape_absent_or_undecodable_unchanged() {
    for mode in MODES {
        for row in [None, Some(b"listed".as_slice())] {
            let what = format!("mode {mode:?} row {row:?}");
            let (_d, _db, _b, mut ctx) = setup(row);
            let half = (
                addr(1),
                NativeAction::PlaceOrder(PlaceOrderParams {
                    market_id: M,
                    is_buy: true,
                    price: fp(100),
                    quantity: FixedPoint::from_raw(FixedPoint::SCALE / 2),
                    order_type: OrderType::Limit,
                    time_in_force: TimeInForce::GTC,
                    reduce_only: false,
                    client_order_id: None,
                }),
            );
            let r = exec(&mut ctx, mode, &[order(1, true, 100, 1, TimeInForce::GTC, false), half]);
            assert_eq!(r.iter().map(|x| x.0).collect::<Vec<_>>(), vec![true, false], "{what}: {r:?}");
            assert_eq!(ctx.take_fatal_error(), None, "{what}");
        }
    }
}

/// `price_band_bps`: the governance band read fails. RED before R02: the
/// ±50% default applied.
#[test]
fn r02_price_band_param_read_fault_fail_stops() {
    for mode in MODES {
        let (_d, _db, backend, mut ctx) = setup(None);
        backend.arm(CF_FEE_CONFIG, BAND_KEY);
        exec(&mut ctx, mode, &[order(1, true, 100, 1, TimeInForce::GTC, false)]);
        assert_injected(ctx.take_fatal_error(), "price band param read", &format!("mode {mode:?}"));
    }
}

/// Regression: an absent band key and an invalid stored value (the
/// deterministic validation of `torus_types::price_band_bps`) read as the
/// default, a valid one as itself; nothing fail-stops. With a fresh mark
/// of 100, a bid at 140 passes ±50% and fails ±10%.
#[test]
fn r02_price_band_param_absent_or_invalid_unchanged() {
    for mode in MODES {
        for (stored, passes) in [(None, true), (Some(b"oops".as_slice()), true), (Some(b"1000".as_slice()), false)] {
            let what = format!("mode {mode:?} stored {stored:?}");
            let (_d, db, _b, mut ctx) = setup(None);
            if let Some(v) = stored {
                db.put_cf_raw(CF_FEE_CONFIG, BAND_KEY, v).unwrap();
            }
            let agg = [fp(100).raw().to_be_bytes().as_slice(), &1u64.to_be_bytes(), &3u32.to_be_bytes(), &NOW.to_be_bytes()].concat();
            db.put_cf_raw(CF_NATIVE_ORACLE, &[b"agg".as_slice(), &M.to_be_bytes()].concat(), &agg).unwrap();
            let r = exec(&mut ctx, mode, &[order(1, true, 140, 1, TimeInForce::GTC, false)]);
            assert_eq!(r[0].0, passes, "{what}: {r:?}");
            assert_eq!(ctx.take_fatal_error(), None, "{what}");
        }
    }
}
