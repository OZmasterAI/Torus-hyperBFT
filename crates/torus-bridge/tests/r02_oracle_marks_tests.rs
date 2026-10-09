//! R02 (audit main-chain-deep-review-2026-10-08, option A, branch 3): the
//! native executor's oracle reads. A LOCAL fault (the aggregate row read
//! fails on this node, or the stored row does not decode) must fail-stop
//! the block, never be read as "no mark" / "no band reference" / a
//! per-market error result:
//! - the block mark table (`fill_block_marks`) and the aggregation step
//!   (`aggregate_oracle_prices`) latch `ctx.fatal_error`;
//! - the readers without `&mut ctx` (`AccountReader::mark`, `price_band`,
//!   also on the Phase 2 / 3 workers) record the fault in
//!   `ctx.reader_fault`, which `take_fatal_error` (the committer's check)
//!   returns.
//!
//! Legitimate absence (no aggregate row) and staleness keep today's results
//! and never fail-stop. The block-level proof (exec_failed, nothing flushed,
//! marker unchanged) is in torus-consensus `app.rs` (`r02_oracle_*`).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use alloy_primitives::Address;

use torus_bridge::native_executor::{NativeExecContext, NativeExecutor};
use torus_core::position::NativeBalance;
use torus_state::cf::{CF_NATIVE_MARKETS, CF_NATIVE_ORACLE};
use torus_state::{AtomicWriteOp, StateBackend, StateDb, StateError};
use torus_types::{FixedPoint, MarketId, NativeAction, OrderType, PlaceOrderParams, TimeInForce};

/// A backend whose point reads of the oracle aggregate rows (`agg‖market`)
/// fail while `armed` (an injected read fault). Everything else delegates.
#[derive(Clone)]
struct FailingAggRead {
    inner: StateDb,
    armed: Arc<AtomicBool>,
}

impl StateBackend for FailingAggRead {
    fn get_cf_raw(&self, cf: &str, key: &[u8]) -> Result<Option<Vec<u8>>, StateError> {
        if self.armed.load(Ordering::SeqCst) && cf == CF_NATIVE_ORACLE && key.starts_with(b"agg") {
            return Err(StateError::Io(std::io::Error::other("injected aggregate read failure")));
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

fn addr(n: u8) -> Address {
    Address::new([n; 20])
}

fn fp(v: i64) -> FixedPoint {
    FixedPoint::from_raw(v as i128 * FixedPoint::SCALE)
}

fn agg_key(m: MarketId) -> Vec<u8> {
    [b"agg".as_slice(), &m.to_be_bytes()].concat()
}

/// The stored aggregate row: price(16) ‖ block(8) ‖ reporters(4) ‖ timestamp(8).
fn agg_row(price: FixedPoint, ts: u64) -> Vec<u8> {
    [price.raw().to_be_bytes().as_slice(), &1u64.to_be_bytes(), &3u32.to_be_bytes(), &ts.to_be_bytes()].concat()
}

type Ctx = NativeExecContext<FailingAggRead>;

/// Market 1 listed (test row), trader 1 rich, `agg` the aggregate row of
/// market 1 (None: no row). Block 2 at `NOW`, fault switch off.
fn setup(agg: Option<Vec<u8>>) -> (tempfile::TempDir, StateDb, Arc<AtomicBool>, Ctx) {
    let dir = tempfile::tempdir().unwrap();
    let db = StateDb::open(dir.path()).unwrap();
    db.put_cf_raw(CF_NATIVE_MARKETS, &M.to_be_bytes(), b"listed").unwrap();
    if let Some(row) = agg {
        db.put_cf_raw(CF_NATIVE_ORACLE, &agg_key(M), &row).unwrap();
    }
    let armed = Arc::new(AtomicBool::new(false));
    let backend = FailingAggRead { inner: db.clone(), armed: armed.clone() };
    let ctx = NativeExecContext::new(backend, 2, NOW, 0, 1_000, 10, addr(99), addr(100), addr(101));
    ctx.positions
        .put_native_balance(&addr(1), &NativeBalance { available: fp(1_000_000), order_margin: FixedPoint::ZERO })
        .unwrap();
    (dir, db, armed, ctx)
}

fn bid(price: i64) -> (Address, NativeAction) {
    (
        addr(1),
        NativeAction::PlaceOrder(PlaceOrderParams {
            market_id: M,
            is_buy: true,
            price: fp(price),
            quantity: fp(1),
            order_type: OrderType::Limit,
            time_in_force: TimeInForce::GTC,
            reduce_only: false,
            client_order_id: None,
        }),
    )
}

/// `None` = the single-action path, `Some(t)` = `execute_batch_engine_mode(_, _, t)`.
const MODES: [Option<usize>; 4] = [None, Some(0), Some(2), Some(4)];

fn exec(ctx: &mut Ctx, mode: Option<usize>, block: &[(Address, NativeAction)]) -> Vec<(bool, Option<String>)> {
    let r: Vec<_> = match mode {
        None => block.iter().map(|(s, a)| NativeExecutor::execute(ctx, s, a)).collect(),
        Some(t) => NativeExecutor::execute_batch_engine_mode(ctx, block, t).results,
    };
    r.into_iter().map(|a| (a.success, a.error.map(String::from))).collect()
}

fn assert_injected(reason: Option<String>, what: &str) {
    let reason = reason.unwrap_or_else(|| panic!("{what}: must fail-stop"));
    assert!(reason.contains("injected aggregate read failure"), "{what}: {reason}");
}

// ---------------------------------------------------------------------------
// Readers without &mut ctx: AccountReader::mark and price_band.
// ---------------------------------------------------------------------------

/// No mark table (the oracle step did not run): every mark reads the oracle.
/// A failed read is recorded on the reader channel and fail-stops the block
/// (`take_fatal_error`), on the single-action path and every batch mode
/// (the Phase 2 markets are read on the exec thread, the placement on
/// workers). RED before R02: the fault read as "no mark" and the order was
/// placed (reserved at its limit price).
#[test]
fn r02_mark_read_fault_fail_stops_through_the_reader_channel() {
    for mode in MODES {
        let (_d, _db, armed, mut ctx) = setup(Some(agg_row(fp(100), NOW)));
        armed.store(true, Ordering::SeqCst);
        exec(&mut ctx, mode, &[bid(100)]);
        assert_injected(ctx.take_fatal_error(), &format!("mode {mode:?}"));
        assert_eq!(ctx.take_fatal_error(), None, "mode {mode:?}: taken once");
    }
}

/// The price band's last-price read (mark not usable, so the band falls back
/// to the last aggregate): a failed read fail-stops. The mark itself comes
/// from the block's table (built before the fault, market 1 = None: the row
/// is stale), so only the band reads the oracle. RED before R02: the fault
/// read as "never marked" (no band) and the far-off bid was placed.
#[test]
fn r02_price_band_read_fault_fail_stops_through_the_reader_channel() {
    for mode in MODES {
        let (_d, _db, armed, mut ctx) = setup(Some(agg_row(fp(100), NOW - 61)));
        NativeExecutor::begin_block_oracle(&mut ctx);
        assert_eq!(ctx.take_fatal_error(), None, "mode {mode:?}: the oracle step is healthy");
        armed.store(true, Ordering::SeqCst);
        exec(&mut ctx, mode, &[bid(1_000)]);
        assert_injected(ctx.take_fatal_error(), &format!("mode {mode:?}"));
    }
}

/// Regression (no fault): absence and staleness keep today's results and
/// never fail-stop. No aggregate row: no mark, no band, the bid rests. A
/// stale row: the band uses the last price (±50% of 100), so 1,000 is
/// rejected and 120 rests. A fresh row: the same band around the mark.
#[test]
fn r02_absent_or_stale_marks_unchanged() {
    for mode in MODES {
        for oracle_step in [false, true] {
            let what = format!("mode {mode:?} step {oracle_step}");
            let run = |agg: Option<Vec<u8>>, block: &[(Address, NativeAction)]| {
                let (_d, _db, _armed, mut ctx) = setup(agg);
                if oracle_step {
                    NativeExecutor::begin_block_oracle(&mut ctx);
                }
                let r = exec(&mut ctx, mode, block);
                assert_eq!(ctx.take_fatal_error(), None, "{what}");
                r.into_iter().map(|(ok, _)| ok).collect::<Vec<_>>()
            };
            assert_eq!(run(None, &[bid(1_000)]), vec![true], "{what}: absent");
            assert_eq!(run(Some(agg_row(fp(100), NOW - 61)), &[bid(1_000)]), vec![false], "{what}: stale");
            assert_eq!(run(Some(agg_row(fp(100), NOW - 61)), &[bid(120)]), vec![true], "{what}: stale");
            assert_eq!(run(Some(agg_row(fp(100), NOW)), &[bid(1_000)]), vec![false], "{what}: fresh");
            assert_eq!(run(Some(agg_row(fp(100), NOW)), &[bid(120)]), vec![true], "{what}: fresh");
        }
    }
}

// ---------------------------------------------------------------------------
// With &mut ctx: the mark table and the aggregation step.
// ---------------------------------------------------------------------------

/// The block mark table's read of an aggregate row fails: `fatal_error`,
/// no table. RED before R02: the market's mark read as None for the block.
#[test]
fn r02_block_mark_table_read_fault_latches() {
    let (_d, _db, armed, mut ctx) = setup(Some(agg_row(fp(100), NOW)));
    armed.store(true, Ordering::SeqCst);
    NativeExecutor::fill_block_marks(&mut ctx, &[M]);
    assert_injected(ctx.fatal_error.take(), "fill_block_marks");
}

/// A delisted market's aggregate row that does not decode (corrupt on this
/// node): only the mark table reads it (no aggregation), and it fail-stops.
/// RED before R02: the market's mark read as None.
#[test]
fn r02_block_mark_table_undecodable_row_latches() {
    let (_d, db, _armed, mut ctx) = setup(None);
    db.put_cf_raw(CF_NATIVE_ORACLE, &agg_key(7), b"garbage").unwrap();
    let agg = NativeExecutor::begin_block_oracle(&mut ctx);
    assert_eq!(agg.len(), 1, "market 1 aggregates (no data: an error result)");
    let reason = ctx.fatal_error.take().expect("must fail-stop");
    assert!(reason.contains("borsh") || reason.contains("Borsh") || reason.contains("decode"), "{reason}");
}

/// The aggregation step's read of the last aggregate fails (market 1 has no
/// submissions, so `aggregate_price` falls back to the last price): the
/// per-market error is a local fault and latches `fatal_error`. RED before
/// R02: an error result row, the block went on.
#[test]
fn r02_aggregate_read_fault_latches() {
    let (_d, _db, armed, mut ctx) = setup(Some(agg_row(fp(100), NOW)));
    armed.store(true, Ordering::SeqCst);
    let r = NativeExecutor::aggregate_oracle_prices(&mut ctx, &[M], &[]);
    assert_eq!(r.len(), 1);
    assert!(!r[0].success);
    assert_injected(ctx.fatal_error.take(), "aggregate_oracle_prices");
}

/// The same with an undecodable stored aggregate of a listed market, through
/// the whole oracle step. RED before R02 (`oracle_block_tests` pinned it as
/// an error result that "never aborts the block").
#[test]
fn r02_aggregate_undecodable_row_latches() {
    let (_d, _db, _armed, mut ctx) = setup(Some(vec![1, 2, 3]));
    let agg = NativeExecutor::begin_block_oracle(&mut ctx);
    assert_eq!(agg.len(), 1);
    assert!(!agg[0].success);
    assert!(ctx.fatal_error.take().is_some(), "must fail-stop");
}

/// Regression: per-market errors that are not local faults stay results.
/// No aggregate row and no submissions (NoOraclePrice) and a stale last
/// aggregate (StaleOraclePrice): one error result each, no fail-stop.
#[test]
fn r02_aggregate_absence_and_staleness_stay_results() {
    for agg in [None, Some(agg_row(fp(100), NOW - 61))] {
        let (_d, _db, _armed, mut ctx) = setup(agg.clone());
        let r = NativeExecutor::begin_block_oracle(&mut ctx);
        assert_eq!(r.iter().map(|x| x.success).collect::<Vec<_>>(), vec![false], "{agg:?}");
        assert_eq!(ctx.take_fatal_error(), None, "{agg:?}");
    }
}
