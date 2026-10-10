//! R02 branch 6a (audit main-chain-deep-review-2026-10-08, inventory
//! section 1b): a LOCAL storage fault (the read or write failed on this
//! node, or a row it stored does not decode) on an action's own state must
//! fail-stop the block (`take_fatal_error`, the committer's check), never
//! become an ordinary rejected-action result that this node alone records:
//! - `take_open_slot` (single + batch): the `cum_volume` read;
//! - `prepare_one` (batch Phase 2, serial and sharded): the sender's
//!   balance, its `pos_net` and its position;
//! - the batch settle (sequential AND parallel, pinned per call): a fill
//!   that does not apply after the book matched;
//! - `place_order_inner` (single path): the reduce-only position read, the
//!   balance read / write, the account read, and a fill after the match;
//! - `check_withdrawal_margin`: the balance and the account view;
//! - the Lockbox actions and the CoreWriter lockbox deposit credit.
//!
//! The action's result keeps its shape (the block never flushes). Errors
//! every validator hits alike (insufficient balance, a margin reject) keep
//! today's result and never fail-stop; fault-free runs are unchanged.

use std::sync::{Arc, Mutex};

use alloy_primitives::{Address, U256};

use torus_bridge::native_executor::{NativeExecContext, NativeExecutor};
use torus_core::position::{cum_volume_key, position_key, NativeBalance};
use torus_core::precompiles::{CoreWriterQueue, QueuedAction, QueuedActionKind};
use torus_state::cf::{CF_ACCOUNTS, CF_NATIVE_BALANCES, CF_NATIVE_POSITIONS};
use torus_state::{AtomicWriteOp, StateBackend, StateDb, StateError};
use torus_types::{FixedPoint, MarketId, NativeAction, OrderType, PlaceOrderParams, TimeInForce};

/// Armed `(cf, key)` rows.
type Armed = Arc<Mutex<Vec<(&'static str, Vec<u8>)>>>;

/// A backend whose point reads (`reads`) or writes (`writes`, including an
/// atomic batch that writes one) of the armed `(cf, key)` rows fail; `hits`
/// counts the injected failures. Everything else delegates.
#[derive(Clone)]
struct Failing {
    inner: StateDb,
    reads: Armed,
    writes: Armed,
    hits: Arc<Mutex<usize>>,
}

const INJECTED_READ: &str = "injected action read failure";
const INJECTED_WRITE: &str = "injected action write failure";

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
    fn fail(&self, what: &str) -> StateError {
        *self.hits.lock().unwrap() += 1;
        StateError::Io(std::io::Error::other(what.to_string()))
    }
}

impl StateBackend for Failing {
    fn get_cf_raw(&self, cf: &str, key: &[u8]) -> Result<Option<Vec<u8>>, StateError> {
        if Self::armed(&self.reads, cf, key) {
            return Err(self.fail(INJECTED_READ));
        }
        self.inner.get_cf_raw(cf, key)
    }
    fn put_cf_raw(&self, cf: &str, key: &[u8], value: &[u8]) -> Result<(), StateError> {
        if Self::armed(&self.writes, cf, key) {
            return Err(self.fail(INJECTED_WRITE));
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
        for op in ops {
            if let AtomicWriteOp::Put { cf, key, .. } = op {
                if Self::armed(&self.writes, cf, key) {
                    return Err(self.fail(INJECTED_WRITE));
                }
            }
        }
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

fn amount(v: i64) -> U256 {
    U256::from(fp(v).raw() as u128)
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

fn params(market: MarketId, is_buy: bool, price: i64, qty: i64) -> PlaceOrderParams {
    PlaceOrderParams {
        market_id: market,
        is_buy,
        price: fp(price),
        quantity: fp(qty),
        order_type: OrderType::Limit,
        time_in_force: TimeInForce::GTC,
        reduce_only: false,
        client_order_id: None,
    }
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
        NativeAction::PlaceOrder(params(market, is_buy, price, qty)),
    )
}

/// How a block runs: the single-action path; a batch with serial Phase-2
/// prepare and the settle loop pinned; a batch with sharded Phase-2 prepare
/// (2 threads) and parallel settle.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Mode {
    Single,
    Settle { parallel: bool },
    Sharded,
}

/// Each action's `(success, error)`.
type Outcome = Vec<(bool, Option<String>)>;

fn exec(ctx: &mut Ctx, mode: Mode, block: &[(Address, NativeAction)]) -> Outcome {
    let r: Vec<_> = match mode {
        Mode::Single => block
            .iter()
            .map(|(s, a)| NativeExecutor::execute(ctx, s, a))
            .collect(),
        Mode::Settle { parallel } => {
            NativeExecutor::execute_batch_settle_workers(ctx, block, parallel, Some(2)).results
        }
        Mode::Sharded => NativeExecutor::execute_batch_engine_mode(ctx, block, 2).results,
    };
    r.into_iter()
        .map(|a| (a.success, a.error.map(String::from)))
        .collect()
}

/// The block fail-stops on the injected fault, recorded by `step` (the site).
fn assert_injected(reason: Option<String>, step: &str, injected: &str, what: &str) {
    let reason = reason.unwrap_or_else(|| panic!("{what}: must fail-stop"));
    assert!(
        reason.starts_with(&format!("{step}: ")),
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

/// An undecodable position row for `trader` in market 7 (a local fault: the
/// row is only ever written by `put_position`).
fn corrupt_position(db: &StateDb, trader: u8) {
    db.put_cf_raw(CF_NATIVE_POSITIONS, &position_key(&addr(trader), 7), b"\xff")
        .unwrap();
}

// ---------------------------------------------------------------------------
// 1. take_open_slot: the cum_volume read (single + batch).
// ---------------------------------------------------------------------------

const PLACE_MODES: [Mode; 3] = [Mode::Single, Mode::Settle { parallel: false }, Mode::Sharded];

/// Trader 1 rests a GTC bid on market 1; trader 4 one on market 2 (two
/// senders: the sharded prepare engages).
fn place_block() -> Vec<(Address, NativeAction)> {
    vec![order(1, 1, true, 100, 1), order(4, 2, true, 40, 1)]
}

/// The open-order limit's `cum_volume` read fails. RED before R02: the
/// order was rejected (`RejectReason::Other`) and the block went on.
#[test]
fn r02_open_slot_cum_volume_read_fault_fail_stops() {
    for mode in PLACE_MODES {
        let (_d, _db, backend, mut ctx) = setup();
        backend.arm_read(CF_NATIVE_BALANCES, &cum_volume_key(&addr(1)));
        let r = exec(&mut ctx, mode, &place_block());
        assert!(backend.hits() > 0, "{mode:?}: fault injected");
        assert!(!r[0].0, "{mode:?}: result shape kept (rejected): {r:?}");
        assert_injected(
            ctx.take_fatal_error(),
            "open order slot cum_volume read",
            INJECTED_READ,
            &format!("{mode:?}"),
        );
        assert_eq!(ctx.take_fatal_error(), None, "{mode:?}: taken once");
    }
}

/// An undecodable `cum_volume` row (wrong length: a local fault).
#[test]
fn r02_open_slot_undecodable_cum_volume_fail_stops() {
    for mode in PLACE_MODES {
        let (_d, db, _backend, mut ctx) = setup();
        db.put_cf_raw(CF_NATIVE_BALANCES, &cum_volume_key(&addr(1)), b"x")
            .unwrap();
        exec(&mut ctx, mode, &place_block());
        let reason = ctx
            .take_fatal_error()
            .unwrap_or_else(|| panic!("{mode:?}: must fail-stop"));
        assert!(
            reason.starts_with("open order slot cum_volume read: borsh"),
            "{mode:?}: {reason}"
        );
    }
}

/// Control (no fault): both orders rest, the bid's reservation is held, the
/// same in every mode; nothing fail-stops. An unfunded sender (trader 5)
/// whose order fails the account check is rejected on margin as today,
/// without a fail-stop.
#[test]
fn r02_place_without_fault_unchanged() {
    let mut seen: Option<(Outcome, Vec<(FixedPoint, FixedPoint)>)> = None;
    for mode in PLACE_MODES {
        let (_d, db, backend, mut ctx) = setup();
        let mut block = place_block();
        block.push(order(5, 1, true, 99, 1_000));
        let r = exec(&mut ctx, mode, &block);
        assert!(r[0].0 && r[1].0, "{mode:?}: {r:?}");
        assert!(!r[2].0, "{mode:?}: unfunded sender rejected: {r:?}");
        assert_eq!(ctx.take_fatal_error(), None, "{mode:?}");
        assert_eq!(backend.hits(), 0);
        let bals: Vec<_> = (1..=5)
            .map(|t| bal(&db, t))
            .map(|b| (b.available, b.order_margin))
            .collect();
        assert!(bals[0].1 > FixedPoint::ZERO, "{mode:?}: reserved");
        match &seen {
            None => seen = Some((r, bals)),
            Some((r0, b0)) => {
                assert_eq!(&r, r0, "{mode:?}: results");
                assert_eq!(&bals, b0, "{mode:?}: balances");
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 2. prepare_one (batch Phase 2) and the single path's pre-book reads.
// ---------------------------------------------------------------------------

/// The sender's balance read fails. RED before R02: rejected
/// (`PrepOutcome::Reject` / `NativeActionResult::err`).
#[test]
fn r02_place_balance_read_fault_fail_stops() {
    for mode in PLACE_MODES {
        let (_d, _db, backend, mut ctx) = setup();
        backend.arm_read(CF_NATIVE_BALANCES, addr(1).as_slice());
        let r = exec(&mut ctx, mode, &place_block());
        assert!(backend.hits() > 0, "{mode:?}: fault injected");
        assert!(!r[0].0, "{mode:?}: result shape kept: {r:?}");
        let step = if mode == Mode::Single {
            "place order balance read"
        } else {
            "phase 2 balance read"
        };
        assert_injected(ctx.take_fatal_error(), step, INJECTED_READ, &format!("{mode:?}"));
    }
}

/// The sender's `pos_net` (all its positions) does not read: one of its
/// position rows does not decode. RED before R02: rejected.
#[test]
fn r02_place_pos_net_fault_fail_stops() {
    for mode in PLACE_MODES {
        let (_d, db, _backend, mut ctx) = setup();
        corrupt_position(&db, 1);
        let r = exec(&mut ctx, mode, &place_block());
        assert!(!r[0].0, "{mode:?}: result shape kept: {r:?}");
        assert!(r[1].0, "{mode:?}: the other sender is unaffected: {r:?}");
        let reason = ctx
            .take_fatal_error()
            .unwrap_or_else(|| panic!("{mode:?}: must fail-stop"));
        let step = if mode == Mode::Single {
            "place order account read"
        } else {
            "phase 2 pos_net read"
        };
        assert!(
            reason.starts_with(&format!("{step}: borsh")),
            "{mode:?}: {reason}"
        );
    }
}

/// The sender's position in the order's market does not read (point read).
/// RED before R02: rejected.
#[test]
fn r02_place_position_read_fault_fail_stops() {
    for mode in PLACE_MODES {
        let (_d, _db, backend, mut ctx) = setup();
        backend.arm_read(CF_NATIVE_POSITIONS, &position_key(&addr(1), 1));
        let r = exec(&mut ctx, mode, &place_block());
        assert!(backend.hits() > 0, "{mode:?}: fault injected");
        assert!(!r[0].0, "{mode:?}: result shape kept: {r:?}");
        let step = if mode == Mode::Single {
            "place order account read"
        } else {
            "phase 2 position read"
        };
        assert_injected(ctx.take_fatal_error(), step, INJECTED_READ, &format!("{mode:?}"));
    }
}

/// Single path: the reservation's balance write fails. RED before R02:
/// `NativeActionResult::err`.
#[test]
fn r02_place_balance_write_fault_fail_stops_single() {
    let (_d, _db, backend, mut ctx) = setup();
    backend.arm_write(CF_NATIVE_BALANCES, addr(1).as_slice());
    let r = exec(&mut ctx, Mode::Single, &place_block());
    assert!(backend.hits() > 0, "fault injected");
    assert!(!r[0].0, "result shape kept: {r:?}");
    assert_injected(
        ctx.take_fatal_error(),
        "place order balance write",
        INJECTED_WRITE,
        "single",
    );
}

/// Single path: a reduce-only order's position read fails. RED before R02:
/// rejected (`FailureReason::Other`).
#[test]
fn r02_reduce_only_position_read_fault_fail_stops_single() {
    let (_d, _db, backend, mut ctx) = setup();
    backend.arm_read(CF_NATIVE_POSITIONS, &position_key(&addr(1), 1));
    let ro = NativeAction::PlaceOrder(PlaceOrderParams {
        reduce_only: true,
        ..params(1, false, 100, 1)
    });
    let r = NativeExecutor::execute(&mut ctx, &addr(1), &ro);
    assert!(backend.hits() > 0, "fault injected");
    assert!(!r.success, "result shape kept: {r:?}");
    assert_injected(
        ctx.take_fatal_error(),
        "reduce-only placement position read",
        INJECTED_READ,
        "single",
    );
}

/// Control: a reduce-only order without a position is rejected as today
/// (deterministic), no fail-stop.
#[test]
fn r02_reduce_only_without_position_unchanged() {
    let (_d, _db, _backend, mut ctx) = setup();
    let ro = NativeAction::PlaceOrder(PlaceOrderParams {
        reduce_only: true,
        ..params(1, false, 100, 1)
    });
    let r = NativeExecutor::execute(&mut ctx, &addr(1), &ro);
    assert!(!r.success, "{r:?}");
    assert_eq!(ctx.take_fatal_error(), None);
}

// ---------------------------------------------------------------------------
// 3 / 4. A fill that does not apply after the book matched.
// ---------------------------------------------------------------------------

const FILL_MODES: [Mode; 3] = [
    Mode::Single,
    Mode::Settle { parallel: false },
    Mode::Settle { parallel: true },
];

/// Trader 2 goes long 1 on market 1 (from trader 3), then rests a closing
/// ask at 101; trader 3 rests a bid on market 2 (so a batch has work in two
/// markets: parallel settle engages).
fn rest_closing_maker(ctx: &mut Ctx) {
    let r = exec(
        ctx,
        Mode::Single,
        &[
            order(3, 1, false, 100, 1),
            order(2, 1, true, 100, 1),
            order(2, 1, false, 101, 1),
            order(3, 2, true, 50, 1),
        ],
    );
    assert!(r.iter().all(|x| x.0), "{r:?}");
    assert_eq!(ctx.take_fatal_error(), None);
}

/// Trader 1 lifts trader 2's closing ask (trader 2 realizes PnL); trader 4
/// bids on market 2 (rests).
fn fill_block() -> Vec<(Address, NativeAction)> {
    vec![order(1, 1, true, 101, 1), order(4, 2, true, 40, 1)]
}

/// Which settle loop ran and how often the parallel one fell back.
fn assert_settle_path(ctx: &Ctx, mode: Mode, fallbacks: u64) {
    if let Mode::Settle { parallel } = mode {
        assert_eq!(
            ctx.phase_accum.settle_pass_a_ns > 0,
            parallel,
            "{mode:?}: settle path pinned"
        );
        let want = if parallel { fallbacks } else { 0 };
        assert_eq!(ctx.phase_accum.settle_fallbacks, want, "{mode:?}: fallbacks");
    }
}

/// The maker's balance read for its realized-PnL credit fails (sequential:
/// `apply_fill_via_caches`; parallel: pass B, no fallback). RED before R02:
/// "maker fill failed" after the book had matched, and the block went on
/// (until branch 4's later release read).
#[test]
fn r02_settle_maker_pnl_balance_read_fault_fail_stops() {
    for mode in [Mode::Settle { parallel: false }, Mode::Settle { parallel: true }] {
        let (_d, _db, backend, mut ctx) = setup();
        rest_closing_maker(&mut ctx);
        backend.arm_read(CF_NATIVE_BALANCES, addr(2).as_slice());
        let r = exec(&mut ctx, mode, &fill_block());
        assert!(backend.hits() > 0, "{mode:?}: fault injected");
        assert_settle_path(&ctx, mode, 0);
        assert!(
            r[0].1.as_deref().is_some_and(|e| e.starts_with("maker fill failed")),
            "{mode:?}: result shape kept: {r:?}"
        );
        assert_injected(
            ctx.take_fatal_error(),
            "settle maker fill",
            INJECTED_READ,
            &format!("{mode:?}"),
        );
    }
}

/// The maker's position read when its fill is applied fails (sequential;
/// parallel falls back to it). RED before R02.
#[test]
fn r02_settle_maker_position_read_fault_fail_stops() {
    for mode in [Mode::Settle { parallel: false }, Mode::Settle { parallel: true }] {
        let (_d, _db, backend, mut ctx) = setup();
        rest_closing_maker(&mut ctx);
        backend.arm_read(CF_NATIVE_POSITIONS, &position_key(&addr(2), 1));
        let r = exec(&mut ctx, mode, &fill_block());
        assert!(backend.hits() > 0, "{mode:?}: fault injected");
        assert_settle_path(&ctx, mode, 1);
        assert!(
            r[0].1.as_deref().is_some_and(|e| e.starts_with("maker fill failed")),
            "{mode:?}: result shape kept: {r:?}"
        );
        assert_injected(
            ctx.take_fatal_error(),
            "settle maker fill",
            INJECTED_READ,
            &format!("{mode:?}"),
        );
    }
}

/// An undecodable maker balance row on the PnL credit (a local fault), the
/// same in both settle loops. RED before R02 (the first fail-stop was the
/// later maker release).
#[test]
fn r02_settle_maker_undecodable_balance_fail_stops_at_the_fill() {
    let mut seen = None;
    for mode in [Mode::Settle { parallel: false }, Mode::Settle { parallel: true }] {
        let (_d, db, _backend, mut ctx) = setup();
        rest_closing_maker(&mut ctx);
        db.put_cf_raw(CF_NATIVE_BALANCES, addr(2).as_slice(), b"x")
            .unwrap();
        let r = exec(&mut ctx, mode, &fill_block());
        assert_settle_path(&ctx, mode, 0);
        let reason = ctx
            .take_fatal_error()
            .unwrap_or_else(|| panic!("{mode:?}: must fail-stop"));
        assert!(
            reason.starts_with("settle maker fill: borsh"),
            "{mode:?}: {reason}"
        );
        match &seen {
            None => seen = Some((r, reason)),
            Some(s) => assert_eq!(s, &(r, reason), "{mode:?}: identical"),
        }
    }
}

/// Single path: the taker's fill (its `cum_volume` write) fails after the
/// match. RED before R02: rejected "taker fill failed".
#[test]
fn r02_place_taker_fill_fault_fail_stops_single() {
    let (_d, _db, backend, mut ctx) = setup();
    rest_closing_maker(&mut ctx);
    backend.arm_write(CF_NATIVE_BALANCES, &cum_volume_key(&addr(1)));
    let r = exec(&mut ctx, Mode::Single, &fill_block());
    assert!(backend.hits() > 0, "fault injected");
    assert!(
        r[0].1.as_deref().is_some_and(|e| e.starts_with("taker fill failed")),
        "result shape kept: {r:?}"
    );
    assert_injected(
        ctx.take_fatal_error(),
        "place order taker fill",
        INJECTED_WRITE,
        "single",
    );
}

/// Single path: the maker's fill (its `cum_volume` write) fails after the
/// match. RED before R02: rejected "maker fill failed".
#[test]
fn r02_place_maker_fill_fault_fail_stops_single() {
    let (_d, _db, backend, mut ctx) = setup();
    rest_closing_maker(&mut ctx);
    backend.arm_write(CF_NATIVE_BALANCES, &cum_volume_key(&addr(2)));
    let r = exec(&mut ctx, Mode::Single, &fill_block());
    assert!(backend.hits() > 0, "fault injected");
    assert!(
        r[0].1.as_deref().is_some_and(|e| e.starts_with("maker fill failed")),
        "result shape kept: {r:?}"
    );
    assert_injected(
        ctx.take_fatal_error(),
        "place order maker fill",
        INJECTED_WRITE,
        "single",
    );
}

/// Control (no fault): the closing fill settles alike in all three modes
/// (results and every balance); nothing fail-stops.
#[test]
fn r02_fill_without_fault_unchanged() {
    let mut seen: Option<(Outcome, Vec<(FixedPoint, FixedPoint)>)> = None;
    for mode in FILL_MODES {
        let (_d, db, backend, mut ctx) = setup();
        rest_closing_maker(&mut ctx);
        let r = exec(&mut ctx, mode, &fill_block());
        assert!(r.iter().all(|x| x.0), "{mode:?}: {r:?}");
        assert_settle_path(&ctx, mode, 0);
        assert_eq!(ctx.take_fatal_error(), None, "{mode:?}");
        assert_eq!(backend.hits(), 0);
        let bals: Vec<_> = (1..=4)
            .map(|t| bal(&db, t))
            .map(|b| (b.available, b.order_margin))
            .collect();
        match &seen {
            None => seen = Some((r, bals)),
            Some((r0, b0)) => {
                assert_eq!(&r, r0, "{mode:?}: results");
                assert_eq!(&bals, b0, "{mode:?}: balances");
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 6 / 7. Withdrawals, deposits and the lockbox deposit credit.
// ---------------------------------------------------------------------------

/// A 72-byte EVM account record holding `wei`.
fn put_evm(db: &StateDb, who: Address, wei: U256) {
    let mut d = vec![0u8; 72];
    d[..32].copy_from_slice(&wei.to_be_bytes::<32>());
    db.put_cf_raw(CF_ACCOUNTS, who.as_slice(), &d).unwrap();
}

fn transfer_to_spot(v: i64) -> NativeAction {
    NativeAction::TransferToSpot { amount: amount(v) }
}

fn withdraw_to(v: i64, to: u8) -> NativeAction {
    NativeAction::Withdraw {
        amount: amount(v),
        to: addr(to),
    }
}

/// The withdrawal margin check's balance read fails. RED before R02:
/// rejected (`FailureReason::Other`).
#[test]
fn r02_withdrawal_margin_balance_read_fault_fail_stops() {
    for action in [transfer_to_spot(10), withdraw_to(10, 9)] {
        let (_d, _db, backend, mut ctx) = setup();
        backend.arm_read(CF_NATIVE_BALANCES, addr(1).as_slice());
        let r = NativeExecutor::execute(&mut ctx, &addr(1), &action);
        assert!(backend.hits() > 0, "{action:?}: fault injected");
        assert!(!r.success, "{action:?}: result shape kept: {r:?}");
        assert_injected(
            ctx.take_fatal_error(),
            "withdrawal margin balance read",
            INJECTED_READ,
            &format!("{action:?}"),
        );
    }
}

/// The withdrawal margin check's account view does not build (a position
/// row does not decode). RED before R02: rejected.
#[test]
fn r02_withdrawal_margin_account_fault_fail_stops() {
    for action in [transfer_to_spot(10), withdraw_to(10, 9)] {
        let (_d, db, _backend, mut ctx) = setup();
        corrupt_position(&db, 1);
        let r = NativeExecutor::execute(&mut ctx, &addr(1), &action);
        assert!(!r.success, "{action:?}: result shape kept: {r:?}");
        let reason = ctx
            .take_fatal_error()
            .unwrap_or_else(|| panic!("{action:?}: must fail-stop"));
        assert!(
            reason.starts_with("withdrawal margin account read: borsh"),
            "{action:?}: {reason}"
        );
    }
}

/// The Lockbox's EVM-side read fails (after the margin check passed). RED
/// before R02: `NativeActionResult::err`.
#[test]
fn r02_lockbox_withdraw_read_fault_fail_stops() {
    for (action, to, step) in [
        (transfer_to_spot(10), 1, "withdraw_from_native"),
        (withdraw_to(10, 9), 9, "withdraw_to"),
    ] {
        let (_d, _db, backend, mut ctx) = setup();
        backend.arm_read(CF_ACCOUNTS, addr(to).as_slice());
        let r = NativeExecutor::execute(&mut ctx, &addr(1), &action);
        assert!(backend.hits() > 0, "{step}: fault injected");
        assert!(!r.success, "{step}: result shape kept: {r:?}");
        assert_injected(ctx.take_fatal_error(), step, INJECTED_READ, step);
    }
}

/// The Lockbox's atomic write fails. RED before R02.
#[test]
fn r02_lockbox_withdraw_write_fault_fail_stops() {
    let (_d, _db, backend, mut ctx) = setup();
    backend.arm_write(CF_ACCOUNTS, addr(9).as_slice());
    let r = NativeExecutor::execute(&mut ctx, &addr(1), &withdraw_to(10, 9));
    assert!(backend.hits() > 0, "fault injected");
    assert!(!r.success, "result shape kept: {r:?}");
    assert_injected(ctx.take_fatal_error(), "withdraw_to", INJECTED_WRITE, "withdraw_to");
}

/// `TransferToPerp`: the Lockbox's EVM balance read fails. RED before R02.
#[test]
fn r02_lockbox_deposit_read_fault_fail_stops() {
    let (_d, db, backend, mut ctx) = setup();
    put_evm(&db, addr(1), U256::from(10u64).pow(U256::from(30u64)));
    backend.arm_read(CF_ACCOUNTS, addr(1).as_slice());
    let r = NativeExecutor::execute(
        &mut ctx,
        &addr(1),
        &NativeAction::TransferToPerp { amount: amount(10) },
    );
    assert!(backend.hits() > 0, "fault injected");
    assert!(!r.success, "result shape kept: {r:?}");
    assert_injected(
        ctx.take_fatal_error(),
        "deposit_to_native",
        INJECTED_READ,
        "deposit",
    );
}

/// Controls (deterministic errors every validator hits alike): a withdrawal
/// over the available balance, and a deposit over the EVM balance, keep
/// today's error result and never fail-stop. A fault-free withdrawal and
/// deposit succeed as today.
#[test]
fn r02_lockbox_deterministic_errors_and_no_fault_unchanged() {
    let (_d, db, backend, mut ctx) = setup();
    for action in [transfer_to_spot(2_000_000), withdraw_to(2_000_000, 9)] {
        let r = NativeExecutor::execute(&mut ctx, &addr(1), &action);
        assert!(!r.success, "{action:?}");
        let err = r.error.as_deref().unwrap_or_default();
        assert!(err.contains("insufficient"), "{action:?}: {err}");
        assert_eq!(ctx.take_fatal_error(), None, "{action:?}");
    }
    let r = NativeExecutor::execute(
        &mut ctx,
        &addr(2),
        &NativeAction::TransferToPerp { amount: amount(10) },
    );
    assert!(!r.success, "no EVM balance: {r:?}");
    assert_eq!(ctx.take_fatal_error(), None, "deposit over the EVM balance");

    put_evm(&db, addr(3), U256::from(10u64).pow(U256::from(30u64)));
    let ok = [
        (addr(3), NativeAction::TransferToPerp { amount: amount(10) }),
        (addr(1), transfer_to_spot(10)),
        (addr(1), withdraw_to(10, 9)),
    ];
    for (s, a) in &ok {
        let r = NativeExecutor::execute(&mut ctx, s, a);
        assert!(r.success, "{a:?}: {r:?}");
    }
    assert_eq!(ctx.take_fatal_error(), None);
    assert_eq!(backend.hits(), 0);
    assert_eq!(bal(&db, 1).available, fp(1_000_000 - 20));
    assert_eq!(bal(&db, 3).available, fp(1_000_000 + 10));
}

/// A CoreWriter lockbox deposit queued in block 1 for trader 7.
fn queue_deposit(db: &StateDb) {
    CoreWriterQueue::enqueue(
        db,
        &QueuedAction {
            trader: addr(7),
            kind: QueuedActionKind::LockboxDeposit { amount: fp(5) },
            block_queued: 1,
        },
    )
    .unwrap();
}

/// The lockbox deposit's native credit fails (its EVM value is already
/// burned). RED before R02: `NativeActionResult::err`, the value lost on
/// this node only.
#[test]
fn r02_lockbox_deposit_credit_fault_fail_stops() {
    for write in [false, true] {
        let (_d, db, backend, mut ctx) = setup();
        queue_deposit(&db);
        if write {
            backend.arm_write(CF_NATIVE_BALANCES, addr(7).as_slice());
        } else {
            backend.arm_read(CF_NATIVE_BALANCES, addr(7).as_slice());
        }
        let r = NativeExecutor::drain_core_writer(&mut ctx).expect("queue reads");
        assert!(backend.hits() > 0, "write {write}: fault injected");
        assert_eq!(r.len(), 1);
        assert!(!r[0].success, "write {write}: result shape kept: {r:?}");
        let injected = if write { INJECTED_WRITE } else { INJECTED_READ };
        assert_injected(
            ctx.take_fatal_error(),
            "lockbox_deposit",
            injected,
            &format!("write {write}"),
        );
    }
}

/// Control: the credit lands, no fail-stop.
#[test]
fn r02_lockbox_deposit_credit_without_fault_unchanged() {
    let (_d, db, backend, mut ctx) = setup();
    queue_deposit(&db);
    let r = NativeExecutor::drain_core_writer(&mut ctx).expect("queue reads");
    assert!(r.len() == 1 && r[0].success, "{r:?}");
    assert_eq!(ctx.take_fatal_error(), None);
    assert_eq!(backend.hits(), 0);
    assert_eq!(bal(&db, 7).available, fp(5));
}
